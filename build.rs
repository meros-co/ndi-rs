//! Links the NDI SDK for the target and generates its FFI bindings.
//!
//! Sets `cfg(has_ndi)` inside this crate when the SDK is linked: Android
//! (every ABI the SDK ships, armeabi-v7a included), iOS and tvOS devices
//! (static archives; simulator builds stub NDI), macOS (libndi.dylib), and,
//! when their SDK is installed, Windows x64/x86 and Linux.
//!
//! Dependents learn the outcome through `links = "ndi"` metadata:
//!  - `DEP_NDI_HAS_NDI=1` when the SDK is linked;
//!  - `DEP_NDI_LINK_ARGS`: `;`-separated linker arguments the final
//!    artifact must pass itself, because Cargo applies `rustc-link-arg` only
//!    to the package that emits it (the Windows delay-load, the iOS and tvOS
//!    clang_rt, the macOS and Linux rpath to the SDK copy for development).
//!
//! Environment:
//!  - `NDI_SDK_DIR` (Windows), `NDI_SDK_ANDROID_DIR`, `NDI_SDK_APPLE_DIR`,
//!    `NDI_SDK_LINUX_DIR`: SDK locations, defaulting to the installers' paths
//!    (/opt/NDI SDK for Linux).
//!  - `NDI_REQUIRE_SDK=1`: fail the build instead of building without NDI
//!    when the Windows or Linux SDK is missing (for release builds).

fn main() {
    println!("cargo:rustc-check-cfg=cfg(has_ndi)");
    println!("cargo:rerun-if-env-changed=NDI_REQUIRE_SDK");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    let mut link_args: Vec<String> = Vec::new();
    let linked = if target_os == "android" {
        link_ndi_android();
        true
    } else if let Some(platform) = AppleStatic::for_target(&target_os, &target_abi) {
        // Frameworks the static libndi depends on.
        for framework in ["AudioToolbox", "Foundation", "AVFoundation", "Network", "Accelerate"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        // libndi references ___chkstk_darwin (compiler-rt). rustc links
        // with -nodefaultlibs, so clang_rt must be passed explicitly.
        // (Xcode's own link of the app gets it automatically from clang.)
        if let Some(clang_rt) = apple_clang_rt_path(&platform) {
            link_args.push(clang_rt);
        }
        link_ndi_apple_static(&platform);
        true
    } else if target_os == "macos" {
        link_args.extend(link_ndi_macos());
        true
    } else if target_os == "linux" {
        match ndi_sdk_linux() {
            Some(sdk_dir) => {
                link_args.extend(link_ndi_linux(&sdk_dir));
                true
            }
            None if std::env::var("NDI_REQUIRE_SDK").as_deref() == Ok("1") => {
                panic!("NDI SDK for Linux not found and NDI_REQUIRE_SDK=1. Set NDI_SDK_LINUX_DIR.")
            }
            None => {
                println!(
                    "cargo:warning=NDI SDK for Linux not found - building without NDI. Set NDI_SDK_LINUX_DIR."
                );
                false
            }
        }
    } else if target_os == "windows" {
        if let Some(sdk_dir) = ndi_sdk_windows() {
            link_args.extend(link_ndi_windows(&sdk_dir));
            true
        } else if std::env::var("NDI_REQUIRE_SDK").as_deref() == Ok("1") {
            panic!(
                "NDI SDK for Windows not found and NDI_REQUIRE_SDK=1. Install it or set NDI_SDK_DIR."
            );
        } else {
            // Not fatal: the crate still builds and reports NDI as
            // unavailable (AVAILABLE = false, probe() says so).
            println!(
                "cargo:warning=NDI SDK for Windows not found — building without NDI.                  Install it or set NDI_SDK_DIR."
            );
            false
        }
    } else {
        false
    };
    if linked {
        println!("cargo:rustc-cfg=has_ndi");
        println!("cargo:has_ndi=1");
    }
    // The crate's own tests and examples link here too.
    for arg in &link_args {
        println!("cargo:rustc-link-arg={arg}");
    }
    println!("cargo:link_args={}", link_args.join(";"));
}

/// Locates the NDI SDK for Windows. The installer exports `NDI_SDK_DIR`, so an
/// explicit env var is the normal case and the Program Files path is the
/// fallback for a machine where the variable was never picked up.
fn ndi_sdk_windows() -> Option<String> {
    println!("cargo:rerun-if-env-changed=NDI_SDK_DIR");
    let candidates = [
        std::env::var("NDI_SDK_DIR").ok(),
        Some("C:/Program Files/NDI/NDI 6 SDK".to_string()),
    ];
    candidates.into_iter().flatten().find(|dir| {
        std::path::Path::new(dir)
            .join("Include/Processing.NDI.Lib.h")
            .is_file()
    })
}

/// Generates FFI bindings against the NDI SDK for Windows and links its
/// import library. Unlike Android/iOS the DLL is redistributed alongside the
/// executable, which the application's packaging must do. Returns the linker
/// arguments the final artifact must pass (see the module docs).
fn link_ndi_windows(sdk_dir: &str) -> Vec<String> {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    // The Windows SDK ships x64 and x86 only; an ARM64 host runs the x64
    // build under emulation rather than getting its own slice.
    let slice = match arch.as_str() {
        "x86_64" => "x64",
        "x86" => "x86",
        a => panic!("NDI for Windows has no {a} slice (x64/x86 only)"),
    };
    println!("cargo:rustc-link-search=native={sdk_dir}/Lib/{slice}");
    println!("cargo:rustc-link-lib=dylib=Processing.NDI.Lib.{slice}");

    let bindings = bindgen::Builder::default()
        .header(format!("{sdk_dir}/Include/Processing.NDI.Lib.h"))
        .allowlist_function("NDIlib_.*")
        .allowlist_type("NDIlib_.*")
        .allowlist_var("NDIlib_.*")
        .derive_default(true)
        .prepend_enum_name(false)
        .generate()
        .expect("bindgen failed for NDI headers");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("ndi_ffi.rs"))
        .expect("failed to write ndi_ffi.rs");

    // Delay-load the NDI DLL so it is not mapped until the first NDI call.
    //
    // The SDK reads ndi-config.v1.json when the library loads, not at
    // NDIlib_initialize, and honours NDI_CONFIG_DIR only if that variable is
    // already set at that moment. With a normal import the DLL maps at process
    // start - before any of our code runs - so configure()'s machinename and
    // adapters settings would be read from the default location and silently
    // ignored. Delaying the load moves it after configure(), which is what
    // makes the config file take effect at all on Windows.
    vec![
        format!("/DELAYLOAD:Processing.NDI.Lib.{slice}.dll"),
        "delayimp.lib".to_string(),
    ]
}

/// A device platform whose NDI SDK for Apple library is a static archive.
struct AppleStatic {
    /// Folder under `lib/` and the archive's link name there.
    dir: &'static str,
    lib: &'static str,
    /// xcrun SDK name and the clang target for bindgen and compiler-rt.
    sdk: &'static str,
    target: &'static str,
}

impl AppleStatic {
    /// iOS and tvOS devices. Their simulators have no NDI slice.
    fn for_target(target_os: &str, target_abi: &str) -> Option<Self> {
        if target_abi == "sim" {
            return None;
        }
        match target_os {
            "ios" => Some(Self {
                dir: "iOS",
                lib: "ndi_ios",
                sdk: "iphoneos",
                target: "arm64-apple-ios13.0",
            }),
            "tvos" => Some(Self {
                dir: "tvOS",
                lib: "ndi_tvos",
                sdk: "appletvos",
                target: "arm64-apple-tvos13.0",
            }),
            _ => None,
        }
    }
}

fn apple_sdk_dir() -> String {
    println!("cargo:rerun-if-env-changed=NDI_SDK_APPLE_DIR");
    std::env::var("NDI_SDK_APPLE_DIR").unwrap_or_else(|_| "/Library/NDI SDK for Apple".to_string())
}

/// Generates FFI bindings against the NDI SDK for Apple and emits link flags
/// for the platform's static libndi, bundled into this crate's rlib so the
/// final static library needs no NDI flags of its own.
fn link_ndi_apple_static(platform: &AppleStatic) {
    let sdk_dir = apple_sdk_dir();
    let out = std::env::var("OUT_DIR").unwrap();
    let source = format!("{sdk_dir}/lib/{}/lib{}.a", platform.dir, platform.lib);
    let dest = format!("{out}/lib{}.a", platform.lib);
    // A fat archive (device + simulator) cannot be bundled by the archive
    // reader in rustc: thin it to the device slice. A thin one is used as is.
    let archs = std::process::Command::new("lipo")
        .args(["-archs", &source])
        .output()
        .map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string())
        .unwrap_or_default();
    if archs.split_whitespace().count() > 1 {
        let status = std::process::Command::new("lipo")
            .args(["-thin", "arm64", &source, "-output", &dest])
            .status()
            .expect("failed to run lipo on the NDI archive");
        assert!(status.success(), "lipo -thin arm64 {source} failed");
    } else {
        std::fs::copy(&source, &dest).unwrap_or_else(|e| panic!("copying {source}: {e}"));
    }
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static={}", platform.lib);
    write_apple_bindings(&sdk_dir, platform.sdk, platform.target);
}

/// Links libndi.dylib from the NDI SDK for Apple (macOS, universal). The app
/// bundles the dylib (Contents/Frameworks) and adds that rpath itself; the
/// returned rpath to the SDK copy lets tests and development builds run.
fn link_ndi_macos() -> Vec<String> {
    let sdk_dir = apple_sdk_dir();
    let lib_dir = format!("{sdk_dir}/lib/macOS");
    println!("cargo:rustc-link-search=native={lib_dir}");
    println!("cargo:rustc-link-lib=dylib=ndi");
    write_apple_bindings(&sdk_dir, "macosx", "arm64-apple-macos11");
    vec![format!("-Wl,-rpath,{lib_dir}")]
}

fn write_apple_bindings(sdk_dir: &str, xcrun_sdk: &str, target: &str) {
    let sysroot = apple_sdk_path(xcrun_sdk);
    let bindings = bindgen::Builder::default()
        .header(format!("{sdk_dir}/include/Processing.NDI.Lib.h"))
        .clang_arg(format!("--target={target}"))
        .clang_arg(format!("-isysroot{sysroot}"))
        .allowlist_function("NDIlib_.*")
        .allowlist_type("NDIlib_.*")
        .allowlist_var("NDIlib_.*")
        .derive_default(true)
        .prepend_enum_name(false)
        .generate()
        .expect("bindgen failed for NDI headers (Apple)");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("ndi_ffi.rs"))
        .expect("failed to write ndi_ffi.rs");
}

fn apple_clang_rt_path(platform: &AppleStatic) -> Option<String> {
    let output = std::process::Command::new("xcrun")
        .args([
            "--sdk",
            platform.sdk,
            "clang",
            "-print-libgcc-file-name",
            "-target",
            platform.target,
        ])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
}

/// Locates the NDI SDK for Linux (its installer unpacks "NDI SDK for Linux"
/// wherever it runs): `NDI_SDK_LINUX_DIR`, else /opt.
fn ndi_sdk_linux() -> Option<String> {
    println!("cargo:rerun-if-env-changed=NDI_SDK_LINUX_DIR");
    let candidates = [
        std::env::var("NDI_SDK_LINUX_DIR").ok(),
        Some("/opt/NDI SDK for Linux".to_string()),
    ];
    candidates.into_iter().flatten().find(|dir| {
        std::path::Path::new(dir)
            .join("include/Processing.NDI.Lib.h")
            .is_file()
    })
}

/// Links libndi.so from the NDI SDK for Linux, per architecture. The app
/// ships libndi.so.6 beside the executable or on the library path; the
/// returned rpath to the SDK copy lets tests and development builds run.
fn link_ndi_linux(sdk_dir: &str) -> Vec<String> {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let triple = match arch.as_str() {
        "x86_64" => "x86_64-linux-gnu",
        "aarch64" => "aarch64-rpi4-linux-gnueabi",
        "arm" => "arm-rpi4-linux-gnueabihf",
        "x86" => "i686-linux-gnu",
        a => panic!("NDI for Linux has no {a} slice"),
    };
    let lib_dir = format!("{sdk_dir}/lib/{triple}");
    println!("cargo:rustc-link-search=native={lib_dir}");
    println!("cargo:rustc-link-lib=dylib=ndi");
    let bindings = bindgen::Builder::default()
        .header(format!("{sdk_dir}/include/Processing.NDI.Lib.h"))
        .allowlist_function("NDIlib_.*")
        .allowlist_type("NDIlib_.*")
        .allowlist_var("NDIlib_.*")
        .derive_default(true)
        .prepend_enum_name(false)
        .generate()
        .expect("bindgen failed for NDI headers (Linux)");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("ndi_ffi.rs"))
        .expect("failed to write ndi_ffi.rs");
    vec![format!("-Wl,-rpath,{lib_dir}")]
}

fn apple_sdk_path(sdk: &str) -> String {
    let output = std::process::Command::new("xcrun")
        .args(["--sdk", sdk, "--show-sdk-path"])
        .output()
        .expect("xcrun --show-sdk-path failed");
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

/// The NDK's prebuilt-toolchain directory name for the machine running the
/// build (bindgen runs the host's libclang against the NDK sysroot).
fn ndk_host_tag() -> &'static str {
    let host = std::env::var("HOST").unwrap_or_default();
    if host.contains("windows") {
        "windows-x86_64"
    } else if host.contains("apple") {
        // The NDK ships one macOS toolchain (universal), under this name.
        "darwin-x86_64"
    } else {
        "linux-x86_64"
    }
}

/// Links against libndi.so from the NDI SDK for Android (C API, per-ABI .so)
/// and generates FFI bindings from its headers with bindgen.
fn link_ndi_android() {
    let arch = std::env::var("CARGO_CFG_TARGET_ARCH").unwrap_or_default();
    let abi = match arch.as_str() {
        "aarch64" => "arm64-v8a",
        "arm" => "armeabi-v7a",
        "x86_64" => "x86_64",
        "x86" => "x86",
        a => panic!("unsupported Android ABI: {a}"),
    };
    let sdk_dir = std::env::var("NDI_SDK_ANDROID_DIR")
        .unwrap_or_else(|_| "C:/Program Files/NDI/NDI 6 SDK (Android)".to_string());
    println!("cargo:rustc-link-search=native={sdk_dir}/Lib/{abi}");
    println!("cargo:rustc-link-lib=dylib=ndi");
    println!("cargo:rerun-if-env-changed=NDI_SDK_ANDROID_DIR");

    // bindgen needs the NDK sysroot for libc headers and the NDK clang
    // builtins (stdbool.h etc.).
    let ndk_home = std::env::var("ANDROID_NDK_HOME")
        .expect("ANDROID_NDK_HOME must be set (cargo-ndk sets it)");
    let toolchain = format!("{ndk_home}/toolchains/llvm/prebuilt/{}", ndk_host_tag());
    let clang_version = std::fs::read_dir(format!("{toolchain}/lib/clang"))
        .ok()
        .and_then(|mut d| d.next())
        .and_then(|e| e.ok())
        .map(|e| e.file_name().to_string_lossy().into_owned())
        .expect("NDK clang resource dir not found");

    let target = std::env::var("TARGET").unwrap();
    let bindings = bindgen::Builder::default()
        .header(format!("{sdk_dir}/Include/Processing.NDI.Lib.h"))
        .clang_arg(format!("--target={target}"))
        .clang_arg(format!("--sysroot={toolchain}/sysroot"))
        .clang_arg(format!("-I{toolchain}/lib/clang/{clang_version}/include"))
        .allowlist_function("NDIlib_.*")
        .allowlist_type("NDIlib_.*")
        .allowlist_var("NDIlib_.*")
        .derive_default(true)
        .prepend_enum_name(false)
        .generate()
        .expect("bindgen failed for NDI headers");
    let out = std::path::PathBuf::from(std::env::var("OUT_DIR").unwrap());
    bindings
        .write_to_file(out.join("ndi_ffi.rs"))
        .expect("failed to write ndi_ffi.rs");
}

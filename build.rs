//! Links the NDI SDK for the target and generates its FFI bindings.
//!
//! Sets `cfg(has_ndi)` inside this crate when the SDK is linked: Android,
//! iOS devices (the SDK's arm64 slice is device-only, so simulator builds
//! stub NDI) and Windows x64/x86 (the only slices the Windows SDK ships)
//! when the SDK is installed.
//!
//! Dependents learn the outcome through `links = "ndi"` metadata:
//!  - `DEP_NDI_HAS_NDI=1` when the SDK is linked;
//!  - `DEP_NDI_LINK_ARGS`: `;`-separated linker arguments the final
//!    artifact must pass itself, because Cargo applies `rustc-link-arg` only
//!    to the package that emits it (the Windows delay-load, iOS clang_rt).
//!
//! Environment:
//!  - `NDI_SDK_DIR` (Windows), `NDI_SDK_ANDROID_DIR`, `NDI_SDK_APPLE_DIR`:
//!    SDK locations, defaulting to the installers' paths.
//!  - `NDI_REQUIRE_SDK=1`: fail the build instead of building without NDI
//!    when the Windows SDK is missing (for release builds).

fn main() {
    println!("cargo:rustc-check-cfg=cfg(has_ndi)");
    println!("cargo:rerun-if-env-changed=NDI_REQUIRE_SDK");
    let target_os = std::env::var("CARGO_CFG_TARGET_OS").unwrap_or_default();
    let target_abi = std::env::var("CARGO_CFG_TARGET_ABI").unwrap_or_default();
    let mut link_args: Vec<String> = Vec::new();
    let linked = if target_os == "android" {
        link_ndi_android();
        true
    } else if target_os == "ios" && target_abi != "sim" {
        // Frameworks libndi_ios.a depends on.
        for framework in ["AudioToolbox", "Foundation", "AVFoundation", "Network", "Accelerate"] {
            println!("cargo:rustc-link-lib=framework={framework}");
        }
        // libndi references ___chkstk_darwin (compiler-rt). rustc links
        // with -nodefaultlibs, so clang_rt must be passed explicitly.
        // (Xcode's own link of the app gets it automatically from clang.)
        if let Some(clang_rt) = ios_clang_rt_path() {
            link_args.push(clang_rt);
        }
        link_ndi_apple();
        true
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

/// Generates FFI bindings against the NDI SDK for Apple and emits link flags
/// for `libndi_ios.a`, bundled into this crate's rlib so the final static
/// library needs no NDI flags of its own.
fn link_ndi_apple() {
    let sdk_dir = std::env::var("NDI_SDK_APPLE_DIR")
        .unwrap_or_else(|_| "/Library/NDI SDK for Apple".to_string());
    let out = std::env::var("OUT_DIR").unwrap();
    // libndi_ios.a is a fat archive (arm64 device + x86_64 sim), which
    // rustc's archive reader cannot bundle. Thin it to the device slice.
    let status = std::process::Command::new("lipo")
        .args([
            "-thin",
            "arm64",
            &format!("{sdk_dir}/lib/iOS/libndi_ios.a"),
            "-output",
            &format!("{out}/libndi_ios.a"),
        ])
        .status()
        .expect("failed to run lipo on libndi_ios.a");
    assert!(status.success(), "lipo -thin arm64 libndi_ios.a failed");
    println!("cargo:rustc-link-search=native={out}");
    println!("cargo:rustc-link-lib=static=ndi_ios");
    println!("cargo:rerun-if-env-changed=NDI_SDK_APPLE_DIR");

    let sysroot = apple_sdk_path("iphoneos");
    let bindings = bindgen::Builder::default()
        .header(format!("{sdk_dir}/include/Processing.NDI.Lib.h"))
        .clang_arg("--target=arm64-apple-ios13.0")
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

fn ios_clang_rt_path() -> Option<String> {
    let output = std::process::Command::new("xcrun")
        .args([
            "--sdk",
            "iphoneos",
            "clang",
            "-print-libgcc-file-name",
            "-target",
            "arm64-apple-ios",
        ])
        .output()
        .ok()?;
    let path = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if path.is_empty() { None } else { Some(path) }
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

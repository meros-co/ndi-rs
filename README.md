# meros-ndi

NDI for Android, iOS and Windows through direct FFI against the NDI SDK:
SDK linking, configuration, discovery, receive and send.

```rust
meros_ndi::configure(&config_dir, "Stage-Left-iPad", "", None)?; // before any NDI call
let names = meros_ndi::discover_sources(2000);
let mut rx = meros_ndi::Receiver::connect(&names[0], meros_ndi::Bandwidth::Highest)?;
let mut tx = meros_ndi::Sender::new("My Output")?;
```

When the SDK is not linked for the target, `AVAILABLE` is `false` and calls
report NDI as unavailable rather than failing to build.

## SDKs

| Target | SDK | Environment variable (default) |
|---|---|---|
| Android | NDI 6 SDK (Android) | `NDI_SDK_ANDROID_DIR` (`C:/Program Files/NDI/NDI 6 SDK (Android)`); needs `ANDROID_NDK_HOME` |
| iOS (device) | NDI SDK for Apple | `NDI_SDK_APPLE_DIR` (`/Library/NDI SDK for Apple`) |
| Windows x64/x86 | NDI 6 SDK | `NDI_SDK_DIR` (`C:/Program Files/NDI/NDI 6 SDK`) |

On Windows a missing SDK builds without NDI and warns; set
`NDI_REQUIRE_SDK=1` in release builds to make it an error. The iOS simulator
has no NDI slice and always builds without NDI.

## Linking into a final artifact

Cargo applies `rustc-link-arg` only to the package whose build script
emits it, so the crate that produces the final library or executable must
forward this crate's linker arguments. Add to its build script:

```rust
if let Ok(args) = std::env::var("DEP_NDI_LINK_ARGS") {
    for arg in args.split(';').filter(|a| !a.is_empty()) {
        println!("cargo:rustc-link-arg={arg}");
    }
}
```

and depend on `meros-ndi` directly (`DEP_` variables reach direct dependents
only). This carries:

- **Windows:** `/DELAYLOAD` of the NDI DLL. Without it the DLL loads at
  process start, before `configure` has written its config, and the machine
  name and adapter settings are silently ignored.
- **iOS:** clang's compiler-rt, which libndi needs and rustc does not link.

`DEP_NDI_HAS_NDI=1` tells a dependent's build script that the SDK is linked.

Windows applications also ship `Processing.NDI.Lib.x64.dll` beside the
executable.

## Tests

`cargo test`. On Windows the NDI DLL must be on `PATH` for the SDK test, e.g.
`PATH="/c/Program Files/NDI/NDI 6 SDK/Bin/x64:$PATH" cargo test`.
`NDI_LIVE_SOURCE=<name>` adds a check against a real sender on the LAN.

## Licence

Proprietary, © Meros. NDI® is a registered trademark of Vizrt NDI AB; use of
the SDK is subject to its licence, and every application must meet NDI's
branding and licence terms itself.

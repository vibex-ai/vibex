# Vibex Mobile

`vibex-mobile` is the native GPUI client for iOS and Android. It links the
mobile platform implementations from `vendor/zed`, connects to the
authoritative desktop runtime, and renders Vibex Agent sessions as a GUI.

## Android

Prerequisites: Android SDK/NDK, `cargo-ndk`, and the Android Rust targets used
by the build (`arm64-v8a` plus `x86_64` for debug; `arm64-v8a` for release).

```bash
pnpm build:mobile:android
```

The command builds `libvibex_mobile.so` and packages a debug APK through the
checked-in NativeActivity Gradle project.

Set `VIBEX_MOBILE_ANDROID_TARGETS` to a space-separated ABI list to override
the defaults during local development.

## iOS

Prerequisites: Xcode, XcodeGen, and the `aarch64-apple-ios` plus
`aarch64-apple-ios-sim` Rust targets.

```bash
pnpm build:mobile:ios
```

The command builds `VibexFFI.xcframework` and generates the Xcode project.

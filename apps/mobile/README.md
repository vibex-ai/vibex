# Vibex Mobile

`vibex-mobile` is the native GPUI client for iOS and Android. It links the
mobile platform implementations from `vendor/zed`, connects to the
authoritative desktop runtime, and renders Vibex Agent sessions as a GUI.

## Android

Prerequisites: Android SDK/NDK, `cargo-ndk`, and the Android Rust targets used
by the build (`arm64-v8a` plus `x86_64` for debug; `arm64-v8a` for release).

### Test build

```bash
pnpm build:mobile:android
```

This command builds the debug native libraries for `arm64-v8a` and `x86_64`
and packages an installable debug APK at:

```text
apps/mobile/android/app/build/outputs/apk/debug/app-debug.apk
```

### Release build

```bash
pnpm package:mobile:android
```

This command builds the optimized native library for `arm64-v8a` and packages
an unsigned release APK at:

```text
apps/mobile/android/app/build/outputs/apk/release/app-release-unsigned.apk
```

The release APK must be aligned and signed with the intended release key before
it can be installed on a device or distributed.

Tagged GitHub releases run `apps/mobile/scripts/sign-android-release.sh` before
publishing Android artifacts. RC and preview releases use an ephemeral CI key
when no repository key is configured; stable releases require the repository's
release keystore secrets. The published APK is therefore installable without a
separate signing step.

Set `VIBEX_MOBILE_ANDROID_TARGETS` to a space-separated ABI list to override
the defaults during local development.

## iOS

Prerequisites: Xcode, XcodeGen, and the `aarch64-apple-ios` plus
`aarch64-apple-ios-sim` Rust targets.

```bash
pnpm build:mobile:ios
```

The command builds `VibexFFI.xcframework` and generates the Xcode project.

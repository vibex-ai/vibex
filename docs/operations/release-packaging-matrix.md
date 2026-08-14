# Release Packaging Matrix

Release artifacts are built from the checked-in Rust workspace and the native
mobile projects. Desktop packaging and mobile packaging are independent; neither
embeds the other product's assets.

| Product | Artifact | Build command | Determinism | Required validation |
| --- | --- | --- | --- | --- |
| Desktop Preview | Linux `.deb`/AppImage | `pnpm package:preview` | Source, Cargo lock, PDFium lock | `pnpm check:release`, package smoke, native content gates |
| Desktop RC/Stable | Platform package | `pnpm package:rc` / `pnpm package:stable` | Source, Cargo lock, release channel | Release runbook, signing, install and rollback evidence |
| Android mobile | Native GPUI APK | `pnpm build:mobile:android` or `pnpm package:mobile:android` | Rust source, Cargo lock, vendor/zed revision, Gradle wrapper | `pnpm check:mobile-native`, APK/device validation, signing pipeline |
| iOS mobile | Native GPUI app/XCFramework | `pnpm build:mobile:ios` on macOS | Rust source, Cargo lock, vendor/zed revision, XcodeGen project | `pnpm check:mobile-native`, simulator/device validation, signing pipeline |
| Relay | Transport container | `pnpm smoke:relay:local` plus deployment scripts | Rust source and Cargo lock | Health/API smoke, TLS/NAT/operator validation |

## Mobile Packaging Rules

- `apps/mobile` contains the source-owned Android and iOS project definitions.
- Rust is built as a shared library for Android and a static library packaged in
  an XCFramework for iOS.
- Build outputs (`jniLibs`, XCFramework contents, APKs, and Xcode projects) are
  ignored and must be regenerated from the exact source identity.
- Mobile release validation must exercise the GUI Agent timeline, pairing,
  reconnect, approvals, composer, and route selection. A terminal-only smoke is
  not sufficient for the product session surface.
- Signing credentials and provisioning profiles remain outside source control.

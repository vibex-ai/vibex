# Release Packaging Matrix

Release artifacts are built from the checked-in Rust workspace and the native
mobile projects. Desktop packaging and mobile packaging are independent; neither
embeds the other product's assets.

| Product | Artifact | Build command | Determinism | Required validation |
| --- | --- | --- | --- | --- |
| Desktop Preview | Linux `.deb`/AppImage | `pnpm package:preview` | Source, Cargo lock, PDFium lock | `pnpm check:release`, package smoke, native content gates |
| Desktop RC/Stable Linux x86_64 | `.deb`/AppImage | `pnpm package:rc` / `pnpm package:stable` | Source, Cargo lock, release channel, reviewed Linux PDFium | Release runbook, signing, install and rollback evidence |
| Desktop RC/Stable Linux aarch64 | `.deb` (cross-built, no PDFium payload) | `node scripts/package-desktop-release.mjs --platform linux --arch aarch64` | Source, Cargo lock, release channel, cross toolchain | Package existence and checksum; native validation deferred with the PDFium review |
| Desktop RC/Stable macOS aarch64/x86_64 | `.dmg` | `node scripts/package-desktop-release.mjs --platform macos --arch aarch64\|x86_64` | Source, Cargo lock, native macOS runner | Package existence and checksum; signing/notarization when credentials exist |
| Desktop RC/Stable Windows x86_64/aarch64 | NSIS `.exe` | `node scripts/package-desktop-release.mjs --platform windows --arch x86_64\|aarch64` | Source, Cargo lock, native Windows runner (aarch64 via cross-compile) | Package existence and checksum; Authenticode when credentials exist |
| Android mobile | Signed native GPUI APK + AAB | `pnpm package:mobile:android` locally; tagged workflow signs before upload | Rust source, Cargo lock, vendor/zed revision, Gradle wrapper, Android API 35/NDK, release key | `pnpm check:mobile-native`, `apksigner`/`jarsigner` verification, APK/AAB checksum, device validation |
| iOS mobile | Unsigned simulator app + XCFramework | `pnpm build:mobile:ios` on macOS | Rust source, Cargo lock, vendor/zed revision, XcodeGen project | `pnpm check:mobile-native`, simulator/device validation and signing pipeline |
| Relay | Transport container | `pnpm smoke:relay:local` plus deployment scripts | Rust source and Cargo lock | Health/API smoke, TLS/NAT/operator validation |

## Linux AppImage Packaging Rules

- Keep `libwayland*` outside the AppImage. Wayland client, cursor, and EGL
  libraries form a host graphics ABI boundary and must match the installed
  Mesa or proprietary driver stack.
- `pnpm check:release` enforces the exclusion across the Linux, Preview, RC,
  and Stable AppImage configurations.
- AppImage packaging stays x86_64-only. The linuxdeploy toolchain cannot
  process cross-architecture binaries, so aarch64 releases ship the `.deb`
  package instead of an AppImage.

## Cross-Architecture Packaging Rules

- Non-native targets are built with `cargo build --target <triple>` through
  `scripts/build-channel.mjs --target` and packaged with
  `cargo packager --target <triple>`.
- Linux aarch64 cross-builds on the x86_64 runner require the Ubuntu ports
  apt sources, the `aarch64-linux-gnu` GNU toolchain, the `:arm64` GTK/WebKit
  development packages, and the pkg-config/cargo cross environment.
- Multi-Arch `:same` packages must hold one version per architecture, so the
  workflow aligns every installed `:same` package to the version published for
  arm64 before the `:arm64` packages install (`--allow-downgrades` also covers
  a ports archive that lags behind amd64). Ubuntu publishes amd64 and ports
  updates hours apart, and a single drifted pair — python3.12, libc6,
  graphite2, harfbuzz, freetype, libssl3 — otherwise makes apt reject the
  whole `:arm64` set with "held broken packages".
- The Linux aarch64 deb, macOS, and Windows packages carry no embedded PDFium
  runtime: distribution approval is scoped to linux-x86_64.

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
- Tagged releases use `.github/workflows/release.yml`: native desktop jobs and
  mobile jobs upload to the GitHub Actions artifact store, then one aggregation
  job publishes all assets to the matching GitHub Release.
- Standard GitHub-hosted runners are the default cost-free path for public
  repositories. Optional updater signing is skipped unless its repository
  variable and secrets are explicitly enabled.

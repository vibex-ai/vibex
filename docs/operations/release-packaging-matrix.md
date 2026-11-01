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
| Android mobile | Signed native GPUI APK + AAB | `pnpm package:mobile:android` locally; tagged workflow signs before upload | Rust source, Cargo lock, pinned gpui-pre/gpui-pre-mobile revisions, Gradle wrapper, Android API 35/NDK, release key | `pnpm check:mobile-native`, `apksigner`/`jarsigner` verification, APK/AAB checksum, device validation |
| iOS mobile | Unsigned simulator app + unsigned device IPA + XCFramework | `pnpm build:mobile:ios` and `pnpm package:mobile:ios` on macOS | Rust source, Cargo lock, pinned gpui-pre/gpui-pre-mobile revisions, XcodeGen project | `pnpm check:mobile-native`, simulator/device validation; the installer re-signs the IPA, see [Installing the Unsigned iOS Build](./ios-unsigned-install.md) |
| Headless server Linux x86_64/aarch64 | `vibex-server-<version>-linux-<arch>.tar.gz` + `.sha256` | `deploy/server/Dockerfile.build` in the tagged workflow; the link step runs inside the pinned Debian 12 image | Debian 12 base digest, Rust 1.97.0, Cargo lock, deterministic tar | `scripts/check-server-artifact.mjs` (ELF arch, no `libdbus`/`libsystemd`, `GLIBC_*` ≤ 2.35), `sha256sum -c`, clean Ubuntu 22.04/24.04 `--version` and `config-check` |
| Relay | Transport container | `pnpm smoke:relay:local` plus deployment scripts | Rust source and Cargo lock | Health/API smoke, TLS/NAT/operator validation |
| Terminal (TUI) | Ships inside the desktop packages as a second, non-main `vibex` binary | Same package commands as the desktop channel | Same as the desktop channel | The binary must be reachable on `PATH` after install on Linux and macOS, and the console subsystem must be verified on Windows. |
| AUR `vibex-bin` | `PKGBUILD` + `.SRCINFO` that unpack the published Linux `.deb` | `.github/workflows/publish-aur.yml`; local dry run is `node scripts/publish-aur.mjs --version <x.y.z> --dry-run` | Stable only; the checksums of the release `.deb` payloads | `makepkg` must build the package from the release assets, `namcap` runs on it, and `publish-aur.mjs` refuses a prerelease or a version that is not newer than the AUR's |

## Linux AppImage Packaging Rules

- Keep `libwayland*` outside the AppImage. Wayland client, cursor, and EGL
  libraries form a host graphics ABI boundary and must match the installed
  Mesa or proprietary driver stack.
- `pnpm check:release` enforces the exclusion across the Linux, Preview, RC,
  and Stable AppImage configurations.
- AppImage packaging stays x86_64-only. The linuxdeploy toolchain cannot
  process cross-architecture binaries, so aarch64 releases ship the `.deb`
  package instead of an AppImage.

## Linux deb Packaging Rules

- cargo-packager writes the deb payload as `control.tar.gz` + `data.tar.gz` and
  exposes no compression setting, so every packaging command repacks both
  members into xz with `scripts/compress-deb.mjs`. Only those two tar members
  change; the installed tree and the control fields stay untouched.
- The repack uses xz preset 9 and removes more than a third of the published
  `.deb` (the 0.1.0-rc.8 preview payload drops from 119.8 MB to 69.8 MB).
- Building a `.deb` requires `xz` on the packaging host; the repacked package
  must still extract with `dpkg-deb -x`/`-e`.

## Linux Binary Linking Rules

- Linux x86_64 release links run through lld with `--icf=all` from
  `scripts/build-channel.mjs`: identical code folding removes the duplicate
  monomorphised bodies, which the GNU bfd linker rustc reaches by default
  cannot do because it has no ICF implementation.
- The flags are scoped to the `x86_64-unknown-linux-gnu` target, so macOS and
  Windows links are unchanged, and the packaging host needs `ld.lld` (the
  `lld` package on Debian/Ubuntu) before a release build can link.

## Headless Server Packaging Rules

- Linux server artifacts are linked inside `deploy/server/Dockerfile.build`, a
  Debian 12 image pinned by digest. Never build them on the `ubuntu-24.04`
  release runner: its glibc is too new and the artifact would not run on
  Ubuntu 22.04.
- The supported floor is glibc 2.35 (Ubuntu 22.04). Building on Debian 12 is
  not enough on its own; `scripts/check-server-artifact.mjs` fails when any
  referenced `GLIBC_*` symbol is newer than 2.35.
- The headless server must not link `libdbus` or `libsystemd`; the OS keychain
  is an opt-in desktop/TUI feature. The same checker enforces this.
- The tarball contains exactly one top-level `vibex-server` file, plus a
  `.sha256` beside it. The signed updater manifest carries the entries in
  `server_artifacts`, selected by OS and architecture.
- macOS server artifacts need Developer ID signing and notarization; they stay
  deferred until those credentials are available and do not block the Linux
  artifacts.

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

## macOS App Icon Rules

- macOS draws an app icon exactly as the bundle provides it: it neither pads nor
  masks the artwork. The macOS ICNS inputs are therefore the `icon-macos-*.png`
  grid renditions — an 824x824 body centred on a 1024x1024 canvas with the
  platform corner radius — while Linux, Windows, and the tray keep the
  full-bleed `icon-*.png` renditions, whose shape their window manager owns.
- A full-bleed rendition in the macOS bundle renders roughly a quarter larger
  than neighbouring Dock icons, so `pnpm check:release` rejects a macOS icon set
  that falls back to the full-bleed files.
- Regenerating the `icon-macos-*.png` renditions from a new master icon is part
  of the artwork change, not a packaging step: the checked-in files are reviewed
  and audited like every other reviewed asset input.

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

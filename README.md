# Vibex

Vibex is a Rust-first, local-first AI coding workbench. It ships a native GPUI
desktop client and a native GPUI mobile client. Both clients use the same typed
session projection and design language; the desktop runtime remains the sole
authority for Agent sessions, files, Git, terminals, providers, and permissions.

| Surface | Source | Stack |
| --- | --- | --- |
| Native desktop | `apps/desktop` | Rust + GPUI |
| Native mobile | `apps/mobile` | Rust + GPUI, `gpui_ios` / `gpui_android` from `vendor/zed` |
| Relay server | `apps/relay-server` | Rust + Axum, zero-knowledge transport |

Mobile is a remote client. It renders the desktop Agent session model as a GUI
timeline with Markdown, tool/process details, approvals, and a composer. It does
not turn the session into a terminal workflow and it does not own local Agent,
filesystem, Git, or PTY state.

Run commands from the repository root.

## Prerequisites

CI uses Node.js 22, pnpm 11.3.0, and Rust 1.97.0. Rust is pinned by
`rust-toolchain.toml`; pnpm is pinned by `package.json`.

```bash
git submodule update --init --recursive
pnpm install --frozen-lockfile
```

Desktop development also needs a graphical session and a working Vulkan driver.
Native mobile builds additionally require the platform toolchain described below.

## Repository Checks

```bash
pnpm check
pnpm check:mobile-native
pnpm release:build-smoke
```

`pnpm check` covers the normal Rust, frontend, license, and deterministic
behavior gates. `check:mobile-native` checks the mobile crate and native project
contract; it does not claim that an Android or iOS SDK is installed on the host.

## Desktop

Run the native workbench:

```bash
pnpm dev:desktop
```

Targeted checks:

```bash
cargo check -p vibex-desktop --locked
cargo test -p vibex-desktop --locked
pnpm smoke:first-frame
```

Linux release packages use cargo-packager and the reviewed PDFium runtime:

```bash
cargo install cargo-packager --version 0.11.8 --locked
pnpm prepare:pdfium
pnpm package:preview
```

See [the desktop release runbook](docs/operations/release.md) before using the
RC or Stable packaging commands.

## Native Mobile

`apps/mobile` is a checked-in native project. The Rust library links the
platform implementations from `vendor/zed`; Android uses a NativeActivity and
`gpui_android`, while iOS uses the UIKit platform in `gpui_ios` and a small
Objective-C application host. The mobile UI follows the compact, dark visual
language used by Zedra while preserving Vibex's GUI Agent timeline semantics.

Both mobile backends recognize single-finger gestures before GPUI sees them: a
tap is replayed as mouse down/up at the touch-down point, and a drag past the
touch slop becomes a `ScrollWheel` stream carrying a `TouchPhase`. Mouse-move
events therefore only ever describe a tap, so swipe handling — the session
drawer, for one — must read the scroll stream rather than pointer movement.

### Android

Install Android SDK/NDK, `cargo-ndk`, and the Rust targets for the ABIs you will
build. Debug defaults to `arm64-v8a` and `x86_64`; release defaults to
`arm64-v8a`.

```bash
pnpm build:mobile:android
```

For a release-profile APK:

```bash
pnpm package:mobile:android
```

Set `VIBEX_MOBILE_ANDROID_TARGETS` to a space-separated ABI list to override the
debug defaults. The generated native libraries and APKs are build outputs and
are intentionally not tracked.

### iOS

On macOS, install Xcode, XcodeGen, and the `aarch64-apple-ios` and
`aarch64-apple-ios-sim` Rust targets:

```bash
pnpm build:mobile:ios
```

The command creates `VibexFFI.xcframework` and generates the Xcode project. Code
signing, simulator/device selection, and distribution credentials remain local
to the developer or release pipeline.

## Remote and Relay

The desktop runtime publishes Direct and optional self-hosted Relay routes. The
mobile client pairs with a one-time offer, persists only the required credential
bundle in its app sandbox, and chooses a validated route through
`AutoRemoteTransport`. Relay forwards encrypted frames and never becomes a
second state authority.

```bash
pnpm smoke:relay:local
```

## Common Build Problems

- Android SDK or NDK not found: set `ANDROID_HOME`/`ANDROID_SDK_ROOT` and make
  sure the requested platform and build tools are installed.
- `cargo-ndk` cannot find an ABI: install the corresponding Rust target and use
  `VIBEX_MOBILE_ANDROID_TARGETS` to select only installed ABIs.
- iOS project generation fails: install XcodeGen and run the command on macOS.
- Desktop package reports a missing PDFium runtime: run `pnpm prepare:pdfium`.

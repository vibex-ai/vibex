# Vibex

Vibex is a Rust-first, local-first AI coding workbench. The repository contains
two product clients; the mobile client is split into a WASM runtime and a native
Capacitor shell:

| Surface | Source | Stack |
| --- | --- | --- |
| Native desktop | `apps/desktop` | Rust + GPUI |
| Mobile runtime | `apps/mobile-wasm` | Rust + GPUI-WASM, Compact/Medium only |
| Mobile shell | `apps/mobile` | Capacitor 8 + bundled mobile runtime |

Vibex does not ship a browser WebUI or PWA. The browser host for
`apps/mobile-wasm` is a development and automation tool only; it is not a
deployable product surface.

Run all commands below from the repository root unless a section says
otherwise.

## Prerequisites

CI uses Node.js 22, pnpm 11.3.0, and Rust 1.97.0. The Rust version is pinned by
`rust-toolchain.toml`, and the pnpm version is pinned by `package.json`.

```bash
git submodule update --init --recursive
corepack enable
pnpm install --frozen-lockfile
```

On Debian or Ubuntu, the complete native dependency set used by CI can be
installed with:

```bash
sudo apt-get update
sudo apt-get install -y \
  clang g++ gcc pkg-config \
  libfontconfig1-dev libglib2.0-dev libgtk-3-dev \
  libayatana-appindicator3-dev librsvg2-dev libssl-dev \
  libvulkan1 libwayland-dev libwebkit2gtk-4.1-dev \
  libx11-xcb-dev libxkbcommon-x11-dev libzstd-dev patchelf
```

## Repository Checks

Run the default formatting, lint, type, Rust, and deterministic behavior gates:

```bash
pnpm check
```

Run the release build smoke for the GPUI desktop, GPUI-WASM mobile shell, and
Relay binary:

```bash
pnpm release:build-smoke
```

Run the GPUI-WASM mobile runtime and package gate with:

```bash
pnpm check:wasm-gate
```

The default gate does not start real Agent CLI sessions. Provider smokes require
the corresponding local CLI, authentication, and network access.

## Desktop

### Run

Build and run the native GPUI desktop app in development mode:

```bash
cargo run -p vibex-desktop --locked
```

A graphical session and a working Vulkan driver are required to open the app.

### Test

```bash
cargo check -p vibex-desktop --locked
cargo test -p vibex-desktop --locked
pnpm smoke:first-frame
```

Use `pnpm check` when the complete GPUI contract and evidence suite is needed.

### Build a Release Binary

```bash
cargo build -p vibex-desktop --release --locked
```

The binary is written to `target/release/vibex-desktop` on Linux and
macOS, or `target/release/vibex-desktop.exe` on Windows.

### Build Linux Packages

Desktop packages use cargo-packager 0.11.8 and bundle the reviewed PDFium runtime.
Prepare the tools and runtime once, then build the local Preview channel:

```bash
cargo install cargo-packager --version 0.11.8 --locked
pnpm prepare:pdfium
pnpm package:preview
```

The `.deb` and AppImage files are written to
`target/release-packages/preview/`. The RC and Stable package commands are
release-controlled operations; follow the
[desktop release runbook](docs/operations/release.md) before using
`pnpm package:rc` or `pnpm package:stable`.

## Mobile GPUI-WASM Runtime

The mobile runtime builds the Rust GPUI remote interface for
`wasm32-unknown-unknown`. Install the pinned nightly target and the
`wasm-bindgen-cli` version locked by the workspace before the first build:

```bash
rustup toolchain install nightly-2026-07-24
rustup target add wasm32-unknown-unknown --toolchain nightly-2026-07-24
cargo install wasm-bindgen-cli --version 0.2.125 --locked
```

### Development Host

```bash
pnpm dev:mobile-wasm
```

Open <http://127.0.0.1:4173>. The command builds the debug runtime and serves it
from a fixed local host for development and automation. Do not deploy this host
or use it as a browser product.

### Test and Build

```bash
pnpm --filter @vibex/mobile-wasm typecheck
pnpm --filter @vibex/mobile-wasm build:release
pnpm check:wasm-integration
pnpm check:mobile-wasm-host
```

The Capacitor-ready HTML, host bridge, and GPUI-WASM files are written to
`apps/mobile-wasm/dist/`. There is no Service Worker, PWA manifest, offline Web
entry, or Wide shell. Agent, filesystem, Git, terminal, and provider operations
remain authoritative on the connected desktop runtime.

## Mobile

The Capacitor shell packages only the shared `apps/mobile-wasm/dist` client. Its
generated Android and iOS projects are intentionally ignored by Git.

### Validate the Shared Shell

```bash
pnpm --filter @vibex/mobile validate
```

This type-checks the Capacitor configuration and builds the GPUI-WASM mobile
runtime. It does not produce an APK or iOS app bundle.

### Android APK

Capacitor Android 8 requires JDK 21. Install Android command-line tools and at
least these SDK components:

```text
cmdline-tools;latest
platform-tools
platforms;android-36
build-tools;36.0.0
```

Set the paths to match the local JDK and Android SDK installations:

```bash
export JAVA_HOME="/path/to/jdk-21"
export ANDROID_HOME="/path/to/android-sdk"
export ANDROID_SDK_ROOT="$ANDROID_HOME"
export PATH="$JAVA_HOME/bin:$ANDROID_HOME/platform-tools:$ANDROID_HOME/cmdline-tools/latest/bin:$PATH"
```

Build the debug APK with the repository script. It generates the ignored native
project when needed, syncs a release GPUI-WASM bundle, builds the app, and
validates that the package contains only the expected GPUI-WASM client assets:

```bash
pnpm --filter @vibex/mobile android:debug
```

The debug APK is written to
`apps/mobile/artifacts/vibex-gate-debug.apk`. From the repository
root, install or replace it on a connected device with:

```bash
adb install -r apps/mobile/artifacts/vibex-gate-debug.apk
```

Build an unsigned release APK for an external signing pipeline with:

```bash
pnpm --filter @vibex/mobile android:release
```

The release APK is written to
`apps/mobile/artifacts/vibex-release-unsigned.apk`. Signing credentials
must remain outside source control.

Validate the source-bound mobile evidence and its checker with:

```bash
pnpm check:wasm-mobile
```

### iOS

iOS builds require macOS and Xcode. Build an unsigned simulator shell in Debug
or Release configuration with:

```bash
pnpm --filter @vibex/mobile ios:debug
pnpm --filter @vibex/mobile ios:release
```

The app bundles and build evidence are written under
`apps/mobile/artifacts/`. Physical device, TestFlight, and App Store builds
require an Apple signing team and separate release validation.

More mobile-specific connection and fixture details are documented in the
[GPUI mobile shell guide](apps/mobile/README.md). Runtime-host and physical
device validation status is tracked by the
[GPUI-WASM mobile runtime gate](docs/platform/mobile-wasm-runtime-gate.md).

## Common Build Problems

- Android `invalid source release: 21`: Gradle is using an older JDK. Check
  `JAVA_HOME` and `java -version` in the same terminal used for the build.
- Android `SDK location not found`: set `ANDROID_HOME` and `ANDROID_SDK_ROOT`,
  or add `sdk.dir=...` to the generated
  `apps/mobile/android/local.properties`.
- GPUI-WASM build reports a missing or mismatched `wasm-bindgen-cli`: install
  version 0.2.125, which must match `Cargo.lock`.
- desktop package reports a missing PDFium runtime: run
  `pnpm prepare:pdfium` before the channel package command.
- Port 4173 is busy: stop the existing mobile WASM development host, or set
  `PORT` when starting `pnpm dev:mobile-wasm`.

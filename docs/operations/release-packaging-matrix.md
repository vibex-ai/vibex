# Release Packaging Matrix

This matrix defines the release-readiness gate for local self-builds and
platform packaging. It records the commands maintainers should run before a tag
and separates deterministic local evidence from explicit manual evidence.

Local self-builds do not require code signing, notarization, store accounts, or
hosted update infrastructure. Signed and notarized distribution artifacts are a
separate release track.

## Evidence Classes

| Class | Meaning | Default release gate |
| --- | --- | --- |
| `deterministic` | Runs locally without real providers, public Relay, physical mobile devices, signing, notarization, hosted update checks, or network credential flows. | Required before tagging. |
| `explicit_manual` | Requires a platform host, Docker, public network path, physical device, real provider, or signing/store credential. | Required only when the release claims that surface. |
| `blocked_follow_up` | Known gap that is intentionally not available in this local environment or not implemented yet. | Must be named before tagging. |

## Local Build Smoke

Run the explicit release smoke when preparing a tag:

```bash
pnpm release:build-smoke
```

The smoke runs:

```bash
cargo check -p vibex-desktop --locked
pnpm --filter @vibex/mobile validate
cargo check -p vibex-relay-server --bin vibex-relay-server --locked
```

The mobile validation command builds the Capacitor-ready GPUI-WASM runtime through
`pnpm --filter @vibex/mobile-wasm build`, then type-checks the Capacitor shell.

This smoke is explicit release evidence and is intentionally not part of
`pnpm check`.

## Platform Matrix

| Surface | Platform | Command | Status | Pre-tag evidence | Known gap / follow-up |
| --- | --- | --- | --- | --- | --- |
| Desktop app | Linux local build path | `cargo check -p vibex-desktop --locked` | `deterministic` | Covered by `pnpm release:build-smoke`. | Compile evidence only; package and native behavior require a Linux release host. |
| Desktop packages | Linux `.deb` and AppImage | `pnpm package:stable` | `explicit_manual` | Record exact package hashes, install/upgrade/uninstall, native launch, and rollback to the prior published artifact. | Requires cargo-packager, PDFium preparation, Linux package tools, X11/Wayland, and the prior published artifact. |
| Desktop app/package | macOS | No stable package command is approved yet. | `blocked_follow_up` | Native macOS build/install/sign/notarize/rollback evidence is required before claiming support. | Requires a macOS host and a reviewed desktop package configuration. |
| Desktop app/package | Windows | No stable package command is approved yet. | `blocked_follow_up` | Native Windows build/install/sign/rollback evidence is required before claiming support. | Requires a Windows host and a reviewed desktop package configuration. |
| Mobile WASM runtime | Capacitor-ready bundled assets | `pnpm --filter @vibex/mobile-wasm build:release` | `deterministic` | `pnpm check:mobile-wasm-host` and `pnpm check:wasm-integration`. | Browser execution is development/automation evidence only; no hosted release exists. |
| Mobile shell | Capacitor config and bundled GPUI-WASM runtime | `pnpm --filter @vibex/mobile validate` | `deterministic` | `pnpm check:wasm-integration` plus the source-bound mobile evidence. | Does not generate or commit Android/iOS native projects. |
| Mobile native package | Android | `pnpm --filter @vibex/mobile android:debug` | `explicit_manual` | Android host summary with JDK 21, SDK/API level, emulator/device, APK install, launch, and screenshot status when claimed. | Requires JDK 21, Android SDK, and an emulator or device; the stable application id is `dev.vibex.remote`. |
| Mobile native package | iOS | `pnpm --filter @vibex/mobile ios:debug` or `ios:release` on macOS | `explicit_manual` | iOS host summary with Xcode version, simulator/device, install, launch, and screenshot status when claimed. | Requires macOS, Xcode, and Apple signing for device/TestFlight/App Store distribution. |
| Relay server | Binary target | `cargo check -p vibex-relay-server --bin vibex-relay-server` | `deterministic` | Covered by `pnpm release:build-smoke`. | Compile check only; container smoke is separate. |
| Relay server | Local container | `docker compose -f deploy/relay/docker-compose.yml up --build -d relay-server` plus `/health` and `/api/info` curls | `explicit_manual` | Redacted local Docker status summary when the release claims container readiness. | Requires Docker. Public HTTPS, DNS, and NAT proof are separate. |
| Relay remote proof | Public Relay/NAT/mobile path | `docs/smoke/relay-nat.md` | `explicit_manual` | Manual NAT/mobile proof summary when the release claims public Relay access. | Requires public network route, real devices, and explicit user-controlled Relay configuration. |

## Auto-update Policy

Vibex currently has no hosted GPUI updater. Auto-update is optional,
user-controllable follow-up work. Local self-builds must not contact hosted
update services and must remain usable without signing or notarization
credentials.

If a later release adds auto-update, the release notes and settings must make
the update source visible to the user, provide a user-controlled enable/disable
path, and keep unsigned local self-builds separate from hosted signed builds.

## Pre-tag Evidence Gate

Required deterministic evidence before tagging:

| Evidence | Command / source | Notes |
| --- | --- | --- |
| Default quality gate | `pnpm check` | Must not start real providers, public Relay, physical mobile, or credential flows. |
| Diagnostics bundle | `pnpm smoke:diagnostics` | Verifies bounded, redacted diagnostic export. |
| Backup/restore | `pnpm smoke:backup` | Verifies backup inspection and restore behavior. |
| Recovery matrix | `docs/operations/recovery-matrix.md` and focused tests | Defines restart and recovery semantics. |
| Performance baseline | `pnpm baseline:performance` | Records the current deterministic performance baseline. |
| E2E regression | `pnpm e2e:regression` | Runs the provider-free regression harness. |
| Release build smoke | `pnpm release:build-smoke` | GPUI desktop, mobile runtime/shell, and Relay local build evidence. |

Explicit manual evidence is required only for release claims that depend on it:

- real Codex, Claude Code, OpenCode, ACP, or scheduled-provider smokes;
- public Relay/NAT/mobile proof;
- physical Android/iOS build, install, launch, and screenshot proof;
- signed, notarized, store, or hosted auto-update distribution proof.

Every native package claim also requires an upgrade and rollback rehearsal using
the published artifact, not a source-only rebuild. The rehearsal must verify
the home lock, retained database/UI-state backups, uninstall/data-retention
behavior, and restoration to the prior published desktop artifact.

Evidence artifacts must stay bounded and redacted. Do not include secrets, auth
tokens, pairing codes, private keys, provider payloads, prompt bodies, terminal
output, raw Git diffs, raw logs, environment values, signing identities,
certificate paths, store-account identifiers, or generated native project files.

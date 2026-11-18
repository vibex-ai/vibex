# UI Architecture Boundary

## Authority

`.trellis/spec/guides/architecture-baseline.md` is the product and architecture
authority. `apps/desktop` is the information-architecture and Agent-session
baseline. `apps/mobile` derives the same session semantics through typed remote
projections and applies the native compact visual treatment used by Zedra.

## Product Boundary

- `apps/desktop` is the complete local GPUI workbench and owns the
  `DesktopRuntime`.
- `apps/mobile` is a native GPUI iOS/Android client built on the published
  `gpui-pre` family plus `gpui-pre-mobile`'s Android/iOS platform layer. It is
  the shipping mobile client until the Native Shell client reaches parity.
- `apps/mobile-nsh` is the iOS/Android client rebuilt on Native Shell (`nsh`):
  one Rust app whose views are the platform's own (UIKit, Android views), with
  the UI layer in `crates/vibex-nsh-ui`. It is a client under the same rules as
  the GPUI client; the two run side by side while screens migrate.
- Mobile renders a GUI Agent timeline: user messages, Markdown responses,
  process/tool details, approvals, and the composer. It does not replace the
  session page with a terminal UI.
- Mobile never owns Agent processes, filesystem state, Git state, PTYs, provider
  configuration, or permission authority. It sends typed mutations to the PC.
- Relay is an encrypted transport only. It does not serve application assets or
  become a second database.

## Shared Code Boundary

Shared across desktop and mobile:

- Domain DTOs and request/response semantics in `crates/core`.
- Framework-neutral timeline and session projections in `crates/desktop-model`.
- Backend capability contracts and workflow state in `crates/vibex-backend` and
  `crates/vibex-ui`.
- Semantic tokens in `crates/vibex-ui/theme/tokens.json`; the nsh client
  generates its theme file from the same source rather than defining colors.
- Platform-neutral pairing (entries, claims, credentials, the on-device store)
  in `crates/vibex-mobile-pairing`, shared by the mobile clients.
- Remote/Relay protocol behavior and tests.

The native mobile renderers own only platform bootstrap, pairing/storage, input,
and the compact composition of those shared projections. Neither client may
introduce a second Agent protocol, a local authority, or a separate
terminal-first session experience. `crates/vibex-nsh-ui` is composition only: it
depends on `nsh-core` and `nsh-base`, never on GPUI, transport or runtime code.

## Visual Contract

The structured token source is `crates/vibex-ui/theme/tokens.json`; generated Rust
values remain the only token implementation source. Desktop and mobile use the
same semantic colors, typography, radii, and state names. Mobile may reduce
density, use a session drawer, and turn auxiliary surfaces into sheets, but it
must preserve desktop Agent timeline semantics and explicit approval actions.

The native mobile surface follows Zedra's restrained dark palette, compact spacing,
small radii, clear hierarchy, and edge-drawer navigation. Product-specific
content remains Vibex GUI session content rather than Zedra's terminal workflow.

The nsh client mounts into the existing Kotlin and Swift shells (the nsh
platform crates' `NshView`/`AppView`), which keep the notifications, the
foreground service, the camera and the discovery bridge. Host observations
reach the app through its host-event channel; the shells never own state.
The nsh client draws the same palette from `apps/mobile-nsh/theme.json`
(generated from the token source; `cargo run -p vibex-mobile-nsh --bin
gen-theme` refreshes it, and a test fails when it is stale). Its parity contract
keeps information architecture, copy, spacing and states identical, while text
metrics, scrolling, the keyboard, selection and navigation transitions are the
platform's own.

## Required Checks

```text
node scripts/generate-tokens.mjs
pnpm check:mobile-native
cargo test -p vibex-ui --locked
cargo test -p vibex-mobile --locked
cargo test -p vibex-mobile-nsh --locked   # theme sync + headless pairing flows
```

Platform SDK builds are separate validation: Android requires the Android
SDK/NDK and `cargo-ndk`; iOS requires Xcode, XcodeGen, and the Apple Rust
targets.

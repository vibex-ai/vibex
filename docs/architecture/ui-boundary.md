# UI Architecture Boundary

## Authority

`.trellis/spec/guides/architecture-baseline.md` is the product and architecture
authority. `apps/desktop` is the information-architecture and Agent-session
baseline. `apps/mobile` derives the same session semantics through typed remote
projections and applies the native compact visual treatment used by Zedra.

## Product Boundary

- `apps/desktop` is the complete local GPUI workbench and owns the
  `DesktopRuntime`.
- `apps/mobile` is a native GPUI iOS/Android client built against
  `vendor/zed`'s `gpui_ios` and `gpui_android` implementations.
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
- Remote/Relay protocol behavior and tests.

The native mobile renderer owns only platform bootstrap, pairing/storage, input,
and the compact composition of those shared projections. It must not introduce a
second Agent protocol, a local authority, or a separate terminal-first session
experience.

## Visual Contract

The structured token source is `crates/vibex-ui/theme/tokens.json`; generated Rust
values remain the only token implementation source. Desktop and mobile use the
same semantic colors, typography, radii, and state names. Mobile may reduce
density, use a session drawer, and turn auxiliary surfaces into sheets, but it
must preserve desktop Agent timeline semantics and explicit approval actions.

The native mobile surface follows Zedra's restrained dark palette, compact spacing,
small radii, clear hierarchy, and edge-drawer navigation. Product-specific
content remains Vibex GUI session content rather than Zedra's terminal workflow.

## Required Evidence

```text
pnpm check:tokens
pnpm check:graph
pnpm check:mobile-native
cargo test -p vibex-ui --locked
cargo test -p vibex-mobile --locked
```

Platform SDK builds are separate evidence: Android requires the Android SDK/NDK
and `cargo-ndk`; iOS requires Xcode, XcodeGen, and the Apple Rust targets.

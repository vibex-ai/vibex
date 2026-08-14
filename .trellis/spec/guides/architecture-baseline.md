# Architecture Baseline

This document is the executable architecture contract for the current Vibex
products. It describes source-backed behavior, not a migration plan or a
historical client.

## Product Surfaces

Vibex has two clients and one optional transport service:

| Surface | Source | Responsibility |
| --- | --- | --- |
| Native desktop | `apps/desktop` | Full GPUI workbench and the authoritative `DesktopRuntime` |
| Native mobile | `apps/mobile` | iOS/Android GPUI remote client and compact GUI composition |
| Self-hosted Relay | `apps/relay-server` | Encrypted frame transport only |

The desktop runtime is the only state authority. Mobile never starts an Agent,
opens a local workspace, owns a PTY, mutates local Git, or stores provider
configuration. It requests typed operations and renders authoritative responses.

## Ownership Invariants

1. `DesktopRuntime` owns Agent lifecycle, session timelines, files, Git, PTY,
   provider configuration, device permissions, audit records, and durable local
   state.
2. `crates/core` owns serialized domain ids, DTOs, errors, capabilities, and
   protocol contracts. It has no GPUI, database, filesystem, or provider
   implementation dependency.
3. `crates/desktop-model` owns framework-neutral timeline/session projections and
   reducers. It does not open sockets or call runtime services.
4. `crates/vibex-backend` owns the provider-neutral capability facade. Native and
   remote adapters implement the same domain traits.
5. `crates/vibex-ui` owns semantic tokens, portable component models, workflow
   controllers, and shell composition. Views do not own transport or authority.
6. `crates/vibex-remote-client` owns pairing, reconnect, synchronization, and
   Direct/Tailnet/Relay transport. Relay never becomes a second database.

## Session Presentation Contract

Desktop is the visual and information-architecture source for Agent sessions.
Mobile projects that same GUI contract into a compact composition:

- user messages and Agent Markdown are rendered as timeline content;
- tool/process details are explicit, bounded, and collapsible;
- permission requests are prominent cards with approve/deny actions;
- elicitation requests remain typed GUI forms with explicit submit/decline;
- the composer supports send, stop, and continue according to authoritative
  session state;
- an empty desktop session list exposes typed new-session creation using a
  desktop-owned workspace and published Agent runtime;
- reconnect and loading states remain visible and never fabricate local history.

Mobile may use a session drawer, edge swipe, sheets, reduced density, and a
Zedra-aligned dark palette. It must not turn the Agent session page into a
terminal-first workflow, copy a separate domain component family, or silently
drop approval/error states.

## Native Platform Boundary

`apps/mobile` is a Rust crate with `cdylib`, `staticlib`, and `rlib` outputs.
It calls `gpui_platform::application()` and links the platform implementations
from the `vendor/zed` submodule:

- Android enters through `android_main`, initializes `gpui_android`, and is
  packaged as a NativeActivity Gradle application.
- iOS exports `vibex_mobile_main`; `gpui_ios` owns the UIKit application loop,
  and a small Objective-C host supplies the executable entry point.

The checked-in project definitions live under `apps/mobile/android` and
`apps/mobile/ios`. Generated libraries, XCFramework contents, Xcode projects,
and APKs are build outputs and are never source dependencies.

GPUI owns platform safe-area and IME inset reporting. The application loads its
reviewed Latin/CJK fonts before opening the window and applies effective insets
at the root composition; device-specific notch or keyboard constants do not
belong in mobile Views.

## Remote Data Flow

```text
DesktopRuntime
  -> RemoteGateway (typed v2 HTTP/WebSocket API)
  -> Direct/Tailnet or encrypted self-hosted Relay
  -> AutoRemoteTransport
  -> WebRemoteBackend
  -> AgentWorkflowController / shared UI projections
  -> native mobile GPUI views
```

Pairing is one-time and server-issued. The mobile app validates the offer,
claims exactly one route, persists the minimum credential bundle in its sandbox,
and keeps session/private keys in memory. Direct candidates and Relay routes are
validated before use; authentication, protocol, permission, and identity failures
are terminal rather than automatic route-reselection triggers.

The server timeline and sequence cursors are authoritative. On reconnect, gap,
queue overflow, or session-epoch change, the client refetches before applying
new live events. Unknown mutations are resolved by an idempotency query and are
never replayed blindly.

## Layout And Design

The shared token source is `crates/vibex-ui/theme/tokens.json`; generated Rust
constants are derived and checked. Desktop owns Wide/Medium/Compact information
architecture. Mobile uses the Compact composition as its primary surface and
borrows Medium behavior only where the viewport permits it.

The mobile visual baseline follows Zedra's restrained dark treatment: near-black
background, subtle elevated surfaces, small radii, compact typography, clear
secondary text, and explicit icon actions. This is a visual treatment only; all
Vibex session labels, timeline states, approval semantics, and remote workflows
remain Vibex-owned and desktop-compatible.

Touch actions must be explicit and usable without hover. Fixed-format controls
have stable dimensions. Loading, empty, streaming, failure, reconnecting,
approval-pending, and destructive-confirmation states are all first-class.

## Dependency Direction

```text
apps/desktop -> NativeBackend -> typed domain services -> crates/core
apps/mobile  -> remote facade -> RemoteGateway -> DesktopRuntime
Relay        -> encrypted frames only
```

No shared UI module may import a runtime composition root, database, socket,
provider SDK, or platform host. No mobile module may call a desktop service
directly or introduce a second authority.

## Security And Privacy

- Native mobile credential files are written atomically with restrictive Unix
  permissions where supported; malformed or mismatched records are discarded.
- Auth tokens, private keys, pairing links, prompt bodies, file contents, and
  terminal bytes are redacted from `Debug`, logs, and evidence.
- RemoteGateway validates Host and HTTP(S) Origin boundaries and rejects secrets
  in URLs. LAN mode requires an explicitly trusted HTTPS/WSS boundary.
- Relay forwards encrypted payloads and has no provider, workspace, or Agent
  authorization logic.

## Required Gates

```bash
pnpm check:graph
pnpm check:licenses
pnpm check:mobile-native
cargo test -p vibex-ui --locked
cargo test -p vibex-mobile --locked
pnpm smoke:relay:local
```

Android and iOS builds are separately qualified on hosts with their SDKs and
devices. A host-side Rust check does not claim physical rendering or signing.

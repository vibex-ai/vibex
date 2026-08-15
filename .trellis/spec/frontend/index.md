# Frontend Development Guidelines

These guidelines define the current Rust GPUI presentation architecture for
Vibex. `apps/desktop` is the visual and information-architecture baseline.
`apps/mobile` is a native iOS/Android GPUI client that projects the same Agent
session model into a compact native surface.

Primary evidence:

- [Architecture Baseline](../guides/architecture-baseline.md)
- `apps/desktop`
- `apps/mobile`
- `vendor/zed/crates/gpui_ios` and `vendor/zed/crates/gpui_android`

## Required Reading Order

| Guide | Use When |
| --- | --- |
| [Directory Structure](./directory-structure.md) | Creating shells, feature modules, shared UI, or client protocol code. |
| [Component Guidelines](./component-guidelines.md) | Building timeline cards, panels, dialogs, and mobile screens. |
| [State Management](./state-management.md) | Deciding local UI state, remote snapshots, streaming buffers, or persisted preferences. |
| [Type Safety](./type-safety.md) | Adding protocol types, timeline events, capabilities, or form models. |
| [Quality Guidelines](./quality-guidelines.md) | Reviewing UI behavior, accessibility, responsiveness, and dark mode. |

## Frontend Architecture Baseline

- `crates/vibex-ui` owns shared tokens, portable component models, workflow
  controllers, and shell composition.
- GPUI Desktop uses `NativeBackend`; native mobile uses `WebRemoteBackend` over
  Direct/Tailnet/Relay routes. The name of that adapter does not imply a browser
  product.
- `DesktopRuntime` remains the only authority for Agent sessions, files, Git,
  PTY, provider configuration, device permissions, and mutation results.
- Mobile renders GUI timeline content, including Markdown, process details,
  approvals, and the composer. It is not a terminal-first session client.
- Compact may reorder and reduce density, but it must not become a second domain
  component family or change desktop session semantics.

## Non-Negotiable Rules

- Render Vibex timeline and capability contracts, never raw provider payloads.
- Treat backend snapshots/events as authoritative; local state is presentation
  state only.
- Permission, plan, tool, diff, and command details remain explicit and
  collapsible, with touch-safe actions that do not depend on hover.
- Keep dark mode, loading, empty, streaming, error, reconnecting, and approval
  states first-class.
- Do not add a local Agent, Git, PTY, or workspace filesystem runtime to mobile.

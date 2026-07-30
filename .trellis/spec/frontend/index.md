# Frontend Development Guidelines

These guidelines define the current GPUI presentation architecture for Vibex.
`apps/desktop` is the only visual, interaction, and information-architecture
baseline. Web uses GPUI-WASM, and mobile packages the same GPUI-WASM product through
Capacitor with viewport-driven adaptive shells.

Primary evidence:
- [Architecture Baseline](../guides/architecture-baseline.md)
- `apps/desktop`

Superseded requirement documents, external design mockups, the former
React/Tauri implementations that once occupied the current `apps/web`,
`apps/mobile`, and `apps/desktop` paths, and the deleted `packages/ui` tree are
historical inputs. They are not component, style, navigation, transport, or
rollback sources for current UI. Any detailed scenario that still names React,
Tauri, Zustand, TanStack Query, localStorage migration, or those former
implementations is pre-cutover evidence unless it explicitly identifies a current
GPUI replacement.

## Required Reading Order

Read these files before frontend work:

| Guide | Use When |
| --- | --- |
| [Directory Structure](./directory-structure.md) | Creating app shells, feature folders, shared UI, or client protocol modules. |
| [Component Guidelines](./component-guidelines.md) | Building layouts, timeline cards, panels, dialogs, and mobile screens. |
| [Controller / Historical Hook Guidelines](./hook-guidelines.md) | Adding shared GPUI controllers or interpreting retained pre-cutover React hook evidence. |
| [State Management](./state-management.md) | Deciding local UI state, server state, streaming buffers, or persisted preferences. |
| [Type Safety](./type-safety.md) | Adding protocol types, timeline events, provider capabilities, or form models. |
| [Quality Guidelines](./quality-guidelines.md) | Reviewing UI behavior, accessibility, responsiveness, and dark mode. |

## Frontend Architecture Baseline

- `crates/vibex-ui` owns shared tokens, primitives, View/Controller state,
  domain components, and Wide/Medium/Compact shells.
- GPUI Desktop uses `NativeBackend`; GPUI-WASM Web/mobile use `WebRemoteBackend`.
- `DesktopRuntime` remains the only authority for Agent, files, Git, PTY, Provider
  configuration, device permissions, and mutation results.
- Wide/Medium/Compact are layouts over one component/state model. Compact may
  reorder and reduce density, but it must not become a second mobile design system.
- Web/mobile remain remote clients: lightweight text editing and review are in scope;
  a local Agent, local Git/PTY, heavy IDE editor, and local workspace filesystem are not.

## Non-Negotiable Frontend Rules

- Do not branch UI behavior on raw Claude or Codex SDK payloads. Render the
  Vibex Agent timeline and capability model.
- Treat the backend timeline as authoritative. Client caches optimize display
  but do not decide correctness.
- Permission, Plan, Tool call, Diff, and command execution UI must be card-based
  and collapsible.
- Wide layout preserves the GPUI Desktop workbench; Medium keeps one primary and
  one auxiliary surface; Compact uses a single task stack and two-level navigation.
- New code must not import or copy old React UI, Tailwind/shadcn composition, old
  TypeScript transport, or old CSS.
- Dark mode is a first-class requirement, not a later theme patch.

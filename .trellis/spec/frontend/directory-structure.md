# Frontend Directory Structure

The presentation layer is Rust GPUI code. Desktop and native mobile share typed
domain projections; neither product has a browser or DOM UI tree.

```text
crates/vibex-ui/
  theme/                 Structured semantic token source
  src/
    generated_tokens.rs  Derived token constants
    component_model.rs   Portable timeline/session models
    controller.rs        Backend-driven workflow state
    shell.rs             Wide/Medium/Compact layout policy
    agent/ files/ git/ terminal/ management/
                          Provider-neutral domain components
crates/vibex-backend/
  src/                   Backend traits, capabilities, errors, NativeBackend
crates/vibex-remote-client/
  src/                   Remote adapter, sync, Direct/Relay transports
apps/desktop/
  src/                   NativeBackend and DesktopRuntime composition
apps/mobile/
  src/                   Native GPUI app, input, pairing, storage, Markdown view
  android/                Checked-in NativeActivity Gradle project
  ios/                    XcodeGen project and Objective-C host
apps/relay-server/
  src/                   User-self-hosted zero-knowledge Relay service
```

## Feature Boundaries

- `agent` owns session/timeline/composer/approval presentation over
  `AgentBackend`.
- `files`, `git`, `terminal`, and `management` own their corresponding typed
  projections and capability-gated actions.
- `shell` owns layout/navigation only; it does not duplicate domain behavior per
  platform.
- `apps/mobile` owns platform bootstrap and compact composition, not a second
  protocol or state authority.

## State Boundaries

- DesktopRuntime/server state is authoritative.
- Controllers reconcile typed snapshots, events, and mutation results.
- Local state is limited to layout, focus, draft, selection, drawer/sheet state,
  and bounded presentation buffers.
- Persist only safe, versioned preferences and the native credential bundle; keep
  private keys and grants out of logs and debug output.

## Naming And Anti-Patterns

- Name components by domain semantics (`PermissionRequestCard`, `AgentComposer`).
- Do not create `Mobile*` and `Desktop*` copies of the same domain component.
- Do not add React, Tailwind, shadcn, Monaco, xterm, or DOM transport code.
- Do not put local Agent/Git/PTY/filesystem behavior in mobile views.

# Frontend Directory Structure

The current presentation layer is Rust GPUI code shared by native and WASM targets.
The GPUI Desktop result is the product baseline; Web/mobile add host bridges and a
RemoteBackend, not independent page/component trees.

Primary architecture: [Architecture Baseline](../guides/architecture-baseline.md).

## Target Layout

```text
crates/vibex-ui/
  theme/                 Structured semantic token source
  src/
    generated_tokens.rs  Shared semantic token projection
    component_model.rs   Portable domain component models
    controller.rs        Backend-driven controller and pure UI state
    shell.rs             Current Wide, Medium, Compact composition owner
    host_bridge.rs       Platform-service contract without product navigation
    agent/ files/ git/ terminal/ management/
                         Provider-neutral domain components added by workflows
crates/vibex-backend/
  src/                   Current domain Backend traits, NativeBackend, and facade
crates/vibex-remote-client/
  src/                   WebRemoteBackend, sync state machine, Direct/Relay transport
apps/desktop/
  src/                   NativeBackend, platform bridge, DesktopRuntime bootstrap
apps/web/
  src/                   wasm-bindgen bootstrap and browser host bridge only
apps/mobile/
                         Capacitor metadata and native host bridge only
```

The current `apps/web`, `apps/mobile`, and `apps/desktop` paths reuse locations
that previously held React/Tauri implementations. Those former contents and
`packages/ui` were deleted at the final legacy cutover. References to the old
composition in detailed specs and evidence describe historical behavior only.
New UI must not recreate, import, copy, or package it; rollback uses published
artifacts.

## Feature Boundaries

- `agent` owns session/timeline/composer/approval presentation over `AgentBackend`.
- `files` owns tree/search/view/light-edit UI over revision-aware `FileBackend`.
- `git` owns the approved review subset over capability-gated `GitBackend`.
- `terminal` owns portable emulator/frame presentation over raw-byte `TerminalBackend`.
- `management` owns Provider/Relay/device projections, never raw config files.
- `shell` owns layout/navigation only. It must not duplicate domain behavior per
  viewport or platform.

## State Boundaries

- DesktopRuntime/server state is authoritative.
- Controllers reconcile typed snapshots/events/mutation results from the injected
  Backend; Views do not own reconnect or protocol state.
- Local state is limited to layout, focus, draft, selection, sheet/dialog, and bounded
  presentation buffers.
- Persist only safe non-secret preferences through a versioned shared model.

## Platform Bridge Boundary

Host bridges may expose safe area, soft keyboard, lifecycle, secure storage,
push/deep link, camera/file picker, share/download, clipboard events, and system URL.
They do not render routes, own server state, or implement domain operations.

Host bridge DTOs keep their full serialized payload for the host call, but sensitive
`Debug` output is metadata-only. Push tokens, deep links, storage values, selected
file names/bytes, and share title/text/URL must not appear in logs or evidence;
presence flags, MIME type, and bounded byte counts are sufficient diagnostics.

## Build-Time Asset Dependencies

Rust `include_bytes!`/`include_str!` paths that resolve through root
`node_modules/.pnpm` must have an exact direct dependency in the surviving root
`package.json`. Do not rely on a deleted React/Tauri package to pull the asset into
the install graph. Keep the declared version, pnpm virtual-store path, license
policy entry, and embedded source path synchronized, then prove the contract with
`pnpm install --frozen-lockfile` followed by a clean `cargo check`.

## Naming

- Name components by Vibex semantics (`PermissionRequestCard`, `UnifiedDiffView`).
- Keep provider names in assets/diagnostics where genuinely required.
- Do not create `Mobile*` and `Desktop*` copies of the same domain component; name
  Shell/container differences explicitly.

## Anti-Patterns

- No new React, Tailwind, shadcn/Radix, xterm.js, Monaco, or Tauri UI dependencies in
  the GPUI tree.
- No source-level legacy shell or `packages/ui` fallback for release rollback.
- No User-Agent layout branching; use viewport and negotiated host capabilities.
- No client-local Agent/Git/PTY/filesystem runtime on Web/mobile.
- No provider-specific timeline renderer or raw transport parsing in a View.

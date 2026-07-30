# Backend Directory Structure

Vibex uses a Rust-first workspace. GPUI Desktop is the local product shell,
GPUI-WASM Web/mobile are remote clients, Axum owns RemoteGateway/Relay network
adapters, SQLite owns durable local state, and typed services own Agent/File/Git/PTY
behavior.

Primary architecture: [Architecture Baseline](../guides/architecture-baseline.md) and current source/tests.

## Workspace Layout

```text
apps/
  desktop/   Complete native GPUI workbench and NativeBackend composition
  web/       GPUI-WASM bootstrap, browser host bridge, and static assets
  mobile/    Capacitor host for the source-bound web artifact
  relay-server/   User-self-hosted zero-knowledge Relay service
crates/
  core/           Domain ids, shared DTOs, errors, event/capability contracts
  desktop-model/  Framework-neutral projections, reducers, and UI persistence
  desktop-runtime/ Typed PC composition root and authoritative services
  vibex-ui/  Shared GPUI tokens, View/Controller state, host contract, and Shells
  vibex-backend/ Domain Backend traits, capability/error model, and NativeBackend
  vibex-remote-client/ Planned WebRemoteBackend and Direct/Relay client state machine
  agent*/         Provider-neutral Agent services and ACP/offline adapters
  fs/ git/ terminal/ config-switch/ db/ remote/ relay/
                  Existing typed domain and transport services
```

The current `apps/desktop`, `apps/web`, and `apps/mobile` paths reuse locations
that previously held the retired React/Tauri clients. Those former contents and
the deleted `packages/ui` tree are historical evidence, not implementation
templates. Release rollback restores a verified published artifact and compatible
data backup; it does not restore a legacy source tree on the active branch.

## Ownership Rules

- `crates/core` owns shared wire/domain types only; it does not depend on GPUI,
  Tauri, Axum, SQLite, Git, PTY, or provider implementations.
- `desktop-runtime` is the only PC composition root and authoritative mutation owner.
- `vibex-ui` depends on shared types/projections, not DesktopRuntime, sockets,
  SQLite, native filesystem APIs, React, or Tauri.
- `vibex-backend` splits Agent, Workspace, File, Git, Terminal, Management,
  and Device capabilities instead of exposing one giant trait.
- `vibex-remote-client` owns client protocol/reconnect/sync/transport logic; Views do
  not parse HTTP/WebSocket/Relay envelopes.
- `remote` exposes provider-neutral PC operations and delegates to service owners.
- `relay-server` is business-payload agnostic and never becomes an authority.
- Deleted legacy apps are not dependencies, maintenance targets, or source
  material for the GPUI tree. Consult their final Git revision only when auditing
  historical compatibility or release evidence.

## Dependency Direction

```text
apps/* -> GPUI UI/Backend adapters -> typed domain services -> crates/core
web/mobile GPUI -> WebRemoteBackend -> versioned RemoteGateway -> DesktopRuntime
Relay transport -> encrypted envelopes only; business authorization -> DesktopRuntime
```

Avoid reverse dependencies. In particular, shared UI cannot call DesktopRuntime,
providers, retired shell commands, or a database directly.

## Module Placement

- Put ids and serialized contracts in `core` or their established domain owner.
- Put deterministic UI projections in `desktop-model` or a documented neutral owner.
- Put GPUI visual primitives and controllers in `vibex-ui`.
- Put native host bridges in `desktop`; Web/Capacitor host bridges stay in their
  apps and expose only safe-area, keyboard, lifecycle, storage, camera/file/share,
  push/deep-link, and system-URL capabilities.
- Put service orchestration in the owning Rust service, never in a View or adapter.
- Put database migrations in `crates/db`.

## Naming

- Use stable Vibex ids (`VibexSessionId`, `WorkspaceId`, `DeviceId`, and related
  value objects) at every cross-service boundary.
- Keep provider-native ids inside adapter/provenance models.
- Name shared UI by domain semantics, not provider or platform (`ApprovalSheet`,
  not `MobileClaudeApproval`).

## Anti-Patterns

- Do not add React/TypeScript UI or transport logic to new GPUI apps/crates.
- Do not recreate `apps/desktop`, `packages/ui`, or a Tauri command bridge as a
  rollback mechanism.
- Do not let Direct and Relay produce separate business APIs.
- Do not hide filesystem/Git/Terminal/Provider side effects inside Agent adapters.
- Do not place decrypted business logic or durable authority in Relay.

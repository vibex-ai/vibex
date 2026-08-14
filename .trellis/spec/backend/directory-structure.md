# Backend Directory Structure

Vibex uses a Rust-first workspace. GPUI Desktop is the local product shell and
native GPUI mobile is a remote client; Axum owns RemoteGateway/Relay adapters,
SQLite owns durable local state, and typed services own Agent/File/Git/PTY
behavior.

```text
apps/
  desktop/       Complete native GPUI workbench and DesktopRuntime composition
  mobile/        Native iOS/Android GPUI remote client
  relay-server/  User-self-hosted zero-knowledge Relay service
crates/
  core/           Domain ids, DTOs, errors, event/capability contracts
  desktop-model/ Framework-neutral projections and UI persistence
  desktop-runtime/ PC composition root and authoritative services
  vibex-ui/       Shared GPUI models, controllers, tokens, and shells
  vibex-backend/  Domain Backend traits and NativeBackend
  vibex-remote-client/ Remote protocol, sync, Direct/Relay transport
  agent*/         Provider-neutral Agent and ACP adapters
  fs/ git/ terminal/ db/ remote/ relay/ typed domain services
```

## Ownership Rules

- `crates/core` owns shared wire/domain types only.
- `desktop-runtime` is the only PC composition root and mutation authority.
- `vibex-ui` depends on shared projections, not sockets, SQLite, or service
  implementations.
- `vibex-remote-client` owns reconnect/sync/transport logic; views do not parse
  wire envelopes.
- `apps/mobile` owns native bootstrap, pairing/storage, input, and compact
  composition. It does not own domain state.
- `relay-server` forwards encrypted payloads and never becomes an authority.

## Dependency Direction

```text
apps/desktop -> NativeBackend -> typed domain services -> crates/core
apps/mobile  -> remote facade -> RemoteGateway -> DesktopRuntime
Relay        -> encrypted transport only
```

Avoid reverse dependencies. In particular, shared UI cannot call
`DesktopRuntime`, providers, a database, or a transport directly.

## Anti-Patterns

- Do not add a second mobile protocol or local backend authority.
- Do not put decrypted business logic in Relay.
- Do not place filesystem/Git/Terminal side effects in Agent adapters.
- Do not add browser/DOM packaging or host assets to the native mobile client.

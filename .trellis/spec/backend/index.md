# Backend Development Guidelines

These guidelines describe the current source-backed backend contracts for Vibex.
The PC `DesktopRuntime` remains authoritative while GPUI Desktop and the
GPUI-WASM/Capacitor mobile client consume shared Native/Remote capabilities.

Primary evidence:
- [Architecture Baseline](../guides/architecture-baseline.md) for current cross-platform UI and remote architecture.
- [ACP Runtime Architecture](../guides/acp-runtime-architecture.md) for why every online Agent runs over ACP.
- Current Rust code, tests, and completed Trellis tasks for domain behavior.

Superseded planning documents are not authoritative and must not override the
current GPUI/GPUI-WASM architecture. They are kept out of this repository; see
[Vibex Reference Materials](../guides/reference-materials.md) for where current
evidence lives.

## Legacy Cutover Status

Checkpoint 2 retired the former React/Tauri desktop tree, `packages/ui`, the
Tauri command adapter, and the one-time browser-storage UI-state import bridge.
Those implementations are available only through Git history and recorded
release/migration evidence; they are not active workspace members, rollback
source paths, or implementation templates. The current GPUI desktop later reused
the `apps/desktop` path.

Detailed backend scenarios that name the former `apps/desktop` contents together
with `packages/ui`, Tauri commands, React hooks, or `desktop_ui_state_import`
describe pre-cutover behavior.
Treat those passages as historical evidence even when their original imperative
wording is retained. Current desktop work must enter through `apps/desktop`,
the typed Backend facade, and `DesktopRuntime`; current UI-state persistence is
owned by `crates/desktop-model::UiStateStore` after the runtime home lock is held.

## Required Reading Order

Read these files before backend work:

| Guide | Use When |
| --- | --- |
| [Directory Structure](./directory-structure.md) | Creating crates, services, adapters, commands, or API modules. |
| [Rust Dependency Sources](./rust-dependency-sources.md) | Adding or updating Rust Git dependencies, lockfiles, license policy, or third-party source inputs. |
| [Agent Session Protocol](./agent-session-protocol.md) | Touching Agent sessions, timeline events, permissions, provider adapters, or live event sync. |
| [Agent Usage Statistics](./agent-usage-statistics.md) | Touching ACP usage capture, execution facts, cumulative checkpoints, token aggregation, or typed Usage queries. |
| [Runtime Switch Coordinator](./runtime-switch-coordinator.md) | Touching durable runtime switching, worker leases, active-work gates, operation journals, or startup reconciliation. |
| [Managed Worktree Identity And Recovery](./worktree-coordinator.md) | Touching Worktree identity, eligibility, lifecycle coordination, recovery, destructive preflight, or remote Worktree capabilities. |
| [Provider Configuration](./provider-config.md) | Touching Provider profiles, MCP, Skills, runtime injection, health checks, or config import/export. |
| [Remote and Relay Protocol](./remote-relay-protocol.md) | Touching LAN access, WebSocket APIs, pairing, device permissions, Relay rooms, or E2EE transport. |
| [Database Guidelines](./database-guidelines.md) | Adding SQLite tables, migrations, persistence, or local file storage. |
| [Error Handling](./error-handling.md) | Adding service errors, adapter failures, API errors, or user-visible diagnostics. |
| [Logging Guidelines](./logging-guidelines.md) | Adding tracing, raw provider logs, audit logs, or diagnostics packages. |
| [Quality Guidelines](./quality-guidelines.md) | Reviewing backend changes or adding cross-layer features. |

## Backend Architecture Baseline

Vibex is a local-first multi-end AI coding workbench. The PC desktop app owns
the trusted backend runtime: Agent CLI process management, filesystem access,
Git operations, PTY terminals, Provider configuration injection, Relay
connections, and durable storage.

The backend must expose one provider-neutral service surface to desktop and
mobile clients. UI code must not speak directly to Claude Code, Codex, or
ACP-specific APIs. Every online Agent session is routed through ACP; native
Claude/Codex files are read only by offline import and parity tooling.

## Non-Negotiable Backend Rules

- Keep Vibex session ids separate from ACP-native handles. Native Claude/Codex
  ids discovered by offline import are provenance only and never become online
  route identities.
- Store the server timeline as authoritative. Clients may cache timeline data,
  but reconnection must fetch authoritative history before applying live events.
- Register online runtimes only with an exact
  `AgentRuntimeRouteKey { agent_id, transport_kind: Acp, adapter_id }`.
  `ProviderKind` is configuration/provenance metadata, not a Logical Session or
  runtime dispatch identity.
- Keep `crates/agent-claude` and `crates/agent-codex` offline-only: transcript
  import and sanitized parity replay may remain there, but online providers,
  Native SDK dependencies, and Native smoke binaries must not return.
- Provider configuration changes must be Vibex-scoped by default. Do not modify
  real user home configuration for `codex`, `claude`, or other Agent CLIs unless
  the user explicitly chooses export and sees diff, backup, atomic write, and
  rollback behavior.
- Relay servers must be zero-knowledge passthroughs for business payloads.
  Encryption and authorization belong at the Vibex device/session layer, not in
  Relay room forwarding logic.
- Every external protocol boundary needs capability probing and version-aware
  fallback because Claude Code, Codex, and ACP providers will evolve.
- Production runtime rollback is limited to a previously verified managed ACP
  Adapter version or compatibility descriptor. Rollback must not restore a
  Native route, Native SDK, legacy binding table, or ProviderKind-based online
  dispatch path.
- Product rollback uses published release artifacts and compatible data backups.
  Do not recreate the deleted Tauri shell or a source-level legacy UI fallback.

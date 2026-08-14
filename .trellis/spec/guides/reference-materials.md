# Vibex Reference Materials

Use this guide when a spec rule needs a source, when implementation details are
unclear, or when a future session needs to find the authoritative answer for a
domain.

Everything listed here is inside this repository. Vibex specs must be backed by
in-repo code, tests, or recorded evidence — never by an external working copy, a
developer's local path, or a third-party source tree.

## Architecture Authority

Read these first for product scope and architecture decisions:

| Document | Owns |
| --- | --- |
| [Architecture Baseline](./architecture-baseline.md) | Cross-platform UI, adaptive shells, remote connectivity, pairing, security, release acceptance criteria |
| [ACP Runtime Architecture](./acp-runtime-architecture.md) | Why every online Agent speaks ACP, host capability surface, process strategy, native parity floor |

Layer rules live in [backend](../backend/index.md) and
[frontend](../frontend/index.md). When a layer guide and an architecture
document disagree, the architecture document wins on boundaries and the layer
guide wins on signatures and error handling.

## Where To Look By Domain

| Need | Authoritative source |
| --- | --- |
| Domain DTOs and errors | `crates/core/src` |
| Agent sessions, timeline, permissions | `crates/agent/src`, [Agent Session Protocol](../backend/agent-session-protocol.md) |
| ACP runtime and adapters | `crates/agent-acp/src` |
| Offline Claude/Codex import and parity | `crates/agent-claude`, `crates/agent-codex`, `docs/parity` |
| Provider profiles, MCP, Skills, injection | `crates/config-switch/src`, [Provider Configuration](../backend/provider-config.md) |
| Runtime switching and leases | `crates/desktop-runtime`, [Runtime Switch Coordinator](../backend/runtime-switch-coordinator.md) |
| Remote protocol and pairing | `crates/core/src/remote.rs`, `crates/remote/src`, `docs/remote/protocol-v2.md`, [Remote and Relay Protocol](../backend/remote-relay-protocol.md) |
| Relay transport and crypto | `crates/relay/src`, `apps/relay-server`, `deploy/relay` |
| Remote client transport | `crates/vibex-remote-client/src` |
| Backend capability traits | `crates/vibex-backend/src` |
| Shared UI, tokens, shells | `crates/vibex-ui/src`, `crates/vibex-ui/theme/tokens.json` |
| Desktop presentation | `apps/desktop/src` |
| Native mobile client | `apps/mobile` and `vendor/zed` mobile platform crates |
| Projections (timeline, preview, diff) | `crates/desktop-model/src` |
| Git and managed worktrees | `crates/git/src`, [Quality Guidelines](../backend/quality-guidelines.md) |
| Filesystem service | `crates/fs/src` |
| Terminal | `crates/terminal/src`, `crates/vibex-terminal-ui/src` |
| Persistence and migrations | `crates/db/src`, [Database Guidelines](../backend/database-guidelines.md) |
| Diagnostics and backup | `crates/diagnostics/src`, `crates/backup/src` |
| Platform support and gates | `docs/platform`, `docs/release` |
| Smoke procedures | `docs/smoke`, `scripts/smoke-*.mjs` |
| Third-party licensing policy | `docs/licenses`, [Rust Dependency Sources](../backend/rust-dependency-sources.md) |

## How To Use These Sources

- Start from the architecture documents to learn the intended behaviour, then
  confirm against current code and tests before writing a rule.
- Prefer the shared Rust contract in `crates/core` over any per-surface shape.
  If two layers disagree about a payload, the `crates/core` DTO is right.
- Prefer recorded evidence (`docs/parity`, `docs/platform/evidence`,
  `docs/release`) over prose when asserting that something is verified.
- When a spec statement can no longer be traced to code, tests, or evidence,
  fix or delete the statement. Do not leave an unbacked rule in place.
- Raw research notes and superseded planning reports are private and live under
  `.trellis/research/`, which is untracked here and synchronized with the task
  repository. They may explain how a decision was reached, but they are never an
  authority for current behaviour and must not be cited from a spec file.

## Anti-Patterns

- Do not cite an absolute local path, a developer home directory, or a
  third-party checkout in any tracked file.
- Do not treat a third-party project's shape as the target architecture. Vibex
  exposes provider-neutral sessions, timeline events, and capability models.
- Do not copy a provider-specific UI or API shape into Vibex UI code.
- Do not adopt global native-config rewriting as default behaviour. Vibex
  defaults to session-scoped runtime injection; writing a user's real Agent
  configuration requires explicit consent plus diff, backup, atomic write, and
  rollback.
- Do not treat a superseded document as current just because it is detailed.
  Check its status header, then check the code.

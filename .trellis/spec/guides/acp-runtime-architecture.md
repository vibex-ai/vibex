# ACP Runtime Architecture

> **Authority**: this document records the transport-level architecture decision
> behind Vibex's Agent runtime. Operational contracts live in
> [Agent Session Protocol](../backend/agent-session-protocol.md),
> [Runtime Switch Coordinator](../backend/runtime-switch-coordinator.md), and
> [Provider Configuration](../backend/provider-config.md). Read those for
> signatures, error matrices, and required tests.

## Decision: ACP Is The Only Online Transport

Every online Agent session — Claude, Codex, and every other Agent CLI — reaches
Vibex over the Agent Client Protocol through a managed ACP Adapter.

```text
Agent identity  ⟂  transport

claude   -> managed Claude ACP Adapter   -> ACP host
codex    -> managed Codex ACP Adapter    -> ACP host
opencode -> native ACP                   -> ACP host
others   -> native ACP or an ACP Adapter -> ACP host
```

Vibex keeps Agent identity (`AgentId`: `claude`, `codex`, `opencode`, …) as a
product concept. Identity no longer selects a protocol.

Why this holds:

- One process layer, one session-attachment layer, one permission surface, and
  one terminal host instead of one per vendor SDK.
- One session-config and mode API.
- One process/attachment snapshot, activation generation, and event sequence.
- One Agent install, version-check, and health-probe path.
- One session resume/load/fresh fallback ladder.
- One process spawn fingerprint and stale-detection rule.
- One runtime hot-switch orchestrator.
- Onboarding a new Agent does not require a new full Provider runtime.

Consequences that are non-negotiable:

- `crates/agent-claude` and `crates/agent-codex` stay offline-only. Transcript
  import and sanitized parity replay may live there; online providers, native
  SDK dependencies, and native smoke binaries must not return.
- `ProviderKind` is configuration, import, diagnostics, and provenance metadata.
  It is never a Logical Session identity or an online dispatch key.
- Production runtime rollback may select a previously verified ACP Adapter
  version or compatibility descriptor. It must never restore a native route, a
  native SDK, a legacy binding table, or `ProviderKind`-based online dispatch.

## What ACP Does Not Guarantee

"Unified ACP" is a transport statement, not a capability statement. ACP does not
guarantee that every Agent:

- supports session resume or `session/load`;
- exposes models or reasoning effort;
- emits complete background-task events;
- carries the file, command, and collaboration semantics the product needs in
  its generic tool-call shape;
- allows base URL, API key, or provider change inside a live process;
- expresses Skills on the wire at all;
- gives fork, rollback, and delegation the same meaning.

Therefore the architecture is:

```text
unified ACP transport
  + an Agent compatibility registry
  + runtime capability probing
  + conservative fallback
```

Capability decisions follow this precedence:

```text
runtime initialize capabilities
  > provider profile compatibility override
  > built-in Vibex compatibility table
  > conservative fallback
```

Adapter-level quirk compatibility stays inside the adapter module
(`crates/agent-acp/src/protocol.rs` documents the current wire quirks). ACP
method names, JSON-RPC ids, and raw update payloads must never leak into the
UI or the business layer:

```text
ACP JSON-RPC -> AcpEvent -> ProviderEvent -> TimelinePayload -> UI
```

## Host Capability Surface

Vibex is the ACP *host*: it declares to each Agent which host capabilities it
will serve, then implements them. The declaration is built from typed profile
configuration, not hardcoded per Agent
(`AcpProviderConfig` in `crates/core/src/provider.rs`).

| Host capability | Wire surface | Default | Owner |
| --- | --- | --- | --- |
| Text file read/write | `fs.readTextFile` / `fs.writeTextFile` | on | Workspace file service with path canonicalization |
| Terminal tools | `terminal` | **off** (`terminal_tools`) | Terminal host behind the permission system |
| Terminal auth | `auth.terminal` | **off** (`terminal_auth`) | Login terminal action, never a timeline token |
| Session config options | `session/new` + config update | on when the Agent reports options | Provider-neutral session config state |
| Session list / import | `session/list`, `session/load` | capability-gated | External session import candidates |
| MCP forwarding | `session/new.mcpServers` | per profile matrix | Enabled MCP descriptors only |
| Redacted debug log | n/a (local) | redacted | Bounded ring buffer of incoming/outgoing/stderr |

Rules:

- **Default conservative.** A new host capability ships off. Turning it on is an
  explicit profile decision with a visible injection preview.
- **Terminal work is permissioned.** Creating a terminal or running a command on
  an Agent's behalf raises a permission request that shows command, args, cwd,
  risk, and a redacted env summary. Denial must reach the Agent as a refusal,
  not a silent failure.
- **Secrets resolve late and print never.** MCP env and auth material resolve
  before process start; debug logs and timeline show only redacted references.
  Full raw transcripts are a local, explicit developer opt-in.
- **MCP forwarding is opt-in per profile.** Disabled servers are omitted, and an
  invalid descriptor fails session start with a typed validation error instead of
  starting a half-configured session.
- **Terminal output is bounded in the timeline.** The tool card carries a
  truncated summary; full output belongs to the terminal surface.

## Process Strategy

`AcpProcessStrategy` (`crates/core/src/provider.rs`) selects how ACP processes
map to Vibex sessions:

| Strategy | Meaning |
| --- | --- |
| `PerSession` (**default**) | One Vibex session owns one ACP process |
| `PerProfilePool` | Compatible sessions under the same profile and workspace share one process |
| `Auto` | Start isolated; pool only after a capability probe proves it safe |

`PerSession` is the default because it is compatible with every ACP CLI, keeps
one crash to one session, and cannot cross session context.

Pooling requires all of the following, and falls back to `PerSession` with a
recorded diagnostic when any is missing:

- the Agent returns distinct `sessionId`s from `session/new`;
- the Agent carries `sessionId` reliably on `session/prompt` and
  `session/update`;
- Vibex can route every update to the right Vibex session by native session id;
- resume, listing, and close behave per session rather than per process.

Pool identity must never be just the Agent id. It is keyed by provider profile,
workspace root, and fingerprints of the resolved command and env, so a config or
credential change starts a new pool instead of reusing a stale one. Env
fingerprints are hashes; secrets never enter a pool key that can be logged.

A pooled process runs one prompt at a time unless a specific Agent is proven to
handle concurrent prompts. When a pooled process exits, every attached session
must receive a recoverable error — pending requests failed, pending permissions
cancelled, pool entry removed.

## Agent Import And Replay Compatibility

Online sessions use ACP. Provider-specific Claude and Codex crates retain only
read-only transcript import and deterministic replay behavior, documented in
`docs/parity/agent-replay.md`; they are not alternate online runtimes.

**Golden event compatibility.** Each replayed Agent event kind provides:

```text
sanitized provider transcript fixture
expected canonical timeline fixture
```

Replay must be deterministic: every fixture is replayed twice per run and must
match itself. Live ACP output may be richer than imported transcripts, but
supported information must not be lost or misclassified when normalized.

**Inputs that fixtures must keep covering.** Claude: streaming text, thinking
deltas, tool calls, tool-input JSON deltas, MCP injection, Skills projection,
model selection, reasoning-effort probing, slash commands, permission requests,
image and file attachments, session resume, external transcript import. Codex:
real model list, per-model reasoning efforts and defaults, base URL / API key /
model provider / wire API, per-session-and-profile runtime home, MCP and Skills
runtime-home injection, thread resume, agent message, reasoning, plan, command
execution, file changes, MCP tool, dynamic tool, collaboration tool, web search,
todo list, fork/rollback capability, terminal activity hooks, external transcript
import.

Fixtures are sanitized, read-only, and scanned for real home paths and
API-key-shaped tokens. Recording new fixtures is env-gated, may capture only
through a managed ACP Adapter, and must pass every line through sanitization
before commit.

## Anti-Patterns

- Letting the frontend see `session/update`, `session/prompt`, JSON-RPC ids, or
  any ACP method name. The frontend consumes provider profiles, session config
  state, permission requests, tool calls, terminal actions, and import
  candidates.
- Assuming a catalog entry's declared capability instead of probing the runtime.
- Enabling `PerProfilePool` from configuration alone, without the routing
  guarantees above.
- Routing an update with no `sessionId` by guessing the current session while
  pooled. Record a diagnostic instead.
- Writing an unknown ACP extension payload straight into the timeline.
- Keeping a provider-specific ACP client alive alongside the generic runtime
  once the generic runtime covers it. Special cases belong in the compatibility
  layer, not in a parallel production client.

## Evidence Map

| Area | Source |
| --- | --- |
| Generic ACP runtime | `crates/agent-acp/src/runtime.rs` |
| ACP client, provider, event mapping | `crates/agent-acp/src/lib.rs`, `crates/agent-acp/src/events.rs` |
| ACP wire quirks and typed params | `crates/agent-acp/src/protocol.rs` |
| Bridge contract | `crates/agent-acp/src/bridge_contract.rs` |
| Provider-neutral AgentManager | `crates/agent/src/manager.rs` |
| Provider profile and capabilities | `crates/core/src/provider.rs` |
| Agent catalog | `crates/core/src/agent_config.rs` |
| ACP typed config, presets, validation | `crates/config-switch/src/lib.rs` |
| Terminal domain types | `crates/core/src/terminal.rs` |
| Agent import and replay fixtures | `docs/parity/agent-replay.md`, `crates/agent-claude/tests`, `crates/agent-codex/tests` |

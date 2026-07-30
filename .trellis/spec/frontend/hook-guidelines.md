# Controller And Legacy Hook Guidelines

Current GPUI code uses shared domain Controllers over injected Backend traits.
Controllers consume already-decoded snapshots/events/mutation results and must not
call provider-native SDKs or parse transport/provider-native event shapes.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md) and shared Rust contracts.

The React hook guidance below is historical evidence from deleted legacy apps, not
a maintenance contract. New GPUI code must keep reconnect, replay, credential
refresh, queue draining, and authoritative refetch inside WebRemoteBackend rather
than rebuilding them in each Controller.

## Data Fetching

Use TanStack Query for request/response server state:

- Project and workspace lists.
- Session lists and session metadata.
- Timeline page fetches.
- File tree and file contents.
- Git status, diffs, branches, and history.
- Provider profiles, MCP, Skills, health, and usage.
- Device and pairing state.

Query keys should include the stable Vibex ids needed to invalidate precisely:
host, project, workspace, session, provider profile, device, or path.

## Live Subscriptions

Use a dedicated client protocol layer for WebSocket subscriptions. Hooks should
subscribe to Vibex event envelopes and apply only events that match the current
feature scope.

Subscription attach requests should carry stable subscription ids, connection
ids, and `sinceSequence`. The transport layer owns attach, replay, snapshot
handling, and live event ordering. Feature hooks consume already-reconciled
events or typed cache updates.

Reconnect flow must:

1. Pause optimistic live rendering where correctness depends on sequence.
2. Fetch authoritative missed timeline or channel data from the server.
3. Resume live event application after catch-up completes.

Remote transports must also handle ping/pong, auth-expired challenges,
credential refresh or re-pairing, exponential backoff, and a bounded send queue
with idempotency keys. Queue draining belongs in the transport/client layer, not
inside individual Agent, Git, file, terminal, or permission hooks.

Do not treat WebSocket events as the only source of truth.

## Agent Action Hooks

Agent hooks should expose provider-neutral actions:

- `sendMessage`
- `interruptTurn`
- `resolvePermission`
- `retryTurn`
- `forkSession`
- `rollbackSession`
- `compactSession`
- `archiveSession`

They should not expose `codexTurnStart`, `claudeResume`, or similar
provider-native operations to components.

## Terminal Hooks

Terminal hooks must account for high-volume data:

- Use throttled rendering or buffered append for output.
- Keep resize events debounced.
- Separate raw PTY output from command-card summaries.
- Handle mobile shortcut actions without assuming hardware keyboard input.

## File and Git Hooks

File and Git mutation hooks must require explicit confirmation for destructive
actions when initiated remotely:

- File delete, move overwrite, and revert.
- Git revert, branch delete, push, pull, and worktree remove.
- Native Provider config export.

## Anti-Patterns

- Do not parse raw Claude/Codex events inside React hooks.
- Do not duplicate cache reconciliation in each component.
- Do not update local session status without either a mutation response or a
  sequenced server event.
- Do not keep long-lived WebSocket objects inside random feature components.

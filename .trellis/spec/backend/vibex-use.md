# Vibex-use Contracts

## 1. Scope / Trigger

Read before changing delegation, automated input, execution results, team access, events, MCP transport, or team
presentation. `DesktopRuntime` owns the state; protocol adapters and native clients use its services.

Also follow [Architecture Baseline](../guides/architecture-baseline.md), [Agent Session
Protocol](./agent-session-protocol.md), [Runtime Switch Coordinator](./runtime-switch-coordinator.md), [Agent Usage
Statistics](./agent-usage-statistics.md), and [Remote and Relay Protocol](./remote-relay-protocol.md).

## 2. Signatures (command/API/DB)

[`VibexUseToolHost`](../../../crates/core/src/vibex_use.rs), implemented by
[`VibexUseService`](../../../crates/desktop-runtime/src/vibex_use.rs), exposes:

```rust
fn call(&self, actor: VibexUseActor, tool: VibexUseTool, arguments: Value)
    -> VibexUseToolFuture<'_>;
fn response_delivered(&self, actor: &VibexUseActor, tool: VibexUseTool, response: &Value)
    -> VibexResult<()>;
```

`VibexUseTool::ALL` defines 20 tools. Keep schemas and service dispatch in sync; the live capability snapshot determines
availability.

| Surface | Tools |
| --- | --- |
| Discovery/reads | `vibex_discover`, `vibex_list_sessions`, `vibex_get_session`, `vibex_read_session`, `vibex_get_operation`, `vibex_get_tasks` |
| Work | `vibex_create_session`, `vibex_delegate`, `vibex_send_message`, `vibex_finish_task`, `vibex_interrupt`, `vibex_cancel_task` |
| Inbox | `vibex_wait`, `vibex_get_events`, `vibex_ack_events` |
| Presentation | `vibex_list_groups`, `vibex_create_group`, `vibex_update_group`, `vibex_present_group`, `vibex_dissolve_group` |

Human clients use [`AgentBackend`](../../../crates/vibex-backend/src/agent.rs): `team_snapshot(TeamSnapshotRequest)`,
`session_tree(SessionTreeRequest)`, `delegate_session(MutationRequest<HumanDelegationRequest>)`,
`send_message_with_mentions`, `set_session_access`, `control_team_task`, `present_team`, and `acknowledge_team_events`.
Typed results use `BackendFuture`. [Native](../../../crates/vibex-backend/src/native.rs) and
[remote](../../../crates/vibex-remote-client/src/backend.rs) implementations share the [human
service](../../../crates/desktop-runtime/src/vibex_use/team.rs).

| Storage | Invariant |
| --- | --- |
| `agent_delegations` | Phase/status change together; current execution, review policy, controller revision, and `parent_task_id` are independent facts. |
| `agent_message_submissions` / payloads | Input identity and dispatch state are durable; enqueue and dispatch use SQLite immediate transactions. |
| `vibex_use_executions` | Unique submission and `(session_id, input_idempotency_key)`; each round saves its root, `origin_task_id`, admission `controller_revision`, outcome, result range, and usage. |
| `vibex_use_operations` | Unique `(authority, actor_key, tool_kind, caller_key)`, semantic payload fingerprint, resource references, and resumable checkpoints. |
| `session_ownership_edges` | One creation parent per child with a saved root; controlling an existing session does not reparent it. |
| `vibex_use_session_grants` / `vibex_use_session_controllers` | Access and current task control are independent; controller changes use revision checks. |
| `vibex_use_cancellation_executions` | `(task_id, execution_id)` captures the exact accepted rounds whose stop the task awaits. |
| `vibex_use_task_events` / `vibex_use_event_deliveries` | Stable events; delivery and acknowledgement belong to one consumer. |
| `vibex_use_group_presentations` | Durable membership, ownership, requested/applied layouts, and revision. |
| `vibex_use_policies` / `vibex_use_task_limits` | Validated policy and each task's fixed deadline. |

Entry points include `MessageSubmissionRepository::{enqueue, mark_about_to_prompt}`,
`AgentDelegationRepository::reserve_or_get`, `settle_vibex_use_execution`, `finish_delegation_task`, and
`request_delegation_tree_cancellation`. See [domain storage](../../../crates/db/src/vibex_use.rs), [submission
storage](../../../crates/db/src/runtime.rs), [task reservation](../../../crates/db/src/lib.rs), and [migration
63](../../../crates/db/src/vibex_use/budget.rs). Migration derives old origins/parents only from surviving creation
evidence; old taskless causal task identity may remain unknown. Existing tasks receive no retrospective deadline.

## 3. Contracts (request/response/env)

### Identity and access

- `vibex://<kind>/<id>` identifies a resource, never a grant. Reauthorize reads, waits, mutations, and recovery under
  the issuing runtime authority.
- `VibexUseActor { authority, session_id, activation_revision }` comes from the credential. Activation fences access; it
  does not change operation identity.
- `owned`/`controlled` allow reads and automated writes; `referenced` allows content reads; `read_only` exposes bounded
  metadata. Hide unauthorized content.
- Only authenticated human mention selection may establish read access. `SessionAccess::{Read, Control, Revoke}` is
  explicit; pasted URIs grant nothing. Replaying an older selection/grant must preserve a later human revoke.
- One unfinished task controls a session. Enqueue/dispatch check current grants, phase, cancellation, and controller
  revision. `expectedControllerRevision`, `expectedExecutionRef`, and task `expectedRevision` fence stale actions;
  runtime-selection revision is separate. Human takeover changes the controller.
- Every automated execution saves its admission controller revision. Human input takes over automated sessions,
  including taskless work. Only a new human `Control` intent may return control; saved request replay never does.
  Handback preserves the active task owner and rejects another parent. Old queued/preparing inputs remain invalid;
  new inputs capture the new revision. Replaying old human input does not retake control.
- Execution ancestry follows the current task or active taskless input; saved root and causal task identity survive
  cross-tree control and settlement. Navigation stays fixed. Deletion traverses `owned_child` creation edges only;
  `controlled_existing` sessions are excluded from the deletion cascade.

### Input, results, and recovery

- `vibex_delegate` requires `idempotencyKey`, `task.prompt`, and target selection; new tasks default to `owner_review`.
  Targets may be new or authorized workers.
- `vibex_create_session` creates a session; optional `firstMessage` creates a taskless execution. `vibex_send_message`
  enqueues a new round from `idempotencyKey`, `sessionRef`, `text`, and optional `taskRef`.
- Accepted creation, enqueued input, settled execution, and accepted task are separate facts. Return the original
  submission, operation, and execution refs. Dispatch reserves the start sequence; observers settle only that round.
- Normal `owner_review` results await review. `vibex_finish_task` accepts/rejects only `awaiting_review`; acceptance
  releases task control. Follow-up input makes another execution. Continuing completed work makes a task with
  `followsTaskRef`. `single_turn_legacy` retains automatic completion compatibility.
- Save fixed result ranges. Late observers cannot finish/replace a newer round. Preserve `completed`, `actions_only`,
  `empty_reply`, `refusal`, `max_tokens`, `auth_required`, `cancelled`, `failed`, and `ambiguous` outcomes
  independently.
- Authority assigns `MessageProvenance`; do not infer it from text. Requested and effective runtime summaries differ;
  dispatch records effective binding identity.
- Fingerprint the complete semantic payload. Same key/payload reuses resources; changed payload conflicts. Checkpoint
  request, selection, context, rendered preamble, reserved session identity, and response. Recovery reuses these facts.
  Uncertain provider dispatch becomes `ambiguous`; never auto-replay its prompt. Keep operation resources, stable error,
  and retryability observable.

### Bounded context and reads

Runtime selection uses discovered identities and optional catalog revision. `context` accepts at most 16 explicit
windows; reauthorize and freeze snapshots, ranges, and rendered provider-only context. Retry preserves those
checkpoints; never copy the whole parent transcript implicitly. See
[selection](../../../crates/desktop-runtime/src/vibex_use/selection.rs),
[checkpoints](../../../crates/desktop-runtime/src/vibex_use/operations.rs), and [read
implementation](../../../crates/desktop-runtime/src/vibex_use/read.rs).

Reads default to 50 items / 16,000 characters, capped at 200 / 64,000; context shares 64,000 characters across all
windows. Report actual `includedChars` and `truncated`; characters are not Tokens. `summary`, `conversation`, and
`timeline` are separate views. Choose latest, exclusive `after`/`before`, or a cursor; `fromSequence`/`throughSequence`
bound a window. Cursors bind session, view, snapshot, range, execution/resource, and Unicode offset. Continue
losslessly. `resourceRef` reads its timeline payload/source sequence, never arbitrary files.

### Cancellation and maintenance

`vibex_interrupt` selects the admitted or first queued execution, guarded by optional `expectedExecutionRef`, and saves
`interrupt_requested_at_ms` before provider waits. This fences preparation, preserves other queued inputs and task
control/phase, and allows follow-ups. Replay uses the saved execution identity. `vibex_cancel_task` records a durable stop:

- An immediate transaction captures exact execution IDs and persists fences before provider calls or lifecycle waits.
  Awaiting/ready captured submissions become cancelled before dispatch; running rounds require observed settlement.
- Cascade follows `agent_delegations.parent_task_id`. `cascade=true` captures task-linked and taskless
  queued/running/ambiguous executions by `origin_task_id`, including work caused in controlled sessions. Pre-existing
  foreign navigation children are outside this causal subtree.
- `cascade=false` captures only the requested task's rounds. Already accepted, uncaptured taskless/child work is not
  interrupted and may still dispatch. The durable fence nevertheless rejects new causal input and child admission.
- Interruption uses captured execution/submission identity. Pause session dispatch and recheck admission before
  interrupting so retries cannot stop a successor or discard unrelated queued input in the same controlled session.
- Interrupt acknowledgement is not completion. Keep `cancelling`, task control, and pending operation state until every
  captured round settles unambiguously. `ambiguous` cannot confirm stop. Only then release control and emit the stable
  `TaskCancelled` event; previously terminal task phases remain terminal.
- Operation success and `cancelled` must reflect the full captured set. A terminal task row alone cannot prove a newly
  requested cascade is finished.
- Maintenance retries persisted manual and deadline cancellations after restart; timeouts/errors preserve the stop.
  Bound concurrency to four attempts with a five-second attempt timeout. Pending scans must eventually reach later
  tasks; unresolved ambiguity must not monopolize the first maintenance page. Persist attempt order separately from task
  revision/update timestamps; deadline scans exclude already requested stops.

See [cancellation ledger](../../../crates/db/src/vibex_use/cancellation.rs), [exact
interruption](../../../crates/agent/src/manager/delegation_cancellation.rs),
[observers](../../../crates/agent/src/manager/delegation_execution.rs), and
[maintenance](../../../crates/agent/src/manager/delegation_budget.rs).

### Event delivery and waits

Events have stable IDs and ordered cursors. Reads change neither delivered nor acknowledged flags. MCP records delivery
only after the exact response is written and flushed to stdout and its broker receipt confirmed. Failed
output/disconnects leave events available; receipts cannot supply replacement event IDs. Agent ACK only covers that
consumer's delivered events, by IDs or `throughEventCursor`. Human `human:team-inbox` collection is independent;
explicit human ACK validates all IDs against one team atomically, witnesses receipt, and acknowledges them.

`vibex_wait` accepts up to 32 task/session targets, `afterEventCursor`, and `timeoutMs` (0–25,000). Results are
`timed_out`, `settled`, `attention`, or `failed`. Register notification interest before snapshot; poll at 250 ms for
external writes. Return at most 50 events; a separate bounded signal query can detect a result behind progress without
skipping the returned page/cursor. Scope-filter payloads and use the originating team for taskless work. A resolved
historical block does not wake waits. Timeout does not cancel work.

### MCP transport

The [broker](../../../crates/agent/src/delegation.rs) and [stdio sidecar](../../../crates/agent/src/delegation/stdio.rs)
use the shared host, including legacy aliases. Missing host/capability fails closed.

| Boundary | Contract |
| --- | --- |
| Identity | Credential binds broker secret, parent session, authority, activation; validate at entry, while waiting, before return, and before delivery. |
| Capacity | 16 in-flight brokered calls/receipt handoffs; 128 broker connections; unique in-flight JSON-RPC IDs; local ping/initialize stay responsive. |
| Framing | Newline JSON or `Content-Length`; MCP 256 KiB, broker 512 KiB, headers 8 KiB / 32 lines; string IDs at most 256 bytes, or integer IDs. |
| Time | Call timeout 30 s; broker I/O and receipt handoff 5 s. |
| Cancellation | `notifications/cancelled` drops a request's waiter/connection, permits ID reuse, and does not cancel the durable task. |
| Ordering | Long polls do not serialize discovery. Pending receipt handoff delays subsequent explicit ACKs, not ping. |
| Diagnostics | Bounded stable metadata; exclude credentials, prompts, raw provider logs, and payloads. |

Launcher-owned environment: `VIBEX_AGENT_DELEGATION_ENDPOINT`, `VIBEX_AGENT_DELEGATION_TOKEN`,
`VIBEX_AGENT_DELEGATION_PARENT_SESSION`, `VIBEX_AGENT_DELEGATION_AUTHORITY`, `VIBEX_AGENT_DELEGATION_ACTIVATION`
(positive). Public server name: `vibex-use`; compatibility wire ID: `vibex-agent-delegation`.

### Budgets, observability, and clients

Optional startup `<runtime home>/vibex-use.json` is camelCase, at most 16 KiB, and rejects unknown/invalid fields.
Missing selects `balanced`; only specified overrides replace the preset. See [policy
types](../../../crates/core/src/vibex_use/budget.rs) and [configuration
loader](../../../crates/desktop-runtime/src/vibex_use/budget.rs).

```json
{"preset":"expanded","perAgentExecutionLimit":3,"maxReportedTokens":100000}
```

| Preset | Depth | Root | Per Agent | Timeout | Idle retention | Warm |
| --- | ---: | ---: | ---: | --- | --- | ---: |
| `conservative` | 1 | 4 | 2 | 30 min | 2 min | 1 |
| `balanced` | 2 | 8 | 8 | 60 min | 5 min | 2 |
| `expanded` | 4 | 16 | 8 | 4 h | 15 min | 4 |

Overrides: `maxDepth` 1–8; `rootExecutionLimit`/`perAgentExecutionLimit` 1–64; `taskTimeoutMs`/`idleWorkerRetentionMs`
1,000–86,400,000; `warmWorkerLimit` 1–64; optional `maxReportedTokens` 1–`i64::MAX` (no preset Token ceiling). Atomic
admission counts queued/running rounds and unconsumed first-input task reservations. Consume each reservation once.
Per-Agent limits span roots; root limits use saved execution origin. Deadlines are fixed at task reservation; expiration
fences input/dispatch/descendants. Lower limits do not cancel admitted work or rewrite deadlines. Pane/process counts
are separate limits. Reported Tokens are a lower bound: prefer reported total, derive only with both input/output, never
add cache/thought usage twice. Unknown remains `None`; exclude other roots and saturate sums. A known total at the
ceiling rejects new work.

Service and manager share `RuntimeObservability`: `delegation_admission_total`, `delegation_queue_wait_ms`,
`delegation_attention_wait_ms`, `delegation_cancellation_total`, `delegation_deadline_total`,
`delegation_delivery_total`, `delegation_presentation_total`. First delivery is `Success`; replay is `Reused` on the
same collector.

Remote reads require `ReadAgentSession`, writes `MutateAgentSession`. The gateway derives `human:device:<device-id>`
from authentication; native uses `human:local`. JSON cannot set trusted identity. Unsupported clients expose
capabilities honestly. Tree pages contain roots/direct children, ancestry, and metadata, not transcripts; desktop pages
64 nodes and retains at most 4,096 nodes / 64 branches. Shared
[`TeamWorkflowController`](../../../crates/vibex-ui/src/team.rs) fences stale loads. Groups grant no execution access;
at most 16 same-workspace members and four live panes. Hidden tabs retain metadata. Return actual
`appliedLayout`/degradation. Manual edits release Agent ownership and fence late replies/recovery. [Automatic
layout](../../../crates/desktop-model/src/session_group.rs) uses 24rem per column: below 48rem tabs; three lead/workers
use columns at 72rem; four use a grid when two columns fit. Preserve selected tab and manual ownership.

## 4. Validation & Error Matrix

| Condition | Result |
| --- | --- |
| Invalid reference/target/context | `vibex_use_request_invalid` |
| Ambiguous read anchor / foreign cursor scope | `read_anchor_ambiguous` / `cursor_scope_mismatch` |
| Unsettled execution result | `vibex_use_execution_result_pending` |
| Changed payload under the same key / stale catalog | `idempotency_payload_conflict` / `catalog_stale` |
| Unreadable resource / write without control | `not_found_or_not_authorized` / `scope_denied` |
| Stale revision/controller | `session_revision_conflict`, `controller_changed`, or dispatch gate error |
| Interrupted round reaches dispatch | `vibex_use_execution_interrupted`; preserve task control and allow new input. |
| Terminal/cancelling task | `task_terminal` / `task_cancelling`; transactional fence: `vibex_use_task_cancelling` |
| Invalid policy | `vibex_use_budget_invalid`; keep previous stored policy |
| Root / Agent / usage limit | `vibex_use_root_budget_exceeded` / `vibex_use_agent_budget_exceeded` / `vibex_use_token_budget_exceeded` |
| Excess depth / expired task | `delegation_depth_exceeded` / `vibex_use_task_deadline_exceeded` |
| Invalid credential / broker timeout | `agent_delegation_unauthorized` / `agent_delegation_broker_timeout`; no receipt |
| Duplicate ID/capacity / malformed JSON / invalid RPC / unknown method | JSON-RPC `-32000` / `-32700` / `-32600` / `-32601` |
| Oversized output / failed flush | Bounded error or disconnect; no event delivery |
| Missing presentation capability / stale manual revision | Error or bounded warning; preserve accepted work and newer layout |
| Human ACK includes another team's event | `team_event_scope_denied`; whole batch unchanged |

Capability admission may return `root_execution_budget_exhausted` or `capability_unavailable` before SQLite; use the
actual stable error/capability.

## 5. Good/Base/Bad Cases

| Case | Expected behavior |
| --- | --- |
| Good: two rounds reuse a worker, then owner accepts | Independent fixed ranges; acceptance closes task and releases control. |
| Good: cascade reaches a controlled worker's causal child | Capture causal rounds; preserve foreign navigation children and saved roots. |
| Good: result follows 60 progress events | Signal is detected; all 61 events remain retrievable through the original page cursor. |
| Base: timeout / unknown usage | Work remains active / usage remains unknown. |
| Base: captured taskless round outlives own task round | Keep cancellation pending and controller held until captured settlement. |
| Bad: captured round is ambiguous | Do not claim cancellation completion or release control. |
| Bad: changed-key payload / replay after human revoke | Reject conflict / preserve revoke. |
| Bad: ACK only-read event / uncertain provider dispatch | ACK zero / retain ambiguity without automatic prompt replay. |

## 6. Tests Required (with assertion points)

| Suite | Required assertions |
| --- | --- |
| [DB budget/cancellation](../../../crates/db/src/vibex_use/budget_tests.rs) | Cross-connection admission, reservation consumption, deadlines, unknown/overflow usage, saved cross-root origin; cascade true/false captures taskless work correctly, excludes foreign children, fences new work, waits for all rounds, and retains ambiguous cancellation/control. |
| [DB domain](../../../crates/db/src/vibex_use.rs) | Phase/status, idempotency races, immutable ranges, unique ownership, controller CAS, delivery-before-ACK. |
| [Runtime integration](../../../crates/desktop-runtime/src/vibex_use/integration_tests.rs) | Independent rounds/stale settlement; review release; delivery `Success`/`Reused`; all 61 backlog events survive; nonzero-cursor failures; scope, grouping, manual revision, and recovery. |
| [Reads](../../../crates/desktop-runtime/src/vibex_use/read.rs) | Frozen snapshots, lossless Unicode, explicit windows exclude neighbors, resources stay source-bound. |
| [Transport](../../../crates/agent/src/delegation/tests.rs) / [framing](../../../crates/agent/src/delegation/framing.rs) | Credentials/revocation, concurrent waits, cancellation/ID reuse, capacity/partial frames, exact post-flush receipts, ACK responsiveness, fail-closed aliases. |
| [Manager recovery](../../../crates/agent/src/manager/delegation_recovery_tests.rs) | Saved creation/runtime checkpoints, cancelled spawn, dispatch revisions, expired queued input; stop retries cannot interrupt unrelated work or report interrupt ACK as settlement. |
| [Human team](../../../crates/desktop-runtime/src/vibex_use/team_tests.rs) | Mention ranges/runtime, revoke-safe retry, taskless human takeover, explicit handback, stale queued-input rejection, old human replay, foreign task-owner guard, navigation pages, atomic independent inbox ACK, manual presentation recovery. |
| [Remote](../../../crates/remote/src/lib.rs) / [shared UI](../../../crates/vibex-ui/src/team.rs) / [desktop](../../../apps/desktop/src/team_tests.rs) | Device permission/trusted identity; bounded/stale loads, separate cursors, retained members, correct pane runtime/actions, four live conversations. |
| [Layout](../../../crates/desktop-model/src/session_group.rs) / [policy](../../../crates/core/src/vibex_use/budget.rs) | Width adaptation/selected tab/manual ownership; preset overrides and invalid ranges. |

Preserve regressions for more than 64 pending/expired cancellations, eventual maintenance progress despite ambiguity,
and operation completion after every captured execution settles. Test service/transaction boundaries and actual
transport readers/writers; a DTO-only test cannot prove these guarantees. Run affected `cargo test --locked -p <crate>
--lib <module>::` suites, including `vibex-db`/`vibex_use`, `vibex-agent`/`delegation` and manager recovery,
`vibex-desktop-runtime`/`vibex_use`; follow the package quality gate.

## 7. Wrong vs Correct

| Wrong | Correct |
| --- | --- |
| Use the worker's newest message as an earlier result. | Read that execution's saved range. |
| Deliver/ACK during response construction. | Exact post-flush delivery, then explicit consumer ACK. |
| Skip directly to a result behind a full event page. | Detect signal separately; return the original page/cursor. |
| Recover with current provider defaults. | Reuse saved runtime, context, identities, and submission key. |
| Cancel a controlled session's whole navigation tree or every queued input. | Capture causal task/execution IDs and recheck the exact admitted submission. |
| Treat interrupt acknowledgement, terminal task phase, or ambiguity as confirmed cancellation. | Require unambiguous settlement of the full captured set. |
| Derive access/budget from references, groups, or visible panes. | Authenticated grants and transactional execution budgets. |

Fixed-result read example; replace placeholders with returned references:

```json
{"sessionRef":"vibex://session/<id>","executionRef":"vibex://execution/<id>","view":"conversation","maxItems":50,"maxChars":16000}
```

Continue with the returned `nextCursor` unchanged; another round needs its own `executionRef`.

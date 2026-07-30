# Durable Runtime Switch Coordinator

> Legacy cutover note (2026-07-29): later Tauri lifecycle examples record the
> deleted shell's former wiring. Preserve the runtime lifecycle invariants, but
> implement current startup and shutdown only through GPUI Desktop and
> `DesktopRuntime`.

## Scenario: Transactional Logical-Session Runtime Switch

### 1. Scope / Trigger

- Trigger: code creates, resumes, mutates, cancels, activates, or reconciles an
  Agent runtime while preserving one logical `VibexSessionId`.
- `crates/agent/src/runtime_switch.rs` owns provider-neutral orchestration.
  Concrete ACP process and attachment operations remain in the adapter layer.
- SQLite rows are authoritative. In-memory tasks, attachment handles, and
  process instances never prove switch completion.
- This coordinator does not expose the product-facing desired/effective API;
  that layer submits a durable request and implements the executor/gate traits.

### 2. Signatures

```rust
RuntimeSwitchCoordinator::request_switch(
    request: RuntimeSwitchRequest,
) -> VibexResult<SwitchOutcome>

RuntimeSwitchCoordinator::drive_switch(
    switch_id: &RuntimeSwitchId,
) -> VibexResult<SwitchOutcome>

RuntimeSwitchCoordinator::cancel_switch(
    session_id: &VibexSessionId,
    switch_id: &RuntimeSwitchId,
) -> VibexResult<SwitchOutcome>

RuntimeSwitchCoordinator::reconcile_on_startup(
) -> VibexResult<RuntimeSwitchReconcileReport>

decide_switch_strategy(
    policy: RuntimeSwitchPolicy,
    assessment: &SwitchTargetAssessment,
) -> RuntimeSwitchStrategy
```

Adapter boundary:

```rust
trait SwitchTargetExecutor {
    async fn assess_target(...);
    async fn ensure_process(...);
    async fn restore_or_create_session(...);
    async fn recover_attachment(...);
    async fn acquire_prepared(...);
    async fn apply_session_config(...);
    async fn apply_live_mutation(...);
    async fn revalidate_prepared(...);
    async fn activate(...);
    async fn cleanup_target(...);
    async fn cleanup_source_after_commit(...);
    async fn reconcile_operation(...);
}

trait ActiveWorkGate {
    async fn probe(...);
    async fn set_prompt_gate(...);
    async fn cancel(...);
}
```

Storage primitives:

```rust
RuntimeSwitchRepository::{
    reserve, compare_and_set_target_binding, advance_status, commit,
    fail, cancel, supersede, mark_ambiguous_external_effect,
    try_acquire_worker_lease, renew_worker_lease, release_worker_lease,
    confirm_committed, revert_committing_to_prepared,
    list_non_terminal, list_committed_current,
}

SwitchOperationJournalRepository::{
    append_about_to_send, mark_succeeded, mark_failed, mark_ambiguous,
    list_by_switch, max_sequence,
}
```

### 3. Contracts

#### Reserve and ownership

- Validate the bounded non-empty idempotency key and redacted requested config
  before any write or executor call.
- `reserve` is one transaction: insert-or-get the switch, validate source
  revision/current binding/pending slot, then CAS `pending_switch_id`.
- A reserve conflict produces zero executor calls, zero journal rows, and zero
  durable switch rows.
- Each drive attempt uses a distinct worker owner. A live owner renews its
  lease in a heartbeat while an external future is pending. A worker that
  loses the lease stops without failing, cleaning, or reopening another
  worker's switch.
- Before each non-Committing phase, verify `revision`, `selection_revision`,
  source binding, and `pending_switch_id`. A changed selection is durably
  `Superseded`; another source mismatch is a conflict and preserves current.

#### Strategy and target identity

- `ForceFreshSession` always selects `RestartFreshAndBridge`.
- Live mutation requires the same route, unchanged process fingerprint,
  session-scoped-only changes, negotiated live operations, and no active turn.
- Compatible/probeable restore or a resumable historical binding selects
  `RestartAndResume`; the remaining cases select fresh-and-bridge.
- Reserve first allocates a fresh target id. While status is still `Reserved`,
  a CAS may replace it with the source binding for LiveMutation. After leaving
  `Reserved`, target identity is immutable.
- Adapter/version rollback is represented as another ACP target identity and
  follows the same assessment, journal, Prepare, Commit, and activation path.
  It must not select a Native route, legacy binding schema, or ProviderKind
  dispatch branch.

#### Active work and prompt admission

- The low-level default is Reject for active turn, pending permission, active
  terminal, and background work.
- Wait deadlines are durable, positive, at most 24 hours, and measured from
  switch creation. Wait writes `WaitingForIdle`, closes the prompt gate, and
  reopens it on timeout/failure. When several active categories use Wait, the
  earliest absolute deadline wins; a later category must not overwrite an
  already-expired deadline result.
- Cancel writes AboutToSend before invoking the gate, then requires a
  structured confirmation. Unsupported or unconfirmed cancellation fails; it
  must not fabricate idle state.
- Once initial work is clear, close the prompt gate before entering Preparing.
  Re-probe after the fence closes. Prepared commit also re-probes; active work
  must never cross Commit.

#### Write-ahead journal

- Every external process/session/config/live/cancel operation appends an
  AboutToSend row before the call. Sequence is `max(sequence) + 1`.
- Succeeded skips replay. Failed creates a new attempt. AboutToSend follows the
  persisted retry semantics:
  - `Idempotent`: mark the old attempt failed and replay.
  - `ReconcileBeforeRetry`: Confirmed becomes succeeded, NotFound replays, and
    Ambiguous terminates the switch.
  - `NonRetryableWhenAmbiguous`: never resend automatically.
- A target binding inserted by this switch is durable exactly-once evidence
  that create/restore returned before the journal success marker was written.
- Request fingerprints are bounded summaries, never raw payloads. Debug output
  hides adapter tokens and native result references.

#### Prepare, Commit, and activation

- Restart prepare inserts a target binding as `Preparing`; it never changes the
  session's current pointer. Prepared attachment events remain quarantined.
- LiveMutation targets the source binding and does not insert a new binding.
- Commit is one four-condition CAS over session revision, selection revision,
  current source binding, and pending switch. The same transaction advances
  session revision/generation, target binding generation/state, effective
  selection, switch status, and clears pending.
- The public request awaits a separately spawned driver, so dropping the caller
  cannot cancel the durable drive or Commit critical section.
- Only after Commit succeeds may the executor activate the target and reopen
  prompts. If activation fails after the durable Commit, keep prompt admission
  closed until startup reconciliation successfully replays activation. Activation
  and source cleanup are idempotent. LiveMutation never calls source cleanup
  because source and target are identical.

#### Startup reconciliation

| Durable state | Recovery action |
| --- | --- |
| Requested initial switch | Claim pending ownership, then resume normal Reserve/Prepare. |
| Reserved / WaitingForIdle | Revalidate ownership, close/follow the gate, then resume Prepare. |
| Preparing | Re-close prompt admission and replay journal rules. |
| Prepared | Reacquire and health-check the durable target before Commit. |
| Committing + target current | Confirm Committed and replay idempotent activation. |
| Committing + source current | Revert to Prepared, revalidate, and Commit once. |
| Committed target still current | Replay only the newest switch activation for that session. |
| Terminal with pending pointer | CAS-clear pending without touching current. |
| Missing switch with pending pointer | CAS-clear the orphan pointer. |

For a committed same-binding LiveMutation at durable generation `N`, startup
activation accepts only two attachment states: fence `N - 1` is advanced
exactly once to `N`, while fence `N` is an idempotent already-activated replay.
Every other generation or a different binding/process/native fence fails
closed. Reconciliation must never apply the live mutation twice or advance to
`N + 1` merely because the durable switch is replayed.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Empty/oversized/control-character idempotency key | `runtime_switch_idempotency_key_required` or `_invalid`; zero writes. |
| Requested config over 16 KiB | `runtime_switch_requested_config_too_large`; zero writes. |
| API key/token/secret/native-id field in requested config | `runtime_switch_requested_config_sensitive`; zero writes. |
| Illegal/stale status transition | `runtime_switch_transition_invalid` or `runtime_switch_status_conflict`. |
| Lease unavailable/lost | Return current status or `runtime_switch_lease_lost`; old worker performs no finish. |
| Reject active work | `runtime_switch_busy_<kind>`; no external operation journal. |
| Wait expires | `runtime_switch_wait_timeout`, Failed, prompt gate open. |
| Cancel unsupported/unconfirmed | `runtime_switch_cancel_unsupported` or `_unconfirmed`; source remains current. |
| Non-retryable AboutToSend without durable evidence | `runtime_switch_ambiguous_external_effect`; never resend create. |
| Desired selection changes before Commit | Superseded; source current, target Failed/cleaned. |
| Target binding missing at Commit | Transaction rollback with `runtime_switch_target_binding_missing`. |
| Activate fails after Commit | Keep Committed authoritative and prompt admission closed; surface activation error, then activate and reopen on startup replay. |
| Committed LiveMutation attachment is not exactly `N - 1` or `N` | `runtime_switch_generation_invalid` or the exact attachment-fence conflict; keep prompts closed and do not mutate generation. |

### 5. Good/Base/Bad Cases

- Good: `session/new` returned, target binding was inserted, then the process
  crashed before journal success. Startup marks the operation succeeded from
  the binding and never sends a second create.
- Good: SQLite committed same-binding generation `N` before process loss;
  startup re-fences the still-registered `N - 1` attachment once, and a second
  reconciliation observes `N` without another config mutation.
- Base: a caller retries the same idempotency key while another worker owns the
  lease. It receives the same switch id/current status and does not drive.
- Bad: assess/spawn before reserve, hold only an in-memory mutex, or mark a
  source idle in SQLite without provider cancellation confirmation.
- Bad: handle an Adapter regression by registering a Native runtime or reading
  a legacy provider binding instead of selecting a verified ACP descriptor.
- Bad: treat Committed as fully activated. A crash can occur after the DB
  transaction and before attachment activation, so startup must replay it.
- Bad: accept any attachment generation below `N`, increment an already-`N`
  handle to `N + 1`, or rerun the provider config mutation during replay.

### 6. Tests Required

- `cargo test -p vibex-core -p vibex-db -p vibex-agent`.
- Strategy matrix: live prerequisites, PreferResume, forced fresh, mixed
  process/session changes, capability and active-turn fallbacks.
- Side-effect gate: reserve conflicts assert no executor calls/journal rows.
- Idempotency: concurrent same-key requests assert one spawn/create and one id.
- Busy gate: four categories x Reject/Wait/Cancel, timeout, unsupported cancel.
- Multiple active Wait categories assert that the earliest deadline terminates
  the switch and reopens the gate.
- Hot switch: prepare failure, Superseded, same-binding LiveMutation generation,
  caller drop shield, target cleanup, source pointer preservation, and an
  activation failure that stays closed until startup replay succeeds.
- Crash matrix: Reserved, AboutToSend create/config, binding-before-marker,
  Prepared, Committing before transaction, Committed before activation.
- Committed LiveMutation recovery tests assert exact `N - 1 -> N` re-fence,
  idempotent `N`, rejection of all other generations, one config mutation, and
  unchanged binding/process/native ownership.
- Lease: unexpired skip, heartbeat during slow calls, expired takeover, and old
  worker stopping without terminal writes.
- Redaction: RuntimeBinding, switch intent/records, journal request/records do
  not Debug-print native ids, resume identities, tokens, or config payloads.

### 7. Wrong vs Correct

#### Wrong

```rust
let target = executor.create_session(request).await?;
repository.reserve(conn, request)?;
repository.commit(conn, target)?;
```

#### Correct

```rust
let switch = repository.reserve(conn, request)?;
let lease = coordinator.claim_with_heartbeat(&switch)?;
let op = journal.append_about_to_send(&switch, CREATE_SESSION)?;
let target = executor.create_session(&switch, &op).await?;
repository.insert_prepared_binding(&target)?;
journal.mark_succeeded(&op, target.native_reference())?;
repository.commit(conn, switch.commit_request())?;
executor.activate(&target).await?;
```

## Scenario: Desired Runtime Selection And Seamless ACP Activation

### 1. Scope / Trigger

- Trigger: an ordinary ACP session is created, queries or changes its
  product-level Agent/Profile/Model selection, cancels a pending seamless
  change, or resumes a durable switch after restart.
- `RuntimeSelectionService` owns the provider-neutral desired/effective API;
  SQLite remains authoritative and ACP owns only process, attachment and
  active-work execution.

### 2. Signatures

```rust
AgentSessionRuntimeRepository::enqueue_initial_runtime_switch(
    conn, switch_id, DesiredRuntimeSwitchEnqueueRequest,
) -> RuntimeSwitchRecord

RuntimeSelectionService::{
    initialize_new_session(session_id, desired),
    set_desired_runtime(request),
    get_selection_state(session_id),
    cancel_switch(request),
    reconcile_on_startup(),
}

RuntimeSelectionResolver::{
    resolve(session_id, desired, preferred_adapter_id),
}
```

Desktop boundary:

```text
agent_switch_runtime
agent_set_desired_runtime
agent_get_runtime_selection
agent_cancel_runtime_switch
agent://runtime-selection-event
```

### 3. Contracts

- Session creation first inserts an `Initializing` Logical Session whose
  `current_agent_id` equals the requested Agent and whose runtime selection
  fields are otherwise empty. No process or ACP request may occur yet.
- `enqueue_initial_runtime_switch` atomically writes the complete desired
  selection, advances `selection_revision` from 0 to 1, and inserts one
  `Requested` switch with `source_binding_id = NULL`. Its target binding id and
  requested config are durable before spawn, `session/new`, `session/load`, or
  any other Adapter side effect.
- The initial idempotency key is deterministic (`session-init:<session_id>`).
  An exact retry returns the same switch; another target or partially
  initialized session fails closed.
- `RuntimeSelectionService::initialize_new_session` resolves the exact ACP
  Adapter, enqueues the initial intent, then calls `drive_switch`. The normal
  Reserve/Prepare/Commit/activate state machine creates the first attachment;
  there is no direct `AgentProvider::create_session` fallback.
- Commit atomically advances current binding, effective selection, activation
  generation, and session revision. The session becomes `Idle` only after the
  durable switch commits and the exact attachment activates.
- Startup reconciliation discovers the durable initial `Requested` switch and
  may resume it after any post-enqueue crash. It must never infer initial state
  from an unjournaled external attachment.
- `SetDesiredAgentSessionRuntime` atomically advances `selection_revision` and
  inserts a `Requested` switch. Only the current selection revision may claim
  pending ownership and Commit. Query always rebuilds the product state from
  SQLite; broadcast is only a wake-up optimization.
- Seamless policy applies the same bounded `Wait` disposition to active turn,
  pending permission/terminal-create, active Agent terminal and background
  work. Cancelling a switch never calls active-work cancellation.
- `FailedUsingPrevious` keeps ordinary prompt admission closed until the
  durable current binding is proven usable again. After Owner/background
  materialization activates that exact binding and generation, a storage CAS
  may restore `Ready` only when desired equals effective, no switch is pending,
  and the binding row is still Current. The failed switch stays in the journal.
- Re-selecting the already effective runtime while `FailedUsingPrevious` is an
  explicit recovery request, not an ordinary `NoChange`: the selection service
  must materialize the exact current runtime before rebuilding authoritative
  state. The same selection while `Ready` remains a side-effect-free no-op.
- A restart target attachment stays Prepared and quarantined before Commit.
  Reacquisition seeds the durable config state, rebuilds runtime evidence and
  replays preferred values. If the rebuilt local state differs, CAS the
  durable old state to that rebuilt state before applying the requested patch;
  otherwise the next config CAS uses a baseline SQLite never stored.
- Agent terminals count as active from successful terminal creation until a
  successful kill, release or wait-for-exit response. Attachment cleanup also
  removes terminal ownership. Failed terminal operations leave the terminal
  active.
- Requested/Prepared/Committed and normal restore/config facts are audit or
  internal events. Only bounded actionable terminal failure is user notice;
  no normal runtime switch row becomes a conversation Timeline item.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Initial request revision/selection/config is not the exact empty-session projection | `runtime_selection_initial_request_invalid`; zero side effects. |
| Session already has current/desired/effective/pending state | `runtime_selection_initial_enqueue_conflict`; preserve existing state. |
| Same initial idempotency key has another target | `runtime_selection_idempotency_payload_conflict`; preserve first intent. |
| Runtime selection service missing during create | `runtime_selection_service_unavailable`; no direct create fallback. |
| Initial switch terminates without Commit | `runtime_selection_initialization_failed`; session becomes recoverable `error`. |
| Current ACP attachment missing/uncommitted | `runtime_selection_current_attachment_missing` / `_uncommitted`. |
| Current attachment config not converged to its generation | `runtime_switch_configuration_unavailable`. |
| Materialized binding/generation is stale, desired differs from effective, or a switch is pending | Preserve `FailedUsingPrevious`; do not reopen prompt admission. |
| Failed selection retries the already effective runtime | Materialize the exact current runtime; return `Ready` only after the strict recovery CAS succeeds. |
| Desired/session revision is stale | `desired_selection_revision_conflict` / `runtime_switch_revision_conflict`. |
| Same idempotency key has another target | `runtime_selection_idempotency_payload_conflict`. |
| Seamless Wait expires | FailedUsingPrevious, source current, prompt gate reopened. |
| Prepared config rebuild CAS conflicts | stop reconciliation; never apply using an unpersisted baseline. |
| Kill/release/wait host call fails | return the host error and keep `active_terminal=true`. |

### 5. Good/Base/Bad Cases

- Good: create commits desired selection plus the initial `Requested` row,
  crashes before `session/new`, and startup reconciliation performs exactly one
  prepared create and one Commit.
- Good: create returns one Ready selection whose `current_binding_id` is the
  committed ACP attachment binding; a later seamless change prepares a second
  quarantined attachment and activates it only after durable Commit.
- Good: restart restores a durable preferred Model, persists the replayed local
  state, then a no-op requested config CAS succeeds against the same baseline.
- Base: setting the already effective selection with no current intent returns
  Ready without spawning a process or creating a native session.
- Base: retrying that same selection from `FailedUsingPrevious` revalidates the
  current attachment (or restores it) before returning Ready.
- Bad: manager calls `session/new`, then tries to backfill desired/effective
  selection and the switch journal from the returned attachment.
- Bad: terminal creation permission completes but active-work probe ignores the
  terminal because only pending terminal-create requests are counted.

### 6. Tests Required

- DB tests assert initial desired selection plus `Requested` switch insertion
  is atomic/idempotent, requires an empty revision-0 session, has a null source,
  and creates no current binding before the driver runs.
- Selection service tests cover Requested/Waiting/Preparing/terminal mapping,
  initial crash recovery, broadcast recovery, latest-revision convergence,
  cancellation separation, timeout fallback, and failed same-selection retry
  without changing the side-effect-free Ready no-op.
- DB and ACP lifecycle tests cover exact-fence recovery from
  `FailedUsingPrevious`, including both an existing attachment and a restored
  attachment; stale binding/generation or divergent selection stays failed.
- Mock ACP tests assert the durable initial switch/journal exists before the
  first spawn or `session/new`, Prepared event quarantine, post-activation
  current routing, exactly-once recovery, live config persistence, and startup
  config-baseline CAS.
- Terminal tests assert create makes the probe active; successful kill,
  release and wait each clear it; detach clears remaining ownership.
- Binding export check, desktop typed invoke mock, TypeScript typecheck and
  desktop build must pass.

### 7. Wrong vs Correct

#### Wrong

```rust
let attachment = provider.create_session(request).await?;
repository.backfill_runtime_state_after_external_create(&attachment)?;
```

#### Correct

```rust
let switch = repository.enqueue_initial_runtime_switch(session, desired)?;
let outcome = coordinator.drive_switch(&switch.switch_id).await?;
assert_eq!(outcome.status, RuntimeSwitchStatus::Committed);
```

The first form leaves an unjournaled external session if the process crashes
between the two lines. The second makes recovery possible before any external
side effect.

## Scenario: Incremental Context Bridge Prepare And Historical Resume

### 1. Scope / Trigger

- Trigger: an ACP restart switch creates a fresh native session or resumes a
  compatible current/historical native session while preserving one Logical
  Session Timeline.
- `crates/agent` owns deterministic bridge construction, `crates/db` owns the
  durable sequence window and apply transaction, and ACP owns restore-candidate
  selection plus cursor inheritance.

### 2. Signatures

```text
runtime_context_bridges(
  switch_id, session_id, target_binding_id,
  from_context_sequence, from_summary_sequence,
  prepare_sequence, summary_sequence, bridge_version,
  content_fingerprint, applied_submission_id,
  applied_context_sequence, created_at_ms, applied_at_ms
)

ContextBridgeRepository::{
  prepare, get_by_switch, get_pending_for_binding, record_successful_turn
}
ContextBridgeService::{prepare_for_switch, pending_for_turn, record_successful_turn}
SwitchTargetExecutor::build_context_delta(intent, prepared_attachment)
```

### 3. Contracts

- Both `RestartAndResume` and `RestartFreshAndBridge` call
  `build_context_delta` after the target binding is durably Preparing and while
  prompt admission is closed. `LiveMutation` creates no bridge attempt.
- The immutable window is
  `(target.last_context_sequence, prepare_sequence]`. Reconcile must insert or
  verify the same switch-scoped row; it must not widen the window. An empty
  Timeline delta needs no row. A non-empty delta filtered to no prompt content
  still keeps metadata so a later completed turn can consume the window.
- SQLite stores only sequence/version/fingerprint/apply metadata. It never
  stores the rendered bridge, Timeline payloads, prompts, native ids, secrets,
  file contents, or terminal output. The fingerprint is lowercase SHA-256 over
  the versioned canonical sanitized projection.
- Restore prefers an exact-compatible or explicitly probeable ACP source.
  Otherwise it selects an Inactive binding with exact Agent/Profile/Adapter,
  ACP transport, state-home, adapter-compatibility and resume identities. A
  newly reserved target binding keeps its own id/generation and copies the
  selected origin's `last_context_sequence`, `last_summary_sequence`, and
  `context_bridge_version`.
- AboutToSend restore recovery must copy the same three cursors when rebuilding
  a missing target binding from confirmed native evidence. `ForceFreshSession`
  never selects a historical binding and starts its target cursors at zero.
- Bridge versions and all cursors are monotonic. Preparing or applying a bridge
  older than the binding version fails closed. A completed retry with no
  pending bridge and no larger sequence performs no binding UPDATE.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Cursor is ahead of Timeline | `context_bridge_timeline_regressed`; no attempt. |
| Invalid sequence/version/fingerprint | `context_bridge_prepare_invalid`; no row. |
| Switch/target/cursor mismatch | `context_bridge_switch_mismatch` or `context_bridge_binding_mismatch`. |
| Same switch has different immutable metadata | `context_bridge_prepare_conflict`; keep the first row. |
| Bridge version would move backwards | `context_bridge_version_regressed`; no prompt/apply. |
| Rebuilt projection differs from fingerprint | `context_bridge_snapshot_changed`; no prompt/apply. |
| No compatible current/historical restore origin | use fresh-and-bridge unless policy requires another result. |

### 5. Good/Base/Bad Cases

- Good: A -> B -> A restores A's native session into a new target binding,
  inherits A's cursors, and prepares only the B-era Timeline delta.
- Good: startup confirms a restore side effect before the target binding was
  written, rebuilds that binding, and inherits the same historical cursors.
- Base: fresh target with no Timeline delta commits without a bridge row.
- Bad: select a Native-transport source as an ACP probe candidate, copy the
  source binding id into the target, or reset bridge cursors during recovery.
- Bad: persist rendered continuity text to make restart recovery convenient.

### 6. Tests Required

- DB migration tests assert schema 27, metadata-only columns, prepare
  idempotency/conflict, version monotonicity, current binding/generation fences,
  atomic apply/audit, rollback, and zero-write completed retry.
- Coordinator/ACP tests cover fresh and resume through the shared hook,
  ForceFresh bypass, incompatible-source historical selection, cursor
  inheritance, A -> B -> A missing-window behavior, and recovery inheritance.
- Run `cargo test -p vibex-core -p vibex-db -p vibex-agent -p vibex-agent-acp`,
  workspace checks, bindings drift, fmt, clippy, and Trellis validation.

### 7. Wrong vs Correct

#### Wrong

```text
restore newest process -> set target cursors to zero
serialize full Timeline into runtime_context_bridges.prompt_text
```

#### Correct

```text
select exact restore origin
  -> create new prepared target fence
  -> inherit origin cursors
  -> snapshot missing Timeline window
  -> store only versioned fingerprint metadata
  -> Commit target
```

## Scenario: Runtime Snapshot, Lease, Idle Sweep And Warm Cache

### 1. Scope / Trigger

- Trigger: Desktop, Remote, a durable submission worker, or a runtime switch
  needs to observe, materialize, protect, or reclaim an in-memory ACP process
  or exact session attachment.
- `RuntimeLifecycleService` owns provider-neutral leases and reconnect cursors.
  ACP registries remain the only owners of process and attachment resources;
  SQLite remains authoritative for the current binding and activation
  generation.

### 2. Signatures

```rust
trait RuntimeLifecycleBackend {
    fn snapshot(session_id) -> RuntimeBackendSnapshot;
    fn process_snapshot(process_id) -> RuntimeProcessSnapshot;
    fn touch(target, now_ms);
    async fn materialize_owner(session_id) -> RuntimeBackendSnapshot;
    async fn sweep(now_ms, protected_targets) -> RuntimeSweepReport;
}

RuntimeLifecycleService::{
    snapshot, process_snapshot, events,
    attach, detach, materialize_internal, acquire_internal,
    sweep_once, start(tokio_runtime_handle), stop,
}

AgentSessionRuntimeRepository::advance_current_activation_generation(
    conn, session_id, binding_id, expected_generation,
) -> next_generation
```

Desktop commands:

```text
agent_get_runtime_snapshot
agent_get_runtime_process_snapshot
agent_get_runtime_events
agent_attach_runtime
agent_detach_runtime
agent://runtime-event
```

Remote request variants:

```text
get_runtime_snapshot | get_runtime_process_snapshot | get_runtime_events
attach_runtime | detach_runtime
```

### 3. Contracts

- Public snapshots contain bounded provider-neutral state only. They use
  binding id, process id and activation generation as the public fence and
  never expose native session ids, raw commands/env, full fingerprints or
  provider payloads.
- Each service start creates a new `RuntimeStreamId`. Snapshot reads the
  current sequence before backend state so it may cause a harmless replay but
  cannot return old state with a cursor that skips a newer event. Catch-up
  advances only through the last returned page; stream mismatch, ring lag or
  a future sequence sets `reset_required=true` and returns no partial events.
- Client leases are keyed by session, holder scope, client id and role.
  `Owner` and `Viewer` are the only public roles; `BackgroundWorker` and
  `SwitchPreparation` are RAII internal guards. Repeating attach renews the
  same lease, role replacement removes the old role, and Remote holder scope
  includes authenticated device id.
- Default heartbeat/TTL is 30/90 seconds. Every successful lease acquire or
  renew touches its exact attachment/process target. Runtime resource guards
  are separate from durable runtime-switch worker leases: the DB lease owns
  orchestration, while the resource guard prevents sweep.
- Viewer attach never materializes. Owner/background work repairs a missing or
  crashed durable current binding lazily. Rebuild order is immutable:
  create a quarantined Prepared attachment at `durable_generation + 1`, CAS
  both session and current binding generations in one transaction, persist
  required config replay, then activate the exact fence. Prepare failure does
  not advance SQLite; later failure cleans only that prepared fence.
- A committed switch source becomes Inactive warm state instead of being
  detached immediately. Defaults are two warm attachments per logical session,
  five-minute attachment idle, eight reusable zero-attachment processes and
  two-minute process idle. The reusable process remains Ready after its last
  attachment detaches; dedicated and non-Ready processes are not warm entries.
- Sweep enumeration is only a candidate hint. Attachment claim must re-check
  under the registry lock: exact fence, Committed/current or Inactive state,
  latest `last_used_at`, active turn, pending permission/terminal-create,
  active terminal, background work, protected process/attachment lease and
  Ready process evidence. A changed oldest quota candidate aborts that LRU
  pass and is recomputed on the next sweep.
- Process claim atomically re-checks Ready status, zero attachments, zero
  pending requests/host callbacks, no process lease, latest use time and exact
  instance before shutdown. Stale processes reject new attachments and may
  drain immediately once every active-work and lease gate clears.
- Desktop builds one lifecycle instance for Tauri, submission and Remote,
  starts it once in setup with Tauri's explicit Tokio runtime handle, and
  awaits `stop()` on `RunEvent::Exit` before Tauri cleanup. The synchronous
  Tauri setup callback is not an ambient Tokio reactor, so lifecycle startup
  must schedule through the supplied handle instead of calling bare
  `tokio::spawn`.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Zero/invalid lifecycle durations or limits | `runtime_lifecycle_config_invalid` or `acp_runtime_lifecycle_limits_invalid`; no worker starts. |
| Client requests an internal role | `runtime_lease_internal_role_forbidden`; zero backend call. |
| Internal API receives Owner/Viewer | `runtime_lease_internal_role_invalid`; no guard. |
| Empty/oversized holder scope | `runtime_client_scope_invalid`; no lease. |
| Service is stopping | `runtime_lifecycle_stopping`; no materialize or acquire. |
| Lifecycle starts from a synchronous host callback | Schedule the sweep with the supplied Tokio handle; do not require an ambient reactor. |
| Cursor epoch mismatch, lag or future sequence | empty batch with `reset_required=true`; caller refetches snapshot. |
| Viewer has no existing Ready/Inactive attachment | `NotMaterialized`, no lease and no spawn. |
| Prepared generation CAS loses | clean only the losing prepared fence; durable winner remains current. |
| Binding row is not the current session binding during generation CAS | transaction rolls back both generation updates. |
| Candidate fence/state/timestamp/process evidence changed | claim returns no-op; replacement remains routable. |
| Process is Crashed/Closing/Closed | never retain it as a warm reusable process. |

### 5. Good/Base/Bad Cases

- Good: a Viewer expires while an Owner remains; only the Viewer count drops
  and the exact runtime stays protected.
- Good: three idle attachments with limit two evict the oldest Inactive fence,
  preserve current, and leave their shared process alive with two attachments.
- Good: a stale reusable process drains its safe attachments, then one atomic
  process claim shuts it down; a late old event cannot target a replacement.
- Good: Tauri setup passes its runtime handle and the lifecycle sweep starts
  without entering an ambient Tokio context.
- Base: the last attachment leaves a Current reusable process with
  `attached_session_count=0`; process idle/LRU policy decides when to stop it.
- Bad: increment SQLite generation before Prepared exists, treat a DB worker
  lease as a process lease, kill a pooled process when one session detaches, or
  close a candidate using timestamps read before an external await.

### 6. Tests Required

- Core/DB: opaque runtime ids and serde shapes; bounded snapshot projection;
  current-generation CAS success, stale conflict and transaction rollback.
- Agent: Owner/Viewer materialization, idempotent heartbeat/touch, role and
  device scope replacement, expiry, internal guard drop, paged catch-up, ring
  lag reset, stop/restart stream epoch, and startup from a synchronous thread
  with an explicit Tokio runtime handle.
- ACP: snapshot bounds/redaction, Prepared-before-CAS materialization, prepare
  failure generation stability, exact idle claim after touch, all active-work
  gates, stable LRU quota, process lease protection, reusable warm process,
  stale drain, crash/replacement and SwitchPreparation guard cleanup.
- Remote: ReadOnly Viewer, denied ReadOnly Owner, internal-role rejection
  before backend, FullControl Owner, and device-scoped detach.
- Final gates: `cargo test -p vibex-agent-acp --lib`,
  `cargo test -p vibex-remote --lib`, `cargo check --workspace --all-targets`,
  and `pnpm check`.

### 7. Wrong vs Correct

#### Wrong

```text
increment durable generation
  -> restore native session directly as current
  -> detach old attachment

list idle candidates
  -> await cleanup
  -> kill candidate process without exact recheck

Tauri synchronous setup
  -> lifecycle start calls bare tokio::spawn
  -> panic because no ambient reactor is entered
```

#### Correct

```text
prepare exact generation in quarantine
  -> transactionally CAS session + binding generation
  -> persist config replay
  -> activate exact fence
  -> retain source as Inactive warm state

list candidate hints
  -> atomically recheck fence/state/latest touch/active work/leases
  -> remove routes before await
  -> detach attachment reservation
  -> atomically claim zero-attachment process before shutdown

Tauri synchronous setup
  -> pass Tauri's Tokio runtime handle to lifecycle start
  -> schedule the sweep through the explicit handle
```

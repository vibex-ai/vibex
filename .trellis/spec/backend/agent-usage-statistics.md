# Agent Usage Statistics

Vibex records provider-neutral Agent token consumption from online ACP turns.
This domain is separate from Provider quota and balance records: it contains no
prices, costs, billing estimates, or Provider-native session identities.

## Scenario: Durable Cumulative Usage Accounting And Typed Queries

### 1. Scope / Trigger

- Read this contract when changing ACP usage decoding, prompt dispatch
  lifecycle, runtime binding/generation fences, Agent usage migrations,
  checkpoint delta calculation, or statistics queries.
- `agent-acp` decodes Provider payloads and correlates them to an active turn.
  `crates/agent` transports typed, best-effort telemetry. `DesktopRuntime` owns
  the durable service, and `crates/db` owns transactional facts/checkpoints.
- `provider_usage_records` remains the Provider quota/balance domain. Agent
  execution facts must not be written there.

### 2. Signatures

Core telemetry contracts use optional cumulative fields. Usage DTO struct fields
serialize as camelCase and enum values as snake_case. The internally tagged
`AgentUsageTimeZone` keeps its Rust `offset_minutes` variant field spelling.

```text
AgentUsageTokenValues {
  input_tokens?, output_tokens?, thought_tokens?, cached_read_tokens?,
  cached_write_tokens?, total_tokens?
}

AgentUsageObservation {
  stream: { session_id, binding_id, activation_generation, agent_id,
            provider_profile_id, model_id },
  execution: AgentUsageExecution?,
  counter_origin: known_zero | resumed | restored_checkpoint | unknown,
  observation_sequence,
  cumulative: AgentUsageTokenValues,
  context_window_used_tokens?, context_window_size_tokens?,
  source: prompt_response | session_usage_update,
  observed_at_ms
}

AgentUsageTelemetryEvent =
  ExecutionDispatched { execution, counter_origin }
  | Observation(AgentUsageObservation)
  | ExecutionStatus(AgentUsageExecutionStatusUpdate)

AgentUsageRepository::record_execution(conn, execution) -> VibexResult<bool>
AgentUsageRepository::record_execution_status(conn, update) -> VibexResult<bool>
AgentUsageRepository::apply_observation(conn, observation)
  -> VibexResult<AgentUsageApplyOutcome>
RuntimeBindingRepository::claim_usage_zero_baseline(
  conn, binding_id, activation_generation, usage_execution_id
) -> VibexResult<bool>

AgentUsageService::apply_telemetry_event(event) -> VibexResult<bool>
AgentUsageService::query_statistics(request) -> VibexResult<AgentUsageStatistics>
AgentBackend::usage_statistics(request)
  -> BackendFuture<AgentUsageStatistics>
```

The typed query is:

```text
AgentUsageStatisticsRequest {
  range: today | last_7_days | last_30_days | all_time,
  agent_ids[], project_ids[], provider_profile_ids[], model_ids[], session_ids[],
  dimension: time | agent | project | model_provider | model,
  trend_metric: requests | total_tokens | input_tokens | output_tokens | cached_tokens,
  sort_metric, sort_direction,
  time_zone: system | { kind: fixed_offset, offset_minutes }
}

AgentUsageStatistics {
  generated_at_ms, effective_range, totals,
  trend_buckets[], dimension_rows[], filter_options,
  annual?: { effective_range, days[] }
}

AgentUsageAnnualDay {
  id, label, start_at_ms, end_at_ms, requests, total_tokens,
  models[]: { model_id, label, requests, total_tokens }
}
```

SQLite owns these additive records:

```text
agent_usage_checkpoints(
  usage_stream_id PRIMARY KEY, session_id, binding_id,
  last_activation_generation, agent_id, provider_profile_id, last_model_id,
  reset_epoch, counter_origin, cumulative_* NULL,
  last_usage_execution_id NULL, last_observation_sequence,
  created_at_ms, updated_at_ms,
  UNIQUE(session_id, binding_id)
)

agent_turn_usage_facts(
  usage_execution_id PRIMARY KEY, message_submission_id NULL,
  session_id, project_id, workspace_id, binding_id, activation_generation,
  reset_epoch, agent_id, provider_profile_id, model_id, execution_status,
  *_delta NULL, cumulative_*_after NULL,
  context_window_used_tokens NULL, context_window_size_tokens NULL,
  reported_fields, coverage, last_source NULL, reset_reason NULL,
  dispatched_at_ms, completed_at_ms NULL, last_observed_at_ms NULL,
  created_at_ms, updated_at_ms
)

session_runtime_bindings(
  ...,
  usage_zero_baseline_state: available | claimed | unavailable,
  usage_zero_baseline_execution_id NULL,
  usage_zero_baseline_activation_generation NULL
)
```

### 3. Contracts

#### Capture And Execution Identity

- ACP token counters are cumulative within a durable runtime binding lineage.
  Every numerical field is independent and optional. A missing field is unknown;
  only an explicitly reported `0` is zero.
- All token and context values must fit SQLite's signed integer range before
  persistence. Context-window gauges never contribute to token totals.
- A `UsageExecutionId` is stable for one actual prompt execution. Durable message
  submissions derive it from `MessageSubmissionId`; continue/retry operations
  receive their own stable execution id.
- Create a fact and count a request only after `session/prompt` was accepted by
  the ACP request dispatcher. Preparation, validation, binding lookup, and other
  pre-dispatch failures create no fact and consume no zero-baseline claim.
- Completed, failed, and interrupted dispatches all count as requests. A missing
  usage payload changes coverage, not request count.
- Telemetry delivery and persistence are best-effort relative to the Agent turn.
  Malformed usage or a database telemetry failure must not fail the prompt.

#### Zero Baseline, Fences, And Ordering

- A newly inserted binding starts with one `available` zero baseline. A binding
  upgraded from an older schema starts `unavailable`, because its native counter
  may already be nonzero.
- Claim the zero baseline only while handling the real
  `ExecutionDispatched` event. The claim is exact and idempotent for
  `(binding_id, activation_generation, usage_execution_id)` and cannot be
  transferred to another execution.
- A claimed zero baseline may turn the first reported cumulative value into a
  delta only when that exact execution has no prior fact on the binding.
- `binding_id` defines the counter stream. `activation_generation` and
  `observation_sequence` fence stale events; model changes do not create a new
  stream. A different binding never subtracts the old binding's checkpoint.
- For executions with different `dispatched_at_ms`, timestamp order decides
  persistence order. If timestamps are equal, SQLite fact `rowid` decides which
  fact was persisted first. A late observation for the earlier fact is ignored
  and must not advance the checkpoint or open a reset epoch.

#### Atomic Delta Application

- `apply_observation` uses an immediate SQLite transaction. Fact creation or
  enrichment and checkpoint advancement commit together or neither commits.
- An observation without exact execution correlation performs no durable write;
  it may update the live runtime snapshot only. Advancing a checkpoint without
  an execution can consume a later turn's delta.
- Apply each cumulative field independently:
  - No checkpoint plus an exact claimed zero origin: delta equals current.
  - No proven zero origin: store current as a baseline and leave delta unknown.
  - Monotonic value: delta equals `current - previous`.
  - Missing value: preserve the existing checkpoint/fact field.
  - Regression within the same execution: ignore only the regressed fields.
  - Regression from a newer execution: increment `reset_epoch`, store a new
    baseline, emit no delta for the reset values, and mark incomplete coverage.
- Duplicate or older generation/sequence observations are no-op successes.
  Late positive enrichment adds to the same fact without replacing known deltas.
- `coverage` is `complete`, `partial`, `baseline_only`, `unreported`, or
  `unsupported`. Missing values stay nullable all the way to the UI.

#### Aggregation And Privacy

- Queries are capped at `MAX_AGENT_USAGE_QUERY_ROWS` (100,000 facts). The selected
  range and all cross-filters apply to totals, trend buckets, and dimension rows.
- Today uses local-hour buckets, 7/30 days use local-day buckets, and all time
  uses local-month buckets. Materialize empty buckets. Calendar boundaries are
  computed from the requested system/fixed-offset time zone, not implicit
  SQLite date behavior.
- Every successful query also projects the latest 365 local calendar days for
  annual charts. This projection ignores only the selected range: it applies the
  same Agent, Model Provider, Model, Project, and Session filters. Materialize all
  365 days, use local-midnight boundaries so DST stays correct, and include daily
  model Requests and Total Token without changing their nullable coverage rules.
  The field is optional on the wire so older serialized responses remain readable;
  a current backend returns `Some` even when every annual day is empty.
- The five dimensions are Time, Agent, Project, Model Provider, and Model.
  Stable Vibex ids are group keys; current labels are projections with the id as
  fallback. Sort by the selected metric/direction, then case-folded label, then id.
- Requests equal the number of dispatched execution facts, including failed and
  interrupted facts.
- Total Token uses a reported `total_delta` when present. It may derive a value
  from `input_delta + output_delta` only when both are present and must expose
  derived/partial coverage. Never add thought, cache read, or cache write to a
  reported/derived total again.
- Cached Token means cached-read Token. Cache hit rate includes only facts with
  both input and cached-read deltas:
  `sum(cached_read) / (sum(input) + sum(cached_read))`. No eligible facts is
  unknown; an eligible zero denominator is 0%. Coverage reports eligible versus
  total requests.
- Facts, checkpoints, queries, logs, and diagnostics contain no prompt/response
  body, raw ACP payload, Provider-native session id, cost, price, currency, or
  pricing version.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Malformed, negative, non-finite, oversized, or unsupported ACP usage field | Ignore the field/payload, emit only a bounded diagnostic, and let the turn continue. |
| Token/context value exceeds `i64::MAX`, sequence is zero, or observation attribution is invalid | `validation/agent_usage_observation_invalid`; no fact/checkpoint mutation. |
| Execution has a negative generation/time, empty model, or oversized model | `validation/agent_usage_execution_invalid`; no insert. |
| Existing execution id has different attribution | `conflict/agent_usage_execution_conflict`; preserve the existing fact. |
| Terminal status is `dispatched` or completes before dispatch | `validation/agent_usage_status_invalid`. |
| Observation has no exact execution | Successful no-op for durable storage. |
| Generation/sequence is stale, or an older same-millisecond fact arrives late | Successful ignored outcome; checkpoint and reset epoch do not change. |
| Zero-baseline generation is negative, binding is absent, or generation is stale | Stable `agent_usage_zero_baseline_*` validation/conflict error; telemetry logs it and does not fail the turn. |
| Query range/time zone/model filter is invalid | Stable `validation/agent_usage_query_range_invalid`, `agent_usage_time_zone_invalid`, or `agent_usage_model_filter_invalid`. |
| Selected range exceeds 100,000 facts | `validation/agent_usage_query_too_large`. |
| Delta or aggregate arithmetic overflows | Stable `agent_usage_delta_overflow` or `agent_usage_aggregate_overflow`; never wrap. |
| SQLite begin/write/commit fails | Stable redacted `storage/agent_usage_*` error; the Agent turn still succeeds. |

### 5. Good/Base/Bad Cases

- Good: a new binding dispatches execution A, A claims the zero origin, and its
  first cumulative input value of 120 persists as a delta of 120.
- Base: a resumed binding first reports input 120 without a proven zero origin;
  the fact is `baseline_only`. Its next execution reports 150 and receives a
  delta of 30.
- Good: executions A and B share a millisecond; A was inserted first, B advanced
  the checkpoint, and a late A observation is ignored by fact `rowid` ordering.
- Base: an input-only payload produces a known input metric and unknown output,
  total, and cache metrics. It never fabricates zeroes.
- Bad: claim a zero baseline while preparing a turn that never reaches the ACP
  dispatcher.
- Bad: subtract a checkpoint from another binding, treat a model change as a
  reset, or let a stale generation mutate a current checkpoint.
- Bad: add thought/cache fields on top of total Token, or persist Provider cost
  fields beside Agent usage.

### 6. Tests Required

- ACP decoder tests cover cumulative partial fields, context-only updates,
  malformed/oversized values, ignored cost fields, and execution correlation.
- Adapter/Manager tests prove pre-dispatch failures emit no
  `ExecutionDispatched` event, create no fact, and leave the zero claim available.
- Dispatch tests prove exactly one event/claim for the actual execution and
  idempotence for its repeated delivery.
- Repository tests cover known-zero, resumed baseline, monotonic deltas, partial
  merging, replay, regression epochs, binding/generation fences, and rollback of
  fact/checkpoint together.
- Repository regression tests create same-millisecond facts and assert a late
  older observation cannot change fact deltas, checkpoint, or reset epoch.
- Query tests cover all ranges/time zones, empty buckets, every cross-filter and
  dimension, deterministic sorting, partial/derived coverage, cache-hit
  eligibility, empty data, row caps, overflow handling, the fixed 365-day annual
  window, range independence, and daily model projections.
- One integration test runs fake ACP cumulative events through adapter,
  `DesktopRuntime`, SQLite, the typed query service, and the GPUI view model.

### 7. Wrong vs Correct

#### Wrong

```rust
// Preparation can still fail, so this consumes the only zero origin too early.
claim_usage_zero_baseline(binding, generation, execution_id)?;
let started = process.start_request("session/prompt", params)?;
```

```rust
// Timestamp ties make a lexicographic execution id an invented chronology.
if incoming.dispatched_at_ms <= checkpoint.dispatched_at_ms {
    advance_checkpoint(incoming)?;
}
```

#### Correct

```rust
let started = process.start_request("session/prompt", params)?;
emit(AgentUsageTelemetryEvent::ExecutionDispatched {
    execution: context.dispatched_at(now_ms),
    counter_origin,
});
```

```rust
let precedes = incoming.dispatched_at_ms < current.dispatched_at_ms
    || (incoming.dispatched_at_ms == current.dispatched_at_ms
        && incoming_fact_rowid < current_fact_rowid);
if precedes {
    outcome.ignored_stale_observation = true;
    tx.commit()?;
    return Ok(outcome);
}
```

# Logging Guidelines

Vibex needs structured logs for a long-running local desktop service, provider
compatibility debugging, remote access auditability, and user-controlled
diagnostic exports.

Evidence: current tracing, audit, diagnostics code/tests, and source-backed specs.

## Logging Model

Use structured logging with spans for:

- App startup and shutdown.
- Provider process lifecycle.
- Agent turn lifecycle.
- Timeline append and live event delivery.
- Permission request and resolution.
- Provider injection planning.
- Git, filesystem, and terminal service operations.
- Remote pairing, reconnect, and device authorization.
- Relay room lifecycle.

Prefer stable fields over prose-only messages so diagnostic packages can be
filtered by project, session, provider, device, and correlation id.

## Required Fields

Where available, include:

- `project_id`
- `workspace_id`
- `session_id`
- `provider_type`
- `provider_profile_id`
- `device_id`
- `request_id` or `correlation_id`
- `timeline_sequence`
- `operation`
- `duration_ms`

Do not invent fake ids. Omit fields that do not apply.

## Levels

- `trace`: high-volume provider or terminal protocol details, disabled by
  default.
- `debug`: lifecycle detail useful for development and compatibility debugging.
- `info`: user-meaningful state transitions such as session start, provider
  switch, device paired, or Relay connected.
- `warn`: recoverable anomalies such as reconnect catch-up, provider capability
  mismatch, failed health probe, or dropped terminal frame due to throttling.
- `error`: failed user action, provider process crash, storage failure,
  unrecoverable config export failure, or security/auth failure.

## Raw Provider Logs

Adapters may keep raw event logs for Codex, Claude Code, and ACP compatibility.
Raw logs must be:

- Redacted.
- Stored under diagnostic/log paths, not mixed into normal user timeline.
- Bounded by retention or user-controlled export.
- Labeled with provider version and adapter version when known.

## Audit Logs

Audit logs are separate from diagnostic logs. Record remote or destructive
actions that affect local state, including permission approvals, Git operations,
terminal input, file writes/deletes, provider export, and device revocation.

Audit logs should reference operation ids and timeline ids rather than copying
sensitive payload contents.

## Redaction

Always redact:

- API keys and auth tokens.
- HTTP headers that may contain secrets.
- Private keys and pairing secrets.
- Environment variable values that look credential-like.
- Full `.env` contents.
- Provider injection secret values.

## Anti-Patterns

- Do not log full prompts by default.
- Do not log terminal output at `info` level.
- Do not put secrets into structured fields and rely on later filtering.
- Do not let Relay logs include decrypted business payloads because Relay must
  never see plaintext business payloads.

## Scenario: ACP Runtime Observability And Diagnostic Safety

### 1. Scope / Trigger

- Trigger: code records an ACP process/session operation, durable runtime
  switch, queued message, restore, route anomaly, or process cleanup result.
- Trigger: Desktop or a CLI exports a diagnostic bundle containing Runtime
  aggregates.
- `vibex-agent` owns the provider-neutral collector and safe log context;
  Desktop owns the single production instance; `vibex-diagnostics` owns the
  bounded diagnostic projection.

### 2. Signatures

```text
RuntimeObservability::increment(name, operation?, result)
RuntimeObservability::observe_duration(name, operation?, result, duration)
RuntimeObservability::snapshot() -> RuntimeMetricSnapshot

RuntimeLogContext::new(operation)
  .with_process_spawn_fingerprint(full_fingerprint)
  .with_native_session_id(full_native_session_id)
  .emit(level, event_code, result, error_code?, duration_ms?)

DiagnosticBundleServiceConfig::with_runtime_observability(
  Arc<RuntimeObservability>,
)

DiagnosticBundle {
  metadata.schemaVersion: "diagnostic_bundle.v2",
  runtime: DiagnosticRuntimeSection {
    processStartedAtMs,
    snapshotAtMs,
    seriesLimit,
    series: DiagnosticRuntimeMetric[]
  }
}
```

Each metric series contains only `name`, optional enum-backed `operation`,
enum-backed `result`, `count`, and optional duration total/min/max/last.

### 3. Contracts

- Desktop creates one `Arc<RuntimeObservability>` and injects it into runtime
  selection, switch coordination, durable submission, ACP runtime, and
  diagnostics. Default constructors may create isolated collectors for tests
  or non-Desktop callers, but production bootstrap must not split observations.
- Metric names, operations, and results are Rust enums. Callers cannot attach
  arbitrary labels. Logical session, binding, process, switch, Agent/Profile,
  fingerprint, native id, path, error text, or provider payload never becomes
  a metric key.
- The collector is in-process, best effort, deterministic, and capped at 256
  series. Counter/duration totals saturate; lock failure or series saturation
  drops only the observation and never changes Runtime behavior.
- The catalog covers spawn/initialize/open/prompt, switch prepare/commit and
  desired-to-effective convergence, startup reconciliation and active-work
  policy, queued/duplicate/ambiguous submission, restore/fresh bridge,
  stale/acquire/crash, unknown/unroutable/quarantined/fallback events,
  transcript lag, and process-tree cleanup failure.
- `RuntimeLogContext` stores correlation ids only when available and never
  invents them. A full spawn fingerprint is reduced to a 16-character prefix;
  a native session id is replaced with a domain-separated SHA-256 projection.
  The raw inputs are not retained by the context, `Debug`, tracing, or serde.
- Runtime events use stable fields where known:
  `logical_session_id`, `binding_id`, `process_instance_id`,
  `activation_generation`, `switch_id`, `agent_id`, `adapter_id`,
  `adapter_version`, `provider_profile_id`,
  `process_spawn_fingerprint_prefix`, `native_session_id_hash`, `operation`,
  `restore_outcome`, `result`, `error_code`, and `duration_ms`.
- Event/error codes accept only bounded stable-code syntax. Invalid provider
  text projects to `invalid_code`; prompt text, raw errors, headers, env values,
  unknown envelopes, and extension payloads are never copied into tracing.
- Diagnostic Runtime metrics contain catalog strings and aggregates only. The
  request record limit also caps exported series. A caller without a live
  collector receives a valid empty Runtime section, not a missing field.
- Unknown or malformed ACP envelopes expose only bounded classification
  metadata. The ACP debug ring, canonical raw extension, Timeline, tracing, and
  diagnostics must pass one sentinel scan with no secret/native/raw payload.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Collector mutex is unavailable | Drop the observation; preserve the Runtime result. |
| A new series would exceed 256 | Drop only that new series; snapshot remains valid and bounded. |
| Event/error code contains provider prose or invalid syntax | Emit `invalid_code`, never a sanitized fragment of the prose. |
| Diagnostic service has no collector | Export `diagnostic_bundle.v2` with an empty `runtime.series`. |
| Unknown/malformed extension contains secret or native id | Keep bounded method/classification metadata only; increment the fixed anomaly metric. |
| Restore returns `AuthenticationRequired` | Record the enum result; do not call `session/new` or increment fresh bridge. |

### 5. Good/Base/Bad Cases

- Good: one durable switch followed by one queued prompt produces prepare,
  commit, desired-to-effective, queue-wait, prompt, and acquire series in the
  same snapshot without any session or binding label.
- Good: tracing contains a fingerprint prefix and native-id hash while a scan
  confirms the full values, prompt, provider error, and secret are absent.
- Base: a diagnostic CLI has no Desktop collector and exports an empty Runtime
  section with timestamps and the fixed series limit.
- Bad: create a collector per service, attach `session_id` as a metric label,
  record `error.to_string()` as `error_code`, or serialize an unknown envelope
  into a diagnostic detail.

### 6. Tests Required

- Collector tests cover concurrent lossless aggregation, deterministic order,
  duration total/min/max/last, saturation, and the hard series cap.
- Tracing subscriber tests capture real events and assert safe correlation
  fields are present while full fingerprints/native ids/secrets/provider text
  are absent.
- Integration tests cover one shared observer across durable switch, queue,
  prompt, process, and attachment owners, including duplicate and ambiguous
  paths without a second external side effect.
- Diagnostic tests and `pnpm smoke:diagnostics` assert schema v2, record-limit
  projection, a valid empty section, and redaction across all output surfaces.
- `cargo test --workspace`, scoped Clippy with repository baseline allowances,
  `pnpm check`, binding drift, forbidden-Native scan, and `git diff --check`
  must pass before commit.

### 7. Wrong vs Correct

#### Wrong

```rust
metrics.increment("prompt", &session_id, error.to_string());
tracing::warn!(native_session_id, fingerprint, raw = ?envelope);
```

#### Correct

```rust
observability.observe_duration(
    RuntimeMetricName::PromptLatency,
    None,
    RuntimeMetricResult::Success,
    elapsed,
);
RuntimeLogContext::new("session_prompt")
    .with_native_session_id(native_session_id)
    .with_process_spawn_fingerprint(fingerprint)
    .emit(RuntimeLogLevel::Info, "runtime_prompt_finished", "success", None, duration_ms);
```

Metrics remain low-cardinality aggregates; high-cardinality correlation stays
in a safe structured context that never retains the raw sensitive inputs.

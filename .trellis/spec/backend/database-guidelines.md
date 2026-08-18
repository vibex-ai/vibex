# Database Guidelines

Vibex stores local state in SQLite plus structured files under `~/.vibex/` and,
optionally, project-level `.vibex/` metadata. The database is the durable source
of truth for projects, sessions, timelines, Provider profiles, devices, and
audit logs.

Evidence: current database code, migrations, tests, and completed storage tasks.

> Legacy cutover note (2026-07-29): later Tauri command, React Query, and
> browser-mock scenarios under the former desktop shell are historical evidence.
> That shell once occupied `apps/desktop`; the current GPUI app now reuses the
> path. The old scenarios' paths, signatures, wiring, and test commands are not
> current implementation contracts, though their domain/database invariants remain useful.

## Storage Layout

Target local paths:

```text
~/.vibex/config.json      Small bootstrap config
~/.vibex/vibex.db         Main SQLite database
~/.vibex/logs/            Diagnostic logs and provider raw logs
~/.vibex/relay/           Device keys and pairing state
~/.vibex/worktrees/       Vibex-managed worktrees
<project>/.vibex/         Optional project metadata and exported indexes
```

Do not store API keys, auth tokens, or private keys in plaintext tables or logs.
Use OS keychain support or encrypted secret storage when available.

## Core Tables

Plan migrations around these domain records:

- `hosts` and `devices`.
- `projects` and `workspaces`.
- `agent_sessions`.
- `agent_timeline_items`.
- `provider_configs`, `provider_bindings`, `provider_health`,
  `provider_usage`, `provider_injection_plans`, and the legacy
  `provider_runtime_option_snapshots` table, plus the active
  `provider_model_runtime_option_snapshots` model cache.
- `agent_configs`, `agent_discovery_records`, and the active Agent-owned
  `agent_runtime_option_snapshots` table.
- `agent_auth_contexts`, `agent_authentication_operations`, and
  `agent_auth_model_catalog_snapshots` for each Agent's one default account,
  its durable operation state, and revision-scoped model evidence.
- `agent_managed_installations`, the durable lifecycle record for verified ACP
  Registry downloads, side-by-side versions, active commands, and crash
  recovery state.
- `mcp_servers`, `skills`, `skill_repos`, and `prompts`.
- `terminals`.
- `git_snapshots`.
- `remote_audit_logs`.

Keep provider-native ids in binding tables or adapter-specific metadata columns,
not as primary Vibex ids.

## Remote Device Tables

Remote trust records are local-first state owned by SQLite:

```text
remote_devices(device_id, display_name, public_key, auth_secret_hash,
  permission_level, status, paired_at_ms, last_seen_at_ms, revoked_at_ms,
  created_at_ms, updated_at_ms)
remote_pairing_codes(pairing_id, code_hash, permission_level, expires_at_ms,
  claimed_device_id, created_at_ms, claimed_at_ms)
remote_audit_logs(audit_id, device_id, action, target_kind, target_id, outcome,
  redacted_summary, request_id, correlation_id, created_at_ms)
```

Rules:

- Store hashes for pairing codes and auth tokens, not plaintext values.
- Device permission/status fields use generated core enums.
- Pairing codes expire and can be claimed once.
- Audit rows store redacted summaries only; never store secrets, terminal input,
  file contents, prompt bodies, provider tokens, or raw pairing/auth material.

## Timeline Persistence

Agent timeline items are append-only by default. Each item needs:

- Vibex session id.
- Monotonic sequence.
- Event kind.
- Author/source.
- Timestamp.
- Provider-native correlation id where applicable.
- JSON payload for event-specific data.

Compaction can add boundary events or summarized records, but must not destroy
the ability to reconstruct user-visible history needed by remote clients.

Replacing the latest user turn is the only interactive truncation exception.
The expected timeline end check, latest-user validation, tail deletion, and
durable edited-message enqueue must share one immediate transaction. The new
submission preserves the logical session id and records a fresh-runtime policy;
never commit a truncated timeline without its recoverable submission row.

## Migrations

- Store migrations in `crates/db`.
- Migrations must be deterministic and safe to run once.
- New tables should include created/updated timestamps when records are mutable.
- Destructive migrations need backup/rollback notes because Vibex is local-first
  and user data may not exist anywhere else.

Migration 43 adds `agent_model_provider_display_order`, an Agent-scoped table
for Config Center presentation order. It is independent from failover storage;
replacement of one Agent's complete profile-id list runs in a transaction and
profile deletion removes its display-order rows.

Migration 44 adds `runtime_switches.activation_completed_at_ms`. A newly
committed switch leaves the marker empty until its target attachment activates;
startup recovery scans only current committed rows with an empty marker. The
migration backfills older committed rows from their commit/update timestamp so
upgrading does not eagerly restore every historical Agent session. Durable
bindings remain lazily materializable when real work later needs them.

Migration 49 adds `agent_message_submissions.required_runtime_policy`. Existing
submissions default to `automatic`; edited-message submissions store
`force_fresh_session` so restart recovery cannot resume stale provider context.

## Transactions

Use transactions for multi-record state transitions:

- Creating a session and native provider binding.
- Appending permission request plus session state change.
- Resolving permission plus timeline event.
- Provider export backup plus native config write record.
- Worktree create/remove records plus project state updates.

If a native side effect cannot be rolled back transactionally, record enough
state before the side effect to support recovery on restart.

## Naming

- Table names should be plural snake_case.
- Columns should be snake_case.
- Foreign keys should include the referenced domain name, such as `session_id`,
  `project_id`, or `provider_profile_id`.
- JSON columns must have documented payload versions.

`agent_runtime_option_snapshots` is the active Runtime Option Catalog fallback. It
stores one row per `agent_id` (not per Provider Profile) with a nullable
`session_config_json`, `last_success_at_ms`, `last_attempt_at_ms`, and
`last_error_code`. Successful Agent probes persist only modes, reasoning
controls, and generic session options. Model ids must be empty because this
fallback feeds Provider Profile sources, whose selectable model ids come from
Provider configuration. Agent-account model evidence belongs only in
`agent_auth_model_catalog_snapshots` under its context revision and runtime
fingerprint. A successful row is reused without a process launch until the
Agent is removed. Removing an Agent deletes the row so a later re-add can probe
again. A failed first attempt records its timestamp and stable error code;
ordinary reads never retry it, while the desktop startup bootstrap may retry
enabled, installed Agents without a successful row.

`provider_model_runtime_option_snapshots` is schema migration 42 and stores
product-safe ACP session configuration for one `(provider_profile_id, model_id)`
pair. Successful rows contain the model's modes, reasoning efforts, and generic
options in `session_config_json`; the target model is projected into the probe
process before discovery. A successful row is reused while the model id remains
configured, including unrelated Profile edits. Missing or failed model rows may
be retried by the background bootstrap; failures preserve a previous success
and otherwise record only the attempt timestamp and stable error code. Removing
a model or deleting its Profile removes the corresponding rows. Empty modes,
reasoning efforts, or generic options in a successful payload are meaningful
negative capability evidence and must not be replaced by Agent fallback values.

`provider_runtime_option_snapshots` is retained only as a migration-compatible
legacy table. Current catalog code must not read, write, invalidate, or key new
records by `provider_profile_id`.

`agent_auth_catalog_snapshots` stores the product-safe ACP authentication catalog
for one `(agent_id, provider_profile_id)` scope. The first successful discovery
persists the snapshot; ordinary Management Center reads reuse it without launching
an ACP process. Only an explicit user refresh or an authentication/logout action
may query again and replace the snapshot. Failed discovery must not overwrite a
previous success. Removing an Agent deletes all of its authentication snapshots so
a later re-add performs fresh discovery.

`agent_managed_installations` is schema migration 41. It stores the Registry
Agent id, serialized managed install state, the exact launch command/arguments,
the published install root, and an update timestamp. The row is not a second
Agent enablement authority: Config Center owns `agent_configs.added/enabled`,
while the installation service owns command/file lifecycle and removes the
row after a successful uninstall. Pending rows are inspected on startup using
the install root and executable files; a version string alone is insufficient
evidence of a usable installation. The row must never contain credentials or
native Agent configuration contents.

Runtime-option payload schema v1 stores the camelCase JSON form of
`AgentSessionConfigProbe` with an empty `models` array. Additive fields require
serde defaults for old rows; incompatible payload changes require a new
migration and an explicit payload-version discriminator before writing the new
shape.

## Anti-Patterns

- Do not use provider-native ids as primary keys for user-facing records.
- Do not write secrets into `provider_injection_plans` or audit logs.
- Do not let UI caches become the only copy of timeline or permission state.
- Do not assume Relay storage can recover local session history.

## Scenario: Phase 9 Backup Restore And Migration Safety

### 1. Scope / Trigger

- Trigger: Phase 9 adds release-readiness backup/restore evidence for the
  local-first SQLite database.
- The backup layer owns manifest validation, migration compatibility reporting,
  and restore safety checks. `crates/db` continues to own schema migrations and
  repository behavior.

### 2. Signatures

Backup service crate:

```text
create_backup(BackupCreateRequest { source_db_path, backup_dir }) -> BackupCreateResult
inspect_backup(backup_dir) -> BackupInspection
restore_backup(BackupRestoreRequest { backup_dir, target_db_path }) -> BackupRestoreResult
classify_migration_compatibility(source_schema_version) -> MigrationCompatibility
```

Deterministic smoke:

```text
pnpm smoke:backup
```

### 3. Contracts

- The first backup format is a directory with `manifest.json` and
  `data/vibex.db`.
- Backup creation must use a consistent SQLite copy operation, not ad hoc
  copying of only the main database file while WAL may be active.
- The manifest must record source schema version, expected current schema
  version, artifact size/checksum, migration compatibility, and excluded
  sensitive-content categories.
- Manifest artifact paths must be relative normalized paths. Restore rejects
  absolute paths, `..`, duplicate paths, and unexpected artifact names.
- Restore must inspect and classify the backup before mutating a target.
- Restore must reject newer-than-current schema backups before creating the
  target database.
- The MVP restore path writes only to an explicit nonexistent/disposable target
  path. It must not silently overwrite the default `~/.vibex/vibex.db`.
- Smoke output is bounded JSON and must not include prompt text, Agent message
  bodies, terminal output, file contents, secrets, env values, raw provider
  payloads, raw Git diffs, raw logs, or plaintext device key/auth material.

### 4. Validation & Error Matrix

- Missing source DB -> `validation/backup_source_database_missing`.
- Existing backup artifact target -> `conflict/backup_target_exists`.
- Unsupported manifest version -> `validation/backup_manifest_version_unsupported`.
- Unsafe artifact path -> `validation/backup_artifact_path_unsafe`.
- Manifest database artifact missing -> `validation/backup_database_artifact_missing`.
- Manifest checksum/size/schema mismatch -> `validation/backup_database_*_mismatch`.
- Newer schema backup -> `validation/backup_restore_newer_schema_unsupported`.
- Existing restore target or sidecar -> `conflict/backup_restore_target_exists`.

### 5. Good/Base/Bad Cases

- Good: a disposable DB smoke creates a source DB, backs it up, inspects the
  manifest, restores to a separate target DB, verifies schema version, and
  checks a sentinel row round-tripped.
- Base: a backup with source schema equal to `CURRENT_SCHEMA_VERSION` reports
  `ready`.
- Base: a backup with older positive source schema reports
  `migration_required` and may run migrations after copying to the explicit
  target.
- Bad: restore overwrites the user's default database without a separate
  deliberate user action.
- Bad: manifest or smoke evidence includes raw prompts, terminal output,
  provider-native payloads, auth tokens, private keys, or raw logs.

### 6. Tests Required

- Unit tests for manifest shape and required sensitive-content exclusions.
- Unit tests for migration compatibility classification.
- Unit tests for unsafe artifact path rejection.
- Unit tests for manifest/database schema mismatch.
- Unit tests for newer schema rejection without target mutation.
- Unit tests for existing target rejection.
- Deterministic backup/restore round-trip smoke using only disposable
  `target/stage0` paths.

## Scenario: Phase 8 Scheduled Task Contract Storage

### 1. Scope / Trigger

- Trigger: Phase 8 adds provider-neutral scheduled/background Agent task
  persistence before scheduler runtime, desktop UI, or real provider execution.
- The storage layer owns durable task lifecycle, next-run metadata, and run
  history. Runtime/UI layers must call repository helpers instead of mutating
  scheduled-task rows directly.

### 2. Signatures

Core DTOs in `crates/core`:

```text
ScheduledTaskId -> "scheduled_task_<uuid>"
ScheduledTaskRunId -> "scheduled_run_<uuid>"
ScheduledTaskSchedule -> { type: one_shot|interval|daily, data: ... }
ScheduledTaskCreateRequest -> ScheduledTask
ScheduledTaskUpdateRequest -> ScheduledTask
ScheduledTaskRunCreateRequest -> ScheduledTaskRun
ScheduledTaskRunUpdateRequest -> ScheduledTaskRun
ScheduledTaskRunListRequest -> Vec<ScheduledTaskRun>
```

SQLite schema in `crates/db`:

```text
scheduled_tasks(
  scheduled_task_id, title, prompt, project_id, workspace_id, workspace_root,
  workspace_mode, provider_kind, provider_profile_id, schedule_json, status,
  safety_json, next_run_at_ms, created_at_ms, updated_at_ms, deleted_at_ms
)

scheduled_task_runs(
  scheduled_task_run_id, scheduled_task_id, status, trigger, session_id,
  due_at_ms, started_at_ms, ended_at_ms, attempt, error_code, error_message,
  redacted_diagnostics_json, created_at_ms, updated_at_ms
)
```

Repository surface:

```text
ScheduledTaskRepository::create/get/list/update/pause/resume/soft_delete
ScheduledTaskRepository::create_run/update_run/list_runs
```

### 3. Contracts

- Schedules are intentionally limited to one-shot `runAtMs`, interval
  `everySeconds/startAtMs/endAtMs?`, and daily
  `localTimeMinutes/timezone/startAtMs/endAtMs?`.
- `schedule_json`, `safety_json`, and `redacted_diagnostics_json` must be JSON
  serialized from typed `crates/core` contracts only.
- Scheduled task contracts must not store provider-native task ids, native
  thread ids, resume tokens, raw provider payloads, secrets, or unredacted
  diagnostics.
- Run records may reference a Vibex `session_id` after a scheduler creates an
  Agent session, but the scheduled-task contract remains provider-neutral.
- Repository methods own timestamps, default safety, lifecycle status changes,
  soft-delete semantics, and bounded failure diagnostics.
- Cross-layer update requests must use explicit clear flags such as
  `clearNextRunAtMs` or `clearErrorCode` instead of nested nullable values,
  because generated TypeScript cannot reliably distinguish Rust
  `Option<Option<T>>` from a single nullable field.

### 4. Validation & Error Matrix

- Missing task id in repository update/delete -> `storage/scheduled_task_not_found`.
- Missing run id in repository update -> `storage/scheduled_task_run_not_found`.
- Invalid stored id, enum, or typed JSON -> `storage/*_decode_failed`.
- SQLite insert/update/list failure -> stable `storage/scheduled_task_*` or
  `storage/scheduled_task_run_*` code with redacted diagnostics.
- Oversized run error code/message/diagnostic strings -> repository truncates
  before storage.
- Runtime due-task claiming, prompt execution, notification emission, and audit
  fan-out are out of scope for this storage contract.

### 5. Good/Base/Bad Cases

- Good: UI creates a daily task through generated protocol types; repository
  stores typed schedule/safety JSON and later returns the same shape.
- Base: scheduler runtime creates a run with `trigger=scheduler`, updates it to
  `succeeded`, and links the created Vibex Agent `session_id`.
- Base: failed run stores bounded `error_code`, `error_message`, and
  `RedactedDiagnostic` entries only.
- Bad: runtime writes `scheduled_tasks.status` directly, bypassing repository
  timestamp and soft-delete behavior.
- Bad: a scheduled task stores a Codex thread id, Claude resume token, raw ACP
  payload, terminal output, or provider auth material.

### 6. Tests Required

- Core serde tests assert schedule `type/data` shape, camelCase struct fields,
  snake_case enum variants, and string id serialization.
- Core serde tests fail on protocol shape drift.
- Migration test applies schema version containing `scheduled_tasks` and
  `scheduled_task_runs`.
- Repository lifecycle test covers create/get/list/update/pause/resume and
  soft-delete exclusion/inclusion.
- Run history test covers create/update/list, optional session id, bounded
  diagnostics, and clearing error fields through explicit clear flags.

### 7. Wrong vs Correct

#### Wrong

```rust
pub struct ScheduledTaskRunUpdateRequest {
    pub error_code: Option<Option<String>>,
}
```

This collapses to ambiguous generated TypeScript such as
`string | null | null`; callers cannot reliably express "leave unchanged" vs
"clear this field".

#### Correct

```rust
pub struct ScheduledTaskRunUpdateRequest {
    pub error_code: Option<String>,
    pub clear_error_code: bool,
}
```

Generated TypeScript exposes both `errorCode` and `clearErrorCode`, so callers
can distinguish setting a value, clearing a value, and leaving the value
unchanged.

## Scenario: Phase 10 Automation Graph Contract Storage

### 1. Scope / Trigger

- Trigger: Phase 10 adds provider-neutral advanced automation graph definition
  and run-state persistence before runtime execution, desktop builder UI, or
  real provider smoke work.
- The storage layer owns durable graph definitions, graph versioning, node/edge
  replacement, run history, and run-step state. Runtime/UI layers must call
  `AutomationGraphRepository` instead of mutating rows directly.

### 2. Signatures

Core DTOs in `crates/core`:

```text
AutomationGraphId -> "automation_graph_<uuid>"
AutomationNodeId -> "automation_node_<uuid>"
AutomationEdgeId -> "automation_edge_<uuid>"
AutomationRunId -> "automation_run_<uuid>"
AutomationRunStepId -> "automation_step_<uuid>"
AutomationGraphTrigger -> manual | scheduled_task
AutomationNodeConfig -> agent_prompt | approval_gate | file_check | git_check | terminal_check
AutomationRunStatus -> queued | running | waiting_for_approval | succeeded | failed | canceled | recovered
AutomationRunStepStatus -> queued | running | waiting_for_approval | succeeded | failed | skipped | canceled
```

SQLite schema in `crates/db` schema version 13:

```text
automation_graphs(
  automation_graph_id, title, description, project_id, workspace_id,
  workspace_root, workspace_mode, provider_kind, provider_profile_id,
  trigger_json, status, version, created_at_ms, updated_at_ms, deleted_at_ms
)

automation_graph_nodes(
  automation_node_id, automation_graph_id, kind, title, config_json,
  position_json, created_at_ms, updated_at_ms
)

automation_graph_edges(
  automation_edge_id, automation_graph_id, source_node_id, target_node_id,
  condition_json, created_at_ms, updated_at_ms
)

automation_graph_runs(
  automation_run_id, automation_graph_id, status, trigger, scheduled_task_id,
  session_id, started_at_ms, ended_at_ms, error_code, error_message,
  redacted_diagnostics_json, created_at_ms, updated_at_ms
)

automation_graph_run_steps(
  automation_run_step_id, automation_run_id, automation_node_id, status,
  session_id, permission_request_id, started_at_ms, ended_at_ms, error_code,
  error_message, redacted_diagnostics_json, created_at_ms, updated_at_ms
)
```

Repository surface:

```text
AutomationGraphRepository::create/get/list/update/soft_delete
AutomationGraphRepository::replace_definition
AutomationGraphRepository::create_run/update_run/list_runs
AutomationGraphRepository::create_run_step/update_run_step/list_run_steps
```

### 3. Contracts

- Automation graph contracts are local-first and provider-neutral. Store Vibex
  ids and typed JSON only; do not store provider-native thread ids, resume
  tokens, raw provider payloads, secrets, terminal output, raw Git diffs, file
  contents, env values, or unredacted diagnostics.
- `trigger_json`, `config_json`, `condition_json`, `position_json`, and
  `redacted_diagnostics_json` must serialize typed `crates/core` contracts,
  never UI-local objects or provider-native payloads.
- Graph `version` starts at `1` and increments when graph metadata changes,
  when definitions are replaced, and when a graph is soft-deleted.
- `replace_definition` is the only supported node/edge replacement path. It
  must run in one transaction, delete old edges before old nodes, validate that
  every new edge references a node in the replacement set, then insert the new
  nodes and edges.
- `AutomationNodeCreateRequest.id` is optional. Callers that create edges in
  the same request must provide provider-neutral node ids up front; otherwise
  the repository generates node ids and no same-request edge can reference them.
- `permission_request_id` in run steps is a bounded reference only, not a hard
  schema dependency. Do not add a SQLite foreign key that makes recording a
  future or externally-owned permission reference fail.
- Run and run-step error code/message/diagnostic values must be bounded before
  storage and use `RedactedDiagnostic` only.
- Cross-layer update requests must use explicit clear flags such as
  `clearErrorCode`, `clearSessionId`, or `clearPermissionRequestId` instead of
  nested nullable values.

### 4. Validation & Error Matrix

- Missing graph id in update/delete/replace -> `storage/automation_graph_not_found`.
- Missing run id in update -> `storage/automation_run_not_found`.
- Missing run-step id in update -> `storage/automation_run_step_not_found`.
- Edge source node is absent from replacement set ->
  `validation/automation_graph_edge_source_missing`.
- Edge target node is absent from replacement set ->
  `validation/automation_graph_edge_target_missing`.
- Invalid stored id, enum, or typed JSON -> `storage/*_decode_failed`.
- SQLite insert/update/list failure -> stable `storage/automation_*` code with
  redacted diagnostics.
- Oversized run or run-step diagnostics -> repository truncates before storage.
- Runtime execution, scheduled claiming, permission fan-out, Tauri commands,
  desktop graph builder, and provider startup are out of scope for this storage
  contract.

### 5. Good/Base/Bad Cases

- Good: UI or runtime creates a manual graph with typed agent-prompt and
  approval-gate nodes, provides node ids for same-request edges, and repository
  returns the graph with nodes/edges round-tripped from SQLite.
- Base: runtime creates a graph run, updates it to `succeeded`, then records a
  run step linked by `automation_node_id` and optional Vibex `session_id`.
- Base: a waiting-for-approval run step records only a `permission_request_id`
  reference plus bounded redacted diagnostics.
- Bad: runtime edits `automation_graph_nodes` directly and forgets to increment
  graph `version`.
- Bad: node config stores a Codex thread id, Claude resume token, ACP raw tool
  input, terminal output, raw Git diff, file contents, or auth material.

### 6. Tests Required

- Core serde tests assert graph trigger tagged shape, node config tagged shape,
  camelCase struct fields, snake_case enum variants, and string id
  serialization.
- Core serde tests fail on protocol shape drift.
- Migration test applies schema version 13 and checks graph, node, edge, run,
  and run-step tables exist.
- Repository lifecycle test covers create/get/list/update/soft-delete and
  deleted exclusion/inclusion.
- Definition replacement test covers atomic node/edge replacement and edge
  source/target validation.
- Run and run-step history tests cover create/update/list, optional session and
  permission references, bounded diagnostics, and clearing nullable fields
  through explicit clear flags.

### 7. Wrong vs Correct

#### Wrong

```text
CREATE TABLE automation_graph_run_steps(
  permission_request_id TEXT REFERENCES permission_requests(request_id)
)
```

This makes a bounded reference fail if the runtime records a permission handle
before the owning permission row exists or if a later child stores permission
state in a different table.

#### Correct

```text
CREATE TABLE automation_graph_run_steps(
  permission_request_id TEXT NULL
)
CREATE INDEX idx_automation_graph_run_steps_permission
  ON automation_graph_run_steps(permission_request_id)
```

The repository can list and correlate by permission id without turning an
optional runtime reference into a hard storage dependency.

## Scenario: Phase 10 Automation Graph Builder And Review UI Boundary

### 1. Scope / Trigger

- Trigger: Phase 10 exposes automation graph definitions through the desktop
  workbench after the storage contract exists, but before runtime execution.
- This is a cross-layer contract because core DTOs, Tauri command signatures,
  shared Rust DTOs, browser mocks, React Query hooks, and
  workbench tab state must agree on the same graph definition lifecycle.
- The surface is definition-only. Runtime execution, graph run creation,
  scheduler claiming, provider startup, permission fan-out, and remote/mobile
  graph controls stay in later children.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
automation_graph_list(AutomationGraphListRequest) -> Vec<AutomationGraph>
automation_graph_create(AutomationGraphCreateRequest) -> AutomationGraph
automation_graph_update(AutomationGraphUpdateRequest) -> AutomationGraph
automation_graph_replace_definition(AutomationGraphDefinitionUpdateRequest) -> AutomationGraph
automation_graph_pause(AutomationGraphId) -> AutomationGraph
automation_graph_resume(AutomationGraphId) -> AutomationGraph
automation_graph_archive(AutomationGraphId) -> AutomationGraph
```

Desktop frontend modules:

```text
apps/desktop/src/lib/tauri.ts
apps/desktop/src/features/automation/useAutomationGraphQueries.ts
apps/desktop/src/features/automation/automationGraphDraft.ts
apps/desktop/src/features/automation/AutomationGraphsPanel.tsx
apps/desktop/src/features/workspace/WorkspaceShell.tsx
```

Workbench tab contract:

```text
WorkbenchTabKind::Automation -> "automation"
```

### 3. Contracts

- Tauri command handlers are thin: open the migrated local database, delegate
  to `AutomationGraphRepository`, and return generated `vibex_core` DTOs.
- `automation_graph_replace_definition` must call
  `AutomationGraphRepository::replace_definition` with a mutable connection so
  node/edge replacement stays atomic and versioned by the repository.
- Frontend code consumes automation graph request/response types through the
  shared Rust Backend contracts; UI code may use local draft view models, but
  must convert them into canonical request types before calling the Backend.
- Browser mock mode must exercise the same lifecycle contract as native Tauri:
  list, create, update, replace definition, pause, resume, and soft archive.
- Draft node ids used by edges must be explicit `AutomationNodeId` strings.
  Same-request edges cannot rely on repository-generated node ids.
- The builder may show and edit prompt/approval text that the user typed, but
  validation summaries and diagnostics must not copy secrets, env values,
  terminal output, raw Git diffs, file contents, provider-native payloads, or
  native provider ids.
- New/create draft state must be distinct from "initial selection not loaded".
  Do not fall back to the first list item after the user explicitly chooses New
  or Duplicate, or the draft can be overwritten by the selected graph.

### 4. Validation & Error Matrix

- Missing workspace selection -> save action disabled or local UI no-op; no
  partially scoped graph create request.
- Missing title -> local validation blocks save before persistence.
- Empty node list -> local validation blocks save before persistence.
- Duplicate draft node id -> local validation blocks save before persistence.
- Edge source/target missing from draft nodes -> local validation blocks save;
  repository remains authoritative and returns
  `validation/automation_graph_edge_source_missing` or
  `validation/automation_graph_edge_target_missing`.
- Unsupported node config fields -> form does not expose them; loaded
  unsupported node kinds render review-only until runtime support exists.
- Archive -> soft delete through repository, return status `deleted` with
  `deletedAtMs`; normal list excludes it unless `includeDeleted` is true.
- Browser mock invocation -> must not start real Claude, Codex, OpenCode, ACP,
  provider-native schedulers, public Relay, or hosted services.

### 5. Good/Base/Bad Cases

- Good: The Automation tab lists active graphs for the selected workspace,
  creates a manual graph with explicit agent-prompt and approval-gate node ids,
  replaces definitions through typed mutations, and invalidates graph queries.
- Base: Browser mock mode renders a seeded manual graph and mutates in-memory
  graph records with the same generated DTO shape as native Tauri.
- Base: Duplicate creates a new draft with fresh node ids and remapped edges
  without selecting or modifying the original graph.
- Bad: UI writes directly to automation graph tables, manually increments
  `version`, or updates node/edge rows outside `replace_definition`.
- Bad: The builder adds a Run button, creates `automation_graph_runs`, starts a
  provider process, or treats a local validation warning as runtime approval.
- Bad: New draft state reuses `null` for both "no selection yet" and "user is
  creating"; the auto-select effect overwrites the draft with the first graph.

### 6. Tests Required

- `cargo fmt --package vibex-core --package vibex-desktop -- --check`.
- `cargo check -p vibex-desktop` after command signature or tab changes.
- `cargo test -p vibex-core -p vibex-db -p vibex-desktop` when core DTO,
  repository, or command plumbing changes.
- Run core protocol tests after adding or changing serialized variants.
- `pnpm check:frontend` and `pnpm --filter @vibex/desktop build:frontend` for
  UI/hook/mock changes.
- Draft validation and draft-to-request conversion tests should be added when
  frontend test tooling exists; until then, keep the conversion layer small and
  covered by TypeScript checks.
- Browser screenshot of the Automation tab in mock mode when local rendering is
  available, plus console review for first-party runtime errors.

### 7. Wrong vs Correct

#### Wrong

```typescript
const [selectedGraphId, setSelectedGraphId] = useState<string | null>(null);
const selectedGraph = graphs.find((graph) => graph.id === selectedGraphId) ?? graphs[0] ?? null;
```

This treats an explicit New/Duplicate draft as "select the first graph", so
the graph-to-draft effect can overwrite user edits before save.

#### Correct

```typescript
const [selectedGraphId, setSelectedGraphId] = useState<string | null | undefined>(undefined);
const selectedGraph = selectedGraphId ? graphs.find((graph) => graph.id === selectedGraphId) ?? null : null;
```

`undefined` means initial list load may auto-select the first graph. `null`
means the user is intentionally editing a new draft and no list item should
replace it.

## Scenario: Phase 8 Desktop Scheduled Tasks UI Boundary

### 1. Scope / Trigger

- Trigger: Phase 8 exposes scheduled task storage and run history through the
  desktop workbench.
- This is a cross-layer contract because Rust DTOs, Tauri command signatures,
  shared Rust DTOs, browser mocks, React Query hooks, and workbench
  tab state must all use the same scheduled task shapes.
- The UI is a desktop control surface only. Scheduler runtime, notification
  badges, audit fan-out, permission policy review, and real provider smoke stay
  in their own Phase 8 children.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
scheduled_task_list(ScheduledTaskListRequest) -> Vec<ScheduledTask>
scheduled_task_create(ScheduledTaskCreateRequest) -> ScheduledTask
scheduled_task_update(ScheduledTaskUpdateRequest) -> ScheduledTask
scheduled_task_pause(ScheduledTaskId) -> ScheduledTask
scheduled_task_resume(ScheduledTaskId) -> ScheduledTask
scheduled_task_delete(ScheduledTaskId) -> ScheduledTask
scheduled_task_list_runs(ScheduledTaskRunListRequest) -> Vec<ScheduledTaskRun>
```

Desktop frontend modules:

```text
apps/desktop/src/lib/tauri.ts
apps/desktop/src/features/scheduled/useScheduledTaskQueries.ts
apps/desktop/src/features/scheduled/ScheduledTasksPanel.tsx
apps/desktop/src/features/workspace/WorkspaceShell.tsx
```

Workbench tab contract:

```text
WorkbenchTabKind::Scheduled -> "scheduled"
```

### 3. Contracts

- Tauri command handlers are thin: open the migrated local database, delegate
  to `ScheduledTaskRepository`, and return generated `vibex_core` DTOs.
- Frontend code consumes scheduled task types through the shared Rust Backend
  contracts; UI code must not redefine request/response transport shapes.
- Browser mock mode must exercise the same lifecycle contract as native Tauri:
  list, create, update, pause, resume, soft delete, and run-history filtering.
- The desktop create form binds tasks to the selected workspace root/mode/id,
  selected provider kind/profile, generated schedule union, and default safety
  when no explicit safety override is provided.
- The first desktop create UI may support one-shot and interval schedules while
  still displaying existing daily schedule records.
- Run history displays only bounded `errorCode`, `errorMessage`, and
  `redactedDiagnostics`. It must not render provider-native payloads, native
  ids, tokens, env values, or raw adapter logs.
- Opening a scheduled run session updates workbench UI state with the Vibex
  `sessionId` and switches to the Agent tab. It must not use native provider ids
  as navigation targets.

### 4. Validation & Error Matrix

- Missing workspace selection -> create action disabled or local UI no-op; no
  partially bound scheduled task request.
- Missing task id in lifecycle command -> repository returns
  `storage/scheduled_task_not_found`.
- Deleted task -> returned as status `deleted` with `deletedAtMs`; normal list
  excludes it unless `includeDeleted` is true.
- Run without `sessionId` -> render history details but disable Agent-session
  jump.
- Unsupported provider profile selection -> backend/runtime capability checks
  remain authoritative; UI selection is advisory.
- Browser mock invocation -> must not start real Claude, Codex, OpenCode, ACP,
  or provider-native schedulers.

### 5. Good/Base/Bad Cases

- Good: the Scheduled tab lists active and paused tasks, shows next/last run,
  creates a one-shot prompt for the selected workspace, pauses/resumes/deletes
  through typed mutations, and opens the Vibex Agent session created by a run.
- Base: browser mock mode renders seeded succeeded/failed runs and mutates
  in-memory scheduled tasks without Tauri internals.
- Base: existing daily scheduled task records display a readable daily summary
  even when the first create form only supports one-shot and interval.
- Bad: UI branches on `codexThreadId`, `claudeSessionId`, ACP native payloads,
  or provider-native task scheduler state.
- Bad: UI mutates task status locally without invalidating scheduled task and
  run-history queries.

### 6. Tests Required

- `cargo fmt --package vibex-core --package vibex-desktop -- --check`.
- `cargo check -p vibex-desktop`.
- `cargo test -p vibex-core -p vibex-desktop` when core tab types or Tauri
  command signatures change.
- Run core protocol tests after adding or changing serialized variants.
- `pnpm check:frontend` and `pnpm --filter @vibex/desktop build` for UI work.
- Browser screenshot of the Scheduled tab in mock mode when local rendering is
  available, plus console review for first-party runtime errors.

### 7. Wrong vs Correct

#### Wrong

```typescript
// UI invents a local lifecycle shape and updates state without the repository.
setTasks(tasks.map((task) => task.id === id ? { ...task, paused: true } : task));
```

This diverges from the repository-owned `status` enum, misses timestamps, and
does not match native Tauri behavior.

#### Correct

```typescript
await api.scheduledTaskPause(task.id);
await queryClient.invalidateQueries({ queryKey: ["scheduled", "tasks", workspaceId] });
await queryClient.invalidateQueries({ queryKey: ["scheduled", "runs", task.id] });
```

Lifecycle transitions go through the typed command/repository boundary, and the
UI refreshes authoritative scheduled task and run-history state.

## Scenario: Phase 8 Scheduled Task Attention And Audit Projection

### 1. Scope / Trigger

- Trigger: Phase 8 adds local visibility and accountability for scheduled task
  runs without changing scheduler execution or adding OS notifications.
- This is a cross-layer contract because core DTOs, DB repository projections,
  Tauri commands, shared Rust DTOs, React Query hooks, browser
  mocks, and workbench badges must agree on the same bounded evidence shape.

### 2. Signatures

Core DTOs in `crates/core`:

```text
ScheduledTaskAttentionListRequest -> { workspace_id?, limit? }
ScheduledTaskAttentionSummary -> task id/title, run id, workspace, provider,
  trigger, status, attention kind, session id?, error code/message?, created_at_ms
ScheduledTaskAuditListRequest -> { workspace_id?, status?, limit? }
ScheduledTaskAuditRecord -> audit id, task id/title, run id, workspace,
  provider, trigger, outcome, status, session id?, bounded error/diagnostics,
  created_at_ms
```

Repository and desktop commands:

```text
ScheduledTaskRepository::list_attention(...)
ScheduledTaskRepository::list_audit(...)
scheduled_task_list_attention(...) -> Vec<ScheduledTaskAttentionSummary>
scheduled_task_list_audit(...) -> Vec<ScheduledTaskAuditRecord>
```

### 3. Contracts

- Run history remains authoritative. Attention and audit rows are projections
  from `scheduled_task_runs JOIN scheduled_tasks`, not a second source of truth.
- The first pass does not add a persistent scheduled audit table because
  `ScheduledTaskRun` already contains the required bounded evidence.
- Projection queries may read task title, workspace id/root, provider kind,
  provider profile id, run id/status/trigger/session id, bounded error fields,
  redacted diagnostics, and timestamps.
- Projection queries must not read or return `scheduled_tasks.prompt`,
  provider-native ids, native provider payloads, terminal output, env values,
  file contents, tokens, raw Git diffs, or unbounded diagnostics.
- `scheduler/permission_required` maps to
  `attentionKind=permission_required` and
  `outcome=permission_required`; the UI should label it as needing user review,
  not as an auto-approved run.
- `scheduler/recovered_stale_run` maps to recovered stale-run attention/audit
  outcome so restart recovery is visible.

### 4. Validation & Error Matrix

- Missing workspace filter -> list across workspaces, still bounded by limit.
- Stored enum/id/JSON decode failure -> `storage/scheduled_task_*_decode_failed`.
- Permission-required scheduled run -> skipped/needs-attention projection; no
  scheduled path may silently approve the provider or Vibex permission request.
- Browser mock mode -> must generate the same projection shapes without
  starting real Claude, Codex, OpenCode, ACP, or provider-native schedulers.

### 5. Good/Base/Bad Cases

- Good: Workbench Scheduled tab shows an attention count before the user opens
  per-task run history, and Scheduled Tasks panel distinguishes
  permission-required from generic failure.
- Base: Audit projection includes a succeeded run and a skipped
  permission-required run for the same task, both referencing run ids.
- Bad: An audit DTO includes the scheduled prompt body, native Codex/Claude
  ids, ACP raw payloads, terminal output, token/env values, or copied provider
  logs.

### 6. Tests Required

- Core serialization test for audit DTO camelCase shape and absence of prompt.
- DB test for attention filtering, permission-required classification, bounded
  diagnostic projection, and workspace/status filters.
- Run core protocol tests after DTO changes.
- `cargo check -p vibex-desktop` after command signature changes.
- `pnpm check:frontend` and desktop build after UI/hook changes.
- Browser screenshot for Scheduled tab attention and audit projection when UI
  changes can render locally.

### 7. Wrong vs Correct

#### Wrong

```rust
SELECT t.prompt, r.redacted_diagnostics_json
FROM scheduled_task_runs r
JOIN scheduled_tasks t ON t.scheduled_task_id = r.scheduled_task_id
```

This leaks scheduled prompt text into an audit surface that should only contain
bounded evidence and identifiers.

#### Correct

```rust
SELECT t.scheduled_task_id, t.title, t.workspace_id, t.workspace_root,
       t.provider_kind, t.provider_profile_id,
       r.scheduled_task_run_id, r.status, r.trigger,
       r.error_code, r.error_message, r.redacted_diagnostics_json,
       r.created_at_ms
FROM scheduled_task_runs r
JOIN scheduled_tasks t ON t.scheduled_task_id = r.scheduled_task_id
```

The projection keeps run history authoritative and references the run id
without copying prompt/provider-native payloads into the attention or audit
surface.

## Scenario: Provider Projection Compatibility Initialization

### 1. Scope / Trigger

- Trigger: opening or migrating a database that contains legacy
  `provider_profiles`, including a fresh database whose local defaults have not
  been seeded yet.
- The v37 projection backfill opens its own transaction, so its initialization
  boundary must remain outside repository reads and caller-owned transactions.

### 2. Signatures

```rust
CURRENT_SCHEMA_VERSION = 37

model_provider_profiles
agent_runtime_profiles
agent_model_provider_bindings_v2
agent_configured_model_bindings

apply_migrations(&mut Connection) -> VibexResult<Vec<String>>
ProviderProfileRepository::ensure_local_defaults(&Connection) -> VibexResult<()>
ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&Connection)
    -> VibexResult<usize>
ProviderProjectionCompatibilityRepository::sync_legacy_profile(
    &Connection,
    &ProviderProfile,
) -> VibexResult<LegacyProviderProjectionRecords>
```

### 3. Contracts

- Migration 37 is additive. It preserves legacy Profile ids in nullable unique
  `legacy_provider_profile_id` columns and never rewrites session/default/
  failover identities or touches an Agent process/home.
- `apply_migrations` completes each schema transaction, then calls
  `ensure_local_defaults`, then performs the compatibility backfill while the
  connection is in autocommit mode. Fresh and upgraded databases therefore
  converge on the same three-entity state.
- `ensure_local_defaults` only performs its `INSERT OR IGNORE` seed writes. It
  must not start projection backfill because legacy repository reads are allowed
  inside caller transactions.
- `backfill_legacy_profiles` is deterministic and idempotent. Each Profile sync
  upserts provider, runtime, binding, and configured-model rows in one
  transaction; a repeated migration returns no newly created binding.
- Legacy facade mutations explicitly call compatibility sync after a successful
  old-record write. Soft deletion marks the three compatibility records deleted
  without deleting historical selection ids.
- Backfill stores Secret references and revision/status metadata only. It never
  resolves keychain/environment values or materializes overlay files.

### 4. Validation & Error Matrix

- Cannot begin/commit compatibility transaction ->
  `provider_projection_backfill_transaction_failed` /
  `provider_projection_backfill_commit_failed`.
- Legacy row or JSON cannot decode ->
  `provider_projection_legacy_decode_failed`; do not publish partial records.
- Missing legacy identity on a compatibility record ->
  `provider_projection_legacy_identity_missing`.
- Entity revision is non-positive or stale -> matching
  `*_revision_invalid` / `*_revision_conflict`.
- `ProviderProfileRepository::get` inside an existing transaction -> succeeds
  without attempting a nested compatibility transaction.

### 5. Good/Base/Bad Cases

- Good: a v36 database with configured Profiles migrates once; a second
  `apply_migrations` preserves exactly one provider/runtime/binding set per
  legacy id.
- Good: a fresh database seeds all local defaults and projects them during the
  same initialization call.
- Base: a catalog snapshot CAS transaction reads a legacy Profile and performs
  only its own snapshot writes.
- Bad: call `backfill_legacy_profiles` from `ensure_local_defaults`; any
  repository read inside a SQLite transaction can then fail with "cannot start
  a transaction within a transaction".
- Bad: swallow the nested-transaction error and continue with partially missing
  compatibility rows.

### 6. Tests Required

- `fresh_migration_seeds_and_projects_local_defaults` asserts every seeded
  Profile has all three compatibility entities.
- `migration_37_backfills_v36_profiles_idempotently` asserts one v37 migration,
  preserved Secret references, and zero new rows on repeated backfill.
- `legacy_repository_reads_are_safe_inside_a_transaction` opens a transaction,
  calls the legacy repository, and commits successfully.
- Desktop Runtime catalog tests assert success/failure snapshot CAS paths do not
  attempt a nested projection transaction.
- Run the full `vibex-db` and `vibex-desktop-runtime` library suites after any
  initialization-order change.

### 7. Wrong vs Correct

#### Wrong

```rust
fn ensure_local_defaults(conn: &Connection) -> VibexResult<()> {
    seed_defaults(conn)?;
    ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(conn)?;
    Ok(())
}
```

#### Correct

```rust
fn apply_migrations(conn: &mut Connection) -> VibexResult<Vec<String>> {
    let applied = apply_schema_transactions(conn)?;
    ProviderProfileRepository::ensure_local_defaults(conn)?;
    ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(conn)?;
    Ok(applied)
}
```

The compatibility transaction begins only after migration transactions finish
and before a caller starts its own application transaction.

## Scenario: Agent Authentication Context And Runtime Source Persistence

### 1. Scope / Trigger

- Trigger: code creates or changes an Agent default account context, persists
  its model catalog, binds a runtime to `AgentAccount`, records a switch or
  usage fact for that source, or upgrades a Provider-only database.
- Schema versions 45, 46, and 47 are one compatibility chain. Version 45 adds
  source columns and backfills Provider rows; 46 rebuilds tables so legacy
  Profile columns can be null; 47 makes usage model ids nullable for semantic
  `AgentDefault`.

### 2. Signatures

```text
CURRENT_SCHEMA_VERSION = 47

agent_auth_contexts(
  auth_context_id PRIMARY KEY,
  agent_id UNIQUE NOT NULL,
  status, account_hint_redacted?, authenticated_via_method?,
  revision CHECK(revision > 0), last_verified_at_ms?,
  created_at_ms, updated_at_ms
)

agent_authentication_operations(
  operation_id PRIMARY KEY,
  auth_context_id REFERENCES agent_auth_contexts ON DELETE CASCADE,
  expected_context_revision CHECK(expected_context_revision > 0),
  method_id, state, error_code?, created_at_ms, updated_at_ms
)

agent_auth_model_catalog_snapshots(
  auth_context_id REFERENCES agent_auth_contexts ON DELETE CASCADE,
  auth_context_revision CHECK(auth_context_revision > 0),
  runtime_fingerprint, discovery_source, status, catalog_json,
  last_success_at_ms?, last_attempt_at_ms, last_error_code?,
  PRIMARY KEY(auth_context_id, auth_context_revision, runtime_fingerprint)
)

session_runtime_bindings(
  ..., provider_profile_id?, profile_revision?,
  auth_source_kind, auth_source_id, auth_source_revision, ...
)

runtime_switches(
  ..., target_profile_id?, target_auth_source_kind,
  target_auth_source_id, target_auth_source_revision, ...
)

AgentAuthContextRepository::{ensure_default, compare_and_set,
  referencing_session_ids}
AgentAuthenticationOperationRepository::{insert, update_state,
  cancel_incomplete_on_startup}
AgentAuthModelCatalogRepository::{upsert, get, list_current, delete_context}
```

### 3. Contracts

- `UNIQUE(agent_id)` is the durable one-account rule. `ensure_default` uses
  insert-on-conflict plus a read, so concurrent callers converge on one stable
  context id instead of creating account slots.
- Context updates are CAS-fenced by a positive revision. Credential identity
  changes increment revision and delete every snapshot for the context in the
  same repository operation; a status-only verification completion may retain
  the just-written current-revision snapshot.
- The partial unique index on active operation states permits at most one
  queued/discovering/authenticating/awaiting/verifying/cancelling operation per
  context. Startup marks every incomplete operation `cancelled` with the safe
  `application_restarted` code; it never resumes an external login call.
- Model snapshot JSON contains typed model descriptors only. Its primary key
  includes context revision and runtime fingerprint, so account and Agent
  runtime changes cannot reuse stale entitlement evidence.
- Version 45 backfills every legacy binding, switch, checkpoint, and usage fact
  as `provider_profile` with the existing Profile id. It does not create a
  synthetic Profile or rewrite selection/timeline JSON to an account source.
- Version 46 rebuilds `session_runtime_bindings` and `runtime_switches` with
  nullable legacy Profile columns and CHECK constraints: Provider source rows
  must match those columns, while Agent-account rows must keep them null. It
  also makes usage Profile columns nullable and recreates all required indexes.
- Version 47 rebuilds usage tables so `last_model_id`/`model_id` may be null
  only for `agent_account`. Any old internal `agent_default` sentinel is
  converted to null during copy. Provider facts still require a concrete model.
- Table rebuilds disable foreign keys only outside the transaction, copy all
  rows, recreate tables/indexes, run `PRAGMA foreign_key_check`, record the
  migration, commit, and restore foreign-key enforcement. Failure at any stage
  must not publish a partially rebuilt schema.
- Runtime repositories decode new kind/id/revision first and cross-check legacy
  Profile columns when present. A mismatch is corruption, not a fallback hint.
  JSON selection compatibility maps old `providerProfileId/modelId` to tagged
  Provider/Explicit variants; AgentAccount has no lossy legacy encoding.
- Account hints are already redacted/bounded before persistence. No token,
  cookie, raw environment value, OAuth state, native state-home path, or raw
  ACP payload belongs in these tables.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Two context rows target one Agent | SQLite uniqueness conflict; keep the existing context. |
| Expected context revision is stale | `agent_auth_context_revision_conflict`; no status/snapshot mutation. |
| A second active operation is inserted | `agent_authentication_operation_in_progress`; no second row/process ownership. |
| Operation state CAS loses | `agent_authentication_operation_state_conflict`; preserve the winner. |
| Snapshot context/revision/fingerprint cannot decode | stable `agent_auth_model_catalog_*` storage/decode error; never substitute another snapshot. |
| Provider source disagrees with legacy Profile id/revision | runtime/usage auth-source legacy mismatch error; fail the read. |
| AgentAccount row has a non-null legacy Profile column | CHECK/migration failure; do not coerce it into Provider source. |
| Provider usage row has null model | CHECK failure; only AgentAccount may use unknown model. |
| Row count, foreign key, or index recreation check fails | roll back the rebuild and leave the prior schema authoritative. |
| Migration 46/47 is already recorded | skip idempotently; do not rebuild again. |

### 5. Good / Base / Bad Cases

- Good: two concurrent `ensure_default(codex)` calls return the same context;
  a direct second insert is rejected by `UNIQUE(agent_id)`.
- Good: a v44 Provider-only database applies 45/46/47, retains every binding,
  switch, checkpoint, and fact, and all rows decode as Provider sources.
- Good: an AgentAccount binding persists null legacy Profile fields, positive
  source revision, and an AgentDefault usage fact with null model id.
- Base: a relogin increments revision and old snapshot rows are deleted; the
  same context id remains referenced by historical bindings and sessions.
- Bad: write an empty/synthetic Profile id to satisfy old NOT NULL columns,
  store `agent_default` as a model, or ignore mismatched dual-read fields.
- Bad: turn foreign keys off inside the rebuild transaction, forget to recreate
  partial indexes, or record the migration before validation/commit.

### 6. Tests Required

- Migration tests apply v45/46/47 from fresh and v44 databases, assert ordered
  migration records, stable row counts, all indexes, CHECK constraints, and
  `PRAGMA foreign_key_check` success.
- Repository tests assert idempotent `ensure_default`, direct second-row
  rejection, context CAS success/conflict, snapshot invalidation, and startup
  cancellation of incomplete operations.
- Runtime repository tests round-trip both source variants, reject legacy/new
  mismatches, and persist null legacy Profile fields only for AgentAccount.
- Usage migration/repository tests convert the old `agent_default` sentinel to
  null, accept null model for AgentAccount, and reject it for Provider source.
- Backup/restore and smoke tests assert schema version 47 and preserve account,
  source, switch, timeline attribution, and usage rows without secret material.
- Run the full `vibex-db`, `vibex-agent`, `vibex-agent-acp`, and
  `vibex-desktop-runtime` suites after changing this chain.

### 7. Wrong vs Correct

#### Wrong

```sql
INSERT INTO provider_profiles(provider_profile_id, display_name)
VALUES ('agent_default', 'Codex subscription');

UPDATE session_runtime_bindings
SET provider_profile_id = 'agent_default';
```

#### Correct

```text
v45 add/backfill tagged source columns
  -> v46 rebuild legacy Profile columns nullable with cross-field CHECKs
  -> persist AgentAccount(id, revision) with Profile columns NULL
  -> v47 store unknown Agent-default model as NULL
  -> decode tagged source and validate every legacy alias
```

# Agent Session Protocol

Vibex must present a provider-neutral Agent session protocol across native
Desktop and installed mobile clients. Every online Agent, including Claude and
Codex, executes through ACP. Native Claude/Codex transcript and parity crates
are offline compatibility inputs only.

Evidence: current Agent/runtime code, tests, and completed Agent-session Trellis tasks.

> Legacy cutover note (2026-07-29): later sections that name Tauri commands,
> React hooks, browser mocks, or files from the former desktop shell are retained
> pre-cutover evidence. That shell once occupied `apps/desktop`; its adapters no
> longer exist. Current callers use the GPUI Backend facade, `DesktopRuntime`, or
> the versioned Remote protocol.

## Scenario: Authoritative Agent Notification Intents

### 1. Scope / Trigger

- Trigger: changing Agent permission/input persistence, terminal turn state,
  notification identity, or any desktop/mobile notification presenter.
- V1 notification kinds are `approval_required`, `input_required`,
  `turn_completed`, and `turn_failed` only.

### 2. Signatures

```text
AgentNotificationIntent {
  notification_id, source_event_id, session_id, kind,
  created_at_ms, expires_at_ms, opaque_locator
}
AgentManager::subscribe_notifications()
```

### 3. Contracts

- `AgentManager` is the single semantic source. Views and clients must not infer
  a notification by inspecting rendered deltas, a timeline snapshot, or a
  refetch result.
- Publish approval/input only after the pending request and its timeline item
  are persisted. Publish completion only after a provider reports the turn
  complete and the authoritative session reaches `Idle`; publish failure only
  after the error item is persisted and the session reaches `Error`.
- Notification IDs are deterministic from immutable session/request/timeline
  identities. Repeated provider snapshots may therefore replace the same OS
  notification, while timeline replay and refetch never create a new intent.
- Approval/input intents expire after 15 minutes. Completion/failure intents
  expire after 24 hours. Presenters must reject an expired intent.
- The serialized intent contains routing identifiers only. It must not contain
  prompt or answer text, commands, paths, tool output, approval details, or
  credentials. `opaque_locator` is not authorization and must be resolved by
  the authenticated PC before notification-tap navigation.
- The current broadcast is a live, process-local stream. It is not a durable
  notification outbox and must not be described as disconnected/background
  delivery.

### 4. Tests Required

- Core tests freeze stable IDs, TTLs, serde shape, and absence of timeline text.
- Agent tests prove only pending authoritative requests emit attention intents.
- Turn lifecycle coverage must keep completion/failure emission after the
  corresponding authoritative state transition.

### 5. Wrong vs Correct

```text
Wrong: rendered final-answer delta -> mobile notification
Correct: persisted turn result + authoritative Idle transition -> one stable intent
```

## Session Identity

Persist three independent layers of identity:

- Logical Session: `AgentSession.id` is the stable id used by UI, remote
  clients, database records, links, and audit logs. `AgentSession.agent_id` is
  required; auth-source/model state is not duplicated on this DTO.
- Product selection: durable desired/effective `SessionRuntimeSelection`
  identifies `agent_id + RuntimeAuthSource + RuntimeModelSelection + optional
  reasoning_effort/mode_id`. The source is either a Provider Profile or the
  Agent's one default authenticated account; an ACP `methodId` is never a
  selection identity.
- Execution fence: the current `RuntimeBinding` and activation generation bind
  the selection to an exact
  `AgentRuntimeRouteKey { agent_id, Acp, adapter_id }`, process instance, ACP
  native session handle, and the source revision verified for that process.

`ProviderKind` may describe configuration, import, diagnostics, or provenance;
it is never a Logical Session identity, online route key, or fallback choice.
Never expose binding ids, generations, Adapter ids, process ids, or native ids
as client routing ids. ACP diagnostics and debug logs use irreversible short
hashes and must not print a raw native session id even when a process has only
one attachment.

## Scenario: Runtime Option Catalog

### 1. Scope / Trigger

Session creation and in-session selectors need one provider-neutral list of
Agent/authentication-source/model combinations. Provider Profiles retain their
configured model list; an Agent default account contributes models discovered
inside its own authenticated state home. Sources remain visible even when
their model catalog is unavailable, so login and retry actions do not disappear.

### 2. Signatures

```text
agent_list_runtime_options() -> SessionRuntimeOptionCatalog
AgentAuthContextService::list() -> Vec<AgentAuthContext>
AgentAuthContextService::refresh_models(request) -> AgentAuthContextMutationResult
RuntimeOptionCatalogService::probe_agent(agent_id) -> RuntimeOptionProbeResult
RuntimeOptionCatalogService::probe_profile_models(provider_profile_id)
  -> ProviderModelRuntimeOptionProbeResult
AgentProvider::probe_agent_session_config(agent_id) -> AgentSessionConfigProbe
AgentProvider::probe_session_config_for_model(profile_id, model_id)
  -> AgentSessionConfigProbe
ProviderConfigService::get_agent_acp_runtime_config(agent_id) -> AcpProviderConfig
AcpRuntimeClient::profile_session_config_evidence()
  -> Map<ProviderProfileId, AgentSessionConfigProbe>
build_runtime_option_catalog(agents, profiles, evidence_by_profile)
  -> SessionRuntimeOptionCatalog

agent_runtime_option_snapshots(
  agent_id PRIMARY KEY,
  session_config_json,
  last_success_at_ms,
  last_attempt_at_ms,
  last_error_code
)

provider_model_runtime_option_snapshots(
  provider_profile_id,
  model_id,
  agent_id,
  session_config_json,
  last_success_at_ms,
  last_attempt_at_ms,
  last_error_code,
  PRIMARY KEY(provider_profile_id, model_id)
)

agent_auth_model_catalog_snapshots(
  auth_context_id, auth_context_revision, runtime_fingerprint,
  discovery_source, status, catalog_json, last_success_at_ms,
  last_attempt_at_ms, last_error_code,
  PRIMARY KEY(auth_context_id, auth_context_revision, runtime_fingerprint)
)
```

### 3. Contracts

- Store at most one successful fallback runtime-option snapshot per `agent_id`
  for Provider Profile projection. It contains modes, reasoning controls, and
  generic session options only; it must not be used as an Agent account model
  catalog. The old Agent fallback probe still clears reported models before
  persistence.
- Agent account model snapshots are keyed by context id, positive context
  revision, and runtime fingerprint. Their model descriptors and per-model
  reasoning/mode/features are evidence from that exact authenticated launch.
- Account discovery switches each enumerated model inside the same short-lived
  authenticated ACP session and captures the resulting reasoning, mode, and
  generic options. A direct catalog such as Codex `model/list` remains
  authoritative for model ids and reasoning levels, while model-scoped ACP
  responses supply modes and generic options. One model's current controls
  must never be copied across every model.
- A source summary is published independently from `options`. An authenticated
  Agent account with no enumerated models publishes one `AgentDefault` option,
  not a guessed model id. The serialized projection key is `agent-default`;
  it is not sent to ACP as a model override. Reasoning, mode, and generic
  controls advertised by that default session remain selectable and are fenced
  against the same snapshot as explicit-model controls.
- Persist whether an account snapshot contains complete model-scoped runtime
  options. Older snapshot JSON remains readable but defaults to incomplete;
  desktop startup refreshes an authenticated incomplete snapshot once and
  publishes the normal runtime-option invalidation. Complete current snapshots
  never launch an Agent process during ordinary startup catalog reads.
- An Agent-level probe launches the configured Agent command with its Agent
  args and Agent env. It must not list or load a Provider Profile, resolve a
  Provider secret, materialize a Provider projection, or use a Provider model.
  A synthetic `ProviderProfileId` may exist only as in-process bookkeeping.
- Trigger the Agent fallback probe when an installed Agent is added, discovered in bulk,
  detected after installation, or enabled and installed at desktop startup without a
  successful persisted snapshot. A failed first attempt may be retried by the
  next startup bootstrap or an explicit Agent action. Ordinary enable/disable
  and Provider Profile create/update/delete never trigger an Agent fallback probe.
- A successful snapshot is immutable while the Agent remains added. Later
  calls return `cached_agent_ids` without launching the CLI. Removing the
  Agent deletes its snapshot so a later re-add can probe once again.
- Saving a configured ACP Provider Profile schedules a background probe for
  each enabled model without a successful `(provider_profile_id, model_id)`
  snapshot. A successful model snapshot is reused while that model id remains
  configured, even when unrelated Profile fields change. Removing or replacing
  a model deletes its stale row; deleting a Profile deletes all model rows.
- Model probes project exactly one target model into the short-lived ACP
  process. The probe must capture the model switch response and any bounded
  `config_option_update` that the Agent emits for that switch; GLM publishes
  model-specific thought levels through the update while returning an empty
  `session/set_model` result. A failed model probe records its attempt and error
  without blocking the Provider save or removing an existing successful model
  snapshot.
- When `session/new.configOptions` advertises the model as a config option, the
  model probe uses `session/set_config_option` first because Agents such as
  OpenCode return the selected model's complete mode, effort, and generic option
  set only from that response. If the operation is rejected, the probe falls
  back to `session/set_model` and re-registers its bounded config-update waiter.
- Ordinary catalog reads load the SQLite Agent fallback, persisted Provider
  model snapshots, current in-memory Profile evidence, and current-revision
  Agent account snapshots; they never start an ACP process. Provider mutations
  trigger only the asynchronous missing-model bootstrap. An explicit Agent
  account refresh is the only account-catalog mutation path.
- A real `session/new`, `session/load`, or `session/resume` response publishes
  its safe session configuration under the exact `provider_profile_id`.
  `ConfigOptionUpdate` replaces that session's complete option set, including
  removal of withdrawn options, then refreshes the Profile evidence.
- Live Profile evidence overrides the Agent fallback only for that Profile.
  Persisted model entries remain attached to their exact models. Other
  Profiles continue to render immediately from cached evidence until one of
  their real sessions supplies newer evidence.
- Profile-wide live evidence is process-memory calibration. Model snapshots are
  durable and keyed by both Profile and model; after restart, unchanged models
  reuse them before falling back to Agent evidence.
- Enabled ACP Provider Profiles contribute only enabled configured model ids,
  or their configured default model when the explicit list is empty. Agent
  discovery and the Provider-only Agent probe never populate a Profile's
  selector. Agent account snapshots populate only their matching account
  source.
- Until live evidence exists, every enabled Profile for one Agent receives the
  same fallback modes, reasoning controls, and Features. Once calibrated, each
  Profile may expose its own current Agent-owned controls.
  `SessionRuntimeOptionCatalog.revision` is a deterministic positive revision
  of the ordered redacted projection.
- `reasoningEffort = null` and `modeId = null` mean that the Adapter's
  converged defaults remain authoritative. Runtime fence matching always
  requires the exact model and compares Effort/Mode only when explicitly set.

### 4. Validation & Error Matrix

- Agent missing, not added, or disabled -> return an empty probe result and do
  not launch a process.
- Successful snapshot exists -> return the Agent in `cached_agent_ids`; do not
  probe again.
- Successful model snapshot exists for the same Profile/model pair -> reuse it;
  do not probe again. A different model id never shares that row.
- Successful model evidence is authoritative even when one of its option sets
  is empty; an empty model-specific reasoning/mode/Feature set must not revive
  the Agent fallback for that field.
- Agent command/config changes while a probe is in flight -> discard the stale
  result by comparing the Agent config revision before commit.
- Probe fails before any success -> persist `last_attempt_at_ms` and the stable
  error code; mark the Agent projection `temporarily_unavailable` and permit an
  explicit retry.
- Adapter returns models -> clear them before the Agent snapshot write.
- A live response or update includes models -> clear them before publishing
  Profile-wide evidence; Provider Profile models stay authoritative. Explicit
  model probe evidence is stored under that model entry only.
- `ConfigOptionUpdate` omits the `configOptions` array -> ignore it rather than
  erasing the current evidence.
- `ConfigOptionUpdate` contains an empty `configOptions` array -> replace the
  previous option set with empty; withdrawn controls must not remain selectable.
- Live Profile evidence is absent or was invalidated -> layer the Agent fallback
  for that Profile.
- No Provider Profile or no configured Provider model -> Agent probing may
  still succeed, but no model probe is launched and the joined catalog has no
  model option for that Profile.
- Disabled Agent/Profile/model -> omit it; cached Agent evidence cannot revive
  an explicitly disabled model.
- Enabled non-ACP Profile -> omit it; never reinterpret it as an ACP runtime
  Profile.
- Unknown/needs-configuration Agent -> `requires_configuration`.
- Catalog revision changes -> clients re-fetch and reject stale desired
  selections.
- Agent account context revision/fingerprint changes -> discard old account
  model options and require a fresh catalog read; never carry an old explicit
  model or reasoning value across a relogin.
- An account model-scoped probe fails -> keep the snapshot marked incomplete so
  startup or an explicit refresh retries; never claim missing controls are a
  complete empty set. When a direct catalog already proved model ids and
  reasoning levels, retain that usable evidence and record the supplemental
  option failure instead of failing account verification.
- Catalog has an authenticated Agent account but no model evidence -> expose
  `AgentDefault`, not `Explicit { model_id: "default" }` or another sentinel.

### 5. Good/Base/Bad Cases

- Good: one Agent probe succeeds, two Provider Profiles render immediately from
  the fallback, then a real session for one Profile replaces only that Profile's
  controls and subsequent updates remove withdrawn choices.
- Base: an Agent has no Provider Profile. Its default account can still be
  selected as `AgentDefault`; the selector does not invent a model.
- Base: a Profile has models but the Agent exposes no reasoning control; its
  models remain selectable and the reasoning selector is empty.
- Bad: saving a Provider API key clears the Agent snapshot, blocks on a probe,
  or copies one model's options into another model's snapshot.
- Bad: a model returned by the Agent probe appears in a Profile that did not
  configure that model.
- Bad: a stale Profile-wide snapshot masks the full option set announced by a
  current session, or an incremental update leaves removed options behind.

### 6. Tests Required

- `cargo test -p vibex-db runtime_option_snapshot --locked` asserts Agent-keyed
  fallback and Provider/model-keyed SQLite round trips and deletion.
- `cargo test -p vibex-config-switch agent_acp_runtime_config --locked` asserts
  command/args/Agent-env resolution without a Provider Profile.
- `cargo test -p vibex-agent-acp session_config::tests --locked` asserts the
  Provider fallback evidence is shared without contributing model ids, while
  Agent-account model evidence remains scoped to its source and revision.
- `cargo test -p vibex-agent-acp config_option_update --locked` asserts a full
  replacement, runtime-state calibration, and model stripping.
- `cargo test -p vibex-desktop-runtime catalog --locked` asserts cached reuse,
  live-over-fallback layering, model stripping, model-specific efforts/modes,
  and failure recording by Agent/model.
- Desktop management tests assert Agent add/discovery/install detection invokes
  `probe_agent`, ordinary toggles and Profile saves do not, and a successful
  snapshot disables the probe button.
- Agent account catalog tests assert one source per Agent, revision/fingerprint
  invalidation, explicit model isolation, and the `AgentDefault` fallback.

### 7. Wrong vs Correct

#### Wrong

```rust
for model in profile.configured_models {
    manager
        .probe_session_config_for_model(profile.agent_id, profile.id, &model.id)
        .await?;
}
```

This starts Agent processes from an ordinary read path and discards the durable
per-model cache.

#### Correct

```rust
let fallback = catalog.probe_agent(&agent_id).await?;
let model_options = catalog.probe_profile_models(&profile_id).await?;
let options = catalog.list().await?;
// Model snapshot -> live/Profile fallback -> Agent fallback.
```

The successful Agent snapshot provides fast fallback; real sessions calibrate
only their own Profiles without probing during a catalog read.

## Scenario: Claude ACP Extension And Transcript Compensation

### 1. Scope / Trigger

Claude ACP may report background Agent/shell/task activity only through `_claude/*` extensions or its JSONL
transcript. These records must join the canonical event pipeline without weakening standard ACP routing.

### 2. Signatures

```text
decode_claude_extension(method, params) -> Option<ClaudeExtensionEvent>
parse_claude_transcript_line(line) -> Result<Option<ClaudeTranscriptEvent>, error>
ClaudeTranscriptTailWatcher::poll() -> Result<ClaudeTranscriptEvent[]>
```

### 3. Contracts

- Only known, versioned `_claude/background_*`, `_claude/task_*` methods decode; unknown extensions stay diagnostics.
- Transcript tail reads complete JSONL lines only and resets offset when a resumed/forked path relocates the file.
- Prompt fingerprints dedupe transcript copies of live ACP prompts; event ids dedupe repeated tail reads.
- Background work is keyed by `binding_id + activation_generation`; active work makes attachment idle sweep unsafe.
- Decoded records become `AgentEventInput` and then `CanonicalAgentEvent`, never a provider-specific timeline variant.

### 4. Validation & Error Matrix

- Malformed JSONL -> bounded diagnostic, continue at next line.
- Unknown extension -> ignore with bounded diagnostic.
- Partial final line -> retain offset and retry next poll.
- Old binding/generation -> route fence rejects before timeline/background state mutation.
- Transcript fixtures are sanitized before commit; live session extension
  values remain bounded and lossless in the authoritative Timeline.

### 5. Good/Base/Bad Cases

- Good: a live prompt and the matching transcript prompt produce one canonical prompt fingerprint.
- Base: a background task keeps its source attachment alive until completed, then idle sweep may reclaim it.
- Bad: an extension event with no routed native session is assigned to the current session by guesswork.

### 6. Tests Required

- Decoder tests cover known/unknown methods, status transitions and bounded fields.
- Tail tests cover complete/partial lines, duplicate polls and resume relocation.
- Fence tests cover binding/generation isolation and idle-sweep protection.
- Leak tests assert Debug/serialized diagnostics contain no prompt, token or native id.

### 7. Wrong vs Correct

```rust
// Wrong: unknown `_claude/*` payload is written directly to Timeline.
timeline.push(raw_params);

// Correct: decode known extension, route the fence, then normalize canonically.
if let Some(event) = decode_claude_extension(method, params) {
    attachment.handle_claude_extension(method, params);
}
```

## Scenario: Codex ACP Runtime Home And Semantic Events

### 1. Scope / Trigger

Codex ACP creates/resumes native threads, projects config/MCP/Skills into `CODEX_HOME`, decodes Codex extensions and
optionally calls unstable fork.

### 2. Signatures

```text
codex_acp_runtime_home_path(runtime_data, session_id, profile_id) -> PathBuf
ensure_private_runtime_directory(path) -> VibexResult<()>
write_private_runtime_file_atomic(path, bytes) -> VibexResult<()>
decode_codex_extension(method, params, compatibility_identity) -> Option<AgentEventInput>
plan_codex_fork(evidence, identity, generation, native_session_id) -> Option<CodexForkPlan>
```

### 3. Contracts

- Runtime home is exactly `<runtime-data>/codex-runtime-homes/<logical-session>/<profile>`; process fingerprints never
  become a home path component.
- Config projection acquires a home-local lock and atomically renames `config.toml`; native thread/history files are
  not deleted or moved by projection changes.
- Every Vibex-created Runtime Home/staging directory is owner-only (`0700` on
  Unix), and every Vibex-created projection, manifest, lock, and temporary file
  is owner-only (`0600` on Unix). Existing overly broad Vibex-owned boundaries
  are tightened before use and remain private after atomic rename.
- Private filesystem helpers inspect final entries without following symlinks,
  reject non-directory/non-file boundaries, create temporary files in the same
  directory, sync, rename, and revalidate the published file. They do not walk
  or chmod Agent-owned thread/history children.
- Non-Unix builds keep the same containment and atomic-write API; platform ACL
  hardening remains owned by the application data directory implementation.
- Diff content maps to `FileOperation` even when `rawInput` is absent. `oldText/newText` are optional lossless fields.
- WebSearch, TodoUpdate, Collaboration and ImageGeneration stay canonical product types.
- Codex `agent_message_chunk._meta.codex.phase` is decoded at the ACP boundary
  into the optional provider-neutral `AgentMessagePhase`. Accept only
  `commentary` and `final_answer`; missing or unknown metadata remains `None`
  for backward compatibility. A phase transition starts a new assistant segment
  so the final authoritative message cannot concatenate commentary into the
  user-facing answer.
- Fork requires exact negotiated `VersionedRaw` evidence for the current identity/generation.

### 4. Validation & Error Matrix

- Home lock timeout -> `codex_runtime_home_busy`.
- Temp write/rename failure -> structured storage error; previous config remains authoritative.
- Final Runtime Home symlink/non-directory ->
  `validation/acp_private_directory_invalid`; do not chmod the target.
- Existing projection symlink/non-file ->
  `validation/acp_private_file_invalid`; do not write the target.
- Create/write/sync/publish/permission failure -> stable `acp_private_*`
  storage error with only the I/O error kind in diagnostics.
- Unknown `_codex/*` -> bounded diagnostic, no timeline mutation.
- Missing or unknown Codex message phase -> preserve the text with phase `None`;
  never guess from wording or mark it as a live final answer.
- Static/old-generation fork evidence -> unavailable; never send the request.
- Unsupported image extension -> `mimeType = null` but keep a bounded image reference.

### 5. Good/Base/Bad Cases

- Good: a fingerprint/config change rewrites config atomically and the existing native thread remains resumable.
- Good: an existing Vibex-owned `0777` Runtime Home is tightened to `0700`,
  a replacement config is `0600`, and an Agent-created history child is not
  recursively modified.
- Base: Diff supplies path plus old/new text only in content; canonical FileOperation keeps all three.
- Bad: `CODEX_HOME` points at `<session>/<profile>/<fingerprint>` and strands earlier thread state.
- Bad: call `fs::write` through a symlink, rely on process umask for secrets,
  or recursively chmod the provider's native session tree.

### 6. Tests Required

- Stable-home tests assert path equality across materializations and preservation of an existing thread-state file.
- Unix permission tests assert `0700` directories, `0600` files after replace,
  broad-mode tightening, temporary cleanup, final symlink rejection, unchanged
  symlink targets, and no recursive ownership of Agent history.
- Workspace check and non-Unix compilation must keep the helper API portable.
- Diff tests assert add/update/delete mapping and old/new text preservation.
- Parity golden tests assert WebSearch/Todo/Collaboration semantic variants.
- ACP runtime tests assert `commentary -> final_answer` preserves both streamed
  phases while the final message contains only the final-answer segment.
- Fork tests assert exact identity/generation/source/encoding gating.

### 7. Wrong vs Correct

```text
Wrong: fs::write(CODEX_HOME/<fingerprint>/config.toml) through ambient umask
Correct: CODEX_HOME/<session>/<profile>/sessions + owner-only atomic .vibex projection
```

## Unified Session States

Use exactly these top-level states at client boundaries:

| State | Meaning |
| --- | --- |
| `initializing` | Logical Session and ACP attachment are being prepared or restored. |
| `idle` | Ready for user input. |
| `running` | A turn is executing and may stream events. |
| `needs_input` | Waiting for permission, a question answer, or another user choice. |
| `error` | The latest turn failed, but the session can continue. |
| `closed` | Session runtime is closed. |
| `archived` | User archived it and it is hidden by default. |

Provider-specific states must map into this set before crossing the Agent
service boundary.

## Required Session Operations

The provider-neutral service must model these operations even if a provider
needs fallback behavior:

- Create session.
- Resume session.
- Import native history.
- Send text, rich attachments, file references, and image inputs.
- Interrupt the current turn.
- Continue, retry, fork, rollback, compact, archive, unarchive, delete, rename,
  and copy session link.
- Set model, reasoning effort, permission mode, working directory, sandbox,
  network/web search, and extra context.
- Read token usage, account status, context window, capabilities, slash
  commands, skills, and MCP status where supported.

If the selected Agent/Adapter does not support an operation, return a capability-aware
error instead of silently ignoring the request.

Session creation and fork may report a durable `initializing` snapshot before
ACP runtime materialization finishes so clients can navigate without waiting for
process startup. The snapshot must be emitted only after the Logical Session and
its initial Timeline prefix commit successfully. The mutation itself still
resolves with the final ready session or a structured initialization error.

## Scenario: ACP Session Attachment Ownership And Native Routing

### 1. Scope / Trigger

- Trigger: an ACP process creates, loads, prompts, updates, requests permission,
  creates a terminal, crashes, or is reused by more than one Logical Session.
- `AcpProcess` is a transport resource. Every session-scoped side effect must
  first resolve a `SessionAttachmentRegistry` fence; dedicated processes do not
  get a routing exception.

### 2. Signatures

```rust
SessionAttachmentAcquireKey {
    binding_id: RuntimeBindingId,
    native_session_id: Option<String>,
    process_instance_id: AcpProcessInstanceId,
    activation_generation: u64,
}

SessionAttachmentEventFence {
    binding_id: RuntimeBindingId,
    activation_generation: u64,
    process_instance_id: AcpProcessInstanceId,
    native_session_id: String,
}

SessionAttachmentRegistry::acquire(session_id, key, operation)
SessionAttachmentRegistry::activate(fence)
SessionAttachmentRegistry::route(process_instance_id, native_session_id, method)
SessionAttachmentRegistry::apply_current(fence, synchronous_operation)
SessionAttachmentRegistry::acquire_prompt(fence)
SessionAttachmentRegistry::mark_crashed(fence)
AcpProcess::request_with_registration_barrier(
  session/new | session/load | session/resume
)
```

Inbound session-scoped ACP envelopes:

```text
notification: session/update { params.sessionId, params.update }
request: session/request_permission { id, params.sessionId, ... }
request: terminal/create { id, params.sessionId, ... }
```

### 3. Contracts

- A live attachment is keyed by surrogate `RuntimeBindingId`; a
  `VibexSessionId` selects current attachment state but is never reused as the
  binding id.
- Opening or selecting a session for history inspection requests a Viewer lease.
  Viewer attachment may keep an already materialized runtime warm, but it must
  not create a process or restore a native session. Actual message/command work
  materializes through an internal worker lease; an Owner lease is reserved for
  clients that explicitly request runtime ownership.
- The native route lookup key is exactly
  `(AcpProcessInstanceId, native_session_id)`. Delivery then checks all four
  fence fields and requires the attachment to be committed and current.
- Same-key concurrent acquire executes `session/new` or `session/load` once.
  Different keys may run concurrently. Load/new run after process acquire and
  outside the process acquire lock; failed/losing attachment candidates release
  their `ProcessLease::attach()` reservation.
- An attachment record retains the exact normalized acquire key, including
  whether `native_session_id` was `None` or `Some(id)`. The final event fence
  cannot reconstruct that distinction, so existing-attachment de-duplication
  must compare the stored key instead of synthesizing a key from the returned
  native id.
- Per-key acquire locks are weak registry entries with RAII cleanup. Success,
  error, panic, waiter cancellation, and cancellation while the load/new future
  is running must all release the strong lock owner; later lookup prunes any
  dead weak entry so cancelled unique keys cannot grow the lock table.
- A created attachment starts `Prepared`. Prepared events are quarantined and
  have zero timeline/config/permission side effects; `activate` atomically
  inactivates the previous current attachment and commits the new generation.
- Missing, empty, unknown, old-process, old-native, old-generation, and
  old-binding events never fall back to a default or unique session. A request
  that needs an ACP response is cancelled or rejected immediately.
- Active turn sink/chunk state, tool-call merge state, available commands,
  model/mode/config state, pending permissions, and pending terminal-create
  requests belong to `AcpSessionAttachment`. `AcpProcess` owns only transport
  requests, initialization capabilities, child lifecycle, terminal host, and
  immutable launch metadata.
- Prompt admission uses the attachment prompt gate, revalidates the fence after
  waiting, claims the active turn, and enqueues `session/prompt` synchronously
  under `apply_current`. The prompt mutex is not held for the response round
  trip. An RAII guard clears prompt/turn/host-request state on error, timeout,
  or future cancellation.
- An async session-config response such as `session/set_model` or
  `session/set_config_option` re-enters `apply_current` before mutating model,
  mode, or config state; a response that became stale while awaiting I/O has
  zero attachment side effects.
- `session/new`, `session/load`, and `session/resume` use a bounded response
  registration barrier: after the response yields or confirms its native id,
  the stdout reader waits only until the exact route is registered and activated
  before reading immediately following notifications.
- An ACP Agent may publish `available_commands_update` before the corresponding
  registration response. While a registration-barrier request is pending, an
  update for an unknown exact native route is retained in a process-local map
  keyed by native session id, capped at 16 catalogs with latest-update-wins
  replacement. The validated response drains that catalog into the new
  attachment before route registration. Updates for an already registered
  pooled-session route are delivered normally, and every other unroutable event
  keeps the normal diagnostic path. The buffer does not infer identity or become
  process-global session authority.
- Each created attachment subscribes to `ProcessLease::subscribe_crashes()`
  before load/new. Broadcast plus process snapshot closes the registration race;
  `mark_crashed(fence)` makes fan-out idempotent. Detach removes the route before
  cancelling state and decrements exactly one process reservation.
- Debug output masks prompt/env values and represents native session ids as a
  short SHA-256 fingerprint. Fence, route diagnostics, terminal request Debug,
  and OpenCode stderr diagnostics must not contain the raw id.

### 4. Validation & Error Matrix

- Empty attachment native id -> `validation/acp_attachment_native_session_id_empty`.
- Operation returns a different expected native id ->
  `conflict/acp_attachment_native_session_mismatch`.
- Live binding has a different acquire key ->
  `conflict/acp_attachment_key_conflict`.
- Same binding/process/generation but a different expected native id, including
  `None` versus `Some(id)`, -> `conflict/acp_attachment_key_conflict` before the
  load/new closure runs.
- Cancelled attachment acquire -> no attachment, route, or live key-lock entry;
  the exact key remains retryable.
- Duplicate `(process, native)` route ->
  `conflict/acp_native_session_route_conflict`; original route remains.
- Missing event `sessionId` -> process diagnostic
  `acp_event_session_id_missing`; requests fail closed.
- Empty event `sessionId` -> process diagnostic `acp_event_session_id_empty`.
- Unknown process/native route -> process diagnostic
  `acp_event_session_route_unknown`.
- Pre-response `available_commands_update` for an unknown native route while a
  registration request is pending -> bounded pending catalog with no
  `acp_event_session_route_unknown` warning; without that pending request it is
  an ordinary unroutable diagnostic.
- More than 16 pending command catalogs -> evict the oldest native-session
  catalog; never grow process memory without a bound.
- Binding or generation mismatch -> process diagnostic `acp_event_fence_stale`.
- Prepared attachment -> quarantine `acp_event_attachment_prepared`.
- Non-current/inactive attachment -> `acp_attachment_not_current` or
  `acp_event_attachment_inactive`.
- Second active prompt -> `conflict/acp_turn_already_running`.
- Activation generation regression ->
  `conflict/acp_attachment_generation_regression`.

### 5. Good/Base/Bad Cases

- Good: two native sessions on one verified pooled process interleave updates,
  model state, permissions, and interrupts; each modifies only its attachment.
- Good: Codex ACP announces `/compact` before answering `session/new`; the
  created attachment exposes `/compact` through live command discovery.
- Base: one pooled session has a committed route while another is registering;
  a command update for the committed session is delivered there, not captured
  by the pending-registration buffer.
- Good: process crash broadcasts once; both affected attachments receive one
  recoverable error carrying their own fence, then a rebuild uses a newer
  generation and rejects late old-process events.
- Base: concurrent duplicate acquire returns `Existing`; its extra process
  reservation is detached without closing a pooled process still in use.
- Base: a permission request has no recognized native id; Vibex returns
  `cancelled` and records only a bounded process diagnostic.
- Bad: `AcpProcess` stores a default native id or first `VibexSessionId` and
  routes a missing-id update to it.
- Bad: permission/tool/config maps are keyed only by native id in process-global
  state, allowing pooled sessions to drain or merge each other's state.

### 6. Tests Required

- Registry unit tests assert same-key at-most-once, exact expected-native key
  comparison, different-key parallelism, error/cancellation retry and lock
  cleanup, binding/route conflict, and original-route preservation.
- Fence tests independently mutate binding, generation, process instance, and
  native id; every mismatch must have zero attachment side effects.
- Prompt tests assert same-attachment conflict, different-attachment isolation,
  replacement while waiting, enqueue fence atomicity, and future-cancellation
  cleanup.
- Mock ACP integration asserts pooled update/model/tool/permission isolation,
  target-only interrupt/close, missing/unknown dedicated routing rejection, and
  terminal/permission fail-closed responses.
- Mock ACP registration tests send `available_commands_update` before successful
  `session/new`, `session/load`, and `session/resume` responses and assert each
  created attachment exposes the command; unit tests assert the pending catalog
  remains capped at 16 and a repeated native-session update replaces its earlier
  value.
- Crash tests assert registration-race coverage, single fan-out per attachment,
  exact reservation decrement, rebuild generation increment, and rejection of
  late old-process events.
- Leak tests scan Registry Debug, ACP debug log, OpenCode stderr error Debug, and
  terminal request Debug for raw native ids, prompt text, env values, tokens,
  and secrets.

### 7. Wrong vs Correct

#### Wrong

```text
session/update without sessionId
  -> process.default_native_session_id
  -> process.active_turns[native]
```

#### Correct

```text
raw ACP envelope(processInstanceId, params.sessionId)
  -> SessionAttachmentRegistry native route
  -> full four-field fence + committed/current check
  -> attachment-local normalization/state update
  -> otherwise quarantine or bounded process diagnostic
```

#### Wrong

```rust
// The returned fence loses whether acquire expected None or Some(native_id),
// and a strong map entry survives future cancellation.
existing.fence == fence_rebuilt_from(&requested_key, &existing.fence.native_session_id)
acquire_locks: HashMap<SessionAttachmentAcquireKey, Arc<Mutex<()>>>
```

#### Correct

```rust
existing.acquire_key == requested_key
acquire_locks: HashMap<SessionAttachmentAcquireKey, Weak<Mutex<()>>>
// Each acquire owns an RAII guard that removes the matching weak entry when
// the final strong lock owner exits, including cancellation paths.
```

#### Wrong

```text
available_commands_update before session/new response
  -> exact route missing -> unroutable diagnostic -> discard
response -> register route -> command catalog stays empty
```

#### Correct

```text
registration request pending + unknown exact route + available_commands_update
  -> bounded pending catalog[nativeSessionId]
response validates nativeSessionId
  -> drain catalog into attachment -> register/activate route -> release barrier
```

## Scenario: ACP Permission Callback Loop

### 1. Scope / Trigger

- Trigger: An ACP agent sends `session/request_permission` while a Vibex turn is
  streaming, and the user resolves that request through Vibex.
- This is a cross-layer contract because a native JSON-RPC request flows through
  `AcpRuntimeClient`, `AcpAgentProvider`, `AgentManager`, permission storage,
  persisted timeline items, and UI/remote resolution commands.

### 2. Signatures

ACP runtime:

```text
agent -> Vibex JSON-RPC:
session/request_permission {
  id,
  params: {
    sessionId,
    toolCall?,
    options?: [{ optionId | id, name | label, kind | type }]
  }
}

AcpClient::resolve_permission(AcpPermissionResolution {
  binding: ProviderBinding, // synthesized from current RuntimeBinding fence
  resolution: PermissionResolution
}) -> ()
```

Provider-neutral manager:

```text
AgentManager::resolve_permission(ResolvePermissionRequest)
  -> TimelineItem

PermissionRepository::insert_request(PermissionRequest)
PermissionRepository::resolve(PermissionResolution)
PermissionRepository::pending_for_session(VibexSessionId)
```

### 3. Contracts

- Every ACP permission request must allocate one stable Vibex `RequestId` and
  persist it as both the timeline permission `id` and ACP
  `provider_request_id` when no better native id exists.
- `AcpSessionAttachment.pending_permissions` owns the native JSON-RPC `id`,
  parsed ACP option summaries, and attachment fence until exactly one
  resolution, cancellation, detach, or process crash drains it.
- `PermissionRequest.response_options` persists every recognized ACP option as
  `{ optionId, label, response }` in Agent order. `label` is the bounded,
  redacted Agent display name; `response` is the provider-neutral semantic kind.
  `allowed_responses` remains the compatibility fallback for old records and
  non-ACP permission producers.
- UI clients render `response_options` when non-empty and return the selected
  `optionId` as `PermissionResolution.provider_resolution_id`. They must not
  collapse multiple options with the same semantic response: an Agent may offer
  both a session-wide allow and a command-prefix allow as distinct
  `allow_always` options.
- Timeline permission payloads must include safe details for tool kind,
  tool-call id, raw input summary, option kinds, or source. Secret-like values
  must be redacted before persistence.
- `Approve` prefers `allow_once`, then `allow_always`; `AlwaysAllowForSession`
  prefers `allow_always`, then `allow_once`; `Deny` prefers `reject_once`, then
  `reject_always`. Option aliases such as `approve`, `deny`, and hyphen/space
  variants must be normalized before selection.
- If no compatible ACP option exists, Vibex must answer the native request with
  `{ "outcome": { "outcome": "cancelled" } }` rather than hanging the agent.
- Duplicate resolution attempts are idempotent: storage may append an audit
  timeline resolution item, but the native ACP JSON-RPC response must not be
  sent more than once.
- Permission resolution reloads the Logical Session's current durable
  selection/binding/generation and validates the exact ACP attachment before
  synthesizing the adapter-local binding. It does not read a legacy session
  provider binding or resolve by ProviderKind.
- While a permission is pending, the provider turn remains running or the
  session remains `needs_input`; once the permission is resolved and the agent
  finishes streaming, the session settles to `idle`.
- Turn interrupt, session close, process shutdown, process exit, or missing
  active turn sink must drain pending ACP permissions with a cancelled response
  where the child process is still reachable.

### 4. Validation & Error Matrix

- Unknown session -> `validation/session_not_found`.
- Missing/stale current RuntimeBinding or attachment fence ->
  `conflict/runtime_binding_missing` or a bounded ACP attachment mismatch; no
  native response is sent to another attachment.
- Provider does not support permission callbacks ->
  `capability/<provider>_permission_resolution_unsupported`.
- Permission request id is unknown ->
  `validation/permission_request_not_found`.
- Native ACP process is gone before first resolution ->
  `conflict/acp_permission_process_missing`.
- Duplicate native resolution after pending state was removed -> no-op success.
- Missing compatible ACP option for selected Vibex response -> cancelled ACP
  outcome response.
- Unknown option id, or an option id whose semantic response does not match the
  submitted response -> `validation/permission_response_option_invalid`; do not
  resolve the durable request or answer the native callback.

### 5. Good/Base/Bad Cases

- Good: ACP sends `session/request_permission`; Vibex persists a timeline
  permission with `provider_request_id`; user approves; runtime sends selected
  allow option once; the blocked turn continues and returns to `idle`.
- Good: User denies; runtime sends selected reject option once and timeline
  state records the denial.
- Good: ACP offers `Allow for session` and `Allow commands starting with cargo`
  as separate allow options; the UI preserves both labels and the runtime sends
  the exact selected option id.
- Base: User interrupts while permission is pending; runtime replies cancelled,
  sends `session/cancel`, and clears pending native state.
- Base: UI retries the same resolution due to reconnect or double-click; manager
  records the retry as audit if needed, and runtime does not send a second
  native JSON-RPC response.
- Bad: `AgentManager` updates storage but does not call provider
  `resolve_permission`, leaving the ACP child blocked forever.
- Bad: Runtime holds a pending-permission lock while writing to child stdin.
- Bad: Missing active turn sink leaves `session/request_permission` unanswered.

### 6. Tests Required

- Runtime unit/integration tests with a mock ACP process must assert approve,
  reject, interrupt/cancel, duplicate resolution no-op, and process close paths.
- Manager-level tests must assert permission timeline persistence includes
  `provider_request_id`, `resolve_permission` delegates to the provider, and
  duplicate resolution succeeds without a second provider callback.
- End-to-end ACP provider tests must assert a mid-turn permission blocks the
  turn until resolution, then streams post-resolution output and settles the
  session to `idle`.
- Regression tests must cover option normalization for `kind`, `type`, and
  `name` fields plus hyphen/space/case variants.
- Persistence and UI projection tests must assert option id/label/order survive
  database and timeline round trips. Runtime tests must assert exact-id
  selection and reject an id/semantic-kind mismatch.

### 7. Wrong vs Correct

#### Wrong

```text
session/request_permission -> persist timeline item -> immediately return cancelled
```

This surfaces a permission card but prevents the user decision from reaching the
ACP agent.

#### Correct

```text
session/request_permission
  -> pending_permissions[request_id] stores native rpc id + options
  -> timeline PermissionRequest(provider_request_id=request_id, response_options)
  -> UI returns the exact option id in provider_resolution_id
  -> AgentManager::resolve_permission
  -> AcpClient::resolve_permission
  -> JSON-RPC response selected/cancelled exactly once
  -> provider turn completes
```

## Scenario: ACP Form Elicitation Callback Loop

### 1. Scope / Trigger

- Trigger: any ACP Agent sends the unstable `elicitation/create` request with a
  session-scoped `form` while a turn is active.
- This contract spans ACP capability negotiation and typed schema decoding,
  provider-neutral Timeline/Core DTOs, SQLite request state, native and Remote
  mutation APIs, and the shared desktop/mobile workbench.
- Support is capability-based for every ACP Agent. It must not branch on
  Claude, Codex, OpenCode, or another Agent id.

### 2. Signatures

ACP transport:

```text
initialize.clientCapabilities.elicitation = { form: {} }

agent -> Vibex JSON-RPC:
elicitation/create {
  id,
  params: {
    message,
    mode: {
      form: {
        scope: { session: { sessionId, toolCallId? } },
        requestedSchema
      }
    }
  }
}

Vibex -> agent JSON-RPC result:
{ action: "accept", content } | { action: "decline" } | { action: "cancel" }
```

Provider-neutral service and storage:

```text
AgentProvider::resolve_elicitation(ProviderElicitationResolution {
  session_id,
  binding,
  execution_identity: { binding_id, activation_generation, model_id },
  resolution
}) -> ()
AgentManager::resolve_elicitation(ResolveElicitationRequest) -> TimelineItem
ElicitationRepository::insert_request(ElicitationRequest)
ElicitationRepository::resolve(ElicitationResolution) // pending-only CAS
ElicitationRepository::pending_for_session(VibexSessionId)

RemoteAgentRequest::ResolveElicitation {
  auth,
  request: ResolveElicitationRequest
} -> RemoteAgentResolveElicitationResponse { item }

elicitation_requests(
  request_id PRIMARY KEY,
  session_id REFERENCES agent_sessions,
  status,
  request_json,
  resolution_json,
  requested_at_ms,
  resolved_at_ms
)
```

### 3. Contracts

- The ACP schema dependency enables its `unstable` feature explicitly. Vibex
  advertises only `elicitation.form`; URL mode and non-session scopes are not
  claimed or inferred.
- Each admitted callback gets one Vibex `RequestId`. The attachment keeps the
  native JSON-RPC id in `pending_elicitations`; Timeline and SQLite keep only
  the provider-neutral request, fields, status, and optional `tool_call_id`.
- Decode string, finite number, integer, boolean, and enum-backed string-array
  fields. Deterministically project required membership, defaults, bounds,
  titles, descriptions, enum values, and enum labels. Durable numbers use
  canonical finite decimal strings so Core DTOs retain `Eq`; only the ACP
  adapter converts them back to JSON numbers.
- Pattern-constrained strings and recognized-but-unrenderable property types
  become `Unsupported` fields. A required unsupported field disables Accept but
  still permits Decline. A malformed, oversized, URL-mode, server-scoped, or
  otherwise unnormalizable request receives `cancel` immediately and creates no
  Timeline or database state.
- Admission is bounded to 32 pending callbacks per attachment, 32 fields per
  form, 64 options per field, 128 characters per field id, 512 characters per
  label, and 4096 characters per message/description.
- `ElicitationRequest` and `ElicitationResolution` are append-only Timeline
  variants. An unresolved request makes the Logical Session `needs_input`; the
  session returns to `idle` only when both pending permissions and pending
  elicitations are empty and no turn is running.
- Accept validates every required field, answer id, answer type, option value,
  uniqueness, length, numeric bound, and item-count bound. Decline and Cancel
  carry no answers.
- Resolution order is provider callback first, then the SQLite pending-only CAS
  and user Timeline append. A callback error leaves the durable request pending
  for retry. The ACP runtime removes its pending callback only after the JSON-RPC
  result enters the process write queue; a closed stdin leaves it retryable.
- `AgentManager` serializes resolutions by request id with weak async keyed
  locks. Therefore concurrent desktop/Remote answers cannot send one value to
  the Agent while persisting another; after the winner commits, losers receive
  `elicitation_request_not_pending` without a provider callback.
- The manager passes the exact durable binding id and activation generation to
  the ACP runtime. Before touching pending state, runtime matches that identity,
  native session id, and Provider Profile against the current attachment.
- Native desktop and shared mobile UI render the same typed fields and fence
  duplicate submissions per elicitation request. Agent mutation tasks are held
  by mutation request id so answering a form cannot cancel an in-flight send,
  permission response, or another elicitation response.
- Remote resolution is an interactive, idempotency-required mutation. It uses
  `ResolveElicitation` authorization, an elicitation-specific audit target, and
  validates nested session/request ids before manager dispatch.

### 4. Validation & Error Matrix

- Envelope request/session id mismatch ->
  `validation/elicitation_resolution_target_mismatch`.
- Unknown request -> `validation/elicitation_request_not_found`.
- Request already accepted/declined/cancelled or loses the SQLite CAS ->
  `conflict/elicitation_request_not_pending`.
- Accept omits a required field, names an unknown field, uses the wrong answer
  type, violates a bound, repeats a multi-select value, or selects an unknown
  option -> `validation/elicitation_answer_invalid` with bounded `fieldId`.
- Decline/Cancel includes answers -> `validation/elicitation_non_accept_answers`.
- ACP profile lacks callback support ->
  `capability/acp_elicitation_resolution_unsupported`.
- Current runtime binding/attachment is missing or stale -> bounded runtime or
  attachment conflict, including `acp_elicitation_attachment_mismatch`; never
  route by ProviderKind or guess another session.
- ACP process stdin is closed before the response is queued ->
  `process/acp_process_stdin_closed`; keep both native callback and durable row
  pending.
- Invalid/unroutable/unsupported inbound form, no active turn sink, or pending
  limit reached -> return ACP `cancel`; persist nothing.
- Remote device lacks approval authority or omits mutation idempotency -> deny
  before manager dispatch and emit no provider callback.

### 5. Good/Base/Bad Cases

- Good: a generic ACP Agent requests a required single choice plus optional
  text; desktop submits typed answers, the Agent receives one `accept`, and the
  persisted resolution matches it exactly.
- Good: stdin closes during response; the first submission fails, reconnect
  restores the write path, and retry sends the same pending callback once.
- Good: desktop and mobile submit different answers concurrently; one wins the
  request lock and both provider callback and Timeline contain that answer.
- Base: the form contains an optional unsupported field; UI identifies it as
  unavailable and can submit the supported required fields.
- Base: the form contains a required unsupported field; Accept is disabled but
  Decline remains available.
- Bad: detect `AskUserQuestion` by Agent name or tool title and synthesize a
  private UI payload instead of decoding ACP `elicitation/create`.
- Bad: remove the pending native RPC id before checking the process write result.
- Bad: use one GPUI task slot for send and elicitation mutations, so opening or
  answering the form drops the active send task.

### 6. Tests Required

- Core tests assert required/type/enum/bounds/multi-select validation, target
  matching, non-Accept answer rejection, and unsupported-pattern behavior.
- ACP protocol/runtime tests assert unstable capability serialization, typed
  form decoding, bounds, session routing, unsupported mode cancellation,
  accept/decline/cancel encoding, duplicate no-op, and callback preservation on
  write failure.
- Manager tests assert request persistence drives `needs_input`, provider
  failure remains retryable, concurrent conflicting resolutions deliver and
  persist one identical winner, and the final pending request returns to idle.
- Database tests assert migration 36, request/status round trip, pending query,
  resolution JSON, and pending-only CAS behavior.
- Backend/Remote tests assert capability exposure, nested id validation,
  approval authorization, audit target, interactive timeout, and mandatory
  mutation idempotency.
- Desktop-model and shared UI tests assert both Timeline kinds project, drafts
  validate before submit, duplicate submits are fenced per request, and native,
  Web, and mobile builds consume the same typed contract.

### 7. Wrong vs Correct

#### Wrong

```text
remove pending_elicitations[requestId]
-> ignore process.send(response) failure
-> resolve SQLite with whichever concurrent caller wins
```

#### Correct

```text
request-keyed manager lock
-> reload and validate pending durable request + exact runtime fence
-> queue ACP JSON-RPC response while native callback remains pending
-> remove native callback only after queue success
-> pending-only SQLite CAS + Timeline ElicitationResolution
-> release lock; later attempts observe not-pending
```

## Scenario: Composer Agent Command Discovery And Execution

### 1. Scope / Trigger

- Trigger: Agent composer command entry uses `/`, `@`, and `$` across Codex,
  Claude Code, ACP/OpenCode, prompts, skills, and workspace references.
- This is a cross-layer contract because Rust DTOs flow through
  `AgentManager`, typed Backend adapters, and the shared desktop composer.

### 2. Signatures

Provider trait:

```text
AgentProvider::discover_commands(AgentCommandDiscoverRequest)
  -> AgentCommandDiscoverResponse
AgentProvider::execute_command(handle, AgentCommandExecuteRequest, ProviderTurnRequest)
  -> ProviderTurnResult
AcpClient::list_session_commands(session_id)
  -> Option<Vec<AcpRuntimeCommand>>
```

Tauri commands:

```text
agent_discover_commands(AgentCommandDiscoverRequest)
  -> AgentCommandDiscoverResponse
agent_execute_command(AgentCommandExecuteRequest)
  -> AgentCommandExecuteResult
```

Discovery request shape:

```text
AgentCommandDiscoverRequest {
  agent_id?,
  provider_profile_id?,
  session_id?,
  workspace_id?,
  trigger?,
  query?,
  limit?
}
```

Command discovery must use a concrete `agent_id`, or derive it from the
session's current durable selection. A Provider Profile id is required only
when the durable auth source is a Provider Profile; an Agent-account session
keeps `provider_profile_id=None`. ProviderKind is not part of the request and
cannot distinguish ACP Agents.

### 3. Contracts

- Provider slash commands must be emitted by the provider adapter with
  `source_kind=provider`, `trigger=slash`,
  `selection_behavior=insert`, and
  `execution_behavior=provider_command`.
- Selecting a provider command inserts command text only; it must not start a
  provider turn until the user sends.
- Send-time provider slash commands call `agent_execute_command`, which
  validates the current selection/binding/generation fence and delegates to
  the exact ACP route's `AgentProvider::execute_command`.
- Session-scoped slash discovery always calls the exact Agent provider even
  when its static `ProviderCapabilities.slash_commands` hint is false. Static
  capabilities may describe a pre-session fallback, but they must not hide a
  live Agent catalog.
- User slash prompts use `source_kind=prompt` and
  `execution_behavior=expand_prompt_and_send`.
- `$` skills and `@` references are insert-only in this contract and must not
  execute directly.
- No immediate `client_builtin` command is registered by default. Future
  immediate execution requires `source_kind=client_builtin`,
  `selection_behavior=execute_immediately`, and `destructive=false`.
- Desktop may merge workspace file references and local skill manifests at the
  Tauri layer, but provider commands stay owned by provider adapters.
- A live ACP `available_commands_update` catalog is authoritative for an
  attached session. `Some(commands)` means an attached authoritative catalog,
  including `Some([])`; `None` means no attachment/catalog exists and permits a
  pre-session fallback. Before a Logical Session exists, the provider adapter
  may expose the built-in catalog of an exact pinned managed Adapter; the Codex
  fallback must match the selected `codex` Agent, a `codex-acp` launch shape,
  and a Profile with `slash_commands` enabled.
- Provider execution accepts only command text beginning with one `/name`
  token, requires an optional request `command_name` to match that token, and
  requires the current session catalog to advertise the same Provider slash
  command. Unknown or stale commands fail before `session/prompt`.
- The exact slash command text must begin the ACP prompt's first text block.
  A pending Context Bridge remains pending for the next ordinary turn and must
  not be prefixed to a provider slash command; adapters such as Codex recognize
  commands only at the beginning of that text block.
- ACP/OpenCode provider slash commands are profile-aware. Generic ACP profiles
  must not receive the OpenCode command catalog unless their typed profile
  config identifies an OpenCode-compatible runtime or catalog marker.

### 4. Validation & Error Matrix

- Current session does not advertise the requested Provider command ->
  `capability/acp_slash_command_not_available`; no provider turn.
- Non-slash provider command trigger -> `validation/provider_command_trigger_invalid`.
- Empty provider command text -> `validation/provider_command_empty`.
- Command text does not begin with one `/name` token ->
  `validation/provider_command_text_invalid`.
- Optional `command_name` differs from the text token ->
  `validation/provider_command_name_mismatch`.
- Direct execution of `skill` or `reference` -> `capability/agent_command_source_not_executable`.
- Unregistered immediate client built-in -> `capability/client_builtin_command_unregistered`.
- Missing session on execution -> `validation/session_not_found`.

### 5. Good/Base/Bad Cases

- Good: Claude Code `/review` is discovered as a provider command, inserted
  into the composer, edited by the user, and executed only after Send.
- Good: OpenCode ACP `/status` is discovered through the ACP adapter and
  executes by sending the slash command text through the ACP provider turn.
- Good: an Agent-account session with a stale static slash capability still
  exposes its live `/review` catalog with no unrelated Provider Profile id.
- Base: an attached Agent publishes an empty live catalog; discovery returns no
  provider commands and does not revive a static Codex fallback.
- Bad: UI hard-codes Claude/Codex/OpenCode commands without provider adapter
  participation.
- Bad: Selecting `/review` immediately starts execution before the user presses
  Send.
- Bad: Multiple ACP sources share one ProviderKind-based static catalog when
  their command sets diverge; discovery must remain Agent/auth-source-aware.
- Bad: prefix a Context Bridge before `/review`, causing the Agent to parse the
  request as ordinary conversation text.

### 6. Tests Required

- Provider adapter tests assert deterministic command discovery for every
  static catalog added.
- Codex ACP tests assert the pinned built-in catalog is available with no
  `session_id`, live session commands replace that fallback, an explicitly
  empty live catalog suppresses it, and Generic ACP Profiles never inherit it.
- Manager tests assert a session catalog is queried despite a false static
  slash capability, Agent-account discovery preserves a null Profile id,
  unknown commands do not reach the provider, and valid command text reaches
  `execute_command` unchanged. Parser tests cover missing, empty, nested, and
  argument-bearing slash tokens.
- Desktop/Tauri tests assert `$` local skills and `@` references are merged
  without duplicating provider entries.
- Frontend checks assert typed generated DTOs are consumed and command
  selection remains insert-only.
- Manual UI smoke covers in-session composer and new-session composer for `/`,
  `@`, and `$`.

### 7. Wrong vs Correct

#### Wrong

```typescript
// UI-local hard-code bypasses provider capability and execution validation.
if (agentId === "claude") {
  suggestions.push({ label: "/review", executeImmediately: true });
}
```

#### Correct

```text
composer -> agent_discover_commands -> AgentManager -> live AgentProvider catalog
selection -> insert text only
send -> validate current catalog -> preserve pending bridge
  -> AgentProvider::execute_command with first text block beginning `/name`
```

## Scenario: Failed Turn One-Click Continue

### 1. Scope / Trigger

- Trigger: Agent conversation UI offers a one-click continue action after a
  provider call or streaming turn fails and the session enters `error`.
- This is a cross-layer contract because Rust DTOs flow through
  `AgentManager`, Tauri commands, remote Agent requests, generated TypeScript
  bindings, and the desktop session UI.

### 2. Signatures

Backend manager and Tauri command:

```text
AgentManager::continue_turn(ContinueAgentTurnRequest) -> Vec<TimelineItem>
agent_continue_turn(ContinueAgentTurnRequest) -> Vec<TimelineItem>
```

Remote request:

```text
RemoteAgentRequest::ContinueTurn(RemoteAgentContinueTurnRequest)
RemoteAgentContinueTurnRequest {
  auth,
  request: ContinueAgentTurnRequest
}
RemoteAgentContinueTurnResponse {
  appended_items: Vec<TimelineItem>
}
```

Core request:

```text
ContinueAgentTurnRequest {
  session_id: VibexSessionId,
  correlation_id: Option<CorrelationId>
}
```

### 3. Contracts

- `continue_turn` is provider-neutral. UI code must call the continue command
  and must not send its own hidden `SendAgentMessageRequest`.
- The backend owns the fallback continue prompt when no negotiated Agent
  continue/resume-turn operation exists.
- The fallback prompt is sent to the provider as turn input but is not appended
  as a `user_message` timeline item and must not create an optimistic user
  bubble in the frontend.
- If a future ACP Agent exposes a failed-turn continuation operation, it
  may be used behind `AgentManager::continue_turn` without changing desktop or
  remote request shapes.
- Normal `send_message` remains visible and appends a `user_message`; only
  `continue_turn` suppresses the user-message timeline item.
- `error -> running` is a valid state transition so failed sessions can start a
  continuation turn.
- `continue_turn` treats a turn as normally complete only when the latest
  conversational timeline segment ends with an explicit final
  `TimelinePayload::AgentMessage { is_final: true }`. An `error` or `idle`
  session whose latest segment has user/Agent content without that final
  message is eligible for continuation; system notices alone are not a turn.
- ACP compatibility adapters must not promote a provider-side terminal failure
  into that final Agent message. Codex terminal text delivered as an
  unattributed `agent_message_chunk` (no `messageId`) is structured Provider
  error evidence even when the adapter subsequently returns `end_turn`. Some
  adapter versions attach a `messageId` to the same account, capacity, rate,
  or upstream failure; those known terminal error forms must be normalized the
  same way before `end_turn` can synthesize a final Agent message.
- After eligibility is confirmed, `continue_turn` materializes the exact
  DB-current runtime through an internal `BackgroundWorker` lifecycle lease and
  holds that lease through prompt completion. A missing, swept, or crashed
  in-memory attachment may be restored from the current `RuntimeBinding`; that
  lifecycle operation may advance the binding's activation generation through
  its normal CAS before turn admission.
- After materialization, `continue_turn` rereads the current `RuntimeBinding`,
  activation generation, effective selection, and committed ACP attachment.
  Missing or mismatched authority fails closed; continuation must not choose a
  different binding, Agent, Profile, or Model, and it must not dispatch against
  the pre-materialization execution fence.

### 4. Validation & Error Matrix

- Missing session id or unknown session -> `validation/session_not_found`.
- The latest turn ended normally, or the session state is not `idle`/`error` ->
  `conflict/agent_continue_requires_incomplete_turn`.
- Session is already `running` ->
  `conflict/agent_continue_requires_incomplete_turn` from the public continue
  guard, not a hidden second turn.
- Imported read-only session -> `capability/imported_session_read_only`.
- Provider fails during continuation -> append a recoverable timeline `error`,
  keep session state `error`, return the structured provider error.
- Codex returns an unattributed terminal message plus `end_turn` -> return a
  structured Provider error and leave no final Agent message that could suppress
  continuation.
- Lifecycle materialization cannot restore the DB-current RuntimeBinding ->
  return its structured runtime/process error and do not send continuation
  input.
- Current RuntimeBinding or committed attachment changes after materialization
  but before prompt admission -> a bounded execution-fence conflict; do not send
  continuation input.

### 5. Good/Base/Bad Cases

- Good: a provider stream fails, the session enters `error`, the user clicks
  Continue, the provider receives backend-owned continuation input, and the
  timeline shows only provider/agent follow-up items without a new user bubble.
- Good: the failed attachment crashed or was swept before Continue; lifecycle
  materialization restores the exact durable binding, advances its activation
  generation if needed, and the manager derives the prompt fence from the fresh
  durable state while the worker lease protects the turn.
- Base: the user manually types a visible follow-up after an error; this uses
  `send_message`, appends a normal `user_message`, and also transitions the
  session through `running`.
- Bad: React calls `agent_send_message` with `"continue"` and locally hides the
  optimistic bubble while the backend still persists a hidden-looking
  `user_message`.

### 6. Tests Required

- Manager unit test: failed send moves the session to `error`, `continue_turn`
  sends provider input, returns provider items, moves the session to `idle`,
  and does not append a `TimelineItemKind::UserMessage`.
- Manager/ACP test: a failed turn retains its current durable binding;
  `continue_turn` sends only through the exact committed attachment produced by
  lifecycle materialization and rejects a stale fence or route failover.
- Manager unit test: an eligible `continue_turn` materializes the runtime before
  prompt dispatch. ACP lifecycle coverage proves a swept/crashed attachment is
  restored only from the DB-current binding, and the internal worker lease
  protects it until the turn returns.
- ACP runtime test: a capacity failure delivered as unattributed Codex text plus
  `end_turn` remains a retryable Provider error and emits no Agent delta/final
  message.
- Manager unit test: `continue_turn` rejects an idle session with
  an explicit final Agent message with
  `agent_continue_requires_incomplete_turn`, and accepts an idle session whose
  latest segment has no final Agent message far enough to reach normal runtime
  validation.
- Frontend typecheck: generated `ContinueAgentTurnRequest` is consumed through
  the typed API wrapper and hook.
- Remote/router check: `RemoteAgentRequest::ContinueTurn` dispatches through
  the same manager method and uses `MutateAgentSession` authorization.

### 7. Wrong vs Correct

#### Wrong

```typescript
api.agentSendMessage({
  sessionId,
  text: "continue",
  attachments: [],
  correlationId: null
});
// Then locally hide the optimistic user bubble.
```

#### Correct

```typescript
api.agentContinueTurn({
  sessionId,
  correlationId: null
});
```

## Timeline Model

Use append-only, sequence-numbered timeline events as the canonical stream.
Supported event classes include:

- User messages.
- Agent message delta and final chunks.
- Reasoning or thought streams.
- Plan and todo updates.
- Tool call started, progress, completed, and failed.
- Command execution.
- File read, write, edit, delete, and move.
- Git diff updates.
- MCP tool calls.
- Web search events.
- Permission requested and resolved.
- Compact or context boundary events.
- Subagent/task start, update, and result events.
- Error, warning, and system notices.

Every live event sent to clients must include a monotonic session sequence. On
reconnect, clients first fetch the authoritative timeline from the last known
sequence and only then apply live events.

`FetchTimelineRequest.afterSequence = null` means "latest bounded window", not
"complete history". Session-detail restore, app restart, and remote reconnect
flows must not treat a latest window as a complete conversation because long
provider turns can contain hundreds of streaming delta rows. Complete timeline
restore must page forward from `afterSequence = 0`, or from the `endSequence` of
a cache known to have `hasOlder = false` and `startSequence <= 1`, until
`hasNewer = false`.

## Timeline Attach and Replay

Live timeline subscriptions use an explicit attach flow, not an implicit
"connect and hope" stream. A client attaches with:

- Stable Vibex session id.
- `subscription_id` for the UI subscription.
- `connection_id` for the current socket/device connection.
- `since_sequence` from the newest authoritative item the client has applied.

The service must respond with either:

- A bounded replay from `since_sequence + 1`, followed by live events.
- A fresh snapshot when the gap is too large, history was compacted, or the
  client has no reliable sequence.

Provider adapters never assign client-visible sequence numbers. The timeline
repository assigns them transactionally when events become authoritative.
Clients may render optimistic and streaming items, but those items must be
reconciled or discarded against persisted timeline rows.

## Scenario: ACP Canonical Event Normalization And Raw Extensions

### 1. Scope / Trigger

- Trigger: an ACP live `session/update` or adapter transcript record needs to
  become an authoritative provider-neutral Timeline event.
- `crates/agent-acp/src/events.rs` owns semantic normalization. Runtime,
  transcript import, and UI code must not independently parse provider tool
  kind strings.

### 2. Signatures

```text
AgentEventInput { source, compatibilityIdentity, nativeEventId, toolName,
  title, status, rawInput, outputSummary, rawOutput, content, locations, meta }
AgentEventEnricher::enrich(input) -> Vec<CanonicalAgentEvent>
CanonicalAgentEvent = AgentMessage | Reasoning | Plan | ToolCall |
  CommandExecution | FileOperation | WebSearch | TodoUpdate | Collaboration |
  ImageGeneration | PermissionRequest | SystemNotice
normalize_agent_event(enricherKind, input) -> Vec<NormalizedAgentEvent>
stable_event_correlation_id(identity, nativeEventId, canonicalKind, ordinal)
  -> String
```

`ToolCallPayload`, `CommandPayload`, `FileOperationPayload`, `WebSearchPayload`,
`TodoUpdatePayload`, `CollaborationPayload`, and `ImageGenerationPayload` may
carry `rawExtension?: AgentEventRawExtension`. Absence preserves legacy JSON.

### 3. Contracts

- Exact Compatibility Registry identity selects Claude, Codex, or Passthrough
  enrichment. Codex advanced kinds require explicit structured fields; Claude
  markers without exact-version evidence and every ambiguous payload fall back
  to generic `ToolCall`.
- Correlation ids hash a fixed versioned domain, exact compatibility identity,
  native event id, canonical kind, and ordinal. Live and transcript inputs with
  equivalent native evidence produce the same id; raw native ids are never
  persisted, logged, or included in Debug.
- `AgentEventRawExtension` is schema version 1. It retains at most 16 content
  blocks, 16 locations, and 16 allowlisted meta entries; todo projection keeps
  at most 32 items. Raw input/output are at most 4096 UTF-8 bytes, summaries
  512 bytes, keys 64 bytes, and locations 1024 bytes, including the truncation
  suffix. Constructors and deserialization apply the same bounds;
  unsupported `schemaVersion` values fail closed. Meta keys are canonicalized
  from ACP camelCase (for example `exitCode` -> `exit_code`) before enrichment.
- Live session values, including credential-shaped text, data URLs, prompt/env
  values, warnings, errors, tool input/output, and private paths, are preserved
  in the authoritative Timeline. Keyword detection must never replace them with
  `[redacted]` or `[redacted-sensitive-output]`. The `...(truncated)` form must
  restore internal truncation state after JSON deserialization so bounds and
  round-trip equality remain stable. Debug exposes only counts, keys, modes,
  and presence flags; diagnostics, permission details, provider configuration,
  and credential projections keep their separate redaction contracts.
- Attachment-local tool state merges live updates only after native route and
  four-field current-fence validation. Cumulative output emits `snapshot`, then
  suffix-only `append`; exact duplicates carry no raw output, non-prefix
  replacement emits a new `snapshot`. The merge state stores only cumulative
  byte length and a SHA-256 prefix fingerprint, never a growing output copy;
  terminal status, turn cleanup, detach, replacement, or crash clears it.
- File diff normalization preserves `oldText` / `newText` exactly, including
  empty strings. Operation classification prefers a recognized top-level
  `kind` / `operation`, then recognized `_meta.kind` / `_meta.operation`, then
  the exact tool kind (`write_file`, `edit_file`, `delete_file`). Only when all
  explicit evidence is absent may the Codex enricher infer ACP v1 lifecycle:
  missing `oldText` with present `newText` is `Write`; present `oldText` with
  empty or missing `newText` is the legacy Codex deletion fallback. UI and
  persistence code consume `FileOperationKind` and never repeat this inference.
- The Agent manager treats an event whose provider correlation, source,
  redaction state, and canonical payload exactly match the latest streamed
  snapshot for the current turn as a no-op before opening a Timeline write or
  broadcasting a live update. A status transition or a non-empty output
  snapshot/append remains a distinct event. Desktop projections of process
  cards must omit lossless file snapshots and raw extensions that the card
  renderer does not consume.
- Permission requests remain on the typed permission path. Unknown extensions
  may become a bounded generic tool event or diagnostic but cannot bypass the
  permission gate or create provider-specific public Timeline variants.

### 4. Validation & Error Matrix

- Missing/unknown/stale/prepared native route -> quarantine or bounded process
  diagnostic; zero Timeline, permission, config, or tool-state side effects.
- Unknown enricher identity or incomplete advanced marker -> generic
  `ToolCall`; do not infer Command/File/Web/Todo/Collaboration/Image semantics.
- Credential-shaped, data-URL, prompt/env, or private-path session value ->
  preserve it subject only to the field's UTF-8-safe size bound.
- Unsupported raw extension schema version -> deserialization error; a
  bounded extension remains durable with its truncation state.
- Over-limit string, collection, or output -> UTF-8-safe deterministic
  truncation; malformed optional structures -> bounded generic fallback.
- Unknown file `kind` with recognized `_meta.kind` -> use the recognized meta
  operation; if neither source is recognized, use the exact tool hint or the
  bounded ACP v1 lifecycle fallback.
- Repeated cumulative output -> no duplicate raw append; output that replaces
  rather than extends the prior snapshot -> explicit `snapshot`.

### 5. Good/Base/Bad Cases

- Good: the same Codex command fixture normalizes live and transcript records
  to equal `CommandExecution` events and equal hashed correlation ids.
- Good: Codex ACP diff content with `_meta.kind = add | update | delete`
  produces `Write | Edit | Delete`, and a provider's explicit `update` remains
  `Edit` even when `newText` is empty.
- Base: an unmanaged tool with safe bounded raw evidence stays `ToolCall` and
  remains readable when old records omit `rawExtension`.
- Bad: matching `command` by title substring, persisting a native tool id,
  copying the complete cumulative output into every event, or rendering raw
  provider JSON directly in the UI.

### 6. Tests Required

- Core serde tests cover all additive Timeline variants, legacy JSON, exact
  credential/private-path preservation, UTF-8 byte bounds, collection bounds,
  truncation-state round-trip, and payload-free Debug.
- Enricher tests cover all twelve variants, exact identity dispatch, ambiguous
  fallback, stable/non-colliding hashes, structured command/file batch/web/
  todo/collaboration/image classification, ACP v1 file lifecycle inference,
  `_meta.kind` precedence, empty-text preservation, exact file-tool hints, and
  Passthrough non-fabrication.
- Runtime tests cover route/fence ordering, attachment isolation, snapshot /
  append / duplicate / replacement behavior, and state cleanup on completion,
  failure, replacement, detach, and crash. They also assert credential-shaped
  warnings, errors, tool summaries, and raw output reach Timeline payloads
  without redaction placeholders.
- Manager tests cover exact streamed-snapshot suppression, preservation of
  status/output transitions, and bounded desktop projection of lossless file
  payloads.
- Golden tests replay every live/transcript case twice, compare both against
  expected canonical Timeline JSON, verify P1 native baseline references, and
  scan sanitized fixtures and debug output for ids, secrets, and private paths
  while asserting that live serialized Timeline content remains lossless.

### 7. Wrong vs Correct

#### Wrong

```text
session/update -> UI/provider-specific kind parsing -> generic title/summary
              -> persist raw native id and full cumulative output
```

#### Correct

```text
session/update -> exact native route + current fence
  -> attachment-local merge -> exact-identity AgentEventEnricher
  -> top-level/meta/tool file operation evidence -> bounded lifecycle fallback
  -> bounded lossless canonical event + stable hashed correlation id
  -> provider-neutral Timeline -> shared desktop/remote rendering
```

## Delegation, Team, and Automation Events

Multi-agent delegation, Team Mode, and scheduled automation are higher-level
workflows over the same session protocol. Model them as provider-neutral
timeline events such as child session started, child progress, child result,
team mailbox message, team task update, automation run started, automation run
completed, and automation run failed.

Do not introduce a second UI-only event bus for delegated agents or scheduled
runs. Child sessions may have their own timeline, but parent sessions need a
durable reference and summary event so mobile clients can reconstruct the
workflow after reconnect.

## MCP Sidecar Tools

Optional MCP sidecars may expose tools such as `delegate_to_agent`,
`check_user_feedback`, `ask_user_question`, and `get_session_info` to native
Agent CLIs. Those tools must call back into Vibex `AgentManager`, permission
handling, and timeline storage. They must not mutate provider-native state
directly or bypass provider-neutral permission records.

Missing or failed sidecars disable only sidecar-backed collaboration features.
Normal single-session Agent operation must continue with a capability warning.

## Provider Adapter Contracts

- `AgentManager::register_runtime` accepts only an exact ACP
  `AgentRuntimeRouteKey` and an `AgentProvider` whose kind is ACP. One active
  route is allowed per concrete Agent; missing or non-ACP routes fail closed.
- `crates/agent-acp` is the only online adapter boundary. Treat negotiated and
  compatibility-scoped capability evidence as authoritative.
- Model command, args, env, cwd template, models, modes, and disabled tools as
  ACP Provider configuration, not UI-specific settings or Native SDK options.
- On Linux, every ACP adapter, probe, and managed-install child spawned from an
  AppImage must remove launcher markers and package-only runtime paths from the
  inherited environment. Preserve host path-list entries in order, and apply
  explicit Provider environment overlays after sanitation so a configured
  value remains authoritative. In particular, AppImage `PYTHONHOME` and
  `PYTHONPATH` must never prevent host Python from importing its standard
  library in an Agent shell.
- ACP-native handles live in durable `RuntimeBinding` records and in-memory
  attachment state. Adapter-local `ProviderBinding` values may be synthesized
  from that exact current fence, but are never persisted as session authority.
- `crates/agent-claude` and `crates/agent-codex` own only read-only transcript
  import, pure-Serde parity replay, and fixture sanitization. They must not
  implement `AgentProvider`, depend on Native SDK crates, or provide online
  smoke binaries.
- Incoming ACP `session/request_permission` requests must be converted at the
  adapter boundary into provider-neutral `PermissionRequest` timeline events
  with bounded/redacted titles and details. Do not persist raw ACP permission
  payloads, full tool inputs, prompts, terminal logs, env values, or tokens.
- If the adapter has not implemented a full provider-side approval callback
  lifecycle, it may conservatively cancel/deny the native ACP request only after
  recording the provider-neutral permission event and returning an incomplete
  turn so the Agent session can move to `needs_input`.
- Backend runtime gates must check provider capabilities before mutating
  permission state or attempting unsupported operations such as interrupt.
  Unsupported ACP operations return `capability/*_unsupported` errors instead
  of writing timeline/permission side effects first.
- Default validation must not start Claude, Codex, OpenCode, or another real
  ACP process. Real managed-Adapter startup belongs to explicit environment and
  login-gated smoke tasks.
- Unit tests for the ACP process environment boundary must cover mixed
  AppImage/host path lists, package-only Python overrides, removed launcher
  markers, and an unrelated inherited sentinel that remains untouched.

## Permissions

Permission requests are timeline events and durable records. They must include:

- The requested action and provider-native request id.
- Human-readable title and details.
- A risk category such as command, file change, patch, network, or custom tool.
- Project/workspace/session context.
- Allowed responses.
- Resolution metadata including responder device and timestamp.

Sensitive files such as `.env`, private keys, token files, and credential stores
default to asking the user even when the provider has a permissive mode.

## Scenario: ACP-only Logical Session Core

### 1. Scope / Trigger

- Trigger: code creates, restores, sends to, commands, interrupts, resolves a
  permission for, or closes an online Logical Session after the ACP-only
  cutover.
- This is a cross-layer contract. Rust serde types in `crates/core` are the
  source of truth; SQLite owns durable runtime authority; frontend code consumes
  those DTOs through the shared Rust Backend contracts.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
agent_list_sessions(include_archived?: bool) -> Vec<AgentSession>
agent_create_session(CreateAgentSessionRequest {
  runtime: SessionRuntimeSelection,
  workspaceRoot, workspaceMode, title?, safety?
}) -> AgentSession
agent_get_session(session_id: VibexSessionId) -> AgentSession
agent_list_runtime_options() -> SessionRuntimeOptionCatalog
agent_get_runtime_selection(session_id) -> AgentSessionRuntimeSelectionState
agent_fetch_timeline(request: FetchTimelineRequest) -> TimelinePage
agent_send_message(request: SendAgentMessageRequest) -> Vec<TimelineItem>
agent_continue_turn(request: ContinueAgentTurnRequest) -> Vec<TimelineItem>
agent_execute_command(request: AgentCommandExecuteRequest) -> AgentCommandExecuteResult
agent_interrupt(session_id: VibexSessionId) -> ()
agent_archive_session(session_id: VibexSessionId) -> ()
agent_delete_session(session_id: VibexSessionId) -> ()
agent_resolve_permission(request: ResolvePermissionRequest) -> TimelineItem
agent_get_capabilities() -> ProviderCapabilitiesResponse

AgentManager::register_runtime(AgentRuntimeRouteKey, Arc<dyn AgentProvider>)
RuntimeSelectionService::initialize_new_session(session_id, desired)
MessageSubmissionCoordinator::submit(SendAgentMessageRequest)
MessageSubmissionCoordinator::replace_user_message(ReplaceUserMessageRequest)
```

Authoritative session/runtime tables:

```text
projects
workspaces
agent_sessions
session_runtime_bindings
runtime_switches
runtime_switch_operations
runtime_switch_events
agent_message_submissions
agent_message_submission_payloads
agent_timeline_items
permission_requests
adapter_diagnostics
```

Repositories:
`WorkspaceRepository`, `SessionRepository`, `AgentSessionRuntimeRepository`,
`RuntimeBindingRepository`, `RuntimeSwitchRepository`,
`MessageSubmissionRepository`, `TimelineRepository`, `PermissionRepository`,
and `AdapterDiagnosticsRepository`.

### 3. Contracts

- `CreateAgentSessionRequest.runtime` is complete and contains the concrete
  Agent, enabled ACP Profile, non-empty Model, and optional explicit
  Effort/Mode. Session creation never derives these fields from ProviderKind.
- Default safety is `workspace_write` with `askOnRisk = true` and
  `bypassAllPermissions = false`. A bypass/all-permissions session must be an
  explicit session setting and visible in durable session/timeline data.
- `AgentSession.id` is the only primary session id crossing UI/API boundaries.
  `AgentSession.agentId` is required. Auth-source/model, binding/generation,
  Adapter, process, and ACP-native ids remain outside the session DTO.
- `AgentSession.updatedAtMs` tracks session state and metadata mutations.
  Sidebar display and ordering use `lastMessageAtMs`, derived from the latest
  persisted timeline item and falling back to `createdAtMs` for an empty
  session. Opening or switching sessions must not change that message time.
- Online registration and dispatch use only exact ACP route keys. A non-ACP
  route/provider is rejected, and a missing exact route has no Native or
  ProviderKind fallback.
- Creation inserts the `Initializing` session, then atomically persists desired
  selection plus a `Requested` initial switch before spawn or `session/new`.
  The normal Prepare/Commit/activate path establishes the first current
  binding; startup reconciliation resumes interrupted initialization.
- Ordinary messages always enter `MessageSubmissionCoordinator`. Continue,
  provider command, permission, interrupt, and close resolve the current
  durable selection/binding/generation and synthesize any adapter-local handle
  from that exact fence.
- Editing the latest completed user message preserves the same logical
  `VibexSessionId`; it never forks, archives, or replaces the product session.
  One immediate database transaction must CAS the expected timeline end,
  verify that the target is the latest user message in an `Idle`/`Error`
  session with no non-terminal submission, delete that user item and the tail,
  and enqueue the edited payload. Completed submissions whose recorded result
  overlaps the removed tail become terminal `superseded_by_edit` records so an
  old idempotency-key retry cannot resend or read the replacement sequence.
- An edited-message submission durably requires `ForceFreshSession`. Dispatch
  may begin only after the stable submission-derived runtime switch commits;
  startup reconciliation resumes the same switch/submission pair after a
  crash. This is the controlled exception to the normal append-only timeline
  rule and prevents stale provider context without changing logical identity.
- Schema v28 intentionally clears old session-scoped data, drops
  `provider_bindings` and `session_provider_bindings`, and removes legacy
  provider columns from `agent_sessions`. There is no compatibility read path.
- `FetchTimelineRequest.afterSequence` supports catch-up. `limit` bounds the
  response. `TimelinePage` includes `startSequence`, `endSequence`, `hasOlder`,
  and `hasNewer`.
- `FetchTimelineRequest.afterSequence = null` returns only a latest bounded
  window and may set `hasOlder = true`; UI code may use it for previews, but
  full conversation restore must use forward pagination so a user/assistant
  turn is not cut in the middle.
- `agent_timeline_items` is append-only except for the atomic latest-user-turn
  replacement above. The repository assigns monotonic per-session sequence
  numbers inside a transaction; replacement reuses the deleted tail sequence
  only inside the same logical session.
- Permission requests are both `permission_requests` rows and
  `permission_request` timeline payloads. Resolutions append a
  `permission_resolution` timeline payload.
- Adapter diagnostics are bounded/redacted and must not become user-visible
  authoritative timeline content.

### 4. Validation & Error Matrix

- Missing session id -> `validation/session_not_found`.
- Empty Model or non-ACP/wrong-Agent Profile during create ->
  `validation/runtime_selection_model_required` or
  `validation/provider_profile_route_mismatch`.
- Missing exact ACP route -> `capability/provider_unregistered`.
- Missing RuntimeSelectionService during create ->
  `process/runtime_selection_service_unavailable`; no direct create.
- Missing MessageSubmissionCoordinator ->
  `process/message_submission_coordinator_unavailable`; no direct send.
- Missing/stale current binding, selection, attachment, or generation ->
  `conflict/message_submission_runtime_*` or
  `conflict/turn_execution_identity_mismatch`; no provider side effect.
- Empty `SendAgentMessageRequest.text` and empty `attachments` ->
  `validation/empty_agent_message`.
- Send while session state is `running` -> `conflict/agent_turn_already_running`.
- Unsupported ACP operation -> `capability/acp_*_unsupported`.
- Missing Adapter/Agent binary -> `process/acp_*` with bounded diagnostics.
- ACP/Adapter failure -> `provider/acp_*` with redacted diagnostic details.
- DB migration or repository failure -> `storage/*`.
- Permission wait is `needs_input`, not `error`; only failed/expired provider
  behavior should move the session to `error`.

### 5. Good/Base/Bad Cases

- Good: Claude and Codex ACP routes coexist with distinct Agent/Adapter ids;
  each create commits one initial switch/current binding and every turn uses
  only its own current attachment fence.
- Base: an option omits Effort/Mode; the Adapter's converged defaults are
  accepted while the requested Model remains exact.
- Bad: frontend sends ProviderKind as Agent identity, manager calls an external
  create before durable intent, reads a removed legacy binding, or switches to
  another Profile after a fence failure.

### 6. Tests Required

- Core serde tests assert `AgentSession` has only required `agentId` as runtime
  identity and create carries a complete selection.
- State-machine tests for allowed and rejected session transitions.
- DB migration/repository tests assert clean-data cutover, removed legacy
  tables/columns, initial intent atomicity, current binding, timeline sequence,
  permission request/resolve, archive, and delete behavior.
- Message-submission tests assert edited-message CAS failure leaves the
  timeline untouched, successful replacement dispatches once in the original
  session, and idempotent retry neither truncates nor switches twice.
- Manager/ACP tests cover exact route registration, initial side-effect order,
  crash recovery, durable message/command/continue/permission/interrupt fences,
  and fail-closed missing ownership.
- Claude/Codex offline import and pure-Serde parity fixture tests continue to
  map sanitized historical data without Native SDK dependencies.
- Tauri/desktop build and frontend type/lint checks proving UI consumes
  generated protocol types.
- `pnpm smoke:acp:bridge-contract` is the default fixed-Adapter contract gate.
  `pnpm smoke:session:codex` and `pnpm smoke:session:claude` are explicit
  environment/login-gated ACP smokes and must never substitute a Native path.

### 7. Wrong vs Correct

#### Wrong

```typescript
// UI code reads raw provider event details.
if (event.codexThreadItem?.type === "reasoning") {
  renderReasoning(event.codexThreadItem.summary);
}
```

#### Correct

```typescript
// UI code renders the generated Vibex timeline contract.
if (item.payload.type === "reasoning") {
  renderReasoning(item.payload.data.text);
}
```

#### Wrong

```text
agent_timeline_items(session_id = native_claude_session_id, sequence = client_count + 1)
```

#### Correct

```text
agent_timeline_items(session_id = vibex_session_id, sequence = repository_transaction_next_sequence)
```

## Scenario: Agent User Message Attachments

### 1. Scope / Trigger

- Trigger: `SendAgentMessageRequest` carries provider-neutral user attachments
  from desktop/mobile clients through the Agent service and authoritative
  timeline.
- This is a cross-layer contract because Rust DTOs, generated TypeScript
  bindings, desktop composer state, remote callers, optimistic timeline
  reconciliation, and provider adapters must agree on the same payload shape.

### 2. Signatures

Core request shape:

```text
SendAgentMessageRequest {
  session_id: VibexSessionId,
  text: String,
  attachments: Vec<MessageAttachment>,
  correlation_id: Option<CorrelationId>
}

MessageAttachment {
  label: String,
  mime_type: Option<String>,
  uri: Option<String>,
  inline_text_offset: Option<u32>
}
```

Generated TypeScript callers must pass `attachments: MessageAttachment[]`.
Text-only callers use `attachments: []`.

### 3. Contracts

- `attachments` defaults to an empty vector for backward-compatible serde
  decoding, but generated frontend clients should send it explicitly.
- A user message is valid when either `text.trim()` is nonempty or
  `attachments` is nonempty.
- The Agent service persists request attachments into
  `TimelinePayload::UserMessage.attachments` before invoking a provider turn,
  so reconnect/catch-up clients see the same authoritative user payload.
- `MessageAttachment.inline_text_offset` is optional and defaults to `None` for
  backward-compatible decoding. Composer clients set it to the zero-based
  offset in `text` where an inline image token appeared. Timeline renderers use
  it only for presentation ordering; provider adapters must not require it for
  image/file materialization.
- Attachment payloads stay provider-neutral at this boundary. Provider adapters
  must explicitly translate supported image/file attachments into ACP content
  blocks and return capability-aware errors when the Agent cannot consume
  them.
- UI optimistic reconciliation compares both trimmed text and attachment
  metadata so a text-only duplicate does not collapse an attachment-bearing
  message.

### 4. Validation & Error Matrix

- Empty text and no attachments -> `validation/empty_agent_message`.
- Attachment-only user message -> accepted and persisted as a user timeline
  item.
- Provider lacks image/file input support -> adapter-level capability error or
  documented fallback, not silent timeline data loss.
- Missing `inline_text_offset` on older timeline rows -> render the attachment
  in the legacy tail position instead of rejecting the row.
- Rust serialization contract drift -> core protocol test failure.

### 5. Good/Base/Bad Cases

- Good: a desktop composer image token sends `text` plus ordered
  `MessageAttachment` records, and the timeline preserves those attachments
  after refetch.
- Good: `这个 [image] 是什么？` sends text with one placeholder spacing point
  and `inline_text_offset` so history renders the image token between `这个`
  and `是什么？`, not below the message.
- Base: scheduled tasks, automation prompts, Web remote text composers, smoke
  binaries, and tests keep using `attachments: []`.
- Bad: `AgentManager::send_message` rejects attachment-only messages because
  text is empty, or appends a user timeline item with `attachments: []` after
  receiving nonempty request attachments.
- Bad: desktop history ignores `inline_text_offset` and renders all image
  attachments after the message body.

### 6. Tests Required

- Binding generation and drift check after changing `SendAgentMessageRequest`.
- Agent service test for text-plus-attachments and attachment-only validation.
- Frontend typecheck for desktop/Web callers proving every request provides
  `attachments`.
- Timeline optimistic merge test or focused review that attachment metadata
  participates in duplicate reconciliation, including `inline_text_offset`.

### 7. Wrong vs Correct

#### Wrong

```rust
if request.text.trim().is_empty() {
    return Err(empty_agent_message());
}

TimelinePayload::UserMessage(UserMessagePayload {
    text: request.text.clone(),
    attachments: Vec::new(),
})
```

#### Correct

```rust
if request.text.trim().is_empty() && request.attachments.is_empty() {
    return Err(empty_agent_message());
}

TimelinePayload::UserMessage(UserMessagePayload {
    text: request.text.clone(),
    attachments: request.attachments.clone(),
})
```

## Scenario: Phase 8 Scheduled Task Runtime

### 1. Scope / Trigger

- Trigger: Phase 8 adds a local scheduled-task runner that executes persisted
  scheduled prompts through Vibex Agent sessions.
- The runner belongs in the provider-neutral Agent runtime boundary. It must
  not call Codex, Claude, ACP, or other provider SDKs directly.

### 2. Signatures

Runtime API in `crates/agent`:

```text
ScheduledTaskRunner::new(&AgentManager)
ScheduledTaskRunner::tick(now_ms) -> ScheduledTaskTickResult
ScheduledTaskRunner::recover_stale_runs(now_ms) -> Vec<ScheduledTaskRun>
next_run_after(schedule, completed_due_at_ms, completed_at_ms) -> Option<i64>
```

DB repository helpers:

```text
ScheduledTaskRepository::list_due(conn, now_ms, limit)
ScheduledTaskRepository::claim_due(conn, task_id, now_ms)
ScheduledTaskRepository::mark_task_after_run(conn, task_id, status, next_run_at_ms, now_ms)
ScheduledTaskRepository::list_stale_running_runs(conn, before_ms, limit)
```

### 3. Contracts

- `tick(now_ms)` is explicit and test-driven. It must not spawn a background
  thread, start a timer, or depend on wall-clock time internally.
- Due tasks are active, non-deleted scheduled tasks with
  `next_run_at_ms <= now_ms`.
- Claiming a task creates a `running` run and clears `next_run_at_ms` before
  provider execution, so repeated ticks do not duplicate local one-shot work.
- Execution uses `AgentManager::create_session` followed by
  `AgentManager::send_message`. The scheduled task run stores only the Vibex
  `session_id`, not provider-native ids.
- If `send_message` leaves the session in `needs_input`, the scheduled run is
  recorded as skipped or failed with a bounded permission/input diagnostic. The
  scheduler must not auto-approve permissions.
- One-shot schedules clear `next_run_at_ms` and pause after a run. Interval and
  daily schedules compute the next due timestamp through a pure function.
- Stale `running` runs are recovered to a non-running status and task state is
  advanced or paused according to the schedule.

### 4. Validation & Error Matrix

- No registered provider -> `capability/provider_unregistered`, stored on the
  run as a failed scheduled execution.
- Provider turn failure -> provider error code/message copied into the run with
  redacted diagnostics only.
- Permission required / `needs_input` -> skipped or failed run with
  `scheduler/permission_required`; session may remain `needs_input` for user
  review.
- Unsupported daily timezone -> `validation/scheduler/unsupported_timezone`;
  task is paused rather than repeatedly retried in a tight loop.
- Stale running run -> failed run with `scheduler/recovered_stale_run`.

### 5. Good/Base/Bad Cases

- Good: a one-shot mock scheduled task creates one Vibex session, sends the
  stored prompt, records one succeeded run, and no repeated tick duplicates it.
- Base: an interval task advances `next_run_at_ms` after success or failure so
  the scheduler does not immediately rerun the same due timestamp.
- Base: a permission-requesting mock prompt records a skipped scheduled run and
  leaves the session in `needs_input`.
- Bad: scheduler calls an ACP Adapter/runtime directly instead of creating and
  sending through the durable Logical Session services.
- Bad: scheduler stores Claude/Codex native ids or raw provider payloads in the
  scheduled task/run tables.

### 6. Tests Required

- Pure next-run tests for one-shot, interval, daily, and unsupported timezone.
- DB tests for due listing, claim dedupe, task post-run update, and stale
  running run listing.
- Agent tests using `MockAgentProvider` for one-shot success, repeated tick
  dedupe, interval next-run update, provider failure, permission skip, and
  stale recovery.
- Default checks must not start real Codex, Claude, OpenCode, or ACP processes.

### 7. Wrong vs Correct

#### Wrong

```rust
provider.send_turn(native_handle, scheduled_task.prompt).await?;
```

This bypasses Vibex session creation, timeline persistence, durable runtime
selection/binding, and permission semantics.

#### Correct

```rust
let session = manager.create_session(request).await?;
manager.send_message(SendAgentMessageRequest {
    session_id: session.id,
    text: scheduled_task.prompt,
    attachments: Vec::new(),
    correlation_id: None,
}).await?;
```

The scheduled run goes through the same provider-neutral Agent boundary as a
manual user turn.

## Scenario: Phase 7 External Session Import Foundation

### 1. Scope / Trigger

- Trigger: Phase 7 adds provider-neutral external Claude/Codex session import
  contracts and storage/service foundation.
- Provider-specific native history discovery/parsing is outside this foundation.
  Import callers pass normalized `ExternalSessionImportCandidate` records.

### 2. Signatures

Core protocol types generated from `crates/core`:

```text
ExternalSessionImportPreviewRequest
ExternalSessionImportPreview
ExternalSessionImportCandidate
ExternalSessionImportRequest
ExternalSessionImportResult
ExternalSessionContinuationStatus = resumable | read_only
ExternalSessionImportCandidateStatus = importable | partial | blocked
```

Agent service boundary:

```text
AgentManager::import_external_sessions(ExternalSessionImportRequest)
  -> ExternalSessionImportResult
```

### 3. Contracts

- Imported sessions receive new Vibex-owned `AgentSession.id` values and a
  concrete `agent_id`, but no current `RuntimeBinding` or desired/effective
  runtime selection. Native ids in the import candidate are bounded provenance
  evidence only and are not copied into online runtime authority.
- Offline parsers may mark a candidate `resumable` only when they recover one
  stable historical handle (`nativeThreadId` for Codex or `nativeSessionId` for
  Claude). After the ACP-only cutover, importing that candidate is still a
  read-only transcript operation until a future explicit ACP materialization
  contract creates a durable selection and binding.
- Import notices/diagnostics may include bounded values for `importSource`,
  `nativeHistoryImported`, `nativeHistoryImportVersion`,
  `importContinuationStatus`, and `importContinuationReason`; they never make
  those fields online route inputs.
- Imported timeline rows are normalized `TimelinePayload` records and must be
  appended through `TimelineRepository` sequence assignment. The import service
  prepends a system notice so imported sessions are visibly distinguishable from
  live Vibex-created sessions.
- Online operations reject transcript-only sessions before appending a user
  item or starting an Adapter because authoritative runtime selection/binding
  state is absent. They must not synthesize state from import provenance.

### 4. Validation & Error Matrix

- Candidate not marked `importable` ->
  `validation/external_session_import_candidate_not_importable`.
- Candidate source/provider mismatch ->
  `validation/external_session_import_provider_mismatch`.
- Resumable candidate missing the provider-specific native handle ->
  `validation/external_session_import_resumable_handle_missing`.
- Transcript-only imported session send -> bounded missing runtime
  selection/binding error; zero Timeline or Adapter side effects.

### 5. Good/Base/Bad Cases

- Good: a normalized Codex candidate with `nativeThreadId` may report
  `resumable` evidence in preview, imports its sanitized Timeline into a new
  Logical Session, and remains read-only because no ACP RuntimeBinding exists.
- Base: a normalized candidate without a stable resume handle imports as
  `read_only`, remains listable/fetchable, and preserves imported timeline
  content.
- Bad: import code reads real native Claude/Codex history files in deterministic
  tests, exposes raw native payloads in generated protocol types, or silently
  starts an unrelated native session for read-only history.

### 6. Tests Required

- Core serde/binding tests for external import DTOs.
- Agent service tests for resumable provenance, absence of RuntimeBinding,
  fail-closed online operations, imported timeline ordering, and list/fetch
  through existing session APIs.
- Default checks must not require real Claude/Codex auth, CLIs, or native
  session files.

### 7. Codex JSONL Import Boundary

- Codex-native JSONL discovery/parsing belongs in the Codex adapter crate, not
  in core DTOs, UI code, remote APIs, or shared Agent storage.
- The parser may decode only recognized bounded fields from native JSONL:
  `session_meta.payload.id` for `nativeThreadId`, safe workspace metadata, and
  supported message/reasoning content that becomes explicit imported timeline
  items.
- Codex imports are `resumable` only when exactly one stable native thread id is
  recovered. Missing or ambiguous native ids must produce `read_only` candidates
  with bounded reasons such as `missing_native_thread_id` or
  `ambiguous_native_thread_id`.
- Malformed and unsupported JSONL records must produce bounded diagnostics with
  metadata such as line number, record type, item type, role, or hashed path.
  Diagnostics must not include raw JSONL, prompt text, tool payloads, terminal
  output, environment values, tokens, or native config blobs.
- Selected Codex import must delegate normalized candidates to
  `AgentManager::import_external_sessions`; provider-specific import code must
  not duplicate database writes or create a second session storage path.

### 8. Claude JSONL Import Boundary

- Claude-native JSONL discovery/parsing belongs in the Claude adapter crate,
  not in core DTOs, UI code, remote APIs, or shared Agent storage.
- The parser may decode only recognized bounded fields from native JSONL:
  top-level `sessionId` for `nativeSessionId`, safe workspace metadata such as
  `cwd`, and supported `message.role` / `message.content` blocks that become
  explicit imported timeline items.
- Claude imports are `resumable` only when exactly one stable native session id
  is recovered. Missing or ambiguous native ids must produce `read_only`
  candidates with bounded reasons such as `missing_native_session_id` or
  `ambiguous_native_session_id`.
- Malformed and unsupported JSONL records must produce bounded diagnostics with
  metadata such as line number, record type, item type, role, or hashed path.
  Diagnostics must not include raw JSONL, prompt text, tool payloads, terminal
  output, environment values, tokens, or native config blobs.
- Imported Claude user/assistant text may appear only as explicit imported
  timeline payloads. Tool-use and tool-result payloads should use bounded
  summaries and must not import raw tool input/output by default.
- Selected Claude import must delegate normalized candidates to
  `AgentManager::import_external_sessions`; provider-specific import code must
  not duplicate database writes or create a second session storage path.
- Claude import code must not register an online provider, create a durable
  binding, or call a Native SDK. Any future continuation must enter through an
  explicit ACP selection/switch contract.

## Scenario: Phase 10 Automation Graph Runtime

### 1. Scope / Trigger

- Trigger: Phase 10 adds the first local automation graph runtime on top of
  saved graph definitions, Agent sessions, durable run/run-step records, and
  permission requests.
- This is a cross-layer runtime contract: `crates/core` request DTOs are the API
  source of truth, `crates/agent` owns execution, and `crates/db` owns durable
  run/read-model state.

### 2. Signatures

Core request DTOs:

```text
AutomationRunStartRequest {
  graph_id: AutomationGraphId,
  trigger: AutomationRunTrigger,
  scheduled_task_id: Option<ScheduledTaskId>,
  now_ms: Option<i64>
}
AutomationRunResumeRequest {
  run_id: AutomationRunId,
  now_ms: Option<i64>
}
AutomationRunCancelRequest {
  run_id: AutomationRunId,
  now_ms: Option<i64>,
  reason: Option<String>
}
```

Tauri commands:

```text
automation_run_start(request) -> AutomationRun
automation_run_resume(request) -> AutomationRun
automation_run_cancel(request) -> AutomationRun
automation_run_list(request: AutomationRunListRequest) -> Vec<AutomationRun>
automation_run_step_list(request: AutomationRunStepListRequest) -> Vec<AutomationRunStep>
```

Runtime surface:

```text
AutomationGraphRunner::new(&AgentManager)
AutomationGraphRunner::start_graph(request) -> AutomationRun
AutomationGraphRunner::resume_run(request) -> AutomationRun
AutomationGraphRunner::cancel_run(request) -> AutomationRun
AutomationGraphRunner::recover_stale_runs(now_ms) -> Vec<AutomationRun>
```

### 3. Contracts

- `AutomationGraphRunner` must load and mutate automation state only through
  `AutomationGraphRepository`; it must not issue ad hoc SQL updates against
  graph, run, or run-step tables.
- `agent_prompt` nodes create an Agent session and call
  `AgentManager::send_message`; run steps may store Vibex session ids and
  permission request ids, but never provider-native ids, resume tokens, or raw
  provider payloads.
- `approval_gate` nodes create a provider-neutral `PermissionRequest` through
  `AgentManager` so the wait is visible in the Agent session timeline and
  durable permission table.
- Run diagnostics use bounded `RedactedDiagnostic` values. They must not copy
  prompt bodies, terminal output, raw file contents, raw Git diffs, env values,
  secrets, native provider ids, or raw logs.
- Default automation runtime tests and checks must use mock providers and must
  not start Claude, Codex, OpenCode, ACP processes, public Relay, hosted
  services, physical mobile flows, terminal commands, or real file/Git actions.
- `file_check`, `git_check`, and `terminal_check` remain unsupported unless a
  later task adds separately tested safe read-only runtimes.

### 4. Validation & Error Matrix

- Missing graph id -> `automation/graph_not_found`.
- Deleted graph -> `automation/graph_deleted`.
- Paused graph on new start -> `automation/graph_not_active`.
- Empty graph -> `automation/empty_graph`.
- Cyclic graph -> `automation/cyclic_graph`.
- Nonempty edge expression -> `automation/unsupported_edge_expression`.
- Unsupported executable node kind -> `automation/unsupported_node_kind`.
- Resume a non-waiting run -> `automation_run_not_waiting_for_approval`.
- Resume with pending permission -> keep run `waiting_for_approval`.
- Resume with denied, expired, or missing permission -> fail the step/run with
  bounded diagnostics and do not traverse unsafe outgoing edges.
- Recover stale `running` run/step -> fail or mark recovered with
  `automation/recovered_stale_run`; do not silently replay dangerous actions.

### 5. Good/Base/Bad Cases

- Good: a mock `agent_prompt` graph creates a run and step, creates an Agent
  session, sends the prompt through `AgentManager`, and completes with
  `succeeded` without storing native provider ids.
- Base: a mock permission request or `approval_gate` moves run and step to
  `waiting_for_approval`; after approval, explicit resume continues the graph;
  after denial, the graph stops safely.
- Bad: an automation runtime directly starts an ACP Adapter process in a
  default test, evaluates edge expressions as code, stores a prompt copy in
  diagnostics, or executes terminal/file/Git actions under a generic node.

### 6. Tests Required

- Core serde tests for runtime request DTOs.
- DB repository tests for run/run-step persistence, bounded diagnostics, and
  lookup/list helpers.
- Agent runtime tests for mock success, mock provider error, permission wait,
  approval approve/deny resume behavior, unsupported node handling, cycle or
  expression rejection, and stale recovery.
- Desktop compile/tests proving the UI uses canonical Rust run and run-step types.
- `git diff --check` and Trellis task validation before task completion.

### 7. Wrong vs Correct

#### Wrong

```rust
// Runtime bypasses repositories and stores provider-native detail.
conn.execute("UPDATE automation_graph_runs SET error_message = ?1", [raw_prompt])?;
```

#### Correct

```rust
// Runtime uses repository helpers and bounded diagnostics.
AutomationGraphRepository::update_run(&conn, AutomationRunUpdateRequest {
    id: run.id,
    status: Some(AutomationRunStatus::Failed),
    redacted_diagnostics: Some(vec![bounded_diagnostic]),
    ..Default::default()
})?;
```

## Anti-Patterns

- Do not render separate Codex and Claude event models in frontend code.
- Do not store only native provider history and reconstruct Vibex events lazily.
- Do not assume streaming order without sequence numbers.
- Do not continue a turn after an approval response unless the response maps to
  the provider-native approval id.

## Scenario: Agent Turn Claim And Timeline Append Concurrency

### 1. Scope / Trigger

- Trigger: Any change that starts, continues, retries, executes a slash command,
  schedules, or remotely dispatches an Agent turn for an existing Vibex session.
- This is a storage and session-state contract because multiple UI events,
  remote clients, or background runners can call the same manager concurrently.

### 2. Signatures

Repository helpers:

```text
SessionRepository::claim_running_turn(conn, session_id, expected_state) -> ()
TimelineRepository::append(conn, session_id, source, payload, correlation_id,
  provider_correlation_id, redaction_state) -> TimelineItem
TimelineRepository::upsert_by_provider_correlation(conn, session_id, source,
  payload, provider_correlation_id, after_sequence, redaction_state) -> TimelineItem
```

Manager entry points:

```text
AgentManager::send_message(SendAgentMessageRequest) -> Vec<TimelineItem>
AgentManager::continue_turn(ContinueAgentTurnRequest) -> Vec<TimelineItem>
AgentManager::execute_command(AgentCommandExecuteRequest) -> AgentCommandExecuteResult
```

### 3. Contracts

- Every turn entry point must flow through `AgentManager::run_agent_turn` or an
  equivalent path that atomically claims the session before provider execution.
- Claiming a turn must update `agent_sessions.state` from the state observed by
  the caller to `running` with a single conditional update:
  `WHERE session_id = ? AND state = ? AND deleted_at_ms IS NULL`.
- A caller that reads `idle` or `error` but loses the conditional update race
  must receive `conflict/agent_turn_already_running`; it must not append a
  user message, startup notice, provider event, or begin ACP Adapter work.
- `TimelineRepository::append` owns sequence allocation and insertion inside
  one write transaction. Callers must not precompute sequence numbers or append
  timeline rows with ad hoc SQL.
- Provider retry/progress events that are user-visible updates to the same
  condition, such as Codex stream `Reconnecting... x/5`, may be coalesced only
  through a repository helper keyed by `provider_correlation_id`, event kind,
  source, and the current turn's start sequence. The manager must broadcast the
  same sequence so clients replace the visible row instead of appending
  duplicates, while later turns get their own retry/progress item.
- Timeline insert failures must include bounded diagnostics such as session id,
  allocated sequence, item kind, source, and the SQLite error string. Do not
  include prompt text, tool payloads, terminal output, env values, or secrets.
- Startup notices are Vibex-owned timeline events. Seeing duplicate startup
  notices for a single visible user turn is evidence of a turn-claim bug unless
  there are multiple explicit user turns.

### 4. Validation & Error Matrix

- Session is already `running` -> `conflict/agent_turn_already_running`.
- Conditional claim updates zero rows after a previously valid state read ->
  `conflict/agent_turn_already_running`.
- Invalid state transition before claim ->
  `conflict/invalid_session_state_transition`.
- Timeline transaction begin or commit fails -> `storage/timeline_*` with
  bounded diagnostics.
- Timeline insert fails -> `storage/timeline_insert_failed` with bounded
  SQLite, session, sequence, kind, and source diagnostics.

### 5. Good/Base/Bad Cases

- Good: two rapid `agent_send_message` calls for the same idle session result
  in exactly one provider turn; the loser receives `agent_turn_already_running`
  and no second user timeline item is persisted.
- Base: a failed session can be continued once by `continue_turn`; a second
  simultaneous continue attempt is rejected by the same claim path.
- Bad: manager code reads the session state, checks `state != running`, then
  performs an unconditional `UPDATE agent_sessions SET state = running`.
- Bad: provider-stream handlers open separate connections and insert timeline
  items without the repository write transaction.

### 6. Tests Required

- Manager unit test with a blocking provider: first send reaches provider,
  second same-session send returns `agent_turn_already_running`, and provider
  turn count remains one.
- DB tests for timeline append must assert monotonically increasing per-session
  sequence numbers through `TimelineRepository::append`.
- Provider adapter tests that emit live and completed events must continue to
  pass through manager append paths; no adapter may assign authoritative
  sequence numbers.
- Regression tests for coalesced provider retry/progress events must assert
  both the manager return value and fetched authoritative timeline contain one
  updated item with the latest payload.

### 7. Wrong vs Correct

#### Wrong

```rust
let session = SessionRepository::get(&conn, &session_id)?.unwrap();
if session.state != AgentSessionState::Running {
    SessionRepository::update_state(&conn, &session_id, AgentSessionState::Running)?;
}
```

Two concurrent callers can both read `idle`, both update to `running`, and then
race while assigning timeline sequence numbers.

#### Correct

```rust
let session = SessionRepository::get(&conn, &session_id)?.unwrap();
validate_transition(session.state, AgentSessionState::Running)?;
SessionRepository::claim_running_turn(&conn, &session_id, session.state)?;
```

The conditional update is the durable claim. Only the winner may append turn
timeline items or call the provider.

## Scenario: Phase 6 Generic ACP Adapter Foundation

### 1. Scope / Trigger

- Trigger: Phase 6 adds `crates/agent-acp` as the generic ACP adapter
  foundation.
- This child establishes the ACP adapter boundary and deterministic event
  mapping only. Provider catalog UI, dynamic capability probing, and real
  OpenCode smoke evidence are later Phase 6 children.

### 2. Signatures

Rust adapter surface:

```text
AcpAgentProvider: AgentProvider
AcpClient::create_session(AcpCreateSessionRequest) -> AcpSession
AcpClient::resume_session(ProviderBinding) -> AcpSession
AcpClient::send_turn(AcpSendTurnRequest) -> AcpTurn
AcpClient::resolve_permission(AcpPermissionResolution) -> ()
```

Workspace package:

```text
crates/agent-acp
```

### 3. Contracts

- `crates/agent-acp` owns ACP-specific event/client mapping. `crates/agent`
  remains provider-neutral and must not depend on ACP protocol or CLI
  dependencies.
- `crates/agent-acp` depends directly on the official
  `agent-client-protocol-schema = 1.6.0` crate and uses its explicit `v1`
  module for stable outbound request types. Do not reintroduce `sacp` as an
  indirect schema source.
- Schema upgrades preserve Vibex's frozen wire contract:
  `initialize.clientCapabilities` retains the adapter extension keys `auth`,
  `mcpServers`, and plain `meta`; session MCP descriptors retain the Vibex
  shape; unknown prompt content blocks and tolerant inbound raw envelopes are
  never discarded.
- ACP is registered as an `AgentProvider` under an exact ACP
  `AgentRuntimeRouteKey`; the manager continues to own Vibex session state,
  durable RuntimeBinding authority, timeline append, permission persistence,
  and live-event fanout.
- `AcpAgentProvider::capabilities_static()` must stay conservative until the
  later capability-probe child. Do not claim unsupported model lists, dynamic
  modes, permission resolution, interrupt, or tool features just because an
  individual fixture can emit those events.
- ACP-native identifiers may be stored only in the internal durable
  `RuntimeBinding`/attachment boundary. Adapter-local `ProviderBinding` values
  are synthesized from that current fence; secret-like values are omitted from
  persistence and diagnostics.
- ACP output must map into existing `TimelinePayload` variants. Unknown ACP
  event kinds become provider notices or redacted diagnostics, not new public
  timeline variants.
- Permission requests emitted through ACP use the existing `PermissionRequest`
  payload and move sessions to `needs_input` through `AgentManager`.
- For PATH-launched OpenCode in the supported `>=1.17.9, <2.0.0` range (last
  exercised against `1.18.11`), Vibex treats `opencode acp` as stdio
  newline-delimited JSON-RPC 2.0. The minimal executable flow is
  `initialize` with ACP protocol version `1`, `session/new` with
  `{ cwd, mcpServers: [] }`, then `session/prompt`; streamed chunks arrive as
  `session/update` notifications and must be mapped to provider-neutral
  timeline events or redacted diagnostics.
- OpenCode `session/new` may include optional `configOptions`, including a
  `model` category with current value and model options. Vibex may summarize
  those fields in redacted binding metadata and explicit smoke evidence, but
  must not persist the raw `session/new` payload or treat this runtime snapshot
  as the default Provider settings capability source.

### 4. Validation & Error Matrix

- Unregistered ACP provider -> `capability/provider_unregistered`.
- ACP permission resolution not supported by the current foundation ->
  `capability/acp_permission_resolution_unsupported`.
- OpenCode ACP initialize/session/prompt response shape mismatch ->
  `provider/acp_opencode_protocol_mismatch` or
  `provider/acp_opencode_handshake_unsupported` with response-key summaries
  only.
- Missing optional OpenCode model/config fields in `session/new` -> continue
  session creation when `sessionId` is present and record an unavailable
  redacted snapshot.
- ACP provider/process failures -> `provider/acp_*` or `process/acp_*` with
  redacted diagnostics when real process wiring is added.
- Secret-like binding metadata or native resume tokens -> omitted from persisted
  binding metadata/native fields.

### 5. Good/Base/Bad Cases

- Good: a deterministic ACP fixture drives a durable initial switch through
  `AgentManager`, commits a safe RuntimeBinding/attachment fence, emits
  provider-neutral timeline events, and can put the session into `needs_input`
  via a mapped permission request.
- Base: ACP capabilities are present but conservative; UI/runtime can see ACP as
  a Provider kind without assuming real provider support.
- Bad: frontend code branches on raw ACP event payloads; `crates/agent` imports
  ACP-specific protocol crates; raw ACP payload dumps or secrets are stored in
  binding metadata.
- Bad: treating OpenCode `serve` HTTP behavior as evidence that the ACP adapter
  works; Phase 6 ACP smoke success must use `opencode acp`.

### 6. Tests Required

- `cargo test -p vibex-agent-acp` must cover conservative capabilities,
  binding redaction, event mapping, and `AgentManager` fixture integration.
- `cargo test -p vibex-agent` must continue to pass to prove the
  provider-neutral manager and existing Mock behavior are not regressed.
- `pnpm check:rust`, `pnpm check`, and `git diff --check` must pass before the
  task is completed.

## Scenario: ACP Dynamic Agent Authentication

### 1. Scope / Trigger

- Trigger: the Management Center opens an enabled Agent and needs to discover
  the Agent's current authentication choices after `initialize`.
- Trigger: a user submits an Agent, environment-variable, or terminal method,
  or invokes logout when the Agent advertises it.
- This boundary spans ACP initialize/authenticate/logout, Provider Profile
  secret references, the shared PTY host, and generation-fenced GPUI state.

### 2. Signatures

```text
AgentProvider::list_auth_methods(agent_id, provider_profile_id?)
  -> AgentAuthCatalog {
       agent_id, methods[], supports_logout, status, refreshed_at_ms
     }
AgentProvider::authenticate_agent(AgentAuthenticateRequest {
  operation_id, agent_id, provider_profile_id?, method_id
})
  -> AgentAuthenticateResult { method_id, terminal? }
AgentProvider::cancel_agent_authentication(
  AgentAuthenticationCancelRequest { operation_id, agent_id }
) -> bool
AgentProvider::logout_agent(AgentLogoutRequest) -> ()

AgentAuthMethod {
  id, name, description?, kind: agent | environment | terminal,
  environment[], credential_link?
}
AgentAuthEnvironmentVariable {
  name, label?, secret, optional, configured
}
```

The ACP adapter also owns the wire builders:

```text
initialize -> authMethods[] and agentCapabilities.auth.logout
authenticate({ methodId })
logout({})
```

### 3. Contracts

- `AgentAuthCatalog` is rebuilt from the selected Agent's actual
  `initialize.authMethods`; static provider tables may supply defaults for
  command/profile setup but never invent the visible method list. A narrow
  compatibility fallback may append a documented CLI terminal-login method
  when an Agent omits it, but only when both the exact Agent id and configured
  launch shape match. Auggie, Kiro, and Poolside replace their trailing ACP
  mode with `login`; Pi preserves its launcher prefix and appends
  `--terminal-login`. Every fallback preserves the resolved
  command/environment/cwd and remains hidden without a terminal host.
- Method ids remain exact and are bounded, deduplicated, and checked against
  the same initialize result before `authenticate` is sent. Unknown method
  shapes are ignored or conservatively treated as Agent-owned methods by the
  typed ACP schema; malformed entries never reach the UI.
- Agent methods call ACP `authenticate` directly. Environment methods require
  an ACP Provider Profile, save each exact advertised key, then call
  `authenticate` with that Profile's projected environment. Terminal methods
  use the shared PTY host and return a redacted `TerminalAuthActionDescriptor`.
  CodeWhale's verified `codewhale-terminal-auth` method replaces the trailing
  `serve --acp` launch mode with its advertised `auth set --provider ...`
  arguments while preserving any managed launcher prefix such as
  `node <script>`; an unexpected launch shape fails closed instead of appending
  auth arguments to the ACP server command.
- `supports_logout` is true only when `agentCapabilities.auth.logout` is an
  object. Logout is optional and must be rejected as an unsupported capability
  when the Agent did not advertise it.
- Secret environment values never cross `AgentAuthCatalog`, descriptors,
  errors, Debug output, or UI state. The catalog exposes only `configured`.
- A terminal auth process advertises `clientCapabilities.auth.terminal` only
  when a real terminal host exists. Headless hosts filter terminal methods and
  never return a fake terminal id.
- Every ACP child is launched with stdio pipes and must detach itself from the
  desktop's controlling terminal before entering its background process group.
  Some CLIs open `/dev/tty` for interactive services even in ACP mode; leaving
  the child attached lets job control deliver `SIGTTIN`, which stalls
  `initialize` and prevents authentication discovery from completing.
- Auth discovery and mutation results are fenced by `(agent_id,
  provider_profile_id?)` and a monotonically increasing UI generation. A late
  result may not replace a newly selected Agent/auth scope or reattach a terminal.
- Every authenticate call carries a product-level
  `AgentAuthenticationOperationId`. The ACP runtime reserves it before process
  initialization, associates it with the dedicated temporary auth process, and
  permits at most one active authentication per Agent. Operations for different
  Agents remain independent and may run concurrently with normal Agent work.
- Cancelling an active operation marks it cancelled before awaiting process
  shutdown, closes its full process group, fails any pending initialize or
  authenticate request, and normalizes the final result to
  `agent_authentication_cancelled`. Cancellation does not expose or accept an
  ACP process instance id. Cancelling an operation that already finished is an
  idempotent `false`; the same Agent may immediately start a new operation with
  a new id after the old operation finishes.
- A terminal monitor retains the final output in the shared terminal buffer,
  classifies exit code `0` without a signal as success, and reports non-zero or
  signaled exits as `agent_terminal_auth_failed`. Closing or changing scope
  kills the temporary terminal; it is not persisted as a normal workspace
  terminal.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Agent is missing, disabled, or not ACP-enabled | `agent_auth_*_unsupported` or the normal enabled-agent validation error; no process is left running. |
| Provider Profile is missing for an environment method | `agent_auth_profile_required`. |
| Profile belongs to another Agent or is not ACP | `agent_auth_profile_mismatch` / `agent_auth_profile_kind_invalid`. |
| Requested method id was not in the latest initialize catalog | `agent_auth_method_not_advertised`; do not send `authenticate`. |
| A second authentication starts for the same Agent | `agent_authentication_in_progress`; do not launch another process. |
| Cancel targets an active operation for another Agent | `agent_authentication_operation_agent_mismatch`; do not stop either process. |
| Cancel targets an active operation | shut down its temporary process and complete authenticate with `agent_authentication_cancelled`. |
| Cancel targets an operation that already finished | return `false`; do not affect a newer operation. |
| Agent does not advertise logout | `agent_logout_not_advertised`; do not send `logout`. |
| Terminal host is unavailable | do not expose terminal methods; never fabricate a successful descriptor. |
| Auth terminal exits non-zero or by signal | `agent_terminal_auth_failed` with exit metadata only. |
| Auth discovery or authenticate initialize fails | return the structured ACP/provider error and shut down the temporary process. |

### 5. Good / Base / Bad Cases

- Good: an Agent advertises browser, environment, and terminal methods; the
  UI renders all three, stores the Profile's exact environment keys, opens a
  PTY for terminal login, and refreshes the catalog after completion.
- Good: two Profiles for one Agent keep independent keychain references and
  switching Profiles changes only the projected credentials used for auth.
- Good: one browser login waits indefinitely, another Agent continues to work,
  and Stop closes only the waiting login; a new login can then start normally.
- Base: an Agent advertises no auth methods; the detail view shows its normal
  unavailable/not-verified state without static login controls.
- Bad: render a generic `API_KEY` field for every Agent, call `authenticate`
  with a stale method id, or include an env value in a descriptor/log.
- Bad: treat a terminal process that exits with a signal as authenticated or
  keep its workspace terminal after the user leaves the Agent.
- Bad: put authentication behind a global management mutex, cancel by raw ACP
  process id, or drop only the UI future while leaving the auth process alive.
- Bad: assume `--acp` prevents every interactive terminal access; a CLI that
  opens `/dev/tty` from a background process group can be stopped before it
  answers ACP, leaving a leaked authentication process behind.

### 6. Tests Required

- `cargo test -p vibex-agent-acp auth::tests --locked` covers method parsing,
  bounds, exact ids, logout capability, terminal-value redaction, the
  fail-closed Auggie/Kiro/Poolside/Pi CLI login mappings, and CodeWhale
  terminal auth argument replacement across direct and managed launchers.
- `cargo test -p vibex-agent-acp agent_auth_methods_authenticate_terminal_and_logout_round_trip --locked`
  covers all method kinds and exact wire calls.
- `cargo test -p vibex-agent-acp hanging_agent_authentication_can_be_cancelled_without_blocking_and_restarted --locked`
  asserts independent discovery while login waits, process-backed cancellation,
  stable cancelled completion, idempotent late cancel, and successful restart.
- The no-terminal-host test asserts `auth.terminal == false` and that terminal
  methods are filtered from the catalog.
- `cargo test -p vibex-agent-acp process_environment::tests --locked` asserts
  an ACP child cannot reopen the controlling terminal; run it once under a
  pseudo-terminal to cover the job-control boundary.
- `cargo test -p vibex-config-switch agent_auth_environment --locked` covers
  exact key names, blank preservation, explicit clear, keychain rollback, and
  secret-free projections.
- Desktop Management tests cover method rendering, masked inputs, logout
  visibility, terminal exit classification, and Agent/auth-scope generation
  fencing.

### 7. Wrong vs Correct

#### Wrong

```rust
let method = static_provider_auth(agent_id).unwrap_or(default_api_key());
render_api_key_field(method);
```

#### Correct

```rust
let catalog = agent.list_auth_methods(agent_id, profile_id).await?;
for method in catalog.methods {
    render_dynamic_auth_method(method);
}
let operation_id = AgentAuthenticationOperationId::new();
agent.authenticate_agent(AgentAuthenticateRequest {
    operation_id: operation_id.clone(),
    agent_id: agent_id.clone(),
    provider_profile_id: profile_id,
    method_id,
}).await?;
agent.cancel_agent_authentication(AgentAuthenticationCancelRequest {
    operation_id,
    agent_id,
}).await?;
```

The Agent owns the method contract; Vibex owns the common UI, Profile secret
reference, `authenticate(method_id)`, and terminal lifecycle.

## Scenario: ACP Default Account Authentication Source And Model Discovery

### 1. Scope / Trigger

- Trigger: an ACP Agent exposes a browser, terminal, or Agent-owned login path
  whose credentials live in the Agent's normal default state home, and a
  session must use that account without a Provider Profile.
- This is the contract for the one `AgentAuthContext` allowed per Agent. It
  complements the dynamic method contract above; a method describes an action,
  while the context is the durable runtime identity.
- Login, verification, model discovery, runtime switching, logout, timeline
  attribution, and usage facts must all use the same context revision.

### 2. Signatures

```text
AgentAuthContextService::ensure_default(agent_id)
  -> AgentAuthContext
AgentAuthContextService::list()
  -> Vec<AgentAuthContext>
AgentAuthContextService::authenticate(
  AgentAuthContextAuthenticateRequest {
    operation_id, auth_context_id, expected_context_revision, method_id
  }
) -> AgentAuthContextAuthenticateResult
AgentAuthContextService::verify(
  AgentAuthContextVerifyRequest {
    auth_context_id, expected_context_revision, operation_id?
  }
) -> AgentAuthContextMutationResult
AgentAuthContextService::refresh_models(
  AgentAuthContextRefreshModelsRequest {
    auth_context_id, expected_context_revision
  }
) -> AgentAuthContextMutationResult
AgentAuthContextService::logout_preview(auth_context_id)
  -> AgentAuthContextLogoutPreview
AgentAuthContextService::logout(
  AgentAuthContextLogoutRequest {
    auth_context_id, expected_context_revision,
    confirmed_affected_session_count
  }
) -> AgentAuthContextMutationResult

RuntimeAuthSource =
  ProviderProfile { provider_profile_id }
  | AgentAccount { auth_context_id }

RuntimeModelSelection = Explicit { model_id } | AgentDefault

AgentAuthModelCatalogSnapshot {
  auth_context_id, auth_context_revision, runtime_fingerprint,
  discovery_source, status, models[], last_success_at_ms?,
  last_attempt_at_ms, last_error_code?
}
```

### 3. Contracts

- `ensure_default` is idempotent and the database `UNIQUE(agent_id)` constraint
  is the final fence. No UI or API may add, copy, name, or select a second
  account for the same Agent.
- The context stores only a bounded redacted account hint and the method id
  used for the latest successful action. It never stores token, cookie,
  credential-file contents, or the raw state-home path.
- Every Agent with a valid built-in ACP runtime configuration may use its
  ordinary process environment and normal default state home for authentication
  discovery and an Agent-account launch. The Agent-account path does not apply
  Provider projection or inject a Provider-specific state home.
- A compatibility descriptor enhances that baseline with exact
  credential/provider environment keys to unset, verified logout support, and
  direct model discovery. Absence of a descriptor means conservative inherited
  environment and ACP session evidence; it does not mean the default state home
  is unsupported.
- `authenticate` re-reads the current method catalog, rejects methods whose
  effect requires a Provider Profile, persists one operation row, and uses the
  same Agent/account launch context as verification and later sessions.
  Interactive terminal/browser work returns an execution location and a
  redacted terminal descriptor; completion is followed by verification in a
  fresh process.
- Verification shuts down old account-source processes first, increments the
  context revision for a credential change, probes models with that exact
  revision, then commits `Authenticated` and the snapshot. A successful RPC
  alone, a non-empty `authMethods` list, or a credential file is not proof.
- Discovery prefers a direct Agent model catalog, then session config evidence,
  a verified compatibility descriptor, and live-session evidence. If no real
  model list exists, it stores `AgentDefaultOnly` and publishes
  `RuntimeModelSelection::AgentDefault`; `session/new` receives no model
  override. A concrete model is selectable only when the same context revision
  proved it available.
- Snapshot cache identity is
  `(auth_context_id, auth_context_revision, runtime_fingerprint)`. Login,
  relogin, logout, an authentication-required runtime error, Agent/adapter
  version change, or state-home identity change invalidates older snapshots.
- A structured authentication-required error from a live turn invalidates only
  the binding's exact account revision. The service shuts down processes for
  that source, marks the context `AuthenticationRequired`, and never silently
  changes the session to a Provider Profile.
- Logout first previews affected logical session ids, then stops all account
  source processes before sending the Agent logout RPC. It increments the
  context revision, clears model snapshots, and leaves affected sessions with
  their desired source but a re-authentication/recovery state.
- Remote and UI projections contain status, labels, action availability,
  bounded hints, and model descriptors only. Native paths, process ids, ACP
  payloads, secrets, and OAuth/device-code values are local/short-lived.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Agent id is absent from the built-in ACP catalog, or its Agent ACP config is invalid | `agent_not_found` or the typed ACP config validation error; do not create a context. |
| Agent has no enhanced auth compatibility descriptor | use the ordinary Agent environment, no descriptor env-unsets, no enhanced logout/direct-catalog claim. |
| A second context is inserted for one Agent | SQLite uniqueness conflict; preserve the first context. |
| Context id is missing or belongs to another Agent | `agent_auth_context_not_found` or `agent_auth_context_agent_mismatch`; no process. |
| Expected revision is non-positive or stale | `agent_auth_context_revision_invalid` or `agent_auth_context_revision_conflict`; no external action. |
| Method is not in the latest catalog | `agent_auth_method_not_advertised`; do not call authenticate. |
| Method effect requires Provider Profile | `agent_auth_method_requires_provider_profile`; do not use it as an Agent account login. |
| Another operation is active for the context | `agent_authentication_operation_in_progress`; no second process. |
| Verification discovers authentication is absent | `agent_authentication_required`; context revision/status and snapshot are updated consistently. |
| Explicit model is absent from the current snapshot | `agent_auth_model_no_longer_available`; keep the old effective source. |
| No model enumeration evidence | `AgentDefaultOnly` catalog; run with `AgentDefault`, never a sentinel model id. |
| Logout impact count changed after preview | `agent_auth_context_in_use_changed`; require a fresh preview. |
| Logout is unsupported | `agent_logout_not_advertised`; do not send the ACP call. |
| Account process shutdown fails before logout | return the shutdown error and do not send logout. |

### 5. Good / Base / Bad Cases

- Good: Codex is logged in outside Vibex; the first catalog read creates one
  unverified context, verification observes the same default home, and a new
  session can select `AgentAccount` without a Provider Profile.
- Good: a relogin changes the account hint and entitlement, keeps the same
  context id, increments revision, and makes the previous model snapshot
  ineligible.
- Base: the Agent reports no models. The source remains selectable as
  “Agent automatically chooses” and the actual model, if later reported, is
  recorded only as effective evidence.
- Base: a catalog Agent without a dedicated compatibility descriptor exposes
  browser and environment methods. Browser/Agent-owned login uses its ordinary
  default environment; the environment method remains a Provider Profile
  action, and no enhanced logout/direct-model claim is fabricated.
- Bad: create a synthetic Provider Profile, set `model_id = "default"`, reuse
  a prior revision's model list, or let a Provider API-key variable leak into
  the Agent-account process.
- Bad: call logout while an account process still owns a token, or silently
  fail over an authentication error to a different billing/organization source.

### 6. Tests Required

- Core serde tests cover both tagged source variants, `AgentDefault`, bounded
  hints, and legacy Provider selection aliases.
- DB tests cover `UNIQUE(agent_id)`, CAS revision updates, operation uniqueness,
  snapshot primary-key isolation, invalidation, and migration 45/46/47 data
  preservation without synthetic Profiles.
- ACP tests assert login, model discovery, and real session launches share the
  same source fingerprint/home and that Registry env-unset keys are applied.
- Catalog tests cover direct/session/compatibility/live discovery precedence,
  `AgentDefaultOnly`, revision invalidation, and stale explicit model rejection.
- Runtime-switch tests cover Provider -> AgentAccount -> Provider, failed
  account verification retaining the old effective source, and account-revision
  authentication-required invalidation.
- Desktop runtime tests cover process shutdown before logout, affected-session
  preview, and no automatic Provider fallback. Remote tests cover redaction and
  permission classes.
- ACP auth tests cover every built-in Agent accepting its valid default ACP
  configuration, plus a catalog Agent without a dedicated compatibility
  descriptor creating one default context and discovering methods. An unknown
  Agent fails before a context is inserted.

### 7. Wrong vs Correct

#### Wrong

```text
authenticate(methodId)
  -> mark the Agent logged in
  -> copy methodId into providerProfileId
  -> reuse the last Provider model list
```

#### Correct

```text
ensure one AgentAuthContext
  -> authenticate(methodId) in the default account context
  -> close auth process and verify with the same launch context
  -> snapshot models under (context, revision, fingerprint)
  -> select RuntimeAuthSource::AgentAccount
  -> send AgentDefault when no concrete model was proven
```

## Scenario: ACP Terminal Tools And Terminal Auth

### 1. Scope / Trigger

- Trigger: an ACP agent asks the Vibex host to create, inspect, kill, release,
  or wait on a terminal through ACP terminal JSON-RPC methods.
- Trigger: an ACP provider needs a terminal-based login flow represented as a
  provider-neutral backend action descriptor.
- This is a cross-layer contract because ACP runtime requests flow through
  provider profile config, capability projection, runtime host seams,
  permission requests, bounded output summaries, and later UI/remote terminal
  surfaces.

### 2. Signatures

Provider config and capability projection:

```text
AcpProviderConfig {
  terminal_tools: bool = false,
  terminal_auth: bool = false
}

ProviderCapabilities {
  terminal_tools: bool,
  terminal_auth: bool
}
```

ACP initialize capabilities:

```text
clientCapabilities.terminal: bool
clientCapabilities.auth.terminal: bool
clientCapabilities.meta.terminal_output: bool
clientCapabilities.meta["terminal-auth"]: bool
```

Runtime terminal host seam:

```text
AcpTerminalHost::create(AcpTerminalCreateRequest) -> TerminalId
AcpTerminalHost::kill(TerminalId) -> ()
AcpTerminalHost::release(TerminalId) -> ()
AcpTerminalHost::output(TerminalId, limit: usize) -> AcpTerminalOutput
AcpTerminalHost::wait_for_exit(TerminalId) -> AcpTerminalExitStatus
AcpTerminalHost::terminal_auth_descriptor(AcpTerminalAuthRequest)
  -> TerminalAuthActionDescriptor
```

ACP runtime methods served by Vibex:

```text
terminal/create
terminal/kill
terminal/release
terminal/output
terminal/wait_for_exit
```

### 3. Contracts

- Terminal tools and terminal auth are disabled by default in every built-in or
  imported ACP profile unless an explicit typed config flag or compatible
  feature token enables them.
- ACP initialize may advertise `terminal=true` and `auth.terminal=true` only
  when the provider profile enables the feature and the runtime was constructed
  with an `AcpTerminalHost`.
- `terminal/create` is permission-producing. The ACP runtime must emit a Vibex
  `PermissionRequest` before calling `AcpTerminalHost::create`.
- Approval and always-allow both create the host terminal once; deny responds to
  the ACP request with a terminal-denied error and must not create a terminal.
- Pending terminal-create requests must be drained on interrupt, shutdown, or
  process exit so the ACP agent is not left waiting forever.
- Terminal output returned through ACP responses must be bounded but otherwise
  unchanged. Secret env values must never be copied by Vibex into permission
  details, logs, or terminal auth descriptors; if an Agent command itself
  prints a value, the authoritative session output preserves it.
- Terminal auth is represented by `TerminalAuthActionDescriptor`. It may include
  command, args, cwd, env keys, and redacted env summaries, but not secret env
  values or auth codes.

### 4. Validation & Error Matrix

- Terminal tools disabled -> do not advertise terminal capability; if a request
  still arrives, respond with ACP method/capability unavailable.
- Missing `terminal/create.command` -> JSON-RPC invalid params.
- Missing terminal id for kill, release, output, or wait -> JSON-RPC invalid
  params.
- User denies terminal create -> terminal command denied; no host terminal is
  created.
- Terminal host create/output/wait/kill/release fails -> provider/process error
  with redacted diagnostics only.
- Terminal host absent while config asks for terminal support -> do not
  advertise terminal capability; disabled host returns a capability error.
- Interrupt, shutdown, or process exit while create permission is pending ->
  pending ACP terminal create is cancelled or errored, never left hanging.

### 5. Good/Base/Bad Cases

- Good: ACP profile enables terminal tools and the runtime has a terminal host;
  `initialize` advertises terminal support, `terminal/create` emits a permission
  request, approval creates exactly one host terminal, output is bounded, and
  wait returns an exit status.
- Good: ACP profile enables terminal auth and the runtime has a terminal host;
  backend code can produce a `TerminalAuthActionDescriptor` without writing
  secrets into ordinary timeline items.
- Base: terminal tools are disabled; ACP runtime continues to serve sessions,
  filesystem methods, and permission callbacks without advertising terminal
  support.
- Bad: terminal support is advertised solely because a provider feature token is
  present, even though no Vibex terminal host exists.
- Bad: command env values or auth tokens appear in permission details, debug
  logs, terminal output responses, or auth descriptors.

### 6. Tests Required

- Capability tests must assert ACP terminal tools/auth default to disabled and
  become effective only through explicit config/feature projection.
- Runtime tests must assert initialize terminal/auth capabilities are gated by
  both config and host support.
- Runtime tests must assert terminal create parses command, args, cwd, and env
  keys, emits a permission request, calls the mock host only after approval, and
  does not call it after denial.
- Runtime tests must assert terminal output is bounded and content-preserving, wait
  returns exit status, and kill/release delegate to the host.
- Auth tests must assert `TerminalAuthActionDescriptor` contains env keys or
  redacted permission/auth summaries only and never copies secret env values.

### 7. Wrong vs Correct

#### Wrong

```text
config.features contains "terminal"
  -> initialize advertises terminal=true
  -> terminal/create directly spawns a process
```

This bypasses Vibex permission handling and can advertise a capability the host
cannot actually serve.

#### Correct

```text
config enables terminal tools + AcpRuntimeClient has AcpTerminalHost
  -> initialize advertises terminal=true
  -> terminal/create stores a pending terminal request
  -> Vibex PermissionRequest is emitted
  -> approval calls AcpTerminalHost::create once
  -> ACP receives { terminalId }
```

The permission system remains the durable approval boundary, and the runtime
host seam keeps terminal execution provider-neutral.

## Scenario: OpenCode ACP Model Error Bridge

### 1. Scope / Trigger

- Trigger: OpenCode keeps `session/prompt` pending while retrying a failed model
  API request and does not emit an ACP error response or notification on stdout.
- Trigger: OpenCode returns `end_turn` without any non-empty Agent message,
  thought, or new tool-call update after the model transport failed before the
  first token.
- This fallback is OpenCode-specific. Generic ACP agents must continue to use
  standard JSON-RPC responses and process lifecycle errors.

### 2. Signatures

OpenCode launch arguments:

```text
opencode acp --print-logs --log-level ERROR
```

OpenCode stderr fields consumed by Vibex:

```text
message="stream error"
session.id=<native-session-id>
small=<true|false>
error.error=<redacted model API error>
```

Runtime correlation and failure:

```text
AcpProcess.pending_prompt_requests[native_session_id] = json_rpc_request_id
user-action-required main stream error -> session/cancel { sessionId }
                                       -> provider/opencode_model_api_error
retryable main stream error -> provider/opencode_model_api_retrying
                            -> progress, eight errors, or two-minute deadline
```

### 3. Contracts

- Effective OpenCode args include `--print-logs` and `--log-level ERROR` when
  absent. Existing same-name args are preserved and never duplicated.
- The effective args, including injected logging args, are used for process
  spawn diagnostics, session metadata, and pool command fingerprints.
- Only `message="stream error"` lines associated with a currently pending
  `session/prompt` and active native session may affect a turn.
- `small=true` identifies title/summary generation and must never fail the main
  turn. A main `small=false` error that requires user action, including exhausted
  usage/quota, rate limiting, insufficient balance/credit, billing/subscription,
  or authentication failure, fails immediately. OpenCode 1.18.1 may emit only one
  such error and then honor a multi-hour `Retry-After`; waiting for a second log
  line leaves the product in `running` indefinitely.
- Other main errors remain retryable. The first error in a retry window emits a
  canonical `opencode_model_api_retrying` Timeline error with a stable provider
  correlation id, so clients show the provider failure without treating it as a
  turn boundary. Eight consecutive errors or two minutes of model-stream silence
  fail the turn. A non-empty Agent message, thought, or new tool-call update proves
  progress, resets the consecutive error count, and slides the silence deadline.
  A later error starts a new retry window from zero.
- The pending JSON-RPC sender is the race arbiter. Vibex sends `session/cancel`
  only when the stderr path successfully removes that sender before a normal
  ACP response does.
- Response, send failure, timeout, shutdown, process exit, and pooled-session
  removal must clear prompt correlation state. No lock may be held while
  sending `session/cancel` or completing a pending sender.
- OpenCode writes model failures to stderr independently from the stdout
  JSON-RPC response. A terminal prompt RPC error keeps its prompt correlation
  for a bounded 250 ms stderr-drain window before classifying the response; a
  one-second detached cleanup removes the correlation if the awaiting caller
  was cancelled. Never assume cross-pipe write order equals reader-task order.
  In particular, one delayed stdout progress event may slide the recovery
  deadline but must not permanently disarm it.
- User-visible errors use stable code `opencode_model_api_error`; their session
  message is bounded and preserved, while the raw stderr copy is independently
  redacted before becoming diagnostic context.
- A normal OpenCode `end_turn` requires evidence of model-stream progress. If
  the turn has no non-empty Agent message, thought, or new tool-call update and
  no permission/elicitation remains pending, Vibex returns
  `opencode_model_api_error` and emits no synthetic empty final Agent message.
  This covers OpenCode versions that reduce a malformed HTTP 200 response or
  other pre-token transport failure to a zero-token `finish=unknown` turn
  without writing a parseable `stream error` line.

### 4. Validation & Error Matrix

- Standard ACP JSON-RPC error -> `provider/acp_rpc_error`.
- OpenCode prompt RPC error whose correlated stderr arrives during the bounded
  drain -> preserve the stderr reason as `provider/opencode_model_api_error`.
- OpenCode title stream error (`small=true`) -> ignored for main-turn state.
- OpenCode usage/quota/rate-limit/balance/billing/authentication stream error ->
  immediate `provider/opencode_model_api_error` plus `session/cancel`.
- First retryable OpenCode main stream error -> correlated
  `provider/opencode_model_api_retrying`; keep waiting within the recovery window.
- Agent message/thought/new tool-call progress after an error -> reset the
  consecutive count; later errors start a new recovery window.
- Eighth consecutive correlated OpenCode main stream error ->
  `provider/opencode_model_api_error` plus `session/cancel`.
- One correlated retryable error followed by two minutes of silence ->
  `provider/opencode_model_api_error` plus `session/cancel`.
- OpenCode `end_turn` with no model-stream progress and no pending host input ->
  `provider/opencode_model_api_error`; no empty final Agent message.
- Stream error for an unknown/inactive session -> diagnostic tail only.
- Prompt timeout wins before stderr failure -> `process/acp_request_timeout`; a
  late stderr line must not cancel a newer or already completed request.

### 5. Good/Base/Bad Cases

- Good: a transient upstream 500 recovers after several retries; subsequent
  model progress resets the budget and the turn continues without cancellation.
- Good: an upstream failure remains unavailable through eight consecutive
  errors; Vibex ends the turn with the bounded original reason and OpenCode receives one
  cancel instead of retrying forever.
- Good: `Free usage exceeded, subscribe to Go` or the underlying `Rate limit
  exceeded` fails on the first correlated main error, enters session `error`, and
  appends a provider-neutral Timeline error that GPUI renders.
- Good: one transient 5xx is visible as a correlated retrying error; recovery
  removes the pending state, while silence reaches the two-minute deadline.
- Base: title generation fails while the main model succeeds; the turn returns
  normally and no cancel is sent.
- Base: another ACP agent writes similar stderr text; Vibex retains it only as
  diagnostics because the profile is not OpenCode.
- Bad: every stderr error fails the first active turn without matching
  `session.id` to its pending prompt.
- Bad: Vibex waits for the two-hour prompt timeout after OpenCode has already
  logged repeated unrecoverable API failures.

### 6. Tests Required

- Unit tests parse quoted logfmt fields, strip known AI SDK error prefixes, and
  reject unrelated and `small=true` lines.
- Argument tests assert missing logging flags are appended only for OpenCode
  and existing flags are not duplicated.
- Unit tests classify real OpenCode 1.18.1 usage-limit/rate-limit text as
  user-action-required while leaving 5xx/service-unavailable errors retryable.
- Mock-process tests emit one correlated usage-limit error and assert immediate
  `opencode_model_api_error`, session `error`, a persisted Timeline error, and one
  `session/cancel` notification.
- Mock-process tests emit eight correlated retryable main errors without a prompt
  response and assert fast `opencode_model_api_error` completion plus exactly
  one `session/cancel` notification.
- Mock-process tests emit one retryable error, one nearby progress update, and
  then silence; assert a correlated `opencode_model_api_retrying` event followed
  by deadline failure despite cross-pipe scheduling order.
- Repeated mock-process tests write stderr before a terminal stdout RPC error and
  assert high-load scheduling still preserves `opencode_model_api_error` rather
  than degrading to `acp_rpc_error`.
- Mock-process tests emit seven errors, model-stream progress, then seven more
  errors and a successful response; progress must reset the error budget and no
  cancellation may be sent.
- Mock-process tests emit a title error followed by a successful prompt and
  assert the main turn completes without cancellation.
- Mock-process tests return `end_turn` without message, thought, tool, or pending
  permission activity and assert `opencode_model_api_error` with no final Agent
  message.

### 7. Wrong vs Correct

#### Wrong

```text
session/prompt -> OpenCode retries model API forever
               -> ACP stdout stays silent
               -> Vibex remains running until the global prompt timeout
```

#### Correct

```text
session/prompt -> correlate native session with JSON-RPC id
stderr usage/rate/billing/auth error -> fail immediately
stderr retryable error -> show correlated retry state
                       -> progress, x8 threshold, or two-minute deadline
                       -> atomically remove sender and send session/cancel
                       -> return opencode_model_api_error
```

The bridge restores a bounded provider-neutral turn lifecycle while keeping the
non-standard stderr dependency isolated to the OpenCode profile.

## Scenario: ACP Session Runtime Configuration State And Operation Gate

### 1. Scope / Trigger

- Trigger: a committed ACP attachment changes Model, Mode, Reasoning Effort, or
  another advertised session option, or replays the same preference after a
  process/attachment rebuild.
- This is a cross-layer contract spanning `crates/core` domain state,
  `crates/db` binding CAS, and `crates/agent-acp` discovery, wire selection,
  attachment fencing, and replay.
- Session configuration is not process configuration: these values must never
  enter `ProcessSpawnConfigSnapshot` or its fingerprint.

### 2. Signatures

```rust
SessionRuntimeConfigState {
    preferred_model: Option<String>,
    effective_model: Option<String>,
    preferred_mode: Option<String>,
    effective_mode: Option<String>,
    preferred_reasoning_effort: Option<String>,
    effective_reasoning_effort: Option<String>,
    config_values: BTreeMap<String, SessionRuntimeConfigValueState>,
    state_revision: i64,
    applied_activation_generation: Option<i64>,
}

SessionRuntimeConfigValueState {
    preferred: Option<ProviderSessionConfigValue>,
    effective: Option<ProviderSessionConfigValue>,
}

SessionRuntimeConfigMutationRequest {
    session_id: VibexSessionId,
    expected_revision: i64,
    expected_binding_id: RuntimeBindingId,
    expected_activation_generation: i64,
    patch: SessionRuntimeConfigPatch,
}

AcpRuntimeClient::update_session_runtime_config(request)
    -> SessionRuntimeConfigMutationResult
RuntimeBindingRepository::compare_and_set_session_runtime_config_state(
    conn, binding_id, expected_state, expected_activation_generation, next_state
) -> ()
AcpAgentCompatibility::config_option_aliases_for_runtime(
    adapter_version, compatibility_identity
) -> Option<&BTreeMap<String, Vec<String>>>
SessionConfigPlanner::option_key_for_option(option)
    -> Result<CanonicalSessionConfigKey, CanonicalKeyError>
```

`model`, `reasoning_effort`, `approval_mode`, and `sandbox_mode` are reserved
canonical keys. Mode is the typed `mode_id` field; reserved values cannot be
smuggled through `config_values`. Generic values use the explicit dual shape,
and legacy `{ value, label }` JSON is read as both preferred and effective but
is written back in the dual shape.

### 3. Contracts

- A mutation first validates the committed/current four-part attachment fence,
  expected revision, process `Ready` state, `ProcessConfigStatus::Current`,
  and an idle attachment (no active turn, permission, or terminal-create
  request). Failure at this gate has zero state or wire side effects.
- A semantic preferred change increments `state_revision` once for the whole
  patch. An identical converged patch is a zero-revision/zero-wire no-op. A
  same-revision pending patch may retry only fields whose effective value has
  not converged.
- Preferred state is recorded before any ACP request. Effective state advances
  only after a successful response, no explicit conflicting value, the same
  mutation revision, and a fresh `SessionAttachmentRegistry::apply_current`
  fence check. A stale response has no attachment or transient-store effect.
- Field order is deterministic: Model, Reasoning Effort, Mode, then generic
  canonical keys in ascending order. Partial wire success keeps confirmed
  effective fields and leaves failed fields preferred-only; ACP effects are not
  rolled back.
- Candidate order is negotiated typed live operation, negotiated versioned raw
  operation, advertised Model config option, exact compatibility-identity
  extension, descriptor startup projection (`RestartAndResume`), then
  unavailable. Only an explicit capability negative (`-32601`, method-not-found,
  or an equivalent unsupported diagnostic) advances to the next candidate.
  Authentication, permission, timeout, transport, malformed response, and
  provider errors stop fallback for that field.
- Alias mapping is explicit and scoped by the descriptor's adapter-version
  requirement. Stable config semantics may use a tested minimum-version range;
  quirks, extensions, and event decoders remain exact-identity scoped.
  An explicitly registered option `category` may map a future option id to its
  canonical key. Normalized labels or value containment are never semantic
  evidence, and conflicting id/category claims fail closed as an ambiguity.
- A new/load/rebuilt attachment seeds effective values only from that response.
  The same `RuntimeBindingId` retains preferred state across a crash and gets a
  new activation generation. Intentional close or replacement removes the
  legacy transient state; no incomplete durable binding row is created.
- Profile-only Model IDs and a session-global effort option do not fabricate
  per-Model reasoning capability. Effort is attached to a Model only when a
  live probe or exact extension explicitly associates the two.
- Diagnostics and outcomes contain bounded canonical keys, operation/encoding,
  stable error codes, revision, and generation only. They do not contain raw
  native session ids, prompts, environment values, secrets, or unbounded ACP
  payloads.

### 4. Validation & Error Matrix

- Expected session/binding/revision/generation mismatch -> structured
  `StaleConfirmation` outcome with `acp_session_config_fence_stale` and no
  ACP request.
- Missing/non-ready process -> `StaleConfirmation` outcome;
  stale process-config status -> `RestartRequired` outcome;
  active turn or pending host work -> `Busy` outcome.
- Unknown/reserved key, set-and-clear conflict, empty/overlong value, or
  invalid effort spelling -> `validation/acp_session_config_*` error.
- No version-compatible alias or ambiguous id/category alias ->
  `validation/acp_session_config_key_*`; labels and selected values never
  trigger a reserved mapping.
- No candidate or advertised value -> `Unavailable`; a descriptor startup
  projection -> `RestartRequired` without replacing the process.
- ACP method-not-found/explicit unsupported -> mark only that operation
  negative for the current identity/generation and try the next candidate.
- Permission/authentication/timeout/transport/provider error -> `Failed` and
  no fallback; prior confirmed fields remain effective.
- Response explicitly reports a value different from the target -> `Failed`
  with `acp_session_config_response_mismatch`.
- Fence/revision/CAS failure after an external success ->
  `StaleConfirmation` or `ReconciliationRequired`; never claim effective for a
  replacement generation.

### 5. Good/Base/Bad Cases

- Good: a four-field patch emits `session/set_model`, then effort, then mode,
  then sorted generic options; one capability-negative Model response falls
  back to its exact advertised config option while an auth error does not.
- Good: a Claude adapter at or above the tested config baseline maps an
  `effort` option with category `thought_level` to `reasoning_effort`, while a
  newer identity still receives no baseline-only event decoder or quirk.
- Good: after a crash, the same binding retains preferred Model, loads a new
  generation's observed Model/Mode, replays only differences, and blocks prompt
  dispatch while an unavailable known preference remains unconverged.
- Base: a Profile contributes Model IDs to the catalog with an empty effort
  set; an explicit session/probe association may add bounded efforts.
- Bad: writing preferred/effective values into the process fingerprint, trying
  `set_config_option` after a permission failure, or accepting a late response
  after the binding/generation changed.
- Bad: using an option label such as `"Model"` or a value containing a model id
  as an alias, or inserting a partial `session_runtime_bindings` row for the
  transient legacy path.

### 6. Tests Required

- Core serde tests cover explicit fields, dual generic values, legacy JSON,
  revision monotonicity, and generation convergence.
- DB tests cover successful CAS, identical-state no-write, stale JSON/revision,
  stale activation generation, and missing binding with zero writes.
- Registry and planner tests cover the minimum-version boundary, exact-only
  policies, category-based future ids, id/category collisions, typed/raw/
  config-option/extension/restart/unavailable priority, generation-scoped
  observed negative, deterministic field order, and catalog no-fabrication.
- ACP mock tests cover Model/Mode/Effort/generic wire shapes, partial success,
  response mismatch, capability-only fallback, auth/timeout no-fallback,
  busy/stale-process zero side effects, pooled attachment isolation, close
  cleanup, and same-binding crash rebuild replay.
- Run `cargo test -p vibex-agent-acp`, `cargo test -p vibex-core runtime`,
  `cargo test -p vibex-db runtime`, workspace check/test, scoped clippy,
  `pnpm check:frontend`, and Trellis validation. Default
  checks remain offline and credential-free.

### 7. Wrong vs Correct

#### Wrong

```text
set_model fails for any reason
  -> try set_config_option
  -> write effective immediately
  -> prompt on whichever attachment answers later
```

#### Correct

```text
validate current fence + idle/process gate
  -> CAS preferred (revision + 1)
  -> map config semantics through the descriptor's version policy
  -> reserve exact identity for quirks/extensions/event decoding
  -> fallback only on capability negative
  -> revalidate fence + revision
  -> CAS effective / mark generation applied
```

## Scenario: ACP Session Restore Compatibility And Stale Detection

### 1. Scope / Trigger

- Trigger: an ACP runtime rebuilds a process for an existing `ProviderBinding`,
  imports a native session, or receives a restore response after the process or
  attachment generation changed.
- `crates/core` owns restore DTOs, `crates/agent-acp` owns current-generation
  capability resolution and the restore gate, and `crates/db` owns exact CAS.

### 2. Signatures

```text
AgentSessionRestoreCompatibilityKey {
  agentId, nativeSessionId, nativeStateHomeId,
  adapterCompatibilityIdentity, agentStateFormatIdentity,
  providerResumeIdentity, workspaceIdentity
}
resolve_restore_compatibility(sourceKey, targetKey, generationEvidence, generation)
  -> Compatible | ProbeRequired { allowedMethods } | Incompatible { reason }
AgentSessionRestoreOutcome = Resumed | Loaded | NotFound |
  AuthenticationRequired | Unsupported | TransientFailure | FatalFailure
RuntimeBindingRepository::compare_and_set_restore_compatibility_key(...)
RuntimeSwitchRepository::compare_and_set_restore_compatibility_result(...)
```

### 3. Contracts

- Restore keys trim and bound identity strings. Native ids are serialized only
  for exact matching; Debug, errors, and diagnostics never print native ids or
  resume tokens.
- Identity comparison is exact across Agent, state home, adapter/state format,
  provider resume identity, and workspace. A mismatch is `Incompatible`.
- `Compatible` requires current-generation `NegotiatedRuntime` or
  `ObservedRuntime` support. Static descriptors and unknown evidence produce
  `ProbeRequired` only.
- The candidate order is typed `session/resume`, then typed `session/load`,
  inside the attachment acquire closure. Only method-not-found,
  explicit unsupported, or stable not-found can advance or use explicit fresh.
- The native session id in a `session/resume` or `session/load` request is the
  authoritative restored identity. Standard ACP restore responses are objects
  that may omit `sessionId`; accept that shape. If an Adapter includes the
  extension field, it must be a string that exactly matches the requested id.
- A restore JSON-RPC error may carry decisive evidence under `error.data` even
  when `error.message` is only `Internal error`. The ACP boundary must inspect
  bounded structured data before discarding it, map a recognized missing native
  resource to the stable redacted `protocolErrorKind=resource_not_found`, and
  never copy the raw data or native id into Debug, diagnostics, or user errors.
  This classification is operation-scoped to `session/resume` and
  `session/load`; the same data on prompt or configuration operations remains a
  provider error.
- Fresh uses a new transient binding id and `nativeSessionId=None` acquire key;
  it must not reuse an old native-id key. Auth/permission, timeout,
  transport/process crash, malformed response, route mismatch, and provider
  failures stop the ordinary restore chain and preserve the source current
  attachment. A deliberate runtime hot switch is the bounded exception: after
  a fatal resume/load failure with no prompt side effect, its journaled
  coordinator may create a quarantined fresh target and bridge the
  authoritative Logical Session timeline. Authentication and transient
  failures still stop the switch.
- Success revalidates the complete fence before config replay or CAS. Stale or
  missing binding/switch rows perform zero writes. The switch result JSON seam
  is an optional bounded cache, not current-binding authority; P5 commit and
  Context Bridge remain out of scope.

### 4. Validation & Error Matrix

- Empty/overlong identity -> `validation/restore_compatibility_identity_*`.
- Identity mismatch -> typed `RestoreIncompatibilityReason`, no wire request.
- Missing generation evidence -> `ProbeRequired`.
- Method-not-found/unsupported -> `Unsupported`; stable missing session ->
  `NotFound`; these are the only automatic fresh candidates.
- Codex `-32603 Internal error` with bounded `error.data.details` matching
  `no rollout found for thread id ...` on resume/load -> `NotFound`; resume
  advances once to load, and a second classified miss may create fresh.
- `-32603 Internal error` without recognized structured missing-resource
  evidence -> `FatalFailure`; no fresh fallback.
- Object response without `sessionId` -> valid restore response; present
  non-string `sessionId`, a different id, or a non-object response ->
  `FatalFailure` with no fresh fallback.
- Auth/permission -> `AuthenticationRequired`; timeout/transport/process ->
  `TransientFailure`; malformed response/native mismatch/provider error ->
  `FatalFailure`; ordinary restore may not fresh-fallback. A deliberate runtime
  hot switch may fresh-and-bridge only the fatal outcome after journaling the
  failed restore operation.
- Binding/switch identity, generation, revision, status, or expected-result CAS
  mismatch -> structured conflict with zero writes.

### 5. Good/Base/Bad Cases

- Good: negotiated resume succeeds once; resume method-not-found advances once
  to load; load not-found creates fresh under a new transient identity.
- Good: Codex ACP returns modes/config options without echoing `sessionId`;
  Vibex retains the requested native id and continues attachment activation.
- Good: Codex reports a missing rollout only in `error.data`; Vibex records the
  redacted `resource_not_found` kind, tries resume/load once each, then creates
  one fresh native session while retaining the Logical Session timeline.
- Good: a same-Profile runtime option change cannot restore the old Codex ACP
  native session; the hot-switch journal records the fatal restore, creates a
  quarantined fresh session, applies the selected options, bridges history,
  commits it, and only then admits the queued message.
- Base: static/unknown capability returns probe-required instead of guessing an
  encoding.
- Base: an unrelated `-32603` on `session/prompt` remains a provider failure
  even if its structured data contains generic session wording.
- Bad: catch-all `session/load` error followed by `session/new`, or fresh retry
  with `Some(oldNativeSessionId)` in the same attachment key.
- Bad: validate a restore response with the `session/new` response contract and
  reject it only because it does not echo `sessionId`.

### 6. Tests Required

- Core serde/Debug tests cover key validation, identity mismatch reasons,
  seven outcomes, and raw-id redaction.
- Resolver tests cover negotiated Compatible, static/unknown ProbeRequired,
  identity mismatches, generation scoping, and deterministic order.
- ACP mock tests cover typed resume/load, fallback, not-found/
  unsupported fresh, auth/timeout/provider/invalid no-fallback, native-id
  mismatch, an omitted optional response `sessionId`, and same-key at-most-once
  effects. Include the exact Codex
  `-32603` + `error.data.details=no rollout found for thread id ...` shape and
  assert one resume, one load, one fresh session, and no raw native id in the
  classified error state.
- Runtime-switch ACP tests separately cover fatal-restore fresh-and-bridge plus
  message continuation for same-Profile option/model, cross-Profile Provider,
  and cross-Agent selections. The ordinary restore tests above must continue to
  reject fatal fallback.
- DB tests cover typed key round-trip, identical no-write, stale generation,
  and switch restore-result CAS/query. Run targeted and workspace checks,
  bindings/frontend checks, fmt, clippy, and Trellis validation.

### 7. Wrong vs Correct

#### Wrong

```text
require restoreResponse.sessionId == requestedNativeId
session/load error (any category) -> warn -> session/new -> replace route
```

#### Correct

```text
exact key + current evidence
  -> resume (typed) -> load (typed)
  -> keep requestedNativeId; validate response.sessionId only when present
  -> inspect bounded JSON-RPC error.data before dropping provider detail
  -> expose only a stable redacted error kind
  -> classify miss/unsupported separately from auth/transient/fatal
  -> fresh only for an allowed miss, with a new transient binding key
  -> fence + CAS before target state becomes effective
```

## Scenario: Turn Execution Attribution Snapshot

### 1. Scope / Trigger

- Trigger: an Agent provider is about to admit a prompt, emit live events, return buffered/transcript events, or
  coalesce an existing timeline item.
- The attribution answers which committed runtime actually executed the visible turn. It is not derived from the
  Composer selection, desired runtime, a later current binding, or provider-native session identifiers.

### 2. Signatures

```text
AgentProvider::prepare_turn_execution(handle, request)
  -> Option<ProviderTurnExecutionIdentity {
       binding_id, activation_generation, model_id?
     }>

ProviderTurnRequest.execution_identity: Option<ProviderTurnExecutionIdentity>

TurnExecutionAttribution {
  agent_id, auth_source, model, effective_model_id?,
  binding_id, activation_generation,
  agent_label, auth_source_label, model_label
}

TimelineItem.executionAttribution?: TurnExecutionAttributionView {
  agentLabel, providerProfileLabel, modelLabel
}

agent_timeline_items.execution_attribution_json TEXT NULL
```

### 3. Contracts

- The manager calls `prepare_turn_execution` once per provider attempt before `send_turn`, creates one immutable
  `TurnExecutionAttribution`, and attaches the same snapshot to live, buffered/final, permission, tool, transcript,
  coalesced, and output-producing terminal-error items from that attempt.
- Generic ACP prepare may establish/restore the attachment and must return the committed binding/generation plus its
  effective Model. ACP send takes the session operation lock, reads only the current committed attachment, and
  revalidates binding, generation, and effective Model before `begin_turn` or `session/prompt`. Send must not restore or
  create a replacement attachment from the old request binding.
- Every online turn is ACP-backed and requires a real committed RuntimeBinding id and activation generation. Do not
  synthesize a Provider Profile for an Agent account, a legacy binding id, or attribution from ProviderKind. An
  `AgentDefault` desired model may have a null effective model until the Agent reports one.
- SQLite stores the complete Rust-only audit snapshot. Timeline reads project it to `TurnExecutionAttributionView`;
  generated TypeScript and remote/Desktop payloads never contain binding ids, generations, native ids, adapter ids,
  credentials, commands, endpoints, or raw provider metadata.
- The field is nullable and omitted from serialized legacy timeline JSON. Existing rows and non-Agent items remain
  readable without inventing a source. Labels and Model ids are trimmed, non-empty, UTF-8-safe, and byte-bounded.
- Provider-correlation coalescing compares the complete stored snapshot before update. It never overwrites, adopts, or
  drops attribution from a different attempt.
- Canonical `FileOperation`, `Command`, `WebSearch`, `TodoUpdate`, `Collaboration`, `ImageGeneration`, and `Permission`
  payloads remain unchanged; attribution is Timeline metadata, not a replacement event or generic ToolCall payload.

### 4. Validation & Error Matrix

- Missing/invalid auth source or Agent definition while creating a proved attribution -> structured
  `turn_execution_profile_missing`/`turn_execution_agent_missing` or
  `turn_execution_attribution_invalid`; no prompt admission.
- ACP send without a prepared identity -> `turn_execution_identity_missing`; no active turn, prompt, permission, or
  timeline side effect.
- Binding, generation, or effective Model changed after prepare -> `turn_execution_identity_mismatch`; no active turn,
  prompt, permission, or timeline side effect.
- Effective ACP Model changed after prepare -> `turn_execution_identity_mismatch`; no provider turn starts and
  attribution is never guessed from UI state. A missing model is valid only when desired is `AgentDefault`.
- Coalesced event attribution differs from stored audit JSON, including `Some` versus `None` ->
  `turn_execution_attribution_conflict`; existing row remains unchanged.
- Old/NULL attribution row -> safe `None` projection and legacy rendering.

### 5. Good/Base/Bad Cases

- Good: ACP prepare restores a session, snapshots binding generation 4 and effective Model A, then every streamed and
  final item retains that source after the user switches to Model B.
- Good: a streamed delta is persisted and the provider then fails; the single terminal error carries the same safe
  attribution as the delta, and the manager does not replay the turn through failover.
- Base: a historical timeline row with no attribution remains fully readable/renderable; new online turns do not use
  that nullable legacy shape as execution authority.
- Bad: `send_turn` calls attachment restore again with the pre-prepare binding, or a coalescing update adds attribution
  to an old un-attributed item.
- Bad: the public Timeline serializes `bindingId`, `activationGeneration`, native session ids, adapter identity, or
  secret-bearing profile configuration.

### 6. Tests Required

- Core tests cover bounded construction, deserialize revalidation, legacy omission, and safe JSON projection with no
  internal fence fields.
- DB tests cover migration/NULL compatibility, complete audit round-trip, safe read projection, coalescing stability,
  and mismatch rejection without mutation.
- Manager tests cover one attribution across streamed/final/error items, exactly one terminal error, stable profile
  labels, and no binding/generation leak in Timeline JSON.
- ACP mock tests cover Claude/Codex managed routes, selected/default/provable Model rules, prepare/restore, stale
  generation rejection before prompt admission, and normal restore/fresh behavior.
- Canonical event golden tests remain exhaustive for semantic variants. Run workspace check/test, bindings generation
  and drift check, frontend typecheck/lint/build, fmt, clippy, and Trellis validation.

### 7. Wrong vs Correct

#### Wrong

```text
send prompt -> read current Composer/profile/model -> tag returned events
late event -> coalesce and overwrite whichever attribution is stored
```

#### Correct

```text
prepare committed execution identity
  -> build one bounded audit snapshot
  -> send revalidates fence under prompt-admission lock
  -> persist the same snapshot on every output item
  -> project only safe labels to clients
  -> reject conflicting coalescing updates
```

## Scenario: Durable Ordinary Message Submission And Current-Binding Fence

### 1. Scope / Trigger

- Trigger: Desktop or Remote submits an ordinary message to an ACP Logical
  Session, retries that action, changes the desired runtime between queued
  messages, interrupts during initial runtime preparation, disconnects while
  waiting, or restarts after prompt admission.
- `crates/core` owns the provider-neutral request/query contract, `crates/db`
  owns enqueue/sequence/result transactions, `crates/agent` owns ordered
  workers and prompt admission, and ACP owns the final attachment fence.

### 2. Signatures

```text
SendAgentMessageRequest {
  sessionId, messageIdempotencyKey, desiredRuntime,
  text, attachments, reasoningEffort, correlationId
}
GetMessageSubmissionRequest { sessionId, messageIdempotencyKey }
MessageSubmissionCoordinator::{submit, get_submission, reconcile_on_startup}
MessageSubmissionRepository::cancel_before_dispatch_for_session
  (&mut Connection, &VibexSessionId) -> Vec<TimelineItem>
ProviderTurnRequest::{message_submission_id, required_runtime, execution_identity}
AcpSendTurnRequest::{message_submission_id, required_runtime, execution_identity}

agent_message_submission_payloads(
  payload_reference, submission_id, session_id, submission_sequence,
  payload_json, result_first_sequence, result_last_sequence,
  created_at_ms, updated_at_ms
)
```

### 3. Contracts

- Enqueue stores the submission and isolated payload in one immediate SQLite
  transaction. Exact `(session, key, payload, desiredRuntime)` retries reuse
  one row and sequence; a different payload with the same key is a conflict.
- One detached worker processes only the earliest nonterminal sequence for a
  session. Caller cancellation never owns worker cancellation; different
  sessions may drain concurrently.
- Dispatch requires authoritative selection status `Ready` and
  `effective == desiredRuntime`. A linked cancelled switch cancels only
  Awaiting/Ready rows. Failed, superseded, or ambiguous preparation never
  falls back to the previous runtime.
- Interrupting an `Initializing` session before a ready effective binding exists
  cancels its `AwaitingRuntime` and `ReadyToDispatch` submissions with
  `message_submission_interrupted_before_dispatch`. In the same immediate
  transaction, materialize each cancelled payload as a user Timeline item in
  submission order, store that one-item result range and user-item id, and
  update the session timestamp. Publish the committed items through the normal
  live-event stream. This preserves the user's submitted history without
  calling the provider. It does not cancel the linked runtime switch and never
  rewrites `AboutToPrompt` or a later state; those states require the normal
  provider-aware interrupt and reconciliation path.
- A previous switch failure may be cleared before enqueue only by successful
  lifecycle materialization of the exact DB-current binding and activation
  generation. The recovery CAS also requires `desired == effective`, no
  pending switch, and a Current binding row; it does not relax the Ready gate
  or route a queued submission as fallback.
- The selection snapshot and linked switch status are separate reads. If the switch
  is observed `Committed` after an earlier non-ready snapshot, reread authoritative
  selection before classifying the submission: advance to Ready when the fresh state
  is Ready/effective at the requested runtime, and fail only when that fresh state
  still diverges.
- `ReadyToDispatch -> AboutToPrompt` is the durable no-replay boundary. The
  manager normally creates the user Timeline item only after that CAS,
  immediately before provider admission. Success stores the user item and
  Timeline range atomically with `Dispatched`, then advances to `Completed`.
  The initialization-time interrupt path above is the only pre-boundary
  materialization exception, and it finishes as `Cancelled` without provider
  admission.
- After claiming the session `Running`, a durable ACP turn rereads the DB
  runtime state and current `RuntimeBinding`. It must match desired/effective,
  Ready, no pending switch, Agent/AuthSource/Model/Effort/Mode, binding state,
  activation generation, and applied-generation evidence. The manager builds
  a temporary provider binding from that current row; it must not use the
  session DTO, import provenance, or caller payload as execution authority.
- ACP prepare and send both require the current committed attachment to match
  the binding id, activation generation, enabled Profile/Agent, effective
  config, and applied generation. Durable prepare never calls
  `ensure_attachment`; a missing or replaced attachment fails closed.
- Durable turns have no automatic Profile failover, streamed/final binding
  authority update, legacy history bridge, alternate direct dispatch, or
  ProviderKind routing fallback. Those paths could silently redirect a
  captured turn after admission.
- Every ordinary message, including Claude/Codex, requires the installed
  coordinator. A dropped coordinator fails closed without a direct dispatch or
  user Timeline fallback.
- An error after `AboutToPrompt` without a complete durable result fence is
  `AmbiguousPromptDispatch`; startup never calls the provider again. Completed
  retries reconstruct the response from the durable Timeline range.
- Submission delivery and Agent turn success are separate outcomes. If dispatch
  returns an error after the submitted user item and non-error Agent/Provider
  output have already been persisted beyond the pre-dispatch Timeline fence,
  reconstruct that exact range and complete the submission. Preserve any turn
  error in the authoritative Timeline/session state; do not also classify the
  already-observed delivery as ambiguous or surface a message-send failure.
- Prompt text, attachments, payload references, provider/native ids, tokens,
  workspace paths, and idempotency key values do not appear in Debug, tracing,
  errors, or public submission projections beyond the query contract's key.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Empty, oversized, or control-character message key | `message_submission_idempotency_key_required` / `_invalid`; zero writes. |
| Same key with another payload/runtime | `message_submission_idempotency_payload_conflict`; no second sequence or prompt. |
| Request effort differs from desired runtime | `message_submission_runtime_config_mismatch`; zero enqueue. |
| Runtime gate changes before manager admission | `message_submission_runtime_gate_changed`; no provider call. |
| Interrupt during initial runtime preparation | Atomically append each queued user payload to Timeline and cancel only `AwaitingRuntime` / `ReadyToDispatch`; publish the committed items and return success when at least one submission was cancelled, otherwise preserve the runtime readiness error. |
| Queued payload/session mismatch or cancellation CAS inconsistency | Roll back every Timeline/status write and return `message_submission_cancel_*`; never publish a partial user history. |
| Old `FailedUsingPrevious` state retries the already effective selection and exact current runtime materializes successfully | CAS the selection to `Ready`, clear only the session projection error, and retain the failed switch journal. |
| Linked switch commits between selection and switch reads | Reread selection; dispatch once if it converged, otherwise `message_submission_runtime_changed_after_commit`. |
| Current binding missing, stale, non-current, or config/generation mismatch | `message_submission_runtime_binding_*`; no provider call. |
| Current committed ACP attachment missing or mismatched | `turn_execution_identity_mismatch`; no `session/prompt`. |
| Installed ACP coordinator weak reference cannot upgrade | `message_submission_coordinator_unavailable`; no direct fallback or user Timeline item. |
| Provider/receipt uncertainty after AboutToPrompt | `message_submission_prompt_dispatch_ambiguous`; never replay automatically. |
| Dispatch returns an error after durable Agent/Provider output | Record the fenced Timeline range as Completed; retain the separate turn error/state. |
| Startup sees AboutToPrompt with complete result fence | Advance Dispatched then Completed without provider work. |

### 5. Good/Base/Bad Cases

- Good: a ForceFresh switch retains an inactive source RuntimeBinding, but
  durable send prompts only the DB-current target attachment once.
- Good: two queued messages with different desired runtimes switch and prompt
  in sequence; dropping either caller does not stop the worker.
- Good: the user interrupts a new session before runtime preparation completes;
  the submitted text remains in authoritative Timeline while no provider prompt
  is admitted.
- Base: a Claude or Codex ACP session with no explicit Effort/Mode uses the
  Adapter-converged defaults and still dispatches through the exact current
  binding once.
- Bad: call `ensure_attachment` from durable prepare using the old request
  binding, auto-fail over to another Profile, or replace durable current
  authority with an adapter-local binding.
- Bad: classify an unknown post-prompt error as Failed and resend on startup.
- Bad: cancel an initialization-time submission without first materializing its
  durable payload, leaving the selected session with an empty Timeline.

### 6. Tests Required

- Core tests cover serde fields and Debug redaction for send/query/state.
- DB tests cover atomic enqueue, exact conflict, sequence ordering, switch
  cancellation, session interrupt before the prompt boundary, atomic user
  Timeline preservation, idempotency, mismatch rollback, CAS transitions,
  dispatch-result transaction, and range load.
- Coordinator tests cover concurrent same-key submit, caller drop, per-session
  order, cross-session parallelism, desired gate, terminal switch outcomes,
  committed-switch selection-read races, AboutToPrompt ambiguity, and result-fenced
  recovery.
- Manager/ACP tests cover dropped-coordinator fail-closed, no durable
  failover/history bridge/binding update, prepare/send identity mismatch, and
  ForceFresh dispatch exclusively to the current target native session. Manager
  tests also cover initialization-time interrupt without an effective binding
  and assert that both `Initializing` and projected-`Running` snapshots preserve
  the submitted user Timeline item.
- Selection-service/ACP-lifecycle/DB tests cover healing stale
  `FailedUsingPrevious` through same-selection retry only after the exact
  current attachment activates; Ready same-selection remains a no-op, while
  mismatched fences and divergent desired state remain non-Ready.
- Remote/Desktop tests and checks cover canonical request/query serialization,
  authorization, production coordinator reuse, bindings drift, frontend
  typecheck/build, workspace tests, fmt, clippy, and Trellis validation.

### 7. Wrong vs Correct

#### Wrong

```text
Composer switch -> await switch -> manager reads legacy session binding
  -> ensure/restore attachment -> prompt -> retry unknown failures
```

#### Correct

```text
enqueue payload + desired runtime
  -> ordered worker makes effective == desired
  -> CAS AboutToPrompt
  -> manager rereads DB current binding/generation/config
  -> ACP revalidates current committed attachment under admission lock
  -> prompt once -> atomically persist Timeline result fence
  -> unknown post-boundary outcome stays Ambiguous

initialization-time interrupt
  -> immediate transaction appends queued user payloads in submission order
  -> records each one-item result range and marks the submissions Cancelled
  -> commit -> publish user Timeline items -> no provider prompt
```

## Scenario: Hidden Context Bridge Injection And Success-Only Cursor Commit

### 1. Scope / Trigger

- Trigger: the first durable ordinary message reaches a DB-current ACP binding
  with a pending incremental Context Bridge, or any later completed durable turn
  must advance that binding's consumed Timeline cursor.
- The bridge is internal provider continuity data. It is not a public Timeline,
  Remote, Desktop, or generated TypeScript contract.

### 2. Signatures

```text
PreparedContextBridge::provider_text(current_user_text) -> provider_text
ContextBridgeService::pending_for_turn(
  session_id, binding_id, activation_generation,
) -> Option<PreparedContextBridge>
ContextBridgeRepository::record_successful_turn(
  session_id, binding_id, activation_generation,
  submission_id, consumed_context_sequence,
) -> Option<ContextBridgeRecord>
ProviderTurnResult { events, binding_update, completed }
```

### 3. Contracts

- After the durable current-binding/config fence and before appending the user
  Timeline item, the manager loads at most one pending bridge, rebuilds its exact
  stored window, and verifies version, summary sequence, and SHA-256 fingerprint.
- Version 1 projection is deterministic and bounded. Priority is older-turn
  rolling summary, recent final User/Assistant turns, latest Todo state,
  unfinished Plan, deduplicated key files, then bounded tool/command/web result
  summaries. Budget reduction drops low-priority and older entries first.
- Always exclude reasoning, deltas, permissions, attachments/image references,
  raw extensions, ToolCall input/native ids, command text/cwd/full output,
  FileOperation old/new text, and Redacted items. Sanitize remaining text for
  credential assignments/tokens, private home paths, `.env`, credential files,
  and private-key/certificate paths before hashing or rendering.
- Only provider text receives `VIBEX_CONTEXT_BRIDGE_*` and current-message
  delimiters. The current user text inside that prefix is not trimmed or
  rewritten. The authoritative UserMessage stores the exact original text and
  attachments; no bridge/system item is appended to the public Timeline.
- `ProviderTurnResult.completed == true` and persisted provider events are
  prerequisites for cursor commit. Provider errors, cancellation, stale fences,
  fingerprint mismatch, and `completed == false` (including unresolved
  permission) leave every cursor and pending attempt unchanged.
- A completed durable turn atomically rechecks session current binding plus
  activation generation, monotonically advances `last_context_sequence`, and,
  when pending, advances summary/version, marks the attempt applied, and appends
  one audit-only `ContextBridgeApplied` event. Completed turns without a pending
  bridge still advance `last_context_sequence`.
- Debug, errors, bridge metadata, runtime-switch events, and submission
  projections expose no bridge body, prompt, fingerprint, secret, native id, or
  private database path.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Binding/session/generation is stale before prompt | `context_bridge_turn_binding_stale`; no UserMessage/provider call. |
| Pending metadata does not match binding cursors/version | `context_bridge_record_mismatch`; no provider call. |
| Timeline rebuild differs from durable fingerprint | `context_bridge_snapshot_changed`; no provider call. |
| Completed result sequence is before prepared window | `context_bridge_consumed_sequence_invalid`; transaction rollback. |
| Current pointer/generation changes before commit | `context_bridge_turn_fence_stale`; no cursor/apply event. |
| Provider returns error or `completed=false` | persist normal allowed Timeline state, but keep bridge pending and cursors unchanged. |

### 5. Good/Base/Bad Cases

- Good: provider receives bounded continuity plus the exact current message;
  clients see only the original UserMessage and normal provider events.
- Good: an unresolved permission produces NeedsInput while the bridge stays
  pending; no cursor claims that the turn completed.
- Base: a completed turn with no pending bridge advances only the context
  cursor; an equal/lower retry is a zero-write no-op.
- Bad: mark the bridge applied when `session/prompt` returns an incomplete
  permission state, or write the rendered prefix into Timeline/audit/Debug.
- Bad: advance only through `prepare_sequence` and omit the successful turn's
  own User/Assistant sequences, causing them to replay after switching back.

### 6. Tests Required

- Builder tests cover deterministic priority/budgets/dedupe and leakage for
  secrets, private paths, Redacted items, reasoning/deltas, attachment/image
  references, raw extensions, file old/new text, command input/full output, and
  duplicate tool events.
- Manager/ACP tests assert hidden provider prefix, exact public UserMessage,
  successful atomic apply, normal no-pending advancement, stale fence rollback,
  provider-error no progress, and unresolved-permission no progress.
- Integration tests assert ForceFresh first-turn apply and A -> B -> A no
  duplicate replay. Debug/error/audit JSON scans must not find bridge content.

### 7. Wrong vs Correct

#### Wrong

```text
append bridge as a SystemMessage
send prompt -> immediately advance cursor
permission pending -> mark attempt applied
```

#### Correct

```text
verify pending bridge under current binding fence
  -> prefix provider text only
  -> persist original UserMessage and provider events
  -> require completed=true
  -> atomically recheck fence, advance cursors, apply attempt, append audit event
```

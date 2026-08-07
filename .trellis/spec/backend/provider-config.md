# Provider Configuration

Vibex owns Provider Profile state as a first-class product feature. Provider
configuration is not a thin environment-variable editor and must not globally
rewrite the user's native Agent configuration by default.

Evidence: current Provider configuration code/tests and completed Provider tasks.

> Legacy cutover note (2026-07-29): later Tauri commands, files from the former
> React/Tauri tree that once occupied `apps/desktop`, generated browser bindings,
> and React form/query flows are retained historical evidence. Current Provider
> operations enter through typed services and the GPUI Backend/Remote boundaries;
> the deleted adapters are not maintenance surfaces.

## Source of Truth

Vibex stores its own Provider Profile SSOT with:

- Base URL and endpoints.
- API key, auth token, OAuth/account references, and account alias.
- Model defaults, small/large model choices, reasoning effort, sandbox and
  network defaults.
- Headers, proxy settings, CLI env, and provider-specific options.
- Permission defaults.
- MCP, Skills, prompts, and app matrix relationships.

New online sessions must bind to an enabled ACP Provider Profile. Project and
workspace defaults may select a profile, and session-level overrides must be
recorded explicitly in `SessionRuntimeSelection`.

Claude/Codex configuration and import APIs may also create profiles whose
`ProviderProfile.kind` is `claude` or `codex`. Those records preserve API-probe,
secret, native-import, and provenance behavior only. They are not eligible for
the Runtime Option Catalog, runtime binding, or online dispatch. An online
Claude/Codex profile uses the same concrete `agent_id` with `kind = acp` and a
typed managed-Adapter configuration.

## Assistant and Workflow Presets

Assistant/Workflow Presets are not Provider Profiles. A Provider Profile answers
"which execution backend and credentials should this session use"; a Preset
answers "which task workflow defaults should be applied."

Preset records may reference:

- A default Provider Profile or provider kind.
- Model, reasoning effort, sandbox, network, and permission defaults.
- Enabled MCP servers, Skills, prompts, and disabled built-in tools.
- Context templates, display metadata, localization, sort order, and enabled
  state.

Detected execution engines only describe what is available on the machine. Do
not store detected engines as user-facing assistant presets, and do not copy
Provider secrets into preset records. Scheduled tasks, delegation flows, and
remote "new session" shortcuts should reference a preset plus explicit
overrides instead of duplicating Provider Profile fields.

## Runtime Injection Order

When starting or resuming an online Agent session, project the selected ACP
profile in this order:

1. Typed ACP `session/new`, `session/load`, or negotiated session configuration.
2. Managed Adapter command and CLI arguments.
3. Process environment variables.
4. A controlled, session/profile-stable runtime home or configuration overlay.

Do not inject through a Native Claude/Codex SDK and do not write user home
configuration as the default switching mechanism.

If a provider stores native conversation state under its profile/config home
such as Codex `CODEX_HOME`, the controlled profile directory must be stable for
the Vibex session and Provider Profile. A per-turn temporary directory is only
valid for stateless probes such as model listing; it must not be used for turns
that later resume a native thread/session id.

## Scenario: ACP Agent Authentication And Profile Secret Transactions

### 1. Scope / Trigger

- Trigger: an ACP Agent advertises an environment-variable authentication
  method and the Management Center saves or clears its credentials.
- Trigger: an authentication method must work across Agents whose environment
  key names differ, while preserving Vibex's multi-Profile switching and
  keychain ownership.
- This contract crosses ACP discovery, Provider Profile persistence, OS
  keychain writes, legacy Provider projection, and runtime process injection.

### 2. Signatures

```text
update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest)
  -> ProviderProfile

AgentAuthEnvironmentUpdateRequest {
  agent_id, provider_profile_id, method_id,
  values: AgentAuthEnvironmentValue[]
}
AgentAuthEnvironmentValue {
  name, value?, secret, optional, clear
}

AcpProviderEnvReference {
  key, source: literal | process_environment | secret_reference,
  value?, secret_lookup_key?, redacted_hint
}
```

### 3. Contracts

- The method id and exact variable names come from the same
  `initialize.authMethods` catalog shown to the user. The service preserves
  the Agent's key spelling; it does not normalize every credential to
  `API_KEY`.
- `secret = true` stores plaintext only through the OS keychain and records an
  opaque Profile-local `ProviderSecretReference`. The ACP config stores a
  `SecretReference`, never the value. Non-secret values use a typed literal
  env reference.
- A blank value with `clear = false` preserves an existing reference. Removal
  requires `clear = true`; clearing a required variable leaves the Profile
  without a usable credential and the UI reports authentication required.
- Profiles never share secret lookup keys by accident: a new key is scoped to
  the Profile and exact environment name. Existing references are updated in
  place so Profile switching remains stable.
- The transaction captures previous keychain values, applies new values, then
  commits the Profile and secret-reference rows together. A database/readback/
  commit failure restores prior keychain values and removes newly created ones.
  Obsolete keychain entries are deleted only after the Profile commit succeeds.
- Legacy Provider projection may already provide an environment key. The
  update preserves an existing projected reference when the user leaves the
  masked field blank, and removes it only on explicit replacement/clear.
- Runtime resolution reads the selected Profile's references at process launch;
  authentication changes do not write native Agent config files or global user
  home state.
- Secret values and resolved keychain material are absent from Profile JSON,
  diagnostics, Debug, runtime-option snapshots, and projection previews.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Profile missing | `provider_profile_not_found`. |
| Profile Agent differs from request Agent | `agent_auth_profile_mismatch`. |
| Profile is not ACP | `agent_auth_profile_kind_invalid`. |
| Empty/control/oversized method id | `agent_auth_method_id_invalid`. |
| Empty, malformed, or duplicate env key | `agent_auth_env_key_invalid`. |
| `clear = true` and a non-empty value are both supplied | `agent_auth_env_clear_value_conflict`. |
| Required value is absent and no existing reference is preserved | `agent_auth_env_value_required`. |
| Keychain write/readback fails | return the storage error and restore every already-applied keychain value. |
| Profile transaction fails after keychain writes | return the storage error and report `keychainRollbackFailures` only when restoration also fails. |

### 5. Good / Base / Bad Cases

- Good: one Agent advertises `GEMINI_API_KEY`, another advertises
  `OPENAI_API_KEY`; each Profile stores the exact key in its own reference and
  the runtime injects only the selected Profile's value.
- Good: editing a masked configured field while leaving it blank keeps the
  previous keychain value; pressing Clear removes the reference explicitly.
- Base: a non-secret project id is stored as a literal env value and appears
  only in redacted configuration metadata.
- Bad: write a secret into `AcpProviderConfig.value`, reuse one global lookup
  key for every Profile, or delete the old keychain value before the DB commit.
- Bad: save credentials by editing a native Agent config file or trigger a
  hidden `session/new` probe for every Profile save.

### 6. Tests Required

- `cargo test -p vibex-config-switch agent_auth_environment_preserves_and_clears_projected_profile_secrets --locked`
  asserts projection compatibility and explicit clear semantics.
- `cargo test -p vibex-config-switch agent_auth_environment_preserves_blank_values_and_requires_explicit_clear --locked`
  asserts masked-input preservation and required-field validation.
- `cargo test -p vibex-config-switch agent_auth_environment_rolls_back_keychain_writes_when_database_commit_fails --locked`
  asserts transaction rollback and no leaked new secret.
- Provider projection/debug tests assert that serialized Profiles and previews
  contain lookup metadata only, never plaintext values.
- Runtime authentication tests assert the selected Profile's env overlay is
  used and another Profile's secret is never injected.

### 7. Wrong vs Correct

#### Wrong

```rust
profile.provider_options.insert("API_KEY", user_input);
```

#### Correct

```rust
let lookup_key = profile_scoped_lookup_key(&profile.id, &advertised_name);
store_keychain(&lookup_key, value)?;
profile.env.push(secret_reference(&advertised_name, &lookup_key));
commit_profile_or_restore_keychain(previous_values)?;
```

The Profile owns the reference and switching context; the Agent owns the exact
environment key and the final ACP authentication decision.

## Import and Export

Import existing Claude/Codex configuration in read-only mode by default. Imported
profiles are Vibex records and do not imply future writes back to native files.

Export to native Agent configuration only after explicit user confirmation. The
export flow must show:

- A diff of native files to change.
- A backup path.
- The atomic write plan.
- The rollback plan.
- Marker-owned deletion behavior where Vibex writes managed blocks.

Failed export must leave native configuration either unchanged or restored from
backup.

## Scenario: Native Import Secret Keychain Fallback

### 1. Scope / Trigger

- Trigger: Native Provider import creates a Vibex Provider Profile from
  Claude/Codex/cc-switch data and may discover a plaintext secret in the native
  source. This crosses file/SQLite parsing, secret storage, Provider profile
  persistence, Tauri responses, and frontend feedback.

### 2. Signatures

```text
provider_create_profile_from_import(ProviderNativeImportCreateRequest)
  -> ProviderNativeImportCreateResult {
       profile: ProviderProfile,
       source: ProviderNativeImportSource,
       diagnostics: ProviderNativeImportDiagnostic[]
     }

secrets::store_provider_secret(lookup_key, secret) -> VibexResult<()>
ProviderSecretReferenceCreateRequest {
  backend: os_keychain | environment | external | placeholder,
  setup_state: available | referenced | missing
}
```

### 3. Contracts

- Preview remains read-only and must never write native files, Vibex profiles,
  or OS keychain entries.
- Create may migrate a discovered cc-switch secret into the Vibex OS keychain.
- If OS keychain storage succeeds, replace the imported placeholder secret with
  an `os_keychain` reference in `available` state.
- If OS keychain storage fails, do not fail the import and do not persist the
  plaintext secret anywhere in Vibex. Keep or create a `placeholder` reference
  in `missing` state so the user can enter the secret later.
- Return a `ProviderNativeImportDiagnostic` with code
  `provider_native_import_cc_switch_secret_keychain_unavailable` when the
  fallback path is used. Redacted details may include backend and stable error
  code, but not the secret value.

### 4. Validation & Error Matrix

- Missing import item -> `provider_native_import_item_not_found`.
- Blocked parse item -> `provider_native_import_item_not_importable`.
- cc-switch database unreadable or item metadata missing -> structured
  validation/storage error; do not silently create a profile from incomplete
  identity data.
- cc-switch secret found and keychain write succeeds -> profile secret backend
  `os_keychain`, setup state `available`.
- cc-switch secret found and keychain write fails -> import succeeds, profile
  secret backend `placeholder`, setup state `missing`, diagnostic code
  `provider_native_import_cc_switch_secret_keychain_unavailable`.

### 5. Good/Base/Bad Cases

- Good: cc-switch Codex provider with `OPENAI_API_KEY` imports a profile and
  stores the API key in OS keychain; debug/profile output contains no plaintext
  secret.
- Base: OS keychain is unavailable on Linux. Import still creates the profile,
  returns a diagnostic, and leaves API Key setup required.
- Bad: Import fails solely because Secret Service/keyring is unavailable.
- Bad: Fallback stores the cc-switch plaintext secret in Provider options,
  diagnostics, SQLite, localStorage, or logs.

### 6. Tests Required

- `cargo test -p vibex-config-switch native_import` must include:
  - successful cc-switch secret migration to `os_keychain`;
  - keychain store failure fallback to `placeholder` + `missing`;
  - no plaintext secret in `ProviderProfile` debug output or diagnostics;
  - idempotent repeated cc-switch imports.
- Frontend checks must cover the import mutation path and display a warning
  toast when the fallback diagnostic is returned.

### 7. Wrong vs Correct

#### Wrong

```rust
secrets::store_provider_secret(&lookup_key, &secret_value)?;
```

This turns a local keychain availability problem into a full import failure.

#### Correct

```rust
if let Err(error) = secrets::store_provider_secret(&lookup_key, &secret_value) {
    keep_missing_placeholder_secret();
    diagnostics.push(keychain_unavailable_diagnostic(error.code));
}
```

The profile import completes, the secret is not persisted as plaintext, and the
UI can tell the user that API Key setup is still required.

## Scenario: Provider Profile Metadata Update Does Not Touch Secrets

### 1. Scope / Trigger

- Trigger: Desktop Provider settings update a model Provider Profile from a
  form that also displays an editable API key/auth JSON view. This crosses
  React form state, TanStack Query invalidation, Tauri commands, SQLite
  Provider Profile persistence, and OS keychain side effects.

### 2. Signatures

```text
agent_model_provider_update_profile(AgentModelProviderProfileUpdateRequest)
  -> ProviderProfile

agent_model_provider_update_secret_value(
  AgentModelProviderProfileSecretValueUpdateRequest {
    agentId,
    providerProfileId,
    value: string | null,
    clear: boolean
  }
) -> AgentModelProviderProfileSecretValueResponse

Provider settings form local state:
  apiKeyTouched: boolean
```

### 3. Contracts

- Profile metadata updates and secret updates are separate commands.
- The UI must call `agent_model_provider_update_profile` for display name,
  endpoint, model, and provider option changes.
- The UI must call `agent_model_provider_update_secret_value` only when the
  user explicitly changed the API key field or a parseable auth JSON key/value.
- Server/query refreshes that populate the API key field must not mark the
  secret as touched.
- Re-fetching the selected Provider Profile after a successful mutation must
  not reset an open editor draft for the same profile id.
- Empty API key means clear only when `apiKeyTouched = true`; an untouched empty
  field means "leave the existing secret as-is".

### 4. Validation & Error Matrix

- Metadata-only update with untouched API key ->
  `agent_model_provider_update_profile` only; no keychain read/write/delete.
- User edits API key to non-empty value ->
  `agent_model_provider_update_secret_value(clear=false, value=<trimmed>)`.
- User edits API key to empty value ->
  `agent_model_provider_update_secret_value(clear=true)`.
- Secret update keychain failure -> user-visible save error; profile metadata
  may already be saved, but the dialog must not silently close as success.
- Query invalidation returns a new `ProviderProfile` object with the same id ->
  keep the active draft instead of reinitializing from server state.

### 5. Good/Base/Bad Cases

- Good: user edits only Base URL and saves; Vibex updates Provider Profile
  metadata and leaves the OS keychain untouched.
- Good: user clears the API key field and saves; Vibex sends an explicit clear
  request and removes the secret reference if deletion succeeds.
- Base: opening an editor loads a secret from keychain and fills the form, but
  saving without editing the key does not re-store the same secret.
- Bad: every profile save sends an empty `value` with `clear=true`, deleting a
  valid secret during metadata-only edits.
- Bad: every profile save re-stores the currently loaded key, causing a local
  keychain outage to break unrelated endpoint/model updates.

### 6. Tests Required

- Frontend checks must cover metadata-only profile save without invoking the
  secret update mutation.
- Frontend checks must cover API key edit and clear paths invoking the secret
  update mutation exactly when touched.
- `pnpm --dir apps/desktop typecheck` and `pnpm check:frontend` must pass after
  changing Provider settings form state or mutation wiring.
- Backend secret update tests remain responsible for keychain store/delete
  behavior once an explicit secret update request reaches the service.

### 7. Wrong vs Correct

#### Wrong

```typescript
await updateProfile.mutateAsync(profilePayload);
await updateSecretValue.mutateAsync({
  agentId,
  providerProfileId,
  value: profileForm.apiKey,
  clear: profileForm.apiKey.trim().length === 0
});
```

This turns an untouched empty form field into an explicit secret clear and can
trigger OS keychain failures during metadata-only updates.

#### Correct

```typescript
await updateProfile.mutateAsync(profilePayload);
if (apiKeyTouched) {
  await updateSecretValue.mutateAsync({
    agentId,
    providerProfileId,
    value: profileForm.apiKey,
    clear: profileForm.apiKey.trim().length === 0
  });
}
```

The metadata update path stays independent from keychain side effects unless
the user intentionally edits the secret.

## Injection Preview

Before creating a session, expose a redacted preview of:

- Selected Provider Profile.
- Endpoint and model.
- Typed ACP session and Adapter options.
- CLI args.
- Env keys and redacted values.
- Temporary config directory or overlay files.
- MCP and Skills that will be enabled.
- Sandbox, network, and permission defaults.

The preview must be available for diagnostics even if the UI only shows a
compact summary.

## Dynamic Switching

Running sessions may switch Provider Profile fields only within evidence-backed
ACP boundaries. If the Adapter cannot hot-switch a field, the service must
prepare a replacement attachment and either resume or bridge current context.
Do not silently mutate global config and pretend the running session changed.

Every switch attempt, success, or failure is durable switch audit data. Only a
bounded actionable terminal failure becomes a conversation system notice.

## Health and Usage

Provider health checks should be split into independent probes:

- Binary exists.
- Version.
- Auth status.
- Model list.
- Streaming first byte.
- Simple prompt.

Usage lookup may be provider-specific and should be recorded separately from
session timeline data.

## MCP and Skills

MCP and Skills are managed as shared Vibex resources with Agent matrices. The
same MCP server or Skill may be enabled for Claude, Codex, or other ACP-backed
Agents independently. `ProviderKind` matrices may describe configuration or
import provenance, but online runtime injection and command discovery resolve a
concrete `agent_id` and an enabled ACP profile without ProviderKind fallback.

Default behavior is Vibex session injection. Native config export follows the
same diff, backup, atomic write, and rollback requirements as Provider export.

## Scenario: Skills Prompts Hooks Resource Management Baseline

### 1. Scope / Trigger

- Trigger: Phase 3 adds Vibex-owned Skills, Prompts, and preview-only Hooks
  across `crates/core`, SQLite schema v8, `vibex-config-switch`, Tauri
  commands, shared Rust DTOs, browser mocks, React Query hooks,
  Provider settings UI, and injection preview.
- This is a cross-layer contract because resource records flow from storage
  through service validation and Tauri APIs into the desktop Provider settings
  surface and redacted injection preview.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
skill_list() -> Vec<Skill>
skill_create(SkillCreateRequest) -> Skill
skill_update(SkillUpdateRequest) -> Skill
skill_delete(SkillDeleteRequest) -> ()
skill_set_provider_matrix(SkillSetProviderMatrixRequest) -> Skill
skill_validate(SkillValidateRequest) -> SkillValidationResult

prompt_list() -> Vec<Prompt>
prompt_create(PromptCreateRequest) -> Prompt
prompt_update(PromptUpdateRequest) -> Prompt
prompt_delete(PromptDeleteRequest) -> ()
prompt_validate(PromptValidateRequest) -> PromptValidationResult

hook_list() -> Vec<Hook>
hook_create(HookCreateRequest) -> Hook
hook_update(HookUpdateRequest) -> Hook
hook_delete(HookDeleteRequest) -> ()
hook_preview_install(HookInstallPreviewRequest) -> HookInstallPreview

provider_preview_injection(ProviderInjectionPreviewRequest)
  -> ProviderInjectionPreview { mcp_servers: Vec<String>, skills: Vec<String>, ... }
```

SQLite schema version 8 owns:

```text
skills(
  skill_id, display_name, source_kind, status, scope_kind, project_id,
  workspace_id, source_uri, description, tags_json, content_preview,
  created_at_ms, updated_at_ms, deleted_at_ms
)

skill_provider_matrix(
  skill_id, provider_kind, enabled, created_at_ms, updated_at_ms
)

prompts(
  prompt_id, display_name, kind, status, scope_kind, project_id,
  workspace_id, body, description, tags_json, created_at_ms,
  updated_at_ms, deleted_at_ms
)

hooks(
  hook_id, display_name, provider_kind, event_kind, status, install_state,
  command_preview, managed_marker, description, created_at_ms,
  updated_at_ms, deleted_at_ms
)

hook_install_previews(
  preview_id, hook_id, target_path, marker, redacted_preview, created_at_ms
)
```

### 3. Contracts

- `crates/core` is the source of truth for `Skill*`, `Prompt*`, `Hook*`, and
  `HookInstallPreview*` DTOs plus `SkillId`, `PromptId`, and `HookId`.
- Skills are Provider-neutral Vibex resources. `SkillProviderMatrix` controls
  per-Provider enablement for `codex`, `claude`, `acp`, and `mock`.
- Skill sources are metadata-only in this baseline:
  `manual`, `git_repo`, `local_folder`, and `marketplace`. Default validation
  must not clone, fetch, scan folders, or write native skill files.
- Prompts are Vibex-owned text records. Enabled prompts appear in injection
  preview summaries, but native prompt import/export and slash-command palette
  runtime integration are later explicit flows.
- Hooks store managed hook intent and install preview metadata only. The
  baseline must not mutate shell startup files, `~/.claude`, Codex config, or
  hook files.
- `ProviderInjectionPreview.skills` contains concise display entries for
  enabled Skills and enabled Prompts. Entries must be diagnostic summaries, not
  provider-native config blobs.

### 4. Validation & Error Matrix

- Empty Skill display name -> `validation/skill_name_empty`.
- Manual Skill without description/content preview ->
  `validation/skill_manual_content_missing`.
- Non-manual Skill with invalid source URI shape ->
  `validation/skill_source_uri_invalid`.
- Missing Skill lookup -> `validation/skill_not_found`.
- Empty Prompt display name -> `validation/prompt_name_empty`.
- Empty Prompt body -> `validation/prompt_body_empty`.
- Missing Prompt lookup -> `validation/prompt_not_found`.
- Empty Hook display name -> `validation/hook_name_empty`.
- Missing Hook lookup -> `validation/hook_not_found`.
- Hook install preview persistence failure -> `storage/hook_install_preview_failed`.
- SQLite insert/update/delete/matrix failures -> `storage/skill_*`,
  `storage/prompt_*`, or `storage/hook_*`.

### 5. Good/Base/Bad Cases

- Good: A user creates one manual Skill, enables it for Mock/Codex, creates one
  reusable Prompt, and sees both in Provider injection preview without native
  config writes.
- Base: No Skills, Prompts, or Hooks exist; Provider Profiles, MCP servers, and
  injection preview still work, and preview resource arrays are empty.
- Bad: A validation button clones a Git Skill source, reads an arbitrary local
  skill folder, fetches a marketplace entry, writes `.claude/skills`, mutates
  Codex config, or installs a terminal hook.

### 6. Tests Required

- `cargo test -p vibex-db skill` must assert Skill persistence, Provider matrix
  persistence, enabled-provider lookup, and soft-delete behavior.
- `cargo test -p vibex-db prompt` must assert Prompt persistence, enabled
  prompt lookup, update behavior, and soft-delete behavior.
- `cargo test -p vibex-db hook` must assert Hook persistence and hook install
  preview metadata persistence without native writes.
- `cargo test -p vibex-config-switch skill` and `prompt` must assert
  deterministic no-network/no-native-write validation and injection preview
  inclusion.
- `cargo test -p vibex-config-switch hook` must assert preview-only Hook
  install metadata and `preview_only` state transition.
- `pnpm check:frontend` and `pnpm check` must pass
  after changing protocol or Provider settings UI.
- Provider settings screenshots are optional visual evidence; capture them for
  Skills/Prompts/Hooks work only when requested, when visual regression risk is
  high, or when local browser mock rendering is already part of validation.

### 7. Wrong vs Correct

#### Wrong

```rust
std::process::Command::new("git")
    .args(["clone", skill.source_uri.as_ref().unwrap()])
    .status()?;
```

This turns default Skill validation into network/filesystem side effects and
violates the metadata-only contract.

#### Correct

```rust
if skill.source_kind != SkillSourceKind::Manual
    && !skill_source_uri_shape_is_valid(skill.source_uri.as_deref())
{
    return Err(VibexError::validation(
        "skill_source_uri_invalid",
        "Skill source URI shape is invalid; no network or filesystem read was used",
    ));
}
```

The baseline only validates local metadata shape. Real clone, marketplace
install, folder scan, native Skill export, and Hook installation belong behind
later explicit user-consent flows.

## Scenario: MCP Resource Management And Injection Matrix

### 1. Scope / Trigger

- Trigger: Phase 3 adds Vibex-owned MCP server records, per-Provider enablement
  matrix, deterministic validation, Tauri commands, generated TypeScript
  bindings, browser mocks, and Provider settings UI.
- This is a cross-layer contract because MCP server records flow through
  `crates/core`, SQLite schema v7, `vibex-config-switch`, Tauri commands,
  canonical `crates/core` DTOs, browser mocks, React Query hooks, and injection preview UI.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
mcp_list_servers() -> Vec<McpServer>
mcp_create_server(McpServerCreateRequest) -> McpServer
mcp_update_server(McpServerUpdateRequest) -> McpServer
mcp_delete_server(McpServerDeleteRequest) -> ()
mcp_set_provider_matrix(McpServerSetProviderMatrixRequest) -> McpServer
mcp_validate_server(McpServerValidateRequest) -> McpServerValidationResult
provider_preview_injection(ProviderInjectionPreviewRequest)
  -> ProviderInjectionPreview { mcp_servers: Vec<String>, ... }
```

SQLite schema version 7 owns:

```text
mcp_servers(
  mcp_server_id, display_name, transport_kind, status, scope_kind,
  project_id, workspace_id, command, args_json, url, description,
  tags_json, created_at_ms, updated_at_ms, deleted_at_ms
)

mcp_server_secret_references(
  secret_ref_id, mcp_server_id, secret_kind, backend, setup_state,
  lookup_key, display_label, redacted_hint, target, created_at_ms,
  updated_at_ms
)

mcp_server_provider_matrix(
  mcp_server_id, provider_kind, enabled, created_at_ms, updated_at_ms
)
```

### 3. Contracts

- `crates/core` is the source of truth for `McpServer*` DTOs and
  `McpServerId`.
- MCP server transports are `stdio`, `http`, and `sse`.
- MCP scopes are `global`, `user`, `project`, and `workspace`.
- MCP server status is independent from per-Provider enablement. A server must
  be globally `enabled` and matrix-enabled for a Provider kind before it appears
  in that Provider's injection preview.
- MCP secrets store references only:
  `secret_kind`, `backend`, `setup_state`, `lookup_key`, `display_label`,
  `redacted_hint`, and target `environment` or `header`.
- `ProviderInjectionPreview.mcp_servers` must contain redacted, diagnostic
  display entries only. It must not contain plaintext env values, bearer
  tokens, raw auth headers, or provider-native config blobs.
- MCP management is Vibex-owned storage. Native Claude/Codex MCP export remains
  an explicit later flow with diff, backup, atomic write, and rollback.

### 4. Validation & Error Matrix

- Empty MCP display name -> `validation/mcp_server_name_empty`.
- Missing MCP lookup -> `validation/mcp_server_not_found`.
- `stdio` server without a non-empty command ->
  `validation/mcp_server_stdio_command_missing`.
- `http` or `sse` server without `http://` or `https://` URL shape ->
  `validation/mcp_server_url_invalid`.
- Validation target without id or candidate ->
  `validation/mcp_server_validation_target_missing`.
- SQLite insert/update/delete/matrix failure -> `storage/mcp_server_*`.
- Default MCP validation must not spawn a process and must not make a network
  request; it only checks metadata shape.

### 5. Good/Base/Bad Cases

- Good: A user creates one `stdio` filesystem MCP server, enables it for Mock
  and Codex, and sees redacted entries in injection preview without any native
  config write.
- Base: No MCP servers exist; Provider Profiles and injection preview still
  work, and `mcp_servers` is empty.
- Bad: A validation button starts `mcp-filesystem`, probes an HTTP endpoint, or
  writes `~/.claude` / Codex config as a side effect.

### 6. Tests Required

- `cargo test -p vibex-db mcp` must assert server persistence, matrix
  persistence, secret reference redaction, enabled-provider lookup, and
  soft-delete behavior.
- `cargo test -p vibex-config-switch mcp` must assert deterministic
  no-process/no-network validation and injection preview inclusion.
- Core protocol tests must pass after adding or changing MCP DTOs.
- `pnpm check:frontend` must pass after adding MCP hooks or UI.
- Provider settings screenshots are optional visual evidence for MCP UI work;
  capture them only when requested, when visual regression risk is high, or when
  local browser mock rendering is already part of validation.

### 7. Wrong vs Correct

#### Wrong

```rust
std::process::Command::new(server.command.unwrap()).status()?;
```

This turns default validation into arbitrary process execution and violates the
metadata-only MCP test contract.

#### Correct

```rust
if server.command.as_deref().is_none_or(|command| command.trim().is_empty()) {
    return Err(VibexError::validation(
        "mcp_server_stdio_command_missing",
        "stdio MCP servers require a command",
    ));
}
```

The check is deterministic and safe. Real process startup belongs behind an
explicit later smoke action, not behind default settings validation.

## Scenario: MCP And Skills Agent Matrix Resource Libraries

### 1. Scope / Trigger

- Trigger: Config Center Phase 3 moves MCP servers and Skills to resource-first
  libraries where each resource owns the concrete agent enablement matrix.
- This is a cross-layer contract because resource records flow through
  `crates/core`, SQLite schema v18, `vibex-config-switch`, Tauri commands,
  shared Rust DTOs, desktop browser mocks, React Query hooks, and
  Agent runtime command/resource injection.

### 2. Signatures

SQLite schema version 18 owns additive tables:

```text
mcp_server_agent_matrix(
  mcp_server_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms,
  PRIMARY KEY(mcp_server_id, agent_id)
)

skill_agent_matrix(
  skill_id, agent_id, enabled, source_kind, created_at_ms, updated_at_ms,
  PRIMARY KEY(skill_id, agent_id)
)
```

New resource-first Tauri commands:

```text
mcp_set_agent_matrix(McpServerSetAgentMatrixRequest) -> McpServer
mcp_list_agent_matrix(McpServerAgentMatrixListRequest) -> Vec<McpServerAgentMatrix>
mcp_list_for_agent(McpServerForAgentListRequest) -> Vec<McpServer>
mcp_discover_sources(McpServerDiscoverRequest) -> McpServerDiscoveryResponse
mcp_import_servers(McpServerImportRequest) -> McpServerImportResult

skill_set_agent_matrix(SkillSetAgentMatrixRequest) -> Skill
skill_list_agent_matrix(SkillAgentMatrixListRequest) -> Vec<SkillAgentMatrix>
skill_list_for_agent(SkillForAgentListRequest) -> Vec<Skill>
skill_discover_sources(SkillDiscoverRequest) -> SkillDiscoveryResponse
skill_import(SkillImportRequest) -> SkillImportResult
```

### 3. Contracts

- `crates/core` owns `McpServerAgentMatrix`, `SkillAgentMatrix`,
  discovery rows, import requests, and import results.
- `source_kind` describes matrix provenance: `manual`, `native_import`, or
  `legacy_backfill`.
- Migration must backfill old provider-kind matrix rows into built-in
  `agent_id` rows for `mock`, `claude`, `codex`, and `acp`.
- MCP and Skills are stored once per resource. Do not duplicate resources per
  agent just to represent enablement.
- Discovery reads native config or local Skill manifests only. Import creates
  or updates Vibex-owned resources and enables only `source_agent_id` unless
  explicit `enable_agent_ids` are supplied.
- Import must not create, edit, delete, back up, or rewrite native Claude,
  Codex, `$CODEX_HOME`, `$AGENTS_HOME`, `~/.claude`, `~/.codex`, or local
  `SKILL.md` files.
- Agent runtime startup summaries, `$` Skill command discovery, MCP lookup, and
  Skill lookup must query by concrete `agent_id` first and use provider-kind
  fallback only when no agent matrix row exists for a legacy resource.
- Full visual resource-library UI belongs to the Config Center UI phase; this
  phase exposes typed service, command, hook, and mock surfaces.

### 4. Validation & Error Matrix

- Missing MCP server on matrix update/list -> `validation/mcp_server_not_found`.
- Missing Skill on matrix update/list -> `validation/skill_not_found`.
- Invalid MCP candidate from import -> existing MCP validation codes such as
  `mcp_server_stdio_command_missing` or `mcp_server_url_invalid`.
- Missing Skill workspace during discovery -> `validation/workspace_not_found`.
- Agent matrix storage failure -> `storage/mcp_server_agent_matrix_*` or
  `storage/skill_agent_matrix_*`.
- Malformed native MCP JSON/TOML -> discovery diagnostic, not native file
  mutation.

### 5. Good/Base/Bad Cases

- Good: importing one Codex MCP server from `$CODEX_HOME/config.toml` creates
  one Vibex MCP resource, enables only `agent_id=codex`, and leaves the TOML
  file byte-for-byte unchanged.
- Good: importing one local Skill from `$AGENTS_HOME/skills/*/SKILL.md` creates
  one Vibex Skill, enables only `agent_id=claude`, and leaves `SKILL.md`
  unchanged.
- Base: an old resource only has provider-kind matrix rows; runtime lookups
  still find it through provider-kind fallback.
- Bad: Config Center creates separate duplicate Skill rows for Claude and
  Codex, or writes native config during import to "sync" the resource.

### 6. Tests Required

- `cargo test -p vibex-db mcp` and `cargo test -p vibex-db skill` must cover
  schema v18 matrices, backfill, round trips, agent lookup, legacy fallback,
  and soft-delete behavior.
- `cargo test -p vibex-config-switch mcp` and
  `cargo test -p vibex-config-switch skill` must cover discovery/import,
  read-only native source behavior, and source-agent default enablement.
- `cargo test -p vibex-agent resource` must cover runtime resource summaries
  using `agent_id`.
- Agent command discovery tests must cover `$` Skill entries coming from
  `skill_agent_matrix`, not only `skill_provider_matrix`.
- `cargo test -p vibex-desktop skill` and
  `pnpm --dir apps/desktop typecheck` must pass after command or binding
  surface changes.

### 7. Wrong vs Correct

#### Wrong

```rust
let skills = SkillRepository::list_enabled_for_provider(conn, provider_kind)?;
```

This ignores multiple ACP-backed agents that share one `ProviderKind::Acp` and
cannot represent resource enablement per concrete agent.

#### Correct

```rust
let skills = SkillRepository::list_enabled_for_agent(conn, &agent_id, provider_kind)?;
```

The repository checks the concrete agent matrix first and uses provider-kind
fallback only for legacy resources.

## Scenario: ACP MCP Runtime Forwarding

### 1. Scope / Trigger

- Trigger: ACP-backed sessions forward enabled Vibex MCP resources into native
  ACP `session/new` and `session/load` requests.
- This is a cross-layer contract because `crates/agent` resolves
  Provider-runtime resources from Provider Profile/workspace policy and
  `crates/agent-acp` validates and serializes those resources into ACP JSON-RPC
  payloads.

### 2. Signatures

Agent provider creation must carry runtime resources:

```text
ProviderCreateRequest {
  session_id: VibexSessionId,
  provider_profile_id: ProviderProfileId,
  model: Option<String>,
  workspace_root: String,
  safety: AgentSessionSafety,
  runtime_resources: ProviderRuntimeResources
}

AcpCreateSessionRequest { ..., runtime_resources: ProviderRuntimeResources }
AcpImportSessionRequest { ..., runtime_resources: ProviderRuntimeResources }
AcpSendTurnRequest { ..., runtime_resources: ProviderRuntimeResources }
```

ACP request builders serialize MCP descriptors into:

```json
{
  "cwd": "/workspace",
  "mcpServers": [
    {
      "id": "filesystem",
      "name": "Filesystem",
      "transport": "stdio",
      "command": "mcp-server-filesystem",
      "args": ["--root", "/workspace"]
    },
    {
      "id": "remote",
      "name": "Remote",
      "transport": "http",
      "url": "https://example.invalid/mcp"
    }
  ]
}
```

### 3. Contracts

- `AgentManager` resolves `ProviderRuntimeResources` immediately before
  provider `create_session`, import, failover, and Provider Profile switching.
- ACP adapters must not query MCP storage directly; they consume the resolved
  runtime resources supplied by `crates/agent`.
- MCP forwarding is profile/config feature-gated. ACP profiles must include
  `mcp` or `mcp_servers` before non-empty runtime MCP resources are serialized.
- When forwarding is disabled, `session/new`, `session/load`, and initialize
  client capabilities must behave as if the MCP resource list is empty.
- When forwarding is enabled, `initialize.params.clientCapabilities.mcpServers`
  is `true` only if at least one descriptor will be forwarded.
- Supported ACP MCP transports are `stdio`, `http`, and `sse`. `stdio`
  descriptors include `command` and `args`; `http`/`sse` descriptors include
  `url`.
- Debug logs and test request logs may record descriptor shape but must not
  include plaintext MCP secrets, bearer tokens, or raw auth headers.

### 4. Validation & Error Matrix

- Feature disabled with invalid runtime MCP resources -> forwarding omitted,
  no ACP MCP validation error.
- Missing descriptor id -> `acp_mcp_server_id_missing`.
- Missing descriptor display name -> `acp_mcp_server_name_missing`.
- `stdio` descriptor without a non-empty command ->
  `acp_mcp_stdio_command_missing`.
- `http` or `sse` descriptor without an `http://` or `https://` URL ->
  `acp_mcp_url_invalid`.
- Runtime-resource resolution failure before provider startup -> transition
  the Vibex session to `Error`, append a provider error event, and do not start
  the ACP process.

### 5. Good/Base/Bad Cases

- Good: an ACP profile with `mcp_servers` enabled receives validated MCP
  descriptors in both `session/new` and `session/load`.
- Good: disabled MCP resources and resources blocked by agent/workspace matrix
  are absent because runtime-resource resolution filters them before ACP sees
  them.
- Base: an ACP profile without `mcp`/`mcp_servers` starts normally with no MCP
  descriptors and `clientCapabilities.mcpServers = false`.
- Bad: ACP runtime starts and only then discovers an enabled MCP descriptor is
  missing a command.
- Bad: ACP adapter bypasses `ProviderRuntimeResources` and re-queries global
  MCP state, ignoring profile/workspace policy.

### 6. Tests Required

- `cargo test -p vibex-agent-acp session_request_builders` must assert
  `session/new` and `session/load` include serialized ACP MCP descriptors.
- `cargo test -p vibex-agent-acp acp_mcp` must assert feature gating and
  validation codes for invalid stdio/http/sse descriptors.
- Mock ACP runtime tests must log JSON-RPC requests and assert initialize,
  `session/new`, and `session/load` MCP payloads.
- `cargo test -p vibex-agent` must continue to cover Provider runtime-resource
  resolution paths when session creation, import, failover, or profile switch
  starts a provider.
- `cargo test -p vibex-config-switch -p vibex-agent-acp -p vibex-agent` must
  pass after ACP MCP forwarding contract changes.

### 7. Wrong vs Correct

#### Wrong

```rust
let descriptors = config_service.list_all_mcp_servers()?;
let params = build_session_new_params(&cwd, model.as_deref(), &descriptors);
```

This bypasses Provider Profile, workspace policy, concrete agent matrix, and
ACP feature gating.

#### Correct

```rust
let runtime_resources =
    manager.resolve_runtime_resources(provider_kind, &provider_profile_id)?;
provider.create_session(ProviderCreateRequest {
    provider_profile_id,
    runtime_resources,
    // ...
}).await?;
```

The manager resolves only approved runtime resources, and the ACP adapter
serializes them only when the ACP profile advertises MCP forwarding support.

## Scenario: ACP Process Registry And Process-Tree Lifecycle

### 1. Scope / Trigger

- Trigger: ACP session create/restore needs one owner for process identity,
  spawn/initialize de-duplication, optional multi-session reuse, crash fan-out,
  and complete process-tree shutdown.
- This is an infra/runtime contract in `crates/agent-acp`. Process ownership is
  separate from session attachment identity and native-session routing; P2-04
  formalizes the latter boundary.

### 2. Signatures

```text
ProcessAcquireKey {
  route_key: AgentRuntimeRouteKey,
  provider_profile_id: ProviderProfileId,
  process_spawn_fingerprint: String,
  workspace_scope: WorkspaceScope
}

WorkspaceScope::new(absolute_existing_path) -> VibexResult<WorkspaceScope>

decide_process_reuse(
  requested: bool,
  descriptor_support: Option<CapabilitySupport>,
  expected_compatibility_identity: Option<&str>,
  evidence: Option<&MultiSessionContractEvidence>
) -> ProcessReuseDecision

AcpProcessRegistry::acquire_reusable(key, spawn, initialize)
  -> VibexResult<ProcessLease<AcpProcess>>
AcpProcessRegistry::acquire_dedicated(key, spawn, initialize)
  -> VibexResult<ProcessLease<AcpProcess>>

ProcessLease::attach() -> VibexResult<()>
ProcessLease::detach() -> VibexResult<usize>
ProcessLease::subscribe_crashes() -> VibexResult<Receiver<AcpProcessCrash>>
ProcessLease::snapshot() -> VibexResult<AcpProcessSnapshot>
```

```text
AcpProcessSnapshot {
  process_instance_id: AcpProcessInstanceId,
  process_spawn_fingerprint: String,
  status: Starting | Ready | Closing | Closed | Crashed,
  protocol_version: Option<i64>,
  attached_session_count: usize,
  pending_request_count: usize
}
```

### 3. Contracts

- `ProcessAcquireKey` contains only route, Provider Profile, versioned spawn
  fingerprint, and canonical workspace scope. Session id, binding id, native
  session id, and PID must never participate in process identity.
- A per-key async lock covers `lookup ready -> spawn -> initialize -> register
  ready`. Registry state locks must never be held across `await`. Different
  keys may initialize concurrently.
- Native `session/new`, `session/load`, model/config application, prompt, and
  event routing run after the process acquire lock has been released.
- Dedicated process mode is the default. Shared reuse requires all of:
  explicit request, exact descriptor `safe_multi_session == Supported`, an
  exact compatibility-identity match, `RealManagedAdapter` evidence, verified
  interleaved session routing, and verified crash isolation. Profile feature
  strings, fixtures, mocks, or either evidence source alone cannot unlock it.
- The runtime reserves one attachment count immediately after process acquire
  and before native `session/new`/`session/load`. Startup failure releases only
  that reservation and shuts the process down only when the remaining count is
  zero. This prevents one failed concurrent attachment from terminating a
  shared process still being attached elsewhere.
- Every spawn receives a unique `AcpProcessInstanceId`. Crash, close, and
  eviction compare both key and instance id, so a late old-instance callback
  cannot evict a replacement registered under the same key.
- Unexpected exit marks the instance `Crashed`, evicts it from reusable lookup,
  and broadcasts one bounded recoverable `acp_process_exited` event. A later
  acquire may create a replacement.
- Shutdown is idempotent and ordered: reject new prompts, cancel active native
  sessions, resolve pending host requests, close ACP stdin, wait to a deadline,
  kill the Unix process group or Windows Job Object if needed, reap, then fail
  and clear all pending state.
- A test-only shortened graceful deadline must still leave scheduler and OS
  process-reaping headroom under concurrent workspace checks. The harness outer
  deadline must exceed the inner graceful deadline; scheduler contention alone must
  not turn a graceful exit fixture into a process-group kill.
- Snapshot, crash event, fallback metadata, and logs must not include PID,
  native session id, prompt text, environment values, or secrets.

### 4. Validation & Error Matrix

- Relative workspace -> `validation/acp_workspace_scope_relative`.
- Missing canonical workspace -> `validation/acp_workspace_scope_missing`.
- Empty spawn fingerprint -> `validation/acp_process_spawn_fingerprint_empty`.
- Reuse not requested -> dedicated, with no fallback reason.
- Descriptor absent/unknown/unsupported -> dedicated with
  `acp_multi_session_descriptor_not_verified`.
- Expected compatibility identity absent -> dedicated with
  `acp_multi_session_identity_missing`.
- Exact contract evidence absent -> dedicated with
  `acp_multi_session_contract_missing`.
- Fixture/mock/unknown evidence, identity mismatch, or either verification
  false -> dedicated with `acp_multi_session_contract_not_verified`.
- Both exact real evidence sources pass -> shared reuse is allowed.
- Spawn/initialize/registration/ready failure -> shut down any spawned process,
  remove the failed entry, return the typed cause, and allow retry.
- Attach to non-ready/closed instance -> `process/acp_process_not_ready`.
- Unexpected exit -> one recoverable `acp_process_exited` broadcast; duplicate
  reports are ignored.
- Graceful fixture exits before the inner deadline -> no `kill_process_group` event.
  A live leader or descendant after the deadline -> group kill, reap, and bounded
  close.

### 5. Good/Base/Bad Cases

- Good: concurrent same-key acquires with complete real evidence spawn and
  initialize once; each caller reserves an attachment before `session/new`.
- Good: an old process reports EOF after its same-key replacement is ready;
  only the old instance changes state and the replacement remains reusable.
- Base: built-in Claude/Codex descriptors do not prove safe multi-session, so
  requested pooling conservatively falls back to dedicated processes.
- Bad: a profile `features` string alone enables sharing.
- Bad: `session/new` fails for one caller, which observes zero committed
  bindings and kills a process another caller is still attaching to.
- Bad: shutdown kills only the adapter leader and leaves its Agent CLI or shell
  descendants alive.
- Bad: use a 250 ms test-only process deadline that intermittently records a group
  kill when workspace tests and build smoke contend for CPU.

### 6. Tests Required

- `cargo test -p vibex-agent-acp process_registry` must assert same-key
  de-duplication, different-key concurrency, dedicated isolation, the complete
  evidence matrix, failure cleanup/retry, crash fan-out, instance-aware
  eviction, bounded snapshots, and concurrent idempotent shutdown.
- ACP runtime tests must prove `session/new` is outside the acquire lock and one
  failed concurrent `session/new` does not shut down another attachment's
  shared process.
- Unix lifecycle integration must prove graceful exit avoids group kill and a
  timed-out adapter leader plus descendant are group-killed and reaped.
- Run the graceful and forced lifecycle tests together and in the full workspace
  suite; the outer graceful-test timeout must remain larger than the test-only
  process deadline.
- `cargo test -p vibex-agent-acp`, `cargo test -p vibex-agent`,
  `cargo test -p vibex-config-switch`, and `cargo check --workspace --all-targets`
  must pass without starting real providers.

### 7. Wrong vs Correct

#### Wrong

```rust
if config.features.contains(&"safe_multi_session".to_string()) {
    reuse_process_by_profile(profile_id).await?;
}
```

This lets mutable profile metadata bypass compatibility evidence and collapses
process identity across route, workspace, and spawn configuration.

#### Correct

```rust
let decision = decide_process_reuse(
    requested,
    Some(descriptor.safe_multi_session.support),
    Some(descriptor.expected_compatibility_identity()),
    exact_real_contract_evidence.as_ref(),
);
let lease = if decision.allows_shared_process() {
    registry.acquire_reusable(key, spawn, initialize).await?
} else {
    registry.acquire_dedicated(key, spawn, initialize).await?
};
lease.attach()?; // provisional reservation before session/new
```

The Registry owns exact process identity and initialization. Reuse requires both
evidence sources, and attachment accounting protects concurrent startup.

For lifecycle tests, retain scheduling headroom:

```rust
#[cfg(test)]
const ACP_GRACEFUL_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(1);

tokio::time::timeout(Duration::from_secs(3), process.shutdown()).await?;
```

## Scenario: ACP Spawn Snapshot, Fingerprint And Profile Stale

### 1. Scope / Trigger

- Trigger: an ACP process is acquired, a Provider Profile/config/secret reference is saved, or the runtime needs to decide whether an existing process can still be reused.
- Ownership: `crates/agent-acp` owns the immutable launch snapshot, canonical fingerprint, process status and bounded status event; `crates/config-switch` owns the post-readback listener; desktop wiring shares the listener-enabled service with the ACP runtime.
- This contract is process-scoped. Model, Mode, Reasoning Effort and other session config remain outside the snapshot and belong to the session-config task.

### 2. Signatures

```rust
ProcessSpawnConfigSnapshot {
  agent_id: AgentId,
  adapter_id: AcpAdapterId,
  adapter_version: String,
  adapter_binary_identity: String,
  provider_profile_id: ProviderProfileId,
  profile_revision: i64,
  command: String,
  args: Vec<String>,
  cwd_policy: String,
  base_url: Option<String>,
  model_provider_id: Option<String>,
  non_secret_env: BTreeMap<String, String>,
  secret_reference_versions: BTreeMap<String, String>,
  mcp_revision: Option<String>,
  skills_revision: Option<String>,
  native_state_home_id: NativeStateHomeId,
}

ProcessSpawnConfigSnapshot::process_spawn_fingerprint()
  -> String // versioned sha256 token

AcpProcessRegistry::acquire_reusable_with_snapshot(key, snapshot, spawn, initialize)
AcpProcessRegistry::acquire_dedicated_with_snapshot(key, snapshot, spawn, initialize)
AcpProcessRegistry::refresh_config_status(process_instance_id, current_snapshot)
  -> VibexResult<Option<ProcessConfigStatusEvent>>

ProviderConfigService::with_profile_change_listener(listener)
ProviderProfileChangeListener::on_provider_profile_saved(profile_id, updated_at_ms)
// A service may compose multiple listeners; each runs after readback.
```

`ProcessConfigStatus` is `Current`, `StaleLiveMutationAvailable`,
`StaleRestartRequired`, `PreparingReplacement`, or `ReplacementFailed`.

### 3. Contracts

- Build the snapshot from an explicit process-field allowlist. Do not serialize or hash the whole API DTO, `ProviderProfile`, or `SessionRuntimeConfigState`.
- Canonical encoding is domain-separated, length-delimited, ordered by fixed field order, with sorted `BTreeMap` keys and preserved vector order. The digest is the only externally observable fingerprint value.
- `profile_revision` is a deterministic revision of the process-effective projection, not the wall-clock `updated_at_ms`; equivalent saves therefore recover the original fingerprint.
- Secret values never enter the snapshot, Debug output, event, log, or error. Secret references use opaque reference metadata/version (or a keyed digest supplied by the secret owner). Sensitive literal env entries are rejected; sensitive parent-environment entries keep only a stable env reference.
- The launch path materializes effective args/env once, uses that materialization for the child and the acquire key, and never resolves a secret in the stale-refresh-only path.
- The process acquire key remains `(route_key, provider_profile_id, process_spawn_fingerprint, workspace_scope)`. Session, binding, native session and PID are not process identity.
- A registered process retains its immutable launch snapshot and current observed fingerprint/status. Profile save refresh compares against the launch snapshot, emits one bounded event for a new observed fingerprint, and does not terminate or mutate the process. Active turns and host requests continue until the later switch coordinator applies its policy.
- Base URL, API key/reference, Provider/Model Provider, command/args, adapter/binary, environment, MCP and Skills drift is restart-required unless a negotiated descriptor explicitly marks every changed field live-mutable. No current adapter may assume in-process Base URL/API key mutation.
- Saving an equivalent process projection transitions stale back to `Current`; a no-op save is silent. The listener runs only after successful database write and readback and cannot roll back a persisted Profile.
- Status events contain process/profile ids, old/new fingerprint tokens, status and changed field names only. They do not contain URL query credentials, env values, prompt text, native session ids or secret references' resolved values.

### 4. Validation & Error Matrix

- Empty process fingerprint -> `validation/acp_process_spawn_fingerprint_empty`.
- Invalid/empty secret reference metadata -> `validation/acp_secret_reference_version_invalid`.
- Sensitive literal environment value -> `validation/acp_sensitive_literal_env`; use `SecretReference` instead.
- Refresh for a removed process -> `process/acp_process_instance_missing`; the config write remains committed and the runtime skips that lost instance.
- Unknown/unsupported live mutation capability -> classify as `StaleRestartRequired`, never silently mutate the old process.
- Registry lock failure -> `process/acp_process_registry_lock_poisoned`; no process shutdown is attempted by stale detection.
- Snapshot/key fingerprint mismatch -> `validation/acp_process_spawn_snapshot_mismatch`; the process is not spawned or registered.
- New attachment against a non-current process configuration -> `process/acp_process_config_stale`; existing attachments remain alive.

### 5. Good/Base/Bad Cases

- Good: changing Base URL emits one `StaleRestartRequired` event, leaves an active ACP turn alive, and lets the later switch coordinator prepare a replacement.
- Good: saving the same values twice emits no second event; changing back to the launch projection emits `Current`.
- Good: changing only Model/Mode/Effort/session state does not change the process fingerprint.
- Base: an unmanaged command without a readable binary uses a hashed command identity; a managed adapter descriptor contributes its exact compatibility identity.
- Bad: `sha256(serde_json::to_vec(&profile))` includes default Model, display metadata, secret references or future fields and makes unrelated saves restart processes.
- Bad: resolving an API key solely to hash it, printing the snapshot with the key, or killing the old process during an active turn.

### 6. Tests Required

- Snapshot unit tests assert fixed field ordering, map-order invariance, vector-order sensitivity, session-field exclusion, deterministic content revision, and no secret material in Debug.
- Fingerprint tests mutate each process-scoped field and assert a different digest; equivalent profile save/revert asserts the original digest.
- Registry tests assert snapshot registration, status transition, event idempotence, changed field names, active attachment count preservation and zero shutdown calls during stale refresh.
- Config service tests assert listener invocation after profile/config/secret readback and no invocation after validation/storage failure.
- ACP mock integration saves a Profile while a live process exists, receives `StaleRestartRequired`, then restores the original value and receives `Current` without process replacement.

### 7. Wrong vs Correct

#### Wrong

```rust
let fingerprint = sha256(serde_json::to_vec(&profile)?);
profile_save(profile)?;
process.shutdown().await; // interrupts an active turn
```

#### Correct

```rust
let snapshot = build_process_spawn_snapshot(profile, config, resources)?;
let key = ProcessAcquireKey::new(route, profile.id.clone(),
    snapshot.process_spawn_fingerprint(), workspace)?;
registry.refresh_config_status(process_id, rebuild_snapshot_after_save(profile)?)?;
// Mark stale and publish a bounded event; replacement/active-work policy is a
// later coordinator concern, so the current process remains alive.
```

## Anti-Patterns

- Do not treat Provider switching as editing `~/.claude` or Codex config files.
- Do not store secrets in logs, raw event payloads, or injection previews.
- Do not rely only on environment variables when ACP or the managed Adapter
  offers a typed session/config option.
- Do not let MCP/Skills management become provider-specific duplicate pages.

## Scenario: Provider Profile Core And Injection Preview

### 1. Scope / Trigger

- Trigger: Phase 3 introduces the first Provider Profile implementation across
  Rust DTOs, SQLite schema, `vibex-config-switch`, Tauri commands, generated
  TypeScript protocol, and desktop Provider settings UI.
- This is a cross-layer contract because Provider Profile records flow through
  storage, service orchestration, Tauri command handlers, browser mocks, and
  React Query hooks.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_list_profiles() -> Vec<ProviderProfile>
provider_create_profile(ProviderProfileCreateRequest) -> ProviderProfile
provider_update_profile(ProviderProfileUpdateRequest) -> ProviderProfile
provider_delete_profile(ProviderProfileDeleteRequest) -> ()
provider_duplicate_profile(ProviderProfileDuplicateRequest) -> ProviderProfile
provider_get_default(ProviderProfileDefaultScope, ProviderKind)
  -> ProviderProfileDefaultSelection
provider_set_default(ProviderProfileSetDefaultRequest)
  -> ProviderProfileDefaultSelection
provider_preview_injection(ProviderInjectionPreviewRequest)
  -> ProviderInjectionPreview
```

SQLite schema version 5 owns:

```text
provider_profiles(
  provider_profile_id, provider_kind, display_name, status, account_alias,
  base_url, default_model, small_model, large_model, reasoning_effort,
  sandbox_defaults_json, network_defaults_json, permission_defaults_json,
  provider_options_json, created_at_ms, updated_at_ms, deleted_at_ms
)

provider_secret_references(
  secret_ref_id, provider_profile_id, secret_kind, backend, setup_state,
  lookup_key, display_label, redacted_hint, created_at_ms, updated_at_ms
)

provider_default_profiles(
  scope_kind, scope_id, provider_kind, provider_profile_id,
  created_at_ms, updated_at_ms
)

provider_injection_previews(
  preview_id, provider_profile_id, request_json, preview_json, created_at_ms
)
```

### 3. Contracts

- `crates/core` is the source of truth for Provider Profile, secret reference,
  default selection, and injection preview DTOs.
- `crates/db` owns migration v5 plus repositories. It stores secret reference
  metadata only; it must not store plaintext secret values.
- `crates/config-switch` owns Provider Profile orchestration and redacted
  injection preview construction.
- `provider_secret_references.backend = placeholder` means Vibex has a durable
  secret reference record but cannot resolve plaintext credentials yet.
- Default local compatibility profiles must exist or be seeded lazily for:
  `provider_local_default_codex`, `provider_local_default_claude`,
  `provider_local_default_acp`, and `provider_local_default_mock`.
- Injection preview fields must be redacted at construction time. Environment
  fields may show keys such as `OPENAI_API_KEY` or `ANTHROPIC_API_KEY`, but
  values must be redacted hints such as `not configured`.

### 4. Validation & Error Matrix

- Empty Provider Profile display name -> `validation/provider_profile_name_empty`.
- Missing Provider Profile lookup -> `validation/provider_profile_not_found`.
- Attempt to delete local default profile -> `validation/provider_profile_default_delete_rejected`.
- Project default scope without `projectId` ->
  `validation/provider_default_project_missing`.
- Workspace default scope without `workspaceId` ->
  `validation/provider_default_workspace_missing`.
- SQLite insert/update/readback failure -> `storage/provider_profile_*`.
- Placeholder secret reference used in preview -> redacted preview succeeds, but
  session startup must not treat it as a usable plaintext credential.

### 5. Good/Base/Bad Cases

- Good: A user creates a Codex Provider Profile, sees `OPENAI_API_KEY` as a
  redacted placeholder in injection preview, and no native Codex config file is
  written.
- Base: A fresh database lazily exposes local default Provider Profiles so
  existing Agent sessions using static local default ids keep working.
- Bad: A UI form stores `sk-...` or another plaintext API key in SQLite,
  generated fixtures, browser mocks, logs, or `provider_injection_previews`.

### 6. Tests Required

- `cargo test -p vibex-db provider` must assert profile, secret reference,
  default selection, and preview persistence round-trip.
- `cargo test -p vibex-config-switch` must assert local default seeding and
  redacted preview construction without plaintext secrets.
- Core protocol tests must pass after adding or changing Provider DTOs.
- `pnpm check:frontend` must pass after adding Provider settings hooks and UI.
- `pnpm smoke:db` should report schema version 5 or later after migration.

### 7. Wrong vs Correct

#### Wrong

```rust
ProviderInjectionField {
    key: "OPENAI_API_KEY".to_string(),
    value: plaintext_api_key,
    secret: true,
    source: "form".to_string(),
}
```

This leaks a credential into the preview response, generated diagnostics, and
possibly SQLite if the preview is persisted.

#### Correct

```rust
ProviderInjectionField {
    key: "OPENAI_API_KEY".to_string(),
    value: "not configured".to_string(),
    secret: true,
    source: "placeholder".to_string(),
}
```

The preview is useful for diagnostics while preserving the invariant that
SQLite and UI state contain only secret references and redacted hints.

## Scenario: Claude/Codex Native Config Read-Only Import

### 1. Scope / Trigger

- Trigger: Phase 3 adds read-only Claude/Codex native config import across Rust
  DTOs, `vibex-config-switch`, Tauri commands, generated TypeScript protocol,
  and desktop Provider settings UI.
- This is a cross-layer contract because native config data flows from local
  files into redacted service previews and then into Vibex-owned Provider
  Profile records.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_native_import_preview(ProviderNativeImportPreviewRequest)
  -> ProviderNativeImportPreview
provider_create_profile_from_import(ProviderNativeImportCreateRequest)
  -> ProviderNativeImportCreateResult
```

Core request/response types include:

```text
ProviderNativeImportPreviewRequest { sources }
ProviderNativeImportPreview { preview_id, sources, files, items, diagnostics, created_at_ms }
ProviderNativeImportItem {
  import_item_id, source, provider_kind, display_name, account_alias,
  base_url, default_model, provider_options, secret_references, status,
  redacted_fields, diagnostics
}
ProviderNativeImportCreateRequest { preview_request, import_item_id }
ProviderNativeImportCreateResult { profile, source, diagnostics }
```

### 3. Contracts

- Codex discovery reads `CODEX_HOME` when set and non-empty, otherwise
  `~/.codex`.
- Claude discovery reads `CLAUDE_CONFIG_DIR` when set and non-empty, otherwise
  `~/.claude`.
- Discovery may inspect `auth.json`, `config.toml`, model cache/catalog JSON,
  `settings.json`, fallback `claude.json`, and `~/.claude.json` for diagnostics.
- Missing native files are normal preview diagnostics and must not fail the
  command.
- Malformed JSON/TOML is a per-file diagnostic when other files can still be
  previewed.
- `provider_create_profile_from_import` must create normal Vibex Provider
  Profile records through the existing Provider Profile storage path.
- The import flow must not create, modify, delete, rename, back up, or rewrite
  native Claude/Codex files.
- Secret values from native config become placeholder/reference metadata only;
  plaintext tokens must not enter SQLite, preview DTOs, logs, generated mocks,
  or UI state.

### 4. Validation & Error Matrix

- Requested import item not found after re-preview ->
  `validation/provider_native_import_item_not_found`.
- Import item blocked by parse errors ->
  `validation/provider_native_import_item_not_importable`.
- File unreadable -> preview diagnostic
  `provider_native_import_file_unreadable`.
- Malformed native JSON/TOML -> preview diagnostic
  `provider_native_import_parse_failed`.
- Missing source config files -> preview diagnostic
  `provider_native_import_source_missing`.
- Profile creation failure -> existing Provider Profile validation/storage
  errors, preserving the original code where possible.

### 5. Good/Base/Bad Cases

- Good: A Codex `config.toml` with active `model_provider` and `auth.json`
  token presence produces an import item with endpoint/model metadata, redacted
  secret fields, placeholder secret references, and no native file writes.
- Base: A machine without Claude or Codex config returns missing-file
  diagnostics and no import items without throwing an error.
- Bad: Import code copies `OPENAI_API_KEY`, bearer tokens, OAuth refresh tokens,
  or private keys into Provider Profile fields, browser mocks, logs, or
  generated protocol fixtures.

### 6. Tests Required

- `cargo test -p vibex-config-switch native_import` must cover Codex config/auth
  parsing, Claude settings parsing, malformed files, missing files, unknown
  fields, redaction, and no-write behavior against seeded native fixture files.
- Core protocol tests must pass after changing native import DTOs.
- `pnpm check:frontend` must pass after adding Provider settings import UI and
  typed Tauri wrappers.
- `pnpm check` must pass before marking the child complete.

### 7. Wrong vs Correct

#### Wrong

```rust
ProviderProfileCreateRequest {
    provider_options: Some(ProviderOptions {
        schema_version: 1,
        entries: vec![option_entry("OPENAI_API_KEY", plaintext_key)],
    }),
    secret_references: vec![],
    // ...
}
```

This turns a native secret into durable Vibex state and may leak through
serialized responses, previews, logs, and UI caches.

#### Correct

```rust
ProviderProfileCreateRequest {
    provider_options: Some(ProviderOptions {
        schema_version: 1,
        entries: vec![option_entry("nativeSource", "codex")],
    }),
    secret_references: vec![placeholder_secret(
        ProviderSecretKind::ApiKey,
        "OPENAI_API_KEY",
        "OpenAI API key from Codex native config",
    )],
    // ...
}
```

The imported profile preserves non-secret native metadata while keeping
credential setup explicit and redacted.

## Scenario: Provider Health, Usage, And Failover Recommendation

### 1. Scope / Trigger

- Trigger: Phase 3 adds Provider health records, Provider usage records, and
  recommendation-only failover signals across Rust DTOs, SQLite, service
  orchestration, Tauri commands, generated TypeScript protocol, and desktop UI.
- This is a cross-layer contract because the same Provider Profile id flows
  through storage, service aggregation, command payloads, browser mocks, React
  Query hooks, and Provider settings cards.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_list_health_summaries() -> Vec<ProviderHealthSummary>
provider_run_health_probes(ProviderRunHealthProbesRequest)
  -> ProviderRunHealthProbesResult
provider_list_usage_summaries(ProviderUsageListRequest)
  -> Vec<ProviderUsageSummary>
provider_list_failover_recommendations(ProviderFailoverRecommendationRequest)
  -> Vec<ProviderFailoverRecommendation>
```

SQLite schema version 6 owns:

```text
provider_health_probe_records(
  health_record_id, provider_profile_id, provider_kind, probe_kind, status,
  summary, latency_ms, checked_at_ms, expires_at_ms, diagnostics_json
)

provider_usage_records(
  usage_record_id, provider_profile_id, provider_kind, source, unit, label,
  used, limit_value, remaining, window_label, window_started_at_ms,
  window_ends_at_ms, recorded_at_ms, metadata_json
)
```

Core probe kinds:

```text
binary_exists, version, auth_status, model_list, streaming_first_byte,
simple_prompt
```

### 3. Contracts

- `crates/core` is the source of truth for health, usage, and failover DTOs;
  frontend code imports generated protocol types and must not redefine command
  payloads locally.
- `provider_health_probe_records` stores individual probe records. Summaries
  are service-derived from latest records per `(provider_profile_id,
  probe_kind)`.
- `provider_usage_records` stores Provider usage separately from
  `agent_timeline_items`. Timeline rendering is not the source of truth for
  Provider quota or budget cards.
- Default health probes are provider-auth-free and deterministic. `pnpm check`
  must not spawn Claude/Codex, call provider APIs, or require native auth.
- Probe-level failures should normally return `ProviderHealthStatus::Fail`
  records instead of failing the whole command, so one failed probe does not
  collapse the health panel.
- Failover recommendations are advisory only. They must not mutate
  `provider_default_profiles`, running sessions, timeline events, native config
  files, or proxy routing.
- Recommendations only compare same-`ProviderKind` enabled candidate profiles.

### 4. Validation & Error Matrix

- Missing selected Provider Profile -> omit from list-style summaries or return
  existing `validation/provider_profile_not_found` when a command requires a
  single concrete profile.
- Unsupported probe capability -> per-probe `unsupported` or `skipped` record,
  not a command-level provider failure.
- Missing or placeholder secret reference -> `auth_status` failed probe record
  with redacted diagnostics.
- Usage query storage failure -> `storage/provider_usage_record_*`.
- Health query storage failure -> `storage/provider_health_record_*`.
- Recommendation source has risk signals but no same-kind enabled candidate ->
  `ProviderFailoverRecommendationStatus::Blocked` with `no_candidate`, not an
  automatic switch attempt.

### 5. Good/Base/Bad Cases

- Good: A Provider Profile with missing auth produces a redacted
  `auth_status/fail` health record, a recommendation card when a same-kind
  healthy candidate exists, and no native config or session mutation.
- Base: A fresh database returns Provider health summaries with `unknown` for
  profiles that have no probe records yet, and usage summaries with empty
  balances when `include_empty` is true.
- Bad: A default check starts a real Claude/Codex session, calls a provider
  model-list endpoint, stores raw prompt/response bodies, or silently changes
  the selected Provider Profile.

### 6. Tests Required

- `cargo test -p vibex-db provider` must assert Provider health and usage
  record round-trips and keep them separate from timeline tables.
- `cargo test -p vibex-config-switch health` must assert deterministic,
  provider-free probe behavior and redacted diagnostics.
- `cargo test -p vibex-config-switch usage` must assert usage summaries come
  from Provider usage records.
- `cargo test -p vibex-config-switch failover` must assert recommendations are
  advisory and do not mutate defaults or sessions.
- `pnpm check:frontend` and `pnpm check` must pass after
  adding or changing health/usage/failover DTOs.
- Provider settings card screenshots are optional visual evidence; capture them
  only when requested, when visual regression risk is high, or when local
  rendering is already part of validation.

### 7. Wrong vs Correct

#### Wrong

```rust
service.set_default(ProviderProfileSetDefaultRequest {
    provider_profile_id: candidate.id.clone(),
    // ...
})?;
```

Doing this inside recommendation generation silently changes user Provider
selection and violates the recommendation-only contract.

#### Correct

```rust
ProviderFailoverRecommendation {
    source_profile: current.summary(),
    candidate_profile: Some(candidate.summary()),
    status: ProviderFailoverRecommendationStatus::Recommended,
    message: "Consider switching after user review".to_string(),
    // ...
}
```

The service returns an actionable card while leaving Provider defaults,
sessions, timeline, native config, and proxy routing unchanged.

## Scenario: Native Config Export With Diff Backup Rollback

### 1. Scope / Trigger

- Trigger: Phase 3 adds explicit Claude/Codex native Provider config export
  across `crates/core`, SQLite schema v9, `vibex-config-switch`, Tauri
  commands, shared Rust DTOs, browser mocks, React Query hooks,
  and Provider settings UI.
- This is a cross-layer contract because export records flow from UI preview
  through Tauri commands, service safety checks, file side effects, and SQLite
  rollback metadata.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_native_export_preview(ProviderNativeExportPreviewRequest)
  -> ProviderNativeExportPreview
provider_native_export_apply(ProviderNativeExportApplyRequest)
  -> ProviderNativeExportApplyResult
provider_native_export_rollback(ProviderNativeExportRollbackRequest)
  -> ProviderNativeExportRollbackResult
provider_native_export_list(ProviderNativeExportListRequest)
  -> Vec<ProviderNativeExportRecordSummary>
```

SQLite schema version 9 owns:

```text
provider_native_export_records(
  export_id, provider_profile_id, source, mode, status, preview_json,
  applied_at_ms, rolled_back_at_ms, created_at_ms, updated_at_ms
)

provider_native_export_file_operations(
  operation_id, export_id, source, file_kind, operation_kind, target_path,
  backup_path, temp_path, marker, status, redacted_diff, diagnostics_json,
  target_size_before, target_size_after, backup_size, created_at_ms,
  updated_at_ms
)
```

### 3. Contracts

- Native export is always explicit opt-in. Editing Provider Profiles, MCP,
  Skills, Prompts, or Hooks must not implicitly write native files.
- Preview is no-write: it must not create parent directories, temp files,
  backups, or native files. Preview payloads must be redacted.
- Apply must read a persisted preview, create backups for existing files, write
  temp files in the target directory, then atomically rename into place.
- If a write fails after backup creation, apply must attempt restore and return
  `failed_restored` when restoration succeeds.
- Rollback restores only Vibex-created backup paths recorded in export file
  operations. It must not guess or delete unrelated user-managed files.
- Codex `config.toml` export is conservative: ready only when the file is
  missing/empty or already contains a Vibex managed marker. Existing unmarked
  TOML must return blocked diagnostics.
- Claude `settings.json` export owns only the top-level `vibex` field and
  preserves other JSON object fields.
- MCP, Skills, Prompts, and combined native export modes remain blocked until
  marker ownership and provider-native shape are explicitly implemented.

### 4. Validation & Error Matrix

- Missing profile -> `provider_native_export_profile_not_found`.
- Missing persisted preview during apply -> `provider_native_export_preview_not_found`.
- Existing unmarked Codex config -> blocked file plan with
  `provider_native_export_blocked`.
- Unsafe target shape or changed post-preview target ->
  `provider_native_export_unsafe_target`.
- Backup/temp/atomic/restore failures -> stable `provider_native_export_*`
  storage/validation errors with redacted diagnostics.

### 5. Good/Base/Bad Cases

- Good: A Codex profile previews a marked `config.toml` update, shows redacted
  diff, backup path, temp path, rollback plan, and applies only after explicit
  user click.
- Base: A machine without native files can preview a supported create plan
  without creating directories or files.
- Bad: Preview writes a native file, stores plaintext secrets, overwrites
  unmarked Codex TOML, installs hooks, or exports MCP/Skills native blocks
  without a clear marker-owned shape.

### 6. Tests Required

- `cargo test -p vibex-db native_export` must assert export record/file
  operation persistence and list summaries.
- `cargo test -p vibex-config-switch native_export` must assert preview
  no-write behavior, redaction, blocked unsafe Codex target, apply backup +
  atomic write, failed write restore, and rollback restore.
- `pnpm check:frontend` and `pnpm check` must pass after
  changing native export DTOs, commands, mocks, hooks, or UI.
- Native export preview screenshots are optional visual evidence; capture them
  only when requested, when visual regression risk is high, or when local
  browser rendering is already part of validation.

### 7. Wrong vs Correct

#### Wrong

```rust
fs::write("~/.codex/config.toml", generated_config)?;
```

This bypasses diff preview, backup, atomic write, rollback metadata, and marker
ownership checks.

#### Correct

```rust
let preview = service.preview_native_export(request)?;
// User reviews redacted diff, backup path, temp path, and rollback plan.
let result = service.apply_native_export(ProviderNativeExportApplyRequest {
    export_id: preview.export_id,
})?;
```

The service applies only a persisted preview and records recoverable state
before native side effects.

## Scenario: Phase 6 ACP Provider Configuration Catalog

### 1. Scope / Trigger

- Trigger: Phase 6 adds command-based ACP Provider configuration and bundled
  presets for concrete ACP Agents, including the managed Claude/Codex
  Adapters.
- This is a cross-layer Provider contract because typed Rust DTOs flow through
  `ProviderConfigService`, Tauri commands, shared Rust DTOs,
  browser mocks, and desktop Provider settings.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_list_acp_catalog_presets() -> AcpProviderCatalogListResponse
provider_create_acp_profile(AcpProviderProfileCreateRequest) -> ProviderProfile
provider_get_acp_profile_config(providerProfileId) -> AcpProviderConfig
provider_update_acp_profile_config(AcpProviderProfileUpdateRequest) -> ProviderProfile
```

Stable `ProviderOptions` storage:

```text
ProviderProfile.kind = acp
ProviderProfile.providerOptions.entries[
  key = "acp.config.v1",
  value = JSON-encoded AcpProviderConfig
]
```

### 3. Contracts

- `AcpProviderConfig` owns command, args, env references, cwd template, models,
  modes, features, and disabled tools. UI code must not parse private
  `ProviderOptions` entries directly.
- The bundled catalog includes OpenCode plus concrete Agent presets. Managed
  Claude uses `claude-agent-acp` backed by
  `@agentclientprotocol/claude-agent-acp@0.64.2`; managed Codex uses
  `codex-acp` backed by `@agentclientprotocol/codex-acp@1.1.9`. The
  Compatibility Registry, not UI strings or PATH guessing, owns exact managed
  versions.
- An enabled ACP Agent may seed one typed `kind = acp` runtime profile from its
  bundled preset. This runtime profile is separate from Claude/Codex
  configuration-only profiles whose kinds remain `claude` or `codex`.
- ACP validation is deterministic and side-effect-free. It must not start
  `opencode`, inspect shell startup files, check auth, or touch the network.
- `ProviderConfigService` is the only owner of ACP config encode/decode and
  validation. Generic ACP profile create/update paths must not bypass typed ACP
  config validation.
- Injection preview expands ACP config into redacted command, arg, env, cwd,
  model, mode, feature, and disabled-tool fields. It must not show the raw
  `acp.config.v1` JSON blob.
- Plaintext credential-like env literals are rejected. Credential env values
  must use process environment or secret-reference metadata.

### 4. Validation & Error Matrix

- Missing ACP config -> `acp_config_missing`.
- Unknown catalog preset -> `acp_preset_not_found`.
- Missing command -> `acp_command_empty`.
- Blank or duplicated provider option key -> `provider_option_key_empty` or
  `provider_option_key_duplicate`.
- Invalid env key -> `acp_env_key_invalid`.
- Duplicated env key -> `acp_env_key_duplicate`.
- Credential-like literal env value -> `acp_env_literal_secret_rejected`.
- Invalid secret env reference -> `acp_env_secret_reference_invalid`.
- Empty or traversal cwd template -> `acp_cwd_template_empty` or
  `acp_cwd_template_traversal`.

### 5. Good/Base/Bad Cases

- Good: A user creates an ACP preset, edits command args or cwd, sees
  command/args/env/cwd in injection preview, and no process is started.
- Good: enabling built-in Claude/Codex seeds an ACP runtime profile that uses
  the fixed managed Adapter command while existing configuration-only profiles
  remain available to API probe/import flows.
- Base: A custom ACP profile with safe literal env metadata validates and stores
  only Vibex-owned ProviderOptions.
- Bad: UI parses `acp.config.v1` JSON directly, stores a plaintext token in
  ProviderOptions/browser mocks, or validation starts the provider binary.

### 6. Tests Required

- `cargo test -p vibex-config-switch acp` must assert preset listing, profile
  creation/update, validation failures, and preview redaction.
- `cargo test -p vibex-core provider` must pass after
  changing ACP DTOs.
- `pnpm --filter @vibex/desktop typecheck`, `pnpm check:frontend`, and root
  `pnpm check` must pass after changing Provider settings UI or browser mocks.

### 7. Wrong vs Correct

#### Wrong

```text
Claude Agent -> ProviderProfile.kind = claude -> dispatch a Native SDK runtime
```

#### Correct

```text
Claude configuration/API profile -> kind = claude (not catalog eligible)
Claude online runtime profile -> kind = acp + claude-agent-acp preset
SessionRuntimeSelection -> exact Agent/Profile -> managed ACP route
```

## Scenario: Phase 6 ACP Capability Probe And Runtime Gating

### 1. Scope / Trigger

- Trigger: Phase 6 adds profile-scoped ACP capability probing and cached
  effective capability summaries before real OpenCode smoke evidence.
- This is a cross-layer Provider contract because `ProviderCapabilities` flows
  from typed ACP config through SQLite, `ProviderConfigService`, Tauri commands,
  shared Rust DTOs, browser mocks, and Provider settings UI.

### 2. Signatures

Tauri commands exposed by `apps/desktop/src-tauri`:

```text
provider_list_capability_summaries() -> Vec<ProviderCapabilitySummary>
provider_run_capability_probes(ProviderRunCapabilityProbesRequest)
  -> ProviderRunCapabilityProbesResult
```

SQLite schema version 11 owns:

```text
provider_capability_probe_records(
  capability_record_id, provider_profile_id, provider_kind, status, summary,
  capabilities_json, source, checked_at_ms, expires_at_ms, diagnostics_json
)
```

### 3. Contracts

- `crates/core` is the source of truth for `ProviderCapabilityProbeStatus`,
  `ProviderCapabilityProbeResult`, `ProviderCapabilitySummary`, and capability
  probe run request/result DTOs.
- ACP probing is deterministic by default. It projects capabilities from typed
  `AcpProviderConfig` only and must not start OpenCode, start an ACP session,
  inspect auth, or touch the network.
- Explicit OpenCode ACP smoke may start `opencode acp` and record a redacted
  runtime `session/new` model/config snapshot, but that evidence is separate
  from default Provider settings capability probes.
- A fresh `pass` capability probe is authoritative for ACP Provider settings
  summaries. Missing, stale, failed, or invalid ACP probe state falls back to
  conservative `ProviderCapabilities::conservative(ProviderKind::Acp,
  "acp-foundation-static")`.
- Capability diagnostics are redacted `ProviderBindingMetadata` only. Do not
  store raw provider payloads, command output, env values, tokens, auth data, or
  plaintext provider config blobs.
- Non-ACP profiles may return unsupported capability probe summaries without
  changing existing Claude, Codex, Mock, health, usage, failover, import/export,
  MCP, Skills, Prompts, or Hooks behavior.

### 4. Validation & Error Matrix

- Missing ACP config -> failed probe result with conservative capabilities.
- Invalid ACP config -> failed probe result with redacted error code diagnostic.
- Expired capability record -> stale summary and conservative effective
  capabilities.
- Non-ACP capability probe -> unsupported summary/record, not a command-level
  provider failure.
- OpenCode runtime model/config snapshot unavailable during explicit smoke ->
  smoke evidence records an unavailable redacted snapshot, not a default
  capability-probe failure.
- Storage failures -> `storage/provider_capability_record_*`.

### 5. Tests Required

- `cargo test -p vibex-db provider_health_and_usage_records_round_trip` or a
  more specific capability repository test must cover capability record
  persistence and `ProviderCapabilities` JSON round-trip.
- `cargo test -p vibex-config-switch capability` must cover deterministic ACP
  capability projection and redacted diagnostics.
- `pnpm --filter @vibex/desktop typecheck` and
  `pnpm check:frontend` must pass after changing capability DTOs or Provider
  settings UI.
- Real OpenCode session smoke must remain a separate explicit task and must not
  run as part of default capability validation.

## Scenario: Desktop Runtime Background Agent Bootstrap

### 1. Scope / Trigger

- Trigger: desktop startup reconciles already-added managed ACP Agents after
  the authoritative runtime is ready. User-driven Add/Upgrade/Uninstall work
  remains owned by Config Center.
- Runtime-option probing runs in the post-activation background bootstrap for
  already-enabled, installed Agents that have no successful persisted snapshot.
  It remains independent from Provider Profile reconciliation.

### 2. Signatures

```rust
DesktopRuntime::start(config) -> Arc<DesktopRuntime>
DesktopRuntime::spawn_agent_bootstrap() -> VibexResult<()>
AgentInstallService::ensure_installed(agent_id).await
RuntimeOptionCatalogService::probe_agent(agent_id).await
ProviderProfileMutationEvent::{Saved, Deleted}(provider_profile_id)
```

### 3. Contracts

- Runtime readiness requires the local authoritative services, lifecycle,
  gateway, and startup reconciliation, but not network-backed adapter installs
  or Agent process capability probes.
- Start reconciliation only after `DesktopRuntime::activate` succeeds. Own
  the task in `DesktopRuntime.tasks` so shutdown aborts any download/npm child
  through its existing kill-on-drop lifecycle. Bootstrap only includes Agent
  snapshots that are both `added` and Registry-managed; a removed Agent is
  never silently reinstalled.
- Use the listener-enabled runtime `ProviderConfigService`. Managed command
  reconciliation must still invalidate ACP process configuration and publish
  Provider Profile changes.
- After managed-install reconciliation, startup scans added, enabled, installed
  Agents for a missing successful runtime-option snapshot. Each missing or
  previously failed snapshot is probed in the background; existing successful
  snapshots remain ordinary SQLite cache reads. A successful probe publishes
  `RuntimeOptionsChanged` so open clients refresh their runtime catalog.
- Provider Profile save/delete consumers publish `ProfilesChanged` only. They
  must not clear Agent snapshots, call `probe_agent`, enqueue a Profile-scoped
  refresh, or publish `RuntimeOptionsChanged` as a consequence of the Provider
  mutation.
- After a Provider mutation, clients rebuild the Profile/model projection and
  read the unchanged Agent snapshot. A newly configured model therefore gains
  the Agent's cached controls without another probe.

### 4. Validation & Error Matrix

- Runtime activation fails -> `DesktopRuntime::start` fails; do not spawn the
  background bootstrap.
- Runtime task ownership lock fails -> return
  `process/desktop_runtime_task_lock_failed` from startup.
- Managed install/reconciliation fails after readiness -> emit a bounded
  warning and keep the runtime available; do not fall through to an option
  probe.
- Provider save/delete succeeds -> publish one coalesced `ProfilesChanged`
  event and remote Provider invalidation only.
- Agent has no successful runtime-option snapshot at startup -> probe it in the
  background and persist either the option snapshot or the failed-attempt
  record. A probe failure must not make the runtime unavailable.
- Agent has a successful snapshot while a Provider changes -> preserve the
  snapshot and reuse it in the rebuilt catalog.

### 5. Good/Base/Bad Cases

- Good: an existing managed installation is repaired in the background while
  the workbench opens; completed command reconciliation publishes Provider
  changes without starting an Agent option probe.
- Base: exact adapters are installed and an Agent has no snapshot; the
  workbench remains ready while the background probe persists options and
  refreshes the runtime catalog.
- Good: adding a Provider Profile updates models and immediately reuses the
  Agent's previously cached modes and Features.
- Bad: `build_agent_manager` or `activate` awaits a managed download, an ACP
  probe, or complete catalog enrichment before reporting the runtime ready.
- Bad: a Profile save calls `refresh_profile`/`probe_agent`.

### 6. Tests Required

- `vibex-desktop-runtime` asserts activation precedes background bootstrap and
  manager construction contains no adapter installation.
- Runtime source assertions require missing-snapshot probing and
  `RuntimeOptionsChanged` in the background bootstrap, while rejecting profile
  refreshes and runtime-option events in Provider mutation consumers.
- Provider mutation tests assert `ProfilesChanged` is published and no later
  `RuntimeOptionsChanged` arrives.
- Runtime catalog tests assert startup/list paths perform no provider or Agent
  process call and Provider changes preserve the Agent snapshot.
- Desktop tests assert the startup brand overlay is released before overview and
  authoritative timeline restoration, while its spinner delay remains five
  seconds.

### 7. Wrong vs Correct

#### Wrong

```rust
prepare_managed_acp_adapters(&config_service, &db_path).await?;
runtime_catalog.refresh_missing().await?;
let runtime = DesktopRuntime::ready();
```

#### Correct

```rust
runtime.activate().await?;
runtime.spawn_agent_bootstrap()?;
Ok(runtime)
```

The runtime becomes usable first; the owned background task converges managed
Agent commands only. Agent setup owns the separate one-time option probe.

## Scenario: Config Center Agent Registry And Selector Gating

### 1. Scope / Trigger

- Trigger: Config Center lists, enables, discovers, and selects concrete online
  Agents after the ACP-only cutover.
- This is a cross-layer contract because `AgentId`, config rows, discovery
  snapshots, Provider Profiles, Runtime Option Catalog, Tauri commands,
  shared Rust DTOs, browser mocks, and
  `AgentManager::create_session` must enforce the same online boundary.

### 2. Signatures

Core DTOs:

```text
AgentId -> lowercase kebab id such as mock, claude, codex, opencode
AgentRuntimeKind = acp
AgentDefinition
AgentConfig
AgentDiscoveryRecord
AgentSnapshotEntry
AgentListRequest { includeDisabled }
AgentUpdateConfigRequest { agentId, enabled?, labelOverride?, ... }
AgentRefreshSnapshotRequest { agentId, cwdScope? }
SessionRuntimeSelection {
  agentId, providerProfileId, modelId, reasoningEffort?, modeId?
}
CreateAgentSessionRequest { runtime: SessionRuntimeSelection, ... }
build_runtime_option_catalog(agents, profiles, evidenceByProfile)
  -> SessionRuntimeOptionCatalog
```

Tauri commands:

```text
agent_list(AgentListRequest) -> AgentListResponse
agent_update_config(AgentUpdateConfigRequest) -> AgentSnapshotEntry
agent_refresh_snapshot(AgentRefreshSnapshotRequest) -> AgentRefreshSnapshotResponse
agent_list_catalog() -> AgentCatalogListResponse
agent_list_runtime_options() -> SessionRuntimeOptionCatalog
```

SQLite schema version 16 owns:

```text
agent_configs(
  agent_id, runtime_kind, source_kind, label_override, description_override,
  enabled, order_index, command_json, env_json, params_json,
  created_at_ms, updated_at_ms, deleted_at_ms
)

agent_discovery_records(
  discovery_record_id, agent_id, cwd_scope, install_status, config_status,
  runtime_status, binary_path, version, native_config_paths_json,
  models_json, modes_json, diagnostics_json, discovered_at_ms
)
```

### 3. Contracts

- `vibex-core` owns the Agent contracts and built-in definitions for `claude`,
  `codex`, and concrete ACP-backed Agents such as `opencode`. Every definition
  uses `runtime_kind = acp`; other crates must reuse these definitions.
- ACP is a connection/adapter protocol, not a user-facing Agent. ACP-backed
  Agents keep a concrete visible `agent_id` and label. Online identity is the
  exact `AgentRuntimeRouteKey { agent_id, Acp, adapter_id }`, not a generic
  `acp` Agent row or `ProviderKind` value.
- Claude/Codex model-provider or native-import profiles may retain
  `ProviderProfile.kind = claude | codex` for configuration, API probes, and
  provenance. Only enabled profiles with `kind = acp` are eligible for the
  Runtime Option Catalog or online session creation.
- `ProviderConfigService::list_agents` merges built-ins, persisted
  `agent_configs`, and latest cached `agent_discovery_records`. It must not
  spawn provider runtimes, ACP processes, or Agent probes.
- The GPUI Config Center Agent sidebar renders every supported Agent in one
  card list. Added/enabled Agents come first, added/disabled Agents follow,
  and unadded Agents come last; each group sorts case-insensitively by display
  name. Unadded Agents expose their Add action on the card instead of a
  separate catalog dropdown.
- The GPUI Config Center Agent surface does not load, render, or mutate Provider
  failover recommendations or queues. Backend failover storage and service
  contracts remain independent of this presentation rule.
- `agent_refresh_snapshot` may perform low-cost filesystem/PATH detection and
  cache a discovery record for an added Agent even while it is disabled. The
  detection may update install/config status, but runtime status remains
  `disabled` and no Agent or ACP process is spawned. For the explicit
  OpenCode refresh path only, Vibex may run the resolved CLI with `--version`
  to record its detected semantic version; this is a bounded metadata probe,
  not an ACP/runtime/session start. Removed Agents are not probed.
- Cached detected versions survive ordinary PATH refresh only while the same
  executable remains installed. A failed explicit version probe, changed
  executable, or missing binary clears the active version identity so Provider
  projection falls back to the conservative surface.
- Discovery keeps Agent installation separate from online runtime readiness.
  For Claude and Codex, `install_status` and `binary_path` resolve the native
  `claude` / `codex` CLI (or fall back to a self-contained Adapter), while
  `runtime_status` resolves the configured `claude-agent-acp` / `codex-acp`
  launch command. A native CLI with no Adapter is `installed + unavailable`,
  never `missing` or `ready`.
- The conversation selector consumes `SessionRuntimeOptionCatalog`. Catalog
  construction publishes only added/enabled Agents, enabled ACP profiles, and
  configured Models. It does not publish configuration-only Claude/Codex
  profiles and has no hardcoded ProviderKind fallback.
- Configured Codex models may obtain per-model reasoning evidence through the
  pinned managed Codex app-server `model/list` operation. This is a bounded,
  short-lived stateless probe: it must not call ACP `session/new`, create a
  native thread, expose credentials, or keep a child process alive. Probe
  failure keeps the configured models selectable with empty Effort metadata.
- Managed Codex modes come from the exact adapter compatibility contract. For
  `codex-acp@1.1.9`, typed ACP configuration publishes `read-only`, `agent`, and
  `agent-full-access`; existing managed profiles with no modes are reconciled
  to this set without changing profile identity or model defaults.
- Catalog selections use `reasoningEffort = null` and `modeId = null` to mean
  "use the Adapter's converged default." Effective matching still requires an
  exact Model and validates Effort/Mode only when the selection explicitly sets
  them.
- `AgentManager::create_session` accepts one complete
  `SessionRuntimeSelection`, rejects disabled/deleted Agents or a non-ACP,
  disabled, wrong-Agent Profile, and requires a concrete Model before inserting
  a Logical Session.
- Online manager registration accepts only an ACP `AgentRuntimeRouteKey` and an
  ACP `AgentProvider`, with one active route per Agent. There is no Native route,
  placeholder Adapter derivation, or ProviderKind-based fallback.

### 4. Validation & Error Matrix

- Invalid `AgentId` shape -> `validation/invalid_agent_id`.
- Unknown agent id -> `validation/agent_not_found`.
- Empty agent label override -> `validation/agent_label_empty`.
- Agent disabled or deleted on session create ->
  `validation/agent_disabled`.
- Missing/blank concrete Model -> `validation/runtime_selection_model_required`.
- Profile missing, disabled, non-ACP, or owned by another Agent ->
  `validation/provider_profile_not_found` or
  `validation/provider_profile_route_mismatch`.
- Non-ACP profile during catalog construction -> omit it; never reinterpret it
  as an online profile.
- Agent list storage failures -> `storage/agent_config_*` or
  `storage/agent_discovery_*`.
- Missing exact ACP route after enabled-Agent resolution ->
  `capability/provider_unregistered`.
- Explicit refresh of an added, disabled Agent -> update filesystem/PATH
  discovery while keeping `runtime_status = disabled`; no ACP/runtime/session
  spawn. OpenCode may additionally run its bounded `--version` metadata probe.
- Native Claude/Codex CLI found but configured ACP command missing ->
  `install_status = installed`, `runtime_status = unavailable`, and bounded
  `acp_runtime_command_missing` diagnostics.
- Native/non-ACP route registration ->
  `validation/runtime_route_transport_invalid`.

### 5. Good/Base/Bad Cases

- Good: disabling `codex` removes every Codex ACP option from the selector and
  a direct create with a Codex `SessionRuntimeSelection` fails before session
  insertion or Adapter work.
- Good: Claude has both a configuration-only `kind = claude` API profile and an
  online `kind = acp` managed-Adapter profile; only the latter appears in the
  Runtime Option Catalog.
- Good: `codex` is on PATH but `codex-acp` is not; Config Center reports Codex
  installed and its online runtime unavailable instead of asking the user to
  reinstall Codex.
- Base: opening Config Center calls `agent_list({ includeDisabled: true })`;
  OpenCode appears as an ACP-backed concrete agent with cached/unknown status
  and no `opencode acp` process is started.
- Base: local-Agent search temporarily adds OpenCode in a disabled state;
  explicit refresh finds the `opencode` executable, then the UI may enable it.
- Good: explicit OpenCode refresh records the CLI's detected semantic version,
  the compatible range resolves, and the API Key/Endpoint/Model/Wire API
  controls appear without pinning a bundled CLI version.
- Base: a catalog option omits Effort/Mode; the Adapter converges defaults and
  the committed attachment still matches the selection because Model is exact.
- Good: a configured Codex model receives only the reasoning efforts returned
  for that exact model by stateless `model/list`; no generic effort list is
  fabricated when the probe is unavailable.
- Bad: frontend hides a disabled agent but backend still accepts direct session
  creation.
- Bad: `agent_list` starts `opencode acp`, Claude, Codex, or any other runtime
  just to render settings.
- Bad: UI sends `providerKind = claude`, manager derives the Agent/Adapter, or a
  configuration-only Claude profile is treated as online execution authority.

### 6. Tests Required

- `cargo test -p vibex-core agent` covers `AgentId` validation.
- `cargo test -p vibex-db agent` covers config/discovery persistence and latest
  discovery selection.
- `cargo test -p vibex-config-switch agent` covers registry merge, enabled
  filtering, low-cost PATH refresh for an added disabled Agent, removed-Agent
  skip behavior, native CLI versus ACP runtime readiness, profile-kind
  separation, and ACP-only catalog filtering.
- `cargo test -p vibex-agent agent` covers disabled-Agent gating, exact route
  lookup, non-ACP registration rejection, and no ProviderKind fallback.
- `cargo test -p vibex-agent-acp --lib` covers exact Claude/Codex managed route
  ids and optional Effort/Mode effective-config matching.
- `cargo test -p vibex-desktop management` covers the unified Agent list order,
  card actions, and absence of the catalog drawer and failover controls.
- `cargo test -p vibex-desktop agent` and
  `pnpm --dir apps/desktop typecheck` and frontend builds
  cover Runtime Option Catalog/create contract consumption.

### 7. Wrong vs Correct

#### Wrong

```rust
if !snapshot.added || !snapshot.enabled {
    return disabled_without_checking_path();
}
let installed = resolve_binary_path(&runtime_command).is_some();

let agent_id = agent_id_for_provider_kind(request.provider_kind);
let provider = self.provider(request.provider_kind)?;
provider.create_session(request).await?;
```

#### Correct

```rust
if !snapshot.added {
    return removed_without_checking_path();
}
let runtime_binary = resolve_binary_path(&runtime_command);
let agent_binary = resolve_native_agent_cli(&agent_id)
    .or_else(|| runtime_binary.clone());
let install_status = install_status_for(agent_binary.as_ref());
let runtime_status = if snapshot.enabled {
    runtime_status_for(runtime_binary.as_ref())
} else {
    AgentRuntimeStatus::Disabled
};

let selection = request.runtime;
let agent = self.resolve_enabled_agent(
    Some(selection.agent_id.clone()),
    ProviderKind::Acp,
    true,
)?;
let profile = ProviderProfileRepository::get(
    &conn,
    &selection.provider_profile_id,
)?.ok_or_else(provider_profile_not_found)?;
if profile.agent_id != selection.agent_id
    || profile.kind != ProviderKind::Acp
    || profile.status != ProviderProfileStatus::Enabled
{
    return Err(provider_profile_route_mismatch());
}
let route = self.route_for_agent(&agent.agent_id)?;
self.runtime(&route)?;
runtime_selection.initialize_new_session(&session_id, selection).await?;
```

Backend session creation owns durable enforcement; frontend catalog filtering
is only a convenience. `ProviderKind` remains valid in configuration/provenance
APIs but does not participate in the online route above.

## Scenario: ACP Registry Managed Agent Installation Lifecycle

### 1. Scope / Trigger

- Trigger: the user clicks Add, Upgrade, or Uninstall for an ACP Agent whose
  `AgentDefinition` maps to the verified ACP Registry. The right-hand Config
  Center panel is the user-visible operation surface.
- Agents without a verified Registry distribution remain external-CLI
  Agents; Vibex does not download or replace their user-installed command.

### 2. Signatures

```text
AgentInstallService::install(agent_id).await
AgentInstallService::check_update(agent_id).await
AgentInstallService::uninstall(agent_id).await
AgentInstallService::bootstrap_agent_ids() -> Vec<AgentId>
AgentNodeRuntimeOptions { node_path, npm_path }
AgentManagedInstallationRecord {
  agent_id, registry_agent_id, state, command, install_root, updated_at_ms
}
```

### 3. Contracts

- Config Center Add starts the managed installation. The panel shows a
  disabled loading state until the verified command is published; only then
  does the UI set `added = true`/`enabled = true`, refresh runtime capability
  data, and discover authentication methods for the user to choose.
- The upstream Registry response is cached for one hour. Binary archives must
  carry a SHA-256. npm distributions must use an exact version, metadata
  integrity, and the canonical `registry.npmjs.org` tarball URL; the lockfile
  `resolved` URL must match that same canonical artifact.
- Pi is a managed npm bundle: Vibex installs the Registry-pinned `pi-acp`
  Adapter and the Vibex-pinned `@earendil-works/pi-coding-agent` runtime in the
  same isolated tree. Both direct packages independently pass metadata,
  integrity, canonical tarball, executable, and lockfile identity checks. The
  generated launcher sets `PI_ACP_PI_COMMAND` to that tree's `.bin/pi` command;
  it never depends on or replaces a user-global Pi installation.
- npm Agents select Node/npm in this order: explicit
  `VIBEX_AGENT_NODE_PATH`/`VIBEX_AGENT_NPM_PATH` configuration, system
  `node`/`npm` from `PATH`, then a downloaded Vibex-managed Node.js 22 runtime.
  Explicit and system candidates must be real files, `node --version` must be
  SemVer 22.0.0 or newer (22.19.0 or newer for Pi), and both version probes must
  succeed; rejection falls through to the next candidate without activating a
  partial installation.
- npm always uses an isolated Vibex cache and blank user/global npm config,
  regardless of runtime source. The user and global config paths must be
  distinct: npm rejects one file assigned to both configuration levels as a
  duplicate load before package resolution begins. The install root is
  content-addressed by the selected runtime identity, publishes through
  staging, keeps healthy versions side by side, and only prunes old versions
  after the new command is durable. Archive paths, extraction budgets, and
  executable paths are bounded and traversal-safe.
- A usable installed SemVer may not be replaced by a lower Registry SemVer.
  Exact-version cache hits are idempotent; an invalid cache entry is removed
  and rebuilt. Pending install/upgrade/uninstall rows are reconciled on the
  next startup using actual install-root and command-file checks, not only a
  version field.
- Uninstall owns the cross-layer transition: it marks the operation pending,
  removes Agent config and authentication snapshots, removes managed files and
  the installation row, and leaves the Agent disabled/deleted. If a later step
  fails while the old command remains usable, the old command and prior
  `added`/`enabled` state are restored; otherwise the Agent remains removed
  with a bounded Failed state.
- The managed installation record is the source for the actual command and
  installed version. ACP runtime compatibility identity, event enrichers,
  versioned operation evidence, restore policy, and process reuse may use an
  exact static descriptor only when the command/version (and required runtime
  dependencies) exactly match it. Adjacent or latest Registry versions use a
  conservative dynamic identity and do not inherit old evidence.
- Authentication catalog cleanup belongs to the configuration service as well
  as the UI boundary, so a crash between the file operation and panel refresh
  cannot leave stale auth methods for a removed Agent.

### 4. Validation & Error Matrix

- Missing Registry mapping -> `capability/agent_managed_install_unavailable`;
  Vibex falls back to the external CLI path.
- Registry parse, unsupported platform, missing checksum, invalid archive,
  canonical npm source, integrity, lockfile, or executable mismatch -> a
  structured validation/capability error; no partial install is activated.
- Download or extraction timeout -> bounded process error and a persisted
  `Failed`/`UpdateAvailable` state suitable for retry.
- Registry downgrade candidate -> `conflict/agent_install_downgrade_rejected`.
- Missing, unexecutable, malformed, pre-22 Node/npm, or pre-22.19 Node/npm for
  Pi -> reject that candidate and continue through system then managed runtime
  fallback.
- Identical `npm_config_userconfig` and `npm_config_globalconfig` paths -> an
  invalid internal command configuration; construct distinct blank files and
  do not rely on npm to accept the duplicate load.
- Interrupted operation or unusable command/root -> recovery marks the row
  failed and does not re-enable a missing process.
- Uninstall failure -> preserve the old usable command and configuration when
  possible; never leave an enabled Agent pointing at deleted files.

### 5. Good/Base/Bad Cases

- Good: clicking Add shows Downloading, verifies the package, enables the
  Agent, detects its authentication methods, and leaves provider profiles and
  native Agent homes untouched.
- Good: Upgrade publishes a new side-by-side version, records the actual
  version in runtime identity, and keeps the old version available until the
  new command is active.
- Base: opening Config Center reads the cached installation state without
  downloading; an explicit Check for updates refreshes the Registry.
- Good: removing an Agent deletes its command, installation row, and auth
  catalog; a later startup does not reinstall it unless the user adds it again.
- Bad: activate an Agent before checksum/lock verification, use npm's global
  state, assign one blank npm config file to both user and global levels,
  accept an unprobed or pre-22 Node/npm candidate, silently downgrade, or
  restore an old exact compatibility workaround for a newer binary.

### 6. Tests Required

- `cargo test -p vibex-desktop-runtime agent_install` covers cache repair,
  canonical npm sources, lockfile identity, checksum/archive limits, SemVer
  downgrade rejection, explicit/system/managed Node selection and fallback,
  malformed/old Node rejection, distinct empty user/global npm configs,
  Pi's dual-package lock and local launcher, interrupted recovery, and uninstall
  cleanup.
- `cargo test -p vibex-config-switch agent` covers removal of runtime and auth
  snapshots plus managed command/version matching.
- `cargo test -p vibex-agent-acp runtime` covers dynamic managed identities,
  conservative event/pool/restore/evidence behavior, and exact external
  descriptor compatibility.
- Desktop management tests cover Add loading, upgrade loading, failed-install
  detail rendering, and Agent registry refresh events.

### 7. Wrong vs Correct

#### Wrong

```rust
command
    .env("npm_config_userconfig", &blank_config)
    .env("npm_config_globalconfig", &blank_config);
```

npm exits during configuration loading because the same path is assigned to
two configuration levels.

#### Correct

```rust
command
    .env("npm_config_userconfig", &blank_user_config)
    .env("npm_config_globalconfig", &blank_global_config);
```

Both files are private and empty, so the selected explicit, system, or managed
Node/npm runtime stays isolated without triggering npm's duplicate-load guard.

## Scenario: ACP Managed Adapter Compatibility Registry

### 1. Scope / Trigger

- Trigger: Vibex launches a Claude/Codex ACP runtime through a managed adapter
  instead of a built-in SDK route.
- This is a provider-configuration contract because the selected Agent,
  adapter package, exact version, resolved runtime dependency, command, install
  root, health probe, and capability evidence all decide whether the runtime may
  become ready.

### 2. Signatures

```text
AcpCompatibilityRegistry::builtin()
  -> AcpCompatibilityRegistry

AcpCompatibilityRegistry::by_agent(agent_id: &AgentId)
  -> Option<&AcpAgentCompatibility>

AcpCompatibilityRegistry::by_adapter(adapter_id: &AcpAdapterId)
  -> Option<&AcpAgentCompatibility>

ManagedAcpAdapterStore::new(root, npm, node)
ManagedAcpAdapterStore::install_or_verify(descriptor)
  -> VerifiedAcpAdapterInstallation {
       adapter_id,
       adapter_version,
       compatibility_identity,
       binary_identity,
       command
     }

VerifiedAcpAdapterActivation::verify(
  descriptor, installation, health_report, bridge_contract_report,
) -> VerifiedAcpAdapterActivation

BridgeContractReport {
  schemaVersion: 2,
  status,
  adapters: AdapterContractReport[]
}
```

### 3. Contracts

- Registry is the single source of truth for managed ACP adapter package names,
  exact adapter versions, npm integrity, trusted HTTPS registry origin, bin
  names, Node requirements, command variants, and baseline capability policies.
- Built-in managed descriptors are exact baselines: Claude
  `@agentclientprotocol/claude-agent-acp@0.64.2` with bin
  `claude-agent-acp`; Codex `@agentclientprotocol/codex-acp@1.1.9` with bin
  `codex-acp`.
- Codex compatibility identity must include the exact resolved
  `@openai/codex` runtime package from the managed install lock tree, for
  example `adapter=codex-acp@1.1.9;runtime=@openai/codex@0.146.0`.
- `codex-acp@1.1.9` still declares `@openai/codex ^0.145.0`, which excludes
  runtime `0.146.0` under npm's pre-1.0 caret semantics. The exact managed
  descriptor therefore owns an explicit npm override to `0.146.0`; this is a
  narrow versioned exception, not permission to ignore dependency ranges.
  Verification requires both the top-level runtime and every nested lock-tree
  occurrence to match the managed pin, integrity, and trusted registry source.
- A Codex profile with an available API-key reference projects the configured
  provider env key plus `CODEX_API_KEY`, `MODEL_PROVIDER`, and the non-secret
  adapter control value `DEFAULT_AUTH_REQUEST={"methodId":"api-key"}` into the
  child process. Secret values remain child-only and never enter typed ACP
  config, diagnostics, process fingerprints, or logs.
- Managed installs must be under a caller-owned absolute Vibex root, use
  staging plus verified publish, pass the descriptor origin explicitly through
  `npm --registry`, use exact dependencies and `--ignore-scripts`, and must not
  write global npm state, user home native config, or provider profile secrets.
- A registry origin is a credential-free HTTPS origin with no path, query, or
  fragment. Adapter and runtime-dependency `package-lock.json` entries must
  contain a `resolved` tarball URL whose origin and canonical package artifact
  path exactly match that descriptor origin, package, and version. Git, file,
  path, alternate-origin, missing-source, and UI-supplied sources fail closed.
- Install verification reads real `package.json`, `package-lock.json`, bin
  target metadata, dependency versions, dependency integrity, and Node
  requirements. Command output alone is not sufficient evidence.
- Capability resolution priority is `NegotiatedRuntime > ObservedRuntime >
  VersionedRegistry > DeclaredProfile > ConservativeDefault`. An observed
  negative for one ACP operation overrides static positives only for that
  operation and only for the matching identity/generation.
- Quirks, config aliases, and event enricher factories match exact
  compatibility identity. Adjacent versions must not inherit workarounds.
- Real Bridge Version Contract evidence is required before a managed baseline is
  treated as verified. Mock/fixture reports may test aggregation logic, but
  cannot validate production baselines.
- A version is activatable only through `VerifiedAcpAdapterActivation`. Install,
  health, and real contract evidence must describe the same adapter, version,
  compatibility identity, and binary identity. Verification recomputes the
  contract summary from the descriptor case matrix and case results; it never
  trusts a mutable public `gatePassed` field by itself.

### 4. Validation & Error Matrix

- Duplicate `AgentId` or `AcpAdapterId` registration -> validation error.
- Unknown managed agent lookup -> no descriptor; do not fall back to Claude,
  Codex, or a generic ACP descriptor.
- Relative install root or unsafe adapter/package path segment -> validation
  error before filesystem mutation.
- Non-HTTPS, credentialed, non-origin registry URL ->
  `validation/acp_registry_origin_invalid` before npm spawn.
- Adapter package/version/integrity/bin mismatch -> validation or process error;
  installation is not runtime-ready.
- Missing or mismatched lock `resolved` URL ->
  `validation/acp_managed_package_resolved_missing` or
  `validation/acp_managed_package_source_mismatch`; installation is not
  published or activated.
- Codex resolved `@openai/codex` version/integrity mismatch -> validation or
  process error; compatibility identity is rejected.
- `node --version`, adapter `--version`, or ACP `initialize` timeout/exit/wrong
  metadata -> structured health failure with bounded redacted diagnostics.
- Required Bridge Contract case fails or is blocked -> gate fails.
- Install/health/contract identity mismatch ->
  `validation/acp_adapter_activation_identity_mismatch`.
- Fixture evidence, forged summary, or recomputed failed case ->
  `capability/acp_adapter_activation_contract_gate_failed`.
- `when_advertised` Bridge Contract case is not advertised -> case is skipped;
  once advertised, failure fails the gate.

### 5. Good/Base/Bad Cases

- Good: Claude `0.64.2` installs in the managed root, verifies the lock/bin
  metadata and npm origin, passes health probe and the real contract, then
  produces activation evidence for identity
  `adapter=claude-agent-acp@0.64.2`.
- Good: Codex `1.1.9` resolves `@openai/codex@0.146.0`; changing that runtime
  version changes compatibility identity and invalidates stale evidence.
- Base: `session/set_model` or `session/fork` is `not_advertised`; the contract
  report records a skip only when the descriptor marks it `when_advertised`.
- Bad: using `codex-acp@1.1.9` alone as the compatibility identity while
  ignoring the resolved `@openai/codex` package.
- Bad: accepting a successful adapter `--version` command without checking
  package-lock source/integrity, bin containment, runtime dependency,
  initialize metadata, health identity, and recomputed contract cases.
- Bad: accepting an alternate registry or trusting `summary.gatePassed = true`
  when a required case is failed.
- Bad: using fixture/mock Bridge Contract output as proof that a production
  Claude/Codex baseline is verified.

### 6. Tests Required

- `cargo test -p vibex-agent-acp registry --lib` covers descriptor uniqueness,
  route keys, exact identity, capability priority, quirks, and contract matrix.
- `cargo test -p vibex-agent-acp managed_adapter --lib` covers isolated install
  verification, explicit registry argument, resolved-source origin/path,
  lock metadata, bin containment, dependency mismatch, health probe failures,
  and concurrent publish winners.
- `cargo test -p vibex-agent-acp adapter_activation --lib` covers exact identity
  joins, real-evidence enforcement, forged summary rejection, and required-case
  recomputation.
- `cargo test -p vibex-agent-acp bridge_contract --lib` covers report schema,
  required vs `when_advertised` aggregation, blocked cases, and explicit
  capability parsing.
- `pnpm smoke:acp:bridge-contract --output <task report>` must generate the
  real schema-v2 baseline evidence for Claude/Codex and pass redaction scans.
- `cargo test -p vibex-agent`, `cargo check --workspace --all-targets`,
  `pnpm check:frontend` and `git diff --check` must pass
  after changing route/export surfaces.

### 7. Wrong vs Correct

#### Wrong

```rust
let identity = format!("adapter=codex-acp@{}", adapter_version);
if command("--version").success() {
    mark_runtime_ready();
}
```

#### Correct

```rust
let verified = store.install_or_verify(descriptor).await?;
let identity = verified.compatibility_identity();
probe.node_version().await?;
probe.adapter_version(&verified).await?;
probe.initialize(&verified).await?;
let activation = VerifiedAcpAdapterActivation::verify(
    descriptor,
    &verified,
    &health,
    &contract,
)?;
```

Runtime readiness is based on registry intent plus managed install inspection
plus health/contract evidence, not on PATH lookup or command exit status.

## Scenario: Desktop Management Facade And Automation CAS

### 1. Scope / Trigger

- Trigger: GPUI management workflows need Provider, Scheduled, Automation,
  Relay, recovery, and right-rail mutations without opening SQLite or writing
  native configuration from the view layer.
- Trigger: graph editors can hold a definition loaded at an older revision and
  must not overwrite a newer durable definition silently.

### 2. Signatures

```text
DesktopRuntime::management() -> ManagementHandle
ManagementHandle::providers() -> ProviderHandle
ManagementHandle::scheduled() -> ScheduledHandle
ManagementHandle::automation() -> AutomationHandle
ManagementHandle::remote() -> RemoteHandle

AutomationGraphRepository::replace_definition(
  connection, graph_id, nodes, edges, expected_version: Option<u32>
) -> VibexResult<AutomationGraph>
```

### 3. Contracts

- GPUI calls only typed management handles. SQLite repositories, config-switch,
  Relay lifecycle, diagnostics, and backup services remain authoritative.
- Every durable management mutation claims a stable key at the runtime boundary;
  the UI pending flag is only a duplicate-click convenience.
- `expected_version = Some(v)` is an atomic compare-and-swap fence. A mismatch
  returns `automation_graph_version_conflict` and leaves both durable rows and
  the local draft unchanged. `None` is reserved for legacy trusted callers.
- Provider projections contain presence/reference metadata only; secret values
  never enter GPUI entities, redacted summaries, logs, or fixtures.
- GPUI v1 Web plugins expose unsupported embedding plus validated HTTP(S)
  external open; no WebView or iframe probe is allocated.

### 4. Validation & Error Matrix

- Duplicate local mutation key -> `management_mutation_in_progress`.
- Expected graph revision differs from the database ->
  `automation_graph_version_conflict` with bounded expected/actual revisions.
- Self, missing, or duplicate graph edge -> typed graph validation issue before
  the repository call.
- External plugin URL is non-HTTP(S), lacks a host, or contains credentials ->
  `right_rail_external_url_invalid` or
  `right_rail_external_url_credentials_rejected`.
- Redaction sentinel appears in a diagnostic bundle -> export fails before the
  temporary file is published.

### 5. Good/Base/Bad Cases

- Good: a stale graph save reports conflict and preserves the user's draft for an
  explicit reload decision.
- Good: concurrent scheduled pause calls coalesce at the facade and the durable
  repository still owns the state transition.
- Base: a legacy caller omits `expected_version` and retains the existing trusted
  command behavior.
- Bad: a GPUI component opens a database, patches TOML/JSON strings, or returns
  a successful "delegated" message without invoking a service.

### 6. Tests Required

- DB test: definition replacement succeeds at the expected revision, then a
  second stale replacement fails and the stored node/edge set is unchanged.
- Runtime test: duplicate mutation claims fail while the first claim is held and
  are available again after its guard is dropped.
- Desktop-model tests: graph reducer rejects invalid edges, preserves dirty state
  after CAS conflict, and serializes only redacted Provider projections.
- GPUI contract test: all ten sections are generation-fenced, pairing URLs are
  exact, and Web activation reports unsupported embedding without a native host.
- Run `cargo check --workspace --all-targets --locked`, workspace tests,
  `pnpm check:frontend` and `git diff --check`.

### 7. Wrong vs Correct

#### Wrong

```rust
repository.replace_definition(&mut conn, &graph_id, nodes, edges, None)?;
// An editor loaded an older graph, but the newer definition is overwritten.
```

#### Correct

```rust
repository.replace_definition(
    &mut conn,
    &graph_id,
    nodes,
    edges,
    draft.base_version,
)?;
// On conflict, keep the local draft and require an explicit reload/merge.
```

## Scenario: Provider Profile Deletion And Runtime Catalog Coherence

### 1. Scope / Trigger

- Trigger: a Provider Profile is deleted, recreated by import, or updated while
  the desktop has cached the Runtime Option Catalog.
- This crosses SQLite profile/default state, Provider mutations, TanStack Query
  invalidation, and the `SessionRuntimeSelection` sent during session creation.

### 2. Contracts

- A successful Provider create, import, update, secret update, or delete that
  can change runtime eligibility or capability evidence must invalidate both
  Provider/model queries and `agent/runtime-options`.
- Soft-deleting a Provider Profile atomically clears active default and
  failover references to that Profile. Session/runtime history may retain the
  id for audit and restore diagnostics.
- Default-profile reads join against non-deleted Provider rows so databases
  created by older builds do not return an orphaned selection.
- An explicitly requested missing Profile must still fail closed. Do not
  silently route a session to an arbitrary fallback Provider; refresh the
  catalog and require a valid selection instead.

### 3. Validation & Error Matrix

- Profile mutation succeeds -> invalidate the Runtime Option Catalog before a
  later new-session form reuses cached data.
- Profile deletion succeeds -> Provider row is soft-deleted and its generic
  defaults, Agent defaults, and failover entries are removed in one transaction.
- Legacy default points to a soft-deleted Profile -> default query returns no
  selection and normal enabled-profile fallback may run.
- Explicit session selection contains a missing Profile id ->
  `validation/provider_profile_not_found`; never substitute another Provider.

### 4. Tests Required

- Database tests cover transactional cleanup of generic defaults, Agent
  defaults, and failover entries when a Profile is soft-deleted.
- Database tests insert legacy orphaned default rows and verify default queries
  ignore them.
- `pnpm --dir apps/desktop typecheck` and `pnpm check:frontend` pass after
  changing Provider mutation invalidation.

### 5. Wrong vs Correct

#### Wrong

```typescript
await queryClient.invalidateQueries({ queryKey: ["providers"] });
// New-session UI can still submit a deleted providerProfileId from its catalog.
```

#### Correct

```typescript
await queryClient.invalidateQueries({ queryKey: ["providers"] });
await queryClient.invalidateQueries({ queryKey: ["agent", "runtime-options"] });
```

The Runtime Option Catalog is derived Provider state and must be invalidated
with its source records.

## Scenario: Versioned Agent Provider Projection Platform

### 1. Scope / Trigger

- Trigger: adding an Agent/provider integration, changing Provider credentials,
  projecting configuration into an ACP process, or exposing Provider controls to
  Desktop/Web/Mobile.
- The platform separates reusable Provider facts, Agent process identity, and
  their versioned binding. `DesktopRuntime` remains the only mutation and Secret
  authority.

### 2. Signatures

```rust
ModelProviderProfile                    // endpoints, credentials, model catalog
AgentRuntimeProfile                     // Agent/Adapter command and runtime policy
AgentModelProviderBinding               // descriptor plus per-model interfaces
AgentConfiguredModelBinding {
    provider_model_id,
    agent_model_id,
    wire_protocol_id,
    sdk_adapter_id,
    deployment,
    process_scoped,
}

AgentProviderProjectionRegistry::resolve(&AgentRuntimeVersionIdentity)
    -> VibexResult<AgentProviderProjectionResolution>

AgentProviderProjectionEngine::plan(
    &ModelProviderProfile,
    &AgentRuntimeProfile,
    &AgentModelProviderBinding,
    &AgentProviderProjectionDescriptor,
    &str, // workspace key
) -> VibexResult<AgentProviderProjectionPlan>
AgentProviderProjectionEngine::resolve_and_materialize(
    &AgentProviderProjectionPlan,
    &Path,
    &str, // workspace key
) -> VibexResult<ResolvedAgentProviderProjection>

ProviderConfigService::{
    list_model_provider_profiles,
    create_model_provider_profile,
    update_model_provider_profile,
    list_agent_runtime_profiles,
    create_agent_runtime_profile,
    update_agent_runtime_profile,
    list_agent_model_provider_bindings,
    create_agent_model_provider_binding,
    update_agent_model_provider_binding,
    agent_provider_projection_capability,
    preview_agent_provider_projection,
    mutate_provider_credential_secret,
}
```

### 3. Contracts

- `ModelProviderProfile` never owns an Agent command or a global Wire API.
  Wire protocol and SDK adapter identity are binding-model fields.
- Descriptor lookup uses the exact `AgentRuntimeRouteKey`, detected Agent/CLI
  version, Adapter version, and managed dependency identity. Unknown, manual, or
  mismatched identity returns a conservative capability that emits no automatic
  Secret or managed overlay.
- Config Center snapshot refreshes use the explicit version-detection path for
  installed Agents whose descriptors declare semantic-version compatibility
  ranges before loading runtime capabilities. Ordinary Agent catalog reads stay
  process-free.
- Codex `0.146.0` accepts only `openai_responses`. Claude exposes no selectable
  Wire API. PATH-launched OpenCode uses its detected CLI version and may expose
  only the adapter/protocol pairs registered for the matching semantic-version
  descriptor. The OpenCode inline-provider contract currently supports
  `>=1.17.9, <2.0.0`; it was exercised against local OpenCode `1.18.11`.
- `AgentCredential` is a discriminated union covering API key, OAuth, AWS, GCP,
  Azure, Snowflake, local, and managed-subscription credentials. Ordinary
  records contain references and status only; plaintext values are resolved
  immediately before process preparation or an explicit auth operation.
- Planning is deterministic and contains no plaintext Secret. Resolution may
  produce child-process Secret env and code-owned JSON/TOML/YAML overlays, but
  the resolved wrapper's `Debug` output, preview, errors, Remote DTOs, and
  fingerprints remain redacted.
- OpenCode overlay fields that are optional in its native schema must be
  omitted when unavailable, not serialized as JSON `null`. In particular, a
  configured model without a display name is emitted as an empty model object;
  OpenCode accepts `name: string | undefined` and rejects `name: null` before
  the ACP handshake.
- Managed overlays live only under the Vibex private runtime root, use atomic
  owner-only writes, and reject absolute/parent traversal, symlink escape, and
  arbitrary catalog templates.
- Updating shared Provider or Runtime data recomputes each affected plan. Only a
  binding whose effective process fingerprint changed becomes
  `stale_restart_required`; saving never stops an active process or advances a
  runtime generation.
- `NativeBackend` may expose the typed entity CRUD, preview, capability, and
  explicit Secret mutation methods. `WebRemoteBackend` exposes only bounded
  capability and redacted preview; raw entities and Secret mutation fail with a
  stable private-boundary error.
- The legacy `ProviderProfile` facade mirrors successful create/update/delete
  operations into the three new records during the compatibility window.
  Native import remains read-only unless the separate confirmed export flow is
  used.

### 4. Validation & Error Matrix

- Codex Chat at create, update, legacy TOML, binding validation, or projection ->
  `agent_model_interface_unsupported`.
- Unknown/manual/out-of-range identity -> conservative resolution with
  `agent_projection_version_mismatch` or
  `agent_projection_version_untrusted`; no Secret projection.
- Binding references another Agent/runtime/provider or an unavailable model ->
  `agent_projection_input_mismatch`,
  `agent_model_provider_binding_agent_mismatch`, or
  `agent_projection_default_model_binding_invalid`.
- Descriptor requests a credential kind not present in its capability ->
  `agent_projection_credential_kind_unsupported`.
- Required Secret reference/value is unavailable ->
  `agent_projection_secret_reference_missing` or
  `agent_projection_secret_missing`; do not mutate current runtime state.
- Unsafe overlay root/path/symlink -> `agent_projection_runtime_directory_invalid`,
  `agent_projection_overlay_path_unsafe`, or
  `agent_projection_overlay_symlink_rejected`.
- Stale revision on any of the three entities -> the matching
  `*_revision_conflict`; never overwrite a newer record.
- Remote raw entity query or Secret mutation -> the matching
  `remote_*_private` / `remote_provider_secret_mutation_unavailable` error.

### 5. Good/Base/Bad Cases

- Good: one Provider profile binds to Claude and Codex; changing an endpoint
  stales only bindings whose effective projection uses that endpoint.
- Good: an API key is stored in approved Secret storage, resolved at ACP spawn,
  and absent from SQLite, preview JSON, `Debug`, Remote, timeline, and switch
  records.
- Good: user-installed OpenCode `1.18.11` is detected from PATH, matches the
  supported `1.x` descriptor, and exposes Endpoint, API Key, Model, and Wire API
  controls without pinning the user's CLI patch/minor version.
- Good: an OpenCode model with no display name projects as `"model-id": {}` and
  the resulting inline configuration reaches the ACP initialize handshake.
- Base: an unknown catalog Agent receives an explicit unverified capability and
  no fake API-key form or managed overlay.
- Base: a future OpenCode `2.x` remains conservative until that breaking major
  version is explicitly verified and assigned a compatible descriptor.
- Bad: infer provider projection support from installability or ACP readiness.
- Bad: serialize a missing optional OpenCode model name as `"name": null`; the
  CLI rejects the entire inline configuration before provider or model access
  can be tested.
- Bad: place one global `wire_api` on the Provider or inject the same env keys
  into every Agent.
- Bad: write `~/.codex`, `~/.claude`, or another user Agent home as the normal
  projection path.

### 6. Tests Required

- Core tests assert exact-over-range resolution, detected OpenCode range
  matching, descriptor uniqueness,
  conservative unknown/manual behavior, all eight credential variants, and
  Codex Responses-only model binding.
- Database tests assert v37 idempotent migration/backfill, three-entity CRUD and
  revision CAS, legacy id preservation, and one Provider bound to two runtimes.
- Config-switch tests assert deterministic env/JSON/TOML/YAML projection,
  private permissions, path/symlink rejection, late Secret resolution,
  redacted `Debug`/preview, selective stale propagation, and omission of an
  absent OpenCode model display name without losing a configured name.
- ACP tests assert Claude/Codex/OpenCode parity, prepare-failure fencing, and
  Profile-save stale marking without process termination.
- Backend/Remote/UI tests assert version-matched capability pass-through,
  OpenCode API-key surface selection, redacted preview, private entity/Secret
  rejection, draft preservation, eight credential surfaces, and Codex Chat
  rejection.
- Run `cargo fmt --all --check`, affected crate tests,
  `cargo check --workspace --all-targets --locked`, workspace Clippy with
  `-D warnings`, `pnpm check:frontend`, and `git diff --check`.

### 7. Wrong vs Correct

#### Wrong

```rust
let key = resolve_secret(&profile)?;
let config = format!("api_key = {key}");
write_user_agent_home(config)?;
```

This resolves too early, builds an untyped Secret-bearing string, and writes a
user-owned Agent home.

#### Correct

```rust
let plan = AgentProviderProjectionEngine::plan(
    provider,
    runtime,
    binding,
    descriptor,
    workspace_key,
)?; // references and safe metadata only
let resolved = AgentProviderProjectionEngine::resolve_and_materialize(
    &plan,
    vibex_private_runtime_root,
    workspace_key,
)?;
spawn_acp_child(resolved.child_environment())?;
```

Only the authoritative runtime resolves the Secret, and every file target is a
validated code-owned overlay under the private runtime root.

## Scenario: Agent Provider Runtime Verification Rollout

### 1. Scope / Trigger

- Trigger: adding or upgrading a catalog Agent descriptor, running an ACP
  provider probe, or deciding whether an existing binding may switch provider.
- The rollout manifest contains exactly three builtin and 36 catalog Agent ids.
  Catalog installability and provider verification remain separate claims.

### 2. Signatures

```text
agent_provider_rollout_manifest() -> VibexResult<Vec<AgentProviderRolloutManifestEntry>>
catalog_projection_descriptors() -> VibexResult<Vec<AgentProviderProjectionDescriptor>>
AgentProviderProjectionDescriptor::validate() -> VibexResult<()>
AgentRuntimeProbeService::{request, spawn, cancel, get, list}
effective_provider_fact(
  initialize, session, model_apply_response, projection,
  expected_provider_identities,
)
status_for_resolution(match_kind, descriptor) -> AgentModelProviderBindingStatus
apply_stale_state(binding, next_fingerprint, match_kind)
capture:agent-provider-runtime -> agent-provider-runtime-evidence.v1
check:agent-provider-runtime[:self-test]
```

### 3. Contracts

- `agent_provider_rollout_manifest()` is the checked source of truth for Agent,
  catalog version policy, descriptor identity, capability mode, credential/model
  shape, evidence state, smoke id, and any conservative diagnostic. Registry and
  catalog drift must fail tests instead of silently dropping an Agent.
- A descriptor with `Documented` evidence still requires a real runtime probe.
  An `Unverified` descriptor emits no automatic endpoint, Secret, model, overlay,
  or switch projection and carries a bounded diagnostic explaining the missing
  contract. Binary identity or ACP initialize alone never promotes it.
- A compatible runtime identity does not override conservative evidence. A
  binding resolved to `Unverified` stays
  `AgentModelProviderBindingStatus::Unverified`, stores no projection
  fingerprint, and cannot be changed to `Ready` by refresh/stale calculation.
  `Unsupported` follows the same rule with `Unsupported` status.
- GLM Agent `1.1.4` projects only the documented
  `ACP_GLM_BASE_URL`/`Z_AI_API_KEY`/`ACP_GLM_MODEL` environment contract.
  CodeBuddy `2.109.0` projects only
  `CODEBUDDY_BASE_URL`/`CODEBUDDY_API_KEY`/`CODEBUDDY_MODEL`. Both are
  process-scoped, restart-based, and remain `Documented` until a real smoke
  confirms the effective provider and model.
- A probe's requested timeout covers identity resolution, projection planning,
  process start, and ACP operations. Cleanup still runs after a terminal
  timeout/cancel decision and must terminate the probe process and remove its
  private root. Cancellation is observed while starting the process as well as
  between stages.
- Before spawn, remove every inherited/projected, case-insensitive occurrence of `HOME`,
  `USERPROFILE`, XDG config/data/state/cache homes, and `CODEX_HOME`, then append
  exactly one probe-owned value for each. Duplicate keys must not allow a later
  value to escape the isolated root.
- `SessionResume` evidence requires an actual successful negotiated
  `session/resume` or `session/load` request against the probe session. An
  advertised capability alone is not evidence; method-not-found is
  `Unsupported`, other request failures are `Failed`, and invalid restore
  responses are `Failed`.
- Provider verification requires an exact or semantic-version-range descriptor
  match, a safe projection fingerprint, passed binary/ACP/auth/session/model/
  provider facts, and passed redaction. A range match must include a detected
  semantic Agent version. Live switching additionally requires
  `LiveSessionConfig`, a passed switch compatibility fact, and proof that failed
  target preparation preserved the source.
- Effective-provider proof compares the runtime's explicit endpoint/provider
  observation with the safe normalized endpoint origins from the selected
  projection preview. The observed origin and exact target model must both
  match. A provider string with no planned safe identity, or the same model name
  served by a different origin, remains blocked.
- The checked evidence capture includes all 39 entries, hashes every bound
  implementation surface, scans forbidden Secret/native/path fields, and rejects
  binary-only or false live-switch claims. Missing accounts, licenses, cloud
  projects, or local model assets produce `blocked/not_run`; they never count as
  a passing real smoke.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Missing/duplicate Agent, descriptor, or Adapter identity | `agent_rollout_manifest_*` error; gate fails. |
| `Unverified` entry has a runtime home, credential/model interface, missing diagnostic, or non-Unverified switch | `agent_rollout_manifest_conservative_shape_invalid`. |
| Any descriptor control or evidence is `Unverified` without the complete fail-closed shape | `agent_projection_unverified_contract_incomplete`. |
| `Unverified`/`Unsupported` binding is created or refreshed | Keep matching conservative status and `projection_fingerprint = None`. |
| Probe timeout or cancel during spawn/ACP work | Terminal typed status, process shutdown, private-root cleanup. |
| Restore operation returns method-not-found | `SessionResume = Unsupported`; load may be tried only when negotiated. |
| Restore response is non-object, has a non-string id, or echoes another session id | `SessionResume = Failed`; never promote resume compatibility. |
| Binary/handshake succeeds without effective provider/model proof | Provider verification remains false. |
| Runtime reports another endpoint while the target model name matches | `effective_provider_mismatch`; provider verification remains blocked. |
| Projection has no safe expected endpoint identity | `effective_provider_not_confirmed`; never infer identity from the model name. |
| Operator credentials/assets are absent | Evidence is `blocked/not_run`, never a passed real smoke. |

### 5. Good/Base/Bad Cases

- Good: CodeBuddy exact `2.109.0` receives only its three documented env keys,
  the isolated probe confirms the planned safe origin plus exact model, and
  evidence remains scoped to that exact descriptor/version.
- Good: an exact Autohand descriptor without a typed projector remains
  `Unverified`, has no runtime home/fingerprint, and returns its stable
  capability diagnostic.
- Base: a licensed Agent is unavailable on the capture machine; the manifest
  and contract gate pass while the real smoke remains explicitly blocked.
- Bad: mark a descriptor `Ready` merely because version matching or ACP
  initialize succeeded, or retain a prior fingerprint for an `Unverified`
  binding.
- Bad: accept Profile B because it reports the requested model when the explicit
  endpoint observation still identifies Profile A.

### 6. Tests Required

- Core tests cover exact and semantic-range 39-entry catalog/descriptor/manifest
  identity, per-entry conservative diagnostics, and the two explicit
  environment descriptors.
- ACP probe tests cover duplicate isolation keys, bounded cleanup, independent
  fact aggregation, restore response identity, matching safe origins, wrong
  origins with identical model names, and missing expected identities.
  Mock/runtime tests must cover timeout, cancel, crash cleanup, prepare failure,
  source preservation, and startup reconciliation.
- `check-agent-provider-runtime.mjs --self-test` must reject missing/duplicate
  Agents, source/version drift, missing conservative diagnostics, Secret/path
  leakage, binary-only verification, false provider verification, and false live
  switching. Evidence changes are made only by the matching capture command.
- Config-switch tests assert conservative evidence never produces `Ready` or a
  projection fingerprint on create, update, or stale refresh.

### 7. Wrong vs Correct

#### Wrong

```rust
if resolution.match_kind == ProjectionDescriptorMatch::Exact {
    binding.status = AgentModelProviderBindingStatus::Ready;
    binding.projection_fingerprint = Some(plan.fingerprint);
}
```

Exact version matching says only which descriptor applies; it does not upgrade
that descriptor's evidence or enable an unimplemented projector.

#### Correct

```rust
binding.status = status_for_resolution(resolution.match_kind, &resolution.descriptor);
if binding.status == AgentModelProviderBindingStatus::Ready {
    binding.projection_fingerprint = Some(plan.fingerprint);
}
```

The descriptor evidence and match kind jointly determine whether any automatic
projection state may become active.

#### Wrong

```rust
if observed_provider.is_some() && current_model == target_model {
    provider_projection = Passed;
}
```

The model id is not a provider identity; two profiles may expose the same model
name.

#### Correct

```rust
let provider_matches = planned_safe_origins.contains(&observed_safe_origin);
if provider_matches && current_model == target_model {
    provider_projection = Passed;
}
```

Only descriptor-owned, redacted projection targets define the expected provider
identity. Raw endpoint paths, query strings, credentials, and response payloads
do not enter durable evidence.

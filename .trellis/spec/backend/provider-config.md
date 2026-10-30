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

The Config Center's model-provider display order is an independent Agent-scoped
preference. It is persisted separately from the failover queue, which remains a
runtime selection policy and must not be reused for visual ordering. Reordering
replaces the complete current profile id list atomically; the service rejects
duplicates, profiles belonging to another Agent, and incomplete lists.

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

## Scenario: Model-Scoped ACP Runtime Options

### 1. Scope / Trigger

- Trigger: the Runtime Option Catalog combines a configured Provider Profile
  model with Agent-, Profile-, or model-scoped ACP session configuration
  evidence.
- This contract prevents one model's reasoning effort, mode, or dynamic option
  from making another model selectable with an unsupported value.

### 2. Signatures

```text
build_runtime_option_catalog(
  agents: AgentSnapshotEntry[],
  profiles: ProviderProfileSummary[],
  evidence_by_profile: Map<ProviderProfileId, RuntimeOptionCatalogProfileEvidence>
) -> SessionRuntimeOptionCatalog

SessionModelCatalogEntry {
  model_id,
  reasoning_efforts,
  modes,
  options,
  runtime_options_complete,
  source: session | probe
}
```

### 3. Contracts

- A Profile with `configured_models` or `default_model` emits only those
  explicitly configured model ids.
- Reasoning effort is model-scoped for an explicit model. It is emitted only
  when the matching `SessionModelCatalogEntry` is complete or carries an
  explicit non-empty effort list. Profile/Agent-global effort evidence is not a
  capability grant for that model.
- A complete matching model entry is authoritative even when its effort, mode,
  or option lists are empty.
- Profile-global modes/options may supplement an incomplete live Session entry
  for the same model because ACP commonly splits current-model identity and
  session-wide configuration across fields. They must not supplement a missing
  model entry or a model-only probe entry for an explicit Provider model.
- When the Profile has no explicit model configuration and its models are
  discovered wholly from the same probe evidence, profile-global runtime
  options may be used as conservative fallback metadata.
- Provider overlays carry only runtime-supported keys. Grok TOML uses
  `model.<id>.{base_url,name,env_key,api_backend}` and
  `models.default`; do not add the obsolete `auth_scheme` key.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Explicit model has matching effort metadata | Emit exactly that model's efforts. |
| Explicit model has no matching model evidence | Emit no reasoning effort, mode, or feature inherited from Agent/Profile fallback. |
| Matching model evidence is complete with empty option lists | Emit empty lists; do not fall back. |
| Incomplete live Session entry identifies the same model | Keep its model evidence and allow session-wide mode/feature fields to supplement it. |
| User submits an unadvertised effort | Resolver returns `runtime_configuration_unavailable` with cause `reasoning_effort_unavailable`. |
| Grok overlay contains an unsupported field | Remove the field from the projector rather than ignoring the runtime warning. |

### 5. Good / Base / Bad Cases

- Good: Grok `grok-4.6` advertises `low/medium/high/xhigh`; its catalog row
  exposes those values.
- Base: a configured OpenAI-compatible model appears in Grok's model list but
  advertises no effort metadata; the row remains usable with no effort selected.
- Bad: copy Grok 4.x session-global `low` onto that configured model, then let
  the stricter runtime resolver reject new-session creation before ACP starts.

### 6. Tests Required

- Catalog unit: explicit configured model plus profile-global effort/mode/options
  produces empty runtime-option lists.
- Catalog unit: matching model entry with explicit effort preserves the effort.
- Catalog unit: an incomplete live Session model entry still receives its
  session-wide mode/features.
- Projection unit: Grok TOML contains `env_key` and `api_backend = "responses"`
  and does not contain `auth_scheme`.
- Runtime resolver: an effort absent from model evidence remains a structured
  `reasoning_effort_unavailable` failure.

### 7. Wrong vs Correct

#### Wrong

```text
configured model has no effort evidence
  -> inherit Agent/Profile effort [low, high]
  -> UI persists low
  -> resolver rejects session creation
```

#### Correct

```text
configured model has no effort evidence
  -> expose the model with an empty effort selector
  -> create the session using provider/base URL/credential/model projection
```

If a provider stores native conversation state under its profile/config home
such as Codex `CODEX_HOME`, the controlled profile directory must be stable for
the Vibex session and Provider Profile. A per-turn temporary directory is only
valid for stateless probes such as model listing; it must not be used for turns
that later resume a native thread/session id.

Codex ACP initialization must continue to identify Vibex truthfully on the ACP
wire. Its managed custom-provider overlay must separately set the HTTP
`originator` header to `codex_cli_rs`, because Codex app-server otherwise derives
that provider-facing header from the embedding ACP client name. This preserves
compatibility with Responses gateways that authorize the Codex HTTP engine while
keeping ACP client attribution accurate.

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
- A documented CLI whose ACP mode cannot advertise authentication before login
  may expose an Agent-scoped terminal login fallback. The fallback must match
  the exact known ACP launch argument and replace only that argument with
  `login`; unexpected commands fail closed. An authentication-required
  initialize exit may still return this catalog entry when terminal auth is
  enabled, while Agents without a configured fallback retain the original
  structured error.
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
| Known CLI exits initialize because login is required | return `authentication_required` with its terminal login fallback when terminal auth is enabled. |
| Known Agent id with an unexpected ACP command shape | expose no synthetic login method; return the original catalog/error. |

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
- ACP auth/runtime tests assert known terminal fallbacks replace the exact ACP
  mode with `login`, fail closed for unexpected commands, and remain available
  when a known CLI exits initialize before login.

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

## Scenario: DeepSeek Harness Multi-Protocol Provider Projection

### 1. Scope / Trigger

- Trigger: starting DeepSeek Harness through `deepseek-harness-acp` with a Vibex Model Provider Profile.
- The Harness's `llm-pi-ai` route supports OpenAI Chat Completions, OpenAI Responses, Anthropic Messages, and — from adapter `0.4.36` — Mistral Conversations; its default DeepSeek environment route must not be mistaken for the complete Agent capability.

### 2. Contracts

- Declare Chat Completions, Responses, Anthropic Messages, and Mistral Conversations as user-selectable model interfaces for DeepSeek Harness, and drop an interface again when the installed release cannot serve it. `model_interface_available_for_adapter` gates `mistral_conversations` on `DEEPSEEK_HARNESS_MISTRAL_WIRE_SINCE` (`0.4.36`, the release whose bundled runtime added the protocol to its pi-ai compat gate). The registry filters the resolved descriptor, so an older install neither offers the choice in the Provider editor nor accepts a binding that already holds it (`agent_model_interface_unsupported`); projecting it anyway would make the runtime refuse the whole route rather than ignore the field. The installed release is `adapter_version`, else `agent_version` — a managed DeepSeek Harness install records the bridge release as the Agent version.
- Materialize the selected Profile as two private files under `$DSH_HOME`: `settings.yaml` with one `llm-pi-ai.providers` route and the matching `agent-default-model` selection, and `cordis.patch.yml` with the same route as the `llm-pi-ai` home patch. Do not reduce the projection to `DEEPSEEK_BASE_URL` / `DEEPSEEK_API_KEY` alone.
- Register the route in both files because the bundled runtime moved from `0.1.5` to `0.1.7` between adapter `0.4.33` and `0.4.35` (and to `0.2.0` in `0.4.36`). `0.1.5` read this Agent's `settings.yaml` through its bundled `dsh-settings-file` plugin, which `0.1.7` removed; `0.1.7` derives llm routes from the cordis entry list, which the adapter composes on every launch from its bundle patches plus the optional `$DSH_HOME/cordis.patch.yml` home patch. A route that lives only in `settings.yaml` is then never registered, the adapter's model discovery skips it, and the `effort` ACP control disappears because the route cannot be resolved — the new session then fails as `runtime_switch_configuration_unavailable` with cause `reasoning_effort_unavailable`. The home patch is composed by every runtime version since `0.1.5`, so writing it beside `settings.yaml` keeps `0.4.32`/`0.4.33` (`0.1.5`) and `0.4.35`+ (`0.1.7`, `0.2.0`) projecting one route.
- Keep `settings.yaml` even though `0.1.7` no longer reads its routes: the adapter reads it for its standalone `agent-default-model` fallback and for `permission.defaultPreset`, and it refuses to start when the file does not parse.
- Map Vibex wire protocols to Harness API ids exactly: `openai_chat_completions` -> `openai-completions`, `openai_responses` -> `openai-responses`, and `anthropic_messages` -> `anthropic-messages`.
- Keep the selected credential in a Profile-scoped environment reference named by `apiKeyEnv`; never write its value into `settings.yaml`.
- Name that environment reference `DEEPSEEK_API_KEY`, and project the route-derived name beside it. The Harness gates `session/new`, `session/load`, and `session/resume` on its launch-level credential lookup, which resolves the default DeepSeek route by exactly that name; a Vibex-scoped alias such as `VIBEX_DEEPSEEK_HARNESS_API_KEY` satisfies `apiKeyEnv` but fails the gate with `Authentication required`. A second, prompt-time gate derives its name from the projected route instead — `<ROUTE_ID>_API_KEY`, upper-cased with every run of non-alphanumeric characters collapsed to a single `_` — and never consults the route's `apiKeyEnv`: `deepseek-harness-acp` 0.4.33 began recording the resolved route id on the session record when a Model switch happens, so the first prompt after a switch asks for `ACP_API_KEY` while the launch gate still asks for `DEEPSEEK_API_KEY`. Project both names, each carrying the selected credential's Secret; either name alone leaves one of the two gates failing with `provider_authentication_required`. Derive the route-derived name from the same route id both overlays write, so the two cannot drift.
- Project the selected model's declared display name, context/output limits, and image modality. Use the Harness defaults of 262,144 context tokens and 32,768 output tokens when those limits are undeclared.
- A Vibex route id is never a pi-ai catalog provider, so the projected `contextWindow` / `maxTokens` are the only limits the Harness knows for that Model: they are what it carries as the run's model metadata and reports back as the session's context window. The same model on an Agent account resolves the Harness's own catalog entry instead (a 1,000,000-token DeepSeek model reports 1.0m there and 262.1k on a Vibex route that declares nothing). Declaring the real limits is what makes a BYOK route report the same window as the account route. See "Declared Model Context And Output Limits".
- Write the model `input` modality only when the Model declares it: `image_input: true` -> `[text, image]`, `image_input: false` -> `[text]`. An undeclared modality omits `input` entirely, because the Harness resolves an absent entry from its own pi-ai catalog first. Projecting an explicit `[text]` for an undeclared Model makes the Harness replace every prompt image with `[image omitted because this model accepts text only; ...]` before the request leaves the process, so the Agent never receives the image and no error surfaces.
- Declare the route-level `defaultInput: [text, image]`. A Vibex-generated route id never matches a pi-ai catalog provider, so this route default is the only modality answer available to a Model the catalog does not describe. A declared `image_input: false` still wins for its own Model, and an image sent to a genuinely text-only endpoint fails loudly at the provider instead of being silently dropped.
- Project the Model's `reasoningEfforts` for every Model that does not declare `reasoning: false`. The Harness advertises its `effort` select only when the Model's reasoning metadata carries at least two levels, and it resolves that metadata from the installed pi-ai entry of the same id under the same route. A Vibex route id is never a pi-ai provider, so an omitted `reasoningEfforts` means "does not reason": the run options lose the thinking-depth selector while the same Agent on an Agent account keeps it. Project the levels the Harness's own DeepSeek route exposes — `off: null`, `high: high`, `max: max` — so selecting a depth reaches the request: `session/set_config_option { configId: "effort" }` feeds pi-ai's `thinkingLevelMap`, which is the spelling that goes on the wire.
- A per-Model declaration replaces that fallback: `ProviderModelCapabilities.reasoning_efforts` (level -> wire spelling; a valueless `off` is `null`) is projected verbatim, and only the declared levels are offered. There is no automatic source for a route Model's real vocabulary — `catalogModels(provider)` answers only for a built-in pi-ai provider id, `GET /models` disclosures carry ids and capacities but no reasoning protocol, and the ACP `effort` probe reflects the projection back. The declaration is therefore authored data, not inference: never derive levels or wire spellings from a Model id, endpoint, or vendor. Two Models of one route may declare different tables, and each selector shows its own.
- Accept only the Agent's thinking-level vocabulary — `off`, `minimal`, `low`, `medium`, `high`, `xhigh`, `max` — with a non-empty wire spelling for every level except `off`, and at least one level beyond `off`. The Harness rejects the whole settings file for anything else (unknown key, valueless level other than `off`, an offer of nothing to think with), so the Provider editor and the projection both refuse it while the Model is still nameable instead of leaving a session that cannot start.
- Saving a Model whose declared capabilities or wire protocol changed drops that Model's cached `provider_model_runtime_option_snapshots` row. A successful per-Model probe is reused by model id, and the cache key carries no part of the declaration, so without the drop the run options keep advertising the previous vocabulary and an edited declaration looks ignored. An edit that cannot change a probe answer (display name, notes, unrelated fields) keeps the evidence.
- Pin the route-level `compat: { supportsDeveloperRole: false }` on every route whose `api` is `openai-completions` or `openai-responses`. pi-ai sends a reasoning Model's system prompt in the `developer` role unless the endpoint's compatibility report says the endpoint takes it, and it resolves that report by detecting the provider id and the endpoint — neither of which recognizes a Vibex route, so the OpenAI default `true` stays in force. A gateway that only speaks OpenAI's older vocabulary then rejects the whole turn (`400`, unknown variant `developer`) while the same Model on an Agent account keeps working, and the rejection reaches the run options as a bare JSON-RPC failure. The pin and `reasoningEfforts` travel together: the role only changes once the Model reasons. Never write the pin for an `anthropic-messages` route — Anthropic Messages offers no such switch, and the Harness refuses a route-level switch no Model on the route can apply rather than ignoring it, which fails the entire settings file.
- Keep the projected ACP model id bare, and accept the Harness's `route::model` spelling only as a read-back alias. `deepseek-harness-acp` 0.4.33 qualifies every model option id once more than one provider route is registered, and the Harness always mounts its own `deepseek-official` route beside the projected `llm-pi-ai` one, so the qualifier is always present on a Vibex session; 0.4.32 answers that same spelling with `-32602 unknown model`. The bare id is therefore the only form both Adapter versions accept on the wire, while the qualified spelling exists so a model id the Harness reports resolves back to its product Model. Without the alias the reported id no longer equals the requested one and the switch fails as `acp_session_config_response_mismatch` before the session converges. Derive the route id exactly as both overlays write it — `vendor_hint`, else the Profile id — never by re-spelling the Profile display name, which is not the route. The bare id is the only form both Adapter versions accept, so the catalog pin moved to `0.4.33`, then `0.4.35`, and now `0.4.38`, while the compatibility floor stays `0.4.32`: raising the floor with the pin would make an installed `0.4.32` runtime conservative and hide the provider editor for a release the projection still supports. The `0.4.36` through `0.4.38` pins need no new read-back shim — the qualification is byte-identical — so the floor does not move with them either.
- Roll back installs the Adapter version Vibex pins in its own catalog for that
  Agent, and is the only install target allowed to move an installation
  backwards. The pin is Vibex's compatibility statement, so a user asking for
  it is not the stale-candidate case the channel target rejects; scoping
  `reject_semver_downgrade` to the channel target is what keeps both rules
  true at once.
- Offer the rollback only where it can actually complete: a usable managed
  installation, a recorded npm or uvx distribution, and a catalog pin that
  differs from the installed version. A binary distribution carries one
  release archive URL with no version to swap, and an Agent whose catalog entry
  is `manual` has no pin at all, so neither may show the action. Derive this
  from the installation state and the catalog rather than storing it, so a
  snapshot can never offer a version the catalog has since moved past.
- Move the distribution's exact spec and the entry version together when
  pinning, because the parser, the install fingerprint, and the version the
  record reports all read both. A rewrite that changes only one leaves an entry
  the installer rejects as `agent_npm_spec_invalid`.
- A rollback is not a hold. The Agent keeps tracking its own release channel
  afterwards, so the next update check may report a newer version again; that
  is the user's decision to make, not a state Vibex pins them into.
- Read a Model the Agent reports back as authoritative on a release that substitutes, and as a mismatch on one that does not. `0.4.36` (runtime `0.2.0`) resolves a Model the projected route no longer lists to the removed-id successor (`deepseek-v4-flash` -> `deepseek-flash`) or the route's first Model, and answers `session/set_config_option` with the substitute. `runtime_config_substituted_model` recognises that answer only for a Model field on a Live/Extension plan from a release at or after `DEEPSEEK_HARNESS_MODEL_SUBSTITUTION_SINCE` (`0.4.36`); the confirmation then records the substitute as both `effective_model` and `preferred_model` — a preference the route cannot serve would never converge — and the attachment's current Model follows it. Every other case keeps `acp_session_config_response_mismatch`, which is what `0.4.35` and older need: they send the requested id verbatim, so a different answer is unexplained. The completed-message marker (`_meta.dsh.event == "assistant_message"`, read before the empty-text bail-out) corroborates the same replacement once, and `model_substitution_notice` keeps it to one `SystemNotice` per distinct pair.
- Stream token deltas from the live channel, not from the durable log. `0.4.36` publishes them as `agent/assistant-stream` frames and stops writing `assistant/chunk`; the adapter projects them onto the same `agent_message_chunk` / `agent_thought_chunk` updates Vibex already consumes, so an upgraded install streams without a runtime change. Do not expect disk-truth for deltas: `assistant/message` carries the compact stream, not one event per token.
- Treat `ask_user_question` as ordinary form elicitation. `0.4.36` routes the Harness question tool to `elicitation/create` (`mode: "form"`, `requestedSchema` carrying `oneOf`/`anyOf` choices, `_meta["dsh.userQuestions"]` beside it), gated on the client advertising `elicitation.form` — which Vibex always does. The field ids are `question_<n>` and `question_<n>_custom`, and the answer echoes the schema's `const` values, so no Harness-specific response encoding is needed.
- Fork a session natively only where the Agent advertises it, and cut only where the Agent can. `sessionCapabilities.fork` enables `session/fork`; `agentCapabilities._meta.jetbrains.air.fork` version `1` (`0.4.37`) additionally allows an inclusive cut that keeps one assistant message and drops every later one. Carry the provider's own message id — DeepSeek Harness publishes `<turn>:<step>` on every projected chunk, retained as the timeline correlation id — and anchor a mid-timeline fork to the last identified assistant segment at or before the requested sequence. Request the cut only when that extension was negotiated: an Agent without it copies the whole committed log, which would leave the new provider session holding messages the app-level timeline does not have, so the runtime refuses and the existing fresh-session replay runs instead. Provider correlation ids are not copied into a fork's timeline (see the fork projection contract).
- Advertise `clientCapabilities._meta["subagent-transcript"]`. The bridge then relays a child session's updates onto the root session, tagging each with `_meta.dsh.subagent` and announcing `_meta.dsh.event == "subagent/lifecycle"`. Scope every id derived from such an update to its child (`subagent:<childSessionId>:<native id>`) — message ids for the fork cut and tool-call ids for the card store — so a child cannot fold into its parent's row; emit the lifecycle marker as a bounded system notice. An attributed update may not rename the session either: the child's own generated title is the prompt the parent delegated to it, so applying it here replaces the root session's title — and its sidebar row — with that prompt.
- Provider, protocol, endpoint, credential, or model changes remain process-scoped and require restart and resume.

### 3. Tests Required

- Core descriptor tests assert all three protocols are advertised in stable order.
- Projection tests parse `settings.yaml` for every protocol and assert route/default-model identity, model capabilities, and absence of Secret material.
- Projection tests parse the `cordis.patch.yml` home patch and assert it is a one-row `llm-pi-ai` list whose `config.providers` route matches the `settings.yaml` route — same route id, `api`, `apiKeyEnv`, `baseURL`, and per-Model `reasoningEfforts` — across every protocol, and that the plan carries both files with `settings.yaml` first.
- Projection tests assert an undeclared modality writes no model `input`, a declared `false` writes `[text]`, a declared `true` writes `[text, image]`, and every route carries `defaultInput: [text, image]`.
- Projection tests assert an undeclared or `true` reasoning capability writes the `off`/`high`/`max` map with identity wire spellings and no undeclared level, while `reasoning: false` writes `reasoningEfforts: false`.
- Projection tests assert a declared table is written verbatim — including a wire spelling that differs from its level — replaces the fallback instead of extending it, differs between two Models of one route, and that an undeclarable table (unknown level, valueless non-`off` level, nothing beyond `off`) fails with the Model named rather than reaching `settings.yaml`. A declared table beside `reasoning: false` still projects `false`.
- Provider editor tests assert a declaration survives profile save normalization, catalogue merge, and an authoritative catalogue refresh for a Model the catalogue still advertises.
- Projection tests assert the developer-role pin is present with value `false` exactly on the Chat Completions and Responses routes and absent — with no `compat` key at all — on the Anthropic Messages route, and that a reasoning route Model keeps its projected `reasoningEfforts` alongside the pin.
- Projection tests assert the DeepSeek Harness read-back alias is `{projected route id}::{model id}` — the same route id the settings overlay writes — and that no other Agent projection declares one. Runtime tests assert the alias resolves to its product Model while the projected wire form stays bare, and that an alias another Model already owns is rejected as `acp_model_id_projection_ambiguous`.
- Runtime tests assert the DeepSeek Harness descriptor pins `0.4.38` while accepting `>=0.4.32`: a `0.4.32` identity resolves to the typed projection with its credential and model controls, a `0.4.33` identity resolves the same way as a semver-range match, and `0.4.31` is conservative with `agent_projection_version_mismatch`. A `0.4.35` identity keeps the three chat protocols, `0.4.36` through `0.4.38` also offer Mistral Conversations, and a binding holding that protocol fails `agent_model_interface_unsupported` against the `0.4.35` descriptor.
- Core tests assert each DeepSeek Harness release gate against its own release: `0.4.35` substitutes nothing, accepts no Mistral route, and advertises no inclusive fork; `0.4.36` substitutes and accepts Mistral; `0.4.37` satisfies the fork gate; `None`, blank, and malformed versions satisfy none of them; an explicit Adapter version wins over the Agent version.
- ACP tests assert the frozen `initialize` shape carries `_meta["subagent-transcript"] = true`, the inclusive `session/fork` params carry `_meta.jetbrains.air.fork` version `1` with the message id while the plain fork shape stays `{ sessionId, cwd, mcpServers }`, an unknown extension version is not read as supported, child-session ids are scoped, a `title` on a subagent-attributed `session_info_update` is ignored while the root session's own still lands, and the completed-message marker reports its Model. A switch test drives a mid-timeline cut against a mock that advertises the extension and asserts the request; the same cut against a mock without it asserts no `session/fork` at all plus the `session/new` fallback. A process test asserts a differing Model answer is a substitution on `0.4.37`+ and `None` (a conflict) on `0.4.35` and on an unmanaged `unknown` install.
- Manager tests assert the fork cut anchors to the last identified assistant segment at or before the requested sequence, that an anonymous segment yields no cut, and that the fork projection still drops provider correlation ids.
- The typed projector matrix asserts the private `settings.yaml` and `cordis.patch.yml`, `DSH_HOME`, and Vibex-scoped credential environment boundary.

## Scenario: Provider Request Headers And Session Variables

### 1. Scope / Trigger

- Trigger: the Provider editor's request-header rows (name + value) carry a Profile's headers into the Agent's model requests, and a value may be a built-in dynamic variable rather than a literal. The concrete case is OpenCode Go, whose endpoint rejects a request without a stable per-conversation `x-opencode-session` header; a Vibex Profile expresses that as the built-in `session_id` variable.

### 2. Contracts

- Persist rows in the legacy `headerOverrideJson` Provider option: a JSON object keyed by header name whose value is a string (literal) or `{"variable":"session_id"}` (dynamic). The Profile sync and the editor accept exactly those two shapes and ignore anything else, so a hand-edited option never becomes a header the user never configured. Header names are HTTP field tokens, unique case-insensitively, at most 32 rows; a rejected save names the row and keeps the dialog open.
- Header values are literal or dynamic — never credentials. Do not map a sensitive-looking value onto the Profile credential the way the pre-header-era sync did; the credential keeps its own `apiKeyEnv` / `apiKey` surface, and a `SecretReference` header is skipped by projections rather than repeated as a header.
- The built-in variable set is typed in `ModelProviderHeaderVariable`; each entry owns its stable wire id (`session_id`), its environment key (`VIBEX_SESSION_ID`), and a projection spelling per Agent. Adding a variable means adding those three, not a string at a call site.
- Fill a session variable at launch, not in the overlay file: the projection names the environment keys it references through `session_env_keys`, and the ACP launch writes each key with the Vibex session id of the process it is spawning. Fill them only when the effective process strategy is `PerSession` — a pooled process would answer every session with the first session's id — and only when the launch serves a session. The value is child-only: it is deliberately excluded from the spawn snapshot, so two sessions of one Profile keep one process identity and a profile-refresh comparison never sees a stale snapshot.
- DeepSeek Harness writes literal headers to both `settings.yaml` and `cordis.patch.yml`. A dynamic header travels only through the Cordis home patch, spelled `!!js process.env.<KEY>`: `settings.yaml` is read as plain YAML by the bundled runtime's own loader, which would send the expression text itself, while the Cordis loader evaluates `!!js` on every launch (verified on the bundled `0.1.5` and `0.2.0` runtimes). Because `serde_yaml` cannot emit a secondary (`!!`) tag, the writer emits the reserved `VIBEX_DYNAMIC_HEADER_ENV_` plain-scalar placeholder and expands it after encoding; a placeholder not followed by an environment key fails the projection instead of reaching the Agent.
- OpenCode writes headers to `provider.<id>.options.headers`; a dynamic value is spelled `{env:<KEY>}`, the same substitution OpenCode already applies to the projected credential.
- The header fingerprint is part of the projection fingerprint whenever a Profile declares headers, so adding, removing, or re-spelling one relaunches the Agent instead of leaving a process built from the previous header set.
- The editor offers one-click common headers (`x-opencode-session` bound to `session_id`, `User-Agent`, `HTTP-Referer`, `X-Title`), imports a JSON object whose values are strings or `{"variable":...}`, and copies the rows back in the same shape. The copy action tolerates unfinished rows; save does not. The section appears only where `AgentProviderProjectionCapability::supports_request_headers` holds — DeepSeek Harness and OpenCode — so no other Agent stores rows its own projection never writes.

### 3. Tests Required

- Core tests assert the header value round-trips as `{"type":"dynamic_variable","value":"session_id"}`, that `dynamic_variable_env_keys` de-duplicates and orders keys, and that profile validation rejects duplicate names and non-token names.
- DB tests assert the legacy override maps string values to literals and `{"variable":...}` values to dynamic variables while skipping unknown shapes.
- Projection tests assert a Harness route carries a literal header in both `settings.yaml` and `cordis.patch.yml`, carries a dynamic header as `!!js process.env.VIBEX_SESSION_ID` only in the home patch, and that the plan names `VIBEX_SESSION_ID` in `session_env_keys`.
- Projection tests assert an OpenCode route spells a dynamic header as `{env:VIBEX_SESSION_ID}` in the inline provider config and that its plan names the same key.
- Runtime tests assert a session-scoped launch resolves the session environment from the session id while a pooled or session-less launch resolves none, and that the editor's override parser round-trips literals and variables and ignores unknown shapes.

## Scenario: Declared Model Context And Output Limits

### 1. Scope / Trigger

- Trigger: the Provider Model editor declares a Model's context window or maximum output tokens, and a projection carries that limit into an Agent's own configuration.
- Only the projections whose Agent config owns a limit read the declaration: DeepSeek Harness, OpenCode, and ZCode. Every other Agent resolves its window from its own catalogue or API, so the declaration must not be invented for it.

### 2. Contracts

- `ProviderModelCapabilities.context_tokens` and `output_tokens` are authored data, entered per Model in the Provider Model editor. Never infer either from a Model id, endpoint, or vendor, and never expect a catalogue refresh to carry one: an Agent-owned catalogue answers ids and wire protocols only.
- An empty field means "not declared", and the projection omits the key or keeps its own default. Zero is rejected by the editor rather than read as unknown, so clearing the field is the only way back to the Agent default.
- The editor parses both fields before it mutates the Model, so a rejected value leaves the draft exactly as it was instead of half-applying the edit.
- DeepSeek Harness writes `contextWindow` / `maxTokens` (undeclared -> 262,144 / 32,768), OpenCode writes `limit.context` and `limit.output` only when both are declared, and ZCode writes `limit.context`.
- The declaration survives profile save normalization, catalogue merge, and an authoritative catalogue refresh, like every other declared capability.

### 3. Tests Required

- Editor parse unit: an empty pair returns undeclared, whole positive numbers are accepted, and zero, negative, fractional, or oversized values are rejected with the field named.
- Round-trip unit: declared limits survive profile normalization, catalogue merge, and an authoritative catalogue refresh.
- Projection units assert each Agent's keys for a declared and an undeclared Model.

## Scenario: Agent Session Options The Agent Cannot Express

### 1. Scope / Trigger

Three failures with one shape: Vibex sends a session option the Agent has no
field for, or sends the right field in the wrong wire shape. Each one failed a
runtime switch or a turn on an Agent that was otherwise healthy.

### 2. Contracts

- **A boolean option carries the ACP `type` tag.** ACP 1.6 flattens
  `SessionConfigOptionValue` into `session/set_config_option`, so a boolean is
  `{ sessionId, configId, type: "boolean", value: true }` — the tag is a sibling
  of `value`, not a wrapper around it. A bare `value: true` is read as a select's
  value id and refused with `expected string, received boolean`, which failed the
  whole configuration step. Current Cline and Copilot both validate this.
- **An option the Agent does not advertise is dropped, not fatal.** A selection
  carries one reasoning effort per Agent, but a route group is shared, so a
  switch to an Agent with no depth dimension (Kimi Code publishes `thinking` as
  an on/off toggle and no effort scale) routinely carries a level it never
  offered. The field is removed from the patch, its preference is cleared, and
  the remaining fields still apply. The check lives inside the per-field loop
  because a Model switch can publish the dimension — an Agent that only
  advertises an effort option for an effort-capable Model must still receive it.
- **An option update reports what the Agent publishes, not a full state.** An
  Agent that never reports a Model — Copilot binds it to the `--model` launch
  argument and omits it from both `session/new` and every `config_option_update`
  — must not have its effective Model cleared by an update that says nothing
  about it. The launch model stays effective until a model option replaces it;
  clearing it made the turn fence reject every prompt with
  `turn_execution_identity_mismatch` against a binding that still named it.
- **A projector writes every file its Agent needs to authenticate.** Antigravity
  reads its credential mode from `settings.json` in its state home and does not
  infer it from `GEMINI_API_KEY` being present; the projection writes
  `antigravity-acp/settings.json` with `auth.type = "gemini-api-key"`. Without it
  `session/new` is refused with `Authentication required` and the session never
  reaches a prompt.

### 3. Contracts — Gemini streaming and Cline's endpoint

- **A Gemini action suffix is the stream flag.** `:generateContent` answers one
  object, `:streamGenerateContent` answers SSE; the body carries neither. The
  path model and the suffix are both copied into the body by `gemini_call`, so
  the shared streaming pipeline sees the same shape it sees for the other three
  protocols. Without it a streaming call was answered with a whole JSON object,
  which the CLI cannot read as SSE — the request looked healthy in the ledger
  and the turn still failed.
- **A route group needs the same ACP authentication as a Profile.** A route
  resolves to a Provider Profile, so it launches through the same credential
  contract. `provider_backed_auth_method` keyed on `ProviderProfile` alone and
  skipped the `authenticate` handshake for every route-group launch; Antigravity
  then refused `session/new` with "No authentication method selected" even
  though both `GEMINI_API_KEY` and `auth.type` were present.
- **Gemini 3 publishes its tool schema as `parametersJsonSchema`.** Reading only
  the older `parameters` key gave every declaration an empty object, so a
  translated tool call reached the upstream with no field names and the model
  guessed them (`path` where the Agent requires `file_path`).
- **A reasoning dimension the Agent does not have is normalized before
  journaling.** Dropping it only while applying the configuration left the
  durable selection still naming it, so the binding converged on `thinking: on`
  while the submission asked for `off` and every dispatch was refused with
  `message_submission_runtime_binding_mismatch`.
- **Antigravity needs gateway mode to accept a route-group Model.** Its built-in
  catalogue holds only its own Gemini ids and refuses anything else through the
  `model` config option ("not available for the current authentication method").
  The projection therefore writes `auth.type = "gateway"` and injects
  `AGY_LLM_GATEWAY_URL` (origin only — Antigravity appends the Google API path),
  `AGY_LLM_GATEWAY_MODELS` (the projected *runtime* id, including the
  thinking-level suffix its picker uses), and `AGY_ACP_ENABLE_GATEWAY_AUTH`.
  This is the one Agent whose projection deliberately leaves its documented
  credential mode, because that is the only mode that can serve a route group.
- **A JSON Schema type name is lowercase outside Gemini.** Gemini's wire says
  `"OBJECT"`/`"STRING"`; OpenAI, Anthropic, and the JSON Schema spec say
  `object`/`string`. Forwarding the upper-case form made the upstream reject the
  whole request (`Invalid schema for function 'read_file': "STRING" is not valid
  under any of the schemas listed in the 'anyOf' key`). The Gemini codec
  lowercases on the way in and uppercases on the way out; only `type`-keyword
  values that name a schema type convert, so property names, enum literals, and
  `format` strings keep their case.
- **A schema refusal names the field it objected to.** Agents answer a refused
  request with `Invalid params` plus a structured `data` body
  (`{"<field>": {"_errors": [...]}}`). Reading only `data.details` (a string)
  left the journal at "Invalid params", which is unactionable.
- **A route group names no Profile, so capabilities cannot be read by Profile.**
  `SessionRuntimeSelection::provider_profile_id()` is `None` for
  `RuntimeAuthSource::ModelRoute`, which sent every route-group session to the
  conservative static set. That set had `permission_requests` cleared, so
  approving any prompt on any route-group session was refused with
  `acp_permission_resolution_unsupported` — one Agent looked singled out only
  because it was the one being tested. Two changes fix it: the ACP provider
  resolves a route to its launching Profile
  (`legacy_profile_for_route`) through `capabilities_for_auth_source`, and the
  static floor describes the protocol (streaming, persistence, tool calls,
  interrupt, permissions, elicitation) rather than the emptiest possible set.
  Clearing a callback flag there disables something the Agent has already been
  observed to send.
- **A route model is shown by the Provider's name, not its gateway address.**
  The effective model of a route session is `group/<id>/<model>` — an address
  the gateway resolves. Attribution wrote that id straight into `model_label`,
  so the turn header and the answer footer printed the address while the
  composer showed the model name the Provider's catalogue carries. The same
  selection then read as two different models on one screen. `model_label`
  resolves through the route's members to the first `display_name` their
  catalogues give that model, and the client's own fallback strips the group
  prefix instead of printing `model_route_<hex>`/`group/<id>/<model>`.
- **A tool result is a tool turn, whatever role wraps it.** Gemini pairs a
  `functionResponse` back to its `functionCall` by name, and Antigravity sends
  both under `model`. Reading that role literally put the result inside an
  assistant message, so the Chat/Responses encoders emitted a `function_call`
  with no `function_call_output` beside it and the upstream refused the whole
  turn with `An assistant message with 'tool_calls' must be followed by tool
  messages responding to each 'tool_call_id'`. A content whose parts are
  entirely tool results is re-homed to the user role during parsing — the
  canonical form has always been a user-role tool result, so the codecs stay
  unchanged. Tools had already run by then, so the turn died after doing its
  work.
- **A reasoning depth in a model id is not a second model.** Antigravity's
  picker publishes one model per depth, so it asks the gateway for
  `group/<id>/<model>-high` while the group holds the base model. Treating the
  suffix as part of the model answered `404 No configured Provider serves the
  model 'group/.../deepseek-v4.1-flash-high'` on every turn. The gateway strips
  `-high`/`-medium`/`-low` when the group does not declare the suffixed id, and
  the level still reaches the upstream through the request's reasoning.
- **A protocol that issues no call ids still needs unique ones.** Gemini names
  a function call but gives it no id, and it pairs a `functionResponse` back by
  name. Synthesizing `call_<name>` for both made two calls to one tool share an
  id, and Chat/Responses refused the translated turn with `Duplicate value for
  'tool_call_id' of call_read_file in message[2]`. Repeated names now gain a
  position suffix on both the call and its result, and the codec strips both
  the prefix and the suffix when it renders back into Gemini, because Gemini
  knows the call by name alone.
- **A Boolean `thinking` toggle is not a reasoning dimension.** It cannot carry
  `off`/`low`/`high`, so it must not make an Agent look depth-capable;
  counting it kept Kimi Code's selection carrying a level the Agent has no
  field for, and every dispatch was refused with
  `message_submission_runtime_binding_mismatch`.
- **A depth *category* is the reasoning dimension, whatever the option id is.**
  Kimi Code publishes `thinking` (`off`/`on`) in the `thought_level` category
  and ships no Registry alias. `extract_probe_reasoning_efforts` already read
  that category as reasoning effort and offered both levels to the composer,
  while `SessionConfigPlanner::option_key_for_option` resolved the option to
  its own id — so the level the catalogue offered could never be applied. The
  apply loop dropped the field, the durable selection kept it, and every
  dispatch was refused with `message_submission_runtime_binding_mismatch`.
  `thought_level` / `thinking_level` / `reasoning` categories now resolve to
  `reasoning_effort`, and a Boolean option stays generic because it cannot
  carry a level.
- **A wire name must be an identifier, not a label.** Antigravity validates
  `mcpServers[].name` against `^[a-zA-Z0-9_-]+$` and refuses the whole
  `session/new` with `-32602 Invalid params` when a display name such as
  "Agent delegation" arrives; Gemini CLI turns the same value into its
  `mcp_<server>_<tool>` function names, which its OpenAI-backed upstream then
  rejects for the same reason. The descriptor therefore keeps the label and
  puts a sanitized form on the wire, falling back to the stable id when the
  label has nothing usable in it.
- **A gateway projection has no ACP authentication method to select.**
  Antigravity advertises only `oauth-*`, `gemini-api-key` and
  `agent-platform`; calling `authenticate` on a gateway projection rewrites
  `settings.json` back from `{"auth":{"type":"gateway"}}`, and its `model`
  config option then answers `Model '<route id>' is not available for the
  current authentication method` because the API-key catalogue holds only
  Gemini ids. The launch skips the handshake when
  `AGY_ACP_ENABLE_GATEWAY_AUTH=true` is projected.
- **A protocol's tool-name charset is not shared.** Gemini allows `.` and `:`
  in a function name (its MCP bridge rewrites only `[^a-zA-Z0-9_\-.:]`),
  while OpenAI and Anthropic accept only `[a-zA-Z0-9_-]` and at most 64
  characters. A Gemini client therefore sends
  `mcp_com.browser-use_browser-use_browser_click`, and the upstream rejects the
  whole request (`Invalid 'tools[36].function.name': string does not match
  pattern`). The gateway rewrites the name into the target vocabulary and
  remembers the mapping, because the reply has to name the tool the client can
  actually call. The rewrite is deterministic (sorted, digest-suffixed on
  truncation) so a resumed conversation replays the same names and a retry does
  not miss the prompt cache; a passthrough is left alone because its reply is
  never re-encoded.
- **A backend registry is not the ACP picker's provider list.** ZCode's bridge
  lists user-added Providers from the legacy `config.json` provider map, but
  the backend registry a Model id is resolved against is built from the
  personal table (`<home>/.zcode/v2/provider_config.json`); since 3.12 the
  desktop writes it there and the legacy map stopped syncing. Projecting only
  `config.json` made the picker advertise the route model while the first
  prompt failed with `CONFIGURATION_ERROR Select a model before continuing` at
  `turnPhase: model_creation` — with the ACP journal still recording a
  successful `session/new`, because the session is created lazily. Both files
  are projected from one binding.
- **A vendor tool type the upstream cannot serve is dropped, not fatal.** A
  Responses client may offer built-ins (`web_search`, `web_search_preview`,
  `custom`, ...) that only OpenAI serves; forwarding one verbatim makes the
  upstream reject the whole turn (`[code=unsupported_tool]`), losing every tool
  call the Agent could actually make. A passthrough now keeps `function`-typed
  tools and drops the rest, removing `tools` entirely when none survive (an
  empty array is not the same as an absent key), and logs what it dropped.
- **Cline reaches a caller-supplied endpoint only through its provider store.**
  `openai-native` is Cline's own OpenAI client: it offers no base-URL config
  option and always posts to api.openai.com, so projecting it sent Vibex's
  placeholder key to OpenAI and failed with "Incorrect API key provided".
  Both OpenAI protocols therefore project `openai-compatible` and the
  `ClineProvidersJson` overlay writes `<CLINE_DATA_DIR>/settings/providers.json`
  with `provider`/`apiKey`/`model`/`baseUrl` plus `tokenSource: "manual"` and an
  `updatedAt` derived from the binding. Without those two fields Cline treats
  the entry as an untouched default: it still reads the base URL but sends the
  provider's built-in model (`gpt-4o`), so the gateway answers `model_not_found`.
  `CLINE_API_BASE_URL` is not read by the ACP bridge.

### 4. Contracts — failure detail

- **A refusal keeps the Agent's own words.** Every live configuration failure is
  reported as `runtime_switch_configuration_unavailable`, and the inner
  `causeCode` names a category (`acp_session_config_value_unavailable`), not a
  cause. The Agent's `rpcMessage` is redacted and bounded into the field
  outcome's `errorDetail`, carried on the switch error as `agentRefusal` +
  `configKey`, and appended to the operation journal's
  `error_detail_redacted`. Without it a rejected boolean read as an unexplained
  switch failure and the only way to learn why was to re-run the Agent by hand.

### 5. Tests Required

`a_gemini_model_is_read_from_the_path` (which also asserts the SSE framing),
`reads_and_writes_the_gemini_3_tool_schema`,
`cline_projection_pins_the_agent_endpoint_and_selects_the_provider_id`,
`a_cline_store_entry_carries_the_fields_that_make_it_authoritative`,
`schema_type_names_are_normalized_between_protocols`,
`a_validation_refusal_names_its_fields`,
`a_vendor_tool_type_the_upstream_cannot_serve_is_dropped`,
`dropping_every_tool_removes_the_key`,
`a_probe_without_a_reasoning_dimension_reports_none`,
`a_depth_category_is_the_reasoning_dimension`,
`acp_capabilities_state_the_protocol_floor`,
`a_route_group_resolves_its_launching_profile_for_capabilities`,
`a_reasoning_depth_suffix_resolves_to_the_base_model`,
`repeated_tool_names_get_distinct_call_ids`,
`a_function_response_under_the_model_role_is_a_tool_turn`,
`antigravity_style_history_pairs_every_call_with_its_result`,
`a_route_model_is_named_by_its_provider_catalogue`,
`a_gemini_tool_result_under_the_model_role_answers_its_call`,
`kimi_depth_category_is_read_as_reasoning_by_probe_and_planner`,
`mcp_server_wire_names_are_identifiers`,
`zcode_personal_provider_table_matches_the_registry_schema`,
`zcode_plan_projects_the_legacy_map_and_the_personal_table`,
`a_dotted_name_is_rewritten_and_restored`,
`a_tool_name_the_target_protocol_refuses_is_rewritten_and_restored`,
`a_rewritten_tool_name_comes_back_as_the_clients_own`,
`only_antigravity_gets_an_extra_overlay`,
`a_refused_option_keeps_the_agents_own_words` and
`a_field_refusal_reaches_the_operation_journal` for the detail path;
`a_boolean_option_is_sent_with_the_acp_type_tag`,
`a_reasoning_effort_the_agent_cannot_express_is_dropped`,
`an_option_update_that_reports_no_model_keeps_the_launch_model`, and
`antigravity_projects_its_auth_mode_into_the_state_home` — one per contract.
`session_runtime_config_applies_the_effort_its_model_switch_publishes` guards the
ordering: a deferred effort option must still be applied after the Model switch
that publishes it.

## Scenario: Kimi Code CLI Provider Projection

### 1. Scope / Trigger

- Trigger: starting or resuming Kimi Code CLI through ACP with a Vibex Model Provider Profile.
- Kimi 1.49 uses `KIMI_SHARE_DIR`, reads `<share>/config.toml`, and checks its persisted Kimi login token before `session/new`, even when an external API key is supplied.

### 2. Contracts

- Materialize an owner-private Kimi runtime root and set `KIMI_SHARE_DIR` to it. `KIMI_CODE_HOME` and the former `KIMI_MODEL_BASE_URL` / `KIMI_MODEL_API_KEY` variables are not Kimi 1.49 configuration boundaries.
- Write `config.toml` with one `vibex` provider and the selected model. Map OpenAI Chat Completions to `openai_legacy` and Anthropic Messages to `anthropic`.
- Materialize the selected Profile secret only inside the private config and a private `credentials/kimi-code.json` compatibility token. The compatibility token satisfies Kimi's pre-session auth gate; it does not become a global Agent login or escape the Profile runtime root.
- Enable ACP terminal tools and terminal authentication for Kimi presets so Shell calls and the advertised login method have a host implementation.
- Forward enabled MCP descriptors through `session/new.mcpServers`; Kimi 1.49 consumes ACP MCP descriptors directly.
- Kimi runtime switching is process-scoped: Profile, endpoint, credential, model, or wire-protocol changes require restart and resume.

### 3. Capability Notes

- Verified Kimi 1.49 ACP declarations: session `new/load/list/resume/prompt/cancel`, one `default` mode, model switching, image and embedded-resource prompts, HTTP MCP, streaming message/thought/tool/diff/plan updates, permission requests, ACP filesystem, and ACP terminal operations.
- `session/fork` is present in the server surface but returns not implemented. Form elicitation and `usage_update` are not emitted by Kimi 1.49. Reasoning effort is represented by Kimi's thinking model variants rather than a separate ACP effort option.

### 4. Tests Required

- Projection tests assert private `config.toml`, private compatibility credentials, selected endpoint/model/protocol, late Secret materialization, and `KIMI_SHARE_DIR`.
- ACP dialect tests assert Kimi forwards MCP descriptors and scrubs inherited Kimi/OpenAI credential overrides on Agent-account launches.
- Run the runtime rollout evidence gate after changing the catalog version or projection descriptor.

## Scenario: ZCode Managed Bridge And Private Provider Runtime

### 1. Scope / Trigger

- Trigger: starting ZCode through the managed `zcode-acp-server` Adapter with
  either ZCode's own account or a Vibex Model Provider Profile.

### 2. Contracts

- ZCode provider projection writes the selected provider, endpoint, credential,
  and model into a workspace-stable private `$HOME/.zcode/v2/config.json`; it
  never rewrites the user's native ZCode configuration.
- The projected ACP model id is `providerId\\modelId`. Mode and thought level
  remain dynamic ACP session options and use the exact values advertised by the
  Adapter.
- ZCode Desktop does not add its bundled CLI to `PATH`. If neither Agent nor
  Profile supplies `ZCODE_BIN`, runtime launch may discover only the documented
  platform bundle path when that file actually exists. Linux uses
  `/opt/ZCode/resources/glm/zcode.cjs`, macOS uses the ZCode application bundle,
  and Windows uses the per-user ZCode installation under `LOCALAPPDATA`.
- An explicit `ZCODE_BIN` environment value always wins. Missing bundle
  discovery must not overwrite the bridge's normal PATH-based fallback.
- `authenticate` may remain unsupported even though `initialize.authMethods`
  describes ZCode's Agent-managed credential source; Vibex Provider Profiles
  authenticate through the private projection instead.
- ZCode may wrap an upstream model failure as JSON-RPC `-32603` with the generic
  message `Internal error` while placing the actionable provider text in
  `error.data.details`. Preserve that bounded nested detail before redaction and
  classify balance, quota, or resource-package exhaustion as an actionable
  provider-access failure rather than a generic ACP protocol error.

### 3. Capability Notes

- Verified `zcode-acp-server@0.17.2` declarations: `session/new/load/list/resume/fork`,
  prompt/cancel, model/mode/thought configuration, streaming Agent text and
  reasoning, tools, plans, usage updates, permissions, form elicitation, MCP,
  Skills/slash commands, file/image context, and a ZCode filesystem extension.
- Vibex's generic ACP host supports standard filesystem and terminal callbacks,
  permission and elicitation loops, canonical timeline events, and usage
  updates. A declaration does not claim every callback occurred in one smoke.

### 4. Tests Required

- Projection tests assert private config shape, secret materialization, selected
  endpoint/model, and the `providerId\\modelId` runtime id.
- ACP runtime tests assert bundle discovery returns only an existing file and
  explicit environment configuration remains authoritative.
- ACP error tests assert nested `error.data.details` survives bounded
  normalization and that ZCode balance/resource-package failures return an
  actionable provider-access error with a recharge-or-switch recovery hint.
- Real verification must force first-prompt materialization; an initialize-only
  handshake cannot detect a missing ZCode CLI bundle or upstream account quota.

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

### Delivery paths differ, and only one of them is automatic

An Agent can only see a resource through a channel its own CLI implements. Vibex
has two, and they are not interchangeable:

| Resource | Automatic path | Explicit path |
| --- | --- | --- |
| MCP server | ACP `session/new.mcpServers`, for Agents whose dialect delivers wire entries | `ProviderNativeExportMode::Mcp` writes the Agent's own MCP config file |
| Skill | none — ACP has no Skills field | `ProviderNativeExportMode::Skills` writes `SKILL.md` into the Agent's Skills folder |

Two facts follow, and product surfaces must not contradict them:

- **MCP wire forwarding is gated per Agent.** `McpWireDelivery` in
  `crates/agent-acp/src/dialect.rs` classifies each Agent as `Delivered`,
  `NativeConfig`, `AcceptedButDropped`, or `Rejected`. Only `Delivered` Agents
  receive entries; for the others the runtime emits
  `acp_mcp_wire_forwarding_skipped` and sends an empty array. An Agent marked
  `NativeConfig` reads its own file at launch, so **native export is its only
  delivery path** — configuring MCP for it without exporting changes nothing.
- **A Skill is inert until it is exported.** Enabling a Skill for an Agent adds
  a `$command` composer entry and changes the session's `skills_revision`
  fingerprint (which restarts the Agent process so a stale session cannot keep
  an old Skill set). It does not put the Skill anywhere the Agent reads. Native
  export is what makes an Agent see it.

Native export therefore follows the same preview → diff → backup → atomic write
→ rollback requirements as Provider export, and adds three of its own:

- **Never re-serialize a file Vibex does not own.** JSON files are edited by
  replacing the byte span of the container value, preserving key order,
  indentation and unrelated subtrees. TOML and YAML files cannot be spliced
  safely without a format-preserving parser, so Vibex owns a marker-delimited
  block and **refuses** a file whose container already exists outside it rather
  than deleting servers the user configured by hand.
- **Export targets come from the import scanner.** The Agent home and Skills
  folder are resolved by `agent_native_home_roots` and
  `import_scan_agent_skill_roots`, the same functions the discovery flow uses,
  so everything Vibex writes is discoverable again. Export/import round trips
  are covered by tests.
- **MCP secrets are materialized; provider-profile secrets are not.** A native
  config file is read by the Agent's own process, which cannot reach Vibex's
  secret store, so `ProviderNativeExportMode::Mcp` resolves secret references
  and writes the values. A secret that fails to resolve is dropped with a
  diagnostic naming only its lookup key, never an empty placeholder that would
  look configured. Provider-profile export still writes public settings only.

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

SQLite owns (Skills table as of schema version 55, which added `body`):

```text
skills(
  skill_id, display_name, source_kind, status, scope_kind, project_id,
  workspace_id, source_uri, description, tags_json, content_preview, body,
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
- Rollback restores the Vibex-created backup recorded in export file
  operations. A file the export *created* has no backup, so it is removed —
  but only while it still holds exactly what Vibex wrote; once it has been
  edited it is left in place with a
  `provider_native_export_rollback_skipped` diagnostic. Rollback must never
  guess at unrelated user-managed files.
- Codex `config.toml` provider-profile export is conservative: ready only when
  the file is missing/empty or already contains a Vibex managed marker.
  Existing unmarked TOML must return blocked diagnostics. The guard is keyed on
  the provider-profile marker so the MCP export, which appends its own separate
  marker block, is still allowed to touch a user-managed file.
- Claude `settings.json` export owns only the top-level `vibex` field and
  preserves other JSON object fields.
- `ProviderNativeExportMode::Mcp` and `Skills` are implemented for the Agents in
  `crates/config-switch/src/native_surface.rs`, and `Combined` runs the profile,
  MCP and Skill plans together. `Prompts` stays blocked with a diagnostic
  because the composer expands a Prompt into the message it sends, so there is
  no native file to own.
- Resource modes target the Agent the selected profile runs. A concrete source
  that names a different Agent is refused with
  `provider_native_export_source_agent_mismatch`; `AgentDefault` always matches.
- An Agent with no native MCP file returns a blocked plan whose reason comes
  from `native_mcp_absent_reason`, never a silent no-op.
- MCP export reports every entry it could not write as
  `provider_native_export_entry_skipped`. TOML and YAML surfaces carry stdio
  servers only.
- Skill export refuses a Skill with no stored `body`
  (`provider_native_export_skill_body_missing`): writing the 2 KiB preview
  would ship truncated instructions.

### 4. Validation & Error Matrix

- Missing profile -> `provider_native_export_profile_not_found`.
- Missing persisted preview during apply -> `provider_native_export_preview_not_found`.
- Existing unmarked Codex config, provider-profile mode -> blocked file plan
  with `provider_native_export_blocked`.
- Target file already defines the container outside Vibex's block (TOML/YAML)
  -> blocked file plan with `provider_native_export_capability_refused`.
- Export source names a different Agent than the profile runs -> blocked file
  plan with `provider_native_export_source_agent_mismatch`.
- Unresolvable MCP secret -> entry dropped with
  `provider_native_export_secret_unresolved` naming only the lookup key.
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
- Bad: Preview writes a native file, overwrites unmarked Codex TOML in
  provider-profile mode, re-serializes a TOML/YAML file Vibex does not own,
  installs hooks, writes a Skill from its truncated preview, or writes an MCP
  secret as an empty placeholder.

### 6. Tests Required

- `cargo test -p vibex-db native_export` must assert export record/file
  operation persistence and list summaries.
- `cargo test -p vibex-config-switch native_export` must assert preview
  no-write behavior, redaction, blocked unsafe Codex target, apply backup +
  atomic write, failed write restore, and rollback restore.
- The same suite must assert the resource modes: an MCP native write is read
  back by `parse_mcp_candidates_for_path`, a TOML write keeps unrelated content
  and stays parseable, a Skill write lands beside its sibling files, a
  mismatched source and a body-less Skill are blocked, and rollback both removes
  a created file and leaves an edited one alone.
- `cargo test -p vibex-config-switch native_surface` must assert that JSON edits
  preserve every untouched byte, that a nested key with the container's name is
  not mistaken for it, and that text-format writes are idempotent and refuse a
  container the user already owns.
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
  `@agentclientprotocol/claude-agent-acp@0.71.0`; managed Codex uses
  `codex-acp` backed by `@agentclientprotocol/codex-acp@1.8.0`. The
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
Provider-backed SessionRuntimeSelection -> exact Agent/Provider Profile -> managed ACP route
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
  The same bootstrap scans enabled configured models for missing model-scoped
  snapshots without delaying runtime readiness.

### 2. Signatures

```rust
DesktopRuntime::start(config) -> Arc<DesktopRuntime>
DesktopRuntime::spawn_agent_bootstrap() -> VibexResult<()>
AgentInstallService::ensure_installed(agent_id).await
RuntimeOptionCatalogService::probe_agent(agent_id).await
RuntimeOptionCatalogService::probe_missing_enabled_profile_models().await
RuntimeOptionCatalogService::probe_profile_models(provider_profile_id).await
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
- Provider Profile save/delete consumers publish `ProfilesChanged` immediately.
  A saved ACP Profile then asynchronously probes only enabled models without a
  successful model-scoped snapshot; it never clears the Agent fallback or waits
  for the probe before the save completes. Successful probes publish a
  coalesced `RuntimeOptionsChanged`; failures do not block the mutation.
- Explicit detected-version refresh may reconcile legacy projection rows, but
  it publishes a profile event only when the effective compatibility rows or
  projection state changed. An idempotent refresh is silent; otherwise the
  `ProfilesChanged` consumer would start the same version probe again.
- After a Provider mutation, clients rebuild the Profile/model projection. A
  newly configured model uses the Agent's cached controls while its background
  probe runs; an unchanged model keeps its durable snapshot without another
  process launch.
- A model switch may publish its runtime options through
  `config_option_update` rather than the `session/set_model` response. The
  background probe waits for that bounded update before persisting the model
  snapshot; an explicitly empty model field remains authoritative over the
  Agent fallback.
- While a client fetches the updated Profile/model projection, it retains the
  last complete runtime catalog. A provisional catalog with empty Agent
  evidence is allowed only when no catalog exists yet; it must never replace
  visible reasoning, mode, or feature controls during an asynchronous refresh.

### 4. Validation & Error Matrix

- Runtime activation fails -> `DesktopRuntime::start` fails; do not spawn the
  background bootstrap.
- Runtime task ownership lock fails -> return
  `process/desktop_runtime_task_lock_failed` from startup.
- Managed install/reconciliation fails after readiness -> emit a bounded
  warning and keep the runtime available; do not fall through to an option
  probe.
- Provider save/delete succeeds -> publish one coalesced `ProfilesChanged`
  event and remote Provider invalidation immediately. A successful model probe
  may publish a later `RuntimeOptionsChanged`; a failed probe only records its
  error and leaves fallback controls available.
- Repeating an explicit detected-version refresh without a projection change
  -> publish no additional `ProfilesChanged` event.
- Agent has no successful runtime-option snapshot at startup -> probe it in the
  background and persist either the option snapshot or the failed-attempt
  record. A probe failure must not make the runtime unavailable.
- Agent has a successful snapshot while a Provider changes -> preserve and use
  it as fallback. A successful model snapshot takes precedence only for its
  exact `(provider_profile_id, model_id)` key.

### 5. Good/Base/Bad Cases

- Good: an existing managed installation is repaired in the background while
  the workbench opens; completed command reconciliation publishes Provider
  changes without starting an Agent option probe.
- Base: exact adapters are installed and an Agent has no snapshot; the
  workbench remains ready while the background probe persists options and
  refreshes the runtime catalog.
- Good: adding a Provider Profile updates models immediately, reuses the
  Agent's cached controls while the background model probe runs, then publishes
  model-specific options when the probe succeeds.
- Good: a repeated Config Center snapshot with the same detected CLI version
  leaves the Profile listener quiet and does not start another refresh cycle.
- Bad: `build_agent_manager` or `activate` awaits a managed download, an ACP
  probe, or complete catalog enrichment before reporting the runtime ready.
- Bad: a Profile save calls `probe_agent`, blocks on `session/new`, or reuses a
  successful snapshot for a different model id.
- Bad: a Profile event installs a catalog built from an empty evidence map over
  an existing enriched catalog, causing non-model controls to disappear.

### 6. Tests Required

- `vibex-desktop-runtime` asserts activation precedes background bootstrap and
  manager construction contains no adapter installation.
- Runtime source assertions require Agent and model missing-snapshot probing
  plus `RuntimeOptionsChanged` in the background bootstrap, while rejecting
  Agent refreshes in Provider mutation consumers.
- Provider mutation tests assert `ProfilesChanged` is published immediately;
  ACP model probes may produce a later `RuntimeOptionsChanged`.
- Config-switch tests assert the first detected-version projection change emits
  an event while the next identical refresh emits none.
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

## Scenario: Managed ACP Companion Compatibility

### 1. Scope / Trigger

- Trigger: a managed ACP Adapter can use either a separately installed companion
  runtime or a private runtime bundled by the Adapter itself.
- This contract applies to the verified installation, update-check, cache
  reuse, and startup-repair paths owned by `AgentInstallService`.

### 2. Contracts

- Install a separate companion only when the Adapter requires Vibex to provide
  its executable through an environment variable. Do not duplicate a runtime
  that the Adapter already bundles and selects as its documented fallback.
- `@openma/deepseek-harness-acp` standalone mode discovers external Harness
  installations first and then falls back to an exact private runtime archived
  inside the Adapter package. Vibex therefore installs only the Adapter; it does
  not separately resolve the large `@deepseek-ai/dsh` dependency graph.
- The DeepSeek launch path must leave `DSH_PATH` unset when no explicit external
  Harness command was configured so the Adapter can select its private runtime.
  Installation manifest validation must likewise not require a separate `dsh`
  executable or companion version for this Adapter-bundled runtime.
- Adapters that still need a Vibex-managed companion record its version in the
  installation manifest and validate the companion launcher before cache reuse.

### 3. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| DeepSeek standalone install is requested | Install only `@openma/deepseek-harness-acp`; let its launcher select the bundled private runtime. |
| DeepSeek manifest has no companion version or managed `.bin/dsh` | Accept it when the Adapter entry point and selected Node runtime are valid; those fields belong only to separately managed companions. |
| Vibex adds `@deepseek-ai/dsh` to the DeepSeek managed package set | Reject in review: the duplicated graph makes installation network-heavy and can exceed the bounded npm timeout. |
| A separately managed companion manifest omits or changes `runtime_version` | Installation is unusable and startup repair is required. |
| Adapter and required companion launcher are present | Reuse the installation without an unnecessary companion network probe. |

### 4. Tests Required

- Installation policy tests assert DeepSeek has no separately managed npm
  companion while Agents that require one retain their normal policy.
- Manifest loading tests accept DeepSeek with no `runtime_version` and no
  managed `.bin/dsh`, while retaining those checks for real companions.
- Launcher tests preserve DeepSeek's `DSH_PATH` integration for explicit
  external Harness overrides without requiring it for managed installation.

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
AgentProviderRolloutManifestEntry::supports_model_provider_configuration()
  -> bool
model_provider_configurable_agent_ids() -> BTreeSet<AgentId>
ProviderProfileRepository::list()
  -> profiles owned by model-provider-configurable Agents
ProviderProfileRepository::list_all()
  -> every ACP-compatible runtime profile
ProviderConfigService::list_runtime_profiles()
  -> ProviderProfileRepository::list_all()
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
- The rollout manifest is the only Agent-level authority for model-provider
  configuration visibility. An entry supports the surface only when its
  capability mode is `ReplaceableProvider` and its evidence/switch contract is
  not `Unverified`. Database and GPUI code must consume
  `model_provider_configurable_agent_ids()` instead of repeating Agent ids.
- `ProviderProfileRepository::list` returns local defaults and profiles whose
  Agent belongs to that capability-derived set. `list_by_agent` remains able to
  read internal ACP runtime profiles for Agent-managed, cloud-credential,
  local-model, marketplace, and conservative Agents; hiding a model-provider
  editor must not delete or make those runtime records unreadable.
- Runtime Option Catalog construction uses `list_runtime_profiles` (backed by
  `ProviderProfileRepository::list_all`) rather than the Config Center list, so
  Agent-owned ACP profiles continue to produce session options even when their
  model-provider editor is intentionally hidden.
- For an added Agent outside the capability-derived set, Config Center keeps
  the installation and Agent-owned authentication surfaces but omits the
  provider count, model-provider card, profile rows, import action, and create
  action. A supported Agent keeps the card even when it currently has zero
  profiles so the user can create the first one.
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
  `codex-acp@1.8.0`, typed ACP configuration publishes `read-only`, `agent`, and
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
- `AgentProviderCapabilityMode != ReplaceableProvider`, unverified projection
  evidence, or unverified switching -> omit the Agent from model-provider
  configuration lists and controls; retain its internal per-Agent profiles.
- Rollout manifest validation failure while deriving the configuration set ->
  propagate the structured manifest error; do not fall back to a stale static
  Agent whitelist.
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
- Good: GLM, Copilot, Qwen Code, and other Agents with documented typed
  projectors appear in the model-provider list without adding their ids to SQL
  or GPUI constants.
- Base: Gemini uses its Agent-owned cloud credential flow; Config Center shows
  installation/authentication but no model-provider card, while its seeded ACP
  runtime profile remains available through `list_by_agent`.
- Good: Google Antigravity 1.0.0 accepts a Profile-scoped `GEMINI_API_KEY` and
  `GOOGLE_GEMINI_BASE_URL`; it appends `/v1beta/models/...` to the configured
  origin and selects the Profile model through its advertised ACP `model`
  config option. Vibex therefore gives each Antigravity Profile a private
  `GEMINI_HOME`, strips a trailing `/v1` or `/v1beta` from the endpoint before
  launch, explicitly selects the advertised `gemini-api-key` authentication
  method after `initialize`, enables ACP terminal callbacks for shell tools,
  and initially maps a Provider-facing model such as `gemini-3.7-flash` to
  Antigravity's concrete default `gemini-3.7-flash-high` ACP variant. Vibex
  exposes `low`, `medium`, and `high` as a synthetic reasoning-effort option;
  choosing one replaces an existing thinking suffix or appends it to the
  Provider model before sending Antigravity's `model` config option. Provider
  model runtime-option probing uses the same authenticated launch and records
  this synthetic option alongside the Agent's advertised modes. It uses
  restart-and-resume switching rather than relying on the ignored
  `GEMINI_MODEL` process variable. Antigravity advertises session listing at
  `sessionCapabilities.list`; the runtime accepts that standard shape as well
  as the legacy top-level `listSessions` boolean.
- Bad: a newly supported typed projector persists successfully but disappears
  after refresh because `ProviderProfileRepository::list` still contains only
  `claude`, `codex`, and `opencode`.
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
- `cargo test -p vibex-core model_provider_configuration_support_follows_the_rollout_contract --locked`
  asserts built-in/typed support and managed/cloud/local/conservative exclusion.
- `cargo test -p vibex-db agent` covers config/discovery persistence and latest
  discovery selection.
- `cargo test -p vibex-db provider_profile_list_uses_model_provider_capability_whitelist --locked`
  asserts a typed catalog Agent is globally visible while an unsupported
  Agent's internal profile remains readable only through its Agent-scoped path.
- `cargo test -p vibex-desktop-runtime runtime_catalog_keeps_agent_owned_profiles_visible --locked`
  asserts runtime option catalog construction retains an unsupported Agent's
  ACP profile through the full runtime listing path.
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
- `cargo test -p vibex-desktop agent_settings_show_model_provider_configuration_only_when_supported --locked`
  asserts capability gating precedes profile rendering and preserves the
  installation/authentication surfaces.
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

const MODEL_PROVIDER_AGENTS: &[&str] = &["claude", "codex", "opencode"];
if snapshot.added {
    render_model_provider_configuration();
}
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

let configurable_agents = model_provider_configurable_agent_ids()?;
if configurable_agents.contains(&agent.id) {
    render_model_provider_configuration();
} else {
    render_agent_installation_and_authentication();
}
```

Backend session creation owns durable enforcement; frontend catalog filtering
is only a convenience. `ProviderKind` remains valid in configuration/provenance
APIs but does not participate in the online route above.

## Scenario: ACP Registry Managed Agent Installation Lifecycle

### 1. Scope / Trigger

- Trigger: the user clicks Add, Upgrade, Roll back to the Vibex-verified
  version, or Uninstall for an ACP Agent whose `AgentDefinition` maps to either
  the verified ACP Registry or a documented Vibex-managed latest CLI channel.
  The right-hand Config Center panel is the user-visible operation surface.
- Agents without a verified Registry distribution or an explicit managed
  latest channel remain external-CLI Agents; Vibex does not download or
  replace their user-installed command.

### 2. Signatures

```text
AgentInstallService::install(agent_id).await
AgentInstallService::rollback(agent_id).await
AgentInstallService::check_update(agent_id).await
AgentInstallService::uninstall(agent_id).await
AgentInstallService::bootstrap_agent_ids() -> Vec<AgentId>
ConfigCenter.agent_mutations[agent_id] -> ManagementMutation
AgentInstallService operation keys:
  agent:<agent_id>
  shared:registry
  shared:runtime:<node|uv>
  shared:download:<artifact_identity>
AgentNodeRuntimeOptions { node_path, npm_path }
AgentUvRuntimeOptions { uv_path }
AgentManagedInstallationRecord {
  agent_id, registry_agent_id, state, command, install_root, updated_at_ms
}
```

### 3. Contracts

- Config Center Add starts the managed installation. The panel shows a
  disabled loading state until the verified command is published; only then
  does the UI set `added = true`/`enabled = true`, refresh runtime capability
  data, and discover authentication methods for the user to choose.
- Agent operations are pending per Agent id in both Config Center and
  `AgentInstallService`. Install, update check, uninstall, Add/remove, toggle,
  and probe for one Agent disable only that Agent's controls; another Agent may
  start and finish an operation concurrently. A second operation for the same
  Agent remains serialized so its durable install/config/auth transitions
  cannot overlap.
- Registry refresh, managed Node/uv publication, and verified download-cache
  publication use separate shared resource keys. Operations for unrelated
  Agents must not share one global lock, while operations that resolve the same
  shared resource must coalesce or serialize before replacing its cache entry.
  Completed UI operations reload the authoritative snapshot; generation fencing
  makes the newest reload win when several Agent operations finish close
  together.
- The upstream Registry response is cached for one hour. Binary archives must
  carry a SHA-256. npm distributions must use an exact version, metadata
  integrity, and the canonical `registry.npmjs.org` tarball URL; the lockfile
  `resolved` URL must match that same canonical artifact.
- Registry `uvx` distributions must use an exact package version in either
  `package==version` or `package@version` form. The package name is a plain
  PyPI identity with optional validated extras, the version must be exact
  SemVer, and it must equal the resolved entry version; ranges, URLs, VCS
  references, and local paths are rejected before `uv` runs.
- Roll back installs the Adapter version Vibex pins in its own catalog for that
  Agent, and is the only install target allowed to move an installation
  backwards. The pin is Vibex's compatibility statement, so a user asking for
  it is not the stale-candidate case the channel target rejects; scoping
  `reject_semver_downgrade` to the channel target is what keeps both rules true
  at once.
- Offer the rollback only where it can actually complete: a usable managed
  installation, a resolved npm or uvx distribution, and a catalog pin that
  differs from the installed version. A binary archive names one release and
  carries no version to swap, and an Agent whose catalog entry is `manual` has
  no pin at all, so neither may show the action. Decide this on the
  platform-resolved distribution, not the entry, because an Agent may publish
  binaries for other platforms and npm for this one. Derive the offer from the
  installation state and the catalog rather than storing it, so a snapshot can
  never advertise a version the catalog has since moved past.
- Move the distribution's exact spec and the entry version together when
  pinning, because the parser, the install fingerprint, and the version the
  record reports all read both. Rewriting only one leaves an entry the
  installer rejects as `agent_npm_spec_invalid`/`agent_uvx_spec_not_exact`.
  Keep the package identity intact: a leading npm scope `@`, a uvx extras
  suffix such as `hermes-agent[acp]`, and whichever uvx separator the Registry
  used all survive the version swap.
- A rollback is not a hold. The Agent keeps tracking its own release channel
  afterwards, so the next update check may report a newer version again; that
  is the user's decision to make, not a state Vibex pins them into.
- A rollback re-probes runtime options after refreshing the snapshot, because
  the Adapter version decides which model catalogue the Agent advertises and
  the options the newer version reported no longer apply.
- Pi is a managed npm bundle: Vibex installs the Registry-pinned `pi-acp`
  Adapter and resolves the latest published `@earendil-works/pi-coding-agent`
  runtime before installing both exact versions in the same isolated tree.
  Both direct packages independently pass metadata, integrity, canonical
  tarball, executable, and lockfile identity checks. The generated launcher
  sets `PI_ACP_PI_COMMAND` to that tree's `.bin/pi` command; it never depends
  on or replaces a user-global Pi installation.
- Four additional Agent CLIs use their official latest published release when
  Add, Upgrade, or Check for updates resolves an installation: CodeWhale uses
  npm `codewhale/latest`; Hermes uses PyPI `hermes-agent` with the `[acp]`
  extra; Kiro uses the stable latest CLI manifest; and Amp uses npm
  `@ampcode/cli/latest`. The resolved release is converted to an exact version
  before installation and recorded in `runtimeVersion`; no floating package
  range reaches npm or uv.
- CodeWhale and Hermes expose ACP from their CLI (`codewhale serve --acp` and
  `hermes acp`). Kiro launches `kiro-cli acp`. Kiro
  archives and their SHA-256 values come from the official manifest; Linux
  installs into a staged private home, macOS retains the private app bundle
  from the DMG, and Windows retains the MSI administrative-extract tree.
  Unsupported manifest platforms fail closed.
- Amp keeps its ACP Adapter and latest CLI in the same isolated npm tree. Its
  generated launcher sets `AMP_CLI_PATH` to that tree's `.bin/amp` command
  before importing the Adapter. This managed latest-channel CLI never probes,
  prefers, replaces, or
  depend on a user-global Agent CLI from `PATH`; only Node/npm and uv toolchain
  discovery may reuse verified user/system executables.
- Minion Code installs the exact `agent-client-protocol==0.8.1` compatibility
  dependency beside its exact Agent package. The install manifest records this
  dependency version, and startup repair, explicit update checks, and
  same-version install cache hits all require that exact manifest entry. A
  legacy Minion manifest without it is `UpdateAvailable` and must be rebuilt
  even when both Agent versions are `0.1.44`.
- Generated Node launchers encode display labels as JavaScript values rather
  than interpolating them inside string literals. Tests syntax-check every
  launcher shape with `node --check` so a label cannot make an otherwise
  verified installation fail before ACP initialize.
- "Latest" is resolved only during Add, explicit Upgrade/install, or Check for
  updates. Desktop startup restores a healthy recorded installation without a
  network lookup or silent upgrade. Check for updates compares both the
  Adapter version, any separately recorded Pi or Amp CLI runtime
  version, and any Agent-scoped exact compatibility dependency recorded in the
  install manifest.
- npm Agents select Node/npm in this order: explicit
  `VIBEX_AGENT_NODE_PATH`/`VIBEX_AGENT_NPM_PATH` configuration, system
  `node`/`npm` from `PATH`, then a downloaded Vibex-managed Node.js 22 runtime.
  Explicit and system candidates must be real files, `node --version` must be
  SemVer 22.0.0 or newer (22.19.0 or newer for Pi), and both version probes must
  succeed; rejection falls through to the next candidate without activating a
  partial installation.
- `uvx` Agents select `uv` in this order: explicit `VIBEX_AGENT_UV_PATH`, the
  system `uv` from `PATH`, then a Vibex-managed archive downloaded from the
  official Astral release endpoint. Every candidate must be a real file and
  pass `uv --version` with SemVer 0.5.0 or newer. Invalid explicit/system
  candidates are logged and skipped before managed fallback.
- Managed `uv` archives fetch the matching `.sha256` sidecar and verify the
  archive digest before extraction. Verified releases are cached side by side
  under the Vibex runtime root by detected version and platform; corrupt cache
  entries are discarded and rebuilt.
- `uvx` package installs use a Vibex-owned `UV_CACHE_DIR` and managed Python
  install directory. User/project `UV_*`, `PIP_*`, `PYTHONPATH`, and virtual
  environment settings are removed; the install uses the PyPI default index,
  disables keyring providers, and copies packages instead of linking them.
  Each package is installed into a Python 3.12 relocatable venv inside the
  staged Agent version. Vibex verifies installed metadata has the exact
  Registry version, every declared compatibility dependency has its exact
  version, and the console entry point is unambiguous. It then launches through
  a metadata-based Python launcher so staging publication does not leave
  absolute paths behind.
- npm always uses an isolated Vibex cache and blank user/global npm config,
  regardless of runtime source. The user and global config paths must be
  distinct: npm rejects one file assigned to both configuration levels as a
  duplicate load before package resolution begins. The install root is
  content-addressed by the selected runtime identity, publishes through
  staging, keeps healthy versions side by side, and only prunes old versions
  after the new command is durable. Archive paths, extraction budgets, and
  executable paths are bounded and traversal-safe.
- A usable installed SemVer may not be replaced by a lower Registry or latest
  channel SemVer.
  Exact-version cache hits are idempotent only when the install manifest also
  matches every Agent-scoped runtime compatibility dependency; an invalid or
  legacy cache entry is removed and rebuilt. Pending install/upgrade/uninstall
  rows are reconciled on the next startup using install-root, command-file, and
  manifest compatibility checks, not only a version field.
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

- Missing Registry/latest-channel mapping ->
  `capability/agent_managed_install_unavailable`; Vibex falls back to the
  external CLI path.
- Registry parse, unsupported platform, missing checksum, invalid archive,
  canonical npm source, integrity, lockfile, or executable mismatch -> a
  structured validation/capability error; no partial install is activated.
- Invalid npm/PyPI latest metadata, a Kiro manifest/download version mismatch,
  a missing trusted package setup script, or a missing private companion CLI
  fails closed without probing a user-global Agent command.
- Download or extraction timeout -> bounded process error and a persisted
  `Failed`/`UpdateAvailable` state suitable for retry.
- Registry downgrade candidate on the channel target ->
  `conflict/agent_install_downgrade_rejected`. The verified-version target is
  exempt by design; it is the one path whose whole purpose is to go backwards.
- Rollback for an Agent whose catalog entry is `manual` ->
  `capability/agent_verified_version_unavailable`.
- Rollback for an Agent whose platform distribution is a binary archive or the
  Kiro manifest -> `capability/agent_verified_version_unsupported`. Neither
  carries a version to swap, so no partial or mislabelled install is written.
- Missing, unexecutable, malformed, pre-22 Node/npm, or pre-22.19 Node/npm for
  Pi -> reject that candidate and continue through system then managed runtime
  fallback.
- Missing, unexecutable, malformed, or pre-0.5 `uv` -> reject that candidate
  and continue through system then managed runtime fallback. Missing or
  non-exact `uvx` package specs, installed-version mismatches, and ambiguous
  console entry points fail closed without activating a partial install.
- Missing or mismatched Agent-scoped runtime dependency in an otherwise usable
  same-version install -> `UpdateAvailable` and rebuild; do not report it as
  current based on the Agent version alone.
- Identical `npm_config_userconfig` and `npm_config_globalconfig` paths -> an
  invalid internal command configuration; construct distinct blank files and
  do not rely on npm to accept the duplicate load.
- Interrupted operation or unusable command/root -> recovery marks the row
  failed and does not re-enable a missing process.
- Uninstall failure -> preserve the old usable command and configuration when
  possible; never leave an enabled Agent pointing at deleted files.
- A second operation for the same Agent while one is pending -> keep the first
  task and show/retain the same-Agent pending state; do not replace its task
  handle. An operation for a different Agent -> admit it immediately unless it
  waits on the exact same Registry/runtime/download resource key.

### 5. Good/Base/Bad Cases

- Good: clicking Add shows Downloading, verifies the package, enables the
  Agent, detects its authentication methods, and leaves provider profiles and
  native Agent homes untouched.
- Good: Upgrade publishes a new side-by-side version, records the actual
  version in runtime identity, and keeps the old version available until the
  new command is active.
- Base: opening Config Center reads the cached installation state without
  downloading; an explicit Check for updates refreshes the Registry and any
  applicable official latest CLI channel.
- Good: an Agent installed ahead of Vibex's pin offers Roll back with the
  verified version beside the installed one; rolling back installs that exact
  version, the offer disappears because the pin now equals what is installed,
  and the next update check may legitimately report a newer version again.
- Base: an Agent installed at the pinned version, one distributed only as a
  binary archive, and one with a `manual` catalog entry show no Roll back
  action at all.
- Good: removing an Agent deletes its command, installation row, and auth
  catalog; a later startup does not reinstall it unless the user adds it again.
- Good: Codex can continue downloading while Claude is toggled or checked; each
  row shows only its own loading/disabled state, and both completions reconcile
  through fresh authoritative snapshots.
- Base: two actions targeting Codex are serialized, and two installs that need
  the same managed Node archive serialize only the shared runtime/cache step.
- Bad: activate an Agent before checksum/lock verification, use npm's global
  state, assign one blank npm config file to both user and global levels,
  accept an unprobed or pre-22 Node/npm candidate, silently downgrade, accept
  an unprobed or pre-0.5 `uv`, install a ranged/URL `uvx` package, or restore
  an old exact compatibility workaround for a newer binary.
- Bad: store one global Config Center Agent mutation or hold one global install
  mutex across every Agent operation; either makes unrelated Agent controls and
  work wait behind a long download.

### 6. Tests Required

- `cargo test -p vibex-desktop-runtime agent_install` covers cache repair,
  canonical npm sources, lockfile identity, checksum/archive limits, SemVer
  downgrade rejection, explicit/system/managed Node selection and fallback,
  malformed/old Node rejection, distinct empty user/global npm configs,
  Pi's dual-package lock, latest runtime companion, and local launcher,
  Amp latest companion binding, Kiro manifest platform selection, generated
  Node launcher syntax, Minion's exact ACP runtime dependency and legacy
  same-version manifest repair,
  explicit/system/managed `uv` selection and fallback, exact `uvx` and Hermes
  `[acp]` extra parsing, metadata entry-point validation, relocatable Pythoncache recovery, interrupted recovery, and uninstall cleanup.
- `cargo test -p vibex-core agent_config` covers the rollback offer: an npm or
  uvx installation on a version other than the pin offers it, the pinned
  version itself, a binary distribution, an unpinned Agent, and a
  not-installed or external state do not.
- `cargo test -p vibex-desktop-runtime repinned_specs` and
  `pinning_to_the_verified_version` cover the version swap: a leading npm
  scope, a uvx extras suffix, both uvx separators, the unchanged channel
  target, and `agent_verified_version_unavailable` for an unpinned Agent.
  The repinned spec must still parse against its own entry version.
- `cargo test -p vibex-config-switch agent` covers removal of runtime and auth
  snapshots plus managed command/version matching.
- `cargo test -p vibex-agent-acp runtime` covers dynamic managed identities,
  conservative event/pool/restore/evidence behavior, and exact external
  descriptor compatibility.
- Desktop management tests cover Add loading, upgrade loading, rollback
  loading, failed-install detail rendering, and Agent registry refresh events.
- `cargo test -p vibex-desktop agent_mutations_are_keyed_by_their_target_agent
  --locked` asserts every concurrent Agent mutation maps to its target id and
  unrelated ids remain independently actionable.
- `cargo test -p vibex-desktop-runtime
  agent_operation_locks_only_serialize_the_same_agent --locked` holds one Agent
  guard, asserts another Agent acquires immediately, then asserts the queued
  same-Agent guard resumes only after release.

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

#### Wrong

```rust
self.mutation = Some(ManagementMutation::AgentInstall(agent_id));
let _guard = self.global_install_lock.lock().await;
```

One long Agent download disables every Agent row and queues unrelated work.

#### Correct

```rust
self.agent_mutations.insert(agent_id.clone(), mutation);
let _guard = self.acquire_operation(format!("agent:{agent_id}")).await;
```

The target Agent stays internally serialized while other Agent ids retain their
own UI state and operation lock. Shared artifacts use their own resource keys.

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
  `@agentclientprotocol/claude-agent-acp@0.71.0` with bin
  `claude-agent-acp`; Codex `@agentclientprotocol/codex-acp@1.8.0` with bin
  `codex-acp`.
- Codex compatibility identity must include the exact resolved
  `@openai/codex` runtime package from the managed install lock tree, for
  example `adapter=codex-acp@1.8.0;runtime=@openai/codex@0.152.1`.
- `codex-acp@1.8.0` declares `@openai/codex ^0.152.0`, and the managed pin
  `0.152.1` satisfies that range, so the descriptor no longer needs an npm
  override. Registry validation rejects an override while the pin is inside
  the declared requirement (`acp_registry_runtime_dependency_override_unnecessary`);
  the flag returns only if a future pin falls outside the adapter's declared
  range.
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

- Good: Claude `0.71.0` installs in the managed root, verifies the lock/bin
  metadata and npm origin, passes health probe and the real contract, then
  produces activation evidence for identity
  `adapter=claude-agent-acp@0.71.0`.
- Good: Codex `1.8.0` resolves `@openai/codex@0.152.1`; changing that runtime
  version changes compatibility identity and invalidates stale evidence.
- Base: `session/set_model` or `session/fork` is `not_advertised`; the contract
  report records a skip only when the descriptor marks it `when_advertised`.
- Bad: using `codex-acp@1.8.0` alone as the compatibility identity while
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

### 4. Validation & Error Matrix

- Duplicate local mutation key -> `management_mutation_in_progress`.
- Expected graph revision differs from the database ->
  `automation_graph_version_conflict` with bounded expected/actual revisions.
- Self, missing, or duplicate graph edge -> typed graph validation issue before
  the repository call.
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
- Codex `0.152.1` accepts only `openai_responses`. Claude exposes no selectable
  Wire API. PATH-launched OpenCode uses its detected CLI version and may expose
  only the adapter/protocol pairs registered for the matching semantic-version
  descriptor. The OpenCode inline-provider contract currently supports
  `>=1.17.9, <2.0.0`; it was exercised against local OpenCode `1.18.11`.
- A descriptor's `model_interfaces` must list exactly the wire protocols the
  installed Agent was observed to speak, because that list is the only thing
  that makes a protocol selectable and it also gates binding validation. The
  observed API-key-expressible surface is:
  `claude` = Anthropic Messages only (its Bedrock and Vertex routes authenticate
  through AWS SigV4 / Google ADC, which `ModelProviderProfile` secrets cannot
  carry, so they must stay unlisted rather than selectable-but-unusable);
  `codex` = Responses only; `opencode` = Responses, Chat Completions, Anthropic
  Messages, Google Generative AI, Google Vertex, Bedrock Converse;
  `gemini` and `antigravity` = Google Generative AI plus Google Vertex;
  `kimi` = Chat Completions, Responses, Anthropic Messages, Google Generative AI;
  `pi` = those four plus Google Vertex and Bedrock Converse; `copilot` = Chat
  Completions, Responses, Anthropic Messages; `grok` = Chat Completions,
  Responses, Anthropic Messages; `hermes` = Chat Completions, Responses,
  Anthropic Messages, Bedrock Converse; `deepseek-harness` = Chat Completions,
  Responses, Anthropic Messages; `zcode` = Chat Completions, Anthropic Messages;
  `codebuddy-code` = Chat Completions; `cline` = Chat Completions, Responses.
- Cline CLI selects its provider, wire protocol, and Model catalogue through one
  provider id, and it rejects a caller-supplied base URL for every id except its
  OpenAI-shaped pair (`openai-compatible` = Chat Completions,
  `openai-native` = Responses). Vibex therefore derives the id from the Model's
  wire protocol instead of offering the id list, and projects
  `CLINE_PROVIDER` / `CLINE_API_KEY` / `CLINE_MODEL` alongside
  `<CLINE_DATA_DIR>/settings/providers.json`.
- Cline owns that catalogue: the selectable ids are whatever the installed
  bridge advertises for the chosen provider id, and an id outside it is silently
  replaced rather than rejected. Discovery therefore launches the Agent with
  `agent_owns_model_catalog` + `discover_runtime_model_catalog` instead of
  calling a Provider model-list endpoint, and the provider form renders the
  result as a closed choice list. The probe passes a placeholder key because the
  bridge only requires that some key is present; no Provider is contacted and no
  real Secret is read.
- Google Vertex is a distinct wire protocol rather than a `google_generative_ai`
  endpoint, because the Agents that speak both read a different origin variable
  per protocol (`GOOGLE_GEMINI_BASE_URL` vs `GOOGLE_VERTEX_BASE_URL`) and the
  SDK appends a different route (`/v1beta/models/...` vs
  `/v1beta1/publishers/google/models/...`). Writing the first-party variable for
  a Vertex Model leaves the Agent silently on the wrong route.
- An Agent that selects its protocol through an environment switch rather than
  the endpoint must have that switch projected from the selected Model:
  Copilot CLI's `COPILOT_PROVIDER_TYPE` / `COPILOT_PROVIDER_WIRE_API`, grok's
  per-model `api_backend`, Kimi Code CLI's `providers.<id>.type`, and pi's
  `models.json` `providers.<id>.api`. Projecting only the base URL and key
  leaves each of them on its own default protocol.
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
- The OpenCode inline-provider overlay also injects one Vibex-managed primary
  agent, `vibex-auto`, because `opencode acp` advertises every primary agent as
  an ACP session mode. That is the only config-only route to an auto-accept
  choice in Vibex's mode selector, since `opencode acp` rejects the TUI's
  `--auto` flag. The agent allows `edit`, `bash`, `webfetch`, `websearch`, and
  `external_directory`. `external_directory` must be listed explicitly because
  it defaults to `ask`, and omitting it makes every workspace-external read — a
  user config file, a sibling checkout — still prompt inside an "auto" mode.
  `read` and `doom_loop` are deliberately left unset so the built-in `.env`
  rules and the repeated-tool-call guard keep asking. OpenCode merges config
  sources, so the agent is added alongside — never in place of — the user's own
  agents. Agent rules outrank global rules, so selecting the mode overrides the
  user's `ask`/`deny` rules for the listed permissions, which is a deliberate
  divergence from `--auto`'s respect for explicit denies.
- Product state uses `provider_model_id` as the canonical model identity. The
  projection engine is the only owner of configured product-model to
  Agent-runtime-model translation. For OpenCode it qualifies the model with
  the generated provider id selected by that model's Wire API; Responses,
  Chat Completions, Anthropic Messages, Google Generative AI, and AWS Bedrock
  Converse therefore use their exact `provider/model` runtime namespaces.
- The legacy `ProviderProfile` compatibility mirror maps a Claude product model
  with a standalone `default`, `opus`, `sonnet`, or `haiku` family token to that
  Claude ACP model alias while preserving the full `provider_model_id` as the
  product identity. Other Claude model ids pass through unchanged. The
  idempotent startup backfill repairs older mirrored rows that incorrectly used
  the full Provider model id as the Claude Agent model id.
- An OpenCode model with no explicit Wire API inherits the legacy `wireApi`
  option when present. Otherwise, exactly one non-empty protocol-specific URL
  override defines the inherited protocol; zero or multiple overrides retain
  the backwards-compatible Responses default. Startup backfill repairs the
  mirrored Wire protocol and SDK adapter, so an Anthropic-only profile cannot
  launch as `@ai-sdk/openai` or select the wrong OpenCode provider namespace.
- OpenCode protocol projection must translate Vibex endpoint semantics into the
  selected AI SDK adapter's base-URL semantics. `@ai-sdk/anthropic` expects the
  versioned API root and appends `/messages`; a provider root therefore projects
  with `/v1`, an existing `/v1` remains unchanged, and a full `/v1/messages`
  endpoint is reduced to `/v1`. Other Wire APIs retain their own endpoint form.
  Passing a bare Anthropic provider root through unchanged can return an HTML
  website from `/messages`, which OpenCode may report as a zero-token empty turn.
- OpenCode model qualification is idempotent. A historical or imported
  `agent_model_id` that already starts with the generated provider id is used
  once, never expanded to `provider/provider/model` in either the overlay or
  ACP requests.
- Each ACP process snapshots the exact configured product/runtime model-id
  pairs from the same projection used to launch it. Outbound ACP model fields
  use the runtime id; inbound session creation, discovery, config updates, and
  mutation responses are normalized back to the product id before comparison,
  event publication, or SQLite persistence. Unknown ids pass through unchanged
  instead of being heuristically stripped.
- A runtime id that maps to multiple configured product ids is ambiguous and
  fails process preparation with `acp_model_id_projection_ambiguous`; the
  runtime must not guess which durable identity to store.
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
- Good: an Anthropic-compatible Provider root such as `https://gateway.example`
  projects to OpenCode as `baseURL = https://gateway.example/v1`, so the AI SDK
  sends the model request to `/v1/messages`.
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
  revision CAS, legacy id preservation, Claude legacy model alias repair,
  OpenCode inherited-protocol repair, and one Provider bound to two runtimes.
- Config-switch tests assert deterministic env/JSON/TOML/YAML projection,
  private permissions, path/symlink rejection, late Secret resolution,
  redacted `Debug`/preview, selective stale propagation, and omission of an
  absent OpenCode model display name without losing a configured name. They
  also assert exact OpenCode runtime ids for all five Wire APIs and idempotent
  handling of already-qualified model ids. Anthropic cases assert root, existing
  `/v1`, nested root, and full `/v1/messages` inputs all produce the exact AI SDK
  API root.
- ACP tests assert Claude/Codex/OpenCode parity, prepare-failure fencing, and
  Profile-save stale marking without process termination. The OpenCode
  regression must exercise a qualified runtime catalog, send the qualified id
  through `session/set_model`, normalize responses back to the product id, and
  prove the durable runtime config remains converged.
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

## Scenario: Minimum-Version Catalog Agent Runtime Projection

### 1. Scope / Trigger

- Trigger: Config Center creates or updates an ACP Provider Profile for one of
  these 17 catalog Agents: `copilot`, `codewhale`, `crow-cli`,
  `deepseek-harness`, `dirac`, `factory-droid`, `goose`, `grok`, `hermes`,
  `kilo`, `kimi`, `mistral-vibe`, `poolside`, `pi`, `qwen-code`, `stakpak`, or
  `vtcode`.
- Trigger: an explicit Config Center refresh changes the detected Agent
  version, or a session/probe must start with the selected Profile's endpoint,
  API key, model, and Wire API.
- This path crosses catalog identity, legacy Profile compatibility records,
  typed projection, late Secret resolution, private overlay materialization,
  ACP process fingerprints, and restart-and-resume switching.

### 2. Signatures

```text
ProviderConfigService::refresh_detected_agent_versions() -> VibexResult<usize>
ProviderConfigService::refresh_agent_snapshot(AgentRefreshSnapshotRequest)
  -> VibexResult<AgentRefreshSnapshotResponse>
ProviderConfigService::sync_legacy_projection(connection, profile)
  -> VibexResult<()>

AgentProviderProjectionEngine::plan(provider, runtime, binding, descriptor, workspace_key)
  -> VibexResult<AgentProviderProjectionPlan>
AgentProviderProjectionEngine::non_secret_environment_for_plan(plan, runtime_root, workspace_key)
  -> VibexResult<BTreeMap<String, String>>
AgentProviderProjectionEngine::process_args_for_plan(plan, runtime_root, workspace_key)
  -> VibexResult<Vec<String>>
AgentProviderProjectionEngine::resolve_and_materialize(plan, runtime_root, workspace_key)
  -> VibexResult<ResolvedAgentProviderProjection>

AgentProviderProjectionPlan {
  non_secret_env,
  secret_env: ProjectionSecretEnvReference[],
  overlay_files: ManagedProjectionOverlay[],
  runtime_home_env_key?,
  process_args, // `{projectionRoot}` templates
  effective_model,
  switch_behavior,
  fingerprint
}
ResolvedAgentProviderProjection {
  non_secret_env,
  secret_env,
  overlay_root,
  overlay_files,
  process_args, // materialized absolute private paths
  effective_model,
  fingerprint
}
```

### 3. Contracts

- The 17 descriptors accept their researched compatibility floors and later
  semantic versions: Copilot `>=1.0.78`, CodeWhale `>=0.8.55`, crow-cli
  `>=0.1.23`, DeepSeek Harness ACP `>=0.4.32`, Dirac `>=0.4.1`, Factory Droid
  `>=0.153.1`, Goose `>=1.33.1`,
  Grok `>=1.0.8`, Hermes `>=0.19.0`, Kilo
  `>=7.2.40`, Kimi `>=1.49.0`, Mistral Vibe `>=2.9.3`, Poolside `>=1.0.0`,
  Pi `>=0.0.33`, Qwen Code `>=0.18.4`, Stakpak `>=0.3.80`, and VT Code
  `>=0.96.14`. Older, missing, manual, or non-semantic versions never inherit
  these schemas.
- The catalog version is the pin — the release Vibex verifies, installs, and
  rolls back to — and the compatibility floor is a separate statement. They
  are the same value unless the entry declares `compatible_version`, or, for a
  builtin, unless the projection gates on its own version-requirement constant.
  Bumping the pin alone must not raise the floor: an already-installed runtime
  below the pin but at or above the floor keeps the typed projection instead of
  collapsing to the conservative surface with
  `agent_projection_version_mismatch`. Two kinds of Agent sit above their floor:
  - Agents whose newer release changed a wire detail, where Vibex answers the
    older spelling with a read-back shim, or whose newer release added a
    capability that only the newer runtime can serve. DeepSeek Harness is the
    reference: the pin is `0.4.38` while the floor stays `0.4.32`. `0.4.33`
    introduced the `route::model` qualification, which Vibex answers with a
    read-back alias; `0.4.35` moved the bundled Harness runtime to `0.1.7`,
    whose route registration Vibex answers by projecting the cordis home patch
    beside `settings.yaml`; and `0.4.36`/`0.4.37` moved it to `0.2.0`, whose
    new protocols and negotiated fork surface are gated per release
    (`DEEPSEEK_HARNESS_MISTRAL_WIRE_SINCE`,
    `DEEPSEEK_HARNESS_MODEL_SUBSTITUTION_SINCE`,
    `DEEPSEEK_HARNESS_SESSION_FORK_SINCE`) rather than by raising the floor.
    An installed `0.4.32` keeps the typed projection: the newer capability is
    withheld, the release is not.
  - Agents whose pin moved forward on a review that found no change to the
    projection contract: CodeBuddy `2.168.2` over `>=2.109.0`, Copilot `1.0.98`
    over `>=1.0.78`, Gemini `0.64.0` over `>=0.47.0`, Grok `1.0.62` over
    `>=1.0.8`, Pi `0.0.34` over `>=0.0.33`, Cline `3.0.73` over `3.0.29`,
    OpenCode `1.19.3` over `>=1.17.9`, Antigravity `1.4.0` over `1.0.0`, and
    Claude Code, whose adapter pin is `0.93.0` while
    `CLAUDE_CONFIG_ALIAS_VERSION_REQUIREMENT` stays `>=0.71.0` because the alias
    contract resolves the same `model` values in both releases. The older
    release is still projected, so the floor stays where the contract was last
    verified.
  - Cline's floor is the one entry that must be declared explicitly. Its
    `ReplaceableProvider` shape would otherwise derive `>=3.0.73` from the pin,
    turning every not-yet-upgraded runtime into the conservative surface with
    `agent_projection_version_mismatch`. The 3.0.49+ model-catalogue rule is
    answered at runtime instead: `CLINE_MODEL` stays only the process-start
    hint, the ACP `model` config option remains the authority, and the
    attachment now reports a substituted `currentModelId` as a warning once.
  - Codex, whose identity is composite. `codex-acp` moves to `2.2.2` and its
    managed runtime to `@openai/codex` `0.160.1`, which is what 2.2.2 declares
    (`^0.160.1`) rather than the `^0.159.1` its predecessor declared. `^0.160.1`
    is a 0.x caret range, so it stops below `0.161.0`; the newest Codex release
    (`0.163.0`) is outside the Adapter's own declaration and npm would install
    a `0.160.x` build anyway. The quirk, the config-option aliases and the
    event enricher are keyed on the exact identity
    `adapter=codex-acp@2.2.2;runtime=@openai/codex@0.160.1`, while
    `CODEX_COMPATIBLE_ADAPTER_VERSION_REQUIREMENT` stays `>=1.8.0`: a runtime
    that has not upgraded keeps the typed projection but stops inheriting the
    alias table, because that table is deliberately exact-identity only. Both
    versions live in `crates/agent-acp/src/registry.rs` and the identity string
    is derived from them rather than spelled out at a call site.
  - ZCode moves to `0.72.0` and its exact alias gate moves with it
    (`=0.72.0`). The mode catalogue gained `guarded` and stopped advertising
    `auto`, the bridge dual-emits turn state under `$/zcode/` and `_zcode/`,
    and `embeddedContext` turned on; the config option ids
    (`model`/`mode`/`thought`) did not move. `ZCODE_CREDENTIAL_SECRET` joins the
    scrub list because 0.68.1 added an encrypted credential store keyed by it.
  - DeepSeek Harness moves to `0.4.38` on the same bundled runtime
    (`0.2.0-rc.2`). The release makes the packaged runtime the only default and
    turns an explicit `DSH_PATH` into a semver-gated override that is *refused*
    rather than falling back, so the managed launch must leave `DSH_PATH`
    unset; its auth-method list is also now derived from the client's declared
    capabilities and has to be re-read per `initialize`.
  - Hermes stays at `0.19.0` because that is the last release on PyPI. Upstream
    ships 0.22.x only as GitHub source tags with no wheel or sdist, so the uvx
    channel cannot reach it; the pin is explicit instead of a "latest" lookup
    that would silently keep returning 0.19.0. Moving to 0.22 needs a VCS or
    installer distribution and is a separate change.
  - Pi's *runtime* is pinned rather than floated. `pi-acp@0.0.34` requires pi
    `>=0.81.0`, and the 1.x line has already changed provider wire defaults, so
    the managed companion installs `@earendil-works/pi-coding-agent@1.3.0`
    instead of resolving npm `latest` on every check.
  - Cursor's version string is compared by its release triple. The Registry
    names `2026.10.14` while the artifact is `2026.10.14-941492e`, and `semver`
    parses that suffix as a pre-release that sorts *below* the base — so a real
    build would look older than itself and either be rejected as a downgrade or
    never seen as an update. A trailing hex build id is read as metadata.
- A pin stays behind its Agent's newest release when the release changes a
  contract Vibex has to implement, not when it only changes a version string.
  The held pins and their reasons:
  - Cline — from 3.0.49 `resolveDefaultModelId(provider, CLINE_MODEL,
    catalogue)` replaces an id outside the selected provider's catalogue with
    that provider's default instead of using it verbatim. This still needs no
    pre-validation, because the catalogue only exists inside a running Agent.
    `CLINE_MODEL` is the process-start hint; the ACP `model` config option is
    the authority, and a substitution is now surfaced as one warning rather
    than silently running a different Model.
  - Cursor — the 2026.10.14 build has a resolvable artifact and a documented
    ACP extension surface (`cursor/ask_question`, `cursor/create_plan`,
    `subagents`), even though Cursor publishes no note for that exact build.
    Both blocking methods are bridged onto the elicitation surface: replying
    `method-not-found` fails Cursor's tool call instead of degrading.
  - Kimi, which is not a pin bump but a move to a different product. Moonshot
    archived the Python `kimi-cli` line — 1.51.0 marked it end-of-life and
    1.52.0 turned its entry point into a deprecation gate that prints a
    migration notice and exits 0 without ever speaking ACP — and the successor
    is the npm package `@moonshot-ai/kimi-code`. The launch shape survives
    (`kimi acp`) and so does the `config.toml` provider/model table, but the
    state root moves from `KIMI_SHARE_DIR`/`~/.kimi` to
    `KIMI_CODE_HOME`/`~/.kimi-code`, and two provider types are respelled:
    `openai_legacy` becomes `openai` and `google_genai` becomes `google-genai`.
    The successor drops an entry whose type is outside its accepted set rather
    than translating it, so the old spelling silently loses the provider. No
    compatibility floor is declared, deliberately: the projection now writes the
    successor's vocabulary, and the archived CLI cannot read it, so advertising
    the floor would promise a credential and model surface that does not work.
    The managed install resolves the successor's npm channel directly, because
    the ACP Registry entry named `kimi` still points at the archived line.
  - DeepSeek Harness did move, and the move was in the bundled runtime rather
    than the adapter's own reader. Reading the bundles suggested 0.4.35 turned
    `settings.yaml` into a one-shot import renamed on read; running the
    published adapter disproved that — an invalid `settings.yaml` still makes it
    refuse to start with a `YAMLParseError`, and a full `initialize` neither
    renames the file nor writes a `profiles/` tree. But the runtime moved from
    `0.1.5` to `0.1.7`, which removed `@deepseek-ai/dsh-settings-file`: the
    `acp` route a Vibex projection writes into `settings.yaml` is no longer
    registered, so the adapter's `effortCatalog` cannot resolve
    `acp::<model>`, drops the `effort` ACP control, and a remembered
    `reasoningEffort: max` then resolves as `reasoning_effort_unavailable`. The
    adapter composes `$DSH_HOME/cordis.patch.yml` into its cordis entry list in
    both runtime versions, so the projection writes the route there as well.
    What also moved — Messages-only, a smaller default catalogue — applies to
    the official `deepseek-official` route, which a projected route does not
    use. `0.4.36` then moved the runtime to `0.2.0`: token deltas left the
    durable log for the `agent/assistant-stream` live channel, an unavailable
    Model started being substituted instead of sent verbatim, the question tool
    started arriving as form elicitation, and `mistral-conversations` joined the
    pi-ai compat gate. `0.4.37` added `session/fork` with the inclusive
    `jetbrains.air.fork` cut. Each of those is answered per release rather than
    by the projection floor, which stays on `0.4.32` because the `route::model`
    read-back qualification it covers is byte-identical through `0.4.38`.
  - Antigravity moved to `1.4.0` on Registry metadata alone: Google publishes
    no changelog for the ACP server, but the Registry's daily protocol probes
    report an identical ACP surface at 1.0.0, 1.1.1 and 1.2.1, and 1.4.0 keeps
    the same entry point, the same Linux `--uid=` argument and the same two
    archive members. The compatibility floor stays on 1.0.0 because the
    handshake, not the archive, is what the floor describes.
- Do not build a client for Gemini CLI's failure taxonomy. `MAX_TOKENS_EXCEEDED`,
  `SAFETY_BLOCKED`, `RECITATION_BLOCKED`, `OTHER_BLOCKED` and
  `THINKING_ONLY_RESPONSE` are internal stream-error classifications inside the
  CLI: they decide only that the turn ends gracefully instead of throwing, and
  the host receives `stopReason: end_turn` with a `_meta.quota` block carrying
  token counts. Nothing on the wire names the reason, so a `stopReason`-shaped
  decoder for them would never fire. The same applies to `NO_RESPONSE_TEXT`,
  `NO_FINISH_REASON`, `MALFORMED_FUNCTION_CALL` and `UNEXPECTED_TOOL_CALL`.
- Do not write a `subagentId` decoder for Copilot CLI. Its changelog says ACP
  clients "receive subagent IDs", but there is no ACP-level field: the string
  appears nowhere in the adapter's ACP region, and the official ACP schema has
  no such member. The identity reaches a client only through the raw-event
  passthrough `github.com/copilot/sessionEvent`, as that event's `agentId` or
  its deprecated `data.parentToolCallId`, and neither the notification's params
  shape nor a way to subscribe to it could be found — six plausible subscribe
  methods all answered `-32601`. Treat the capability as unavailable to a plain
  ACP client until that passthrough is documented.
- Streamed terminal output has two channels with opposite semantics, and a host
  that treats them alike corrupts the transcript. `_meta.terminal_output.data`
  is a delta: append it, never compare it against the previous text. The
  non-streaming fallback `content[0].content.text` is a full replacement
  rewritten on every frame, so it must replace. At settlement `rawOutput` still
  carries the whole output even when `terminal_output` carries only the tail,
  and a truncated stream makes that tail the entire text again — which is why
  `merge_tool_call_update` prefers `rawOutput` for the delta and treats the
  accumulated chunks as a fallback. Feeding the chunks through the
  prefix-comparison delta as if each were a growing total would report every
  chunk after the first as a replacement and drop what came before it.
- CodeBuddy's `multitask` switch needs no dedicated code, and adding some would
  duplicate a path that already works. It is an ordinary advertised
  config option — `{ "type": "boolean", "id": "multitask", "category":
  "_codebuddy.ai/multitask" }` — so `config_option_kind` reads it as
  `Boolean`, the runtime catalogue surfaces it as a Toggle, and
  `runtime_config_wire_value` sends a JSON boolean through the raw
  `session/set_config_option` envelope. That envelope exists precisely so a
  boolean is not coerced into the select builder's string. The id is not in
  `STRUCTURAL_KEYS`, so it is not filtered out of the catalogue either. The one
  thing never to do is pass `--multitask` or `--agent multitask` on the command
  line: the entry guard rejects the process outright when either meets `--acp`.
- `session/steer` is the second way to steer a turn, and the dialect table is
  what makes it safe to offer. It is neither ACP core nor namespaced with a
  leading underscore, so it travels the extension channel while looking like a
  core method, and no capability flag announces it. `session/steering` (the
  `_session/steering` extension) is tried first when negotiated; otherwise the
  bare method is sent **only** for an Agent the dialect table records as
  implementing it, because sending it to an Agent that does not turns an
  immediate local rejection into a round trip that waits out the prompt timeout
  on an adapter that accepts the request and never answers. The response still
  decides: `{"steered":true}` is `Injected`, `{"steered":false}` with any
  `reason` (`idle`, `stale`) is `PromptRequired`, and `-32601` maps back to
  `acp_steering_unsupported`.
- Cline delivers a generated image as an ordinary `agent_message_chunk` whose
  `content` is `{ "type": "image", "data": <base64>, "mimeType": ... }` — not a
  tool call and not a tool result — and falls back to a plain text placeholder
  `[Generated <modality>: <mediaType>]` when the media is not a base64 image.
  The text reader returns nothing for such a block, so the chunk used to vanish
  without a trace. It now emits an `ImageGeneration` event naming the media type,
  which is the most the host can honestly say about it: displaying the pixels
  needs somewhere to put them, and the two obvious places do not work.
  `generated_image_preview_source` resolves a reference through
  `generated_image_local_source`, which requires the path to start with the
  session's workspace root, so a private-root path renders nothing; and
  `raw_extension` truncates its blocks, so an inline `data:` URI cannot ride
  along either. Writing generated media into the user's workspace would work and
  is deliberately not done — it is a product decision about side effects in
  someone's repository, not a parsing one. Whoever makes that call should know
  the wire shape is already handled and only the destination is missing.
- DeepSeek Harness holds its model selection in `$DSH_HOME/acp-standalone-model.json`
  (`{"provider":"acp","model":"...","reasoningEffort":"..."}`), and reads
  `settings.yaml` only as a fallback for the model plus `permission.defaultPreset`.
  The `llm-pi-ai` provider route is consumed by the cordis entry list, which the
  adapter composes from its bundle patches plus the optional
  `$DSH_HOME/cordis.patch.yml` home patch; `settings.yaml` supplied that route
  only through `0.1.5`'s `dsh-settings-file`, which `0.1.7` removed. Settle the
  layer by running the Agent, not by reading bundles: with the route only in
  `settings.yaml`, 0.4.35 discovery logs
  `resolveModelInfo(acp::<model>) failed: no adapter registered for provider
  "acp"` and drops the `effort` option, while adding the same route as a
  `cordis.patch.yml` home patch restores `acp` and `off`/`high`/`max` in both
  0.4.33 and 0.4.35. The Adapter's `--help` also names
  `--provider`/`DSH_PROVIDER`, `--model`/`DSH_MODEL`, `--models`/`DSH_ACP_MODELS`,
  `--permission-mode`, and `--reasoning-effort`.
- Settle a question about a config file by running the Agent, not by reading its
  bundle: a parse error is a probe that costs one command. Corrupting
  `settings.yaml` and starting the published adapter produced an immediate
  `YAMLParseError` refusal, which proves the file is read every launch, while
  inspecting the same file after a full `initialize` showed it was neither
  renamed nor consumed. Both facts contradicted a careful reading of the
  shipped JavaScript. Reading is not consuming, though: 0.1.7 still parses the
  file for the standalone default and permission preset while deriving llm
  routes from the cordis home patch, so a `YAMLParseError` probe cannot prove
  that the route inside it is used. The adapter also documents its real projection surface in
  `--help` — `--provider`/`DSH_PROVIDER`, `--model`/`DSH_MODEL`,
  `--models`/`DSH_ACP_MODELS`, `--permission-mode`, `--reasoning-effort` — which
  is a faster route to a contract than any amount of bundle archaeology.
- Explicit refresh may run `<binary> --version` only for these trusted binary
  names: `copilot`, `codewhale`, `crow-cli`, `goose`, `grok`, `hermes`, `kilo`,
  `kimi`, `vibe-acp`, `pool`, `stakpak`, and `vtcode`. Dirac, Factory Droid,
  Pi, and Qwen Code derive the catalog version only when the
  complete `npx`/`uvx` command and argument vector exactly match the pinned
  catalog entry. Ordinary catalog reads remain process-free.
- The four environment projectors use these process contracts:

| Agent | Endpoint / Secret / Model | Additional selection or state |
| --- | --- | --- |
| Copilot | `COPILOT_PROVIDER_BASE_URL` / `COPILOT_PROVIDER_API_KEY` / `COPILOT_MODEL` | `COPILOT_HOME` points at the private projection root. |
| CodeWhale | `CODEWHALE_BASE_URL` / `OPENAI_API_KEY` / `CODEWHALE_MODEL` | `CODEWHALE_PROVIDER=openai`; `CODEWHALE_HOME` is private. |
| DeepSeek Harness | `DEEPSEEK_BASE_URL` / `DEEPSEEK_API_KEY` / `DSH_MODEL` | The ACP bridge documents all three launch variables; `DSH_HOME` keeps credentials and sessions in Vibex-owned state. The same Secret is also projected under the route-derived `<ROUTE_ID>_API_KEY` name the prompt-time gate reads. |
| Poolside | `POOLSIDE_STANDALONE_BASE_URL` / `POOLSIDE_API_KEY` / `POOLSIDE_STANDALONE_MODEL` | A trailing `/v1` is removed before launch. |

- The other 13 use code-owned typed overlays; serializers, relative paths,
  Secret environment keys, and selection controls are fixed in code:

| Agent | Overlay and Secret reference | Private selection boundary |
| --- | --- | --- |
| crow-cli | `config.yaml`, `${VIBEX_CROW_API_KEY}` | Append `--config-dir <projectionRoot>` after `acp`. |
| Dirac | `data/globalState.json`, child `OPENAI_API_KEY` | `DIRAC_DIR=<projectionRoot>`. |
| Factory Droid | `settings.json`, `${VIBEX_FACTORY_DROID_API_KEY}` | `FACTORY_HOME_OVERRIDE=<projectionRoot>`. |
| Goose | `config/custom_providers/vibex.json`, `VIBEX_GOOSE_API_KEY` by name | `GOOSE_PATH_ROOT`, `GOOSE_PROVIDER`, and `GOOSE_MODEL` select the injected entry. |
| Grok | `config.toml`, `env_key=VIBEX_GROK_API_KEY` | `GROK_HOME`; `[models].default` selects the injected model. |
| Hermes | `config.yaml`, `key_env=VIBEX_HERMES_API_KEY` | `HERMES_HOME=<projectionRoot>`. |
| Kilo | `kilo.json`, `{env:VIBEX_KILO_API_KEY}` | The same JSON is supplied through `KILO_CONFIG_CONTENT`. |
| Kimi | `config.toml` plus `credentials/kimi-code.json`, late-bound `VIBEX_KIMI_API_KEY` | `KIMI_SHARE_DIR=<projectionRoot>`; the private compatibility credential satisfies Kimi's pre-session login gate. |
| Mistral Vibe | `config.toml`, `api_key_env_var=VIBEX_MISTRAL_VIBE_API_KEY` | `VIBE_HOME=<projectionRoot>`. |
| Pi | `models.json`, `apiKey=$VIBEX_PI_API_KEY` | `PI_CODING_AGENT_DIR=<projectionRoot>`. |
| Qwen Code | `settings.json`, `envKey=VIBEX_QWEN_CODE_API_KEY` | `QWEN_HOME=<projectionRoot>`. |
| Stakpak | `stakpak.toml`, child `OPENAI_API_KEY` | Insert `--profile vibex --config <projectionRoot>/stakpak.toml` before root `acp`. |
| VT Code | `vtcode.toml`, `api_key_env=VIBEX_VTCODE_API_KEY` | `VTCODE_CONFIG_PATH` is the materialized file path. |

- Planned overlay content may contain only an environment-key reference, never
  the resolved API key. Secret lookup occurs once immediately before launch.
  Most Agents receive the value only in the child environment; formats that
  require an inline credential, including Kimi's private TOML and compatibility
  credential JSON, receive it only during owner-private materialization.
  Snapshots and fingerprints retain opaque Secret-reference versions, not values.
- Overlay roots are deterministic per binding and workspace, owner-only, and
  validated before atomic writes. Environment-only and inline projectors never
  write the user's Agent home. Agent-specific home variables and launch
  arguments point only at this root.
- Planning, process acquisition, and spawn use one projection identity.
  Materialized non-Secret env and process args enter the process snapshot;
  those exact args/env launch the child. Runtime Probe uses the same resolved
  child environment and argument-order helper as a real session.
- A Config Center version refresh re-synchronizes every non-local ACP Profile
  for that Agent before notifying Profile listeners. Profile endpoint, Secret
  reference, model, Wire API, overlay, home-key, or process-argument changes
  alter the fingerprint and mark the old process stale.
- All 17 descriptors use `RestartAndResume`. A save does not mutate or kill the
  current process in place; the switch coordinator prepares the new projection,
  restores the logical session when supported, commits the new binding, and
  then drains the old runtime.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Agent version is absent, manual, non-semantic, or below the catalog minimum | Conservative capability with `agent_projection_version_untrusted` or `agent_projection_version_mismatch`; inject nothing. |
| PATH binary basename is not the Agent's trusted name | Record `agent_version_probe_command_untrusted`; do not execute it or enable the projector. |
| Pinned `npx`/`uvx` command or any argument differs | Do not infer the catalog version; automatic projection remains closed. |
| Model Wire API is not in the descriptor's interface list | `agent_model_interface_unsupported`; preserve the current binding/process. |
| Required Secret reference or resolved value is missing | `agent_projection_secret_reference_missing` or `agent_projection_secret_missing`; do not spawn the target. |
| Legacy ACP Profile has no compatibility binding | `agent_projection_legacy_binding_missing`; retain the ordinary ACP Profile path without pretending projection succeeded. |
| Workspace key, overlay path, directory, or symlink is unsafe | The matching `agent_projection_workspace_key_invalid`, `agent_projection_overlay_path_unsafe`, `agent_projection_runtime_directory_invalid`, or `agent_projection_overlay_symlink_rejected` error. |
| Process snapshot fingerprint differs after a Profile/version refresh | Mark the process stale; new attachment returns `acp_process_config_stale` until restart-and-resume converges. |
| Crow or Stakpak projection args are already present | Keep one ordered copy; never duplicate config/profile flags. |

### 5. Good / Base / Bad Cases

- Good: Profile A and Profile B configure different endpoints, credentials, and
  models for Goose. Switching to B creates a new private overlay, sets
  `GOOSE_PROVIDER`/`GOOSE_MODEL` to B's generated entry, resumes the logical
  session on the new process, and leaves A's Secret out of B's child.
- Good: Grok registers a custom model and sets `[models].default` to that model;
  Crow and Stakpak probes receive the same materialized config arguments as
  real sessions.
- Base: Copilot or Poolside needs no config file. The process
  receives only its typed endpoint/key/model environment plus safe runtime
  metadata.
- Bad: apply a `1.33.1` Goose overlay to Goose `1.32.9`, a missing version, or
  a non-semantic version, or treat successful ACP `initialize` as proof that
  the endpoint was selected.
- Bad: register a Goose custom provider without selecting it, register a Grok
  custom model without `[models].default`, write an API key into an overlay, or
  materialize one set of args for the fingerprint and spawn another set.

### 6. Tests Required

- Core `catalog_projection_contracts_are_explicit_and_conservative` asserts all
  17 ids, catalog-derived `>=` semantic-version compatibility, typed
  provider/model boundaries, `VibexPrivate`, and `RestartAndResume`; the five
  blocked Agents remain conservative.
- DB `pinned_catalog_commands_supply_exact_versions_only_for_full_command_matches`
  asserts the four fixed-command identities and fail-closed mismatches.
- Config-switch
  `all_typed_catalog_projectors_map_provider_env_secret_model_and_private_state`
  parses every JSON/TOML/YAML overlay, checks all 17 endpoint/key/model
  projections, Goose/Grok selection, home paths, Crow/Stakpak args, fingerprint
  drift, and Secret redaction. It asserts the full projected Secret environment
  name list per Agent, so the DeepSeek Harness case must show both
  `DEEPSEEK_API_KEY` and the fixture's route-derived `MATRIX_PROVIDER_API_KEY`
  sharing one credential. Version-probe tests assert the 12 trusted system
  binary names.
- Config-switch
  `deepseek_harness_projects_the_route_derived_credential_name_beside_the_declared_one`
  pins the wrapper's name transform (including separator-run collapsing) and
  asserts the overlay keeps declaring `apiKeyEnv: DEEPSEEK_API_KEY` while the
  plan's Secret environment carries both names against one credential.
- ACP `projection_process_args_preserve_stakpak_root_argument_order` and
  `probe_process_args_use_materialized_probe_overlay_paths` assert real/probe
  argument parity, ordering, and deduplication. Process snapshot tests assert
  resolved Secret values never enter snapshots or diagnostics.
- Run `cargo fmt --all --check`, tests for `vibex-core`, `vibex-db`,
  `vibex-config-switch`, and `vibex-agent-acp`, scoped Clippy with
  `-D warnings`, `cargo check -p vibex-desktop --locked`, and
  `git diff --check` before commit.

### 7. Wrong vs Correct

#### Wrong

```rust
let snapshot = process_spawn_config_snapshot(profile, base_agent_args)?;
let projection = resolve_projection(profile)?;
spawn(base_agent_args, projection.child_environment())?;
```

This fingerprints the base command but launches a different process, and it
omits Crow/Stakpak config arguments from process acquisition.

#### Correct

```rust
let (key, materialized) = process_launch_materialization(
    profile_id,
    config,
    workspace,
    runtime_resources,
)?;
spawn_with_snapshot(
    key,
    materialized.snapshot.args.clone(),
    materialized.env_overlays,
)?;
```

One materialization supplies the process key, snapshot, ordered arguments, and
child environment. Profile switching therefore compares and launches the same
effective runtime configuration.

## Scenario: Agent Provider Runtime Verification Rollout

### 1. Scope / Trigger

- Trigger: adding or upgrading a catalog Agent descriptor, running an ACP
  provider probe, or deciding whether an existing binding may switch provider.
- The rollout manifest contains exactly four builtin and 33 catalog Agent ids.
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
  CodeBuddy `2.160.0` projects only
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
- The checked evidence capture includes all 34 entries, hashes every bound
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

- Good: a Goose `>=1.33.1` runtime receives only its documented projection,
  the isolated probe confirms the planned safe origin plus exact model, and
  evidence remains scoped to the matching descriptor and runtime version.
- Good: an exact Cline descriptor without a typed projector remains
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

- Core tests cover exact builtin and semantic-range catalog 34-entry
  catalog/descriptor/manifest identity, per-entry conservative diagnostics,
  and the two explicit environment descriptors.
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

## Scenario: Hermes ACP Provider Projection And Event Normalization

### 1. Scope / Trigger

- Trigger: creating or resuming a Hermes `0.19.x` ACP session from a Vibex
  Provider Profile that uses a managed `HermesYaml` overlay.
- This contract covers the model-id boundary and the conservative translation of
  Hermes tool updates into Vibex timeline events.

### 2. Signatures

```text
projected_runtime_model_id(provider, descriptor, configured_model) -> String
normalize_agent_event(AgentEventEnricherKind::Hermes, input)
  -> Vec<CanonicalAgentEvent>
```

The overlay writes a custom Provider entry to Hermes `config.yaml` and selects
that Provider/model pair through `model.provider` and `model.default`.

### 3. Contracts

- Vibex keeps the product model id unqualified. Hermes exposes models from a
  custom Provider over ACP as `custom:<model>`; every outbound session/model
  selection therefore uses that qualified id, and the reverse projection maps
  it back to the product id.
- The projection is idempotent: an already qualified `custom:<model>` must not
  become `custom:custom:<model>`.
- Hermes ACP tool updates use generic kinds, so the event enricher may recover
  only the verified title forms: `read:`, `write:`, `patch`, `search:`,
  `terminal:`, `python:`, `web search:`, and `todo`. Terminal/Python titles may
  supply a command when `rawInput` is absent; diff blocks become file
  operations. Unknown or ambiguous calls remain generic ToolCall events.
- Standard ACP message, reasoning, plan, permission, elicitation, terminal,
  filesystem, configuration, and usage updates remain on the shared runtime
  paths; the Hermes enricher must not reinterpret them.
- Credentials are resolved only while materializing the private overlay. Model
  projections, previews, Debug output, and event summaries remain secret-free.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Product model is `gpt-x` | ACP/runtime model is `custom:gpt-x`. |
| Product model is already `custom:gpt-x` | Keep exactly `custom:gpt-x`. |
| Hermes title is unknown or lacks a verified prefix | Emit a generic ToolCall. |
| Hermes diff contains a bounded file path | Emit a FileOperation with the inferred operation. |
| Secret lookup or endpoint projection fails | Return the existing typed projection error; do not spawn. |

### 5. Good/Base/Bad Cases

- Good: the overlay selects `custom:configured-model`, the ACP session starts,
  and a terminal title becomes a bounded command event while reasoning and plan
  updates remain visible.
- Base: a provider emits a new generic tool title; Vibex keeps the raw bounded
  ToolCall until a versioned Hermes descriptor proves a new mapping.
- Bad: send the unqualified product id to Hermes, double-prefix a qualified id,
  or classify every generic `execute` update as a shell command.

### 6. Tests Required

- Projection tests assert unqualified and prequalified Hermes model ids both
  resolve to one `custom:<model>` runtime id and reverse-map correctly.
- Overlay tests assert endpoint/model/provider fields and late secret
  materialization remain deterministic and redacted.
- Event tests cover terminal, write/diff, and read titles plus the generic
  fallback for an ambiguous title.
- The Hermes ACP smoke must exercise session creation and prompt streaming with
  the projected model before claiming the Provider binding is usable.

### 7. Wrong vs Correct

#### Wrong

```rust
session_new.model = configured_model.agent_model_id.clone();
```

#### Correct

```rust
session_new.model = projected_runtime_model_id(provider, descriptor, model);
```

The ACP-facing id belongs to Hermes' custom-provider namespace; the product id
must remain stable for Provider/Profile selectors and persisted bindings.

## Scenario: Source-Aware ACP Launch Projection

### 1. Scope / Trigger

- Trigger: an ACP process is started for authentication, model discovery, or a
  real session and the requested runtime source is either a Provider Profile or
  an Agent default account.
- Read this contract with the Agent session and runtime-switch source-revision
  contracts. Provider projection is a source-specific operation, not a generic
  Agent launch default.

### 2. Signatures

```text
RuntimeAuthSource = ProviderProfile { provider_profile_id }
                 | AgentAccount { auth_context_id }

AgentProviderProjectionEngine::plan/resolve_and_materialize(
  ProviderProfile source, agent/version/workspace
) -> ResolvedAgentProviderProjection

AcpRuntimeClient::process_spawn_config_snapshot_for_agent_account(
  agent_id, auth_context_id, context_revision
) -> redacted spawn snapshot

AgentAuthContextCapabilities {
  credential_env_keys_to_unset[],
  supports_logout,
  supports_direct_model_catalog,
  auth_verification_strategy
}
```

### 3. Contracts

- Provider Profile launches resolve secret references late, apply the
  versioned projection descriptor, materialize the Provider-scoped runtime
  home/overlay, and include projection identity in the spawn fingerprint. Secret
  values never enter the fingerprint, Debug, or persisted preview.
- AgentAccount launches verify the context/revision, resolve the Agent's normal
  default state home, do not call the Provider projection engine, and do not
  create an isolated Vibex account home. Every valid built-in ACP Agent may use
  this baseline with its ordinary Agent process environment.
- When an enhanced compatibility descriptor exists, AgentAccount launch unsets
  exactly its Registry-listed Provider/API-key/selector variables. Without a
  descriptor, the runtime uses no guessed global unset list and makes no
  enhanced logout or direct-model-discovery claim.
- The same source-aware launch decision is used by auth, model discovery, and
  real session creation. A successful login process is closed before a fresh
  verification/discovery process starts, so an in-memory credential cannot
  masquerade as persisted login.
- State-home identity, source kind/id, context/Profile revision, Agent/Adapter
  compatibility, and redacted environment fingerprint are part of runtime
  binding/restore identity. Workspace path is a separate compatibility input.
- AgentAccount process snapshots may expose only bounded command/config facts to
  diagnostics. They never persist raw default-home paths or environment values.
- Provider projection compatibility/backfill remains available for legacy
  Provider rows. It must not synthesize a Profile for an AgentAccount source.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Provider source lacks a valid enabled ACP Profile | existing Provider route/config validation error; no process. |
| AgentAccount context missing, Agent mismatch, or revision stale | `agent_auth_context_not_found`, `agent_auth_context_agent_mismatch`, or revision conflict; no projection. |
| Agent is absent from the built-in ACP catalog or has invalid Agent ACP config | `agent_not_found` or the typed ACP config validation error; source unavailable. |
| Agent lacks an enhanced compatibility descriptor | inherit the ordinary Agent environment; use no descriptor env-unsets and advertise no enhanced logout/direct-model capability. |
| AgentAccount path sees Provider projection plan | fail closed with source/projection mismatch; do not spawn. |
| Registry env-unset key is invalid/unbounded | compatibility descriptor validation error; do not guess a global key list. |
| Secret resolution fails on Provider path | redacted secret/config error; no partial overlay or process. |
| Auth/model/session fingerprints differ for same source revision | stale/recovery conflict; do not reuse the process or native session. |

### 5. Good / Base / Bad Cases

- Good: Provider source receives its configured API key and projection home;
  AgentAccount source starts from the normal Agent home with Provider keys
  explicitly unset, and both fingerprints remain distinct.
- Good: model discovery and the first real session use the same account source
  revision and state-home identity, so the discovered entitlement is relevant.
- Base: an Agent has no Registry descriptor; the runtime inherits its ordinary
  environment and falls back to ACP session evidence or `AgentDefault` without
  inventing a Provider model or enhanced account actions.
- Bad: set `CODEX_HOME`/equivalent to a Provider temp directory on AgentAccount,
  inherit a parent API key, or copy a token into the Vibex database.
- Bad: let a Provider projection write into the Agent's default state home or
  use a broad hard-coded env-unset list shared by every Agent.

### 6. Tests Required

- Projection tests cover Provider plan/materialization, secret-free preview,
  legacy compatibility, and source-specific fingerprints.
- ACP tests cover AgentAccount env unsets, default-home preservation, no
  projection calls, and equal launch identity across auth/probe/session.
- Registry tests cover enhanced descriptor validation and bounded env keys.
- ACP auth tests cover every built-in Agent accepting its valid default ACP
  config, a catalog Agent without a descriptor using its ordinary environment,
  and an unknown Agent failing before context creation.
- Restore/switch tests reject source/home/revision fingerprint mismatches and
  never reuse a Provider process for an AgentAccount target.
- Redaction tests inspect Debug, diagnostics, database rows, and remote DTOs for
  absence of secrets and raw paths.

### 7. Wrong vs Correct

#### Wrong

```rust
let projection = plan_provider_projection(profile_or_fake_profile)?;
spawn_with_home(projection.runtime_home);
```

#### Correct

```rust
match selection.auth_source {
    RuntimeAuthSource::ProviderProfile { .. } => {
        let projection = resolve_provider_projection(...)?;
        spawn_with_projection(projection)
    }
    RuntimeAuthSource::AgentAccount { auth_context_id } => {
        let env_unsets = registry
            .auth_context_capabilities(agent_id)
            .map(|capabilities| capabilities.credential_env_keys_to_unset)
            .unwrap_or_default();
        spawn_with_default_home_and_unsets(auth_context_id, env_unsets)
    }
}
```

## Scenario: Unsaved Provider Draft Model Discovery

### 1. Scope / Trigger

- Trigger: the Provider editor asks an endpoint for its Model list while the
  Provider is still a draft, because the user has typed a request URL and an API
  key but has not saved anything.
- This contract crosses the desktop Provider editor, the typed Backend boundary,
  the config-switch Model-discovery path, and the remote provider RPC.

### 2. Signatures

```text
AgentModelProviderDraftFetchModelsRequest {
  agent_id, base_url, api_key, configured_models[], provider_options?
}
AgentModelProviderDraftFetchModelsResponse { agent_id, models[], diagnostics[] }

ManagementBackend::fetch_agent_model_provider_draft_models(request)
  -> response
```

### 3. Contracts

- Draft discovery is offered as soon as the editor holds what a fetch reads: an
  Agent is selected, the credential surface is an API key, the Agent accepts a
  caller-supplied endpoint, and both the request URL and the key are non-empty.
  An Agent that pins its own endpoint owns its catalogue too, so it keeps the
  saved-Profile path instead of a draft one.
- The request carries the typed key for exactly one call. It creates no Profile
  row, persists nothing, and the key never appears in Debug output: the request
  implements `Debug` by hand and reports `has_api_key` instead.
- The draft is described to the fetch path exactly as a saved Profile is: the
  normalized Models the draft already configures can pin the wire protocol, the
  first enabled one is the default a save would record, and the draft's Provider
  options carry per-protocol address overrides and `apiRequestFullUrl`. When no
  Model pins a protocol, `agent_id` resolves the default through the projection
  registry.
- A blank key is refused before any request leaves the runtime
  (`agent_model_provider_secret_missing`); a missing address fails the protocol
  fetch (`agent_model_provider_endpoint_missing`).
- The answer fills the picker's catalogue only. It never writes the Models the
  Provider offers, and it is authoritative for the rows it advertises.
- A paired device reaches the same command through
  `RemoteProviderRequest::FetchAgentModelProviderDraftModels` under
  `ReadProviderSettings`, and the call hands it no stored Secret.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Draft key blank after trimming | `agent_model_provider_secret_missing`; no HTTP request. |
| Draft has no request URL | `agent_model_provider_endpoint_missing`. |
| Protocol is Mistral Conversations | `agent_model_provider_protocol_unsupported`; add Models manually. |
| No Model pins a protocol and the Agent declares none | `agent_model_provider_protocol_missing`. |
| Endpoint answers 404/405 on one candidate path | try the next candidate; other HTTP failures surface with a redacted endpoint and status. |

### 5. Good / Base / Bad Cases

- Good: the user types an address and a key in "Add model provider", fetches, and
  picks from the returned ids before saving anything.
- Good: a draft whose only configured Model declares Anthropic Messages lists
  through the Anthropic endpoint, and a per-protocol override wins over the
  default URL.
- Base: the fetch fails; the editor reports it beside the dialog's own actions
  and the draft stays exactly as typed.
- Bad: create a hidden Profile row so a fetch can run, send an empty bearer
  credential, or let the typed key reach Profile JSON, diagnostics, or Debug.

### 6. Tests Required

- `cargo test -p vibex-core draft_fetch` asserts the typed key survives
  serialization and is absent from Debug output, both on the request and through
  the remote provider envelope.
- `cargo test -p vibex-config-switch provider_draft_fetch` asserts the draft is
  described like a saved Profile (protocol pinning, per-protocol override,
  default Model) and that a blank key never reaches the endpoint.
- `cargo test -p vibex-desktop provider_draft_fetch_waits_for_a_request_url_and_a_key`
  asserts the fetch command appears only once the request URL and the key are
  both typed.


## Scenario: Local Model Gateway

### 1. Scope / Trigger

- Trigger: changing how Agents reach a Provider at request time, adding a
  Provider preset, adding a routing strategy, or touching the gateway's tables.
- The gateway is one loopback HTTP endpoint (`crates/model-gateway`) that serves
  the four wire APIs an Agent already speaks and forwards each call to whichever
  shared Provider Profile serves the requested model. `DesktopRuntime` owns its
  lifecycle; the Provider Profile and Agent binding contracts keep owning durable
  configuration.

### 2. Signatures

```rust
// crates/core/src/model_gateway.rs
GatewayPolicy {
    enabled, token_limit: Option<GatewayTokenLimit>, translate: GatewayTranslatePolicy,
    account_limits, preferred_keys,
}
ModelGatewaySettings { display_name, port, policy, revision, updated_at_ms }
GatewaySnapshot { settings, runtime, upstreams, models }

// crates/model-gateway
ModelGatewayService::{new, start, shutdown, settings, read_snapshot, prepare}
WireProtocol::{path, parse_request, render_response}
plan_route(RouteRequest) -> RoutePlan
classify_failure(status, body) -> GatewayFailureKind
conversation_key(agent_id, headers, body) -> Option<String>

// crates/config-switch
ProviderConfigService::{
    gateway_agents,
    gateway_agent_options,
    gateway_account_states,
    set_gateway_agent(agent_id, enabled, base_url, model_catalog),
    refresh_gateway_profiles(base_url, model_catalog),
    gateway_model_catalog,
}
```

### 3. Contracts

- **One gateway, one port.** The port persists in `model_gateway_settings`. A
  collision probes `+1..+20`, and the port that actually bound is stored. The
  bind is never ephemeral after the first success, because an Agent's base URL
  points at it.
- **A gateway failure never fails startup.** The status becomes `Unavailable`,
  the Config Center says so, and every Agent keeps its direct Provider
  configuration. Silence is the one forbidden outcome.
- **Routing state is request state.** Cooldowns, pool position, conversation
  pins, and token windows never enter `ProcessSpawnConfigSnapshot`,
  `ProviderProfile::launch_revision`, or any other process fingerprint. Replacing
  a Key must not restart an Agent process; a test asserts this.
- **Passthrough before translation.** When the upstream publishes an endpoint for
  the client's own protocol, the body is forwarded unchanged and only sniffed for
  usage. Translation runs only when the protocols differ, and a protocol the user
  disabled in `GatewayTranslatePolicy` is refused with
  `model_gateway_translation_disabled` rather than silently used.
- **Passthrough means the client's bytes, not a re-encoding.** `build_member_body`
  clones the inbound body and rewrites only `model` — plus `stream`, which the
  gateway owns. The encoder runs for a translation, and for a member that
  overrides the route's reasoning effort. Re-encoding a passthrough silently
  dropped every vendor field the IR does not model (`store`,
  `previous_response_id`, `prompt_cache_key`, `metadata`, `text.format`), so the
  vendor's own conversation state and prompt cache were lost while the ledger
  still reported a passthrough.
- **A tool result is an input item, never message content.** Responses models
  `function_call_output` as a sibling of the message that carries it.
  `input_item_parts` therefore excludes `ContentPart::ToolResult`, and
  `build_request` pushes the item before the text of the same turn. Nesting it
  made every vendor that flattens `input` into a Chat-shaped message list reject
  the whole turn (`unknown variant function_call_output, expected one of text,
  image_url, file`), so an Agent failed the moment its first tool result entered
  the history and worked before it. A vendor-file reference is a content part
  like any other; returning it as the whole message dropped the text beside it.
- **A Gemini model is in the path, so it has to be carried into the body.**
  `generateContent` names the model in the URL
  (`/v1beta/models/<model>:streamGenerateContent`) and the Google SDKs never
  repeat it in the JSON. `gemini_call` decodes the path segment and fills
  `model` into the body before routing; a body that already names one keeps it.
  Reading only the body resolved the model as `""` and answered
  `No configured Provider serves the model ""` for every Gemini call, while the
  ACP handshake had already succeeded — so the Agent looked connected and every
  prompt failed.
- **A refusal before routing is still a ledger row.** `prepare` can refuse a call
  before any member is consulted (no such group, no model, a bad body). Those
  rows are written with `outcome = rejected` and the gateway's own `errorCode`,
  because an empty ledger at the moment the Agent shows an error reads as "the
  request never happened" — which is what hid the path-model bug for a whole
  session.
- **A translated non-streaming call is read from the upstream as a stream.**
  The gateway asks the upstream for `stream: true`, gathers the SSE frames, and
  renders one JSON body for the client. An upstream that ignores the flag and
  answers a whole body is accepted too, so "only supports streaming" and "only
  supports whole bodies" both work without a capability flag.
- **Anthropic cache breakpoints survive passthrough, not translation.** The IR
  has no breakpoint field; a same-protocol call keeps `cache_control` because
  the body is forwarded byte for byte, while a cross-protocol call drops it.
  This is a documented limitation, not a silent promise that the prompt cache
  survived.
- **The intermediate representation carries what the protocols cannot share.**
  `CanonicalRequest.system` is its own field (Anthropic and Gemini keep it
  outside the message list). `ContentPart::Opaque` carries vendor-authored
  reasoning back to the protocol that produced it, byte for byte.
  `ImageSource::VendorFileId` is modelled so a same-protocol passthrough can
  carry it, and refused with `unsupported_image_reference` by any translation.
- **Tool calls survive a cross-protocol stream.** `CanonicalEvent::{ToolCallStart,
  ToolCallArgumentsDelta, ToolCallEnd}` are the only shapes the encoders see, so
  Chat's index-keyed fragments, Anthropic's `content_block` state machine,
  Responses' `output_item` events, and Gemini's whole-object `functionCall` all
  normalize into one accumulator and re-split on the way out.
- **Gemini reports `finishReason: STOP` even when it called a tool.** The decoder
  therefore records that a call was seen and reports `StopReason::ToolUse`.
- **A failure before the first byte may fail over; a failure after it may not.**
  An `auth` failure marks that Key `Invalid` with no expiry, because the
  credential is wrong until it is replaced, but the *request* still tries the
  next Key: a rejected Key in a pool must not fail a request another Key can
  serve. Every other kind gets a cooldown duration. A stream that ends without
  its protocol's terminal event is reported as an in-stream error rather than a
  clean `Stop`.
- **Cooling is per Key, not per Provider.** `model_provider_account_state` holds
  one row per `(profile, credential_id)`; a Key's `GatewayKeyState` is what the
  UI renders, and only `Ready` is selectable.
- **Conversation pinning preserves the vendor's prompt cache.** The key comes from
  `prompt_cache_key`, then an explicit session header, then a hash of the system
  prompt, tool set, and first user message, scoped by Agent. Pins expire after
  `GatewayConversationPin::TTL_MS` and are pruned.
- **The gateway's Providers are the groups' members.** It stores no list of its
  own; `read_snapshot` collects them from the enabled route groups. Pointing an
  Agent at the gateway creates one hidden Profile
  (`vibex.internal.profileRole = model_gateway`) per bound Agent, whose address
  follows the listener and whose model list is exactly that Agent's bound
  groups. That hidden Profile never appears in an Agent's provider list, and its
  protocol is taken from the Agent's own descriptor so the existing interface
  validation still passes.
- **Every published model id is a group id.** `/v1/models` and the Config Center
  list `group/<id>/<model>`. A bare `alias/model` id is not a gateway model:
  there is no upstream list left to derive an alias from, and resolving one
  would reintroduce it.
- **Usage is tokens only.** No `cost`, `price`, `currency`, `amount`, or `budget`
  field exists on any gateway contract, and `ModelGatewayRequestLog` carries no
  prompt, reply, image bytes, or Key material. It is pruned to a rolling seven
  days.
- **The global Providers page reads and writes the shared list.**
  `ManagementSnapshotPayload` carries `model_provider_profiles` and
  `agent_model_provider_bindings` beside the legacy `profiles`. The global page
  groups by Profile identity, so one Provider appears once however many Agents
  bind it, and the "used by" line is the bindings aggregated by
  `model_provider_profile_id`. Its detail pane edits the v2 record directly:
  the display name and status go through `update_model_provider_profile`, and a
  Key is added, replaced, or removed through the v2 credential endpoints. The
  Agent-scoped list stays during the compatibility window; it is what the
  Agent-scoped editor and its drag ordering still read.
- **Neither list is derived from the other.** The gateway's upstream picker uses
  the v2 identity the settings store, and a client never computes one id from
  the other; the authority resolves the pair once and publishes candidates.
- **The cross-Agent binding read is its own entry point.** The "used by" line
  needs the inverse of both indexed lookups, so
  `list_all_agent_model_provider_bindings()` exists beside
  `list_agent_model_provider_bindings()`. The filtered one still refuses a
  request that names neither an Agent nor a Provider: a caller that forgets a
  filter gets `agent_model_provider_binding_filter_missing` rather than a
  silent table scan. The bundle reads the whole table once and groups it in
  memory, so per-Agent details cost no extra query and cannot drift from the
  aggregate the page draws. An unfiltered call to the filtered method used to
  fail the entire `ManagementSnapshotPayload`, which surfaced as a gateway
  stuck on `Starting` and an empty Providers list.
- **An unreported gateway is not a starting one.** A bundle assembled without a
  gateway (an older authority, or a read that did not run) is reported as
  `Unavailable` with a notice, never left at `GatewaySnapshot::default()`. The
  default's `Starting` claims a boot is under way, so defaulting to it hides a
  failed read behind a state that looks like it will resolve itself.
- **The gateway is the pinned row of the Providers page.** It is not a
  top-level section: `ManagementSection::ALL` omits it, the navigation has no
  Gateway item, and a stored `gateway` cursor resolves to `Providers`. The
  Providers list draws the gateway first, with a `●` marker (direct Providers
  use `○`), its automatic-routing state, the aggregate Key summary, and the
  Agents bound to it. An empty upstream list is said in warning colour rather
  than shown as `0 keys`, because it is the reason the row cannot route.
  Selecting that row renders the gateway's detail in the same right pane as any
  other Provider; the old separate sidebar is gone.
- **The token ceiling is set where its window is.** The gateway detail edits the
  count and the window (day/week/month) as one pair, because a count without a
  window is not a ceiling. An empty field is refused, not read as zero; removing
  the ceiling has its own control. The copy states that the count is local
  tokens the gateway observed, not the vendor's own quota or balance.
- **A Key is switched off where it is listed.** `enabled` is durable account
  state, not policy: turning a Key off takes it out of rotation without touching
  its Provider or its secret, and works even before the Key ever failed. Turning
  it back on clears any failure cooldown, because an authentication failure never
  expires on its own and the user has just asked for the Key to be tried again.
- **A preferred Key leads every strategy.** The preference lives in
  `GatewayPolicy.preferred_keys`, one credential per Provider. Marking a Key
  edits policy, so it is carried by the settings write and appears in the
  snapshot; the router puts it first, then applies rotation or usage to the rest
  of the pool. Clearing it returns the Provider to its configured Key order.
- **An Agent's mode is stated before its Providers.** The provider card opens
  with one line: gateway (named), gateway unavailable and falling back to
  direct, direct (named Provider), or direct because the Agent manages its own
  provider. A configurable Agent the snapshot has not reported yet shows no
  line rather than guessing. Changing the mode rewrites process configuration,
  so the switch asks first and names the restart and the interrupted
  conversation; the row says `Switching…` until the authority answers. Turning
  the gateway on needs no confirmation, because it restores service.
- **The gateway's display name is edited in its own header.** The pencil opens
  the current name in place; the write trims it, rejects an empty or over-long
  name (counted in characters, so a CJK name is measured like the authority
  measures it), and goes through the same settings command as routing. The
  detail pane keeps the name as its heading, so a rename is visible where it was
  made.
- **Translation directions are user policy.** The gateway detail lists the four
  protocol directions with switches. The settings write replaces the policy, so
  every unrelated edit carries it forward; turning a direction off makes a
  request that needs it a `400 model_gateway_translation_disabled` rather than
  a mangled request.
- **Cooling is visible in the Providers list, and expires without a reload.**
  The global list borrows the gateway Key summary for upstreams (total, cooling,
  invalid, soonest recovery). A wake-up is armed for the soonest future cooldown
  (at most a minute apart, so the countdown ticks) and re-reads only the gateway
  snapshot, so the countdown disappears when the Key comes back instead of
  waiting for a page change.
- **Replacing a secret keeps the Key.** The inline replacement writes the same
  credential id, name, limits, and position. The write clears the failure
  cooldown because an authentication failure never expires on its own; removing
  and re-adding is not equivalent, and clearing the value does not revive it.
- **Destructive gateway actions are confirmed.** Removing the last Key, clearing
  the token ceiling, and turning automatic routing off ask first. Removing a
  non-last Key, switching one Key off, and turning automatic routing on do not:
  they are reversible or restore service.
- **The TUI's Provider columns are the gateway's vocabulary.** Groups, Keys,
  State, and Models: a direct Provider shows one group, its single secret as 0 or
  1 Keys, ready/off, and its model count; the gateway shows the enabled route
  groups it serves, the aggregate Key summary, and the models those groups
  publish. No column names an Agent, because the gateway has no Agent list. The
  old Agent/Kind/Model/Credential facts remain in the row detail.
- **A Key row wraps rather than truncating its own name.** The switch, state,
  preference, replacement, and removal controls are one wrapping cluster, so a
  narrow window pushes them to a second line instead of cutting the Key label.
- **The TUI preset picker names a plan's price.** When a plan preset publishes
  vendor copy such as `$9.99/mo`, that is the picker's second column; the
  address and the no-key hint fill it only for presets without one.
- **Concurrency and rate limits are per Key.** A lane bounds in-flight requests
  and a rolling minute bounds request starts. A request that cannot start within
  its own queue deadline is refused with `429` instead of waiting forever, and a
  permit is released on drop so a cancelled request cannot leak capacity. These
  guards bound what Vibex sends; they claim nothing about what the vendor
  counted.
- **A catalogue value is data, not inference.** `crates/model-catalog` reads a
  maintained table of model metadata. The Provider rule against inferring a
  capability from a model id, endpoint, or vendor is untouched: the catalogue is
  a second *source of data*, the same kind of statement a user makes by typing a
  value. Three constraints follow:
  - A value is only ever a **suggestion**. Nothing is applied until the user
    adopts that field, and the adoption is recorded per field in
    `ProviderModelCapabilities::sources`; an absent entry means `Declared`, so
    older records stay authoritative without a migration.
  - A hand-typed value always wins and loses its catalogue mark.
  - Every failure is silent. A catalogue that cannot be fetched, parsed, stored,
    or read yields no suggestion and changes nothing else; a stale document still
    suggests, but its matches are marked approximate.
  - `reasoning_efforts` is never suggested. The catalogue does not publish an
    Agent's own effort vocabulary, and inventing one produces a declaration the
    Agent refuses.
  - No money field exists anywhere in the catalogue, its cache, or its DTOs.
- **A route group steps aside, not the gateway.** Three consecutive requests
  that produce no upstream answer put *that group* into a stepped-aside state:
  the listener stays bound, everything the group would have served but one
  request at a time is refused with a `503` that says the group stepped aside,
  and one success clears both the count and the group. The service-wide
  `Degraded` status is derived from those per-group breakers (`degraded_routes`)
  and never latched independently; `Running` means none is set. The scope is the
  group because that is what a failure belongs to — three Agents bound to the
  same group share its breakers, and a group whose upstream is healthy keeps
  serving while its neighbour steps aside.
  `Degraded` is deliberately distinct from `Unavailable`: the port is alive
  either way, `base_url()` and `is_routable()` stay true while degraded (the
  remaining groups still serve), and only `Unavailable` means "restart or wait".
  A failure older than the memory window starts the count over, and the
  stepped-aside refusal itself never counts, or the state could never clear.
- **An undeclared protocol is a guess, and a guess is corrected once.** An
  endpoint that carries no `wireProtocolId` is an address whose API is unknown,
  not an endpoint that speaks all four. `GatewayUpstream::endpoint_for` matches
  only an endpoint that *names* the protocol; `untyped_endpoint` is the single
  fallback, and `choose_target` uses it by sending a passthrough first — the
  cheapest try, and the one that keeps the vendor's prompt cache — with every
  other protocol the translation policy allows queued behind it.
  `model_gateway_endpoint_protocol` remembers the protocol that answered, keyed
  by `(profile, endpoint_id)`, so the walk happens at most once per endpoint.
  Treating "no declaration" as "speaks every protocol" is what sent a Responses
  body to a Chat Completions endpoint unmodified.
- **A wrong protocol blames nothing.** A `404`/`415`/`422`, or a `400` whose body
  names a shape problem, is classified as `GatewayError::ProtocolMismatch` and
  answered as `502 model_gateway_protocol_mismatch`. It does *not* cool the Key
  (`mark_failure` is skipped), does not count toward a group's breaker
  (`GatewayError::blames_upstream()` is false for it), and does not latch a
  degraded state. The Key worked; only the body was addressed to the wrong API.
  Cooling the Key on that reply is how three wrong guesses closed the whole
  gateway: every retry then hit `429 model_gateway_keys_cooling`, which counted
  as another failure.
- **Only a guess may be called a wrong protocol.** The same shape refusal on a
  *settled* protocol — declared by the endpoint, remembered, or simply the one
  the client spoke through a passthrough — is `GatewayError::TranslationFailed`
  (`502 model_gateway_translation_failed`). The protocol was never in question and
  no Provider setting can fix the body, so telling the reader to change it is a
  false lead; and because nothing is rewritten in
  `model_gateway_endpoint_protocol` on that path, one encoder bug cannot be
  remembered as a wrong verdict for every later request. Both failures are
  `blames_upstream() == false` and neither cools a Key.
- **A failed request is still a ledger row.** `open_attempt` records the failure
  before returning it, with the gateway's own error code in the route trace
  (`GatewayRouteTrace::error_code`) and in the client's `GatewayRouteSummary`.
  Previously only requests that reached `forward`/`gather` were recorded, so a
  gateway-side refusal left the ledger reading "all ok" while every turn failed —
  the one state an investigation cannot start from.
- **The gateway's own switch reaches the listener and the bindings.** Turning it
  off stops the listener and removes every hidden gateway Profile, so each bound
  Agent really falls back to its direct Provider instead of pointing at a dead
  port. The *bindings* survive, which is what a later start re-reads to recreate
  the Profiles without asking again. A start failure publishes `Unavailable`
  rather than leaving the surface at `Starting`, and a failed start removes the
  Profiles for the same reason. `gateway_active_agents()` is the projection;
  the bindings are the only durable choice.
- **It starts with nothing.** A fresh installation has the gateway on and no
  route group; the listener binds, `/v1/models` is empty, and the surface says
  "no route groups" rather than "not running". A missing route is not a failure,
  and reporting it as one is what made the gateway look like a feature of an
  upstream list it no longer has.
- **A Key is added and removed on the shared Provider.** The reference is
  written first with a placeholder and the value goes through the same secret
  store as every other credential, so plaintext never enters the profile row.
  Removing a Key deletes its stored Secret. Either operation advances the profile
  revision, which is what restales the bindings that project it.
- **The gateway accepts loopback callers only, and no browser.** The `Host`
  header must name `127.0.0.1`, `localhost`, or `::1`, and any `Origin` header is
  refused; the second check is what defeats a hostile page reaching the loopback
  port (including DNS rebinding).
- **Presets ship with the binary.** A preset supplies addresses per wire
  protocol, a starting model list, and the caveats its data cannot express.
  Creating from a preset writes the protocol into `providerOptions["wireApi"]`
  explicitly: the editor's last-resort fallback is "the first protocol that has a
  URL", which otherwise relabels an Anthropic Provider as Chat Completions on
  reopen.
- **A plan preset states what its vendor's list cannot.** A plan that resells
  models carries its own `models`, an `only` prefix that keeps a metered model
  from being mistaken for a covered one, and a caveat. A fetch that returns none
  of the plan's ids falls back to that list instead of reporting an empty
  account, and a fetch that cannot be read at all keeps the plan's models in the
  picker. `caveats` is editor copy; no runtime branch keys off it. A plan whose
  free models its vendor refuses to other clients is not listed at all — an
  editor that offered them would send the user into a `403`.
- **A preset is a starting point, never a mode.** A region choice rewrites only
  the address fields, and it resolves against the preset as shipped, so a second
  choice replaces the first one's addresses. The editor marks a field that still
  holds its preset value by re-reading the draft: nothing records "came from a
  preset", so an edit removes the mark by itself and restoring the value brings
  it back.
- **The routing order is the group's member order.** Moving a member swaps it
  with its neighbour and changes nothing else; an edge row offers no move past
  the end rather than wrapping. The order lives on `ModelRoute.members`, which
  is the order the group's strategy tries them in; the gateway has no list of
  its own to order.
- **Clearing the ledger is confirmed and bounded by its copy.** The request is
  confirmed before it runs, and the wording names both the seven-day window and
  what the records never contained (prompt, reply, key, image content), so it
  cannot be read as clearing usage history.
- **The ledger explains the route, not only the outcome.** A row carries a
  versioned trace (`gatewayRouteTrace` payload v1) naming the Keys the router set
  aside because they were cooling or over their ceiling, and the Keys it tried
  before one answered, each with its failure kind. It holds ids and kinds only, so
  it still carries no prompt, reply, or Key material. A payload the reader cannot
  interpret — an older or newer version, or a corrupt blob — reads as *no trace*
  rather than failing the list: the ledger is a diagnostic, and one unreadable
  annotation must not hide every other request.
- **A failover is an attempt that failed, not a pool that had more than one Key.**
  The outcome follows the recorded attempts. A request served on the first try
  from a Provider holding five Keys is `ok`; labelling it `failover` made the
  column useless exactly where it mattered.
- **The route map and the request records are one tab.** The gateway page's
  Routing tab (`provider_workspace/routes.rs`) draws the route map
  (`route_stage.rs`: Agents → gateway → the route group's members, each member
  with its Key chips), the story of one request under it, and the request
  records below. Picking a record pins the map to that request and replays its
  walk; picking it again replays it; Back to live unpins. Traffic keeps only the
  aggregates. Splitting the map and the records across two tabs left a record
  with no way to show its route.
- **The map plays what the gateway announced, never a guess.** Every
  `GatewayEvent` is folded into a per-request trail (`note_gateway_trail`): one
  step per member the gateway announced, timed by when the window heard it, plus
  the first token and the end. A request already running when the page opens is
  drawn from the live table. Member failover is therefore visible while it
  happens; which Key refused and why arrives with the ledger row, and
  `ledger_walk` rebuilds the order from it: a member's refusing Keys precede the
  member in `failedAttempts`, a member none of whose Keys refused was passed
  over, and the Keys left over belong to the member that answered. A `rejected`
  row walked nowhere — the map shows the gateway turning it back.
- **The map asks for frames only while something travels.** A dot in flight,
  a shaking member or a fresh refusal tag asks for animation frames; a request
  that is only waiting redraws on a ~15 fps timer for its pulse and flowing
  wire; with reduced motion, or the pause-when-inactive setting on an inactive
  window, nothing moves and the map shows where each request stands. Every
  animation frame re-renders the views above the map as well, so the map owns
  no work beyond its own frame.
- **A created Provider is verified by one real minimal request, and the check
  cannot undo it.** The save writes the row first and probes it afterwards, so a
  blocked probe — a proxy, a rate limit, a vendor that refuses this client — is
  reported as a failed check rather than rolled back into a Provider that does not
  exist. Updates are not re-verified; that Provider already has an answer.
- **Creating a Provider starts by choosing a preset.** The picker groups vendors,
  local services, and third-party plans, and its filter matches the id, the
  display name, and the address, because those are the three things a user
  remembers about a vendor. Plan rows carry the vendor's own price copy; plan
  limits are marked in the list and stated in full only once the preset is
  chosen. "Custom provider" sits at the foot of the list and resets the draft the
  way a fresh dialog does, so a preset that was looked at and left behind cannot
  leave its addresses in a form the user asked to start empty.
- **The TUI's Providers table mirrors the gateway's hierarchy.** The gateway
  row is followed by one row per group member Provider (`├`) and one per Key
  (`│ ├`), so the reader can see which account is cooling without opening the
  desktop. A Key row offers `Toggle` and `Prefer`; the gateway row offers
  `Token limit` and `Recent requests` only, because the member strategy and the
  conversation pin belong to each route group. The token
  ceiling is a prompt because a character grid has no room for a combined
  number-and-window control; an empty answer removes the ceiling. The request
  ledger is a read-only text view with the model, group, tokens, and skipped
  Keys. Key creation, replacement, and removal stay on the desktop, which owns
  the masked input and the confirmation dialogs.
- **A conversation's pin and the group's failover setting are two halves of
  one decision.** The pin names the member a conversation is already on, and it
  is hoisted to the front of the attempt order before any strategy runs: the
  strategy still orders the rest, so a conversation that has to leave its pinned
  member inherits the group's preferred fallback order instead of an arbitrary
  one. Pinning only inside the chosen member's Keys made the pin record
  meaningless under round-robin, because the strategy re-picked the member every
  turn and the pin re-ordered Keys inside whichever member it happened to
  choose. The other half is `failover`, a per-route column beside `pin`:
  `auto` leaves the failed member silently and reports the switch afterwards,
  while `ask` puts the question to the session first. Asking is bounded by
  `GatewayFailoverPrompt::ANSWER_WINDOW_MS` — five seconds — because the
  gateway is a synchronous HTTP proxy and the Agent turn is blocked on the
  response: an unbounded question would hang the turn. Silence accepts the
  switch, so the prompt is a countdown rather than a blocking dialog, and the
  client draws it the way the auto-continue box is drawn. A failure that does
  not blame the upstream — a protocol-shape refusal, an unusable Key — moves the
  conversation without asking, because leaving a member over the gateway's own
  mistake would punish the wrong party. The prompt's one-click "always switch"
  writes `failover = auto` to the route itself, so a choice made under pressure
  survives the next restart. When no session can be found to ask, the gateway
  behaves as `auto` and the switch is recorded rather than the request failing.
- **The composer's model menu leads with the model the user already chose.** The
  remembered selections — most-recent-last, already capped — break ties inside a
  group after stars; a search term outranks both, so recency never displaces a
  better match. An empty search echoes the term back and names the two things
  that produce a match, because from inside a filtered panel a missing Provider
  and a typo look identical.

### 4. Validation & Error Matrix

- No upstream serves the model -> `404 model_not_found` with up to three closest
  configured ids.
- Every Key for a served model is cooling or over its ceiling -> `429
  model_gateway_keys_cooling`, and no upstream call is made.
- A local token ceiling is crossed -> `429 model_gateway_token_ceiling_reached`;
  requests resume when the window rolls over.
- A translation the policy disabled is required -> `400
  model_gateway_translation_disabled`.
- A request carries a `file_id` image and the target is not Responses ->
  `400 unsupported_image_reference`; the image is never silently dropped.
- The gateway is enabled for an Agent that manages its own provider ->
  `model_gateway_agent_unsupported`.
- The gateway is enabled with no running listener -> `model_gateway_unavailable`.
- Gateway settings skip or repeat a revision -> `model_gateway_settings_revision_invalid`
  / `model_gateway_settings_revision_conflict`.
- A non-loopback `Host` or any `Origin` -> refused before routing.
- A Key's queue is full, or the wait would exceed its deadline -> `429` before an
  upstream call.
- A Key's `max_concurrency` or `max_rpm` is zero -> `model_gateway_concurrency_invalid`
  / `model_gateway_rpm_invalid` when the settings are validated.
- An account update changes neither `enabled` nor `preferred` ->
  `model_gateway_account_update_empty`.
- An account update names a Provider that is not an upstream ->
  `model_gateway_account_upstream_unknown`; names a Provider that does not exist
  -> `model_gateway_account_provider_unknown`; names a Key the Provider does not
  have -> `model_gateway_account_key_unknown`. None of them writes anything.

### 5. Good / Base / Bad Cases

- Good: one gateway with two upstreams serves `alias/model`; the first Key of the
  answering upstream returns 429 and the request lands on its second Key, which
  is recorded as `failover`.
- Good: an Anthropic-speaking upstream answers a Chat-speaking Agent; frames are
  re-encoded as `chat.completion.chunk` and the ledger records both protocols.
- Good: a conversation pinned to one Key stays there across turns even under
  round-robin routing; when that Provider fails under `ask`, the session is
  asked for five seconds and either answer settles the switch.
- Good: a route set to `auto` switches a failing Provider silently, and the
  conversation shows one line naming the model and both Providers.
- Base: a Key's authentication is rejected; it becomes `Invalid`, never
  `Cooling`, and the next Key answers.
- Bad: resolve the model id, pick a Key, or emit a marker that changes an Agent
  process fingerprint.
- Bad: write a translation or preset behaviour that branches on a vendor's model
  id to guess its capabilities.

### 6. Tests Required

- `cargo test -p vibex-model-gateway` covers the IR, all four decoders and
  encoders, SSE frame boundaries, tool-call accumulation, failure classification,
  route planning, the pin's hoist ahead of the strategy order, an answer
  reaching the waiting request, and the loopback perimeter.
- `cargo test -p vibex-db route_policy_round_trips` covers `failover` surviving
  storage beside `pin` and the token ceiling.
- `cargo test -p vibex-db model_gateway` covers a Key switched off before it
  ever failed and re-enabling clearing an authentication failure.
- `cargo test -p vibex-model-gateway` covers a preferred Key leading the
  ordered, round-robin, and least-used strategies.
- `cargo test -p vibex-desktop gateway` covers the token-ceiling editor's parsing
  and the Key row's switch/preference wiring.
- The same desktop suite covers the Agent mode line and its restart
  confirmation, the four translation switches, the cooldown wake-up target, the
  Provider-list Key summary, the in-place Key replacement, and the destructive
  confirmations.
- `cargo test -p vibex-tui --test config_center` covers the Groups/Keys/
  State/Models Provider headings and the gateway row's ready/off wording.
- `cargo test -p vibex-model-gateway --test gateway` drives a real listener
  against a mock upstream: passthrough, translation, gathering, cooling, auth
  invalidation, the token ceiling, pinning, an unknown model, `file_id` refusal,
  and the Origin check.
- `cargo test -p vibex-config-switch gateway` asserts one hidden Profile per
  Agent, that it stays out of the visible list, that the address follows the
  listener, and that a no-op refresh does not churn the binding.
- `cargo test -p vibex-db model_gateway` asserts migration 64 and 65
  idempotency, revision compare-and-swap, window bucketing, pin expiry, ledger
  retention, that the route trace round-trips, that an unreadable trace reads as
  absent instead of failing the list, and that no prompt or Key text reaches the
  ledger.
- `cargo test -p vibex-model-gateway --test gateway` asserts the route trace
  against a real upstream: a cooling Key is counted, a refused Key is named with
  its failure kind on the row that records the Key that answered instead, and a
  first-try success on a multi-Key Provider is not labelled a failover.
- `cargo test -p vibex-core model_gateway` asserts a request summary names the
  Provider and each Key in its route, drops a trace that says nothing, and keeps
  the id of a Key that has since been removed.
- `cargo test -p vibex-model-gateway --test gateway` asserts that three
  unreachable requests step a group aside, that a stepped-aside group refuses
  with `503` instead of retrying, and that the state is reported with a reason.
- `cargo test -p vibex-model-gateway --test gateway` also asserts the protocol
  contracts end to end: an undeclared endpoint is tried once and the winning
  protocol is remembered (the second request skips the guess), and a mismatch
  with no allowed fallback answers `502 model_gateway_protocol_mismatch` while
  leaving the Key un-cooled and `degraded_routes` at zero.
- `cargo test -p vibex-model-gateway --lib` asserts the breaker's transitions
  directly: three failures degrade one group while its neighbour keeps serving,
  one success recovers, and an old failure does not accumulate. It also asserts
  `choose_target`'s precedence — a declared endpoint outranks passthrough, an
  untyped one is guessed once with an ordered fallback list, and a remembered
  verdict replaces the guess.
- `cargo test -p vibex-config-switch credential_tests` asserts that a Key can be
  added and removed, that the profile row never carries the value, that an empty
  Key is refused, and that each add advances the revision.
- `cargo test -p vibex-model-catalog` asserts exact and approximate matching, that
  adoption records provenance per field, that a hand-typed value is not
  overwritten, that `reasoning_efforts` is never suggested, that a corrupt or
  older-schema cache reads as missing, and that no cached field carries money.
- `cargo test -p vibex-desktop gateway_surface_tests` asserts every gateway state
  is named in words, that a gateway without a ready Key is not routable, that a
  shared Provider appears once with every binding Agent named, that a deleted
  binding drops out of that list, that a cooldown names its remaining time, that
  moving an upstream swaps only one neighbour, that every caveat the preset table
  declares has copy, that every failure kind has human copy in every locale, that
  the ledger row opens its route and that the route line names the Key that
  answered and the one it stepped over, that the preset picker groups and filters
  its library, and that the preset, verification, and ledger paths stay wired to
  their surfaces.
- `cargo test -p vibex-desktop --lib a_remembered_model_floats_to_the_top_of_its_group`
  asserts the composer's model menu leads with a remembered model and that a
  search term still outranks recency.
- `cargo test -p vibex-core provider_presets` asserts every preset is addressable
  by its own id and that a plan preset's model list survives a vendor answer that
  omits it.
- `cargo test -p vibex-model-gateway limit` asserts the lane serializes, queues
  within its bound, refuses when full or waited out, and that a minute allows
  exactly its limit.
- `cargo test -p vibex-model-gateway --test gateway` measures upstream overlap
  directly: a limit of one yields a peak of one, and a companion case without a
  limit shows the same load overlapping.
- `cargo test -p vibex-config-switch` asserts that rotating an upstream Key
  leaves the gateway-bound Agent's launch revision unchanged, while moving the
  gateway's address moves it.
- `cargo test -p vibex-tui --test config_center` asserts a preset-created
  Provider keeps the preset's protocol and per-protocol addresses, that the
  gateway's switch answers the press and files the write, that a gateway nobody
  has read cannot be switched, and that the effect reaches
  `set_model_gateway_enabled` and draws the snapshot it returns.
- `cargo test -p vibex-tui --test config_center` asserts the gateway leads the
  Providers table with its Key and Agent summary, and that a missing gateway
  leaves the direct Providers readable.

### 7. Wrong vs Correct

#### Wrong

```rust
// Pick a credential once and cache it on the Profile, so every request reuses it.
profile.selected_credential = Some(pick_first_ready_key(&pool));
save_profile(&profile)?;
```

This moves a request-time decision into durable configuration, invalidates the
Agent's launch revision, and restarts the process on every Key change.

#### Correct

```rust
let plan = plan_route(RouteRequest { candidates, strategy, rotation, pinned })?;
let (attempt, key) = service.open_attempt(&prepared).await?;
// The Key lives for this attempt only; the ledger records its id, never its value.
service.record(&prepared, &RequestOutcome { credential: Some(key.id), .. });
```

## Scenario: The Gateway Has No Configuration Of Its Own

### 1. Scope / Trigger

- Trigger: the Config Center switches the local model gateway on or off.
- Trigger: a running listener's address moves.
- Trigger: an Agent is bound to, or unbound from, a route group.

### 2. Signatures

```text
ProviderConfigService::gateway_agents()            -> Set<AgentId>   (intent)
ProviderConfigService::gateway_active_agents()     -> Set<AgentId>   (projection)
ProviderConfigService::attach_gateway_to_agents(&url, &catalog) -> Vec<AgentId>
ProviderConfigService::detach_gateway_from_agents() -> Vec<AgentId>
ProviderConfigService::refresh_gateway_profiles(&url, &catalog) -> Vec<AgentId>
ProviderConfigService::gateway_route_members()     -> Vec<ModelProviderProfileId>
ModelGatewayService::start()                       -> ModelGatewayRuntimeState
```

### 3. Contracts

- **The gateway is a transport and a scheduler, not a Provider collection.** It
  holds no upstream list (`ModelGatewaySettings` has no `upstreams`), and it has
  no per-Agent switch (`model_gateway_agents` is dropped). Its only inputs are
  the enabled route groups: the Providers they name are the Providers it may
  call, read fresh on every snapshot.
- **It runs whether or not a route exists.** `start` binds whenever the policy is
  enabled and the port is available. An empty group set is a valid state, not a
  configuration error: `/v1/models` answers an empty list and a request for a
  model the listener does not serve is a `404 model_not_found`. The runtime
  state's count is `route_count`, because "how many upstreams" no longer means
  anything.
- **`GatewayPolicy` carries no routing strategy and no pin.** Both belong to the
  composition, so they live on `ModelRoute` and the group decides them. What
  remains gateway-wide is what describes the listener and the accounts: the
  switch, the token ceiling, the Key limits, preferred Keys, and the translation
  switches.
- **A binding is the only way onto the gateway.** `attach_gateway_to_agents`
  walks the live bindings, and each Agent's hidden Profile receives only the
  model ids of the groups it is bound to. An Agent that loses its last enabled
  binding loses its Profile, so it falls back to its own direct Provider.
- **One Agent may bind several groups.** The hidden Profile's catalogue is the
  union of those groups' model ids; exactly one binding is the Agent's default,
  which is what a new session starts on.
- **Switching the gateway off makes the groups unusable, and nothing else.** It
  stops the listener and removes every hidden Profile. Direct Providers are
  untouched because they never reached the gateway. The bindings survive, so a
  later start re-creates the Profiles for the same Agents and groups without
  asking again.
- **A failure is reported, never silent.** A failed bind publishes
  `Unavailable` and removes the Profiles; repeated upstream failures raise
  `degraded_routes` and publish `Degraded` (the listener stays bound, and each
  stepped-aside group lets one request at a time through). Neither state is
  `Disabled`, because neither means the user turned it off.
- **There is no plain gateway route.** `RouteKind` has two shapes:
  `ProviderGroup` (served by the gateway) and `Direct` (never is). The retired
  `Gateway` kind and its `route/<id>` ids are gone from the model namespace;
  every gateway model id is `group/<id>/<model>`.
- `set_model_gateway_agent` is refused with `model_gateway_agent_scope_removed`,
  and the request type survives only so an older client gets a readable refusal.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Attach with an empty address | `model_gateway_unavailable` |
| Attach while the policy is disabled | Detach instead; no Profile points at the gateway |
| Attach with no bindings at all | No Profile is created; the listener is still `Running` |
| Attach for an Agent lacking ACP configuration | Skipped; that Agent keeps its direct Provider |
| Any per-Agent gateway write | `model_gateway_agent_scope_removed` |
| Policy disabled | `gateway_agents()` is empty and every hidden Profile is removed |
| A model id without `group/` | `404 model_not_found`; it is not a gateway model |
| Settings write naming a Provider no group uses | `model_gateway_account_upstream_unknown` |

### 5. Good / Base / Bad Cases

- Good: a fresh installation ships with the gateway on and no groups; the
  listener is `Running` with an empty model list, and creating a group is the
  first thing that makes it useful.
- Base: an Agent is bound to two groups. Its Profile lists both groups' models,
  one binding is its default, and unbinding one leaves the other usable.
- Bad: a per-Agent toggle, or a gateway-owned upstream list, puts a second
  authority beside the bindings; one of them then claims a model the other does
  not serve.

### 6. Tests Required

- `vibex-model-gateway`: a listener with no groups is `Running`, publishes an
  empty model list, and answers `404` for a group it does not have.
- `vibex-config-switch`: an Agent with no binding is not attached; a binding
  attaches only its own Agent with only its own groups' models; a second group
  joins the same Profile; unbinding the last group removes it; a disabled group
  leaves the Profile; detaching removes all of them; a stopped listener removes
  the Profiles but keeps the bindings; an empty address is refused.
- `vibex-desktop`: the gateway page renders no Agent card and no upstream
  editor; no `snapshot.agents` or `snapshot.upstream_candidates` reference
  remains.
- `vibex-tui`: the gateway row counts groups and models, not Agents, and offers
  no routing or pinning verb.

### 7. Wrong vs Correct

#### Wrong

```rust
// Give the gateway its own Provider list, and switch it per Agent.
settings.upstreams = vec![provider];
ModelGatewayAgentRepository::set(&conn, agent_id, true)?;
```

Both create a second authority beside the route groups: the listener can be off
while an Agent says it is routed, or serve a Provider no group names.

#### Correct

```rust
if !settings.policy.enabled {
    return self.deactivate_gateway_profiles();
}
// The bindings decide who is on the gateway, and each Profile gets only the
// groups its own Agent references.
for (agent_id, catalog) in catalogs_by_agent(&conn)? {
    let _ = self.ensure_gateway_profile(&agent_id, base_url, &catalog);
}
```

The groups decide; the hidden Profiles are their projection, and a failure for
one Agent is local to that Agent.

## Scenario: Route Groups Compose Whole Providers

### 1. Scope / Trigger

- Trigger: the Config Center's Providers page creates, edits, or deletes a route
  group in its route-group tab.
- Trigger: an Agent's model-provider card references or drops a reference to a
  route group.
- Trigger: the gateway resolves a `group/<route>/<model>` model id.

### 2. Signatures

```text
RouteKind::{Gateway, ProviderGroup, Direct}
ModelRoute::model_id()               -> "route/<id>"        (plain routes)
ModelRoute::group_model_id(&model)   -> "group/<id>/<model>" (provider groups)
provider_group_models(&route, &providers) -> Vec<ProviderGroupModel>
provider_group_model_ref(&str)       -> Option<{group_id, model_id}>
GatewaySnapshot::group_for_model(&model)
shared_route_group_rows(&routes, &bindings) -> Vec<SharedRouteGroupRow>
```

### 3. Contracts

- **A route group is a global record, not an Agent's property.** `ModelRoute`
  carries no `agent_id`; an Agent references a group through
  `AgentModelRouteBinding`, the same way it references a shared Provider. One
  group can serve several Agents, and editing its composition reaches all of
  them.
- Creating a group is one write that never requires an Agent: the Providers
  page's route-group tab creates a record nobody references yet, and an Agent's
  chooser creates the same global record and then binds that Agent to it.
- **Deleting and un-binding are different operations.** The Agent card's remove
  control calls `unbind_agent_model_route` and leaves the record alone; only the
  Providers page's group detail pane calls `delete_model_route`, because that is
  the one surface that lists every Agent the deletion affects. Un-binding must
  never be implemented as a delete.
- The Providers page lists both records in one left column with two tabs:
  Providers and route groups. Both tabs keep the gateway row above them, since a
  group is served by that gateway. The tab, not the selected row, decides which
  detail pane the page renders.
- The Agent's `+` chooser owns both halves of the same question. It lists
  Providers that can be bound, then route groups that can be referenced (minus
  the ones this Agent already references), then a "new route group" entry. A
  group created from the chooser is global and bound to that Agent on save.
- A `ProviderGroup` member names a **Provider**, not a model: its `model_id` is
  empty and `validate` rejects a model on a group member. A group therefore
  cannot say which models it offers; they are derived.
- The group's models are the **intersection** of its enabled Providers'
  enabled catalogues, in the group's member order, then each catalogue's order.
  A model only one Provider serves is not part of the group.
- A group needs at least **one** Provider. A group of one is deliberate: it
  puts the gateway — and therefore its protocol translation — in front of a
  single upstream, which is something a direct Provider cannot do. It must not
  claim failover (`auto_routes` stays false, `member_count` is 1), and the
  editor says so instead of implying a missing choice.
- `group_for_model` resolves only models the group actually offers. Refusing
  the rest is what keeps the strategy from picking a Provider that has never
  heard of the model.
- A group contributes one gateway catalogue entry and one session option per
  shared model. A group is not a model, so the model lives in the id.
- Group capabilities are the intersection of the capabilities of the Providers
  serving that one model; an explicit route `context_tokens` overrides the
  smallest-window rule.
- A group is bound to an Agent like any route (`AgentModelRouteBinding`), and
  its launch row is the Agent's gateway Profile. A request for one of its models
  keeps the loopback address, so switching among group models does not restart
  the Agent.
- Creating or editing a group runs the same service rules as any route write.
  The route budget (`MAX_ROUTES`) counts **user-authored** routes only: a
  `Direct` row materialised by `backfill_direct_routes` is derived from the
  Provider list, so it must not spend a budget the user never asked it to spend.
  Counting them made an installation with many direct Providers fail a group
  save with `model_route_limit_exceeded` even though the user had authored one
  route.
- The served-model check applies only to a member that names a model. A group
  member names a Provider and carries an empty `model_id`, so there is nothing to
  look up; the group's models are derived later from the Providers' catalogues.
  Running the check on the empty id refused every group with
  `model_route_member_model_unknown` once the budget stopped refusing it first.
- **A route write must republish the gateway catalogue.** An Agent's selectable
  models are read from its hidden gateway `ProviderProfile`, which is a copy of
  the catalogue, not a live view. Creating, editing, deleting, binding or
  unbinding a route therefore has to call `refresh_gateway_profiles` in the same
  operation. Without it the group exists, is bound, is served by the gateway, and
  still never appears in the session's model picker, because the Profile it would
  be selected through was last written before the group. A runtime whose listener
  is not bound removes the Profiles instead, matching the gateway switch.
- **A gateway Profile carries typed ACP configuration.** Every Profile write
  validates it, so the hidden gateway Profile inherits the Agent's ACP runtime
  config through `inherit_agent_acp_runtime_options` on both create and refresh
  and keeps it through `with_acp_configured_models`. Without it an ACP Agent
  (`deepseek-harness` and the other non-`claude`/`codex` Agents) failed Profile
  creation with `acp_config_missing` and was silently skipped, so no ACP Agent
  could ever be routed through the gateway at all. The internal role keys must
  survive these writes: the agent-scoped update path strips
  `vibex.internal.profileRole`, which stops the Profile being recognised as the
  gateway Profile and makes the next refresh create a second one.
- **A gateway binding resolves its Secret to the local placeholder.** The hidden
  gateway Profile stores no credential by design: it forwards with the upstream
  Key it selects and ignores whatever the Agent sends. Resolving that absence as
  a real missing credential rejected every gateway-backed launch with
  `agent_projection_secret_missing`, so a session created on a route never
  started. `AgentProviderProjectionPlan::local_placeholder_secret` carries the
  placeholder for a binding whose `provider_options` mark the gateway role, and
  `resolve_and_materialize` uses it instead of the secret store. The marker lives
  on the **binding**, not on the shared Provider record, because "this Agent
  talks to its own gateway" is a property of the binding. An ordinary Provider
  with an unset Key keeps failing loudly.
- **The placeholder must be a legal credential, not a readable sentence.** The
  Agent validates the Key's *shape* before it builds a request, so the injected
  value is `GATEWAY_LOCAL_KEY` (`vibex-local-gateway`), while
  `GATEWAY_LOCAL_KEY_HINT` (`managed by Vibex`) stays display text on the stored
  credential summary. Sending the hint as the Key meant no request ever reached
  the gateway: DeepSeek Harness allows printable ASCII only and rejected the
  space with `INVALID_CREDENTIAL`, which the ACP layer's authentication
  vocabulary then reported as "Agent authentication is required" — pointing the
  user at a sign-in page that could not fix it. Any replacement placeholder has
  to stay inside printable ASCII; the test asserts that rather than the exact
  string, because nothing interprets the value and only its legality matters.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Group member names a model | `route_group_member_model_unexpected` |
| Group lists one Provider twice | `route_group_provider_duplicate` |
| Group has no Provider | Not served; absent from the gateway snapshot |
| Group has one Provider | Served; no failover is claimed |
| Group Providers share no enabled model | Not served; the editor shows the empty intersection |
| Request names a model the intersection lacks | `404` from the gateway, not a Provider call |
| Plain route member names no model | `route_member_model_invalid` |
| Group member names a Provider that does not exist | `model_route_member_provider_missing` |
| Budget already spent by user-authored routes | `model_route_limit_exceeded` |
| Budget "spent" only by materialised direct routes | Group saves; direct rows do not count |
| Drop a reference from an Agent's card | Binding removed; the group row survives |
| Delete from the Providers page | Record removed and every binding released |
| A route write without republishing the catalogue | Group is bound but absent from every session's picker |
| An ACP Agent's gateway Profile written without ACP config | `acp_config_missing`; the Agent silently stays off the gateway |
| The internal gateway role key stripped from the Profile | Next refresh creates a second Profile instead of updating |
| A gateway launch without the placeholder | `agent_projection_secret_missing` |
| An ordinary Provider with no Key | `agent_projection_secret_missing` (unchanged) |
| The gateway placeholder containing a character no header can carry | Agent reports `INVALID_CREDENTIAL`; the gateway never sees the request |

### 5. Good / Base / Bad Cases

- Good: two Providers both list `deepseek-chat`; the group offers
  `group/<id>/deepseek-chat` once, and `LeastUsed` picks the answering Provider.
- Good: one Provider in a group; the group offers its whole enabled catalogue
  through the gateway, which may translate the protocol on the way out.
- Base: one Provider lists `deepseek-chat`, `left-only`; the other lists
  `deepseek-chat`; the group offers only `deepseek-chat`.
- Base: two Agents reference one group; the Providers page lists both as its
  users, and the group stays in the tab after one of them drops its reference.
- Bad: offering the union and letting `RoundRobin` send `left-only` to the
  second Provider, which answers `404`.
- Bad: drawing every global group on each Agent's card and offering delete
  there, which made a shared record look like one Agent's property and let an
  Agent's page destroy another Agent's model source.

### 6. Tests Required

- `vibex-core` unit: intersection keeps only shared models; capabilities are
  the shared subset; a group member rejects a model; a duplicate Provider is
  rejected; `group/<id>/<model>` parses.
- `vibex-db` unit: a group round-trips with empty member `model_id`s; the
  budget count excludes a `Direct` row and includes a plain route and a group;
  a soft-deleted route frees its place.
- `vibex-config-switch` unit: a group saves while more than `MAX_ROUTES` direct
  rows exist; a group is not refused for its empty member model; a group naming
  a missing Provider is still refused; the thirty-third user-authored route is
  still refused with `model_route_limit_exceeded`; a bound group is absent from
  the Agent's Profile until the catalogue is republished and present after; an
  ACP Agent's gateway Profile accepts a new catalogue and reports the change;
  a gateway binding materialises the local placeholder while an ordinary Provider
  without a Key still reports `agent_projection_secret_missing`.
- `vibex-model-gateway` integration: a shared model is routable through the
  group and reaches a serving Provider; a model outside the intersection is
  refused with `404`.
- `vibex-agent-acp` unit: a group publishes one option per shared model and each
  option's model id is `group/<id>/<model>`.
- `vibex-desktop` unit: the Providers page has a route-group tab whose Add
  command opens the group editor and whose detail pane is the page's own
  subject; the Agent card lists only the groups that Agent references and its
  remove control unbinds rather than deletes; saving a group needs no Agent and
  binds only when opened from one; the `+` chooser offers existing groups and
  the new-group entry; the old routing section is not rendered; the group draft
  accepts one Provider and refuses none.

### 7. Wrong vs Correct

#### Wrong

```rust
// Offer every model any member serves; the strategy will pick one.
let models = members.flat_map(|member| member.models()).unique();
```

This makes the group an unreliable union: the chosen Provider may not serve the
model the session asked for.

#### Correct

```rust
let models = vibex_core::provider_group_models(&route, &providers);
let model_id = route.group_model_id(&models[0].model_id);
// The gateway resolves only models actually in the intersection.
```

## Scenario: Model-Level Route Groups

### 1. Scope / Trigger

- Trigger: the Providers page creates or edits a model-level route group, or an
  Agent picks the virtual model one publishes.
- Trigger: the gateway resolves `group/<route>/<model>`, orders a model group's
  members, applies a rule, waits for a first token, or retries under patience.
- Trigger: the Gateway page reads its dashboard, live routing card, or ledger
  decision trace.

### 2. Signatures

```rust
RouteKind::{ProviderGroup, ModelGroup, Direct}
ModelRoute {
  kind, agent_model_id: Option<String>, rules: Vec<ModelRouteRule>,
  classifier: Option<String>, effort: Option<String>, patience_ms: Option<u64>,
  sink: bool, unlists: bool, levels: Vec<String>,
  pick_member_id: Option<ModelRouteMemberId>,
}
ModelRouteMember { weight: u32, context_tokens: Option<u32>, first_token_timeout_ms: Option<u64> }
RouteStrategy::{Ordered, RoundRobin, LeastUsed, Smart, LowestLatency, Weighted, Manual}
RouteTimeWindow { days, start_minute, end_minute, zone }
RouteRuleContext { agent_id, context_tokens, has_images, requested_effort, compact, intent, workspace_root, local_weekday, local_minute }
simulate_model_route(route, context, rotation, observations, failed_member) -> RouteSimulation
GatewayRouteDecisionTrace {
    rule_index, rule_id, reason, classifier, classifier_error,
    intent, effort, strategy, patience_ms, retries,
}
```

### 3. Contracts

- **ModelGroup is additive; ProviderGroup does not change.** A provider group
  still names Providers and offers the intersection of their catalogues. A
  model group names `(Provider, upstream model)` pairs and exposes exactly one
  Agent-facing id, `agent_model_id`. Existing provider-group rows therefore read
  back unchanged, and `route.kind` is the discriminator.
- **The virtual id is the only address.** A model group is called as
  `group/<route>/<agent_model_id>`. The member's upstream model id is never
  addressable through the group: accepting it would let a caller bypass the
  composition. The id may not contain `/`, because the gateway splits the model
  path on the first slash.
- **"Same model" is a catalogue identity, not a spelling.** When the builder
  opens it asks for the canonical id of every enabled model in one batched
  lookup; the template groups by that id and keeps each Provider's own upstream
  id as the member. A model the catalogue does not know falls back to its model
  id, and a catalogue-less install therefore keeps the old same-id suggestion.
  One member per Provider, because the same Provider twice is not a failover.
- **A local model alias is a member handle, not a second catalogue row.** The
  Provider metadata `modelAliases` maps an agent-facing alias to one upstream
  entry id. A model-group member may name either id; the gateway resolves the
  alias to the upstream id only when it builds the outbound body, so the group
  and the Agent can use the handle while the vendor still receives its own
  model name. A classifier `provider/model` pair accepts the alias the same way.
- **A model group needs one enabled member, and each member needs a model.** A
  provider-group member must keep an empty `model_id`; a model-group member must
  fill it. A duplicate `(Provider, model)` pair, a zero weight, a zero member
  timeout, or a manual strategy with no picked member is refused at write time
  as well as at `validate`.
- **Weights are 1–100 and survive a strategy switch.** Under `Weighted` the
  cycle interleaves each unit round by round (3:1 reads A, B, A, A) and rotates
  its start by the request counter, so the first attempt is proportional
  instead of always the same member. Other strategies keep the value for the
  next switch.
- **Seven strategies are computed from local observations only.**
  - `Ordered` uses the configured order.
  - `RoundRobin` advances one member per request.
  - `LeastUsed` prefers the fewest locally observed tokens.
  - `Smart` orders by cooling, observed p50, inflight, then usage.
  - `LowestLatency` orders by p50, then p95.
  - `Weighted` uses the weighted cycle.
  - `Manual` uses only `pick_member_id`; it deliberately has no fallback.
  No strategy reads a price, balance, quota, or vendor allowance.
- **Rules are ordered and the first match wins.** A rule must name at least one
  condition and, unless it is a pause rule, the member to try first. `more`
  members follow in order; members the rule did not name keep the strategy's
  order, so a rule changes preference without removing a fallback. A pause rule
  is limited to time, Agent, folder, and except-folder conditions and refuses
  every request for the turn before any Provider is called.
- **A classifier is one small, real request, and only when it can decide
  something.** It runs once per prepared request when the route names a
  classifier and either a rule can match an intent or `effort = "auto"`. The
  request is a non-streaming Chat call with an eight-second timeout; it is not a
  gateway model call, so it never enters the request ledger or the inflight
  table. Its intent feeds rule matching; its effort replaces the Agent's
  requested effort unless a member pins its own. A timeout, a missing model, a
  missing ready Key, an unusable reply, or a non-success status leaves the
  default order in place and records `classifier_error` on the decision trace.
- **A rule time window is local unless the zone is `local`.** Weekdays are ISO
  `1..=7`, minutes are `0..=1439`, and `start > end` crosses midnight. The rule
  belongs to the weekday it names, so a Friday 22:00–02:00 window does not match
  Saturday's tail. A non-local IANA zone is stored but not evaluated yet; the
  rule does not match rather than silently using the wrong clock.
- **Rules match an estimated context, not a tokenizer count.** The gateway
  bounds the turn's text/tool size at roughly four characters per token and
  labels the result an estimate. Images, Agent id, requested effort, intent,
  compaction, folder fragments, and except-folder vetoes are exact matches.
- **Patience retries only recoverable failures.** 408/429/5xx, all-keys-cooling,
  and first-token timeouts qualify; auth, missing models, malformed bodies,
  wrong protocols, and translation failures do not. The schedule is
  2/4/8/16/30 seconds, bounded by `patience_ms` (None is off, maximum four
  minutes). Each retry re-prepares the call, which is what refreshes Key
  cooldowns and member rotation. A recoverable failure during that re-read
  continues the wait rather than turning it into an immediate refusal.
- **A cooldown override replaces the built-in rest for that failure kind.** The
  keys are the failure-kind ids that can cool a Key (`rate`, `upstream`,
  `proxy`, `credit`, `quota`, `other`); `auth` is rejected because a rejected
  credential does not heal with time. Values are one millisecond to one day.
  The override is read when a failure is recorded, so it applies from the next
  rest onward and does not change a Key already cooling.
- **Provider reliability P2 is the two confirmed behaviours.** AutoInclude is
  the shared Provider's `autoRefreshModels` flag: a model-list refresh adds
  newly discovered entries and never rewrites a row the user authored
  (capabilities, display name, enabled state), while a Provider without the
  flag keeps the replace-the-list refresh. Cooldown overrides are the policy
  above. There is no Provider budget, quota, or compatibility flag in this
  wave.
- **The first-token deadline can fail over, and the last member never times
  out.** When a member is not the last, the gateway reads its first body byte
  before committing; a timeout or an empty body is recorded as a skipped member
  and the next member is tried. The last member waits indefinitely because
  there is nowhere to go. The deadline is the member override or the route
  default, and only applies to streaming calls.
- **Unlists and Sink hide a model from lists, not from its id.** `unlists`
  removes the group's model from the gateway catalogue, Agent pickers, and the
  config-center catalogue; `sink` does the same for a composition that exists to
  be called explicitly. `group_for_model` still resolves the id, which is what
  makes an explicitly-addressed hidden route usable.
- **The gateway catalogue has one entry per virtual model.** A model group
  contributes one `group/<id>/<agent_model_id>` entry; its capabilities are the
  intersection of the capabilities of its reachable members. A model group whose
  members cannot be resolved contributes nothing rather than a broken entry.
- **The decision trace explains the order.** A rule match writes its zero-based
  index, stable id, reason, chosen effort, and strategy; a non-ordered strategy
  writes its id; patience writes its budget and retry count. The trace is
  additive to `GatewayRouteTrace`, ignores an unreadable payload like every
  other trace, and never contains a prompt or reply.
- **The live routing card consumes real events only.** It derives from
  `GatewayInflight` plus `Began`/`FirstToken`/`Ended`; the newest 24 attempts are
  drawn and the rest are aggregated. It never draws direct Providers, because
  they produce no gateway events. Its rows are a static, reduced-motion-readable
  equivalent of the graph, not random or simulated traffic.
- **The dashboard is local observation.** It reports uptime from the listener's
  start time, request count/QPS for the chosen window, success rate, p50/p95,
  input/output/cache tokens, failovers, inflight, and top Providers/models/errors.
  It has no cost, price, quota, or balance field.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Model group has no `agent_model_id` | `route_agent_model_missing` |
| `agent_model_id` contains `/` | `route_agent_model_invalid` |
| Model group has no enabled member | `route_group_no_enabled_member` |
| Two members name the same Provider and model | `route_member_duplicate_model` |
| Member weight is 0 or above 100 | `route_member_weight_invalid` |
| Manual strategy has no picked member | `route_manual_member_missing` |
| A rule names a member outside the route | `route_rule_member_unknown` |
| A pause rule also selects a member | `route_rule_pause_action_invalid` |
| Patience is zero or above four minutes | `route_patience_invalid` |
| A second live group publishes the same virtual id | `model_route_agent_model_conflict` / unique index |
| A request names a member's upstream id through the group | `404 model_not_found` |
| A matching pause rule applies | `503 model_gateway_route_paused`, no upstream call |
| First-token timeout with another member left | That member is skipped; the next is called |
| First-token timeout on the last member | The member keeps waiting |
| Non-recoverable upstream failure under patience | Refused immediately; no retry |

### 5. Good / Base / Bad Cases

- Good: two Providers serve different upstream models; a model group exposes
  `vibex-fast`, sends `deepseek-chat` to the first, and falls back to
  `kimi-k2` on the second without the Agent ever seeing either name.
- Good: a 3:1 weighted pair starts on the heavy member three requests out of
  four and still keeps the light member as the fallback.
- Base: a legacy provider group reads back with `agent_model_id = None`, no
  rules, default weights, and behaves exactly as before.
- Base: a rule matches "context > 100k" and names member B; A remains second in
  the order for failover, and the ledger names rule 2 in its decision trace.
- Bad: publishing the member's upstream model id as a second gateway entry;
  that bypasses `agent_model_id` and makes the group two incompatible sources.
- Bad: treating a first-token timeout on the last member as an error; there is
  no fallback, and the correct behaviour is to wait.

### 6. Tests Required

- `vibex-core` unit: a model group validates across different Providers and
  upstream models; it rejects a missing virtual id, a slash, no enabled member,
  a duplicate pair, an out-of-range weight, a manual pick that is absent, a rule
  with no condition, a pause rule with an action, and a cross-midnight window.
  `simulate_model_route` orders by strategy, reports a matched pause rule, and
  skips a simulated failure.
- `vibex-db` unit: a model group round-trips its virtual id, rules, levels,
  patience, sink/unlists, member weight/timeouts, and picked member; clearing
  those fields stores NULL/empty; the partial unique index refuses a second live
  group with the same virtual id; a provider group still round-trips unchanged.
- `vibex-config-switch` unit: a model group is created and edited through the
  service; a duplicate virtual id is refused with a readable validation error
  before the database constraint; a member whose Provider no longer serves the
  model is refused.
- `vibex-model-gateway` integration: a model group falls back across different
  upstream models; a matching rule changes the first member and the ledger trace
  names the rule; a first-token timeout fails over to the next member; patience
  retries a 503 and succeeds once the upstream recovers; a classifier supplies
  intent and automatic effort; a per-kind cooldown override replaces the
  built-in rest.
- `vibex-model-gateway` unit: the seven strategy ids round-trip; weighted
  rotation starts in proportion; manual uses only the picked member; smart and
  lowest-latency prefer health and p50; a paused rule returns
  `model_gateway_route_paused` without an upstream call.
- `vibex-desktop` unit: a new group opens as a model group; the editor, palette,
  and simulator expose the virtual id, model/alias palette, pipeline, policy,
  templates, suggestions, simulated failure, patience, and Unlists/Sink; the save
  path writes the model-group fields; the dashboard and live card read real
  gateway state and expose no money metric; AutoInclude adds a discovered model
  without rewriting an authored row; cooldown inputs write the policy map.
- Manual: 10k request rows, 20–30 concurrent inflight attempts, reduced motion,
  keyboard-only editing, and a legacy provider group that must not regress.

### 7. Wrong vs Correct

#### Wrong

```rust
// Offer the member's upstream model id as a second entry, so old callers work.
catalog.push(group.model_id(&member.model_id));
```

The Agent then names `deepseek-chat` directly and bypasses the group's virtual
id, its rules, and its attempted order.

#### Correct

```rust
let virtual_id = route.gateway_model_id()?; // group/<route>/<agent_model_id>
let members = route.enabled_members().collect::<Vec<_>>();
let plan = plan_members(&members, route.strategy, rotation, &observations, route.pick_member_id.as_ref());
```

## Scenario: Unified Agent Default Model Source

### 1. Scope / Trigger

- Trigger: a new session, scheduled task, or automation needs the model source an
  Agent starts on.
- Trigger: the Config Center reads or writes the default that used to live in
  `agent_default_model_provider_profiles` and
  `agent_model_route_bindings.is_default`.

### 2. Signatures

```rust
AgentModelSourceRef::Provider(ProviderProfileId)
AgentModelSourceRef::RouteGroup(ModelRouteId)

AgentModelSourceDefault {
    scope: ProviderProfileDefaultScope,
    agent_id: AgentId,
    source: Option<AgentModelSourceRef>,
    updated_at_ms: i64,
}

AgentModelSourceDefaultRequest { scope, agent_id }
SetAgentModelSourceDefaultRequest { scope, agent_id, source }
ClearAgentModelSourceDefaultRequest { scope, agent_id }
```

The stored table is keyed by `(scope_kind, scope_id, agent_id)`. `source` is
optional on a read because a read is a resolution, not a row lookup.

### 3. Contracts

- Precedence: explicit unified row, then the Agent's route default, then the
  scoped Provider default, then the Agent's sole binding, then `None`.
- A Provider source must be an Agent Profile that belongs to this Agent, is not
  deleted, is enabled, and is not an internal placeholder.
- A route source must exist, be enabled, have at least one enabled member, and
  be bound to this Agent.
- Writing a Provider source clears the Agent's route default; writing a route
  source clears the scoped Provider default. Both happen in one immediate
  transaction so no reader sees two defaults.
- Clearing removes only the explicit unified row; the legacy fallbacks remain.
- The retired `get/set_agent_model_provider_default` pair reads and writes the
  unified source for user-facing Providers. The hidden gateway Profile and the
  per-kind local fallback keep their legacy row because the launch path still
  reads it and they are machinery rather than a user default. The retired
  `set_agent_default_model_route` writes the unified route source in the same
  call as the binding's default flag, so the route the user chose is the one the
  unified read returns.
- The snapshot carries explicit rows only, not resolved fallbacks. A client that
  needs the answer calls the read API; it must not invent a row.
- A disabled or deleted source is never promoted by fallback. An explicit row
  that later becomes unusable is a data-repair case, not a new default.
- Repair deletes the stale unified row, never rewrites the legacy row: the
  Agent card offers "Clear saved default" for the Agent it describes, and the
  Advanced page lists every stale row in the current scope with the same
  action. Clearing lets the resolver fall back; it does not invent a new source.

### 4. Tests Required

- `vibex-core`: tagged camelCase wire shape; `None` source.
- `vibex-config-switch`: the retired Provider-default get/set pair reads and
  writes the unified source; the retired route-default setter makes the unified
  route source win over an explicit Provider source.
- `vibex-db`: a disabled explicit Provider source is ignored by resolution and
  falls through to a usable route default.
- `vibex-db`: unique row per scope/Agent; backfill precedence; double-default
  report; clear is idempotent.
- `vibex-config-switch`: read fallback order; Provider write clears route
  default; route write clears scoped Provider default; ownership and usability
  validation.
- `vibex-desktop`: Agent page shows one resolved default; global Provider and
  route pages do not show an object-level default.

## Scenario: Provider Advisory Metadata

### 1. Scope / Trigger

- Trigger: the Provider editor stores a models.dev catalogue id, icon key,
  category, tags, or the "only through route groups" visibility choice.
- The data is advisory: no launch projection reads it as a credential, command,
  protocol, or model declaration.

### 2. Signatures

```rust
ModelProviderMetadata {
    catalog_id: Option<String>,
    only_through_route_groups: bool,
    icon: Option<String>,
    category: Option<String>,
    tags: Vec<String>,
}

ModelProviderProfile.metadata: ModelProviderMetadata
```

`model_provider_profiles.metadata_json` stores the record; older rows read as
the empty value. The table keeps the column out of the launch contract.

### 3. Contracts

- `catalog_id` keys capability suggestions; a missing id means no suggestion,
  never a guessed one.
- `only_through_route_groups` hides the Provider from direct Agent pickers; the
  Provider still appears in route-group member choices.
- Metadata never overwrites a hand-typed model capability and never carries a
  price, cost, currency, or quota field.
- A create request that carries no metadata writes the empty record; the
  catalogue id may be added by a follow-up v2 update when it comes from a
  preset.

### 4. Tests Required

- `vibex-db`: metadata round-trips; older rows default; schema migration adds the
  column without rewriting rows.
- `vibex-core`: serde defaults and no money field.
- `vibex-desktop`: shared editor writes the metadata through the v2 update; a
  Provider marked only-through-groups is absent from the direct picker.

## Scenario: Shared Provider Discovery and Catalogue Status

### 1. Scope / Trigger

- Trigger: a shared Provider's detail page runs `Test` or `Refresh models`.
- Trigger: the Providers page shows or refreshes the advisory models.dev catalogue.

### 2. Signatures

```rust
ModelProviderFetchModelsRequest { model_provider_profile_id }
ModelProviderFetchModelsResponse { model_provider_profile_id, models, diagnostics }
ModelProviderTestRequest { model_provider_profile_id }
ModelProviderTestResult { model_provider_profile_id, status, code, message, diagnostics, checked_at_ms }

ModelCatalogStatus { availability: Fresh|Stale|Bundled|Missing, fetched_at_ms? }
```

### 3. Contracts

- Shared discovery resolves the first API-credential Secret from the v2
  credential references and builds a transient v1 Profile only to reuse the
  protocol-specific fetch path. The transient Profile is never persisted.
- A shared test is a model-list probe: it proves the endpoint, protocol, and
  Secret without sending a prompt or spending completion tokens.
- Existing configured Models are carried over by id when a refresh returns
  them, so capabilities and provenance survive.
- The catalogue status is advisory: `Bundled`, `Missing`, and `Stale` are normal
  states and never block Provider editing. A missing/corrupt cache reads the
  compiled fallback (`Bundled`) rather than returning no suggestions. A manual
  refresh failure leaves the previous document in place and reports the failure
  to the caller.
- The catalogue never carries a price, cost, currency, or quota field.

### 4. Tests Required

- `vibex-model-catalog`: status reports missing/fresh/stale and its fetch time.
- `vibex-config-switch`: shared discovery maps credentials/endpoints and carries
  existing model metadata; a missing Secret reports the normal missing-secret
  error.
- `vibex-desktop`: shared detail exposes Test and Refresh models; the Providers
  page shows catalogue freshness and a refresh control.


## Scenario: Provider List Favorites, Filters, and Sort

### 1. Scope / Trigger

- Trigger: the global Providers list is filtered, sorted, or a Provider is
  starred.

### 2. Contracts

- Search matches display name, endpoint URL, configured model id, and Key
  display name.
- Status filters: all, enabled, disabled, needs key, needs attention, shared
  (two or more Agents), and one Agent.
- Sort: name, recently updated, Agent count, and health. Favorites sort before
  every other key.
- Favorites are persisted as a JSON string array in
  `management-provider-favorites.json` beside the desktop UI state; a read or
  write failure is non-fatal.
- Health is the worst summary across the Provider's bound legacy Profiles; an
  unchecked Provider reads `Unchecked`, not `Healthy`.
- Row counts and Agent icons are read-only; binding changes stay on the Agent
  page.

### 3. Tests Required

- `vibex-desktop`: the favorite control, status filters, sort controls, and
  favorite file name remain wired; a 10+ Agent row wraps without changing the
  page width.

## Scenario: Two-Step Add Provider — Source Step

### 1. Scope / Trigger

- Trigger: the user adds a Provider from the global Providers page or an Agent
  page.

### 2. Contracts

- Step one is the source step; step two is the existing connection-and-model
  editor. There is no third step.
- Category chips are exactly Vendors / Relays / Local, multi-select, all
  selected by default. There is no "All" chip.
- A preset tile shows a logo (bundled SVG or monogram fallback), its name, and
  one detail line. Plan notes are badges, not categories.
- A local preset is one with `no_key`, a loopback endpoint, or a known local id
  (`ollama`, `lmstudio`). A relay is one of the reseller presets
  (`openrouter`, `opencode-go`, `clinepass`, `commandcode`). Every other preset
  is a vendor.
- When the step is reached from an Agent page, the scope control offers "Only
  this Agent" and "Share with Agents"; the choice drives the existing shared-vs-
  Agent save branch.
- "Import configuration" opens the existing native import preview for the
  selected Agent; "Custom provider" opens the empty form. Both stay on step one.
- Search matches the preset id, name, and endpoint host.

### 3. Tests Required

- `vibex-desktop`: all three categories classify known presets; search filters
  tiles; category selection never empties the set; the source step keeps the
  import and custom entries; the picker starts at step one for a new Provider.

### Source library sizing and recents

- The curated source library ships 40–60 presets and covers the top vendor set
  by id; it is not a flat dump of the models.dev long tail.
- Recent presets are a separate row above the grid, most-recent first, capped at
  six. They persist in `management-provider-recents.json` beside the desktop UI
  state; a read or write failure is non-fatal and never blocks adding a Provider.
- A preset choice is recorded before the draft is filled, so a failed save still
  leaves the source in recents.

## Scenario: Shared Provider Workbench Tabs

### 1. Scope / Trigger

- Trigger: a shared Provider is selected on the Providers page.

### 2. Contracts

- The workbench tabs are Overview / Models / Keys / Routing / Traffic /
  Advanced. The tab state is page state, not persisted per Provider.
- Overview owns the Agent binding list, the bind-another-Agent offer, and the
  shared editor entry. It does not duplicate Keys.
- Models lists the configured catalogue with enable/default/limit labels; it
  never renders a price column.
- Keys owns the credential list and per-Key status. Routing lists the route
  groups whose member set includes this Provider, with the member position.
- Traffic reads only observed `GatewayRequestSummary` and health rows: request
  count, success, tokens, and the aggregate health label. It never computes
  cost, quota, or balance.
- Advanced lists endpoint roles, proxy policy, header count, and the
  only-through-route-groups visibility flag; Delete lives only here.
- Editing remains one shared v2 write; an Agent-only field is reached from the
  Agent page, not silently written into v2.

### 3. Tests Required

- `vibex-desktop`: the six tab labels and tab bar remain wired; Delete is only
  rendered in Advanced; Models has no price header; Traffic has no cost/quota
  control.

### Models tab controls

- The Models tab owns a search field, filters (all/enabled/disabled/reasoning/
  vision), bulk enable/disable, and a per-row default star.
- A bulk visibility write goes through the same v2 update as an inline edit; a
  disabled default is replaced by the first enabled model, or cleared when none
  remain.
- The table never renders price, cost, quota, or balance. It may render model
  capability declarations and the default marker.

## Scenario: Route Group Workbench Tabs

- Tabs: Overview / Members / Policy / Traffic.
- Overview owns the Agent pills, bind-another-Agent offer, and Delete.
- Members lists the attempt order, Provider, model, enabled state, and fixed
  effort; editing still opens the group editor.
- Policy lists strategy, pin, failover, first-token timeout, context, and the
  local token ceiling. It never shows a remaining-token percentage.
- Traffic aggregates only observed Gateway requests across the group's member
  Providers; it shows request/success/token counts and no cost or quota.

### Gateway request ledger filters

- The recent-request ledger has a search field (Provider, model, Agent) and
  outcome filters: all, OK, failover, failed/cooldown.
- The visible list is capped at 50 rows per read; it is metadata-only and keeps
  the existing per-row trace detail.
- Clearing still deletes the whole ledger, not the filtered view; the button and
  its retention copy sit at the foot of the unfiltered list.

### Static gateway topology

- The topology card has three columns: Agents, Gateway, Route groups/Providers.
- Edges are static text/columns; the only live number is `gateway_inflight.len()`.
- Direct Provider edges are omitted; route-group membership is the drawn path.
- The card never animates traffic and never shows prompt, reply, Key value, cost,
  or quota.

### Agent default stars in binding pills

- A bound Agent pill on a shared Provider or route group carries a star.
- The star writes the unified `AgentModelSourceDefault` for that Agent; it does
  not change the object-level default and does not bind or unbind.
- A Provider source uses the Agent's legacy v1 Profile id, not the shared v2 id.
- A missing legacy Profile means no star is offered for that Agent.
- The star is read-only when a mutation is already in flight.

### Shared Key batch and per-Key controls

- The Keys tab supports adding one Key, pasting several (one per line), and
  per-Key enable/disable, prefer, replace, remove, and account limits.
- Batch add is sequential, not atomic: the report names how many landed and the
  first failure. A failed line leaves the earlier Keys stored.
- Batch input is a multiline text area; the lines are never echoed back after a
  successful write.
- Per-Key limits live in the gateway policy map keyed by credential id, not in
  the Provider credential reference.
- There is no balance, quota, or budget control.

### Advanced override metadata (v2)

- Advanced overrides live in `ModelProviderMetadata`, never in the launch
  contract: auth header mode, extra body JSON, User-Agent mode, TLS acceptance,
  connect/request/stream-idle timeouts, auto-refresh, and model aliases.
- The shared Advanced tab edits them as a JSON object. The object is validated
  before the v2 update; invalid JSON or an unknown auth/UA mode leaves the draft
  open and reports the error.
- `tlsAcceptInvalid` is refused for a catalogued vendor Provider; it is only for
  a custom or local record.
- `extraBody` must be a JSON object. `modelAliases` is an object of local id to
  upstream id.
- None of these fields may carry a price, cost, quota, or balance value.

### Pre-save connection test

- Creating a Provider can test the draft's URL and key before any row exists.
- The test uses the draft model-list request; it returns status, discovered
  model count, latency diagnostics, and a typed failure.
- A failed test leaves the draft intact and does not clear the key field.
- The saved-Provider Test still uses the saved credential and model-list probe.

### Detect APIs

- Detect APIs tries the four supported wire protocols in order against the
  draft URL and key: OpenAI Chat Completions, OpenAI Responses, Anthropic
  Messages, Gemini.
- The first protocol whose model-list request succeeds is written into the
  draft's `wireApi` option; the dialog is not saved.
- All four failing is a typed "No supported API answered" error; the draft and
  key remain.
- Detection is a sequence of draft fetches, so it uses the draft fetch API and
  never persists a transient Profile.

### Traffic percentiles

- Provider and route-group Traffic cards compute p50/p95 from observed
  `GatewayRequestSummary.latency_ms` values; missing latency is ignored and an
  empty sample renders `—`.
- The calculation is nearest-rank over observed values, not a vendor report or
  a money/cost projection.

### Interactive binding pills and removal guards

- Clicking a bound Agent pill removes that binding; clicking an unbound Agent
  pill binds the shared Provider or route group for that Agent.
- The default star stops propagation and writes only the default source.
- Removing the Agent's only model source is refused with a blocking message.
- Removing the current default source asks first and says the source record is
  not deleted; the Agent needs a new default afterwards.
- Provider unbinding deletes the Agent's legacy v1 Profile; the shared v2 record
  survives for every other Agent. Route unbinding removes only the reference.

### Gateway alert thresholds and default timeouts

- `GatewayPolicy` stores an optional error-rate warning percentage, latency
  warning milliseconds, and default connect/request/stream-idle timeouts.
- The error threshold is an integer percentage in `0..=100`; a timeout or
  latency threshold of zero is rejected.
- These are observability/fallback settings, not a vendor quota. They do not
  pause the gateway, compute cost, or show a balance.
- The desktop editor keeps unrelated policy edits carrying the stored values
  forward, like account limits and translation.

### Per-Key model scope and proxy

- A shared Provider credential may carry `models` (empty means every model) and
  `proxyUrl` (empty inherits the Provider/gateway proxy).
- The fields live inside the Provider's `credentials` JSON, so an older record
  reads empty/default fields and no schema migration is needed.
- The gateway skips a Key whose non-empty model scope does not contain the bare
  requested model, before it evaluates cooldown or limits.
- A per-Key proxy builds a one-attempt HTTP client; an unusable URL is logged and
  falls back to the gateway client rather than failing the request.
- The Keys tab edits both fields through a small JSON object and writes them in
  the same v2 profile update as the rest of the shared Provider.

### Single-Key Test

- A per-Key Test resolves that credential's own Secret and reuses the shared
  Provider's transient probe Profile.
- It is a model-list request: status, discovered model count, and latency; it
  never sends a prompt.
- A missing/expired Secret reports the normal missing-secret failure and leaves
  the Key in place.
- The result is correlated by the clicked credential in the UI; the backend
  result keeps the existing profile-level shape.

### Import preview, model paging, and list keyboard

- The source step can run a native import scan before importing; the scan stores
  the preview, the panel shows the importable count, and Apply runs the existing
  import path. A successful import leaves a dismissible report with imported
  and missing-secret counts.
- The shared Provider Models tab is windowed to 50 filtered rows with previous/
  next controls and a `first-last / total` range. This is bounded rendering, not
  a nested virtual scroll; filters clamp the page.
- The Provider list container is focusable: Up/Down move the selection,
  Home/End jump to the edges, and Enter leaves the current selection. Escape and
  search shortcuts keep their existing paths.
- Per-Key proxy URLs are validated for http/https/socks5/socks5h; the policy
  editor offers System, 7890, and 1080 shortcuts that rewrite only `proxyUrl`.

### Per-Key proxy connectivity preview

- `Test saved proxy` resolves the Key's saved proxy URL and sends a GET to the
  Provider's primary API endpoint through it.
- Any HTTP response proves the proxy reached the endpoint; status code and
  latency are reported. A transport failure reports a failed proxy test.
- The probe carries no credential, prompt, or response body.
- The proxy policy editor validates the URL scheme and offers System / 7890 /
  1080 shortcuts; the test always uses the saved value, not the unsaved draft.

### No export entry, traffic ranges, request paging

- The Advanced page no longer hosts a native export card. The native export
  functions remain unreachable from this surface; the product exposes import
  only.
- Provider and route-group Traffic cards share a 1h/24h/7d range and show the
  most frequent HTTP error status in the window, alongside request/success/
  token/p50/p95 values.
- The Gateway request ledger is filtered first, then paginated at 50 rows with a
  range label and previous/next controls; the clear action still clears the
  whole ledger, not the page.

### Inline Model editor

- Clicking a Models-tab row expands an inline editor; the default star stops
  propagation and does not toggle the row.
- The editor edits context/output tokens and four tri-state capabilities
  (image, PDF, temperature, reasoning); there is no price/cost field.
- Context/output are parsed as optional positive whole numbers and written into
  the same v2 profile update as the rest of the shared record.
- Reasoning off also clears fixed reasoning efforts, matching the Agent model
  editor's declaration rule.

### Virtual Models table, endpoint probes, field controls, row keyboard

- The Models tab renders through `v_virtual_list` with fixed row heights and an
  expanded row height for the inline editor. It no longer pages.
- Endpoint latency probes GET each saved endpoint through the first Key's proxy,
  order passing probes by latency, and `Use fastest` moves the fastest API
  endpoint to the front of the profile before a v2 update.
- Advanced exposes auth-header, User-Agent, TLS, and auto-refresh as direct
  controls; timeouts, extra body, and aliases remain in the JSON editor. TLS is
  disabled for catalogued non-local Providers.
- Provider, Model, and Gateway Request rows are focusable; Enter opens the same
  detail the click opens, while the default star stops propagation.

### Advanced metadata consumption and request virtualization

- The model gateway now consumes per-Provider auth header mode, extra body JSON,
  User-Agent pass-through, TLS acceptance, connect/request timeouts, and model
  aliases.
- `autoRefreshModels` remains UI metadata; model refresh is still an explicit
  action.
- Auth modes override the protocol default; Anthropic keeps its version header.
- Extra body is a shallow top-level object merge; it cannot replace the model
  field except through an alias map.
- The gateway request ledger renders through `v_virtual_list` with a fixed
  collapsed row height and a larger expanded trace height; filtering happens
  before the list, and the clear action still clears the whole ledger.

### Source tile badges

- A source tile shows one badge derived from its shape: Plan, Region, Local,
  or Official. The badge is separate from the three category chips and never
  becomes a fourth category.

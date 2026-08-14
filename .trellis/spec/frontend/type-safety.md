# Type Safety

Frontend types must model Vibex protocol contracts, not provider-native SDK
payloads. Use discriminated unions for timeline items, permission requests,
capabilities, and remote events.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md),
shared Rust DTOs, and protocol golden tests. GPUI Desktop and native mobile consume
Rust contracts directly; the unused TypeScript compatibility package was retired
after the React/Tauri cutover.

## Type Ownership

Protocol types belong to the established Rust domain/protocol owner. GPUI feature
modules may define local view models, but must not redefine wire or Backend contracts.
Do not add a language-specific binding package or code-generation dependency only
as evidence. A future non-Rust consumer requires an explicit schema/code-generation
design tied to that consumer.

## Timeline Types

Model timeline items with a discriminant such as `kind`:

```ts
type AgentTimelineItem =
  | { kind: "user_message"; sequence: number; /* payload */ }
  | { kind: "agent_message"; sequence: number; /* payload */ }
  | { kind: "tool_call"; sequence: number; /* payload */ }
  | { kind: "permission_request"; sequence: number; /* payload */ }
  | { kind: "system_notice"; sequence: number; /* payload */ };
```

Every renderer switch over timeline kind should be exhaustive.

## Capability Types

Runtime selectors use the generated `SessionRuntimeOptionCatalog` and
`SessionRuntimeSelection` contracts. Model, Effort and Mode options must be
derived from the selected catalog option (or negotiated session config), not
from provider-kind constants or static pseudo-capabilities. Preserve the
catalog availability state so unavailable combinations cannot be submitted.

Provider capability data should answer whether an operation exists before UI
shows it as available:

- Session persistence.
- Session listing.
- Dynamic modes.
- Model list.
- MCP servers.
- Slash commands.
- Skills.
- Reasoning stream.
- Plan support.
- Tool invocations.
- Permission requests.
- Image input.
- File attachments.
- Fork/rollback.
- Terminal activity hooks.

Unsupported capabilities should render disabled states or fallback messaging,
not hidden provider-specific crashes.

## Preset and Engine Types

Keep detected execution engines, Provider Profiles, and Assistant/Workflow
Presets as separate types:

- Detected engine: what local binaries or backends are available.
- Provider Profile: credentials, endpoint, model/provider defaults, and
  injection strategy.
- Assistant/Workflow Preset: task-facing defaults that may reference Provider
  Profiles, MCP, Skills, prompts, permission modes, and disabled tools.

Do not copy secrets or provider-native config blobs into preset view models.
Scheduled tasks, delegation flows, and remote new-session UI should pass preset
ids plus explicit overrides through typed protocol contracts.

Provider-specific message variants such as Codex or ACP tool-call payloads must
be normalized at the protocol boundary. UI renderers should merge streaming text
and tool-call updates by Vibex ids, not by provider-native event names.

## Runtime Validation

Validate inbound remote payloads at protocol boundaries, especially for:

- Pairing and device auth.
- WebSocket event envelopes.
- Chunked file or screenshot transfer.
- Provider import/export forms.
- Plugin, MCP, Skills, and prompt metadata from external sources.

Validation should produce structured errors that can map to backend error codes.

## Forms

Provider, MCP, Skills, prompt, and device permission forms should have explicit
form schemas. Secrets must use secret-specific field types and redacted display
models.

## Forbidden Patterns

- Do not use `any` for protocol payloads.
- Do not cast raw provider events into UI timeline types.
- Do not duplicate Rust enum variants manually in multiple feature folders.
- Do not store redacted display values back into secret fields.

## Scenario: Turn Attribution Header Projection

### 1. Scope / Trigger

- Trigger: Desktop groups authoritative `TimelineItem[]` into user/Agent turns and may display the execution source of
  a response turn.
- The backend Agent Session Protocol owns audit/fence semantics. Frontend code owns only a safe, generated display
  projection and must not infer runtime identity from current selectors.

### 2. Signatures

```typescript
type TurnExecutionAttributionView = {
  agentLabel: string;
  providerProfileLabel: string;
  modelLabel: string;
};

type TimelineItem = {
  executionAttribution?: TurnExecutionAttributionView | null;
  payload: TimelinePayload;
};

consistentTurnExecutionAttribution(responseItems)
  -> TurnExecutionAttributionView | null;
```

### 3. Contracts

- Consume both canonical Rust types through the shared Backend projection; feature
  code must not redefine, cast, or augment the transport shape with
  binding/generation/native fields.
- Derive attribution once at the turn-grouping boundary. Ignore missing views for compatibility, require every present
  view to match all three labels, and return `null` on any conflict.
- Adjacent live text/reasoning compaction preserves the consistent view. It must stop merging when two present views
  differ so compaction cannot hide an execution-source boundary.
- A non-null turn view renders compact `Agent · Provider Profile · Model` metadata before the existing response body.
  `null` keeps the legacy layout without a placeholder or guessed selector value.
- The metadata header is orthogonal to payload dispatch. FileOperation, Command, WebSearch, TodoUpdate,
  Collaboration, ImageGeneration, Permission, Plan, Reasoning, and final Agent content keep their canonical renderers.

### 4. Validation & Error Matrix

- All response items omit attribution -> `null`, render legacy turn.
- Some response items omit attribution and all present views agree -> render the agreed safe labels.
- Any two present views differ -> `null`, hide the header rather than selecting the first/last source.
- Empty pending turn -> `null`, keep existing thinking indicator.
- Unknown provider-native payload or internal identity field -> do not parse/render; backend generated types remain the
  only contract.

### 5. Good/Base/Bad Cases

- Good: plan, command, permission, streamed text, and final answer share one view; the header renders once while each
  payload keeps its dedicated component.
- Base: an imported/legacy turn has no view and looks exactly as it did before attribution support.
- Bad: render `currentProfile.name` or Composer Model above historical turns after a runtime switch.
- Bad: read `bindingId` through `unknown as`, or collapse semantic event cards into generic ToolCall text to attach
  metadata.

### 6. Tests Required

- Generated binding drift check asserts the optional safe view exists and contains only the three label fields.
- Frontend typecheck/lint and Desktop build cover generated imports, mock Timeline parity, and exhaustive payload
  renderers.
- Browser mock data should include one attributed response turn and retain at least one legacy/unattributed item.
- Backend/core serialization tests remain the security assertion that binding/generation/native fields cannot reach
  this UI contract.

### 7. Wrong vs Correct

#### Wrong

```typescript
const source = `${selectedAgent} · ${selectedProfile} · ${selectedModel}`;
```

This labels history from mutable Composer state rather than the runtime that accepted the turn.

#### Correct

```typescript
const source = consistentTurnExecutionAttribution(turn.responseItems);
return source ? `${source.agentLabel} · ${source.providerProfileLabel} · ${source.modelLabel}` : null;
```

## Scenario: Descriptor-Driven Agent Provider Binding Editor

### 1. Scope / Trigger

- Trigger: rendering or refreshing Provider binding controls in GPUI Desktop or
  native mobile.
- The editor consumes the backend's version-matched capability and redacted
  preview. It never parses a Provider-native options blob or invents support
  from Agent/catalog metadata.

### 2. Signatures

```rust
AgentProviderBindingEditorState {
    capability: Option<AgentProviderProjectionCapability>,
    preview: Option<AgentProviderProjectionPreview>,
    draft_revision: u64,
    secret_touched: bool,
    secret_clear: bool,
}

ProjectionCredentialSurface =
    ApiKey | OAuth | Cloud | AgentManaged | Local |
    ServiceMarketplace | Unsupported

AgentProviderBindingEditorState::{
    replace_capability,
    replace_preview,
    mark_draft_changed,
    set_secret_intent,
    credential_surface,
    wire_api_choices,
    accepts_wire_api,
}
```

### 3. Contracts

- `AgentProviderProjectionCapability.form_controls` is the only source for
  visible Provider controls. API key, OAuth, AWS/GCP/Azure/Snowflake,
  Agent-managed, local, marketplace, and unsupported states use distinct
  semantic surfaces.
- Wire choices come only from descriptor model interfaces marked
  `user_selectable`. Codex `0.146.0` displays/accepts Responses only; Claude does
  not receive a fake Wire API menu.
- The shared editor state contains no Secret value. A blank Secret control means
  no mutation until explicit user input sets `secret_touched`; clearing requires
  both `secret_touched=true` and `secret_clear=true`.
- Native Desktop may keep the resolved Provider Profile API Key transiently in
  its local masked `InputState` so the standard eye toggle can reveal it. This
  does not add the Secret to `AgentProviderBindingEditorState`, snapshots, Debug,
  notices, Web, or mobile. The async read is fenced by the exact Agent/Profile
  scope, programmatic `set_value` leaves `secret_touched=false`, and dialog close
  cancels the read and clears the input.
- Capability/preview refresh replaces authoritative read state but preserves
  `draft_revision` and Secret intent. A background query must not reset an open
  draft or turn an untouched blank field into deletion.
- Preview renders only descriptor/version, bounded command/target summaries,
  restart behavior, verification state, and redacted values supplied by the
  backend. UI code does not reconstruct env, overlay content, native paths, or
  fingerprints.
- Desktop is the visual authority, but Desktop/Web/Mobile share this controller
  and typed DTO. Remote clients may query capability/preview and must surface
  stable private-boundary errors for entity/Secret mutations.

### 4. Validation & Error Matrix

- No capability yet -> empty/disabled editor state; do not infer controls.
- Capability is unsupported/unverified/version-mismatched -> explicit status
  surface and no automatic Secret action.
- Submitted Wire API is absent from `wire_api_choices` -> reject before mutation;
  backend remains authoritative with `agent_model_interface_unsupported`.
- Background capability refresh while draft is dirty -> preserve draft and
  Secret intent.
- Saved Secret read completes for a closed or different Agent/Profile editor ->
  ignore it and do not populate the current input.
- Remote raw entity/Secret action -> show the structured
  `remote_*_private` / `remote_provider_secret_mutation_unavailable` recovery
  state; never fall back to a local mutation.

### 5. Good/Base/Bad Cases

- Good: an API-key descriptor shows a Secret-specific control and sends a
  separate Secret mutation only after the user edits or explicitly clears it.
- Good: native Desktop loads a configured API Key into a masked input; the eye
  toggle reveals the original value, while saving another field leaves the
  Secret mutation untouched.
- Good: AWS and local descriptors show cloud/local controls rather than an API
  key field with a different label.
- Base: an unknown Agent shows an unverified state with no selectable model
  interface.
- Bad: branch on `agent_id == "codex"` in each client to build controls.
- Bad: preserve a redacted placeholder in a field and submit it as a new Secret.
- Bad: render a fixed `***` placeholder for a configured Provider Profile key;
  the eye toggle cannot reveal the original value.
- Bad: deserialize `provider_options` in GPUI to construct env or overlay files.

### 6. Tests Required

- Shared UI tests map all eight credential kinds to semantic surfaces and assert
  unsupported states expose no credential input.
- Shared UI and Desktop tests assert capability refresh preserves draft and
  `secret_touched`/`secret_clear` intent.
- Desktop Management tests assert that the editor calls the local Secret-value
  read, writes the response into the masked `InputState`, keeps the eye toggle,
  and rejects stale Agent/Profile callbacks.
- Codex tests assert Responses is accepted and Chat is absent/rejected in the
  editor and at the backend validation boundary.
- Backend parity tests cover Native, Disconnected, and WebRemote capability
  behavior. Remote tests assert entity/Secret methods stay private and previews
  contain no Secret env, overlay content, or native path.
- Run `cargo test -p vibex-ui --locked`, Desktop tests, WebRemote tests,
  `pnpm check:frontend`, and `pnpm lint`.

### 7. Wrong vs Correct

#### Wrong

```rust
if agent_id.as_str() == "codex" {
    show_api_key = true;
    wire_choices = vec![Responses, ChatCompletions];
}
```

#### Correct

```rust
let surface = editor.credential_surface();
let wire_choices = editor.wire_api_choices();
let preview = editor.preview.as_ref(); // already bounded and redacted
```

For the native Desktop Provider Profile editor only:

```rust
// Wrong: there is no original value for the eye toggle to reveal.
state.set_placeholder("***", window, cx);
state.set_value("", window, cx);

// Correct: keep the resolved value local and masked by default.
state.set_masked(true, window, cx);
state.set_value(secret.value.unwrap_or_default(), window, cx);
profile_secret_touched = false;
```

The exact descriptor determines the controls once, and every client consumes
the same typed capability.

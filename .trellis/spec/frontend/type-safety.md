# Type Safety

Frontend types must model Vibex protocol contracts, not provider-native SDK
payloads. Use discriminated unions for timeline items, permission requests,
capabilities, and remote events.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md),
shared Rust DTOs, and protocol golden tests. GPUI Desktop and GPUI-WASM consume
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

# State Management

Frontend state is split into authoritative server state, local UI state,
streaming presentation buffers, and persisted preferences. Keep this separation
strict so remote reconnect and multi-device control remain reliable.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md), `desktop-model`, and current
runtime/remote tests. React Query/Zustand/localStorage/Tauri sections below are
pre-cutover historical evidence, not maintenance contracts. Their deleted paths,
commands, DTOs, and test instructions must not be restored or executed as current
architecture. New GPUI code uses Backend-driven Controllers and versioned Rust UI
state.

## State Categories

Server state:
- Projects, workspaces, sessions, timelines, files, Git state, terminals,
  Provider profiles, MCP, Skills, devices, audit logs, and health checks.
- Owned by DesktopRuntime and projected through NativeBackend/WebRemoteBackend.

Local UI state:
- Open panels, selected tab, split sizes, collapsed cards, command palette
  visibility, active right rail, and modal visibility.
- Managed by shared Rust View/Controller state with one reducer/owner per domain.

Streaming presentation buffers:
- In-progress Agent deltas.
- Terminal output windows.
- Tool progress display.
- Upload/download chunks.
- Managed with bounded buffers and sequence-aware reconciliation.
- A `TerminalFrameBatch` with `reset_required = true` invalidates the current
  incremental Terminal parser/buffer before any returned frames are applied. Treat
  those frames as the new base and adopt the batch's `next_sequence`; never append
  across a server cursor rewind, an evicted frame gap, or a dropped-frame increase.

Persisted preferences:
- Theme, layout sizes, sidebar collapsed state, recent projects, local display
  settings, and safe non-secret UI preferences.
- Interface font and code font settings are local UI preferences.
  Store a selected font family name from the locally detected/fallback option
  list plus bounded numeric font size and font weight values. Do not store
  arbitrary CSS strings or backend-synced configuration.

## Authoritative Timeline

The backend timeline is authoritative. Client state may keep optimistic or
partial items for responsiveness, but those items must reconcile against
server-assigned sequence numbers.

Timeline presentation state should distinguish:

- `persisted`: rows confirmed by the backend with session sequence numbers.
- `optimistic`: local user actions awaiting mutation response or replay match.
- `streaming`: in-progress Agent/tool/terminal chunks that are not yet a final
  persisted item.

Optimistic user turns need client message ids so replayed cross-device echoes
can deduplicate them. Streaming buffers must be bounded and replaceable by the
persisted item when the authoritative timeline catches up.

When the reader is not following the timeline bottom, the unread badge counts
new non-empty Agent message rows only. Streaming deltas that reconcile into the
same row count once; reasoning, plans, tools, permissions, and other activity
rows do not increment the badge.

Timeline virtualization caches are presentation state, not authoritative
content. When a non-stream event appends structured content to the latest
projected turn, rebuild that projection before sizing, invalidate the turn's
previous measured height, and recompute its estimated row size. Reusing a
measurement from the turn's earlier structure can clip newly added command,
file-operation, image-generation, collaboration, or permission cards. Structured
timeline cards whose outer containers clip overflow must also opt out of flex
shrinking until intrinsic measurement converges. This applies to standalone
permission cards and permission footers embedded in command cards. Compound
command/permission rows must be estimated and measured as one rendered element.
Regression coverage for this path must assert both last-turn height invalidation
and the non-shrinking card container.

### Convention: Streaming Markdown height ownership

`MarkdownView` parses streaming input in the background, so the authoritative
row body can be newer than the document currently laid out on screen. During the
append-only Agent-message fast path, keep the virtual turn at its current extent
and preserve any pending intrinsic measurement. This applies equally to a
`FinalAnswer` conclusion row and to commentary or unknown-phase Agent text kept
in process history. Do not resize the virtual row from character counts or
Markdown syntax in the newer source.

While the same streaming row remains active, intrinsic measurements may grow but
must not shrink the turn: incomplete Markdown syntax can temporarily reparse into
a shorter document and otherwise make bottom-follow bounce in both directions.
A text-only event that changes presentation structure, such as commentary moving
to the conclusion or the final message reconciling the stream, keeps the current
virtual extent for that frame but invalidates the old intrinsic measurement. Once
the new structure is laid out, prepaint owns its next extent and bottom-follow
scrolls against that measured extent once.

```rust
// Wrong: the virtual row expands before MarkdownView renders the new document,
// then contracts to the old prepaint measurement and visibly bounces.
virtual_height += estimate_height(delta);

// Correct: source update -> background parse -> intrinsic measurement -> scroll.
estimated_heights.insert(turn_id, (signature, current_virtual_height));
measured_height = previous_measured_height.max(measured_height); // same streaming row only
```

The same ownership rule applies inside `MarkdownVirtualFlow`. A long streaming
document is reparsed as each accepted snapshot arrives, but replacing the
document must not reset every top-level block from a measured height back to its
estimate. Carry the virtual layout width and the previous block measurements
across an append-only source update: reuse a block with the same stable `NodeId`
and reuse the final block when its kind, start offset, and old source are a
prefix of the new source. Keep a per-block measured flag so an already measured
block can grow but cannot shrink while `MarkdownViewOptions::streaming` is true;
after streaming ends, the normal measurement path may shrink it to the final
intrinsic height. Streaming Agent documents enter block virtualization before
the large-document threshold once they have at least 8 top-level blocks and 8
KiB of source, so short structured answers keep the full renderer while long
answers do not switch rendering modes only after substantial content is already
laid out.

```rust
// Wrong: every background parse throws away the visible block geometry.
self.virtual_block_sizes = estimate_all_blocks(&document);

// Correct: stable prefix and the continuing tail keep their current extent;
// prepaint measures the new source and grows it when the rendered block grows.
restore_append_only_virtual_layout(previous_layout);
height = if streaming && block_was_measured {
    previous_height.max(measured_height)
} else {
    measured_height
};
```

Regression coverage must assert that the streaming fast path neither clears
pending turn heights nor invalidates a measured height, and does not mutate the
virtual row-size vector. Cover both conclusion and process-history Agent text,
the non-shrinking same-row measurement rule, and the text-only structural
transition path that bypasses estimated height. Non-text structured events
continue to use the full invalidation contract above.
For long Markdown, also assert that an append-only document update preserves
the measured prefix and continuing tail, that repeated streaming measurements
are monotonic, and that the final non-streaming measurement can converge
downward.

### Convention: Existing-session composer drafts

Unsent Composer state is local presentation state scoped by `VibexSessionId`.
Preserve the text, inline attachments, and selected command entry together: save
the active draft before changing sessions and restore the target session's draft
before rendering its Composer. A draft must never migrate into another session.

Remove empty drafts, consume the active draft when it is sent, and delete stored
drafts with their sessions. Regression coverage must switch between at least two
sessions with different unsent text, assert that each Composer shows only its own
draft, and assert that returning to the first session restores its original
content.

When a new-session draft includes an initial message, await session creation
only long enough to obtain the authoritative session record. Commit and select
that session immediately, attach its timeline view, and submit the initial
message in the background. The new-session page must not remain open until the
Agent turn finishes; streamed timeline events and the final authoritative
refetch belong on the selected session page.

Once that authoritative session exists, the submitted draft remains consumed
even if its initial Agent turn fails. Keep the persisted user message and error
timeline authoritative, and let manual or automatic continuation resume the
unfinished turn; never copy the submitted text or attachments into the selected
session composer. Restore the new-session draft only when session creation
itself fails before an authoritative session record is returned. Regression
coverage must assert both sides of this failure boundary.

Session fork follows the same authoritative-first rule. As soon as the backend
reports the persisted `initializing` fork and copied Timeline prefix, select that
session and show explicit runtime-preparation feedback while final ACP setup
continues. Do not invent a local placeholder session, and do not attach an Owner
runtime concurrently with the backend's initial materialization.

After reconnect, discard or reconcile optimistic state based on authoritative
fetch. Do not assume the client saw every live event before disconnect.

Timeline restore after app restart, session switch, or remote reconnect must
rebuild a complete authoritative prefix before rendering it as a full
conversation. Do not use a latest-window fetch (`afterSequence = null`) as the
session history cache: if `hasOlder` is true, a long single turn may be missing
the user message and early assistant deltas. Page forward from sequence `0`, or
from an existing cache known to start at sequence `1`, until `hasNewer` is
false.

Agent continuation after an unfinished turn uses a hidden provider prompt and
may not append a visible `user_message` boundary. Timeline view-model builders
must therefore treat persisted `error` items as completed historical segments,
and split any later Agent/provider output into a new display turn even when no
new user message exists. While the continue/send mutation is pending, the UI may
derive a presentation-only `running` state from the authoritative `idle` or
`error` session state so the interruption banner closes immediately and a
pending Agent indicator renders. Do not write this presentation state back to
the session cache. Show the continue banner for an `Idle` or `Error` session
only after authoritative timeline reconciliation confirms that the latest
conversational segment does not end in an explicit final Agent message.

## Scenario: Live Reasoning As Turn Progress

### 1. Scope / Trigger

- Trigger: an ACP Agent streams non-final reasoning while a conversation turn is
  running. The authoritative timeline persists that payload, while the turn view
  needs a replaceable loading label rather than a permanent process row.

### 2. Signatures

```rust
TimelinePayload::Reasoning(ReasoningPayload { text, is_final })

TimelineConversationTurn {
    live_status: Option<String>,
    process_rows: Vec<TimelineRow>,
    complete: bool,
}
```

### 3. Contracts

- `desktop-model` owns this projection. The GPUI renderer must not inspect ACP
  `session/update` or `agent_thought_chunk` payloads.
- The latest non-empty `Reasoning { is_final: false }` timeline item becomes
  `live_status` while the turn is incomplete and is removed from
  `process_rows`. Select from authoritative items rather than a merged row body,
  so multiple adjacent status updates do not accumulate in the loading label.
- A pending permission label takes precedence over `live_status`; otherwise
  `live_status` replaces the localized default `Thinking...` label.
- Treat `live_status` as Markdown-formatted provider text and project it through
  the shared Markdown parser before rendering the loading label. Progress labels
  render as plain text with exactly one ASCII `...` suffix.
- Empty, whitespace-only, or formatting-only progress text falls back to the
  localized `Thinking...` label. An incomplete turn with an empty streaming
  conclusion row must keep showing that fallback instead of rendering a blank
  response area.
- The projection is cleared when the turn completes, so non-final reasoning does
  not remain in conversation history.
- `Reasoning { is_final: true }` remains a historical process row. Tool, command,
  file, plan, and final Agent content keep their existing canonical renderers.

### 4. Validation & Error Matrix

- Empty or whitespace-only non-final reasoning -> ignore it and use the default
  loading label.
- Markdown-decorated or formatting-only reasoning -> render its parsed plain
  text, or the localized default when parsing produces no visible content.
- Empty streaming conclusion while the turn is incomplete -> keep the progress
  indicator visible with the localized default label.
- Multiple non-final reasoning segments -> show the latest item and
  retain no non-final reasoning process rows.
- Pending permission plus live reasoning -> show the localized waiting-for-
  confirmation label.
- Completed turn -> `live_status = None`; keep only final reasoning history.
- Authoritative last-session state is Idle/Error/Closed/Archived without final
  Agent text -> complete the turn and clear the loading label.

### 5. Good/Base/Bad Cases

- Good: `Planning targeted extraction` replaces `Thinking...` during the turn,
  then disappears when the final answer arrives.
- Base: an Agent that emits no reasoning keeps the localized default loading
  label.
- Bad: the View parses `agent_thought_chunk`, or completed history retains every
  transient reasoning status as a separate row.

### 6. Tests Required

- `vibex-desktop-model` asserts active projection, latest-status selection,
  process-row filtering, completion cleanup, and final-reasoning retention.
- `vibex-desktop` asserts progress suffix normalization and localized waiting
  copy.

### 7. Wrong vs Correct

#### Wrong

```rust
TimelineRowKind::Reasoning => render_process_row(row)
```

#### Correct

```rust
let label = turn.live_status.as_deref().unwrap_or(strings.agent_pending_response);
render_agent_thinking_indicator(&agent_progress_label(label), cx)
```

## Scenario: Stream The Conclusion After Collapsing Process Activity

- `AgentMessageDelta.phase` is the provider-neutral transition signal. Keep
  `Commentary` and unknown-phase Agent text in process history even when that
  text is the current turn tail. Do not collapse process activity for status
  updates, implementation notes, or other intermediate Agent descriptions.
- When the first `FinalAnswer` delta arrives, project that phase into a separate
  streaming `conclusion_row` and collapse process activity before rendering the
  text. Adjacent final-answer deltas append to that row through the bounded
  streaming cache.
- Do not wait for `AgentMessage { is_final: true }` to expose the conclusion.
  The final item reconciles and completes the final-answer row. Providers that
  cannot supply a trustworthy phase retain process presentation until the
  authoritative final item or completed-turn fallback arrives.
- Phase transitions fence Agent text compaction. A final-answer row must not
  include preceding commentary, and the renderer must never infer phase from
  message wording. Explicit commentary is never promoted by the completed-turn
  compatibility fallback; when a turn ends without a final answer, keep that
  text as completed process history and expose no conclusion row.
- An explicit user expansion overrides the collapsed default. Copy, fork, and
  timestamp actions remain hidden until the conclusion row stops streaming.
- Model tests cover commentary-at-tail, commentary-to-final transition, legacy
  fallback, and later-process behavior. Desktop tests cover collapse defaults,
  explicit expansion, and rejection of commentary by the conclusion fast path.

## Provider State

Provider state displayed in the UI should come from Vibex Provider Profiles,
session bindings, capabilities, and health records. Do not mirror native config
files directly into long-lived UI state. Native config import/export is an
explicit action with preview and rollback metadata.

New-session Agent selection is draft-local UI state until the session is
created. Switching an Agent chip on the new-session home may update local
`agentId`, provider profile, model, and command-context derivations, but it must
not immediately write to the global/current-session provider state. Commit the
selected provider only through the create-session draft.

## Derived State

Compute derived badges from server data:

- Project status: Agent running, terminal running, pending permission, Git dirty,
  or sync disconnected.
- Session status: state plus pending permission or failed turn.
- Provider status: selected profile plus latest health probe.

Do not store derived badges as independent mutable state unless there is a clear
cache invalidation path.

## Anti-Patterns

- Do not put authoritative backend data in a global presentation store; reconcile
  it through the injected Backend and domain Controller.
- Do not keep secrets in frontend persisted stores.
- Do not let mobile clients maintain their own session truth.
- Do not update multiple local stores from the same event with duplicated
  switch statements.

### GPUI Parent / Child Entity Updates

When a parent GPUI entity updates a child entity, the child update closure must
not synchronously `read` or `read_with` the parent. The parent entity is already
leased for update, so a reverse read panics at runtime. Snapshot every required
parent-owned value before entering the child update and pass those values into
the child method.

```rust
// Wrong: sync_controls reads workbench while workbench is being rendered.
self.settings.update(cx, |settings, cx| settings.sync_controls(cx));

// Correct: the parent passes an owned snapshot across the entity boundary.
let terminal = self.ui_state.terminal_preferences.clone();
self.settings.update(cx, |settings, cx| {
    settings.sync_controls(&terminal, cx)
});
```

Regression coverage for parent-driven control synchronization must assert that
the child method accepts the required state snapshots and contains no reverse
access to the parent entity.

## Scenario: Interface And Code Font Preferences

### 1. Scope / Trigger

- Trigger: Desktop settings let users choose the workbench UI font and code
  rendering font from locally installed font families and tune numeric font
  size/weight for each surface.

### 2. Signatures

```text
appearance_list_system_fonts() -> string[]
localStorage["vibex.interfaceFont"] -> string
localStorage["vibex.interfaceFontSize"] -> numeric string
localStorage["vibex.interfaceFontWeight"] -> numeric string
localStorage["vibex.codeFont"] -> string
localStorage["vibex.codeFontSize"] -> numeric string
localStorage["vibex.codeFontWeight"] -> numeric string
documentElement.style["--vibex-code-font-family"] -> generated CSS font stack
documentElement.style["--vibex-code-font-size"] -> "<number>px"
documentElement.style["--vibex-code-font-weight"] -> numeric string
```

### 3. Contracts

- The Tauri command returns a best-effort sorted list of font family names. It
  must include safe fallback families so the UI still has options when platform
  font discovery is unavailable.
- The UI may store only one normalized font family name, not a raw CSS
  `font-family` expression. CSS stacks are generated by frontend helpers.
- Font size is stored as a bounded pixel number. Font weight is stored as a
  bounded numeric `font-weight`.
- Code font preferences are separate from interface font preferences. They
  apply to Markdown code, command/detail code blocks, xterm surfaces, and
  Monaco editor options. Plain UI text must keep using the interface font.
- Code font CSS variables are presentation outputs generated from normalized
  preferences. Do not persist CSS variable values as the source of truth.
- Legacy `inter/system/serif/mono` font values and
  `compact/standard/comfortable/large` size values should migrate in the
  read-path without breaking existing local preferences.

### 4. Validation & Error Matrix

- Missing font preference -> default interface font family.
- Missing code font preference -> default monospace font family.
- Empty, too-long, or control-character font family -> default interface font
  family or default code font family for code preferences.
- Missing size preference -> default size.
- Legacy size mode -> mapped numeric pixel size.
- Non-numeric or out-of-range size/weight -> normalized to the nearest valid
  bounded value or default when parsing fails.
- System font discovery failure -> fallback font list, not a settings dialog
  failure.

### 5. Good/Base/Bad Cases

- Good: user picks `Noto Sans` for interface, picks `JetBrains Mono` for code,
  sets code size `14px` and code weight `450`, reloads, and the workbench root,
  Markdown code blocks, terminal surfaces, and Monaco all apply normalized
  values.
- Base: platform font discovery fails; the settings dialog still shows fallback
  options plus the currently selected family.
- Bad: storing `Arial, sans-serif` or any arbitrary CSS string directly in
  localStorage and assigning it to `documentElement.style.fontFamily`.
- Bad: changing code font settings but only updating CSS `font-mono`, leaving
  xterm and Monaco at hard-coded font settings.

### 6. Tests Required

- Run `pnpm --dir apps/desktop typecheck` after settings preference changes.
- Run `pnpm check:frontend` after UI control changes.
- Manually smoke the settings dialog in desktop/browser mode when practical:
  open settings, change code font, code size, and code weight, then verify CSS
  variables/localStorage update and code surfaces remain readable.

### 7. Wrong vs Correct

#### Wrong

```typescript
window.localStorage.setItem("vibex.interfaceFont", "Arial, sans-serif");
document.documentElement.style.fontFamily = storedValue;
```

#### Correct

```typescript
const family = normalizeInterfaceFontFamily(storedValue);
window.localStorage.setItem("vibex.interfaceFont", family);
document.documentElement.style.fontFamily = interfaceFontFamily(family);
```

```typescript
const family = normalizeCodeFontFamily(storedValue);
window.localStorage.setItem("vibex.codeFont", family);
document.documentElement.style.setProperty("--vibex-code-font-family", codeFontFamily(family));
```

## Scenario: Desktop Compound Preview Layout State

### 1. Scope / Trigger

- Trigger: Desktop workbench features add pane/tab preview layouts for files,
  terminals, web pages, or future diff surfaces beside the Agent conversation.

### 2. Signatures

```text
PreviewTabTarget =
  file(path)
  terminal(terminalId)
  git_diff(path, staged)

localStorage["vibex-workbench"].state.previewTabs -> Record<string, PreviewTab>
localStorage["vibex-workbench"].state.previewRoot -> PreviewSplitNode
localStorage["vibex-workbench"].state.previewFocusedPaneId -> string | null
```

### 3. Contracts

- Preview state is local UI state in the workbench Zustand store. It is not
  authoritative file, terminal, Git, or browser state.
- Stable resources use deterministic tab ids: `file:<path>`,
  `terminal:<terminalId>`, and `git:<staged|unstaged>:<path>`.
- Terminal tabs opened in the compound preview are separate UI-owned shell
  surfaces from the Agent composer terminal mode. Do not let a shared terminal
  create mutation auto-select a global terminal id for both surfaces. Track
  composer-owned terminal ids as local UI state and pass filtered terminal
  lists to the composer vs. preview/workbench terminal panels.
- Preview file tabs distinguish three states: temporary preview, normal open,
  and pinned. A single click from the Files integrated panel creates or replaces
  one temporary file preview tab. A double click or explicit open action creates
  a normal open tab, not a pinned tab. Only the user's explicit tab context-menu
  pin action may set `PreviewTab.pinned = true`; pinned tabs remain protected
  from ordinary close, close-other, and close-all actions.
- Persist only lightweight layout and tab targets. File contents stay in the
  existing editor buffer flow; terminal output stays in terminal query state.
  Do not persist unpinned temporary file preview tabs.
- Restored file preview tabs whose editor buffer is missing must re-read the
  file for the current workspace before treating the tab as unavailable.
  Background read completions must be ignored after the user switches
  workspace or closes the restored tab.
- File preview routing must use `FileReadResponse.previewKind` after the file is
  read. Extension hints may select known native formats, but unknown extensions
  and extensionless files must still be read as text candidates; do not gate the
  editor behind a closed text-extension allowlist. A service result of `binary`
  remains binary even when the path has a text-like suffix.
- Preview state can grow large when users keep many tabs open. Components should
  subscribe to specific Zustand fields instead of the entire workbench store,
  avoid per-tab scans of the full tab record during render, and avoid
  synchronous large `localStorage` writes on high-frequency tab state updates.
- Preview tab drag state should keep the active tab id in React state while the
  drag is in progress. `dragover` handlers may use MIME type presence to accept
  local preview-tab drags, but must not depend on `dataTransfer.getData()` being
  readable until `drop`, because some browser/runtime combinations expose the
  payload only at drop time.
- Splitting a preview pane from a single tab may intentionally leave the source
  pane empty so `split right` / `split down` remains visible and actionable.
  Close, close-all, and tab move actions should prune empty panes; split actions
  should not immediately prune away the new layout.
- Tab context-menu close-other and close-all actions are scoped to the tab's
  current pane/group. They must not close matching unpinned tabs in sibling
  panes, while pinned tabs in the same pane remain protected.
- Hydration must normalize persisted tab and split records from `unknown`
  before rendering, dropping malformed targets and invalid pane references.
- Workspace changes must clear preview tabs, preview panes, selected file/Git
  path, selected terminal reference, and editor buffers so relative paths from
  one workspace cannot render in another.

### 4. Validation & Error Matrix

- Missing or malformed `previewTabs` -> empty preview tab record.
- Missing or malformed `previewRoot` -> default single empty pane.
- Pane references unknown tab ids -> drop those tab ids from the pane.
- Persisted tabs missing from all panes -> place them in the first valid pane.
- Focused pane id no longer exists -> focus the first valid pane.
- Empty preview tab record -> hide the preview region so Agent uses the space.
- Persisted file tab with no editor buffer after reload -> issue a workspace
  `file_read`; only show unavailable after the read fails or the tab is closed.
- Opening a second unpinned file preview -> remove the previous unpinned file
  preview from panes and preview tab records.
- Close other tabs from pane A -> close only unpinned pane-A siblings; pane-B
  tabs stay open.
- Close all tabs from pane A -> close only unpinned pane-A tabs; pinned pane-A
  tabs and all pane-B tabs stay open.
- Creating a preview terminal tab -> opens a terminal preview tab without adding
  that terminal to the Agent composer terminal id set.
- Creating an Agent composer terminal -> adds that terminal only to the composer
  terminal id set and keeps it out of preview/workbench terminal panels.

### 5. Good/Base/Bad Cases

- Good: reload restores a file tab and terminal tab in their split panes after
  normalizing the persisted layout, then hydrates the file tab content through
  the normal file-read buffer path.
- Good: clicking file A previews A, clicking file B replaces temporary A,
  double-clicking B opens normal B without pinning it, and right-clicking B to
  pin it protects B from later temporary preview replacement and ordinary close
  actions.
- Good: a preview terminal tab and an Agent composer terminal can run different
  shells/PTYs at the same time; typing in one does not affect the other.
- Good: splitting into two panes and choosing Close All Tabs in one pane leaves
  the other pane's tabs untouched.
- Base: corrupt persisted preview JSON falls back to an empty preview region
  without crashing the workbench.
- Bad: rendering persisted split nodes directly and letting invalid tab ids or
  pane ids reach React.
- Bad: switching projects leaves `file:src/App.tsx` open against the new
  workspace without a fresh file read.
- Bad: the shared `terminal_create` mutation writes directly to a global
  selected terminal field, making preview terminal tabs and composer terminal
  mode attach to the same PTY.

### 6. Tests Required

- Run `pnpm --filter @vibex/desktop typecheck` after changing preview state
  types or actions.
- Run `pnpm check:frontend` after UI wiring changes.
- Browser smoke should cover file tab open, terminal tab open, web URL submit,
  close-all auto-hide, and reload with persisted preview state.

### 7. Wrong vs Correct

#### Wrong

```typescript
const root = persisted.previewRoot as PreviewSplitNode;
renderPreview(root);
```

#### Correct

```typescript
const previewTabs = normalizePreviewTabsRecord(persisted.previewTabs);
const previewRoot = normalizePreviewRootRecord(persisted.previewRoot, previewTabs);
```

## Scenario: Seamless Runtime UI And Durable Composer Recovery

### 1. Scope / Trigger

- Trigger: Desktop or Remote lets a user change Agent/authentication-source/model/Effort/Mode
  while preserving one logical session and accepting ordinary messages during
  runtime preparation.
- Server selection, lifecycle snapshots, and durable submissions are separate
  authoritative projections. React state is presentation only.

### 2. Signatures

```text
RuntimeSelectionOverlay { idempotencyKey, desired }
RuntimeViewFence { sessionId, bindingId, activationGeneration }

mergeAuthoritativeRuntimeSelection(current, incoming) -> state
matchingRuntimeAttachment(snapshot, fence) -> attachment | null

SubmissionLocator {
  connectionScope, sessionId, messageIdempotencyKey,
  desiredRuntime, createdAtMs
}

Desktop history attach role -> viewer
Remote history attach role  -> viewer
```

### 3. Contracts

- A selector gesture immediately creates a complete desired selection overlay
  and one stable idempotency key. It never invents a revision, effective
  selection, binding, generation, or switch result.
- Authoritative merge accepts a greater `selectionRevision`, or the same
  selection revision with a non-older `sessionRevision`. CAS failure clears the
  matching overlay and refetches instead of displaying a technical conflict.
- Desktop keeps normal runtime preparation presentation-transparent: the
  selector shows the desired choice, while `WaitingForCurrentWork` and
  `Preparing` render no top status banner regardless of duration. Only
  `FailedUsingPrevious` may render an actionable recovery row, and it must not
  expose spawn/resume/bridge/commit internals.
- Current message/tool/permission projections are accepted only from the exact
  `sessionId + bindingId + activationGeneration` snapshot. A mismatch clears
  live state and triggers selection/snapshot recovery. Historical Timeline
  items remain valid logical-session history.
- Desktop uses event-assisted recovery plus bounded polling and a Viewer lease
  for selected-session history. Remote uses cursor polling and a Viewer lease.
  Message or command work materializes through the backend worker path; merely
  selecting history never requires runtime ownership. Stream reset or epoch
  change discards the cursor and refetches; push delivery is never correctness
  proof.
- Ordinary send calls the durable send API directly with the displayed desired
  runtime and a stable message idempotency key. It does not await the switch and
  is not disabled by another pending submission.
- When an ordinary Composer action enters durable send, Desktop immediately
  projects the captured user message and a presentation-only running Agent turn
  before spawning the durable send future. Runtime switching, Context Bridge
  preparation, Ready convergence, and prompt dispatch remain background work.
  The projection never mutates the authoritative Timeline: a matching persisted
  user row replaces it, while a terminal send failure removes it and clears the
  local running state.
- If the user interrupts that turn before initial runtime preparation reaches
  provider dispatch, Desktop removes the optimistic row and pending Agent state
  when the durable cancellation completes, without presenting the intentional
  `message_submission_interrupted_before_dispatch` outcome as an initial-message
  failure. Runtime preparation may continue independently for later prompts.
- Local/session storage may persist only the bounded locator and runtime client
  handle. Prompt text, attachments, tokens, secrets, raw errors, native ids,
  and live binding data must not be persisted there.
- Queued/sending/failed/ambiguous states render beside Composer, not as
  synthetic Timeline items. Ambiguous dispatch is never replayed
  automatically. Agent history attribution comes from the item's safe
  execution attribution, never the current selector.
- `InputEvent.inputType` is not guaranteed for autofill, automation, or every
  WebView synthetic event. Test its runtime type before string operations in a
  contentEditable `beforeinput` handler.

### 4. Validation & Error Matrix

- Same desired selection -> local no-op and no new switch request.
- Stale authoritative event -> ignored by revision merge.
- Snapshot binding/generation mismatch -> reject live projection and refetch.
- Cursor reset/stream id change -> clear cursor/snapshot, refetch selection,
  and reattach.
- Authentication-required failure -> retain effective runtime and open the
  selected source's login or Provider configuration recovery.
- Final transient failure -> retain effective runtime and offer an explicit
  retry when a target is remembered.
- Ambiguous message dispatch -> show uncertain delivery and never auto-resend.
- User interrupt before initial prompt dispatch -> clear the optimistic turn and
  show no failure notification; do not claim that the provider was interrupted.
- Missing `beforeinput.inputType` -> treat as an unclassified insertion event;
  do not throw or suppress input.

### 5. Good/Base/Bad Cases

- Good: a user selects another Agent, immediately sends two messages, reloads,
  and sees the selector, live activity, and ordered submission states converge
  from backend revisions without duplicate prompts.
- Base: a switch completes inside 400 ms and the UI moves directly from the old
  effective label to the new one without flashing a pending row.
- Bad: optimistic UI changes effective state, writes a fake authoritative user
  Timeline row, waits for runtime readiness before projecting the captured user
  turn, stores message text in localStorage, or applies an old attachment
  snapshot to the new generation.

### 6. Tests Required

- Shared unit tests cover revision ordering, overlay reconciliation, Catalog
  grouping, exact fence rejection, status presentation, locator sanitization,
  bounds, TTL, and deduplication.
- Desktop tests cover synchronous optimistic user-turn plus pending-Agent
  projection before the background durable send, authoritative replacement
  without duplication, and failure cleanup.
- Desktop/Web typechecks and builds cover generated contracts and controller
  wiring; lint must report no unsafe casts or suppressed errors.
- Browser checks exercise preparing, queued, settled, read-only, and recovery
  controls at desktop and 390 px widths, including dark mode, console errors,
  and horizontal overflow.
- Backend suites remain the authority for CAS, at-most-once prompt dispatch,
  authorization, error projection, and secret redaction.

### 7. Wrong vs Correct

#### Wrong

```typescript
setEffective(selection);
await switchRuntime(selection);
appendOptimisticTimelineMessage(text);
await sendMessage(text);
```

#### Correct

```typescript
setOverlay({ idempotencyKey, desired: selection });
setDesiredRuntime({ expectedRevision, expectedSelectionRevision, desired: selection });
registerSubmissionLocator({ sessionId, messageIdempotencyKey, desiredRuntime: selection });
projectOptimisticUserTurn({ sessionId, text, attachments });
sendMessage({ sessionId, messageIdempotencyKey, desiredRuntime: selection, text });
```

The backend revisions and durable submission state later replace presentation
overlays; no local state declares commit or delivery.

## Scenario: GPUI Agent Workbench Projection And Session Fencing

### 1. Scope / Trigger

- Trigger: the native GPUI desktop renders Agent project/session navigation,
  timeline rows, runtime controls, permissions, and Composer submission through
  the shared desktop runtime.
- This is a cross-layer contract: `desktop-model` owns deterministic projection,
  `desktop-runtime` exposes typed services, and the GPUI entity owns only
  view-lifetime tasks, focus, scrolling, and overlay state.

### 2. Signatures

```text
project_sidebar_rows(sessions, SidebarState, query) -> Vec<AgentSidebarRow>
SidebarState::move_row_relative(moving_id, target_id, after) -> bool
timeline_rows(items) -> Vec<TimelineRow>
timeline_conversation_turns(items, session_state, pending_turn_active)
  -> Vec<TimelineConversationTurn>
TimelineModel::replace_authoritative(session_id, items)
TimelineModel::apply_live(TimelineLiveEvent) -> changed
TimelineModel::mark_lagged()

MarkdownViewState::should_virtualize_blocks() -> bool
MarkdownVirtualFlow {
  outer_scroll_content_mask, estimated_total_height,
  measured_top_level_block_heights, overscanned_visible_range
}

AgentSessionViewCacheEntry {
  timeline, runtime_selection, timeline_follow, timeline_scroll,
  collapsed_timeline_rows, timeline_process_expansion
}

DesktopPollingPolicy {
  timeline_fallback_ms: 300,
  runtime_events_ms: 2000,
  attach_heartbeat_ms: 30000
}

MessageSubmissionCoordinator::submit(SendAgentMessageRequest)
AgentManager::get_session(session_id) -> AgentSession

Composer submission completion order:
  submit result -> authoritative AgentSession snapshot
                -> clear local pending -> recovery policy

RuntimeCascadeProjection::from_catalog(catalog, desired)
  -> RuntimeCascadeProjection

ComposerUiState {
  runtime_selections_by_agent:
    BTreeMap<AgentId, SessionRuntimeSelection>,
  runtime_selections_by_model:
    Vec<SessionRuntimeSelection> // max 256, identity = Agent/AuthSource/Model
}

ComposerGeometry {
  input_bounds, runtime_trigger_bounds
}

RuntimeMenuPlacement { anchor, height, trigger_offset }
```

### 3. Contracts

- GPUI must render stable virtual sidebar/timeline rows projected by
  `desktop-model`; GPUI entities must not become a second timeline, permission,
  ordering, or runtime authority.
- Selecting a session increments a view generation before starting fetch,
  polling, heartbeat, command discovery, or mutations. Every session-scoped
  completion checks that generation before changing the active timeline,
  runtime selection, error, Composer, focus, or pending state.
- A session switch clears view-local pending state from the previous generation.
  The durable backend operation may finish, but its stale completion cannot keep
  the newly selected session disabled or replace its state.
- Initial load starts generation-fenced sibling work for the Viewer lease,
  authoritative timeline, and runtime selection. Only the authoritative timeline
  controls `agent_loading`: Viewer heartbeat, runtime catalog/model probes,
  and terminal metadata must not delay persisted history. After the timeline is
  ready, merge live events with the 300 ms timeline fallback and 2-second runtime
  poll. Sequence gaps, lag, or cursor reset mark the model for authoritative
  refetch; push delivery alone is never correctness proof.
- The full-window GPUI startup brand overlay is one-shot local UI state, separate
  from `agent_loading`. Dismiss it as soon as the authoritative `DesktopRuntime`
  reaches `Ready`, or when runtime startup fails. Initial overview and complete
  authoritative timeline restoration continue under their workbench loading and
  error states; they must not retain or reopen the brand overlay. Later session
  switches must not reopen it either.
- Switching sessions detaches the previous Viewer and starts the configured
  30-second heartbeat for the selected session. Polling and heartbeat intervals
  come from `DesktopPollingPolicy`, not view-local constants.
- Correlation ids are not a streaming-text boundary: ACP chunks may omit them or
  assign a different provider correlation to each chunk. Adjacent deltas of the
  same stream type merge while execution attribution remains compatible; a User,
  another event type, or conflicting attribution fences the merge. An adjacent
  compatible final Agent message replaces the streaming body while retaining the
  first row id and the row's inclusive sequence range.
- An append-only live batch may update only the final projected Turn when the
  session identity, item identity, and sequence values are strictly contiguous
  and no authoritative refetch is pending. A missing sequence, reconnect marker,
  replacement, attribution change, or non-delta snapshot must abandon that fast
  path and rebuild from the authoritative timeline. A merged streaming row keeps
  at most its first and last source item ids; `turn_item_count` and the inclusive
  sequence range retain the count and navigation semantics without one heap String
  per token.
- Streaming row height estimates keep a compact text-metrics accumulator. After
  the first full scan, each accepted delta updates only the appended chunk;
  replacement/final snapshots clear the accumulator before measuring the new body.
  The incremental estimate must remain equivalent to the bounded full estimator,
  including newline wrapping and workspace-link affordances.
- Turn projection treats a persisted `error` as the conclusion of a completed
  display turn. Later Agent/Provider output starts a continuation turn even
  when the provider used a hidden prompt and no new `user_message` exists.
  Permission resolutions remove their request id from the turn's pending set.
  Project each permission row's pending state from its own request id and the
  matching resolution, independently of the turn-level pending flag. The turn
  flag answers whether any request still blocks the turn; it must not keep an
  already resolved card actionable while a sibling request remains pending.
- Timeline rows carry stable turn metadata (`turnId`, item count, failed,
  pending-permission, and conclusion) so virtual cards can expose conclusion
  and failure state without reparsing the full transcript in the renderer.
- `TimelineConversationTurn` owns the User bubble, compacted process rows,
  consistent execution attribution, conclusion, and completion state. GPUI
  virtualizes Turns, keeps an active Turn's process open, defaults completed
  process history closed, and never repeats Agent/Streaming attribution per
  delta row.
- Turn virtualization is not sufficient for one extremely tall Agent message.
  Long `MarkdownPresentation::Agent` documents must additionally virtualize
  top-level Markdown blocks against the outer timeline content mask, retain a
  bounded overscan window, and converge estimated block heights to measured
  heights. The virtual flow reserves the complete document height and continues
  to use the timeline's one vertical scroll handle; it must not add an inner
  height cap or nested vertical scrollbar. Short Agent messages and non-Agent
  document surfaces keep the full-render path. Markdown above the synchronous
  parse budget parses in the background and applies only the latest generation.
- Copying a complete streaming Markdown source is throttled per row (time and
  byte thresholds) while deltas arrive; a final or non-streaming row refreshes
  immediately. Markdown and tool-card projection caches are both bounded by an
  entry limit and a resident-byte budget, and a single oversized active value may
  remain as the sole cached entry without allowing older entries to grow
  unbounded.
- A GPUI User bubble uses one full-width `flex + justify_end` wrapper and an
  intrinsic-width, non-shrinking bubble with a bounded maximum width. Keep its
  text body as a plain intrinsic child: do not put `min_w_0` on it and do not
  apply `gpui_component::ScrollableElement::overflow_y_scrollbar`. That wrapper
  projects `size_full`, so an auto-width bubble can collapse to a padding-only
  vertical pill even while the virtual Turn reserves the expected height. Do
  not wrap the bubble in a second full-width horizontal flex either.
- Opened sessions use a bounded 6-entry LRU presentation cache. Before a switch,
  snapshot the current `TimelineModel`, runtime selection, follow/scroll state,
  and disclosure state; restore a cached target synchronously with no loading
  blank, then replace it from the complete authoritative prefix in the
  background. A cache is display optimization only and never advances authority
  or bypasses generation fencing.
- The session presentation cache also enforces a total resident-byte budget. Its
  entries carry a precomputed weight from timeline/presentation strings and
  containers, so eviction does not serialize or rescan inactive conversations;
  an individual session over the budget is rendered from storage and is not
  retained in the cache.
- The desktop runtime event bridge drains a bounded batch of queued signals per
  GPUI update. Contiguous `DesktopEvent::Timeline` values for the selected
  session go through one `TimelineModel::apply_live_batch`; plan reconciliation,
  derived Turn projection, row-size rebuilding, follow-state updates, and root
  notification run once per batch. Non-timeline events fence batches so their
  original ordering remains observable. Background-session timeline events are
  discarded before projection and do not repaint the selected workbench. The
  runtime-to-GPUI channel is bounded and applies sender backpressure; if the
  upstream broadcast then reports `Lagged`, the existing authoritative-refetch
  path restores correctness instead of allowing queued event memory to grow.
- Root workbench layout must not observe or tracked-read the complete
  `CodeWorkbench` entity. The child emits a narrow `CodeWorkbenchEvent` only for
  parent-owned layout changes such as Preview fullscreen; the root mirrors that
  scalar state. Stable, fixed-size child entities (`CodeWorkbench`, right rail,
  Management, Usage, Terminal, PDF, and Office surfaces) render through GPUI
  `Entity::cached(StyleRefinement::default().size_full())` boundaries so child
  notifications do not rebuild unrelated parent element trees.
- The unfiltered sidebar project/workspace/session projection is cached behind
  an explicit revision and shared as `Rc<Vec<SidebarProjectProjection>>`.
  Authoritative workspace/session/context replacement, pin changes, and
  project/session ordering invalidate it. Rendering consumes projections by
  reference; query-filtered projections remain uncached because their input is
  transient.
- Editor keystrokes update the editor entity immediately but debounce the full
  recovery snapshot by 200 ms. Ordinary layout persistence reuses the last
  recovery snapshot and must not walk every dirty buffer. Recovery buffers use
  shared immutable storage when `DesktopUiStateV1` is cloned for the throttled
  writer, while serde preserves the existing JSON array contract. Successful
  save, force-close/discard, rename/delete, and workspace reset refresh recovery;
  app quit synchronously captures the latest buffers before the final flush.
- Embedded terminals poll only while visible. Preview activates the terminal
  selected in every visible split pane; Composer activates only its selected
  terminal while terminal mode is on. Hidden surfaces cancel polling and cursor
  blink tasks. A visible terminal polls at 16 ms while output changes, then
  backs off to 100 ms after four idle snapshots; repeated identical errors do
  not notify again.
- Timeline fallback polling resets its idle counter after content and backs off
  exponentially after repeated empty polls, capped at two seconds. Files and Git
  refresh loops use the actual rendered surface visibility: hidden surfaces skip
  normal refresh work and only perform the low-frequency 30-second check; opening
  a surface triggers an immediate refresh.
- Session full-text search builds its timeline index only for the open search
  dialog and clears the index/task while advancing its generation when the
  dialog closes, so a closed search cannot retain every session's message text
  or accept a stale indexing completion.
- Turn-preview summaries use a strict UTF-8 source budget (8 KiB), node budget,
  and workspace-resource budget. The full selected-session timeline remains
  authoritative; only the compact preview is truncated and bounded.
- A compact Turn preview rail may derive one button per stable `turnId` from the
  visible row projection. Activating a preview only scrolls the virtual list to
  that row and marks the reader away from the bottom; it must not mutate timeline
  items, turn grouping, or authoritative cursor state.
- Session-row rename/delete controls capture the target `VibexSessionId` before
  opening their dialog and call the typed manager mutation for that id. They must
  not silently retarget the currently selected session; successful non-selected
  mutations refresh the session list while preserving the active generation.
- The global short action lock is owned by the mutation operation, not by the
  selected-session view generation. Every rename/delete (including optimistic
  batch delete) completion releases `agent_action_pending` unconditionally;
  generation fencing applies only to active-view data and error updates. A
  deletion can refresh the sidebar and select a replacement session before its
  backend completion arrives, and that navigation must not strand the global
  lock or disable unrelated session, Composer, or new-session controls.
- Session-row context menus reuse those captured typed targets. Pin/unpin must call
  the same persisted `SidebarState` mutation as the inline pin button; menu actions
  must not maintain a second pin projection or infer the target from selection.
- Session-row drag/drop carries the typed session id, project id, and pin band. A
  drop may reorder only another session in the same project and pin band; the
  deterministic `SidebarState.row_order` mutation owns before/after insertion and
  the UI persists it only after a real move.
- A session absent from persisted `SidebarState.row_order` is newly discovered.
  Sort newly discovered sessions by recency ahead of manually ordered sessions in
  the same pin band, while pinned sessions remain above unpinned sessions.
- GPUI dispatches a typed `on_drag_move` callback to every rendered target that
  listens for that drag type. A row that does not contain the pointer must not
  clear the shared reorder target established by the row that does contain it.
  Keep the last valid preview order, widen row hit testing across the inter-row
  gap, and commit that preview from the drag source's inside/outside mouse-release
  handlers instead of requiring the animated target row to remain under the
  pointer. Project reordering may transiently collapse project children so target
  geometry stays fixed; restore the persisted collapsed state after release.
- When runtime selection is `failed_using_previous`, Retry re-enqueues the
  remembered desired selection through the durable selector, while Reset
  requests the effective selection. Neither action mutates effective state
  optimistically.
- Runtime controls derive an Agent → authentication source → Model → Reasoning
  Effort → Mode cascade from `SessionRuntimeOptionCatalog`. New-session UI may
  omit the Agent level, while current-session UI keeps it. Each visible choice
  maps back to a complete `SessionRuntimeSelection`; source summaries remain
  visible even when their model options are unavailable, and the selected
  request still goes through the durable runtime-selection service.
- Reasoning Effort selector names derive from the catalog value (`low` -> `Low`,
  `medium` -> `Medium`, `xhigh` -> `XHigh`), never from the explanatory
  description. Known levels render from lower to higher depth
  (`none/minimal/low/medium/high/xhigh/max/ultra`), followed by unknown values in
  deterministic order. Do not insert or retain a synthetic `Default` effort;
  `reasoning_effort = null` continues to mean the Adapter's converged default.
- Mode choices are exactly the values advertised by the runtime catalog. Do not
  inject a null-selection `Default`; an Agent-advertised mode whose id is
  `default` remains a normal real choice. ACP `SessionMode` exposes id, name,
  description, and opaque `_meta`, but no risk level. Do not infer danger from
  mode ids/names/descriptions or `_meta`, and do not apply warning colors until
  a provider-neutral typed risk field exists.
- `DesktopUiStateV1.composer.runtime_selections_by_agent` persists the most recent
  complete selection per Agent so Agent A -> B -> A restores the last route.
  `runtime_selections_by_model` additionally persists at most 256 selections by
  exact Agent/AuthSource/Model identity, so choosing a Model restores its own valid
  Effort/Mode/feature values even after selecting another Model. Explicit
  new-session choices and successful in-session runtime changes update both
  projections. Existing per-Agent values seed the per-Model collection during
  normalization for backward compatibility. Every restore is revalidated against
  the current catalog before it reaches a selector or mutation. Persist only
  catalog-backed Toggle/Select feature values; freeform String feature values may
  contain user content and remain view-local.
- Async in-session preference writes carry a per-Agent intent epoch. A completion
  persists only while its epoch is still current, so an older switch cannot
  overwrite a newer choice; activity for another Agent does not invalidate it.
- Above the compact breakpoint, the runtime trigger visibly renders
  `authentication source / Model`; compact icon-only triggers keep the same
  complete value in their tooltip. Rendering only the Model name is ambiguous
  when two sources expose similarly named Models.
- Composer terminal controls own only a filtered terminal-id list and selected
  id. Create goes through the typed desktop runtime terminal facade and persists
  metadata; switch changes the Composer selection without auto-selecting a
  preview/workbench terminal tab. Terminal metadata refresh is best effort and
  must not prevent the authoritative session timeline/runtime load. PTY/xterm
  rendering remains a native-surface responsibility.
- Image attachments remain typed Composer nodes. Image tokens use MIME/path
  detection for compact and Dialog previews, generate a bounded escaped
  `file://` Markdown reference on explicit insertion, and save collision-safe
  copies under the isolated desktop home without mutating the source file.
  Native image chooser results, clipboard image entries, dropped paths, and
  bounded HTML `data:image/*;base64` payloads converge through the same typed
  attachment path; unrecognized clipboard text remains ordinary Composer text.
- Inline attachment marker ranges contain only non-breaking word characters;
  insertion padding stays outside the marker and is removed from the submitted
  text. Root-level token overlays render only when the marker's buffer row is in
  `InputState::visible_row_range` and its complete bounds remain inside the
  tracked input viewport. Never trust `range_to_bounds` alone for an offset above
  the visible prefix, and never let a marker range include a wrapping space. The
  opaque token covers the complete horizontal marker bounds without an inset, so
  raw marker glyphs cannot remain visible at either rounded edge.
- New-session text, attachments, workspace, Agent, and runtime options are one
  view-local draft. Navigating to an existing session hides but does not reset
  that draft; reopening restores it. Clear it only on explicit Cancel or after
  session creation is accepted. Prompt text and attachments remain excluded from
  persisted UI state.
- Runtime Popovers track their trigger. Choose the side with usable viewport
  space, cap menu height to that space, and keep the menu attached to the trigger
  with a fixed 4 px visual gap. Upward menus may overlay non-trigger Composer
  content; offsetting them beyond the complete Composer surface leaves an
  excessive gap from the control that opened them.
- Native Composer cut and paste must use the history-recording InputState edit
  path. Calling a `*_silent` replacement from clipboard actions makes Ctrl+Z
  undo an older IME/text edit and prevents Ctrl+Y/Ctrl+Shift+Z from restoring
  the clipboard operation. Product-level Ctrl+V handling may explicitly call
  `InputState::paste_from_clipboard` after image/HTML attachment detection, but
  must defer that mutation out of the active key-dispatch borrow.
- Composer suggestion selection is a bounded model projection. Async refresh
  clamps the selected index, Up/Down wrap within the visible result count, Tab
  inserts the selected entry, and Escape dismisses the menu. While native IME
  marked text is active, these suggestion shortcuts perform no mutation.
- Composer submission snapshots text and attachments and calls the durable
  coordinator with the displayed desired runtime and an idempotency key. Clear
  the visible draft/attachments only after coordinator acceptance; validation,
  switch, dispatch, task, and recovery errors preserve them.
- A submission completion, including a provider failure, reloads the
  authoritative `AgentSession` before clearing the local turn-pending projection
  and evaluating session recovery such as auto-continue. Timeline live events
  update timeline content, not the sidebar/session-state snapshot; using the
  pre-submit `Idle` or `Running` snapshot can hide the continue affordance and
  skip recovery even though storage has already committed `Error`.
- After a successful submission reaches its turn boundary, automatic Composer
  queue dispatch may ignore a lagging sidebar `Running` snapshot. The local
  per-session turn-pending flag and an authoritative `NeedsInput` state remain
  hard fences, and the dispatch must still honor the global action lock, queue
  pause state, and Auto/Manual send mode. This same completion path applies to
  the initial prompt of a newly created Session: release the new-session action
  lock before advancing the queue, preserve pause behavior after prompt failure
  or a user interrupt, and honor an explicit queued-message steer request.
- Auto-continue is a safe local preference persisted in `DesktopUiStateV1`.
  Store project defaults separately from per-session boolean overrides so an
  explicit session disable survives restart even when its project default is
  enabled. Normalize and bound both collections, remove stale project/session
  ids during authoritative UI-state cleanup, and keep countdown/handled-turn
  bookkeeping transient. The trigger is not limited to `AgentSessionState::Error`:
  after the latest session snapshot and authoritative timeline are reconciled,
  an enabled `Idle` or `Error` session starts the countdown whenever its latest
  conversational segment lacks an explicit final Agent message. A final Agent
  message suppresses the countdown even if the session state is `Error`.
  Starting any local send/continue turn invalidates the previous completion
  probe, handled-turn marker, and countdown immediately. Replacing an
  authoritative session snapshot also invalidates them when either state or
  `updatedAtMs` changes, so an `Idle -> Error` transition cannot reuse a normal
  completion cached in the same millisecond.
- Claude/Codex JSONL support in this surface is offline import only. Adding the
  offline import crates must not introduce a Native online runtime route or
  provider-specific timeline rendering.

### 4. Validation & Error Matrix

- Live sequence skips one or more items -> keep bounded presentation state,
  set `needs_authoritative_refetch`, and reload the selected session.
- A batch is not a strictly contiguous append, or a reconnect/replacement/final
  snapshot arrives -> discard streaming text metrics and rebuild the affected
  projection from the authoritative items; never append into a stale row.
- Runtime-to-GPUI delivery reaches its bounded capacity -> apply sender
  backpressure. If the upstream broadcast reports lag, mark the affected
  projection for authoritative refetch; never switch back to an unbounded queue.
- Async result generation differs from active generation -> ignore active-view
  mutations; a global list refresh may still reconcile durable changes.
- A rename/delete completion arrives after navigation changed the session
  generation -> always release the operation's global short action lock, while
  suppressing stale active-view success/error updates.
- Session switches while send/runtime/permission/interrupt is pending -> the
  new view is usable and the old completion cannot clear or overwrite it.
- Runtime switches for the same Agent complete out of order -> only the newest
  intent may update the remembered Composer selection.
- Cached session selected -> render the cached Turn projection immediately,
  keep `agent_loading=false`, and refresh authoritatively in the background.
- Cached refresh fails -> retain cached content and surface the bounded error;
  do not replace the conversation with a loading or empty state.
- One inactive session exceeds the presentation-cache byte budget -> do not
  cache it; reload its authoritative prefix when selected again.
- Owner materialization or a runtime catalog probe remains pending -> render the
  authoritative timeline as soon as its fetch completes; runtime-dependent
  Composer controls may remain unavailable until their projection arrives.
- A non-empty User message projects into a zero/invisible-width bubble -> reject
  the layout; keep the single right-aligned wrapper and intrinsic bubble width.
- A long Agent Markdown view materializes every top-level block for each scroll
  frame, reports only the viewport height, or creates a second vertical scroll
  surface -> reject the layout. Keep the full estimated/measured extent while
  materializing only the content-mask range plus bounded overscan.
- Coordinator rejects or task join fails -> preserve Composer text and
  attachments and show the typed/bounded error.
- Provider dispatch fails after the backend commits session `Error` -> merge the
  latest `AgentSession`, clear local pending, then evaluate auto-continue once
  against the authoritative turn-completion snapshot. A failed snapshot reload
  still surfaces the submission error and must not infer `Error` from provider
  message text. The same reconciliation applies when an `Idle` session has
  user/Agent content without a normal final message.
- A new turn starts while its session still has the previous turn's timestamp,
  or an authoritative state transition reuses that millisecond -> discard the
  previous completion/handled cache and probe the current timeline; do not use
  timestamp equality as the sole turn identity.
- Runtime event stream lags -> keep fallback polling active and refetch the
  required authoritative projections.
- Session search closes while indexing -> advance its generation, cancel/drop
  the task, and clear all indexed documents; stale completions are ignored.
- Files, Git, or Terminal is hidden -> skip its normal refresh/blink work. The
  next visible transition performs an immediate refresh before normal polling.
- No sessions exist -> keep the global new-session action reachable and render
  an honest empty state rather than fabricating a runtime/session.
- Drag target is the same row, another project, or another pin band -> reject the
  target and do not persist a new order.
- Relative reorder is missing either id or already adjacent in that direction ->
  return `false`; do not schedule a persistence write.
- Remembered runtime selection is missing from the current catalog, unavailable,
  or contains a removed Effort/Mode/feature value -> ignore it for that catalog
  snapshot and show the first available option for the Agent. Do not overwrite
  the remembered value from automatic reconciliation: startup first publishes a
  provisional configured-model catalog, and later capability enrichment may make
  the preference valid again. Only an explicit user choice or a confirmed
  in-session switch replaces it.
- Persisted runtime preference key does not match `selection.agent_id`, has an
  empty explicit Model, or exceeds bounded key/value/count limits -> drop that
  preference during `DesktopUiStateV1::normalize`; `AgentDefault` is valid and
  does not require a model id. Preferences never become runtime authority.
- A per-Model preference has the same Agent/AuthSource/Model identity as an earlier
  entry -> keep the latest normalized configuration in that slot. An invalid or
  257th distinct entry is dropped/evicted without affecting runtime authority.
- Catalog exposes no Reasoning Effort values, or only a `default` sentinel ->
  render no Effort choice; do not fabricate a level or a `Default` menu item.
- Catalog exposes no Mode values -> render no Mode choice. Catalog exposes a real
  `default` Mode -> render it once; never add a second synthetic choice.
- Attachment row is above/below the input viewport, including a stale
  `range_to_bounds` result mapped to the first visible line -> omit its overlay.
- Runtime menu cannot fit at full height on either side -> use the larger side
  and reduce its scroll viewport without crossing the trigger.

### 5. Good/Base/Bad Cases

- Good: 1,800 adjacent deltas with missing or per-chunk correlation ids project
  to one stable streaming row, then reconcile with a differently correlated
  final message without duplication.
- Good: switching A -> B -> A restores A's User/Agent Turns and runtime selection
  synchronously while an authoritative prefix refresh runs behind the cached view.
- Base: a two-character User message remains a readable right-aligned bubble,
  including when the idle turn has only streamed deltas and no final message.
- Good: a response with hundreds of Markdown blocks retains its full timeline
  height while the first frame and a later scroll position each materialize only
  a small viewport-relative block range.
- Base: a short response uses the normal full Markdown renderer, including its
  existing cross-block text selection and resource behavior.
- Good: switch from session A to B while A's runtime switch is pending; B loads
  its own selection and remains enabled when A finishes.
- Good: an enabled auto-continue session receives a provider 429 failure; the
  completion reloads its durable `Error` snapshot, starts one countdown, and
  continues without matching the human-readable error text in GPUI.
- Good: a Codex capacity failure is normalized to a Provider timeline error;
  even when the session transition shares a millisecond with the previous
  snapshot, stale final-message evidence is discarded and one countdown starts.
- Good: an enabled session becomes `Idle` after a delta-only or interrupted
  turn; the authoritative timeline has no final Agent message, so the same
  countdown and continuation path are offered.
- Base: an `Idle` or `Error` session whose latest segment ends with an explicit
  final Agent message does not show the continue banner or start auto-continue.
- Good: choose `high` for Codex and `low` for Claude, switch between their
  new-session chips, restart the desktop, and restore each choice only while its
  exact Profile/Model and advertised values remain valid.
- Good: choose Model A with high/Agent mode and Model B with low/Read-only mode;
  switching A -> B -> A restores both exact configurations after catalog
  validation.
- Good: type a new-session prompt with an image, open an existing session, then
  reopen New Session and recover the same prompt, attachment, and runtime options.
- Base: ACP advertises a real `default` mode named `Manual`; it appears once with
  the Agent-provided label and no inferred warning color.
- Base: a clean preview home has no sessions; the workbench renders its empty
  state while the shared runtime lifecycle remains healthy.
- Base: a remembered Model was disabled since the last run; the selector falls
  back to the first available option for that Agent while retaining the bounded
  preference until an explicit replacement is chosen.
- Bad: store raw `TimelineItem` merge logic in the GPUI render method, clear the
  draft before `submit` accepts, replace the active runtime from a stale task,
  label Effort choices with long descriptions, sort them alphabetically, show a
  synthetic `Default`, infer mode risk from a label, clear a hidden new-session
  draft, or remove polling because live events appeared healthy in one run.

### 6. Tests Required

- `desktop-model` tests cover all 17 timeline kinds, missing/different correlation
  delta/final reconciliation, attribution/event fences, process compaction,
  sequence-gap refetch, collapsed search, follow-bottom/unread, 5,000 rows, and
  bounded streamed-row aggregation.
- Multiple permission requests in one turn resolve independently: after the
  first resolution, its row is non-pending while an unresolved sibling and the
  turn-level pending flag remain pending.
- Delta-only idle-turn regression tests include hidden system notices, a short
  User message, no final Agent message, and assert both the User row and merged
  fallback conclusion remain visible in the Turn projection.
- GPUI source-contract coverage keeps the User bubble non-shrinking and bounded,
  rejects `min_w_0` or `overflow_y_scrollbar` in its intrinsic body helper, and
  keeps runtime catalog/Owner attach calls outside the timeline loading task.
- The `vibex-markdown` GPUI fixture must render a document with hundreds of
  top-level blocks inside a fixed-height outer scroll surface, assert that the
  visible range is a strict subset, change the outer offset, and assert that the
  range advances while total height remains larger than the viewport. Desktop
  coverage must continue proving that long Agent Markdown exceeds the old height
  cap and does not gain an inner scrollbar.
- GPUI unit tests cover LRU recency/eviction and complete-prefix pagination; a
  cached session switch must not clear its Timeline or enter a loading state.
- GPUI unit/source-contract tests cover the session-cache byte budget, bounded
  runtime event channel, search-index release, and real Files/Git/Terminal
  visibility gates.
- Streaming tests assert historical Turns remain `Rc`-shared for append-only
  updates, sequence gaps reject the fast path, incremental height metrics equal
  full estimation, and a throttled Markdown snapshot keeps its source and
  revision paired while replacement/final snapshots refresh immediately.
  The `vibex-markdown` GPUI tests also cover append-only virtual-block geometry:
  measured stable blocks and the continuing tail survive each parse, streaming
  heights never shrink, and the first non-streaming measurement may settle to a
  smaller final intrinsic height.
- `desktop-model` runtime tests feed descriptions and shuffled Effort values,
  then assert value-derived names, semantic low-to-high ordering, deterministic
  unknown ordering, and no `Default` effort.
- UI-state tests cover per-Agent and per-Model preference normalization, legacy
  seeding, exact-identity deduplication, bounds, and atomic round-trip. GPUI tests
  assert Agent A -> B -> A preference isolation, Model A -> B -> A configuration
  restoration, provisional -> enriched catalog restoration, freeform feature
  exclusion, stale catalog fallback, out-of-order completion fencing, and visible
  `Provider Profile / Model` labels.
- GPUI tests assert Mode projection contains only catalog values, runtime menu
  placement stays adjacent to its trigger in both directions, hidden new-session
  navigation does not clear the draft, attachment markers do not own wrapping
  spaces or show glyphs beyond the opaque token, and off-viewport attachment
  bounds are rejected.
- GPUI contract probe asserts virtualization, authoritative/live merge,
  generation fencing, row drag reorder, durable submission, Owner heartbeat,
  native IME input, attachment drop, and permission actions.
- GPUI regression coverage asserts a failed submission reconciles the latest
  session snapshot before clearing per-session pending state and invoking the
  auto-continue decision. Auto-continue tests cover both incomplete `Idle` and
  `Error` snapshots, explicit final-message suppression, per-session timeline
  probe fencing, and session-update invalidation. Session-row context-menu tests
  assert the captured row id and checked state drive the same session-scoped
  toggle as the Composer. Session mutation completion tests cover rename,
  single/optimistic batch delete, and navigation changing the generation before
  the backend result; each completion must release the global short action lock
  without applying stale active-view state.
- `desktop-model` navigation tests assert before/after insertion plus missing-id
  and already-adjacent no-op behavior.
- Run targeted GPUI/model tests, `cargo check --workspace --all-targets --locked`,
  `cargo test --workspace --locked`, frontend/binding checks, Foundation capture
  verification, and `git diff --check` before commit.

### 7. Wrong vs Correct

#### Wrong

```rust
let result = switch_runtime(old_session).await?;
self.runtime_selection = Some(result);
self.composer.clear();
```

#### Correct

```rust
let generation = self.session_generation;
let result = coordinator.submit(snapshot.request).await;
if self.session_generation != generation {
    return;
}
match result {
    Ok(_) => snapshot.accept_and_clear(&mut self.composer),
    Err(error) => snapshot.preserve_and_report(&mut self.composer, error),
}
```

#### Wrong

```text
delta correlation A -> row A
delta correlation B -> row B
switch session -> clear Timeline -> show Loading session
attach Owner -> wait for provider restore -> fetch persisted timeline
```

#### Correct

```text
adjacent compatible deltas -> one stable row -> adjacent final replaces body
switch opened session -> restore bounded cache -> refresh authoritative prefix
fetch persisted timeline -> end loading; attach Viewer in the background
```

#### Wrong

```text
virtualize Turn -> render every block of its 100,000-character Agent message
long Agent message -> max-height container -> nested vertical scrollbar
```

#### Correct

```text
virtualize Turn -> reserve full Markdown height -> render content-mask blocks + overscan
large parse -> background generation fence -> latest document replaces prior render
```

#### Wrong

```rust
let label = effort.description.clone().unwrap_or(effort.value.clone());
let mut choices = vec![default_choice];
choices.extend(BTreeMap::from_iter(efforts)); // alphabetic, not depth order
```

#### Correct

```rust
let mut choices = reasoning_efforts
    .filter(|effort| effort.value != "default")
    .map(|effort| (reasoning_effort_rank(&effort.value), reasoning_effort_label(&effort.value)))
    .collect::<Vec<_>>();
choices.sort_by_key(|(rank, _)| rank.clone());
let preferred = persisted_by_agent
    .get(&agent_id)
    .filter(|selection| catalog_has_runtime_selection(catalog, selection));
```

#### Wrong

```rust
let mut modes = vec![RuntimeCascadeChoice::default_choice()];
let bounds = input.range_to_bounds(&attachment_range)?;
render_root_overlay(bounds); // may pin a scrolled-out marker to the first row
```

#### Correct

```rust
let modes = catalog_modes.map(RuntimeCascadeChoice::from_catalog).collect();
let remembered = persisted_by_model
    .iter()
    .find(|selection| runtime_selection_identity_matches(selection, &choice.selection))
    .filter(|selection| catalog_has_runtime_selection(catalog, selection));
let bounds = input.range_to_bounds(&attachment_range)?;
if visible_rows.contains(&buffer_row)
    && composer_attachment_bounds_are_visible(bounds, input_bounds)
{
    render_root_overlay(bounds);
}
```

## Scenario: Workspace-Less New Session Creation

### 1. Scope / Trigger

- Trigger: Desktop Agent session creation must remain reachable when no project
  or workspace has been opened yet.

### 2. Signatures

```text
workspace_ensure_temporary_session_root() -> string
CreateAgentSessionRequest.workspaceRoot -> string
CreateAgentSessionRequest.workspaceMode -> WorkspaceMode
```

### 3. Contracts

- "New session" is a global Agent action, not a workspace-scoped action.
- The sidebar new-session button must not be disabled just because the workspace
  list is empty.
- When the new-session form has no project path, the UI must call
  `workspace_ensure_temporary_session_root()` before `agent_create_session`.
- The returned path must be an existing directory; session creation continues
  through the normal `CreateAgentSessionRequest` contract.
- Existing workspace-scoped actions such as files, Git, and terminals should
  still guard on `workspaceId`.

### 4. Validation & Error Matrix

- No projects and no sessions after initial load -> show the new-session panel.
- Empty project path on create -> request a temporary session root, then create
  the session.
- Temporary root creation fails -> keep the panel open and render the mutation
  error in the form.
- User selects a project path -> use that path directly and do not request a
  temporary root.

### 5. Good/Base/Bad Cases

- Good: first app open with no projects lands on the new-session panel and the
  create action is enabled.
- Good: creating from an empty path creates a session backed by the temporary
  root and then selects the created workspace/session.
- Base: existing users with projects continue to see their selected workspace
  and can still choose another directory.
- Bad: disabling "New session" with `workspaces.length === 0`.
- Bad: passing an empty string as `workspaceRoot` to `agent_create_session`.

### 6. Tests Required

- Run `pnpm --filter @vibex/desktop typecheck` after Agent session creation UI
  changes.
- Run `pnpm check:frontend` after sidebar or new-session form changes.
- Run `cargo check -p vibex-desktop` when adding or changing the Tauri temporary
  root command.

### 7. Wrong vs Correct

#### Wrong

```tsx
<SidebarQuickAction disabled={creatingSession || workspaces.length === 0} />
```

#### Correct

```tsx
<SidebarQuickAction disabled={creatingSession} />
```

## Scenario: Worktree-aware New Session And Sidebar Projection

### 1. Scope / Trigger

- Trigger: GPUI Desktop adds an optional managed Worktree to New Session, or
  presents Project/Workspace/Session identity and concurrent Agent status.
- `DesktopRuntime` remains authoritative for Git and Workspace creation.
  `desktop-model` owns pure form/projection state; `apps/desktop` owns Backend
  calls and GPUI composition. Mobile may consume read-only identity but does
  not gain local Worktree mutation.

### 2. Signatures

```rust
NewSessionWorkspaceState {
    project_id, origin_workspace_id, fixed_workspace,
    preference, location, eligibility, generation,
    base_ref, worktree_name, worktree_path,
    name_touched, path_touched, submission
}

NewSessionProjectTicket { generation, project_id, origin_workspace_id, project_root }
NewSessionLocation::{CurrentCheckout, NewWorktree}
SidebarHierarchyMode::{Compact, Detailed}
sidebar_project_projections(workspaces, sessions, contexts, ...) -> Vec<SidebarProjectProjection>
WorkspaceContextProjection {
    project_id, project_name, workspace_id, workspace_mode,
    workspace_root, branch, managed_worktree_id, git_dirty
}
```

Persisted additive/defaulted UI fields:

```text
SidebarUiState.collapsedWorkspaceIds
SidebarUiState.hierarchyMode = compact
SidebarUiState.projectLocationPreferences
```

### 3. Contracts

- The Project picker contains each Project once and never mixes branch or
  Workspace rows into that list. Location, base ref, and Worktree settings are
  independent controls that share one form model at every viewport width.
- A missing Project preference means `CurrentCheckout`. An explicit preference
  switch may persist `NewWorktree`, but the one-shot location control never
  writes that preference. A detailed Workspace-row `+` fixes that exact
  Workspace and ignores the Project preference.
- Eligibility results apply only when the full
  `{generation, projectId, originWorkspaceId, projectRoot}` ticket still
  matches. Programmatic input synchronization also compares the incoming value
  with the model value before setting `nameTouched/pathTouched`; delayed GPUI
  Change events must not turn automatic previews into custom input.
- Worktree submission uses the Backend capability and exact sequence:

```text
expected-revision/idempotent Worktree create
  -> authoritative WorkspaceReady
  -> Session create at returned root/mode
  -> SessionReady/select
  -> initial prompt
```

- If Worktree creation fails, the complete form draft remains. If Session
  creation fails after `WorkspaceReady`, retry reuses the stored authoritative
  Workspace and idempotency key instead of creating another Worktree. If the
  initial prompt fails after the Session exists, move the original text,
  attachments, and command selection to that Session composer; never create a
  duplicate Session to retry a prompt.
- Compact and Detailed sidebars are projections of the same stable IDs and
  selection. Compact renders one Project plus flattened Sessions and adds
  `Worktree · <branch>` identity only to Worktree Sessions. Detailed renders
  one Project, Workspace rows, and their Sessions; Workspace rows expose
  branch, mode, dirty state, Agent summary, and an explicit `+`.
- Title/current context uses `WorkspaceContextProjection`, not a Project-root
  guess. Agent, Terminal, Files, Git, Search, Diagnostics, and Preview continue
  to route through the selected Session's authoritative Workspace ID/root.
- Render Worktree mutation controls only when the Backend advertises
  `GitWorktreeCreate`. Read-only clients may still render branch/mode identity.

### 4. Validation & Error Matrix

| Condition | Required UI/model result |
| --- | --- |
| No preference or a new Project | Select `CurrentCheckout`; do not inspect active Agent count. |
| Eligibility is pending or failed | Keep current checkout usable; disable Worktree with a short localized reason/retry. |
| Project is non-Git, bare, unborn, or lacks a base ref | Disable Worktree from the typed reason; do not inspect `.git` in UI code. |
| Late eligibility result targets an old ticket | Ignore it without changing location, base, name, or path. |
| Automatic input emits a delayed Change event with the same value | Keep touched flags unchanged. |
| Name/path is empty, unbounded, controlled, or custom path is relative | Disable submit and retain the draft; Backend validation remains authoritative. |
| Worktree succeeds and Session create fails | Keep `created_workspace`, prompt, attachments, runtime, settings, and key; retry starts at Session create. |
| Backend lacks `GitWorktreeCreate` | Hide mutation controls; keep read-only Workspace identity. |
| Hierarchy mode changes | Persist the mode while retaining selected Session/Workspace and workbench state. |

### 5. Good / Base / Bad Cases

- Good: rapidly select Project A then B; A's late Git result is ignored and B's
  manually edited Worktree name/path remain unchanged.
- Good: two running Sessions share one Worktree; both retain independent state,
  while the detailed Workspace row aggregates two running Agents.
- Good: Worktree creation succeeds, Session creation fails, and retry creates
  exactly one Worktree, Workspace, and Session.
- Base: a workspace-less first launch still creates a normal temporary-root
  Session through the existing current-checkout flow.
- Bad: put Worktrees in the Project picker, infer mode from path text, change
  location based on active Agent count, or rebuild selection when hierarchy
  mode changes.

### 6. Tests Required

- Pure model tests cover preference versus one-shot selection, stale ticket
  rejection, touched fields, custom input bounds, retry Workspace reuse, one
  Project for multiple Workspaces, and independent Agent aggregation.
- UI-state tests decode older JSON without the new fields and normalize stale
  Project/Workspace references and bounded preferences.
- GPUI source/interaction tests assert separate Project/location/base/settings
  controls, capability hiding, authoritative `WorkspaceReady` before Session
  create, prompt-draft transfer, hierarchy persistence, Workspace `+`, Worktree
  tooltip, and title context.
- Runtime/Git tests use temporary repositories for custom/default paths,
  eligibility revisions, idempotent create, nested/linked worktrees, and path
  ownership boundaries.
- Run `pnpm check:rust` with incompatible AppImage `PYTHONHOME/PYTHONPATH`
  variables removed when ACP mock tests need the system Python installation.

### 7. Wrong vs Correct

#### Wrong

```rust
if active_agents > 0 { location = NewWorktree; }
let workspace_id = WorkspaceId::new();
create_session(project.root_path, WorkspaceMode::VibexWorktree).await?;
```

#### Correct

```rust
let ticket = form.select_project(&project, &checkout, saved_preference, ...);
let eligibility = backend.git().git_worktree_eligibility(checkout.id).await?;
if form.apply_eligibility(&ticket, eligibility) {
    let created = backend.git().git_worktree_create(mutation).await?;
    form.mark_workspace_ready(created.workspace.clone());
    create_session(created.workspace.root_path, created.workspace.mode).await?;
}
```

The Project sidebar groups a registered Worktree beneath the Project that owns
its authoritative Workspace. Legacy databases may contain a second Project and
Workspace for the same normalized Worktree root and mode; presentation may
fold those aliases into the Workspace owned by a Project with a current
checkout and aggregate their Sessions there. This compatibility projection is
non-destructive and must not delete or rewrite stored Workspace references.

## Scenario: GPUI Agent Authentication Projection

### 1. Scope / Trigger

- Trigger: a user selects an Agent in the Management Center and the detail
  surface must show the current ACP authentication methods.
- Trigger: discovery, environment save, native Agent login, terminal login,
  logout, and terminal exit can complete out of order while the user changes
  Agent or authentication scope.
- The Agent detail surface must keep authentication understandable without
  exposing runtime verification, runtime-option, or Provider projection
  implementation panels.

### 2. Signatures

```text
auth_scope = (agent_id: String, provider_profile_id: Option<String>)

ManagementCenter {
  agent_auth_generation: u64,
  agent_auth_scope: Option<auth_scope>,
  agent_auth_catalog: Option<AgentAuthCatalog>,
  agent_auth_inputs: Map<(method_id, env_name), InputState>,
  agent_auth_operations: Map<agent_id, {
    operation_id, scope, method_id, phase: Running | Cancelling
  }>,
  agent_auth_terminal: Option<TerminalAuthActionDescriptor>,
  agent_auth_terminal_state: None | Running | Succeeded | Failed
}

load_agent_auth(force) -> BackendFuture<AgentAuthCatalog>
authenticate_agent(method_id, operation_id) -> BackendFuture<(AgentAuthCatalog, terminal?)>
cancel_agent_authentication(agent_id, operation_id) -> BackendFuture<bool>
logout_agent() -> BackendFuture<AgentAuthCatalog>
```

### 3. Contracts

- The catalog is rendered from the Agent's dynamic `AgentAuthMethod` list.
  `Agent`, `Environment`, and `Terminal` kinds each have their own action and
  input treatment; the UI never creates a generic API-key field for an Agent
  that did not advertise one.
- An auth callback may mutate state only when its generation and exact scope
  still match. Selecting another Agent/auth scope clears inputs, errors, catalog,
  terminal surface, and the old temporary terminal.
- Environment inputs are keyed by an unambiguous `(method_id, env_name)` value,
  masked when `secret`, and show only `configured` for an existing secret.
  Empty masked input means preserve; Clear is an explicit action. Plaintext
  values are sent only in the single backend mutation and are never copied to
  snapshots, Debug, or notices.
- The Agent page renders authentication first, followed by Provider Profile
  configuration. Runtime verification, runtime-option catalogs, and Provider
  projection internals are not rendered in the Agent detail surface.
- Logout is rendered only when the catalog advertises `supports_logout`.
  Discovery/auth errors are structured status text; they do not expose raw ACP
  envelopes or credential values.
- A terminal method opens a shared PTY surface with a bounded fixed panel size,
  retains the final output, shows running/succeeded/failed state, and treats
  non-zero or signaled exit as authentication failure. The temporary terminal
  is killed on close, scope change, runtime disconnect, or stale callback and
  is excluded from persisted workspace terminals.
- The UI exposes refresh/retry, loading, unavailable, not-verified,
  authentication-required, authenticated, and terminal-pending states without
  making a hover-only primary action.
- Authentication and logout use the existing Agent-keyed mutation lane, never
  the global Management mutation slot. A pending login blocks conflicting
  actions only for its target Agent; other Agents and their sessions remain
  usable. The operation task is retained independently from the currently
  selected auth scope so navigation cannot detach the backend work.
- While an Agent/environment authenticate call is waiting, that method's action
  becomes an accessible danger Stop action. The first click changes its phase
  to `Cancelling`, disables repeat clicks, and invokes the typed cancel request.
  A running terminal method uses the same Stop action and kills its temporary
  PTY. Successful cancellation clears pending state without presenting an error,
  after which the original sign-in action is available again.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| No runtime or disabled Agent | disable auth actions and show the unavailable state. |
| Discovery completes for an old scope/generation | ignore it; keep the current Agent/auth-scope state. |
| Environment method has no Profile | keep the action disabled and show the Profile-required state. |
| Required credential is explicitly cleared | refresh catalog, mark authentication required, and do not invoke ACP authenticate. |
| Selected method has a Running auth operation | render Stop; keep other methods for that Agent disabled. |
| Stop is clicked | transition to Cancelling once, cancel by exact operation id, and wait for authoritative completion. |
| Backend returns `agent_authentication_cancelled` | clear Agent-keyed pending state, keep auth required, and allow restart without an error notification. |
| Terminal callback is stale or terminal was closed | kill/release the old terminal if still owned; do not alter current auth state. |
| Terminal exits non-zero or by signal | show failed state and the structured exit diagnostic only. |
| Logout is not advertised | do not render the logout action. |

### 5. Good / Base / Bad Cases

- Good: selecting a new authentication scope while discovery is in flight leaves the
  new Profile's catalog intact when the old request returns.
- Good: two methods both contain a variable named `TOKEN`; their input state
  remains independent because the method id is part of the key.
- Good: a terminal login remains visible while running, then reports its final
  output and refreshes auth status after exit.
- Good: Codex browser login remains waiting while another Agent can be toggled
  or used; the Codex action becomes Stop and returns to Sign in after cancellation.
- Base: an Agent advertises no methods; the page shows a compact unavailable
  state and still allows Provider Profile management.
- Bad: show runtime validation or projection debug cards beside auth methods,
  fill a configured secret into an InputState, or let a stale callback replace
  the selected Agent's credentials.
- Bad: store login in `ManagementCenter.mutation`, disable every Agent action,
  or make closing the settings window the only way to abandon browser login.

### 6. Tests Required

- Desktop Management tests assert that the Agent renderer includes dynamic
  method kinds and authentication, while internal runtime panels are absent.
- Input-key tests assert `(method_id, env_name)` cannot collide and that masked
  configured values are represented by a placeholder only.
- Generation/scope tests assert stale discovery, mutation, logout, and terminal
  monitor callbacks do not mutate current state.
- Agent-keyed mutation tests assert auth reports its target Agent rather than
  occupying the global lane; renderer tests assert Running/Cancelling actions
  expose Stop and invoke `cancel_agent_authentication`.
- Terminal tests assert bounded surface creation, final output retention,
  success/failure/signal classification, and temporary-terminal cleanup.
- Run the locked `vibex-desktop`, `vibex-desktop-runtime`, and `vibex-terminal`
  tests plus dark-mode/responsive checks for the Management Center.

### 7. Wrong vs Correct

#### Wrong

```rust
self.auth_catalog = response.catalog;
self.auth_inputs.insert(variable.name, input);
self.mutation = Some(ManagementMutation::AgentAuth(method_id));
```

#### Correct

```rust
if response.generation == self.agent_auth_generation
    && response.scope == self.agent_auth_scope
{
    self.agent_auth_catalog = Some(response.catalog);
    self.agent_auth_inputs
        .insert((method_id, variable_name), input);
}
self.agent_mutations.insert(
    agent_id.clone(),
    ManagementMutation::AgentAuth {
        agent_id: agent_id.clone(),
        action: method_id.clone(),
    },
);
self.agent_auth_operations.insert(
    agent_id,
    AgentAuthPendingOperation {
        operation_id,
        scope,
        method_id,
        phase: AgentAuthOperationPhase::Running,
    },
);
```

The view owns transient presentation state; `DesktopRuntime` and the ACP
adapter remain authoritative for authentication and credentials.

## Scenario: GPUI Authentication-Source Cascade And Default Account Login

### 1. Scope / Trigger

- Trigger: the Composer's two-in-one or three-in-one runtime selector must let
  a user switch between Provider Profiles and the selected Agent's one default
  logged-in account, then choose a model or the Agent's own default behavior.
- `DesktopRuntime`/Backend owns catalog and authentication state. GPUI stores
  only the selected view, pending presentation state, and bounded desired
  request; mobile consumes the same projection through the remote backend.

### 2. Signatures

```text
RuntimeCascadeProjection {
  agents[], auth_sources[], models[], reasoning_efforts[], modes[]
}

ComposerRuntimeMenuView = Agent | AuthSource | Authentication | Model

RuntimeAuthSourceSummary {
  source, agent_id, label, kind, auth_source_revision,
  availability, account_hint?, model_catalog_status, supported_actions[]
}

RuntimeModelSelection = Explicit { model_id } | AgentDefault

New-session selector: AuthSource -> Model -> Effort -> Mode
Current-session selector: Agent -> AuthSource -> Model -> Effort -> Mode
```

### 3. Contracts

- The catalog's `auth_sources` list is independent of `options`. A source with
  no models, a stale catalog, or `RequiresAuthentication` must still render a
  stable source row and its status/action affordance.
- New-session menus skip the Agent level because the selected Agent is already
  implied by the surrounding composer. Current-session menus keep the Agent
  level and filter sources to that Agent. Both paths use the same
  `RuntimeCascadeProjection` and complete `SessionRuntimeSelection` values.
- Source rows show Provider Profile or AgentAccount semantics with distinct
  icon/label treatment. An AgentAccount row is the sole default account for
  that Agent, may show a bounded account hint, and must not expose Add account,
  Duplicate, account list, or account deletion controls.
- `Available` sources enter the model view. `RequiresAuthentication` opens the
  authentication view; `Verifying`/`DiscoveringModels` keeps the same row size
  with a progress state; temporary failure exposes retry; unsupported/config
  states remain actionable where the backend advertises an action.
- Browser/Agent-owned methods can complete in the popover. Terminal and
  Provider-required methods open Config Center, which owns PTY/environment
  input and the full login lifecycle. A successful login/verify returns to the
  model view only after the catalog is refreshed.
- If a source has no concrete model evidence, the model view shows the semantic
  label “Selected automatically by Agent”. The projection key is
  `agent-default`, but `RuntimeModelSelection::AgentDefault` is preserved in
  the request and no model sentinel is sent to ACP.
- Selecting a new source/model creates a complete desired selection with the
  catalog revision and stable idempotency key. It never locally changes
  effective state. Desired/effective differences render “Preparing”, “Waiting
  for current work”, “Needs sign-in”, or “Still using previous source”.
- Active turns are not reconfigured in place. The backend waits/rejects per its
  active-work policy; the UI does not disable unrelated Agents or fabricate a
  successful switch when the target fails.
- Context revision/catalog refresh invalidates old explicit model and effort
  choices. The UI clears an unavailable choice or asks for a fresh selection;
  it does not silently substitute a Provider or another account.
- Remote clients receive the same source/model projection and revisions. A
  read-only client may inspect but cannot authenticate or change the source;
  all permission checks remain server-side.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Source absent from current catalog | reject click/no mutation; refetch catalog. |
| Catalog revision changed after menu opened | backend CAS conflict; clear overlay and rebuild choices. |
| AgentAccount requires authentication | open authentication view; preserve old effective source. |
| AgentAccount has no model list | expose `AgentDefault`; never use `"default"` as a model id. |
| Login/verify changes context revision | refresh catalog and discard old model/effort selection. |
| Browser/terminal auth callback is stale | ignore it by auth generation and exact source/context id. |
| Runtime switch fails | keep old effective label and show actionable retry/login/configure state. |
| Read-only remote client attempts source/auth mutation | backend returns permission denial; local disabled state is convenience only. |
| Long label/account hint exceeds row width | truncate within a stable row; tooltip carries the bounded full label, never raw path. |

### 5. Good / Base / Bad Cases

- Good: new session opens the two-level menu, shows “Codex · default CLI
  account · Sign in”, completes browser login, refreshes models, and returns to
  the model list without creating a Provider Profile.
- Good: current session opens Agent -> AgentAccount -> AgentDefault, then later
  switches back to a Provider Profile; desired/effective feedback remains tied
  to the same logical session.
- Base: a Provider Profile exposes eight models while the Agent account exposes
  only AgentDefault; both source rows remain visible and independently usable.
- Base: terminal login routes to Config Center and returning to the composer
  preserves the selected Agent/source but reloads its status and catalog.
- Bad: hide an account source because its model list is empty, render a second
  account row for a relogin, set effective state optimistically, or silently
  fall back to a Provider after authentication failure.

### 6. Tests Required

- Shared GPUI model tests cover source grouping, AgentDefault projection key,
  model filtering by exact source, stable ordering, and revision invalidation.
- Desktop app tests cover two-level new-session and three-level current-session
  navigation, source status labels/icons, login subview, Config Center routing,
  and one-account-per-Agent rendering.
- Async generation tests cover stale catalog/auth callbacks, login completion,
  model refresh, failed switch feedback, and desired/effective reconciliation.
- Remote/client tests cover identical DTO projection, legacy capability
  filtering, read-only rendering, and permission-denied mutation handling.
- Responsive/dark-mode checks cover fixed menu row dimensions, long account
  hints, compact layout, and no overflow at desktop and phone widths.

### 7. Wrong vs Correct

#### Wrong

```rust
set_effective(selection);
let model_id = source.models.first().unwrap_or("default");
hide_source_when_models_empty();
```

#### Correct

```rust
let source = catalog.auth_sources_for(agent_id).find(source_id)?;
set_desired_runtime(catalog.revision, source, RuntimeModelSelection::AgentDefault);
render_status(source.availability);
// Backend commits effective state only after the source-specific switch is ready.
```

## Scenario: GPUI Management Section Lifetime

### 1. Scope / Trigger

- Trigger: a native management center contains independent dense forms and an
  Automation graph draft while async refreshes and mutations may finish out of
  order.

### 2. Signatures

```text
ManagementNavigation::switch(section, discard_dirty_current) -> bool
ManagementCenter::refresh() -> generation-fenced snapshot
AutomationGraphDraft::to_definition_request() -> Result<..., issues>
InputEvent::{Change, Focus, Blur, PressEnter { .. }}
```

### 3. Contracts

- Section entities live for the management center lifetime; switching sections
  does not recreate form state, pending mutation state, or retry context.
- A dirty section blocks navigation until an explicit discard confirmation. Each
  accepted switch increments a generation, and stale refresh completions are
  ignored.
- Management form subscriptions mark a draft dirty only for `InputEvent::Change`.
  `Focus`, `Blur`, and `PressEnter` are interaction events, not evidence that
  the input value changed; treating them as edits creates false discard prompts
  and can incorrectly mark credentials as touched.
- The graph draft is a pure model. Canvas controls only dispatch reducer actions;
  runtime/DB services execute and persist automation.
- Pairing, diagnostics, backup, and plugin projections contain bounded display
  values and never become a second source of durable state.

### 4. Validation & Error Matrix

- Dirty current section plus no confirmation -> switch returns `false`.
- Focus, blur, or submit without a value change -> keep the section clean.
- Value change -> mark the owning section dirty and preserve the draft until it
  is saved or explicitly discarded.
- Refresh completion with an old generation -> discard without mutating active
  section data.
- Invalid graph title/node/edge -> render validation issues and do not submit.
- CAS conflict -> keep the draft dirty and show reload/recovery guidance.

### 5. Good/Base/Bad Cases

- Good: editing a provider form, switching away, and returning preserves the
  input until the user confirms discard.
- Good: a refresh that completes after a section switch cannot replace the new
  section's visible error or draft.
- Base: a clean section can switch immediately and keeps authoritative data.
- Bad: subscribing to every `InputEvent` and marking the section dirty when a
  field merely receives focus.
- Bad: rebuilding a section entity on every tab click or clearing a form when a
  background query enters loading.

### 6. Tests Required

- Model tests cover blocked/confirmed section switches and generation increments.
- A GPUI event test emits `Focus` and asserts the section remains clean, then
  emits `Change` and asserts that the section becomes dirty.
- GPUI tests cover stale refresh fencing, dirty graph confirmation, and redacted
  Provider draft debug/serialization.
- Run the narrow layout/accessibility checks plus frontend and binding checks for
  any generated or state-contract change.

### 7. Wrong vs Correct

#### Wrong

```rust
self.active_section = next;
self.form = Default::default();

cx.subscribe(&input, |this, _, _: &InputEvent, _| {
    this.navigation.mark_dirty(section, true);
});
```

#### Correct

```rust
if self.navigation.switch(next, confirmed_discard) {
    self.generation = self.generation.saturating_add(1);
}

cx.subscribe(&input, |this, _, event: &InputEvent, _| {
    if matches!(event, InputEvent::Change) {
        this.navigation.mark_dirty(section, true);
    }
});
```

## Scenario: GPUI Desktop UI-State Persistence After Tauri Retirement

### 1. Scope / Trigger

- Trigger: changing GPUI desktop startup, persisted preferences, UI-state schema,
  corruption recovery, shutdown flushing, or backup behavior after the Tauri
  importer has been removed.
- The active contract is entirely Rust-owned. Browser storage, Tauri commands,
  generated import DTOs, and a retained exporter are not current inputs.

### 2. Signatures

```rust
UiStateStore::load_read_only(&self) -> Result<UiStateLoad, UiStateError>
UiStateStore::load_or_default(&self, now_ms: i64) -> Result<UiStateLoad, UiStateError>
UiStateStore::save(&self, state: &DesktopUiStateV1) -> Result<(), UiStateError>
UiStateStore::backup_snapshot(&self, backup_dir, now_ms)
    -> Result<UiStateBackupMetadata, UiStateError>
ThrottledUiStateWriter::queue(&mut self, state: DesktopUiStateV1, now_ms: i64)
ThrottledUiStateWriter::flush_if_due(&mut self, now_ms: i64)
    -> Result<bool, UiStateError>
ThrottledUiStateWriter::flush(&mut self) -> Result<(), UiStateError>
DesktopRuntime::ui_state_path(&self) -> PathBuf
```

### 3. Contracts

- `DesktopUiStateV1` in `<runtime-home>/desktop-ui-state.json` is the only
  persisted desktop presentation state. It contains bounded, non-secret
  preferences and references; SQLite remains authoritative for business data.
- Before `DesktopRuntime::start` acquires the selected home lock, the first frame
  may call only `load_read_only`. That path never writes, renames, quarantines,
  or prunes files.
- After runtime start owns the home lock, call `load_or_default`. Only a successful
  post-lock load may install `ThrottledUiStateWriter`; an unsupported schema or IO
  failure leaves persistence disabled so defaults cannot overwrite unknown data.
- Invalid JSON or invalid current-schema content is quarantined only by the
  post-lock load, with a bounded corrupt-backup set, then replaced in memory by
  normalized defaults. A future schema is returned as an error and is not
  quarantined.
- `save` normalizes a clone, writes a private temporary file, flushes it, atomically
  replaces the destination, and syncs the parent directory. Failed writes remove
  the temporary file best-effort and preserve the last valid destination.
- `backup_snapshot` copies the versioned UI-state file beside the business-data
  backup and returns bounded basename, size, schema, and SHA-256 metadata. It does
  not expose file contents or restore browser-storage values.
- Release rollback restores a published artifact and compatible backup. It never
  calls `desktop_ui_state_import`, reconstructs `DesktopUiStateImportRequest`, or
  reintroduces a Tauri/localStorage exporter.

### 4. Validation & Error Matrix

- Missing file -> normalized `DesktopUiStateV1::default()` with no recovery flag.
- Invalid JSON/current-schema value before lock -> defaults for the first frame,
  no filesystem mutation; quarantine remains pending.
- Invalid JSON/current-schema value after lock -> bounded corrupt backup plus
  defaults and `recovered_corrupt_state=true`.
- Unsupported future schema ->
  `validation/desktop_ui_state_version_unsupported`; do not write or quarantine.
- Normalization failure -> `validation/desktop_ui_state_invalid`; do not publish
  the temporary file.
- File IO or atomic replacement failure -> `storage/desktop_ui_state_io`; retain
  the previous destination and report a bounded persistence note.
- Home already owned -> `desktop_runtime_home_locked`; do not create a writer or
  start a second authoritative runtime.

### 5. Good/Base/Bad Cases

- Good: the first frame reads state without mutation, runtime startup acquires the
  home lock, performs the authoritative load, and then enables a throttled writer.
- Base: no UI-state file exists; defaults render and the first post-lock change
  creates the versioned file atomically.
- Bad: quarantine during first-frame preload, enable a writer after a future-schema
  error, treat UI references as business truth, or import deleted WebView storage.

### 6. Tests Required

- `cargo test -p vibex-desktop-model ui_state --locked` covers v0 migration,
  normalization, read-only behavior, corrupt quarantine/pruning, atomic save,
  throttled flush, and redacted backup metadata.
- `cargo test -p vibex-desktop-runtime --lib --locked` covers exclusive home-lock
  ownership and shutdown release.
- `cargo check -p vibex-desktop --locked` proves current shell wiring.
- `pnpm check:legacy-cutover` proves retired import DTOs, commands, paths, and
  dependencies do not return to active source.

### 7. Wrong vs Correct

#### Wrong

```rust
let load = UiStateStore::new(path).load_or_default(now_ms)?; // no home lock
let writer = ThrottledUiStateWriter::new(UiStateStore::new(path), 200);
```

#### Correct

```rust
let preload = UiStateStore::new(ui_state_path(&config.home_dir)).load_read_only()?;
let runtime = DesktopRuntime::start(config).await?; // owns the home lock
let load = UiStateStore::new(runtime.ui_state_path()).load_or_default(now_ms)?;
let writer = ThrottledUiStateWriter::new(
    UiStateStore::new(runtime.ui_state_path()),
    200,
);
```

## Historical Evidence: Retired Tauri UI-State Export

> **Retired at Checkpoint 2 (2026-07-29).** The complete scenario below records
> the one-time pre-cutover bridge. Its files came from the former Tauri shell that
> once occupied `apps/desktop`; those files, TypeScript signatures, Tauri commands,
> import DTOs, and test commands no longer exist and must not be implemented or
> used for rollback. The active replacement is the Rust-only persistence scenario
> above.

### 1. Scope / Trigger

- Trigger: the final Tauri shell must hand off persisted desktop preferences to
  the isolated GPUI Preview/RC shell during the release observation window.
- This is a frontend/backend boundary: browser storage is read in React, but
  Rust owns interpretation, validation, persistence, and rollback safety.

### 2. Historical Signatures (Retired)

```typescript
collectFrozenDesktopUiStateEntries(localStorage, sessionStorage)
  -> DesktopUiStateStorageEntry[] // exactly 25 frozen keys
exportTauriDesktopUiState(options?) -> Promise<DesktopUiStateImportResult>
api.desktopUiStateImport(DesktopUiStateImportRequest)
```

```text
localStorage/sessionStorage -> typed Tauri command -> desktop-ui-state.json
```

### 3. Contracts

- `FROZEN_DESKTOP_UI_STORAGE_KEYS` is the single frontend inventory. Every key
  is emitted even when its value is `null`; the exporter never calls
  `removeItem` and never scrapes WebView storage files.
- Transport shapes come only from the canonical `crates/core` DTOs. The request carries import
  schema, source shell/version, export timestamp, entries, mode, and optional
  checksum/reference fields; UI code does not redefine them.
- `first_import` may be retried idempotently for the same checksum. A changed
  snapshot returns `reimport_available` until an explicit `reimport` is chosen;
  `reset` is explicit. Session runtime ids, submission locators, and secrets are
  observed for checksum/inventory purposes but are not restored as durable GPUI
  state.
- Preserve the frozen keys' real encodings: `vibex.sidebarSessionOrder` is a
  workspace-to-session-order object, the legacy right-rail order uses
  `files|git|terminal`, and `vibex.rightRailPluginOrderMigrated` uses `"1"` as
  its completed marker. The Rust adapter maps these to canonical GPUI state.
- Browser fixture mode uses the same typed API/mock shape, while real Tauri
  runtime invokes the command. Migration errors keep the existing Tauri UI and
  legacy storage intact; status/error text is bounded and actionable.
- Before the runtime home lock exists, GPUI may only call
  `UiStateStore::load_read_only`; that first-frame preload cannot quarantine,
  replace, or flush a file. Import, corruption quarantine, authoritative SQLite
  reference lookup, and writer creation happen only after
  `DesktopRuntime::start` owns the selected home lock. A future-schema/read
  failure leaves the writer disabled so defaults cannot overwrite the file.
- GPUI channel selection is explicit and compiled into packaged artifacts.
  Preview and RC use isolated homes; a runtime variable cannot change an
  already packaged channel. An unchannelled development binary may opt into RC,
  but Stable requires an artifact built with `VIBEX_CHANNEL=stable` after
  transfer approval and uses the `desktop-stable` copy. No frontend
  selector may infer stable ownership.

### 4. Validation & Error Matrix

- Non-Tauri exporter call -> reject before reading storage.
- Missing/duplicate/frozen-key or medium drift -> typed backend validation error;
  no localStorage mutation occurs.
- Browser storage value is malformed -> Rust falls back by section and returns
  `invalidKeys`; the whole workbench is not discarded.
- Tauri command failure -> preserve the current browser state and expose a
  bounded retry/status path; never surface raw paths, secrets, or provider text.
- Unknown optional keys -> report in `ignoredKeys` and keep known preferences.
- Packaged/runtime channel mismatch -> `release_channel_override_rejected`;
  runtime-only Stable selection -> `stable_channel_requires_release_build`.

### 5. Good/Base/Bad Cases

- Good: a Tauri startup exports all 25 entries, leaves legacy keys untouched,
  and a typed RC import restores theme, widths, ordering, selected ids, and
  valid preview targets after stale references are removed while the isolated
  home lock is held.
- Base: a missing optional value yields a GPUI default while the other sections
  import successfully.
- Bad: copy only the Zustand blob, delete localStorage after export, cast an
  `unknown` payload in a component, or let a browser id list decide database
  existence.
- Bad: import or quarantine before `DesktopRuntime::start`, construct a writer
  before the lock is held, or let a runtime environment variable upgrade an RC
  artifact to Stable.

### 6. Tests Required

- Typecheck the desktop app and run binding drift after request/response changes.
- Assert the exporter inventory count/uniqueness and no `removeItem` calls.
- Browser mock tests exercise first import, reimport-required, reset, ignored
  keys, and command failure without starting a provider.
- Rust migration tests remain authoritative for checksum, atomicity, stale-id,
  corruption, and business-database non-mutation guarantees.
- Assert read-only preload leaves corrupt bytes untouched; runtime/GPUI tests
  assert post-lock reload, channel mismatch rejection, and Stable build-only
  selection.

### 7. Wrong vs Correct

#### Wrong

```typescript
const state = JSON.parse(localStorage.getItem("vibex-workbench") ?? "{}");
await invoke("desktop_ui_state_import", { state });
localStorage.clear();
```

#### Correct

```typescript
const request: DesktopUiStateImportRequest = {
  export: { schema, sourceShell: "tauri", sourceAppVersion, exportedAtMs, entries },
  mode: "first_import",
  expectedChecksum: null,
  references: emptyReferenceFixture
};
return api.desktopUiStateImport(request);
```

## Scenario: Shared Agent/File/Git Workflow Controller And Safe Remote Projection

### 1. Scope / Trigger

- Trigger: Native GPUI desktop and native GPUI mobile need the same Agent timeline,
  approval, file editor, and Git review behavior over either `NativeBackend` or
  `WebRemoteBackend`.
- The shared controller lives in `vibex-ui`; it may depend on
  `desktop-model` projections but must not depend on `DesktopRuntime`, HTTP
  envelopes, React, or provider-native payloads.

### 2. Signatures

```rust
AgentFileGitController::from_facade(&BackendFacade)
AgentWorkflowController::begin_session_load(session_id) -> AgentSessionLoadTicket
AgentWorkflowController::load_session(ticket) -> BackendFuture<AgentSessionSnapshot>
FileWorkflowController::begin_save_active() -> FileSaveOperation
FileWorkflowController::save_file(operation) -> BackendFuture<FileSaveOutcome>
FileWorkflowView { selected_path, active_file, editor_content, editor_base_revision, status, conflict }
GitWorkflowController::request_commit_confirmation(message, paths)
GitWorkflowController::begin_confirmed_commit() -> GitMutationOperation
```

### 3. Contracts

- `AgentSessionLoadTicket` carries a monotonic view generation and session id;
  stale completions cannot mutate the active view. Timeline restore pages from
  sequence `0` until `has_newer=false`, with a bounded 20,000-item projection.
- Timeline live events are sequence/session fenced; gaps mark authoritative
  refetch, and reconnect/disconnect is represented separately from RPC failure.
- Pending approvals are derived from authoritative timeline items. Resolution
  requests must match an allowed response and are coalesced by request id;
  duplicate authoritative request rows project one approval surface.
- Every file selection advances the workflow generation. A read completion is
  accepted only for that generation/workspace/path, and an edit/save may use a
  buffer only when its path still matches the active file. This prevents a late
  text read or a previous text buffer from becoming active after selecting a
  binary/image file. Reopening a cached dirty path uses `observe_external` and
  projects `{editor_content, editor_base_revision}` separately from the server
  snapshot, so a read never silently overwrites local edits.
- File saves send both `FileWriteRequest.expected_revision` and the mutation
  idempotency/revision envelope. A conflict retains local text and exposes a
  redacted-metadata comparison plus an explicit server reload action.
- Shared file editing accepts only UTF-8 text up to 1 MiB. Binary, truncated,
  and larger files are read-only/unsupported in this workflow.
- Git v1 exposes only status, diff, stage, unstage, and explicitly confirmed
  commit. Push, revert, branch/worktree, and history-rewrite actions are not
  represented by the shared controller.
- Git queries and mutations are fenced before backend execution and again when
  applying the response. Mutation identity is
  `{workspace_id, view_generation, operation_id}`; matching only the workspace
  is insufficient because a late completion can otherwise finish a newer
  mutation in the same workspace. Diff/status/commit responses must also match
  the requested path/staged flag or workspace.
- `AgentFileGitCapabilities` filters the backend snapshot for this v1 surface;
  remote capability discovery also omits file move/delete. Server authorization
  remains authoritative and is never replaced by client visibility.
- Shared workflow state/view, save outcome, approval, and commit-operation
  `Debug` implementations expose ids, phases, counts, byte lengths, and stable
  error codes only. Timeline text, file contents, diff lines, approval details,
  paths selected for commit, and commit messages must not enter logs/evidence.

### 4. Validation & Error Matrix

- View generation differs -> ignore result and keep the current projection.
- Timeline page/session/sequence mismatch -> `agent_timeline_page_invalid` or
  `agent_timeline_session_mismatch`; do not render the page as authoritative.
- Timeline exceeds the bounded projection -> `agent_timeline_limit_exceeded`.
- Permission is already resolved or has a disallowed response ->
  `agent_permission_not_pending` / `agent_permission_response_not_allowed`.
- File content exceeds 1 MiB -> `file_edit_too_large`; no local mutation is
  applied.
- File CAS mismatch -> `Conflict/file_revision_conflict`; never overwrite the
  server version silently.
- File read/save response generation, workspace, or path mismatch -> ignore the
  stale read or record `file_save_response_mismatch`; never activate the wrong
  file/buffer.
- File save operation no longer matches the active generation/path/pending save
  id -> `file_save_generation_stale`; do not call the backend write, and clear
  only that inactive buffer's pending marker when its stale outcome is observed.
- Git commit without confirmation -> `git_commit_confirmation_required`.
- Stale/non-current Git query or mutation -> `git_query_generation_stale` /
  `git_mutation_generation_stale`; no backend mutation is started for the stale
  operation.
- Git diff/status/commit response target mismatch ->
  `git_diff_response_mismatch` / `git_status_response_mismatch` /
  `git_commit_response_mismatch`; clear only the matching pending operation.
- Filtered dangerous operation -> `<operation>_unsupported`, even when a wider
  native facade has that capability for another desktop surface.

### 5. Good / Base / Bad Cases

- Good: the same `AgentFileGitController` is constructed from a Native or Remote
  facade; only the backend implementation changes, while view models and
  generation rules remain identical.
- Base: runtime-selection metadata is unavailable but the authoritative Agent
  timeline still loads and renders.
- Good: selecting a binary file after a text file leaves the prior buffer cached
  but not editable, and the active status becomes `Unsupported`.
- Bad: a stale file read replaces the newly selected file, a prior Git operation
  id completes a newer mutation, a conflict retries with a new revision, or a
  Compact UI calls `delete_path` directly.

### 6. Tests Required

- Controller tests assert stale generation rejection, complete timeline paging,
  sequence-gap refetch, approval prominence/deduplication, and session mutation
  fencing.
- File fixture tests assert tree/search bounds, 1 MiB enforcement, CAS conflict,
  local/server comparison, explicit reload, stale open rejection, and binary
  read-only status. A stale-save fixture must prove the backend file was not
  written and the old pending marker was released.
- A temporary real Git repository test asserts status/diff/stage/unstage and
  confirmed commit; the test must leave no changes in the developer worktree.
- Git model tests forge a stale operation id/workspace and mismatched diff path;
  they must be rejected without reaching the backend or changing the current
  mutation.
- Sentinel Debug tests format workflow state, approval, file conflict/save, and
  commit models and assert user text/file contents/diffs/messages are absent.
- Native mobile checks compile the controller graph; remote capability tests assert file
  move/delete are not advertised; Native desktop checks exercise the same
  facade construction path.

### 7. Wrong vs Correct

```rust
// Wrong: a view calls a provider/runtime, retries a stale save blindly, or
// applies a mutation using only the workspace as its fence.
runtime.files().write(&request).await?;
send_again_with_a_new_revision_after_timeout().await?;
if operation.workspace_id == active_workspace { apply(result); }

// Correct: the controller preserves CAS and the complete mutation identity.
let operation = files.begin_save_active()?;
let outcome = files.save_file(operation.clone()).await?;
files.apply_save_outcome(&operation, Ok(outcome));
if operation.generation == generation
    && pending_operation_id == operation.operation_id
{
    git.apply_paths_mutation(&operation, result);
}
```

## Scenario: Shared Terminal And ManagementCenter Workflow

### 1. Scope / Trigger

- Trigger: Native GPUI desktop and native GPUI mobile need one recoverable Terminal
  surface and one compact ManagementCenter over Native/Remote Backend facades.
- Portable ANSI presentation is owned by `vibex-terminal-ui`; PTY/process and
  socket ownership remain in the backend. The shared controller consumes raw
  `TerminalFrameBatch` bytes and never treats provider/config payloads as UI state.

### 2. Signatures

```rust
TerminalWorkflowController::refresh(workspace_id)
TerminalWorkflowController::attach(terminal_id)
TerminalWorkflowController::poll() -> BackendFuture<bool>
TerminalWorkflowController::send_input(TerminalInput)
TerminalWorkflowController::resize(rows, cols)
TerminalRawBuffer::apply_batch(&TerminalFrameBatch)
ManagementWorkflowController::refresh()
ManagementWorkflowController::create_pairing_offer(request)
ManagementWorkflowController::cancel_pairing_offer(offer_id)
ManagementWorkflowController::revoke_device(request)
```

### 3. Contracts

- Terminal output remains raw bytes until the shared emulator consumes it. Frame
  sequence, terminal id, `reset_required`, dropped-frame count, and generation are
  all fenced before presentation; a gap/rewind clears the incremental buffer and
  rebuilds from the returned batch rather than appending across a discontinuity.
- Raw frame memory is bounded by both frame count and byte budget. Eviction is
  represented in metadata and never exposes output bytes through `Debug`.
- Terminal `list/create/attach/input/resize/close` operations require the filtered
  capability snapshot. A read-only paired device can attach and render but cannot
  send input, and a disconnected/reconnecting state never invents a local shell.
- Compact key-bar actions include Esc, Ctrl, Tab, arrows, Enter, and Backspace;
  every action has a >=44px target and is not hover-only. Ctrl latch state belongs
  to the controller, so keyboard blur does not lose the modifier.
- Host safe-area and keyboard insets reduce the visible terminal viewport. The
  controller keeps the recent output region inside that bounded visible height.
- ManagementCenter v1 projects only Agents, redacted Provider profiles, health,
  Relay status, paired devices, audit count, and short-lived pairing offers.
  Provider secrets, raw config, public keys, launch challenges, and raw audit
  payloads are not retained in shared state/debug output.
- Management section switches preserve long-lived state and require explicit
  discard when the current section is dirty. Refresh and mutation completions are
  generation/operation fenced; stale responses cannot replace a newer section.
- Pairing offers are server-generated and short-lived. Cancel/revoke actions are
  capability-gated and delegated to the authoritative runtime; Compact displays
  them as explicit touch actions.
- Remote clients may list redacted Provider profiles and health when authorized,
  while profile selection, device administration, and pairing creation remain
  unavailable unless the backend advertises the corresponding operation.

### 4. Validation & Error Matrix

- Terminal target/sequence mismatch -> `terminal_frame_target_mismatch` /
  `terminal_frame_sequence_gap`; do not append the batch.
- `reset_required` or dropped-frame increase -> rebuild the emulator before
  applying returned bytes; never claim a contiguous incremental stream.
- Raw frame count/bytes exceed budget -> evict oldest complete frames and retain
  bounded metadata.
- Input on a read-only or unavailable capability -> structured permission or
  unsupported error; no backend write is attempted.
- Dirty Management section without explicit discard -> switch returns `false`.
- Pairing/device response from an old operation -> `management_operation_stale`;
  retain the current offer/device list.
- Debug/evidence containing terminal bytes, pairing fragment/challenge, public
  key, provider secret, or raw audit text -> fail the redaction gate.

### 5. Good / Base / Bad Cases

- Good: Native and Remote controllers receive identical raw frame batches and
  produce the same provider-neutral view; only the backend transport differs.
- Good: a slow client reaches a dropped-frame gap, rebuilds, and resumes with a
  bounded raw buffer and no duplicated bytes.
- Base: Remote profile selection is unavailable and the UI renders a structured
  capability state rather than opening a local config file.
- Bad: convert terminal output to lossy UTF-8 before the emulator, create a local
  mobile shell, expose a pairing challenge in `Debug`, or let a stale health/device
  response overwrite a newer ManagementCenter generation.

### 6. Tests Required

- Portable emulator tests cover ANSI/CJK/frame projection on native and the
  platform-neutral fallback compiles without `polling`, `home`, PTY, or runtime crates.
- Terminal controller tests cover contiguous replay, rebuild/gap, bounded raw
  memory, read-only input rejection, key-bar touch discoverability, and keyboard
  viewport math.
- Management model tests cover dirty-section confirmation, capability filtering,
  profile/device/health redacted projections, pairing cancel/revoke fencing, and
  sensitive `Debug` output.
- Remote backend tests assert redacted profile/health capabilities and that device
  administration/profile selection are not advertised when unsupported.
- Run the locked Terminal/UI/Remote tests, native mobile gate, and
  workspace Rust quality gate before commit.

## Scenario: Current-Worktree Changes Lifecycle And Conflict Projection

### 1. Scope / Trigger

- Trigger: GPUI Changes or sidebar code renders managed Worktree readiness,
  merge/lifecycle actions, a target-owned conflict, or Agent assistance.
- `GitWorktreeLifecycleSnapshot` is the single authoritative input for both the
  detailed Changes surface and compact Workspace/Session identity. The view
  never owns a second operation state or infers Git completion from a toast.
- Desktop is the mutation surface. Mobile may render negotiated read-only
  lifecycle state but never expose local Worktree mutation controls.

### 2. Signatures

```text
WorktreeLifecycleView {
  workspaceId,
  managed: GitManagedWorktreeRecord?,
  readiness: GitWorktreeReadinessRecord?,
  operation: GitWorktreeOperationRecord?,
  targetOwned: bool,
  state: Working | Reviewing | Ready | Queued | Merging |
    NeedsResolution | Aborting | Archiving | Archived | Restoring |
    Discarding | Discarded | Failed | NeedsAttention
}

WorktreeLifecycleView::from_snapshot(workspaceId, snapshot)
  -> WorktreeLifecycleView?

WorktreeLifecycleConfirmation =
  Merge(GitWorktreeMergePlan)
  | Archive { request, preflight }
  | Restore { request, preflight }
  | Discard { request, preflight }
  | Continue(GitWorktreeOperationRequest)
  | Abort(GitWorktreeOperationRequest)

WorkspaceContextProjection {
  workspaceId, workspaceMode, workspaceRoot, branch,
  managedWorktreeId, gitDirty, worktreeLifecycleState
}
```

Capability contract:

```text
GitWorktreeRead             -> fetch/render lifecycle snapshot
GitWorktreeLifecycleMutate  -> readiness, merge, conflict, archive/restore/discard controls
```

### 3. Contracts

- Code Workbench loads lifecycle state on Workspace activation and with Git
  refresh. Results are fenced by the Workspace generation. If a refresh is
  already running, set one reload request and fetch again after completion;
  concurrent polling must not drop the newest state or spawn an unbounded task
  set.
- Every lifecycle snapshot, plan/preflight query, readiness update, and mutation
  completion captures the exact `{workspaceId, generation}` before dispatch and
  may change pending/error/confirmation/snapshot state only while that fence
  still matches. A Workspace switch resets the task slots and pending state;
  the old completion is ignored rather than applied to the new Workspace.
- `WorktreeLifecycleView::from_snapshot` first selects a visible operation owned
  by the current target Workspace, then a source operation. A normal checkout
  therefore has no managed header, while the same checkout still receives a
  persistent conflict/needs-attention banner when it owns the target operation.
- The Changes header shows managed branch, fixed target, readiness, dirty state,
  exact-head summary, and concrete lifecycle actions. Confirmation state stores
  typed plans/preflights, not reconstructed labels or booleans. Changed plan
  facts clear stale confirmation and require another review.
- `Queued` remains visibly actionable through `Review merge`, which requests a
  fresh typed merge plan. Reaching the front never auto-executes the previously
  confirmed plan; changed target facts require another explicit confirmation.
- Conflict rows are derived from the target-owned operation and render before
  ordinary Changes. Their typed category and binary state come from Core DTOs.
  Active conflict paths are removed from the ordinary tree and generic
  stage/commit selection; target/source version selection and stage use the
  operation-scoped Backend methods.
- Continue stays disabled until the authoritative operation has no unmerged
  entries. Continue and Abort each show a named confirmation. Abort copy states
  that only the target resolution scene is discarded and the source Worktree is
  unchanged.
- Merge conflict/needs-attention completion focuses the target Workspace's
  Changes surface. Agent assistance creates or reuses the operation-associated
  target Session, injects bounded structured context with a deterministic key,
  and uses its own task slot. It must not set global `agent_action_pending` or
  prevent unrelated Sessions from running.
- Sidebar detailed rows and compact Worktree icons consume
  `WorkspaceContextProjection.worktreeLifecycleState`. Pending/running Archive,
  Restore, and Discard keep their own labels instead of appearing as Merge.
  Unknown/ambiguous states render NeedsAttention and remain actionable through
  the detailed Changes surface.
- Rendering `GitWorktreeRead` without `GitWorktreeLifecycleMutate` is valid:
  branch/readiness/conflict state remains visible, while every mutation and its
  confirmation entry point is absent, including conflict resolution, staging,
  and Agent-assistance controls. Read-only users may still inspect conflict
  diffs and open a terminal. Capability hiding is presentation only; the Backend
  still rejects unsupported calls.

### 4. Validation & Error Matrix

| Condition | Required result |
| --- | --- |
| Lifecycle response belongs to an older Workspace generation | Ignore it; preserve the current Workspace snapshot. |
| Lifecycle plan/readiness/mutation completes for another Workspace ID or generation | Ignore the entire completion, including pending/error/confirmation changes; the current Workspace owns its own task state. |
| Refresh is requested while one is loading | Coalesce one follow-up load; do not lose it or run parallel unbounded loads. |
| Current checkout is neither managed nor a target | Render ordinary Changes with no lifecycle header/banner. |
| Current checkout owns NeedsResolution/NeedsAttention | Render the persistent target banner and conflict rows even without a managed source record. |
| Conflict path also appears in ordinary Git status | Show it only in the typed conflict section; exclude it from generic selection. |
| Backend loses `GitWorktreeLifecycleMutate` | Remove mutation controls/confirmation; retain read-only projection when `GitWorktreeRead` remains. |
| Merge/preflight returns stale-plan error | Clear old confirmation, refresh lifecycle, and require a new plan. |
| A queued merge reaches the front or its target facts change | Keep a visible merge-review action, fetch a fresh plan, and require explicit confirmation before execution. |
| Assistance operation/Session fence is invalid | Show the stable Backend error; do not fall back to another Workspace or global Agent lock. |
| Continue/Abort succeeds or conflict is returned | Refresh status and lifecycle, refresh sidebar projection, and focus target Changes when attention is required. |
| Lifecycle state is unknown | Render NeedsAttention; never treat it as Working, Ready, or success. |

### 5. Good / Base / Bad Cases

- Good: a managed source is marked Ready, reviews an exact merge plan, enters a
  conflict, and the UI moves to the target Changes surface where typed conflict
  rows precede ordinary changes.
- Good: the user asks an Agent for help; the associated target Session opens,
  other Sessions keep running, and only the explicit Continue confirmation can
  finalize the merge.
- Good: a remote read-only client displays `Archived` or `NeedsResolution`
  without showing Archive, Discard, Continue, or Abort controls.
- Base: a CurrentCheckout Workspace with no managed or target-owned operation
  behaves exactly like the ordinary Git Changes surface.
- Bad: keep a page-local merge status after navigating away, infer conflicts
  from `GitChangeKind` alone, stage active conflicts through generic selection,
  reuse `agent_action_pending` for assistance, or render a disabled mutation
  control on a client that cannot call it.

### 6. Tests Required

- Desktop-model tests assert target-operation precedence, source/target projection
  from one snapshot, full lifecycle-state ordering, and distinct
  Archiving/Restoring/Discarding labels.
- Git Workbench model tests assert conflict paths precede ordinary rows and are
  excluded from generic select-all, stage, and commit path sets.
- GPUI source-contract tests assert managed header, persistent target banner,
  typed conflict actions, named confirmations, stale-plan refresh, exact
  Workspace ID/generation fences for reads and mutations, queued merge review,
  capability gates, and Agent assistance's dedicated task/deterministic message
  contract.
- Backend fixture tests keep Native, disconnected, and Remote Git trait surfaces
  exhaustive; remote tests assert read capability never implies lifecycle
  mutation.
- Run Desktop/model/UI tests, `pnpm check:rust`, and
  `pnpm check:code-workbench`. When physical visual capture is deferred, evidence
  must explicitly remain `model_passed_visual_pending` rather than claiming a
  passed screenshot review.

### 7. Wrong vs Correct

#### Wrong

```rust
self.merge_state = Some("conflict".into());
self.git.select_path(conflict_path, false);
self.agent_action_pending = true;
render_merge_button_even_when_remote();
```

#### Correct

```rust
let view = WorktreeLifecycleView::from_snapshot(&workspace.id, &snapshot);
self.git.set_lifecycle_conflict_paths(target_conflict_paths(&view));

if capabilities.git.supports(BackendOperation::GitWorktreeLifecycleMutate) {
    render_typed_lifecycle_actions(view);
}

start_operation_assistance_task_without_blocking_other_agent_sessions();
```

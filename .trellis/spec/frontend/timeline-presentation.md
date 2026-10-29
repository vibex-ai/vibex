# Shared Timeline Presentation

## 1. Scope / Trigger

Read this when changing timeline activity identity, details, grouping,
reasoning preferences, or disclosure motion in `apps/desktop`, `apps/mobile`,
or `crates/vibex-tui`.
Desktop remains the visual and semantic baseline. Mobile adapts it to touch;
TUI retains character-grid navigation and incremental Markdown rendering.
The desktop runtime remains the sole owner of session state.

## 2. Signatures

Portable APIs live in `crates/vibex-ui` and compile with no default features:

```rust
Activity::project(&TimelineRow, Option<&TimelinePayload>, Locale) -> Activity
ActivitySummary::record(&mut self, &Activity, row_id: &str)
ActivitySummary::label(&self, Locale) -> String
group_open(&TimelineConversationTurn, &TimelineProcessActivityGroup, Option<bool>) -> bool
is_running(&TimelineRow, turn_live: bool) -> bool
tool_detail::project(&TimelineRow, Option<&TimelinePayload>, Option<&str>, Locale) -> Vec<Detail>
tool_detail::text(&[Detail]) -> String
AgentWorkflowState::is_turn_live(&self) -> bool
AgentWorkflowState::conversation_turns_with_reasoning_mode(&self, ReasoningDisplayMode)
    -> Vec<TimelineConversationTurn>
```

`Activity` exposes action, target, full path, icon kind, and failure through
getters. `Detail` exposes its label, bounded source, readable text, copy text,
metadata classification, and file-content classification. Neither type owns UI
state or platform handles. `REASONING_WINDOW_LINES` is six.

TUI presentation state lives in `crates/vibex-tui/src/transcript.rs`:

```rust
Transcript::set_reasoning_preferences(&mut self, bool, ReasoningExpansionMode)
Transcript::toggle_block(&mut self, usize) -> bool
Transcript::reveal_block(&mut self, usize)
Transcript::advance_disclosures(&mut self, Instant) -> bool
```

## 3. Contracts

- Project the latest typed payload belonging to the row. Mobile uses the
  sequence-sorted timeline for lookup; TUI builds an item lookup per sync.
- Action and target stay separate. Files use basenames for display and full
  paths for identity, accessible labels, details, and copy. A generic tool name
  with no useful summary falls back to its recognized command/path/query input.
- Prefer bounded captured output, even when explicitly empty. Decode known
  result envelopes; preserve unknown JSON fields and non-text blocks.
  Input copy returns the original arguments. Successful exit codes and the
  current workspace directory are omitted; failures and different directories
  remain visible.
- Detail formatting is lazy. Hidden TUI rows do not parse output on every
  delta. Expanded height estimates use detail sources, and search/copy use the
  same readable details as rendering. Content and locale participate in cache
  identity, including same-sequence or equal-length corrections.
- Mobile activity headers use full-width standard Buttons, three-rem touch
  targets, semantic icons, and muted text. Explicit copy controls sit beside
  bounded detail scroll regions. File detail preferences change content inside
  the activity group.
- Explicit expansion/collapse wins over live defaults and survives refresh.
  Mobile opens active groups by the shared desktop rule. Individual tool
  details stay collapsed until requested: a TUI tool call that settles while
  the reader is following the tail keeps its compact row instead of unfolding
  its payload.
- TUI groups three or more settled typed activities by turn and runtime,
  summarizes all action categories, and deduplicates changed files by full
  path. Running, failing, or approval-pending activities stay visible.
  Each group has a synthetic `activity-group:{first_member_id}` summary that
  remains mounted when open. Its three levels are summary, child headers, and
  individually expanded details. Opening a detail does not split the group;
  closing the summary resets every child detail. Retain both levels by stable
  ID during updates; derive parent indices separately from content cache keys.
  History budgets and prepend detection count source rows, excluding synthetic
  summaries, so loading older history preserves the requested page.
- Opened detail is fetched, not shipped. A page or live event carries
  `TimelinePayloadDepth::Summary`, which keeps every field a collapsed activity
  row renders and withholds the parts only an opened one shows. An activity
  whose item is not complete has detail to read, so a client must keep it
  openable even when the stripped payload projects to nothing, and must read it
  back by sequence. Prefetch older history before the reader reaches the top;
  waiting for the literal top shows an empty band for the length of a round
  trip.
- Group and detail connectors use the one-column `glyphs::connector` (`│` or
  ASCII `|`). The outer line stops at the last child header; the inner line
  belongs to that child's details. Account for both indents in wrapping and
  pointer selection, using spaces in copied display text. Whole-transcript
  export and search skip synthetic summaries; search reveals the containing
  group and the matched child. Batch collapse maps a hidden selection to its
  visible summary. Mouse hit tests only activate visible headers.
- TUI disclosure motion lasts 180 ms with cubic ease-out, reverses from the
  currently displayed progress, and stops after drawing its resting frame.
  The main loop advances it by `Instant` at frame cadence independently of
  the slower chrome animation phase. Cache full rendered content and clip
  layout extents during motion; do not parse detail bodies on each frame.
  Closing children remain drawable until the parent's reveal ends. Turning
  **Motion** off settles active disclosures immediately.
- TUI uses the shared desktop turn projection for reasoning placement and
  liveness. **Latest at bottom** follows the current turn's `live_status` in a
  stable `reasoning-live:{turn_id}` row; that non-final indicator disappears
  when another process row follows or the turn ends. **In timeline** retains
  historical reasoning. These preferences are local to each client.
- Nothing opens itself: default expansion decides the initial state of every
  reasoning row, settled or live. Explicit expansion/collapse wins and
  survives streamed updates, including a temporarily absent bottom indicator
  within the same turn. **Window** mode is a shape, not an invitation: once a
  row is open it clips a body that is still arriving to its newest rows, while
  a settled thought opens in full. Every row of that window is the thought's
  own text: the live header and the rail already say it is unfinished, so no
  row is spent marking where the older rows went. Changing display mode or
  default expansion clears per-row reasoning overrides; changing expansion
  mode preserves them. Mobile likewise supports full reasoning and explicit
  collapse during streaming. TUI keeps its incremental Markdown renderer,
  rail, and scroll controls.
- Reasoning content keeps its natural height, bottom-aligned within the
  six-line cap. Answers and commentary use body foreground; reasoning stays
  muted. A turn without process rows has no empty process header.
- TUI draws work one grey under the prose it produced: an activity row's action
  and target, and the summary standing for a run of them, wear
  `TuiTheme::quiet()` (`ThemeRole::gray_quiet`, derived a quarter of the way
  from the theme's muted grey to its background). The step is a role rather
  than a fade so the indexed colour tiers keep the same hierarchy; failures
  stay on `danger` and a running row's mark carries its liveness, so its words
  are quiet too. Answer, commentary, and reasoning inks are unchanged.
- A settled, answered turn folds its process section. The fold is only offered
  when the transcript actually draws work for the turn and the turn produced a
  non-empty answer (`complete || superseded` plus a `conclusion_row` body):
  a running turn keeps its work visible because the arriving rows are the
  point, and a settled turn with no answer keeps them because the work is the
  only record. A turn with no drawn process row has nothing to fold and gets no
  divider — a bare question and answer must not pay a rule for work the reader
  never saw. This matches the desktop's `timeline_turn_process_expanded`
  (`explicit.unwrap_or(conclusion.is_none() && (!complete || superseded))`),
  with the added guard that the drawable process run is non-empty.
- The TUI fold control is the fold's own row, drawn inside the blocks the turn
  already has: `Worked for <duration>` alone as the first row of the conclusion
  while the section is shut, and open, the same labelled title as the first row
  of the turn's first process block, and the rule below the work on the
  conclusion, which repeats the duration between its rules. The two ends are one
  gesture: the title's `▾` and the rule's `▴` both point into the work they put
  away, both toggle the turn, and one blank row parts either end from the text
  beside it. The title leads with
  the Agent's brand mark in both states, in the same two-cell slot the session
  list reserves and matched by the same identity string
  (`vibex_brands::agent_mark`); a terminal that cannot hold a picture, or an
  Agent with no mark, keeps the words alone rather than indenting for nothing.
  The duration comes from the turn's own `started_at_ms`/`ended_at_ms` (a
  running turn measures to now), so it survives a reload and agrees with the
  desktop and mobile labels. The rows ride on the turn's own blocks rather than
  on a synthetic block, so they add nothing to search or copy.
- TUI turn folds keep explicit choices by turn id in the transcript's own map,
  exactly as reasoning expansion does: the projection supplies the default on
  every rebuild and the map overrides it. It is applied to every block of the
  turn in one pass, which is what makes the fold a property of the turn rather
  than a coincidence of neighbouring blocks agreeing. Entries leave with the
  turn that owns them, so the map cannot grow with the session. `e` on the
  control and a click on the divider row both toggle it; `F2` expands folds as
  well as bodies; a search hit inside a shut fold opens it before revealing
  the row.

TUI settings use `InterfacePreferences` in `tui-interface.json`; all three fields
are optional, and absent or unknown choices resolve to the shipped default:

| JSON field | Values | TUI default |
| --- | --- | --- |
| `reasoningDisplayMode` | `latest_at_bottom`, `timeline` | `latest_at_bottom` |
| `reasoningExpandedByDefault` | boolean | `true` |
| `reasoningExpansionMode` | `window`, `full` | `window` |

## 4. Validation & Error Matrix

| Input / state | Required result |
| --- | --- |
| Captured output replaces an attachment summary | Display captured text with its line breaks |
| Empty captured result and no arguments | No stale output; tool identity remains inspectable |
| Unknown JSON or partial input | Preserve content in details and original input copy |
| Two edits with equal basenames in different folders | Two changed files |
| Same sequence and equal-length correction | Invalidate the affected content cache |
| Running group manually collapsed | Remain collapsed after progress/history refresh |
| Group summary opens | Show child headers with each detail still independently controlled |
| Group summary closes with open child details | Close all details and retain the summary control |
| Search matches a hidden tool detail | Reveal and select that child, without opening sibling details |
| A tool follows streaming reasoning | Stop the old window; remove the non-final bottom indicator |
| Live reasoning is explicitly collapsed | Keep it closed across deltas and the next bottom indicator in that turn |
| Turn settles with a non-empty answer and drawn process | Fold the process behind the `Worked for` divider; keep the answer and the control visible |
| Turn running, or settled with no answer, or no drawn process | Keep every row visible and draw no divider |
| Reader opens/closes a turn fold | The explicit choice survives rebuilds and streaming deltas for that turn |
| Search matches a row inside a shut fold | Open the fold, then reveal the matched row |
| Older preference file omits reasoning settings | Latest at bottom, default expansion on, Window |
| Disclosure is reversed or Motion is disabled | Continue from displayed progress or settle immediately |
| Failed or pending operation | Keep its failure/approval affordance visible |

## 5. Good / Base / Bad Cases

- Base: a completed file read shows `Read` and a basename, and opens the full
  path and captured result on demand.
- Good: a mixed completed run reports read, changed-file, and search counts;
  the next running command stays separately visible. The reader opens the
  run, opens one tool, and can still use the summary to close everything.
- Bad: displaying `Run execute` when the input contains the command, replacing
  empty output with a stale summary, or reopening a group after its reader
  collapsed it.

## 6. Tests Required

- Shared detail tests cover empty snapshots, generic argument targets,
  lossless input copy, unknown envelopes, and failure exit codes.
- `apps/mobile/src/timeline_tests.rs` drives production turn rendering at
  phone widths and different rem/scale settings. Assert header bounds, touch
  and keyboard disclosure, copy independence, capped output height, reasoning
  tail alignment, and explicit choices across updates.
- `crates/vibex-tui/tests/timeline.rs` drives real projection and rendering:
  mixed counts, visible running/failing rows, cache corrections, search/copy,
  three-level expansion, Unicode/ASCII connector columns, selection after
  batch collapse, both reasoning display/expansion modes, and explicit choices
  across streamed updates. Assert partial disclosure heights, reversal
  continuity, immediate motion disable, and no idle animation. Preference
  roundtrip/default tests live in `interface_prefs.rs` and `app.rs`; real mouse
  and keyboard disclosure tests live in `run.rs`.
- Run affected Rust tests and Clippy. TUI changes also run
  `cargo test -p vibex-tui --features pty-harness --test pty --locked`.
  Shared presentation changes additionally run desktop timeline regressions,
  `cargo check -p vibex-ui --no-default-features --locked`, and the native
  mobile contract checker. Mobile GPUI tests that initialize global event
  subscribers require process isolation; serial test threads alone do not
  isolate those subscribers.

## 7. Wrong vs Correct

Wrong: choose detail text from `output_summary` whenever decoded captured
output is empty, or key Markdown caches only by sequence and byte length.

Correct: captured output wins by presence; compare the bounded source content.
Preserve the reader's expansion state separately from content cache validity.

Wrong: replace an expanded group summary with its members, or set every tool
detail to expanded when opening that summary.

Correct: retain a separate summary and independent child disclosures; closing
the summary clears every child detail so reopening starts with headers.

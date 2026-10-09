# Shared Timeline Presentation

## 1. Scope / Trigger

Read this when changing timeline activity identity, details, grouping, or
reasoning windows in `apps/desktop`, `apps/mobile`, or `crates/vibex-tui`.
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
```

`Activity` exposes action, target, full path, icon kind, and failure through
getters. `Detail` exposes its label, bounded source, readable text, copy text,
metadata classification, and file-content classification. Neither type owns UI
state or platform handles. `REASONING_WINDOW_LINES` is six.

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
  details stay collapsed until requested.
- TUI groups three or more settled typed activities by turn and runtime,
  summarizes all action categories, and deduplicates changed files by full
  path. Running, failing, or approval-pending activities stay visible.
  Opening a grouped head opens all members and survives streamed updates.
- Only the current live thought automatically opens a reasoning window.
  Settled/superseded turns and a thought followed by another process row do not
  animate. Mobile supports full reasoning and an explicit collapse while
  streaming. TUI keeps its tail window, fold marker, rail, and scroll controls.
- Reasoning content keeps its natural height, bottom-aligned within the
  six-line cap. Answers and commentary use body foreground; reasoning stays
  muted. A turn without process rows has no empty process header.

## 4. Validation & Error Matrix

| Input / state | Required result |
| --- | --- |
| Captured output replaces an attachment summary | Display captured text with its line breaks |
| Empty captured result and no arguments | No stale output; tool identity remains inspectable |
| Unknown JSON or partial input | Preserve content in details and original input copy |
| Two edits with equal basenames in different folders | Two changed files |
| Same sequence and equal-length correction | Invalidate the affected content cache |
| Running group manually collapsed | Remain collapsed after progress/history refresh |
| A tool follows streaming reasoning | Stop the old automatic reasoning window |
| Failed or pending operation | Keep its failure/approval affordance visible |

## 5. Good / Base / Bad Cases

- Base: a completed file read shows `Read` and a basename, and opens the full
  path and captured result on demand.
- Good: a mixed completed run reports read, changed-file, and search counts;
  the next running command stays separately visible. The reader can expand the
  run and copy each complete input.
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
  grouped expansion, and the newest reasoning lines across terminal widths.
- Run affected Rust tests and Clippy, desktop timeline regressions,
  `cargo test -p vibex-tui --features pty-harness --test pty --locked`,
  `cargo check -p vibex-ui --no-default-features --locked`, and the native
  mobile contract checker. Mobile GPUI tests that initialize global event
  subscribers require process isolation; serial test threads alone do not
  isolate those subscribers.

## 7. Wrong vs Correct

Wrong: choose detail text from `output_summary` whenever decoded captured
output is empty, or key Markdown caches only by sequence and byte length.

Correct: captured output wins by presence; compare the bounded source content.
Preserve the reader's expansion state separately from content cache validity.

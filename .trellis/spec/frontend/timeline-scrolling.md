# Session Timeline Scrolling

## 1. Scope / Trigger

Read this when changing the desktop transcript's scroll container, follow
behavior, scrollbar, bottom control, or when evaluating
`gpui-component`'s `MessageScroller` for a session message list.

## 2. Current Ownership

The desktop main transcript (`SessionView::timeline_scroll`) is a
`VirtualListScrollHandle` driving `v_virtual_list` with the per-turn
`timeline_row_sizes` table. That table is not merely an optimization: the
transcript needs pixel-level scroll control that the table makes possible.

| Behavior | Why the current list owns it |
| --- | --- |
| Session switch / authoritative replacement | `capture_timeline_scroll_anchor` records `(turn index, offset inside turn)`; `apply_timeline_scroll_anchor` restores that exact `ListOffset`. |
| Search and permission reveal | `reveal_session_search_row` moves the viewport by the delta between a painted row's bounds and the viewport, so a match or card inside a turn taller than the viewport is reachable. |
| Follow at the live edge | `scroll_timeline_to_latest` sets the offset to the content extent before the first layout, and the wheel-idle state machine distinguishes a gesture from streaming growth. |
| First frame | Per-turn estimated heights give the scrollbar and the jump-target math a full extent before off-screen turns are measured. |
| Split panes | Each `SessionView` owns its handle, so one pane cannot move another pane's transcript. |

The child Agent rail is different: it is read-only, has no per-turn anchor
table, no search reveal, and no writer that needs a pixel offset. It uses
`MessageScroller` with one `MessageScrollerState` per child session
(`VibexWorkbench::child_agent_timeline_scrollers`). The component owns its
virtualization, scrollbar, jump-to-latest control, and row measurement; the
rail opens each tab at the top and remembers each tab's place.

## 3. Disclosures on the Bottom Edge

A virtual row's box is the height an earlier frame measured, so the content of a
travelling disclosure is always ahead of the row table: one frame in flow, up to
three through a windowed group and process run, each of which reserves a height
of its own. Anywhere the top of the screen is held still, that difference falls
on the row's bottom edge, usually out of view. On the bottom edge the scroll
offset is resolved from the table itself, so the same difference becomes
visible: content under the row is thrown down by a frame of the spring and comes
back, and a closing row flashes a blank band.

| Rule | Why |
| --- | --- |
| A disclosure click keeps the turn's measured extent (`prepare_timeline_disclosure`, `remeasure_timeline_turn`). A detail read landing does too. | An estimate in the row table for one frame moves the whole timeline on the bottom edge. Elsewhere the same estimate only moves rows below the screen, so the bug looks position-dependent. |
| Opening a row of a finished turn leaves the follow and keeps the screen (`hold_timeline_for_disclosure`). | The row opens below the pointer exactly as it does further up the timeline. Growing upward lifts the row out from under the click that opened it. |
| A running turn stays on the live edge, and so does a row closing there. | The reader is watching the turn arrive, and a closing row has to bring the viewport down either way. |
| Rows hang from the bottom of their boxes only while a disclosure moves on the bottom edge and for `AGENT_TIMELINE_DISCLOSURE_SETTLE_FRAMES` after it stops (`timeline_rows_hang_from_bottom`), and never for a row opened with the screen held. | It keeps the visible edge exact and moves the difference to the top of the turn. A stream must not do this: a shorter Markdown reparse is held in place by the same slack, and hanging it bounces the whole turn. |
| The windowed run and group body place their rows through `timeline_window_origins`, measuring every row before placing any. | They reserve a height of their own, so hanging the virtual row alone still moves what is under a disclosure. A row with no measured neighbour starts from the table, which keeps a pinned reveal on its own origin. |
| The child Agent rail takes no part in any of this. | It scrolls by itself, and its rows name turns of another timeline, so they neither release the parent's follow nor open its settle window. |
| A disclosure landing on the bottom edge resumes the follow (`resume_timeline_follow_after_disclosure`). | The reader never scrolled, so the wheel-idle state machine has nothing to resume from. The distance comes from the row table, because the handle's extent is still the last prepaint's. |

## 4. Why the Main Transcript Does Not Use MessageScroller

`gpui-component 0.8.x`'s `MessageScrollerState` exposes `logical_scroll_top`,
`scroll_to_item(index)`, `scroll_to_end`, and row-count mutations. It does not
expose:

- `scroll_to(ListOffset)` / `scroll_by`, so an offset inside a turn cannot be
  restored;
- `viewport_bounds` / current offset / maximum offset from the state entity;
- per-row size hints, so an unmeasured turn contributes only the state's
  uniform hint instead of the transcript's own estimate;
- a row renderer that receives the owning entity (`v_virtual_list` passes
  `&mut V`, `MessageScroller` passes only the index).

A literal replacement would therefore collapse anchors to turn boundaries,
lose reveal inside turns taller than the viewport, and change the first-frame
extent. Forking or vendoring the crate is prohibited by
[`rust-dependency-sources.md`](../backend/rust-dependency-sources.md).

## 5. Required Upstream API Before Migrating

The main transcript can move to `MessageScroller` once the published state
exposes:

```rust
MessageScrollerState::scroll_to(ListOffset);
MessageScrollerState::scroll_by(Pixels);
MessageScrollerState::viewport_bounds() -> Bounds<Pixels>;
MessageScrollerState::offset() -> Point<Pixels>;
MessageScrollerState::max_offset() -> Point<Pixels>;
MessageScrollerState::set_row_height_hints(impl IntoIterator<Item = Pixels>);
MessageScroller::new_with_renderer(entity, id, state, |view: &mut V, index, window, cx| ...);
```

Until then, keep the per-turn virtual list and adopt only compatible
`MessageScroller` affordances (for example, the log role or the jump-button
visual language) without removing the pixel-scroll state.

## 6. Tests Required

- `cargo test -p vibex-desktop --locked timeline` covers anchor restore,
  wheel-idle follow, reveal inside a tall turn, and split-pane isolation when
  the transcript scroll path changes.
- `cargo test -p vibex-desktop --locked timeline_tests` covers disclosures on the
  bottom edge. Step the clock with `advance_clock` and assert every frame's
  bounds: a bounce lasts one frame of the spring, so a settled frame cannot see
  it.
- `cargo test -p vibex-desktop --locked child_agent_rail` covers the child
  Agent rail's virtualization and its separation from the parent's Markdown
  surfaces.

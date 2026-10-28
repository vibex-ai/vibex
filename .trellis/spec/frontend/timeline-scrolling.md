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

## 3. Why the Main Transcript Does Not Use MessageScroller

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

## 4. Required Upstream API Before Migrating

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

## 5. Tests Required

- `cargo test -p vibex-desktop --locked timeline` covers anchor restore,
  wheel-idle follow, reveal inside a tall turn, and split-pane isolation when
  the transcript scroll path changes.
- `cargo test -p vibex-desktop --locked child_agent_rail` covers the child
  Agent rail's virtualization and its separation from the parent's Markdown
  surfaces.

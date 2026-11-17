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

## 3a. Where a Fixed-Size Virtual List Is Wrong

`v_virtual_list` places its rows from the sizes it is handed; it never measures
them. The timeline can do that because `timeline_row_sizes` is the authority for
every row's height, including the estimates it starts from.

That is not true of a row whose own content decides its height — an inline
editor, a settings panel, an expanded trace. The panel's height depends on the
pane width, the locale, and how its chips and labels wrap, so any constant
budget is right only at the size it was measured at; where it is wrong, the rows
after the panel are placed inside it and paint over its fields. Those surfaces
use GPUI's measured `list` with a `ListState` the view keeps, re-measured when
the panel opens or closes. The Providers → Models tab is the worked example:
its list lives on the entity, the tab is a fixed detail page so the list has a
definite height, and the card's row set is reconciled before every draw.

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

All of these now exist on the kit fork's `feat/message-scroller-api` branch, but
**not in the published `gpui-component 0.10.0` that this workspace resolves**, and
the dependency-source policy (`.trellis/spec/backend/rust-dependency-sources.md`)
forbids reaching a fork through `[patch.crates-io]` or a vendored copy without
explicit approval. The published crate still gives the row renderer only an
index, so a caller cannot build a row from `&mut self` without routing through a
weak entity and a snapshot.

Two things follow. The API gap is no longer the blocker — the *payoff* is
(see §7.1: a layer only pays when its content is stable across scrolled frames).
And until a release carries the new API, any migration would have to work
through the published surface, which is a larger rewrite of the row builders
than the geometry changes alone suggest.

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

## 7. Sidebar list migration

The sidebar's session lists are hand-rolled `SidebarSessionRun` virtual lists
inside a single outer `sidebar_scroll`: the run reserves its own `flow_height()`
and builds only the rows the outer viewport shows, placing each at a fixed
stride. `MessageScroller` is itself a scroll container, so it cannot be dropped
in as a sub-list — it would either build every row (full-height content) or
nest a second scroll over the outer one.

### 7.1 Why the migration does not pay off on its own

Measured in a `VisualTestContext` workbench with 200 sessions, drawing a wheel
scroll over the sidebar column:

| Probe | Result |
| --- | --- |
| Sidebar's outer scroll container | `layer_frames_composited` stays 0 across every frame |
| A bare scrolling `div` in the same harness | composites from its second scrolled frame |
| Per frame while scrolling | prepaint 7–11 ms, build ~1.7 ms, paint ~0.7 ms |
| Views built per frame | 1 (`views_built` 1, `views_reused` 1) |

Two conclusions follow, and they are the reason this section is a record of a
rejected plan rather than a to-do:

- **`SidebarSessionRun` already virtualizes.** One view is built per scrolled
  frame, not one per row. Swapping the element for `MessageScroller` cannot
  improve on that; the remaining cost is the prepaint walk over the rows that
  are on screen, which any list pays.
- **The sidebar's container never composites a layer, so a list layer would not
  engage either.** Instrumenting `fast::layers::policy::decide` shows the
  container reaching `streak >= 2` and then losing its record to
  `owner_animating`, which `eligible()` and the `Bypass` arm both reject. The
  animation frame comes from `gpui_base::Scrollbar`'s fade: it calls
  `Window::request_animation_frame` while its opacity animates, and
  `request_animation_frame` attributes the request to `window.current_view()`.

### 7.2 The scrollbar-overlay experiment

`pinned_scrollbar` (see `apps/desktop/src/gpui_ext/mod.rs`) draws the bar as a
child element of the view that owns the scroller, so the fade's animation frame
is attributed to that view. Giving the bar a view of its own — an overlay held
by `Window::use_keyed_state`, rebuilding the bar each frame from the handle —
moves the attribution to the overlay. Measured A/B, twenty scrolled frames each,
wall-clock delayed so the fade can settle:

| Subject | Baseline | With the overlay |
| --- | --- | --- |
| Bare `div` with stable rows | `Composite` after promotion, paint 176 µs → 21 µs | same |
| Sidebar, 200 sessions | 310.8 ms prepaint | 303.7 ms prepaint |
| `im_channels` lane | 3.30 ms prepaint, 0 composited | 4.23 ms prepaint, 0 composited |

The overlay does move the animation frame off the owner, and a container whose
content is stable then reaches `Composite` and keeps it. It does **not** help
either real surface: the sidebar's content is `changed` on every frame
(`deps_unchanged=false` in `scroll_only`, i.e. its recorded dependencies move),
so its layer is `Repaint`ed and then demoted after
`DEMOTE_AFTER_CHANGED_FRAMES`, and the `im_channels` lane never composites for
reasons of its own. Net cost was equal or slightly worse, so the experiment was
reverted rather than merged.

### 7.3 What has to be true before either migration is worth doing

A scroll layer only pays for itself when its content is stable across scrolled
frames. For the sidebar that means the recorded dependencies of the column must
stop moving every frame — the P0b work of narrowing notifications, so that a
scroll frame notifies the scroll container and not the whole workbench. Until
that holds, neither `MessageScroller` nor an overlay scrollbar changes what the
reader pays, and the existing `SidebarSessionRun` is the right shape.

If the migration is revisited, the mechanics are unchanged: flatten every
section (project, workspace, folder, group, session run) into one row list,
render it with `MessageScroller::new` (its renderer signature is already
`Fn(usize, &mut Window, &mut App) -> AnyElement`, matching the current
`SidebarSessionRowBuilder`), give fixed-height session rows
`set_row_height_hints`, and move `sidebar_scroll`'s semantics onto
`MessageScrollerState`: the selected-session anchor, `locate current session`
reveal, the rename row (measured, not hinted), drag/drop targets, and the
scroll position memory. Steps must keep the existing sidebar tests green:
`cargo test -p vibex-desktop --lib --locked sidebar` covers 94 cases.

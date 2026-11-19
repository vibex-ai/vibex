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
| The streaming preserve policy holds a turn's extent only while the reader is scrolled up (`record_timeline_turn_height`). | A held extent is a blank band under the last row and the viewport is anchored to it, so releasing the band slides the last row down by its height. At the live edge the band is the last thing on screen: the hold buys nothing there, and a disclosure frame — which releases it by dropping `timeline_disclosure_animating` — turns the release into a visible jump of everything below. Scrolled up, the band sits below the fold, where it is exactly what keeps a transient reparse from bouncing. |
| The settle window reads the clock that drives the view's frames (`cx.background_executor().now()`), not `Instant::now()`. | A test advances the frame clock to draw, so a process-clock reading makes the window depend on how long the fixture took: the same probe failed alone and passed in the suite because wall time had settled the hold. |
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

`MessageScrollerState` in the resolved `gpui-component 0.10.0` exposes
`logical_scroll_top`, `scroll_to_item`, `scroll_to_offset`, `scroll_to_end`, and
row-count mutations. It does not expose:

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

That is an argument against the *wrapper*, not against the element. Every one of
those capabilities exists on `ListState`, which `MessageScrollerState` wraps and
does not re-export, and `gpui::list(state, renderer)` is public gpui — the same
shape §3a already uses for the Providers → Models tab. `v_virtual_list` is
`gpui_base::VirtualList`, which contains no `fast::layers` code at all, so the
transcript gets no list layer today; `gpui::list` carries the full one. §5
records the measurement and the shape of the migration.

## 5. What the Wrapper Would Need, and What Is Already Available

`MessageScroller` — the wrapper — would need its published state to expose:

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

Verified against the resolved `gpui-component 0.10.0` source
(`src/message_scroller.rs`): `scroll_to_offset(ListOffset)` is published; the
rest of the list above is not, and `MessageScroller::new` still takes
`impl FnMut(usize, &mut Window, &mut App) -> E`. So four of the five state
accessors, the row-height hints, and the owning-entity renderer are all still
absent from what this workspace can build against.

Two things follow. The payoff is real and the element is the reason (see the
measurement below): the transcript's element has no list layer, while
`gpui::list` does. And the `MessageScrollerState` surface above is a thin
re-export, not the whole capability — most of what the requirement list asks
for is already reachable one level down.

**Status: the transcript can get a list layer, without changing or forking
`gpui-component`.** An earlier revision of this section closed the question on
the claim that a list layer would not engage on the transcript. That claim was
wrong, and the reason is worth recording because it is the whole argument for
the migration.

The two elements are not the same thing:

| Element | `fast::layers` references | List layer |
| --- | --- | --- |
| `gpui::list` — what `MessageScroller` renders internally | 17 | supported (`begin_list` / `paint_row` / `end_list`) |
| `gpui_base::VirtualList` — what `v_virtual_list` is | **0** | none |

So the transcript does not merely fail to composite today: the element it uses
has no list-layer code path at all, and measured on the real transcript the
container composites zero frames across twelve scrolled frames. `MessageScroller`
wraps `gpui::list`, which *does* carry the full list layer — rows replay instead
of being rebuilt, and a list layer is exempt from the tile budget. On the
transcript that is the difference between rebuilding the visible turns every
scrolled frame and replaying them.

**The API gap is not a wall: use `gpui::list` directly.** Everything the
requirement list asks for already exists one level down, on `ListState` — the
type `MessageScrollerState` wraps and never re-exports:

| Wanted | Where it lives in `gpui-fast 0.1.16` |
| --- | --- |
| `scroll_to(ListOffset)` | `ListState::scroll_to` |
| `scroll_by` | `ListState::scroll_by` |
| `viewport_bounds` | `ListState::viewport_bounds` |
| offset / max offset | `ListState::scroll_px_offset_for_scrollbar`, `max_offset_for_scrollbar` |
| row height hints | `ListState::with_uniform_item_height`, per-item `size_hint` |
| renderer owning entity | not on `list()`, but a captured `WeakEntity` is the standard pattern here already |

`gpui::list(state, renderer)` is a public gpui constructor, and `ListState` is
`Clone`, so a view can own one and still pass it to the element. Nothing in
`gpui-component` has to change, and nothing has to be forked — this is the
dependency-source policy's preferred outcome, not an exception to it.

Measured: a plain `gpui::list` over 200 stable rows composites **eleven frames of
twelve** scrolled frames, against **zero** for the same content through
`v_virtual_list`. So "can the transcript get a list layer" is answered yes, and
the remaining work is a row-builder rewrite onto `ListState`, not a wait on an
upstream release.

**The migration is done.** The transcript renders through `gpui::list` with a
`ListState` the `SessionView` owns, and every desktop test passes (2016). What
the rewrite had to solve is recorded here because none of it is obvious from the
API:

| Problem | What the rewrite does |
| --- | --- |
| `list()`'s renderer receives only an index | the row builder reaches the view through a `WeakEntity` and `Entity::update`, the pattern every other row callback here already used |
| The renderer runs inside the list's own layout borrow | nothing in a row may read the list's state. The one scroll-derived decision a row makes — whether the turn hangs from the bottom edge — is resolved once before the element is built and cached on the view |
| Unmeasured rows contribute no extent | `reset_with_uniform_height` seeds them from the table's median row height. Without it the scrollbar reads a zero-length document and the transcript cannot scroll at all until every row has been on screen |
| `scroll_px_offset_for_scrollbar` counts the list's padding, `max_offset_for_scrollbar` does not | both are read through view-level accessors that put them in one coordinate system; comparing the raw pair leaves a padding-sized band that makes the transcript read as never reaching its bottom edge |
| `scroll_to_end` parks the viewport past the last row | `scroll_timeline_to_latest` anchors at the end only when the content overflows; a transcript that fits drops the anchor instead |

**The layer still does not engage on the transcript.** Measured after the
migration: the same `ListState`, drawn through a bare `gpui::list` in another
window, composites eleven of twelve scrolled frames — while the workbench's own
path composites zero. The element is no longer the blocker; the owner is. A
wheel over the transcript notifies the workbench, and `owner_scrolled_only`
requires that nothing but the scroll notified it. That is §7.3's second blocker,
the P0b work of narrowing notifications, and it now applies to the transcript
exactly as it does to the sidebar.

**Solved: the scrollbar's fade was the blocker.** Bisecting the surface found
it. `pinned_scrollbar` draws `Scrollbar` as a child of the view holding the
scroller, and the bar asks for an animation frame while its fade runs;
`Window::request_animation_frame` attributes the request to
`window.current_view()`, so it landed inside the list's own layer path and
`eligible` refused the layer for as long as the fade ran — every scrolled frame.
`ScrollbarOverlay` (in `apps/desktop/src/gpui_ext/mod.rs`) paints the bar in a
view of its own, which moves the attribution off the surface. Measured on the
transcript's surface with the same rows and state: **0 of 12 composited frames
with the bar inline, 11 of 12 with it in its own view.** This is the same
blocker §7.2 recorded for the sidebar; the sidebar also had a second one behind
it, which is why the overlay did not help there, while the transcript had only
this.

How to bisect this class of problem, since reading the policy does not tell you
which rule fired: build a minimal surface that returns the list alone and
confirm it composites, then add the real surface's pieces back one at a time.
The minimal list composited 11 of 12 immediately, which ruled out the element,
the rows, the view boundary and the wrapper in one step and left the bar.

Two more things were measured while narrowing it, and both matter to whoever
picks this up:

- **The rows are the cost, not the shell.** Stubbing the row builder's content
  takes a scrolled frame from thirteen rebuilt views to one. So the work below
  is worth doing on its own account even before the layer engages: a scrolled
  frame stops rebuilding every visible turn.
- **`gpui::list` notifies the view that painted it, and no subscription can
  refuse that.** The notify is delivered by `App::notify` straight to the
  view's invalidator, not through the observer list, so a view that records the
  scroll count and skips the notify in its `observe` callback is still rebuilt.
  The count (`TranscriptScrollNotifies`, taken in the list's own scroll handler,
  which runs just before the notify) therefore buys nothing until the list is
  painted by a view that holds nothing else. That is the shape §7.3 already
  describes for the sidebar and the shape `ChildAgentRailView` uses: the
  element and its scroll state move into a view of their own, and a scroll then
  rebuilds that view alone.

## 5b. A row-count change must not move the reader

`gpui::list` keeps the reader's place in its scroll anchor, and the only way to
tell it a row was added or removed is `reset` — which drops the anchor. A list
without one starts at `ListOffset::default()`, i.e. row zero, so the next frame
paints the top of the conversation. Both ways the row count moves are therefore
still broken if the reset is taken as the whole job:

| Change | What has to happen |
| --- | --- |
| A turn is appended (live event, a sent message) | the reader's offset is already the place they keep: nothing above the viewport moved. Read the offset before the reset and set it back afterwards. |
| An older page is prepended (a history read) | the content above the viewport grew, so the offset has to follow the row the reader was in: `capture_timeline_scroll_anchor` before the merge, and the pending anchor after the reset. |

Order is part of the rule, and it splits the two pending scrolls apart:

- `apply_pending_timeline_scroll` (the bottom follow) runs **before**
  `sync_timeline_list_rows`: `scroll_to_end` on a list that has not been told its
  count yet is a no-op, so the count has to exist first — that is the order the
  transcript already had.
- `apply_pending_timeline_anchor` runs **after** it. Applying an anchor first and
  resetting the count afterwards wiped the anchor it had just restored — the
  reader landed at row zero with no pending work left to correct it — and the
  restore was clamped against the previous table's extent.

The two cannot share one call: a bottom follow must win over a stale anchor, and
an anchor must land on the table the reset installed.

## 5c. One transcript surface per session

`TimelineSurfaceView` mounts the list, so it is the view that paints a pane's
transcript. It renders during the paint pass, after the pane loop has handed its
borrowed views back, so a surface with no session of its own paints whichever
view happens to be borrowed — the focused one. Shared across panes, that showed
every pane the focused conversation over that pane's `ListState`: identical rows,
identical offset, and a scroll in one moved them all. A pane looked right only
once it was clicked, because clicking made its session the focused one.

Rules that follow:

- A surface is created per session (`timeline_surface_views`, keyed by session
  id) and borrows that session for its render
  (`with_session_view_for_render`), exactly as `ComposerView` does.
- It is dropped with the session's view and cleared with the panes it is mounted
  beside.
- `notify_timeline_surfaces` has to notify the surfaces by name: a pane view
  only mounts its surface, it does not notify it.

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
- `a_turn_arriving_while_scrolled_up_keeps_the_readers_place` and
  `an_older_page_above_the_reader_keeps_their_place` pin §5b: both fail with the
  reader at offset zero (an append) or parked on the wrong rows (a prepend) if
  the reset is left to itself or the order is reversed.
- `a_pane_gets_its_own_transcript_surface` pins §5c: two sessions must not share
  one surface.

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
rejected plan rather than a to-do — with a third recorded below the next
paragraph, once the container's background was measured separately:

- **`SidebarSessionRun` already virtualizes.** One view is built per scrolled
  frame, not one per row. Swapping the element for `MessageScroller` cannot
  improve on that; the remaining cost is the prepaint walk over the rows that
  are on screen, which any list pays.
- **The sidebar's container never composites a layer, so a list layer would not
  engage either.** Instrumenting `fast::layers::policy::decide` shows the
  container reaching `streak >= 2` and then losing its record to
  `owner_animating` (`ineligible=false owner_animating=true` on every scrolled
  frame), which the `Bypass` arm rejects. The animation frame comes from
  `gpui_base::Scrollbar`'s fade: it calls `Window::request_animation_frame`
  while its opacity animates, and `request_animation_frame` attributes the
  request to `window.current_view()` — the workbench, which also owns the
  scroller.

The fade samples `Instant::now`, so a test that draws its frames in a few
milliseconds keeps the animation running for every one of them; that is why the
blocker shows up in tests even when a reader's own machine would let the fade
settle between scrolls. Sleeping past the fade before each frame does not make
the sidebar composite either, so the finding is not an artifact of the harness.

**The scroll container's background is not a blocker.** It is painted
`cx.theme().transparent` by default, and `fast::layers::background::bake` only
clears a layer's tiles with a solid, opaque, borderless, unrounded quad that
covers the whole viewport — so it looked like a second, independent cause.
Measured on the real `#sidebar-session-list` in a 200-session workbench, drawing
sixteen scrolled frames, `window.layout_stats().layer_frames_composited`:

| Background | Scrollbar | Composited frames |
| --- | --- | --- |
| transparent | present | 0 |
| transparent | removed | 8, then 0 |
| opaque | present | 0 |
| opaque | removed | 8, then 0 |

The background makes no difference at all: the sidebar's panel (`#agent-sidebar`,
`.bg(cx.theme().sidebar)`) is already painted under the container, and the bake
rule finds that quad either way. A minimal isolated `div` probe that scrolls
fifteen stable rows *does* show the difference (0 composited transparent, 9
opaque), which is what made the wrong reading available — a bare container has
no opaque ancestor to bake, while the sidebar does. Trust the sidebar harness,
not the synthetic one, for this question.

The scrollbar is the first blocker and the dependency record is the second: with
the bar gone the container composites eight frames and is then demoted, which is
the `deps_unchanged=false` finding of §7.2 arriving on schedule
(`REPROMOTE_AFTER_STABLE_FRAMES`). Neither is removed by painting the container.

### 7.2 The scrollbar-overlay experiment

`pinned_scrollbar` (see `apps/desktop/src/gpui_ext/mod.rs`) draws the bar as a
child element of the view that owns the scroller, so the fade's animation frame
is attributed to that view. Giving the bar a view of its own — an overlay held
by `Window::use_keyed_state`, rebuilding the bar each frame from the handle —
moves the attribution to the overlay. Measured A/B, twenty scrolled frames each,
wall-clock delayed so the fade can settle:

| Subject | Baseline | With the overlay |
| --- | --- | --- |
| Bare `div` with stable rows | composites, paint ~176 µs | `Composite` held, paint ~21 µs |
| Sidebar, 200 sessions | 310.8 ms prepaint | 303.7 ms prepaint |
| `im_channels` lane | 3.30 ms prepaint, 0 composited | 4.23 ms prepaint, 0 composited |

The overlay does move the animation frame off the owner, and a container whose
content is stable then reaches `Composite` and keeps it. It does **not** help
either real surface. In the overlay build the sidebar's layer was promoted and
then demoted again eight frames later, with `scroll_only` reporting
`deps_unchanged=false` — the column's recorded dependencies move on every
scrolled frame, so its content is repainted rather than composited. Without the
overlay the sidebar never reaches `scroll_only` at all, because
`owner_animating` rejects it first; the two blockers stack, and clearing one
only reveals the other. The `im_channels` lane never composites for reasons of
its own. Net cost was equal or slightly worse, so the experiment was reverted
rather than merged.

### 7.3 What has to be true before either migration is worth doing

A scroll layer only pays for itself when its content is stable across scrolled
frames. This subsection is about the **sidebar**; the transcript's situation is
the opposite one and is recorded in §5 — its element has no list layer at all,
so there is nothing to stabilise until it moves to an element that does.

The sidebar has two independent blockers, and both have to go:

1. The scrollbar's fade must not be attributed to the view that owns the
   scroller (the overlay experiment in §7.2).
2. The column's recorded dependencies must stop moving on every scrolled frame.
   That is the P0b work of narrowing notifications: a scroll frame should
   notify the scroll container, not the whole workbench.

The container's background is deliberately not on this list: §7.1 measured it
against the real sidebar and it changes nothing, because the panel behind the
container is already opaque. Painting the container solid would only be needed
if it were ever hosted over a translucent surface.

**The first of those two is now solved**, by the same change that let the
transcript composite: `pinned_scrollbar` paints the bar in a view of its own
(see §5), so the sidebar's column now composites eight of sixteen scrolled
frames instead of none. The second is measured precisely and is the remaining
work:

- With the row builder's content stubbed, the column composites **fifteen of
  sixteen** frames and its prepaint drops from ~6.5 ms to ~47 µs. The rows are
  the whole of what remains.
- The cause is not what a row reads. It is that the row builder must reach the
  workbench through `Entity::update`, and an update is a write: the access log
  bumps its write generation even when the closure does nothing, which
  `owner_change` reports as `Changed` and `scroll_only` refuses. An *empty*
  `update` reproduces the demotion on its own, so no amount of making the row
  read less will fix it.
- `&mut self` is needed because the row resolves `sidebar_selection_index`,
  which `render_agent_sidebar` rebuilds each pass, and because
  `sidebar_workspace_groups` calls `reconcile_legacy_sidebar_folder_worktree_owners`
  and then `queue_ui_state`. Both early-return on unchanged inputs, but they are
  `&mut`, and so the whole chain is.

So the work is: make the sidebar's render chain take `&self`. The two caches it
writes (`sidebar_selection_index`, `sidebar_projection_cache`) can move behind
`RefCell`; the reconcile step cannot, because `queue_ui_state` is a real write
to the UI-state writer. That step belongs before the render, next to the other
per-pass work in `render_agent_sidebar`, which is where the rest of this
section's blockers have been cleared from.

Until both hold, neither `MessageScroller` nor an overlay scrollbar changes what
the reader pays *for the sidebar*, and the existing `SidebarSessionRun` is the
right shape. Both were measured, not inferred: removing the bar alone lifts
`owner_animating` and the container composites for eight frames before the
dependency record demotes it again, which is the same finding the overlay
experiment reached from the other side.

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

## 8. What a sidebar scroll actually costs

A wheel over a `track_scroll` element notifies `window.current_view()`, captured
when the element was painted. The sidebar's container is built inside
`render_agent_sidebar`, which `SidebarView::render` runs through
`workbench.update(cx, |this, cx| this.render_agent_sidebar(cx))`, so that view is
the workbench.

That sounds like the workbench is rebuilt on every wheel step, and measuring it
shows it is not. In a release build with a 200-session column:

| Frame | Views built | Layout calls | Prepaint |
| --- | --- | --- | --- |
| Idle redraw | 1 | 1 | ~21 µs |
| Sidebar scrolled 60 px | 1 | 30–33 | 2.3–3.8 ms |
| Workbench notified | 2 | 1 | ~90 µs |

So the notification reaches the workbench, but only one view is built, and
asking the workbench to render costs 90 µs — not the milliseconds a scroll does.
The cost is in the scroll frame itself: `SidebarSessionRun` clears its rows on
every frame and then, for each visible row, builds the element, calls
`layout_as_root`, and calls `prepaint_at`. `AnyElement` is consumed by drawing,
so the rows cannot be carried across frames; the run re-does that work for every
row in the viewport plus the overscan band.

The scaling confirms it. Layout calls follow the band, not the window:

| `SIDEBAR_SESSION_RUN_OVERSCAN_PX` | Layout calls | Prepaint (200 sessions) |
| --- | --- | --- |
| 480 | 36 | 6.4 ms |
| 240 | 30 | 4.9 ms |
| 160 | — | 4.4 ms |
| 0 | 25 | 2.7 ms |

That is why the constant was trimmed to 240 (§7.2 records the numbers): the band
is the one term in the scroll's cost that a call site controls.

A row's own build is the other term, and it is almost entirely element
construction. Measured in a release build, `render_sidebar_session` takes
~39 µs per row, while every workbench lookup it does first — the row state, the
branch collapse, the move selection, the focus handle — comes to 79 ns together.
So the cost is not the lookups, the maps, or the string keys; it is building the
element tree of a row, which is what any list pays per row it has to rebuild.

Two consequences. Trimming the band is the whole of the cheap win, and it is
done. Making rows survive a scroll needs a cache the run can reuse across
frames, which `AnyElement` cannot be — it is consumed by drawing — so that would
mean a virtual list that owns the rows, i.e. the `MessageScroller` migration in
§7.1, and it would only pay for the rows it does *not* have to rebuild.

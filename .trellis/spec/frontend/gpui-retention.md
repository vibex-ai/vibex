# GPUI Fast Retention

How the desktop behaves under gpui-fast's retained renderer, and what the app
has to do for a frame to pick up a change.

## 1. Scope / Trigger

Read this before changing a desktop repaint or notify path, adding a view
boundary, or judging whether a desktop behavior difference belongs to the app
or to gpui-fast.

## 2. Backend

- `apps/desktop` runs on `gpui-fast` through the `gpui-fast` feature of
  `gpui-component`, `gpui-base`, `gpui-fps` and `gpui-kit-assets`.
- `apps/mobile` still uses `gpui-pre`; this migration is desktop-only.
- `GPUI_VIEW_RETENTION=0` draws every view from scratch at runtime.
  `GPUI_SCROLL_LAYERS=0` turns off scroll layers. Neither disables retained
  Taffy layout nodes or text-measurement carry.

## 3. What the app must do

- A retained frame rebuilds a view only when something it reads changed. State
  written through untracked cells or plain fields keeps showing the last frame
  unless its owner is notified (`cx.notify()`), the same rule cached views
  already impose.
- Keep a notification as narrow as the change. `apply_runtime_ui_signals`
  answers with `RuntimeUiRepaint { dirty, shell, timeline, sidebar }` and
  `apply_runtime_ui_repaint` applies it: a shell answer notifies the workbench,
  otherwise the named boundaries are notified (`notify_timeline_surfaces` for
  the focused `TimelineView` and the `SessionGroupPaneView`s,
  `notify_sidebar_surface` for the `SidebarView`). The agent poll classifies its
  timeline page the same way.
- The shell views (`SidebarView`, `TimelineView`, `SessionGroupPaneView`,
  `ChildAgentRailView`, `SessionTerminalView`) observe the workbench, so a
  workbench notify rebuilds all of them. A hot path that reaches
  `cx.notify()` on the workbench has to be worth rebuilding the shell.
- A list row that can be recycled or shifted carries an `ElementId` built from
  its domain id, not its position, so prepending keeps its layout node and
  element state. The timeline keys a row by session and turn.

## 4. Residual behavior differences

Three probe failures the migration first surfaced were test artifacts that
relied on an unconditional redraw, and are fixed:

- The cached-view hover probe moved the pointer onto a child that already sat
  under it (the child fills the window's top), so no hover changed. It parks
  the pointer below the child first.
- The companion entrance probe drew once after advancing the clock, but the
  lane's ticker is a repeating animation that reduced motion leaves still. It
  asks the harness to notify before the draw.
- The im-channels probe filled a maximized window, so both columns held a few
  pixels more than their content and neither scrolled. It uses a 420px
  definite-height viewport and asserts each column can scroll before the wheel.

The windowed-run disclosure family still fails when run on its own
(`cargo test -p vibex-desktop --lib <name>`):

- `a_row_that_lands_in_one_frame_keeps_the_rows_under_it_still`
- `a_windowed_run_keeps_the_rows_under_a_closing_row_still_on_the_bottom_edge`
- `a_windowed_run_keeps_the_rows_under_a_disclosure_still_on_the_live_edge`

The first frame after a toggle moves the row below with the disclosure. The
probe refreshes every frame, so view retention is not involved; the difference
is in the retained Taffy layout or the timeline's bottom-edge anchoring. The
full library suite can pass it depending on scheduling, so a green suite does
not clear it.

## 5. Timeline list layer

[Session Timeline Scrolling](./timeline-scrolling.md) names the `MessageScroller`
API the main transcript needs before it can move off `v_virtual_list`. As of
gpui-component 0.10.0 the published state still does not expose
`scroll_by`, `viewport_bounds`, `offset`, `max_offset`, per-row size hints, or a
row renderer that receives the owning view, so the transcript stays on its own
virtual list and the child Agent rail stays on `MessageScroller`.

## 6. The composer boundary

Moving the composer out of the pane view into its own retained subtree does not
pay off yet, and the experiment was reverted rather than committed. A probe
that counts renders showed the timeline boundary and the composer rebuilding on
every frame even at rest, while a composer view that draws nothing stays
retained. The workbench's observers do not fire in those frames, so the rebuild
comes from a shared-state write inside the composer's component tree: a global
written while the composer draws invalidates every view that read it, which is
the per-frame registration the architecture notes describe. The boundary is
worth redoing once that write is gone; on its own it reintroduces the per-pane
borrow swap the pane views were written to avoid.

The app pattern itself is sound: on the local gpui-fast fork's
`fix/retention-behaviour` branch, a view that updates the model it observes
while rendering stays retained, and so do two siblings doing it to one model.

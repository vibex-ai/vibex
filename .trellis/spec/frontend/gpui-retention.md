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

The composer is its own retained subtree — `ComposerView`, keyed by session and
mounted by the focused timeline and by each session-group pane. It observes the
workbench and borrows its session's view for the render. It stays retained while
nothing changes, which `a_timeline_repaint_reuses_the_composer` pins. A pane
repaint still builds it once in that frame: the pane writes the workbench as it
renders, and that write is foreign to the composer's record. Relaxing that — a
write made by any subtree in a frame not counting against another subtree built
in the same frame — is the generic fix, and was left undone as too risky for the
gain.

The every-frame rebuild that first hid the boundary was app-side. The composer's
`on_prepaint` hook notified the workbench on every frame because
`adopt_composer_geometry` took the parked measurement out of the map, so the
next frame always compared against a default and reported a change. The hook now
keeps the entry, and the workbench is no longer notified from prepaint.

The app pattern itself is sound: on the local gpui-fast fork's
`fix/retention-behaviour` branch, a view that updates the model it observes
while rendering stays retained, and so do two siblings doing it to one model.

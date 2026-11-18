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

### The shimmer gate was process state, not a retention miss

`activity_shimmer_stops_when_settled_or_motion_is_reduced` passed alone and
failed inside the suite, which read as a retention problem. It was not. The
live-label frame gate kept its state in process-wide statics: one epoch, one
last-frame stamp, one verdict. Three separate ways to go wrong came from that:

- A window that ran late — a background timeline, another pane, a slow headless
  draw — stopped the sweep in every other window.
- A window id is unique only inside one `App`, and every `App` starts its
  slotmap at the same key, so a table keyed by window id let two tests share an
  entry.
- An epoch is only meaningful against the clock it was taken from, and each
  `App` owns its executor clock, so a shared epoch compared two unrelated time
  bases.

The gate is now `LiveLabelFrameGate` on `SessionView`: one workbench, one
clock, one verdict, and no statics. It is sampled once per
`render_agent_workbench_for` with `cx.background_executor().now()` — the
executor clock, not `Instant::now`, so a test's `advance_clock` is the only
thing that moves a frame's gap — and the rest of the frame reads
`self.live_label_frame_gate.animating()`.

The general rule: never key a cadence decision, or anything timed, on process
state. The clock a measurement subtracts from belongs to the same `App` as the
frames it times.

### The windowed-run disclosure family was a layout defect, not retention

These failed when run on their own (`cargo test -p vibex-desktop --lib <name>`)
and also failed with `GPUI_VIEW_RETENTION=0`, so view retention was not the
cause:

- `a_row_that_lands_in_one_frame_keeps_the_rows_under_it_still`
- `a_windowed_run_keeps_the_rows_under_a_closing_row_still_on_the_bottom_edge`
- `a_windowed_run_keeps_the_rows_under_a_disclosure_still_on_the_live_edge`

Measured, on the first frame after a toggle:

| | before click | frame 0 |
| --- | --- | --- |
| row table | 2385 | 2363 |
| `max_offset` | 2017 | 1995 |
| row below the toggle | 263.5 | 285.5 |

The row below moved by exactly the 22px the row table lost, and it moved
because the viewport is bottom-anchored: the offset is `-max_offset`, so a
smaller extent pulls the content down.

The 22px was stale streaming slack, not the disclosure's own delta. While a turn
streams, `record_timeline_turn_height` holds the larger previous extent
(`stable_streaming_timeline_height`), so the table kept 2385 while the laid-out
box was 2363. `preserve_streaming_extent` was gated only on
`!self.timeline_disclosure_animating`, so the click released that hold in the
same frame it opened the row, the table took the true 2363, and the release
landed as a jump before the disclosure's growth arrived.

The fix is to scope the hold to where it earns its keep: it now also requires
`!self.timeline_follow.following_bottom`. At the live edge the held band is the
last thing on screen, so holding it bought nothing and the release was a visible
slide; scrolled up, the band sits below the fold, which is exactly where it
absorbs a transient reparse. All three probes pass, alone and in the suite.

Two earlier candidate fixes were tried and both left the family red, so neither
was committed:

- Keeping the hold while `timeline_disclosure_animating` (or while settle
  frames remain) fixes the first test but leaves the closing-row and live-edge
  probes jumping at frame 19, when `timeline_rows_hang_from_bottom` turns off
  while the table is still behind.
- Clamping the recorded extent to the previous height during the settle window
  changes which frame jumps rather than removing the jump.

The settle window also now reads `cx.background_executor().now()` instead of
`Instant::now()`. That is not cosmetic: a probe advances the frame clock to
draw, so a process-clock reading made the window depend on how long the fixture
took, which is why the same probe failed alone and passed in the suite.

## 5. Timeline list layer

The transcript gets no list layer today, and the reason is the element, not the
app: `v_virtual_list` is `gpui_base::VirtualList`, which contains no
`fast::layers` code at all, while `gpui::list` carries the full one.

The transcript now renders through `gpui::list` with a view-owned `ListState`,
with no crate change and no fork: everything the migration needed was already
on `ListState`, which `MessageScrollerState` wraps and does not re-export. See
[Session Timeline Scrolling](./timeline-scrolling.md) §4–§5 for the capability
table and for the five things the rewrite had to solve (the borrow the row
renderer runs inside, the missing row-height hint, the padding split between
the two offset getters, and the end anchor).

**The layer still does not engage, and the element is no longer why.** Measured
after the migration, the same `ListState` drawn through a bare `gpui::list`
composites eleven of twelve scrolled frames while the workbench's own path
composites zero. A wheel over the transcript notifies the workbench, and
`owner_scrolled_only` wants the scroll to be the only thing that did — §7.3's
second blocker, which the transcript now shares with the sidebar.

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

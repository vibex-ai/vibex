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
  answers with `RuntimeUiRepaint { dirty, timeline_only }`. A timeline-only
  batch is applied by `notify_timeline_surfaces` (the focused `TimelineView`
  and the `SessionGroupPaneView`s); every other batch notifies the workbench.
  The agent poll classifies its timeline page the same way.
- The shell views (`SidebarView`, `TimelineView`, `SessionGroupPaneView`,
  `ChildAgentRailView`, `SessionTerminalView`) observe the workbench, so a
  workbench notify rebuilds all of them. A hot path that reaches
  `cx.notify()` on the workbench has to be worth rebuilding the shell.

## 4. Known upstream gaps

The desktop library tests carry six failures under gpui-fast that are not
missing app notifications. Each reproduces with
`cargo test -p vibex-desktop --lib <name>`.

Four of them also fail with `GPUI_VIEW_RETENTION=0`, so they come from
gpui-fast's retained layout or wheel path rather than view retention:

- `a_row_that_lands_in_one_frame_keeps_the_rows_under_it_still`
- `a_windowed_run_keeps_the_rows_under_a_closing_row_still_on_the_bottom_edge`
- `a_windowed_run_keeps_the_rows_under_a_disclosure_still_on_the_live_edge`
- `im_channels::tests::the_two_columns_scroll_independently`, which also fails
  with `GPUI_SCROLL_LAYERS=0`.

Two are retention mechanics:

- `a_click_and_a_hover_reach_an_element_inside_a_cached_view`: a hover over a
  plainly mounted nested view does not rebuild it.
- `arena::companion::tests::a_starting_turn_brings_the_companion_in_its_own_way`:
  an inline `with_animation` ticker does not advance the pose during the
  companion's entrance.

## 5. Timeline list layer

[Session Timeline Scrolling](./timeline-scrolling.md) names the `MessageScroller`
API the main transcript needs before it can move off `v_virtual_list`. As of
gpui-component 0.10.0 the published state still does not expose
`scroll_by`, `viewport_bounds`, `offset`, `max_offset`, per-row size hints, or a
row renderer that receives the owning view, so the transcript stays on its own
virtual list and the child Agent rail stays on `MessageScroller`.

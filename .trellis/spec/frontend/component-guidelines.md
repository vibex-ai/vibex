# Component Guidelines

Vibex UI components must make Agent activity, local workspace state, and remote
approval flows readable across desktop and mobile. `apps/desktop` is the only
visual, interaction, and information-architecture baseline.

Current evidence: [Architecture Baseline](../guides/architecture-baseline.md), GPUI Desktop source, and
source-bound GPUI parity evidence.

React/Tailwind/shadcn/Radix sections retained below are historical evidence from
deleted legacy apps, not maintenance contracts. Their paths, APIs, and test
instructions must not be used to build current GPUI components.

## Layout Components

PC desktop uses a multi-panel workbench:

- Left navigation for projects and conversations.
- Central area for Agent chat, editor, terminal, or integrated workspace tabs.
- Right rail for files, Git, details, or contextual panels.
- Collapsible sidebar, file panel, Git panel, and terminal panel.
- Split panes and tab navigation.

Compact GPUI shells use single-task list-to-detail navigation:

- Host/device list.
- Project list.
- Session list.
- Session detail.
- Permission approval.
- Files, Git, terminal, Provider settings, and system settings.

Keep layout state explicit and serializable enough for persistence or restore.

### Top Bar Critical Actions

Top-bar actions that open core surfaces, such as mobile/Web pairing and
settings, must remain reachable at narrow desktop widths. Keep these controls
outside the shrinkable title/path/status content flow, usually by anchoring them
to the right side next to native window controls and reserving matching
right-side padding on the draggable/title region.

Do not hide critical top-bar actions with breakpoint-only classes such as
`hidden ... min-[860px]:flex`. Non-critical badges may hide first, but the
buttons that open pairing, settings, or other primary workbench controls should
stay visible.

Icon-only top-bar controls and right-rail activity/plugin buttons should expose
a shadcn/Radix tooltip whose text matches the button's accessible title or
`aria-label`. For right-edge activity bars, place tooltips on the left side so
they remain visible inside the workbench viewport. Keep these icon-button
tooltips subtle and arrowless; the label text is the affordance. Icon-button
tooltips should wait about one second before opening so rapid button scanning
does not create visual noise, and they should close immediately when the pointer
leaves the trigger rather than staying hoverable and covering adjacent buttons.

The same contract applies to native GPUI controls. A `gpui_component::Button`
with a visible `.label(...)` uses that label as its AccessKit name. An icon-only
button may use `.tooltip(...)` as the accessible-name fallback, and the rendered
button must project the resolved value through `aria_label`; giving the floating
tooltip itself `Role::Tooltip` does not name its trigger. When both are present,
the visible label wins so explanatory tooltip copy cannot replace the command's
short accessible name.

### Documentation Help Buttons

A surface that needs a help affordance uses `gpui_ext::docs_help_button(id,
label, url)` rather than composing its own glyph button. The helper owns one
geometry (a 24px ghost target with a 14px `circle-question-mark` child, matching
the panel headers around it), sets `label` as both tooltip and AccessKit name,
and passes the URL through `validate_external_open_url` before the platform
opens it. Documentation pages live as `DOCS_*_URL` constants in the same module
so a domain change is one edit, and the glyph stays a child rather than
`Button::icon` so the frame owns its size. `gpui_ext` tests assert every docs URL
passes the external-open boundary and that the glyph is in the asset bundle,
because an unregistered icon path renders as nothing with no error.

### Window Caption Controls

The workbench draws its own minimize/maximize/close buttons only when it owns the
caption. The decision is one platform policy in `apps/desktop/src/app.rs`
(`resolve_window_controls`); the title bar only renders what that policy returns.

- macOS keeps its AppKit traffic lights, so the workbench draws nothing.
- Windows keeps the right-hand trio. GPUI maps those buttons to system hit areas
  (`WindowControlArea`), so the system runs the command, owns Snap Layouts, and
  keeps the native title-bar double click.
- Linux draws caption buttons only when the compositor negotiated client-side
  decorations. A compositor that answers `xdg-decoration` with `SERVER_SIDE`
  (Hyprland does) owns the caption, and painting a second one duplicates it.
- Web has no window to control and draws nothing.

For a client-decorated Linux window the desktop's own layout decides which
buttons exist and which edge they sit on: read GTK's `gtk-decoration-layout`
(`appmenu:close`, `:minimize,maximize,close`) instead of hard-coding the GNOME
order, fall back to the right-hand trio when the value cannot be read, and honor
a layout that names no caption button as "no buttons". Drop buttons outside the
window's advertised capabilities (`Window::window_controls`); closing always
survives.

Caption buttons render in `window-controls-left` / `window-controls-right`
clusters that sit outside the `WindowControlArea::Drag` region, and each button
stops mouse-down propagation so pressing a caption never starts a window move.
Every button carries the localized tooltip as its `aria_label`, and the maximize
button swaps to the restore glyph while `Window::is_maximized()` is true.

### Floating window chrome

`render_title_bar` is an overlay, not a layout row: it is positioned against the
window (`absolute().top_0().left_0().right_0()`) and paints above the shell but below
overlays. Every column that must not underlap it reserves `TITLE_BAR_HEIGHT` at its
top — the inline sidebar, the workbench column, the preview and right-rail panels,
and the activity rail — so a panel reads as a column under the bar instead of a
surface behind it, while every column keeps its full height.

The strip splits its treatment at the sidebar seam. The workbench end (the main
segment and the right cluster) paints the chrome surface and the closing hairline,
which is a child of the bar rather than a border so it can start past an open
sidebar — a rail that is open runs its own tone and seam up to the window edge, and
nothing separates it from the chrome. The control cluster owns no surface at all:
it keeps only the reserved width (the docked sidebar's width, or the collapsed
width) so the session title stays aligned with the workbench column, and its
controls keep one set of metrics — a square `TITLE_BAR_CONTROL_SIZE` control, a
`TITLE_BAR_CONTROL_GAP` inside a group, a `TITLE_BAR_GROUP_GAP` between groups, and a
`TITLE_BAR_CLUSTER_PAD` inset.

Anything interactive placed in the top `TITLE_BAR_HEIGHT` of a column without that
reservation is covered by the drag region, and a press on it starts a window move
instead of the control's command. A collapsed right rail draws no seam of its own:
the activity strip only adds its divider while the panel it belongs to is open. The
management view's compact sidebar limits read `TITLE_BAR_HEIGHT` too, so the chrome
height lives in one constant.

### GPUI Button hover ownership

The kit's `gpui-component` `Button` renderer owns the enabled/unselected hover
style. Do not call GPUI's generic `InteractiveElement::hover` directly on a
`Button`: the caller populates the base element's hover slot, then `Button::render`
tries to populate the same slot and GPUI panics with `hover style already set`.
Use a built-in `ButtonVariants` style and shared theme tokens. Use `.on_hover(...)`
only when an event callback is required; it does not replace the visual hover
contract. If a genuinely new visual variant is needed, express and test it through
the component variant API or a dedicated semantic control instead of stacking a
second generic hover refinement.

Wrong:

```rust
Button::new("preview-pane-new")
    .ghost()
    .hover(|style| style.bg(cx.theme().background))
```

Correct:

```rust
Button::new("preview-pane-new").ghost()
```

Regression coverage must render the enabled, unselected control through a real
GPUI fixture because compile-only and model tests do not execute `Button::render`.
The Code Workbench source contract additionally rejects a direct hover override on
its preview-tab add button.

### GPUI Entity update reentrancy

An entity update callback must not synchronously update the same entity through
another handle. GPUI panics when an `Entity::update` for `FoundationSettings`
(or any other view) is re-entered, including when a sibling workbench callback
publishes a result back to that view. Defer the cross-entity update until the
current effect cycle completes:

```rust
let settings = self.settings_view.clone();
cx.defer(move |cx| {
    let _ = settings.update(cx, |settings, cx| {
        settings.operation_note = note;
        cx.notify();
    });
});
```

Keep local state mutations inside the active callback; only the hand-off to a
currently-updating entity needs to be deferred.

### GPUI Dialog Input Lifetime

A `window.open_dialog` builder is evaluated again on every repaint of the view
that renders the dialog layer, and the workbench repaints often. An
`Entity<InputState>` created inside the builder is therefore replaced on each
rebuild: the text resets to its default value and the focus handle the user is
typing into disappears, which reads as an input that cannot be edited at all.

Create the input entity once before `open_dialog`, move it into the builder, and
request focus from `window.on_next_frame` because the dialog's focus trap claims
focus while it mounts. Select the current value so typing replaces it rather
than appending to it:

```rust
// Wrong: a new entity on every dialog repaint.
window.open_dialog(cx, move |dialog, window, cx| {
    let input = cx.new(|cx| InputState::new(window, cx).default_value(current));
    dialog.child(Input::new(&input))
});

// Correct: one entity, focused after the dialog mounts.
let input = cx.new(|cx| InputState::new(window, cx).default_value(current.clone()));
let input_for_focus = input.clone();
let selection_end = current.len();
window.open_dialog(cx, move |dialog, _window, cx| {
    dialog.child(Input::new(&input))
});
window.on_next_frame(move |window, cx| {
    input_for_focus.update(cx, |input, cx| {
        input.set_selected_range(0..selection_end, cx);
        input.focus(window, cx);
    });
});
```

A dialog that can reject its input must surface the reason inside the dialog.
Notes rendered on the page behind a modal are occluded, so a rejected value
looks like an unresponsive control. The exception is a rejection raised while the
dialog is still open for a correction that is not tied to a field: route it
through the notification layer, which stacks above every dialog.

A mobile overlay sheet that exists to be typed into — a rename, a new-project
path — follows the same rule from the other side: focus its field and call
`crate::platform::show_keyboard()` when the sheet opens. The mobile app keeps its
kit input entities for the lifetime of the app, so the failure is not a replaced
entity but a sheet that opens with the caret nowhere: the first keystroke is lost
and the user has to tap the field before the keyboard appears.

### Light Hints

Transient feedback that answers an action the user just took — a link copied, a
device paired, storage cleared, a setting rejected — is a light hint and must be
shown through the kit's `Notification` on the notification layer
(`window.push_notification`). Do not hand-roll a banner, strip, or colored box in
the page for it.

The reasons are the ones the kit component already solves: the notification layer
stacks above sheets and dialogs, it auto-hides, it is click-dismissable, and a
hint pushed with the same `.id::<T>()` replaces the previous one instead of
stacking duplicates. A page banner has none of that and needs the page to
remember to clear it on the next action. That the hint clears a dialog backdrop
is not free — it comes from the layer's deferred priority, so read "Overlay Layer
Z-Order" before moving or re-mounting the layer.

```rust
struct GitMutationNotification;

Theme::global_mut(cx).notification.placement = Anchor::TopCenter;
window.push_notification(
    hint_notification(NotificationType::Success, message, cx)
        .id::<GitMutationNotification>()
        .autohide(true)
        .on_click(|_, _, _| {}),
    cx,
);
```

Rules that keep the pattern consistent:

- Build every hint through `gpui_ext::hint_notification`. The kit paints
  `Notification::message` as text no selection layer can reach, so a hint built
  with `Notification::info` / `success` / `warning` / `error` shows a message the
  user cannot copy — including the error text they most often need to paste
  somewhere else. `hint_notification` carries the same message as selectable
  content instead, and keeps tone, placement, autohide, and the replace-by-id
  contract on the kit's component. A source contract in `app.rs` rejects the raw
  tone constructors, so a new hint cannot regress to unselectable text.
- Set `Theme::global_mut(cx).notification.placement = Anchor::TopCenter` before
  pushing, and give each hint family its own private zero-sized id type so
  unrelated hints do not replace each other.
- Pick the type from the meaning, not the page: `success` for a completed action,
  `error` for one that failed, `info` for a neutral result, `warning` for one the
  user should look at. Do not paint a failure with a success tone.
- Present the hint from `render` when the producer has no `Window` (an async
  callback or a background task). Keep the pending hint on the owner entity, take
  it in the presenter, and push it inside `window.defer` so the push never
  re-enters the update that produced it.
- Keep a message that is still true on screen as state instead of a hint:
  connection status with a retry affordance, loading and progress lines, and
  persistent error banners with a recovery action are not light hints.
- Keep a validation message inside the dialog or field that rejected the value;
  the notification layer is for results, not for pointing at the control that
  needs correcting.

The selectable message owns three interactions that a hint must not lose, and
each one is covered by a test in `app.rs`:

- A release that resolved a selection over the message is a copy gesture, not a
  click, so it must not dismiss the hint and take the selected text with it. The
  guard reads the run's own selection snapshot rather than the window selection,
  because a selection elsewhere in the window is not this hint's gesture.
- A plain click still dismisses the hint, and it must not move focus: a tracked
  focus handle takes focus on mouse down unless the press is prevented, which
  would pull the caret out of whatever the user is typing at.
- The copy shortcut is dispatched from the focused node, so a release that left a
  selection takes focus. Without that step the selection exists but
  `ctrl-c`/`cmd-c` copies nothing.

### GPUI Post-Mutation Scroll Timing

When a GPUI action changes text or other content whose layout determines a scroll
range, do not calculate the final scroll target in the first `on_next_frame`
callback. GPUI runs next-frame callbacks before that frame's `draw`, so the first
callback can still observe the pre-mutation layout and clamp the requested offset
against a stale scroll range. Use the first callback as a layout barrier and apply
the cursor or item reveal from a second next-frame callback after the updated
content has been drawn.

Cover this behavior with a real GPUI layout test. The test must perform the content
mutation in the same action order as production, advance both frame boundaries,
and assert that the target range is actually laid out inside the viewport. A
source-string assertion or a test that only checks callback registration does not
prove scroll behavior.

### Overlay Layer Z-Order

Production GPUI workbench roots must mount the component overlay hosts after the
main shell content. Append `Root::render_sheet_layer`,
`Root::render_dialog_layer`, and `Root::render_notification_layer` in that
stacking order from the root view that owns the window. Calling
`gpui_component::init`, constructing `Root`, or invoking `window.open_sheet`
alone is not evidence that the corresponding layer is rendered. Keep sheet
state in the window/root owner (`has_active_sheet`, `close_sheet`) so title-bar
buttons, Escape/outside close, and programmatic startup all observe one overlay.

Mounting order is not z-order. Only inline content paints in tree order: the kit
renders dialogs through `gpui_base::Dialog`, which is a `deferred` draw at
priority `10 + layer`, popups (menus, selects, popovers, `gpui_base::Popup`) at
`POPUP_PRIORITY` (100), and tooltips at 200. A layer left inline therefore paints
*under* every dialog backdrop, which dims anything it hosts. Any overlay whose
content must stay readable over a dialog — the workbench's top-centered
notification layer, for example — has to be deferred at a priority inside the
band it belongs to:

```rust
const NOTIFICATION_LAYER_PRIORITY: usize = 99;

fn render_notification_layer(window: &Window, cx: &App) -> impl IntoElement + use<> {
    deferred(
        div()
            .absolute()
            .top_0()
            .left_0()
            .right_0()
            .flex()
            .justify_center()
            .child(Root::read(window, cx).notification.clone()),
    )
    .with_priority(NOTIFICATION_LAYER_PRIORITY)
}
```

Pick the band from what the surface is for: above the dialog band for transient
feedback that answers an action taken in the dialog, below `POPUP_PRIORITY` so an
open menu or the tooltip under the pointer still wins. Assert the result against
the painted scene (`window.painted_quads()`), not against the element tree: two
layers can sit in the order the root mounts them and still paint the wrong way
round.

Floating sidebars rendered with shadcn/Radix `Sheet` must account for nested
portaled overlays such as `DropdownMenu`. If a sidebar action menu portals its
content outside the Sheet subtree, the Sheet outside-interaction handler should
ignore interactions inside that menu content so opening or using the menu does
not collapse the sidebar drawer. Check both the wrapper event target and Radix
`event.detail.originalEvent.target`, and prefer `DropdownMenu modal={false}`
for action menus inside Sheet content to avoid competing modal focus layers.

Project/session sidebars may collapse session groups per project. Keep the
collapsed project ids in the workbench owner rather than inside one rendered
sidebar instance, so inline sidebar, floating drawer, and hover preview all show
the same expanded/collapsed state. A "collapse all" control should snapshot the
previous collapsed-id set before collapsing every project and restore that
snapshot on the next activation. Use recognizable expand/collapse iconography
for this global control rather than history or generic chevron symbols. Pinned
sessions should keep a persistent inline marker before the session title so the
pin state remains visible when the row action buttons are hidden.

A session-group workspace renders several Agent sessions side by side, and every
pane owns a complete per-session view: timeline, derived projections, scroll and
measurement state, expansions, runtime selection and composer. Focus only moves
the keyboard — losing focus must never stop a pane's timeline from updating or
change what that pane renders. The render pass is not the whole frame, though:
virtual lists and custom elements build their rows during layout and prepaint,
after the workspace already handed the borrowed view back. Code that renders or
measures a pane's rows there has to borrow that pane's own view first
(`with_session_view_for_render` in `apps/desktop/src/app.rs`). Resolving
payloads, expansion state, workspace paths or row heights against whichever view
happens to be borrowed paints the focused session inside another pane and leaves
that pane's layout frozen at its first estimates, which shows up as a timeline
that stops updating and as blank bands between rows. Measurements a pane records
during prepaint belong to the same view, and the transient borrow is
deliberately unweighed so a per-row swap does not re-walk a whole timeline.

The same rule covers input that arrives without focusing the pane. A wheel event
is routed to the element under the pointer, so scrolling a pane the keyboard is
not on clears that pane's follow state — borrow its view in the listener too. A
follow state written into the borrowed (focused) view leaves the scrolled pane
still following its own stream, so the next chunk drags the reader back to the
bottom, and the focused pane stops following for a reason the user never asked
for. Mouse-down interactions do not need this: the pane focuses itself on
mouse-down before the click handler runs.

A pane's render pass must be free of asynchronous side effects, and it must not
re-weigh the view it borrows. Rendering runs once per pane per frame, so anything
a pane starts there is multiplied by the split: syncing auto-continue from the
composer re-entered the completion probe — a backend round trip whose answer
repaints the window — once per pane, per frame, and the workbench never reached an
idle frame. Drive such work from the events that change its inputs (a session
snapshot, an adopted timeline, a preference toggle, a countdown tick, a submitted
message) and let the pane read only what those cached. The same multiplication
applies to derived values and to elements that are not on screen: share a
projection per distinct runtime selection instead of memoizing one slot a split
thrashes, and build a closed dropdown's rows only while it is open. The pane's own
borrow is unweighed (`borrow_session_view_unweighed`); the pane hands its own
borrow back once it has been built, and the workspace drops the pane views the
layout no longer has.

A group pane owns its element tree behind a cached view boundary
(`SessionGroupPaneView` in `apps/desktop/src/app.rs`). The workbench is a single
entity, so building the panes inline made every repaint cost N panes and made one
pane's animation everyone's problem: `Window::request_animation_frame` notifies
the view whose tree holds the animated element, and that view was always the
workbench. A cached pane re-renders alone, so a spinner, a shimmer or a
scrollbar in one pane no longer rebuilds its siblings. Because the pane still
reads the workbench's state, its view observes the workbench and refreshes on any
workbench notify — that is what keeps a cached pane from going stale. The same
rule applies to any long-lived animation inside a pane: a repeating element is a
frame driver for as long as it is mounted, so it must be reserved for work that
is actually in flight. The agent thinking shimmer is the worked example: a row's
`streaming` flag outlives an interrupted turn, so the sweep asks the session
(`borrowed_session_turn_is_live`) and an idle row keeps the label without the
animation.

A drag over the group workspace is resolved per pane, and only the pane that
contains the pointer may claim the shared drop target. GPUI dispatches a typed
`on_drag_move` callback to every rendered target that listens for that drag type,
so each pane measures the same pointer against its own bounds; the ownership
check and the region both live in `session_group_pane_drop_region` in
`apps/desktop/src/app.rs`. A pane the pointer is not inside must neither claim
the target nor clear a claim another pane just made — release it only while it
still owns it. Without that check the pane painted last won every move: a tab
dragged inside its own pane resolved to the "merge into this pane" region, which
is a no-op for the pane it came from, while the same drag onto the last-painted
pane still split it. A drag that has not left its own pane cannot reorder tabs,
so below the tab strip it reads as a split on the axis the pointer left the
pane's center along.

Project headers in the session sidebar should display the project name only,
not the workspace root path, to keep the rail scannable. Project-header clicks
that expand/collapse sessions or activate a project are internal sidebar
interactions and must not auto-close a floating Sheet/sidebar; reserve automatic
drawer closure for explicit navigation actions that intentionally leave the
sidebar context.

When explicit navigation closes a hover-preview sidebar, suppress hover-driven
reopening for longer than the sidebar's exit transition. The closing panel stays
mounted during that transition, and residual pointer events from a slightly
dragged click can otherwise reverse the animation halfway through. Capture the
pre-navigation hover state before clearing it and use the shared suppression
path; docked sidebar navigation must keep its requested-open preference.

```rust
let hover_preview_was_open = self.sidebar_hover_preview_open;
// Apply navigation state before clearing the preview.
if hover_preview_was_open {
    self.suppress_sidebar_hover_preview();
} else {
    self.close_sidebar_hover_preview();
}
```

Regression coverage must assert both the requested-open/drawer result and that
hover-preview navigation enters the suppression path.

Popup menus opened from a hover-preview sidebar (row context menus, ellipsis
dropdown menus) occlude the sidebar panel, so the panel's `on_hover(false)`
fires while the pointer is over the menu and would collapse the sidebar out
from under it. Latch the preview open whenever such a menu is built: set a
menu-open latch (which also cancels any pending close), subscribe to the menu
entity's `DismissEvent` to release the latch, and guard the delayed-close
scheduler with the latch. On dismissal, re-arm the auto-close only when the
pointer is outside both the floating panel and the title-bar trigger strip,
measured from `window.mouse_position()` because the panel's hover state is
stale after occlusion. Every dismissal path (item click, Escape, click
outside) emits `DismissEvent`, so the latch cannot stick; each sidebar menu
builder must arm it, and source-inspection tests should assert that.

Panel resize handles should stay below floating sidebars, drawers, dialogs, and
other portaled overlays in z-index. The resize hot zone only needs to sit above
its owning panel content; using overlay-level z-index values can make resize
guides or hover zones bleed through floating sidebar previews.

Brand and product logos that are unavailable in lucide-react should render
through Iconify React in offline mode, using a checked-in subset of Iconify logo
data rather than hand-drawn local SVG approximations or runtime network icon
fetches. Prefer multicolor `logos:*` icons for products such as VS Code,
JetBrains IDEs, OpenAI, Claude, and Gemini; use `simple-icons:*` only when no
multicolor logo exists in Iconify. When extracting a local Iconify subset, keep
the source collection's root-level default `width` and `height` values as well
as per-icon overrides; missing collection dimensions can produce a too-narrow
SVG viewBox and visibly clip logos.

In GPUI, render SVGs with fixed embedded brand colors through `gpui::img`.
`gpui_component::Icon` paints SVGs as alpha masks and therefore collapses those
colors into one theme color. Reserve `Icon` for `currentColor` SVGs and other
intentionally monochrome glyphs.

GPUI new-session runtime selectors must follow the Tauri responsive contract.
Above `860px`, show the icon, selected value, and chevron; at `860px` and below,
use fixed `32px` icon-only triggers while keeping the selector name and current
value in the tooltip. Use the Provider database, current model brand, reasoning
brain, and conversation-mode shield icons in that order.

Non-compact runtime-selector dropdown triggers size intrinsically to the current
selected label. Do not assign per-selector widths or truncate the selected label;
keep the icon, no-wrap label, and chevron non-shrinking. Wrap the `Popover` in a
`flex_none` container so the composer row scrolls and the new-session row wraps
before any trigger is compressed. Applying `flex_none` directly to `Popover`
styles its overlay content rather than its rendered trigger wrapper.

When the composer runtime-selector row is narrower than its contents, keep it
horizontally scrollable without rendering a scrollbar. GPUI's x-only overflow
maps a regular mouse wheel's vertical delta onto the horizontal axis, so users
can move through the choices by scrolling directly over the row.

```rust
// Wrong: short values waste space and long values are clipped.
Button::new("runtime-selector").w(px(112.0)).child(truncated_label);

// Correct: the selected label contributes its full intrinsic width.
let trigger = Button::new("runtime-selector").px_2().child(intrinsic_content);
div().flex_none().child(Popover::new("runtime-menu").trigger(trigger));
```

## Timeline Cards

Render Agent activity through provider-neutral cards:

- User message.
- Agent message.
- Reasoning/thought.
- Plan step and Todo.
- Tool call.
- Command execution.
- File operation.
- Git diff update.
- MCP call.
- Permission request.
- Compact/context boundary.
- Subagent/task event.
- Delegation, Team, and automation run event.
- Error, warning, and system notice.

Cards that can grow large must be collapsible. Tool, diff, terminal, and plan
cards should support compact summaries for mobile.

A user message delivered by a queued action marks itself on the bubble's lower
edge through the kit's reaction region (`BubbleReactions` with an icon-only ghost
`Button`), never through a chip above the bubble or a tinted bubble border. The
glyph carries the delivery hue (steer green, interrupted resend yellow) so the
hue survives hover, while the delivery label is the button's tooltip and its
accessibility name. A bubble that owns a reaction reserves the strip the pill
hangs into (the kit anchors it `1.25rem` below the edge) so the mark cannot
overlap the row's hover action line.

Delegation and Team cards should show parent/child session links, role or slot,
current state, pending permission ownership, and latest result summary without
requiring components to read provider-native child-turn payloads.

## Preview Components

Generated result previews are part of the coding workflow, not a separate office
suite. Preview UI should support:

- Markdown, code/text, HTML, image, diff, logs, test reports, and common binary
  file summaries.
- Source/preview toggles where both representations matter.
- Multi-tab preview state on desktop.
- Snapshot/version history when the backend exposes it.
- Open in system app, download, copy path/content, and request-Agent-edit
  actions.

Mobile/Web previews should stay lightweight: read-only rendering, diff review,
history inspection, and explicit "ask Agent to modify" flows are preferred over
large embedded editors.

### Markdown Table Scrolling

When a Markdown table is wider than its viewport, keep a persistent
`ScrollHandle` for that table node and attach the overflow container and visible
horizontal scrollbar to the same handle. A predominantly vertical wheel gesture
over the table must move that handle horizontally and stop propagation while
the table has horizontal overflow, including at either horizontal edge. This
prevents the table and its scrollable page or timeline ancestor from moving at
the same time. If the table has no horizontal overflow, leave the event
unconsumed so normal page scrolling still works.

Cover this behavior with a real GPUI layout test: place a wide table inside a
parent with a non-zero vertical scroll range, dispatch a vertical
`ScrollWheelEvent` over a laid-out table cell, then assert that the table's x
offset changed and the parent's y offset remained zero.

### Empty States

Empty-state cards rendered inside right rails, preview tabs, file panels, Git
panels, and other resizable workbench panes must size against the available pane
width, not their content's intrinsic minimum width. Use `w-full min-w-0` on the
card/container plus normal text wrapping on title and description. Otherwise
narrow panes can collapse Chinese or long unspaced copy into one-character-wide
vertical columns.

### Diff Views

Diff line highlighting must define readable foreground colors for both light
and dark themes. Do not use light-only text classes such as `text-*-100` unless
they are guarded by a `dark:` variant; in light theme, added/deleted/hunk text
should use darker foregrounds while the background carries the status tint.

Diff summary counts must come from the same line-level edit script used to
render the preview. Do not treat everything between a shared prefix and suffix
as changed: separate edits with unchanged lines between them would inflate both
the added and removed counts. Preview context and truncation may bound rendered
rows, but must not change the full edit-script totals.

A height-capped GPUI diff must resolve that cap into a definite scroll viewport.
Give the stateful node that tracks the persistent `ScrollHandle` an explicit
bounded size, use GPUI overflow on that node, and attach both-axis scrollbars to
the same handle. Content-sized descendants inside an ancestor with only a
maximum height are not sufficient evidence that vertical wheel scrolling works.

When that diff viewport is nested inside a scrollable timeline, attach an
`on_scroll_wheel` handler to the same stateful node and call
`cx.stop_propagation()` unconditionally. GPUI applies the viewport's overflow
scroll before bubble listeners run, so the diff still moves while the timeline
does not. Leaving the event unconsumed makes both scroll handles move; consuming
it only when the diff is away from an edge introduces unwanted scroll chaining
at the first or last row.

Cover both axes with a real GPUI layout test: render content taller and wider
than the viewport inside a parent with a non-zero scroll range, dispatch
vertical and horizontal `ScrollWheelEvent` values inside the diff, and assert
the diff handle offsets, the parent offset remaining zero, and a late row's
changed bounds for the vertical path. Compile-only checks and source-string
assertions do not exercise GPUI layout, wheel routing, or event propagation.

### File Tree Icons

Desktop file trees, including the right rail file preview panel, should keep
file icon shape and file-type color in one descriptor map keyed by filename or
extension. Do not maintain parallel icon and color maps that can drift.

File-type icon colors should come from theme CSS variables, not raw Tailwind
color classes. Keep Git status, selected state, and ignored state readable as
separate row/text state; file-type color must not become the only status signal.

When a directory contains exactly one child and that child is also a directory,
desktop file trees may compact the chain into one row such as
`archive / 2026-06`. The compacted row should render the full loaded
single-directory chain, not a fixed number of segments. Each displayed segment
should remain independently clickable; selecting a segment should make the
following rows render from that directory as the temporary subtree root, while a
row-level toggle should expand/collapse every directory path in the displayed
chain together. Segment selection may add the clicked chain to expanded state
once, but derived effects that watch selected path or rendered rows must not
re-add chain paths after the user toggles them closed. Do not restrict
expand/collapse to only the folder icon or one segment button. Segment-specific
hover/focus affordances should apply only when the row displays a real compact
chain with more than one segment, and the hover state should remain visible for
as long as the pointer stays over the segment hit area. Prefer text emphasis or
a highlighted subtle underline rather than fading to a neutral gray; use
explicit pointer/focus state when CSS pseudo-class hover does not remain stable
in nested row controls. Avoid dark inline background chips or persistent blocks
that compete with the full-row selection state.
Git Changes trees should use the same compact directory-chain visual language
as the Files panel, with a single checkbox for the compacted directory row and a
row-level expand/collapse action that toggles every directory path in the chain
together.
Git Changes file-name status colors should reuse the Files panel status text
mapping where it does not conflict with Git semantics: unstaged new files are
red, staged added files use the added color, modified-like files use the
modified color, and deleted files keep a muted strikethrough state. Keep the
short status badge as the precise Git state indicator.
Git History commit-file lists should reuse the Git Changes tree renderer and
directory compaction instead of building a separate tree. Convert
`GitCommitDetail.files` into the same view-model shape used by Git Changes
rows, but keep history rows read-only: no staging checkboxes or working-tree
mutation affordances. When the history surface only needs the file list, request
commit detail with `includePatch: false` so selecting a commit does not load a
large patch unnecessarily.

Selected file rows should use a full-row highlight frame, not just text color,
so selection remains obvious when file-type icon colors and Git status colors
are also present.

Desktop file trees may support drag-to-move for files and directories through
the typed file rename/move mutation. Drop targets should be directories only,
including the workspace root, and must reject no-op moves, moving into the same
path, and moving a directory into one of its descendants. For compact directory
chains, dragging the row should move the first displayed segment so the whole
chain moves together, while dragging a hovered segment should move that specific
directory. Segments may also act as directory drop targets when doing so keeps
the target unambiguous. While dragging inside a scrollable file tree, hovering
near the top or bottom edge should auto-scroll the list so users can move items
beyond the currently visible rows. When a hovered directory drop target is
expanded, the visible rows in that directory subtree should receive a subtle
full-row range highlight in addition to the stronger target-row highlight, so
the destination scope is clear. Visible non-directory rows inside an expanded
directory may also accept drops by resolving the destination to their parent
directory, so users do not have to release exactly on the directory row after
the intended folder is already expanded. Do not make every draggable row or
segment show a persistent hand/grab cursor in its normal hover state; keep the
normal file-tree cursor calm and reserve grab-style cursor feedback for the
active drag state.

File tree typeahead should be scoped to currently rendered rows only. When the
file tree panel has focus and users type characters, show the current typed
buffer in the panel without truncating it, highlight visible files and visible
directory segments whose displayed names contain that exact continuous text,
and mark the matched substring characters with a subtle background. Keep that
substring highlight stable until the user deletes the entire typed buffer,
presses an explicit clear key such as Escape, or focus leaves the file tree
panel; do not clear it through a short inactivity timer. Do not search folded,
unloaded, or otherwise hidden descendants.

Desktop file trees should provide a right-click context menu on both rows and
empty file-tree space. Row context targets should act on that file, directory,
symlink, or compact-chain segment target; empty-space context targets should
act on the workspace root or currently shown root so users can create or paste
even in an empty directory. Keep filesystem mutations behind typed file hooks
and backend service commands instead of direct browser-only state updates.
File explorer panel headers may include compact workspace-level actions such as
a fullscreen toggle and an Open With dropdown. Fullscreen should enlarge only
the right-rail file panel surface and keep a clear exit affordance in the same
header. Header Open With actions operate on the workspace root and should show
stable system actions such as File Manager and Native Terminal plus only
backend-detected installed IDE/project tools; do not hard-code unavailable IDEs
in the UI, and do not pass arbitrary shell command strings from React. When
space allows, prefer a split button: the primary side shows the currently
selected tool icon only and directly opens the workspace root with that tool,
while the chevron side only opens the tool menu. Before the user selects a
tool, the primary side should show the same generic Open In icon used by the
menu entry.
Menus should include create file/folder, cut, copy, paste, copy relative path,
copy absolute path, copy file name, rename, delete, and Open In actions where
the current target supports them. Destructive delete still requires explicit
confirmation through an app-styled Dialog, not a native `window.confirm`, and
Open In must use desktop shell capabilities only after resolving the selected
relative path against the workspace root. The Editor Open In action must be
disabled for directories, symlinks with unknown targets, compressed archives,
media, fonts, executables, database files, Office/PDF documents, and other
known binary-like extensions such as `gz`. Default App and Native Terminal Open
In actions should call backend/Tauri file commands with the workspace-relative
path, not construct absolute filesystem paths in React; Native Terminal opens a
directory directly and a file's parent directory. Row and empty-space Open In
menus should also include backend-detected IDE/project tools, grouped separately
from fixed system actions, and call the typed open-with-tool command for the
current context target path. File rows should not show
persistent or hover-only trailing delete icons; keep destructive actions in the
context menu or editor chrome. New file/folder actions should insert an inline
pending row under the target directory with a focused name input; Enter or blur
commits the typed name, Escape cancels, and empty input cancels without
mutation. Guard the pending row against duplicate Enter/blur submission until
the create mutation settles, and prefer read-only/submitting styling over
disabling the input if disabling would trigger blur and re-enter the commit
path. Rename must use the same inline input interaction in the existing row or
compact-chain segment instead of a native prompt. Inline create/rename
validation or mutation errors should render beneath the pending input with
wrapping text so narrow right rails do not hide the message horizontally. When
inline rename is started from a Radix context menu, defer mounting/focusing the
input until after the context menu has closed; otherwise the menu's close-time
focus restore can blur the new input and commit the unchanged name, making edit
mode appear to flash and immediately exit. Directory rename must update every
visible file-tree cache entry whose path or parent path is the renamed directory
or one of its descendants, and must also copy expanded subtree query cache keys
from the old directory path to the new directory path; otherwise stale expanded
subtree data can make the folder appear to revert even though the filesystem
rename succeeded.
If the app has a global native `contextmenu` suppressor, it must skip
Radix/shadcn context menu triggers so Radix can receive the unmodified
right-click event and position the custom menu. While a row context menu is
open, the target row should use the same full-row selected frame as normal file
selection. If users right-click a different row while a context menu is already
open, the previous menu should be replaced by a newly positioned menu for the
new target.

Wrong:

```tsx
const iconByExtension = new Map([["md", BookOpenText]]);
const colorByExtension = new Map([["md", "text-blue-400"]]);
```

Correct:

```tsx
const iconByExtension = new Map([
  ["md", { icon: BookOpenText, tone: "markdown" }]
]);
```

## Rendering Performance for High-Frequency Workbench Panels

The desktop workbench owner component subscribes to high-frequency state
(terminal polling, git status, file tree, chat streams). Any panel it renders —
especially the compound preview panel, whose tab strip renders one Radix
`ContextMenu` tree per tab — must be isolated behind `memo` boundaries with
referentially stable props, or every unrelated state tick re-renders the whole
panel tree and the UI feels sluggish as tab count grows.

### Convention: memo boundaries need stable props end to end

**What**: Wrapping a component in `memo` is only half the contract. Every prop
passed to it (and to memoized children like `PreviewTabButton`) must be
referentially stable: callbacks via `useCallback`, derived arrays/maps/values
via `useMemo`, and constant fallbacks as module-level constants (e.g.
`const EMPTY_OPEN_TOOLS: FileOpenTool[] = []`), never fresh `[]`/`{}` literals
or inline arrows in JSX.

**Why**: A prior perf pass found `PreviewTabButton` was already `memo(...)` but
completely ineffective because the pane view passed six inline handler consts
and a per-tab `openTools.filter(...)` array — new identities every render.

**Wrong**:

```tsx
<PreviewTabButton
  detectedOpenTools={fileOpenPath ? openTools.filter((t) => t.kind === "ide") : []}
  onOpenTabInEditor={(tab) => { /* inline const or arrow, new identity per render */ }}
/>
```

**Correct**:

```tsx
const ideOpenTools = useMemo(() => openTools.filter((t) => t.kind === "ide"), [openTools]);
const handleOpenTabInEditor = useCallback((tab: PreviewTab) => { ... }, [onOpenFileForEdit]);

<PreviewTabButton
  detectedOpenTools={fileOpenPath && selectedWorkspaceId ? ideOpenTools : EMPTY_OPEN_TOOLS}
  onOpenTabInEditor={handleOpenTabInEditor}
/>
```

Per-item shared derived state (such as the closable-tab count feeding every
tab's context-menu enablement) must be computed once per pane with `useMemo`,
not rescanned inside each tab render.

> **Warning**: The repo ESLint config does not enable
> `react-hooks/exhaustive-deps`. Dependency arrays for new `useCallback`/
> `useMemo` hooks must be audited manually; list every component-scope reactive
> capture. React-query `mutation.mutate` and Zustand actions are stable and safe
> to depend on directly.

> **Warning**: Do not replace the per-tab Radix `ContextMenu` in the preview
> tab strip with a single shared context menu. That direction was tried and
> rolled back because it felt slower in desktop testing; do not reapply it
> without profiling evidence.

## Permission Components

Permission UI must show:

- Requested action.
- Provider/session/project context.
- Risk category.
- Details and diff/command preview where applicable.
- Available responses.
- Which device/user resolved it after completion.

Primary approve/deny actions must be reachable by thumb on mobile. Dangerous
actions need clear confirmation, especially terminal commands, file deletion,
Git revert, push, and native config export.

In the conversation timeline, an adjacent `Command` and command-risk
`PermissionRequest` from the same turn form one interaction. Render the command
once, put the approval actions in its footer, and let the pending permission
override any provisional `Running` label. Do not stack a second permission card
that repeats the same command.

Permission presentation is a user-facing projection, not a dump of provider
transport details. Parse structured input to recover actionable fields such as
the command, arguments, and working directory, while suppressing correlation,
routing, and response-option fields such as `requestId`, `toolCallId`, `tool`,
and `options`. A standalone permission card may retain additional clearly
user-facing details, but raw JSON and provider bookkeeping do not belong in the
message body.

When a turn is blocked on approval, both the command card and the pending-turn
footer must say that confirmation is awaited; neither may imply that execution
is already running. Resolved historical permissions keep their outcome and no
longer render response buttons.

## Provider Components

Provider UI must show Vibex Provider Profiles, not raw config files. Components
for runtime injection preview should display redacted env, headers, endpoints,
SDK options, CLI args, and temporary config overlay paths.

The Agent detail surface is a user-facing authentication and Profile-management
surface, not a diagnostics console:

- Render dynamically advertised Agent, environment, and terminal auth methods
  before the selected Agent's Provider Profiles.
- Keep method choices as separated rows within one authentication section; do
  not nest decorative method cards inside an outer card.
- Secret environment fields use masked inputs and expose only a configured
  marker plus an explicit clear action. Optional fields are labeled; exact
  Agent-provided environment names remain visible.
- The native Desktop Provider Profile API Key editor is the deliberate local
  exception: it may resolve the saved value through `DesktopRuntime`, place it
  in a masked `InputState`, and expose the standard eye toggle. Do not represent
  a configured key with a fixed `***` placeholder because the toggle would have
  no original value to reveal. The plaintext remains confined to that local
  editor state and the Secret mutation request.
- Render logout only when advertised. A terminal method opens the shared
  Terminal surface with a close action and explicit running/success/failure
  state.
- Do not render runtime verification, runtime-option snapshots, Provider
  projection internals, raw ACP payloads, or resolved authentication-method
  credential values in the Agent detail surface. The local Provider Profile
  editor exception above applies only to its explicit masked API Key control.

Native export UI must always include diff, backup, atomic write, and rollback
information.

## Styling

- Use semantic tokens from `crates/vibex-ui/theme/tokens.json` across desktop and native mobile GPUI. See [Themes](#themes) before adding or changing a palette or a role.
- Every panel integrated in the right rail — files, Git, and the child Agent
  timeline — paints the shared `right-rail-surface` token as its base, and the
  section washes inside it layer that same token rather than `sidebar` or
  `background`. A rail panel that paints another surface token reads as a
  different pane from its neighbours.
- Preserve dark mode as a first-class path, and keep every curated theme in both appearances equally readable.
- Use shared GPUI/gpui-component primitives; legacy React may keep shadcn/Radix until cutover.
- GPUI delete, remove, clear, and destructive close actions must use the shared
  `icons/vibex/trash-2.svg` glyph. Do not use `IconName::Delete`: the locked
  component library renders it as a backspace-style symbol rather than a trash
  can, which makes destructive actions ambiguous. Conditional actions must
  return `Icon` values from every branch:

  ```rust
  // Wrong: renders the backspace-style delete glyph.
  Button::new("delete-item").icon(IconName::Delete);

  // Correct: uses the shared destructive-action glyph.
  Button::new("delete-item").icon(Icon::default().path("icons/vibex/trash-2.svg"));

  // Correct: both conditional branches resolve to Icon.
  button.icon(if clearing {
      Icon::new(IconName::Undo2)
  } else {
      Icon::default().path("icons/vibex/trash-2.svg")
  });
  ```

  Desktop source-contract coverage must reject `IconName::Delete` in
  `apps/desktop/src/app.rs` and `apps/desktop/src/management.rs`.
- Text-bearing overlays such as dialogs, sheets, dropdown menus, selects,
  context menus, command palettes, and tooltips should avoid scale/zoom
  animations and backdrop blur. In Tauri/WebKit these effects can keep text on
  a composited layer and make glyphs look soft or fuzzy. Prefer opacity and
  slide-only transitions with opaque `bg-popover` surfaces. For centered
  dialogs, prefer `inset` plus auto margins over translating the content
  container with `transform`; transform-based centering can soften all text
  inside the dialog.
- Do not default to flat, generic layouts when implementing new screens. Follow the
  current GPUI Desktop workbench and its domain-component language.

### Themes

`crates/vibex-ui/theme/tokens.json` is keyed by theme id. Each entry carries a
display name, the appearance it is authored for (`light` or `dark`), the full
semantic role map, and a syntax highlight block. `defaultTheme` names the
product default for each appearance. The file is the only place a built-in
palette is authored; `scripts/generate-tokens.mjs` validates it, converts OKLCH
to sRGB, and emits `crates/vibex-ui/src/generated_tokens.rs`.

- **A theme variant belongs to exactly one appearance.** A light palette is not
  a valid dark selection; resolvers reject the mismatch and fall back to the
  appearance's default rather than painting unreadable text.
- **Light and dark selections are independent.** Persisted state stores one
  theme id per appearance, so any light palette can pair with any dark one.
  Never collapse them into a single "current theme" setting.
- **All themes carry the same role set.** A new role must be added to every
  theme in `tokens.json`; the generator rejects a file whose themes disagree on
  role names or order. Add the role to the default themes first, then to the
  curated ones.
- **Resolve colors through the active theme, never a fixed pair.** Use
  `cx.theme()` for component surfaces and `theme::semantic_color(name, is_dark)`
  where no context is available. A missing role panics by design: it is a
  build-time contract between the token source and the call site.
- **Derived tones follow the theme.** Washes, scrims, and mixed plates must be
  computed from the active theme's own background or foreground so a warm or
  tinted palette does not receive neutral grey chrome.
- **Contrast is a gate, not a preference.** Body and muted text must clear
  4.5:1 against the surface they are painted on, in every theme.
- **The gpui-component palette is completed, never left partial.**
  `Theme::change` reloads gpui-component's own palette, so every token an
  appearance pass does not assign keeps a stock neutral color. After mapping
  the product roles, a client calls
  `vibex_ui::apply_component_palette(theme, active_theme)` and then
  `Theme::sync_base(cx)`, which publishes the result to the base layer that
  owns scrollbars, resize handles, and the text view defaults. Skipping either
  step is what leaves switches, segmented tabs, outline buttons, and skeletons
  grey under a tinted palette.
- **Token ownership is explicit.** `CORE_TOKENS` is what a client maps before
  the bridge runs (surfaces, text, borders, washes, high-contrast overrides);
  `COMPONENT_TOKENS` is what the bridge owns. The two must together cover every
  color token gpui-component defines — the bridge's coverage test fails on a
  framework upgrade that adds one, so a new token is a decision rather than a
  silent stock color.
- **Component colors derive from the active theme; they are not authored
  twice.** Where the catalog owns a matching role the bridge uses it directly
  (`switch` ← `muted`, `danger` ← `destructive`, `success` ← `chart-2`,
  `info` and `link` ← `chart-category-1`). Elsewhere it derives from the theme:
  status inks take the pole that contrasts most with their plate, hover and
  pressed plates mix toward the theme foreground, and `_light` swatches mix
  toward the theme background.

Users can add their own palettes by dropping a theme file into the `themes`
directory under the app home. A file names only the roles it changes;
compilation starts from the built-in default for the entry's appearance and
re-derives every unpinned `*-foreground` against the surface it is painted on.
A malformed entry is reported and skipped without hiding its valid siblings,
and a user theme may shadow a built-in id. `crates/vibex-ui/theme/example-theme-file.json`
is the copy-pasteable reference and is kept compiling by a test.

## Accessibility

- Interactive cards need keyboard focus states.
- Collapsible cards need accessible expanded/collapsed state.
- Dialogs and command palettes must trap focus.
- Controlled dialogs whose trigger is rendered outside their Radix `Dialog` root must
  capture the opening element and restore it after Escape, Cancel, or close when that
  element is still connected. Do not prevent Radix close autofocus when the original
  trigger was removed by the completed action.
- Workbench dialogs that can open at narrow desktop widths should keep the
  shadcn/Radix `DialogContent` centered unless intentionally anchored, size
  against viewport margins such as `calc(100vw - 2rem)` and
  `calc(100vh - 2rem)`, and keep the outer content non-scrolling when possible
  so the built-in close button remains reachable.
- Long dialog forms should split their layout into a bounded field scroll area
  and a `DialogFooter` with `shrink-0`, so submit/cancel actions do not float in
  the middle of form content when the available height is narrow.
- Dialogs that use a `flex` column shell with `flex-1` body content and
  `overflow-hidden` must set an explicit viewport-bounded height, not only
  `max-height`; otherwise a transform-free `h-fit` dialog shell can collapse the
  body region and leave only the header visible.
- Approval buttons need labels that describe the action, not only icons.
- Terminal views need copy/select behavior that works without pointer-only
  interactions.

## Anti-Patterns

- Do not render raw JSON provider events as user-facing UI.
- Do not create separate Claude and Codex timeline component trees.
- Do not put destructive actions in icon-only buttons.
- Do not make mobile controls depend on hover.

//! The touch selection toolbar and its drag handles.
//!
//! Android's own text selection UI is a floating bar over the selection with a
//! drag handle at each end of it. The kit's context menu is neither: it is a
//! vertical list that opens below the touch point and takes focus, so it cannot
//! sit beside the text it acts on, and nothing in the input draws or moves a
//! handle. Both are drawn here instead, over the composer, out of the geometry
//! the input already reports for the IME.
//!
//! The bar deliberately does not take focus. The caret staying in the input is
//! what keeps the soft keyboard up, and the keyboard is what holds the layout
//! the bar is positioned against.

use gpui::prelude::FluentBuilder as _;
use gpui::{
    AnyElement, App, Bounds, BoxShadow, Context, Entity, EntityInputHandler as _, Focusable as _,
    InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent, MouseMoveEvent,
    ParentElement as _, Pixels, Point, Render, ScrollWheelEvent, Size,
    StatefulInteractiveElement as _, Styled as _, Subscription, TouchPhase, Window, anchored, div,
    point, px, size,
};
use gpui_component::input::{
    Copy as CopyAction, Cut as CutAction, Paste as PasteAction, RopeExt as _,
    SelectAll as SelectAllAction, TextareaState,
};

use crate::locale;
use crate::theme;

/// Width of one toolbar item. The bar's width follows from the item count, so
/// everything drawn against it can be placed without measuring text.
const ITEM_WIDTH: f32 = 72.0;
const ITEM_FONT_SIZE: f32 = 15.0;
const BAR_HEIGHT: f32 = 44.0;
const BAR_RADIUS: f32 = 8.0;
const DIVIDER_WIDTH: f32 = 1.0;
const DIVIDER_HEIGHT: f32 = 20.0;
/// Distance between the selection and the bar above it.
const BAR_GAP: f32 = 12.0;
const HANDLE_SIZE: f32 = 18.0;
/// How far a handle hangs below the selected line.
const HANDLE_OVERHANG: f32 = 3.0;
/// Touch area around a handle. A fingertip is wider than the drawn shape, so
/// accepting only the shape would make the handle feel broken.
const HANDLE_TOUCH_SIZE: f32 = 48.0;
/// Keeps the bar and the handles off the window edges.
const SCREEN_MARGIN: f32 = 8.0;

/// Which end of the selection a drag is moving.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Handle {
    Start,
    End,
}

impl Handle {
    const ALL: [Handle; 2] = [Handle::Start, Handle::End];
}

/// The actions the bar offers, in the order it shows them.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Command {
    SelectAll,
    Cut,
    Copy,
    Paste,
}

impl Command {
    const ALL: [Command; 4] = [
        Command::SelectAll,
        Command::Cut,
        Command::Copy,
        Command::Paste,
    ];

    fn label(self) -> &'static str {
        match self {
            Command::SelectAll => locale::common("Select All"),
            Command::Cut => locale::common("Cut"),
            Command::Copy => locale::common("Copy"),
            Command::Paste => locale::common("Paste"),
        }
    }

    fn action(self) -> Box<dyn gpui::Action> {
        match self {
            Command::SelectAll => Box::new(SelectAllAction),
            Command::Cut => Box::new(CutAction),
            Command::Copy => Box::new(CopyAction),
            Command::Paste => Box::new(PasteAction),
        }
    }

    fn is_available(self, editable: bool, copyable: bool, has_clipboard: bool) -> bool {
        match self {
            Command::SelectAll => editable,
            Command::Cut => editable && copyable,
            Command::Copy => copyable,
            Command::Paste => editable && has_clipboard,
        }
    }
}

/// Where the bar and the handles sit for one frame's selection.
#[derive(Clone, Copy)]
struct Layout {
    bar: Bounds<Pixels>,
    /// Touch area and drawn corner for each end, in [`Handle::ALL`] order.
    handles: [(Bounds<Pixels>, Point<Pixels>); 2],
}

impl Layout {
    fn new(viewport: Size<Pixels>, selection: Bounds<Pixels>) -> Self {
        let bar_width = Command::ALL.len() as f32 * ITEM_WIDTH
            + (Command::ALL.len() - 1) as f32 * DIVIDER_WIDTH;
        let max_x = (f32::from(viewport.width) - bar_width - SCREEN_MARGIN).max(SCREEN_MARGIN);
        // The bar sits above the selected line, unless the selection is on the
        // first line and there is no room left for it there.
        let mut bar_y = f32::from(selection.top()) - BAR_HEIGHT - BAR_GAP;
        if bar_y < SCREEN_MARGIN {
            bar_y = f32::from(selection.bottom()) + HANDLE_TOUCH_SIZE;
        }
        let bar = Bounds::new(
            point(
                px((f32::from(selection.left()) - ITEM_WIDTH).clamp(SCREEN_MARGIN, max_x)),
                px(bar_y),
            ),
            size(px(bar_width), px(BAR_HEIGHT)),
        );

        let touch_half = HANDLE_TOUCH_SIZE / 2.0;
        let center_y = f32::from(selection.bottom()) + HANDLE_OVERHANG + HANDLE_SIZE / 2.0;
        let centers = [f32::from(selection.left()), f32::from(selection.right())];
        // Overlapping touch areas would leave a short selection with only one
        // reachable end, so they meet at the midpoint instead.
        let (start_right, end_left) = if centers[1] - centers[0] < HANDLE_TOUCH_SIZE {
            let midpoint = (centers[0] + centers[1]) / 2.0;
            (midpoint, midpoint)
        } else {
            (centers[0] + touch_half, centers[1] - touch_half)
        };
        let boxes = [
            Bounds::new(
                point(px(centers[0] - touch_half), px(center_y - touch_half)),
                size(
                    px(start_right - (centers[0] - touch_half)),
                    px(HANDLE_TOUCH_SIZE),
                ),
            ),
            Bounds::new(
                point(px(end_left), px(center_y - touch_half)),
                size(
                    px(centers[1] + touch_half - end_left),
                    px(HANDLE_TOUCH_SIZE),
                ),
            ),
        ];
        Self {
            bar,
            handles: [
                (boxes[0], Self::visual(&boxes[0], centers[0], center_y)),
                (boxes[1], Self::visual(&boxes[1], centers[1], center_y)),
            ],
        }
    }

    /// Where the drawn handle sits inside its touch area.
    fn visual(touch_area: &Bounds<Pixels>, center_x: f32, center_y: f32) -> Point<Pixels> {
        point(
            px(center_x - HANDLE_SIZE / 2.0 - f32::from(touch_area.left())),
            px(center_y - HANDLE_SIZE / 2.0 - f32::from(touch_area.top())),
        )
    }

    fn handle(&self, handle: Handle) -> (Bounds<Pixels>, Point<Pixels>) {
        let index = match handle {
            Handle::Start => 0,
            Handle::End => 1,
        };
        (self.handles[index].0, self.handles[index].1)
    }
}

fn handle_shadow() -> Vec<BoxShadow> {
    vec![BoxShadow {
        color: gpui::hsla(0.0, 0.0, 0.0, 0.28),
        offset: point(px(0.0), px(2.0)),
        blur_radius: px(8.0),
        spread_radius: px(0.0),
        inset: false,
    }]
}

/// A handle drag in flight.
///
/// The platform reports a travelling finger as a scroll gesture, not as mouse
/// moves, so the drag is claimed from that stream and the rest of the stream
/// belongs to it. The tail matters: the same stream carries the fling the
/// gesture ends with, which would otherwise be handed to the list underneath
/// and scroll the transcript away from the text being selected.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Drag {
    /// Following the finger.
    Following(Handle),
    /// The finger is gone; the rest of the gesture is momentum to discard.
    Settling,
}

/// The toolbar is opened by a long press and lives until the selection it acts
/// on is gone.
pub struct SelectionMenu {
    target: Entity<TextareaState>,
    active: bool,
    drag: Option<Drag>,
    /// The geometry the touch targets were drawn at, for hit-testing the
    /// scroll stream that drives a drag.
    layout: Option<Layout>,
    /// Kept alive for as long as the menu is; dropping it would stop the
    /// handles from following a selection that changes elsewhere.
    _target_subscription: Subscription,
}

impl SelectionMenu {
    pub fn new(target: Entity<TextareaState>, cx: &mut Context<Self>) -> Self {
        // The selection can change without the toolbar being touched, through
        // the keyboard or the input's own editing actions, and the handles
        // have to follow it.
        let target_subscription = cx.observe(&target, |_, _, cx| cx.notify());
        Self {
            target,
            active: false,
            drag: None,
            layout: None,
            _target_subscription: target_subscription,
        }
    }

    /// Show the toolbar for the current selection. Called from the input's
    /// context menu hook, which is where a long press lands.
    pub fn open(&mut self, cx: &mut Context<Self>) {
        if self.active {
            return;
        }
        self.active = true;
        self.drag = None;
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.active {
            self.active = false;
            self.drag = None;
            cx.notify();
        }
    }

    /// The box the text occupies, in window coordinates.
    fn text_bounds(&self, cx: &App) -> Option<Bounds<Pixels>> {
        let state = self.target.read(cx);
        state.range_to_bounds(&(0..state.text().len()))
    }

    /// Where the caret sits for `offset`, in window coordinates.
    fn caret_x(&self, offset: usize, cx: &App) -> Option<Pixels> {
        self.target
            .read(cx)
            .range_to_bounds(&(offset..offset))
            .map(|bounds| bounds.left())
    }

    /// The offset a finger at `position` is pointing at.
    ///
    /// The input resolves a point to the character it falls inside, which is
    /// not quite what a handle needs: a finger past the middle of a character
    /// belongs on that character's far side, and a handle dragged clear of the
    /// text belongs at its end. Both cases are handled here, and the point is
    /// pulled back onto the text first, because the handles hang below the
    /// line and the input answers only for points inside its own box.
    fn offset_for_point(
        &self,
        position: Point<Pixels>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Option<usize> {
        let text = self.text_bounds(cx)?;
        let length = self.target.read(cx).text().len();
        if position.x > text.right() {
            return Some(length);
        }
        let inside = point(
            position
                .x
                .clamp(text.left(), (text.right() - px(1.0)).max(text.left())),
            position
                .y
                .clamp(text.top(), (text.bottom() - px(1.0)).max(text.top())),
        );
        let utf16 = self.target.update(cx, |state, cx| {
            state.character_index_for_point(inside, window, cx)
        })?;
        let offset = self.target.read(cx).text().offset_utf16_to_offset(utf16);
        let next = {
            let state = self.target.read(cx);
            let text = state.text();
            text.char_index_to_offset(text.offset_to_char_index(offset) + 1)
        };
        match (self.caret_x(offset, cx), self.caret_x(next, cx)) {
            (Some(here), Some(there)) if position.x > (here + there) / 2.0 => Some(next),
            _ => Some(offset),
        }
    }

    /// Move the dragged end of the selection to `position`.
    fn drag_to(&mut self, position: Point<Pixels>, window: &mut Window, cx: &mut Context<Self>) {
        let Some(Drag::Following(handle)) = self.drag else {
            return;
        };
        let Some(offset) = self.offset_for_point(position, window, cx) else {
            return;
        };
        let current = self.target.read(cx).selected_range();
        let range = match handle {
            // Dragging one end past the other collapses the selection rather
            // than swapping the handles. Material swaps them; a finger does
            // not report its intent, and swapping mid-drag would move the end
            // the user is holding out from under it.
            Handle::Start => offset.min(current.end)..current.end,
            Handle::End => current.start..offset.max(current.start),
        };
        if range != current {
            self.target
                .update(cx, |state, cx| state.set_selected_range(range, cx));
            cx.notify();
        }
    }

    /// Take a touch drag out of the scroll stream, or let it scroll.
    fn on_touch_scroll(
        &mut self,
        event: &ScrollWheelEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A new gesture replaces whatever the last one left behind: the drag
        // is claimed from where the finger landed, and a gesture that starts
        // anywhere else is a scroll or a caret tap, which ends the toolbar.
        if event.touch_phase == TouchPhase::Started {
            self.drag = None;
            let handle = self.layout.and_then(|layout| {
                Handle::ALL
                    .into_iter()
                    .find(|handle| layout.handle(*handle).0.contains(&event.position))
            });
            if let Some(handle) = handle {
                self.drag = Some(Drag::Following(handle));
                cx.stop_propagation();
            } else if self
                .layout
                .is_some_and(|layout| layout.bar.contains(&event.position))
            {
                cx.stop_propagation();
            } else {
                self.close(cx);
            }
            return;
        }
        match self.drag {
            Some(Drag::Following(_)) => {
                cx.stop_propagation();
                if event.touch_phase == TouchPhase::Ended {
                    self.drag = Some(Drag::Settling);
                    cx.notify();
                } else {
                    self.drag_to(event.position, window, cx);
                }
            }
            // The tail of a finished drag is the fling the gesture ends with.
            // It is discarded rather than handed to the list under the toolbar.
            Some(Drag::Settling) => cx.stop_propagation(),
            None => {}
        }
    }

    fn render_handle(&self, handle: Handle, layout: &Layout, cx: &mut Context<Self>) -> AnyElement {
        let (touch_area, visual) = layout.handle(handle);
        anchored()
            .position(touch_area.origin)
            .child(
                div()
                    .relative()
                    .w(touch_area.size.width)
                    .h(touch_area.size.height)
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(move |this, _: &MouseDownEvent, _, cx| {
                            // Only the mouse path reaches here, and a mouse
                            // drag is not claimed until it is known to be one:
                            // a tap on a handle must not move the selection.
                            if this.drag.is_none() {
                                this.drag = Some(Drag::Following(handle));
                                cx.notify();
                            }
                            cx.stop_propagation();
                        }),
                    )
                    .child(
                        div()
                            .absolute()
                            .left(visual.x)
                            .top(visual.y)
                            .size(px(HANDLE_SIZE))
                            .rounded_full()
                            .bg(theme::accent_green())
                            .shadow(handle_shadow()),
                    ),
            )
            .into_any_element()
    }
}

impl Render for SelectionMenu {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.active {
            return div().into_any_element();
        }

        let (range, bounds, editable, copyable, focused) = {
            let state = self.target.read(cx);
            let capabilities = state.context_menu_capabilities();
            (
                state.selected_range(),
                state.range_to_bounds(&state.selected_range()),
                capabilities.is_editable(),
                capabilities.is_copyable(),
                state.focus_handle(cx).is_focused(window),
            )
        };
        // An empty selection means the gesture that opened the toolbar is over:
        // the caret moved, the text was replaced, or the message was sent.
        if range.is_empty() || !focused {
            self.active = false;
            self.drag = None;
            self.layout = None;
            return div().into_any_element();
        }
        // The line is not laid out yet, or has scrolled out of the input.
        let Some(selection) = bounds else {
            return div().into_any_element();
        };

        let has_clipboard = cx.read_from_clipboard().is_some();
        let layout = Layout::new(window.viewport_size(), selection);
        self.layout = Some(layout);
        let bar_width = layout.bar.size.width;

        let items = Command::ALL.into_iter().enumerate().fold(
            div().flex().items_center().h_full(),
            |bar, (index, command)| {
                let available = command.is_available(editable, copyable, has_clipboard);
                bar.when(index > 0, |bar| {
                    bar.child(
                        div()
                            .w(px(DIVIDER_WIDTH))
                            .h(px(DIVIDER_HEIGHT))
                            .bg(theme::border_subtle()),
                    )
                })
                .child(
                    div()
                        .id(("selection-command", index))
                        .w(px(ITEM_WIDTH))
                        .h_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .text_size(px(ITEM_FONT_SIZE))
                        .text_color(if available {
                            theme::text_primary()
                        } else {
                            theme::text_muted()
                        })
                        .when(available, |item| {
                            item.cursor_pointer()
                                .active(|item| item.bg(theme::row_pressed_bg()))
                                .on_mouse_down(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, window, cx| {
                                        cx.stop_propagation();
                                        window.dispatch_action(command.action(), cx);
                                        if command == Command::SelectAll {
                                            // The selection grew under the
                                            // handles, which the input has
                                            // already notified.
                                            cx.notify();
                                        } else {
                                            this.close(cx);
                                        }
                                    }),
                                )
                        })
                        .child(command.label()),
                )
            },
        );

        div()
            .absolute()
            .inset_0()
            .child(
                // Catches the tap that dismisses the toolbar. It deliberately
                // lets the event through, so the same tap still places the
                // caret, and the collapsed selection closes the menu anyway.
                div()
                    .absolute()
                    .inset_0()
                    .on_mouse_down(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| this.close(cx)),
                    )
                    .on_mouse_move(cx.listener(|this, event: &MouseMoveEvent, window, cx| {
                        this.drag_to(event.position, window, cx)
                    }))
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(|this, _, _, cx| {
                            if matches!(this.drag, Some(Drag::Following(_))) {
                                this.drag = None;
                                cx.notify();
                            }
                        }),
                    )
                    .on_scroll_wheel(cx.listener(|this, event: &ScrollWheelEvent, window, cx| {
                        this.on_touch_scroll(event, window, cx)
                    })),
            )
            .child(self.render_handle(Handle::Start, &layout, cx))
            .child(self.render_handle(Handle::End, &layout, cx))
            .child(
                anchored().position(layout.bar.origin).child(
                    div()
                        .w(bar_width)
                        .h(px(BAR_HEIGHT))
                        .rounded(px(BAR_RADIUS))
                        .bg(theme::bg_popover())
                        .border_1()
                        .border_color(theme::border_default())
                        .shadow(handle_shadow())
                        .overflow_hidden()
                        // Taps on the bar's own padding are not taps outside it.
                        .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                        .child(items),
                ),
            )
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, MouseUpEvent, ScrollDelta, TestAppContext, VisualTestContext};
    use gpui_component::input::Textarea;

    /// A phone-sized viewport, in logical pixels.
    fn viewport() -> Size<Pixels> {
        size(px(390.0), px(844.0))
    }

    fn selection(left: f32, top: f32, width: f32) -> Bounds<Pixels> {
        Bounds::new(point(px(left), px(top)), size(px(width), px(20.0)))
    }

    #[test]
    fn bar_sits_above_the_selected_line() {
        let layout = Layout::new(viewport(), selection(40.0, 700.0, 60.0));

        assert_eq!(layout.bar.bottom(), px(700.0) - px(BAR_GAP));
        assert_eq!(layout.bar.size.height, px(BAR_HEIGHT));
    }

    #[test]
    fn bar_drops_below_a_selection_with_no_room_above_it() {
        let layout = Layout::new(viewport(), selection(40.0, 20.0, 60.0));

        assert!(layout.bar.top() >= px(SCREEN_MARGIN));
        assert!(layout.bar.top() > px(40.0), "the bar should clear the line");
    }

    #[test]
    fn bar_stays_inside_the_viewport() {
        let narrow = Layout::new(viewport(), selection(380.0, 700.0, 10.0));
        assert!(narrow.bar.right() <= px(390.0) - px(SCREEN_MARGIN));

        let wide = Layout::new(viewport(), selection(0.0, 700.0, 10.0));
        assert!(wide.bar.left() >= px(SCREEN_MARGIN));
    }

    #[test]
    fn handles_hang_below_the_selection_at_its_ends() {
        let selected = selection(40.0, 700.0, 60.0);
        let layout = Layout::new(viewport(), selected);

        for (handle, center) in [(Handle::Start, 40.0), (Handle::End, 100.0)] {
            let (touch_area, visual) = layout.handle(handle);
            let visual_x = f32::from(touch_area.left()) + f32::from(visual.x);
            let visual_y = f32::from(touch_area.top()) + f32::from(visual.y);
            assert_eq!(visual_x + HANDLE_SIZE / 2.0, center);
            assert_eq!(visual_y, f32::from(selected.bottom()) + HANDLE_OVERHANG);
            // The touch area is centred on the drawn handle, so it reaches
            // above the line as well as below it.
            assert!(touch_area.contains(&point(px(center), selected.bottom())));
        }
    }

    /// A word is narrower than two fingertips, so the touch areas meet in the
    /// middle instead of overlapping: one target per end, both reachable.
    #[test]
    fn short_selections_still_have_two_reachable_handles() {
        let layout = Layout::new(viewport(), selection(40.0, 700.0, 12.0));
        let (start, _) = layout.handle(Handle::Start);
        let (end, _) = layout.handle(Handle::End);

        assert!(!start.intersects(&end));
        assert_eq!(start.right(), end.left());
        assert!(start.size.width > px(0.0) && end.size.width > px(0.0));
    }

    #[test]
    fn commands_act_only_on_what_the_selection_can_do() {
        assert!(Command::Copy.is_available(false, true, false));
        assert!(!Command::Copy.is_available(false, false, true));
        assert!(!Command::Cut.is_available(false, true, false));
        assert!(Command::Cut.is_available(true, true, false));
        assert!(!Command::Paste.is_available(true, false, false));
        assert!(Command::Paste.is_available(true, false, true));
        assert!(!Command::SelectAll.is_available(false, false, true));
    }

    /// The app's composer wiring: the input, with the toolbar drawn over it.
    struct Probe {
        input: Entity<TextareaState>,
        menu: Entity<SelectionMenu>,
    }

    impl Probe {
        const TEXT: &'static str = "hello world";

        fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
            let input = cx.new(|cx| TextareaState::new(window, cx).default_value(Self::TEXT));
            let menu = cx.new(|cx| SelectionMenu::new(input.clone(), cx));
            input.update(cx, |state, cx| state.focus(window, cx));
            Self { input, menu }
        }

        fn is_active(&self, cx: &App) -> bool {
            self.menu.read(cx).active
        }

        fn selection(&self, cx: &App) -> std::ops::Range<usize> {
            self.input.read(cx).selected_range()
        }

        /// Where a caret at `offset` is drawn, in window coordinates.
        fn caret(&self, offset: usize, cx: &App) -> Point<Pixels> {
            self.input
                .read(cx)
                .range_to_bounds(&(offset..offset))
                .expect("the text should be laid out")
                .origin
        }

        /// The point a finger has to land on to grab a handle.
        fn handle(&self, handle: Handle, cx: &App) -> Point<Pixels> {
            let layout = self
                .menu
                .read(cx)
                .layout
                .expect("the toolbar should be laid out");
            let (touch_area, visual) = layout.handle(handle);
            point(
                touch_area.left() + visual.x + px(HANDLE_SIZE / 2.0),
                touch_area.top() + visual.y + px(HANDLE_SIZE / 2.0),
            )
        }
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .child(Textarea::new(&self.input).appearance(false))
                .child(self.menu.clone())
        }
    }

    fn select(probe: &Entity<Probe>, range: std::ops::Range<usize>, cx: &mut VisualTestContext) {
        let input = probe.read_with(cx, |probe, _| probe.input.clone());
        input.update(cx, |state, cx| state.set_selected_range(range, cx));
    }

    fn open_toolbar(probe: &Entity<Probe>, cx: &mut VisualTestContext) {
        let menu = probe.read_with(cx, |probe, _| probe.menu.clone());
        menu.update(cx, |menu, cx| menu.open(cx));
    }

    fn draw(cx: &mut VisualTestContext) {
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
    }

    /// A composer with `range` selected and the toolbar open over it.
    fn opened(
        cx: &mut TestAppContext,
        range: std::ops::Range<usize>,
    ) -> (Entity<Probe>, &mut VisualTestContext) {
        cx.update(gpui_component::init);
        let (probe, cx) = cx.add_window_view(Probe::new);
        select(&probe, range, cx);
        draw(cx);
        open_toolbar(&probe, cx);
        draw(cx);
        (probe, cx)
    }

    /// The stream the platform reports for a finger that travels from one point
    /// to another: a scroll gesture, not mouse moves.
    fn drag(cx: &mut VisualTestContext, from: Point<Pixels>, to: Point<Pixels>) {
        cx.simulate_event(ScrollWheelEvent {
            position: from,
            delta: ScrollDelta::Pixels(point(px(0.0), px(0.0))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Started,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: to,
            delta: ScrollDelta::Pixels(to - from),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Moved,
        });
        cx.simulate_event(ScrollWheelEvent {
            position: to,
            delta: ScrollDelta::Pixels(point(px(0.0), px(0.0))),
            modifiers: Default::default(),
            touch_phase: TouchPhase::Ended,
        });
    }

    /// A finger that lands on a handle and travels has to claim the drag from
    /// the scroll stream, and the selection has to follow it.
    #[gpui::test]
    fn dragging_the_end_handle_extends_the_selection(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 0..5);
        let (handle, target) = probe.read_with(cx, |probe, cx| {
            (probe.handle(Handle::End, cx), probe.caret(11, cx))
        });

        drag(cx, handle, target);

        assert_eq!(probe.read_with(cx, |probe, cx| probe.selection(cx)), 0..11);
    }

    /// The handle is routinely dragged clear of the text, where the input
    /// resolves no character at all.
    #[gpui::test]
    fn dragging_past_the_end_selects_to_the_end_of_the_text(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 0..5);
        let (handle, beyond) = probe.read_with(cx, |probe, cx| {
            (
                probe.handle(Handle::End, cx),
                probe.caret(11, cx).x + px(30.0),
            )
        });

        let target = point(beyond, handle.y);
        drag(cx, handle, target);

        assert_eq!(probe.read_with(cx, |probe, cx| probe.selection(cx)), 0..11);
    }

    #[gpui::test]
    fn dragging_the_start_handle_back_selects_from_the_start(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 6..11);
        let (handle, before) = probe.read_with(cx, |probe, cx| {
            (probe.handle(Handle::Start, cx), probe.caret(0, cx).x)
        });

        let target = point(before, handle.y);
        drag(cx, handle, target);

        assert_eq!(probe.read_with(cx, |probe, cx| probe.selection(cx)), 0..11);
    }

    /// The caret has to stay in the input: it is what keeps the soft keyboard
    /// up, and the keyboard holds the layout the toolbar is drawn against.
    #[gpui::test]
    fn the_toolbar_never_takes_focus_from_the_input(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 0..5);
        let (handle, target) = probe.read_with(cx, |probe, cx| {
            (probe.handle(Handle::End, cx), probe.caret(11, cx))
        });

        drag(cx, handle, target);

        cx.update(|window, cx| {
            assert!(
                probe
                    .read(cx)
                    .input
                    .read(cx)
                    .focus_handle(cx)
                    .is_focused(window),
                "the composer should still hold the caret"
            );
        });
    }

    /// The bar's items act on the input the caret is in, and one of them
    /// changes the selection out from under the handles.
    #[gpui::test]
    fn tapping_select_all_selects_the_whole_message(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 0..5);
        let item = probe.read_with(cx, |probe, cx| {
            let bar = probe
                .menu
                .read(cx)
                .layout
                .expect("the toolbar should be laid out")
                .bar;
            point(
                bar.left() + px(ITEM_WIDTH / 2.0),
                bar.top() + px(BAR_HEIGHT / 2.0),
            )
        });

        cx.simulate_event(MouseDownEvent {
            button: MouseButton::Left,
            position: item,
            modifiers: Default::default(),
            click_count: 1,
            first_mouse: false,
        });
        cx.simulate_event(MouseUpEvent {
            button: MouseButton::Left,
            position: item,
            modifiers: Default::default(),
            click_count: 1,
        });
        draw(cx);

        assert_eq!(
            probe.read_with(cx, |probe, cx| probe.selection(cx)),
            0..Probe::TEXT.len()
        );
        assert!(
            probe.read_with(cx, |probe, cx| probe.is_active(cx)),
            "the toolbar should stay for the selection it just made"
        );
    }

    /// A touch that starts anywhere but the toolbar is the user leaving it.
    #[gpui::test]
    fn a_touch_outside_the_toolbar_dismisses_it(cx: &mut TestAppContext) {
        let (probe, cx) = opened(cx, 0..5);
        let away = probe.read_with(cx, |probe, cx| {
            probe.caret(2, cx) + point(px(0.0), px(120.0))
        });

        drag(cx, away, away);

        assert!(
            !probe.read_with(cx, |probe, cx| probe.is_active(cx)),
            "the toolbar should be gone"
        );
        assert_eq!(probe.read_with(cx, |probe, cx| probe.selection(cx)), 0..5);
    }
}

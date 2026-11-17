//! Keeps an application drag from leaving a text selection behind.
//!
//! GPUI's window-level text selection starts a gesture from the same left
//! press that starts a drag, because the selection layer anchors at any press
//! that does not explicitly ask it not to. The two gestures then overlap: a
//! drop target consumes the release — most handlers call `stop_propagation`,
//! and a drag whose payload a target accepts takes the active drag with it —
//! so the selection layer never sees the mouse-up that would end its gesture.
//! It stays armed with the press as its anchor, and the first pointer move
//! after the release resolves that anchor against whatever text is under the
//! cursor, selecting a stretch of the conversation, the editor, or the next
//! pane.
//!
//! Suppressing the selection on each drag source's press works, and the
//! surfaces that own a press do it. It does not scale: every `on_drag` in the
//! product would have to repeat it, and a new one forgetting to is a silent
//! selection bug. This layer sits above every window root instead. It watches
//! the pointer for an active drag and, exactly once per gesture, clears the
//! window selection before the drop handlers and the selection layer can
//! disagree about how the press ended. Text selection itself never sets an
//! active drag, so dragging inside an input is untouched.
//!
//! A release that never reaches the window — the pointer came up outside it —
//! is covered too: once a gesture is known to have dragged, the first move
//! that sees no active drag clears the anchor the lost release left behind.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, DispatchPhase, Element, ElementId, GlobalElementId, InspectorElementId, IntoElement,
    LayoutId, MouseDownEvent, MouseMoveEvent, MouseUpEvent, Render, Style, Window,
};
use gpui_base::{Root, RootPlugin, TextSelection};

/// Registers the guard for every window root created afterwards.
///
/// Must run during application initialization, before the first window opens.
pub fn init(cx: &mut App) {
    Root::register_plugin::<SelectionGuard>(cx, SelectionGuard::new);
}

/// The root plugin that owns the guard layer.
pub struct SelectionGuard {
    gesture: Rc<GestureGuard>,
}

impl SelectionGuard {
    fn new(_window: &mut Window, _cx: &mut gpui::Context<Self>) -> Self {
        Self {
            gesture: Rc::new(GestureGuard::default()),
        }
    }
}

/// What the guard knows about the gesture that is in progress.
///
/// A clear is only ever worth doing once per press — after it the selection
/// layer is disarmed and later moves have nothing left to extend — so the flag
/// is per gesture rather than per frame, and the press resets it.
#[derive(Default)]
struct GestureGuard {
    cleared: Cell<bool>,
}

impl GestureGuard {
    fn reset(&self) {
        self.cleared.set(false);
    }

    /// Drops this gesture's selection, unless it has been dropped already.
    fn clear(&self, window: &mut Window, cx: &mut App) {
        if self.cleared.replace(true) {
            return;
        }
        TextSelection::clear(window, cx);
    }
}

impl Render for SelectionGuard {
    fn render(&mut self, _window: &mut Window, _cx: &mut gpui::Context<Self>) -> impl IntoElement {
        SelectionGuardLayer {
            gesture: self.gesture.clone(),
        }
    }
}

impl RootPlugin for SelectionGuard {}

/// A transparent element whose only job is to register the window listeners.
///
/// It has to be an element rather than a plugin hook: `RootPlugin::prepare`
/// runs while the frame is still being built, and `Window::on_mouse_event` is
/// only valid during paint. Painting after the application content also puts
/// these listeners at the front of the bubble phase, so they run before a
/// drop target can consume the release.
struct SelectionGuardLayer {
    gesture: Rc<GestureGuard>,
}

impl Element for SelectionGuardLayer {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some("selection-guard".into())
    }

    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: gpui::Bounds<gpui::Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _cx: &mut App,
    ) {
        // Capture, so the flags are fresh before any handler of the press runs.
        window.on_mouse_event({
            let gesture = self.gesture.clone();
            move |_: &MouseDownEvent, phase, _, _| {
                if phase == DispatchPhase::Capture {
                    gesture.reset();
                }
            }
        });

        // A drag usually becomes active *during* the move that arms it, after
        // this listener has already run, so the move that arms it is not the
        // one that clears. The next move is: the selection layer ignores the
        // pointer for as long as the drag is live, so that is the last chance
        // to drop the anchor before the release.
        window.on_mouse_event({
            let gesture = self.gesture.clone();
            move |_: &MouseMoveEvent, phase, window, cx| {
                if phase != DispatchPhase::Bubble {
                    return;
                }
                if cx.has_active_drag() {
                    gesture.clear(window, cx);
                }
            }
        });

        // A drag can be released without another move after it armed, so the
        // release has to clear as well — and in the capture phase, where a
        // drop target's `stop_propagation` cannot hide it. GPUI still reports
        // the drag as active here; it takes the drag down after the listeners.
        window.on_mouse_event({
            let gesture = self.gesture.clone();
            move |_: &MouseUpEvent, phase, window, cx| {
                if phase == DispatchPhase::Capture && cx.has_active_drag() {
                    gesture.clear(window, cx);
                }
            }
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{
        AppContext as _, Context, InteractiveElement as _, Modifiers, MouseButton,
        ParentElement as _, Render, StatefulInteractiveElement as _, Styled as _, TestAppContext,
        div, point, px,
    };
    use gpui_base::{SelectableText, TextSelectionLayer};

    struct DraggedChip;

    impl Render for DraggedChip {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().size(px(12.0))
        }
    }

    struct DragSourceProbe;

    impl Render for DragSourceProbe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
                .size_full()
                .flex()
                .flex_col()
                .child(TextSelectionLayer)
                .child(
                    div()
                        .id("selection-guard-drag-source")
                        .debug_selector(|| "selection-guard-drag-source".into())
                        .w(px(160.0))
                        .h(px(24.0))
                        .on_drag(DraggedChip, |_, _, _, cx| cx.new(|_| DraggedChip)),
                )
                .child(
                    div()
                        .id("selection-guard-text")
                        .debug_selector(|| "selection-guard-text".into())
                        .w(px(320.0))
                        .h(px(48.0))
                        .child(SelectableText::new(
                            "selection-guard-body",
                            "alpha beta gamma delta epsilon zeta eta theta",
                        ))
                        // A drop target that consumes the release, like every
                        // real one does. Without the guard this is what leaves
                        // the selection layer armed.
                        .on_drop(|_: &DraggedChip, _, cx| cx.stop_propagation()),
                )
        }
    }

    /// A drag that leaves the window's selection layer armed must not select
    /// text on the moves that follow the release.
    ///
    /// The press starts both gestures. The drop target swallows the mouse-up,
    /// so the selection layer's own `end` never runs, and the next move extends
    /// the anchor the press left behind across the text under the cursor. This
    /// is the sidebar-row, group-tab and split-divider bug in one gesture.
    #[gpui::test]
    fn a_drag_does_not_select_text_on_the_moves_after_its_release(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let probe = cx.new(|_| DragSourceProbe);
            Root::new(probe, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let source = cx
            .debug_bounds("selection-guard-drag-source")
            .expect("the drag source is laid out")
            .center();
        let text = cx
            .debug_bounds("selection-guard-text")
            .expect("the text is laid out");
        let over_text = point(text.origin.x + px(240.0), text.center().y);

        // Press on the drag source — blank, not text — then move far enough to
        // arm the drag and carry it over the body.
        cx.simulate_mouse_down(source, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(
            point(source.x + px(20.0), source.y),
            Some(MouseButton::Left),
            Modifiers::none(),
        );
        cx.simulate_mouse_move(over_text, Some(MouseButton::Left), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        // The drop target consumes this release.
        cx.simulate_mouse_up(over_text, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        // The moves after the release are what used to extend the selection.
        cx.simulate_mouse_move(
            point(over_text.x + px(60.0), over_text.y),
            None,
            Modifiers::none(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let selected = cx.update(TextSelection::selected_text);
        assert_eq!(
            selected, "",
            "a drag must not leave the window selection armed"
        );
    }

    /// A drag released on the very move that arms it never gets the guard's
    /// usual "next move" clear, so the release itself has to do it.
    #[gpui::test]
    fn a_drag_released_right_after_it_arms_still_clears(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let probe = cx.new(|_| DragSourceProbe);
            Root::new(probe, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let source = cx
            .debug_bounds("selection-guard-drag-source")
            .expect("the drag source is laid out")
            .center();
        let text = cx
            .debug_bounds("selection-guard-text")
            .expect("the text is laid out");
        let over_text = point(text.origin.x + px(240.0), text.center().y);

        // One move arms the drag; the release follows immediately, with no
        // move in between for the usual path to clear on.
        cx.simulate_mouse_down(source, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(over_text, Some(MouseButton::Left), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_up(over_text, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        cx.simulate_mouse_move(
            point(over_text.x + px(60.0), over_text.y),
            None,
            Modifiers::none(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let selected = cx.update(TextSelection::selected_text);
        assert_eq!(
            selected, "",
            "a fast drag must not leave the window selection armed"
        );
    }

    /// The guard watches for a drag, so an ordinary pointer selection must be
    /// untouched: no active drag means nothing clears it mid-gesture.
    #[gpui::test]
    fn a_pointer_selection_over_text_still_selects_it(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        cx.update(init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let probe = cx.new(|_| DragSourceProbe);
            Root::new(probe, window, cx)
        });
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let text = cx
            .debug_bounds("selection-guard-text")
            .expect("the text is laid out");
        let start = point(text.origin.x + px(4.0), text.center().y);
        let end = point(text.origin.x + px(240.0), text.center().y);

        cx.simulate_mouse_down(start, MouseButton::Left, Modifiers::none());
        cx.simulate_mouse_move(end, Some(MouseButton::Left), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_up(end, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let selected = cx.update(TextSelection::selected_text);
        assert!(
            selected.starts_with("alpha") && !selected.is_empty(),
            "a pointer drag over text must still select it, got {selected:?}"
        );
    }
}

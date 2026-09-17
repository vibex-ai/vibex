//! A container that observes scroll-wheel gestures before its children do.
//!
//! `gpui-pre` only exposes scroll-wheel listeners in the bubble phase, where a
//! nested scroll container has already consumed the gesture. The workspace
//! drawer has to see a touch pan first so an edge swipe can claim it, so this
//! element installs a capture-phase listener from its paint pass — the pattern
//! `gpui-kit` uses for its own scroll mask — and wraps the page container.

use std::rc::Rc;

use gpui::{
    AnyElement, App, Bounds, DispatchPhase, Element, ElementId, GlobalElementId, Hitbox,
    HitboxBehavior, InspectorElementId, IntoElement, LayoutId, ScrollWheelEvent, Window,
};

type CaptureScrollWheelListener = Rc<dyn Fn(&ScrollWheelEvent, &mut Window, &mut App)>;

/// Wraps `child` and reports scroll-wheel events in the capture phase.
///
/// The event is only reported while the wrapped element is under the pointer,
/// so an occluding overlay suppresses the gesture exactly as it does for a
/// regular scroll container.
pub struct CaptureScrollWheel {
    id: ElementId,
    child: AnyElement,
    listener: CaptureScrollWheelListener,
}

/// Wraps `child` with a capture-phase scroll-wheel listener.
pub fn capture_scroll_wheel(
    id: impl Into<ElementId>,
    child: impl IntoElement,
    listener: impl Fn(&ScrollWheelEvent, &mut Window, &mut App) + 'static,
) -> CaptureScrollWheel {
    CaptureScrollWheel {
        id: id.into(),
        child: child.into_any_element(),
        listener: Rc::new(listener),
    }
}

impl Element for CaptureScrollWheel {
    type RequestLayoutState = ();
    type PrepaintState = Hitbox;

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) -> Self::PrepaintState {
        self.child.prepaint(window, cx);
        window.insert_hitbox(bounds, HitboxBehavior::Normal)
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<gpui::Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        hitbox: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let hitbox_id = hitbox.id;
        window.on_mouse_event({
            let listener = self.listener.clone();
            move |event: &ScrollWheelEvent, phase, window, cx| {
                if phase != DispatchPhase::Capture {
                    return;
                }
                if !hitbox_id.should_handle_scroll(window) {
                    return;
                }
                (listener)(event, window, cx);
            }
        });
        self.child.paint(window, cx);
    }
}

impl IntoElement for CaptureScrollWheel {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

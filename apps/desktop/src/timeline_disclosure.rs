use gpui::{
    AnyElement, App, AvailableSpace, Bounds, ContentMask, Element, ElementId, GlobalElementId,
    InspectorElementId, IntoElement, LayoutId, Pixels, Style, Window, px, relative, size,
};

/// A height reveal that leaves settled content in normal layout. History and
/// width changes therefore measure on their first frame, without a zero-height
/// placeholder. Only an active transition uses the retained intrinsic height.
pub(super) struct TimelineDisclosure {
    id: ElementId,
    progress: f32,
    child: AnyElement,
}

#[derive(Default)]
struct DisclosureState {
    height: Pixels,
}

impl TimelineDisclosure {
    pub(super) fn new(id: String, progress: f32, child: AnyElement) -> Self {
        Self {
            id: id.into(),
            progress: progress.clamp(0.0, 1.0),
            child,
        }
    }
}

impl IntoElement for TimelineDisclosure {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}

impl Element for TimelineDisclosure {
    type RequestLayoutState = Option<LayoutId>;
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some(self.id.clone())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.flex_shrink = 0.0;
        let child_layout = if self.progress == 1.0 {
            Some(self.child.request_layout(window, cx))
        } else {
            let height = window.with_element_state(
                global_id.expect("disclosure id"),
                |state: Option<DisclosureState>, _| {
                    let state = state.unwrap_or_default();
                    (state.height, state)
                },
            );
            style.size.height = (height * self.progress).into();
            None
        };
        (window.request_layout(style, child_layout, cx), child_layout)
    }

    fn prepaint(
        &mut self,
        global_id: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        child_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let height = if let Some(id) = child_layout {
            window.layout_bounds(*id).size.height
        } else {
            self.child
                .layout_as_root(
                    size(
                        AvailableSpace::Definite(bounds.size.width),
                        AvailableSpace::MinContent,
                    ),
                    window,
                    cx,
                )
                .height
        };
        let changed = window.with_element_state(
            global_id.expect("disclosure id"),
            |state: Option<DisclosureState>, _| {
                let state = state.unwrap_or_default();
                (
                    (height - state.height).abs() >= px(1.0),
                    DisclosureState { height },
                )
            },
        );
        if changed && self.progress < 1.0 {
            window.request_animation_frame();
        }
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            if child_layout.is_some() {
                self.child.prepaint(window, cx);
            } else {
                self.child.prepaint_at(bounds.origin, window, cx);
            }
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.with_content_mask(Some(ContentMask { bounds }), |window| {
            self.child.paint(window, cx)
        });
    }
}

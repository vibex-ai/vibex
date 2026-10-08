//! A compact V-shaped activity mark shared by live session surfaces.
//!
//! One clock moves a soft highlight down one stroke and up the other, then
//! back again. The turnarounds ease to rest, including at the loop boundary.
//! Cells are painted inside a fixed, rem-sized slot so motion never reflows
//! the adjacent label or status lane.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;

use crate::{motion, spinner::STATUS_INDICATOR_MAX_FPS};

const CYCLE: Duration = Duration::from_millis(2100);
const MAX_FPS: f32 = 30.0;

// Normalized cell centers follow the V from its left tip to its right tip.
// These are glyph coordinates; the enclosing slot owns the interface scale.
const CELLS: [(f32, f32); 7] = [
    (0.14, 0.20),
    (0.26, 0.40),
    (0.38, 0.60),
    (0.50, 0.80),
    (0.62, 0.60),
    (0.74, 0.40),
    (0.86, 0.20),
];

#[derive(IntoElement)]
pub(crate) struct ActivityIndicator {
    id: ElementId,
    compact: bool,
    color: Option<Hsla>,
}

impl ActivityIndicator {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            compact: false,
            color: None,
        }
    }

    /// Use the sidebar's smaller status slot and lower animation frame budget.
    pub(crate) fn compact(mut self) -> Self {
        self.compact = true;
        self
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for ActivityIndicator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let color = self.color.unwrap_or(cx.theme().foreground);
        let slot = if self.compact {
            div().size_3()
        } else {
            div().size_4()
        }
        .flex_none();

        if motion::reduced_motion(cx) || motion::pauses_while_inactive(!window.is_window_active()) {
            return slot.child(mark(color, None)).into_any_element();
        }

        slot.with_animation(
            self.id,
            Animation::new(CYCLE)
                .repeat_synced()
                .with_max_fps(if self.compact {
                    STATUS_INDICATOR_MAX_FPS
                } else {
                    MAX_FPS
                }),
            move |this, phase| this.child(mark(color, Some(phase))),
        )
        .into_any_element()
    }
}

fn mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let head = phase.map(|phase| 0.5 - 0.5 * (phase * TAU).cos());
            let cell_size = bounds.size.width.min(bounds.size.height) * 0.15;
            for (ix, (x, y)) in CELLS.into_iter().enumerate() {
                let intensity = head.map_or(1.0, |head| {
                    let position = ix as f32 / (CELLS.len() - 1) as f32;
                    let distance = ((position - head).abs() / 0.42).min(1.0);
                    0.5 + 0.5 * (distance * PI).cos()
                });
                let origin = point(
                    bounds.origin.x + bounds.size.width * x - cell_size * 0.5,
                    bounds.origin.y + bounds.size.height * y - cell_size * 0.5,
                );
                window.paint_quad(
                    fill(
                        Bounds::new(origin, size(cell_size, cell_size)),
                        color.opacity(0.28 + 0.72 * intensity),
                    )
                    .corner_radii(cell_size * 0.22),
                );
            }
        },
    )
    .size_full()
}

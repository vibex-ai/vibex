//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar passes
//! light between four dots in a diamond. Both use a synchronized, bounded
//! clock and a fixed, rem-sized canvas so motion never reflows adjacent text.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;

use crate::{motion, spinner::STATUS_INDICATOR_MAX_FPS};

const CYCLE: Duration = Duration::from_millis(2100);
const TIMELINE_MAX_FPS: f32 = 30.0;

// Normalized cell centers follow the V from its left tip to its right tip.
// These are glyph coordinates; the enclosing slot owns the interface scale.
const TIMELINE_CELLS: [(f32, f32); 7] = [
    (0.14, 0.20),
    (0.26, 0.40),
    (0.38, 0.60),
    (0.50, 0.80),
    (0.62, 0.60),
    (0.74, 0.40),
    (0.86, 0.20),
];

// Clockwise from the top. The dots stay still; only their light travels.
const SIDEBAR_DOTS: [(f32, f32); 4] = [(0.50, 0.22), (0.78, 0.50), (0.50, 0.78), (0.22, 0.50)];

#[derive(Clone, Copy)]
enum ActivityStyle {
    Timeline,
    Sidebar,
}

#[derive(IntoElement)]
pub(crate) struct ActivityIndicator {
    id: ElementId,
    style: ActivityStyle,
    color: Option<Hsla>,
}

impl ActivityIndicator {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: ActivityStyle::Timeline,
            color: None,
        }
    }

    /// A four-dot mark using the sidebar's smaller slot and frame budget.
    /// The owning row scopes `id` to its session, group, or workspace.
    pub(crate) fn sidebar(id: impl Into<ElementId>) -> Self {
        Self {
            style: ActivityStyle::Sidebar,
            ..Self::new(id)
        }
    }

    pub(crate) fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }
}

impl RenderOnce for ActivityIndicator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self { id, style, color } = self;
        let (slot, default_color, max_fps) = match style {
            ActivityStyle::Timeline => (div().size_4(), cx.theme().foreground, TIMELINE_MAX_FPS),
            ActivityStyle::Sidebar => {
                (div().size_3(), cx.theme().primary, STATUS_INDICATOR_MAX_FPS)
            }
        };
        let slot = slot.flex_none();
        let color = color.unwrap_or(default_color);
        let render_mark = move |phase| match style {
            ActivityStyle::Timeline => timeline_mark(color, phase).into_any_element(),
            ActivityStyle::Sidebar => sidebar_mark(color, phase).into_any_element(),
        };

        if motion::reduced_motion(cx) || motion::pauses_while_inactive(!window.is_window_active()) {
            return slot.child(render_mark(None)).into_any_element();
        }

        slot.with_animation(
            id,
            Animation::new(CYCLE).repeat_synced().with_max_fps(max_fps),
            move |this, phase| this.child(render_mark(Some(phase))),
        )
        .into_any_element()
    }
}

fn timeline_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let head = phase.map(|phase| 0.5 - 0.5 * (phase * TAU).cos());
            let cell_size = bounds.size.width.min(bounds.size.height) * 0.15;
            for (ix, (x, y)) in TIMELINE_CELLS.into_iter().enumerate() {
                let intensity = head.map_or(1.0, |head| {
                    let position = ix as f32 / (TIMELINE_CELLS.len() - 1) as f32;
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

fn sidebar_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let dot_size = bounds.size.width.min(bounds.size.height) * 0.18;
            for (ix, (x, y)) in SIDEBAR_DOTS.into_iter().enumerate() {
                // Overlapping cosine pulses hand light to the next dot without
                // a blink or a seam at the end of the cycle. Dim dots keep the
                // constellation legible throughout the handoff.
                let intensity = phase.map_or(1.0, |phase| {
                    let offset = ix as f32 / SIDEBAR_DOTS.len() as f32;
                    let pulse = 0.5 + 0.5 * ((phase - offset) * TAU).cos();
                    pulse * pulse
                });
                let center = point(
                    bounds.origin.x + bounds.size.width * x,
                    bounds.origin.y + bounds.size.height * y,
                );
                // Two faint shoulders soften the bright core without a blur
                // surface. All three layers fit inside the status slot.
                for (scale, opacity) in [
                    (1.9, 0.08 * intensity),
                    (1.4, 0.16 * intensity),
                    (1.0, 0.28 + 0.72 * intensity),
                ] {
                    let diameter = dot_size * scale;
                    let origin = point(center.x - diameter * 0.5, center.y - diameter * 0.5);
                    window.paint_quad(
                        fill(
                            Bounds::new(origin, size(diameter, diameter)),
                            color.opacity(opacity),
                        )
                        .corner_radii(diameter * 0.5),
                    );
                }
            }
        },
    )
    .size_full()
}

//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar sends a
//! diagonal wave through a dot matrix. Both use a synchronized, bounded
//! clock and a fixed, rem-sized canvas so motion never reflows adjacent text.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;

use crate::motion;

const CYCLE: Duration = Duration::from_millis(2100);
// Both waves share a smooth, bounded sample rate independent of display refresh.
const MAX_FPS: f32 = 30.0;
const SIDEBAR_GRID_SIDE: usize = 5;

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

    /// A dot-matrix mark in a fixed sidebar status slot.
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
        let (slot, default_color) = match style {
            ActivityStyle::Timeline => (div().size_4(), cx.theme().foreground),
            ActivityStyle::Sidebar => (div().size_5(), cx.theme().primary),
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
            Animation::new(CYCLE).repeat_synced().with_max_fps(MAX_FPS),
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
            let extent = bounds.size.width.min(bounds.size.height);
            let center = bounds.center();
            let last = (SIDEBAR_GRID_SIDE - 1) as f32;
            let pitch = extent * 0.72 / last;

            // Fixed dot centers keep the matrix crisp. The diagonal phase delay
            // carries one soft wave across it, with enough overlap at the loop
            // boundary to keep the running cue visible throughout the cycle.
            for row in 0..SIDEBAR_GRID_SIDE {
                for column in 0..SIDEBAR_GRID_SIDE {
                    let intensity = phase.map_or(1.0, |phase| {
                        let offset = (row + column) as f32 / last * 0.36;
                        let wave = 0.5 + 0.5 * ((phase - offset) * TAU).cos();
                        wave * wave
                    });
                    let diameter = extent * (0.075 + 0.055 * intensity);
                    let origin = point(
                        center.x + pitch * (column as f32 - last * 0.5) - diameter * 0.5,
                        center.y + pitch * (row as f32 - last * 0.5) - diameter * 0.5,
                    );
                    window.paint_quad(
                        fill(
                            Bounds::new(origin, size(diameter, diameter)),
                            color.opacity(0.30 + 0.70 * intensity),
                        )
                        .corner_radii(diameter * 0.5),
                    );
                }
            }
        },
    )
    .size_full()
}

//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar orbits
//! two comet trails around a breathing spark. Both use a synchronized, bounded
//! clock and a fixed, rem-sized canvas so motion never reflows adjacent text.

use std::f32::consts::{FRAC_PI_2, FRAC_PI_4, PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    PathBuilder, Pixels, Point, RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;

use crate::motion;

const CYCLE: Duration = Duration::from_millis(2100);
// Moving comet heads need the same smooth sampling as the timeline sweep.
// The shared cap keeps repeating marks independent of the display refresh rate.
const MAX_FPS: f32 = 30.0;
const COMET_TRAIL_SAMPLES: usize = 18;

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

    /// An orbital mark in a fixed sidebar status slot.
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
        let highlight = color.blend(cx.theme().sidebar_foreground.opacity(0.55));
        let render_mark = move |phase| match style {
            ActivityStyle::Timeline => timeline_mark(color, phase).into_any_element(),
            ActivityStyle::Sidebar => sidebar_mark(color, highlight, phase).into_any_element(),
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

fn sidebar_mark(color: Hsla, highlight: Hsla, phase: Option<f32>) -> impl IntoElement {
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let extent = bounds.size.width.min(bounds.size.height);
            let center = bounds.center();
            let breath = phase.map_or(1.0, |phase| 0.5 - 0.5 * (phase * TAU).cos());
            let phase = phase.unwrap_or(0.0);

            paint_light(
                window,
                center,
                extent * (0.30 + 0.035 * breath),
                color.opacity(0.13 + 0.04 * breath),
            );

            // Opposing orbits give the mark depth. Tapered, overlapping samples
            // form the tails without blur surfaces or independently timed dots.
            // Even the outer head's glow stays inside the fixed canvas.
            for (radius, head, sweep, diameter) in [
                (0.355, phase * TAU - FRAC_PI_2, TAU * 0.40, 0.105),
                (0.23, -phase * TAU + FRAC_PI_2, -TAU * 0.36, 0.075),
            ] {
                let orbit_point = |angle: f32| {
                    point(
                        center.x + extent * radius * angle.cos(),
                        center.y + extent * radius * angle.sin(),
                    )
                };
                for ix in (1..COMET_TRAIL_SAMPLES).rev() {
                    let tail = ix as f32 / (COMET_TRAIL_SAMPLES - 1) as f32;
                    let intensity = 1.0 - tail;
                    paint_dot(
                        window,
                        orbit_point(head - sweep * tail),
                        extent * diameter * (0.35 + 0.65 * intensity),
                        color.opacity(0.08 + 0.78 * intensity * intensity),
                    );
                }
                let head = orbit_point(head);
                paint_light(window, head, extent * diameter, color);
                paint_dot(window, head, extent * diameter * 0.48, highlight);
            }

            // The four-point spark turns a quarter revolution per cycle, so
            // its symmetric silhouette loops seamlessly and never disappears.
            let mut spark = PathBuilder::fill();
            for ix in 0..8 {
                let angle = phase * FRAC_PI_2 + ix as f32 * FRAC_PI_4 - FRAC_PI_2;
                let radius = extent
                    * if ix % 2 == 0 {
                        0.135 + 0.02 * breath
                    } else {
                        0.045
                    };
                let vertex = point(
                    center.x + radius * angle.cos(),
                    center.y + radius * angle.sin(),
                );
                if ix == 0 {
                    spark.move_to(vertex);
                } else {
                    spark.line_to(vertex);
                }
            }
            spark.close();
            if let Ok(spark) = spark.build() {
                window.paint_path(spark, highlight);
            }
        },
    )
    .size_full()
}

fn paint_light(window: &mut Window, center: Point<Pixels>, diameter: Pixels, color: Hsla) {
    for (scale, opacity) in [(2.2, 0.10), (1.5, 0.20), (1.0, 1.0)] {
        paint_dot(window, center, diameter * scale, color.opacity(opacity));
    }
}

fn paint_dot(window: &mut Window, center: Point<Pixels>, diameter: Pixels, color: Hsla) {
    let origin = point(center.x - diameter * 0.5, center.y - diameter * 0.5);
    window.paint_quad(
        fill(Bounds::new(origin, size(diameter, diameter)), color).corner_radii(diameter * 0.5),
    );
}

//! The working mark: a ring of six dots with one highlight walking it.
//!
//! Every surface that reports "still working" draws this one mark, so a row,
//! a tab and a header never disagree about what a running session looks
//! like. The lap runs on the platform's own animation clock; nothing here
//! wakes the app per frame.

use nsh_core::{
    element::{Animation, Timing},
    prelude::*,
};

/// The slot the ring is drawn in.
pub const ACTIVITY_SLOT: f32 = 20.0;
/// One lap of the highlight, in milliseconds: eight spinner frames at 100 ms.
const ACTIVITY_LAP_MS: f32 = 800.0;

/// One lap of the V's highlight.
///
/// The desktop's timeline indicator takes this long to walk the mark, and the
/// two clients read as the same thing only at the same speed.
pub const V_LAP_MS: f32 = 2100.0;
/// The dots, in ring order: down the left column, back up the right one.
const RING: [(f32, f32); 6] = [
    (0.0, 0.0),
    (0.0, 1.0),
    (0.0, 2.0),
    (1.0, 2.0),
    (1.0, 1.0),
    (1.0, 0.0),
];

/// The clock every ring on a page follows, so they turn together.
#[derive(Clone, Copy)]
pub struct ActivityClock {
    phase: AnimatedValue,
    moving: bool,
}

impl ActivityClock {
    /// Starts the lap under the current owner. With Reduce Motion the ring is
    /// drawn still, its highlight parked at the lap's start.
    pub fn start() -> Self {
        Self::with_lap(ACTIVITY_LAP_MS)
    }

    /// The same clock, lapping in `lap_ms`.
    ///
    /// The V walks a longer path than the ring does — seven dots from one tip
    /// to the other, not six around a cell — and it reads as a sweep rather
    /// than a turn: the desktop gives it 2100 ms, and at the ring's 800 ms the
    /// highlight jumps between dots instead of travelling.
    pub fn with_lap(lap_ms: f32) -> Self {
        let phase = animated_value(0.0);
        let moving = !crate::motion::reduce_motion();
        if moving {
            Animation::timing(&phase, 1.0, Timing::new(lap_ms, Easing::Linear))
                .looped(None)
                .start();
        }
        Self { phase, moving }
    }
}

/// How bright dot `index` is while the highlight is `head` dots along.
fn brightness(index: usize, head: f32) -> f32 {
    let lap = RING.len() as f32;
    let offset = (index as f32 - head).rem_euclid(lap);
    let distance = offset.min(lap - offset);
    let intensity = 0.5 + 0.5 * (distance / lap * std::f32::consts::TAU).cos();
    0.28 + 0.72 * intensity
}

/// The ring in `color`, following `clock`.
pub fn activity_ring(clock: ActivityClock, color: impl Fn() -> Hsla + Copy + 'static) -> View {
    let center = ACTIVITY_SLOT / 2.0;
    let pitch = ACTIVITY_SLOT * 0.26;
    let diameter = ACTIVITY_SLOT * 0.16;
    // The highlight's place at each sixth of the lap; the platform interpolates
    // between them, which is the cosine sampled where it turns.
    let stops: Vec<f32> = (0..=12).map(|step| step as f32 / 12.0).collect();
    view()
        .w(px(ACTIVITY_SLOT))
        .h(px(ACTIVITY_SLOT))
        .flex_shrink_0()
        .relative()
        .children(
            RING.into_iter()
                .enumerate()
                .map(move |(index, (column, row))| {
                    let x = center + (column - 0.5) * pitch - diameter * 0.5;
                    let y = center + (row - 1.0) * pitch - diameter * 0.5;
                    let dot = view()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(diameter))
                        .h(px(diameter))
                        .rounded(px(diameter * 0.5))
                        .bg(color);
                    if clock.moving {
                        let levels: Vec<f32> = stops
                            .iter()
                            .map(|stop| brightness(index, stop * RING.len() as f32))
                            .collect();
                        dot.bind_opacity(&clock.phase.interpolate(&stops, &levels))
                    } else {
                        dot.opacity(brightness(index, 0.0))
                    }
                }),
        )
}

/// The dots of the V a running turn leads with, in glyph coordinates.
///
/// They follow the mark from its left tip down to the point and back up to the
/// right one: the path the desktop's timeline indicator walks. One highlight
/// travels that stroke, so the shape reads as the product's V rather than as a
/// generic spinner.
const V_DOTS: [(f32, f32); 7] = [
    (0.14, 0.20),
    (0.26, 0.40),
    (0.38, 0.60),
    (0.50, 0.80),
    (0.62, 0.60),
    (0.74, 0.40),
    (0.86, 0.20),
];

/// The V of dots, with one highlight walking its stroke.
///
/// The mark a running turn leads with on the desktop. The dots sit on the V's
/// own path and the highlight is a brightness wave along it, so the glyph is
/// the product's mark and the motion happens inside it.
pub fn activity_v(clock: ActivityClock, color: impl Fn() -> Hsla + Copy + 'static) -> View {
    let size = ACTIVITY_SLOT;
    let diameter = size * 0.16;
    let stops: Vec<f32> = (0..=12).map(|step| step as f32 / 12.0).collect();
    view()
        .w(px(size))
        .h(px(size))
        .flex_shrink_0()
        .relative()
        .children(
            V_DOTS
                .into_iter()
                .enumerate()
                .map(move |(index, (column, row))| {
                    let x = column * size - diameter * 0.5;
                    let y = row * size - diameter * 0.5;
                    let dot = view()
                        .absolute()
                        .left(px(x))
                        .top(px(y))
                        .w(px(diameter))
                        .h(px(diameter))
                        .rounded(px(diameter * 0.5))
                        .bg(color);
                    if clock.moving {
                        let levels: Vec<f32> = stops
                            .iter()
                            .map(|stop| brightness(index, stop * V_DOTS.len() as f32))
                            .collect();
                        dot.bind_opacity(&clock.phase.interpolate(&stops, &levels))
                    } else {
                        dot.opacity(brightness(index, 0.0))
                    }
                }),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_dot_under_the_highlight_is_brightest() {
        assert!((brightness(0, 0.0) - 1.0).abs() < 1e-5);
        assert!((brightness(3, 0.0) - 0.28).abs() < 1e-5);
        assert!(brightness(1, 0.0) > brightness(2, 0.0));
    }

    #[test]
    fn the_lap_closes_without_a_jump() {
        for index in 0..RING.len() {
            assert!((brightness(index, 0.0) - brightness(index, 6.0)).abs() < 1e-5);
        }
    }

    /// The V's dots run left to right and dip to the point in the middle: the
    /// shape is the product's mark, not a ring.
    #[test]
    fn the_v_dots_follow_the_marks_stroke() {
        for pair in V_DOTS.windows(2) {
            assert!(pair[0].0 < pair[1].0);
        }
        assert!(V_DOTS[3].1 > V_DOTS[0].1);
        assert!(V_DOTS[3].1 > V_DOTS[6].1);
    }
}

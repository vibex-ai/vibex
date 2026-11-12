//! Activity marks for live timeline labels and compact sidebar rows.
//!
//! The timeline's V sweeps a highlight along its strokes; the sidebar walks one
//! around the Braille spinner's dot ring. Both use a synchronized, bounded clock
//! and a fixed, rem-sized canvas so motion never reflows adjacent text.

use std::f32::consts::{PI, TAU};
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Bounds, ElementId, Hsla, IntoElement, ParentElement as _,
    Pixels, RenderOnce, Styled as _, Window, canvas, div, fill, point, size,
};
use gpui_component::ActiveTheme as _;
use vibex_tui::glyphs::{self, GlyphTier};

use crate::motion;

const TIMELINE_CYCLE: Duration = Duration::from_millis(2100);

/// Frames per second both marks are sampled at.
///
/// The sidebar mark used to step through the TUI's eight spinner frames at
/// their 100ms cadence. That cap never saved the workbench a frame while the
/// running title's sweep still asked for one every display refresh, and it made
/// the one moving thing in the row arrive on a timer: a `with_max_fps`
/// re-render lands some time *after* the interval it waits for, so the phase
/// read on arrival skipped frames and the rotation dropped dots. The title
/// sweep now shares this budget, and a continuous mark sampled at 30fps costs
/// the same frames the row spends without stepping.
const ACTIVITY_MAX_FPS: f32 = 30.0;

/// How long the TUI holds one of its spinner frames.
const SIDEBAR_FRAME_DURATION: Duration = Duration::from_millis(100);

/// One lap of the sidebar highlight: the time the TUI takes to walk its frames.
fn sidebar_cycle() -> Duration {
    SIDEBAR_FRAME_DURATION * glyphs::spinner_frames(GlyphTier::Full).len() as u32
}

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

/// The painted sidebar mark's dots, as indexes into the Braille cell.
///
/// Dots 1-3 run down the left column and 4-6 back up the right one, so the six
/// form a closed ring around the cell — the path the TUI's frames carry their
/// lit window along. The mark walks that ring instead of replaying the frames:
/// the same rotation, with the light moving continuously instead of jumping
/// 100ms at a time.
///
/// The table is the character client's, because the turn band steps the same
/// ring (`glyphs::activity_ring_frames`): one definition, so a surface cannot
/// keep walking a ring the other has stopped drawing.
const SIDEBAR_RING: [usize; 6] = glyphs::ACTIVITY_RING;

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
    size: Option<Pixels>,
}

impl ActivityIndicator {
    pub(crate) fn new(id: impl Into<ElementId>) -> Self {
        Self {
            id: id.into(),
            style: ActivityStyle::Timeline,
            color: None,
            size: None,
        }
    }

    /// The TUI's spinner dot ring in a fixed sidebar status slot.
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

    /// The same mark in a box of the caller's own size.
    ///
    /// Surfaces that swap this mark with a logo — the new-session Agent chips
    /// carry the mark while the Agent downloads and the brand mark once it is
    /// ready — need both faces to occupy the same square, or the label beside
    /// them would step sideways when the download completes.
    pub(crate) fn size(mut self, size: Pixels) -> Self {
        self.size = Some(size);
        self
    }
}

impl RenderOnce for ActivityIndicator {
    fn render(self, window: &mut Window, cx: &mut App) -> impl IntoElement {
        let Self {
            id,
            style,
            color,
            size,
        } = self;
        let (slot, default_color, cycle, max_fps) = match style {
            ActivityStyle::Timeline => (
                div().size_4(),
                cx.theme().foreground,
                TIMELINE_CYCLE,
                ACTIVITY_MAX_FPS,
            ),
            ActivityStyle::Sidebar => (
                div().size_5(),
                cx.theme().primary,
                sidebar_cycle(),
                ACTIVITY_MAX_FPS,
            ),
        };
        let slot = match size {
            Some(size) => slot.size(size),
            None => slot,
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
            Animation::new(cycle).repeat_synced().with_max_fps(max_fps),
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

/// How far apart two ring positions are, the short way around the loop.
fn ring_distance(index: f32, head: f32, lap: f32) -> f32 {
    let offset = (index - head).rem_euclid(lap);
    offset.min(lap - offset)
}

/// The cell coordinates the lit dots are centred on.
///
/// The spinner's dots occupy three of the Braille cell's four rows, so the
/// visual middle of the ring is row 1, not the cell's own 1.5. Anything the
/// mark is centred against — the status column, the title it reports on — is
/// lined up with the dots that are actually painted.
fn ring_center(ring: &[usize]) -> (f32, f32) {
    let (mut columns, mut rows) = (0.0_f32, 0.0_f32);
    for dot in ring {
        let (column, row) = braille_dot_cell(*dot);
        columns += column;
        rows += row;
    }
    let count = ring.len() as f32;
    (columns / count, rows / count)
}

/// The sidebar mark: the spinner's dot ring with one highlight walking it.
///
/// `phase` is the fraction of a lap travelled by the head, or `None` for the
/// fully visible still mark reduced motion and the opt-in inactive-window pause
/// fall back to — the head parked at the lap's start, the pose gpui's own
/// reduced-motion handling leaves a repeating animation in.
fn sidebar_mark(color: Hsla, phase: Option<f32>) -> impl IntoElement {
    let head = phase.unwrap_or(0.0) * SIDEBAR_RING.len() as f32;
    canvas(
        |_, _, _| (),
        move |bounds, _, window, _| {
            let extent = bounds.size.width.min(bounds.size.height);
            let center = bounds.center();
            let diameter = extent * 0.16;
            let pitch = extent * 0.26;
            let lap = SIDEBAR_RING.len() as f32;
            // The ring lights three of the cell's four rows, so the middle of
            // the *painted* dots is what meets the slot's centre. Centring the
            // whole Braille cell would lift the visible ring half a pitch above
            // the title it reports on.
            let (middle_column, middle_row) = ring_center(&SIDEBAR_RING);
            // The frame is painted, not shaped: the reader's code font is free
            // to lack the Braille block — JetBrains Mono, for one, draws its
            // replacement box for it — and then the running cue would be that
            // box instead of a spinner. Each dot sits at its Braille cell
            // center, so the ring renders in whatever font the workbench is set
            // to, and only its brightness carries the motion.
            for (index, dot) in SIDEBAR_RING.iter().copied().enumerate() {
                let (column, row) = braille_dot_cell(dot);
                // Brightness is the cosine of the dot's distance from the head
                // around the ring: the dot under the head is at full contrast,
                // the one opposite it is at rest, and every dot between rides a
                // smooth ramp. The same falloff the timeline sweep uses, wrapped
                // around a circle so one lap closes seamlessly.
                let distance = ring_distance(index as f32, head, lap);
                let intensity = 0.5 + 0.5 * (distance / lap * TAU).cos();
                let origin = point(
                    center.x + (column - middle_column) * pitch - diameter * 0.5,
                    center.y + (row - middle_row) * pitch - diameter * 0.5,
                );
                window.paint_quad(
                    fill(
                        Bounds::new(origin, size(diameter, diameter)),
                        color.opacity(0.28 + 0.72 * intensity),
                    )
                    .corner_radii(diameter * 0.5),
                );
            }
        },
    )
    .size_full()
}

/// The eight Braille dots of one spinner frame, as a bit mask.
///
/// The TUI's frames still name the dots the sidebar ring paints and the pace it
/// walks them; only the frame-by-frame replay is gone, so the read-back lives
/// with the tests that hold the two tables together.
#[cfg(test)]
fn braille_dots(frame: &str) -> u8 {
    frame
        .chars()
        .next()
        .and_then(|glyph| (glyph as u32).checked_sub('\u{2800}' as u32))
        .filter(|dots| *dots <= u8::MAX as u32)
        .map_or(0, |dots| dots as u8)
}

/// Where each Braille dot sits in its two-by-four cell.
///
/// Dots 1-3 run down the left column and 4-6 down the right one, then the
/// bottom row holds dots 7 and 8 — the arrangement a terminal cell uses, which
/// is what makes the painted mark read as the same spinner the TUI draws.
fn braille_dot_cell(index: usize) -> (f32, f32) {
    match index {
        0 => (0.0, 0.0),
        1 => (0.0, 1.0),
        2 => (0.0, 2.0),
        3 => (1.0, 0.0),
        4 => (1.0, 1.0),
        5 => (1.0, 2.0),
        6 => (0.0, 3.0),
        _ => (1.0, 3.0),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_frames_read_back_as_braille_dots() {
        let frames = glyphs::spinner_frames(GlyphTier::Full);
        assert!(!frames.is_empty());
        let mut masks = Vec::new();
        for frame in frames {
            let dots = braille_dots(frame);
            // Every frame of the working spinner is a partial cell, so an empty
            // mask would paint a mark that says nothing.
            assert_ne!(dots, 0, "{frame} carries no dots");
            assert!(!masks.contains(&dots), "{frame} repeats a frame");
            masks.push(dots);
        }
    }

    #[test]
    fn non_braille_frames_have_no_dots_to_paint() {
        assert_eq!(braille_dots("|"), 0);
        assert_eq!(braille_dots(""), 0);
    }

    /// The painted ring carries exactly the dots the TUI's spinner cycles, at
    /// the pace it cycles them.
    ///
    /// The frames are no longer replayed, so this test is what keeps the two
    /// surfaces the same cue: the desktop may not quietly drop a dot the spinner
    /// lights, paint one twice, or turn at a speed of its own.
    #[test]
    fn the_sidebar_ring_matches_the_tui_spinner() {
        let frames = glyphs::spinner_frames(GlyphTier::Full);
        let lit = frames
            .iter()
            .map(|frame| braille_dots(frame))
            .fold(0, |all, dots| all | dots);
        let ring = SIDEBAR_RING
            .iter()
            .map(|index| 1u8 << index)
            .fold(0, |all, dot| all | dot);
        assert_eq!(
            ring, lit,
            "the ring paints a different dot set than the spinner cycles"
        );
        assert_eq!(
            ring.count_ones() as usize,
            SIDEBAR_RING.len(),
            "a dot is painted twice"
        );
        assert_eq!(
            sidebar_cycle(),
            SIDEBAR_FRAME_DURATION * frames.len() as u32
        );
    }

    /// The ring this mark sweeps is the walk the character client steps.
    ///
    /// The band above the TUI's composer draws the same running mark, so the
    /// two surfaces have to make the same move: frame `i` of the client's ring
    /// lights the window around `ACTIVITY_RING[i]`, which is where this canvas
    /// puts the head at step `i`. A ring that lit a different window would be a
    /// second mark that happens to share the dot set.
    #[test]
    fn the_sidebar_ring_steps_the_frames_the_turn_band_draws() {
        let frames = glyphs::activity_ring_frames(GlyphTier::Full);
        assert_eq!(
            frames.len(),
            SIDEBAR_RING.len(),
            "a lap is one frame per dot of the ring"
        );
        let lap = SIDEBAR_RING.len();
        for (step, frame) in frames.iter().enumerate() {
            let window = [step + lap - 1, step, step + 1]
                .into_iter()
                .map(|index| 1u8 << SIDEBAR_RING[index % lap])
                .fold(0u8, |all, dot| all | dot);
            assert_eq!(
                braille_dots(frame),
                window,
                "{frame} is not the ring's step {step}"
            );
        }
    }

    /// The painted ring is centred in its slot by the dots it actually lights.
    ///
    /// The slot is a 20px box in a row whose title sits on the row's centre
    /// line. The dots fill three of the Braille cell's four rows, so centring
    /// the *cell* leaves the visible ring half a pitch (2.6px at the sidebar
    /// scale) above the text. The mark must be centred on the dots, and the
    /// dots must sit symmetrically about that centre.
    #[test]
    fn the_sidebar_ring_is_centred_on_the_dots_it_paints() {
        let (middle_column, middle_row) = ring_center(&SIDEBAR_RING);
        assert_eq!(middle_column, 0.5);
        assert_eq!(middle_row, 1.0, "the lit dots span rows 0..=2");
        // The lit rows are symmetric about the centre: the topmost dot is as
        // far above it as the bottommost is below it, or the mark would still
        // read as leaning.
        let rows = SIDEBAR_RING.map(|dot| braille_dot_cell(dot).1);
        let top = rows.iter().copied().fold(f32::INFINITY, f32::min);
        let bottom = rows.iter().copied().fold(f32::NEG_INFINITY, f32::max);
        assert_eq!(middle_row - top, bottom - middle_row);
        // And the whole ring stays inside the slot once shifted onto that
        // centre: a dot may not be painted past the box that holds it.
        let extent = 20.0_f32;
        let pitch = extent * 0.26;
        let radius = extent * 0.16 * 0.5;
        let half = extent * 0.5;
        for dot in SIDEBAR_RING {
            let (column, row) = braille_dot_cell(dot);
            let x = (column - middle_column) * pitch;
            let y = (row - middle_row) * pitch;
            assert!(
                x - radius >= -half && x + radius <= half,
                "dot {dot} overflows the slot horizontally"
            );
            assert!(
                y - radius >= -half && y + radius <= half,
                "dot {dot} overflows the slot vertically"
            );
        }
    }

    /// Consecutive ring positions are neighbours in the Braille cell.
    ///
    /// This is the "steps missing" regression. The highlight used to be a frame
    /// index, so a late re-render dropped whole frames and the light jumped
    /// across the cell; a head that walks neighbouring dots cannot jump.
    #[test]
    fn the_sidebar_ring_walks_neighbouring_dots_and_closes() {
        for step in 0..SIDEBAR_RING.len() {
            let (column, row) = braille_dot_cell(SIDEBAR_RING[step]);
            let next = SIDEBAR_RING[(step + 1) % SIDEBAR_RING.len()];
            let (next_column, next_row) = braille_dot_cell(next);
            let distance = (column - next_column).abs() + (row - next_row).abs();
            assert_eq!(distance, 1.0, "ring step {step} jumps");
        }
    }

    /// Every dot takes its turn at full brightness at the mark's real sample
    /// rate, and no frame-to-frame change is large enough to read as a pop.
    #[test]
    fn the_sidebar_highlight_lights_every_dot_smoothly() {
        let lap = SIDEBAR_RING.len() as f32;
        let brightness = |index: usize, phase: f32| {
            let distance = ring_distance(index as f32, phase * lap, lap);
            0.5 + 0.5 * (distance / lap * TAU).cos()
        };
        // One lap lasts `sidebar_cycle()` and the animation is sampled at
        // `ACTIVITY_MAX_FPS`, so this is the sequence a reader actually sees.
        let samples = (sidebar_cycle().as_secs_f32() * ACTIVITY_MAX_FPS).round() as usize;
        assert!(
            samples > glyphs::spinner_frames(GlyphTier::Full).len(),
            "the mark must be sampled faster than the frames it replaced"
        );
        let mut peaks = [0.0_f32; SIDEBAR_RING.len()];
        let mut previous = [0.0_f32; SIDEBAR_RING.len()];
        let mut worst_step = 0.0_f32;
        for sample in 0..=samples {
            let phase = sample as f32 / samples as f32;
            for index in 0..SIDEBAR_RING.len() {
                let value = brightness(index, phase);
                peaks[index] = peaks[index].max(value);
                if sample > 0 {
                    worst_step = worst_step.max((value - previous[index]).abs());
                }
                previous[index] = value;
            }
        }
        for (index, peak) in peaks.iter().enumerate() {
            assert!(*peak > 0.98, "dot {index} only reached {peak}");
        }
        // A step under a fifth of the ramp is a ramp, not a flicker.
        assert!(
            worst_step < 0.2,
            "brightness stepped {worst_step} in one frame"
        );
    }
}

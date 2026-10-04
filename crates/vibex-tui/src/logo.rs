//! The wordmark a reader lands on.
//!
//! The first screen of a client is the only one that is allowed to be a
//! *picture*: there is nothing to read yet, so the space is spent saying who
//! this is and what to do next rather than on an empty transcript.
//!
//! The mark is the brand's own — two descending slabs that read as a `V` whose
//! right arm stops a third of the way down — rather than a banner font's
//! letterform. A shape every client shares says nothing about this one, and the
//! geometry is what the light animates along.
//!
//! # Where the art comes from
//!
//! Each tier is a hand-checked rasterisation of `logo-white.svg` at the root of
//! the repository. The sampling grid is not free: a half block is one column
//! wide and half a row tall, so a sub-cell is square only when the terminal's
//! rows are twice its columns, and the mark's own bounding box is 1.06:1.
//! Sampled honestly across six rows that leaves thirteen columns, which is too
//! coarse to hold the two strokes apart — they merge into one wedge. The
//! shipped art is therefore deliberately wider than the original (1.67:1 rather
//! than 1.06:1), and the narrow tiers wider still, because legibility is what
//! the mark is for. The shape reads; the measurement does not.
//!
//! # The light
//!
//! A static block of glyphs cannot say that the client is alive and waiting, so
//! one light does: it draws the mark stroke-first, lets it settle, and crosses
//! it once more with a narrow glint. All three are one pass of [`SWEEP_FRAMES`],
//! which is the number the client counts to before it stops repainting
//! ([`crate::app::LANDING_SWEEP_FRAMES`]).
//!
//! Three things about the light are deliberate and easy to get wrong:
//!
//! * the wavefront travels *along* the stroke, not across the screen. The mark
//!   is a pair of descending slabs; a vertical or horizontal band fills it like
//!   a shutter, and a wavefront perpendicular to the slabs draws it the way a
//!   hand would;
//! * the mark at rest is not at full strength. It is brightest at the top and
//!   steps down toward the bottom, and the top keeps headroom the closing glint
//!   can still spend. A mark that rests at maximum has nowhere left to light up;
//! * a cell's light is *hue first, then brightness*. Walking the mark straight
//!   toward the foreground would leave the middle of the pass as a grey smudge;
//!   the ramp goes through the theme's cyan so the light has a colour before it
//!   has a core.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{ColorMode, GlyphMode, TuiTheme};

/// The name the mark stands for, and the half the mark does not spell.
///
/// Never localised: it is a wordmark, and the mark beside it is its first
/// letter, so the two read as the product's name in every language.
pub const WORD: &str = "ibex";

// ---------------------------------------------------------------------------
// Art
// ---------------------------------------------------------------------------

/// The mark at its widest: the tier most terminals get.
const MARK_WIDE: [&str; 6] = [
    "▀█████▄       █████▀",
    "  ▀█████▄      ▀█▀",
    "   ▀██████▄",
    "     ▀█████▄",
    "       █████▀",
    "        ▀█▀",
];

/// The mark for a terminal too narrow for [`MARK_WIDE`].
///
/// The narrowest tier there is, and the narrowest the new-session page is ever
/// drawn at: below sixteen columns that page cannot print its own name
/// (`ibex` and a version number), so it shows an empty state instead of a mark
/// and this tier stops being reachable. A third tier sized for ten columns was
/// measured and dropped rather than shipped dead.
const MARK_MID: [&str; 4] = ["▀███▄     ███▀", "  ▀███▄    ▀", "    ▀███▄", "     ▀█▀"];

/// The mark for a console with no block glyphs.
///
/// Not a rasterisation of the same shape — at this size that would be a smear
/// of substitution characters. It is the same statement made smaller: the
/// brand's asymmetry, a heavy left arm and a light right one, drawn with the
/// three characters a legacy console font certainly has.
const MARK_ASCII: [&str; 4] = ["\\\\       /", " \\\\     /", "  \\\\   /", "   \\\\_/"];

/// One size of the mark, and the geometry its light needs.
struct Tier {
    /// The art, one string per row, ragged: rows are padded when they are drawn.
    rows: &'static [&'static str],
    /// Columns the stroke descends per row, measured off the art.
    ///
    /// The wavefront has to be perpendicular to the stroke, so this is measured
    /// rather than chosen.
    slope: f32,
}

const TIERS: [Tier; 2] = [
    Tier {
        rows: &MARK_WIDE,
        slope: 1.60,
    },
    Tier {
        rows: &MARK_MID,
        slope: 1.67,
    },
];

const ASCII_TIER: Tier = Tier {
    rows: &MARK_ASCII,
    slope: 1.00,
};

impl Tier {
    /// The widest row of the art. Every drawn row is padded to it.
    fn width(&self) -> usize {
        self.rows
            .iter()
            .map(|row| row.chars().count())
            .max()
            .unwrap_or(0)
    }

    fn height(&self) -> usize {
        self.rows.len()
    }

    /// Where a cell sits along the stroke: 0.0 at the top of the left arm, 1.0
    /// at the far end of the right one.
    fn along_stroke(&self, column: usize, row: usize) -> f32 {
        let length = (self.width().saturating_sub(1)) as f32 * self.slope
            + (self.height().saturating_sub(1)) as f32;
        if length <= 0.0 {
            return 0.0;
        }
        (column as f32 * self.slope + row as f32) / length
    }
}

// ---------------------------------------------------------------------------
// The light
// ---------------------------------------------------------------------------

/// Frames the mark takes to draw itself.
const REVEAL_FRAMES: u32 = 20;

/// Frames a freshly drawn cell stays hot for before it settles.
const COOL_FRAMES: f32 = 12.0;

/// How far above its resting weight a just-drawn cell starts.
const HEAT: f32 = 0.55;

/// The frame the closing glint enters on.
const GLINT_START: u32 = 38;

/// Frames the closing glint takes to cross the mark and leave it.
const GLINT_FRAMES: u32 = 22;

/// Columns either side of the glint's centre that it reaches across.
const GLINT_WIDTH: f32 = 1.7;

/// Frames one whole landing animation takes: drawn, settled, crossed.
///
/// The number the client repaints for and then stops at.
pub const SWEEP_FRAMES: u32 = GLINT_START + GLINT_FRAMES;

/// How far down the mark's own grey scale the resting bottom row sits.
///
/// The top row rests at `REST_HEAD` of the way there rather than at the
/// foreground, which is the headroom the closing glint spends.
const REST_HEAD: f32 = 0.25;

/// Above this much light a cell is drawn bold in a palette that cannot blend.
///
/// Below [`HEAT`] on purpose: the peak of a cooling cell is `HEAT` itself, so a
/// threshold at or above it would mean no cell is ever lit in a palette that
/// has only weight to spend, and the light would not be drawn at all.
const LIT: f32 = 0.45;

/// The mark's light: the colour a glint is tinted with.
///
/// Cyan rather than the running rail's blue. Both mean "alive", but the landing
/// page is doing nothing at all, and the blue that means *a turn is running*
/// would say the wrong thing on it.
fn light_hue(theme: &TuiTheme) -> Color {
    theme.roles.accent_tool
}

/// Whether this terminal can be shown a light at all.
///
/// With colour switched off there is nothing to spend: the mark would still be
/// drawn stroke by stroke, in one colour, which is decoration the reader asked
/// not to have. The shape is still the brand's; only the movement is dropped.
fn lights_up(theme: &TuiTheme) -> bool {
    !matches!(theme.capability.mode, ColorMode::None)
}

/// How wide the mark drawn into `available` columns is, or zero when the
/// terminal is too narrow for even the smallest tier.
pub fn width(glyphs: GlyphMode, available: u16) -> usize {
    tier(glyphs, available)
        .map(|tier| tier.width())
        .unwrap_or(0)
}

/// The tier that fits `available` columns, if one does.
///
/// Two columns of margin: a mark that reaches the edge of the pane reads as
/// clipped whether or not it is.
fn tier(glyphs: GlyphMode, available: u16) -> Option<&'static Tier> {
    let available = usize::from(available);
    if glyphs == GlyphMode::Ascii {
        return (available >= ASCII_TIER.width() + 2).then_some(&ASCII_TIER);
    }
    TIERS.iter().find(|tier| available >= tier.width() + 2)
}

/// Rows of the mark, lit as if a light were passing through it.
///
/// `phase` advances a frame at a time and is not wrapped: past [`SWEEP_FRAMES`]
/// the mark is simply at rest, which is what keeps an idle client at zero
/// frames. `bright` is whether the page the mark sits on is waiting for input
/// at all; a mark on a page that is not is drawn at rest and never animated.
pub fn rows(theme: &TuiTheme, phase: u32, available: u16, bright: bool) -> Vec<Line<'static>> {
    let Some(tier) = tier(theme.glyphs(), available) else {
        return Vec::new();
    };
    let width = tier.width();
    let height = tier.height();
    let animating = bright && lights_up(theme) && phase < SWEEP_FRAMES;
    // Linear rather than eased: the heat a cell carries is measured in frames
    // since it appeared, and a curved front could not be turned back into one
    // without a search.
    let progress = (phase as f32 / REVEAL_FRAMES as f32).min(1.0);
    let glint = glint_position(phase, width);

    tier.rows
        .iter()
        .enumerate()
        .map(|(row, art)| {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
            let mut column = 0usize;
            for character in art.chars() {
                if character != ' ' && (!animating || tier.along_stroke(column, row) <= progress) {
                    let light = if animating {
                        glow(phase, tier.along_stroke(column, row), column, glint)
                    } else {
                        0.0
                    };
                    spans.push(Span::styled(
                        character.to_string(),
                        style_for(theme, row, height, light),
                    ));
                } else {
                    // A cell that is not part of the mark yet is a gap, not a
                    // dim letter; it keeps its column so the mark's width — and
                    // so its centring — does not shift as it appears.
                    spans.push(Span::raw(" "));
                }
                column += 1;
            }
            while column < width {
                spans.push(Span::raw(" "));
                column += 1;
            }
            Line::from(spans)
        })
        .collect()
}

/// Where the closing glint's centre is, in columns.
fn glint_position(phase: u32, width: usize) -> f32 {
    if !(GLINT_START..SWEEP_FRAMES).contains(&phase) {
        return f32::NEG_INFINITY;
    }
    let crossed = (phase - GLINT_START) as f32 / GLINT_FRAMES as f32;
    // Enters from off the left edge and leaves past the right one, so the last
    // frame of the pass is indistinguishable from rest and the mark hands
    // straight over to the idle state.
    crossed * (width as f32 + 8.0) - 4.0
}

/// How much light a cell carries: 0.0 at rest, 1.0 at the core of a glint.
fn glow(phase: u32, along: f32, column: usize, glint: f32) -> f32 {
    // The heat of having just been drawn, cooling on a clock of its own: a cell
    // revealed at the end of the pass has to be able to settle, which is why
    // this is measured in frames since the reveal rather than in the distance
    // between the cell and the wavefront.
    let since = phase as f32 - along * REVEAL_FRAMES as f32;
    let heat = HEAT * (1.0 - since / COOL_FRAMES).clamp(0.0, 1.0);
    let crossing = (-((column as f32 - glint) / GLINT_WIDTH).powi(2)).exp();
    heat.max(crossing)
}

/// The style one cell of the mark is drawn in.
///
/// A cell's colour is its resting weight walked toward the light and then
/// toward the foreground, so a glint has a core and a tinted fringe.
/// [`ColorMode::blends`] decides whether that ramp can be drawn at all; where
/// it cannot, the resting weight becomes a step between two theme greys and the
/// light becomes weight, which is the only brightness a shallow palette has.
fn style_for(theme: &TuiTheme, row: usize, height: usize, light: f32) -> Style {
    if !lights_up(theme) {
        return Style::default().fg(theme.roles.foreground);
    }
    // The lower half rests a step down its own greys; a palette that cannot
    // blend carries that step as `DIM` instead of as a colour.
    let low = height > 1 && row * 2 >= height;
    if !theme.capability.mode.blends() {
        let colour = if low {
            theme.roles.gray_bright
        } else {
            theme.roles.foreground
        };
        return if light > LIT {
            Style::default().fg(colour).add_modifier(Modifier::BOLD)
        } else if low {
            Style::default().fg(colour).add_modifier(Modifier::DIM)
        } else {
            Style::default().fg(colour)
        };
    }
    let depth = if height <= 1 {
        REST_HEAD
    } else {
        REST_HEAD + (1.0 - REST_HEAD) * (row as f32 / (height - 1) as f32)
    };
    let rest = theme.blend(theme.roles.foreground, theme.roles.gray_dim, depth);
    let colour = if light <= 0.0 {
        rest
    } else if light < 0.5 {
        theme.blend(rest, light_hue(theme), light * 2.0)
    } else {
        theme.blend(
            light_hue(theme),
            theme.roles.foreground,
            (light - 0.5) * 2.0,
        )
    };
    Style::default().fg(colour)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::ColorCapability;
    use vibex_ui::GpuiThemeMode;

    fn theme(mode: ColorMode, glyphs: GlyphMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability { mode, glyphs },
        )
    }

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    /// Every cell of a tier's art, as `(column, row, position along the stroke)`.
    fn cells(tier: &Tier) -> Vec<(usize, usize, f32)> {
        let mut out = Vec::new();
        for (row, art) in tier.rows.iter().enumerate() {
            for (column, character) in art.chars().enumerate() {
                if character != ' ' {
                    out.push((column, row, tier.along_stroke(column, row)));
                }
            }
        }
        out
    }

    fn first_colour(line: &Line<'static>) -> Color {
        line.spans
            .iter()
            .find_map(|span| span.style.fg)
            .expect("no drawn cell on this row")
    }

    fn brightness(colour: Color) -> u32 {
        match colour {
            Color::Rgb(r, g, b) => u32::from(r) + u32::from(g) + u32::from(b),
            other => panic!("true colour produced {other:?}"),
        }
    }

    #[test]
    fn the_mark_fits_the_terminal_it_is_drawn_in() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let wide = rows(&unicode, SWEEP_FRAMES, 80, true);
        assert_eq!(wide.len(), MARK_WIDE.len());
        assert_eq!(width(GlyphMode::Unicode, 80), MARK_WIDE[0].chars().count());
        assert!(
            wide.iter()
                .all(|line| line.width() == width(GlyphMode::Unicode, 80)),
            "a row of the mark is not the width the mark is centred on"
        );

        // A narrow terminal steps down rather than clipping: the mark is the
        // one thing on the page that must not lose a stroke to the pane edge.
        // Sixteen columns is the floor the new-session page itself renders at.
        assert_eq!(rows(&unicode, SWEEP_FRAMES, 18, true).len(), MARK_MID.len());
        assert_eq!(width(GlyphMode::Unicode, 18), MARK_MID[0].chars().count());
        assert!(rows(&unicode, SWEEP_FRAMES, 4, true).is_empty());

        // A console font has no block glyphs: the mark falls back to the three
        // characters such a font certainly has.
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        let art = rows(&ascii, SWEEP_FRAMES, 80, true);
        assert_eq!(art.len(), MARK_ASCII.len());
        assert!(
            art.iter().all(|line| line.to_string().is_ascii()),
            "{art:?}"
        );
    }

    #[test]
    fn the_mark_is_drawn_along_its_stroke() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let wide = &TIERS[0];
        let drawn = cells(wide);

        // The light starts at the top of the left arm ...
        let (_, first_row, first) = drawn
            .iter()
            .copied()
            .min_by(|a, b| a.2.total_cmp(&b.2))
            .expect("the mark has no cells");
        assert!(first.abs() < f32::EPSILON, "the mark starts at {first}");
        assert_eq!(first_row, 0);

        // ... and ends on the short right arm, not at the bottom of the long
        // one, which is the whole reason the wavefront follows the stroke.
        let (last_column, last_row, last) = drawn
            .iter()
            .copied()
            .max_by(|a, b| a.2.total_cmp(&b.2))
            .expect("the mark has no cells");
        assert_eq!(last_row, 0, "the light finished at the bottom of the mark");
        assert!(
            last_column * 2 >= wide.width(),
            "the light finished on the left arm at column {last_column}"
        );
        let left_arm_tip = cells(wide)
            .into_iter()
            .filter(|(column, _, _)| *column * 2 < wide.width())
            .map(|(_, _, along)| along)
            .fold(f32::NEG_INFINITY, f32::max);
        assert!(
            left_arm_tip < last,
            "the arms are not drawn in order: {left_arm_tip} then {last}"
        );

        // Halfway through, the mark is genuinely part-drawn rather than faded
        // in as a whole.
        let halfway = text_of(&rows(&unicode, REVEAL_FRAMES / 2, 80, true));
        let finished = text_of(&rows(&unicode, SWEEP_FRAMES, 80, true));
        let inked = |art: &[String]| {
            art.iter()
                .map(|line| line.trim().chars().count())
                .sum::<usize>()
        };
        assert!(
            inked(&halfway) > 0,
            "nothing was drawn by the halfway frame"
        );
        assert!(
            inked(&halfway) < inked(&finished),
            "the reveal added no cells: {halfway:?}"
        );
    }

    #[test]
    fn the_mark_comes_to_rest_every_time() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let art = text_of(&rows(&unicode, SWEEP_FRAMES, 80, true));
        // Past the pass the mark is still and whole: this is the state an
        // untouched client sits in for the rest of the session, so no frame
        // after the pass may differ from it.
        for phase in [SWEEP_FRAMES, SWEEP_FRAMES + 1, SWEEP_FRAMES + 997] {
            assert_eq!(
                text_of(&rows(&unicode, phase, 80, true)),
                art,
                "phase {phase}"
            );
        }
        // Every frame of the pass is a subset of the mark, so nothing is drawn
        // that the resting shape does not have.
        for phase in 0..SWEEP_FRAMES {
            let frame = rows(&unicode, phase, 80, true);
            for (row, line) in frame.iter().enumerate() {
                for (column, character) in line.to_string().chars().enumerate() {
                    if character != ' ' {
                        let resting = MARK_WIDE[row].chars().nth(column).unwrap_or(' ');
                        assert_ne!(resting, ' ', "phase {phase} drew outside the mark");
                    }
                }
            }
        }

        // The block the mark occupies is reserved from the first frame — same
        // rows, same widths — so the page under it does not jump as the mark
        // is drawn. Only the cells inside that block change.
        let resting_widths: Vec<usize> = rows(&unicode, SWEEP_FRAMES, 80, true)
            .iter()
            .map(|line| line.width())
            .collect();
        for phase in 0..SWEEP_FRAMES {
            let frame = rows(&unicode, phase, 80, true);
            assert_eq!(frame.len(), art.len(), "phase {phase} changed the height");
            assert_eq!(
                frame.iter().map(|line| line.width()).collect::<Vec<_>>(),
                resting_widths,
                "phase {phase} changed the width"
            );
        }

        // A page that is not waiting gets the resting mark, never a frozen
        // frame of the animation.
        assert_eq!(text_of(&rows(&unicode, 3, 80, false)), art);
    }

    #[test]
    fn the_light_spends_the_headroom_the_rest_state_keeps() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        // At rest the mark is a gradient, not a flat block: the top is a
        // different colour from the bottom.
        let resting = rows(&unicode, SWEEP_FRAMES, 80, true);
        let rest_top = first_colour(&resting[0]);
        let rest_bottom = first_colour(&resting[MARK_WIDE.len() - 1]);
        assert_ne!(rest_top, rest_bottom, "the mark rests flat");

        // Across the pass some cell gets brighter than the mark ever rests,
        // which it can only do because the rest state kept the headroom ...
        let mut brightest = brightness(rest_top);
        let mut tinted = false;
        for phase in 0..SWEEP_FRAMES {
            for line in rows(&unicode, phase, 80, true) {
                for span in line.spans {
                    let Some(colour) = span.style.fg else {
                        continue;
                    };
                    if span.content.trim().is_empty() {
                        continue;
                    }
                    let Color::Rgb(r, g, b) = colour else {
                        panic!("true colour produced {colour:?}")
                    };
                    // ... and does it in a colour rather than in a lighter
                    // grey, which is what stops the pass reading as a smudge.
                    tinted |= r != g || g != b;
                    brightest = brightest.max(brightness(colour));
                }
            }
        }
        assert!(
            brightest > brightness(rest_top),
            "the light never rose above the resting mark"
        );
        assert!(tinted, "the light never took a hue");
    }

    #[test]
    fn a_terminal_that_cannot_blend_gets_weight_instead() {
        // A shallow palette has no room for the ramp, so the light is carried
        // by modifiers, which is the only brightness it has. What it must not
        // do is draw the fade anyway and let the terminal quantise it into a
        // flicker.
        for mode in [ColorMode::Ansi16, ColorMode::Ansi256] {
            let theme = theme(mode, GlyphMode::Unicode);
            let lit = rows(&theme, REVEAL_FRAMES / 2, 80, true);
            assert!(
                lit.iter().any(|line| line
                    .spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::BOLD))),
                "{mode:?} did not mark the light"
            );
            // The resting mark still steps down its own greys, so the page is
            // not left with the flat block a gradient-less palette would give.
            let resting = rows(&theme, SWEEP_FRAMES, 80, true);
            assert_ne!(
                first_colour(&resting[0]),
                first_colour(&resting[MARK_WIDE.len() - 1]),
                "{mode:?} left the mark flat"
            );
        }

        // Colour switched off entirely is the floor: the mark is drawn whole
        // and still, because a light with neither colour nor weight to spend
        // would only flicker between two identical frames.
        let plain = theme(ColorMode::None, GlyphMode::Unicode);
        assert_eq!(
            text_of(&rows(&plain, REVEAL_FRAMES / 2, 80, true)),
            text_of(&rows(&plain, SWEEP_FRAMES, 80, true))
        );
        assert_eq!(
            text_of(&rows(&plain, 0, 80, true)),
            text_of(&rows(&plain, SWEEP_FRAMES, 80, true))
        );
    }
}

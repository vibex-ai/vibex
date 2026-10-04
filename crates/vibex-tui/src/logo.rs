//! The wordmark a reader lands on.
//!
//! The first screen of a client is the only one that is allowed to be a
//! *picture*: there is nothing to read yet, so the space is spent saying who
//! this is and what to do next rather than on an empty transcript. The mark is
//! drawn from text — a terminal has no image pipeline, and a mark made of
//! characters keeps its shape at every font size — and a light crosses it,
//! because movement is what says the client is alive and waiting and a static
//! block of glyphs cannot.
//!
//! # The light
//!
//! Two lights cross the word, and the word never changes while they do. The
//! first is a wide, soft wash that opens the page; the second, a full pause
//! later, is a narrow glint with a bright core. Both are one pass of
//! [`PASS_FRAMES`], which is the number the client counts to before it stops
//! repainting ([`crate::app::LANDING_SWEEP_FRAMES`]).
//!
//! Three things about the light are deliberate and easy to get wrong:
//!
//! * **it brightens the mark; it does not dim it.** The cells under the light
//!   are lit *in addition to* the mark's own colour, so the mark at rest has to
//!   keep headroom for the light to spend. A mark that rests at full strength
//!   has nowhere left to light up, and a band that mixes the mark toward the
//!   background — which is what this used to do — draws a shadow crossing the
//!   word rather than a light;
//! * **the light has a hue before it has a core.** A cell walks from its
//!   resting grey to the theme's cyan and only then to the foreground, so the
//!   middle of the pass is a coloured band rather than a lighter smudge;
//! * **the mark at rest is a gradient, not a flat block.** It is brightest at
//!   the top and steps down toward the bottom, which is both the headroom the
//!   closing glint spends and what keeps the page from looking like a wall of
//!   one colour once the animation is over.

use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};

use crate::theme::{ColorMode, GlyphMode, TuiTheme};

/// The wordmark, one string per row.
///
/// Six rows is the height that fits a short terminal without pushing the prompt
/// off the screen.
const WORDMARK: [&str; 6] = [
    "██╗   ██╗ ██╗ ██████╗  ███████╗ ██╗  ██╗",
    "██║   ██║ ██║ ██╔══██╗ ██╔════╝ ╚██╗██╔╝",
    "██║   ██║ ██║ ██████╔╝ █████╗    ╚███╔╝ ",
    "╚██╗ ██╔╝ ██║ ██╔══██╗ ██╔══╝    ██╔██╗ ",
    " ╚████╔╝  ██║ ██████╔╝ ███████╗ ██╔╝ ██╗",
    "  ╚═══╝   ╚═╝ ╚═════╝  ╚══════╝ ╚═╝  ╚═╝",
];

/// The mark for a terminal that cannot draw the block glyphs.
///
/// Two rows, plain ASCII, in the spirit of the mark: a V and the word. It is
/// not a translation of the art above — that would be six rows of noise in a
/// legacy console font — it is the same statement made smaller.
const WORDMARK_ASCII: [&str; 2] = ["V  V  i  b  e  x", " VV   vibex.dev"];

// ---------------------------------------------------------------------------
// The light
// ---------------------------------------------------------------------------

/// Frames the opening wash takes to cross the word.
const WASH_FRAMES: u32 = 20;

/// Columns either side of the wash's centre that it reaches across.
const WASH_WIDTH: f32 = 5.0;

/// How much light the wash carries at its centre.
///
/// Half of the ramp, which is exactly the hue: a wash is a colour passing over
/// the word, and the white core belongs to the glint alone.
const WASH_PEAK: f32 = 0.5;

/// The frame the closing glint enters on.
const GLINT_START: u32 = 38;

/// Frames the closing glint takes to cross the word and leave it.
const GLINT_FRAMES: u32 = 22;

/// Columns either side of the glint's centre that it reaches across.
const GLINT_WIDTH: f32 = 1.7;

/// Frames one whole landing animation takes: washed, settled, crossed.
///
/// The number the client repaints for and then stops at.
pub const PASS_FRAMES: u32 = GLINT_START + GLINT_FRAMES;

/// How far down the mark's own grey scale the resting bottom row sits.
///
/// The top row rests at `REST_HEAD` of the way there rather than at the
/// foreground, which is the headroom the lights spend.
const REST_HEAD: f32 = 0.25;

/// Above this much light a cell is drawn bold in a palette that cannot blend.
///
/// Below the wash's peak on purpose: a threshold above it would mean the
/// opening wash is never marked at all in a palette that has only weight to
/// spend, and the page would open on a still mark.
const LIT: f32 = 0.45;

/// The light's colour.
///
/// Cyan rather than the running rail's blue. Both mean "alive", but the landing
/// page is doing nothing at all, and the blue that means *a turn is running*
/// would say the wrong thing on it.
fn light_hue(theme: &TuiTheme) -> Color {
    theme.roles.accent_tool
}

/// Whether this terminal can be shown a light at all.
///
/// With colour switched off there is nothing to spend: a light drawn in one
/// colour, with no weight to carry it, is a mark that would only flicker
/// between two identical frames. The wordmark is still the mark; only the
/// movement is dropped.
fn lights_up(theme: &TuiTheme) -> bool {
    !matches!(theme.capability.mode, ColorMode::None)
}

/// The art a terminal of `available` columns is drawn with, if it fits.
///
/// A mark that cannot be drawn whole is not drawn at all: half a wordmark reads
/// as a rendering fault, while the lines under it say the product's name
/// anyway.
fn art(glyphs: GlyphMode, available: u16) -> Option<&'static [&'static str]> {
    let available = usize::from(available);
    let art: &'static [&'static str] = match glyphs {
        GlyphMode::Unicode => &WORDMARK,
        GlyphMode::Ascii => &WORDMARK_ASCII,
    };
    (available >= width_of(art) + 2).then_some(art)
}

fn width_of(art: &[&str]) -> usize {
    art.iter().map(|row| row.chars().count()).max().unwrap_or(0)
}

/// How wide the mark drawn into `available` columns is, or zero when the
/// terminal is too narrow to hold it.
pub fn width(glyphs: GlyphMode, available: u16) -> usize {
    art(glyphs, available).map(width_of).unwrap_or(0)
}

/// Rows of the mark, lit as if a light were crossing it.
///
/// `phase` advances a frame at a time and is not wrapped: past [`PASS_FRAMES`]
/// the mark is simply at rest, which is what keeps an idle client at zero
/// frames. `bright` is whether the page the mark sits on is waiting for input
/// at all; a mark on a page that is not is drawn at rest and never animated.
pub fn rows(theme: &TuiTheme, phase: u32, available: u16, bright: bool) -> Vec<Line<'static>> {
    let Some(art) = art(theme.glyphs(), available) else {
        return Vec::new();
    };
    let width = width_of(art);
    let height = art.len();
    let animating = bright && lights_up(theme) && phase < PASS_FRAMES;
    let wash = wash_position(phase, width);
    let glint = glint_position(phase, width);

    art.iter()
        .enumerate()
        .map(|(row, art)| {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(width);
            let mut column = 0usize;
            for character in art.chars() {
                if character == ' ' {
                    // A cell that is not part of the mark is a gap, not a dim
                    // letter.
                    spans.push(Span::raw(" "));
                } else {
                    let light = if animating {
                        light_at(column, wash, glint)
                    } else {
                        0.0
                    };
                    spans.push(Span::styled(
                        character.to_string(),
                        style_for(theme, row, height, light),
                    ));
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

/// Where the opening wash's centre is, in columns, or nowhere once it has left.
fn wash_position(phase: u32, width: usize) -> f32 {
    if phase >= WASH_FRAMES {
        return f32::NEG_INFINITY;
    }
    let crossed = phase as f32 / WASH_FRAMES as f32;
    // Enters from off the left edge and leaves past the right one, so the
    // frame the wash ends on is indistinguishable from rest.
    crossed * (width as f32 + 2.0 * WASH_WIDTH) - WASH_WIDTH
}

/// Where the closing glint's centre is, in columns, or nowhere before it starts.
fn glint_position(phase: u32, width: usize) -> f32 {
    if !(GLINT_START..PASS_FRAMES).contains(&phase) {
        return f32::NEG_INFINITY;
    }
    let crossed = (phase - GLINT_START) as f32 / GLINT_FRAMES as f32;
    crossed * (width as f32 + 8.0) - 4.0
}

/// How much light a cell carries: 0.0 at rest, 1.0 at the core of the glint.
fn light_at(column: usize, wash: f32, glint: f32) -> f32 {
    let column = column as f32;
    let washed = WASH_PEAK * (-((column - wash) / WASH_WIDTH).powi(2)).exp();
    let flashed = (-((column - glint) / GLINT_WIDTH).powi(2)).exp();
    washed.max(flashed)
}

/// The style one cell of the mark is drawn in.
///
/// A cell's colour is its resting weight walked toward the light and then
/// toward the foreground, so the glint has a core and a tinted fringe.
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
        let wide = rows(&unicode, PASS_FRAMES, 80, true);
        assert_eq!(wide.len(), WORDMARK.len());
        assert_eq!(width(GlyphMode::Unicode, 80), WORDMARK[0].chars().count());
        assert!(
            wide.iter()
                .all(|line| line.width() == width(GlyphMode::Unicode, 80)),
            "a row of the mark is not the width the mark is centred on"
        );

        // A pane too narrow for the word gets the lines under it and nothing
        // else, rather than half a wordmark.
        assert!(rows(&unicode, PASS_FRAMES, 30, true).is_empty());
        assert_eq!(width(GlyphMode::Unicode, 30), 0);

        // A console font has no block glyphs: the mark falls back to words
        // rather than to six rows of substitution characters.
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        let art = rows(&ascii, PASS_FRAMES, 80, true);
        assert_eq!(art.len(), WORDMARK_ASCII.len());
        assert!(
            art.iter().all(|line| line.to_string().is_ascii()),
            "{art:?}"
        );
    }

    #[test]
    fn the_light_crosses_the_letters_and_never_redraws_them() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let letters = |phase: u32| text_of(&rows(&unicode, phase, 80, true));
        let resting = letters(PASS_FRAMES);
        // Every frame spells the same word: a pass that revealed the letters
        // would make a page that is waiting look like a page that is loading.
        for phase in 0..=PASS_FRAMES {
            assert_eq!(letters(phase), resting, "phase {phase} redrew the mark");
        }
        // The lights move, whatever else is true.
        let lit = |phase: u32| format!("{:?}", rows(&unicode, phase, 80, true));
        assert_ne!(lit(0), lit(WASH_FRAMES - 1), "the wash did not move");
        assert_ne!(
            lit(WASH_FRAMES),
            lit(GLINT_START + GLINT_FRAMES / 2),
            "the glint did not move"
        );
    }

    #[test]
    fn the_mark_comes_to_rest_every_time() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let art = text_of(&rows(&unicode, PASS_FRAMES, 80, true));
        // Past the pass the mark is still: this is the state an untouched
        // client sits in for the rest of the session, so no frame after the
        // pass may differ from it, in colour or in letters.
        for phase in [PASS_FRAMES, PASS_FRAMES + 1, PASS_FRAMES + 997] {
            assert_eq!(
                format!("{:?}", rows(&unicode, phase, 80, true)),
                format!("{:?}", rows(&unicode, PASS_FRAMES, 80, true)),
                "phase {phase}"
            );
        }
        // A page that is not waiting gets the resting mark, never a frozen
        // frame of the animation.
        assert_eq!(text_of(&rows(&unicode, 3, 80, false)), art);
        assert_eq!(text_of(&rows(&unicode, 0, 80, false)), art);
    }

    #[test]
    fn the_light_spends_the_headroom_the_rest_state_keeps() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        // At rest the mark is a gradient, not a flat block: the top is a
        // different colour from the bottom.
        let resting = rows(&unicode, PASS_FRAMES, 80, true);
        let rest_top = first_colour(&resting[0]);
        let rest_bottom = first_colour(&resting[WORDMARK.len() - 1]);
        assert_ne!(rest_top, rest_bottom, "the mark rests flat");

        // Across the pass some cell gets brighter than the mark ever rests,
        // which it can only do because the rest state kept the headroom ...
        let mut brightest = brightness(rest_top);
        let mut tinted = false;
        for phase in 0..PASS_FRAMES {
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
            let lit = rows(&theme, WASH_FRAMES / 2, 80, true);
            assert!(
                lit.iter().any(|line| line
                    .spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::BOLD))),
                "{mode:?} did not mark the wash"
            );
            // The resting mark still steps down its own greys, so the page is
            // not left with the flat block a gradient-less palette would give.
            let resting = rows(&theme, PASS_FRAMES, 80, true);
            assert_ne!(
                first_colour(&resting[0]),
                first_colour(&resting[WORDMARK.len() - 1]),
                "{mode:?} left the mark flat"
            );
        }

        // Colour switched off entirely is the floor: the mark is drawn whole
        // and still, because a light with neither colour nor weight to spend
        // would only flicker between two identical frames.
        let plain = theme(ColorMode::None, GlyphMode::Unicode);
        let resting = format!("{:?}", rows(&plain, PASS_FRAMES, 80, true));
        for phase in [0, WASH_FRAMES / 2, GLINT_START + 4] {
            assert_eq!(
                format!("{:?}", rows(&plain, phase, 80, true)),
                resting,
                "a colourless terminal animated the mark at phase {phase}"
            );
        }
    }
}

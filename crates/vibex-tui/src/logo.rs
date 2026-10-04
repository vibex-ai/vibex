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
//! One soft band of silver-white crosses the word, pauses, and comes round
//! again. The word never changes while it does: a pass that drew the letters as
//! it went would make a page that is waiting look like a page that is loading.
//!
//! Three things about the light are deliberate and easy to get wrong:
//!
//! * **it brightens the mark; it does not dim it.** The cells under the light
//!   are lit *in addition to* the mark's own colour, so the mark at rest has to
//!   keep headroom for the light to spend. A mark that rests at full strength
//!   has nowhere left to light up, and a band that mixes the mark toward the
//!   background — which is what this used to do — draws a shadow crossing the
//!   word rather than a light over it;
//! * **the light is the canvas's opposite, not a hue.** Silver-white on a dark
//!   page and ink on a light one, which is the same statement twice: the
//!   strongest thing that can be drawn where it falls;
//! * **the loop has no seam, and no wasted frame.** The light starts and ends
//!   two widths off the word, so the first and last frames of a pass are the
//!   resting mark; and [`moving`] names exactly the frames that draw something
//!   other than rest, which is what lets the client run the loop's clock
//!   between passes without repainting for it
//!   ([`crate::app::App::advance_transcript_animation`]).

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

/// Frames one light takes to cross the word.
pub const SWEEP_FRAMES: u32 = 20;

/// Frames one whole loop takes: the crossing, and the quiet stretch after it.
///
/// The quiet stretch is most of the loop on purpose. A light that came round
/// again the moment it left would stop being something that happens to the page
/// and start being a flicker on it. At the tick the client runs while a light
/// is moving, and the slower one it runs between lights, a loop is about ten
/// seconds with the light itself taking a quarter of that.
pub const LOOP_FRAMES: u32 = 60;

/// Columns either side of the light's centre that it reaches across.
const WASH_WIDTH: f32 = 5.0;

/// How far down the mark's own grey scale the resting bottom row sits.
///
/// The top row rests at `REST_HEAD` of the way there rather than at the
/// foreground, which is the headroom the light spends.
const REST_HEAD: f32 = 0.25;

/// Above this much light a cell is drawn bold in a palette that cannot blend.
const LIT: f32 = 0.45;

/// The colour a light crossing the mark takes.
///
/// The catalogue has no token named for a light, and the one it carries for the
/// job is `border`: authored as the canvas's opposite in every shipped theme —
/// `#ffffff` in all ten dark ones, `#000000` in all ten light ones — which is
/// what light is, and what makes a specular read as a specular rather than as a
/// slightly lighter grey. On a light page that inverts to ink, which is the
/// same statement: the strongest thing that can be drawn on the canvas it falls
/// on. The token is named for hairlines because that is what it was authored
/// for; its value is what a light needs, and minting a second white here would
/// put a colour outside the catalogue that every theme would then have to be
/// checked against.
fn silver(theme: &TuiTheme) -> Color {
    theme.roles.border
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

/// Whether the mark at `phase` draws anything other than its resting self.
///
/// Every frame outside a pass is the resting mark exactly, and every frame
/// inside one differs from it. That is the contract the client's clock between
/// passes relies on: it can keep the loop turning without repainting for it.
/// The frames at either end of a pass differ from rest by a level or two of one
/// channel, because the band starts and ends two widths off the word rather
/// than on its edge — the difference is what a seam would be made of, and it is
/// why there is none to see.
pub const fn moving(phase: u32) -> bool {
    phase % LOOP_FRAMES < SWEEP_FRAMES
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
/// `phase` advances a frame at a time and is read through [`LOOP_FRAMES`], so
/// the light comes round again for as long as the page waits. `bright` is
/// whether the page the mark sits on is waiting for input at all; a mark on a
/// page that is not is drawn at rest and never animated.
pub fn rows(theme: &TuiTheme, phase: u32, available: u16, bright: bool) -> Vec<Line<'static>> {
    let Some(art) = art(theme.glyphs(), available) else {
        return Vec::new();
    };
    let width = width_of(art);
    let height = art.len();
    let animating = bright && lights_up(theme);
    let wash = wash_position(phase, width);

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
                        light_at(column, wash)
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

/// Where the light's centre is, in columns, or nowhere between passes.
fn wash_position(phase: u32, width: usize) -> f32 {
    let swept = phase % LOOP_FRAMES;
    if swept >= SWEEP_FRAMES {
        return f32::NEG_INFINITY;
    }
    let crossed = swept as f32 / SWEEP_FRAMES as f32;
    // Two widths off the left edge and two past the right one, so the band is
    // already dark where the word begins and dark again where it ends: the
    // first and last frames of a pass are the resting mark, and the loop has no
    // seam to see.
    crossed * (width as f32 + 4.0 * WASH_WIDTH) - 2.0 * WASH_WIDTH
}

/// How much light a cell carries: 0.0 at rest, 1.0 under the centre of a band.
fn light_at(column: usize, wash: f32) -> f32 {
    (-((column as f32 - wash) / WASH_WIDTH).powi(2)).exp()
}

/// The style one cell of the mark is drawn in.
///
/// A cell's colour is its resting weight walked toward the light.
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
    Style::default().fg(theme.blend(rest, silver(theme), light))
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

    fn light_theme() -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-light"),
            GpuiThemeMode::Light,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn text_of(lines: &[Line<'static>]) -> Vec<String> {
        lines.iter().map(|line| line.to_string()).collect()
    }

    fn painted(lines: &[Line<'static>]) -> String {
        format!("{lines:?}")
    }

    fn first_colour(line: &Line<'static>) -> Color {
        line.spans
            .iter()
            .find_map(|span| span.style.fg)
            .expect("no drawn cell on this row")
    }

    fn brightness(colour: Color) -> i32 {
        match colour {
            Color::Rgb(r, g, b) => i32::from(r) + i32::from(g) + i32::from(b),
            other => panic!("true colour produced {other:?}"),
        }
    }

    #[test]
    fn the_mark_fits_the_terminal_it_is_drawn_in() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let wide = rows(&unicode, 0, 80, true);
        assert_eq!(wide.len(), WORDMARK.len());
        assert_eq!(width(GlyphMode::Unicode, 80), WORDMARK[0].chars().count());
        assert!(
            wide.iter()
                .all(|line| line.width() == width(GlyphMode::Unicode, 80)),
            "a row of the mark is not the width the mark is centred on"
        );

        // A pane too narrow for the word gets the lines under it and nothing
        // else, rather than half a wordmark.
        assert!(rows(&unicode, 0, 30, true).is_empty());
        assert_eq!(width(GlyphMode::Unicode, 30), 0);

        // A console font has no block glyphs: the mark falls back to words
        // rather than to six rows of substitution characters.
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        let art = rows(&ascii, 0, 80, true);
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
        let resting = letters(LOOP_FRAMES);
        // Every frame of two whole loops spells the same word.
        for phase in 0..(2 * LOOP_FRAMES) {
            assert_eq!(letters(phase), resting, "phase {phase} redrew the mark");
        }
        // The light moves, whatever else is true.
        let lit = |phase: u32| painted(&rows(&unicode, phase, 80, true));
        assert_ne!(lit(0), lit(SWEEP_FRAMES / 2), "the light did not move");
        assert_ne!(
            lit(SWEEP_FRAMES / 2),
            lit(SWEEP_FRAMES - 1),
            "the light stopped halfway"
        );
    }

    #[test]
    fn the_loop_has_no_seam_and_no_wasted_frame() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        // The resting mark is read from the frame the light has just left on,
        // which is the first frame of the quiet stretch. A whole loop on from
        // the start of a pass is the start of the *next* pass, not rest.
        let resting = painted(&rows(&unicode, SWEEP_FRAMES, 80, true));
        for phase in SWEEP_FRAMES..LOOP_FRAMES {
            assert_eq!(
                painted(&rows(&unicode, phase, 80, true)),
                resting,
                "phase {phase} of the quiet stretch does not rest"
            );
        }
        // The loop comes round exactly: what the client draws at a phase and at
        // that phase plus a loop is the same picture.
        for phase in 0..LOOP_FRAMES {
            assert_eq!(
                painted(&rows(&unicode, phase, 80, true)),
                painted(&rows(&unicode, phase + LOOP_FRAMES, 80, true)),
                "phase {phase} does not repeat"
            );
        }
        // `moving` names the frames that differ from rest, in both directions:
        // a quiet frame that drew something would be a repaint the client asked
        // for and did not get, and a moving frame that drew the resting mark
        // would be a repaint it paid for and did not need.
        for phase in 0..(2 * LOOP_FRAMES) {
            let frame = painted(&rows(&unicode, phase, 80, true));
            if moving(phase) {
                assert_ne!(frame, resting, "phase {phase} moves but draws rest");
            } else {
                assert_eq!(frame, resting, "phase {phase} is quiet but draws");
            }
        }
    }

    #[test]
    fn the_light_is_the_canvas_s_opposite() {
        // Halfway through a pass the band's centre is on column twenty, so that
        // cell carries the light at full strength: the colour to read the light
        // off.
        let lit_cell = |theme: &TuiTheme| {
            rows(theme, SWEEP_FRAMES / 2, 80, true)[0].spans[20]
                .style
                .fg
                .expect("the cell is drawn")
        };
        let rest_top = |theme: &TuiTheme| first_colour(&rows(theme, SWEEP_FRAMES, 80, true)[0]);

        let dark = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        assert_eq!(
            lit_cell(&dark),
            dark.roles.border,
            "the light is not the colour the catalogue keeps for it"
        );
        assert!(
            brightness(lit_cell(&dark)) > brightness(rest_top(&dark)),
            "the light does not rise above the resting mark"
        );
        // The mark rests as a gradient rather than a flat block, which is the
        // headroom that light is spending.
        let resting = rows(&dark, SWEEP_FRAMES, 80, true);
        assert_ne!(
            first_colour(&resting[0]),
            first_colour(&resting[WORDMARK.len() - 1]),
            "the mark rests flat"
        );

        // On a light page the same ramp is ink: a light cannot be brighter than
        // the paper it falls on, so the strongest thing that can be drawn there
        // is the darkest.
        let light = light_theme();
        assert_eq!(lit_cell(&light), light.roles.border);
        assert!(
            brightness(lit_cell(&light)) < brightness(rest_top(&light)),
            "the light on a light page does not darken the mark"
        );
    }

    #[test]
    fn a_terminal_that_cannot_blend_gets_weight_instead() {
        // A shallow palette has no room for the ramp, so the light is carried
        // by modifiers, which is the only brightness it has. What it must not
        // do is draw the fade anyway and let the terminal quantise it into a
        // flicker.
        for mode in [ColorMode::Ansi16, ColorMode::Ansi256] {
            let theme = theme(mode, GlyphMode::Unicode);
            let lit = rows(&theme, SWEEP_FRAMES / 2, 80, true);
            assert!(
                lit.iter().any(|line| line
                    .spans
                    .iter()
                    .any(|span| span.style.add_modifier.contains(Modifier::BOLD))),
                "{mode:?} did not mark the light"
            );
            // The resting mark still steps down its own greys, so the page is
            // not left with the flat block a gradient-less palette would give.
            let resting = rows(&theme, LOOP_FRAMES, 80, true);
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
        let resting = painted(&rows(&plain, LOOP_FRAMES, 80, true));
        for phase in [0, SWEEP_FRAMES / 2, SWEEP_FRAMES + 3] {
            assert_eq!(
                painted(&rows(&plain, phase, 80, true)),
                resting,
                "a colourless terminal animated the mark at phase {phase}"
            );
        }
    }
}

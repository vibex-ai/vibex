//! The wordmark a reader lands on.
//!
//! The first screen of a client is the only one that is allowed to be a
//! *picture*: there is nothing to read yet, so the space is spent saying who
//! this is and what to do next rather than on an empty transcript. The mark is
//! drawn from text, and a light sweeps across it — the movement is what says
//! the client is alive and waiting, which a static block of glyphs cannot.

use ratatui::style::Style;
use ratatui::text::{Line, Span};

use crate::theme::{GlyphMode, TuiTheme};

/// The wordmark, one string per row.
///
/// Block-drawing glyphs rather than the real logo: a terminal has no image
/// pipeline, and a mark drawn from characters keeps its shape at every font
/// size. Six rows is the height that fits a short terminal without pushing the
/// prompt off the screen.
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

/// How wide the art is, so a caller can decide whether it fits.
pub fn width(tier: GlyphMode) -> usize {
    match tier {
        GlyphMode::Unicode => WORDMARK.iter().map(|row| row.chars().count()).max(),
        GlyphMode::Ascii => WORDMARK_ASCII.iter().map(|row| row.chars().count()).max(),
    }
    .unwrap_or(0)
}

/// Rows of the mark, lit as if a light were sweeping across it.
///
/// `phase` advances a frame at a time; the sweep is a soft band that moves
/// left to right and wraps. Every cell takes its brightness from how far it is
/// from the band, so the mark reads as one object being lit rather than as a
/// cursor travelling over it. A terminal without true colour gets the mark at
/// full strength: a fade it cannot draw would be a mark that never lights up.
pub fn rows(theme: &TuiTheme, phase: u32, bright: bool) -> Vec<Line<'static>> {
    let art: Vec<&str> = match theme.glyphs() {
        GlyphMode::Unicode => WORDMARK.to_vec(),
        GlyphMode::Ascii => WORDMARK_ASCII.to_vec(),
    };
    let columns = width(theme.glyphs()).max(1);
    // One sweep every sixty frames: slow enough to read as light, quick enough
    // that a reader who looks twice sees it move.
    let sweep = (phase % 60) as f32 / 60.0 * (columns as f32 + 16.0) - 8.0;
    let base = theme.roles.foreground;
    let lit = theme.roles.accent_user;
    art.into_iter()
        .map(|row| {
            let mut spans: Vec<Span<'static>> = Vec::with_capacity(row.chars().count());
            for (index, character) in row.chars().enumerate() {
                // A cell that is not part of the mark is a gap, not a dim letter.
                if character == ' ' {
                    spans.push(Span::raw(" "));
                    continue;
                }
                let distance = (index as f32 - sweep).abs();
                // Six columns of falloff either side of the band.
                let near = (1.0 - distance / 6.0).clamp(0.0, 1.0);
                let colour = if !bright || near <= 0.0 {
                    base
                } else {
                    theme.fade(lit, 0.35 + near * 0.65)
                };
                spans.push(Span::styled(
                    character.to_string(),
                    Style::default().fg(colour),
                ));
            }
            Line::from(spans)
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorCapability, ColorMode};
    use vibex_ui::GpuiThemeMode;

    fn theme(mode: ColorMode, glyphs: GlyphMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability { mode, glyphs },
        )
    }

    #[test]
    fn the_mark_fits_the_terminal_it_is_drawn_in() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        assert_eq!(rows(&unicode, 0, true).len(), WORDMARK.len());
        assert!(
            rows(&unicode, 0, true)
                .iter()
                .all(|line| line.width() == width(GlyphMode::Unicode))
        );

        // A console font has no block glyphs: the mark falls back to words
        // rather than to six rows of substitution characters.
        let ascii = theme(ColorMode::TrueColor, GlyphMode::Ascii);
        let art = rows(&ascii, 0, true);
        assert_eq!(art.len(), WORDMARK_ASCII.len());
        assert!(
            art.iter().all(|line| line.to_string().is_ascii()),
            "{art:?}"
        );
    }

    #[test]
    fn the_sweep_moves_the_light_and_not_the_letters() {
        let unicode = theme(ColorMode::TrueColor, GlyphMode::Unicode);
        let letters = |phase: u32| {
            rows(&unicode, phase, true)
                .iter()
                .map(|line| line.to_string())
                .collect::<Vec<_>>()
        };
        assert_eq!(letters(0), letters(37), "the letters changed");
        let lit = |phase: u32| format!("{:?}", rows(&unicode, phase, true));
        assert_ne!(lit(0), lit(15), "the light did not move");

        // Every phase draws the same shape, so nothing depends on where the
        // sweep happens to be when the frame is taken.
        for phase in 0..120 {
            assert_eq!(letters(phase), letters(0), "phase {phase} redrew the mark");
        }
    }

    #[test]
    fn a_terminal_that_cannot_fade_gets_the_mark_at_full_strength() {
        // `fade` has nothing to blend with in a sixteen-colour terminal, so the
        // mark stays legible instead of turning into a gradient of one colour.
        for mode in [ColorMode::Ansi16, ColorMode::None] {
            let theme = theme(mode, GlyphMode::Unicode);
            let lit = rows(&theme, 20, true);
            let plain = rows(&theme, 20, false);
            // The mark is still the mark ...
            assert!(
                lit.iter().any(|line| line.to_string().contains('█')),
                "the mark lost its glyphs"
            );
            // ... and a terminal that cannot blend draws it one colour, which
            // is the honest degradation: a sweep it cannot show would be a mark
            // that flickers between two identical frames.
            assert_eq!(
                format!("{lit:?}"),
                format!("{plain:?}"),
                "{mode:?} drew a fade it cannot show"
            );
        }
    }
}

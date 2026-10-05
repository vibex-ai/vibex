//! The chrome glyph vocabulary.
//!
//! Every glyph the interface draws for structure comes from here, for two
//! reasons: one place to change a symbol, and one place to decide what a
//! terminal can actually render.
//!
//! Terminals disagree about far more than colour. The conservative Windows
//! console does no font fallback at all, so a glyph outside its raster font
//! renders as tofu; a terminal in a non-UTF-8 locale cannot encode box drawing
//! either. Each entry therefore declares its fallback and the width invariant it
//! must keep, because a glyph that changes width shifts everything after it and
//! turns a cosmetic fallback into a layout bug.
//!
//! Widths are part of the contract: [`PROMPT_ARROW`] is always two columns, the
//! spinners are always one. That is what lets the renderer reserve a column
//! without asking which glyph was chosen.

use crate::theme::{GlyphMode, TuiTheme};

/// How much of the vocabulary the terminal can render.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphTier {
    /// Geometric shapes, box drawing and arrows are all available.
    Full,
    /// Only characters a legacy console font has: ASCII and CP437.
    Legacy,
}

impl GlyphTier {
    pub const fn of(theme: &TuiTheme) -> Self {
        match theme.glyphs() {
            GlyphMode::Unicode => Self::Full,
            GlyphMode::Ascii => Self::Legacy,
        }
    }
}

/// The prompt's leading arrow. Always two columns.
pub fn prompt_arrow(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "❯ ",
        GlyphTier::Legacy => "> ",
    }
}

/// Width of [`prompt_arrow`] in columns.
pub const PROMPT_ARROW_WIDTH: usize = 2;

/// The heavy vertical used for an accent rail drawn as a line. One column.
pub fn accent_bar(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "┃",
        GlyphTier::Legacy => "|",
    }
}

/// Failure and close markers. One column.
pub fn ballot_x(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "✗",
        GlyphTier::Legacy => "x",
    }
}

/// Completion markers. One column.
pub fn check_mark(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "✓",
        GlyphTier::Legacy => "v",
    }
}

/// The arrow preceding a token count. One column.
pub fn token_arrow(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "⇣",
        GlyphTier::Legacy => "v",
    }
}

/// A filled diamond, used for the current item and for done steps. One column.
pub fn diamond_filled(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◆",
        GlyphTier::Legacy => "*",
    }
}

/// A hollow diamond, used for idle markers. One column.
pub fn diamond_hollow(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◇",
        GlyphTier::Legacy => "o",
    }
}

/// A dotted diamond, used for queued markers. One column.
pub fn diamond_dotted(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "◈",
        GlyphTier::Legacy => "+",
    }
}

/// Disclosure markers for a collapsible region. One column.
pub fn disclosure(open: bool, tier: GlyphTier) -> &'static str {
    match (open, tier) {
        (true, GlyphTier::Full) => "▾",
        (false, GlyphTier::Full) => "▸",
        (true, GlyphTier::Legacy) => "v",
        (false, GlyphTier::Legacy) => ">",
    }
}

/// The marker beside a row the reader has pinned. One column.
///
/// A star rather than a flag: this column already carries the Agent's letter in
/// the next cell, and a flag beside a letter read as two pieces of the same
/// word. The star is the shape every list uses for "kept", it is one cell in
/// the same width class as the other marks, and at the legacy tier it degrades
/// to the ASCII star.
pub fn pin_marker(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "★",
        GlyphTier::Legacy => "*",
    }
}

/// A session with something new in it. One column.
///
/// Distinct from the pin in shape rather than only in colour: the two sit in
/// the same column, and a reader who cannot tell the colours apart still has to
/// tell a kept session from an unread one.
pub fn unread_marker(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "●",
        GlyphTier::Legacy => "o",
    }
}

/// The mark beside a session's state. One column.
///
/// Colour carries the same distinction, but the shape does not depend on it: a
/// failed session and a running one are told apart on a monochrome terminal.
pub fn state_marker(state: &str, tier: GlyphTier) -> &'static str {
    let unicode = tier == GlyphTier::Full;
    match state {
        "running" => {
            if unicode {
                "▶"
            } else {
                ">"
            }
        }
        "failed" => {
            if unicode {
                "✗"
            } else {
                "x"
            }
        }
        "waiting" => {
            if unicode {
                "◆"
            } else {
                "*"
            }
        }
        "archived" => {
            if unicode {
                "▤"
            } else {
                "#"
            }
        }
        _ => {
            if unicode {
                "·"
            } else {
                "."
            }
        }
    }
}

/// The control that renames the row it sits on. One column.
///
/// The mark is a single pen stroke rather than a drawn pencil: the pencil
/// dingbat is missing from every console font and falls back to a serif
/// typeface, where it renders as a busy picture at one cell. A box-drawing
/// diagonal is in the same family as the heading rule, so a terminal that can
/// draw the list can draw the control. At the legacy tier the letter is the
/// action's own initial, which is also the chord that runs it.
pub fn rename_icon(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "╱",
        GlyphTier::Legacy => "r",
    }
}

/// The control that deletes the row it sits on. One column.
///
/// A cross rather than the failed session's `✗`: the state mark already owns
/// that shape on the same row, and two spellings of "bad" in one line read as
/// one fact. At the legacy tier the letter is the action's own initial.
pub fn delete_icon(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "✕",
        GlyphTier::Legacy => "d",
    }
}

/// The rule a section heading trails to the edge of its list. One column.
///
/// It is a box-drawing character, so a legacy console gets the hyphen rather
/// than a blank: the rule is what turns a stack of rows into sections, and a
/// heading with no rule is one more line of text.
pub fn heading_rule(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "─",
        GlyphTier::Legacy => "-",
    }
}

/// A chevron pointing at what is not shown yet. One column.
///
/// It survives the turn rail it was written for, which no longer jumps a turn
/// at a time: the dock's "and N more" row points down with it.
pub fn chevron(up: bool, tier: GlyphTier) -> &'static str {
    match (up, tier) {
        (true, GlyphTier::Full) => "▴",
        (false, GlyphTier::Full) => "▾",
        (true, GlyphTier::Legacy) => "^",
        (false, GlyphTier::Legacy) => "v",
    }
}

/// The turn marker on the timeline rail. One column.
pub fn timeline_tick(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "•",
        GlyphTier::Legacy => ".",
    }
}

/// The heavier tick marking the turn the viewport is on. One column.
pub fn timeline_tick_active(tier: GlyphTier) -> &'static str {
    match tier {
        GlyphTier::Full => "▪",
        GlyphTier::Legacy => "#",
    }
}

/// Braille spinner frames, one column each.
///
/// Braille is absent from a legacy console font, so the fallback is the classic
/// four-step ASCII spinner rather than a substitute shape.
pub fn spinner_frames(tier: GlyphTier) -> &'static [&'static str] {
    const FULL: &[&str] = &["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧"];
    const LEGACY: &[&str] = &["|", "/", "-", "\\"];
    match tier {
        GlyphTier::Full => FULL,
        GlyphTier::Legacy => LEGACY,
    }
}

/// The idle pulse: a ring that fills and empties. One column each.
///
/// Used for a session that is alive but not working, so the interface still
/// reads as connected without claiming activity that is not happening.
pub fn idle_pulse_frames(tier: GlyphTier) -> &'static [&'static str] {
    const FULL: &[&str] = &["○", "◎", "◉", "◎"];
    const LEGACY: &[&str] = &["o", "O", "0", "O"];
    match tier {
        GlyphTier::Full => FULL,
        GlyphTier::Legacy => LEGACY,
    }
}

/// How many ticks of the animation clock each spinner frame is held.
///
/// A spinner that changes every frame at 120ms is a flicker; holding each frame
/// for a few ticks reads as rotation.
pub const SPINNER_TICKS_PER_FRAME: u32 = 3;

/// How many ticks each idle-pulse frame is held.
///
/// The idle pulse is deliberately slower than the working spinner: it says
/// "alive", not "busy".
pub const IDLE_PULSE_TICKS_PER_FRAME: u32 = 6;

/// Pick a frame from a cycle held for `ticks_per_frame`.
pub fn frame_at<'a>(frames: &'a [&'a str], phase: u32, ticks_per_frame: u32) -> &'a str {
    if frames.is_empty() {
        return " ";
    }
    let index = (phase / ticks_per_frame.max(1)) as usize % frames.len();
    frames[index]
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::theme::{ColorCapability, ColorMode, TuiTheme};
    use vibex_ui::GpuiThemeMode;

    fn theme(glyphs: GlyphMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs,
            },
        )
    }

    fn full() -> GlyphTier {
        GlyphTier::of(&theme(GlyphMode::Unicode))
    }

    fn legacy() -> GlyphTier {
        GlyphTier::of(&theme(GlyphMode::Ascii))
    }

    #[test]
    fn the_prompt_arrow_is_always_two_columns() {
        // The renderer reserves this many columns without asking which glyph
        // was chosen, so a fallback that changed width would shift the layout.
        for tier in [full(), legacy()] {
            assert_eq!(
                crate::text::display_width(prompt_arrow(tier)),
                PROMPT_ARROW_WIDTH
            );
        }
    }

    #[test]
    fn single_column_glyphs_stay_single_column() {
        for tier in [full(), legacy()] {
            for glyph in [
                accent_bar(tier),
                ballot_x(tier),
                check_mark(tier),
                token_arrow(tier),
                diamond_filled(tier),
                diamond_hollow(tier),
                diamond_dotted(tier),
                disclosure(true, tier),
                disclosure(false, tier),
                chevron(true, tier),
                chevron(false, tier),
                timeline_tick(tier),
                timeline_tick_active(tier),
                heading_rule(tier),
                rename_icon(tier),
                delete_icon(tier),
            ] {
                assert_eq!(
                    crate::text::display_width(glyph),
                    1,
                    "{glyph:?} is not one column"
                );
            }
        }
    }

    #[test]
    fn every_spinner_frame_is_one_column_in_both_tiers() {
        // A spinner frame wider than its neighbours would jitter the label.
        for tier in [full(), legacy()] {
            for frame in spinner_frames(tier).iter().chain(idle_pulse_frames(tier)) {
                assert_eq!(
                    crate::text::display_width(frame),
                    1,
                    "{frame:?} is not one column"
                );
            }
        }
    }

    #[test]
    fn the_legacy_tier_avoids_characters_its_font_lacks() {
        // A legacy console font has ASCII and some CP437; box drawing, Braille
        // and the small geometric shapes are not in it.
        let forbidden = [
            "│", "┃", "▏", "▌", "▾", "▴", "•", "▪", "⠋", "⠙", "○", "◎", "◉", "◆", "◇", "◈", "✗",
            "✓", "⇣", "❯", "╭", "╰", "─",
        ];
        let tier = legacy();
        let mut drawn = vec![
            prompt_arrow(tier),
            accent_bar(tier),
            ballot_x(tier),
            check_mark(tier),
            token_arrow(tier),
            diamond_filled(tier),
            diamond_hollow(tier),
            diamond_dotted(tier),
            disclosure(true, tier),
            disclosure(false, tier),
            chevron(true, tier),
            chevron(false, tier),
            timeline_tick(tier),
            timeline_tick_active(tier),
            heading_rule(tier),
            rename_icon(tier),
            delete_icon(tier),
        ];
        drawn.extend_from_slice(spinner_frames(tier));
        drawn.extend_from_slice(idle_pulse_frames(tier));
        for glyph in drawn {
            for character in forbidden {
                assert!(
                    !glyph.contains(character),
                    "{glyph:?} contains {character:?}, which a legacy console cannot render"
                );
            }
        }
    }

    #[test]
    fn frame_selection_cycles_and_holds() {
        let frames = &["a", "b", "c"];
        assert_eq!(frame_at(frames, 0, 2), "a");
        assert_eq!(frame_at(frames, 1, 2), "a");
        assert_eq!(frame_at(frames, 2, 2), "b");
        assert_eq!(frame_at(frames, 6, 2), "a");
        // A zero or absurd hold must still terminate rather than divide by zero.
        assert_eq!(frame_at(frames, 5, 0), "c");
        assert_eq!(frame_at(&[], 5, 2), " ");
    }

    #[test]
    fn the_idle_pulse_is_slower_than_the_working_spinner() {
        // Idle says "alive"; working says "busy". The same cadence for both
        // would make an idle session look like it was doing something.
        const { assert!(IDLE_PULSE_TICKS_PER_FRAME > SPINNER_TICKS_PER_FRAME) };
    }
}

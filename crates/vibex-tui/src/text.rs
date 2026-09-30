//! Width-correct text measurement, wrapping and bidi reordering.
//!
//! Two rules drive this module:
//!
//! 1. **Column count is never `chars().count()`.** Every width decision goes
//!    through `unicode-width` so CJK, Hangul and emoji occupy the columns the
//!    terminal actually advances.
//! 2. **A wrapped line remembers how to un-wrap.** Each produced line carries a
//!    [`LineJoiner`] describing the whitespace that was consumed, so copying a
//!    selection back out reproduces the original prose instead of a wall of
//!    broken fragments.
//!
//! Bidi text is reordered for display only. Stored text is never rewritten —
//! the logical order stays intact in the model and in anything copied out.

use unicode_segmentation::UnicodeSegmentation;
use unicode_width::UnicodeWidthStr;

/// How a line was separated from the following one during wrapping.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LineJoiner {
    /// The line ended because the text ended.
    End,
    /// A single space was consumed at the break.
    Space,
    /// The line broke without consuming whitespace (CJK, or a hard break).
    None,
    /// An explicit newline in the source.
    Newline,
    /// A word longer than the available width had to be split.
    Hyphen,
}

impl LineJoiner {
    /// Whether re-joining should insert a space.
    pub const fn inserts_space(self) -> bool {
        matches!(self, Self::Space)
    }
}

/// One display line produced by [`wrap_text`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WrappedLine {
    /// The logical text of this line, already bidi-reordered for display.
    pub text: String,
    /// Display columns the line occupies.
    pub width: usize,
    pub joiner: LineJoiner,
    /// Byte offset of this line's first character in the source string.
    pub source_start: usize,
}

impl WrappedLine {
    pub fn is_blank(&self) -> bool {
        self.text.trim_end().is_empty()
    }
}

/// Display width of a string in terminal columns.
pub fn display_width(text: &str) -> usize {
    // Tabs are expanded by the renderer, not measured here; treat them as one
    // column so a stray tab cannot silently shift a layout calculation.
    UnicodeWidthStr::width(text.replace('\t', " ").as_str())
}

/// Truncate to `max` columns, appending `ellipsis` when content was dropped.
pub fn truncate_to_width(text: &str, max: usize, ellipsis: &str) -> String {
    if max == 0 {
        return String::new();
    }
    if display_width(text) <= max {
        return text.to_string();
    }
    let ellipsis_width = display_width(ellipsis);
    if ellipsis_width >= max {
        // Not even the ellipsis fits; take the widest prefix of the ellipsis.
        return take_width(ellipsis, max).0;
    }
    let (prefix, _) = take_width(text, max - ellipsis_width);
    let mut output = prefix.trim_end().to_string();
    output.push_str(ellipsis);
    output
}

/// Split a string at the byte boundary that fills `max` columns.
///
/// Returns the prefix and the remaining suffix.
pub fn take_width(text: &str, max: usize) -> (String, &str) {
    let mut width = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);
        if width + grapheme_width > max {
            return (text[..offset].to_string(), &text[offset..]);
        }
        width += grapheme_width;
        if width == max {
            let end = offset + grapheme.len();
            return (text[..end].to_string(), &text[end..]);
        }
    }
    (text.to_string(), "")
}

/// Pad to exactly `width` columns.
pub fn pad_to_width(text: &str, width: usize) -> String {
    let current = display_width(text);
    if current >= width {
        return truncate_to_width(text, width, "");
    }
    let mut output = String::with_capacity(text.len() + (width - current));
    output.push_str(text);
    output.extend(std::iter::repeat_n(' ', width - current));
    output
}

/// A grapheme that can be broken after without a space.
fn is_breakable_after(grapheme: &str) -> bool {
    grapheme.chars().any(|character| {
        matches!(character,
            '\u{3000}'..='\u{303F}'   // CJK symbols and punctuation
            | '\u{3040}'..='\u{30FF}' // kana
            | '\u{3400}'..='\u{4DBF}' // CJK extension A
            | '\u{4E00}'..='\u{9FFF}' // CJK unified ideographs
            | '\u{AC00}'..='\u{D7AF}' // Hangul syllables
            | '\u{F900}'..='\u{FAFF}' // CJK compatibility ideographs
            | '\u{FF00}'..='\u{FFEF}' // half/full-width forms
        )
    })
}

fn is_space(grapheme: &str) -> bool {
    grapheme.chars().all(char::is_whitespace)
}

/// Wrap `text` to `width` columns.
///
/// Wrapping is word-based for space-separated scripts and grapheme-based for
/// CJK, so a Chinese paragraph breaks at a sensible column instead of
/// overflowing.
pub fn wrap_text(text: &str, width: usize) -> Vec<WrappedLine> {
    wrap_text_inner(text, width, 0)
}

/// Wrap the tail of a longer document, offsetting the recorded source spans.
pub fn wrap_text_at(text: &str, width: usize, source_offset: usize) -> Vec<WrappedLine> {
    wrap_text_inner(text, width, source_offset)
}

fn wrap_text_inner(text: &str, width: usize, source_offset: usize) -> Vec<WrappedLine> {
    let width = width.max(1);
    let mut lines = Vec::new();
    let mut logical_start = 0usize;

    for (paragraph_index, paragraph) in text.split('\n').enumerate() {
        let paragraph_offset = source_offset
            + text
                .split('\n')
                .take(paragraph_index)
                .map(|part| part.len() + 1)
                .sum::<usize>();
        wrap_paragraph(
            paragraph,
            width,
            paragraph_offset,
            &mut lines,
            logical_start,
        );
        logical_start += paragraph.len() + 1;
    }

    if lines.is_empty() {
        lines.push(WrappedLine {
            text: String::new(),
            width: 0,
            joiner: LineJoiner::End,
            source_start: source_offset,
        });
    }

    // The final line never joins to anything.
    if let Some(last) = lines.last_mut()
        && last.joiner == LineJoiner::Newline
    {
        last.joiner = LineJoiner::End;
    }
    lines
}

/// One unbreakable unit of a paragraph.
///
/// Space-separated scripts produce word tokens; CJK produces one token per
/// ideograph, because a Chinese paragraph has no spaces to break on and must
/// still wrap at the column limit.
struct Token {
    text: String,
    width: usize,
    /// Byte offset of the token in the paragraph.
    offset: usize,
    /// Whether a space preceded it, which is what re-joining needs to know.
    after_space: bool,
}

fn tokenize(paragraph: &str) -> Vec<Token> {
    let mut tokens = Vec::new();
    let mut current = String::new();
    let mut current_offset = 0usize;
    let mut offset = 0usize;
    let mut pending_space = false;

    let flush = |current: &mut String,
                 current_offset: usize,
                 after_space: bool,
                 tokens: &mut Vec<Token>| {
        if current.is_empty() {
            return;
        }
        let text = std::mem::take(current);
        tokens.push(Token {
            width: UnicodeWidthStr::width(text.as_str()),
            text,
            offset: current_offset,
            after_space,
        });
    };

    for grapheme in paragraph.graphemes(true) {
        if is_space(grapheme) {
            flush(&mut current, current_offset, pending_space, &mut tokens);
            pending_space = true;
            offset += grapheme.len();
            continue;
        }
        if current.is_empty() {
            current_offset = offset;
        }
        // A CJK ideograph is its own break opportunity, so it ends the token it
        // was accumulating and stands alone.
        if is_breakable_after(grapheme) {
            flush(&mut current, current_offset, pending_space, &mut tokens);
            let after_space = pending_space;
            pending_space = false;
            tokens.push(Token {
                text: grapheme.to_string(),
                width: UnicodeWidthStr::width(grapheme),
                offset,
                after_space,
            });
            offset += grapheme.len();
            continue;
        }
        current.push_str(grapheme);
        offset += grapheme.len();
    }
    flush(&mut current, current_offset, pending_space, &mut tokens);
    tokens
}

/// Split a token that is wider than the whole line.
fn hard_split(token: &Token, width: usize) -> Vec<Token> {
    let mut pieces = Vec::new();
    let mut rest = token.text.as_str();
    let mut offset = token.offset;
    while !rest.is_empty() {
        let (prefix, suffix) = take_width(rest, width);
        if prefix.is_empty() {
            break;
        }
        pieces.push(Token {
            width: UnicodeWidthStr::width(prefix.as_str()),
            offset,
            text: prefix,
            after_space: pieces.is_empty() && token.after_space,
        });
        offset += rest.len() - suffix.len();
        rest = suffix;
    }
    pieces
}

fn wrap_paragraph(
    paragraph: &str,
    width: usize,
    paragraph_offset: usize,
    lines: &mut Vec<WrappedLine>,
    _logical_start: usize,
) {
    if paragraph.is_empty() {
        lines.push(WrappedLine {
            text: String::new(),
            width: 0,
            joiner: LineJoiner::Newline,
            source_start: paragraph_offset,
        });
        return;
    }

    let tokens = tokenize(paragraph);
    let mut current = String::new();
    let mut current_width = 0usize;
    let mut line_start = 0usize;
    let mut started = false;

    for token in tokens {
        // A token wider than the line has to be split; everything else moves to
        // the next line whole, because breaking a word to save two columns
        // reads as a rendering fault.
        let pieces = if token.width > width {
            hard_split(&token, width)
        } else {
            vec![Token {
                text: token.text,
                width: token.width,
                offset: token.offset,
                after_space: token.after_space,
            }]
        };
        for piece in pieces {
            // Only a space that was in the source becomes a separator; a CJK
            // token follows its predecessor with no gap, and adding one would
            // invent punctuation the author did not write.
            let mut space = piece.after_space && current_width > 0;
            if started && current_width + usize::from(space) + piece.width > width {
                lines.push(WrappedLine {
                    text: std::mem::take(&mut current),
                    width: current_width,
                    joiner: if space {
                        LineJoiner::Space
                    } else {
                        LineJoiner::None
                    },
                    source_start: line_start,
                });
                current_width = 0;
                started = false;
                // The break consumed the space.
                space = false;
            }
            if !started {
                line_start = paragraph_offset + piece.offset;
                started = true;
            }
            if space {
                current.push(' ');
                current_width += 1;
            }
            current.push_str(&piece.text);
            current_width += piece.width;
        }
    }

    if !current.is_empty() {
        lines.push(WrappedLine {
            text: current,
            width: current_width,
            joiner: LineJoiner::Newline,
            source_start: line_start,
        });
    } else if let Some(last) = lines.last_mut()
        && matches!(last.joiner, LineJoiner::None | LineJoiner::Space)
    {
        last.joiner = LineJoiner::Newline;
    }
}

/// Reorder a single visual line for display according to the bidi algorithm.
///
/// Returns the text in visual order. The input is never modified, and callers
/// that copy text out must copy the *logical* string, not this projection.
pub fn visual_order(text: &str) -> String {
    use unicode_bidi::BidiInfo;
    if text.is_empty() {
        return String::new();
    }
    let info = BidiInfo::new(text, None);
    let Some(paragraph) = info.paragraphs.first() else {
        return text.to_string();
    };
    // Fast path: pure LTR (the overwhelming majority of lines) needs no work.
    if paragraph.level.is_ltr() {
        return text.to_string();
    }
    let (levels, runs) = info.visual_runs(paragraph, paragraph.range.clone());
    let mut output = String::with_capacity(text.len());
    for run in runs {
        let slice = &text[run.clone()];
        let level = levels[run.start];
        if level.is_rtl() {
            output.extend(slice.chars().rev());
        } else {
            output.push_str(slice);
        }
    }
    output
}

/// Whether a string contains any right-to-left script.
pub fn contains_rtl(text: &str) -> bool {
    text.chars().any(|character| {
        matches!(character as u32,
            0x0590..=0x05FF   // Hebrew
            | 0x0600..=0x06FF // Arabic
            | 0x0700..=0x074F // Syriac
            | 0x0750..=0x077F // Arabic supplement
            | 0x08A0..=0x08FF // Arabic extended-A
            | 0xFB1D..=0xFB4F // Hebrew presentation forms
            | 0xFB50..=0xFDFF // Arabic presentation forms-A
            | 0xFE70..=0xFEFF // Arabic presentation forms-B
        )
    })
}

/// Format a Unix millisecond timestamp as a UTC civil date and time.
///
/// The client shows absolute times in exactly one place — a session's detail
/// card — and the runtime stores epoch milliseconds. A calendar library would
/// be a dependency for one label, so the civil-date conversion is done here:
/// days since the epoch are shifted to the 0000-03-01 era, where the leap-year
/// rule is a single division, and the month/day are recovered from the
/// 400-year cycle.
///
/// The result is `YYYY-MM-DD HH:MM` in UTC. It is deliberately not localised:
/// a terminal has no reliable timezone database, and an hour that silently
/// disagrees with the reader's clock is worse than one labelled UTC.
pub fn format_utc_timestamp(epoch_ms: i64) -> String {
    let seconds = epoch_ms.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let time_of_day = seconds.rem_euclid(86_400);
    let (year, month, day) = civil_from_days(days);
    let hour = time_of_day / 3_600;
    let minute = (time_of_day % 3_600) / 60;
    format!("{year:04}-{month:02}-{day:02} {hour:02}:{minute:02}")
}

/// Days since 1970-01-01 → (year, month, day), proleptic Gregorian.
fn civil_from_days(days: i64) -> (i64, u32, u32) {
    // Shift the epoch to 0000-03-01 so February (the leap-month special case)
    // is the last month of the year.
    let shifted = days + 719_468;
    let era = shifted.div_euclid(146_097);
    let day_of_era = shifted.rem_euclid(146_097);
    let year_of_era =
        (day_of_era - day_of_era / 1_460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
    let year = year_of_era + era * 400;
    let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
    let month_index = (5 * day_of_year + 2) / 153;
    let day = (day_of_year - (153 * month_index + 2) / 5 + 1) as u32;
    let month = if month_index < 10 {
        month_index + 3
    } else {
        month_index - 9
    } as u32;
    (if month <= 2 { year + 1 } else { year }, month, day)
}

/// A coarse "how long ago" label for a timestamp in milliseconds.
///
/// Buckets match the ones the rest of the interface speaks in: minutes, hours,
/// days, then months. `now_ms` is passed in rather than read from the clock so
/// the function stays pure and testable.
pub fn format_age(epoch_ms: i64, now_ms: i64) -> String {
    let delta = now_ms.saturating_sub(epoch_ms).max(0);
    let minutes = delta / 60_000;
    if minutes < 1 {
        return "just now".to_string();
    }
    if minutes < 60 {
        return format!("{minutes}m");
    }
    let hours = minutes / 60;
    if hours < 24 {
        return format!("{hours}h");
    }
    let days = hours / 24;
    if days < 30 {
        return format!("{days}d");
    }
    format!("{}mo", days / 30)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cjk_width_is_two_columns_per_ideograph() {
        assert_eq!(display_width("中文"), 4);
        assert_eq!(display_width("a中b"), 4);
        assert_eq!(display_width("こんにちは"), 10);
    }

    #[test]
    fn wrapping_never_exceeds_the_requested_width() {
        let text = "the quick brown fox jumps over the lazy dog";
        for width in 8..40 {
            for line in wrap_text(text, width) {
                assert!(
                    line.width <= width,
                    "line {:?} is {} columns wide (limit {width})",
                    line.text,
                    line.width
                );
                assert_eq!(display_width(&line.text), line.width);
            }
        }
    }

    #[test]
    fn cjk_wraps_at_the_column_limit() {
        let lines = wrap_text("这是一段很长的中文文本需要正确换行", 10);
        assert!(lines.len() >= 3);
        for line in &lines {
            assert!(line.width <= 10);
        }
        // Reassembling must reproduce the original text.
        let joined = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<String>();
        assert_eq!(joined, "这是一段很长的中文文本需要正确换行");
    }

    #[test]
    fn joiners_let_callers_rebuild_the_source_prose() {
        let text = "alpha beta gamma delta epsilon";
        let lines = wrap_text(text, 12);
        let mut rebuilt = String::new();
        for (index, line) in lines.iter().enumerate() {
            rebuilt.push_str(&line.text);
            if index + 1 < lines.len() && line.joiner.inserts_space() {
                rebuilt.push(' ');
            }
        }
        assert_eq!(rebuilt, text);
    }

    #[test]
    fn explicit_newlines_are_preserved_as_separate_lines() {
        let lines = wrap_text("first\nsecond", 40);
        assert_eq!(lines.len(), 2);
        assert_eq!(lines[0].text, "first");
        assert_eq!(lines[0].joiner, LineJoiner::Newline);
        assert_eq!(lines[1].text, "second");
    }

    #[test]
    fn blank_lines_survive_wrapping() {
        let lines = wrap_text("a\n\nb", 40);
        assert_eq!(lines.len(), 3);
        assert!(lines[1].is_blank());
    }

    #[test]
    fn long_words_are_split_rather_than_overflowing() {
        let lines = wrap_text("supercalifragilisticexpialidocious", 10);
        assert!(lines.len() > 1);
        for line in &lines {
            assert!(line.width <= 10);
        }
    }

    #[test]
    fn truncation_appends_an_ellipsis_within_budget() {
        assert_eq!(truncate_to_width("hello world", 8, "…"), "hello w…");
        assert_eq!(truncate_to_width("short", 8, "…"), "short");
        assert_eq!(truncate_to_width("中文中文", 5, "…"), "中文…");
    }

    #[test]
    fn padding_and_taking_agree_with_display_width() {
        assert_eq!(pad_to_width("ab", 5), "ab   ");
        assert_eq!(pad_to_width("中文", 6), "中文  ");
        let (prefix, rest) = take_width("中文abc", 5);
        assert_eq!(prefix, "中文a");
        assert_eq!(rest, "bc");
    }

    #[test]
    fn rtl_text_is_reordered_for_display_only() {
        let hebrew = "שלום עולם";
        assert!(contains_rtl(hebrew));
        assert!(!contains_rtl("hello"));
        // Pure LTR is returned untouched.
        assert_eq!(visual_order("hello"), "hello");
        // RTL output differs from the logical order but preserves the characters.
        let visual = visual_order(hebrew);
        let mut logical_chars = hebrew.chars().collect::<Vec<_>>();
        let mut visual_chars = visual.chars().collect::<Vec<_>>();
        logical_chars.sort_unstable();
        visual_chars.sort_unstable();
        assert_eq!(logical_chars, visual_chars);
    }

    #[test]
    fn emoji_and_combining_marks_do_not_split_graphemes() {
        let text = "e\u{301}clair 👨‍👩‍👧 done";
        for line in wrap_text(text, 6) {
            assert_eq!(display_width(&line.text), line.width);
        }
        let joined = wrap_text(text, 6)
            .iter()
            .map(|line| line.text.clone())
            .collect::<String>();
        assert_eq!(joined.replace(' ', ""), text.replace(' ', ""));
    }

    #[test]
    fn utc_timestamps_land_on_the_right_civil_date() {
        // 2025-09-30T13:12:00Z
        assert_eq!(format_utc_timestamp(1_759_237_920_000), "2025-09-30 13:12");
        // The epoch itself, and a leap day.
        assert_eq!(format_utc_timestamp(0), "1970-01-01 00:00");
        assert_eq!(format_utc_timestamp(1_709_164_800_000), "2024-02-29 00:00");
        // A timestamp before the epoch must not wrap into a negative year.
        assert_eq!(format_utc_timestamp(-1), "1969-12-31 23:59");
    }

    #[test]
    fn ages_bucket_into_the_units_a_reader_thinks_in() {
        let now = 1_000_000_000_000i64;
        assert_eq!(format_age(now, now), "just now");
        assert_eq!(format_age(now - 5 * 60_000, now), "5m");
        assert_eq!(format_age(now - 3 * 3_600_000, now), "3h");
        assert_eq!(format_age(now - 2 * 86_400_000, now), "2d");
        assert_eq!(format_age(now - 61 * 86_400_000, now), "2mo");
        // A clock that runs backwards must not produce a negative age.
        assert_eq!(format_age(now + 60_000, now), "just now");
    }
}

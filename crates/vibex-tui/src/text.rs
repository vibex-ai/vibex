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

    let mut current = String::new();
    let mut current_width = 0usize;
    let mut line_start_offset = paragraph_offset;
    let mut pending_space = false;
    let mut cursor_offset = paragraph_offset;

    let break_line = |lines: &mut Vec<WrappedLine>,
                      current: &mut String,
                      current_width: &mut usize,
                      line_start_offset: &mut usize,
                      cursor_offset: usize,
                      joiner: LineJoiner| {
        lines.push(WrappedLine {
            text: std::mem::take(current),
            width: *current_width,
            joiner,
            source_start: *line_start_offset,
        });
        *current_width = 0;
        *line_start_offset = cursor_offset;
    };

    for grapheme in paragraph.graphemes(true) {
        let grapheme_width = UnicodeWidthStr::width(grapheme);

        if is_space(grapheme) {
            // Collapse runs of whitespace to a single break opportunity.
            pending_space = !current.is_empty();
            cursor_offset += grapheme.len();
            continue;
        }

        let separator_width = usize::from(pending_space);
        if current_width + separator_width + grapheme_width > width && !current.is_empty() {
            let joiner = if pending_space {
                LineJoiner::Space
            } else {
                LineJoiner::None
            };
            break_line(
                lines,
                &mut current,
                &mut current_width,
                &mut line_start_offset,
                cursor_offset,
                joiner,
            );
            pending_space = false;
            line_start_offset = cursor_offset;
        }

        if pending_space && !current.is_empty() {
            current.push(' ');
            current_width += 1;
        }
        pending_space = false;

        // A single grapheme wider than the whole line still has to go somewhere.
        if grapheme_width > width && current.is_empty() {
            current.push_str(grapheme);
            current_width = grapheme_width;
        } else {
            current.push_str(grapheme);
            current_width += grapheme_width;
        }
        cursor_offset += grapheme.len();

        if is_breakable_after(grapheme) && current_width >= width {
            break_line(
                lines,
                &mut current,
                &mut current_width,
                &mut line_start_offset,
                cursor_offset,
                LineJoiner::None,
            );
        }
    }

    if !current.is_empty() || lines.is_empty() {
        lines.push(WrappedLine {
            text: current,
            width: current_width,
            joiner: LineJoiner::Newline,
            source_start: line_start_offset,
        });
    } else if let Some(last) = lines.last_mut() {
        // The paragraph ended on a break; mark the boundary as a real newline.
        if last.joiner == LineJoiner::None || last.joiner == LineJoiner::Space {
            last.joiner = LineJoiner::Newline;
        }
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
}

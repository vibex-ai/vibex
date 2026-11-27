//! Marking the words a find looked for.
//!
//! The transcript is a wall of prose, calls and answers, and a find that
//! painted whole rows left the reader hunting inside a highlighted paragraph
//! for the one word they typed. What a find marks is the matched text and
//! nothing around it: the row's own surface stays as it was, and the keyword
//! carries the tint — the row the bar is on painted stronger than the rest, so
//! it stays identifiable among the hits.
//!
//! Prose is a `TextView` over a Markdown document, so the mark is made in the
//! document: the parser's inline tree is walked and each matched run becomes
//! an [`Inline::Highlight`], which is the `<mark>` the view already draws.
//! Everything else is a plain `text`, whose children are spans, so the same
//! walk happens over a string.

use nsh_base::text_view::{
    Block, Document, Inline, ListItem, MarkdownOptions, Parser, Table, TableCell,
};
use nsh_core::prelude::*;

use crate::theme;

/// Byte ranges of the case-insensitive occurrences of `query` in `text`.
///
/// Case folding only moves offsets for cased characters, and most of what the
/// transcript holds is ASCII or has no case at all, so an ASCII pair is
/// searched in place rather than folded into a copy with an offset map.
pub fn match_ranges(text: &str, query: &str) -> Vec<std::ops::Range<usize>> {
    let query = query.trim();
    if text.is_empty() || query.is_empty() {
        return Vec::new();
    }
    let folded = query.to_lowercase();
    if folded.is_empty() {
        return Vec::new();
    }
    if text.is_ascii() && folded.is_ascii() {
        let haystack = text.as_bytes();
        let needle = folded.as_bytes();
        let mut ranges = Vec::new();
        let mut from = 0;
        while from + needle.len() <= haystack.len() {
            let Some(at) = haystack[from..]
                .windows(needle.len())
                .position(|window| window.eq_ignore_ascii_case(needle))
            else {
                break;
            };
            let start = from + at;
            ranges.push(start..start + needle.len());
            from = start + needle.len();
        }
        return ranges;
    }
    // A non-ASCII haystack: fold both and map the folded offsets back by
    // walking the original's characters, so a `İ` or a `ß` that folds to more
    // than one character still marks the whole original word.
    let mut folded_text = String::with_capacity(text.len());
    // (byte offset in `text`, byte offset in `folded_text`) pairs.
    let mut marks: Vec<(usize, usize)> = Vec::with_capacity(text.len() + 1);
    for (at, character) in text.char_indices() {
        marks.push((at, folded_text.len()));
        for lower in character.to_lowercase() {
            folded_text.push(lower);
        }
    }
    marks.push((text.len(), folded_text.len()));
    let mut ranges = Vec::new();
    let mut from = 0;
    while from < folded_text.len() {
        let Some(at) = folded_text[from..].find(&folded) else {
            break;
        };
        let start = from + at;
        let end = start + folded.len();
        let original_start = marks
            .iter()
            .rev()
            .find(|(_, folded_at)| *folded_at <= start)
            .map_or(0, |(at, _)| *at);
        let original_end = marks
            .iter()
            .find(|(_, folded_at)| *folded_at >= end)
            .map_or(text.len(), |(at, _)| *at);
        ranges.push(original_start..original_end);
        from = end;
    }
    ranges
}

/// Where the first match of `query` sits inside `text`, as a 0..=1 fraction of
/// its characters.
///
/// A row taller than the viewport cannot be revealed whole, so the scroll aims
/// at this position rather than at the row's top: the word a long answer buries
/// in its middle is exactly what the find bar is looking for.
pub fn match_aim(text: &str, query: &str) -> f32 {
    let ranges = match_ranges(text, query);
    let Some(range) = ranges.first() else {
        return 0.0;
    };
    // Characters, not bytes: a byte offset would overstate how far into a CJK
    // row the match sits, and those rows are the common case here.
    let before = text[..range.start].chars().count();
    let total = text.chars().count();
    if total == 0 {
        return 0.0;
    }
    before as f32 / total as f32
}

/// The tint one match is painted with: the current row's stronger than the
/// rest.
///
/// Read on the engine thread: a theme token is a signal, so this belongs in a
/// view's own closure rather than in the marking itself.
pub fn highlight_tint(active: bool) -> Hsla {
    theme::accent_yellow().opacity(if active { 0.55 } else { 0.30 })
}

/// [`highlight_tint`] as the CSS color a Markdown highlight takes.
pub fn highlight_color(active: bool) -> String {
    highlight_tint(active).to_hex()
}

/// One piece of a marked string.
pub struct TextSpan {
    pub text: String,
    /// Whether this piece is a match.
    pub marked: bool,
}

/// `text` cut into pieces, the matches among them; `None` when it holds none.
///
/// A text with no match costs one scan and no rebuild: the caller keeps the
/// plain string it already had.
pub fn spans(text: &str, query: &str) -> Option<Vec<TextSpan>> {
    let ranges = match_ranges(text, query);
    if ranges.is_empty() {
        return None;
    }
    let mut spans = Vec::with_capacity(ranges.len() * 2 + 1);
    let mut at = 0;
    for range in ranges {
        if range.start > at {
            spans.push(TextSpan {
                text: text[at..range.start].to_owned(),
                marked: false,
            });
        }
        spans.push(TextSpan {
            text: text[range.clone()].to_owned(),
            marked: true,
        });
        at = range.end;
    }
    if at < text.len() {
        spans.push(TextSpan {
            text: text[at..].to_owned(),
            marked: false,
        });
    }
    Some(spans)
}

/// `document` with every match of `query` wrapped in a highlight.
///
/// The walk follows the inline tree the parser made, so a match inside a bold
/// run, a link or a table cell is marked where it sits rather than in a copy
/// of the text: the document keeps its structure, and the mark is the view's
/// own `<mark>`.
pub fn marked_document(document: Document, query: &str, color: &str) -> Document {
    if query.trim().is_empty() {
        return document;
    }
    let blocks = document
        .blocks
        .into_iter()
        .map(|block| marked_block(block, query, color))
        .collect();
    let footnotes = document
        .footnotes
        .into_iter()
        .map(|mut footnote| {
            footnote.children = footnote
                .children
                .into_iter()
                .map(|block| marked_block(block, query, color))
                .collect();
            footnote
        })
        .collect();
    let mut document = Document::new(blocks);
    document.footnotes = footnotes;
    document
}

/// The document `source` reads, with its matches marked.
pub fn marked_markdown(
    source: &str,
    query: &str,
    color: &str,
    options: MarkdownOptions,
) -> Document {
    let document = Parser::new().options(options).markdown(source);
    marked_document(document, query, color)
}

/// A line of plain text whose matched runs carry the find's tint.
///
/// `style` dresses the whole line the way the row wants it (its type size, its
/// ink, its wrapping); the marks are spans inside it, so a match is tinted and
/// the words around it are not.
pub fn marked_line(
    body: Memo<String>,
    query: Signal<String>,
    active: Memo<bool>,
    style: impl Fn(Text) -> Text + Clone + 'static,
) -> impl IntoElement {
    dynamic(move || {
        let query = query.get();
        let value = body.get();
        let marked = (!query.trim().is_empty())
            .then(|| spans(&value, &query))
            .flatten();
        let Some(marked) = marked else {
            return style(text(value)).into_element();
        };
        // The theme's tint, alpha and all: a highlight span keeps the alpha
        // the style was given.
        let color = highlight_tint(active.get());
        // The first piece is the text itself and the rest are its spans: a
        // text with no content of its own is not a block the platform merges
        // its children into, so the run has to start with real text.
        let mut pieces = marked.into_iter();
        let first = pieces.next().expect("a match always leaves text before it");
        let mut line = text(first.text);
        if first.marked {
            line = line.bg(color).rounded(px(3.0));
        }
        let rest = pieces
            .map(|span| {
                let piece = text(span.text);
                if span.marked {
                    piece.bg(color).rounded(px(3.0)).into_element()
                } else {
                    piece.into_element()
                }
            })
            .collect::<Vec<_>>();
        style(line.children(rest)).into_element()
    })
}

/// A Markdown document whose matched runs carry the find's tint.
///
/// `style` dresses the view the way the row wants it; the marking happens in
/// the document, so a match inside bold text, a list or a table is marked
/// where it sits.
pub fn marked_markdown_view(
    body: Memo<String>,
    query: Signal<String>,
    active: Memo<bool>,
    streaming: Memo<bool>,
    style: impl Fn(nsh_base::text_view::TextViewElement) -> nsh_base::text_view::TextViewElement
    + Clone
    + 'static,
) -> impl IntoElement {
    dynamic(move || {
        let query = query.get();
        let value = body.get();
        let options = MarkdownOptions {
            streaming: streaming.get(),
            ..MarkdownOptions::default()
        };
        let color = highlight_color(active.get());
        let document = marked_markdown(&value, &query, &color, options);
        style(
            nsh_base::text_view::text_view()
                .document(Some(document))
                .options(options),
        )
        .into_element()
    })
}

fn marked_block(block: Block, query: &str, color: &str) -> Block {
    match block {
        Block::Paragraph { content } => Block::Paragraph {
            content: marked_inlines(content, query, color),
        },
        Block::Heading {
            level,
            content,
            anchor,
        } => Block::Heading {
            level,
            content: marked_inlines(content, query, color),
            anchor,
        },
        Block::Blockquote { children, alert } => Block::Blockquote {
            children: children
                .into_iter()
                .map(|child| marked_block(child, query, color))
                .collect(),
            alert,
        },
        Block::List {
            ordered,
            start,
            loose,
            items,
        } => Block::List {
            ordered,
            start,
            loose,
            items: items
                .into_iter()
                .map(|item| marked_item(item, query, color))
                .collect(),
        },
        Block::Table(table) => Block::Table(marked_table(table, query, color)),
        Block::Details {
            open,
            summary,
            children,
        } => Block::Details {
            open,
            summary: marked_inlines(summary, query, color),
            children: children
                .into_iter()
                .map(|child| marked_block(child, query, color))
                .collect(),
        },
        // A code block, a formula, a rule or a custom node is not prose: the
        // reader searching the conversation is looking for what was said, and
        // a mark inside a code sample would move its indentation.
        other => other,
    }
}

fn marked_item(item: ListItem, query: &str, color: &str) -> ListItem {
    let mut marked = ListItem::new(
        item.children
            .into_iter()
            .map(|child| marked_block(child, query, color))
            .collect(),
    );
    marked.checked = item.checked;
    marked
}

fn marked_table(table: Table, query: &str, color: &str) -> Table {
    let mut marked = Table::default();
    marked.align = table.align;
    marked.head = table
        .head
        .into_iter()
        .map(|cell| marked_cell(cell, query, color))
        .collect();
    marked.rows = table
        .rows
        .into_iter()
        .map(|row| {
            row.into_iter()
                .map(|cell| marked_cell(cell, query, color))
                .collect()
        })
        .collect();
    marked
}

fn marked_cell(cell: TableCell, query: &str, color: &str) -> TableCell {
    let mut marked = TableCell::new(marked_inlines(cell.content, query, color));
    marked.colspan = cell.colspan;
    marked
}

fn marked_inlines(inlines: Vec<Inline>, query: &str, color: &str) -> Vec<Inline> {
    let mut out = Vec::with_capacity(inlines.len());
    for inline in inlines {
        marked_inline(inline, query, color, &mut out);
    }
    out
}

fn marked_inline(inline: Inline, query: &str, color: &str, out: &mut Vec<Inline>) {
    match inline {
        Inline::Text { text } => marked_run(&text, query, color, out),
        // Code is marked too: a find that skipped it would leave the word the
        // reader typed visible but unmarked inside the answer's snippet.
        Inline::Code { code } => marked_run(&code, query, color, out),
        Inline::Strong { content } => out.push(Inline::Strong {
            content: marked_inlines(content, query, color),
        }),
        Inline::Emphasis { content } => out.push(Inline::Emphasis {
            content: marked_inlines(content, query, color),
        }),
        Inline::Strikethrough { content } => out.push(Inline::Strikethrough {
            content: marked_inlines(content, query, color),
        }),
        Inline::Underline { content } => out.push(Inline::Underline {
            content: marked_inlines(content, query, color),
        }),
        Inline::Highlight {
            content,
            color: own,
        } => out.push(Inline::Highlight {
            content: marked_inlines(content, query, color),
            color: own,
        }),
        Inline::TextStyle {
            content,
            color: own,
        } => out.push(Inline::TextStyle {
            content: marked_inlines(content, query, color),
            color: own,
        }),
        Inline::Superscript { content } => out.push(Inline::Superscript {
            content: marked_inlines(content, query, color),
        }),
        Inline::Subscript { content } => out.push(Inline::Subscript {
            content: marked_inlines(content, query, color),
        }),
        Inline::Small { content } => out.push(Inline::Small {
            content: marked_inlines(content, query, color),
        }),
        Inline::Link {
            url,
            title,
            content,
        } => out.push(Inline::Link {
            url,
            title,
            content: marked_inlines(content, query, color),
        }),
        other => out.push(other),
    }
}

/// Writes `text` into `out`, its matches wrapped in a highlight.
fn marked_run(text: &str, query: &str, color: &str, out: &mut Vec<Inline>) {
    let ranges = match_ranges(text, query);
    if ranges.is_empty() {
        out.push(Inline::Text {
            text: text.to_owned(),
        });
        return;
    }
    let mut at = 0;
    for range in ranges {
        if range.start > at {
            out.push(Inline::Text {
                text: text[at..range.start].to_owned(),
            });
        }
        out.push(Inline::Highlight {
            content: vec![Inline::Text {
                text: text[range.clone()].to_owned(),
            }],
            color: Some(color.to_owned()),
        });
        at = range.end;
    }
    if at < text.len() {
        out.push(Inline::Text {
            text: text[at..].to_owned(),
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_ascii_match_is_found_whatever_its_case() {
        assert_eq!(match_ranges("Call the Tool twice", "tool"), vec![9..13]);
        assert_eq!(match_ranges("aaaa", "aa"), vec![0..2, 2..4]);
        assert!(match_ranges("anything", "  ").is_empty());
    }

    #[test]
    fn a_match_that_folds_to_more_characters_still_marks_the_word() {
        let ranges = match_ranges("Straße und STRASSE", "strasse");
        assert_eq!(ranges.len(), 1, "only the spelled-out one holds it");
        // `İ` folds to `i` plus a combining dot, so the whole word is the
        // match rather than only its first letter.
        assert_eq!(
            match_ranges("İstanbul", "i̇stanbul"),
            vec![0.."İstanbul".len()]
        );
    }

    #[test]
    fn marking_a_paragraph_keeps_the_text_and_marks_only_the_word() {
        let document = marked_markdown(
            "The **Tool** ran, and the tool said so.",
            "tool",
            "#FDE047",
            MarkdownOptions::default(),
        );
        assert_eq!(
            document.plain_text(),
            "The Tool ran, and the tool said so.",
            "the words are untouched"
        );
        let Block::Paragraph { content } = &document.blocks[0] else {
            panic!("a paragraph");
        };
        assert_eq!(
            count_highlights(content),
            2,
            "one in the bold run, one in the plain text"
        );
    }

    #[test]
    fn a_string_is_cut_around_its_matches() {
        let pieces = spans("a Tool and a tool", "tool").expect("two matches");
        let texts = pieces
            .iter()
            .map(|span| (span.text.as_str(), span.marked))
            .collect::<Vec<_>>();
        assert_eq!(
            texts,
            vec![
                ("a ", false),
                ("Tool", true),
                (" and a ", false),
                ("tool", true),
            ]
        );
        assert!(spans("nothing here", "tool").is_none());
    }

    fn count_highlights(inlines: &[Inline]) -> usize {
        inlines
            .iter()
            .map(|inline| match inline {
                Inline::Highlight { content, .. } => 1 + count_highlights(content),
                Inline::Strong { content }
                | Inline::Emphasis { content }
                | Inline::Strikethrough { content }
                | Inline::Underline { content }
                | Inline::TextStyle { content, .. }
                | Inline::Superscript { content }
                | Inline::Subscript { content }
                | Inline::Small { content }
                | Inline::Link { content, .. } => count_highlights(content),
                _ => 0,
            })
            .sum()
    }
}

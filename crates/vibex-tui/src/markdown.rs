//! Markdown → terminal lines.
//!
//! Parsing is delegated to `vibex-markdown` with `default-features = false`,
//! which is GPUI-free and already used by `vibex-desktop-model`. Everything the
//! terminal cannot express degrades explicitly rather than silently:
//!
//! | Source | Terminal rendering |
//! | --- | --- |
//! | Mermaid / PlantUML / LaTeX | source block plus a hint that it needs an external renderer |
//! | inline image | `[image: alt WxH]` placeholder |
//! | HTML | the parser's safe-text projection |
//!
//! Syntax colouring is lexical rather than tree-sitter based. It uses the
//! theme's own `syntaxHighlight` palette, so a code block matches the rest of
//! the UI without adding a highlighting dependency to the client.

use std::collections::BTreeMap;

use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use vibex_markdown::{
    Block, BlockNode, DiagramKind, Inline, InlineNode, MarkdownInput, parse_markdown,
};

use crate::locale::Strings;
use crate::text::{display_width, take_width, wrap_text};
use crate::theme::TuiTheme;

/// A rendered markdown document: display lines plus the plain text of each,
/// which the copy action and the search index both need.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderedMarkdown {
    pub lines: Vec<Line<'static>>,
    /// Plain text per line, same length as `lines`.
    pub plain: Vec<String>,
}

impl RenderedMarkdown {
    pub fn height(&self) -> usize {
        self.lines.len()
    }

    pub fn text(&self) -> String {
        let mut output = String::new();
        for (index, line) in self.plain.iter().enumerate() {
            if index > 0 {
                output.push('\n');
            }
            output.push_str(line);
        }
        output
    }
}

/// Render markdown at a given width.
pub fn render_markdown(
    source: &str,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
) -> RenderedMarkdown {
    let width = width.max(8);
    let input = MarkdownInput::new(source, "", 0);
    let document = parse_markdown(input);
    let mut builder = Builder {
        theme,
        strings,
        highlight: HighlightPalette::for_theme(theme),
        width,
        lines: Vec::new(),
    };
    builder.blocks(&document.blocks, 0);
    builder.finish()
}

/// Render plain text (no markdown syntax) with the same wrapping rules.
pub fn render_plain(source: &str, theme: &TuiTheme, width: usize) -> RenderedMarkdown {
    let mut lines = Vec::new();
    let plain_style = theme.base();
    for raw in source.split('\n') {
        for wrapped in wrap_text(raw, width.max(8)) {
            let text = wrapped.text;
            lines.push((Line::from(Span::styled(text.clone(), plain_style)), text));
        }
    }
    if lines.is_empty() {
        lines.push((Line::default(), String::new()));
    }
    let (lines, plain) = lines.into_iter().unzip();
    RenderedMarkdown { lines, plain }
}

struct Builder<'a> {
    theme: &'a TuiTheme,
    strings: Strings,
    highlight: HighlightPalette,
    width: usize,
    lines: Vec<(Line<'static>, String)>,
}

/// One logical line before wrapping: an indent, an optional marker, and spans.
struct Logical {
    indent: usize,
    marker: Vec<Span<'static>>,
    spans: Vec<Span<'static>>,
    /// Background applied across the whole line (code blocks, quotes).
    background: Option<Style>,
}

impl Logical {
    fn new() -> Self {
        Self {
            indent: 0,
            marker: Vec::new(),
            spans: Vec::new(),
            background: None,
        }
    }

    /// The wrappable body. The marker is a prefix, not part of the text, so a
    /// long hint is wrapped against the width that is actually left for it.
    fn body(&self) -> String {
        let mut output = String::new();
        for span in &self.spans {
            output.push_str(span.content.as_ref());
        }
        output
    }
}

impl<'a> Builder<'a> {
    fn finish(self) -> RenderedMarkdown {
        let (lines, plain) = self.lines.into_iter().unzip();
        RenderedMarkdown { lines, plain }
    }

    fn blank(&mut self) {
        if self.lines.last().is_some_and(|(_, text)| text.is_empty()) {
            return;
        }
        self.lines.push((Line::default(), String::new()));
    }

    fn push_logical(&mut self, logical: Logical) {
        let prefix_width = logical.indent
            + logical
                .marker
                .iter()
                .map(|span| display_width(span.content.as_ref()))
                .sum::<usize>();
        let body = logical.body();
        let available = self.width.saturating_sub(prefix_width).max(1);
        let wrapped = wrap_text(&body, available);
        // Span styling survives wrapping by re-applying the logical line's own
        // style to the wrapped segment; markdown emphasis is applied when the
        // spans are built, so the wrapping above only splits plain text.
        let primary_style = logical
            .spans
            .first()
            .map(|span| span.style)
            .unwrap_or_default();
        // The marker only decorates the first visual line; continuations line
        // up under the text so a long bullet stays readable.
        let hanging = " ".repeat(prefix_width);
        for (index, segment) in wrapped.iter().enumerate() {
            let mut spans = Vec::new();
            if index == 0 {
                if logical.indent > 0 {
                    spans.push(Span::raw(" ".repeat(logical.indent)));
                }
                spans.extend(logical.marker.iter().cloned());
            } else {
                spans.push(Span::raw(hanging.clone()));
            }
            let content = segment.text.clone();
            spans.push(Span::styled(content.clone(), primary_style));
            let mut line = Line::from(spans);
            if let Some(background) = logical.background {
                line = line.style(background);
            }
            let mut plain = String::new();
            if index > 0 {
                plain.push_str(&hanging);
            } else {
                plain.push_str(&" ".repeat(logical.indent));
                for span in &logical.marker {
                    plain.push_str(span.content.as_ref());
                }
            }
            plain.push_str(&content);
            self.lines.push((line, plain));
        }
    }

    fn blocks(&mut self, blocks: &[BlockNode], indent: usize) {
        for block in blocks {
            self.block(block, indent);
        }
    }

    fn block(&mut self, block: &BlockNode, indent: usize) {
        match &block.kind {
            Block::Paragraph(inlines) => {
                self.logical_with(indent, Vec::new(), self.inlines(inlines));
                self.blank();
            }
            Block::Heading { level, content, .. } => {
                let style = match level {
                    1 => self.theme.strong().add_modifier(Modifier::UNDERLINED),
                    _ => self.theme.strong(),
                };
                let mut spans = self.inlines_styled(content, style);
                if spans.is_empty() {
                    spans.push(Span::styled(String::new(), style));
                }
                let marker = if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                    vec![Span::styled(
                        format!("{} ", "#".repeat(*level as usize)),
                        self.theme.muted(),
                    )]
                } else {
                    Vec::new()
                };
                self.logical_with(indent, marker, spans);
                self.blank();
            }
            Block::Quote(children) => {
                let marker = vec![Span::styled(
                    if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                        "▏ ".to_string()
                    } else {
                        "| ".to_string()
                    },
                    self.theme.muted(),
                )];
                self.quote_children(children, indent, marker);
                self.blank();
            }
            Block::Callout {
                kind,
                title,
                children,
            } => {
                let marker = vec![Span::styled(
                    format!("{} ", kind.title()),
                    self.callout_style(*kind),
                )];
                if !title.is_empty() {
                    self.logical_with(
                        indent,
                        marker.clone(),
                        vec![Span::styled(title.clone(), self.callout_style(*kind))],
                    );
                    self.quote_children(children, indent + 2, Vec::new());
                } else {
                    self.quote_children(children, indent, marker);
                }
                self.blank();
            }
            Block::Code {
                language, source, ..
            } => {
                self.code_block(language.as_deref(), source, indent);
                self.blank();
            }
            Block::Diff { source } => {
                self.diff_block(source, indent);
                self.blank();
            }
            Block::Math { source } => {
                self.logical_with(
                    indent,
                    vec![Span::styled("∑ ", self.theme.muted())],
                    vec![Span::styled(source.clone(), self.theme.accent())],
                );
                self.blank();
            }
            Block::Diagram { kind, source } => {
                let label = match kind {
                    DiagramKind::Mermaid => "mermaid",
                    DiagramKind::PlantUml => "plantuml",
                };
                self.logical_with(
                    indent,
                    vec![Span::styled(format!("{label} "), self.theme.muted())],
                    vec![Span::styled(
                        self.strings.transcript_mermaid_hint().to_string(),
                        self.theme.muted(),
                    )],
                );
                for line in source.split('\n') {
                    self.logical_with(
                        indent + 2,
                        Vec::new(),
                        vec![Span::styled(line.to_string(), self.theme.code())],
                    );
                }
                self.blank();
            }
            Block::List { start, items } => {
                for (index, item) in items.iter().enumerate() {
                    let marker_text = match item.checked {
                        Some(true) => {
                            if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                                "☑ ".to_string()
                            } else {
                                "[x] ".to_string()
                            }
                        }
                        Some(false) => {
                            if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                                "☐ ".to_string()
                            } else {
                                "[ ] ".to_string()
                            }
                        }
                        None => match start {
                            Some(start) => format!("{}. ", start + index as u64),
                            None => {
                                if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                                    "• ".to_string()
                                } else {
                                    "* ".to_string()
                                }
                            }
                        },
                    };
                    let marker = vec![Span::styled(marker_text, self.theme.muted())];
                    let mut first = true;
                    for child in &item.children {
                        match &child.kind {
                            Block::Paragraph(inlines) if first => {
                                self.logical_with(indent, marker.clone(), self.inlines(inlines));
                            }
                            _ => self.block(child, indent + 2),
                        }
                        first = false;
                    }
                    if first {
                        self.logical_with(indent, marker, Vec::new());
                    }
                }
                self.blank();
            }
            Block::DefinitionList(items) => {
                for item in items {
                    self.logical_with(indent, Vec::new(), self.inlines(&item.term));
                    for definition in &item.definitions {
                        self.quote_children(definition, indent + 2, Vec::new());
                    }
                }
                self.blank();
            }
            Block::Table {
                alignments,
                header,
                rows,
            } => {
                self.table(alignments, header.as_ref(), rows, indent);
                self.blank();
            }
            Block::ThematicBreak => {
                let glyph = if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                    "─"
                } else {
                    "-"
                };
                self.logical_with(
                    indent,
                    Vec::new(),
                    vec![Span::styled(
                        glyph.repeat(self.width.min(40)),
                        self.theme.muted(),
                    )],
                );
                self.blank();
            }
            Block::TableOfContents => {
                // The outline is rendered by the page chrome, not inline.
            }
            Block::Details {
                summary, children, ..
            } => {
                let marker = vec![Span::styled(
                    if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                        "▸ ".to_string()
                    } else {
                        "> ".to_string()
                    },
                    self.theme.muted(),
                )];
                self.logical_with(
                    indent,
                    marker,
                    self.inlines_styled(summary, self.theme.strong()),
                );
                self.quote_children(children, indent + 2, Vec::new());
                self.blank();
            }
            Block::Progress { value, max, label } => {
                let ratio = if *max > 0.0 {
                    (value / max).clamp(0.0, 1.0)
                } else {
                    0.0
                };
                let bar_width = self.width.saturating_sub(12).clamp(4, 30);
                let filled = (ratio * bar_width as f64).round() as usize;
                let bar = format!(
                    "[{}{}] {:>3}%",
                    "█".repeat(filled),
                    "░".repeat(bar_width.saturating_sub(filled)),
                    (ratio * 100.0).round() as u32
                );
                let mut spans = vec![Span::styled(bar, self.theme.accent())];
                if let Some(label) = label {
                    spans.push(Span::raw(" "));
                    spans.push(Span::styled(label.clone(), self.theme.muted()));
                }
                self.logical_with(indent, Vec::new(), spans);
                self.blank();
            }
            Block::Image(image) => {
                let label = format!(
                    "[{}: {}{}]",
                    self.strings.transcript_image(),
                    if image.alt.is_empty() {
                        resource_label(&image.destination)
                    } else {
                        image.alt.clone()
                    },
                    image
                        .title
                        .as_ref()
                        .map(|title| format!(" — {title}"))
                        .unwrap_or_default()
                );
                self.logical_with(
                    indent,
                    Vec::new(),
                    vec![Span::styled(label, self.theme.muted())],
                );
                self.blank();
            }
            Block::FootnoteDefinition { label, children } => {
                self.logical_with(
                    indent,
                    vec![Span::styled(format!("[^{label}] "), self.theme.muted())],
                    Vec::new(),
                );
                self.quote_children(children, indent + 2, Vec::new());
                self.blank();
            }
            Block::SafeHtml(children) => self.blocks(children, indent),
            Block::Literal(text) => {
                for line in text.split('\n') {
                    self.logical_with(
                        indent,
                        Vec::new(),
                        vec![Span::styled(line.to_string(), self.theme.base())],
                    );
                }
                self.blank();
            }
        }
    }

    fn quote_children(
        &mut self,
        children: &[BlockNode],
        indent: usize,
        marker: Vec<Span<'static>>,
    ) {
        let mut first = true;
        for child in children {
            if let Block::Paragraph(inlines) = &child.kind {
                self.logical_with(
                    indent,
                    if first { marker.clone() } else { Vec::new() },
                    self.inlines(inlines),
                );
                first = false;
            } else {
                self.block(child, indent + if first { 2 } else { 0 });
                first = false;
            }
        }
    }

    fn callout_style(&self, kind: vibex_markdown::CalloutKind) -> Style {
        use vibex_markdown::CalloutKind;
        match kind {
            CalloutKind::Note | CalloutKind::Tip => self.theme.accent(),
            CalloutKind::Important => self.theme.strong(),
            CalloutKind::Warning | CalloutKind::Caution => self.theme.warning(),
        }
    }

    fn code_block(&mut self, language: Option<&str>, source: &str, indent: usize) {
        let base = self.theme.code();
        if let Some(language) = language.filter(|value| !value.is_empty()) {
            self.logical_with(
                indent,
                Vec::new(),
                vec![Span::styled(language.to_string(), self.theme.muted())],
            );
        }
        let lines = source.split('\n').collect::<Vec<_>>();
        // Trim the trailing empty line a fenced block always carries.
        let lines = match lines.split_last() {
            Some((last, rest)) if last.trim().is_empty() => rest.to_vec(),
            _ => lines,
        };
        for line in lines {
            let spans = self.highlight.highlight(language, line, base, self.theme);
            self.logical_code(indent + 2, spans, base);
        }
    }

    fn diff_block(&mut self, source: &str, indent: usize) {
        let base = self.theme.code();
        for line in source.split('\n') {
            let style = if line.starts_with('+') && !line.starts_with("+++") {
                self.theme.success()
            } else if line.starts_with('-') && !line.starts_with("---") {
                self.theme.danger()
            } else if line.starts_with("@@") {
                self.theme.accent()
            } else {
                self.theme.muted()
            };
            self.logical_code(
                indent + 2,
                vec![Span::styled(line.to_string(), style)],
                base,
            );
        }
    }

    fn table(
        &mut self,
        alignments: &[vibex_markdown::TableAlignment],
        header: Option<&vibex_markdown::TableRow>,
        rows: &[vibex_markdown::TableRow],
        indent: usize,
    ) {
        let columns = header
            .map(|header| header.cells.len())
            .unwrap_or_default()
            .max(rows.iter().map(|row| row.cells.len()).max().unwrap_or(0));
        if columns == 0 {
            return;
        }
        let mut cells =
            vec![vec![String::new(); columns]; rows.len() + usize::from(header.is_some())];
        let mut offset = 0;
        if let Some(header) = header {
            for (index, cell) in header.cells.iter().enumerate().take(columns) {
                cells[0][index] = plain_inlines(cell);
            }
            offset = 1;
        }
        for (row_index, row) in rows.iter().enumerate() {
            for (index, cell) in row.cells.iter().enumerate().take(columns) {
                cells[row_index + offset][index] = plain_inlines(cell);
            }
        }
        // Column widths share the available space, bounded so one wide cell
        // cannot push the table past the viewport.
        let separators = columns.saturating_sub(1) * 3 + 2;
        let budget = self.width.saturating_sub(indent + separators).max(columns);
        let mut widths = vec![0usize; columns];
        for row in &cells {
            for (index, cell) in row.iter().enumerate() {
                widths[index] = widths[index].max(display_width(cell).min(budget));
            }
        }
        let total = widths.iter().sum::<usize>();
        if total > budget {
            let mut remaining = budget;
            for width in widths.iter_mut() {
                let share = (*width * budget / total).max(3);
                *width = share.min(remaining.max(3));
                remaining = remaining.saturating_sub(*width);
            }
        }
        let glyphs = self.theme.glyphs() == crate::theme::GlyphMode::Unicode;
        let (vertical, horizontal) = if glyphs { ("│", "─") } else { ("|", "-") };
        let rule = |widths: &[usize]| {
            let mut text = String::from(if glyphs { "├" } else { "+" });
            for (index, width) in widths.iter().enumerate() {
                text.push_str(&horizontal.repeat(width + 2));
                text.push_str(if index + 1 == widths.len() {
                    if glyphs { "┤" } else { "+" }
                } else if glyphs {
                    "┼"
                } else {
                    "+"
                });
            }
            text
        };
        let mut header_rule = String::from(if glyphs { "┌" } else { "+" });
        for (index, width) in widths.iter().enumerate() {
            header_rule.push_str(&horizontal.repeat(width + 2));
            header_rule.push_str(if index + 1 == widths.len() {
                if glyphs { "┐" } else { "+" }
            } else if glyphs {
                "┬"
            } else {
                "+"
            });
        }
        self.logical_with(
            indent,
            Vec::new(),
            vec![Span::styled(header_rule, self.theme.border_style())],
        );
        for (row_index, row) in cells.iter().enumerate() {
            let mut spans = vec![Span::styled(
                format!("{vertical} "),
                self.theme.border_style(),
            )];
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).map(String::as_str).unwrap_or_default();
                let padded = match alignments.get(index) {
                    Some(vibex_markdown::TableAlignment::Right) => {
                        format!("{:>width$}", truncate(cell, *width), width = *width)
                    }
                    Some(vibex_markdown::TableAlignment::Center) => {
                        let text = truncate(cell, *width);
                        let padding = width.saturating_sub(display_width(&text));
                        format!(
                            "{}{}{}",
                            " ".repeat(padding / 2),
                            text,
                            " ".repeat(padding - padding / 2)
                        )
                    }
                    _ => format!("{:<width$}", truncate(cell, *width), width = *width),
                };
                let style = if row_index == 0 && header.is_some() {
                    self.theme.strong()
                } else {
                    self.theme.base()
                };
                spans.push(Span::styled(padded, style));
                spans.push(Span::styled(
                    format!(" {vertical} "),
                    self.theme.border_style(),
                ));
            }
            self.logical_with(indent, Vec::new(), spans);
            if row_index == 0 && header.is_some() {
                self.logical_with(
                    indent,
                    Vec::new(),
                    vec![Span::styled(rule(&widths), self.theme.border_style())],
                );
            }
        }
    }

    fn inlines(&self, inlines: &[InlineNode]) -> Vec<Span<'static>> {
        self.inlines_styled(inlines, self.theme.base())
    }

    fn inlines_styled(&self, inlines: &[InlineNode], base: Style) -> Vec<Span<'static>> {
        let mut spans = Vec::new();
        for inline in inlines {
            self.inline(inline, base, &mut spans);
        }
        spans
    }

    fn inline(&self, node: &InlineNode, style: Style, out: &mut Vec<Span<'static>>) {
        match &node.kind {
            Inline::Text(text) | Inline::Literal(text) => {
                out.push(Span::styled(text.clone(), style));
            }
            Inline::Code(text) => {
                out.push(Span::styled(
                    format!("`{text}`"),
                    style.patch(self.theme.code()).fg(self.theme.roles.accent),
                ));
            }
            Inline::Emphasis(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::ITALIC), out);
            }
            Inline::Strong(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::BOLD), out);
            }
            Inline::Deletion(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::CROSSED_OUT), out);
            }
            Inline::Underline(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::UNDERLINED), out);
            }
            Inline::Superscript(children) | Inline::Subscript(children) => {
                self.inlines_into(children, style, out);
            }
            Inline::Mark(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::REVERSED), out);
            }
            Inline::Keycap(children) => {
                let mut inner = Vec::new();
                self.inlines_into(children, style.add_modifier(Modifier::BOLD), &mut inner);
                let text = inner
                    .iter()
                    .map(|span| span.content.as_ref())
                    .collect::<String>();
                out.push(Span::styled(format!("[{text}]"), style));
            }
            Inline::Link {
                destination,
                children,
                ..
            } => {
                self.inlines_into(
                    children,
                    style
                        .add_modifier(Modifier::UNDERLINED)
                        .fg(self.theme.roles.accent),
                    out,
                );
                // Show the target when the label does not already name it.
                let label = plain_inlines(children);
                let target = resource_target(destination);
                if !label.contains(&target) {
                    out.push(Span::styled(format!(" <{target}>"), self.theme.muted()));
                }
            }
            Inline::Image(image) => {
                out.push(Span::styled(
                    format!(
                        "[{}: {}]",
                        self.strings.transcript_image(),
                        if image.alt.is_empty() {
                            resource_label(&image.destination)
                        } else {
                            image.alt.clone()
                        }
                    ),
                    self.theme.muted(),
                ));
            }
            Inline::Math(source) => {
                out.push(Span::styled(format!("${source}$"), self.theme.accent()));
            }
            Inline::Break => out.push(Span::raw(" ")),
            Inline::FootnoteReference(label) => {
                out.push(Span::styled(format!("[^{label}]"), self.theme.muted()));
            }
        }
    }

    fn inlines_into(&self, nodes: &[InlineNode], style: Style, out: &mut Vec<Span<'static>>) {
        for node in nodes {
            self.inline(node, style, out);
        }
    }

    /// A logical line that carries a background, so wrapping pads the tail.
    fn logical_code(&mut self, indent: usize, spans: Vec<Span<'static>>, background: Style) {
        let mut logical = Logical::new();
        logical.indent = indent;
        logical.spans = spans;
        logical.background = Some(background);
        self.push_logical(logical);
    }

    fn logical_with(
        &mut self,
        indent: usize,
        marker: Vec<Span<'static>>,
        spans: Vec<Span<'static>>,
    ) {
        let mut logical = Logical::new();
        logical.indent = indent;
        logical.marker = marker;
        logical.spans = spans;
        self.push_logical(logical);
    }
}

/// Where a markdown resource actually points.
fn resource_target(resource: &vibex_markdown::ResolvedResource) -> String {
    resource
        .resolved
        .clone()
        .unwrap_or_else(|| resource.source.clone())
}

/// How a resource should be described when it has no explicit alt text.
fn resource_label(resource: &vibex_markdown::ResolvedResource) -> String {
    resource
        .label
        .clone()
        .filter(|label| !label.trim().is_empty())
        .unwrap_or_else(|| resource_target(resource))
}

fn truncate(text: &str, width: usize) -> String {
    if display_width(text) <= width {
        return text.to_string();
    }
    take_width(text, width.saturating_sub(1)).0 + "…"
}

pub fn plain_inlines(inlines: &[InlineNode]) -> String {
    vibex_markdown::plain_text(inlines)
}

/// Lexical colouring driven by the theme's `syntaxHighlight` palette.
///
/// Deliberately shallow: comments, strings, numbers and a small keyword set per
/// language family. That covers the readability win without pulling a
/// tree-sitter grammar set into a terminal client.
#[derive(Debug, Clone, Default)]
pub struct HighlightPalette {
    keyword: Option<ratatui::style::Color>,
    string: Option<ratatui::style::Color>,
    comment: Option<ratatui::style::Color>,
    number: Option<ratatui::style::Color>,
    function: Option<ratatui::style::Color>,
    type_name: Option<ratatui::style::Color>,
}

impl HighlightPalette {
    pub fn for_theme(theme: &TuiTheme) -> Self {
        use crate::theme::ColorMode;
        if theme.capability.mode == ColorMode::None {
            return Self::default();
        }
        let Some(definition) = vibex_ui::theme_catalog::theme(theme.id) else {
            return Self::default();
        };
        let Ok(json) = serde_json::from_str::<serde_json::Value>(definition.highlight_json) else {
            return Self::default();
        };
        let lookup = |key: &str| -> Option<ratatui::style::Color> {
            let value = json.get("syntax")?.get(key)?.get("color")?.as_str()?;
            parse_hex_color(value).and_then(|rgb| theme.capability.color(rgb))
        };
        Self {
            keyword: lookup("keyword"),
            string: lookup("string"),
            comment: lookup("comment"),
            number: lookup("number"),
            function: lookup("function"),
            type_name: lookup("type"),
        }
    }

    fn highlight(
        &self,
        language: Option<&str>,
        line: &str,
        base: Style,
        theme: &TuiTheme,
    ) -> Vec<Span<'static>> {
        if self.keyword.is_none()
            && self.string.is_none()
            && self.comment.is_none()
            && self.number.is_none()
        {
            return vec![Span::styled(line.to_string(), base)];
        }
        let keywords = keywords_for(language);
        let bytes = line.as_bytes();
        let mut spans = Vec::new();
        let mut token = String::new();
        let mut token_style = base;
        let mut index = 0usize;
        let flush = |token: &mut String, style: Style, spans: &mut Vec<Span<'static>>| {
            if !token.is_empty() {
                spans.push(Span::styled(std::mem::take(token), style));
            }
        };
        while index < bytes.len() {
            let character = line[index..].chars().next().unwrap_or('\0');
            let width = character.len_utf8();
            // Comments run to end of line in every language we colour.
            if character == '#'
                || (character == '/' && line[index..].starts_with("//"))
                || (character == '-' && line[index..].starts_with("--"))
            {
                flush(&mut token, token_style, &mut spans);
                let style = self
                    .comment
                    .map_or(base, |color| Style::default().fg(color));
                spans.push(Span::styled(line[index..].to_string(), style));
                return spans;
            }
            if matches!(character, '"' | '\'' | '`') {
                flush(&mut token, token_style, &mut spans);
                let quote = character;
                let mut end = index + width;
                let mut escaped = false;
                while end < bytes.len() {
                    let next = line[end..].chars().next().unwrap_or('\0');
                    if escaped {
                        escaped = false;
                    } else if next == '\\' {
                        escaped = true;
                    } else if next == quote {
                        end += next.len_utf8();
                        break;
                    }
                    end += next.len_utf8();
                }
                let style = self.string.map_or(base, |color| Style::default().fg(color));
                spans.push(Span::styled(line[index..end].to_string(), style));
                index = end;
                continue;
            }
            if character.is_ascii_digit()
                && (token.is_empty() || !token.chars().last().is_some_and(|c| c.is_alphanumeric()))
            {
                flush(&mut token, token_style, &mut spans);
                let mut end = index;
                while end < bytes.len() {
                    let next = line[end..].chars().next().unwrap_or('\0');
                    if next.is_ascii_alphanumeric() || next == '.' || next == '_' {
                        end += next.len_utf8();
                    } else {
                        break;
                    }
                }
                let style = self.number.map_or(base, |color| Style::default().fg(color));
                spans.push(Span::styled(line[index..end].to_string(), style));
                index = end;
                continue;
            }
            if character.is_alphanumeric() || character == '_' {
                token.push(character);
                index += width;
                continue;
            }
            if !token.is_empty() {
                let style = if keywords.contains(&token.as_str()) {
                    self.keyword
                        .map_or(base, |color| Style::default().fg(color))
                } else if line[index..].starts_with('(') {
                    self.function
                        .map_or(base, |color| Style::default().fg(color))
                } else if token.chars().next().is_some_and(char::is_uppercase) {
                    self.type_name
                        .map_or(base, |color| Style::default().fg(color))
                } else {
                    base
                };
                flush(&mut token, style, &mut spans);
            }
            token_style = base;
            let mut buffer = String::new();
            buffer.push(character);
            spans.push(Span::styled(buffer, theme.base().patch(base)));
            index += width;
        }
        flush(&mut token, token_style, &mut spans);
        spans
    }
}

fn parse_hex_color(value: &str) -> Option<u32> {
    let hex = value.trim().trim_start_matches('#');
    if hex.len() < 6 {
        return None;
    }
    u32::from_str_radix(&hex[..6], 16).ok()
}

/// Keyword sets per language family. Unknown languages get no colouring rather
/// than wrong colouring.
fn keywords_for(language: Option<&str>) -> &'static [&'static str] {
    const RUST: &[&str] = &[
        "as", "async", "await", "break", "const", "continue", "crate", "dyn", "else", "enum",
        "extern", "false", "fn", "for", "if", "impl", "in", "let", "loop", "match", "mod", "move",
        "mut", "pub", "ref", "return", "self", "Self", "static", "struct", "super", "trait",
        "true", "type", "unsafe", "use", "where", "while",
    ];
    const PYTHON: &[&str] = &[
        "and", "as", "assert", "async", "await", "break", "class", "continue", "def", "del",
        "elif", "else", "except", "False", "finally", "for", "from", "global", "if", "import",
        "in", "is", "lambda", "None", "not", "or", "pass", "raise", "return", "True", "try",
        "while", "with", "yield",
    ];
    const JAVASCRIPT: &[&str] = &[
        "async",
        "await",
        "break",
        "case",
        "catch",
        "class",
        "const",
        "continue",
        "default",
        "delete",
        "do",
        "else",
        "export",
        "extends",
        "false",
        "finally",
        "for",
        "function",
        "if",
        "import",
        "in",
        "instanceof",
        "let",
        "new",
        "null",
        "return",
        "static",
        "super",
        "switch",
        "this",
        "throw",
        "true",
        "try",
        "typeof",
        "undefined",
        "var",
        "while",
        "yield",
    ];
    const SHELL: &[&str] = &[
        "case", "do", "done", "elif", "else", "esac", "fi", "for", "function", "if", "in", "local",
        "return", "then", "while", "export", "set", "source",
    ];
    const SQL: &[&str] = &[
        "ALTER", "AND", "AS", "BY", "CREATE", "DELETE", "DROP", "FROM", "GROUP", "HAVING", "INDEX",
        "INSERT", "INTO", "JOIN", "LIMIT", "NOT", "NULL", "ON", "OR", "ORDER", "SELECT", "SET",
        "TABLE", "UPDATE", "VALUES", "WHERE",
    ];
    const TOML_JSON: &[&str] = &["true", "false", "null"];

    match language.unwrap_or("").to_ascii_lowercase().as_str() {
        "rust" | "rs" => RUST,
        "python" | "py" => PYTHON,
        "javascript" | "js" | "typescript" | "ts" | "tsx" | "jsx" => JAVASCRIPT,
        "bash" | "sh" | "shell" | "zsh" | "fish" => SHELL,
        "sql" => SQL,
        "toml" | "json" | "yaml" | "yml" => TOML_JSON,
        _ => &[],
    }
}

/// Group a document's lines into searchable text, used by the transcript search.
pub fn search_index(rendered: &RenderedMarkdown) -> BTreeMap<usize, String> {
    rendered
        .plain
        .iter()
        .enumerate()
        .map(|(index, line)| (index, line.to_lowercase()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::locale::Locale;
    use crate::theme::{ColorCapability, ColorMode, GlyphMode, TuiTheme};
    use vibex_ui::GpuiThemeMode;

    fn theme(mode: ColorMode) -> TuiTheme {
        TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode,
                glyphs: GlyphMode::Unicode,
            },
        )
    }

    fn render(source: &str, width: usize) -> RenderedMarkdown {
        render_markdown(
            source,
            &theme(ColorMode::TrueColor),
            width,
            Strings::for_locale(Locale::En),
        )
    }

    #[test]
    fn paragraphs_wrap_within_the_width() {
        let rendered = render(
            "The quick brown fox jumps over the lazy dog and keeps running for a while.",
            24,
        );
        assert!(rendered.height() > 1);
        for line in &rendered.plain {
            assert!(display_width(line) <= 24, "{line:?} is too wide");
        }
    }

    #[test]
    fn headings_and_paragraphs_both_survive() {
        let rendered = render("# Title\n\nBody text.", 40);
        let text = rendered.text();
        assert!(text.contains("Title"));
        assert!(text.contains("Body text."));
    }

    #[test]
    fn code_blocks_are_indented_and_keep_their_content() {
        let rendered = render("```rust\nfn main() {}\n```", 40);
        let text = rendered.text();
        assert!(text.contains("fn main() {}"), "{text}");
        assert!(text.contains("rust"));
    }

    #[test]
    fn cjk_markdown_wraps_by_column_not_by_character_count() {
        let rendered = render(
            "这是一段需要换行的中文说明文字，应该按照列宽换行而不是按字符数。",
            20,
        );
        for line in &rendered.plain {
            assert!(display_width(line) <= 20, "{line:?}");
        }
        assert!(rendered.height() >= 2);
    }

    #[test]
    fn tables_render_with_borders_and_alignment() {
        let rendered = render(
            "| Name | Count |\n|:-----|------:|\n| a | 1 |\n| longer | 22 |",
            60,
        );
        let text = rendered.text();
        assert!(text.contains("Name"));
        assert!(text.contains("longer"));
        assert!(text.contains('│') || text.contains('|'));
    }

    #[test]
    fn lists_render_markers_and_task_state() {
        let rendered = render("- one\n- two\n\n1. first\n\n- [x] done\n- [ ] todo", 40);
        let text = rendered.text();
        assert!(text.contains("one"));
        assert!(text.contains("1. first"));
        assert!(text.contains('☑') || text.contains("[x]"));
        assert!(text.contains('☐') || text.contains("[ ]"));
    }

    #[test]
    fn diagrams_degrade_to_source_with_a_hint() {
        let rendered = render("```mermaid\ngraph TD; A-->B;\n```", 60);
        let text = rendered.text();
        assert!(text.contains("graph TD"), "{text}");
        assert!(text.contains("external tool"), "{text}");
    }

    #[test]
    fn images_degrade_to_a_placeholder() {
        let rendered = render("![diagram](https://example.test/a.png)", 60);
        let text = rendered.text();
        assert!(text.contains("diagram"), "{text}");
        assert!(text.contains("Image") || text.contains("image"), "{text}");
    }

    #[test]
    fn links_show_their_destination_when_the_label_differs() {
        let rendered = render("see [the docs](https://example.test/docs)", 80);
        let text = rendered.text();
        assert!(text.contains("the docs"));
        assert!(text.contains("https://example.test/docs"));
    }

    #[test]
    fn math_is_shown_as_source() {
        let rendered = render("$E = mc^2$", 40);
        assert!(rendered.text().contains("E = mc^2"));
    }

    #[test]
    fn no_color_mode_still_produces_all_the_text() {
        let rendered = render_markdown(
            "# Title\n\n- item\n\n```rust\nlet x = 1;\n```",
            &theme(ColorMode::None),
            40,
            Strings::for_locale(Locale::ZhCn),
        );
        let text = rendered.text();
        assert!(text.contains("Title"));
        assert!(text.contains("item"));
        assert!(text.contains("let x = 1;"));
    }

    #[test]
    fn ascii_glyph_mode_uses_ascii_markers() {
        let ascii = TuiTheme::resolve(
            Some("vibex-dark"),
            GpuiThemeMode::Dark,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Ascii,
            },
        );
        let rendered = render_markdown(
            "- item\n\n---\n\n| a | b |\n|---|---|\n| 1 | 2 |",
            &ascii,
            40,
            Strings::for_locale(Locale::En),
        );
        let text = rendered.text();
        assert!(text.contains("* item") || text.contains("- item"), "{text}");
        assert!(!text.contains('─'), "{text}");
        assert!(!text.contains('│'), "{text}");
    }

    #[test]
    fn highlight_palette_reads_the_theme_and_colours_keywords() {
        let palette = HighlightPalette::for_theme(&theme(ColorMode::TrueColor));
        assert!(palette.keyword.is_some());
        let spans = palette.highlight(
            Some("rust"),
            "let x = \"a\"; // note",
            Style::default(),
            &theme(ColorMode::TrueColor),
        );
        let text = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        assert_eq!(text, "let x = \"a\"; // note");
        // The keyword, string and comment must not all share one colour.
        let colours = spans
            .iter()
            .filter_map(|span| span.style.fg.map(|color| format!("{color:?}")))
            .collect::<std::collections::BTreeSet<_>>();
        assert!(colours.len() >= 3, "{colours:?}");
    }

    #[test]
    fn highlight_palette_is_empty_without_colour() {
        let palette = HighlightPalette::for_theme(&theme(ColorMode::None));
        assert!(palette.keyword.is_none());
        let spans = palette.highlight(None, "plain", Style::default(), &theme(ColorMode::None));
        assert_eq!(spans.len(), 1);
    }

    #[test]
    fn search_index_lowercases_every_line() {
        let rendered = render("Hello World", 40);
        let index = search_index(&rendered);
        assert!(index.values().any(|line| line.contains("hello world")));
    }

    #[test]
    fn plain_rendering_does_not_interpret_markup() {
        let rendered = render_plain(
            "# not a heading\n**not bold**",
            &theme(ColorMode::TrueColor),
            40,
        );
        let text = rendered.text();
        assert!(text.contains("# not a heading"));
        assert!(text.contains("**not bold**"));
    }
}

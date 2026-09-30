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
use unicode_segmentation::UnicodeSegmentation;

use crate::text::{display_width, take_width, wrap_text};
use crate::theme::TuiTheme;

/// A rendered markdown document: display lines plus the plain text of each,
/// which the copy action and the search index both need.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    render_markdown_with(source, theme, width, strings, theme.prose())
}

/// As [`render_markdown`], with the style ordinary prose is drawn in.
///
/// The caller knows what the block *is*: a user's own message keeps the
/// brightest text, while an Agent's answer is set one step down so its
/// headings and code can stand out.
pub fn render_markdown_with(
    source: &str,
    theme: &TuiTheme,
    width: usize,
    strings: Strings,
    prose: Style,
) -> RenderedMarkdown {
    let width = width.max(8);
    let input = MarkdownInput::new(source, "", 0);
    let document = parse_markdown(input);
    let mut builder = Builder {
        theme,
        strings,
        highlight: HighlightPalette::for_theme(theme),
        width,
        prose,
        lines: Vec::new(),
    };
    builder.blocks(&document.blocks, 0);
    builder.finish()
}

/// A markdown document that grows a chunk at a time.
///
/// Re-rendering the whole document on every delta is quadratic in the length of
/// the answer: a paragraph that took a millisecond to lay out at fifty tokens
/// takes a hundred times that at five thousand, once per token. This keeps a
/// *frozen prefix* instead — source bytes that can never be re-interpreted by
/// the text that follows — and re-renders only what comes after it.
///
/// A freeze point is a blank line that ends a top-level block: a paragraph, a
/// heading, a closed fence. Nothing inside a list, a quote, a table or an
/// unclosed fence qualifies, because the text that follows can still change how
/// those lines read.
#[derive(Debug, Clone, Default)]
pub struct StreamingMarkdown {
    /// Everything received so far.
    source: String,
    /// Bytes of `source` the frozen rows were rendered from.
    frozen_bytes: usize,
    /// Rows of [`Self::rendered`] that are frozen.
    frozen_rows: usize,
    /// The frozen rows followed by the live tail, in one buffer.
    ///
    /// One buffer rather than two documents joined per delta: a join would copy
    /// the whole answer again on every token, which is the cost this type
    /// exists to avoid.
    rendered: RenderedMarkdown,
}

impl StreamingMarkdown {
    pub fn new() -> Self {
        Self::default()
    }

    /// The text received so far.
    pub fn source(&self) -> &str {
        &self.source
    }

    /// Bytes that can no longer be re-interpreted, for tests and diagnostics.
    pub fn frozen_bytes(&self) -> usize {
        self.frozen_bytes
    }

    /// The document as it stands: rendered rows only.
    pub fn rendered(&self) -> &RenderedMarkdown {
        &self.rendered
    }

    /// Append a chunk and re-render only the unfrozen remainder.
    ///
    /// Returns the whole document, because that is what a caller painting a
    /// block needs; the frozen rows are reused rather than rebuilt.
    pub fn push(
        &mut self,
        delta: &str,
        theme: &TuiTheme,
        width: usize,
        strings: Strings,
        prose: Style,
    ) -> &RenderedMarkdown {
        if delta.is_empty() {
            return &self.rendered;
        }
        self.source.push_str(delta);
        self.rerender(theme, width, strings, prose);
        &self.rendered
    }

    fn rerender(&mut self, theme: &TuiTheme, width: usize, strings: Strings, prose: Style) {
        let checkpoint = freeze_point(&self.source, self.frozen_bytes);
        if checkpoint > self.frozen_bytes {
            // The stale tail is dropped before the newly settled source is
            // rendered once and appended: the rows before it are never looked
            // at again.
            self.rendered.lines.truncate(self.frozen_rows);
            self.rendered.plain.truncate(self.frozen_rows);
            let settled = render_markdown_with(
                &self.source[self.frozen_bytes..checkpoint],
                theme,
                width,
                strings,
                prose,
            );
            self.rendered.lines.extend(settled.lines);
            self.rendered.plain.extend(settled.plain);
            self.frozen_rows = self.rendered.lines.len();
            self.frozen_bytes = checkpoint;
        }
        let tail = render_markdown_with(
            &self.source[self.frozen_bytes..],
            theme,
            width,
            strings,
            prose,
        );
        self.rendered.lines.truncate(self.frozen_rows);
        self.rendered.plain.truncate(self.frozen_rows);
        self.rendered.lines.extend(tail.lines);
        self.rendered.plain.extend(tail.plain);
    }
}

/// Where the next freeze can be taken, at or after `from`.
///
/// The answer is a source offset: the end of the last blank line that follows a
/// complete top-level block. `from` itself is returned when nothing qualifies,
/// which is what keeps the frozen prefix monotonically growing.
pub fn freeze_point(source: &str, from: usize) -> usize {
    let mut checkpoint = from;
    let mut fence_open = false;
    let mut offset = 0usize;
    let mut previous: Option<&str> = None;
    for line in source.split_inclusive('\n') {
        let start = offset;
        offset += line.len();
        if start < from {
            previous = Some(line.trim_end_matches('\n'));
            continue;
        }
        let text = line.trim_end_matches('\n');
        let trimmed = text.trim_start();
        // A fence toggles the region that must never be frozen.
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fence_open = !fence_open;
            previous = Some(text);
            continue;
        }
        if text.trim().is_empty() && !fence_open && previous.is_some_and(is_top_level_block_end) {
            // Freeze after the blank line: the paragraph break belongs to the
            // frozen part, so the tail never starts with an empty row.
            checkpoint = offset;
        }
        previous = Some(text);
    }
    checkpoint
}

/// Whether a line can end a complete top-level block.
///
/// List items, quotes, table rows and indented lines continue into whatever
/// follows them, so a blank line after one of those is not a boundary the
/// renderer may cut at.
fn is_top_level_block_end(line: &str) -> bool {
    let trimmed = line.trim_start();
    if trimmed.is_empty() {
        return false;
    }
    if line.starts_with(' ') || line.starts_with('\t') {
        return false;
    }
    if trimmed.starts_with('>')
        || trimmed.starts_with('|')
        || trimmed.starts_with("- ")
        || trimmed.starts_with("* ")
        || trimmed.starts_with("+ ")
    {
        return false;
    }
    // An ordered list marker: digits followed by `.` or `)`.
    let digits = trimmed.chars().take_while(char::is_ascii_digit).count();
    if digits > 0 && trimmed[digits..].starts_with(['.', ')']) {
        return false;
    }
    true
}

/// Render plain text (no markdown syntax) with the same wrapping rules.
pub fn render_plain(source: &str, theme: &TuiTheme, width: usize) -> RenderedMarkdown {
    let mut lines = Vec::new();
    let plain_style = theme.prose();
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
    /// The style ordinary prose is drawn in.
    prose: Style,
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
        // The marker only decorates the first visual line; continuations line
        // up under the text so a long bullet stays readable.
        let hanging = " ".repeat(prefix_width);
        // The wrapped text is matched back onto the styled runs one segment at
        // a time, so emphasis, code and links keep their styling across a line
        // break instead of collapsing to whichever span came first.
        let mut cursor = RunCursor::new(&logical.spans);
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
            spans.extend(take_styled_segment(&mut cursor, &content));
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

    /// A line whose spacing is content: code, tables and rules are never
    /// re-flowed, because collapsing their runs of spaces destroys the layout
    /// they were written with.
    fn push_preformatted(
        &mut self,
        indent: usize,
        spans: Vec<Span<'static>>,
        background: Option<Style>,
    ) {
        let available = self.width.saturating_sub(indent).max(1);
        let body = spans
            .iter()
            .map(|span| span.content.as_ref())
            .collect::<String>();
        let mut cursor = RunCursor::new(&spans);
        for range in hard_wrap_ranges(&body, available) {
            let segment = &body[range];
            let mut line_spans = Vec::new();
            if indent > 0 {
                line_spans.push(Span::raw(" ".repeat(indent)));
            }
            line_spans.extend(take_styled_segment(&mut cursor, segment));
            let mut line = Line::from(line_spans);
            if let Some(background) = background {
                line = line.style(background);
            }
            let mut plain = " ".repeat(indent);
            plain.push_str(segment);
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
                // Emphasis carries the heading, not the source's `#`s: the
                // marker is markup, and printing it is noise the reader has to
                // skip on every heading. The *colour* carries the level, so a
                // document's outline is legible from the hue ladder alone and
                // survives a terminal whose CJK face has no bold cut.
                let index = usize::from((*level).clamp(1, 6)) - 1;
                let style = Style::default()
                    .fg(self.theme.markdown.heading[index])
                    .add_modifier(Modifier::BOLD);
                let mut spans = self.inlines_styled(content, style);
                if spans.is_empty() {
                    spans.push(Span::styled(String::new(), style));
                }
                self.logical_with(indent, Vec::new(), spans);
                self.blank();
            }
            Block::Quote(children) => {
                // A quiet bar, not a rule: the quote's own text is the content
                // and the bar only says where it starts.
                let marker = vec![Span::styled(
                    if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                        "▏ ".to_string()
                    } else {
                        "| ".to_string()
                    },
                    Style::default().fg(self.theme.markdown.quote),
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
                    let marker_style = match item.checked {
                        Some(true) => Style::default().fg(self.theme.markdown.task_done),
                        Some(false) => Style::default().fg(self.theme.markdown.task_todo),
                        None => Style::default().fg(self.theme.markdown.marker),
                    };
                    let marker = vec![Span::styled(marker_text, marker_style)];
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
                // Three cells, not a width-filling line: a rule is a pause in
                // the prose, and a full row of dashes reads as a table border.
                let glyph = if self.theme.glyphs() == crate::theme::GlyphMode::Unicode {
                    "─"
                } else {
                    "-"
                };
                self.logical_with(
                    indent,
                    Vec::new(),
                    vec![Span::styled(
                        glyph.repeat(3),
                        Style::default().fg(self.theme.markdown.rule),
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
        // Literals are text on the code background; a highlighter overrides
        // only the runs it recognises.
        let base = self.theme.code().fg(self.theme.markdown.code);
        if let Some(language) = language.filter(|value| !value.is_empty()) {
            self.logical_with(
                indent,
                Vec::new(),
                vec![Span::styled(
                    language.to_string(),
                    Style::default().fg(self.theme.markdown.code_language),
                )],
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
            self.push_preformatted(indent + 2, spans, Some(base));
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
            self.push_preformatted(
                indent + 2,
                vec![Span::styled(line.to_string(), style)],
                Some(base),
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
        let border = Style::default().fg(self.theme.markdown.table_border);
        // A table is a box: top, a rule under the header, and a closed bottom.
        // Without the bottom rule it reads as content that got cut off.
        let rule = |left: &str, middle: &str, right: &str, widths: &[usize]| {
            let (left, middle, right) = if glyphs {
                (left, middle, right)
            } else {
                ("+", "+", "+")
            };
            let mut text = String::from(left);
            for (index, width) in widths.iter().enumerate() {
                text.push_str(&horizontal.repeat(width + 2));
                text.push_str(if index + 1 == widths.len() {
                    right
                } else {
                    middle
                });
            }
            text
        };
        self.push_preformatted(
            indent,
            vec![Span::styled(rule("┌", "┬", "┐", &widths), border)],
            None,
        );
        for (row_index, row) in cells.iter().enumerate() {
            let mut spans = vec![Span::styled(format!("{vertical} "), border)];
            for (index, width) in widths.iter().enumerate() {
                let cell = row.get(index).map(String::as_str).unwrap_or_default();
                // Padding is measured in cells: `{:<width$}` counts
                // characters, which drags a CJK table's columns out of line.
                let padded = pad_cell(cell, *width, alignments.get(index).copied());
                let style = if row_index == 0 && header.is_some() {
                    self.theme.strong()
                } else {
                    self.theme.base()
                };
                spans.push(Span::styled(padded, style));
                // The closing edge carries no trailing space: a row must be
                // exactly as wide as the rule that closes it.
                let edge = if index + 1 == widths.len() {
                    format!(" {vertical}")
                } else {
                    format!(" {vertical} ")
                };
                spans.push(Span::styled(edge, border));
            }
            self.push_preformatted(indent, spans, None);
            if row_index == 0 && header.is_some() {
                self.push_preformatted(
                    indent,
                    vec![Span::styled(rule("├", "┼", "┤", &widths), border)],
                    None,
                );
            }
        }
        self.push_preformatted(
            indent,
            vec![Span::styled(rule("└", "┴", "┘", &widths), border)],
            None,
        );
    }

    fn inlines(&self, inlines: &[InlineNode]) -> Vec<Span<'static>> {
        self.inlines_styled(inlines, self.prose)
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
                // No backticks: the colour is the marker. A literal takes the
                // syntax palette's own colour rather than a background, so a
                // sentence with three code spans in it stays a sentence.
                out.push(Span::styled(
                    text.clone(),
                    Style::default().fg(self.theme.markdown.code),
                ));
            }
            Inline::Emphasis(children) => {
                self.inlines_into(children, style.add_modifier(Modifier::ITALIC), out);
            }
            Inline::Strong(children) => {
                // Bright as well as bold: a modifier alone is invisible in the
                // terminals whose CJK face has no bold cut, and a bold run at
                // prose brightness would not stand out from the prose anyway.
                self.inlines_into(
                    children,
                    style
                        .fg(self.theme.roles.foreground)
                        .add_modifier(Modifier::BOLD),
                    out,
                );
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
                        .fg(self.theme.roles.link),
                    out,
                );
                // Show the target when the label does not already name it.
                let label = plain_inlines(children);
                let target = resource_target(destination);
                if !label.contains(&target) {
                    out.push(Span::styled(
                        format!(" <{target}>"),
                        Style::default().fg(self.theme.markdown.link_target),
                    ));
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
                out.push(Span::styled(
                    format!("[^{label}]"),
                    Style::default().fg(self.theme.markdown.marker),
                ));
            }
        }
    }

    fn inlines_into(&self, nodes: &[InlineNode], style: Style, out: &mut Vec<Span<'static>>) {
        for node in nodes {
            self.inline(node, style, out);
        }
    }

    /// A logical line that carries a background, so wrapping pads the tail.
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

/// Pad one table cell to `width` display columns.
fn pad_cell(text: &str, width: usize, alignment: Option<vibex_markdown::TableAlignment>) -> String {
    let text = truncate(text, width);
    let padding = width.saturating_sub(display_width(&text));
    match alignment {
        Some(vibex_markdown::TableAlignment::Right) => {
            format!("{}{text}", " ".repeat(padding))
        }
        Some(vibex_markdown::TableAlignment::Center) => format!(
            "{}{text}{}",
            " ".repeat(padding / 2),
            " ".repeat(padding - padding / 2)
        ),
        _ => format!("{text}{}", " ".repeat(padding)),
    }
}

/// A position inside a logical line's styled runs.
struct RunCursor<'a> {
    runs: &'a [Span<'static>],
    run: usize,
    offset: usize,
}

impl<'a> RunCursor<'a> {
    fn new(runs: &'a [Span<'static>]) -> Self {
        Self {
            runs,
            run: 0,
            offset: 0,
        }
    }

    /// The unstyled text left in the current run, skipping exhausted runs.
    fn rest(&mut self) -> &'a str {
        while let Some(span) = self.runs.get(self.run) {
            if self.offset < span.content.len() {
                return &span.content[self.offset..];
            }
            self.run += 1;
            self.offset = 0;
        }
        ""
    }

    fn style(&self) -> Style {
        self.runs
            .get(self.run)
            .map(|span| span.style)
            .unwrap_or_default()
    }

    fn take(&mut self, bytes: usize) {
        self.offset += bytes;
    }
}

/// The styled runs of one wrapped segment.
///
/// `wrap_text` returns the text of each visual line rather than byte offsets,
/// because a break consumes the space it broke on and an over-long token is cut
/// in two. Walking the runs in step with each segment therefore matches on
/// characters rather than offsets: the wrapper stays the single authority on
/// where lines break, and every span keeps its colour and background across the
/// break.
fn take_styled_segment(cursor: &mut RunCursor<'_>, segment: &str) -> Vec<Span<'static>> {
    let mut spans: Vec<Span<'static>> = Vec::new();
    let mut remaining = segment;
    while let Some(character) = remaining.chars().next() {
        let rest = cursor.rest();
        match rest.chars().next() {
            Some(other) if other == character => {
                push_styled(&mut spans, character, cursor.style());
                cursor.take(character.len_utf8());
                remaining = &remaining[character.len_utf8()..];
            }
            // The wrapper dropped or collapsed a space at this point.
            Some(other) if other.is_whitespace() => cursor.take(other.len_utf8()),
            // The wrapper re-joined two tokens with a space of its own.
            None | Some(_) if character == ' ' => remaining = &remaining[1..],
            // A mismatch the wrapper should not produce: take the segment's
            // character rather than loop forever.
            _ => {
                push_styled(&mut spans, character, cursor.style());
                remaining = &remaining[character.len_utf8()..];
            }
        }
    }
    spans
}

/// Append a character, merging it into the previous span when the style is the
/// same so a paragraph does not become one span per grapheme.
fn push_styled(spans: &mut Vec<Span<'static>>, character: char, style: Style) {
    if let Some(last) = spans.last_mut()
        && last.style == style
    {
        let mut text = last.content.to_string();
        text.push(character);
        *last = Span::styled(text, style);
        return;
    }
    spans.push(Span::styled(character.to_string(), style));
}

/// Split preformatted text into ranges of at most `width` display columns.
fn hard_wrap_ranges(text: &str, width: usize) -> Vec<std::ops::Range<usize>> {
    let width = width.max(1);
    let mut ranges = Vec::new();
    let mut start = 0usize;
    let mut used = 0usize;
    for (offset, grapheme) in text.grapheme_indices(true) {
        let grapheme_width = display_width(grapheme);
        if used > 0 && used + grapheme_width > width {
            ranges.push(start..offset);
            start = offset;
            used = 0;
        }
        used += grapheme_width;
    }
    ranges.push(start..text.len());
    ranges
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
    /// The palette the theme already parsed.
    ///
    /// The catalogue ships the syntax colours as JSON; parsing it here would
    /// put a JSON parse inside every render, including every delta of a
    /// streaming answer.
    pub fn for_theme(theme: &TuiTheme) -> Self {
        let syntax = theme.syntax;
        Self {
            keyword: syntax.keyword,
            string: syntax.string,
            comment: syntax.comment,
            number: syntax.number,
            function: syntax.function,
            type_name: syntax.type_name,
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
        // A table is a box: the header rule and the bottom rule are both there.
        assert!(
            text.contains('┌') && text.contains('├') && text.contains('└'),
            "{text}"
        );
    }

    #[test]
    fn a_table_with_double_width_cells_stays_in_column() {
        // Character-counted padding put every `│` after a CJK cell in a
        // different column, which is what made a table look like spilled text.
        let rendered = render(
            "| 目录 | 内容 |\n| --- | --- |\n| `crates/` | 约 25 个库 |\n| `apps/` | 三个客户端 |",
            60,
        );
        let widths = rendered
            .plain
            .iter()
            .filter(|line| !line.is_empty())
            .map(|line| display_width(line))
            .collect::<Vec<_>>();
        assert!(widths.len() >= 5, "{:?}", rendered.plain);
        assert!(
            widths.windows(2).all(|pair| pair[0] == pair[1]),
            "the table rows are not the same width: {widths:?}\n{}",
            rendered.text()
        );
    }

    #[test]
    fn inline_code_and_headings_hide_their_markup() {
        let rendered = render("## 项目概览\n\n用 `pnpm check:rust` 检查。", 40);
        let text = rendered.text();
        assert!(
            !text.contains('#'),
            "the heading marker is still printed: {text}"
        );
        assert!(
            !text.contains('`'),
            "the code backticks are still printed: {text}"
        );
        assert!(text.contains("项目概览"), "{text}");
        assert!(text.contains("pnpm check:rust"), "{text}");
        // The code span carries the literal colour rather than a pair of
        // backticks, so it is still identifiable as code — and it keeps the
        // prose background, because a sentence with a code span in it is still
        // a sentence.
        let palette = theme(ColorMode::TrueColor);
        let code = rendered
            .lines
            .iter()
            .flat_map(|line| line.spans.iter())
            .find(|span| span.content.contains("pnpm check:rust"))
            .expect("the code span is rendered");
        assert_eq!(code.style.bg, None);
        assert_eq!(code.style.fg, Some(palette.markdown.code));
    }

    #[test]
    fn emphasis_and_code_survive_a_line_break() {
        // The styled span is neither first nor last, and the paragraph wraps:
        // re-applying only the first span's style to each visual line is what
        // erased every markdown cue in a wrapped paragraph.
        let rendered = render(
            "aaaaaaaa bbbb **bold words here** cccc `code_span` dddd eeee ffff gggg",
            24,
        );
        assert!(rendered.height() >= 2, "the paragraph did not wrap");
        let by_style = |wanted: Style| {
            rendered
                .lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .filter(|span| {
                    span.style.add_modifier == wanted.add_modifier && span.style.bg == wanted.bg
                })
                .map(|span| span.content.as_ref())
                .collect::<String>()
        };
        let palette = theme(ColorMode::TrueColor);
        let bold = by_style(
            palette
                .prose()
                .fg(palette.roles.foreground)
                .add_modifier(Modifier::BOLD),
        );
        assert!(bold.contains("bold"), "bold text lost its style: {bold:?}");
        let code = by_style(Style::default().fg(palette.markdown.code));
        assert!(
            code.contains("code"),
            "the code span lost its colour: {code:?}"
        );
    }

    #[test]
    fn code_blocks_keep_their_alignment() {
        // Runs of spaces inside a code line are content: collapsing them
        // destroys an aligned comment block.
        let rendered = render("```sh\npnpm dev   # start\npnpm check # gate\n```", 60);
        let line = rendered
            .plain
            .iter()
            .find(|line| line.contains("pnpm dev"))
            .expect("the code line is rendered");
        assert!(
            line.contains("dev   #"),
            "the alignment was collapsed: {line:?}"
        );
    }

    #[test]
    fn prose_headings_code_and_links_are_four_different_colours() {
        // The complaint this guards against: everything on screen is white.
        // A single-colour theme still has to spread what it has across the
        // roles that carry meaning.
        let palette = theme(ColorMode::TrueColor);
        let rendered = render(
            "# 标题\n\n正文 `code` 与 [链接](https://example.test) 还有 **重点**。",
            60,
        );
        let colour_of = |needle: &str| {
            rendered
                .lines
                .iter()
                .flat_map(|line| line.spans.iter())
                .find(|span| span.content.contains(needle))
                .unwrap_or_else(|| panic!("{needle} is not rendered"))
                .style
                .fg
        };
        let prose = colour_of("正文");
        let heading = colour_of("标题");
        let code = colour_of("code");
        let link = colour_of("链接");
        assert_eq!(
            prose,
            Some(palette.roles.gray_bright),
            "prose is not dimmed"
        );
        assert_eq!(
            heading,
            Some(palette.markdown.heading[0]),
            "heading does not wear its level's colour"
        );
        assert_eq!(
            code,
            Some(palette.markdown.code),
            "code is not its own colour"
        );
        assert_eq!(
            link,
            Some(palette.markdown.link),
            "link is not its own colour"
        );
        let distinct = [prose, heading, code, link]
            .into_iter()
            .map(|colour| format!("{colour:?}"))
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(
            distinct.len(),
            4,
            "the four roles collapsed into fewer colours"
        );
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

    /// The document streamed in chunks must render exactly like the whole
    /// document, or the last frame of a stream would differ from a reload.
    #[test]
    fn a_streamed_document_renders_like_a_finished_one() {
        let source = "\
# Title

First paragraph with `code` in it.

- one
- two

```rust
fn main() {}
```

Second paragraph that arrives later.

---

> quoted line
";
        let theme = theme(ColorMode::TrueColor);
        let strings = Strings::for_locale(Locale::En);
        let mut stream = StreamingMarkdown::new();
        let mut grown = String::new();
        for chunk in source.as_bytes().chunks(7) {
            let chunk = std::str::from_utf8(chunk).unwrap();
            grown.push_str(chunk);
            let streamed = stream.push(chunk, &theme, 40, strings, theme.prose());
            let whole = render(grown.as_str(), 40);
            assert_eq!(
                streamed.plain.join("\n").trim_end(),
                whole.plain.join("\n").trim_end(),
                "streaming diverged after {grown:?}"
            );
        }
    }

    #[test]
    fn the_frozen_prefix_only_grows_at_settled_boundaries() {
        let theme = theme(ColorMode::TrueColor);
        let strings = Strings::for_locale(Locale::En);
        let mut stream = StreamingMarkdown::new();
        stream.push("first paragraph", &theme, 40, strings, theme.prose());
        assert_eq!(
            stream.frozen_bytes(),
            0,
            "an unterminated paragraph is not settled"
        );
        stream.push(" continued\n\n", &theme, 40, strings, theme.prose());
        let settled = stream.frozen_bytes();
        assert!(settled > 0, "a blank line ends a top-level block");
        stream.push("second paragraph", &theme, 40, strings, theme.prose());
        assert_eq!(
            stream.frozen_bytes(),
            settled,
            "text inside the live paragraph moved the freeze point"
        );
    }

    #[test]
    fn nothing_inside_a_structure_is_frozen() {
        // A list continues across blank lines, an open fence swallows
        // everything after it, and a table's rows belong together: freezing
        // any of them would re-render the tail out of context.
        let cases = [
            "- one\n\n- two\n\n",
            "1. one\n\n2. two\n\n",
            "> quoted\n\n> more\n\n",
            "| a | b |\n\n| - | - |\n\n",
            "```rust\ncode\n\nstill code\n",
            "  indented\n\n  still indented\n\n",
        ];
        for case in cases {
            assert_eq!(freeze_point(case, 0), 0, "froze inside {case:?}");
        }
        // A closed fence *is* settled.
        let closed = "```rust\ncode\n```\n\nnext\n";
        assert!(freeze_point(closed, 0) > 0, "a closed fence can freeze");
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

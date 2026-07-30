use std::collections::BTreeMap;
use std::sync::Arc;

use pulldown_cmark::{
    Alignment, BlockQuoteKind, CodeBlockKind, Event, HeadingLevel, Options, Parser, Tag, TagEnd,
};

use crate::html::{HtmlParseResult, parse_html_fragment};
use crate::limits::{MarkdownLimits, bounded_text, utf8_prefix};
use crate::model::{
    Block, BlockNode, CalloutKind, DefinitionItem, DiagnosticSeverity, DiagramKind, FootnoteIndex,
    Inline, InlineImage, InlineNode, ListItem, MarkdownDiagnostic, MarkdownDocument, MarkdownInput,
    NodeId, OutlineEntry, SourceRange, TableAlignment, TableRow, plain_text, stable_node_id,
};
use crate::resource::{ResolvedResource, ResourcePolicy, ResourceRole};

pub fn parse_markdown(input: MarkdownInput) -> MarkdownDocument {
    parse_markdown_with_limits(input, MarkdownLimits::default())
}

pub fn parse_markdown_with_limits(
    input: MarkdownInput,
    limits: MarkdownLimits,
) -> MarkdownDocument {
    ParserState::new(input, limits).parse()
}

struct ParserState {
    input: MarkdownInput,
    source: Arc<str>,
    limits: MarkdownLimits,
    policy: ResourcePolicy,
    stack: Vec<Frame>,
    blocks: Vec<BlockNode>,
    outline: Vec<OutlineEntry>,
    footnotes: FootnoteIndex,
    resources: Vec<ResolvedResource>,
    diagnostics: Vec<MarkdownDiagnostic>,
    slug_counts: BTreeMap<String, usize>,
    node_count: usize,
    occurrence: u32,
    truncated: bool,
}

struct Frame {
    start: usize,
    expected: Option<TagEnd>,
    kind: FrameKind,
}

enum FrameKind {
    Paragraph(Vec<InlineNode>),
    Heading {
        level: u8,
        explicit_id: Option<String>,
        content: Vec<InlineNode>,
    },
    Quote {
        kind: Option<CalloutKind>,
        children: Vec<BlockNode>,
    },
    Code {
        language: Option<String>,
        fenced: bool,
        source: String,
    },
    HtmlBlock(String),
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    Item {
        checked: Option<bool>,
        children: Vec<BlockNode>,
    },
    Footnote {
        label: String,
        children: Vec<BlockNode>,
    },
    DefinitionList(Vec<DefinitionItem>),
    DefinitionTitle(Vec<InlineNode>),
    Definition(Vec<BlockNode>),
    Table {
        alignments: Vec<TableAlignment>,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    TableHead(Vec<Vec<InlineNode>>),
    TableRow(Vec<Vec<InlineNode>>),
    TableCell(Vec<InlineNode>),
    InlineContainer {
        kind: InlineContainerKind,
        children: Vec<InlineNode>,
    },
    Link {
        destination: String,
        title: Option<String>,
        children: Vec<InlineNode>,
    },
    Image {
        destination: String,
        title: Option<String>,
        children: Vec<InlineNode>,
    },
    Metadata(String),
    InlineHtml {
        tag: String,
        nesting: usize,
    },
}

#[derive(Clone, Copy)]
enum InlineContainerKind {
    Emphasis,
    Strong,
    Deletion,
    Superscript,
    Subscript,
}

impl ParserState {
    fn new(input: MarkdownInput, limits: MarkdownLimits) -> Self {
        let (source, truncated) = if input.source.len() > limits.max_source_bytes {
            (
                Arc::<str>::from(utf8_prefix(&input.source, limits.max_source_bytes)),
                true,
            )
        } else {
            (input.source.clone(), false)
        };
        let policy = ResourcePolicy::new(&input.base_path);
        Self {
            input,
            source,
            limits,
            policy,
            stack: Vec::new(),
            blocks: Vec::new(),
            outline: Vec::new(),
            footnotes: FootnoteIndex::default(),
            resources: Vec::new(),
            diagnostics: Vec::new(),
            slug_counts: BTreeMap::new(),
            node_count: 0,
            occurrence: 0,
            truncated,
        }
    }

    fn parse(mut self) -> MarkdownDocument {
        if self.truncated {
            self.diagnostic(
                "markdown_source_limit",
                Some(SourceRange::new(0, self.source.len())),
                "Markdown source exceeded the byte limit and was truncated",
            );
        }
        let options = Options::ENABLE_TABLES
            | Options::ENABLE_FOOTNOTES
            | Options::ENABLE_STRIKETHROUGH
            | Options::ENABLE_TASKLISTS
            | Options::ENABLE_HEADING_ATTRIBUTES
            | Options::ENABLE_MATH
            | Options::ENABLE_GFM
            | Options::ENABLE_DEFINITION_LIST
            | Options::ENABLE_SUPERSCRIPT
            | Options::ENABLE_SUBSCRIPT;
        let source = self.source.clone();
        for (event, offset) in Parser::new_ext(&source, options).into_offset_iter() {
            let range = SourceRange::from(offset);
            if self.handle_inline_html_capture(&event, range) {
                continue;
            }
            if self.stack.len() >= self.limits.max_depth {
                return self.literal_document(
                    "markdown_depth_limit",
                    "Markdown nesting exceeded the document limit",
                );
            }
            match event {
                Event::Start(tag) => self.start(tag, range),
                Event::End(end) => self.end(end, range),
                Event::Text(text) => self.text(text.as_ref(), range),
                Event::Code(code) => {
                    self.push_inline(Inline::Code(code.into_string()), range, "code")
                }
                Event::InlineMath(math) => {
                    self.push_inline(Inline::Math(math.into_string()), range, "inline_math")
                }
                Event::DisplayMath(math) => {
                    let source = self.bounded_code(math.as_ref(), range);
                    let block = self.block(Block::Math { source }, range);
                    self.push_block(block);
                }
                Event::Html(html) => self.raw_html(html.as_ref(), range, false),
                Event::InlineHtml(html) => self.raw_html(html.as_ref(), range, true),
                Event::FootnoteReference(label) => {
                    let label = label.into_string();
                    let node = self.inline(Inline::FootnoteReference(label.clone()), range, &label);
                    self.footnotes
                        .references
                        .entry(label)
                        .or_default()
                        .push(node.id);
                    self.push_inline_node(node);
                }
                Event::SoftBreak | Event::HardBreak => {
                    self.push_inline(Inline::Break, range, "break")
                }
                Event::Rule => {
                    let block = self.block(Block::ThematicBreak, range);
                    self.push_block(block);
                }
                Event::TaskListMarker(checked) => self.set_task_marker(checked),
            }
            if self.node_count > self.limits.max_nodes {
                return self.literal_document(
                    "markdown_node_limit",
                    "Markdown node count exceeded the document limit",
                );
            }
        }

        while let Some(frame) = self.stack.pop() {
            let range = SourceRange::new(frame.start, self.source.len());
            match frame.kind {
                FrameKind::InlineHtml { .. } => {
                    let literal = self.source_slice(range).to_string();
                    self.push_inline(Inline::Literal(literal), range, "incomplete_inline_html");
                }
                _ => {
                    self.diagnostic(
                        "markdown_incomplete_structure",
                        Some(range),
                        "Incomplete trailing Markdown was kept as source",
                    );
                    let literal = self.source_slice(range).to_string();
                    let block = self.block(Block::Literal(literal), range);
                    self.push_block(block);
                }
            }
        }

        MarkdownDocument {
            source: self.source,
            base_path: self.input.base_path,
            revision: self.input.revision,
            blocks: self.blocks.into(),
            outline: self.outline.into(),
            footnotes: self.footnotes,
            resources: self.resources.into(),
            diagnostics: self.diagnostics.into(),
            truncated: self.truncated,
        }
    }

    fn start(&mut self, tag: Tag<'_>, range: SourceRange) {
        let (expected, kind) = match tag {
            Tag::Paragraph => (TagEnd::Paragraph, FrameKind::Paragraph(Vec::new())),
            Tag::Heading { level, id, .. } => (
                TagEnd::Heading(level),
                FrameKind::Heading {
                    level: heading_level(level),
                    explicit_id: id.map(|id| id.into_string()),
                    content: Vec::new(),
                },
            ),
            Tag::BlockQuote(kind) => (
                TagEnd::BlockQuote(kind),
                FrameKind::Quote {
                    kind: kind.map(callout_kind),
                    children: Vec::new(),
                },
            ),
            Tag::CodeBlock(kind) => {
                let (language, fenced) = match kind {
                    CodeBlockKind::Indented => (None, false),
                    CodeBlockKind::Fenced(info) => {
                        let language = info
                            .split_whitespace()
                            .next()
                            .filter(|value| !value.is_empty())
                            .map(normalize_language);
                        (language, true)
                    }
                };
                (
                    TagEnd::CodeBlock,
                    FrameKind::Code {
                        language,
                        fenced,
                        source: String::new(),
                    },
                )
            }
            Tag::HtmlBlock => (TagEnd::HtmlBlock, FrameKind::HtmlBlock(String::new())),
            Tag::List(start) => (
                TagEnd::List(start.is_some()),
                FrameKind::List {
                    start,
                    items: Vec::new(),
                },
            ),
            Tag::Item => (
                TagEnd::Item,
                FrameKind::Item {
                    checked: None,
                    children: Vec::new(),
                },
            ),
            Tag::FootnoteDefinition(label) => (
                TagEnd::FootnoteDefinition,
                FrameKind::Footnote {
                    label: label.into_string(),
                    children: Vec::new(),
                },
            ),
            Tag::DefinitionList => (
                TagEnd::DefinitionList,
                FrameKind::DefinitionList(Vec::new()),
            ),
            Tag::DefinitionListTitle => (
                TagEnd::DefinitionListTitle,
                FrameKind::DefinitionTitle(Vec::new()),
            ),
            Tag::DefinitionListDefinition => (
                TagEnd::DefinitionListDefinition,
                FrameKind::Definition(Vec::new()),
            ),
            Tag::Table(alignments) => (
                TagEnd::Table,
                FrameKind::Table {
                    alignments: alignments.into_iter().map(table_alignment).collect(),
                    header: None,
                    rows: Vec::new(),
                },
            ),
            Tag::TableHead => (TagEnd::TableHead, FrameKind::TableHead(Vec::new())),
            Tag::TableRow => (TagEnd::TableRow, FrameKind::TableRow(Vec::new())),
            Tag::TableCell => (TagEnd::TableCell, FrameKind::TableCell(Vec::new())),
            Tag::Emphasis => (
                TagEnd::Emphasis,
                FrameKind::InlineContainer {
                    kind: InlineContainerKind::Emphasis,
                    children: Vec::new(),
                },
            ),
            Tag::Strong => (
                TagEnd::Strong,
                FrameKind::InlineContainer {
                    kind: InlineContainerKind::Strong,
                    children: Vec::new(),
                },
            ),
            Tag::Strikethrough => (
                TagEnd::Strikethrough,
                FrameKind::InlineContainer {
                    kind: InlineContainerKind::Deletion,
                    children: Vec::new(),
                },
            ),
            Tag::Superscript => (
                TagEnd::Superscript,
                FrameKind::InlineContainer {
                    kind: InlineContainerKind::Superscript,
                    children: Vec::new(),
                },
            ),
            Tag::Subscript => (
                TagEnd::Subscript,
                FrameKind::InlineContainer {
                    kind: InlineContainerKind::Subscript,
                    children: Vec::new(),
                },
            ),
            Tag::Link {
                dest_url, title, ..
            } => (
                TagEnd::Link,
                FrameKind::Link {
                    destination: dest_url.into_string(),
                    title: (!title.is_empty()).then(|| title.into_string()),
                    children: Vec::new(),
                },
            ),
            Tag::Image {
                dest_url, title, ..
            } => (
                TagEnd::Image,
                FrameKind::Image {
                    destination: dest_url.into_string(),
                    title: (!title.is_empty()).then(|| title.into_string()),
                    children: Vec::new(),
                },
            ),
            Tag::MetadataBlock(kind) => (
                TagEnd::MetadataBlock(kind),
                FrameKind::Metadata(String::new()),
            ),
        };
        self.stack.push(Frame {
            start: range.start,
            expected: Some(expected),
            kind,
        });
    }

    fn end(&mut self, end: TagEnd, event_range: SourceRange) {
        let Some(frame) = self.stack.pop() else {
            self.diagnostic(
                "markdown_unbalanced_end",
                Some(event_range),
                "Markdown emitted an unmatched closing event",
            );
            return;
        };
        if frame.expected.as_ref() != Some(&end) {
            self.diagnostic(
                "markdown_unbalanced_structure",
                Some(event_range),
                "Markdown container events were not balanced",
            );
        }
        let range = SourceRange::new(frame.start, event_range.end);
        let source_text = self.source_slice(range).to_string();
        match frame.kind {
            FrameKind::Paragraph(content) => {
                let text = plain_text(&content);
                let kind = if text.trim().eq_ignore_ascii_case("[toc]") {
                    Block::TableOfContents
                } else {
                    Block::Paragraph(content)
                };
                let block = self.block(kind, range);
                self.push_block(block);
            }
            FrameKind::Heading {
                level,
                explicit_id,
                content,
            } => {
                let title = plain_text(&content);
                let base = explicit_id
                    .filter(|id| valid_heading_id(id))
                    .unwrap_or_else(|| slugify(&title));
                let slug = self.unique_slug(base);
                let block = self.block(
                    Block::Heading {
                        level,
                        slug: slug.clone(),
                        content,
                    },
                    range,
                );
                self.outline.push(OutlineEntry {
                    node_id: block.id,
                    level,
                    slug,
                    title,
                    range,
                });
                self.push_block(block);
            }
            FrameKind::Quote { kind, children } => {
                let kind = kind.map_or(Block::Quote(children.clone()), |kind| Block::Callout {
                    kind,
                    title: kind.title().into(),
                    children,
                });
                let block = self.block(kind, range);
                self.push_block(block);
            }
            FrameKind::Code {
                language,
                fenced,
                source,
            } => {
                let source = self.bounded_code(&source, range);
                let kind = match language.as_deref() {
                    Some("mermaid") => Block::Diagram {
                        kind: DiagramKind::Mermaid,
                        source,
                    },
                    Some("plantuml" | "puml") => Block::Diagram {
                        kind: DiagramKind::PlantUml,
                        source,
                    },
                    Some("diff" | "patch") => Block::Diff { source },
                    Some("math" | "latex" | "tex") => Block::Math { source },
                    _ => Block::Code {
                        language,
                        source,
                        fenced,
                    },
                };
                let block = self.block(kind, range);
                self.push_block(block);
            }
            FrameKind::HtmlBlock(html) => self.push_html_blocks(&html, range),
            FrameKind::List { start, items } => {
                let block = self.block(Block::List { start, items }, range);
                self.push_block(block);
            }
            FrameKind::Item { checked, children } => {
                let id = self.id("list_item", range, &source_text);
                let item = ListItem {
                    id,
                    range,
                    checked,
                    children,
                };
                if let Some(FrameKind::List { items, .. }) =
                    self.stack.last_mut().map(|frame| &mut frame.kind)
                {
                    items.push(item);
                } else {
                    self.diagnostic(
                        "markdown_list_item_orphaned",
                        Some(range),
                        "A list item appeared outside a list",
                    );
                }
            }
            FrameKind::Footnote { label, children } => {
                let block = self.block(
                    Block::FootnoteDefinition {
                        label: label.clone(),
                        children,
                    },
                    range,
                );
                self.footnotes.definitions.insert(label, block.id);
                self.push_block(block);
            }
            FrameKind::DefinitionList(items) => {
                let block = self.block(Block::DefinitionList(items), range);
                self.push_block(block);
            }
            FrameKind::DefinitionTitle(term) => {
                if let Some(FrameKind::DefinitionList(items)) =
                    self.stack.last_mut().map(|frame| &mut frame.kind)
                {
                    items.push(DefinitionItem {
                        term,
                        definitions: Vec::new(),
                    });
                }
            }
            FrameKind::Definition(definition) => {
                if let Some(FrameKind::DefinitionList(items)) =
                    self.stack.last_mut().map(|frame| &mut frame.kind)
                    && let Some(item) = items.last_mut()
                {
                    item.definitions.push(definition);
                }
            }
            FrameKind::Table {
                alignments,
                header,
                rows,
            } => {
                let block = self.block(
                    Block::Table {
                        alignments,
                        header,
                        rows,
                    },
                    range,
                );
                self.push_block(block);
            }
            FrameKind::TableHead(cells) => {
                if let Some(FrameKind::Table { header, .. }) =
                    self.stack.last_mut().map(|frame| &mut frame.kind)
                {
                    *header = Some(TableRow { cells });
                }
            }
            FrameKind::TableRow(cells) => {
                if let Some(FrameKind::Table { rows, .. }) =
                    self.stack.last_mut().map(|frame| &mut frame.kind)
                {
                    rows.push(TableRow { cells });
                }
            }
            FrameKind::TableCell(content) => {
                if let Some(frame) = self.stack.last_mut() {
                    match &mut frame.kind {
                        FrameKind::TableHead(cells) | FrameKind::TableRow(cells) => {
                            cells.push(content)
                        }
                        _ => {}
                    }
                }
            }
            FrameKind::InlineContainer { kind, children } => {
                let kind = match kind {
                    InlineContainerKind::Emphasis => Inline::Emphasis(children),
                    InlineContainerKind::Strong => Inline::Strong(children),
                    InlineContainerKind::Deletion => Inline::Deletion(children),
                    InlineContainerKind::Superscript => Inline::Superscript(children),
                    InlineContainerKind::Subscript => Inline::Subscript(children),
                };
                self.push_inline(kind, range, "inline_container");
            }
            FrameKind::Link {
                destination,
                title,
                children,
            } => {
                let label = plain_text(&children);
                let destination = self.resource(
                    ResourceRole::Link,
                    &destination,
                    (!label.is_empty()).then_some(label.as_str()),
                    range,
                );
                self.push_inline(
                    Inline::Link {
                        destination,
                        title,
                        children,
                    },
                    range,
                    "link",
                );
            }
            FrameKind::Image {
                destination,
                title,
                children,
            } => {
                let alt = plain_text(&children);
                let destination = self.resource(
                    ResourceRole::Image,
                    &destination,
                    (!alt.is_empty()).then_some(alt.as_str()),
                    range,
                );
                self.push_inline(
                    Inline::Image(InlineImage {
                        destination,
                        alt,
                        title,
                    }),
                    range,
                    "image",
                );
            }
            FrameKind::Metadata(source) => {
                let block = self.block(Block::Literal(source), range);
                self.push_block(block);
            }
            FrameKind::InlineHtml { .. } => {
                let literal = self.source_slice(range).to_string();
                self.push_inline(Inline::Literal(literal), range, "inline_html");
            }
        }
    }

    fn text(&mut self, text: &str, range: SourceRange) {
        match self.stack.last_mut().map(|frame| &mut frame.kind) {
            Some(FrameKind::Code { source, .. })
            | Some(FrameKind::HtmlBlock(source))
            | Some(FrameKind::Metadata(source)) => source.push_str(text),
            _ => self.push_inline(Inline::Text(text.to_string()), range, "text"),
        }
    }

    fn raw_html(&mut self, html: &str, range: SourceRange, inline: bool) {
        if matches!(
            self.stack.last().map(|frame| &frame.kind),
            Some(FrameKind::HtmlBlock(_))
        ) {
            if let Some(FrameKind::HtmlBlock(source)) =
                self.stack.last_mut().map(|frame| &mut frame.kind)
            {
                source.push_str(html);
            }
            return;
        }
        if inline {
            let token = html.trim();
            if let Some(tag) = opening_inline_tag(token) {
                if is_void_inline_tag(token, &tag) {
                    self.push_inline_html_fragment(token, range);
                } else {
                    self.stack.push(Frame {
                        start: range.start,
                        expected: None,
                        kind: FrameKind::InlineHtml { tag, nesting: 0 },
                    });
                }
                return;
            }
        }
        self.push_html_blocks(html, range);
    }

    fn handle_inline_html_capture(&mut self, event: &Event<'_>, range: SourceRange) -> bool {
        let Some(Frame {
            start,
            kind: FrameKind::InlineHtml { tag, nesting },
            ..
        }) = self.stack.last_mut()
        else {
            return false;
        };
        let Event::InlineHtml(token) = event else {
            return true;
        };
        let token = token.trim();
        if let Some(opening) = opening_inline_tag(token)
            && !token.starts_with("</")
        {
            if is_void_inline_tag(token, &opening) {
                return true;
            }
            *nesting = nesting.saturating_add(1);
            return true;
        }
        if closing_inline_tag(token).is_some() {
            if *nesting > 0 {
                *nesting -= 1;
                return true;
            }
            let start = *start;
            let expected = tag.clone();
            if closing_inline_tag(token).as_deref() == Some(expected.as_str()) {
                self.stack.pop();
                let full_range = SourceRange::new(start, range.end);
                let html = self.source_slice(full_range).to_string();
                self.push_inline_html_fragment(&html, full_range);
            }
            return true;
        }
        true
    }

    fn push_inline_html_fragment(&mut self, html: &str, range: SourceRange) {
        let parsed = self.parse_html(html, range);
        self.extend_html_metadata(parsed.resources, parsed.diagnostics);
        for mut block in parsed.blocks {
            self.index_html_headings(&mut block);
            match block.kind {
                Block::Paragraph(inlines) => {
                    for inline in inlines {
                        self.push_inline_node(inline);
                    }
                }
                Block::Image(image) => self.push_inline(Inline::Image(image), range, "html_image"),
                Block::SafeHtml(blocks) => {
                    let block = self.block(Block::SafeHtml(blocks), range);
                    self.push_block(block);
                }
                kind => {
                    let block = self.block(kind, range);
                    self.push_block(block);
                }
            }
        }
    }

    fn push_html_blocks(&mut self, html: &str, range: SourceRange) {
        let parsed = self.parse_html(html, range);
        self.extend_html_metadata(parsed.resources, parsed.diagnostics);
        for mut block in parsed.blocks {
            self.index_html_headings(&mut block);
            self.push_block(block);
        }
    }

    fn parse_html(&mut self, html: &str, range: SourceRange) -> HtmlParseResult {
        let mut limits = self.limits;
        limits.max_nodes = limits.max_nodes.saturating_sub(self.node_count);
        limits.max_resources = limits.max_resources.saturating_sub(self.resources.len());
        limits.max_diagnostics = limits
            .max_diagnostics
            .saturating_sub(self.diagnostics.len());
        let parsed = parse_html_fragment(html, range, &self.policy, limits);
        if parsed
            .diagnostics
            .iter()
            .any(|diagnostic| diagnostic.code == "markdown_html_limit")
        {
            self.truncated = true;
        }
        self.node_count = self.node_count.saturating_add(parsed.node_count);
        parsed
    }

    fn index_html_headings(&mut self, block: &mut BlockNode) {
        if let Block::Heading {
            level,
            slug,
            content,
        } = &mut block.kind
        {
            let title = plain_text(content);
            *slug = self.unique_slug(slugify(&title));
            self.outline.push(OutlineEntry {
                node_id: block.id,
                level: *level,
                slug: slug.clone(),
                title,
                range: block.range,
            });
        }
        match &mut block.kind {
            Block::Quote(children)
            | Block::Callout { children, .. }
            | Block::Details { children, .. }
            | Block::FootnoteDefinition { children, .. }
            | Block::SafeHtml(children) => {
                for child in children {
                    self.index_html_headings(child);
                }
            }
            Block::List { items, .. } => {
                for item in items {
                    for child in &mut item.children {
                        self.index_html_headings(child);
                    }
                }
            }
            Block::DefinitionList(items) => {
                for item in items {
                    for definition in &mut item.definitions {
                        for child in definition {
                            self.index_html_headings(child);
                        }
                    }
                }
            }
            _ => {}
        }
    }

    fn extend_html_metadata(
        &mut self,
        resources: Vec<ResolvedResource>,
        diagnostics: Vec<MarkdownDiagnostic>,
    ) {
        let available_resources = self
            .limits
            .max_resources
            .saturating_sub(self.resources.len());
        self.resources
            .extend(resources.into_iter().take(available_resources));
        let available_diagnostics = self
            .limits
            .max_diagnostics
            .saturating_sub(self.diagnostics.len());
        self.diagnostics
            .extend(diagnostics.into_iter().take(available_diagnostics));
    }

    fn push_inline(&mut self, kind: Inline, range: SourceRange, kind_name: &str) {
        let content = self.source_slice(range).to_string();
        let node = self.inline(
            kind,
            range,
            if content.is_empty() {
                kind_name
            } else {
                &content
            },
        );
        self.push_inline_node(node);
    }

    fn push_inline_node(&mut self, node: InlineNode) {
        for frame in self.stack.iter_mut().rev() {
            match &mut frame.kind {
                FrameKind::Paragraph(children)
                | FrameKind::Heading {
                    content: children, ..
                }
                | FrameKind::DefinitionTitle(children)
                | FrameKind::TableCell(children)
                | FrameKind::InlineContainer { children, .. }
                | FrameKind::Link { children, .. }
                | FrameKind::Image { children, .. } => {
                    children.push(node);
                    return;
                }
                FrameKind::Code { .. }
                | FrameKind::HtmlBlock(_)
                | FrameKind::Metadata(_)
                | FrameKind::InlineHtml { .. } => return,
                _ => {}
            }
        }
        let text = match &node.kind {
            Inline::Text(text) | Inline::Literal(text) => text.clone(),
            _ => plain_text(std::slice::from_ref(&node)),
        };
        let range = node.range;
        let block = self.block(Block::Paragraph(vec![node]), range);
        if !text.is_empty() {
            self.push_block(block);
        }
    }

    fn push_block(&mut self, block: BlockNode) {
        for frame in self.stack.iter_mut().rev() {
            match &mut frame.kind {
                FrameKind::Quote { children, .. }
                | FrameKind::Item { children, .. }
                | FrameKind::Footnote { children, .. }
                | FrameKind::Definition(children) => {
                    children.push(block);
                    return;
                }
                FrameKind::InlineHtml { .. } => return,
                _ => {}
            }
        }
        self.blocks.push(block);
    }

    fn set_task_marker(&mut self, checked: bool) {
        if let Some(FrameKind::Item {
            checked: marker, ..
        }) = self
            .stack
            .iter_mut()
            .rev()
            .find_map(|frame| match &mut frame.kind {
                item @ FrameKind::Item { .. } => Some(item),
                _ => None,
            })
        {
            *marker = Some(checked);
        }
    }

    fn resource(
        &mut self,
        role: ResourceRole,
        source: &str,
        label: Option<&str>,
        range: SourceRange,
    ) -> ResolvedResource {
        let resource = self.policy.resolve(role, source, label);
        if self.resources.len() < self.limits.max_resources {
            self.resources.push(resource.clone());
        } else {
            self.truncated = true;
            self.diagnostic(
                "markdown_resource_limit",
                Some(range),
                "Markdown resources exceeded the document limit",
            );
        }
        resource
    }

    fn bounded_code(&mut self, source: &str, range: SourceRange) -> String {
        if source.len() <= self.limits.max_code_bytes {
            return source.to_string();
        }
        self.truncated = true;
        self.diagnostic(
            "markdown_code_limit",
            Some(range),
            "Code or generated source exceeded the byte limit",
        );
        utf8_prefix(source, self.limits.max_code_bytes).to_string()
    }

    fn unique_slug(&mut self, base: String) -> String {
        let count = self.slug_counts.entry(base.clone()).or_default();
        let slug = if *count == 0 {
            base
        } else {
            format!("{base}-{count}")
        };
        *count = count.saturating_add(1);
        slug
    }

    fn id(&mut self, kind: &str, range: SourceRange, content: &str) -> NodeId {
        self.node_count = self.node_count.saturating_add(1);
        self.occurrence = self.occurrence.saturating_add(1);
        stable_node_id(kind, range, content, self.occurrence)
    }

    fn inline(&mut self, kind: Inline, range: SourceRange, content: &str) -> InlineNode {
        InlineNode {
            id: self.id("inline", range, content),
            range,
            kind,
        }
    }

    fn block(&mut self, kind: Block, range: SourceRange) -> BlockNode {
        let content = self.source_slice(range).to_string();
        BlockNode {
            id: self.id(kind.kind_name(), range, &content),
            range,
            kind,
        }
    }

    fn diagnostic(
        &mut self,
        code: &'static str,
        range: Option<SourceRange>,
        message: impl Into<String>,
    ) {
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics.push(MarkdownDiagnostic {
                code,
                severity: DiagnosticSeverity::Warning,
                range,
                message: bounded_text(&message.into(), 240),
            });
        }
    }

    fn source_slice(&self, range: SourceRange) -> &str {
        self.source.get(range.start..range.end).unwrap_or_default()
    }

    fn literal_document(&self, code: &'static str, message: impl Into<String>) -> MarkdownDocument {
        let input = MarkdownInput {
            source: self.source.clone(),
            base_path: self.input.base_path.clone(),
            revision: self.input.revision,
            surface: self.input.surface,
        };
        MarkdownDocument::literal(&input, code, message)
    }
}

fn heading_level(level: HeadingLevel) -> u8 {
    match level {
        HeadingLevel::H1 => 1,
        HeadingLevel::H2 => 2,
        HeadingLevel::H3 => 3,
        HeadingLevel::H4 => 4,
        HeadingLevel::H5 => 5,
        HeadingLevel::H6 => 6,
    }
}

fn callout_kind(kind: BlockQuoteKind) -> CalloutKind {
    match kind {
        BlockQuoteKind::Note => CalloutKind::Note,
        BlockQuoteKind::Tip => CalloutKind::Tip,
        BlockQuoteKind::Important => CalloutKind::Important,
        BlockQuoteKind::Warning => CalloutKind::Warning,
        BlockQuoteKind::Caution => CalloutKind::Caution,
    }
}

fn table_alignment(alignment: Alignment) -> TableAlignment {
    match alignment {
        Alignment::None => TableAlignment::None,
        Alignment::Left => TableAlignment::Left,
        Alignment::Center => TableAlignment::Center,
        Alignment::Right => TableAlignment::Right,
    }
}

pub fn normalize_language(language: &str) -> String {
    match language.trim().to_ascii_lowercase().as_str() {
        "sh" | "shell" | "zsh" => "bash".into(),
        "c++" | "cc" | "cxx" => "cpp".into(),
        "cs" | "c#" => "csharp".into(),
        "js" | "jsx" | "node" => "javascript".into(),
        "ts" => "typescript".into(),
        "py" => "python".into(),
        "rb" => "ruby".into(),
        "rs" => "rust".into(),
        "yml" => "yaml".into(),
        "md" => "markdown".into(),
        "puml" => "puml".into(),
        value => value.to_string(),
    }
}

fn valid_heading_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 128
        && id
            .chars()
            .all(|character| character.is_alphanumeric() || matches!(character, '-' | '_' | '.'))
}

pub fn slugify(value: &str) -> String {
    let mut slug = String::new();
    let mut pending_dash = false;
    for character in value.trim().chars().flat_map(char::to_lowercase) {
        if character.is_alphanumeric() || character == '_' {
            if pending_dash && !slug.is_empty() {
                slug.push('-');
            }
            pending_dash = false;
            slug.push(character);
        } else if character.is_whitespace() || character == '-' {
            pending_dash = true;
        }
    }
    if slug.is_empty() {
        "section".into()
    } else {
        slug
    }
}

fn opening_inline_tag(token: &str) -> Option<String> {
    let token = token.strip_prefix('<')?;
    if token.starts_with('/') || token.starts_with('!') || token.starts_with('?') {
        return None;
    }
    let tag = token
        .split(|character: char| character.is_whitespace() || matches!(character, '>' | '/'))
        .next()?
        .to_ascii_lowercase();
    matches!(
        tag.as_str(),
        "a" | "abbr"
            | "b"
            | "br"
            | "code"
            | "del"
            | "em"
            | "i"
            | "img"
            | "kbd"
            | "mark"
            | "q"
            | "s"
            | "small"
            | "span"
            | "strong"
            | "sub"
            | "sup"
            | "u"
    )
    .then_some(tag)
}

fn closing_inline_tag(token: &str) -> Option<String> {
    let token = token.strip_prefix("</")?;
    token
        .split(|character: char| character.is_whitespace() || character == '>')
        .next()
        .filter(|tag| !tag.is_empty())
        .map(str::to_ascii_lowercase)
}

fn is_void_inline_tag(token: &str, tag: &str) -> bool {
    matches!(tag, "br" | "img") || token.trim_end().ends_with("/>")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceKind;

    fn input(source: &str, revision: u64) -> MarkdownInput {
        MarkdownInput::new(source, "docs", revision)
    }

    #[test]
    fn parses_advanced_matrix_into_one_document_contract() {
        let document = parse_markdown(input(
            r#"# Intro

# Intro

[TOC]

> [!NOTE]
> Native callout

- [x] parsed
- [ ] rendered

Term
: Definition

Inline $a+b$ and <kbd>Ctrl</kbd> plus <mark>safe</mark>.[^n]

[^n]: Footnote

```diff
+added
-removed
```

```mermaid
flowchart LR
A-->B
```

```puml
@startuml
participant A
A -> B: hi
@enduml
```

<details open><summary>More</summary><progress value="2" max="4">half</progress></details>
"#,
            7,
        ));

        assert_eq!(document.revision, 7);
        assert_eq!(document.outline.len(), 2);
        assert_eq!(document.outline[0].slug, "intro");
        assert_eq!(document.outline[1].slug, "intro-1");
        assert!(document.footnotes.definitions.contains_key("n"));
        assert!(document.footnotes.references.contains_key("n"));
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::TableOfContents))
        );
        assert!(document.blocks.iter().any(|block| matches!(
            block.kind,
            Block::Callout {
                kind: CalloutKind::Note,
                ..
            }
        )));
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Diff { .. }))
        );
        assert_eq!(
            document
                .blocks
                .iter()
                .filter(|block| matches!(block.kind, Block::Diagram { .. }))
                .count(),
            2
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Details { .. }))
        );
    }

    #[test]
    fn agent_math_respects_markdown_code_fences() {
        let raw = parse_markdown(
            input("Inline $E = mc^2$.\n\n$$\n\\frac{a}{b}\n$$", 1)
                .surface(crate::model::MarkdownSurface::Agent),
        );
        assert!(raw.blocks.iter().any(|block| {
            matches!(&block.kind, Block::Paragraph(inlines) if inlines.iter().any(|inline| matches!(inline.kind, Inline::Math(_))))
        }));
        assert!(
            raw.blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Math { .. }))
        );

        let fenced = parse_markdown(
            input(
                "```markdown\nInline $E = mc^2$.\n\n$$\n\\frac{a}{b}\n$$\n```",
                2,
            )
            .surface(crate::model::MarkdownSurface::Agent),
        );
        assert_eq!(fenced.blocks.len(), 1);
        assert!(matches!(
            &fenced.blocks[0].kind,
            Block::Code {
                language: Some(language),
                ..
            } if language == "markdown"
        ));
    }

    #[test]
    fn basic_typography_preserves_heading_levels_and_inline_semantics() {
        fn contains_inline(
            inlines: &[InlineNode],
            predicate: impl Fn(&Inline) -> bool + Copy,
        ) -> bool {
            inlines.iter().any(|inline| {
                predicate(&inline.kind)
                    || inline
                        .kind
                        .children()
                        .is_some_and(|children| contains_inline(children, predicate))
            })
        }

        let document = parse_markdown(input(
            "# 一级标题\n## 二级标题\n### 三级标题\n#### 四级标题\n##### 五级标题\n###### 六级标题\n\n普通文本，**这是粗体**，*这是斜体*，***这是粗斜体***，~~这是删除线~~，<u>这是下划线</u>，`const answer = 42;`。",
            3,
        ));
        let levels = document
            .blocks
            .iter()
            .filter_map(|block| match block.kind {
                Block::Heading { level, .. } => Some(level),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(levels, [1, 2, 3, 4, 5, 6]);

        let inlines = document
            .blocks
            .iter()
            .find_map(|block| match &block.kind {
                Block::Paragraph(inlines) => Some(inlines.as_slice()),
                _ => None,
            })
            .expect("typography paragraph");
        assert!(contains_inline(inlines, |inline| matches!(
            inline,
            Inline::Strong(_)
        )));
        assert!(contains_inline(inlines, |inline| matches!(
            inline,
            Inline::Emphasis(_)
        )));
        assert!(contains_inline(inlines, |inline| matches!(
            inline,
            Inline::Deletion(_)
        )));
        assert!(contains_inline(inlines, |inline| matches!(
            inline,
            Inline::Underline(_)
        )));
        assert!(contains_inline(inlines, |inline| matches!(
            inline,
            Inline::Code(_)
        )));
    }

    #[test]
    fn append_only_content_keeps_existing_node_ids() {
        let first = parse_markdown(input("# A\n\nBody", 1));
        let appended = parse_markdown(input("# A\n\nBody\n\nNext", 2));
        assert_eq!(first.blocks[0].id, appended.blocks[0].id);
        assert_eq!(first.blocks[1].id, appended.blocks[1].id);
    }

    #[test]
    fn resources_and_html_share_the_same_policy() {
        let document = parse_markdown(MarkdownInput::new(
            "[local](../src/lib.rs) ![bad](file:///etc/passwd) <a href=\"javascript:bad()\">x</a>",
            "docs/guide",
            1,
        ));
        assert!(
            document
                .resources
                .iter()
                .any(|resource| resource.kind == ResourceKind::Workspace)
        );
        assert_eq!(
            document
                .resources
                .iter()
                .filter(|resource| resource.kind == ResourceKind::Blocked)
                .count(),
            2
        );
    }

    #[test]
    fn source_and_depth_limits_fall_back_without_panicking() {
        let limits = MarkdownLimits {
            max_source_bytes: 8,
            max_depth: 2,
            ..MarkdownLimits::default()
        };
        let oversized = parse_markdown_with_limits(input("1234567890", 1), limits);
        assert!(oversized.truncated);

        let deep = parse_markdown_with_limits(
            input("> > > > nested", 2),
            MarkdownLimits {
                max_depth: 2,
                ..MarkdownLimits::default()
            },
        );
        assert!(
            deep.diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "markdown_depth_limit")
        );

        let combined_source = format!("> > > > {}", "x".repeat(64));
        let combined = parse_markdown_with_limits(
            input(&combined_source, 3),
            MarkdownLimits {
                max_source_bytes: 16,
                max_depth: 2,
                ..MarkdownLimits::default()
            },
        );
        assert!(combined.source.len() <= 16);
    }

    #[test]
    fn checked_in_fixture_covers_the_native_feature_matrix() {
        let document = parse_markdown(input(include_str!("../fixtures/advanced.md"), 11));
        let callouts = document
            .blocks
            .iter()
            .filter_map(|block| {
                if let Block::Callout { kind, .. } = block.kind {
                    Some(kind)
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();

        for kind in [
            CalloutKind::Note,
            CalloutKind::Tip,
            CalloutKind::Important,
            CalloutKind::Warning,
            CalloutKind::Caution,
        ] {
            assert!(callouts.contains(&kind), "missing {kind:?}");
        }
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Math { .. }))
        );
        assert_eq!(
            document
                .blocks
                .iter()
                .filter(|block| matches!(block.kind, Block::Diagram { .. }))
                .count(),
            2
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Diff { .. }))
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Table { .. }))
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Details { .. }))
        );
        assert!(
            document
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::DefinitionList(_)))
        );
        assert!(document.footnotes.definitions.contains_key("source"));
        let duplicate_slugs = document
            .outline
            .iter()
            .filter(|entry| entry.slug.starts_with("duplicate-heading"))
            .map(|entry| entry.slug.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            duplicate_slugs,
            ["duplicate-heading", "duplicate-heading-1"]
        );
    }

    #[test]
    fn malformed_fixture_keeps_source_and_reports_blocked_html() {
        let document = parse_markdown(input(include_str!("../fixtures/malformed.md"), 12));

        assert!(!document.blocks.is_empty());
        assert!(
            document
                .plain_text()
                .contains("not a supported mermaid diagram")
        );
        assert!(
            document
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "markdown_html_tag_rejected")
        );
        assert!(
            document
                .resources
                .iter()
                .any(|resource| resource.kind == ResourceKind::Blocked)
        );
    }

    #[test]
    fn unknown_callout_kind_remains_a_readable_blockquote() {
        let document = parse_markdown(input("> [!CUSTOM]\n> Keep this text", 13));

        assert!(matches!(document.blocks[0].kind, Block::Quote(_)));
        assert!(document.plain_text().contains("Keep this text"));
    }

    #[test]
    fn html_headings_share_the_outline_and_duplicate_slug_index() {
        let document = parse_markdown(input(
            "# Shared\n\n<h2>Shared</h2>\n\n<h3>Nested <em>heading</em></h3>",
            14,
        ));

        assert_eq!(
            document
                .outline
                .iter()
                .map(|entry| entry.slug.as_str())
                .collect::<Vec<_>>(),
            ["shared", "shared-1", "nested-heading"]
        );
    }

    #[test]
    fn nested_void_inline_html_does_not_consume_the_closing_container() {
        let document = parse_markdown(input("Press <kbd>A<br>B</kbd> now.", 15));

        assert_eq!(document.plain_text(), "Press A\nB now.");
        assert!(document.diagnostics.is_empty());
    }

    #[test]
    fn html_fragments_share_the_document_node_budget() {
        let source = "<p><strong>a</strong></p>\n\n<p><strong>b</strong></p>";
        let document = parse_markdown_with_limits(
            input(source, 16),
            MarkdownLimits {
                max_nodes: 4,
                ..MarkdownLimits::default()
            },
        );

        assert!(document.truncated);
        assert!(document.diagnostics.iter().any(|diagnostic| matches!(
            diagnostic.code,
            "markdown_node_limit" | "markdown_html_limit"
        )));
    }
}

use std::collections::BTreeMap;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::resource::ResolvedResource;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(transparent)]
pub struct NodeId(pub u64);

pub(crate) fn stable_node_id(
    kind: &str,
    range: SourceRange,
    content: &str,
    occurrence: u32,
) -> NodeId {
    let mut digest = Sha256::new();
    digest.update(kind.as_bytes());
    digest.update([0]);
    digest.update(range.start.to_le_bytes());
    digest.update([0]);
    digest.update(content.trim().as_bytes());
    digest.update([0]);
    digest.update(occurrence.to_le_bytes());
    let bytes = digest.finalize();
    NodeId(u64::from_le_bytes(
        bytes[..8].try_into().unwrap_or_default(),
    ))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SourceRange {
    pub start: usize,
    pub end: usize,
}

impl SourceRange {
    pub fn new(start: usize, end: usize) -> Self {
        Self {
            start: start.min(end),
            end: end.max(start),
        }
    }

    pub fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    pub fn is_empty(self) -> bool {
        self.start == self.end
    }
}

impl From<std::ops::Range<usize>> for SourceRange {
    fn from(value: std::ops::Range<usize>) -> Self {
        Self::new(value.start, value.end)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkdownSurface {
    Agent,
    #[default]
    FilePreview,
    Fixture,
}

#[derive(Debug, Clone)]
pub struct MarkdownInput {
    pub source: Arc<str>,
    pub base_path: Arc<str>,
    pub revision: u64,
    pub surface: MarkdownSurface,
}

impl MarkdownInput {
    pub fn new(source: impl Into<Arc<str>>, base_path: impl Into<Arc<str>>, revision: u64) -> Self {
        Self {
            source: source.into(),
            base_path: base_path.into(),
            revision,
            surface: MarkdownSurface::default(),
        }
    }

    pub fn surface(mut self, surface: MarkdownSurface) -> Self {
        self.surface = surface;
        self
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagnosticSeverity {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownDiagnostic {
    pub code: &'static str,
    pub severity: DiagnosticSeverity,
    pub range: Option<SourceRange>,
    pub message: String,
}

impl MarkdownDiagnostic {
    pub fn warning(
        code: &'static str,
        range: Option<SourceRange>,
        message: impl Into<String>,
    ) -> Self {
        Self {
            code,
            severity: DiagnosticSeverity::Warning,
            range,
            message: message.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutlineEntry {
    pub node_id: NodeId,
    pub level: u8,
    pub slug: String,
    pub title: String,
    pub range: SourceRange,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FootnoteIndex {
    pub definitions: BTreeMap<String, NodeId>,
    pub references: BTreeMap<String, Vec<NodeId>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct MarkdownDocument {
    pub source: Arc<str>,
    pub base_path: Arc<str>,
    pub revision: u64,
    pub blocks: Arc<[BlockNode]>,
    pub outline: Arc<[OutlineEntry]>,
    pub footnotes: FootnoteIndex,
    pub resources: Arc<[ResolvedResource]>,
    pub diagnostics: Arc<[MarkdownDiagnostic]>,
    pub truncated: bool,
}

impl MarkdownDocument {
    pub fn literal(input: &MarkdownInput, code: &'static str, message: impl Into<String>) -> Self {
        let source = input.source.clone();
        let range = SourceRange::new(0, source.len());
        Self {
            source: source.clone(),
            base_path: input.base_path.clone(),
            revision: input.revision,
            blocks: vec![BlockNode {
                id: NodeId(0),
                range,
                kind: Block::Literal(source.to_string()),
            }]
            .into(),
            outline: Arc::default(),
            footnotes: FootnoteIndex::default(),
            resources: Arc::default(),
            diagnostics: vec![MarkdownDiagnostic::warning(code, Some(range), message)].into(),
            truncated: true,
        }
    }

    pub fn source_for(&self, range: SourceRange) -> &str {
        self.source.get(range.start..range.end).unwrap_or_default()
    }

    pub fn block(&self, id: NodeId) -> Option<&BlockNode> {
        fn find(blocks: &[BlockNode], id: NodeId) -> Option<&BlockNode> {
            for block in blocks {
                if block.id == id {
                    return Some(block);
                }
                if let Some(children) = block.kind.children()
                    && let Some(found) = find(children, id)
                {
                    return Some(found);
                }
            }
            None
        }
        find(&self.blocks, id)
    }

    pub fn plain_text(&self) -> String {
        fn append_blocks(blocks: &[BlockNode], output: &mut String) {
            for block in blocks {
                match &block.kind {
                    Block::Paragraph(inlines)
                    | Block::Heading {
                        content: inlines, ..
                    } => {
                        output.push_str(&plain_text(inlines));
                    }
                    Block::Quote(children)
                    | Block::Callout { children, .. }
                    | Block::SafeHtml(children) => append_blocks(children, output),
                    Block::Code { source, .. }
                    | Block::Diff { source }
                    | Block::Math { source }
                    | Block::Diagram { source, .. }
                    | Block::Literal(source) => output.push_str(source),
                    Block::List { items, .. } => {
                        for item in items {
                            append_blocks(&item.children, output);
                        }
                    }
                    Block::DefinitionList(items) => {
                        for item in items {
                            output.push_str(&plain_text(&item.term));
                            output.push('\n');
                            for definition in &item.definitions {
                                append_blocks(definition, output);
                            }
                        }
                    }
                    Block::Table { header, rows, .. } => {
                        for row in header.iter().chain(rows) {
                            for (index, cell) in row.cells.iter().enumerate() {
                                if index > 0 {
                                    output.push('\t');
                                }
                                output.push_str(&plain_text(cell));
                            }
                            output.push('\n');
                        }
                    }
                    Block::ThematicBreak | Block::TableOfContents => {}
                    Block::Details {
                        summary, children, ..
                    } => {
                        output.push_str(&plain_text(summary));
                        output.push('\n');
                        append_blocks(children, output);
                    }
                    Block::Progress { label, .. } => {
                        if let Some(label) = label {
                            output.push_str(label);
                        }
                    }
                    Block::Image(image) => output.push_str(&image.alt),
                    Block::FootnoteDefinition { label, children } => {
                        output.push_str("[^");
                        output.push_str(label);
                        output.push_str("]: ");
                        append_blocks(children, output);
                    }
                }
                if !output.ends_with('\n') {
                    output.push('\n');
                }
            }
        }

        let mut output = String::new();
        append_blocks(&self.blocks, &mut output);
        output.trim_end().to_string()
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct BlockNode {
    pub id: NodeId,
    pub range: SourceRange,
    pub kind: Block,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CalloutKind {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

impl CalloutKind {
    pub fn title(self) -> &'static str {
        match self {
            Self::Note => "Note",
            Self::Tip => "Tip",
            Self::Important => "Important",
            Self::Warning => "Warning",
            Self::Caution => "Caution",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DiagramKind {
    Mermaid,
    PlantUml,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TableAlignment {
    None,
    Left,
    Center,
    Right,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Block {
    Paragraph(Vec<InlineNode>),
    Heading {
        level: u8,
        slug: String,
        content: Vec<InlineNode>,
    },
    Quote(Vec<BlockNode>),
    Callout {
        kind: CalloutKind,
        title: String,
        children: Vec<BlockNode>,
    },
    Code {
        language: Option<String>,
        source: String,
        fenced: bool,
    },
    Diff {
        source: String,
    },
    Math {
        source: String,
    },
    Diagram {
        kind: DiagramKind,
        source: String,
    },
    List {
        start: Option<u64>,
        items: Vec<ListItem>,
    },
    DefinitionList(Vec<DefinitionItem>),
    Table {
        alignments: Vec<TableAlignment>,
        header: Option<TableRow>,
        rows: Vec<TableRow>,
    },
    ThematicBreak,
    TableOfContents,
    Details {
        summary: Vec<InlineNode>,
        children: Vec<BlockNode>,
        initially_open: bool,
    },
    Progress {
        value: f64,
        max: f64,
        label: Option<String>,
    },
    Image(InlineImage),
    FootnoteDefinition {
        label: String,
        children: Vec<BlockNode>,
    },
    SafeHtml(Vec<BlockNode>),
    Literal(String),
}

impl Block {
    pub fn children(&self) -> Option<&[BlockNode]> {
        match self {
            Self::Quote(children)
            | Self::Callout { children, .. }
            | Self::Details { children, .. }
            | Self::FootnoteDefinition { children, .. }
            | Self::SafeHtml(children) => Some(children),
            _ => None,
        }
    }

    pub fn kind_name(&self) -> &'static str {
        match self {
            Self::Paragraph(_) => "paragraph",
            Self::Heading { .. } => "heading",
            Self::Quote(_) => "quote",
            Self::Callout { .. } => "callout",
            Self::Code { .. } => "code",
            Self::Diff { .. } => "diff",
            Self::Math { .. } => "math",
            Self::Diagram { .. } => "diagram",
            Self::List { .. } => "list",
            Self::DefinitionList(_) => "definition_list",
            Self::Table { .. } => "table",
            Self::ThematicBreak => "rule",
            Self::TableOfContents => "toc",
            Self::Details { .. } => "details",
            Self::Progress { .. } => "progress",
            Self::Image(_) => "image",
            Self::FootnoteDefinition { .. } => "footnote_definition",
            Self::SafeHtml(_) => "html",
            Self::Literal(_) => "literal",
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct ListItem {
    pub id: NodeId,
    pub range: SourceRange,
    pub checked: Option<bool>,
    pub children: Vec<BlockNode>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct DefinitionItem {
    pub term: Vec<InlineNode>,
    pub definitions: Vec<Vec<BlockNode>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct TableRow {
    pub cells: Vec<Vec<InlineNode>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct InlineNode {
    pub id: NodeId,
    pub range: SourceRange,
    pub kind: Inline,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Inline {
    Text(String),
    Emphasis(Vec<InlineNode>),
    Strong(Vec<InlineNode>),
    Deletion(Vec<InlineNode>),
    Underline(Vec<InlineNode>),
    Superscript(Vec<InlineNode>),
    Subscript(Vec<InlineNode>),
    Code(String),
    Link {
        destination: ResolvedResource,
        title: Option<String>,
        children: Vec<InlineNode>,
    },
    Image(InlineImage),
    Math(String),
    Keycap(Vec<InlineNode>),
    Mark(Vec<InlineNode>),
    Break,
    FootnoteReference(String),
    Literal(String),
}

impl Inline {
    pub fn children(&self) -> Option<&[InlineNode]> {
        match self {
            Self::Emphasis(children)
            | Self::Strong(children)
            | Self::Deletion(children)
            | Self::Underline(children)
            | Self::Superscript(children)
            | Self::Subscript(children)
            | Self::Keycap(children)
            | Self::Mark(children)
            | Self::Link { children, .. } => Some(children),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct InlineImage {
    pub destination: ResolvedResource,
    pub alt: String,
    pub title: Option<String>,
}

pub fn plain_text(inlines: &[InlineNode]) -> String {
    fn append(inlines: &[InlineNode], output: &mut String) {
        for inline in inlines {
            match &inline.kind {
                Inline::Text(text)
                | Inline::Code(text)
                | Inline::Math(text)
                | Inline::Literal(text) => output.push_str(text),
                Inline::Image(image) => output.push_str(&image.alt),
                Inline::Break => output.push('\n'),
                Inline::FootnoteReference(label) => {
                    output.push_str("[^");
                    output.push_str(label);
                    output.push(']');
                }
                other => {
                    if let Some(children) = other.children() {
                        append(children, output);
                    }
                }
            }
        }
    }
    let mut output = String::new();
    append(inlines, &mut output);
    output
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::parser::parse_markdown;

    #[test]
    fn document_plain_text_preserves_generated_sources_in_reading_order() {
        let document = parse_markdown(MarkdownInput::new(
            "Read [the guide](guide.md) with $a+b$.\n\n```mermaid\nflowchart LR\nA-->B\n```",
            "docs",
            1,
        ));

        assert_eq!(
            document.plain_text(),
            "Read the guide with a+b.\nflowchart LR\nA-->B"
        );
    }
}

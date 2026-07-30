use std::cell::RefCell;
use std::rc::Rc;

use html5ever::tendril::TendrilSink as _;
use html5ever::{ParseOpts, parse_document};
use markup5ever_rcdom::{Node, NodeData, RcDom};

use crate::limits::{MarkdownLimits, bounded_text, utf8_prefix};
use crate::model::{
    Block, BlockNode, DefinitionItem, DiagnosticSeverity, Inline, InlineImage, InlineNode,
    ListItem, MarkdownDiagnostic, NodeId, SourceRange, TableAlignment, TableRow, stable_node_id,
};
use crate::resource::{ResolvedResource, ResourcePolicy, ResourceRole};

pub(crate) struct HtmlParseResult {
    pub blocks: Vec<BlockNode>,
    pub resources: Vec<ResolvedResource>,
    pub diagnostics: Vec<MarkdownDiagnostic>,
    pub node_count: usize,
}

pub(crate) fn parse_html_fragment(
    source: &str,
    range: SourceRange,
    policy: &ResourcePolicy,
    limits: MarkdownLimits,
) -> HtmlParseResult {
    let source = utf8_prefix(source, limits.max_code_bytes);
    let dom = parse_document(RcDom::default(), ParseOpts::default()).one(source);
    let mut converter = HtmlConverter {
        range,
        policy,
        limits,
        resources: Vec::new(),
        diagnostics: Vec::new(),
        node_count: 0,
        occurrence: 0,
    };
    let blocks = converter.convert_block_children(&dom.document, 0);
    HtmlParseResult {
        blocks,
        resources: converter.resources,
        diagnostics: converter.diagnostics,
        node_count: converter.node_count,
    }
}

struct HtmlConverter<'a> {
    range: SourceRange,
    policy: &'a ResourcePolicy,
    limits: MarkdownLimits,
    resources: Vec<ResolvedResource>,
    diagnostics: Vec<MarkdownDiagnostic>,
    node_count: usize,
    occurrence: u32,
}

impl HtmlConverter<'_> {
    fn id(&mut self, kind: &str, content: &str) -> NodeId {
        self.node_count = self.node_count.saturating_add(1);
        self.occurrence = self.occurrence.saturating_add(1);
        stable_node_id(kind, self.range, content, self.occurrence)
    }

    fn block(&mut self, kind: Block, content: &str) -> BlockNode {
        BlockNode {
            id: self.id(kind.kind_name(), content),
            range: self.range,
            kind,
        }
    }

    fn inline(&mut self, kind: Inline, content: &str) -> InlineNode {
        InlineNode {
            id: self.id("html_inline", content),
            range: self.range,
            kind,
        }
    }

    fn diagnostic(&mut self, code: &'static str, message: impl Into<String>) {
        if self.diagnostics.len() < self.limits.max_diagnostics {
            self.diagnostics.push(MarkdownDiagnostic {
                code,
                severity: DiagnosticSeverity::Warning,
                range: Some(self.range),
                message: bounded_text(&message.into(), 240),
            });
        }
    }

    fn resource(
        &mut self,
        role: ResourceRole,
        source: &str,
        label: Option<&str>,
    ) -> ResolvedResource {
        let resource = self.policy.resolve(role, source, label);
        if self.resources.len() < self.limits.max_resources {
            self.resources.push(resource.clone());
        } else {
            self.diagnostic(
                "markdown_resource_limit",
                "HTML resources exceeded the document limit",
            );
        }
        resource
    }

    fn convert_block_children(&mut self, node: &Rc<Node>, depth: usize) -> Vec<BlockNode> {
        if depth >= self.limits.max_depth || self.node_count >= self.limits.max_nodes {
            self.diagnostic(
                "markdown_html_limit",
                "HTML nesting or node count exceeded the document limit",
            );
            return Vec::new();
        }
        let mut blocks = Vec::new();
        let mut pending_inline = Vec::new();
        for child in node.children.borrow().iter() {
            if is_block_node(child) {
                if !pending_inline.is_empty() {
                    let text = inline_text(&pending_inline);
                    let paragraph = self.block(Block::Paragraph(pending_inline), &text);
                    blocks.push(paragraph);
                    pending_inline = Vec::new();
                }
                blocks.extend(self.convert_block(child, depth + 1));
            } else {
                pending_inline.extend(self.convert_inline(child, depth + 1));
            }
        }
        if !pending_inline.is_empty() {
            let text = inline_text(&pending_inline);
            if !text.trim().is_empty() {
                let paragraph = self.block(Block::Paragraph(pending_inline), &text);
                blocks.push(paragraph);
            }
        }
        blocks
    }

    fn convert_block(&mut self, node: &Rc<Node>, depth: usize) -> Vec<BlockNode> {
        if depth >= self.limits.max_depth || self.node_count >= self.limits.max_nodes {
            self.diagnostic(
                "markdown_html_limit",
                "HTML nesting or node count exceeded the document limit",
            );
            return Vec::new();
        }
        let NodeData::Element {
            ref name,
            ref attrs,
            ..
        } = node.data
        else {
            return self.convert_block_children(node, depth + 1);
        };
        let tag = name.local.as_ref().to_ascii_lowercase();
        self.validate_attributes(&tag, attrs);
        if is_forbidden_tag(&tag) {
            self.diagnostic(
                "markdown_html_tag_rejected",
                format!("HTML <{tag}> is not allowed"),
            );
            return Vec::new();
        }
        match tag.as_str() {
            "html" | "head" | "body" | "main" | "article" | "section" | "div" => {
                self.convert_block_children(node, depth + 1)
            }
            "p" => {
                let content = self.convert_inline_children(node, depth + 1);
                let text = inline_text(&content);
                vec![self.block(Block::Paragraph(content), &text)]
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                let content = self.convert_inline_children(node, depth + 1);
                let title = inline_text(&content);
                let level = tag[1..].parse::<u8>().unwrap_or(6);
                let slug = slugify(&title);
                vec![self.block(
                    Block::Heading {
                        level,
                        slug,
                        content,
                    },
                    &title,
                )]
            }
            "blockquote" => {
                let children = self.convert_block_children(node, depth + 1);
                vec![self.block(Block::Quote(children), "blockquote")]
            }
            "pre" => {
                let source = text_content(node);
                let language = node
                    .children
                    .borrow()
                    .iter()
                    .find_map(|child| element(child, "code"))
                    .and_then(|code| attr(&code, "class"))
                    .and_then(|class| {
                        class
                            .split_whitespace()
                            .find_map(|value| value.strip_prefix("language-"))
                            .map(str::to_string)
                    });
                vec![self.block(
                    Block::Code {
                        language,
                        source: source.clone(),
                        fenced: false,
                    },
                    &source,
                )]
            }
            "ul" | "ol" => {
                let start = (tag == "ol")
                    .then(|| attr_from_cell(attrs, "start"))
                    .flatten()
                    .and_then(|value| value.parse().ok())
                    .or((tag == "ol").then_some(1));
                let mut items = Vec::new();
                for child in node.children.borrow().iter() {
                    if let Some(item) = element(child, "li") {
                        let children = self.convert_block_children(&item, depth + 1);
                        let text = text_content(&item);
                        items.push(ListItem {
                            id: self.id("html_list_item", &text),
                            range: self.range,
                            checked: None,
                            children,
                        });
                    }
                }
                vec![self.block(Block::List { start, items }, &tag)]
            }
            "dl" => vec![self.convert_definition_list(node, depth + 1)],
            "table" => vec![self.convert_table(node, depth + 1)],
            "details" => vec![self.convert_details(node, attrs, depth + 1)],
            "progress" => vec![self.convert_progress(node, attrs)],
            "img" => self.convert_image(attrs).map_or_else(Vec::new, |image| {
                let alt = image.alt.clone();
                vec![self.block(Block::Image(image), &alt)]
            }),
            "hr" => vec![self.block(Block::ThematicBreak, "hr")],
            _ if is_inline_tag(&tag) => {
                let content = self.convert_inline(node, depth + 1);
                let text = inline_text(&content);
                vec![self.block(Block::Paragraph(content), &text)]
            }
            _ => {
                self.diagnostic(
                    "markdown_html_tag_unsupported",
                    format!("HTML <{tag}> is not supported; its safe text was retained"),
                );
                self.convert_block_children(node, depth + 1)
            }
        }
    }

    fn convert_inline_children(&mut self, node: &Rc<Node>, depth: usize) -> Vec<InlineNode> {
        let mut output = Vec::new();
        for child in node.children.borrow().iter() {
            output.extend(self.convert_inline(child, depth + 1));
        }
        output
    }

    fn convert_inline(&mut self, node: &Rc<Node>, depth: usize) -> Vec<InlineNode> {
        if depth >= self.limits.max_depth || self.node_count >= self.limits.max_nodes {
            self.diagnostic(
                "markdown_html_limit",
                "HTML nesting or node count exceeded the document limit",
            );
            return Vec::new();
        }
        match node.data {
            NodeData::Text { ref contents } => {
                let text = contents.borrow().to_string();
                if text.is_empty() {
                    Vec::new()
                } else {
                    vec![self.inline(Inline::Text(text.clone()), &text)]
                }
            }
            NodeData::Element {
                ref name,
                ref attrs,
                ..
            } => {
                let tag = name.local.as_ref().to_ascii_lowercase();
                self.validate_attributes(&tag, attrs);
                if is_forbidden_tag(&tag) {
                    self.diagnostic(
                        "markdown_html_tag_rejected",
                        format!("HTML <{tag}> is not allowed"),
                    );
                    return Vec::new();
                }
                let content = self.convert_inline_children(node, depth + 1);
                let text = inline_text(&content);
                let kind = match tag.as_str() {
                    "em" | "i" => Inline::Emphasis(content),
                    "strong" | "b" => Inline::Strong(content),
                    "del" | "s" => Inline::Deletion(content),
                    "u" => Inline::Underline(content),
                    "code" => Inline::Code(text.clone()),
                    "kbd" => Inline::Keycap(content),
                    "mark" => Inline::Mark(content),
                    "sup" => Inline::Superscript(content),
                    "sub" => Inline::Subscript(content),
                    "a" => {
                        let href = attr_from_cell(attrs, "href").unwrap_or_default();
                        let title = attr_from_cell(attrs, "title");
                        let destination = self.resource(ResourceRole::Link, &href, Some(&text));
                        Inline::Link {
                            destination,
                            title,
                            children: content,
                        }
                    }
                    "img" => {
                        return self.convert_image(attrs).map_or_else(Vec::new, |image| {
                            let alt = image.alt.clone();
                            vec![self.inline(Inline::Image(image), &alt)]
                        });
                    }
                    "br" => Inline::Break,
                    "span" | "small" | "q" | "abbr" => {
                        return content;
                    }
                    _ => {
                        self.diagnostic(
                            "markdown_html_tag_unsupported",
                            format!("HTML <{tag}> is not supported; its safe text was retained"),
                        );
                        return content;
                    }
                };
                vec![self.inline(kind, &text)]
            }
            NodeData::Comment { .. } | NodeData::Doctype { .. } => Vec::new(),
            _ => self.convert_inline_children(node, depth + 1),
        }
    }

    fn convert_image(&mut self, attrs: &RefCell<Vec<html5ever::Attribute>>) -> Option<InlineImage> {
        let source = attr_from_cell(attrs, "src")?;
        let alt = attr_from_cell(attrs, "alt").unwrap_or_default();
        let title = attr_from_cell(attrs, "title");
        let destination = self.resource(ResourceRole::Image, &source, Some(&alt));
        Some(InlineImage {
            destination,
            alt,
            title,
        })
    }

    fn convert_definition_list(&mut self, node: &Rc<Node>, depth: usize) -> BlockNode {
        let mut items = Vec::<DefinitionItem>::new();
        for child in node.children.borrow().iter() {
            if element(child, "dt").is_some() {
                items.push(DefinitionItem {
                    term: self.convert_inline_children(child, depth + 1),
                    definitions: Vec::new(),
                });
            } else if element(child, "dd").is_some() {
                let definition = self.convert_block_children(child, depth + 1);
                if let Some(item) = items.last_mut() {
                    item.definitions.push(definition);
                }
            }
        }
        self.block(Block::DefinitionList(items), "dl")
    }

    fn convert_table(&mut self, node: &Rc<Node>, depth: usize) -> BlockNode {
        let mut table_rows = Vec::new();
        collect_elements(node, "tr", &mut table_rows);
        let mut header = None;
        let mut rows = Vec::new();
        let mut column_count = 0usize;
        for row in table_rows {
            let mut cells = Vec::new();
            let mut has_header = false;
            for cell in row.children.borrow().iter() {
                if element(cell, "th").is_some() || element(cell, "td").is_some() {
                    has_header |= element(cell, "th").is_some();
                    cells.push(self.convert_inline_children(cell, depth + 1));
                }
            }
            column_count = column_count.max(cells.len());
            if has_header && header.is_none() {
                header = Some(TableRow { cells });
            } else if !cells.is_empty() {
                rows.push(TableRow { cells });
            }
        }
        self.block(
            Block::Table {
                alignments: vec![TableAlignment::None; column_count],
                header,
                rows,
            },
            "table",
        )
    }

    fn convert_details(
        &mut self,
        node: &Rc<Node>,
        attrs: &RefCell<Vec<html5ever::Attribute>>,
        depth: usize,
    ) -> BlockNode {
        let mut summary = Vec::new();
        let mut children = Vec::new();
        for child in node.children.borrow().iter() {
            if element(child, "summary").is_some() && summary.is_empty() {
                summary = self.convert_inline_children(child, depth + 1);
            } else if is_block_node(child) {
                children.extend(self.convert_block(child, depth + 1));
            } else {
                let inline = self.convert_inline(child, depth + 1);
                if !inline_text(&inline).trim().is_empty() {
                    let text = inline_text(&inline);
                    children.push(self.block(Block::Paragraph(inline), &text));
                }
            }
        }
        if summary.is_empty() {
            summary.push(self.inline(Inline::Text("Details".into()), "Details"));
        }
        self.block(
            Block::Details {
                summary,
                children,
                initially_open: has_attr(attrs, "open"),
            },
            "details",
        )
    }

    fn convert_progress(
        &mut self,
        node: &Rc<Node>,
        attrs: &RefCell<Vec<html5ever::Attribute>>,
    ) -> BlockNode {
        let max = attr_from_cell(attrs, "max")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite() && *value > 0.0)
            .unwrap_or(1.0);
        let value = attr_from_cell(attrs, "value")
            .and_then(|value| value.parse::<f64>().ok())
            .filter(|value| value.is_finite())
            .unwrap_or(0.0)
            .clamp(0.0, max);
        let label = text_content(node);
        self.block(
            Block::Progress {
                value,
                max,
                label: (!label.trim().is_empty()).then(|| bounded_text(label.trim(), 240)),
            },
            "progress",
        )
    }

    fn validate_attributes(&mut self, tag: &str, attrs: &RefCell<Vec<html5ever::Attribute>>) {
        for attribute in attrs.borrow().iter() {
            let name = attribute.name.local.as_ref().to_ascii_lowercase();
            if name.starts_with("on") || name == "style" {
                self.diagnostic(
                    "markdown_html_attribute_rejected",
                    format!("HTML {name} is not allowed on <{tag}>"),
                );
                continue;
            }
            let allowed = match tag {
                "a" => matches!(name.as_str(), "href" | "title"),
                "img" => matches!(name.as_str(), "src" | "alt" | "title" | "width" | "height"),
                "details" => name == "open",
                "progress" => matches!(name.as_str(), "value" | "max"),
                "ol" => name == "start",
                "code" => name == "class",
                _ => matches!(name.as_str(), "title" | "aria-label"),
            };
            if !allowed {
                self.diagnostic(
                    "markdown_html_attribute_unsupported",
                    format!("HTML {name} is not supported on <{tag}>"),
                );
            }
        }
    }
}

fn attr_from_cell(attrs: &RefCell<Vec<html5ever::Attribute>>, name: &str) -> Option<String> {
    attrs.borrow().iter().find_map(|attribute| {
        attribute
            .name
            .local
            .as_ref()
            .eq_ignore_ascii_case(name)
            .then(|| attribute.value.to_string())
    })
}

fn attr(node: &Rc<Node>, name: &str) -> Option<String> {
    let NodeData::Element { ref attrs, .. } = node.data else {
        return None;
    };
    attr_from_cell(attrs, name)
}

fn has_attr(attrs: &RefCell<Vec<html5ever::Attribute>>, name: &str) -> bool {
    attrs
        .borrow()
        .iter()
        .any(|attribute| attribute.name.local.as_ref().eq_ignore_ascii_case(name))
}

fn element(node: &Rc<Node>, expected: &str) -> Option<Rc<Node>> {
    match node.data {
        NodeData::Element { ref name, .. }
            if name.local.as_ref().eq_ignore_ascii_case(expected) =>
        {
            Some(node.clone())
        }
        _ => None,
    }
}

fn collect_elements(node: &Rc<Node>, expected: &str, output: &mut Vec<Rc<Node>>) {
    for child in node.children.borrow().iter() {
        if let Some(child) = element(child, expected) {
            output.push(child);
        } else {
            collect_elements(child, expected, output);
        }
    }
}

fn text_content(node: &Rc<Node>) -> String {
    let mut output = String::new();
    fn visit(node: &Rc<Node>, output: &mut String) {
        if let NodeData::Text { ref contents } = node.data {
            output.push_str(&contents.borrow());
        }
        for child in node.children.borrow().iter() {
            visit(child, output);
        }
    }
    visit(node, &mut output);
    output
}

fn inline_text(nodes: &[InlineNode]) -> String {
    crate::model::plain_text(nodes)
}

fn is_block_node(node: &Rc<Node>) -> bool {
    let NodeData::Element { ref name, .. } = node.data else {
        return false;
    };
    matches!(
        name.local.as_ref().to_ascii_lowercase().as_str(),
        "html"
            | "head"
            | "body"
            | "main"
            | "article"
            | "section"
            | "div"
            | "p"
            | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "pre"
            | "ul"
            | "ol"
            | "dl"
            | "table"
            | "details"
            | "progress"
            | "hr"
            | "script"
            | "style"
            | "iframe"
            | "form"
            | "object"
            | "embed"
            | "svg"
            | "math"
    )
}

fn is_inline_tag(tag: &str) -> bool {
    matches!(
        tag,
        "em" | "i"
            | "strong"
            | "b"
            | "del"
            | "s"
            | "u"
            | "code"
            | "kbd"
            | "mark"
            | "sup"
            | "sub"
            | "a"
            | "img"
            | "br"
            | "span"
            | "small"
            | "q"
            | "abbr"
    )
}

fn is_forbidden_tag(tag: &str) -> bool {
    matches!(
        tag,
        "script"
            | "style"
            | "iframe"
            | "form"
            | "input"
            | "button"
            | "select"
            | "textarea"
            | "object"
            | "embed"
            | "video"
            | "audio"
            | "source"
            | "svg"
            | "math"
            | "link"
            | "meta"
    )
}

fn slugify(value: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::resource::ResourceKind;

    #[test]
    fn safe_subset_maps_semantics_and_rejects_active_content() {
        let policy = ResourcePolicy::new("docs");
        let result = parse_html_fragment(
            r#"<p><strong>Safe</strong> <kbd>Ctrl</kbd> <a href="../x.md">link</a></p>
               <details open onclick="bad()"><summary>More</summary><mark>Body</mark></details>
               <progress value="8" max="4">done</progress><script>alert(1)</script>"#,
            SourceRange::new(0, 256),
            &policy,
            MarkdownLimits::default(),
        );
        assert!(
            result
                .blocks
                .iter()
                .any(|block| matches!(block.kind, Block::Details { .. }))
        );
        assert!(
            result.blocks.iter().any(
                |block| matches!(block.kind, Block::Progress { value, max, .. } if value == max)
            )
        );
        assert_eq!(result.resources[0].kind, ResourceKind::Workspace);
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "markdown_html_tag_rejected")
        );
        assert!(
            result
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "markdown_html_attribute_rejected")
        );
    }
}

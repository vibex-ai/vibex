//! Markdown projections shared by the GPUI clients.
//!
//! Rendering belongs to gpui-component's text view; what both clients need on
//! top of it is the same reading of a Markdown source — a plain-text
//! projection for previews and labels, the resource destinations it names, and
//! the workspace-path resolution a click on one of them needs.

use std::collections::BTreeMap;

use ::markdown::mdast;

/// The longest prefix of `value` that fits in `max_bytes` without splitting a
/// character.
pub fn utf8_prefix(value: &str, max_bytes: usize) -> &str {
    let mut end = value.len().min(max_bytes);
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    &value[..end]
}

/// The directory a document's relative destinations resolve against.
pub fn base_path_for_file(path: &str) -> String {
    std::path::Path::new(path)
        .parent()
        .map(|parent| normalize_base_path(&parent.to_string_lossy()))
        .unwrap_or_default()
}

/// Escape `text` so a Markdown parser renders it literally.
///
/// User-authored message bodies are shown as written, not interpreted. Every
/// ASCII punctuation character is escapable in CommonMark, and escaping all of
/// them is what keeps a line that happens to start with `#`, `>` or `-`, or a
/// word wrapped in `*`, from turning into a block or inline construct.
///
/// A newline in the source is a Markdown soft break, which a rendered paragraph
/// is free to fold into a space; the writer's own line breaks are kept by
/// ending every line with the two trailing spaces that make it a hard break.
pub fn escape_markdown_literal(text: &str) -> String {
    let mut escaped = String::with_capacity(text.len());
    for character in text.chars() {
        if character.is_ascii_punctuation() {
            escaped.push('\\');
        }
        if character == '\n' {
            escaped.push_str("  ");
        }
        escaped.push(character);
    }
    escaped
}

/// The plain text a Markdown source projects to.
///
/// This is the projection the timeline wants for previews, labels and search
/// excerpts: block text in reading order, one line per block, markup removed.
pub fn plain_text(source: &str) -> String {
    let mut output = String::new();
    match ::markdown::to_mdast(source, &::markdown::ParseOptions::gfm()) {
        Ok(root) => append_plain(&root, &mut output),
        // A source the parser rejects is still text the reader typed.
        Err(_) => output.push_str(source),
    }
    output.trim_end().to_string()
}

/// One resource a Markdown source points at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownResource {
    /// The destination exactly as written in the source.
    pub url: String,
    /// Whether the resource came from an image rather than a link.
    pub image: bool,
}

/// Every link and image destination a source names, in document order.
///
/// Reference-style links are resolved through the document's definitions.
pub fn resources(source: &str) -> Vec<MarkdownResource> {
    let Ok(root) = ::markdown::to_mdast(source, &::markdown::ParseOptions::gfm()) else {
        return Vec::new();
    };
    let mut definitions = BTreeMap::new();
    collect_definitions(&root, &mut definitions);
    let mut resources = Vec::new();
    collect_resources(&root, &definitions, &mut resources);
    resources
}

/// Normalize a workspace path written as a link destination.
///
/// Returns the path relative to the workspace root, or `None` when the
/// destination is not a workspace path: an external URL, a fragment, a data
/// URL, or something that walks out of the workspace.
pub fn resolve_workspace_path(base_path: &str, source: &str) -> Option<String> {
    if source.is_empty() || source.starts_with("//") || source.contains('\0') {
        return None;
    }
    let source = source.split(['?', '#']).next().unwrap_or(source);
    let source = strip_editor_location(source);
    let mut segments = if source.starts_with('/') {
        Vec::new()
    } else {
        normalize_base_path(base_path)
            .split('/')
            .filter(|segment| !segment.is_empty())
            .map(str::to_string)
            .collect::<Vec<_>>()
    };
    for segment in source.trim_start_matches('/').replace('\\', "/").split('/') {
        match segment {
            "" | "." => {}
            ".." => {
                segments.pop()?;
            }
            segment if segment.contains(':') => return None,
            segment => segments.push(segment.to_string()),
        }
    }
    (!segments.is_empty()).then(|| segments.join("/"))
}

fn normalize_base_path(base_path: &str) -> String {
    base_path
        .replace('\\', "/")
        .split('/')
        .filter(|segment| !segment.is_empty() && *segment != ".")
        .fold(Vec::<&str>::new(), |mut segments, segment| {
            if segment == ".." {
                segments.pop();
            } else {
                segments.push(segment);
            }
            segments
        })
        .join("/")
}

/// Drop a trailing `:line` or `:line:column` from a link destination.
fn strip_editor_location(source: &str) -> &str {
    let Some((without_last_number, last_number)) = source.rsplit_once(':') else {
        return source;
    };
    if last_number.is_empty() || !last_number.bytes().all(|byte| byte.is_ascii_digit()) {
        return source;
    }
    let Some((without_line, line)) = without_last_number.rsplit_once(':') else {
        return without_last_number;
    };
    if !line.is_empty() && line.bytes().all(|byte| byte.is_ascii_digit()) {
        without_line
    } else {
        without_last_number
    }
}

fn append_plain(node: &mdast::Node, output: &mut String) {
    let mut block = true;
    match node {
        mdast::Node::Root(root) => {
            for child in &root.children {
                append_plain(child, output);
            }
            block = false;
        }
        mdast::Node::Paragraph(paragraph) => append_inlines(&paragraph.children, output),
        mdast::Node::Heading(heading) => append_inlines(&heading.children, output),
        mdast::Node::Blockquote(quote) => {
            for child in &quote.children {
                append_plain(child, output);
            }
        }
        mdast::Node::List(list) => {
            for child in &list.children {
                append_plain(child, output);
            }
        }
        mdast::Node::ListItem(item) => {
            for child in &item.children {
                append_plain(child, output);
            }
        }
        mdast::Node::Table(table) => {
            for row in &table.children {
                append_plain(row, output);
            }
        }
        mdast::Node::TableRow(row) => {
            for (index, cell) in row.children.iter().enumerate() {
                if index > 0 {
                    output.push('\t');
                }
                append_plain(cell, output);
                block = false;
            }
        }
        mdast::Node::TableCell(cell) => {
            append_inlines(&cell.children, output);
            block = false;
        }
        mdast::Node::FootnoteDefinition(definition) => {
            output.push_str("[^");
            output.push_str(&definition.identifier);
            output.push_str("]: ");
            for child in &definition.children {
                append_plain(child, output);
            }
        }
        mdast::Node::Code(code) => output.push_str(&code.value),
        mdast::Node::Math(math) => output.push_str(&math.value),
        mdast::Node::Html(html) => output.push_str(&html.value),
        mdast::Node::Yaml(yaml) => output.push_str(&yaml.value),
        mdast::Node::Toml(toml) => output.push_str(&toml.value),
        mdast::Node::MdxjsEsm(esm) => output.push_str(&esm.value),
        mdast::Node::MdxFlowExpression(expression) => output.push_str(&expression.value),
        mdast::Node::MdxTextExpression(expression) => output.push_str(&expression.value),
        // Definitions carry a destination, not reader-visible text, and a
        // thematic break carries nothing at all.
        mdast::Node::Definition(_) | mdast::Node::ThematicBreak(_) => block = false,
        mdast::Node::Text(text) => {
            output.push_str(&text.value);
            block = false;
        }
        mdast::Node::InlineCode(code) => {
            output.push_str(&code.value);
            block = false;
        }
        mdast::Node::InlineMath(math) => {
            output.push_str(&math.value);
            block = false;
        }
        mdast::Node::Break(_) => {
            output.push('\n');
            block = false;
        }
        mdast::Node::Emphasis(emphasis) => {
            append_inlines(&emphasis.children, output);
            block = false;
        }
        mdast::Node::Strong(strong) => {
            append_inlines(&strong.children, output);
            block = false;
        }
        mdast::Node::Delete(delete) => {
            append_inlines(&delete.children, output);
            block = false;
        }
        mdast::Node::Link(link) => {
            append_inlines(&link.children, output);
            block = false;
        }
        mdast::Node::LinkReference(link) => {
            append_inlines(&link.children, output);
            block = false;
        }
        mdast::Node::Image(image) => {
            output.push_str(&image.alt);
            block = false;
        }
        mdast::Node::ImageReference(image) => {
            output.push_str(&image.alt);
            block = false;
        }
        mdast::Node::FootnoteReference(reference) => {
            output.push_str("[^");
            output.push_str(&reference.identifier);
            output.push(']');
            block = false;
        }
        mdast::Node::MdxJsxFlowElement(element) => {
            for child in &element.children {
                append_plain(child, output);
            }
        }
        mdast::Node::MdxJsxTextElement(element) => {
            for child in &element.children {
                append_plain(child, output);
            }
            block = false;
        }
    }
    if block && !output.ends_with('\n') {
        output.push('\n');
    }
}

fn append_inlines(nodes: &[mdast::Node], output: &mut String) {
    for node in nodes {
        append_plain(node, output);
    }
}

fn collect_definitions(node: &mdast::Node, definitions: &mut BTreeMap<String, String>) {
    if let mdast::Node::Definition(definition) = node {
        definitions
            .entry(normalize_identifier(&definition.identifier))
            .or_insert_with(|| definition.url.clone());
    }
    for child in node.children().into_iter().flatten() {
        collect_definitions(child, definitions);
    }
}

fn collect_resources(
    node: &mdast::Node,
    definitions: &BTreeMap<String, String>,
    resources: &mut Vec<MarkdownResource>,
) {
    match node {
        mdast::Node::Link(link) => resources.push(MarkdownResource {
            url: link.url.clone(),
            image: false,
        }),
        mdast::Node::Image(image) => resources.push(MarkdownResource {
            url: image.url.clone(),
            image: true,
        }),
        mdast::Node::LinkReference(link) => {
            if let Some(url) = definitions.get(&normalize_identifier(&link.identifier)) {
                resources.push(MarkdownResource {
                    url: url.clone(),
                    image: false,
                });
            }
        }
        mdast::Node::ImageReference(image) => {
            if let Some(url) = definitions.get(&normalize_identifier(&image.identifier)) {
                resources.push(MarkdownResource {
                    url: url.clone(),
                    image: true,
                });
            }
        }
        _ => {}
    }
    for child in node.children().into_iter().flatten() {
        collect_resources(child, definitions, resources);
    }
}

fn normalize_identifier(identifier: &str) -> String {
    identifier
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
        .to_lowercase()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn plain_text_drops_markup_and_keeps_block_order() {
        let text = plain_text("# Title\n\nBody **bold** and `code`.\n\n- one\n- two\n");
        assert_eq!(text, "Title\nBody bold and code.\none\ntwo");
    }

    #[test]
    fn plain_text_keeps_code_and_link_labels() {
        let text = plain_text("See [the guide](docs/guide.md):\n\n```rust\nfn main() {}\n```\n");
        assert!(text.contains("See the guide:"), "{text}");
        assert!(text.contains("fn main() {}"), "{text}");
    }

    #[test]
    fn plain_text_rejects_nothing_it_cannot_parse() {
        assert_eq!(plain_text("[unclosed"), "[unclosed");
    }

    #[test]
    fn resources_collect_links_images_and_references() {
        let found =
            resources("![a](a.png) [b](https://example.com)\n\n[c][ref]\n\n[ref]: docs/c.md\n");
        let urls: Vec<&str> = found.iter().map(|item| item.url.as_str()).collect();
        assert_eq!(urls, vec!["a.png", "https://example.com", "docs/c.md"]);
        assert!(found[0].image);
        assert!(!found[2].image);
    }

    #[test]
    fn workspace_paths_resolve_against_the_document_directory() {
        assert_eq!(
            resolve_workspace_path("docs/guide", "../assets/a.png").as_deref(),
            Some("docs/assets/a.png")
        );
        assert_eq!(
            resolve_workspace_path("", "/README.md").as_deref(),
            Some("README.md")
        );
        assert_eq!(
            resolve_workspace_path("docs", "notes.md:12:3").as_deref(),
            Some("docs/notes.md")
        );
        assert_eq!(resolve_workspace_path("docs", "https://example.com"), None);
        assert_eq!(
            resolve_workspace_path("docs", "data:image/png;base64,AA"),
            None
        );
        assert_eq!(resolve_workspace_path("", "#fragment"), None);
        assert_eq!(resolve_workspace_path("", "../../escape.md"), None);
    }

    #[test]
    fn literal_escaping_keeps_every_character_readable() {
        let source = "# not a heading *not emphasis* [not a link](x) 2. not a list";
        let escaped = escape_markdown_literal(source);
        assert!(!escaped.is_empty());
        assert!(escaped.contains("\\#"));
        assert!(escaped.contains("\\*"));
        assert!(escaped.contains("\\["));
        assert_eq!(plain_text(&escaped), source);
    }

    #[test]
    fn literal_escaping_keeps_the_writers_line_breaks() {
        let source = "first line\nsecond line\n\n  indented text";
        let escaped = escape_markdown_literal(source);
        // Every source line ends in the two spaces that make it a hard break,
        // so a renderer cannot fold the writer's newlines into spaces.
        assert!(escaped.contains("first line  \nsecond line  \n"));
        assert!(escaped.ends_with("  indented text"));
        assert_eq!(
            plain_text(&escaped),
            "first line\nsecond line\nindented text"
        );
    }
}

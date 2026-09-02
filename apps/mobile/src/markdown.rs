use std::sync::Arc;

use gpui::{
    AnyElement, AppContext as _, Context, IntoElement, ParentElement as _, Render, Styled as _,
    Task, WeakEntity, Window, div, prelude::FluentBuilder as _, px, rgb,
};
use vibex_markdown::{
    Block, BlockNode, MarkdownDocument, MarkdownInput, MarkdownSurface, parse_markdown, plain_text,
    utf8_prefix,
};

use crate::theme;

const PENDING_RENDER_MAX_BYTES: usize = 16 * 1024;
const PARSE_COALESCE_DELAY: std::time::Duration = std::time::Duration::from_millis(16);

pub struct MarkdownView {
    source: Arc<str>,
    revision: u64,
    document: Arc<MarkdownDocument>,
    parse_generation: u64,
    parse_task: Option<Task<()>>,
}

impl MarkdownView {
    pub fn new(source: Arc<str>, revision: u64, cx: &mut Context<Self>) -> Self {
        let input = markdown_input(source.clone(), revision);
        let mut this = Self {
            source,
            revision,
            document: pending_document(&input),
            parse_generation: 1,
            parse_task: None,
        };
        this.queue_background_parse(input, 1, cx);
        this
    }

    pub fn set_source(&mut self, source: Arc<str>, revision: u64, cx: &mut Context<Self>) {
        if self.revision == revision && self.source.as_ref() == source.as_ref() {
            return;
        }
        self.source = source.clone();
        self.revision = revision;
        self.parse_generation = self.parse_generation.saturating_add(1).max(1);
        let generation = self.parse_generation;
        let input = markdown_input(source, revision);
        self.document = pending_document(&input);
        self.queue_background_parse(input, generation, cx);
        cx.notify();
    }

    fn queue_background_parse(
        &mut self,
        input: MarkdownInput,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if self.parse_task.is_some() {
            return;
        }
        let background = cx.background_executor().clone();
        let parse = cx.background_spawn(async move {
            background.timer(PARSE_COALESCE_DELAY).await;
            Arc::new(parse_markdown(input))
        });
        self.parse_task = Some(cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let document = parse.await;
            let _ = entity.update(cx, |this, cx| {
                this.parse_task = None;
                if this.parse_generation == generation
                    && this.revision == document.revision
                    && this.source.as_ref() == document.source.as_ref()
                {
                    this.document = document;
                    cx.notify();
                } else {
                    let latest = markdown_input(this.source.clone(), this.revision);
                    let latest_generation = this.parse_generation;
                    this.queue_background_parse(latest, latest_generation, cx);
                }
            });
        }));
    }
}

pub fn render(source: Arc<str>, revision: u64, cx: &mut Context<MarkdownView>) -> MarkdownView {
    MarkdownView::new(source, revision, cx)
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(13.0))
            .line_height(px(19.0))
            .text_color(theme::text_secondary())
            .children(render_blocks(&self.document.blocks, 0))
    }
}

fn markdown_input(source: Arc<str>, revision: u64) -> MarkdownInput {
    MarkdownInput::new(source, "", revision).surface(MarkdownSurface::Agent)
}

fn pending_document(input: &MarkdownInput) -> Arc<MarkdownDocument> {
    let mut fallback = input.clone();
    fallback.source = Arc::from(utf8_prefix(&input.source, PENDING_RENDER_MAX_BYTES));
    Arc::new(MarkdownDocument::literal(
        &fallback,
        "mobile_markdown_parse_pending",
        "Markdown parsing is running in the background",
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pending_document_caps_large_source_without_splitting_utf8() {
        let source = "😀".repeat(PENDING_RENDER_MAX_BYTES / 4 + 1);
        let input = markdown_input(Arc::from(source.as_str()), 1);
        let document = pending_document(&input);

        assert!(document.source.len() <= PENDING_RENDER_MAX_BYTES);
        assert!(document.source.len() < source.len());
        assert!(source.is_char_boundary(document.source.len()));
    }
}

fn render_blocks(blocks: &[BlockNode], depth: usize) -> Vec<AnyElement> {
    blocks
        .iter()
        .map(|node| render_block(node, depth))
        .collect()
}

fn render_block(node: &BlockNode, depth: usize) -> AnyElement {
    match &node.kind {
        Block::Paragraph(inlines) => div()
            .whitespace_normal()
            .child(plain_text(inlines))
            .into_any_element(),
        Block::Heading { level, content, .. } => {
            let size = match level {
                1 => 19.0,
                2 => 17.0,
                _ => 14.0,
            };
            div()
                .mt_1()
                .text_size(px(size))
                .text_color(theme::text_primary())
                .child(plain_text(content))
                .into_any_element()
        }
        Block::Quote(children) => div()
            .border_l_2()
            .border_color(rgb(theme::BORDER_DEFAULT))
            .pl_3()
            .children(render_blocks(children, depth + 1))
            .into_any_element(),
        Block::Callout {
            title, children, ..
        } => div()
            .rounded(px(theme::RADIUS_CARD))
            .border_1()
            .border_color(rgb(theme::BORDER_DEFAULT))
            .bg(theme::bg_card_dim())
            .p_3()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_size(px(12.0))
                    .text_color(rgb(theme::ACCENT_YELLOW))
                    .child(title.clone()),
            )
            .children(render_blocks(children, depth + 1))
            .into_any_element(),
        Block::Code {
            language, source, ..
        } => code_block(language.as_deref(), source),
        Block::Diff { source } => code_block(Some("diff"), source),
        Block::Math { source } => code_block(Some("math"), source),
        Block::Diagram { source, .. } => code_block(Some("diagram"), source),
        Block::List { start, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(items.iter().enumerate().map(|(index, item)| {
                let marker = match (item.checked, start) {
                    (Some(true), _) => "[x]".to_string(),
                    (Some(false), _) => "[ ]".to_string(),
                    (None, Some(start)) => format!("{}.", start + index as u64),
                    (None, None) => "-".to_string(),
                };
                div()
                    .flex()
                    .items_start()
                    .gap_2()
                    .pl(px((depth.min(3) * 8) as f32))
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme::text_muted())
                            .child(marker),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .children(render_blocks(&item.children, depth + 1)),
                    )
            }))
            .into_any_element(),
        Block::DefinitionList(items) => div()
            .flex()
            .flex_col()
            .gap_2()
            .children(items.iter().map(|item| {
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .text_color(theme::text_primary())
                            .child(plain_text(&item.term)),
                    )
                    .children(
                        item.definitions
                            .iter()
                            .flat_map(|blocks| render_blocks(blocks, depth + 1)),
                    )
            }))
            .into_any_element(),
        Block::Table { header, rows, .. } => {
            let mut all_rows = Vec::new();
            if let Some(header) = header {
                all_rows.push((true, header));
            }
            all_rows.extend(rows.iter().map(|row| (false, row)));
            div()
                .border_1()
                .border_color(rgb(theme::BORDER_DEFAULT))
                .rounded(px(theme::RADIUS_CARD))
                .overflow_hidden()
                .children(all_rows.into_iter().map(|(header, row)| {
                    div()
                        .flex()
                        .border_b_1()
                        .border_color(rgb(theme::BORDER_SUBTLE))
                        .when(header, |row| row.bg(theme::bg_card_dim()))
                        .children(row.cells.iter().map(|cell| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .px_2()
                                .py_1()
                                .child(plain_text(cell))
                        }))
                }))
                .into_any_element()
        }
        Block::ThematicBreak => div()
            .h(px(1.0))
            .my_2()
            .bg(rgb(theme::BORDER_DEFAULT))
            .into_any_element(),
        Block::Details {
            summary, children, ..
        } => div()
            .flex()
            .flex_col()
            .gap_2()
            .child(
                div()
                    .text_color(theme::text_primary())
                    .child(plain_text(summary)),
            )
            .children(render_blocks(children, depth + 1))
            .into_any_element(),
        Block::Progress { value, max, label } => div()
            .text_color(theme::text_muted())
            .child(
                label
                    .clone()
                    .unwrap_or_else(|| format!("Progress: {value:.0}/{max:.0}")),
            )
            .into_any_element(),
        Block::Image(image) => div()
            .text_color(theme::text_muted())
            .child(format!("[Image: {}]", image.alt))
            .into_any_element(),
        Block::FootnoteDefinition { label, children } => div()
            .flex()
            .gap_2()
            .child(format!("[^{label}]"))
            .child(div().flex_1().children(render_blocks(children, depth + 1)))
            .into_any_element(),
        Block::SafeHtml(children) => div()
            .flex()
            .flex_col()
            .children(render_blocks(children, depth + 1))
            .into_any_element(),
        Block::Literal(source) => div()
            .whitespace_normal()
            .child(source.clone())
            .into_any_element(),
        Block::TableOfContents => div().into_any_element(),
    }
}

fn code_block(language: Option<&str>, source: &str) -> AnyElement {
    div()
        .rounded(px(theme::RADIUS_CARD))
        .border_1()
        .border_color(rgb(theme::BORDER_DEFAULT))
        .bg(theme::bg_card_dim())
        .overflow_hidden()
        .when_some(
            language.filter(|language| !language.is_empty()),
            |block, language| {
                block.child(
                    div()
                        .border_b_1()
                        .border_color(rgb(theme::BORDER_SUBTLE))
                        .px_3()
                        .py_1()
                        .text_size(px(10.0))
                        .text_color(theme::text_muted())
                        .child(language.to_string()),
                )
            },
        )
        .child(
            div()
                .p_3()
                .text_size(px(12.0))
                .line_height(px(18.0))
                .text_color(theme::text_secondary())
                .whitespace_normal()
                .child(source.to_string()),
        )
        .into_any_element()
}

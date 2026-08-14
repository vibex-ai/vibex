use gpui::{
    AnyElement, Div, IntoElement as _, ParentElement as _, Styled as _, div,
    prelude::FluentBuilder as _, px, rgb,
};
use vibex_markdown::{
    Block, BlockNode, MarkdownInput, MarkdownSurface, parse_markdown, plain_text,
};

use crate::theme;

pub fn render(source: &str, revision: u64) -> Div {
    let document =
        parse_markdown(MarkdownInput::new(source, "", revision).surface(MarkdownSurface::Agent));
    div()
        .flex()
        .flex_col()
        .gap_2()
        .text_size(px(13.0))
        .line_height(px(19.0))
        .text_color(theme::text_secondary())
        .children(render_blocks(&document.blocks, 0))
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

//! Markdown rendering for the mobile timeline.
//!
//! Rendering belongs to the gpui-component text view, exactly as it does on the
//! desktop: this module only holds the state a streamed row keeps across
//! frames — the source it was last handed and the revision that produced it —
//! and hands the text view the themed presentation.

use std::sync::Arc;

use gpui::{
    App, Context, ElementId, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};
use gpui_base::text::{TextView, TextViewStyle};
use vibex_ui::markdown::plain_text;

use crate::theme;

pub struct MarkdownView {
    source: Arc<str>,
    revision: u64,
}

impl MarkdownView {
    pub fn new(source: Arc<str>, revision: u64, _cx: &mut Context<Self>) -> Self {
        Self { source, revision }
    }

    pub fn set_source(&mut self, source: Arc<str>, revision: u64, cx: &mut Context<Self>) {
        if self.revision == revision && self.source.as_ref() == source.as_ref() {
            return;
        }
        self.source = source;
        self.revision = revision;
        cx.notify();
    }
}

/// The plain text a Markdown source projects to, for reasoning summaries.
pub fn markdown_plain_text(source: &str) -> String {
    plain_text(source)
}

/// The style the phone's Markdown wears: the component theme's colors at the
/// timeline's secondary foreground.
fn text_view_style(cx: &App) -> TextViewStyle {
    TextViewStyle::from_theme(&gpui_base::Theme::global(cx))
        .with_foreground(theme::text_secondary())
}

pub fn render(source: Arc<str>, revision: u64, cx: &mut Context<MarkdownView>) -> MarkdownView {
    MarkdownView::new(source, revision, cx)
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .flex()
            .flex_col()
            .gap_2()
            .text_size(px(13.0))
            .line_height(px(19.0))
            .text_color(theme::text_secondary())
            .child(
                TextView::markdown(
                    ElementId::Name(SharedString::from(format!(
                        "mobile-markdown:{}",
                        cx.entity_id()
                    ))),
                    self.source.clone(),
                )
                .style(text_view_style(cx)),
            )
    }
}

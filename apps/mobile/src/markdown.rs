//! Markdown rendering for the mobile timeline.
//!
//! Rendering belongs to the gpui-component text view, exactly as it does on the
//! desktop: this module only holds the state a streamed row keeps across
//! frames — the source it was last handed and the revision that produced it —
//! and hands the text view the themed presentation.

use std::sync::Arc;

use gpui::{
    App, Context, ElementId, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div,
};
use gpui_base::text::{TextView, TextViewStyle};
use vibex_ui::markdown::plain_text;

use crate::theme;

/// Body text and its line height share the same rem scale as the live window.
pub const LINE_HEIGHT_REM: f32 = 0.875 * 1.5;

pub struct MarkdownView {
    source: Arc<str>,
    revision: u64,
    muted: bool,
}

impl MarkdownView {
    pub fn new(source: Arc<str>, revision: u64, _cx: &mut Context<Self>) -> Self {
        Self {
            source,
            revision,
            muted: false,
        }
    }

    pub fn set_muted(&mut self, muted: bool, cx: &mut Context<Self>) {
        if self.muted != muted {
            self.muted = muted;
            cx.notify();
        }
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

/// Commentary and answers use the body foreground; reasoning stays secondary.
fn text_view_style(muted: bool, cx: &App) -> TextViewStyle {
    TextViewStyle::from_theme(&gpui_base::Theme::global(cx)).with_foreground(if muted {
        theme::text_muted()
    } else {
        theme::text_primary()
    })
}

pub fn render(
    source: Arc<str>,
    revision: u64,
    muted: bool,
    cx: &mut Context<MarkdownView>,
) -> MarkdownView {
    let mut view = MarkdownView::new(source, revision, cx);
    view.muted = muted;
    view
}

impl Render for MarkdownView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .w_full()
            .min_w_0()
            .flex_none()
            .flex()
            .flex_col()
            .gap_2()
            .text_sm()
            .line_height(gpui::relative(1.5))
            .text_color(if self.muted {
                theme::text_muted()
            } else {
                theme::text_primary()
            })
            .child(
                TextView::markdown(
                    ElementId::Name(SharedString::from(format!(
                        "mobile-markdown:{}",
                        cx.entity_id()
                    ))),
                    self.source.clone(),
                )
                .style(text_view_style(self.muted, cx)),
            )
    }
}

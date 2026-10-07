//! Markdown rendering for the desktop surfaces.
//!
//! Vibex ships no Markdown renderer of its own. Every surface renders with the
//! text view behind `gpui_component::text::TextView` — `gpui_base::text::TextView`
//! — and this module only adapts Vibex data to it: the presentation style a
//! surface wears, the embedded-image decoding a preview's own resolver needs,
//! and the projections shared with the mobile client through
//! [`vibex_ui::markdown`].
//!
//! Nothing here renders: the component view owns layout, selection, code
//! highlighting, tables and images.

use gpui::{App, HighlightStyle, StyleRefinement, Styled as _};
use gpui_base::text::TextViewStyle;
use gpui_component::ActiveTheme as _;

pub use gpui_base::text::{RangeHighlight, RenderedText, TextView, TextViewState};

/// Preserve the measured prefix and selection while an answer grows. A full
/// replacement is reserved for corrections, edits, or a different document.
pub(crate) fn update_timeline_text(
    state: &mut TextViewState,
    previous: &str,
    source: &str,
    streaming: bool,
    cx: &mut gpui::Context<TextViewState>,
) {
    state.set_motion(timeline_text_motion(streaming));
    if let Some(delta) = source.strip_prefix(previous) {
        state.push_str(delta, cx);
    } else {
        state.set_text(source, cx);
    }
}

pub(crate) fn timeline_text_motion(streaming: bool) -> gpui_base::text::TextViewMotion {
    gpui_base::text::TextViewMotion::default().with_stream_fade(if streaming {
        crate::motion::FADE_QUICK.total()
    } else {
        std::time::Duration::ZERO
    })
}
pub use vibex_ui::markdown::{
    MarkdownResource, base_path_for_file, escape_markdown_literal, plain_text,
    resolve_workspace_path, resources, utf8_prefix,
};

/// The largest embedded image a preview decodes, matching the previous
/// renderer's budget.
const DATA_IMAGE_MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;

/// Decode an embedded `data:` image.
///
/// The text view decodes these itself only while it carries no image resolver;
/// a preview that resolves workspace images installs one, so it has to keep the
/// embedded case working on its own.
pub fn data_url_image(url: &str) -> Option<std::sync::Arc<gpui::Image>> {
    if url.len() > DATA_IMAGE_MAX_ENCODED_BYTES {
        return None;
    }
    let (header, encoded) = url.split_once(',')?;
    let (mime, encoding) = header.split_once(';')?;
    if !encoding.eq_ignore_ascii_case("base64") {
        return None;
    }
    let format = gpui::ImageFormat::from_mime_type(mime.strip_prefix("data:")?)?;
    let bytes =
        base64::Engine::decode(&base64::engine::general_purpose::STANDARD, encoded.trim()).ok()?;
    if bytes.is_empty() {
        return None;
    }
    Some(std::sync::Arc::new(gpui::Image::from_bytes(format, bytes)))
}

/// Which surface a rendered document belongs to.
///
/// The distinction is presentation only: it selects the text view style and
/// nothing about parsing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MarkdownPresentation {
    /// A file preview or release note: the themed default.
    #[default]
    Document,
    /// An agent answer: the themed default.
    Agent,
    /// A reasoning thought: the themed default at the muted foreground.
    Thought,
}

/// The style a presentation renders with.
///
/// `None` leaves the text view on the defaults the component theme installed,
/// which is what the agent and document surfaces want.
pub fn text_view_style(presentation: MarkdownPresentation, cx: &App) -> Option<TextViewStyle> {
    match presentation {
        MarkdownPresentation::Thought => {
            let theme = cx.theme();
            Some(themed_text_view_style(theme).with_foreground(theme.muted_foreground))
        }
        MarkdownPresentation::Document | MarkdownPresentation::Agent => None,
    }
}

/// The base rich-text style the component theme derives.
///
/// Mirrors the component adapter: a text view that carries its own style
/// replaces the installed defaults wholesale, so the theme's palette and the
/// corner radii it gives code blocks and tables are repeated here.
fn themed_text_view_style(theme: &gpui_component::Theme) -> TextViewStyle {
    let radius = theme.semantic_tokens().radius.md;
    let mut table = StyleRefinement::default();
    table.corner_radii.top_left = Some(radius.into());
    table.corner_radii.top_right = Some(radius.into());
    table.corner_radii.bottom_left = Some(radius.into());
    table.corner_radii.bottom_right = Some(radius.into());
    let code_block = StyleRefinement {
        corner_radii: table.corner_radii.clone(),
        ..StyleRefinement::default()
    };
    let table_head = StyleRefinement::default()
        .bg(theme.table_head)
        .text_color(theme.table_head_foreground);

    TextViewStyle::default()
        .with_foreground(theme.foreground)
        .with_muted_foreground(theme.muted_foreground)
        .with_link(theme.link)
        .with_selection(theme.selection)
        .with_code_background(theme.muted)
        .with_border(theme.border)
        .with_code_block(code_block)
        .with_table(table)
        .with_table_head(table_head)
        .with_inline_code(HighlightStyle {
            background_color: Some(theme.accent),
            ..Default::default()
        })
        .with_dark(theme.is_dark())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, TestAppContext};

    #[gpui::test]
    fn streaming_append_keeps_selection_and_accepts_unicode_and_corrections(
        cx: &mut TestAppContext,
    ) {
        cx.update(gpui_component::init);
        let initial = "**已有段落**\n\n";
        let state =
            cx.update(|cx| cx.new(|cx| TextViewState::markdown(initial, cx).selectable(true)));
        cx.run_until_parked();
        state.update(cx, |state, cx| state.select_all(cx));
        let appended = "**已有段落**\n\n下一段 🦀";
        state.update(cx, |state, cx| {
            update_timeline_text(state, initial, appended, true, cx)
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert!(state.rendered_text().as_str().contains("下一段 🦀"));
            assert!(state.selected_text().contains("已有段落"));
        });
        state.update(cx, |state, cx| {
            update_timeline_text(state, appended, "修订后的内容", false, cx)
        });
        cx.run_until_parked();
        state.read_with(cx, |state, _| {
            assert_eq!(state.rendered_text().as_str().trim(), "修订后的内容")
        });
    }
}

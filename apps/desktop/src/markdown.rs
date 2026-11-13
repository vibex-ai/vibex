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

use gpui::{
    App, HighlightStyle, InteractiveElement as _, ParentElement as _, StyleRefinement, Styled as _,
};
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
    // A fade that outlives the gap between two streamed updates keeps its own
    // 33ms tick alive for the whole answer. The body already arrives in 50ms
    // chunks, so the fade adds a second, independent frame clock without adding
    // readable motion. Settled text still fades.
    let fade = if streaming {
        std::time::Duration::ZERO
    } else {
        crate::motion::FADE_QUICK.total()
    };
    gpui_base::text::TextViewMotion::default().with_stream_fade(fade)
}
pub use vibex_ui::markdown::{
    MarkdownLinkKind, MarkdownResource, base_path_for_file, escape_markdown_literal, is_file_url,
    is_windows_drive_path, local_file_path, markdown_link_kind, plain_text, resolve_workspace_path,
    resources, utf8_prefix,
};

/// The largest embedded image a preview decodes, matching the previous
/// renderer's budget.
const DATA_IMAGE_MAX_ENCODED_BYTES: usize = 8 * 1024 * 1024;

/// The widest a link's destination tooltip may grow before it wraps.
///
/// A query string can be longer than the window is wide, and a tooltip is not
/// the place to read it in one line. The destination still wraps inside the
/// tooltip's own chrome, so the reader sees all of it without the plate
/// stretching past the edge of the screen.
const LINK_TOOLTIP_MAX_WIDTH: gpui::Rems = gpui::rems(32.0);

/// The tooltip a rendered link shows while the pointer rests on it.
///
/// A transcript states a destination only through the link's label, and a label
/// shortened for reading does not say where the link goes. The text view paints
/// this beside the pointer and keeps it while the pointer stays on the link, so
/// the builder only owns the content: the kit supplies the chrome, the delay and
/// the dismissal.
pub fn link_tooltip(
    url: &gpui::SharedString,
    window: &mut gpui::Window,
    cx: &mut App,
) -> gpui::AnyView {
    let destination = url.to_string();
    gpui_component::tooltip::Tooltip::element(move |_, _| {
        gpui::div()
            // `debug_bounds` looks the plate up by this name in tests; the
            // destination is the plate's only child, so seeing the plate is
            // seeing the destination.
            .debug_selector(|| "markdown-link-destination".to_string())
            .max_w(LINK_TOOLTIP_MAX_WIDTH)
            .child(destination.clone())
    })
    .build(window, cx)
}

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
/// which is what the agent and document surfaces want: the kit's own body
/// typography, tracked as the kit revises it. That is deliberate. The paragraph
/// gap, heading scale, and table metrics are tuned as one set, so pinning a
/// single value back would match neither the old design nor the new one.
/// gpui-kit 0.7.2 tightened the gap from `rems(1.)` to `rems(0.75)`; restoring
/// the old spacing is one `.with_paragraph_gap(rems(1.))` on a
/// `TextViewStyle::default()` built here.
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
        // The component adapter gives table body rows a hover wash; a style
        // built here replaces that adapter wholesale, so the wash has to be
        // named again or a themed table stops answering the pointer.
        .with_table_row_hover(theme.table_hover)
        .with_inline_code(HighlightStyle {
            background_color: Some(theme.accent),
            ..Default::default()
        })
        .with_dark(theme.is_dark())
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::{AppContext as _, Context, IntoElement, Render, TestAppContext, Window, div, px};

    /// The theme a surface's own style is derived from.
    fn themed_component_theme() -> gpui_component::Theme {
        gpui_component::Theme::default()
    }

    /// A style built here replaces the component adapter wholesale, so it has
    /// to carry the table row wash the adapter installs. The kit added the wash
    /// in 0.9.1; dropping it would leave every themed table inert to the
    /// pointer while the unthemed ones still answered it.
    #[test]
    fn the_themed_style_keeps_the_table_row_hover_wash() {
        let theme = themed_component_theme();
        let style = themed_text_view_style(&theme);
        assert_eq!(style.table_row_hover(), Some(theme.table_hover));
    }

    /// A live answer already repaints on its own 30fps stream clock. A 150ms
    /// fade between those updates would keep a second 33ms clock alive for the
    /// whole turn, so streaming text must not schedule one.
    #[test]
    fn streaming_text_does_not_schedule_a_fade_clock() {
        assert_eq!(
            timeline_text_motion(true).stream_fade(),
            std::time::Duration::ZERO
        );
        assert_eq!(
            timeline_text_motion(false).stream_fade(),
            crate::motion::FADE_QUICK.total()
        );
    }

    struct TooltipHost {
        view: gpui::AnyView,
    }

    impl Render for TooltipHost {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div().child(self.view.clone())
        }
    }

    /// A link's tooltip states where the link goes, which the label may hide.
    #[gpui::test]
    fn a_links_tooltip_names_its_destination(cx: &mut TestAppContext) {
        cx.update(gpui_component::init);
        let url = "file:///tmp/report.json";
        let (_, cx) = cx.add_window_view(|window, cx| TooltipHost {
            view: link_tooltip(&gpui::SharedString::from(url), window, cx),
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.refresh();
            let _ = window.draw(cx);
        });
        let painted = cx
            .debug_bounds("markdown-link-destination")
            .unwrap_or_else(|| panic!("the tooltip should paint {url}"));
        assert!(
            painted.size.width > px(0.0) && painted.size.height > px(0.0),
            "the destination must take room in the tooltip, got {painted:?}"
        );
    }

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

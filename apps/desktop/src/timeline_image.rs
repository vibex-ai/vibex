//! Read-event thumbnails in the activity timeline.
//!
//! A file read is one compact line, and a picture the Agent looked at is the
//! one read whose name cannot carry its subject: `final-scan.png` says nothing
//! about what the scan found. The row therefore draws the picture itself in the
//! slot the file-type mark normally occupies — the same place the screenshot's
//! call-out pointed at — and the thumbnail opens the full-window preview a
//! transcript image uses, so the read line stays the same height it measured.

use super::*;

#[cfg(test)]
#[path = "timeline_image_tests.rs"]
mod tests;

/// The height a read thumbnail occupies in an activity header, in rem.
///
/// It stays below [`timeline_activity::ROW_HEIGHT_REM`] once the chip's own
/// padding is counted, so adding a picture never changes a row's height and the
/// virtual list keeps the measurement it already has.
const THUMBNAIL_HEIGHT_REMS: f32 = 1.75;

/// The widest a read thumbnail may draw, in rem.
///
/// The picture keeps its own shape inside this box (`Contain`), so a screenshot
/// reads as a figure while a phone-width capture gets a narrow card; the box
/// only stops a panorama from pushing the file name out of the row.
const THUMBNAIL_WIDTH_REMS: f32 = 2.5;

/// The picture a read activity points at, when it names one in this workspace.
///
/// Only reads qualify: an edit or a write line already says what changed, and a
/// thumbnail there would suggest the row can be looked at rather than read. The
/// path goes through the same workspace resolver the file card's open action
/// uses, so a relative path, an absolute one under the root and a `file://` URI
/// all land on the file the read touched. A path outside the workspace, a
/// non-file scheme or a name that is not an image yields none, and the row
/// keeps its file-type mark.
pub(super) fn image_read_path(
    activity: &timeline_activity::Activity,
    workspace_root: Option<&str>,
) -> Option<PathBuf> {
    if activity.icon() != vibex_ui::timeline::ActivityKind::FileRead {
        return None;
    }
    let workspace_root = workspace_root?.trim();
    if workspace_root.is_empty() {
        return None;
    }
    let relative = agent_file_operation_preview_path(activity.path()?, Some(workspace_root))?;
    let path = Path::new(&local_file_path(workspace_root)).join(relative);
    // The extension (or a MIME type, when one is known) decides whether the
    // preview can decode it. This runs per row per frame, so it must stay a
    // string check — the bytes are read by the asset loader, not here.
    attachment_preview_image_format(&path, None)?;
    Some(path)
}

/// The thumbnail a read line draws in place of its file-type mark.
///
/// It is the same kind of card as a transcript image — a focusable button that
/// opens the preview and a tooltip that names it — only sized to sit inside the
/// activity header. The press is claimed here for the same reason the
/// transcript card claims it: text selection is window-level, and a press that
/// falls through would anchor a selection behind the click.
pub(super) fn thumbnail(
    row_id: &str,
    path: &Path,
    label: &str,
    entity: WeakEntity<VibexWorkbench>,
    cx: &App,
) -> AnyElement {
    let theme = cx.theme();
    let border = theme.border;
    let hover_border = theme.ring;
    let plate = theme.muted;
    let action_label: SharedString = format!(
        "{}: {label}",
        locale::text("Preview image", "预览图片", "預覽圖片")
    )
    .into();
    let tooltip_label = action_label.clone();
    let open_path = path.to_path_buf();
    let open_label = label.to_string();
    let fallback_color = theme.muted_foreground;
    div()
        .id(ElementId::Name(
            format!("timeline-read-image:{row_id}").into(),
        ))
        .debug_selector(|| format!("timeline-read-image:{row_id}"))
        .flex_none()
        .w(gpui::rems(THUMBNAIL_WIDTH_REMS))
        .h(gpui::rems(THUMBNAIL_HEIGHT_REMS))
        .overflow_hidden()
        .rounded(theme.radius * 0.5)
        .border_1()
        .border_color(border)
        .bg(plate)
        .cursor_pointer()
        .focusable()
        .tab_stop(true)
        .role(Role::Button)
        .aria_label(action_label)
        .tooltip(move |window, cx| Tooltip::new(tooltip_label.clone()).build(window, cx))
        .hover(move |style| style.border_color(hover_border))
        .focus_visible(move |style| style.border_color(hover_border))
        .active(|style| style.opacity(0.85))
        .on_mouse_down(MouseButton::Left, |_, _, cx| {
            gpui_base::GlobalState::suppress_text_selection(cx);
        })
        .on_click(move |_, window, cx| {
            // The row this sits in is a disclosure button; a click on the
            // picture previews it instead of toggling the line's body.
            cx.stop_propagation();
            let _ = entity.update(cx, |this, cx| {
                this.open_session_image_preview(open_path.clone(), open_label.clone(), window, cx);
            });
        })
        .child(
            img(path.to_path_buf())
                .size_full()
                .object_fit(ObjectFit::Contain)
                .with_fallback(move || {
                    div()
                        .size_full()
                        .flex()
                        .items_center()
                        .justify_center()
                        .child(
                            Icon::new(IconName::File)
                                .size(px(12.0))
                                .text_color(fallback_color),
                        )
                        .into_any_element()
                }),
        )
        .into_any_element()
}

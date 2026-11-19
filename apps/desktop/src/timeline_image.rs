//! Read-event thumbnails in the activity timeline.
//!
//! A file read is one compact line, and a picture the Agent looked at is the
//! one read whose name cannot carry its subject: `final-scan.png` says nothing
//! about what the scan found. The row therefore draws the picture itself in the
//! slot the file-type mark normally occupies — the same place the screenshot's
//! call-out pointed at — and the thumbnail opens the full-window preview a
//! transcript image uses, so the read line stays the same height it measured.

use std::path::Component;

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

/// The picture a read activity points at.
///
/// Only reads qualify: an edit or a write line already says what changed, and a
/// thumbnail there would suggest the row can be looked at rather than read. The
/// destination may live outside the session workspace — a capture in the home
/// directory or the system temp directory is still the picture the Agent looked
/// at — so an absolute path and a `file://` URI are used as written. A relative
/// path is made absolute against the workspace root. A non-file scheme or a
/// name that is not an image yields none, and the row keeps its file-type mark.
pub(super) fn image_read_path(
    activity: &timeline_activity::Activity,
    workspace_root: Option<&str>,
) -> Option<PathBuf> {
    if activity.icon() != vibex_ui::timeline::ActivityKind::FileRead {
        return None;
    }
    let path = read_image_path(activity.path()?, workspace_root)?;
    // The extension (or a MIME type, when one is known) decides whether the
    // preview can decode it. This runs per row per frame, so it must stay a
    // string check — the bytes are read by the asset loader, not here.
    attachment_preview_image_format(&path, None)?;
    Some(path)
}

/// Resolve a read's destination to the file it names.
///
/// An absolute path is already the file, wherever it lives. A relative path is
/// joined to the workspace root and normalised lexically, so `../shots/a.png`
/// names the sibling directory the read meant instead of failing the containment
/// check the file card's open action applies. A relative path with no workspace
/// has nothing to resolve against.
fn read_image_path(raw: &str, workspace_root: Option<&str>) -> Option<PathBuf> {
    let raw = raw.trim();
    if raw.is_empty() {
        return None;
    }
    let path = match url::Url::parse(raw) {
        Ok(url) if url.scheme() == "file" => url.to_file_path().ok()?,
        // A Windows path parses as a one-letter scheme; it is a path, not a URL.
        Ok(_) if Path::new(raw).is_absolute() => PathBuf::from(raw),
        // A non-file scheme (`https:`, `asset:`, ...) names no local file.
        Ok(_) => return None,
        Err(_) => PathBuf::from(local_file_path(raw)),
    };
    let path = if path.is_absolute() {
        path
    } else {
        let root = workspace_root?.trim();
        if root.is_empty() {
            return None;
        }
        Path::new(&local_file_path(root)).join(path)
    };
    Some(normalize_path(path))
}

/// Fold `.` and `..` without touching the filesystem.
///
/// The asset loader opens the path it is given, so a lexical normalisation is
/// only about identity: the same file reached as `shots/../a.png` and `a.png`
/// must produce one cache key, and a `..` above a workspace root means what it
/// says rather than silently naming the root.
fn normalize_path(path: PathBuf) -> PathBuf {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() {
                    normalized.push(component.as_os_str());
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    normalized
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

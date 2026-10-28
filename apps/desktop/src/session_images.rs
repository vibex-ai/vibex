//! Workspace images embedded in session Markdown.
//!
//! The text view only decodes `data:` URLs by itself; every other destination
//! is handed to GPUI's asset loader as a URI, which fetches over HTTP. A
//! transcript that points at a file in the session workspace therefore needs a
//! resolver: this plugin claims those image nodes, draws them from the
//! canonical workspace path as a bounded thumbnail, and turns a click into the
//! full-window image preview the attachment surfaces already use.

use std::{
    hash::{Hash as _, Hasher as _},
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
};

use gpui::{
    App, ElementId, ImageSource, InteractiveElement as _, MouseButton, ObjectFit,
    ParentElement as _, Pixels, Resource, Role, SharedString, Size,
    StatefulInteractiveElement as _, Styled as _, StyledImage as _, Window, div, img, px, relative,
    size,
};
use gpui_base::text::{
    InlineElement, InlineRenderContext, MarkdownNode, MarkdownParseContext, MarkdownPlugin,
    markdown_ast::Node as MarkdownAst,
};
use gpui_component::{ActiveTheme as _, tooltip::Tooltip};
use percent_encoding::percent_decode_str;

/// Opens a workspace image in the full-window preview.
pub(super) type SessionImageOpen =
    Arc<dyn Fn(PathBuf, String, &mut Window, &mut App) + Send + Sync>;

const NAME: &str = "vibex-session-image";
const MAX_IMAGE_BYTES: u64 = 8 * 1024 * 1024;

/// The largest a transcript thumbnail may draw, in rem.
///
/// A transcript is prose, and a screenshot pasted into it is evidence rather
/// than the subject: at its intrinsic 1432×928 the README capture filled most
/// of the session column and pushed the surrounding answer off screen. These
/// two caps keep every thumbnail the size of a figure — big enough to recognize,
/// small enough that the words around it stay the reading surface. They are
/// rem-based so interface zoom scales the figure with the text beside it, and
/// the ratio is applied rather than a fixed box so tall and wide images keep
/// their shape.
const THUMBNAIL_MAX_WIDTH_REMS: f32 = 20.0;
const THUMBNAIL_MAX_HEIGHT_REMS: f32 = 12.0;

/// The smallest edge a thumbnail may draw, in rem.
///
/// A 16 px icon is already its own best preview and would still have to carry
/// a click target. The floor keeps that target reachable (40 px at the default
/// scale); the picture itself stays its intrinsic size inside the card, so a
/// small mark is not blown up into a blurry tile.
const THUMBNAIL_MIN_EDGE_REMS: f32 = 2.5;

/// The shape an image with no readable header falls back to, so an undecodable
/// SVG or a corrupt file cannot collapse its slot to a hairline.
const THUMBNAIL_FALLBACK_ASPECT_RATIO: f32 = 4.0 / 3.0;

/// A local image claimed from a Markdown `Image` node.
struct SessionImage {
    path: PathBuf,
    label: String,
    /// Pixel dimensions after EXIF orientation, read from the file header when
    /// the document is parsed. `None` when the format keeps its size only in
    /// the payload (an SVG without a viewBox), in which case the thumbnail
    /// falls back to a 4:3 slot.
    intrinsic_size: Option<(u32, u32)>,
}

/// Claims workspace image nodes in a session's Markdown surface.
pub(super) struct SessionImagePlugin {
    workspace_root: PathBuf,
    /// Canonicalized on the first image the parser resolves, not on every
    /// frame: the plugin is rebuilt for every Markdown view render.
    canonical_root: OnceLock<Option<PathBuf>>,
    /// The width the transcript column currently offers, in pixels. The
    /// thumbnail cap is the smaller of this and its own rem-based limit, so a
    /// figure in a narrow pane shrinks with the column instead of overflowing
    /// it. Render-time only: a pane resize must not reparse the document.
    available_width: f32,
    open: SessionImageOpen,
    parser_revision: u64,
}

impl SessionImagePlugin {
    pub(super) fn new(
        workspace_root: PathBuf,
        available_width: f32,
        open: SessionImageOpen,
    ) -> Self {
        let parser_revision = {
            let mut hasher = std::collections::hash_map::DefaultHasher::new();
            workspace_root.hash(&mut hasher);
            hasher.finish()
        };
        Self {
            workspace_root,
            canonical_root: OnceLock::new(),
            available_width,
            open,
            parser_revision,
        }
    }

    /// A stable parser identity for this workspace. Rebuilding the plugin each
    /// frame keeps the parsed tree; changing the workspace reparses it.
    pub(super) fn parser_revision(&self) -> u64 {
        self.parser_revision
    }

    /// Resolve a Markdown image destination to a file inside the workspace.
    ///
    /// `data:` and HTTP(S) destinations are left to the built-in renderer.
    /// Everything else is treated as a workspace path: relative to the
    /// workspace root, root-relative when it starts with `/`, or absolute when
    /// it already lives under the root. `file://` is accepted too. The result
    /// must canonicalize back inside the workspace, name a regular file with an
    /// image extension, and stay under the same byte budget the Markdown
    /// preview uses.
    fn resolve(&self, url: &str) -> Option<PathBuf> {
        // A destination may carry a query or fragment the file never has; the
        // Markdown link resolver drops them too. Percent-encoded `?`/`#` stay
        // part of the name and are decoded below.
        let url = url
            .trim()
            .split(['?', '#'])
            .next()
            .unwrap_or_default()
            .trim();
        if url.is_empty() {
            return None;
        }
        let lowercase = url.to_ascii_lowercase();
        if lowercase.starts_with("data:")
            || lowercase.starts_with("http://")
            || lowercase.starts_with("https://")
        {
            return None;
        }

        // Compare canonical paths on both sides: the workspace may sit behind a
        // symlink (`/var` on macOS), and a lexical prefix would then reject
        // every real file under it.
        let root = self
            .canonical_root
            .get_or_init(|| self.workspace_root.canonicalize().ok());
        let root = root.as_deref()?;

        let path = if lowercase.starts_with("file:") {
            // The URL parser decodes percent escapes once; decoding first
            // would turn `%2520` into a space instead of a literal `%20`.
            let path = url::Url::parse(url).ok()?.to_file_path().ok()?;
            path.is_absolute().then_some(path)?
        } else {
            let decoded = percent_decode_str(url).decode_utf8().ok()?;
            let path = Path::new(decoded.as_ref());
            if !path.is_absolute() && url::Url::parse(decoded.as_ref()).is_ok() {
                // A non-file scheme (`asset:`, `vibex:`, ...) is not a local
                // path. Absolute Windows paths are checked above because
                // `Url::parse` reads their drive letter as a scheme.
                return None;
            }
            let relative = path.strip_prefix("/").unwrap_or(path);
            if path.is_absolute() && path.starts_with(root) {
                path.to_path_buf()
            } else {
                // A Markdown path that starts with `/` is workspace-relative,
                // matching how links are resolved elsewhere. Joining an
                // absolute path outside the root replaces the root on Windows
                // and the containment check below then rejects it.
                self.workspace_root.join(relative)
            }
        };

        let canonical = path.canonicalize().ok()?;
        if !canonical.starts_with(root) || !canonical.is_file() {
            return None;
        }
        if canonical.metadata().ok()?.len() > MAX_IMAGE_BYTES {
            return None;
        }
        // Extension (or MIME, when one is known) decides whether the preview
        // can decode it; a text file named `notes.png` is still rejected later
        // by the decoder, but this keeps the renderer from trying.
        super::attachment_preview_image_format(&canonical, None)?;
        Some(canonical)
    }
}

impl MarkdownPlugin for SessionImagePlugin {
    fn name(&self) -> &str {
        NAME
    }

    fn parse(&self, node: &MarkdownAst, cx: &MarkdownParseContext<'_>) -> Option<MarkdownNode> {
        let MarkdownAst::Image(image) = node else {
            return None;
        };
        let path = self.resolve(&image.url)?;
        let label = if image.alt.trim().is_empty() {
            path.file_name()?.to_string_lossy().into_owned()
        } else {
            image.alt.clone()
        };
        // Keep the original syntax, including a reference definition or title,
        // so selecting and copying the image round-trips as the author wrote it.
        let markdown = cx
            .node_source(node)
            .map(str::to_string)
            .unwrap_or_else(|| format!("![{}]({})", image.alt, image.url));
        let data = SessionImage {
            intrinsic_size: intrinsic_image_size(&path),
            path,
            label: label.clone(),
        };
        Some(
            MarkdownNode::new(NAME, data)
                .text(label.clone())
                .markdown(markdown)
                .accessibility_label(label),
        )
    }

    fn render_inline(
        &self,
        node: &MarkdownNode,
        context: &InlineRenderContext,
        _window: &mut Window,
        cx: &mut App,
    ) -> Option<InlineElement> {
        let image = node.data::<SessionImage>()?;
        let source_path: Arc<Path> = Arc::from(image.path.clone().into_boxed_path());
        let source = ImageSource::Resource(Resource::Path(source_path));
        let open = self.open.clone();
        let path = image.path.clone();
        let label = image.label.clone();
        let theme = cx.theme();
        let thumbnail = thumbnail_size(
            image.intrinsic_size,
            self.available_width,
            f32::from(context.rem_size()),
        );
        let border = theme.border;
        let hover_border = theme.ring;
        let plate = theme.muted;
        let start = node.source_range().map(|range| range.start).unwrap_or(0);
        let id = SharedString::from(format!("session-image:{start}:{}", image.path.display()));
        let action_label = format!(
            "{}: {label}",
            crate::locale::text("Preview image", "预览图片", "預覽圖片")
        );
        let tooltip_label = action_label.clone();
        let element = div()
            .id(ElementId::Name(id))
            .debug_selector(|| "vibex-session-image".to_string())
            .flex_none()
            .w(thumbnail.width)
            .h(thumbnail.height)
            // The session column is the cap above, but a figure can also sit in
            // a table cell or a narrower block, where the immediate container
            // is tighter than the column. Never paint past it.
            .max_w(relative(1.0))
            .overflow_hidden()
            .rounded(theme.radius)
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
                // The press belongs to this card, the way it belongs to a
                // button. Text selection in this stack is window-level, so a
                // press that is not claimed here anchors a selection in the
                // transcript behind the preview; the card's click then stops
                // the release from reaching the selection layer, which leaves
                // that anchor live. Every move over the opened preview drags it
                // — selecting the transcript nobody touched — and the drag
                // auto-scrolls the pane the anchor is in, which the preview
                // reads as a wheel and shrinks the picture with.
                gpui_base::GlobalState::suppress_text_selection(cx);
            })
            .on_click(move |_, window, cx| {
                cx.stop_propagation();
                open(path.clone(), label.clone(), window, cx);
            })
            .child(
                // `ScaleDown` keeps a small mark crisp at its own size rather
                // than growing it into the card; a large screenshot is bounded
                // by the card, which is already the fitted size.
                img(source).size_full().object_fit(ObjectFit::ScaleDown),
            );
        Some(InlineElement::new(element))
    }
}

/// The card a transcript thumbnail occupies.
///
/// `available_width` is the transcript column the figure sits in, so a narrow
/// pane shrinks the card instead of overflowing it; the rem caps keep it the
/// size of a figure at any interface zoom. The result preserves the image's
/// aspect ratio, never exceeds its intrinsic size, and grows a tiny picture's
/// card up to a clickable target while leaving the picture itself centred at its
/// own resolution.
fn thumbnail_size(
    intrinsic_size: Option<(u32, u32)>,
    available_width: f32,
    rem_size: f32,
) -> Size<Pixels> {
    let rem_size = if rem_size.is_finite() && rem_size > 0.0 {
        rem_size
    } else {
        16.0
    };
    let max_width = (rem_size * THUMBNAIL_MAX_WIDTH_REMS).min(if available_width.is_finite() {
        available_width
    } else {
        f32::INFINITY
    });
    let max_width = max_width.max(rem_size * THUMBNAIL_MIN_EDGE_REMS);
    let max_height = rem_size * THUMBNAIL_MAX_HEIGHT_REMS;
    let min_edge = rem_size * THUMBNAIL_MIN_EDGE_REMS;

    let Some((intrinsic_width, intrinsic_height)) = intrinsic_size
        .filter(|(width, height)| *width > 0 && *height > 0)
        .map(|(width, height)| (width as f32, height as f32))
    else {
        // No header to read: an SVG without a viewBox, or a file the decoder
        // cannot open. A 4:3 plate the size of the figure is a better guess
        // than a one-pixel card, and the fallback card never pretends to be
        // the picture's exact shape.
        let width = max_width.min(max_height * THUMBNAIL_FALLBACK_ASPECT_RATIO);
        return size(px(width), px(width / THUMBNAIL_FALLBACK_ASPECT_RATIO));
    };
    let scale = (max_width / intrinsic_width)
        .min(max_height / intrinsic_height)
        .min(1.0);
    // The two axes are floored independently: a tiny mark gets a target large
    // enough to click, and a panorama's hairline card gets a usable height. The
    // picture inside keeps its aspect ratio and is centred, so the floor widens
    // the card around it rather than stretching it.
    let card_width = (intrinsic_width * scale).clamp(min_edge.min(max_width), max_width);
    let card_height = (intrinsic_height * scale).clamp(min_edge.min(max_height), max_height);
    size(px(card_width), px(card_height))
}

/// Pixel dimensions of an image file, after the EXIF orientation it declares.
///
/// Read from the header only, so the parser never decodes the pixels it is
/// about to hand to the renderer. Formats whose size lives in the payload
/// (an SVG without a viewBox) and unreadable files return `None`, and the
/// thumbnail falls back to its default shape.
fn intrinsic_image_size(path: &Path) -> Option<(u32, u32)> {
    use image::ImageDecoder as _;

    let mut decoder = image::ImageReader::open(path)
        .ok()?
        .with_guessed_format()
        .ok()?
        .into_decoder()
        .ok()?;
    let orientation = decoder.orientation().ok()?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return None;
    }
    Some(match orientation {
        image::metadata::Orientation::Rotate90
        | image::metadata::Orientation::Rotate270
        | image::metadata::Orientation::Rotate90FlipH
        | image::metadata::Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui_base::text::MarkdownExtensions;
    use std::fs;

    fn plugin(root: &Path) -> SessionImagePlugin {
        SessionImagePlugin::new(root.to_path_buf(), 720.0, Arc::new(|_, _, _, _| {}))
    }

    fn write_image(root: &Path, relative: &str) -> PathBuf {
        let path = root.join(relative);
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent).unwrap();
        }
        fs::write(&path, b"\x89PNG\r\n\x1a\n").unwrap();
        path
    }

    fn canonical(path: &Path) -> PathBuf {
        path.canonicalize().unwrap()
    }

    #[test]
    fn thumbnails_keep_transcript_figures_bounded_and_in_shape() {
        // The README capture, 1432×928, must fit the 20rem × 12rem figure at
        // the default 16 px rem instead of taking its intrinsic pixel size.
        // Height is the binding cap here, and the ratio decides the width.
        let showcase = thumbnail_size(Some((1432, 928)), 720.0, 16.0);
        assert_eq!(showcase, size(px(296.27588), px(192.0)));
        assert!(showcase.width < px(320.0));

        // A portrait screenshot is capped by height and keeps its ratio.
        let tall = thumbnail_size(Some((900, 1600)), 720.0, 16.0);
        assert_eq!(tall.height, px(192.0));
        assert!(tall.width < px(192.0));

        // A narrow transcript column, not the rem cap, decides there.
        let narrow = thumbnail_size(Some((1432, 928)), 240.0, 16.0);
        assert_eq!(narrow.width, px(240.0));
        assert!((f32::from(narrow.height) - 155.5).abs() < 0.1);

        // An icon keeps its shape and gets a click target, but is not blown up.
        let icon = thumbnail_size(Some((16, 16)), 720.0, 16.0);
        assert_eq!(icon, size(px(40.0), px(40.0)));

        // An unreadable header falls back to a 4:3 slot rather than nothing.
        let unknown = thumbnail_size(None, 720.0, 16.0);
        assert_eq!(unknown, size(px(256.0), px(192.0)));
    }

    #[test]
    fn thumbnail_caps_follow_interface_zoom() {
        let base = thumbnail_size(Some((1432, 928)), 720.0, 16.0);
        let zoomed = thumbnail_size(Some((1432, 928)), 720.0, 24.0);
        assert_eq!(zoomed.height, px(288.0));
        assert_eq!(zoomed.width, px(444.4138));
        assert!(zoomed.width > base.width);
    }

    #[test]
    fn intrinsic_size_reads_the_file_header_without_decoding_pixels() {
        use base64::{Engine as _, engine::general_purpose::STANDARD};

        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("one.png");
        fs::write(&path, STANDARD.decode(ONE_PIXEL_PNG).unwrap()).unwrap();
        assert_eq!(intrinsic_image_size(&path), Some((1, 1)));
        assert_eq!(intrinsic_image_size(&root.path().join("missing.png")), None);
    }

    #[test]
    fn resolves_workspace_relative_and_root_relative_paths() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/demo.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/demo.png"), Some(image.clone()));
        assert_eq!(plugin.resolve("images/demo.png?raw=1"), Some(image.clone()));
        assert_eq!(
            plugin.resolve("images/demo.png#preview"),
            Some(image.clone())
        );
        assert_eq!(plugin.resolve("/images/demo.png"), Some(image.clone()));
        assert_eq!(plugin.resolve("./images/../images/demo.png"), Some(image));
    }

    #[test]
    fn resolves_absolute_and_file_urls_inside_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "shot.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(
            plugin.resolve(image.to_string_lossy().as_ref()),
            Some(image.clone())
        );
        let url = url::Url::from_file_path(&image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), Some(image));
    }

    #[test]
    fn resolves_percent_encoded_names() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/two words.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/two%20words.png"), Some(image));
    }

    #[test]
    fn decodes_percent_escapes_once() {
        let root = tempfile::tempdir().unwrap();
        let image = write_image(root.path(), "images/100%20.png");
        let image = canonical(&image);
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("images/100%2520.png"), Some(image.clone()));
        let url = url::Url::from_file_path(&image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), Some(image));
    }

    #[test]
    fn leaves_embedded_and_remote_images_to_the_built_in_renderer() {
        let root = tempfile::tempdir().unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("data:image/png;base64,iVBORw0KGgo="), None);
        assert_eq!(plugin.resolve("https://example.com/a.png"), None);
        assert_eq!(plugin.resolve("http://example.com/a.png"), None);
    }

    #[test]
    fn rejects_paths_that_escape_the_workspace() {
        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir_in(root.path().parent().unwrap()).unwrap();
        let outside_image = write_image(outside.path(), "secret.png");
        let plugin = plugin(root.path());

        let relative = format!(
            "../{}/secret.png",
            outside.path().file_name().unwrap().to_string_lossy()
        );
        assert_eq!(plugin.resolve(&relative), None);
        assert_eq!(
            plugin.resolve(outside_image.to_string_lossy().as_ref()),
            None
        );
        let url = url::Url::from_file_path(&outside_image).unwrap();
        assert_eq!(plugin.resolve(url.as_str()), None);
    }

    #[test]
    fn rejects_non_images_directories_and_oversized_files() {
        let root = tempfile::tempdir().unwrap();
        fs::write(root.path().join("notes.txt"), b"not an image").unwrap();
        fs::create_dir(root.path().join("picture.png")).unwrap();
        let oversized = root.path().join("huge.png");
        fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(&oversized)
            .unwrap()
            .set_len(MAX_IMAGE_BYTES + 1)
            .unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("notes.txt"), None);
        assert_eq!(plugin.resolve("picture.png"), None);
        assert_eq!(plugin.resolve("huge.png"), None);
    }

    #[cfg(unix)]
    #[test]
    fn rejects_a_symlink_that_escapes_the_workspace() {
        use std::os::unix::fs::symlink;

        let root = tempfile::tempdir().unwrap();
        let outside = tempfile::tempdir().unwrap();
        let outside_image = write_image(outside.path(), "secret.png");
        symlink(&outside_image, root.path().join("link.png")).unwrap();
        let plugin = plugin(root.path());

        assert_eq!(plugin.resolve("link.png"), None);
    }

    const ONE_PIXEL_PNG: &str = "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR4nGP4z8DwHwAFAAH/iZk9HQAAAABJRU5ErkJggg==";

    #[gpui::test]
    fn a_local_image_parses_and_draws_in_a_text_view(cx: &mut gpui::TestAppContext) {
        use std::sync::atomic::{AtomicBool, Ordering};

        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use gpui::{AppContext as _, Context, Entity, IntoElement, Modifiers, Render, Window};

        use crate::markdown::{TextView, TextViewState};

        struct Host {
            state: Entity<TextViewState>,
            extensions: MarkdownExtensions,
        }

        impl Render for Host {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                TextView::new(&self.state)
                    .markdown_extensions(self.extensions.clone())
                    .text_sm()
            }
        }

        cx.update(gpui_component::init);
        let root = tempfile::tempdir().unwrap();
        let bytes = STANDARD.decode(ONE_PIXEL_PNG).unwrap();
        fs::write(root.path().join("shot.png"), bytes).unwrap();
        let clicked = Arc::new(AtomicBool::new(false));
        let clicked_flag = clicked.clone();
        let plugin = SessionImagePlugin::new(
            root.path().to_path_buf(),
            720.0,
            Arc::new(move |_, _, _, _| {
                clicked_flag.store(true, Ordering::SeqCst);
            }),
        );
        let extensions = MarkdownExtensions::default()
            .plugin(plugin)
            .parser_revision(1);
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown("![alt](shot.png)", cx)));
        let state_for_host = state.clone();
        let (host, cx) = cx.add_window_view(move |_, _| Host {
            state: state_for_host,
            extensions,
        });
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
        cx.run_until_parked();
        let rendered = state.read_with(cx, |state, _| state.rendered_text().as_str().to_string());
        assert!(rendered.contains("alt"), "{rendered}");
        let bounds = cx
            .debug_bounds("vibex-session-image")
            .expect("the local image should be claimed by the session plugin");
        // A 1×1 image draws as a bounded, clickable card rather than at its
        // intrinsic size or at whatever the text line happened to offer.
        assert_eq!(bounds.size, size(px(40.0), px(40.0)), "{bounds:?}");
        cx.simulate_click(bounds.center(), Modifiers::none());
        assert!(
            clicked.load(Ordering::SeqCst),
            "clicking the inline image should open its preview",
        );
        assert!(host.read_with(cx, |host, _| host.state.entity_id() == state.entity_id()));
    }

    /// Opening a thumbnail must not arm the transcript's text selection.
    ///
    /// The card answers a click, and its click stops the release from
    /// reaching the window-level selection layer. If the card does not also
    /// claim the press, the layer still anchors a selection at the press — in
    /// the transcript, behind the overlay the click just opened — and nothing
    /// ever ends that gesture. The pointer moves over the preview then extend
    /// it, and the drag auto-scrolls the transcript underneath, which the
    /// preview reads as a wheel and shrinks the picture with.
    #[gpui::test]
    fn opening_a_thumbnail_does_not_arm_the_transcript_selection(cx: &mut gpui::TestAppContext) {
        use base64::{Engine as _, engine::general_purpose::STANDARD};
        use gpui::{
            AppContext as _, Context, Entity, IntoElement, Modifiers, MouseButton, Render, Window,
            point,
        };
        use gpui_base::{Root, TextSelection};

        use crate::markdown::{TextView, TextViewState};
        use crate::selection_guard;

        struct Host {
            state: Entity<TextViewState>,
            extensions: MarkdownExtensions,
        }

        impl Render for Host {
            fn render(
                &mut self,
                _window: &mut Window,
                _cx: &mut Context<Self>,
            ) -> impl IntoElement {
                TextView::new(&self.state)
                    .markdown_extensions(self.extensions.clone())
                    .text_sm()
            }
        }

        cx.update(gpui_component::init);
        cx.update(selection_guard::init);
        let root = tempfile::tempdir().unwrap();
        let bytes = STANDARD.decode(ONE_PIXEL_PNG).unwrap();
        fs::write(root.path().join("shot.png"), bytes).unwrap();
        let plugin =
            SessionImagePlugin::new(root.path().to_path_buf(), 720.0, Arc::new(|_, _, _, _| {}));
        let extensions = MarkdownExtensions::default()
            .plugin(plugin)
            .parser_revision(1);
        let source = "The paragraph beside the figure is the text a \
                      stray selection would take.\n\n![alt](shot.png)\n";
        let state = cx.update(|cx| cx.new(|cx| TextViewState::markdown(source, cx)));
        let state_for_host = state.clone();
        let (_, cx) = cx.add_window_view(move |window, cx| {
            let host = cx.new(|_| Host {
                state: state_for_host,
                extensions,
            });
            Root::new(host, window, cx)
        });
        cx.run_until_parked();
        for _ in 0..4 {
            cx.update(|window, cx| {
                window.refresh();
                let _ = window.draw(cx);
            });
        }
        cx.run_until_parked();
        let card = cx
            .debug_bounds("vibex-session-image")
            .expect("the local image should be claimed by the session plugin")
            .center();

        // Press and release the card the way the preview opens, then travel
        // back over the transcript body with the button up.
        cx.simulate_mouse_down(card, MouseButton::Left, Modifiers::none());
        assert!(
            cx.update(|_, cx| gpui_base::GlobalState::is_text_selection_suppressed(cx)),
            "the card must claim its press so the transcript is not anchored",
        );
        cx.simulate_mouse_up(card, MouseButton::Left, Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        cx.simulate_mouse_move(
            point(card.x - px(80.0), card.y - px(40.0)),
            None,
            Modifiers::none(),
        );
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        let selected = cx.update(TextSelection::selected_text);
        assert_eq!(
            selected, "",
            "opening a thumbnail must not select the transcript behind it",
        );
    }
}

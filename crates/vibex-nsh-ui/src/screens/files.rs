//! A read-only file browser over the workspace.
//!
//! The runtime owns the files; this surface lists one directory at a time
//! (`FileBackend::file_tree`) and shows a file's text (`read_file`) when one is
//! opened. Editing, diffing and Git live elsewhere; this is the phone's first
//! look at the workspace.

use std::rc::Rc;

use nsh_base::{
    button, editor,
    text_view::{MarkdownOptions, text_view},
};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{copy, theme, ui::action};

/// One row of the listing.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub directory: bool,
}

/// An opened file.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePreview {
    pub title: String,
    pub body: String,
    pub truncated: bool,
    /// The language the runtime reported, for highlighting.
    pub language: String,
    /// The face the runtime says the file wants.
    pub kind: FilePreviewKindView,
    /// A `data:` URL for an image, when one was read.
    pub image_uri: Option<String>,
}

/// How a file's preview is drawn.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePreviewKindView {
    Text,
    Markdown,
    Image,
    Binary,
}

/// What the browser may do, and the state it shows.
#[derive(Clone)]
pub struct FilesHost {
    pub open: Signal<bool>,
    /// The directory being listed ("" is the workspace root).
    pub path: Signal<String>,
    pub entries: Signal<Vec<FileEntry>>,
    /// The file being read, when one is open.
    pub file: Signal<Option<FilePreview>>,
    pub error: Signal<Option<SharedString>>,
    /// The open file's text, as edited.
    pub content: Signal<String>,
    /// Whether the text differs from what was read.
    pub dirty: Signal<bool>,
    pub saving: Signal<bool>,
    pub on_save: Rc<dyn Fn()>,
    pub on_open_dir: Rc<dyn Fn(&str)>,
    pub on_open_file: Rc<dyn Fn(&str)>,
    pub on_back: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The files surface.
pub fn files(host: FilesHost) -> impl IntoElement {
    let title = {
        let path = host.path;
        let file = host.file;
        move || {
            if let Some(file) = file.get() {
                return file.title;
            }
            let path = path.get();
            if path.is_empty() {
                copy::text("Files", "文件", "檔案").to_owned()
            } else {
                path
            }
        }
    };
    let on_back = host.on_back.clone();
    let on_close = host.on_close.clone();
    let entries = host.entries;
    let file = host.file;
    let error = host.error;
    let dirty = host.dirty;
    let saving = host.saving;
    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .pb_safe_area()
        .child(
            view()
                .w_full()
                .pt_safe_area()
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    view()
                        .w_full()
                        .h(px(48.0))
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_XS))
                        .px(px(theme::SPACING_SM))
                        .child({
                            let on_back = on_back.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/chevron-left.svg")),
                                copy::text("Back", "返回", "返回"),
                                move || on_back(),
                            )
                        })
                        .child(
                            text(title)
                                .flex_1()
                                .truncate()
                                .text_size(px(theme::font_heading()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                        )
                        .child(when(
                            move || {
                                file.get().is_some_and(|file| {
                                    matches!(
                                        file.kind,
                                        FilePreviewKindView::Text | FilePreviewKindView::Markdown
                                    )
                                })
                            },
                            {
                                let on_save = host.on_save.clone();
                                move || {
                                    let on_save = on_save.clone();
                                    action::icon_button(
                                        icon(include_str!("../../assets/icons/check.svg")),
                                        copy::text("Save", "保存", "儲存"),
                                        move || on_save(),
                                    )
                                    .disabled(move || !dirty.get() || saving.get())
                                    .disabled_style(|style| style.opacity(0.35))
                                }
                            },
                        ))
                        .child({
                            let on_close = on_close.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/x.svg")),
                                copy::text("Close", "关闭", "關閉"),
                                move || on_close(),
                            )
                        }),
                ),
        )
        .child(when(
            move || error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::accent_red)
            },
        ))
        .child(when(
            move || file.get().is_some(),
            move || file_preview(file, host.content, dirty),
        ))
        .child(when(
            move || file.get().is_none(),
            move || file_list(entries, host.on_open_dir.clone(), host.on_open_file.clone()),
        ))
}

fn file_list(
    entries: Signal<Vec<FileEntry>>,
    on_open_dir: Rc<dyn Fn(&str)>,
    on_open_file: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view().flex_1().min_h(px(0.0)).child(
        virtual_list(
            move || entries.with(|entries| entries.len()),
            move |at: Position| file_row(at, entries, on_open_dir.clone(), on_open_file.clone()),
        )
        .keys(move || {
            entries.with(|entries| {
                entries
                    .iter()
                    .map(|entry| entry.path.clone())
                    .collect::<Vec<_>>()
            })
        })
        .estimated_item_size(px(44.0))
        .flex_1(),
    )
}

fn file_row(
    at: Position,
    entries: Signal<Vec<FileEntry>>,
    on_open_dir: Rc<dyn Fn(&str)>,
    on_open_file: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let entry = computed(move || entries.with(|entries| entries.get(at.index()).cloned()));
    button()
        .w_full()
        .min_h(px(44.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_LG))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .active(|style| style.opacity(0.8))
        .on_press(move || {
            let Some(entry) = entry.get_untracked() else {
                return;
            };
            if entry.directory {
                on_open_dir(&entry.path);
            } else {
                on_open_file(&entry.path);
            }
        })
        .child(
            svg(icon(
                if entry.with(|entry| entry.as_ref().is_some_and(|entry| entry.directory)) {
                    include_str!("../../assets/icons/folder.svg")
                } else {
                    include_str!("../../assets/icons/file-text.svg")
                },
            ))
            .w(px(theme::ICON_SM))
            .h(px(theme::ICON_SM))
            .flex_shrink_0()
            .text_color(theme::text_muted),
        )
        .child(
            text(move || {
                entry.with(|entry| {
                    entry
                        .as_ref()
                        .map(|entry| entry.name.clone())
                        .unwrap_or_default()
                })
            })
            .flex_1()
            .truncate()
            .text_size(px(theme::font_body()))
            .text_color(theme::foreground),
        )
}

fn file_preview(
    file: Signal<Option<FilePreview>>,
    content: Signal<String>,
    dirty: Signal<bool>,
) -> impl IntoElement {
    let truncated =
        computed(move || file.with(|file| file.as_ref().is_some_and(|file| file.truncated)));
    view()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(when(
            move || {
                file.with(|file| {
                    file.as_ref()
                        .is_some_and(|file| file.kind == FilePreviewKindView::Markdown)
                })
            },
            move || {
                scroll().flex_1().child(
                    view().p(px(theme::SPACING_MD)).child(
                        text_view()
                            .markdown(move || Some(content.get().into()))
                            .options(MarkdownOptions::default)
                            .selectable(true),
                    ),
                )
            },
        ))
        .child(when(
            move || {
                file.with(|file| {
                    file.as_ref()
                        .is_some_and(|file| file.kind == FilePreviewKindView::Image)
                })
            },
            move || {
                view().flex_1().items_center().justify_center().child(
                    image(move || {
                        ImageSrc::from(file.with(|file| {
                            file.as_ref()
                                .and_then(|file| file.image_uri.clone())
                                .unwrap_or_default()
                        }))
                    })
                    .w_full()
                    .object_fit(nsh_core::ResizeMode::Contain),
                )
            },
        ))
        .child(when(
            move || {
                file.with(|file| {
                    file.as_ref().is_some_and(|file| {
                        matches!(
                            file.kind,
                            FilePreviewKindView::Text | FilePreviewKindView::Binary
                        )
                    })
                })
            },
            move || {
                editor()
                    .value(move || Some(content.get().into()))
                    .language(move || {
                        file.with(|file| {
                            file.as_ref()
                                .map(|file| SharedString::from(file.language.clone()))
                                .filter(|language| !language.is_empty())
                        })
                    })
                    .test_id("file-content")
                    .read_only(move || {
                        file.with(|file| {
                            file.as_ref()
                                .is_some_and(|file| file.kind == FilePreviewKindView::Binary)
                        })
                    })
                    .on_change(move |value: &SharedString| {
                        content.set(value.to_string());
                        dirty.set(true);
                    })
                    .flex_1()
                    .min_h(px(240.0))
            },
        ))
        .child(when(
            move || truncated.get(),
            || {
                text(copy::text(
                    "The file is larger than the preview limit.",
                    "文件超过预览上限。",
                    "檔案超過預覽上限。",
                ))
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted)
            },
        ))
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

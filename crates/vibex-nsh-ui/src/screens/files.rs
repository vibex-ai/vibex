//! A read-only file browser over the workspace.
//!
//! The runtime owns the files; this surface lists one directory at a time
//! (`FileBackend::file_tree`) and shows a file's text (`read_file`) when one is
//! opened. Editing, diffing and Git live elsewhere; this is the phone's first
//! look at the workspace.

use std::rc::Rc;

use nsh_base::button;
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
                                .text_size(px(theme::FONT_HEADING))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                        )
                        .child(when(move || file.get().is_some(), {
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
                        }))
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
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::accent_red)
            },
        ))
        .child(when(
            move || file.get().is_some(),
            move || file_preview(file),
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
            .text_size(px(theme::FONT_BODY))
            .text_color(theme::foreground),
        )
}

fn file_preview(file: Signal<Option<FilePreview>>) -> impl IntoElement {
    let body = computed(move || {
        file.with(|file| {
            file.as_ref()
                .map(|file| file.body.clone())
                .unwrap_or_default()
        })
    });
    let truncated =
        computed(move || file.with(|file| file.as_ref().is_some_and(|file| file.truncated)));
    view()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_CAPTION))
                .line_height(theme::line_height_for(theme::FONT_CAPTION))
                .text_color(theme::foreground),
        )
        .child(when(
            move || truncated.get(),
            || {
                text(copy::text(
                    "The file is larger than the preview limit.",
                    "文件超过预览上限。",
                    "檔案超過預覽上限。",
                ))
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted)
            },
        ))
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

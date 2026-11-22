//! The file tree and preview for a remote workspace.
//!
//! The host projects the tree and owns its reads and revision-checked writes.

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
    pub depth: usize,
    pub expanded: bool,
    pub loading: bool,
    pub git: Option<String>,
}

/// One file content search result.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearchRow {
    pub path: String,
    pub name: String,
    pub line: Option<u32>,
    pub snippet: String,
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
    pub entries: Signal<Vec<FileEntry>>,
    pub search: Signal<Vec<FileSearchRow>>,
    pub on_search: FileSearch,
    pub on_refresh: Rc<dyn Fn()>,
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
}

pub type FileSearch = Rc<dyn Fn(&str, bool)>;

/// The files surface.
pub fn files(host: FilesHost) -> impl IntoElement {
    let on_back = host.on_back.clone();
    let entries = host.entries;
    let search = host.search;
    let search_query = signal(String::new());
    let search_contents = signal(false);
    let has_query = move || search_query.with(|query: &String| !query.trim().is_empty());
    let on_search = host.on_search.clone();
    let on_open_file = host.on_open_file.clone();
    let file = host.file;
    let error = host.error;
    let dirty = host.dirty;
    let saving = host.saving;
    view()
        .flex_1()
        .bg(theme::sidebar_bg)
        .flex_col()
        .pb_safe_area()
        .child(when(
            move || file.with(Option::is_some),
            move || {
                view()
                    .w_full()
                    .border_b_1()
                    .border_b_color(theme::border_subtle)
                    .child(
                        view()
                            .w_full()
                            .h(px(theme::TOUCH_TARGET))
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .px(px(theme::SPACING_SM))
                            .child({
                                let on_back = on_back.clone();
                                action::icon_button(
                                    icon!("chevron-left"),
                                    copy::text("Back", "返回", "返回"),
                                    move || on_back(),
                                )
                            })
                            .child(
                                text(move || {
                                    file.with(|file| {
                                        file.as_ref()
                                            .map_or_else(String::new, |file| file.title.clone())
                                    })
                                })
                                .flex_1()
                                .truncate()
                                .text_size(px(theme::font_body()))
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_color(theme::foreground),
                            )
                            .child(when(
                                move || {
                                    file.get().is_some_and(|file| {
                                        matches!(
                                            file.kind,
                                            FilePreviewKindView::Text
                                                | FilePreviewKindView::Markdown
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
                            )),
                    )
            },
        ))
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
        .child(when(move || file.with(Option::is_none), {
            let on_search = on_search.clone();
            let on_open_file = on_open_file.clone();
            let on_open_dir = host.on_open_dir.clone();
            let on_refresh = host.on_refresh.clone();
            move || {
                view()
                    .flex_1()
                    .min_h(px(0.0))
                    .flex_col()
                    .child(
                        view()
                            .h(px(theme::TOUCH_TARGET))
                            .flex_shrink_0()
                            .px(px(theme::SPACING_XS))
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .border_b_1()
                            .border_b_color(theme::border_subtle)
                            .child(
                                crate::ui::field::text_field_flat_with(
                                    search_query,
                                    copy::text("Search files", "搜索文件", "搜尋檔案"),
                                    {
                                        let on_search = on_search.clone();
                                        move |text| on_search(text, search_contents.get_untracked())
                                    },
                                )
                                .flex_1()
                                .min_w(px(0.0)),
                            )
                            .child(when(has_query, {
                                let on_search = on_search.clone();
                                move || {
                                    let on_search = on_search.clone();
                                    action::icon_button(
                                        icon!("x"),
                                        copy::text("Clear search", "清除搜索", "清除搜尋"),
                                        move || {
                                            search_query.set(String::new());
                                            on_search("", search_contents.get_untracked());
                                        },
                                    )
                                    .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                                    .flex_shrink_0()
                                }
                            }))
                            .child(
                                button()
                                    .h(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                                    .px(px(theme::SPACING_SM))
                                    .flex_shrink_0()
                                    .items_center()
                                    .justify_center()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_press({
                                        let on_search = on_search.clone();
                                        move || {
                                            let next = !search_contents.get_untracked();
                                            search_contents.set(next);
                                            on_search(&search_query.get_untracked(), next);
                                        }
                                    })
                                    .child(
                                        text(move || {
                                            if search_contents.get() {
                                                copy::text("Content", "内容", "內容")
                                            } else {
                                                copy::text("Name", "名称", "名稱")
                                            }
                                        })
                                        .text_size(px(theme::font_caption()))
                                        .text_color(theme::text_secondary),
                                    ),
                            )
                            .child({
                                let on_refresh = on_refresh.clone();
                                action::icon_button(
                                    icon!("refresh"),
                                    copy::text("Refresh files", "刷新文件", "重新整理檔案"),
                                    move || on_refresh(),
                                )
                                .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                                .flex_shrink_0()
                            }),
                    )
                    .child(
                        view()
                            .flex_1()
                            .min_h(px(0.0))
                            .style_when(has_query, |style| style.hidden())
                            .child(file_list(
                                entries,
                                on_open_dir.clone(),
                                on_open_file.clone(),
                            )),
                    )
                    .child(when(has_query, {
                        let on_open_file = on_open_file.clone();
                        move || file_search_list(search, on_open_file.clone())
                    }))
            }
        }))
}

fn file_search_list(
    rows: Signal<Vec<FileSearchRow>>,
    on_open_file: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    scroll()
        .flex_1()
        .child(view().w_full().flex_col().child(each(
            move || rows.get(),
            |row| (row.path.clone(), row.line),
            move |row, _| {
                let on_open_file = on_open_file.clone();
                button()
                    .on_press(move || on_open_file(&row.get_untracked().path))
                    .w_full()
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_XS))
                    .flex_col()
                    .items_start()
                    .border_b_1()
                    .border_b_color(theme::border_subtle)
                    .child(
                        text(move || {
                            let row = row.get();
                            match row.line {
                                Some(line) => format!("{}:{line}", row.name),
                                None => row.name,
                            }
                        })
                        .text_size(px(theme::font_body()))
                        .text_color(theme::foreground),
                    )
                    .child(
                        text(move || row.get().snippet)
                            .truncate()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                    )
            },
        )))
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
        .estimated_item_size(px(theme::TOUCH_TARGET))
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
        .min_h(px(theme::TOUCH_TARGET))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_SM))
        .active(|style| style.bg(theme::row_pressed_bg()))
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
        .child(dynamic(move || {
            let row = entry.get();
            let indent = row.as_ref().map_or(0, |row| row.depth.min(12)) as f32 * theme::SPACING_SM;
            let glyph = match row {
                Some(row) if row.loading => icon!("loader-circle"),
                Some(row) if row.directory && row.expanded => icon!("folder-open"),
                Some(row) if row.directory => icon!("folder"),
                _ => icon!("file-text"),
            };
            svg(glyph)
                .size(px(theme::ICON_SM))
                .ml(px(indent))
                .flex_shrink_0()
                .text_color(theme::text_muted)
        }))
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
        .child(
            text(move || {
                entry.with(|row| {
                    row.as_ref()
                        .and_then(|row| row.git.clone())
                        .unwrap_or_default()
                })
            })
            .text_size(px(theme::font_caption()))
            .text_color(theme::text_muted),
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

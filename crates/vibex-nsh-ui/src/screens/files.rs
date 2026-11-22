//! The remote workspace's file tree, search, and read-only viewer.

use std::rc::Rc;

use nsh_base::{
    button, editor,
    editor::{Editor, EditorApi, EditorCommand, EditorSearch, EditorSearchResult},
    text_view::{MarkdownOptions, text_view},
};
use nsh_core::{
    element::{ImageSrc, Position},
    prelude::*,
};

use crate::{
    copy, theme,
    ui::{action, field},
};

const TREE_ROW_HEIGHT: f32 = 38.0;
const TREE_INDENT: f32 = 20.0;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileEntry {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub depth: usize,
    pub expanded: bool,
    pub loading: bool,
    pub git: Option<String>,
    pub icon: FileGlyph,
    pub ignored: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum FileGlyph {
    #[default]
    Text,
    Code,
    Markdown,
    Script,
    Database,
    Style,
    Image,
    Data,
    Archive,
}

impl FileGlyph {
    fn icon(self) -> ImageSrc {
        match self {
            Self::Text => icon!("file-text"),
            Self::Code => icon!("file-code"),
            Self::Markdown => icon!("book-open-text"),
            Self::Script => icon!("file-terminal"),
            Self::Database => icon!("database"),
            Self::Style => icon!("hash"),
            Self::Image => icon!("image"),
            Self::Data => icon!("braces"),
            Self::Archive => icon!("package"),
        }
    }

    fn color(self) -> Hsla {
        match self {
            Self::Code | Self::Script => theme::accent_blue(),
            Self::Markdown | Self::Image => theme::accent_purple(),
            Self::Database | Self::Data => theme::accent_yellow(),
            _ => theme::text_muted(),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FileSearchRow {
    pub path: String,
    pub name: String,
    pub line: Option<u32>,
    pub snippet: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FilePreview {
    pub title: String,
    pub body: String,
    pub truncated: bool,
    pub language: String,
    pub kind: FilePreviewKindView,
    pub image_uri: Option<String>,
    pub line: Option<u32>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FilePreviewKindView {
    Text,
    Markdown,
    Image,
    Binary,
}

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchOptions {
    pub contents: bool,
    pub case_sensitive: bool,
    pub whole_word: bool,
    pub regex: bool,
}

pub type FileSearch = Rc<dyn Fn(&str, SearchOptions)>;
pub type OpenFile = Rc<dyn Fn(&str, Option<u32>)>;

#[derive(Clone)]
pub struct FilesHost {
    pub entries: Signal<Vec<FileEntry>>,
    pub search: Signal<Vec<FileSearchRow>>,
    pub searching: Signal<bool>,
    pub path: Signal<Option<String>>,
    pub file: Signal<Option<FilePreview>>,
    pub error: Signal<Option<SharedString>>,
    pub on_search: FileSearch,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_collapse: Rc<dyn Fn()>,
    pub on_open_dir: Rc<dyn Fn(&str)>,
    pub on_open_file: OpenFile,
    pub on_back: Rc<dyn Fn()>,
}

pub fn files(host: FilesHost) -> impl IntoElement {
    let path = host.path;
    let preview = host.clone();
    view()
        .flex_1()
        .min_h(px(0.0))
        .bg(theme::sidebar_bg)
        .pb_safe_area()
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .style_when(move || path.with(Option::is_some), |style| style.hidden())
                .child(file_browser(host)),
        )
        .child(when(
            move || path.with(Option::is_some),
            move || file_viewer(preview.clone()),
        ))
}

fn file_browser(host: FilesHost) -> impl IntoElement {
    let query = signal(String::new());
    let options = signal(SearchOptions::default());
    let has_query = move || query.with(|query| !query.trim().is_empty());
    let search = host.on_search;
    let run_search: Rc<dyn Fn()> = Rc::new({
        let search = search.clone();
        move || search(&query.get_untracked(), options.get_untracked())
    });
    let error = host.error;
    let searching = host.searching;
    let results = host.search;
    let entries = host.entries;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .px(px(theme::SPACING_XS))
                .gap(px(theme::SPACING_XS))
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    field::text_field_flat_with(
                        query,
                        copy::text("Search files", "搜索文件", "搜尋檔案"),
                        {
                            let search = search.clone();
                            move |value| search(value, options.get_untracked())
                        },
                    )
                    .flex_1()
                    .min_w(px(0.0))
                    .px(px(theme::SPACING_SM)),
                )
                .child(when(has_query, {
                    let run_search = run_search.clone();
                    move || {
                        let run_search = run_search.clone();
                        rail_button(
                            icon!("x"),
                            copy::text("Clear search", "清除搜索", "清除搜尋"),
                            move || {
                                query.set(String::new());
                                run_search();
                            },
                        )
                    }
                }))
                .child(
                    button()
                        .h(px(36.0))
                        .px(px(theme::SPACING_XS))
                        .justify_center()
                        .flex_shrink_0()
                        .on_press({
                            let run_search = run_search.clone();
                            move || {
                                options.update(|options| options.contents = !options.contents);
                                run_search();
                            }
                        })
                        .child(
                            text(move || {
                                if options.get().contents {
                                    copy::text("Content", "内容", "內容")
                                } else {
                                    copy::text("Name", "名称", "名稱")
                                }
                            })
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::text_secondary),
                        ),
                )
                .child(rail_button(
                    icon!("refresh"),
                    copy::text("Refresh files", "刷新文件", "重新整理檔案"),
                    move || (host.on_refresh)(),
                ))
                .child(rail_button(
                    icon!("chevrons-down-up"),
                    copy::text("Collapse folders", "折叠文件夹", "摺疊資料夾"),
                    move || (host.on_collapse)(),
                )),
        )
        .child(when(
            move || options.get().contents,
            move || {
                view()
                    .flex_row()
                    .gap(px(theme::SPACING_XS))
                    .px(px(theme::SPACING_SM))
                    .child(search_toggle(
                        "Aa",
                        copy::text("Case sensitive", "区分大小写", "區分大小寫"),
                        options,
                        |options| &mut options.case_sensitive,
                        run_search.clone(),
                    ))
                    .child(search_toggle(
                        copy::text("Word", "整词", "整詞"),
                        copy::text("Whole word", "匹配整词", "符合整詞"),
                        options,
                        |options| &mut options.whole_word,
                        run_search.clone(),
                    ))
                    .child(search_toggle(
                        ".*",
                        copy::text("Regex", "正则表达式", "規則運算式"),
                        options,
                        |options| &mut options.regex,
                        run_search.clone(),
                    ))
            },
        ))
        .child(when(
            move || error.with(Option::is_some) && !searching.get(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_MD))
                    .py(px(theme::SPACING_SM))
                    .text_color(theme::accent_red)
                    .text_size(px(theme::font_caption()))
            },
        ))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .style_when(has_query, |style| style.hidden())
                .child({
                    let open = host.on_open_file.clone();
                    virtual_list(
                        move || entries.with(Vec::len),
                        move |at: Position| {
                            let entry = computed(move || {
                                entries.with(|entries| entries.get(at.index()).cloned())
                            });
                            let open = open.clone();
                            let dir = host.on_open_dir.clone();
                            dynamic(move || match entry.get() {
                                Some(row) => {
                                    file_row(row, open.clone(), dir.clone()).into_element()
                                }
                                None => view().into_element(),
                            })
                        },
                    )
                    .keys(move || {
                        entries.with(|entries| {
                            entries
                                .iter()
                                .map(|entry| entry.path.clone())
                                .collect::<Vec<_>>()
                        })
                    })
                    .estimated_item_size(px(TREE_ROW_HEIGHT))
                    .flex_1()
                }),
        )
        .child(when(has_query, move || {
            let open = host.on_open_file.clone();
            when(
                move || searching.get(),
                || super::session::empty_hint(copy::text("Searching…", "正在搜索…", "正在搜尋…")),
            )
            .otherwise(move || {
                let open = open.clone();
                when(
                    move || error.with(Option::is_none) && results.with(Vec::is_empty),
                    || {
                        super::session::empty_hint(copy::text(
                            "No matching items",
                            "没有匹配项",
                            "沒有符合項目",
                        ))
                    },
                )
                .otherwise(move || file_search_list(results, open.clone()))
            })
        }))
}

fn search_toggle(
    title: &'static str,
    label: &'static str,
    options: Signal<SearchOptions>,
    field: fn(&mut SearchOptions) -> &mut bool,
    submit: Rc<dyn Fn()>,
) -> impl IntoElement {
    button()
        .h(px(32.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .accessibility_label(label)
        .style_when(
            move || *field(&mut options.get()),
            |style| style.bg(theme::sidebar_selected_bg()),
        )
        .on_press(move || {
            options.update(|options| {
                let value = field(options);
                *value = !*value;
            });
            submit();
        })
        .child(
            text(title)
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
}

fn file_search_list(rows: Signal<Vec<FileSearchRow>>, open: OpenFile) -> impl IntoElement {
    scroll()
        .flex_1()
        .min_h(px(0.0))
        .child(view().w_full().child(each(
            move || rows.get(),
            |row| (row.path.clone(), row.line),
            move |row, _| {
                let open = open.clone();
                button()
                    .h(px(theme::TOUCH_TARGET))
                    .mx(px(theme::SPACING_XS))
                    .px(px(theme::SPACING_SM))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .active(|style| style.bg(theme::row_pressed_bg()))
                    .on_press(move || {
                        let row = row.get_untracked();
                        dismiss_keyboard();
                        open(&row.path, row.line);
                    })
                    .child(
                        svg(icon!("file-text"))
                            .size(px(theme::ICON_SM))
                            .flex_shrink_0()
                            .text_color(theme::text_muted),
                    )
                    .child(
                        view()
                            .flex_1()
                            .min_w(px(0.0))
                            .child(
                                text(move || {
                                    row.with(|row| match row.line {
                                        Some(line) => format!("{}:{line}", row.name),
                                        None => row.name.clone(),
                                    })
                                })
                                .truncate()
                                .text_size(px(theme::font_body()))
                                .text_color(theme::foreground),
                            )
                            .child(
                                text(move || row.with(|row| row.path.clone()))
                                    .truncate()
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted),
                            ),
                    )
                    .child(
                        text(move || row.with(|row| row.snippet.clone()))
                            .max_w(px(110.0))
                            .truncate()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                    )
            },
        )))
}

fn file_row(row: FileEntry, open: OpenFile, toggle: Rc<dyn Fn(&str)>) -> impl IntoElement {
    let directory = row.directory;
    let depth = row.depth.min(10);
    let (icon, color) = if directory {
        (
            if row.expanded {
                icon!("folder-open")
            } else {
                icon!("folder")
            },
            theme::text_secondary(),
        )
    } else {
        (row.icon.icon(), row.icon.color())
    };
    let path = row.path;
    let status = row.git.unwrap_or_default();
    let status_color = match status.as_str() {
        "A" | "?" => theme::accent_success(),
        "D" => theme::accent_red(),
        "M" | "R" => theme::accent_yellow(),
        _ => theme::text_muted(),
    };
    button()
        .min_h(px(TREE_ROW_HEIGHT))
        .relative()
        .mx(px(theme::SPACING_XS))
        .px(px(theme::SPACING_XS))
        .rounded(px(theme::RADIUS_CONTROL))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_press(move || {
            if directory {
                toggle(&path);
            } else {
                open(&path, None);
            }
        })
        .children((0..depth).map(|level| {
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(theme::SPACING_MD + level as f32 * TREE_INDENT))
                .w(px(1.0))
                .bg(theme::sidebar_tree_guide)
                .pointer_events(PointerEvents::None)
        }))
        .child(view().w(px(depth as f32 * TREE_INDENT)).flex_shrink_0())
        .child(
            view()
                .w(px(16.0))
                .flex_shrink_0()
                .items_center()
                .child(when(
                    move || directory,
                    move || {
                        svg(if row.expanded {
                            icon!("chevron-down")
                        } else {
                            icon!("chevron-right")
                        })
                        .size(px(12.0))
                        .text_color(theme::text_muted)
                    },
                )),
        )
        .child(
            svg(icon)
                .size(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(color)
                .opacity(if row.ignored { 0.55 } else { 1.0 }),
        )
        .child(
            text(row.name)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_body()))
                .text_color(if row.ignored {
                    theme::text_muted
                } else {
                    theme::foreground
                }),
        )
        .children(row.loading.then(|| {
            svg(icon!("loader-circle"))
                .size(px(14.0))
                .flex_shrink_0()
                .text_color(theme::text_muted)
        }))
        .child(
            text(status)
                .w(px(16.0))
                .flex_shrink_0()
                .text_size(px(theme::font_micro()))
                .text_color(status_color),
        )
}

fn file_viewer(host: FilesHost) -> impl IntoElement {
    let path = host.path;
    let file = host.file;
    let error = host.error;
    let reader = component_ref::<Editor>();
    let find = signal(false);
    let jump = signal(false);
    let query = signal(String::new());
    let line = signal(String::new());
    let matches = signal(EditorSearchResult::default());
    let initial = stored(None::<(String, u32)>);
    let can_find = move || {
        file.with(|file| {
            file.as_ref().is_some_and(|file| {
                matches!(
                    file.kind,
                    FilePreviewKindView::Text | FilePreviewKindView::Markdown
                )
            })
        })
    };
    let on_jump = move || {
        let Some(api) = reader.api::<EditorApi>() else {
            return;
        };
        if let Ok(line) = line.get_untracked().trim().parse::<u32>() {
            let last = api.value().lines().count().saturating_sub(1) as u32;
            api.go_to_line(line.saturating_sub(1).min(last));
            dismiss_keyboard();
        }
    };
    view()
        .flex_1()
        .min_h(px(0.0))
        .pb_keyboard()
        .child(
            view()
                .h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(rail_button(
                    icon!("chevron-left"),
                    copy::text("Back", "返回", "返回"),
                    move || {
                        dismiss_keyboard();
                        (host.on_back)();
                    },
                ))
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(
                            text(move || {
                                path.with(|path| {
                                    path.as_deref()
                                        .unwrap_or_default()
                                        .rsplit('/')
                                        .next()
                                        .unwrap_or_default()
                                        .to_owned()
                                })
                            })
                            .truncate()
                            .text_size(px(theme::font_body()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        )
                        .child(
                            text(move || {
                                path.with(|path| {
                                    path.as_deref()
                                        .and_then(|path| {
                                            path.rsplit_once('/')
                                                .map(|(parent, _)| parent.to_owned())
                                        })
                                        .unwrap_or_default()
                                })
                            })
                            .truncate()
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::text_muted),
                        ),
                )
                .child(when(can_find, move || {
                    view()
                        .flex_row()
                        .flex_shrink_0()
                        .child(rail_button(
                            icon!("search"),
                            copy::text("Find in file", "在文件中查找", "在檔案中尋找"),
                            move || {
                                jump.set(false);
                                find.update(|value| *value = !*value);
                            },
                        ))
                        .child(rail_button(
                            icon!("hash"),
                            copy::text("Go to line", "转到行", "前往行"),
                            move || {
                                find.set(false);
                                jump.update(|value| *value = !*value);
                            },
                        ))
                })),
        )
        .child(when(
            move || find.get(),
            move || {
                view()
                    .h(px(theme::TOUCH_TARGET))
                    .flex_row()
                    .items_center()
                    .px(px(theme::SPACING_XS))
                    .child(
                        field::text_field_flat(
                            query,
                            copy::text("Find in file", "在文件中查找", "在檔案中尋找"),
                        )
                        .flex_1()
                        .min_w(px(0.0)),
                    )
                    .child(
                        text(move || {
                            matches.with(|result| {
                                format!("{}/{}", result.current.map_or(0, |i| i + 1), result.count)
                            })
                        })
                        .flex_shrink_0()
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                    )
                    .child(find_button(
                        reader,
                        EditorCommand::FindPrevious,
                        icon!("chevron-up"),
                        copy::text("Previous match", "上一个匹配", "上一個符合"),
                    ))
                    .child(find_button(
                        reader,
                        EditorCommand::FindNext,
                        icon!("chevron-down"),
                        copy::text("Next match", "下一个匹配", "下一個符合"),
                    ))
                    .child(rail_button(
                        icon!("x"),
                        copy::text("Close search", "关闭搜索", "關閉搜尋"),
                        move || {
                            find.set(false);
                            query.set(String::new());
                            dismiss_keyboard();
                        },
                    ))
            },
        ))
        .child(when(
            move || jump.get(),
            move || {
                view()
                    .h(px(theme::TOUCH_TARGET))
                    .flex_row()
                    .items_center()
                    .px(px(theme::SPACING_XS))
                    .child(
                        field::text_field_flat(line, copy::text("Line number", "行号", "行號"))
                            .flex_1()
                            .min_w(px(0.0))
                            .on_submit(move |_: &str| on_jump()),
                    )
                    .child(rail_button(
                        icon!("arrow-to-top"),
                        copy::text("Go", "跳转", "前往"),
                        on_jump,
                    ))
                    .child(rail_button(
                        icon!("x"),
                        copy::text("Close go to line", "关闭行定位", "關閉行定位"),
                        move || {
                            jump.set(false);
                            dismiss_keyboard();
                        },
                    ))
            },
        ))
        .child(dynamic(move || {
            let Some(preview) = file.get() else {
                return view()
                    .flex_1()
                    .items_center()
                    .justify_center()
                    .px(px(theme::SPACING_MD))
                    .child(
                        text(move || {
                            error.get().unwrap_or_else(|| {
                                copy::text("Loading file…", "正在加载文件…", "正在載入檔案…").into()
                            })
                        })
                        .text_size(px(theme::font_body()))
                        .text_color(theme::text_muted),
                    )
                    .into_element();
            };
            let force_source = find.get() || jump.get() || preview.line.is_some();
            let body = match preview.kind {
                FilePreviewKindView::Markdown if !force_source => scroll()
                    .flex_1()
                    .child(
                        view().p(px(theme::SPACING_MD)).child(
                            text_view()
                                .markdown(Some(SharedString::from(preview.body)))
                                .options(MarkdownOptions::default)
                                .selectable(true),
                        ),
                    )
                    .into_element(),
                FilePreviewKindView::Image => view()
                    .flex_1()
                    .min_h(px(0.0))
                    .justify_center()
                    .items_center()
                    .child(
                        image(ImageSrc::from(preview.image_uri.unwrap_or_default()))
                            .w_full()
                            .h_full()
                            .object_fit(nsh_core::ResizeMode::Contain),
                    )
                    .into_element(),
                FilePreviewKindView::Binary => super::session::empty_hint(copy::text(
                    "No preview for this file",
                    "无法预览此文件",
                    "無法預覽此檔案",
                ))
                .into_element(),
                _ => {
                    let location = path.get_untracked().zip(preview.line);
                    editor()
                        .component_ref(&reader)
                        .value(Some(SharedString::from(preview.body)))
                        .language(Some(SharedString::from(preview.language)))
                        .read_only(true)
                        .search(move || find.get().then(|| EditorSearch::new(query.get())))
                        .on_search_change(move |result: &EditorSearchResult| {
                            matches.set(result.clone())
                        })
                        .on_layout(move |_: &Bounds<Points>| {
                            if initial.get() != location
                                && let Some((_, line)) = location.as_ref()
                                && let Some(api) = reader.api::<EditorApi>()
                            {
                                initial.replace(location.clone());
                                api.go_to_line(line.saturating_sub(1));
                            }
                        })
                        .test_id("file-content")
                        .flex_1()
                        .min_h(px(0.0))
                        .into_element()
                }
            };
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(body)
                .children(preview.truncated.then(|| {
                    text(copy::text(
                        "The file is larger than the preview limit.",
                        "文件超过预览上限。",
                        "檔案超過預覽上限。",
                    ))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
                }))
                .into_element()
        }))
}

fn find_button(
    reader: ComponentRef<Editor>,
    command: EditorCommand,
    icon: ImageSrc,
    label: &'static str,
) -> impl IntoElement {
    rail_button(icon, label, move || {
        if let Some(api) = reader.api::<EditorApi>() {
            api.run(command);
        }
    })
}

fn rail_button(
    icon: ImageSrc,
    label: &'static str,
    action: impl Fn() + 'static,
) -> nsh_base::ButtonElement {
    action::icon_button(icon, label, action)
        .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
        .flex_shrink_0()
}

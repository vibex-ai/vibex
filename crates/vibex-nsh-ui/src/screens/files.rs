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
    ui::{field, surface},
};

/// A row of the tree.
const TREE_ROW_HEIGHT: f32 = 40.0;
/// How far a level of the tree steps in.
const TREE_INDENT: f32 = 18.0;

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
    // What a search reads: a file's name, or what is written in it.
    let scope = |contents: bool, run_search: Rc<dyn Fn()>| {
        move || {
            if options.get_untracked().contents != contents {
                options.update(|options| options.contents = contents);
                run_search();
            }
        }
    };
    view()
        .flex_1()
        .min_h(px(0.0))
        // The search field, and the two things done to the tree as a whole.
        .child(
            view()
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .child(
                    surface::field_pill()
                        .flex_1()
                        .min_w(px(0.0))
                        .child(surface::field_glyph(icon!("search")))
                        .child(
                            field::text_field_bare_with(
                                query,
                                copy::text("Search files", "搜索文件", "搜尋檔案"),
                                {
                                    let search = search.clone();
                                    move |value| search(value, options.get_untracked())
                                },
                            )
                            .flex_1()
                            .min_w(px(0.0))
                            .h_full(),
                        )
                        .child(when(has_query, {
                            let run_search = run_search.clone();
                            move || {
                                let run_search = run_search.clone();
                                surface::field_clear(
                                    copy::text("Clear search", "清除搜索", "清除搜尋"),
                                    move || {
                                        query.set(String::new());
                                        dismiss_keyboard();
                                        run_search();
                                    },
                                )
                            }
                        })),
                )
                .child(surface::ghost_button(
                    icon!("refresh"),
                    copy::text("Refresh files", "刷新文件", "重新整理檔案"),
                    move || (host.on_refresh)(),
                ))
                .child(surface::ghost_button(
                    icon!("chevrons-down-up"),
                    copy::text("Collapse folders", "折叠文件夹", "摺疊資料夾"),
                    move || (host.on_collapse)(),
                )),
        )
        // What the search reads, and how strictly it matches what was typed.
        .child(
            view()
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_MD))
                .py(px(theme::SPACING_SM))
                .child(
                    surface::segment_track()
                        .w(px(172.0))
                        .flex_shrink_0()
                        .child(surface::segment(
                            copy::text("Name", "名称", "名稱"),
                            move || !options.get().contents,
                            scope(false, run_search.clone()),
                        ))
                        .child(surface::segment(
                            copy::text("Content", "内容", "內容"),
                            move || options.get().contents,
                            scope(true, run_search.clone()),
                        )),
                )
                .child(view().flex_1().min_w(px(0.0)))
                .child(when(
                    move || options.get().contents,
                    move || {
                        view()
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
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
                )),
        )
        .child(when(
            move || error.with(Option::is_some) && !searching.get(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .pb(px(theme::SPACING_SM))
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
                        surface::empty_state(
                            icon!("search"),
                            copy::text("No matching items", "没有匹配项", "沒有符合項目"),
                            copy::text(
                                "Try another word, or search what the files contain.",
                                "换个关键词，或者改为搜索文件内容。",
                                "換個關鍵字，或者改為搜尋檔案內容。",
                            ),
                        )
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
    let on = move || *field(&mut options.get());
    button()
        .h(px(30.0))
        .min_w(px(34.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_TILE))
        .accessibility_label(label)
        .bg(move || {
            if on() {
                theme::action_primary()
            } else {
                theme::fill()
            }
        })
        .active(|style| style.opacity(0.7))
        .on_press(move || {
            options.update(|options| {
                let value = field(options);
                *value = !*value;
            });
            submit();
        })
        .child(
            text(title)
                .font_family("monospace")
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(move || {
                    if on() {
                        theme::action_primary_label()
                    } else {
                        theme::text_secondary()
                    }
                }),
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
                    .min_h(px(theme::ROW_HEIGHT))
                    .mx(px(theme::SPACING_SM))
                    .px(px(theme::SPACING_SM))
                    .py(px(6.0))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_MD))
                    .rounded(px(theme::RADIUS_CHIP))
                    .active(|style| style.bg(theme::row_pressed_bg()))
                    .on_press(move || {
                        let row = row.get_untracked();
                        dismiss_keyboard();
                        open(&row.path, row.line);
                    })
                    .child(surface::tile(icon!("file-text")))
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
                                .text_size(px(theme::font_detail()))
                                .text_color(theme::foreground),
                            )
                            .child(
                                text(move || {
                                    row.with(|row| {
                                        if row.snippet.trim().is_empty() {
                                            row.path.clone()
                                        } else {
                                            row.snippet.trim().to_owned()
                                        }
                                    })
                                })
                                .truncate()
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            ),
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
    // The row's own path: a compacted `a/b/c` row toggles the whole chain,
    // which the service resolves from its own tree.
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
        .mx(px(theme::SPACING_SM))
        .px(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CHIP))
        .flex_row()
        .items_center()
        .gap(px(6.0))
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
                .left(px(theme::SPACING_LG + level as f32 * TREE_INDENT))
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
                .text_size(px(theme::font_detail()))
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
        .children((!status.is_empty()).then(|| {
            text(status)
                .w(px(16.0))
                .flex_shrink_0()
                .text_center()
                .font_family("monospace")
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(status_color)
        }))
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
        // The head: the way back to the tree, what is open and where it
        // lives, and the two ways to move about in it.
        .child(
            view()
                .min_h(px(theme::TOUCH_TARGET))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .pb(px(theme::SPACING_SM))
                .child(surface::round_button(
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
                            .text_size(px(theme::font_detail()))
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
                            .text_size(px(theme::font_hint()))
                            .text_color(theme::text_muted),
                        ),
                )
                .child(when(can_find, move || {
                    view()
                        .flex_row()
                        .flex_shrink_0()
                        .child(surface::ghost_button(
                            icon!("search"),
                            copy::text("Find in file", "在文件中查找", "在檔案中尋找"),
                            move || {
                                jump.set(false);
                                find.update(|value| *value = !*value);
                            },
                        ))
                        .child(surface::ghost_button(
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
                tool_bar()
                    .child(
                        surface::field_pill()
                            .flex_1()
                            .min_w(px(0.0))
                            .pr(px(theme::SPACING_MD))
                            .mr(px(theme::SPACING_XS))
                            .child(surface::field_glyph(icon!("search")))
                            .child(
                                field::text_field_bare(
                                    query,
                                    copy::text("Find in file", "在文件中查找", "在檔案中尋找"),
                                )
                                .auto_focus(true)
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full(),
                            )
                            .child(
                                text(move || {
                                    matches.with(|result| {
                                        format!(
                                            "{}/{}",
                                            result.current.map_or(0, |i| i + 1),
                                            result.count
                                        )
                                    })
                                })
                                .flex_shrink_0()
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            ),
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
                    .child(surface::ghost_button(
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
                tool_bar()
                    .child(
                        surface::field_pill()
                            .flex_1()
                            .min_w(px(0.0))
                            .mr(px(theme::SPACING_XS))
                            .child(surface::field_glyph(icon!("hash")))
                            .child(
                                field::text_field_bare(
                                    line,
                                    copy::text("Line number", "行号", "行號"),
                                )
                                .auto_focus(true)
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full()
                                .on_submit(move |_: &str| on_jump()),
                            ),
                    )
                    .child(surface::ghost_button(
                        icon!("arrow-to-top"),
                        copy::text("Go", "跳转", "前往"),
                        on_jump,
                    ))
                    .child(surface::ghost_button(
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
                    .px(px(theme::SPACING_XL))
                    .child(
                        text(move || {
                            error.get().unwrap_or_else(|| {
                                copy::text("Loading file…", "正在加载文件…", "正在載入檔案…").into()
                            })
                        })
                        .text_center()
                        .text_size(px(theme::font_detail()))
                        .text_color(theme::text_muted),
                    )
                    .into_element();
            };
            let force_source = find.get() || jump.get() || preview.line.is_some();
            let body = match preview.kind {
                FilePreviewKindView::Markdown if !force_source => scroll()
                    .flex_1()
                    .child(
                        view()
                            .px(px(theme::SPACING_LG))
                            .py(px(theme::SPACING_SM))
                            .child(
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
                FilePreviewKindView::Binary => surface::empty_state(
                    icon!("file-text"),
                    copy::text(
                        "No preview for this file",
                        "无法预览此文件",
                        "無法預覽此檔案",
                    ),
                    copy::text(
                        "It is not text, Markdown or an image.",
                        "它不是文本、Markdown 或图片。",
                        "它不是文字、Markdown 或圖片。",
                    ),
                )
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
                .border_t_1()
                .border_color(theme::hairline)
                .child(body)
                .children(preview.truncated.then(|| {
                    text(copy::text(
                        "The file is larger than the preview limit.",
                        "文件超过预览上限。",
                        "檔案超過預覽上限。",
                    ))
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
                }))
                .into_element()
        }))
}

/// The strip a viewer's tool is used in: a field, and the controls beside it.
fn tool_bar() -> View {
    view()
        .flex_shrink_0()
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .pl(px(theme::SPACING_MD))
        .pr(px(theme::SPACING_SM))
        .pb(px(theme::SPACING_SM))
}

fn find_button(
    reader: ComponentRef<Editor>,
    command: EditorCommand,
    icon: ImageSrc,
    label: &'static str,
) -> impl IntoElement {
    surface::ghost_button(icon, label, move || {
        if let Some(api) = reader.api::<EditorApi>() {
            api.run(command);
        }
    })
}

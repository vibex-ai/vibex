//! The remote workspace's file tree, search, and read-only viewer.

use std::{rc::Rc, time::Duration};

use nsh_base::{
    button,
    text_view::{MarkdownOptions, text_view},
};
use nsh_core::{
    element::{ImageSrc, Position},
    prelude::*,
};
use unicode_width::UnicodeWidthChar as _;

use crate::{
    copy, theme,
    ui::{field, surface},
};

/// A row of the tree.
const TREE_ROW_HEIGHT: f32 = 40.0;
/// How far a level of the tree steps in.
const TREE_INDENT: f32 = 18.0;
/// Typography and geometry for the read-only source code viewer.
const CODE_FONT_SIZE: f32 = 13.0;
const CODE_LINE_HEIGHT: f32 = 20.0;
const CODE_PAD_X: f32 = 12.0;
const CODE_PAD_Y: f32 = 8.0;
/// Files up to this many lines mount every line directly so native Select All
/// covers the full file without virtualization boundaries.
const DIRECT_RENDER_LIMIT: usize = 400;
/// Extra lines mounted above and below the viewport for large files.
const VISIBLE_OVERSCAN_LINES: usize = 60;
/// Scroll quantization step so micro-scrolls do not churn visible row memos.
const SCROLL_BUCKET_LINES: usize = 12;

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

/// Active content-search highlight carried from the browser into the viewer.
#[derive(Clone, Debug, PartialEq, Eq)]
struct FileSearchHighlight {
    query: String,
    case_sensitive: bool,
    whole_word: bool,
    regex: bool,
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
    let highlight = signal(None::<FileSearchHighlight>);
    view()
        .flex_1()
        .min_h(px(0.0))
        .pb_safe_area()
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .style_when(move || path.with(Option::is_some), |style| style.hidden())
                .child(file_browser(host, highlight)),
        )
        .child(when(
            move || path.with(Option::is_some),
            move || file_viewer(preview.clone(), highlight),
        ))
}

fn file_browser(
    host: FilesHost,
    highlight: Signal<Option<FileSearchHighlight>>,
) -> impl IntoElement {
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
                    copy::text(
                        "Expand or collapse folders",
                        "展开或折叠文件夹",
                        "展開或摺疊資料夾",
                    ),
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
                                Some(row) => file_row(row, open.clone(), dir.clone(), highlight)
                                    .into_element(),
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
                .otherwise(move || {
                    file_search_list(results, open.clone(), query, options, highlight)
                })
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

fn file_search_list(
    rows: Signal<Vec<FileSearchRow>>,
    open: OpenFile,
    query: Signal<String>,
    options: Signal<SearchOptions>,
    highlight: Signal<Option<FileSearchHighlight>>,
) -> impl IntoElement {
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
                        let opts = options.get_untracked();
                        let needle = query.get_untracked().trim().to_owned();
                        if opts.contents && !needle.is_empty() {
                            highlight.set(Some(FileSearchHighlight {
                                query: needle,
                                case_sensitive: opts.case_sensitive,
                                whole_word: opts.whole_word,
                                regex: opts.regex,
                            }));
                        } else {
                            highlight.set(None);
                        }
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

fn file_row(
    row: FileEntry,
    open: OpenFile,
    toggle: Rc<dyn Fn(&str)>,
    highlight: Signal<Option<FileSearchHighlight>>,
) -> impl IntoElement {
    let directory = row.directory;
    let depth = row.depth.min(10);
    let ignored = row.ignored;
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
    let status_color = git_status_color(&status, ignored);
    let name_color = file_name_color(&status, ignored);
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
                highlight.set(None);
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
                        .text_color(if ignored {
                            theme::text_muted().opacity(0.42)
                        } else {
                            theme::text_muted()
                        })
                    },
                )),
        )
        .child(
            svg(icon)
                .size(px(theme::ICON_SM))
                .flex_shrink_0()
                .text_color(color)
                .opacity(if ignored { 0.35 } else { 1.0 }),
        )
        .child(
            text(row.name)
                .flex_1()
                .min_w(px(0.0))
                .truncate()
                .text_size(px(theme::font_detail()))
                .text_color(name_color),
        )
        .children(
            row.loading
                .then(|| crate::ui::activity::spinner(14.0, theme::text_muted)),
        )
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

fn git_status_color(status: &str, ignored: bool) -> Hsla {
    if ignored || status == "I" {
        return theme::text_muted().opacity(0.42);
    }
    match status {
        "A" => theme::status_added(),
        "?" => theme::status_untracked(),
        "D" | "U" => theme::accent_red(),
        "M" | "R" | "C" => theme::status_modified(),
        _ => theme::text_muted(),
    }
}

fn file_name_color(status: &str, ignored: bool) -> Hsla {
    if ignored || status == "I" {
        return theme::foreground().opacity(0.38);
    }
    match status {
        "A" => theme::status_added(),
        "?" => theme::status_untracked(),
        "D" | "U" => theme::accent_red(),
        "M" | "R" | "C" => theme::status_modified(),
        _ => theme::foreground(),
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct CodeMatch {
    line: usize,
    start: usize,
    end: usize,
}

fn split_source_lines(body: &str) -> Vec<String> {
    if body.is_empty() {
        vec![String::new()]
    } else {
        body.lines().map(ToOwned::to_owned).collect()
    }
}

fn is_word_char(ch: char) -> bool {
    ch.is_alphanumeric() || ch == '_'
}

fn is_word_boundary(line: &str, start: usize, end: usize) -> bool {
    let before = line[..start].chars().next_back();
    let after = line[end..].chars().next();
    !before.is_some_and(is_word_char) && !after.is_some_and(is_word_char)
}

fn find_code_matches(lines: &[String], query: &str, options: SearchOptions) -> Vec<CodeMatch> {
    if query.is_empty() {
        return Vec::new();
    }
    let mut matches = Vec::new();
    if options.case_sensitive {
        for (line_idx, line) in lines.iter().enumerate() {
            for (start, matched) in line.match_indices(query) {
                let end = start + matched.len();
                if options.whole_word && !is_word_boundary(line, start, end) {
                    continue;
                }
                matches.push(CodeMatch {
                    line: line_idx,
                    start,
                    end,
                });
            }
        }
        return matches;
    }

    let needle = query.to_lowercase();
    if needle.is_empty() {
        return matches;
    }
    for (line_idx, line) in lines.iter().enumerate() {
        if line.is_ascii() && needle.is_ascii() {
            let lowered = line.to_ascii_lowercase();
            for (start, matched) in lowered.match_indices(&needle) {
                let end = start + matched.len();
                if options.whole_word && !is_word_boundary(line, start, end) {
                    continue;
                }
                matches.push(CodeMatch {
                    line: line_idx,
                    start,
                    end,
                });
            }
            continue;
        }
        let mut folded = String::with_capacity(line.len());
        let mut byte_map: Vec<(usize, usize)> = Vec::with_capacity(line.len());
        for (byte_idx, ch) in line.char_indices() {
            let char_end = byte_idx + ch.len_utf8();
            for lower in ch.to_lowercase() {
                let start_len = folded.len();
                folded.push(lower);
                for _ in start_len..folded.len() {
                    byte_map.push((byte_idx, char_end));
                }
            }
        }
        for (folded_start, matched) in folded.match_indices(&needle) {
            let folded_end = folded_start + matched.len();
            if folded_end == 0 || folded_end > byte_map.len() {
                continue;
            }
            let start = byte_map[folded_start].0;
            let end = byte_map[folded_end - 1].1;
            if !line.is_char_boundary(start) || !line.is_char_boundary(end) || start >= end {
                continue;
            }
            if options.whole_word && !is_word_boundary(line, start, end) {
                continue;
            }
            matches.push(CodeMatch {
                line: line_idx,
                start,
                end,
            });
        }
    }
    matches
}

fn display_line_width(line: &str) -> usize {
    line.chars()
        .map(|ch| {
            if ch == '\t' {
                4
            } else {
                ch.width().unwrap_or(1)
            }
        })
        .sum()
}

fn scroll_target_y(line_index: usize, total_lines: usize, viewport_height: f32) -> f32 {
    let total_height = total_lines as f32 * CODE_LINE_HEIGHT + CODE_PAD_Y * 2.0;
    let max_y = (total_height - viewport_height).max(0.0);
    let line_y = CODE_PAD_Y + line_index as f32 * CODE_LINE_HEIGHT;
    let centered = line_y - (viewport_height - CODE_LINE_HEIGHT).max(0.0) * 0.35;
    centered.clamp(0.0, max_y)
}

fn file_viewer(
    host: FilesHost,
    highlight: Signal<Option<FileSearchHighlight>>,
) -> impl IntoElement {
    let path = host.path;
    let file = host.file;
    let error = host.error;
    let vscroll = element_ref();
    let initial_hl = highlight.get_untracked();
    let find = signal(initial_hl.is_some());
    let find_auto_focus = signal(false);
    let jump = signal(false);
    let query = signal(
        initial_hl
            .as_ref()
            .map(|hl| hl.query.clone())
            .unwrap_or_default(),
    );
    let find_options = signal(
        initial_hl
            .as_ref()
            .map(|hl| SearchOptions {
                contents: true,
                case_sensitive: hl.case_sensitive,
                whole_word: hl.whole_word,
                regex: hl.regex,
            })
            .unwrap_or_default(),
    );
    let line = signal(String::new());
    let current_match = signal(None::<usize>);
    let active_line = signal(None::<usize>);
    let viewport_height = signal(0.0_f32);
    let initial = stored(None::<(String, u32)>);

    let source_lines = computed(move || {
        Rc::new(file.with(|preview| {
            preview
                .as_ref()
                .map(|preview| split_source_lines(&preview.body))
                .unwrap_or_default()
        }))
    });

    let search_matches = computed(move || {
        if !find.get() {
            return Rc::new(Vec::<CodeMatch>::new());
        }
        let lines = source_lines.get();
        let needle = query.get();
        let opts = find_options.get();
        Rc::new(find_code_matches(&lines, &needle, opts))
    });

    effect(move || {
        let _ = path.get();
        active_line.set(None);
        current_match.set(None);
        if let Some(hl) = highlight.get() {
            query.set(hl.query);
            find_options.set(SearchOptions {
                contents: true,
                case_sensitive: hl.case_sensitive,
                whole_word: hl.whole_word,
                regex: hl.regex,
            });
            find_auto_focus.set(false);
            find.set(true);
            jump.set(false);
        }
    });

    effect(move || {
        let _ = query.get();
        let _ = find.get();
        let _ = find_options.get();
        current_match.set(None);
    });

    let scroll_to_line = move |target: usize| {
        let total = source_lines.get_untracked().len();
        if total == 0 {
            return;
        }
        let clamped = target.min(total.saturating_sub(1));
        active_line.set(Some(clamped));
        let vh = viewport_height.get_untracked().max(CODE_LINE_HEIGHT * 8.0);
        let y = scroll_target_y(clamped, total, vh);
        vscroll.scroll_to(Point::new(px(0.0), px(y)), false);
    };

    let step_match = move |forward: bool| {
        let matches = search_matches.get_untracked();
        if matches.is_empty() {
            current_match.set(None);
            return;
        }
        let next = match current_match.get_untracked() {
            None if forward => 0,
            None => matches.len().saturating_sub(1),
            Some(index) if forward => (index + 1) % matches.len(),
            Some(0) => matches.len().saturating_sub(1),
            Some(index) => index - 1,
        };
        current_match.set(Some(next));
        scroll_to_line(matches[next].line);
    };

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
        if let Ok(parsed) = line.get_untracked().trim().parse::<u32>() {
            let total = source_lines.get_untracked().len();
            if total > 0 {
                let target = (parsed.saturating_sub(1) as usize).min(total.saturating_sub(1));
                scroll_to_line(target);
            }
            jump.set(false);
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
                        find.set(false);
                        jump.set(false);
                        highlight.set(None);
                        current_match.set(None);
                        active_line.set(None);
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
                                find_auto_focus.set(true);
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
                let focus_find = find_auto_focus.get_untracked();
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
                                .auto_focus(focus_find)
                                .flex_1()
                                .min_w(px(0.0))
                                .h_full(),
                            )
                            .child(
                                text(move || {
                                    let count = search_matches.get().len();
                                    let current = current_match.get().map_or(0, |i| i + 1);
                                    format!("{current}/{count}")
                                })
                                .flex_shrink_0()
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted),
                            ),
                    )
                    .child(surface::ghost_button(
                        icon!("chevron-up"),
                        copy::text("Previous match", "上一个匹配", "上一個符合"),
                        move || step_match(false),
                    ))
                    .child(surface::ghost_button(
                        icon!("chevron-down"),
                        copy::text("Next match", "下一个匹配", "下一個符合"),
                        move || step_match(true),
                    ))
                    .child(surface::ghost_button(
                        icon!("x"),
                        copy::text("Close search", "关闭搜索", "關閉搜尋"),
                        move || {
                            find.set(false);
                            highlight.set(None);
                            query.set(String::new());
                            current_match.set(None);
                            active_line.set(None);
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
                    read_only_code_viewer(
                        source_lines.get_untracked(),
                        vscroll,
                        viewport_height,
                        active_line,
                        search_matches,
                        current_match,
                        location,
                        initial,
                        scroll_to_line,
                    )
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

/// Strictly read-only source code viewer built on native `SelectableView`.
///
/// Unlike `nsh_base::editor::Editor`, this mounts no `<input>` element inside
/// the document, so tapping, scrolling, or selecting file content never raises
/// the soft keyboard on Android or iOS while keeping native selection handles,
/// Copy, and Select All fully usable across lines.
#[allow(clippy::too_many_arguments)]
fn read_only_code_viewer(
    lines: Rc<Vec<String>>,
    vscroll: ElementRef,
    viewport_height: Signal<f32>,
    active_line: Signal<Option<usize>>,
    search_matches: Memo<Rc<Vec<CodeMatch>>>,
    current_match: Signal<Option<usize>>,
    location: Option<(String, u32)>,
    initial: Stored<Option<(String, u32)>>,
    scroll_to_line: impl Fn(usize) + Copy + 'static,
) -> impl IntoElement {
    let total_lines = lines.len().max(1);
    let total_height = total_lines as f32 * CODE_LINE_HEIGHT + CODE_PAD_Y * 2.0;
    let gutter_digits = total_lines.to_string().len().max(2);
    let gutter_width = (gutter_digits as f32 * 7.8 + 16.0).max(36.0);
    let max_cols = lines
        .iter()
        .map(|line| display_line_width(line))
        .max()
        .unwrap_or(0);
    let content_width = max_cols as f32 * 8.2 + CODE_PAD_X * 2.0 + 24.0;

    let scroll_bucket = signal(0usize);
    let visible_lines = computed(move || {
        if total_lines <= DIRECT_RENDER_LIMIT {
            return (0..total_lines).collect::<Vec<_>>();
        }
        let bucket = scroll_bucket.get();
        let vh = viewport_height.get().max(800.0);
        let first_visible = bucket.saturating_mul(SCROLL_BUCKET_LINES);
        let visible_count = (vh / CODE_LINE_HEIGHT).ceil() as usize + 2;
        let start = first_visible.saturating_sub(VISIBLE_OVERSCAN_LINES);
        let end = (first_visible + visible_count + VISIBLE_OVERSCAN_LINES).min(total_lines);
        (start..end).collect::<Vec<_>>()
    });

    scroll()
        .element_ref(&vscroll)
        .flex_1()
        .min_h(px(0.0))
        .on_scroll(move |event: &ScrollEvent| {
            if total_lines > DIRECT_RENDER_LIMIT {
                let first = ((event.offset.y.0 - CODE_PAD_Y).max(0.0) / CODE_LINE_HEIGHT) as usize;
                let bucket = first / SCROLL_BUCKET_LINES;
                if scroll_bucket.get_untracked() != bucket {
                    scroll_bucket.set(bucket);
                }
            }
        })
        .on_layout(move |bounds: &Bounds<Points>| {
            if bounds.size.height.0 <= 0.0 {
                return;
            }
            if (viewport_height.get_untracked() - bounds.size.height.0).abs() > 0.5 {
                viewport_height.set(bounds.size.height.0);
            }
            if initial.get() != location
                && let Some((_, line)) = location.as_ref()
            {
                initial.replace(location.clone());
                let target = (*line).saturating_sub(1) as usize;
                let matches = search_matches.get_untracked();
                if let Some(index) = matches.iter().position(|m| m.line == target) {
                    current_match.set(Some(index));
                }
                scroll_to_line(target);
                spawn(async move {
                    timer(Duration::from_millis(48)).await;
                    scroll_to_line(target);
                });
            }
        })
        .child(
            view()
                .w_full()
                .h(px(total_height))
                .flex_row()
                // Gutter lives outside `selectable(true)` with `select_none()` so
                // cross-line copies never include line numbers.
                .child(
                    view()
                        .test_id("file-gutter")
                        .relative()
                        .flex_shrink_0()
                        .w(px(gutter_width))
                        .h(px(total_height))
                        .border_r_1()
                        .border_color(theme::hairline)
                        .select_none()
                        .child(each(
                            visible_lines,
                            |index: &usize| *index,
                            move |index, _| {
                                let line_index = index.get_untracked();
                                let top = CODE_PAD_Y + line_index as f32 * CODE_LINE_HEIGHT;
                                view()
                                    .absolute()
                                    .left_0()
                                    .right_0()
                                    .top(px(top))
                                    .h(px(CODE_LINE_HEIGHT))
                                    .px(px(theme::SPACING_SM))
                                    .justify_center()
                                    .bg(move || {
                                        if active_line.get() == Some(line_index) {
                                            theme::accent_blue().alpha(0.10)
                                        } else {
                                            transparent_black()
                                        }
                                    })
                                    .child(
                                        text((line_index + 1).to_string())
                                            .select_none()
                                            .text_right()
                                            .font_family("monospace")
                                            .text_size(px(11.5))
                                            .line_height(px(CODE_LINE_HEIGHT))
                                            .text_color(move || {
                                                if active_line.get() == Some(line_index) {
                                                    theme::foreground()
                                                } else {
                                                    theme::text_muted()
                                                }
                                            }),
                                    )
                            },
                        )),
                )
                .child(
                    scroll().horizontal().flex_1().min_w(px(0.0)).child(
                        view()
                            .test_id("file-content")
                            .selectable(true)
                            .relative()
                            .min_w(relative(1.0))
                            .w(px(content_width))
                            .h(px(total_height))
                            .child(each(
                                visible_lines,
                                |index: &usize| *index,
                                move |index, _| {
                                    let line_index = index.get_untracked();
                                    let content =
                                        lines.get(line_index).cloned().unwrap_or_default();
                                    let top = CODE_PAD_Y + line_index as f32 * CODE_LINE_HEIGHT;
                                    code_content_row(
                                        line_index,
                                        top,
                                        content,
                                        active_line,
                                        search_matches,
                                        current_match,
                                    )
                                },
                            )),
                    ),
                ),
        )
}

fn code_content_row(
    line_index: usize,
    top: f32,
    content: String,
    active_line: Signal<Option<usize>>,
    search_matches: Memo<Rc<Vec<CodeMatch>>>,
    current_match: Signal<Option<usize>>,
) -> impl IntoElement {
    let line_matches = computed(move || {
        let all = search_matches.get();
        let active = current_match.get();
        all.iter()
            .enumerate()
            .filter(|(_, m)| m.line == line_index)
            .map(|(idx, m)| (m.start, m.end, active == Some(idx)))
            .collect::<Vec<_>>()
    });
    view()
        .absolute()
        .left_0()
        .right_0()
        .top(px(top))
        .h(px(CODE_LINE_HEIGHT))
        .px(px(CODE_PAD_X))
        .justify_center()
        .bg(move || {
            if active_line.get() == Some(line_index) {
                theme::accent_blue().alpha(0.10)
            } else {
                transparent_black()
            }
        })
        .child(dynamic(move || {
            let matches = line_matches.get();
            if matches.is_empty() {
                text(content.clone())
                    .select_text()
                    .font_family("monospace")
                    .text_size(px(CODE_FONT_SIZE))
                    .line_height(px(CODE_LINE_HEIGHT))
                    .text_color(theme::foreground)
                    .number_of_lines(1u32)
                    .text_overflow(nsh_core::EllipsizeMode::Clip)
                    .into_element()
            } else {
                let mut spans: Vec<AnyElement> = Vec::new();
                let mut cursor = 0usize;
                for (start, end, is_current) in matches {
                    if start > cursor && content.is_char_boundary(start) {
                        spans.push(text(content[cursor..start].to_owned()).into_element());
                    }
                    if end > start
                        && content.is_char_boundary(start)
                        && content.is_char_boundary(end)
                    {
                        let bg = if is_current {
                            theme::accent_yellow().alpha(0.55)
                        } else {
                            theme::accent_yellow().alpha(0.25)
                        };
                        spans.push(
                            text(content[start..end].to_owned())
                                .bg(bg)
                                .text_color(theme::foreground)
                                .into_element(),
                        );
                        cursor = end;
                    }
                }
                if cursor < content.len() && content.is_char_boundary(cursor) {
                    spans.push(text(content[cursor..].to_owned()).into_element());
                }
                text("")
                    .select_text()
                    .font_family("monospace")
                    .text_size(px(CODE_FONT_SIZE))
                    .line_height(px(CODE_LINE_HEIGHT))
                    .text_color(theme::foreground)
                    .number_of_lines(1u32)
                    .text_overflow(nsh_core::EllipsizeMode::Clip)
                    .children(spans)
                    .into_element()
            }
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn find_code_matches_handles_case_whole_word_and_unicode() {
        let lines = split_source_lines(
            "fn greeting() {\n    let greeting_id = \"Greeting\";\n    println!(\"你好 greeting\");\n}",
        );
        let ci = find_code_matches(&lines, "greeting", SearchOptions::default());
        assert_eq!(ci.len(), 4);
        assert_eq!(ci[0].line, 0);
        assert_eq!(ci[1].line, 1);
        assert_eq!(ci[2].line, 1);
        assert_eq!(ci[3].line, 2);

        let whole = find_code_matches(
            &lines,
            "greeting",
            SearchOptions {
                whole_word: true,
                ..Default::default()
            },
        );
        // Skips `greeting_id`, matches `greeting`, `Greeting`, and `greeting` after CJK.
        assert_eq!(whole.len(), 3);

        let cs = find_code_matches(
            &lines,
            "Greeting",
            SearchOptions {
                case_sensitive: true,
                ..Default::default()
            },
        );
        assert_eq!(cs.len(), 1);
        assert_eq!(cs[0].line, 1);
    }

    #[test]
    fn split_source_lines_preserves_empty_lines_and_handles_empty_file() {
        assert_eq!(split_source_lines(""), vec![String::new()]);
        assert_eq!(split_source_lines("a\n\nb"), vec!["a", "", "b"]);
    }
}

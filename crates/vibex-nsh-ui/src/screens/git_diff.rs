//! Structured unified diff parsing and mobile-friendly rendering for file diffs
//! and multi-file commit details.

use std::collections::BTreeSet;

use nsh_base::button;
use nsh_core::prelude::*;

use crate::{copy, theme, ui::surface};

/// Maximum diff lines drawn inline before showing an expand button.
const MAX_INLINE_FILE_DIFF_LINES: usize = 300;
/// Maximum diff lines drawn inline per file inside a multi-file commit view.
const MAX_INLINE_COMMIT_FILE_LINES: usize = 200;
/// Auto-expand all files in a commit when at most this many files changed.
const AUTO_EXPAND_MAX_FILES: usize = 5;

/// One changed file inside a commit detail view.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitFileDiff {
    pub path: String,
    pub old_path: Option<String>,
    pub status: String,
    pub additions: u32,
    pub deletions: u32,
    pub binary: bool,
    pub patch: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DiffLineKind {
    Hunk,
    Context,
    Added,
    Removed,
    Note,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DiffLine {
    pub kind: DiffLineKind,
    pub old_line: Option<u32>,
    pub new_line: Option<u32>,
    pub text: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ParsedFilePatch {
    pub path: String,
    pub old_path: Option<String>,
    pub binary: bool,
    pub additions: u32,
    pub deletions: u32,
    pub lines: Vec<DiffLine>,
}

/// Splits a unified diff (single-file or multi-file `git show` / `git diff`)
/// into structured per-file sections with line numbers and noisy git headers
/// stripped.
pub fn parse_unified_diff(raw: &str) -> Vec<ParsedFilePatch> {
    let mut sections = Vec::new();
    let mut current: Option<ParsedFilePatch> = None;
    let mut old_num: u32 = 0;
    let mut new_num: u32 = 0;
    let mut in_hunk = false;

    for raw_line in raw.lines() {
        if let Some((old_p, new_p)) = parse_diff_git_header(raw_line) {
            if let Some(section) = current.take() {
                sections.push(section);
            }
            let old_path = (old_p != new_p).then_some(old_p);
            current = Some(ParsedFilePatch {
                path: new_p,
                old_path,
                binary: false,
                additions: 0,
                deletions: 0,
                lines: Vec::new(),
            });
            in_hunk = false;
            continue;
        }

        if let Some((old_start, new_start)) = parse_hunk_header(raw_line) {
            let section = current.get_or_insert_with(|| ParsedFilePatch {
                path: String::new(),
                old_path: None,
                binary: false,
                additions: 0,
                deletions: 0,
                lines: Vec::new(),
            });
            old_num = old_start;
            new_num = new_start;
            in_hunk = true;
            section.lines.push(DiffLine {
                kind: DiffLineKind::Hunk,
                old_line: None,
                new_line: None,
                text: raw_line.to_owned(),
            });
            continue;
        }

        if !in_hunk {
            if raw_line.starts_with("Binary files ") || raw_line == "GIT binary patch" {
                let section = current.get_or_insert_with(|| ParsedFilePatch {
                    path: String::new(),
                    old_path: None,
                    binary: true,
                    additions: 0,
                    deletions: 0,
                    lines: Vec::new(),
                });
                section.binary = true;
                continue;
            }
            if let Some(rest) = raw_line.strip_prefix("rename from ") {
                if let Some(section) = current.as_mut() {
                    section.old_path = Some(clean_diff_path(rest));
                }
                continue;
            }
            if let Some(rest) = raw_line.strip_prefix("rename to ") {
                if let Some(section) = current.as_mut() {
                    section.path = clean_diff_path(rest);
                }
                continue;
            }
            if let Some(rest) = raw_line.strip_prefix("+++ ") {
                let cleaned = clean_diff_path(rest);
                if cleaned != "/dev/null" {
                    let section = current.get_or_insert_with(|| ParsedFilePatch {
                        path: cleaned.clone(),
                        old_path: None,
                        binary: false,
                        additions: 0,
                        deletions: 0,
                        lines: Vec::new(),
                    });
                    if section.path.is_empty() {
                        section.path = cleaned;
                    }
                }
                continue;
            }
            if raw_line.starts_with("--- ")
                || raw_line.starts_with("index ")
                || raw_line.starts_with("old mode ")
                || raw_line.starts_with("new mode ")
                || raw_line.starts_with("new file mode ")
                || raw_line.starts_with("deleted file mode ")
                || raw_line.starts_with("similarity index ")
                || raw_line.starts_with("dissimilarity index ")
                || raw_line.starts_with("copy from ")
                || raw_line.starts_with("copy to ")
            {
                continue;
            }
            continue;
        }

        let Some(section) = current.as_mut() else {
            continue;
        };

        if let Some(content) = raw_line.strip_prefix('+') {
            section.additions = section.additions.saturating_add(1);
            section.lines.push(DiffLine {
                kind: DiffLineKind::Added,
                old_line: None,
                new_line: Some(new_num),
                text: expand_tabs(content),
            });
            new_num = new_num.saturating_add(1);
        } else if let Some(content) = raw_line.strip_prefix('-') {
            section.deletions = section.deletions.saturating_add(1);
            section.lines.push(DiffLine {
                kind: DiffLineKind::Removed,
                old_line: Some(old_num),
                new_line: None,
                text: expand_tabs(content),
            });
            old_num = old_num.saturating_add(1);
        } else if let Some(content) = raw_line.strip_prefix(' ') {
            section.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_num),
                new_line: Some(new_num),
                text: expand_tabs(content),
            });
            old_num = old_num.saturating_add(1);
            new_num = new_num.saturating_add(1);
        } else if raw_line.starts_with('\\') {
            section.lines.push(DiffLine {
                kind: DiffLineKind::Note,
                old_line: None,
                new_line: None,
                text: raw_line.to_owned(),
            });
        } else {
            section.lines.push(DiffLine {
                kind: DiffLineKind::Context,
                old_line: Some(old_num),
                new_line: Some(new_num),
                text: expand_tabs(raw_line),
            });
            old_num = old_num.saturating_add(1);
            new_num = new_num.saturating_add(1);
        }
    }

    if let Some(section) = current {
        sections.push(section);
    }

    sections
}

/// Splits a raw unified commit patch into raw per-file patch slices keyed by
/// target path so `SessionService` can associate each `GitCommitFileChange`
/// with its exact patch text.
pub fn split_raw_patch_by_file(raw: &str) -> Vec<(String, String)> {
    let mut parts = Vec::new();
    let mut current_path: Option<String> = None;
    let mut current_lines: Vec<&str> = Vec::new();

    for line in raw.lines() {
        if let Some((_, new_p)) = parse_diff_git_header(line) {
            if let Some(path) = current_path.take() {
                parts.push((path, current_lines.join("\n")));
                current_lines.clear();
            }
            current_path = Some(new_p);
            current_lines.push(line);
        } else if current_path.is_some() {
            current_lines.push(line);
        }
    }
    if let Some(path) = current_path {
        parts.push((path, current_lines.join("\n")));
    }
    parts
}

fn parse_diff_git_header(line: &str) -> Option<(String, String)> {
    let rest = line.strip_prefix("diff --git ")?;
    if let Some(split_idx) = rest.find(" b/") {
        let a = clean_diff_path(&rest[..split_idx]);
        let b = clean_diff_path(&rest[split_idx + 1..]);
        return Some((a, b));
    }
    let mut parts = rest.split_whitespace();
    let a = clean_diff_path(parts.next()?);
    let b = clean_diff_path(parts.next()?);
    Some((a, b))
}

fn clean_diff_path(raw: &str) -> String {
    let trimmed = raw.trim().trim_matches('"');
    trimmed
        .strip_prefix("a/")
        .or_else(|| trimmed.strip_prefix("b/"))
        .unwrap_or(trimmed)
        .to_owned()
}

fn parse_hunk_header(line: &str) -> Option<(u32, u32)> {
    let rest = line.strip_prefix("@@ ")?;
    let end = rest.find(" @@")?;
    let ranges = &rest[..end];
    let mut parts = ranges.split_whitespace();
    let old_part = parts.next()?.strip_prefix('-')?;
    let new_part = parts.next()?.strip_prefix('+')?;
    let old_start = old_part
        .split(',')
        .next()
        .and_then(|s| s.parse::<u32>().ok())?;
    let new_start = new_part
        .split(',')
        .next()
        .and_then(|s| s.parse::<u32>().ok())?;
    Some((old_start, new_start))
}

fn expand_tabs(line: &str) -> String {
    if line.contains('\t') {
        line.replace('\t', "    ")
    } else {
        line.to_owned()
    }
}

/// Renders a single file's structured diff with stats summary and horizontal
/// scroll for crisp, unwrapped code lines.
pub fn single_file_diff(raw: String, staged: bool) -> impl IntoElement {
    let sections = parse_unified_diff(&raw);
    let (additions, deletions, binary, lines) = if let Some(first) = sections.into_iter().next() {
        (first.additions, first.deletions, first.binary, first.lines)
    } else {
        (0, 0, false, Vec::new())
    };
    let show_all = signal(false);

    view()
        .flex_1()
        .min_h(px(0.0))
        .child(
            // Compact stats header bar.
            view()
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_MD))
                .py(px(theme::SPACING_SM))
                .border_b_1()
                .border_color(theme::hairline)
                .bg(theme::fill)
                .child(
                    text(if staged {
                        copy::text("Staged", "已暂存", "已暫存")
                    } else {
                        copy::text("Working tree", "工作区", "工作區")
                    })
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(4.0))
                    .bg(theme::background_card)
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(theme::text_secondary),
                )
                .child(view().flex_1().min_w(px(0.0)))
                .child(diff_stat_counts(additions, deletions)),
        )
        .child(scroll().flex_1().min_h(px(0.0)).child(if binary {
            binary_diff_notice().into_element()
        } else if lines.is_empty() {
            if raw.trim().is_empty() {
                empty_diff_notice().into_element()
            } else {
                plain_fallback_block(raw).into_element()
            }
        } else {
            diff_code_block(lines, MAX_INLINE_FILE_DIFF_LINES, show_all).into_element()
        }))
}

/// Renders a commit detail screen with metadata header card, file summary with
/// Expand/Collapse All toggle, and per-file collapsible diff cards.
pub fn commit_detail_content(detail: super::git::CommitDetailPreview) -> impl IntoElement {
    // The runtime reports one entry per changed file, each with its own
    // patch. A runtime that answers with only a raw patch is still split by
    // the same headers, so both shapes draw as the same per-file cards.
    let files = if !detail.files.is_empty() {
        detail.files
    } else {
        let parsed = parse_unified_diff(&detail.body);
        parsed
            .iter()
            .filter(|section| !section.path.is_empty() || !section.lines.is_empty())
            .map(|section| CommitFileDiff {
                path: section.path.clone(),
                old_path: section.old_path.clone(),
                status: "M".to_owned(),
                additions: section.additions,
                deletions: section.deletions,
                binary: section.binary,
                patch: detail.body.clone(),
            })
            .collect()
    };

    let file_count = files.len();
    let total_additions: u32 = files.iter().map(|f| f.additions).sum();
    let total_deletions: u32 = files.iter().map(|f| f.deletions).sum();

    // Auto-expand all files for compact commits; for large commits expand the
    // first file so the list opens smoothly and remains easy to scan.
    let initial_collapsed: BTreeSet<usize> = if file_count <= AUTO_EXPAND_MAX_FILES {
        BTreeSet::new()
    } else {
        (1..file_count).collect()
    };
    let collapsed = signal(initial_collapsed);
    let message_expanded = signal(false);
    let subject = if detail.subject.is_empty() {
        detail.title.clone()
    } else {
        detail.subject.clone()
    };
    let message_body = detail.message_body.trim().to_owned();
    let show_message_body = !message_body.is_empty() && message_body != subject.trim();

    let subject_header: AnyElement = if show_message_body {
        button()
            .test_id("commit-message-toggle")
            .accessibility_label(move || {
                Some(SharedString::from(if message_expanded.get() {
                    copy::text("Collapse commit message", "收起提交说明", "收起提交說明")
                } else {
                    copy::text("Expand commit message", "展开提交说明", "展開提交說明")
                }))
            })
            .on_press(move || message_expanded.update(|expanded| *expanded = !*expanded))
            .w_full()
            .flex_row()
            .items_start()
            .gap(px(theme::SPACING_SM))
            .active(|style| style.opacity(0.7))
            .child(
                text(subject)
                    .flex_1()
                    .min_w(px(0.0))
                    .text_size(px(theme::font_body()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .line_height(theme::line_height_for(theme::font_body()))
                    .text_color(theme::foreground),
            )
            .child(
                view()
                    .flex_shrink_0()
                    .flex_row()
                    .items_center()
                    .gap(px(2.0))
                    .pt(px(2.0))
                    .child(
                        text(move || {
                            if message_expanded.get() {
                                copy::text("Collapse", "收起", "收起")
                            } else {
                                copy::text("Expand", "展开", "展開")
                            }
                        })
                        .text_size(px(theme::font_micro()))
                        .text_color(theme::text_muted),
                    )
                    .child(
                        when(
                            move || message_expanded.get(),
                            || {
                                svg(icon!("chevron-up"))
                                    .size(px(14.0))
                                    .text_color(theme::text_muted)
                            },
                        )
                        .otherwise(|| {
                            svg(icon!("chevron-down"))
                                .size(px(14.0))
                                .text_color(theme::text_muted)
                        }),
                    ),
            )
            .into_element()
    } else {
        text(subject)
            .text_size(px(theme::font_body()))
            .font_weight(FontWeight::SEMIBOLD)
            .line_height(theme::line_height_for(theme::font_body()))
            .text_color(theme::foreground)
            .into_element()
    };

    scroll().flex_1().min_h(px(0.0)).child(
        view()
            .w_full()
            .flex_col()
            .gap(px(theme::SPACING_MD))
            .px(px(theme::SPACING_MD))
            .py(px(theme::SPACING_MD))
            // Commit metadata card.
            .child(
                view()
                    .w_full()
                    .flex_col()
                    .gap(px(theme::SPACING_SM))
                    .p(px(theme::SPACING_MD))
                    .rounded(px(theme::RADIUS_CHIP))
                    .bg(theme::fill)
                    .border_1()
                    .border_color(theme::hairline)
                    .child(subject_header)
                    .children(show_message_body.then(|| {
                        let body = message_body.clone();
                        when(
                            move || message_expanded.get(),
                            move || {
                                text(body.clone())
                                    .test_id("commit-message-body")
                                    .selectable(true)
                                    .text_size(px(theme::font_caption()))
                                    .line_height(theme::line_height_for(theme::font_caption()))
                                    .text_color(theme::text_secondary)
                            },
                        )
                    }))
                    .child(
                        view()
                            .flex_row()
                            .flex_wrap()
                            .items_center()
                            .gap(px(theme::SPACING_SM))
                            .pt(px(2.0))
                            .child(
                                text(detail.title)
                                    .px(px(6.0))
                                    .py(px(2.0))
                                    .rounded(px(4.0))
                                    .bg(theme::background_card)
                                    .font_family("monospace")
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::accent_blue),
                            )
                            .children((!detail.author.is_empty()).then(|| {
                                text(detail.author)
                                    .text_size(px(theme::font_micro()))
                                    .font_weight(FontWeight::MEDIUM)
                                    .text_color(theme::text_secondary)
                            }))
                            .children((!detail.authored_at.is_empty()).then(|| {
                                text(detail.authored_at)
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted)
                            })),
                    ),
            )
            // Files changed summary + collapse/expand all toggle.
            .children((file_count > 0).then(|| {
                view()
                    .w_full()
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .px(px(2.0))
                    .child(
                        text(match copy::language() {
                            copy::Language::SimplifiedChinese => format!("{file_count} 个文件变更"),
                            copy::Language::TraditionalChinese => {
                                format!("{file_count} 個檔案變更")
                            }
                            _ if file_count == 1 => "1 file changed".to_owned(),
                            _ => format!("{file_count} files changed"),
                        })
                        .flex_1()
                        .min_w(px(0.0))
                        .text_size(px(theme::font_caption()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::text_secondary),
                    )
                    .child(diff_stat_counts(total_additions, total_deletions))
                    .child(surface::ghost_button(
                        icon!("chevrons-down-up"),
                        copy::text(
                            "Expand or collapse all files",
                            "展开或折叠全部文件",
                            "展開或摺疊全部檔案",
                        ),
                        move || {
                            collapsed.update(|set| {
                                if set.len() < file_count {
                                    *set = (0..file_count).collect();
                                } else {
                                    set.clear();
                                }
                            });
                        },
                    ))
            }))
            // Per-file collapsible diff cards.
            .children(files.into_iter().enumerate().map(move |(index, file)| {
                commit_file_card(index, file, collapsed, detail.truncated)
            }))
            // Fallback when a legacy fixture only has raw body text.
            .children(
                (file_count == 0 && !detail.body.trim().is_empty())
                    .then(|| plain_fallback_block(detail.body)),
            ),
    )
}

fn commit_file_card(
    index: usize,
    file: CommitFileDiff,
    collapsed: Signal<BTreeSet<usize>>,
    commit_truncated: bool,
) -> impl IntoElement {
    let is_expanded = move || collapsed.with(|set| !set.contains(&index));
    let show_all = signal(false);
    let parsed = if !file.patch.is_empty() {
        parse_unified_diff(&file.patch).into_iter().next()
    } else {
        None
    };
    let binary = file.binary || parsed.as_ref().is_some_and(|p| p.binary);
    let lines = parsed.map(|p| p.lines).unwrap_or_default();
    let (file_name, parent_dir) = split_display_path(&file.path);
    let rename_label = file
        .old_path
        .as_deref()
        .filter(|old| *old != file.path.as_str())
        .map(|old| format!("{old} → {}", file.path));
    let subtitle = rename_label.unwrap_or(parent_dir);
    let status = file.status.clone();
    let status_color = diff_status_color(&status);
    let additions = file.additions;
    let deletions = file.deletions;

    view()
        .w_full()
        .flex_col()
        .rounded(px(theme::RADIUS_CHIP))
        .border_1()
        .border_color(theme::hairline)
        .overflow_hidden()
        .child(
            button()
                .w_full()
                .min_h(px(44.0))
                .px(px(theme::SPACING_SM))
                .py(px(theme::SPACING_SM))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .bg(theme::fill)
                .active(|style| style.bg(theme::row_pressed_bg()))
                .on_press(move || {
                    collapsed.update(|set| {
                        if !set.remove(&index) {
                            set.insert(index);
                        }
                    });
                })
                .child(
                    svg(move || {
                        if is_expanded() {
                            icon!("chevron-down")
                        } else {
                            icon!("chevron-right")
                        }
                    })
                    .size(px(14.0))
                    .flex_shrink_0()
                    .text_color(theme::text_muted),
                )
                .child(
                    text(status)
                        .w(px(18.0))
                        .h(px(18.0))
                        .flex_shrink_0()
                        .rounded(px(4.0))
                        .bg(status_color.alpha(0.16))
                        .text_center()
                        .font_family("monospace")
                        .text_size(px(theme::font_micro()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(status_color),
                )
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_col()
                        .gap(px(1.0))
                        .child(
                            text(file_name)
                                .truncate()
                                .text_size(px(theme::font_caption()))
                                .font_weight(FontWeight::MEDIUM)
                                .text_color(theme::foreground),
                        )
                        .children((!subtitle.is_empty()).then(|| {
                            text(subtitle)
                                .truncate()
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::text_muted)
                        })),
                )
                .child(diff_stat_counts(additions, deletions)),
        )
        .child(when(is_expanded, move || {
            let lines = lines.clone();
            view()
                .w_full()
                .border_t_1()
                .border_color(theme::hairline)
                .bg(theme::block_page_adaptive)
                .child(if binary {
                    binary_diff_notice().into_element()
                } else if lines.is_empty() {
                    text(if commit_truncated {
                        copy::text(
                            "Patch truncated by preview limit.",
                            "补丁超出预览上限，未包含此文件详情。",
                            "修補程式超出預覽上限，未包含此檔案詳情。",
                        )
                    } else {
                        copy::text(
                            "No textual diff for this file.",
                            "此文件无文本差异。",
                            "此檔案無文字差異。",
                        )
                    })
                    .px(px(theme::SPACING_MD))
                    .py(px(theme::SPACING_MD))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
                    .into_element()
                } else {
                    diff_code_block(lines, MAX_INLINE_COMMIT_FILE_LINES, show_all).into_element()
                })
        }))
}

fn diff_code_block(
    lines: Vec<DiffLine>,
    inline_limit: usize,
    show_all: Signal<bool>,
) -> impl IntoElement {
    let total = lines.len();
    let max_line_num = lines
        .iter()
        .filter_map(|line| line.old_line.max(line.new_line))
        .max()
        .unwrap_or(1);
    let gutter_digits = max_line_num.to_string().len().max(2);
    let gutter_width = (gutter_digits as f32 * 7.2 + 8.0).max(26.0);

    view()
        .w_full()
        .flex_col()
        .child(scroll().horizontal().w_full().child(dynamic(move || {
            let visible = if show_all.get() || total <= inline_limit {
                total
            } else {
                inline_limit
            };
            view()
                .flex_col()
                .flex_shrink_0()
                .py(px(4.0))
                .children(
                    lines
                        .iter()
                        .take(visible)
                        .cloned()
                        .map(move |line| diff_code_row(line, gutter_width)),
                )
                .into_element()
        })))
        .child(when(
            move || total > inline_limit && !show_all.get(),
            move || {
                let remaining = total.saturating_sub(inline_limit);
                button()
                    .w_full()
                    .py(px(theme::SPACING_SM))
                    .px(px(theme::SPACING_MD))
                    .border_t_1()
                    .border_color(theme::hairline)
                    .bg(theme::fill)
                    .items_center()
                    .justify_center()
                    .active(|style| style.opacity(0.7))
                    .on_press(move || show_all.set(true))
                    .child(
                        text(match copy::language() {
                            copy::Language::SimplifiedChinese => {
                                format!("还有 {remaining} 行未显示，点击展开全部")
                            }
                            copy::Language::TraditionalChinese => {
                                format!("還有 {remaining} 行未顯示，點擊展開全部")
                            }
                            _ if remaining == 1 => "1 more line — tap to expand".to_owned(),
                            _ => format!("{remaining} more lines — tap to expand"),
                        })
                        .text_size(px(theme::font_micro()))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::accent_blue),
                    )
            },
        ))
}

fn diff_code_row(line: DiffLine, gutter_width: f32) -> impl IntoElement {
    let font_size = (theme::font_hint()).max(11.0);
    let row_lh = px((font_size * 1.48).round());
    if line.kind == DiffLineKind::Hunk {
        return view()
            .w_full()
            .min_h(px(24.0))
            .px(px(theme::SPACING_SM))
            .py(px(3.0))
            .bg(theme::diff_hunk_bg)
            .border_t_1()
            .border_b_1()
            .border_color(theme::hairline)
            .child(
                text(line.text)
                    .font_family(theme::TERMINAL_FONT_FAMILY)
                    .text_size(px(font_size - 0.5))
                    .line_height(row_lh)
                    .text_color(theme::accent_blue),
            )
            .into_element();
    }

    if line.kind == DiffLineKind::Note {
        return view()
            .w_full()
            .px(px(theme::SPACING_SM))
            .py(px(2.0))
            .child(
                text(line.text)
                    .font_family(theme::TERMINAL_FONT_FAMILY)
                    .text_size(px(font_size - 0.5))
                    .line_height(row_lh)
                    .text_color(theme::text_muted),
            )
            .into_element();
    }

    let (row_bg, sign, sign_color, text_color) = match line.kind {
        DiffLineKind::Added => (
            theme::diff_added_bg(),
            "+",
            theme::accent_success(),
            theme::foreground(),
        ),
        DiffLineKind::Removed => (
            theme::diff_removed_bg(),
            "-",
            theme::accent_red(),
            theme::foreground(),
        ),
        _ => (
            theme::background().opacity(0.0),
            " ",
            theme::text_muted(),
            theme::text_secondary(),
        ),
    };
    let old_label = line.old_line.map(|n| n.to_string()).unwrap_or_default();
    let new_label = line.new_line.map(|n| n.to_string()).unwrap_or_default();

    view()
        .w_full()
        .flex_row()
        .items_start()
        .bg(row_bg)
        .pr(px(theme::SPACING_MD))
        .child(
            text(old_label)
                .w(px(gutter_width))
                .flex_shrink_0()
                .pr(px(4.0))
                .text_right()
                .font_family(theme::TERMINAL_FONT_FAMILY)
                .text_size(px(font_size - 1.0))
                .line_height(row_lh)
                .text_color(move || theme::text_muted().opacity(0.6)),
        )
        .child(
            text(new_label)
                .w(px(gutter_width))
                .flex_shrink_0()
                .pr(px(6.0))
                .text_right()
                .font_family(theme::TERMINAL_FONT_FAMILY)
                .text_size(px(font_size - 1.0))
                .line_height(row_lh)
                .text_color(move || theme::text_muted().opacity(0.6)),
        )
        .child(
            text(sign)
                .w(px(14.0))
                .flex_shrink_0()
                .text_center()
                .font_family(theme::TERMINAL_FONT_FAMILY)
                .text_size(px(font_size))
                .font_weight(FontWeight::SEMIBOLD)
                .line_height(row_lh)
                .text_color(sign_color),
        )
        .child(
            text(line.text)
                .flex_shrink_0()
                .font_family(theme::TERMINAL_FONT_FAMILY)
                .text_size(px(font_size))
                .line_height(row_lh)
                .text_color(text_color),
        )
        .into_element()
}

fn diff_stat_counts(additions: u32, deletions: u32) -> impl IntoElement {
    view()
        .flex_row()
        .flex_shrink_0()
        .items_center()
        .gap(px(4.0))
        .children((additions > 0).then(|| {
            text(format!("+{additions}"))
                .font_family("monospace")
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::accent_success)
        }))
        .children((deletions > 0).then(|| {
            text(format!("-{deletions}"))
                .font_family("monospace")
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(theme::accent_red)
        }))
}

fn binary_diff_notice() -> impl IntoElement {
    view()
        .w_full()
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_MD))
        .child(
            text(copy::text(
                "Binary file changed",
                "二进制文件已变更",
                "二進位檔案已變更",
            ))
            .text_size(px(theme::font_caption()))
            .text_color(theme::text_muted),
        )
}

fn empty_diff_notice() -> impl IntoElement {
    view()
        .w_full()
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_XL))
        .items_center()
        .child(
            text(copy::text(
                "No diff content to display.",
                "暂无可显示的差异内容。",
                "暫無可顯示的差異內容。",
            ))
            .text_size(px(theme::font_caption()))
            .text_color(theme::text_muted),
        )
}

fn plain_fallback_block(raw: String) -> impl IntoElement {
    view().w_full().p(px(theme::SPACING_MD)).child(
        text(raw)
            .font_family(theme::TERMINAL_FONT_FAMILY)
            .text_size(px(theme::font_hint()))
            .line_height(theme::line_height_for(theme::font_hint()))
            .text_color(theme::text_secondary),
    )
}

fn split_display_path(path: &str) -> (String, String) {
    match path.rsplit_once('/') {
        Some((dir, name)) => (name.to_owned(), dir.to_owned()),
        None => (path.to_owned(), String::new()),
    }
}

fn diff_status_color(status: &str) -> Hsla {
    match status {
        "A" => theme::status_added(),
        "?" => theme::status_untracked(),
        "D" => theme::accent_red(),
        "M" | "R" | "C" => theme::status_modified(),
        _ => theme::text_muted(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_multi_file_unified_diff_with_line_numbers_and_strips_headers() {
        let patch = "\
diff --git a/crates/foo/src/lib.rs b/crates/foo/src/lib.rs
index 1111111..2222222 100644
--- a/crates/foo/src/lib.rs
+++ b/crates/foo/src/lib.rs
@@ -10,3 +10,4 @@ fn demo() {
 let a = 1;
-let b = 2;
+let b = 3;
+let c = 4;
diff --git a/assets/icon.png b/assets/icon.png
new file mode 100644
index 0000000..3333333
Binary files /dev/null and b/assets/icon.png differ
";
        let sections = parse_unified_diff(patch);
        assert_eq!(sections.len(), 2);

        let first = &sections[0];
        assert_eq!(first.path, "crates/foo/src/lib.rs");
        assert!(!first.binary);
        assert_eq!(first.additions, 2);
        assert_eq!(first.deletions, 1);
        assert_eq!(first.lines.len(), 5);
        assert_eq!(first.lines[0].kind, DiffLineKind::Hunk);
        assert_eq!(first.lines[1].old_line, Some(10));
        assert_eq!(first.lines[1].new_line, Some(10));
        assert_eq!(first.lines[1].text, "let a = 1;");
        assert_eq!(first.lines[2].kind, DiffLineKind::Removed);
        assert_eq!(first.lines[2].old_line, Some(11));
        assert_eq!(first.lines[2].new_line, None);
        assert_eq!(first.lines[3].kind, DiffLineKind::Added);
        assert_eq!(first.lines[3].old_line, None);
        assert_eq!(first.lines[3].new_line, Some(11));
        assert_eq!(first.lines[4].new_line, Some(12));

        let second = &sections[1];
        assert_eq!(second.path, "assets/icon.png");
        assert!(second.binary);
        assert!(second.lines.is_empty());
    }

    #[test]
    fn splits_raw_patch_by_file_path() {
        let patch = "\
diff --git a/a.rs b/a.rs
--- a/a.rs
+++ b/a.rs
@@ -1 +1 @@
-old
+new
diff --git a/b.rs b/b.rs
--- a/b.rs
+++ b/b.rs
@@ -1 +1 @@
-x
+y
";
        let parts = split_raw_patch_by_file(patch);
        assert_eq!(parts.len(), 2);
        assert_eq!(parts[0].0, "a.rs");
        assert!(parts[0].1.contains("+new"));
        assert_eq!(parts[1].0, "b.rs");
        assert!(parts[1].1.contains("+y"));
    }
}

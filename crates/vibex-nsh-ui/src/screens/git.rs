//! A read-only Git surface over the workspace.
//!
//! The runtime owns the repository; this lists its status (`git_status`) and
//! shows a file's diff (`git_diff`). Staging, committing and worktrees stay on
//! the desktop for now.

use std::rc::Rc;

use nsh_base::{ButtonElement, button};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{
    copy, theme,
    ui::{action, field},
};

/// Opens a path's diff, staged or not.
type OpenDiff = Rc<dyn Fn(&str, bool)>;

/// One changed path in the status list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeRow {
    pub path: String,
    /// The one-letter kind ("M", "A", "D", "R", "?").
    pub status: String,
    pub staged: bool,
    pub additions: u32,
    pub deletions: u32,
}

/// One commit in the history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRow {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub detail: String,
}

/// An opened diff.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitDiffPreview {
    pub title: String,
    pub body: String,
    pub truncated: bool,
    /// The path it belongs to, for reverting it.
    pub path: String,
    /// Whether it is the staged version.
    pub staged: bool,
}

/// An opened commit.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitDetailPreview {
    pub title: String,
    pub body: String,
    pub truncated: bool,
}

/// What the surface may do, and the state it shows.
#[derive(Clone)]
pub struct GitHost {
    pub open: Signal<bool>,
    pub branch: Signal<Option<SharedString>>,
    pub changes: Signal<Vec<GitChangeRow>>,
    pub diff: Signal<Option<GitDiffPreview>>,
    pub error: Signal<Option<SharedString>>,
    /// How many paths are staged (the commit's precondition).
    pub staged: Signal<u32>,
    /// The commit message being written.
    pub message: Signal<String>,
    pub committing: Signal<bool>,
    /// Whether the next commit amends the last one, and whether it pushes after.
    pub amend: Signal<bool>,
    pub push_after: Signal<bool>,
    pub remote_busy: Signal<bool>,
    pub on_toggle_amend: Rc<dyn Fn()>,
    pub on_toggle_push_after: Rc<dyn Fn()>,
    pub on_push: Rc<dyn Fn()>,
    pub on_pull: Rc<dyn Fn()>,
    /// The commit history, on its own view.
    pub history: Signal<Vec<CommitRow>>,
    pub show_history: Signal<bool>,
    /// The commit being read, shown over the history.
    pub commit_detail: Signal<Option<CommitDetailPreview>>,
    pub on_open_commit: Rc<dyn Fn(&str)>,
    /// Reverts the path whose diff is open, after the app confirms it.
    pub on_revert: Rc<dyn Fn(&str)>,
    pub on_commit: Rc<dyn Fn()>,
    pub on_toggle_history: Rc<dyn Fn()>,
    /// Opens a path's diff; the second argument says whether it is the staged
    /// or the working-tree version.
    pub on_open_diff: OpenDiff,
    /// Stages an unstaged path.
    pub on_stage: Rc<dyn Fn(&str)>,
    /// Unstages a staged path.
    pub on_unstage: Rc<dyn Fn(&str)>,
    pub on_back: Rc<dyn Fn()>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_close: Rc<dyn Fn()>,
}

/// The Git surface.
pub fn git(host: GitHost) -> impl IntoElement {
    let branch = host.branch;
    let diff = host.diff;
    let changes = host.changes;
    let error = host.error;
    let on_refresh = host.on_refresh.clone();
    let show_history = host.show_history;
    let history_query = signal(String::new());
    let commit_detail = host.commit_detail;
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
                            let on_back = host.on_back.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/chevron-left.svg")),
                                copy::text("Back", "返回", "返回"),
                                move || on_back(),
                            )
                        })
                        .child(
                            text(move || {
                                if let Some(detail) = commit_detail.get() {
                                    return detail.title;
                                }
                                if show_history.get() {
                                    return copy::text("History", "历史", "歷史").to_owned();
                                }
                                match branch.get() {
                                    Some(branch) => branch.to_string(),
                                    None => copy::text("Changes", "变更", "變更").to_owned(),
                                }
                            })
                            .flex_1()
                            .truncate()
                            .text_size(px(theme::font_heading()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        )
                        .child({
                            let on_refresh = on_refresh.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/refresh.svg")),
                                copy::text("Refresh", "刷新", "重新整理"),
                                move || on_refresh(),
                            )
                        })
                        .child({
                            let on_toggle_history = host.on_toggle_history.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/history.svg")),
                                copy::text("History", "历史", "歷史"),
                                move || on_toggle_history(),
                            )
                        })
                        .child({
                            let on_close = host.on_close.clone();
                            action::icon_button(
                                icon(include_str!("../../assets/icons/x.svg")),
                                copy::text("Close", "关闭", "關閉"),
                                move || on_close(),
                            )
                        }),
                ),
        )
        .child(when(move || commit_detail.get().is_some(), {
            let detail = commit_detail;
            move || commit_detail_view(detail)
        }))
        .child(when(
            move || commit_detail.get().is_none() && show_history.get(),
            {
                let history = host.history;
                let on_open_commit = host.on_open_commit.clone();
                move || history_list(history, history_query, on_open_commit.clone())
            },
        ))
        .child(when(
            move || commit_detail.get().is_none() && !show_history.get() && diff.get().is_none(),
            {
                let on_toggle_amend = host.on_toggle_amend.clone();
                let on_toggle_push_after = host.on_toggle_push_after.clone();
                let on_push = host.on_push.clone();
                let on_pull = host.on_pull.clone();
                move || {
                    git_tools(
                        host.amend,
                        host.push_after,
                        host.remote_busy,
                        on_toggle_amend.clone(),
                        on_toggle_push_after.clone(),
                        on_push.clone(),
                        on_pull.clone(),
                    )
                }
            },
        ))
        .child(when(
            move || commit_detail.get().is_none() && !show_history.get() && error.get().is_some(),
            move || {
                text(move || error.get().unwrap_or_default())
                    .px(px(theme::SPACING_LG))
                    .py(px(theme::SPACING_SM))
                    .text_size(px(theme::font_caption()))
                    .text_color(theme::accent_red)
            },
        ))
        .child(when(
            move || commit_detail.get().is_none() && !show_history.get() && diff.get().is_none(),
            {
                let staged = host.staged;
                let message = host.message;
                let committing = host.committing;
                let on_commit = host.on_commit.clone();
                move || commit_bar(staged, message, committing, host.amend, on_commit.clone())
            },
        ))
        .child(when(
            move || commit_detail.get().is_none() && !show_history.get() && diff.get().is_some(),
            {
                let on_revert = host.on_revert.clone();
                move || diff_view(diff, on_revert.clone())
            },
        ))
        .child(when(
            move || commit_detail.get().is_none() && !show_history.get() && diff.get().is_none(),
            move || {
                change_list(
                    changes,
                    host.on_open_diff.clone(),
                    host.on_stage.clone(),
                    host.on_unstage.clone(),
                )
            },
        ))
}

fn change_list(
    changes: Signal<Vec<GitChangeRow>>,
    on_open_diff: OpenDiff,
    on_stage: Rc<dyn Fn(&str)>,
    on_unstage: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    view().flex_1().min_h(px(0.0)).child(
        virtual_list(
            move || changes.with(|changes| changes.len()),
            move |at: Position| {
                change_row(
                    at,
                    changes,
                    on_open_diff.clone(),
                    on_stage.clone(),
                    on_unstage.clone(),
                )
            },
        )
        .keys(move || {
            changes.with(|changes| {
                changes
                    .iter()
                    .map(|change| format!("{}:{}", change.path, change.staged))
                    .collect::<Vec<_>>()
            })
        })
        .estimated_item_size(px(48.0))
        .flex_1(),
    )
}

fn change_row(
    at: Position,
    changes: Signal<Vec<GitChangeRow>>,
    on_open_diff: OpenDiff,
    on_stage: Rc<dyn Fn(&str)>,
    on_unstage: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let change = computed(move || changes.with(|changes| changes.get(at.index()).cloned()));
    let opened = change;
    view()
        .w_full()
        .min_h(px(48.0))
        .flex_row()
        .items_center()
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .child(
            button()
                .flex_1()
                .min_h(px(48.0))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_LG))
                .active(|style| style.opacity(0.8))
                .on_press(move || {
                    let Some(change) = opened.get_untracked() else {
                        return;
                    };
                    on_open_diff(&change.path, change.staged);
                })
                .child(
                    text(move || {
                        change
                            .with(|change| change.as_ref().map(|change| change.status.clone()))
                            .unwrap_or_default()
                    })
                    .w(px(16.0))
                    .text_size(px(theme::font_micro()))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(move || {
                        change
                            .with(|change| {
                                change.as_ref().map(|change| status_color(&change.status))
                            })
                            .unwrap_or_else(theme::text_muted)
                    }),
                )
                .child(
                    text(move || {
                        change
                            .with(|change| change.as_ref().map(|change| change.path.clone()))
                            .unwrap_or_default()
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::font_detail()))
                    .text_color(theme::foreground),
                )
                .child(
                    text(move || {
                        change.with(|change| {
                            change.as_ref().map_or(String::new(), |change| {
                                format!("+{} −{}", change.additions, change.deletions)
                            })
                        })
                    })
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                )
                .child(when(
                    move || {
                        change.with(|change| change.as_ref().is_some_and(|change| change.staged))
                    },
                    || {
                        text("●")
                            .text_size(px(theme::font_micro()))
                            .text_color(theme::color_token("chart-1"))
                    },
                )),
        )
        .child({
            let staged = change;
            button()
                .min_h(px(theme::TOUCH_TARGET))
                .px(px(theme::SPACING_MD))
                .items_center()
                .justify_center()
                .active(|style| style.opacity(0.7))
                .on_press(move || {
                    let Some(change) = staged.get_untracked() else {
                        return;
                    };
                    if change.staged {
                        on_unstage(&change.path);
                    } else {
                        on_stage(&change.path);
                    }
                })
                .child(
                    text(move || {
                        if staged.with(|change| change.as_ref().is_some_and(|change| change.staged))
                        {
                            copy::text("Unstage", "取消暂存", "取消暫存")
                        } else {
                            copy::text("Stage", "暂存", "暫存")
                        }
                    })
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_secondary),
                )
        })
}

/// The message field and the Commit action.
fn git_tools(
    amend: Signal<bool>,
    push_after: Signal<bool>,
    busy: Signal<bool>,
    on_toggle_amend: Rc<dyn Fn()>,
    on_toggle_push_after: Rc<dyn Fn()>,
    on_push: Rc<dyn Fn()>,
    on_pull: Rc<dyn Fn()>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_XS))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .child(tool_button(
            || copy::text("Pull", "拉取", "拉取"),
            {
                let on_pull = on_pull.clone();
                move || on_pull()
            },
            move || busy.get(),
        ))
        .child(tool_button(
            || copy::text("Push", "推送", "推送"),
            {
                let on_push = on_push.clone();
                move || on_push()
            },
            move || busy.get(),
        ))
        .child(tool_button(
            move || {
                if amend.get() {
                    copy::text("Amending", "修补提交", "修補提交")
                } else {
                    copy::text("Amend", "修补提交", "修補提交")
                }
            },
            move || on_toggle_amend(),
            move || busy.get(),
        ))
        .child(tool_button(
            move || {
                if push_after.get() {
                    copy::text("Push after ✓", "提交后推送 ✓", "提交後推送 ✓")
                } else {
                    copy::text("Push after", "提交后推送", "提交後推送")
                }
            },
            move || on_toggle_push_after(),
            move || busy.get(),
        ))
}

fn tool_button(
    label: impl Fn() -> &'static str + 'static,
    on_press: impl Fn() + 'static,
    busy: impl Fn() -> bool + 'static,
) -> ButtonElement {
    button()
        .on_press(move || {
            if !busy() {
                on_press();
            }
        })
        .min_h(px(32.0))
        .px(px(theme::SPACING_SM))
        .items_center()
        .justify_center()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_dim)
        .active(|style| style.opacity(0.7))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
}

fn commit_bar(
    staged: Signal<u32>,
    message: Signal<String>,
    committing: Signal<bool>,
    amend: Signal<bool>,
    on_commit: Rc<dyn Fn()>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_row()
        .items_end()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .child(
            crate::ui::field::text_field(
                message,
                copy::text("Commit message", "提交说明", "提交說明"),
            )
            .flex_1(),
        )
        .child(
            action::primary(
                move || {
                    if amend.get() {
                        copy::text("Amend", "修补提交", "修補提交")
                    } else {
                        copy::text("Commit", "提交", "提交")
                    }
                },
                icon(include_str!("../../assets/icons/check.svg")),
                {
                    let on_commit = on_commit.clone();
                    move || on_commit()
                },
            )
            .disabled(move || {
                committing.get()
                    || (staged.with(|staged| *staged == 0) && !amend.get())
                    || message.with(|message| message.trim().is_empty())
            })
            .disabled_style(|style| style.opacity(0.4)),
        )
}

/// The history list.
fn history_list(
    history: Signal<Vec<CommitRow>>,
    query: Signal<String>,
    on_open_commit: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let filtered = computed(move || {
        query.with(|query| {
            let query = query.trim().to_lowercase();
            history.with(|history| {
                history
                    .iter()
                    .filter(|commit| {
                        query.is_empty()
                            || commit.subject.to_lowercase().contains(&query)
                            || commit.detail.to_lowercase().contains(&query)
                            || commit.short_hash.to_lowercase().contains(&query)
                            || commit.hash.to_lowercase().contains(&query)
                    })
                    .cloned()
                    .collect::<Vec<_>>()
            })
        })
    });
    view()
        .flex_1()
        .min_h(px(0.0))
        .flex_col()
        .child(
            view()
                .px(px(theme::SPACING_LG))
                .py(px(theme::SPACING_SM))
                .child(field::text_field(
                    query,
                    copy::text("Search commits", "搜索提交", "搜尋提交"),
                )),
        )
        .child(
            virtual_list(
                move || filtered.with(|filtered| filtered.len()),
                move |at: Position| history_row(at, filtered, on_open_commit.clone()),
            )
            .keys(move || {
                filtered.with(|filtered| {
                    filtered
                        .iter()
                        .map(|commit| commit.hash.clone())
                        .collect::<Vec<_>>()
                })
            })
            .estimated_item_size(px(56.0))
            .flex_1(),
        )
}

fn history_row(
    at: Position,
    history: Memo<Vec<CommitRow>>,
    on_open_commit: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let commit = computed(move || history.with(|history| history.get(at.index()).cloned()));
    button()
        .w_full()
        .min_h(px(56.0))
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_XS))
        .flex_col()
        .items_start()
        .justify_center()
        .gap(px(2.0))
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .active(|style| style.opacity(0.8))
        .on_press(move || {
            if let Some(commit) = commit.get_untracked() {
                on_open_commit(&commit.hash);
            }
        })
        .child(
            text(move || {
                commit
                    .with(|commit| commit.as_ref().map(|commit| commit.subject.clone()))
                    .unwrap_or_default()
            })
            .truncate()
            .text_size(px(theme::font_detail()))
            .text_color(theme::foreground),
        )
        .child(
            text(move || {
                commit.with(|commit| {
                    commit.as_ref().map_or(String::new(), |commit| {
                        format!("{} · {}", commit.short_hash, commit.detail)
                    })
                })
            })
            .truncate()
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted),
        )
}

/// The diff with the action that discards it.
/// A long text, one virtualized row per line, so it scrolls.
fn line_list(lines: Memo<Vec<String>>) -> impl IntoElement {
    view().flex_1().min_h(px(0.0)).child(
        virtual_list(
            move || lines.with(|lines| lines.len()),
            move |at: Position| {
                let line = computed(move || {
                    lines.with(|lines| lines.get(at.index()).cloned().unwrap_or_default())
                });
                view().px(px(theme::SPACING_LG)).child(
                    text(move || line.get())
                        .text_size(px(theme::font_micro()))
                        .line_height(theme::line_height_for(theme::font_micro()))
                        .text_color(theme::foreground),
                )
            },
        )
        .keys(move || (0..lines.with(|lines| lines.len())).collect::<Vec<_>>())
        .estimated_item_size(px(16.0))
        .flex_1(),
    )
}

fn diff_view(
    diff: Signal<Option<GitDiffPreview>>,
    on_revert: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let body = computed(move || {
        diff.with(|diff| {
            diff.as_ref()
                .map(|diff| diff.body.clone())
                .unwrap_or_default()
        })
    });
    let lines = computed(move || body.get().lines().map(str::to_owned).collect::<Vec<_>>());
    let title = computed(move || {
        diff.with(|diff| {
            diff.as_ref()
                .map(|diff| diff.title.clone())
                .unwrap_or_default()
        })
    });
    let truncated =
        computed(move || diff.with(|diff| diff.as_ref().is_some_and(|diff| diff.truncated)));
    let path = computed(move || {
        diff.with(|diff| {
            diff.as_ref()
                .map(|diff| diff.path.clone())
                .unwrap_or_default()
        })
    });
    let staged = computed(move || diff.with(|diff| diff.as_ref().is_some_and(|diff| diff.staged)));
    view()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::SPACING_XS))
        .py(px(theme::SPACING_SM))
        .child(
            text(move || title.get())
                .px(px(theme::SPACING_LG))
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted),
        )
        .child(line_list(lines))
        .child(when(move || truncated.get(), || {
            text(copy::text(
                "The diff is larger than the preview limit.",
                "差异超过预览上限。",
                "差異超過預覽上限。",
            ))
            .text_size(px(theme::font_micro()))
            .text_color(theme::text_muted)
        }))
        .child(when(move || !staged.get(), {
            let on_revert = on_revert.clone();
            move || {
                let path = path.get_untracked();
                let on_revert = on_revert.clone();
                button()
                    .w_full()
                    .min_h(px(theme::TOUCH_TARGET))
                    .justify_center()
                    .items_center()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::accent_red)
                    .active(|style| style.opacity(0.7))
                    .on_press(move || {
                        let path = path.clone();
                        let on_revert = on_revert.clone();
                        spawn(async move {
                            let answer = nsh_base::alert_dialog()
                                .title(copy::text(
                                    "Revert this file?",
                                    "还原这个文件？",
                                    "還原這個檔案？",
                                ))
                                .description(copy::text(
                                    "The working-tree changes to this path are discarded.",
                                    "该路径在工作区的改动会被丢弃。",
                                    "該路徑在工作區的改動會被丟棄。",
                                ))
                                .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                                .destructive("revert", copy::text("Revert", "还原", "還原"))
                                .show()
                                .await;
                            if matches!(&answer, Ok(answer) if answer.action.as_deref() == Some("revert"))
                            {
                                on_revert(&path);
                            }
                        });
                    })
                    .child(
                        text(copy::text("Revert changes", "还原改动", "還原改動"))
                            .text_size(px(theme::font_detail()))
                            .text_color(theme::accent_red),
                    )
            }
        }))
}

/// One commit's message and patch.
fn commit_detail_view(detail: Signal<Option<CommitDetailPreview>>) -> impl IntoElement {
    let body = computed(move || {
        detail.with(|detail| {
            detail
                .as_ref()
                .map(|detail| detail.body.clone())
                .unwrap_or_default()
        })
    });
    let truncated = computed(move || {
        detail.with(|detail| detail.as_ref().is_some_and(|detail| detail.truncated))
    });
    let lines = computed(move || body.get().lines().map(str::to_owned).collect::<Vec<_>>());
    view()
        .flex_1()
        .min_h(px(0.0))
        .gap(px(theme::SPACING_XS))
        .py(px(theme::SPACING_SM))
        .child(line_list(lines))
        .child(when(
            move || truncated.get(),
            || {
                text(copy::text(
                    "The patch is larger than the preview limit.",
                    "补丁超过预览上限。",
                    "補丁超過預覽上限。",
                ))
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_muted)
            },
        ))
}

fn status_color(status: &str) -> Hsla {
    match status {
        "A" | "?" => theme::color_token("chart-1"),
        "D" => theme::accent_red(),
        "M" | "R" => theme::color_token("warning"),
        _ => theme::text_muted(),
    }
}

fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

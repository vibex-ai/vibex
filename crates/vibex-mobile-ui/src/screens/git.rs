//! Changes, commits, and worktree controls for the runtime's repository.

use std::rc::Rc;

use nsh_base::{ButtonElement, button};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{
    copy, theme,
    ui::{action, field, surface},
};

/// Opens a path's diff, staged or not.
type OpenDiff = Rc<dyn Fn(&str, bool)>;
pub type ResolveWorktree = Rc<dyn Fn(&str, &str, bool)>;
type ToggleDirectory = Rc<dyn Fn(&[String])>;

/// One local branch in the branch sheet.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BranchRow {
    pub name: String,
    pub current: bool,
    pub detail: String,
    pub ahead: u32,
    pub behind: u32,
}

/// One managed worktree.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeRow {
    pub id: String,
    pub branch: String,
    pub path: String,
    pub status: String,
    pub can_archive: bool,
    pub can_restore: bool,
    pub can_merge: bool,
    pub can_discard: bool,
}

/// One unresolved conflict.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeConflictRow {
    pub path: String,
}

/// One merge/operation needing attention.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorktreeOperationRow {
    pub id: String,
    pub label: String,
    pub status: String,
    pub can_continue: bool,
    pub can_abort: bool,
    pub conflicts: Vec<WorktreeConflictRow>,
}

/// One changed path in the status list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct GitChangeRow {
    pub path: String,
    pub name: String,
    pub directory: bool,
    pub depth: usize,
    pub expanded: bool,
    pub segments: Vec<String>,
    pub selection: PathSelection,
    /// The one-letter kind ("M", "A", "D", "R", "?").
    pub status: String,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum PathSelection {
    #[default]
    Unchecked,
    Partial,
    Checked,
}

/// One commit in the history list.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct CommitRow {
    pub hash: String,
    pub short_hash: String,
    pub subject: String,
    pub detail: String,
    /// When it was authored, for the time shown beside it.
    pub authored_at_ms: Option<i64>,
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
    /// The raw message and patch, kept as the fallback for a runtime that
    /// reports no per-file changes.
    pub body: String,
    pub truncated: bool,
    /// The commit's first line.
    pub subject: String,
    /// The rest of its message, when it has one.
    pub message_body: String,
    pub author: String,
    /// When it was authored, already written for the reader.
    pub authored_at: String,
    /// One entry per changed file, each with its own patch slice.
    pub files: Vec<super::git_diff::CommitFileDiff>,
}

/// A Git remote or revert operation running in the background.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GitRemoteActivity {
    Fetch,
    Pull,
    Push,
    Revert,
}

impl GitRemoteActivity {
    fn spinner_test_id(self) -> &'static str {
        match self {
            Self::Fetch => "git-remote-spinner-fetch",
            Self::Pull => "git-remote-spinner-pull",
            Self::Push => "git-remote-spinner-push",
            Self::Revert => "git-remote-spinner-revert",
        }
    }
}

/// What the surface may do, and the state it shows.
#[derive(Clone)]
pub struct GitHost {
    pub active_session: Signal<Option<String>>,
    pub branch: Signal<Option<SharedString>>,
    pub totals: Signal<(u32, u32)>,
    pub changes: Signal<Vec<GitChangeRow>>,
    pub diff: Signal<Option<GitDiffPreview>>,
    pub error: Signal<Option<SharedString>>,
    pub selected: Signal<Vec<String>>,
    pub on_toggle_selection: Rc<dyn Fn(&str)>,
    pub on_toggle_directory: ToggleDirectory,
    pub on_toggle_directories: Rc<dyn Fn()>,
    pub on_revert_selection: ToggleDirectory,
    /// The commit message being written.
    pub message: Signal<String>,
    pub committing: Signal<bool>,
    /// Whether the next commit amends the last one, and whether it pushes after.
    pub amend: Signal<bool>,
    pub push_after: Signal<bool>,
    /// Which remote/revert action is currently running, if any.
    pub remote_busy: Signal<Option<GitRemoteActivity>>,
    pub branches: Signal<Vec<BranchRow>>,
    pub branch_menu: Signal<bool>,
    pub on_open_branches: Rc<dyn Fn()>,
    pub on_close_branches: Rc<dyn Fn()>,
    pub on_select_branch: Rc<dyn Fn(&str)>,
    pub worktrees: Signal<Vec<WorktreeRow>>,
    pub worktree_operations: Signal<Vec<WorktreeOperationRow>>,
    pub worktree_menu: Signal<bool>,
    pub worktree_busy: Signal<bool>,
    pub worktree_error: Signal<Option<SharedString>>,
    pub on_open_worktrees: Rc<dyn Fn()>,
    pub on_close_worktrees: Rc<dyn Fn()>,
    pub on_archive_worktree: Rc<dyn Fn(&str)>,
    pub on_merge_worktree: Rc<dyn Fn(&str)>,
    pub on_discard_worktree: Rc<dyn Fn(&str)>,
    pub on_continue_worktree: Rc<dyn Fn(&str)>,
    pub on_abort_worktree: Rc<dyn Fn(&str)>,
    pub on_resolve_worktree: ResolveWorktree,
    pub on_restore_worktree: Rc<dyn Fn(&str)>,
    pub on_toggle_amend: Rc<dyn Fn()>,
    pub on_toggle_push_after: Rc<dyn Fn()>,
    pub on_push: Rc<dyn Fn()>,
    pub on_pull: Rc<dyn Fn()>,
    pub on_fetch: Rc<dyn Fn()>,
    /// The commit history, on its own view.
    pub history: Signal<Vec<CommitRow>>,
    pub show_history: Signal<bool>,
    /// Whether more pages of the history exist, and one is being read.
    pub history_more: Signal<bool>,
    pub history_loading: Signal<bool>,
    /// The commit search, answered against the whole history.
    pub history_query: Signal<String>,
    /// The commit being read, shown over the history.
    pub commit_detail: Signal<Option<CommitDetailPreview>>,
    pub on_open_commit: Rc<dyn Fn(&str)>,
    /// Reverts the path whose diff is open, after the app confirms it.
    pub on_revert: Rc<dyn Fn(&str)>,
    pub on_commit: Rc<dyn Fn()>,
    pub on_toggle_history: Rc<dyn Fn()>,
    /// Reads the next page of the history, and searches all of it.
    pub on_load_more_history: Rc<dyn Fn()>,
    pub on_search_history: Rc<dyn Fn(&str)>,
    /// Opens a path's diff; the second argument says whether it is the staged
    /// or the working-tree version.
    pub on_open_diff: OpenDiff,
    /// Stages an unstaged path.
    pub on_stage: Rc<dyn Fn(&str)>,
    /// Unstages a staged path.
    pub on_unstage: Rc<dyn Fn(&str)>,
    pub on_back: Rc<dyn Fn()>,
    pub on_refresh: Rc<dyn Fn()>,
}

/// The Git surface.
pub fn git(host: GitHost) -> impl IntoElement {
    let diff = host.diff;
    let commit = host.commit_detail;
    let details = move || diff.with(Option::is_some) || commit.with(Option::is_some);
    let detail_host = host.clone();
    let overview = git_overview(host.clone());
    view()
        .flex_1()
        .min_h(px(0.0))
        .pb_keyboard()
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .style_when(details, |style| style.hidden())
                .child(overview),
        )
        .child(when(details, move || {
            let back = detail_host.on_back.clone();
            let revert = detail_host.on_revert.clone();
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(
                    view()
                        .min_h(px(theme::TOUCH_TARGET))
                        .flex_shrink_0()
                        .flex_row()
                        .items_center()
                        .gap(px(theme::SPACING_SM))
                        .px(px(theme::SPACING_MD))
                        .pb(px(theme::SPACING_SM))
                        .child(surface::round_button(
                            icon!("chevron-left"),
                            copy::text("Back", "返回", "返回"),
                            move || back(),
                        ))
                        .child(
                            text(move || {
                                diff.with(|diff| diff.as_ref().map(|value| value.title.clone()))
                                    .or_else(|| {
                                        commit.with(|commit| {
                                            commit.as_ref().map(|value| value.title.clone())
                                        })
                                    })
                                    .unwrap_or_default()
                            })
                            .flex_1()
                            .min_w(px(0.0))
                            .truncate()
                            .text_size(px(theme::font_detail()))
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_color(theme::foreground),
                        ),
                )
                .child(
                    when(
                        move || diff.with(Option::is_some),
                        move || diff_view(diff, revert.clone()),
                    )
                    .otherwise(move || commit_detail_view(commit)),
                )
        }))
        // `pb_keyboard` covers the keyboard while it is up; with it down the
        // surface still has to clear the home indicator.
        .child(when(
            move || keyboard_height().0 == 0.0,
            || view().pb_safe_area().flex_shrink_0(),
        ))
        .child(branch_sheet(
            host.branch_menu,
            host.branches,
            host.on_close_branches,
            host.on_select_branch,
        ))
        .child(worktree_sheet(
            host.worktree_menu,
            host.worktrees,
            host.worktree_operations,
            host.worktree_busy,
            host.worktree_error,
            host.on_close_worktrees,
            host.on_archive_worktree,
            host.on_merge_worktree,
            host.on_discard_worktree,
            host.on_continue_worktree,
            host.on_abort_worktree,
            host.on_resolve_worktree,
            host.on_restore_worktree,
        ))
}

fn git_overview(host: GitHost) -> impl IntoElement {
    let history = host.show_history;
    let query = signal(String::new());
    let branch = host.branch;
    let totals = host.totals;
    let busy = host.remote_busy;
    let selected = host.selected;
    let active_session = host.active_session;
    let error = host.error;
    let history_host = host.clone();
    let change_host = host.clone();
    let search_host = host.clone();
    let mode = |commits: bool, toggle: Rc<dyn Fn()>| {
        move || {
            if history.get_untracked() != commits {
                dismiss_keyboard();
                toggle();
            }
        }
    };
    view()
        .flex_1()
        .min_h(px(0.0))
        // Where the work stands: the branch and what it differs by, or the
        // search of its commits, and the three ways it meets its remote.
        .child(
            view()
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .child(
                    view().flex_1().min_w(px(0.0)).child(
                        when(
                            move || history.get(),
                            move || {
                                let search = search_host.on_search_history.clone();
                                let clear = search.clone();
                                surface::field_pill()
                                    .w_full()
                                    .child(surface::field_glyph(icon!("search")))
                                    .child(
                                        // The runtime filters the whole
                                        // history, not the page already read,
                                        // so a commit older than the first page
                                        // is found too.
                                        field::text_field_bare_with(
                                            query,
                                            copy::text("Search commits", "搜索提交", "搜尋提交"),
                                            move |value| search(value),
                                        )
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .h_full(),
                                    )
                                    .child(when(
                                        move || !query.with(String::is_empty),
                                        move || {
                                            let clear = clear.clone();
                                            surface::field_clear(
                                                copy::text(
                                                    "Clear commit search",
                                                    "清除提交搜索",
                                                    "清除提交搜尋",
                                                ),
                                                move || {
                                                    query.set(String::new());
                                                    clear("");
                                                },
                                            )
                                        },
                                    ))
                            },
                        )
                        .otherwise({
                            let branches = host.on_open_branches.clone();
                            move || {
                                let branches = branches.clone();
                                button()
                                    .w_full()
                                    .h(px(theme::ROUND_CONTROL))
                                    .pl(px(theme::SPACING_MD))
                                    .pr(px(theme::SPACING_MD))
                                    .flex_row()
                                    .items_center()
                                    .gap(px(theme::SPACING_SM))
                                    .rounded(px(theme::RADIUS_CHIP))
                                    .bg(theme::fill)
                                    .active(|style| style.opacity(0.6))
                                    .accessibility_label(copy::text("Branches", "分支", "分支"))
                                    .on_press(move || branches())
                                    .child(surface::field_glyph(icon!("git-branch")))
                                    .child(
                                        text(move || {
                                            branch.get().unwrap_or_else(|| {
                                                copy::text("No repository", "没有仓库", "沒有儲存庫")
                                                    .into()
                                            })
                                        })
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(theme::font_detail()))
                                        .font_weight(FontWeight::MEDIUM)
                                        .text_color(theme::foreground),
                                    )
                                    .child(when(
                                        move || totals.get().0 > 0,
                                        move || {
                                            count(
                                                move || format!("+{}", totals.get().0),
                                                theme::accent_success,
                                            )
                                        },
                                    ))
                                    .child(when(
                                        move || totals.get().1 > 0,
                                        move || {
                                            count(
                                                move || format!("-{}", totals.get().1),
                                                theme::accent_red,
                                            )
                                        },
                                    ))
                                    .child(
                                        svg(icon!("chevron-down"))
                                            .size(px(12.0))
                                            .flex_shrink_0()
                                            .text_color(theme::text_muted),
                                    )
                            }
                        }),
                    ),
                )
                .child(remote_button(
                    GitRemoteActivity::Fetch,
                    icon!("refresh"),
                    copy::text("Fetch", "获取", "擷取"),
                    busy,
                    host.on_fetch.clone(),
                ))
                .child(remote_button(
                    GitRemoteActivity::Pull,
                    icon!("download"),
                    copy::text("Pull", "拉取", "拉取"),
                    busy,
                    host.on_pull.clone(),
                ))
                .child(remote_button(
                    GitRemoteActivity::Push,
                    icon!("upload"),
                    copy::text("Push", "推送", "推送"),
                    busy,
                    host.on_push.clone(),
                )),
        )
        // What is being read, the working tree or its history, and what can
        // be done to the changes as a whole.
        .child(
            view()
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .pl(px(theme::SPACING_MD))
                .pr(px(theme::SPACING_SM))
                .py(px(theme::SPACING_SM))
                .child(
                    surface::segment_track()
                        .w(px(176.0))
                        .flex_shrink_0()
                        .child(surface::segment(
                            copy::text("Changes", "更改", "變更"),
                            move || !history.get(),
                            mode(false, host.on_toggle_history.clone()),
                        ))
                        .child(surface::segment(
                            copy::text("Commits", "提交", "提交"),
                            move || history.get(),
                            mode(true, host.on_toggle_history),
                        )),
                )
                .child(view().flex_1().min_w(px(0.0)))
                .child(when(move || !history.get(), {
                    let select = host.on_toggle_selection.clone();
                    let revert = host.on_revert_selection.clone();
                    let dirs = host.on_toggle_directories.clone();
                    move || {
                        let select = select.clone();
                        let revert = revert.clone();
                        let dirs = dirs.clone();
                        view()
                            .flex_row()
                            .items_center()
                            .child(surface::ghost_button(
                                icon!("list-checks"),
                                copy::text("Select all changes", "选择全部变更", "選取全部變更"),
                                move || select(""),
                            ))
                            .child(
                                surface::ghost_button(
                                    icon!("undo"),
                                    copy::text(
                                        "Rollback selected",
                                        "回滚所选变更",
                                        "復原已選變更",
                                    ),
                                    move || {
                                        let revert = revert.clone();
                                        let paths = selected.get_untracked();
                                        let owner = active_session.get_untracked();
                                        spawn(async move {
                                            let result = nsh_base::alert_dialog()
                                                .title(copy::text(
                                                    "Rollback selected changes?",
                                                    "回滚所选变更？",
                                                    "復原已選變更？",
                                                ))
                                                .description(copy::text(
                                                    "Changes to the selected paths will be discarded.",
                                                    "所选路径的改动将被丢弃。",
                                                    "已選路徑的變更將被捨棄。",
                                                ))
                                                .cancel(
                                                    "cancel",
                                                    copy::text("Cancel", "取消", "取消"),
                                                )
                                                .destructive(
                                                    "revert",
                                                    copy::text("Rollback", "回滚", "復原"),
                                                )
                                                .show()
                                                .await;
                                            if matches!(result, Ok(answer) if answer.action.as_deref() == Some("revert"))
                                                && active_session.get_untracked() == owner
                                            {
                                                revert(&paths);
                                            }
                                        });
                                    },
                                )
                                .disabled(move || {
                                    selected.with(Vec::is_empty) || busy.with(Option::is_some)
                                }),
                            )
                            .child(surface::ghost_button(
                                icon!("chevrons-down-up"),
                                copy::text(
                                    "Expand or collapse all",
                                    "展开或折叠目录",
                                    "展開或摺疊資料夾",
                                ),
                                move || dirs(),
                            ))
                    }
                }))
                .child(surface::ghost_button(
                    icon!("git-branch"),
                    copy::text("Worktrees", "Worktree", "Worktree"),
                    move || (host.on_open_worktrees)(),
                )),
        )
        .child(when(
            move || error.with(Option::is_some),
            move || {
                let refresh = host.on_refresh.clone();
                view()
                    .flex_shrink_0()
                    .mx(px(theme::SPACING_MD))
                    .mb(px(theme::SPACING_SM))
                    .pl(px(theme::SPACING_MD))
                    .pr(px(theme::SPACING_XS))
                    .min_h(px(theme::ROUND_CONTROL))
                    .flex_row()
                    .items_center()
                    .gap(px(theme::SPACING_SM))
                    .rounded(px(theme::RADIUS_CHIP))
                    .bg(move || theme::tint(theme::accent_red()))
                    .child(
                        text(move || error.get().unwrap_or_default())
                            .flex_1()
                            .min_w(px(0.0))
                            .py(px(theme::SPACING_SM))
                            .text_size(px(theme::font_caption()))
                            .text_color(theme::accent_red),
                    )
                    .child(surface::ghost_button(
                        icon!("refresh"),
                        copy::text("Refresh", "刷新", "重新整理"),
                        move || refresh(),
                    ))
            },
        ))
        .child(
            when(
                move || history.get(),
                move || {
                    let history_host = history_host.clone();
                    history_list(
                        history_host.history,
                        history_host.history_more,
                        history_host.history_loading,
                        history_host.on_open_commit.clone(),
                        history_host.on_load_more_history.clone(),
                    )
                },
            )
            .otherwise(move || {
                view()
                    .flex_1()
                    .min_h(px(0.0))
                    .child(change_list(change_host.clone()))
                    .child(commit_bar(change_host.clone()))
            }),
        )
}

/// One of the three ways the branch meets its remote: only the pressed button
/// swaps its glyph for a rotating spinner while running; the sibling buttons
/// show a normal disabled state without spinning.
fn remote_button(
    kind: GitRemoteActivity,
    glyph: ImageSrc,
    label: &'static str,
    busy: Signal<Option<GitRemoteActivity>>,
    on_press: Rc<dyn Fn()>,
) -> impl IntoElement {
    let active = move || busy.get() == Some(kind);
    let any_busy = move || busy.with(Option::is_some);
    let spinner_id = kind.spinner_test_id();
    button()
        .accessibility_label(label)
        .on_press(move || on_press())
        .busy(active)
        .disabled(any_busy)
        .size(px(theme::COMPACT_CONTROL))
        .flex_shrink_0()
        .rounded_full()
        .items_center()
        .justify_center()
        .hit_slop(px(5.0))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .style_when(move || any_busy() && !active(), |style| style.opacity(0.4))
        .child(
            when(active, move || {
                crate::ui::activity::spinner(theme::ICON_MD, theme::text_secondary)
                    .test_id(spinner_id)
            })
            .otherwise(move || {
                svg(glyph.clone())
                    .size(px(theme::ICON_MD))
                    .text_color(theme::text_secondary)
            }),
        )
}

/// A count of lines in the color of the side it counts.
fn count(value: impl Fn() -> String + 'static, color: fn() -> Hsla) -> impl IntoElement {
    text(value)
        .flex_shrink_0()
        .font_family("monospace")
        .text_size(px(theme::font_micro()))
        .text_color(color)
}

/// One command of a worktree or of an operation on one, written out.
fn worktree_action(
    label: &'static str,
    danger: bool,
    busy: Signal<bool>,
    on_press: impl Fn() + 'static,
) -> ButtonElement {
    let ink: fn() -> Hsla = if danger {
        theme::accent_red
    } else {
        theme::text_secondary
    };
    button()
        .on_press(move || {
            if !busy.get_untracked() {
                on_press();
            }
        })
        .h(px(theme::CHIP_HEIGHT))
        .px(px(theme::SPACING_MD))
        .flex_shrink_0()
        .items_center()
        .justify_center()
        .rounded_full()
        .bg(theme::fill)
        .active(|style| style.opacity(0.6))
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .font_weight(FontWeight::MEDIUM)
                .text_color(ink),
        )
}

/// The note under a patch that was cut to the preview's limit.
fn preview_limit(message: &'static str) -> impl IntoElement {
    text(message)
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .text_size(px(theme::font_micro()))
        .text_color(theme::text_muted)
}

fn change_list(host: GitHost) -> impl IntoElement {
    let changes = host.changes;
    view().flex_1().min_h(px(0.0)).child(
        when(
            move || changes.with(Vec::is_empty),
            || {
                surface::empty_state(
                    icon!("check"),
                    copy::text("No changes", "没有变更", "沒有變更"),
                    copy::text(
                        "The working tree matches the last commit.",
                        "工作区与最近一次提交一致。",
                        "工作區與最近一次提交一致。",
                    ),
                )
            },
        )
        .otherwise(move || {
            let host = host.clone();
            virtual_list(
                move || changes.with(Vec::len),
                move |at: Position| {
                    let change =
                        computed(move || changes.with(|changes| changes.get(at.index()).cloned()));
                    let host = host.clone();
                    dynamic(move || {
                        change.get().map_or_else(
                            || view().into_element(),
                            |row| change_row(row, host.clone()).into_element(),
                        )
                    })
                },
            )
            .keys(move || {
                changes.with(|changes| {
                    changes
                        .iter()
                        .map(|change| change.path.clone())
                        .collect::<Vec<_>>()
                })
            })
            .estimated_item_size(px(theme::TOUCH_TARGET))
            .flex_1()
        }),
    )
}

fn change_row(row: GitChangeRow, host: GitHost) -> impl IntoElement {
    let path = row.path.clone();
    let color = status_color(&row.status);
    let action_path = path.clone();
    let selection_path = path.clone();
    let selection = row.selection;
    let stage_path = path.clone();
    let stage = if row.staged {
        host.on_unstage
    } else {
        host.on_stage
    };
    view()
        .h(px(theme::TOUCH_TARGET))
        .mx(px(theme::SPACING_SM))
        .pl(px(theme::SPACING_XS))
        .pr(px(2.0))
        .flex_row()
        .items_center()
        .gap(px(2.0))
        .rounded(px(theme::RADIUS_CHIP))
        .child(view().w(px(row.depth.min(8) as f32 * 16.0)).flex_shrink_0())
        .child(
            button()
                .w(px(34.0))
                .h_full()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .accessibility_label(format!(
                    "{} {}",
                    copy::text("Select", "选择", "選取"),
                    selection_path
                ))
                .active(|style| style.opacity(0.6))
                .on_press(move || (host.on_toggle_selection)(&selection_path))
                .child(surface::check_mark(move || match selection {
                    PathSelection::Unchecked => None,
                    PathSelection::Partial => Some(false),
                    PathSelection::Checked => Some(true),
                })),
        )
        .child(
            button()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .px(px(theme::SPACING_XS))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .rounded(px(theme::RADIUS_CONTROL))
                .accessibility_label(path)
                .active(|style| style.bg(theme::row_pressed_bg()))
                .on_press(move || {
                    if row.directory {
                        (host.on_toggle_directory)(&row.segments);
                    } else {
                        (host.on_open_diff)(&action_path, !row.unstaged && row.staged);
                    }
                })
                .child(
                    svg(if row.directory {
                        if row.expanded {
                            icon!("folder-open")
                        } else {
                            icon!("folder")
                        }
                    } else {
                        icon!("file-code")
                    })
                    .size(px(theme::ICON_SM))
                    .flex_shrink_0()
                    .text_color(theme::text_muted),
                )
                .child(
                    text(row.name)
                        .flex_1()
                        .min_w(px(0.0))
                        .truncate()
                        .text_size(px(theme::font_detail()))
                        .text_color(if row.directory {
                            theme::text_secondary
                        } else {
                            theme::foreground
                        }),
                )
                .children((!row.directory).then(|| {
                    view()
                        .flex_row()
                        .items_center()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .children((row.additions > 0).then(|| {
                            text(format!("+{}", row.additions))
                                .font_family("monospace")
                                .text_size(px(theme::font_hint()))
                                .text_color(theme::accent_success)
                        }))
                        .children((row.deletions > 0).then(|| {
                            text(format!("-{}", row.deletions))
                                .font_family("monospace")
                                .text_size(px(theme::font_hint()))
                                .text_color(theme::accent_red)
                        }))
                        .child(
                            view()
                                .size(px(20.0))
                                .rounded(px(6.0))
                                .bg(theme::tint(color))
                                .items_center()
                                .justify_center()
                                .child(
                                    text(row.status)
                                        .font_family("monospace")
                                        .text_size(px(theme::font_hint()))
                                        .font_weight(FontWeight::SEMIBOLD)
                                        .text_color(color),
                                ),
                        )
                })),
        )
        .children((!row.directory).then(|| {
            surface::ghost_button(
                if row.staged {
                    icon!("minus")
                } else {
                    icon!("plus")
                },
                if row.staged {
                    copy::text("Unstage", "取消暂存", "取消暫存")
                } else {
                    copy::text("Stage", "暂存", "暫存")
                },
                move || stage(&stage_path),
            )
        }))
}

/// What is written about the selected changes, and the action that commits
/// them. It stands at the foot of the list, over the keyboard.
fn commit_bar(host: GitHost) -> impl IntoElement {
    let selected = host.selected;
    let message = host.message;
    let amend = host.amend;
    let push = host.push_after;
    let committing = host.committing;
    view()
        .w_full()
        .flex_shrink_0()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_MD))
        .pt(px(theme::SPACING_SM))
        .pb(px(theme::SPACING_SM))
        .border_t_1()
        .border_t_color(theme::hairline)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || {
                        format!(
                            "{} {}",
                            selected.with(Vec::len),
                            copy::text("selected", "已选择", "已選取")
                        )
                    })
                    .flex_1()
                    .min_w(px(0.0))
                    .truncate()
                    .pl(px(theme::SPACING_XS))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                )
                .child(surface::toggle_pill(
                    icon!("rotate-ccw"),
                    copy::text("Amend", "修补提交", "修補提交"),
                    move || amend.get(),
                    move || (host.on_toggle_amend)(),
                ))
                .child(surface::toggle_pill(
                    icon!("upload"),
                    copy::text("Push after", "提交后推送", "提交後推送"),
                    move || push.get(),
                    move || (host.on_toggle_push_after)(),
                )),
        )
        .child(
            view()
                .flex_row()
                .gap(px(theme::SPACING_SM))
                .items_center()
                .child(
                    field::text_field_well(
                        message,
                        copy::text("Commit message", "提交说明", "提交說明"),
                    )
                    .h(px(theme::TOUCH_TARGET))
                    .flex_1()
                    .min_w(px(0.0)),
                )
                .child(
                    action::solid(
                        move || {
                            SharedString::from(if amend.get() {
                                copy::text("Amend", "修补提交", "修補提交")
                            } else {
                                copy::text("Commit", "提交", "提交")
                            })
                        },
                        move || {
                            dismiss_keyboard();
                            (host.on_commit)();
                        },
                    )
                    .disabled(move || {
                        committing.get()
                            || selected.with(Vec::is_empty)
                            || message.with(|message| message.trim().is_empty())
                    }),
                ),
        )
}

#[allow(clippy::too_many_arguments)]
fn worktree_sheet(
    open: Signal<bool>,
    worktrees: Signal<Vec<WorktreeRow>>,
    operations: Signal<Vec<WorktreeOperationRow>>,
    busy: Signal<bool>,
    error: Signal<Option<SharedString>>,
    on_close: Rc<dyn Fn()>,
    on_archive: Rc<dyn Fn(&str)>,
    on_merge: Rc<dyn Fn(&str)>,
    on_discard: Rc<dyn Fn(&str)>,
    on_continue: Rc<dyn Fn(&str)>,
    on_abort: Rc<dyn Fn(&str)>,
    on_resolve: ResolveWorktree,
    on_restore: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_header};
    bottom_sheet(
        move || open.get(),
        {
            let on_close = on_close.clone();
            move || on_close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.8))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Worktrees", "Worktree", "Worktree"),
                move || on_close(),
            ))
            .child(
                scroll().flex_shrink(1.0).min_h(px(0.0)).child(
                    sheet_body()
                        .child(when(
                            move || error.get().is_some(),
                            move || {
                                text(move || error.get().unwrap_or_default())
                                    .px(px(theme::SPACING_XS))
                                    .text_size(px(theme::font_caption()))
                                    .text_color(theme::accent_red)
                            },
                        ))
                        .child(each(
                            move || worktrees.get(),
                            |worktree| worktree.id.clone(),
                            move |worktree, _| {
                                let on_archive = on_archive.clone();
                                let on_restore = on_restore.clone();
                                let on_merge = on_merge.clone();
                                let on_discard = on_discard.clone();
                                let id = worktree.with_untracked(|worktree| worktree.id.clone());
                                // One worktree: what it is and how it stands,
                                // where it lives, and what can be done to it.
                                view().w_full().pb(px(theme::SPACING_MD)).child(
                                    surface::card()
                                        .p(px(surface::ROW_INSET))
                                        .gap(px(6.0))
                                        .child(
                                            view()
                                                .flex_row()
                                                .items_center()
                                                .gap(px(theme::SPACING_SM))
                                                .child(
                                                    text(move || {
                                                        worktree.with(|worktree| {
                                                            if worktree.branch.trim().is_empty() {
                                                                worktree.path.clone()
                                                            } else {
                                                                worktree.branch.clone()
                                                            }
                                                        })
                                                    })
                                                    .flex_1()
                                                    .min_w(px(0.0))
                                                    .truncate()
                                                    .text_size(px(theme::font_detail()))
                                                    .font_weight(FontWeight::MEDIUM)
                                                    .text_color(theme::foreground),
                                                )
                                                .child(surface::badge(
                                                    move || {
                                                        SharedString::from(worktree.with(
                                                            |worktree| worktree.status.clone(),
                                                        ))
                                                    },
                                                    theme::text_secondary,
                                                )),
                                        )
                                        .child(
                                            text(move || {
                                                worktree.with(|worktree| worktree.path.clone())
                                            })
                                            .truncate()
                                            .font_family("monospace")
                                            .text_size(px(theme::font_hint()))
                                            .text_color(theme::text_muted),
                                        )
                                        .child(
                                            view()
                                                .flex_row()
                                                .flex_wrap()
                                                .gap(px(theme::SPACING_SM))
                                                .pt(px(theme::SPACING_XS))
                                                .child(when(
                                                    move || {
                                                        worktree.with(|worktree| worktree.can_merge)
                                                    },
                                                    {
                                                        let id = id.clone();
                                                        move || {
                                                            let on_merge = on_merge.clone();
                                                            let id = id.clone();
                                                            worktree_action(
                                                                copy::text("Merge", "合并", "合併"),
                                                                false,
                                                                busy,
                                                                move || on_merge(&id),
                                                            )
                                                        }
                                                    },
                                                ))
                                                .child(when(
                                                    move || {
                                                        worktree
                                                            .with(|worktree| worktree.can_archive)
                                                    },
                                                    {
                                                        let id = id.clone();
                                                        move || {
                                                            let on_archive = on_archive.clone();
                                                            let id = id.clone();
                                                            worktree_action(
                                                                copy::text(
                                                                    "Archive", "归档", "封存",
                                                                ),
                                                                false,
                                                                busy,
                                                                move || on_archive(&id),
                                                            )
                                                        }
                                                    },
                                                ))
                                                .child(when(
                                                    move || {
                                                        worktree
                                                            .with(|worktree| worktree.can_restore)
                                                    },
                                                    {
                                                        let id = id.clone();
                                                        move || {
                                                            let on_restore = on_restore.clone();
                                                            let id = id.clone();
                                                            worktree_action(
                                                                copy::text(
                                                                    "Restore", "恢复", "還原",
                                                                ),
                                                                false,
                                                                busy,
                                                                move || on_restore(&id),
                                                            )
                                                        }
                                                    },
                                                ))
                                                .child(when(
                                                    move || {
                                                        worktree
                                                            .with(|worktree| worktree.can_discard)
                                                    },
                                                    {
                                                        let id = id.clone();
                                                        move || {
                                                            let on_discard = on_discard.clone();
                                                            let id = id.clone();
                                                            worktree_action(
                                                                copy::text(
                                                                    "Discard", "丢弃", "丟棄",
                                                                ),
                                                                true,
                                                                busy,
                                                                move || on_discard(&id),
                                                            )
                                                        }
                                                    },
                                                )),
                                        ),
                                )
                            },
                        ))
                        .child(when(
                            move || !operations.with(|operations| operations.is_empty()),
                            move || {
                                let on_continue = on_continue.clone();
                                let on_abort = on_abort.clone();
                                let on_resolve = on_resolve.clone();
                                view()
                                    .w_full()
                                    .flex_col()
                                    .gap(px(theme::SPACING_SM))
                                    .child(surface::group_label(copy::text(
                                        "Operations",
                                        "操作",
                                        "操作",
                                    )))
                                    .child(each(
                                        move || operations.get(),
                                        |operation| operation.id.clone(),
                                        move |operation, _| {
                                            let on_continue = on_continue.clone();
                                            let on_abort = on_abort.clone();
                                            let on_resolve = on_resolve.clone();
                                            let id = operation
                                                .with_untracked(|operation| operation.id.clone());
                                            surface::card()
                                                .p(px(surface::ROW_INSET))
                                                .gap(px(theme::SPACING_SM))
                                                .child(
                                                    view()
                                                        .flex_row()
                                                        .items_center()
                                                        .gap(px(theme::SPACING_SM))
                                                        .child(
                                                            text(move || {
                                                                operation.with(|operation| {
                                                                    operation.label.clone()
                                                                })
                                                            })
                                                            .flex_1()
                                                            .min_w(px(0.0))
                                                            .truncate()
                                                            .text_size(px(theme::font_detail()))
                                                            .font_weight(FontWeight::MEDIUM)
                                                            .text_color(theme::foreground),
                                                        )
                                                        .child(surface::badge(
                                                            move || {
                                                                SharedString::from(operation.with(
                                                                    |operation| {
                                                                        operation.status.clone()
                                                                    },
                                                                ))
                                                            },
                                                            theme::accent_yellow,
                                                        )),
                                                )
                                                .child(each(
                                                    move || {
                                                        operation.with(|operation| {
                                                            operation.conflicts.clone()
                                                        })
                                                    },
                                                    |conflict| conflict.path.clone(),
                                                    {
                                                        let id = id.clone();
                                                        move |conflict: ReadSignal<WorktreeConflictRow>, _| {
                                                            let on_resolve = on_resolve.clone();
                                                            let id = id.clone();
                                                            let path = conflict
                                                                .with_untracked(|conflict| {
                                                                    conflict.path.clone()
                                                                });
                                                            // A conflicted path, and the two
                                                            // sides it can be settled for.
                                                            view()
                                                                .w_full()
                                                                .flex_row()
                                                                .items_center()
                                                                .gap(px(theme::SPACING_SM))
                                                                .child(
                                                                    text(path.clone())
                                                                        .flex_1()
                                                                        .min_w(px(0.0))
                                                                        .truncate()
                                                                        .font_family("monospace")
                                                                        .text_size(px(
                                                                            theme::font_hint(),
                                                                        ))
                                                                        .text_color(
                                                                            theme::text_secondary,
                                                                        ),
                                                                )
                                                                .child(worktree_action(
                                                                    copy::text(
                                                                        "Worktree",
                                                                        "工作树",
                                                                        "工作樹",
                                                                    ),
                                                                    false,
                                                                    busy,
                                                                    {
                                                                        let on_resolve =
                                                                            on_resolve.clone();
                                                                        let id = id.clone();
                                                                        let path = path.clone();
                                                                        move || {
                                                                            on_resolve(
                                                                                &id, &path, true,
                                                                            )
                                                                        }
                                                                    },
                                                                ))
                                                                .child(worktree_action(
                                                                    copy::text(
                                                                        "Target", "目标", "目標",
                                                                    ),
                                                                    false,
                                                                    busy,
                                                                    move || {
                                                                        on_resolve(&id, &path, false)
                                                                    },
                                                                ))
                                                        }
                                                    },
                                                ))
                                                .child(
                                                    view()
                                                        .flex_row()
                                                        .flex_wrap()
                                                        .gap(px(theme::SPACING_SM))
                                                        .child(when(
                                                            move || {
                                                                operation.with(|operation| {
                                                                    operation.can_continue
                                                                })
                                                            },
                                                            {
                                                                let id = id.clone();
                                                                move || {
                                                                    let on_continue =
                                                                        on_continue.clone();
                                                                    let id = id.clone();
                                                                    worktree_action(
                                                                        copy::text(
                                                                            "Continue",
                                                                            "继续",
                                                                            "繼續",
                                                                        ),
                                                                        false,
                                                                        busy,
                                                                        move || on_continue(&id),
                                                                    )
                                                                }
                                                            },
                                                        ))
                                                        .child(when(
                                                            move || {
                                                                operation.with(|operation| {
                                                                    operation.can_abort
                                                                })
                                                            },
                                                            {
                                                                let id = id.clone();
                                                                move || {
                                                                    let on_abort = on_abort.clone();
                                                                    let id = id.clone();
                                                                    worktree_action(
                                                                        copy::text(
                                                                            "Abort", "中止", "中止",
                                                                        ),
                                                                        true,
                                                                        busy,
                                                                        move || on_abort(&id),
                                                                    )
                                                                }
                                                            },
                                                        )),
                                                )
                                        },
                                    ))
                            },
                        )),
                ),
            ),
    )
}

fn branch_sheet(
    open: Signal<bool>,
    branches: Signal<Vec<BranchRow>>,
    on_close: Rc<dyn Fn()>,
    on_select: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    use crate::ui::sheet::{bottom_sheet, sheet_body, sheet_card, sheet_header};
    bottom_sheet(
        move || open.get(),
        {
            let on_close = on_close.clone();
            move || on_close()
        },
        view()
            .w_full()
            .max_h(move || px(window().height.0 * 0.75))
            .min_h(px(0.0))
            .child(sheet_header(
                copy::text("Branches", "分支", "分支"),
                move || on_close(),
            ))
            .child(
                scroll()
                    .flex_shrink(1.0)
                    .min_h(px(0.0))
                    .child(sheet_body().child(sheet_card().child(each(
                        move || branches.get(),
                        |branch| branch.name.clone(),
                        move |branch, _| {
                            let on_select = on_select.clone();
                            button()
                                .w_full()
                                .min_h(px(theme::ROW_HEIGHT))
                                .px(px(surface::ROW_INSET))
                                .py(px(theme::SPACING_SM))
                                .flex_row()
                                .items_center()
                                .gap(px(theme::SPACING_MD))
                                .border_b_1()
                                .border_b_color(theme::hairline)
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_press(move || {
                                    on_select(&branch.with_untracked(|branch| branch.name.clone()))
                                })
                                .child(surface::tile(icon!("git-branch")))
                                .child(
                                    view()
                                        .flex_1()
                                        .min_w(px(0.0))
                                        .flex_col()
                                        .gap(px(1.0))
                                        .child(
                                            text(move || branch.with(|branch| branch.name.clone()))
                                                .truncate()
                                                .text_size(px(theme::font_detail()))
                                                .text_color(theme::foreground),
                                        )
                                        .child(when(
                                            move || branch.with(|branch| !branch.detail.is_empty()),
                                            move || {
                                                text(move || {
                                                    branch.with(|branch| branch.detail.clone())
                                                })
                                                .truncate()
                                                .text_size(px(theme::font_micro()))
                                                .text_color(theme::text_muted)
                                            },
                                        )),
                                )
                                .child(when(
                                    move || branch.with(|branch| branch.behind > 0),
                                    move || {
                                        count(
                                            move || {
                                                format!("↓{}", branch.with(|branch| branch.behind))
                                            },
                                            theme::accent_blue,
                                        )
                                    },
                                ))
                                .child(when(
                                    move || branch.with(|branch| branch.ahead > 0),
                                    move || {
                                        count(
                                            move || {
                                                format!("↑{}", branch.with(|branch| branch.ahead))
                                            },
                                            theme::accent_success,
                                        )
                                    },
                                ))
                                .child(when(
                                    move || branch.with(|branch| branch.current),
                                    || {
                                        svg(icon!("check"))
                                            .size(px(theme::ICON_MD))
                                            .flex_shrink_0()
                                            .text_color(theme::foreground)
                                    },
                                ))
                        },
                    )))),
            ),
    )
}

/// The history list.
fn history_list(
    history: Signal<Vec<CommitRow>>,
    has_more: Signal<bool>,
    loading: Signal<bool>,
    on_open_commit: Rc<dyn Fn(&str)>,
    on_load_more: Rc<dyn Fn()>,
) -> impl IntoElement {
    view().flex_1().min_h(px(0.0)).flex_col().child(
        virtual_list(
            move || history.with(Vec::len),
            move |at: Position| history_row(at, history, on_open_commit.clone()),
        )
        .keys(move || {
            history.with(|history| {
                history
                    .iter()
                    .map(|commit| commit.hash.clone())
                    .collect::<Vec<_>>()
            })
        })
        .estimated_item_size(px(HISTORY_ROW_HEIGHT))
        // Reaching the end walks one more page back through the history.
        .on_load_more(move || on_load_more())
        .footer(history_footer(history, has_more, loading))
        .flex_1(),
    )
}

/// The strip under the history: the next page on its way, or the end of it.
fn history_footer(
    history: Signal<Vec<CommitRow>>,
    has_more: Signal<bool>,
    loading: Signal<bool>,
) -> impl IntoElement {
    view()
        .w_full()
        .flex_shrink_0()
        .py(px(theme::SPACING_MD))
        .items_center()
        .justify_center()
        .child(when(
            move || loading.get(),
            || {
                let clock = crate::ui::activity::ActivityClock::start();
                crate::ui::activity::activity_ring(clock, theme::text_muted)
            },
        ))
        .child(when(
            move || {
                !loading.get() && !has_more.get() && history.with(|history| !history.is_empty())
            },
            || {
                text(copy::text("End of history", "已到最早提交", "已到最早提交"))
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted)
            },
        ))
}

/// A commit in the history list.
const HISTORY_ROW_HEIGHT: f32 = 60.0;

fn history_row(
    at: Position,
    history: Signal<Vec<CommitRow>>,
    on_open_commit: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    let commit = computed(move || history.with(|history| history.get(at.index()).cloned()));
    button()
        .min_h(px(HISTORY_ROW_HEIGHT))
        .mx(px(theme::SPACING_SM))
        .px(px(theme::SPACING_SM))
        .py(px(6.0))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_MD))
        .rounded(px(theme::RADIUS_CHIP))
        .active(|style| style.bg(theme::row_pressed_bg()))
        .on_press(move || {
            if let Some(commit) = commit.get_untracked() {
                on_open_commit(&commit.hash);
            }
        })
        // The mark of a commit on the line of its history.
        .child(
            view()
                .w(px(theme::SPACING_LG))
                .flex_shrink_0()
                .items_center()
                .child(
                    view()
                        .size(px(9.0))
                        .rounded_full()
                        .border(px(2.0))
                        .border_color(theme::text_muted),
                ),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .flex_col()
                .gap(px(2.0))
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
                                let mut line = format!("{} · {}", commit.short_hash, commit.detail);
                                if let Some(stamp) =
                                    commit.authored_at_ms.and_then(format_commit_time)
                                {
                                    line.push_str(" · ");
                                    line.push_str(&stamp);
                                }
                                line
                            })
                        })
                    })
                    .truncate()
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                ),
        )
        .child(surface::chevron())
}

/// When a commit was authored, written compactly for the history list: the
/// clock time for today, and the date for anything older.
fn format_commit_time(authored_at_ms: i64) -> Option<String> {
    if authored_at_ms <= 0 {
        return None;
    }
    let seconds = (now_ms() - authored_at_ms).max(0) / 1_000;
    Some(match seconds {
        0..=59 => copy::text("now", "刚刚", "剛剛").to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        86_400..=2_591_999 => format!("{}d", seconds / 86_400),
        _ => {
            let days = seconds / 86_400;
            format!("{}d", days)
        }
    })
}

/// The wall clock in milliseconds.
fn now_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or_default()
}

/// The diff with the action that discards it.
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
        .border_t_1()
        .border_color(theme::hairline)
        // The diff is drawn from its parsed hunks, with line numbers and the
        // two change tints, rather than as one flat block of raw text.
        .child(dynamic(move || {
            let staged = staged.get();
            super::git_diff::single_file_diff(body.get(), staged).into_element()
        }))
        .child(when(move || truncated.get(), || {
            preview_limit(copy::text(
                "The diff is larger than the preview limit.",
                "差异超过预览上限。",
                "差異超過預覽上限。",
            ))
        }))
        .child(when(move || !staged.get(), {
            let on_revert = on_revert.clone();
            move || {
                let path = path.get_untracked();
                let on_revert = on_revert.clone();
                view()
                    .w_full()
                    .flex_shrink_0()
                    .px(px(theme::SPACING_MD))
                    .py(px(theme::SPACING_SM))
                    .border_t_1()
                    .border_color(theme::hairline)
                    .child(
                        action::tonal_danger(
                            copy::text("Revert changes", "还原改动", "還原改動"),
                            move || {
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
                                        .destructive(
                                            "revert",
                                            copy::text("Revert", "还原", "還原"),
                                        )
                                        .show()
                                        .await;
                                    if matches!(&answer, Ok(answer) if answer.action.as_deref() == Some("revert"))
                                    {
                                        on_revert(&path);
                                    }
                                });
                            },
                        )
                        .w_full(),
                    )
            }
        }))
}

/// One commit's message and patch.
fn commit_detail_view(detail: Signal<Option<CommitDetailPreview>>) -> impl IntoElement {
    let preview = computed(move || detail.get());
    view()
        .flex_1()
        .min_h(px(0.0))
        .border_t_1()
        .border_color(theme::hairline)
        .child(dynamic(move || match preview.get() {
            Some(detail) => super::git_diff::commit_detail_content(detail).into_element(),
            None => view().into_element(),
        }))
}

/// The mark's color for a changed path, matching the file tree and desktop:
/// added is green, untracked is red, modified is blue, deleted is red.
fn status_color(status: &str) -> Hsla {
    match status {
        "A" => theme::status_added(),
        "?" => theme::status_untracked(),
        "D" | "!" => theme::accent_red(),
        "M" | "R" | "C" | "T" => theme::status_modified(),
        _ => theme::text_muted(),
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn rollback_undo_icon_inherits_current_color() {
        let svg = include_str!("../../assets/icons/undo.svg");
        assert!(
            svg.contains("stroke=\"currentColor\""),
            "undo.svg must use currentColor so it follows theme ink instead of looking permanently grey"
        );
        assert!(
            !svg.contains("stroke=\"#"),
            "undo.svg must not hardcode a hex stroke color: {svg}"
        );
    }
}

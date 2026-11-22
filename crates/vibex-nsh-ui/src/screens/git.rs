//! Changes, commits, and worktree controls for the runtime's repository.

use std::rc::Rc;

use nsh_base::{ButtonElement, SheetEvent, button, sheet};
use nsh_core::{element::ImageSrc, prelude::*};

use crate::{
    copy, theme,
    ui::{action, field},
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
    pub remote_busy: Signal<bool>,
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
        .bg(theme::sidebar_bg)
        .pb_safe_area()
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
                        .h(px(theme::TOUCH_TARGET))
                        .flex_shrink_0()
                        .flex_row()
                        .items_center()
                        .border_b_1()
                        .border_b_color(theme::border_subtle)
                        .child(git_icon(
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
                            .text_size(px(theme::font_body()))
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
    view().flex_1().min_h(px(0.0))
        .child(view().h(px(theme::TOUCH_TARGET)).flex_shrink_0().flex_row().items_center().px(px(theme::SPACING_SM))
            .gap(px(theme::SPACING_XS)).border_b_1().border_b_color(theme::border_subtle)
            .child(view().flex_1().min_w(px(0.0)).child(when(move || history.get(), move || {
                view().flex_row().items_center()
                    .child(field::text_field_flat(query, copy::text("Search commits", "搜索提交", "搜尋提交")).flex_1().min_w(px(0.0)))
                    .child(when(move || !query.with(String::is_empty), move || git_icon(icon!("x"), copy::text("Clear commit search", "清除提交搜索", "清除提交搜尋"), move || query.set(String::new()))))
            }).otherwise({
                let branches = host.on_open_branches.clone();
                move || {
                    let branches = branches.clone();
                    button().flex_row().items_center().gap(px(theme::SPACING_XS)).h(px(theme::TOUCH_TARGET)).min_w(px(0.0))
                        .accessibility_label(copy::text("Branches", "分支", "分支"))
                        .on_press(move || branches())
                        .child(svg(icon!("git-branch")).size(px(theme::ICON_SM)).flex_shrink_0().text_color(theme::text_muted))
                        .child(text(move || branch.get().unwrap_or_else(|| copy::text("No repository", "没有仓库", "沒有儲存庫").into()))
                            .flex_1().min_w(px(0.0)).truncate().text_size(px(theme::font_detail())).text_color(theme::foreground))
                        .child(when(move || totals.get().0 > 0, move || text(move || format!("+{}", totals.get().0))
                            .flex_shrink_0().text_size(px(theme::font_micro())).text_color(theme::accent_success)))
                        .child(when(move || totals.get().1 > 0, move || text(move || format!("-{}", totals.get().1))
                            .flex_shrink_0().text_size(px(theme::font_micro())).text_color(theme::accent_red)))
                }
            })))
            .child(git_icon(icon!("refresh"), copy::text("Fetch", "获取", "擷取"), move || (host.on_fetch)())
                .disabled(move || busy.get()).disabled_style(|style| style.opacity(0.4)))
            .child(git_icon(icon!("download"), copy::text("Pull", "拉取", "拉取"), move || (host.on_pull)())
                .disabled(move || busy.get()).disabled_style(|style| style.opacity(0.4)))
            .child(git_icon(icon!("upload"), copy::text("Push", "推送", "推送"), move || (host.on_push)())
                .disabled(move || busy.get()).disabled_style(|style| style.opacity(0.4))))
        .child(view().h(px(theme::TOUCH_TARGET)).flex_shrink_0().flex_row().items_center().px(px(theme::SPACING_XS))
            .border_b_1().border_b_color(theme::border_subtle)
            .child(git_mode_button(copy::text("Changes", "更改", "變更"), history, false, host.on_toggle_history.clone()))
            .child(git_mode_button(copy::text("Commits", "提交", "提交"), history, true, host.on_toggle_history))
            .child(view().flex_1().min_w(px(0.0)))
            .child(when(move || !history.get(), {
                let select = host.on_toggle_selection.clone();
                let revert = host.on_revert_selection.clone();
                let dirs = host.on_toggle_directories.clone();
                move || {
                    let select = select.clone();
                    let revert = revert.clone();
                    let dirs = dirs.clone();
                    view().flex_row().items_center()
                        .child(git_icon(icon!("list-checks"), copy::text("Select all changes", "选择全部变更", "選取全部變更"), move || select("")))
                        .child(git_icon(icon!("undo"), copy::text("Rollback selected", "回滚所选变更", "復原已選變更"), move || {
                            let revert = revert.clone();
                            let paths = selected.get_untracked();
                            let owner = active_session.get_untracked();
                            spawn(async move {
                                let result = nsh_base::alert_dialog()
                                    .title(copy::text("Rollback selected changes?", "回滚所选变更？", "復原已選變更？"))
                                    .description(copy::text("Changes to the selected paths will be discarded.", "所选路径的改动将被丢弃。", "已選路徑的變更將被捨棄。"))
                                    .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                                    .destructive("revert", copy::text("Rollback", "回滚", "復原"))
                                    .show().await;
                                if matches!(result, Ok(answer) if answer.action.as_deref() == Some("revert"))
                                    && active_session.get_untracked() == owner { revert(&paths); }
                            });
                        }).disabled(move || selected.with(Vec::is_empty) || busy.get()).disabled_style(|style| style.opacity(0.4)))
                        .child(git_icon(icon!("chevrons-down-up"), copy::text("Expand or collapse all", "展开或折叠目录", "展開或摺疊資料夾"), move || dirs()))
                }
            }))
            .child(git_icon(icon!("folder"), copy::text("Worktrees", "Worktree", "Worktree"), move || (host.on_open_worktrees)())))
        .child(when(move || error.with(Option::is_some), move || {
            view().flex_row().items_center().px(px(theme::SPACING_SM)).gap(px(theme::SPACING_XS))
                .child(text(move || error.get().unwrap_or_default()).flex_1().min_w(px(0.0)).text_size(px(theme::font_caption())).text_color(theme::accent_red))
                .child({ let refresh = host.on_refresh.clone(); git_icon(icon!("refresh"), copy::text("Refresh", "刷新", "重新整理"), move || refresh()) })
        }))
        .child(when(move || history.get(), move || history_list(history_host.history, query, history_host.on_open_commit.clone()))
            .otherwise(move || view().flex_1().min_h(px(0.0))
                .child(change_list(change_host.clone()))
                .child(commit_bar(change_host.clone()))))
}

fn git_icon(icon: ImageSrc, label: &'static str, action: impl Fn() + 'static) -> ButtonElement {
    action::icon_button(icon, label, action)
        .size(px(34.0))
        .flex_shrink_0()
}

fn git_mode_button(
    label: &'static str,
    history: Signal<bool>,
    selected: bool,
    toggle: Rc<dyn Fn()>,
) -> impl IntoElement {
    button()
        .h(px(32.0))
        .px(px(theme::SPACING_SM))
        .rounded(px(theme::RADIUS_CONTROL))
        .items_center()
        .justify_center()
        .style_when(
            move || history.get() == selected,
            |style| style.bg(theme::rail_tab_bg()),
        )
        .on_press(move || {
            if history.get_untracked() != selected {
                dismiss_keyboard();
                toggle();
            }
        })
        .child(
            text(label)
                .text_size(px(theme::font_caption()))
                .text_color(theme::text_secondary),
        )
}

fn change_list(host: GitHost) -> impl IntoElement {
    let changes = host.changes;
    view().flex_1().min_h(px(0.0)).child(
        when(
            move || changes.with(Vec::is_empty),
            || super::session::empty_hint(copy::text("No changes", "没有变更", "沒有變更")),
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
        .mx(px(theme::SPACING_XS))
        .px(px(theme::SPACING_XS))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_XS))
        .rounded(px(theme::RADIUS_CONTROL))
        .child(view().w(px(row.depth.min(8) as f32 * 16.0)).flex_shrink_0())
        .child(
            button()
                .w(px(28.0))
                .h_full()
                .flex_shrink_0()
                .items_center()
                .justify_center()
                .accessibility_label(format!(
                    "{} {}",
                    copy::text("Select", "选择", "選取"),
                    selection_path
                ))
                .on_press(move || (host.on_toggle_selection)(&selection_path))
                .child(
                    view()
                        .size(px(18.0))
                        .rounded(px(4.0))
                        .border_1()
                        .items_center()
                        .justify_center()
                        .border_color(if selection == PathSelection::Unchecked {
                            theme::border_default
                        } else {
                            theme::primary
                        })
                        .when(selection != PathSelection::Unchecked, |view| {
                            view.bg(theme::primary).child(
                                svg(if selection == PathSelection::Checked {
                                    icon!("check")
                                } else {
                                    icon!("minus")
                                })
                                .size(px(12.0))
                                .text_color(theme::primary_foreground),
                            )
                        }),
                ),
        )
        .child(
            button()
                .flex_1()
                .min_w(px(0.0))
                .h_full()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
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
                            icon!("chevron-down")
                        } else {
                            icon!("chevron-right")
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
                        .text_color(theme::foreground),
                )
                .children((!row.directory).then(|| {
                    view()
                        .flex_row()
                        .items_center()
                        .flex_shrink_0()
                        .gap(px(theme::SPACING_XS))
                        .child(
                            text(row.status)
                                .px(px(theme::SPACING_XS))
                                .rounded(px(4.0))
                                .bg(color.opacity(0.18))
                                .text_size(px(theme::font_micro()))
                                .text_color(color),
                        )
                        .children((row.additions > 0).then(|| {
                            text(format!("+{}", row.additions))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::accent_success)
                        }))
                        .children((row.deletions > 0).then(|| {
                            text(format!("-{}", row.deletions))
                                .text_size(px(theme::font_micro()))
                                .text_color(theme::accent_red)
                        }))
                })),
        )
        .children((!row.directory).then(|| {
            git_icon(
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

fn commit_bar(host: GitHost) -> impl IntoElement {
    let selected = host.selected;
    let message = host.message;
    let amend = host.amend;
    let push = host.push_after;
    let committing = host.committing;
    view()
        .w_full()
        .flex_shrink_0()
        .pb_keyboard()
        .gap(px(theme::SPACING_XS))
        .px(px(theme::SPACING_SM))
        .py(px(theme::SPACING_SM))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
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
                    .text_size(px(theme::font_micro()))
                    .text_color(theme::text_muted),
                )
                .child(
                    git_icon(
                        icon!("rotate-ccw"),
                        copy::text("Amend", "修补提交", "修補提交"),
                        move || (host.on_toggle_amend)(),
                    )
                    .style_when(move || amend.get(), |style| style.bg(theme::rail_tab_bg())),
                )
                .child(
                    git_icon(
                        icon!("upload"),
                        copy::text("Push after", "提交后推送", "提交後推送"),
                        move || (host.on_toggle_push_after)(),
                    )
                    .style_when(move || push.get(), |style| style.bg(theme::rail_tab_bg())),
                ),
        )
        .child(
            view()
                .flex_row()
                .gap(px(theme::SPACING_XS))
                .items_center()
                .child(
                    crate::ui::field::text_field(
                        message,
                        copy::text("Commit message", "提交说明", "提交說明"),
                    )
                    .flex_1()
                    .min_w(px(0.0)),
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
                            move || {
                                dismiss_keyboard();
                                (host.on_commit)();
                            }
                        },
                    )
                    .disabled(move || {
                        committing.get()
                            || selected.with(Vec::is_empty)
                            || message.with(|message| message.trim().is_empty())
                    })
                    .disabled_style(|style| style.opacity(0.4)),
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
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                on_close();
            }
        })
        .child(
            view()
                .w_full()
                .max_h(px(520.0))
                .gap(px(theme::SPACING_XS))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Worktrees", "Worktree", "Worktree"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(when(
                    move || error.get().is_some(),
                    move || {
                        text(move || error.get().unwrap_or_default())
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
                        view()
                            .w_full()
                            .flex_col()
                            .gap(px(2.0))
                            .p(px(theme::SPACING_SM))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::background_dim)
                            .child(
                                view()
                                    .flex_row()
                                    .items_center()
                                    .gap(px(theme::SPACING_XS))
                                    .child(
                                        text(move || {
                                            let worktree = worktree.get();
                                            if worktree.branch.trim().is_empty() {
                                                worktree.path.clone()
                                            } else {
                                                worktree.branch.clone()
                                            }
                                        })
                                        .flex_1()
                                        .truncate()
                                        .text_size(px(theme::font_detail()))
                                        .text_color(theme::foreground),
                                    )
                                    .child(
                                        text(move || worktree.get().status)
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_muted),
                                    ),
                            )
                            .child(
                                text(move || worktree.get().path)
                                    .truncate()
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted),
                            )
                            .child(
                                view()
                                    .flex_row()
                                    .gap(px(theme::SPACING_XS))
                                    .child(when(move || worktree.get().can_merge, {
                                        let on_merge = on_merge.clone();
                                        let id = worktree.get_untracked().id;
                                        move || {
                                            let on_merge = on_merge.clone();
                                            let id = id.clone();
                                            button()
                                                .on_press(move || {
                                                    if !busy.get_untracked() {
                                                        on_merge(&id);
                                                    }
                                                })
                                                .min_h(px(28.0))
                                                .px(px(theme::SPACING_SM))
                                                .items_center()
                                                .rounded(px(theme::RADIUS_CONTROL))
                                                .child(
                                                    text(copy::text("Merge", "合并", "合併"))
                                                        .text_size(px(theme::font_micro()))
                                                        .text_color(theme::text_secondary),
                                                )
                                        }
                                    }))
                                    .child(when(
                                        move || worktree.get().can_archive,
                                        move || {
                                            let on_archive = on_archive.clone();
                                            let id = worktree.get_untracked().id;
                                            button()
                                                .on_press(move || {
                                                    if !busy.get_untracked() {
                                                        on_archive(&id);
                                                    }
                                                })
                                                .min_h(px(28.0))
                                                .px(px(theme::SPACING_SM))
                                                .items_center()
                                                .rounded(px(theme::RADIUS_CONTROL))
                                                .child(
                                                    text(copy::text("Archive", "归档", "封存"))
                                                        .text_size(px(theme::font_micro()))
                                                        .text_color(theme::text_secondary),
                                                )
                                        },
                                    ))
                                    .child(when(move || worktree.get().can_discard, {
                                        let on_discard = on_discard.clone();
                                        let id = worktree.get_untracked().id;
                                        move || {
                                            let on_discard = on_discard.clone();
                                            let id = id.clone();
                                            button()
                                                .on_press(move || {
                                                    if !busy.get_untracked() {
                                                        on_discard(&id);
                                                    }
                                                })
                                                .min_h(px(28.0))
                                                .px(px(theme::SPACING_SM))
                                                .items_center()
                                                .rounded(px(theme::RADIUS_CONTROL))
                                                .child(
                                                    text(copy::text("Discard", "丢弃", "丟棄"))
                                                        .text_size(px(theme::font_micro()))
                                                        .text_color(theme::accent_red),
                                                )
                                        }
                                    }))
                                    .child(when(
                                        move || worktree.get().can_restore,
                                        move || {
                                            let on_restore = on_restore.clone();
                                            let id = worktree.get_untracked().id;
                                            button()
                                                .on_press(move || {
                                                    if !busy.get_untracked() {
                                                        on_restore(&id);
                                                    }
                                                })
                                                .min_h(px(28.0))
                                                .px(px(theme::SPACING_SM))
                                                .items_center()
                                                .rounded(px(theme::RADIUS_CONTROL))
                                                .child(
                                                    text(copy::text("Restore", "恢复", "還原"))
                                                        .text_size(px(theme::font_micro()))
                                                        .text_color(theme::text_secondary),
                                                )
                                        },
                                    )),
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
                            .gap(px(theme::SPACING_XS))
                            .child(
                                text(copy::text("Operations", "操作", "操作"))
                                    .text_size(px(theme::font_micro()))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_muted),
                            )
                            .child(each(
                                move || operations.get(),
                                |operation| operation.id.clone(),
                                move |operation, _| {
                                    let on_continue = on_continue.clone();
                                    let on_abort = on_abort.clone();
                                    let on_resolve = on_resolve.clone();
                                    view()
                                        .w_full()
                                        .flex_row()
                                        .flex_wrap()
                                        .items_center()
                                        .gap(px(theme::SPACING_XS))
                                        .p(px(theme::SPACING_SM))
                                        .rounded(px(theme::RADIUS_CONTROL))
                                        .bg(theme::background_dim)
                                        .child(
                                            text(move || {
                                                let operation = operation.get();
                                                format!(
                                                    "{} · {}",
                                                    operation.label, operation.status
                                                )
                                            })
                                            .flex_1()
                                            .truncate()
                                            .text_size(px(theme::font_micro()))
                                            .text_color(theme::text_secondary),
                                        )
                                        .child(when(
                                            move || operation.get().can_continue,
                                            move || {
                                                let on_continue = on_continue.clone();
                                                let id = operation.get_untracked().id;
                                                button()
                                                    .on_press(move || {
                                                        if !busy.get_untracked() {
                                                            on_continue(&id);
                                                        }
                                                    })
                                                    .min_h(px(28.0))
                                                    .px(px(theme::SPACING_SM))
                                                    .items_center()
                                                    .child(
                                                        text(copy::text(
                                                            "Continue", "继续", "繼續",
                                                        ))
                                                        .text_size(px(theme::font_micro()))
                                                        .text_color(theme::text_secondary),
                                                    )
                                            },
                                        ))
                                        .child(when(
                                            move || operation.get().can_abort,
                                            move || {
                                                let on_abort = on_abort.clone();
                                                let id = operation.get_untracked().id;
                                                button()
                                                    .on_press(move || {
                                                        if !busy.get_untracked() {
                                                            on_abort(&id);
                                                        }
                                                    })
                                                    .min_h(px(28.0))
                                                    .px(px(theme::SPACING_SM))
                                                    .items_center()
                                                    .child(
                                                        text(copy::text("Abort", "中止", "中止"))
                                                            .text_size(px(theme::font_micro()))
                                                            .text_color(theme::accent_red),
                                                    )
                                            },
                                        ))
                                        .child(each(
                                            move || operation.get().conflicts,
                                            |conflict| conflict.path.clone(),
                                            move |conflict, _| {
                                                let on_resolve = on_resolve.clone();
                                                let id = operation.get_untracked().id;
                                                let path = conflict.get_untracked().path;
                                                view()
                                                    .w_full()
                                                    .flex_row()
                                                    .items_center()
                                                    .gap(px(theme::SPACING_XS))
                                                    .child(
                                                        text(path.clone())
                                                            .flex_1()
                                                            .truncate()
                                                            .text_size(px(theme::font_micro()))
                                                            .text_color(theme::text_muted),
                                                    )
                                                    .child(small_text_button(
                                                        copy::text("Worktree", "工作树", "工作樹"),
                                                        {
                                                            let on_resolve = on_resolve.clone();
                                                            let id = id.clone();
                                                            let path = path.clone();
                                                            move || on_resolve(&id, &path, true)
                                                        },
                                                    ))
                                                    .child(small_text_button(
                                                        copy::text("Target", "目标", "目標"),
                                                        move || on_resolve(&id, &path, false),
                                                    ))
                                            },
                                        ))
                                },
                            ))
                    },
                )),
        )
}

fn small_text_button(label: &'static str, on_press: impl Fn() + 'static) -> ButtonElement {
    button()
        .on_press(on_press)
        .min_h(px(24.0))
        .px(px(theme::SPACING_XS))
        .items_center()
        .child(
            text(label)
                .text_size(px(theme::font_micro()))
                .text_color(theme::text_secondary),
        )
}

fn branch_sheet(
    open: Signal<bool>,
    branches: Signal<Vec<BranchRow>>,
    on_close: Rc<dyn Fn()>,
    on_select: Rc<dyn Fn(&str)>,
) -> impl IntoElement {
    sheet()
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(false, _) = event {
                on_close();
            }
        })
        .child(
            view()
                .w_full()
                .max_h(px(480.0))
                .gap(px(theme::SPACING_XS))
                .p(px(theme::SPACING_MD))
                .pb_safe_area_plus(px(theme::SPACING_LG))
                .bg(theme::background_card)
                .rounded_t(px(16.0))
                .child(
                    text(copy::text("Branches", "分支", "分支"))
                        .text_size(px(theme::font_heading()))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::foreground),
                )
                .child(each(
                    move || branches.get(),
                    |branch| branch.name.clone(),
                    move |branch, _| {
                        let on_select = on_select.clone();
                        button()
                            .w_full()
                            .min_h(px(44.0))
                            .px(px(theme::SPACING_SM))
                            .flex_row()
                            .items_center()
                            .gap(px(theme::SPACING_XS))
                            .rounded(px(theme::RADIUS_CONTROL))
                            .bg(theme::background_dim)
                            .active(|style| style.opacity(0.8))
                            .on_press(move || on_select(&branch.get_untracked().name))
                            .child(
                                text(move || {
                                    let branch = branch.get();
                                    if branch.current {
                                        format!("✓ {}", branch.name)
                                    } else {
                                        branch.name
                                    }
                                })
                                .flex_1()
                                .truncate()
                                .text_size(px(theme::font_body()))
                                .text_color(theme::foreground),
                            )
                            .child(
                                text(move || branch.get().detail)
                                    .truncate()
                                    .text_size(px(theme::font_micro()))
                                    .text_color(theme::text_muted),
                            )
                    },
                )),
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
    view().flex_1().min_h(px(0.0)).flex_col().child(
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
                        .text_color(move || {
                            line.with(|line| {
                                if line.starts_with('+') && !line.starts_with("+++") {
                                    theme::accent_success()
                                } else if line.starts_with('-') && !line.starts_with("---") {
                                    theme::accent_red()
                                } else if line.starts_with("@@") {
                                    theme::accent_blue()
                                } else {
                                    theme::foreground()
                                }
                            })
                        }),
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

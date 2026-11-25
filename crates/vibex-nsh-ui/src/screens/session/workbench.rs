//! The workbench: the page beside the conversation that holds the session's
//! workspace. Files, changes and the terminal are three surfaces of one
//! place, so they share one head and are switched by one control.

use nsh_core::prelude::*;

use super::{Pager, SessionHost, ShellPage, workbench_surfaces};
use crate::{copy, theme, ui::surface};

/// The workbench's head, as tall as the conversation's.
const HEADER_HEIGHT: f32 = theme::HEADER_HEIGHT;

#[derive(Clone, Copy, Default, PartialEq, Eq)]
pub(super) enum WorkbenchTab {
    #[default]
    Files,
    Changes,
    Terminal,
}

impl WorkbenchTab {
    fn label(self) -> &'static str {
        match self {
            Self::Files => copy::text("Files", "文件", "檔案"),
            Self::Changes => copy::text("Changes", "变更", "變更"),
            Self::Terminal => copy::text("Terminal", "终端", "終端機"),
        }
    }

    fn ensure_loaded(self, host: &SessionHost) {
        match self {
            Self::Files if !host.files_open.get_untracked() => (host.on_open_files)(),
            Self::Changes if !host.git_open.get_untracked() => (host.on_open_git)(),
            Self::Terminal if !host.terminal_open.get_untracked() => (host.on_open_terminal)(),
            _ => {}
        }
    }

    pub(super) fn back(self, host: &SessionHost) -> bool {
        match self {
            Self::Files if host.files_path.with(Option::is_some) => {
                (host.on_files_back)();
                true
            }
            Self::Changes
                if host.git_commit_detail.with(Option::is_some)
                    || host.git_diff.with(Option::is_some)
                    || host.git_show_history.get_untracked() =>
            {
                (host.on_git_back)();
                true
            }
            _ => false,
        }
    }
}

pub(super) fn workbench(
    pager: Pager,
    tab: Signal<WorkbenchTab>,
    terminal_find: Signal<bool>,
    host: SessionHost,
) -> impl IntoElement {
    let branches = host.git_branches;
    let counts = computed(move || {
        branches.with(|branches| {
            branches
                .iter()
                .find(|branch| branch.current && !branch.detail.is_empty())
                .map(|branch| (branch.behind, branch.ahead))
                .unwrap_or_default()
        })
    });
    let terminal_visible = host.terminal_visible;
    cleanup(move || {
        terminal_visible.try_set(false);
    });
    effect({
        let host = host.clone();
        move || {
            let open = pager.page() == ShellPage::Workbench && host.active.get().is_some();
            let selected = tab.get();
            terminal_visible.set(open && selected == WorkbenchTab::Terminal);
            if open {
                untracked(|| selected.ensure_loaded(&host));
            }
        }
    });
    view()
        .flex_1()
        .min_h(px(0.0))
        .pt_safe_area()
        .bg(theme::block_page_adaptive())
        .test_id("workbench-page")
        // The head: the way back to the conversation, and the three surfaces
        // of the session's workspace as one control.
        .child(
            view()
                .h(px(HEADER_HEIGHT))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .px(px(theme::SPACING_MD))
                .child(surface::round_button(
                    icon!("chevron-left"),
                    copy::text("Close", "关闭", "關閉"),
                    move || pager.close(),
                ))
                .child(
                    surface::segment_track().flex_1().min_w(px(0.0)).children(
                        [
                            WorkbenchTab::Files,
                            WorkbenchTab::Changes,
                            WorkbenchTab::Terminal,
                        ]
                        .map(|candidate| {
                            surface::segment(
                                candidate.label(),
                                move || tab.get() == candidate,
                                move || {
                                    dismiss_keyboard();
                                    tab.set(candidate);
                                },
                            )
                            // What the branch has to pull and to push shows on
                            // the tab, so it is seen from the other two.
                            .child(when(
                                move || candidate == WorkbenchTab::Changes && counts.get().0 > 0,
                                move || {
                                    sync_badge(format!("↓{}", counts.get().0), theme::accent_blue)
                                },
                            ))
                            .child(when(
                                move || candidate == WorkbenchTab::Changes && counts.get().1 > 0,
                                move || {
                                    sync_badge(
                                        format!("↑{}", counts.get().1),
                                        theme::accent_success,
                                    )
                                },
                            ))
                        }),
                    ),
                ),
        )
        .child(when(
            move || host.active.with(Option::is_none),
            || {
                surface::empty_state(
                    icon!("panel-right"),
                    copy::text("Select a session first", "请先选择会话", "請先選擇工作階段"),
                    copy::text(
                        "Files, changes and the terminal belong to a session's workspace.",
                        "文件、变更与终端都属于某个会话的工作区。",
                        "檔案、變更與終端機都屬於某個工作階段的工作區。",
                    ),
                )
            },
        ))
        .child(workbench_surfaces(host, tab, terminal_find))
}

fn sync_badge(label: String, color: fn() -> Hsla) -> impl IntoElement {
    text(label)
        .flex_shrink_0()
        .font_family("monospace")
        .text_size(px(theme::font_hint()))
        .font_weight(FontWeight::MEDIUM)
        .text_color(color)
}

pub(super) fn tab_panel(
    tab: Signal<WorkbenchTab>,
    candidate: WorkbenchTab,
    loaded: impl Fn() -> bool + 'static,
    content: impl Fn() -> AnyElement + 'static,
) -> impl IntoElement {
    view()
        .flex_1()
        .min_h(px(0.0))
        .style_when(move || tab.get() != candidate, |style| style.hidden())
        .child(when(loaded, content))
}

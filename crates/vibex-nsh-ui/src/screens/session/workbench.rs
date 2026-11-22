use nsh_base::button;
use nsh_core::prelude::*;

use super::{Pager, SessionHost, ShellPage, workbench_surfaces};
use crate::{copy, theme, ui::action};

const HEADER_HEIGHT: f32 = 44.0;
const TAB_HEIGHT: f32 = 32.0;
const TAB_RADIUS: f32 = 5.0;
const TAB_GAP: f32 = 2.0;

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
            Self::Files if host.files_preview.with(Option::is_some) => {
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
        .bg(theme::sidebar_bg)
        .child(
            view()
                .h(px(HEADER_HEIGHT))
                .flex_shrink_0()
                .flex_row()
                .items_center()
                .gap(px(TAB_GAP))
                .pl(px(theme::SPACING_XS))
                .pr(px(TAB_GAP))
                .border_b_1()
                .border_b_color(theme::border_subtle)
                .child(
                    view()
                        .flex_1()
                        .min_w(px(0.0))
                        .flex_row()
                        .items_center()
                        .gap(px(TAB_GAP))
                        .children(
                            [
                                WorkbenchTab::Files,
                                WorkbenchTab::Changes,
                                WorkbenchTab::Terminal,
                            ]
                            .map(|candidate| {
                                button()
                                    .h(px(TAB_HEIGHT))
                                    .px(px(theme::SPACING_SM))
                                    .min_w(px(0.0))
                                    .flex_row()
                                    .gap(px(theme::SPACING_XS))
                                    .rounded(px(TAB_RADIUS))
                                    .items_center()
                                    .justify_center()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .style_when(
                                        move || tab.get() == candidate,
                                        |style| style.bg(theme::rail_tab_bg()),
                                    )
                                    .accessibility_label(candidate.label())
                                    .on_press(move || {
                                        dismiss_keyboard();
                                        tab.set(candidate);
                                    })
                                    .child(
                                        text(candidate.label())
                                            .truncate()
                                            .text_size(px(theme::font_body()))
                                            .text_color(move || {
                                                if tab.get() == candidate {
                                                    theme::foreground()
                                                } else {
                                                    theme::text_muted()
                                                }
                                            }),
                                    )
                                    .child(when(
                                        move || {
                                            candidate == WorkbenchTab::Changes && counts.get().0 > 0
                                        },
                                        move || {
                                            sync_badge(
                                                format!("↓{}", counts.get().0),
                                                theme::accent_blue,
                                            )
                                        },
                                    ))
                                    .child(when(
                                        move || {
                                            candidate == WorkbenchTab::Changes && counts.get().1 > 0
                                        },
                                        move || {
                                            sync_badge(
                                                format!("↑{}", counts.get().1),
                                                theme::accent_success,
                                            )
                                        },
                                    ))
                            }),
                        ),
                )
                .child(
                    action::icon_button(
                        icon!("x"),
                        copy::text("Close", "关闭", "關閉"),
                        move || pager.close(),
                    )
                    .size(px(theme::HEADER_TRAILING_BUTTON_SIZE))
                    .flex_shrink_0(),
                ),
        )
        .child(when(
            move || host.active.with(Option::is_none),
            || {
                super::empty_hint(copy::text(
                    "Select a session first",
                    "请先选择会话",
                    "請先選擇工作階段",
                ))
            },
        ))
        .child(workbench_surfaces(host, tab))
}

fn sync_badge(label: String, color: fn() -> Hsla) -> impl IntoElement {
    text(label)
        .flex_shrink_0()
        .px(px(theme::SPACING_XS))
        .rounded(px(4.0))
        .bg(move || color().opacity(0.10))
        .text_size(px(theme::font_hint()))
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

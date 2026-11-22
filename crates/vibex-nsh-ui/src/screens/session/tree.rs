use std::rc::Rc;

use nsh_core::{AccessibilityRole, element::Position, prelude::*};

use super::{Pager, SessionState, agent_icon, icon};
use crate::{
    copy, theme,
    ui::activity::{ActivityClock, activity_ring},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum SidebarRowKind {
    Project,
    Workspace,
    Folder,
    Group,
    #[default]
    Session,
    EmptyWorkspace,
}

#[derive(Clone, Debug, PartialEq)]
pub struct SidebarCardSlice {
    pub indent: f32,
    pub top: bool,
    pub bottom: bool,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct SidebarRowView {
    pub id: String,
    pub kind: SidebarRowKind,
    pub project_id: Option<String>,
    pub workspace_id: Option<String>,
    pub title: String,
    pub detail: String,
    pub metadata: String,
    pub indent: f32,
    pub guides: Vec<f32>,
    pub card: Option<SidebarCardSlice>,
    pub count: usize,
    pub collapsed: bool,
    pub active: bool,
    pub pinned: bool,
    pub unread: bool,
    pub auto_continue: bool,
    pub auto_continue_paused: bool,
    pub scheduled: bool,
    pub state: Option<SessionState>,
}

impl SidebarRowView {
    fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.id)
    }
}

type Rows = Rc<dyn Fn(&str) -> Vec<SidebarRowView>>;
type RowAction = Rc<dyn Fn(&SidebarRowView)>;
type MoveRow = Rc<dyn Fn(&SidebarRowView, f32)>;

#[derive(Clone)]
pub struct SidebarTree {
    pub rows: Rows,
    pub detailed: Signal<bool>,
    pub on_toggle_hierarchy: Rc<dyn Fn()>,
    pub on_toggle_row: RowAction,
    pub on_collapse_all: Rc<dyn Fn()>,
    pub on_reveal_active: Rc<dyn Fn()>,
    pub on_move_row: MoveRow,
    pub on_new_session: RowAction,
    pub on_new_folder: RowAction,
}

#[derive(Clone)]
pub(super) struct TreeActions {
    pub toggle: RowAction,
    pub select: RowAction,
    pub menu: RowAction,
    pub move_row: MoveRow,
}

#[derive(Clone, Copy)]
pub(super) struct TreeState {
    pub pager: Pager,
    pub rows: Memo<Vec<SidebarRowView>>,
    pub query: Signal<String>,
    pub select_mode: Signal<bool>,
    pub selected: Signal<Vec<String>>,
    pub locate: Signal<Option<String>>,
}

pub(super) fn list(state: TreeState, actions: TreeActions) -> impl IntoElement {
    let rows = state.rows;
    let clock = ActivityClock::start();
    let list_ref = element_ref();
    effect(move || {
        if let Some(id) = state.locate.get()
            && let Some(index) = rows.with(|rows| {
                rows.iter()
                    .position(|row| row.kind == SidebarRowKind::Session && row.id == id)
            })
        {
            list_ref.scroll_to_index(
                index as u32,
                Percentage(0.5),
                px(0.0),
                !crate::motion::reduce_motion(),
            );
            state.locate.set(None);
        }
    });
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(when(
            move || rows.with(Vec::is_empty),
            move || {
                super::empty_hint(if state.query.with(|query| query.trim().is_empty()) {
                    copy::text("No sessions yet", "还没有会话", "還沒有工作階段")
                } else {
                    copy::text(
                        "No matching sessions",
                        "没有匹配的会话",
                        "沒有符合的工作階段",
                    )
                })
            },
        ))
        .child(
            virtual_list(
                move || rows.with(Vec::len),
                move |position: Position| {
                    let row =
                        computed(move || rows.with(|rows| rows.get(position.index()).cloned()));
                    dynamic({
                        let actions = actions.clone();
                        move || match row.get() {
                            Some(row) => {
                                row_item(row, state, actions.clone(), clock).into_element()
                            }
                            None => view().into_element(),
                        }
                    })
                    .into_element()
                },
            )
            .keys(move || {
                rows.with(|rows| rows.iter().map(SidebarRowView::key).collect::<Vec<_>>())
            })
            .estimated_item_size(px(theme::sidebar_row_height()))
            .element_ref(&list_ref)
            .flex_1(),
        )
}

fn row_item(
    row: SidebarRowView,
    state: TreeState,
    actions: TreeActions,
    clock: ActivityClock,
) -> impl IntoElement {
    let id = row.id.clone();
    let is_session = row.kind == SidebarRowKind::Session;
    let is_empty = row.kind == SidebarRowKind::EmptyWorkspace;
    let card = row.card.clone();
    let inset = theme::SIDEBAR_LIST_PADDING;
    let indent = row.indent
        + if card.is_some() {
            theme::SIDEBAR_CARD_INSET
        } else {
            0.0
        };
    let height = theme::sidebar_row_height();
    let selection = move || state.select_mode.get() && is_session;
    let checked = computed(move || state.selected.with(|selected| selected.contains(&id)));
    let main_action = {
        let row = row.clone();
        let toggle = actions.toggle;
        let select = actions.select;
        move || {
            if !is_session {
                toggle(&row);
            } else if state.select_mode.get_untracked() {
                state.selected.update(|selected| {
                    if let Some(at) = selected.iter().position(|id| id == &row.id) {
                        selected.remove(at);
                    } else {
                        selected.push(row.id.clone());
                    }
                });
            } else {
                select(&row);
                state.pager.close();
            }
        }
    };
    let on_menu = {
        let menu = actions.menu.clone();
        let row = row.clone();
        move || {
            if !state.select_mode.get_untracked() {
                menu(&row);
            }
        }
    };
    let logo = match row.kind {
        SidebarRowKind::Project => icon!("boxes"),
        SidebarRowKind::Workspace => icon!("git-branch"),
        SidebarRowKind::Folder if row.collapsed => icon!("folder"),
        SidebarRowKind::Folder => icon!("folder-open"),
        SidebarRowKind::Group => icon!("layers"),
        SidebarRowKind::Session => icon(agent_icon(&row.detail)),
        SidebarRowKind::EmptyWorkspace => icon!("message-square"),
    };
    let menu_label = match row.kind {
        SidebarRowKind::Project => copy::text("Project actions", "项目操作", "專案操作"),
        SidebarRowKind::Workspace => copy::text("Workspace actions", "工作区操作", "工作區操作"),
        SidebarRowKind::Session => copy::text("Session actions", "会话操作", "工作階段操作"),
        _ => copy::text("Folder actions", "文件夹操作", "資料夾操作"),
    };
    let active = is_session && row.active;
    let detail = (row.kind == SidebarRowKind::Workspace
        && row.detail != row.title
        && !row.detail.is_empty())
    .then(|| row.detail.clone());
    view()
        .h(px(height))
        .w_full()
        .px(px(inset))
        .relative()
        .children(card.into_iter().map(|card| {
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(inset + card.indent))
                .right(px(inset))
                .bg(theme::block_inset)
                .border_l_1()
                .border_r_1()
                .border_color(theme::border_subtle)
                .when(card.top, |frame| {
                    frame.border_t_1().rounded_t(px(theme::SIDEBAR_CARD_RADIUS))
                })
                .when(card.bottom, |frame| {
                    frame.border_b_1().rounded_b(px(theme::SIDEBAR_CARD_RADIUS))
                })
                .pointer_events(PointerEvents::None)
        }))
        .children(row.guides.iter().map(|guide| {
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(inset + guide))
                .w(px(1.0))
                .bg(theme::sidebar_tree_guide)
                .pointer_events(PointerEvents::None)
        }))
        .child(
            view()
                .ml(px(indent))
                .h_full()
                .flex_row()
                .items_center()
                .rounded(px(theme::SIDEBAR_ROW_RADIUS))
                .style_when(
                    move || active || (selection() && checked.get()),
                    |style| style.bg(theme::sidebar_selected_bg()),
                )
                .child(
                    view()
                        .min_w(px(0.0))
                        .h_full()
                        .flex_1()
                        .flex_row()
                        .items_center()
                        .when(!is_empty, |view| {
                            view.role(AccessibilityRole::Button)
                                .label(row.title.clone())
                                .focusable(true)
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_press(main_action)
                                .on_long_press(on_menu.clone())
                        })
                        .child(
                            view()
                                .w(px(theme::SIDEBAR_ICON_SLOT))
                                .flex_shrink_0()
                                .items_center()
                                .justify_center()
                                .child(
                                    when(selection, move || {
                                        view()
                                            .size(px(theme::ICON_SM))
                                            .rounded(px(4.0))
                                            .border_1()
                                            .border_color(theme::border_default)
                                            .items_center()
                                            .justify_center()
                                            .child(when(
                                                move || checked.get(),
                                                || {
                                                    svg(icon!("check"))
                                                        .size(px(12.0))
                                                        .text_color(theme::foreground)
                                                },
                                            ))
                                    })
                                    .otherwise(move || {
                                        svg(logo.clone())
                                            .size(px(if is_session {
                                                theme::SIDEBAR_AGENT_LOGO_SIZE
                                            } else {
                                                theme::SIDEBAR_PROJECT_LOGO_SIZE
                                            }))
                                            .text_color(theme::sidebar_text_secondary)
                                    }),
                                ),
                        )
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .flex_col()
                                .child(
                                    text(row.title.clone())
                                        .min_w(px(0.0))
                                        .truncate()
                                        .text_size(px(theme::font_sidebar_row()))
                                        .font_weight(if active || !is_session && !is_empty {
                                            FontWeight::SEMIBOLD
                                        } else {
                                            FontWeight::NORMAL
                                        })
                                        .text_color(if is_empty {
                                            theme::sidebar_placeholder
                                        } else if is_session {
                                            theme::sidebar_session_title
                                        } else {
                                            theme::sidebar_text_primary
                                        }),
                                )
                                .children(detail.into_iter().map(|detail| {
                                    text(detail)
                                        .truncate()
                                        .text_size(px(theme::font_hint()))
                                        .text_color(theme::sidebar_text_muted)
                                })),
                        )
                        .child(row_metadata(&row, clock)),
                )
                .child(when(
                    move || !is_empty && !state.select_mode.get(),
                    move || {
                        let on_menu = on_menu.clone();
                        let move_row = actions.move_row.clone();
                        let row = row.clone();
                        let travel = animated_value(0.0);
                        view()
                            .size(px(theme::SIDEBAR_ACTION_WIDTH))
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .role(AccessibilityRole::Button)
                            .label(menu_label)
                            .focusable(true)
                            .on_press(on_menu)
                            .pan_values(None, Some(&travel))
                            .pan_enabled(move || state.query.with(|query| query.trim().is_empty()))
                            .on_pan_end(move |event: &PanEvent| {
                                travel.set(0.0);
                                if !event.cancelled {
                                    move_row(&row, event.translation.y.0);
                                }
                            })
                            .child(
                                svg(icon!("ellipsis"))
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::sidebar_text_muted),
                            )
                    },
                )),
        )
}

fn row_metadata(row: &SidebarRowView, clock: ActivityClock) -> impl IntoElement {
    let muted = theme::sidebar_session_meta;
    let mut row_view = view()
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(theme::SPACING_XS))
        .flex_shrink_0();
    if row.pinned {
        row_view = row_view.child(
            svg(icon!("pin"))
                .size(px(theme::ICON_SM))
                .text_color(muted)
                .label(copy::text("Pinned", "已置顶", "已釘選")),
        );
    }
    if row.unread {
        row_view = row_view.child(
            view()
                .size(px(theme::SIDEBAR_UNREAD_DOT))
                .rounded_full()
                .bg(theme::accent_blue),
        );
    }
    row_view = match row.state {
        Some(SessionState::Running) => row_view.child(activity_ring(
            clock,
            if row.auto_continue {
                theme::accent_purple
            } else {
                theme::accent_blue
            },
        )),
        Some(SessionState::Waiting) => row_view.child(
            text("?")
                .font_weight(FontWeight::BOLD)
                .text_color(theme::accent_yellow),
        ),
        Some(SessionState::Failed) => row_view.child(
            view()
                .size(px(theme::SIDEBAR_STATUS_DOT))
                .rounded_full()
                .bg(theme::accent_red),
        ),
        _ if row.auto_continue_paused => row_view.child(
            svg(icon!("pause"))
                .size(px(theme::ICON_SM))
                .text_color(muted),
        ),
        _ if row.scheduled => row_view.child(
            svg(icon!("clock"))
                .size(px(theme::ICON_SM))
                .text_color(muted),
        ),
        _ if !row.metadata.is_empty() => row_view.child(
            text(row.metadata.clone())
                .text_size(px(theme::font_sidebar_meta()))
                .text_color(muted),
        ),
        _ => row_view,
    };
    if row.kind != SidebarRowKind::Session && row.kind != SidebarRowKind::EmptyWorkspace {
        if row.count > 0 {
            row_view = row_view.child(
                text(row.count.to_string())
                    .text_size(px(theme::font_caption()))
                    .text_color(muted),
            );
        }
        row_view = row_view.child(
            svg(if row.collapsed {
                icon!("chevron-right")
            } else {
                icon!("chevron-down")
            })
            .size(px(12.0))
            .text_color(muted),
        );
    }
    row_view
}

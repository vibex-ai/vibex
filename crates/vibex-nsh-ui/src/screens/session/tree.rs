use std::rc::Rc;

use nsh_base::button;
use nsh_core::{
    AccessibilityRole,
    element::{ImageSrc, Position},
    prelude::*,
};

use super::{Pager, SessionState, agent_icon, icon};
use crate::{
    copy, theme,
    ui::{
        activity::{ActivityClock, activity_ring},
        surface,
    },
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
    pub project_mark: Option<(ImageSrc, Hsla)>,
    pub collapsed: bool,
    pub active: bool,
    pub pinned: bool,
    pub unread: bool,
    pub auto_continue: bool,
    pub auto_continue_paused: bool,
    pub scheduled: bool,
    pub state: Option<SessionState>,
    /// Whether a project's new sessions start in a fresh worktree. The row
    /// menu flips the same preference the desktop keeps per project.
    pub project_starts_in_worktree: bool,
}

impl SidebarRowView {
    fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.id)
    }
}

type Rows = Rc<dyn Fn(&str) -> Vec<SidebarRowView>>;
type RowAction = Rc<dyn Fn(&SidebarRowView)>;
/// Raises a row's menu, with how far down the page the row was held.
type RowMenu = Rc<dyn Fn(&SidebarRowView, f32)>;
type BatchScope = Rc<dyn Fn(&SidebarRowView) -> Vec<String>>;
type MoveRow = Rc<dyn Fn(&SidebarRowView, f32)>;
type MoveRowStep = Rc<dyn Fn(&SidebarRowView, bool)>;
type MoveTargets = Rc<dyn Fn(&SidebarRowView) -> (bool, bool)>;

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
    pub batch_scope: BatchScope,
    /// Moves one row one step up or down, for the menu entry a finger cannot
    /// reach by dragging in a sheet.
    pub on_move_row_step: MoveRowStep,
    /// Whether a row has a neighbour above and below it, so the menu only
    /// offers a move that can happen.
    pub row_move_targets: MoveTargets,
}

impl SidebarTree {
    /// One row as the list currently projects it.
    ///
    /// The actions sheet is not inside the list, so it asks for the row it was
    /// opened on rather than carrying a second copy of the tree.
    pub fn row(&self, kind: SidebarRowKind, id: &str) -> Option<SidebarRowView> {
        (self.rows)("")
            .into_iter()
            .find(|row| row.kind == kind && row.id == id)
    }
}

#[derive(Clone)]
pub(super) struct TreeActions {
    pub toggle: RowAction,
    pub select: RowAction,
    pub menu: RowMenu,
    pub move_row: MoveRow,
    pub new_session: RowAction,
    pub batch_scope: BatchScope,
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
    let is_scope = matches!(row.kind, SidebarRowKind::Project | SidebarRowKind::Folder);
    let is_container = matches!(
        row.kind,
        SidebarRowKind::Project
            | SidebarRowKind::Workspace
            | SidebarRowKind::Folder
            | SidebarRowKind::Group
    );
    let adds_sessions = matches!(
        row.kind,
        SidebarRowKind::Project | SidebarRowKind::Workspace
    );
    let card = row.card.clone();
    let in_card = card.is_some();
    let card_indent = card.as_ref().map(|card| card.indent);
    let card_inset = if in_card {
        theme::SIDEBAR_CARD_INSET
    } else {
        0.0
    };
    let inset = theme::SIDEBAR_LIST_PADDING;
    let selection = move || state.select_mode.get() && is_session;
    let scope_selection = move || state.select_mode.get() && is_scope;
    let checked = computed(move || state.selected.with(|selected| selected.contains(&id)));
    let dragging = signal(false);
    let main_action = {
        let row = row.clone();
        let toggle = actions.toggle;
        let select = actions.select;
        move || {
            if !is_session {
                toggle(&row);
            } else if state.select_mode.get_untracked() {
                toggle_batch(state.selected, std::slice::from_ref(&row.id));
            } else {
                select(&row);
                state.pager.close();
            }
        }
    };
    // A row's commands are behind a long press: the row itself stays a title
    // and what trails it.
    let on_menu = {
        let menu = actions.menu.clone();
        let row = row.clone();
        move |event: &PressEvent| {
            if !state.select_mode.get_untracked() {
                crate::motion::tick();
                menu(&row, event.page_position.y.0);
            }
        }
    };
    let (logo, logo_color) = row.project_mark.clone().unwrap_or_else(|| {
        (
            match row.kind {
                SidebarRowKind::Project => icon!("boxes"),
                SidebarRowKind::Workspace => icon!("git-branch"),
                SidebarRowKind::Folder if row.collapsed => icon!("folder"),
                SidebarRowKind::Folder => icon!("folder-open"),
                SidebarRowKind::Group => icon!("layers"),
                SidebarRowKind::Session | SidebarRowKind::EmptyWorkspace => {
                    icon(agent_icon(&row.detail))
                }
            },
            theme::sidebar_text_secondary(),
        )
    });
    let active = is_session && row.active;
    let detail = (row.kind == SidebarRowKind::Workspace
        && row.detail != row.title
        && !row.detail.is_empty())
    .then(|| row.detail.clone());
    view()
        .h(px(theme::sidebar_row_height()))
        .w_full()
        .px(px(inset))
        .py(px(theme::SIDEBAR_ROW_GAP))
        .relative()
        .children(card.into_iter().map(|card| {
            // One row's slice of the card a worktree shares with its sessions.
            view()
                .absolute()
                .top(px(if card.top {
                    theme::SIDEBAR_ROW_GAP
                } else {
                    0.0
                }))
                .bottom(px(if card.bottom {
                    theme::SIDEBAR_ROW_GAP
                } else {
                    0.0
                }))
                .left(px(inset + card.indent))
                .right(px(inset))
                .bg(theme::block_surface)
                .when(card.top, |frame| {
                    frame.rounded_t(px(theme::SIDEBAR_CARD_RADIUS))
                })
                .when(card.bottom, |frame| {
                    frame.rounded_b(px(theme::SIDEBAR_CARD_RADIUS))
                })
                .pointer_events(PointerEvents::None)
        }))
        .children(row.guides.iter().map(|guide| {
            // A guide that starts inside the card moves in with its rows.
            let shift = if card_indent.is_some_and(|indent| *guide >= indent) {
                theme::SIDEBAR_CARD_INSET
            } else {
                0.0
            };
            view()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(inset + guide + shift))
                .w(px(1.0))
                .bg(theme::sidebar_tree_guide)
                .pointer_events(PointerEvents::None)
        }))
        .child(
            // The plate: the open session's is raised off the page, or sunk
            // into the card it is already on.
            view()
                .ml(px(row.indent + card_inset))
                .mr(px(card_inset))
                .flex_1()
                .min_h(px(0.0))
                .flex_row()
                .items_center()
                .rounded(px(theme::SIDEBAR_ROW_RADIUS))
                .when(active && !in_card, |plate| {
                    plate.bg(theme::block_surface).shadow_xs()
                })
                .when(active && in_card, |plate| plate.bg(theme::sidebar_well))
                .style_when(
                    move || dragging.get(),
                    |style| style.bg(theme::sidebar_well()),
                )
                .child(when(scope_selection, {
                    let row = row.clone();
                    let scope = actions.batch_scope.clone();
                    move || {
                        let ids = computed({
                            let row = row.clone();
                            let scope = scope.clone();
                            move || scope(&row)
                        });
                        let selected_count = move || {
                            state.selected.with(|selected| {
                                ids.with(|ids| {
                                    ids.iter().filter(|id| selected.contains(id)).count()
                                })
                            })
                        };
                        // The mark takes the disclosure's place as well: what
                        // it selects is everything the row holds.
                        button()
                            .w(px(theme::SIDEBAR_ROW_INSET
                                + theme::SIDEBAR_CHEVRON_SLOT
                                + theme::SIDEBAR_ICON_SLOT))
                            .h_full()
                            .flex_shrink_0()
                            .items_end()
                            .justify_center()
                            .pr(px((theme::SIDEBAR_ICON_SLOT
                                - theme::SIDEBAR_SELECTION_MARK)
                                / 2.0))
                            .accessibility_label(format!(
                                "{}: {}",
                                copy::text("Select", "选择", "選取"),
                                row.title
                            ))
                            .disabled(move || ids.with(Vec::is_empty))
                            .disabled_style(|style| style.opacity(0.4))
                            .on_press(move || toggle_batch(state.selected, &ids.get_untracked()))
                            .child(surface::check_mark(move || {
                                let count = selected_count();
                                (count > 0).then(|| count == ids.with(Vec::len))
                            }))
                    }
                }))
                .child(
                    view()
                        .min_w(px(0.0))
                        .h_full()
                        .flex_1()
                        .flex_row()
                        .items_center()
                        .pr(px(theme::SIDEBAR_ROW_INSET))
                        .rounded(px(theme::SIDEBAR_ROW_RADIUS))
                        .when(!is_empty, |view| {
                            view.role(AccessibilityRole::Button)
                                .label(row.title.clone())
                                .focusable(true)
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_press(main_action)
                                .on_long_press(on_menu)
                        })
                        .child(when(
                            move || !scope_selection(),
                            move || {
                                let logo = logo.clone();
                                view()
                                    .ml(px(theme::SIDEBAR_ROW_INSET))
                                    .flex_shrink_0()
                                    .flex_row()
                                    .items_center()
                                    .child(
                                        view()
                                            .w(px(theme::SIDEBAR_CHEVRON_SLOT))
                                            .flex_shrink_0()
                                            .items_center()
                                            .justify_center()
                                            .when(is_container, |slot| {
                                                slot.child(
                                                    svg(if row.collapsed {
                                                        icon!("chevron-right")
                                                    } else {
                                                        icon!("chevron-down")
                                                    })
                                                    .size(px(theme::SIDEBAR_CHEVRON_SIZE))
                                                    .text_color(theme::sidebar_text_muted),
                                                )
                                            }),
                                    )
                                    .child(
                                        view()
                                            .w(px(theme::SIDEBAR_ICON_SLOT))
                                            .flex_shrink_0()
                                            .items_center()
                                            .justify_center()
                                            .child(
                                                when(selection, move || {
                                                    surface::check_mark(move || {
                                                        checked.get().then_some(true)
                                                    })
                                                })
                                                .otherwise(move || {
                                                    svg(logo.clone())
                                                        .size(px(if is_session {
                                                            theme::SIDEBAR_AGENT_LOGO_SIZE
                                                        } else {
                                                            theme::SIDEBAR_PROJECT_LOGO_SIZE
                                                        }))
                                                        .text_color(logo_color)
                                                        .when(is_empty, |mark| mark.opacity(0.0))
                                                }),
                                            ),
                                    )
                            },
                        ))
                        .child(
                            view()
                                .flex_1()
                                .min_w(px(0.0))
                                .ml(px(theme::SIDEBAR_ICON_TITLE_GAP))
                                .overflow_hidden()
                                .flex_col()
                                .child(
                                    view()
                                        .w_full()
                                        .min_w(px(0.0))
                                        .overflow_hidden()
                                        .flex_row()
                                        .items_center()
                                        .gap(px(theme::SPACING_SM))
                                        .child(
                                            text(row.title.clone())
                                                .flex_shrink(1.0)
                                                .min_w(px(0.0))
                                                .max_w(relative(1.0))
                                                .overflow_hidden()
                                                .truncate()
                                                .text_size(px(if is_empty {
                                                    theme::font_detail()
                                                } else {
                                                    theme::font_sidebar_row()
                                                }))
                                                .font_weight(if is_container {
                                                    FontWeight::MEDIUM
                                                } else {
                                                    FontWeight::NORMAL
                                                })
                                                .text_color(if is_empty {
                                                    theme::sidebar_text_muted
                                                } else if is_session {
                                                    theme::foreground
                                                } else {
                                                    theme::text_secondary
                                                }),
                                        )
                                        .when(
                                            matches!(
                                                row.kind,
                                                SidebarRowKind::Project | SidebarRowKind::Group
                                            ),
                                            |title| {
                                                title.child(
                                                    text(row.count.to_string())
                                                        .flex_shrink_0()
                                                        .text_size(px(theme::font_sidebar_meta()))
                                                        .text_color(theme::sidebar_text_muted),
                                                )
                                            },
                                        ),
                                )
                                .children(detail.into_iter().map(|detail| {
                                    text(detail)
                                        .min_w(px(0.0))
                                        .max_w(relative(1.0))
                                        .overflow_hidden()
                                        .truncate()
                                        .text_size(px(theme::font_hint()))
                                        .text_color(theme::sidebar_text_muted)
                                })),
                        )
                        .child(row_metadata(&row, clock)),
                )
                .child(when(move || adds_sessions && !state.select_mode.get(), {
                    let row = row.clone();
                    let create = actions.new_session.clone();
                    move || {
                        let row = row.clone();
                        let create = create.clone();
                        button()
                            .size(px(theme::SIDEBAR_ROW_ACTION))
                            .mr(px(theme::SIDEBAR_ROW_INSET))
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .bg(theme::sidebar_well)
                            .hit_slop(px(theme::SPACING_SM))
                            .active(|style| style.opacity(0.6))
                            .accessibility_label(copy::text(
                                "New session here",
                                "在此新建会话",
                                "在此新增工作階段",
                            ))
                            .on_press(move || create(&row))
                            .child(
                                svg(icon!("plus"))
                                    .size(px(14.0))
                                    .text_color(theme::text_secondary),
                            )
                    }
                }))
                .child(when(
                    // Rows are put in order while they are being selected:
                    // the grip is the one place a drag does not scroll.
                    move || {
                        !is_empty
                            && state.select_mode.get()
                            && state.query.with(|query| query.trim().is_empty())
                    },
                    move || {
                        let move_row = actions.move_row.clone();
                        let row = row.clone();
                        let travel = animated_value(0.0);
                        view()
                            .w(px(theme::SIDEBAR_ROW_ACTION))
                            .h_full()
                            .mr(px(theme::SPACING_XS))
                            .flex_shrink_0()
                            .items_center()
                            .justify_center()
                            .label(copy::text("Drag to reorder", "拖动排序", "拖曳排序"))
                            .pan_values(None, Some(&travel))
                            .pan_enabled(true)
                            .on_pan_start(move |_: &PanEvent| {
                                crate::motion::tick();
                                dragging.set(true);
                            })
                            .on_pan_end(move |event: &PanEvent| {
                                travel.set(0.0);
                                dragging.set(false);
                                if !event.cancelled {
                                    move_row(&row, event.translation.y.0);
                                }
                            })
                            .child(
                                svg(icon!("grip-vertical"))
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::sidebar_text_muted),
                            )
                    },
                )),
        )
}

pub(super) fn toggle_batch(selection: Signal<Vec<String>>, ids: &[String]) {
    if ids.is_empty() {
        return;
    }
    selection.update(|selected| {
        if ids.iter().all(|id| selected.contains(id)) {
            selected.retain(|id| !ids.contains(id));
        } else {
            for id in ids {
                if !selected.contains(id) {
                    selected.push(id.clone());
                }
            }
        }
    });
}

/// What trails a row's title: how the session stands, when it was last
/// touched, and the marks it carries.
fn row_metadata(row: &SidebarRowView, clock: ActivityClock) -> impl IntoElement {
    let muted = theme::sidebar_text_muted;
    let mut row_view = view()
        .flex_row()
        .items_center()
        .justify_end()
        .gap(px(theme::SPACING_XS))
        .ml(px(theme::SPACING_SM))
        .flex_shrink_0();
    if row.pinned {
        row_view = row_view.child(
            svg(icon!("pin"))
                .size(px(theme::SIDEBAR_META_ICON))
                .text_color(muted)
                .label(copy::text("Pinned", "已置顶", "已釘選")),
        );
    }
    if row.auto_continue_paused {
        row_view = row_view.child(
            svg(icon!("pause"))
                .size(px(theme::SIDEBAR_META_ICON))
                .text_color(muted),
        );
    }
    if row.scheduled {
        row_view = row_view.child(
            svg(icon!("clock"))
                .size(px(theme::SIDEBAR_META_ICON))
                .text_color(muted),
        );
    }
    let state = if row.kind == SidebarRowKind::Group && !row.collapsed {
        None
    } else {
        row.state
    };
    row_view = match state {
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
                .text_size(px(theme::font_sidebar_meta()))
                .font_weight(FontWeight::BOLD)
                .text_color(theme::accent_yellow),
        ),
        Some(SessionState::Failed) => row_view.child(
            view()
                .size(px(theme::SIDEBAR_STATUS_DOT))
                .rounded_full()
                .bg(theme::accent_red),
        ),
        _ if !row.metadata.is_empty() => row_view.child(
            text(row.metadata.clone())
                .text_size(px(theme::font_sidebar_meta()))
                .text_color(muted),
        ),
        _ => row_view,
    };
    if row.unread {
        row_view = row_view.child(
            view()
                .size(px(theme::SIDEBAR_UNREAD_DOT))
                .rounded_full()
                .bg(theme::accent_blue),
        );
    }
    row_view
}

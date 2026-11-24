use std::{collections::HashMap, rc::Rc};

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
        menu, surface,
    },
};

/// How near the list's edge a carried row starts scrolling it.
const DRAG_EDGE: f32 = 56.0;

/// How far a held row must travel before it takes the touches for a drag:
/// past the hand's own tremor, under the list's scroll slop, so carrying a
/// row wins the gesture against scrolling.
const DRAG_SLOP: f32 = 3.0;

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
    pub(super) fn key(&self) -> String {
        format!("{:?}:{}", self.kind, self.id)
    }
}

/// The row a finger is carrying: where it started, how far it has come, and
/// what the page draws in its place while it travels.
#[derive(Clone, PartialEq)]
pub struct DraggedRow {
    /// The row's key, so the list hides the copy it is carrying.
    pub key: String,
    /// The row's top edge on the page, where it started.
    pub top: f32,
    /// How far the finger has carried it.
    pub delta: f32,
    pub mark: Option<ImageSrc>,
    pub title: SharedString,
    /// The title's type size, as the row sets it.
    pub title_size: f32,
    pub trailing: SharedString,
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
    /// The row a finger is carrying, shared with the page that draws it.
    pub drag: Signal<Option<DraggedRow>>,
    /// The menu the page has raised: it names the row a hold is about, so a
    /// drag is decided from page state and survives the list rebuilding the
    /// row under the finger.
    pub menu: Signal<Option<menu::Menu>>,
    /// How far each row has moved over to open the carried row's slot, by
    /// key. The page owns it, like the carry itself.
    pub shifts: Signal<HashMap<String, f32>>,
    /// The list's viewport on the page (top, height), its scroll offset and
    /// how far a drag near an edge has scrolled it so far.
    pub list: Signal<Option<(f32, f32)>>,
    pub list_offset: Signal<f32>,
    pub scroll_travel: Signal<f32>,
    pub auto_scroll: Signal<f32>,
    /// The list itself, for the page's edge scrolling.
    pub list_ref: ElementRef,
}

pub(super) fn list(state: TreeState, actions: TreeActions) -> impl IntoElement {
    let rows = state.rows;
    let clock = ActivityClock::start();
    let list_ref = state.list_ref;
    // The carried row and the slot it is over, as values that only change
    // when the finger crosses a row: a row between the two moves over as a
    // step, not on every pixel of the drag.
    let carried = computed(move || {
        state
            .drag
            .with(|drag| drag.as_ref().map(|drag| drag.key.clone()))
    });
    let slot = computed(move || {
        let scrolled = state.scroll_travel.get();
        state.drag.with(|drag| {
            drag.as_ref().map(|drag| {
                (
                    drag.key.clone(),
                    ((drag.delta + scrolled) / theme::sidebar_row_height().max(1.0)).round() as i32,
                )
            })
        })
    });
    // When the finger crosses a row, the rows between the carried row and
    // that slot move over one place; the signals carry the transition.
    {
        let rows = state.rows;
        let carried = carried;
        let shifts = state.shifts;
        effect(move || {
            let slot = slot.get();
            let carried = carried.get();
            rows.with(|rows| {
                let mut wanted = HashMap::new();
                for (index, row) in rows.iter().enumerate() {
                    let key = row.key();
                    let target = if carried.as_deref() == Some(key.as_str()) {
                        0.0
                    } else {
                        drag_shift(rows, slot.clone(), index)
                    };
                    if target != 0.0 {
                        wanted.insert(key, target);
                    }
                }
                shifts.set(wanted);
            });
        });
    }
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
                                let is_carried = carried
                                    .with(|carried| carried.as_deref() == Some(row.key().as_str()));
                                row_item(row, state, actions.clone(), clock, is_carried)
                                    .into_element()
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
            .on_scroll(move |event: &ScrollEvent| state.list_offset.set(event.offset.y.0))
            .flex_1(),
        )
}

/// How far a row moves over to open the slot the carried row would land in.
fn drag_shift(rows: &[SidebarRowView], slot: Option<(String, i32)>, index: usize) -> f32 {
    let Some((key, slots)) = slot else {
        return 0.0;
    };
    let height = theme::sidebar_row_height();
    let Some(from) = rows.iter().position(|row| row.key() == key) else {
        return 0.0;
    };
    let target = (from as i32 + slots).clamp(0, rows.len().saturating_sub(1) as i32) as usize;
    if from < target && index > from && index <= target {
        -height
    } else if target < from && index >= target && index < from {
        height
    } else {
        0.0
    }
}

/// What trails a row's title, as the card a drag carries shows it.
fn row_trailing(row: &SidebarRowView) -> String {
    if !row.metadata.is_empty() {
        row.metadata.clone()
    } else if row.count > 0 {
        row.count.to_string()
    } else {
        String::new()
    }
}

fn row_item(
    row: SidebarRowView,
    state: TreeState,
    actions: TreeActions,
    clock: ActivityClock,
    carried: bool,
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
    // Folder and group rows take the list's smaller title step, the same
    // one the sessions under them use, so the tree's row titles read as one
    // size below the project and workspace scopes.
    let is_nested = matches!(row.kind, SidebarRowKind::Folder | SidebarRowKind::Group);
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
        let toggle = actions.toggle.clone();
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
    // A row's commands are behind a press held still, and a drag that
    // follows the hold moves the row. Holding raises the menu at once; a
    // finger that then travels puts it away and carries the row instead.
    //
    // What is carried lives in the page, not in this row: the list rebuilds
    // its rows while a gesture is in flight, and a row held in a local
    // signal would forget the press it is in the middle of.
    let key = row.key();
    let shift_key = key.clone();
    let on_hold = {
        let menu = actions.menu.clone();
        let menu_row = row.clone();
        move |event: &PressEvent| {
            if !state.select_mode.get_untracked() {
                crate::motion::tick();
                // Where the row's top is, not where the finger is: the menu
                // stands against the row itself. The press lands in the row's
                // inner content, which starts one row gap below its top.
                let top = event.page_position.y.0 - event.position.y.0 - theme::SIDEBAR_ROW_GAP;
                menu(&menu_row, top);
            }
        }
    };
    let start_drag = {
        let mark = logo.clone();
        let title: SharedString = row.title.clone().into();
        let title_size = if is_session || is_empty || is_nested {
            theme::font_detail()
        } else {
            theme::font_sidebar_row()
        };
        let trailing: SharedString = row_trailing(&row).into();
        let key = key.clone();
        // A folder or group dragged while open collapses first: the drag
        // carries one row, and the row cannot be dropped into the space its
        // own children used to occupy.
        let toggle = actions.toggle.clone();
        let drag_row = row.clone();
        move || {
            // The list rebuilds the row when a nested row collapses as the
            // drag begins, and the rebuilt element is granted the touch
            // again. A row already being carried keeps its carry rather
            // than starting over.
            if state
                .drag
                .with_untracked(|drag| drag.as_ref().is_some_and(|drag| drag.key == key))
            {
                return;
            }
            let Some(top) = state.menu.with_untracked(|menu| {
                menu.as_ref()
                    .and_then(|menu| menu.row.as_ref())
                    .map(|row| row.top)
            }) else {
                return;
            };
            if is_nested && !drag_row.collapsed {
                toggle(&drag_row);
            }
            state.scroll_travel.set(0.0);
            state.auto_scroll.set(0.0);
            state.drag.set(Some(DraggedRow {
                key: key.clone(),
                top,
                delta: 0.0,
                mark: Some(mark.clone()),
                title: title.clone(),
                title_size,
                trailing: trailing.clone(),
            }));
        }
    };
    let on_drag_move = {
        let key = key.clone();
        move |event: &ResponderEvent| {
            let delta = event.gesture.delta.y.0;
            state.drag.update(|drag| {
                if let Some(drag) = drag
                    && drag.key == key
                {
                    drag.delta = delta;
                }
            });
            // Near an edge of the list the page scrolls it, so a row can be
            // carried past what is on screen.
            let y = event.page.y.0;
            let edge = state.list.with_untracked(|list| {
                list.map_or(0.0, |(top, height)| {
                    if y < top + DRAG_EDGE {
                        -1.0
                    } else if y > top + height - DRAG_EDGE {
                        1.0
                    } else {
                        0.0
                    }
                })
            });
            state.auto_scroll.set(edge);
        }
    };
    let on_drag_end = {
        let key = key.clone();
        let move_row = actions.move_row.clone();
        let moved_row = row.clone();
        move |_: &ResponderEvent| {
            let delta = state.drag.with_untracked(|drag| {
                drag.as_ref()
                    .filter(|drag| drag.key == key)
                    .map_or(0.0, |drag| drag.delta)
            }) + state.scroll_travel.get_untracked();
            state.auto_scroll.set(0.0);
            state.scroll_travel.set(0.0);
            let ours = state
                .drag
                .with_untracked(|drag| drag.as_ref().is_some_and(|drag| drag.key == key));
            if ours {
                state.drag.set(None);
                move_row(&moved_row, delta);
            }
        }
    };
    let on_drag_cancelled = {
        let key = key.clone();
        move |_: &ResponderEvent| {
            let ours = state
                .drag
                .with_untracked(|drag| drag.as_ref().is_some_and(|drag| drag.key == key));
            if ours {
                state.drag.set(None);
            }
            state.auto_scroll.set(0.0);
            state.scroll_travel.set(0.0);
        }
    };
    // The row is armed by the hold and taken by the first travel, which is
    // decided again on every move from page state, so a row rebuilt under
    // the finger still claims the gesture. The menu names the row a hold
    // is about; once the drag has begun the carry itself names it, so a
    // row rebuilt by the collapse a drag triggered still claims.
    let on_drag_claim = {
        let key = key.clone();
        move |request: &ClaimRequest| {
            let armed = state.menu.with_untracked(|menu| {
                menu.as_ref()
                    .and_then(|menu| menu.row.as_ref())
                    .is_some_and(|row| row.key == key)
            }) || state
                .drag
                .with_untracked(|drag| drag.as_ref().is_some_and(|drag| drag.key == key));
            if armed && request.event.gesture.delta.y.0.abs() > DRAG_SLOP {
                request.claim();
            }
        }
    };
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
        // A row between the carried row and the slot it would land in moves
        // one place over, opening that slot; the carried row itself is drawn
        // by the page, so its own place stays empty. The transform animates,
        // so the rows slide rather than jump.
        .translate_y(move || {
            px(state
                .shifts
                .with(|shifts| shifts.get(&shift_key).copied().unwrap_or(0.0)))
        })
        .transition_transform()
        .duration(160.0)
        .when(carried, |row| row.opacity(0.0))
        // An open folder's children hang from a line centred under its
        // icon, right where the leading chevron used to be.
        .children(
            (row.kind == SidebarRowKind::Folder && !row.collapsed).then(|| {
                view()
                    .absolute()
                    .left(px(inset
                        + row.indent
                        + card_inset
                        + theme::SIDEBAR_ROW_INSET
                        + theme::SIDEBAR_ICON_SLOT / 2.0))
                    .top(px(
                        theme::sidebar_row_height() / 2.0 + theme::SIDEBAR_PROJECT_LOGO_SIZE / 2.0
                    ))
                    .bottom(px(0.0))
                    .w(px(1.0))
                    .bg(theme::sidebar_tree_guide())
                    .pointer_events(PointerEvents::None)
            }),
        )
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
                    move || dragging.get() || carried,
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
                                .on_long_press(on_hold)
                                // The drag takes the touches from the list
                                // only once a held row moves; a scroll that
                                // starts on a row still scrolls.
                                .claim_touches(TouchClaim::on_move().ask(on_drag_claim))
                                .keep_touches(true)
                                .on_responder_grant(move |_: &ResponderEvent| {
                                    start_drag();
                                })
                                .on_responder_move(on_drag_move)
                                .on_responder_release(on_drag_end)
                                .on_responder_terminate(on_drag_cancelled)
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
                                                // A session's name is the
                                                // list's prose; the folders
                                                // and groups that hold them
                                                // take the same step, while
                                                // the project and workspace
                                                // scopes above keep the
                                                // larger one.
                                                .text_size(px(
                                                    if is_empty || is_session || is_nested {
                                                        theme::font_detail()
                                                    } else {
                                                        theme::font_sidebar_row()
                                                    },
                                                ))
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
                                        )
                                        // A folder or worktree keeps its
                                        // disclosure beside its name: the
                                        // leading gutter belongs to the tree
                                        // now.
                                        .children(
                                            matches!(
                                                row.kind,
                                                SidebarRowKind::Folder | SidebarRowKind::Workspace
                                            )
                                            .then(
                                                || {
                                                    svg(if row.collapsed {
                                                        icon!("chevron-right")
                                                    } else {
                                                        icon!("chevron-down")
                                                    })
                                                    .size(px(theme::SIDEBAR_CHEVRON_SIZE))
                                                    .flex_shrink_0()
                                                    .text_color(theme::sidebar_text_muted)
                                                },
                                            ),
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

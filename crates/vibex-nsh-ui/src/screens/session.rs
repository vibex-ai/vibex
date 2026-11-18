//! The session shell: the drawer of sessions, the timeline and the composer.
//!
//! The compact phone composition the GPUI client uses, drawn with `nsh`: a
//! top bar that opens the session drawer, the transcript as a virtualized list
//! of rows, and a composer pinned above the keyboard. The app's session service
//! owns the data and the actions; this module binds them and decides how a row
//! looks.
//!
//! One row per timeline projection (`vibex-desktop-model`'s `TimelineRow`):
//! user messages as bubbles, Agent messages as prose, tools and notices as
//! cards, errors in the destructive tone. Markdown is still plain text here;
//! the `TextView` renderer replaces it once the row shapes settle.

use std::rc::Rc;

use nsh_base::{SheetEvent, button, sheet};
use nsh_core::{
    element::{ImageSrc, Position},
    prelude::*,
};

use crate::{
    copy, theme,
    ui::{action, brand, field},
};

/// The height of the top bar, above the safe area.
const HEADER_HEIGHT: f32 = 48.0;
/// The drawer's width on a phone.
const DRAWER_WIDTH: f32 = 300.0;

/// How a session is doing, for the drawer's dot.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SessionState {
    /// The Agent is working.
    Running,
    /// Waiting for a person: a question or a permission.
    Waiting,
    /// Idle.
    Idle,
    /// The last turn failed.
    Failed,
}

impl SessionState {
    fn dot(self) -> Hsla {
        match self {
            SessionState::Running => theme::color_token("chart-1"),
            SessionState::Waiting => theme::color_token("warning"),
            SessionState::Idle => theme::muted_foreground(),
            SessionState::Failed => theme::accent_red(),
        }
    }
}

/// A session in the drawer.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SessionSummary {
    pub id: String,
    pub title: String,
    /// The Agent's name.
    pub agent: String,
    /// When it last spoke, already formatted ("2m", "14:05").
    pub meta: String,
    pub state: SessionState,
}

/// What kind of timeline row this is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EntryKind {
    /// What the person wrote.
    User,
    /// What the Agent answered.
    Agent,
    /// The Agent's reasoning.
    Reasoning,
    /// A tool call or command.
    Tool,
    /// A system or Git notice.
    Notice,
    /// A failure.
    Error,
}

/// One row of the transcript.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct TimelineEntry {
    pub id: String,
    pub kind: EntryKind,
    /// A tool's name, the Agent's name, a row's heading.
    pub title: String,
    /// The text itself.
    pub body: String,
    /// A short status ("done", "running", a path).
    pub meta: String,
    pub streaming: bool,
}

/// What the shell may do, and the state it shows.
#[derive(Clone)]
pub struct SessionHost {
    pub sessions: Signal<Vec<SessionSummary>>,
    pub active: Signal<Option<String>>,
    pub timeline: Signal<Vec<TimelineEntry>>,
    pub draft: Signal<String>,
    pub sending: Signal<bool>,
    pub error: Signal<Option<SharedString>>,
    /// The connected runtime's label, shown in the drawer.
    pub runtime: Signal<Option<SharedString>>,
    pub on_select: Rc<dyn Fn(&str)>,
    pub on_send: Rc<dyn Fn(&str)>,
    pub on_refresh: Rc<dyn Fn()>,
    pub on_disconnect: Rc<dyn Fn()>,
}

/// The session shell.
pub fn session(host: SessionHost) -> impl IntoElement {
    let drawer_open = signal(false);

    view()
        .flex_1()
        .bg(theme::background)
        .flex_col()
        .child(header(host.clone(), drawer_open))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .child(timeline_view(host.clone()))
                .child(when(move || host.error.get().is_some(), {
                    let error = host.error;
                    move || {
                        text(move || error.get().unwrap_or_default())
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::accent_red)
                            .px(px(theme::SPACING_LG))
                            .pb(px(theme::SPACING_SM))
                    }
                })),
        )
        .child(composer(host.clone()))
        .child(drawer(host, drawer_open))
}

/// The top bar: the drawer, the title, a refresh.
fn header(host: SessionHost, drawer_open: Signal<bool>) -> impl IntoElement {
    let sessions = host.sessions;
    let active = host.active;
    let refresh = host.on_refresh.clone();
    view()
        .w_full()
        .pt_safe_area()
        .border_b_1()
        .border_b_color(theme::border_subtle)
        .child(
            view()
                .w_full()
                .h(px(HEADER_HEIGHT))
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .px(px(theme::SPACING_SM))
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/menu.svg")),
                    copy::text("Sessions", "会话", "工作階段"),
                    move || drawer_open.set(true),
                ))
                .child(
                    text(move || {
                        let active = active.get();
                        sessions
                            .with(|sessions| {
                                active.and_then(|active| {
                                    sessions
                                        .iter()
                                        .find(|session| session.id == active)
                                        .map(|session| session.title.clone())
                                })
                            })
                            .unwrap_or_else(|| "Vibex".to_owned())
                    })
                    .flex_1()
                    .truncate()
                    .text_size(px(theme::FONT_HEADING))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(theme::foreground),
                )
                .child(action::icon_button(
                    icon(include_str!("../../assets/icons/refresh.svg")),
                    copy::text("Refresh", "刷新", "重新整理"),
                    move || refresh(),
                )),
        )
}

/// The transcript: a row per entry, or what to do when there is none.
fn timeline_view(host: SessionHost) -> impl IntoElement {
    let timeline = host.timeline;
    let active = host.active;
    let error = host.error;
    view()
        .flex_1()
        .min_h(px(0.0))
        .child(when(
            move || active.get().is_none(),
            move || {
                empty_hint(copy::text(
                    "Choose a session",
                    "选择一个会话",
                    "選擇一個工作階段",
                ))
            },
        ))
        .child(when(
            move || active.get().is_some() && timeline.with(|entries| entries.is_empty()),
            move || empty_hint(copy::text("No messages yet", "还没有消息", "還沒有訊息")),
        ))
        .child(when(
            move || active.get().is_some() && error.get().is_some(),
            move || {
                view().flex_1().items_center().justify_center().child(
                    text(move || error.get().unwrap_or_default())
                        .text_size(px(theme::FONT_BODY))
                        .text_color(theme::accent_red),
                )
            },
        ))
        .child(when(
            move || timeline.with(|entries| !entries.is_empty()),
            move || {
                virtual_list(
                    move || timeline.with(|entries| entries.len()),
                    move |at: Position| entry_row(at, timeline),
                )
                .keys(move || {
                    timeline.with(|entries| {
                        entries
                            .iter()
                            .map(|entry| entry.id.clone())
                            .collect::<Vec<_>>()
                    })
                })
                .estimated_item_size(px(72.0))
                .flex_1()
            },
        ))
}

fn empty_hint(message: &'static str) -> impl IntoElement {
    view().flex_1().items_center().justify_center().child(
        text(message)
            .text_size(px(theme::FONT_DETAIL))
            .text_color(theme::text_muted),
    )
}

/// One timeline row, shaped by its kind.
///
/// The row is made once and reused by the list, so each shape is a region
/// bound to the row's kind: when a cell shows another kind, its branch swaps
/// without rebuilding the cell.
fn entry_row(at: Position, entries: Signal<Vec<TimelineEntry>>) -> impl IntoElement {
    let entry = computed(move || entries.with(|entries| entries.get(at.index()).cloned()));
    view()
        .w_full()
        .px(px(theme::SPACING_LG))
        .py(px(theme::SPACING_SM))
        .child(entry_element(entry))
}

fn entry_element(entry: Memo<Option<TimelineEntry>>) -> impl IntoElement {
    let kind = computed(move || entry.with(|e| e.as_ref().map(|e| e.kind)));
    let body =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.body.clone()).unwrap_or_default()));
    let title =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.title.clone()).unwrap_or_default()));
    let meta =
        computed(move || entry.with(|e| e.as_ref().map(|e| e.meta.clone()).unwrap_or_default()));
    let streaming = computed(move || entry.with(|e| e.as_ref().is_some_and(|e| e.streaming)));

    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::User)),
            move || user_bubble(body),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Agent)),
            move || agent_prose(title, body, streaming),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Reasoning)),
            move || muted_prose(copy::text("Thought", "思考", "思考"), body),
        ))
        .child(when(
            move || kind.with(|kind| matches!(kind, Some(EntryKind::Tool | EntryKind::Notice))),
            move || tool_card(title, body, meta),
        ))
        .child(when(
            move || kind.with(|kind| *kind == Some(EntryKind::Error)),
            move || error_card(body),
        ))
}

fn user_bubble(body: Memo<String>) -> impl IntoElement {
    view().w_full().flex_row().justify_end().child(
        view()
            .max_w(px(320.0))
            .rounded(px(14.0))
            .bg(theme::background_dim)
            .px(px(theme::SPACING_MD))
            .py(px(theme::SPACING_SM))
            .child(
                text(move || body.get())
                    .text_size(px(theme::FONT_BODY))
                    .line_height(theme::line_height_for(theme::FONT_BODY))
                    .text_color(theme::foreground),
            ),
    )
}

fn agent_prose(title: Memo<String>, body: Memo<String>, streaming: Memo<bool>) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_XS))
                .child(
                    text(move || title.get())
                        .text_size(px(theme::FONT_MICRO))
                        .font_weight(FontWeight::SEMIBOLD)
                        .text_color(theme::text_muted),
                )
                .child(when(
                    move || streaming.get(),
                    || {
                        text("…")
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted)
                    },
                )),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_BODY))
                .line_height(theme::line_height_for(theme::FONT_BODY))
                .text_color(theme::foreground),
        )
}

fn muted_prose(label: &'static str, body: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .flex_col()
        .gap(px(theme::SPACING_XS))
        .child(
            text(label)
                .text_size(px(theme::FONT_MICRO))
                .text_color(theme::text_muted),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_DETAIL))
                .line_height(theme::line_height_for(theme::FONT_DETAIL))
                .text_color(theme::text_secondary),
        )
}

fn tool_card(title: Memo<String>, body: Memo<String>, meta: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_subtle)
        .bg(theme::background_card)
        .p(px(theme::SPACING_MD))
        .gap(px(theme::SPACING_XS))
        .child(
            view()
                .flex_row()
                .items_center()
                .gap(px(theme::SPACING_SM))
                .child(
                    text(move || title.get())
                        .flex_1()
                        .truncate()
                        .text_size(px(theme::FONT_DETAIL))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(theme::foreground),
                )
                .child(
                    text(move || meta.get())
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::text_muted),
                ),
        )
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_CAPTION))
                .line_height(theme::line_height_for(theme::FONT_CAPTION))
                .text_color(theme::text_secondary),
        )
}

fn error_card(body: Memo<String>) -> impl IntoElement {
    view()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::accent_red)
        .p(px(theme::SPACING_MD))
        .child(
            text(move || body.get())
                .text_size(px(theme::FONT_DETAIL))
                .text_color(theme::accent_red),
        )
}

/// The composer, kept above the software keyboard.
fn composer(host: SessionHost) -> impl IntoElement {
    let draft = host.draft;
    let sending = host.sending;
    let send = host.on_send.clone();
    let submit = {
        let send = send.clone();
        move || {
            if sending.get_untracked() {
                return;
            }
            let text = draft.get_untracked();
            let text = text.trim();
            if text.is_empty() {
                return;
            }
            send(text);
            draft.set(String::new());
        }
    };
    view()
        .w_full()
        .flex_row()
        .items_end()
        .gap(px(theme::SPACING_SM))
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_SM))
        .border_t_1()
        .border_t_color(theme::border_subtle)
        .bg(theme::background)
        .pb_keyboard()
        .child(
            field::text_field(
                draft,
                copy::text("Message the Agent…", "给 Agent 发消息…", "傳訊息給 Agent…"),
            )
            .multiline(true)
            .min_h(px(theme::TOUCH_TARGET))
            .max_h(px(120.0))
            .flex_1(),
        )
        .child(
            action::primary(
                copy::text("Send", "发送", "傳送"),
                icon(include_str!("../../assets/icons/send.svg")),
                submit,
            )
            .disabled(move || sending.get() || draft.with(|draft| draft.trim().is_empty()))
            .disabled_style(|style| style.opacity(0.4)),
        )
}

/// The session drawer, from the left edge.
fn drawer(host: SessionHost, open: Signal<bool>) -> impl IntoElement {
    let sessions = host.sessions;
    let active = host.active;
    let runtime = host.runtime;
    let on_select = host.on_select.clone();
    let on_disconnect = host.on_disconnect.clone();
    sheet()
        .side(Placement::Left)
        .open(move || Some(open.get()))
        .close_on_swipe(true)
        .on_open_change(move |event: &SheetEvent| {
            if let SheetEvent::OpenChange(next, _) = event {
                open.set(*next);
            }
        })
        .child(
            view()
                .w(px(DRAWER_WIDTH))
                .h_full()
                .flex_col()
                .bg(theme::background_card)
                .pt_safe_area_plus(px(theme::SPACING_LG))
                .pb_safe_area_plus(px(theme::SPACING_MD))
                .child(
                    view()
                        .px(px(theme::SPACING_LG))
                        .pb(px(theme::SPACING_MD))
                        .child(brand::wordmark()),
                )
                .child(
                    view().flex_1().min_h(px(0.0)).child(
                        scroll().flex_1().child(
                            view()
                                .gap(px(theme::SPACING_XS))
                                .px(px(theme::SPACING_SM))
                                .child(each(
                                    move || sessions.get(),
                                    |session| session.id.clone(),
                                    {
                                        let on_select = on_select.clone();
                                        move |session, _| {
                                            session_row(session, active, on_select.clone(), open)
                                        }
                                    },
                                )),
                        ),
                    ),
                )
                .child(
                    view()
                        .border_t_1()
                        .border_t_color(theme::border_subtle)
                        .px(px(theme::SPACING_LG))
                        .pt(px(theme::SPACING_MD))
                        .gap(px(theme::SPACING_XS))
                        .child(
                            text(move || {
                                runtime.get().unwrap_or_else(|| {
                                    copy::text("No runtime", "未连接运行时", "未連線執行環境")
                                        .into()
                                })
                            })
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted),
                        )
                        .child(
                            action::secondary(
                                copy::text("Disconnect", "断开连接", "中斷連線"),
                                icon(include_str!("../../assets/icons/plug-zap.svg")),
                                false,
                                move || on_disconnect(),
                            )
                            .w_full(),
                        ),
                ),
        )
}

fn session_row(
    session: ReadSignal<SessionSummary>,
    active: Signal<Option<String>>,
    on_select: Rc<dyn Fn(&str)>,
    open: Signal<bool>,
) -> impl IntoElement {
    button()
        .w_full()
        .rounded(px(theme::RADIUS_CONTROL))
        .px(px(theme::SPACING_MD))
        .py(px(theme::SPACING_SM))
        .flex_row()
        .items_center()
        .gap(px(theme::SPACING_SM))
        .active(|style| style.opacity(0.85))
        .on_press(move || {
            on_select(&session.get_untracked().id);
            open.set(false);
        })
        .child(
            view()
                .w(px(8.0))
                .h(px(8.0))
                .flex_shrink_0()
                .rounded(px(4.0))
                .bg(move || session.get().state.dot()),
        )
        .child(
            view()
                .flex_1()
                .min_w(px(0.0))
                .gap(px(2.0))
                .child(
                    text(move || session.get().title.clone())
                        .truncate()
                        .text_size(px(theme::FONT_BODY))
                        .font_weight(FontWeight::MEDIUM)
                        .text_color(move || {
                            let id = session.get().id;
                            if active.with(|active| active.as_deref() == Some(id.as_str())) {
                                theme::foreground()
                            } else {
                                theme::text_secondary()
                            }
                        }),
                )
                .child(
                    text(move || {
                        let session = session.get();
                        format!("{} · {}", session.agent, session.meta)
                    })
                    .truncate()
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(theme::text_muted),
                ),
        )
}

/// An SVG from this crate's assets, drawn inline.
fn icon(markup: &'static str) -> ImageSrc {
    ImageSrc::svg_markup(markup)
}

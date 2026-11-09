//! Scheduling for unsent composer drafts. The existing queue owns the message
//! until its deadline; only the ordinary submission path creates a timeline turn.

use super::*;
use chrono::{Local, NaiveDateTime, TimeZone as _};
use gpui::{App, InteractiveElement as _, KeyBinding};
// The popover answers Enter and Space with its own confirm action; the panel
// claims it while it is open.
use gpui_base::actions::Confirm as PopoverConfirm;
use gpui_component::{
    calendar::{Calendar, CalendarEvent, CalendarState, Date},
    input::Enter as InputEnter,
    popover::Popover,
    time_field::{HourCycle, TimeField, TimeFieldEvent, TimeFieldState, TimePrecision},
};

const MAX_COUNTDOWN_SECONDS: u64 = 365 * 24 * 60 * 60;
/// The time field's own key context, nested inside the popover content.
///
/// `PopoverContent` binds Enter to `NoAction` so a focused control does not
/// confirm the surface. The segmented time field has no Enter binding of its
/// own, so without this context that `NoAction` would swallow the keystroke
/// before the panel's `Confirm` handler could commit the deadline.
const TIME_CONTEXT: &str = "MessageScheduleTimeField";

#[derive(Default)]
struct Bindings;

impl gpui::Global for Bindings {}

fn init(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    cx.set_global(Bindings);
    cx.bind_keys([KeyBinding::new(
        "enter",
        PopoverConfirm { secondary: false },
        Some(TIME_CONTEXT),
    )]);
}

const CLOCK_ICON: &str = "icons/vibex/clock.svg";
/// The inline calendar's day grid is seven `size_7` cells (1.75rem each) with
/// six `gap_0p5` gaps (0.125rem), so 13rem is exactly as wide as the calendar
/// and every other row in the panel shares that one spine. Rem units, not
/// pixels: the grid is built from the same scale and has to keep matching it
/// when the interface font size changes.
const MESSAGE_SCHEDULE_PANEL_REM: f32 = 13.0;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum MessageSchedule {
    At(i64),
    After(u64),
}

impl MessageSchedule {
    pub(super) fn deadline(self, now_ms: i64) -> Result<i64, &'static str> {
        match self {
            Self::At(at_ms) if at_ms > now_ms => Ok(at_ms),
            Self::At(_) => Err(locale::text(
                "Choose a future time.",
                "请选择未来的时间。",
                "請選擇未來的時間。",
            )),
            Self::After(seconds) if (1..=MAX_COUNTDOWN_SECONDS).contains(&seconds) => now_ms
                .checked_add((seconds * 1_000) as i64)
                .ok_or_else(invalid_countdown),
            Self::After(_) => Err(invalid_countdown()),
        }
    }

    fn summary(self) -> String {
        match self {
            Self::At(at_ms) => format_send_time(at_ms),
            Self::After(seconds) => {
                let duration = format_remaining(seconds.saturating_mul(1_000) as i64);
                match locale::current_locale() {
                    locale::ResolvedLocale::En => format!("Send in {duration}"),
                    locale::ResolvedLocale::ZhCn => format!("{duration} 后发送"),
                    locale::ResolvedLocale::ZhTw => format!("{duration} 後傳送"),
                    _ => format!("Send in {duration}"),
                }
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum MessageScheduleTarget {
    NewSession,
    Session(VibexSessionId),
    Queued(u64),
}

fn invalid_countdown() -> &'static str {
    locale::text(
        "Enter a countdown between 1 second and 365 days; minutes and seconds must be 0–59.",
        "倒计时须为 1 秒至 365 天，分钟和秒须为 0–59。",
        "倒計時須為 1 秒至 365 天，分鐘和秒須為 0–59。",
    )
}

fn parse_countdown(hours: &str, minutes: &str, seconds: &str) -> Result<u64, &'static str> {
    let parse = |value: &str| value.trim().parse::<u64>().map_err(|_| invalid_countdown());
    let (hours, minutes, seconds) = (parse(hours)?, parse(minutes)?, parse(seconds)?);
    if minutes >= 60 || seconds >= 60 {
        return Err(invalid_countdown());
    }
    let seconds = hours
        .checked_mul(3_600)
        .and_then(|hours| hours.checked_add(minutes * 60 + seconds))
        .filter(|seconds| (1..=MAX_COUNTDOWN_SECONDS).contains(seconds))
        .ok_or_else(invalid_countdown)?;
    Ok(seconds)
}

fn local_deadline(date_time: NaiveDateTime) -> Result<i64, &'static str> {
    // DST gaps and repeated local times must not silently choose another time.
    Local
        .from_local_datetime(&date_time)
        .single()
        .map(|time| time.timestamp_millis())
        .ok_or_else(|| {
            locale::text(
                "This local time is ambiguous or unavailable. Choose another time.",
                "此本地时间不存在或不唯一，请选择其他时间。",
                "此本機時間不存在或不唯一，請選擇其他時間。",
            )
        })
}

fn format_send_time(at_ms: i64) -> String {
    chrono::DateTime::from_timestamp_millis(at_ms)
        .map(|time| {
            time.with_timezone(&Local)
                .format("%Y-%m-%d %H:%M:%S")
                .to_string()
        })
        .unwrap_or_default()
}

fn format_remaining(remaining_ms: i64) -> String {
    let seconds = remaining_ms.max(0).saturating_add(999) / 1_000;
    format!(
        "{:02}:{:02}:{:02}",
        seconds / 3_600,
        seconds / 60 % 60,
        seconds % 60
    )
}

/// How long a scheduled row still waits, in the compact form the sidebar's
/// status column can hold.
///
/// The seconds tick: a countdown that never moves reads as decoration, not as
/// the deadline the row is actually waiting for. Past a day the seconds stop
/// mattering and the days start to, so the shape changes with the distance
/// instead of growing an unreadable hour field.
pub(super) fn format_schedule_countdown(remaining_ms: i64) -> String {
    let seconds = remaining_ms.max(0).saturating_add(999) / 1_000;
    if seconds < 24 * 60 * 60 {
        return format!(
            "{:02}:{:02}:{:02}",
            seconds / 3_600,
            seconds / 60 % 60,
            seconds % 60
        );
    }
    format!(
        "{}d {:02}:{:02}",
        seconds / (24 * 60 * 60),
        seconds / 3_600 % 24,
        seconds / 60 % 60
    )
}

/// The one queue entry the workbench may hand to the submission path next.
///
/// The pre-send queue is a single ordered list across sessions, and only its
/// head is ever a candidate: a scheduled message cannot be overtaken by the
/// messages behind it, and whichever message the reader moves in front of a
/// schedule becomes the next send. A row that is being edited holds the whole
/// queue — the reader is deciding what that message says, and nothing behind it
/// may leave before it does.
pub(super) fn next_queue_message_index(
    queue: &[ComposerQueueMessage],
    is_editing: impl Fn(u64) -> bool,
) -> Option<usize> {
    let head = queue.first()?;
    (!is_editing(head.id)).then_some(0)
}

/// The schedule editor one Composer target is currently editing.
///
/// The popover owns the surface; this owns the values, so switching modes or
/// re-rendering the surface never rebuilds the calendar or the countdown
/// fields the reader is typing into.
pub(super) struct MessageScheduleEditor {
    target: MessageScheduleTarget,
    view: Entity<MessageScheduleEditorView>,
}

impl MessageScheduleEditor {
    fn new(
        target: MessageScheduleTarget,
        initial: Option<MessageSchedule>,
        window: &mut Window,
        cx: &mut App,
    ) -> Self {
        let view = cx.new(|cx| MessageScheduleEditorView::new(initial, window, cx));
        Self { target, view }
    }

    pub(super) fn target(&self) -> &MessageScheduleTarget {
        &self.target
    }
}

struct MessageScheduleEditorView {
    countdown: bool,
    calendar: Entity<CalendarState>,
    time_field: Entity<TimeFieldState>,
    hours: Entity<InputState>,
    minutes: Entity<InputState>,
    seconds: Entity<InputState>,
    error: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

impl MessageScheduleEditorView {
    fn new(initial: Option<MessageSchedule>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        init(cx);
        let at_ms = match initial {
            Some(MessageSchedule::At(at_ms)) => at_ms,
            _ => unix_timestamp_ms() + 5 * 60 * 1_000,
        };
        let local = chrono::DateTime::from_timestamp_millis(at_ms)
            .map(|time| time.with_timezone(&Local).naive_local())
            .unwrap_or_else(|| Local::now().naive_local());
        let calendar = cx.new(|cx| {
            let mut state = CalendarState::new(window, cx);
            state.set_date(Date::Single(Some(local.date())), window, cx);
            state
        });
        let time_field = cx.new(|cx| {
            let mut state = TimeFieldState::new(window, cx)
                .precision(TimePrecision::Second)
                .hour_cycle(HourCycle::H23);
            state.set_time(local.time(), window, cx);
            state
        });
        let duration = match initial {
            Some(MessageSchedule::After(seconds)) => seconds,
            _ => 5 * 60,
        };
        let hours =
            cx.new(|cx| InputState::new(window, cx).default_value((duration / 3_600).to_string()));
        let minutes = cx
            .new(|cx| InputState::new(window, cx).default_value((duration / 60 % 60).to_string()));
        let seconds =
            cx.new(|cx| InputState::new(window, cx).default_value((duration % 60).to_string()));
        let mut subscriptions = vec![
            cx.subscribe(&calendar, |this, _, _: &CalendarEvent, cx| {
                this.error = None;
                cx.notify();
            }),
            cx.subscribe(&time_field, |this, _, _: &TimeFieldEvent, cx| {
                this.error = None;
                cx.notify();
            }),
        ];
        for input in [&hours, &minutes, &seconds] {
            subscriptions.push(cx.subscribe(input, |this, _, event, cx| {
                if matches!(event, InputEvent::Change) {
                    this.error = None;
                    cx.notify();
                }
            }));
        }
        Self {
            countdown: matches!(initial, Some(MessageSchedule::After(_))),
            calendar,
            time_field,
            hours,
            minutes,
            seconds,
            error: None,
            _subscriptions: subscriptions,
        }
    }

    /// Puts the keyboard where the edit usually starts: the countdown's
    /// minutes, or the time of an absolute deadline whose date is normally
    /// already right.
    fn focus_initial(&self, window: &mut Window, cx: &mut App) {
        if self.countdown {
            self.minutes.update(cx, |input, cx| {
                input.set_selected_range(0..input.value().len(), cx);
                input.focus(window, cx);
            });
        } else {
            window.focus(&self.time_field.read(cx).focus_handle(cx), cx);
        }
    }

    fn selected_deadline(&self, cx: &App) -> Result<i64, &'static str> {
        let date = self.calendar.read(cx).date().start().ok_or_else(|| {
            locale::text(
                "Choose a date and time.",
                "请选择日期和时间。",
                "請選擇日期和時間。",
            )
        })?;
        local_deadline(NaiveDateTime::new(date, self.time_field.read(cx).time()))
    }

    fn validate(&mut self, cx: &mut Context<Self>) -> Option<MessageSchedule> {
        let schedule = if self.countdown {
            parse_countdown(
                self.hours.read(cx).value().as_ref(),
                self.minutes.read(cx).value().as_ref(),
                self.seconds.read(cx).value().as_ref(),
            )
            .map(MessageSchedule::After)
        } else {
            self.selected_deadline(cx).map(MessageSchedule::At)
        };
        match schedule.and_then(|schedule| schedule.deadline(unix_timestamp_ms()).map(|_| schedule))
        {
            Ok(schedule) => {
                self.error = None;
                Some(schedule)
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                None
            }
        }
    }
}

impl Render for MessageScheduleEditorView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("message-schedule-editor")
            .w_full()
            .min_w_0()
            .gap_3()
            .child(
                TabBar::new("message-schedule-mode")
                    .segmented()
                    .small()
                    .selected_index(usize::from(self.countdown))
                    .child(
                        Tab::new()
                            .debug_selector(|| "schedule-date-mode".into())
                            .child(locale::text("Date and time", "定时", "定時")),
                    )
                    .child(
                        Tab::new()
                            .debug_selector(|| "schedule-countdown-mode".into())
                            .child(locale::text("Countdown", "倒计时", "倒計時")),
                    )
                    .on_click(cx.listener(|this, ix: &usize, _, cx| {
                        this.countdown = *ix == 1;
                        this.error = None;
                        cx.notify();
                    })),
            )
            .child(if self.countdown {
                h_flex()
                    .w_full()
                    .gap_3()
                    .children(
                        [
                            ("schedule-hours", &self.hours, locale::text("Hours", "时", "時")),
                            ("schedule-minutes", &self.minutes, locale::text("Minutes", "分", "分")),
                            ("schedule-seconds", &self.seconds, locale::text("Seconds", "秒", "秒")),
                        ]
                        .map(|(id, input, label)| {
                            v_flex()
                                .min_w_0()
                                .flex_1()
                                .gap_1()
                                .child(div().text_sm().child(label))
                                .child(Input::new(input).id(id).aria_label(label).small())
                        }),
                    )
                    .into_any_element()
            } else {
                // The calendar is rendered inline rather than as the date
                // picker's own dropdown: one anchored surface per decision, and
                // no second overlay layered over this popover.
                v_flex()
                    .w_full()
                    .gap_2()
                    .child(
                        Calendar::new(&self.calendar)
                            .small()
                            .w_full()
                            .border_0()
                            .rounded_none()
                            .p_0(),
                    )
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .border_t_1()
                            .border_color(cx.theme().border)
                            .pt_2()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(locale::text("Time", "时间", "時間")),
                            )
                            .child(
                                div()
                                    .key_context(TIME_CONTEXT)
                                    .child(TimeField::new(&self.time_field).small()),
                            ),
                    )
                    .into_any_element()
            })
            .when_some(self.error, |this, error| {
                this.child(
                    div()
                        .id("message-schedule-error")
                        .debug_selector(|| "message-schedule-error".into())
                        .role(Role::Alert)
                        .aria_label(error)
                        .text_sm()
                        .text_color(cx.theme().danger)
                        .child(error),
                )
            })
            .child(
                div()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(locale::text(
                        "Sends automatically when due. If the session is running, waits for it to finish. Keep Vibex open.",
                        "到时自动发送；会话运行中则等待结束。请保持 Vibex 开启。",
                        "到時自動傳送；會話執行中則等待結束。請保持 Vibex 開啟。",
                    )),
            )
    }
}

/// Applies a schedule to the target the panel belongs to. The workbench reports
/// whether it accepted the value, which is what lets the surface close.
type MessageScheduleApply = Rc<dyn Fn(Option<MessageSchedule>, &mut Window, &mut App) -> bool>;
/// Closes the popover the panel is rendered in.
type MessageScheduleDismiss = Rc<dyn Fn(&mut Window, &mut App)>;

/// Validates the editor and applies the schedule it holds. Closing is the
/// workbench's answer rather than a decision of this panel's own: a value it
/// rejected keeps the surface open with the error it produced.
fn confirm_message_schedule(
    view: &Entity<MessageScheduleEditorView>,
    apply: &MessageScheduleApply,
    dismiss: &MessageScheduleDismiss,
    window: &mut Window,
    cx: &mut App,
) {
    let Some(schedule) = view.update(cx, |view, cx| view.validate(cx)) else {
        return;
    };
    if apply(Some(schedule), window, cx) {
        dismiss(window, cx);
    }
}

/// One schedule editor plus the two decisions it offers.
///
/// Confirming validates the editor and applies the result; clearing voids the
/// schedule the panel is showing and keeps the message. Both close the surface
/// only after the workbench accepted the value. The Confirm button, Enter in a
/// countdown field and Enter in the time field all reach that one commit.
fn render_message_schedule_panel(
    view: Entity<MessageScheduleEditorView>,
    clearable: bool,
    apply: MessageScheduleApply,
    dismiss: MessageScheduleDismiss,
) -> AnyElement {
    let click_view = view.clone();
    let click_apply = apply.clone();
    let click_dismiss = dismiss.clone();
    let clear_view = view.clone();
    let clear_apply = apply;
    let clear_dismiss = dismiss.clone();
    let field_view = view.clone();
    let field_apply = click_apply.clone();
    let field_dismiss = click_dismiss.clone();
    let time_view = view.clone();
    let time_apply = click_apply.clone();
    let time_dismiss = click_dismiss.clone();
    v_flex()
        .id("message-schedule-panel")
        .w(gpui::rems(MESSAGE_SCHEDULE_PANEL_REM))
        .gap_3()
        .child(view)
        .child(
            h_flex()
                .w_full()
                .items_center()
                .gap_2()
                .when(clearable, |this| {
                    this.child(
                        Button::new("clear-message-schedule")
                            .debug_selector(|| "clear-message-schedule".into())
                            .small()
                            .ghost()
                            .label(locale::text("Clear schedule", "清除定时", "清除定時"))
                            .on_click(move |_, window, cx| {
                                // Clearing voids the timer and keeps the
                                // message: a queued one falls back to the
                                // ordinary queue, a draft keeps its text.
                                clear_view.update(cx, |view, cx| {
                                    view.error = None;
                                    cx.notify();
                                });
                                if clear_apply(None, window, cx) {
                                    clear_dismiss(window, cx);
                                }
                            }),
                    )
                })
                .child(div().flex_1())
                .child(
                    Button::new("confirm-message-schedule")
                        .debug_selector(|| "confirm-message-schedule".into())
                        .small()
                        .primary()
                        .label(locale::text("Confirm", "确认", "確認"))
                        .on_click(move |_, window, cx| {
                            confirm_message_schedule(
                                &click_view,
                                &click_apply,
                                &click_dismiss,
                                window,
                                cx,
                            );
                        }),
                ),
        )
        .on_action(move |_: &InputEnter, window, cx| {
            // Enter commits the panel, the way the dialog this replaced did.
            // A single-line `Input` propagates the `input::Enter` action it
            // answered instead of inserting a newline, so this handler sits on
            // the focused field's own path and stops the action before the
            // Composer's own Enter handling can see it.
            cx.stop_propagation();
            confirm_message_schedule(&field_view, &field_apply, &field_dismiss, window, cx);
        })
        .on_action(move |_: &PopoverConfirm, window, cx| {
            // The time field is a segmented editor rather than an `Input`, so
            // it never answers Enter with `input::Enter`. What the keystroke
            // becomes instead is the popover's own confirm action, which only
            // closes the surface and drops the deadline being edited. This
            // panel is what that gesture is confirming, so it claims the action
            // and turns it into the same commit; the countdown fields never
            // reach it because they take the handler above first.
            cx.stop_propagation();
            confirm_message_schedule(&time_view, &time_apply, &time_dismiss, window, cx);
        })
        .into_any_element()
}

impl VibexWorkbench {
    pub(super) fn composer_message_schedule(&self) -> Option<MessageSchedule> {
        self.view_session_id
            .as_ref()
            .and_then(|id| self.composer_drafts.get(id.as_str()))
            .and_then(|draft| draft.schedule)
    }

    fn schedule_for_target(&self, target: &MessageScheduleTarget) -> Option<MessageSchedule> {
        match target {
            MessageScheduleTarget::NewSession => self.new_session_schedule,
            MessageScheduleTarget::Session(id) => self
                .composer_drafts
                .get(id.as_str())
                .and_then(|draft| draft.schedule),
            MessageScheduleTarget::Queued(id) => self
                .composer_queue
                .iter()
                .find(|message| message.id == *id)
                .and_then(|message| message.scheduled_at_ms)
                .map(MessageSchedule::At),
        }
    }

    fn apply_message_schedule(
        &mut self,
        target: &MessageScheduleTarget,
        schedule: Option<MessageSchedule>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match target {
            MessageScheduleTarget::NewSession => self.new_session_schedule = schedule,
            MessageScheduleTarget::Session(id) => {
                if !self.sessions.iter().any(|session| session.id == *id) {
                    return;
                }
                if self.view_session_id.as_ref() == Some(id) {
                    self.remember_composer_draft(cx);
                }
                let mut draft = self
                    .composer_drafts
                    .get(id.as_str())
                    .cloned()
                    .unwrap_or_else(|| ComposerDraft {
                        content: InputContent::from(""),
                        attachments: Vec::new(),
                        command_entry: None,
                        attachment_serial: 0,
                        schedule: None,
                    });
                draft.schedule = schedule;
                self.composer_drafts.save(id.as_str(), draft);
            }
            MessageScheduleTarget::Queued(id) => {
                // The editor validated the value. An absolute deadline that
                // elapsed during confirmation stays due instead of being
                // cleared, and clearing hands the message back to the ordinary
                // queue instead of deleting what the reader wrote.
                let at_ms = schedule.map(|schedule| match schedule {
                    MessageSchedule::At(at_ms) => at_ms,
                    MessageSchedule::After(seconds) => {
                        unix_timestamp_ms() + (seconds * 1_000) as i64
                    }
                });
                if let Some(message) = self
                    .composer_queue
                    .iter_mut()
                    .find(|message| message.id == *id)
                {
                    message.scheduled_at_ms = at_ms;
                    self.composer_queue_ready_after_continuation_session_ids
                        .insert(message.session_id.to_string());
                }
                self.start_message_schedule_timer(window, cx);
                self.publish_sidebar_invalidation();
                // Editing the deadline changes what the row waits for, so both
                // the name its scheduled message gives it and the persisted
                // queue are rewritten together.
                self.composer_queue_changed();
            }
        }
        cx.notify();
    }

    /// Mounts the schedule editor for `target`.
    ///
    /// Called from the popover's own open transition, so the surface the reader
    /// is looking at and the values it edits are created together.
    fn open_message_schedule_editor(
        &mut self,
        target: MessageScheduleTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self
            .message_schedule_editor
            .as_ref()
            .is_some_and(|editor| editor.target() == &target)
        {
            return;
        }
        let initial = self.schedule_for_target(&target);
        let editor = MessageScheduleEditor::new(target, initial, window, cx);
        let view = editor.view.clone();
        window.on_next_frame(move |window, cx| {
            view.update(cx, |view, cx| view.focus_initial(window, cx));
        });
        self.message_schedule_editor = Some(editor);
        cx.notify();
    }

    /// Releases the editor the popover was showing. Dismissal is not a decision:
    /// the value the target already holds is left untouched.
    fn close_message_schedule_editor(
        &mut self,
        target: &MessageScheduleTarget,
        cx: &mut Context<Self>,
    ) {
        let Some(editor) = self.message_schedule_editor.take() else {
            return;
        };
        if editor.target() != target {
            self.message_schedule_editor = Some(editor);
            return;
        }
        // A queued message stops waiting out the edit, so the queue can move on
        // without another click.
        if let MessageScheduleTarget::Queued(id) = target
            && let Some(session_id) = self
                .composer_queue
                .iter()
                .find(|message| message.id == *id)
                .map(|message| message.session_id.clone())
        {
            self.mark_composer_queue_for_recheck(&session_id);
        }
        cx.notify();
    }

    /// The clock button and the popover that carries its schedule editor.
    ///
    /// The surface is the smallest one that fits the decision: it opens next to
    /// the control the reader pressed, leaves the conversation and the Composer
    /// usable, and closes on Escape or a click elsewhere without applying
    /// anything.
    pub(super) fn render_message_schedule_button(
        &self,
        target: MessageScheduleTarget,
        enabled: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let id = match &target {
            MessageScheduleTarget::NewSession => "new-session-schedule".to_string(),
            MessageScheduleTarget::Session(id) => format!("composer-schedule-{id}"),
            MessageScheduleTarget::Queued(id) => format!("schedule-composer-queue-{id}"),
        };
        let schedule = self.schedule_for_target(&target);
        let label = locale::text("Schedule message…", "定时发送…", "定時傳送…");
        let tooltip = schedule.map_or_else(
            || label.to_string(),
            |schedule| format!("{label} {}", schedule.summary()),
        );
        let trigger = button_with_aria_label(
            Button::new(id.clone())
                .small()
                .ghost()
                .compact()
                .icon(sidebar_icon(CLOCK_ICON))
                // Open is the popover's own state, and a target that carries a
                // schedule stays marked while its panel is closed.
                .selected(schedule.is_some())
                .tooltip(tooltip)
                .disabled(!enabled),
            label,
        );
        let open_target = target.clone();
        let submit_workbench = cx.weak_entity();
        let apply: MessageScheduleApply = Rc::new(move |schedule, window, cx| {
            submit_workbench
                .update(cx, |this, cx| {
                    this.apply_message_schedule(&open_target, schedule, window, cx);
                })
                .is_ok()
        });
        let clearable = schedule.is_some();
        let editor = self
            .message_schedule_editor
            .as_ref()
            .filter(|editor| editor.target() == &target)
            .map(|editor| editor.view.clone());
        let content_target = target.clone();
        let content_apply = apply.clone();
        Popover::new(id)
            .anchor(Anchor::BottomRight)
            .offset(px(8.0))
            .overlay_closable(true)
            .trigger(trigger)
            .on_open_change(cx.listener(move |this, open: &bool, window, cx| {
                if *open {
                    this.open_message_schedule_editor(content_target.clone(), window, cx);
                } else {
                    this.close_message_schedule_editor(&content_target, cx);
                }
            }))
            .content(move |_state, _window, cx| {
                let Some(view) = editor.clone() else {
                    return div().into_any_element();
                };
                let state = cx.entity();
                let dismiss: MessageScheduleDismiss = Rc::new(move |window, cx| {
                    state.update(cx, |state, cx| state.dismiss(window, cx));
                });
                render_message_schedule_panel(view, clearable, content_apply.clone(), dismiss)
            })
            .into_any_element()
    }

    pub(super) fn next_scheduled_message_at(&self, session_id: &VibexSessionId) -> Option<i64> {
        self.composer_queue
            .iter()
            .filter(|message| &message.session_id == session_id)
            .filter_map(|message| message.scheduled_at_ms)
            .min()
    }

    /// The name a session borrows from the scheduled message it is still
    /// waiting to send, when the session has no name of its own yet.
    ///
    /// A session opened for a scheduled message exists before the message does:
    /// the manager persists it under its default `<agent> session` title until a
    /// message is actually submitted, so for the whole wait every authoritative
    /// snapshot would put that placeholder on the row. The name is read from
    /// what the reader wrote instead, and stops being read the moment the send
    /// leaves the queue — which is exactly what leaves the Agent's own rename
    /// free to land once the message has really been sent.
    pub(super) fn pending_scheduled_session_title(&self, session: &AgentSession) -> Option<String> {
        scheduled_session_title(&self.composer_queue, session)
    }

    /// Names every loaded session that is only waiting for a scheduled send.
    ///
    /// The sidebar reads the name straight from the queue, so this is what keeps
    /// the rest of the shell (title bar, session menus) reading the same one.
    pub(super) fn apply_pending_scheduled_titles(&mut self) {
        let titles = self
            .sessions
            .iter()
            .filter_map(|session| {
                self.pending_scheduled_session_title(session)
                    .map(|title| (session.id.as_str().to_string(), title))
            })
            .collect::<Vec<_>>();
        let mut changed = false;
        for (session_id, title) in titles {
            let Some(session) = self
                .sessions
                .iter_mut()
                .find(|session| session.id.as_str() == session_id)
            else {
                continue;
            };
            if session.title != title {
                session.title = title;
                changed = true;
            }
        }
        if changed {
            self.invalidate_sidebar_projection_cache();
            self.publish_sidebar_invalidation();
        }
    }

    /// Writes the pre-send queue into the persisted UI state.
    ///
    /// The queue is the reader's own instruction about what to send next, so it
    /// is stored with the rest of the shell state: a restart restores it instead
    /// of dropping messages that were already committed to.
    ///
    /// The write stays throttled on purpose. A throttled write can leave an
    /// entry that already left the queue on disk, and the next launch replays
    /// it — which is harmless, because the entry carries the
    /// [`ComposerQueueEntry::dispatch_key`] its send used, so the replay is the
    /// same durable submission rather than a second turn. Forcing the write
    /// here instead would close that replay window and open a worse one: the
    /// entry would be gone from disk while the submission it describes might
    /// still fail to become durable, and the reader's message would be lost
    /// rather than deduplicated.
    pub(super) fn persist_composer_queue(&mut self) {
        self.ui_state.composer.queue = self
            .composer_queue
            .iter()
            .map(persisted_composer_queue_entry)
            .collect();
        self.queue_ui_state();
    }

    /// Everything the shell owes the reader after the queue changed: the
    /// scheduled names its rows borrow, and the persisted copy of the queue
    /// itself. One call per mutation keeps the panel, the sidebar and the file
    /// telling the same story.
    pub(super) fn composer_queue_changed(&mut self) {
        self.apply_pending_scheduled_titles();
        self.persist_composer_queue();
    }

    /// The sends this shell is holding for a later hour, as the shared view
    /// publishes them to the clients that are not the one waiting.
    pub(super) fn published_scheduled_sends(&self) -> Vec<SidebarScheduledSend> {
        self.composer_queue
            .iter()
            .filter_map(published_scheduled_send)
            .collect()
    }

    /// The whole pre-send queue, as the shared view publishes it.
    ///
    /// A compact client can now draw the queue the Desktop is actually holding
    /// instead of keeping a second one of its own, so the ordinary held messages
    /// travel too: a queue that published only its scheduled entries would show
    /// a reader two messages where the Desktop has five. What does not travel is
    /// the fields a client has no use for — the runtime selection the message
    /// will be sent with and any command it runs — because a client addresses a
    /// message by its id and never builds one.
    pub(super) fn published_queued_sends(&self) -> Vec<SidebarQueuedSend> {
        self.composer_queue
            .iter()
            .map(|message| SidebarQueuedSend {
                message_id: message.id,
                session_id: message.session_id.as_str().to_string(),
                text: message.text.clone(),
                attachments: message.attachments.clone(),
                mentions: message.mentions.clone(),
                scheduled_at_ms: message.scheduled_at_ms,
            })
            .collect()
    }

    /// Move, or take away, the hour one queued message waits for.
    ///
    /// The message keeps its place and its words: a send time says *when* it
    /// goes out, never what it says. Answers whether the queue still holds that
    /// message — a deadline that arrived for a message that has already gone
    /// out is a change that has already happened, not one to make.
    ///
    /// A bridge request arrives without a window, and both the clock that
    /// releases the message and the send itself need one, so the shell's own
    /// window is asked for. Without a window the queue still changes and is
    /// persisted; the deadline is honoured by whichever pass next has a window
    /// to run the timer in.
    pub(super) fn set_queued_send_schedule(
        &mut self,
        session_id: &str,
        message_id: u64,
        scheduled_at_ms: Option<i64>,
        window_handle: Option<AnyWindowHandle>,
        cx: &mut Context<Self>,
    ) -> bool {
        if !set_queued_message_schedule(
            &mut self.composer_queue,
            session_id,
            message_id,
            scheduled_at_ms,
        ) {
            return false;
        }
        // The dispatch rule the panel's own editor applies: a deadline moved by
        // a reader is a decision about this message, so it does not wait behind
        // a turn that is merely continuing.
        self.composer_queue_ready_after_continuation_session_ids
            .insert(session_id.to_string());
        if let Some(handle) = window_handle {
            let entity = cx.weak_entity();
            let _ = cx.update_window(handle, move |_, window, cx| {
                let _ = entity.update(cx, |this, cx| {
                    this.start_message_schedule_timer(window, cx);
                    // A deadline that is already due — a client asking for the
                    // message now, which is a deadline of "now" — is released on
                    // this pass rather than on a tick of a clock that may not
                    // have been running at all.
                    this.dispatch_due_messages(unix_timestamp_ms(), window, cx);
                });
            });
        }
        self.publish_sidebar_invalidation();
        self.composer_queue_changed();
        true
    }

    /// Rewrites one message this shell is holding.
    ///
    /// A client edits a queue it did not build, so the message keeps everything
    /// the client cannot see — its runtime selection, any command it runs — and
    /// only the parts a client actually showed it change. A deadline is left
    /// alone for the same reason: editing words is not an instruction about the
    /// hour, and a client that wants both sends the schedule change too.
    pub(super) fn set_queued_send(
        &mut self,
        session_id: &str,
        message_id: u64,
        text: &str,
        attachments: &[MessageAttachment],
        mentions: &[vibex_core::VibexUseMention],
    ) -> bool {
        let Some(message) = self
            .composer_queue
            .iter_mut()
            .find(|message| message.session_id.as_str() == session_id && message.id == message_id)
        else {
            return false;
        };
        message.text = text.to_string();
        message.attachments = attachments.to_vec();
        message.mentions = mentions.to_vec();
        // A command was resolved from the words this message no longer has, so
        // it no longer applies: running it would send something the reader did
        // not write.
        message.command_invocation = None;
        self.publish_sidebar_invalidation();
        self.composer_queue_changed();
        true
    }

    /// Takes one message a client asked this shell to hold.
    ///
    /// The client cannot build a queue entry — it does not know the session's
    /// runtime selection, and it must not choose the id — so the shell builds
    /// one here, behind everything already waiting for that session. Returns
    /// whether the message was taken: a session the shell cannot resolve a
    /// runtime for is one it cannot hold a message for.
    pub(super) fn enqueue_queued_send(
        &mut self,
        session_id: &VibexSessionId,
        text: &str,
        attachments: &[MessageAttachment],
        mentions: &[vibex_core::VibexUseMention],
    ) -> bool {
        let Some(desired_runtime) = self.runtime_selection_for_session(session_id) else {
            return false;
        };
        self.composer_queue_serial = self.composer_queue_serial.saturating_add(1).max(1);
        let insert_at = self
            .composer_queue
            .iter()
            .position(|message| message.session_id == *session_id)
            .map(|first| {
                self.composer_queue[first..]
                    .iter()
                    .position(|message| message.session_id != *session_id)
                    .map_or(self.composer_queue.len(), |offset| first + offset)
            })
            .unwrap_or(self.composer_queue.len());
        self.composer_queue.insert(
            insert_at,
            ComposerQueueMessage {
                id: self.composer_queue_serial,
                session_id: session_id.clone(),
                desired_runtime,
                text: text.to_string(),
                attachments: attachments.to_vec(),
                command_invocation: None,
                scheduled_at_ms: None,
                mentions: mentions.to_vec(),
                dispatch_key: mint_composer_queue_dispatch_key(),
            },
        );
        self.publish_sidebar_invalidation();
        self.composer_queue_changed();
        true
    }

    /// Moves one queued message inside its session's block.
    pub(super) fn move_queued_send(
        &mut self,
        session_id: &VibexSessionId,
        message_id: u64,
        before_message_id: Option<u64>,
    ) -> bool {
        let positions: Vec<usize> = self
            .composer_queue
            .iter()
            .enumerate()
            .filter(|(_, message)| message.session_id == *session_id)
            .map(|(index, _)| index)
            .collect();
        let Some(from) = positions
            .iter()
            .position(|index| self.composer_queue[*index].id == message_id)
        else {
            return false;
        };
        let target = match before_message_id {
            Some(anchor) => positions
                .iter()
                .position(|index| self.composer_queue[*index].id == anchor),
            None => Some(positions.len()),
        };
        let Some(target) = target else {
            return false;
        };
        if from == target {
            return false;
        }
        let block_start = positions[0];
        let message = self.composer_queue.remove(positions[from]);
        let remaining: Vec<usize> = self
            .composer_queue
            .iter()
            .enumerate()
            .filter(|(_, queued)| queued.session_id == *session_id)
            .map(|(index, _)| index)
            .collect();
        let insert_at = match remaining.get(target) {
            Some(index) => *index,
            // Past the end of this session's block, never past the point where
            // the next session's begins: a drop at the bottom of one session's
            // panel must not put its message behind another session's.
            None => remaining
                .last()
                .map_or(block_start.min(self.composer_queue.len()), |last| last + 1),
        };
        self.composer_queue.insert(insert_at, message);
        self.publish_sidebar_invalidation();
        self.composer_queue_changed();
        true
    }

    /// Drops one message this shell is holding.
    pub(super) fn remove_queued_send(&mut self, session_id: &str, message_id: u64) -> bool {
        let before = self.composer_queue.len();
        self.composer_queue.retain(|message| {
            !(message.session_id.as_str() == session_id && message.id == message_id)
        });
        if self.composer_queue.len() == before {
            return false;
        }
        if self.composer_queue_editing_id == Some(message_id) {
            self.composer_queue_editing_id = None;
            self.composer_queue_edit_attachments.clear();
            self.composer_queue_edit_geometry.input_bounds = None;
        }
        if self
            .composer_queue_edit_in_composer
            .is_some_and(|edit| edit.message_id == message_id)
        {
            self.composer_queue_edit_in_composer = None;
        }
        self.publish_sidebar_invalidation();
        self.composer_queue_changed();
        true
    }

    /// Holds, or releases, one session's queue.
    ///
    /// Only the switch moves here. Releasing has to re-evaluate the head, and
    /// that needs a window — so the answer says whether the caller should,
    /// rather than this reaching for one. A caller already inside an update of
    /// this entity cannot open a window without re-entering it, and a bridge
    /// request is exactly that: the mutation has to land in the state the
    /// response reports, and the release can follow.
    pub(super) fn set_queue_paused(&mut self, session_id: &VibexSessionId, paused: bool) -> bool {
        let changed = if paused {
            self.composer_queue_paused_session_ids
                .insert(session_id.as_str().to_string())
        } else {
            self.composer_queue_paused_session_ids
                .remove(session_id.as_str())
        };
        if !changed {
            return false;
        }
        self.publish_sidebar_invalidation();
        !paused
    }

    /// Switches one session's queue between automatic and manual sending.
    ///
    /// Switching back to automatic is itself the instruction to release
    /// whatever is at the head, so it answers the same way [`Self::set_queue_
    /// paused`] does: the caller that has a window does the release.
    pub(super) fn set_queue_manual(&mut self, session_id: &VibexSessionId, manual: bool) -> bool {
        let changed = if manual {
            self.composer_queue_manual_session_ids
                .insert(session_id.as_str().to_string())
        } else {
            self.composer_queue_manual_session_ids
                .remove(session_id.as_str())
        };
        if !changed {
            return false;
        }
        self.publish_sidebar_invalidation();
        !manual
    }

    /// Releases one session's queue now that a client has lifted its hold.
    ///
    /// Separate from the switch above because it needs a window and the request
    /// that set the switch did not have one to give: the caller runs this after
    /// its own update has finished.
    pub(super) fn release_queue_after_hold_lifted(
        &mut self,
        session_id: &VibexSessionId,
        behavior: ComposerQueueDispatchBehavior,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.maybe_dispatch_next_composer_queue_message(Some(session_id), behavior, window, cx);
        cx.notify();
    }

    /// Loads the queue the previous session left behind.
    ///
    /// The entries are held until the overview arrives. Missing owned sessions
    /// are resolved individually before a restored message can be dispatched.
    ///
    /// The file is not the only copy that matters. An in-process reload (a
    /// runtime retry, an RC import) re-reads it while the shell is live, so the
    /// memory queue may hold messages the file has not seen yet — queued after
    /// its last write, or never written because the write is throttled. Those
    /// survive, because the shell is the only thing that has seen the reader's
    /// latest intent.
    ///
    /// The live queue is therefore kept as it is, and only a message the file
    /// has and memory does not is folded in — at the position the file gives
    /// it, so the single ordered list the dispatcher reads still decides which
    /// head leaves. A message both sides have is taken from memory: the key it
    /// carries is the one a send may already have used, and restoring the
    /// stored copy would resurrect a key the authority no longer matches.
    pub(super) fn restore_composer_queue(&mut self, queue: Vec<ComposerQueueEntry>) {
        let stored = queue
            .into_iter()
            .filter_map(restored_composer_message)
            .collect::<Vec<_>>();
        let live = std::mem::take(&mut self.composer_queue);
        let mut merged = Vec::with_capacity(stored.len().max(live.len()));
        let mut remaining = live.into_iter().peekable();
        for entry in stored {
            if remaining
                .peek()
                .is_some_and(|message| message.dispatch_key == entry.dispatch_key)
            {
                merged.extend(remaining.next());
            } else if !remaining
                .clone()
                .any(|message| message.dispatch_key == entry.dispatch_key)
            {
                merged.push(entry);
            }
        }
        merged.extend(remaining);
        self.composer_queue = merged;
        self.composer_queue_serial = self
            .composer_queue
            .iter()
            .map(|message| message.id)
            .max()
            .unwrap_or(0);
        self.composer_queue_restore_pending = !self.composer_queue.is_empty();
    }

    /// Resolves one queued session absent from the navigation roots.
    pub(super) fn load_queued_session(
        &mut self,
        session_id: VibexSessionId,
        behavior: ComposerQueueDispatchBehavior,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let Some(backend) = self.backend.clone() else {
            return;
        };
        if !self
            .composer_queue_session_loads
            .insert(session_id.to_string())
        {
            return;
        }
        let authority = self.ui_state.sidebar.active_authority().to_string();
        let generation = self.delegated_tree_generation;
        let requested = session_id.clone();
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { backend.agent().open_session(requested).await },
            );
        cx.spawn_in(window, async move |entity, cx| {
            let outcome = runner.await;
            let _ = entity.update_in(cx, |this, window, cx| {
                if this.delegated_tree_generation != generation
                    || this.ui_state.sidebar.active_authority() != authority
                {
                    return;
                }
                this.composer_queue_session_loads
                    .remove(session_id.as_str());
                if this.session_hidden_from_sidebar(session_id.as_str()) {
                    return;
                }
                match outcome {
                    Ok(Ok(session)) if session.id != session_id => {
                        this.agent_error =
                            Some("The runtime returned a different conversation.".into());
                    }
                    Ok(Ok(session))
                        if session.deleted_at_ms.is_none() && session.archived_at_ms.is_none() =>
                    {
                        this.delegated_sessions
                            .insert(session.id.to_string(), session.clone());
                        this.upsert_session_snapshot(session);
                        this.mark_composer_queue_for_recheck(&session_id);
                        // The queue is one ordered list, so the session that was
                        // just loaded only advances the queue when its message
                        // is the head; the behavior of a message behind another
                        // entry is the head's own.
                        this.maybe_dispatch_next_composer_queue_message(
                            Some(&session_id),
                            behavior,
                            window,
                            cx,
                        );
                    }
                    Ok(Ok(_)) => {
                        this.composer_queue
                            .retain(|message| message.session_id != session_id);
                        this.composer_queue_changed();
                    }
                    Ok(Err(error)) if error.code == "session_not_found" => {
                        this.composer_queue
                            .retain(|message| message.session_id != session_id);
                        this.composer_queue_changed();
                    }
                    Ok(Err(error)) => this.agent_error = Some(error.message),
                    Err(error) => this.agent_error = Some(error.to_string()),
                }
                cx.notify();
            });
        })
        .detach();
    }

    /// Adopts the restored queue after root metadata is available.
    ///
    /// A restored message whose session is gone is dropped, a due or ready one
    /// is re-evaluated on the next frame, and the schedule timer is re-armed so
    /// a deadline that passed while Vibex was closed is still honored.
    ///
    /// Before anything is dispatched, each entry is checked against the
    /// authoritative submission record its key names. An entry can outlive its
    /// own send — the removal is persisted through a throttled write — and a
    /// send that already reached the provider must not be replayed. The check
    /// is what makes the guarantee hold for an entry that has no key too, the
    /// one a queue written by an older build leaves behind.
    fn adopt_restored_composer_queue(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.composer_queue_restore_pending = false;
        self.prune_composer_queue_against_submissions(cx);
        let session_ids = self
            .composer_queue
            .iter()
            .map(|message| message.session_id.clone())
            .collect::<BTreeSet<_>>();
        for session_id in session_ids {
            self.mark_composer_queue_for_recheck(&session_id);
        }
        // A message the reader lined up may have become sendable while Vibex
        // was closed. The queue is one ordered list, so only its head is
        // re-evaluated, and the ordinary rules still decide: a deadline that
        // has not arrived waits, a message behind a schedule waits for it, and
        // Manual send mode still needs the reader's own send before anything
        // leaves the queue.
        self.maybe_dispatch_next_composer_queue_message(
            None,
            ComposerQueueDispatchBehavior::Automatic,
            window,
            cx,
        );
        self.start_message_schedule_timer(window, cx);
        self.apply_pending_scheduled_titles();
        self.persist_composer_queue();
        cx.notify();
    }

    /// Drops restored entries the authority has already accepted.
    ///
    /// The submission table is the only place that knows whether a message that
    /// left the queue was really sent, delivered but unresolved, or never left
    /// at all. Every state except "the provider may have it" is replayable: a
    /// failed or cancelled submission never reached a provider, and an
    /// `AwaitingRuntime`/`ReadyToDispatch` one is still waiting for its own
    /// worker, which `reconcile_on_startup` resumes.
    ///
    /// A record that is terminal *and* reached the provider is the one case the
    /// queue must not touch. It is dropped and its optimistic row is discarded,
    /// because the authoritative timeline already holds the turn.
    pub(super) fn prune_composer_queue_against_submissions(&mut self, cx: &mut Context<Self>) {
        let Some(runtime) = self.runtime.clone() else {
            // A remote authority answers through the backend, and its record is
            // the authority's own; the durable key on the entry is what makes a
            // replay there the same submission, so nothing is pruned blindly.
            return;
        };
        let coordinator = runtime.agent().manager().message_submission_coordinator();
        let Some(coordinator) = coordinator else {
            return;
        };
        let sessions = self
            .composer_queue
            .iter()
            .map(|message| (message.session_id.clone(), message.dispatch_key.clone()))
            .collect::<Vec<_>>();
        for (session_id, dispatch_key) in sessions {
            let request = GetMessageSubmissionRequest {
                session_id: session_id.clone(),
                message_idempotency_key: dispatch_key.clone(),
            };
            let Ok(state) = coordinator.get_submission(&request) else {
                // No record under this key: the entry never became a durable
                // submission, so it is an ordinary unsent message.
                continue;
            };
            let delivered = matches!(
                state.status,
                MessageSubmissionStatus::Completed
                    | MessageSubmissionStatus::Dispatched
                    | MessageSubmissionStatus::AboutToPrompt
                    | MessageSubmissionStatus::AmbiguousPromptDispatch
            ) || composer_submission_failure_reached_provider(&state);
            if !delivered {
                continue;
            }
            self.composer_queue
                .retain(|message| message.dispatch_key != dispatch_key);
            // The turn is already in the transcript, so the pending row this
            // launch never installed has nothing left to wait for.
            self.discard_optimistic_user_message(&session_id);
            eprintln!(
                "vibex-foundation: dropped a restored queued message the authority already accepted ({:?})",
                state.status
            );
        }
        cx.notify();
    }

    /// Adopts the restored queue before the frame that would render it.
    pub(super) fn adopt_restored_composer_queue_if_ready(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.composer_queue_restore_pending && self.sessions_loaded {
            self.adopt_restored_composer_queue(window, cx);
        }
    }

    pub(super) fn render_message_schedule_draft_hint(
        &self,
        schedule: MessageSchedule,
        cx: &App,
    ) -> AnyElement {
        let label = match schedule {
            MessageSchedule::At(_) => locale::text("Send at", "发送时间", "傳送時間"),
            MessageSchedule::After(_) => locale::text(
                "Countdown starts when submitted",
                "发送后开始倒计时",
                "傳送後開始倒計時",
            ),
        };
        let value = match schedule {
            MessageSchedule::At(at_ms) => format_send_time(at_ms),
            MessageSchedule::After(seconds) => format_remaining((seconds * 1_000) as i64),
        };
        div()
            .min_w_0()
            .px_4()
            .pb_2()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(format!("{label} · {value}"))
            .into_any_element()
    }

    pub(super) fn queue_message_is_being_edited(&self, id: u64) -> bool {
        self.composer_queue_editing_id == Some(id)
            || self
                .session_views
                .values()
                .any(|view| view.composer_queue_editing_id == Some(id))
            || self
                .composer_queue_edit_in_composer
                .is_some_and(|edit| edit.message_id == id)
            || self.session_views.values().any(|view| {
                view.composer_queue_edit_in_composer
                    .as_ref()
                    .is_some_and(|edit| edit.message_id == id)
            })
            || self
                .message_schedule_editor
                .as_ref()
                .is_some_and(|editor| editor.target() == &MessageScheduleTarget::Queued(id))
    }

    pub(super) fn start_message_schedule_timer(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.message_schedule_task.is_some()
            || !self
                .composer_queue
                .iter()
                .any(|message| message.scheduled_at_ms.is_some())
        {
            return;
        }
        self.message_schedule_task = Some(cx.spawn_in(
            window,
            async move |entity: WeakEntity<Self>, cx| {
                loop {
                    cx.background_executor().timer(Duration::from_secs(1)).await;
                    // The handle is released in the same update that finds
                    // nothing left to wait for, so the field never points at a
                    // task that has already decided to stop: a schedule armed
                    // later either joins a running timer or starts one.
                    let keep_running = match entity.update_in(cx, |this, window, cx| {
                        this.dispatch_due_messages(unix_timestamp_ms(), window, cx);
                        let pending = this
                            .composer_queue
                            .iter()
                            .any(|message| message.scheduled_at_ms.is_some());
                        if pending {
                            cx.notify();
                        } else {
                            this.message_schedule_task = None;
                        }
                        pending
                    }) {
                        Ok(keep_running) => keep_running,
                        Err(_) => {
                            // The window the clock was armed in can close while
                            // the workbench lives on in the tray: there is no
                            // window left to dispatch in. Releasing the handle
                            // here is what lets the window that comes back arm
                            // the clock again instead of finding a task that
                            // already stopped and a message that never sends.
                            let _ = entity.update(cx, |this, _| {
                                this.message_schedule_task = None;
                            });
                            false
                        }
                    };
                    if !keep_running {
                        break;
                    }
                }
            },
        ));
    }

    pub(super) fn dispatch_due_messages(
        &mut self,
        now_ms: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        // A deadline only matters once its message reaches the head of the
        // queue, but the head is re-evaluated on every tick rather than only
        // for the sessions that hold a due message: the queue is one list, and
        // whatever stands in front of a due schedule decides when it may fire.
        let due = self
            .composer_queue
            .iter()
            .any(|message| message.scheduled_at_ms.is_some_and(|at_ms| at_ms <= now_ms));
        if !due {
            return;
        }
        self.maybe_dispatch_next_composer_queue_message(
            None,
            ComposerQueueDispatchBehavior::Automatic,
            window,
            cx,
        );
    }

    pub(super) fn render_scheduled_queue_status(
        &self,
        at_ms: i64,
        session_id: &VibexSessionId,
        cx: &App,
    ) -> AnyElement {
        let remaining = at_ms.saturating_sub(unix_timestamp_ms());
        let label = if remaining > 0 {
            format!(
                "{} · {}",
                format_send_time(at_ms),
                format_remaining(remaining)
            )
        } else if self
            .composer_queue_paused_session_ids
            .contains(session_id.as_str())
        {
            locale::text(
                "Due · queue paused",
                "已到时 · 排队已暂停",
                "已到時 · 排隊已暫停",
            )
            .to_string()
        } else if self.agent_session_is_active(session_id) {
            locale::text(
                "Due · waiting for the current turn",
                "已到时 · 等待当前运行结束",
                "已到時 · 等待目前執行結束",
            )
            .to_string()
        } else {
            locale::text(
                "Due · waiting to send",
                "已到时 · 等待发送",
                "已到時 · 等待傳送",
            )
            .to_string()
        };
        div()
            .text_xs()
            .text_color(cx.theme().muted_foreground)
            .child(label)
            .into_any_element()
    }

    pub(super) fn render_scheduled_empty_timeline(&self, at_ms: i64, cx: &App) -> AnyElement {
        v_flex()
            .id("scheduled-message-empty-timeline")
            .size_full()
            .min_w_0()
            .items_center()
            .justify_center()
            .gap_3()
            .p_6()
            .text_color(cx.theme().muted_foreground)
            .child(sidebar_icon(CLOCK_ICON).large())
            .child(
                div()
                    .text_sm()
                    .child(locale::text("Scheduled message", "定时消息", "定時訊息")),
            )
            .child(
                div()
                    .text_2xl()
                    .child(format_remaining(at_ms.saturating_sub(unix_timestamp_ms()))),
            )
            .child(div().text_xs().child(format_send_time(at_ms)))
            .into_any_element()
    }

    pub(super) fn render_scheduled_session_indicator(
        &self,
        session_id: &str,
        at_ms: i64,
        cx: &App,
    ) -> AnyElement {
        // The row itself carries the countdown, so the tooltip is where the
        // absolute deadline stays readable: "when" and "how long" are the two
        // questions a scheduled row is asked.
        let label = format!(
            "{} {} · {}",
            locale::text("Scheduled send", "定时发送", "定時傳送"),
            format_send_time(at_ms),
            format_schedule_countdown(at_ms.saturating_sub(unix_timestamp_ms()))
        );
        div()
            .id(format!("sidebar-session-scheduled-{session_id}"))
            .role(Role::Status)
            .aria_label(label.clone())
            .tooltip(move |window, cx| Tooltip::new(label.clone()).build(window, cx))
            .child(
                sidebar_icon(CLOCK_ICON)
                    .small()
                    .text_color(cx.theme().muted_foreground),
            )
            .into_any_element()
    }
}

/// The name a session borrows from the scheduled message it is still waiting to
/// send, when the session has no name of its own yet.
///
/// The earliest scheduled message names the session: it is the one that will
/// actually open the conversation.
fn scheduled_session_title(
    queue: &[ComposerQueueMessage],
    session: &AgentSession,
) -> Option<String> {
    let message = queue
        .iter()
        .filter(|message| message.session_id == session.id)
        .filter(|message| message.scheduled_at_ms.is_some())
        .min_by_key(|message| message.scheduled_at_ms)?;
    let title = session_title_from_first_message(&message.text, &message.attachments)?;
    // A session that already carries a name — a manual rename, an Agent title,
    // or an earlier message — keeps it.
    (session.title == agent_session_fallback_title(&session.agent_id) || session.title == title)
        .then_some(title)
}

/// The published shape of one queued message, when it is waiting on an hour.
///
/// An ordinary held message has no hour to publish: it waits for this window's
/// own turn, and a client that has no part in that turn has nothing to draw.
fn published_scheduled_send(message: &ComposerQueueMessage) -> Option<SidebarScheduledSend> {
    Some(SidebarScheduledSend {
        session_id: message.session_id.as_str().to_string(),
        message_id: message.id,
        text: message.text.clone(),
        attachments: message.attachments.clone(),
        scheduled_at_ms: message.scheduled_at_ms?,
    })
}

/// Move, or take away, the hour one queued message waits for.
///
/// Answers whether the queue still holds that message: a deadline that arrived
/// for a message that has already gone out is a change that has already
/// happened, not one to make. The message keeps its place either way — a
/// schedule says when it goes, never whether it does.
fn set_queued_message_schedule(
    queue: &mut [ComposerQueueMessage],
    session_id: &str,
    message_id: u64,
    scheduled_at_ms: Option<i64>,
) -> bool {
    let Some(message) = queue
        .iter_mut()
        .find(|message| message.id == message_id && message.session_id.as_str() == session_id)
    else {
        return false;
    };
    message.scheduled_at_ms = scheduled_at_ms;
    true
}

/// The persisted shape of one queued message.
fn persisted_composer_queue_entry(message: &ComposerQueueMessage) -> ComposerQueueEntry {
    ComposerQueueEntry {
        id: message.id,
        session_id: message.session_id.as_str().to_string(),
        desired_runtime: message.desired_runtime.clone(),
        text: message.text.clone(),
        attachments: message.attachments.clone(),
        mentions: message.mentions.clone(),
        command: message
            .command_invocation
            .as_ref()
            .map(|invocation| ComposerQueueCommand {
                command_id: invocation.command_id.clone(),
                trigger: invocation.trigger,
                source_kind: invocation.source_kind,
                command_text: invocation.command_text.clone(),
                command_name: invocation.command_name.clone(),
                arguments: invocation.arguments.clone(),
                prompt_id: invocation.prompt_id.clone(),
            }),
        scheduled_at_ms: message.scheduled_at_ms,
        dispatch_key: Some(message.dispatch_key.clone()),
    }
}

/// Rebuilds a queued message from its persisted shape. A session id the
/// authority would not accept cannot address a send, so it is dropped rather
/// than queued against a session that cannot exist.
///
/// A queue written before the key was persisted carries none, and guessing one
/// would be worse than minting a fresh key: the entry may or may not have been
/// sent already, and only the key a send actually used can prove it. Such an
/// entry is restored with a new key and is replayed once, which is exactly what
/// an older build did — the difference is that everything queued from now on
/// carries the key that makes the replay harmless.
fn restored_composer_message(entry: ComposerQueueEntry) -> Option<ComposerQueueMessage> {
    let session_id = VibexSessionId::parse(&entry.session_id).ok()?;
    Some(ComposerQueueMessage {
        id: entry.id,
        session_id,
        desired_runtime: entry.desired_runtime,
        text: entry.text,
        attachments: entry.attachments,
        command_invocation: entry.command.map(|command| ComposerCommandInvocation {
            command_id: command.command_id,
            trigger: command.trigger,
            source_kind: command.source_kind,
            command_text: command.command_text,
            command_name: command.command_name,
            arguments: command.arguments,
            prompt_id: command.prompt_id,
        }),
        scheduled_at_ms: entry.scheduled_at_ms,
        mentions: entry.mentions,
        dispatch_key: entry
            .dispatch_key
            .unwrap_or_else(mint_composer_queue_dispatch_key),
    })
}

#[cfg(test)]
#[path = "message_schedule_tests.rs"]
mod tests;

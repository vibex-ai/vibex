//! Scheduling for unsent composer drafts. The existing queue owns the message
//! until its deadline; only the ordinary submission path creates a timeline turn.

use super::*;
use chrono::{Local, NaiveDateTime, TimeZone as _};
use gpui_component::{
    date_picker::{DatePicker, DatePickerEvent, DatePickerState},
    dialog::DialogButtonProps,
    time_field::{HourCycle, TimePrecision},
};

const MAX_COUNTDOWN_SECONDS: u64 = 365 * 24 * 60 * 60;
const CLOCK_ICON: &str = "icons/vibex/clock.svg";

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

pub(super) fn next_message_index(
    queue: &[ComposerQueueMessage],
    session_id: &VibexSessionId,
    behavior: ComposerQueueDispatchBehavior,
    ordinary_dispatch_enabled: bool,
    now_ms: i64,
    is_editing: impl Fn(u64) -> bool,
) -> Option<usize> {
    queue.iter().position(|message| {
        &message.session_id == session_id
            && !is_editing(message.id)
            && match message.scheduled_at_ms {
                Some(at_ms) => at_ms <= now_ms,
                None => {
                    behavior != ComposerQueueDispatchBehavior::Scheduled
                        && ordinary_dispatch_enabled
                }
            }
    })
}

struct MessageSchedulePicker {
    countdown: bool,
    date_time: Entity<DatePickerState>,
    hours: Entity<InputState>,
    minutes: Entity<InputState>,
    seconds: Entity<InputState>,
    error: Option<&'static str>,
    _subscriptions: Vec<Subscription>,
}

impl MessageSchedulePicker {
    fn open(
        initial: Option<MessageSchedule>,
        clearable: bool,
        on_apply: impl Fn(Option<MessageSchedule>, &mut Window, &mut App) -> bool + 'static,
        on_close: impl Fn(&mut App) + 'static,
        window: &mut Window,
        cx: &mut App,
    ) -> Entity<Self> {
        let picker = cx.new(|cx| Self::new(initial, window, cx));
        let dialog_picker = picker.clone();
        let on_apply = Rc::new(on_apply);
        let on_close = Rc::new(on_close);
        window.open_dialog(cx, move |dialog, _, _| {
            let submit_picker = dialog_picker.clone();
            let submit = on_apply.clone();
            let clear = on_apply.clone();
            let clear_close = on_close.clone();
            let close = on_close.clone();
            dialog
                .title(locale::text("Schedule message", "定时发送", "定時傳送"))
                .child(dialog_picker.clone())
                .when(initial.is_some() && clearable, |dialog| {
                    dialog.child(
                        Button::new("clear-message-schedule")
                            .debug_selector(|| "clear-message-schedule".into())
                            .ghost()
                            .small()
                            .label(locale::text("Remove schedule", "取消定时", "取消定時"))
                            .on_click(move |_, window, cx| {
                                if clear(None, window, cx) {
                                    window.close_dialog(cx);
                                    clear_close(cx);
                                }
                            }),
                    )
                })
                .button_props(
                    DialogButtonProps::default()
                        .ok_text(locale::text("Set schedule", "设置定时", "設定定時"))
                        .cancel_text(locale::text("Cancel", "取消", "取消"))
                        .show_cancel(true),
                )
                .on_ok(move |_, window, cx| {
                    let Some(schedule) = submit_picker.update(cx, |picker, cx| picker.validate(cx))
                    else {
                        return false;
                    };
                    submit(Some(schedule), window, cx)
                })
                .on_close(move |_, _, cx| close(cx))
        });
        let focus_picker = picker.clone();
        window.on_next_frame(move |window, cx| {
            focus_picker.update(cx, |picker, cx| {
                if picker.countdown {
                    picker.minutes.update(cx, |input, cx| {
                        input.set_selected_range(0..input.value().len(), cx);
                        input.focus(window, cx);
                    });
                } else {
                    window.focus(&picker.date_time.focus_handle(cx), cx);
                }
            });
        });
        picker
    }

    fn new(initial: Option<MessageSchedule>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let at_ms = match initial {
            Some(MessageSchedule::At(at_ms)) => at_ms,
            _ => unix_timestamp_ms() + 5 * 60 * 1_000,
        };
        let date_time = cx.new(|cx| {
            let mut state = DatePickerState::new(window, cx)
                .date_format("%Y-%m-%d")
                .time_precision(TimePrecision::Second)
                .hour_cycle(HourCycle::H23);
            if let Some(time) = chrono::DateTime::from_timestamp_millis(at_ms) {
                state.set_date_time(time.with_timezone(&Local).naive_local(), window, cx);
            }
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
        let mut subscriptions =
            vec![
                cx.subscribe(&date_time, |this, _, _: &DatePickerEvent, cx| {
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
            date_time,
            hours,
            minutes,
            seconds,
            error: None,
            _subscriptions: subscriptions,
        }
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
            self.date_time
                .read(cx)
                .date_time()
                .start()
                .ok_or_else(|| {
                    locale::text(
                        "Choose a date and time.",
                        "请选择日期和时间。",
                        "請選擇日期和時間。",
                    )
                })
                .and_then(local_deadline)
                .map(MessageSchedule::At)
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

impl Render for MessageSchedulePicker {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .id("message-schedule-picker")
            .w_full()
            .min_w_0()
            .gap_4()
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
                v_flex()
                    .w_full()
                    .gap_1()
                    .child(div().text_sm().child(locale::text(
                        "Local date and time", "本地日期和时间", "本機日期和時間",
                    )))
                    .child(
                        DatePicker::new(&self.date_time)
                            .cleanable(false)
                            .small()
                            .w_full(),
                    )
                    .into_any_element()
            })
            .when_some(self.error, |this, error| {
                this.child(
                    div()
                        .id("message-schedule-error")
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
                // The picker validated the value. An absolute deadline that
                // elapsed during confirmation stays due instead of being cleared.
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
            }
        }
        cx.notify();
    }

    pub(super) fn open_message_schedule(
        &mut self,
        target: MessageScheduleTarget,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if window.has_active_dialog(cx) {
            return;
        }
        let initial = self.schedule_for_target(&target);
        self.message_schedule_dialog_target = Some(target.clone());
        let submit_workbench = cx.weak_entity();
        let close_workbench = cx.weak_entity();
        MessageSchedulePicker::open(
            initial,
            !matches!(&target, MessageScheduleTarget::Queued(_)),
            move |schedule, window, cx| {
                submit_workbench
                    .update(cx, |this, cx| {
                        this.apply_message_schedule(&target, schedule, window, cx);
                    })
                    .is_ok()
            },
            move |cx| {
                let _ = close_workbench.update(cx, |this, cx| {
                    if let Some(MessageScheduleTarget::Queued(id)) =
                        this.message_schedule_dialog_target.take()
                        && let Some(session_id) = this
                            .composer_queue
                            .iter()
                            .find(|message| message.id == id)
                            .map(|message| message.session_id.clone())
                    {
                        this.mark_composer_queue_for_recheck(&session_id);
                    }
                    cx.notify();
                });
            },
            window,
            cx,
        );
        cx.notify();
    }

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
        button_with_aria_label(
            Button::new(id)
                .small()
                .ghost()
                .compact()
                .icon(sidebar_icon(CLOCK_ICON))
                .selected(
                    schedule.is_some()
                        || self.message_schedule_dialog_target.as_ref() == Some(&target),
                )
                .tooltip(tooltip)
                .disabled(!enabled)
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_message_schedule(target.clone(), window, cx)
                })),
            label,
        )
        .into_any_element()
    }

    pub(super) fn next_scheduled_message_at(&self, session_id: &VibexSessionId) -> Option<i64> {
        self.composer_queue
            .iter()
            .filter(|message| &message.session_id == session_id)
            .filter_map(|message| message.scheduled_at_ms)
            .min()
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
            || self.message_schedule_dialog_target == Some(MessageScheduleTarget::Queued(id))
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
                    let keep_running = entity
                        .update_in(cx, |this, window, cx| {
                            this.dispatch_due_messages(unix_timestamp_ms(), window, cx);
                            let pending = this
                                .composer_queue
                                .iter()
                                .any(|message| message.scheduled_at_ms.is_some());
                            if pending {
                                cx.notify();
                            }
                            pending
                        })
                        .unwrap_or(false);
                    if !keep_running {
                        break;
                    }
                }
                let _ = entity.update_in(cx, |this, _, _| this.message_schedule_task = None);
            },
        ));
    }

    fn dispatch_due_messages(&mut self, now_ms: i64, window: &mut Window, cx: &mut Context<Self>) {
        let session_ids = self
            .composer_queue
            .iter()
            .filter(|message| message.scheduled_at_ms.is_some_and(|at_ms| at_ms <= now_ms))
            .map(|message| message.session_id.clone())
            .collect::<BTreeSet<_>>();
        for session_id in session_ids {
            self.maybe_dispatch_next_composer_queue_message(
                &session_id,
                ComposerQueueDispatchBehavior::Scheduled,
                window,
                cx,
            );
        }
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
        session_id: &VibexSessionId,
        at_ms: i64,
        cx: &App,
    ) -> AnyElement {
        let label = format!(
            "{} {}",
            locale::text("Scheduled send", "定时发送", "定時傳送"),
            format_send_time(at_ms)
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

#[cfg(test)]
#[path = "message_schedule_tests.rs"]
mod tests;

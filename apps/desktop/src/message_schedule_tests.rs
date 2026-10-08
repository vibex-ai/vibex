use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use vibex_core::ProviderProfileId;

fn queued(id: u64, session: &VibexSessionId, at_ms: Option<i64>) -> ComposerQueueMessage {
    ComposerQueueMessage {
        id,
        session_id: session.clone(),
        desired_runtime: SessionRuntimeSelection::provider(
            AgentId::parse("agent-schedule-test").unwrap(),
            ProviderProfileId::parse("provider_schedule_test").unwrap(),
            "test-model",
        ),
        text: format!("scheduled message {id}"),
        attachments: Vec::new(),
        command_invocation: None,
        scheduled_at_ms: at_ms,
    }
}

#[test]
fn countdown_validates_units_and_overflow() {
    assert_eq!(parse_countdown("1", "02", "03"), Ok(3_723));
    assert_eq!(parse_countdown(" 0 ", "0", "1"), Ok(1));
    assert_eq!(parse_countdown("8760", "0", "0"), Ok(MAX_COUNTDOWN_SECONDS));
    for (hours, minutes, seconds) in [
        ("0", "0", "0"),
        ("-1", "0", "0"),
        ("1.5", "0", "0"),
        ("", "0", "1"),
        ("0", "60", "0"),
        ("0", "0", "60"),
        ("8760", "0", "1"),
        ("18446744073709551615", "0", "0"),
    ] {
        assert!(
            parse_countdown(hours, minutes, seconds).is_err(),
            "{hours}:{minutes}:{seconds}"
        );
    }
}

#[test]
fn countdown_starts_on_submission_and_absolute_time_does_not_move() {
    let countdown = MessageSchedule::After(90);
    assert_eq!(countdown.deadline(1_000), Ok(91_000));
    assert_eq!(countdown.deadline(31_000), Ok(121_000));
    let absolute = MessageSchedule::At(91_000);
    assert_eq!(absolute.deadline(1_000), Ok(91_000));
    assert_eq!(absolute.deadline(31_000), Ok(91_000));
    assert!(absolute.deadline(91_000).is_err());
    assert!(absolute.deadline(92_000).is_err());
    assert!(countdown.deadline(i64::MAX).is_err());
    assert_eq!(format_remaining(1), "00:00:01");
    assert_eq!(format_remaining(3_661_001), "01:01:02");
    assert_eq!(format_remaining(-1), "00:00:00");
}

#[test]
fn deadlines_do_not_block_ready_messages_or_leak_between_sessions() {
    let session = VibexSessionId::new();
    let other = VibexSessionId::new();
    let mut queue = vec![
        queued(1, &session, Some(30_000)),
        queued(2, &other, Some(1_000)),
        queued(3, &session, None),
        queued(4, &session, Some(10_000)),
    ];
    let candidate = |queue: &[ComposerQueueMessage], behavior, ordinary, now| {
        next_message_index(queue, &session, behavior, ordinary, now, |_| false)
    };
    assert_eq!(
        candidate(
            &queue,
            ComposerQueueDispatchBehavior::Automatic,
            true,
            5_000
        ),
        Some(2)
    );
    assert_eq!(
        candidate(
            &queue,
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            9_999
        ),
        None
    );
    // A timer sends its own due message even with ordinary Auto send disabled.
    assert_eq!(
        candidate(
            &queue,
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            10_000
        ),
        Some(3)
    );
    let sent = queue.remove(3);
    assert_eq!(sent.id, 4);
    assert_eq!(
        candidate(
            &queue,
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            10_001
        ),
        None
    );
    assert_eq!(
        candidate(
            &queue,
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            30_000
        ),
        Some(0)
    );
    // Resuming a paused queue cannot send a future message early.
    assert_eq!(
        candidate(
            &queue[..1],
            ComposerQueueDispatchBehavior::ForceNext,
            true,
            29_999
        ),
        None
    );
}

#[test]
fn due_messages_wait_for_editing_and_for_a_busy_or_finished_session() {
    let session = VibexSessionId::new();
    let queue = vec![queued(1, &session, Some(1_000))];
    assert_eq!(
        next_message_index(
            &queue,
            &session,
            ComposerQueueDispatchBehavior::Scheduled,
            true,
            1_000,
            |_| true
        ),
        None
    );
    assert_eq!(
        next_message_index(
            &queue,
            &session,
            ComposerQueueDispatchBehavior::Scheduled,
            true,
            1_000,
            |_| false
        ),
        Some(0)
    );
    // A deferred new session reports `Initializing` — and a session the desktop
    // has not loaded yet reports nothing — until its first message is
    // submitted. Neither is work in progress, so neither may strand the
    // schedule that would start it.
    for state in [
        None,
        Some(AgentSessionState::Initializing),
        Some(AgentSessionState::Idle),
        Some(AgentSessionState::Error),
    ] {
        assert!(
            !composer_queue_session_blocks_dispatch(
                ComposerQueueDispatchBehavior::Scheduled,
                false,
                state
            ),
            "{state:?}"
        );
    }
    for state in [
        Some(AgentSessionState::Running),
        Some(AgentSessionState::NeedsInput),
        Some(AgentSessionState::Closed),
        Some(AgentSessionState::Archived),
    ] {
        assert!(
            composer_queue_session_blocks_dispatch(
                ComposerQueueDispatchBehavior::Scheduled,
                false,
                state
            ),
            "{state:?}"
        );
    }
    for state in [AgentSessionState::Idle, AgentSessionState::Error] {
        assert!(composer_queue_session_blocks_dispatch(
            ComposerQueueDispatchBehavior::Scheduled,
            true,
            Some(state)
        ));
    }
}

#[test]
fn moving_a_scheduled_message_keeps_its_deadline_but_send_now_clears_it() {
    let session = VibexSessionId::new();
    let mut queue = vec![
        queued(1, &session, Some(1_000)),
        queued(2, &session, Some(2_000)),
    ];
    assert!(reorder_composer_queue_message(&mut queue, 2, 1, false));
    assert_eq!(queue[0].scheduled_at_ms, Some(2_000));
    assert_eq!(promote_composer_queue_message(&mut queue, 1), Some(session));
    assert_eq!(queue[0].id, 1);
    assert_eq!(queue[0].scheduled_at_ms, None);
    assert_eq!(queue[1].scheduled_at_ms, Some(2_000));
}

#[test]
fn empty_scheduled_drafts_survive_switches_and_are_consumed_with_the_message() {
    let mut drafts = ComposerDraftStore::default();
    let draft = |schedule| ComposerDraft {
        content: InputContent::from(""),
        attachments: Vec::new(),
        command_entry: None,
        attachment_serial: 0,
        schedule,
    };
    drafts.save("first", draft(Some(MessageSchedule::After(60))));
    drafts.save("second", draft(Some(MessageSchedule::At(90_000))));
    assert_eq!(
        drafts.get("first").unwrap().schedule,
        Some(MessageSchedule::After(60))
    );
    assert_eq!(
        drafts.get("second").unwrap().schedule,
        Some(MessageSchedule::At(90_000))
    );
    drafts.consume("first");
    assert!(drafts.get("first").is_none());
    assert!(drafts.get("second").is_some());
    drafts.save("second", draft(None));
    assert!(drafts.get("second").is_none());
}

/// Stands in for the Composer's clock button: one trigger, one popover, one
/// editor created with it — the wiring `render_message_schedule_button`
/// installs around the shared panel.
struct SchedulePopoverHost {
    results: Rc<RefCell<Vec<Option<MessageSchedule>>>>,
    closed: Rc<Cell<usize>>,
    /// Whether the target already carries a schedule, which is what the clear
    /// action needs to be about.
    clearable: Rc<Cell<bool>>,
    editor: Option<MessageScheduleEditor>,
}

impl Render for SchedulePopoverHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let editor = self.editor.as_ref().map(|editor| editor.view.clone());
        let clearable = self.clearable.get();
        let applied = self.results.clone();
        let apply: MessageScheduleApply = Rc::new(move |schedule, _, _| {
            applied.borrow_mut().push(schedule);
            true
        });
        Popover::new("schedule-popover")
            .trigger(
                Button::new("open-schedule")
                    .debug_selector(|| "open-schedule".into())
                    .label("Schedule…"),
            )
            .on_open_change(cx.listener(|this, open: &bool, window, cx| {
                if *open {
                    this.editor = Some(MessageScheduleEditor::new(
                        MessageScheduleTarget::NewSession,
                        Some(MessageSchedule::After(60)),
                        window,
                        cx,
                    ));
                    let view = this.editor.as_ref().unwrap().view.clone();
                    window.on_next_frame(move |window, cx| {
                        view.update(cx, |view, cx| view.focus_initial(window, cx));
                    });
                    cx.notify();
                } else if this.editor.take().is_some() {
                    this.closed.set(this.closed.get() + 1);
                    cx.notify();
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
                render_message_schedule_panel(view, clearable, apply.clone(), dismiss)
            })
            .into_any_element()
    }
}

fn draw_frames(cx: &mut VisualTestContext) {
    for _ in 0..3 {
        cx.run_until_parked();
        cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            window.refresh();
            window.draw(cx).clear(cx);
        });
    }
}

#[gpui::test]
fn schedule_popover_keeps_invalid_input_and_confirms_from_the_keyboard(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let results = Rc::new(RefCell::new(Vec::new()));
    let closed = Rc::new(Cell::new(0));
    let clearable = Rc::new(Cell::new(false));
    let mut host = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| SchedulePopoverHost {
            results: results.clone(),
            closed: closed.clone(),
            clearable: clearable.clone(),
            editor: None,
        });
        host = Some(view.clone());
        Root::new(view, window, cx)
    });
    let host = host.unwrap();
    let editor = |host: &Entity<SchedulePopoverHost>, cx: &VisualTestContext| {
        host.read_with(cx, |host, _| {
            host.editor
                .as_ref()
                .expect("the popover owns an editor while it is open")
                .view
                .clone()
        })
    };
    draw_frames(cx);
    let trigger = cx.debug_bounds("open-schedule").unwrap();
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    // The editor mounts with the surface and focuses the minutes field.
    let view = editor(&host, cx);
    cx.update(|window, cx| {
        assert!(view.read(cx).minutes.focus_handle(cx).is_focused(window));
    });
    cx.simulate_input("0");
    view.read_with(cx, |view, cx| {
        assert_eq!(view.minutes.read(cx).value().as_ref(), "0");
    });
    // A zero countdown is rejected in place: the surface stays open and keeps
    // what was typed.
    cx.simulate_keystrokes("enter");
    draw_frames(cx);
    assert!(results.borrow().is_empty());
    assert_eq!(closed.get(), 0);
    assert!(cx.debug_bounds("message-schedule-error").is_some());
    view.read_with(cx, |view, cx| {
        assert!(view.error.is_some());
        assert_eq!(view.minutes.read(cx).value().as_ref(), "0");
    });
    cx.simulate_keystrokes("secondary-a");
    cx.simulate_input("2");
    cx.simulate_keystrokes("enter");
    draw_frames(cx);
    assert_eq!(*results.borrow(), vec![Some(MessageSchedule::After(120))]);
    assert_eq!(closed.get(), 1, "confirming closes the popover");
    assert!(host.read_with(cx, |host, _| host.editor.is_none()));

    // A draft without a schedule offers nothing to clear; switching modes and
    // dismissing changes nothing.
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let date_tab = cx.debug_bounds("schedule-date-mode").unwrap();
    cx.simulate_click(date_tab.center(), Modifiers::none());
    draw_frames(cx);
    let view = editor(&host, cx);
    assert!(!view.read_with(cx, |view, _| view.countdown));
    assert!(cx.debug_bounds("clear-message-schedule").is_none());
    let countdown_tab = cx.debug_bounds("schedule-countdown-mode").unwrap();
    cx.simulate_click(countdown_tab.center(), Modifiers::none());
    draw_frames(cx);
    assert!(view.read_with(cx, |view, _| view.countdown));
    cx.simulate_keystrokes("escape");
    draw_frames(cx);
    assert_eq!(
        results.borrow().len(),
        1,
        "dismissal must not change the schedule"
    );
    assert_eq!(closed.get(), 2);

    // With a schedule in place, clearing voids it and keeps the message.
    clearable.set(true);
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let clear = cx.debug_bounds("clear-message-schedule").unwrap();
    cx.simulate_click(clear.center(), Modifiers::none());
    draw_frames(cx);
    assert_eq!(
        *results.borrow(),
        vec![Some(MessageSchedule::After(120)), None]
    );
    assert_eq!(closed.get(), 3);

    // The panel's own button confirms as well, and the countdown the editor
    // opens with is the one it submits.
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let confirm = cx.debug_bounds("confirm-message-schedule").unwrap();
    cx.simulate_click(confirm.center(), Modifiers::none());
    draw_frames(cx);
    assert_eq!(
        *results.borrow(),
        vec![
            Some(MessageSchedule::After(120)),
            None,
            Some(MessageSchedule::After(60))
        ]
    );
    assert_eq!(closed.get(), 4);
}

/// The schedule surface is the smallest one that fits the decision: a popover
/// anchored to the clock button, not the modal dialog it replaced. The panel
/// takes the inline calendar's own width — seven `size_7` day columns and their
/// `gap_0p5` gaps add up to 13rem — so the tabs, the time row and the footer
/// share one spine at every interface font size.
#[test]
fn the_schedule_surface_is_a_popover_sized_to_its_calendar() {
    let source = include_str!("message_schedule.rs");
    assert!(
        !source.contains(".open_dialog("),
        "the schedule editor must not own a modal surface"
    );
    assert!(source.contains("Popover::new(id)"));
    assert!(source.contains("const MESSAGE_SCHEDULE_PANEL_REM: f32 = 13.0;"));
    assert!(source.contains(".w(gpui::rems(MESSAGE_SCHEDULE_PANEL_REM))"));
    assert!(
        !source.contains("MESSAGE_SCHEDULE_CALENDAR_WIDTH"),
        "a second width literal would drift from the day grid it is meant to match"
    );
}

/// The schedule timer releases its own handle in the update that finds nothing
/// left to wait for, so the workbench never carries a handle to a task that has
/// already decided to stop.
#[test]
fn the_schedule_timer_releases_its_handle_where_it_stops() {
    let source = include_str!("message_schedule.rs");
    let timer = source
        .split_once("    pub(super) fn start_message_schedule_timer(")
        .and_then(|(_, tail)| tail.split_once("\n    fn dispatch_due_messages("))
        .map(|(body, _)| body)
        .expect("the schedule timer should remain inspectable");
    let pending = timer
        .find("let pending = this")
        .expect("the timer should decide from the schedules still waiting");
    let release = timer
        .find("this.message_schedule_task = None;")
        .expect("the timer should release its handle");
    assert!(
        pending < release,
        "the handle is released in the same update that finds nothing pending"
    );
    assert!(
        !timer.contains("|this, _, _| this.message_schedule_task = None"),
        "a second update outside the loop describes a decision already made"
    );
}

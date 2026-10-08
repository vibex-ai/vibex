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
fn due_messages_wait_for_editing_and_for_running_or_initializing_sessions() {
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
    for state in [
        None,
        Some(AgentSessionState::Initializing),
        Some(AgentSessionState::Running),
        Some(AgentSessionState::NeedsInput),
        Some(AgentSessionState::Closed),
        Some(AgentSessionState::Archived),
    ] {
        assert!(composer_queue_session_blocks_dispatch(
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            state
        ));
    }
    for state in [AgentSessionState::Idle, AgentSessionState::Error] {
        assert!(!composer_queue_session_blocks_dispatch(
            ComposerQueueDispatchBehavior::Scheduled,
            false,
            Some(state)
        ));
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

struct ScheduleDialogHost {
    results: Rc<RefCell<Vec<Option<MessageSchedule>>>>,
    closed: Rc<Cell<usize>>,
    picker: Option<Entity<MessageSchedulePicker>>,
}

impl Render for ScheduleDialogHost {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            div().debug_selector(|| "open-schedule".into()).child(
                Button::new("open-schedule")
                    .label("Schedule…")
                    .on_click(cx.listener(|this, _, window, cx| {
                        let results = this.results.clone();
                        let closed = this.closed.clone();
                        this.picker = Some(MessageSchedulePicker::open(
                            Some(MessageSchedule::After(60)),
                            true,
                            move |schedule, _, _| {
                                results.borrow_mut().push(schedule);
                                true
                            },
                            move |_| closed.set(closed.get() + 1),
                            window,
                            cx,
                        ));
                        cx.notify();
                    })),
            ),
        )
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
fn schedule_dialog_keeps_invalid_input_and_confirms_once_from_the_keyboard(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let results = Rc::new(RefCell::new(Vec::new()));
    let closed = Rc::new(Cell::new(0));
    let mut host = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|_| ScheduleDialogHost {
            results: results.clone(),
            closed: closed.clone(),
            picker: None,
        });
        host = Some(view.clone());
        Root::new(view, window, cx)
    });
    let host = host.unwrap();
    draw_frames(cx);
    let trigger = cx.debug_bounds("open-schedule").unwrap();
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let picker = host.read_with(cx, |host, _| host.picker.clone().unwrap());
    // The dialog focuses and selects the minutes field after it mounts.
    cx.update(|window, cx| {
        assert!(picker.read(cx).minutes.focus_handle(cx).is_focused(window));
    });
    cx.simulate_input("0");
    picker.read_with(cx, |picker, cx| {
        assert_eq!(picker.minutes.read(cx).value().as_ref(), "0");
    });
    cx.simulate_keystrokes("enter");
    draw_frames(cx);
    assert!(results.borrow().is_empty());
    assert_eq!(closed.get(), 0);
    picker.read_with(cx, |picker, cx| {
        assert!(picker.error.is_some());
        assert_eq!(picker.minutes.read(cx).value().as_ref(), "0");
    });
    cx.simulate_keystrokes("secondary-a");
    cx.simulate_input("2");
    cx.simulate_keystrokes("enter");
    draw_frames(cx);
    assert_eq!(*results.borrow(), vec![Some(MessageSchedule::After(120))]);
    assert_eq!(closed.get(), 1);

    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let date_tab = cx.debug_bounds("schedule-date-mode").unwrap();
    cx.simulate_click(date_tab.center(), Modifiers::none());
    draw_frames(cx);
    let picker = host.read_with(cx, |host, _| host.picker.clone().unwrap());
    assert!(!picker.read_with(cx, |picker, _| picker.countdown));
    let countdown_tab = cx.debug_bounds("schedule-countdown-mode").unwrap();
    cx.simulate_click(countdown_tab.center(), Modifiers::none());
    draw_frames(cx);
    assert!(picker.read_with(cx, |picker, _| picker.countdown));
    cx.simulate_keystrokes("escape");
    draw_frames(cx);
    assert_eq!(
        results.borrow().len(),
        1,
        "dismissal must not change the schedule"
    );
    assert_eq!(closed.get(), 2);

    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let clear = cx.debug_bounds("clear-message-schedule").unwrap();
    cx.simulate_click(clear.center(), Modifiers::none());
    draw_frames(cx);
    assert_eq!(
        *results.borrow(),
        vec![Some(MessageSchedule::After(120)), None]
    );
    assert_eq!(
        closed.get(),
        3,
        "removing a schedule releases the dialog target"
    );
}

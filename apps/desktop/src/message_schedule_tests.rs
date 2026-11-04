use super::*;
use gpui::{Modifiers, TestAppContext, VisualTestContext};
use vibex_core::ProviderProfileId;

fn queued(id: u64, session: &VibexSessionId, at_ms: Option<i64>) -> ComposerQueueMessage {
    ComposerQueueMessage {
        mentions: Vec::new(),
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
        dispatch_key: mint_composer_queue_dispatch_key(),
    }
}

/// A queued message is persisted with the key its send will use.
///
/// The key is what makes a replay after an abrupt restart join the original
/// submission instead of dispatching the message twice, so losing it on the
/// way to the file would silently restore exactly the bug it prevents.
#[test]
fn a_queued_message_persists_the_key_it_submits_with() {
    let session = VibexSessionId::new();
    let message = queued(4, &session, None);
    let entry = persisted_composer_queue_entry(&message);
    assert_eq!(
        entry.dispatch_key.as_deref(),
        Some(message.dispatch_key.as_str())
    );
    let restored = restored_composer_message(entry).expect("the entry round-trips");
    assert_eq!(restored.dispatch_key, message.dispatch_key);
}

/// A queue written before the key was persisted still restores.
///
/// Such an entry may or may not have been sent, and only the key a real send
/// used can prove which; guessing one would collide with a different message.
/// It is therefore restored with a fresh key — the same replay an older build
/// performed — and the durable-record check at adoption is what protects it.
#[test]
fn an_entry_written_without_a_key_is_restored_with_a_fresh_one() {
    let session = VibexSessionId::new();
    let mut entry = persisted_composer_queue_entry(&queued(1, &session, None));
    entry.dispatch_key = None;
    let first = restored_composer_message(entry.clone()).expect("legacy entry restores");
    let second = restored_composer_message(entry).expect("legacy entry restores");
    assert!(first.dispatch_key.starts_with("gpui:queued:"));
    assert_ne!(
        first.dispatch_key, second.dispatch_key,
        "a missing key must not collapse two different messages onto one"
    );
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

/// The sidebar's status column is one narrow lane, so the countdown changes
/// shape with the distance instead of growing an hour field nobody reads.
#[test]
fn the_row_countdown_ticks_in_seconds_and_switches_to_days() {
    assert_eq!(format_schedule_countdown(0), "00:00:00");
    assert_eq!(format_schedule_countdown(-5_000), "00:00:00");
    // A deadline that has not arrived yet never reads as zero.
    assert_eq!(format_schedule_countdown(1), "00:00:01");
    assert_eq!(format_schedule_countdown(59_001), "00:01:00");
    assert_eq!(format_schedule_countdown(3_661_001), "01:01:02");
    assert_eq!(
        format_schedule_countdown(23 * 3_600_000 + 59 * 60_000 + 59_000),
        "23:59:59"
    );
    assert_eq!(format_schedule_countdown(24 * 3_600_000), "1d 00:00");
    assert_eq!(
        format_schedule_countdown(3 * 24 * 3_600_000 + 2 * 3_600_000 + 5 * 60_000),
        "3d 02:05"
    );
}

/// A session opened for a scheduled message is persisted under the manager's
/// default `<agent> session` title, and the message is what names it instead —
/// until the send leaves the queue, after which the Agent's own title may land.
#[test]
fn a_waiting_schedule_names_its_session_after_the_message() {
    let session = |id: &VibexSessionId, title: &str| AgentSession {
        id: id.clone(),
        title: title.to_string(),
        project_id: Default::default(),
        workspace_id: Default::default(),
        workspace_root: "/tmp/workspace".to_string(),
        workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
        agent_id: AgentId::parse("deepseek-harness").unwrap(),
        state: AgentSessionState::Idle,
        safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: 1,
        updated_at_ms: 1,
        last_message_at_ms: 1,
        archived_at_ms: None,
        deleted_at_ms: None,
        tools: Default::default(),
        transport: vibex_core::SessionTransport::Acp,
        terminal: None,
    };
    let session_id = VibexSessionId::new();
    let placeholder = session(&session_id, "deepseek-harness session");
    let mut waiting = queued(1, &session_id, Some(30_000));
    waiting.text = "  Summarize\n the release notes  ".to_string();
    let queue = vec![waiting];
    assert_eq!(
        scheduled_session_title(&queue, &placeholder),
        Some("Summarize the release notes".to_string())
    );
    // The optimistic row already carries the message-derived title, so the name
    // has to hold there too rather than only over the manager's placeholder.
    let optimistic = session(&session_id, "Summarize the release notes");
    assert_eq!(
        scheduled_session_title(&queue, &optimistic),
        Some("Summarize the release notes".to_string())
    );
    // An ordinary queued message names nothing.
    let unscheduled = vec![queued(1, &session_id, None)];
    assert_eq!(scheduled_session_title(&unscheduled, &placeholder), None);
    // A session that already has a name of its own keeps it.
    let named = session(&session_id, "Fix the parser bug");
    assert_eq!(scheduled_session_title(&queue, &named), None);
    // The earliest scheduled message is the one that opens the conversation.
    let mut later = queued(2, &session_id, Some(60_000));
    later.text = "Second message".to_string();
    let mut earliest = queued(3, &session_id, Some(10_000));
    earliest.text = "First message".to_string();
    assert_eq!(
        scheduled_session_title(&[later, earliest], &placeholder),
        Some("First message".to_string())
    );
}

/// The queue is persisted as the panel shows it, and comes back as the message
/// the reader lined up — command, attachments and deadline included.
#[test]
fn the_persisted_queue_round_trips_a_scheduled_command() {
    let session = VibexSessionId::new();
    let mut message = queued(7, &session, Some(1_800_000_000_000));
    message.attachments = vec![vibex_core::MessageAttachment {
        label: "notes.md".to_string(),
        mime_type: Some("text/markdown".to_string()),
        uri: Some("file:///tmp/notes.md".to_string()),
        inline_text_offset: Some(4),
    }];
    message.command_invocation = Some(ComposerCommandInvocation {
        command_id: Some("command:review".to_string()),
        trigger: AgentCommandTrigger::Slash,
        source_kind: AgentCommandSourceKind::Prompt,
        command_text: "/review".to_string(),
        command_name: Some("review".to_string()),
        arguments: Some("the release".to_string()),
        prompt_id: Some(PromptId::parse("prompt_review").unwrap()),
    });
    let restored = restored_composer_message(persisted_composer_queue_entry(&message))
        .expect("a valid entry restores");
    assert_eq!(restored, message);
    // An id the authority would not accept cannot address a send.
    let mut unknown_session = persisted_composer_queue_entry(&message);
    unknown_session.session_id = "not a session id".to_string();
    assert!(restored_composer_message(unknown_session).is_none());
}

/// An in-process reload re-reads the queue file while the shell is live.
///
/// The file is a restored copy, not the authority: a message queued after its
/// last write is only in memory, and replacing the queue with the file would
/// drop a send the reader already committed to.
#[gpui::test]
fn reloading_the_queue_from_disk_keeps_messages_only_memory_has(cx: &mut gpui::TestAppContext) {
    struct Probe {
        workbench: gpui::Entity<VibexWorkbench>,
    }

    impl Render for Probe {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    cx.update(gpui_component::init);
    let (probe, cx) = cx.add_window_view(|window, cx| {
        let workbench = cx.new(|cx| {
            VibexWorkbench::with_initial_state(None, DesktopUiStateV1::default(), None, window, cx)
        });
        Probe { workbench }
    });
    let workbench = probe.read_with(cx, |probe, _| probe.workbench.clone());
    let session = VibexSessionId::new();

    workbench.update(cx, |workbench, _| {
        let live_only = queued(1, &session, None);
        let also_on_disk = queued(2, &session, Some(1_800_000_000_000));
        workbench.composer_queue = vec![live_only.clone(), also_on_disk.clone()];

        // The file the reload reads still holds the older copy of the second
        // message and none of the first.
        let mut stale = persisted_composer_queue_entry(&also_on_disk);
        stale.text = "a stale copy".to_string();
        workbench.restore_composer_queue(vec![stale]);

        assert_eq!(
            workbench.composer_queue,
            vec![live_only, also_on_disk],
            "the reload must not drop a send that only memory holds, \
             nor resurrect the stored copy of one it already has"
        );
    });
}

#[test]
fn the_queue_only_ever_offers_its_head() {
    let session = VibexSessionId::new();
    let other = VibexSessionId::new();
    let queue = vec![
        queued(1, &session, Some(30_000)),
        queued(2, &other, Some(1_000)),
        queued(3, &session, None),
        queued(4, &session, Some(10_000)),
    ];
    // Whatever the head is — a deadline that has not arrived, a due schedule,
    // an ordinary message — nothing behind it is offered, and the session an
    // entry belongs to never changes that: the queue is one ordered list.
    assert_eq!(next_queue_message_index(&queue, |_| false), Some(0));
    // The message behind the head is only offered once the head has left.
    assert_eq!(next_queue_message_index(&queue[1..], |_| false), Some(0));
    // A row the reader is editing holds the queue: the message cannot leave
    // while it is being rewritten, and nothing behind it may overtake it.
    assert_eq!(next_queue_message_index(&queue, |id| id == 1), None);
    assert_eq!(next_queue_message_index(&queue, |id| id == 3), Some(0));
    assert_eq!(next_queue_message_index(&[], |_| false), None);
}

#[test]
fn due_messages_wait_for_editing_and_for_a_busy_or_finished_session() {
    let session = VibexSessionId::new();
    let queue = vec![queued(1, &session, Some(1_000))];
    assert_eq!(next_queue_message_index(&queue, |_| true), None);
    assert_eq!(next_queue_message_index(&queue, |_| false), Some(0));
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
fn only_the_messages_waiting_on_an_hour_are_published_to_other_clients() {
    // A queue holds two different things: a message waiting for this window's
    // turn, and a message waiting for the clock. Only the second is a fact
    // another client can draw — and act on.
    let session = VibexSessionId::new();
    assert!(published_scheduled_send(&queued(1, &session, None)).is_none());

    let published = published_scheduled_send(&queued(2, &session, Some(1_800_000_000_000)))
        .expect("a message with an hour travels");
    assert_eq!(published.session_id, session.as_str());
    assert_eq!(published.message_id, 2);
    assert_eq!(published.text, "scheduled message 2");
    assert_eq!(published.scheduled_at_ms, 1_800_000_000_000);
}

#[test]
fn another_clients_deadline_lands_on_the_message_it_names() {
    let session = VibexSessionId::new();
    let other = VibexSessionId::new();
    let mut queue = vec![
        queued(1, &session, Some(1_000)),
        queued(2, &other, Some(2_000)),
    ];

    assert!(set_queued_message_schedule(
        &mut queue,
        session.as_str(),
        1,
        Some(9_000)
    ));
    assert_eq!(queue[0].scheduled_at_ms, Some(9_000));
    assert_eq!(queue[0].text, "scheduled message 1", "the words stay");
    assert_eq!(queue[1].scheduled_at_ms, Some(2_000), "the other session's");
    assert_eq!(queue.len(), 2, "a deadline is not a deletion");

    // Cancelling hands the message back to the ordinary queue.
    assert!(set_queued_message_schedule(
        &mut queue,
        session.as_str(),
        1,
        None
    ));
    assert_eq!(queue[0].scheduled_at_ms, None);
    assert_eq!(queue[0].id, 1);

    // An id is only unique inside the queue that minted it, so the session is
    // part of the address: another session's message is not this one.
    assert!(!set_queued_message_schedule(
        &mut queue,
        other.as_str(),
        1,
        Some(1_000)
    ));
    assert_eq!(queue[0].scheduled_at_ms, None);
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

/// The absolute editor has no single-line field to answer Enter, so the time
/// field's keystroke has to reach the same decision the Confirm button makes.
/// Handing it to the popover instead would close the surface and quietly drop
/// the time the reader just typed.
#[gpui::test]
fn schedule_panel_confirms_from_the_time_field_with_enter(cx: &mut TestAppContext) {
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
    draw_frames(cx);
    let trigger = cx.debug_bounds("open-schedule").unwrap();
    cx.simulate_click(trigger.center(), Modifiers::none());
    draw_frames(cx);
    let date_tab = cx.debug_bounds("schedule-date-mode").unwrap();
    cx.simulate_click(date_tab.center(), Modifiers::none());
    draw_frames(cx);
    let view = host.read_with(cx, |host, _| {
        host.editor
            .as_ref()
            .expect("the popover owns an editor while it is open")
            .view
            .clone()
    });
    assert!(!view.read_with(cx, |view, _| view.countdown));
    // Point the calendar and the time at a deadline the editor cannot have
    // guessed, then leave the keyboard in the time field, which is where the
    // absolute editor starts the reader.
    let date = Local::now().date_naive() + chrono::Days::new(2);
    let time = chrono::NaiveTime::from_hms_opt(9, 30, 15).unwrap();
    cx.update(|window, cx| {
        view.update(cx, |view, cx| {
            view.calendar.update(cx, |calendar, cx| {
                calendar.set_date(Date::Single(Some(date)), window, cx);
            });
            view.time_field.update(cx, |field, cx| {
                field.set_time(time, window, cx);
            });
            view.focus_initial(window, cx);
        });
    });
    draw_frames(cx);
    cx.update(|window, cx| {
        assert!(
            view.read(cx)
                .time_field
                .read(cx)
                .focus_handle(cx)
                .is_focused(window),
            "the time field is the keyboard this test presses Enter on"
        );
    });
    cx.simulate_keystrokes("enter");
    draw_frames(cx);
    let deadline = local_deadline(NaiveDateTime::new(date, time)).unwrap();
    assert_eq!(*results.borrow(), vec![Some(MessageSchedule::At(deadline))]);
    assert_eq!(closed.get(), 1, "confirming closes the popover");
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

/// The schedule timer releases its own handle wherever it stops.
///
/// Two ways out of the loop have to leave the field empty: finding nothing left
/// to wait for, and finding no window to update in — the workbench outlives its
/// window when it is closed to the tray, and a handle naming a task that has
/// stopped would make the reopened window's re-arm a no-op.
#[test]
fn the_schedule_timer_releases_its_handle_where_it_stops() {
    let source = include_str!("message_schedule.rs");
    let timer = source
        .split_once("    pub(super) fn start_message_schedule_timer(")
        .and_then(|(_, tail)| tail.split_once("\n    pub(super) fn dispatch_due_messages("))
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
    let lost_window = timer
        .find("Err(_) => {")
        .expect("a tick the window no longer supports should be answered");
    let released_without_a_window = timer[lost_window..]
        .find("this.message_schedule_task = None;")
        .expect("a clock that cannot run should release its handle");
    assert!(
        released_without_a_window > 0,
        "the stopped clock releases the handle the reopened window has to replace"
    );
    assert!(
        !timer.contains("|this, _, _| this.message_schedule_task = None"),
        "the windowless release belongs inside the tick that lost its window"
    );
}

/// A window that comes back takes the schedule clock over.
///
/// Closing the window stops the tick without sending anything, so reopening it
/// has to replace the handle, arm a fresh clock for future deadlines, and run
/// the due check at once — a message that fell due while there was no window
/// cannot wait for a tick that was never armed. `defer_in` is part of the
/// wiring: it records the window the entity now lives in, which is what lets
/// the new tick update the workbench.
#[test]
fn reopening_a_window_takes_the_schedule_clock_over() {
    let source = include_str!("app.rs");
    let bind = source
        .split_once("    pub(crate) fn bind_to_window(")
        .and_then(|(_, tail)| tail.split_once("\n    pub(crate) fn prepare_for_window_close("))
        .map(|(body, _)| body)
        .expect("the window binding should remain inspectable");
    let release = bind
        .find("self.message_schedule_task = None;")
        .expect("the reopened window replaces a clock that may have stopped");
    let arm = bind
        .find("self.start_message_schedule_timer(window, cx);")
        .expect("the reopened window arms the clock again");
    let catch_up = bind
        .find("this.dispatch_due_messages(unix_timestamp_ms(), window, cx);")
        .expect("a deadline that passed while closed is released at once");
    assert!(release < arm && arm < catch_up);
    assert!(
        bind.contains("cx.defer_in(window, |this, window, cx| {"),
        "the catch-up registers the window the new clock will run in"
    );
}

/// The two queue switches a client can set are the two the dispatcher reads.
///
/// A phone cannot release a message — the queue is the Desktop's — so every
/// control it draws has to land in the state `maybe_dispatch_next_composer_
/// queue_message` actually consults. This pins that wiring at the source level,
/// because the failure mode is silent: a mutation that updated a private copy
/// would leave the panel looking right and the queue still draining.
#[test]
fn the_queue_switches_a_client_sets_are_the_ones_the_dispatcher_reads() {
    let source = include_str!("app.rs");
    let dispatcher = source
        .split_once("    fn maybe_dispatch_next_composer_queue_message(")
        .and_then(|(_, tail)| tail.split_once("\n    /// "))
        .map(|(body, _)| body)
        .expect("the dispatcher should remain inspectable");
    assert!(
        dispatcher.contains("composer_queue_paused_session_ids"),
        "a paused queue is what holds the head back"
    );
    assert!(
        dispatcher.contains("composer_queue_dispatch_enabled("),
        "and manual mode is the other half of the same decision"
    );

    let schedule = include_str!("message_schedule.rs");
    let pause = schedule
        .split_once("    pub(super) fn set_queue_paused(")
        .and_then(|(_, tail)| tail.split_once("\n    /// "))
        .map(|(body, _)| body)
        .expect("the pause handler should remain inspectable");
    assert!(
        pause.contains("composer_queue_paused_session_ids"),
        "the pause a client sets is the set the dispatcher reads, not a copy"
    );
    // The switch and the release are split, because the release needs a window
    // and the bridge request that sets the switch is already inside an update
    // of the workbench. Lifting a pause must still release the head rather than
    // wait for an event that may never come.
    assert!(
        pause.contains("!paused"),
        "lifting a pause is the caller's cue to release the head"
    );
    let release = schedule
        .split_once("    pub(super) fn release_queue_after_hold_lifted(")
        .and_then(|(_, tail)| tail.split_once("\n    /// "))
        .map(|(body, _)| body)
        .expect("the release should remain inspectable");
    assert!(
        release.contains("maybe_dispatch_next_composer_queue_message("),
        "the release re-evaluates the head"
    );
    assert!(
        source.contains("ComposerQueueDispatchBehavior::ForceNext"),
        "and a lifted pause releases it now instead of waiting for an event"
    );

    let manual = schedule
        .split_once("    pub(super) fn set_queue_manual(")
        .and_then(|(_, tail)| tail.split_once("\n    /// "))
        .map(|(body, _)| body)
        .expect("the mode handler should remain inspectable");
    assert!(
        manual.contains("composer_queue_manual_session_ids"),
        "the mode a client sets is the set the dispatcher reads"
    );
    assert!(
        manual.contains("!manual"),
        "switching back to automatic is the cue to drain the head"
    );
    assert!(
        source.contains("ComposerQueueDispatchBehavior::Automatic"),
        "and it drains through the same release"
    );

    // Publishing the switches is what lets a client draw them at all: a queue
    // that travelled without them would show messages the reader cannot explain
    // the state of.
    assert!(
        source.contains("paused_queue_session_ids: self.composer_queue_paused_session_ids.clone()"),
        "the snapshot carries the pause"
    );
    assert!(
        source.contains("manual_queue_session_ids: self.composer_queue_manual_session_ids.clone()"),
        "and the send mode"
    );
    // The whole queue travels, not only its scheduled subset: a client drawing
    // the schedules alone would show an empty queue beside the Desktop's three
    // waiting messages.
    assert!(
        source.contains("queued_sends: self.published_queued_sends()"),
        "every held message is published, not just the ones with an hour"
    );
}

/// A queue mutation from a client never re-enters the workbench.
///
/// `serve_sidebar_organization` runs inside `entity.update`, so a mutation that
/// opened the window and updated the entity again would take a second lease on
/// a view that is already being updated, and GPUI aborts the process. The work
/// that genuinely needs a window is deferred instead. This pins that split,
/// because the failure is a runtime panic on a path no unit test otherwise
/// walks: a phone sending a prompt during a running turn.
#[test]
fn a_client_queue_mutation_never_re_enters_the_workbench() {
    let source = include_str!("app.rs");
    let serve = source
        .split_once("    fn serve_sidebar_organization(")
        .and_then(|(_, tail)| {
            tail.split_once("\n    /// Runs the queue work a bridge mutation could not do")
        })
        .map(|(body, _)| body)
        .expect("the bridge handler should remain inspectable");
    assert!(
        !serve.contains("entity.update("),
        "the handler already runs inside an update of this entity"
    );
    assert!(
        !serve.contains("cx.update_window("),
        "and it cannot open a window without re-entering that same view"
    );
    // The queue's own work is still done, just later: the mutation lands in the
    // state the response reports, and the window work is collected.
    assert!(
        serve.contains("self.enqueue_queued_send("),
        "the accepted message is in the queue before the response is built"
    );
    assert!(
        serve.contains("self.run_deferred_queue_work(deferred_queue_work, cx)"),
        "and the work needing a window runs after this update returns"
    );

    let deferred = source
        .split_once("    fn run_deferred_queue_work(")
        .and_then(|(_, tail)| tail.split_once("\n    fn start_sidebar_auto_archive_schedule("))
        .map(|(body, _)| body)
        .expect("the deferred pass should remain inspectable");
    assert!(
        deferred.contains("cx.defer("),
        "it waits for the current effect cycle to finish"
    );
    assert!(
        deferred.contains("cx.update_window(") && deferred.contains("entity.update("),
        "and only then opens the window and updates the view once"
    );
}

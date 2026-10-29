use super::*;
use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};
use vibex_backend::DisconnectedBackend;
use vibex_core::{
    AgentEventRawExtension, AgentEventRawOutput, AgentEventRawOutputMode, AgentSession,
    AgentSessionSafety, CommandPayload, CommandStatus, ProjectId, ReasoningPayload, TimelineItemId,
    ToolCallPayload, ToolCallStatus, WorkspaceId,
};

// GPUI's test lookup requires static selectors. Only these bounded fixture
// names are leaked; production selectors retain their normal row ownership.

/// A narrow viewport around the production turn renderer, with the real
/// controller and expansion state. Input still reaches MobileApp's listeners.
struct TimelineProbe {
    app: Entity<MobileApp>,
    width: f32,
    rem: f32,
    _subscription: gpui::Subscription,
}

impl Render for TimelineProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.rem));
        let turn = self.app.read(cx).timeline_turns.last().unwrap().clone();
        let composer = self.app.read(cx).composer_input.clone();
        let content = self.app.update(cx, |app, cx| {
            app.render_turn(&turn, true, true, window, cx)
                .into_any_element()
        });
        div()
            .debug_selector(|| "timeline-probe".into())
            .w(px(self.width))
            .min_w_0()
            .tab_group()
            // The same release layer the app root mounts.
            .child(crate::touch_selection::watch(composer))
            .child(content)
    }
}

fn item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_mobile_{sequence}")).unwrap(),
        session_id: VibexSessionId::parse("session_mobile_timeline").unwrap(),
        sequence,
        timestamp_ms: sequence,
        source: TimelineSource::Agent,
        kind: payload.kind(),
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: TimelineRedactionState::None,
        execution_attribution: None,
        payload_depth: vibex_core::TimelinePayloadDepth::Detail,
        payload,
    }
}

fn open_timeline<'a>(
    cx: &'a mut TestAppContext,
    data_dir: &std::path::Path,
    payloads: Vec<TimelinePayload>,
) -> (
    Entity<MobileApp>,
    Entity<TimelineProbe>,
    &'a mut VisualTestContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let mut handles = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let app = cx.new(|cx| {
            let mut app = MobileApp::new_for_test(data_dir.to_path_buf(), window, cx);
            let backend = DisconnectedBackend::facade();
            let mut controller =
                AgentWorkflowController::new(backend.agent().clone(), backend.capabilities().agent);
            let session_id = VibexSessionId::parse("session_mobile_timeline").unwrap();
            controller.state.selected_session_id = Some(session_id.clone());
            controller.state.active_session.resolve(AgentSession {
                id: session_id.clone(),
                title: "Review changes".into(),
                project_id: ProjectId::new(),
                workspace_id: WorkspaceId::new(),
                workspace_root: "/workspace".into(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                agent_id: AgentId::parse("codex").unwrap(),
                state: AgentSessionState::Running,
                safety: AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: 1,
                updated_at_ms: 1,
                last_message_at_ms: 1,
                archived_at_ms: None,
                deleted_at_ms: None,
            });
            let items: Vec<_> = std::iter::once(TimelinePayload::UserMessage(UserMessagePayload {
                text: "Review the implementation".into(),
                ..Default::default()
            }))
            .chain(payloads)
            .enumerate()
            .map(|(ix, payload)| item(ix as i64 + 1, payload))
            .collect();
            controller
                .state
                .timeline
                .replace_authoritative(session_id, items);
            app.controller = Some(controller);
            app.desktop_timeline_display_settings.reasoning_display_mode =
                AgentTimelineReasoningDisplayMode::Timeline;
            app.rebuild_timeline_turns();
            app
        });
        let probe = cx.new(|cx| TimelineProbe {
            app: app.clone(),
            width: 320.0,
            rem: 15.0,
            _subscription: cx.observe(&app, |_, _, cx| cx.notify()),
        });
        handles = Some((app, probe.clone()));
        gpui_component::Root::new(probe, window, cx)
    });
    let (app, probe) = handles.unwrap();
    draw(cx);
    (app, probe, cx)
}

#[gpui::test]
fn session_metadata_refreshes_roots_without_reloading_the_open_worker(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::AgentMessage(
            vibex_core::AgentMessagePayload {
                text: "Worker result".into(),
                is_final: true,
            },
        )],
    );
    let worker = app.read_with(cx, |app, _| {
        app.controller
            .as_ref()
            .unwrap()
            .state
            .active_session
            .value
            .clone()
            .unwrap()
    });
    let root = AgentSession {
        id: VibexSessionId::new(),
        title: "Team owner".into(),
        ..worker.clone()
    };
    let new_root = AgentSession {
        id: VibexSessionId::new(),
        title: "New independent session".into(),
        ..root.clone()
    };
    let updated_worker = AgentSession {
        title: "Worker ready for review".into(),
        updated_at_ms: worker.updated_at_ms + 1,
        ..worker.clone()
    };
    cx.update(|window, cx| {
        app.update(cx, |app, cx| {
            app.composer_input.update(cx, |input, cx| {
                input.set_value("Keep this worker draft", window, cx);
            });
            let controller = app.controller.as_mut().unwrap();
            controller.apply_sessions(Ok(vec![root.clone()])).unwrap();
            // A root snapshot is already in flight when metadata arrives.
            // Retain one follow-up refresh instead of losing the new root.
            controller.begin_sessions_refresh();
            app.session_sync_busy = true;
        });
    });
    let (generation, timeline) = app.read_with(cx, |app, _| {
        let state = &app.controller.as_ref().unwrap().state;
        (state.generation, state.timeline.items.clone())
    });
    app.update(cx, |app, cx| {
        assert!(!app.apply_event_batch(
            vec![
                BackendEvent::SessionUpdated(updated_worker.clone()),
                BackendEvent::SessionUpdated(new_root.clone()),
            ],
            cx,
        ));
        assert!(app.session_sync_queued);
        let controller = app.controller.as_mut().unwrap();
        assert_eq!(controller.state.sessions.value, Some(vec![root.clone()]));
        controller
            .apply_sessions(Ok(vec![root.clone(), new_root.clone()]))
            .unwrap();
        app.sync_sidebar_state();
    });
    draw(cx);
    app.read_with(cx, |app, cx| {
        let state = &app.controller.as_ref().unwrap().state;
        assert_eq!(state.sessions.value, Some(vec![root, new_root]));
        assert_eq!(state.selected_session_id, Some(worker.id));
        assert_eq!(state.active_session.value, Some(updated_worker));
        assert_eq!(state.generation, generation);
        assert_eq!(state.timeline.items, timeline);
        assert_eq!(
            app.composer_input.read(cx).value(),
            "Keep this worker draft"
        );
    });
}

fn read(path: &str, status: ToolCallStatus, output: &str) -> TimelinePayload {
    let input = serde_json::json!({ "path": path, "limit": 40 }).to_string();
    TimelinePayload::ToolCall(ToolCallPayload {
        tool_call_id: format!("read:{path}"),
        tool_name: "read_file".into(),
        status,
        summary: "read_file".into(),
        input_summary: None,
        output_summary: Some("terminal output".into()),
        raw_extension: Some(AgentEventRawExtension::new(
            Vec::new(),
            Some(input),
            Some(AgentEventRawOutput::new(AgentEventRawOutputMode::Snapshot, output).0),
            Vec::new(),
            Default::default(),
            false,
        )),
    })
}

fn draw(cx: &mut VisualTestContext) {
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

fn click(cx: &mut VisualTestContext, selector: &str) {
    let bounds = cx
        .debug_bounds(selector.to_string().leak())
        .unwrap_or_else(|| panic!("{selector} is visible"));
    cx.simulate_click(bounds.center(), Modifiers::none());
    draw(cx);
}

fn press(cx: &mut VisualTestContext, key: &str) {
    let keystroke = Keystroke::parse(key).unwrap();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
    draw(cx);
}

/// Long-presses the first word of the fixture's answer and returns what the
/// gesture selected.
///
/// The probe draws one user message above the answer, so the press lands on the
/// answer's first line; the returned text is what says the press found text at
/// all, rather than a gap between rows.
fn long_press_answer_word(cx: &mut VisualTestContext) -> String {
    let probe = cx
        .debug_bounds("timeline-probe")
        .expect("the timeline probe is drawn");
    let position = gpui::point(probe.left() + px(20.0), probe.top() + px(100.0));
    for phase in [gpui::TouchPhase::Started, gpui::TouchPhase::Ended] {
        cx.simulate_event(gpui::LongPressEvent {
            phase,
            start_position: position,
            position,
        });
        draw(cx);
    }
    cx.update(gpui_base::TextSelection::selected_text)
}

/// A long press over a message leaves the kit's copy menu; pressing Copy has to
/// put the selected word on the clipboard, which is what pasting into another
/// application reads.
///
/// The press is identified by where it is rather than by what it says: the
/// labels follow the language setting, and this test is not about them.
#[gpui::test]
fn pressing_copy_in_the_touch_menu_reaches_the_clipboard(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (_app, _probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::AgentMessage(
            vibex_core::AgentMessagePayload {
                text: "quick select value".into(),
                is_final: true,
            },
        )],
    );

    let ticks = crate::platform::feedbacks();
    assert_eq!(
        long_press_answer_word(cx).trim(),
        "quick",
        "the press should select the answer's first word"
    );
    assert_eq!(
        crate::platform::feedbacks(),
        ticks + 1,
        "a long press that takes a selection is what asks for the tick"
    );
    // Read-only text offers Copy and Select All, in that order.
    let items = crate::test_support::touch_menu_items(cx);
    assert_eq!(
        items.len(),
        2,
        "read-only text offers Copy and Select All: {:?}",
        items.iter().map(|(label, _)| label).collect::<Vec<_>>()
    );
    let copied = items[0].1.center();
    let elsewhere = items[1].1.center();

    cx.simulate_click(copied, Modifiers::none());
    draw(cx);

    assert_eq!(
        cx.read_from_clipboard()
            .and_then(|item| item.text())
            .as_deref(),
        Some("quick")
    );
    assert!(
        crate::test_support::touch_menu_items(cx).is_empty(),
        "the menu should close once it has acted"
    );
    assert!(
        cx.update(|window, cx| gpui_base::TextSelection::touch_selection(window, cx))
            .is_none(),
        "the selection should let go once Copy has taken what it wanted"
    );

    // Select All is the other item, and the one case that keeps what it made:
    // the menu comes back over the whole answer, with the selection under it.
    assert_eq!(long_press_answer_word(cx).trim(), "quick");
    cx.simulate_click(elsewhere, Modifiers::none());
    draw(cx);
    let snapshot = cx
        .update(|window, cx| gpui_base::TextSelection::touch_selection(window, cx))
        .expect("Select All keeps the selection");
    assert!(snapshot.is_menu_open());
    assert_eq!(
        cx.update(gpui_base::TextSelection::selected_text).trim(),
        "quick select value"
    );
}

/// A finger that scrolls the text under its selection is not a press on the
/// toolbar: the menu steps aside for the gesture and comes back when the finger
/// lifts, and neither end of it is a new selection asking for a tick.
#[gpui::test]
fn scrolling_under_a_selection_keeps_it(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (_app, _probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::AgentMessage(
            vibex_core::AgentMessagePayload {
                text: "quick select value".into(),
                is_final: true,
            },
        )],
    );
    assert_eq!(long_press_answer_word(cx).trim(), "quick");
    let ticks = crate::platform::feedbacks();
    let over_the_text = cx
        .update(|window, cx| gpui_base::TextSelection::touch_selection(window, cx))
        .and_then(|snapshot| snapshot.bounds())
        .expect("the press selected text")
        .center();

    for phase in [
        gpui::TouchPhase::Started,
        gpui::TouchPhase::Moved,
        gpui::TouchPhase::Ended,
    ] {
        cx.simulate_event(gpui::ScrollWheelEvent {
            position: over_the_text,
            delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(-40.0))),
            modifiers: Modifiers::none(),
            touch_phase: phase,
        });
        draw(cx);
    }

    let snapshot = cx
        .update(|window, cx| gpui_base::TextSelection::touch_selection(window, cx))
        .expect("a scroll does not take the selection away");
    assert!(
        snapshot.is_menu_open(),
        "the menu comes back with the finger up"
    );
    assert_eq!(
        crate::platform::feedbacks(),
        ticks,
        "the same selection coming back is not a new gesture"
    );
}

/// Those buttons belong to the kit, so their labels come from the kit's own
/// translation table; the language setting has to reach it, or a phone set to
/// Chinese still shows English under its finger.
#[gpui::test]
fn the_touch_menu_over_a_message_follows_the_language_setting(cx: &mut TestAppContext) {
    crate::locale::set_preference(crate::locale::LanguagePreference::ZhCn);
    let data_dir = tempfile::tempdir().unwrap();
    let (_app, _probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::AgentMessage(
            vibex_core::AgentMessagePayload {
                text: "quick select value".into(),
                is_final: true,
            },
        )],
    );

    assert_eq!(long_press_answer_word(cx).trim(), "quick");
    let labels = crate::test_support::touch_menu_items(cx)
        .into_iter()
        .map(|(label, _)| label)
        .collect::<Vec<_>>();
    // Restore before asserting: a failure must not leave the process — and the
    // tests running beside this one — on a language the user never picked.
    crate::locale::set_preference(crate::locale::LanguagePreference::System);

    assert_eq!(labels, vec!["复制".to_string(), "全选".to_string()]);
}

#[gpui::test]
fn live_activity_groups_honor_touch_and_keyboard_choices_across_refresh(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![
            read("Cargo.toml", ToolCallStatus::Completed, "workspace"),
            read("src/lib.rs", ToolCallStatus::Completed, "pub mod app;"),
            read("src/app.rs", ToolCallStatus::Started, ""),
        ],
    );
    let (group_id, row_id) = app.read_with(cx, |app, _| {
        let turn = &app.timeline_turns[0];
        (
            turn.process_activity_groups_with_file_operations[0]
                .id
                .clone(),
            turn.process_rows[0].id.clone(),
        )
    });
    let group_header = format!("activity-group-header:{group_id}");
    let row = format!("activity:{row_id}");
    assert!(
        cx.debug_bounds(row.clone().leak()).is_some(),
        "the live group starts open"
    );
    click(cx, &group_header);
    assert!(
        cx.debug_bounds(row.clone().leak()).is_none(),
        "a running group can be collapsed"
    );
    app.update(cx, |app, cx| {
        let controller = app.controller.as_mut().unwrap();
        let TimelinePayload::ToolCall(tool) = &mut controller.state.timeline.items[3].payload
        else {
            panic!("tool call")
        };
        tool.status = ToolCallStatus::Completed;
        app.rebuild_timeline_turns();
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds(row.clone().leak()).is_none(),
        "history refresh preserves the choice"
    );
    cx.update(|window, cx| {
        window.blur(cx);
        window.focus_next(cx);
    });
    press(cx, "tab");
    press(cx, "enter");
    assert!(
        cx.debug_bounds(row.clone().leak()).is_some(),
        "Enter opens the same disclosure"
    );
    press(cx, "space");
    assert!(
        cx.debug_bounds(row.clone().leak()).is_none(),
        "Space also collapses the group"
    );
}

#[gpui::test]
fn activity_details_fit_phone_widths_and_copy_the_complete_invocation(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let path = "src/a-long-directory/a-component-name-that-must-fit-the-phone-header.rs";
    let input = serde_json::json!({ "path": path, "limit": 40 }).to_string();
    let (app, probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![read(
            path,
            ToolCallStatus::Started,
            &"first line\n  indented line\n".repeat(40),
        )],
    );
    let row_id = app.read_with(cx, |app, _| {
        app.timeline_turns[0].process_rows[0].id.clone()
    });
    let header = format!("activity-header:{row_id}");
    for (width, rem, scale) in [(320.0, 15.0, 1.0), (400.0, 18.0, 1.25)] {
        cx.simulate_scale_factor_change(scale);
        probe.update(cx, |probe, cx| {
            probe.width = width;
            probe.rem = rem;
            cx.notify();
        });
        app.update(cx, |app, cx| {
            app.set_row_expanded(row_id.clone(), false, cx)
        });
        draw(cx);
        let bounds = cx.debug_bounds(header.clone().leak()).unwrap();
        let target = cx
            .debug_bounds(format!("activity-target:{row_id}").leak())
            .unwrap();
        assert!((bounds.size.height - px(rem * HEADER_HEIGHT_REM)).abs() <= px(1.0));
        assert!(target.size.width > px(0.0));
        assert!(target.right() <= bounds.right());
        assert!(bounds.right() <= cx.debug_bounds("timeline-probe").unwrap().right());
        click(cx, &header);
        let details = cx
            .debug_bounds(format!("activity-detail:{row_id}:1").leak())
            .unwrap();
        let copy = cx
            .debug_bounds(format!("activity-copy:{row_id}:1").leak())
            .unwrap();
        assert!(
            details.size.height <= px(rem * 12.0 + 1.0),
            "output has a bounded scroll area"
        );
        assert!(details.right() <= copy.left());
        assert!(copy.right() <= bounds.right());
        assert!(
            copy.size.width >= px(rem * 3.0 - 1.0),
            "copy stays touch-sized"
        );
        click(cx, &format!("activity-copy:{row_id}:0"));
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some(input.clone())
        );
        assert!(app.read_with(cx, |app, _| app.timeline_row_expanded(&row_id)));
    }
}

#[gpui::test]
fn reasoning_tracks_the_tail_and_preserves_explicit_full_or_collapsed_choices(
    cx: &mut TestAppContext,
) {
    let data_dir = tempfile::tempdir().unwrap();
    let source = (0..30)
        .map(|ix| format!("Reasoning paragraph {ix}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let (app, _, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::Reasoning(ReasoningPayload {
            text: source,
            is_final: false,
        })],
    );
    let row_id = app.read_with(cx, |app, _| {
        app.timeline_turns[0].process_rows[0].id.clone()
    });
    let window_selector = format!("reasoning-window:{row_id}");
    let body_selector = format!("reasoning-content:{row_id}");
    // Default expansion is off, so a live thought stays folded: nothing opens
    // itself, not even the thought the Agent is on.
    assert!(cx.debug_bounds(body_selector.clone().leak()).is_none());
    // The reader opens it, and the live window is the shape it opens in.
    click(cx, &format!("reasoning-header:{row_id}"));
    let window = cx.debug_bounds(window_selector.clone().leak()).unwrap();
    let content = cx.debug_bounds(body_selector.clone().leak()).unwrap();
    assert!(window.size.height <= px(15.0 * theme::BODY_LINE_HEIGHT_REM * 6.0 + 1.0));
    assert!(content.size.height > window.size.height);
    assert!(
        (content.bottom() - window.bottom()).abs() <= px(1.0),
        "the newest content stays at the bottom"
    );
    click(cx, &format!("reasoning-full:{row_id}"));
    assert!(cx.debug_bounds(window_selector.clone().leak()).is_none());
    assert!(
        cx.debug_bounds(body_selector.clone().leak())
            .unwrap()
            .size
            .height
            > window.size.height
    );
    app.update(cx, |app, cx| {
        let controller = app.controller.as_mut().unwrap();
        let TimelinePayload::Reasoning(reasoning) = &mut controller.state.timeline.items[1].payload
        else {
            panic!("reasoning")
        };
        // An equal-length, same-sequence correction must update the cached view.
        reasoning.text = reasoning.text.replace("paragraph", "corrected");
        app.rebuild_timeline_turns();
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds(window_selector.clone().leak()).is_none(),
        "full mode survives streaming updates"
    );
    let cached = app.read_with(cx, |app, _| {
        app.timeline_markdown_views
            .borrow()
            .get(&format!("thought:{row_id}"))
            .unwrap()
            .2
            .clone()
    });
    assert!(cached.contains("corrected"));
    assert!(!cached.contains("paragraph"));
    click(cx, &format!("reasoning-full:{row_id}"));
    assert!(cx.debug_bounds(window_selector.clone().leak()).is_some());
    click(cx, &format!("reasoning-header:{row_id}"));
    assert!(cx.debug_bounds(body_selector.clone().leak()).is_none());
    app.update(cx, |app, cx| {
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .items
            .push(item(
                3,
                TimelinePayload::Command(CommandPayload {
                    command: "cargo check".into(),
                    cwd: None,
                    status: CommandStatus::Started,
                    exit_code: None,
                    output_summary: None,
                    raw_extension: None,
                }),
            ));
        app.rebuild_timeline_turns();
        cx.notify();
    });
    draw(cx);
    assert!(
        cx.debug_bounds(window_selector.clone().leak()).is_none(),
        "superseded reasoning has no live window"
    );
    assert!(
        cx.debug_bounds(body_selector.clone().leak()).is_none(),
        "explicit collapse survives the next tool"
    );
}

/// A window read from disk is a stand-in projection: it paints the
/// conversation before the load that replaces it answers, and the controller's
/// projection takes over the moment it exists.
#[gpui::test]
fn a_cached_window_paints_until_the_controller_has_a_projection(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    let session_id = VibexSessionId::parse("session_mobile_timeline").unwrap();
    app.update(cx, |app, cx| {
        // A cold load is still in flight, so the controller has no projection
        // and the cached window is what the reader sees.
        app.controller.as_mut().unwrap().state.timeline = TimelineModel::default();
        app.timeline_cache_timeline = Some(TimelineModel {
            session_id: Some(session_id.clone()),
            items: vec![item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "from disk".into(),
                    ..Default::default()
                }),
            )],
            ..Default::default()
        });
        app.rebuild_timeline_turns();
        assert_eq!(app.timeline_turns.len(), 1);
        assert_eq!(app.timeline_turns[0].item_count, 1);
        assert!(app.timeline_turns[0].conclusion_row.is_none());
        assert!(app.timeline_is_using_cached_window());

        // The authoritative answer is the conversation now.
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .replace_authoritative(
                session_id,
                vec![
                    item(
                        1,
                        TimelinePayload::UserMessage(UserMessagePayload {
                            text: "from disk".into(),
                            ..Default::default()
                        }),
                    ),
                    item(
                        2,
                        TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                            text: "answered".into(),
                            is_final: true,
                        }),
                    ),
                ],
            );
        app.rebuild_timeline_turns();
        assert_eq!(app.timeline_turns.len(), 1);
        assert_eq!(app.timeline_turns[0].item_count, 2);
        assert!(app.timeline_turns[0].conclusion_row.is_some());
        assert!(!app.timeline_is_using_cached_window());
        cx.notify();
    });
    draw(cx);
}

/// Pages ship summary payloads, so an opened activity asks for exactly the
/// item behind its row — and only when that item is missing detail.
#[gpui::test]
fn opening_a_row_resolves_the_summary_item_it_needs(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![read(
            "src/lib.rs",
            ToolCallStatus::Completed,
            "pub mod app;",
        )],
    );
    let row_id = app.read_with(cx, |app, _| {
        app.timeline_turns[0].process_rows[0].id.clone()
    });
    app.update(cx, |app, _| {
        // The fixture item is stored at full depth, so there is nothing to
        // fetch and expanding it must not ask the runtime for anything.
        assert_eq!(app.summary_row_sequence(&row_id), None);

        let items = &mut app.controller.as_mut().unwrap().state.timeline.items;
        items[1] = items[1].clone().into_summary();
        assert_eq!(app.summary_row_sequence(&row_id), Some(2));
        // A row the client projects itself has no item behind it.
        assert_eq!(app.summary_row_sequence("pending-user-message"), None);
    });
}

/// The open window is what the next cold start paints, and it is keyed by the
/// runtime that produced it.
#[gpui::test]
fn the_open_window_is_written_for_the_runtime_that_produced_it(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    let session_id = VibexSessionId::parse("session_mobile_timeline").unwrap();
    app.update(cx, |app, _| {
        app.active_host_id = Some("desktop-primary".to_string());
        app.persist_active_session_timeline();
        let record = app
            .load_cached_session_window(&session_id)
            .expect("the open window should be cached");
        assert_eq!(record.session_id, session_id);
        assert_eq!(record.items.len(), 1);

        // Session ids are minted per runtime, so another one never reads it.
        app.active_host_id = Some("desktop-secondary".to_string());
        assert!(app.load_cached_session_window(&session_id).is_none());
    });
}

/// Older history arrives above the window: the turn the reader is on stays
/// under the viewport, and the prefetch band is derived from where that landed
/// rather than from a count of rows pushed in at the head.
#[gpui::test]
fn an_older_page_prepends_without_moving_the_reader(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    app.update(cx, |app, cx| {
        // The reader is parked up the conversation, not following the end.
        app.timeline_list.pause_following_tail();
        let first_visible_before = 1;
        app.timeline_viewport = Some(TimelineViewport {
            first_visible: first_visible_before,
            visible: 4,
        });
        let session_id = app
            .controller
            .as_ref()
            .unwrap()
            .state
            .timeline
            .session_id
            .clone()
            .unwrap();
        // The page the runtime answered with sits below the loaded window.
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .replace_authoritative(
                session_id,
                vec![
                    item(
                        0,
                        TimelinePayload::UserMessage(UserMessagePayload {
                            text: "earlier question".into(),
                            ..Default::default()
                        }),
                    ),
                    item(
                        1,
                        TimelinePayload::UserMessage(UserMessagePayload {
                            text: "Review the implementation".into(),
                            ..Default::default()
                        }),
                    ),
                ],
            );
        app.prepend_timeline_turns();
        assert_eq!(app.timeline_turns.len(), 2);
        assert_eq!(app.timeline_list.item_count(), 2);
        // The reader was on the loaded turn, which the page pushed down by one:
        // the restore names that turn, and the band is read back from where the
        // restore landed rather than from a count of rows added at the head.
        assert_eq!(
            app.timeline_viewport.unwrap().first_visible,
            1,
            "the viewport stays on the turn the reader was reading"
        );
        assert_eq!(
            app.timeline_viewport.unwrap().visible,
            4,
            "the band keeps the height the list last reported"
        );
        cx.notify();
    });
    draw(cx);
}

/// A page landing above the reader renames the turn they are on, and the
/// anchor has to survive it.
///
/// A turn is named after whichever of its rows the window saw first:
/// `turn:continuation:{first response item}` while the window opens inside the
/// turn, `turn:{user item}` once a page reaches back to the turn's own message.
/// The rename happens exactly when older history lands, which is when the anchor
/// is being re-resolved — so an anchor keyed only by id stops resolving at the
/// one moment it is needed. The sequence span is the turn's own content and
/// carries the anchor across the rename.
#[gpui::test]
fn an_older_page_that_renames_the_leading_turn_keeps_the_reader_on_it(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    app.update(cx, |app, cx| {
        let session_id = app
            .controller
            .as_ref()
            .unwrap()
            .state
            .timeline
            .session_id
            .clone()
            .unwrap();

        // A window that opens inside a turn: its own message is above the cut,
        // so the turn is named after its first response row.
        let tail_only = vec![
            item(
                2,
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: "the answer so far".into(),
                    is_final: false,
                }),
            ),
            item(
                3,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "a later question".into(),
                    ..Default::default()
                }),
            ),
        ];
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .replace_authoritative(session_id.clone(), tail_only);
        app.rebuild_timeline_turns();
        let renamed_from = app.timeline_turns[0].id.clone();
        assert!(
            renamed_from.starts_with("turn:continuation:"),
            "the fixture has to start with the turn named after its first response row: {renamed_from}"
        );

        // The reader parks inside that turn, which is the turn a page landing
        // above is about to rename.
        let anchor = TimelineScrollAnchor {
            turn_id: renamed_from.clone(),
            span: timeline_turn_anchor_span(&app.timeline_turns[0]),
            offset_in_item: 120,
        };
        assert_eq!(anchor.span.map(|span| span.first_sequence), Some(2));

        // The page reaches back to the turn's own message, so the projection can
        // finally name the turn after it.
        let framed = vec![
            item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "the original question".into(),
                    ..Default::default()
                }),
            ),
            item(
                2,
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: "the answer so far".into(),
                    is_final: false,
                }),
            ),
            item(
                3,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "a later question".into(),
                    ..Default::default()
                }),
            ),
        ];
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .replace_authoritative(session_id, framed);
        app.timeline_turns = std::sync::Arc::new(app.build_timeline_turns());
        let renamed_to = app.timeline_turns[0].id.clone();
        assert_ne!(
            renamed_to, renamed_from,
            "the page has to rename the turn for this test to mean anything"
        );

        // The id alone no longer resolves; the span still does.
        assert!(
            app.timeline_turns
                .iter()
                .all(|turn| turn.id != renamed_from)
        );
        assert_eq!(
            timeline_anchor_row_index(&app.timeline_turns, &anchor),
            Some(0),
            "the renamed turn is found by the sequences it covers"
        );

        app.timeline_list
            .reset_with_uniform_height(app.timeline_turns.len(), px(180.0));
        assert_eq!(
            app.restore_timeline_scroll_anchor(Some(anchor)),
            Some(0),
            "the reader is put back on the turn they were reading"
        );
        cx.notify();
    });
    draw(cx);
}

/// An anchor no turn accounts for leaves the list where it is.
///
/// The window keeps the newest turns, so a turn the reader was on can be gone
/// from it entirely. Jumping to the newest turn there is the far end of the
/// conversation from where they were; leaving the position alone is strictly
/// closer to it, whichever way it turns out to be wrong.
#[gpui::test]
fn an_unresolvable_anchor_does_not_jump_to_the_newest_turn(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    app.update(cx, |app, cx| {
        app.timeline_list
            .reset_with_uniform_height(app.timeline_turns.len(), px(180.0));
        app.timeline_list.scroll_to(ListOffset {
            item_ix: 0,
            offset_in_item: px(0.0),
        });
        let before = app.timeline_list.logical_scroll_top().item_ix;

        let gone = TimelineScrollAnchor {
            turn_id: "turn:a-turn-the-window-dropped".into(),
            span: Some(vibex_desktop_model::TimelineAnchorSpan {
                first_sequence: 900,
                last_sequence: 905,
            }),
            offset_in_item: 42,
        };
        assert_eq!(app.restore_timeline_scroll_anchor(Some(gone)), None);
        assert_eq!(
            app.timeline_list.logical_scroll_top().item_ix,
            before,
            "nothing is scrolled when the anchor names a turn the window lost"
        );
        cx.notify();
    });
    draw(cx);
}

/// A turn appended while the conversation runs still has a height.
///
/// `splice` inserts rows with no size hint, which the list summarises as zero
/// height, so a turn that arrived during streaming used to leave the reported
/// extent exactly as short as it was. The extent is what the list clamps its own
/// scrolling to, and it is what the follow decision measures against, so a tail
/// of heightless rows is a transcript that measures as if it ended early.
#[gpui::test]
fn a_turn_appended_during_a_run_grows_the_reported_extent(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _, cx) = open_timeline(cx, data_dir.path(), vec![]);
    app.update(cx, |app, cx| {
        let before = app.timeline_list.max_offset_for_scrollbar().y;
        let turns_before = app.timeline_list.item_count();

        let session_id = app
            .controller
            .as_ref()
            .unwrap()
            .state
            .timeline
            .session_id
            .clone()
            .unwrap();
        let mut items = app
            .controller
            .as_ref()
            .unwrap()
            .state
            .timeline
            .items
            .clone();
        // Two more turns, each opened by its own message.
        for n in 0..2 {
            items.push(item(
                items.len() as i64 + 1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: format!("later question {n}"),
                    ..Default::default()
                }),
            ));
        }
        app.controller
            .as_mut()
            .unwrap()
            .state
            .timeline
            .replace_authoritative(session_id, items);
        app.rebuild_timeline_turns();

        let turns_after = app.timeline_list.item_count();
        assert!(turns_after > turns_before);
        let added = (turns_after - turns_before) as f32;
        assert_eq!(
            app.timeline_list.max_offset_for_scrollbar().y,
            before + px(added * TIMELINE_TURN_ESTIMATED_HEIGHT_PX),
            "each appended turn has to weigh its estimate in the extent"
        );
        cx.notify();
    });
    draw(cx);
}

/// The timeline list as the app configures it, with one tall live turn.
struct TallTurnList {
    list: ListState,
}

impl Render for TallTurnList {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        div().size_full().child(
            gpui::list(self.list.clone(), |_, _, _| {
                div().w_full().h(px(4000.0)).into_any_element()
            })
            .size_full(),
        )
    }
}

/// A reader dragging upward inside the streaming turn is not handed back.
///
/// The live turn is one list item and usually taller than the screen, so a
/// gesture inside it never moves the top to another turn. Following has to
/// stop on the gesture itself, and a re-measure of the growing turn must leave
/// the reader where they scrolled to.
#[gpui::test]
fn scrolling_up_inside_the_live_turn_stops_following(cx: &mut TestAppContext) {
    let list = timeline_list_state(1);
    let (_, cx) = cx.add_window_view(|_, _| TallTurnList { list: list.clone() });
    draw(cx);
    assert!(
        list.is_following_tail(),
        "a fresh timeline follows the live end"
    );

    cx.simulate_event(gpui::ScrollWheelEvent {
        position: list.viewport_bounds().center(),
        delta: gpui::ScrollDelta::Pixels(gpui::point(px(0.0), px(30.0))),
        modifiers: Modifiers::none(),
        touch_phase: gpui::TouchPhase::Moved,
    });
    draw(cx);
    assert!(
        !list.is_following_tail(),
        "a 30px pull toward older content is already the reader's decision"
    );
    let parked = list.logical_scroll_top();
    assert_eq!(parked.item_ix, 0, "the reader is inside the one tall turn");

    // The turn keeps streaming; the list re-measures it the way the event
    // batch does.
    list.remeasure_items(0..1);
    draw(cx);
    let after = list.logical_scroll_top();
    assert_eq!(
        (parked.item_ix, parked.offset_in_item),
        (after.item_ix, after.offset_in_item),
        "the growing turn does not pull the reader back"
    );
}

/// A request the reader has answered collapses to a record they can open.
///
/// The request keeps its `Pending` status after it is resolved — the durable
/// resolution is its own timeline item — so a row that asked the payload would
/// keep drawing the form. This pins the behaviour the phone owes the reader:
/// once answered, the row is a summary; the answers are one tap away.
#[gpui::test]
fn a_settled_input_request_collapses_to_its_record(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let request_id = RequestId::parse("request_mobile_settled").unwrap();
    let request = vibex_core::ElicitationRequest {
        id: request_id.clone(),
        session_id: VibexSessionId::parse("session_mobile_timeline").unwrap(),
        provider_request_id: None,
        tool_call_id: None,
        message: "Which state model should the rail use?".into(),
        title: None,
        description: None,
        fields: vec![vibex_core::ElicitationField {
            id: "approach".into(),
            title: "Approach".into(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: vec![vibex_core::ElicitationOption {
                    value: "global".into(),
                    title: "Global state".into(),
                    description: None,
                }],
            },
        }],
        status: vibex_core::ElicitationRequestStatus::Pending,
        requested_at_ms: 1,
    };
    // The form is up first, exactly as the reader sees it before answering.
    let (app, _, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![TimelinePayload::ElicitationRequest(request.clone())],
    );
    let awaiting = app.read_with(cx, |app, _| {
        app.timeline_turns
            .iter()
            .flat_map(|turn| turn.process_rows.iter())
            .find(|row| row.kind == TimelineRowKind::ElicitationRequest)
            .expect("the request has a row")
            .pending_permission
    });
    assert!(awaiting, "an unanswered request is pending");
    assert!(
        app.read_with(cx, |app, _| app.question_form_open()),
        "and it is what the panel is asking for"
    );

    // The reader answers. The resolution arrives as its own live item on the
    // same session, which is how the phone learns a form is done.
    let resolution = vibex_core::ElicitationResolution {
        request_id: request.id.clone(),
        session_id: request.session_id.clone(),
        action: ElicitationResolutionAction::Accept,
        answers: BTreeMap::from([(
            "approach".into(),
            vibex_core::ElicitationAnswerValue::String("global".into()),
        )]),
        responder_device_id: None,
        resolved_at_ms: 2,
    };
    cx.update(|_, cx| {
        app.update(cx, |app, cx| {
            // The device learns a form is done from the event stream, so the
            // test takes the same door rather than poking the controller.
            assert!(
                !app.apply_event_batch(
                    vec![BackendEvent::Timeline(TimelineLiveEvent {
                        session_id: request.session_id.clone(),
                        sequence: 3,
                        item: item(
                            3,
                            TimelinePayload::ElicitationResolution(resolution.clone()),
                        ),
                    })],
                    cx,
                ),
                "a settle needs no authoritative refetch"
            );
        });
    });
    draw(cx);

    // Answering is what closes the panel: the request is no longer waiting.
    assert!(
        !app.read_with(cx, |app, _| app.question_form_open()),
        "the panel is gone once the form is submitted"
    );
    // The timeline row is the question, still in place.
    let row = app.read_with(cx, |app, _| {
        app.timeline_turns
            .iter()
            .flat_map(|turn| turn.process_rows.iter())
            .find(|row| row.kind == TimelineRowKind::ElicitationRequest)
            .expect("the request keeps its row")
            .clone()
    });
    let (row_id, pending) = (row.id.clone(), row.pending_permission);
    assert!(
        !pending,
        "a resolution in the timeline settles the row it answers"
    );

    let header = format!("settled-question:{row_id}");
    let record = cx.debug_bounds(header.clone().leak()).is_some();
    assert!(
        record,
        "an answered request is a record on the timeline, not a form"
    );
    // The collapsed record says which answer was given; the per-field body
    // that spells it out stays behind the disclosure until it is asked for.
    let answer_selector = "settled-answer:approach";
    assert!(
        cx.debug_bounds(answer_selector).is_none(),
        "the answer body is folded away until it is opened"
    );

    // Opening it does not take the record away; it adds the per-field detail.
    click(cx, &header);
    assert!(
        cx.debug_bounds(header.clone().leak()).is_some(),
        "the record stays where it was"
    );
    let expanded = app.read_with(cx, |app, _| {
        app.expanded_settled_questions.contains(&row_id)
    });
    assert!(expanded, "one tap opens the record");
    // Opening it is what puts the reader's own answer on screen.
    let answer = cx
        .debug_bounds(answer_selector)
        .expect("the opened record shows the answer that was given");
    let header_bounds = cx.debug_bounds(header.clone().leak()).expect("the record");
    assert!(
        answer.top() >= header_bounds.top(),
        "the answer reads under the question it answered"
    );
    click(cx, &header);
    assert!(
        cx.debug_bounds(answer_selector).is_none(),
        "and closing it folds the answer away again"
    );
}

/// A question the reader answered stays on the timeline after the turn ends.
///
/// The turn folds its process activity away when it completes, and the record
/// of a question is part of that list. If the fold takes the record with it, the
/// reader loses the answer they gave — which is the thing they were told they
/// could come back and read.
#[gpui::test]
fn a_settled_question_survives_the_end_of_its_turn(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let request_id = RequestId::parse("request_mobile_endturn").unwrap();
    let session_id = VibexSessionId::parse("session_mobile_timeline").unwrap();
    let request = vibex_core::ElicitationRequest {
        id: request_id.clone(),
        session_id: session_id.clone(),
        provider_request_id: None,
        tool_call_id: None,
        message: "Which state model should the rail use?".into(),
        title: None,
        description: None,
        fields: vec![vibex_core::ElicitationField {
            id: "approach".into(),
            title: "Approach".into(),
            description: None,
            required: true,
            kind: ElicitationFieldKind::Text {
                min_length: None,
                max_length: None,
                pattern: None,
                format: None,
                default: None,
                options: vec![vibex_core::ElicitationOption {
                    value: "global".into(),
                    title: "Global state".into(),
                    description: None,
                }],
            },
        }],
        status: vibex_core::ElicitationRequestStatus::Pending,
        requested_at_ms: 1,
    };
    let (_app, _probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![
            TimelinePayload::ElicitationRequest(request.clone()),
            TimelinePayload::ElicitationResolution(vibex_core::ElicitationResolution {
                request_id: request.id.clone(),
                session_id: session_id.clone(),
                action: ElicitationResolutionAction::Accept,
                answers: BTreeMap::from([(
                    "approach".into(),
                    vibex_core::ElicitationAnswerValue::String("global".into()),
                )]),
                responder_device_id: None,
                resolved_at_ms: 2,
            }),
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "The rail keeps global state.".into(),
                is_final: true,
            }),
        ],
    );

    // The turn is over, so its process activity folds away.
    let row_id = _app.read_with(cx, |app, _| {
        let turn = app.timeline_turns.last().expect("a turn");
        assert!(turn.complete, "the final answer completes the turn");
        turn.process_rows
            .iter()
            .find(|row| row.kind == TimelineRowKind::ElicitationRequest)
            .map(|row| row.id.clone())
            .expect("the question keeps a row after the turn ends")
    });
    let header = format!("settled-question:{row_id}");

    // The reader must still be able to find and open what they answered. The
    // fold takes the turn's steps, not the record: a question is not activity,
    // so it is on screen whether the turn is folded or open.
    let turn_id = _app.read_with(cx, |app, _| app.timeline_turns.last().unwrap().id.clone());
    assert!(
        cx.debug_bounds(header.clone().leak()).is_some(),
        "the answered question stays on screen once its turn completes"
    );
    click(cx, &format!("turn-process:{turn_id}"));
    assert!(
        cx.debug_bounds(header.clone().leak()).is_some(),
        "and opening the turn's activity keeps it in view"
    );
}

/// The answer is set apart by air, not by a rail down its left edge.
///
/// The rule was drawn the full height of the message beside every wrapped line,
/// which made the answer read as a quotation of the process above it instead of
/// the point of the turn.
#[test]
fn the_turn_answer_carries_no_left_rail() {
    let source = include_str!("app.rs");
    let (_, tail) = source
        .split_once("fn render_turn(")
        .expect("the turn renderer should stay inspectable");
    let body = &tail[..tail.find("\n    fn ").unwrap_or(tail.len())];
    assert!(
        !body.contains("border_l_"),
        "the conclusion must not be drawn against a rule"
    );
    assert!(
        body.contains("conclusion_row"),
        "the conclusion is still its own block in the turn"
    );
}

/// The turn header reads `logo · Worked for 20s · ▸`, as the desktop's does.
///
/// The Agent's mark leads the line rather than trailing it: it answers "who did
/// this?" before the reader reads how long it took, and it is what the desktop
/// paints at the start of the same header.
#[test]
fn the_turn_header_leads_with_the_agent_mark() {
    let source = include_str!("timeline.rs");
    let (_, tail) = source
        .split_once("fn render_turn_process_header(")
        .expect("the turn header should stay inspectable");
    let body = &tail[..tail.find("\n    pub(super) fn ").unwrap_or(tail.len())];

    let logo = body
        .find("turn-process-agent:")
        .expect("the header should carry the Agent's mark");
    let label = body
        .find(".child(label)")
        .expect("the header should carry the duration label");
    let chevron = body
        .find("disclosure_icon(expanded)")
        .expect("the header should carry its disclosure chevron");

    assert!(
        logo < label,
        "the mark leads the label: the reader learns who ran the turn first"
    );
    assert!(
        label < chevron,
        "the chevron closes the line, the way the desktop's does"
    );

    // The kit's button centres its content row, so the run of marks has to take
    // the full width and justify to the start to sit on the leading edge.
    assert!(
        body.contains(".w_full()") && body.contains("justify_start()"),
        "the header starts at the row's leading edge, not in its middle"
    );
}

/// Opening a row is the reader taking charge of the screen.
///
/// The list follows the live end while the Agent works. If a tap on a row left
/// that follow in place, the row's new height would drag the list back down and
/// lift the row the reader just opened out of view.
#[gpui::test]
fn opening_a_row_stops_the_list_following_the_live_end(cx: &mut TestAppContext) {
    let data_dir = tempfile::tempdir().unwrap();
    let (app, _probe, cx) = open_timeline(
        cx,
        data_dir.path(),
        vec![read(
            "src/a-long-file.rs",
            ToolCallStatus::Completed,
            &"first line\n  indented line\n".repeat(120),
        )],
    );
    let row_id = app.read_with(cx, |app, _| {
        app.timeline_turns[0].process_rows[0].id.clone()
    });
    assert!(
        app.read_with(cx, |app, _| app.timeline_list.is_following_tail()),
        "the fixture starts following the live end"
    );
    let header = format!("activity-header:{row_id}");
    let before = cx
        .debug_bounds(header.clone().leak())
        .expect("the row's header is on screen")
        .top();
    app.update(cx, |app, cx| app.set_row_expanded(row_id.clone(), true, cx));
    draw(cx);
    let follow = app.read_with(cx, |app, _| app.timeline_list.is_following_tail());
    assert!(!follow, "opening a row hands the screen to the reader");
    let after = cx
        .debug_bounds(header.clone().leak())
        .expect("the row's header is still on screen")
        .top();
    assert!(
        (f32::from(after) - f32::from(before)).abs() <= 2.0,
        "and the row stays where the reader found it: {before:?} then {after:?}"
    );
}

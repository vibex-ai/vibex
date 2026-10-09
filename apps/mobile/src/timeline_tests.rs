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
        let content = self.app.update(cx, |app, cx| {
            app.render_turn(&turn, true, true, window, cx)
                .into_any_element()
        });
        div()
            .debug_selector(|| "timeline-probe".into())
            .w(px(self.width))
            .min_w_0()
            .tab_group()
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
            let mut app = MobileApp::new(data_dir.to_path_buf(), window, cx);
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
    let window = cx.debug_bounds(window_selector.clone().leak()).unwrap();
    let content = cx.debug_bounds(body_selector.clone().leak()).unwrap();
    assert!(window.size.height <= px(15.0 * markdown::LINE_HEIGHT_REM * 6.0 + 1.0));
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

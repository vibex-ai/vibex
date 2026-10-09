//! Desktop activity semantics through the real TUI projection and renderer.

use ratatui::{Terminal, backend::TestBackend};
use unicode_width::UnicodeWidthStr as _;
use vibex_backend::DisconnectedBackend;
use vibex_core::*;
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::transcript::GroupRole;
use vibex_tui::{
    Locale, SeatKind,
    action::Intent,
    app::{App, AppOptions, Page},
};

fn app(payloads: Vec<TimelinePayload>, state: AgentSessionState) -> App {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Remote,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
            locale: Locale::En,
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    let session = AgentSession {
        id: VibexSessionId::new(),
        title: "Review changes".into(),
        project_id: ProjectId::new(),
        workspace_id: WorkspaceId::new(),
        workspace_root: "/workspace".into(),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        agent_id: AgentId::parse("codex").unwrap(),
        state,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: 1,
        updated_at_ms: 1,
        last_message_at_ms: 1,
        archived_at_ms: None,
        deleted_at_ms: None,
    };
    let id = session.id.clone();
    app.agent.state.selected_session_id = Some(id.clone());
    app.agent.state.active_session.resolve(session);
    let payloads = std::iter::once(TimelinePayload::UserMessage(UserMessagePayload {
        text: "Review the implementation".into(),
        ..Default::default()
    }))
    .chain(payloads);
    let items = payloads
        .enumerate()
        .map(|(ix, payload)| TimelineItem {
            id: TimelineItemId::new(),
            session_id: id.clone(),
            sequence: ix as i64 + 1,
            timestamp_ms: 0,
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        })
        .collect::<Vec<_>>();
    app.agent.state.timeline.replace_authoritative(id, items);
    app.navigate_to(Page::Agent);
    app.sync_transcript();
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> String {
    app.resize(width, height);
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .unwrap();
    let buffer = terminal.backend().buffer();
    (0..height)
        .map(|y| {
            let mut line = String::new();
            let mut x = 0;
            while x < width {
                let symbol = buffer[(x, y)].symbol();
                line.push_str(symbol);
                x += symbol.width().max(1) as u16;
            }
            line
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn command(command: &str, status: CommandStatus) -> TimelinePayload {
    TimelinePayload::Command(CommandPayload {
        command: command.into(),
        cwd: Some("/workspace".into()),
        status,
        exit_code: None,
        output_summary: None,
        raw_extension: None,
    })
}

fn file(path: &str, operation: FileOperationKind) -> TimelinePayload {
    TimelinePayload::FileOperation(FileOperationPayload {
        operation,
        path: path.into(),
        summary: String::new(),
        old_text: None,
        new_text: None,
        patch: None,
        raw_extension: None,
    })
}

#[test]
fn typed_tool_details_keep_the_full_input_and_refresh_captured_output() {
    let input = r#"{"path":"src/nested/main.rs","limit":40}"#;
    let extension = AgentEventRawExtension::new(
        Vec::new(),
        Some(input.into()),
        Some(
            AgentEventRawOutput::new(
                AgentEventRawOutputMode::Snapshot,
                r#"{"formatted_output":"first line\n  indented line\n","exit_code":0}"#,
            )
            .0,
        ),
        vec![AgentEventLocation::new("src/nested/main.rs", None, None).0],
        Default::default(),
        false,
    );
    let tool = TimelinePayload::ToolCall(ToolCallPayload {
        tool_call_id: "read-1".into(),
        tool_name: "read_file".into(),
        status: ToolCallStatus::Completed,
        summary: "Read src/nested/main.rs".into(),
        input_summary: None,
        output_summary: Some("terminal output".into()),
        raw_extension: Some(extension),
    });
    let mut app = app(vec![tool], AgentSessionState::Idle);
    let compact = draw(&mut app, 80, 30);
    assert!(
        compact.contains("Read") && compact.contains("main.rs"),
        "{compact}"
    );
    assert!(!compact.contains("read_file") && !compact.contains("terminal output"));
    app.set_selection(vibex_tui::keymap::Scope::Agent, 1);
    app.perform(Intent::ToggleBlockExpanded);
    for width in [40, 80, 120] {
        let expanded = draw(&mut app, width, 40);
        assert!(expanded.contains("first line"), "{expanded}");
        assert!(expanded.contains("indented line"), "{expanded}");
        assert!(expanded.contains("limit: 40"), "{expanded}");
        assert!(!expanded.contains("formatted_output"));
    }
    assert_eq!(app.transcript.blocks()[1].details[0].copy_text(), input);
    let TimelinePayload::ToolCall(tool) = &mut app.agent.state.timeline.items[1].payload else {
        panic!("tool")
    };
    tool.raw_extension.as_mut().unwrap().raw_output =
        Some(AgentEventRawOutput::new(AgentEventRawOutputMode::Snapshot, "corrected result").0);
    // Same sequence and unchanged summary: the detail source itself invalidates
    // the cached block, while the reader's expansion survives.
    app.sync_transcript();
    let corrected = draw(&mut app, 80, 30);
    assert!(app.transcript.blocks()[1].expanded);
    assert!(corrected.contains("corrected result"), "{corrected}");
    assert!(!corrected.contains("first line"));
    assert_eq!(app.transcript.search("corrected result"), vec![1]);
    assert!(app.transcript.search("first line").is_empty());
    let pattern = vibex_tui::search::SearchPattern::compile("corrected result").unwrap();
    assert_eq!(app.transcript.search_blocks(&pattern), (vec![1], 1));
}

#[test]
fn mixed_activity_runs_count_distinct_paths_and_keep_live_failures_visible() {
    let mut app = app(
        vec![
            file("Cargo.toml", FileOperationKind::Read),
            file("src/main.rs", FileOperationKind::Edit),
            file("tests/main.rs", FileOperationKind::Edit),
            command("cargo test", CommandStatus::Started),
            command("cargo check", CommandStatus::Failed),
        ],
        AgentSessionState::Running,
    );
    let text = draw(&mut app, 100, 30);
    assert!(text.contains("Read 1 file · Changed 2 files"), "{text}");
    assert!(
        text.contains("cargo test") && text.contains("cargo check"),
        "{text}"
    );
    assert_eq!(
        app.transcript.blocks()[1].group,
        GroupRole::Head { hidden: 2 }
    );
    assert_eq!(app.transcript.blocks()[4].group, GroupRole::Solo);
    assert_eq!(app.transcript.blocks()[5].group, GroupRole::Solo);
    assert!(app.transcript.blocks()[5].failed);
    app.set_selection(vibex_tui::keymap::Scope::Agent, 1);
    app.perform(Intent::ToggleBlockExpanded);
    assert!(
        app.transcript.blocks()[1..4]
            .iter()
            .all(|block| block.expanded)
    );
    app.sync_transcript();
    assert!(
        app.transcript.blocks()[1..4]
            .iter()
            .all(|block| block.expanded)
    );
    let text = draw(&mut app, 100, 40);
    assert!(
        text.contains("src/main.rs") && text.contains("tests/main.rs"),
        "{text}"
    );
}

#[test]
fn reasoning_keeps_its_live_tail_window_and_settles_when_a_tool_follows() {
    let thought = (0..20)
        .map(|ix| format!("Reasoning paragraph {ix}"))
        .collect::<Vec<_>>()
        .join("\n\n");
    let mut app = app(
        vec![TimelinePayload::Reasoning(ReasoningPayload {
            text: thought,
            is_final: false,
        })],
        AgentSessionState::Running,
    );
    let text = draw(&mut app, 80, 30);
    assert!(text.contains("Reasoning paragraph 19"), "{text}");
    assert!(!text.contains("Reasoning paragraph 0"));
    let TimelinePayload::Reasoning(reasoning) = &mut app.agent.state.timeline.items[1].payload
    else {
        panic!("thought")
    };
    reasoning.text.push_str("\n\nLatest reasoning 中文");
    app.sync_transcript();
    let text = draw(&mut app, 80, 30);
    assert!(text.contains("Latest reasoning 中文"), "{text}");
    assert!(!text.contains("Reasoning paragraph 0"));
    let mut item = app.agent.state.timeline.items[1].clone();
    item.id = TimelineItemId::new();
    item.sequence += 1;
    item.payload = command("cargo check", CommandStatus::Started);
    item.kind = item.payload.kind();
    app.agent.state.timeline.items.push(item);
    app.sync_transcript();
    let text = draw(&mut app, 80, 30);
    assert!(text.contains("cargo check"), "{text}");
    assert!(
        !text.contains("Latest reasoning 中文"),
        "the previous thought must fold: {text}"
    );
}

#[test]
fn collaboration_keeps_its_target_in_the_collapsed_activity_row() {
    let mut app = app(
        vec![TimelinePayload::Collaboration(CollaborationPayload {
            action: "delegate".into(),
            status: ToolCallStatus::Started,
            summary: "Review the parser".into(),
            agent_label: Some("Reviewer".into()),
            delegation_id: None,
            child_session_id: None,
            raw_extension: None,
        })],
        AgentSessionState::Running,
    );
    let screen = draw(&mut app, 80, 30);
    assert!(screen.contains("Agent  Review the parser"), "{screen}");
    let block = &app.transcript.blocks()[1];
    assert!(!block.expanded);
    assert_eq!(block.group, GroupRole::Solo);
    assert!(vibex_tui::transcript::is_dense_row(block.kind));
}

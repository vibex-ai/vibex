use super::*;
use gpui::{KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, TestAppContext, VisualTestContext};
use std::cell::Cell;
use vibex_core::{
    AgentMessagePayload, CommandPayload, CommandStatus, FileOperationPayload, ToolCallPayload,
    ToolCallStatus,
};

fn item(sequence: i64, payload: TimelinePayload) -> TimelineItem {
    TimelineItem {
        id: TimelineItemId::parse(format!("timeline_activity-{sequence}")).unwrap(),
        session_id: VibexSessionId::parse("session_activity").unwrap(),
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

fn command(text: &str) -> TimelinePayload {
    TimelinePayload::Command(CommandPayload {
        command: text.into(),
        cwd: None,
        status: CommandStatus::Completed,
        exit_code: Some(0),
        output_summary: None,
        raw_extension: None,
    })
}

fn file(operation: FileOperationKind, path: &str) -> TimelinePayload {
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

fn row_for(payload: &TimelinePayload) -> TimelineRow {
    vibex_desktop_model::timeline_rows(&[item(1, payload.clone())]).remove(0)
}

fn running_turn() -> TimelineConversationTurn {
    vibex_desktop_model::timeline_conversation_turns(
        &[
            item(
                1,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Inspect the project".into(),
                    ..Default::default()
                }),
            ),
            item(2, file(FileOperationKind::Read, "Cargo.toml")),
            item(3, file(FileOperationKind::Read, "src/lib.rs")),
            item(4, command("cargo check")),
        ],
        Some(AgentSessionState::Running),
        true,
    )
    .remove(0)
}

#[test]
fn activity_labels_are_single_line_without_changing_the_invocation() {
    let source = "printf '%s\\n' 'two words'\n  cargo check\t--locked";
    let payload = command(source);
    let row = row_for(&payload);
    let projection = tool_card_projection(&row, Some(&payload));
    assert_eq!(
        projection.activity.target(),
        "printf '%s\\n' 'two words' cargo check --locked"
    );
    let ToolCardDetailBlock::Terminal { command, .. } = &projection.details[0] else {
        panic!("command detail")
    };
    assert_eq!(command, source);

    let file = file(FileOperationKind::Read, "src/a directory/main.rs");
    let activity = project(&row_for(&file), Some(&file));
    assert_eq!(activity.path(), Some("src/a directory/main.rs"));
    assert_eq!(activity.target(), "main.rs");
    assert!(activity.label().ends_with("src/a directory/main.rs"));
}

#[test]
fn activity_file_labels_keep_their_names_and_complete_paths() {
    for (path, name) in [
        ("src/a directory/main.rs", "main.rs"),
        (r"C:\work\资料\main.rs", "main.rs"),
        (r"\\server\share\README.md", "README.md"),
        (
            "file:///workspace/notes%20and%20plans/%E6%A3%80%E6%9F%A5.md",
            "检查.md",
        ),
        (
            "file:///C:/workspace/notes%20and%20plans.md",
            "notes and plans.md",
        ),
        (
            "file://server/share/notes%20and%20plans.md",
            "notes and plans.md",
        ),
        (
            "file:///workspace/review+notes%23next.md",
            "review+notes#next.md",
        ),
        ("config/.env.local", ".env.local"),
        ("src/nested/", "nested"),
        ("/", "/"),
    ] {
        let payload = file(FileOperationKind::Read, path);
        let activity = project(&row_for(&payload), Some(&payload));
        assert_eq!(activity.target(), name);
        assert_eq!(activity.path(), Some(path));
        assert!(activity.label().ends_with(path));
    }

    let mut summary = ActivitySummary::default();
    for path in ["src/main.rs", "tests/main.rs"] {
        let payload = file(FileOperationKind::Edit, path);
        let activity = project(&row_for(&payload), Some(&payload));
        assert_eq!(activity.target(), "main.rs");
        summary.record(&activity, path);
    }
    assert_eq!(
        summary.label(vibex_ui::locale::Locale::En),
        "Changed 2 files",
        "short labels must not merge distinct files"
    );
}

#[test]
fn activity_details_prefer_the_complete_bounded_invocation() {
    let invocation = "{\"path\":\"src/main.rs\",\"offset\":10,\"limit\":40}";
    let payload = TimelinePayload::ToolCall(ToolCallPayload {
        tool_call_id: "read-1".into(),
        tool_name: "read_file".into(),
        status: ToolCallStatus::Completed,
        summary: "Read src/main.rs".into(),
        input_summary: Some("src/main.rs".into()),
        output_summary: None,
        raw_extension: Some(vibex_core::AgentEventRawExtension::new(
            Vec::new(),
            Some(invocation.into()),
            None,
            Vec::new(),
            BTreeMap::new(),
            false,
        )),
    });
    let projection = tool_card_projection(&row_for(&payload), Some(&payload));
    assert_eq!(projection.activity.target(), "src/main.rs");
    let ToolCardDetailBlock::Invocation { value, raw } = &projection.details[0] else {
        panic!("invocation detail")
    };
    assert!(value.starts_with("src/main.rs\n"));
    assert!(value.contains("offset: 10"));
    assert!(value.contains("limit: 40"));
    assert_eq!(raw, invocation);

    let mut empty = payload;
    let TimelinePayload::ToolCall(tool) = &mut empty else {
        unreachable!()
    };
    tool.input_summary = None;
    tool.raw_extension = None;
    assert!(
        !tool_card_projection(&row_for(&empty), Some(&empty))
            .details
            .is_empty(),
        "a call remains inspectable before output arrives"
    );
}

#[test]
fn activity_read_details_show_the_full_target_and_decoded_output() {
    let text = "fn main() {\n    run();\n}\n";
    let extension = vibex_core::AgentEventRawExtension::new(
        Vec::new(),
        None,
        Some(
            vibex_core::AgentEventRawOutput::new(
                vibex_core::AgentEventRawOutputMode::Snapshot,
                serde_json::json!({"formatted_output": text, "exit_code": 0}).to_string(),
            )
            .0,
        ),
        vec![vibex_core::AgentEventLocation::new("/workspace/src/main.rs", None, None).0],
        BTreeMap::new(),
        false,
    );
    let payload = TimelinePayload::ToolCall(ToolCallPayload {
        tool_call_id: "read-1".into(),
        tool_name: "read_file".into(),
        status: ToolCallStatus::Completed,
        summary: "Read main.rs".into(),
        input_summary: None,
        output_summary: Some("short result".into()),
        raw_extension: Some(extension),
    });
    let projection = tool_card_projection(&row_for(&payload), Some(&payload));
    assert_eq!(projection.details.len(), 2);
    assert_eq!(
        projection.details[0],
        ToolCardDetailBlock::Invocation {
            value: "/workspace/src/main.rs".into(),
            raw: "/workspace/src/main.rs".into(),
        }
    );
    let ToolCardDetailBlock::Mono { value, .. } = &projection.details[1] else {
        panic!("read result")
    };
    assert_eq!(value, text);
}

#[test]
fn activity_commands_use_captured_output_and_only_estimate_visible_metadata() {
    let mut payload = command("cargo check");
    let TimelinePayload::Command(command) = &mut payload else {
        unreachable!()
    };
    command.cwd = Some("/workspace".into());
    command.output_summary = Some("terminal output".into());
    command.raw_extension = Some(vibex_core::AgentEventRawExtension::new(
        Vec::new(),
        None,
        Some(
            vibex_core::AgentEventRawOutput::new(
                vibex_core::AgentEventRawOutputMode::Snapshot,
                "checking\nfinished\n",
            )
            .0,
        ),
        Vec::new(),
        BTreeMap::new(),
        false,
    ));
    let projection = tool_card_projection(&row_for(&payload), Some(&payload));
    let ToolCardDetailBlock::Terminal { output, .. } = &projection.details[0] else {
        panic!("command result")
    };
    assert_eq!(output.as_deref(), Some("checking\nfinished\n"));
    let block = &projection.details[0];
    let height = block.estimated_activity_height(80, 16.0, 14.0, Some("/workspace"));
    assert!(block.estimated_activity_height(80, 16.0, 14.0, None) > height);
    let mut failed = block.clone();
    let ToolCardDetailBlock::Terminal { exit_code, .. } = &mut failed else {
        unreachable!()
    };
    *exit_code = Some(1);
    assert!(failed.estimated_activity_height(80, 16.0, 14.0, Some("/workspace")) > height);
}

#[test]
fn activity_summary_uses_tool_semantics_and_counts_each_changed_file_once() {
    let mut summary = ActivitySummary::default();
    for (index, payload) in [
        file(FileOperationKind::Write, "src/main.rs"),
        file(FileOperationKind::Edit, "src/main.rs"),
        TimelinePayload::ToolCall(ToolCallPayload {
            tool_call_id: "read-1".into(),
            tool_name: "read_file".into(),
            status: ToolCallStatus::Failed,
            summary: "Read missing.rs".into(),
            input_summary: None,
            output_summary: None,
            raw_extension: None,
        }),
        command("cargo check"),
    ]
    .iter()
    .enumerate()
    {
        let row = row_for(payload);
        summary.record(&project(&row, Some(payload)), &index.to_string());
    }
    assert_eq!(
        summary.label(vibex_ui::locale::Locale::En),
        "Ran 1 command · Read 1 file · Changed 1 file · 1 failed",
        "semantic counts and typed failures survive a successful turn"
    );
    assert!(
        summary
            .label(current_locale())
            .contains(&locale::text("{n} failed", "{n} 项失败", "{n} 項失敗").replace("{n}", "1"))
    );
}

#[test]
fn activity_images_preserve_the_typed_failure_and_description() {
    let payload = TimelinePayload::ImageGeneration(vibex_core::ImageGenerationPayload {
        status: ToolCallStatus::Failed,
        summary: "The image could not be generated".into(),
        mime_type: None,
        image_reference: None,
        raw_extension: None,
    });
    let row = row_for(&payload);
    let projection = tool_card_projection(&row, Some(&payload));
    assert!(projection.activity.is_failed());
    assert_eq!(
        projection.activity.target(),
        "The image could not be generated"
    );
    let mut summary = ActivitySummary::default();
    summary.record(&projection.activity, &row.id);
    assert!(
        summary
            .label(vibex_ui::locale::Locale::En)
            .contains("1 failed")
    );
}

#[test]
fn activity_detail_estimates_follow_zoom_and_bound_long_output() {
    let section = |value: String| ToolCardDetailBlock::Mono {
        label: "Output".into(),
        value,
    };
    let short = section("first line\nsecond line".into());
    let normal = short.estimated_activity_height(80, 16.0, 14.0, None);
    assert_eq!(
        short.estimated_activity_height(80, 32.0, 28.0, None),
        normal * 2.0,
        "interface zoom must scale the header, spacing and text together"
    );
    assert!(short.estimated_activity_height(80, 16.0, 18.0, None) > normal);
    assert!(short.estimated_activity_height(80, 20.0, 14.0, None) >= normal);

    let long = section("output line\n".repeat(100));
    let longer = section("output line\n".repeat(1000));
    for rem in [12.0, 16.0, 20.0] {
        let height = long.estimated_activity_height(80, rem, 14.0, None);
        assert_eq!(
            height,
            longer.estimated_activity_height(80, rem, 14.0, None)
        );
        assert!(
            height < rem * 12.0,
            "large output stays inside a bounded body"
        );
    }
}

#[test]
fn activity_groups_follow_execution_until_the_reader_overrides_them() {
    let mut turn = running_turn();
    let group = timeline_process_activity_groups_for_display(&turn, false)[0].clone();
    assert_eq!(
        group.start_row..group.end_row,
        0..3,
        "file details remain inside the command's activity run"
    );
    let enhanced_groups = timeline_process_activity_groups_for_display(&turn, true);
    assert_eq!(enhanced_groups.len(), 1);
    assert_eq!(
        enhanced_groups[0].start_row..enhanced_groups[0].end_row,
        0..2,
        "enhanced commands keep their standalone card while files stay grouped"
    );
    assert!(group_open(&turn, &group, None));
    assert!(!group_open(&turn, &group, Some(false)));
    turn.process_rows
        .push(row_for(&TimelinePayload::AgentMessage(
            AgentMessagePayload {
                text: "The check passed.".into(),
                is_final: false,
            },
        )));
    assert!(!group_open(&turn, &group, None));
    assert!(group_open(&turn, &group, Some(true)));
    turn.process_rows[0].streaming = true;
    assert!(
        group_open(&turn, &group, None),
        "a parallel operation must remain visible after commentary"
    );
    turn.process_rows[0].streaming = false;
    turn.process_rows[0].pending_permission = true;
    assert!(group_open(&turn, &group, None));
    turn.conclusion_row = Some(row_for(&TimelinePayload::AgentMessage(
        AgentMessagePayload {
            text: "Done".into(),
            is_final: true,
        },
    )));
    assert!(!group_open(&turn, &group, None));
    turn.conclusion_row = None;
    turn.complete = true;
    assert!(!group_open(&turn, &group, None));
    assert!(group_open(&turn, &group, Some(true)));
    turn.complete = false;
    turn.superseded = true;
    assert!(!group_open(&turn, &group, None));
    assert!(group_open(&turn, &group, Some(true)));
}

#[test]
fn activity_running_state_requires_an_active_turn_and_operation() {
    let mut payload = command("cargo check");
    let TimelinePayload::Command(invocation) = &mut payload else {
        unreachable!()
    };
    invocation.status = CommandStatus::Started;
    invocation.exit_code = None;
    let mut row = row_for(&payload);
    assert!(is_running(&row, true));
    assert!(
        !is_running(&row, false),
        "interrupted rows cannot keep animating"
    );
    row.pending_permission = true;
    assert!(!is_running(&row, true));
    row.pending_permission = false;
    row.failed = true;
    assert!(!is_running(&row, true));
    assert!(!is_running(&row_for(&command("cargo check")), true));
    assert_eq!(
        generic_tool_activity_icon("", "Interact with subagent reviewer"),
        ProcessActivityIcon::Collaboration
    );
}

struct ActivityProbe {
    activity: Activity,
    expanded: bool,
    group_expanded: bool,
    running: bool,
    rem: f32,
    width: f32,
    target_color: Option<Rc<Cell<Hsla>>>,
}

fn press(cx: &mut VisualTestContext, key: &str) {
    let keystroke = Keystroke::parse(key).unwrap();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
    cx.update(|window, cx| {
        let _ = window.draw(cx);
    });
}

impl Render for ActivityProbe {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        window.set_rem_size(px(self.rem));
        let target = self.target_color.clone().map(|color| {
            div()
                .child(self.activity.target().to_string())
                .child(
                    gpui::canvas(
                        move |_, window, _| color.set(window.text_style().color),
                        |_, _, _, _| {},
                    )
                    .size(px(0.0)),
                )
                .into_any_element()
        });
        let first = header(
            "first-header".into(),
            &self.activity,
            self.running,
            Some(self.expanded),
            target,
            None,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.expanded = !this.expanded;
            cx.notify();
        }));
        let body = self.expanded.then(|| {
            div()
                .debug_selector(|| "activity-body".into())
                .w_full()
                .min_w_0()
                .child(detail(
                    "test-output".into(),
                    "Output",
                    "first line\n    indented line\nlast line",
                    "first line\n    indented line\nlast line",
                    cx,
                ))
                .into_any_element()
        });
        let first = row(
            "first",
            self.activity.icon(),
            self.activity.is_failed(),
            Some(true),
            div()
                .debug_selector(|| "first-header".into())
                .child(first)
                .into_any_element(),
            body,
            cx,
        );
        let second = row(
            "second",
            self.activity.icon(),
            false,
            Some(false),
            header(
                "second-header".into(),
                &self.activity,
                false,
                Some(false),
                None,
                None,
                cx,
            )
            .into_any_element(),
            None,
            cx,
        );
        let summary = summary_header(
            "summary".into(),
            "Read 2 files".into(),
            self.group_expanded,
            self.running,
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            this.group_expanded = !this.group_expanded;
            cx.notify();
        }));
        v_flex()
            .w(px(self.width))
            .tab_group()
            .child(div().debug_selector(|| "summary".into()).child(summary))
            .when(self.group_expanded, |this| {
                this.child(div().debug_selector(|| "first-row".into()).child(first))
                    .child(div().debug_selector(|| "second-row".into()).child(second))
            })
    }
}

#[gpui::test]
fn activity_disclosures_keep_their_columns_and_keyboard_behavior_at_different_sizes(
    cx: &mut TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let payload = file(
        FileOperationKind::Read,
        "src/a-long-directory-name/a-very-long-component-name-that-must-stay-inside-the-activity-header.rs",
    );
    let activity = project(&row_for(&payload), Some(&payload));
    let view = cx.new(|_| ActivityProbe {
        activity,
        expanded: false,
        group_expanded: true,
        running: true,
        rem: 16.0,
        width: 400.0,
        target_color: None,
    });
    let (_, cx) =
        cx.add_window_view(|window, cx| gpui_component::Root::new(view.clone(), window, cx));
    for (rem, width, scale) in [(12.0, 260.0, 1.0), (16.0, 400.0, 1.25), (20.0, 680.0, 2.0)] {
        cx.simulate_scale_factor_change(scale);
        view.update(cx, |view, cx| {
            view.rem = rem;
            view.width = width;
            view.expanded = false;
            view.group_expanded = true;
            cx.notify();
        });
        cx.update(|window, cx| {
            window.blur(cx);
            let _ = window.draw(cx);
        });
        let closed = cx.debug_bounds("first-row").unwrap();
        assert_eq!(closed.size.height, px(rem * ROW_HEIGHT_REM));
        assert_eq!(
            cx.debug_bounds("summary").unwrap().size.height,
            px(rem * SUMMARY_HEIGHT_REM)
        );
        let trigger = cx.debug_bounds("first-header").unwrap();
        let target = cx.debug_bounds("activity-target:first-header").unwrap();
        let disclosure = cx.debug_bounds("activity-disclosure:first-header").unwrap();
        assert!(target.right() <= disclosure.left());
        assert!(disclosure.right() <= trigger.right());
        let icon = cx.debug_bounds("activity-rail-icon:first").unwrap();
        assert!(
            (icon.center().y - trigger.center().y).abs() * scale < px(1.0),
            "the rail icon stays centered within device-pixel rounding"
        );
        cx.simulate_click(trigger.center(), Modifiers::none());
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        assert!(view.read_with(cx, |view, _| view.expanded));
        let body = cx.debug_bounds("activity-body").unwrap();
        let text = cx.debug_bounds("activity-detail-text:test-output").unwrap();
        assert_eq!(body.top(), text.top(), "details start with their content");
        assert_eq!(
            body.size.height, text.size.height,
            "copy shares the content row"
        );
        let header = cx.debug_bounds("first-header").unwrap();
        assert_eq!(body.left(), header.left());
        assert_eq!(header.left() - closed.left(), px(rem * 2.0));
        assert!(body.bottom() <= cx.debug_bounds("second-row").unwrap().top());
        assert!(body.right() <= closed.right());
        assert_eq!(
            (f32::from(
                cx.debug_bounds("activity-rail-after:first")
                    .unwrap()
                    .bottom()
            ) * scale)
                .round(),
            (f32::from(
                cx.debug_bounds("activity-rail-before:second")
                    .unwrap()
                    .top()
            ) * scale)
                .round(),
            "the rail connects consecutive rows at the same device pixel"
        );
        cx.update(|window, cx| {
            window.blur(cx);
            window.focus_next(cx);
            let _ = window.draw(cx);
        });
        press(cx, "space");
        assert!(
            !view.read_with(cx, |view, _| view.group_expanded),
            "a running group's summary stays operable from the keyboard"
        );
        assert!(cx.debug_bounds("first-row").is_none());
        for key in ["enter", "tab", "space"] {
            press(cx, key);
        }
        assert!(
            !view.read_with(cx, |view, _| view.expanded),
            "Space must operate the same disclosure as a click"
        );
        assert_eq!(
            cx.debug_bounds("first-row").unwrap().size.height,
            closed.size.height
        );
        press(cx, "enter");
        assert!(view.read_with(cx, |view, _| view.expanded));
        press(cx, "tab");
        press(cx, "enter");
        assert_eq!(
            cx.read_from_clipboard().and_then(|item| item.text()),
            Some("first line\n    indented line\nlast line".into()),
            "copy preserves the complete output and its indentation"
        );
        assert!(
            view.read_with(cx, |view, _| view.expanded),
            "copying details must not toggle their disclosure"
        );
    }
}

#[gpui::test]
fn activity_hover_brightens_text_without_painting_a_row_background(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        cx.set_reduce_motion(true);
    });
    let payload = file(FileOperationKind::Read, "src/main.rs");
    let color = Rc::new(Cell::new(gpui::transparent_black()));
    let view = cx.new(|_| ActivityProbe {
        activity: project(&row_for(&payload), Some(&payload)),
        expanded: false,
        group_expanded: true,
        running: false,
        rem: 16.0,
        width: 400.0,
        target_color: Some(color.clone()),
    });
    let (_, cx) =
        cx.add_window_view(|window, cx| gpui_component::Root::new(view.clone(), window, cx));
    let surfaces = |cx: &mut VisualTestContext| {
        cx.update(|window, _| {
            window
                .painted_quads()
                .into_iter()
                .map(|quad| (quad.bounds, quad.background))
                .collect::<Vec<_>>()
        })
    };
    for mode in [
        gpui_component::ThemeMode::Dark,
        gpui_component::ThemeMode::Light,
    ] {
        cx.update(|window, cx| {
            gpui_component::Theme::change(mode, Some(window), cx);
            window.simulate_mouse_move(point(px(600.0), px(300.0)), cx);
            let _ = window.draw(cx);
        });
        let (rest, hover, danger) =
            cx.update(|_, cx| (resting_color(cx), cx.theme().foreground, cx.theme().danger));
        assert_eq!(color.get(), rest);
        assert_ne!(rest, hover);
        let before = surfaces(cx);
        let header = cx.debug_bounds("first-header").unwrap();
        cx.update(|window, cx| {
            window.simulate_mouse_move(header.center(), cx);
            let _ = window.draw(cx);
        });
        assert_eq!(color.get(), hover);
        assert_eq!(
            surfaces(cx),
            before,
            "hover changes glyphs, never the row surface"
        );
        let second = cx.debug_bounds("second-row").unwrap();
        cx.update(|window, cx| {
            window.simulate_mouse_move(second.center(), cx);
            let _ = window.draw(cx);
        });
        assert_eq!(color.get(), rest, "hover does not brighten adjacent rows");
        let summary = cx.debug_bounds("summary").unwrap();
        cx.update(|window, cx| {
            window.simulate_mouse_move(summary.center(), cx);
            let _ = window.draw(cx);
        });
        assert_eq!(
            surfaces(cx),
            before,
            "the group summary also stays transparent"
        );

        view.update(cx, |view, cx| {
            let mut failed_row = row_for(&payload);
            failed_row.failed = true;
            view.activity = project(&failed_row, None);
            cx.notify();
        });
        cx.update(|window, cx| {
            window.simulate_mouse_move(header.center(), cx);
            let _ = window.draw(cx);
        });
        assert_eq!(color.get(), danger, "hover preserves failure emphasis");
        view.update(cx, |view, cx| {
            view.activity = project(&row_for(&payload), Some(&payload));
            cx.notify();
        });
    }
}

struct ActivityTextProbe {
    running: bool,
}

impl Render for ActivityTextProbe {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .debug_selector(|| "activity-text".into())
            .w_64()
            .text_xs()
            .text_color(resting_color(cx))
            .child(activity_text(
                "tool-target",
                "cargo check --locked",
                self.running,
            ))
    }
}

#[gpui::test]
fn activity_shimmer_stops_when_settled_or_motion_is_reduced(cx: &mut TestAppContext) {
    cx.update(gpui_component::init);
    let view = cx.new(|_| ActivityTextProbe { running: false });
    let (_, cx) =
        cx.add_window_view(|window, cx| gpui_component::Root::new(view.clone(), window, cx));
    let mut height = None;
    for (running, reduced) in [(false, false), (true, false), (false, false), (true, true)] {
        cx.update(|_, cx| cx.set_reduce_motion(reduced));
        view.update(cx, |view, cx| {
            view.running = running;
            cx.notify();
        });
        let frames = cx.update(|window, cx| {
            window.simulate_next_frame(cx);
            let _ = window.draw(cx);
            window.simulate_next_frame(cx)
        });
        assert_eq!(frames > 0, running && !reduced);
        let current = cx.debug_bounds("activity-text").unwrap().size.height;
        assert_eq!(
            *height.get_or_insert(current),
            current,
            "shimmer cannot change row geometry"
        );
    }
}

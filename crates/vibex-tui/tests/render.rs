//! Render assertions against a `TestBackend`.
//!
//! These tests drive the real `view::render` at real terminal sizes and assert
//! on the resulting buffer text. They are the layer that catches layout
//! regressions — a pane that disappears at 100 columns, an approval card whose
//! actions fall off the bottom of an 80×24 screen — which a state-machine test
//! cannot see.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_tui::app::{App, AppOptions, Page};
use vibex_tui::keymap::Scope;
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::{Locale, SeatKind, Strings};

fn app(columns: u16, rows: u16) -> App {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Remote,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
            theme_id: Some("vibex-dark".to_string()),
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            // No arrangement file: a test must never write into the runner's
            // home directory, and each test wants a clean list.
            sidebar_path: None,
        },
    );
    app.resize(columns, rows);
    app
}

/// Draw one frame and return the terminal buffer, for checks that need cells
/// rather than the text a person would read.
fn render_buffer(app: &mut App, width: u16, height: u16) -> ratatui::buffer::Buffer {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    terminal.backend().buffer().clone()
}

/// The last column of `row` that holds a visible character.
///
/// A wide glyph occupies its own cell and leaves the next one blank, so a row
/// ending in CJK reports one column short of where its content actually ends.
fn last_used_column(buffer: &ratatui::buffer::Buffer, row: u16) -> Option<u16> {
    let width = buffer.area.width;
    (0..width).rev().find(|column| {
        buffer
            .cell((*column, row))
            .is_some_and(|cell| !cell.symbol().trim().is_empty())
    })
}

/// The column the row's content visually ends at, accounting for the blank
/// spacer that follows a wide glyph.
fn content_end_column(buffer: &ratatui::buffer::Buffer, row: u16) -> Option<u16> {
    let used = last_used_column(buffer, row)?;
    let width = buffer.area.width;
    let is_wide = buffer
        .cell((used, row))
        .is_some_and(|cell| vibex_tui::text::display_width(cell.symbol()) == 2);
    Some(if is_wide && used + 1 < width {
        used + 1
    } else {
        used
    })
}

/// The screen as text, one line per row.
fn buffer_lines(buffer: &ratatui::buffer::Buffer, width: u16, height: u16) -> Vec<String> {
    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| {
                    buffer
                        .cell((column, row))
                        .map(|cell| cell.symbol().to_string())
                        .unwrap_or_default()
                })
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Draw one frame and return the screen as text, one line per row.
fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
    buffer_lines(&buffer, width, height)
}

fn text(lines: &[String]) -> String {
    lines.join("\n")
}

#[test]
fn every_supported_size_produces_a_full_frame() {
    // The matrix from the design report: the layout must degrade, not break.
    for (width, height) in [(80u16, 24u16), (100, 30), (120, 40), (200, 50)] {
        let mut app = app(width, height);
        let lines = render(&mut app, width, height);
        assert_eq!(lines.len(), usize::from(height));
        let screen = text(&lines);
        // The product chrome is always present.
        assert!(
            screen.contains("Vibex"),
            "{width}x{height} lost the top bar"
        );
        assert!(
            screen.contains("Sessions"),
            "{width}x{height} lost the navigation"
        );
        assert!(
            screen.contains("Quit") || screen.contains("Commands"),
            "{width}x{height} lost the key bar"
        );
    }
}

#[test]
fn the_welcome_state_tells_the_user_what_to_do() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoSessions);
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("No sessions yet"),
        "an empty session list must say how to start one:\n{screen}"
    );
}

#[test]
fn the_transcript_gets_the_full_width_at_every_size() {
    // The screen is a stack of full-width bands, so no permanent side pane
    // steals columns from the thing being read.
    for (width, height) in [(80u16, 24u16), (120, 40), (200, 50)] {
        let mut app = app(width, height);
        app.perform(vibex_tui::action::Intent::GotoSessions);
        let lines = render(&mut app, width, height);
        // The status band's location label starts at the left padding, and the
        // right-aligned segment group ends at the right padding: both prove the
        // band spans the whole content width.
        let status = &lines[1];
        assert!(
            status.starts_with("  "),
            "{width}x{height}: the status band ignores the outer padding: {status:?}"
        );
        let trimmed = status.trim_end();
        assert!(
            vibex_tui::text::display_width(trimmed) <= usize::from(width),
            "{width}x{height}: a band overflowed"
        );
    }
}

#[test]
fn the_status_band_reports_liveness_and_seat_together() {
    let mut app = app(120, 40);
    let lines = render(&mut app, 120, 40);
    let status = &lines[1];
    // The right-aligned group is joined by a separator, so the reader can tell
    // one segment from the next.
    assert!(
        status.contains('│'),
        "segments are not separated: {status:?}"
    );
    assert!(
        status.contains("Remote") || status.contains("Authority"),
        "the seat must be visible: {status:?}"
    );
    assert!(
        status.contains("Connecting") || status.contains("Disconnected") || status.contains("Done"),
        "the live state must be visible: {status:?}"
    );
}

#[test]
fn the_shortcuts_band_is_the_last_row() {
    let mut app = app(120, 40);
    let lines = render(&mut app, 120, 40);
    // The last row is the outer bottom padding; the band sits above it.
    let band = &lines[lines.len() - 2];
    assert!(
        band.contains("Ctrl+P") || band.contains("Commands"),
        "the shortcut band is not at the bottom: {band:?}\n{}",
        lines.join("\n")
    );
}

#[test]
fn the_prompt_sits_directly_above_the_shortcuts_band() {
    let mut app = app(120, 40);
    // The composer belongs to a session view.
    app.navigate_to(Page::Agent);
    app.composer.set_text("hello");
    let lines = render(&mut app, 120, 40);
    let prompt_row = lines
        .iter()
        .position(|line| line.contains('❯'))
        .expect("the prompt is on screen");
    // Below the prompt: its own bottom border, the status line, the shortcut
    // band, then the outer padding. Nothing else may intrude.
    assert!(
        lines[prompt_row + 1].contains('╰'),
        "the prompt has no bottom border: {:?}",
        lines[prompt_row + 1]
    );
    let band_row = lines
        .iter()
        .position(|line| line.contains("Ctrl+P"))
        .expect("the shortcut band is on screen");
    assert!(
        band_row > prompt_row + 1,
        "the shortcut band is above the prompt:\n{}",
        lines.join("\n")
    );
    assert!(
        band_row >= lines.len() - 3,
        "the shortcut band is not at the bottom:\n{}",
        lines.join("\n")
    );
}

#[test]
fn the_prompt_border_carries_the_context_line() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    let lines = render(&mut app, 120, 40);
    let bottom = lines
        .iter()
        .rev()
        .find(|line| line.contains('╰'))
        .expect("the prompt has a bottom border");
    // The rule continues around the context rather than a bare line of dashes.
    let inner = bottom.trim_matches(|c| c == ' ' || c == '╰' || c == '╯');
    assert!(
        inner.chars().any(|c| c.is_alphanumeric()),
        "the prompt's bottom border carries no context: {bottom:?}"
    );
}

#[test]
fn a_cjk_draft_with_a_collapsed_space_run_renders() {
    // Regression for the reported abort. The renderer places the terminal's
    // caret from an offset in the draft; the composer used to wrap with
    // `wrap_text`, whose rows are rendered text, so a run of spaces before an
    // ideograph shifted every later offset until the slice landed inside a
    // character and the process aborted mid-frame.
    let mut app = app(80, 24);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text("ab  中文");
    // The caret lands between the two ideographs, the offset in the report.
    app.composer.move_to_end();
    app.composer.move_left();
    assert_eq!(app.composer.cursor(), 7);
    // A wide glyph leaves its trailing cell blank and a wrapping may collapse
    // the run of spaces, so the painted row is not the draft byte for byte; the
    // ASCII run and both ideographs are what prove the row was drawn whole —
    // and that the caret could be placed on it.
    let screen = text(&render(&mut app, 80, 24));
    assert!(screen.contains("ab"), "{screen}");
    assert!(screen.contains('中'), "{screen}");
    assert!(screen.contains('文'), "{screen}");

    // The same draft on a narrow terminal wraps, and the caret still has to be
    // placeable.
    app.composer.set_text(
        "颜色太少了，你可以按  markdown 语法来选取不同的强调色，你可以看下 grok-build 的相关实现",
    );
    app.composer.move_to_end();
    for _ in 0..6 {
        app.composer.move_left();
    }
    app.resize(40, 24);
    let screen = text(&render(&mut app, 40, 24));
    assert!(screen.contains('颜'), "{screen}");
    assert!(screen.contains("markdown"), "{screen}");
}

#[test]
fn the_prompt_names_the_runtime_the_session_is_on() {
    // The Agent and model switcher has to be visible from the composer: the
    // line names the session's own runtime — not the first catalogue entry —
    // and the key that moves it.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let option = |agent: &str, model: &str| vibex_core::SessionRuntimeOption {
        selection: vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse(agent).expect("agent id"),
            vibex_core::ProviderProfileId::new(),
            model,
        ),
        agent_label: agent.to_string(),
        auth_source_label: "bal".to_string(),
        model_label: model.to_string(),
        reasoning_efforts: Vec::new(),
        modes: Vec::new(),
        features: Vec::new(),
        availability: vibex_core::RuntimeOptionAvailability::Available,
    };
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![option("claude", "claude-sonnet"), option("codex", "gpt-5")],
    });
    app.agent
        .state
        .active_session
        .resolve(vibex_core::AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: "a session".to_string(),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/tmp/vibex-render-workspace".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse("codex").expect("agent id"),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        });
    // The session is on the second entry, so the second one is what the line
    // has to name.
    let desired = app.runtime_options.as_ref().unwrap().options[1]
        .selection
        .clone();
    app.agent
        .state
        .runtime_selection
        .resolve(vibex_core::AgentSessionRuntimeSelectionState {
            desired: desired.clone(),
            effective: desired,
            status: vibex_core::SessionRuntimeSelectionStatus::Ready,
            session_revision: 1,
            selection_revision: 1,
            current_binding_id: None,
            activation_generation: 1,
            pending_switch_id: None,
            actionable_error: None,
        });

    let lines = render(&mut app, 120, 40);
    let screen = text(&lines);
    assert!(
        lines.iter().any(|line| line.contains("bal/gpt-5")),
        "the prompt does not name the provider and model:\n{screen}"
    );
    assert!(
        lines.iter().any(|line| line.contains("Ctrl+G")),
        "the prompt does not advertise the runtime switch:\n{screen}"
    );
    assert!(
        !screen.contains("bal/claude-sonnet"),
        "the prompt named a catalogue entry the session is not on:\n{screen}"
    );
    // It rides the far end of the bottom border: the reader looks for the
    // runtime beside the box's corner, and the rule on the left is the line the
    // eye follows into the prompt.
    let info_row = lines
        .iter()
        .find(|line| line.contains("bal/gpt-5"))
        .expect("the info line is on screen");
    let start = info_row
        .char_indices()
        .find(|(_, character)| !matches!(character, '─' | ' ' | '│' | '╰' | '╭'))
        .map(|(index, _)| index)
        .expect("the info line has content");
    assert!(
        start > 60,
        "the runtime is still on the left of the box: column {start}\n{screen}"
    );
    assert!(
        info_row[start..].starts_with("codex · bal/gpt-5"),
        "the info line starts with something else: {:?}",
        &info_row[start..]
    );
}

#[test]
fn the_settings_page_lists_theme_language_and_keys() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::OpenSettings);
    let screen = text(&render(&mut app, 120, 40));
    for needle in ["Theme", "Language", "Key bindings", "vibex-dark"] {
        assert!(screen.contains(needle), "settings lost {needle}:\n{screen}");
    }
}

#[test]
fn the_help_page_renders_keys_from_the_binding_table() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ToggleHelp);
    let screen = text(&render(&mut app, 120, 40));
    // Help is generated from the tables, so a known global binding must appear.
    assert!(
        screen.contains("Ctrl+P"),
        "help lost the palette key:\n{screen}"
    );
    assert!(screen.contains("Commands") || screen.contains("Command palette"));
}

#[test]
fn the_help_page_filter_narrows_the_binding_table() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ToggleHelp);
    app.perform(vibex_tui::action::Intent::BeginFilter);
    for character in "palette".chars() {
        app.filter.push(character);
    }
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("Ctrl+P"),
        "the filter dropped the matching key:\n{screen}"
    );
    assert!(
        !screen.contains("F6"),
        "the filter kept a binding that does not match:\n{screen}"
    );
}

#[test]
fn the_command_palette_shows_matches_and_a_query_line() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::OpenCommandPalette);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Command palette"), "{screen}");
    assert!(
        screen.contains('>'),
        "the palette needs a query line:\n{screen}"
    );
}

#[test]
fn a_confirm_overlay_offers_both_answers() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::RequestQuit);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Confirm"), "{screen}");
    assert!(screen.contains("Cancel"), "{screen}");
}

#[test]
fn a_secret_prompt_never_echoes_the_secret() {
    let mut app = app(120, 40);
    app.overlay = Some(vibex_tui::app::Overlay::Prompt {
        title: "Credential".to_string(),
        field: vibex_tui::app::PromptField::ProviderSecret,
        value: "sk-super-secret-sentinel".to_string(),
    });
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        !screen.contains("sk-super-secret-sentinel"),
        "a stored secret was echoed to the screen:\n{screen}"
    );
    assert!(
        screen.contains("never displayed") || screen.contains("replaces it"),
        "the prompt must explain write-only behaviour:\n{screen}"
    );
    // The mask is present, so the user can see *that* something is typed.
    assert!(screen.contains('•'), "{screen}");
}

#[test]
fn the_management_index_lists_every_section() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoManagement);
    let screen = text(&render(&mut app, 120, 40));
    for needle in [
        "Agents",
        "Providers",
        "MCP servers",
        "Skills",
        "Devices",
        "Recovery",
    ] {
        assert!(
            screen.contains(needle),
            "management lost {needle}:\n{screen}"
        );
    }
}

#[test]
fn unavailable_capabilities_are_visible_not_hidden() {
    // A disconnected backend supports nothing, so every gated row must say so
    // rather than silently disappearing.
    let mut app = app(120, 40);
    app.navigate_to(Page::Recovery);
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("cannot") || screen.contains("unavailable"),
        "gated recovery actions must explain themselves:\n{screen}"
    );
}

#[test]
fn usage_never_invents_cost_data() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Usage);
    let screen = text(&render(&mut app, 120, 40));
    // `$` is deliberately absent: it is the composer's skill trigger, so it is
    // legitimately on screen. The words are what would signal invented cost data.
    for forbidden in ["Cost", "Price", "USD", "EUR"] {
        assert!(
            !screen.contains(forbidden),
            "the usage page invented {forbidden}:\n{screen}"
        );
    }
    assert!(
        screen.contains("token counts only") || screen.contains("Token usage"),
        "{screen}"
    );
}

#[test]
fn cjk_copy_renders_without_breaking_the_frame() {
    let mut app = app(100, 30);
    app.strings = Strings::for_locale(Locale::ZhCn);
    let lines = render(&mut app, 100, 30);
    // A wide glyph occupies two cells, so the buffer's second cell is empty and
    // reading it back inserts a space. Collapsing those spaces recovers the
    // text the user actually sees.
    let screen = text(&lines).replace(' ', "");
    assert!(
        screen.contains("会话") || screen.contains("工作階段"),
        "{screen}"
    );
    // Every rendered row is exactly the frame width, so the right border must
    // be present on every content row: a wide glyph that ran past the column
    // budget would have pushed it off. (Measuring the extracted string would
    // double-count the empty second cell of each wide glyph, so the border is
    // the honest check here.)
    // Reading the text back inserts a cell of padding after every wide glyph,
    // so it cannot measure columns. The buffer can: the status band's
    // right-aligned group must end on the content's last column, which only
    // holds if every wide glyph before it was counted as two.
    let buffer = render_buffer(&mut app, 100, 30);
    let used = content_end_column(&buffer, 1).expect("the status band is not empty");
    let hpad = vibex_tui::layout::OUTER_HPAD;
    assert_eq!(
        used,
        100 - hpad - 1,
        "the status band does not reach the right padding edge under CJK (ends at {used}):\n{}",
        lines.join("\n")
    );
}

#[test]
fn a_session_with_turns_gets_a_turn_rail_in_the_gutter() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    // Two turns is the threshold: one turn has nowhere to navigate.
    app.transcript.set_blocks(vec![
        block("a", "turn-1"),
        block("b", "turn-1"),
        block("c", "turn-2"),
    ]);
    let lines = render(&mut app, 120, 40);
    // The rail occupies the transcript's last two columns.
    let rail_rows = lines
        .iter()
        .filter(|line| line.trim_end().ends_with('•') || line.trim_end().ends_with('▪'))
        .count();
    assert!(rail_rows >= 2, "no turn rail drawn:\n{}", lines.join("\n"));
}

#[test]
fn a_single_turn_draws_one_tick() {
    // The rail counts turns. Falling back to a scrollbar in the same columns
    // made a one-turn session show a mark that stood for nothing the reader
    // could name — and a long one, a thumb they read as a turn.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.transcript
        .set_blocks(vec![block("a", "turn-1"), block("b", "turn-1")]);
    let lines = render(&mut app, 120, 40);
    let ticks = lines
        .iter()
        .filter(|line| line.trim_end().ends_with('•') || line.trim_end().ends_with('▪'))
        .count();
    assert_eq!(ticks, 1, "a single turn is not one tick:\n{}", text(&lines));
}

fn block(id: &str, turn: &str) -> vibex_tui::transcript::Block {
    vibex_tui::transcript::Block {
        id: id.to_string(),
        kind: vibex_desktop_model::TimelineRowKind::AgentMessage,
        title: id.to_string(),
        body: "body".to_string(),
        turn_id: Some(turn.to_string()),
        sequence: 1,
        expanded: false,
        collapsible: false,
        streaming: false,
        failed: false,
        pending_permission: false,
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
    }
}

#[test]
fn color_less_mode_still_renders_every_label() {
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Authority,
            capability: ColorCapability {
                mode: ColorMode::None,
                glyphs: GlyphMode::Ascii,
            },
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            sidebar_path: None,
        },
    );
    app.resize(100, 30);
    let screen = text(&render(&mut app, 100, 30));
    assert!(screen.contains("Authority") || screen.contains("Remote"));
    // The ASCII glyph tier must not emit glyphs a legacy console lacks.
    for forbidden in ['╭', '╰', '╮', '╯'] {
        assert!(
            !screen.contains(forbidden),
            "{forbidden:?} in legacy mode:\n{screen}"
        );
    }
}

#[test]
fn the_selection_marker_follows_the_scope() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoManagement);
    app.set_selection(Scope::Management, 2);
    let screen = text(&render(&mut app, 120, 40));
    // The third row is MCP servers; the list renders without panicking and the
    // row is present at the new index.
    assert!(screen.contains("MCP servers"), "{screen}");
}

#[test]
fn a_tiny_terminal_shows_the_degradation_notice() {
    let app = app(40, 10);
    let message = vibex_tui::view::degradation_message(&app);
    assert!(
        message.is_some(),
        "a 40x10 terminal must produce a degradation message"
    );
    let message = message.unwrap();
    assert!(message.contains("60x16"), "{message}");
    assert!(message.contains("40x10"), "{message}");
}

#[test]
fn a_large_terminal_does_not_trigger_the_degradation_notice() {
    let app = app(120, 40);
    assert!(vibex_tui::view::degradation_message(&app).is_none());
}

#[test]
fn the_locked_runtime_message_gives_three_ways_out() {
    let message = vibex_tui::view::locked_seat_help(Strings::for_locale(Locale::En));
    assert!(message.contains("Remote Access"));
    assert!(message.contains("vibex connect"));
}

fn seeded_session(id: &str, title: &str) -> vibex_core::AgentSession {
    vibex_core::AgentSession {
        id: vibex_core::VibexSessionId::parse(id).expect("valid session id"),
        title: title.to_string(),
        project_id: vibex_core::ProjectId::new(),
        workspace_id: vibex_core::WorkspaceId::new(),
        workspace_root: "/tmp/vibex-card-workspace".to_string(),
        workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
        agent_id: vibex_core::AgentId::parse("claude").expect("valid agent id"),
        state: vibex_core::AgentSessionState::Idle,
        safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: 1_759_237_920_000,
        updated_at_ms: 1_759_251_200_000,
        last_message_at_ms: 1_759_251_200_000,
        archived_at_ms: None,
        deleted_at_ms: None,
    }
}

#[test]
fn a_session_detail_card_opens_and_closes_on_the_row() {
    let mut app = app(120, 40);
    app.agent
        .apply_sessions(Ok(vec![seeded_session(
            "session_card0001",
            "fix the flaky test",
        )]))
        .expect("sessions apply");
    app.perform(vibex_tui::action::Intent::GotoSessions);
    // Row 0 is the workspace group; row 1 is the session.
    app.set_selection(Scope::Sessions, 1);
    let collapsed = text(&render(&mut app, 120, 40));
    assert!(collapsed.contains("fix the flaky test"), "{collapsed}");
    assert!(
        !collapsed.contains("session_card0001"),
        "the id belongs to the card, not the row:\n{collapsed}"
    );

    app.perform(vibex_tui::action::Intent::ToggleSessionCard);
    let expanded = text(&render(&mut app, 120, 40));
    for needle in [
        "session_card0001",
        "/tmp/vibex-card-workspace",
        "2025-09-30",
        "claude",
    ] {
        assert!(
            expanded.contains(needle),
            "the card lost {needle}:\n{expanded}"
        );
    }

    app.perform(vibex_tui::action::Intent::CollapseSessionCards);
    let closed = text(&render(&mut app, 120, 40));
    assert!(!closed.contains("session_card0001"), "{closed}");
}

/// Two sessions in the same workspace, so they share one group heading.
fn session_pair() -> Vec<vibex_core::AgentSession> {
    let base = seeded_session("session_sidebar_alpha", "alpha session");
    let mut first = base.clone();
    first.last_message_at_ms = 1_759_251_100_000;
    let mut second = base;
    second.id = vibex_core::VibexSessionId::parse("session_sidebar_beta").expect("valid id");
    second.title = "beta session".to_string();
    second.last_message_at_ms = 1_759_251_200_000;
    vec![first, second]
}

#[test]
fn pinning_a_session_hoists_it_above_the_rest() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.agent.apply_sessions(Ok(session_pair())).expect("apply");
    app.perform(Intent::GotoSessions);
    // Row 0 is the workspace heading, rows 1 and 2 are the sessions; the newer
    // one sorts first.
    let before = text(&render(&mut app, 120, 40));
    let alpha = before.find("alpha session").expect("alpha on screen");
    let beta = before.find("beta session").expect("beta on screen");
    assert!(beta < alpha, "the newer session should lead:\n{before}");

    app.set_selection(Scope::Sessions, 2);
    app.perform(Intent::PinSession);
    let after = text(&render(&mut app, 120, 40));
    let alpha = after.find("alpha session").expect("alpha on screen");
    let beta = after.find("beta session").expect("beta on screen");
    assert!(alpha < beta, "the pinned session did not move up:\n{after}");
}

#[test]
fn a_pinned_session_cannot_be_passed_by_a_manual_move() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.agent.apply_sessions(Ok(session_pair())).expect("apply");
    app.perform(Intent::GotoSessions);
    // Row 2 is the older session; pinning it hoists it above the newer one.
    app.set_selection(Scope::Sessions, 2);
    app.perform(Intent::PinSession);
    app.set_selection(Scope::Sessions, 2);
    // The unpinned session cannot be moved above the pinned one: the projection
    // always sorts pins first, so the move would be a lie.
    app.perform(Intent::MoveSessionUp);
    let screen = text(&render(&mut app, 120, 40));
    let alpha = screen.find("alpha session").expect("alpha on screen");
    let beta = screen.find("beta session").expect("beta on screen");
    assert!(alpha < beta, "the pinned order was disturbed:\n{screen}");
    assert!(
        screen.contains("Pinned sessions always come first"),
        "the refusal was silent:\n{screen}"
    );
}

#[test]
fn a_manual_move_reorders_two_unpinned_sessions() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.agent.apply_sessions(Ok(session_pair())).expect("apply");
    app.perform(Intent::GotoSessions);
    // Row 2 is the older session; moving it up swaps the two.
    app.set_selection(Scope::Sessions, 2);
    app.perform(Intent::MoveSessionUp);
    let screen = text(&render(&mut app, 120, 40));
    let alpha = screen.find("alpha session").expect("alpha on screen");
    let beta = screen.find("beta session").expect("beta on screen");
    assert!(alpha < beta, "the move did not take:\n{screen}");
    // The cursor followed the row that moved.
    assert_eq!(app.selection_for(Scope::Sessions), 1);
}

#[test]
fn grouping_can_be_folded_away_without_losing_the_sessions() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.agent.apply_sessions(Ok(session_pair())).expect("apply");
    app.perform(Intent::GotoSessions);
    let grouped = text(&render(&mut app, 120, 40));
    assert!(grouped.contains("vibex-card-workspace"), "{grouped}");
    app.perform(Intent::ToggleSidebarGrouping);
    let flat = text(&render(&mut app, 120, 40));
    assert!(
        !flat.contains("vibex-card-workspace"),
        "the heading survived the toggle:\n{flat}"
    );
    assert!(flat.contains("alpha session"), "{flat}");
    assert!(flat.contains("beta session"), "{flat}");
}

fn seeded_block(
    id: &str,
    kind: vibex_desktop_model::TimelineRowKind,
    body: &str,
) -> vibex_tui::transcript::Block {
    vibex_tui::transcript::Block {
        id: id.to_string(),
        kind,
        title: String::new(),
        body: body.to_string(),
        turn_id: Some("turn-1".to_string()),
        sequence: 1,
        expanded: false,
        collapsible: false,
        streaming: false,
        failed: false,
        pending_permission: false,
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
    }
}

/// One user message per sequence, so the projection has stable ids and the
/// rendered text names the sequence it came from.
fn seeded_items(
    session_id: &vibex_core::VibexSessionId,
    sequences: std::ops::RangeInclusive<i64>,
) -> Vec<vibex_core::TimelineItem> {
    sequences
        .map(|sequence| vibex_core::TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: 1_759_237_920_000 + sequence,
            source: vibex_core::TimelineSource::User,
            kind: vibex_core::TimelineItemKind::UserMessage,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: vibex_core::TimelineRedactionState::None,
            execution_attribution: None,
            payload: vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: format!("history message {sequence}"),
                attachments: Vec::new(),
                ..Default::default()
            }),
        })
        .collect()
}

fn transcript_app(width: u16, height: u16) -> App {
    let mut app = app(width, height);
    app.transcript.set_blocks(vec![
        seeded_block(
            "block-1",
            vibex_desktop_model::TimelineRowKind::UserMessage,
            "fix the flaky upload test",
        ),
        seeded_block(
            "block-2",
            vibex_desktop_model::TimelineRowKind::AgentMessage,
            "The UPLOAD path was retried twice before it succeeded.",
        ),
    ]);
    app.navigate_to(Page::Agent);
    app
}

#[test]
fn search_opens_a_bar_with_a_counter_and_smart_case() {
    let mut app = transcript_app(120, 40);
    assert!(app.begin_search(), "a non-empty transcript can be searched");
    app.search
        .as_mut()
        .expect("search is open")
        .set_query("upload".to_string());
    app.refresh_search_matches();
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("search:"), "no search bar:\n{screen}");
    // A lowercase query folds case, so both spellings match; the counter is
    // the current match over the total.
    assert!(screen.contains("1/2"), "wrong counter:\n{screen}");
    assert!(app.step_search(1), "the second match is reachable");
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("2/2"),
        "stepping lost the counter:\n{screen}"
    );

    // An uppercase letter turns smart case off, and nothing spells it exactly
    // that way.
    app.search
        .as_mut()
        .expect("search is open")
        .set_query("Upload".to_string());
    app.refresh_search_matches();
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("no matches"),
        "uppercase must not fold:\n{screen}"
    );
}

#[test]
fn search_marks_matches_in_the_rendered_line() {
    let mut app = transcript_app(120, 40);
    assert!(app.begin_search());
    app.search
        .as_mut()
        .expect("search is open")
        .set_query("flaky".to_string());
    app.refresh_search_matches();
    let theme = vibex_tui::TuiTheme::resolve(
        Some("vibex-dark"),
        vibex_ui::GpuiThemeMode::Dark,
        ColorCapability {
            mode: ColorMode::TrueColor,
            glyphs: GlyphMode::Unicode,
        },
    );
    let buffer = render_buffer(&mut app, 120, 40);
    let mut highlighted = 0usize;
    for row in 0..40u16 {
        for column in 0..120u16 {
            if let Some(cell) = buffer.cell((column, row))
                && cell.bg == theme.roles.accent_attention
            {
                highlighted += 1;
            }
        }
    }
    assert!(
        highlighted >= "flaky".len(),
        "the match is not painted: {highlighted} cells"
    );
}

#[test]
fn an_invalid_pattern_says_so_instead_of_matching_nothing_silently() {
    let mut app = transcript_app(120, 40);
    assert!(app.begin_search());
    app.search
        .as_mut()
        .expect("search is open")
        .set_query("(unclosed".to_string());
    app.refresh_search_matches();
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("bad pattern"), "{screen}");
}

#[test]
fn a_match_is_a_regular_expression() {
    let mut app = transcript_app(120, 40);
    assert!(app.begin_search());
    app.search
        .as_mut()
        .expect("search is open")
        .set_query("flaky|retried".to_string());
    app.refresh_search_matches();
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("1/2"), "{screen}");
}

#[test]
fn a_scrolled_transcript_pins_the_prompt_it_belongs_to() {
    let mut app = app(120, 40);
    let mut blocks = vec![seeded_block(
        "prompt-0",
        vibex_desktop_model::TimelineRowKind::UserMessage,
        "PINNED QUESTION about the upload path",
    )];
    for index in 0..12 {
        blocks.push(seeded_block(
            &format!("reply-{index}"),
            vibex_desktop_model::TimelineRowKind::AgentMessage,
            "an answer long enough to occupy a row or two of the transcript",
        ));
    }
    app.transcript.set_blocks(blocks);
    app.navigate_to(Page::Agent);
    // Scroll past the question without expanding it.
    app.scroll.follow = false;
    app.scroll.offset = 8;
    let lines = render(&mut app, 120, 40);
    let screen = text(&lines);
    let pinned_row = lines
        .iter()
        .position(|line| line.contains("PINNED QUESTION"))
        .unwrap_or_else(|| panic!("the prompt is not pinned:\n{screen}"));
    // The pinned header is the first row of the transcript band, above the
    // content that scrolled it away.
    assert!(
        pinned_row <= 4,
        "the pinned header is not at the top of the band: row {pinned_row}\n{screen}"
    );

    // Scrolled to the very top, the question is inline and nothing is pinned a
    // second time.
    app.scroll.offset = 0;
    let lines = render(&mut app, 120, 40);
    let pinned_row = lines
        .iter()
        .position(|line| line.contains("PINNED QUESTION"))
        .expect("the question is visible inline");
    assert!(pinned_row >= 3, "the question must not be drawn twice");
}

#[test]
fn a_dragged_selection_becomes_the_text_on_the_clipboard() {
    let mut app = transcript_app(120, 40);
    // One frame publishes the band's rectangle, which a real drag needs.
    let _ = render(&mut app, 120, 40);
    assert!(
        app.regions.scrollback.height > 0,
        "the transcript band must be published for the mouse"
    );
    let expected = app
        .transcript
        .plain_lines(0, 1, &app.theme.clone(), Strings::for_locale(Locale::En))
        .into_iter()
        .next()
        .expect("a first line");
    let (prefix, _) = vibex_tui::text::take_width(&expected, 4);

    app.begin_text_selection(0, 0);
    app.extend_text_selection(0, 4);
    assert!(app.finish_text_selection(), "the drag covered cells");
    let copied = app.selected_text().expect("a non-empty selection");
    assert_eq!(copied, prefix.trim_end());

    // A selection over several lines joins them with newlines and drops the
    // padding a terminal would otherwise put on the clipboard. Block rows are
    // dense, so the second *text* row is the next block's first line.
    app.begin_text_selection(0, 0);
    app.extend_text_selection(2, 6);
    let copied = app.selected_text().expect("a multi-line selection");
    assert!(copied.contains('\n'), "lines are not joined: {copied:?}");
    for line in copied.lines() {
        assert_eq!(line, line.trim_end(), "trailing padding was copied");
    }

    // `Esc` is what dismisses the highlight.
    assert!(app.clear_text_selection());
    assert!(app.selected_text().is_none());
}

/// The cell column a needle starts at on a rendered row, counted in terminal
/// cells rather than bytes, so a double-width title cannot skew the reading.
fn column_of(buffer: &ratatui::buffer::Buffer, row: u16, needle: &str) -> Option<u16> {
    let width = buffer.area.width;
    let cells = (0..width)
        .map(|column| {
            buffer
                .cell((column, row))
                .map(|cell| cell.symbol().to_string())
                .unwrap_or_default()
        })
        .collect::<Vec<_>>();
    (0..width).find(|column| {
        let mut text = String::new();
        let mut cursor = *column;
        while text.len() < needle.len() && cursor < width {
            text.push_str(&cells[usize::from(cursor)]);
            cursor += 1;
        }
        text.starts_with(needle)
    })
}

/// The text of each held message, for the tests that only care about words.
/// The queue of the session in front of the reader, which is what the UI
/// shows and what its keys act on.
fn queued_texts(app: &App) -> Vec<String> {
    app.queued_for_active()
        .into_iter()
        .map(|index| app.queued_messages[index].text.clone())
        .collect()
}

/// Open a session: a message can only be held for one, so the queue's own
/// tests have to be inside one.
fn enter_session(app: &mut App, id: &str) {
    let session = seeded_session(id, "queue fixture");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session);
}

fn settings_app(width: u16, height: u16) -> App {
    let mut app = app(width, height);
    app.perform(vibex_tui::action::Intent::OpenSettings);
    app
}

fn select_setting(app: &mut App, row: vibex_tui::settings::SettingRow) {
    let index = app
        .visible_settings()
        .iter()
        .position(|candidate| *candidate == row)
        .expect("the row is visible");
    app.set_selection(Scope::Settings, index);
}

/// Seed a plan update the way the runtime delivers one.
///
/// The transcript deliberately draws no row for it, so a test that wants to see
/// the plan has to put it where the projection reads it.
fn seed_plan(app: &mut App, session_id: &vibex_core::VibexSessionId) {
    app.agent.state.selected_session_id = Some(session_id.clone());
    app.agent.state.timeline.replace_authoritative(
        session_id.clone(),
        vec![seeded_item(
            session_id,
            1,
            vibex_core::TimelineItemKind::TodoUpdate,
            vibex_core::TimelinePayload::TodoUpdate(vibex_core::TodoUpdatePayload {
                title: "Ship the dock".into(),
                items: vec![
                    vibex_core::PlanStepPayload {
                        title: "read the design".into(),
                        status: vibex_core::PlanStepStatus::Completed,
                    },
                    vibex_core::PlanStepPayload {
                        title: "write the band".into(),
                        status: vibex_core::PlanStepStatus::Running,
                    },
                    vibex_core::PlanStepPayload {
                        title: "add a test".into(),
                        status: vibex_core::PlanStepStatus::Pending,
                    },
                ],
                raw_extension: None,
            }),
        )],
    );
    app.sync_transcript();
}

#[test]
fn the_dock_lists_the_plan_and_the_held_queue() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_queue0001");
    seed_plan(&mut app, &vibex_core::VibexSessionId::new());
    app.enqueue_message("fix the flake".to_string());
    app.perform(Intent::ToggleDock);
    let screen = text(&render(&mut app, 120, 40));
    for needle in ["Running", "Plan", "write the band", "Held", "fix the flake"] {
        assert!(screen.contains(needle), "the dock lost {needle}:\n{screen}");
    }
    // The band publishes its rows so the mouse can select them.
    assert!(app.regions.dock.is_some(), "the dock is not clickable");

    // Esc folds the panel away before it means anything else.
    app.perform(Intent::Back);
    assert!(!app.dock_open, "Esc left the dock open");
    let closed = text(&render(&mut app, 120, 40));
    assert!(
        !closed.contains("Held"),
        "the dock's own section heading outlived it:\n{closed}"
    );
    assert!(
        app.regions.dock.is_none(),
        "the closed dock kept its hit rect"
    );
}

#[test]
fn the_dock_cursor_skips_headings_and_takes_a_held_message_back() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_queue0001");
    seed_plan(&mut app, &vibex_core::VibexSessionId::new());
    app.enqueue_message("fix the flake".to_string());
    app.perform(Intent::ToggleDock);
    let rows = app.dock_rows();
    // Sections are headings plus their rows; the queue row is last.
    let queue_row = rows
        .iter()
        .position(|row| matches!(row, vibex_tui::app::DockRow::Queue { .. }))
        .expect("the held message is listed");
    // Walking down from the top lands on bindings only, never on a heading.
    for _ in 0..rows.len() {
        assert!(
            !matches!(
                rows[app.dock_selection.expect("the dock has a cursor")],
                vibex_tui::app::DockRow::Header { .. }
            ),
            "the cursor stopped on a heading"
        );
        // `j`/`Down` move the dock cursor while it owns the keys.
        app.perform(Intent::SelectNext);
    }
    app.dock_selection = Some(queue_row);
    app.perform(Intent::DockActivate);
    assert_eq!(app.composer.text(), "fix the flake");
    assert!(app.queued_messages.is_empty(), "the message was not taken");
}

#[test]
fn a_long_dock_says_how_much_it_is_not_showing() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_queue0007");
    // Ten held messages, against a dock that can show seven rows besides its
    // title: the rest must be counted rather than silently dropped.
    for index in 0..10 {
        app.enqueue_message(format!("held message {index}"));
    }
    app.perform(Intent::ToggleDock);
    assert_eq!(app.dock_height() as usize, vibex_tui::app::MAX_DOCK_ROWS);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("more"), "{screen}");
    assert!(screen.contains("held message 0"), "{screen}");
}

#[test]
fn hiding_finished_dock_work_leaves_the_running_step() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    seed_plan(&mut app, &vibex_core::VibexSessionId::new());
    app.perform(Intent::ToggleDock);
    let before = text(&render(&mut app, 120, 40));
    assert!(before.contains("1/3"), "{before}");
    app.perform(Intent::DockHideDone);
    let after = text(&render(&mut app, 120, 40));
    assert!(
        after.contains("write the band"),
        "a running step was hidden as finished:\n{after}"
    );
}

#[test]
fn an_attached_image_shows_as_a_chip_and_a_count() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.insert_str("look at this ");
    app.composer
        .insert_image(
            "image/png",
            vibex_tui::composer::ImageSource::Path("/tmp/shot.png".into()),
        )
        .expect("the image attaches");
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("[Image #1]"), "{screen}");
    // The info line turns the attachment into a number the reader can check.
    assert!(screen.contains("1 images"), "{screen}");
    // The label is not part of what the Agent is told in words.
    assert_eq!(app.composer.expanded_text(), "look at this ");
}

#[test]
fn a_pasted_image_path_attaches_instead_of_typing_the_path() {
    let directory = tempfile::tempdir().expect("temp dir");
    let shot = directory.path().join("shot.png");
    std::fs::write(&shot, b"png").expect("write");
    assert_eq!(
        vibex_tui::app::App::image_path_from_paste(&shot.display().to_string()),
        Some(shot.display().to_string())
    );
    assert_eq!(
        vibex_tui::app::App::image_path_from_paste("just some words"),
        None
    );
}

#[test]
fn markdown_styling_reaches_the_screen() {
    use vibex_desktop_model::TimelineRowKind;
    let mut app = transcript_app(120, 44);
    let mut block = seeded_block(
        "styled-body",
        TimelineRowKind::AgentMessage,
        "### 渲染标题\n\n正文里有 `inline_code` 片段，还有 **重点** 内容。\n\n\
         升级 `vibex-tui` 到 `0.1.0-rc.7`，改 `crates/vibex-tui/src/view.rs`。",
    );
    block.expanded = true;
    block.collapsible = true;
    app.transcript.set_blocks(vec![block]);
    let buffer = render_buffer(&mut app, 120, 44);
    let rows = (0..44)
        .map(|row| display_row(&buffer, row, 120))
        .collect::<Vec<_>>();

    // Markup is never printed: no `#` heading markers, no code backticks.
    let screen = rows.join("\n");
    assert!(
        !screen.contains('#'),
        "a heading marker is on screen:\n{screen}"
    );
    assert!(!screen.contains('`'), "backticks are on screen:\n{screen}");

    // The heading is bold.
    let heading_row = rows
        .iter()
        .position(|row| row.contains("渲染标题"))
        .expect("the heading is on screen");
    let bold = (0..120)
        .filter_map(|column| buffer.cell((column, heading_row as u16)))
        .any(|cell| {
            cell.symbol() == "标"
                && cell
                    .style()
                    .add_modifier
                    .contains(ratatui::style::Modifier::BOLD)
        });
    assert!(bold, "the heading is not bold:\n{screen}");

    // The colour hierarchy reaches the screen: prose is a step down from the
    // heading, and neither is the same colour as the code.
    let theme = vibex_tui::theme::TuiTheme::resolve(
        Some("vibex-dark"),
        vibex_ui::GpuiThemeMode::Dark,
        vibex_tui::ColorCapability {
            mode: vibex_tui::ColorMode::TrueColor,
            glyphs: vibex_tui::GlyphMode::Unicode,
        },
    );
    let roles = theme.roles;
    let cell_colour = |needle: &str| {
        let row = rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is on screen:\n{screen}"));
        // The first *content* cell: the rail is a filled space and the current
        // block's pointer glyph sits in it.
        (0..120)
            .filter_map(|column| buffer.cell((column, row as u16)))
            .find(|cell| cell.symbol().chars().any(char::is_alphanumeric))
            .and_then(|cell| cell.style().fg)
    };
    assert_eq!(
        cell_colour("正文里有"),
        Some(roles.gray_bright),
        "prose is not set one step down from the heading"
    );
    assert_eq!(
        cell_colour("渲染标题"),
        Some(theme.markdown.heading[2]),
        "the heading does not wear its level's colour"
    );

    // The inline code keeps the literal colour over its whole run, which is
    // what the backticks used to stand for, and it does not need a background
    // of its own to read as code.
    let code_row = rows
        .iter()
        .position(|row| row.contains("inline_code"))
        .expect("the code span is on screen");
    let code_cells = (0..120)
        .filter_map(|column| buffer.cell((column, code_row as u16)))
        .filter(|cell| !cell.symbol().trim().is_empty())
        .collect::<Vec<_>>();
    let literal = code_cells
        .iter()
        .filter(|cell| cell.style().fg == Some(theme.markdown.code))
        .count();
    assert!(
        literal >= "inline_code".len(),
        "only {literal} cells carry the literal colour:\n{screen}"
    );
    assert!(
        code_cells
            .iter()
            .all(|cell| cell.style().bg != Some(roles.code_background)),
        "an inline literal still wears a code background:\n{screen}"
    );

    // A literal is coloured by what it is, so the version and the path in one
    // sentence are findable at a glance rather than by reading the line.
    let span_colour = |needle: &str| {
        let row = rows
            .iter()
            .position(|row| row.contains(needle))
            .unwrap_or_else(|| panic!("{needle} is on screen:\n{screen}"));
        // Column, not byte offset: the row is full of double-width glyphs.
        let byte = rows[row].find(needle).expect("the needle is in the row");
        let column = vibex_tui::text::display_width(&rows[row][..byte]).min(119) as u16;
        (column..120)
            .filter_map(|column| buffer.cell((column, row as u16)))
            .find(|cell| cell.symbol().chars().any(char::is_alphanumeric))
            .and_then(|cell| cell.style().fg)
    };
    assert_eq!(
        span_colour("vibex-tui "),
        Some(theme.markdown.code),
        "an identifier is not drawn as code:\n{screen}"
    );
    assert_eq!(
        span_colour("0.1.0-rc.7"),
        Some(theme.markdown.code_number),
        "a version is not drawn as a number:\n{screen}"
    );
    assert_eq!(
        span_colour("crates/vibex-tui/src/view.rs"),
        Some(theme.markdown.code_path),
        "a path is not drawn as a path:\n{screen}"
    );
}

/// A row's text with the filler cell after each wide glyph dropped, so a
/// double-width string can be matched as it reads.
fn display_row(buffer: &ratatui::buffer::Buffer, row: u16, width: u16) -> String {
    let mut out = String::new();
    let mut column = 0u16;
    while column < width {
        let Some(cell) = buffer.cell((column, row)) else {
            break;
        };
        let symbol = cell.symbol();
        out.push_str(symbol);
        let advance = vibex_tui::text::display_width(symbol).max(1) as u16;
        column += advance;
    }
    out
}

#[test]
fn the_transcript_uses_the_whole_band_on_a_wide_terminal() {
    let mut app = transcript_app(200, 44);
    // One long paragraph, the shape an Agent's prose arrives in.
    let mut block = seeded_block(
        "wide-body",
        vibex_desktop_model::TimelineRowKind::AgentMessage,
        "This paragraph is plain prose that should be wrapped to the width of the \
         transcript band rather than to a sidebar-adjusted pane width, because on a \
         wide terminal the difference is the entire right half of the screen and the \
         reader has to scan a narrow column with nothing beside it.",
    );
    block.expanded = true;
    block.collapsible = true;
    app.transcript.set_blocks(vec![block]);
    let buffer = render_buffer(&mut app, 200, 44);
    let widest = (0..44)
        .filter_map(|row| {
            (0..200).rev().find(|column| {
                buffer
                    .cell((*column, row))
                    .is_some_and(|cell| !cell.symbol().trim().is_empty())
            })
        })
        .max()
        .expect("the block is on screen");
    // The band ends at the transcript's right edge; allow for the block's own
    // right padding and the gutter, but not for a pane that is not there.
    assert!(
        widest > 170,
        "the transcript stopped at column {widest} of 200 -- it is not using the band"
    );
}

#[test]
fn the_session_state_column_lines_up_on_every_row() {
    let mut app = app(120, 40);
    // One ASCII title and two double-width ones: character-counted padding put
    // the state column in a different place on each of these.
    let base = seeded_session("session_column0001", "short");
    let mut cjk = base.clone();
    cjk.id = vibex_core::VibexSessionId::parse("session_column0002").expect("valid id");
    cjk.title = "帮我看看这个会话的状态列".to_string();
    let mut long = base.clone();
    long.id = vibex_core::VibexSessionId::parse("session_column0003").expect("valid id");
    long.title = "一个特别特别特别特别特别特别长的中文会话标题".to_string();
    app.agent
        .apply_sessions(Ok(vec![base, cjk, long]))
        .expect("sessions apply");
    app.perform(vibex_tui::action::Intent::GotoSessions);
    let buffer = render_buffer(&mut app, 120, 40);
    let mut columns = Vec::new();
    for row in 0..40 {
        if let Some(column) = column_of(&buffer, row, "Idle") {
            columns.push(column);
        }
    }
    assert!(columns.len() >= 3, "not every session row was drawn");
    assert!(
        columns.windows(2).all(|pair| pair[0] == pair[1]),
        "the state column is ragged: {columns:?}"
    );
    // And the whole row stays inside the frame.
    for row in 0..40 {
        let last = (0..120).rev().find(|column| {
            buffer
                .cell((*column, row))
                .is_some_and(|cell| !cell.symbol().trim().is_empty())
        });
        assert!(last.is_none_or(|column| column < 120));
    }
}

#[test]
fn the_session_list_does_not_wear_the_agent_pages_chrome() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.agent.apply_sessions(Ok(session_pair())).expect("apply");
    enter_session(&mut app, "session_queue0002");
    seed_plan(&mut app, &vibex_core::VibexSessionId::new());
    app.enqueue_message("held while listing".to_string());
    app.perform(Intent::GotoSessions);
    let listing = text(&render(&mut app, 120, 40));
    for leaked in ["write the band", "held while listing", "Held"] {
        assert!(
            !listing.contains(leaked),
            "the session list showed the agent page's {leaked}:\n{listing}"
        );
    }
    // The session view still has them.
    app.navigate_to(Page::Agent);
    let agent_page = text(&render(&mut app, 120, 40));
    assert!(agent_page.contains("write the band"), "{agent_page}");
    assert!(agent_page.contains("held while listing"), "{agent_page}");
}

#[test]
fn a_click_on_the_composer_takes_the_keyboard_and_the_caret() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Main;
    app.composer.set_text("first line\nsecond line");
    let _ = render(&mut app, 120, 40);
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");
    assert!(app.click_composer(region.x + 5, region.y));
    assert_eq!(app.focus, vibex_tui::app::Focus::Composer);
    assert_eq!(app.composer.cursor(), 5);

    // The border has no cell to place a caret on, but it still takes focus: a
    // box the reader can see and click must accept typing.
    app.focus = vibex_tui::app::Focus::Main;
    let band = app.regions.composer_band.expect("the band is published");
    assert!(app.click_composer(band.x + 1, band.y + band.height - 1));
    assert_eq!(app.focus, vibex_tui::app::Focus::Composer);
}

#[test]
fn entering_a_session_focuses_the_composer() {
    let mut app = app(120, 40);
    app.agent
        .apply_sessions(Ok(session_pair()))
        .expect("sessions apply");
    let session_id = app.agent.state.sessions.value.as_ref().unwrap()[0]
        .id
        .clone();
    app.open_session(session_id);
    assert_eq!(app.page, Page::Agent);
    assert_eq!(
        app.focus,
        vibex_tui::app::Focus::Composer,
        "opening a session left the keyboard outside the composer"
    );
}

#[test]
fn the_composer_places_the_terminal_cursor_on_the_draft() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text("ab");
    let backend = TestBackend::new(120, 40);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, &mut app))
        .expect("frame draws");
    let region = app.regions.composer.expect("composer rows");
    let position = terminal.get_cursor_position().expect("the caret is placed");
    assert_eq!(position.y, region.y);
    // Two characters in, plus the prompt arrow's two columns.
    assert_eq!(position.x, region.x + 2 + 2);

    // The caret follows the draft, including onto a wrapped row.
    for _ in 0..30 {
        app.composer.insert_char('x');
    }
    terminal
        .draw(|frame| vibex_tui::view::render(frame, &mut app))
        .expect("frame draws");
    let wrapped = terminal.get_cursor_position().expect("the caret moved");
    assert!(
        wrapped.y > region.y || wrapped.x > position.x,
        "the caret did not follow the draft"
    );
}

#[test]
fn a_wrapped_draft_keeps_every_row_on_screen() {
    // Regression: the composer's band counted newlines, so one draft line that
    // wrapped was one row tall and the renderer clipped everything past it.
    let mut app = app(80, 24);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text(
        "the quick brown fox jumps over the lazy dog and keeps on running past the edge of the box",
    );
    let lines = render(&mut app, 80, 24);
    let screen = text(&lines);
    assert!(
        screen.contains("quick"),
        "the draft's first row is missing:\n{screen}"
    );
    assert!(
        screen.contains("edge of the box"),
        "the wrapped draft was clipped to its first row:\n{screen}"
    );
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");
    assert!(region.height >= 2, "the box did not grow: {region:?}");
    // The box grew by exactly the rows it drew: the bottom border still sits on
    // the row after the last text row.
    assert_eq!(
        usize::from(region.bottom()),
        lines
            .iter()
            .position(|line| line.contains('╰'))
            .expect("the composer has a bottom border"),
        "the box and its rows disagree:\n{screen}"
    );
}

#[test]
fn a_draft_taller_than_the_composer_scrolls_to_the_caret() {
    let mut app = app(80, 24);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    let draft = (1..=40)
        .map(|row| format!("row {row}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.composer.set_text(&draft);
    app.composer.move_to_end();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, &mut app))
        .expect("frame draws");
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");
    // The window followed the caret rather than showing the head of the draft.
    assert!(
        app.regions.composer_scroll > 0,
        "a draft forty rows tall did not scroll"
    );
    let screen = text(&buffer_lines(terminal.backend().buffer(), 80, 24));
    assert!(screen.contains("row 40"), "{screen}");
    assert!(
        !screen.contains("row 1\n"),
        "the box did not scroll:\n{screen}"
    );
    // The caret is on the last row inside the box, not clipped off the bottom.
    let caret = terminal.get_cursor_position().expect("the caret is placed");
    assert_eq!(caret.y, region.bottom() - 1);
}

#[test]
fn a_click_in_a_scrolled_composer_lands_on_the_row_that_was_clicked() {
    let mut app = app(80, 24);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    let draft = (1..=40)
        .map(|row| format!("row {row}"))
        .collect::<Vec<_>>()
        .join("\n");
    app.composer.set_text(&draft);
    app.composer.move_to_end();
    let _ = render(&mut app, 80, 24);
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");
    // A click on the first visible row is the row the box scrolled to, not the
    // draft's first row.
    assert!(app.click_composer(region.x, region.y));
    let width = usize::from(region.width) - vibex_tui::glyphs::PROMPT_ARROW_WIDTH;
    assert_eq!(
        app.composer.cursor_cell(width).0,
        app.regions.composer_scroll,
        "the click did not map through the scroll offset"
    );
}

#[test]
fn the_settings_keys_row_opens_the_binding_editor() {
    use vibex_tui::action::Intent;
    use vibex_tui::app::Overlay;
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    select_setting(&mut app, SettingRow::Keys);
    app.perform(Intent::ActivateSetting);
    assert!(
        matches!(app.overlay, Some(Overlay::Keys { .. })),
        "the key-bindings row did not open the editor"
    );
    let lines = render(&mut app, 120, 40);
    let screen = text(&lines);
    assert!(screen.contains("Command palette"), "{screen}");
    assert!(screen.contains("Ctrl+P"), "{screen}");
    // Scopes are headings, so the list says where each chord applies.
    assert!(screen.contains("GLOBAL"), "{screen}");
}

#[test]
fn a_conflicting_rebind_is_refused_and_named() {
    use vibex_tui::action::Intent;
    use vibex_tui::app::Overlay;
    use vibex_tui::keymap::{Chord, Scope};
    let mut app = app(120, 40);
    app.overlay = Some(Overlay::Keys {
        query: String::new(),
        selected: 0,
        capturing: None,
        message: None,
        dirty: false,
    });
    // `Ctrl+P` belongs to the command palette; rebinding the settings action to
    // it would shadow the palette with nothing on screen to say so.
    app.begin_key_capture(Intent::OpenSettings);
    app.finish_key_capture(Chord::ctrl('p'));
    assert!(
        !app.keymap.is_overridden(Intent::OpenSettings),
        "a shadowing chord was accepted"
    );
    let Some(Overlay::Keys { message, .. }) = app.overlay.clone() else {
        panic!("the editor closed on a refused chord");
    };
    let message = message.expect("the refusal explains itself");
    assert!(message.contains("command_palette"), "{message}");
    // A free chord goes through and marks the table dirty.
    let free = Chord::ctrl('j');
    assert_eq!(
        app.keymap
            .conflict(Scope::Global, free, Intent::OpenSettings),
        None,
        "the test picked a chord something else already owns"
    );
    app.begin_key_capture(Intent::OpenSettings);
    app.finish_key_capture(free);
    assert_eq!(app.keymap.chord_for(Intent::OpenSettings), Some(free));
}

#[test]
fn the_settings_filter_narrows_the_list_to_matching_rows() {
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    app.perform(vibex_tui::action::Intent::BeginFilter);
    for character in "workspace".chars() {
        app.push_settings_filter(character);
    }
    assert_eq!(app.visible_settings(), vec![SettingRow::Workspace]);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("filter:"), "no filter bar:\n{screen}");
    assert!(screen.contains("Workspace"), "{screen}");

    // `Esc` clears the query and leaves the mode.
    app.perform(vibex_tui::action::Intent::Back);
    assert!(app.visible_settings().len() > 1);
}

#[test]
fn the_settings_chooser_previews_and_escape_puts_the_value_back() {
    use vibex_tui::app::SettingsMode;
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    select_setting(&mut app, SettingRow::Theme);
    let original = app.settings.theme_id.clone();
    app.perform(vibex_tui::action::Intent::ActivateSetting);
    assert!(
        matches!(app.settings.view, SettingsMode::Picking { .. }),
        "Enter on a choice row opens the chooser"
    );
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("Esc"),
        "the chooser has no footer:\n{screen}"
    );
    assert!(
        screen.contains("Vibex Dark") && screen.contains("Catppuccin Mocha"),
        "the chooser does not list its values:\n{screen}"
    );

    app.step_setting_pick(1);
    assert_ne!(
        app.settings.theme_id, original,
        "moving in the chooser must preview the value"
    );

    app.perform(vibex_tui::action::Intent::Back);
    assert_eq!(
        app.settings.theme_id, original,
        "Esc must put the previewed value back"
    );
    assert!(app.settings.view.is_browse());
}

#[test]
fn the_settings_editor_commits_a_workspace_path() {
    use vibex_tui::app::SettingsMode;
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    select_setting(&mut app, SettingRow::Workspace);
    app.perform(vibex_tui::action::Intent::ActivateSetting);
    let SettingsMode::Editing { row, .. } = app.settings.view else {
        panic!("Enter on a text row opens the editor");
    };
    assert_eq!(row, SettingRow::Workspace);
    app.settings.view = SettingsMode::Editing {
        row,
        buffer: "/tmp/vibex-settings-workspace".to_string(),
    };
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("/tmp/vibex-settings-workspace"),
        "the buffer is not visible while editing:\n{screen}"
    );
    app.commit_setting_edit();
    assert_eq!(
        app.workspace_path.as_deref(),
        Some("/tmp/vibex-settings-workspace")
    );
}

#[test]
fn resetting_a_setting_asks_first_and_then_restores_the_default() {
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    select_setting(&mut app, SettingRow::Theme);
    let themes = vibex_ui::theme_catalog::themes_for(app.settings.mode).collect::<Vec<_>>();
    let other = themes
        .iter()
        .find(|theme| theme.id != "vibex-dark")
        .expect("more than one theme ships");
    app.apply_setting_value(SettingRow::Theme, other.id);
    assert_eq!(app.settings.theme_id, other.id);

    app.perform(vibex_tui::action::Intent::ResetSetting);
    assert!(
        app.overlay.is_some(),
        "a reset is destructive enough to ask first"
    );
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert_eq!(app.settings.theme_id, "vibex-dark");
    assert!(app.overlay.is_none());
}

#[test]
fn the_bottom_status_line_can_be_turned_off() {
    use vibex_tui::settings::SettingRow;
    let mut app = app(120, 44);
    app.navigate_to(Page::Agent);
    let with = text(&render(&mut app, 120, 44));
    app.settings.status_line = false;
    let without = text(&render(&mut app, 120, 44));
    // The rows go back to the transcript, so the two frames differ.
    assert_ne!(with, without);
    // And the setting is a real row with a real value.
    assert_eq!(
        app.setting_value(SettingRow::StatusLine),
        vibex_tui::Strings::for_locale(Locale::En).disabled()
    );
    assert!(app.apply_setting_value(SettingRow::StatusLine, "on"));
    assert!(app.settings.status_line);
}

#[test]
fn a_warning_banner_outranks_a_tip_and_is_not_displaced() {
    use vibex_tui::app::{Banner, BannerPriority};
    let mut app = app(120, 40);
    assert!(app.set_banner(Banner::info("a tip").with_priority(BannerPriority::Tip)));
    assert!(
        !app.set_banner(Banner::info("another tip").with_priority(BannerPriority::Transient)),
        "a lower priority must not displace a higher one"
    );
    assert!(app.set_banner(Banner::danger("offline").with_priority(BannerPriority::Warning)));
    assert_eq!(
        app.banner.as_ref().map(|banner| banner.text.as_str()),
        Some("offline")
    );
    // The condition clearing withdraws only its own banner.
    app.clear_banner(BannerPriority::Tip);
    assert!(app.banner.is_some(), "the warning is not a tip");
    app.clear_banner(BannerPriority::Warning);
    assert!(app.banner.is_none());
}

#[test]
fn the_banner_row_is_published_and_clickable() {
    use vibex_tui::app::{Banner, BannerPriority};
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.set_banner(Banner::danger("the runtime is offline").with_priority(BannerPriority::Warning));
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("the runtime is offline"), "{screen}");
    let rect = app.regions.banner.expect("the banner is clickable");
    assert!(rect.height == 1 && rect.width > 0);
}

#[test]
fn the_frame_publishes_the_regions_the_mouse_needs() {
    use vibex_tui::keymap::Scope;
    let mut app = app(120, 40);
    app.agent
        .apply_sessions(Ok(vec![seeded_session("session_mouse001", "a session")]))
        .expect("sessions apply");
    app.perform(vibex_tui::action::Intent::GotoSessions);
    let _ = render(&mut app, 120, 40);
    let list = app.regions.list.expect("the session list is clickable");
    assert_eq!(list.scope, Scope::Sessions);
    assert!(list.rows > 0 && list.rect.height > 0);
    // A click on the first row maps to row 0; one above the list maps to none.
    assert_eq!(
        vibex_tui::app::list_row_at(&list, list.rect.x + 1, list.rect.y),
        Some(0)
    );
    assert_eq!(
        vibex_tui::app::list_row_at(&list, list.rect.x + 1, list.rect.y.saturating_sub(1)),
        None
    );
    assert!(
        !app.regions.hints.is_empty(),
        "the shortcut band is not clickable"
    );

    // The composer belongs to the session pages, and publishes its text rows
    // there so a click can place the cursor.
    app.navigate_to(Page::Agent);
    let _ = render(&mut app, 120, 40);
    assert!(app.regions.composer.is_some());
}

#[test]
fn the_turn_rail_publishes_a_tick_per_turn() {
    let mut app = transcript_app(120, 44);
    // Two turns, so the rail draws ticks rather than a scrollbar.
    let mut blocks = app.transcript.blocks().to_vec();
    blocks[1].turn_id = Some("turn-2".to_string());
    app.transcript.set_blocks(blocks);
    app.scroll.follow = false;
    app.scroll.offset = 4;
    let _ = render(&mut app, 120, 44);
    assert!(
        !app.regions.turns.is_empty(),
        "the rail published no clickable ticks"
    );
    let (rect, turn) = app.regions.turns[0];
    assert!(rect.height == 1);
    assert!(turn < app.transcript.turn_count());
}

#[test]
fn a_composer_click_places_the_cursor() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text("first line\nsecond line");
    let _ = render(&mut app, 120, 40);
    app.composer.move_cursor_to_cell(0, 5);
    assert_eq!(app.composer.cursor(), 5);
    app.composer.move_cursor_to_cell(1, 3);
    // The second display row starts after the newline.
    assert_eq!(&app.composer.text()[app.composer.cursor()..], "ond line");
}

#[test]
fn a_draft_selection_is_painted_over_the_text() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text("hello world");
    app.composer.move_to_start();
    for _ in 0..5 {
        app.composer.extend_right();
    }
    let buffer = render_buffer(&mut app, 120, 40);
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");

    // Collect the style of two cells in the same row: one inside the
    // selection and one outside it.
    let mut selected = None;
    let mut plain = None;
    for row in region.y..region.bottom() {
        for column in region.x..region.right() {
            let Some(cell) = buffer.cell((column, row)) else {
                continue;
            };
            match cell.symbol() {
                "h" => selected = Some(cell.style()),
                "w" => plain = Some(cell.style()),
                _ => {}
            }
        }
    }
    let (selected, plain) = (
        selected.expect("draft on screen"),
        plain.expect("draft on screen"),
    );
    assert_ne!(
        selected, plain,
        "the selected grapheme is painted like the rest of the draft"
    );
    assert_eq!(app.composer.selected_text().as_deref(), Some("hello"));
}

#[test]
fn a_drag_inside_the_composer_extends_the_draft_selection() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.set_text("first line\nsecond line");
    let _ = render(&mut app, 120, 40);
    let region = app
        .regions
        .composer
        .expect("the composer published its rows");

    // A press on the first cell starts the selection; a drag to column 5 of the
    // same row covers the first word.
    app.composer.move_cursor_to_cell(0, 0);
    app.composer.begin_selection();
    assert!(app.drag_draft_selection(region.x + 5, region.y));
    assert_eq!(app.composer.selected_text().as_deref(), Some("first"));

    // The selection survives a frame, so a highlight the reader made is not
    // erased by the next repaint.
    let _ = render(&mut app, 120, 40);
    assert_eq!(app.composer.selected_text().as_deref(), Some("first"));
}

#[test]
fn the_queue_band_shows_the_cursor_and_its_keys() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_queue0003");
    app.enqueue_message("first held message".to_string());
    app.enqueue_message("second held message".to_string());
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("second held message"), "{screen}");
    assert!(
        screen.contains("Alt+E edit"),
        "the key hints are missing:\n{screen}"
    );
    assert!(screen.contains('▸'), "the cursor is not drawn:\n{screen}");
}

#[test]
fn queue_editing_moves_a_message_back_into_the_draft() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_queue0004");
    app.enqueue_message("first".to_string());
    app.enqueue_message("second".to_string());
    app.queue_selection = Some(1);
    assert!(app.edit_queued_message());
    assert_eq!(app.composer.text(), "second");
    assert_eq!(queued_texts(&app), vec!["first".to_string()]);
    // The draft that was there is not lost: it joins the queue.
    app.composer.set_text("a new draft");
    app.queue_selection = Some(0);
    assert!(app.edit_queued_message());
    assert_eq!(queued_texts(&app), vec!["a new draft".to_string()]);
}

#[test]
fn queue_reordering_and_dropping_keep_the_cursor_sane() {
    let mut app = app(120, 40);
    enter_session(&mut app, "session_queue0005");
    for message in ["one", "two", "three"] {
        app.enqueue_message(message.to_string());
    }
    app.queue_selection = Some(1);
    assert!(app.move_queued_message(-1));
    assert_eq!(queued_texts(&app), vec!["two", "one", "three"]);
    assert_eq!(app.queue_selection, Some(0));
    // At the top, raising again is a no-op rather than a wrap.
    assert!(!app.move_queued_message(-1));
    assert!(app.delete_queued_message());
    assert_eq!(queued_texts(&app), vec!["one", "three"]);
    assert_eq!(app.queue_selection, Some(0));
}

#[test]
fn a_held_message_is_sent_once_the_turn_ends() {
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let session = seeded_session("session_queue0001", "queued work");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session.clone());
    // Idle: the queue drains immediately.
    app.enqueue_message("held".to_string());
    let released = app.drain_queue();
    assert_eq!(
        released.first().map(|(_, text, _)| text.as_str()),
        Some("held")
    );
    assert_eq!(released[0].0, session.id);
    assert!(app.queued_messages.is_empty());

    // Running: nothing drains until the state changes.
    let mut running = session;
    running.state = vibex_core::AgentSessionState::Running;
    app.agent.state.active_session.resolve(running.clone());
    app.enqueue_message("waits".to_string());
    assert!(app.drain_queue().is_empty());
    running.state = vibex_core::AgentSessionState::Idle;
    app.agent.state.active_session.resolve(running.clone());
    app.agent
        .apply_sessions(Ok(vec![running]))
        .expect("sessions apply");
    assert_eq!(
        app.drain_queue().first().map(|(_, text, _)| text.as_str()),
        Some("waits")
    );
}

#[test]
fn the_plan_band_reports_progress_from_the_timeline() {
    // The band reads the projection: a plan update does not earn a transcript
    // row, and the progress must not disappear with it.
    use vibex_desktop_model::TimelineRowKind;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.projection.rows = vec![vibex_desktop_model::TimelineRow {
        id: "todo-1".to_string(),
        kind: TimelineRowKind::TodoUpdate,
        item_ids: Vec::new(),
        turn_id: Some("turn-1".to_string()),
        turn_item_count: 0,
        turn_failed: false,
        turn_pending_permission: false,
        conclusion: false,
        first_sequence: 1,
        last_sequence: 1,
        title: "Ship the plan band".to_string(),
        body: "Completed: read the design\nRunning: write the band\nPending: add a test"
            .to_string(),
        streaming: false,
        collapsible: false,
        pending_permission: false,
        failed: false,
        runtime_attribution: None,
        file_path: None,
    }];
    assert_eq!(app.todo_done_count(), 1);
    assert_eq!(app.todo_total_count(), 3);
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("1/3"),
        "the progress bar is missing:\n{screen}"
    );
    assert!(screen.contains("write the band"), "{screen}");
}

#[test]
fn the_shortcuts_cheatsheet_groups_bindings_by_category() {
    use vibex_tui::keymap::Category;
    let app = app(120, 40);
    let rows = vibex_tui::view::shortcut_rows(&app, "", &std::collections::BTreeSet::new());
    let categories = rows.iter().filter_map(|row| row.header).collect::<Vec<_>>();
    assert_eq!(
        categories,
        Category::ALL,
        "a category is missing or unsorted"
    );
    // Every binding is filed under exactly one header.
    assert!(rows.iter().any(|row| row.binding.is_some()));

    // On screen, the first categories and the fold triangles are visible.
    let mut app = app;
    app.perform(vibex_tui::action::Intent::ToggleHelp);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Global"), "{screen}");
    assert!(screen.contains("Transcript"), "{screen}");
}

#[test]
fn filtering_the_cheatsheet_keeps_only_matching_bindings() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ToggleHelp);
    app.overlay = Some(vibex_tui::app::Overlay::Help {
        query: "palette".to_string(),
        selected: 0,
        collapsed: std::collections::BTreeSet::new(),
    });
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Ctrl+P"), "{screen}");
    assert!(
        !screen.contains("Expand all"),
        "unmatched rows survived:\n{screen}"
    );
}

#[test]
fn folding_a_category_hides_its_bindings_but_keeps_its_header() {
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ToggleHelp);
    let mut collapsed = std::collections::BTreeSet::new();
    collapsed.insert("global".to_string());
    app.overlay = Some(vibex_tui::app::Overlay::Help {
        query: String::new(),
        selected: 0,
        collapsed: collapsed.clone(),
    });
    let screen = text(&render(&mut app, 120, 40));
    // The header stays and is marked folded; the bindings under it are gone.
    assert!(screen.contains("Global"), "the header is gone:\n{screen}");
    assert!(
        screen.contains('▸'),
        "the fold marker is missing:\n{screen}"
    );

    let rows = vibex_tui::view::shortcut_rows(&app, "", &collapsed);
    assert!(
        !rows.iter().any(|row| {
            row.binding.is_some_and(|binding| {
                binding.scope.category() == vibex_tui::keymap::Category::Global
            })
        }),
        "a folded category still contributes rows"
    );
}

#[test]
fn the_palette_groups_commands_and_remembers_recent_ones() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.perform(Intent::OpenCommandPalette);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("View"), "no group headings:\n{screen}");
    assert!(
        screen.contains("Workbench") || screen.contains("Session"),
        "{screen}"
    );

    // Running a command remembers it, and the next open puts it first.
    app.remember_command(Intent::GotoUsage);
    let entries = app.palette_entries("");
    assert_eq!(entries[0].intent, Intent::GotoUsage);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Recent"), "{screen}");
}

#[test]
fn palette_fuzzy_matching_finds_a_two_word_query() {
    let strings = Strings::for_locale(Locale::En);
    let matches = vibex_tui::view::palette_matches("newsess", strings);
    assert!(
        matches.iter().any(|entry| entry.label == "New session"),
        "a subsequence query must match: {:?}",
        matches.iter().map(|entry| entry.label).collect::<Vec<_>>()
    );
}

#[test]
fn a_big_paste_shows_as_one_chip_in_the_composer() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    let log = (0..40)
        .map(|index| format!("2025-09-30 INFO line {index}"))
        .collect::<Vec<_>>()
        .join("\n");
    assert!(app.composer.insert_paste(&log), "40 lines is a chip");
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("[Pasted: 40 lines]"),
        "the chip is not drawn:\n{screen}"
    );
    assert!(
        !screen.contains("INFO line 7"),
        "the paste is not collapsed:\n{screen}"
    );
    // What is sent is the bytes, not the label.
    assert!(app.composer.expanded_text().contains("INFO line 7"));
}

#[test]
fn the_history_drawer_lists_and_recalls_sent_messages() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    for entry in [
        "fix the flaky upload test",
        "add a retry to the upload path",
        "write the release notes",
    ] {
        app.history.push(entry);
    }
    app.composer.set_text("? upload");
    app.sync_composer_mode();
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("History"), "no drawer:\n{screen}");
    assert!(screen.contains("2"), "the count is missing:\n{screen}");
    assert!(screen.contains("fix the flaky upload test"), "{screen}");
    assert!(!screen.contains("write the release notes"), "{screen}");

    // Newest first, so the more recent match is the drawer's first row.
    let matches = app.history_matches();
    assert_eq!(matches[0].1, "add a retry to the upload path");
    app.move_history_selection(1);
    assert!(app.accept_history_match());
    assert_eq!(app.composer.text(), "fix the flaky upload test");
    assert_eq!(app.composer_mode, vibex_tui::app::ComposerMode::Normal);
}

#[test]
fn the_welcome_screen_orders_the_first_run_steps() {
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(vibex_tui::action::Intent::GotoSessions);
    let screen = text(&render(&mut app, 120, 40));
    let positions = [
        "Connect to the runtime",
        "Choose where the Agent works",
        "Start a session",
        "Write the first message",
    ]
    .map(|needle| {
        screen
            .find(needle)
            .unwrap_or_else(|| panic!("the guide lost {needle}:\n{screen}"))
    });
    assert!(
        positions.windows(2).all(|pair| pair[0] < pair[1]),
        "the steps are out of order:\n{screen}"
    );
    // The first incomplete step is the one that explains itself.
    assert!(screen.contains("Browse directories"), "{screen}");
}

#[test]
fn the_first_run_guide_retires_once_every_step_is_done() {
    let mut app = app(120, 40);
    let session = seeded_session("session_onboard01", "a session");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.active_session.resolve(session);
    app.transcript.set_blocks(vec![seeded_block(
        "block-1",
        vibex_desktop_model::TimelineRowKind::UserMessage,
        "the first message",
    )]);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(vibex_tui::action::Intent::GotoSessions);
    assert!(app.onboarding_complete(), "every step is satisfied");
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        !screen.contains("Getting started"),
        "the guide must retire when it has nothing left to say:\n{screen}"
    );
}

#[test]
fn a_prepended_history_page_keeps_the_readers_viewport_anchored() {
    let session_id = vibex_core::VibexSessionId::new();
    let mut app = app(100, 24);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    // The reader has hydrated the newest five items and scrolled to the top.
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), seeded_items(&session_id, 6..=10));
    app.agent.state.timeline_has_older = true;
    app.sync_transcript();
    app.scroll.follow = false;
    app.scroll.offset = 0;

    let anchor = app
        .transcript
        .block(0)
        .expect("the window has a first block")
        .id
        .clone();
    let anchor_text = app
        .transcript
        .block(0)
        .expect("the window has a first block")
        .body
        .clone();
    let before = render(&mut app, 100, 24);
    assert!(
        text(&before).contains(&anchor_text),
        "the oldest loaded item starts the viewport:\n{}",
        text(&before)
    );

    // One page of older history arrives through the controller's real apply
    // path. The ticket is built by hand because this app talks to the
    // disconnected backend, which cannot issue one.
    let ticket = vibex_ui::AgentTimelineBeforeTicket {
        generation: app.agent.state.generation,
        session_id: session_id.clone(),
        before_sequence: 6,
    };
    let page = vibex_core::TimelinePage {
        session_id: session_id.clone(),
        items: seeded_items(&session_id, 1..=5),
        start_sequence: Some(1),
        end_sequence: Some(5),
        has_older: false,
        has_newer: true,
    };
    assert!(
        app.agent
            .apply_timeline_before(&ticket, Ok(page))
            .expect("the page is valid")
    );
    app.sync_transcript();

    // The prepend grew the content above the viewport, so the line offset had
    // to move with it; an unanchored offset would have jumped back to the
    // newly fetched first item.
    assert!(
        app.scroll.offset > 0,
        "prepending history must shift the viewport down"
    );
    let anchored = app
        .transcript
        .block_at_line(app.scroll.offset)
        .and_then(|index| app.transcript.block(index))
        .map(|block| block.id.clone());
    assert_eq!(anchored, Some(anchor));

    let after = render(&mut app, 100, 24);
    let screen = text(&after);
    assert!(
        screen.contains(&anchor_text),
        "the block the reader was looking at must stay on screen:\n{screen}"
    );
    // The pinned prompt is the row *above* the viewport, so the last fetched
    // message may legitimately appear there; the page itself must not. The
    // match is on a line ending, because "message 1" is inside "message 10".
    for fetched in [
        "history message 1",
        "history message 2",
        "history message 3",
        "history message 4",
    ] {
        assert!(
            !after.iter().any(|line| line.trim_end().ends_with(fetched)),
            "{fetched} landed in the viewport instead of above it:\n{screen}"
        );
    }
    // The older page really is loaded: it is what the reader now scrolls into.
    assert_eq!(app.agent.state.timeline_oldest_sequence(), Some(1));
    assert!(!app.agent.state.timeline_has_older);
}

/// A timeline item with every field the wire requires.
fn seeded_item(
    session_id: &vibex_core::VibexSessionId,
    sequence: i64,
    kind: vibex_core::TimelineItemKind,
    payload: vibex_core::TimelinePayload,
) -> vibex_core::TimelineItem {
    vibex_core::TimelineItem {
        id: vibex_core::TimelineItemId::new(),
        session_id: session_id.clone(),
        sequence,
        timestamp_ms: 1_000 + sequence,
        source: vibex_core::TimelineSource::Agent,
        kind,
        correlation_id: Some(vibex_core::CorrelationId::new()),
        provider_correlation_id: None,
        redaction_state: vibex_core::TimelineRedactionState::None,
        execution_attribution: None,
        payload,
    }
}

#[test]
fn a_tool_heavy_turn_stays_a_short_run_of_rows() {
    // The density contract: a dozen timeline events are a dozen *lines* of
    // transcript, not a dozen sections. Each tool call is one row, a run of
    // them folds into its first, and the bookkeeping rows (a plan update, an
    // approval's resolution) are not drawn at all.
    let session_id = vibex_core::VibexSessionId::new();
    let mut app = app(110, 40);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    let mut items = vec![
        seeded_item(
            &session_id,
            1,
            vibex_core::TimelineItemKind::UserMessage,
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: "Fix the flaky test in the runner".into(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        ),
        seeded_item(
            &session_id,
            2,
            vibex_core::TimelineItemKind::SystemNotice,
            vibex_core::TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                message: "session resumed from disk".into(),
                level: vibex_core::SystemNoticeLevel::Info,
            }),
        ),
        seeded_item(
            &session_id,
            3,
            vibex_core::TimelineItemKind::AgentMessage,
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "I will look at the runner tests first.".into(),
                is_final: true,
            }),
        ),
    ];
    for (offset, (tool, argument)) in [
        ("read_file", "crates/runner/src/lib.rs"),
        ("read_file", "crates/runner/src/tests.rs"),
        ("grep", "flaky"),
        ("read_file", "crates/runner/src/scheduler.rs"),
        ("write_file", "crates/runner/src/scheduler.rs"),
    ]
    .into_iter()
    .enumerate()
    {
        items.push(seeded_item(
            &session_id,
            4 + offset as i64,
            vibex_core::TimelineItemKind::ToolCall,
            vibex_core::TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
                tool_call_id: format!("call-{offset}"),
                tool_name: tool.into(),
                status: vibex_core::ToolCallStatus::Completed,
                summary: String::new(),
                input_summary: Some(argument.into()),
                output_summary: Some("412 lines".into()),
                raw_extension: None,
            }),
        ));
    }
    items.push(seeded_item(
        &session_id,
        9,
        vibex_core::TimelineItemKind::TodoUpdate,
        vibex_core::TimelinePayload::TodoUpdate(vibex_core::TodoUpdatePayload {
            title: "fix flaky test".into(),
            items: Vec::new(),
            raw_extension: None,
        }),
    ));
    items.push(seeded_item(
        &session_id,
        10,
        vibex_core::TimelineItemKind::PermissionResolution,
        vibex_core::TimelinePayload::PermissionResolution(vibex_core::PermissionResolution {
            request_id: vibex_core::RequestId::new(),
            session_id: session_id.clone(),
            response: vibex_core::PermissionResponseKind::Approve,
            responder_device_id: None,
            provider_resolution_id: None,
            note: None,
            resolved_at_ms: 1_000,
        }),
    ));
    items.push(seeded_item(
        &session_id,
        11,
        vibex_core::TimelineItemKind::AgentMessage,
        vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: "Done: the test now uses the fake clock.".into(),
            is_final: true,
        }),
    ));
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id, items);
    app.sync_transcript();

    // Nine of the eleven items are conversation or the one notice worth
    // keeping; the plan update and the approval's resolution are the dock's
    // and the request row's business.
    assert_eq!(app.transcript.blocks().len(), 9);
    let screen = text(&render(&mut app, 110, 40));
    assert!(
        screen.contains("❯ Fix the flaky test in the runner"),
        "{screen}"
    );
    assert!(screen.contains("session resumed from disk"), "{screen}");
    assert!(
        screen.contains("Done: the test now uses the fake clock."),
        "{screen}"
    );
    // Five tool calls read as one row that says how many it stands for.
    assert!(screen.contains("read_file  +4"), "{screen}");
    assert!(
        !screen.contains("Tool read_file"),
        "the kind label doubles the title:\n{screen}"
    );
    assert!(
        !screen.contains("Plan ") && !screen.contains("Permission"),
        "bookkeeping rows reached the transcript:\n{screen}"
    );
    // The whole turn fits in a screen and a half, where one row per event plus
    // a body for each section would not.
    assert!(
        app.transcript.total_height() <= 14,
        "the turn costs {} rows",
        app.transcript.total_height()
    );
}

#[test]
fn the_plan_band_reports_a_plan_the_transcript_does_not_draw() {
    // A plan update is bookkeeping: it has no transcript row, and the band that
    // summarises it has to read the projection instead — otherwise silencing
    // the row would silence the progress with it.
    let session_id = vibex_core::VibexSessionId::new();
    let mut app = app(110, 40);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    app.agent.state.timeline.replace_authoritative(
        session_id.clone(),
        vec![seeded_item(
            &session_id,
            1,
            vibex_core::TimelineItemKind::TodoUpdate,
            vibex_core::TimelinePayload::TodoUpdate(vibex_core::TodoUpdatePayload {
                title: "fix the flaky test".into(),
                items: vec![
                    vibex_core::PlanStepPayload {
                        title: "read the runner tests".into(),
                        status: vibex_core::PlanStepStatus::Completed,
                    },
                    vibex_core::PlanStepPayload {
                        title: "use the fake clock".into(),
                        status: vibex_core::PlanStepStatus::Running,
                    },
                ],
                raw_extension: None,
            }),
        )],
    );
    app.sync_transcript();

    assert!(
        app.transcript.blocks().is_empty(),
        "a plan update earned a transcript row: {:?}",
        app.transcript
            .blocks()
            .iter()
            .map(|block| block.kind)
            .collect::<Vec<_>>()
    );
    assert_eq!(app.todo_total_count(), 2);
    assert_eq!(app.todo_done_count(), 1);
    let screen = text(&render(&mut app, 110, 40));
    assert!(
        screen.contains("fix the flaky test") || screen.contains("1/2"),
        "the plan band lost the plan:\n{screen}"
    );
}

#[test]
fn a_streamed_answer_grows_frame_by_frame() {
    // The streaming contract: every delta that arrives is on screen in the
    // frame that follows it, and the block only ever grows.
    let session_id = vibex_core::VibexSessionId::new();
    let mut app = app(100, 30);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), Vec::new());

    let mut heights = Vec::new();
    for (sequence, (delta, visible)) in [
        ("# Report\n\n", "Report"),
        ("The runner test", "The runner test"),
        (" is flaky because", "is flaky because"),
        (" it reads the wall clock.\n\n", "wall clock"),
        ("The fix uses the fake clock.\n", "fake clock"),
    ]
    .into_iter()
    .enumerate()
    {
        let sequence = sequence as i64 + 1;
        let applied = app.agent.apply_event(vibex_backend::BackendEvent::Timeline(
            vibex_core::TimelineLiveEvent {
                session_id: session_id.clone(),
                sequence,
                item: seeded_item(
                    &session_id,
                    sequence,
                    vibex_core::TimelineItemKind::AgentMessage,
                    vibex_core::TimelinePayload::AgentMessageDelta(
                        vibex_core::AgentMessageDeltaPayload {
                            text_delta: delta.to_string(),
                            chunk_index: sequence as u32 - 1,
                            phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
                        },
                    ),
                ),
            },
        ));
        assert_eq!(applied, vibex_ui::AgentEventDecision::Applied);
        app.sync_transcript();
        let screen = text(&render(&mut app, 100, 30));
        // Wide glyph spacing and the rail make an exact match fragile, so the
        // check is on the words the reader would see.
        assert!(
            screen.contains(visible),
            "{visible:?} is missing from the frame after {delta:?}:\n{screen}"
        );
        heights.push(app.transcript.total_height());
    }
    // The block grows by what arrived, never shrinks: a frame that re-laid the
    // whole body out would still pass this, which is why the incremental
    // renderer has its own test.
    assert!(
        heights.windows(2).all(|pair| pair[0] <= pair[1]),
        "the block shrank while streaming: {heights:?}"
    );
    assert!(heights.last() > heights.first(), "{heights:?}");
}

#[test]
fn a_clipboard_image_paste_becomes_an_attachment_not_a_draft() {
    // The route the client's own clipboard reader takes: the worker reports
    // bytes and the composer shows a chip. The chip's label is part of the
    // draft — it is what the reader sees and can delete — but the *path* of a
    // pasted picture never becomes prose the Agent reads.
    let mut app = app(110, 30);
    app.navigate_to(Page::Agent);
    assert!(app.composer.text().is_empty());

    let label = app
        .attach_image_bytes("image/png", vec![0x89, b'P', b'N', b'G'])
        .expect("clipboard bytes attach");
    assert!(label.starts_with("[Image #"), "{label}");
    assert_eq!(app.composer.image_count(), 1);
    assert_eq!(app.composer.text(), label);

    let directory = tempfile::tempdir().expect("temp dir");
    let shot = directory.path().join("shot.png");
    std::fs::write(&shot, b"png").expect("write");
    app.insert_pasted_text(&shot.display().to_string());
    assert_eq!(app.composer.image_count(), 2);
    assert!(
        !app.composer.text().contains("shot.png"),
        "the pasted path leaked into the draft: {:?}",
        app.composer.text()
    );

    // Text still goes into the draft, after the chips.
    app.insert_pasted_text("a pasted sentence");
    assert!(
        app.composer.text().ends_with("a pasted sentence"),
        "{:?}",
        app.composer.text()
    );

    // And the chips are what the screen shows.
    let screen = text(&render(&mut app, 110, 30));
    assert!(
        screen.contains("[Image #1]"),
        "the attachment is not on screen:\n{screen}"
    );
}

#[test]
fn a_finished_turn_stops_claiming_to_stream() {
    let session_id = vibex_core::VibexSessionId::new();
    let correlation = vibex_core::CorrelationId::new();
    let mut user = seeded_item(
        &session_id,
        1,
        vibex_core::TimelineItemKind::UserMessage,
        vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
            text: "question".into(),
            attachments: Vec::new(),
            ..Default::default()
        }),
    );
    user.correlation_id = Some(correlation.clone());
    let mut delta = seeded_item(
        &session_id,
        2,
        vibex_core::TimelineItemKind::AgentMessage,
        vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
            text_delta: "the answer".into(),
            chunk_index: 0,
            phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
        }),
    );
    delta.correlation_id = Some(correlation.clone());

    let build = |state: vibex_core::AgentSessionState| {
        let mut app = app(120, 30);
        app.navigate_to(Page::Agent);
        app.agent.state.selected_session_id = Some(session_id.clone());
        let mut session = seeded_session("session_probe", "probe");
        session.id = session_id.clone();
        session.state = state;
        app.agent
            .apply_sessions(Ok(vec![session.clone()]))
            .expect("apply");
        app.agent.state.active_session.resolve(session);
        app.agent
            .state
            .timeline
            .replace_authoritative(session_id.clone(), vec![user.clone(), delta.clone()]);
        app.sync_transcript();
        app
    };

    // The runtime says the turn is over, so the row cannot still be arriving:
    // a client that believed it would draw a spinner over a finished answer.
    let mut settled = build(vibex_core::AgentSessionState::Idle);
    assert!(
        !settled.transcript_animating(),
        "a settled turn still animates"
    );
    let screen = text(&render(&mut settled, 120, 30));
    assert!(
        !screen.contains("运行中") && !screen.contains("Running"),
        "a settled session is drawn as running:\\n{screen}"
    );

    // While the runtime says it is running, the same rows do stream.
    let mut running = build(vibex_core::AgentSessionState::Running);
    assert!(
        running.transcript_animating(),
        "a running turn stopped streaming"
    );
    let screen = text(&render(&mut running, 120, 30));
    assert!(
        screen.contains("运行中") || screen.contains("Running"),
        "a running session is not drawn as running:\\n{screen}"
    );
}

#[test]
fn scrolling_down_stops_at_the_bottom_of_the_session() {
    use vibex_desktop_model::TimelineRowKind;
    let mut app = transcript_app(100, 24);
    app.transcript.set_blocks(
        (0..12)
            .map(|index| {
                let mut block = seeded_block(
                    &format!("block-{index}"),
                    TimelineRowKind::AgentMessage,
                    &format!("message {index} with enough words to wrap at least once"),
                );
                block.collapsible = false;
                block
            })
            .collect(),
    );
    // One frame publishes the band height the clamp is measured against.
    let _ = render(&mut app, 100, 24);

    // A reader holding the wheel down walks the offset into blank space: the
    // offset has no ceiling of its own, so each click went further past the end.
    for _ in 0..200 {
        app.scroll_lines(3);
    }
    let rows = app.transcript_band_rows;
    let total = app.transcript.total_height();
    assert!(
        total > rows,
        "the fixture must outgrow the band: {total} rows"
    );
    assert_eq!(
        app.scroll.offset,
        total - rows,
        "the viewport is not resting on the bottom"
    );
    assert!(app.scroll.follow, "reaching the bottom resumes following");

    // The band ends on the last line of the session rather than on blank rows.
    let screen = text(&render(&mut app, 100, 24));
    assert!(screen.contains("message 11"), "{screen}");
    let band = app.regions.scrollback;
    let rows_on_screen = screen.lines().collect::<Vec<_>>();
    let band_rows = rows_on_screen[usize::from(band.y)..usize::from(band.y + band.height)].to_vec();
    let last_content = band_rows
        .iter()
        .rev()
        .find(|line| !line.trim().is_empty())
        .expect("the band has content");
    assert!(
        last_content.contains("message 11"),
        "the band ends below the last message:\n{screen}"
    );
}

#[test]
fn a_new_session_shows_one_tick_per_turn_and_nothing_when_empty() {
    use vibex_desktop_model::TimelineRowKind;
    let rail = |app: &mut App, width: u16, rows: u16| {
        let buffer = render_buffer(app, width, rows);
        (0..rows)
            .filter(|row| {
                (0..width).any(|column| {
                    buffer
                        .cell((column, *row))
                        .is_some_and(|cell| matches!(cell.symbol(), "•" | "▪" | "┃"))
                })
            })
            .count()
    };

    // A session with nothing in it has nothing to draw on its rail: a bar
    // beside an empty transcript is a mark the reader counts for a turn that
    // does not exist.
    let mut empty = app(100, 24);
    empty.navigate_to(Page::Agent);
    assert_eq!(rail(&mut empty, 100, 24), 0, "an empty session drew a rail");

    // One turn is one tick — including when it is the turn the reader is on.
    let mut single = app(100, 24);
    single.navigate_to(Page::Agent);
    single.transcript.set_blocks(vec![
        seeded_block("a", TimelineRowKind::UserMessage, "hello"),
        seeded_block("b", TimelineRowKind::AgentMessage, "hi there"),
    ]);
    assert_eq!(rail(&mut single, 100, 24), 1, "one turn is not one tick");

    // Two turns are two ticks, in conversation order.
    let mut pair = app(100, 24);
    pair.navigate_to(Page::Agent);
    pair.transcript.set_blocks(
        [(0, "first"), (1, "second")]
            .into_iter()
            .flat_map(|(turn, text)| {
                [TimelineRowKind::UserMessage, TimelineRowKind::AgentMessage]
                    .into_iter()
                    .enumerate()
                    .map(move |(index, kind)| {
                        let mut block = seeded_block(&format!("{turn}-{index}"), kind, text);
                        block.turn_id = Some(format!("turn-{turn}"));
                        block
                    })
            })
            .collect(),
    );
    assert_eq!(rail(&mut pair, 100, 24), 2, "two turns are not two ticks");
}

#[test]
fn the_first_scroll_after_opening_a_session_is_a_step_not_a_leap() {
    use vibex_desktop_model::TimelineRowKind;
    let mut app = app(100, 24);
    app.navigate_to(Page::Agent);
    app.transcript.set_blocks(
        (0..8)
            .flat_map(|turn| {
                [TimelineRowKind::UserMessage, TimelineRowKind::AgentMessage]
                    .into_iter()
                    .enumerate()
                    .map(move |(index, kind)| {
                        let mut block = seeded_block(
                            &format!("turn{turn}-{index}"),
                            kind,
                            &format!("turn {turn} row {index} with text in it"),
                        );
                        block.turn_id = Some(format!("turn-{turn}"));
                        block.collapsible = false;
                        block
                    })
            })
            .collect(),
    );
    // Opening a session follows its tail.
    let _ = render(&mut app, 100, 24);
    let rows = app.transcript_band_rows;
    let bottom = app.transcript.total_height() - rows;
    assert!(app.scroll.follow);
    assert_eq!(
        app.scroll.offset, bottom,
        "the state's offset is not where the frame is"
    );

    // One scroll up is one step up. Stepping from the offset the state was last
    // dragged to — zero, for a viewport that has only ever followed — threw the
    // reader into a different turn at the top of the session.
    app.scroll_lines(-3);
    assert_eq!(
        app.scroll.offset,
        bottom - 3,
        "the first scroll up jumped instead of stepping"
    );
    assert!(!app.scroll.follow, "scrolling up still follows the tail");

    // And one step down from the top of a session stops there.
    app.scroll_lines(-10_000);
    assert_eq!(app.scroll.offset, 0);
    app.scroll_lines(3);
    assert_eq!(app.scroll.offset, 3);
}

#[test]
fn the_frames_click_regions_do_not_accumulate() {
    // Rect coordinates are per-frame: the previous frame's rows describe a
    // layout that no longer exists, and a list that only ever grows both leaks
    // and lets a click land on a row that has moved.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    let _ = render(&mut app, 120, 40);
    let hints = app.regions.hints.len();
    let turns = app.regions.turns.len();
    for _ in 0..3 {
        let _ = render(&mut app, 120, 40);
    }
    assert!(hints > 0, "the frame published no hints");
    assert_eq!(app.regions.hints.len(), hints, "the hint list grew");
    assert_eq!(app.regions.turns.len(), turns, "the turn list grew");

    // A session with turns publishes one rect per visible tick, and no more.
    app.transcript.set_blocks(
        (0..3)
            .map(|turn| {
                let mut block = seeded_block(
                    &format!("t{turn}"),
                    vibex_desktop_model::TimelineRowKind::AgentMessage,
                    "body",
                );
                block.turn_id = Some(format!("turn-{turn}"));
                block
            })
            .collect(),
    );
    let _ = render(&mut app, 120, 40);
    assert_eq!(app.regions.turns.len(), 3, "one rect per turn");
    let _ = render(&mut app, 120, 40);
    assert_eq!(app.regions.turns.len(), 3, "the turn list grew");
}

/// A pointer can land anywhere, including where nothing is drawn.
///
/// The terminal reports motion for the whole window, so the mouse path has to
/// survive coordinates outside every band. This walks the public entry points
/// with corners and past-the-end values; the bug it guards against was an
/// arithmetic overflow inside a guard that only *looked* like it protected the
/// computation.
#[test]
fn mouse_coordinates_outside_every_band_are_harmless() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    // A held message publishes the queue band, and the dock publishes its own.
    enter_session(&mut app, "session_queue0006");
    app.enqueue_message("held".to_string());
    app.perform(Intent::ToggleDock);
    let _ = render(&mut app, 120, 40);

    for column in [0u16, 1, 60, 119, 120, 400] {
        for row in [0u16, 1, 20, 39, 40, 200] {
            app.begin_text_selection(row as usize, column);
            app.extend_text_selection(row as usize, column);
            let _ = app.finish_text_selection();
            let _ = app.select_word_at(row as usize, column);
            let _ = app.drag_draft_selection(column, row);
            let _ = app.clear_text_selection();
        }
    }
    // The frame still draws, which is the reader-visible half of "harmless".
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("held"), "{screen}");
}

#[test]
fn a_sent_image_keeps_its_place_in_the_message() {
    // The wire contract the desktop draws from: an attachment with no offset is
    // appended at the end of the message, and a URI the reader's client cannot
    // resolve is a label with no picture behind it. Both were wrong in the same
    // way — the picture arrived, but not where it was written and not in a form
    // the desktop could show.
    use vibex_tui::action::Intent;
    let send = |seat: vibex_tui::view::SeatKind| {
        let mut app = app(100, 30);
        app.navigate_to(Page::Agent);
        app.live = vibex_tui::app::LiveState::Ready;
        app.seat = seat;
        let session = seeded_session("session_image0001", "image message");
        app.agent
            .apply_sessions(Ok(vec![session.clone()]))
            .expect("sessions apply");
        app.agent.state.selected_session_id = Some(session.id.clone());
        app.agent.state.active_session.resolve(session);
        app.composer.insert_str("before ");
        let label = app
            .attach_image_bytes("image/png", vec![0x89, b'P', b'N', b'G'])
            .expect("the image attaches");
        app.composer.insert_str(" after");
        let outcome = app.perform(Intent::SubmitComposer);
        let effect = outcome
            .effects
            .into_iter()
            .find(|effect| matches!(effect, vibex_tui::Effect::SendMessage { .. }))
            .expect("the message was sent");
        match effect {
            vibex_tui::Effect::SendMessage {
                text, attachments, ..
            } => (text, attachments, label),
            _ => unreachable!(),
        }
    };

    // The authority writes the clipboard's bytes beside the message, because
    // the runtime that has to read them is this host and a path is the only
    // form the desktop can draw.
    let (text, attachments, label) = send(vibex_tui::view::SeatKind::Authority);
    assert_eq!(text, "before  after", "the label stays out of the text");
    assert!(!text.contains(&label));
    assert_eq!(attachments.len(), 1);
    assert_eq!(
        attachments[0].inline_text_offset,
        Some(7),
        "the picture was not placed where it was written"
    );
    let uri = attachments[0].uri.clone().expect("a uri");
    let path = uri
        .strip_prefix("file://")
        .unwrap_or_else(|| panic!("a client cannot draw {uri}"));
    assert_eq!(
        std::fs::read(path).expect("the bytes were written beside the message"),
        vec![0x89, b'P', b'N', b'G']
    );
    let _ = std::fs::remove_file(path);

    // A remote seat has no file the runtime can reach, so the bytes travel with
    // the message — but the place is still named.
    let (_, attachments, _) = send(vibex_tui::view::SeatKind::Remote);
    assert_eq!(attachments[0].inline_text_offset, Some(7));
    assert!(
        attachments[0]
            .uri
            .as_deref()
            .is_some_and(|uri| uri.starts_with("data:image/png;base64,")),
        "{:?}",
        attachments[0].uri
    );
}

#[test]
fn starting_a_session_lands_on_a_page_with_the_prompt_in_it() {
    // `n` used to open a dialog asking what to call the session. A reader who
    // asked for a session asked to *write*, so the gesture now lands on a page
    // that hands them the composer.
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);

    assert_eq!(app.page, vibex_tui::app::Page::NewSession);
    assert!(
        app.overlay.is_none(),
        "a dialog was opened: {:?}",
        app.overlay
    );
    assert_eq!(app.focus, vibex_tui::app::Focus::Composer);

    let screen = text(&render(&mut app, 100, 30));
    assert!(screen.contains("██"), "the mark is missing:\n{screen}");
    assert!(
        screen.contains("New session"),
        "the page does not name itself:\n{screen}"
    );
    assert!(
        screen.contains("Send a message"),
        "the prompt is not on the page:\n{screen}"
    );
    // The runtime the message will go through is named on the page *and* on the
    // composer's own line, because choosing it is what the page is for.
    assert!(
        screen.contains("Ctrl+G"),
        "the runtime switch is not offered:\n{screen}"
    );
    assert!(
        screen.contains("Commands") && screen.contains("Files"),
        "the draft's vocabulary is not spelled out:\n{screen}"
    );
}

#[test]
fn the_first_message_creates_the_session_it_is_written_in() {
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    // The page starts with no directory of its own: one chosen for a previous
    // session is not silently reused. The reader picks one, or the open
    // session's directory answers.
    assert_ne!(app.new_session_workspace(), "/tmp/vibex-new-session");
    app.workspace_path = Some("/tmp/vibex-new-session".to_string());
    assert_eq!(app.new_session_workspace(), "/tmp/vibex-new-session");
    app.composer.insert_str("fix the flaky test");

    let outcome = app.perform(Intent::SubmitComposer);
    let created = outcome
        .effects
        .iter()
        .find_map(|effect| match effect {
            vibex_tui::Effect::CreateSession {
                workspace_root,
                title,
                runtime,
            } => Some((workspace_root.clone(), title.clone(), runtime.clone())),
            _ => None,
        })
        .expect("no session was asked for");
    assert_eq!(created.0, "/tmp/vibex-new-session");
    assert_eq!(
        created.1, None,
        "the reader is asked to name a session they have already described"
    );
    assert_eq!(
        created.2, None,
        "a runtime was invented for a page that chose none"
    );
    // Empty draft: the same key says so instead of creating an empty session.
    assert!(
        app.composer.text().is_empty(),
        "the draft stayed in the box after being handed over"
    );

    // The session answers; the held message opens it.
    let session_id = vibex_core::VibexSessionId::new();
    let effect = app
        .pending_send_effect(session_id.clone())
        .expect("the message that asked for the session is sent");
    match effect {
        vibex_tui::Effect::SendMessage {
            session_id: sent_to,
            text,
            ..
        } => {
            assert_eq!(sent_to, session_id);
            assert_eq!(text, "fix the flaky test");
        }
        other => panic!("unexpected effect: {other:?}"),
    }
    // Only once: the held message is not a queue.
    assert!(app.pending_send_effect(session_id).is_none());
}

#[test]
fn leaving_the_composing_page_keeps_what_was_written() {
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    app.composer.insert_str("half a thought");
    app.perform(Intent::Back);
    assert_eq!(app.page, vibex_tui::app::Page::Sessions);
    assert_eq!(
        app.composer.text(),
        "half a thought",
        "cancelling threw the draft away"
    );
}

#[test]
fn the_mark_moves_only_where_it_is_drawn() {
    use vibex_tui::action::Intent;
    let theme = vibex_tui::theme::TuiTheme::resolve(
        Some("vibex-dark"),
        vibex_ui::GpuiThemeMode::Dark,
        vibex_tui::ColorCapability {
            mode: vibex_tui::ColorMode::TrueColor,
            glyphs: vibex_tui::GlyphMode::Unicode,
        },
    );
    // The sweep is a lit band, so two phases cannot draw the same mark.
    let first = vibex_tui::logo::rows(&theme, 0, true);
    let later = vibex_tui::logo::rows(&theme, 20, true);
    assert_ne!(text_of(&first), text_of(&later), "the mark does not move");
    // The letters never change, only the light on them.
    assert_eq!(
        text_of(&vibex_tui::logo::rows(&theme, 0, false)),
        text_of(&vibex_tui::logo::rows(&theme, 40, false))
    );

    // A page that waits animates; every other page still holds still.
    let mut app = app(100, 30);
    assert!(!app.chrome_animating());
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    assert!(
        app.chrome_animating(),
        "the page that waits does not breathe"
    );
    assert!(app.advance_transcript_animation());
    app.perform(Intent::Back);
    assert!(!app.chrome_animating());
    assert!(!app.advance_transcript_animation());
}

/// The mark with its styling: the sweep changes the light, not the letters.
fn text_of(lines: &[ratatui::text::Line<'static>]) -> String {
    lines
        .iter()
        .map(|line| format!("{line:?}"))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_composing_page_chooses_the_runtime_the_session_is_born_with() {
    use vibex_tui::action::Intent;
    let option = |agent: &str, model: &str| vibex_core::SessionRuntimeOption {
        selection: vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse(agent).expect("agent id"),
            vibex_core::ProviderProfileId::new(),
            model,
        ),
        agent_label: agent.to_string(),
        auth_source_label: "bal".to_string(),
        model_label: model.to_string(),
        reasoning_efforts: Vec::new(),
        modes: Vec::new(),
        features: Vec::new(),
        availability: vibex_core::RuntimeOptionAvailability::Available,
    };
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![option("claude", "claude-sonnet"), option("codex", "gpt-5")],
    });
    app.perform(Intent::NewSession);

    // The picker opens on the page, and the choice is kept for the session
    // rather than needing one to exist first. (The session list's own key is
    // gated on the backend supporting a switch, which a client with no backend
    // cannot; the overlay and its confirm are what this test is about.)
    app.show_runtime_picker();
    app.perform(Intent::SelectNext);
    app.perform(Intent::ConfirmOverlay);
    assert!(app.overlay.is_none(), "the picker stayed open");
    assert_eq!(
        app.new_session_runtime.as_ref().map(|selection| selection
            .model
            .model_id()
            .unwrap_or_default()
            .to_string()),
        Some("gpt-5".to_string())
    );

    // Writing and sending creates the session with that runtime.
    app.composer.insert_str("write something");
    let outcome = app.perform(Intent::SubmitComposer);
    let runtime = outcome
        .effects
        .iter()
        .find_map(|effect| match effect {
            vibex_tui::Effect::CreateSession { runtime, .. } => Some(runtime.clone()),
            _ => None,
        })
        .expect("no session was asked for")
        .expect("the page's choice was dropped");
    assert_eq!(
        runtime.model.model_id(),
        Some("gpt-5"),
        "the session is created on the page's runtime"
    );
}

#[test]
fn the_composing_page_can_choose_the_directory_it_works_in() {
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    app.composer.insert_str("keep me");

    // The browser is a picker over the listing the runtime sent, not a text
    // field: the directory has to exist on the runtime's host, which this
    // client cannot check for itself.
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home/peatboy".to_string(),
        parent: Some("/home".to_string()),
        entries: vec![
            vibex_core::RemoteWorkspaceDirectoryEntry {
                name: "code".to_string(),
                path: "/home/peatboy/code".to_string(),
            },
            vibex_core::RemoteWorkspaceDirectoryEntry {
                name: "notes".to_string(),
                path: "/home/peatboy/notes".to_string(),
            },
        ],
    });
    app.perform(Intent::OpenWorkspaceBrowser);
    assert!(matches!(
        app.overlay,
        Some(vibex_tui::app::Overlay::WorkspacePicker { .. })
    ));
    let screen = text(&render(&mut app, 100, 30));
    assert!(
        screen.contains("code"),
        "the listing is not drawn:\n{screen}"
    );
    assert!(screen.contains("notes"), "{screen}");

    app.perform(Intent::SelectNext);
    app.perform(Intent::ConfirmOverlay);
    assert!(app.overlay.is_none(), "the picker stayed open");
    assert_eq!(
        app.workspace_path.as_deref(),
        Some("/home/peatboy/notes"),
        "the highlighted directory was not chosen"
    );
    assert_eq!(app.page, vibex_tui::app::Page::NewSession);
    assert_eq!(app.composer.text(), "keep me", "the draft was lost");
    let screen = text(&render(&mut app, 100, 30));
    assert!(
        screen.contains("notes"),
        "the chosen directory is not named on the page:\n{screen}"
    );
}

#[test]
fn the_composing_page_owns_the_keyboard_it_shows() {
    // The composer's text path is tried before the binding table, but only for
    // pages whose scope list starts with `Composer`. On the page where a
    // session is written the list did not: a printable key found the *Agent*
    // scope instead, where `/` is "search the transcript" — so typing did
    // nothing and `/` left the page for a session the reader had not opened.
    use vibex_tui::action::Intent;
    use vibex_tui::keymap::Scope;
    let chord = |character: char| {
        vibex_tui::keymap::Chord::plain(ratatui::crossterm::event::KeyCode::Char(character))
    };

    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    assert_eq!(app.focus, vibex_tui::app::Focus::Composer);

    let scopes = app.active_scopes();
    assert_eq!(
        scopes.first(),
        Some(&Scope::Composer),
        "the composer does not own the keyboard on its own page: {scopes:?}"
    );
    assert_eq!(
        app.documented_scopes().first(),
        Some(&Scope::Composer),
        "the key bar advertises another page's keys"
    );
    // The text path is tried before the table, so `/` is a character the
    // composer inserts — but the table's own meaning for it is checked here,
    // because that is what a reader on another page would get.
    assert_eq!(
        app.keymap.resolve(&[Scope::Agent], chord('/')),
        Some(Intent::BeginTranscriptSearch)
    );
    // And that meaning no longer moves the reader: with no transcript to
    // search, `/` says so where they are.
    let before = app.page;
    app.perform(Intent::BeginTranscriptSearch);
    assert_eq!(app.page, before, "`/` navigated away from an empty page");
    assert!(app.search.is_none());
}

#[test]
fn a_message_held_for_one_session_waits_for_that_session() {
    // The queue used to be one list for the whole client. A message written
    // while session A's turn ran was released into whatever session the reader
    // happened to be looking at when that turn ended — so leaving and coming
    // back found the queue gone, and the message was sent somewhere it was
    // never written for.
    use vibex_core::AgentSessionState;
    let state_of = |session: &vibex_core::AgentSession, state| {
        let mut copy = session.clone();
        copy.state = state;
        copy
    };
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let first = seeded_session("session_queue0100", "first");
    let second = seeded_session("session_queue0101", "second");
    let running = state_of(&first, AgentSessionState::Running);
    let settled = state_of(&first, AgentSessionState::Idle);
    app.agent
        .apply_sessions(Ok(vec![running.clone(), second.clone()]))
        .expect("sessions apply");

    // Session A is running, so the message is held for it.
    app.agent.state.selected_session_id = Some(running.id.clone());
    app.agent.state.active_session.resolve(running.clone());
    app.enqueue_message("for the first session".to_string());
    assert_eq!(
        queued_texts(&app),
        vec!["for the first session".to_string()]
    );

    // The reader moves to session B, which is idle. A's message is not B's to
    // send, and does not vanish from A's queue.
    app.agent.state.selected_session_id = Some(second.id.clone());
    app.agent.state.active_session.resolve(second.clone());
    assert!(
        queued_texts(&app).is_empty(),
        "another session's queue is shown here"
    );
    assert!(
        app.drain_queue().is_empty(),
        "a message was released into a session it was not written for"
    );
    assert_eq!(app.queued_messages.len(), 1, "the held message was lost");

    // Back on A, with its turn over: the held message is released — into A.
    app.agent
        .apply_sessions(Ok(vec![settled.clone(), second.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(settled.id.clone());
    app.agent.state.active_session.resolve(settled.clone());
    let released = app.drain_queue();
    assert_eq!(released.len(), 1);
    assert_eq!(released[0].0, settled.id, "released into the wrong session");
    assert_eq!(released[0].1, "for the first session");
    assert!(app.queued_messages.is_empty());
}

#[test]
fn a_held_message_is_released_while_the_reader_is_elsewhere() {
    // The other half of the same bug: the message belongs to its session, so it
    // goes out when *that* turn ends — even if the reader has moved on and is
    // looking at another session when it does.
    use vibex_core::AgentSessionState;
    let state_of = |session: &vibex_core::AgentSession, state| {
        let mut copy = session.clone();
        copy.state = state;
        copy
    };
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let busy = seeded_session("session_queue0110", "busy");
    let elsewhere = seeded_session("session_queue0111", "elsewhere");
    let running = state_of(&busy, AgentSessionState::Running);
    let settled = state_of(&busy, AgentSessionState::Idle);
    let busy_elsewhere = state_of(&elsewhere, AgentSessionState::Running);
    app.agent
        .apply_sessions(Ok(vec![running.clone(), elsewhere.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(running.id.clone());
    app.agent.state.active_session.resolve(running.clone());
    app.enqueue_message("release me".to_string());

    // The reader is on the other session when the first one's turn ends.
    app.agent
        .apply_sessions(Ok(vec![settled.clone(), elsewhere.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(elsewhere.id.clone());
    app.agent.state.active_session.resolve(elsewhere.clone());
    let released = app.drain_queue();
    assert_eq!(released.len(), 1, "the queue did not follow its session");
    assert_eq!(released[0].0, settled.id);
    assert_eq!(released[0].1, "release me");

    // A session that is still running keeps what was written for it.
    app.agent
        .apply_sessions(Ok(vec![settled.clone(), busy_elsewhere.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(busy_elsewhere.id.clone());
    app.agent.state.active_session.resolve(busy_elsewhere);
    app.enqueue_message("second".to_string());
    assert!(app.drain_queue().is_empty());
    assert_eq!(queued_texts(&app), vec!["second".to_string()]);
}

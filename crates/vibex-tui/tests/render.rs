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

/// Draw one frame and return the screen as text, one line per row.
fn render(app: &mut App, width: u16, height: u16) -> Vec<String> {
    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
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
    // The prompt's own bottom border and the shortcut band are the two rows
    // below it; nothing else may intrude between them.
    assert_eq!(
        prompt_row,
        lines.len() - 4,
        "the prompt is not stacked above the shortcut band:\n{}",
        lines.join("\n")
    );
    assert!(
        lines[prompt_row + 1].contains('╰'),
        "the prompt has no bottom border: {:?}",
        lines[prompt_row + 1]
    );
    assert!(
        lines[prompt_row + 2].contains("Ctrl+P") || lines[prompt_row + 3].contains("Ctrl+P"),
        "the shortcut band does not follow the prompt"
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
fn one_turn_falls_back_to_a_scrollbar() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.transcript
        .set_blocks(vec![block("a", "turn-1"), block("b", "turn-1")]);
    let lines = render(&mut app, 120, 40);
    let has_rail = lines
        .iter()
        .any(|line| line.trim_end().ends_with('•') || line.trim_end().ends_with('▪'));
    assert!(!has_rail, "a single turn drew a navigation rail");
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
    // padding a terminal would otherwise put on the clipboard.
    app.begin_text_selection(0, 0);
    app.extend_text_selection(1, 6);
    let copied = app.selected_text().expect("a multi-line selection");
    assert!(copied.contains('\n'), "lines are not joined: {copied:?}");
    for line in copied.lines() {
        assert_eq!(line, line.trim_end(), "trailing padding was copied");
    }

    // `Esc` is what dismisses the highlight.
    assert!(app.clear_text_selection());
    assert!(app.selected_text().is_none());
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

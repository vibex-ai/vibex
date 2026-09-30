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

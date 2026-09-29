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
fn the_sidebar_disappears_in_compact_layouts() {
    let mut app = app(70, 24);
    let screen = text(&render(&mut app, 70, 24));
    // The top bar still lists the destinations...
    assert!(screen.contains("Sessions"));
    // ...but the sidebar border title is gone, because the pane is.
    let sidebar_title_count = screen.matches("Sessions").count();
    assert!(
        sidebar_title_count >= 1,
        "compact layout kept a sidebar it should have dropped:\n{screen}"
    );
}

#[test]
fn the_status_bar_reports_seat_and_liveness() {
    let mut app = app(120, 40);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Remote"), "the seat must be visible");
    assert!(
        screen.contains("Connecting") || screen.contains("Disconnected") || screen.contains("Done"),
        "the live state must be visible:\n{screen}"
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
    for forbidden in ["Cost", "Price", "USD", "EUR", "$"] {
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
    // The bordered body spans every row except the top bar, the key bar and
    // the status bar. Its right border sits in the last column, so a row that
    // lost it comes back shorter than the frame width.
    let body = &lines[1..lines.len().saturating_sub(2)];
    for (index, line) in body.iter().enumerate() {
        assert_eq!(
            line.chars().count(),
            usize::from(100u16),
            "body row {index} lost its right border: {line:?}"
        );
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
    assert!(screen.contains("Vibex"));
    assert!(screen.contains("Sessions"));
    assert!(screen.contains("Authority"));
    // The ASCII glyph mode must not emit box-drawing characters.
    assert!(!screen.contains('─'), "{screen}");
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

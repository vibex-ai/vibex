//! The client has no minimum terminal size.
//!
//! A terminal can be resized to anything — a split pane, a tiling layout, a
//! window manager that sizes a window before its content — and every size has
//! to produce a whole frame. These tests drive the real renderer far below any
//! size the design targets and assert that the two bands a reader cannot do
//! without survive: a line of the conversation, and a composer to type in.
//! Nothing paints a notice about the terminal being small, because there is no
//! size at which the client refuses to draw itself.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use vibex_backend::DisconnectedBackend;
use vibex_tui::app::{App, AppOptions, Page};
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::{Locale, SeatKind};

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
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.resize(columns, rows);
    app
}

/// One agent message, so the transcript band has something to draw.
fn message(id: &str) -> vibex_tui::transcript::Block {
    vibex_tui::transcript::Block {
        id: id.to_string(),
        kind: vibex_desktop_model::TimelineRowKind::AgentMessage,
        title: "a reply".to_string(),
        body: "the conversation the reader came for".to_string(),
        turn_id: Some("turn-1".to_string()),
        sequence: 1,
        timestamp_ms: None,
        expanded: false,
        collapsible: false,
        streaming: false,
        failed: false,
        pending_permission: false,
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
        activity: None,
        details: Vec::new(),
        group_summary: None,
    }
}

/// Draw one frame and return the rows a reader would see.
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
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect()
}

/// Sizes a terminal can genuinely report once a window is squeezed, from one
/// cell to just under the size the interface used to refuse.
const SIZES: &[(u16, u16)] = &[
    (1, 1),
    (2, 1),
    (4, 2),
    (8, 3),
    (12, 4),
    (20, 5),
    (24, 6),
    (30, 8),
    (40, 10),
    (56, 14),
    (59, 15),
];

const PAGES: &[Page] = &[
    Page::Agent,
    Page::NewSession,
    Page::Sessions,
    Page::Management,
    Page::Providers,
    Page::Agents,
    Page::Mcp,
    Page::Skills,
    Page::Prompts,
    Page::Devices,
    Page::Usage,
    Page::Settings,
    Page::Help,
];

#[test]
fn every_page_draws_at_every_size() {
    for &(columns, rows) in SIZES {
        for &page in PAGES {
            let mut app = app(columns, rows);
            app.navigate_to(page);
            // Drawing is the assertion: an over-constrained layout or a
            // subtraction past zero would panic here.
            let _ = render(&mut app, columns, rows);
        }
    }
}

#[test]
fn no_size_is_answered_with_a_notice() {
    for &(columns, rows) in SIZES {
        let mut app = app(columns, rows);
        app.navigate_to(Page::NewSession);
        let screen = render(&mut app, columns, rows).join("\n");
        for refusal in ["too small", "Minimum size", "Current size"] {
            assert!(
                !screen.contains(refusal),
                "{columns}x{rows} refused with {refusal:?}:\n{screen}"
            );
        }
    }
}

#[test]
fn a_small_terminal_keeps_the_transcript_and_the_composer() {
    for &(columns, rows) in &[(40u16, 10u16), (30, 8), (24, 6), (20, 5)] {
        let mut app = app(columns, rows);
        // The agent page is the one with a transcript band above the composer;
        // the composing page has a form of its own in that band.
        app.navigate_to(Page::Agent);
        app.transcript.set_blocks(vec![message("m1")]);
        // A draft, because "can the reader type here" is the question a size
        // notice used to answer with no.
        app.composer.insert_str("hello");
        let lines = render(&mut app, columns, rows);
        let screen = lines.join("\n");
        assert!(
            screen.contains("hello"),
            "a {columns}x{rows} terminal cannot show what is typed:\n{screen}"
        );
        let scrollback = app.regions.scrollback;
        assert!(
            scrollback.height >= 1,
            "no line of conversation at {columns}x{rows}"
        );
        let composer = app
            .regions
            .composer_band
            .expect("the composer band is published");
        assert!(
            composer.height >= 3,
            "the composer lost its rows at {columns}x{rows}"
        );
        assert!(
            composer.bottom() <= rows,
            "the composer fell off the {columns}x{rows} frame"
        );
    }
}

#[test]
fn the_bands_around_the_conversation_are_what_gives_way() {
    // The status row is worth a row on a normal terminal and not worth the last
    // one of the transcript, so it is gone by the time the frame is this small.
    let mut app = app(24, 6);
    app.navigate_to(Page::Agent);
    app.transcript.set_blocks(vec![message("m1")]);
    let _ = render(&mut app, 24, 6);
    assert!(
        app.regions.scrollback.height >= 1 && app.regions.composer_band.is_some(),
        "the frame kept its chrome and lost the conversation"
    );
}

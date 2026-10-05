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
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    // Rendering fixtures assert the built-in bindings, independent of a
    // developer's local key overrides.
    app.keymap = vibex_tui::keymap::Keymap::built_in();
    app.resize(columns, rows);
    app
}

/// Something only the composing page draws.
///
/// The page stopped naming itself — it is the prompt, and a reader who typed the
/// client's name to get here knows which page the cursor is in — so the tests
/// that ask "is the composing page on screen" have to ask for the one row that
/// is nobody else's: the Agent the message would be sent through.
const COMPOSING_PAGE: &str = "Agent setup";

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
        // The chrome that says which build this is: the page does not name
        // itself, so the version line is the product's own mark on it.
        assert!(
            screen.contains(env!("CARGO_PKG_VERSION")),
            "{width}x{height} lost the version line"
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
fn the_client_opens_on_the_page_a_session_is_written_on() {
    use vibex_tui::app::Focus;
    // The reader who typed `vibex` in a directory wants to write there, so the
    // prompt is the first screen and the list is one gesture away. The
    // directory the client was started in is the workspace that message is
    // sent for: it is the answer the reader already gave.
    let cwd = std::env::current_dir().expect("a working directory");
    let mut app = app(120, 40);
    assert_eq!(app.page, Page::NewSession);
    assert_eq!(app.focus, Focus::Composer);
    assert_eq!(
        app.new_session_workspace(),
        cwd.display().to_string(),
        "the page did not name the directory the client was started in"
    );

    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains(COMPOSING_PAGE), "{screen}");
    assert!(
        screen.contains("Workspace") && screen.contains(&cwd.display().to_string()),
        "the page does not name where the session would work:\n{screen}"
    );
}

#[test]
fn the_prompt_offers_the_session_list_in_its_corner() {
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    let buffer = render_buffer(&mut app, 120, 40);
    let status = display_row(&buffer, 1, 120);
    assert!(
        status.contains("Sessions") && status.contains("Ctrl+L"),
        "the corner entry is missing from the status band: {status:?}"
    );
    // The entry is a control, not a label: its rect is on the status row, at
    // the right end of it, and clicking it runs the same move as the chord.
    let (rect, intent) = app
        .regions
        .hints
        .iter()
        .find(|(_, intent)| *intent == Intent::GotoSessions)
        .copied()
        .expect("the corner entry is not clickable");
    assert_eq!(rect.y, 1, "the entry is not on the status row");
    assert_eq!(intent, Intent::GotoSessions);
    assert_eq!(
        rect.x + rect.width,
        120 - 2,
        "the entry does not end at the right padding"
    );
    let entry = (rect.x..rect.x + rect.width)
        .map(|column| buffer.cell((column, 1)).expect("cell").symbol())
        .collect::<String>();
    assert!(
        entry.contains("Sessions"),
        "the clickable rect is not over the entry: {entry:?}"
    );
}

/// The corner entry is not the prompt's alone.
///
/// A reader already inside a session leaves for another one the same way, and
/// the chord the entry names is the one that answers where the keyboard is:
/// `Ctrl+L` while the composer holds it, the global `1` once focus has moved to
/// the transcript.
#[test]
fn the_session_page_offers_the_session_list_in_its_corner() {
    use vibex_tui::action::Intent;
    use vibex_tui::app::Focus;
    let mut app = app(120, 40);
    enter_session(&mut app, "session_corner0001");
    app.focus = Focus::Composer;
    let buffer = render_buffer(&mut app, 120, 40);
    let (rect, intent) = app
        .regions
        .hints
        .iter()
        .find(|(_, intent)| *intent == Intent::GotoSessions)
        .copied()
        .expect("the corner entry is not clickable");
    assert_eq!(rect.y, 1, "the entry is not on the status row");
    assert_eq!(
        rect.x + rect.width,
        120 - 2,
        "the entry does not end at the right padding"
    );
    let status = display_row(&buffer, rect.y, 120);
    assert!(
        status.trim_end().ends_with("Sessions Ctrl+L"),
        "the composer's entry named the wrong chord: {status:?}"
    );
    let entry = (rect.x..rect.x + rect.width)
        .map(|column| buffer.cell((column, rect.y)).expect("cell").symbol())
        .collect::<String>();
    assert!(
        entry.contains("Sessions"),
        "the clickable rect is not over the entry: {entry:?}"
    );

    // Browsing the transcript is not composing: the composer's chord is no
    // longer bound there, so the entry stops naming it.
    app.focus = Focus::Main;
    let buffer = render_buffer(&mut app, 120, 40);
    let status = display_row(&buffer, rect.y, 120);
    assert!(
        status.trim_end().ends_with("Sessions 1"),
        "the transcript's entry named a chord it does not answer: {status:?}"
    );

    // The click is the same move as the chord, and it keeps the draft.
    app.composer.set_text("half a thought");
    app.perform(intent);
    assert_eq!(app.page, Page::Sessions);
    assert_eq!(
        app.composer.text(),
        "half a thought",
        "reaching the list threw the draft away"
    );
}

/// The entry yields to the state it sits beside.
///
/// `render_zoned_line` drops a right-hand group that does not fit whole, so an
/// entry that pushed the group over the edge would take the connection and the
/// seat with it. It is navigation, and the chord it names still answers when it
/// is gone, so it is the segment that goes.
#[test]
fn the_corner_entry_yields_to_the_status_beside_it() {
    use vibex_tui::action::Intent;
    // 48 columns: the seat and the connection fit, the entry would not.
    let mut narrow = app(48, 24);
    let buffer = render_buffer(&mut narrow, 48, 24);
    let status = display_row(&buffer, 1, 48);
    assert!(
        status.contains("Connecting") && status.contains("Remote mode"),
        "the entry pushed the status off a narrow terminal: {status:?}"
    );
    assert!(
        !status.contains("Sessions"),
        "the entry was drawn where it did not fit: {status:?}"
    );
    assert!(
        !narrow
            .regions
            .hints
            .iter()
            .any(|(_, intent)| *intent == Intent::GotoSessions),
        "the entry published a rect it was not drawn in"
    );

    // A few columns wider and the group fits whole, so the entry is back in
    // the corner it holds wherever it is drawn.
    let mut wider = app(60, 24);
    let buffer = render_buffer(&mut wider, 60, 24);
    let status = display_row(&buffer, 1, 60);
    assert!(
        status.trim_end().ends_with("Sessions Ctrl+L"),
        "the entry did not come back where it fits: {status:?}"
    );
}

#[test]
fn escape_walks_between_the_prompt_and_the_list() {
    use vibex_tui::action::Intent;
    use vibex_tui::app::Focus;
    // The two halves of where the client starts are a pair, so leaving the
    // list lands back on the prompt rather than on a page that ignores the key.
    let mut app = app(120, 40);
    app.perform(Intent::Back);
    assert_eq!(app.page, Page::Sessions);
    app.perform(Intent::Back);
    assert_eq!(app.page, Page::NewSession);
    assert_eq!(app.focus, Focus::Composer);
}

#[test]
fn the_composer_reaches_the_session_list_without_giving_up_the_draft() {
    use vibex_tui::action::Intent;
    use vibex_tui::keymap::{Chord, Keymap};
    let mut app = app(120, 40);
    // A developer's key file must not decide this: the built-in table is what
    // the page names in its corner.
    app.keymap = Keymap::built_in();
    app.composer.insert_str("half a thought");

    let intent = app
        .keymap
        .resolve(&app.active_scopes(), Chord::ctrl('l'))
        .expect("Ctrl+L is bound while the composer owns the keyboard");
    assert_eq!(intent, Intent::GotoSessions);
    app.perform(intent);
    assert_eq!(app.page, Page::Sessions);
    assert_eq!(
        app.composer.text(),
        "half a thought",
        "reaching the list threw the draft away"
    );

    // The digits the global table uses are still digits: a reader writing `1`
    // means the character.
    for character in ['1', '2'] {
        app.composer.insert_char(character);
    }
    assert_eq!(app.composer.text(), "half a thought12");
}

#[test]
fn the_landing_mark_loops_without_repainting_between_passes() {
    // The light comes round for as long as the page waits, but the stretch
    // between passes draws what is already on screen: the clock runs, the
    // repaint does not. That is what keeps a looping mark from being a repaint
    // every animation tick for the life of the page.
    let mut app = app(120, 40);
    assert!(app.chrome_animating(), "the landing mark does not light up");
    let mut drew = 0usize;
    let mut quiet = 0usize;
    // One whole loop. Every tick moves the clock, and asks for a repaint
    // exactly when the frame it lands on is one that differs from rest.
    for _ in 0..vibex_tui::logo::LOOP_FRAMES {
        let lands_on = (app.animation_phase() + 1) % vibex_tui::logo::LOOP_FRAMES;
        assert!(app.is_animating(), "the loop's clock stopped");
        assert_eq!(
            app.advance_transcript_animation(),
            vibex_tui::logo::moving(lands_on),
            "the tick landing on phase {lands_on} asked for the wrong thing"
        );
        if vibex_tui::logo::moving(lands_on) {
            drew += 1;
        } else {
            quiet += 1;
        }
    }
    // Most of a loop is the quiet stretch, which is the point of it.
    assert_eq!(drew, vibex_tui::logo::SWEEP_FRAMES as usize);
    assert!(quiet > drew, "the light is on more often than it is off");
    // And the clock has come round exactly.
    assert_eq!(app.animation_phase() % vibex_tui::logo::LOOP_FRAMES, 0);
    assert!(app.chrome_animating(), "the light did not come round again");
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
        status.contains("Remote mode"),
        "the seat must be visible: {status:?}"
    );
    assert!(
        status.contains("Connecting") || status.contains("Disconnected") || status.contains("Done"),
        "the live state must be visible: {status:?}"
    );
}

#[test]
fn the_shortcuts_band_is_the_last_row() {
    // A page that is navigated rather than written on keeps the bar: the page
    // a session is written on spends its last row on the prompt instead.
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::GotoSessions);
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
fn the_prompt_is_the_last_band_on_a_session_page() {
    let mut app = app(120, 40);
    // The composer belongs to a session view.
    app.navigate_to(Page::Agent);
    app.composer.set_text("hello");
    let lines = render(&mut app, 120, 40);
    let prompt_row = lines
        .iter()
        .position(|line| line.contains('❯'))
        .expect("the prompt is on screen");
    // One padding row, then the info line, then the outer padding: nothing is
    // drawn between the prompt and the bottom of the screen.
    assert!(lines[prompt_row + 1].trim().is_empty());
    assert!(
        !lines[prompt_row + 2].trim().is_empty(),
        "the prompt's info line is missing:\n{}",
        lines.join("\n")
    );
    assert_eq!(
        prompt_row + 2,
        lines.len() - 2,
        "something is drawn under the prompt:\n{}",
        lines.join("\n")
    );
}

#[test]
fn a_session_page_draws_no_shortcut_band() {
    // The pages a reader writes on spend their last row on the draft: the
    // composer's own line names the runtime, and every key the band would
    // advertise is a `?` away.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    let lines = render(&mut app, 120, 40);
    assert!(
        !lines.iter().any(|line| line.contains("Ctrl+P")),
        "the session page still draws the key bar:\n{}",
        lines.join("\n")
    );

    // The list page keeps it: that page is navigated rather than written on.
    app.perform(vibex_tui::action::Intent::GotoSessions);
    let lines = render(&mut app, 120, 40);
    assert!(
        lines.iter().any(|line| line.contains("Ctrl+P")),
        "the list page lost the key bar:\n{}",
        lines.join("\n")
    );
}

#[test]
fn the_prompt_has_a_padded_surface_and_separate_context_line() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    let buffer = render_buffer(&mut app, 120, 40);
    let band = app.regions.composer_band.unwrap();
    let editor = app.regions.composer.unwrap();
    assert_eq!(editor.y, band.y + 1);
    assert_eq!(editor.bottom() + 2, band.bottom());
    assert_eq!(buffer[(band.x, band.y)].bg, app.theme.roles.surface_raised);
    assert_ne!(
        buffer[(band.x, band.bottom() - 1)].bg,
        app.theme.roles.surface_raised
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
    // The page shows this session, which is what the info line answers for:
    // without a selected session the page is writing a new one and names the
    // entry that creation would use.
    app.agent.state.selected_session_id = app
        .agent
        .state
        .active_session
        .value
        .as_ref()
        .map(|session| session.id.clone());
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
        lines.iter().any(|line| line.contains("gpt-5")),
        "the prompt does not name its model:\n{screen}"
    );
    // The switcher's key is not advertised on a session page — the key legend
    // lives behind `?` there — so what the prompt owes the reader is the
    // runtime it is actually on, never a catalogue entry it is not.
    assert!(
        !screen.contains("claude-sonnet"),
        "the prompt named a catalogue entry the session is not on:\n{screen}"
    );
    // It rides the far end of the bottom border: the reader looks for the
    // runtime beside the box's corner, and the rule on the left is the line the
    // eye follows into the prompt.
    let info_row = lines
        .iter()
        .find(|line| line.contains("gpt-5"))
        .expect("the info line is on screen");
    let start = info_row
        .char_indices()
        .find(|(_, character)| !matches!(character, '─' | ' ' | '│' | '╰' | '╭'))
        .map(|(index, _)| index)
        .expect("the info line has content");
    assert!(
        start < 8,
        "the runtime is not aligned with the editor: column {start}\n{screen}"
    );
    assert!(
        info_row[start..].starts_with("codex · gpt-5"),
        "the info line starts with something else: {:?}",
        &info_row[start..]
    );
}

/// A catalogue whose second entry publishes run options: a thinking ladder, a
/// conversation mode, and a switch.
fn run_option_catalog() -> vibex_core::SessionRuntimeOptionCatalog {
    let value = |value: &str, label: &str| vibex_core::SessionConfigValue {
        value: value.to_string(),
        label: (!label.is_empty()).then(|| label.to_string()),
    };
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
    let mut codex = option("codex", "gpt-5");
    codex.reasoning_efforts = vec![value("low", "Low"), value("high", "High")];
    codex.modes = vec![value("plan", "Plan")];
    codex.features = vec![vibex_core::SessionRuntimeFeature {
        id: "web_search".to_string(),
        label: "Web search".to_string(),
        description: None,
        kind: vibex_core::SessionRuntimeFeatureKind::Toggle,
        current_value: Some(value("false", "")),
        default_value: Some(value("false", "")),
        values: Vec::new(),
    }];
    vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![option("claude", "claude-sonnet"), codex],
    }
}

/// Put the open session on `desired`, as the runtime reports it.
fn open_session_on(app: &mut App, desired: vibex_core::SessionRuntimeSelection) {
    let session_id = vibex_core::VibexSessionId::new();
    app.agent
        .state
        .active_session
        .resolve(vibex_core::AgentSession {
            id: session_id.clone(),
            title: "a session".to_string(),
            project_id: vibex_core::ProjectId::new(),
            workspace_id: vibex_core::WorkspaceId::new(),
            workspace_root: "/tmp/vibex-render-workspace".to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: desired.agent_id.clone(),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms: 1,
            last_message_at_ms: 1,
            archived_at_ms: None,
            deleted_at_ms: None,
        });
    app.agent.state.selected_session_id = Some(session_id);
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
}

/// Put the switcher's cursor on one catalogue entry.
///
/// The list is a tree — an Agent heading above its entries, and the pinned
/// recent and starred rows above those — so the row an entry is drawn on is not
/// its index in the catalogue. Tests ask for the row the way the renderer finds
/// it, and open the group first: the picker opens as a list of Agents, with
/// every Agent but the one in use folded away.
fn pick_entry(app: &mut App, index: usize) {
    let agent_id = app.runtime_options.as_ref().expect("catalogue").options[index]
        .selection
        .agent_id
        .clone();
    app.fold_runtime_group(&agent_id, false);
    let row = app
        .runtime_picker_rows()
        .iter()
        .position(|row| row.entry() == Some(index))
        .expect("the entry is on the list");
    app.overlay = Some(vibex_tui::app::Overlay::RuntimePicker {
        view: vibex_tui::app::RuntimePickerView::Choices,
        selected: row,
    });
}

#[test]
fn a_pinned_row_names_the_agent_and_how_it_ran() {
    // A pinned row stands outside every Agent group, so it has to carry what the
    // heading above it would otherwise say — the Agent — and, as the row the
    // reader comes back to, how that entry ran last time. An entry under its own
    // heading leaves both to the heading: the group is right above it, and a
    // whole list of repeated run options would be a wall rather than a list.
    let mut app = app(140, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let catalog = run_option_catalog();
    let desired = catalog.options[1].selection.clone();
    app.runtime_options = Some(catalog.clone());
    open_session_on(&mut app, desired.clone());

    // The reader ran the entry with a thinking depth, and it is remembered.
    let mut remembered = desired;
    remembered.reasoning_effort = Some("high".to_string());
    app.remember_runtime_selection(&remembered);
    app.show_runtime_picker();

    let screen = text(&render(&mut app, 140, 40));
    assert!(
        screen.contains("Recent"),
        "the entry the reader used last is not offered:\n{screen}"
    );
    let row = screen
        .lines()
        .find(|line| line.contains("Thinking depth High"))
        .unwrap_or_else(|| panic!("the recent row is not on screen:\n{screen}"));
    for expected in ["codex", "bal", "gpt-5"] {
        assert!(
            row.contains(expected),
            "the recent row does not name {expected:?}: {row:?}"
        );
    }
    assert!(
        row.contains("Thinking depth High"),
        "the recent row does not say how it ran: {row:?}"
    );
    // The Agent's own entries say none of that twice: the heading above them is
    // the Agent, and the run options belong to the pinned row.
    let entry = screen
        .lines()
        .find(|line| line.contains("● bal · gpt-5"))
        .unwrap_or_else(|| panic!("the entry in use is not on screen:\n{screen}"));
    assert!(
        !entry.contains("Thinking depth"),
        "an entry under its heading repeats the run options: {entry:?}"
    );
}

#[test]
fn the_switcher_keeps_the_run_options_one_key_away_from_the_catalogue() {
    // Picking an Agent is only half of "what will this message be sent
    // through". The run options are the switcher's second view rather than rows
    // appended under the catalogue: this catalogue is short, but a machine with
    // fifty models would push them past the bottom of the list.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let catalog = run_option_catalog();
    let desired = catalog.options[1].selection.clone();
    app.runtime_options = Some(catalog);
    open_session_on(&mut app, desired);
    app.show_runtime_picker();

    // The catalogue view answers "which Agent", and says how to reach the rest.
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("codex"), "{screen}");
    assert!(
        screen.contains("Tab") && screen.contains("Run options"),
        "the switcher does not advertise its second view:\n{screen}"
    );
    assert!(
        !screen.contains("Thinking depth"),
        "the catalogue view mixes in the run options:\n{screen}"
    );

    // One `Tab` and the run options are the whole list, reachable without
    // scrolling past the catalogue.
    app.perform(vibex_tui::action::Intent::OverlayNextField);
    let screen = text(&render(&mut app, 120, 40));
    for expected in [
        "Run options",
        "Thinking depth",
        "Conversation mode",
        "Web search",
        // The values in effect, named rather than left to the wire's words.
        "Off",
    ] {
        assert!(
            screen.contains(expected),
            "the run-option view does not offer {expected:?}:\n{screen}"
        );
    }
    assert!(
        !screen.contains("claude-sonnet"),
        "the run-option view still lists the catalogue:\n{screen}"
    );
}

#[test]
fn the_switcher_names_what_the_choice_applies_to() {
    // The Agent key is global, so the surface it opens has to answer "what am I
    // on" before it asks "what do you want": the entry in use is marked, and the
    // catalogue opens on it. On a session's own page that is the session's
    // Agent; on a page showing no session — the list, or the page writing a new
    // one — it is the Agent the *next* session would be created with, never the
    // one the list has open behind it. Nothing else may move, which is what
    // keeps the two independent.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let catalog = run_option_catalog();
    let desired = catalog.options[1].selection.clone();
    app.runtime_options = Some(catalog);
    open_session_on(&mut app, desired);
    app.show_runtime_picker();
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("Agent setup · Choose an Agent"),
        "the picker does not name the surface it is:\n{screen}"
    );
    assert!(
        screen.contains("Current"),
        "the picker does not mark the Agent the session is on:\n{screen}"
    );

    // The reader steps back to the list, where no session is shown: the same key
    // now chooses for the session they are about to write.
    app.overlay = None;
    app.navigate_to(Page::Sessions);
    app.show_runtime_picker();
    assert!(
        app.runtime_option_is_current(&app.runtime_options.as_ref().expect("catalogue").options[0]),
        "the list's picker opened on the session behind it"
    );
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("Current"),
        "the list's picker does not mark the Agent a creation would use:\n{screen}"
    );
}

#[test]
fn a_long_catalogue_does_not_bury_the_run_options() {
    // The case that made the appended-list design useless: forty models on
    // screen, and the handful of run options past the bottom of them.
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let mut catalog = run_option_catalog();
    let desired = catalog.options[1].selection.clone();
    let model = vibex_core::RuntimeModelSelection::explicit("gpt-5");
    for index in 0..40 {
        let mut extra = catalog.options[1].clone();
        extra.model_label = format!("gpt-5-{index}");
        extra.selection.model = model.clone();
        catalog.options.push(extra);
    }
    app.runtime_options = Some(catalog);
    open_session_on(&mut app, desired);
    app.show_runtime_picker();
    app.perform(vibex_tui::action::Intent::OverlayNextField);

    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("Thinking depth") && screen.contains("Conversation mode"),
        "the run options are still under the catalogue:\n{screen}"
    );
    assert!(
        !screen.contains("gpt-5-39"),
        "the run-option view drew catalogue rows:\n{screen}"
    );
}

#[test]
fn a_run_option_offers_the_agents_own_default_and_what_it_accepts() {
    let mut app = app(120, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let catalog = run_option_catalog();
    let desired = catalog.options[1].selection.clone();
    app.runtime_options = Some(catalog);
    open_session_on(&mut app, desired);
    app.show_runtime_picker();

    // Confirm the thinking-depth row, which lists what the Agent accepts.
    app.overlay = Some(vibex_tui::app::Overlay::RuntimePicker {
        view: vibex_tui::app::RuntimePickerView::Options,
        selected: 0,
    });
    let outcome = app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert!(outcome.effects.is_empty(), "{outcome:?}");
    assert!(matches!(
        app.overlay,
        Some(vibex_tui::app::Overlay::RunOptionValues { row: 0, .. })
    ));

    let screen = text(&render(&mut app, 120, 40));
    for expected in ["Thinking depth", "Default", "Low", "High", "Current"] {
        assert!(
            screen.contains(expected),
            "the value list does not offer {expected:?}:\n{screen}"
        );
    }
}

#[test]
fn the_prompt_names_the_run_options_the_session_is_on() {
    // The values the reader set ride beside the runtime they belong to, so
    // "how will this message be sent" is answered from the composer itself.
    let mut app = app(140, 40);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let catalog = run_option_catalog();
    let mut desired = catalog.options[1].selection.clone();
    desired.reasoning_effort = Some("high".to_string());
    desired.mode_id = Some("plan".to_string());
    app.runtime_options = Some(catalog);
    open_session_on(&mut app, desired);

    let lines = render(&mut app, 140, 40);
    let screen = text(&lines);
    let info_row = lines
        .iter()
        .find(|line| line.contains("codex · gpt-5"))
        .expect("the info line is on screen");
    assert!(
        info_row.contains("codex · gpt-5 · High · Plan"),
        "the info line does not name the run options in effect: {info_row:?}"
    );
    // A feature stays in the switcher: the line carries the shape of the
    // message, not every setting behind it.
    assert!(
        !info_row.contains("Web search"),
        "the info line grew a feature list: {info_row:?}"
    );
    assert!(!screen.is_empty());
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
    use vibex_tui::settings::SettingRow;
    let mut app = settings_app(120, 40);
    select_setting(&mut app, SettingRow::Theme);
    app.perform(vibex_tui::action::Intent::ResetSetting);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Confirm"), "{screen}");
    assert!(screen.contains("Cancel"), "{screen}");
}

#[test]
fn the_quit_command_leaves_without_a_second_question() {
    // `Ctrl+Q` used to raise a modal asking whether the reader meant it. The
    // chord is gone, and so is the modal: the palette's Quit is a command the
    // reader picked out by name, and `Ctrl+C` still owns the two-press version
    // for the key that can be hit by accident.
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::RequestQuit);
    assert!(app.overlay.is_none(), "quitting asked a second question");
    assert!(app.should_quit, "the quit command did not leave");
}

#[test]
fn a_ctrl_c_with_nothing_left_to_cancel_asks_before_it_leaves() {
    // The reader pressed `Ctrl+C` at a prompt with nothing to cancel. That is a
    // common misfire — a press aimed at a turn that had just finished — so the
    // client asks instead of leaving, and asks on the page it is already on.
    let hint = Strings::for_locale(Locale::En).quit_hint();
    let mut app = app(120, 40);
    assert!(!app.quit_armed());
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(!app.should_quit, "one press was taken for an instruction");
    assert!(app.quit_armed(), "the page did not ask anything");
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains(hint),
        "the page did not say how to answer:\n{screen}"
    );

    // The second press is the answer.
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(app.should_quit, "the second press did not leave");
}

#[test]
fn a_question_about_quitting_expires_with_its_hint() {
    // The hint and the arming are one piece of state, so the answer is live
    // exactly as long as the question is on screen.
    let hint = Strings::for_locale(Locale::En).quit_hint();
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(app.quit_armed());
    for _ in 0..vibex_tui::app::QUIT_ARM_FRAMES {
        app.tick();
    }
    assert!(!app.quit_armed(), "the question never went away");
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        !screen.contains(hint),
        "the hint outlived the arming:\n{screen}"
    );
    // A press after that is a new question, not a stale answer.
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(!app.should_quit, "a stale press was taken for an answer");
    assert!(app.quit_armed(), "the new question was not asked");
}

#[test]
fn a_ctrl_c_that_cancels_something_else_answers_nothing() {
    // The draft case: the first press asked about quitting, the reader went
    // back to writing, and the second press is for the draft. Letting it answer
    // the old question would quit on a reader who had moved on.
    let mut app = app(120, 40);
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(app.quit_armed());
    app.composer.insert_str("a thought");
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(!app.should_quit, "clearing a draft quit the client");
    assert_eq!(app.composer.text(), "");
    assert!(!app.quit_armed(), "the old question outlived its press");
    // And the press after that asks again rather than leaving.
    app.perform(vibex_tui::action::Intent::ContextualCancel);
    assert!(!app.should_quit, "the spent question was answered");
    assert!(app.quit_armed(), "the next question was not asked");
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
    for needle in ["Agents", "Providers", "MCP servers", "Skills", "Devices"] {
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
    // A connected client whose backend does not offer device management: the
    // index's gated row has to say so rather than pretending it is there.
    app.live = vibex_tui::app::LiveState::Ready;
    app.capabilities.device = vibex_backend::DomainCapabilities::unavailable();
    app.perform(vibex_tui::action::Intent::GotoManagement);
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("cannot") || screen.contains("unavailable"),
        "gated rows must explain themselves:\n{screen}"
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
        opened("a", "turn-1"),
        block("b", "turn-1"),
        opened("c", "turn-2"),
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
        .set_blocks(vec![opened("a", "turn-1"), block("b", "turn-1")]);
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
    }
}

/// The row that opens a turn: the message the reader sent.
///
/// The rail counts turns by the messages that began them, so a fixture that
/// stands for a turn has to carry the message and not only the answer to it.
fn opened(id: &str, turn: &str) -> vibex_tui::transcript::Block {
    let mut block = block(id, turn);
    block.kind = vibex_desktop_model::TimelineRowKind::UserMessage;
    block
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
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.resize(100, 30);
    let screen = text(&render(&mut app, 100, 30));
    assert!(screen.contains("Local mode"));
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
    // The reader's own message pads its text, so the first line with anything
    // on it is not necessarily the first line of the transcript.
    let lines =
        app.transcript
            .plain_lines(0, 12, &app.theme.clone(), Strings::for_locale(Locale::En));
    let row = lines
        .iter()
        .position(|line| !line.is_empty())
        .expect("a line with text on it");
    let next = row
        + 1
        + lines[row + 1..]
            .iter()
            .position(|line| !line.is_empty())
            .expect("a second block under the first");
    let (prefix, _) = vibex_tui::text::take_width(&lines[row], 4);

    app.begin_text_selection(row, 0);
    app.extend_text_selection(row, 4);
    assert!(app.finish_text_selection(), "the drag covered cells");
    let copied = app.selected_text().expect("a non-empty selection");
    assert_eq!(copied, prefix.trim_end());

    // A selection over several lines joins them with newlines and drops the
    // padding a terminal would otherwise put on the clipboard — the blank rows
    // inside the reader's own box among them.
    app.begin_text_selection(row, 0);
    app.extend_text_selection(next, 6);
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
/// tests have to be inside one — which is the session's page, not the prompt
/// the client opens on.
fn enter_session(app: &mut App, id: &str) {
    let session = seeded_session(id, "queue fixture");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session);
    app.navigate_to(Page::Agent);
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
    assert!(before.contains("read the design"), "{before}");
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
    // sentence are findable at a glance rather than by reading the line. The
    // search is anchored to that sentence: the status band names the directory
    // the client was started in, which can spell the crate's own name too.
    let literal_row = rows
        .iter()
        .position(|row| row.contains("升级"))
        .expect("the sentence holding the literals is on screen");
    let span_colour = |needle: &str| {
        // Column, not byte offset: the row is full of double-width glyphs.
        let byte = rows[literal_row]
            .find(needle)
            .unwrap_or_else(|| panic!("{needle} is on screen:\n{screen}"));
        let column = vibex_tui::text::display_width(&rows[literal_row][..byte]).min(119) as u16;
        (column..120)
            .filter_map(|column| buffer.cell((column, literal_row as u16)))
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
    // The state mark, not a word: every row's mark has to start at the same
    // cell however wide its title is. Measured on the buffer rather than on
    // extracted text, because a wide character occupies two cells and only the
    // buffer knows which.
    let row_text = |row: u16| {
        (0..120)
            .filter_map(|column| buffer.cell((column, row)))
            .map(|cell| cell.symbol().to_string())
            .collect::<String>()
    };
    // Identify these rows by their fixture titles. Relative ages change with
    // the clock and must not decide whether the alignment assertion runs.
    let columns = (0..40)
        .filter(|row| {
            // Continuation cells of a wide glyph are blank in TestBackend.
            let text = row_text(*row).replace(' ', "");
            ["short", "帮我看看", "一个特别"]
                .iter()
                .any(|title| text.contains(title))
        })
        .filter_map(|row| column_of(&buffer, row, "◇"))
        .collect::<Vec<_>>();
    assert!(
        columns.len() >= 3,
        "not every session row was drawn:\n{columns:?}"
    );
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
    let band = app.regions.composer_band.unwrap();
    assert_eq!(
        region.bottom() + 2,
        band.bottom(),
        "editor padding and context overlap"
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
    // The reader's own key file must not decide this: a rebind onto a chord
    // their file has already moved is not the shadowing this test is about, and
    // running the suite on a machine that has one would otherwise fail here.
    app.keymap = vibex_tui::keymap::Keymap::built_in();
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
    let original = app.theme.id.to_string();
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
        app.theme.id,
        original.as_str(),
        "moving in the chooser must preview the value"
    );

    app.perform(vibex_tui::action::Intent::Back);
    assert_eq!(
        app.theme.id,
        original.as_str(),
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
    assert_eq!(app.theme.id, other.id);

    app.perform(vibex_tui::action::Intent::ResetSetting);
    assert!(
        app.overlay.is_some(),
        "a reset is destructive enough to ask first"
    );
    app.perform(vibex_tui::action::Intent::ConfirmOverlay);
    assert_eq!(
        app.theme.id,
        vibex_ui::theme_catalog::default_theme_id(app.settings.mode)
    );
    assert!(app.overlay.is_none());
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
    let list = app
        .regions
        .list
        .clone()
        .expect("the session list is clickable");
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
    blocks[1].kind = vibex_desktop_model::TimelineRowKind::UserMessage;
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
        timestamp_ms: 1_000,
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

    // Running a command remembers it, and the next open leads with it.
    app.remember_command(Intent::GotoUsage);
    let entries = app.palette_entries("");
    assert_eq!(entries[0].entry.intent, Intent::GotoUsage);
    assert_eq!(entries[0].group, vibex_tui::view::PaletteGroup::Recent);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("Recent"), "{screen}");

    // The `Recent` band is a shortcut, not a move: the command is still filed
    // under its own heading, so a section never empties as it is used.
    let groups = entries
        .iter()
        .filter(|listing| listing.entry.intent == Intent::GotoUsage)
        .map(|listing| listing.group)
        .collect::<Vec<_>>();
    assert_eq!(
        groups,
        vec![
            vibex_tui::view::PaletteGroup::Recent,
            vibex_tui::view::PaletteGroup::View
        ],
        "the remembered command left its own section"
    );

    // The frame draws it twice, under the two headings those listings name: the
    // rows the frame published say which line each one landed on.
    let buffer = render_buffer(&mut app, 120, 40);
    let region = app
        .regions
        .palette
        .clone()
        .expect("the palette published no rows");
    let line_text = |line: usize| -> String {
        (region.rect.x..region.rect.right())
            .filter_map(|column| buffer.cell((column, region.rect.y + line as u16)))
            .map(|cell| cell.symbol().to_string())
            .collect()
    };
    let line_of = |index: usize| -> usize {
        region
            .rows
            .iter()
            .position(|row| *row == Some(index))
            .unwrap_or_else(|| panic!("entry {index} was not drawn:\n{screen}"))
    };
    let heading_above = |mut line: usize| -> String {
        while line > 0 {
            line -= 1;
            if region.rows[line].is_none() {
                return line_text(line).trim().to_string();
            }
        }
        panic!("no heading above line {line}:\n{screen}");
    };
    let recent = entries
        .iter()
        .position(|listing| {
            listing.entry.intent == Intent::GotoUsage
                && listing.group == vibex_tui::view::PaletteGroup::Recent
        })
        .expect("no remembered listing");
    let filed = entries
        .iter()
        .position(|listing| {
            listing.entry.intent == Intent::GotoUsage
                && listing.group == vibex_tui::view::PaletteGroup::View
        })
        .expect("no `View` listing");
    assert_eq!(
        heading_above(line_of(recent)),
        "Recent",
        "the remembered command is not drawn under `Recent`:\n{screen}"
    );
    assert_eq!(
        heading_above(line_of(filed)),
        "View",
        "`Usage` is no longer drawn in the `View` section:\n{screen}"
    );
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
    // The first incomplete step is the one that explains itself. The workspace
    // step is already answered — the client opens in the directory it was
    // started in — so the guide starts at the session.
    assert!(screen.contains("One session per task"), "{screen}");
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
    // Different actions remain distinct; grouping must not hide a write as a read.
    assert!(screen.contains("read_file"), "{screen}");
    assert!(screen.contains("write_file"), "{screen}");
    assert!(screen.contains("grep"), "{screen}");
    assert!(
        !screen.contains("Tool read_file"),
        "the kind label doubles the title:\n{screen}"
    );
    assert!(
        !screen.contains("Plan ") && !screen.contains("Permission"),
        "bookkeeping rows reached the transcript:\n{screen}"
    );
    // The whole turn fits in a screen and a half, where one row per event plus
    // a body for each section would not. The reader's own message is a box with
    // padding above and below its text, and that box is part of the count.
    assert!(
        app.transcript.total_height() <= 16,
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
    let settled_screen = text(&render(&mut settled, 120, 30));
    // The turn line names the phase the turn is in, so a settled session must
    // not be wearing any of the words a live one does.
    for running_word in [
        settled.strings.running(),
        settled.strings.phase_preparing(),
        settled.strings.phase_thinking(),
        settled.strings.phase_calling_tool(),
        settled.strings.phase_generating(),
        settled.strings.phase_waiting_approval(),
    ] {
        assert!(
            !settled_screen.contains(running_word),
            "a settled session is drawn as running ({running_word}):\n{settled_screen}"
        );
    }

    // While the runtime says it is running, the same rows do stream.
    let mut running = build(vibex_core::AgentSessionState::Running);
    assert!(
        running.transcript_animating(),
        "a running turn stopped streaming"
    );
    let running_screen = text(&render(&mut running, 120, 30));
    assert!(
        running_screen.contains(running.strings.phase_generating()),
        "the running turn's phase is not on its own turn line:\n{running_screen}"
    );
}

#[test]
fn the_turn_line_reports_what_the_running_turn_has_done() {
    // The desktop draws a session's live reading above its composer: the phase,
    // the clock, the tools it has called and the pace it is writing at. The TUI
    // draws the same line above its prompt, and this is that line with
    // everything it can say.
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
    let mut call = seeded_item(
        &session_id,
        2,
        vibex_core::TimelineItemKind::ToolCall,
        vibex_core::TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
            tool_call_id: "call-1".into(),
            tool_name: "read_file".into(),
            status: vibex_core::ToolCallStatus::Completed,
            summary: String::new(),
            input_summary: Some("crates/vibex-tui/src/view.rs".into()),
            output_summary: Some("42 lines".into()),
            raw_extension: None,
        }),
    );
    call.correlation_id = Some(correlation.clone());
    let mut delta = seeded_item(
        &session_id,
        3,
        vibex_core::TimelineItemKind::AgentMessage,
        vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
            text_delta: "the answer".into(),
            chunk_index: 0,
            phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
        }),
    );
    delta.correlation_id = Some(correlation.clone());

    let mut app = app(120, 30);
    app.navigate_to(Page::Agent);
    let mut session = seeded_session("session_turnline01", "the line above the prompt");
    session.id = session_id.clone();
    session.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session_id.clone());
    app.agent.state.active_session.resolve(session);
    let mut timeline = vec![user, call, delta.clone()];
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), timeline.clone());
    // The clock is the session's: the turn has been running for a while.
    app.turn_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(189));
    app.sync_transcript();

    // The pace is a difference between two readings, so the answer has to grow
    // before there is one to report.
    let mut grown = delta;
    grown.sequence = 4;
    grown.payload =
        vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
            text_delta: " and more of it".into(),
            chunk_index: 1,
            phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
        });
    timeline.push(grown);
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), timeline);
    std::thread::sleep(std::time::Duration::from_millis(20));
    app.sync_transcript();

    let screen = text(&render(&mut app, 120, 30));
    let line = screen
        .lines()
        .find(|line| line.contains(app.strings.phase_generating()))
        .unwrap_or_else(|| panic!("no turn line on the session page:\n{screen}"));
    assert!(
        line.contains("3m09s"),
        "the turn's clock is not on the line: {line:?}"
    );
    assert!(
        line.contains(&format!("1 {}", app.strings.tool_calls())),
        "the tool count is not on the line: {line:?}"
    );
    assert!(
        line.contains("t/s"),
        "the pace is not on the line: {line:?}"
    );
    assert!(
        !line.contains(app.strings.idle()),
        "the running turn reads as idle: {line:?}"
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
fn the_runtime_starting_up_earns_neither_a_row_nor_a_tick() {
    // The reproduction. A fresh session writes a line as it is created and
    // another as its first turn starts its runtime; the first stands before any
    // message, so the projection made a turn of it. Both were drawn as rows and
    // the pair read as two turns, on a session the reader had asked one
    // question of.
    let session_id = vibex_core::VibexSessionId::new();
    let mut app = app(100, 24);
    app.navigate_to(Page::Agent);
    app.agent.state.selected_session_id = Some(session_id.clone());
    let items = vec![
        seeded_item(
            &session_id,
            1,
            vibex_core::TimelineItemKind::SystemNotice,
            vibex_core::TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                level: vibex_core::SystemNoticeLevel::Info,
                message: vibex_core::SESSION_INITIALIZING_NOTICE.to_string(),
            }),
        ),
        seeded_item(
            &session_id,
            2,
            vibex_core::TimelineItemKind::UserMessage,
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: "what is this project?".into(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        ),
        seeded_item(
            &session_id,
            3,
            vibex_core::TimelineItemKind::SystemNotice,
            vibex_core::TimelinePayload::SystemNotice(vibex_core::SystemNoticePayload {
                level: vibex_core::SystemNoticeLevel::Info,
                message: vibex_core::turn_startup_notice("ACP", Some("2 MCP servers")),
            }),
        ),
        seeded_item(
            &session_id,
            4,
            vibex_core::TimelineItemKind::AgentMessage,
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "Vibex is a local-first AI coding workbench.".into(),
                is_final: true,
            }),
        ),
    ];
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id, items);
    app.sync_transcript();

    let screen = text(&render(&mut app, 100, 24));
    assert!(screen.contains("what is this project?"), "{screen}");
    assert!(
        screen.contains("Vibex is a local-first AI coding workbench."),
        "{screen}"
    );
    assert!(
        !screen.contains("initializing"),
        "the startup line is on screen:\n{screen}"
    );
    assert!(
        !screen.contains("Starting ACP agent runtime"),
        "the turn startup line is on screen:\n{screen}"
    );

    // One question is one turn, however many lines the runtime wrote around it.
    assert_eq!(
        app.transcript.turn_count(),
        1,
        "the startup line took a turn"
    );
    assert_eq!(
        app.regions.turns.len(),
        1,
        "the rail drew a tick for the runtime's own line"
    );
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
    // The key legend belongs to the list page, so that is where the hint rects
    // are published and where the leak would show.
    let _ = render(&mut app, 120, 40);
    let hints = app.regions.hints.len();
    assert!(hints > 0, "the frame published no hints");
    for _ in 0..3 {
        let _ = render(&mut app, 120, 40);
    }
    assert_eq!(app.regions.hints.len(), hints, "the hint list grew");

    app.navigate_to(Page::Agent);
    let _ = render(&mut app, 120, 40);
    let turns = app.regions.turns.len();
    assert_eq!(app.regions.turns.len(), turns, "the turn list grew");

    // A session with turns publishes one rect per visible tick, and no more.
    app.transcript.set_blocks(
        (0..3)
            .map(|turn| {
                let mut block = seeded_block(
                    &format!("t{turn}"),
                    vibex_desktop_model::TimelineRowKind::UserMessage,
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
fn a_sent_image_is_drawn_where_it_sat() {
    // The composer's `[Image #1]` is a draft surface: it leaves the text on the
    // way out, and `inline_text_offset` is the only thing that still says where
    // the picture sat. A client that draws the wire text as it stands shows the
    // reader their own message with a hole in it, and no hint that a picture is
    // there at all.
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    let session = seeded_session("session_placeholder1", "image message");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session.clone());
    app.composer.insert_str("before ");
    let label = app
        .attach_image_bytes("image/png", vec![0x89, b'P', b'N', b'G'])
        .expect("the image attaches");
    app.composer.insert_str(" after");
    let (wire_text, attachments) = app
        .perform(Intent::SubmitComposer)
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            vibex_tui::Effect::SendMessage {
                text, attachments, ..
            } => Some((text, attachments)),
            _ => None,
        })
        .expect("the message was sent");
    assert_eq!(
        wire_text, "before  after",
        "the label stays out of the wire text"
    );
    let expected = format!("before {label} after");

    // The row projected from Enter, until the runtime echoes the message.
    let screen = text(&render(&mut app, 100, 30));
    let folded = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        folded.contains(&expected),
        "the optimistic row lost the picture's place:\n{screen}"
    );

    // And the row the echo becomes: the timeline holds the attachments, the
    // row itself does not, so the projection has to put them back.
    let item = seeded_item(
        &session.id,
        1,
        vibex_core::TimelineItemKind::UserMessage,
        vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
            text: wire_text,
            attachments,
            ..Default::default()
        }),
    );
    app.agent
        .state
        .timeline
        .replace_authoritative(session.id.clone(), vec![item]);
    app.abandon_pending_send();
    app.sync_transcript();
    let screen = text(&render(&mut app, 100, 30));
    let folded = screen.split_whitespace().collect::<Vec<_>>().join(" ");
    assert!(
        folded.contains(&expected),
        "the echoed row lost the picture's place:\n{screen}"
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
        screen.contains(COMPOSING_PAGE),
        "the composing page is not on screen:\n{screen}"
    );
    // The empty composer is where the draft's vocabulary is spelled out, and it
    // is the only place: the page itself used to repeat the same three words.
    assert_eq!(
        screen.matches("/ Commands").count(),
        1,
        "the prompt's vocabulary is not in the composer exactly once:\n{screen}"
    );
    assert!(
        screen.contains("@ Files") && screen.contains("$ Skills"),
        "the prompt's vocabulary is incomplete:\n{screen}"
    );
    // The runtime the message will go through is named on the page *and* on the
    // composer's own line, because choosing it is what the page is for.
    assert!(
        screen.contains("Ctrl+G"),
        "the runtime switch is not offered:\n{screen}"
    );
}

#[test]
fn the_first_message_creates_the_session_it_is_written_in() {
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    // The page names the directory the client was started in, and a directory
    // chosen for a previous session is not silently reused in its place.
    assert_eq!(
        app.new_session_workspace(),
        std::env::current_dir()
            .expect("a working directory")
            .display()
            .to_string()
    );
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
                ..
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
    let session_id = app.selected_session_id().unwrap().clone();
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
    // The lights cross the word, so two phases of the pass cannot agree.
    let first = vibex_tui::logo::rows(&theme, 0, 80, true);
    let later = vibex_tui::logo::rows(&theme, 20, 80, true);
    assert_ne!(text_of(&first), text_of(&later), "the light does not move");
    // The letters are the same in every frame: a pass that redrew them would
    // make a page that is waiting look like a page that is loading.
    let waxing = vibex_tui::logo::rows(&theme, 3, 80, true);
    assert_eq!(
        waxing
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
        later
            .iter()
            .map(|line| line.to_string())
            .collect::<Vec<_>>(),
        "the light redrew the mark"
    );
    // Between passes the mark holds still, and it holds the same mark an
    // untouched client sits on for the rest of the session.
    let resting = text_of(&vibex_tui::logo::rows(
        &theme,
        vibex_tui::logo::SWEEP_FRAMES,
        80,
        true,
    ));
    assert_eq!(
        resting,
        text_of(&vibex_tui::logo::rows(&theme, 997, 80, true))
    );
    // A page that is not waiting draws that same resting mark, and never a
    // frame of the animation.
    assert_eq!(
        resting,
        text_of(&vibex_tui::logo::rows(&theme, 0, 80, false))
    );

    // A page that waits animates; every other page still holds still. The
    // client opens on the page that waits, so the list is the still one.
    let mut app = app(100, 30);
    app.perform(Intent::GotoSessions);
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

/// The mark with its styling: the light changes, and so does the mark it draws.
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
    pick_entry(&mut app, 1);
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
fn the_new_session_page_names_the_agent_it_will_be_created_with() {
    // The reader leaves the open session to write a new one. The page answers
    // for itself: it names the entry the creation carries — with nothing chosen
    // on the page, the catalogue's first available entry — and never the Agent
    // of the session behind it, which is how a page came to promise an Agent
    // the session was never created with.
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
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let open = seeded_session("session_page000001", "the session behind the page");
    app.agent
        .apply_sessions(Ok(vec![open.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(open.id.clone());
    app.agent.state.active_session.resolve(open);
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![option("claude", "claude-sonnet"), option("codex", "gpt-5")],
    });
    // The session behind the page is on the *second* entry, so a page that read
    // the session instead of itself would name codex.
    let desired = app.runtime_options.as_ref().expect("catalogue").options[1]
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

    app.perform(Intent::NewSession);
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("claude"),
        "the page does not name the entry it will be created with:\n{screen}"
    );
    assert!(
        !screen.contains("codex"),
        "the page named the session behind it:\n{screen}"
    );
}

#[test]
fn the_new_session_page_does_not_wear_the_session_behind_it() {
    // A reader who leaves a running session to write a new one is on a page
    // with no session of its own. The turn line, the clock and the composer's
    // status answer for the page: a running turn behind it, a turn streaming
    // into it, and its plan are all that session's own page, not this one.
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let mut behind = seeded_session("session_behind0001", "a long turn behind the page");
    behind.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![behind.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(behind.id.clone());
    app.agent.state.active_session.resolve(behind.clone());
    app.navigate_to(Page::Agent);
    // The clock is the session's: it has been running for a while.
    app.turn_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(189));
    app.turn_tokens = Some(4_096);
    app.sync_transcript();
    assert!(app.is_animating(), "the running session is not animating");
    assert!(app.turn_elapsed().is_some());
    let session_screen = text(&render(&mut app, 120, 40));
    // The turn line answers with the phase the turn is in — a session that has
    // not produced anything yet says so rather than claiming a phase it has
    // not reached — so either word is the page reading as running.
    assert!(
        session_screen.contains(app.strings.phase_preparing())
            || session_screen.contains(app.strings.running()),
        "the session's own page does not read as running:\n{session_screen}"
    );

    app.perform(Intent::NewSession);
    app.sync_turn_clock();

    // The page's own answers: no turn, no clock, no tokens, no activity.
    assert!(
        !app.turn_reads_running(),
        "the page read the session as running"
    );
    assert!(!app.session_running());
    assert!(
        app.turn_elapsed().is_none(),
        "the page kept the session's clock"
    );
    assert!(app.turn_tokens().is_none());
    assert!(app.current_activity().is_none());
    assert!(
        !app.transcript_animating(),
        "the page animates for a transcript it does not draw"
    );
    // The mark still shines while the page waits, so the page is alive — but
    // for itself, not for the turn behind it.
    assert!(app.composing_page_shines());
    // The clock itself keeps counting: it is the session's, and the reader who
    // goes back to it must find the turn's real elapsed time.
    assert!(
        app.turn_started.is_some(),
        "the session's clock was thrown away"
    );

    let page_screen = text(&render(&mut app, 120, 40));
    assert!(
        page_screen.contains(COMPOSING_PAGE),
        "the composing page is not on screen:\n{page_screen}"
    );
    assert!(
        !page_screen
            .chars()
            .any(|character| "⠋⠙⠹⠸⠼⠴⠦⠧".contains(character)),
        "the session's turn spinner is on the composing page:\n{page_screen}"
    );
    assert!(
        !page_screen.contains("3m09s") && !page_screen.contains("189"),
        "the session's clock is on the composing page:\n{page_screen}"
    );
    assert!(
        !page_screen.contains("4.1k") && !page_screen.contains("4096"),
        "the session's tokens are on the composing page:\n{page_screen}"
    );
    assert!(
        !page_screen.contains("Steer · Ctrl+S"),
        "the page offers to steer a turn it does not have:\n{page_screen}"
    );
    assert!(
        page_screen.contains("/ Commands"),
        "the empty page does not offer its own prompt:\n{page_screen}"
    );
}

#[test]
fn steering_on_the_new_session_page_keeps_the_draft_out_of_the_session_behind() {
    // `Ctrl+S` steers the turn of the session the reader is *in*. On the page
    // where a session is being written there is no such turn, and steering the
    // one behind the page would deliver the draft to the Agent the reader is
    // leaving — the exact Agent they did not choose.
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let mut behind = seeded_session("session_behind0002", "codex turn");
    behind.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![behind.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(behind.id.clone());
    app.agent.state.active_session.resolve(behind);

    app.perform(Intent::NewSession);
    app.composer.insert_str("this belongs to the new session");
    let outcome = app.perform(Intent::SteerRunningTurn);
    assert!(
        outcome.effects.is_empty(),
        "the draft was steered into the session behind the page: {outcome:?}"
    );
    assert_eq!(
        app.composer.text(),
        "this belongs to the new session",
        "the draft left the composer for another session"
    );
    assert!(
        app.toast.is_some(),
        "the refusal says nothing to the reader"
    );
}

#[test]
fn the_composing_page_never_names_the_agent_behind_it() {
    // The catalogue has not arrived — the page cannot name the entry it will
    // create with — and the session behind it is a codex one. The page must
    // still not name that Agent: it names the entry a creation with no choice
    // would use once the catalogue lands, and says the runtime is unavailable
    // when it never does.
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let mut open = seeded_session("session_behind0003", "codex session");
    open.agent_id = vibex_core::AgentId::parse("codex").expect("agent id");
    app.agent
        .apply_sessions(Ok(vec![open.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(open.id.clone());
    app.agent.state.active_session.resolve(open);
    app.perform(Intent::NewSession);

    assert_eq!(
        app.composer_runtime_labels().0,
        app.strings.runtime_unavailable()
    );
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        !screen.contains("codex"),
        "the page named the Agent behind it:\n{screen}"
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
    app.navigate_to(Page::Agent);
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
    app.navigate_to(Page::Agent);
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

#[test]
fn a_typed_space_moves_the_caret_with_it() {
    // The caret is the reader's only feedback about where the next character
    // lands, and a space is the one character that draws nothing. When the row
    // stopped at the last *word*, typing a space left the caret where it was
    // and the space invisible until the next word arrived.
    let mut app = app(100, 30);
    app.navigate_to(Page::Agent);
    enter_session(&mut app, "session_space0001");
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.insert_str("hello");
    assert_eq!(app.composer.cursor_cell(60).1, 5);

    for (after, column) in [(' ', 6), (' ', 7), ('x', 8)] {
        app.composer.insert_char(after);
        assert_eq!(
            app.composer.cursor_cell(60).1,
            column,
            "the caret did not follow {after:?}"
        );
    }
    // And the row the frame paints carries the spaces, so the caret is drawn
    // past them rather than on top of the last letter.
    let row = &app.composer.display_lines(60)[0].0;
    assert_eq!(row, "hello  x");

    // A draft that is nothing but spaces is still a row for the caret to sit on.
    app.composer.set_text("   ");
    app.composer.move_to_end();
    assert_eq!(app.composer.cursor_cell(60).1, 3);

    // The frame still draws, and the terminal cursor is asked for the right
    // cell: this is the reader-visible half of the same claim.
    let _ = render(&mut app, 100, 30);
    let (row, column) = app.composer.cursor_cell(60);
    assert_eq!((row, column), (0, 3), "{:?}", app.regions.composer);
}

#[test]
fn a_sent_message_is_on_screen_before_the_runtime_echoes_it() {
    // The client does not own the timeline: the authoritative copy of the
    // reader's own message arrives a round trip later. A transcript that waits
    // for it reads as a client that dropped the message, and a status band that
    // still says idle reads as one that ignored Enter.
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let session = seeded_session("session_pending0001", "pending send");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session.clone());
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;

    app.composer.insert_str("what is the plan?");
    let outcome = app.perform(Intent::SubmitComposer);
    assert!(outcome.effects.iter().any(|effect| matches!(
        effect,
        vibex_tui::Effect::SendMessage { text, .. } if text == "what is the plan?"
    )));

    // Immediately: the message is in the transcript, and the turn reads running.
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        screen.contains("what is the plan?"),
        "the message is not on screen yet:\n{screen}"
    );
    assert!(
        app.turn_reads_running(),
        "the session reads idle while the send is in flight"
    );
    // A held message must not be released into the round trip either: the
    // runtime has not reported the turn yet, so releasing now would interleave
    // two turns.
    app.enqueue_message("held for later".to_string());
    assert!(
        app.drain_queue().is_empty(),
        "the queue ran while the first message was still in flight"
    );
    assert_eq!(queued_texts(&app), vec!["held for later".to_string()]);
    assert!(app.is_animating(), "nothing is turning while it waits");

    let pending = app.pending_send_for_active().unwrap().clone();
    // The echo lands: the projection is dropped, and the message is still there
    // exactly once — the reader sees one message, not two.
    app.agent.state.timeline.replace_authoritative(
        session.id.clone(),
        vec![seeded_item(
            &session.id,
            1,
            vibex_core::TimelineItemKind::UserMessage,
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: "what is the plan?".to_string(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        )],
    );
    app.agent.state.timeline.items[0].correlation_id = Some(pending.correlation_id);
    app.finish_send(&session.id, pending.serial);
    assert!(
        app.settle_pending_send(),
        "the echo did not settle the send"
    );
    app.sync_transcript();
    // The clock and the animation stop with the send: an idle client is back to
    // zero frames rather than repainting for the rest of the session.
    app.sync_turn_clock();
    assert!(!app.turn_reads_running());
    assert!(app.turn_elapsed().is_none(), "the elapsed readout ran on");
    assert!(
        !app.is_animating(),
        "the client is still animating while idle"
    );
    let screen = text(&render(&mut app, 120, 40));
    assert_eq!(
        screen.matches("what is the plan?").count(),
        1,
        "the message was drawn twice:\n{screen}"
    );
}

#[test]
fn a_send_the_runtime_refuses_stops_being_projected() {
    // The projected row is a promise. When the send fails, the promise has to
    // be withdrawn — a message that never landed must not sit in the
    // transcript.
    use vibex_tui::action::Intent;
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let session = seeded_session("session_pending0002", "refused send");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer.insert_str("this one fails");
    let _ = app.perform(Intent::SubmitComposer);
    let screen = text(&render(&mut app, 120, 40));
    assert!(screen.contains("this one fails"), "{screen}");

    assert!(app.abandon_pending_send());
    app.sync_transcript();
    let screen = text(&render(&mut app, 120, 40));
    assert!(
        !screen.contains("this one fails"),
        "a refused send kept its row:\n{screen}"
    );
    assert!(!app.turn_reads_running());
}

#[test]
fn the_send_that_is_still_in_flight_reads_as_running() {
    // The reader's only evidence that Enter worked is the transcript row and the
    // turn line. Both have to answer before the runtime does.
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    let session = seeded_session("session_pending0004", "pending indicator");
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session);
    app.navigate_to(Page::Agent);
    app.focus = vibex_tui::app::Focus::Composer;

    // Idle before the send, and the turn line says so.
    assert!(!app.turn_reads_running());
    app.composer.insert_str("go");
    app.perform(Intent::SubmitComposer);
    assert!(
        app.turn_reads_running(),
        "the send is not counted as running"
    );

    let screen = text(&render(&mut app, 100, 30));
    assert!(screen.contains("go"), "{screen}");
    // The running label, not the idle mark: the band is the second half of the
    // feedback, and it is the half the reader looks at when the transcript is
    // scrolled away.
    assert!(
        screen.contains(app.strings.running()),
        "the turn line still reads idle:\n{screen}"
    );
}

/// A running session is a list of rows, not a wall of text.
///
/// Every reasoning paragraph in full, every tool call with its JSON payload,
/// and the runtime's name repeated on each row: the three of them together are
/// what made a working session unreadable — the rows the reader is scanning for
/// were buried in the evidence.
#[test]
fn a_running_session_reads_as_rows() {
    let mut app = app(110, 26);
    app.navigate_to(Page::Agent);
    let session_id = vibex_core::VibexSessionId::new();
    let attribution = Some("DeepSeek Harness · bai · deepseek-v4.1-flash".to_string());
    let tool = |index: i64, command: &str, status: vibex_core::ToolCallStatus| {
        seeded_item(
            &session_id,
            index,
            vibex_core::TimelineItemKind::ToolCall,
            vibex_core::TimelinePayload::ToolCall(vibex_core::ToolCallPayload {
                tool_call_id: format!("call-{index}"),
                tool_name: "execute".to_string(),
                status,
                summary: "execute".to_string(),
                input_summary: Some(format!("{{\"command\":\"{command}\"}}")),
                output_summary: None,
                raw_extension: None,
            }),
        )
    };
    let user_item = seeded_item(
        &session_id,
        1,
        vibex_core::TimelineItemKind::UserMessage,
        vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
            text: "optimise the timeline".to_string(),
            attachments: Vec::new(),
            ..Default::default()
        }),
    );
    let thought = seeded_item(
        &session_id,
        2,
        vibex_core::TimelineItemKind::Reasoning,
        vibex_core::TimelinePayload::Reasoning(vibex_core::ReasoningPayload {
            text: "FIRST-SENTINEL the timeline shows every reasoning paragraph in full.\n\n\
                   The block cache measures each block once, so a body that grew\n\
                   without a bound would push the session off the screen.\n\n\
                   The fix is a window on the tail of the thought.\n\n\
                   The newest rows stay and the oldest leave the top.\n\n\
                   A fold marker says that older rows are above.\n\n\
                   The rail says how tall the window is.\n\n\
                   LAST-SENTINEL keep a dense row one row tall while it streams."
                .to_string(),
            is_final: false,
        }),
    );
    // The projection stamps every row with the runtime that produced it; the
    // client would otherwise show it as the reader's own.
    let with_attribution = |app: &mut App| {
        let mut blocks = app.transcript.blocks().to_vec();
        for block in &mut blocks {
            block.runtime_attribution = attribution.clone();
        }
        app.transcript.set_blocks(blocks);
    };
    app.agent.state.selected_session_id = Some(session_id.clone());
    // While the Agent is thinking, the thought is the last thing it produced:
    // that is the row the window belongs to.
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), vec![user_item.clone(), thought.clone()]);
    app.sync_transcript();
    with_attribution(&mut app);
    let screen = text(&render(&mut app, 110, 26));

    // A running thought is the one dense row whose body is drawn without being
    // opened. It is drawn in a fixed window on its tail: the newest rows are
    // there, the oldest have left the top, and the rail marks the window's
    // height — one cell per row, the header included.
    assert!(
        screen.contains("LAST-SENTINEL"),
        "the newest rows of the thought are not on screen:\n{screen}"
    );
    assert!(
        !screen.contains("FIRST-SENTINEL"),
        "the window grew to the whole thought:\n{screen}"
    );
    assert!(
        screen.contains("┃ …"),
        "the fold marker is missing from the window:\n{screen}"
    );
    let thinking = screen
        .lines()
        .position(|line| line.contains("Thinking…"))
        .expect("the running thought's header");
    let railed = screen
        .lines()
        .skip(thinking)
        .take_while(|line| line.starts_with("  ┃"))
        .count();
    assert!(railed >= 2, "the live window is not drawn:\n{screen}");
    assert!(
        railed <= 1 + vibex_tui::transcript::STREAMING_WINDOW_LINES,
        "the window grew past its bound: {railed} rows:\n{screen}"
    );

    // The Agent moves on to a tool call. The runtime does not close a reasoning
    // stream, so the row still says it is streaming — but it is no longer the
    // tail, and the window folds to the one row a finished thought keeps.
    app.agent.state.timeline.replace_authoritative(
        session_id.clone(),
        vec![
            user_item,
            thought,
            tool(
                3,
                "cd /home/peatboy/code/peatboy/vibex-dev/vibex && git diff -- crates/vibex-tui/src/app.rs",
                vibex_core::ToolCallStatus::Started,
            ),
            tool(
                4,
                "cargo test -p vibex-tui --offline --test render",
                vibex_core::ToolCallStatus::Completed,
            ),
            tool(
                5,
                "cargo clippy -p vibex-tui --all-targets --offline",
                vibex_core::ToolCallStatus::Completed,
            ),
        ],
    );
    app.sync_transcript();
    with_attribution(&mut app);
    let screen = text(&render(&mut app, 110, 26));

    assert!(
        !screen.contains("LAST-SENTINEL"),
        "a thought the Agent has left behind kept its window open:\n{screen}"
    );
    assert!(
        screen.contains("▸ Thinking"),
        "the finished thought is not one row:\n{screen}"
    );
    // A tool row names its action; the payload stays behind the fold. The run's
    // head is the row that carries it, and the rest are counted beside it. The
    // command shares its row with the rest of the run, so what is asserted is
    // the head of it; the tail belongs to the expanded body.
    assert!(
        screen.contains("cd /home/peatboy/code/peatboy/vibex-dev/vibex && git diff"),
        "the tool's command is not on its row:\n{screen}"
    );
    assert!(
        !screen.contains("\"command\""),
        "a raw payload is on screen:\n{screen}"
    );
    // Three work items in a row are a run, with the rest counted.
    assert!(screen.contains("+2"), "the run is not folded:\n{screen}");
    // The runtime is named once for the run, not on every row of it — and not
    // under the reader's own message, which no runtime wrote.
    assert_eq!(
        screen.matches("DeepSeek Harness").count(),
        0,
        "the attribution repeats:\n{screen}"
    );
    let user_row = screen
        .lines()
        .position(|line| line.contains("optimise the timeline"))
        .expect("the reader's message");
    assert!(
        !screen
            .lines()
            .nth(user_row + 1)
            .unwrap_or_default()
            .contains("DeepSeek"),
        "the reader's own message is attributed to a runtime:\n{screen}"
    );
}

#[test]
fn the_runtime_chosen_on_the_new_session_page_is_not_the_old_sessions() {
    // With a session open, pressing `n` opens the composing page — and the
    // client still has that session selected. The picker was answering the
    // selected session first, so choosing an Agent for the *new* session
    // switched the old one's runtime instead (and failed, if the old session
    // could not move), leaving the new session to be created on whatever the
    // catalogue happened to offer first.
    use vibex_tui::action::Intent;
    let option = |agent: &str, model: &str| vibex_core::SessionRuntimeOption {
        selection: vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse(agent).expect("agent id"),
            vibex_core::ProviderProfileId::new(),
            model,
        ),
        agent_label: agent.to_string(),
        auth_source_label: "bai".to_string(),
        model_label: model.to_string(),
        reasoning_efforts: Vec::new(),
        modes: Vec::new(),
        features: Vec::new(),
        availability: vibex_core::RuntimeOptionAvailability::Available,
    };
    let mut app = app(120, 40);
    app.live = vibex_tui::app::LiveState::Ready;
    let open = seeded_session("session_runtime0001", "the session behind the page");
    app.agent
        .apply_sessions(Ok(vec![open.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(open.id.clone());
    app.agent.state.active_session.resolve(open.clone());
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![
            option("claude", "claude-sonnet"),
            option("deepseek", "deepseek-v4.1-flash"),
        ],
    });

    app.perform(Intent::NewSession);
    assert_eq!(app.page, vibex_tui::app::Page::NewSession);
    // The picker opens on the page's own choice, and a choice made there stays
    // on the page: nothing is dispatched at the session behind it.
    app.show_runtime_picker();
    pick_entry(&mut app, 1);
    let outcome = app.perform(Intent::ConfirmOverlay);
    assert!(
        outcome.effects.is_empty(),
        "the choice was applied to a session: {:?}",
        outcome.effects
    );
    assert_eq!(
        app.new_session_runtime
            .as_ref()
            .and_then(|selection| selection.model.model_id())
            .map(str::to_string),
        Some("deepseek-v4.1-flash".to_string()),
        "the page's choice was not kept"
    );

    // And it is what the session is created with.
    app.composer.insert_str("what is this project?");
    let created = app
        .perform(Intent::SubmitComposer)
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            vibex_tui::Effect::CreateSession { runtime, .. } => Some(runtime),
            _ => None,
        })
        .expect("no session was asked for")
        .expect("the page's choice was dropped");
    assert_eq!(
        created.model.model_id(),
        Some("deepseek-v4.1-flash"),
        "the session would be created on another runtime"
    );
    assert_eq!(
        created.agent_id,
        vibex_core::AgentId::parse("deepseek").unwrap()
    );
}

#[test]
fn sending_from_the_new_session_page_lands_in_the_session() {
    // The page asked for a session and the runtime takes a moment to make one.
    // Waiting on the page for that answer reads as nothing having happened at
    // all — the reader sent a message and watched a logo — so the send moves
    // them into the session view, with the message already on its timeline.
    use vibex_tui::action::Intent;
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    let old = seeded_session("session_landing0001", "the session behind the page");
    app.agent
        .apply_sessions(Ok(vec![old.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(old.id.clone());
    app.agent.state.active_session.resolve(old);

    app.perform(Intent::NewSession);
    app.composer.insert_str("what is this project?");
    app.perform(Intent::SubmitComposer);

    // Straight into the session view, with the message on the timeline and the
    // turn reading as running — the session itself is still being created.
    assert_eq!(app.page, vibex_tui::app::Page::Agent);
    assert!(
        app.pending_send_for_active()
            .is_some_and(|pending| pending.session_id.as_ref() == app.selected_session_id()),
        "the message is not projected while the session is created"
    );
    assert!(
        app.turn_reads_running(),
        "the turn does not read as running"
    );
    assert!(app.is_animating(), "nothing is turning");
    // The session the reader came from is not on screen: what is about to
    // appear here is a new one, and its history is not this session's.
    assert!(
        app.selected_session_id().is_some(),
        "the pending session has no reserved identity"
    );
    assert!(app.active_session().is_none());
    let screen = text(&render(&mut app, 110, 30));
    assert!(
        screen.contains("what is this project?"),
        "the message is not on the timeline:\n{screen}"
    );
    assert!(
        !screen.contains("the session behind the page"),
        "the old session's view is still on screen:\n{screen}"
    );
    assert!(
        !screen.contains(COMPOSING_PAGE),
        "the composing page is still on screen:\n{screen}"
    );
    // The runtime answers: the session opens, the message is sent into it, and
    // the projection moves to the session it belongs to.
    let mut created = seeded_session("session_landing0002", "created session");
    created.id = app.selected_session_id().unwrap().clone();
    let effect = app
        .pending_send_effect(created.id.clone())
        .expect("the held message is sent");
    match effect {
        vibex_tui::Effect::SendMessage {
            session_id, text, ..
        } => {
            assert_eq!(session_id, created.id);
            assert_eq!(text, "what is this project?");
        }
        other => panic!("unexpected effect: {other:?}"),
    }
    app.open_session(created.id.clone());
    app.agent
        .apply_sessions(Ok(vec![created.clone()]))
        .expect("sessions apply");
    app.agent.state.active_session.resolve(created.clone());
    assert!(
        app.pending_send_for_active()
            .is_some_and(|pending| pending.session_id.as_ref() == Some(&created.id)),
        "the projection did not follow the session it was sent to"
    );
    app.sync_transcript();
    let screen = text(&render(&mut app, 110, 30));
    assert_eq!(
        screen.matches("what is this project?").count(),
        1,
        "the message is not drawn exactly once:\n{screen}"
    );
}

#[test]
fn a_new_session_that_could_not_be_created_goes_back_to_its_page() {
    // The other half of landing in the session: if the runtime never makes the
    // session, the reader is put back where the message can be sent again —
    // with the words still in the box, and no phantom session view.
    use vibex_tui::action::Intent;
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    app.composer.insert_str("this one will not be created");
    app.perform(Intent::SubmitComposer);
    assert_eq!(app.page, vibex_tui::app::Page::Agent);

    // What `AppMessage::SessionCreated(Err(..))` does: withdraw the projection,
    // put the draft back, and return to the page.
    let request_id = app.selected_session_id().unwrap().clone();
    assert!(app.fail_creation(&request_id));
    assert_eq!(app.composer.text(), "this one will not be created");
    assert!(app.pending_sends.is_empty());
    assert!(!app.turn_reads_running());
    let screen = text(&render(&mut app, 110, 30));
    assert!(screen.contains(COMPOSING_PAGE), "{screen}");
}

#[test]
fn the_creating_session_view_waits_for_its_session() {
    // The landing has to survive the round trip: the view names the runtime the
    // session is being made with, the turn reads as running, the message is on
    // screen — and an `Esc` there does not walk the reader into another
    // session's view, which does not exist yet.
    use vibex_tui::action::Intent;
    let option = |agent: &str, model: &str| vibex_core::SessionRuntimeOption {
        selection: vibex_core::SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse(agent).expect("agent id"),
            vibex_core::ProviderProfileId::new(),
            model,
        ),
        agent_label: agent.to_string(),
        auth_source_label: "bai".to_string(),
        model_label: model.to_string(),
        reasoning_efforts: Vec::new(),
        modes: Vec::new(),
        features: Vec::new(),
        availability: vibex_core::RuntimeOptionAvailability::Available,
    };
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    // The entry the reader picks is *not* the catalogue's first, so a view that
    // named the default here would name an Agent the session is not being made
    // with.
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![
            option("claude", "claude-sonnet"),
            option("deepseek", "deepseek-v4.1-flash"),
        ],
    });
    app.perform(Intent::NewSession);
    app.show_runtime_picker();
    pick_entry(&mut app, 1);
    app.perform(Intent::ConfirmOverlay);
    assert_eq!(
        app.composer_runtime_labels().0,
        "deepseek",
        "the page did not take the reader's pick"
    );
    app.composer.insert_str("first message");
    app.perform(Intent::SubmitComposer);

    let screen = text(&render(&mut app, 110, 30));
    assert!(screen.contains("first message"), "{screen}");
    assert!(
        screen.contains("deepseek · deepseek-v4.1-flash"),
        "the runtime being used is not named:\n{screen}"
    );
    assert!(
        !screen.contains("claude"),
        "the creating view named the catalogue's default:\n{screen}"
    );
    assert!(
        screen.contains(app.strings.running()) || screen.contains("ctrl"),
        "the view does not read as working:\n{screen}"
    );
    // Nothing about a session that does not exist yet is selectable state: the
    // view is the new session's, and only its own message is in it.
    assert!(app.active_session().is_none());
    assert_eq!(app.transcript.len(), 1);
}

#[test]
fn an_answer_reaches_the_timeline_as_it_is_written() {
    // The question this answers: a streamed answer is not buffered until the
    // turn ends. Each delta the runtime sends grows the row on screen, and the
    // client keeps repainting while they arrive.
    let session_id = vibex_core::VibexSessionId::new();
    let correlation = vibex_core::CorrelationId::new();
    let mut user = seeded_item(
        &session_id,
        1,
        vibex_core::TimelineItemKind::UserMessage,
        vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
            text: "explain the density rule".into(),
            attachments: Vec::new(),
            ..Default::default()
        }),
    );
    user.correlation_id = Some(correlation.clone());
    let delta = |sequence: i64, chunk: u32, text: &str| {
        let mut item = seeded_item(
            &session_id,
            sequence,
            vibex_core::TimelineItemKind::AgentMessage,
            vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
                text_delta: text.to_string(),
                chunk_index: chunk,
                phase: Some(vibex_core::AgentMessagePhase::FinalAnswer),
            }),
        );
        item.correlation_id = Some(correlation.clone());
        item
    };

    let mut app = app(120, 30);
    app.navigate_to(Page::Agent);
    let mut session = seeded_session("session_probe", "streaming");
    session.id = session_id.clone();
    session.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("apply");
    app.agent.state.selected_session_id = Some(session_id.clone());
    app.agent.state.active_session.resolve(session.clone());
    app.agent
        .state
        .timeline
        .replace_authoritative(session_id.clone(), vec![user.clone()]);
    app.sync_transcript();

    // Three deltas, three frames: each one shows more than the last, on the
    // same row — the answer is being written, not assembled in secret.
    let mut seen = String::new();
    for (sequence, chunk) in [
        (2, "The rule is "),
        (3, "one row per work item"),
        (4, ", detail on demand."),
    ] {
        app.agent
            .state
            .timeline
            .apply_live(vibex_core::TimelineLiveEvent {
                session_id: session_id.clone(),
                sequence,
                item: delta(sequence, sequence as u32 - 2, chunk),
            });
        app.sync_transcript();
        assert!(
            app.is_animating(),
            "the client stopped repainting mid-answer"
        );
        // Compared with whitespace folded: the frame wraps the answer, so a
        // phrase can straddle two display rows.
        let screen = text(&render(&mut app, 120, 30));
        let folded = screen.split_whitespace().collect::<Vec<_>>().join(" ");
        let expected = format!("{seen}{chunk}");
        let expected = expected.split_whitespace().collect::<Vec<_>>().join(" ");
        assert!(
            folded.contains(&expected),
            "the frame does not show the answer so far ({expected:?}):\n{screen}"
        );
        seen.push_str(chunk);
    }

    // One row, not three: the deltas merge into the message being written.
    assert_eq!(
        app.transcript
            .blocks()
            .iter()
            .filter(|block| block.kind == vibex_desktop_model::TimelineRowKind::AgentMessage)
            .count(),
        1,
        "each delta became its own row"
    );

    // And when the runtime says the turn is over, the row stops claiming to
    // stream — the last frame is not a spinner over a finished answer.
    let mut settled = session.clone();
    settled.state = vibex_core::AgentSessionState::Idle;
    app.agent.state.active_session.resolve(settled.clone());
    app.agent.apply_sessions(Ok(vec![settled])).expect("apply");
    app.sync_transcript();
    assert!(
        !app.transcript_animating(),
        "a finished answer still streams"
    );
}

#[test]
fn the_workspace_key_on_the_new_session_page_picks_a_directory() {
    // The page names the directory and offers the key that changes it. The key
    // used to list workspaces into state nothing drew, and moved the reader off
    // the page while it did: pressing it looked like nothing happened, which is
    // exactly what the page's own hint promised it would not.
    use vibex_tui::action::Intent;
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::NewSession);
    app.composer.insert_str("keep my draft");
    let page = app.page;

    // Ctrl+W: the picker opens over the runtime's listing, on this page.
    let outcome = app.perform(Intent::SwitchWorkspace);
    assert!(
        matches!(
            app.overlay,
            Some(vibex_tui::app::Overlay::WorkspacePicker { .. })
        ),
        "no picker opened: {:?}",
        app.overlay
    );
    assert_eq!(app.page, page, "the reader was moved off the page");
    assert!(
        outcome
            .effects
            .iter()
            .any(|effect| matches!(effect, vibex_tui::Effect::BrowseDirectories { .. })),
        "the listing was not asked for: {:?}",
        outcome.effects
    );
    // The page is still readable behind the picker, with the draft intact.
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home/peatboy".to_string(),
        parent: Some("/home".to_string()),
        entries: vec![vibex_core::RemoteWorkspaceDirectoryEntry {
            name: "vibex-dev".to_string(),
            path: "/home/peatboy/vibex-dev".to_string(),
        }],
    });
    let screen = text(&render(&mut app, 110, 30));
    assert!(
        screen.contains("vibex-dev"),
        "the listing is not drawn:\n{screen}"
    );

    // Enter chooses it, and the page names the directory it will use. The
    // cursor starts on the `..` row the listing carries, so the directory is
    // one step below it.
    app.perform(Intent::SelectNext);
    app.perform(Intent::ConfirmOverlay);
    assert!(app.overlay.is_none());
    assert_eq!(
        app.new_session_workspace(),
        "/home/peatboy/vibex-dev",
        "the chosen directory was not kept"
    );
    assert_eq!(app.page, page);
    assert_eq!(app.composer.text(), "keep my draft", "the draft was lost");

    // And the session is created in it.
    let created = app
        .perform(Intent::SubmitComposer)
        .effects
        .into_iter()
        .find_map(|effect| match effect {
            vibex_tui::Effect::CreateSession { workspace_root, .. } => Some(workspace_root),
            _ => None,
        })
        .expect("no session was asked for");
    assert_eq!(created, "/home/peatboy/vibex-dev");
}

#[test]
fn the_workspace_picker_draws_the_way_out_of_the_directory() {
    // The picker opens on one directory, so a reader choosing a workspace
    // somewhere else on the machine has to be able to see how to leave it: the
    // parent is drawn as the `..` row above the listing, and the footer names
    // the key that takes the same step. A directory with nothing in it is not a
    // dead end either — the way up is still drawn above it.
    use vibex_tui::action::Intent;
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.perform(Intent::SwitchWorkspace);
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home/peatboy".to_string(),
        parent: Some("/home".to_string()),
        entries: vec![vibex_core::RemoteWorkspaceDirectoryEntry {
            name: "vibex-dev".to_string(),
            path: "/home/peatboy/vibex-dev".to_string(),
        }],
    });
    let lines = render(&mut app, 110, 30);
    let screen = text(&lines);
    let up = lines
        .iter()
        .position(|line| line.contains(".."))
        .expect("the way up is not drawn");
    let entry = lines
        .iter()
        .position(|line| line.contains("vibex-dev"))
        .expect("the listing is not drawn");
    assert!(up < entry, "the way up is below the listing:\n{screen}");
    assert!(
        screen.contains(app.strings.workspace_parent()),
        "the footer does not name the way up:\n{screen}"
    );

    // An empty directory still offers the step out of it.
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home/peatboy/empty".to_string(),
        parent: Some("/home/peatboy".to_string()),
        entries: vec![],
    });
    let screen = text(&render(&mut app, 110, 30));
    assert!(
        screen.contains(".."),
        "an empty directory has no way out:\n{screen}"
    );
    assert!(
        !screen.contains(app.strings.workspace_empty()),
        "an empty directory was called empty where the way out belongs:\n{screen}"
    );

    // At the top there is no way up, so nothing offers one — not a row, not a
    // key in the footer.
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home".to_string(),
        parent: None,
        entries: vec![],
    });
    let screen = text(&render(&mut app, 110, 30));
    assert!(
        !screen.contains(".."),
        "a row promises a step that does not exist:\n{screen}"
    );
    assert!(
        !screen.contains(app.strings.workspace_parent()),
        "the footer names a step that does not exist:\n{screen}"
    );
    assert!(
        screen.contains(app.strings.workspace_empty()),
        "an empty root says nothing:\n{screen}"
    );
}

#[test]
fn the_workspace_key_from_the_session_list_does_not_move_the_reader() {
    // The same key is global. Wherever it is pressed, the picker answers that
    // page — a reader choosing a directory from the list is not asking to be
    // dropped into a new session.
    use vibex_tui::action::Intent;
    let mut app = app(110, 30);
    app.live = vibex_tui::app::LiveState::Ready;
    app.navigate_to(Page::Sessions);
    app.perform(Intent::SwitchWorkspace);
    app.workspace_browse = Some(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: vec!["/home".to_string()],
        path: "/home/peatboy".to_string(),
        parent: Some("/home".to_string()),
        entries: vec![vibex_core::RemoteWorkspaceDirectoryEntry {
            name: "notes".to_string(),
            path: "/home/peatboy/notes".to_string(),
        }],
    });
    // The `..` row is above the single directory, so the cursor has to step
    // past it to reach one.
    app.perform(Intent::SelectNext);
    app.perform(Intent::ConfirmOverlay);
    assert_eq!(
        app.page,
        Page::Sessions,
        "the reader was moved to a new session"
    );
    assert_eq!(app.workspace_path.as_deref(), Some("/home/peatboy/notes"));
}

#[test]
fn the_spinner_turns_through_the_quiet_parts_of_a_turn() {
    // The runtime spends whole seconds starting up, thinking, or waiting on a
    // tool, with nothing streaming into the transcript. A spinner held on one
    // frame for that stretch reads as a frozen client — which is exactly what
    // the turn line is there to disprove.
    let mut app = app(110, 30);
    app.navigate_to(Page::Agent);
    let mut session = seeded_session("session_spinner0001", "quiet turn");
    session.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(session.id.clone());
    app.agent.state.active_session.resolve(session);
    // Nothing else moves: no stream, no question, no send in flight.
    app.sync_transcript();
    assert!(!app.transcript_animating(), "the transcript is idle");
    assert!(app.is_animating(), "a running turn is not animating");

    // The spinner is a function of the phase, so a moving one is a phase that
    // moves — and the frame drawn at each phase has to differ.
    let glyph = |app: &mut App| {
        let screen = text(&render(app, 110, 30));
        screen
            .chars()
            .find(|character| "⠋⠙⠹⠸⠼⠴⠦⠧".contains(*character))
            .unwrap_or_else(|| panic!("no spinner on screen:\n{screen}"))
    };
    let mut seen = Vec::new();
    for _ in 0..8 {
        assert!(
            app.advance_transcript_animation(),
            "the client stopped animating mid-turn"
        );
        seen.push(glyph(&mut app));
    }
    assert!(
        seen.iter().collect::<std::collections::HashSet<_>>().len() > 1,
        "the spinner never changed frame: {seen:?}"
    );

    // And the idle contract still holds: a session with nothing running costs
    // no frames at all.
    let mut idle = seeded_session("session_spinner0002", "idle");
    idle.state = vibex_core::AgentSessionState::Idle;
    app.agent
        .apply_sessions(Ok(vec![idle.clone()]))
        .expect("sessions apply");
    app.agent.state.active_session.resolve(idle);
    app.agent.state.selected_session_id = Some(vibex_core::VibexSessionId::new());
    app.sync_transcript();
    assert!(!app.is_animating(), "an idle session is animating");
    assert!(!app.advance_transcript_animation());
}

/// A list row carries what the desktop's sidebar row carries — who is
/// answering, what state it is in, whether anything is new, and when it last
/// said anything. Order and folders come from the arrangement the desktop
/// publishes (see the arrangement test below); this one covers the row's own
/// contents, which are the same either way.
#[test]
fn the_session_list_names_the_agent_and_when_it_last_spoke() {
    let now = vibex_core::unix_timestamp_ms();
    let mut running = seeded_session("session_list0001", "fix the flaky test");
    running.state = vibex_core::AgentSessionState::Running;
    running.last_message_at_ms = now - 3 * 60 * 1000;
    let mut idle = seeded_session("session_list0002", "what is this project?");
    idle.last_message_at_ms = now - 5 * 60 * 60 * 1000;
    let mut failed = seeded_session("session_list0003", "the one that failed");
    failed.state = vibex_core::AgentSessionState::Error;
    failed.last_message_at_ms = now - 20 * 1000;

    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![running.clone(), idle.clone(), failed.clone()]))
        .expect("sessions apply");
    // One Agent for all of them: the catalogue is where its name lives.
    app.runtime_options = Some(vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: vec![vibex_core::RuntimeAgentSummary {
            agent_id: running.agent_id.clone(),
            label: "Claude Code".to_string(),
        }],
        auth_sources: Vec::new(),
        options: Vec::new(),
    });
    app.unread_sessions.insert(idle.id.as_str().to_string());
    let screen = text(&render(&mut app, 110, 24));

    // The Agent's name follows the title, and the time it last spoke trails
    // the row.
    assert!(
        screen.contains("fix the flaky test · Claude Code"),
        "the row does not name the Agent answering it:\n{screen}"
    );
    assert!(screen.contains("3m"), "no relative time:\n{screen}");
    assert!(screen.contains("5h"), "{screen}");
    assert!(screen.contains("now"), "{screen}");
    // The unread mark belongs to the session that has something new, and to no
    // other row.
    let unread_row = screen
        .lines()
        .find(|line| line.contains("what is this project?"))
        .expect("the unread session's row");
    assert!(
        unread_row.contains('●'),
        "the unread session is not marked: {unread_row:?}"
    );
    assert_eq!(
        screen.matches('●').count(),
        1,
        "more than one row claims something new:\n{screen}"
    );
    // States are told apart by shape as well as by colour — and by shape
    // *only*: the word is gone from the row, which is what gives the titles
    // their width back. An idle session is a hollow diamond: alive, and not
    // working.
    for mark in ['▶', '✗', '◇'] {
        assert!(
            screen.contains(mark),
            "missing the {mark:?} mark:\n{screen}"
        );
    }
    for (title, word) in [
        ("fix the flaky test", "Running"),
        ("the one that failed", "Failed"),
        ("what is this project?", "Idle"),
    ] {
        let row = screen
            .lines()
            .find(|line| line.contains(title))
            .unwrap_or_else(|| panic!("no row for {title:?}:\n{screen}"));
        assert!(
            !row.contains(word),
            "the state word is back on the row: {row:?}"
        );
    }
}

/// A session row carries the two controls that act on it — rename and delete —
/// while the reader is on it: the row the cursor is on, and the row under the
/// pointer. Every other row gives their columns back to its title, and each
/// control publishes the rect a click would land on, against its own row.
#[test]
fn a_session_row_offers_its_controls_while_the_reader_is_on_it() {
    use vibex_tui::action::Intent;
    let now = vibex_core::unix_timestamp_ms();
    let mut first = seeded_session("session_ruler0001", "fix the flaky test");
    first.last_message_at_ms = now;
    let mut second = seeded_session("session_ruler0002", "the other one");
    // One workspace, so the two sessions are rows under one heading.
    second.project_id = first.project_id.clone();
    second.workspace_id = first.workspace_id.clone();
    second.workspace_root = first.workspace_root.clone();
    second.last_message_at_ms = now;

    let mut app = app(100, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![first, second]))
        .expect("sessions apply");
    // Row 0 is the workspace heading; rows 1 and 2 are the sessions.
    app.set_selection(Scope::Sessions, 1);
    let screen = text(&render(&mut app, 100, 24));
    let row_of = |needle: &str| {
        screen
            .lines()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle:?}:\n{screen}"))
    };
    let selected = screen.lines().nth(row_of("fix the flaky test")).unwrap();
    assert!(
        selected.trim_end().ends_with("R D"),
        "the row the cursor is on carries no controls: {selected:?}"
    );
    // The controls take the age's place: a row does not answer "when" and "act
    // on this" in the same column.
    assert!(
        !selected.contains("now"),
        "the age is still drawn under the controls: {selected:?}"
    );
    let other = screen.lines().nth(row_of("the other one")).unwrap();
    assert!(
        other.trim_end().ends_with("now"),
        "a row nobody is on does not end in its age: {other:?}"
    );

    // The pair is a click target, and each half names the row it belongs to.
    let controls = app.regions.row_actions.clone();
    assert_eq!(
        controls.len(),
        2,
        "the selected row's controls: {controls:?}"
    );
    assert!(controls.iter().all(|control| control.row == 1));
    for control in &controls {
        assert_eq!(
            control.rect.y,
            row_of("fix the flaky test") as u16,
            "a control was published on another line: {control:?}"
        );
    }
    let rename = controls
        .iter()
        .find(|control| control.intent == Intent::BeginRenameSession)
        .expect("the rename control is not clickable");
    let delete = controls
        .iter()
        .find(|control| control.intent == Intent::DeleteSession)
        .expect("the delete control is not clickable");
    assert!(
        rename.rect.right() <= delete.rect.x && delete.rect.right() <= 100,
        "the pair does not fit the row: {rename:?} {delete:?}"
    );
    // The age keeps the right edge on the rows nobody is on, and the controls
    // end where it does: a row being acted on does not move its right end.
    let age_edge = |line: &str| {
        let at = line
            .find("now")
            .unwrap_or_else(|| panic!("no age on the row: {line:?}"));
        vibex_tui::text::display_width(&line[..at]) + 3
    };
    assert_eq!(
        usize::from(delete.rect.right()),
        age_edge(other),
        "the controls do not end where the age does: {delete:?} {other:?}"
    );

    // The pointer's row answers the same way, without moving the cursor: the
    // reader points at a row to act on it, and the selected row is not it.
    app.hover = Some((Scope::Sessions, 2));
    let screen = text(&render(&mut app, 100, 24));
    let hovered = screen.lines().nth(row_of("the other one")).unwrap();
    assert!(
        hovered.trim_end().ends_with("R D"),
        "the row under the pointer carries no controls: {hovered:?}"
    );
    assert!(
        app.regions
            .row_actions
            .iter()
            .any(|control| control.row == 2),
        "the hovered row's controls are not clickable: {:?}",
        app.regions.row_actions
    );
    assert_eq!(
        app.selection_for(Scope::Sessions),
        1,
        "the pointer moved the cursor"
    );

    // A control answers the pointer before the click does: the one under it
    // lights, and the one beside it does not.
    let resting = render_buffer(&mut app, 100, 24);
    app.hovered_row_action = Some((2, Intent::DeleteSession));
    let lit = render_buffer(&mut app, 100, 24);
    let control = |app: &App, intent: Intent| {
        app.regions
            .row_actions
            .iter()
            .find(|control| control.row == 2 && control.intent == intent)
            .map(|control| control.rect)
            .unwrap_or_else(|| panic!("row 2 published no {intent:?} control"))
    };
    let changed = |rect: ratatui::layout::Rect| {
        (rect.x..rect.right())
            .filter(|column| {
                resting.cell((*column, rect.y)).map(|cell| cell.style())
                    != lit.cell((*column, rect.y)).map(|cell| cell.style())
            })
            .count()
    };
    let delete = control(&app, Intent::DeleteSession);
    assert_eq!(
        changed(delete),
        usize::from(delete.width),
        "the control under the pointer did not light"
    );
    assert_eq!(
        changed(control(&app, Intent::BeginRenameSession)),
        0,
        "the hover spilled onto the control beside it"
    );
}

/// A control names the chord that runs it, in the spelling the key bar and the
/// help page use. A single cell is worth a hint; a chord that would take more
/// than one is left out rather than allowed to widen the row, because the row
/// is a list of titles and not a key reference.
#[test]
fn a_session_rows_controls_spell_the_chord_that_runs_them() {
    use vibex_tui::action::Intent;
    use vibex_tui::keymap::{Chord, Scope};
    let now = vibex_core::unix_timestamp_ms();
    let mut session = seeded_session("session_hint0001", "name the key");
    session.last_message_at_ms = now;
    let mut app = app(100, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![session]))
        .expect("sessions apply");
    app.set_selection(Scope::Sessions, 1);
    fn row(app: &mut App) -> String {
        let screen = text(&render(app, 100, 24));
        screen
            .lines()
            .find(|line| line.contains("name the key"))
            .expect("the session row")
            .to_string()
    }
    let drawn = row(&mut app);
    assert!(
        drawn.trim_end().ends_with("R D") && !drawn.contains('╱'),
        "the row does not name the chords that run its controls: {drawn:?}"
    );

    // A rebound key is spelled the way the key bar spells it.
    app.keymap
        .rebind(Intent::BeginRenameSession, "q".parse::<Chord>().unwrap());
    let drawn = row(&mut app);
    assert!(
        drawn.trim_end().ends_with("Q D"),
        "the control did not follow the binding: {drawn:?}"
    );

    // A chord that takes more than one cell is spelled out rather than
    // truncated: the control is the key, whatever the key is.
    app.keymap.rebind(
        Intent::BeginRenameSession,
        "ctrl+x".parse::<Chord>().unwrap(),
    );
    let drawn = row(&mut app);
    assert!(
        drawn.trim_end().ends_with("Ctrl+X D"),
        "the control does not spell the whole chord: {drawn:?}"
    );
}

/// The columns of the list line up, measured in cells rather than bytes: the
/// rows carry box drawing and geometric glyphs, and the eye scans columns.
#[test]
fn the_session_list_columns_line_up() {
    let now = vibex_core::unix_timestamp_ms();
    let mut first = seeded_session("session_columns0001", "short");
    first.state = vibex_core::AgentSessionState::Running;
    first.last_message_at_ms = now - 3 * 60 * 1000;
    let mut second = seeded_session("session_columns0002", "a much longer title than that one");
    second.state = vibex_core::AgentSessionState::Error;
    second.last_message_at_ms = now - 5 * 60 * 60 * 1000;

    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![first, second]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    let column_of = |line: &str, needle: &str| {
        line.find(needle)
            .map(|at| vibex_tui::text::display_width(&line[..at]))
    };
    let row_of = |needle: &str| {
        screen
            .lines()
            .find(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle:?}:\n{screen}"))
    };
    // The mark, the title and the age are columns: two rows of different title
    // lengths put each of them at the same place.
    assert_eq!(
        column_of(row_of("short"), "▶"),
        column_of(row_of("a much longer title"), "✗"),
        "the state column drifts with the length of a title:\n{screen}"
    );
    assert_eq!(
        column_of(row_of("short"), "short"),
        column_of(row_of("a much longer title"), "a much longer"),
        "the title column follows the group depth:\n{screen}"
    );
    let time_edge = |line: &str, needle: &str| {
        column_of(line, needle).map(|at| at + vibex_tui::text::display_width(needle))
    };
    assert_eq!(
        time_edge(row_of("short"), "3m"),
        time_edge(row_of("a much longer title"), "5h"),
        "the age is not right-aligned:\n{screen}"
    );
}

#[test]
fn an_unread_session_is_marked_until_it_is_opened() {
    // Unread is the client's own notion: the event says an answer finished, and
    // the list knows where the reader was when it did.
    let session_id = vibex_core::VibexSessionId::parse("session_unread0001").unwrap();
    let other = vibex_core::VibexSessionId::parse("session_unread0002").unwrap();
    // A completion is the *final* message item, exactly as the desktop reads it
    // for its own unread marks; a delta is still work in progress.
    let answer = |session_id: &vibex_core::VibexSessionId, finished: bool| {
        let payload = if finished {
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "done".to_string(),
                is_final: true,
            })
        } else {
            vibex_core::TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
                text_delta: "work".to_string(),
                chunk_index: 0,
                phase: None,
            })
        };
        vibex_core::TimelineLiveEvent {
            session_id: session_id.clone(),
            sequence: 7,
            item: seeded_item(
                session_id,
                7,
                vibex_core::TimelineItemKind::AgentMessage,
                payload,
            ),
        }
    };

    let mut app = app(110, 24);
    app.live = vibex_tui::app::LiveState::Ready;
    let mut busy = seeded_session("session_unread0001", "elsewhere");
    busy.id = session_id.clone();
    let mut open = seeded_session("session_unread0002", "in front of me");
    open.id = other.clone();
    app.agent
        .apply_sessions(Ok(vec![busy, open]))
        .expect("sessions apply");
    app.agent.state.selected_session_id = Some(other.clone());

    // A finished answer for the session on screen is not unread.
    assert!(!app.note_activity(&answer(&other, true)));
    assert!(!app.session_is_unread(&other));
    // A delta is not a completion either.
    assert!(!app.note_activity(&answer(&session_id, false)));
    assert!(!app.session_is_unread(&session_id));
    // A finished answer elsewhere is.
    assert!(app.note_activity(&answer(&session_id, true)));
    assert!(app.session_is_unread(&session_id));
    // And opening that session clears it.
    app.open_session(session_id.clone());
    assert!(!app.session_is_unread(&session_id));
}

#[test]
fn the_session_list_marks_degrade_to_a_legacy_terminal() {
    // The marks are part of the row's meaning, so they have to survive a
    // console font: single column, and no character it cannot draw.
    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Authority,
            capability: ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Ascii,
            },
            theme_id: None,
            mode: vibex_ui::GpuiThemeMode::Dark,
            locale: Locale::En,
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.resize(110, 24);
    app.navigate_to(Page::Sessions);
    let mut failed = seeded_session("session_list0101", "the one that failed");
    failed.state = vibex_core::AgentSessionState::Error;
    let mut running = seeded_session("session_list0102", "working");
    running.state = vibex_core::AgentSessionState::Running;
    app.agent
        .apply_sessions(Ok(vec![failed, running.clone()]))
        .expect("sessions apply");
    app.unread_sessions.insert(running.id.as_str().to_string());
    // The cursor is on a session, so the row's controls are drawn and have to
    // survive the same console font the marks do.
    app.set_selection(Scope::Sessions, 1);
    let screen = text(&render(&mut app, 110, 24));
    for mark in ['x', '>', 'o'] {
        assert!(
            screen.contains(mark),
            "missing the {mark:?} mark:\n{screen}"
        );
    }
    assert!(
        screen
            .lines()
            .filter(|line| line.contains("Failed") || line.contains("Running"))
            .all(|line| line.is_ascii()),
        "a legacy console was handed a glyph it cannot draw:\n{screen}"
    );
    let buffer = render_buffer(&mut app, 110, 24);
    let drawn = app
        .regions
        .row_actions
        .iter()
        .map(|control| {
            (
                control.intent,
                buffer
                    .cell((control.rect.x, control.rect.y))
                    .map(|cell| cell.symbol().to_string())
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>();
    assert!(
        drawn.contains(&(
            vibex_tui::action::Intent::BeginRenameSession,
            "R".to_string()
        )) && drawn.contains(&(vibex_tui::action::Intent::DeleteSession, "D".to_string())),
        "the controls do not degrade to the legacy tier: {drawn:?}"
    );
}

/// The list a reader arranged on the desktop is the list this client draws:
/// the same folders, in the same order, with the pins where they left them.
#[test]
fn the_session_list_draws_the_arrangement_the_desktop_published() {
    let mut first = seeded_session("session_arranged001", "recent answer");
    first.last_message_at_ms = 1_759_251_200_000;
    let mut second = seeded_session("session_arranged002", "kept on top");
    second.project_id = first.project_id.clone();
    second.last_message_at_ms = 1_759_251_100_000;
    let mut third = seeded_session("session_arranged003", "put away");
    third.project_id = first.project_id.clone();
    third.last_message_at_ms = 1_759_251_000_000;

    let project_id = first.project_id.as_str().to_string();
    let snapshot = vibex_core::RemoteSidebarOrganizationSnapshot {
        revision: 5,
        folders: vec![vibex_core::RemoteSidebarFolder {
            id: "folder-archive".to_string(),
            name: "archive".to_string(),
            project_id: Some(project_id.clone()),
            workspace_id: None,
            auto_archive_after_days: None,
        }],
        groups: Vec::new(),
        placements: vec![
            vibex_core::RemoteSidebarPlacement {
                item: vibex_core::RemoteSidebarItemRef {
                    kind: vibex_core::RemoteSidebarItemKind::Project,
                    id: project_id.clone(),
                },
                parent_folder_id: None,
            },
            vibex_core::RemoteSidebarPlacement {
                item: vibex_core::RemoteSidebarItemRef {
                    kind: vibex_core::RemoteSidebarItemKind::Session,
                    id: "session_arranged001".to_string(),
                },
                parent_folder_id: None,
            },
            vibex_core::RemoteSidebarPlacement {
                item: vibex_core::RemoteSidebarItemRef {
                    kind: vibex_core::RemoteSidebarItemKind::Folder,
                    id: "folder-archive".to_string(),
                },
                parent_folder_id: None,
            },
            vibex_core::RemoteSidebarPlacement {
                item: vibex_core::RemoteSidebarItemRef {
                    kind: vibex_core::RemoteSidebarItemKind::Session,
                    id: "session_arranged003".to_string(),
                },
                parent_folder_id: Some("folder-archive".to_string()),
            },
        ],
        collapsed_folder_ids: Vec::new(),
        collapsed_group_ids: Vec::new(),
        collapsed_project_ids: Vec::new(),
        collapsed_workspace_ids: Vec::new(),
        pinned_session_ids: vec!["session_arranged002".to_string()],
        session_order: vec![
            "session_arranged001".to_string(),
            "session_arranged002".to_string(),
            "session_arranged003".to_string(),
        ],
        session_order_anchored_at_ms: i64::MAX,
        hierarchy_mode: vibex_core::RemoteSidebarHierarchyMode::Compact,
        project_order: Vec::new(),
        workspace_order: std::collections::BTreeMap::new(),
        project_appearances: std::collections::BTreeMap::new(),
        worktree_titles: std::collections::BTreeMap::new(),
        project_new_session_locations: std::collections::BTreeMap::new(),
        auto_continue_project_ids: Vec::new(),
        auto_continue_session_overrides: std::collections::BTreeMap::new(),
        auto_continue_session_ids: Vec::new(),
        auto_continue_paused_session_ids: Vec::new(),
        unread_session_ids: Vec::new(),
    };

    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![first, second, third]))
        .expect("sessions apply");
    assert!(app.apply_sidebar_organization(&snapshot));
    let screen = text(&render(&mut app, 110, 24));

    // The folder is a row of its own, and it sits where the arrangement put it:
    // after the two sessions, before the one inside it.
    let lines = screen.lines().collect::<Vec<_>>();
    let row_of = |needle: &str| {
        lines
            .iter()
            .position(|line| line.contains(needle))
            .unwrap_or_else(|| panic!("no row for {needle:?}:\n{screen}"))
    };
    let kept = row_of("kept on top");
    let archive = row_of("archive");
    let recent = row_of("recent answer");
    assert!(
        kept < recent && recent < archive,
        "the arrangement's order was not drawn:\n{screen}"
    );
    assert!(
        row_of("put away") > archive,
        "the folder's child was drawn outside it:\n{screen}"
    );
    // The pinned row is hoisted above the manual order and carries the flag.
    assert!(kept < recent, "the pin did not hoist the row:\n{screen}");
    assert!(
        lines[kept].contains('★'),
        "the pinned row lost its marker: {:?}",
        lines[kept]
    );

    // The tree is drawn as a tree: a heading is the leftmost thing in its band,
    // and everything the arrangement put inside it starts to its right — the
    // folder a step in from the project, and the session that lives in the
    // folder a step in from the folder. Without this a folder's children start
    // left of their own heading, which is what makes the nesting unreadable.
    let left_edge = |index: usize| {
        let line = lines[index];
        vibex_tui::text::display_width(&line[..line.len() - line.trim_start().len()])
    };
    let project = row_of("vibex-card-workspace");
    assert!(
        left_edge(recent) > left_edge(project)
            && left_edge(archive) > left_edge(project)
            && left_edge(row_of("put away")) > left_edge(archive),
        "the arrangement's depth is not drawn:\n{screen}"
    );
}

/// State is a mark and auto-continue is a mark: the list no longer spells the
/// state out, which is what gives the titles their width back, and a session
/// that is about to continue itself says so with the seconds it has left.
#[test]
fn the_session_list_marks_state_and_auto_continue_without_words() {
    let mut running = seeded_session("session_marks0001", "still working");
    running.state = vibex_core::AgentSessionState::Running;
    let mut idle = seeded_session("session_marks0002", "watching this one");
    idle.project_id = running.project_id.clone();
    idle.workspace_id = running.workspace_id.clone();
    idle.workspace_root = running.workspace_root.clone();
    let sessions = vec![running.clone(), idle.clone()];

    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(sessions.clone()))
        .expect("sessions apply");
    // The authority has auto-continue on for the idle session, and its last
    // turn stopped without an answer.
    app.auto_continue.apply_authority(
        &std::collections::BTreeSet::new(),
        &std::collections::BTreeMap::new(),
        &std::collections::BTreeSet::from([idle.id.as_str().to_string()]),
        &std::collections::BTreeSet::new(),
        &sessions,
    );
    app.auto_continue
        .note_status(&idle.id, idle.updated_at_ms, Some(false));
    app.sync_auto_continue();
    let screen = text(&render(&mut app, 110, 24));

    let row = screen
        .lines()
        .find(|line| line.contains("watching this one"))
        .unwrap_or_else(|| panic!("no row for the idle session:\n{screen}"));
    assert!(
        row.contains("↻5"),
        "the countdown is not on the row: {row:?}"
    );
    assert!(
        !row.contains("Idle"),
        "the state word is back on the row: {row:?}"
    );

    let row = screen
        .lines()
        .find(|line| line.contains("still working"))
        .unwrap_or_else(|| panic!("no row for the running session:\n{screen}"));
    assert!(row.contains('▶'), "the running mark is missing: {row:?}");
    assert!(
        !row.contains("Running"),
        "the state word is back on the row: {row:?}"
    );
    assert!(
        !row.contains('↻'),
        "a session without it was marked: {row:?}"
    );
}

/// The page says what the sessions are doing before the reader reads a single
/// row, and keeps the one action a session list needs on the line below it.
#[test]
fn the_session_list_summarises_itself_and_offers_a_new_session() {
    let mut waiting = seeded_session("session_head0001", "blocked on you");
    waiting.state = vibex_core::AgentSessionState::NeedsInput;
    let mut working = seeded_session("session_head0002", "still working");
    working.state = vibex_core::AgentSessionState::Running;
    let mut failed = seeded_session("session_head0003", "the one that failed");
    failed.state = vibex_core::AgentSessionState::Error;
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![waiting, working, failed]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    let header = screen
        .lines()
        .find(|line| line.contains("waiting"))
        .unwrap_or_else(|| panic!("no summary line:\n{screen}"));
    // One chip per state the list holds, counted, and none for a state it does
    // not: the chips are the list's tally, not a legend.
    for chip in ["◆ 1 waiting", "▶ 1 running", "✗ 1 failed"] {
        assert!(header.contains(chip), "missing {chip:?}: {header:?}");
    }
    assert!(
        !header.contains("idle"),
        "a state nothing is in was counted: {header:?}"
    );
    // The line names the page. The location is the status band's, one row up:
    // printing the same path twice in two rows is how a page looks broken.
    assert!(
        header.contains("Session list"),
        "the header does not name the page: {header:?}"
    );
    let actions = screen
        .lines()
        .find(|line| line.contains("+ New session"))
        .unwrap_or_else(|| panic!("no actions line:\n{screen}"));
    assert!(
        actions.contains("[Workspace"),
        "the actions line does not name the key that moves the workspace: {actions:?}"
    );
    // The button is a button: it publishes the rect a click would land on.
    let button = app
        .regions
        .hints
        .iter()
        .find(|(_, intent)| *intent == vibex_tui::action::Intent::NewSession)
        .map(|(rect, _)| *rect)
        .expect("the new-session button is not clickable");
    // A row of air under the action is what keeps the button off the list.
    assert!(button.width > 0 && button.y == app.regions.list.unwrap().rect.y - 2);
}

/// A list longer than the page scrolls by page and by end without moving the
/// cursor: the reader scans it, and the row they left the cursor on is still
/// the row it was on.
#[test]
fn a_long_session_list_pages_without_moving_the_cursor() {
    use vibex_tui::action::Intent;
    let mut first = seeded_session("session_long0001", "long session 1");
    // One workspace, and an age per row: the list is built from one heading and
    // forty rows in an order the test can name, newest first.
    first.workspace_id =
        vibex_core::WorkspaceId::parse("workspace_long01").expect("valid workspace id");
    let sessions = (1..=40)
        .map(|index| {
            let mut session = first.clone();
            session.id = vibex_core::VibexSessionId::parse(format!("session_long{index:04}"))
                .expect("valid session id");
            session.title = format!("long session {index}");
            session.last_message_at_ms = index;
            session.created_at_ms = index;
            session
        })
        .collect::<Vec<_>>();
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(sessions))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    assert!(screen.contains("long session 40"), "{screen}");
    assert!(
        !screen.contains("long session 1"),
        "the whole list is on one page, so nothing here is being tested"
    );

    // A page key moves the window, not the cursor.
    let before = app.session_scroll;
    app.perform(Intent::ScrollPageDown);
    let _ = render(&mut app, 110, 24);
    assert!(
        app.session_scroll > before,
        "the page key did not move the window"
    );
    assert_eq!(
        app.selection_for(Scope::Sessions),
        0,
        "paging moved the cursor"
    );

    // The end of the list is reachable in one gesture, and the top again.
    app.perform(Intent::ScrollToBottom);
    let screen = text(&render(&mut app, 110, 24));
    assert!(screen.contains("long session 1"), "{screen}");
    app.perform(Intent::ScrollToTop);
    let screen = text(&render(&mut app, 110, 24));
    assert!(screen.contains("long session 40"), "{screen}");

    // Moving the cursor hands the window back to it: the row the reader lands
    // on is the row they see.
    app.perform(Intent::ScrollToBottom);
    let _ = render(&mut app, 110, 24);
    for _ in 0..3 {
        app.perform(Intent::SelectNext);
    }
    let screen = text(&render(&mut app, 110, 24));
    assert!(
        screen.contains("long session 37"),
        "the cursor's row is off screen after paging and stepping:\n{screen}"
    );
}

/// A control answers the pointer before the click does: the new-session button
/// carries its own surface while the mouse rests on it, and only then.
#[test]
fn the_new_session_button_lights_up_under_the_pointer() {
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![seeded_session("session_hover0001", "lit")]))
        .expect("sessions apply");
    let resting = render_buffer(&mut app, 110, 24);
    let button = app
        .regions
        .hints
        .iter()
        .find(|(_, intent)| *intent == vibex_tui::action::Intent::NewSession)
        .map(|(rect, _)| *rect)
        .expect("the new-session button is a click target");
    app.hovered_hint = Some(vibex_tui::action::Intent::NewSession);
    let hovered = render_buffer(&mut app, 110, 24);
    let lit = (button.x..button.right())
        .filter(|column| {
            resting.cell((*column, button.y)).map(|cell| cell.style())
                != hovered.cell((*column, button.y)).map(|cell| cell.style())
        })
        .count();
    assert_eq!(
        lit,
        usize::from(button.width),
        "the button does not answer the pointer"
    );
    // The row it sits on is otherwise unchanged: a hover is the control's, not
    // the page's.
    for column in button.right()..110 {
        assert_eq!(
            resting.cell((column, button.y)).map(|cell| cell.style()),
            hovered.cell((column, button.y)).map(|cell| cell.style()),
            "the hover spilled past the button at column {column}"
        );
    }
}

/// The list is the page, not a panel on it: nothing draws a frame around it,
/// and the rows own the columns and rows a border would have taken.
#[test]
fn the_session_list_is_not_drawn_inside_a_frame() {
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![seeded_session("session_frame0001", "no frame")]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    for corner in ['╭', '╮', '╰', '╯'] {
        assert!(
            !screen.contains(corner),
            "the list is wearing a frame again ({corner}):\n{screen}"
        );
    }
    // The page's own chrome is the first thing in the band: the name, a row of
    // air, the action, another row of air, then the list.
    let lines = screen.lines().collect::<Vec<_>>();
    let named = lines
        .iter()
        .position(|line| line.trim_start().starts_with("Session list"))
        .unwrap_or_else(|| panic!("the page does not name itself:\n{screen}"));
    assert!(
        lines[named + 1].trim().is_empty(),
        "there is no air under the page's name:\n{screen}"
    );
    assert!(
        lines[named + 2].contains("+ New session"),
        "the action is not under the page's name:\n{screen}"
    );
    assert!(
        lines[named + 3].trim().is_empty(),
        "there is no air under the action:\n{screen}"
    );
    assert!(
        lines[named + 4].trim_start().starts_with('▾'),
        "the list does not start under the page's own chrome:\n{screen}"
    );
}

/// A heading says what it holds and rules off the section, so a stack of rows
/// reads as groups rather than as one long list.
#[test]
fn a_group_heading_counts_its_sessions_and_rules_the_section() {
    let mut first = seeded_session("session_group0001", "first");
    first.last_message_at_ms = vibex_core::unix_timestamp_ms();
    let mut second = seeded_session("session_group0002", "second");
    second.project_id = first.project_id.clone();
    second.workspace_id = first.workspace_id.clone();
    second.workspace_root = first.workspace_root.clone();
    second.last_message_at_ms = first.last_message_at_ms;

    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![first, second]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    let heading = screen
        .lines()
        .find(|line| line.trim_start_matches(['│', ' ']).starts_with('▾'))
        .unwrap_or_else(|| panic!("no heading:\n{screen}"));
    // The count is the last word on the line, past the rule: the rule carries
    // the eye to it.
    let (before, count) = heading
        .rsplit_once(' ')
        .unwrap_or_else(|| panic!("the heading has one word: {heading:?}"));
    assert_eq!(
        count.trim(),
        "2",
        "the count does not close the line: {heading:?}"
    );
    assert!(
        before.trim_end().ends_with('─'),
        "the count is not past the rule: {heading:?}"
    );
    assert!(
        before.contains("vibex-card-workspace"),
        "the heading does not name the workspace: {heading:?}"
    );
    // A session row is not a heading: it carries the mark and the age instead.
    let row = screen
        .lines()
        .find(|line| line.contains("first"))
        .unwrap_or_else(|| panic!("no session row:\n{screen}"));
    assert!(
        !row.contains('─'),
        "a session row was drawn as a heading: {row:?}"
    );
}

/// Under its title, a row says what the session last did. A session waiting on
/// the reader says that first: it is the one thing on the row to act on.
#[test]
fn a_session_row_carries_what_it_last_did_under_its_title() {
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    let mut session = seeded_session("session_echo0001", "fix the flaky test");
    session.last_message_at_ms = vibex_core::unix_timestamp_ms();
    app.agent
        .apply_sessions(Ok(vec![session.clone()]))
        .expect("sessions apply");
    app.note_session_echo(&vibex_core::TimelineItem {
        id: vibex_core::TimelineItemId::new(),
        session_id: session.id.clone(),
        sequence: 4,
        timestamp_ms: vibex_core::unix_timestamp_ms(),
        source: vibex_core::TimelineSource::Agent,
        kind: vibex_core::TimelineItemKind::AgentMessage,
        correlation_id: None,
        provider_correlation_id: None,
        redaction_state: vibex_core::TimelineRedactionState::None,
        execution_attribution: None,
        payload: vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
            text: "Ran cargo test and it passed".to_string(),
            is_final: true,
        }),
    });
    let screen = text(&render(&mut app, 110, 24));
    let lines = screen.lines().collect::<Vec<_>>();
    let title_at = lines
        .iter()
        .position(|line| line.contains("fix the flaky test"))
        .unwrap_or_else(|| panic!("no title row:\n{screen}"));
    let secondary = lines
        .get(title_at + 1)
        .copied()
        .unwrap_or_else(|| panic!("the row has no second line:\n{screen}"));
    assert!(
        secondary.contains("Ran cargo test and it passed"),
        "the second line does not say what the session did: {secondary:?}"
    );
    // It starts under the title, not under the mark. `.find` counts bytes and
    // the row's chrome is not ASCII, so the offsets are measured in cells.
    let column_of = |line: &str, needle: &str| {
        line.find(needle)
            .map(|at| vibex_tui::text::display_width(&line[..at]))
    };
    assert_eq!(
        column_of(lines[title_at], "fix the flaky test"),
        column_of(secondary, "Ran cargo test"),
        "the second line does not line up under the title:\n{screen}"
    );

    // A session this client has not seen do anything is one line: the
    // workspace is the heading it already sits under, and repeating the
    // directory under every row is noise, not information.
    let mut quiet = seeded_session("session_echo0002", "nothing to report");
    quiet.project_id = session.project_id.clone();
    quiet.workspace_id = session.workspace_id.clone();
    quiet.workspace_root = session.workspace_root.clone();
    app.agent
        .apply_sessions(Ok(vec![quiet.clone()]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    let quiet_at = screen
        .lines()
        .position(|line| line.contains("nothing to report"))
        .unwrap_or_else(|| panic!("no row for the quiet session:\n{screen}"));
    assert!(
        !screen
            .lines()
            .nth(quiet_at + 1)
            .is_some_and(|line| { line.contains("/tmp") || line.contains("vibex-card-workspace") }),
        "the row repeats its workspace under the title:\n{screen}"
    );

    // A session blocked on the reader says so even when it has not been seen
    // saying anything: that is the one thing on the row to act on.
    let mut blocked = seeded_session("session_echo0003", "waiting on you");
    blocked.state = vibex_core::AgentSessionState::NeedsInput;
    blocked.project_id = session.project_id.clone();
    blocked.workspace_id = session.workspace_id.clone();
    blocked.workspace_root = session.workspace_root.clone();
    app.agent
        .apply_sessions(Ok(vec![blocked]))
        .expect("sessions apply");
    let screen = text(&render(&mut app, 110, 24));
    let line = screen
        .lines()
        .find(|line| line.contains("blocked until you answer"))
        .unwrap_or_else(|| panic!("a waiting session does not say so:\n{screen}"));
    assert!(
        !line.contains("vibex-card-workspace"),
        "the waiting row names its workspace instead of what it waits on: {line:?}"
    );
    assert!(
        !screen.contains("Running") && !screen.contains("Idle"),
        "the state words are back on the rows:\n{screen}"
    );
}

/// Rows are one line or two, so a click has to be measured against the row it
/// landed in rather than against the line number it landed on.
#[test]
fn a_click_on_a_session_rows_second_line_selects_that_row() {
    let mut first = seeded_session("session_click0001", "first row");
    first.last_message_at_ms = vibex_core::unix_timestamp_ms();
    let mut second = seeded_session("session_click0002", "second row");
    second.project_id = first.project_id.clone();
    second.workspace_id = first.workspace_id.clone();
    second.workspace_root = first.workspace_root.clone();
    second.last_message_at_ms = first.last_message_at_ms;
    let mut app = app(110, 24);
    app.navigate_to(Page::Sessions);
    app.agent
        .apply_sessions(Ok(vec![first, second]))
        .expect("sessions apply");
    let _ = render(&mut app, 110, 24);
    let region = app
        .regions
        .list
        .clone()
        .expect("the list is a clickable region");
    // Row 0 is the heading, row 1 the first session, row 2 the second: a click
    // on the second line of row 1 has to answer row 1.
    let line_of_row = |row: usize| {
        region
            .heights
            .iter()
            .take(row)
            .map(|height| usize::from(*height))
            .sum::<usize>()
    };
    let second_line = region.rect.y + (line_of_row(1) + 1) as u16;
    assert_eq!(
        vibex_tui::app::list_row_at(&region, region.rect.x + 1, second_line),
        Some(1),
        "a click on a row's second line landed on another row"
    );
    let third_line = region.rect.y + (line_of_row(2)) as u16;
    assert_eq!(
        vibex_tui::app::list_row_at(&region, region.rect.x + 1, third_line),
        Some(2),
    );
}

/// Inside the session, the countdown is on the composer's info line — the same
/// edge the desktop puts its "Continue (Ns)" button on — so a continuation the
/// reader is inside of is never a surprise.
#[test]
fn the_session_view_shows_the_continuation_countdown() {
    let mut app = app(110, 24);
    let mut idle = seeded_session("session_count0001", "watching this one");
    idle.state = vibex_core::AgentSessionState::Idle;
    let sessions = vec![idle.clone()];
    app.agent
        .apply_sessions(Ok(sessions.clone()))
        .expect("sessions apply");
    app.agent.state.active_session.resolve(idle.clone());
    app.agent.state.selected_session_id = Some(idle.id.clone());
    app.navigate_to(Page::Agent);
    app.auto_continue.apply_authority(
        &std::collections::BTreeSet::new(),
        &std::collections::BTreeMap::new(),
        &std::collections::BTreeSet::from([idle.id.as_str().to_string()]),
        &std::collections::BTreeSet::new(),
        &sessions,
    );
    app.auto_continue
        .note_status(&idle.id, idle.updated_at_ms, Some(false));
    app.sync_auto_continue();

    let screen = text(&render(&mut app, 110, 24));
    assert!(
        screen.contains("↻5"),
        "the countdown is not beside the composer:\n{screen}"
    );
}

#[test]
fn reasoning_toggle_opens_and_closes_every_member_of_a_group() {
    use vibex_tui::action::Intent;
    let mut app = app(100, 30);
    app.navigate_to(Page::Agent);
    let row = vibex_tui::transcript::Block {
        id: "thought-0".into(),
        kind: vibex_desktop_model::TimelineRowKind::Reasoning,
        title: "Thinking".into(),
        body: "Detailed reasoning".into(),
        turn_id: Some("turn-1".into()),
        sequence: 1,
        timestamp_ms: None,
        expanded: false,
        collapsible: true,
        streaming: false,
        failed: false,
        pending_permission: false,
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
    };
    app.transcript.set_blocks(
        (0..4)
            .map(|index| {
                let mut row = row.clone();
                row.id = format!("thought-{index}");
                row
            })
            .collect(),
    );
    app.perform(Intent::ToggleReasoningExpanded);
    assert!(app.transcript.blocks().iter().all(|row| row.expanded));
    app.perform(Intent::ToggleReasoningExpanded);
    assert!(app.transcript.blocks().iter().all(|row| !row.expanded));
    assert!(matches!(
        app.transcript.blocks()[0].group,
        vibex_tui::transcript::GroupRole::Head { hidden: 3 }
    ));
}

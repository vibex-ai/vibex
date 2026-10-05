//! Render a frame with representative content and print it.
//!
//! Design work needs eyes on the result, and a `TestBackend` assertion can only
//! check what someone already thought to assert. This example builds a session
//! with one of every block kind, draws it at a chosen size, and prints the
//! buffer as text.
//!
//! ```bash
//! cargo run -p vibex-tui --example preview -- 140 44
//! cargo run -p vibex-tui --example preview -- 100 30 --light
//! cargo run -p vibex-tui --example preview -- 140 44 --no-color
//! cargo run -p vibex-tui --example preview -- 120 34 --settings
//! cargo run -p vibex-tui --example preview -- 100 30 --thinking --phase 4
//! ```
//!
//! `--thinking` adds a thought that is still arriving, so the live window and
//! its rail can be reviewed at a chosen `--phase` of the animation clock.
//!
//! `--ansi` emits real SGR codes instead of plain text, which is the only way
//! to review the rails and background bands from a pipeline.

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Modifier;
use vibex_backend::DisconnectedBackend;
use vibex_desktop_model::TimelineRowKind;
use vibex_tui::app::{App, AppOptions, Overlay, Page};
use vibex_tui::theme::{ColorCapability, ColorMode, GlyphMode};
use vibex_tui::transcript::{Block, ScrollState};
use vibex_tui::{Locale, SeatKind};

fn main() {
    let arguments = std::env::args().skip(1).collect::<Vec<_>>();
    let mut width = 140u16;
    let mut height = 44u16;
    let mut mode = ColorMode::TrueColor;
    let mut light = false;
    let mut ansi = false;
    let mut welcome = false;
    let mut settings = false;
    let mut thinking = false;
    let mut phase = 0u32;
    let mut numbers = Vec::new();
    let mut arguments = arguments.into_iter();
    while let Some(argument) = arguments.next() {
        match argument.as_str() {
            "--light" => light = true,
            "--no-color" => mode = ColorMode::None,
            "--ascii" => mode = ColorMode::Ansi16,
            "--ansi" => ansi = true,
            "--welcome" => welcome = true,
            "--settings" => settings = true,
            // A running thought, to review the live window and its rail.
            "--thinking" => thinking = true,
            "--phase" => {
                phase = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
            }
            other => {
                if let Ok(value) = other.parse::<u16>() {
                    numbers.push(value);
                }
            }
        }
    }
    if let Some(value) = numbers.first() {
        width = *value;
    }
    if let Some(value) = numbers.get(1) {
        height = *value;
    }

    let mut app = App::new(
        DisconnectedBackend::facade(),
        AppOptions {
            seat: SeatKind::Authority,
            capability: ColorCapability {
                mode,
                glyphs: GlyphMode::Unicode,
            },
            theme_id: Some(if light { "vibex-light" } else { "vibex-dark" }.to_string()),
            mode: if light {
                vibex_ui::GpuiThemeMode::Light
            } else {
                vibex_ui::GpuiThemeMode::Dark
            },
            locale: Locale::En,
            // A preview must not leave state behind.
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    app.resize(width, height);
    app.focus = vibex_tui::app::Focus::Composer;
    app.composer
        .set_text("add a retry to the upload path and cover it with a test");
    app.navigate_to(Page::Agent);
    app.live = vibex_tui::app::LiveState::Ready;
    if welcome {
        // The empty session list, which is what the prompt's corner entry
        // opens; the prompt itself is the state the app is built in.
        app.navigate_to(vibex_tui::app::Page::Sessions);
    }
    if settings {
        app.perform(vibex_tui::action::Intent::OpenSettings);
        app.set_selection(vibex_tui::keymap::Scope::Settings, 1);
    }
    let blocks = sample_session();
    if !welcome {
        let mut blocks = blocks;
        if thinking {
            blocks.push(live_thought());
        }
        app.transcript.set_blocks(blocks);
    }
    if thinking {
        // The view hands the transcript the app's own clock, so a frozen frame
        // is a matter of stopping the clock rather than setting it twice.
        app.animation_phase = phase;
    }
    // A live turn, so the rail and the turn line render rather than the idle
    // and single-turn fallbacks.
    app.turn_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(72));
    app.turn_tokens = Some(12_400);

    let backend = TestBackend::new(width, height);
    let mut terminal = Terminal::new(backend).expect("test terminal");
    terminal
        .draw(|frame| vibex_tui::view::render(frame, &mut app))
        .expect("frame draws");
    let buffer = terminal.backend().buffer().clone();
    for row in 0..height {
        let mut line = String::new();
        let mut open: Option<String> = None;
        for column in 0..width {
            let Some(cell) = buffer.cell((column, row)) else {
                continue;
            };
            if ansi {
                let sgr = sgr_for(cell.fg, cell.bg, cell.modifier);
                if open.as_deref() != Some(sgr.as_str()) {
                    // A reset then the new parameters as one sequence: written
                    // as two pieces the parameters would be printed as text.
                    line.push_str("\u{1b}[0m\u{1b}[");
                    line.push_str(&sgr);
                    line.push('m');
                    open = Some(sgr);
                }
            }
            line.push_str(cell.symbol());
        }
        if ansi {
            line.push_str("\u{1b}[0m");
        }
        println!(
            "{}",
            if ansi {
                line
            } else {
                line.trim_end().to_string()
            }
        );
    }
    let _ = ScrollState::default();
    let _ = Overlay::Help {
        query: String::new(),
        selected: 0,
        collapsed: std::collections::BTreeSet::new(),
    };
}

/// A thought that is still arriving, long enough to overflow its window.
fn live_thought() -> Block {
    let mut block = entry(
        "live-thought",
        TimelineRowKind::Reasoning,
        "Thinking",
        "I should check how the timeline renders a long thought before I change it.\n\
         The block cache measures each block once, so a body that grows without a\n\
         bound would push everything above it off the screen while the reader is\n\
         still reading. The fix is a fixed window on the tail, with the newest rows\n\
         arriving at the bottom and the oldest leaving the top.\n\
         \n\
         The window needs a left rail so the reader can see how tall it is, and the\n\
         rail should move while the thought is still arriving.",
        false,
    );
    block.streaming = true;
    block
}

/// The SGR sequence for one cell's style.
fn sgr_for(fg: ratatui::style::Color, bg: ratatui::style::Color, modifier: Modifier) -> String {
    let channel = |color: ratatui::style::Color, base: u8| -> String {
        match color {
            ratatui::style::Color::Rgb(r, g, b) => format!("{};2;{};{};{}", base, r, g, b),
            ratatui::style::Color::Indexed(index) => format!("{};5;{}", base, index),
            ratatui::style::Color::Reset => format!("{}", base - 10 + 9),
            other => {
                let code = match other {
                    ratatui::style::Color::Black => 30,
                    ratatui::style::Color::Red => 31,
                    ratatui::style::Color::Green => 32,
                    ratatui::style::Color::Yellow => 33,
                    ratatui::style::Color::Blue => 34,
                    ratatui::style::Color::Magenta => 35,
                    ratatui::style::Color::Cyan => 36,
                    ratatui::style::Color::Gray => 37,
                    ratatui::style::Color::DarkGray => 90,
                    ratatui::style::Color::LightRed => 91,
                    ratatui::style::Color::LightGreen => 92,
                    ratatui::style::Color::LightYellow => 93,
                    ratatui::style::Color::LightBlue => 94,
                    ratatui::style::Color::LightMagenta => 95,
                    ratatui::style::Color::LightCyan => 96,
                    ratatui::style::Color::White => 97,
                    _ => 39,
                };
                format!("{}", if base == 38 { code } else { code + 10 })
            }
        }
    };
    let mut parts = vec![channel(fg, 38), channel(bg, 48)];
    if modifier.contains(Modifier::BOLD) {
        parts.push("1".to_string());
    }
    if modifier.contains(Modifier::DIM) {
        parts.push("2".to_string());
    }
    if modifier.contains(Modifier::ITALIC) {
        parts.push("3".to_string());
    }
    if modifier.contains(Modifier::UNDERLINED) {
        parts.push("4".to_string());
    }
    if modifier.contains(Modifier::CROSSED_OUT) {
        parts.push("9".to_string());
    }
    if modifier.contains(Modifier::REVERSED) {
        parts.push("7".to_string());
    }
    parts.join(";")
}

/// One of every block kind, in the order a real session produces them.
fn sample_session() -> Vec<Block> {
    vec![
        entry(
            "u1",
            TimelineRowKind::UserMessage,
            "Add a retry to the upload path",
            "The upload helper gives up on the first 5xx. It should retry with a\nshort backoff and give up after three attempts.",
            false,
        ),
        entry(
            "r1",
            TimelineRowKind::Reasoning,
            "Reading the upload path",
            "The helper is in `src/net/upload.rs`. It builds one request and\nreturns the first response, so a 503 fails the whole turn.",
            false,
        ),
        entry(
            "t1",
            TimelineRowKind::ToolCall,
            "read src/net/upload.rs",
            "fn upload(path: &Path) -> Result<Response> {\n    let request = build(path)?;\n    client.send(request).await\n}",
            false,
        ),
        entry(
            "t2",
            TimelineRowKind::ToolCall,
            "grep \"retry\" src/net/",
            "src/net/upload.rs:41:    // TODO: retry\nsrc/net/mod.rs:12:pub use upload::upload;",
            false,
        ),
        entry(
            "t3",
            TimelineRowKind::ToolCall,
            "read src/net/mod.rs",
            "pub mod upload;\npub use upload::upload;",
            false,
        ),
        entry(
            "t4",
            TimelineRowKind::ToolCall,
            "read Cargo.toml",
            "[dependencies]\ntokio = { version = \"1\" }",
            false,
        ),
        entry(
            "t5",
            TimelineRowKind::ToolCall,
            "grep \"backoff\" --glob '*.rs'",
            "crates/net/src/retry.rs:8:pub fn backoff(attempt: u32) -> Duration {",
            false,
        ),
        entry(
            "c1",
            TimelineRowKind::Command,
            "cargo test -p net",
            "running 12 tests\ntest upload::rejects_empty ... ok\ntest upload::retries_on_503 ... ok\n\ntest result: ok. 12 passed",
            false,
        ),
        entry(
            "f1",
            TimelineRowKind::FileOperation,
            "edit src/net/upload.rs",
            "@@ -38,6 +38,14 @@ pub async fn upload(path: &Path) -> Result<Response> {\n-    client.send(request).await\n+    for attempt in 0..3 {\n+        match client.send(request.clone()).await {\n+            Ok(response) if response.status().is_server_error() => continue,\n+            result => return result,\n+        }\n+    }",
            false,
        ),
        entry(
            "a1",
            TimelineRowKind::AgentMessage,
            "Done",
            "`upload` now retries up to three times on a 5xx. The backoff helper\nin `src/net/retry.rs` already existed, so no new dependency was needed.\n\n- retries: 3, 100ms/200ms/400ms\n- covered by `upload::retries_on_503`",
            false,
        ),
        entry(
            "p1",
            TimelineRowKind::PermissionRequest,
            "run cargo publish --dry-run",
            "Publishing uploads the crate to the registry.",
            false,
        ),
        entry(
            "e1",
            TimelineRowKind::Error,
            "failed to reach the registry",
            "connection reset by peer after 30s",
            true,
        ),
        entry(
            "s1",
            TimelineRowKind::SystemNotice,
            "context compacted",
            "",
            false,
        ),
    ]
}

/// Spread the sample across three turns so the rail has somewhere to navigate.
fn turn_of(id: &str) -> &'static str {
    match id.as_bytes().first().copied() {
        Some(b'u') | Some(b'r') | Some(b't') | Some(b'c') | Some(b'f') => "turn-1",
        Some(b'a') | Some(b'p') => "turn-2",
        _ => "turn-3",
    }
}

fn entry(id: &str, kind: TimelineRowKind, title: &str, body: &str, failed: bool) -> Block {
    Block {
        id: id.to_string(),
        kind,
        title: title.to_string(),
        body: body.to_string(),
        turn_id: Some(turn_of(id).to_string()),
        sequence: 1,
        expanded: false,
        collapsible: matches!(
            kind,
            TimelineRowKind::Reasoning
                | TimelineRowKind::ToolCall
                | TimelineRowKind::Command
                | TimelineRowKind::FileOperation
        ),
        streaming: false,
        failed,
        pending_permission: matches!(kind, TimelineRowKind::PermissionRequest),
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
    }
}

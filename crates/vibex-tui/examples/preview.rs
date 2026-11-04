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
//! cargo run -p vibex-tui --example preview -- 100 30 --new
//! cargo run -p vibex-tui --example preview -- 100 30 --new --glitch --phase 2
//! cargo run -p vibex-tui --example preview -- 120 40 --paste
//! cargo run -p vibex-tui --example preview -- 120 40 --image --ansi
//! ```
//!
//! `--thinking` adds a thought that is still arriving, so the live window and
//! its rail can be reviewed at a chosen `--phase` of the animation clock.
//!
//! `--paste` and `--image` leave a collapsed chip under the caret, which is
//! what the panel above the composer explains. `--image` draws a picture this
//! example generates, so the raster has real pixels in it; `--ansi` is the only
//! way to see them from a pipeline. The raster is what this example can show —
//! a terminal that speaks an image protocol is handed the picture itself, and
//! that only happens in the interface the run loop is driving.
//!
//! `--ansi` emits real SGR codes instead of plain text, which is the only way
//! to review the rails and background bands from a pipeline.
//!
//! `--showcase` fills the frame the way a reader's own client is filled:
//! attached to a running desktop rather than owning the runtime, with the
//! desktop's session list behind it and the Agent, account, model and run
//! options the composer's line names. Without it the frame is a client with
//! nothing behind it — the disconnected shape, worth drawing, but not the one
//! a reader recognises. `--settled` finishes the sample turn, so its work folds
//! behind the divider the answer carries.
//!
//! `--cursor` prints the cell the client asked the terminal for after the
//! frame, as `#cursor <column> <row>` or `#cursor hidden`. A terminal draws
//! that cursor itself; a page carrying the frame over has to place it, and the
//! client is the one that knows where it goes.
//!
//! `--locale zh-CN` draws the interface in one of the shipped languages, so a
//! translation can be reviewed in the grid that has to hold it: Chinese copy is
//! twice as wide per character, and a label that no longer fits is a layout
//! bug, not a translation one. The tags are the client's own (`en`, `zh-CN`,
//! `zh-TW`), and the sample conversation stays in English — it is the reader's
//! data, not the client's copy.

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
    let mut new_session = false;
    let mut glitch = false;
    let mut thinking = false;
    let mut paste = false;
    let mut image = false;
    let mut phase = 0u32;
    let mut locale = Locale::En;
    let mut showcase = false;
    let mut settled = false;
    let mut cursor = false;
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
            // The page a new session opens on, whose mark is the whole point of
            // the two `--glitch`/`--phase` flags beside it.
            "--new" => new_session = true,
            "--glitch" => glitch = true,
            // A running thought, to review the live window and its rail.
            "--thinking" => thinking = true,
            // A collapsed chip under the caret, which is what the preview panel
            // above the composer explains.
            "--paste" => paste = true,
            "--image" => image = true,
            // The state a reader's own client is in, rather than the empty one
            // a preview can build on its own.
            "--showcase" => showcase = true,
            // A finished turn: the work folds behind the answer's divider.
            "--settled" => settled = true,
            // Print the cell the client asked the terminal to put its cursor
            // on, which is the one thing in a frame that is not a cell.
            "--cursor" => cursor = true,
            "--phase" => {
                phase = arguments
                    .next()
                    .and_then(|value| value.parse().ok())
                    .unwrap_or(0);
            }
            // The language the product copy is drawn in, resolved the way the
            // client resolves a system locale: a Chinese tag picks its script,
            // anything else reads as English.
            "--locale" => {
                locale = Locale::from_system_tag(arguments.next().as_deref());
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
            locale,
            // A preview must not leave state behind.
            sidebar_path: None,
            runtime_path: None,
            preferences_path: None,
            ..Default::default()
        },
    );
    // A preview is a text pipeline: there is no terminal here to hand a
    // picture to, so every mark is drawn the way the client draws it on a
    // terminal that cannot hold one — as the name it stands for.
    app.terminal_images = Default::default();
    app.resize(width, height);
    app.focus = vibex_tui::app::Focus::Composer;
    // The draft's vocabulary is what an empty composer names, and an empty one
    // is what a reader looks at most of the time. The flags below that fill it
    // — a paste, a picture — still do.
    if !showcase {
        app.composer
            .set_text("add a retry to the upload path and cover it with a test");
    }
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
    if new_session {
        app.perform(vibex_tui::action::Intent::NewSession);
    }
    if glitch {
        app.settings.mark = vibex_tui::logo::MarkStyle::Glitch;
    }
    // What the reader's own client holds, in the order the authority would have
    // answered: the seat it attached through, the runtime catalogue, the
    // session list, and the session the page is about.
    if showcase {
        app.seat = SeatKind::Remote;
        // The example's own state is empty, which the pages draw as low as they
        // keep the room for — but a session the reader is in has everything
        // below it, so a fixture that never scrolled draws over an empty band.
        app.scroll.follow = true;
        let selection = showcase_selection();
        app.runtime_options = Some(showcase_catalog(&selection));
        app.new_session_runtime = Some(selection.clone());
        let sessions = showcase_sessions(locale);
        let open = sessions.first().map(|session| session.id.clone());
        let reconciled = app.reconcile_session_list(Ok(sessions));
        app.agent
            .apply_sessions(reconciled)
            .expect("the fixture applies");
        // After the list, which carries no runtime of its own: a read that
        // lands over this one would have to name the same selection to clear it.
        app.agent.state.runtime_selection.value = Some(showcase_selection_state(&selection));
        if let Some(session_id) = open {
            // Only the page that shows a session opens one: the list is a page
            // over all of them, the composing page is writing a new one, and
            // the settings sheet belongs to the session already behind it.
            if !welcome && !new_session && !settings {
                app.open_session(session_id);
            }
        }
    }
    if paste {
        // The caret lands at the label's right edge, where a paste leaves it
        // and where the panel says the same block pasted again opens it.
        // Focus is set here rather than at the top because moving to a page
        // hands the keyboard back to the transcript.
        app.focus = vibex_tui::app::Focus::Composer;
        app.composer.set_text("");
        let log = (0..40)
            .map(|index| format!("2025-09-30 12:04:{index:02} INFO  upload: retry {index}"))
            .collect::<Vec<_>>()
            .join("\n");
        app.composer.insert_paste(&log);
    }
    if image {
        app.focus = vibex_tui::app::Focus::Composer;
        app.composer.set_text("");
        let _ = app.attach_image_bytes("image/png", sample_png());
        app.composer.move_left();
    }
    let mut blocks = sample_session();
    if settled {
        blocks = settled_session(blocks);
    }
    if !welcome {
        let mut blocks = blocks;
        if thinking {
            blocks.push(live_thought());
        }
        app.transcript.set_blocks(blocks);
    }
    if thinking || glitch {
        // The view hands the transcript the app's own clock, so a frozen frame
        // is a matter of stopping the clock rather than setting it twice. The
        // mark is read at a chosen frame for the same reason: a burst is only
        // reviewable if it can be held still.
        app.animation_phase = phase;
    }
    if showcase {
        // A preview is one frame, and the frame worth carrying over is the one
        // the reader ends on: the lines that changed for this fixture arrive and
        // the clock is then held past the transition, so nothing is drawn as the
        // noise it wears on the way in.
        app.observe_composer_text();
        app.animation_phase = app.animation_phase.wrapping_add(600);
    }
    // A live turn, so the rail and the turn line render rather than the idle
    // and single-turn fallbacks.
    // A settled turn is not running: the work is behind the divider and the
    // turn line says what the session is, not what it is doing.
    if !settled {
        app.turn_started = Some(std::time::Instant::now() - std::time::Duration::from_secs(72));
    }

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
    if cursor {
        // What the frame asked the terminal for, printed after the grid: a
        // terminal puts its cursor there itself, and a page carrying the frame
        // over has nowhere else to learn where it goes.
        let position = terminal.backend().cursor_position();
        if terminal.backend().cursor_visible() {
            println!("#cursor {} {}", position.x, position.y);
        } else {
            println!("#cursor hidden");
        }
    }
    let _ = Overlay::Help {
        query: String::new(),
        selected: 0,
        collapsed: std::collections::BTreeSet::new(),
    };
}

/// A picture the image preview has something to draw for.
///
/// A screenshot is the case the panel exists for, and a generated one keeps
/// this example runnable without a fixture file: a gradient with a darker
/// block in it, so the raster's shape and its colours are both visible.
fn sample_png() -> Vec<u8> {
    let (width, height) = (480u32, 270u32);
    let mut picture = image::RgbImage::new(width, height);
    for (x, y, pixel) in picture.enumerate_pixels_mut() {
        let horizon = height / 3;
        *pixel = if y < horizon {
            image::Rgb([
                (40 + x * 120 / width) as u8,
                90,
                (180 - x * 60 / width) as u8,
            ])
        } else {
            image::Rgb([(24 + x % 60) as u8, (28 + y % 40) as u8, 48])
        };
    }
    let mut bytes = Vec::new();
    picture
        .write_to(
            &mut std::io::Cursor::new(&mut bytes),
            image::ImageFormat::Png,
        )
        .expect("the sample encodes");
    bytes
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

/// The runtime a reader's own client would be naming under the composer.
///
/// One Agent, on one account, with a model and the two run options the info
/// line carries: the depth the turn thinks at and what the session is allowed
/// to do. The values are the fixture's own, so a frame never quotes a real
/// account or a model a reader has configured.
fn showcase_selection() -> vibex_core::SessionRuntimeSelection {
    let mut selection = vibex_core::SessionRuntimeSelection::provider(
        vibex_core::AgentId::parse("codex").expect("agent id"),
        vibex_core::ProviderProfileId::new(),
        "gpt-6-astra",
    );
    selection.reasoning_effort = Some("max".to_string());
    selection.mode_id = Some("full-access".to_string());
    selection
}

fn showcase_catalog(
    selection: &vibex_core::SessionRuntimeSelection,
) -> vibex_core::SessionRuntimeOptionCatalog {
    let value = |value: &str, label: &str| vibex_core::SessionConfigValue {
        value: value.to_string(),
        label: Some(label.to_string()),
    };
    let option = vibex_core::SessionRuntimeOption {
        // The same selection the session runs on: an entry built from a second
        // call would carry a fresh account id and never match the page.
        selection: selection.clone(),
        agent_label: "Codex".to_string(),
        auth_source_label: "Agent account".to_string(),
        model_label: "gpt-6-astra".to_string(),
        reasoning_efforts: vec![value("max", "Max"), value("low", "Low")],
        modes: vec![
            value("full-access", "Full access"),
            value("ask", "Ask before acting"),
        ],
        features: Vec::new(),
        availability: vibex_core::RuntimeOptionAvailability::Available,
        member_count: 1,
        auto_routes: false,
    };
    vibex_core::SessionRuntimeOptionCatalog {
        revision: 1,
        agents: Vec::new(),
        auth_sources: Vec::new(),
        options: vec![option],
    }
}

fn showcase_selection_state(
    selection: &vibex_core::SessionRuntimeSelection,
) -> vibex_core::AgentSessionRuntimeSelectionState {
    vibex_core::AgentSessionRuntimeSelectionState {
        desired: selection.clone(),
        effective: selection.clone(),
        status: vibex_core::SessionRuntimeSelectionStatus::Ready,
        session_revision: 1,
        selection_revision: 1,
        current_binding_id: None,
        activation_generation: 0,
        pending_switch_id: None,
        actionable_error: None,
    }
}

/// The list the desktop would have published, which is what the sessions page
/// is a window over: two workspaces, a session running in one of them, one
/// waiting on the reader, and the rest settled at ages the sidebar's own
/// formatter reads back as "now", "01:05" and "Yesterday".
///
/// Titles are the fixture's, and follow the frame's language: they stand in for
/// what a reader would have written, and a Chinese frame whose list was still
/// English would be showing a different session from the one the interface
/// around it is speaking to.
fn showcase_sessions(locale: Locale) -> Vec<vibex_core::AgentSession> {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|elapsed| elapsed.as_millis() as i64)
        .unwrap_or(0);
    let title = |en: &'static str, zh_cn: &'static str, zh_tw: &'static str| {
        locale.text(en, zh_cn, zh_tw).to_string()
    };
    let repo = "/home/peatboy/code/peatboy/vibex-dev/vibex";
    let site = "/home/peatboy/code/peatboy/vibex-dev/vibex-site";
    // One workspace and project per group: the arrangement the client falls back
    // to is the tree the desktop publishes, and a fresh id per row would draw a
    // heading per row. The ids are the fixture's own rather than random ones,
    // because a list whose groups swap places between two runs cannot be
    // compared with the frame before it.
    let workspaces = [
        (
            vibex_core::WorkspaceId::parse("workspace_0001").expect("workspace id"),
            vibex_core::ProjectId::parse("project_0001").expect("project id"),
        ),
        (
            vibex_core::WorkspaceId::parse("workspace_0002").expect("workspace id"),
            vibex_core::ProjectId::parse("project_0002").expect("project id"),
        ),
    ];
    let mut rows = Vec::new();
    for (index, (name, state, age_ms, root, group)) in [
        (
            title(
                "Refactor agent session timeline",
                "重构会话时间线",
                "重構工作階段時間軸",
            ),
            vibex_core::AgentSessionState::Running,
            65_000,
            repo,
            0,
        ),
        (
            title(
                "Fix flaky worktree merge test",
                "修复工作树合并测试不稳定",
                "修復工作樹合併測試不穩定",
            ),
            vibex_core::AgentSessionState::NeedsInput,
            9 * 60_000,
            repo,
            0,
        ),
        (
            title(
                "Polish the composer's empty state",
                "打磨输入框空状态",
                "打磨輸入框空狀態",
            ),
            vibex_core::AgentSessionState::Idle,
            26 * 60 * 60_000,
            repo,
            0,
        ),
        (
            title("Draft the release notes", "起草发布说明", "起草發佈說明"),
            vibex_core::AgentSessionState::Idle,
            27 * 60 * 60_000,
            repo,
            0,
        ),
        (
            title(
                "Align the mobile showcase",
                "对齐移动端产品展示页",
                "對齊行動端產品展示頁",
            ),
            vibex_core::AgentSessionState::Idle,
            3 * 60_000,
            site,
            1,
        ),
        (
            title(
                "List the built-in Agent marks",
                "列出内置 Agent 标记并提交",
                "列出內建 Agent 標記並提交",
            ),
            vibex_core::AgentSessionState::Idle,
            31 * 60_000,
            site,
            1,
        ),
    ]
    .into_iter()
    .enumerate()
    {
        let (workspace_id, project_id) = workspaces[group].clone();
        rows.push(vibex_core::AgentSession {
            id: vibex_core::VibexSessionId::parse(format!("session_{:04}", index + 1))
                .expect("session id"),
            title: name,
            project_id,
            workspace_id,
            workspace_root: root.to_string(),
            workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
            agent_id: vibex_core::AgentId::parse("codex").expect("agent id"),
            state,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now - age_ms - 60_000,
            updated_at_ms: now - age_ms,
            last_message_at_ms: now - age_ms,
            archived_at_ms: None,
            deleted_at_ms: None,
            tools: Default::default(),
            transport: vibex_core::SessionTransport::Acp,
            terminal: None,
        });
    }
    rows
}

/// The sample turn, finished: its work folded behind the divider the answer
/// carries, the way a session that has been read for a while mostly looks.
fn settled_session(mut blocks: Vec<Block>) -> Vec<Block> {
    const TURN: &str = "turn-1";
    let mut head_taken = false;
    for block in blocks.iter_mut() {
        let work = matches!(
            block.kind,
            TimelineRowKind::Reasoning
                | TimelineRowKind::ToolCall
                | TimelineRowKind::Command
                | TimelineRowKind::FileOperation
        );
        let answer = matches!(block.kind, TimelineRowKind::AgentMessage);
        if !work && !answer {
            continue;
        }
        block.turn_id = Some(TURN.to_string());
        block.fold = Some(vibex_tui::transcript::TurnFold {
            turn_id: TURN.to_string(),
            label: "1m 45s".to_string(),
            expanded: false,
            is_conclusion: answer,
            // The first process row opens the section; the answer owns the
            // divider while the section is shut.
            is_head: work && !head_taken,
            // The mark the divider leads with, which is the Agent the turn ran
            // on rather than the one the page would name now.
            agent_identity: Some("codex".to_string()),
        });
        if work {
            head_taken = true;
        }
    }
    blocks
}

/// Spread the sample across three turns so the rail has somewhere to navigate.
fn turn_of(id: &str) -> &'static str {
    match id.as_bytes().first().copied() {
        Some(b'u') | Some(b'r') | Some(b't') | Some(b'c') | Some(b'f') => "turn-1",
        Some(b'a') | Some(b'p') => "turn-2",
        _ => "turn-3",
    }
}

/// The clock a sample row is stamped with, if it is a row that carries one.
///
/// A message is what the transcript stamps, so those are the rows a preview has
/// to stamp too. The times are fixed rather than read from the clock: a design
/// frame whose labels moved every run could not be compared with the one before
/// it.
fn sample_timestamp(kind: TimelineRowKind) -> Option<i64> {
    // 2025-09-30 13:12 UTC, the hour the sample conversation was written.
    const SPOKEN_AT_MS: i64 = 1_759_237_920_000;
    match kind {
        TimelineRowKind::UserMessage => Some(SPOKEN_AT_MS),
        TimelineRowKind::AgentMessage => Some(SPOKEN_AT_MS + 240_000),
        _ => None,
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
        timestamp_ms: sample_timestamp(kind),
        expanded: false,
        reasoning_window: false,
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
        activity: None,
        details: Vec::new(),
        group_summary: None,
        file_path: None,
        runtime_attribution: None,
        conclusion: false,
        group: vibex_tui::transcript::GroupRole::Solo,
        fold: None,
    }
}

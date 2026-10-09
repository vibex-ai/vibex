//! PTY end-to-end tests: the real binary, a real pseudo-terminal, and
//! assertions on what a user would actually see.
//!
//! `TestBackend` renders into a buffer. This layer is the only one that can
//! prove the things that go wrong *outside* the renderer:
//!
//! * the process really enters raw mode and paints a first frame;
//! * an idle interface writes **nothing** — the measured `idle_cost` contract;
//! * diagnostics written by the process itself never reach the frame;
//! * quitting restores the terminal (no raw mode left behind, cursor visible,
//!   alternate screen left);
//! * a resize storm does not lose the frame.
//!
//! The screen is reconstructed by feeding the captured bytes to
//! `vibex_terminal_ui::TerminalEmulator`, which is the same alacritty-backed
//! grid the terminal pane uses, so the assertion is on the rendered result
//! rather than on a byte pattern.

#![cfg(feature = "pty-harness")]

use std::io::Read;
use std::sync::mpsc::{Receiver, RecvTimeoutError, channel};
use std::time::{Duration, Instant};

use portable_pty::{CommandBuilder, MasterPty, PtySize, native_pty_system};
use vibex_terminal_ui::TerminalEmulator;

/// How long a scenario waits for the screen to settle.
const SETTLE_TIMEOUT: Duration = Duration::from_secs(10);
/// How long an idle scenario watches for stray output.
const IDLE_WINDOW: Duration = Duration::from_millis(1_500);

/// A running client under a pseudo-terminal.
struct Session {
    child: Box<dyn portable_pty::Child + Send + Sync>,
    master: Box<dyn MasterPty + Send>,
    writer: Box<dyn std::io::Write + Send>,
    output: Receiver<Vec<u8>>,
    /// Every byte the process has written, in order.
    captured: Vec<u8>,
    emulator: TerminalEmulator,
    /// Owns the harness's interface files and captured diagnostics.
    spill: tempfile::TempDir,
}

impl Session {
    fn start(columns: u16, rows: u16) -> Self {
        Self::start_with(columns, rows, &[])
    }

    /// Start with extra environment variables, which is how a scenario makes
    /// the client behave like a process that hosts a runtime.
    fn start_with(columns: u16, rows: u16, environment: &[(&str, &str)]) -> Self {
        Self::start_at(None, columns, rows, environment)
    }

    /// Start with the client's working directory set, so a scenario that reads
    /// the filesystem browses a tree the test owns instead of the runner's.
    fn start_at(
        directory: Option<&std::path::Path>,
        columns: u16,
        rows: u16,
        environment: &[(&str, &str)],
    ) -> Self {
        let pty = native_pty_system();
        let pair = pty
            .openpty(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("a pseudo-terminal is available");
        let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_vibex-tui-harness"));
        if let Some(directory) = directory {
            command.cwd(directory);
        }
        command.env("TERM", "xterm-256color");
        command.env("COLORTERM", "truecolor");
        // Pin the locale: the assertions are on English copy, and inheriting a
        // developer's zh_CN environment would make them fail for the wrong
        // reason. Locale detection itself is covered by the unit tests.
        command.env("LC_ALL", "C.UTF-8");
        command.env("LANG", "C.UTF-8");
        command.env_remove("VIBEX_TUI_COLOR");
        command.env_remove("VIBEX_TUI_ICONS");
        command.env_remove("NO_COLOR");
        // The client spills in-process diagnostics into a file; keep it in a
        // directory this scenario owns instead of the shared temporary
        // directory.
        let spill = tempfile::tempdir().expect("a temporary directory for the spill file");
        command.env("VIBEX_TUI_LOG", spill.path().join("vibex-tui.log"));
        // Remembered settings outrank the process locale. Keep every client
        // file local to this scenario so a developer's language, motion,
        // workspace, or key overrides cannot change what the harness draws.
        command.env("VIBEX_TUI_HOME", spill.path());
        command.env(
            "VIBEX_TUI_INTERFACE",
            spill.path().join("tui-interface.json"),
        );
        command.env("VIBEX_TUI_RUNTIME", spill.path().join("tui-runtime.json"));
        command.env("VIBEX_TUI_KEYS", spill.path().join("tui-keys.toml"));
        for (key, value) in environment {
            command.env(key, value);
        }
        let child = pair
            .slave
            .spawn_command(command)
            .expect("the harness binary starts");
        drop(pair.slave);

        let mut reader = pair.master.try_clone_reader().expect("a pty reader");
        let writer = pair.master.take_writer().expect("a pty writer");
        let master = pair.master;
        let (sender, output) = channel();
        std::thread::spawn(move || {
            let mut buffer = [0u8; 4096];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => break,
                    Ok(read) => {
                        if sender.send(buffer[..read].to_vec()).is_err() {
                            break;
                        }
                    }
                }
            }
        });

        Self {
            child,
            master,
            writer,
            output,
            captured: Vec::new(),
            emulator: TerminalEmulator::new(rows, columns),
            spill,
        }
    }

    /// The file the client diverts in-process diagnostics into.
    fn spill_path(&self) -> std::path::PathBuf {
        self.spill.path().join("vibex-tui.log")
    }

    /// The spill file's contents, or an empty string when nothing was diverted.
    fn spilled(&self) -> String {
        std::fs::read_to_string(self.spill_path()).unwrap_or_default()
    }

    /// Drain whatever arrived, updating the emulated screen.
    /// Drain until the process has been silent for `quiet` milliseconds.
    ///
    /// A startup toast legitimately repaints while it is visible, so an idle
    /// measurement has to wait for that to finish rather than pretending the
    /// first quiet millisecond is steady state.
    fn settle(&mut self, quiet: Duration, limit: Duration) {
        let deadline = Instant::now() + limit;
        loop {
            let received = self.pump(quiet);
            if !received || Instant::now() >= deadline {
                return;
            }
        }
    }

    fn pump(&mut self, timeout: Duration) -> bool {
        // Wait the *whole* window, draining whatever arrives inside it, so a
        // caller can ask "was the process silent for this long?".
        let mut received = false;
        let deadline = Instant::now() + timeout;
        loop {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                break;
            }
            match self.output.recv_timeout(remaining) {
                Ok(bytes) => {
                    received = true;
                    self.captured.extend_from_slice(&bytes);
                    self.emulator.advance(&bytes);
                }
                Err(RecvTimeoutError::Timeout) => break,
                Err(RecvTimeoutError::Disconnected) => break,
            }
        }
        received
    }

    /// Wait until `predicate` holds for the rendered screen.
    fn wait_for(&mut self, predicate: impl Fn(&str) -> bool) -> String {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        loop {
            let screen = self.screen();
            if predicate(&screen) {
                return screen;
            }
            if Instant::now() >= deadline {
                panic!(
                    "the screen never matched.\n--- screen ---\n{screen}\n--- raw ---\n{}",
                    self.raw_tail()
                );
            }
            self.pump(Duration::from_millis(100));
        }
    }

    /// Wait for the first frame the interface paints.
    ///
    /// The prompt is what the composing page — the page a fresh client opens on
    /// — keeps at every size; the product name lives in the empty-state panels,
    /// which that page does not draw.
    fn wait_for_first_frame(&mut self) -> String {
        self.wait_for(|screen| screen.contains('❯'))
    }

    fn screen(&mut self) -> String {
        let snapshot = self.emulator.frame();
        let mut output = String::new();
        for row in 0..snapshot.rows {
            for column in 0..snapshot.columns {
                let cell = snapshot
                    .cells
                    .iter()
                    .find(|cell| cell.row == row && cell.column == column);
                match cell {
                    Some(cell) if !cell.wide_spacer => output.push_str(&cell.text),
                    Some(_) => {}
                    None => output.push(' '),
                }
            }
            output.push('\n');
        }
        output
    }

    fn raw_tail(&self) -> String {
        String::from_utf8_lossy(&self.captured[self.captured.len().saturating_sub(400)..])
            .escape_debug()
            .to_string()
    }

    fn send(&mut self, bytes: &[u8]) {
        self.writer.write_all(bytes).expect("the pty accepts input");
        self.writer.flush().expect("the pty flushes");
    }

    fn resize(&mut self, columns: u16, rows: u16) {
        // Both sides move: the pty delivers SIGWINCH to the client, and the
        // emulator follows so the assertion sees what the terminal would show.
        self.master
            .resize(PtySize {
                rows,
                cols: columns,
                pixel_width: 0,
                pixel_height: 0,
            })
            .expect("the pty resizes");
        self.emulator.resize(rows, columns);
    }

    fn bytes_since(&self, mark: usize) -> usize {
        self.captured.len().saturating_sub(mark)
    }

    /// Wait until the client has written `needle` at or after `from`.
    ///
    /// The clipboard conversation is only visible in the bytes: what the client
    /// asks the terminal for is the whole point of it, and the emulated screen
    /// cannot show a query the terminal is meant to answer.
    fn wait_for_output(&mut self, needle: &[u8], from: usize) -> bool {
        let deadline = Instant::now() + SETTLE_TIMEOUT;
        loop {
            let tail = &self.captured[from.min(self.captured.len())..];
            if tail.windows(needle.len()).any(|window| window == needle) {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            self.pump(Duration::from_millis(50));
        }
    }
}

impl Drop for Session {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

#[test]
fn startup_paints_a_first_frame() {
    let mut session = Session::start(120, 40);
    let screen = session.wait_for_first_frame();
    // The status band, the prompt and the corner entry are three different
    // bands; seeing all three proves the stack was assembled rather than
    // half-painted. The band names the seat the frame is attached to — a
    // healthy connection is the quiet case now, so there is no "Done" badge to
    // look for.
    assert!(
        screen.contains("Remote mode") || screen.contains("Local mode"),
        "the status band is missing:\n{screen}"
    );
    assert!(
        screen.contains("Agent setup") && screen.contains("Workspace"),
        "the prompt does not name what the message goes through:\n{screen}"
    );
    assert!(
        screen.contains("Sessions"),
        "the corner entry to the session list is missing:\n{screen}"
    );
}

#[test]
fn quitting_restores_the_terminal() {
    let mut session = Session::start(100, 30);
    session.wait_for_first_frame();
    session.send(b"\x03"); // Ctrl+C: nothing to cancel, so the band asks
    session.pump(Duration::from_millis(300));
    session.send(b"\x03"); // the second press leaves
    session.pump(Duration::from_millis(500));

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if let Ok(Some(_)) = session.child.try_wait() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the client did not exit after the second Ctrl+C");
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    // Leaving the alternate screen and showing the cursor are the two things a
    // failed restore would leave behind.
    let raw = String::from_utf8_lossy(&session.captured).to_string();
    assert!(
        raw.contains("\u{1b}[?1049l"),
        "the alternate screen was never left:\n{}",
        session.raw_tail()
    );
    assert!(
        raw.contains("\u{1b}[?25h"),
        "the cursor was never shown again:\n{}",
        session.raw_tail()
    );
}

#[test]
fn an_idle_interface_writes_nothing() {
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    // The landing mark's greeting is real content and repaints while it runs,
    // as a startup notice would; the contract under test is that nothing
    // repaints once the greeting is over, so this waits it out rather than
    // pretending the first quiet millisecond is steady state.
    session.settle(Duration::from_millis(800), Duration::from_secs(30));

    let mark = session.captured.len();
    session.pump(IDLE_WINDOW);
    let idle_bytes = session.bytes_since(mark);

    assert_eq!(
        idle_bytes,
        0,
        "an idle client wrote {idle_bytes} bytes; the idle contract is zero frames\n{}",
        String::from_utf8_lossy(&session.captured[mark..]).escape_debug()
    );
}

/// `vibex` hosts the authority runtime in its own process, and the runtime
/// reports startup stages on background tasks after the first frame is up.
/// While the interface owns the terminal none of that may reach it — and none
/// of it may be lost either.
#[test]
fn in_process_diagnostics_never_reach_the_terminal() {
    let mut session = Session::start_with(120, 40, &[("VIBEX_TUI_HARNESS_STRAY_STDERR", "1")]);
    session.wait_for_first_frame();
    // Let a burst of stray writes happen while the interface owns the screen.
    session.pump(Duration::from_millis(700));

    // Everything after the alternate-screen switch belongs to the interface.
    // The bytes are checked rather than only the rendered frame: a write that
    // scrolls the grid can leave the visible cells looking plausible while the
    // user's terminal has already been displaced.
    let raw = String::from_utf8_lossy(&session.captured).to_string();
    let owned = raw
        .rsplit_once("\u{1b}[?1049h")
        .map(|(_, tail)| tail)
        .expect("the client entered the alternate screen");
    assert!(
        !owned.contains("harness_stray"),
        "a write aimed at stderr reached the terminal:\n{}",
        session.raw_tail()
    );

    // Quit so the client restores the terminal and names the spill file.
    session.send(b"\x03");
    session.pump(Duration::from_millis(300));
    session.send(b"\x03");
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    while session.child.try_wait().ok().flatten().is_none() {
        if Instant::now() >= deadline {
            panic!("the client did not exit after the second Ctrl+C");
        }
        session.pump(Duration::from_millis(50));
    }

    let spilled = session.spilled();
    assert!(
        spilled.contains("vibex-startup: stage-begin stage=harness_stray_"),
        "the diagnostics were dropped instead of diverted:\n{spilled}"
    );
    let raw = String::from_utf8_lossy(&session.captured).to_string();
    assert!(
        raw.contains("in-process diagnostics were captured"),
        "the client never said where the diverted output went:\n{}",
        session.raw_tail()
    );
}

#[test]
fn typing_echoes_into_the_frame() {
    // The client opens with the prompt in front of the reader, so a fresh
    // client can be typed into without opening anything first.
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    session.send("中文 abc".as_bytes());
    let screen = session.wait_for(|screen| screen.contains("abc"));
    assert!(screen.contains("abc"), "{screen}");
    // The wide characters must land in the frame too, which is what proves the
    // input path handles multi-byte characters rather than dropping them.
    assert!(screen.contains('中'), "{screen}");
}

#[test]
fn the_workspace_key_opens_the_picker_on_the_new_session_page() {
    // The page advertises `Ctrl+W` as the way to change where the session will
    // work, and the composer binds the same chord to its word kill. Which one
    // answers is decided in dispatch, by scope order — the layer no state test
    // can see — so the reader who pressed the key the page named got nothing.
    //
    // The picker then has to *list* something: this seat is native, which
    // browses the machine it runs on rather than reporting the capability a
    // paired client uses, so the listing is the client's own filesystem — the
    // directory this client was started in, which is the page's own answer.
    let directory = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(directory.path().join("clash-report")).expect("a directory to choose");
    let mut session = Session::start_at(Some(directory.path()), 120, 40, &[]);
    // The prompt is the first screen, so there is nothing to open first.
    session.wait_for_first_frame();
    // Ctrl+W, with the empty draft the page is in when it makes the promise.
    session.send(b"\x17");
    let screen = session.wait_for(|screen| screen.contains("clash-report"));
    // The picker's own hint row, which is what says the rows are a listing
    // rather than a line of the page behind it.
    assert!(
        screen.contains("Workspace") && screen.contains("nav"),
        "the picker did not open on the directory:\n{screen}"
    );
}

#[test]
fn the_workspace_picker_walks_out_of_the_directory_it_opened_on() {
    // The picker opens on the directory the page names, so a reader choosing a
    // workspace somewhere else on the machine has to be able to walk out of it.
    // The listing draws the way up as its own `..` row, `u` is the key the
    // footer names, and `←` is the arrow a reader reaches for; all three ask
    // for the parent listing. The key did not: it re-entered the picker's own
    // handler until the stack ran out, which is a crash no state test can see.
    let parent = tempfile::tempdir().expect("a temporary directory");
    let project = parent.path().join("project");
    let notes = parent.path().join("notes");
    std::fs::create_dir(&project).expect("a directory to open on");
    std::fs::create_dir(&notes).expect("a sibling to walk to");
    let mut session = Session::start_at(Some(&project), 120, 40, &[]);
    session.wait_for_first_frame();
    session.send(b"\x17");
    let screen = session.wait_for(|screen| screen.contains(".."));
    assert!(
        !screen.contains("notes"),
        "the picker opened above the directory it was told to open on:\n{screen}"
    );

    // `←` climbs: the parent is where the sibling shows up.
    session.send(b"\x1b[D");
    session.wait_for(|screen| screen.contains("notes"));

    // `u` climbs on from there, and the way up is still drawn.
    session.send(b"u");
    let screen = session.wait_for(|screen| !screen.contains("notes"));
    assert!(
        screen.contains(".."),
        "the way out of the directory was lost:\n{screen}"
    );
}

#[test]
fn the_workspace_picker_opens_a_folder_and_takes_it() {
    // `Enter` opens the highlighted directory — the picker walks the tree the
    // way a file manager does — and `Space` takes the directory the picker is
    // showing, which is what the page then works in. Both keys are answered
    // before the binding table, so this is also the end-to-end proof that the
    // arrows and the space bar reach the picker at all.
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir_all(root.path().join("project").join("nested")).expect("a tree to walk");
    let mut session = Session::start_at(Some(root.path()), 120, 40, &[]);
    session.wait_for_first_frame();
    session.send(b"\x17");
    session.wait_for(|screen| screen.contains("project"));

    // The cursor starts on the `..` row; Down walks it onto the only directory
    // and Enter opens that directory rather than choosing it.
    session.send(b"\x1b[B");
    session.send(b"\r");
    session.wait_for(|screen| screen.contains("nested"));

    // Down again onto the nested directory, Enter opens it: the picker is now
    // showing a directory with nothing in it but the way back up.
    session.send(b"\x1b[B");
    session.send(b"\r");
    session.wait_for(|screen| screen.contains("Parent directory"));

    // `Space` takes the directory being shown, and the page names it.
    session.send(b" ");
    let screen = session
        .wait_for(|screen| !screen.contains("Parent directory") && screen.contains("nested"));
    assert!(
        !screen.contains("Parent directory"),
        "the picker stayed open:\n{screen}"
    );
}

#[test]
fn the_workspace_picker_takes_the_folder_under_the_cursor() {
    // The key takes the row the reader pointed at rather than the directory the
    // picker happens to be showing: with the cursor on `project`, `Space` makes
    // that folder the workspace without walking into it, and the footer says so
    // before the key is pressed.
    let root = tempfile::tempdir().expect("a temporary directory");
    std::fs::create_dir(root.path().join("project")).expect("a folder to point at");
    let mut session = Session::start_at(Some(root.path()), 120, 40, &[]);
    session.wait_for_first_frame();
    session.send(b"\x17");
    session.wait_for(|screen| screen.contains("project"));

    // The cursor starts on `..`, where the footer promises the directory being
    // shown; one Down moves it onto the folder and changes the promise.
    session.send(b"\x1b[B");
    let screen = session.wait_for(|screen| screen.contains("use selected"));
    assert!(
        !screen.contains("use this directory"),
        "the footer still promised the directory being shown:\n{screen}"
    );

    // Space takes the folder, and the picker closes over the choice.
    session.send(b" ");
    let screen = session
        .wait_for(|screen| !screen.contains("Parent directory") && screen.contains("project"));
    assert!(
        !screen.contains("Parent directory"),
        "the picker stayed open:\n{screen}"
    );
}

#[test]
fn a_resize_storm_does_not_lose_the_frame() {
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    // The pty is resized from the master side; the client sees SIGWINCH and
    // re-lays out. Rapid changes must not leave a torn frame.
    for (columns, rows) in [(80u16, 24u16), (200, 50), (100, 30), (120, 40)] {
        session.resize(columns, rows);
        session.pump(Duration::from_millis(150));
    }
    let screen = session.wait_for_first_frame();
    assert!(screen.contains("Sessions"), "{screen}");
}

#[test]
fn a_tiny_terminal_still_gets_the_interface() {
    // There is no minimum terminal size: a split pane that reports 30×8 gets
    // the interface rather than a notice saying the terminal is too small. The
    // composer's prompt mark and its placeholder are what say the frame is the
    // writing surface and not an error message.
    let mut session = Session::start(30, 8);
    let screen = session.wait_for(|screen| screen.contains('❯') && screen.contains("/ Commands"));
    assert!(
        !screen.contains("too small"),
        "a 30x8 terminal was refused:\n{screen}"
    );
}

#[test]
fn shrinking_into_a_tiny_size_keeps_the_composer() {
    // The other direction: a terminal that starts large and is dragged down to
    // a few rows must keep the place the reader types, with the bands around it
    // giving their rows back.
    let mut session = Session::start(120, 40);
    session.wait_for(|screen| screen.contains("Sessions"));
    session.resize(24, 5);
    let screen = session.wait_for(|screen| screen.contains('❯'));
    assert!(
        !screen.contains("too small"),
        "a 24x5 terminal was refused:\n{screen}"
    );
}

#[test]
fn the_session_list_paints_without_a_frame() {
    // The harness inherits the test process environment, so the assertion is
    // that the glyph set is chosen for the terminal rather than assumed. The
    // list is frameless — it is the page, not a panel on it — so what a
    // complete paint looks like is the page's own name over its action, with
    // no border drawn around either.
    let mut session = Session::start(100, 30);
    let screen = session.wait_for_first_frame();
    assert!(!screen.is_empty());
    session.send(b"\x0c"); // Ctrl+L: the session list, from the prompt.
    let screen = session
        .wait_for(|screen| screen.contains("Session list") && screen.contains("+ New session"));
    assert!(
        !screen.contains('╭') && !screen.contains('╰'),
        "the session list is wearing a frame again:\n{screen}"
    );
    assert!(
        !screen.lines().any(|line| {
            let line = line.trim();
            line.len() > 2 && line.starts_with('+') && line.ends_with('+')
        }),
        "the session list is wearing an ASCII frame:\n{screen}"
    );
}

/// `Shift+Enter` as a terminal that has been asked for the keyboard protocol
/// sends it: `CSI 13 ; 2 u`. A terminal that ignores the protocol sends the same
/// carriage return `Enter` sends — which is why `Ctrl+J`, a line feed every
/// terminal sends as a byte of its own, is the chord that works everywhere.
const SHIFT_ENTER: &[u8] = b"\x1b[13;2u";

/// The row a frame draws `needle` on, so an assertion can name *where* something
/// is rather than only that it is there.
fn row_of(screen: &str, needle: &str) -> Option<usize> {
    screen.lines().position(|line| line.contains(needle))
}

/// Quit a running client and wait for it to exit.
///
/// `Ctrl+C` is the only key that leaves, and it asks once: the first press
/// arms the quit and puts the hint on the status band, the second one goes.
fn quit(session: &mut Session) {
    session.send(b"\x03"); // Ctrl+C: nothing to cancel, so the band asks
    session.pump(Duration::from_millis(300));
    session.send(b"\x03"); // the second press leaves
    let deadline = Instant::now() + SETTLE_TIMEOUT;
    while session.child.try_wait().ok().flatten().is_none() {
        if Instant::now() >= deadline {
            panic!("the client did not exit after the second Ctrl+C");
        }
        session.pump(Duration::from_millis(50));
    }
}

#[test]
fn the_client_asks_the_terminal_for_disambiguated_keys() {
    // Without this push a terminal cannot report `Shift+Enter` as anything but
    // `Enter`, so it is the whole reason the chord can work: disambiguate escape
    // codes (bit 1) plus alternate keys (bit 4), which is what keeps shifted
    // text intact under the first flag.
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    let raw = String::from_utf8_lossy(&session.captured).to_string();
    assert!(
        raw.contains("\u{1b}[>5u"),
        "the client never asked for disambiguated keys:\n{}",
        session.raw_tail()
    );

    quit(&mut session);

    // Popped on the way out, so the program that gets the terminal back reads
    // the keys it expects.
    let raw = String::from_utf8_lossy(&session.captured).to_string();
    assert!(
        raw.contains("\u{1b}[<1u"),
        "the client never gave the keyboard protocol back:\n{}",
        session.raw_tail()
    );
}

#[test]
fn the_newline_chords_break_the_line_instead_of_sending() {
    // `Shift+Enter` is the chord the keyboard protocol makes possible and
    // `Ctrl+J` the one that works without it; both have to reach the composer as
    // a line break. Three words on three rows of the draft is what proves the
    // newline landed: a chord that submitted instead would send the draft and
    // leave the *next* word concatenated onto the last one, which is exactly the
    // bug this pair of chords exists to prevent.
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    for (word, chord) in [
        ("alpha", SHIFT_ENTER),
        ("bravo", b"\n".as_slice()),
        ("charlie", b"".as_slice()),
    ] {
        session.send(word.as_bytes());
        session.pump(Duration::from_millis(200));
        session.send(chord);
        session.pump(Duration::from_millis(200));
    }
    let screen = session.wait_for(|screen| screen.contains("charlie"));
    let rows = ["alpha", "bravo", "charlie"].map(|word| {
        row_of(&screen, word).unwrap_or_else(|| panic!("{word} is not on screen:\n{screen}"))
    });
    assert!(
        rows[0] < rows[1] && rows[1] < rows[2],
        "the words are not on rows of their own:\n{screen}"
    );
    for concatenated in ["alphabravo", "bravocharlie"] {
        assert!(
            !screen.contains(concatenated),
            "{concatenated} was typed on one line:\n{screen}"
        );
    }
    // The draft's first row carries the prompt mark and its continuations are
    // indented past it — which is what says these are lines of one draft rather
    // than rows of three.
    let lines = screen.lines().collect::<Vec<_>>();
    assert!(
        lines[rows[0]].contains('❯'),
        "the first line is not the draft's:\n{screen}"
    );
    for row in &rows[1..] {
        assert!(
            !lines[*row].contains('❯'),
            "a continuation line started a new draft:\n{screen}"
        );
    }
}

#[test]
fn enter_still_sends_where_the_newline_chords_do_not() {
    // The other half of the contract: a newline chord must not take `Enter`'s
    // job. A draft of nothing but line breaks is the case the two paths can be
    // told apart in — the send path refuses it by name, the newline chord says
    // nothing at all.
    let mut session = Session::start(120, 40);
    session.wait_for_first_frame();
    for chord in [SHIFT_ENTER, b"\n".as_slice()] {
        session.send(chord);
        session.pump(Duration::from_millis(250));
        let screen = session.screen();
        assert!(
            !screen.contains("Message is empty"),
            "a newline chord took the send path:\n{screen}"
        );
    }
    session.send(b"\r");
    let screen = session.wait_for(|screen| screen.contains("Message is empty"));
    assert!(screen.contains('❯'), "{screen}");
}

/// The mode report a terminal that implements OSC 5522 sends back.
const CAPABILITY_YES: &[u8] = b"\x1b[?5522;2$y";

/// A one-pixel PNG, base64, exactly as the protocol carries a picture.
///
/// The client never decodes it in this test — the chip is what the reader sees
/// — but a real PNG is what a terminal would answer with, so the test speaks
/// the same wire format the reader's clipboard would.
const PIXEL_PNG: &str =
    "iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mP8z8AAAwAB/AGtE0zaAAAAAElFTkSuQmCC";

/// One `DATA` packet of the answer, carrying a chunk of a PNG.
fn data_packet(chunk: &str) -> Vec<u8> {
    // `aW1hZ2UvcG5n` is `image/png`: the media type travels base64 too.
    format!("\x1b]5522;type=read:status=DATA:mime=aW1hZ2UvcG5n;{chunk}\x1b\\").into_bytes()
}

/// The answer to the type list: one `DATA` packet per media type on offer.
fn types_packet(mimes: &[&str]) -> Vec<u8> {
    let mut packets = Vec::new();
    for mime in mimes {
        // The listed types carry their name and no data: the list is what a
        // terminal may hand over without asking its reader.
        packets.extend_from_slice(
            format!(
                "\x1b]5522;type=read:status=DATA:mime={};\x1b\\",
                base64(mime.as_bytes())
            )
            .as_bytes(),
        );
    }
    packets.extend_from_slice(b"\x1b]5522;type=read:status=DONE\x1b\\");
    packets
}

/// Base64, so the test can spell the media types the way the wire does.
fn base64(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut output = String::new();
    for chunk in bytes.chunks(3) {
        let b0 = chunk[0] as u32;
        let b1 = *chunk.get(1).unwrap_or(&0) as u32;
        let b2 = *chunk.get(2).unwrap_or(&0) as u32;
        let triple = (b0 << 16) | (b1 << 8) | b2;
        output.push(ALPHABET[((triple >> 18) & 0x3f) as usize] as char);
        output.push(ALPHABET[((triple >> 12) & 0x3f) as usize] as char);
        output.push(if chunk.len() > 1 {
            ALPHABET[((triple >> 6) & 0x3f) as usize] as char
        } else {
            '='
        });
        output.push(if chunk.len() > 2 {
            ALPHABET[(triple & 0x3f) as usize] as char
        } else {
            '='
        });
    }
    output
}

/// The client's request for the media types the clipboard offers.
const TYPES_REQUEST: &[u8] = b"\x1b]5522;type=read;Lg==\x1b\\";

/// The client's request for the clipboard's data, which the terminal confirms
/// with its reader.
const DATA_REQUEST: &[u8] = b"\x1b]5522;type=read:name=";

/// A terminal with no clipboard tools of its own.
///
/// This is the `ssh` case in one line: the host the client runs on cannot see
/// the reader's clipboard, so the terminal is the only one that can hand it
/// over. An empty `PATH` is what guarantees the host's tools are not found
/// first — a developer machine with `wl-paste` installed would otherwise answer
/// with their real clipboard and the test would prove nothing.
fn session_without_host_clipboard() -> Session {
    Session::start_with(120, 40, &[("PATH", "/nonexistent")])
}

#[test]
fn a_paste_over_a_link_takes_the_picture_from_the_terminal() {
    let mut session = session_without_host_clipboard();
    session.wait_for_first_frame();

    // `Ctrl+V` is the reader's paste chord.
    session.send(b"\x16");

    // The client asks whether the protocol is there before it asks for data,
    // because only a terminal that answers will ever answer the read.
    let mark = session.captured.len();
    assert!(
        session.wait_for_output(b"\x1b[?5522$p", mark),
        "the client never asked the terminal about its clipboard:\n{}",
        session.raw_tail()
    );
    session.send(CAPABILITY_YES);

    // Asking for the clipboard prompts the terminal's reader, so the client
    // asks what is on offer first — a request no terminal puts to its reader —
    // and only asks for data when there is something it can use.
    let mark = session.captured.len();
    assert!(
        session.wait_for_output(TYPES_REQUEST, mark),
        "the client never asked the terminal what it holds:\n{}",
        session.raw_tail()
    );
    session.send(&types_packet(&["image/png"]));

    let mark = session.captured.len();
    assert!(
        session.wait_for_output(DATA_REQUEST, mark),
        "the client never asked the terminal for its clipboard:\n{}",
        session.raw_tail()
    );

    // The picture arrives in chunks and ends with `DONE`, which is the shape
    // every answer to a read has.
    let (first, second) = PIXEL_PNG.split_at(32);
    session.send(&data_packet(first));
    session.send(&data_packet(second));
    session.send(b"\x1b]5522;type=read:status=DONE\x1b\\");

    let screen = session.wait_for(|screen| screen.contains("[Image #1]"));
    assert!(screen.contains("Image attached"), "{screen}");
    // The answer was read off the input queue, so none of it may have reached
    // the draft as text: that is the failure this whole path exists to avoid.
    assert!(
        !screen.contains("5522") && !screen.contains(PIXEL_PNG),
        "the answer was typed into the draft:\n{screen}"
    );
}

#[test]
fn a_silent_terminal_leaves_the_draft_alone() {
    // Nothing answers the capability query, which is every terminal that does
    // not implement the protocol. The client has to give the input queue back
    // and say what happened rather than wait for an answer that is not coming.
    let mut session = session_without_host_clipboard();
    session.wait_for_first_frame();
    session.send(b"\x16");
    let screen = session.wait_for(|screen| screen.contains("Nothing to paste"));
    assert!(screen.contains('❯'), "{screen}");

    session.send(b"typed after the paste");
    let screen = session.wait_for(|screen| screen.contains("typed after the paste"));
    assert!(
        !screen.contains("5522"),
        "the capability query was typed into the draft:\n{screen}"
    );
}

#[test]
fn a_terminal_that_refuses_the_read_says_so() {
    // The default terminal configuration asks its reader before it hands the
    // clipboard over, and the answer may be no. "Nothing to paste" would send
    // the reader looking in the wrong place.
    let mut session = session_without_host_clipboard();
    session.wait_for_first_frame();
    session.send(b"\x16");

    let mark = session.captured.len();
    assert!(
        session.wait_for_output(b"\x1b[?5522$p", mark),
        "the client never asked the terminal about its clipboard:\n{}",
        session.raw_tail()
    );
    session.send(CAPABILITY_YES);

    let mark = session.captured.len();
    assert!(
        session.wait_for_output(TYPES_REQUEST, mark),
        "the client never asked the terminal what it holds:\n{}",
        session.raw_tail()
    );
    session.send(&types_packet(&["image/png"]));

    let mark = session.captured.len();
    assert!(
        session.wait_for_output(DATA_REQUEST, mark),
        "the client never asked the terminal for its clipboard:\n{}",
        session.raw_tail()
    );
    session.send(b"\x1b]5522;type=read:status=EPERM\x1b\\");

    let screen = session.wait_for(|screen| screen.contains("would not hand over its clipboard"));
    assert!(!screen.contains("Nothing to paste"), "{screen}");
}

#[test]
fn a_gesture_with_nothing_to_take_never_asks_for_data() {
    // `Alt+I` wants a picture. The clipboard holds text, which the type list
    // says without asking the reader anything — so the request that *would*
    // make the terminal ask ("allow this program to read the clipboard?") is
    // never sent, and the composer falls back to the path prompt it always had.
    let mut session = session_without_host_clipboard();
    session.wait_for_first_frame();
    session.send(b"\x1bi");

    // The capability query comes first, and this terminal speaks the protocol.
    let mark = session.captured.len();
    assert!(
        session.wait_for_output(b"\x1b[?5522$p", mark),
        "the client never asked the terminal about its clipboard:\n{}",
        session.raw_tail()
    );
    session.send(CAPABILITY_YES);

    let mark = session.captured.len();
    assert!(
        session.wait_for_output(TYPES_REQUEST, mark),
        "the client never asked the terminal what it holds:\n{}",
        session.raw_tail()
    );
    session.send(&types_packet(&["text/plain", "text/html"]));

    let screen = session.wait_for(|screen| screen.contains("No image on the clipboard"));
    assert!(
        !session.wait_for_output(DATA_REQUEST, mark),
        "the client asked for data the gesture cannot use:\n{}",
        session.raw_tail()
    );
    assert!(screen.contains("Image path"), "{screen}");
}

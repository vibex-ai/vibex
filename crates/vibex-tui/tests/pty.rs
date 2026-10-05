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
    /// Owns the spill file the client diverts in-process diagnostics into.
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
        screen.contains("Runtime") && screen.contains("Workspace"),
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

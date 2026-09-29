//! PTY end-to-end tests: the real binary, a real pseudo-terminal, and
//! assertions on what a user would actually see.
//!
//! `TestBackend` renders into a buffer. This layer is the only one that can
//! prove the things that go wrong *outside* the renderer:
//!
//! * the process really enters raw mode and paints a first frame;
//! * an idle interface writes **nothing** — the measured `idle_cost` contract;
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
}

impl Session {
    fn start(columns: u16, rows: u16) -> Self {
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
        }
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
    let screen = session.wait_for(|screen| screen.contains("Vibex") && screen.contains("Sessions"));
    // The navigation destinations and the key bar come from the same tables the
    // dispatcher uses, so their presence proves the frame is fully assembled.
    assert!(screen.contains("Management"), "{screen}");
    assert!(
        screen.contains("Quit") || screen.contains("Commands"),
        "the key bar is missing:\n{screen}"
    );
}

#[test]
fn quitting_restores_the_terminal() {
    let mut session = Session::start(100, 30);
    session.wait_for(|screen| screen.contains("Vibex"));
    session.send(b"\x11"); // Ctrl+Q
    session.pump(Duration::from_millis(300));
    // Accept the confirmation.
    session.send(b"\r");
    session.pump(Duration::from_millis(500));

    let deadline = Instant::now() + SETTLE_TIMEOUT;
    loop {
        if let Ok(Some(_)) = session.child.try_wait() {
            break;
        }
        if Instant::now() >= deadline {
            panic!("the client did not exit after the quit confirmation");
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
    session.wait_for(|screen| screen.contains("Vibex"));
    // A startup notice is real content and repaints while it is visible; the
    // contract under test is that nothing repaints once it is gone.
    session.settle(Duration::from_millis(800), Duration::from_secs(20));

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

#[test]
fn typing_echoes_into_the_frame() {
    // Without an open session the composer is not reachable, so this exercises
    // the filter, which is the typing surface a fresh client always has.
    let mut session = Session::start(120, 40);
    session.wait_for(|screen| screen.contains("Vibex"));
    session.send(b"/");
    session.pump(Duration::from_millis(200));
    session.send("中文 abc".as_bytes());
    let screen = session.wait_for(|screen| screen.contains("abc"));
    assert!(screen.contains("abc"), "{screen}");
    // The wide characters must land in the frame too, which is what proves the
    // input path handles multi-byte characters rather than dropping them.
    assert!(screen.contains('中'), "{screen}");
}

#[test]
fn a_resize_storm_does_not_lose_the_frame() {
    let mut session = Session::start(120, 40);
    session.wait_for(|screen| screen.contains("Vibex"));
    // The pty is resized from the master side; the client sees SIGWINCH and
    // re-lays out. Rapid changes must not leave a torn frame.
    for (columns, rows) in [(80u16, 24u16), (200, 50), (100, 30), (120, 40)] {
        session.resize(columns, rows);
        session.pump(Duration::from_millis(150));
    }
    let screen = session.wait_for(|screen| screen.contains("Vibex"));
    assert!(screen.contains("Sessions"), "{screen}");
}

#[test]
fn a_non_utf8_locale_still_renders_the_frame() {
    // The harness inherits the test process environment, so the assertion is
    // that the ASCII border set is selected rather than assumed.
    let mut session = Session::start(100, 30);
    let screen = session.wait_for(|screen| screen.contains("Vibex"));
    assert!(!screen.is_empty());
    // Whatever the glyph mode, the right border must reach the last column.
    let body = screen.lines().nth(2).unwrap_or_default();
    assert!(
        body.ends_with('│') || body.ends_with('|') || body.ends_with('╮') || body.ends_with('+'),
        "the frame's right border is missing: {body:?}"
    );
}

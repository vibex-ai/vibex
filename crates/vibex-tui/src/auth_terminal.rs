//! The sign-in terminal: a PTY the authority opened, drawn inside the client.
//!
//! A terminal authentication method runs the Agent's own login flow — an
//! interactive TUI of its own — in a terminal the *authority* owns. The client
//! is a viewer by id: the frames arrive from `TerminalBackend`, the reader's
//! keys travel back through `write_terminal`, and the process is never the
//! client's. That is what makes this work in both seats, because the authority
//! seat and a remote seat serve the same terminal domain.
//!
//! The emulator lives here rather than in the view because a frame is *state*:
//! the view draws whatever the last frame put on the grid, and the bytes that
//! produce the next one arrive asynchronously on the worker. Keeping the
//! emulator in the client's own state is what lets a repaint be a pure function
//! of what has already arrived.
//!
//! What is deliberately *not* here: running the command. The descriptor the
//! authority returns names the command, arguments, and environment values, but
//! a client that executed them would be a client running an authority-chosen
//! program on the machine the reader is sitting at. The terminal is the
//! authority's; this module only looks at it.

use vibex_terminal_ui::{
    TerminalCellSnapshot, TerminalCursorShape, TerminalEmulator, TerminalFrameSnapshot,
};

/// The size the client asks the terminal for, as rows and columns.
///
/// A sign-in flow is a small interactive program — a list of providers, a
/// prompt, a "press enter to open the browser" — so it does not need the
/// client's whole grid. It is drawn inside a modal, and the modal's content
/// area is what bounds it; this is the ceiling rather than the size.
pub const AUTH_TERMINAL_ROWS: u16 = 24;
pub const AUTH_TERMINAL_COLUMNS: u16 = 100;

/// One attached sign-in terminal.
pub struct AuthTerminal {
    /// The Agent whose account is being signed into, so an answer that lands
    /// late cannot be drawn over a different panel.
    pub agent_id: vibex_core::AgentId,
    pub terminal_id: vibex_core::TerminalId,
    pub title: String,
    emulator: TerminalEmulator,
    /// The grid as of the last frame, which is what the view reads.
    snapshot: Option<TerminalFrameSnapshot>,
    /// Bytes received but not yet fed to the emulator.
    ///
    /// A UTF-8 sequence can be split across two frames, so the tail of a
    /// partial character is held here between them rather than replaced with a
    /// substitute glyph. A terminal cannot report "wait", and a login prompt
    /// that renders a stray `?` over the character it could not decode is a
    /// prompt the reader cannot copy.
    pending: Vec<u8>,
}

impl AuthTerminal {
    pub fn new(
        agent_id: vibex_core::AgentId,
        terminal_id: vibex_core::TerminalId,
        title: String,
        rows: u16,
        columns: u16,
    ) -> Self {
        let rows = rows.clamp(1, AUTH_TERMINAL_ROWS).max(1);
        let columns = columns.clamp(1, AUTH_TERMINAL_COLUMNS).max(1);
        Self {
            agent_id,
            terminal_id,
            title,
            emulator: TerminalEmulator::new(rows, columns),
            snapshot: None,
            pending: Vec::new(),
        }
    }

    /// Feed the bytes of one frame.
    ///
    /// The frame's tail may be a partial character; only the complete prefix is
    /// advanced, and the rest waits for the next frame. A byte stream that
    /// never becomes valid UTF-8 is eventually flushed as replacement
    /// characters rather than held forever, so one corrupt frame cannot wedge
    /// the terminal.
    pub fn advance(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        self.pending.extend_from_slice(bytes);
        let valid = match std::str::from_utf8(&self.pending) {
            Ok(_) => self.pending.len(),
            Err(error) => error.valid_up_to(),
        };
        if valid > 0 {
            let complete = self.pending.drain(..valid).collect::<Vec<_>>();
            self.emulator.advance(&complete);
        }
        // A partial tail longer than the longest UTF-8 sequence (4 bytes) is
        // not a split character; it is a stream that will not decode, and
        // holding it would stall every frame after it.
        if self.pending.len() > 3 {
            let pending = std::mem::take(&mut self.pending);
            self.emulator
                .advance(&String::from_utf8_lossy(&pending).into_owned().into_bytes());
        }
    }

    /// Resize the grid the next frame is rendered into.
    pub fn resize(&mut self, rows: u16, columns: u16) {
        let rows = rows.clamp(1, AUTH_TERMINAL_ROWS).max(1);
        let columns = columns.clamp(1, AUTH_TERMINAL_COLUMNS).max(1);
        if self.emulator.rows() == rows && self.emulator.columns() == columns {
            return;
        }
        self.emulator.resize(rows, columns);
        self.snapshot = None;
    }

    /// Take the grid as it now stands.
    ///
    /// The emulator's own damage tracking is for a client that repaints the
    /// whole grid from a snapshot; this one draws every visible cell from the
    /// frame it holds, so what it wants is the settled grid rather than the
    /// rows that moved. Calling this after every batch is what keeps the view a
    /// pure function of the last thing that arrived.
    pub fn refresh(&mut self) {
        self.snapshot = Some(self.emulator.frame());
    }

    pub fn rows(&self) -> u16 {
        self.emulator.rows()
    }

    pub fn columns(&self) -> u16 {
        self.emulator.columns()
    }

    /// The grid to draw, if anything has arrived yet.
    pub fn snapshot(&self) -> Option<&TerminalFrameSnapshot> {
        self.snapshot.as_ref()
    }

    /// The visible text, one line per row, for a client that cannot draw the
    /// grid cell by cell (a narrow terminal, or a colour-less one).
    pub fn visible_text(&self) -> String {
        self.emulator.visible_text()
    }

    /// Whether the grid has a cell at this position, so a click can be mapped
    /// to a real position rather than a phantom one.
    pub fn contains(&self, row: u16, column: u16) -> bool {
        row < self.emulator.rows() && column < self.emulator.columns()
    }

    /// The cursor, if the program inside is showing one.
    pub fn cursor(&self) -> Option<(u16, u16, TerminalCursorShape)> {
        self.snapshot.as_ref().and_then(|snapshot| {
            snapshot
                .cursor
                .map(|cursor| (cursor.row, cursor.column, cursor.shape))
        })
    }

    /// Whether the program inside asked for mouse reporting.
    ///
    /// A login flow that draws its own list needs the pointer, and it asked the
    /// terminal for it in the way a terminal is asked: by emitting the mode. A
    /// click the client kept for itself would be a click the list never saw.
    pub fn wants_mouse(&self) -> bool {
        self.snapshot
            .as_ref()
            .is_some_and(|snapshot| snapshot.modes.mouse_reporting)
    }

    /// The cells of one row, trimmed of the trailing blanks a terminal pads
    /// every line with.
    pub fn row_cells(&self, row: u16) -> Vec<&TerminalCellSnapshot> {
        self.snapshot
            .as_ref()
            .map(|snapshot| {
                snapshot
                    .cells
                    .iter()
                    .filter(|cell| cell.row == row)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn terminal() -> AuthTerminal {
        AuthTerminal::new(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::TerminalId::new(),
            "Sign in".to_string(),
            6,
            20,
        )
    }

    #[test]
    fn the_grid_keeps_the_text_that_was_written_to_it() {
        let mut terminal = terminal();
        terminal.advance(b"Sign in with ChatGPT\r\n");
        terminal.refresh();
        assert!(
            terminal.visible_text().contains("Sign in with ChatGPT"),
            "the frame did not reach the grid: {:?}",
            terminal.visible_text()
        );
    }

    #[test]
    fn a_character_split_across_two_frames_is_held_rather_than_mangled() {
        // "中" is three bytes; a PTY flushes on its own boundaries, so the
        // split is the common case rather than a corner. Dropping the partial
        // sequence would put a replacement glyph in a login prompt.
        let mut terminal = terminal();
        let bytes = "中文".as_bytes();
        terminal.advance(&bytes[..4]);
        terminal.advance(&bytes[4..]);
        terminal.refresh();
        assert!(
            terminal.visible_text().contains("中文"),
            "the split character was not reassembled: {:?}",
            terminal.visible_text()
        );
    }

    #[test]
    fn a_stream_that_never_decodes_does_not_wedge_the_terminal() {
        let mut terminal = terminal();
        // A lone continuation byte is never the start of a character.
        terminal.advance(&[0x80, 0x80, 0x80, 0x80, 0x80, 0x80]);
        terminal.advance(b"after");
        terminal.refresh();
        assert!(
            terminal.visible_text().contains("after"),
            "the stream stalled behind bytes that cannot decode: {:?}",
            terminal.visible_text()
        );
    }

    #[test]
    fn a_program_that_asks_for_the_mouse_is_reported_as_wanting_it() {
        let mut terminal = terminal();
        terminal.advance(b"plain text");
        terminal.refresh();
        assert!(!terminal.wants_mouse());

        // DECSET 1000 is "report mouse presses", which a login list emits to
        // make its rows clickable.
        terminal.advance(b"\x1b[?1000h");
        terminal.refresh();
        assert!(terminal.wants_mouse());
    }

    #[test]
    fn resizing_changes_the_grid_and_drops_the_stale_frame() {
        let mut terminal = terminal();
        terminal.advance(b"hello");
        terminal.refresh();
        assert!(terminal.snapshot().is_some());
        terminal.resize(3, 10);
        assert_eq!((terminal.rows(), terminal.columns()), (3, 10));
        assert!(
            terminal.snapshot().is_none(),
            "a frame drawn at the old size was kept after a resize"
        );
    }

    #[test]
    fn the_requested_size_is_clamped_to_what_a_login_flow_needs() {
        let terminal = AuthTerminal::new(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::TerminalId::new(),
            "Sign in".to_string(),
            500,
            900,
        );
        assert_eq!(terminal.rows(), AUTH_TERMINAL_ROWS);
        assert_eq!(terminal.columns(), AUTH_TERMINAL_COLUMNS);
    }
}

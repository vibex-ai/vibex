use serde::Serialize;
use vibex_core::{TerminalId, TerminalResizeRequest, VibexError, VibexResult};
use vibex_terminal::{
    TerminalCellSnapshot, TerminalCursorSnapshot, TerminalEmulator, TerminalFrameSnapshot,
    TerminalGridPoint, TerminalHostSignals, TerminalManager, TerminalModeSnapshot,
    TerminalRawSnapshot, TerminalScrollbackMatch, TerminalSearchMatch,
};

use crate::{
    ContentResourceMetrics, ContentSurfaceKind, ContentSurfaceLifecycle, ContentSurfaceOrigin,
};

pub const TERMINAL_RAW_BUFFER_BYTES: usize = 16 * 1024 * 1024;
pub const TERMINAL_COMPATIBILITY_CHUNKS: usize = 2_000;
pub const TERMINAL_SCROLLBACK_LINES: usize = 10_000;
pub const TERMINAL_MODEL_BUDGET_BYTES: usize = 128 * 1024 * 1024;
pub const TERMINAL_EMULATOR_REVISION: &str = "alacritty-terminal-0.26";

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrameUpdate {
    pub full_repaint: bool,
    pub changed_rows: Vec<u16>,
    pub changed_cells: usize,
}

#[derive(Debug, Clone)]
pub struct TerminalFrameCache {
    rows: u16,
    columns: u16,
    cells: Vec<Option<TerminalCellSnapshot>>,
    cursor: Option<TerminalCursorSnapshot>,
    modes: TerminalModeSnapshot,
    history_lines: usize,
    display_offset: usize,
    ingested_bytes: u64,
    force_full_repaint: bool,
}

impl TerminalFrameCache {
    pub fn new(frame: &TerminalFrameSnapshot) -> Self {
        let mut cache = Self {
            rows: frame.rows,
            columns: frame.columns,
            cells: vec![None; frame_cell_capacity(frame.rows, frame.columns)],
            cursor: frame.cursor,
            modes: frame.modes,
            history_lines: frame.history_lines,
            display_offset: frame.display_offset,
            ingested_bytes: frame.ingested_bytes,
            force_full_repaint: true,
        };
        cache.apply(frame);
        cache
    }

    pub fn apply(&mut self, frame: &TerminalFrameSnapshot) -> TerminalFrameUpdate {
        let dimensions_changed = self.rows != frame.rows || self.columns != frame.columns;
        let full_repaint = dimensions_changed || frame.full_damage || self.force_full_repaint;
        let mut changed_rows = Vec::new();
        let mut changed_cells = 0usize;

        if full_repaint {
            self.rows = frame.rows;
            self.columns = frame.columns;
            self.cells = vec![None; frame_cell_capacity(frame.rows, frame.columns)];
            for cell in &frame.cells {
                if let Some(index) =
                    frame_cell_index(frame.rows, frame.columns, cell.row, cell.column)
                {
                    self.cells[index] = Some(cell.clone());
                    changed_cells = changed_cells.saturating_add(1);
                }
            }
            changed_rows.extend(0..frame.rows);
        } else {
            let mut row_changed = vec![false; usize::from(frame.rows)];
            for damage in &frame.damage {
                if damage.row >= frame.rows {
                    continue;
                }
                let start = damage.start_column.min(frame.columns.saturating_sub(1));
                let end = damage.end_column.min(frame.columns.saturating_sub(1));
                for column in start..=end {
                    if let Some(index) =
                        frame_cell_index(frame.rows, frame.columns, damage.row, column)
                    {
                        self.cells[index] = None;
                    }
                }
                row_changed[usize::from(damage.row)] = true;
                changed_cells = changed_cells.saturating_add(usize::from(end - start + 1));
            }
            for cell in &frame.cells {
                if row_changed
                    .get(usize::from(cell.row))
                    .copied()
                    .unwrap_or(false)
                    && frame.damage.iter().any(|damage| {
                        damage.row == cell.row
                            && cell.column >= damage.start_column
                            && cell.column <= damage.end_column
                    })
                    && let Some(index) =
                        frame_cell_index(frame.rows, frame.columns, cell.row, cell.column)
                {
                    self.cells[index] = Some(cell.clone());
                }
            }
            changed_rows.extend(
                row_changed
                    .into_iter()
                    .enumerate()
                    .filter_map(|(row, changed)| changed.then_some(row as u16)),
            );
        }

        self.cursor = frame.cursor;
        self.modes = frame.modes;
        self.history_lines = frame.history_lines;
        self.display_offset = frame.display_offset;
        self.ingested_bytes = frame.ingested_bytes;
        self.force_full_repaint = false;

        TerminalFrameUpdate {
            full_repaint,
            changed_rows,
            changed_cells,
        }
    }

    pub fn force_full_repaint(&mut self) {
        self.force_full_repaint = true;
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }

    pub fn columns(&self) -> u16 {
        self.columns
    }

    pub fn cells(&self) -> impl Iterator<Item = &TerminalCellSnapshot> {
        self.cells.iter().filter_map(Option::as_ref)
    }

    pub fn cell(&self, point: TerminalGridPoint) -> Option<&TerminalCellSnapshot> {
        frame_cell_index(self.rows, self.columns, point.row, point.column)
            .and_then(|index| self.cells.get(index))
            .and_then(Option::as_ref)
    }

    pub fn cursor(&self) -> Option<TerminalCursorSnapshot> {
        self.cursor
    }

    pub fn modes(&self) -> TerminalModeSnapshot {
        self.modes
    }

    pub fn history_lines(&self) -> usize {
        self.history_lines
    }

    pub fn display_offset(&self) -> usize {
        self.display_offset
    }

    pub fn ingested_bytes(&self) -> u64 {
        self.ingested_bytes
    }
}

fn frame_cell_capacity(rows: u16, columns: u16) -> usize {
    usize::from(rows).saturating_mul(usize::from(columns))
}

fn frame_cell_index(rows: u16, columns: u16, row: u16, column: u16) -> Option<usize> {
    if row >= rows || column >= columns {
        return None;
    }
    Some(
        usize::from(row)
            .saturating_mul(usize::from(columns))
            .saturating_add(usize::from(column)),
    )
}

pub fn new_ui_terminal_manager() -> TerminalManager {
    TerminalManager::with_raw_observation_capacity(
        TERMINAL_COMPATIBILITY_CHUNKS,
        TERMINAL_RAW_BUFFER_BYTES,
    )
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSyncOutcome {
    pub ingested_chunks: usize,
    pub ingested_bytes: usize,
    pub next_sequence: i64,
    pub gap_detected: bool,
    pub rebuilt: bool,
    pub dropped_chunks: u64,
    /// Bytes the emulator owes the program, to be written back into the PTY.
    ///
    /// A device-attributes, cursor-position or keyboard-protocol query is
    /// answered by the terminal core; the transport that owns the write side is
    /// the only one that can deliver it.
    pub pty_replies: Vec<u8>,
    /// Requests the program addressed to the host in this batch: a title, a
    /// clipboard operation, or a colour query.
    #[serde(default)]
    pub host: TerminalHostSignals,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSurfaceDiagnostics {
    pub backend: &'static str,
    pub backend_revision: &'static str,
    pub next_sequence: i64,
    pub ingested_bytes: u64,
    pub rebuild_count: u64,
    pub sequence_gap_count: u64,
    pub source_dropped_chunks: u64,
    pub rows: u16,
    pub columns: u16,
    pub history_lines: usize,
}

pub struct TerminalSurfaceBackend {
    lifecycle: ContentSurfaceLifecycle,
    emulator: TerminalEmulator,
    terminal_id: Option<TerminalId>,
    next_sequence: i64,
    rebuild_count: u64,
    sequence_gap_count: u64,
    source_dropped_chunks: u64,
}

impl TerminalSurfaceBackend {
    pub fn new(rows: u16, columns: u16) -> Self {
        Self {
            lifecycle: ContentSurfaceLifecycle::restored(
                ContentSurfaceKind::Terminal,
                ContentSurfaceOrigin::Preview,
            ),
            emulator: TerminalEmulator::new(rows, columns),
            terminal_id: None,
            next_sequence: 1,
            rebuild_count: 0,
            sequence_gap_count: 0,
            source_dropped_chunks: 0,
        }
    }

    pub fn lifecycle(&self) -> &ContentSurfaceLifecycle {
        &self.lifecycle
    }

    pub fn lifecycle_mut(&mut self) -> &mut ContentSurfaceLifecycle {
        &mut self.lifecycle
    }

    pub fn next_sequence(&self) -> i64 {
        self.next_sequence
    }

    pub fn modes(&self) -> vibex_terminal::TerminalModeSnapshot {
        self.emulator.modes()
    }

    pub fn sync(&mut self, snapshot: &TerminalRawSnapshot) -> VibexResult<TerminalSyncOutcome> {
        if snapshot.next_sequence < 1 {
            return Err(VibexError::validation(
                "terminal_raw_sequence_invalid",
                "terminal raw snapshot sequence must be positive",
            ));
        }
        if let Some(terminal_id) = &self.terminal_id {
            if terminal_id != &snapshot.session.id {
                self.reset_for(snapshot, false);
            }
        } else {
            self.terminal_id = Some(snapshot.session.id.clone());
            self.resize(snapshot.session.rows, snapshot.session.cols);
        }

        validate_raw_snapshot(snapshot)?;
        let first_available = snapshot.chunks.first().map(|chunk| chunk.sequence);
        let sequence_restarted = snapshot.next_sequence < self.next_sequence;
        let expected_missing = snapshot.next_sequence > self.next_sequence
            && !snapshot
                .chunks
                .iter()
                .any(|chunk| chunk.sequence == self.next_sequence);
        let retained_gap = first_available.is_some_and(|first| first > self.next_sequence);
        let rebuild = sequence_restarted || expected_missing || retained_gap;

        let mut ingested_chunks = 0;
        let mut ingested_bytes = 0;
        if rebuild {
            self.sequence_gap_count = self.sequence_gap_count.saturating_add(1);
            self.reset_for(snapshot, true);
            for chunk in &snapshot.chunks {
                self.emulator.advance(&chunk.data);
                ingested_chunks += 1;
                ingested_bytes += chunk.data.len();
            }
            self.next_sequence = snapshot.next_sequence;
        } else {
            let mut expected_sequence = self.next_sequence;
            for chunk in &snapshot.chunks {
                if chunk.sequence < expected_sequence {
                    continue;
                }
                if chunk.sequence != expected_sequence {
                    return Err(VibexError::conflict(
                        "terminal_raw_sequence_gap",
                        "terminal raw snapshot contains a non-contiguous sequence",
                    ));
                }
                self.emulator.advance(&chunk.data);
                expected_sequence += 1;
                ingested_chunks += 1;
                ingested_bytes += chunk.data.len();
            }
            self.next_sequence = expected_sequence;
            if self.next_sequence != snapshot.next_sequence {
                return Err(VibexError::conflict(
                    "terminal_raw_snapshot_incomplete",
                    "terminal raw snapshot does not reach its declared next sequence",
                ));
            }
        }
        self.source_dropped_chunks = snapshot.dropped_chunks;

        Ok(TerminalSyncOutcome {
            ingested_chunks,
            ingested_bytes,
            next_sequence: self.next_sequence,
            gap_detected: rebuild,
            rebuilt: rebuild,
            dropped_chunks: snapshot.dropped_chunks,
            // Drained even when a rebuild replayed every chunk: the program is
            // waiting on the last answer, not on the ones it already got.
            pty_replies: self.emulator.take_pty_replies(),
            host: self.emulator.take_host_signals(),
        })
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        self.emulator.resize(rows, columns);
    }

    pub fn frame(&mut self) -> TerminalFrameSnapshot {
        self.emulator.frame()
    }

    pub fn scroll(&mut self, line_delta: i32) {
        self.emulator.scroll(line_delta);
    }

    pub fn scroll_to_bottom(&mut self) {
        self.emulator.scroll_to_bottom();
    }

    /// Scroll so `offset` lines sit above the viewport.
    pub fn scroll_to_offset(&mut self, offset: usize) {
        self.emulator.scroll_to_offset(offset);
    }

    pub fn display_offset(&self) -> usize {
        self.emulator.display_offset()
    }

    /// The grid's height, without consuming damage the way `frame` does.
    pub fn rows(&self) -> u16 {
        self.emulator.rows()
    }

    pub fn history_lines(&self) -> usize {
        self.emulator.history_lines()
    }

    /// Every match in the retained buffer, scrollback included.
    pub fn search_scrollback(
        &self,
        query: &str,
        case_sensitive: bool,
    ) -> Vec<TerminalScrollbackMatch> {
        self.emulator.find_all(query, case_sensitive)
    }

    /// Selects the word under a point and returns its text.
    pub fn select_word_at(&mut self, point: TerminalGridPoint) -> VibexResult<String> {
        self.emulator.select_word_at(point)
    }

    /// Selects the whole line under a point and returns its text.
    pub fn select_line_at(&mut self, point: TerminalGridPoint) -> VibexResult<String> {
        self.emulator.select_line_at(point)
    }

    pub fn select_text(
        &mut self,
        start: TerminalGridPoint,
        end: TerminalGridPoint,
    ) -> VibexResult<String> {
        self.emulator.select_text(start, end)
    }

    pub fn clear_selection(&mut self) {
        self.emulator.clear_selection();
    }

    pub fn find_visible(&self, query: &str, case_sensitive: bool) -> Vec<TerminalSearchMatch> {
        self.emulator.find_visible(query, case_sensitive)
    }

    pub fn hyperlink_at(&self, point: TerminalGridPoint) -> VibexResult<Option<String>> {
        self.emulator.hyperlink_at(point)
    }

    pub fn encode_key(&self, key: TerminalKey, modifiers: TerminalModifiers) -> Option<Vec<u8>> {
        encode_key(key, modifiers, self.emulator.modes())
    }

    pub fn encode_paste(&self, text: &str) -> Vec<u8> {
        let sanitized = text.replace('\0', "");
        if self.emulator.modes().bracketed_paste {
            [b"\x1b[200~".as_slice(), sanitized.as_bytes(), b"\x1b[201~"].concat()
        } else {
            sanitized.into_bytes()
        }
    }

    pub fn encode_text(&self, text: &str) -> Vec<u8> {
        text.replace('\0', "").into_bytes()
    }

    pub fn encode_focus(&self, focused: bool) -> Option<&'static [u8]> {
        self.emulator
            .modes()
            .focus_reporting
            .then_some(if focused { b"\x1b[I" } else { b"\x1b[O" })
    }

    pub fn encode_mouse(&self, event: TerminalMouseEvent) -> Option<Vec<u8>> {
        encode_mouse(event, self.emulator.modes(), MouseCoordinates::Cells)
    }

    /// Encodes a report for a program that asked for pixel coordinates
    /// (`DECSET 1016`). The surface reports grid points, so the cell the pointer
    /// is over is expanded to that cell's centre in pixels.
    pub fn encode_mouse_pixels(
        &self,
        event: TerminalMouseEvent,
        cell_width: f32,
        cell_height: f32,
    ) -> Option<Vec<u8>> {
        encode_mouse(
            event,
            self.emulator.modes(),
            MouseCoordinates::Pixels {
                cell_width,
                cell_height,
            },
        )
    }

    pub fn diagnostics(&mut self) -> TerminalSurfaceDiagnostics {
        let frame = self.emulator.frame();
        TerminalSurfaceDiagnostics {
            backend: "alacritty-terminal",
            backend_revision: TERMINAL_EMULATOR_REVISION,
            next_sequence: self.next_sequence,
            ingested_bytes: frame.ingested_bytes,
            rebuild_count: self.rebuild_count,
            sequence_gap_count: self.sequence_gap_count,
            source_dropped_chunks: self.source_dropped_chunks,
            rows: frame.rows,
            columns: frame.columns,
            history_lines: frame.history_lines,
        }
    }

    pub fn resource_metrics(&mut self) -> ContentResourceMetrics {
        let frame = self.emulator.frame();
        let resident_items = frame.history_lines.saturating_add(usize::from(frame.rows));
        let resident_bytes = resident_items
            .saturating_mul(usize::from(frame.columns))
            .saturating_mul(std::mem::size_of::<TerminalCellSnapshot>());
        ContentResourceMetrics {
            resident_items,
            resident_bytes,
            budget_items: TERMINAL_SCROLLBACK_LINES.saturating_add(usize::from(frame.rows)),
            budget_bytes: TERMINAL_MODEL_BUDGET_BYTES,
            evictions: self.source_dropped_chunks,
        }
    }

    fn reset_for(&mut self, snapshot: &TerminalRawSnapshot, count_rebuild: bool) {
        self.emulator = TerminalEmulator::new(snapshot.session.rows, snapshot.session.cols);
        self.terminal_id = Some(snapshot.session.id.clone());
        self.next_sequence = snapshot
            .chunks
            .first()
            .map(|chunk| chunk.sequence)
            .unwrap_or(snapshot.next_sequence);
        if count_rebuild {
            self.rebuild_count = self.rebuild_count.saturating_add(1);
        }
    }
}

fn validate_raw_snapshot(snapshot: &TerminalRawSnapshot) -> VibexResult<()> {
    let mut previous = None;
    let mut retained_bytes = 0usize;
    for chunk in &snapshot.chunks {
        if chunk.sequence < 1 || previous.is_some_and(|previous| chunk.sequence != previous + 1) {
            return Err(VibexError::validation(
                "terminal_raw_snapshot_non_contiguous",
                "terminal raw snapshot chunks must be ordered and contiguous",
            ));
        }
        previous = Some(chunk.sequence);
        retained_bytes = retained_bytes.saturating_add(chunk.data.len());
    }
    if retained_bytes != snapshot.retained_bytes {
        return Err(VibexError::validation(
            "terminal_raw_snapshot_size_mismatch",
            "terminal raw snapshot retained byte count is inconsistent",
        ));
    }
    if previous.is_some_and(|previous| previous >= snapshot.next_sequence) {
        return Err(VibexError::validation(
            "terminal_raw_snapshot_next_sequence_invalid",
            "terminal raw snapshot next sequence must follow retained chunks",
        ));
    }
    Ok(())
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalKey {
    Text(char),
    Enter,
    Backspace,
    Tab,
    Escape,
    Up,
    Down,
    Left,
    Right,
    Home,
    End,
    PageUp,
    PageDown,
    Insert,
    Delete,
    Function(u8),
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct TerminalModifiers {
    pub shift: bool,
    pub alt: bool,
    pub control: bool,
}

fn encode_key(
    key: TerminalKey,
    modifiers: TerminalModifiers,
    modes: vibex_terminal::TerminalModeSnapshot,
) -> Option<Vec<u8>> {
    // A program that negotiated the kitty keyboard protocol is asking for the
    // chord itself, not for the text it would type. `Shift+Enter` has no legacy
    // encoding at all (it arrives as a plain carriage return), so a terminal
    // that pretends otherwise is what makes "newline" and "send" the same key.
    //
    // Everything below this branch is the legacy fallback, and it is what a
    // program that never negotiated still receives: the protocol is opt-in per
    // program, so enabling it here cannot change the bytes an unaware CLI gets.
    if modes.kitty_keyboard_flags & KITTY_DISAMBIGUATE != 0
        && let Some(sequence) = encode_kitty_key(key, modifiers, modes.kitty_keyboard_flags)
    {
        return Some(sequence);
    }
    if let TerminalKey::Text(character) = key {
        let mut output = Vec::new();
        if modifiers.alt {
            output.push(0x1b);
        }
        if modifiers.control && character.is_ascii() {
            let upper = character.to_ascii_uppercase() as u8;
            if upper == b'?' {
                output.push(0x7f);
            } else if (b'@'..=b'_').contains(&upper) {
                output.push(upper & 0x1f);
            } else {
                return None;
            }
        } else {
            let mut bytes = [0; 4];
            output.extend_from_slice(character.encode_utf8(&mut bytes).as_bytes());
        }
        return Some(output);
    }

    let modifier = 1
        + usize::from(modifiers.shift)
        + usize::from(modifiers.alt) * 2
        + usize::from(modifiers.control) * 4;
    let modified = modifier != 1;
    let sequence = match key {
        TerminalKey::Enter => "\r".to_string(),
        TerminalKey::Backspace => {
            if modifiers.alt {
                "\x1b\x7f".to_string()
            } else {
                "\x7f".to_string()
            }
        }
        TerminalKey::Tab if modifiers.shift => "\x1b[Z".to_string(),
        TerminalKey::Tab => "\t".to_string(),
        TerminalKey::Escape => "\x1b".to_string(),
        TerminalKey::Up | TerminalKey::Down | TerminalKey::Left | TerminalKey::Right => {
            let suffix = match key {
                TerminalKey::Up => 'A',
                TerminalKey::Down => 'B',
                TerminalKey::Right => 'C',
                TerminalKey::Left => 'D',
                _ => unreachable!(),
            };
            if modified {
                format!("\x1b[1;{modifier}{suffix}")
            } else if modes.application_cursor {
                format!("\x1bO{suffix}")
            } else {
                format!("\x1b[{suffix}")
            }
        }
        TerminalKey::Home if modified => format!("\x1b[1;{modifier}H"),
        TerminalKey::End if modified => format!("\x1b[1;{modifier}F"),
        TerminalKey::Home => "\x1b[H".to_string(),
        TerminalKey::End => "\x1b[F".to_string(),
        TerminalKey::Insert => csi_tilde(2, modifier),
        TerminalKey::Delete => csi_tilde(3, modifier),
        TerminalKey::PageUp => csi_tilde(5, modifier),
        TerminalKey::PageDown => csi_tilde(6, modifier),
        TerminalKey::Function(number) => function_key(number, modifier)?,
        TerminalKey::Text(_) => unreachable!(),
    };
    Some(sequence.into_bytes())
}

/// `DISAMBIGUATE_ESC_CODES`: report a chord as `CSI u` instead of its legacy
/// bytes.
const KITTY_DISAMBIGUATE: u8 = 1;
/// `REPORT_ALTERNATE_KEYS`: also report the shifted key and the base layout key.
const KITTY_ALTERNATE_KEYS: u8 = 4;
/// `REPORT_ALL_KEYS_AS_ESC`: report even plain text keys as escapes.
const KITTY_ALL_KEYS_AS_ESC: u8 = 8;

/// Encodes a chord under the kitty keyboard protocol.
///
/// Returns `None` for a key the protocol does not override, which keeps the
/// caller on its legacy path. Plain text keys stay text unless the program asked
/// for every key as an escape: turning `a` into `CSI 97 u` for a program that
/// only wanted `Shift+Enter` disambiguated would break ordinary typing.
fn encode_kitty_key(key: TerminalKey, modifiers: TerminalModifiers, flags: u8) -> Option<Vec<u8>> {
    let modifier = 1
        + u32::from(modifiers.shift)
        + u32::from(modifiers.alt) * 2
        + u32::from(modifiers.control) * 4;
    let modified = modifier != 1;
    let (code, shifted_code) = match key {
        TerminalKey::Enter => (13_u32, None),
        TerminalKey::Tab => (9, None),
        TerminalKey::Backspace => (127, None),
        TerminalKey::Escape => (27, None),
        TerminalKey::Up => (57352, None),
        TerminalKey::Down => (57353, None),
        TerminalKey::Left => (57351, None),
        TerminalKey::Right => (57350, None),
        TerminalKey::Home => (57356, None),
        TerminalKey::End => (57357, None),
        TerminalKey::PageUp => (57358, None),
        TerminalKey::PageDown => (57359, None),
        TerminalKey::Insert => (57348, None),
        TerminalKey::Delete => (57349, None),
        TerminalKey::Function(number) => {
            // F1..F12 are the contiguous range 57364..=57375.
            if !(1..=12).contains(&number) {
                return None;
            }
            (57363 + u32::from(number), None)
        }
        TerminalKey::Text(character) => {
            if flags & KITTY_ALL_KEYS_AS_ESC == 0 {
                return None;
            }
            // The protocol reports the base-layout key plus, when the program
            // asked for alternates, the shifted form it produced.
            let shifted = (modified && flags & KITTY_ALTERNATE_KEYS != 0)
                .then(|| character.to_uppercase().next())
                .flatten()
                .filter(|upper| *upper != character)
                .map(|upper| upper as u32);
            (character as u32, shifted)
        }
    };
    let mut sequence = format!("\x1b[{code}");
    if modified || shifted_code.is_some() {
        sequence.push(';');
        sequence.push_str(&modifier.to_string());
        if let Some(shifted) = shifted_code {
            sequence.push(':');
            sequence.push_str(&shifted.to_string());
        }
    }
    sequence.push('u');
    Some(sequence.into_bytes())
}

fn csi_tilde(code: u8, modifier: usize) -> String {
    if modifier == 1 {
        format!("\x1b[{code}~")
    } else {
        format!("\x1b[{code};{modifier}~")
    }
}

fn function_key(number: u8, modifier: usize) -> Option<String> {
    let suffix = match number {
        1 => "P",
        2 => "Q",
        3 => "R",
        4 => "S",
        5 => return Some(csi_tilde(15, modifier)),
        6 => return Some(csi_tilde(17, modifier)),
        7 => return Some(csi_tilde(18, modifier)),
        8 => return Some(csi_tilde(19, modifier)),
        9 => return Some(csi_tilde(20, modifier)),
        10 => return Some(csi_tilde(21, modifier)),
        11 => return Some(csi_tilde(23, modifier)),
        12 => return Some(csi_tilde(24, modifier)),
        _ => return None,
    };
    Some(if modifier == 1 {
        format!("\x1bO{suffix}")
    } else {
        format!("\x1b[1;{modifier}{suffix}")
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalMouseKind {
    Press,
    Release,
    Move,
    WheelUp,
    WheelDown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TerminalMouseEvent {
    pub kind: TerminalMouseKind,
    pub button: u8,
    pub row: u16,
    pub column: u16,
    pub modifiers: TerminalModifiers,
}

/// Which coordinate space a report uses.
#[derive(Clone, Copy)]
enum MouseCoordinates {
    /// Cell coordinates, the original protocol.
    Cells,
    /// Device-independent pixels, for `DECSET 1016`.
    Pixels { cell_width: f32, cell_height: f32 },
}

fn encode_mouse(
    event: TerminalMouseEvent,
    modes: vibex_terminal::TerminalModeSnapshot,
    coordinates: MouseCoordinates,
) -> Option<Vec<u8>> {
    if !modes.mouse_reporting {
        return None;
    }
    let mut code = match event.kind {
        TerminalMouseKind::Press => event.button.min(2),
        TerminalMouseKind::Release => 3,
        TerminalMouseKind::Move => event.button.min(2) + 32,
        TerminalMouseKind::WheelUp => 64,
        TerminalMouseKind::WheelDown => 65,
    };
    code += u8::from(event.modifiers.shift) * 4;
    code += u8::from(event.modifiers.alt) * 8;
    code += u8::from(event.modifiers.control) * 16;

    // 1016 is an extension of 1006: when a program enables both, the pixel
    // coordinates are what it is asking for. The syntax is SGR either way.
    if modes.sgr_pixels_mouse {
        let (x, y) = match coordinates {
            MouseCoordinates::Pixels {
                cell_width,
                cell_height,
            } if cell_width.is_finite()
                && cell_height.is_finite()
                && cell_width > 0.0
                && cell_height > 0.0 =>
            {
                (
                    ((f32::from(event.column) + 0.5) * cell_width).max(0.0) as u32 + 1,
                    ((f32::from(event.row) + 0.5) * cell_height).max(0.0) as u32 + 1,
                )
            }
            // Without a cell size the report degrades to cell coordinates in
            // the same syntax rather than inventing pixels.
            _ => (u32::from(event.column) + 1, u32::from(event.row) + 1),
        };
        let final_byte = if event.kind == TerminalMouseKind::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{x};{y}{final_byte}").into_bytes());
    }

    let column = u32::from(event.column) + 1;
    let row = u32::from(event.row) + 1;
    if modes.sgr_mouse {
        let final_byte = if event.kind == TerminalMouseKind::Release {
            'm'
        } else {
            'M'
        };
        return Some(format!("\x1b[<{code};{column};{row}{final_byte}").into_bytes());
    }

    // 1015 carries the same button code and cell coordinates as the original
    // protocol, but as a decimal CSI instead of three bytes.
    if modes.urxvt_mouse {
        return Some(format!("\x1b[{code};{column};{row}M").into_bytes());
    }

    // 1005 widens the printable range by encoding each value as UTF-8. The
    // three-byte X10 form tops out around column 223; this is how a wide
    // terminal reports a cell past it.
    if modes.utf8_mouse {
        let mut output = Vec::with_capacity(12);
        output.extend_from_slice(b"\x1b[M");
        for value in [u32::from(code), column, row] {
            push_utf8_mouse_value(&mut output, value + 32);
        }
        return Some(output);
    }

    if column <= 223 && row <= 223 {
        Some(vec![
            0x1b,
            b'[',
            b'M',
            code.saturating_add(32),
            column as u8 + 32,
            row as u8 + 32,
        ])
    } else {
        None
    }
}

/// Appends `value` as the UTF-8 code point xterm's 1005 mode expects.
fn push_utf8_mouse_value(output: &mut Vec<u8>, value: u32) {
    let character = char::from_u32(value).unwrap_or('\u{fffd}');
    let mut buffer = [0_u8; 4];
    output.extend_from_slice(character.encode_utf8(&mut buffer).as_bytes());
}

#[derive(Debug, Clone, Copy, PartialEq)]
pub struct TerminalCellMetrics {
    pub cell_width: f32,
    pub cell_height: f32,
    pub horizontal_padding: f32,
    pub vertical_padding: f32,
}

impl TerminalCellMetrics {
    pub fn dimensions(self, width: f32, height: f32) -> VibexResult<(u16, u16)> {
        if !self.cell_width.is_finite()
            || !self.cell_height.is_finite()
            || self.cell_width <= 0.0
            || self.cell_height <= 0.0
        {
            return Err(VibexError::validation(
                "terminal_cell_metrics_invalid",
                "terminal cell metrics must be finite and positive",
            ));
        }
        let usable_width = (width - self.horizontal_padding * 2.0).max(self.cell_width);
        let usable_height = (height - self.vertical_padding * 2.0).max(self.cell_height);
        let columns = (usable_width / self.cell_width)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16;
        let rows = (usable_height / self.cell_height)
            .floor()
            .clamp(1.0, u16::MAX as f32) as u16;
        Ok((rows, columns))
    }
}

pub struct TerminalResizeCoordinator {
    metrics: TerminalCellMetrics,
    last_sent: Option<(u16, u16)>,
    pending: Option<(u16, u16)>,
    stable_observations: u8,
}

impl TerminalResizeCoordinator {
    pub fn new(metrics: TerminalCellMetrics) -> Self {
        Self {
            metrics,
            last_sent: None,
            pending: None,
            stable_observations: 0,
        }
    }

    pub fn observe(
        &mut self,
        terminal_id: TerminalId,
        width: f32,
        height: f32,
    ) -> VibexResult<Option<TerminalResizeRequest>> {
        let dimensions = self.metrics.dimensions(width, height)?;
        if self.last_sent == Some(dimensions) {
            self.pending = None;
            self.stable_observations = 0;
            return Ok(None);
        }
        if self.pending == Some(dimensions) {
            self.stable_observations = self.stable_observations.saturating_add(1);
        } else {
            self.pending = Some(dimensions);
            self.stable_observations = 1;
        }
        if self.stable_observations < 2 {
            return Ok(None);
        }
        self.last_sent = Some(dimensions);
        self.pending = None;
        self.stable_observations = 0;
        Ok(Some(TerminalResizeRequest {
            terminal_id,
            rows: dimensions.0,
            cols: dimensions.1,
        }))
    }
}

#[cfg(test)]
mod tests {
    use vibex_core::{TerminalSession, TerminalStatus, WorkspaceId};
    use vibex_terminal::TerminalRawOutputChunk;

    use super::*;

    fn snapshot(chunks: Vec<(i64, &[u8])>, next_sequence: i64) -> TerminalRawSnapshot {
        let chunks = chunks
            .into_iter()
            .map(|(sequence, data)| TerminalRawOutputChunk {
                sequence,
                data: data.to_vec(),
            })
            .collect::<Vec<_>>();
        TerminalRawSnapshot {
            session: TerminalSession {
                id: TerminalId::new(),
                workspace_id: WorkspaceId::new(),
                title: "test".to_string(),
                shell: "sh".to_string(),
                cwd: "/tmp".to_string(),
                rows: 4,
                cols: 40,
                status: TerminalStatus::Running,
                created_at_ms: 1,
                updated_at_ms: 1,
                closed_at_ms: None,
            },
            retained_bytes: chunks.iter().map(|chunk| chunk.data.len()).sum(),
            chunks,
            next_sequence,
            dropped_chunks: 0,
        }
    }

    #[test]
    fn raw_chunks_preserve_utf8_split_across_boundaries() {
        let mut snapshot = snapshot(vec![(1, &[0xe4, 0xbd]), (2, &[0xa0, 0xe5, 0xa5, 0xbd])], 3);
        let mut backend = TerminalSurfaceBackend::new(4, 40);
        backend.sync(&snapshot).unwrap();
        assert!(backend.frame().cells.iter().any(|cell| cell.text == "你"));
        assert!(backend.frame().cells.iter().any(|cell| cell.text == "好"));

        snapshot.chunks.push(TerminalRawOutputChunk {
            sequence: 3,
            data: b"!".to_vec(),
        });
        snapshot.retained_bytes += 1;
        snapshot.next_sequence = 4;
        let outcome = backend.sync(&snapshot).unwrap();
        assert_eq!(outcome.ingested_chunks, 1);
        assert!(!outcome.rebuilt);
    }

    #[test]
    fn retained_sequence_gap_rebuilds_from_the_bounded_snapshot() {
        let mut first = snapshot(vec![(1, b"old")], 2);
        let terminal_id = first.session.id.clone();
        let mut backend = TerminalSurfaceBackend::new(4, 40);
        backend.sync(&first).unwrap();

        first.chunks = vec![TerminalRawOutputChunk {
            sequence: 4,
            data: b"current".to_vec(),
        }];
        first.retained_bytes = 7;
        first.next_sequence = 5;
        first.dropped_chunks = 3;
        first.session.id = terminal_id;
        let outcome = backend.sync(&first).unwrap();
        assert!(outcome.gap_detected);
        assert!(outcome.rebuilt);
        assert!(backend.frame().cells.iter().any(|cell| cell.text == "c"));
        assert_eq!(backend.diagnostics().sequence_gap_count, 1);
    }

    #[test]
    fn input_encoding_tracks_terminal_modes() {
        let mut snapshot = snapshot(vec![(1, b"\x1b[?1h\x1b[?2004h\x1b[?1006h\x1b[?1000h")], 2);
        let mut backend = TerminalSurfaceBackend::new(4, 40);
        backend.sync(&snapshot).unwrap();
        assert_eq!(
            backend.encode_key(TerminalKey::Up, TerminalModifiers::default()),
            Some(b"\x1bOA".to_vec())
        );
        assert_eq!(backend.encode_paste("hello"), b"\x1b[200~hello\x1b[201~");
        let mouse = backend
            .encode_mouse(TerminalMouseEvent {
                kind: TerminalMouseKind::Press,
                button: 0,
                row: 1,
                column: 2,
                modifiers: TerminalModifiers::default(),
            })
            .unwrap();
        assert_eq!(mouse, b"\x1b[<0;3;2M");

        snapshot.next_sequence = 2;
    }

    /// The three dialects that widen or reshape a mouse report are encoded
    /// from the modes the scanner tracked, not from a single SGR switch.
    #[test]
    fn extended_mouse_dialects_encode_their_own_shape() {
        let press = TerminalMouseEvent {
            kind: TerminalMouseKind::Press,
            button: 0,
            row: 1,
            column: 2,
            modifiers: TerminalModifiers::default(),
        };
        // Each dialect gets a fresh backend: `snapshot` mints a new terminal id,
        // and a second sync would reset the emulator's modes instead of setting
        // another bit.
        let encode = |modes: &[u8], event: TerminalMouseEvent, pixels: bool| {
            let mut backend = TerminalSurfaceBackend::new(4, 40);
            let data = [b"\x1b[?1000h".as_slice(), modes].concat();
            backend
                .sync(&snapshot(vec![(1, &data)], 2))
                .expect("terminal sync");
            if pixels {
                backend.encode_mouse_pixels(event, 8.0, 16.0)
            } else {
                backend.encode_mouse(event)
            }
        };

        // 1005: each value is a UTF-8 code point with 32 added, which is what
        // lets a column past the 223-byte limit be reported at all.
        let wide = TerminalMouseEvent {
            column: 200,
            ..press
        };
        let bytes = encode(b"\x1b[?1005h", wide, false).expect("utf-8 report");
        assert_eq!(bytes[..3], [0x1b, b'[', b'M']);
        assert_eq!(
            bytes[3..],
            [0x20, 0xc3, 0xa9, 0x22],
            "button 32, column 201+32 as UTF-8, row 2+32"
        );

        // 1015: decimal CSI with the same button code and cell coordinates.
        assert_eq!(
            encode(b"\x1b[?1015h", press, false).unwrap(),
            b"\x1b[0;3;2M",
            "urxvt reports the cell, 1-based"
        );

        // 1016: SGR syntax with pixel coordinates. A cell is expanded to its
        // centre at the measured cell size.
        assert_eq!(
            encode(b"\x1b[?1006h\x1b[?1016h", press, true).unwrap(),
            b"\x1b[<0;21;25M"
        );
    }

    /// A title, a clipboard write or a colour query ends up in the outcome the
    /// host applies, not discarded by the emulator.
    #[test]
    fn host_addressed_requests_reach_the_sync_outcome() {
        let mut backend = TerminalSurfaceBackend::new(4, 40);
        let outcome = backend
            .sync(&snapshot(
                vec![(1, b"\x1b]2;Build\x07\x1b]52;c;aGVsbG8=\x07\x1b]10;?\x07")],
                2,
            ))
            .unwrap();
        assert_eq!(outcome.host.title.as_deref(), Some("Build"));
        assert_eq!(outcome.host.clipboard_store.as_deref(), Some("aGVsbG8="));
        assert_eq!(
            outcome.host.color_queries,
            vec![vibex_terminal::TerminalColorQuery::Foreground]
        );
        assert!(backend.sync(&snapshot(vec![], 2)).unwrap().host.is_empty());
    }

    #[test]
    fn scrollback_search_reaches_lines_the_viewport_cannot_show() {
        let mut backend = TerminalSurfaceBackend::new(3, 20);
        let mut data = b"needle first\r\n".to_vec();
        for index in 0..10 {
            data.extend(format!("line {index}\r\n").as_bytes());
        }
        backend.sync(&snapshot(vec![(1, &data)], 2)).unwrap();
        assert!(backend.find_visible("needle", false).is_empty());
        let matches = backend.search_scrollback("needle", false);
        assert_eq!(matches.len(), 1);
        backend.scroll_to_offset(backend.history_lines());
        assert!(backend.frame().display_offset > 0);
    }

    #[test]
    fn resize_requires_two_stable_observations_and_deduplicates() {
        let terminal_id = TerminalId::new();
        let mut coordinator = TerminalResizeCoordinator::new(TerminalCellMetrics {
            cell_width: 8.0,
            cell_height: 16.0,
            horizontal_padding: 8.0,
            vertical_padding: 8.0,
        });
        assert!(
            coordinator
                .observe(terminal_id.clone(), 816.0, 416.0)
                .unwrap()
                .is_none()
        );
        let request = coordinator
            .observe(terminal_id.clone(), 816.0, 416.0)
            .unwrap()
            .unwrap();
        assert_eq!((request.rows, request.cols), (25, 100));
        assert!(
            coordinator
                .observe(terminal_id, 817.0, 416.0)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn diagnostics_do_not_contain_terminal_output() {
        let snapshot = snapshot(vec![(1, b"vibex-secret-sentinel")], 2);
        let mut backend = TerminalSurfaceBackend::new(4, 40);
        backend.sync(&snapshot).unwrap();
        let json = serde_json::to_string(&backend.diagnostics()).unwrap();
        assert!(!json.contains("vibex-secret-sentinel"));
    }

    #[test]
    fn frame_cache_applies_only_damaged_rows_after_the_initial_frame() {
        let mut emulator = TerminalEmulator::new(3, 20);
        let first = emulator.frame();
        let mut cache = TerminalFrameCache::new(&first);
        emulator.advance(b"styled");
        let second = emulator.frame();
        let update = cache.apply(&second);

        assert!(!update.full_repaint);
        assert_eq!(update.changed_rows, vec![0]);
        assert!(update.changed_cells > 0);
        assert_eq!(
            cache
                .cell(TerminalGridPoint { row: 0, column: 0 })
                .unwrap()
                .text,
            "s"
        );

        cache.force_full_repaint();
        let forced = cache.apply(&emulator.frame());
        assert!(forced.full_repaint);
        assert_eq!(forced.changed_rows.len(), 3);
    }

    #[test]
    fn terminal_resource_metrics_measure_residency_not_lifetime_input() {
        let snapshot = snapshot(vec![(1, &vec![0; 32 * 1024])], 2);
        let mut backend = TerminalSurfaceBackend::new(24, 100);
        backend.sync(&snapshot).unwrap();

        let diagnostics = backend.diagnostics();
        let resources = backend.resource_metrics();
        assert_eq!(diagnostics.ingested_bytes, 32 * 1024);
        assert!(resources.resident_bytes < diagnostics.ingested_bytes as usize);
        assert!(resources.is_within_budget());
    }
    /// A program that never negotiated the keyboard protocol keeps the legacy
    /// bytes: `Shift+Enter` is a carriage return, precisely because it never
    /// asked for anything better.
    #[test]
    fn legacy_encoding_is_unchanged_without_negotiation() {
        let backend = TerminalSurfaceBackend::new(4, 20);
        assert_eq!(backend.modes().kitty_keyboard_flags, 0);
        let shift_enter = backend
            .encode_key(
                TerminalKey::Enter,
                TerminalModifiers {
                    shift: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(shift_enter, b"\r");
    }

    /// Once the program pushes the disambiguate flag, a modified key is reported
    /// as `CSI u` — the only shape that can carry "shift".
    #[test]
    fn a_negotiated_shift_enter_is_reported_as_csi_u() {
        let mut backend = TerminalSurfaceBackend::new(4, 20);
        backend.sync(&snapshot(vec![(1, b"\x1b[>1u")], 2)).unwrap();
        assert_eq!(backend.modes().kitty_keyboard_flags, 1);
        let out = backend
            .encode_key(
                TerminalKey::Enter,
                TerminalModifiers {
                    shift: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(out, b"\x1b[13;2u");
        // A plain Enter stays a carriage return: the flag disambiguates modified
        // keys, it does not rewrite the unmodified ones.
        let plain = backend
            .encode_key(TerminalKey::Enter, TerminalModifiers::default())
            .unwrap();
        assert_eq!(plain, b"\x1b[13u");
    }

    /// A program that pushes the mode and then pops it returns to legacy bytes,
    /// so a TUI that temporarily asked cannot corrupt what follows it.
    #[test]
    fn popping_the_keyboard_mode_restores_legacy_encoding() {
        let mut backend = TerminalSurfaceBackend::new(4, 20);
        backend
            .sync(&snapshot(vec![(1, b"\x1b[>1u"), (2, b"\x1b[<u")], 3))
            .unwrap();
        assert_eq!(backend.modes().kitty_keyboard_flags, 0);
        let out = backend
            .encode_key(
                TerminalKey::Enter,
                TerminalModifiers {
                    shift: true,
                    ..Default::default()
                },
            )
            .unwrap();
        assert_eq!(out, b"\r");
    }

    /// Plain text is never rewritten unless the program asked for every key as
    /// an escape.
    #[test]
    fn text_keys_stay_text_unless_all_keys_were_negotiated() {
        let mut backend = TerminalSurfaceBackend::new(4, 20);
        backend.sync(&snapshot(vec![(1, b"\x1b[>1u")], 2)).unwrap();
        let out = backend
            .encode_key(TerminalKey::Text('a'), TerminalModifiers::default())
            .unwrap();
        assert_eq!(out, b"a");
    }

    /// The replies the emulator owes the program are surfaced for the transport
    /// that owns the write side.
    #[test]
    fn a_device_query_reply_reaches_the_sync_outcome() {
        let mut backend = TerminalSurfaceBackend::new(4, 20);
        let outcome = backend.sync(&snapshot(vec![(1, b"\x1b[c")], 2)).unwrap();
        assert!(
            outcome.pty_replies.starts_with(b"\x1b[?"),
            "a device-attributes query must be answered: {:?}",
            outcome.pty_replies
        );
        // The next sync has nothing left to send.
        let second = backend.sync(&snapshot(vec![], 2)).unwrap();
        assert!(second.pty_replies.is_empty());
    }
}

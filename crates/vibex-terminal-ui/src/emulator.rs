use std::sync::{Arc, Mutex};

use alacritty_terminal::{
    event::{Event, EventListener},
    grid::{Dimensions, Scroll},
    index::{Column, Point, Side},
    selection::{Selection, SelectionType},
    term::{Config, Term, TermDamage, TermMode, cell::Flags, point_to_viewport, viewport_to_point},
    vte::ansi::{self, Color, CursorShape},
};
use serde::Serialize;
use vibex_core::{VibexError, VibexResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalGridPoint {
    pub row: u16,
    pub column: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalCellColor {
    Named { index: u16 },
    Indexed { index: u8 },
    Rgb { red: u8, green: u8, blue: u8 },
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCellSnapshot {
    pub row: u16,
    pub column: u16,
    pub text: String,
    pub foreground: TerminalCellColor,
    pub background: TerminalCellColor,
    pub bold: bool,
    pub dim: bool,
    pub italic: bool,
    pub underline: bool,
    pub inverse: bool,
    pub hidden: bool,
    pub strikeout: bool,
    pub wide: bool,
    pub wide_spacer: bool,
    pub selected: bool,
    pub hyperlink: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalCursorShape {
    Block,
    Underline,
    Beam,
    HollowBlock,
    Hidden,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCursorSnapshot {
    pub row: u16,
    pub column: u16,
    pub shape: TerminalCursorShape,
    pub blinking: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalDamageRange {
    pub row: u16,
    pub start_column: u16,
    pub end_column: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalModeSnapshot {
    pub alternate_screen: bool,
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub mouse_reporting: bool,
    pub sgr_mouse: bool,
    pub focus_reporting: bool,
    /// The kitty keyboard protocol flags the program has pushed.
    ///
    /// Zero means the program asked for nothing and legacy encoding applies.
    /// Non-zero tells the encoder it may report a chord as `CSI u` with
    /// modifiers, which is the only way an Agent can tell `Shift+Enter` from
    /// `Enter`.
    #[serde(default)]
    pub kitty_keyboard_flags: u8,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalFrameSnapshot {
    pub rows: u16,
    pub columns: u16,
    pub history_lines: usize,
    pub display_offset: usize,
    pub full_damage: bool,
    pub damage: Vec<TerminalDamageRange>,
    pub cells: Vec<TerminalCellSnapshot>,
    pub cursor: Option<TerminalCursorSnapshot>,
    pub modes: TerminalModeSnapshot,
    pub ingested_bytes: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSearchMatch {
    pub start: TerminalGridPoint,
    pub end: TerminalGridPoint,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TerminalGridSize {
    rows: u16,
    columns: u16,
}

impl Dimensions for TerminalGridSize {
    fn total_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn screen_lines(&self) -> usize {
        usize::from(self.rows)
    }

    fn columns(&self) -> usize {
        usize::from(self.columns)
    }
}

/// Collects the replies the emulator owes a program on the other end.
///
/// The terminal core answers queries — device attributes, cursor position,
/// keyboard protocol — by raising an [`Event::PtyWrite`]; the host has to send
/// that text back into the PTY. A `VoidListener` (what this emulator used) drops
/// them, so an Agent that asks "do you understand `CSI u`?" gets silence and
/// falls back to legacy encoding, and one that waits for a device-attributes
/// reply can stall outright.
///
/// The queue is a plain `Mutex<Vec<u8>>`: it is written on whatever thread feeds
/// the parser and drained by the host between reads, so it never has to be
/// async or lock-free to keep up.
#[derive(Clone, Default)]
pub struct PtyReplyQueue {
    pending: Arc<Mutex<Vec<u8>>>,
}

impl PtyReplyQueue {
    /// Takes the replies accumulated since the last drain.
    pub fn take(&self) -> Vec<u8> {
        let mut pending = match self.pending.lock() {
            Ok(pending) => pending,
            Err(poisoned) => poisoned.into_inner(),
        };
        std::mem::take(&mut *pending)
    }

    pub fn is_empty(&self) -> bool {
        self.pending
            .lock()
            .map(|pending| pending.is_empty())
            .unwrap_or(true)
    }
}

impl EventListener for PtyReplyQueue {
    fn send_event(&self, event: Event) {
        // Only replies travel back to the program. Everything else — title,
        // bell, clipboard, wakeup — belongs to a UI this crate does not own.
        if let Event::PtyWrite(text) = event {
            let mut pending = match self.pending.lock() {
                Ok(pending) => pending,
                Err(poisoned) => poisoned.into_inner(),
            };
            pending.extend_from_slice(text.as_bytes());
        }
    }
}

pub struct TerminalEmulator {
    size: TerminalGridSize,
    term: Term<PtyReplyQueue>,
    parser: ansi::Processor,
    ingested_bytes: u64,
    replies: PtyReplyQueue,
}

impl TerminalEmulator {
    pub fn new(rows: u16, columns: u16) -> Self {
        let size = normalized_size(rows, columns);
        let replies = PtyReplyQueue::default();
        // The grid can encode `CSI u`, so it may advertise the protocol.
        // Alacritty refuses to even remember a pushed keyboard mode while this
        // is false, which is why enabling it here is what lets an Agent
        // negotiate `Shift+Enter` at all.
        //
        // Advertising it does not force it on any child: a program still has to
        // push a mode, and one that never asks keeps receiving legacy sequences.
        let config = Config {
            kitty_keyboard: true,
            ..Config::default()
        };
        Self {
            size,
            term: Term::new(config, &size, replies.clone()),
            parser: ansi::Processor::new(),
            ingested_bytes: 0,
            replies,
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.parser.advance(&mut self.term, bytes);
        self.ingested_bytes = self.ingested_bytes.saturating_add(bytes.len() as u64);
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        self.size = normalized_size(rows, columns);
        self.term.resize(self.size);
    }

    pub fn rows(&self) -> u16 {
        self.size.rows
    }

    pub fn columns(&self) -> u16 {
        self.size.columns
    }

    pub fn ingested_bytes(&self) -> u64 {
        self.ingested_bytes
    }

    pub fn alternate_screen_active(&self) -> bool {
        self.term.mode().contains(TermMode::ALT_SCREEN)
    }

    pub fn modes(&self) -> TerminalModeSnapshot {
        mode_snapshot(*self.term.mode())
    }

    /// Replies the emulator owes the program, to be written back into the PTY.
    ///
    /// A host drains this after every `advance`; an empty queue is the common
    /// case.
    pub fn take_pty_replies(&self) -> Vec<u8> {
        self.replies.take()
    }

    pub fn scroll(&mut self, line_delta: i32) {
        self.term.scroll_display(Scroll::Delta(line_delta));
    }

    pub fn scroll_to_bottom(&mut self) {
        self.term.scroll_display(Scroll::Bottom);
    }

    pub fn visible_text(&self) -> String {
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let mut rows = vec![String::new(); usize::from(self.size.rows)];
        for indexed in content.display_iter {
            let Some(point) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            if indexed.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            rows[point.line].push(indexed.c);
            if let Some(combining) = indexed.zerowidth() {
                rows[point.line].extend(combining);
            }
        }
        rows.into_iter()
            .map(|row| row.trim_end().to_string())
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn select_text(
        &mut self,
        start: TerminalGridPoint,
        end: TerminalGridPoint,
    ) -> VibexResult<String> {
        self.validate_point(start)?;
        self.validate_point(end)?;
        let display_offset = self.term.grid().display_offset();
        let mut selection = Selection::new(
            SelectionType::Simple,
            viewport_point(display_offset, start),
            Side::Left,
        );
        selection.update(viewport_point(display_offset, end), Side::Right);
        self.term.selection = Some(selection);
        self.term.selection_to_string().ok_or_else(|| {
            VibexError::validation(
                "terminal_selection_empty",
                "terminal selection did not contain any text",
            )
        })
    }

    pub fn clear_selection(&mut self) {
        self.term.selection = None;
    }

    pub fn hyperlink_at(&self, point: TerminalGridPoint) -> VibexResult<Option<String>> {
        self.validate_point(point)?;
        let point = viewport_point(self.term.grid().display_offset(), point);
        Ok(self.term.grid()[point]
            .hyperlink()
            .map(|hyperlink| hyperlink.uri().to_string()))
    }

    pub fn find_visible(&self, query: &str, case_sensitive: bool) -> Vec<TerminalSearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let needle = normalize_search_text(query, case_sensitive);
        let mut rows = vec![Vec::<(u16, String)>::new(); usize::from(self.size.rows)];
        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        for indexed in content.display_iter {
            let Some(point) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            if point.line >= rows.len() || indexed.flags.contains(Flags::WIDE_CHAR_SPACER) {
                continue;
            }
            let mut text = indexed.c.to_string();
            if let Some(combining) = indexed.zerowidth() {
                text.extend(combining);
            }
            rows[point.line].push((point.column.0 as u16, text));
        }

        let mut matches = Vec::new();
        for (row, cells) in rows.into_iter().enumerate() {
            let mut text = String::new();
            let mut byte_to_column = Vec::new();
            for (column, cell) in cells {
                for _ in 0..cell.len() {
                    byte_to_column.push(column);
                }
                text.push_str(&cell);
            }
            let haystack = normalize_search_text(&text, case_sensitive);
            for (start, _) in haystack.match_indices(&needle) {
                let end_byte = start + needle.len() - 1;
                if let (Some(start_column), Some(end_column)) =
                    (byte_to_column.get(start), byte_to_column.get(end_byte))
                {
                    matches.push(TerminalSearchMatch {
                        start: TerminalGridPoint {
                            row: row as u16,
                            column: *start_column,
                        },
                        end: TerminalGridPoint {
                            row: row as u16,
                            column: *end_column,
                        },
                    });
                }
            }
        }
        matches
    }

    pub fn frame(&mut self) -> TerminalFrameSnapshot {
        let (full_damage, damage) = match self.term.damage() {
            TermDamage::Full => (
                true,
                (0..self.size.rows)
                    .map(|row| TerminalDamageRange {
                        row,
                        start_column: 0,
                        end_column: self.size.columns.saturating_sub(1),
                    })
                    .collect(),
            ),
            TermDamage::Partial(ranges) => (
                false,
                ranges
                    .filter_map(|range| {
                        (range.line < usize::from(self.size.rows)).then_some(TerminalDamageRange {
                            row: range.line as u16,
                            start_column: range.left.min(usize::from(u16::MAX)) as u16,
                            end_column: range.right.min(usize::from(u16::MAX)) as u16,
                        })
                    })
                    .collect(),
            ),
        };
        self.term.reset_damage();

        let content = self.term.renderable_content();
        let display_offset = content.display_offset;
        let mode = content.mode;
        let selection = content.selection;
        let cursor_style = self.term.cursor_style();
        let cursor = point_to_viewport(display_offset, content.cursor.point).map(|point| {
            TerminalCursorSnapshot {
                row: point.line as u16,
                column: point.column.0 as u16,
                shape: cursor_shape(content.cursor.shape),
                blinking: cursor_style.blinking,
            }
        });
        let mut cells =
            Vec::with_capacity(usize::from(self.size.rows) * usize::from(self.size.columns));
        for indexed in content.display_iter {
            let Some(point) = point_to_viewport(display_offset, indexed.point) else {
                continue;
            };
            let flags = indexed.flags;
            let mut text = indexed.c.to_string();
            if let Some(combining) = indexed.zerowidth() {
                text.extend(combining);
            }
            let (foreground, background) = if flags.contains(Flags::INVERSE) {
                (terminal_color(indexed.bg), terminal_color(indexed.fg))
            } else {
                (terminal_color(indexed.fg), terminal_color(indexed.bg))
            };
            cells.push(TerminalCellSnapshot {
                row: point.line as u16,
                column: point.column.0 as u16,
                text,
                foreground,
                background,
                bold: flags.contains(Flags::BOLD),
                dim: flags.contains(Flags::DIM),
                italic: flags.contains(Flags::ITALIC),
                underline: flags.intersects(Flags::ALL_UNDERLINES),
                inverse: flags.contains(Flags::INVERSE),
                hidden: flags.contains(Flags::HIDDEN),
                strikeout: flags.contains(Flags::STRIKEOUT),
                wide: flags.contains(Flags::WIDE_CHAR),
                wide_spacer: flags.contains(Flags::WIDE_CHAR_SPACER),
                selected: selection.is_some_and(|selection| selection.contains(indexed.point)),
                hyperlink: indexed
                    .hyperlink()
                    .map(|hyperlink| hyperlink.uri().to_string()),
            });
        }
        let screen_lines = self.term.grid().screen_lines();
        TerminalFrameSnapshot {
            rows: self.size.rows,
            columns: self.size.columns,
            history_lines: self.term.grid().total_lines().saturating_sub(screen_lines),
            display_offset,
            full_damage,
            damage,
            cells,
            cursor,
            modes: mode_snapshot(mode),
            ingested_bytes: self.ingested_bytes,
        }
    }

    fn validate_point(&self, point: TerminalGridPoint) -> VibexResult<()> {
        if point.row >= self.size.rows || point.column >= self.size.columns {
            return Err(VibexError::validation(
                "terminal_selection_out_of_bounds",
                "terminal selection point is outside the viewport",
            ));
        }
        Ok(())
    }
}

fn normalized_size(rows: u16, columns: u16) -> TerminalGridSize {
    TerminalGridSize {
        rows: rows.max(1),
        columns: columns.max(1),
    }
}

fn viewport_point(display_offset: usize, value: TerminalGridPoint) -> Point {
    viewport_to_point(
        display_offset,
        Point::new(usize::from(value.row), Column(usize::from(value.column))),
    )
}

fn terminal_color(color: Color) -> TerminalCellColor {
    match color {
        Color::Named(color) => TerminalCellColor::Named {
            index: color as u16,
        },
        Color::Indexed(index) => TerminalCellColor::Indexed { index },
        Color::Spec(color) => TerminalCellColor::Rgb {
            red: color.r,
            green: color.g,
            blue: color.b,
        },
    }
}

fn cursor_shape(shape: CursorShape) -> TerminalCursorShape {
    match shape {
        CursorShape::Block => TerminalCursorShape::Block,
        CursorShape::Underline => TerminalCursorShape::Underline,
        CursorShape::Beam => TerminalCursorShape::Beam,
        CursorShape::HollowBlock => TerminalCursorShape::HollowBlock,
        CursorShape::Hidden => TerminalCursorShape::Hidden,
    }
}

fn mode_snapshot(mode: TermMode) -> TerminalModeSnapshot {
    TerminalModeSnapshot {
        alternate_screen: mode.contains(TermMode::ALT_SCREEN),
        application_cursor: mode.contains(TermMode::APP_CURSOR),
        bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
        mouse_reporting: mode.intersects(TermMode::MOUSE_MODE),
        sgr_mouse: mode.contains(TermMode::SGR_MOUSE),
        focus_reporting: mode.contains(TermMode::FOCUS_IN_OUT),
        kitty_keyboard_flags: kitty_keyboard_flags(mode),
    }
}

/// The five protocol bits an Agent can push, packed the way `CSI u` reports them.
fn kitty_keyboard_flags(mode: TermMode) -> u8 {
    let mut flags = 0_u8;
    if mode.contains(TermMode::DISAMBIGUATE_ESC_CODES) {
        flags |= 1;
    }
    if mode.contains(TermMode::REPORT_EVENT_TYPES) {
        flags |= 2;
    }
    if mode.contains(TermMode::REPORT_ALTERNATE_KEYS) {
        flags |= 4;
    }
    if mode.contains(TermMode::REPORT_ALL_KEYS_AS_ESC) {
        flags |= 8;
    }
    if mode.contains(TermMode::REPORT_ASSOCIATED_TEXT) {
        flags |= 16;
    }
    flags
}

fn normalize_search_text(value: &str, case_sensitive: bool) -> String {
    let mut value = value.to_string();
    if !case_sensitive {
        value.make_ascii_lowercase();
    }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_cjk_and_uses_alacritty_selection_text() {
        let mut emulator = TerminalEmulator::new(3, 20);
        emulator.advance("copy-me 中文".as_bytes());

        assert!(emulator.visible_text().contains("copy-me 中文"));
        assert_eq!(
            emulator
                .select_text(
                    TerminalGridPoint { row: 0, column: 0 },
                    TerminalGridPoint { row: 0, column: 6 },
                )
                .unwrap(),
            "copy-me"
        );
    }

    #[test]
    fn preserves_primary_grid_across_alternate_screen_and_resizes() {
        let mut emulator = TerminalEmulator::new(3, 20);
        emulator.advance(b"primary");
        emulator.advance(b"\x1b[?1049h");
        assert!(emulator.alternate_screen_active());
        emulator.advance(b"alternate");
        assert!(emulator.visible_text().contains("alternate"));

        emulator.advance(b"\x1b[?1049l");
        assert!(!emulator.alternate_screen_active());
        assert!(emulator.visible_text().contains("primary"));

        emulator.resize(30, 100);
        assert_eq!((emulator.rows(), emulator.columns()), (30, 100));
    }

    /// The bug this crate now declares its way out of: an image-protocol escape
    /// writes nothing this grid can draw.
    ///
    /// A client that believes it is talking to kitty places a brand mark with
    /// `ESC _ G … ESC \` and then paints the cells it reserved blank, because
    /// the picture is supposed to stand over them. The emulator consumes the
    /// APC — correctly, it has no pixel layer — so those cells stay empty. The
    /// declaration in [`crate::capability`] is what stops a child from asking,
    /// and this test is the evidence that asking would have failed.
    #[test]
    fn an_image_protocol_escape_leaves_the_cells_it_would_have_covered_empty() {
        let mut emulator = TerminalEmulator::new(2, 10);
        // The kitty transmit-and-display sequence, as the TUI writes it for a
        // mark: park the cursor, carry a PNG under an id, then paint the two
        // cells blank so the picture can show through.
        emulator.advance(
            b"\x1b7\x1b[1;1H\x1b_Ga=T,f=100,t=d,q=2,i=100,p=100,c=2,r=1;AAAA\x1b\\\x1b8  ",
        );

        let frame = emulator.frame();
        let row = frame
            .cells
            .iter()
            .filter(|cell| cell.row == 0)
            .map(|cell| cell.text.as_str())
            .collect::<String>();
        assert!(
            row.trim().is_empty(),
            "the emulator drew the escape instead of dropping it: {row:?}"
        );
        assert_eq!(emulator.visible_text().trim(), "");
    }

    #[test]
    fn rejects_selection_outside_the_viewport() {
        let mut emulator = TerminalEmulator::new(2, 2);
        let error = emulator
            .select_text(
                TerminalGridPoint { row: 0, column: 0 },
                TerminalGridPoint { row: 2, column: 0 },
            )
            .unwrap_err();
        assert_eq!(error.code, "terminal_selection_out_of_bounds");
    }

    #[test]
    fn frame_preserves_style_color_cursor_damage_and_hyperlink() {
        let mut emulator = TerminalEmulator::new(3, 20);
        emulator
            .advance(b"\x1b[1;3;4;38;2;10;20;30mA\x1b]8;;https://example.com\x1b\\B\x1b]8;;\x1b\\");
        let frame = emulator.frame();

        assert!(frame.full_damage);
        assert_eq!(frame.cursor.unwrap().column, 2);
        let styled = frame.cells.iter().find(|cell| cell.text == "A").unwrap();
        assert!(styled.bold);
        assert!(styled.italic);
        assert!(styled.underline);
        assert_eq!(
            styled.foreground,
            TerminalCellColor::Rgb {
                red: 10,
                green: 20,
                blue: 30,
            }
        );
        let linked = frame.cells.iter().find(|cell| cell.text == "B").unwrap();
        assert_eq!(linked.hyperlink.as_deref(), Some("https://example.com"));
        assert!(!emulator.frame().full_damage);
    }

    #[test]
    fn scrollback_search_and_selection_use_viewport_coordinates() {
        let mut emulator = TerminalEmulator::new(2, 20);
        emulator.advance(b"first\r\nsecond\r\nthird");
        assert!(emulator.frame().history_lines > 0);
        emulator.scroll(1);
        assert!(emulator.visible_text().contains("second"));
        let matches = emulator.find_visible("SECOND", false);
        assert_eq!(matches.len(), 1);
        assert_eq!(matches[0].start.row, 1);
        assert_eq!(
            emulator
                .select_text(matches[0].start, matches[0].end)
                .unwrap(),
            "second"
        );
    }

    #[test]
    fn scrollback_is_bounded_to_the_product_history_limit() {
        let mut emulator = TerminalEmulator::new(4, 20);
        for index in 0..10_200 {
            emulator.advance(format!("line-{index:05}\r\n").as_bytes());
        }

        let frame = emulator.frame();
        assert_eq!(frame.history_lines, 10_000);
        assert!(frame.cells.len() <= usize::from(frame.rows) * usize::from(frame.columns));
    }
    /// The emulator answers the queries a CLI uses to decide what it may send.
    ///
    /// Without these replies an Agent that asks "which terminal are you?" or
    /// "where is the cursor?" waits for an answer that never comes, and one that
    /// asks about the keyboard protocol falls back to legacy encoding.
    #[test]
    fn the_emulator_answers_device_and_cursor_queries() {
        let mut emulator = TerminalEmulator::new(4, 20);
        emulator.advance(b"\x1b[c");
        let primary = emulator.take_pty_replies();
        assert!(
            primary.starts_with(b"\x1b[?"),
            "primary device attributes should be answered: {primary:?}"
        );
        // The queue drains, so a host does not resend the same reply forever.
        assert!(emulator.take_pty_replies().is_empty());

        emulator.advance(b"\x1b[2;3H\x1b[6n");
        let cursor = emulator.take_pty_replies();
        assert_eq!(
            cursor, b"\x1b[2;3R",
            "a cursor position report should carry the current cell"
        );
    }

    /// A program that pushes a keyboard mode is remembered, and popping it
    /// returns the emulator to legacy encoding.
    #[test]
    fn the_keyboard_protocol_is_negotiated_by_the_program() {
        let mut emulator = TerminalEmulator::new(4, 20);
        // Nothing pushed yet: the encoder must keep legacy behaviour.
        assert_eq!(emulator.modes().kitty_keyboard_flags, 0);

        // Push disambiguate + report-alternate-keys, the combination an Agent
        // uses to tell Shift+Enter from Enter.
        emulator.advance(b"\x1b[>5u");
        assert_eq!(emulator.modes().kitty_keyboard_flags, 5);

        // A query reports the active mode rather than changing it.
        emulator.advance(b"\x1b[?u");
        assert_eq!(emulator.take_pty_replies(), b"\x1b[?5u");

        // Popping every pushed mode restores legacy encoding.
        emulator.advance(b"\x1b[<u");
        assert_eq!(emulator.modes().kitty_keyboard_flags, 0);
    }

    /// Advertising the protocol does not change what a program that never asks
    /// receives.
    #[test]
    fn a_program_that_never_negotiates_keeps_legacy_modes() {
        let emulator = TerminalEmulator::new(4, 20);
        let modes = emulator.modes();
        assert_eq!(modes.kitty_keyboard_flags, 0);
        assert!(!modes.bracketed_paste);
        assert!(!modes.mouse_reporting);
    }
}

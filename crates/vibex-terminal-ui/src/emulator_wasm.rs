//! Small WASM-safe fallback emulator.
//!
//! The native build uses the full alacritty terminal core.  WASM hosts cannot
//! link alacritty's native polling/home helpers, so this bounded fallback keeps
//! the same provider-neutral frame contract and raw-byte accounting.  A future
//! web-native parser can replace this module without changing the controller.

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

#[derive(Clone)]
pub struct TerminalEmulator {
    rows: u16,
    columns: u16,
    cursor_row: u16,
    cursor_column: u16,
    cells: Vec<char>,
    ingested_bytes: u64,
    modes: TerminalModeSnapshot,
    history_lines: usize,
}

impl TerminalEmulator {
    pub fn new(rows: u16, columns: u16) -> Self {
        let rows = rows.max(1);
        let columns = columns.max(1);
        Self {
            rows,
            columns,
            cursor_row: 0,
            cursor_column: 0,
            cells: vec![' '; usize::from(rows) * usize::from(columns)],
            ingested_bytes: 0,
            modes: TerminalModeSnapshot {
                alternate_screen: false,
                application_cursor: false,
                bracketed_paste: false,
                mouse_reporting: false,
                sgr_mouse: false,
                focus_reporting: false,
            },
            history_lines: 0,
        }
    }

    pub fn advance(&mut self, bytes: &[u8]) {
        self.ingested_bytes = self.ingested_bytes.saturating_add(bytes.len() as u64);
        let mut index = 0;
        while index < bytes.len() {
            match bytes[index] {
                b'\x1b' if bytes.get(index + 1) == Some(&b'[') => {
                    if let Some(end) = bytes[index + 2..]
                        .iter()
                        .position(|byte| matches!(*byte, b'@'..=b'~'))
                    {
                        let final_byte = bytes[index + 2 + end];
                        let sequence = &bytes[index + 2..index + 2 + end];
                        self.apply_csi(sequence, final_byte);
                        index += end + 3;
                        continue;
                    }
                }
                b'\r' => self.cursor_column = 0,
                b'\n' => self.newline(),
                b'\x08' => self.cursor_column = self.cursor_column.saturating_sub(1),
                b'\t' => self.cursor_column = (self.cursor_column + 8).min(self.columns - 1),
                byte if byte.is_ascii() && !byte.is_ascii_control() => self.put_char(byte as char),
                byte if byte >= 0x80 => self.put_char('�'),
                _ => {}
            }
            index += 1;
        }
    }

    fn apply_csi(&mut self, sequence: &[u8], final_byte: u8) {
        let private = sequence.first() == Some(&b'?');
        let numbers = sequence
            .iter()
            .filter(|byte| byte.is_ascii_digit() || **byte == b';')
            .copied()
            .collect::<Vec<_>>();
        let first = std::str::from_utf8(&numbers)
            .ok()
            .and_then(|value| value.split(';').next())
            .and_then(|value| value.parse::<u16>().ok())
            .unwrap_or(0);
        match (private, first, final_byte) {
            (true, 1049, b'h') => self.modes.alternate_screen = true,
            (true, 1049, b'l') => self.modes.alternate_screen = false,
            (true, 1, b'h') => self.modes.application_cursor = true,
            (true, 1, b'l') => self.modes.application_cursor = false,
            (true, 2004, b'h') => self.modes.bracketed_paste = true,
            (true, 2004, b'l') => self.modes.bracketed_paste = false,
            _ => {}
        }
    }

    fn put_char(&mut self, character: char) {
        let index = usize::from(self.cursor_row) * usize::from(self.columns)
            + usize::from(self.cursor_column);
        if let Some(cell) = self.cells.get_mut(index) {
            *cell = character;
        }
        if self.cursor_column + 1 >= self.columns {
            self.newline();
        } else {
            self.cursor_column += 1;
        }
    }

    fn newline(&mut self) {
        self.cursor_column = 0;
        if self.cursor_row + 1 >= self.rows {
            self.history_lines = self.history_lines.saturating_add(1);
        } else {
            self.cursor_row += 1;
        }
    }

    pub fn resize(&mut self, rows: u16, columns: u16) {
        let mut replacement = Self::new(rows, columns);
        replacement.ingested_bytes = self.ingested_bytes;
        replacement.modes = self.modes;
        replacement.history_lines = self.history_lines;
        self.rows = replacement.rows;
        self.columns = replacement.columns;
        self.cursor_row = replacement.cursor_row;
        self.cursor_column = replacement.cursor_column;
        self.cells = replacement.cells;
        self.modes = replacement.modes;
        self.history_lines = replacement.history_lines;
    }

    pub fn rows(&self) -> u16 {
        self.rows
    }
    pub fn columns(&self) -> u16 {
        self.columns
    }
    pub fn ingested_bytes(&self) -> u64 {
        self.ingested_bytes
    }
    pub fn alternate_screen_active(&self) -> bool {
        self.modes.alternate_screen
    }
    pub fn modes(&self) -> TerminalModeSnapshot {
        self.modes
    }
    pub fn scroll(&mut self, _line_delta: i32) {}
    pub fn scroll_to_bottom(&mut self) {}

    pub fn visible_text(&self) -> String {
        (0..self.rows)
            .map(|row| {
                let start = usize::from(row) * usize::from(self.columns);
                self.cells[start..start + usize::from(self.columns)]
                    .iter()
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
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
        if start.row != end.row {
            return Ok(self.visible_text());
        }
        let row_start = usize::from(start.row) * usize::from(self.columns);
        let (left, right) = if start.column <= end.column {
            (start.column, end.column)
        } else {
            (end.column, start.column)
        };
        Ok(
            self.cells[row_start + usize::from(left)..=row_start + usize::from(right)]
                .iter()
                .collect::<String>()
                .trim_end()
                .to_string(),
        )
    }

    pub fn clear_selection(&mut self) {}

    pub fn hyperlink_at(&self, point: TerminalGridPoint) -> VibexResult<Option<String>> {
        self.validate_point(point)?;
        Ok(None)
    }

    pub fn find_visible(&self, query: &str, case_sensitive: bool) -> Vec<TerminalSearchMatch> {
        if query.is_empty() {
            return Vec::new();
        }
        let text = if case_sensitive {
            self.visible_text()
        } else {
            self.visible_text().to_ascii_lowercase()
        };
        let needle = if case_sensitive {
            query.to_string()
        } else {
            query.to_ascii_lowercase()
        };
        text.match_indices(&needle)
            .map(|(offset, _)| TerminalSearchMatch {
                start: TerminalGridPoint {
                    row: 0,
                    column: offset as u16,
                },
                end: TerminalGridPoint {
                    row: 0,
                    column: offset.saturating_add(needle.len().saturating_sub(1)) as u16,
                },
            })
            .collect()
    }

    pub fn frame(&mut self) -> TerminalFrameSnapshot {
        let cells = self
            .cells
            .iter()
            .enumerate()
            .map(|(index, character)| TerminalCellSnapshot {
                row: (index / usize::from(self.columns)) as u16,
                column: (index % usize::from(self.columns)) as u16,
                text: character.to_string(),
                foreground: TerminalCellColor::Named { index: 7 },
                background: TerminalCellColor::Named { index: 0 },
                bold: false,
                dim: false,
                italic: false,
                underline: false,
                inverse: false,
                hidden: false,
                strikeout: false,
                wide: false,
                wide_spacer: false,
                selected: false,
                hyperlink: None,
            })
            .collect();
        TerminalFrameSnapshot {
            rows: self.rows,
            columns: self.columns,
            history_lines: self.history_lines,
            display_offset: 0,
            full_damage: true,
            damage: (0..self.rows)
                .map(|row| TerminalDamageRange {
                    row,
                    start_column: 0,
                    end_column: self.columns.saturating_sub(1),
                })
                .collect(),
            cells,
            cursor: Some(TerminalCursorSnapshot {
                row: self.cursor_row,
                column: self.cursor_column,
                shape: TerminalCursorShape::Block,
                blinking: true,
            }),
            modes: self.modes,
            ingested_bytes: self.ingested_bytes,
        }
    }

    fn validate_point(&self, point: TerminalGridPoint) -> VibexResult<()> {
        if point.row >= self.rows || point.column >= self.columns {
            return Err(VibexError::validation(
                "terminal_selection_out_of_bounds",
                "terminal selection point is outside the viewport",
            ));
        }
        Ok(())
    }
}

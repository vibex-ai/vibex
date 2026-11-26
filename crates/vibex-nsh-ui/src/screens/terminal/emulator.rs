//! `alacritty_terminal`-backed VT/xterm emulator for the mobile terminal grid.
//!
//! Uses `alacritty_terminal` directly so prompts (Starship, Powerlevel10k,
//! tmux, vim, Claude Code, etc.), UTF-8 sequences split across frames,
//! truecolor / 256-color SGR, alternate screen buffers, DSR/OSC queries, and
//! reflow on resize behave identically to a full desktop terminal.

use std::sync::{Arc, Mutex};

use alacritty_terminal::{
    Term,
    event::{Event, EventListener},
    grid::Dimensions,
    index::{Column, Line},
    term::{
        Config, TermMode,
        cell::{Cell, Flags},
    },
    vte::ansi::{Color, NamedColor, Processor, Rgb},
};

/// Default scrollback history kept above the active viewport.
pub const DEFAULT_SCROLLBACK: usize = 2000;

/// Events emitted by the emulator while advancing raw PTY bytes.
pub enum EmulatorEvent {
    Title(String),
    ResetTitle,
    PtyWrite(String),
    ColorRequest(usize, Arc<dyn Fn(Rgb) -> String + Send + Sync + 'static>),
    Bell,
}

#[derive(Clone, Default)]
struct Proxy {
    events: Arc<Mutex<Vec<EmulatorEvent>>>,
}

impl EventListener for Proxy {
    fn send_event(&self, event: Event) {
        let mapped = match event {
            Event::Title(title) => EmulatorEvent::Title(title),
            Event::ResetTitle => EmulatorEvent::ResetTitle,
            Event::PtyWrite(data) => EmulatorEvent::PtyWrite(data),
            Event::ColorRequest(index, formatter) => EmulatorEvent::ColorRequest(index, formatter),
            Event::Bell => EmulatorEvent::Bell,
            _ => return,
        };
        if let Ok(mut queue) = self.events.lock() {
            queue.push(mapped);
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct GridSize {
    cols: usize,
    rows: usize,
}

impl Dimensions for GridSize {
    fn total_lines(&self) -> usize {
        self.rows
    }

    fn screen_lines(&self) -> usize {
        self.rows
    }

    fn columns(&self) -> usize {
        self.cols
    }
}

/// Terminal modes that affect input encoding.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct EmulatorModes {
    pub application_cursor: bool,
    pub bracketed_paste: bool,
    pub alt_screen: bool,
}

/// Color stored on a rendered cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CellColor {
    DefaultFg,
    DefaultBg,
    Ansi(u8),
    Indexed(u8),
    Rgb(u8, u8, u8),
}

/// Visual attributes of a rendered cell.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct CellStyle {
    pub fg: CellColor,
    pub bg: CellColor,
    pub bold: bool,
    pub italic: bool,
    pub underline: bool,
    pub strikethrough: bool,
    pub dim: bool,
    pub hidden: bool,
    pub inverse: bool,
}

impl Default for CellStyle {
    fn default() -> Self {
        Self {
            fg: CellColor::DefaultFg,
            bg: CellColor::DefaultBg,
            bold: false,
            italic: false,
            underline: false,
            strikethrough: false,
            dim: false,
            hidden: false,
            inverse: false,
        }
    }
}

/// One cell in a rendered row, carrying its exact grid column and cell span.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RowCell {
    pub col: usize,
    pub width: usize,
    pub text: String,
    pub style: CellStyle,
}

/// Merged background rectangle covering `[col, col + cols)` on a row.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct BgSpan {
    pub col: usize,
    pub cols: usize,
    pub color: CellColor,
    pub inverse: bool,
    pub fg_for_inverse: CellColor,
}

/// Snapshot of a single logical line (scrollback or visible screen).
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TerminalRow {
    pub backgrounds: Vec<BgSpan>,
    pub cells: Vec<RowCell>,
    /// Plain text of the row with trailing whitespace trimmed (for find/search).
    pub plain: String,
}

/// The terminal state machine wrapping `alacritty_terminal::Term`.
pub struct Emulator {
    term: Term<Proxy>,
    processor: Processor,
    proxy: Proxy,
    cols: u16,
    rows: u16,
}

impl Emulator {
    pub fn new(cols: u16, rows: u16, scrollback: usize) -> Self {
        let cols = cols.max(2);
        let rows = rows.max(2);
        let proxy = Proxy::default();
        let config = Config {
            scrolling_history: scrollback,
            ..Config::default()
        };
        let size = GridSize {
            cols: usize::from(cols),
            rows: usize::from(rows),
        };
        Self {
            term: Term::new(config, &size, proxy.clone()),
            processor: Processor::new(),
            proxy,
            cols,
            rows,
        }
    }

    /// Feeds raw PTY bytes into the VTE parser and returns any emitted events.
    pub fn write(&mut self, bytes: &[u8]) -> Vec<EmulatorEvent> {
        if !bytes.is_empty() {
            self.processor.advance(&mut self.term, bytes);
        }
        self.drain_events()
    }

    /// Resizes the terminal grid, reflowing wrapped lines.
    ///
    /// Ignores degenerate sizes (`< 2` columns or rows) so hiding a view or
    /// collapsing a tab panel never truncates or destroys screen contents.
    pub fn resize(&mut self, cols: u16, rows: u16) -> bool {
        if cols < 2 || rows < 2 || (self.cols == cols && self.rows == rows) {
            return false;
        }
        self.cols = cols;
        self.rows = rows;
        self.term.resize(GridSize {
            cols: usize::from(cols),
            rows: usize::from(rows),
        });
        true
    }

    /// Performs a full terminal reset (RIS) and clears scrollback history.
    pub fn reset(&mut self) {
        self.processor.advance(&mut self.term, b"\x1bc");
        self.term.grid_mut().clear_history();
        let _ = self.drain_events();
    }

    #[cfg(test)]
    pub fn size(&self) -> (u16, u16) {
        (self.cols, self.rows)
    }

    /// Total number of logical rows (scrollback + active viewport).
    pub fn line_count(&self) -> usize {
        let grid = self.term.grid();
        grid.history_size() + grid.screen_lines()
    }

    /// Active input/screen modes.
    pub fn modes(&self) -> EmulatorModes {
        let mode = *self.term.mode();
        EmulatorModes {
            application_cursor: mode.contains(TermMode::APP_CURSOR),
            bracketed_paste: mode.contains(TermMode::BRACKETED_PASTE),
            alt_screen: mode.contains(TermMode::ALT_SCREEN),
        }
    }

    /// Cursor position as `(logical_line_index, column)` even when hidden
    /// (used for positioning IME composition previews).
    pub fn cursor_position(&self) -> (usize, usize) {
        let grid = self.term.grid();
        let history = grid.history_size();
        let point = grid.cursor.point;
        let row = (history as i32 + point.line.0).max(0) as usize;
        (row, point.column.0)
    }

    /// Visible cursor position as `(logical_line_index, column)`, or `None`
    /// when the program hid the cursor (`DECTCEM`).
    pub fn cursor(&self) -> Option<(usize, usize)> {
        if !self.term.mode().contains(TermMode::SHOW_CURSOR) {
            return None;
        }
        Some(self.cursor_position())
    }

    /// Extracts a snapshot of logical line `index` (`0` = oldest scrollback).
    pub fn row(&self, index: usize) -> TerminalRow {
        let grid = self.term.grid();
        let history = grid.history_size();
        let total = history + grid.screen_lines();
        if index >= total {
            return TerminalRow::default();
        }
        let line = Line(index as i32 - history as i32);
        let row = &grid[line];
        let cols = grid.columns();

        let mut backgrounds: Vec<BgSpan> = Vec::new();
        let mut raw_cells: Vec<RowCell> = Vec::with_capacity(cols);
        let mut last_visible_cell = 0usize;

        for col in 0..cols {
            let cell = &row[Column(col)];
            let flags = cell.flags;
            if flags.contains(Flags::WIDE_CHAR_SPACER)
                || flags.contains(Flags::LEADING_WIDE_CHAR_SPACER)
            {
                continue;
            }
            let width = if flags.contains(Flags::WIDE_CHAR) {
                2
            } else {
                1
            };
            let style = cell_style(cell);

            // Background boxes cover every cell whose effective background
            // differs from the terminal's default surface (including inverse
            // spaces, colored status bars, and Starship/Powerlevel10k pills).
            let has_bg = style.inverse || !matches!(style.bg, CellColor::DefaultBg);
            if has_bg {
                if let Some(last) = backgrounds.last_mut()
                    && last.col + last.cols == col
                    && last.color == style.bg
                    && last.inverse == style.inverse
                    && last.fg_for_inverse == style.fg
                {
                    last.cols += width;
                } else {
                    backgrounds.push(BgSpan {
                        col,
                        cols: width,
                        color: style.bg,
                        inverse: style.inverse,
                        fg_for_inverse: style.fg,
                    });
                }
            }

            let mut text = String::with_capacity(4);
            text.push(cell.c);
            if let Some(zerowidth) = cell.zerowidth() {
                for &extra in zerowidth {
                    text.push(extra);
                }
            }

            let is_blank = cell.c == ' '
                && cell.zerowidth().is_none()
                && !style.underline
                && !style.strikethrough;
            raw_cells.push(RowCell {
                col,
                width,
                text,
                style,
            });
            if !is_blank {
                last_visible_cell = raw_cells.len();
            }
        }

        raw_cells.truncate(last_visible_cell);
        let mut plain = String::with_capacity(raw_cells.len());
        for cell in &raw_cells {
            plain.push_str(&cell.text);
        }
        let trimmed_len = plain.trim_end().len();
        plain.truncate(trimmed_len);

        TerminalRow {
            backgrounds,
            cells: raw_cells,
            plain,
        }
    }

    /// Plain text across scrollback and viewport, one line per row.
    pub fn plain_text(&self) -> String {
        let count = self.line_count();
        let mut lines = Vec::with_capacity(count);
        for idx in 0..count {
            lines.push(self.row(idx).plain);
        }
        lines.join("\n")
    }

    fn drain_events(&self) -> Vec<EmulatorEvent> {
        self.proxy
            .events
            .lock()
            .map(|mut queue| std::mem::take(&mut *queue))
            .unwrap_or_default()
    }
}

fn cell_style(cell: &Cell) -> CellStyle {
    let flags = cell.flags;
    let bold = flags.contains(Flags::BOLD);
    let dim = flags.contains(Flags::DIM);
    CellStyle {
        fg: map_color(cell.fg, true, bold),
        bg: map_color(cell.bg, false, false),
        bold,
        italic: flags.contains(Flags::ITALIC),
        underline: flags.intersects(Flags::ALL_UNDERLINES),
        strikethrough: flags.contains(Flags::STRIKEOUT),
        dim,
        hidden: flags.contains(Flags::HIDDEN),
        inverse: flags.contains(Flags::INVERSE),
    }
}

fn map_color(color: Color, is_fg: bool, bold: bool) -> CellColor {
    match color {
        Color::Spec(Rgb { r, g, b }) => CellColor::Rgb(r, g, b),
        Color::Indexed(n) if n < 16 => CellColor::Ansi(n),
        Color::Indexed(n) => CellColor::Indexed(n),
        Color::Named(named) => match named {
            NamedColor::Black => CellColor::Ansi(if bold && is_fg { 8 } else { 0 }),
            NamedColor::Red => CellColor::Ansi(if bold && is_fg { 9 } else { 1 }),
            NamedColor::Green => CellColor::Ansi(if bold && is_fg { 10 } else { 2 }),
            NamedColor::Yellow => CellColor::Ansi(if bold && is_fg { 11 } else { 3 }),
            NamedColor::Blue => CellColor::Ansi(if bold && is_fg { 12 } else { 4 }),
            NamedColor::Magenta => CellColor::Ansi(if bold && is_fg { 13 } else { 5 }),
            NamedColor::Cyan => CellColor::Ansi(if bold && is_fg { 14 } else { 6 }),
            NamedColor::White => CellColor::Ansi(if bold && is_fg { 15 } else { 7 }),
            NamedColor::BrightBlack => CellColor::Ansi(8),
            NamedColor::BrightRed => CellColor::Ansi(9),
            NamedColor::BrightGreen => CellColor::Ansi(10),
            NamedColor::BrightYellow => CellColor::Ansi(11),
            NamedColor::BrightBlue => CellColor::Ansi(12),
            NamedColor::BrightMagenta => CellColor::Ansi(13),
            NamedColor::BrightCyan => CellColor::Ansi(14),
            NamedColor::BrightWhite => CellColor::Ansi(15),
            NamedColor::DimBlack => CellColor::Ansi(0),
            NamedColor::DimRed => CellColor::Ansi(1),
            NamedColor::DimGreen => CellColor::Ansi(2),
            NamedColor::DimYellow => CellColor::Ansi(3),
            NamedColor::DimBlue => CellColor::Ansi(4),
            NamedColor::DimMagenta => CellColor::Ansi(5),
            NamedColor::DimCyan => CellColor::Ansi(6),
            NamedColor::DimWhite => CellColor::Ansi(7),
            NamedColor::Foreground
            | NamedColor::BrightForeground
            | NamedColor::DimForeground
            | NamedColor::Cursor => {
                if is_fg {
                    CellColor::DefaultFg
                } else {
                    CellColor::DefaultBg
                }
            }
            NamedColor::Background => CellColor::DefaultBg,
        },
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn split_utf8_bytes_render_intact() {
        let mut emu = Emulator::new(40, 10, 100);
        for &byte in "中文终端 🚀".as_bytes() {
            emu.write(&[byte]);
        }
        let text = emu.plain_text();
        assert!(text.contains("中文终端"), "got: {text:?}");
    }

    #[test]
    fn degenerate_resize_is_ignored_and_preserves_screen() {
        let mut emu = Emulator::new(48, 12, 100);
        emu.write(b"peatboy via main at ~/vibex\r\n");
        let before = emu.plain_text();
        assert!(!emu.resize(0, 0));
        assert!(!emu.resize(1, 1));
        assert!(!emu.resize(1, 20));
        assert!(!emu.resize(20, 1));
        assert_eq!(emu.size(), (48, 12));
        assert_eq!(emu.plain_text(), before);
    }

    #[test]
    fn colored_backgrounds_and_cursor_positions_stay_aligned() {
        let mut emu = Emulator::new(40, 6, 100);
        emu.write(b"\x1b[42;30m OK \x1b[0m next");
        let row = emu.row(0);
        assert_eq!(row.plain, " OK  next");
        assert_eq!(row.backgrounds.len(), 1);
        assert_eq!(row.backgrounds[0].col, 0);
        assert_eq!(row.backgrounds[0].cols, 4);
        assert_eq!(emu.cursor(), Some((0, 9)));
    }

    #[test]
    fn dsr_query_emits_pty_write_response() {
        let mut emu = Emulator::new(40, 10, 100);
        emu.write(b"abc");
        let events = emu.write(b"\x1b[6n");
        assert!(
            events
                .iter()
                .any(|e| matches!(e, EmulatorEvent::PtyWrite(s) if s == "\x1b[1;4R")),
            "DSR should report 1-based row/col cursor position"
        );
    }
}

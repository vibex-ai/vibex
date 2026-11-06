//! Control sequences a terminal host has to act on itself.
//!
//! The emulator core owns the grid, but a few escape sequences are requests
//! *to the host*: set the window title, put text on the clipboard, ask which
//! colour the terminal uses, or switch the mouse-reporting dialect. Alacritty
//! parses most of these and raises an [`alacritty_terminal::event::Event`], but
//! the host that would answer lives outside the parser, and the alternative —
//! routing a closure-bearing event across a process boundary — cannot survive
//! serialization.
//!
//! This scanner therefore watches the same raw bytes as the parser and records
//! only the requests as plain data. It never consumes or rewrites anything: the
//! emulator still sees every byte, so parsing is unchanged and an unknown
//! sequence simply falls through to it.
//!
//! The DEC private modes 1005, 1015 and 1016 get the same treatment. Alacritty
//! knows 1005 (`UTF8_MOUSE`), ignores 1015 and 1016, and the host has to know
//! all three to encode a mouse report the program can read.

use serde::Serialize;

/// Largest OSC payload the scanner keeps.
///
/// A clipboard store can carry an image; the host only wants text, so a payload
/// past this bound is dropped rather than buffered until the process grows.
const MAX_OSC_BYTES: usize = 1 << 20;
/// Largest CSI parameter string the scanner keeps. Mouse modes are short;
/// anything longer is some other sequence and is not this crate's business.
const MAX_CSI_BYTES: usize = 256;
/// Largest title the scanner forwards. A title is painted in a tab, not stored.
const MAX_TITLE_BYTES: usize = 1024;

/// The mouse-reporting dialects that extend the original X10 encoding.
///
/// Alacritty models 1005 as [`alacritty_terminal::term::TermMode::UTF8_MOUSE`]
/// and does not model 1015/1016 at all, so the emulator merges this scanner's
/// view into the mode snapshot it publishes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalMouseModes {
    /// `DECSET 1005`: X10-shaped reports whose values are UTF-8 code points.
    pub utf8: bool,
    /// `DECSET 1015`: `CSI Cb ; Cx ; Cy M` with decimal coordinates.
    pub urxvt: bool,
    /// `DECSET 1016`: SGR reports whose coordinates are pixels, not cells.
    pub sgr_pixels: bool,
}

impl TerminalMouseModes {
    /// Whether every dialect bit is clear, so the plain X10 encoding applies.
    pub fn is_legacy(&self) -> bool {
        !self.utf8 && !self.urxvt && !self.sgr_pixels
    }
}

/// A colour the program asked the terminal to report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TerminalColorQuery {
    /// `OSC 4 ; index ; ?` — one palette slot.
    Palette { index: u8 },
    /// `OSC 10 ; ?` — the default foreground.
    Foreground,
    /// `OSC 11 ; ?` — the default background.
    Background,
    /// `OSC 12 ; ?` — the cursor colour.
    Cursor,
}

/// Everything the host owes the program after one batch of output.
///
/// The values are plain data so they can cross the runtime protocol; the host
/// turns them into clipboard operations, replies, or a tab title.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalHostSignals {
    /// The newest `OSC 0`/`OSC 2` title. An empty string is a reset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub title: Option<String>,
    /// An `OSC 52` store, still base64-encoded the way the program wrote it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_store: Option<String>,
    /// An `OSC 52` read. The value is the requested selection (`c`, `p`, ...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub clipboard_read: Option<String>,
    /// Colour queries, in the order they arrived.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub color_queries: Vec<TerminalColorQuery>,
}

impl TerminalHostSignals {
    /// Whether there is nothing for the host to do.
    pub fn is_empty(&self) -> bool {
        self.title.is_none()
            && self.clipboard_store.is_none()
            && self.clipboard_read.is_none()
            && self.color_queries.is_empty()
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
enum ScanState {
    #[default]
    Ground,
    /// An ESC was read and its introducer is still unknown.
    Escape,
    /// Inside `CSI ...`, before the final byte.
    Csi,
    /// Inside `OSC ...`, before BEL or ST.
    Osc,
    /// An ESC inside an OSC; `\` would terminate it.
    OscEscape,
}

/// Watches raw PTY output for host-addressed escape sequences.
#[derive(Debug, Default)]
pub struct TerminalSignalScanner {
    state: ScanState,
    buffer: Vec<u8>,
    overlong: bool,
    mouse: TerminalMouseModes,
    signals: TerminalHostSignals,
}

impl TerminalSignalScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one chunk of output. Chunk boundaries are irrelevant: a sequence
    /// split across two reads is reassembled in `state` and `buffer`.
    pub fn observe(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            match self.state {
                ScanState::Ground => {
                    if byte == 0x1b {
                        self.state = ScanState::Escape;
                    }
                }
                ScanState::Escape => self.consume_escape(byte),
                ScanState::Csi => {
                    if byte == 0x1b {
                        // A new escape aborts the malformed CSI.
                        self.state = ScanState::Escape;
                    } else if (0x40..=0x7e).contains(&byte) {
                        self.finish_csi(byte);
                        self.state = ScanState::Ground;
                    } else if self.buffer.len() < MAX_CSI_BYTES {
                        self.buffer.push(byte);
                    } else {
                        self.overlong = true;
                    }
                }
                ScanState::Osc => match byte {
                    0x07 => {
                        self.finish_osc();
                        self.state = ScanState::Ground;
                    }
                    0x1b => self.state = ScanState::OscEscape,
                    _ => {
                        if self.buffer.len() < MAX_OSC_BYTES {
                            self.buffer.push(byte);
                        } else {
                            self.overlong = true;
                        }
                    }
                },
                ScanState::OscEscape => match byte {
                    b'\\' => {
                        self.finish_osc();
                        self.state = ScanState::Ground;
                    }
                    // ESC followed by anything else is not a string terminator.
                    // The OSC is abandoned, but the byte after the ESC can
                    // still introduce a sequence of its own — a truncated
                    // title followed by `CSI ...` must not swallow the CSI.
                    0x1b => self.state = ScanState::OscEscape,
                    _ => {
                        self.buffer.clear();
                        self.overlong = false;
                        self.state = ScanState::Escape;
                        self.consume_escape(byte);
                    }
                },
            }
        }
    }

    /// The mouse dialects the program has enabled.
    pub fn mouse_modes(&self) -> TerminalMouseModes {
        self.mouse
    }

    /// Takes the requests accumulated since the last call.
    pub fn take(&mut self) -> TerminalHostSignals {
        std::mem::take(&mut self.signals)
    }

    fn consume_escape(&mut self, byte: u8) {
        match byte {
            b'[' => {
                self.buffer.clear();
                self.overlong = false;
                self.state = ScanState::Csi;
            }
            b']' => {
                self.buffer.clear();
                self.overlong = false;
                self.state = ScanState::Osc;
            }
            // A second ESC restarts the sequence; anything else ends it.
            0x1b => self.state = ScanState::Escape,
            _ => self.state = ScanState::Ground,
        }
    }

    fn finish_csi(&mut self, final_byte: u8) {
        let payload = std::mem::take(&mut self.buffer);
        let overlong = std::mem::take(&mut self.overlong);
        if overlong || !matches!(final_byte, b'h' | b'l') {
            return;
        }
        // Only a DEC private mode set/reset says anything about the mouse.
        let Some(parameters) = payload.strip_prefix(b"?") else {
            return;
        };
        let enabled = final_byte == b'h';
        for parameter in parameters.split(|byte| *byte == b';') {
            let value = parameter
                .iter()
                .take_while(|byte| byte.is_ascii_digit())
                .fold(0_u32, |value, byte| {
                    value
                        .saturating_mul(10)
                        .saturating_add(u32::from(byte - b'0'))
                });
            match value {
                1005 => self.mouse.utf8 = enabled,
                1015 => self.mouse.urxvt = enabled,
                1016 => self.mouse.sgr_pixels = enabled,
                _ => {}
            }
        }
    }

    fn finish_osc(&mut self) {
        let payload = std::mem::take(&mut self.buffer);
        let overlong = std::mem::take(&mut self.overlong);
        if overlong {
            return;
        }
        let Some(separator) = payload.iter().position(|byte| *byte == b';') else {
            return;
        };
        let identifier = &payload[..separator];
        let rest = &payload[separator + 1..];
        match identifier {
            b"0" | b"2" => {
                let mut title = String::new();
                for character in String::from_utf8_lossy(rest).chars() {
                    if character.is_control() {
                        continue;
                    }
                    if title.len() + character.len_utf8() > MAX_TITLE_BYTES {
                        break;
                    }
                    title.push(character);
                }
                self.signals.title = Some(title.trim().to_string());
            }
            // OSC 1 is the icon name; no surface of this application draws one.
            b"1" => {}
            b"52" => self.parse_clipboard(rest),
            b"4" => self.parse_palette(rest),
            b"10" => self.parse_color_query(rest, TerminalColorQuery::Foreground),
            b"11" => self.parse_color_query(rest, TerminalColorQuery::Background),
            b"12" => self.parse_color_query(rest, TerminalColorQuery::Cursor),
            _ => {}
        }
    }

    fn parse_clipboard(&mut self, rest: &[u8]) {
        let (selection, data) = match rest.iter().position(|byte| *byte == b';') {
            Some(separator) => (&rest[..separator], &rest[separator + 1..]),
            None => (&b"c"[..], &b""[..]),
        };
        let selection = String::from_utf8_lossy(selection)
            .chars()
            .filter(|character| character.is_ascii_alphabetic())
            .take(8)
            .collect::<String>();
        let selection = if selection.is_empty() {
            "c".to_string()
        } else {
            selection
        };
        if data == b"?" {
            self.signals.clipboard_read = Some(selection);
        } else {
            self.signals.clipboard_store = Some(String::from_utf8_lossy(data).into_owned());
        }
    }

    /// `OSC 4 ; index ; ? [; index ; ?]` — a palette query.
    ///
    /// A set request (`4 ; index ; #rrggbb`) is left to the emulator: it owns
    /// the palette, and the host has no RGB to answer with beyond the theme.
    fn parse_palette(&mut self, rest: &[u8]) {
        let fields = rest.split(|byte| *byte == b';').collect::<Vec<_>>();
        let mut index = 0;
        while index + 1 < fields.len() {
            let slot = parse_decimal(fields[index]);
            let query = fields[index + 1] == b"?";
            if query && let Some(slot) = slot.and_then(|slot| u8::try_from(slot).ok()) {
                self.signals
                    .color_queries
                    .push(TerminalColorQuery::Palette { index: slot });
            }
            index += 2;
        }
    }

    fn parse_color_query(&mut self, rest: &[u8], query: TerminalColorQuery) {
        if rest.iter().all(|byte| matches!(byte, b'?' | b' ' | b'\t')) {
            self.signals.color_queries.push(query);
        }
    }
}

fn parse_decimal(bytes: &[u8]) -> Option<u32> {
    let digits = bytes
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .copied()
        .collect::<Vec<_>>();
    if digits.is_empty() {
        return None;
    }
    Some(digits.iter().fold(0_u32, |value, byte| {
        value
            .saturating_mul(10)
            .saturating_add(u32::from(byte - b'0'))
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_a_title_and_ignores_control_characters() {
        let mut scanner = TerminalSignalScanner::new();
        scanner.observe(b"\x1b]2;hello\x07");
        assert_eq!(scanner.take().title.as_deref(), Some("hello"));

        scanner.observe(b"\x1b]0;vi\x1b\\");
        assert_eq!(scanner.take().title.as_deref(), Some("vi"));

        scanner.observe(b"\x1b]2;a\x01b\x07");
        assert_eq!(
            scanner.take().title.as_deref(),
            Some("ab"),
            "control bytes inside the title are not painted"
        );
    }

    #[test]
    fn reassembles_a_sequence_split_across_chunks() {
        let mut scanner = TerminalSignalScanner::new();
        scanner.observe(b"\x1b]5");
        scanner.observe(b"2;c;Y2lwYm9hcmQ=\x07");
        assert_eq!(
            scanner.take().clipboard_store.as_deref(),
            Some("Y2lwYm9hcmQ="),
            "an OSC 52 store arrives even when split mid-sequence"
        );
    }

    #[test]
    fn distinguishes_a_clipboard_store_from_a_read() {
        let mut scanner = TerminalSignalScanner::new();
        scanner.observe(b"\x1b]52;p;?\x07");
        let read = scanner.take();
        assert_eq!(read.clipboard_read.as_deref(), Some("p"));
        assert!(read.clipboard_store.is_none());

        scanner.observe(b"\x1b]52;c;aGVsbG8=\x07");
        let store = scanner.take();
        assert_eq!(store.clipboard_store.as_deref(), Some("aGVsbG8="));
        assert!(store.clipboard_read.is_none());

        scanner.observe(b"\x1b]52;c;\x07");
        assert_eq!(
            scanner.take().clipboard_store.as_deref(),
            Some(""),
            "an empty payload clears the clipboard"
        );
    }

    #[test]
    fn reads_palette_and_default_colour_queries() {
        let mut scanner = TerminalSignalScanner::new();
        scanner.observe(b"\x1b]4;1;?;9;?\x07");
        scanner.observe(b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07");
        let queries = scanner.take().color_queries;
        assert_eq!(
            queries,
            vec![
                TerminalColorQuery::Palette { index: 1 },
                TerminalColorQuery::Palette { index: 9 },
                TerminalColorQuery::Foreground,
                TerminalColorQuery::Background,
                TerminalColorQuery::Cursor,
            ]
        );

        // A palette set is not a query and must not produce a reply.
        scanner.observe(b"\x1b]4;1;#ff0000\x07");
        assert!(scanner.take().color_queries.is_empty());
    }

    #[test]
    fn tracks_the_extended_mouse_reporting_modes() {
        let mut scanner = TerminalSignalScanner::new();
        assert!(scanner.mouse_modes().is_legacy());

        scanner.observe(b"\x1b[?1000;1006;1005h");
        assert_eq!(
            scanner.mouse_modes(),
            TerminalMouseModes {
                utf8: true,
                ..TerminalMouseModes::default()
            },
            "1005 is tracked alongside the modes alacritty already knows"
        );

        scanner.observe(b"\x1b[?1015h\x1b[?1016h");
        assert!(scanner.mouse_modes().urxvt);
        assert!(scanner.mouse_modes().sgr_pixels);

        scanner.observe(b"\x1b[?1005;1015l");
        assert!(!scanner.mouse_modes().utf8);
        assert!(!scanner.mouse_modes().urxvt);
        assert!(
            scanner.mouse_modes().sgr_pixels,
            "a reset leaves the other dialect alone"
        );
    }

    #[test]
    fn ignores_payloads_it_cannot_frame() {
        let mut scanner = TerminalSignalScanner::new();
        // An image protocol APC is not an OSC and writes nothing here.
        scanner.observe(b"\x1b_Ga=T,f=100;AAAA\x1b\\");
        assert!(scanner.take().is_empty());

        // A truncated OSC followed by a fresh escape does not wedge the scanner.
        scanner.observe(b"\x1b]2;unterminated");
        scanner.observe(b"\x1b[?1005h");
        assert!(scanner.mouse_modes().utf8);

        // An oversized payload is dropped without buffering forever.
        scanner.observe(b"\x1b]52;c;");
        scanner.observe(&vec![b'A'; MAX_OSC_BYTES + 8]);
        scanner.observe(b"\x07");
        assert!(scanner.take().clipboard_store.is_none());
    }
}

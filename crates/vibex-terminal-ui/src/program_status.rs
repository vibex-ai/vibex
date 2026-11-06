//! The Program Status Protocol (OSC 7501).
//!
//! An Agent CLI that runs inside a terminal has no way to tell the workbench
//! what it is doing — the workbench is not a party to its conversation. The
//! protocol is that channel: the program draws nothing, it emits an escape
//! sequence, and the terminal turns it into a state instead of pixels.
//!
//! Two halves live here, because both must agree on one spelling:
//!
//! * [`ProgramStatusScanner`] reads the reports a program emits.
//! * [`PROGRAM_STATUS_PROBE_REPLY`] is what a terminal answers when a program
//!   asks whether the protocol is supported. A program is entitled to stay
//!   silent if nobody answers, so a terminal that parses reports without
//!   answering the probe sees none of them.
//!
//! The state words are the program's own. Nothing here is inferred from the
//! screen, and a word this module does not know is ignored rather than guessed
//! at.

/// The state a program says it is in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProgramState {
    /// Waiting for the reader to type something.
    Idle,
    /// Running a turn.
    Working,
    /// Finished a turn; the conversation stays open.
    Done,
    /// Cannot continue without the reader, most often an approval prompt.
    Blocked,
    /// The program failed and is not going to recover on its own.
    Error,
    /// The program withdrew whatever it reported before.
    Clear,
}

impl ProgramState {
    fn parse(word: &str) -> Option<Self> {
        match word {
            "idle" => Some(Self::Idle),
            "working" => Some(Self::Working),
            "done" => Some(Self::Done),
            "blocked" => Some(Self::Blocked),
            "error" => Some(Self::Error),
            "clear" => Some(Self::Clear),
            _ => None,
        }
    }
}

/// One report, as the program sent it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProgramStatus {
    pub state: ProgramState,
    /// What a blocked program is blocked on: `permission`, `question`, `auth`.
    pub kind: Option<String>,
    /// The record this report updates. Empty is the program's own record, and
    /// that is the one that describes the session; a program may report on
    /// several things it is running, and those must not overwrite it.
    pub id: String,
    /// The program's name, e.g. `claude-code`.
    pub app: String,
}

impl ProgramStatus {
    /// Whether this is the record that describes the program itself.
    pub fn is_root(&self) -> bool {
        self.id.is_empty()
    }
}

/// The introducer every report and the capability probe start with.
const INTRODUCER: &[u8] = b"\x1b]7501;";

/// What a program sends to ask whether the terminal speaks the protocol.
pub const PROGRAM_STATUS_PROBE: &[u8] = b"\x1b]7501;?";

/// What a supporting terminal answers.
///
/// The reply is the probe echoed back: that is the whole handshake, and a
/// program that gets anything else — or nothing — concludes the terminal does
/// not support the protocol and reports nothing.
pub const PROGRAM_STATUS_PROBE_REPLY: &[u8] = b"\x1b]7501;?\x1b\\";

/// The length of a complete probe at the front of `bytes`, if there is one.
///
/// `Some(None)` means the bytes are the start of a probe but stop before its
/// terminator, which is what a PTY read boundary looks like; the caller must
/// keep them and try again with more input.
pub fn probe_length(bytes: &[u8]) -> Option<Option<usize>> {
    if !bytes.starts_with(PROGRAM_STATUS_PROBE) {
        return None;
    }
    let rest = &bytes[PROGRAM_STATUS_PROBE.len()..];
    if rest.starts_with(b"\x1b\\") {
        return Some(Some(PROGRAM_STATUS_PROBE.len() + 2));
    }
    if rest.starts_with(b"\x07") {
        return Some(Some(PROGRAM_STATUS_PROBE.len() + 1));
    }
    // A terminator arriving in pieces: an introducer that is exactly the probe
    // so far is still the start of one.
    if rest.is_empty() || rest == b"\x1b" {
        return Some(None);
    }
    None
}

/// Whether `bytes` could grow into a probe.
pub fn is_probe_prefix(bytes: &[u8]) -> bool {
    !bytes.is_empty() && PROGRAM_STATUS_PROBE.starts_with(bytes)
}

/// A bounded buffer that turns PTY bytes into reports.
///
/// Reads arrive in arbitrary slices, so a report can be split across two of
/// them. Only the tail that could still become a report is kept.
#[derive(Debug, Default)]
pub struct ProgramStatusScanner {
    pending: Vec<u8>,
}

/// A report long enough to matter is still far below this; anything past it is
/// a program writing free text into an escape sequence, and is dropped rather
/// than buffered forever.
const MAX_PENDING: usize = 16 * 1024;

impl ProgramStatusScanner {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feeds one read and returns the reports it completed, in order.
    pub fn push(&mut self, bytes: &[u8]) -> Vec<ProgramStatus> {
        if bytes.is_empty() {
            return Vec::new();
        }
        self.pending.extend_from_slice(bytes);
        let mut reports = Vec::new();
        let mut consumed = 0;
        while let Some(offset) = find(&self.pending[consumed..], INTRODUCER) {
            let body_start = consumed + offset + INTRODUCER.len();
            match termination(&self.pending[body_start..]) {
                Termination::Complete { body, terminator } => {
                    let body_end = body_start + body;
                    if let Some(report) = parse_body(&self.pending[body_start..body_end]) {
                        reports.push(report);
                    }
                    consumed = body_end + terminator;
                }
                Termination::Incomplete => {
                    // Keep the introducer and the body after it: the rest of
                    // the report is in a later read.
                    self.pending.drain(..body_start - INTRODUCER.len());
                    self.cap();
                    return reports;
                }
                Termination::Malformed => {
                    // An escape that cannot terminate this body means the
                    // sequence was not a report after all. Skip its introducer
                    // and keep scanning from there instead of stalling.
                    consumed = body_start;
                }
            }
        }
        self.pending.drain(..consumed);
        self.trim_partial_introducer();
        self.cap();
        reports
    }

    /// Keeps only a tail that could still become an introducer.
    ///
    /// Nothing complete is left in the buffer, so dropping the rest is what
    /// keeps ordinary output from accumulating.
    fn trim_partial_introducer(&mut self) {
        let keep = (1..INTRODUCER.len())
            .rev()
            .find(|length| {
                self.pending.len() >= *length
                    && INTRODUCER.starts_with(&self.pending[self.pending.len() - length..])
            })
            .unwrap_or(0);
        let drop = self.pending.len() - keep;
        if drop > 0 {
            self.pending.drain(..drop);
        }
    }

    /// Bounds an unterminated report, so a program that opens one and never
    /// closes it cannot grow this buffer forever.
    fn cap(&mut self) {
        if self.pending.len() > MAX_PENDING {
            self.pending.clear();
        }
    }
}

enum Termination {
    /// The body is this long, and the terminator that ends it is this long.
    Complete { body: usize, terminator: usize },
    /// The read ended mid-report.
    Incomplete,
    /// An escape that cannot be this report's terminator.
    Malformed,
}

fn termination(bytes: &[u8]) -> Termination {
    let mut index = 0;
    while index < bytes.len() {
        match bytes[index] {
            0x07 => {
                return Termination::Complete {
                    body: index,
                    terminator: 1,
                };
            }
            0x1b => {
                return match bytes.get(index + 1) {
                    Some(b'\\') => Termination::Complete {
                        body: index,
                        terminator: 2,
                    },
                    // Nothing after the escape yet.
                    None => Termination::Incomplete,
                    Some(_) => Termination::Malformed,
                };
            }
            _ => index += 1,
        }
    }
    Termination::Incomplete
}

/// Reads `key=value` fields separated by `:`.
///
/// A report without a state word this module knows is not a report: guessing
/// what `state=bogus` meant is how a terminal starts inventing states.
fn parse_body(bytes: &[u8]) -> Option<ProgramStatus> {
    let text = std::str::from_utf8(bytes).ok()?;
    let mut state = None;
    let mut kind = None;
    let mut id = String::new();
    let mut app = String::new();
    for field in text.split(':') {
        let Some((key, value)) = field.split_once('=') else {
            continue;
        };
        match key {
            "state" => state = ProgramState::parse(value),
            "kind" => kind = Some(value.to_string()),
            "id" => id = value.to_string(),
            "app" => app = value.to_string(),
            // `progress`, `title` and `msg` describe a state the reader sees
            // elsewhere, or are base64 an Agent reports about its own UI.
            _ => {}
        }
    }
    Some(ProgramStatus {
        state: state?,
        kind,
        id,
        app,
    })
}

fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn states(reports: &[ProgramStatus]) -> Vec<ProgramState> {
        reports.iter().map(|report| report.state).collect()
    }

    /// The reports a CLI actually emits.
    #[test]
    fn reads_the_reports_a_program_emits() {
        let mut scanner = ProgramStatusScanner::new();
        let reports = scanner.push(
            b"\x1b]7501;state=working:app=claude-code\x1b\\\x1b]7501;state=blocked:kind=permission\x07",
        );
        assert_eq!(
            states(&reports),
            vec![ProgramState::Working, ProgramState::Blocked]
        );
        assert_eq!(reports[0].app, "claude-code");
        assert_eq!(reports[1].kind.as_deref(), Some("permission"));
        assert!(reports.iter().all(ProgramStatus::is_root));
    }

    /// A report split across reads is still one report.
    #[test]
    fn reads_a_report_split_across_reads() {
        let mut scanner = ProgramStatusScanner::new();
        assert!(scanner.push(b"output\x1b]75").is_empty());
        assert!(scanner.push(b"01;state=work").is_empty());
        let reports = scanner.push(b"ing:app=codex\x1b\\more output");
        assert_eq!(states(&reports), vec![ProgramState::Working]);
        assert_eq!(reports[0].app, "codex");
    }

    /// Screen text around a report is not the report.
    #[test]
    fn ignores_output_that_is_not_a_report() {
        let mut scanner = ProgramStatusScanner::new();
        let reports = scanner.push(b"plain text\r\n\x1b[31mred\x1b[0m");
        assert!(reports.is_empty());
        // A state no program may claim is not guessed at.
        assert!(scanner.push(b"\x1b]7501;state=bogus\x1b\\").is_empty());
        assert!(scanner.push(b"\x1b[2J").is_empty());
    }

    /// The capability probe is a question, not a report.
    #[test]
    fn the_probe_is_not_a_report() {
        let mut scanner = ProgramStatusScanner::new();
        assert!(scanner.push(PROGRAM_STATUS_PROBE).is_empty());
        assert!(scanner.push(b"\x1b\\").is_empty());
        assert_eq!(
            probe_length(b"\x1b]7501;?\x1b\\"),
            Some(Some(PROGRAM_STATUS_PROBE.len() + 2))
        );
        assert_eq!(probe_length(b"\x1b]7501;?\x07"), Some(Some(9)));
        // The head without a terminator is waiting for more input.
        assert_eq!(probe_length(b"\x1b]7501;?"), Some(None));
        assert_eq!(probe_length(b"\x1b]7501"), None);
        assert_eq!(probe_length(b"\x1b]7501;state=idle"), None);
    }

    /// A child record belongs to one thing the program runs, not to the
    /// program.
    #[test]
    fn a_child_record_is_not_the_program() {
        let mut scanner = ProgramStatusScanner::new();
        let reports = scanner.push(b"\x1b]7501;state=working:id=task/1\x1b\\");
        assert_eq!(states(&reports), vec![ProgramState::Working]);
        assert!(!reports[0].is_root());
    }

    /// A `clear` withdraws a state rather than naming a new one.
    #[test]
    fn clear_is_a_state_of_its_own() {
        let mut scanner = ProgramStatusScanner::new();
        let reports = scanner.push(b"\x1b]7501;state=clear\x1b\\");
        assert_eq!(states(&reports), vec![ProgramState::Clear]);
    }

    /// An unterminated body cannot grow without bound.
    #[test]
    fn an_unterminated_report_does_not_grow_forever() {
        let mut scanner = ProgramStatusScanner::new();
        scanner.push(b"\x1b]7501;state=working");
        for _ in 0..64 {
            scanner.push(&[b'x'; 1024]);
        }
        assert!(scanner.pending.len() <= MAX_PENDING);
    }
}

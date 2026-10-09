//! The clipboard the *terminal* holds: OSC 5522.
//!
//! A client running on the reader's own machine reads the clipboard from the
//! desktop, which is what [`crate::terminal::read_clipboard_image`] does. The
//! same client on the far side of an `ssh` link cannot: the clipboard belongs
//! to the machine the terminal is on, so the host's `wl-paste`/`xclip` find
//! nothing at all — a screenshot pasted there does nothing, however it is
//! pasted.
//!
//! The bytes still have a way across. The terminal emulator is the one holding
//! the clipboard, it is on the reader's side of the link, and kitty's OSC 5522
//! — an extension of the OSC 52 the client already writes with — lets a program
//! ask it for the clipboard over the same connection the screen travels on.
//! Images included, which is the half no terminal can paste by itself.
//!
//! The conversation has three steps, ordered by how much of the reader each one
//! costs, and [`Probe`] is the whole of it:
//!
//! 1. A capability query, which is a mode report nobody is asked about, so a
//!    terminal that does not speak the protocol is found out once per run
//!    rather than waited on once per paste.
//! 2. The list of media types the clipboard offers, which a terminal is
//!    required to serve without a prompt. A clipboard holding nothing the
//!    gesture can use — an empty one, or text under the attach action — ends
//!    here, and its reader is never asked for anything.
//! 3. The read itself, for the one medium the gesture wants, named and
//!    passworded so a terminal can remember "yes, and do not ask again".
//!
//! Two things make that conversation awkward, and both shape this module:
//!
//! * **The answer arrives on the input queue.** `crossterm` reads that queue,
//!   and `ESC ] 5522 ; …` is not a sequence it knows: it would deliver the
//!   packet as `Alt+]` followed by its body as typed text, straight into the
//!   reader's draft. So a probe is driven from the event loop, which stops
//!   reading keys while one is in flight — and it reads the queue at the byte
//!   level itself, because that is the only level at which a protocol packet
//!   can be told apart from a keystroke.
//! * **The terminal asks its reader first.** The read may be confirmed, so an
//!   answer may be seconds behind the request and no keystroke may be read in
//!   between. The probe therefore waits for the *first* packet of a read much
//!   longer than it waits between packets: silence is what a prompt looks like.
//!
//! Nothing here is required for a client that runs where the reader is: the
//! host's own clipboard tools answer first, and this is what is tried when they
//! come back empty — which is exactly the `ssh` case, and also a machine that
//! simply has no clipboard tool installed.

use std::io::Write;
use std::sync::atomic::{AtomicU8, Ordering};
use std::time::{Duration, Instant};

/// The name the terminal shows its reader when it asks about this client.
///
/// Together with the password below it is what lets a terminal remember "yes,
/// and do not ask again", so a paste is only ever confirmed once per run.
const CLIENT_NAME: &str = "Vibex TUI";

/// The media types this client can take from a clipboard, in the order it
/// prefers them. PNG first: a screenshot is always one.
const IMAGE_TYPES: [&str; 5] = [
    "image/png",
    "image/jpeg",
    "image/webp",
    "image/gif",
    "image/bmp",
];

/// The type a paste falls back to when the clipboard holds no picture.
const TEXT_TYPE: &str = "text/plain";

/// How long the terminal may take to answer the capability query.
///
/// The answer is a mode report, not a question for the reader: a terminal that
/// speaks the protocol replies at once, and one that does not replies never.
const CAPABILITY_LIMIT: Duration = Duration::from_millis(300);

/// How long the terminal may take to send the *first* packet of an answer.
///
/// This is the prompt budget rather than a network one: the reader has to
/// notice the terminal's confirmation and answer it. Once an answer is in
/// flight the packets arrive back to back, and [`ANSWER_QUIET`] takes over.
const ANSWER_LIMIT: Duration = Duration::from_secs(10);

/// How long an answer in flight may pause between packets.
const ANSWER_QUIET: Duration = Duration::from_millis(500);

/// How long the terminal may take to answer a request it does not have to put
/// to its reader.
///
/// The list of media types is served without a prompt, so the wait for it is a
/// round trip rather than a reader's decision — and a terminal that has already
/// said it speaks the protocol and then says nothing is not worth waiting out.
const LIST_LIMIT: Duration = Duration::from_secs(2);

/// The longest one answer may take, however steadily it arrives.
///
/// An answer that is arriving is worth waiting out — a screenshot is megabytes
/// over a link that may be slow — but a terminal that never stops talking is
/// not, and the reader's input queue is held for as long as this lasts.
const ANSWER_TOTAL: Duration = Duration::from_secs(60);

/// The most one answer may carry, base64 and metadata included.
///
/// Base64 inflates by a third, so this is past the largest picture the composer
/// would attach: a bigger answer is one that would be refused at the end of it,
/// and the cap stops a terminal that streams forever from being read forever.
const ANSWER_BYTES: usize = 12 * 1024 * 1024;

/// The most one packet may carry before it is treated as noise.
const PACKET_BYTES: usize = 64 * 1024;

/// The longest one [`Probe::pump`] waits for the terminal before handing the
/// loop back so it can paint and drain worker results.
const PUMP_WAIT: Duration = Duration::from_millis(20);

/// How long a pump sleeps when the queue reports ready but has nothing to read.
const PUMP_RETRY: Duration = Duration::from_millis(2);

const ESC: u8 = 0x1b;
const BEL: u8 = 0x07;
const CTRL_C: u8 = 0x03;

/// The introducer of an OSC 5522 packet.
const PACKET_START: &[u8] = b"\x1b]5522;";

/// The mode the capability query asks about.
const CAPABILITY_QUERY: &str = "\x1b[?5522$p";

/// The answer to the capability query, which is a mode report rather than a
/// clipboard packet.
const CAPABILITY_PREFIX: &[u8] = b"\x1b[?5522;";

/// What the terminal has said about the protocol.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Support {
    /// Never asked, or asked and not answered.
    Unknown,
    /// The terminal implements the mode, so the protocol is there.
    Yes,
    /// The terminal does not implement it, or would not answer.
    No,
}

/// What the terminal has answered so far, remembered for the whole run.
///
/// One run has one terminal, and the second paste in a terminal that does not
/// speak the protocol should not pay the first paste's wait again.
static SUPPORT: AtomicU8 = AtomicU8::new(Support::Unknown as u8);

/// What the terminal has said about the protocol.
pub fn support() -> Support {
    match SUPPORT.load(Ordering::Relaxed) {
        1 => Support::Yes,
        2 => Support::No,
        _ => Support::Unknown,
    }
}

fn remember(support: Support) {
    SUPPORT.store(support as u8, Ordering::Relaxed);
}

/// Whether asking the terminal for its clipboard can work at all.
///
/// The protocol is a conversation on the terminal's input queue, and reading
/// that queue at the byte level is a descriptor operation: the Unix build has
/// it, and a platform without one keeps to the host's own clipboard tools.
pub fn available() -> bool {
    cfg!(unix) && support() != Support::No
}

/// Which half of the terminal's clipboard a read wants.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Wanted {
    /// An image when there is one, the text otherwise: the reader's paste.
    Everything,
    /// Only an image: the composer's attach action names a file when there is
    /// no picture to take.
    Image,
}

/// What the terminal's clipboard held for one read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalClipboard {
    Image {
        mime_type: String,
        bytes: Vec<u8>,
    },
    Text(String),
    /// The terminal answered and holds nothing this client can use.
    Empty,
    /// The terminal was asked and would not hand its clipboard over: the
    /// reader declined, or the terminal's configuration forbids the read.
    Refused,
    /// The terminal does not speak the protocol, or stopped answering.
    Unsupported,
}

/// What one [`Probe::pump`] produced.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Progress {
    /// The terminal has not finished; pump again.
    Waiting,
    /// The terminal answered, or will not.
    Answered(TerminalClipboard),
    /// The reader pressed `Ctrl+C` while the probe owned the input queue.
    ///
    /// The probe must end and the key must reach the interface: a client
    /// waiting on a prompt its reader cannot see must still be quittable.
    Interrupted,
}

/// One question in flight: what does the terminal hold?
///
/// The probe owns the terminal's input queue for as long as it is alive: the
/// event loop must not read keys while one is in flight, or the terminal's
/// answer would arrive as keystrokes. [`Probe::pump`] is the only reader.
pub struct Probe {
    wanted: Wanted,
    /// When the stage in flight began, for [`ANSWER_TOTAL`].
    started: Instant,
    /// The hard end of the stage in flight.
    deadline: Instant,
    /// When the stage gives up if no packet arrives.
    quiet_at: Instant,
    /// Whether the terminal has answered with a packet of its own.
    heard: bool,
    stage: Option<Stage>,
}

/// How far a probe has got.
enum Stage {
    /// The capability query is in flight.
    Capability { reply: Vec<u8> },
    /// The list of media types the clipboard offers is in flight.
    Types { packets: Packets },
    /// The read request is in flight and the terminal is answering.
    Answer { packets: Packets },
}

impl Probe {
    /// Ask the terminal for the clipboard its reader is holding.
    pub fn start(wanted: Wanted) -> Self {
        let now = Instant::now();
        let mut probe = Self {
            wanted,
            started: now,
            deadline: now + CAPABILITY_LIMIT,
            quiet_at: now + CAPABILITY_LIMIT,
            heard: false,
            stage: Some(Stage::Capability { reply: Vec::new() }),
        };
        if support() == Support::Yes {
            // Already asked and answered in this run; the query would only
            // cost a round trip.
            probe.begin_types();
        } else {
            // A terminal that does not implement the mode answers `Ps` 0 or 4,
            // and one that does not implement the query at all says nothing:
            // both are found out inside `CAPABILITY_LIMIT`, and neither asks
            // the reader a question, which is why the capability comes first.
            send(CAPABILITY_QUERY);
        }
        probe
    }

    /// Read whatever the terminal has sent, and advance the conversation.
    ///
    /// Returns [`Progress::Waiting`] when the terminal has not finished, which
    /// is the loop's cue to paint, drain worker results, and pump again.
    pub fn pump(&mut self) -> Progress {
        let mut buffer = [0u8; 4096];
        let mut waited = false;
        loop {
            let now = Instant::now();
            let give_up_at = self.deadline.min(self.quiet_at);
            if now >= give_up_at {
                return Progress::Answered(self.give_up());
            }
            // Everything already queued is taken first; only an empty queue is
            // worth waiting on, and never for longer than one pump slice.
            let wait = if waited {
                Duration::ZERO
            } else {
                (give_up_at - now).min(PUMP_WAIT)
            };
            waited = true;
            match ready(wait) {
                Ready::Yes => {}
                Ready::No => return Progress::Waiting,
                Ready::Gone => return Progress::Answered(self.give_up()),
            }
            match read_terminal(&mut buffer) {
                // The terminal closed, or something else owns it now.
                Read::Bytes(0) | Read::Failed => {
                    return Progress::Answered(self.give_up());
                }
                Read::Bytes(count) => {
                    if let Some(progress) = self.absorb(&buffer[..count]) {
                        return progress;
                    }
                }
                // A queue that reports ready with nothing in it: give it a
                // moment rather than spinning on it.
                Read::Retry => std::thread::sleep(PUMP_RETRY),
            }
        }
    }

    /// Fold one read's bytes into the stage in flight.
    ///
    /// `Some(progress)` when the stage is over.
    fn absorb(&mut self, bytes: &[u8]) -> Option<Progress> {
        // The capability reply is a mode report rather than a packet stream,
        // so it is the one stage read on its own.
        if let Some(Stage::Capability { reply }) = self.stage.as_mut() {
            // The reader's one way out of a wait they cannot see the end of is
            // the same in every stage.
            if bytes.contains(&CTRL_C) {
                return Some(Progress::Interrupted);
            }
            reply.extend_from_slice(bytes);
            // The query is one short sequence; anything longer is not an answer
            // to it.
            if reply.len() > 1024 {
                return Some(Progress::Answered(self.give_up()));
            }
            return match capability_status(reply) {
                None => None,
                Some(false) => Some(Progress::Answered(self.give_up())),
                Some(true) => {
                    remember(Support::Yes);
                    self.begin_types();
                    None
                }
            };
        }
        let verdict = match self.stage.as_mut() {
            Some(Stage::Types { packets }) | Some(Stage::Answer { packets }) => {
                let verdict = packets.push(bytes);
                if packets.heard {
                    // The answer is in flight, so it is worth waiting out: the
                    // quiet window catches a stall, the budget is refreshed
                    // because the terminal is plainly answering, and the total
                    // cap catches an answer that never ends.
                    self.heard = true;
                    let now = Instant::now();
                    let total = self.started + ANSWER_TOTAL;
                    self.quiet_at = (now + ANSWER_QUIET).min(total);
                    self.deadline = (now + ANSWER_LIMIT).min(total);
                }
                verdict
            }
            _ => return None,
        };
        match verdict {
            Verdict::Pending => None,
            Verdict::Interrupted => Some(Progress::Interrupted),
            Verdict::TooLarge => Some(Progress::Answered(TerminalClipboard::Empty)),
            Verdict::Done => self.finish(),
        }
    }

    /// The end of a packet stage: the type list says what to ask for, and the
    /// read says what the clipboard held.
    fn finish(&mut self) -> Option<Progress> {
        let packets = match self.stage.take() {
            Some(Stage::Types { packets }) => {
                if packets.refused {
                    return Some(Progress::Answered(TerminalClipboard::Refused));
                }
                let Some(mime) = choose(&packets.types, self.wanted) else {
                    // Nothing on the clipboard is something this gesture can
                    // use, and the read that would have asked the reader for
                    // permission is not sent at all.
                    return Some(Progress::Answered(TerminalClipboard::Empty));
                };
                self.begin_read(mime);
                return None;
            }
            Some(Stage::Answer { packets }) => packets,
            _ => return Some(Progress::Answered(TerminalClipboard::Empty)),
        };
        Some(Progress::Answered(packets.answer(self.wanted)))
    }

    /// Ask which media types the clipboard offers.
    ///
    /// This is the one read a terminal has to serve without asking its reader,
    /// which is what keeps a gesture with nothing to take — an empty clipboard,
    /// a clipboard of text under the attach action — from prompting for a
    /// permission it would not use.
    fn begin_types(&mut self) {
        let now = Instant::now();
        send(&types_request());
        self.stage = Some(Stage::Types {
            packets: Packets::default(),
        });
        self.started = now;
        // No reader is being asked anything here, so the wait is a round trip.
        self.deadline = now + LIST_LIMIT;
        self.quiet_at = self.deadline;
    }

    /// Ask for the one medium the gesture wants, named so a terminal can
    /// remember the reader's answer.
    fn begin_read(&mut self, mime: &str) {
        let now = Instant::now();
        send(&read_request(&[mime], &password()));
        self.stage = Some(Stage::Answer {
            packets: Packets::default(),
        });
        self.started = now;
        self.deadline = now + ANSWER_LIMIT;
        // Silence before the first packet is the terminal prompting its
        // reader, and a prompt has no deadline of its own; silence *between*
        // packets is an answer that stopped, and that has.
        self.quiet_at = self.deadline;
    }

    /// The end of a stage that produced nothing usable.
    fn give_up(&mut self) -> TerminalClipboard {
        if !self.heard {
            // Waiting the whole budget out is an answer of its own: do not
            // make the next paste wait it out again.
            remember(Support::No);
        }
        self.stage = None;
        TerminalClipboard::Unsupported
    }
}

/// One answer's packets, as they arrive.
#[derive(Default)]
struct Packets {
    /// Bytes received that are not yet a whole packet.
    buffer: Vec<u8>,
    /// The data of each media type, in the order the terminal sent them.
    types: Vec<(String, Vec<u8>)>,
    /// Payload bytes accepted so far, for [`ANSWER_BYTES`].
    bytes: usize,
    /// Whether a whole packet has been taken, which is what tells a quiet
    /// answer from a reader who is still reading a prompt.
    heard: bool,
    /// Whether the terminal refused the request.
    refused: bool,
}

/// How one delivery of bytes left the answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Verdict {
    Pending,
    Done,
    TooLarge,
    Interrupted,
}

impl Packets {
    /// Fold the bytes just read into the answer.
    ///
    /// Bytes that are not part of a packet are dropped: a pointer report or a
    /// keystroke can land in the same queue while the terminal is answering,
    /// and neither may end the answer — a reader clicking the terminal's own
    /// confirmation is the normal way through this code.
    fn push(&mut self, incoming: &[u8]) -> Verdict {
        // A packet body is base64 and metadata: it never carries `Ctrl+C`.
        if incoming.contains(&CTRL_C) {
            return Verdict::Interrupted;
        }
        self.buffer.extend_from_slice(incoming);
        loop {
            if !skip_to_packet(&mut self.buffer) {
                break;
            }
            let Some(body) = take_packet(&mut self.buffer) else {
                break;
            };
            self.heard = true;
            if let Some(verdict) = self.apply(&body) {
                return verdict;
            }
        }
        if self.bytes > ANSWER_BYTES {
            return Verdict::TooLarge;
        }
        Verdict::Pending
    }

    /// Fold one packet body in. `Some(verdict)` when the answer is over.
    fn apply(&mut self, body: &str) -> Option<Verdict> {
        // `<metadata>;<payload>`, and a status-only packet has no payload at
        // all — `status=OK` arrives as metadata alone.
        let (metadata, payload) = body.split_once(';').unwrap_or((body, ""));
        let mut status = None;
        let mut mime = None;
        for field in metadata.split(':') {
            match field.split_once('=') {
                Some(("status", value)) => status = Some(value),
                Some(("mime", value)) => mime = Some(value),
                _ => {}
            }
        }
        match status {
            // The terminal accepted the request; the data follows.
            Some("OK") => None,
            Some("DATA") => {
                self.absorb_data(mime, payload);
                None
            }
            Some("DONE") => Some(Verdict::Done),
            // `EPERM`, `ENOSYS`, `EBUSY`, `EIO`: the terminal will not serve
            // this request, and there is nothing to paste.
            Some(_) => {
                self.refused = true;
                Some(Verdict::Done)
            }
            None => None,
        }
    }

    /// Append one chunk of one media type.
    fn absorb_data(&mut self, mime: Option<&str>, payload: &str) {
        // The media type is base64, like the payload it labels.
        let Some(mime) = mime
            .and_then(crate::terminal::decode_base64)
            .and_then(|bytes| String::from_utf8(bytes).ok())
        else {
            return;
        };
        // Every chunk is base64 on its own: the protocol pads each one.
        let Some(chunk) = crate::terminal::decode_base64(payload) else {
            return;
        };
        self.bytes += chunk.len();
        match self.types.iter_mut().find(|(kind, _)| *kind == mime) {
            Some((_, bytes)) => bytes.extend_from_slice(&chunk),
            None => self.types.push((mime, chunk)),
        }
    }

    /// The medium this answer carried, if any.
    fn answer(self, wanted: Wanted) -> TerminalClipboard {
        if self.refused {
            return TerminalClipboard::Refused;
        }
        pick(self.types, wanted)
    }
}

/// The request that asks which media types the clipboard offers.
///
/// The payload is the protocol's one-period spelling of "just the list", and
/// the request carries no password: there is nothing here for a terminal to
/// confirm with its reader.
fn types_request() -> String {
    let payload = crate::terminal::encode_base64(b".");
    format!("\x1b]5522;type=read;{payload}\x1b\\")
}

/// The request that asks for `types`, named so a terminal can remember the
/// reader's answer.
fn read_request(types: &[&str], password: &str) -> String {
    let payload = crate::terminal::encode_base64(types.join(" ").as_bytes());
    let name = crate::terminal::encode_base64(CLIENT_NAME.as_bytes());
    let password = crate::terminal::encode_base64(password.as_bytes());
    format!("\x1b]5522;type=read:name={name}:pw={password};{payload}\x1b\\")
}

/// The one medium to ask for, out of the ones the clipboard offers.
///
/// An image wins over text: the picture is the half a terminal cannot paste by
/// itself, and a clipboard can hold both at once. `None` is a clipboard holding
/// nothing this gesture can use, which is a request that is never sent.
fn choose(offered: &[(String, Vec<u8>)], wanted: Wanted) -> Option<&'static str> {
    for preferred in IMAGE_TYPES {
        if offered.iter().any(|(mime, _)| mime == preferred) {
            return Some(preferred);
        }
    }
    if wanted == Wanted::Everything && offered.iter().any(|(mime, _)| mime == TEXT_TYPE) {
        return Some(TEXT_TYPE);
    }
    None
}

/// The password this run gives the terminal.
///
/// Random per run and never persisted, which is what the protocol asks for: it
/// is what "allow every future request" is scoped to, so a second paste in the
/// same run is not a second question.
fn password() -> String {
    static PASSWORD: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    PASSWORD
        .get_or_init(|| vibex_core::RequestId::new().into_string())
        .clone()
}

/// Write one whole sequence to the terminal.
fn send(sequence: &str) {
    let mut stdout = std::io::stdout();
    let _ = stdout
        .write_all(sequence.as_bytes())
        .and_then(|()| stdout.flush());
}

/// Whether a DECRPM reply says the mode is implemented.
///
/// `0` is "not recognised" and `4` is "permanently reset"; `1`, `2` and `3` all
/// mean the terminal knows the mode, which is what the protocol needs. `None`
/// means the reply has not arrived whole yet.
fn capability_status(reply: &[u8]) -> Option<bool> {
    let start = find(reply, CAPABILITY_PREFIX)? + CAPABILITY_PREFIX.len();
    let end = reply[start..].iter().position(|byte| *byte == b'$')? + start;
    let value: u8 = std::str::from_utf8(&reply[start..end])
        .ok()?
        .trim()
        .parse()
        .ok()?;
    Some((1..=3).contains(&value))
}

/// The first index of `needle` in `haystack`.
fn find(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

/// Drop everything that cannot be the start of a packet.
///
/// `true` when a packet start is now at the front of the buffer. A suffix that
/// could still become one is kept, so a packet split across two reads is not
/// thrown away with the noise in front of it.
fn skip_to_packet(buffer: &mut Vec<u8>) -> bool {
    if buffer.starts_with(PACKET_START) {
        return true;
    }
    if let Some(start) = find(buffer, PACKET_START) {
        buffer.drain(..start);
        return true;
    }
    let keep = (1..PACKET_START.len())
        .rev()
        .find(|length| {
            *length <= buffer.len() && buffer[buffer.len() - length..] == PACKET_START[..*length]
        })
        .unwrap_or(0);
    let drop = buffer.len() - keep;
    buffer.drain(..drop);
    false
}

/// Take one whole packet's body off the front of the buffer.
///
/// `None` when the packet is still arriving.
fn take_packet(buffer: &mut Vec<u8>) -> Option<String> {
    debug_assert!(buffer.starts_with(PACKET_START));
    let body_start = PACKET_START.len();
    let rest = &buffer[body_start..];
    let mut terminator = None;
    for (index, byte) in rest.iter().enumerate() {
        if *byte == BEL {
            terminator = Some((index, 1));
            break;
        }
        if *byte == ESC {
            if index + 1 == rest.len() {
                // `ESC \` is the string terminator; until the byte after the
                // escape arrives there is no packet to read.
                break;
            }
            // A lone `ESC` ends the body too, and whatever follows it is noise
            // rather than part of this packet.
            terminator = Some((index, if rest[index + 1] == b'\\' { 2 } else { 1 }));
            break;
        }
    }
    let Some((length, terminator)) = terminator else {
        if rest.len() > PACKET_BYTES {
            // No terminator in sight: this is not a packet, it is noise that
            // happens to start like one.
            buffer.clear();
        }
        return None;
    };
    let body = String::from_utf8_lossy(&rest[..length]).into_owned();
    buffer.drain(..body_start + length + terminator);
    Some(body)
}

/// The one medium a terminal answered with, chosen the way this client wants
/// it.
///
/// An image wins over text: the picture is the half a terminal cannot paste by
/// itself, and a clipboard can hold both at once.
fn pick(mut types: Vec<(String, Vec<u8>)>, wanted: Wanted) -> TerminalClipboard {
    for preferred in IMAGE_TYPES {
        let found = types
            .iter()
            .position(|(kind, bytes)| kind == preferred && !bytes.is_empty());
        if let Some(index) = found {
            let (mime_type, bytes) = types.swap_remove(index);
            return TerminalClipboard::Image { mime_type, bytes };
        }
    }
    if wanted == Wanted::Everything {
        let found = types
            .iter()
            .position(|(kind, bytes)| kind == TEXT_TYPE && !bytes.is_empty());
        if let Some(index) = found {
            let (_, bytes) = types.swap_remove(index);
            if let Ok(text) = String::from_utf8(bytes) {
                return TerminalClipboard::Text(text);
            }
        }
    }
    TerminalClipboard::Empty
}

/// Whether the terminal has bytes for us.
enum Ready {
    Yes,
    No,
    /// The queue cannot be polled at all; the probe should stop.
    Gone,
}

/// Wait up to `wait` for the terminal to have something to read.
#[cfg(unix)]
fn ready(wait: Duration) -> Ready {
    use rustix::event::{PollFd, PollFlags, Timespec, poll};

    let stdin = rustix::stdio::stdin();
    let mut fds = [PollFd::new(&stdin, PollFlags::IN)];
    let timeout = Timespec {
        tv_sec: wait.as_secs() as rustix::event::Secs,
        tv_nsec: wait.subsec_nanos() as rustix::event::Nsecs,
    };
    match poll(&mut fds, Some(&timeout)) {
        Ok(0) => Ready::No,
        Ok(_) => Ready::Yes,
        Err(_) => Ready::Gone,
    }
}

#[cfg(not(unix))]
fn ready(_wait: Duration) -> Ready {
    Ready::Gone
}

/// What one read of the terminal's input queue produced.
enum Read {
    Bytes(usize),
    /// Nothing to read after all, or nothing that can be read now.
    Retry,
    Failed,
}

/// Read whatever the terminal's input queue holds, without waiting.
#[cfg(unix)]
fn read_terminal(buffer: &mut [u8]) -> Read {
    match rustix::io::read(rustix::stdio::stdin(), buffer) {
        Ok(0) => Read::Bytes(0),
        Ok(count) => Read::Bytes(count),
        Err(rustix::io::Errno::INTR) | Err(rustix::io::Errno::AGAIN) => Read::Retry,
        Err(_) => Read::Failed,
    }
}

#[cfg(not(unix))]
fn read_terminal(_buffer: &mut [u8]) -> Read {
    Read::Failed
}

#[cfg(test)]
mod tests {
    use super::*;

    /// One packet as the terminal writes it, `ST`-terminated.
    fn packet(body: &str) -> String {
        format!("\x1b]5522;{body}\x1b\\")
    }

    /// One `DATA` packet carrying `bytes` as `mime`.
    fn chunk(mime: &str, bytes: &[u8]) -> String {
        packet(&format!(
            "type=read:status=DATA:mime={};{}",
            crate::terminal::encode_base64(mime.as_bytes()),
            crate::terminal::encode_base64(bytes),
        ))
    }

    #[test]
    fn the_request_names_the_client_and_the_types_it_wants() {
        assert_eq!(
            read_request(&["image/png", "text/plain"], "secret"),
            format!(
                "\x1b]5522;type=read:name={}:pw={};{}\x1b\\",
                crate::terminal::encode_base64(b"Vibex TUI"),
                crate::terminal::encode_base64(b"secret"),
                crate::terminal::encode_base64(b"image/png text/plain"),
            )
        );
    }

    /// A clipboard offering `mimes`, as the type list reports it.
    fn offered(mimes: &[&str]) -> Vec<(String, Vec<u8>)> {
        mimes
            .iter()
            .map(|mime| ((*mime).to_string(), Vec::new()))
            .collect()
    }

    #[test]
    fn the_type_asked_for_is_the_one_this_client_most_wants() {
        // A picture is the half a terminal cannot paste by itself, so it wins
        // over the text a clipboard may also hold, and PNG wins over the JPEG a
        // browser screenshot arrives as.
        assert_eq!(
            choose(&offered(&["text/plain", "image/jpeg"]), Wanted::Everything),
            Some("image/jpeg")
        );
        assert_eq!(
            choose(
                &offered(&["text/plain", "image/jpeg", "image/png"]),
                Wanted::Everything
            ),
            Some("image/png")
        );
        assert_eq!(
            choose(&offered(&["text/plain"]), Wanted::Everything),
            Some("text/plain")
        );
        // The attach action has no use for text, and nothing at all is not a
        // reason to ask the terminal's reader for anything.
        assert_eq!(choose(&offered(&["text/plain"]), Wanted::Image), None);
        assert_eq!(choose(&offered(&[]), Wanted::Everything), None);
        assert_eq!(
            choose(
                &offered(&["image/svg+xml", "text/html"]),
                Wanted::Everything
            ),
            None
        );
    }

    #[test]
    fn only_a_mode_report_that_knows_the_mode_means_the_protocol_is_there() {
        assert_eq!(capability_status(b"\x1b[?5522;1$y"), Some(true));
        assert_eq!(capability_status(b"\x1b[?5522;2$y"), Some(true));
        assert_eq!(capability_status(b"\x1b[?5522;3$y"), Some(true));
        assert_eq!(capability_status(b"\x1b[?5522;0$y"), Some(false));
        assert_eq!(capability_status(b"\x1b[?5522;4$y"), Some(false));
        // A reply that has not arrived whole is not an answer yet.
        assert_eq!(capability_status(b"\x1b[?5522;"), None);
        assert_eq!(capability_status(b""), None);
        // A device-attributes reply in front of it does not hide it.
        assert_eq!(capability_status(b"\x1b[?6c\x1b[?5522;2$y"), Some(true));
    }

    #[test]
    fn an_answer_is_reassembled_from_the_packets_it_arrives_in() {
        let mut packets = Packets::default();
        assert_eq!(
            packets.push(packet("type=read:status=OK").as_bytes()),
            Verdict::Pending
        );
        // The chunks of one type arrive back to back and are joined.
        assert_eq!(
            packets.push(chunk("image/png", b"one").as_bytes()),
            Verdict::Pending
        );
        assert_eq!(
            packets.push(chunk("image/png", b"two").as_bytes()),
            Verdict::Pending
        );
        assert_eq!(
            packets.push(packet("type=read:status=DONE").as_bytes()),
            Verdict::Done
        );
        assert_eq!(
            packets.answer(Wanted::Everything),
            TerminalClipboard::Image {
                mime_type: "image/png".to_string(),
                bytes: b"onetwo".to_vec(),
            }
        );
    }

    #[test]
    fn a_packet_split_across_two_reads_still_arrives() {
        let mut packets = Packets::default();
        let whole = chunk("image/png", b"split across reads");
        let (first, second) = whole.as_bytes().split_at(9);
        assert_eq!(packets.push(first), Verdict::Pending);
        assert_eq!(packets.push(second), Verdict::Pending);
        assert_eq!(
            packets.push(packet("type=read:status=DONE").as_bytes()),
            Verdict::Done
        );
        assert_eq!(
            packets.answer(Wanted::Image),
            TerminalClipboard::Image {
                mime_type: "image/png".to_string(),
                bytes: b"split across reads".to_vec(),
            }
        );
    }

    #[test]
    fn a_packet_marker_split_by_a_read_boundary_is_not_mistaken_for_noise() {
        let mut packets = Packets::default();
        // Four of the marker's seven bytes, with noise in front of them.
        assert_eq!(packets.push(b"typed\x1b]55"), Verdict::Pending);
        let whole = packet("type=read:status=DONE");
        assert_eq!(packets.push(&whole.as_bytes()[4..]), Verdict::Done);
    }

    #[test]
    fn noise_around_a_packet_does_not_hide_it() {
        // A keystroke or a pointer report lands in the same queue while the
        // terminal is answering, and neither may end the answer.
        let mut packets = Packets::default();
        let mut bytes = b"typed while the prompt was up\x1b[<0;12;8M".to_vec();
        bytes.extend_from_slice(chunk("image/png", b"picture").as_bytes());
        bytes.extend_from_slice(b"\x1b[<0;13;8M");
        assert_eq!(packets.push(&bytes), Verdict::Pending);
        assert_eq!(
            packets.push(packet("type=read:status=DONE").as_bytes()),
            Verdict::Done
        );
        assert_eq!(
            packets.answer(Wanted::Everything),
            TerminalClipboard::Image {
                mime_type: "image/png".to_string(),
                bytes: b"picture".to_vec(),
            }
        );
    }

    #[test]
    fn a_bel_terminated_packet_is_read_too() {
        let mut packets = Packets::default();
        assert_eq!(
            packets.push(b"\x1b]5522;type=read:status=DONE\x07"),
            Verdict::Done
        );
    }

    #[test]
    fn a_refusal_is_reported_rather_than_a_missing_picture() {
        let mut packets = Packets::default();
        assert_eq!(
            packets.push(packet("type=read:status=EPERM").as_bytes()),
            Verdict::Done
        );
        assert_eq!(
            packets.answer(Wanted::Everything),
            TerminalClipboard::Refused
        );
    }

    #[test]
    fn the_picture_wins_over_the_text_the_clipboard_also_holds() {
        let both = vec![
            ("text/plain".to_string(), b"words".to_vec()),
            ("image/jpeg".to_string(), b"jpeg".to_vec()),
            ("image/png".to_string(), b"png".to_vec()),
        ];
        assert_eq!(
            pick(both, Wanted::Everything),
            TerminalClipboard::Image {
                mime_type: "image/png".to_string(),
                bytes: b"png".to_vec(),
            }
        );
        assert_eq!(
            pick(
                vec![("text/plain".to_string(), b"words".to_vec())],
                Wanted::Everything
            ),
            TerminalClipboard::Text("words".to_string())
        );
        // The attach action has no use for text, and nothing at all is not a
        // picture either.
        assert_eq!(
            pick(
                vec![("text/plain".to_string(), b"words".to_vec())],
                Wanted::Image
            ),
            TerminalClipboard::Empty
        );
        assert_eq!(
            pick(
                vec![("image/png".to_string(), Vec::new())],
                Wanted::Everything
            ),
            TerminalClipboard::Empty
        );
    }

    #[test]
    fn ctrl_c_reaches_the_reader_through_an_answer_in_flight() {
        let mut packets = Packets::default();
        assert_eq!(packets.push(b"\x03"), Verdict::Interrupted);
    }

    #[test]
    fn a_packet_that_never_ends_is_not_buffered_forever() {
        let mut packets = Packets::default();
        let mut bytes = PACKET_START.to_vec();
        bytes.extend(std::iter::repeat_n(b'A', PACKET_BYTES + 1));
        assert_eq!(packets.push(&bytes), Verdict::Pending);
        assert!(packets.buffer.is_empty());
    }

    #[test]
    fn an_answer_past_the_cap_is_abandoned() {
        let mut packets = Packets::default();
        let bytes = vec![0u8; 64 * 1024];
        let mut verdict = Verdict::Pending;
        for _ in 0..(ANSWER_BYTES / bytes.len() + 2) {
            verdict = packets.push(chunk("image/png", &bytes).as_bytes());
            if verdict != Verdict::Pending {
                break;
            }
        }
        assert_eq!(verdict, Verdict::TooLarge);
    }

    #[test]
    fn the_password_is_one_value_for_the_whole_run() {
        assert_eq!(password(), password());
        assert!(!password().is_empty());
    }
}

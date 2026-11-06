//! What Vibex can tell about a terminal-mode Agent from the outside.
//!
//! The Agent owns its interface, so nothing here parses a conversation. The
//! only question is the one the sidebar has to answer: is this session working,
//! is it parked on the reader, or has it settled — and how sure are we.
//!
//! Three rules shape the module:
//!
//! * **A program that reports its own state is believed over the screen.** The
//!   Program Status Protocol (OSC 7501) is the Agent saying what it is doing;
//!   the screen rules are what is left when it says nothing. A terminal that
//!   parses the reports but never answers the protocol's capability probe
//!   receives none of them, which is why the PTY host answers it.
//! * **A terminal is read from its bottom buffer, never from the viewport.** A
//!   reader who scrolls up has not changed what the Agent is doing, so the
//!   emulator is fed in full and the *last screenful* is what rules see.
//! * **`Unknown` is a real answer.** `Idle` is published only when a rule
//!   matched live idle chrome, `working -> idle` needs several agreeing scans,
//!   and a reading older than [`OBSERVATION_FRESH_MS`] must be treated as
//!   unknown by the reader. Guessing "idle" while a CLI waits on an approval
//!   prompt is the one wrong answer a reader would act on.
//!
//! The module is deliberately free of I/O and clock reads: the caller supplies
//! the timestamp, so every rule and every damping decision is testable without
//! a PTY.

use std::collections::HashMap;

use vibex_core::{AgentId, TerminalActivity, TerminalActivitySource, TerminalId};
use vibex_terminal_ui::{ProgramState, ProgramStatusScanner, TerminalEmulator};

/// How long an observation stays believable.
///
/// A reader ages anything older back to `Unknown`, so a session whose observer
/// stopped is never left claiming a state it can no longer see.
#[cfg(test)]
pub const OBSERVATION_FRESH_MS: i64 = 10_000;

/// A `working` screen becomes idle only after this many agreeing scans.
///
/// A TUI that blanks between repaints would otherwise publish a completion, and
/// a completion is what drives the unread mark and the notification.
pub const IDLE_CONFIRMATIONS: u8 = 3;

/// The reading a screen rule produced for one scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ScreenReading {
    Idle,
    Working,
    Waiting,
    /// The Agent's CLI reported, or the screen showed, a failure it will not
    /// continue from on its own.
    Failed,
    /// The Agent's CLI has ended its session but the process is still there.
    ///
    /// A terminal Agent does not die on the first interrupt: Claude asks for a
    /// second one, Codex disconnects the task and leaves a shell-like prompt.
    /// Both keep the process alive, so a liveness check on the process alone
    /// never sees the session end — the screen is the only place the CLI says
    /// so.
    Exited,
    /// The screen shows no live chrome either way, and no report arrived.
    Unknown,
}

impl ScreenReading {
    pub fn activity(self) -> TerminalActivity {
        match self {
            Self::Idle => TerminalActivity::Idle,
            Self::Working => TerminalActivity::Working,
            Self::Waiting => TerminalActivity::Waiting,
            Self::Failed => TerminalActivity::Failed,
            // A session whose CLI ended is not "doing" anything: it reports no
            // activity, and the caller closes the session from the reading.
            Self::Exited | Self::Unknown => TerminalActivity::Unknown,
        }
    }

    /// Whether the screen says the Agent's session is over.
    pub fn is_exit(self) -> bool {
        self == Self::Exited
    }

    fn from_program_state(state: ProgramState) -> Option<Self> {
        match state {
            ProgramState::Idle | ProgramState::Done => Some(Self::Idle),
            ProgramState::Working => Some(Self::Working),
            ProgramState::Blocked => Some(Self::Waiting),
            ProgramState::Error => Some(Self::Failed),
            // A withdrawal is not a reading: the next sweep falls back to the
            // rules rather than inventing one.
            ProgramState::Clear => None,
        }
    }
}

/// One Agent's bottom-of-screen rules.
///
/// Each rule binds to controls an Agent shows only while it is in that state.
/// Matching incidental prose is what makes a detector wrong on a long enough
/// conversation, so no rule matches free text alone.
pub struct ScreenProfile {
    agent: AgentId,
    rules: Vec<ScreenRule>,
}

struct ScreenRule {
    reading: ScreenReading,
    /// Every needle must appear.
    contains: &'static [&'static str],
    /// At least one must appear; empty means "no alternative required".
    any: &'static [&'static str],
    /// Any of these makes the rule inapplicable.
    without: &'static [&'static str],
}

impl ScreenRule {
    fn applies(&self, haystack: &str) -> bool {
        self.contains.iter().all(|needle| haystack.contains(needle))
            && (self.any.is_empty() || self.any.iter().any(|needle| haystack.contains(needle)))
            && !self.without.iter().any(|needle| haystack.contains(needle))
    }
}

impl ScreenProfile {
    /// The bundled profiles, one per Agent terminal mode supports.
    pub fn bundled() -> Vec<Self> {
        vec![
            ScreenProfile {
                agent: AgentId::parse("claude").expect("literal agent id"),
                rules: vec![
                    // The CLI stays alive after one interrupt and asks for a
                    // second, so this is the only place the session's end is
                    // visible. It outranks every other rule: an exit prompt is
                    // not an idle prompt, and acting on the wrong one leaves the
                    // reader typing into a session that is over.
                    ScreenRule {
                        reading: ScreenReading::Exited,
                        contains: &[],
                        any: &["press ctrl-c again to exit", "press ctrl+c again to exit"],
                        without: &[],
                    },
                    ScreenRule {
                        reading: ScreenReading::Exited,
                        contains: &[],
                        any: &["resume this session with:"],
                        without: &[],
                    },
                    // A permission or selection dialog. The cancel footer is what
                    // distinguishes it from prose that happens to mention a
                    // choice.
                    ScreenRule {
                        reading: ScreenReading::Waiting,
                        contains: &["esc to cancel"],
                        any: &[
                            "enter to confirm",
                            "enter to select",
                            "tab/arrow keys to navigate",
                            "arrow keys to navigate",
                        ],
                        without: &[],
                    },
                    ScreenRule {
                        reading: ScreenReading::Waiting,
                        contains: &["do you want to proceed?"],
                        any: &["1. yes", "2. yes", "3. no", "yes, and don't ask again"],
                        without: &[],
                    },
                    // An approval prompt without the numbered layout.
                    ScreenRule {
                        reading: ScreenReading::Waiting,
                        contains: &["waiting for permission"],
                        any: &[],
                        without: &[],
                    },
                    // Live idle chrome: the shortcut footer belongs to the prompt
                    // box, which only exists while the Agent is waiting for text.
                    ScreenRule {
                        reading: ScreenReading::Idle,
                        contains: &[],
                        any: &["? for shortcuts", "shift+tab to cycle"],
                        without: &["esc to cancel", "esc to interrupt"],
                    },
                    ScreenRule {
                        reading: ScreenReading::Working,
                        contains: &[],
                        any: &["esc to interrupt", "esc to stop"],
                        without: &[],
                    },
                ],
            },
            ScreenProfile {
                agent: AgentId::parse("codex").expect("literal agent id"),
                rules: vec![
                    // Codex answers an interrupt by ending the task and leaving
                    // a prompt that looks like a shell; the process survives, so
                    // only the message distinguishes "done" from "idle".
                    ScreenRule {
                        reading: ScreenReading::Exited,
                        contains: &[],
                        any: &["disconnected from this task", "stop the current turn:"],
                        without: &[],
                    },
                    ScreenRule {
                        reading: ScreenReading::Waiting,
                        contains: &[],
                        any: &[
                            "press enter to confirm",
                            "enter to submit answer",
                            "enter to submit all",
                            "allow command?",
                            "action required",
                        ],
                        without: &[],
                    },
                    ScreenRule {
                        reading: ScreenReading::Idle,
                        contains: &[],
                        any: &["? for shortcuts", "ctrl+j newline"],
                        without: &["esc to interrupt", "press enter to confirm"],
                    },
                    ScreenRule {
                        reading: ScreenReading::Working,
                        contains: &[],
                        any: &["esc to interrupt", "esc to stop", " to interrupt)"],
                        without: &[],
                    },
                ],
            },
        ]
    }

    pub fn agent(&self) -> &AgentId {
        &self.agent
    }

    /// Evaluates this profile against an already-extracted bottom buffer.
    ///
    /// Earlier rules win, so each profile lists its blocking shapes first: a
    /// dialog that also shows an idle footer must read as waiting.
    pub fn read(&self, screen: &str) -> ScreenReading {
        let haystack = screen.to_lowercase();
        for rule in &self.rules {
            if rule.applies(&haystack) {
                return rule.reading;
            }
        }
        ScreenReading::Unknown
    }
}

/// Damping for one terminal's published activity.
#[derive(Debug, Default)]
struct Damping {
    published: TerminalActivity,
    pending_idle: u8,
}

impl Damping {
    /// Decides what to publish for a fresh reading.
    ///
    /// `working -> idle` needs several agreeing scans because it is the
    /// transition that fires completions, and a TUI that blanks between
    /// repaints would otherwise announce a finished turn. Every other change
    /// publishes at once: a blocker is worth interrupting for, and a return to
    /// work should clear the mark without a delay.
    ///
    /// `reported` is the program's own word, so it is not damped: a program
    /// that says it finished means it finished, and waiting for three sweeps
    /// only delays the reader's answer.
    fn accept(&mut self, reading: ScreenReading, reported: bool) -> Option<TerminalActivity> {
        let next = reading.activity();
        if next == self.published {
            self.pending_idle = 0;
            return None;
        }
        if !reported
            && self.published == TerminalActivity::Working
            && next == TerminalActivity::Idle
        {
            self.pending_idle = self.pending_idle.saturating_add(1);
            if self.pending_idle < IDLE_CONFIRMATIONS {
                return None;
            }
        }
        self.pending_idle = 0;
        self.published = next;
        Some(next)
    }
}

/// A state change worth persisting and broadcasting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservationUpdate {
    pub terminal_id: TerminalId,
    pub session_id: String,
    pub activity: TerminalActivity,
    pub source: TerminalActivitySource,
    pub observed_at_ms: i64,
    /// The Agent's CLI said its session is over, while the process may still be
    /// alive waiting for a second interrupt.
    pub exited: bool,
    /// The Agent's own session id, when the CLI printed how to resume it.
    ///
    /// A terminal Agent keeps its conversation in its own store, so Vibex cannot
    /// read it — but the exit notice names it, and that is enough for a restart
    /// to continue the same conversation instead of opening an empty one.
    pub native_session_id: Option<String>,
}

/// The session id a CLI printed in its exit notice.
///
/// Both supported CLIs print a ready-to-paste resume command on the way out
/// (`claude --resume <id>`, `codex resume <id>`). Reading that line is the only
/// way to learn the Agent's own session id, because Vibex is not a party to the
/// conversation.
fn resume_session_id(screen: &str) -> Option<String> {
    for line in screen.lines() {
        let trimmed = line.trim();
        let candidate = trimmed
            .strip_prefix("claude --resume ")
            .or_else(|| trimmed.strip_prefix("codex resume "))
            .or_else(|| trimmed.strip_prefix("codex resume --last "));
        if let Some(rest) = candidate {
            let id = rest
                .split_whitespace()
                .next()
                .unwrap_or_default()
                .trim_matches(|character: char| {
                    !character.is_ascii_alphanumeric() && character != '-'
                });
            // A uuid, not a flag: the notice is copied from a screen a reader
            // also reads, so the check is on shape rather than on position.
            if id.len() >= 8 && id.chars().all(|c| c.is_ascii_alphanumeric() || c == '-') {
                return Some(id.to_string());
            }
        }
    }
    None
}

/// One watched terminal: its emulator, its rules, and its damping.
pub struct WatchedTerminal {
    session_id: String,
    /// Retained for diagnostics; the rules are resolved at construction.
    #[allow(dead_code)]
    agent: AgentId,
    emulator: TerminalEmulator,
    rules: ScreenProfile,
    damping: Damping,
    /// The last activity this watch published, so a steady screen writes nothing.
    published: Option<(TerminalActivity, TerminalActivitySource)>,
    /// Set once the screen has said the session is over.
    published_exit: bool,
    /// Reports from the Program Status Protocol, which outrank the screen.
    scanner: ProgramStatusScanner,
    /// What the program last said about the session itself.
    ///
    /// Kept beside the screen reading rather than replacing it: a program that
    /// reports once and then goes quiet is still the best evidence there is,
    /// and a program that only ever reports a child record has said nothing
    /// about the session.
    reported: Option<ProgramState>,
}

impl WatchedTerminal {
    /// Starts watching one session's terminal.
    ///
    /// A profile is chosen by Agent; an Agent without one still gets liveness
    /// (its PTY is the fact), it simply never reaches `idle` or `working` from
    /// the screen.
    pub fn new(
        session_id: impl Into<String>,
        agent: AgentId,
        rows: u16,
        columns: u16,
        profiles: &[ScreenProfile],
    ) -> Self {
        let rules = profiles
            .iter()
            .find(|profile| profile.agent() == &agent)
            .map(|profile| ScreenProfile {
                agent: profile.agent.clone(),
                rules: profile
                    .rules
                    .iter()
                    .map(|rule| ScreenRule {
                        reading: rule.reading,
                        contains: rule.contains,
                        any: rule.any,
                        without: rule.without,
                    })
                    .collect(),
            });
        let rules = rules.unwrap_or_else(|| ScreenProfile {
            agent: agent.clone(),
            rules: Vec::new(),
        });
        Self {
            session_id: session_id.into(),
            agent,
            emulator: TerminalEmulator::new(rows.max(1), columns.max(1)),
            rules,
            damping: Damping::default(),
            published: None,
            published_exit: false,
            scanner: ProgramStatusScanner::new(),
            reported: None,
        }
    }

    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Feeds bytes from the PTY.
    ///
    /// The emulator's own scrollback is what makes the bottom buffer correct;
    /// the reader's scroll position is not part of it.
    pub fn advance(&mut self, bytes: &[u8]) {
        if bytes.is_empty() {
            return;
        }
        for report in self.scanner.push(bytes) {
            // A program's own record describes the session. Records it opens
            // for the things it runs are its business, and letting one of them
            // paint the row would report a subtask's state as the session's.
            if !report.is_root() {
                continue;
            }
            self.reported = match report.state {
                ProgramState::Clear => None,
                state => Some(state),
            };
        }
        self.emulator.advance(bytes);
    }

    /// The last screenful, which is what rules read.
    pub fn bottom_screen(&self) -> String {
        self.emulator.visible_text()
    }

    /// The activity the program last said it was doing, when it said anything.
    ///
    /// This is the only evidence a CLI gives that does not have to be guessed
    /// from pixels, so the row's reading is taken from it whenever it exists.
    fn reported_reading(&self) -> Option<ScreenReading> {
        self.reported.and_then(ScreenReading::from_program_state)
    }

    /// Reads the screen and returns the activity to publish, if it changed.
    ///
    /// The caller supplies `now_ms` so the update is stamped with the same
    /// clock the store writes, and so tests need no wall clock.
    pub fn scan(&mut self, now_ms: i64) -> Option<ObservationUpdate> {
        let screen = self.bottom_screen();
        let screen_reading = self.rules.read(&screen);
        // A program's own report wins over the screen. It is the only evidence
        // that does not have to be inferred, and the CLIs that speak the
        // protocol keep their chrome terse enough that rules misread it: an
        // "esc to interrupt" footer can survive the end of a turn, and an
        // approval dialog built from a border and a list matches no rule at all.
        //
        // The exit notice is the exception, because it is not part of the turn
        // the program reported on: the process is alive behind a prompt, and
        // the screen is the only place that says so.
        let reported = self.reported_reading();
        if screen_reading.is_exit() {
            // Publish once. The session is then closed by the caller, and a
            // closed session produces no further readings.
            if self.published_exit {
                return None;
            }
            self.published_exit = true;
            // The CLI prints how to continue this conversation. Capturing it is
            // what lets a restart resume the Agent's own history instead of
            // starting a second, empty session beside it.
            let native_session_id = resume_session_id(&screen);
            return Some(ObservationUpdate {
                terminal_id: TerminalId::new(),
                session_id: self.session_id.clone(),
                activity: TerminalActivity::Unknown,
                source: TerminalActivitySource::Screen,
                observed_at_ms: now_ms,
                exited: true,
                native_session_id,
            });
        }
        let reading = reported.unwrap_or(screen_reading);
        let activity = self.damping.accept(reading, reported.is_some())?;
        let source = match activity {
            TerminalActivity::Unknown => TerminalActivitySource::None,
            // A reading the program supplied is a hook report, even when the
            // screen would have said the same thing: the reader is told which
            // one to trust if they ever disagree.
            _ if reported.is_some() => TerminalActivitySource::Hook,
            _ => TerminalActivitySource::Screen,
        };
        if self.published == Some((activity, source)) {
            return None;
        }
        self.published = Some((activity, source));
        Some(ObservationUpdate {
            terminal_id: TerminalId::new(),
            session_id: self.session_id.clone(),
            activity,
            source,
            observed_at_ms: now_ms,
            exited: false,
            native_session_id: None,
        })
    }

    /// The activity a `scan` would publish right now, without damping history.
    #[cfg(test)]
    pub fn current_reading(&self) -> ScreenReading {
        self.rules.read(&self.bottom_screen())
    }
}

/// The set of terminals a runtime is observing, keyed by terminal id.
#[derive(Default)]
pub struct TerminalObservations {
    profiles: Vec<ScreenProfile>,
    terminals: HashMap<TerminalId, WatchedTerminal>,
    /// Updates produced by the byte tap between two sweeps.
    ///
    /// The tap runs on the PTY reader thread and has nowhere to publish to, so
    /// a reading it produced used to be dropped on the floor: the tap advanced
    /// the damping, and the sweep then saw an unchanged reading and published
    /// nothing. A turn that started and finished inside one 300ms window was
    /// therefore invisible. Queueing it here makes the tap a producer and the
    /// sweep the single publisher.
    pending: Vec<ObservationUpdate>,
}

impl TerminalObservations {
    pub fn new() -> Self {
        Self {
            profiles: ScreenProfile::bundled(),
            terminals: HashMap::new(),
            pending: Vec::new(),
        }
    }

    pub fn watch(
        &mut self,
        terminal_id: TerminalId,
        session_id: impl Into<String>,
        agent: AgentId,
        rows: u16,
        columns: u16,
    ) {
        let watched =
            WatchedTerminal::new(session_id, agent, rows, columns, self.profiles.as_slice());
        self.terminals.insert(terminal_id, watched);
    }

    /// Every watched terminal with the session it belongs to.
    ///
    /// The retirement sweep asks the launcher whether each one is still alive,
    /// which needs both ids: the launcher speaks in terminal ids, the session
    /// store in session ids.
    pub fn watched_terminals(&self) -> Vec<(TerminalId, String)> {
        self.terminals
            .iter()
            .map(|(terminal_id, watched)| (terminal_id.clone(), watched.session_id().to_string()))
            .collect()
    }

    pub fn unwatch(&mut self, terminal_id: &TerminalId) -> Option<WatchedTerminal> {
        // A reading the tap queued but the sweep has not published describes a
        // terminal nobody is watching any more. Publishing it would write an
        // activity onto a session that has already been retired, which reads as
        // a closed session whose Agent started working again.
        self.pending
            .retain(|update| &update.terminal_id != terminal_id);
        self.terminals.remove(terminal_id)
    }

    #[cfg(test)]
    pub fn watched_mut(&mut self, terminal_id: &TerminalId) -> Option<&mut WatchedTerminal> {
        self.terminals.get_mut(terminal_id)
    }

    /// Feeds one read and records whether the reading changed.
    ///
    /// The update is queued for the next sweep rather than returned, because
    /// the caller is the PTY reader thread: it must not touch the session store
    /// or the event bus, and the sweep is the one place that publishes.
    pub fn observe(&mut self, terminal_id: &TerminalId, bytes: &[u8], now_ms: i64) {
        let Some(watched) = self.terminals.get_mut(terminal_id) else {
            return;
        };
        watched.advance(bytes);
        if let Some(mut update) = watched.scan(now_ms) {
            update.terminal_id = terminal_id.clone();
            self.pending.push(update);
        }
    }

    /// Everything that changed since the last sweep.
    ///
    /// The queue comes first: a report the tap already read is older than
    /// whatever the screen says now, and publishing it after the fresh scan
    /// would make the row walk backwards.
    pub fn scan_all(&mut self, now_ms: i64) -> Vec<ObservationUpdate> {
        let mut updates = std::mem::take(&mut self.pending);
        for (terminal_id, watched) in self.terminals.iter_mut() {
            if let Some(mut update) = watched.scan(now_ms) {
                update.terminal_id = terminal_id.clone();
                updates.push(update);
            }
        }
        updates
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn claude_profile() -> ScreenProfile {
        ScreenProfile::bundled()
            .into_iter()
            .find(|profile| profile.agent().as_str() == "claude")
            .expect("claude has a bundle")
    }

    /// The screen says when the CLI ended its session, even though the process
    /// is still alive.
    ///
    /// A terminal Agent does not die on the first interrupt: Claude asks for a
    /// second one, Codex disconnects the task and leaves a shell-like prompt.
    /// A liveness check alone therefore never sees the end, which is what left
    /// the session stuck behind an exit prompt.
    #[test]
    fn an_exit_prompt_reads_as_an_ended_session() {
        for (agent, screen) in [
            (
                "claude",
                "\n\nPress Ctrl-C again to exit\n\nResume this session with:\nclaude --resume abc",
            ),
            (
                "codex",
                "^C^CDisconnected from this task. Any running work continues.\n\
                 To reconnect, run:\n  codex resume abc\nStop the current turn: run codex agents",
            ),
        ] {
            let mut watched = WatchedTerminal::new(
                "session-1",
                AgentId::parse(agent).unwrap(),
                10,
                80,
                &ScreenProfile::bundled(),
            );
            watched.advance(b"\x1b[2J\x1b[H");
            watched.advance(screen.as_bytes());
            assert_eq!(
                watched.current_reading(),
                ScreenReading::Exited,
                "{agent} exit prompt must read as an ended session"
            );
        }
    }

    /// An exit reading is published once, and closes the session rather than
    /// reporting an activity.
    #[test]
    fn an_exit_reading_is_published_once() {
        let mut observations = TerminalObservations::new();
        let terminal_id = TerminalId::new();
        observations.watch(
            terminal_id.clone(),
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
        );
        observations
            .watched_mut(&terminal_id)
            .unwrap()
            .advance(b"\x1b[2J\x1b[HPress Ctrl-C again to exit");
        let updates = observations.scan_all(1_000);
        assert_eq!(updates.len(), 1);
        assert!(updates[0].exited);
        assert_eq!(updates[0].activity, TerminalActivity::Unknown);
        // A settled exit screen is not re-published on every sweep.
        assert!(observations.scan_all(1_100).is_empty());
    }

    /// The exit notice's resume command is the only place the Agent's own
    /// session id appears, so it is what a restart continues from.
    #[test]
    fn the_exit_notice_yields_the_agents_session_id() {
        assert_eq!(
            resume_session_id(
                "Resume this session with:\nclaude --resume 54689079-d7b6-4dc5-9a38-8a4202f5f2ad"
            ),
            Some("54689079-d7b6-4dc5-9a38-8a4202f5f2ad".to_string())
        );
        assert_eq!(
            resume_session_id(
                "To reconnect, run:\n  codex resume 01a1aa4d-b3b1-7723-a20b-3e04da40dd97"
            ),
            Some("01a1aa4d-b3b1-7723-a20b-3e04da40dd97".to_string())
        );
    }

    /// A screen that only says the session ended carries no id.
    #[test]
    fn an_exit_notice_without_a_resume_command_yields_nothing() {
        assert_eq!(resume_session_id("Press Ctrl-C again to exit"), None);
        assert_eq!(resume_session_id(""), None);
        // A line that names the command but no usable id is not a session.
        assert_eq!(resume_session_id("claude --resume"), None);
    }

    /// The live idle footer is what makes an idle claim safe; its absence is
    /// not evidence of anything.
    #[test]
    fn idle_requires_the_prompt_footer() {
        let profile = claude_profile();
        assert_eq!(
            profile.read("some output\n\n❯ \n? for shortcuts"),
            ScreenReading::Idle
        );
        assert_eq!(
            profile.read("some output\n\n❯ "),
            ScreenReading::Unknown,
            "an empty prompt box without its footer is not proof of idleness"
        );
    }

    /// A dialog outranks the footer it is drawn over.
    #[test]
    fn a_blocking_dialog_reads_as_waiting() {
        let profile = claude_profile();
        let screen = "Do you want to proceed?\n❯ 1. Yes\n  2. No\n\
                      Esc to cancel · Enter to confirm\n? for shortcuts";
        assert_eq!(profile.read(screen), ScreenReading::Waiting);
    }

    #[test]
    fn the_working_footer_reads_as_working() {
        let profile = claude_profile();
        assert_eq!(
            profile.read("✻ Thinking… (esc to interrupt)"),
            ScreenReading::Working
        );
    }

    /// `working -> idle` is the transition that fires completions, so it waits
    /// for several agreeing scans.
    #[test]
    fn a_working_screen_needs_agreement_before_it_settles() {
        let mut damping = Damping::default();
        assert_eq!(
            damping.accept(ScreenReading::Working, false),
            Some(TerminalActivity::Working)
        );
        for expected in 1..IDLE_CONFIRMATIONS {
            assert_eq!(
                damping.accept(ScreenReading::Idle, false),
                None,
                "scan {expected} must not settle the session"
            );
        }
        assert_eq!(
            damping.accept(ScreenReading::Idle, false),
            Some(TerminalActivity::Idle)
        );
    }

    /// A blocker is published at once: it is the one state worth interrupting
    /// for.
    #[test]
    fn a_blocker_is_published_without_damping() {
        let mut damping = Damping::default();
        assert_eq!(
            damping.accept(ScreenReading::Working, false),
            Some(TerminalActivity::Working)
        );
        assert_eq!(
            damping.accept(ScreenReading::Waiting, false),
            Some(TerminalActivity::Waiting)
        );
    }

    /// An unrelated screen never invents a state.
    #[test]
    fn an_unrecognized_screen_stays_unknown() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            10,
            80,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"just some command output\n");
        assert_eq!(watched.current_reading(), ScreenReading::Unknown);
        assert_eq!(watched.scan(1_000), None);
    }

    /// The observer reads the bottom buffer, so a rule fires only once the live
    /// chrome is actually on the last screen.
    #[test]
    fn the_reading_follows_the_bottom_of_the_screen() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            4,
            40,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b[2J\x1b[H");
        watched.advance("❯ \n? for shortcuts".as_bytes());
        assert_eq!(watched.current_reading(), ScreenReading::Idle);
        // Enough output to push the footer off the screen retires the reading.
        for index in 0..6 {
            watched.advance(format!("line {index}\r\n").as_bytes());
        }
        assert_eq!(watched.current_reading(), ScreenReading::Unknown);
    }

    /// An Agent without a profile still gets watched; it just never reaches a
    /// state from the screen.
    #[test]
    fn an_agent_without_a_profile_stays_unknown() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("opencode").unwrap(),
            10,
            80,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"? for shortcuts\nesc to interrupt\n");
        assert_eq!(watched.current_reading(), ScreenReading::Unknown);
    }

    /// The sweep publishes only the terminals whose reading changed.
    #[test]
    fn scanning_all_reports_only_changes() {
        let mut observations = TerminalObservations::new();
        let terminal_id = TerminalId::new();
        observations.watch(
            terminal_id.clone(),
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
        );
        observations
            .watched_mut(&terminal_id)
            .unwrap()
            .advance(b"\x1b[2J\x1b[H? for shortcuts");
        // The first scan publishes idle, and the second publishes nothing.
        assert_eq!(observations.scan_all(1_000).len(), 1);
        assert!(observations.scan_all(1_100).is_empty());
    }

    /// A report the program sent is what the row publishes, whatever the
    /// screen around it looks like.
    ///
    /// This is the whole reason the protocol exists: Claude Code keeps an
    /// "esc to interrupt" footer after a turn ends and draws an approval dialog
    /// no bottom-of-screen rule recognizes, so the screen rules alone report
    /// the wrong state exactly when the reader needs the right one.
    #[test]
    fn a_program_report_outranks_the_screen() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
            &ScreenProfile::bundled(),
        );
        let update = |watched: &mut WatchedTerminal, now| watched.scan(now).unwrap();

        watched
            .advance(b"\x1b[2J\x1b[H? for shortcuts\x1b]7501;state=working:app=claude-code\x1b\\");
        let working = update(&mut watched, 1_000);
        assert_eq!(working.activity, TerminalActivity::Working);
        assert_eq!(working.source, TerminalActivitySource::Hook);

        // The footer survives the end of the turn; the report does not.
        watched.advance(b"\x1b]7501;state=done:app=claude-code\x1b\\");
        let done = update(&mut watched, 1_100);
        assert_eq!(done.activity, TerminalActivity::Idle);
        assert_eq!(done.source, TerminalActivitySource::Hook);
        assert_eq!(
            watched.current_reading(),
            ScreenReading::Idle,
            "the screen still shows the idle footer, so only the report can tell"
        );
        // The report is addressed to the terminal, not to the reader: the
        // emulator must consume it instead of drawing its fields.
        assert!(
            !watched.bottom_screen().contains("7501")
                && !watched.bottom_screen().contains("state="),
            "a status report must never reach the visible screen: {:?}",
            watched.bottom_screen()
        );
    }

    /// `blocked` is the state a row has to interrupt for, and the report
    /// publishes it without waiting for a second agreeing scan.
    #[test]
    fn a_blocked_report_publishes_at_once() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b[2J\x1b[H? for shortcuts\x1b]7501;state=working\x1b\\");
        assert_eq!(
            watched.scan(1_000).unwrap().activity,
            TerminalActivity::Working
        );

        watched.advance(b"\x1b]7501;state=blocked:kind=permission\x1b\\");
        let blocked = watched.scan(1_100).unwrap();
        assert_eq!(blocked.activity, TerminalActivity::Waiting);
        assert_eq!(blocked.source, TerminalActivitySource::Hook);
    }

    /// A report that says `error` is a failure the reader can act on, not an
    /// unknown observation.
    #[test]
    fn an_error_report_reads_as_failed() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("codex").unwrap(),
            6,
            60,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b]7501;state=error:app=codex\x1b\\");
        let update = watched.scan(1_000).unwrap();
        assert_eq!(update.activity, TerminalActivity::Failed);
        assert_eq!(update.source, TerminalActivitySource::Hook);
    }

    /// A program that withdraws its report leaves the screen rules in charge.
    #[test]
    fn a_cleared_report_falls_back_to_the_screen() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b[2J\x1b[H? for shortcuts\x1b]7501;state=working\x1b\\");
        assert_eq!(
            watched.scan(1_000).unwrap().activity,
            TerminalActivity::Working
        );

        // The program withdrew its record; the idle footer is now the only
        // evidence, and it is damped like any other screen reading.
        watched.advance(b"\x1b]7501;state=clear\x1b\\");
        // The screen still says idle, but the damping that guards every screen
        // reading applies again now that the program withdrew its word.
        let mut settled = None;
        for scan in 1..=u32::from(IDLE_CONFIRMATIONS) {
            settled = watched.scan(1_000 + i64::from(scan) * 100);
            if scan < u32::from(IDLE_CONFIRMATIONS) {
                assert_eq!(settled, None, "scan {scan} must not settle the session");
            }
        }
        let settled = settled.unwrap();
        assert_eq!(settled.activity, TerminalActivity::Idle);
        assert_eq!(settled.source, TerminalActivitySource::Screen);
    }

    /// A record the program opened for one of its tasks is not the program.
    #[test]
    fn a_child_record_does_not_paint_the_session() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b[2J\x1b[Hesc to interrupt\x1b]7501;state=working\x1b\\");
        assert_eq!(
            watched.scan(1_000).unwrap().activity,
            TerminalActivity::Working
        );

        watched.advance(b"\x1b]7501;state=idle:id=task/1\x1b\\");
        assert_eq!(
            watched.scan(1_100),
            None,
            "a subtask finishing must not settle the session"
        );
    }

    /// The exit notice is not part of a turn report: the process is still
    /// alive behind a prompt, and only the screen says so.
    #[test]
    fn an_exit_notice_outranks_a_stale_report() {
        let mut watched = WatchedTerminal::new(
            "session-1",
            AgentId::parse("claude").unwrap(),
            8,
            60,
            &ScreenProfile::bundled(),
        );
        watched.advance(b"\x1b]7501;state=working\x1b\\");
        assert_eq!(
            watched.scan(1_000).unwrap().activity,
            TerminalActivity::Working
        );

        watched.advance(b"\x1b[2J\x1b[HPress Ctrl-C again to exit");
        let exit = watched.scan(1_100).unwrap();
        assert!(exit.exited);
        assert_eq!(exit.activity, TerminalActivity::Unknown);
    }

    /// An update the byte tap produced is published by the next sweep.
    ///
    /// The tap runs on the PTY reader thread and cannot write to the store, so
    /// what it observed has to survive until the sweep runs. Dropping it made a
    /// turn that started and finished inside one 300ms window invisible: the
    /// tap had already advanced the damping, so the sweep saw no change and
    /// published nothing.
    #[test]
    fn a_tap_update_is_published_by_the_next_sweep() {
        let mut observations = TerminalObservations::new();
        let terminal_id = TerminalId::new();
        observations.watch(
            terminal_id.clone(),
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
        );
        observations.observe(
            &terminal_id,
            b"\x1b[2J\x1b[H? for shortcuts\x1b]7501;state=working\x1b\\",
            1_000,
        );
        let updates = observations.scan_all(1_100);
        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].activity, TerminalActivity::Working);
        assert_eq!(updates[0].source, TerminalActivitySource::Hook);
        assert_eq!(updates[0].terminal_id, terminal_id);
        // And it is published once, like any other update.
        assert!(observations.scan_all(1_200).is_empty());
    }

    /// A reading queued for a terminal that stopped being watched is dropped.
    ///
    /// The session was already retired, so publishing the update would write an
    /// activity onto a closed row and make it look like the Agent came back.
    #[test]
    fn unwatching_drops_what_the_tap_queued() {
        let mut observations = TerminalObservations::new();
        let terminal_id = TerminalId::new();
        observations.watch(
            terminal_id.clone(),
            "session-1",
            AgentId::parse("claude").unwrap(),
            6,
            60,
        );
        observations.observe(
            &terminal_id,
            b"\x1b[2J\x1b[Hesc to interrupt\x1b]7501;state=working\x1b\\",
            1_000,
        );
        observations.unwatch(&terminal_id);
        assert!(observations.scan_all(1_100).is_empty());
    }

    /// A reading is only believable while it is fresh.
    #[test]
    fn a_reading_ages_out() {
        let facts = vibex_core::TerminalSessionFacts {
            observed_at_ms: 1_000,
            ..Default::default()
        };
        assert!(!facts.is_stale_at(1_000 + OBSERVATION_FRESH_MS, OBSERVATION_FRESH_MS));
        assert!(facts.is_stale_at(1_001 + OBSERVATION_FRESH_MS, OBSERVATION_FRESH_MS));
    }
}

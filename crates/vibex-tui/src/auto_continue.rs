//! The desktop's auto-continue, on the character grid.
//!
//! A session the reader has switched auto-continue on continues itself: when a
//! turn stops without an answer — the Agent errored, or went idle without
//! producing a final message — the client asks the runtime to continue it. The
//! reader gets a countdown first, because a message that appears by itself with
//! no warning is indistinguishable from a runaway Agent.
//!
//! The rules are the Desktop's on purpose. The same predicate decides whether a
//! turn needs continuing ([`agent_session_turn_requires_continuation`]), the
//! same reading of the timeline decides whether it ended normally
//! ([`latest_timeline_turn_ended_normally`]), the same five-second countdown
//! precedes the send, and the same per-turn bookkeeping keeps it from firing
//! twice for one turn. The preference itself lives in the sidebar arrangement's
//! auto-continue fields, so switching it on here switches it on there.
//!
//! What differs is where the answer comes from: the Desktop reads its open
//! timeline, so a client that is not showing the session asks the runtime for
//! that session's page instead ([`AutoContinueAction::Probe`]).

use std::collections::{BTreeMap, BTreeSet};

use vibex_core::{
    AgentSession, AgentSessionState, VibexSessionId, agent_session_turn_requires_continuation,
};

/// How long the reader has to stop a continuation before it is sent. The
/// Desktop counts the same five seconds down in its composer.
pub const COUNTDOWN_MS: i64 = 5_000;

/// How long a probe that was not answered (or whose answer was superseded) is
/// left alone before it is asked again. The Desktop uses the same second.
pub const PROBE_RETRY_MS: i64 = 1_000;

/// What a session's last turn did, as far as the client has been able to tell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct TurnStatus {
    /// The `updated_at_ms` the answer belongs to. A session that has moved on
    /// since makes the answer useless rather than wrong.
    pub updated_at_ms: i64,
    /// `None` when the timeline had produced nothing to judge yet.
    pub ended_normally: Option<bool>,
}

/// A continuation waiting out its countdown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Countdown {
    pub updated_at_ms: i64,
    pub deadline_ms: i64,
    /// The seconds last shown, so a tick that does not move the display is not
    /// a reason to repaint.
    pub shown_seconds: u8,
}

/// What the client should do about auto-continue after a sync or a tick.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AutoContinueAction {
    /// Ask the runtime what the session's last turn did: the session list alone
    /// cannot say whether a turn ended normally, and only the timeline can.
    Probe {
        session_id: VibexSessionId,
        updated_at_ms: i64,
    },
    /// Send the continuation.
    Continue {
        session_id: VibexSessionId,
        updated_at_ms: i64,
    },
}

/// The reader's auto-continue preferences, and the turns they are acting on.
#[derive(Debug, Clone, Default)]
pub struct AutoContinue {
    /// Sessions the authority has switched on, effective.
    enabled: BTreeSet<String>,
    /// Session overrides, so a toggle knows what to write: `false` entries are
    /// sessions switched off under an on-by-default project.
    overrides: BTreeMap<String, bool>,
    /// Sessions the authority has suspended until the reader resumes them.
    authority_paused: BTreeSet<String>,
    /// Sessions this client suspended. The protocol has no "pause" change —
    /// only the enable/disable that clears one — so a pause made here is this
    /// client's until the session is switched on again or a message resumes it.
    local_paused: BTreeSet<String>,
    /// Turns the reader stopped: the countdown was called off, so this turn
    /// must not be started again however long it stays idle.
    paused_turns: BTreeSet<String>,
    /// The session revision a continuation has already been sent for.
    handled_turns: BTreeMap<String, i64>,
    /// The last answer per session, and the revision it belongs to.
    statuses: BTreeMap<String, TurnStatus>,
    /// In-flight probes: session → (revision, when it was asked).
    probes: BTreeMap<String, (i64, i64)>,
    countdowns: BTreeMap<String, Countdown>,
}

impl AutoContinue {
    /// Adopt the authority's preferences.
    ///
    /// `sessions` hydrates a project default the way the Desktop does at
    /// startup: a project with auto-continue on continues its sessions, whether
    /// or not each one carries its own override. A suspension survives, and so
    /// does a local toggle that the authority has not echoed yet.
    pub fn apply_authority(
        &mut self,
        project_ids: &BTreeSet<String>,
        overrides: &BTreeMap<String, bool>,
        enabled: &BTreeSet<String>,
        paused: &BTreeSet<String>,
        sessions: &[AgentSession],
    ) {
        self.overrides = overrides.clone();
        self.authority_paused = paused.clone();
        let mut effective = enabled.clone();
        // A project default first, then the session's own override: switching a
        // session off under a project that is on is exactly what an override is
        // for, so it has the last word.
        for session in sessions {
            if project_ids.contains(session.project_id.as_str()) {
                effective.insert(session.id.as_str().to_string());
            }
        }
        for (session_id, on) in overrides {
            if *on {
                effective.insert(session_id.clone());
            } else {
                effective.remove(session_id);
            }
        }
        // A suspension is a suspension of something that is on: a session the
        // authority does not list as enabled cannot be waiting to resume.
        let suspended = self
            .authority_paused
            .union(&self.local_paused)
            .cloned()
            .collect::<BTreeSet<_>>();
        effective.retain(|session_id| !suspended.contains(session_id));
        self.enabled = effective;
    }

    pub fn is_enabled(&self, session_id: &VibexSessionId) -> bool {
        self.enabled.contains(session_id.as_str())
    }

    pub fn is_paused(&self, session_id: &VibexSessionId) -> bool {
        self.authority_paused.contains(session_id.as_str())
            || self.local_paused.contains(session_id.as_str())
    }

    /// Whether this session has a continuation waiting out its countdown.
    pub fn is_counting_down(&self, session_id: &VibexSessionId) -> bool {
        self.countdowns.contains_key(session_id.as_str())
    }

    /// Any countdown at all, which is what keeps the interface repainting its
    /// seconds.
    pub fn any_counting_down(&self) -> bool {
        !self.countdowns.is_empty()
    }

    /// The seconds left on a countdown, rounded up: a countdown that reads `1`
    /// still has most of a second to run.
    pub fn countdown_seconds(&self, session_id: &VibexSessionId) -> Option<u8> {
        self.countdowns
            .get(session_id.as_str())
            .map(|countdown| countdown.shown_seconds)
    }

    /// Switch a session on or off, locally. The caller sends the change to the
    /// authority as well when there is one.
    pub fn set_enabled(&mut self, session_id: &VibexSessionId, enabled: bool) {
        let key = session_id.as_str().to_string();
        if enabled {
            self.enabled.insert(key.clone());
            self.authority_paused.remove(&key);
            self.local_paused.remove(&key);
            self.paused_turns.remove(&key);
            self.handled_turns.remove(&key);
        } else {
            self.enabled.remove(&key);
            self.authority_paused.remove(&key);
            self.local_paused.remove(&key);
            self.paused_turns.remove(&key);
            self.handled_turns.remove(&key);
            self.countdowns.remove(&key);
        }
        self.overrides.insert(key, enabled);
    }

    /// Stop the countdown and suspend this session until the reader resumes it
    /// or sends something. The Desktop's pause button does the same: the
    /// preference is kept, the suspension is what changes.
    pub fn pause(&mut self, session_id: &VibexSessionId, updated_at_ms: i64) {
        let key = session_id.as_str().to_string();
        if !self.enabled.contains(&key) {
            return;
        }
        self.local_paused.insert(key.clone());
        if let Some(countdown) = self.countdowns.remove(&key) {
            self.paused_turns.insert(key.clone());
            self.handled_turns.insert(key, countdown.updated_at_ms);
        }
        let _ = updated_at_ms;
    }

    /// Lift a suspension. A manual continuation is a reactivation too, which is
    /// why the Desktop clears the same state when one is sent.
    pub fn resume(&mut self, session_id: &VibexSessionId) {
        let key = session_id.as_str().to_string();
        self.authority_paused.remove(&key);
        self.local_paused.remove(&key);
        self.paused_turns.remove(&key);
        self.handled_turns.remove(&key);
    }

    /// Remember what a probe answered. An answer for a revision the session has
    /// left is dropped: the next sync asks again for the current one.
    pub fn note_status(
        &mut self,
        session_id: &VibexSessionId,
        updated_at_ms: i64,
        ended_normally: Option<bool>,
    ) {
        let key = session_id.as_str().to_string();
        self.probes.remove(&key);
        self.statuses.insert(
            key,
            TurnStatus {
                updated_at_ms,
                ended_normally,
            },
        );
    }

    /// Remember that a continuation went out, so this turn is not continued
    /// twice however long it stays idle.
    pub fn note_continued(&mut self, session_id: &VibexSessionId, updated_at_ms: i64) {
        let key = session_id.as_str().to_string();
        self.countdowns.remove(&key);
        self.handled_turns.insert(key, updated_at_ms);
    }

    /// Decide what each session needs. Called whenever the session list or an
    /// event could have changed a turn's state.
    pub fn sync(
        &mut self,
        sessions: &[AgentSession],
        now_ms: i64,
        turn_pending: impl Fn(&VibexSessionId) -> bool,
    ) -> Vec<AutoContinueAction> {
        let known = sessions
            .iter()
            .filter(|session| session.deleted_at_ms.is_none())
            .map(|session| session.id.as_str().to_string())
            .collect::<BTreeSet<_>>();
        self.statuses.retain(|id, _| known.contains(id));
        self.probes.retain(|id, _| known.contains(id));
        self.countdowns.retain(|id, _| known.contains(id));
        self.handled_turns.retain(|id, _| known.contains(id));
        self.paused_turns.retain(|id| known.contains(id));

        let mut actions = Vec::new();
        for session in sessions
            .iter()
            .filter(|session| session.deleted_at_ms.is_none())
        {
            let key = session.id.as_str().to_string();
            let resting = matches!(
                session.state,
                AgentSessionState::Idle | AgentSessionState::Error
            );
            if !resting {
                // The turn is live, so whatever was decided about the last one
                // is done with.
                self.paused_turns.remove(&key);
                self.handled_turns.remove(&key);
            }
            let status = self
                .statuses
                .get(&key)
                .filter(|status| status.updated_at_ms == session.updated_at_ms)
                .copied();
            if self.enabled.contains(&key) && resting && status.is_none() {
                // Asked and not answered: wait, then ask again. A countdown
                // cannot run on a turn nobody has judged yet — the answer may
                // be that it ended normally.
                self.countdowns.remove(&key);
                let asked = self.probes.get(&key).copied();
                let due = asked.is_none_or(|(revision, at_ms)| {
                    revision != session.updated_at_ms || now_ms - at_ms >= PROBE_RETRY_MS
                });
                if due {
                    self.probes.insert(key, (session.updated_at_ms, now_ms));
                    actions.push(AutoContinueAction::Probe {
                        session_id: session.id.clone(),
                        updated_at_ms: session.updated_at_ms,
                    });
                }
                continue;
            }
            let should_start = self.enabled.contains(&key)
                && agent_session_turn_requires_continuation(
                    session.state,
                    status.and_then(|status| status.ended_normally),
                )
                && !turn_pending(&session.id)
                && !self.paused_turns.contains(&key)
                && self.handled_turns.get(&key) != Some(&session.updated_at_ms)
                && !self.is_paused(&session.id);
            if !should_start {
                self.countdowns.remove(&key);
                continue;
            }
            if self
                .countdowns
                .get(&key)
                .is_some_and(|countdown| countdown.updated_at_ms == session.updated_at_ms)
            {
                continue;
            }
            self.countdowns.insert(
                key,
                Countdown {
                    updated_at_ms: session.updated_at_ms,
                    deadline_ms: now_ms + COUNTDOWN_MS,
                    shown_seconds: (COUNTDOWN_MS / 1_000) as u8,
                },
            );
        }
        actions
    }

    /// Move the countdowns on. Returns the continuations that are due and
    /// whether the display changed (the seconds it shows).
    pub fn tick(
        &mut self,
        sessions: &[AgentSession],
        now_ms: i64,
        turn_pending: impl Fn(&VibexSessionId) -> bool,
    ) -> (Vec<AutoContinueAction>, bool) {
        let mut actions = Vec::new();
        let mut changed = false;
        let due = self
            .countdowns
            .iter()
            .filter(|(_, countdown)| now_ms >= countdown.deadline_ms)
            .map(|(key, countdown)| (key.clone(), countdown.updated_at_ms))
            .collect::<Vec<_>>();
        for (key, updated_at_ms) in due {
            let Some(session) = sessions
                .iter()
                .find(|session| session.id.as_str() == key && session.deleted_at_ms.is_none())
            else {
                self.countdowns.remove(&key);
                changed = true;
                continue;
            };
            let still_wanted = self.enabled.contains(&key)
                && session.updated_at_ms == updated_at_ms
                && agent_session_turn_requires_continuation(
                    session.state,
                    self.statuses
                        .get(&key)
                        .filter(|status| status.updated_at_ms == updated_at_ms)
                        .and_then(|status| status.ended_normally),
                )
                && !self.paused_turns.contains(&key)
                && self.handled_turns.get(&key) != Some(&updated_at_ms)
                && !self.is_paused(session_id_of(session));
            if !still_wanted {
                self.countdowns.remove(&key);
                changed = true;
                continue;
            }
            if turn_pending(session_id_of(session)) {
                // A send is in flight; continuing now would interleave two
                // turns. The countdown holds at zero until it settles.
                continue;
            }
            self.countdowns.remove(&key);
            self.handled_turns.insert(key, updated_at_ms);
            changed = true;
            actions.push(AutoContinueAction::Continue {
                session_id: session.id.clone(),
                updated_at_ms,
            });
        }
        for countdown in self.countdowns.values_mut() {
            let remaining = ((countdown.deadline_ms - now_ms).max(0) as u64).div_ceil(1_000) as u8;
            if remaining != countdown.shown_seconds {
                countdown.shown_seconds = remaining;
                changed = true;
            }
        }
        (actions, changed)
    }
}

fn session_id_of(session: &AgentSession) -> &VibexSessionId {
    &session.id
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{AgentId, ProjectId, WorkspaceId, WorkspaceMode};

    fn session(
        id: &str,
        project: &str,
        state: AgentSessionState,
        updated_at_ms: i64,
    ) -> AgentSession {
        AgentSession {
            id: VibexSessionId::parse(id).expect("valid session id"),
            title: id.to_string(),
            project_id: ProjectId::parse(format!("project_{project}")).expect("valid project id"),
            workspace_id: WorkspaceId::new(),
            workspace_root: format!("/repo/{project}"),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id: AgentId::parse("claude").expect("valid agent id"),
            state,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: 1,
            updated_at_ms,
            last_message_at_ms: updated_at_ms,
            archived_at_ms: None,
            deleted_at_ms: None,
        }
    }

    fn enabled(ids: &[&str]) -> BTreeSet<String> {
        ids.iter().map(|id| id.to_string()).collect()
    }

    fn session_id(id: &str) -> VibexSessionId {
        VibexSessionId::parse(id).expect("valid session id")
    }

    /// The reader switched one session on; the authority hands that back with
    /// the tree.
    fn machine() -> AutoContinue {
        let mut machine = AutoContinue::default();
        machine.apply_authority(
            &BTreeSet::new(),
            &BTreeMap::new(),
            &enabled(&["session_a"]),
            &BTreeSet::new(),
            &[],
        );
        machine
    }

    #[test]
    fn a_turn_that_stopped_without_an_answer_is_continued() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        // The session list cannot say whether the turn ended normally, so the
        // first thing auto-continue does is ask.
        let actions = machine.sync(&sessions, 1_000, |_| false);
        assert_eq!(
            actions,
            vec![AutoContinueAction::Probe {
                session_id: session_id("session_a"),
                updated_at_ms: 10,
            }]
        );
        // The answer: it stopped without producing a final message.
        machine.note_status(&session_id("session_a"), 10, Some(false));
        assert!(machine.sync(&sessions, 1_010, |_| false).is_empty());
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), Some(5));

        // The countdown runs out and the continuation goes out, once.
        let (actions, changed) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS, |_| false);
        assert!(changed);
        assert_eq!(
            actions,
            vec![AutoContinueAction::Continue {
                session_id: session_id("session_a"),
                updated_at_ms: 10,
            }]
        );
        let (again, _) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS + 1_000, |_| false);
        assert!(again.is_empty(), "the same turn was continued twice");
    }

    #[test]
    fn a_turn_that_ended_normally_is_left_alone() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&sessions, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(true));
        assert!(machine.sync(&sessions, 1_010, |_| false).is_empty());
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), None);
    }

    #[test]
    fn a_resting_session_is_judged_again_only_when_it_moves() {
        // An idle session that already produced its answer must not keep
        // re-probing: that is a repaint loop with a network call in it.
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&sessions, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(false));
        machine.sync(&sessions, 1_010, |_| false);
        assert!(machine.sync(&sessions, 1_020, |_| false).is_empty());
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), Some(5));
    }

    #[test]
    fn the_countdown_can_be_stopped_before_it_fires() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&sessions, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(false));
        machine.sync(&sessions, 1_010, |_| false);
        machine.pause(&session_id("session_a"), 1_020);
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), None);

        // However long the turn stays idle, the reader's stop stands.
        let (actions, _) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS * 4, |_| false);
        assert!(actions.is_empty(), "a stopped continuation went out anyway");
        assert!(machine.sync(&sessions, 1_020, |_| false).is_empty());
        // Until the reader reactivates it — with a message, or by resuming.
        machine.resume(&session_id("session_a"));
        machine.sync(&sessions, 1_030, |_| false);
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), Some(5));
    }

    #[test]
    fn a_session_that_moved_on_is_judged_again() {
        let resting = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&resting, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(false));
        machine.sync(&resting, 1_010, |_| false);
        assert!(
            machine
                .countdown_seconds(&session_id("session_a"))
                .is_some()
        );

        // The session runs again: the countdown for the old revision is done
        // with, and its answer cannot settle the new one.
        let running = vec![session("session_a", "p", AgentSessionState::Running, 20)];
        machine.sync(&running, 1_020, |_| false);
        assert_eq!(machine.countdown_seconds(&session_id("session_a")), None);
        let actions = machine.sync(&running, 1_030, |_| false);
        assert!(actions.is_empty(), "a running turn is not judged");

        // It stops again, and this time the answer is a fresh probe.
        let resting_again = vec![session("session_a", "p", AgentSessionState::Idle, 30)];
        let actions = machine.sync(&resting_again, 1_040, |_| false);
        assert!(matches!(
            actions.as_slice(),
            [AutoContinueAction::Probe { .. }]
        ));
    }

    #[test]
    fn a_send_in_flight_holds_the_continuation() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&sessions, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(false));
        machine.sync(&sessions, 1_010, |_| false);

        // The reader pressed Enter a moment before the countdown ran out: the
        // continuation must not be sent on top of it.
        let (actions, _) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS, |_| true);
        assert!(actions.is_empty());
        // It waits rather than giving up: the turn is still unanswered.
        let (actions, _) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS + 500, |_| false);
        assert_eq!(
            actions,
            vec![AutoContinueAction::Continue {
                session_id: session_id("session_a"),
                updated_at_ms: 10,
            }]
        );
    }

    #[test]
    fn switching_a_session_off_forgets_its_countdown() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = machine();
        machine.sync(&sessions, 1_000, |_| false);
        machine.note_status(&session_id("session_a"), 10, Some(false));
        machine.sync(&sessions, 1_010, |_| false);
        machine.set_enabled(&session_id("session_a"), false);
        assert!(!machine.is_enabled(&session_id("session_a")));
        let (actions, _) = machine.tick(&sessions, 1_010 + COUNTDOWN_MS, |_| false);
        assert!(actions.is_empty());
    }

    #[test]
    fn a_project_default_continues_its_sessions() {
        let sessions = vec![
            session("session_a", "p", AgentSessionState::Idle, 10),
            session("session_b", "q", AgentSessionState::Idle, 10),
        ];
        let mut machine = AutoContinue::default();
        machine.apply_authority(
            &enabled(&["project_p"]),
            &BTreeMap::new(),
            &BTreeSet::new(),
            &BTreeSet::new(),
            &sessions,
        );
        assert!(machine.is_enabled(&session_id("session_a")));
        assert!(!machine.is_enabled(&session_id("session_b")));
    }

    #[test]
    fn an_authority_suspension_survives_and_a_session_off_beats_a_project_on() {
        let sessions = vec![session("session_a", "p", AgentSessionState::Idle, 10)];
        let mut machine = AutoContinue::default();
        machine.apply_authority(
            &enabled(&["project_p"]),
            &BTreeMap::from([("session_a".to_string(), false)]),
            &BTreeSet::new(),
            &enabled(&["session_b"]),
            &sessions,
        );
        // The override wins over the project default, and a session the
        // authority suspended is not waiting to continue.
        assert!(!machine.is_enabled(&session_id("session_a")));
        assert!(machine.is_paused(&session_id("session_b")));
    }
}

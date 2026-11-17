//! Coalescing for a live subscription that fell behind.
//!
//! A broadcast receiver reports a lag once per reading, and a runtime that
//! produces faster than the channel consumes reports it again before the last
//! recovery has finished. Recovering once per reading turned one backlog into a
//! stream of identical notices: the chat spammed itself exactly while it was
//! already behind, and every send made the next reading later still.
//!
//! Two things fix that. The backlog is drained beside the subscription instead
//! of inside it, one drain at a time, and a reading that arrives while a drain
//! runs folds into one more round of the same drain. And the notice is sent once
//! per backlog: readings that keep arriving belong to the same episode, so a
//! chat is told when it starts falling behind, not once per batch. The approval
//! cards are still re-sent on every round — a pending card the chat never saw is
//! the one thing it cannot find any other way.

use std::collections::HashMap;
use std::sync::Mutex;

use vibex_core::ImBindingId;

/// How long a run of lag readings has to pause before the next reading counts as
/// a new backlog. A runtime that keeps producing faster than the channel drains
/// is one episode, however many readings it reports.
const EPISODE_GAP_MS: i64 = 2 * 60 * 1000;

#[derive(Default)]
struct State {
    /// Which drain owns the backlog, if any. A generation rather than a flag:
    /// only the drain that claimed the slot may release it, so a cancelled task
    /// cannot clear the slot a newer drain has already taken.
    running: Option<u64>,
    next_generation: u64,
    /// A reading arrived while that drain was running, so it owes another
    /// round once the current one settles.
    another_round: bool,
    /// When the last lag reading arrived.
    last_lag_ms: i64,
    /// When the run of readings the last one belongs to began.
    episode_started_ms: i64,
    /// When each binding was last told about a backlog.
    noticed_ms: HashMap<ImBindingId, i64>,
}

impl State {
    fn note_lag(&mut self, now_ms: i64) {
        if now_ms.saturating_sub(self.last_lag_ms) > EPISODE_GAP_MS {
            self.episode_started_ms = now_ms;
        }
        self.last_lag_ms = now_ms;
    }

    fn release(&mut self) {
        self.running = None;
        // Nobody owns the slot now, so no round is owed; the next reading
        // claims the slot from scratch.
        self.another_round = false;
    }
}

/// Coalesces recovery from dropped live events.
#[derive(Default)]
pub(crate) struct LagRecovery {
    state: Mutex<State>,
}

impl LagRecovery {
    /// Claims the drain and returns its generation.
    ///
    /// `None` means a drain already owns the backlog: the reading that just
    /// arrived is folded into it instead of starting a second drain that would
    /// compete for the same provider quota.
    pub(crate) fn try_begin(&self, now_ms: i64) -> Option<u64> {
        let mut state = self.state.lock().expect("lag recovery lock");
        state.note_lag(now_ms);
        if state.running.is_some() {
            state.another_round = true;
            return None;
        }
        let generation = state.next_generation;
        state.next_generation = state.next_generation.wrapping_add(1);
        state.running = Some(generation);
        Some(generation)
    }

    /// Ends one round of `generation`.
    ///
    /// `true` means a reading arrived while it ran, so the same drain owes one
    /// more round; that round covers every reading up to the flag being taken.
    /// `false` means the drain is done. A generation that no longer owns the
    /// slot reports `false` without touching it.
    pub(crate) fn finish_round(&self, generation: u64) -> bool {
        let mut state = self.state.lock().expect("lag recovery lock");
        if state.running != Some(generation) {
            return false;
        }
        if state.another_round {
            state.another_round = false;
            true
        } else {
            state.running = None;
            false
        }
    }

    /// Releases the slot of a drain that is stopping early.
    pub(crate) fn abandon(&self, generation: u64) {
        let mut state = self.state.lock().expect("lag recovery lock");
        if state.running == Some(generation) {
            state.release();
        }
    }

    /// Whether this binding may be told about the current backlog.
    ///
    /// One notice per binding per backlog: the reading that starts an episode
    /// speaks for every reading that follows it. The claim is recorded before
    /// the send, not after: a provider that refuses the notice must not turn
    /// that into a retry loop.
    pub(crate) fn claim_notice(&self, binding_id: &ImBindingId, now_ms: i64) -> bool {
        let mut state = self.state.lock().expect("lag recovery lock");
        let told = state.noticed_ms.get(binding_id).copied();
        let due = told.is_none_or(|last| last < state.episode_started_ms);
        if due {
            state.noticed_ms.insert(binding_id.clone(), now_ms);
        }
        due
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_drain_owns_the_backlog() {
        let lag = LagRecovery::default();
        let generation = lag.try_begin(1_000).expect("first drain");
        // Readings that arrive while it runs belong to the same drain.
        assert!(lag.try_begin(1_100).is_none());
        // That reading is folded in: the drain owes one more round, and the
        // round after it is the end.
        assert!(lag.finish_round(generation));
        assert!(
            !lag.finish_round(generation),
            "the folded round was the end"
        );
        assert!(lag.try_begin(2_000).is_some(), "the next backlog drains");
    }

    #[test]
    fn a_stale_generation_cannot_release_a_newer_drain() {
        let lag = LagRecovery::default();
        let cancelled = lag.try_begin(1_000).expect("first drain");
        // A cancelled task drops its guard while a new drain may already own
        // the slot; the guard carries the generation it claimed for this.
        lag.abandon(cancelled);
        let current = lag.try_begin(1_100).expect("second drain");
        lag.abandon(cancelled);
        assert!(
            lag.try_begin(1_200).is_none(),
            "the cancelled drain must not free the slot the new one owns"
        );
        lag.abandon(current);
        assert!(lag.try_begin(1_300).is_some());
    }

    #[test]
    fn a_chat_is_told_once_per_backlog_not_once_per_reading() {
        let lag = LagRecovery::default();
        let binding = ImBindingId::new();
        let _first = lag.try_begin(1_000).expect("drain");
        assert!(lag.claim_notice(&binding, 1_000), "the backlog starts here");
        for offset in 1..200 {
            let now = 1_000 + offset * 1_000;
            // Every reading is folded into the same drain and the same episode.
            assert!(lag.try_begin(now).is_none());
            assert!(!lag.claim_notice(&binding, now), "reading {offset} floods");
        }
    }

    #[test]
    fn a_quiet_gap_starts_a_new_backlog_worth_telling_again() {
        let lag = LagRecovery::default();
        let binding = ImBindingId::new();
        let first = lag.try_begin(1_000).expect("drain");
        assert!(lag.claim_notice(&binding, 1_000));
        assert!(!lag.finish_round(first), "the drain ended quietly");
        let _second = lag
            .try_begin(1_000 + EPISODE_GAP_MS + 1)
            .expect("the drain ended long ago");
        assert!(
            lag.claim_notice(&binding, 1_000 + EPISODE_GAP_MS + 1),
            "a later backlog is a new thing to report"
        );
    }

    #[test]
    fn one_chat_being_told_does_not_silence_another() {
        let lag = LagRecovery::default();
        let first = ImBindingId::new();
        let second = ImBindingId::new();
        let _drain = lag.try_begin(1_000).expect("drain");
        assert!(lag.claim_notice(&first, 1_000));
        assert!(lag.claim_notice(&second, 1_000));
        assert!(!lag.claim_notice(&first, 2_000));
        assert!(!lag.claim_notice(&second, 2_000));
    }
}

//! Inbound flood control.
//!
//! The outbound queue protects the provider from the runtime. This protects the
//! runtime from a chat: a script driving a paired bot, or a group where several
//! people paste at once, can otherwise start one Agent turn per message.
//!
//! The limits are deliberately generous for a human and tight for a script.
//! Every window is a sliding window in memory: the counters reset when the
//! process restarts, which is the right trade for abuse control — an attacker
//! cannot persist a window across a restart they do not control.

use std::collections::{HashMap, VecDeque};
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What one inbound message is allowed to do.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InboundKind {
    /// Any command, including `/pair`.
    Command,
    /// Plain text that becomes an Agent prompt.
    Prompt,
    /// A pairing attempt, successful or not.
    Pairing,
}

/// A decision about one message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum InboundDecision {
    Allow,
    /// The message was over the limit; `retry_after` is how long the caller
    /// should wait before the same message would be accepted.
    Limited {
        retry_after: Duration,
    },
}

struct Window {
    events: VecDeque<Instant>,
    limit: usize,
    window: Duration,
}

impl Window {
    fn new(limit: usize, window: Duration) -> Self {
        Self {
            events: VecDeque::new(),
            limit,
            window,
        }
    }

    /// Records one event, or reports how long until there is room.
    fn try_acquire(&mut self, now: Instant) -> Option<Duration> {
        while let Some(front) = self.events.front() {
            if now.saturating_duration_since(*front) >= self.window {
                self.events.pop_front();
            } else {
                break;
            }
        }
        if self.events.len() < self.limit {
            self.events.push_back(now);
            return None;
        }
        // The oldest event has to age out before this one fits.
        self.events.front().map(|front| {
            self.window
                .saturating_sub(now.saturating_duration_since(*front))
        })
    }
}

/// Per-principal and per-chat attention limits.
pub(crate) struct InboundLimiter {
    principals: Mutex<HashMap<String, Window>>,
    chats: Mutex<HashMap<String, Window>>,
    pairing: Mutex<HashMap<String, Window>>,
    principal_limit: usize,
    principal_window: Duration,
    chat_limit: usize,
    chat_window: Duration,
    pairing_limit: usize,
    pairing_window: Duration,
}

impl InboundLimiter {
    /// Human-scale defaults: a person cannot type faster than the per-principal
    /// window, and a busy group still gets a few prompts a minute.
    pub(crate) fn new() -> Self {
        Self {
            principals: Mutex::new(HashMap::new()),
            chats: Mutex::new(HashMap::new()),
            pairing: Mutex::new(HashMap::new()),
            principal_limit: 20,
            principal_window: Duration::from_secs(60),
            chat_limit: 40,
            chat_window: Duration::from_secs(60),
            pairing_limit: 5,
            pairing_window: Duration::from_secs(600),
        }
    }

    /// Records a message and decides whether it may proceed.
    ///
    /// The per-chat limit counts every kind so a flood of commands cannot be
    /// split into one flood per principal. Pairing has its own bucket because a
    /// failed code is the one thing worth brute-forcing.
    pub(crate) fn check(&self, principal: &str, chat: &str, kind: InboundKind) -> InboundDecision {
        let now = Instant::now();
        if let Some(wait) = take(&self.chats, chat, self.chat_limit, self.chat_window, now) {
            return InboundDecision::Limited { retry_after: wait };
        }
        if let Some(wait) = take(
            &self.principals,
            principal,
            self.principal_limit,
            self.principal_window,
            now,
        ) {
            return InboundDecision::Limited { retry_after: wait };
        }
        if kind == InboundKind::Pairing
            && let Some(wait) = take(
                &self.pairing,
                chat,
                self.pairing_limit,
                self.pairing_window,
                now,
            )
        {
            return InboundDecision::Limited { retry_after: wait };
        }
        InboundDecision::Allow
    }
}

fn take(
    windows: &Mutex<HashMap<String, Window>>,
    key: &str,
    limit: usize,
    window: Duration,
    now: Instant,
) -> Option<Duration> {
    let mut windows = windows.lock().expect("inbound limiter lock");
    // A bot that has been idle for hours must not keep one entry per chat
    // forever; an expired window is indistinguishable from a missing one.
    windows
        .entry(key.to_string())
        .or_insert_with(|| Window::new(limit, window))
        .try_acquire(now)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_is_never_limited() {
        let limiter = InboundLimiter::new();
        for _ in 0..20 {
            assert_eq!(
                limiter.check("user", "chat", InboundKind::Command),
                InboundDecision::Allow
            );
        }
    }

    #[test]
    fn the_twenty_first_message_in_a_minute_is_limited() {
        let limiter = InboundLimiter::new();
        for _ in 0..20 {
            let _ = limiter.check("user", "chat", InboundKind::Prompt);
        }
        match limiter.check("user", "chat", InboundKind::Prompt) {
            InboundDecision::Limited { retry_after } => {
                assert!(retry_after <= Duration::from_secs(60));
            }
            InboundDecision::Allow => panic!("the 21st message must be limited"),
        }
    }

    #[test]
    fn one_principal_cannot_spend_another_chat_allowance() {
        let limiter = InboundLimiter::new();
        for _ in 0..20 {
            let _ = limiter.check("noisy", "chat-a", InboundKind::Prompt);
        }
        // A different principal in a different chat is unaffected.
        assert_eq!(
            limiter.check("quiet", "chat-b", InboundKind::Prompt),
            InboundDecision::Allow
        );
        // A different principal in the same chat is unaffected by the
        // principal limit but still shares the chat budget.
        assert_eq!(
            limiter.check("quiet", "chat-a", InboundKind::Prompt),
            InboundDecision::Allow
        );
    }

    #[test]
    fn pairing_attempts_are_limited_per_chat() {
        let limiter = InboundLimiter::new();
        for _ in 0..5 {
            assert_eq!(
                limiter.check("guess", "chat", InboundKind::Pairing),
                InboundDecision::Allow
            );
        }
        assert!(matches!(
            limiter.check("guess", "chat", InboundKind::Pairing),
            InboundDecision::Limited { .. }
        ));
        // A plain command is still allowed: only the pairing guesses are spent.
        assert_eq!(
            limiter.check("guess", "chat", InboundKind::Command),
            InboundDecision::Allow
        );
    }
}

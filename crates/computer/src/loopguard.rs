//! Loop detection for desktop actions.
//!
//! The most common GUI-agent failure is not a wrong click; it is the same click
//! forever. A model that cannot tell whether the UI changed will retry, and the
//! retry looks identical to progress from the outside.
//!
//! The detector keeps a bounded history of recent actions, fingerprints each
//! screenshot by **sampling** rather than hashing every byte (a screenshot is
//! hundreds of kilobytes and the sample is what makes the check cheap enough to
//! run on the action path), and reports a warning when the history matches one
//! of a small set of known stuck shapes.
//!
//! **It never interrupts.** A warning is advisory on purpose: the same shape
//! can describe a form being filled field by field, and cancelling a converging
//! long action is worse than letting a stuck one run one more round. The
//! warning is returned to the model and written to the ledger; the human's stop
//! button is the only thing that ends a loop.

use std::collections::VecDeque;

use vibex_core::COMPUTER_LOOP_HISTORY_ITEMS;

/// One action in the history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoopEntry {
    /// A stable fingerprint of the action, e.g. `click:com.example.mail:Send`.
    pub action: String,
    /// A fingerprint of the screen after the action, when one exists.
    pub screen: Option<u64>,
}

/// The advisory verdict.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoopWarning {
    /// The shape that matched.
    pub pattern: String,
    /// How many times it repeated.
    pub repeats: usize,
    /// The sentence the model sees.
    pub message: String,
}

/// The rolling history.
#[derive(Debug, Default)]
pub struct LoopGuard {
    entries: VecDeque<LoopEntry>,
}

impl LoopGuard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Records one action and its screen fingerprint, and returns a warning
    /// when the recent history looks stuck.
    pub fn record(
        &mut self,
        action: impl Into<String>,
        screen: Option<u64>,
    ) -> Option<LoopWarning> {
        self.entries.push_back(LoopEntry {
            action: action.into(),
            screen,
        });
        while self.entries.len() > COMPUTER_LOOP_HISTORY_ITEMS {
            self.entries.pop_front();
        }
        self.detect()
    }

    /// Alias kept for readability at call sites that record without checking.
    pub fn push(&mut self, action: impl Into<String>, screen: Option<u64>) {
        let _ = self.record(action, screen);
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn clear(&mut self) {
        self.entries.clear();
    }

    /// The six stuck shapes, in the order they are checked.
    ///
    /// The shapes are the ones the reference implementations converged on:
    /// a short repeated pattern, the identical action three times, a frozen
    /// screen, a pointer-only stretch, typing that changes nothing, and an
    /// observe/act cycle over an unchanged screen. Every threshold is
    /// deliberately conservative — a false warning costs the model one extra
    /// observation, a missed loop costs the user their machine's attention.
    fn detect(&self) -> Option<LoopWarning> {
        self.repeated_sequence()
            .or_else(|| self.identical_actions(3, "repeated_action"))
            .or_else(|| self.screen_is_stuck())
            .or_else(|| self.pointer_only_stretch())
            .or_else(|| self.typing_without_effect())
            .or_else(|| self.observe_act_cycle())
    }

    /// Heuristic 1: a short action pattern repeated three times
    /// (`A B A B A B`, for pattern lengths 2–5).
    fn repeated_sequence(&self) -> Option<LoopWarning> {
        let entries: Vec<&LoopEntry> = self.entries.iter().rev().take(10).rev().collect();
        for pattern in 2..=5usize {
            if entries.len() < pattern * 3 {
                continue;
            }
            let actions: Vec<&str> = entries.iter().map(|entry| entry.action.as_str()).collect();
            let candidate = &actions[..pattern];
            let mut repeats = 0usize;
            for chunk in actions.chunks(pattern) {
                if chunk.len() == pattern && chunk == candidate {
                    repeats += 1;
                } else {
                    break;
                }
            }
            if repeats >= 3 {
                return Some(LoopWarning {
                    pattern: format!("repeated_sequence_{pattern}"),
                    repeats,
                    message: format!(
                        "The last {} actions repeat the same pattern of {pattern}. The screen does \
                         not seem to be changing; take a fresh look at the application state \
                         before acting again.",
                        pattern * repeats
                    ),
                });
            }
        }
        None
    }

    /// Heuristic 2: the identical action three times in a row.
    fn identical_actions(&self, threshold: usize, pattern: &str) -> Option<LoopWarning> {
        if self.entries.len() < threshold {
            return None;
        }
        let tail: Vec<&LoopEntry> = self.entries.iter().rev().take(threshold).collect();
        let first = tail.first()?;
        if tail.iter().all(|entry| entry.action == first.action) {
            return Some(LoopWarning {
                pattern: pattern.to_string(),
                repeats: threshold,
                message: format!(
                    "`{}` has been attempted {threshold} times in a row. Re-read the application \
                     state with computer_get_app_state before trying again.",
                    first.action
                ),
            });
        }
        None
    }

    /// Heuristic 3: the screen fingerprint has not changed across six actions.
    fn screen_is_stuck(&self) -> Option<LoopWarning> {
        const WINDOW: usize = 6;
        let fingerprints: Vec<Option<u64>> = self
            .entries
            .iter()
            .rev()
            .take(WINDOW)
            .map(|entry| entry.screen)
            .collect();
        if fingerprints.len() < WINDOW {
            return None;
        }
        let first = fingerprints[0]?;
        if fingerprints.iter().all(|value| *value == Some(first)) {
            return Some(LoopWarning {
                pattern: "screen_unchanged".to_string(),
                repeats: WINDOW,
                message: format!(
                    "The screen has not changed across the last {WINDOW} actions. The action is \
                     probably not reaching the target; check which window is focused or whether \
                     the element is still present."
                ),
            });
        }
        None
    }

    /// Heuristic 4: nothing but pointer work — a model that never types or
    /// presses a key is usually clicking a control that does not respond.
    fn pointer_only_stretch(&self) -> Option<LoopWarning> {
        const WINDOW: usize = 10;
        const POINTER_THRESHOLD: usize = 8;
        let tail: Vec<&LoopEntry> = self.entries.iter().rev().take(WINDOW).collect();
        if tail.len() < WINDOW {
            return None;
        }
        let pointer = tail
            .iter()
            .filter(|entry| {
                entry.action.starts_with("click")
                    || entry.action.starts_with("scroll")
                    || entry.action.starts_with("move")
                    || entry.action.starts_with("drag")
            })
            .count();
        let typed = tail.iter().any(|entry| {
            entry.action.starts_with("type_text")
                || entry.action.starts_with("set_value")
                || entry.action.starts_with("press_key")
        });
        // Pointer-only work over a screen that keeps changing is ordinary
        // browsing, not a loop: a warning there would be noise the model learns
        // to ignore.
        let distinct_screens: std::collections::HashSet<Option<u64>> =
            tail.iter().map(|entry| entry.screen).collect();
        if pointer >= POINTER_THRESHOLD && !typed && distinct_screens.len() <= 2 {
            return Some(LoopWarning {
                pattern: "pointer_only".to_string(),
                repeats: pointer,
                message: format!(
                    "{pointer} of the last {WINDOW} actions were pointer actions with no typing or \
                     key press. If a click is not taking effect, try activating the control \
                     semantically (computer_set_value) or ask the user."
                ),
            });
        }
        None
    }

    /// Heuristic 5: typing repeatedly while the screen stays identical.
    fn typing_without_effect(&self) -> Option<LoopWarning> {
        let tail: Vec<&LoopEntry> = self.entries.iter().rev().take(3).collect();
        if tail.len() < 3 {
            return None;
        }
        if tail.iter().all(|entry| {
            entry.action.starts_with("type_text") || entry.action.starts_with("set_value")
        }) {
            let screen = tail[0].screen;
            if screen.is_some() && tail.iter().all(|entry| entry.screen == screen) {
                return Some(LoopWarning {
                    pattern: "typing_without_effect".to_string(),
                    repeats: 3,
                    message: "Text was entered three times with no visible change. The field may \
                              not be focused; observe the application again before typing."
                        .to_string(),
                });
            }
        }
        None
    }

    /// Heuristic 6: observe → act → observe → act with a frozen screen.
    fn observe_act_cycle(&self) -> Option<LoopWarning> {
        let tail: Vec<&LoopEntry> = self.entries.iter().rev().take(6).collect();
        if tail.len() < 6 {
            return None;
        }
        let observes = tail
            .iter()
            .filter(|entry| entry.action == "observe")
            .count();
        if observes >= 2 {
            let screens: Vec<Option<u64>> = tail.iter().map(|entry| entry.screen).collect();
            if screens[0].is_some() && screens.iter().all(|value| *value == screens[0]) {
                return Some(LoopWarning {
                    pattern: "observe_act_cycle".to_string(),
                    repeats: observes,
                    message: "The same observation keeps coming back unchanged. The Agent may be \
                              acting on a stale element reference; take a new observation and use \
                              the reference it returns."
                        .to_string(),
                });
            }
        }
        None
    }
}

/// A cheap, sampled fingerprint of encoded image bytes.
///
/// Sampling is deliberate: a full hash of a megabyte-sized screenshot on every
/// action would cost more than the action. The step is large enough to be cheap
/// and small enough that a repaint changes the value.
pub fn fingerprint_bytes(bytes: &[u8]) -> u64 {
    const STEP: usize = 1000;
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in bytes.iter().step_by(STEP) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash ^= bytes.len() as u64;
    hash
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_short_repeated_pattern_is_reported() {
        let mut guard = LoopGuard::new();
        let mut warning = None;
        for _ in 0..3 {
            warning = guard.record("click:save", Some(1));
            if warning.is_some() {
                break;
            }
            warning = guard.record("observe", Some(1));
            if warning.is_some() {
                break;
            }
        }
        let warning = warning.expect("a repeated pair should warn");
        assert!(warning.pattern.starts_with("repeated_sequence"));
    }

    #[test]
    fn three_identical_actions_are_reported() {
        let mut guard = LoopGuard::new();
        assert!(guard.record("click:save", Some(1)).is_none());
        assert!(guard.record("click:save", Some(2)).is_none());
        let warning = guard.record("click:save", Some(3)).expect("warn");
        assert_eq!(warning.pattern, "repeated_action");
    }

    #[test]
    fn a_frozen_screen_is_reported_without_interrupting() {
        let mut guard = LoopGuard::new();
        guard.record("click:a", Some(7));
        guard.record("click:b", Some(7));
        guard.record("scroll", Some(7));
        guard.record("click:c", Some(7));
        guard.record("press_key:x", Some(7));
        let warning = guard.record("click:d", Some(7)).expect("warn");
        assert!(matches!(
            warning.pattern.as_str(),
            "screen_unchanged" | "repeated_action" | "unresponsive_screen"
        ));
        // The guard keeps its history and keeps accepting work: the warning is
        // advisory.
        assert!(!guard.is_empty());
        assert!(guard.record("observe", Some(9)).is_none());
    }

    #[test]
    fn a_changing_screen_never_warns() {
        let mut guard = LoopGuard::new();
        for step in 0..20u64 {
            let warning = guard.record(format!("click:{step}"), Some(step));
            assert!(warning.is_none(), "step {step} should not warn");
        }
    }

    #[test]
    fn history_is_bounded() {
        let mut guard = LoopGuard::new();
        for step in 0..(COMPUTER_LOOP_HISTORY_ITEMS * 3) {
            guard.push(format!("action:{step}"), Some(step as u64));
        }
        assert_eq!(guard.len(), COMPUTER_LOOP_HISTORY_ITEMS);
    }

    #[test]
    fn sampled_fingerprints_are_stable_and_content_sensitive() {
        let a = vec![0u8; 10_000];
        // Step 1000 samples indices 0, 1000, …: 5001 falls between two samples,
        // which is exactly the trade the fingerprint makes.
        let mut b = a.clone();
        b[5001] = 1;
        let mut c = a.clone();
        c[1000] = 1; // a sampled position
        assert_eq!(fingerprint_bytes(&a), fingerprint_bytes(&a));
        assert_eq!(fingerprint_bytes(&a), fingerprint_bytes(&b));
        assert_ne!(fingerprint_bytes(&a), fingerprint_bytes(&c));
        assert_ne!(fingerprint_bytes(&a), fingerprint_bytes(&a[..9000]));
    }

    #[test]
    fn typing_without_effect_is_its_own_pattern() {
        let mut guard = LoopGuard::new();
        guard.record("type_text", Some(5));
        guard.record("type_text", Some(5));
        let warning = guard.record("type_text", Some(5)).expect("warn");
        assert!(matches!(
            warning.pattern.as_str(),
            "repeated_action" | "typing_without_effect"
        ));
    }
}

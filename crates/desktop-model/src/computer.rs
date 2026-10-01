//! Persisted computer-use preferences.
//!
//! These are the user's decisions, not the runtime's state: whether the feature
//! may run at all, how much approval the Agent needs, whether it may drive
//! Vibex's own window, and how long one desktop call may take. They live in the
//! desktop UI state because they are edited in the settings and read at startup
//! — the runtime is handed the answer rather than owning it.
//!
//! Everything here has a conservative default. The feature ships **off**, the
//! policy ships as "ask", and driving Vibex's own window ships disabled,
//! because a default that an upgrade could flip on is exactly the failure this
//! file exists to prevent.

use serde::{Deserialize, Serialize};
use vibex_core::{
    COMPUTER_CALL_TIMEOUT_CHOICES_MS, COMPUTER_CALL_TIMEOUT_DEFAULT_MS, ComputerApprovalPolicy,
    normalize_call_timeout_ms,
};

/// The four decisions the settings own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUiState {
    /// Whether the feature may start at all. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// How much approval the Agent needs. Default "ask".
    #[serde(default)]
    pub approval_policy: ComputerApprovalPolicy,
    /// Whether the Agent may operate Vibex's own window. Default `false`.
    ///
    /// This is a separate switch rather than part of the policy because the
    /// failure it prevents is different in kind: an Agent driving its own
    /// window is a feedback loop, not an over-permissioned action.
    #[serde(default)]
    pub allow_self_target: bool,
    /// How long one desktop call may take, in milliseconds.
    #[serde(default = "default_call_timeout_ms")]
    pub call_timeout_ms: u64,
}

fn default_call_timeout_ms() -> u64 {
    COMPUTER_CALL_TIMEOUT_DEFAULT_MS
}

impl Default for ComputerUiState {
    fn default() -> Self {
        Self {
            enabled: false,
            approval_policy: ComputerApprovalPolicy::Ask,
            allow_self_target: false,
            call_timeout_ms: COMPUTER_CALL_TIMEOUT_DEFAULT_MS,
        }
    }
}

impl ComputerUiState {
    /// Bounds a value read from disk onto the offered choices.
    ///
    /// A hand-edited file must not be able to set an unlimited timeout or an
    /// unknown policy: the settings UI would then show a state it cannot
    /// represent and the runtime would apply something the user never chose.
    pub fn normalize(&mut self) {
        self.call_timeout_ms = normalize_call_timeout_ms(self.call_timeout_ms);
        if !COMPUTER_CALL_TIMEOUT_CHOICES_MS.contains(&self.call_timeout_ms) {
            self.call_timeout_ms = COMPUTER_CALL_TIMEOUT_DEFAULT_MS;
        }
    }

    /// The policy label the settings show.
    pub fn policy_label(self) -> &'static str {
        match self.approval_policy {
            ComputerApprovalPolicy::Allow => "allow",
            ComputerApprovalPolicy::Ask => "ask",
            ComputerApprovalPolicy::Deny => "deny",
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_defaults_are_conservative() {
        let state = ComputerUiState::default();
        assert!(!state.enabled, "computer use must ship off");
        assert_eq!(state.approval_policy, ComputerApprovalPolicy::Ask);
        assert!(!state.allow_self_target);
        assert_eq!(state.call_timeout_ms, COMPUTER_CALL_TIMEOUT_DEFAULT_MS);
    }

    #[test]
    fn a_file_without_the_field_reads_as_the_defaults() {
        let state: ComputerUiState = serde_json::from_str("{}").unwrap();
        assert_eq!(state, ComputerUiState::default());
    }

    #[test]
    fn an_unknown_policy_is_rejected_rather_than_guessed() {
        let parsed = serde_json::from_str::<ComputerUiState>(r#"{"approvalPolicy":"yolo"}"#);
        assert!(parsed.is_err(), "an unknown policy must not be guessed at");
    }

    #[test]
    fn a_hand_edited_timeout_snaps_onto_an_offered_choice() {
        let mut state = ComputerUiState {
            call_timeout_ms: 7,
            ..ComputerUiState::default()
        };
        state.normalize();
        assert_eq!(state.call_timeout_ms, 30_000);
        let mut state = ComputerUiState {
            call_timeout_ms: 999_999,
            ..ComputerUiState::default()
        };
        state.normalize();
        assert_eq!(state.call_timeout_ms, COMPUTER_CALL_TIMEOUT_DEFAULT_MS);
    }

    #[test]
    fn the_state_round_trips_through_json() {
        let state = ComputerUiState {
            enabled: true,
            approval_policy: ComputerApprovalPolicy::Allow,
            allow_self_target: true,
            call_timeout_ms: 120_000,
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: ComputerUiState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(state.policy_label(), "allow");
    }
}

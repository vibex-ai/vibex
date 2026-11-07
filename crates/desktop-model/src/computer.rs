//! Persisted computer-use preferences.
//!
//! These are the user's decisions, not the runtime's state: whether the feature
//! may run at all, how much approval the Agent needs, whether it may drive
//! Vibex's own window, whether the floating panel opens when an Agent starts,
//! how long one desktop call may take, and whether the driver's experimental
//! Wayland backend is used. They live in the desktop UI state because they are
//! edited in the settings and read at startup — the runtime is handed the
//! answer rather than owning it.
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

/// Which shell the computer-use panel opens in.
///
/// The panel is the human's half of the feature, so how it is hosted is a
/// persisted decision rather than a per-open one: a reader who wants the desktop
/// in the corner of their screen should get that every time, not have to ask
/// again.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub enum ComputerPanelMode {
    /// A normal, resizable window.
    #[default]
    Window,
    /// A small window that stays above other windows where the platform
    /// supports it, for keeping the desktop in view while working elsewhere.
    Floating,
}

impl ComputerPanelMode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Window => "window",
            Self::Floating => "floating",
        }
    }

    pub fn is_floating(self) -> bool {
        matches!(self, Self::Floating)
    }
}

/// The decisions the settings own.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ComputerUiState {
    /// Whether the feature may start at all. Default `false`.
    #[serde(default)]
    pub enabled: bool,
    /// Which shell the panel opens in. Default [`ComputerPanelMode::Window`].
    #[serde(default)]
    pub panel_mode: ComputerPanelMode,
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
    /// Whether the floating panel opens by itself when an Agent starts using
    /// computer use. Default `true`.
    ///
    /// The panel is the human's half of the feature: it shows what the Agent is
    /// doing and carries the emergency stop. An Agent acting on the real
    /// desktop should not begin unseen, so the default is on. It is still the
    /// reader's decision, and a closed panel stays closed for the rest of that
    /// Agent session so the window never fights the reader.
    #[serde(default = "default_auto_open_panel")]
    pub auto_open_panel: bool,
    /// How long one desktop call may take, in milliseconds.
    #[serde(default = "default_call_timeout_ms")]
    pub call_timeout_ms: u64,
    /// Whether the engine's experimental Wayland backend is on.
    ///
    /// `None` — the default, and the right answer on every platform that is
    /// not Wayland — means "decide from the machine": the runtime measures
    /// whether this session has native Wayland windows the driver cannot see
    /// without its experimental backend, and turns it on when it does. A
    /// concrete value is the reader's own decision and is never overridden.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wayland_opt_in: Option<bool>,
}

fn default_call_timeout_ms() -> u64 {
    COMPUTER_CALL_TIMEOUT_DEFAULT_MS
}

fn default_auto_open_panel() -> bool {
    true
}

impl Default for ComputerUiState {
    fn default() -> Self {
        Self {
            enabled: false,
            panel_mode: ComputerPanelMode::Window,
            approval_policy: ComputerApprovalPolicy::Ask,
            allow_self_target: false,
            auto_open_panel: true,
            call_timeout_ms: COMPUTER_CALL_TIMEOUT_DEFAULT_MS,
            wayland_opt_in: None,
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
    fn the_panel_auto_opens_by_default_and_survives_a_missing_field() {
        // Auto-open is a convenience, not a security decision: it does not
        // grant the Agent anything, and the emergency stop is more useful when
        // the panel is already on screen. A file written before the switch
        // existed must still get it.
        assert!(ComputerUiState::default().auto_open_panel);
        let from_disk: ComputerUiState = serde_json::from_str("{}").unwrap();
        assert!(from_disk.auto_open_panel);
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
            panel_mode: ComputerPanelMode::Floating,
            approval_policy: ComputerApprovalPolicy::Allow,
            allow_self_target: true,
            auto_open_panel: false,
            call_timeout_ms: 120_000,
            wayland_opt_in: Some(false),
        };
        let encoded = serde_json::to_string(&state).unwrap();
        let decoded: ComputerUiState = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, state);
        assert_eq!(state.policy_label(), "allow");
        assert_eq!(state.panel_mode.as_str(), "floating");
        assert!(state.panel_mode.is_floating());
        assert!(!decoded.auto_open_panel);
    }

    #[test]
    fn the_panel_shell_defaults_to_a_normal_window_and_survives_a_missing_field() {
        let state = ComputerUiState::default();
        assert_eq!(state.panel_mode, ComputerPanelMode::Window);
        assert!(!state.panel_mode.is_floating());
        let from_disk: ComputerUiState = serde_json::from_str("{}").unwrap();
        assert_eq!(from_disk.panel_mode, ComputerPanelMode::Window);
    }

    #[test]
    fn automatic_is_the_default_and_is_not_written_to_disk() {
        // Automatic is the absence of a decision, so a state that has one has
        // to survive a round trip and a state that does not must not grow one.
        assert_eq!(ComputerUiState::default().wayland_opt_in, None);
        let automatic = serde_json::to_string(&ComputerUiState::default()).unwrap();
        assert!(!automatic.contains("waylandOptIn"), "{automatic}");
        let forced = serde_json::to_string(&ComputerUiState {
            wayland_opt_in: Some(true),
            ..ComputerUiState::default()
        })
        .unwrap();
        assert!(forced.contains("\"waylandOptIn\":true"), "{forced}");
        let decoded: ComputerUiState = serde_json::from_str(&forced).unwrap();
        assert_eq!(decoded.wayland_opt_in, Some(true));
    }
}

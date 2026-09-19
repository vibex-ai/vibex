//! Provider-neutral goal state for Agents that expose a goal/objective mode.
//!
//! Several ACP Agents grew a "keep pursuing this objective" surface: the
//! provider-neutral `_meta.goal` extension (claude-agent-acp 0.66+,
//! codex-acp 1.2+) and Codex's legacy `_meta.codex.goal` namespace. The
//! vocabulary here is the superset Vibex normalizes those dialects into, so
//! the runtime, the manager and every client render one shape regardless of
//! which Agent produced it.
//!
//! Goal state is deliberately **event-sourced**: a provider publishes
//! snapshots, and the product derives the current goal by folding the
//! `TimelinePayload::Goal` items. Nothing here is stored on `AgentSession`,
//! which keeps the session state machine orthogonal to goal state.

use serde::{Deserialize, Serialize};

/// Where the provider stands on the session's goal.
///
/// Serialized names are the product vocabulary, not any single provider's:
/// ACP adapters normalize `budgetLimited`/`usage_limited`/`budget limited`
/// into these values before they reach a client.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalPhase {
    Active,
    Paused,
    Blocked,
    UsageLimited,
    BudgetLimited,
    Complete,
    /// The provider reported a status this build does not know. Kept distinct
    /// from the known phases so the UI can stay honest instead of guessing.
    #[default]
    Unknown,
}

impl GoalPhase {
    /// Whether the goal can no longer be resumed in place. Replacing a
    /// terminal goal starts fresh accounting, so the UI must ask before it
    /// overwrites one.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::BudgetLimited)
    }

    /// Whether a provider-side continuation may still be running.
    pub const fn is_open(self) -> bool {
        matches!(self, Self::Active | Self::Unknown)
    }

    /// Parse a provider status token leniently.
    ///
    /// Accepts the camelCase Codex spelling (`budgetLimited`), the
    /// whitespace/hyphen variants older bridges emitted (`budget limited`),
    /// and the snake_case product spelling. Unrecognized values become
    /// [`GoalPhase::Unknown`] rather than failing the whole event: a goal
    /// status is presentation data, not a protocol fence.
    pub fn parse_wire(value: &str) -> Self {
        let normalized = normalize_wire_token(value);
        match normalized.as_str() {
            "active" | "in_progress" | "inprogress" | "running" => Self::Active,
            "paused" | "pause" => Self::Paused,
            "blocked" | "stalled" => Self::Blocked,
            "usage_limited" | "usagelimited" | "rate_limited" | "ratelimited" => Self::UsageLimited,
            "budget_limited" | "budgetlimited" | "limited" => Self::BudgetLimited,
            "complete" | "completed" | "done" => Self::Complete,
            _ => Self::Unknown,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Paused => "paused",
            Self::Blocked => "blocked",
            Self::UsageLimited => "usage_limited",
            Self::BudgetLimited => "budget_limited",
            Self::Complete => "complete",
            Self::Unknown => "unknown",
        }
    }
}

/// Process-local continuation eligibility reported by providers that keep the
/// "is the goal allowed to keep running" bit outside the durable state
/// (DeepSeek Harness `armed`/`disarmed`). Providers without that concept leave
/// it absent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalActivation {
    Armed,
    Disarmed,
}

impl GoalActivation {
    pub fn parse_wire(value: &str) -> Option<Self> {
        match normalize_wire_token(value).as_str() {
            "armed" => Some(Self::Armed),
            "disarmed" => Some(Self::Disarmed),
            _ => None,
        }
    }
}

/// Durable explanation attached to a blocked goal.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalBlockedReason {
    pub code: String,
    pub message: String,
}

/// The goal-control verbs a client may send. Adapters advertise which of these
/// they actually implement; a control is never offered from a hard-coded
/// assumption.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalAction {
    Set,
    Edit,
    Pause,
    Resume,
    Clear,
}

impl GoalAction {
    /// Parse an adapter-advertised or user-requested action token.
    pub fn parse_wire(value: &str) -> Option<Self> {
        match normalize_wire_token(value).as_str() {
            "set" => Some(Self::Set),
            "edit" => Some(Self::Edit),
            "pause" => Some(Self::Pause),
            "resume" => Some(Self::Resume),
            "clear" => Some(Self::Clear),
            _ => None,
        }
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Set => "set",
            Self::Edit => "edit",
            Self::Pause => "pause",
            Self::Resume => "resume",
            Self::Clear => "clear",
        }
    }
}

/// One normalized goal snapshot.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalSnapshot {
    pub objective: String,
    pub phase: GoalPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked_reason: Option<GoalBlockedReason>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_used: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_budget: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time_used_seconds: Option<i64>,
    /// Continuation rounds started toward this goal, when the provider counts
    /// them (DeepSeek Harness). Codex's thread goal does not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rounds_started: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rounds: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub activation: Option<GoalActivation>,
}

impl GoalSnapshot {
    pub fn new(objective: impl Into<String>, phase: GoalPhase) -> Self {
        Self {
            objective: objective.into(),
            phase,
            goal_id: None,
            revision: None,
            blocked_reason: None,
            tokens_used: None,
            token_budget: None,
            time_used_seconds: None,
            rounds_started: None,
            max_rounds: None,
            activation: None,
        }
    }

    /// Remaining budget when both sides are known and the budget is positive.
    pub fn remaining_tokens(&self) -> Option<i64> {
        let budget = self.token_budget?;
        let used = self.tokens_used?;
        Some(budget.saturating_sub(used))
    }
}

/// What one timeline goal item records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GoalChangeKind {
    /// The provider published a goal snapshot (including phase changes).
    Snapshot,
    /// The provider cleared the goal.
    Cleared,
    /// Vibex sent a control request to the provider.
    ControlRequested,
    /// Vibex's control request failed before it landed.
    ControlFailed,
}

/// One goal timeline payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GoalPayload {
    pub change: GoalChangeKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub goal: Option<GoalSnapshot>,
    /// The control vocabulary the live adapter advertised for this session.
    /// Empty means "unknown": clients must not offer controls.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<GoalAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub action: Option<GoalAction>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

impl GoalPayload {
    pub fn snapshot(goal: GoalSnapshot, actions: Vec<GoalAction>) -> Self {
        Self {
            change: GoalChangeKind::Snapshot,
            goal: Some(goal),
            actions,
            action: None,
            message: None,
        }
    }

    pub fn cleared(actions: Vec<GoalAction>) -> Self {
        Self {
            change: GoalChangeKind::Cleared,
            goal: None,
            actions,
            action: None,
            message: None,
        }
    }

    pub fn control_requested(action: GoalAction) -> Self {
        Self {
            change: GoalChangeKind::ControlRequested,
            goal: None,
            actions: Vec::new(),
            action: Some(action),
            message: None,
        }
    }

    pub fn control_failed(action: GoalAction, message: impl Into<String>) -> Self {
        Self {
            change: GoalChangeKind::ControlFailed,
            goal: None,
            actions: Vec::new(),
            action: Some(action),
            message: Some(message.into()),
        }
    }
}

/// How far this Agent's goal surface reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GoalSupport {
    /// The Agent has no goal surface (or has not proven one).
    #[default]
    None,
    /// Goal state can be observed but not controlled.
    Observe,
    /// Goal state can be observed and controlled through `actions`.
    Control,
}

/// Goal capability advertised by a provider profile.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GoalCapability {
    #[serde(default)]
    pub support: GoalSupport,
    /// Concrete control verbs the live adapter implements. Empty means the
    /// client must not render controls, even when `support` is `Control`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub actions: Vec<GoalAction>,
    /// Diagnostic: the wire method controls are sent to, when one is known.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub control_method: Option<String>,
}

impl GoalCapability {
    pub const fn unsupported() -> Self {
        Self {
            support: GoalSupport::None,
            actions: Vec::new(),
            control_method: None,
        }
    }

    pub fn observe() -> Self {
        Self {
            support: GoalSupport::Observe,
            actions: Vec::new(),
            control_method: None,
        }
    }

    pub fn control(actions: Vec<GoalAction>, control_method: Option<String>) -> Self {
        Self {
            support: GoalSupport::Control,
            actions,
            control_method,
        }
    }

    pub fn supports_control(&self, action: GoalAction) -> bool {
        self.support == GoalSupport::Control && self.actions.contains(&action)
    }
}

/// Normalize a provider token: camelCase/kebab/spaces all fold to snake_case.
fn normalize_wire_token(value: &str) -> String {
    let trimmed = value.trim();
    let mut out = String::with_capacity(trimmed.len() + 2);
    for (index, ch) in trimmed.chars().enumerate() {
        if ch.is_ascii_uppercase() {
            if index != 0 && !out.ends_with('_') {
                out.push('_');
            }
            out.push(ch.to_ascii_lowercase());
        } else if ch.is_whitespace() || ch == '-' || ch == '.' {
            if !out.is_empty() && !out.ends_with('_') {
                out.push('_');
            }
        } else {
            out.push(ch.to_ascii_lowercase());
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn goal_phase_parses_provider_spellings() {
        assert_eq!(GoalPhase::parse_wire("active"), GoalPhase::Active);
        assert_eq!(
            GoalPhase::parse_wire("budgetLimited"),
            GoalPhase::BudgetLimited
        );
        assert_eq!(
            GoalPhase::parse_wire("budget limited"),
            GoalPhase::BudgetLimited
        );
        assert_eq!(
            GoalPhase::parse_wire("usage-limited"),
            GoalPhase::UsageLimited
        );
        assert_eq!(GoalPhase::parse_wire("complete"), GoalPhase::Complete);
        assert_eq!(GoalPhase::parse_wire("stalled"), GoalPhase::Blocked);
        assert_eq!(GoalPhase::parse_wire("something-new"), GoalPhase::Unknown);
        assert_eq!(GoalPhase::parse_wire(""), GoalPhase::Unknown);
    }

    #[test]
    fn terminal_phases_are_complete_and_budget_limited() {
        assert!(GoalPhase::Complete.is_terminal());
        assert!(GoalPhase::BudgetLimited.is_terminal());
        assert!(!GoalPhase::Active.is_terminal());
        assert!(!GoalPhase::Paused.is_terminal());
        assert!(!GoalPhase::Blocked.is_terminal());
        assert!(!GoalPhase::UsageLimited.is_terminal());
        assert!(!GoalPhase::Unknown.is_terminal());
    }

    #[test]
    fn goal_action_roundtrips_wire_tokens() {
        for action in [
            GoalAction::Set,
            GoalAction::Edit,
            GoalAction::Pause,
            GoalAction::Resume,
            GoalAction::Clear,
        ] {
            assert_eq!(GoalAction::parse_wire(action.as_str()), Some(action));
        }
        assert_eq!(GoalAction::parse_wire("RESUME"), Some(GoalAction::Resume));
        assert_eq!(GoalAction::parse_wire("delete"), None);
    }

    #[test]
    fn goal_payload_serializes_without_empty_optional_fields() {
        let payload = GoalPayload::snapshot(
            GoalSnapshot::new("ship the feature", GoalPhase::Active),
            vec![GoalAction::Pause, GoalAction::Clear],
        );
        let json = serde_json::to_value(&payload).unwrap();
        assert_eq!(json["change"], "snapshot");
        assert_eq!(json["goal"]["phase"], "active");
        assert_eq!(json["actions"][0], "pause");
        assert!(json.get("action").is_none());
        assert!(json.get("message").is_none());

        let cleared = serde_json::to_value(GoalPayload::cleared(Vec::new())).unwrap();
        assert_eq!(cleared["change"], "cleared");
        assert!(cleared.get("goal").is_none());
        assert!(cleared.get("actions").is_none());
    }

    #[test]
    fn remaining_tokens_needs_both_sides() {
        let mut goal = GoalSnapshot::new("x", GoalPhase::Active);
        assert_eq!(goal.remaining_tokens(), None);
        goal.token_budget = Some(100);
        assert_eq!(goal.remaining_tokens(), None);
        goal.tokens_used = Some(30);
        assert_eq!(goal.remaining_tokens(), Some(70));
        goal.tokens_used = Some(140);
        assert_eq!(goal.remaining_tokens(), Some(0));
    }

    #[test]
    fn goal_capability_gates_controls_on_the_advertised_vocabulary() {
        let capability = GoalCapability::control(
            vec![GoalAction::Pause, GoalAction::Clear],
            Some("_session/goal".to_string()),
        );
        assert!(capability.supports_control(GoalAction::Pause));
        assert!(!capability.supports_control(GoalAction::Resume));
        assert!(!GoalCapability::observe().supports_control(GoalAction::Pause));
        assert!(!GoalCapability::unsupported().supports_control(GoalAction::Clear));
    }
}

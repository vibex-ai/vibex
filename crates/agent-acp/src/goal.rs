//! ACP goal channel: per-Agent dialect knowledge plus normalization.
//!
//! Two dialects exist in the field:
//!
//! - The **provider-neutral** extension (claude-agent-acp 0.66+, codex-acp
//!   1.2+) advertised at `initialize` as `_meta.goal` with a `version`, an
//!   optional `controlMethod` and an `actions` vocabulary. Goal snapshots then
//!   ride `session_info_update._meta.goal`.
//! - Codex's **legacy** namespace (codex-acp 1.1.x): goal snapshots ride
//!   `session_info_update._meta.codex.goal` and controls go to
//!   `_codex/session/goal_control`. Still accepted by newer adapters.
//!
//! Agents with neither stay unsupported: this module never guesses a goal
//! channel from text or tool names.

use serde_json::Value;
use vibex_core::{GoalAction, GoalActivation, GoalBlockedReason, GoalPhase, GoalSnapshot};

/// The goal wire dialect an Agent speaks.
///
/// The provider-neutral `_meta.goal` advertisement is honored for every Agent:
/// it is a negotiated capability, not an identity guess. The dialect only
/// decides whether the legacy Codex namespace/method is available when that
/// advertisement is missing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct GoalDialect {
    /// Whether `_meta.codex.goal` / `_codex/session/goal_control` may be used
    /// as a fallback (codex-acp before the neutral extension).
    pub legacy_codex: bool,
}

impl GoalDialect {
    pub const fn unsupported() -> Self {
        Self {
            legacy_codex: false,
        }
    }

    /// Provider-neutral extension only (claude-agent-acp).
    pub const fn neutral() -> Self {
        Self {
            legacy_codex: false,
        }
    }

    /// Provider-neutral extension with the Codex legacy fallback (codex-acp).
    pub const fn codex() -> Self {
        Self { legacy_codex: true }
    }
}

pub(crate) const NEUTRAL_GOAL_CONTROL_METHOD: &str = "_session/goal";
pub(crate) const LEGACY_GOAL_CONTROL_METHOD: &str = "_codex/session/goal_control";

/// The resolved goal channel of one live ACP activation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AcpGoalChannel {
    /// `true` when the neutral `_meta.goal` namespace is in use; `false` for
    /// the legacy `_meta.codex.goal` one. Pinned at initialize so a
    /// transitional adapter that emits both cannot double-report.
    pub neutral_namespace: bool,
    /// Wire method goal controls are sent to.
    pub control_method: String,
    /// Control verbs the adapter implements. Empty means "observe only".
    pub actions: Vec<GoalAction>,
}

impl AcpGoalChannel {
    pub fn supports(&self, action: GoalAction) -> bool {
        self.actions.contains(&action)
    }
}

/// Resolve the goal channel from an `initialize` result.
///
/// Returns `None` when the Agent advertises no goal surface, or when the
/// dialect is unsupported for this Agent.
pub(crate) fn goal_channel_from_initialize(
    result: &Value,
    dialect: GoalDialect,
) -> Option<AcpGoalChannel> {
    let neutral = result
        .pointer("/_meta/goal")
        .filter(|goal| {
            goal.get("version")
                .and_then(Value::as_i64)
                .is_some_and(|version| version >= 1)
        })
        .and_then(Value::as_object);
    if let Some(goal) = neutral {
        let control_method = goal
            .get("controlMethod")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|method| !method.is_empty())
            .map(str::to_string)
            .or_else(|| {
                dialect
                    .legacy_codex
                    .then(|| LEGACY_GOAL_CONTROL_METHOD.to_string())
            });
        let actions = goal
            .get("actions")
            .and_then(Value::as_array)
            .map(|entries| {
                entries
                    .iter()
                    .filter_map(Value::as_str)
                    .filter_map(GoalAction::parse_wire)
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        return Some(AcpGoalChannel {
            neutral_namespace: true,
            control_method: control_method
                .unwrap_or_else(|| NEUTRAL_GOAL_CONTROL_METHOD.to_string()),
            actions: if goal.get("actions").is_some() {
                actions
            } else {
                Vec::new()
            },
        });
    }
    if dialect.legacy_codex {
        // Codex before the neutral extension: the bespoke method accepts only
        // pause/clear. Anything else goes through the `/goal` prompt.
        return Some(AcpGoalChannel {
            neutral_namespace: false,
            control_method: LEGACY_GOAL_CONTROL_METHOD.to_string(),
            actions: vec![GoalAction::Pause, GoalAction::Clear],
        });
    }
    None
}

/// One goal update decoded from a `session_info_update`'s `_meta`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AcpGoalUpdate {
    Snapshot(GoalSnapshot),
    Cleared,
}

/// Decode the goal payload out of a `session_info_update`.
///
/// `None` means this update carries no goal information for the pinned
/// channel. A JSON `null` goal means the provider cleared it.
pub(crate) fn goal_update_from_session_info(
    update: &Value,
    channel: &AcpGoalChannel,
) -> Option<AcpGoalUpdate> {
    let meta = update.get("_meta")?;
    let goal = if channel.neutral_namespace {
        meta.get("goal")?
    } else {
        meta.get("codex")?.get("goal")?
    };
    if goal.is_null() {
        return Some(AcpGoalUpdate::Cleared);
    }
    parse_goal_snapshot(goal).map(AcpGoalUpdate::Snapshot)
}

/// Normalize one provider goal object.
///
/// Requires a non-empty objective: a goal without one cannot be rendered and
/// would otherwise produce an empty card. Status is lenient (unknown values
/// become [`GoalPhase::Unknown`]) because a new provider status must not drop
/// the whole goal.
pub(crate) fn parse_goal_snapshot(value: &Value) -> Option<GoalSnapshot> {
    let objective = value
        .get("objective")
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|objective| !objective.is_empty())?
        .to_string();
    let phase = value
        .get("status")
        .and_then(Value::as_str)
        .map(GoalPhase::parse_wire)
        .unwrap_or(GoalPhase::Active);
    let mut snapshot = GoalSnapshot::new(objective, phase);
    snapshot.goal_id = string_field(value, &["goalId", "goal_id", "id"]);
    snapshot.revision =
        integer_field(value, &["revision"]).and_then(|revision| u64::try_from(revision).ok());
    snapshot.tokens_used = integer_field(value, &["tokensUsed", "tokens_used"]);
    snapshot.token_budget = integer_field(value, &["tokenBudget", "token_budget"]);
    snapshot.time_used_seconds = integer_field(value, &["timeUsedSeconds", "time_used_seconds"]);
    snapshot.rounds_started = integer_field(value, &["roundsStarted", "rounds_started"])
        .and_then(|rounds| u32::try_from(rounds).ok());
    snapshot.max_rounds = integer_field(value, &["maxRounds", "max_rounds", "maxGoalRounds"])
        .and_then(|rounds| u32::try_from(rounds).ok());
    snapshot.blocked_reason = parse_blocked_reason(
        value
            .get("blockedReason")
            .or_else(|| value.get("blocked_reason")),
    );
    snapshot.activation = value
        .get("activation")
        .and_then(Value::as_str)
        .and_then(GoalActivation::parse_wire);
    Some(snapshot)
}

fn parse_blocked_reason(value: Option<&Value>) -> Option<GoalBlockedReason> {
    match value? {
        Value::String(message) => {
            let message = message.trim();
            if message.is_empty() {
                return None;
            }
            Some(GoalBlockedReason {
                code: "provider-reported".to_string(),
                message: message.to_string(),
            })
        }
        Value::Object(object) => {
            let message = object
                .get("message")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|message| !message.is_empty())?;
            let code = object
                .get("code")
                .and_then(Value::as_str)
                .map(str::trim)
                .filter(|code| !code.is_empty())
                .unwrap_or("provider-reported");
            Some(GoalBlockedReason {
                code: code.to_string(),
                message: message.to_string(),
            })
        }
        _ => None,
    }
}

fn string_field(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn integer_field(value: &Value, keys: &[&str]) -> Option<i64> {
    keys.iter().find_map(|key| {
        let value = value.get(*key)?;
        value
            .as_i64()
            .or_else(|| value.as_u64().and_then(|value| i64::try_from(value).ok()))
    })
}

/// Request params for a goal control. Both dialects take the same shape; the
/// objective rides along only for `set`/`edit`, where the provider needs it.
pub(crate) fn build_goal_control_params(
    session_id: &str,
    action: GoalAction,
    objective: Option<&str>,
) -> Value {
    let mut params = serde_json::json!({
        "sessionId": session_id,
        "action": action.as_str(),
    });
    if let Some(objective) = objective.map(str::trim).filter(|value| !value.is_empty())
        && let Some(object) = params.as_object_mut()
    {
        object.insert(
            "objective".to_string(),
            Value::String(objective.to_string()),
        );
    }
    params
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn neutral_initialize_resolves_the_advertised_channel() {
        let result = json!({
            "_meta": {
                "goal": {
                    "version": 1,
                    "controlMethod": "_session/goal",
                    "actions": ["set", "pause", "resume", "clear"],
                }
            }
        });
        let channel = goal_channel_from_initialize(&result, GoalDialect::codex()).unwrap();
        assert!(channel.neutral_namespace);
        assert_eq!(channel.control_method, "_session/goal");
        assert_eq!(
            channel.actions,
            vec![
                GoalAction::Set,
                GoalAction::Pause,
                GoalAction::Resume,
                GoalAction::Clear
            ]
        );
        assert!(channel.supports(GoalAction::Pause));
    }

    #[test]
    fn codex_falls_back_to_the_legacy_channel_without_an_advertisement() {
        let result = json!({ "_meta": {} });
        let channel = goal_channel_from_initialize(&result, GoalDialect::codex()).unwrap();
        assert!(!channel.neutral_namespace);
        assert_eq!(channel.control_method, LEGACY_GOAL_CONTROL_METHOD);
        assert_eq!(channel.actions, vec![GoalAction::Pause, GoalAction::Clear]);
    }

    #[test]
    fn agents_without_an_advertisement_have_no_channel() {
        let result = json!({ "_meta": {} });
        assert!(goal_channel_from_initialize(&result, GoalDialect::neutral()).is_none());
        assert!(goal_channel_from_initialize(&result, GoalDialect::unsupported()).is_none());
    }

    #[test]
    fn any_agent_advertising_the_neutral_extension_gets_a_channel() {
        let result = json!({
            "_meta": { "goal": { "version": 1, "controlMethod": "_session/goal",
                "actions": ["pause"] } }
        });
        let channel = goal_channel_from_initialize(&result, GoalDialect::unsupported()).unwrap();
        assert!(channel.neutral_namespace);
        assert_eq!(channel.actions, vec![GoalAction::Pause]);
    }

    #[test]
    fn advertised_but_empty_actions_stay_authoritative() {
        let result = json!({
            "_meta": { "goal": { "version": 1, "controlMethod": "_session/goal", "actions": [] } }
        });
        let channel = goal_channel_from_initialize(&result, GoalDialect::neutral()).unwrap();
        assert!(channel.actions.is_empty());
        assert!(!channel.supports(GoalAction::Clear));
    }

    #[test]
    fn unknown_action_tokens_are_dropped_not_fatal() {
        let result = json!({
            "_meta": { "goal": { "version": 1, "controlMethod": "_session/goal",
                "actions": ["pause", "teleport", "clear"] } }
        });
        let channel = goal_channel_from_initialize(&result, GoalDialect::neutral()).unwrap();
        assert_eq!(channel.actions, vec![GoalAction::Pause, GoalAction::Clear]);
    }

    #[test]
    fn session_info_reads_the_pinned_namespace_only() {
        let neutral = AcpGoalChannel {
            neutral_namespace: true,
            control_method: NEUTRAL_GOAL_CONTROL_METHOD.to_string(),
            actions: vec![GoalAction::Clear],
        };
        let legacy = AcpGoalChannel {
            neutral_namespace: false,
            control_method: LEGACY_GOAL_CONTROL_METHOD.to_string(),
            actions: vec![GoalAction::Pause],
        };
        let update = json!({
            "sessionUpdate": "session_info_update",
            "_meta": {
                "goal": { "objective": "ship it", "status": "active" },
                "codex": { "goal": { "objective": "legacy", "status": "paused" } },
            }
        });
        let AcpGoalUpdate::Snapshot(neutral_goal) =
            goal_update_from_session_info(&update, &neutral).unwrap()
        else {
            panic!("expected a neutral snapshot");
        };
        assert_eq!(neutral_goal.objective, "ship it");
        let AcpGoalUpdate::Snapshot(legacy_goal) =
            goal_update_from_session_info(&update, &legacy).unwrap()
        else {
            panic!("expected a legacy snapshot");
        };
        assert_eq!(legacy_goal.objective, "legacy");
        assert_eq!(legacy_goal.phase, GoalPhase::Paused);
    }

    #[test]
    fn null_goal_is_a_clear() {
        let channel = AcpGoalChannel {
            neutral_namespace: true,
            control_method: NEUTRAL_GOAL_CONTROL_METHOD.to_string(),
            actions: Vec::new(),
        };
        let update = json!({ "_meta": { "goal": null } });
        assert_eq!(
            goal_update_from_session_info(&update, &channel),
            Some(AcpGoalUpdate::Cleared)
        );
    }

    #[test]
    fn snapshot_normalizes_codex_spellings_and_stats() {
        let value = json!({
            "objective": "  ship the feature  ",
            "status": "budgetLimited",
            "tokensUsed": 1200,
            "tokenBudget": 5000,
            "timeUsedSeconds": 90,
            "blockedReason": { "code": "round-limit", "message": "out of rounds" },
        });
        let goal = parse_goal_snapshot(&value).unwrap();
        assert_eq!(goal.objective, "ship the feature");
        assert_eq!(goal.phase, GoalPhase::BudgetLimited);
        assert_eq!(goal.tokens_used, Some(1200));
        assert_eq!(goal.token_budget, Some(5000));
        assert_eq!(goal.remaining_tokens(), Some(3800));
        assert_eq!(goal.time_used_seconds, Some(90));
        assert_eq!(
            goal.blocked_reason,
            Some(GoalBlockedReason {
                code: "round-limit".to_string(),
                message: "out of rounds".to_string(),
            })
        );
    }

    #[test]
    fn snapshot_requires_an_objective_and_keeps_unknown_statuses() {
        assert!(parse_goal_snapshot(&json!({ "status": "active" })).is_none());
        assert!(parse_goal_snapshot(&json!({ "objective": "   " })).is_none());
        let goal =
            parse_goal_snapshot(&json!({ "objective": "x", "status": "hibernating" })).unwrap();
        assert_eq!(goal.phase, GoalPhase::Unknown);
    }

    #[test]
    fn snapshot_accepts_harness_round_fields() {
        let goal = parse_goal_snapshot(&json!({
            "objective": "x",
            "status": "active",
            "goalId": "goal-1",
            "revision": 3,
            "roundsStarted": 2,
            "maxRounds": 256,
            "activation": "armed",
        }))
        .unwrap();
        assert_eq!(goal.goal_id.as_deref(), Some("goal-1"));
        assert_eq!(goal.revision, Some(3));
        assert_eq!(goal.rounds_started, Some(2));
        assert_eq!(goal.max_rounds, Some(256));
        assert_eq!(goal.activation, Some(GoalActivation::Armed));
    }

    #[test]
    fn control_params_use_the_lowercase_wire_action() {
        assert_eq!(
            build_goal_control_params("session-1", GoalAction::Pause, None),
            json!({ "sessionId": "session-1", "action": "pause" })
        );
        assert_eq!(
            build_goal_control_params("session-1", GoalAction::Clear, None),
            json!({ "sessionId": "session-1", "action": "clear" })
        );
    }

    #[test]
    fn control_params_carry_the_objective_only_when_provided() {
        assert_eq!(
            build_goal_control_params("session-1", GoalAction::Set, Some("  ship it  ")),
            json!({ "sessionId": "session-1", "action": "set", "objective": "ship it" })
        );
        assert_eq!(
            build_goal_control_params("session-1", GoalAction::Set, Some("   ")),
            json!({ "sessionId": "session-1", "action": "set" })
        );
    }
}

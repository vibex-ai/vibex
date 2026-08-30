use serde::{Deserialize, Serialize};

use crate::{AgentDelegationId, AgentId, ProviderProfileId, TimelineItemId, VibexSessionId};

/// A durable, product-owned relationship between a parent session and a
/// separately executable child session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentDelegation {
    pub id: AgentDelegationId,
    pub parent_session_id: VibexSessionId,
    pub parent_timeline_item_id: Option<TimelineItemId>,
    pub child_session_id: Option<VibexSessionId>,
    pub idempotency_key: String,
    pub title: String,
    /// Bounded user-facing task summary. The complete prompt is represented by
    /// the child session's normal message submission.
    pub task_summary: String,
    pub requested_agent_id: Option<AgentId>,
    pub effective_agent_id: Option<AgentId>,
    pub status: AgentDelegationStatus,
    pub result_summary: Option<String>,
    pub error_code: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub started_at_ms: Option<i64>,
    pub completed_at_ms: Option<i64>,
}

impl AgentDelegation {
    pub const fn is_terminal(&self) -> bool {
        self.status.is_terminal()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentDelegationStatus {
    Queued,
    Starting,
    Running,
    NeedsInput,
    Completed,
    Failed,
    Cancelled,
}

impl AgentDelegationStatus {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

/// Request accepted by the local delegation bridge. The parent session comes
/// from the scoped MCP process, never from an Agent-controlled provider field.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub idempotency_key: String,
    pub task: String,
    pub title: Option<String>,
    pub agent_id: Option<AgentId>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub model: Option<String>,
    pub reasoning_effort: Option<String>,
    pub mode_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub delegation_id: AgentDelegationId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CancelAgentDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub delegation_id: AgentDelegationId,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn delegation_contract_round_trips_with_stable_field_names() {
        let delegation = AgentDelegation {
            id: AgentDelegationId::new(),
            parent_session_id: VibexSessionId::new(),
            parent_timeline_item_id: None,
            child_session_id: Some(VibexSessionId::new()),
            idempotency_key: "delegate-review".to_string(),
            title: "Review changes".to_string(),
            task_summary: "Inspect the implementation".to_string(),
            requested_agent_id: None,
            effective_agent_id: None,
            status: AgentDelegationStatus::Completed,
            result_summary: Some("No issues found".to_string()),
            error_code: None,
            created_at_ms: 10,
            updated_at_ms: 20,
            started_at_ms: Some(11),
            completed_at_ms: Some(20),
        };

        let encoded = serde_json::to_value(&delegation).unwrap();

        assert!(encoded.get("parentSessionId").is_some());
        assert!(encoded.get("childSessionId").is_some());
        assert!(encoded.get("parent_session_id").is_none());
        assert_eq!(encoded["status"], "completed");
        assert_eq!(
            serde_json::from_value::<AgentDelegation>(encoded).unwrap(),
            delegation
        );
        assert!(AgentDelegationStatus::Completed.is_terminal());
        assert!(!AgentDelegationStatus::NeedsInput.is_terminal());
    }
}

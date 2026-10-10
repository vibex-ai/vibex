//! Typed human-facing team contracts shared by native and remote clients.
//!
//! Selecting a mention is a separate action from typing a resource URI. Only
//! the authenticated human submission boundary may turn these tokens into a
//! read grant. Control is always an explicit access mutation.

use serde::{Deserialize, Serialize};

use crate::{
    AgentDelegationId, AgentSession, DelegationCompletionPolicy, DelegationExecution,
    DelegationTaskEvent, DelegationTaskView, MessageAttachment, PresentationActivationPolicy,
    PresentationOutcome, SendAgentMessageRequest, SessionTreeNode, VibexSessionId,
    VibexUseCapabilitySnapshot, VibexUseGroupSummary, VibexUseMention, VibexUseRef,
};

pub const TEAM_PAGE_LIMIT: usize = 100;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTeamReadRequest<T> {
    pub auth: crate::RemoteAuthProof,
    pub request: T,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteTeamMutationRequest<T> {
    pub auth: crate::RemoteAuthProof,
    pub request: T,
    pub idempotency_key: String,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanAgentMessageRequest {
    pub message: SendAgentMessageRequest,
    #[serde(default)]
    pub mentions: Vec<VibexUseMention>,
}

impl std::fmt::Debug for HumanAgentMessageRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanAgentMessageRequest")
            .field("message", &self.message)
            .field("mention_count", &self.mentions.len())
            .finish()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionAccess {
    Read,
    Control,
    Revoke,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAccessRequest {
    pub grantee_session_id: VibexSessionId,
    pub target_session_id: VibexSessionId,
    pub access: SessionAccess,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionAccessResult {
    pub grantee_session_id: VibexSessionId,
    pub target_session_id: VibexSessionId,
    pub access: SessionAccess,
    pub changed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeRequest {
    /// `None` selects ownership roots; `Some` selects direct children only.
    #[serde(default)]
    pub parent_session_id: Option<VibexSessionId>,
    #[serde(default)]
    pub cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
    #[serde(default)]
    pub include_archived: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreePage {
    pub nodes: Vec<SessionTreeNode>,
    /// Ownership root through the requested parent, bounded to 16 nodes.
    /// Root pages have no ancestors. This allows direct navigation to a child
    /// without first fetching every branch of the forest.
    #[serde(default)]
    pub ancestors: Vec<SessionTreeNode>,
    /// Metadata for nodes and ancestors, including owned children omitted from
    /// the ordinary root session list. No transcript previews are fetched.
    pub registry: Vec<AgentSession>,
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSnapshotRequest {
    pub session_id: VibexSessionId,
    #[serde(default)]
    pub after_event_cursor: Option<i64>,
    #[serde(default)]
    pub task_cursor: Option<String>,
    #[serde(default)]
    pub execution_cursor: Option<String>,
    #[serde(default)]
    pub limit: Option<usize>,
}

impl TeamSnapshotRequest {
    pub fn new(session_id: VibexSessionId) -> Self {
        Self {
            session_id,
            after_event_cursor: None,
            task_cursor: None,
            execution_cursor: None,
            limit: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct TeamInbox {
    pub events: Vec<DelegationTaskEvent>,
    pub next_cursor: i64,
    pub has_more: bool,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamSnapshot {
    pub root_session_id: VibexSessionId,
    pub tasks: Vec<DelegationTaskView>,
    pub executions: Vec<DelegationExecution>,
    pub inbox: TeamInbox,
    pub tree: SessionTreePage,
    pub groups: Vec<VibexUseGroupSummary>,
    pub capability: VibexUseCapabilitySnapshot,
    pub next_task_cursor: Option<String>,
    pub has_more_tasks: bool,
    pub next_execution_cursor: Option<String>,
    pub has_more_executions: bool,
}

impl std::fmt::Debug for TeamSnapshot {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TeamSnapshot")
            .field("root_session_id", &self.root_session_id)
            .field("task_count", &self.tasks.len())
            .field("execution_count", &self.executions.len())
            .field("event_count", &self.inbox.events.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamContextReference {
    pub session_id: VibexSessionId,
    #[serde(default)]
    pub from_sequence: Option<i64>,
    #[serde(default)]
    pub through_sequence: Option<i64>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanDelegationRequest {
    pub parent_session_id: VibexSessionId,
    pub task: String,
    #[serde(default)]
    pub title: Option<String>,
    #[serde(default)]
    pub runtime_option_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub existing_session_id: Option<VibexSessionId>,
    #[serde(default = "human_completion_policy")]
    pub completion_policy: DelegationCompletionPolicy,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    #[serde(default)]
    pub context_refs: Vec<TeamContextReference>,
    #[serde(default)]
    pub mentions: Vec<VibexUseMention>,
}

fn human_completion_policy() -> DelegationCompletionPolicy {
    DelegationCompletionPolicy::OwnerReview
}

impl HumanDelegationRequest {
    pub fn new(parent_session_id: VibexSessionId, task: impl Into<String>) -> Self {
        Self {
            parent_session_id,
            task: task.into(),
            title: None,
            runtime_option_ref: None,
            existing_session_id: None,
            completion_policy: DelegationCompletionPolicy::OwnerReview,
            acceptance_criteria: Vec::new(),
            attachments: Vec::new(),
            context_refs: Vec::new(),
            mentions: Vec::new(),
        }
    }
}

impl std::fmt::Debug for HumanDelegationRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("HumanDelegationRequest")
            .field("parent_session_id", &self.parent_session_id)
            .field("existing_session_id", &self.existing_session_id)
            .field("attachment_count", &self.attachments.len())
            .field("context_count", &self.context_refs.len())
            .field("mention_count", &self.mentions.len())
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct HumanDelegationResult {
    pub task: DelegationTaskView,
    pub operation_ref: VibexUseRef,
    pub presentation: Option<PresentationOutcome>,
    pub warnings: Vec<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum TeamTaskAction {
    Accept,
    Cancel { cascade: bool },
    Interrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamTaskControlRequest {
    pub session_id: VibexSessionId,
    pub task_id: AgentDelegationId,
    pub action: TeamTaskAction,
    #[serde(default)]
    pub expected_revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamPresentationRequest {
    pub session_id: VibexSessionId,
    pub group_ref: VibexUseRef,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub activation_policy: PresentationActivationPolicy,
    #[serde(default)]
    pub focus_session_id: Option<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TeamAcknowledgeRequest {
    pub session_id: VibexSessionId,
    pub event_ids: Vec<String>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_human_delegation_defaults_to_owner_review_over_the_wire() {
        let request: HumanDelegationRequest = serde_json::from_value(serde_json::json!({
            "parentSessionId": VibexSessionId::new(), "task": "Review the change"
        }))
        .unwrap();
        assert_eq!(
            request.completion_policy,
            DelegationCompletionPolicy::OwnerReview
        );
    }
}

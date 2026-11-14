use serde::{Deserialize, Serialize};

use crate::ids::{DeviceId, ProjectId, RequestId, VibexSessionId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionMode {
    WorkspaceWrite,
    AskOnRisk,
    BypassAll,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRiskCategory {
    Command,
    FileReadSensitive,
    FileWrite,
    FileDeleteOrMove,
    Network,
    GitDestructive,
    ProviderConfigExport,
    CustomTool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionResponseKind {
    Approve,
    Deny,
    AlwaysAllowForSession,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResponseOption {
    pub option_id: String,
    pub label: String,
    pub response: PermissionResponseKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PermissionRequestStatus {
    Pending,
    Approved,
    Denied,
    Expired,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionActionDetail {
    pub label: String,
    pub value: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionRequest {
    pub id: RequestId,
    pub session_id: VibexSessionId,
    pub project_id: Option<ProjectId>,
    pub workspace_id: Option<WorkspaceId>,
    pub provider_request_id: Option<String>,
    pub risk_category: PermissionRiskCategory,
    pub title: String,
    pub details: Vec<PermissionActionDetail>,
    pub allowed_responses: Vec<PermissionResponseKind>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub response_options: Vec<PermissionResponseOption>,
    pub status: PermissionRequestStatus,
    pub requested_at_ms: i64,
    /// When this card stops accepting an answer, if its producer set one.
    ///
    /// The producer that raises a card and blocks on it owns this deadline: the
    /// embedded browser and computer use publish theirs, and the runtime-wide
    /// expiry sweep moves a row past it to `expired` without inventing a
    /// resolution. `None` means the card has no deadline and its producer
    /// closes it explicitly.
    pub expires_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PermissionResolution {
    pub request_id: RequestId,
    pub session_id: VibexSessionId,
    pub response: PermissionResponseKind,
    pub responder_device_id: Option<DeviceId>,
    pub provider_resolution_id: Option<String>,
    pub note: Option<String>,
    pub resolved_at_ms: i64,
    /// Who answered, when the answer did not come from a person.
    ///
    /// A parent Agent may answer its own child's card when the child inherited
    /// the parent's sandbox and tool set. Recording that is what keeps the
    /// timeline honest: a reader looking at an approved command has to be able
    /// to tell "I approved this" from "the Agent that owns this task approved it
    /// on my behalf".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub responder: Option<ApprovalResponder>,
}

/// Who answered a permission card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum ApprovalResponder {
    /// A person, through a surface.
    Human,
    /// The Agent that owns the task the card belongs to.
    ParentAgent {
        /// The session that answered.
        session_id: VibexSessionId,
        /// The task whose child asked.
        task_id: crate::AgentDelegationId,
    },
}

impl ApprovalResponder {
    /// The label a timeline row shows beside the resolution.
    pub const fn label(&self) -> &'static str {
        match self {
            Self::Human => "human",
            Self::ParentAgent { .. } => "parent_agent",
        }
    }
}

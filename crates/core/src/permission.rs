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
    pub status: PermissionRequestStatus,
    pub requested_at_ms: i64,
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
}

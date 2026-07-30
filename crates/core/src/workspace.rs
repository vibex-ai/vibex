use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, WorkspaceId};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkspaceMode {
    CurrentCheckout,
    VibexWorktree,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectRecord {
    pub id: ProjectId,
    pub name: String,
    pub root_path: String,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceRecord {
    pub id: WorkspaceId,
    pub project_id: ProjectId,
    pub root_path: String,
    pub mode: WorkspaceMode,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct OpenWorkspaceRequest {
    pub root_path: String,
    pub mode: Option<WorkspaceMode>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceAggregateStatus {
    pub agent_running: bool,
    pub terminal_running: bool,
    pub pending_permission: bool,
    pub git_dirty: bool,
    pub sync_disconnected: bool,
}

impl WorkspaceAggregateStatus {
    pub fn empty() -> Self {
        Self {
            agent_running: false,
            terminal_running: false,
            pending_permission: false,
            git_dirty: false,
            sync_disconnected: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProjectWorkspaceSummary {
    pub project: ProjectRecord,
    pub workspace: WorkspaceRecord,
    pub aggregate_status: WorkspaceAggregateStatus,
    pub git_branch: Option<String>,
    pub git_dirty: bool,
}

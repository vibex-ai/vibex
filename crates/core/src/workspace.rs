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

impl ProjectRecord {
    /// Whether this row is the project a session with no project of its own
    /// runs in.
    ///
    /// Both authorities keep that project at one fixed path shape: the native
    /// runtime creates it under `std::env::temp_dir()/vibex/sessions`, and the
    /// remote runtime under `<runtime-home>/tmp/vibex/sessions`. The project
    /// row is created from that root, so its trailing components identify it
    /// without a second persisted flag that could drift from the directory the
    /// sessions actually run in — or from rows written before such a flag
    /// existed.
    ///
    /// A project the reader opened at a coincidentally identical path is the
    /// same row: there is nothing else that directory could mean.
    pub fn is_temporary(&self) -> bool {
        is_temporary_session_root(std::path::Path::new(&self.root_path))
    }
}

/// The trailing components of an authority's temporary session root.
pub fn is_temporary_session_root(path: &std::path::Path) -> bool {
    let mut components = path.components().rev();
    matches!(
        components.next(),
        Some(std::path::Component::Normal(name)) if name == "sessions"
    ) && matches!(
        components.next(),
        Some(std::path::Component::Normal(name)) if name == "vibex"
    )
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

#[cfg(test)]
mod tests {
    use super::*;

    fn project(root: &str) -> ProjectRecord {
        ProjectRecord {
            id: ProjectId::new(),
            name: "sessions".into(),
            root_path: root.into(),
            created_at_ms: 1,
            updated_at_ms: 1,
        }
    }

    #[test]
    fn only_the_authority_temporary_root_is_temporary() {
        assert!(project("/tmp/vibex/sessions").is_temporary());
        assert!(project("/var/lib/vibex/tmp/vibex/sessions").is_temporary());
        assert!(project("/tmp/vibex/sessions/").is_temporary());

        // A project the reader happens to call `sessions` is an ordinary
        // project: the name alone is not what makes a row temporary.
        assert!(!project("/home/reader/sessions").is_temporary());
        assert!(!project("/tmp/vibex/sessions-2024").is_temporary());
        assert!(!project("/tmp/vibex").is_temporary());
        assert!(!project("/tmp/other/sessions").is_temporary());
        assert!(!project("").is_temporary());
    }
}

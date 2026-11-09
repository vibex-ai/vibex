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

/// Creates and resolves an authority's temporary session root.
///
/// The root lives under the operating system's temporary directory on the
/// native runtime, so a reboot or a `/tmp` sweep removes it while the project
/// and workspace rows that point at it survive in the database. Creating it is
/// therefore part of every path that is about to use it, not just the first
/// session that happens to create the project.
pub fn ensure_temporary_session_root(root: &std::path::Path) -> crate::VibexResult<String> {
    std::fs::create_dir_all(root).map_err(|error| {
        crate::VibexError::storage(
            "temporary_workspace_create_failed",
            "failed to create temporary session workspace",
        )
        .with_diagnostic("path", root.display().to_string())
        .with_diagnostic("error", error.to_string())
    })?;
    root.canonicalize()
        .map(|path| path.to_string_lossy().into_owned())
        .map_err(|error| {
            crate::VibexError::storage(
                "temporary_workspace_canonicalize_failed",
                "failed to resolve temporary session workspace",
            )
            .with_diagnostic("path", root.display().to_string())
            .with_diagnostic("error", error.to_string())
        })
}

/// Recreates the authority's temporary session root when a persisted row
/// outlived it.
///
/// Only the temporary root is recreated. A reader's own project directory that
/// disappeared is missing because the reader moved or deleted it, and silently
/// recreating an empty directory in its place would hide that — an Agent would
/// start in an empty folder instead of reporting the real problem.
///
/// Returns whether the directory had to be created.
pub fn restore_temporary_session_root(root: &str) -> crate::VibexResult<bool> {
    let path = std::path::Path::new(root);
    if !is_temporary_session_root(path) || path.is_dir() {
        return Ok(false);
    }
    ensure_temporary_session_root(path)?;
    Ok(true)
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

    /// A `/tmp` sweep removes the temporary root while its rows survive. Every
    /// path that is about to start an Agent there has to put it back.
    #[test]
    fn a_missing_temporary_root_is_recreated() {
        let parent = std::env::temp_dir().join(format!(
            "vibex-temp-root-{}",
            crate::RequestId::new().as_str()
        ));
        let root = parent.join("vibex").join("sessions");
        let root_text = root.to_string_lossy().into_owned();
        assert!(!root.exists());

        assert!(restore_temporary_session_root(&root_text).unwrap());
        assert!(root.is_dir());
        // A second call finds the directory and touches nothing.
        assert!(!restore_temporary_session_root(&root_text).unwrap());

        let _ = std::fs::remove_dir_all(&parent);
    }

    /// A vanished reader project is not recreated: an empty directory under the
    /// reader's home would start the Agent somewhere the reader never chose.
    #[test]
    fn a_missing_project_directory_is_reported_not_recreated() {
        let root = std::env::temp_dir().join(format!(
            "vibex-reader-project-{}",
            crate::RequestId::new().as_str()
        ));
        let root_text = root.to_string_lossy().into_owned();

        assert!(!restore_temporary_session_root(&root_text).unwrap());
        assert!(!root.exists());
    }
}

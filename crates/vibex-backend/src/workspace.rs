use serde::{Deserialize, Serialize};
use vibex_core::{OpenWorkspaceRequest, ProjectId, ProjectRecord, WorkspaceId, WorkspaceRecord};

use crate::{BackendBound, BackendFuture, MutationRequest};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub project: ProjectRecord,
    pub workspace: WorkspaceRecord,
    /// The branch observed while the workspace list was produced. Native
    /// callers may leave this unset; remote summaries provide it for compact
    /// clients that mirror the desktop Worktree row.
    #[serde(default)]
    pub git_branch: Option<String>,
}

pub trait WorkspaceBackend: BackendBound {
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<WorkspaceSummary>>;

    fn open_workspace(
        &self,
        request: MutationRequest<OpenWorkspaceRequest>,
    ) -> BackendFuture<'_, WorkspaceSummary>;

    fn get_workspace(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, WorkspaceSummary>;

    fn delete_workspace(&self, request: MutationRequest<WorkspaceId>) -> BackendFuture<'_, ()>;

    fn delete_project(&self, request: MutationRequest<ProjectId>) -> BackendFuture<'_, ()>;
}

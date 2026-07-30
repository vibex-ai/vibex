use serde::{Deserialize, Serialize};
use vibex_core::{OpenWorkspaceRequest, ProjectId, ProjectRecord, WorkspaceId, WorkspaceRecord};

use crate::{BackendBound, BackendFuture, MutationRequest};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WorkspaceSummary {
    pub project: ProjectRecord,
    pub workspace: WorkspaceRecord,
}

pub trait WorkspaceBackend: BackendBound {
    fn list_workspaces(&self) -> BackendFuture<'_, Vec<WorkspaceSummary>>;

    fn open_workspace(
        &self,
        request: MutationRequest<OpenWorkspaceRequest>,
    ) -> BackendFuture<'_, WorkspaceSummary>;

    fn get_workspace(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, WorkspaceSummary>;

    fn delete_project(&self, request: MutationRequest<ProjectId>) -> BackendFuture<'_, ()>;
}

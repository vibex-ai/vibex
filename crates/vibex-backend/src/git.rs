use vibex_core::{
    GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse, GitStageRequest,
    GitStatusSummary, WorkspaceId,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait GitBackend: BackendBound {
    fn git_status(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary>;

    fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse>;

    fn stage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary>;

    fn unstage(
        &self,
        request: MutationRequest<GitStageRequest>,
    ) -> BackendFuture<'_, GitStatusSummary>;

    fn commit(
        &self,
        request: MutationRequest<GitCommitRequest>,
    ) -> BackendFuture<'_, GitCommitResult>;
}

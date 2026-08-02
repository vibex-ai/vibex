use vibex_core::{
    GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse, GitProjectEligibility,
    GitStageRequest, GitStatusSummary, GitWorktreeCreateRequest, GitWorktreeCreateResult,
    GitWorktreeLifecycleSnapshot, WorkspaceId,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

pub trait GitBackend: BackendBound {
    fn git_status(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary>;

    fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse>;

    fn git_worktree_eligibility(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitProjectEligibility>;

    fn git_worktree_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, GitWorktreeLifecycleSnapshot>;

    fn git_worktree_create(
        &self,
        request: MutationRequest<GitWorktreeCreateRequest>,
    ) -> BackendFuture<'_, GitWorktreeCreateResult>;

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

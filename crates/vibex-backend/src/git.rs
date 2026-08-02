use vibex_core::{
    GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse, GitProjectEligibility,
    GitStageRequest, GitStatusSummary, GitWorktreeArchiveRequest,
    GitWorktreeAssistanceSessionRequest, GitWorktreeConflictResolveRequest,
    GitWorktreeConflictStageRequest, GitWorktreeCreateRequest, GitWorktreeCreateResult,
    GitWorktreeDestructivePreflight, GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot,
    GitWorktreeMergePlan, GitWorktreeMergeRequest, GitWorktreeOperationRecord,
    GitWorktreeOperationRequest, GitWorktreeReadinessRecord, GitWorktreeReadinessRequest,
    GitWorktreeRestoreRequest, WorkspaceId,
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

    fn git_worktree_readiness(
        &self,
        workspace_id: WorkspaceId,
    ) -> BackendFuture<'_, Option<GitWorktreeReadinessRecord>>;

    fn git_worktree_set_readiness(
        &self,
        request: MutationRequest<GitWorktreeReadinessRequest>,
    ) -> BackendFuture<'_, GitWorktreeReadinessRecord>;

    fn git_worktree_merge_plan(
        &self,
        request: GitWorktreeMergeRequest,
    ) -> BackendFuture<'_, GitWorktreeMergePlan>;

    fn git_worktree_merge(
        &self,
        request: MutationRequest<GitWorktreeMergeRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_resolve_conflict(
        &self,
        request: MutationRequest<GitWorktreeConflictResolveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_stage_conflicts(
        &self,
        request: MutationRequest<GitWorktreeConflictStageRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_bind_assistance_session(
        &self,
        request: MutationRequest<GitWorktreeAssistanceSessionRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_continue_merge(
        &self,
        request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_abort_merge(
        &self,
        request: MutationRequest<GitWorktreeOperationRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_archive_preflight(
        &self,
        request: GitWorktreeArchiveRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight>;

    fn git_worktree_archive(
        &self,
        request: MutationRequest<GitWorktreeArchiveRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_restore_preflight(
        &self,
        request: GitWorktreeRestoreRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight>;

    fn git_worktree_restore(
        &self,
        request: MutationRequest<GitWorktreeRestoreRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

    fn git_worktree_discard_preflight(
        &self,
        request: GitWorktreeDiscardRequest,
    ) -> BackendFuture<'_, GitWorktreeDestructivePreflight>;

    fn git_worktree_discard(
        &self,
        request: MutationRequest<GitWorktreeDiscardRequest>,
    ) -> BackendFuture<'_, GitWorktreeOperationRecord>;

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

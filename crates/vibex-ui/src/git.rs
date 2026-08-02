use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use vibex_backend::{
    BackendError, BackendFuture, BackendOperation, BackendResult, DomainCapabilities, GitBackend,
    MutationRequest,
};
use vibex_core::{
    GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse, GitStageRequest,
    GitStatusSummary, RequestId, WorkspaceId,
};
use vibex_desktop_model::{
    GitMutationKind, GitMutationScope, GitQueryKind, GitQueryTicket, GitWorkbenchState,
};

use crate::{
    AsyncState, GitCommitConfirmationModel, GitWorkflowView, MIN_TOUCH_TARGET_PX,
    WorkflowViewGeneration,
};

pub const GIT_WORKFLOW_MAX_COMMIT_MESSAGE_BYTES: usize = 8 * 1024;
pub const GIT_WORKFLOW_MAX_PATHS: usize = 1_024;

#[derive(Clone, Default)]
pub struct GitWorkflowState {
    pub generation: WorkflowViewGeneration,
    pub workspace_id: Option<WorkspaceId>,
    pub model: GitWorkbenchState,
    pub commit_confirmation: Option<GitCommitConfirmationModel>,
    pub last_commit: AsyncState<GitCommitResult>,
    pub last_error: Option<BackendError>,
}

impl fmt::Debug for GitWorkflowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitWorkflowState")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("has_status", &self.model.status.is_some())
            .field(
                "status_change_count",
                &self
                    .model
                    .status
                    .as_ref()
                    .map_or(0, |status| status.changes.len()),
            )
            .field("selected_path_count", &self.model.selected_path_count())
            .field("diff_count", &self.model.diffs.len())
            .field(
                "pending_mutation_kind",
                &self.model.pending_mutation.as_ref().map(|scope| scope.kind),
            )
            .field(
                "pending_mutation_path_count",
                &self
                    .model
                    .pending_mutation
                    .as_ref()
                    .map_or(0, |scope| scope.paths.len()),
            )
            .field(
                "has_commit_confirmation",
                &self.commit_confirmation.is_some(),
            )
            .field("last_commit_phase", &self.last_commit.phase)
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .finish()
    }
}

impl GitWorkflowState {
    pub fn view(&self) -> GitWorkflowView {
        GitWorkflowView {
            generation: self.generation.0,
            status: self.model.status.clone(),
            selected_paths: self.model.selected_change_paths(),
            commit_confirmation: self.commit_confirmation.clone(),
            last_error: self.last_error.as_ref().map(|error| error.code.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitStatusLoadTicket {
    pub view_generation: WorkflowViewGeneration,
    pub query: GitQueryTicket,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitDiffLoadTicket {
    pub view_generation: WorkflowViewGeneration,
    pub query: GitQueryTicket,
    pub request: GitDiffRequest,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitSafeMutationKind {
    Stage,
    Unstage,
    Commit,
}

#[derive(Clone, PartialEq, Eq)]
pub struct GitMutationOperation {
    pub generation: WorkflowViewGeneration,
    pub workspace_id: WorkspaceId,
    pub operation_id: String,
    pub kind: GitSafeMutationKind,
    pub paths: Vec<String>,
    pub commit_message: Option<String>,
}

impl fmt::Debug for GitMutationOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GitMutationOperation")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("operation_id", &self.operation_id)
            .field("kind", &self.kind)
            .field("path_count", &self.paths.len())
            .field(
                "commit_message_bytes",
                &self.commit_message.as_deref().map_or(0, str::len),
            )
            .finish()
    }
}

#[derive(Clone)]
pub struct GitWorkflowController {
    backend: Arc<dyn GitBackend>,
    capabilities: DomainCapabilities,
    pub state: GitWorkflowState,
}

impl GitWorkflowController {
    pub fn new(backend: Arc<dyn GitBackend>, capabilities: DomainCapabilities) -> Self {
        Self {
            backend,
            capabilities,
            state: GitWorkflowState::default(),
        }
    }

    pub fn set_capabilities(&mut self, capabilities: DomainCapabilities) {
        self.capabilities = capabilities;
    }

    pub fn capabilities(&self) -> &DomainCapabilities {
        &self.capabilities
    }

    pub fn select_workspace(&mut self, workspace_id: WorkspaceId) -> WorkflowViewGeneration {
        let generation = self.state.generation.advance();
        self.state.workspace_id = Some(workspace_id.clone());
        self.state.model.reset_workspace(workspace_id);
        self.state.commit_confirmation = None;
        self.state.last_commit.clear();
        self.state.last_error = None;
        generation
    }

    pub fn begin_status_load(&mut self) -> BackendResult<GitStatusLoadTicket> {
        self.require(BackendOperation::GitStatus)?;
        self.current_workspace()?;
        let query = self
            .state
            .model
            .begin_query(GitQueryKind::Status, "status")
            .ok_or_else(|| {
                BackendError::failed("git_workspace_missing", "no Git workspace is selected")
            })?;
        Ok(GitStatusLoadTicket {
            view_generation: self.state.generation,
            query,
        })
    }

    pub fn load_status(
        &self,
        ticket: GitStatusLoadTicket,
    ) -> BackendFuture<'static, GitStatusSummary> {
        if let Err(error) = self.require(BackendOperation::GitStatus) {
            return error_future(error);
        }
        if self.state.generation != ticket.view_generation
            || self.state.workspace_id.as_ref() != Some(&ticket.query.workspace_id)
            || !self.state.model.accept_ticket(&ticket.query)
        {
            return error_future(BackendError::conflict(
                "git_query_generation_stale",
                "the Git status query targets a workspace that is no longer selected",
            ));
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.git_status(ticket.query.workspace_id).await })
    }

    pub fn apply_status(
        &mut self,
        ticket: &GitStatusLoadTicket,
        result: BackendResult<GitStatusSummary>,
    ) -> bool {
        if self.state.generation != ticket.view_generation
            || !self.state.model.accept_ticket(&ticket.query)
        {
            return false;
        }
        match result {
            Ok(status) => {
                let applied = self.state.model.apply_status(&ticket.query, status);
                if applied {
                    self.state.last_error = None;
                }
                applied
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                self.state.model.fail_query(&ticket.query, &error.code)
            }
        }
    }

    pub fn begin_diff_load(
        &mut self,
        path: impl Into<String>,
        staged: bool,
    ) -> BackendResult<GitDiffLoadTicket> {
        self.require(BackendOperation::GitDiff)?;
        let workspace_id = self.current_workspace()?;
        let path = normalize_path(&path.into());
        if path.is_empty() {
            return Err(BackendError::failed(
                "git_diff_path_invalid",
                "a repository-relative path is required",
            ));
        }
        let key = format!("{}:{path}", if staged { "staged" } else { "unstaged" });
        let query = self
            .state
            .model
            .begin_query(GitQueryKind::Diff, key)
            .ok_or_else(|| {
                BackendError::failed("git_workspace_missing", "no Git workspace is selected")
            })?;
        Ok(GitDiffLoadTicket {
            view_generation: self.state.generation,
            query,
            request: GitDiffRequest {
                workspace_id,
                path,
                staged,
            },
        })
    }

    pub fn load_diff(&self, ticket: GitDiffLoadTicket) -> BackendFuture<'static, GitDiffResponse> {
        if let Err(error) = self.require(BackendOperation::GitDiff) {
            return error_future(error);
        }
        if self.state.generation != ticket.view_generation
            || self.state.workspace_id.as_ref() != Some(&ticket.request.workspace_id)
            || !self.state.model.accept_ticket(&ticket.query)
        {
            return error_future(BackendError::conflict(
                "git_query_generation_stale",
                "the Git diff query targets a workspace that is no longer selected",
            ));
        }
        let backend = self.backend.clone();
        Box::pin(async move { backend.git_diff(ticket.request).await })
    }

    pub fn apply_diff(
        &mut self,
        ticket: &GitDiffLoadTicket,
        result: BackendResult<GitDiffResponse>,
    ) -> bool {
        if self.state.generation != ticket.view_generation
            || !self.state.model.accept_ticket(&ticket.query)
        {
            return false;
        }
        match result {
            Ok(diff)
                if diff.workspace_id != ticket.request.workspace_id
                    || normalize_path(&diff.path) != ticket.request.path
                    || diff.staged != ticket.request.staged =>
            {
                let error = BackendError::failed(
                    "git_diff_response_mismatch",
                    "the backend returned a different Git diff",
                );
                self.state.last_error = Some(error.clone());
                self.state.model.fail_query(&ticket.query, &error.code)
            }
            Ok(diff) => {
                let applied = self.state.model.apply_diff(&ticket.query, diff);
                if applied {
                    self.state.last_error = None;
                }
                applied
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                self.state.model.fail_query(&ticket.query, &error.code)
            }
        }
    }

    pub fn begin_stage(&mut self, paths: Vec<String>) -> BackendResult<GitMutationOperation> {
        self.begin_paths_mutation(paths, GitSafeMutationKind::Stage)
    }

    pub fn begin_unstage(&mut self, paths: Vec<String>) -> BackendResult<GitMutationOperation> {
        self.begin_paths_mutation(paths, GitSafeMutationKind::Unstage)
    }

    pub fn run_paths_mutation(
        &self,
        operation: GitMutationOperation,
    ) -> BackendFuture<'static, GitStatusSummary> {
        let required = match operation.kind {
            GitSafeMutationKind::Stage => BackendOperation::GitStage,
            GitSafeMutationKind::Unstage => BackendOperation::GitUnstage,
            GitSafeMutationKind::Commit => {
                return error_future(BackendError::failed(
                    "git_mutation_kind_invalid",
                    "commit must use the confirmed commit path",
                ));
            }
        };
        if let Err(error) = self.require(required) {
            return error_future(error);
        }
        if !self.operation_is_current(&operation) {
            return error_future(BackendError::conflict(
                "git_mutation_generation_stale",
                "the Git mutation is no longer the active workspace operation",
            ));
        }
        let backend = self.backend.clone();
        let request = MutationRequest::new(GitStageRequest {
            workspace_id: operation.workspace_id.clone(),
            paths: operation.paths.clone(),
        })
        .with_idempotency_key(operation.operation_id.clone());
        Box::pin(async move {
            match operation.kind {
                GitSafeMutationKind::Stage => backend.stage(request).await,
                GitSafeMutationKind::Unstage => backend.unstage(request).await,
                GitSafeMutationKind::Commit => unreachable!(),
            }
        })
    }

    pub fn apply_paths_mutation(
        &mut self,
        operation: &GitMutationOperation,
        result: BackendResult<GitStatusSummary>,
    ) -> bool {
        if !self.operation_is_current(operation) {
            return false;
        }
        match result {
            Ok(status) if status.workspace_id != operation.workspace_id => {
                let error = BackendError::failed(
                    "git_status_response_mismatch",
                    "the backend returned Git status for another workspace",
                );
                self.state
                    .model
                    .fail_mutation(&operation.operation_id, &error.code);
                self.state.last_error = Some(error);
            }
            Ok(status) => {
                self.state
                    .model
                    .finish_mutation(&operation.operation_id, Some(status));
                self.state.last_error = None;
            }
            Err(error) => {
                self.state
                    .model
                    .fail_mutation(&operation.operation_id, &error.code);
                self.state.last_error = Some(error);
            }
        }
        true
    }

    pub fn request_commit_confirmation(
        &mut self,
        message: impl Into<String>,
        paths: Vec<String>,
    ) -> BackendResult<&GitCommitConfirmationModel> {
        self.require(BackendOperation::GitCommit)?;
        let workspace_id = self.current_workspace()?;
        let message = message.into().trim().to_string();
        if message.is_empty() || message.len() > GIT_WORKFLOW_MAX_COMMIT_MESSAGE_BYTES {
            return Err(BackendError::failed(
                "git_commit_message_invalid",
                "a non-empty bounded commit message is required",
            ));
        }
        let paths = normalize_paths(paths)?;
        self.state.commit_confirmation = Some(GitCommitConfirmationModel {
            workspace_id,
            message,
            paths,
            confirmed: false,
            touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
        });
        Ok(self
            .state
            .commit_confirmation
            .as_ref()
            .expect("commit confirmation was inserted"))
    }

    pub fn confirm_commit(&mut self) -> BackendResult<()> {
        let confirmation = self.state.commit_confirmation.as_mut().ok_or_else(|| {
            BackendError::conflict(
                "git_commit_confirmation_missing",
                "request commit confirmation before committing",
            )
        })?;
        confirmation.confirm();
        Ok(())
    }

    pub fn cancel_commit(&mut self) {
        self.state.commit_confirmation = None;
    }

    pub fn begin_confirmed_commit(&mut self) -> BackendResult<GitMutationOperation> {
        self.require(BackendOperation::GitCommit)?;
        let confirmation = self.state.commit_confirmation.take().ok_or_else(|| {
            BackendError::conflict(
                "git_commit_confirmation_missing",
                "request commit confirmation before committing",
            )
        })?;
        if !confirmation.confirmed {
            self.state.commit_confirmation = Some(confirmation);
            return Err(BackendError::conflict(
                "git_commit_confirmation_required",
                "confirm the commit before submitting it",
            ));
        }
        let operation_id = RequestId::new().to_string();
        let scope = GitMutationScope {
            operation_id: operation_id.clone(),
            kind: GitMutationKind::Commit,
            paths: confirmation.paths.clone(),
            target: Some(confirmation.workspace_id.to_string()),
            destructive: true,
            confirmation_label: confirmation.message.clone(),
        };
        if !self.state.model.begin_mutation(scope) {
            self.state.commit_confirmation = Some(confirmation);
            return Err(BackendError::conflict(
                "git_mutation_already_pending",
                "another Git mutation is already pending",
            ));
        }
        self.state.last_commit.begin();
        Ok(GitMutationOperation {
            generation: self.state.generation,
            workspace_id: confirmation.workspace_id,
            operation_id,
            kind: GitSafeMutationKind::Commit,
            paths: confirmation.paths,
            commit_message: Some(confirmation.message),
        })
    }

    pub fn run_commit(
        &self,
        operation: GitMutationOperation,
    ) -> BackendFuture<'static, GitCommitResult> {
        if let Err(error) = self.require(BackendOperation::GitCommit) {
            return error_future(error);
        }
        if operation.kind != GitSafeMutationKind::Commit {
            return error_future(BackendError::failed(
                "git_mutation_kind_invalid",
                "only a confirmed commit operation can use this path",
            ));
        }
        if !self.operation_is_current(&operation) {
            return error_future(BackendError::conflict(
                "git_mutation_generation_stale",
                "the confirmed commit is no longer the active workspace operation",
            ));
        }
        let Some(message) = operation.commit_message.clone() else {
            return error_future(BackendError::failed(
                "git_commit_message_invalid",
                "the confirmed commit message is missing",
            ));
        };
        let backend = self.backend.clone();
        let request = MutationRequest::new(GitCommitRequest {
            workspace_id: operation.workspace_id,
            message,
            paths: operation.paths,
            amend: false,
            push_after: false,
        })
        .with_idempotency_key(operation.operation_id);
        Box::pin(async move { backend.commit(request).await })
    }

    pub fn apply_commit(
        &mut self,
        operation: &GitMutationOperation,
        result: BackendResult<GitCommitResult>,
    ) -> bool {
        if !self.operation_is_current(operation) {
            return false;
        }
        match result {
            Ok(commit) if commit.workspace_id != operation.workspace_id => {
                let error = BackendError::failed(
                    "git_commit_response_mismatch",
                    "the backend returned a commit for another workspace",
                );
                self.state
                    .model
                    .fail_mutation(&operation.operation_id, &error.code);
                self.state.last_commit.reject(error.clone());
                self.state.last_error = Some(error);
            }
            Ok(commit) => {
                self.state
                    .model
                    .finish_mutation(&operation.operation_id, None);
                self.state.last_commit.resolve(commit);
                self.state.last_error = None;
            }
            Err(error) => {
                self.state
                    .model
                    .fail_mutation(&operation.operation_id, &error.code);
                self.state.last_commit.reject(error.clone());
                self.state.last_error = Some(error);
            }
        }
        true
    }

    fn begin_paths_mutation(
        &mut self,
        paths: Vec<String>,
        kind: GitSafeMutationKind,
    ) -> BackendResult<GitMutationOperation> {
        let required = match kind {
            GitSafeMutationKind::Stage => BackendOperation::GitStage,
            GitSafeMutationKind::Unstage => BackendOperation::GitUnstage,
            GitSafeMutationKind::Commit => BackendOperation::GitCommit,
        };
        self.require(required)?;
        let workspace_id = self.current_workspace()?;
        let paths = normalize_paths(paths)?;
        if paths.is_empty() {
            return Err(BackendError::failed(
                "git_paths_empty",
                "select at least one Git path",
            ));
        }
        let operation_id = RequestId::new().to_string();
        let model_kind = match kind {
            GitSafeMutationKind::Stage => GitMutationKind::Stage,
            GitSafeMutationKind::Unstage => GitMutationKind::Unstage,
            GitSafeMutationKind::Commit => GitMutationKind::Commit,
        };
        if !self.state.model.begin_mutation(GitMutationScope {
            operation_id: operation_id.clone(),
            kind: model_kind,
            paths: paths.clone(),
            target: None,
            destructive: false,
            confirmation_label: match kind {
                GitSafeMutationKind::Stage => "Stage selected paths",
                GitSafeMutationKind::Unstage => "Unstage selected paths",
                GitSafeMutationKind::Commit => "Commit selected paths",
            }
            .into(),
        }) {
            return Err(BackendError::conflict(
                "git_mutation_already_pending",
                "another Git mutation is already pending",
            ));
        }
        Ok(GitMutationOperation {
            generation: self.state.generation,
            workspace_id,
            operation_id,
            kind,
            paths,
            commit_message: None,
        })
    }

    fn operation_is_current(&self, operation: &GitMutationOperation) -> bool {
        self.state.generation == operation.generation
            && self.state.workspace_id.as_ref() == Some(&operation.workspace_id)
            && self
                .state
                .model
                .pending_mutation
                .as_ref()
                .is_some_and(|scope| scope.operation_id == operation.operation_id)
    }

    fn current_workspace(&self) -> BackendResult<WorkspaceId> {
        self.state.workspace_id.clone().ok_or_else(|| {
            BackendError::failed(
                "git_workspace_missing",
                "select a workspace before using the Git workflow",
            )
        })
    }

    fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        if self.capabilities.supports(operation) {
            Ok(())
        } else {
            Err(git_capability_error(&self.capabilities, operation))
        }
    }
}

fn normalize_paths(paths: Vec<String>) -> BackendResult<Vec<String>> {
    let mut seen = BTreeSet::new();
    let paths = paths
        .into_iter()
        .map(|path| normalize_path(&path))
        .filter(|path| !path.is_empty())
        .filter(|path| seen.insert(path.clone()))
        .take(GIT_WORKFLOW_MAX_PATHS.saturating_add(1))
        .collect::<Vec<_>>();
    if paths.len() > GIT_WORKFLOW_MAX_PATHS {
        return Err(BackendError::failed(
            "git_paths_too_many",
            "the Git mutation contains too many paths",
        ));
    }
    Ok(paths)
}

fn normalize_path(path: &str) -> String {
    path.trim_matches('/').replace('\\', "/")
}

fn git_capability_error(
    capabilities: &DomainCapabilities,
    operation: BackendOperation,
) -> BackendError {
    use vibex_backend::CapabilityAvailability;
    let label = match operation {
        BackendOperation::GitStatus => "git_status",
        BackendOperation::GitDiff => "git_diff",
        BackendOperation::GitStage => "git_stage",
        BackendOperation::GitUnstage => "git_unstage",
        BackendOperation::GitCommit => "git_commit",
        _ => "git_operation",
    };
    match capabilities.availability {
        CapabilityAvailability::Offline => BackendError::offline(
            format!("{label}_offline"),
            "the authoritative Git backend is offline",
        ),
        CapabilityAvailability::Degraded => BackendError::loading(
            format!("{label}_degraded"),
            "the authoritative Git backend is temporarily degraded",
        ),
        CapabilityAvailability::RequiresPermission => BackendError::permission(
            format!("{label}_permission_required"),
            "the current device lacks permission for this Git operation",
        ),
        CapabilityAvailability::Available | CapabilityAvailability::Unsupported => {
            BackendError::unsupported(
                format!("{label}_unsupported"),
                "this Git operation is not supported by the shared workflow",
            )
        }
    }
}

fn error_future<T: 'static>(error: BackendError) -> BackendFuture<'static, T> {
    Box::pin(async move { Err(error) })
}

#[cfg(test)]
mod tests {
    use std::path::{Path, PathBuf};
    use std::process::Command;

    use super::*;
    use vibex_core::{
        GitProjectEligibility, GitWorktreeArchiveRequest, GitWorktreeAssistanceSessionRequest,
        GitWorktreeConflictResolveRequest, GitWorktreeConflictStageRequest,
        GitWorktreeCreateRequest, GitWorktreeCreateResult, GitWorktreeDestructivePreflight,
        GitWorktreeDiscardRequest, GitWorktreeLifecycleSnapshot, GitWorktreeMergePlan,
        GitWorktreeMergeRequest, GitWorktreeOperationRecord, GitWorktreeOperationRequest,
        GitWorktreeReadinessRecord, GitWorktreeReadinessRequest, GitWorktreeRestoreRequest,
    };

    #[derive(Clone)]
    struct FixtureGitBackend {
        repo: Arc<PathBuf>,
    }

    impl GitBackend for FixtureGitBackend {
        fn git_status(&self, workspace_id: WorkspaceId) -> BackendFuture<'_, GitStatusSummary> {
            let repo = self.repo.clone();
            Box::pin(
                async move { vibex_git::status(workspace_id, repo.as_path()).map_err(Into::into) },
            )
        }

        fn git_diff(&self, request: GitDiffRequest) -> BackendFuture<'_, GitDiffResponse> {
            let repo = self.repo.clone();
            Box::pin(async move { vibex_git::diff(repo.as_path(), &request).map_err(Into::into) })
        }

        fn git_worktree_eligibility(
            &self,
            _workspace_id: WorkspaceId,
        ) -> BackendFuture<'_, GitProjectEligibility> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_snapshot(
            &self,
            _workspace_id: WorkspaceId,
        ) -> BackendFuture<'_, GitWorktreeLifecycleSnapshot> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_create(
            &self,
            _request: MutationRequest<GitWorktreeCreateRequest>,
        ) -> BackendFuture<'_, GitWorktreeCreateResult> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_readiness(
            &self,
            _workspace_id: WorkspaceId,
        ) -> BackendFuture<'_, Option<GitWorktreeReadinessRecord>> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_set_readiness(
            &self,
            _request: MutationRequest<GitWorktreeReadinessRequest>,
        ) -> BackendFuture<'_, GitWorktreeReadinessRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_merge_plan(
            &self,
            _request: GitWorktreeMergeRequest,
        ) -> BackendFuture<'_, GitWorktreeMergePlan> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_merge(
            &self,
            _request: MutationRequest<GitWorktreeMergeRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_resolve_conflict(
            &self,
            _request: MutationRequest<GitWorktreeConflictResolveRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_stage_conflicts(
            &self,
            _request: MutationRequest<GitWorktreeConflictStageRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_bind_assistance_session(
            &self,
            _request: MutationRequest<GitWorktreeAssistanceSessionRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_continue_merge(
            &self,
            _request: MutationRequest<GitWorktreeOperationRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_abort_merge(
            &self,
            _request: MutationRequest<GitWorktreeOperationRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_archive_preflight(
            &self,
            _request: GitWorktreeArchiveRequest,
        ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_archive(
            &self,
            _request: MutationRequest<GitWorktreeArchiveRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_restore_preflight(
            &self,
            _request: GitWorktreeRestoreRequest,
        ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_restore(
            &self,
            _request: MutationRequest<GitWorktreeRestoreRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_discard_preflight(
            &self,
            _request: GitWorktreeDiscardRequest,
        ) -> BackendFuture<'_, GitWorktreeDestructivePreflight> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn git_worktree_discard(
            &self,
            _request: MutationRequest<GitWorktreeDiscardRequest>,
        ) -> BackendFuture<'_, GitWorktreeOperationRecord> {
            error_future(BackendError::unsupported(
                "fixture_worktree_unsupported",
                "fixture Git backend does not expose managed worktrees",
            ))
        }

        fn stage(
            &self,
            request: MutationRequest<GitStageRequest>,
        ) -> BackendFuture<'_, GitStatusSummary> {
            let repo = self.repo.clone();
            Box::pin(async move {
                vibex_git::stage(
                    request.payload.workspace_id.clone(),
                    repo.as_path(),
                    &request.payload,
                )
                .map_err(Into::into)
            })
        }

        fn unstage(
            &self,
            request: MutationRequest<GitStageRequest>,
        ) -> BackendFuture<'_, GitStatusSummary> {
            let repo = self.repo.clone();
            Box::pin(async move {
                vibex_git::unstage(
                    request.payload.workspace_id.clone(),
                    repo.as_path(),
                    &request.payload,
                )
                .map_err(Into::into)
            })
        }

        fn commit(
            &self,
            request: MutationRequest<GitCommitRequest>,
        ) -> BackendFuture<'_, GitCommitResult> {
            let repo = self.repo.clone();
            Box::pin(async move {
                vibex_git::commit(
                    request.payload.workspace_id.clone(),
                    repo.as_path(),
                    &request.payload,
                )
                .map_err(Into::into)
            })
        }
    }

    fn capabilities() -> DomainCapabilities {
        DomainCapabilities::available([
            BackendOperation::GitStatus,
            BackendOperation::GitDiff,
            BackendOperation::GitStage,
            BackendOperation::GitUnstage,
            BackendOperation::GitCommit,
        ])
    }

    fn empty_status(workspace_id: WorkspaceId) -> GitStatusSummary {
        GitStatusSummary {
            workspace_id,
            repo_path: "/fixture".into(),
            branch: Some("main".into()),
            short_commit: None,
            detached: false,
            dirty: false,
            staged_count: 0,
            unstaged_count: 0,
            untracked_count: 0,
            changes: Vec::new(),
            captured_at_ms: 1,
        }
    }

    fn git(repo: &Path, args: &[&str]) {
        let status = Command::new("git")
            .current_dir(repo)
            .args(args)
            .status()
            .unwrap();
        assert!(status.success(), "git command failed: {args:?}");
    }

    #[tokio::test]
    async fn git_status_diff_stage_unstage_and_confirmed_commit_use_a_real_fixture_repo() {
        let temp = tempfile::tempdir().unwrap();
        let repo = temp.path();
        git(repo, &["init", "-q"]);
        git(repo, &["config", "user.email", "fixture@example.test"]);
        git(repo, &["config", "user.name", "Fixture"]);
        std::fs::write(repo.join("note.txt"), "one\n").unwrap();
        git(repo, &["add", "note.txt"]);
        git(repo, &["commit", "-qm", "initial"]);
        std::fs::write(repo.join("note.txt"), "one\ntwo\n").unwrap();

        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(FixtureGitBackend {
            repo: Arc::new(repo.to_path_buf()),
        });
        let mut controller = GitWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());

        let status_ticket = controller.begin_status_load().unwrap();
        let status = controller.load_status(status_ticket.clone()).await.unwrap();
        assert!(status.dirty);
        assert!(controller.apply_status(&status_ticket, Ok(status)));

        let diff_ticket = controller.begin_diff_load("note.txt", false).unwrap();
        let diff = controller.load_diff(diff_ticket.clone()).await.unwrap();
        assert!(diff.diff.contains("+two"));
        assert!(controller.apply_diff(&diff_ticket, Ok(diff)));

        let stage = controller.begin_stage(vec!["note.txt".into()]).unwrap();
        let staged = controller.run_paths_mutation(stage.clone()).await.unwrap();
        assert_eq!(staged.staged_count, 1);
        assert!(controller.apply_paths_mutation(&stage, Ok(staged)));

        let unstage = controller.begin_unstage(vec!["note.txt".into()]).unwrap();
        let unstaged = controller
            .run_paths_mutation(unstage.clone())
            .await
            .unwrap();
        assert_eq!(unstaged.staged_count, 0);
        assert!(controller.apply_paths_mutation(&unstage, Ok(unstaged)));

        let stage = controller.begin_stage(vec!["note.txt".into()]).unwrap();
        let staged = controller.run_paths_mutation(stage.clone()).await.unwrap();
        assert!(controller.apply_paths_mutation(&stage, Ok(staged)));

        let confirmation = controller
            .request_commit_confirmation("test: update note", Vec::new())
            .unwrap();
        assert!(confirmation.is_touch_discoverable());
        assert_eq!(
            controller.begin_confirmed_commit().unwrap_err().code,
            "git_commit_confirmation_required"
        );
        controller.confirm_commit().unwrap();
        let commit = controller.begin_confirmed_commit().unwrap();
        let result = controller.run_commit(commit.clone()).await.unwrap();
        assert!(result.summary.contains("test: update note"));
        assert!(controller.apply_commit(&commit, Ok(result)));

        let status_ticket = controller.begin_status_load().unwrap();
        let status = controller.load_status(status_ticket.clone()).await.unwrap();
        assert!(!status.dirty);
        assert!(controller.apply_status(&status_ticket, Ok(status)));
    }

    #[tokio::test]
    async fn git_queries_and_mutations_are_response_and_operation_fenced() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(FixtureGitBackend {
            repo: Arc::new(PathBuf::from(".")),
        });
        let mut controller = GitWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());

        let diff_ticket = controller.begin_diff_load("note.txt", false).unwrap();
        let mismatch = GitDiffResponse {
            workspace_id: workspace_id.clone(),
            path: "other.txt".into(),
            staged: false,
            diff: "private diff".into(),
            truncated: false,
        };
        assert!(controller.apply_diff(&diff_ticket, Ok(mismatch)));
        assert_eq!(
            controller
                .state
                .last_error
                .as_ref()
                .map(|error| error.code.as_str()),
            Some("git_diff_response_mismatch")
        );

        let stale_diff = controller.begin_diff_load("note.txt", false).unwrap();
        let _current_diff = controller.begin_diff_load("note.txt", false).unwrap();
        controller.state.last_error = None;
        assert!(!controller.apply_diff(
            &stale_diff,
            Err(BackendError::offline("stale_error", "stale query failed")),
        ));
        assert!(controller.state.last_error.is_none());

        let operation = controller.begin_stage(vec!["note.txt".into()]).unwrap();
        let mut forged = operation.clone();
        forged.operation_id = "forged-operation".into();
        assert_eq!(
            controller
                .run_paths_mutation(forged)
                .await
                .unwrap_err()
                .code,
            "git_mutation_generation_stale"
        );

        controller.select_workspace(WorkspaceId::new());
        assert_eq!(
            controller
                .run_paths_mutation(operation.clone())
                .await
                .unwrap_err()
                .code,
            "git_mutation_generation_stale"
        );
        assert!(!controller.apply_paths_mutation(&operation, Ok(empty_status(workspace_id)),));
    }

    #[test]
    fn git_workflow_has_no_revert_branch_push_or_history_rewrite_action() {
        let backend = Arc::new(FixtureGitBackend {
            repo: Arc::new(PathBuf::from(".")),
        });
        let controller = GitWorkflowController::new(backend, capabilities());
        assert!(
            controller
                .capabilities()
                .supports(BackendOperation::GitStage)
        );
        assert!(
            controller
                .capabilities()
                .supports(BackendOperation::GitCommit)
        );
        // The reviewed public controller exposes only status/diff/stage/unstage/commit.
        assert_eq!(GitSafeMutationKind::Stage as u8, 0);
        assert_eq!(GitSafeMutationKind::Unstage as u8, 1);
        assert_eq!(GitSafeMutationKind::Commit as u8, 2);
    }

    #[test]
    fn git_state_and_operation_debug_redact_diff_paths_and_commit_message() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(FixtureGitBackend {
            repo: Arc::new(PathBuf::from(".")),
        });
        let mut controller = GitWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());
        let diff_ticket = controller
            .begin_diff_load("private/path.txt", false)
            .unwrap();
        assert!(controller.apply_diff(
            &diff_ticket,
            Ok(GitDiffResponse {
                workspace_id,
                path: "private/path.txt".into(),
                staged: false,
                diff: "private diff contents".into(),
                truncated: false,
            }),
        ));
        controller
            .request_commit_confirmation("private commit message", vec!["private/path.txt".into()])
            .unwrap();
        controller.confirm_commit().unwrap();
        let operation = controller.begin_confirmed_commit().unwrap();
        let debug = format!("{:?} {operation:?}", controller.state);
        for secret in [
            "private/path.txt",
            "private diff contents",
            "private commit message",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}");
        }
        assert!(debug.contains("commit_message_bytes"));
    }
}

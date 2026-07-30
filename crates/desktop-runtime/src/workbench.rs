use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::sync::{Arc, Mutex};

use vibex_core::{
    FileMutationRequest, FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult,
    FileTreeEntry, FileTreeRequest, FileWriteRequest, GitBlameRequest, GitBlameResponse,
    GitBranchCheckoutRequest, GitBranchCreateRequest, GitBranchListResponse, GitCommitDetail,
    GitCommitDetailRequest, GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse,
    GitHistoryRequest, GitHistoryResponse, GitManagedWorktreeStatus, GitRemoteActionRequest,
    GitRemoteActionResult, GitStageRequest, GitStatusSummary, GitWorktreeCreateRequest,
    GitWorktreeCreateResult, GitWorktreeDiscardRequest, GitWorktreeListResponse,
    GitWorktreeMergeRequest, GitWorktreeOperationKind, GitWorktreeOperationRecord,
    GitWorktreeOperationStatus, ProjectId, RequestId, VibexError, VibexResult, WorkspaceId,
    WorkspaceMode, WorkspaceRecord, unix_timestamp_ms,
};
use vibex_db::{
    GitSnapshotRepository, ManagedWorktreeRecord, ManagedWorktreeRepository, WorkspaceRepository,
    WorktreeOperationRepository, open_database,
};
use vibex_fs::{MAX_NATIVE_TREE_ENTRIES, WorkspaceFileService};

use crate::{FileHandle, GitHandle};

impl FileHandle {
    pub fn list_tree(&self, request: &FileTreeRequest) -> VibexResult<Vec<FileTreeEntry>> {
        self.service(&request.workspace_id)?.list_tree(request)
    }

    pub fn list_native_tree(&self, request: &FileTreeRequest) -> VibexResult<Vec<FileTreeEntry>> {
        self.service(&request.workspace_id)?
            .list_tree_with_limit(request, MAX_NATIVE_TREE_ENTRIES)
    }

    pub fn read(&self, request: &FileReadRequest) -> VibexResult<FileReadResponse> {
        self.service(&request.workspace_id)?.read_file(request)
    }

    pub fn read_bytes(
        &self,
        workspace_id: &WorkspaceId,
        path: &str,
        max_bytes: usize,
    ) -> VibexResult<Vec<u8>> {
        self.service(workspace_id)?
            .read_bytes(workspace_id, path, max_bytes)
    }

    pub fn write(&self, request: &FileWriteRequest) -> VibexResult<FileReadResponse> {
        self.service(&request.workspace_id)?.write_file(request)
    }

    pub fn create_directory(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.service(&request.workspace_id)?
            .create_directory(request)
    }

    pub fn copy(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.service(&request.workspace_id)?.copy_path(request)
    }

    pub fn rename(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.service(&request.workspace_id)?.rename_path(request)
    }

    pub fn delete(&self, request: &FileMutationRequest) -> VibexResult<()> {
        self.service(&request.workspace_id)?.delete_path(request)
    }

    pub fn search(&self, request: &FileSearchRequest) -> VibexResult<Vec<FileSearchResult>> {
        self.service(&request.workspace_id)?.search(request)
    }

    pub fn resolve_existing_path(
        &self,
        workspace_id: &WorkspaceId,
        path: &str,
    ) -> VibexResult<PathBuf> {
        self.service(workspace_id)?
            .resolve_existing_path(workspace_id, path)
    }

    fn service(&self, workspace_id: &WorkspaceId) -> VibexResult<WorkspaceFileService> {
        let connection = open_database(&self.db_path)?;
        let (_, workspace) =
            WorkspaceRepository::get(&connection, workspace_id)?.ok_or_else(|| {
                VibexError::validation("workspace_not_found", "workspace was not found")
            })?;
        WorkspaceFileService::new(workspace.root_path, workspace_id.clone())
    }
}

#[derive(Debug)]
struct GitMutationClaim {
    claims: Arc<Mutex<BTreeSet<String>>>,
    key: String,
}

impl GitMutationClaim {
    fn claim(
        claims: Arc<Mutex<BTreeSet<String>>>,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<Self> {
        let key = workspace_id.as_str().to_string();
        let mut active = claims
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if !active.insert(key.clone()) {
            return Err(VibexError::conflict(
                "git_mutation_in_progress",
                "another Git mutation is already in progress for this workspace",
            )
            .with_diagnostic("workspaceId", workspace_id.as_str()));
        }
        drop(active);
        Ok(Self { claims, key })
    }
}

impl Drop for GitMutationClaim {
    fn drop(&mut self) {
        self.claims
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove(&self.key);
    }
}

impl GitHandle {
    pub fn status(&self, workspace_id: &WorkspaceId) -> VibexResult<GitStatusSummary> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, workspace_id)?.1;
        let status = vibex_git::status(workspace.id, &workspace.root_path)?;
        persist_git_snapshot(&connection, &status)?;
        Ok(status)
    }

    pub fn diff(&self, request: &GitDiffRequest) -> VibexResult<GitDiffResponse> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        vibex_git::diff(&workspace.root_path, request)
    }

    pub fn history(&self, request: &GitHistoryRequest) -> VibexResult<GitHistoryResponse> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        vibex_git::history(&workspace.root_path, request)
    }

    pub fn commit_detail(&self, request: &GitCommitDetailRequest) -> VibexResult<GitCommitDetail> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        vibex_git::commit_detail(&workspace.root_path, request)
    }

    pub fn blame(&self, request: &GitBlameRequest) -> VibexResult<GitBlameResponse> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        vibex_git::blame(&workspace.root_path, request)
    }

    pub fn branch_list(&self, workspace_id: &WorkspaceId) -> VibexResult<GitBranchListResponse> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, workspace_id)?.1;
        vibex_git::branch_list(workspace.id, &workspace.root_path)
    }

    pub fn stage(&self, request: &GitStageRequest) -> VibexResult<GitStatusSummary> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        self.stage_inner(request, GitStageOperation::Stage)
    }

    pub fn unstage(&self, request: &GitStageRequest) -> VibexResult<GitStatusSummary> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        self.stage_inner(request, GitStageOperation::Unstage)
    }

    pub fn revert(&self, request: &GitStageRequest) -> VibexResult<GitStatusSummary> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        self.stage_inner(request, GitStageOperation::Revert)
    }

    fn stage_inner(
        &self,
        request: &GitStageRequest,
        operation: GitStageOperation,
    ) -> VibexResult<GitStatusSummary> {
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        let status = match operation {
            GitStageOperation::Stage => {
                vibex_git::stage(workspace.id.clone(), &workspace.root_path, request)?
            }
            GitStageOperation::Unstage => {
                vibex_git::unstage(workspace.id.clone(), &workspace.root_path, request)?
            }
            GitStageOperation::Revert => {
                vibex_git::revert(workspace.id.clone(), &workspace.root_path, request)?
            }
        };
        persist_git_snapshot(&connection, &status)?;
        Ok(status)
    }

    pub fn commit(&self, request: &GitCommitRequest) -> VibexResult<GitCommitResult> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        let result = vibex_git::commit(workspace.id.clone(), &workspace.root_path, request)?;
        let status = vibex_git::status(workspace.id, &workspace.root_path)?;
        persist_git_snapshot(&connection, &status)?;
        Ok(result)
    }

    pub fn branch_create(&self, request: &GitBranchCreateRequest) -> VibexResult<GitStatusSummary> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        let status = vibex_git::branch_create(workspace.id, &workspace.root_path, request)?;
        persist_git_snapshot(&connection, &status)?;
        Ok(status)
    }

    pub fn branch_checkout(
        &self,
        request: &GitBranchCheckoutRequest,
    ) -> VibexResult<GitStatusSummary> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        let status = vibex_git::branch_checkout(workspace.id, &workspace.root_path, request)?;
        persist_git_snapshot(&connection, &status)?;
        Ok(status)
    }

    pub fn remote_action(
        &self,
        request: &GitRemoteActionRequest,
    ) -> VibexResult<GitRemoteActionResult> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let workspace = workspace_record(&connection, &request.workspace_id)?.1;
        let result = vibex_git::remote_action(workspace.id, &workspace.root_path, request)?;
        if let Some(status) = result.status_after.as_ref() {
            persist_git_snapshot(&connection, status)?;
        }
        Ok(result)
    }

    pub fn worktree_list(
        &self,
        workspace_id: &WorkspaceId,
    ) -> VibexResult<GitWorktreeListResponse> {
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, workspace_id)?;
        let mut response = vibex_git::worktree_list(workspace.id, &workspace.root_path)?;
        let managed = ManagedWorktreeRepository::list_for_project(&connection, &project.id)?;
        for worktree in &mut response.worktrees {
            if let Some(record) = managed
                .iter()
                .find(|record| same_path_text(&record.worktree_path, &worktree.path))
            {
                worktree.managed = true;
                worktree.workspace_id = record.workspace_id.clone();
            }
        }
        Ok(response)
    }

    pub fn worktree_create(
        &self,
        request: &GitWorktreeCreateRequest,
    ) -> VibexResult<GitWorktreeCreateResult> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, &request.workspace_id)?;
        let branch = request.branch_name.clone();
        let operation = insert_worktree_operation(
            &connection,
            project.id.clone(),
            Some(workspace.id.clone()),
            None,
            GitWorktreeOperationKind::Create,
            None,
            Some(branch.clone()),
            request.base_ref.clone(),
            current_head(&workspace.root_path).ok(),
        )?;
        WorktreeOperationRepository::update(
            &connection,
            &operation.operation_id,
            GitWorktreeOperationStatus::Running,
            None,
            None,
        )?;
        let worktree_path =
            self.managed_worktree_path(&project.id, request.name.as_deref().unwrap_or(&branch))?;
        if let Some(parent) = worktree_path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                VibexError::storage(
                    "worktree_parent_create_failed",
                    "failed to create managed worktree directory",
                )
                .with_diagnostic("path", parent.display().to_string())
                .with_diagnostic("error", error.to_string())
            })?;
        }
        match vibex_git::worktree_add(&workspace.root_path, &worktree_path, request) {
            Ok(mut worktree) => {
                let worktree_workspace = WorkspaceRepository::ensure_for_project(
                    &connection,
                    &project.id,
                    &worktree.path,
                    WorkspaceMode::VibexWorktree,
                )?;
                worktree.managed = true;
                worktree.workspace_id = Some(worktree_workspace.id.clone());
                let now = unix_timestamp_ms();
                ManagedWorktreeRepository::insert(
                    &connection,
                    &ManagedWorktreeRecord {
                        worktree_id: RequestId::new(),
                        project_id: project.id,
                        workspace_id: Some(worktree_workspace.id.clone()),
                        repo_root: workspace.root_path,
                        worktree_path: worktree.path.clone(),
                        branch: worktree.branch.clone().or(Some(branch)),
                        base_ref: request.base_ref.clone(),
                        head: worktree.head.clone(),
                        status: GitManagedWorktreeStatus::Active,
                        created_at_ms: now,
                        updated_at_ms: now,
                        closed_at_ms: None,
                    },
                )?;
                WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Completed,
                    worktree.head.as_deref(),
                    None,
                )?;
                Ok(GitWorktreeCreateResult {
                    worktree,
                    workspace: worktree_workspace,
                })
            }
            Err(error) => {
                let _ = WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Failed,
                    None,
                    Some(&error.message),
                );
                Err(error)
            }
        }
    }

    pub fn worktree_merge(
        &self,
        request: &GitWorktreeMergeRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let (source_project, source_workspace) =
            workspace_record(&connection, &request.workspace_id)?;
        let managed = ManagedWorktreeRepository::get_by_path(&connection, &request.source_path)?
            .ok_or_else(|| {
                VibexError::validation("worktree_not_managed", "worktree is not managed by Vibex")
            })?;
        if managed.project_id != source_project.id {
            return Err(VibexError::validation(
                "worktree_project_mismatch",
                "managed worktree belongs to a different project",
            ));
        }
        let target_workspace_id = match request.target_workspace_id.clone() {
            Some(workspace_id) => workspace_id,
            None => source_project_workspace_id(&connection, &source_project.id)
                .unwrap_or_else(|_| source_workspace.id.clone()),
        };
        let target_workspace = workspace_record(&connection, &target_workspace_id)?.1;
        let operation = insert_worktree_operation(
            &connection,
            source_project.id,
            Some(source_workspace.id),
            Some(target_workspace.id.clone()),
            GitWorktreeOperationKind::MergeBack,
            Some(managed.worktree_path.clone()),
            managed.branch.clone(),
            managed.base_ref.clone(),
            current_head(&target_workspace.root_path).ok(),
        )?;
        WorktreeOperationRepository::update(
            &connection,
            &operation.operation_id,
            GitWorktreeOperationStatus::Running,
            None,
            None,
        )?;
        let source_ref = managed
            .branch
            .as_deref()
            .or(managed.head.as_deref())
            .ok_or_else(|| {
                VibexError::validation(
                    "worktree_source_ref_missing",
                    "managed worktree has no branch or head",
                )
            })?;
        match vibex_git::worktree_merge_preflight(&target_workspace.root_path, request)
            .and_then(|_| vibex_git::worktree_merge(&target_workspace.root_path, source_ref))
        {
            Ok(_) => {
                let head_after = current_head(&target_workspace.root_path).ok();
                ManagedWorktreeRepository::update_status(
                    &connection,
                    &managed.worktree_path,
                    GitManagedWorktreeStatus::Merged,
                    head_after.as_deref(),
                    Some(unix_timestamp_ms()),
                )?;
                WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Completed,
                    head_after.as_deref(),
                    None,
                )
            }
            Err(error) => {
                WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Failed,
                    None,
                    Some(&error.message),
                )?;
                Err(error)
            }
        }
    }

    pub fn worktree_discard(
        &self,
        request: &GitWorktreeDiscardRequest,
    ) -> VibexResult<GitWorktreeOperationRecord> {
        let _claim = GitMutationClaim::claim(self.mutation_claims.clone(), &request.workspace_id)?;
        let connection = open_database(&self.db_path)?;
        let (project, workspace) = workspace_record(&connection, &request.workspace_id)?;
        let managed = ManagedWorktreeRepository::get_by_path(&connection, &request.worktree_path)?
            .ok_or_else(|| {
                VibexError::validation("worktree_not_managed", "worktree is not managed by Vibex")
            })?;
        if managed.project_id != project.id {
            return Err(VibexError::validation(
                "worktree_project_mismatch",
                "managed worktree belongs to a different project",
            ));
        }
        let operation = insert_worktree_operation(
            &connection,
            project.id,
            Some(workspace.id),
            None,
            GitWorktreeOperationKind::Discard,
            Some(managed.worktree_path.clone()),
            managed.branch.clone(),
            managed.base_ref.clone(),
            managed.head.clone(),
        )?;
        WorktreeOperationRepository::update(
            &connection,
            &operation.operation_id,
            GitWorktreeOperationStatus::Running,
            None,
            None,
        )?;
        match vibex_git::worktree_remove(&managed.repo_root, request) {
            Ok(_) => {
                ManagedWorktreeRepository::update_status(
                    &connection,
                    &managed.worktree_path,
                    GitManagedWorktreeStatus::Discarded,
                    managed.head.as_deref(),
                    Some(unix_timestamp_ms()),
                )?;
                WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Completed,
                    managed.head.as_deref(),
                    None,
                )
            }
            Err(error) => {
                WorktreeOperationRepository::update(
                    &connection,
                    &operation.operation_id,
                    GitWorktreeOperationStatus::Failed,
                    None,
                    Some(&error.message),
                )?;
                Err(error)
            }
        }
    }

    fn managed_worktree_path(&self, project_id: &ProjectId, name: &str) -> VibexResult<PathBuf> {
        let root = self.db_path.parent().ok_or_else(|| {
            VibexError::storage(
                "desktop_runtime_home_parent_missing",
                "desktop runtime database has no home directory",
            )
        })?;
        let request_id = RequestId::new();
        let short_id = request_id
            .as_str()
            .rsplit('_')
            .next()
            .unwrap_or(request_id.as_str())
            .chars()
            .take(8)
            .collect::<String>();
        Ok(root
            .join("worktrees")
            .join(project_id.as_str())
            .join(format!("{}-{short_id}", safe_path_slug(name))))
    }
}

#[derive(Debug, Clone, Copy)]
enum GitStageOperation {
    Stage,
    Unstage,
    Revert,
}

fn workspace_record(
    connection: &vibex_db::DbConnection,
    workspace_id: &WorkspaceId,
) -> VibexResult<(vibex_core::ProjectRecord, WorkspaceRecord)> {
    WorkspaceRepository::get(connection, workspace_id)?
        .ok_or_else(|| VibexError::validation("workspace_not_found", "workspace was not found"))
}

fn source_project_workspace_id(
    connection: &vibex_db::DbConnection,
    project_id: &ProjectId,
) -> VibexResult<WorkspaceId> {
    WorkspaceRepository::list(connection)?
        .into_iter()
        .find(|(_, workspace)| {
            &workspace.project_id == project_id && workspace.mode == WorkspaceMode::CurrentCheckout
        })
        .map(|(_, workspace)| workspace.id)
        .ok_or_else(|| {
            VibexError::validation(
                "target_workspace_not_found",
                "target checkout workspace was not found",
            )
        })
}

#[allow(clippy::too_many_arguments)]
fn insert_worktree_operation(
    connection: &vibex_db::DbConnection,
    project_id: ProjectId,
    source_workspace_id: Option<WorkspaceId>,
    target_workspace_id: Option<WorkspaceId>,
    operation: GitWorktreeOperationKind,
    worktree_path: Option<String>,
    branch: Option<String>,
    base_ref: Option<String>,
    head_before: Option<String>,
) -> VibexResult<GitWorktreeOperationRecord> {
    let now = unix_timestamp_ms();
    let record = GitWorktreeOperationRecord {
        operation_id: RequestId::new(),
        project_id,
        source_workspace_id,
        target_workspace_id,
        operation,
        status: GitWorktreeOperationStatus::Pending,
        worktree_path,
        branch,
        base_ref,
        head_before,
        head_after: None,
        error: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    WorktreeOperationRepository::insert(connection, &record)?;
    Ok(record)
}

fn persist_git_snapshot(
    connection: &vibex_db::DbConnection,
    status: &GitStatusSummary,
) -> VibexResult<()> {
    GitSnapshotRepository::upsert(
        connection,
        &status.workspace_id,
        status.branch.as_deref(),
        status.short_commit.as_deref(),
        status.dirty,
        status.changes.len() as u32,
        status.captured_at_ms,
    )
}

fn current_head(repo_path: impl AsRef<Path>) -> VibexResult<String> {
    vibex_git::status(WorkspaceId::new(), repo_path).and_then(|status| {
        status
            .short_commit
            .ok_or_else(|| VibexError::validation("git_head_missing", "Git HEAD is not available"))
    })
}

fn same_path_text(left: &str, right: &str) -> bool {
    let left = Path::new(left);
    let right = Path::new(right);
    if left == right {
        return true;
    }
    match (left.canonicalize(), right.canonicalize()) {
        (Ok(left), Ok(right)) => left == right,
        _ => normalized_path_text(left) == normalized_path_text(right),
    }
}

fn normalized_path_text(path: &Path) -> String {
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !normalized.has_root() {
                    normalized.push("..");
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    let normalized = normalized.to_string_lossy().replace('\\', "/");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

fn safe_path_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_' | '.') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches('-')
        .to_string();
    if slug.is_empty() {
        "worktree".to_string()
    } else {
        slug
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalized_path_comparison_keeps_leading_parent_components() {
        assert_ne!(
            normalized_path_text(Path::new("../repo")),
            normalized_path_text(Path::new("repo"))
        );
    }

    #[test]
    fn mutation_claim_rejects_a_duplicate_workspace_side_effect() {
        let claims = Arc::new(Mutex::new(BTreeSet::new()));
        let workspace_id = WorkspaceId::new();
        let first = GitMutationClaim::claim(claims.clone(), &workspace_id).unwrap();
        let error = GitMutationClaim::claim(claims.clone(), &workspace_id).unwrap_err();
        assert_eq!(error.code, "git_mutation_in_progress");
        drop(first);
        assert!(GitMutationClaim::claim(claims, &workspace_id).is_ok());
    }
}

use std::collections::BTreeSet;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use vibex_core::{
    FileMutationRequest, FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult,
    FileTreeEntry, FileTreeRequest, FileWriteRequest, GitBlameRequest, GitBlameResponse,
    GitBranchCheckoutRequest, GitBranchCreateRequest, GitBranchListResponse, GitCommitDetail,
    GitCommitDetailRequest, GitCommitRequest, GitCommitResult, GitDiffRequest, GitDiffResponse,
    GitHistoryRequest, GitHistoryResponse, GitRemoteActionRequest, GitRemoteActionResult,
    GitStageRequest, GitStatusSummary, VibexError, VibexResult, WorkspaceId, WorkspaceRecord,
};
use vibex_db::{GitSnapshotRepository, WorkspaceRepository, open_database};
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
}

#[derive(Debug, Clone, Copy)]
enum GitStageOperation {
    Stage,
    Unstage,
    Revert,
}

pub(crate) fn workspace_record(
    connection: &vibex_db::DbConnection,
    workspace_id: &WorkspaceId,
) -> VibexResult<(vibex_core::ProjectRecord, WorkspaceRecord)> {
    WorkspaceRepository::get(connection, workspace_id)?
        .ok_or_else(|| VibexError::validation("workspace_not_found", "workspace was not found"))
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

#[cfg(test)]
mod tests {
    use super::*;

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

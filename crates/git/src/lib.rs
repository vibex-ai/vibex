use std::collections::HashMap;
use std::fs::{File, OpenOptions};
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use fs2::FileExt as _;
use serde::Serialize;
use sha2::{Digest, Sha256};
use vibex_core::{
    GitBlameLine, GitBlameRequest, GitBlameResponse, GitBranchCheckoutRequest,
    GitBranchCreateRequest, GitBranchListResponse, GitBranchSummary, GitChange, GitChangeKind,
    GitCommitDetail, GitCommitDetailRequest, GitCommitFileChange, GitCommitRequest,
    GitCommitResult, GitCommitSummary, GitDiffRequest, GitDiffResponse, GitHistoryAuthor,
    GitHistoryRequest, GitHistoryResponse, GitRemoteActionKind, GitRemoteActionRequest,
    GitRemoteActionResult, GitRemoteSummary, GitStageRequest, GitStatusSummary,
    GitWorktreeChangeSummary, GitWorktreeConflictFile, GitWorktreeConflictKind,
    GitWorktreeConflictVersion, GitWorktreeCreateRequest, GitWorktreeDiscardRequest,
    GitWorktreeListResponse, GitWorktreeMergeRequest, GitWorktreeMergeStrategy, GitWorktreeSummary,
    VibexError, VibexResult, WorkspaceId, unix_timestamp_ms,
};

mod identity;
pub use identity::*;

const MAX_DIFF_BYTES: usize = 512 * 1024;
const MAX_HISTORY_LIMIT: u32 = 100;
const DEFAULT_HISTORY_LIMIT: u32 = 50;
const MAX_BLAME_LINES: u32 = 500;
const VIBEX_GIT_MUTATION_LOCK_FILE: &str = "vibex-mutation.lock";

fn git_command() -> Command {
    let command = Command::new("git");
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        command.creation_flags(0x0800_0000);
    }
    command
}

struct GitMutationGuard {
    file: File,
}

impl GitMutationGuard {
    fn claim(root: &Path) -> VibexResult<Self> {
        let identity = repository_identity(root)?;
        let common_dir = PathBuf::from(
            identity
                .git_common_dir
                .canonical_path
                .as_deref()
                .unwrap_or(&identity.git_common_dir.normalized_path),
        );
        let lock_path = common_dir.join(VIBEX_GIT_MUTATION_LOCK_FILE);
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&lock_path)
            .map_err(|error| {
                VibexError::storage(
                    "git_mutation_lock_open_failed",
                    "failed to open the Git mutation lock",
                )
                .with_diagnostic("path", lock_path.display().to_string())
                .with_diagnostic("error", error.to_string())
            })?;
        file.try_lock_exclusive().map_err(|error| {
            let mut conflict = VibexError::conflict(
                "git_mutation_in_progress",
                "another Git mutation is already in progress for this repository",
            )
            .with_diagnostic("path", root.display().to_string());
            if error.kind() != std::io::ErrorKind::WouldBlock {
                conflict = conflict.with_diagnostic("error", error.to_string());
            }
            conflict
        })?;
        Ok(Self { file })
    }
}

impl Drop for GitMutationGuard {
    fn drop(&mut self) {
        let _ = self.file.unlock();
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSmokeResult {
    pub status: GitStatusSummary,
    pub diff_available: bool,
}

pub fn git_status(repo_path: impl AsRef<Path>) -> VibexResult<GitStatusSummary> {
    status(WorkspaceId::new(), repo_path)
}

pub fn status(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
) -> VibexResult<GitStatusSummary> {
    let repo_path = repo_path.as_ref();
    ensure_git_repo(repo_path)?;
    let root = repo_root(repo_path)?;

    let symbolic_branch = run_git_optional(&root, &["symbolic-ref", "--short", "-q", "HEAD"])?;
    let branch = symbolic_branch.map(|value| value.trim().to_string());
    let detached = branch.is_none();
    let short_commit = run_git_optional(&root, &["rev-parse", "--short", "HEAD"])?
        .map(|value| value.trim().to_string());
    let porcelain = run_git(&root, &["status", "--porcelain=v1"])?;
    let changes = enrich_status_changes(&root, parse_porcelain(&porcelain))?;
    let staged_count = changes.iter().filter(|change| change.staged).count() as u32;
    let unstaged_count = changes.iter().filter(|change| change.unstaged).count() as u32;
    let untracked_count = changes
        .iter()
        .filter(|change| change.kind == GitChangeKind::Untracked)
        .count() as u32;

    Ok(GitStatusSummary {
        workspace_id,
        repo_path: root.to_string_lossy().to_string(),
        branch,
        short_commit,
        detached,
        dirty: !changes.is_empty(),
        staged_count,
        unstaged_count,
        untracked_count,
        changes,
        captured_at_ms: unix_timestamp_ms(),
    })
}

pub fn diff(repo_path: impl AsRef<Path>, request: &GitDiffRequest) -> VibexResult<GitDiffResponse> {
    let root = repo_root(repo_path.as_ref())?;
    validate_git_path(&request.path)?;
    let raw = if request.staged {
        run_git(&root, &["diff", "--staged", "--", &request.path])?
    } else if is_untracked(&root, &request.path)? {
        run_git_no_index_for_untracked(&root, &request.path)?
    } else {
        run_git(&root, &["diff", "--", &request.path])?
    };
    let (diff, truncated) = truncate_diff(raw);
    Ok(GitDiffResponse {
        workspace_id: request.workspace_id.clone(),
        path: request.path.clone(),
        staged: request.staged,
        diff,
        truncated,
    })
}

pub fn history(
    repo_path: impl AsRef<Path>,
    request: &GitHistoryRequest,
) -> VibexResult<GitHistoryResponse> {
    let root = repo_root(repo_path.as_ref())?;
    if run_git_optional(&root, &["rev-parse", "--verify", "HEAD"])?.is_none() {
        return Ok(GitHistoryResponse {
            workspace_id: request.workspace_id.clone(),
            commits: Vec::new(),
            has_more: false,
            authors: Vec::new(),
        });
    }

    if let Some(before_commit) = request.before_commit.as_deref() {
        validate_commitish(before_commit)?;
    }
    let ref_name = normalized_optional(&request.ref_name);
    if let Some(ref_name) = ref_name {
        validate_existing_commitish(&root, ref_name)?;
    }
    let author = normalized_optional(&request.author);

    let limit = request
        .limit
        .unwrap_or(DEFAULT_HISTORY_LIMIT)
        .clamp(1, MAX_HISTORY_LIMIT);
    let mut args = vec![
        "log".to_string(),
        format!("-n{}", limit + 1),
        "--decorate=short".to_string(),
        "--date=unix".to_string(),
        "--pretty=format:%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1e".to_string(),
    ];
    if let Some(author) = author {
        args.push(format!("--author={author}"));
    }
    if let Some(before_commit) = request.before_commit.as_deref() {
        args.push("--skip=1".to_string());
        args.push(before_commit.to_string());
    } else if let Some(ref_name) = ref_name {
        args.push(ref_name.to_string());
    }
    let authors = history_authors(&root, ref_name)?;
    let output = run_git_owned(&root, &args)?;
    let mut commits = parse_history_output(&output)?;
    let has_more = commits.len() as u32 > limit;
    if has_more {
        commits.truncate(limit as usize);
    }
    Ok(GitHistoryResponse {
        workspace_id: request.workspace_id.clone(),
        commits,
        has_more,
        authors,
    })
}

pub fn commit_detail(
    repo_path: impl AsRef<Path>,
    request: &GitCommitDetailRequest,
) -> VibexResult<GitCommitDetail> {
    let root = repo_root(repo_path.as_ref())?;
    validate_commitish(&request.commit_hash)?;
    let summary_output = run_git_owned(
        &root,
        &[
            "show".to_string(),
            "-s".to_string(),
            "--decorate=short".to_string(),
            "--date=unix".to_string(),
            "--pretty=format:%H%x1f%h%x1f%P%x1f%an%x1f%ae%x1f%at%x1f%D%x1f%s%x1f%B".to_string(),
            request.commit_hash.clone(),
        ],
    )?;
    let (summary, body) = parse_commit_detail_header(&summary_output)?;
    let files = commit_file_changes(&root, &request.commit_hash)?;
    let (patch, patch_truncated) = if request.include_patch {
        let raw = run_git_owned(
            &root,
            &[
                "show".to_string(),
                "--format=".to_string(),
                "--patch".to_string(),
                "--no-ext-diff".to_string(),
                "--find-renames".to_string(),
                request.commit_hash.clone(),
            ],
        )?;
        let (patch, truncated) = truncate_diff(raw);
        (Some(patch), truncated)
    } else {
        (None, false)
    };

    Ok(GitCommitDetail {
        workspace_id: request.workspace_id.clone(),
        summary,
        body,
        files,
        patch,
        patch_truncated,
    })
}

pub fn blame(
    repo_path: impl AsRef<Path>,
    request: &GitBlameRequest,
) -> VibexResult<GitBlameResponse> {
    let root = repo_root(repo_path.as_ref())?;
    validate_git_path(&request.path)?;
    let (start, end, truncated_by_range) =
        bounded_line_range(request.start_line, request.end_line)?;
    let mut args = vec!["blame".to_string(), "--line-porcelain".to_string()];
    if let Some(start) = start {
        let end = end.unwrap_or(start.saturating_add(MAX_BLAME_LINES - 1));
        args.push(format!("-L{},{}", start, end));
    }
    args.push("--".to_string());
    args.push(request.path.clone());

    let output = run_git_owned(&root, &args)?;
    let lines = parse_blame_output(&output)?;
    let truncated = truncated_by_range || lines.len() as u32 >= MAX_BLAME_LINES;
    Ok(GitBlameResponse {
        workspace_id: request.workspace_id.clone(),
        path: request.path.clone(),
        lines,
        truncated,
    })
}

pub fn branch_list(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
) -> VibexResult<GitBranchListResponse> {
    let root = repo_root(repo_path.as_ref())?;
    let branches = list_branches(&root)?;
    let remotes = list_remotes(&root)?;
    Ok(GitBranchListResponse {
        workspace_id,
        branches,
        remotes,
    })
}

pub fn branch_create(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitBranchCreateRequest,
) -> VibexResult<GitStatusSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    validate_branch_name(&root, &request.name)?;
    if let Some(base_ref) = request.base_ref.as_deref() {
        validate_commitish(base_ref)?;
    }
    let mut args = if request.checkout {
        vec!["switch".to_string(), "-c".to_string(), request.name.clone()]
    } else {
        vec!["branch".to_string(), request.name.clone()]
    };
    if let Some(base_ref) = request.base_ref.as_deref() {
        args.push(base_ref.to_string());
    }
    run_git_owned(&root, &args)?;
    status(workspace_id, root)
}

pub fn branch_checkout(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitBranchCheckoutRequest,
) -> VibexResult<GitStatusSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    validate_branch_name(&root, &request.name)?;
    run_git_owned(&root, &["switch".to_string(), request.name.clone()])?;
    status(workspace_id, root)
}

pub fn remote_action(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitRemoteActionRequest,
) -> VibexResult<GitRemoteActionResult> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    remote_action_inner(workspace_id, &root, request)
}

fn remote_action_inner(
    workspace_id: WorkspaceId,
    root: &Path,
    request: &GitRemoteActionRequest,
) -> VibexResult<GitRemoteActionResult> {
    let output = match request.kind {
        GitRemoteActionKind::Fetch => {
            let mut args = vec!["fetch".to_string()];
            if let Some(remote) = request.remote.as_deref() {
                validate_remote_name(remote)?;
                args.push(remote.to_string());
            }
            if let Some(branch) = request.branch.as_deref() {
                validate_ref_arg(branch)?;
                args.push(branch.to_string());
            }
            run_git_owned(root, &args)?
        }
        GitRemoteActionKind::Push => push_output(root, request)?,
    };
    let status_after = status(workspace_id.clone(), root).ok();
    Ok(GitRemoteActionResult {
        workspace_id,
        kind: request.kind,
        summary: remote_action_summary(request.kind, &output),
        status_after,
    })
}

pub fn worktree_list(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
) -> VibexResult<GitWorktreeListResponse> {
    let root = repo_root(repo_path.as_ref())?;
    let repository_identity = repository_identity(&root)?;
    let output = run_git(&root, &["worktree", "list", "--porcelain"])?;
    let mut worktrees = parse_worktree_list(&output);
    for worktree in &mut worktrees {
        worktree.path_identity = Some(canonical_path_identity(&worktree.path));
        worktree.repository_identity = Some(repository_identity.clone());
    }
    Ok(GitWorktreeListResponse {
        workspace_id,
        worktrees,
    })
}

pub fn worktree_add(
    repo_path: impl AsRef<Path>,
    path: impl AsRef<Path>,
    request: &GitWorktreeCreateRequest,
) -> VibexResult<GitWorktreeSummary> {
    worktree_add_recoverable(repo_path, path, request, None, false)
}

pub fn validate_worktree_create(
    repo_path: impl AsRef<Path>,
    request: &GitWorktreeCreateRequest,
) -> VibexResult<()> {
    let root = repo_root(repo_path.as_ref())?;
    validate_branch_name(&root, &request.branch_name)?;
    if request.name.as_deref().is_some_and(|name| {
        name.trim().is_empty() || name.len() > 128 || name.chars().any(char::is_control)
    }) {
        return Err(VibexError::validation(
            "worktree_name_invalid",
            "worktree name must be non-empty and bounded",
        ));
    }
    if let Some(worktree_path) = request.worktree_path.as_deref() {
        validate_requested_worktree_path(worktree_path)?;
    }
    if let Some(base_ref) = request.base_ref.as_deref() {
        validate_existing_commitish(&root, base_ref)?;
    }
    if local_branch_head(&root, &request.branch_name)?.is_some() {
        return Err(VibexError::conflict(
            "worktree_branch_exists",
            "Git branch already exists",
        ));
    }
    Ok(())
}

fn validate_requested_worktree_path(worktree_path: &str) -> VibexResult<()> {
    if worktree_path.trim().is_empty()
        || worktree_path.len() > 4_096
        || worktree_path.chars().any(char::is_control)
    {
        return Err(VibexError::validation(
            "worktree_path_invalid",
            "custom worktree path must be non-empty and bounded",
        ));
    }
    if !Path::new(worktree_path).is_absolute() {
        return Err(VibexError::validation(
            "worktree_path_not_absolute",
            "custom worktree path must be absolute",
        ));
    }
    Ok(())
}

pub fn worktree_add_recoverable(
    repo_path: impl AsRef<Path>,
    path: impl AsRef<Path>,
    request: &GitWorktreeCreateRequest,
    expected_base_head: Option<&str>,
    allow_existing_branch: bool,
) -> VibexResult<GitWorktreeSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    validate_branch_name(&root, &request.branch_name)?;
    if let Some(base_ref) = request.base_ref.as_deref() {
        validate_commitish(base_ref)?;
    }
    let path = path.as_ref();
    let path_text = path.to_string_lossy().to_string();
    let existing = worktree_list(request.workspace_id.clone(), &root)?
        .worktrees
        .into_iter()
        .find(|worktree| same_path_identity(&worktree.path, path));
    if let Some(worktree) = existing {
        verify_created_worktree(&worktree, request, expected_base_head)?;
        return Ok(worktree);
    }
    if path.exists() {
        return Err(VibexError::conflict(
            "worktree_unregistered_path_exists",
            "managed worktree path exists without matching Git registration",
        ));
    }

    let existing_branch_head = local_branch_head(&root, &request.branch_name)?;
    let mut args = vec!["worktree".to_string(), "add".to_string()];
    match existing_branch_head {
        Some(branch_head) => {
            if !allow_existing_branch || expected_base_head != Some(branch_head.as_str()) {
                return Err(VibexError::conflict(
                    "worktree_branch_recovery_conflict",
                    "managed worktree branch already exists with unproven ownership",
                ));
            }
            args.push(path_text);
            args.push(request.branch_name.clone());
        }
        None => {
            args.push("-b".to_string());
            args.push(request.branch_name.clone());
            args.push(path_text);
            if let Some(base_ref) = request.base_ref.as_deref() {
                args.push(base_ref.to_string());
            }
        }
    }
    run_git_owned(&root, &args)?;
    let list = worktree_list(request.workspace_id.clone(), &root)?;
    let worktree = list
        .worktrees
        .into_iter()
        .find(|worktree| same_path_identity(&worktree.path, path))
        .ok_or_else(|| {
            VibexError::process(
                "worktree_create_missing_after_add",
                "created worktree was not found in Git worktree list",
            )
        })?;
    verify_created_worktree(&worktree, request, expected_base_head)?;
    Ok(worktree)
}

fn verify_created_worktree(
    worktree: &GitWorktreeSummary,
    request: &GitWorktreeCreateRequest,
    expected_base_head: Option<&str>,
) -> VibexResult<()> {
    if worktree.branch.as_deref() != Some(request.branch_name.as_str()) {
        return Err(VibexError::conflict(
            "worktree_branch_identity_mismatch",
            "Git worktree branch does not match the durable create intent",
        ));
    }
    if expected_base_head.is_some_and(|expected| worktree.head.as_deref() != Some(expected)) {
        return Err(VibexError::conflict(
            "worktree_head_identity_mismatch",
            "Git worktree head does not match the durable create baseline",
        ));
    }
    Ok(())
}

pub fn worktree_merge_preflight(
    target_repo_path: impl AsRef<Path>,
    request: &GitWorktreeMergeRequest,
) -> VibexResult<()> {
    let target_root = repo_root(target_repo_path.as_ref())?;
    if status(request.workspace_id.clone(), &target_root)?.dirty {
        return Err(VibexError::conflict(
            "worktree_merge_dirty_target",
            "target checkout has uncommitted changes",
        ));
    }
    Ok(())
}

pub fn worktree_merge(
    target_repo_path: impl AsRef<Path>,
    source_ref: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    let target_root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&target_root)?;
    validate_ref_arg(source_ref)?;
    validate_ref_arg(expected_target_branch)?;
    let source_head = resolve_ref_head(&target_root, source_ref)?;
    if source_head != expected_source_head {
        return Err(VibexError::conflict(
            "worktree_source_head_changed",
            "source head changed after worktree preflight",
        ));
    }
    if current_branch(&target_root)?.as_deref() != Some(expected_target_branch) {
        return Err(VibexError::conflict(
            "worktree_target_branch_changed",
            "target branch changed after worktree preflight",
        ));
    }
    if resolve_head(&target_root)? != expected_target_head {
        return Err(VibexError::conflict(
            "worktree_target_head_changed",
            "target head changed after worktree preflight",
        ));
    }
    if status(WorkspaceId::new(), &target_root)?.dirty {
        return Err(VibexError::conflict(
            "worktree_merge_dirty_target",
            "target checkout has uncommitted changes",
        ));
    }
    let output = run_git_owned(
        &target_root,
        &[
            "merge".to_string(),
            "--no-ff".to_string(),
            "--no-edit".to_string(),
            expected_source_head.to_string(),
        ],
    )?;
    Ok(output.trim().to_string())
}

pub fn worktree_rebase_source(
    source_repo_path: impl AsRef<Path>,
    target_repo_path: impl AsRef<Path>,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    let source_root = repo_root(source_repo_path.as_ref())?;
    let target_root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&source_root)?;
    verify_rebase_start(
        &source_root,
        &target_root,
        source_branch,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    )?;
    run_git_owned(
        &source_root,
        &[
            "-c".to_string(),
            "rebase.updateRefs=false".to_string(),
            "rebase".to_string(),
            "--no-autostash".to_string(),
            "--no-rebase-merges".to_string(),
            expected_target_head.to_string(),
        ],
    )?;
    verify_rebase_completed_source(
        &source_root,
        &target_root,
        source_branch,
        expected_target_branch,
        expected_target_head,
    )
}

pub fn worktree_rebase_continue(
    source_repo_path: impl AsRef<Path>,
    target_repo_path: impl AsRef<Path>,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    let source_root = repo_root(source_repo_path.as_ref())?;
    let target_root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&source_root)?;
    verify_rebase_scene(
        &source_root,
        &target_root,
        source_branch,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    )?;
    if !worktree_conflicts(&source_root)?.is_empty() {
        return Err(VibexError::conflict(
            "worktree_conflicts_unresolved",
            "all rebase conflicts must be staged before continuing",
        ));
    }
    run_git_owned(
        &source_root,
        &[
            "-c".to_string(),
            "core.editor=true".to_string(),
            "-c".to_string(),
            "rebase.updateRefs=false".to_string(),
            "rebase".to_string(),
            "--continue".to_string(),
        ],
    )?;
    verify_rebase_completed_source(
        &source_root,
        &target_root,
        source_branch,
        expected_target_branch,
        expected_target_head,
    )
}

pub fn worktree_rebase_finish(
    source_repo_path: impl AsRef<Path>,
    target_repo_path: impl AsRef<Path>,
    source_branch: &str,
    rebased_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    let source_root = repo_root(source_repo_path.as_ref())?;
    let target_root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&source_root)?;
    validate_ref_arg(source_branch)?;
    validate_commitish(rebased_source_head)?;
    validate_ref_arg(expected_target_branch)?;
    validate_commitish(expected_target_head)?;
    verify_same_repository(&source_root, &target_root)?;
    if current_branch(&source_root)?.as_deref() != Some(source_branch)
        || resolve_head(&source_root)? != rebased_source_head
        || resolve_ref_head(&source_root, source_branch)? != rebased_source_head
        || status(WorkspaceId::new(), &source_root)?.dirty
    {
        return Err(VibexError::conflict(
            "worktree_rebased_source_changed",
            "rebased source no longer matches the recorded head",
        ));
    }
    verify_target_for_rebase(&target_root, expected_target_branch, expected_target_head)?;
    run_git_owned(
        &target_root,
        &[
            "merge".to_string(),
            "--ff-only".to_string(),
            "--no-edit".to_string(),
            rebased_source_head.to_string(),
        ],
    )?;
    let head_after = resolve_head(&target_root)?;
    if head_after != rebased_source_head {
        return Err(VibexError::conflict(
            "worktree_rebase_fast_forward_unproven",
            "target did not reach the exact rebased source head",
        ));
    }
    Ok(head_after)
}

pub fn worktree_rebase_abort(
    source_repo_path: impl AsRef<Path>,
    target_repo_path: impl AsRef<Path>,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    let source_root = repo_root(source_repo_path.as_ref())?;
    let target_root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&source_root)?;
    verify_rebase_scene(
        &source_root,
        &target_root,
        source_branch,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    )?;
    run_git_owned(&source_root, &["rebase".to_string(), "--abort".to_string()])?;
    if current_branch(&source_root)?.as_deref() != Some(source_branch)
        || resolve_head(&source_root)? != expected_source_head
        || resolve_ref_head(&source_root, source_branch)? != expected_source_head
        || status(WorkspaceId::new(), &source_root)?.dirty
    {
        return Err(VibexError::conflict(
            "worktree_rebase_abort_unproven",
            "rebase abort did not restore the exact clean source state",
        ));
    }
    verify_target_for_rebase(&target_root, expected_target_branch, expected_target_head)?;
    Ok(())
}

pub fn worktree_rebase_scene_matches(
    source_repo_path: impl AsRef<Path>,
    target_repo_path: impl AsRef<Path>,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<bool> {
    let source_root = repo_root(source_repo_path.as_ref())?;
    let target_root = repo_root(target_repo_path.as_ref())?;
    match verify_rebase_scene(
        &source_root,
        &target_root,
        source_branch,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    ) {
        Ok(()) => Ok(true),
        Err(error)
            if matches!(
                error.code.as_str(),
                "worktree_rebase_resolution_scene_changed"
                    | "worktree_target_branch_changed"
                    | "worktree_target_head_changed"
                    | "worktree_merge_dirty_target"
                    | "worktree_repository_identity_mismatch"
            ) =>
        {
            Ok(false)
        }
        Err(error) => Err(error),
    }
}

pub fn worktree_dirty_fingerprint(repo_path: impl AsRef<Path>) -> VibexResult<String> {
    let root = repo_root(repo_path.as_ref())?;
    let head = resolve_head(&root)?;
    let porcelain = run_git_owned(
        &root,
        &[
            "status".to_string(),
            "--porcelain=v1".to_string(),
            "--untracked-files=all".to_string(),
            "-z".to_string(),
        ],
    )?;
    let mut digest = Sha256::new();
    digest.update(head.as_bytes());
    digest.update([0]);
    digest.update(porcelain.as_bytes());
    Ok(format!("worktree-dirty-v1:{:x}", digest.finalize()))
}

pub fn worktree_merge_summary(
    repo_path: impl AsRef<Path>,
    target_head: &str,
    source_head: &str,
) -> VibexResult<GitWorktreeChangeSummary> {
    let root = repo_root(repo_path.as_ref())?;
    validate_commitish(target_head)?;
    validate_commitish(source_head)?;
    let commit_count = run_git_owned(
        &root,
        &[
            "rev-list".to_string(),
            "--count".to_string(),
            format!("{target_head}..{source_head}"),
        ],
    )?
    .trim()
    .parse::<u32>()
    .map_err(|error| {
        VibexError::process(
            "worktree_merge_summary_parse_failed",
            "failed to parse merge commit count",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let numstat = run_git_owned(
        &root,
        &[
            "diff".to_string(),
            "--numstat".to_string(),
            format!("{target_head}...{source_head}"),
        ],
    )?;
    let mut file_count = 0_u32;
    let mut additions = 0_u32;
    let mut deletions = 0_u32;
    for line in numstat.lines() {
        let mut fields = line.splitn(3, '\t');
        let Some(added) = fields.next() else {
            continue;
        };
        let Some(deleted) = fields.next() else {
            continue;
        };
        if fields.next().is_none() {
            continue;
        }
        file_count = file_count.saturating_add(1);
        additions = additions.saturating_add(parse_numstat_count(added));
        deletions = deletions.saturating_add(parse_numstat_count(deleted));
    }
    Ok(GitWorktreeChangeSummary {
        commit_count,
        file_count,
        additions,
        deletions,
    })
}

pub fn unpushed_commit_count(
    repo_path: impl AsRef<Path>,
    branch: &str,
) -> VibexResult<Option<u32>> {
    let root = repo_root(repo_path.as_ref())?;
    validate_ref_arg(branch)?;
    let upstream = run_git_optional(
        &root,
        &[
            "rev-parse",
            "--abbrev-ref",
            "--symbolic-full-name",
            &format!("{branch}@{{upstream}}"),
        ],
    )?;
    let Some(upstream) = upstream
        .as_deref()
        .map(str::trim)
        .filter(|upstream| !upstream.is_empty())
    else {
        return Ok(None);
    };
    let count = run_git_owned(
        &root,
        &[
            "rev-list".to_string(),
            "--count".to_string(),
            format!("{upstream}..{branch}"),
        ],
    )?
    .trim()
    .parse::<u32>()
    .map_err(|error| {
        VibexError::process(
            "git_unpushed_count_parse_failed",
            "failed to parse unpushed commit count",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    Ok(Some(count))
}

pub fn active_git_operation(repo_path: impl AsRef<Path>) -> VibexResult<Option<String>> {
    let root = repo_root(repo_path.as_ref())?;
    if merge_head(&root)?.is_some() {
        return Ok(Some("merge".to_string()));
    }
    if run_git_optional(&root, &["rev-parse", "-q", "--verify", "CHERRY_PICK_HEAD"])?.is_some() {
        return Ok(Some("cherry_pick".to_string()));
    }
    let git_dir = run_git(&root, &["rev-parse", "--absolute-git-dir"])?;
    let git_dir = PathBuf::from(git_dir.trim());
    if git_dir.join("rebase-merge").exists() || git_dir.join("rebase-apply").exists() {
        return Ok(Some("rebase".to_string()));
    }
    Ok(None)
}

pub fn merge_head(repo_path: impl AsRef<Path>) -> VibexResult<Option<String>> {
    let root = repo_root(repo_path.as_ref())?;
    Ok(
        run_git_optional(&root, &["rev-parse", "-q", "--verify", "MERGE_HEAD"])?
            .map(|head| head.trim().to_string())
            .filter(|head| !head.is_empty()),
    )
}

pub fn worktree_conflicts(
    repo_path: impl AsRef<Path>,
) -> VibexResult<Vec<GitWorktreeConflictFile>> {
    let root = repo_root(repo_path.as_ref())?;
    let output = run_git_owned(
        &root,
        &["ls-files".to_string(), "-u".to_string(), "-z".to_string()],
    )?;
    let mut stages_by_path: HashMap<String, Vec<u8>> = HashMap::new();
    for entry in output.split('\0').filter(|entry| !entry.is_empty()) {
        let Some((metadata, path)) = entry.split_once('\t') else {
            continue;
        };
        let Some(stage) = metadata
            .split_whitespace()
            .nth(2)
            .and_then(|value| value.parse::<u8>().ok())
        else {
            continue;
        };
        stages_by_path
            .entry(path.to_string())
            .or_default()
            .push(stage);
    }
    let mut conflicts = stages_by_path
        .into_iter()
        .map(|(path, mut stages)| {
            stages.sort_unstable();
            stages.dedup();
            let binary = conflict_path_is_binary(&root, &path);
            let kind = if binary {
                GitWorktreeConflictKind::Binary
            } else {
                match stages.as_slice() {
                    [2, 3] => GitWorktreeConflictKind::BothAdded,
                    [1, 2] => GitWorktreeConflictKind::DeletedBySource,
                    [1, 3] => GitWorktreeConflictKind::DeletedByTarget,
                    [1, 2, 3] => GitWorktreeConflictKind::BothModified,
                    _ => GitWorktreeConflictKind::Other,
                }
            };
            GitWorktreeConflictFile {
                path,
                kind,
                binary,
                resolved: false,
            }
        })
        .collect::<Vec<_>>();
    conflicts.sort_by(|left, right| left.path.cmp(&right.path));
    Ok(conflicts)
}

pub fn worktree_select_conflict_version(
    repo_path: impl AsRef<Path>,
    path: &str,
    version: GitWorktreeConflictVersion,
) -> VibexResult<Vec<GitWorktreeConflictFile>> {
    worktree_select_conflict_version_for_strategy(
        repo_path,
        path,
        version,
        GitWorktreeMergeStrategy::NoFfMerge,
    )
}

pub fn worktree_select_conflict_version_for_strategy(
    repo_path: impl AsRef<Path>,
    path: &str,
    version: GitWorktreeConflictVersion,
    strategy: GitWorktreeMergeStrategy,
) -> VibexResult<Vec<GitWorktreeConflictFile>> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    validate_git_path(path)?;
    let operation_matches = match strategy {
        GitWorktreeMergeStrategy::NoFfMerge => merge_head(&root)?.is_some(),
        GitWorktreeMergeStrategy::RebaseAndMerge => {
            active_git_operation(&root)?.as_deref() == Some("rebase")
        }
        GitWorktreeMergeStrategy::Unknown => false,
    };
    if !operation_matches {
        return Err(VibexError::conflict(
            "worktree_merge_scene_missing",
            "workspace no longer has the expected integration operation",
        ));
    }
    let conflicts = worktree_conflicts(&root)?;
    let _conflict = conflicts
        .iter()
        .find(|conflict| conflict.path == path)
        .ok_or_else(|| {
            VibexError::conflict(
                "worktree_conflict_path_missing",
                "path is not an unresolved merge conflict",
            )
        })?;
    let stage = match version {
        GitWorktreeConflictVersion::Target => 2,
        GitWorktreeConflictVersion::Source => 3,
    };
    let has_stage = conflict_has_stage(&root, path, stage)?;
    if has_stage {
        run_git_owned(
            &root,
            &[
                "checkout-index".to_string(),
                "--force".to_string(),
                format!("--stage={stage}"),
                "--".to_string(),
                path.to_string(),
            ],
        )?;
    } else {
        let full_path = root.join(path);
        if full_path.is_file() || full_path.is_symlink() {
            std::fs::remove_file(&full_path).map_err(|error| {
                VibexError::storage(
                    "worktree_conflict_version_remove_failed",
                    "failed to apply the selected deleted file version",
                )
                .with_diagnostic("path", path)
                .with_diagnostic("error", error.to_string())
            })?;
        }
    }
    worktree_conflicts(&root)
}

pub fn worktree_stage_conflicts(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    paths: &[String],
) -> VibexResult<Vec<GitWorktreeConflictFile>> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    if paths.is_empty() {
        return Err(VibexError::validation(
            "worktree_conflict_paths_empty",
            "at least one conflict path is required",
        ));
    }
    let current = worktree_conflicts(&root)?;
    for path in paths {
        validate_git_path(path)?;
        if !current.iter().any(|conflict| conflict.path == *path) {
            return Err(VibexError::conflict(
                "worktree_conflict_path_missing",
                "path is not an unresolved merge conflict",
            )
            .with_diagnostic("path", path));
        }
    }
    let _ = workspace_id;
    let paths = validate_paths(paths)?;
    run_git_paths(&root, &["add"], &paths)?;
    worktree_conflicts(&root)
}

pub fn worktree_merge_continue(
    target_repo_path: impl AsRef<Path>,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    let root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    verify_merge_scene(
        &root,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    )?;
    if !worktree_conflicts(&root)?.is_empty() {
        return Err(VibexError::conflict(
            "worktree_conflicts_unresolved",
            "all merge conflicts must be staged before continuing",
        ));
    }
    run_git_owned(
        &root,
        &[
            "-c".to_string(),
            "core.editor=true".to_string(),
            "commit".to_string(),
            "--no-edit".to_string(),
        ],
    )?;
    let head_after = resolve_head(&root)?;
    let parents = run_git_owned(
        &root,
        &[
            "rev-list".to_string(),
            "--parents".to_string(),
            "-n".to_string(),
            "1".to_string(),
            head_after.clone(),
        ],
    )?;
    let fields = parents.split_whitespace().collect::<Vec<_>>();
    if fields.len() != 3 || fields[1] != expected_target_head || fields[2] != expected_source_head {
        return Err(VibexError::conflict(
            "worktree_merge_commit_identity_mismatch",
            "completed merge commit does not have the expected parents",
        ));
    }
    Ok(head_after)
}

pub fn is_expected_merge_commit(
    repo_path: impl AsRef<Path>,
    commit: &str,
    expected_target_parent: &str,
    expected_source_parent: &str,
) -> VibexResult<bool> {
    let root = repo_root(repo_path.as_ref())?;
    validate_commitish(commit)?;
    validate_commitish(expected_target_parent)?;
    validate_commitish(expected_source_parent)?;
    let parents = run_git_owned(
        &root,
        &[
            "rev-list".to_string(),
            "--parents".to_string(),
            "-n".to_string(),
            "1".to_string(),
            commit.to_string(),
        ],
    )?;
    let fields = parents.split_whitespace().collect::<Vec<_>>();
    Ok(fields.len() == 3
        && fields[0] == commit
        && fields[1] == expected_target_parent
        && fields[2] == expected_source_parent)
}

pub fn worktree_merge_abort(
    target_repo_path: impl AsRef<Path>,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    let root = repo_root(target_repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    verify_merge_scene(
        &root,
        expected_source_head,
        expected_target_branch,
        expected_target_head,
    )?;
    run_git_owned(&root, &["merge".to_string(), "--abort".to_string()])?;
    if merge_head(&root)?.is_some()
        || resolve_head(&root)? != expected_target_head
        || status(WorkspaceId::new(), &root)?.dirty
    {
        return Err(VibexError::conflict(
            "worktree_merge_abort_unproven",
            "merge abort did not restore the exact clean target state",
        ));
    }
    Ok(())
}

pub fn worktree_restore(
    repo_path: impl AsRef<Path>,
    worktree_path: impl AsRef<Path>,
    branch: &str,
    expected_head: &str,
) -> VibexResult<GitWorktreeSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let path = worktree_path.as_ref();
    if !path.is_absolute() {
        return Err(VibexError::validation(
            "worktree_path_not_absolute",
            "restored worktree path must be absolute",
        ));
    }
    if path.exists() {
        return Err(VibexError::conflict(
            "worktree_restore_path_exists",
            "original worktree path is already occupied",
        ));
    }
    validate_ref_arg(branch)?;
    if resolve_ref_head(&root, branch)? != expected_head {
        return Err(VibexError::conflict(
            "worktree_source_head_changed",
            "worktree branch changed after archive",
        ));
    }
    run_git_owned(
        &root,
        &[
            "worktree".to_string(),
            "add".to_string(),
            path.to_string_lossy().to_string(),
            branch.to_string(),
        ],
    )?;
    worktree_list(WorkspaceId::new(), &root)?
        .worktrees
        .into_iter()
        .find(|worktree| same_path_identity(&worktree.path, path))
        .ok_or_else(|| {
            VibexError::process(
                "worktree_restore_missing_after_add",
                "restored worktree is missing from Git registration",
            )
        })
}

pub fn worktree_remove(
    repo_path: impl AsRef<Path>,
    request: &GitWorktreeDiscardRequest,
) -> VibexResult<String> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let path = Path::new(&request.worktree_path);
    if path.as_os_str().is_empty() {
        return Err(VibexError::validation(
            "worktree_path_empty",
            "worktree path must not be empty",
        ));
    }
    if let Some(expected_head) = request.expected_head.as_deref() {
        let registered = worktree_list(request.workspace_id.clone(), &root)?
            .worktrees
            .into_iter()
            .find(|worktree| same_path_identity(&worktree.path, path))
            .ok_or_else(|| {
                VibexError::conflict(
                    "worktree_registration_missing",
                    "managed worktree is no longer registered with Git",
                )
            })?;
        if registered.head.as_deref() != Some(expected_head) {
            return Err(VibexError::conflict(
                "worktree_source_head_changed",
                "source head changed after worktree preflight",
            ));
        }
    }
    let mut args = vec!["worktree".to_string(), "remove".to_string()];
    if request.force {
        args.push("--force".to_string());
    }
    args.push(request.worktree_path.clone());
    let output = run_git_owned(&root, &args)?;
    Ok(output.trim().to_string())
}

pub fn stage(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitStageRequest,
) -> VibexResult<GitStatusSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let paths = validate_paths(&request.paths)?;
    run_git_paths(&root, &["add"], &paths)?;
    status(workspace_id, root)
}

pub fn unstage(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitStageRequest,
) -> VibexResult<GitStatusSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let paths = validate_paths(&request.paths)?;
    if run_git_paths(&root, &["restore", "--staged"], &paths).is_err() {
        run_git_paths(&root, &["reset"], &paths)?;
    }
    status(workspace_id, root)
}

pub fn revert(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitStageRequest,
) -> VibexResult<GitStatusSummary> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let paths = validate_paths(&request.paths)?;
    let status_before = status(workspace_id.clone(), &root)?;
    let mut tracked = Vec::new();
    for path in &paths {
        let untracked = status_before
            .changes
            .iter()
            .any(|change| change.path == *path && change.kind == GitChangeKind::Untracked);
        if untracked {
            let absolute = root.join(path);
            if absolute.is_dir() {
                std::fs::remove_dir_all(&absolute).map_err(|err| {
                    VibexError::storage(
                        "git_revert_untracked_failed",
                        "failed to remove untracked directory",
                    )
                    .with_diagnostic("path", path)
                    .with_diagnostic("error", err.to_string())
                })?;
            } else if absolute.exists() {
                std::fs::remove_file(&absolute).map_err(|err| {
                    VibexError::storage(
                        "git_revert_untracked_failed",
                        "failed to remove untracked file",
                    )
                    .with_diagnostic("path", path)
                    .with_diagnostic("error", err.to_string())
                })?;
            }
        } else {
            tracked.push(path.clone());
        }
    }
    if !tracked.is_empty() {
        run_git_paths(&root, &["restore", "--staged", "--worktree"], &tracked)?;
    }
    status(workspace_id, root)
}

pub fn commit(
    workspace_id: WorkspaceId,
    repo_path: impl AsRef<Path>,
    request: &GitCommitRequest,
) -> VibexResult<GitCommitResult> {
    let root = repo_root(repo_path.as_ref())?;
    let _mutation = GitMutationGuard::claim(&root)?;
    let message = request.message.trim();
    if message.is_empty() {
        return Err(VibexError::validation(
            "empty_commit_message",
            "commit message must not be empty",
        ));
    }
    let paths = if request.paths.is_empty() {
        Vec::new()
    } else {
        validate_paths(&request.paths)?
    };
    if paths.is_empty() && !has_staged_changes(&root)? {
        return Err(VibexError::conflict(
            "no_staged_changes",
            "there are no staged changes to commit",
        ));
    }
    if !paths.is_empty() {
        let status_before = status(workspace_id.clone(), &root)?;
        let selected_untracked = paths
            .iter()
            .filter(|path| {
                status_before
                    .changes
                    .iter()
                    .any(|change| change.path == **path && change.kind == GitChangeKind::Untracked)
            })
            .cloned()
            .collect::<Vec<_>>();
        if !selected_untracked.is_empty() {
            run_git_paths(&root, &["add"], &selected_untracked)?;
        }
    }
    let output = commit_output(&root, message, &paths, request.amend)?;
    let short_commit = run_git(&root, &["rev-parse", "--short", "HEAD"])?
        .trim()
        .to_string();
    let push_result = if request.push_after {
        let push_request = GitRemoteActionRequest {
            workspace_id: workspace_id.clone(),
            kind: GitRemoteActionKind::Push,
            remote: None,
            branch: None,
        };
        Some(remote_action_inner(
            workspace_id.clone(),
            &root,
            &push_request,
        )?)
    } else {
        None
    };
    Ok(GitCommitResult {
        workspace_id,
        short_commit,
        summary: output.trim().to_string(),
        committed_at_ms: unix_timestamp_ms(),
        push_result,
    })
}

pub fn run_git_smoke(repo_path: impl AsRef<Path>) -> VibexResult<GitSmokeResult> {
    let status = git_status(repo_path)?;
    Ok(GitSmokeResult {
        diff_available: true,
        status,
    })
}

fn ensure_git_repo(repo_path: &Path) -> VibexResult<()> {
    if !repo_path.exists() {
        return Err(
            VibexError::validation("repo_path_missing", "Git path does not exist")
                .with_diagnostic("path", repo_path.display().to_string()),
        );
    }

    let output = git_command()
        .arg("-C")
        .arg(repo_path)
        .args(["rev-parse", "--is-inside-work-tree"])
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;

    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "true" {
        return Err(VibexError::validation(
            "not_git_repository",
            "path is not inside a Git work tree",
        )
        .with_diagnostic("path", repo_path.display().to_string()));
    }

    Ok(())
}

fn repo_root(repo_path: &Path) -> VibexResult<PathBuf> {
    ensure_git_repo(repo_path)?;
    Ok(PathBuf::from(
        run_git(repo_path, &["rev-parse", "--show-toplevel"])?.trim(),
    ))
}

fn parse_porcelain(porcelain: &str) -> Vec<GitChange> {
    porcelain
        .lines()
        .filter(|line| line.len() >= 3)
        .map(|line| {
            let bytes = line.as_bytes();
            let x = bytes[0] as char;
            let y = bytes[1] as char;
            let raw_path = line[3..].to_string();
            let (path, original_path) = parse_porcelain_path(raw_path);
            let kind = change_kind(x, y);
            GitChange {
                path,
                original_path,
                kind,
                staged: x != ' ' && x != '?',
                unstaged: y != ' ' || x == '?',
                additions: 0,
                deletions: 0,
            }
        })
        .collect()
}

fn enrich_status_changes(root: &Path, mut changes: Vec<GitChange>) -> VibexResult<Vec<GitChange>> {
    let stats = status_numstat(root)?;
    for change in &mut changes {
        if change.kind == GitChangeKind::Untracked {
            change.additions = count_untracked_file_lines(root, &change.path);
            change.deletions = 0;
            continue;
        }
        if let Some((additions, deletions)) = stats.get(&change.path) {
            change.additions = *additions;
            change.deletions = *deletions;
        }
    }
    Ok(changes)
}

fn status_numstat(root: &Path) -> VibexResult<HashMap<String, (u32, u32)>> {
    let mut stats = HashMap::new();
    merge_status_numstat(
        &mut stats,
        &run_git_owned(
            root,
            &[
                "diff".to_string(),
                "--numstat".to_string(),
                "-M".to_string(),
            ],
        )?,
    );
    merge_status_numstat(
        &mut stats,
        &run_git_owned(
            root,
            &[
                "diff".to_string(),
                "--cached".to_string(),
                "--numstat".to_string(),
                "-M".to_string(),
            ],
        )?,
    );
    Ok(stats)
}

fn merge_status_numstat(stats: &mut HashMap<String, (u32, u32)>, output: &str) {
    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 3 {
            continue;
        }
        let path = if parts.len() >= 4 { parts[3] } else { parts[2] };
        let entry = stats.entry(path.to_string()).or_insert((0, 0));
        entry.0 += parse_numstat_count(parts[0]);
        entry.1 += parse_numstat_count(parts[1]);
    }
}

fn count_untracked_file_lines(root: &Path, path: &str) -> u32 {
    let full_path = root.join(path);
    let Ok(metadata) = std::fs::metadata(&full_path) else {
        return 0;
    };
    if !metadata.is_file() || metadata.len() > MAX_DIFF_BYTES as u64 {
        return 0;
    }
    let Ok(content) = std::fs::read_to_string(full_path) else {
        return 0;
    };
    if content.is_empty() {
        return 0;
    }
    content.lines().count() as u32
}

fn parse_porcelain_path(raw_path: String) -> (String, Option<String>) {
    if let Some((from, to)) = raw_path.split_once(" -> ") {
        (to.to_string(), Some(from.to_string()))
    } else {
        (raw_path, None)
    }
}

fn change_kind(x: char, y: char) -> GitChangeKind {
    if x == '?' {
        return GitChangeKind::Untracked;
    }
    if x == 'U' || y == 'U' || (x == 'A' && y == 'A') || (x == 'D' && y == 'D') {
        return GitChangeKind::Unmerged;
    }
    let code = if y != ' ' { y } else { x };
    match code {
        'A' => GitChangeKind::Added,
        'M' => GitChangeKind::Modified,
        'D' => GitChangeKind::Deleted,
        'R' => GitChangeKind::Renamed,
        'C' => GitChangeKind::Copied,
        'T' => GitChangeKind::TypeChanged,
        _ => GitChangeKind::Unknown,
    }
}

fn conflict_has_stage(root: &Path, path: &str, expected_stage: u8) -> VibexResult<bool> {
    let output = run_git_owned(
        root,
        &[
            "ls-files".to_string(),
            "-u".to_string(),
            "--".to_string(),
            path.to_string(),
        ],
    )?;
    Ok(output.lines().any(|line| {
        line.split_once('\t')
            .and_then(|(metadata, _)| metadata.split_whitespace().nth(2))
            .and_then(|stage| stage.parse::<u8>().ok())
            == Some(expected_stage)
    }))
}

fn conflict_path_is_binary(root: &Path, path: &str) -> bool {
    let Ok(bytes) = std::fs::read(root.join(path)) else {
        return false;
    };
    bytes.iter().take(8 * 1024).any(|byte| *byte == 0)
}

fn verify_merge_scene(
    root: &Path,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    validate_commitish(expected_source_head)?;
    validate_ref_arg(expected_target_branch)?;
    validate_commitish(expected_target_head)?;
    if merge_head(root)?.as_deref() != Some(expected_source_head) {
        return Err(VibexError::conflict(
            "worktree_merge_head_changed",
            "active merge does not match the fixed source head",
        ));
    }
    if current_branch(root)?.as_deref() != Some(expected_target_branch) {
        return Err(VibexError::conflict(
            "worktree_target_branch_changed",
            "target branch changed during merge resolution",
        ));
    }
    if resolve_head(root)? != expected_target_head {
        return Err(VibexError::conflict(
            "worktree_target_head_changed",
            "target head changed during merge resolution",
        ));
    }
    Ok(())
}

fn verify_rebase_start(
    source_root: &Path,
    target_root: &Path,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    validate_ref_arg(source_branch)?;
    validate_commitish(expected_source_head)?;
    validate_ref_arg(expected_target_branch)?;
    validate_commitish(expected_target_head)?;
    verify_same_repository(source_root, target_root)?;
    if current_branch(source_root)?.as_deref() != Some(source_branch)
        || resolve_head(source_root)? != expected_source_head
        || resolve_ref_head(source_root, source_branch)? != expected_source_head
    {
        return Err(VibexError::conflict(
            "worktree_source_head_changed",
            "source branch changed before rebase",
        ));
    }
    if status(WorkspaceId::new(), source_root)?.dirty {
        return Err(VibexError::conflict(
            "worktree_rebase_dirty_source",
            "source worktree has uncommitted changes",
        ));
    }
    verify_target_for_rebase(target_root, expected_target_branch, expected_target_head)
}

fn verify_rebase_completed_source(
    source_root: &Path,
    target_root: &Path,
    source_branch: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<String> {
    if active_git_operation(source_root)?.is_some() {
        return Err(VibexError::conflict(
            "worktree_rebase_incomplete",
            "source worktree still has an active Git operation",
        ));
    }
    let rebased_head = resolve_head(source_root)?;
    if current_branch(source_root)?.as_deref() != Some(source_branch)
        || resolve_ref_head(source_root, source_branch)? != rebased_head
        || status(WorkspaceId::new(), source_root)?.dirty
    {
        return Err(VibexError::conflict(
            "worktree_rebased_source_changed",
            "rebased source does not have the expected clean branch identity",
        ));
    }
    verify_target_for_rebase(target_root, expected_target_branch, expected_target_head)?;
    if !is_ancestor(target_root, expected_target_head, &rebased_head)? {
        return Err(VibexError::conflict(
            "worktree_rebase_result_invalid",
            "rebased source is not based on the fixed target head",
        ));
    }
    Ok(rebased_head)
}

fn verify_rebase_scene(
    source_root: &Path,
    target_root: &Path,
    source_branch: &str,
    expected_source_head: &str,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    validate_ref_arg(source_branch)?;
    validate_commitish(expected_source_head)?;
    validate_ref_arg(expected_target_branch)?;
    validate_commitish(expected_target_head)?;
    verify_same_repository(source_root, target_root)?;
    if active_git_operation(source_root)?.as_deref() != Some("rebase")
        || resolve_ref_head(source_root, source_branch)? != expected_source_head
        || rebase_state_value(source_root, "orig-head")?.as_deref() != Some(expected_source_head)
        || rebase_state_value(source_root, "onto")?.as_deref() != Some(expected_target_head)
        || rebase_state_value(source_root, "head-name")?.as_deref()
            != Some(format!("refs/heads/{source_branch}").as_str())
    {
        return Err(VibexError::conflict(
            "worktree_rebase_resolution_scene_changed",
            "source Git scene no longer matches the durable rebase operation",
        ));
    }
    verify_target_for_rebase(target_root, expected_target_branch, expected_target_head)
}

fn verify_target_for_rebase(
    target_root: &Path,
    expected_target_branch: &str,
    expected_target_head: &str,
) -> VibexResult<()> {
    if current_branch(target_root)?.as_deref() != Some(expected_target_branch) {
        return Err(VibexError::conflict(
            "worktree_target_branch_changed",
            "target branch changed during rebase integration",
        ));
    }
    if resolve_head(target_root)? != expected_target_head {
        return Err(VibexError::conflict(
            "worktree_target_head_changed",
            "target head changed during rebase integration",
        ));
    }
    if status(WorkspaceId::new(), target_root)?.dirty {
        return Err(VibexError::conflict(
            "worktree_merge_dirty_target",
            "target checkout has uncommitted changes",
        ));
    }
    Ok(())
}

fn verify_same_repository(left: &Path, right: &Path) -> VibexResult<()> {
    if repository_identity(left)?.comparison_key != repository_identity(right)?.comparison_key {
        return Err(VibexError::conflict(
            "worktree_repository_identity_mismatch",
            "source and target belong to different Git repositories",
        ));
    }
    Ok(())
}

fn rebase_state_value(root: &Path, name: &str) -> VibexResult<Option<String>> {
    let git_dir = PathBuf::from(run_git(root, &["rev-parse", "--absolute-git-dir"])?.trim());
    for directory in ["rebase-merge", "rebase-apply"] {
        let path = git_dir.join(directory).join(name);
        match std::fs::read_to_string(&path) {
            Ok(value) => return Ok(Some(value.trim().to_string())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(error) => {
                return Err(VibexError::storage(
                    "worktree_rebase_state_read_failed",
                    "failed to inspect the active rebase state",
                )
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", error.to_string()));
            }
        }
    }
    Ok(None)
}

fn is_ancestor(root: &Path, ancestor: &str, descendant: &str) -> VibexResult<bool> {
    validate_commitish(ancestor)?;
    validate_commitish(descendant)?;
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["merge-base", "--is-ancestor", ancestor, descendant])
        .output()
        .map_err(|error| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", error.to_string())
        })?;
    match output.status.code() {
        Some(0) => Ok(true),
        Some(1) => Ok(false),
        _ => Err(git_command_error(
            "git_command_failed",
            "git merge-base failed",
            &output,
        )),
    }
}

fn is_untracked(root: &Path, path: &str) -> VibexResult<bool> {
    let porcelain = run_git(root, &["status", "--porcelain=v1", "--", path])?;
    Ok(porcelain.lines().any(|line| line.starts_with("?? ")))
}

fn has_staged_changes(root: &Path) -> VibexResult<bool> {
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["diff", "--staged", "--quiet"])
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    match output.status.code() {
        Some(0) => Ok(false),
        Some(1) => Ok(true),
        _ => Err(git_command_error(
            "git_command_failed",
            "git staged diff check failed",
            &output,
        )),
    }
}

fn commit_output(root: &Path, message: &str, paths: &[String], amend: bool) -> VibexResult<String> {
    let mut args = vec!["commit".to_string()];
    if amend {
        args.push("--amend".to_string());
    }
    if !paths.is_empty() {
        args.push("--only".to_string());
    }
    args.push("-m".to_string());
    args.push(message.to_string());
    if !paths.is_empty() {
        args.push("--".to_string());
        args.extend(paths.iter().cloned());
    }
    run_git_owned(root, &args)
}

fn push_output(root: &Path, request: &GitRemoteActionRequest) -> VibexResult<String> {
    let remote = normalized_optional(&request.remote);
    let branch = normalized_optional(&request.branch);
    match (remote, branch) {
        (Some(remote), Some(branch)) => {
            validate_remote_name(remote)?;
            validate_ref_arg(branch)?;
            return run_git_owned(
                root,
                &[
                    "push".to_string(),
                    "-u".to_string(),
                    remote.to_string(),
                    branch.to_string(),
                ],
            );
        }
        (Some(_), None) | (None, Some(_)) => {
            return Err(VibexError::validation(
                "git_push_target_incomplete",
                "push target requires both remote and branch",
            ));
        }
        (None, None) => {}
    }

    if current_upstream(root)?.is_some() {
        return run_git_owned(root, &["push".to_string()]);
    }

    let branch = current_branch(root)?.ok_or_else(|| {
        VibexError::conflict(
            "git_push_detached_head",
            "cannot push automatically from a detached HEAD",
        )
    })?;
    if !remote_exists(root, "origin")? {
        return Err(VibexError::conflict(
            "git_push_no_origin",
            "cannot push automatically because remote 'origin' is not configured",
        ));
    }
    run_git_owned(
        root,
        &[
            "push".to_string(),
            "-u".to_string(),
            "origin".to_string(),
            branch,
        ],
    )
}

fn current_branch(root: &Path) -> VibexResult<Option<String>> {
    Ok(
        run_git_optional(root, &["symbolic-ref", "--short", "-q", "HEAD"])?
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty()),
    )
}

fn current_upstream(root: &Path) -> VibexResult<Option<String>> {
    Ok(run_git_optional(
        root,
        &["rev-parse", "--abbrev-ref", "--symbolic-full-name", "@{u}"],
    )?
    .map(|value| value.trim().to_string())
    .filter(|value| !value.is_empty()))
}

fn remote_exists(root: &Path, name: &str) -> VibexResult<bool> {
    Ok(list_remotes(root)?
        .iter()
        .any(|remote| remote.name.as_str() == name))
}

fn remote_action_summary(kind: GitRemoteActionKind, output: &str) -> String {
    let output = output.trim();
    if !output.is_empty() {
        return output.to_string();
    }
    match kind {
        GitRemoteActionKind::Fetch => "fetch completed".to_string(),
        GitRemoteActionKind::Push => "push completed".to_string(),
    }
}

fn history_authors(root: &Path, ref_name: Option<&str>) -> VibexResult<Vec<GitHistoryAuthor>> {
    let mut args = vec![
        "log".to_string(),
        "--pretty=format:%an%x1f%ae%x1e".to_string(),
    ];
    if let Some(ref_name) = ref_name {
        args.push(ref_name.to_string());
    }
    let output = run_git_owned(root, &args)?;
    let mut authors = Vec::new();
    for record in output.trim_end_matches('\x1e').split('\x1e') {
        if record.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.trim_start_matches('\n').split('\x1f').collect();
        if fields.len() != 2 {
            continue;
        }
        let author = GitHistoryAuthor {
            name: fields[0].to_string(),
            email: fields[1].to_string(),
        };
        if !authors.iter().any(|existing: &GitHistoryAuthor| {
            existing.name == author.name && existing.email == author.email
        }) {
            authors.push(author);
        }
    }
    Ok(authors)
}

fn parse_history_output(output: &str) -> VibexResult<Vec<GitCommitSummary>> {
    output
        .trim_end_matches('\x1e')
        .split('\x1e')
        .filter(|record| !record.trim().is_empty())
        .map(parse_history_record)
        .collect()
}

fn parse_history_record(record: &str) -> VibexResult<GitCommitSummary> {
    let fields: Vec<&str> = record.trim_start_matches('\n').split('\x1f').collect();
    if fields.len() != 8 {
        return Err(VibexError::process(
            "git_history_parse_failed",
            "failed to parse git history output",
        )
        .with_diagnostic("fieldCount", fields.len().to_string()));
    }
    Ok(GitCommitSummary {
        hash: fields[0].to_string(),
        short_hash: fields[1].to_string(),
        parents: split_space_list(fields[2]),
        author_name: fields[3].to_string(),
        author_email: fields[4].to_string(),
        authored_at_ms: unix_seconds_to_ms(fields[5]),
        refs: split_refs(fields[6]),
        subject: fields[7].trim_end().to_string(),
    })
}

fn parse_commit_detail_header(output: &str) -> VibexResult<(GitCommitSummary, Option<String>)> {
    let fields: Vec<&str> = output.splitn(9, '\x1f').collect();
    if fields.len() != 9 {
        return Err(VibexError::process(
            "git_commit_detail_parse_failed",
            "failed to parse git commit detail output",
        )
        .with_diagnostic("fieldCount", fields.len().to_string()));
    }
    let summary = GitCommitSummary {
        hash: fields[0].to_string(),
        short_hash: fields[1].to_string(),
        parents: split_space_list(fields[2]),
        author_name: fields[3].to_string(),
        author_email: fields[4].to_string(),
        authored_at_ms: unix_seconds_to_ms(fields[5]),
        refs: split_refs(fields[6]),
        subject: fields[7].to_string(),
    };
    let body = fields[8].trim().to_string();
    Ok((summary, (!body.is_empty()).then_some(body)))
}

fn commit_file_changes(root: &Path, commit_hash: &str) -> VibexResult<Vec<GitCommitFileChange>> {
    let numstat = run_git_owned(
        root,
        &[
            "diff-tree".to_string(),
            "--root".to_string(),
            "--no-commit-id".to_string(),
            "--numstat".to_string(),
            "-r".to_string(),
            "-M".to_string(),
            commit_hash.to_string(),
        ],
    )?;
    let name_status = run_git_owned(
        root,
        &[
            "diff-tree".to_string(),
            "--root".to_string(),
            "--no-commit-id".to_string(),
            "--name-status".to_string(),
            "-r".to_string(),
            "-M".to_string(),
            commit_hash.to_string(),
        ],
    )?;
    let status_by_path = parse_name_status(&name_status);
    Ok(parse_numstat(&numstat, &status_by_path))
}

fn parse_name_status(output: &str) -> HashMap<String, (GitChangeKind, Option<String>)> {
    let mut out = HashMap::new();
    for line in output.lines() {
        let parts: Vec<&str> = line.split('\t').collect();
        if parts.len() < 2 {
            continue;
        }
        let code = parts[0];
        let (path, original_path) = if code.starts_with('R') || code.starts_with('C') {
            if parts.len() >= 3 {
                (parts[2].to_string(), Some(parts[1].to_string()))
            } else {
                (parts[1].to_string(), None)
            }
        } else {
            (parts[1].to_string(), None)
        };
        out.insert(path, (status_code_kind(code), original_path));
    }
    out
}

fn parse_numstat(
    output: &str,
    status_by_path: &HashMap<String, (GitChangeKind, Option<String>)>,
) -> Vec<GitCommitFileChange> {
    output
        .lines()
        .filter_map(|line| {
            let parts: Vec<&str> = line.split('\t').collect();
            if parts.len() < 3 {
                return None;
            }
            let (path, original_path) = if parts.len() >= 4 {
                (parts[3].to_string(), Some(parts[2].to_string()))
            } else {
                (parts[2].to_string(), None)
            };
            let (kind, status_original_path) = status_by_path
                .get(&path)
                .cloned()
                .unwrap_or((GitChangeKind::Modified, None));
            Some(GitCommitFileChange {
                path,
                original_path: original_path.or(status_original_path),
                kind,
                additions: parse_numstat_count(parts[0]),
                deletions: parse_numstat_count(parts[1]),
            })
        })
        .collect()
}

fn parse_blame_output(output: &str) -> VibexResult<Vec<GitBlameLine>> {
    let mut lines = Vec::new();
    let mut commit_hash = String::new();
    let mut final_line_number = 0_u32;
    let mut author_name = String::new();
    let mut authored_at_ms = None;
    let mut summary = String::new();

    for line in output.lines() {
        if let Some(content) = line.strip_prefix('\t') {
            lines.push(GitBlameLine {
                line_number: final_line_number,
                commit_hash: commit_hash.clone(),
                short_hash: commit_hash.chars().take(8).collect(),
                author_name: author_name.clone(),
                authored_at_ms,
                summary: summary.clone(),
                content: content.to_string(),
            });
            continue;
        }
        if let Some(value) = line.strip_prefix("author ") {
            author_name = value.to_string();
            continue;
        }
        if let Some(value) = line.strip_prefix("author-time ") {
            authored_at_ms = unix_seconds_to_ms(value);
            continue;
        }
        if let Some(value) = line.strip_prefix("summary ") {
            summary = value.to_string();
            continue;
        }
        let parts: Vec<&str> = line.split_whitespace().collect();
        if parts.len() >= 3 && is_hash_like(parts[0]) {
            commit_hash = parts[0].to_string();
            final_line_number = parts[2].parse::<u32>().unwrap_or(0);
        }
    }

    Ok(lines)
}

fn list_branches(root: &Path) -> VibexResult<Vec<GitBranchSummary>> {
    let output = run_git(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)%1f%(HEAD)%1f%(upstream:short)%1e",
            "refs/heads",
            "refs/remotes",
        ],
    )?;
    let mut branches = Vec::new();
    for record in output.trim_end_matches('\x1e').split('\x1e') {
        if record.trim().is_empty() {
            continue;
        }
        let fields: Vec<&str> = record.trim_start_matches('\n').split('\x1f').collect();
        if fields.len() != 3 {
            continue;
        }
        if fields[0].ends_with("/HEAD") {
            continue;
        }
        let upstream = (!fields[2].is_empty()).then(|| fields[2].to_string());
        let (ahead, behind) = if let Some(upstream) = upstream.as_deref() {
            ahead_behind(root, fields[0], upstream).unwrap_or((0, 0))
        } else {
            (0, 0)
        };
        branches.push(GitBranchSummary {
            name: fields[0].to_string(),
            current: fields[1] == "*",
            upstream,
            ahead,
            behind,
            detached: false,
        });
    }
    if branches.iter().all(|branch| !branch.current)
        && let Some(short_commit) = run_git_optional(root, &["rev-parse", "--short", "HEAD"])?
    {
        branches.push(GitBranchSummary {
            name: short_commit.trim().to_string(),
            current: true,
            upstream: None,
            ahead: 0,
            behind: 0,
            detached: true,
        });
    }
    Ok(branches)
}

fn list_remotes(root: &Path) -> VibexResult<Vec<GitRemoteSummary>> {
    let output = run_git(root, &["remote", "-v"])?;
    let mut by_name: HashMap<String, GitRemoteSummary> = HashMap::new();
    for line in output.lines() {
        let mut fields = line.split_whitespace();
        let Some(name) = fields.next() else {
            continue;
        };
        let Some(url) = fields.next() else {
            continue;
        };
        let kind = fields.next().unwrap_or_default();
        let entry = by_name.entry(name.to_string()).or_insert(GitRemoteSummary {
            name: name.to_string(),
            fetch_url: None,
            push_url: None,
        });
        match kind {
            "(fetch)" => entry.fetch_url = Some(url.to_string()),
            "(push)" => entry.push_url = Some(url.to_string()),
            _ => {}
        }
    }
    let mut remotes: Vec<_> = by_name.into_values().collect();
    remotes.sort_by(|left, right| left.name.cmp(&right.name));
    Ok(remotes)
}

fn parse_worktree_list(output: &str) -> Vec<GitWorktreeSummary> {
    let mut worktrees = Vec::new();
    let mut current: Option<GitWorktreeSummary> = None;
    for line in output.lines() {
        if line.trim().is_empty() {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            continue;
        }
        if let Some(path) = line.strip_prefix("worktree ") {
            if let Some(worktree) = current.take() {
                worktrees.push(worktree);
            }
            current = Some(GitWorktreeSummary {
                path: path.to_string(),
                path_identity: None,
                repository_identity: None,
                branch: None,
                head: None,
                detached: false,
                bare: false,
                prunable: false,
                workspace_id: None,
                managed: false,
            });
            continue;
        }
        let Some(worktree) = current.as_mut() else {
            continue;
        };
        if let Some(head) = line.strip_prefix("HEAD ") {
            worktree.head = Some(head.to_string());
        } else if let Some(branch) = line.strip_prefix("branch ") {
            worktree.branch = Some(branch.trim_start_matches("refs/heads/").to_string());
        } else if line == "detached" {
            worktree.detached = true;
        } else if line == "bare" {
            worktree.bare = true;
        } else if line.starts_with("prunable") {
            worktree.prunable = true;
        }
    }
    if let Some(worktree) = current.take() {
        worktrees.push(worktree);
    }
    worktrees
}

fn ahead_behind(root: &Path, local: &str, upstream: &str) -> VibexResult<(u32, u32)> {
    let output = run_git_owned(
        root,
        &[
            "rev-list".to_string(),
            "--left-right".to_string(),
            "--count".to_string(),
            format!("{local}...{upstream}"),
        ],
    )?;
    let counts: Vec<&str> = output.split_whitespace().collect();
    if counts.len() != 2 {
        return Ok((0, 0));
    }
    Ok((
        counts[0].parse::<u32>().unwrap_or(0),
        counts[1].parse::<u32>().unwrap_or(0),
    ))
}

fn bounded_line_range(
    start_line: Option<u32>,
    end_line: Option<u32>,
) -> VibexResult<(Option<u32>, Option<u32>, bool)> {
    let start = start_line.unwrap_or(1).max(1);
    let requested_end = end_line.unwrap_or(start.saturating_add(MAX_BLAME_LINES - 1));
    if requested_end < start {
        return Err(VibexError::validation(
            "git_blame_invalid_range",
            "blame end line must be greater than or equal to start line",
        ));
    }
    let max_end = start.saturating_add(MAX_BLAME_LINES - 1);
    Ok((
        Some(start),
        Some(requested_end.min(max_end)),
        requested_end > max_end,
    ))
}

fn split_space_list(value: &str) -> Vec<String> {
    value
        .split_whitespace()
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn split_refs(value: &str) -> Vec<String> {
    value
        .split(',')
        .map(str::trim)
        .filter(|part| !part.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

fn unix_seconds_to_ms(value: &str) -> Option<i64> {
    value
        .trim()
        .parse::<i64>()
        .ok()
        .map(|seconds| seconds * 1000)
}

fn status_code_kind(code: &str) -> GitChangeKind {
    match code.chars().next() {
        Some('A') => GitChangeKind::Added,
        Some('D') => GitChangeKind::Deleted,
        Some('R') => GitChangeKind::Renamed,
        Some('C') => GitChangeKind::Copied,
        Some('T') => GitChangeKind::TypeChanged,
        Some('U') => GitChangeKind::Unmerged,
        Some('M') => GitChangeKind::Modified,
        _ => GitChangeKind::Unknown,
    }
}

fn parse_numstat_count(value: &str) -> u32 {
    value.parse::<u32>().unwrap_or(0)
}

fn is_hash_like(value: &str) -> bool {
    let value = value.strip_prefix('^').unwrap_or(value);
    value.len() >= 8 && value.chars().all(|character| character.is_ascii_hexdigit())
}

fn normalized_optional(value: &Option<String>) -> Option<&str> {
    value
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
}

fn validate_commitish(value: &str) -> VibexResult<()> {
    validate_ref_arg(value)
}

fn validate_existing_commitish(root: &Path, value: &str) -> VibexResult<()> {
    validate_commitish(value)?;
    let verify_ref = format!("{value}^{{commit}}");
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["rev-parse", "--verify", &verify_ref])
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    if !output.status.success() {
        return Err(
            VibexError::validation("git_ref_not_found", "Git ref was not found")
                .with_diagnostic("ref", value)
                .with_diagnostic(
                    "stderr",
                    String::from_utf8_lossy(&output.stderr)
                        .trim()
                        .chars()
                        .take(2000)
                        .collect::<String>(),
                ),
        );
    }
    Ok(())
}

fn validate_branch_name(root: &Path, value: &str) -> VibexResult<()> {
    validate_ref_arg(value)?;
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["check-ref-format", "--branch", value])
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    if !output.status.success() {
        return Err(VibexError::validation(
            "git_invalid_branch_name",
            "Git branch name is invalid",
        )
        .with_diagnostic("branch", value)
        .with_diagnostic("stderr", String::from_utf8_lossy(&output.stderr).trim()));
    }
    Ok(())
}

fn validate_remote_name(value: &str) -> VibexResult<()> {
    validate_ref_arg(value)
}

fn validate_ref_arg(value: &str) -> VibexResult<()> {
    if value.trim().is_empty() {
        return Err(VibexError::validation(
            "git_ref_empty",
            "Git ref must not be empty",
        ));
    }
    if value.starts_with('-') || value.contains('\0') || value.chars().any(char::is_whitespace) {
        return Err(VibexError::validation(
            "git_ref_invalid",
            "Git ref contains unsupported characters",
        )
        .with_diagnostic("ref", value));
    }
    Ok(())
}

fn run_git_owned(root: &Path, args: &[String]) -> VibexResult<String> {
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;

    if !output.status.success() {
        return Err(
            git_command_error("git_command_failed", "git command failed", &output)
                .with_diagnostic("args", args.join(" ")),
        );
    }

    String::from_utf8(output.stdout).map_err(|err| {
        VibexError::process("git_output_not_utf8", "git output was not valid UTF-8")
            .with_diagnostic("error", err.to_string())
    })
}

fn run_git(root: &Path, args: &[&str]) -> VibexResult<String> {
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;

    if !output.status.success() {
        return Err(
            git_command_error("git_command_failed", "git command failed", &output)
                .with_diagnostic("args", args.join(" ")),
        );
    }

    String::from_utf8(output.stdout).map_err(|err| {
        VibexError::process("git_output_not_utf8", "git output was not valid UTF-8")
            .with_diagnostic("error", err.to_string())
    })
}

fn run_git_optional(root: &Path, args: &[&str]) -> VibexResult<Option<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(root)
        .args(args)
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;

    if !output.status.success() {
        return Ok(None);
    }

    String::from_utf8(output.stdout).map(Some).map_err(|err| {
        VibexError::process("git_output_not_utf8", "git output was not valid UTF-8")
            .with_diagnostic("error", err.to_string())
    })
}

fn run_git_paths(root: &Path, args: &[&str], paths: &[String]) -> VibexResult<String> {
    let mut command = git_command();
    command.arg("-C").arg(root).args(args).arg("--");
    for path in paths {
        command.arg(path);
    }
    let output = command.output().map_err(|err| {
        VibexError::process("git_spawn_failed", "failed to spawn git")
            .with_diagnostic("error", err.to_string())
    })?;
    if !output.status.success() {
        return Err(
            git_command_error("git_command_failed", "git command failed", &output)
                .with_diagnostic("args", args.join(" ")),
        );
    }
    String::from_utf8(output.stdout).map_err(|err| {
        VibexError::process("git_output_not_utf8", "git output was not valid UTF-8")
            .with_diagnostic("error", err.to_string())
    })
}

fn run_git_no_index_for_untracked(root: &Path, path: &str) -> VibexResult<String> {
    let null_path = if cfg!(windows) { "NUL" } else { "/dev/null" };
    let output = git_command()
        .arg("-C")
        .arg(root)
        .args(["diff", "--no-index", "--", null_path, path])
        .output()
        .map_err(|err| {
            VibexError::process("git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    match output.status.code() {
        Some(0) | Some(1) => String::from_utf8(output.stdout).map_err(|err| {
            VibexError::process("git_output_not_utf8", "git output was not valid UTF-8")
                .with_diagnostic("error", err.to_string())
        }),
        _ => Err(git_command_error(
            "git_untracked_diff_failed",
            "failed to create untracked file diff",
            &output,
        )),
    }
}

fn git_command_error(code: &'static str, message: &'static str, output: &Output) -> VibexError {
    VibexError::process(code, message)
        .with_diagnostic("status", output.status.to_string())
        .with_diagnostic(
            "stderr",
            String::from_utf8_lossy(&output.stderr)
                .trim()
                .chars()
                .take(2000)
                .collect::<String>(),
        )
}

fn validate_paths(paths: &[String]) -> VibexResult<Vec<String>> {
    if paths.is_empty() {
        return Err(VibexError::validation(
            "git_paths_empty",
            "at least one Git path is required",
        ));
    }
    for path in paths {
        validate_git_path(path)?;
    }
    Ok(paths.to_vec())
}

fn validate_git_path(path: &str) -> VibexResult<()> {
    if path.trim().is_empty() {
        return Err(VibexError::validation(
            "git_path_empty",
            "Git path must not be empty",
        ));
    }
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(VibexError::validation(
            "git_absolute_path_rejected",
            "Git path must be relative to the repository",
        ));
    }
    for component in path.components() {
        match component {
            Component::Normal(_) | Component::CurDir => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VibexError::validation(
                    "git_path_traversal_rejected",
                    "Git path must not escape the repository",
                ));
            }
        }
    }
    Ok(())
}

fn truncate_diff(diff: String) -> (String, bool) {
    if diff.len() <= MAX_DIFF_BYTES {
        return (diff, false);
    }
    let mut end = MAX_DIFF_BYTES;
    while !diff.is_char_boundary(end) {
        end -= 1;
    }
    (diff[..end].to_string(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn current_workspace_reports_status() {
        let status = git_status(".").unwrap();
        assert!(status.branch.is_some() || status.short_commit.is_some());
    }

    #[test]
    fn non_git_directory_is_typed_error() {
        let temp_dir = std::env::temp_dir();
        let err = git_status(temp_dir).unwrap_err();
        assert_eq!(err.code, "not_git_repository");
    }

    #[test]
    fn repository_lock_rejects_duplicate_mutations_at_the_service_boundary() {
        let root = temp_repo("mutation-lock");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "a.txt", "one\n", "initial");
        std::fs::write(root.join("a.txt"), "two\n").unwrap();
        let workspace_id = WorkspaceId::new();
        let request = GitStageRequest {
            workspace_id: workspace_id.clone(),
            paths: vec!["a.txt".into()],
        };
        let repo_root = repo_root(&root).unwrap();
        let guard = GitMutationGuard::claim(&repo_root).unwrap();
        let error = stage(workspace_id.clone(), &root, &request).unwrap_err();
        assert_eq!(error.code, "git_mutation_in_progress");
        drop(guard);
        assert!(stage(workspace_id, &root, &request).is_ok());
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn stage_unstage_revert_and_commit_in_temp_repo() {
        let root = temp_repo("ops");
        std::fs::create_dir_all(&root).unwrap();
        run_raw(&root, &["init"]).unwrap();
        run_raw(&root, &["config", "user.email", "vibex@example.invalid"]).unwrap();
        run_raw(&root, &["config", "user.name", "Vibex Test"]).unwrap();
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        run_raw(&root, &["add", "README.md"]).unwrap();
        run_raw(&root, &["commit", "-m", "initial"]).unwrap();

        std::fs::write(root.join("README.md"), "hello\nworld\n").unwrap();
        let workspace_id = WorkspaceId::new();
        let request = GitStageRequest {
            workspace_id: workspace_id.clone(),
            paths: vec!["README.md".to_string()],
        };
        let staged = stage(workspace_id.clone(), &root, &request).unwrap();
        assert_eq!(staged.staged_count, 1);
        let unstaged = unstage(workspace_id.clone(), &root, &request).unwrap();
        assert_eq!(unstaged.staged_count, 0);
        let reverted = revert(workspace_id.clone(), &root, &request).unwrap();
        assert!(!reverted.dirty);

        std::fs::write(root.join("README.md"), "changed\n").unwrap();
        stage(workspace_id.clone(), &root, &request).unwrap();
        let committed = commit(
            workspace_id,
            &root,
            &GitCommitRequest {
                workspace_id: request.workspace_id,
                message: "update readme".to_string(),
                paths: Vec::new(),
                amend: false,
                push_after: false,
            },
        )
        .unwrap();
        assert!(!committed.short_commit.is_empty());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn selected_file_commit_leaves_unchecked_staged_paths_staged() {
        let root = temp_repo("selected-commit");
        std::fs::create_dir_all(&root).unwrap();
        run_raw(&root, &["init"]).unwrap();
        run_raw(&root, &["config", "user.email", "vibex@example.invalid"]).unwrap();
        run_raw(&root, &["config", "user.name", "Vibex Test"]).unwrap();
        std::fs::write(root.join("a.txt"), "a1\n").unwrap();
        std::fs::write(root.join("b.txt"), "b1\n").unwrap();
        run_raw(&root, &["add", "a.txt", "b.txt"]).unwrap();
        run_raw(&root, &["commit", "-m", "initial"]).unwrap();

        std::fs::write(root.join("a.txt"), "a2\n").unwrap();
        std::fs::write(root.join("b.txt"), "b2\n").unwrap();
        run_raw(&root, &["add", "b.txt"]).unwrap();
        let workspace_id = WorkspaceId::new();
        commit(
            workspace_id.clone(),
            &root,
            &GitCommitRequest {
                workspace_id,
                message: "commit selected a".to_string(),
                paths: vec!["a.txt".to_string()],
                amend: false,
                push_after: false,
            },
        )
        .unwrap();

        let staged = run_git(&root, &["diff", "--staged", "--name-only"]).unwrap();
        assert_eq!(staged.trim(), "b.txt");
        let committed_files =
            run_git(&root, &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(committed_files.lines().any(|line| line == "a.txt"));
        assert!(!committed_files.lines().any(|line| line == "b.txt"));

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn selected_untracked_file_is_added_before_commit() {
        let root = temp_repo("selected-untracked");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        std::fs::write(root.join("new.txt"), "new\n").unwrap();

        let workspace_id = WorkspaceId::new();
        commit(
            workspace_id.clone(),
            &root,
            &GitCommitRequest {
                workspace_id,
                message: "add selected file".to_string(),
                paths: vec!["new.txt".to_string()],
                amend: false,
                push_after: false,
            },
        )
        .unwrap();

        let committed_files =
            run_git(&root, &["show", "--name-only", "--format=", "HEAD"]).unwrap();
        assert!(committed_files.lines().any(|line| line == "new.txt"));
        assert!(!status(WorkspaceId::new(), &root).unwrap().dirty);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn amend_selected_file_commit_rewrites_head_without_new_commit() {
        let root = temp_repo("selected-amend");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let original_head = run_git(&root, &["rev-parse", "HEAD"]).unwrap();
        std::fs::write(root.join("README.md"), "amended\n").unwrap();

        let workspace_id = WorkspaceId::new();
        commit(
            workspace_id.clone(),
            &root,
            &GitCommitRequest {
                workspace_id,
                message: "initial amended".to_string(),
                paths: vec!["README.md".to_string()],
                amend: true,
                push_after: false,
            },
        )
        .unwrap();

        let commit_count = run_git(&root, &["rev-list", "--count", "HEAD"]).unwrap();
        let new_head = run_git(&root, &["rev-parse", "HEAD"]).unwrap();
        let content = run_git(&root, &["show", "HEAD:README.md"]).unwrap();
        assert_eq!(commit_count.trim(), "1");
        assert_ne!(original_head.trim(), new_head.trim());
        assert_eq!(content, "amended\n");

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_detail_and_blame_are_bounded_and_typed() {
        let root = temp_repo("inspect");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\nworld\n", "initial");
        std::fs::write(root.join("README.md"), "hello\nworld\nagain\n").unwrap();
        run_raw(&root, &["add", "README.md"]).unwrap();
        run_raw(&root, &["commit", "-m", "update readme"]).unwrap();

        let workspace_id = WorkspaceId::new();
        let history = history(
            &root,
            &GitHistoryRequest {
                workspace_id: workspace_id.clone(),
                limit: Some(1),
                before_commit: None,
                ref_name: None,
                author: None,
            },
        )
        .unwrap();
        assert_eq!(history.commits.len(), 1);
        assert!(history.has_more);
        assert_eq!(history.commits[0].subject, "update readme");

        let detail = commit_detail(
            &root,
            &GitCommitDetailRequest {
                workspace_id: workspace_id.clone(),
                commit_hash: history.commits[0].hash.clone(),
                include_patch: true,
            },
        )
        .unwrap();
        assert!(detail.files.iter().any(|file| file.path == "README.md"));
        assert!(
            detail
                .patch
                .as_deref()
                .unwrap_or_default()
                .contains("again")
        );

        let blame = blame(
            &root,
            &GitBlameRequest {
                workspace_id,
                path: "README.md".to_string(),
                start_line: Some(1),
                end_line: Some(2),
            },
        )
        .unwrap();
        assert_eq!(blame.lines.len(), 2);
        assert_eq!(blame.lines[0].line_number, 1);

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn history_filters_by_ref_and_author_with_independent_author_list() {
        let root = temp_repo("history-filter");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let default_branch = run_git(&root, &["rev-parse", "--abbrev-ref", "HEAD"]).unwrap();
        run_raw(&root, &["checkout", "-b", "feature/history-filter"]).unwrap();
        std::fs::write(root.join("alice.txt"), "alice\n").unwrap();
        run_raw(&root, &["add", "alice.txt"]).unwrap();
        run_raw(
            &root,
            &[
                "-c",
                "user.name=Alice",
                "-c",
                "user.email=alice@example.invalid",
                "commit",
                "-m",
                "alice feature",
            ],
        )
        .unwrap();
        std::fs::write(root.join("bob.txt"), "bob\n").unwrap();
        run_raw(&root, &["add", "bob.txt"]).unwrap();
        run_raw(
            &root,
            &[
                "-c",
                "user.name=Bob",
                "-c",
                "user.email=bob@example.invalid",
                "commit",
                "-m",
                "bob feature",
            ],
        )
        .unwrap();
        run_raw(&root, &["checkout", default_branch.trim()]).unwrap();
        std::fs::write(root.join("main.txt"), "main\n").unwrap();
        run_raw(&root, &["add", "main.txt"]).unwrap();
        run_raw(&root, &["commit", "-m", "main only"]).unwrap();

        let history = history(
            &root,
            &GitHistoryRequest {
                workspace_id: WorkspaceId::new(),
                limit: Some(10),
                before_commit: None,
                ref_name: Some("feature/history-filter".to_string()),
                author: Some("Bob".to_string()),
            },
        )
        .unwrap();

        assert_eq!(history.commits.len(), 1);
        assert_eq!(history.commits[0].subject, "bob feature");
        assert!(
            history
                .commits
                .iter()
                .all(|commit| commit.author_name == "Bob")
        );
        assert!(
            !history
                .commits
                .iter()
                .any(|commit| commit.subject == "main only")
        );
        assert!(
            history
                .authors
                .iter()
                .any(|author| author.name == "Alice" && author.email == "alice@example.invalid")
        );
        assert!(
            history
                .authors
                .iter()
                .any(|author| author.name == "Bob" && author.email == "bob@example.invalid")
        );

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn branch_create_checkout_and_local_fetch_work() {
        let root = temp_repo("branch");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let workspace_id = WorkspaceId::new();
        let created = branch_create(
            workspace_id.clone(),
            &root,
            &GitBranchCreateRequest {
                workspace_id: workspace_id.clone(),
                name: "feature/test-branch".to_string(),
                base_ref: Some("HEAD".to_string()),
                checkout: true,
            },
        )
        .unwrap();
        assert_eq!(created.branch.as_deref(), Some("feature/test-branch"));
        let checked_out = branch_checkout(
            workspace_id.clone(),
            &root,
            &GitBranchCheckoutRequest {
                workspace_id: workspace_id.clone(),
                name: "master".to_string(),
            },
        )
        .unwrap();
        assert_eq!(checked_out.branch.as_deref(), Some("master"));
        let branches = branch_list(workspace_id.clone(), &root).unwrap();
        assert!(
            branches
                .branches
                .iter()
                .any(|branch| branch.name == "feature/test-branch")
        );

        let bare = temp_repo("bare");
        run_raw(&root, &["init", "--bare", bare.to_str().unwrap()]).unwrap();
        run_raw(&root, &["remote", "add", "origin", bare.to_str().unwrap()]).unwrap();
        run_raw(&root, &["push", "-u", "origin", "master"]).unwrap();
        let result = remote_action(
            workspace_id,
            &root,
            &GitRemoteActionRequest {
                workspace_id: WorkspaceId::new(),
                kind: GitRemoteActionKind::Fetch,
                remote: Some("origin".to_string()),
                branch: None,
            },
        )
        .unwrap();
        assert!(result.summary.contains("fetch"));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(bare);
    }

    #[test]
    fn push_without_upstream_uses_origin_and_reports_failures() {
        let root = temp_repo("push-origin");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let bare = temp_repo("push-origin-bare");
        run_raw(&root, &["init", "--bare", bare.to_str().unwrap()]).unwrap();
        run_raw(&root, &["remote", "add", "origin", bare.to_str().unwrap()]).unwrap();
        let workspace_id = WorkspaceId::new();

        let pushed = remote_action(
            workspace_id.clone(),
            &root,
            &GitRemoteActionRequest {
                workspace_id: workspace_id.clone(),
                kind: GitRemoteActionKind::Push,
                remote: None,
                branch: None,
            },
        )
        .unwrap();
        assert_eq!(pushed.kind, GitRemoteActionKind::Push);
        assert!(!pushed.summary.trim().is_empty());
        let upstream = current_upstream(&root).unwrap().unwrap();
        assert!(upstream.starts_with("origin/"));

        let no_origin = temp_repo("push-no-origin");
        std::fs::create_dir_all(&no_origin).unwrap();
        init_repo_with_commit(&no_origin, "README.md", "hello\n", "initial");
        let err = remote_action(
            WorkspaceId::new(),
            &no_origin,
            &GitRemoteActionRequest {
                workspace_id: WorkspaceId::new(),
                kind: GitRemoteActionKind::Push,
                remote: None,
                branch: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "git_push_no_origin");

        let detached = temp_repo("push-detached");
        std::fs::create_dir_all(&detached).unwrap();
        init_repo_with_commit(&detached, "README.md", "hello\n", "initial");
        let detached_bare = temp_repo("push-detached-bare");
        run_raw(
            &detached,
            &["init", "--bare", detached_bare.to_str().unwrap()],
        )
        .unwrap();
        run_raw(
            &detached,
            &["remote", "add", "origin", detached_bare.to_str().unwrap()],
        )
        .unwrap();
        run_raw(&detached, &["checkout", "--detach", "HEAD"]).unwrap();
        let err = remote_action(
            WorkspaceId::new(),
            &detached,
            &GitRemoteActionRequest {
                workspace_id: WorkspaceId::new(),
                kind: GitRemoteActionKind::Push,
                remote: None,
                branch: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, "git_push_detached_head");

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(bare);
        let _ = std::fs::remove_dir_all(no_origin);
        let _ = std::fs::remove_dir_all(detached);
        let _ = std::fs::remove_dir_all(detached_bare);
    }

    #[test]
    fn worktree_add_list_and_remove_work() {
        let root = temp_repo("worktree-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let worktree_path = temp_repo("worktree-feature");
        let workspace_id = WorkspaceId::new();
        let request = GitWorktreeCreateRequest {
            workspace_id: workspace_id.clone(),
            branch_name: "feature/worktree-test".to_string(),
            base_ref: Some("HEAD".to_string()),
            name: None,
            worktree_path: None,
            target_workspace_id: None,
            target_branch: None,
        };
        let created = worktree_add(&root, &worktree_path, &request).unwrap();
        assert!(same_path_identity(&created.path, &worktree_path));

        let list = worktree_list(workspace_id.clone(), &root).unwrap();
        assert!(
            list.worktrees
                .iter()
                .any(|worktree| same_path_identity(&worktree.path, &worktree_path))
        );

        let removed = worktree_remove(
            &root,
            &GitWorktreeDiscardRequest {
                workspace_id,
                worktree_path: worktree_path.to_string_lossy().to_string(),
                force: false,
                expected_head: created.head,
                preflight_revision: None,
            },
        )
        .unwrap();
        assert!(removed.is_empty() || removed.contains("Preparing"));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(worktree_path);
    }

    #[test]
    fn worktree_create_rejects_invalid_custom_name_and_path() {
        let root = temp_repo("worktree-validation-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let mut request = GitWorktreeCreateRequest {
            workspace_id: WorkspaceId::new(),
            branch_name: "feature/validation".to_string(),
            base_ref: Some("HEAD".to_string()),
            name: Some("valid".to_string()),
            worktree_path: Some("relative/worktree".to_string()),
            target_workspace_id: None,
            target_branch: None,
        };
        assert_eq!(
            validate_worktree_create(&root, &request).unwrap_err().code,
            "worktree_path_not_absolute"
        );

        request.worktree_path = Some(
            temp_repo("worktree-validation-target")
                .to_string_lossy()
                .into_owned(),
        );
        request.name = Some("bad\nname".to_string());
        assert_eq!(
            validate_worktree_create(&root, &request).unwrap_err().code,
            "worktree_name_invalid"
        );
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn worktree_merge_revalidates_source_and_target_under_mutation_lock() {
        let root = temp_repo("worktree-verified-merge-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let worktree_path = temp_repo("worktree-verified-merge-feature");
        let workspace_id = WorkspaceId::new();
        let branch = "feature/verified-merge";
        let _created = worktree_add(
            &root,
            &worktree_path,
            &GitWorktreeCreateRequest {
                workspace_id: workspace_id.clone(),
                branch_name: branch.to_string(),
                base_ref: Some("HEAD".to_string()),
                name: None,
                worktree_path: None,
                target_workspace_id: None,
                target_branch: None,
            },
        )
        .unwrap();
        std::fs::write(worktree_path.join("feature.txt"), "feature\n").unwrap();
        run_raw(&worktree_path, &["add", "feature.txt"]).unwrap();
        run_raw(&worktree_path, &["commit", "-m", "feature"]).unwrap();
        let source_head = resolve_ref_head(&root, branch).unwrap();
        let target_branch = current_branch(&root).unwrap().unwrap();
        let target_head = resolve_head(&root).unwrap();

        let error = worktree_merge(&root, branch, &"0".repeat(40), &target_branch, &target_head)
            .unwrap_err();
        assert_eq!(error.code, "worktree_source_head_changed");
        let error = worktree_merge(&root, branch, &source_head, &target_branch, &"0".repeat(40))
            .unwrap_err();
        assert_eq!(error.code, "worktree_target_head_changed");
        assert!(!root.join("feature.txt").exists());

        worktree_merge(&root, branch, &source_head, &target_branch, &target_head).unwrap();
        assert!(root.join("feature.txt").is_file());
        worktree_remove(
            &root,
            &GitWorktreeDiscardRequest {
                workspace_id,
                worktree_path: worktree_path.to_string_lossy().to_string(),
                force: false,
                expected_head: Some(source_head),
                preflight_revision: None,
            },
        )
        .unwrap();

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(worktree_path);
    }

    #[test]
    fn worktree_rebase_and_merge_rewrites_source_then_fast_forwards_target() {
        let root = temp_repo("worktree-rebase-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "base\n", "base");
        let source_path = temp_repo("worktree-rebase-source");
        let source_branch = "feature/rebase";
        worktree_add(
            &root,
            &source_path,
            &GitWorktreeCreateRequest {
                workspace_id: WorkspaceId::new(),
                branch_name: source_branch.to_string(),
                base_ref: Some("HEAD".to_string()),
                name: None,
                worktree_path: None,
                target_workspace_id: None,
                target_branch: None,
            },
        )
        .unwrap();
        std::fs::write(source_path.join("source.txt"), "source\n").unwrap();
        run_raw(&source_path, &["add", "source.txt"]).unwrap();
        run_raw(&source_path, &["commit", "-m", "source"]).unwrap();
        let source_head = resolve_head(&source_path).unwrap();
        std::fs::write(root.join("target.txt"), "target\n").unwrap();
        run_raw(&root, &["add", "target.txt"]).unwrap();
        run_raw(&root, &["commit", "-m", "target"]).unwrap();
        let target_branch = current_branch(&root).unwrap().unwrap();
        let target_head = resolve_head(&root).unwrap();

        let rebased_head = worktree_rebase_source(
            &source_path,
            &root,
            source_branch,
            &source_head,
            &target_branch,
            &target_head,
        )
        .unwrap();
        assert_ne!(rebased_head, source_head);
        assert_eq!(resolve_head(&root).unwrap(), target_head);
        let head_after = worktree_rebase_finish(
            &source_path,
            &root,
            source_branch,
            &rebased_head,
            &target_branch,
            &target_head,
        )
        .unwrap();
        assert_eq!(head_after, rebased_head);
        let parents = run_git(&root, &["rev-list", "--parents", "-n", "1", &head_after]).unwrap();
        assert_eq!(parents.split_whitespace().count(), 2);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(source_path);
    }

    #[test]
    fn worktree_rebase_conflicts_can_continue_or_abort_exactly() {
        let root = temp_repo("worktree-rebase-conflict-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "conflict.txt", "base\n", "base");
        let source_path = temp_repo("worktree-rebase-conflict-source");
        let source_branch = "feature/rebase-conflict";
        worktree_add(
            &root,
            &source_path,
            &GitWorktreeCreateRequest {
                workspace_id: WorkspaceId::new(),
                branch_name: source_branch.to_string(),
                base_ref: Some("HEAD".to_string()),
                name: None,
                worktree_path: None,
                target_workspace_id: None,
                target_branch: None,
            },
        )
        .unwrap();
        std::fs::write(source_path.join("conflict.txt"), "source\n").unwrap();
        run_raw(&source_path, &["add", "conflict.txt"]).unwrap();
        run_raw(&source_path, &["commit", "-m", "source"]).unwrap();
        let source_head = resolve_head(&source_path).unwrap();
        std::fs::write(root.join("conflict.txt"), "target\n").unwrap();
        run_raw(&root, &["add", "conflict.txt"]).unwrap();
        run_raw(&root, &["commit", "-m", "target"]).unwrap();
        let target_branch = current_branch(&root).unwrap().unwrap();
        let target_head = resolve_head(&root).unwrap();

        worktree_rebase_source(
            &source_path,
            &root,
            source_branch,
            &source_head,
            &target_branch,
            &target_head,
        )
        .unwrap_err();
        assert!(
            worktree_rebase_scene_matches(
                &source_path,
                &root,
                source_branch,
                &source_head,
                &target_branch,
                &target_head,
            )
            .unwrap()
        );
        worktree_select_conflict_version_for_strategy(
            &source_path,
            "conflict.txt",
            GitWorktreeConflictVersion::Source,
            GitWorktreeMergeStrategy::RebaseAndMerge,
        )
        .unwrap();
        worktree_stage_conflicts(
            WorkspaceId::new(),
            &source_path,
            &["conflict.txt".to_string()],
        )
        .unwrap();
        let rebased_head = worktree_rebase_continue(
            &source_path,
            &root,
            source_branch,
            &source_head,
            &target_branch,
            &target_head,
        )
        .unwrap();
        assert_eq!(
            std::fs::read_to_string(source_path.join("conflict.txt")).unwrap(),
            "source\n"
        );
        worktree_rebase_finish(
            &source_path,
            &root,
            source_branch,
            &rebased_head,
            &target_branch,
            &target_head,
        )
        .unwrap();

        let abort_root = temp_repo("worktree-rebase-abort-main");
        std::fs::create_dir_all(&abort_root).unwrap();
        init_repo_with_commit(&abort_root, "conflict.txt", "base\n", "base");
        let abort_source = temp_repo("worktree-rebase-abort-source");
        let abort_branch = "feature/rebase-abort";
        worktree_add(
            &abort_root,
            &abort_source,
            &GitWorktreeCreateRequest {
                workspace_id: WorkspaceId::new(),
                branch_name: abort_branch.to_string(),
                base_ref: Some("HEAD".to_string()),
                name: None,
                worktree_path: None,
                target_workspace_id: None,
                target_branch: None,
            },
        )
        .unwrap();
        std::fs::write(abort_source.join("conflict.txt"), "source\n").unwrap();
        run_raw(&abort_source, &["add", "conflict.txt"]).unwrap();
        run_raw(&abort_source, &["commit", "-m", "source"]).unwrap();
        let abort_source_head = resolve_head(&abort_source).unwrap();
        std::fs::write(abort_root.join("conflict.txt"), "target\n").unwrap();
        run_raw(&abort_root, &["add", "conflict.txt"]).unwrap();
        run_raw(&abort_root, &["commit", "-m", "target"]).unwrap();
        let abort_target_branch = current_branch(&abort_root).unwrap().unwrap();
        let abort_target_head = resolve_head(&abort_root).unwrap();
        worktree_rebase_source(
            &abort_source,
            &abort_root,
            abort_branch,
            &abort_source_head,
            &abort_target_branch,
            &abort_target_head,
        )
        .unwrap_err();
        worktree_rebase_abort(
            &abort_source,
            &abort_root,
            abort_branch,
            &abort_source_head,
            &abort_target_branch,
            &abort_target_head,
        )
        .unwrap();
        assert_eq!(resolve_head(&abort_source).unwrap(), abort_source_head);
        assert_eq!(resolve_head(&abort_root).unwrap(), abort_target_head);

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(source_path);
        let _ = std::fs::remove_dir_all(abort_root);
        let _ = std::fs::remove_dir_all(abort_source);
    }

    #[test]
    fn merge_readiness_facts_and_conflict_lifecycle_are_exact() {
        let root = temp_repo("worktree-conflict-continue");
        let (source_branch, source_head, target_branch, target_head) =
            prepare_conflict(&root, Some(b"base\n"), Some(b"source\n"), Some(b"target\n"));
        let clean_fingerprint = worktree_dirty_fingerprint(&root).unwrap();
        assert!(!clean_fingerprint.is_empty());
        let summary = worktree_merge_summary(&root, &target_head, &source_head).unwrap();
        assert_eq!(summary.commit_count, 1);
        assert_eq!(summary.file_count, 1);

        let error = worktree_merge(
            &root,
            &source_branch,
            &source_head,
            &target_branch,
            &target_head,
        )
        .unwrap_err();
        assert_eq!(
            active_git_operation(&root).unwrap().as_deref(),
            Some("merge")
        );
        assert!(!error.code.is_empty());
        let conflicts = worktree_conflicts(&root).unwrap();
        assert_eq!(conflicts.len(), 1);
        assert_eq!(conflicts[0].kind, GitWorktreeConflictKind::BothModified);
        assert_ne!(
            worktree_dirty_fingerprint(&root).unwrap(),
            clean_fingerprint
        );

        worktree_select_conflict_version(&root, "conflict.bin", GitWorktreeConflictVersion::Source)
            .unwrap();
        assert_eq!(
            std::fs::read(root.join("conflict.bin")).unwrap(),
            b"source\n"
        );
        assert_eq!(
            worktree_stage_conflicts(WorkspaceId::new(), &root, &["conflict.bin".to_string()])
                .unwrap(),
            Vec::new()
        );
        let head_after =
            worktree_merge_continue(&root, &source_head, &target_branch, &target_head).unwrap();
        assert_eq!(resolve_head(&root).unwrap(), head_after);
        assert!(merge_head(&root).unwrap().is_none());

        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn conflict_categories_and_abort_preserve_the_target() {
        type ConflictScenario<'a> = (
            &'a str,
            Option<&'a [u8]>,
            Option<&'a [u8]>,
            Option<&'a [u8]>,
            GitWorktreeConflictKind,
        );
        let scenarios: [ConflictScenario<'_>; 3] = [
            (
                "both-added",
                None,
                Some(b"source\n"),
                Some(b"target\n"),
                GitWorktreeConflictKind::BothAdded,
            ),
            (
                "delete-modify",
                Some(b"base\n"),
                None,
                Some(b"target\n"),
                GitWorktreeConflictKind::DeletedBySource,
            ),
            (
                "binary",
                Some(b"base\0value"),
                Some(b"source\0value"),
                Some(b"target\0value"),
                GitWorktreeConflictKind::Binary,
            ),
        ];
        for (label, base, source, target, expected_kind) in scenarios {
            let root = temp_repo(label);
            let (source_branch, source_head, target_branch, target_head) =
                prepare_conflict(&root, base, source, target);
            worktree_merge(
                &root,
                &source_branch,
                &source_head,
                &target_branch,
                &target_head,
            )
            .unwrap_err();
            let conflicts = worktree_conflicts(&root).unwrap();
            assert_eq!(conflicts.len(), 1, "{label}");
            assert_eq!(conflicts[0].kind, expected_kind, "{label}");
            worktree_merge_abort(&root, &source_head, &target_branch, &target_head).unwrap();
            assert_eq!(resolve_head(&root).unwrap(), target_head, "{label}");
            assert!(merge_head(&root).unwrap().is_none(), "{label}");
            assert!(!status(WorkspaceId::new(), &root).unwrap().dirty, "{label}");
            let _ = std::fs::remove_dir_all(root);
        }
    }

    #[test]
    fn archived_worktree_restores_the_exact_path_branch_and_head() {
        let root = temp_repo("worktree-restore-main");
        std::fs::create_dir_all(&root).unwrap();
        init_repo_with_commit(&root, "README.md", "hello\n", "initial");
        let path = temp_repo("worktree-restore-feature");
        let branch = "feature/restore";
        let workspace_id = WorkspaceId::new();
        let created = worktree_add(
            &root,
            &path,
            &GitWorktreeCreateRequest {
                workspace_id: workspace_id.clone(),
                branch_name: branch.to_string(),
                base_ref: Some("HEAD".to_string()),
                name: None,
                worktree_path: None,
                target_workspace_id: None,
                target_branch: None,
            },
        )
        .unwrap();
        let expected_head = created.head.unwrap();
        worktree_remove(
            &root,
            &GitWorktreeDiscardRequest {
                workspace_id,
                worktree_path: path.to_string_lossy().into_owned(),
                force: false,
                expected_head: Some(expected_head.clone()),
                preflight_revision: None,
            },
        )
        .unwrap();
        assert!(!path.exists());
        let restored = worktree_restore(&root, &path, branch, &expected_head).unwrap();
        assert!(same_path_identity(&restored.path, &path));
        assert_eq!(restored.branch.as_deref(), Some(branch));
        assert_eq!(restored.head.as_deref(), Some(expected_head.as_str()));

        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(path);
    }

    #[test]
    fn same_path_identity_matches_canonical_aliases() {
        let root = temp_repo("path-alias");
        let child = root.join("child");
        std::fs::create_dir_all(&child).unwrap();
        let aliased = child.join("..");

        assert!(same_path_identity(&root, &aliased));

        let _ = std::fs::remove_dir_all(root);
    }

    fn temp_repo(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-git-{label}-{}",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn prepare_conflict(
        root: &Path,
        base: Option<&[u8]>,
        source: Option<&[u8]>,
        target: Option<&[u8]>,
    ) -> (String, String, String, String) {
        std::fs::create_dir_all(root).unwrap();
        run_raw(root, &["init"]).unwrap();
        run_raw(root, &["config", "user.email", "vibex@example.invalid"]).unwrap();
        run_raw(root, &["config", "user.name", "Vibex Test"]).unwrap();
        std::fs::write(root.join("seed.txt"), b"seed\n").unwrap();
        if let Some(base) = base {
            std::fs::write(root.join("conflict.bin"), base).unwrap();
        }
        run_raw(root, &["add", "-A"]).unwrap();
        run_raw(root, &["commit", "-m", "base"]).unwrap();
        let target_branch = current_branch(root).unwrap().unwrap();
        let source_branch = format!("feature/{}", vibex_core::RequestId::new().as_str());
        run_raw(root, &["switch", "-c", &source_branch]).unwrap();
        match source {
            Some(content) => std::fs::write(root.join("conflict.bin"), content).unwrap(),
            None => {
                if root.join("conflict.bin").exists() {
                    std::fs::remove_file(root.join("conflict.bin")).unwrap();
                }
            }
        }
        run_raw(root, &["add", "-A"]).unwrap();
        run_raw(root, &["commit", "-m", "source"]).unwrap();
        let source_head = resolve_head(root).unwrap();
        run_raw(root, &["switch", &target_branch]).unwrap();
        match target {
            Some(content) => std::fs::write(root.join("conflict.bin"), content).unwrap(),
            None => {
                if root.join("conflict.bin").exists() {
                    std::fs::remove_file(root.join("conflict.bin")).unwrap();
                }
            }
        }
        run_raw(root, &["add", "-A"]).unwrap();
        run_raw(root, &["commit", "-m", "target"]).unwrap();
        let target_head = resolve_head(root).unwrap();
        (source_branch, source_head, target_branch, target_head)
    }

    fn run_raw(root: &Path, args: &[&str]) -> std::io::Result<()> {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()?;
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        Ok(())
    }

    fn init_repo_with_commit(root: &Path, file: &str, content: &str, message: &str) {
        run_raw(root, &["init"]).unwrap();
        run_raw(root, &["config", "user.email", "vibex@example.invalid"]).unwrap();
        run_raw(root, &["config", "user.name", "Vibex Test"]).unwrap();
        std::fs::write(root.join(file), content).unwrap();
        run_raw(root, &["add", file]).unwrap();
        run_raw(root, &["commit", "-m", message]).unwrap();
    }
}

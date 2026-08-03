use serde::{Deserialize, Serialize};

use crate::ids::{ProjectId, RequestId, VibexSessionId, WorkspaceId};
use crate::workspace::WorkspaceRecord;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitChangeKind {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    TypeChanged,
    Untracked,
    Unmerged,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitChange {
    pub path: String,
    pub original_path: Option<String>,
    pub kind: GitChangeKind,
    pub staged: bool,
    pub unstaged: bool,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStatusSummary {
    pub workspace_id: WorkspaceId,
    pub repo_path: String,
    pub branch: Option<String>,
    pub short_commit: Option<String>,
    pub detached: bool,
    pub dirty: bool,
    pub staged_count: u32,
    pub unstaged_count: u32,
    pub untracked_count: u32,
    pub changes: Vec<GitChange>,
    pub captured_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffRequest {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub staged: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitDiffResponse {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub staged: bool,
    pub diff: String,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitStageRequest {
    pub workspace_id: WorkspaceId,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitRequest {
    pub workspace_id: WorkspaceId,
    pub message: String,
    #[serde(default)]
    pub paths: Vec<String>,
    #[serde(default)]
    pub amend: bool,
    #[serde(default)]
    pub push_after: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitResult {
    pub workspace_id: WorkspaceId,
    pub short_commit: String,
    pub summary: String,
    pub committed_at_ms: i64,
    pub push_result: Option<GitRemoteActionResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSnapshot {
    pub workspace_id: WorkspaceId,
    pub branch: Option<String>,
    pub short_commit: Option<String>,
    pub dirty: bool,
    pub changed_files: u32,
    pub captured_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryRequest {
    pub workspace_id: WorkspaceId,
    pub limit: Option<u32>,
    pub before_commit: Option<String>,
    #[serde(default)]
    pub ref_name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitSummary {
    pub hash: String,
    pub short_hash: String,
    pub parents: Vec<String>,
    pub author_name: String,
    pub author_email: String,
    pub authored_at_ms: Option<i64>,
    pub subject: String,
    pub refs: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryAuthor {
    pub name: String,
    pub email: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryResponse {
    pub workspace_id: WorkspaceId,
    pub commits: Vec<GitCommitSummary>,
    pub has_more: bool,
    pub authors: Vec<GitHistoryAuthor>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDetailRequest {
    pub workspace_id: WorkspaceId,
    pub commit_hash: String,
    pub include_patch: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitFileChange {
    pub path: String,
    pub original_path: Option<String>,
    pub kind: GitChangeKind,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitCommitDetail {
    pub workspace_id: WorkspaceId,
    pub summary: GitCommitSummary,
    pub body: Option<String>,
    pub files: Vec<GitCommitFileChange>,
    pub patch: Option<String>,
    pub patch_truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlameRequest {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub start_line: Option<u32>,
    pub end_line: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlameLine {
    pub line_number: u32,
    pub commit_hash: String,
    pub short_hash: String,
    pub author_name: String,
    pub authored_at_ms: Option<i64>,
    pub summary: String,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBlameResponse {
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub lines: Vec<GitBlameLine>,
    pub truncated: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchSummary {
    pub name: String,
    pub current: bool,
    pub upstream: Option<String>,
    pub ahead: u32,
    pub behind: u32,
    pub detached: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemoteSummary {
    pub name: String,
    pub fetch_url: Option<String>,
    pub push_url: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchListResponse {
    pub workspace_id: WorkspaceId,
    pub branches: Vec<GitBranchSummary>,
    pub remotes: Vec<GitRemoteSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchCreateRequest {
    pub workspace_id: WorkspaceId,
    pub name: String,
    pub base_ref: Option<String>,
    pub checkout: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitBranchCheckoutRequest {
    pub workspace_id: WorkspaceId,
    pub name: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitRemoteActionKind {
    Fetch,
    Push,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemoteActionRequest {
    pub workspace_id: WorkspaceId,
    pub kind: GitRemoteActionKind,
    pub remote: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRemoteActionResult {
    pub workspace_id: WorkspaceId,
    pub kind: GitRemoteActionKind,
    pub summary: String,
    pub status_after: Option<GitStatusSummary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitManagedWorktreeStatus {
    Active,
    Archiving,
    Archived,
    Restoring,
    Discarding,
    Merged,
    Discarded,
    Failed,
    NeedsAttention,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeOperationKind {
    Create,
    Open,
    Archive,
    Restore,
    MergeBack,
    Discard,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeOperationStatus {
    Pending,
    Queued,
    Running,
    Completed,
    Failed,
    Recoverable,
    NeedsResolution,
    Aborting,
    NeedsAttention,
    Aborted,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitProjectEligibilityState {
    Probing,
    Eligible,
    Ineligible,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitProjectIneligibleReason {
    PathMissing,
    PathNotDirectory,
    GitUnavailable,
    NotWorkingTree,
    BareRepository,
    UnbornHead,
    BaseRefUnavailable,
    RepositoryIdentityUnavailable,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitPathIdentity {
    pub original_path: String,
    pub normalized_path: String,
    pub canonical_path: Option<String>,
    pub filesystem_id: Option<String>,
    pub comparison_key: String,
    pub exists: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitRepositoryIdentity {
    pub repository_root: GitPathIdentity,
    pub git_common_dir: GitPathIdentity,
    pub comparison_key: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitProjectEligibility {
    pub project_id: ProjectId,
    pub project_canonical_path: GitPathIdentity,
    pub state: GitProjectEligibilityState,
    pub repository_identity: Option<GitRepositoryIdentity>,
    pub current_branch: Option<String>,
    pub default_base_ref: Option<String>,
    pub selectable_base_refs: Vec<String>,
    pub observed_head: Option<String>,
    pub revision: String,
    pub disabled_reason: Option<GitProjectIneligibleReason>,
}

impl GitProjectEligibility {
    pub fn is_eligible(&self) -> bool {
        self.state == GitProjectEligibilityState::Eligible
            && self.repository_identity.is_some()
            && self.observed_head.is_some()
            && self.default_base_ref.is_some()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeLockKind {
    Repository,
    WorktreePath,
    WorkspaceIndex,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeLockKey {
    pub kind: GitWorktreeLockKind,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeOperationCheckpoint {
    IntentRecorded,
    LocksAcquired,
    GitAddStarted,
    GitAdded,
    WorkspacePersisted,
    ManagedRecordPersisted,
    DatabaseCommitted,
    MergeStarted,
    RebaseStarted,
    RebaseCompleted,
    FastForwardStarted,
    FastForwarded,
    ConflictDetected,
    ContinueStarted,
    AbortStarted,
    ArchiveStarted,
    WorktreeRemoved,
    RestoreStarted,
    WorktreeRestored,
    Completed,
    CompensationPending,
    Compensated,
    NeedsAttention,
    #[serde(other)]
    Unknown,
}

impl Default for GitWorktreeOperationCheckpoint {
    fn default() -> Self {
        Self::IntentRecorded
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeDiagnosticSeverity {
    Info,
    Warning,
    Error,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeDiagnostic {
    pub code: String,
    pub summary: String,
    pub severity: GitWorktreeDiagnosticSeverity,
    pub retryable: bool,
    pub recovery_action: Option<String>,
    pub operation_id: Option<RequestId>,
    pub worktree_id: Option<RequestId>,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeOperationDetail {
    pub schema_version: u16,
    pub idempotency_key: Option<String>,
    pub request_fingerprint: Option<String>,
    pub repository_identity: Option<GitRepositoryIdentity>,
    pub source_path_identity: Option<GitPathIdentity>,
    pub target_path_identity: Option<GitPathIdentity>,
    pub lock_keys: Vec<GitWorktreeLockKey>,
    pub origin_workspace_id: Option<WorkspaceId>,
    pub base_head: Option<String>,
    pub target_branch: Option<String>,
    pub expected_source_head: Option<String>,
    pub expected_target_head: Option<String>,
    pub preflight_revision: Option<String>,
    pub checkpoint: GitWorktreeOperationCheckpoint,
    pub attempt: u32,
    pub lease_owner: Option<String>,
    pub lease_expires_at_ms: Option<i64>,
    pub diagnostic: Option<GitWorktreeDiagnostic>,
    #[serde(default)]
    pub merge_strategy: Option<GitWorktreeMergeStrategy>,
    #[serde(default)]
    pub queue_key: Option<String>,
    #[serde(default)]
    pub queue_position: Option<u32>,
    #[serde(default)]
    pub conflicts: Vec<GitWorktreeConflictFile>,
    #[serde(default)]
    pub source_commits_after_start: u32,
    #[serde(default)]
    pub assistance_session_id: Option<VibexSessionId>,
}

impl Default for GitWorktreeOperationDetail {
    fn default() -> Self {
        Self {
            schema_version: 1,
            idempotency_key: None,
            request_fingerprint: None,
            repository_identity: None,
            source_path_identity: None,
            target_path_identity: None,
            lock_keys: Vec::new(),
            origin_workspace_id: None,
            base_head: None,
            target_branch: None,
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
            checkpoint: GitWorktreeOperationCheckpoint::IntentRecorded,
            attempt: 0,
            lease_owner: None,
            lease_expires_at_ms: None,
            diagnostic: None,
            merge_strategy: None,
            queue_key: None,
            queue_position: None,
            conflicts: Vec::new(),
            source_commits_after_start: 0,
            assistance_session_id: None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeMergeStrategy {
    NoFfMerge,
    RebaseAndMerge,
    #[serde(other)]
    Unknown,
}

impl Default for GitWorktreeMergeStrategy {
    fn default() -> Self {
        Self::NoFfMerge
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeConflictKind {
    BothModified,
    BothAdded,
    DeletedBySource,
    DeletedByTarget,
    Binary,
    Other,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeConflictFile {
    pub path: String,
    pub kind: GitWorktreeConflictKind,
    pub binary: bool,
    pub resolved: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeReadinessState {
    #[default]
    Working,
    Reviewing,
    ReadyToMerge,
    MergeQueued,
    MergeRunning,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeCheckOutcome {
    Passed,
    Failed,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeCheckRecord {
    pub command: String,
    pub outcome: GitWorktreeCheckOutcome,
    pub recorded_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeReadinessRecord {
    pub worktree_id: RequestId,
    pub workspace_id: WorkspaceId,
    pub state: GitWorktreeReadinessState,
    pub source_head: String,
    pub dirty_fingerprint: String,
    pub target_workspace_id: WorkspaceId,
    pub target_branch: String,
    #[serde(default)]
    pub checks: Vec<GitWorktreeCheckRecord>,
    pub revision: String,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeReconciliationState {
    Unverified,
    Consistent,
    Recoverable,
    NeedsAttention,
    #[serde(other)]
    Unknown,
}

impl Default for GitWorktreeReconciliationState {
    fn default() -> Self {
        Self::Unverified
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitManagedWorktreeRecord {
    pub worktree_id: RequestId,
    pub project_id: ProjectId,
    pub workspace_id: Option<WorkspaceId>,
    pub repo_root: String,
    pub worktree_path: String,
    pub repository_identity: Option<GitRepositoryIdentity>,
    pub worktree_path_identity: Option<GitPathIdentity>,
    pub branch: Option<String>,
    pub origin_workspace_id: Option<WorkspaceId>,
    pub base_ref: Option<String>,
    pub base_head: Option<String>,
    pub target_workspace_id: Option<WorkspaceId>,
    pub target_branch: Option<String>,
    pub head: Option<String>,
    pub status: GitManagedWorktreeStatus,
    pub reconciliation_state: GitWorktreeReconciliationState,
    pub diagnostic: Option<GitWorktreeDiagnostic>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub closed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeSummary {
    pub path: String,
    #[serde(default)]
    pub path_identity: Option<GitPathIdentity>,
    #[serde(default)]
    pub repository_identity: Option<GitRepositoryIdentity>,
    pub branch: Option<String>,
    pub head: Option<String>,
    pub detached: bool,
    pub bare: bool,
    pub prunable: bool,
    pub workspace_id: Option<WorkspaceId>,
    pub managed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeListResponse {
    pub workspace_id: WorkspaceId,
    pub worktrees: Vec<GitWorktreeSummary>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeCreateRequest {
    pub workspace_id: WorkspaceId,
    pub branch_name: String,
    pub base_ref: Option<String>,
    pub name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_path: Option<String>,
    #[serde(default)]
    pub target_workspace_id: Option<WorkspaceId>,
    #[serde(default)]
    pub target_branch: Option<String>,
}

pub fn managed_worktree_name_slug(value: &str) -> String {
    let slug = value
        .chars()
        .map(|character| {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                character
            } else {
                '-'
            }
        })
        .collect::<String>();
    let slug = slug
        .split('-')
        .filter(|segment| !segment.is_empty())
        .collect::<Vec<_>>()
        .join("-")
        .trim_matches(['-', '_'])
        .to_string();
    if slug.is_empty() {
        "worktree".to_string()
    } else {
        slug
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeCreateResult {
    pub worktree: GitWorktreeSummary,
    pub workspace: WorkspaceRecord,
    pub managed: GitManagedWorktreeRecord,
    pub operation: GitWorktreeOperationRecord,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeMergeRequest {
    pub workspace_id: WorkspaceId,
    pub source_path: String,
    pub target_workspace_id: Option<WorkspaceId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<GitWorktreeMergeStrategy>,
    #[serde(default)]
    pub expected_source_head: Option<String>,
    #[serde(default)]
    pub expected_target_head: Option<String>,
    #[serde(default)]
    pub preflight_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeReadinessRequest {
    pub workspace_id: WorkspaceId,
    pub state: GitWorktreeReadinessState,
    #[serde(default)]
    pub expected_source_head: Option<String>,
    #[serde(default)]
    pub expected_dirty_fingerprint: Option<String>,
    #[serde(default)]
    pub checks: Vec<GitWorktreeCheckRecord>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeOperationRequest {
    pub operation_id: RequestId,
    pub workspace_id: WorkspaceId,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeAssistanceSessionRequest {
    pub operation_id: RequestId,
    pub workspace_id: WorkspaceId,
    pub session_id: VibexSessionId,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeConflictVersion {
    Target,
    Source,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeConflictResolveRequest {
    pub operation_id: RequestId,
    pub workspace_id: WorkspaceId,
    pub path: String,
    pub version: GitWorktreeConflictVersion,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeConflictStageRequest {
    pub operation_id: RequestId,
    pub workspace_id: WorkspaceId,
    pub paths: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeArchiveRequest {
    pub workspace_id: WorkspaceId,
    pub worktree_path: String,
    #[serde(default)]
    pub expected_head: Option<String>,
    #[serde(default)]
    pub preflight_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeRestoreRequest {
    pub workspace_id: WorkspaceId,
    pub worktree_id: RequestId,
    #[serde(default)]
    pub preflight_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeDiscardRequest {
    pub workspace_id: WorkspaceId,
    pub worktree_path: String,
    pub force: bool,
    #[serde(default)]
    pub expected_head: Option<String>,
    #[serde(default)]
    pub preflight_revision: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeOperationRecord {
    pub operation_id: RequestId,
    pub project_id: ProjectId,
    pub source_workspace_id: Option<WorkspaceId>,
    pub target_workspace_id: Option<WorkspaceId>,
    pub operation: GitWorktreeOperationKind,
    pub status: GitWorktreeOperationStatus,
    pub worktree_path: Option<String>,
    pub branch: Option<String>,
    pub base_ref: Option<String>,
    pub head_before: Option<String>,
    pub head_after: Option<String>,
    pub error: Option<String>,
    #[serde(default)]
    pub detail: GitWorktreeOperationDetail,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeDestructiveAction {
    MergeBack,
    Discard,
    Archive,
    Restore,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorktreeRiskKind {
    DirtySource,
    DirtyTarget,
    SourceHeadChanged,
    TargetHeadChanged,
    OwnershipMismatch,
    ActiveOperation,
    MissingGitRegistration,
    StaleReadiness,
    WrongTargetBranch,
    ActiveGitOperation,
    UnpushedCommits,
    RunningConsumers,
    PathConflict,
    UnknownState,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeRisk {
    pub kind: GitWorktreeRiskKind,
    pub blocking: bool,
    pub summary: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeDestructivePreflight {
    pub action: GitWorktreeDestructiveAction,
    pub allowed: bool,
    pub revision: String,
    pub source_head: Option<String>,
    pub target_head: Option<String>,
    pub risks: Vec<GitWorktreeRisk>,
    #[serde(default)]
    pub action_label: String,
    pub observed_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeChangeSummary {
    pub commit_count: u32,
    pub file_count: u32,
    pub additions: u32,
    pub deletions: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeRunningConsumers {
    pub agent_count: u32,
    pub terminal_count: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeMergePlan {
    pub plan_id: RequestId,
    pub source_workspace_id: WorkspaceId,
    pub source_path: String,
    pub source_branch: String,
    pub source_head: String,
    pub target_workspace_id: WorkspaceId,
    pub target_path: String,
    pub target_branch: String,
    pub target_head: String,
    pub summary: GitWorktreeChangeSummary,
    pub readiness: GitWorktreeReadinessRecord,
    pub running_consumers: GitWorktreeRunningConsumers,
    pub preflight: GitWorktreeDestructivePreflight,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeLifecycleSnapshot {
    pub workspace_id: WorkspaceId,
    pub eligibility: GitProjectEligibility,
    pub managed_worktrees: Vec<GitManagedWorktreeRecord>,
    pub operations: Vec<GitWorktreeOperationRecord>,
    #[serde(default)]
    pub readiness: Vec<GitWorktreeReadinessRecord>,
    pub diagnostics: Vec<GitWorktreeDiagnostic>,
    pub revision: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct GitWorktreeReconcileReport {
    pub inspected_operations: u32,
    pub inspected_worktrees: u32,
    pub completed_operations: u32,
    pub failed_operations: u32,
    pub recoverable_operations: u32,
    pub needs_attention: u32,
    pub diagnostics: Vec<GitWorktreeDiagnostic>,
}

#[cfg(test)]
mod worktree_contract_tests {
    use super::*;

    #[test]
    fn managed_worktree_slug_is_valid_for_a_git_branch_component() {
        assert_eq!(
            managed_worktree_name_slug("  feature..lock  "),
            "feature-lock"
        );
        assert_eq!(managed_worktree_name_slug("___"), "worktree");
        assert_eq!(managed_worktree_name_slug("中文 task"), "task");
    }

    #[test]
    fn absent_custom_worktree_path_preserves_the_legacy_request_shape() {
        let mut request = GitWorktreeCreateRequest {
            workspace_id: WorkspaceId::new(),
            branch_name: "feature/worktree".to_string(),
            base_ref: Some("main".to_string()),
            name: Some("worktree".to_string()),
            worktree_path: None,
            target_workspace_id: None,
            target_branch: None,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert!(encoded.get("worktreePath").is_none());

        request.worktree_path = Some("/tmp/worktree".to_string());
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["worktreePath"], "/tmp/worktree");
    }

    #[test]
    fn absent_merge_strategy_preserves_the_legacy_request_shape() {
        let mut request = GitWorktreeMergeRequest {
            workspace_id: WorkspaceId::new(),
            source_path: "/tmp/worktree".to_string(),
            target_workspace_id: None,
            strategy: None,
            expected_source_head: None,
            expected_target_head: None,
            preflight_revision: None,
        };
        let encoded = serde_json::to_value(&request).unwrap();
        assert!(encoded.get("strategy").is_none());

        request.strategy = Some(GitWorktreeMergeStrategy::RebaseAndMerge);
        let encoded = serde_json::to_value(&request).unwrap();
        assert_eq!(encoded["strategy"], "rebase_and_merge");
    }

    #[test]
    fn managed_identity_and_fixed_target_round_trip() {
        let origin_workspace_id = WorkspaceId::new();
        let target_workspace_id = WorkspaceId::new();
        let record = GitManagedWorktreeRecord {
            worktree_id: RequestId::new(),
            project_id: ProjectId::new(),
            workspace_id: Some(WorkspaceId::new()),
            repo_root: "/repo".to_string(),
            worktree_path: "/worktrees/feature".to_string(),
            repository_identity: None,
            worktree_path_identity: None,
            branch: Some("feature/demo".to_string()),
            origin_workspace_id: Some(origin_workspace_id.clone()),
            base_ref: Some("main".to_string()),
            base_head: Some("a".repeat(40)),
            target_workspace_id: Some(target_workspace_id.clone()),
            target_branch: Some("main".to_string()),
            head: Some("a".repeat(40)),
            status: GitManagedWorktreeStatus::Active,
            reconciliation_state: GitWorktreeReconciliationState::Consistent,
            diagnostic: None,
            created_at_ms: 1,
            updated_at_ms: 1,
            closed_at_ms: None,
        };

        let encoded = serde_json::to_string(&record).unwrap();
        let decoded: GitManagedWorktreeRecord = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, record);
        assert_eq!(decoded.origin_workspace_id, Some(origin_workspace_id));
        assert_eq!(decoded.target_workspace_id, Some(target_workspace_id));
    }

    #[test]
    fn unknown_lifecycle_states_fail_closed() {
        let status: GitWorktreeOperationStatus = serde_json::from_str("\"future_state\"").unwrap();
        let managed: GitManagedWorktreeStatus = serde_json::from_str("\"future_state\"").unwrap();
        let checkpoint: GitWorktreeOperationCheckpoint =
            serde_json::from_str("\"future_checkpoint\"").unwrap();
        let lock_kind: GitWorktreeLockKind = serde_json::from_str("\"future_lock_kind\"").unwrap();
        let strategy: GitWorktreeMergeStrategy =
            serde_json::from_str("\"future_strategy\"").unwrap();
        assert_eq!(status, GitWorktreeOperationStatus::Unknown);
        assert_eq!(managed, GitManagedWorktreeStatus::Unknown);
        assert_eq!(checkpoint, GitWorktreeOperationCheckpoint::Unknown);
        assert_eq!(lock_kind, GitWorktreeLockKind::Unknown);
        assert_eq!(strategy, GitWorktreeMergeStrategy::Unknown);
    }

    #[test]
    fn legacy_operation_without_detail_remains_readable() {
        let value = serde_json::json!({
            "operationId": RequestId::new(),
            "projectId": ProjectId::new(),
            "sourceWorkspaceId": null,
            "targetWorkspaceId": null,
            "operation": "create",
            "status": "pending",
            "worktreePath": null,
            "branch": "feature/demo",
            "baseRef": "main",
            "headBefore": null,
            "headAfter": null,
            "error": null,
            "createdAtMs": 1,
            "updatedAtMs": 1
        });
        let decoded: GitWorktreeOperationRecord = serde_json::from_value(value).unwrap();
        assert_eq!(
            decoded.detail.checkpoint,
            GitWorktreeOperationCheckpoint::IntentRecorded
        );
    }

    #[test]
    fn readiness_and_conflict_detail_round_trip_without_changing_legacy_defaults() {
        let readiness = GitWorktreeReadinessRecord {
            worktree_id: RequestId::new(),
            workspace_id: WorkspaceId::new(),
            state: GitWorktreeReadinessState::ReadyToMerge,
            source_head: "a".repeat(40),
            dirty_fingerprint: "clean-v1".to_string(),
            target_workspace_id: WorkspaceId::new(),
            target_branch: "main".to_string(),
            checks: vec![GitWorktreeCheckRecord {
                command: "cargo test -p vibex-git --locked".to_string(),
                outcome: GitWorktreeCheckOutcome::Passed,
                recorded_at_ms: 1,
            }],
            revision: "ready-v1".to_string(),
            updated_at_ms: 1,
        };
        let encoded = serde_json::to_string(&readiness).unwrap();
        assert_eq!(
            serde_json::from_str::<GitWorktreeReadinessRecord>(&encoded).unwrap(),
            readiness
        );

        let detail: GitWorktreeOperationDetail = serde_json::from_value(serde_json::json!({
            "schemaVersion": 1,
            "checkpoint": "conflict_detected",
            "conflicts": [{
                "path": "src/lib.rs",
                "kind": "both_modified",
                "binary": false,
                "resolved": false
            }]
        }))
        .unwrap();
        assert_eq!(detail.conflicts.len(), 1);
        assert_eq!(detail.merge_strategy, None);
        assert_eq!(detail.source_commits_after_start, 0);
        assert_eq!(detail.assistance_session_id, None);

        let assistance_session_id = VibexSessionId::new();
        let associated = GitWorktreeOperationDetail {
            assistance_session_id: Some(assistance_session_id.clone()),
            ..detail
        };
        let encoded = serde_json::to_string(&associated).unwrap();
        assert_eq!(
            serde_json::from_str::<GitWorktreeOperationDetail>(&encoded)
                .unwrap()
                .assistance_session_id,
            Some(assistance_session_id)
        );
    }
}

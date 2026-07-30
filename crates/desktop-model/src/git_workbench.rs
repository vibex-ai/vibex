use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use sha2::{Digest as _, Sha256};
use vibex_core::{
    GitBlameResponse, GitBranchListResponse, GitChange, GitCommitDetail, GitHistoryAuthor,
    GitHistoryResponse, GitStatusSummary, GitWorktreeListResponse, WorkspaceId,
};

use crate::{PreparedDiffRow, UnifiedDiffFile, VirtualDiffRows, parse_unified_diff};

pub const GIT_HISTORY_MAX_ROWS: usize = 10_000;
pub const GIT_CHANGE_MAX_ROWS: usize = 100_000;
pub const GIT_DIFF_CACHE_ITEM_LIMIT: usize = 32;
pub const GIT_DIFF_CACHE_ROW_LIMIT: usize = 200_000;
pub const GIT_DIFF_CACHE_BYTE_LIMIT: usize = 64 * 1024 * 1024;
pub const GIT_COMMIT_CACHE_ITEM_LIMIT: usize = 32;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitQueryKind {
    Status,
    History,
    Diff,
    CommitDetail,
    Blame,
    Branches,
    Worktrees,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitQueryTicket {
    pub workspace_id: WorkspaceId,
    pub workspace_generation: u64,
    pub revision_epoch: u64,
    pub query_generation: u64,
    pub kind: GitQueryKind,
    pub key: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitWorkbenchMode {
    Changes,
    History,
}

impl Default for GitWorkbenchMode {
    fn default() -> Self {
        Self::Changes
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitPathSelectionState {
    Checked,
    Unchecked,
    Indeterminate,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GitTreeRowKind {
    Directory,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeSegment {
    pub name: String,
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GitTreeRow {
    pub id: String,
    pub kind: GitTreeRowKind,
    pub path: String,
    pub segments: Vec<GitTreeSegment>,
    pub depth: usize,
    pub expanded: bool,
    pub change_index: Option<usize>,
}

#[derive(Debug, Clone, Default)]
struct GitTreeNode {
    name: String,
    path: String,
    change_index: Option<usize>,
    children: BTreeMap<String, GitTreeNode>,
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitSelectionKey {
    pub path: String,
    pub staged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GitMutationKind {
    Stage,
    Unstage,
    Revert,
    Commit,
    Amend,
    Fetch,
    Push,
    BranchCreate,
    BranchCheckout,
    WorktreeCreate,
    WorktreeMerge,
    WorktreeDiscard,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitMutationScope {
    pub operation_id: String,
    pub kind: GitMutationKind,
    pub paths: Vec<String>,
    pub target: Option<String>,
    pub destructive: bool,
    pub confirmation_label: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitHistoryFilter {
    pub ref_name: Option<String>,
    pub author: Option<String>,
}

impl Default for GitHistoryFilter {
    fn default() -> Self {
        Self {
            ref_name: None,
            author: None,
        }
    }
}

#[derive(Debug, Clone)]
pub struct GitDiffDocument {
    pub path: String,
    pub staged: bool,
    pub revision: String,
    pub truncated: bool,
    pub files: Vec<UnifiedDiffFile>,
    pub rows: VirtualDiffRows,
}

impl GitDiffDocument {
    pub fn new(
        path: impl Into<String>,
        staged: bool,
        revision: impl Into<String>,
        diff: &str,
        truncated: bool,
    ) -> Self {
        let path = normalize_path(&path.into());
        let revision = revision.into();
        let files = parse_unified_diff(diff);
        let rows = VirtualDiffRows::new(revision.clone(), &files);
        Self {
            path,
            staged,
            revision,
            truncated,
            files,
            rows,
        }
    }

    pub fn row_count(&self) -> usize {
        self.rows.len()
    }

    pub fn estimated_bytes(&self) -> usize {
        self.path
            .len()
            .saturating_add(self.revision.len())
            .saturating_add(
                self.files
                    .iter()
                    .map(|file| {
                        file.header
                            .iter()
                            .map(String::len)
                            .sum::<usize>()
                            .saturating_add(
                                file.lines
                                    .iter()
                                    .map(|line| line.content.len())
                                    .sum::<usize>(),
                            )
                    })
                    .sum::<usize>(),
            )
            .saturating_add(self.rows.estimated_bytes())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum GitCommitPatchRowRef {
    FileHeader { file_index: usize },
    Diff { row_index: usize },
    Empty { file_index: usize },
}

#[derive(Debug, Clone)]
pub enum GitCommitPatchRow {
    FileHeader {
        file_index: usize,
        path: String,
        original_path: Option<String>,
        additions: u32,
        deletions: u32,
        collapsed: bool,
    },
    Diff(PreparedDiffRow),
    Empty {
        file_index: usize,
        path: String,
    },
}

#[derive(Debug, Clone)]
pub struct GitCommitDocument {
    pub detail: GitCommitDetail,
    pub patch: Option<GitDiffDocument>,
    collapsed_files: BTreeSet<String>,
    file_row_ranges: Vec<std::ops::Range<usize>>,
    visible_rows: Vec<GitCommitPatchRowRef>,
}

impl GitCommitDocument {
    fn new(detail: GitCommitDetail, collapsed_files: BTreeSet<String>) -> Self {
        let patch = detail.patch.as_deref().map(|patch| {
            let content_hash = format!("sha256:{:x}", Sha256::digest(patch.as_bytes()));
            GitDiffDocument::new(
                format!("commit:{}", detail.summary.hash),
                true,
                format!("{}:{content_hash}", detail.summary.hash),
                patch,
                detail.patch_truncated,
            )
        });
        let mut document = Self {
            detail,
            patch,
            collapsed_files,
            file_row_ranges: Vec::new(),
            visible_rows: Vec::new(),
        };
        document.rebuild_visible_rows();
        document
    }

    pub fn has_patch(&self) -> bool {
        self.patch.is_some()
    }

    pub fn row_count(&self) -> usize {
        self.visible_rows.len()
    }

    pub fn visible_window(&mut self, start: usize, length: usize) -> Vec<GitCommitPatchRow> {
        let start = start.min(self.visible_rows.len());
        let end = start.saturating_add(length).min(self.visible_rows.len());
        let refs = self.visible_rows[start..end].to_vec();
        refs.into_iter()
            .filter_map(|row| match row {
                GitCommitPatchRowRef::FileHeader { file_index } => {
                    let file = self.patch.as_ref()?.files.get(file_index)?;
                    let path = file.display_path().to_string();
                    let stat =
                        self.detail.files.iter().find(|candidate| {
                            normalize_path(&candidate.path) == normalize_path(&path)
                        });
                    let original_path =
                        stat.and_then(|stat| stat.original_path.clone())
                            .or_else(|| {
                                file.old_path
                                    .as_ref()
                                    .filter(|old_path| {
                                        normalize_path(old_path) != normalize_path(&path)
                                    })
                                    .cloned()
                            });
                    Some(GitCommitPatchRow::FileHeader {
                        file_index,
                        path: path.clone(),
                        original_path,
                        additions: stat.map(|stat| stat.additions).unwrap_or_default(),
                        deletions: stat.map(|stat| stat.deletions).unwrap_or_default(),
                        collapsed: self.collapsed_files.contains(&path),
                    })
                }
                GitCommitPatchRowRef::Diff { row_index } => self
                    .patch
                    .as_mut()?
                    .rows
                    .prepared_row(row_index)
                    .map(GitCommitPatchRow::Diff),
                GitCommitPatchRowRef::Empty { file_index } => {
                    let file = self.patch.as_ref()?.files.get(file_index)?;
                    Some(GitCommitPatchRow::Empty {
                        file_index,
                        path: file.display_path().to_string(),
                    })
                }
            })
            .collect()
    }

    pub fn toggle_file(&mut self, path: &str) -> bool {
        let Some(path) = self.matching_file_path(path) else {
            return false;
        };
        if !self.collapsed_files.remove(&path) {
            self.collapsed_files.insert(path);
        }
        self.rebuild_visible_rows();
        true
    }

    pub fn focus_file(&mut self, path: &str) -> Option<usize> {
        let path = self.matching_file_path(path)?;
        if self.collapsed_files.remove(&path) {
            self.rebuild_visible_rows();
        }
        self.visible_rows.iter().position(|row| {
            let GitCommitPatchRowRef::FileHeader { file_index } = row else {
                return false;
            };
            self.patch
                .as_ref()
                .and_then(|patch| patch.files.get(*file_index))
                .is_some_and(|file| file.display_path() == path)
        })
    }

    pub fn estimated_bytes(&self) -> usize {
        let detail_bytes = self
            .detail
            .summary
            .subject
            .len()
            .saturating_add(
                self.detail
                    .body
                    .as_deref()
                    .map(str::len)
                    .unwrap_or_default(),
            )
            .saturating_add(
                self.detail
                    .files
                    .iter()
                    .map(|file| {
                        file.path.len().saturating_add(
                            file.original_path.as_deref().map(str::len).unwrap_or(0),
                        )
                    })
                    .sum::<usize>(),
            );
        detail_bytes.saturating_add(
            self.patch
                .as_ref()
                .map(GitDiffDocument::estimated_bytes)
                .unwrap_or_default(),
        )
    }

    fn matching_file_path(&self, path: &str) -> Option<String> {
        let path = normalize_path(path);
        self.patch.as_ref()?.files.iter().find_map(|file| {
            let display_path = file.display_path();
            [
                Some(display_path),
                file.new_path.as_deref(),
                file.old_path.as_deref(),
            ]
            .into_iter()
            .flatten()
            .any(|candidate| normalize_path(candidate) == path)
            .then(|| display_path.to_string())
        })
    }

    fn rebuild_visible_rows(&mut self) {
        self.file_row_ranges.clear();
        self.visible_rows.clear();
        let Some(patch) = self.patch.as_ref() else {
            return;
        };
        self.file_row_ranges = vec![0..0; patch.files.len()];
        for row_index in 0..patch.rows.len() {
            let Some(row) = patch.rows.row(row_index) else {
                continue;
            };
            let Some(range) = self.file_row_ranges.get_mut(row.file_index) else {
                continue;
            };
            if range.start == range.end {
                range.start = row_index;
            }
            range.end = row_index.saturating_add(1);
        }
        for (file_index, file) in patch.files.iter().enumerate() {
            self.visible_rows
                .push(GitCommitPatchRowRef::FileHeader { file_index });
            if self.collapsed_files.contains(file.display_path()) {
                continue;
            }
            let Some(range) = self.file_row_ranges.get(file_index) else {
                continue;
            };
            if range.is_empty() {
                self.visible_rows
                    .push(GitCommitPatchRowRef::Empty { file_index });
            } else {
                self.visible_rows.extend(
                    range
                        .clone()
                        .map(|row_index| GitCommitPatchRowRef::Diff { row_index }),
                );
            }
        }
    }
}

#[derive(Debug, Clone, Default)]
pub struct GitWorkbenchState {
    workspace_id: Option<WorkspaceId>,
    workspace_generation: u64,
    revision_epoch: u64,
    next_query_generation: u64,
    active_queries: BTreeMap<(GitQueryKind, String), u64>,
    pub mode: GitWorkbenchMode,
    pub status: Option<GitStatusSummary>,
    pub selected_changes: BTreeSet<GitSelectionKey>,
    change_selection_seeded: bool,
    change_row_index: Vec<(usize, bool)>,
    change_tree_root: GitTreeNode,
    change_tree_rows: Vec<GitTreeRow>,
    change_directory_paths: BTreeSet<String>,
    expanded_change_directories: BTreeSet<String>,
    pub history: Vec<vibex_core::GitCommitSummary>,
    pub history_has_more: bool,
    pub history_authors: Vec<GitHistoryAuthor>,
    pub history_filter: GitHistoryFilter,
    pub selected_commit_hash: Option<String>,
    pub commit_detail: Option<GitCommitDetail>,
    pub commit_documents: BTreeMap<String, GitCommitDocument>,
    commit_document_epochs: BTreeMap<String, u64>,
    commit_cache_epoch: u64,
    commit_tree_changes: Vec<GitChange>,
    commit_tree_root: GitTreeNode,
    commit_tree_rows: Vec<GitTreeRow>,
    commit_directory_paths: BTreeSet<String>,
    expanded_commit_directories: BTreeSet<String>,
    pub blame: Option<GitBlameResponse>,
    pub branches: Option<GitBranchListResponse>,
    pub worktrees: Option<GitWorktreeListResponse>,
    pub diffs: BTreeMap<GitSelectionKey, GitDiffDocument>,
    diff_epochs: BTreeMap<GitSelectionKey, u64>,
    diff_cache_epoch: u64,
    pub pending_mutation: Option<GitMutationScope>,
    pub last_error_code: Option<String>,
}

impl GitWorkbenchState {
    pub fn workspace_id(&self) -> Option<&WorkspaceId> {
        self.workspace_id.as_ref()
    }

    pub fn workspace_generation(&self) -> u64 {
        self.workspace_generation
    }

    pub fn revision_epoch(&self) -> u64 {
        self.revision_epoch
    }

    pub fn reset_workspace(&mut self, workspace_id: WorkspaceId) {
        self.workspace_id = Some(workspace_id);
        self.workspace_generation = self.workspace_generation.saturating_add(1).max(1);
        self.revision_epoch = 1;
        self.next_query_generation = 0;
        self.active_queries.clear();
        self.status = None;
        self.selected_changes.clear();
        self.change_selection_seeded = false;
        self.change_row_index.clear();
        self.change_tree_root = GitTreeNode::default();
        self.change_tree_rows.clear();
        self.change_directory_paths.clear();
        self.expanded_change_directories.clear();
        self.history.clear();
        self.history_has_more = false;
        self.history_authors.clear();
        self.selected_commit_hash = None;
        self.commit_detail = None;
        self.commit_documents.clear();
        self.commit_document_epochs.clear();
        self.commit_cache_epoch = 0;
        self.commit_tree_changes.clear();
        self.commit_tree_root = GitTreeNode::default();
        self.commit_tree_rows.clear();
        self.commit_directory_paths.clear();
        self.expanded_commit_directories.clear();
        self.blame = None;
        self.branches = None;
        self.worktrees = None;
        self.diffs.clear();
        self.diff_epochs.clear();
        self.diff_cache_epoch = 0;
        self.pending_mutation = None;
        self.last_error_code = None;
    }

    pub fn begin_query(
        &mut self,
        kind: GitQueryKind,
        key: impl Into<String>,
    ) -> Option<GitQueryTicket> {
        let workspace_id = self.workspace_id.clone()?;
        let key = bounded_text(&key.into(), 1_024);
        self.next_query_generation = self.next_query_generation.saturating_add(1).max(1);
        let query_generation = self.next_query_generation;
        self.active_queries
            .insert((kind, key.clone()), query_generation);
        Some(GitQueryTicket {
            workspace_id,
            workspace_generation: self.workspace_generation,
            revision_epoch: self.revision_epoch,
            query_generation,
            kind,
            key,
        })
    }

    pub fn accept_ticket(&self, ticket: &GitQueryTicket) -> bool {
        self.workspace_id.as_ref() == Some(&ticket.workspace_id)
            && self.workspace_generation == ticket.workspace_generation
            && self.revision_epoch == ticket.revision_epoch
            && self
                .active_queries
                .get(&(ticket.kind, ticket.key.clone()))
                .is_some_and(|generation| *generation == ticket.query_generation)
    }

    pub fn apply_status(&mut self, ticket: &GitQueryTicket, status: GitStatusSummary) -> bool {
        if ticket.kind != GitQueryKind::Status
            || !self.accept_ticket(ticket)
            || status.workspace_id != ticket.workspace_id
        {
            return false;
        }
        let selected_paths = self.selected_path_set();
        self.status = Some(status);
        self.rebuild_change_row_index();
        self.rebuild_change_tree();
        self.reconcile_path_selection(selected_paths);
        self.last_error_code = None;
        true
    }

    pub fn apply_history(
        &mut self,
        ticket: &GitQueryTicket,
        response: GitHistoryResponse,
        append: bool,
    ) -> bool {
        if ticket.kind != GitQueryKind::History
            || !self.accept_ticket(ticket)
            || response.workspace_id != ticket.workspace_id
        {
            return false;
        }
        if !append {
            self.history.clear();
        }
        let mut seen = self
            .history
            .iter()
            .map(|commit| commit.hash.clone())
            .collect::<BTreeSet<_>>();
        self.history.extend(
            response
                .commits
                .into_iter()
                .filter(|commit| seen.insert(commit.hash.clone()))
                .take(GIT_HISTORY_MAX_ROWS.saturating_sub(self.history.len())),
        );
        self.history_has_more = response.has_more;
        self.history_authors = response.authors;
        self.last_error_code = None;
        true
    }

    pub fn apply_diff(
        &mut self,
        ticket: &GitQueryTicket,
        response: vibex_core::GitDiffResponse,
    ) -> bool {
        if ticket.kind != GitQueryKind::Diff
            || !self.accept_ticket(ticket)
            || response.workspace_id != ticket.workspace_id
        {
            return false;
        }
        let key = GitSelectionKey {
            path: normalize_path(&response.path),
            staged: response.staged,
        };
        let content_hash = format!("sha256:{:x}", Sha256::digest(response.diff.as_bytes()));
        let revision = format!(
            "{}:{}:{}:{}",
            self.revision_epoch, response.staged, response.path, content_hash
        );
        self.diff_cache_epoch = self.diff_cache_epoch.saturating_add(1).max(1);
        self.diff_epochs.insert(key.clone(), self.diff_cache_epoch);
        self.diffs.insert(
            key,
            GitDiffDocument::new(
                response.path,
                response.staged,
                revision,
                &response.diff,
                response.truncated,
            ),
        );
        self.enforce_diff_cache_budget();
        self.last_error_code = None;
        true
    }

    pub fn diff_mut(&mut self, key: &GitSelectionKey) -> Option<&mut GitDiffDocument> {
        if !self.diffs.contains_key(key) {
            return None;
        }
        self.diff_cache_epoch = self.diff_cache_epoch.saturating_add(1).max(1);
        self.diff_epochs.insert(key.clone(), self.diff_cache_epoch);
        self.diffs.get_mut(key)
    }

    pub fn apply_commit_detail(
        &mut self,
        ticket: &GitQueryTicket,
        detail: GitCommitDetail,
    ) -> bool {
        if ticket.kind != GitQueryKind::CommitDetail
            || !self.accept_ticket(ticket)
            || detail.workspace_id != ticket.workspace_id
            || detail.summary.hash != ticket.key
        {
            return false;
        }
        let hash = detail.summary.hash.clone();
        let previous = self.commit_documents.get(&hash).cloned();
        let document = if detail.patch.is_none()
            && previous.as_ref().is_some_and(GitCommitDocument::has_patch)
        {
            previous.expect("patch-bearing document checked above")
        } else {
            let collapsed_files = previous
                .map(|document| document.collapsed_files)
                .unwrap_or_default();
            GitCommitDocument::new(detail, collapsed_files)
        };
        self.commit_cache_epoch = self.commit_cache_epoch.saturating_add(1).max(1);
        self.commit_document_epochs
            .insert(hash.clone(), self.commit_cache_epoch);
        self.commit_documents.insert(hash.clone(), document);
        self.enforce_commit_cache_budget();
        if self.selected_commit_hash.as_deref() == Some(hash.as_str()) {
            self.sync_selected_commit_detail();
        }
        self.last_error_code = None;
        true
    }

    pub fn commit_detail_for(&self, hash: &str) -> Option<&GitCommitDetail> {
        self.commit_documents
            .get(hash)
            .map(|document| &document.detail)
    }

    pub fn commit_patch_ready(&self, hash: &str) -> bool {
        self.commit_documents
            .get(hash)
            .is_some_and(GitCommitDocument::has_patch)
    }

    pub fn commit_preview_row_count(&self, hash: &str) -> usize {
        self.commit_documents
            .get(hash)
            .map(GitCommitDocument::row_count)
            .unwrap_or_default()
    }

    pub fn commit_preview_window(
        &mut self,
        hash: &str,
        start: usize,
        length: usize,
    ) -> Vec<GitCommitPatchRow> {
        self.commit_documents
            .get_mut(hash)
            .map(|document| document.visible_window(start, length))
            .unwrap_or_default()
    }

    pub fn toggle_commit_file(&mut self, hash: &str, path: &str) -> bool {
        self.commit_documents
            .get_mut(hash)
            .is_some_and(|document| document.toggle_file(path))
    }

    pub fn focus_commit_file(&mut self, hash: &str, path: &str) -> Option<usize> {
        self.commit_documents
            .get_mut(hash)
            .and_then(|document| document.focus_file(path))
    }

    pub fn apply_blame(&mut self, ticket: &GitQueryTicket, blame: GitBlameResponse) -> bool {
        if ticket.kind != GitQueryKind::Blame
            || !self.accept_ticket(ticket)
            || blame.workspace_id != ticket.workspace_id
        {
            return false;
        }
        self.blame = Some(blame);
        self.last_error_code = None;
        true
    }

    pub fn apply_branches(
        &mut self,
        ticket: &GitQueryTicket,
        branches: GitBranchListResponse,
    ) -> bool {
        if ticket.kind != GitQueryKind::Branches
            || !self.accept_ticket(ticket)
            || branches.workspace_id != ticket.workspace_id
        {
            return false;
        }
        self.branches = Some(branches);
        self.last_error_code = None;
        true
    }

    pub fn apply_worktrees(
        &mut self,
        ticket: &GitQueryTicket,
        worktrees: GitWorktreeListResponse,
    ) -> bool {
        if ticket.kind != GitQueryKind::Worktrees
            || !self.accept_ticket(ticket)
            || worktrees.workspace_id != ticket.workspace_id
        {
            return false;
        }
        self.worktrees = Some(worktrees);
        self.last_error_code = None;
        true
    }

    pub fn fail_query(&mut self, ticket: &GitQueryTicket, error_code: &str) -> bool {
        if !self.accept_ticket(ticket) {
            return false;
        }
        self.last_error_code = Some(bounded_text(error_code, 120));
        true
    }

    pub fn set_mode(&mut self, mode: GitWorkbenchMode) {
        self.mode = mode;
    }

    pub fn set_history_filter(&mut self, filter: GitHistoryFilter) {
        self.history_filter = GitHistoryFilter {
            ref_name: filter.ref_name.and_then(normalized_optional),
            author: filter.author.and_then(normalized_optional),
        };
        self.history.clear();
        self.history_has_more = false;
        self.selected_commit_hash = None;
        self.commit_detail = None;
        self.clear_commit_tree();
        self.invalidate_queries();
    }

    pub fn select_commit(&mut self, hash: impl Into<String>) {
        let hash = hash.into();
        self.selected_commit_hash = Some(hash.clone());
        if self.commit_documents.contains_key(&hash) {
            self.sync_selected_commit_detail();
        } else {
            self.commit_detail = None;
            self.clear_commit_tree();
        }
    }

    pub fn clear_commit_selection(&mut self) {
        self.selected_commit_hash = None;
        self.commit_detail = None;
        self.clear_commit_tree();
    }

    pub fn select_change(&mut self, key: GitSelectionKey, toggle: bool) -> bool {
        let key = GitSelectionKey {
            path: normalize_path(&key.path),
            staged: key.staged,
        };
        if !self.change_exists(&key) {
            return false;
        }
        self.change_selection_seeded = true;
        if toggle {
            if !self.selected_changes.remove(&key) {
                self.selected_changes.insert(key);
            }
        } else {
            self.selected_changes.clear();
            self.selected_changes.insert(key);
        }
        true
    }

    pub fn select_all(&mut self, staged: Option<bool>) {
        self.change_selection_seeded = true;
        self.selected_changes = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .flat_map(change_selection_keys)
            .filter(|key| staged.is_none_or(|staged| key.staged == staged))
            .take(GIT_CHANGE_MAX_ROWS)
            .collect();
    }

    pub fn select_path(&mut self, path: &str, selected: bool) -> bool {
        let path = normalize_path(path);
        let Some(change) = self
            .status
            .as_ref()
            .and_then(|status| status.changes.iter().find(|change| change.path == path))
        else {
            return false;
        };
        let keys = change_selection_keys(change).collect::<Vec<_>>();
        self.change_selection_seeded = true;
        for key in keys {
            if selected {
                self.selected_changes.insert(key);
            } else {
                self.selected_changes.remove(&key);
            }
        }
        true
    }

    pub fn select_path_prefix(&mut self, path: &str, selected: bool) -> bool {
        let path = normalize_path(path);
        let paths = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .filter(|change| path.is_empty() || path_is_equal_or_descendant(&change.path, &path))
            .map(|change| change.path.clone())
            .collect::<Vec<_>>();
        if paths.is_empty() {
            return false;
        }
        for candidate in paths {
            self.select_path(&candidate, selected);
        }
        true
    }

    pub fn selected_change_paths(&self) -> Vec<String> {
        self.selected_path_set().into_iter().collect()
    }

    pub fn selected_path_count(&self) -> usize {
        self.selected_path_set().len()
    }

    pub fn path_selection_state(&self, path: &str) -> GitPathSelectionState {
        let path = normalize_path(path);
        let available = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .filter(|change| path.is_empty() || path_is_equal_or_descendant(&change.path, &path))
            .map(|change| change.path.as_str())
            .collect::<BTreeSet<_>>();
        if available.is_empty() {
            return GitPathSelectionState::Unchecked;
        }
        let selected = self.selected_path_set();
        let selected_count = available
            .iter()
            .filter(|candidate| selected.contains(**candidate))
            .count();
        if selected_count == 0 {
            GitPathSelectionState::Unchecked
        } else if selected_count == available.len() {
            GitPathSelectionState::Checked
        } else {
            GitPathSelectionState::Indeterminate
        }
    }

    pub fn selected_paths(&self, staged: bool) -> Vec<String> {
        self.selected_changes
            .iter()
            .filter(|key| key.staged == staged)
            .map(|key| key.path.clone())
            .collect()
    }

    pub fn change_row_count(&self) -> usize {
        self.change_row_index.len()
    }

    pub fn change_row(&self, index: usize) -> Option<(&GitChange, bool)> {
        let (change_index, staged) = *self.change_row_index.get(index)?;
        Some((self.status.as_ref()?.changes.get(change_index)?, staged))
    }

    pub fn change_tree_row_count(&self) -> usize {
        self.change_tree_rows.len()
    }

    pub fn change_tree_row(&self, index: usize) -> Option<(&GitTreeRow, Option<&GitChange>)> {
        let row = self.change_tree_rows.get(index)?;
        let change = row
            .change_index
            .and_then(|change_index| self.status.as_ref()?.changes.get(change_index));
        Some((row, change))
    }

    pub fn toggle_change_directories(&mut self, paths: &[String]) {
        toggle_directories(&mut self.expanded_change_directories, paths);
        self.rebuild_change_tree_rows();
    }

    pub fn toggle_all_change_directories(&mut self) {
        toggle_all_directories(
            &mut self.expanded_change_directories,
            &self.change_directory_paths,
        );
        self.rebuild_change_tree_rows();
    }

    pub fn all_change_directories_expanded(&self) -> bool {
        !self.change_directory_paths.is_empty()
            && self
                .change_directory_paths
                .iter()
                .all(|path| self.expanded_change_directories.contains(path))
    }

    pub fn has_change_directories(&self) -> bool {
        !self.change_directory_paths.is_empty()
    }

    pub fn history_row_count(&self) -> usize {
        self.history.len()
    }

    pub fn history_row(&self, index: usize) -> Option<&vibex_core::GitCommitSummary> {
        self.history.get(index)
    }

    pub fn commit_tree_row_count(&self) -> usize {
        self.commit_tree_rows.len()
    }

    pub fn commit_tree_row(&self, index: usize) -> Option<(&GitTreeRow, Option<&GitChange>)> {
        let row = self.commit_tree_rows.get(index)?;
        let change = row
            .change_index
            .and_then(|change_index| self.commit_tree_changes.get(change_index));
        Some((row, change))
    }

    pub fn toggle_commit_directories(&mut self, paths: &[String]) {
        toggle_directories(&mut self.expanded_commit_directories, paths);
        self.rebuild_commit_tree_rows();
    }

    pub fn move_path(&mut self, source: &str, destination: &str) {
        let source = normalize_path(source);
        let destination = normalize_path(destination);
        if source.is_empty() || destination.is_empty() || source == destination {
            return;
        }
        if let Some(status) = self.status.as_mut() {
            for change in &mut status.changes {
                change.path = replace_path_prefix(&change.path, &source, &destination);
                change.original_path = change
                    .original_path
                    .take()
                    .map(|path| replace_path_prefix(&path, &source, &destination));
            }
        }
        self.selected_changes = std::mem::take(&mut self.selected_changes)
            .into_iter()
            .map(|key| GitSelectionKey {
                path: replace_path_prefix(&key.path, &source, &destination),
                staged: key.staged,
            })
            .collect();
        self.rebuild_change_row_index();
        self.rebuild_change_tree();
        self.invalidate_queries();
        self.reconcile_change_selection();
    }

    pub fn delete_path(&mut self, path: &str) {
        let path = normalize_path(path);
        if path.is_empty() {
            return;
        }
        self.selected_changes
            .retain(|key| !path_is_equal_or_descendant(&key.path, &path));
        self.invalidate_queries();
    }

    pub fn history_window(
        &self,
        start: usize,
        length: usize,
        overscan: usize,
    ) -> &[vibex_core::GitCommitSummary] {
        let start = start.saturating_sub(overscan).min(self.history.len());
        let end = start
            .saturating_add(length)
            .saturating_add(overscan.saturating_mul(2))
            .min(self.history.len());
        &self.history[start..end]
    }

    pub fn begin_mutation(&mut self, scope: GitMutationScope) -> bool {
        if self.pending_mutation.is_some() || scope.operation_id.trim().is_empty() {
            return false;
        }
        self.pending_mutation = Some(GitMutationScope {
            operation_id: bounded_text(&scope.operation_id, 160),
            kind: scope.kind,
            paths: normalize_paths(scope.paths),
            target: scope.target.and_then(normalized_optional),
            destructive: scope.destructive,
            confirmation_label: bounded_text(&scope.confirmation_label, 240),
        });
        self.last_error_code = None;
        true
    }

    pub fn finish_mutation(
        &mut self,
        operation_id: &str,
        status: Option<GitStatusSummary>,
    ) -> bool {
        if self
            .pending_mutation
            .as_ref()
            .is_none_or(|scope| scope.operation_id != operation_id)
        {
            return false;
        }
        self.pending_mutation = None;
        self.revision_epoch = self.revision_epoch.saturating_add(1).max(1);
        self.active_queries.clear();
        self.diffs.clear();
        self.diff_epochs.clear();
        self.commit_detail = None;
        self.commit_documents.clear();
        self.commit_document_epochs.clear();
        self.commit_cache_epoch = 0;
        self.clear_commit_tree();
        self.blame = None;
        let selected_paths = self.selected_path_set();
        if let Some(status) = status
            && self.workspace_id.as_ref() == Some(&status.workspace_id)
        {
            self.status = Some(status);
        }
        self.rebuild_change_row_index();
        self.rebuild_change_tree();
        self.reconcile_path_selection(selected_paths);
        self.last_error_code = None;
        true
    }

    pub fn fail_mutation(&mut self, operation_id: &str, error_code: &str) -> bool {
        if self
            .pending_mutation
            .as_ref()
            .is_none_or(|scope| scope.operation_id != operation_id)
        {
            return false;
        }
        self.pending_mutation = None;
        self.last_error_code = Some(bounded_text(error_code, 120));
        true
    }

    pub fn invalidate_queries(&mut self) {
        self.revision_epoch = self.revision_epoch.saturating_add(1).max(1);
        self.active_queries.clear();
        self.diffs.clear();
        self.diff_epochs.clear();
    }

    fn sync_selected_commit_detail(&mut self) {
        let Some(hash) = self.selected_commit_hash.clone() else {
            self.commit_detail = None;
            self.clear_commit_tree();
            return;
        };
        let Some(detail) = self
            .commit_documents
            .get(&hash)
            .map(|document| document.detail.clone())
        else {
            self.commit_detail = None;
            self.clear_commit_tree();
            return;
        };
        self.commit_tree_changes = detail
            .files
            .iter()
            .map(|file| GitChange {
                path: file.path.clone(),
                original_path: file.original_path.clone(),
                kind: file.kind,
                staged: true,
                unstaged: false,
                additions: file.additions,
                deletions: file.deletions,
            })
            .collect();
        self.commit_detail = Some(detail);
        self.rebuild_commit_tree();
    }

    fn enforce_commit_cache_budget(&mut self) {
        while self.commit_documents.len() > GIT_COMMIT_CACHE_ITEM_LIMIT {
            let Some(oldest) = self
                .commit_document_epochs
                .iter()
                .min_by_key(|(_, epoch)| *epoch)
                .map(|(hash, _)| hash.clone())
            else {
                break;
            };
            self.commit_document_epochs.remove(&oldest);
            self.commit_documents.remove(&oldest);
        }
    }

    fn enforce_diff_cache_budget(&mut self) {
        loop {
            let rows = self
                .diffs
                .values()
                .map(GitDiffDocument::row_count)
                .sum::<usize>();
            let bytes = self
                .diffs
                .values()
                .map(GitDiffDocument::estimated_bytes)
                .sum::<usize>();
            if self.diffs.len() <= GIT_DIFF_CACHE_ITEM_LIMIT
                && rows <= GIT_DIFF_CACHE_ROW_LIMIT
                && bytes <= GIT_DIFF_CACHE_BYTE_LIMIT
            {
                break;
            }
            let Some(oldest) = self
                .diff_epochs
                .iter()
                .min_by_key(|(_, epoch)| *epoch)
                .map(|(key, _)| key.clone())
            else {
                break;
            };
            self.diff_epochs.remove(&oldest);
            self.diffs.remove(&oldest);
        }
    }

    fn change_exists(&self, key: &GitSelectionKey) -> bool {
        self.status.as_ref().is_some_and(|status| {
            status
                .changes
                .iter()
                .flat_map(change_selection_keys)
                .any(|candidate| candidate == *key)
        })
    }

    fn reconcile_change_selection(&mut self) {
        let valid = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .flat_map(change_selection_keys)
            .collect::<BTreeSet<_>>();
        self.selected_changes.retain(|key| valid.contains(key));
    }

    fn reconcile_path_selection(&mut self, selected_paths: BTreeSet<String>) {
        let available_paths = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .map(|change| normalize_path(&change.path))
            .collect::<BTreeSet<_>>();
        if available_paths.is_empty() {
            self.selected_changes.clear();
            self.change_selection_seeded = false;
            return;
        }
        let retained_paths = if self.change_selection_seeded {
            selected_paths
                .intersection(&available_paths)
                .cloned()
                .collect::<BTreeSet<_>>()
        } else {
            self.change_selection_seeded = true;
            available_paths
        };
        self.selected_changes = self
            .status
            .as_ref()
            .into_iter()
            .flat_map(|status| status.changes.iter())
            .filter(|change| retained_paths.contains(&normalize_path(&change.path)))
            .flat_map(change_selection_keys)
            .collect();
    }

    fn selected_path_set(&self) -> BTreeSet<String> {
        self.selected_changes
            .iter()
            .map(|key| normalize_path(&key.path))
            .collect()
    }

    fn rebuild_change_tree(&mut self) {
        self.change_tree_root = build_git_tree(
            self.status
                .as_ref()
                .map(|status| status.changes.as_slice())
                .unwrap_or_default(),
        );
        let next_paths = collect_git_directory_paths(&self.change_tree_root);
        if next_paths != self.change_directory_paths {
            self.expanded_change_directories = next_paths.clone();
        }
        self.change_directory_paths = next_paths;
        self.rebuild_change_tree_rows();
    }

    fn rebuild_change_tree_rows(&mut self) {
        self.change_tree_rows.clear();
        flatten_git_tree(
            &self.change_tree_root,
            0,
            &self.expanded_change_directories,
            &mut self.change_tree_rows,
        );
    }

    fn rebuild_commit_tree(&mut self) {
        self.commit_tree_root = build_git_tree(&self.commit_tree_changes);
        self.commit_directory_paths = collect_git_directory_paths(&self.commit_tree_root);
        self.expanded_commit_directories = self.commit_directory_paths.clone();
        self.rebuild_commit_tree_rows();
    }

    fn rebuild_commit_tree_rows(&mut self) {
        self.commit_tree_rows.clear();
        flatten_git_tree(
            &self.commit_tree_root,
            0,
            &self.expanded_commit_directories,
            &mut self.commit_tree_rows,
        );
    }

    fn clear_commit_tree(&mut self) {
        self.commit_tree_changes.clear();
        self.commit_tree_root = GitTreeNode::default();
        self.commit_tree_rows.clear();
        self.commit_directory_paths.clear();
        self.expanded_commit_directories.clear();
    }

    fn rebuild_change_row_index(&mut self) {
        self.change_row_index.clear();
        let Some(status) = self.status.as_ref() else {
            return;
        };
        for (index, change) in status.changes.iter().enumerate() {
            if self.change_row_index.len() >= GIT_CHANGE_MAX_ROWS {
                break;
            }
            if change.staged {
                self.change_row_index.push((index, true));
            }
            if change.unstaged && self.change_row_index.len() < GIT_CHANGE_MAX_ROWS {
                self.change_row_index.push((index, false));
            }
        }
    }
}

fn build_git_tree(changes: &[GitChange]) -> GitTreeNode {
    let mut root = GitTreeNode::default();
    for (change_index, change) in changes.iter().enumerate().take(GIT_CHANGE_MAX_ROWS) {
        let path = normalize_path(&change.path);
        let parts = path.split('/').filter(|part| !part.is_empty());
        let mut current = &mut root;
        let mut current_path = String::new();
        for part in parts {
            current_path = if current_path.is_empty() {
                part.to_string()
            } else {
                format!("{current_path}/{part}")
            };
            current = current
                .children
                .entry(part.to_string())
                .or_insert_with(|| GitTreeNode {
                    name: part.to_string(),
                    path: current_path.clone(),
                    change_index: None,
                    children: BTreeMap::new(),
                });
        }
        if !current.path.is_empty() {
            current.change_index = Some(change_index);
        }
    }
    root
}

fn collect_git_directory_paths(root: &GitTreeNode) -> BTreeSet<String> {
    fn collect(node: &GitTreeNode, paths: &mut BTreeSet<String>) {
        for child in node.children.values() {
            if !child.children.is_empty() {
                paths.insert(child.path.clone());
                collect(child, paths);
            }
        }
    }

    let mut paths = BTreeSet::new();
    collect(root, &mut paths);
    paths
}

fn flatten_git_tree(
    node: &GitTreeNode,
    depth: usize,
    expanded_directories: &BTreeSet<String>,
    rows: &mut Vec<GitTreeRow>,
) {
    for child in node
        .children
        .values()
        .filter(|child| !child.children.is_empty())
    {
        let mut chain = vec![child];
        let mut visible = child;
        loop {
            let mut directory_children = visible
                .children
                .values()
                .filter(|candidate| !candidate.children.is_empty());
            let Some(next) = directory_children.next() else {
                break;
            };
            if directory_children.next().is_some()
                || visible.children.len() != 1
                || visible.change_index.is_some()
            {
                break;
            }
            chain.push(next);
            visible = next;
        }
        let paths = chain
            .iter()
            .map(|segment| segment.path.clone())
            .collect::<Vec<_>>();
        let expanded = paths.iter().all(|path| expanded_directories.contains(path));
        rows.push(GitTreeRow {
            id: format!("git-directory:{}", visible.path),
            kind: GitTreeRowKind::Directory,
            path: visible.path.clone(),
            segments: chain
                .iter()
                .map(|segment| GitTreeSegment {
                    name: segment.name.clone(),
                    path: segment.path.clone(),
                })
                .collect(),
            depth,
            expanded,
            change_index: None,
        });
        if expanded {
            flatten_git_tree(visible, depth.saturating_add(1), expanded_directories, rows);
        }
    }

    for child in node
        .children
        .values()
        .filter(|child| child.children.is_empty() && child.change_index.is_some())
    {
        rows.push(GitTreeRow {
            id: format!("git-file:{}", child.path),
            kind: GitTreeRowKind::File,
            path: child.path.clone(),
            segments: vec![GitTreeSegment {
                name: child.name.clone(),
                path: child.path.clone(),
            }],
            depth,
            expanded: false,
            change_index: child.change_index,
        });
    }
}

fn toggle_directories(expanded: &mut BTreeSet<String>, paths: &[String]) {
    let paths = paths
        .iter()
        .map(|path| normalize_path(path))
        .filter(|path| !path.is_empty())
        .collect::<Vec<_>>();
    if paths.is_empty() {
        return;
    }
    let all_expanded = paths.iter().all(|path| expanded.contains(path));
    for path in paths {
        if all_expanded {
            expanded.remove(&path);
        } else {
            expanded.insert(path);
        }
    }
}

fn toggle_all_directories(expanded: &mut BTreeSet<String>, available: &BTreeSet<String>) {
    if available.is_empty() {
        return;
    }
    if available.iter().all(|path| expanded.contains(path)) {
        expanded.clear();
    } else {
        *expanded = available.clone();
    }
}

pub fn mutation_scope(
    operation_id: impl Into<String>,
    kind: GitMutationKind,
    paths: Vec<String>,
    target: Option<String>,
) -> GitMutationScope {
    let paths = normalize_paths(paths);
    let destructive = matches!(
        kind,
        GitMutationKind::Revert
            | GitMutationKind::Push
            | GitMutationKind::BranchCheckout
            | GitMutationKind::WorktreeMerge
            | GitMutationKind::WorktreeDiscard
    );
    let noun = match kind {
        GitMutationKind::Stage => "Stage",
        GitMutationKind::Unstage => "Unstage",
        GitMutationKind::Revert => "Revert",
        GitMutationKind::Commit => "Commit",
        GitMutationKind::Amend => "Amend commit",
        GitMutationKind::Fetch => "Fetch",
        GitMutationKind::Push => "Push",
        GitMutationKind::BranchCreate => "Create branch",
        GitMutationKind::BranchCheckout => "Checkout branch",
        GitMutationKind::WorktreeCreate => "Create worktree",
        GitMutationKind::WorktreeMerge => "Merge worktree",
        GitMutationKind::WorktreeDiscard => "Discard worktree",
    };
    let confirmation_label = if paths.is_empty() {
        target
            .as_deref()
            .map(|target| format!("{noun} {target}"))
            .unwrap_or_else(|| noun.to_string())
    } else {
        format!("{noun} {} selected path(s)", paths.len())
    };
    GitMutationScope {
        operation_id: operation_id.into(),
        kind,
        paths,
        target,
        destructive,
        confirmation_label,
    }
}

fn change_selection_keys(change: &GitChange) -> impl Iterator<Item = GitSelectionKey> + '_ {
    [
        change.staged.then_some(GitSelectionKey {
            path: normalize_path(&change.path),
            staged: true,
        }),
        change.unstaged.then_some(GitSelectionKey {
            path: normalize_path(&change.path),
            staged: false,
        }),
    ]
    .into_iter()
    .flatten()
}

fn normalize_paths(paths: Vec<String>) -> Vec<String> {
    paths
        .into_iter()
        .map(|path| normalize_path(&path))
        .filter(|path| !path.is_empty())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect()
}

fn normalize_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
}

fn path_is_equal_or_descendant(candidate: &str, ancestor: &str) -> bool {
    candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn replace_path_prefix(path: &str, source: &str, destination: &str) -> String {
    let path = normalize_path(path);
    if path == source {
        destination.to_string()
    } else {
        path.strip_prefix(source)
            .filter(|suffix| suffix.starts_with('/'))
            .map(|suffix| format!("{destination}{suffix}"))
            .unwrap_or(path)
    }
}

fn normalized_optional(value: String) -> Option<String> {
    let value = bounded_text(&value, 1_024);
    (!value.is_empty()).then_some(value)
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        GitChangeKind, GitCommitDetail, GitCommitFileChange, GitCommitSummary, GitDiffResponse,
    };

    fn status(workspace_id: &WorkspaceId) -> GitStatusSummary {
        GitStatusSummary {
            workspace_id: workspace_id.clone(),
            repo_path: "/redacted/repo".into(),
            branch: Some("main".into()),
            short_commit: Some("abc1234".into()),
            detached: false,
            dirty: true,
            staged_count: 1,
            unstaged_count: 1,
            untracked_count: 0,
            changes: vec![GitChange {
                path: "src/lib.rs".into(),
                original_path: None,
                kind: GitChangeKind::Modified,
                staged: true,
                unstaged: true,
                additions: 1,
                deletions: 1,
            }],
            captured_at_ms: 1,
        }
    }

    fn commit_detail(
        workspace_id: &WorkspaceId,
        hash: &str,
        patch: Option<&str>,
    ) -> GitCommitDetail {
        GitCommitDetail {
            workspace_id: workspace_id.clone(),
            summary: GitCommitSummary {
                hash: hash.into(),
                short_hash: hash.chars().take(8).collect(),
                parents: Vec::new(),
                author_name: "Ada".into(),
                author_email: "ada@example.test".into(),
                authored_at_ms: Some(1_700_000_000_000),
                subject: format!("Commit {hash}"),
                refs: Vec::new(),
            },
            body: Some("Commit body".into()),
            files: vec![
                GitCommitFileChange {
                    path: "src/one.rs".into(),
                    original_path: None,
                    kind: GitChangeKind::Modified,
                    additions: 1,
                    deletions: 1,
                },
                GitCommitFileChange {
                    path: "src/two.rs".into(),
                    original_path: None,
                    kind: GitChangeKind::Added,
                    additions: 1,
                    deletions: 0,
                },
            ],
            patch: patch.map(str::to_string),
            patch_truncated: false,
        }
    }

    #[test]
    fn stale_query_cannot_overwrite_a_new_revision_epoch() {
        let workspace_id = WorkspaceId::new();
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        let ticket = state.begin_query(GitQueryKind::Status, "status").unwrap();
        state.invalidate_queries();
        assert!(!state.apply_status(&ticket, status(&workspace_id)));
        assert!(state.status.is_none());
    }

    #[test]
    fn staged_and_unstaged_selection_are_distinct_and_scoped() {
        let workspace_id = WorkspaceId::new();
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        let ticket = state.begin_query(GitQueryKind::Status, "status").unwrap();
        state.apply_status(&ticket, status(&workspace_id));
        state.select_all(None);
        assert_eq!(state.selected_changes.len(), 2);
        assert_eq!(state.selected_paths(true), vec!["src/lib.rs"]);
        assert_eq!(state.selected_paths(false), vec!["src/lib.rs"]);
        assert_eq!(state.change_row_count(), 2);
        assert!(state.change_row(0).unwrap().1);
        assert!(!state.change_row(1).unwrap().1);
    }

    #[test]
    fn change_tree_compacts_directories_and_selects_paths_like_tauri() {
        let workspace_id = WorkspaceId::new();
        let mut response = status(&workspace_id);
        response.changes = vec![
            GitChange {
                path: "src/generated/one.rs".into(),
                original_path: None,
                kind: GitChangeKind::Modified,
                staged: true,
                unstaged: true,
                additions: 2,
                deletions: 1,
            },
            GitChange {
                path: "src/generated/two.rs".into(),
                original_path: None,
                kind: GitChangeKind::Untracked,
                staged: false,
                unstaged: true,
                additions: 4,
                deletions: 0,
            },
            GitChange {
                path: "Cargo.toml".into(),
                original_path: None,
                kind: GitChangeKind::Modified,
                staged: false,
                unstaged: true,
                additions: 1,
                deletions: 1,
            },
        ];
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id);
        let ticket = state.begin_query(GitQueryKind::Status, "status").unwrap();
        assert!(state.apply_status(&ticket, response));

        assert_eq!(state.selected_path_count(), 3);
        assert_eq!(state.change_tree_row_count(), 4);
        let (directory, change) = state.change_tree_row(0).unwrap();
        assert_eq!(directory.kind, GitTreeRowKind::Directory);
        assert_eq!(
            directory
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            vec!["src", "generated"]
        );
        assert!(directory.expanded);
        assert!(change.is_none());

        let directory_paths = directory
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect::<Vec<_>>();
        state.toggle_change_directories(&directory_paths);
        assert_eq!(state.change_tree_row_count(), 2);
        state.toggle_change_directories(&directory_paths);
        assert_eq!(state.change_tree_row_count(), 4);

        assert!(state.select_path("src/generated/one.rs", false));
        assert_eq!(state.selected_path_count(), 2);
        assert_eq!(
            state.path_selection_state("src/generated"),
            GitPathSelectionState::Indeterminate
        );
        assert!(state.select_path_prefix("src/generated", false));
        assert_eq!(
            state.path_selection_state("src/generated"),
            GitPathSelectionState::Unchecked
        );
        assert_eq!(state.selected_change_paths(), vec!["Cargo.toml"]);
    }

    #[test]
    fn file_rename_and_delete_remap_git_selection_immediately() {
        let workspace_id = WorkspaceId::new();
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        let ticket = state.begin_query(GitQueryKind::Status, "status").unwrap();
        assert!(state.apply_status(&ticket, status(&workspace_id)));
        state.select_all(None);

        state.move_path("src", "source");
        assert_eq!(state.selected_paths(true), vec!["source/lib.rs"]);
        assert_eq!(state.selected_paths(false), vec!["source/lib.rs"]);
        assert_eq!(state.change_row(0).unwrap().0.path, "source/lib.rs");

        state.delete_path("source");
        assert!(state.selected_changes.is_empty());
    }

    #[test]
    fn only_one_mutation_can_claim_the_ui_state_and_completion_invalidates_queries() {
        let workspace_id = WorkspaceId::new();
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        let before = state.revision_epoch();
        assert!(state.begin_mutation(mutation_scope(
            "op-1",
            GitMutationKind::Revert,
            vec!["src/lib.rs".into()],
            None,
        )));
        assert!(!state.begin_mutation(mutation_scope(
            "op-2",
            GitMutationKind::Push,
            Vec::new(),
            None,
        )));
        assert!(state.finish_mutation("op-1", Some(status(&workspace_id))));
        assert!(state.revision_epoch() > before);
        assert!(state.pending_mutation.is_none());
    }

    #[test]
    fn equal_length_diffs_receive_distinct_content_revisions() {
        let workspace_id = WorkspaceId::new();
        let apply = |diff: &str| {
            let mut state = GitWorkbenchState::default();
            state.reset_workspace(workspace_id.clone());
            let ticket = state.begin_query(GitQueryKind::Diff, "false:a.rs").unwrap();
            assert!(state.apply_diff(
                &ticket,
                GitDiffResponse {
                    workspace_id: workspace_id.clone(),
                    path: "a.rs".into(),
                    staged: false,
                    diff: diff.into(),
                    truncated: false,
                },
            ));
            state
                .diffs
                .get(&GitSelectionKey {
                    path: "a.rs".into(),
                    staged: false,
                })
                .unwrap()
                .revision
                .clone()
        };
        assert_ne!(apply("-one\n+two\n"), apply("-red\n+tan\n"));
    }

    #[test]
    fn diff_cache_is_lru_bounded_by_item_count() {
        let workspace_id = WorkspaceId::new();
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        for index in 0..GIT_DIFF_CACHE_ITEM_LIMIT {
            let path = format!("file-{index}.rs");
            let ticket = state
                .begin_query(GitQueryKind::Diff, format!("false:{path}"))
                .unwrap();
            assert!(state.apply_diff(
                &ticket,
                GitDiffResponse {
                    workspace_id: workspace_id.clone(),
                    path,
                    staged: false,
                    diff: format!(
                        "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-{index}\n+next\n"
                    ),
                    truncated: false,
                },
            ));
        }
        let first = GitSelectionKey {
            path: "file-0.rs".into(),
            staged: false,
        };
        assert!(state.diff_mut(&first).is_some());

        let path = format!("file-{}.rs", GIT_DIFF_CACHE_ITEM_LIMIT);
        let ticket = state
            .begin_query(GitQueryKind::Diff, format!("false:{path}"))
            .unwrap();
        state.apply_diff(
            &ticket,
            GitDiffResponse {
                workspace_id,
                path,
                staged: false,
                diff: "diff --git a/a b/a\n--- a/a\n+++ b/a\n@@ -1 +1 @@\n-old\n+new\n".into(),
                truncated: false,
            },
        );
        assert_eq!(state.diffs.len(), GIT_DIFF_CACHE_ITEM_LIMIT);
        assert!(state.diffs.contains_key(&first));
        assert!(!state.diffs.contains_key(&GitSelectionKey {
            path: "file-1.rs".into(),
            staged: false,
        }));
    }

    #[test]
    fn commit_details_are_cached_per_hash_without_hijacking_the_selected_commit() {
        let workspace_id = WorkspaceId::new();
        let patch = concat!(
            "diff --git a/src/one.rs b/src/one.rs\n",
            "--- a/src/one.rs\n",
            "+++ b/src/one.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
        );
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        state.select_commit("commit-a");
        let ticket_a = state
            .begin_query(GitQueryKind::CommitDetail, "commit-a")
            .unwrap();
        assert!(state.apply_commit_detail(
            &ticket_a,
            commit_detail(&workspace_id, "commit-a", Some(patch)),
        ));

        let ticket_b = state
            .begin_query(GitQueryKind::CommitDetail, "commit-b")
            .unwrap();
        assert!(state.apply_commit_detail(
            &ticket_b,
            commit_detail(&workspace_id, "commit-b", Some(patch)),
        ));
        assert_eq!(state.selected_commit_hash.as_deref(), Some("commit-a"));
        assert_eq!(
            state
                .commit_detail
                .as_ref()
                .map(|detail| detail.summary.hash.as_str()),
            Some("commit-a")
        );
        assert!(state.commit_detail_for("commit-a").is_some());
        assert!(state.commit_detail_for("commit-b").is_some());

        let metadata_ticket = state
            .begin_query(GitQueryKind::CommitDetail, "commit-a")
            .unwrap();
        assert!(state.apply_commit_detail(
            &metadata_ticket,
            commit_detail(&workspace_id, "commit-a", None),
        ));
        assert!(state.commit_patch_ready("commit-a"));
    }

    #[test]
    fn commit_patch_projection_collapses_and_focus_expands_files() {
        let workspace_id = WorkspaceId::new();
        let patch = concat!(
            "diff --git a/src/one.rs b/src/one.rs\n",
            "--- a/src/one.rs\n",
            "+++ b/src/one.rs\n",
            "@@ -1 +1 @@\n",
            "-old\n",
            "+new\n",
            "diff --git a/src/two.rs b/src/two.rs\n",
            "--- /dev/null\n",
            "+++ b/src/two.rs\n",
            "@@ -0,0 +1 @@\n",
            "+two\n",
        );
        let mut state = GitWorkbenchState::default();
        state.reset_workspace(workspace_id.clone());
        let ticket = state
            .begin_query(GitQueryKind::CommitDetail, "commit-a")
            .unwrap();
        assert!(state.apply_commit_detail(
            &ticket,
            commit_detail(&workspace_id, "commit-a", Some(patch)),
        ));

        assert_eq!(state.commit_preview_row_count("commit-a"), 7);
        let rows = state.commit_preview_window("commit-a", 0, 1);
        assert!(matches!(
            rows.as_slice(),
            [GitCommitPatchRow::FileHeader {
                path,
                additions: 1,
                deletions: 1,
                collapsed: false,
                ..
            }] if path == "src/one.rs"
        ));

        assert!(state.toggle_commit_file("commit-a", "src/one.rs"));
        assert_eq!(state.commit_preview_row_count("commit-a"), 4);
        assert_eq!(state.focus_commit_file("commit-a", "src/one.rs"), Some(0));
        assert_eq!(state.commit_preview_row_count("commit-a"), 7);
        assert_eq!(state.focus_commit_file("commit-a", "src/two.rs"), Some(4));
    }
}

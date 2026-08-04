use std::collections::{BTreeMap, BTreeSet};
use std::ops::Range;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vibex_core::{FileEntryKind, FileTreeEntry, GitChange, GitChangeKind, WorkspaceId};

pub const FILE_TREE_MAX_ROWS: usize = 100_000;
pub const FILE_TREE_DEFAULT_OVERSCAN: usize = 24;

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileTreeLoadState {
    #[default]
    Unloaded,
    Loading,
    Loaded,
    Error {
        code: String,
        retryable: bool,
    },
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileMutationKind {
    CreateFile,
    CreateDirectory,
    Copy,
    Rename,
    Move,
    Delete,
    OpenExternal,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PendingFileMutation {
    pub operation_id: String,
    pub kind: FileMutationKind,
    pub source_path: String,
    pub target_path: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileGitSignal {
    Added,
    Modified,
    Deleted,
    Renamed,
    Copied,
    Untracked,
    Conflicted,
    Ignored,
}

impl FileGitSignal {
    pub fn short_label(self) -> &'static str {
        match self {
            Self::Added => "A",
            Self::Modified => "M",
            Self::Deleted => "D",
            Self::Renamed => "R",
            Self::Copied => "C",
            Self::Untracked => "?",
            Self::Conflicted => "!",
            Self::Ignored => "I",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileGitState {
    pub signal: FileGitSignal,
    pub staged: bool,
    pub unstaged: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileIconKind {
    Directory,
    Code,
    Java,
    Rust,
    TypeScript,
    JavaScript,
    Json,
    Markdown,
    Image,
    Svg,
    Audio,
    Video,
    Archive,
    Database,
    Spreadsheet,
    Font,
    Pdf,
    Office,
    Config,
    Lock,
    Secret,
    Script,
    Style,
    Markup,
    Text,
    File,
    Symlink,
    Other,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileIconTone {
    Directory,
    Source,
    Data,
    Document,
    Media,
    Config,
    Neutral,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileIconDescriptor {
    pub kind: FileIconKind,
    pub tone: FileIconTone,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeSegment {
    pub path: String,
    pub name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileExplorerRow {
    pub id: String,
    pub path: String,
    pub name: String,
    pub depth: usize,
    pub kind: FileEntryKind,
    pub icon: FileIconDescriptor,
    pub expanded: bool,
    pub selected: bool,
    pub hidden: bool,
    pub ignored: bool,
    pub load_state: FileTreeLoadState,
    pub git: Option<FileGitState>,
    pub pending: Option<PendingFileMutation>,
    pub match_ranges: Vec<Range<usize>>,
    pub accessible_name: String,
    #[serde(default)]
    pub segments: Vec<FileTreeSegment>,
    #[serde(default)]
    pub path_chain: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct FileTreeNode {
    entry: FileTreeEntry,
    load_state: FileTreeLoadState,
    #[serde(default)]
    lower_name: String,
    #[serde(default)]
    lower_path: String,
}

impl FileTreeNode {
    fn new(entry: FileTreeEntry, load_state: FileTreeLoadState) -> Self {
        let lower_name = entry.name.to_lowercase();
        let lower_path = entry.path.to_lowercase();
        Self {
            entry,
            load_state,
            lower_name,
            lower_path,
        }
    }

    fn refresh_search_keys(&mut self) {
        self.lower_name = self.entry.name.to_lowercase();
        self.lower_path = self.entry.path.to_lowercase();
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct FileTreeProjection {
    workspace_id: Option<WorkspaceId>,
    #[serde(default = "default_file_tree_root_name")]
    root_name: String,
    #[serde(default)]
    root_collapsed: bool,
    #[serde(default)]
    root_load_state: FileTreeLoadState,
    generation: u64,
    #[serde(default)]
    load_generations: BTreeMap<String, u64>,
    nodes: BTreeMap<String, FileTreeNode>,
    #[serde(default)]
    children_by_parent: BTreeMap<String, Vec<String>>,
    expanded_paths: BTreeSet<String>,
    selected_paths: BTreeSet<String>,
    selection_anchor: Option<String>,
    #[serde(default)]
    selected_directory_path: Option<String>,
    git_states: BTreeMap<String, FileGitState>,
    pending: BTreeMap<String, PendingFileMutation>,
    query: String,
    #[serde(skip)]
    visible_rows: Arc<Vec<FileExplorerRow>>,
    #[serde(skip)]
    presentation_revision: u64,
}

impl FileTreeProjection {
    pub fn workspace_id(&self) -> Option<&WorkspaceId> {
        self.workspace_id.as_ref()
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn reset_workspace(&mut self, workspace_id: WorkspaceId) -> u64 {
        self.generation = self.generation.saturating_add(1).max(1);
        self.workspace_id = Some(workspace_id);
        self.root_name = default_file_tree_root_name();
        self.root_collapsed = false;
        self.root_load_state = FileTreeLoadState::Unloaded;
        self.nodes.clear();
        self.children_by_parent.clear();
        self.load_generations.clear();
        self.expanded_paths.clear();
        self.selected_paths.clear();
        self.selection_anchor = None;
        self.selected_directory_path = None;
        self.git_states.clear();
        self.pending.clear();
        self.query.clear();
        self.visible_rows = Arc::default();
        self.presentation_revision = self.presentation_revision.saturating_add(1).max(1);
        self.generation
    }

    pub fn set_root_name(&mut self, root_name: impl Into<String>) {
        let root_name = bounded_text(&root_name.into(), 255);
        self.root_name = if root_name.is_empty() {
            default_file_tree_root_name()
        } else {
            root_name
        };
        self.rebuild_visible_rows();
    }

    pub fn root_name(&self) -> &str {
        &self.root_name
    }

    pub fn presentation_revision(&self) -> u64 {
        self.presentation_revision
    }

    pub fn visible_rows_snapshot(&self) -> Arc<Vec<FileExplorerRow>> {
        self.visible_rows.clone()
    }

    pub fn expanded_paths_snapshot(&self) -> Arc<BTreeSet<String>> {
        Arc::new(self.expanded_paths.clone())
    }

    pub fn begin_load(&mut self, path: &str) -> u64 {
        self.generation = self.generation.saturating_add(1).max(1);
        let path = normalize_relative_path(path);
        self.load_generations.insert(path.clone(), self.generation);
        if path.is_empty() {
            self.root_load_state = FileTreeLoadState::Loading;
        } else if let Some(node) = self.nodes.get_mut(&path) {
            node.load_state = FileTreeLoadState::Loading;
        }
        self.rebuild_visible_rows();
        self.generation
    }

    pub fn begin_refresh(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1).max(1);
        self.generation
    }

    pub fn invalidate_refresh(&mut self) {
        self.generation = self.generation.saturating_add(1).max(1);
    }

    pub fn expanded_directory_paths(&self) -> Vec<String> {
        self.expanded_paths
            .iter()
            .filter(|path| {
                self.nodes
                    .get(*path)
                    .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
            })
            .cloned()
            .collect()
    }

    pub fn apply_entries(
        &mut self,
        workspace_id: &WorkspaceId,
        generation: u64,
        base_path: &str,
        entries: Vec<FileTreeEntry>,
    ) -> bool {
        let base_path = normalize_relative_path(base_path);
        if self.workspace_id.as_ref() != Some(workspace_id)
            || self.load_generations.get(&base_path) != Some(&generation)
        {
            return false;
        }
        self.load_generations.remove(&base_path);
        let incoming = entries
            .into_iter()
            .take(FILE_TREE_MAX_ROWS)
            .filter(|entry| entry.workspace_id == *workspace_id)
            .filter_map(|mut entry| {
                entry.path = normalize_relative_path(&entry.path);
                if entry.path.is_empty() || path_contains_git_directory(&entry.path) {
                    return None;
                }
                entry.parent_path = entry
                    .parent_path
                    .take()
                    .map(|path| normalize_relative_path(&path));
                Some((entry.path.clone(), entry))
            })
            .collect::<BTreeMap<_, _>>();
        self.nodes.retain(|path, _| {
            path == &base_path
                || !path_is_equal_or_descendant(path, &base_path)
                || incoming.contains_key(path)
        });
        for (path, entry) in incoming {
            let load_state = if entry.kind == FileEntryKind::Directory {
                FileTreeLoadState::Unloaded
            } else {
                FileTreeLoadState::Loaded
            };
            self.nodes
                .insert(path, FileTreeNode::new(entry, load_state));
        }
        if base_path.is_empty() {
            self.root_load_state = FileTreeLoadState::Loaded;
        } else if let Some(node) = self.nodes.get_mut(&base_path) {
            node.load_state = FileTreeLoadState::Loaded;
        }
        self.selected_paths
            .retain(|path| self.nodes.contains_key(path));
        self.rebuild_children_index();
        self.rebuild_visible_rows();
        true
    }

    pub fn apply_refresh_entries(
        &mut self,
        workspace_id: &WorkspaceId,
        generation: u64,
        entries: Vec<FileTreeEntry>,
        failed_subtrees: &[String],
    ) -> bool {
        if self.workspace_id.as_ref() != Some(workspace_id) || self.generation != generation {
            return false;
        }

        let mut incoming = entries
            .into_iter()
            .take(FILE_TREE_MAX_ROWS)
            .filter(|entry| entry.workspace_id == *workspace_id)
            .filter_map(|mut entry| {
                entry.path = normalize_relative_path(&entry.path);
                if entry.path.is_empty() || path_contains_git_directory(&entry.path) {
                    return None;
                }
                entry.parent_path = entry
                    .parent_path
                    .take()
                    .map(|path| normalize_relative_path(&path));
                Some((entry.path.clone(), entry))
            })
            .collect::<BTreeMap<_, _>>();

        for failed_subtree in failed_subtrees {
            let failed_subtree = normalize_relative_path(failed_subtree);
            if !incoming.contains_key(&failed_subtree) {
                continue;
            }
            for (path, node) in &self.nodes {
                if path_is_equal_or_descendant(path, &failed_subtree) {
                    incoming
                        .entry(path.clone())
                        .or_insert_with(|| node.entry.clone());
                }
            }
        }

        let previous_nodes = std::mem::take(&mut self.nodes);
        self.nodes = incoming
            .into_iter()
            .map(|(path, entry)| {
                let load_state = if entry.kind == FileEntryKind::Directory {
                    previous_nodes
                        .get(&path)
                        .map(|node| node.load_state.clone())
                        .unwrap_or(FileTreeLoadState::Unloaded)
                } else {
                    FileTreeLoadState::Loaded
                };
                (path, FileTreeNode::new(entry, load_state))
            })
            .collect();
        self.root_load_state = FileTreeLoadState::Loaded;
        self.load_generations
            .retain(|path, _| path.is_empty() || self.nodes.contains_key(path));
        self.expanded_paths.retain(|path| {
            self.nodes
                .get(path)
                .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
        });
        self.selected_paths
            .retain(|path| self.nodes.contains_key(path));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|path| !self.nodes.contains_key(path))
        {
            self.selection_anchor = None;
        }
        if self
            .selected_directory_path
            .as_ref()
            .is_some_and(|path| !path.is_empty() && !self.nodes.contains_key(path))
        {
            self.selected_directory_path = None;
        }
        self.rebuild_children_index();
        self.rebuild_visible_rows();
        true
    }

    pub fn fail_load(&mut self, generation: u64, path: &str, code: &str) -> bool {
        let path = normalize_relative_path(path);
        if self.load_generations.get(&path) != Some(&generation) {
            return false;
        }
        self.load_generations.remove(&path);
        let error = FileTreeLoadState::Error {
            code: bounded_text(code, 120),
            retryable: true,
        };
        if path.is_empty() {
            self.root_load_state = error;
        } else if let Some(node) = self.nodes.get_mut(&path) {
            node.load_state = error;
        }
        self.rebuild_visible_rows();
        true
    }

    pub fn set_git_changes(&mut self, changes: &[GitChange]) {
        self.git_states.clear();
        for change in changes {
            let signal = match change.kind {
                GitChangeKind::Added if change.staged => FileGitSignal::Added,
                GitChangeKind::Added => FileGitSignal::Untracked,
                GitChangeKind::Modified => FileGitSignal::Modified,
                GitChangeKind::Deleted => FileGitSignal::Deleted,
                GitChangeKind::Renamed => FileGitSignal::Renamed,
                GitChangeKind::Copied => FileGitSignal::Copied,
                GitChangeKind::TypeChanged => FileGitSignal::Modified,
                GitChangeKind::Untracked => FileGitSignal::Untracked,
                GitChangeKind::Unmerged => FileGitSignal::Conflicted,
                GitChangeKind::Unknown => FileGitSignal::Modified,
            };
            self.git_states.insert(
                normalize_relative_path(&change.path),
                FileGitState {
                    signal,
                    staged: change.staged,
                    unstaged: change.unstaged,
                },
            );
        }
        self.rebuild_visible_rows();
    }

    pub fn set_query(&mut self, query: impl Into<String>) {
        self.query = bounded_text(&query.into().to_lowercase(), 512);
        self.rebuild_visible_rows();
    }

    pub fn query(&self) -> &str {
        &self.query
    }

    pub fn load_state(&self, path: &str) -> FileTreeLoadState {
        let path = normalize_relative_path(path);
        if path.is_empty() {
            return self.root_load_state.clone();
        }
        self.nodes
            .get(&path)
            .map(|node| node.load_state.clone())
            .unwrap_or(FileTreeLoadState::Unloaded)
    }

    pub fn is_expanded(&self, path: &str) -> bool {
        let path = normalize_relative_path(path);
        if path.is_empty() {
            !self.root_collapsed
        } else {
            self.expanded_paths.contains(&path)
        }
    }

    pub fn toggle_expanded(&mut self, path: &str) -> bool {
        let path = normalize_relative_path(path);
        if path.is_empty() {
            self.root_collapsed = !self.root_collapsed;
            if self.root_collapsed {
                self.selected_directory_path = None;
            }
            self.rebuild_visible_rows();
            return true;
        }
        if !self
            .nodes
            .get(&path)
            .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
        {
            return false;
        }
        if !self.expanded_paths.remove(&path) {
            self.expanded_paths.insert(path);
        }
        self.rebuild_visible_rows();
        true
    }

    pub fn chain_is_expanded(&self, paths: &[String]) -> bool {
        !paths.is_empty() && paths.iter().all(|path| self.is_expanded(path))
    }

    pub fn set_chain_expanded(&mut self, paths: &[String], expanded: bool) -> bool {
        let paths = paths
            .iter()
            .map(|path| normalize_relative_path(path))
            .collect::<Vec<_>>();
        if paths.is_empty()
            || paths.iter().any(|path| {
                !path.is_empty()
                    && !self
                        .nodes
                        .get(path)
                        .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
            })
        {
            return false;
        }
        if paths.iter().any(String::is_empty) {
            self.root_collapsed = !expanded;
        }
        if expanded {
            self.expanded_paths
                .extend(paths.iter().filter(|path| !path.is_empty()).cloned());
        } else {
            self.expanded_paths.retain(|candidate| {
                !paths
                    .iter()
                    .any(|path| path_is_equal_or_descendant(candidate, path))
            });
        }
        if !expanded
            && self
                .selected_directory_path
                .as_ref()
                .is_some_and(|selected| {
                    paths
                        .iter()
                        .any(|path| path_is_equal_or_descendant(selected, path))
                })
        {
            self.selected_directory_path = None;
        }
        self.rebuild_visible_rows();
        true
    }

    pub fn select_directory_segment(&mut self, path: &str, path_chain: &[String]) -> bool {
        let path = normalize_relative_path(path);
        let normalized_chain = path_chain
            .iter()
            .map(|path| normalize_relative_path(path))
            .collect::<Vec<_>>();
        let Some(selected_index) = normalized_chain.iter().position(|item| item == &path) else {
            return false;
        };
        if !path.is_empty()
            && !self
                .nodes
                .get(&path)
                .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
        {
            return false;
        }
        self.selected_directory_path = Some(path);
        self.root_collapsed = false;
        self.expanded_paths.extend(
            normalized_chain
                .into_iter()
                .take(selected_index.saturating_add(1))
                .filter(|path| !path.is_empty()),
        );
        self.rebuild_visible_rows();
        true
    }

    pub fn clear_selected_directory(&mut self) {
        if self.selected_directory_path.take().is_some() {
            self.rebuild_visible_rows();
        }
    }

    pub fn selected_directory_path(&self) -> Option<&str> {
        self.selected_directory_path.as_deref()
    }

    pub fn contains_path(&self, path: &str) -> bool {
        let path = normalize_relative_path(path);
        path.is_empty() || self.nodes.contains_key(&path)
    }

    pub fn select(&mut self, path: &str, extend: bool, toggle: bool) -> bool {
        let path = normalize_relative_path(path);
        if !self.nodes.contains_key(&path) {
            return false;
        }
        if extend {
            let anchor = self
                .selection_anchor
                .as_ref()
                .and_then(|anchor| self.visible_rows.iter().position(|row| &row.path == anchor));
            let target = self.visible_rows.iter().position(|row| row.path == path);
            if let (Some(anchor), Some(target)) = (anchor, target) {
                if !toggle {
                    self.selected_paths.clear();
                }
                let range = anchor.min(target)..=anchor.max(target);
                self.selected_paths
                    .extend(self.visible_rows[range].iter().map(|row| row.path.clone()));
            }
        } else if toggle {
            if !self.selected_paths.remove(&path) {
                self.selected_paths.insert(path.clone());
            }
            self.selection_anchor = Some(path);
        } else {
            self.selected_paths.clear();
            self.selected_paths.insert(path.clone());
            self.selection_anchor = Some(path);
        }
        self.rebuild_visible_rows();
        true
    }

    pub fn selected_paths(&self) -> &BTreeSet<String> {
        &self.selected_paths
    }

    pub fn set_pending(&mut self, mutation: PendingFileMutation) -> bool {
        let source = normalize_relative_path(&mutation.source_path);
        if source.is_empty() || self.pending.contains_key(&source) {
            return false;
        }
        let mut mutation = mutation;
        mutation.source_path = source.clone();
        mutation.target_path = mutation
            .target_path
            .take()
            .map(|path| normalize_relative_path(&path));
        self.pending.insert(source, mutation);
        self.rebuild_visible_rows();
        true
    }

    pub fn finish_pending(&mut self, operation_id: &str) -> bool {
        let before = self.pending.len();
        self.pending
            .retain(|_, pending| pending.operation_id != operation_id);
        let changed = self.pending.len() != before;
        if changed {
            self.rebuild_visible_rows();
        }
        changed
    }

    pub fn move_path(&mut self, source: &str, destination: &str) {
        let source = normalize_relative_path(source);
        let destination = normalize_relative_path(destination);
        if source.is_empty() || destination.is_empty() || source == destination {
            return;
        }
        let mut replacements = Vec::new();
        for path in self.nodes.keys() {
            if path_is_equal_or_descendant(path, &source) {
                replacements.push((
                    path.clone(),
                    replace_path_prefix(path, &source, &destination),
                ));
            }
        }
        for (old_path, new_path) in &replacements {
            if let Some(mut node) = self.nodes.remove(old_path) {
                node.entry.path = new_path.clone();
                node.entry.name = file_name(new_path);
                node.entry.parent_path = parent_path(new_path);
                self.nodes.insert(new_path.clone(), node);
            }
        }
        remap_set(&mut self.expanded_paths, &source, &destination);
        remap_set(&mut self.selected_paths, &source, &destination);
        remap_map(&mut self.load_generations, &source, &destination);
        self.selection_anchor = self
            .selection_anchor
            .take()
            .map(|path| replace_path_prefix(&path, &source, &destination));
        self.selected_directory_path = self
            .selected_directory_path
            .take()
            .map(|path| replace_path_prefix(&path, &source, &destination));
        remap_map(&mut self.git_states, &source, &destination);
        remap_map(&mut self.pending, &source, &destination);
        for node in self.nodes.values_mut() {
            node.refresh_search_keys();
        }
        self.rebuild_children_index();
        self.rebuild_visible_rows();
    }

    pub fn delete_path(&mut self, path: &str) {
        let path = normalize_relative_path(path);
        self.nodes
            .retain(|candidate, _| !path_is_equal_or_descendant(candidate, &path));
        self.expanded_paths
            .retain(|candidate| !path_is_equal_or_descendant(candidate, &path));
        self.selected_paths
            .retain(|candidate| !path_is_equal_or_descendant(candidate, &path));
        self.git_states
            .retain(|candidate, _| !path_is_equal_or_descendant(candidate, &path));
        self.pending
            .retain(|candidate, _| !path_is_equal_or_descendant(candidate, &path));
        self.load_generations
            .retain(|candidate, _| !path_is_equal_or_descendant(candidate, &path));
        if self
            .selection_anchor
            .as_ref()
            .is_some_and(|candidate| path_is_equal_or_descendant(candidate, &path))
        {
            self.selection_anchor = None;
        }
        if self
            .selected_directory_path
            .as_ref()
            .is_some_and(|candidate| path_is_equal_or_descendant(candidate, &path))
        {
            self.selected_directory_path = None;
        }
        self.rebuild_children_index();
        self.rebuild_visible_rows();
    }

    pub fn visible_row_count(&self) -> usize {
        self.visible_rows.len()
    }

    pub fn visible_window(
        &self,
        start: usize,
        length: usize,
        overscan: usize,
    ) -> &[FileExplorerRow] {
        let start = start.saturating_sub(overscan).min(self.visible_rows.len());
        let end = start
            .saturating_add(length)
            .saturating_add(overscan.saturating_mul(2))
            .min(self.visible_rows.len());
        &self.visible_rows[start..end]
    }

    pub fn all_visible_rows(&self) -> &[FileExplorerRow] {
        &self.visible_rows
    }

    pub fn visible_row(&self, index: usize) -> Option<&FileExplorerRow> {
        self.visible_rows.get(index)
    }

    pub fn visible_row_position(&self, path: &str) -> Option<usize> {
        let path = normalize_relative_path(path);
        self.visible_rows
            .iter()
            .position(|row| row.path_chain.iter().any(|candidate| candidate == &path))
    }

    fn rebuild_children_index(&mut self) {
        self.children_by_parent.clear();
        for node in self.nodes.values_mut() {
            if node.lower_name.is_empty() || node.lower_path.is_empty() {
                node.refresh_search_keys();
            }
            self.children_by_parent
                .entry(node.entry.parent_path.clone().unwrap_or_default())
                .or_default()
                .push(node.entry.path.clone());
        }
        for siblings in self.children_by_parent.values_mut() {
            siblings.sort_by(|left, right| {
                let left = &self.nodes[left];
                let right = &self.nodes[right];
                let left_dir = left.entry.kind == FileEntryKind::Directory;
                let right_dir = right.entry.kind == FileEntryKind::Directory;
                right_dir
                    .cmp(&left_dir)
                    .then_with(|| left.lower_name.cmp(&right.lower_name))
                    .then_with(|| left.entry.path.cmp(&right.entry.path))
            });
        }
    }

    fn rebuild_visible_rows(&mut self) {
        if self.children_by_parent.is_empty() && !self.nodes.is_empty() {
            self.rebuild_children_index();
        }
        let query = self.query.clone();
        let matching_paths = (!query.is_empty()).then(|| matching_paths(&self.nodes, &query));
        let projected_git_states = git_states_with_ancestors(&self.git_states);
        let mut rows =
            Vec::with_capacity(self.nodes.len().saturating_add(1).min(FILE_TREE_MAX_ROWS));
        if self.workspace_id.is_some() {
            let root_name = if self.root_name.is_empty() {
                default_file_tree_root_name()
            } else {
                self.root_name.clone()
            };
            rows.push(FileExplorerRow {
                id: "file-row:workspace-root".to_string(),
                path: String::new(),
                name: root_name.clone(),
                depth: 0,
                kind: FileEntryKind::Directory,
                icon: file_icon_descriptor(&root_name, FileEntryKind::Directory),
                expanded: !self.root_collapsed,
                selected: self.selected_directory_path.as_deref() == Some(""),
                hidden: false,
                ignored: false,
                load_state: self.root_load_state.clone(),
                git: projected_git_states.get("").copied(),
                pending: None,
                match_ranges: substring_ranges(&root_name.to_lowercase(), &query),
                accessible_name: root_name.clone(),
                segments: vec![FileTreeSegment {
                    path: String::new(),
                    name: root_name,
                }],
                path_chain: vec![String::new()],
            });
        }
        if !self.root_collapsed {
            append_visible_children(
                "",
                1,
                &self.nodes,
                &self.children_by_parent,
                &self.expanded_paths,
                &self.selected_paths,
                self.selected_directory_path.as_deref(),
                &projected_git_states,
                &self.pending,
                &query,
                matching_paths.as_ref(),
                &mut rows,
            );
        }
        self.visible_rows = Arc::new(rows);
        self.presentation_revision = self.presentation_revision.saturating_add(1).max(1);
    }
}

#[allow(clippy::too_many_arguments)]
fn append_visible_children(
    parent: &str,
    depth: usize,
    nodes: &BTreeMap<String, FileTreeNode>,
    children: &BTreeMap<String, Vec<String>>,
    expanded: &BTreeSet<String>,
    selected: &BTreeSet<String>,
    selected_directory_path: Option<&str>,
    git_states: &BTreeMap<String, FileGitState>,
    pending: &BTreeMap<String, PendingFileMutation>,
    query: &str,
    matching_paths: Option<&BTreeSet<String>>,
    rows: &mut Vec<FileExplorerRow>,
) {
    let Some(child_paths) = children.get(parent) else {
        return;
    };
    for child_path in child_paths {
        if rows.len() >= FILE_TREE_MAX_ROWS {
            return;
        }
        if !nodes.contains_key(child_path) {
            continue;
        }
        if matching_paths.is_some_and(|paths| !paths.contains(child_path)) {
            continue;
        }
        let path_chain = compact_directory_chain(child_path, nodes, children, matching_paths);
        let visible_path = path_chain.last().unwrap_or(child_path);
        let Some(visible_node) = nodes.get(visible_path) else {
            continue;
        };
        let segments = path_chain
            .iter()
            .filter_map(|path| {
                nodes.get(path).map(|node| FileTreeSegment {
                    path: path.clone(),
                    name: node.entry.name.clone(),
                })
            })
            .collect::<Vec<_>>();
        let ignored = path_chain
            .iter()
            .filter_map(|path| nodes.get(path))
            .any(|node| node.entry.ignored);
        let hidden = path_chain
            .iter()
            .filter_map(|path| nodes.get(path))
            .any(|node| node.entry.hidden);
        let git = path_chain
            .iter()
            .filter_map(|path| git_states.get(path).copied())
            .reduce(merge_git_state)
            .or_else(|| {
                ignored.then_some(FileGitState {
                    signal: FileGitSignal::Ignored,
                    staged: false,
                    unstaged: false,
                })
            });
        let status = git
            .map(|state| format!(", Git {}", state.signal.short_label()))
            .unwrap_or_default();
        let display_name = segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        rows.push(FileExplorerRow {
            id: format!(
                "file-row:{}:{parent}:{}",
                parent.len(),
                visible_node.entry.path
            ),
            path: visible_node.entry.path.clone(),
            name: visible_node.entry.name.clone(),
            depth,
            kind: visible_node.entry.kind,
            icon: file_icon_descriptor(&visible_node.entry.name, visible_node.entry.kind),
            expanded: path_chain.iter().all(|path| expanded.contains(path)),
            selected: path_chain.iter().any(|path| selected.contains(path))
                || selected_directory_path
                    .is_some_and(|selected| path_chain.iter().any(|path| path == selected)),
            hidden,
            ignored,
            load_state: visible_node.load_state.clone(),
            git,
            pending: path_chain
                .iter()
                .find_map(|path| pending.get(path).cloned()),
            match_ranges: substring_ranges(&visible_node.lower_name, query),
            accessible_name: format!("{display_name}{status}"),
            segments,
            path_chain: path_chain.clone(),
        });

        let selected_chain_index = selected_directory_path
            .and_then(|selected| path_chain.iter().position(|path| path == selected));
        let children_root_path = selected_chain_index
            .and_then(|index| path_chain.get(index))
            .unwrap_or(visible_path);
        let expansion_path_chain = selected_chain_index
            .map(|index| &path_chain[..=index])
            .unwrap_or(path_chain.as_slice());
        if nodes
            .get(children_root_path)
            .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
            && (expansion_path_chain
                .iter()
                .all(|path| expanded.contains(path))
                || !query.is_empty())
        {
            append_visible_children(
                children_root_path,
                depth.saturating_add(1),
                nodes,
                children,
                expanded,
                selected,
                selected_directory_path,
                git_states,
                pending,
                query,
                matching_paths,
                rows,
            );
        }
    }
}

fn compact_directory_chain(
    start_path: &str,
    nodes: &BTreeMap<String, FileTreeNode>,
    children: &BTreeMap<String, Vec<String>>,
    matching_paths: Option<&BTreeSet<String>>,
) -> Vec<String> {
    let mut chain = vec![start_path.to_string()];
    let mut current_path = start_path;
    while let Some(current) = nodes.get(current_path) {
        if current.entry.kind != FileEntryKind::Directory {
            break;
        }
        let Some(child_paths) = children.get(current_path) else {
            break;
        };
        let visible_children = child_paths
            .iter()
            .filter(|path| matching_paths.is_none_or(|matching| matching.contains(*path)))
            .collect::<Vec<_>>();
        let [next_path] = visible_children.as_slice() else {
            break;
        };
        if !nodes
            .get(next_path.as_str())
            .is_some_and(|node| node.entry.kind == FileEntryKind::Directory)
        {
            break;
        }
        chain.push((*next_path).clone());
        current_path = next_path;
    }
    chain
}

fn git_states_with_ancestors(
    git_states: &BTreeMap<String, FileGitState>,
) -> BTreeMap<String, FileGitState> {
    let mut projected = BTreeMap::new();
    for (path, state) in git_states {
        let mut current = Some(path.as_str());
        while let Some(path) = current {
            projected
                .entry(path.to_string())
                .and_modify(|existing| *existing = merge_git_state(*existing, *state))
                .or_insert(*state);
            current = if path.is_empty() {
                None
            } else {
                path.rsplit_once('/').map(|(parent, _)| parent).or(Some(""))
            };
        }
    }
    projected
}

fn merge_git_state(left: FileGitState, right: FileGitState) -> FileGitState {
    FileGitState {
        signal: if left.signal == right.signal {
            left.signal
        } else {
            FileGitSignal::Modified
        },
        staged: left.staged || right.staged,
        unstaged: left.unstaged || right.unstaged,
    }
}

fn matching_paths(nodes: &BTreeMap<String, FileTreeNode>, query: &str) -> BTreeSet<String> {
    let mut matching = BTreeSet::new();
    for node in nodes.values() {
        if !node.lower_name.contains(query) && !node.lower_path.contains(query) {
            continue;
        }
        matching.insert(node.entry.path.clone());
        let mut parent = node.entry.parent_path.as_deref();
        while let Some(path) = parent {
            if path.is_empty() || !matching.insert(path.to_string()) {
                break;
            }
            parent = nodes
                .get(path)
                .and_then(|parent| parent.entry.parent_path.as_deref());
        }
    }
    matching
}

pub fn file_icon_descriptor(name: &str, kind: FileEntryKind) -> FileIconDescriptor {
    if kind == FileEntryKind::Directory {
        return FileIconDescriptor {
            kind: FileIconKind::Directory,
            tone: FileIconTone::Directory,
        };
    }
    if kind == FileEntryKind::Symlink {
        return FileIconDescriptor {
            kind: FileIconKind::Symlink,
            tone: FileIconTone::Neutral,
        };
    }
    if kind == FileEntryKind::Other {
        return FileIconDescriptor {
            kind: FileIconKind::Other,
            tone: FileIconTone::Neutral,
        };
    }
    let name = name.to_lowercase();
    if name.starts_with(".env") {
        return descriptor(FileIconKind::Secret, FileIconTone::Config);
    }
    if name.starts_with(".git") {
        return descriptor(FileIconKind::Config, FileIconTone::Config);
    }
    let named = match name.as_str() {
        ".dockerignore" | ".editorconfig" | ".eslintignore" | ".eslintrc" | ".npmrc"
        | ".prettierrc" | "cargo.toml" | "dockerfile" | "go.mod" | "tsconfig.json"
        | "vite.config.js" | "vite.config.mjs" | "vite.config.ts" => {
            Some(descriptor(FileIconKind::Config, FileIconTone::Config))
        }
        "bun.lock" | "cargo.lock" | "go.sum" | "package-lock.json" | "pnpm-lock.yaml"
        | "yarn.lock" => Some(descriptor(FileIconKind::Lock, FileIconTone::Config)),
        "justfile" | "makefile" => Some(descriptor(FileIconKind::Script, FileIconTone::Source)),
        "license" => Some(descriptor(FileIconKind::Text, FileIconTone::Document)),
        "readme" => Some(descriptor(FileIconKind::Markdown, FileIconTone::Document)),
        "package.json" => Some(descriptor(FileIconKind::Json, FileIconTone::Data)),
        _ => None,
    };
    if let Some(descriptor) = named {
        return descriptor;
    }
    let extension =
        if name.ends_with(".d.ts") || name.ends_with(".d.mts") || name.ends_with(".d.cts") {
            Some("ts")
        } else {
            name.rsplit_once('.').map(|(_, extension)| extension)
        };
    match extension {
        Some(
            "c" | "clj" | "cpp" | "cs" | "dart" | "erl" | "ex" | "exs" | "fs" | "fsx" | "go" | "h"
            | "hpp" | "kt" | "kts" | "lua" | "php" | "py" | "r" | "rb" | "scala" | "swift",
        ) => descriptor(FileIconKind::Code, FileIconTone::Source),
        Some("java") => descriptor(FileIconKind::Java, FileIconTone::Source),
        Some("rs") => descriptor(FileIconKind::Rust, FileIconTone::Source),
        Some("ts" | "tsx") => descriptor(FileIconKind::TypeScript, FileIconTone::Source),
        Some("js" | "jsx" | "mjs" | "cjs") => {
            descriptor(FileIconKind::JavaScript, FileIconTone::Source)
        }
        Some("json" | "json5" | "jsonc" | "jsonl") => {
            descriptor(FileIconKind::Json, FileIconTone::Data)
        }
        Some("md" | "mdx" | "markdown") => {
            descriptor(FileIconKind::Markdown, FileIconTone::Document)
        }
        Some(
            "png" | "jpg" | "jpeg" | "gif" | "webp" | "bmp" | "avif" | "heic" | "ico" | "tiff",
        ) => descriptor(FileIconKind::Image, FileIconTone::Media),
        Some("svg") => descriptor(FileIconKind::Svg, FileIconTone::Media),
        Some("aac" | "flac" | "m4a" | "mp3" | "ogg" | "wav") => {
            descriptor(FileIconKind::Audio, FileIconTone::Media)
        }
        Some("avi" | "mkv" | "mov" | "mp4" | "webm") => {
            descriptor(FileIconKind::Video, FileIconTone::Media)
        }
        Some("zip" | "gz" | "tar" | "7z" | "bz2" | "rar" | "tgz" | "xz") => {
            descriptor(FileIconKind::Archive, FileIconTone::Data)
        }
        Some("db" | "sqlite" | "sql") => descriptor(FileIconKind::Database, FileIconTone::Data),
        Some("csv" | "ods" | "xls" | "xlsx") => {
            descriptor(FileIconKind::Spreadsheet, FileIconTone::Data)
        }
        Some("otf" | "ttf" | "woff" | "woff2") => {
            descriptor(FileIconKind::Font, FileIconTone::Document)
        }
        Some("pdf") => descriptor(FileIconKind::Pdf, FileIconTone::Document),
        Some("doc" | "docx" | "ppt" | "pptx") => {
            descriptor(FileIconKind::Office, FileIconTone::Document)
        }
        Some("toml" | "yaml" | "yml" | "ini" | "env" | "conf") => {
            descriptor(FileIconKind::Config, FileIconTone::Config)
        }
        Some("lock") => descriptor(FileIconKind::Lock, FileIconTone::Config),
        Some("bash" | "bat" | "cmd" | "fish" | "ps1" | "sh" | "zsh") => {
            descriptor(FileIconKind::Script, FileIconTone::Source)
        }
        Some("css" | "less" | "sass" | "scss") => {
            descriptor(FileIconKind::Style, FileIconTone::Source)
        }
        Some("astro" | "htm" | "html" | "svelte" | "vue" | "xml") => {
            descriptor(FileIconKind::Markup, FileIconTone::Source)
        }
        Some("cer" | "crt" | "key" | "p12" | "pem") => {
            descriptor(FileIconKind::Secret, FileIconTone::Config)
        }
        Some("log" | "txt") => descriptor(FileIconKind::Text, FileIconTone::Document),
        _ => descriptor(FileIconKind::File, FileIconTone::Neutral),
    }
}

fn descriptor(kind: FileIconKind, tone: FileIconTone) -> FileIconDescriptor {
    FileIconDescriptor { kind, tone }
}

fn default_file_tree_root_name() -> String {
    "workspace".to_string()
}

fn normalize_relative_path(path: &str) -> String {
    path.trim()
        .replace('\\', "/")
        .trim_matches('/')
        .trim_start_matches("./")
        .to_string()
}

fn path_is_equal_or_descendant(candidate: &str, ancestor: &str) -> bool {
    ancestor.is_empty()
        || candidate == ancestor
        || candidate
            .strip_prefix(ancestor)
            .is_some_and(|suffix| suffix.starts_with('/'))
}

fn path_contains_git_directory(path: &str) -> bool {
    normalize_relative_path(path)
        .split('/')
        .any(|segment| segment == ".git")
}

fn replace_path_prefix(path: &str, source: &str, destination: &str) -> String {
    if path == source {
        destination.to_string()
    } else {
        path.strip_prefix(source)
            .filter(|suffix| suffix.starts_with('/'))
            .map(|suffix| format!("{destination}{suffix}"))
            .unwrap_or_else(|| path.to_string())
    }
}

fn remap_set(values: &mut BTreeSet<String>, source: &str, destination: &str) {
    *values = std::mem::take(values)
        .into_iter()
        .map(|path| replace_path_prefix(&path, source, destination))
        .collect();
}

fn remap_map<T>(values: &mut BTreeMap<String, T>, source: &str, destination: &str) {
    *values = std::mem::take(values)
        .into_iter()
        .map(|(path, value)| (replace_path_prefix(&path, source, destination), value))
        .collect();
}

fn parent_path(path: &str) -> Option<String> {
    path.rsplit_once('/').map(|(parent, _)| parent.to_string())
}

fn file_name(path: &str) -> String {
    path.rsplit('/').next().unwrap_or(path).to_string()
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

fn substring_ranges(value: &str, query: &str) -> Vec<Range<usize>> {
    if query.is_empty() {
        return Vec::new();
    }
    value
        .match_indices(query)
        .map(|(start, matched)| start..start + matched.len())
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(workspace_id: &WorkspaceId, path: &str, kind: FileEntryKind) -> FileTreeEntry {
        FileTreeEntry {
            workspace_id: workspace_id.clone(),
            path: path.to_string(),
            name: file_name(path),
            parent_path: parent_path(path),
            kind,
            size_bytes: None,
            modified_at_ms: None,
            hidden: false,
            ignored: false,
        }
    }

    #[test]
    fn stale_load_is_rejected_and_visible_rows_follow_expansion() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        assert!(!tree.apply_entries(&workspace_id, generation.saturating_sub(1), "", vec![],));
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/lib.rs", FileEntryKind::File),
                entry(&workspace_id, "README.md", FileEntryKind::File),
            ],
        ));
        assert_eq!(tree.visible_row_count(), 3);
        assert_eq!(tree.load_state("src"), FileTreeLoadState::Unloaded);
        assert!(tree.toggle_expanded("src"));
        assert_eq!(tree.visible_row_count(), 4);
        assert_eq!(tree.visible_window(2, 1, 0)[0].path, "src/lib.rs");
    }

    #[test]
    fn independent_subtree_loads_do_not_invalidate_each_other() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let root_generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            root_generation,
            "",
            vec![
                entry(&workspace_id, "a", FileEntryKind::Directory),
                entry(&workspace_id, "b", FileEntryKind::Directory),
            ],
        ));
        let a_generation = tree.begin_load("a");
        let b_generation = tree.begin_load("b");
        assert!(tree.apply_entries(
            &workspace_id,
            a_generation,
            "a",
            vec![entry(&workspace_id, "a/one.rs", FileEntryKind::File)],
        ));
        assert_eq!(tree.load_state("a"), FileTreeLoadState::Loaded);
        assert!(tree.apply_entries(
            &workspace_id,
            b_generation,
            "b",
            vec![entry(&workspace_id, "b/two.rs", FileEntryKind::File)],
        ));
        assert!(tree.toggle_expanded("a"));
        assert!(tree.toggle_expanded("b"));
        assert_eq!(tree.visible_row_count(), 5);
    }

    #[test]
    fn aggregate_refresh_preserves_interaction_state_and_reconciles_entries() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let root_generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            root_generation,
            "",
            vec![
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/keep.rs", FileEntryKind::File),
                entry(&workspace_id, "src/remove.rs", FileEntryKind::File),
            ],
        ));
        assert!(tree.toggle_expanded("src"));
        assert!(tree.select("src/keep.rs", false, false));
        tree.set_query("src");

        let refresh_generation = tree.begin_refresh();
        assert!(tree.apply_refresh_entries(
            &workspace_id,
            refresh_generation,
            vec![
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/keep.rs", FileEntryKind::File),
                entry(&workspace_id, "src/added.rs", FileEntryKind::File),
            ],
            &[],
        ));

        assert!(tree.is_expanded("src"));
        assert!(tree.selected_paths().contains("src/keep.rs"));
        assert_eq!(tree.query(), "src");
        assert!(tree.contains_path("src/added.rs"));
        assert!(!tree.contains_path("src/remove.rs"));
    }

    #[test]
    fn aggregate_refresh_keeps_cached_children_for_failed_existing_subtree() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let root_generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            root_generation,
            "",
            vec![
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/lib.rs", FileEntryKind::File),
            ],
        ));

        let refresh_generation = tree.begin_refresh();
        assert!(tree.apply_refresh_entries(
            &workspace_id,
            refresh_generation,
            vec![entry(&workspace_id, "src", FileEntryKind::Directory)],
            &["src".to_string()],
        ));
        assert!(tree.contains_path("src/lib.rs"));

        let stale_refresh = tree.begin_refresh();
        tree.invalidate_refresh();
        assert!(!tree.apply_refresh_entries(&workspace_id, stale_refresh, vec![], &[],));
    }

    #[test]
    fn rename_delete_selection_and_git_signal_stay_in_one_projection() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/lib.rs", FileEntryKind::File),
            ],
        );
        tree.toggle_expanded("src");
        tree.select("src/lib.rs", false, false);
        tree.set_git_changes(&[GitChange {
            path: "src/lib.rs".into(),
            original_path: None,
            kind: GitChangeKind::Modified,
            staged: false,
            unstaged: true,
            additions: 1,
            deletions: 1,
        }]);
        tree.move_path("src", "source");
        assert!(tree.selected_paths().contains("source/lib.rs"));
        let row = tree
            .all_visible_rows()
            .iter()
            .find(|row| row.path == "source/lib.rs")
            .unwrap();
        assert_eq!(row.git.unwrap().signal.short_label(), "M");
        tree.delete_path("source");
        assert!(tree.selected_paths().is_empty());
        assert_eq!(tree.visible_row_count(), 1);
    }

    #[test]
    fn icon_shape_and_tone_come_from_one_descriptor() {
        assert_eq!(
            file_icon_descriptor("README.md", FileEntryKind::File),
            FileIconDescriptor {
                kind: FileIconKind::Markdown,
                tone: FileIconTone::Document,
            }
        );
        assert_eq!(
            file_icon_descriptor("Main.java", FileEntryKind::File).kind,
            FileIconKind::Java
        );
        assert_eq!(
            file_icon_descriptor("report.xlsx", FileEntryKind::File).kind,
            FileIconKind::Spreadsheet
        );
        assert_eq!(
            file_icon_descriptor("app.js", FileEntryKind::File).kind,
            FileIconKind::JavaScript
        );
        assert_eq!(
            file_icon_descriptor("logo.svg", FileEntryKind::File),
            FileIconDescriptor {
                kind: FileIconKind::Svg,
                tone: FileIconTone::Media,
            }
        );
    }

    #[test]
    fn search_projects_only_matching_rows_and_their_ancestors() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        let mut entries = vec![
            entry(&workspace_id, "src", FileEntryKind::Directory),
            entry(&workspace_id, "src/generated", FileEntryKind::Directory),
        ];
        entries.extend((0..10_000).map(|index| {
            entry(
                &workspace_id,
                &format!("src/generated/file-{index:05}.rs"),
                FileEntryKind::File,
            )
        }));
        assert!(tree.apply_entries(&workspace_id, generation, "", entries));

        tree.set_query("file-09999");
        assert_eq!(
            tree.all_visible_rows()
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["", "src/generated", "src/generated/file-09999.rs"]
        );
    }

    #[test]
    fn workspace_root_and_single_directory_chains_match_the_desktop_tree_contract() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        tree.set_root_name("vibex");
        let generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, ".agents", FileEntryKind::Directory),
                entry(&workspace_id, ".agents/skills", FileEntryKind::Directory),
                entry(
                    &workspace_id,
                    ".agents/skills/example.md",
                    FileEntryKind::File,
                ),
                entry(&workspace_id, "src", FileEntryKind::Directory),
                entry(&workspace_id, "src/lib.rs", FileEntryKind::File),
                entry(&workspace_id, "src/main.rs", FileEntryKind::File),
            ],
        ));

        let root = tree.visible_row(0).unwrap();
        assert_eq!(root.name, "vibex");
        assert_eq!(root.path_chain, [""]);
        let compact = tree.visible_row(1).unwrap();
        assert_eq!(compact.path, ".agents/skills");
        assert_eq!(
            compact
                .segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>(),
            [".agents", "skills"]
        );

        assert!(tree.set_chain_expanded(&compact.path_chain.clone(), true));
        assert!(
            tree.all_visible_rows()
                .iter()
                .any(|row| row.path == ".agents/skills/example.md")
        );
        assert!(tree.toggle_expanded(""));
        assert_eq!(tree.visible_row_count(), 1);
    }

    #[test]
    fn selecting_a_compact_segment_temporarily_renders_from_that_directory() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, "a", FileEntryKind::Directory),
                entry(&workspace_id, "a/b", FileEntryKind::Directory),
                entry(&workspace_id, "a/b/c.rs", FileEntryKind::File),
            ],
        ));
        let chain = tree.visible_row(1).unwrap().path_chain.clone();
        assert!(tree.select_directory_segment("a", &chain));
        assert_eq!(tree.selected_directory_path(), Some("a"));
        assert_eq!(
            tree.all_visible_rows()
                .iter()
                .map(|row| row.path.as_str())
                .collect::<Vec<_>>(),
            ["", "a/b", "a/b"]
        );
        let visible_ids = tree
            .all_visible_rows()
            .iter()
            .map(|row| row.id.as_str())
            .collect::<BTreeSet<_>>();
        assert_eq!(visible_ids.len(), tree.visible_row_count());

        let lower_directory_chain = tree.visible_row(2).unwrap().path_chain.clone();
        assert_eq!(lower_directory_chain, ["a/b"]);
        assert!(tree.set_chain_expanded(&lower_directory_chain, true));
        assert!(
            tree.all_visible_rows()
                .iter()
                .any(|row| row.path == "a/b/c.rs")
        );
        assert!(tree.set_chain_expanded(&lower_directory_chain, false));
        assert!(
            tree.all_visible_rows()
                .iter()
                .all(|row| row.path != "a/b/c.rs")
        );
    }

    #[test]
    fn collapsing_a_directory_chain_clears_expanded_descendants() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, "a", FileEntryKind::Directory),
                entry(&workspace_id, "a/b", FileEntryKind::Directory),
                entry(&workspace_id, "a/b/c", FileEntryKind::Directory),
                entry(&workspace_id, "a/b/c/file.rs", FileEntryKind::File),
            ],
        ));
        assert!(tree.set_chain_expanded(
            &["a".to_string(), "a/b".to_string(), "a/b/c".to_string()],
            true,
        ));
        assert!(tree.set_chain_expanded(&["a".to_string()], false));
        assert!(!tree.is_expanded("a"));
        assert!(!tree.is_expanded("a/b"));
        assert!(!tree.is_expanded("a/b/c"));
    }

    #[test]
    fn git_internal_directory_is_excluded_without_hiding_dot_prefixed_siblings() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![
                entry(&workspace_id, ".git", FileEntryKind::Directory),
                entry(&workspace_id, ".git/config", FileEntryKind::File),
                entry(&workspace_id, ".github", FileEntryKind::Directory),
                entry(&workspace_id, ".github/workflows", FileEntryKind::Directory,),
            ],
        ));
        assert!(!tree.contains_path(".git"));
        assert!(!tree.contains_path(".git/config"));
        assert!(tree.contains_path(".github"));
        assert!(tree.contains_path(".github/workflows"));
    }

    #[test]
    fn unstaged_additions_use_the_untracked_visual_signal() {
        let workspace_id = WorkspaceId::new();
        let mut tree = FileTreeProjection::default();
        tree.reset_workspace(workspace_id.clone());
        let generation = tree.begin_load("");
        assert!(tree.apply_entries(
            &workspace_id,
            generation,
            "",
            vec![entry(&workspace_id, "new.rs", FileEntryKind::File)],
        ));
        tree.set_git_changes(&[GitChange {
            path: "new.rs".into(),
            original_path: None,
            kind: GitChangeKind::Added,
            staged: false,
            unstaged: true,
            additions: 1,
            deletions: 0,
        }]);
        assert_eq!(
            tree.visible_row(1)
                .and_then(|row| row.git)
                .map(|git| git.signal),
            Some(FileGitSignal::Untracked)
        );
    }
}

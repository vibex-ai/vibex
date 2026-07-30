use std::collections::BTreeSet;
use std::fs::{self, File, OpenOptions};
use std::io::{Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{SystemTime, UNIX_EPOCH};

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use sha2::{Digest as _, Sha256};
use vibex_core::{
    FileEncoding, FileEntryKind, FileLineEnding, FileMutationRequest, FilePreviewKind,
    FileReadRequest, FileReadResponse, FileSearchRequest, FileSearchResult, FileTreeEntry,
    FileTreeRequest, FileWriteRequest, VibexError, VibexResult, WorkspaceId, unix_timestamp_ms,
};

const DEFAULT_MAX_READ_BYTES: u64 = 1024 * 1024;
const MAX_SEARCH_RESULTS: usize = 200;
const MAX_TREE_ENTRIES: usize = 2000;
pub const MAX_NATIVE_TREE_ENTRIES: usize = 100_000;
const FILE_READ_CHUNK_BYTES: usize = 64 * 1024;

static ACTIVE_MUTATIONS: OnceLock<Mutex<BTreeSet<PathBuf>>> = OnceLock::new();
static TEMP_FILE_SEQUENCE: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(1);

struct FileMutationGuard {
    paths: Vec<PathBuf>,
}

impl FileMutationGuard {
    fn claim(paths: impl IntoIterator<Item = PathBuf>) -> VibexResult<Self> {
        let mut paths = paths.into_iter().collect::<Vec<_>>();
        paths.sort();
        paths.dedup();
        let active = ACTIVE_MUTATIONS.get_or_init(|| Mutex::new(BTreeSet::new()));
        let mut active = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if let Some(path) = paths.iter().find(|path| {
            active
                .iter()
                .any(|claimed| mutation_paths_overlap(path, claimed))
        }) {
            return Err(VibexError::conflict(
                "file_mutation_in_progress",
                "another file mutation is already in progress for this path",
            )
            .with_diagnostic("path", path.display().to_string()));
        }
        active.extend(paths.iter().cloned());
        drop(active);
        Ok(Self { paths })
    }
}

impl Drop for FileMutationGuard {
    fn drop(&mut self) {
        let active = ACTIVE_MUTATIONS.get_or_init(|| Mutex::new(BTreeSet::new()));
        let mut active = active
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        for path in &self.paths {
            active.remove(path);
        }
    }
}

pub struct WorkspaceFileService {
    root: PathBuf,
    workspace_id: WorkspaceId,
    ignore_matcher: Gitignore,
}

impl WorkspaceFileService {
    pub fn new(root: impl AsRef<Path>, workspace_id: WorkspaceId) -> VibexResult<Self> {
        let root = root.as_ref();
        if !root.exists() {
            return Err(VibexError::validation(
                "workspace_root_missing",
                "workspace root does not exist",
            )
            .with_diagnostic("path", root.display().to_string()));
        }
        if !root.is_dir() {
            return Err(VibexError::validation(
                "workspace_root_not_directory",
                "workspace root must be a directory",
            )
            .with_diagnostic("path", root.display().to_string()));
        }
        let root = root.canonicalize().map_err(|err| {
            VibexError::storage(
                "workspace_root_canonicalize_failed",
                "failed to resolve workspace root",
            )
            .with_diagnostic("path", root.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?;
        let ignore_matcher = build_workspace_ignore_matcher(&root)?;
        Ok(Self {
            root,
            workspace_id,
            ignore_matcher,
        })
    }

    pub fn list_tree(&self, request: &FileTreeRequest) -> VibexResult<Vec<FileTreeEntry>> {
        self.list_tree_with_limit(request, MAX_TREE_ENTRIES)
    }

    pub fn list_tree_with_limit(
        &self,
        request: &FileTreeRequest,
        max_entries: usize,
    ) -> VibexResult<Vec<FileTreeEntry>> {
        self.ensure_workspace(&request.workspace_id)?;
        let base = self.resolve_existing_dir(request.path.as_deref().unwrap_or(""))?;
        let base_rel = self.relative_path(&base)?;
        let max_depth = request.max_depth.unwrap_or(2).min(8);
        let max_entries = max_entries.clamp(1, MAX_NATIVE_TREE_ENTRIES);
        let mut entries = Vec::new();
        self.collect_tree(
            &base,
            base_rel.as_deref(),
            0,
            max_depth,
            request.include_hidden,
            max_entries,
            &mut entries,
        )?;
        entries.sort_by(|left, right| {
            let left_dir = left.kind == FileEntryKind::Directory;
            let right_dir = right.kind == FileEntryKind::Directory;
            right_dir
                .cmp(&left_dir)
                .then_with(|| left.path.to_lowercase().cmp(&right.path.to_lowercase()))
        });
        Ok(entries)
    }

    pub fn read_file(&self, request: &FileReadRequest) -> VibexResult<FileReadResponse> {
        self.ensure_workspace(&request.workspace_id)?;
        let path = self.resolve_existing_file(&request.path)?;
        let metadata = path_metadata(&path)?;
        let size_bytes = metadata.len();
        let max_bytes = request.max_bytes.unwrap_or(DEFAULT_MAX_READ_BYTES);
        let preview_kind = preview_kind(&path);
        let name = file_name(&path);
        let modified_at_ms = modified_at_ms(&metadata);
        let observation = read_file_observation(&path, max_bytes)?;

        if preview_kind == FilePreviewKind::Image || binary_preview_kind(&path) {
            return Ok(FileReadResponse {
                workspace_id: self.workspace_id.clone(),
                path: self.relative_path(&path)?.unwrap_or_default(),
                name,
                preview_kind: if preview_kind == FilePreviewKind::Image {
                    preview_kind
                } else {
                    FilePreviewKind::Binary
                },
                content: None,
                size_bytes,
                modified_at_ms,
                language: language_for_path(&path),
                truncated: false,
                encoding: FileEncoding::Binary,
                line_ending: FileLineEnding::None,
                content_revision: observation.revision,
            });
        }

        let truncated = size_bytes > max_bytes;
        let (preview_kind, content, encoding, line_ending) =
            match decode_utf8_prefix(&observation.prefix, truncated) {
                Some((text, encoding)) => {
                    (preview_kind, Some(text), encoding, observation.line_ending)
                }
                None => (
                    FilePreviewKind::Binary,
                    None,
                    FileEncoding::Binary,
                    FileLineEnding::None,
                ),
            };

        Ok(FileReadResponse {
            workspace_id: self.workspace_id.clone(),
            path: self.relative_path(&path)?.unwrap_or_default(),
            name,
            preview_kind,
            content,
            size_bytes,
            modified_at_ms,
            language: language_for_path(&path),
            truncated,
            encoding,
            line_ending,
            content_revision: observation.revision,
        })
    }

    pub fn write_file(&self, request: &FileWriteRequest) -> VibexResult<FileReadResponse> {
        self.ensure_workspace(&request.workspace_id)?;
        let path = self.resolve_for_write(&request.path)?;
        let _mutation = FileMutationGuard::claim([path.clone()])?;
        if path.exists() && path.is_dir() {
            return Err(VibexError::validation(
                "file_write_target_is_directory",
                "cannot write file content to a directory",
            ));
        }
        if path.exists()
            && fs::symlink_metadata(&path)
                .map(|metadata| metadata.file_type().is_symlink())
                .unwrap_or(false)
        {
            return Err(VibexError::validation(
                "file_write_symlink_rejected",
                "file writes do not replace symbolic links",
            ));
        }
        if !path.exists() && !request.create_if_missing {
            return Err(VibexError::validation(
                "file_write_target_missing",
                "file does not exist and createIfMissing is false",
            ));
        }
        if let Some(expected_revision) = request
            .expected_revision
            .as_deref()
            .map(str::trim)
            .filter(|revision| !revision.is_empty())
        {
            let current_revision = if path.exists() {
                file_revision(&path)?
            } else {
                String::new()
            };
            if current_revision != expected_revision {
                return Err(VibexError::conflict(
                    "file_external_revision_changed",
                    "file changed outside the editor; reload or compare before saving",
                )
                .with_diagnostic("expectedRevision", expected_revision)
                .with_diagnostic("currentRevision", current_revision));
            }
        }
        let bytes = encode_file_content(&request.content, request.encoding, request.line_ending)?;
        atomic_replace(&path, &bytes)?;
        self.read_file(&FileReadRequest {
            workspace_id: request.workspace_id.clone(),
            path: request.path.clone(),
            max_bytes: None,
        })
    }

    pub fn create_directory(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.ensure_workspace(&request.workspace_id)?;
        let path = self.resolve_for_write(&request.path)?;
        let _mutation = FileMutationGuard::claim([path.clone()])?;
        if path.exists() {
            if !request.overwrite {
                return Err(VibexError::conflict(
                    "file_create_directory_target_exists",
                    "directory target already exists",
                ));
            }
            if !path.is_dir() {
                return Err(VibexError::validation(
                    "file_create_directory_target_is_file",
                    "directory target is an existing file",
                ));
            }
            return self.entry_for_path(&path);
        }
        let create_result = if request.recursive {
            fs::create_dir_all(&path)
        } else {
            fs::create_dir(&path)
        };
        create_result.map_err(|err| {
            VibexError::storage("file_create_directory_failed", "failed to create directory")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        self.entry_for_path(&path)
    }

    pub fn delete_path(&self, request: &FileMutationRequest) -> VibexResult<()> {
        self.ensure_workspace(&request.workspace_id)?;
        let path = self.resolve_existing_entry(&request.path)?;
        let _mutation = FileMutationGuard::claim([path.clone()])?;
        let metadata = fs::symlink_metadata(&path).map_err(|err| {
            VibexError::storage("file_metadata_failed", "failed to read file metadata")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        if metadata.is_dir() {
            if request.recursive {
                fs::remove_dir_all(&path)
            } else {
                fs::remove_dir(&path)
            }
        } else {
            fs::remove_file(&path)
        }
        .map_err(|err| {
            VibexError::storage("file_delete_failed", "failed to delete path")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        Ok(())
    }

    pub fn copy_path(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.ensure_workspace(&request.workspace_id)?;
        let source = self.resolve_existing_entry(&request.path)?;
        let new_path = request.new_path.as_ref().ok_or_else(|| {
            VibexError::validation("file_copy_target_missing", "copy requires newPath")
        })?;
        let target = self.resolve_for_write(new_path)?;
        let _mutation = FileMutationGuard::claim([source.clone(), target.clone()])?;
        if source == target {
            return Err(VibexError::validation(
                "file_copy_target_same_as_source",
                "copy target must differ from source",
            ));
        }
        if target.exists() && !request.overwrite {
            return Err(VibexError::conflict(
                "file_copy_target_exists",
                "copy target already exists",
            ));
        }
        let source_metadata = fs::symlink_metadata(&source).map_err(|err| {
            VibexError::storage("file_metadata_failed", "failed to read file metadata")
                .with_diagnostic("path", source.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        if source_metadata.file_type().is_symlink() {
            return Err(VibexError::capability(
                "file_copy_symlink_unsupported",
                "copying symbolic links is not supported",
            ));
        }
        if source_metadata.is_dir() {
            if !request.recursive {
                return Err(VibexError::validation(
                    "file_copy_directory_requires_recursive",
                    "copying a directory requires recursive=true",
                ));
            }
            if target.starts_with(&source) {
                return Err(VibexError::validation(
                    "file_copy_target_inside_source",
                    "cannot copy a directory into itself",
                ));
            }
            copy_directory_recursive(&source, &target, request.overwrite)?;
        } else {
            copy_file(&source, &target, request.overwrite)?;
        }
        self.entry_for_path(&target)
    }

    pub fn rename_path(&self, request: &FileMutationRequest) -> VibexResult<FileTreeEntry> {
        self.ensure_workspace(&request.workspace_id)?;
        let source = self.resolve_existing_entry(&request.path)?;
        let new_path = request.new_path.as_ref().ok_or_else(|| {
            VibexError::validation("file_rename_target_missing", "rename requires newPath")
        })?;
        let target = self.resolve_for_write(new_path)?;
        let _mutation = FileMutationGuard::claim([source.clone(), target.clone()])?;
        if source == target {
            return Err(VibexError::validation(
                "file_rename_target_same_as_source",
                "rename target must differ from source",
            ));
        }
        if fs::symlink_metadata(&source)
            .map(|metadata| metadata.is_dir())
            .unwrap_or(false)
            && target.starts_with(&source)
        {
            return Err(VibexError::validation(
                "file_rename_target_inside_source",
                "cannot move a directory into itself",
            ));
        }
        if target.exists() && !request.overwrite {
            return Err(VibexError::conflict(
                "file_rename_target_exists",
                "rename target already exists",
            ));
        }
        fs::rename(&source, &target).map_err(|err| {
            VibexError::storage("file_rename_failed", "failed to rename path")
                .with_diagnostic("from", source.display().to_string())
                .with_diagnostic("to", target.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        self.entry_for_path(&target)
    }

    pub fn resolve_existing_path(
        &self,
        workspace_id: &WorkspaceId,
        relative: &str,
    ) -> VibexResult<PathBuf> {
        self.ensure_workspace(workspace_id)?;
        self.resolve_existing(relative)
    }

    pub fn read_bytes(
        &self,
        workspace_id: &WorkspaceId,
        relative: &str,
        max_bytes: usize,
    ) -> VibexResult<Vec<u8>> {
        self.ensure_workspace(workspace_id)?;
        let path = self.resolve_existing_file(relative)?;
        let metadata = path_metadata(&path)?;
        if metadata.len() > max_bytes as u64 {
            return Err(VibexError::capability(
                "file_binary_exceeds_limit",
                "file exceeds the bounded preview limit",
            )
            .with_diagnostic("sizeBytes", metadata.len().to_string())
            .with_diagnostic("limitBytes", max_bytes.to_string()));
        }
        let file = File::open(&path).map_err(|err| {
            VibexError::storage("file_read_failed", "failed to read file")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        let mut bytes = Vec::with_capacity(metadata.len() as usize);
        file.take(max_bytes.saturating_add(1) as u64)
            .read_to_end(&mut bytes)
            .map_err(|err| {
                VibexError::storage("file_read_failed", "failed to read file")
                    .with_diagnostic("path", path.display().to_string())
                    .with_diagnostic("error", err.to_string())
            })?;
        if bytes.len() > max_bytes {
            return Err(VibexError::capability(
                "file_binary_exceeds_limit",
                "file exceeds the bounded preview limit",
            ));
        }
        Ok(bytes)
    }

    pub fn search(&self, request: &FileSearchRequest) -> VibexResult<Vec<FileSearchResult>> {
        self.ensure_workspace(&request.workspace_id)?;
        let query = request.query.trim().to_lowercase();
        if query.is_empty() {
            return Err(VibexError::validation(
                "file_search_empty_query",
                "file search query must not be empty",
            ));
        }
        let limit = request
            .limit
            .unwrap_or(MAX_SEARCH_RESULTS as u32)
            .clamp(1, 500) as usize;
        let mut results = Vec::new();
        self.search_dir(
            &self.root,
            &query,
            request.include_content,
            limit,
            &mut results,
        )?;
        Ok(results)
    }

    fn collect_tree(
        &self,
        dir: &Path,
        parent_path: Option<&str>,
        depth: u32,
        max_depth: u32,
        include_hidden: bool,
        max_entries: usize,
        entries: &mut Vec<FileTreeEntry>,
    ) -> VibexResult<()> {
        if depth > max_depth || entries.len() >= max_entries {
            return Ok(());
        }
        let read_dir = fs::read_dir(dir).map_err(|err| {
            VibexError::storage("file_tree_read_failed", "failed to read directory")
                .with_diagnostic("path", dir.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        let mut directories = Vec::new();
        for entry in read_dir {
            if entries.len() >= max_entries {
                break;
            }
            let entry = entry.map_err(|err| {
                VibexError::storage("file_tree_entry_failed", "failed to read directory entry")
                    .with_diagnostic("error", err.to_string())
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if name == ".git" {
                continue;
            }
            if !include_hidden && is_hidden_name(&name) {
                continue;
            }
            let item =
                self.entry_for_path_with_parent(&path, parent_path.map(ToOwned::to_owned))?;
            let is_dir = item.kind == FileEntryKind::Directory;
            let ignored = item.ignored;
            let rel = item.path.clone();
            entries.push(item);
            if is_dir {
                directories.push((path, rel, ignored));
            }
        }

        if depth >= max_depth {
            return Ok(());
        }

        directories.sort_by(|left, right| {
            left.2
                .cmp(&right.2)
                .then_with(|| left.1.to_lowercase().cmp(&right.1.to_lowercase()))
        });

        for (path, rel, _) in directories {
            if entries.len() >= max_entries {
                break;
            }
            self.collect_tree(
                &path,
                Some(&rel),
                depth + 1,
                max_depth,
                include_hidden,
                max_entries,
                entries,
            )?;
        }
        Ok(())
    }

    fn search_dir(
        &self,
        dir: &Path,
        query: &str,
        include_content: bool,
        limit: usize,
        results: &mut Vec<FileSearchResult>,
    ) -> VibexResult<()> {
        if results.len() >= limit {
            return Ok(());
        }
        let read_dir = fs::read_dir(dir).map_err(|err| {
            VibexError::storage(
                "file_search_read_failed",
                "failed to read directory during search",
            )
            .with_diagnostic("path", dir.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?;
        for entry in read_dir {
            if results.len() >= limit {
                break;
            }
            let entry = entry.map_err(|err| {
                VibexError::storage("file_search_entry_failed", "failed to read search entry")
                    .with_diagnostic("error", err.to_string())
            })?;
            let path = entry.path();
            let name = entry.file_name().to_string_lossy().to_string();
            if is_hidden_name(&name) {
                continue;
            }
            let kind = kind_for_path(&path)?;
            if name.to_lowercase().contains(query) {
                results.push(FileSearchResult {
                    workspace_id: self.workspace_id.clone(),
                    path: self.relative_path(&path)?.unwrap_or_default(),
                    name: name.clone(),
                    kind,
                    line: None,
                    snippet: None,
                });
            }
            if kind == FileEntryKind::Directory {
                if name != ".git" {
                    self.search_dir(&path, query, include_content, limit, results)?;
                }
            } else if include_content && results.len() < limit {
                self.search_file_content(&path, query, results)?;
            }
        }
        Ok(())
    }

    fn search_file_content(
        &self,
        path: &Path,
        query: &str,
        results: &mut Vec<FileSearchResult>,
    ) -> VibexResult<()> {
        let metadata = path_metadata(path)?;
        if metadata.len() > DEFAULT_MAX_READ_BYTES
            || preview_kind(path) == FilePreviewKind::Image
            || binary_preview_kind(path)
        {
            return Ok(());
        }
        let Ok(content) = fs::read_to_string(path) else {
            return Ok(());
        };
        for (index, line) in content.lines().enumerate() {
            if line.to_lowercase().contains(query) {
                results.push(FileSearchResult {
                    workspace_id: self.workspace_id.clone(),
                    path: self.relative_path(path)?.unwrap_or_default(),
                    name: file_name(path),
                    kind: FileEntryKind::File,
                    line: Some((index + 1) as u32),
                    snippet: Some(line.trim().chars().take(240).collect()),
                });
                break;
            }
        }
        Ok(())
    }

    fn entry_for_path(&self, path: &Path) -> VibexResult<FileTreeEntry> {
        let parent = path
            .parent()
            .and_then(|parent| self.relative_path(parent).ok().flatten());
        self.entry_for_path_with_parent(path, parent)
    }

    fn entry_for_path_with_parent(
        &self,
        path: &Path,
        parent_path: Option<String>,
    ) -> VibexResult<FileTreeEntry> {
        let metadata = fs::symlink_metadata(path).map_err(|err| {
            VibexError::storage("file_metadata_failed", "failed to read file metadata")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        let name = file_name(path);
        let relative_path = self.relative_path(path)?.unwrap_or_default();
        let kind = kind_for_metadata(&metadata);
        Ok(FileTreeEntry {
            workspace_id: self.workspace_id.clone(),
            path: relative_path.clone(),
            name: name.clone(),
            parent_path,
            kind,
            size_bytes: if metadata.is_file() {
                Some(metadata.len())
            } else {
                None
            },
            modified_at_ms: modified_at_ms(&metadata),
            hidden: is_hidden_name(&name),
            ignored: is_git_internal_path(&relative_path)
                || self.is_gitignored(&relative_path, kind),
        })
    }

    fn is_gitignored(&self, relative_path: &str, kind: FileEntryKind) -> bool {
        if relative_path.is_empty() {
            return false;
        }
        self.ignore_matcher
            .matched_path_or_any_parents(relative_path, kind == FileEntryKind::Directory)
            .is_ignore()
    }

    fn ensure_workspace(&self, workspace_id: &WorkspaceId) -> VibexResult<()> {
        if workspace_id != &self.workspace_id {
            return Err(VibexError::validation(
                "workspace_mismatch",
                "file request workspace does not match service workspace",
            ));
        }
        Ok(())
    }

    fn resolve_existing(&self, relative: &str) -> VibexResult<PathBuf> {
        let path = self.resolve_child_path(relative)?;
        let canonical = path.canonicalize().map_err(|err| {
            VibexError::validation("file_path_missing", "path does not exist")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        self.ensure_inside_root(&canonical)?;
        Ok(canonical)
    }

    fn resolve_existing_entry(&self, relative: &str) -> VibexResult<PathBuf> {
        let path = self.resolve_child_path(relative)?;
        fs::symlink_metadata(&path).map_err(|err| {
            VibexError::validation("file_path_missing", "path does not exist")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        let parent = path.parent().ok_or_else(|| {
            VibexError::validation(
                "file_parent_missing",
                "workspace entry has no parent directory",
            )
        })?;
        let canonical_parent = parent.canonicalize().map_err(|err| {
            VibexError::validation("file_parent_missing", "parent directory does not exist")
                .with_diagnostic("path", parent.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        self.ensure_inside_root(&canonical_parent)?;
        if fs::symlink_metadata(&path)
            .map(|metadata| metadata.file_type().is_symlink())
            .unwrap_or(false)
        {
            let canonical_target = path.canonicalize().map_err(|err| {
                VibexError::validation(
                    "file_symlink_target_missing",
                    "symbolic link target does not exist",
                )
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
            })?;
            self.ensure_inside_root(&canonical_target)?;
        }
        Ok(path)
    }

    fn resolve_existing_dir(&self, relative: &str) -> VibexResult<PathBuf> {
        let path = self.resolve_existing(relative)?;
        if !path.is_dir() {
            return Err(VibexError::validation(
                "file_path_not_directory",
                "path is not a directory",
            ));
        }
        Ok(path)
    }

    fn resolve_existing_file(&self, relative: &str) -> VibexResult<PathBuf> {
        let path = self.resolve_existing(relative)?;
        if !path.is_file() {
            return Err(VibexError::validation(
                "file_path_not_file",
                "path is not a file",
            ));
        }
        Ok(path)
    }

    fn resolve_for_write(&self, relative: &str) -> VibexResult<PathBuf> {
        let path = self.resolve_child_path(relative)?;
        if let Some(parent) = path.parent() {
            let canonical_parent = parent.canonicalize().map_err(|err| {
                VibexError::validation("file_parent_missing", "parent directory does not exist")
                    .with_diagnostic("path", parent.display().to_string())
                    .with_diagnostic("error", err.to_string())
            })?;
            self.ensure_inside_root(&canonical_parent)?;
        }
        Ok(path)
    }

    fn resolve_child_path(&self, relative: &str) -> VibexResult<PathBuf> {
        validate_relative_path(relative)?;
        Ok(if relative.is_empty() {
            self.root.clone()
        } else {
            self.root.join(relative)
        })
    }

    fn relative_path(&self, path: &Path) -> VibexResult<Option<String>> {
        if path == self.root {
            return Ok(None);
        }
        self.ensure_inside_root(path)?;
        let rel = path.strip_prefix(&self.root).map_err(|err| {
            VibexError::storage(
                "file_relative_path_failed",
                "failed to compute relative path",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        Ok(Some(path_to_slash(rel)))
    }

    fn ensure_inside_root(&self, path: &Path) -> VibexResult<()> {
        if !path.starts_with(&self.root) {
            return Err(VibexError::validation(
                "file_path_outside_workspace",
                "path must stay inside the workspace root",
            )
            .with_diagnostic("path", path.display().to_string())
            .with_diagnostic("root", self.root.display().to_string()));
        }
        Ok(())
    }
}

fn build_workspace_ignore_matcher(root: &Path) -> VibexResult<Gitignore> {
    let mut builder = GitignoreBuilder::new(root);
    add_gitignore_file(&mut builder, &root.join(".gitignore"));
    add_gitignore_file(
        &mut builder,
        &root.join(".git").join("info").join("exclude"),
    );
    builder.build().map_err(|err| {
        VibexError::storage(
            "gitignore_matcher_failed",
            "failed to build gitignore matcher",
        )
        .with_diagnostic("root", root.display().to_string())
        .with_diagnostic("error", err.to_string())
    })
}

fn add_gitignore_file(builder: &mut GitignoreBuilder, path: &Path) {
    if path.is_file() {
        let _ = builder.add(path);
    }
}

fn is_git_internal_path(relative_path: &str) -> bool {
    Path::new(relative_path)
        .components()
        .any(|component| component.as_os_str() == ".git")
}

fn validate_relative_path(path: &str) -> VibexResult<()> {
    let path = Path::new(path);
    if path.is_absolute() {
        return Err(VibexError::validation(
            "absolute_child_path_rejected",
            "workspace child path must be relative",
        ));
    }
    for component in path.components() {
        match component {
            Component::CurDir | Component::Normal(_) => {}
            Component::ParentDir | Component::RootDir | Component::Prefix(_) => {
                return Err(VibexError::validation(
                    "path_traversal_rejected",
                    "workspace child path must not escape the workspace",
                ));
            }
        }
    }
    Ok(())
}

struct FileReadObservation {
    prefix: Vec<u8>,
    revision: String,
    line_ending: FileLineEnding,
}

fn read_file_observation(path: &Path, max_bytes: u64) -> VibexResult<FileReadObservation> {
    let mut file = File::open(path).map_err(|err| {
        VibexError::storage("file_read_failed", "failed to read file")
            .with_diagnostic("path", path.display().to_string())
            .with_diagnostic("error", err.to_string())
    })?;
    let prefix_limit = usize::try_from(max_bytes).unwrap_or(usize::MAX);
    let mut prefix = Vec::with_capacity(prefix_limit.min(FILE_READ_CHUNK_BYTES));
    let mut hasher = Sha256::new();
    let mut buffer = [0_u8; FILE_READ_CHUNK_BYTES];
    let mut lf = 0_u64;
    let mut crlf = 0_u64;
    let mut lone_cr = 0_u64;
    let mut pending_cr = false;
    loop {
        let read = file.read(&mut buffer).map_err(|err| {
            VibexError::storage("file_read_failed", "failed to read file")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        if read == 0 {
            break;
        }
        let chunk = &buffer[..read];
        hasher.update(chunk);
        if prefix.len() < prefix_limit {
            let remaining = prefix_limit - prefix.len();
            prefix.extend_from_slice(&chunk[..chunk.len().min(remaining)]);
        }
        for byte in chunk {
            if pending_cr {
                if *byte == b'\n' {
                    crlf = crlf.saturating_add(1);
                    pending_cr = false;
                    continue;
                }
                lone_cr = lone_cr.saturating_add(1);
                pending_cr = false;
            }
            match *byte {
                b'\r' => pending_cr = true,
                b'\n' => lf = lf.saturating_add(1),
                _ => {}
            }
        }
    }
    if pending_cr {
        lone_cr = lone_cr.saturating_add(1);
    }
    let line_ending = match (lf > 0, crlf > 0, lone_cr > 0) {
        (false, false, false) => FileLineEnding::None,
        (true, false, false) => FileLineEnding::Lf,
        (false, true, false) => FileLineEnding::Crlf,
        _ => FileLineEnding::Mixed,
    };
    Ok(FileReadObservation {
        prefix,
        revision: format!("sha256:{:x}", hasher.finalize()),
        line_ending,
    })
}

fn decode_utf8_prefix(bytes: &[u8], truncated: bool) -> Option<(String, FileEncoding)> {
    let (bytes, encoding) = bytes
        .strip_prefix(&[0xEF, 0xBB, 0xBF])
        .map(|bytes| (bytes, FileEncoding::Utf8Bom))
        .unwrap_or((bytes, FileEncoding::Utf8));
    let mut end = bytes.len();
    while std::str::from_utf8(&bytes[..end]).is_err() {
        if !truncated || bytes.len().saturating_sub(end) >= 4 || end == 0 {
            return None;
        }
        end -= 1;
    }
    std::str::from_utf8(&bytes[..end])
        .ok()
        .map(|text| (text.to_string(), encoding))
}

fn file_revision(path: &Path) -> VibexResult<String> {
    read_file_observation(path, 0).map(|observation| observation.revision)
}

fn encode_file_content(
    content: &str,
    encoding: Option<FileEncoding>,
    line_ending: Option<FileLineEnding>,
) -> VibexResult<Vec<u8>> {
    if encoding == Some(FileEncoding::Binary) {
        return Err(VibexError::validation(
            "file_write_binary_encoding_rejected",
            "binary files cannot be written through the text editor",
        ));
    }
    let normalized = normalize_text_line_endings(content);
    let text = match line_ending.unwrap_or(FileLineEnding::None) {
        FileLineEnding::Crlf => normalized.replace('\n', "\r\n"),
        FileLineEnding::None | FileLineEnding::Lf | FileLineEnding::Mixed => normalized,
    };
    let mut bytes = Vec::with_capacity(text.len().saturating_add(3));
    if encoding == Some(FileEncoding::Utf8Bom) {
        bytes.extend_from_slice(&[0xEF, 0xBB, 0xBF]);
    }
    bytes.extend_from_slice(text.as_bytes());
    Ok(bytes)
}

fn normalize_text_line_endings(content: &str) -> String {
    let mut normalized = String::with_capacity(content.len());
    let mut chars = content.chars().peekable();
    while let Some(ch) = chars.next() {
        if ch == '\r' {
            if chars.peek() == Some(&'\n') {
                chars.next();
            }
            normalized.push('\n');
        } else {
            normalized.push(ch);
        }
    }
    normalized
}

fn atomic_replace(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    let name = path
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| VibexError::validation("file_name_invalid", "file name is invalid"))?;
    let (temporary, mut file) = create_atomic_temp(path, name)?;
    let result = (|| {
        file.write_all(bytes).map_err(|err| {
            VibexError::storage(
                "file_atomic_temp_write_failed",
                "failed to write file content",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        file.sync_all().map_err(|err| {
            VibexError::storage(
                "file_atomic_temp_sync_failed",
                "failed to synchronize file content",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        drop(file);
        replace_file(&temporary, path).map_err(|err| {
            VibexError::storage("file_atomic_publish_failed", "failed to publish saved file")
                .with_diagnostic("path", path.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        sync_parent(path).map_err(|err| {
            VibexError::storage(
                "file_atomic_parent_sync_failed",
                "failed to synchronize the file directory",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        Ok(())
    })();
    if result.is_err() {
        let _ = fs::remove_file(&temporary);
    }
    result
}

fn create_atomic_temp(path: &Path, name: &str) -> VibexResult<(PathBuf, File)> {
    let timestamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    for attempt in 0..16_u64 {
        let sequence = TEMP_FILE_SEQUENCE.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        let temporary = path.with_file_name(format!(
            ".{name}.vibex-tmp-{}-{timestamp}-{sequence}-{attempt}",
            std::process::id()
        ));
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temporary)
        {
            Ok(file) => {
                if let Ok(metadata) = fs::metadata(path)
                    && let Err(error) = fs::set_permissions(&temporary, metadata.permissions())
                {
                    let _ = fs::remove_file(&temporary);
                    return Err(VibexError::storage(
                        "file_atomic_temp_permissions_failed",
                        "failed to preserve file permissions for atomic save",
                    )
                    .with_diagnostic("path", temporary.display().to_string())
                    .with_diagnostic("error", error.to_string()));
                }
                return Ok((temporary, file));
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(VibexError::storage(
                    "file_atomic_temp_create_failed",
                    "failed to create an atomic-write temporary file",
                )
                .with_diagnostic("path", temporary.display().to_string())
                .with_diagnostic("error", error.to_string()));
            }
        }
    }
    Err(VibexError::conflict(
        "file_atomic_temp_collision",
        "could not allocate an atomic-write temporary file",
    ))
}

fn mutation_paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(not(target_os = "windows"))]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    fs::rename(source, destination)
}

#[cfg(target_os = "windows")]
fn replace_file(source: &Path, destination: &Path) -> std::io::Result<()> {
    use std::os::windows::ffi::OsStrExt as _;
    use windows_sys::Win32::Storage::FileSystem::{
        MOVEFILE_REPLACE_EXISTING, MOVEFILE_WRITE_THROUGH, MoveFileExW,
    };
    let source = source
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let destination = destination
        .as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect::<Vec<_>>();
    let result = unsafe {
        MoveFileExW(
            source.as_ptr(),
            destination.as_ptr(),
            MOVEFILE_REPLACE_EXISTING | MOVEFILE_WRITE_THROUGH,
        )
    };
    if result == 0 {
        Err(std::io::Error::last_os_error())
    } else {
        Ok(())
    }
}

fn sync_parent(path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    if let Some(parent) = path.parent() {
        File::open(parent)?.sync_all()?;
    }
    Ok(())
}

fn path_metadata(path: &Path) -> VibexResult<fs::Metadata> {
    fs::metadata(path).map_err(|err| {
        VibexError::storage("file_metadata_failed", "failed to read file metadata")
            .with_diagnostic("path", path.display().to_string())
            .with_diagnostic("error", err.to_string())
    })
}

fn copy_file(source: &Path, target: &Path, overwrite: bool) -> VibexResult<()> {
    if target.exists() {
        if !overwrite {
            return Err(VibexError::conflict(
                "file_copy_target_exists",
                "copy target already exists",
            ));
        }
        if target.is_dir() {
            return Err(VibexError::validation(
                "file_copy_target_is_directory",
                "cannot overwrite a directory with a file",
            ));
        }
    }
    fs::copy(source, target).map_err(|err| {
        VibexError::storage("file_copy_failed", "failed to copy file")
            .with_diagnostic("from", source.display().to_string())
            .with_diagnostic("to", target.display().to_string())
            .with_diagnostic("error", err.to_string())
    })?;
    Ok(())
}

fn copy_directory_recursive(source: &Path, target: &Path, overwrite: bool) -> VibexResult<()> {
    if target.exists() {
        if !overwrite {
            return Err(VibexError::conflict(
                "file_copy_target_exists",
                "copy target already exists",
            ));
        }
        if !target.is_dir() {
            return Err(VibexError::validation(
                "file_copy_target_is_file",
                "cannot overwrite a file with a directory",
            ));
        }
    } else {
        fs::create_dir(target).map_err(|err| {
            VibexError::storage(
                "file_copy_directory_failed",
                "failed to create copied directory",
            )
            .with_diagnostic("path", target.display().to_string())
            .with_diagnostic("error", err.to_string())
        })?;
    }

    for entry in fs::read_dir(source).map_err(|err| {
        VibexError::storage(
            "file_copy_directory_read_failed",
            "failed to read source directory",
        )
        .with_diagnostic("path", source.display().to_string())
        .with_diagnostic("error", err.to_string())
    })? {
        let entry = entry.map_err(|err| {
            VibexError::storage(
                "file_copy_directory_entry_failed",
                "failed to read source directory entry",
            )
            .with_diagnostic("error", err.to_string())
        })?;
        let source_child = entry.path();
        let target_child = target.join(entry.file_name());
        let metadata = fs::symlink_metadata(&source_child).map_err(|err| {
            VibexError::storage("file_metadata_failed", "failed to read file metadata")
                .with_diagnostic("path", source_child.display().to_string())
                .with_diagnostic("error", err.to_string())
        })?;
        if metadata.file_type().is_symlink() {
            return Err(VibexError::capability(
                "file_copy_symlink_unsupported",
                "copying symbolic links is not supported",
            ));
        }
        if metadata.is_dir() {
            copy_directory_recursive(&source_child, &target_child, overwrite)?;
        } else {
            copy_file(&source_child, &target_child, overwrite)?;
        }
    }
    Ok(())
}

fn kind_for_path(path: &Path) -> VibexResult<FileEntryKind> {
    let metadata = fs::symlink_metadata(path).map_err(|err| {
        VibexError::storage("file_metadata_failed", "failed to read file metadata")
            .with_diagnostic("path", path.display().to_string())
            .with_diagnostic("error", err.to_string())
    })?;
    Ok(kind_for_metadata(&metadata))
}

fn kind_for_metadata(metadata: &fs::Metadata) -> FileEntryKind {
    let ty = metadata.file_type();
    if ty.is_dir() {
        FileEntryKind::Directory
    } else if ty.is_file() {
        FileEntryKind::File
    } else if ty.is_symlink() {
        FileEntryKind::Symlink
    } else {
        FileEntryKind::Other
    }
}

fn preview_kind(path: &Path) -> FilePreviewKind {
    match path
        .extension()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
    {
        "md" | "mdx" | "markdown" => FilePreviewKind::Markdown,
        "png" | "jpg" | "jpeg" | "gif" | "webp" | "svg" => FilePreviewKind::Image,
        _ => FilePreviewKind::Text,
    }
}

fn binary_preview_kind(path: &Path) -> bool {
    matches!(
        path.extension()
            .and_then(|value| value.to_str())
            .map(|value| value.to_ascii_lowercase())
            .as_deref(),
        Some(
            "aac"
                | "avi"
                | "doc"
                | "docx"
                | "flac"
                | "m4a"
                | "mkv"
                | "mov"
                | "mp3"
                | "mp4"
                | "ods"
                | "ogg"
                | "pdf"
                | "ppt"
                | "pptx"
                | "wav"
                | "webm"
                | "xls"
                | "xlsx"
        )
    )
}

fn language_for_path(path: &Path) -> Option<String> {
    let language = match path.extension().and_then(|value| value.to_str())? {
        "rs" => "rust",
        "ts" => "typescript",
        "tsx" => "typescriptreact",
        "js" => "javascript",
        "jsx" => "javascriptreact",
        "json" => "json",
        "md" | "mdx" => "markdown",
        "toml" => "toml",
        "yaml" | "yml" => "yaml",
        "css" => "css",
        "html" => "html",
        "py" => "python",
        "go" => "go",
        "java" => "java",
        "kt" => "kotlin",
        "sh" | "bash" => "shell",
        _ => return None,
    };
    Some(language.to_string())
}

fn modified_at_ms(metadata: &fs::Metadata) -> Option<i64> {
    metadata
        .modified()
        .ok()
        .and_then(|time| time.duration_since(UNIX_EPOCH).ok())
        .map(|duration| duration.as_millis() as i64)
}

fn file_name(path: &Path) -> String {
    path.file_name()
        .and_then(|value| value.to_str())
        .unwrap_or_default()
        .to_string()
}

fn is_hidden_name(name: &str) -> bool {
    name.starts_with('.')
}

fn path_to_slash(path: &Path) -> String {
    path.components()
        .map(|component| component.as_os_str().to_string_lossy())
        .collect::<Vec<_>>()
        .join("/")
}

#[derive(Debug, Clone, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FileSmokeResult {
    pub workspace_root: PathBuf,
    pub listed_entries: usize,
    pub read_path: String,
    pub search_hits: usize,
    #[serde(rename = "timestampMs")]
    pub timestamp_ms: i64,
}

pub fn run_files_smoke(root: impl AsRef<Path>) -> VibexResult<FileSmokeResult> {
    let root = root.as_ref();
    fs::create_dir_all(root).map_err(|err| {
        VibexError::storage(
            "files_smoke_root_failed",
            "failed to create files smoke root",
        )
        .with_diagnostic("path", root.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    let workspace_id = WorkspaceId::new();
    let service = WorkspaceFileService::new(root, workspace_id.clone())?;
    let marker_path = "vibex-files-smoke.txt";
    let content = format!("vibex files smoke {}", unix_timestamp_ms());
    let written = service.write_file(&FileWriteRequest {
        workspace_id: workspace_id.clone(),
        path: marker_path.to_string(),
        content: content.clone(),
        create_if_missing: true,
        expected_revision: None,
        encoding: None,
        line_ending: None,
    })?;
    let entries = service.list_tree(&FileTreeRequest {
        workspace_id: workspace_id.clone(),
        path: None,
        max_depth: Some(1),
        include_hidden: false,
    })?;
    let read = service.read_file(&FileReadRequest {
        workspace_id: workspace_id.clone(),
        path: marker_path.to_string(),
        max_bytes: None,
    })?;
    if read.content.as_deref() != Some(content.as_str()) {
        return Err(VibexError::storage(
            "files_smoke_mismatch",
            "file smoke content did not round-trip",
        ));
    }
    let hits = service.search(&FileSearchRequest {
        workspace_id,
        query: "vibex-files-smoke".to_string(),
        include_content: false,
        limit: Some(10),
    })?;
    Ok(FileSmokeResult {
        workspace_root: root.to_path_buf(),
        listed_entries: entries.len(),
        read_path: written.path,
        search_hits: hits.len(),
        timestamp_ms: unix_timestamp_ms(),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_path_traversal() {
        let root = temp_root("traversal");
        fs::create_dir_all(&root).unwrap();
        let service = WorkspaceFileService::new(&root, WorkspaceId::new()).unwrap();
        let err = service.resolve_existing("../outside").unwrap_err();
        assert_eq!(err.code, "path_traversal_rejected");
        cleanup(root);
    }

    #[test]
    fn reads_writes_and_searches_text() {
        let root = temp_root("roundtrip");
        fs::create_dir_all(&root).unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "src/main.rs".to_string(),
                content: "fn main() {}\n".to_string(),
                create_if_missing: true,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap_err();
        fs::create_dir_all(root.join("src")).unwrap();
        service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "src/main.rs".to_string(),
                content: "fn main() {}\n".to_string(),
                create_if_missing: true,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap();
        let read = service
            .read_file(&FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "src/main.rs".to_string(),
                max_bytes: None,
            })
            .unwrap();
        assert_eq!(read.language.as_deref(), Some("rust"));
        assert_eq!(read.content.as_deref(), Some("fn main() {}\n"));
        service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "src/App.tsx".to_string(),
                content: "export function App() { return <main />; }\n".to_string(),
                create_if_missing: true,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap();
        let tsx = service
            .read_file(&FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "src/App.tsx".to_string(),
                max_bytes: None,
            })
            .unwrap();
        assert_eq!(tsx.language.as_deref(), Some("typescriptreact"));
        let hits = service
            .search(&FileSearchRequest {
                workspace_id,
                query: "main.rs".to_string(),
                include_content: false,
                limit: Some(10),
            })
            .unwrap();
        assert_eq!(hits.len(), 1);
        cleanup(root);
    }

    #[test]
    fn preserves_utf8_bom_and_crlf_and_rejects_a_stale_revision() {
        let root = temp_root("revision-crlf-bom");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("notes.txt"), b"\xef\xbb\xbfone\r\ntwo\r\n").unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let first = service
            .read_file(&FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "notes.txt".into(),
                max_bytes: None,
            })
            .unwrap();
        assert_eq!(first.encoding, FileEncoding::Utf8Bom);
        assert_eq!(first.line_ending, FileLineEnding::Crlf);

        fs::write(root.join("notes.txt"), b"external\r\n").unwrap();
        let error = service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "notes.txt".into(),
                content: "local\n".into(),
                create_if_missing: false,
                expected_revision: Some(first.content_revision),
                encoding: Some(FileEncoding::Utf8Bom),
                line_ending: Some(FileLineEnding::Crlf),
            })
            .unwrap_err();
        assert_eq!(error.code, "file_external_revision_changed");

        let external = service
            .read_file(&FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "notes.txt".into(),
                max_bytes: None,
            })
            .unwrap();
        let saved = service
            .write_file(&FileWriteRequest {
                workspace_id,
                path: "notes.txt".into(),
                content: "local\nnext\n".into(),
                create_if_missing: false,
                expected_revision: Some(external.content_revision),
                encoding: Some(FileEncoding::Utf8Bom),
                line_ending: Some(FileLineEnding::Crlf),
            })
            .unwrap();
        assert_eq!(saved.encoding, FileEncoding::Utf8Bom);
        assert_eq!(saved.line_ending, FileLineEnding::Crlf);
        assert_eq!(
            fs::read(root.join("notes.txt")).unwrap(),
            b"\xef\xbb\xbflocal\r\nnext\r\n"
        );
        cleanup(root);
    }

    #[test]
    fn duplicate_mutation_claim_is_rejected_at_the_service_boundary() {
        let root = temp_root("mutation-claim");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("a.txt"), "one\n").unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let _claim = FileMutationGuard::claim([root.clone()]).unwrap();
        let error = service
            .write_file(&FileWriteRequest {
                workspace_id,
                path: "a.txt".into(),
                content: "two\n".into(),
                create_if_missing: false,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap_err();
        assert_eq!(error.code, "file_mutation_in_progress");
        cleanup(root);
    }

    #[cfg(unix)]
    #[test]
    fn atomic_save_preserves_executable_permissions() {
        use std::os::unix::fs::PermissionsExt as _;

        let root = temp_root("atomic-permissions");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("script.sh");
        fs::write(&path, "#!/bin/sh\necho old\n").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let read = service
            .read_file(&FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "script.sh".into(),
                max_bytes: None,
            })
            .unwrap();
        service
            .write_file(&FileWriteRequest {
                workspace_id,
                path: "script.sh".into(),
                content: "#!/bin/sh\necho new\n".into(),
                create_if_missing: false,
                expected_revision: Some(read.content_revision),
                encoding: Some(FileEncoding::Utf8),
                line_ending: Some(FileLineEnding::Lf),
            })
            .unwrap();
        assert_eq!(
            fs::metadata(&path).unwrap().permissions().mode() & 0o777,
            0o755
        );
        cleanup(root);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_writes_and_copies_are_rejected_without_touching_the_target() {
        use std::os::unix::fs::symlink;

        let root = temp_root("symlink-mutation");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("target.txt"), "target\n").unwrap();
        symlink("target.txt", root.join("link.txt")).unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let write_error = service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "link.txt".into(),
                content: "replacement\n".into(),
                create_if_missing: false,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap_err();
        assert_eq!(write_error.code, "file_write_symlink_rejected");
        let copy_error = service
            .copy_path(&FileMutationRequest {
                workspace_id,
                path: "link.txt".into(),
                new_path: Some("copy.txt".into()),
                recursive: false,
                overwrite: false,
            })
            .unwrap_err();
        assert_eq!(copy_error.code, "file_copy_symlink_unsupported");
        assert_eq!(
            fs::read_to_string(root.join("target.txt")).unwrap(),
            "target\n"
        );
        cleanup(root);
    }

    #[test]
    fn creates_directories_and_copies_paths() {
        let root = temp_root("create-copy");
        fs::create_dir_all(&root).unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();

        let directory = service
            .create_directory(&FileMutationRequest {
                workspace_id: workspace_id.clone(),
                path: "docs".to_string(),
                new_path: None,
                recursive: false,
                overwrite: false,
            })
            .unwrap();
        assert_eq!(directory.kind, FileEntryKind::Directory);
        assert_eq!(directory.path, "docs");

        service
            .write_file(&FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "docs/readme.md".to_string(),
                content: "# Docs\n".to_string(),
                create_if_missing: true,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            })
            .unwrap();
        let copied = service
            .copy_path(&FileMutationRequest {
                workspace_id: workspace_id.clone(),
                path: "docs/readme.md".to_string(),
                new_path: Some("docs/copy.md".to_string()),
                recursive: false,
                overwrite: false,
            })
            .unwrap();
        assert_eq!(copied.path, "docs/copy.md");
        assert_eq!(
            fs::read_to_string(root.join("docs").join("copy.md")).unwrap(),
            "# Docs\n"
        );

        let copied_dir = service
            .copy_path(&FileMutationRequest {
                workspace_id,
                path: "docs".to_string(),
                new_path: Some("docs-copy".to_string()),
                recursive: true,
                overwrite: false,
            })
            .unwrap();
        assert_eq!(copied_dir.kind, FileEntryKind::Directory);
        assert!(root.join("docs-copy").join("readme.md").is_file());
        cleanup(root);
    }

    #[test]
    fn rejects_copying_directory_into_itself() {
        let root = temp_root("copy-self");
        fs::create_dir_all(root.join("docs")).unwrap();
        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();

        let err = service
            .copy_path(&FileMutationRequest {
                workspace_id,
                path: "docs".to_string(),
                new_path: Some("docs/nested".to_string()),
                recursive: true,
                overwrite: false,
            })
            .unwrap_err();
        assert_eq!(err.code, "file_copy_target_inside_source");
        cleanup(root);
    }

    #[test]
    fn marks_gitignored_files_and_directories() {
        let root = temp_root("gitignored");
        fs::create_dir_all(root.join("src")).unwrap();
        fs::create_dir_all(root.join("target")).unwrap();
        fs::write(root.join(".gitignore"), "target/\n*.log\n").unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();
        fs::write(root.join("target").join("build.log"), "ignored\n").unwrap();
        fs::write(root.join("debug.log"), "ignored\n").unwrap();

        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let entries = service
            .list_tree(&FileTreeRequest {
                workspace_id,
                path: None,
                max_depth: Some(2),
                include_hidden: false,
            })
            .unwrap();

        assert!(!entry_by_path(&entries, "src").ignored);
        assert!(!entry_by_path(&entries, "src/main.rs").ignored);
        assert!(entry_by_path(&entries, "target").ignored);
        assert!(entry_by_path(&entries, "target/build.log").ignored);
        assert!(entry_by_path(&entries, "debug.log").ignored);
        cleanup(root);
    }

    #[test]
    fn hides_git_directories_from_tree() {
        let root = temp_root("hide-git");
        fs::create_dir_all(root.join(".git").join("objects")).unwrap();
        fs::create_dir_all(root.join("src")).unwrap();
        fs::write(root.join(".git").join("HEAD"), "ref: refs/heads/main\n").unwrap();
        fs::write(root.join("src").join("main.rs"), "fn main() {}\n").unwrap();

        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let entries = service
            .list_tree(&FileTreeRequest {
                workspace_id,
                path: None,
                max_depth: Some(2),
                include_hidden: true,
            })
            .unwrap();

        assert!(entries.iter().all(|entry| {
            !Path::new(&entry.path)
                .components()
                .any(|component| component.as_os_str() == ".git")
        }));
        assert!(entries.iter().any(|entry| entry.path == "src/main.rs"));
        cleanup(root);
    }

    #[test]
    fn prioritizes_normal_directory_children_before_large_ignored_subtree() {
        let root = temp_root("ignored-subtree-budget");
        let ignored_dir = root.join("node_modules");
        let source_dir = root.join("src");
        fs::create_dir_all(&ignored_dir).unwrap();
        fs::create_dir_all(&source_dir).unwrap();
        fs::write(root.join(".gitignore"), "node_modules/\n").unwrap();
        fs::write(source_dir.join("main.rs"), "fn main() {}\n").unwrap();
        for index in 0..(MAX_TREE_ENTRIES + 16) {
            fs::write(ignored_dir.join(format!("package-{index:04}.json")), "{}\n").unwrap();
        }

        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let entries = service
            .list_tree(&FileTreeRequest {
                workspace_id,
                path: None,
                max_depth: Some(2),
                include_hidden: true,
            })
            .unwrap();

        assert!(entry_by_path(&entries, "node_modules").ignored);
        assert!(!entry_by_path(&entries, "src").ignored);
        assert!(!entry_by_path(&entries, "src/main.rs").ignored);
        cleanup(root);
    }

    #[test]
    fn keeps_root_siblings_when_large_child_directory_hits_entry_limit() {
        let root = temp_root("root-siblings");
        let large_dir = root.join("aaa-large");
        fs::create_dir_all(&large_dir).unwrap();
        fs::write(root.join("zzz-root.txt"), "root\n").unwrap();
        for index in 0..(MAX_TREE_ENTRIES + 16) {
            fs::write(large_dir.join(format!("entry-{index:04}.txt")), "nested\n").unwrap();
        }

        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let entries = service
            .list_tree(&FileTreeRequest {
                workspace_id,
                path: None,
                max_depth: Some(2),
                include_hidden: true,
            })
            .unwrap();

        assert!(entries.len() <= MAX_TREE_ENTRIES);
        assert!(entries.iter().any(|entry| entry.path == "aaa-large"));
        assert!(entries.iter().any(|entry| entry.path == "zzz-root.txt"));
        cleanup(root);
    }

    #[test]
    fn lists_directory_subtree_when_root_scan_budget_was_exhausted() {
        let root = temp_root("subtree-budget");
        let large_dir = root.join("aaa-large");
        let late_dir = root.join("zzz-late");
        fs::create_dir_all(&large_dir).unwrap();
        fs::create_dir_all(&late_dir).unwrap();
        fs::write(late_dir.join("child.rs"), "fn child() {}\n").unwrap();
        for index in 0..(MAX_TREE_ENTRIES + 16) {
            fs::write(large_dir.join(format!("entry-{index:04}.txt")), "nested\n").unwrap();
        }

        let workspace_id = WorkspaceId::new();
        let service = WorkspaceFileService::new(&root, workspace_id.clone()).unwrap();
        let root_entries = service
            .list_tree(&FileTreeRequest {
                workspace_id: workspace_id.clone(),
                path: None,
                max_depth: Some(2),
                include_hidden: true,
            })
            .unwrap();
        let subtree_entries = service
            .list_tree(&FileTreeRequest {
                workspace_id,
                path: Some("zzz-late".to_string()),
                max_depth: Some(2),
                include_hidden: true,
            })
            .unwrap();

        assert!(root_entries.iter().any(|entry| entry.path == "zzz-late"));
        assert!(
            subtree_entries
                .iter()
                .any(|entry| entry.path == "zzz-late/child.rs")
        );
        cleanup(root);
    }

    fn temp_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-fs-{label}-{}",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn entry_by_path<'a>(entries: &'a [FileTreeEntry], path: &str) -> &'a FileTreeEntry {
        entries
            .iter()
            .find(|entry| entry.path == path)
            .unwrap_or_else(|| panic!("missing file tree entry: {path}"))
    }

    fn cleanup(path: PathBuf) {
        let _ = fs::remove_dir_all(path);
    }
}

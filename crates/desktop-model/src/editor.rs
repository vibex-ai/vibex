use std::{collections::BTreeMap, sync::Arc};

use serde::{Deserialize, Serialize};
use vibex_core::{
    FileEncoding, FileLineEnding, FilePreviewKind, FileReadResponse, FileWriteRequest, WorkspaceId,
};

pub const EDITOR_MAX_EDITABLE_BYTES: u64 = 8 * 1024 * 1024;
pub const EDITOR_MAX_EDITABLE_LINES: usize = 50_000;
pub const EDITOR_RECOVERY_BUFFER_LIMIT: usize = 32;
pub const EDITOR_RECOVERY_TOTAL_BYTES: usize = 8 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorBufferAvailability {
    Ready,
    LargeFileReadOnly,
    BinaryReadOnly,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EditorExternalState {
    Current,
    VerificationRequired,
    Changed { revision: String },
    Deleted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorPendingSave {
    pub request_id: u64,
    pub local_revision: u64,
    pub content: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorSaveTicket {
    pub request_id: u64,
    pub path: String,
    pub local_revision: u64,
    pub expected_revision: String,
    pub content: String,
    pub encoding: FileEncoding,
    pub line_ending: FileLineEnding,
}

impl EditorSaveTicket {
    pub fn into_request(self, workspace_id: WorkspaceId) -> FileWriteRequest {
        FileWriteRequest {
            workspace_id,
            path: self.path,
            content: self.content,
            create_if_missing: false,
            expected_revision: Some(self.expected_revision),
            encoding: Some(self.encoding),
            line_ending: Some(self.line_ending),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EditorCloseGuard {
    Clean,
    Dirty,
    SavePending,
    ExternalConflict,
    DirtyMissingFile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorBufferModel {
    pub path: String,
    pub encoding: FileEncoding,
    pub line_ending: FileLineEnding,
    pub language: Option<String>,
    pub content: String,
    pub saved_content: String,
    pub saved_revision: String,
    pub local_revision: u64,
    pub dirty: bool,
    pub external: EditorExternalState,
    pub pending_save: Option<EditorPendingSave>,
    pub availability: EditorBufferAvailability,
    pub size_bytes: u64,
    pub truncated: bool,
    pub last_error_code: Option<String>,
}

impl EditorBufferModel {
    pub fn from_read(file: FileReadResponse) -> Self {
        let content = normalize_line_endings(file.content.as_deref().unwrap_or_default());
        let line_count = content.lines().count().max(1);
        let availability = if file.preview_kind == FilePreviewKind::Binary
            || file.encoding == FileEncoding::Binary
        {
            EditorBufferAvailability::BinaryReadOnly
        } else if file.truncated
            || file.size_bytes > EDITOR_MAX_EDITABLE_BYTES
            || line_count > EDITOR_MAX_EDITABLE_LINES
        {
            EditorBufferAvailability::LargeFileReadOnly
        } else {
            EditorBufferAvailability::Ready
        };
        Self {
            path: normalize_path(&file.path),
            encoding: file.encoding,
            line_ending: file.line_ending,
            language: file.language,
            content: content.clone(),
            saved_content: content,
            saved_revision: file.content_revision,
            local_revision: 1,
            dirty: false,
            external: EditorExternalState::Current,
            pending_save: None,
            availability,
            size_bytes: file.size_bytes,
            truncated: file.truncated,
            last_error_code: None,
        }
    }

    pub fn editable(&self) -> bool {
        self.availability == EditorBufferAvailability::Ready
            && !matches!(self.external, EditorExternalState::Deleted)
    }

    pub fn update_content(&mut self, content: impl Into<String>) -> bool {
        if !self.editable() {
            return false;
        }
        let content = normalize_line_endings(&content.into());
        if self.content == content {
            return false;
        }
        self.content = content;
        self.local_revision = self.local_revision.saturating_add(1).max(1);
        self.dirty = self.content != self.saved_content;
        self.last_error_code = None;
        true
    }

    pub fn begin_save(&mut self, request_id: u64) -> Option<EditorSaveTicket> {
        if !self.editable()
            || !self.dirty
            || self.pending_save.is_some()
            || matches!(
                self.external,
                EditorExternalState::Changed { .. }
                    | EditorExternalState::Deleted
                    | EditorExternalState::VerificationRequired
            )
        {
            return None;
        }
        let ticket = EditorSaveTicket {
            request_id,
            path: self.path.clone(),
            local_revision: self.local_revision,
            expected_revision: self.saved_revision.clone(),
            content: self.content.clone(),
            encoding: self.encoding,
            line_ending: self.line_ending,
        };
        self.pending_save = Some(EditorPendingSave {
            request_id,
            local_revision: ticket.local_revision,
            content: ticket.content.clone(),
        });
        self.last_error_code = None;
        Some(ticket)
    }

    pub fn finish_save(&mut self, request_id: u64, file: FileReadResponse) -> bool {
        let Some(pending) = self.pending_save.take() else {
            return false;
        };
        if pending.request_id != request_id || normalize_path(&file.path) != self.path {
            self.pending_save = Some(pending);
            return false;
        }
        self.saved_content = pending.content;
        self.saved_revision = file.content_revision;
        self.encoding = file.encoding;
        self.line_ending = file.line_ending;
        self.size_bytes = file.size_bytes;
        self.truncated = file.truncated;
        self.external = EditorExternalState::Current;
        self.dirty = self.content != self.saved_content;
        self.last_error_code = None;
        true
    }

    pub fn fail_save(&mut self, request_id: u64, error_code: &str) -> bool {
        if self
            .pending_save
            .as_ref()
            .is_none_or(|pending| pending.request_id != request_id)
        {
            return false;
        }
        self.pending_save = None;
        self.last_error_code = Some(bounded_text(error_code, 120));
        self.dirty = self.content != self.saved_content;
        true
    }

    pub fn observe_external(&mut self, file: FileReadResponse) -> bool {
        if normalize_path(&file.path) != self.path {
            return false;
        }
        if file.content_revision == self.saved_revision {
            self.external = EditorExternalState::Current;
            return false;
        }
        if self.dirty || self.pending_save.is_some() {
            self.external = EditorExternalState::Changed {
                revision: file.content_revision,
            };
            return true;
        }
        *self = Self::from_read(file);
        true
    }

    pub fn accept_external(&mut self, file: FileReadResponse) -> bool {
        if normalize_path(&file.path) != self.path || self.pending_save.is_some() {
            return false;
        }
        *self = Self::from_read(file);
        true
    }

    pub fn mark_deleted(&mut self) {
        self.external = EditorExternalState::Deleted;
        self.availability = EditorBufferAvailability::Missing;
        self.pending_save = None;
    }

    pub fn move_path(&mut self, source: &str, destination: &str) -> bool {
        let next = replace_path_prefix(&self.path, source, destination);
        if next == self.path {
            return false;
        }
        self.path = next;
        true
    }

    pub fn close_guard(&self) -> EditorCloseGuard {
        if self.pending_save.is_some() {
            EditorCloseGuard::SavePending
        } else if matches!(self.external, EditorExternalState::Changed { .. }) {
            EditorCloseGuard::ExternalConflict
        } else if matches!(self.external, EditorExternalState::Deleted) && self.dirty {
            EditorCloseGuard::DirtyMissingFile
        } else if self.dirty {
            EditorCloseGuard::Dirty
        } else {
            EditorCloseGuard::Clean
        }
    }

    pub fn recovery_buffer(&self) -> Option<EditorRecoveryBuffer> {
        self.dirty.then(|| EditorRecoveryBuffer {
            path: self.path.clone(),
            content: self.content.clone(),
            saved_content: self.saved_content.clone(),
            saved_revision: self.saved_revision.clone(),
            encoding: self.encoding,
            line_ending: self.line_ending,
            language: self.language.clone(),
            local_revision: self.local_revision,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct EditorRecoveryBuffer {
    pub path: String,
    pub content: String,
    pub saved_content: String,
    pub saved_revision: String,
    pub encoding: FileEncoding,
    pub line_ending: FileLineEnding,
    pub language: Option<String>,
    pub local_revision: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EditorRecoverySnapshot {
    #[serde(with = "recovery_buffers")]
    pub buffers: Arc<[EditorRecoveryBuffer]>,
    pub truncated: bool,
}

mod recovery_buffers {
    use std::sync::Arc;

    use serde::{Deserialize as _, Deserializer, Serialize as _, Serializer};

    use super::EditorRecoveryBuffer;

    pub fn serialize<S>(
        buffers: &Arc<[EditorRecoveryBuffer]>,
        serializer: S,
    ) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        buffers.as_ref().serialize(serializer)
    }

    pub fn deserialize<'de, D>(deserializer: D) -> Result<Arc<[EditorRecoveryBuffer]>, D::Error>
    where
        D: Deserializer<'de>,
    {
        Vec::<EditorRecoveryBuffer>::deserialize(deserializer).map(Arc::from)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct EditorBufferRegistry {
    pub buffers: BTreeMap<String, EditorBufferModel>,
    pub active_path: Option<String>,
    next_request_id: u64,
}

impl EditorBufferRegistry {
    pub fn insert_read(&mut self, file: FileReadResponse) -> &EditorBufferModel {
        let buffer = EditorBufferModel::from_read(file);
        let path = buffer.path.clone();
        self.active_path = Some(path.clone());
        self.buffers.insert(path.clone(), buffer);
        self.buffers.get(&path).expect("inserted editor buffer")
    }

    pub fn active(&self) -> Option<&EditorBufferModel> {
        self.active_path
            .as_ref()
            .and_then(|path| self.buffers.get(path))
    }

    pub fn active_mut(&mut self) -> Option<&mut EditorBufferModel> {
        let path = self.active_path.clone()?;
        self.buffers.get_mut(&path)
    }

    pub fn focus(&mut self, path: &str) -> bool {
        let path = normalize_path(path);
        if !self.buffers.contains_key(&path) {
            return false;
        }
        self.active_path = Some(path);
        true
    }

    pub fn begin_save(&mut self, path: &str) -> Option<EditorSaveTicket> {
        self.next_request_id = self.next_request_id.saturating_add(1).max(1);
        self.buffers
            .get_mut(&normalize_path(path))?
            .begin_save(self.next_request_id)
    }

    pub fn move_path(&mut self, source: &str, destination: &str) {
        let source = normalize_path(source);
        let destination = normalize_path(destination);
        let mut next = BTreeMap::new();
        for (_, mut buffer) in std::mem::take(&mut self.buffers) {
            buffer.move_path(&source, &destination);
            next.insert(buffer.path.clone(), buffer);
        }
        self.buffers = next;
        self.active_path = self
            .active_path
            .take()
            .map(|path| replace_path_prefix(&path, &source, &destination));
    }

    pub fn delete_path(&mut self, path: &str) {
        let path = normalize_path(path);
        let affected = self
            .buffers
            .keys()
            .filter(|candidate| path_is_equal_or_descendant(candidate, &path))
            .cloned()
            .collect::<Vec<_>>();
        for candidate in affected {
            if let Some(buffer) = self.buffers.get_mut(&candidate) {
                if buffer.dirty {
                    buffer.mark_deleted();
                } else {
                    self.buffers.remove(&candidate);
                }
            }
        }
        if self
            .active_path
            .as_ref()
            .is_some_and(|active| !self.buffers.contains_key(active))
        {
            self.active_path = self.buffers.keys().next().cloned();
        }
    }

    pub fn close(&mut self, path: &str, force: bool) -> EditorCloseGuard {
        let path = normalize_path(path);
        let guard = self
            .buffers
            .get(&path)
            .map(EditorBufferModel::close_guard)
            .unwrap_or(EditorCloseGuard::Clean);
        if force || guard == EditorCloseGuard::Clean {
            self.buffers.remove(&path);
            if self.active_path.as_deref() == Some(path.as_str()) {
                self.active_path = self.buffers.keys().next().cloned();
            }
        }
        guard
    }

    pub fn dirty_paths(&self) -> impl Iterator<Item = &str> {
        self.buffers
            .values()
            .filter(|buffer| buffer.dirty)
            .map(|buffer| buffer.path.as_str())
    }

    pub fn recovery_snapshot(&self) -> EditorRecoverySnapshot {
        let mut total = 0_usize;
        let mut truncated = false;
        let mut buffers = Vec::new();
        for buffer in self
            .buffers
            .values()
            .filter_map(EditorBufferModel::recovery_buffer)
        {
            let bytes = buffer
                .content
                .len()
                .saturating_add(buffer.saved_content.len());
            if buffers.len() >= EDITOR_RECOVERY_BUFFER_LIMIT
                || total.saturating_add(bytes) > EDITOR_RECOVERY_TOTAL_BYTES
            {
                truncated = true;
                continue;
            }
            total = total.saturating_add(bytes);
            buffers.push(buffer);
        }
        EditorRecoverySnapshot {
            buffers: Arc::from(buffers),
            truncated,
        }
    }

    pub fn restore_recovery(&mut self, snapshot: EditorRecoverySnapshot) {
        for recovery in snapshot
            .buffers
            .iter()
            .take(EDITOR_RECOVERY_BUFFER_LIMIT)
            .cloned()
        {
            let path = normalize_path(&recovery.path);
            if path.is_empty()
                || recovery
                    .content
                    .len()
                    .saturating_add(recovery.saved_content.len())
                    > EDITOR_RECOVERY_TOTAL_BYTES
            {
                continue;
            }
            let dirty = recovery.content != recovery.saved_content;
            if !dirty {
                continue;
            }
            self.buffers.insert(
                path.clone(),
                EditorBufferModel {
                    path: path.clone(),
                    encoding: recovery.encoding,
                    line_ending: recovery.line_ending,
                    language: recovery.language,
                    content: recovery.content,
                    saved_content: recovery.saved_content,
                    saved_revision: recovery.saved_revision,
                    local_revision: recovery.local_revision.max(1),
                    dirty,
                    external: EditorExternalState::VerificationRequired,
                    pending_save: None,
                    availability: EditorBufferAvailability::Ready,
                    size_bytes: 0,
                    truncated: false,
                    last_error_code: None,
                },
            );
            self.active_path.get_or_insert(path);
        }
    }
}

fn normalize_line_endings(content: &str) -> String {
    content.replace("\r\n", "\n").replace('\r', "\n")
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
    if path == source {
        destination.to_string()
    } else {
        path.strip_prefix(source)
            .filter(|suffix| suffix.starts_with('/'))
            .map(|suffix| format!("{destination}{suffix}"))
            .unwrap_or_else(|| path.to_string())
    }
}

fn bounded_text(value: &str, max_chars: usize) -> String {
    value.trim().chars().take(max_chars).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn read(path: &str, content: &str, revision: &str) -> FileReadResponse {
        FileReadResponse {
            workspace_id: WorkspaceId::new(),
            path: path.into(),
            name: path.rsplit('/').next().unwrap_or(path).into(),
            preview_kind: FilePreviewKind::Text,
            content: Some(content.into()),
            size_bytes: content.len() as u64,
            modified_at_ms: Some(1),
            language: Some("rust".into()),
            truncated: false,
            encoding: FileEncoding::Utf8,
            line_ending: if content.contains("\r\n") {
                FileLineEnding::Crlf
            } else {
                FileLineEnding::Lf
            },
            content_revision: revision.into(),
        }
    }

    #[test]
    fn save_completion_does_not_erase_edits_made_while_pending() {
        let mut buffer = EditorBufferModel::from_read(read("src/lib.rs", "one\n", "r1"));
        assert!(buffer.update_content("two\n"));
        let ticket = buffer.begin_save(7).unwrap();
        assert!(buffer.update_content("three\n"));
        assert!(buffer.finish_save(7, read("src/lib.rs", "two\n", "r2")));
        assert_eq!(buffer.saved_content, "two\n");
        assert_eq!(buffer.content, "three\n");
        assert!(buffer.dirty);
        assert_eq!(ticket.expected_revision, "r1");
    }

    #[test]
    fn external_revision_never_overwrites_dirty_content() {
        let mut buffer = EditorBufferModel::from_read(read("a.rs", "one", "r1"));
        buffer.update_content("local");
        assert!(buffer.observe_external(read("a.rs", "external", "r2")));
        assert_eq!(buffer.content, "local");
        assert_eq!(buffer.close_guard(), EditorCloseGuard::ExternalConflict);
    }

    #[test]
    fn external_observation_for_another_path_is_ignored_even_with_same_revision() {
        let mut buffer = EditorBufferModel::from_read(read("a.rs", "one", "r1"));
        buffer.external = EditorExternalState::VerificationRequired;
        assert!(!buffer.observe_external(read("b.rs", "other", "r1")));
        assert_eq!(buffer.path, "a.rs");
        assert_eq!(buffer.external, EditorExternalState::VerificationRequired);
    }

    #[test]
    fn dirty_deleted_buffers_survive_and_recover_after_restart() {
        let mut registry = EditorBufferRegistry::default();
        registry.insert_read(read("src/lib.rs", "one", "r1"));
        registry.active_mut().unwrap().update_content("local");
        registry.delete_path("src");
        let buffer = registry.buffers.get("src/lib.rs").unwrap();
        assert_eq!(buffer.close_guard(), EditorCloseGuard::DirtyMissingFile);

        let snapshot = registry.recovery_snapshot();
        let mut restored = EditorBufferRegistry::default();
        restored.restore_recovery(snapshot);
        let restored = restored.buffers.get("src/lib.rs").unwrap();
        assert!(restored.dirty);
        assert_eq!(restored.external, EditorExternalState::VerificationRequired);
    }

    #[test]
    fn recovery_snapshot_clones_share_buffers_and_keep_the_json_contract() {
        let mut registry = EditorBufferRegistry::default();
        registry.insert_read(read("src/lib.rs", "one", "r1"));
        registry.active_mut().unwrap().update_content("local");

        let snapshot = registry.recovery_snapshot();
        let cloned = snapshot.clone();
        assert!(Arc::ptr_eq(&snapshot.buffers, &cloned.buffers));

        let json = serde_json::to_value(&snapshot).unwrap();
        assert!(json["buffers"].is_array());
        let decoded: EditorRecoverySnapshot = serde_json::from_value(json).unwrap();
        assert_eq!(decoded, snapshot);
    }

    #[test]
    fn crlf_is_normalized_in_memory_and_preserved_in_save_request() {
        let workspace_id = WorkspaceId::new();
        let mut buffer = EditorBufferModel::from_read(read("a.rs", "one\r\ntwo\r\n", "r1"));
        assert_eq!(buffer.content, "one\ntwo\n");
        buffer.update_content("one\nthree\n");
        let request = buffer.begin_save(1).unwrap().into_request(workspace_id);
        assert_eq!(request.line_ending, Some(FileLineEnding::Crlf));
    }
}

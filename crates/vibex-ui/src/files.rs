use std::fmt;
use std::sync::Arc;

use vibex_backend::{
    BackendError, BackendErrorKind, BackendFuture, BackendOperation, BackendResult,
    DomainCapabilities, FileBackend, MutationRequest,
};
use vibex_core::{
    FileEncoding, FileLineEnding, FilePreviewKind, FileReadRequest, FileReadResponse,
    FileSearchRequest, FileSearchResult, FileTreeEntry, FileTreeRequest, FileWriteRequest,
    WorkspaceId,
};
use vibex_desktop_model::{
    EditorBufferAvailability, EditorBufferRegistry, EditorExternalState, EditorSaveTicket,
    FileTreeProjection,
};

use crate::{
    AsyncPhase, AsyncState, FileConflictComparison, FileEditorStatus, FileWorkflowView,
    WorkflowViewGeneration,
};

pub const FILE_WORKFLOW_MAX_EDIT_BYTES: u64 = 1024 * 1024;
pub const FILE_WORKFLOW_SEARCH_LIMIT: u32 = 200;

#[derive(Clone, Default, PartialEq, Eq)]
pub struct FileWorkflowState {
    pub generation: WorkflowViewGeneration,
    pub workspace_id: Option<WorkspaceId>,
    pub selected_path: Option<String>,
    pub tree: FileTreeProjection,
    pub search: AsyncState<Vec<FileSearchResult>>,
    pub active_file: AsyncState<FileReadResponse>,
    pub buffers: EditorBufferRegistry,
    pub conflict: Option<FileConflictComparison>,
    pub last_error: Option<BackendError>,
    pub last_saved_revision: Option<String>,
    conflict_server: Option<FileReadResponse>,
}

impl fmt::Debug for FileWorkflowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        let active = self.buffers.active();
        formatter
            .debug_struct("FileWorkflowState")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("selected_path", &self.selected_path)
            .field("visible_row_count", &self.tree.all_visible_rows().len())
            .field("search_phase", &self.search.phase)
            .field(
                "search_result_count",
                &self.search.value.as_ref().map(Vec::len),
            )
            .field("active_file_phase", &self.active_file.phase)
            .field(
                "active_file_path",
                &self
                    .active_file
                    .value
                    .as_ref()
                    .map(|file| file.path.as_str()),
            )
            .field("buffer_count", &self.buffers.buffers.len())
            .field("active_buffer_path", &self.buffers.active_path)
            .field("active_buffer_dirty", &active.map(|buffer| buffer.dirty))
            .field("has_conflict", &self.conflict.is_some())
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .field(
                "has_last_saved_revision",
                &self.last_saved_revision.is_some(),
            )
            .finish()
    }
}

impl FileWorkflowState {
    pub fn view(&self) -> FileWorkflowView {
        let active_buffer = self.buffers.active().filter(|buffer| {
            self.selected_path
                .as_deref()
                .is_none_or(|path| path == buffer.path)
        });
        FileWorkflowView {
            generation: self.generation.0,
            rows: self.tree.all_visible_rows().to_vec(),
            search: self.search.value.clone().unwrap_or_default(),
            selected_path: self.selected_path.clone(),
            active_file: self.active_file.value.clone(),
            editor_content: active_buffer.map(|buffer| buffer.content.clone()),
            editor_base_revision: active_buffer.map(|buffer| buffer.saved_revision.clone()),
            status: self.editor_status(),
            conflict: self.conflict.clone(),
        }
    }

    pub fn editor_status(&self) -> FileEditorStatus {
        if self.active_file.phase == AsyncPhase::Loading {
            return FileEditorStatus::Loading;
        }
        if self.conflict.is_some() {
            return FileEditorStatus::Conflict;
        }
        if self
            .last_error
            .as_ref()
            .is_some_and(|error| error.kind == BackendErrorKind::Offline)
        {
            return FileEditorStatus::Disconnected;
        }
        let active_file = self.active_file.value.as_ref();
        let buffer = self.buffers.active().filter(|buffer| {
            self.selected_path
                .as_deref()
                .is_none_or(|path| path == buffer.path)
                && active_file.is_none_or(|file| normalize_path(&file.path) == buffer.path)
        });
        let Some(buffer) = buffer else {
            return active_file.map_or(FileEditorStatus::Clean, file_metadata_status);
        };
        if buffer.pending_save.is_some() {
            return FileEditorStatus::Saving;
        }
        if matches!(buffer.external, EditorExternalState::Changed { .. }) {
            return FileEditorStatus::Conflict;
        }
        if buffer.external == EditorExternalState::Deleted {
            return FileEditorStatus::Unsupported;
        }
        match buffer.availability {
            EditorBufferAvailability::BinaryReadOnly | EditorBufferAvailability::Missing => {
                FileEditorStatus::Unsupported
            }
            EditorBufferAvailability::LargeFileReadOnly => FileEditorStatus::TooLarge,
            EditorBufferAvailability::Ready if buffer.dirty => FileEditorStatus::Dirty,
            EditorBufferAvailability::Ready
                if self.last_saved_revision.as_deref() == Some(&buffer.saved_revision) =>
            {
                FileEditorStatus::Saved
            }
            EditorBufferAvailability::Ready => FileEditorStatus::Clean,
        }
    }

    pub fn compare_conflict(&self) -> Option<&FileConflictComparison> {
        self.conflict.as_ref()
    }

    pub fn reload_server_version(&mut self) -> bool {
        let Some(server) = self.conflict_server.take() else {
            return false;
        };
        self.active_file.resolve(server.clone());
        self.selected_path = Some(normalize_path(&server.path));
        self.buffers.insert_read(server);
        self.conflict = None;
        self.last_error = None;
        self.last_saved_revision = self
            .buffers
            .active()
            .map(|buffer| buffer.saved_revision.clone());
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileTreeLoadTicket {
    pub view_generation: WorkflowViewGeneration,
    pub tree_generation: u64,
    pub workspace_id: WorkspaceId,
    pub base_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileOpenTicket {
    pub generation: WorkflowViewGeneration,
    pub workspace_id: WorkspaceId,
    pub path: String,
}

#[derive(Clone, PartialEq, Eq)]
pub struct FileSaveOperation {
    pub generation: WorkflowViewGeneration,
    pub workspace_id: WorkspaceId,
    pub ticket: EditorSaveTicket,
}

impl fmt::Debug for FileSaveOperation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("FileSaveOperation")
            .field("generation", &self.generation)
            .field("workspace_id", &self.workspace_id)
            .field("request_id", &self.ticket.request_id)
            .field("path", &self.ticket.path)
            .field("local_revision", &self.ticket.local_revision)
            .field(
                "expected_revision_bytes",
                &self.ticket.expected_revision.len(),
            )
            .field("content_bytes", &self.ticket.content.len())
            .field("encoding", &self.ticket.encoding)
            .field("line_ending", &self.ticket.line_ending)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq)]
pub enum FileSaveOutcome {
    Saved(FileReadResponse),
    Conflict {
        error: BackendError,
        server: FileReadResponse,
    },
    Failed(BackendError),
}

impl fmt::Debug for FileSaveOutcome {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Saved(file) => formatter
                .debug_struct("Saved")
                .field("path", &file.path)
                .field(
                    "content_bytes",
                    &file.content.as_deref().map_or(0, str::len),
                )
                .field("size_bytes", &file.size_bytes)
                .field("content_revision", &file.content_revision)
                .field("truncated", &file.truncated)
                .finish(),
            Self::Conflict { error, server } => formatter
                .debug_struct("Conflict")
                .field("error_code", &error.code)
                .field("server_path", &server.path)
                .field(
                    "server_content_bytes",
                    &server.content.as_deref().map_or(0, str::len),
                )
                .field("server_revision", &server.content_revision)
                .finish(),
            Self::Failed(error) => formatter
                .debug_struct("Failed")
                .field("error_kind", &error.kind)
                .field("error_code", &error.code)
                .finish(),
        }
    }
}

#[derive(Clone)]
pub struct FileWorkflowController {
    backend: Arc<dyn FileBackend>,
    capabilities: DomainCapabilities,
    pub state: FileWorkflowState,
}

impl FileWorkflowController {
    pub fn new(backend: Arc<dyn FileBackend>, capabilities: DomainCapabilities) -> Self {
        Self {
            backend,
            capabilities,
            state: FileWorkflowState::default(),
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
        self.state.selected_path = None;
        self.state.tree.reset_workspace(workspace_id);
        self.state.search.clear();
        self.state.active_file.clear();
        self.state.buffers = EditorBufferRegistry::default();
        self.state.conflict = None;
        self.state.conflict_server = None;
        self.state.last_error = None;
        self.state.last_saved_revision = None;
        generation
    }

    pub fn begin_tree_load(&mut self, base_path: &str) -> BackendResult<FileTreeLoadTicket> {
        self.require(BackendOperation::FileTree)?;
        let workspace_id = self.current_workspace()?;
        let tree_generation = self.state.tree.begin_load(base_path);
        Ok(FileTreeLoadTicket {
            view_generation: self.state.generation,
            tree_generation,
            workspace_id,
            base_path: normalize_path(base_path),
        })
    }

    pub fn load_tree(
        &self,
        ticket: FileTreeLoadTicket,
    ) -> BackendFuture<'static, Vec<FileTreeEntry>> {
        if let Err(error) = self.require(BackendOperation::FileTree) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move {
            backend
                .file_tree(FileTreeRequest {
                    workspace_id: ticket.workspace_id,
                    path: (!ticket.base_path.is_empty()).then_some(ticket.base_path),
                    max_depth: Some(1),
                    include_hidden: false,
                })
                .await
        })
    }

    pub fn apply_tree_load(
        &mut self,
        ticket: &FileTreeLoadTicket,
        result: BackendResult<Vec<FileTreeEntry>>,
    ) -> bool {
        if self.state.generation != ticket.view_generation
            || self.state.workspace_id.as_ref() != Some(&ticket.workspace_id)
        {
            return false;
        }
        match result {
            Ok(entries) => self.state.tree.apply_entries(
                &ticket.workspace_id,
                ticket.tree_generation,
                &ticket.base_path,
                entries,
            ),
            Err(error) => {
                self.state.last_error = Some(error.clone());
                self.state
                    .tree
                    .fail_load(ticket.tree_generation, &ticket.base_path, &error.code)
            }
        }
    }

    pub fn begin_search(&mut self) -> BackendResult<()> {
        self.require(BackendOperation::FileSearch)?;
        self.current_workspace()?;
        self.state.search.begin();
        Ok(())
    }

    pub fn search_files(
        &self,
        mut request: FileSearchRequest,
    ) -> BackendFuture<'static, Vec<FileSearchResult>> {
        if let Err(error) = self.require(BackendOperation::FileSearch) {
            return error_future(error);
        }
        if self.state.workspace_id.as_ref() != Some(&request.workspace_id) {
            return error_future(BackendError::conflict(
                "file_workspace_generation_stale",
                "the file search targets a workspace that is no longer selected",
            ));
        }
        request.limit = Some(
            request
                .limit
                .unwrap_or(FILE_WORKFLOW_SEARCH_LIMIT)
                .clamp(1, FILE_WORKFLOW_SEARCH_LIMIT),
        );
        let backend = self.backend.clone();
        Box::pin(async move { backend.search_files(request).await })
    }

    pub fn apply_search(
        &mut self,
        generation: WorkflowViewGeneration,
        result: BackendResult<Vec<FileSearchResult>>,
    ) -> bool {
        if self.state.generation != generation {
            return false;
        }
        match result {
            Ok(results) => self.state.search.resolve(results),
            Err(error) => {
                self.state.last_error = Some(error.clone());
                self.state.search.reject(error);
            }
        }
        true
    }

    pub fn begin_open_file(&mut self, path: impl Into<String>) -> BackendResult<FileOpenTicket> {
        self.require(BackendOperation::FileRead)?;
        let workspace_id = self.current_workspace()?;
        let path = normalize_path(&path.into());
        if path.is_empty() {
            return Err(BackendError::failed(
                "file_path_invalid",
                "a file path is required",
            ));
        }
        let generation = self.state.generation.advance();
        let path_changed = self.state.selected_path.as_deref() != Some(path.as_str());
        self.state.selected_path = Some(path.clone());
        if self.state.buffers.buffers.contains_key(&path) {
            let _ = self.state.buffers.focus(&path);
        } else {
            self.state.buffers.active_path = None;
        }
        if path_changed {
            self.state.active_file.clear();
        }
        self.state.active_file.begin();
        self.state.conflict = None;
        self.state.conflict_server = None;
        self.state.last_error = None;
        Ok(FileOpenTicket {
            generation,
            workspace_id,
            path,
        })
    }

    pub fn read_file(&self, ticket: FileOpenTicket) -> BackendFuture<'static, FileReadResponse> {
        if let Err(error) = self.require(BackendOperation::FileRead) {
            return error_future(error);
        }
        let backend = self.backend.clone();
        Box::pin(async move {
            backend
                .read_file(FileReadRequest {
                    workspace_id: ticket.workspace_id,
                    path: ticket.path,
                    max_bytes: Some(FILE_WORKFLOW_MAX_EDIT_BYTES.saturating_add(1)),
                })
                .await
        })
    }

    pub fn apply_file_read(
        &mut self,
        ticket: &FileOpenTicket,
        result: BackendResult<FileReadResponse>,
    ) -> bool {
        if self.state.generation != ticket.generation
            || self.state.workspace_id.as_ref() != Some(&ticket.workspace_id)
            || self.state.selected_path.as_deref() != Some(ticket.path.as_str())
        {
            return false;
        }
        match result {
            Ok(file)
                if file.workspace_id == ticket.workspace_id
                    && normalize_path(&file.path) == ticket.path =>
            {
                self.state.active_file.resolve(file.clone());
                if matches!(
                    file.preview_kind,
                    FilePreviewKind::Text | FilePreviewKind::Markdown
                ) {
                    let path = normalize_path(&file.path);
                    self.state.buffers.active_path = Some(path.clone());
                    if let Some(buffer) = self.state.buffers.buffers.get_mut(&path) {
                        let had_local_changes = buffer.dirty || buffer.pending_save.is_some();
                        let base_revision = buffer.saved_revision.clone();
                        let local_content = buffer.content.clone();
                        if buffer.observe_external(file.clone()) && had_local_changes {
                            self.state.conflict = Some(FileConflictComparison {
                                workspace_id: ticket.workspace_id.clone(),
                                path: ticket.path.clone(),
                                base_revision,
                                server_revision: file.content_revision.clone(),
                                local_content,
                                server_content: file.content.clone().unwrap_or_default(),
                            });
                            self.state.conflict_server = Some(file.clone());
                        }
                    } else {
                        self.state.buffers.insert_read(file.clone());
                    }
                    enforce_workflow_edit_limit(&mut self.state.buffers, &path);
                } else {
                    self.state.buffers.active_path = None;
                }
                self.state.last_error = None;
                self.state.last_saved_revision = None;
            }
            Ok(_) => {
                let error = BackendError::failed(
                    "file_read_response_mismatch",
                    "the backend returned a different file",
                );
                self.state.active_file.reject(error.clone());
                self.state.last_error = Some(error);
            }
            Err(error) => {
                self.state.active_file.reject(error.clone());
                self.state.last_error = Some(error);
            }
        }
        true
    }

    pub fn update_active_content(&mut self, content: impl Into<String>) -> BackendResult<bool> {
        self.require(BackendOperation::FileWrite)?;
        let content = content.into();
        if content.len() as u64 > FILE_WORKFLOW_MAX_EDIT_BYTES {
            return Err(BackendError::failed(
                "file_edit_too_large",
                "shared Web/mobile editing is limited to 1 MiB UTF-8 text files",
            ));
        }
        let active_file_path = self
            .state
            .active_file
            .value
            .as_ref()
            .map(|file| normalize_path(&file.path));
        if self.state.active_file.phase != AsyncPhase::Ready
            || active_file_path.as_deref() != self.state.buffers.active_path.as_deref()
        {
            return Err(BackendError::conflict(
                "file_buffer_missing",
                "no editable file is selected",
            ));
        }
        let buffer = self.state.buffers.active_mut().ok_or_else(|| {
            BackendError::failed("file_buffer_missing", "no editable file is selected")
        })?;
        if buffer.availability != EditorBufferAvailability::Ready {
            return Err(buffer_edit_error(buffer));
        }
        self.state.last_saved_revision = None;
        let changed = buffer.update_content(content);
        if changed && let Some(conflict) = self.state.conflict.as_mut() {
            conflict.local_content = buffer.content.clone();
        }
        Ok(changed)
    }

    pub fn begin_save_active(&mut self) -> BackendResult<FileSaveOperation> {
        self.require(BackendOperation::FileWrite)?;
        let workspace_id = self.current_workspace()?;
        let path =
            self.state.buffers.active_path.clone().ok_or_else(|| {
                BackendError::failed("file_buffer_missing", "no file is selected")
            })?;
        let active_file_path = self
            .state
            .active_file
            .value
            .as_ref()
            .map(|file| normalize_path(&file.path));
        if self.state.active_file.phase != AsyncPhase::Ready
            || active_file_path.as_deref() != Some(path.as_str())
        {
            return Err(BackendError::conflict(
                "file_buffer_missing",
                "no editable file is selected",
            ));
        }
        if let Some(buffer) = self.state.buffers.active()
            && buffer.availability != EditorBufferAvailability::Ready
        {
            return Err(buffer_edit_error(buffer));
        }
        if self
            .state
            .buffers
            .active()
            .is_some_and(|buffer| buffer.content.len() as u64 > FILE_WORKFLOW_MAX_EDIT_BYTES)
        {
            return Err(BackendError::failed(
                "file_edit_too_large",
                "shared Web/mobile editing is limited to 1 MiB UTF-8 text files",
            ));
        }
        let ticket = self.state.buffers.begin_save(&path).ok_or_else(|| {
            let code = match self.state.buffers.active().map(|buffer| &buffer.external) {
                Some(EditorExternalState::Changed { .. }) => "file_revision_conflict",
                _ => "file_save_not_ready",
            };
            BackendError::conflict(code, "the selected file is not ready to save")
        })?;
        self.state.conflict = None;
        self.state.conflict_server = None;
        self.state.last_error = None;
        Ok(FileSaveOperation {
            generation: self.state.generation,
            workspace_id,
            ticket,
        })
    }

    pub fn save_file(
        &self,
        operation: FileSaveOperation,
    ) -> BackendFuture<'static, FileSaveOutcome> {
        if let Err(error) = self.require(BackendOperation::FileWrite) {
            return Box::pin(async move { Ok(FileSaveOutcome::Failed(error)) });
        }
        if !self.save_operation_is_current(&operation) {
            return Box::pin(async {
                Ok(FileSaveOutcome::Failed(BackendError::conflict(
                    "file_save_generation_stale",
                    "the file save is no longer the active buffer operation",
                )))
            });
        }
        let backend = self.backend.clone();
        Box::pin(async move {
            let expected_revision = operation.ticket.expected_revision.clone();
            let path = operation.ticket.path.clone();
            let request = MutationRequest::new(
                operation
                    .ticket
                    .clone()
                    .into_request(operation.workspace_id.clone()),
            )
            .with_idempotency_key(format!(
                "file-save-{}-{}",
                operation.workspace_id.as_str(),
                operation.ticket.request_id
            ))
            .with_expected_revision(expected_revision);
            match backend.write_file(request).await {
                Ok(file) => Ok(FileSaveOutcome::Saved(file)),
                Err(error) if error.kind == BackendErrorKind::Conflict => {
                    let server = backend
                        .read_file(FileReadRequest {
                            workspace_id: operation.workspace_id,
                            path,
                            max_bytes: Some(FILE_WORKFLOW_MAX_EDIT_BYTES.saturating_add(1)),
                        })
                        .await?;
                    Ok(FileSaveOutcome::Conflict { error, server })
                }
                Err(error) => Ok(FileSaveOutcome::Failed(error)),
            }
        })
    }

    pub fn apply_save_outcome(
        &mut self,
        operation: &FileSaveOperation,
        result: BackendResult<FileSaveOutcome>,
    ) -> bool {
        if !self.save_operation_is_current(operation) {
            if self.save_operation_matches_pending(operation)
                && let Some(buffer) = self.state.buffers.buffers.get_mut(&operation.ticket.path)
            {
                let _ = buffer.fail_save(operation.ticket.request_id, "file_save_generation_stale");
            }
            return false;
        }
        let outcome = match result {
            Ok(outcome) => outcome,
            Err(error) => FileSaveOutcome::Failed(error),
        };
        match outcome {
            FileSaveOutcome::Saved(file)
                if file.workspace_id != operation.workspace_id
                    || normalize_path(&file.path) != operation.ticket.path =>
            {
                let error = BackendError::failed(
                    "file_save_response_mismatch",
                    "the backend returned a different file",
                );
                if let Some(buffer) = self.state.buffers.buffers.get_mut(&operation.ticket.path) {
                    let _ = buffer.fail_save(operation.ticket.request_id, &error.code);
                }
                self.state.last_error = Some(error);
                self.state.last_saved_revision = None;
            }
            FileSaveOutcome::Saved(file) => {
                if let Some(buffer) = self.state.buffers.buffers.get_mut(&operation.ticket.path) {
                    let _ = buffer.finish_save(operation.ticket.request_id, file.clone());
                }
                self.state.active_file.resolve(file.clone());
                self.state.last_saved_revision = Some(file.content_revision);
                self.state.last_error = None;
                self.state.conflict = None;
                self.state.conflict_server = None;
            }
            FileSaveOutcome::Conflict { error: _, server }
                if server.workspace_id != operation.workspace_id
                    || normalize_path(&server.path) != operation.ticket.path =>
            {
                let mismatch = BackendError::failed(
                    "file_save_response_mismatch",
                    "the backend returned a different conflict file",
                );
                if let Some(buffer) = self.state.buffers.buffers.get_mut(&operation.ticket.path) {
                    let _ = buffer.fail_save(operation.ticket.request_id, &mismatch.code);
                }
                self.state.last_error = Some(mismatch);
                self.state.last_saved_revision = None;
            }
            FileSaveOutcome::Conflict { error, server } => {
                let local_content = self
                    .state
                    .buffers
                    .buffers
                    .get_mut(&operation.ticket.path)
                    .map(|buffer| {
                        let _ = buffer.fail_save(operation.ticket.request_id, &error.code);
                        let local_content = buffer.content.clone();
                        let _ = buffer.observe_external(server.clone());
                        local_content
                    })
                    .unwrap_or_else(|| operation.ticket.content.clone());
                let server_content = server.content.clone().unwrap_or_default();
                self.state.conflict = Some(FileConflictComparison {
                    workspace_id: operation.workspace_id.clone(),
                    path: operation.ticket.path.clone(),
                    base_revision: operation.ticket.expected_revision.clone(),
                    server_revision: server.content_revision.clone(),
                    local_content,
                    server_content,
                });
                self.state.conflict_server = Some(server);
                self.state.last_error = Some(error);
                self.state.last_saved_revision = None;
            }
            FileSaveOutcome::Failed(error) => {
                if let Some(buffer) = self.state.buffers.buffers.get_mut(&operation.ticket.path) {
                    let _ = buffer.fail_save(operation.ticket.request_id, &error.code);
                }
                self.state.last_error = Some(error);
                self.state.last_saved_revision = None;
            }
        }
        true
    }

    pub fn create_text_file(
        &self,
        workspace_id: WorkspaceId,
        path: impl Into<String>,
        content: impl Into<String>,
    ) -> BackendFuture<'static, FileReadResponse> {
        if let Err(error) = self.require(BackendOperation::FileWrite) {
            return error_future(error);
        }
        if self.state.workspace_id.as_ref() != Some(&workspace_id) {
            return error_future(BackendError::conflict(
                "file_workspace_generation_stale",
                "the file create request targets a workspace that is no longer selected",
            ));
        }
        let path = normalize_path(&path.into());
        let content = content.into();
        if path.is_empty() || content.len() as u64 > FILE_WORKFLOW_MAX_EDIT_BYTES {
            return error_future(BackendError::failed(
                "file_create_invalid",
                "a bounded UTF-8 path and content are required",
            ));
        }
        let backend = self.backend.clone();
        Box::pin(async move {
            backend
                .write_file(
                    MutationRequest::new(FileWriteRequest {
                        workspace_id,
                        path,
                        content,
                        create_if_missing: true,
                        expected_revision: None,
                        encoding: Some(FileEncoding::Utf8),
                        line_ending: Some(FileLineEnding::Lf),
                    })
                    .with_idempotency_key(vibex_core::RequestId::new().to_string()),
                )
                .await
        })
    }

    fn current_workspace(&self) -> BackendResult<WorkspaceId> {
        self.state.workspace_id.clone().ok_or_else(|| {
            BackendError::failed(
                "file_workspace_missing",
                "select a workspace before using the file workflow",
            )
        })
    }

    fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        if self.capabilities.supports(operation) {
            Ok(())
        } else {
            Err(file_capability_error(&self.capabilities, operation))
        }
    }

    fn save_operation_is_current(&self, operation: &FileSaveOperation) -> bool {
        self.state.generation == operation.generation
            && self.state.workspace_id.as_ref() == Some(&operation.workspace_id)
            && self.state.active_file.phase == AsyncPhase::Ready
            && self
                .state
                .active_file
                .value
                .as_ref()
                .is_some_and(|file| normalize_path(&file.path) == operation.ticket.path)
            && self.save_operation_matches_pending(operation)
    }

    fn save_operation_matches_pending(&self, operation: &FileSaveOperation) -> bool {
        self.state
            .buffers
            .buffers
            .get(&operation.ticket.path)
            .and_then(|buffer| buffer.pending_save.as_ref())
            .is_some_and(|pending| {
                pending.request_id == operation.ticket.request_id
                    && pending.local_revision == operation.ticket.local_revision
            })
    }
}

fn enforce_workflow_edit_limit(registry: &mut EditorBufferRegistry, path: &str) {
    if let Some(buffer) = registry.buffers.get_mut(&normalize_path(path))
        && buffer.size_bytes > FILE_WORKFLOW_MAX_EDIT_BYTES
    {
        buffer.availability = EditorBufferAvailability::LargeFileReadOnly;
    }
}

fn file_metadata_status(file: &FileReadResponse) -> FileEditorStatus {
    if file.encoding == FileEncoding::Binary
        || !matches!(
            file.preview_kind,
            FilePreviewKind::Text | FilePreviewKind::Markdown
        )
    {
        FileEditorStatus::Unsupported
    } else if file.truncated || file.size_bytes > FILE_WORKFLOW_MAX_EDIT_BYTES {
        FileEditorStatus::TooLarge
    } else {
        FileEditorStatus::Clean
    }
}

fn buffer_edit_error(buffer: &vibex_desktop_model::EditorBufferModel) -> BackendError {
    match buffer.availability {
        EditorBufferAvailability::LargeFileReadOnly => BackendError::failed(
            "file_edit_too_large",
            "shared Web/mobile editing is limited to 1 MiB UTF-8 text files",
        ),
        EditorBufferAvailability::BinaryReadOnly => BackendError::unsupported(
            "file_binary_edit_unsupported",
            "binary files cannot be edited in the shared workflow",
        ),
        EditorBufferAvailability::Missing => BackendError::unsupported(
            "file_edit_unsupported",
            "the selected file is not editable in the shared workflow",
        ),
        EditorBufferAvailability::Ready => BackendError::failed(
            "file_save_not_ready",
            "the selected file is not ready to edit",
        ),
    }
}

fn normalize_path(path: &str) -> String {
    path.trim_matches('/').replace('\\', "/")
}

fn file_capability_error(
    capabilities: &DomainCapabilities,
    operation: BackendOperation,
) -> BackendError {
    use vibex_backend::CapabilityAvailability;
    let label = match operation {
        BackendOperation::FileTree => "file_tree",
        BackendOperation::FileSearch => "file_search",
        BackendOperation::FileRead => "file_read",
        BackendOperation::FileWrite => "file_write",
        _ => "file_operation",
    };
    match capabilities.availability {
        CapabilityAvailability::Offline => BackendError::offline(
            format!("{label}_offline"),
            "the authoritative file backend is offline",
        ),
        CapabilityAvailability::Degraded => BackendError::loading(
            format!("{label}_degraded"),
            "the authoritative file backend is temporarily degraded",
        ),
        CapabilityAvailability::RequiresPermission => BackendError::permission(
            format!("{label}_permission_required"),
            "the current device lacks permission for this file operation",
        ),
        CapabilityAvailability::Available | CapabilityAvailability::Unsupported => {
            BackendError::unsupported(
                format!("{label}_unsupported"),
                "this file operation is not supported by the shared workflow",
            )
        }
    }
}

fn error_future<T: 'static>(error: BackendError) -> BackendFuture<'static, T> {
    Box::pin(async move { Err(error) })
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use vibex_core::{FileEntryKind, FileMutationRequest, FileSearchResult};

    #[derive(Clone)]
    struct MockFileBackend {
        file: Arc<Mutex<FileReadResponse>>,
    }

    impl FileBackend for MockFileBackend {
        fn file_tree(&self, request: FileTreeRequest) -> BackendFuture<'_, Vec<FileTreeEntry>> {
            Box::pin(async move {
                Ok(vec![FileTreeEntry {
                    workspace_id: request.workspace_id,
                    path: "src/lib.rs".into(),
                    name: "lib.rs".into(),
                    parent_path: Some("src".into()),
                    kind: FileEntryKind::File,
                    size_bytes: Some(4),
                    modified_at_ms: Some(1),
                    hidden: false,
                    ignored: false,
                }])
            })
        }

        fn search_files(
            &self,
            request: FileSearchRequest,
        ) -> BackendFuture<'_, Vec<FileSearchResult>> {
            Box::pin(async move {
                Ok(vec![FileSearchResult {
                    workspace_id: request.workspace_id,
                    path: "src/lib.rs".into(),
                    name: "lib.rs".into(),
                    kind: FileEntryKind::File,
                    line: Some(1),
                    snippet: Some("fn main".into()),
                    match_start: Some(3),
                    match_end: Some(7),
                    matched_text: Some("main".into()),
                    snippet_match_start: Some(3),
                    snippet_match_end: Some(7),
                }])
            })
        }

        fn read_file(&self, _request: FileReadRequest) -> BackendFuture<'_, FileReadResponse> {
            let file = self.file.clone();
            Box::pin(async move {
                file.lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))
                    .map(|file| file.clone())
            })
        }

        fn write_file(
            &self,
            request: MutationRequest<FileWriteRequest>,
        ) -> BackendFuture<'_, FileReadResponse> {
            let file = self.file.clone();
            Box::pin(async move {
                let mut current = file
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock poisoned"))?;
                if request.payload.expected_revision.as_deref()
                    != Some(current.content_revision.as_str())
                {
                    return Err(BackendError::conflict(
                        "file_revision_conflict",
                        "file changed on the server",
                    ));
                }
                current.content = Some(request.payload.content);
                current.size_bytes = current.content.as_deref().unwrap_or_default().len() as u64;
                current.content_revision = "rev-3".into();
                Ok(current.clone())
            })
        }

        fn create_directory(
            &self,
            _request: MutationRequest<FileMutationRequest>,
        ) -> BackendFuture<'_, FileTreeEntry> {
            error_future(BackendError::unsupported("dangerous", "unsupported"))
        }

        fn copy_path(
            &self,
            _request: MutationRequest<FileMutationRequest>,
        ) -> BackendFuture<'_, FileTreeEntry> {
            error_future(BackendError::unsupported("dangerous", "unsupported"))
        }

        fn rename_path(
            &self,
            _request: MutationRequest<FileMutationRequest>,
        ) -> BackendFuture<'_, FileTreeEntry> {
            error_future(BackendError::unsupported("dangerous", "unsupported"))
        }

        fn delete_path(
            &self,
            _request: MutationRequest<FileMutationRequest>,
        ) -> BackendFuture<'_, ()> {
            error_future(BackendError::unsupported("dangerous", "unsupported"))
        }
    }

    fn text_file(workspace_id: WorkspaceId, revision: &str, content: &str) -> FileReadResponse {
        FileReadResponse {
            workspace_id,
            path: "src/lib.rs".into(),
            name: "lib.rs".into(),
            preview_kind: FilePreviewKind::Text,
            content: Some(content.into()),
            size_bytes: content.len() as u64,
            modified_at_ms: Some(1),
            language: Some("rust".into()),
            truncated: false,
            encoding: FileEncoding::Utf8,
            line_ending: FileLineEnding::Lf,
            content_revision: revision.into(),
        }
    }

    fn capabilities() -> DomainCapabilities {
        DomainCapabilities::available([
            BackendOperation::FileTree,
            BackendOperation::FileSearch,
            BackendOperation::FileRead,
            BackendOperation::FileWrite,
        ])
    }

    #[tokio::test]
    async fn file_revision_conflict_preserves_local_content_and_supports_compare_reload() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "server v1",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend.clone(), capabilities());
        controller.select_workspace(workspace_id.clone());
        let open = controller.begin_open_file("src/lib.rs").unwrap();
        let file = controller.read_file(open.clone()).await.unwrap();
        assert!(controller.apply_file_read(&open, Ok(file)));
        controller.update_active_content("local edit").unwrap();
        *backend.file.lock().unwrap() = text_file(workspace_id, "rev-2", "server v2");

        let operation = controller.begin_save_active().unwrap();
        let outcome = controller.save_file(operation.clone()).await.unwrap();
        assert!(controller.apply_save_outcome(&operation, Ok(outcome)));
        assert_eq!(controller.state.editor_status(), FileEditorStatus::Conflict);
        let comparison = controller.state.compare_conflict().unwrap();
        assert_eq!(comparison.local_content, "local edit");
        assert_eq!(comparison.server_content, "server v2");
        assert!(controller.state.reload_server_version());
        assert_eq!(
            controller
                .state
                .buffers
                .active()
                .map(|buffer| buffer.content.as_str()),
            Some("server v2")
        );
    }

    #[tokio::test]
    async fn dirty_buffer_survives_switch_and_reopen_with_external_revision() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "server v1",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend.clone(), capabilities());
        controller.select_workspace(workspace_id.clone());
        let first = controller.begin_open_file("src/lib.rs").unwrap();
        let first_file = controller.read_file(first.clone()).await.unwrap();
        assert!(controller.apply_file_read(&first, Ok(first_file)));
        controller
            .update_active_content("local dirty edit")
            .unwrap();

        controller.begin_open_file("src/other.rs").unwrap();
        *backend.file.lock().unwrap() = text_file(workspace_id, "rev-2", "server v2");
        let reopened = controller.begin_open_file("src/lib.rs").unwrap();
        let server_file = controller.read_file(reopened.clone()).await.unwrap();
        assert!(controller.apply_file_read(&reopened, Ok(server_file)));
        assert_eq!(controller.state.editor_status(), FileEditorStatus::Conflict);
        assert_eq!(
            controller.state.view().editor_content.as_deref(),
            Some("local dirty edit")
        );
        assert_eq!(
            controller.state.view().editor_base_revision.as_deref(),
            Some("rev-1")
        );
        let comparison = controller.state.compare_conflict().unwrap();
        assert_eq!(comparison.local_content, "local dirty edit");
        assert_eq!(comparison.server_content, "server v2");
    }

    #[test]
    fn file_workflow_enforces_one_mib_utf8_edit_limit() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "small",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());
        controller
            .state
            .buffers
            .insert_read(text_file(workspace_id, "rev-1", "small"));
        let error = controller
            .update_active_content("x".repeat(FILE_WORKFLOW_MAX_EDIT_BYTES as usize + 1))
            .unwrap_err();
        assert_eq!(error.code, "file_edit_too_large");
        assert_eq!(controller.state.buffers.active().unwrap().content, "small");
    }

    #[test]
    fn file_open_generation_fences_old_reads_and_marks_binary_read_only() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "small",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());

        let old = controller.begin_open_file("src/old.rs").unwrap();
        let current = controller.begin_open_file("assets/data.bin").unwrap();
        assert!(
            !controller.apply_file_read(&old, Ok(text_file(workspace_id.clone(), "rev-1", "old")),)
        );

        let binary = FileReadResponse {
            workspace_id,
            path: "assets/data.bin".into(),
            name: "data.bin".into(),
            preview_kind: FilePreviewKind::Binary,
            content: None,
            size_bytes: 4,
            modified_at_ms: Some(1),
            language: None,
            truncated: false,
            encoding: FileEncoding::Binary,
            line_ending: FileLineEnding::None,
            content_revision: "rev-2".into(),
        };
        assert!(controller.apply_file_read(&current, Ok(binary)));
        assert_eq!(
            controller.state.editor_status(),
            FileEditorStatus::Unsupported
        );
        assert!(controller.update_active_content("nope").is_err());
    }

    #[tokio::test]
    async fn stale_file_save_is_rejected_before_backend_write() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "server v1",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend.clone(), capabilities());
        controller.select_workspace(workspace_id);
        let open = controller.begin_open_file("src/lib.rs").unwrap();
        let file = controller.read_file(open.clone()).await.unwrap();
        assert!(controller.apply_file_read(&open, Ok(file)));
        controller.update_active_content("local edit").unwrap();
        let operation = controller.begin_save_active().unwrap();

        controller.begin_open_file("src/other.rs").unwrap();
        let outcome = controller.save_file(operation.clone()).await.unwrap();
        assert_eq!(
            match &outcome {
                FileSaveOutcome::Failed(error) => error.code.as_str(),
                _ => "unexpected",
            },
            "file_save_generation_stale"
        );
        assert!(!controller.apply_save_outcome(&operation, Ok(outcome)));
        assert!(
            controller
                .state
                .buffers
                .buffers
                .get("src/lib.rs")
                .is_some_and(|buffer| buffer.pending_save.is_none())
        );
        assert_eq!(
            backend.file.lock().unwrap().content.as_deref(),
            Some("server v1")
        );
    }

    #[tokio::test]
    async fn file_tree_and_search_use_shared_bounded_backend_contracts() {
        let workspace_id = WorkspaceId::new();
        let backend = Arc::new(MockFileBackend {
            file: Arc::new(Mutex::new(text_file(
                workspace_id.clone(),
                "rev-1",
                "small",
            ))),
        });
        let mut controller = FileWorkflowController::new(backend, capabilities());
        controller.select_workspace(workspace_id.clone());
        let tree = controller.begin_tree_load("").unwrap();
        let entries = controller.load_tree(tree.clone()).await.unwrap();
        assert!(controller.apply_tree_load(&tree, Ok(entries)));
        assert_eq!(controller.state.tree.visible_row_count(), 1);

        controller.begin_search().unwrap();
        let generation = controller.state.generation;
        let results = controller
            .search_files(FileSearchRequest {
                workspace_id,
                query: "lib".into(),
                include_content: true,
                case_sensitive: false,
                whole_word: false,
                regex: false,
                limit: Some(10_000),
            })
            .await
            .unwrap();
        assert!(controller.apply_search(generation, Ok(results)));
        assert_eq!(controller.state.search.value.as_ref().unwrap().len(), 1);
    }

    #[test]
    fn file_state_operation_and_outcome_debug_redact_contents() {
        let workspace_id = WorkspaceId::new();
        let file = text_file(workspace_id.clone(), "rev-1", "private server text");
        let mut state = FileWorkflowState {
            workspace_id: Some(workspace_id.clone()),
            ..FileWorkflowState::default()
        };
        state.active_file.resolve(file.clone());
        state.buffers.insert_read(file.clone());
        state
            .buffers
            .active_mut()
            .unwrap()
            .update_content("private local edit");
        let operation = FileSaveOperation {
            generation: WorkflowViewGeneration(1),
            workspace_id,
            ticket: EditorSaveTicket {
                request_id: 1,
                path: "src/lib.rs".into(),
                local_revision: 2,
                expected_revision: "rev-1".into(),
                content: "private save payload".into(),
                encoding: FileEncoding::Utf8,
                line_ending: FileLineEnding::Lf,
            },
        };
        let outcome = FileSaveOutcome::Saved(file);
        let debug = format!("{state:?} {operation:?} {outcome:?}");
        for secret in [
            "private server text",
            "private local edit",
            "private save payload",
        ] {
            assert!(!debug.contains(secret), "debug leaked {secret}");
        }
        assert!(debug.contains("content_bytes"));
    }
}

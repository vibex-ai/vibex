use std::collections::BTreeMap;
use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, Entity, IntoElement, MouseButton, MouseUpEvent, ParentElement as _, Render,
    Styled as _, Task, WeakEntity, Window, div, prelude::*, px, rgb, svg,
};
use vibex_backend::{
    AgentBackend as _, BackendError, BackendOperation, BackendResult, MutationRequest,
    TerminalBackend as _,
};
use vibex_core::{
    AgentSessionRuntimeSelectionState, FileEntryKind, FileSearchRequest, GitChange, GitChangeKind,
    GitCommitSummary, GitDiffResponse, GitHistoryRequest, GitRemoteActionKind,
    GitRemoteActionRequest, GitStageRequest, ProviderRunHealthProbesRequest, RemoteActionClass,
    RequestId, RuntimeOptionAvailability, RuntimeSelectionInteraction, SessionRuntimeFeature,
    SessionRuntimeFeatureKind, SessionRuntimeOption, SessionRuntimeOptionCatalog,
    SessionRuntimeSelection, SetDesiredAgentSessionRuntimeRequest, TerminalCreateRequest,
    TerminalId, TerminalSnapshot, VibexSessionId, WorkspaceId,
};
use vibex_desktop_model::{
    FileGitSignal, FileIconDescriptor, FileIconKind, GitPathSelectionState, GitQueryKind,
    GitTreeRow, GitTreeRowKind, GitWorkbenchMode, file_icon_descriptor,
};
use vibex_remote_client::WebRemoteBackend;
use vibex_ui::{
    FileEditorStatus, FileWorkflowController, GitWorkflowController,
    ManagementWorkflowCapabilities, ManagementWorkflowController, ShellKind, TerminalInput,
    TerminalKey, TerminalKeyModifiers, TerminalWorkflowCapabilities, TerminalWorkflowController,
};

use crate::input::TextInput;
use crate::locale;
use crate::theme;

const TERMINAL_OUTPUT_LIMIT: usize = 64 * 1024;
const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(600);
const RUNTIME_FEATURE_VALUE_LIMIT: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchSurface {
    Files,
    Git,
    Terminal,
    Providers,
    Runtime,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MobileFileSearchMode {
    Name,
    Content,
}

impl MobileFileSearchMode {
    fn toggle(self) -> Self {
        match self {
            Self::Name => Self::Content,
            Self::Content => Self::Name,
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::Name => "Name",
            Self::Content => "Content",
        }
    }
}

impl WorkbenchSurface {
    pub const ALL: [Self; 3] = [Self::Files, Self::Git, Self::Terminal];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Files => "Files",
            Self::Git => "Git",
            Self::Terminal => "Terminal",
            Self::Providers => "Providers",
            Self::Runtime => "Runtime",
        }
    }

    pub fn localized_label(self) -> &'static str {
        locale::common(self.label())
    }
}

pub struct MobileWorkbench {
    backend: Arc<WebRemoteBackend>,
    workspace_id: WorkspaceId,
    surface: WorkbenchSurface,
    files: FileWorkflowController,
    git: GitWorkflowController,
    terminal: TerminalWorkflowController,
    management: ManagementWorkflowController,
    file_search_input: Entity<TextInput>,
    file_search_mode: MobileFileSearchMode,
    file_editor_input: Entity<TextInput>,
    git_commit_input: Entity<TextInput>,
    git_history_query_input: Entity<TextInput>,
    terminal_input: Entity<TextInput>,
    file_editor_path: Option<String>,
    git_diff: Option<GitDiffResponse>,
    git_commit_confirmation: bool,
    git_history_loading: bool,
    git_history_request_generation: u64,
    terminal_snapshot: Option<TerminalSnapshot>,
    terminal_close_confirmation: Option<TerminalId>,
    agent_summaries: Vec<vibex_core::RemoteAgentConfigSummary>,
    runtime_session_id: Option<VibexSessionId>,
    runtime_catalog: Option<SessionRuntimeOptionCatalog>,
    runtime_state: Option<AgentSessionRuntimeSelectionState>,
    runtime_draft: Option<SessionRuntimeSelection>,
    runtime_feature_inputs: BTreeMap<String, Entity<TextInput>>,
    terminal_poll_generation: u64,
    management_request_generation: u64,
    runtime_request_generation: u64,
    management_busy_generation: Option<u64>,
    runtime_busy_generation: Option<u64>,
    busy: bool,
    notice: Option<String>,
    error: Option<BackendError>,
    tasks: Vec<Task<()>>,
}

impl MobileWorkbench {
    pub fn new(
        backend: Arc<WebRemoteBackend>,
        workspace_id: WorkspaceId,
        session_id: Option<VibexSessionId>,
        cx: &mut Context<Self>,
    ) -> Self {
        let capabilities = backend.capability_snapshot();
        let mut files = FileWorkflowController::new(backend.clone(), capabilities.file.clone());
        files.select_workspace(workspace_id.clone());
        let mut git = GitWorkflowController::new(backend.clone(), capabilities.git.clone());
        git.select_workspace(workspace_id.clone());
        let terminal = TerminalWorkflowController::new(
            backend.clone(),
            TerminalWorkflowCapabilities::from_backend(&capabilities),
        );
        let management = ManagementWorkflowController::new(
            backend.clone(),
            backend.clone(),
            ManagementWorkflowCapabilities::from_backend(&capabilities),
        );
        let mut workbench = Self {
            backend,
            workspace_id,
            surface: WorkbenchSurface::Files,
            files,
            git,
            terminal,
            management,
            file_search_input: cx
                .new(|cx| TextInput::new(locale::text("Search files", "搜索文件", "搜尋檔案"), cx)),
            file_search_mode: MobileFileSearchMode::Name,
            file_editor_input: cx.new(|cx| {
                TextInput::new(locale::text("File content", "文件内容", "檔案內容"), cx).multiline()
            }),
            git_commit_input: cx.new(|cx| {
                TextInput::new(locale::text("Commit message", "提交消息", "提交訊息"), cx)
            }),
            git_history_query_input: cx.new(|cx| {
                TextInput::new(
                    locale::text(
                        "Search commit information or code",
                        "搜索提交信息或代码",
                        "搜尋提交資訊或代碼",
                    ),
                    cx,
                )
            }),
            terminal_input: cx.new(|cx| {
                TextInput::new(locale::text("Type a command", "输入命令", "輸入命令"), cx)
            }),
            file_editor_path: None,
            git_diff: None,
            git_commit_confirmation: false,
            git_history_loading: false,
            git_history_request_generation: 0,
            terminal_snapshot: None,
            terminal_close_confirmation: None,
            agent_summaries: Vec::new(),
            runtime_session_id: session_id,
            runtime_catalog: None,
            runtime_state: None,
            runtime_draft: None,
            runtime_feature_inputs: BTreeMap::new(),
            terminal_poll_generation: 0,
            management_request_generation: 0,
            runtime_request_generation: 0,
            management_busy_generation: None,
            runtime_busy_generation: None,
            busy: false,
            notice: None,
            error: None,
            tasks: Vec::new(),
        };
        workbench.refresh_all(cx);
        workbench
    }

    pub fn set_surface(&mut self, surface: WorkbenchSurface, cx: &mut Context<Self>) {
        if self.surface == WorkbenchSurface::Terminal && surface != WorkbenchSurface::Terminal {
            self.stop_terminal_poll();
        }
        self.sync_capabilities();
        self.surface = surface;
        match surface {
            WorkbenchSurface::Files => self.refresh_files(cx),
            WorkbenchSurface::Git => self.refresh_git(cx),
            WorkbenchSurface::Terminal => self.refresh_terminals(cx),
            WorkbenchSurface::Providers => self.refresh_management(cx),
            WorkbenchSurface::Runtime => self.refresh_runtime(cx),
        }
        cx.notify();
    }

    pub fn set_workspace(&mut self, workspace_id: WorkspaceId, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.files.select_workspace(workspace_id.clone());
        self.git.select_workspace(workspace_id);
        self.file_editor_path = None;
        self.git_diff = None;
        self.terminal_snapshot = None;
        self.terminal_close_confirmation = None;
        self.git_history_loading = false;
        self.git_history_request_generation = self.git_history_request_generation.wrapping_add(1);
        self.git_history_query_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.stop_terminal_poll();
        self.refresh_all(cx);
    }

    pub fn set_session(&mut self, session_id: Option<VibexSessionId>, cx: &mut Context<Self>) {
        if self.runtime_session_id == session_id {
            return;
        }
        self.runtime_session_id = session_id;
        self.runtime_state = None;
        self.runtime_draft = None;
        self.runtime_feature_inputs.clear();
        self.refresh_runtime(cx);
    }

    pub fn suspend(&mut self) {
        self.stop_terminal_poll();
        self.management_request_generation =
            self.management_request_generation.saturating_add(1).max(1);
        self.runtime_request_generation = self.runtime_request_generation.saturating_add(1).max(1);
        let management_busy = self.management_busy_generation.take().is_some();
        let runtime_busy = self.runtime_busy_generation.take().is_some();
        if management_busy || runtime_busy {
            self.busy = false;
        }
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.refresh_all(cx);
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.sync_capabilities();
        self.refresh_files(cx);
        self.refresh_git(cx);
        self.refresh_terminals(cx);
        self.refresh_management(cx);
        self.refresh_runtime(cx);
    }

    fn sync_capabilities(&mut self) {
        let capabilities = self.backend.capability_snapshot();
        self.files.set_capabilities(capabilities.file.clone());
        self.git.set_capabilities(capabilities.git.clone());
        self.terminal
            .set_capabilities(TerminalWorkflowCapabilities::from_backend(&capabilities));
        self.management
            .set_capabilities(ManagementWorkflowCapabilities::from_backend(&capabilities));
    }

    fn refresh_active_surface(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.set_surface(self.surface, cx);
    }

    fn refresh_files(&mut self, cx: &mut Context<Self>) {
        self.load_file_tree_path(String::new(), cx);
    }

    fn load_file_tree_path(&mut self, path: String, cx: &mut Context<Self>) {
        let ticket = match self.files.begin_tree_load(&path) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.load_tree(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_tree_load(&ticket, outcome);
                this.error = this.files.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn select_file(&mut self, path: String, cx: &mut Context<Self>) {
        let ticket = match self.files.begin_open_file(path) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.read_file(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_file_read(&ticket, outcome);
                this.error = this.files.state.last_error.clone();
                if let Some(content) = this.files.state.view().editor_content {
                    this.file_editor_path = Some(ticket.path.clone());
                    this.file_editor_input
                        .update(cx, |input, cx| input.set_text(content, cx));
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    /// Opens a workspace-relative file from another mobile surface.
    pub fn open_file(&mut self, path: String, cx: &mut Context<Self>) {
        self.select_file(path, cx);
    }

    fn activate_file_row(
        &mut self,
        path: String,
        path_chain: Vec<String>,
        kind: FileEntryKind,
        cx: &mut Context<Self>,
    ) {
        if kind == FileEntryKind::Directory {
            let was_expanded = self.files.state.tree.chain_is_expanded(&path_chain);
            if self
                .files
                .state
                .tree
                .set_chain_expanded(&path_chain, !was_expanded)
                && !was_expanded
            {
                self.load_file_tree_path(path, cx);
            } else {
                cx.notify();
            }
        } else {
            self.select_file(path, cx);
        }
    }

    fn start_file_search(&mut self, cx: &mut Context<Self>) {
        let query = self.file_search_input.read(cx).text().trim().to_string();
        if query.is_empty() {
            self.files.state.search.clear();
            cx.notify();
            return;
        }
        if let Err(error) = self.files.begin_search() {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let generation = self.files.state.generation;
        let request = FileSearchRequest {
            workspace_id: self.workspace_id.clone(),
            query,
            include_content: self.file_search_mode == MobileFileSearchMode::Content,
            case_sensitive: false,
            whole_word: false,
            regex: false,
            limit: Some(100),
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.search_files(request));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_search(generation, outcome);
                this.error = this.files.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn search_files(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.start_file_search(cx);
    }

    fn toggle_file_search_mode(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.file_search_mode = self.file_search_mode.toggle();
        if !self.file_search_input.read(cx).text().trim().is_empty() {
            self.start_file_search(cx);
        } else {
            cx.notify();
        }
    }

    fn clear_file_search(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.file_search_input
            .update(cx, |input, cx| input.set_text("", cx));
        self.files.state.search.clear();
        cx.notify();
    }

    fn save_file(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let content = self.file_editor_input.read(cx).text().to_string();
        if let Err(error) = self.files.update_active_content(content) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let operation = match self.files.begin_save_active() {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        self.error = None;
        let runner = gpui_tokio::Tokio::spawn(cx, self.files.save_file(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.files.apply_save_outcome(&operation, outcome);
                this.busy = false;
                this.error = this.files.state.last_error.clone();
                if this.error.is_none() {
                    this.notice = Some(locale::common("File saved on desktop").to_string());
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn reload_desktop_file(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.files.state.reload_server_version() {
            self.error = Some(BackendError::conflict(
                "mobile_file_conflict_version_unavailable",
                locale::text(
                    "The desktop conflict version is no longer available.",
                    "桌面端的冲突版本已不可用。",
                    "桌面版的衝突版本已無法使用。",
                ),
            ));
            cx.notify();
            return;
        }
        if let Some(content) = self.files.state.view().editor_content {
            self.file_editor_input
                .update(cx, |input, cx| input.set_text(content, cx));
        }
        self.error = None;
        self.notice = Some(locale::common("Desktop file version loaded").to_string());
        cx.notify();
    }

    fn refresh_git(&mut self, cx: &mut Context<Self>) {
        let ticket = match self.git.begin_status_load() {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.load_status(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_status(&ticket, outcome);
                this.error = this.git.state.last_error.clone();
                if let Some(status) = this.git.state.model.status.as_ref() {
                    this.files.state.tree.set_git_changes(&status.changes);
                }
                if this.git.state.model.mode == GitWorkbenchMode::History {
                    this.refresh_git_history(false, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn can_mutate_git(&self) -> bool {
        self.backend
            .permits_remote_action(RemoteActionClass::MutateGit)
    }

    fn set_git_mode(&mut self, mode: GitWorkbenchMode, cx: &mut Context<Self>) {
        self.git.state.model.set_mode(mode);
        if mode == GitWorkbenchMode::History {
            self.refresh_git_history(false, cx);
        }
        cx.notify();
    }

    fn refresh_git_history(&mut self, append: bool, cx: &mut Context<Self>) {
        if self.git_history_loading && !append {
            self.git_history_request_generation =
                self.git_history_request_generation.wrapping_add(1);
        }
        let mut filter = self.git.state.model.history_filter.clone();
        if filter.ref_name.is_none() {
            let branch = self
                .git
                .state
                .model
                .status
                .as_ref()
                .and_then(|status| status.branch.clone());
            if branch.is_some() {
                filter.ref_name = branch;
                self.git.state.model.set_history_filter(filter.clone());
            }
        }
        let before_commit = append
            .then(|| {
                self.git
                    .state
                    .model
                    .history
                    .last()
                    .map(|commit| commit.hash.clone())
            })
            .flatten();
        let key = format!(
            "{}:{}:{}:{}:{}:{}",
            filter.ref_name.as_deref().unwrap_or_default(),
            filter.author.as_deref().unwrap_or_default(),
            filter.query.as_deref().unwrap_or_default(),
            filter.authored_after_ms.unwrap_or_default(),
            filter.authored_before_ms.unwrap_or_default(),
            before_commit.as_deref().unwrap_or_default(),
        );
        let Some(ticket) = self.git.state.model.begin_query(GitQueryKind::History, key) else {
            return;
        };
        let request = GitHistoryRequest {
            workspace_id: self.workspace_id.clone(),
            limit: Some(60),
            before_commit,
            ref_name: filter.ref_name,
            author: filter.author,
            query: filter.query,
            authored_after_ms: filter.authored_after_ms,
            authored_before_ms: filter.authored_before_ms,
        };
        self.git_history_loading = true;
        self.git_history_request_generation = self.git_history_request_generation.wrapping_add(1);
        let generation = self.git_history_request_generation;
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_history(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if generation != this.git_history_request_generation {
                    return;
                }
                this.git_history_loading = false;
                match outcome {
                    Ok(history) => {
                        if !this.git.state.model.apply_history(&ticket, history, append) {
                            return;
                        }
                        this.error = None;
                    }
                    Err(error) => {
                        this.git.state.model.fail_query(&ticket, &error.code);
                        this.error = Some(error);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn search_git_history(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let query = self
            .git_history_query_input
            .read(cx)
            .text()
            .trim()
            .to_string();
        let mut filter = self.git.state.model.history_filter.clone();
        filter.query = (!query.is_empty()).then_some(query);
        self.git.state.model.set_history_filter(filter);
        self.refresh_git_history(false, cx);
    }

    fn clear_git_history_search(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git_history_query_input
            .update(cx, |input, cx| input.set_text("", cx));
        let mut filter = self.git.state.model.history_filter.clone();
        filter.query = None;
        self.git.state.model.set_history_filter(filter);
        self.refresh_git_history(false, cx);
    }

    fn toggle_git_history_author(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let authors = self.git.state.model.history_authors.clone();
        let current = self.git.state.model.history_filter.author.clone();
        let next = if authors.is_empty() {
            None
        } else if let Some(current) = current {
            authors
                .iter()
                .position(|author| author.email == current || author.name == current)
                .and_then(|index| authors.get(index.saturating_add(1)))
                .map(|author| author.email.clone())
        } else {
            authors.first().map(|author| author.email.clone())
        };
        let mut filter = self.git.state.model.history_filter.clone();
        filter.author = next;
        self.git.state.model.set_history_filter(filter);
        self.refresh_git_history(false, cx);
    }

    fn load_more_git_history(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self.git_history_loading && self.git.state.model.history_has_more {
            self.refresh_git_history(true, cx);
        }
    }

    fn run_git_remote_action(&mut self, kind: GitRemoteActionKind, cx: &mut Context<Self>) {
        if self.busy
            || !self
                .git
                .capabilities()
                .supports(BackendOperation::GitStatus)
            || !self.can_mutate_git()
        {
            return;
        }
        let request = GitRemoteActionRequest {
            workspace_id: self.workspace_id.clone(),
            kind,
            remote: None,
            branch: None,
        };
        self.busy = true;
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_remote_action(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(result) => {
                        // Reconcile through the normal ticketed status path so
                        // the shared tree and selection model stay authoritative.
                        this.refresh_git(cx);
                        this.notice = Some(result.summary);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn revert_selected_git(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.busy
            || !self
                .git
                .capabilities()
                .supports(BackendOperation::GitStatus)
            || !self.can_mutate_git()
        {
            return;
        }
        let paths = self.git.state.model.selected_change_paths();
        if paths.is_empty() {
            return;
        }
        let request = GitStageRequest {
            workspace_id: self.workspace_id.clone(),
            paths,
        };
        self.busy = true;
        let backend = self.backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move { backend.git_revert(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.busy = false;
                match outcome {
                    Ok(_) => {
                        this.refresh_git(cx);
                        this.notice =
                            Some(locale::common("Selected changes rolled back").to_string());
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn open_git_diff(&mut self, path: String, staged: bool, cx: &mut Context<Self>) {
        let ticket = match self.git.begin_diff_load(path, staged) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.load_diff(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let display = outcome.as_ref().ok().cloned();
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_diff(&ticket, outcome);
                this.git_diff = display;
                this.error = this.git.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn mutate_git_path(&mut self, path: String, stage: bool, cx: &mut Context<Self>) {
        let operation = if stage {
            self.git.begin_stage(vec![path])
        } else {
            self.git.begin_unstage(vec![path])
        };
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.run_paths_mutation(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_paths_mutation(&operation, outcome);
                this.busy = false;
                this.error = this.git.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn request_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if !self
            .git
            .capabilities()
            .supports(BackendOperation::GitCommit)
            || !self.can_mutate_git()
        {
            return;
        }
        let message = self.git_commit_input.read(cx).text().trim().to_string();
        let paths = self.git.state.model.selected_change_paths();
        if paths.is_empty() {
            self.error = Some(BackendError::failed(
                "git_paths_empty",
                locale::text(
                    "Select at least one change first.",
                    "请先选择至少一项更改。",
                    "請先選擇至少一項變更。",
                ),
            ));
            cx.notify();
            return;
        }
        match self.git.request_commit_confirmation(message, paths) {
            Ok(_) => {
                self.git_commit_confirmation = true;
                self.error = None;
            }
            Err(error) => self.error = Some(error),
        }
        cx.notify();
    }

    fn cancel_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.git.cancel_commit();
        self.git_commit_confirmation = false;
        cx.notify();
    }

    fn confirm_git_commit(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let operation = self
            .git
            .confirm_commit()
            .and_then(|()| self.git.begin_confirmed_commit());
        let operation = match operation {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.git_commit_confirmation = false;
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.git.run_commit(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git.apply_commit(&operation, outcome);
                this.busy = false;
                this.error = this.git.state.last_error.clone();
                if this.error.is_none() {
                    this.git_commit_input.update(cx, |input, cx| {
                        let _ = input.take(cx);
                    });
                    this.notice = Some(locale::common("Commit created on desktop").to_string());
                    this.refresh_git(cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_terminals(&mut self, cx: &mut Context<Self>) {
        self.stop_terminal_poll();
        let ticket = match self.terminal.begin_refresh(self.workspace_id.clone()) {
            Ok(ticket) => ticket,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.load_sessions(ticket.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_refresh(&ticket, outcome);
                this.error = this.terminal.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn create_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let request = MutationRequest::new(TerminalCreateRequest {
            workspace_id: self.workspace_id.clone(),
            title: Some(locale::text("Mobile terminal", "移动端终端", "行動端終端機").to_string()),
            shell: None,
            cwd: None,
            rows: 24,
            cols: 80,
        });
        let operation = match self.terminal.begin_create(request) {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_create(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let created_id = outcome.as_ref().ok().map(|session| session.id.clone());
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_create(&operation, outcome);
                this.busy = false;
                this.error = this.terminal.state.last_error.clone();
                if let Some(terminal_id) = created_id {
                    this.attach_terminal(terminal_id, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn attach_terminal(&mut self, terminal_id: TerminalId, cx: &mut Context<Self>) {
        self.terminal_close_confirmation = None;
        match self.terminal.attach(terminal_id.clone()) {
            Ok(()) => {
                self.error = None;
                self.start_terminal_poll(terminal_id, cx);
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
            }
        }
    }

    fn stop_terminal_poll(&mut self) {
        self.terminal_poll_generation = self.terminal_poll_generation.saturating_add(1).max(1);
    }

    fn start_terminal_poll(&mut self, terminal_id: TerminalId, cx: &mut Context<Self>) {
        self.stop_terminal_poll();
        let generation = self.terminal_poll_generation;
        self.poll_terminal_snapshot(terminal_id, generation, cx);
    }

    fn poll_terminal_snapshot(
        &mut self,
        terminal_id: TerminalId,
        generation: u64,
        cx: &mut Context<Self>,
    ) {
        if generation != self.terminal_poll_generation {
            return;
        }
        let backend = self.backend.clone();
        let requested_terminal_id = terminal_id.clone();
        let background = cx.background_executor().clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            backend.terminal_snapshot(requested_terminal_id).await
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let continue_polling = entity
                .update(cx, |this, cx| {
                    let current = generation == this.terminal_poll_generation
                        && this.surface == WorkbenchSurface::Terminal
                        && this
                            .terminal
                            .state
                            .active_session
                            .as_ref()
                            .is_some_and(|session| session.id == terminal_id);
                    if !current {
                        return false;
                    }
                    match outcome {
                        Ok(snapshot) => {
                            this.terminal_snapshot = Some(snapshot);
                            this.error = None;
                        }
                        Err(error) => this.error = Some(error),
                    }
                    cx.notify();
                    true
                })
                .unwrap_or(false);
            if !continue_polling {
                return;
            }
            background.timer(TERMINAL_POLL_INTERVAL).await;
            let _ = entity.update(cx, |this, cx| {
                if generation == this.terminal_poll_generation
                    && this.surface == WorkbenchSurface::Terminal
                    && this
                        .terminal
                        .state
                        .active_session
                        .as_ref()
                        .is_some_and(|session| session.id == terminal_id)
                {
                    this.poll_terminal_snapshot(terminal_id.clone(), generation, cx);
                }
            });
        });
        task.detach();
    }

    fn refresh_terminal_snapshot(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(terminal_id) = self
            .terminal
            .state
            .active_session
            .as_ref()
            .map(|session| session.id.clone())
        {
            self.start_terminal_poll(terminal_id, cx);
        }
    }

    fn resize_terminal_by(&mut self, row_delta: i16, col_delta: i16, cx: &mut Context<Self>) {
        let Some(session) = self.terminal.state.active_session.as_ref() else {
            return;
        };
        let rows = (i32::from(session.rows) + i32::from(row_delta)).clamp(4, 200) as u16;
        let cols = (i32::from(session.cols) + i32::from(col_delta)).clamp(20, 400) as u16;
        if rows == session.rows && cols == session.cols {
            return;
        }
        let operation = match self.terminal.begin_resize(rows, cols) {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_resize(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_resize(&operation, outcome);
                this.busy = false;
                this.error = this.terminal.state.last_error.clone();
                if this.error.is_none() {
                    this.notice = Some(format!(
                        "{} {cols} x {rows}",
                        locale::text("Terminal resized to", "终端已调整为", "終端機已調整為")
                    ));
                    if let Some(terminal_id) = this
                        .terminal
                        .state
                        .active_session
                        .as_ref()
                        .map(|session| session.id.clone())
                    {
                        this.start_terminal_poll(terminal_id, cx);
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn resize_terminal_rows_down(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_terminal_by(-4, 0, cx);
    }

    fn resize_terminal_rows_up(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_terminal_by(4, 0, cx);
    }

    fn resize_terminal_cols_down(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_terminal_by(0, -8, cx);
    }

    fn resize_terminal_cols_up(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.resize_terminal_by(0, 8, cx);
    }

    fn send_terminal_input(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let value = self.terminal_input.read(cx).text().to_string();
        if value.is_empty() {
            return;
        }
        self.send_terminal_value(TerminalInput::Text(format!("{value}\r")), true, cx);
    }

    fn send_terminal_key(&mut self, key: TerminalKey, cx: &mut Context<Self>) {
        self.send_terminal_value(
            TerminalInput::Key(key, TerminalKeyModifiers::default()),
            false,
            cx,
        );
    }

    fn send_terminal_value(
        &mut self,
        input: TerminalInput,
        clear_input: bool,
        cx: &mut Context<Self>,
    ) {
        let operation = match self.terminal.begin_send_input(input) {
            Ok(Some(operation)) => operation,
            Ok(None) => {
                cx.notify();
                return;
            }
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_input(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_input(&operation, outcome);
                this.busy = false;
                this.error = this.terminal.state.last_error.clone();
                if clear_input && this.error.is_none() {
                    this.terminal_input.update(cx, |input, cx| {
                        let _ = input.take(cx);
                    });
                }
                if let Some(terminal_id) = this
                    .terminal
                    .state
                    .active_session
                    .as_ref()
                    .map(|session| session.id.clone())
                {
                    this.start_terminal_poll(terminal_id, cx);
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn request_close_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal_close_confirmation = self
            .terminal
            .state
            .active_session
            .as_ref()
            .map(|session| session.id.clone());
        cx.notify();
    }

    fn cancel_close_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.terminal_close_confirmation = None;
        cx.notify();
    }

    fn confirm_close_terminal(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let Some(confirmed_id) = self.terminal_close_confirmation.take() else {
            return;
        };
        if !self
            .terminal
            .state
            .active_session
            .as_ref()
            .is_some_and(|session| session.id == confirmed_id)
        {
            self.error = Some(BackendError::conflict(
                "mobile_terminal_close_target_changed",
                locale::text(
                    "The selected terminal changed before close confirmation.",
                    "确认关闭前，所选终端已发生变化。",
                    "確認關閉前，所選終端機已變更。",
                ),
            ));
            cx.notify();
            return;
        }
        let operation = match self.terminal.begin_close() {
            Ok(operation) => operation,
            Err(error) => {
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_close(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.terminal.apply_close(&operation, outcome);
                this.busy = false;
                this.terminal_snapshot = None;
                this.error = this.terminal.state.last_error.clone();
                if this.error.is_none() {
                    this.stop_terminal_poll();
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_management(&mut self, cx: &mut Context<Self>) {
        self.management_request_generation =
            self.management_request_generation.saturating_add(1).max(1);
        let generation = self.management_request_generation;
        if self.management_busy_generation.take().is_some() {
            self.busy = false;
        }
        let mut controller = self.management.clone();
        let backend = self.backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let result = controller.refresh().await;
            let agent_summaries = backend.list_agent_config_summaries(true).await;
            (controller, result, agent_summaries)
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if generation != this.management_request_generation {
                    return;
                }
                match outcome {
                    Ok((controller, result, agent_summaries)) => {
                        this.management = controller;
                        this.error = result.err();
                        match agent_summaries {
                            Ok(summaries) => this.agent_summaries = summaries,
                            Err(error) if this.error.is_none() => this.error = Some(error),
                            Err(_) => {}
                        }
                    }
                    Err(_) => {
                        this.error = Some(background_task_error());
                    }
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn run_health_probes(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.management_request_generation =
            self.management_request_generation.saturating_add(1).max(1);
        let generation = self.management_request_generation;
        self.management_busy_generation = Some(generation);
        let mut controller = self.management.clone();
        self.busy = true;
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let result = controller
                .run_health_probes(MutationRequest::new(ProviderRunHealthProbesRequest {
                    provider_profile_ids: None,
                    probe_kinds: None,
                }))
                .await;
            (controller, result)
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = runner.await;
            let _ = entity.update(cx, |this, cx| {
                if generation != this.management_request_generation {
                    return;
                }
                this.management_busy_generation = None;
                this.busy = false;
                match outcome {
                    Ok((controller, result)) => {
                        this.management = controller;
                        this.error = result.err();
                        if this.error.is_none() {
                            this.notice = Some(
                                locale::common("Provider health probes completed").to_string(),
                            );
                        }
                    }
                    Err(_) => this.error = Some(background_task_error()),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn refresh_runtime(&mut self, cx: &mut Context<Self>) {
        self.runtime_request_generation = self.runtime_request_generation.saturating_add(1).max(1);
        let generation = self.runtime_request_generation;
        if self.runtime_busy_generation.take().is_some() {
            self.busy = false;
        }
        let Some(session_id) = self.runtime_session_id.clone() else {
            self.runtime_catalog = None;
            self.runtime_state = None;
            self.runtime_draft = None;
            cx.notify();
            return;
        };
        let requested_session_id = session_id.clone();
        let backend = self.backend.clone();
        let runner = gpui_tokio::Tokio::spawn(cx, async move {
            let catalog = backend.list_runtime_options().await?;
            let state = backend.runtime_selection(session_id).await?;
            Ok::<_, BackendError>((catalog, state))
        });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if generation != this.runtime_request_generation
                    || this.runtime_session_id.as_ref() != Some(&requested_session_id)
                {
                    return;
                }
                match outcome {
                    Ok((catalog, state)) => {
                        this.runtime_draft = Some(state.desired.clone());
                        this.runtime_catalog = Some(catalog);
                        this.runtime_state = Some(state);
                        this.sync_runtime_feature_inputs(cx);
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
    }

    fn choose_runtime_option(
        &mut self,
        selection: SessionRuntimeSelection,
        cx: &mut Context<Self>,
    ) {
        self.runtime_draft = Some(selection);
        self.sync_runtime_feature_inputs(cx);
        self.error = None;
        cx.notify();
    }

    fn sync_runtime_feature_inputs(&mut self, cx: &mut Context<Self>) {
        let features = self
            .runtime_catalog
            .as_ref()
            .and_then(|catalog| {
                self.runtime_draft
                    .as_ref()
                    .and_then(|draft| matching_runtime_option(&catalog.options, draft))
            })
            .map(|option| option.features.clone())
            .unwrap_or_default();
        let config_values = self
            .runtime_draft
            .as_ref()
            .map(|draft| draft.config_values.clone())
            .unwrap_or_default();
        self.runtime_feature_inputs = features
            .into_iter()
            .filter(|feature| feature.kind == SessionRuntimeFeatureKind::String)
            .map(|feature| {
                let value = config_values.get(&feature.id).cloned().unwrap_or_default();
                let input = cx.new(|cx| {
                    let mut input = TextInput::new(locale::common("Value"), cx);
                    input.set_text(value, cx);
                    input
                });
                (feature.id, input)
            })
            .collect();
    }

    fn apply_runtime_feature_inputs(&mut self, cx: &mut Context<Self>) -> BackendResult<()> {
        let Some(draft) = self.runtime_draft.as_mut() else {
            return Ok(());
        };
        for (feature_id, input) in &self.runtime_feature_inputs {
            let value = input.read(cx).text().to_string();
            match runtime_string_override(value)? {
                Some(value) => {
                    draft.config_values.insert(feature_id.clone(), value);
                }
                None => {
                    draft.config_values.remove(feature_id);
                }
            }
        }
        Ok(())
    }

    fn choose_runtime_reasoning(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if let Some(draft) = self.runtime_draft.as_mut() {
            draft.reasoning_effort = value;
        }
        cx.notify();
    }

    fn choose_runtime_mode(&mut self, value: Option<String>, cx: &mut Context<Self>) {
        if let Some(draft) = self.runtime_draft.as_mut() {
            draft.mode_id = value;
        }
        cx.notify();
    }

    fn choose_runtime_feature(
        &mut self,
        id: String,
        value: Option<String>,
        cx: &mut Context<Self>,
    ) {
        if let Some(draft) = self.runtime_draft.as_mut() {
            if let Some(value) = value {
                draft.config_values.insert(id, value);
            } else {
                draft.config_values.remove(&id);
            }
        }
        cx.notify();
    }

    fn apply_runtime(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if let Err(error) = self.apply_runtime_feature_inputs(cx) {
            self.error = Some(error);
            cx.notify();
            return;
        }
        let (Some(session_id), Some(state), Some(desired)) = (
            self.runtime_session_id.clone(),
            self.runtime_state.clone(),
            self.runtime_draft.clone(),
        ) else {
            return;
        };
        self.runtime_request_generation = self.runtime_request_generation.saturating_add(1).max(1);
        let generation = self.runtime_request_generation;
        self.runtime_busy_generation = Some(generation);
        let requested_session_id = session_id.clone();
        let request = MutationRequest::new(SetDesiredAgentSessionRuntimeRequest {
            session_id,
            idempotency_key: RequestId::new().into_string(),
            expected_revision: state.session_revision,
            expected_selection_revision: state.selection_revision,
            desired,
            interaction: RuntimeSelectionInteraction::Seamless,
        });
        let backend = self.backend.clone();
        self.busy = true;
        let runner =
            gpui_tokio::Tokio::spawn(
                cx,
                async move { backend.set_desired_runtime(request).await },
            );
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                if generation != this.runtime_request_generation
                    || this.runtime_session_id.as_ref() != Some(&requested_session_id)
                {
                    return;
                }
                this.runtime_busy_generation = None;
                this.busy = false;
                match outcome {
                    Ok(state) => {
                        this.runtime_draft = Some(state.desired.clone());
                        this.runtime_state = Some(state);
                        this.sync_runtime_feature_inputs(cx);
                        this.notice =
                            Some(locale::common("Runtime selection sent to desktop").to_string());
                        this.error = None;
                    }
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }
}

impl MobileWorkbench {
    fn render_files(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.files.state.view();
        let input_dirty = view
            .editor_content
            .is_some_and(|content| self.file_editor_input.read(cx).text() != content);
        let status = if input_dirty {
            "Unsaved"
        } else {
            file_status_label(view.status)
        };
        let status_color = if input_dirty {
            rgb(theme::ACCENT_YELLOW).into()
        } else {
            file_status_color(view.status)
        };
        let selected_path = view.selected_path.clone();
        let rows = view.rows.clone();
        let search = view.search.clone();
        let query_present = !self.file_search_input.read(cx).text().trim().is_empty();
        let search_loading = self.files.state.search.is_loading();
        let search_has_results = !search.is_empty();
        let has_conflict = view.status == FileEditorStatus::Conflict;
        let can_write = self
            .files
            .capabilities()
            .supports(BackendOperation::FileWrite);

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .p_2()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .h(px(32.0))
                            .min_w_0()
                            .flex_1()
                            .rounded(px(theme::RADIUS_CONTROL))
                            .border_1()
                            .border_color(theme::border_default())
                            .bg(theme::workbench_panel_bg())
                            .px_2()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                svg()
                                    .path("icons/search.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_muted()),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .flex_1()
                                    .child(self.file_search_input.clone()),
                            )
                            .when(search_loading, |bar| {
                                bar.child(
                                    svg()
                                        .path("icons/loader-circle.svg")
                                        .size(px(theme::ICON_SM))
                                        .text_color(theme::text_primary()),
                                )
                            })
                            .when(query_present, |bar| {
                                bar.child(
                                    icon_button("clear-file-search", "icons/x.svg", "Clear search")
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::clear_file_search),
                                        ),
                                )
                            }),
                    )
                    .child(
                        compact_action(
                            "toggle-file-search-mode",
                            self.file_search_mode.label().to_string(),
                        )
                        .h(px(32.0))
                        .when(query_present, |button| {
                            button.on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::toggle_file_search_mode),
                            )
                        })
                        .when(!query_present, |button| {
                            button.on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::toggle_file_search_mode),
                            )
                        }),
                    ),
            )
            .child(
                div()
                    .id("mobile-files-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(!query_present, |body| {
                        body.children(
                            rows.into_iter()
                                .map(|row| self.render_file_tree_row(row, cx)),
                        )
                    })
                    .when(query_present && search_loading, |body| {
                        body.child(empty_label("Searching"))
                    })
                    .when(
                        query_present && !search_loading && !search_has_results,
                        |body| {
                            body.child(empty_label(
                                if self.file_search_mode == MobileFileSearchMode::Name {
                                    "No matching items"
                                } else {
                                    "No matching files"
                                },
                            ))
                        },
                    )
                    .when(query_present && search_has_results, |body| {
                        body.child(
                            div()
                                .h(px(30.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .border_t_1()
                                .border_b_1()
                                .border_color(theme::border_subtle())
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(theme::text_muted())
                                .child(format!("{} {}", search.len(), locale::common("results"))),
                        )
                        .children(search.into_iter().map(|result| {
                            let path = result.path.clone();
                            let line = result.line.unwrap_or_default();
                            let select_path = path.clone();
                            div()
                                .id(format!("file-search-result:{path}:{line}"))
                                .min_h(px(36.0))
                                .px_3()
                                .py_1()
                                .flex()
                                .items_center()
                                .gap_2()
                                .cursor_pointer()
                                .active(|style| style.bg(theme::row_pressed_bg()))
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(move |this, _, _, cx| {
                                        this.select_file(select_path.clone(), cx)
                                    }),
                                )
                                .child(mobile_file_tree_icon(
                                    file_icon_descriptor(&path, FileEntryKind::File),
                                    false,
                                    false,
                                ))
                                .child({
                                    let display_path = if line > 0 {
                                        format!("{path}:{line}")
                                    } else {
                                        path.clone()
                                    };
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(theme::text_secondary())
                                        .child(display_path)
                                })
                                .when_some(result.snippet, |row, snippet| {
                                    row.child(
                                        div()
                                            .max_w(px(120.0))
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(snippet),
                                    )
                                })
                                .into_any_element()
                        }))
                    })
                    .when(!query_present, |body| {
                        body.when_some(selected_path, |body, path| {
                            body.child(
                                div()
                                    .h(px(30.0))
                                    .px_3()
                                    .flex()
                                    .items_center()
                                    .border_t_1()
                                    .border_color(theme::border_subtle())
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child(locale::common("Editor")),
                            )
                            .child(
                                div()
                                    .px_3()
                                    .pb_3()
                                    .flex()
                                    .flex_col()
                                    .gap_2()
                                    .child(
                                        div()
                                            .flex()
                                            .items_center()
                                            .justify_between()
                                            .gap_2()
                                            .child(
                                                div()
                                                    .flex_1()
                                                    .min_w_0()
                                                    .overflow_hidden()
                                                    .text_ellipsis()
                                                    .whitespace_nowrap()
                                                    .text_size(px(theme::FONT_CAPTION))
                                                    .text_color(theme::text_muted())
                                                    .child(path),
                                            )
                                            .child(
                                                div()
                                                    .text_size(px(theme::FONT_MICRO))
                                                    .text_color(status_color)
                                                    .child(status),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .h(px(240.0))
                                            .rounded(px(theme::RADIUS_CONTROL))
                                            .border_1()
                                            .border_color(theme::border_default())
                                            .bg(theme::bg_card())
                                            .overflow_hidden()
                                            .child(self.file_editor_input.clone()),
                                    )
                                    .child(
                                        div()
                                            .flex()
                                            .justify_end()
                                            .gap_2()
                                            .when(has_conflict, |actions| {
                                                actions.child(
                                                    action_button(
                                                        "reload-desktop-file",
                                                        "Use desktop version",
                                                    )
                                                    .on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::reload_desktop_file),
                                                    ),
                                                )
                                            })
                                            .child(
                                                action_button(
                                                    "save-file",
                                                    if !can_write {
                                                        "Read only"
                                                    } else if self.busy {
                                                        "Saving..."
                                                    } else {
                                                        "Save"
                                                    },
                                                )
                                                .when(!self.busy && can_write, |button| {
                                                    button.on_mouse_up(
                                                        MouseButton::Left,
                                                        cx.listener(Self::save_file),
                                                    )
                                                })
                                                .when(!can_write, |button| button.opacity(0.55)),
                                            ),
                                    ),
                            )
                        })
                    }),
            )
            .into_any_element()
    }

    fn render_file_tree_row(
        &self,
        row: vibex_desktop_model::FileExplorerRow,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let path = row.path.clone();
        let kind = row.kind;
        let path_chain = row.path_chain.clone();
        let is_directory = kind == FileEntryKind::Directory;
        let text_color = file_tree_row_text_color(&row);
        let label = if is_directory && !row.segments.is_empty() {
            row.segments
                .iter()
                .map(|segment| segment.name.as_str())
                .collect::<Vec<_>>()
                .join(" / ")
        } else {
            row.name.clone()
        };
        let status = row.git.map(|git| git.signal);
        let loading = matches!(
            row.load_state,
            vibex_desktop_model::FileTreeLoadState::Loading
        );
        div()
            .id(format!("file:{}", row.id))
            .relative()
            .min_h(px(30.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .when(row.selected, |item| item.bg(theme::sidebar_selected_bg()))
            .when(!row.selected, |item| {
                item.hover(|style| style.bg(theme::row_pressed_bg()))
            })
            .cursor_pointer()
            .active(|style| style.bg(theme::row_pressed_bg()))
            .children(file_tree_guides_mobile(row.depth))
            .child(div().w(px(row.depth as f32 * 20.0)).flex_none())
            .child(
                div()
                    .w(px(14.0))
                    .h(px(20.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_directory {
                        svg()
                            .path(if row.expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(12.0))
                            .text_color(theme::text_muted())
                    } else {
                        svg()
                            .path("icons/chevron-right.svg")
                            .size(px(12.0))
                            .text_color(theme::workbench_bg())
                    }),
            )
            .child(mobile_file_tree_icon(row.icon, row.ignored, row.expanded))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_BODY))
                    .text_color(text_color)
                    .child(label),
            )
            .when(loading, |item| {
                item.child(
                    svg()
                        .path("icons/loader-circle.svg")
                        .size(px(theme::ICON_SM))
                        .text_color(theme::text_primary()),
                )
            })
            .when_some(status, |item, signal| {
                item.child(
                    div()
                        .w(px(16.0))
                        .flex_none()
                        .font_family("monospace")
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(file_git_signal_color(signal))
                        .child(signal.short_label()),
                )
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.activate_file_row(path.clone(), path_chain.clone(), kind, cx)
                }),
            )
            .into_any_element()
    }

    fn render_git(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let mode = self.git.state.model.mode;
        let changes_active = mode == GitWorkbenchMode::Changes;
        let history_active = mode == GitWorkbenchMode::History;
        let status = self.git.state.model.status.clone();
        let status_loading = status.is_none();
        let additions = status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .map(|change| change.additions)
                    .sum::<u32>()
            })
            .unwrap_or_default();
        let deletions = status
            .as_ref()
            .map(|status| {
                status
                    .changes
                    .iter()
                    .map(|change| change.deletions)
                    .sum::<u32>()
            })
            .unwrap_or_default();
        let change_count = status.as_ref().map_or(0, |status| status.changes.len());
        let can_commit = self
            .git
            .capabilities()
            .supports(BackendOperation::GitCommit)
            && self.can_mutate_git();
        let can_remote = self
            .git
            .capabilities()
            .supports(BackendOperation::GitStatus)
            && self.can_mutate_git();
        let has_selected_paths = self.git.state.model.selected_path_count() > 0;
        let can_revert = can_remote && has_selected_paths;

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .h(px(40.0))
                    .flex_shrink_0()
                    .border_b_1()
                    .border_color(theme::border_default())
                    .flex()
                    .children([
                        git_mode_tab(
                            "git-mode-changes",
                            locale::text("Changes", "更改", "變更"),
                            changes_active,
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.set_git_mode(GitWorkbenchMode::Changes, cx)
                            }),
                        ),
                        git_mode_tab(
                            "git-mode-history",
                            locale::text("Commits", "提交", "提交"),
                            history_active,
                        )
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(|this, _, _, cx| {
                                this.set_git_mode(GitWorkbenchMode::History, cx)
                            }),
                        ),
                    ]),
            )
            .child(
                div()
                    .h(px(40.0))
                    .flex_shrink_0()
                    .px_2()
                    .gap_1()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .child(
                        icon_button("git-fetch", "icons/download.svg", "Fetch")
                            .when(can_remote && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.run_git_remote_action(GitRemoteActionKind::Fetch, cx)
                                    }),
                                )
                            })
                            .when(!can_remote, |button| button.opacity(0.55)),
                    )
                    .child(
                        icon_button("git-push", "icons/upload.svg", "Push")
                            .when(can_remote && !self.busy, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        this.run_git_remote_action(GitRemoteActionKind::Push, cx)
                                    }),
                                )
                            })
                            .when(!can_remote, |button| button.opacity(0.55)),
                    )
                    .child(
                        icon_button("git-refresh", "icons/rotate-ccw.svg", "Refresh").when(
                            !self.busy,
                            |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::refresh_active_surface),
                                )
                            },
                        ),
                    )
                    .when(changes_active, |bar| {
                        bar.child(
                            icon_button("git-revert", "icons/undo.svg", "Rollback selected")
                                .when(!self.busy && can_revert, |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::revert_selected_git),
                                    )
                                })
                                .when(!can_revert, |button| button.opacity(0.55)),
                        )
                        .child(div().flex_1())
                        .child(
                            icon_button(
                                "git-toggle-directories",
                                "icons/chevrons-down-up.svg",
                                "Expand or collapse all",
                            )
                            .when(
                                self.git.state.model.has_change_directories(),
                                |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.git.state.model.toggle_all_change_directories();
                                            cx.notify();
                                        }),
                                    )
                                },
                            ),
                        )
                    })
                    .when(history_active, |bar| {
                        bar.child(
                            div()
                                .h(px(30.0))
                                .min_w_0()
                                .flex_1()
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(theme::border_default())
                                .bg(theme::workbench_panel_bg())
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1()
                                .child(
                                    svg()
                                        .path("icons/search.svg")
                                        .size(px(theme::ICON_SM))
                                        .text_color(theme::text_muted()),
                                )
                                .child(
                                    div()
                                        .min_w_0()
                                        .flex_1()
                                        .child(self.git_history_query_input.clone()),
                                )
                                .when(
                                    !self
                                        .git_history_query_input
                                        .read(cx)
                                        .text()
                                        .trim()
                                        .is_empty(),
                                    |bar| {
                                        bar.child(
                                            icon_button(
                                                "clear-git-history-search",
                                                "icons/x.svg",
                                                "Clear commit search",
                                            )
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::clear_git_history_search),
                                            ),
                                        )
                                    },
                                )
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::search_git_history),
                                ),
                        )
                        .child(
                            compact_action(
                                "git-history-author",
                                self.git
                                    .state
                                    .model
                                    .history_filter
                                    .author
                                    .as_deref()
                                    .unwrap_or("All authors")
                                    .to_string(),
                            )
                            .h(px(30.0))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::toggle_git_history_author),
                            ),
                        )
                    }),
            )
            .when(changes_active, |root| {
                root.child(
                    div()
                        .h(px(44.0))
                        .flex_shrink_0()
                        .px_2()
                        .border_b_1()
                        .border_color(theme::border_subtle())
                        .flex()
                        .items_center()
                        .gap_2()
                        .child(
                            icon_button("select-all-git", "icons/list-checks.svg", "Select all")
                                .on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(|this, _, _, cx| {
                                        let selected =
                                            this.git.state.model.path_selection_state("")
                                                != GitPathSelectionState::Checked;
                                        this.git.state.model.select_path_prefix("", selected);
                                        cx.notify();
                                    }),
                                ),
                        )
                        .child(git_selection_indicator_mobile(
                            self.git.state.model.path_selection_state(""),
                        ))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .whitespace_nowrap()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text_secondary())
                                .child(
                                    status
                                        .as_ref()
                                        .and_then(|status| status.branch.clone())
                                        .unwrap_or_else(|| "No repository".to_string()),
                                ),
                        )
                        .child(
                            div()
                                .font_family("monospace")
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(theme::text_muted())
                                .child(format!("{} files", change_count)),
                        )
                        .child(
                            div()
                                .font_family("monospace")
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(if additions > 0 {
                                    rgb(theme::ACCENT_GREEN).into()
                                } else {
                                    theme::text_muted()
                                })
                                .child(format!("+{additions}")),
                        )
                        .child(
                            div()
                                .font_family("monospace")
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(if deletions > 0 {
                                    rgb(theme::ACCENT_RED).into()
                                } else {
                                    theme::text_muted()
                                })
                                .child(format!("-{deletions}")),
                        ),
                )
            })
            .child(
                div()
                    .id("mobile-git-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(changes_active, |body| {
                        let row_count = self.git.state.model.change_tree_row_count();
                        body.when(row_count == 0, |body| {
                            body.child(empty_label(if status_loading {
                                "Loading repository status..."
                            } else {
                                "Working tree is clean"
                            }))
                        })
                        .children((0..row_count).filter_map(|index| {
                            self.git
                                .state
                                .model
                                .change_tree_row(index)
                                .map(|(row, change)| {
                                    self.render_git_tree_row_mobile(
                                        row.clone(),
                                        change.cloned(),
                                        cx,
                                    )
                                })
                        }))
                        .when_some(self.git_diff.clone(), |body, diff| {
                            body.child(self.render_git_diff(diff))
                        })
                    })
                    .when(history_active, |body| {
                        self.render_git_history_body(body, cx)
                    }),
            )
            .when(changes_active, |root| {
                root.child(self.render_git_commit_panel(can_commit, has_selected_paths, cx))
            })
            .into_any_element()
    }

    fn render_git_tree_row_mobile(
        &self,
        row: GitTreeRow,
        change: Option<GitChange>,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let is_directory = row.kind == GitTreeRowKind::Directory;
        let path = row.path.clone();
        let path_chain = row
            .segments
            .iter()
            .map(|segment| segment.path.clone())
            .collect::<Vec<_>>();
        let label = row
            .segments
            .iter()
            .map(|segment| segment.name.as_str())
            .collect::<Vec<_>>()
            .join(" / ");
        let selection = self.git.state.model.path_selection_state(&path);
        let selected_path = change.as_ref().is_some_and(|change| {
            self.git
                .state
                .model
                .selected_change_paths()
                .contains(&change.path)
        });
        let row_id = row.id.clone();
        let change_staged = change
            .as_ref()
            .is_some_and(|change| change.staged && !change.unstaged);
        let text_color = change
            .as_ref()
            .map(git_change_text_color_mobile)
            .unwrap_or_else(theme::text_primary);
        let file_name = row
            .segments
            .last()
            .map(|segment| segment.name.as_str())
            .unwrap_or(row.path.as_str())
            .to_string();
        let icon = if is_directory {
            file_icon_descriptor("", FileEntryKind::Directory)
        } else {
            file_icon_descriptor(&file_name, FileEntryKind::File)
        };
        div()
            .id(format!("git-tree:{row_id}"))
            .relative()
            .min_h(px(32.0))
            .px_2()
            .flex()
            .items_center()
            .gap_1()
            .when(selected_path, |item| item.bg(theme::sidebar_selected_bg()))
            .when(!selected_path, |item| {
                item.hover(|style| style.bg(theme::row_pressed_bg()))
            })
            .children(file_tree_guides_mobile(row.depth))
            .child(div().w(px(row.depth as f32 * 20.0)).flex_none())
            .child({
                let select_path = path.clone();
                div()
                    .id(format!("git-select:{row_id}"))
                    .size(px(24.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            let selected = selection != GitPathSelectionState::Checked;
                            if is_directory {
                                this.git
                                    .state
                                    .model
                                    .select_path_prefix(&select_path, selected);
                            } else {
                                this.git.state.model.select_path(&select_path, selected);
                            }
                            cx.stop_propagation();
                            cx.notify();
                        }),
                    )
                    .child(git_selection_indicator_mobile(selection))
            })
            .child(
                div()
                    .w(px(14.0))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(if is_directory {
                        svg()
                            .path(if row.expanded {
                                "icons/chevron-down.svg"
                            } else {
                                "icons/chevron-right.svg"
                            })
                            .size(px(12.0))
                            .text_color(theme::text_muted())
                    } else {
                        svg()
                            .path("icons/chevron-right.svg")
                            .size(px(12.0))
                            .text_color(theme::workbench_bg())
                    }),
            )
            .child(mobile_file_tree_icon(icon, false, row.expanded))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_BODY))
                    .text_color(text_color)
                    .when(
                        change
                            .as_ref()
                            .is_some_and(|change| change.kind == GitChangeKind::Deleted),
                        |item| item.line_through(),
                    )
                    .child(if label.is_empty() {
                        path.clone()
                    } else {
                        label
                    }),
            )
            .when_some(change.clone(), |item, change| {
                item.child(
                    div()
                        .h(px(18.0))
                        .min_w(px(22.0))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(3.0))
                        .border_1()
                        .border_color(theme::border_default())
                        .px_1()
                        .flex_none()
                        .font_family("monospace")
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(git_change_text_color_mobile(&change))
                        .child(git_change_label_mobile(change.kind)),
                )
                .child(
                    div()
                        .w(px(30.0))
                        .flex_none()
                        .font_family("monospace")
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(if change.additions > 0 {
                            rgb(theme::ACCENT_GREEN).into()
                        } else {
                            theme::text_muted()
                        })
                        .child(format!("+{}", change.additions)),
                )
                .child(
                    div()
                        .w(px(30.0))
                        .flex_none()
                        .font_family("monospace")
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(if change.deletions > 0 {
                            rgb(theme::ACCENT_RED).into()
                        } else {
                            theme::text_muted()
                        })
                        .child(format!("-{}", change.deletions)),
                )
            })
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    if is_directory {
                        this.git.state.model.toggle_change_directories(&path_chain);
                    } else if change.is_some() {
                        this.open_git_diff(path.clone(), change_staged, cx);
                    }
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn render_git_diff(&self, diff: GitDiffResponse) -> gpui::AnyElement {
        div()
            .px_3()
            .py_2()
            .border_t_1()
            .border_color(theme::border_subtle())
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_secondary())
                    .child(diff.path),
            )
            .children(diff.diff.lines().take(500).map(|line| {
                let line = line.to_string();
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    rgb(theme::ACCENT_GREEN).into()
                } else if line.starts_with('-') && !line.starts_with("---") {
                    rgb(theme::ACCENT_RED).into()
                } else {
                    theme::text_muted()
                };
                div()
                    .font_family("IBM Plex Mono")
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(color)
                    .whitespace_normal()
                    .child(if line.is_empty() {
                        " ".to_string()
                    } else {
                        line
                    })
            }))
            .into_any_element()
    }

    fn render_git_commit_panel(
        &self,
        can_commit: bool,
        has_selected_paths: bool,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        div()
            .flex_shrink_0()
            .border_t_1()
            .border_color(theme::border_default())
            .bg(theme::workbench_panel_bg())
            .p_2()
            .flex()
            .flex_col()
            .gap_1()
            .child(input_shell(self.git_commit_input.clone()))
            .when(self.git_commit_confirmation, |panel| {
                panel.child(
                    div()
                        .p_2()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(rgb(theme::ACCENT_YELLOW))
                        .flex()
                        .items_center()
                        .gap_2()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_secondary())
                        .child(locale::text(
                            "Commit staged changes?",
                            "提交已暂存的更改？",
                            "提交已暫存的變更？",
                        ))
                        .child(div().flex_1())
                        .child(
                            compact_action("cancel-commit", locale::common("Cancel")).on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::cancel_git_commit),
                            ),
                        )
                        .child(
                            compact_action("confirm-commit", locale::common("Commit"))
                                .when(can_commit, |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::confirm_git_commit),
                                    )
                                })
                                .when(!can_commit, |button| button.opacity(0.55)),
                        ),
                )
            })
            .when(!self.git_commit_confirmation, |panel| {
                panel.child(
                    div().flex().justify_end().child(
                        compact_action(
                            "request-commit",
                            if !can_commit {
                                locale::common("Read only")
                            } else if !has_selected_paths {
                                locale::common("Select changes")
                            } else if self.busy {
                                locale::common("Working...")
                            } else {
                                locale::common("Commit")
                            },
                        )
                        .when(!self.busy && can_commit && has_selected_paths, |button| {
                            button.on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::request_git_commit),
                            )
                        })
                        .when(!can_commit || !has_selected_paths, |button| {
                            button.opacity(0.55)
                        }),
                    ),
                )
            })
            .into_any_element()
    }

    fn render_git_history_body(
        &self,
        body: gpui::Stateful<gpui::Div>,
        cx: &mut Context<Self>,
    ) -> gpui::Stateful<gpui::Div> {
        let count = self.git.state.model.history_row_count();
        body.when(count == 0, |body| {
            body.child(empty_label(if self.git_history_loading {
                "Loading history"
            } else {
                "No commits found"
            }))
        })
        .children((0..count).filter_map(|index| {
            self.git
                .state
                .model
                .history_row(index)
                .cloned()
                .map(|commit| self.render_git_history_row(commit, cx))
        }))
        .when(self.git.state.model.history_has_more, |body| {
            body.child(
                compact_action(
                    "load-more-git-history",
                    if self.git_history_loading {
                        locale::common("Loading...")
                    } else {
                        locale::common("Load more")
                    },
                )
                .mx_3()
                .my_2()
                .on_mouse_up(MouseButton::Left, cx.listener(Self::load_more_git_history)),
            )
        })
    }

    fn render_git_history_row(
        &self,
        commit: GitCommitSummary,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let hash = commit.hash.clone();
        let selected = self.git.state.model.selected_commit_hash.as_deref() == Some(hash.as_str());
        div()
            .id(format!("git-history:{}", commit.hash))
            .min_h(px(58.0))
            .px_2()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .cursor_pointer()
            .when(selected, |item| item.bg(theme::sidebar_selected_bg()))
            .when(!selected, |item| {
                item.hover(|style| style.bg(theme::row_pressed_bg()))
            })
            .child(
                div()
                    .w(px(18.0))
                    .h(px(42.0))
                    .flex_none()
                    .relative()
                    .flex()
                    .items_center()
                    .justify_center()
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .bottom_0()
                            .w(px(1.0))
                            .bg(theme::border_default()),
                    )
                    .child(
                        div()
                            .relative()
                            .size(px(if selected { 9.0 } else { 7.0 }))
                            .rounded_full()
                            .bg(rgb(theme::ACCENT_BLUE)),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .flex_col()
                    .gap_1()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_1()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_primary())
                                    .child(commit.subject.clone()),
                            )
                            .children(commit.refs.iter().take(2).cloned().map(|reference| {
                                div()
                                    .max_w(px(88.0))
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .rounded(px(3.0))
                                    .bg(rgb(theme::ACCENT_BLUE).opacity(0.18))
                                    .px_1()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(rgb(theme::ACCENT_BLUE))
                                    .child(reference)
                            })),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(history_relative_time(commit.authored_at_ms))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .child(commit.author_name),
                            )
                            .child(div().font_family("monospace").child(commit.short_hash)),
                    ),
            )
            .on_mouse_up(
                MouseButton::Left,
                cx.listener(move |this, _, _, cx| {
                    this.git.state.model.select_commit(hash.clone());
                    cx.notify();
                }),
            )
            .into_any_element()
    }

    fn render_terminal(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.terminal.state.view(ShellKind::Compact);
        let sessions = self.terminal.state.sessions.clone();
        let active = self.terminal.state.active_session.clone();
        let can_create = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalCreate);
        let can_input = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalInput);
        let can_resize = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalResize);
        let can_close = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalClose);
        let output = self
            .terminal_snapshot
            .as_ref()
            .map(terminal_output)
            .unwrap_or_default();

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .gap_2()
                    .child(
                        action_button("create-terminal", "New")
                            .when(can_create, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::create_terminal),
                                )
                            })
                            .when(!can_create, |button| button.opacity(0.55)),
                    )
                    .when(active.is_some(), |bar| {
                        bar.child(
                            action_button("refresh-terminal-output", "Refresh").on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::refresh_terminal_snapshot),
                            ),
                        )
                        .child(
                            action_button("close-terminal", "Close")
                                .when(can_close, |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::request_close_terminal),
                                    )
                                })
                                .when(!can_close, |button| button.opacity(0.55)),
                        )
                    }),
            )
            .when_some(self.terminal_close_confirmation.clone(), |terminal, _| {
                terminal.child(
                    div()
                        .flex_shrink_0()
                        .mx_3()
                        .my_2()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(rgb(theme::ACCENT_YELLOW))
                        .p_3()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(theme::text_secondary())
                        .child(locale::text(
                            "Close this terminal and stop its running process?",
                            "关闭此终端并停止正在运行的进程？",
                            "關閉此終端機並停止正在執行的程序？",
                        ))
                        .child(
                            div()
                                .flex()
                                .justify_end()
                                .gap_2()
                                .child(
                                    action_button("cancel-terminal-close", "Cancel").on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::cancel_close_terminal),
                                    ),
                                )
                                .child(
                                    action_button("confirm-terminal-close", "Close terminal")
                                        .when(can_close, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::confirm_close_terminal),
                                            )
                                        })
                                        .when(!can_close, |button| button.opacity(0.55)),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .id("mobile-terminal-list-scroll")
                    .flex_shrink_0()
                    .max_h(px(132.0))
                    .overflow_y_scroll()
                    .children(sessions.into_iter().map(|session| {
                        let terminal_id = session.id.clone();
                        let selected = active
                            .as_ref()
                            .is_some_and(|active| active.id == session.id);
                        div()
                            .id(format!("terminal:{}", session.id))
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .when(selected, |row| row.bg(theme::bg_card()))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.attach_terminal(terminal_id.clone(), cx)
                                }),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .child(session.title),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{:?}  {}x{}",
                                        session.status, session.cols, session.rows
                                    )),
                            )
                    })),
            )
            .when_some(active.clone(), |terminal, session| {
                terminal.child(
                    div()
                        .flex_shrink_0()
                        .min_h(px(theme::TOUCH_TARGET))
                        .px_3()
                        .border_t_1()
                        .border_color(theme::border_subtle())
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap_2()
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text_muted())
                                .child(format!("{} rows", session.rows)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    compact_action("terminal-rows-down", "-")
                                        .when(can_resize, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::resize_terminal_rows_down),
                                            )
                                        })
                                        .when(!can_resize, |button| button.opacity(0.55)),
                                )
                                .child(
                                    compact_action("terminal-rows-up", "+")
                                        .when(can_resize, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::resize_terminal_rows_up),
                                            )
                                        })
                                        .when(!can_resize, |button| button.opacity(0.55)),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(theme::FONT_CAPTION))
                                .text_color(theme::text_muted())
                                .child(format!("{} cols", session.cols)),
                        )
                        .child(
                            div()
                                .flex()
                                .gap_1()
                                .child(
                                    compact_action("terminal-cols-down", "-")
                                        .when(can_resize, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::resize_terminal_cols_down),
                                            )
                                        })
                                        .when(!can_resize, |button| button.opacity(0.55)),
                                )
                                .child(
                                    compact_action("terminal-cols-up", "+")
                                        .when(can_resize, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::resize_terminal_cols_up),
                                            )
                                        })
                                        .when(!can_resize, |button| button.opacity(0.55)),
                                ),
                        ),
                )
            })
            .child(
                div()
                    .id("mobile-terminal-output-scroll")
                    .flex_1()
                    .min_h_0()
                    .bg(rgb(0x080808))
                    .overflow_y_scroll()
                    .p_3()
                    .font_family("IBM Plex Mono")
                    .children(output.lines().map(|line| {
                        div()
                            .min_h(px(16.0))
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_secondary())
                            .whitespace_nowrap()
                            .child(if line.is_empty() {
                                " ".to_string()
                            } else {
                                line.to_string()
                            })
                    }))
                    .when(output.is_empty(), |terminal| {
                        terminal.child(empty_label(if active.is_some() {
                            "No output yet"
                        } else {
                            "Select or create a terminal"
                        }))
                    }),
            )
            .when(active.is_some(), |terminal| {
                terminal
                    .child(
                        div()
                            .id("mobile-terminal-keys-scroll")
                            .flex_shrink_0()
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_2()
                            .overflow_x_scroll()
                            .flex()
                            .items_center()
                            .gap_1()
                            .children(view.key_bar.into_iter().map(|action| {
                                let key = action.key;
                                compact_action(
                                    format!("terminal-key:{:?}", action.key),
                                    action.label,
                                )
                                .when(can_input, |button| {
                                    button.on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.send_terminal_key(key, cx)
                                        }),
                                    )
                                })
                                .when(!can_input, |button| button.opacity(0.55))
                            })),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .p_2()
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .gap_2()
                            .child(input_shell(self.terminal_input.clone()))
                            .child(
                                action_button("send-terminal-input", "Send")
                                    .when(can_input, |button| {
                                        button.on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::send_terminal_input),
                                        )
                                    })
                                    .when(!can_input, |button| button.opacity(0.55)),
                            ),
                    )
            })
            .into_any_element()
    }

    fn render_providers(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.management.state.view(ShellKind::Compact);
        let can_check_health = self
            .management
            .capabilities
            .supports(BackendOperation::ManagementHealth)
            && self
                .backend
                .permits_remote_action(vibex_core::RemoteActionClass::MutateProviderSettings);
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .justify_between()
                    .items_center()
                    .child(
                        div()
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(theme::text_muted())
                            .child(format!("{:?}", view.load_state)),
                    )
                    .child(
                        action_button(
                            "provider-health-probes",
                            if !can_check_health {
                                "Read only"
                            } else if self.busy {
                                "Checking..."
                            } else {
                                "Check health"
                            },
                        )
                        .when(!self.busy && can_check_health, |button| {
                            button.on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::run_health_probes),
                            )
                        })
                        .when(!can_check_health, |button| button.opacity(0.55)),
                    ),
            )
            .child(
                div()
                    .id("mobile-providers-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(section_heading("Agents"))
                    .when(self.agent_summaries.is_empty(), |body| {
                        body.child(empty_label("No Agent summaries published"))
                    })
                    .children(self.agent_summaries.iter().cloned().map(|agent| {
                        div()
                            .px_3()
                            .py_3()
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .items_center()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_primary())
                                            .child(agent.label),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(format!(
                                                "{} models  {:?}",
                                                agent.model_count, agent.config_status
                                            )),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(if agent.enabled {
                                        agent_runtime_status_color(agent.runtime_status)
                                    } else {
                                        theme::text_muted()
                                    })
                                    .child(if agent.enabled {
                                        format!("{:?}", agent.runtime_status)
                                    } else {
                                        "Disabled".to_string()
                                    }),
                            )
                    }))
                    .child(section_heading("Provider profiles"))
                    .when(view.profiles.is_empty(), |body| {
                        body.child(empty_label("No provider profiles published"))
                    })
                    .children(view.profiles.into_iter().map(|profile| {
                        div()
                            .px_3()
                            .py_3()
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .flex_col()
                            .gap_1()
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .gap_2()
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_primary())
                                            .child(profile.display_name),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(theme::text_muted())
                                            .child(format!("{:?}", profile.status)),
                                    ),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_muted())
                                    .child(format!(
                                        "{:?}  {} models  secret {:?}",
                                        profile.kind,
                                        profile.configured_model_count,
                                        profile.secret_setup_state
                                    )),
                            )
                    }))
                    .child(section_heading("Health"))
                    .when(view.health.is_empty(), |body| {
                        body.child(empty_label("No health results yet"))
                    })
                    .children(view.health.into_iter().map(|health| {
                        div()
                            .min_h(px(theme::TOUCH_TARGET))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_between()
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .child(
                                div()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .child(health.display_name),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(provider_health_color(health.status))
                                    .child(format!("{:?}", health.status)),
                            )
                    }))
                    .child(section_heading("Runtime probes"))
                    .when(view.runtime_probes.is_empty(), |body| {
                        body.child(empty_label("No runtime probes recorded"))
                    })
                    .children(view.runtime_probes.into_iter().take(20).map(|probe| {
                        div()
                            .px_3()
                            .py_2()
                            .border_b_1()
                            .border_color(theme::border_subtle())
                            .flex()
                            .justify_between()
                            .gap_2()
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_secondary())
                                    .child(format!("{} / {}", probe.agent_id, probe.adapter_id)),
                            )
                            .child(
                                div()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child(format!("{:?}", probe.status)),
                            )
                    })),
            )
            .into_any_element()
    }

    fn render_runtime(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let catalog = self.runtime_catalog.clone();
        let draft = self.runtime_draft.clone();
        let can_switch_runtime = self
            .backend
            .capability_snapshot()
            .agent
            .supports(BackendOperation::AgentSwitchRuntime);
        let selected_option = catalog.as_ref().and_then(|catalog| {
            draft
                .as_ref()
                .and_then(|draft| matching_runtime_option(&catalog.options, draft))
                .cloned()
        });
        let selected_reasoning = draft
            .as_ref()
            .and_then(|draft| draft.reasoning_effort.clone());
        let selected_mode = draft.as_ref().and_then(|draft| draft.mode_id.clone());
        let can_apply = can_switch_runtime
            && catalog.as_ref().is_some_and(|catalog| {
                draft
                    .as_ref()
                    .is_some_and(|draft| runtime_selection_is_available(&catalog.options, draft))
            });
        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .px_3()
                    .py_2()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_muted())
                    .child(
                        self.runtime_state
                            .as_ref()
                            .map(|state| format!("{:?}", state.status))
                            .unwrap_or_else(|| {
                                locale::common("Select an Agent session first").to_string()
                            }),
                    ),
            )
            .child(
                div()
                    .id("mobile-runtime-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(section_heading("Agent / provider / model"))
                    .when(catalog.is_none(), |body| {
                        body.child(empty_label("Runtime catalog unavailable"))
                    })
                    .children(
                        catalog
                            .as_ref()
                            .into_iter()
                            .flat_map(|catalog| catalog.options.iter().cloned())
                            .map(|option| {
                                let selection = option.selection.clone();
                                let available =
                                    option.availability == RuntimeOptionAvailability::Available;
                                let selected = draft
                                    .as_ref()
                                    .is_some_and(|draft| runtime_option_matches(&option, draft));
                                div()
                                    .id(format!(
                                        "runtime-option:{}:{}:{}",
                                        option.selection.agent_id,
                                        option.auth_source_label,
                                        option.model_label
                                    ))
                                    .mx_3()
                                    .mb_2()
                                    .min_h(px(58.0))
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(if selected {
                                        rgb(theme::ACCENT_BLUE).into()
                                    } else {
                                        theme::border_default()
                                    })
                                    .px_3()
                                    .py_2()
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .when(available, |row| {
                                        row.cursor_pointer()
                                            .active(|style| style.bg(theme::row_pressed_bg()))
                                            .on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(move |this, _, _, cx| {
                                                    this.choose_runtime_option(
                                                        selection.clone(),
                                                        cx,
                                                    )
                                                }),
                                            )
                                    })
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(if selected {
                                                theme::text_primary()
                                            } else {
                                                theme::text_secondary()
                                            })
                                            .child(format!(
                                                "{} / {}",
                                                option.agent_label, option.model_label
                                            )),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(
                                                if option.availability
                                                    == RuntimeOptionAvailability::Available
                                                {
                                                    theme::text_muted()
                                                } else {
                                                    rgb(theme::ACCENT_YELLOW).into()
                                                },
                                            )
                                            .child(format!(
                                                "{}  {:?}",
                                                option.auth_source_label, option.availability
                                            )),
                                    )
                                    .into_any_element()
                            }),
                    )
                    .when_some(selected_option, |body, option| {
                        body.child(section_heading("Reasoning"))
                            .child(
                                div()
                                    .px_3()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .child(
                                        choice_button(
                                            "runtime-reasoning:default",
                                            "Default",
                                            selected_reasoning.is_none(),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.choose_runtime_reasoning(None, cx)
                                            }),
                                        ),
                                    )
                                    .children(option.reasoning_efforts.into_iter().map(|value| {
                                        let id = value.value.clone();
                                        let label = value.label.unwrap_or_else(|| id.clone());
                                        choice_button(
                                            format!("runtime-reasoning:{id}"),
                                            label,
                                            selected_reasoning.as_deref() == Some(id.as_str()),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.choose_runtime_reasoning(Some(id.clone()), cx)
                                            }),
                                        )
                                    })),
                            )
                            .child(section_heading("Mode"))
                            .child(
                                div()
                                    .px_3()
                                    .flex()
                                    .flex_wrap()
                                    .gap_2()
                                    .child(
                                        choice_button(
                                            "runtime-mode:default",
                                            "Default",
                                            selected_mode.is_none(),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(|this, _, _, cx| {
                                                this.choose_runtime_mode(None, cx)
                                            }),
                                        ),
                                    )
                                    .children(option.modes.into_iter().map(|value| {
                                        let id = value.value.clone();
                                        let label = value.label.unwrap_or_else(|| id.clone());
                                        choice_button(
                                            format!("runtime-mode:{id}"),
                                            label,
                                            selected_mode.as_deref() == Some(id.as_str()),
                                        )
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.choose_runtime_mode(Some(id.clone()), cx)
                                            }),
                                        )
                                    })),
                            )
                            .when(!option.features.is_empty(), |body| {
                                body.child(section_heading("Session options")).children(
                                    option
                                        .features
                                        .into_iter()
                                        .map(|feature| self.render_runtime_feature(feature, cx)),
                                )
                            })
                    }),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .p_3()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .justify_end()
                    .child(
                        action_button(
                            "apply-runtime-selection",
                            if !can_switch_runtime {
                                "Read only"
                            } else if self.busy {
                                "Applying..."
                            } else {
                                "Apply runtime"
                            },
                        )
                        .when(!self.busy && can_apply, |button| {
                            button.on_mouse_up(MouseButton::Left, cx.listener(Self::apply_runtime))
                        })
                        .when(!can_switch_runtime, |button| button.opacity(0.55)),
                    ),
            )
            .into_any_element()
    }

    fn render_runtime_feature(
        &self,
        feature: SessionRuntimeFeature,
        cx: &mut Context<Self>,
    ) -> gpui::AnyElement {
        let feature_id = feature.id.clone();
        let selected_value = self
            .runtime_draft
            .as_ref()
            .and_then(|draft| draft.config_values.get(&feature_id))
            .cloned();
        let values = match feature.kind {
            SessionRuntimeFeatureKind::Toggle => vec![
                (locale::common("Default").to_string(), None),
                (locale::common("On").to_string(), Some("true".to_string())),
                (locale::common("Off").to_string(), Some("false".to_string())),
            ],
            SessionRuntimeFeatureKind::Select => {
                std::iter::once((locale::common("Default").to_string(), None))
                    .chain(feature.values.iter().map(|value| {
                        (
                            value.label.clone().unwrap_or_else(|| value.value.clone()),
                            Some(value.value.clone()),
                        )
                    }))
                    .collect()
            }
            SessionRuntimeFeatureKind::String => Vec::new(),
        };
        let row = div().px_3().pb_3().flex().flex_col().gap_2().child(
            div()
                .text_size(px(theme::FONT_CAPTION))
                .text_color(theme::text_secondary())
                .child(feature.label.clone()),
        );
        if feature.kind == SessionRuntimeFeatureKind::String {
            return match self.runtime_feature_inputs.get(&feature_id) {
                Some(input) => row.child(input_shell(input.clone())).into_any_element(),
                None => row
                    .child(
                        div()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(locale::common("Loading value...")),
                    )
                    .into_any_element(),
            };
        }
        if values.is_empty() {
            return row
                .child(
                    div()
                        .text_size(px(theme::FONT_MICRO))
                        .text_color(theme::text_muted())
                        .child(
                            feature
                                .current_value
                                .or(feature.default_value)
                                .map(|value| value.value)
                                .unwrap_or_else(|| {
                                    locale::common("Configured by Agent").to_string()
                                }),
                        ),
                )
                .into_any_element();
        }

        row.child(
            div()
                .flex()
                .flex_wrap()
                .gap_2()
                .children(values.into_iter().map(|(label, value)| {
                    let id = feature_id.clone();
                    let value_id = value.as_deref().unwrap_or("default").to_string();
                    choice_button(
                        format!("runtime-feature:{id}:{value_id}"),
                        label,
                        selected_value.as_deref() == value.as_deref(),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.choose_runtime_feature(id.clone(), value.clone(), cx)
                        }),
                    )
                })),
        )
        .into_any_element()
    }
}

impl Render for MobileWorkbench {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let surface = self.surface;
        div()
            .size_full()
            .bg(theme::sidebar_bg())
            .flex()
            .flex_col()
            .child(
                div()
                    .id("mobile-workbench-tabs-scroll")
                    .flex_shrink_0()
                    .w_full()
                    .min_w_0()
                    .h(px(theme::TOUCH_TARGET))
                    .border_b_1()
                    .border_color(theme::border_default())
                    .flex()
                    .items_center()
                    .children(WorkbenchSurface::ALL.into_iter().map(|candidate| {
                        div()
                            .id(format!("workbench-tab:{}", candidate.label()))
                            .h_full()
                            .min_w_0()
                            .flex_1()
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_b_1()
                            .border_color(if candidate == surface {
                                rgb(theme::ACCENT_BLUE).into()
                            } else {
                                theme::sidebar_bg()
                            })
                            .text_size(px(theme::FONT_CAPTION))
                            .text_color(if candidate == surface {
                                theme::text_primary()
                            } else {
                                theme::text_muted()
                            })
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| this.set_surface(candidate, cx)),
                            )
                            .child(candidate.localized_label())
                    })),
            )
            .when_some(self.notice.clone(), |root, notice| {
                root.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .bg(theme::bg_card_dim())
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(rgb(theme::ACCENT_GREEN))
                        .child(notice),
                )
            })
            .when_some(self.error.clone(), |root, error| {
                root.child(
                    div()
                        .flex_shrink_0()
                        .px_3()
                        .py_2()
                        .bg(theme::bg_card_dim())
                        .text_size(px(theme::FONT_CAPTION))
                        .text_color(rgb(theme::ACCENT_RED))
                        .child(error.message),
                )
            })
            .child(div().flex_1().min_h_0().child(match surface {
                WorkbenchSurface::Files => self.render_files(cx),
                WorkbenchSurface::Git => self.render_git(cx),
                WorkbenchSurface::Terminal => self.render_terminal(cx),
                WorkbenchSurface::Providers => self.render_providers(cx),
                WorkbenchSurface::Runtime => self.render_runtime(cx),
            }))
            .child(
                div()
                    .absolute()
                    .right(px(theme::SPACING_SM))
                    .bottom(px(theme::SPACING_SM))
                    .size(px(theme::TOUCH_TARGET))
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_card())
                    .flex()
                    .items_center()
                    .justify_center()
                    .cursor_pointer()
                    .on_mouse_up(MouseButton::Left, cx.listener(Self::refresh_active_surface))
                    .child(
                        svg()
                            .path("icons/refresh.svg")
                            .size(px(theme::ICON_SM))
                            .text_color(theme::text_secondary()),
                    ),
            )
    }
}

fn input_shell(input: Entity<TextInput>) -> gpui::Div {
    div()
        .h(px(theme::TOUCH_TARGET))
        .flex_1()
        .min_w_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default())
        .bg(theme::bg_card())
        .px_1()
        .child(input)
}

fn icon_button(
    id: impl Into<gpui::ElementId>,
    path: &'static str,
    label: &'static str,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .size(px(30.0))
        .rounded(px(theme::RADIUS_CONTROL))
        .flex()
        .items_center()
        .justify_center()
        .aria_label(locale::common(label))
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(
            svg()
                .path(path)
                .size(px(theme::ICON_SM))
                .text_color(theme::text_secondary()),
        )
}

fn git_mode_tab(
    id: &'static str,
    label: &'static str,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h_full()
        .min_w_0()
        .flex_1()
        .px_3()
        .flex()
        .items_center()
        .justify_center()
        .border_b_2()
        .border_color(if selected {
            rgb(theme::TEXT_PRIMARY).into()
        } else {
            theme::workbench_bg()
        })
        .text_size(px(theme::FONT_CAPTION))
        .text_color(if selected {
            theme::text_primary()
        } else {
            theme::text_muted()
        })
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(label)
}

fn file_tree_guides_mobile(depth: usize) -> Vec<gpui::AnyElement> {
    (0..depth)
        .map(|index| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(index as f32 * 20.0 + 16.0))
                .w(px(1.0))
                .bg(theme::border_default())
                .into_any_element()
        })
        .collect()
}

fn mobile_file_tree_icon(
    icon: FileIconDescriptor,
    ignored: bool,
    expanded: bool,
) -> gpui::AnyElement {
    let path = match icon.kind {
        FileIconKind::Directory => {
            return svg()
                .path(if expanded {
                    "icons/folder-open.svg"
                } else {
                    "icons/folder.svg"
                })
                .size(px(theme::ICON_SM))
                .text_color(mobile_file_icon_color(icon.kind, ignored))
                .into_any_element();
        }
        FileIconKind::Code
        | FileIconKind::Rust
        | FileIconKind::TypeScript
        | FileIconKind::JavaScript => "icons/file-code.svg",
        FileIconKind::Java => "icons/coffee.svg",
        FileIconKind::Json => "icons/file-braces.svg",
        FileIconKind::Archive => "icons/file-archive.svg",
        FileIconKind::Spreadsheet => "icons/file-spreadsheet.svg",
        FileIconKind::Audio => "icons/audio-lines.svg",
        FileIconKind::Video => "icons/file-video-camera.svg",
        FileIconKind::Symlink => "icons/file-symlink.svg",
        FileIconKind::Config => "icons/file-cog.svg",
        FileIconKind::Lock => "icons/file-lock.svg",
        FileIconKind::Secret => "icons/file-key.svg",
        FileIconKind::Font => "icons/file-type.svg",
        FileIconKind::Markdown => "icons/book-open-text.svg",
        FileIconKind::Image => "icons/image.svg",
        FileIconKind::Svg | FileIconKind::Markup => "icons/code-xml.svg",
        FileIconKind::Database => "icons/database.svg",
        FileIconKind::Style => "icons/hash.svg",
        FileIconKind::Script => "icons/file-terminal.svg",
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => "icons/file-text.svg",
    };
    svg()
        .path(path)
        .size(px(theme::ICON_SM))
        .text_color(mobile_file_icon_color(icon.kind, ignored))
        .into_any_element()
}

fn mobile_file_icon_color(kind: FileIconKind, ignored: bool) -> gpui::Hsla {
    let color = match kind {
        FileIconKind::Directory => rgb(0x85899d).into(),
        FileIconKind::Code
        | FileIconKind::Java
        | FileIconKind::Rust
        | FileIconKind::TypeScript
        | FileIconKind::Markdown
        | FileIconKind::Image
        | FileIconKind::Svg => rgb(theme::ACCENT_BLUE).into(),
        FileIconKind::JavaScript | FileIconKind::Script => rgb(theme::ACCENT_YELLOW).into(),
        FileIconKind::Json => rgb(theme::ACCENT_PURPLE).into(),
        FileIconKind::Archive | FileIconKind::Config => rgb(0xf0a050).into(),
        FileIconKind::Database | FileIconKind::Spreadsheet => rgb(theme::ACCENT_GREEN).into(),
        FileIconKind::Style => rgb(0x5ed2d9).into(),
        FileIconKind::Markup => rgb(0xf0a050).into(),
        FileIconKind::Audio => rgb(0xd08ad8).into(),
        FileIconKind::Video => rgb(0xf08fc4).into(),
        FileIconKind::Symlink => rgb(0xb091f2).into(),
        FileIconKind::Lock | FileIconKind::Secret => rgb(theme::ACCENT_YELLOW).into(),
        FileIconKind::Font => rgb(0xd08ad8).into(),
        FileIconKind::Pdf
        | FileIconKind::Office
        | FileIconKind::Text
        | FileIconKind::File
        | FileIconKind::Other => theme::text_secondary(),
    };
    if ignored { color.opacity(0.35) } else { color }
}

fn file_tree_row_text_color(row: &vibex_desktop_model::FileExplorerRow) -> gpui::Hsla {
    if row.ignored {
        return theme::text_muted();
    }
    match row.git.map(|git| git.signal) {
        Some(FileGitSignal::Added) => rgb(theme::ACCENT_GREEN).into(),
        Some(FileGitSignal::Untracked) => rgb(theme::ACCENT_YELLOW).into(),
        Some(FileGitSignal::Ignored) => theme::text_muted(),
        Some(_) => rgb(theme::ACCENT_BLUE).into(),
        None => theme::text_primary(),
    }
}

fn file_git_signal_color(signal: FileGitSignal) -> gpui::Hsla {
    match signal {
        FileGitSignal::Added => rgb(theme::ACCENT_GREEN).into(),
        FileGitSignal::Untracked => rgb(theme::ACCENT_YELLOW).into(),
        FileGitSignal::Modified
        | FileGitSignal::Deleted
        | FileGitSignal::Renamed
        | FileGitSignal::Copied
        | FileGitSignal::Conflicted => rgb(theme::ACCENT_BLUE).into(),
        FileGitSignal::Ignored => theme::text_muted(),
    }
}

fn git_selection_indicator_mobile(state: GitPathSelectionState) -> gpui::AnyElement {
    let selected = state != GitPathSelectionState::Unchecked;
    div()
        .size(px(14.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(if selected {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            theme::border_default()
        })
        .bg(if selected {
            rgb(theme::ACCENT_BLUE).into()
        } else {
            theme::workbench_bg()
        })
        .text_color(theme::text_primary())
        .when(state == GitPathSelectionState::Checked, |item| {
            item.child(
                svg()
                    .path("icons/x.svg")
                    .size(px(10.0))
                    .text_color(theme::text_primary()),
            )
        })
        .when(state == GitPathSelectionState::Indeterminate, |item| {
            item.child(div().w(px(7.0)).h(px(1.0)).bg(theme::text_primary()))
        })
        .into_any_element()
}

fn file_icon_kind_for_path(path: &str) -> FileIconKind {
    file_icon_descriptor(path, FileEntryKind::File).kind
}

fn git_change_text_color_mobile(change: &GitChange) -> gpui::Hsla {
    match change.kind {
        GitChangeKind::Deleted => theme::text_muted(),
        GitChangeKind::Untracked => rgb(theme::ACCENT_YELLOW).into(),
        GitChangeKind::Added if !change.staged => rgb(theme::ACCENT_YELLOW).into(),
        GitChangeKind::Added => rgb(theme::ACCENT_GREEN).into(),
        GitChangeKind::Modified
        | GitChangeKind::Renamed
        | GitChangeKind::Copied
        | GitChangeKind::TypeChanged
        | GitChangeKind::Unmerged
        | GitChangeKind::Unknown => rgb(theme::ACCENT_BLUE).into(),
    }
}

fn git_change_label_mobile(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "A",
        GitChangeKind::Deleted => "D",
        GitChangeKind::Renamed => "R",
        GitChangeKind::Copied => "C",
        GitChangeKind::Untracked => "U",
        GitChangeKind::Unmerged => "!",
        GitChangeKind::Modified | GitChangeKind::TypeChanged | GitChangeKind::Unknown => "M",
    }
}

fn history_relative_time(authored_at_ms: Option<i64>) -> String {
    let Some(timestamp) = authored_at_ms
        .and_then(chrono::DateTime::<chrono::Utc>::from_timestamp_millis)
        .map(|timestamp| timestamp.with_timezone(&chrono::Local))
    else {
        return locale::common("Unknown").to_string();
    };
    let age = chrono::Local::now().signed_duration_since(timestamp);
    let seconds = age.num_seconds().max(0);
    if seconds < 60 {
        format!("{seconds}s ago")
    } else if seconds < 3_600 {
        format!("{}m ago", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h ago", seconds / 3_600)
    } else {
        timestamp.format("%Y-%m-%d %H:%M").to_string()
    }
}

fn action_button(id: impl Into<gpui::ElementId>, label: &'static str) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(theme::TOUCH_TARGET))
        .px_3()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default())
        .bg(theme::bg_card())
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::FONT_BODY))
        .text_color(theme::text_secondary())
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(locale::common(label))
}

fn compact_action(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
) -> gpui::Stateful<gpui::Div> {
    div()
        .id(id)
        .h(px(36.0))
        .px_2()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default())
        .flex()
        .items_center()
        .justify_center()
        .text_size(px(theme::FONT_MICRO))
        .text_color(theme::text_secondary())
        .cursor_pointer()
        .active(|style| style.bg(theme::row_pressed_bg()))
        .child(label.into())
}

fn choice_button(
    id: impl Into<gpui::ElementId>,
    label: impl Into<String>,
    selected: bool,
) -> gpui::Stateful<gpui::Div> {
    compact_action(id, label).when(selected, |button| {
        button
            .border_color(rgb(theme::ACCENT_BLUE))
            .bg(theme::bg_card())
            .text_color(theme::text_primary())
    })
}

fn section_heading(label: &'static str) -> gpui::Div {
    div()
        .h(px(34.0))
        .px_3()
        .flex()
        .items_center()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::text_muted())
        .child(locale::common(label))
}

fn empty_label(label: &'static str) -> gpui::Div {
    div()
        .px_3()
        .py_4()
        .text_size(px(theme::FONT_CAPTION))
        .text_color(theme::text_muted())
        .child(locale::common(label))
}

fn file_status_label(status: FileEditorStatus) -> &'static str {
    locale::common(match status {
        FileEditorStatus::Loading => "Loading",
        FileEditorStatus::Clean => "Clean",
        FileEditorStatus::Dirty => "Unsaved",
        FileEditorStatus::Saving => "Saving",
        FileEditorStatus::Saved => "Saved",
        FileEditorStatus::Conflict => "Conflict",
        FileEditorStatus::Disconnected => "Offline",
        FileEditorStatus::Unsupported => "Read only",
        FileEditorStatus::TooLarge => "Too large",
    })
}

fn file_status_color(status: FileEditorStatus) -> gpui::Hsla {
    match status {
        FileEditorStatus::Conflict | FileEditorStatus::Disconnected => {
            rgb(theme::ACCENT_RED).into()
        }
        FileEditorStatus::Dirty | FileEditorStatus::Saving => rgb(theme::ACCENT_YELLOW).into(),
        FileEditorStatus::Saved => rgb(theme::ACCENT_GREEN).into(),
        _ => theme::text_muted(),
    }
}

fn provider_health_color(status: vibex_core::ProviderHealthStatus) -> gpui::Hsla {
    use vibex_core::ProviderHealthStatus;
    match status {
        ProviderHealthStatus::Pass => rgb(theme::ACCENT_GREEN).into(),
        ProviderHealthStatus::Warn
        | ProviderHealthStatus::Unknown
        | ProviderHealthStatus::Skipped
        | ProviderHealthStatus::Unsupported => rgb(theme::ACCENT_YELLOW).into(),
        ProviderHealthStatus::Fail => rgb(theme::ACCENT_RED).into(),
    }
}

fn agent_runtime_status_color(status: vibex_core::AgentRuntimeStatus) -> gpui::Hsla {
    use vibex_core::AgentRuntimeStatus;
    match status {
        AgentRuntimeStatus::Ready => rgb(theme::ACCENT_GREEN).into(),
        AgentRuntimeStatus::Unknown => rgb(theme::ACCENT_YELLOW).into(),
        AgentRuntimeStatus::Unavailable
        | AgentRuntimeStatus::Disabled
        | AgentRuntimeStatus::ProbeFailed => rgb(theme::ACCENT_RED).into(),
    }
}

fn terminal_output(snapshot: &TerminalSnapshot) -> String {
    let mut output = snapshot
        .chunks
        .iter()
        .map(|chunk| chunk.data.as_str())
        .collect::<String>();
    if output.len() > TERMINAL_OUTPUT_LIMIT {
        let mut start = output.len() - TERMINAL_OUTPUT_LIMIT;
        while !output.is_char_boundary(start) {
            start += 1;
        }
        output = output[start..].to_string();
    }
    output
}

fn runtime_string_override(value: String) -> BackendResult<Option<String>> {
    if value.trim().is_empty() {
        return Ok(None);
    }
    if value.len() > RUNTIME_FEATURE_VALUE_LIMIT {
        return Err(BackendError::failed(
            "mobile_runtime_feature_value_too_long",
            "runtime option values must be at most 256 bytes",
        ));
    }
    Ok(Some(value))
}

fn runtime_option_matches(
    option: &SessionRuntimeOption,
    selection: &SessionRuntimeSelection,
) -> bool {
    option.selection.agent_id == selection.agent_id
        && option.selection.auth_source == selection.auth_source
        && option.selection.model == selection.model
}

fn matching_runtime_option<'a>(
    options: &'a [SessionRuntimeOption],
    selection: &SessionRuntimeSelection,
) -> Option<&'a SessionRuntimeOption> {
    options
        .iter()
        .find(|option| runtime_option_matches(option, selection))
}

fn runtime_selection_is_available(
    options: &[SessionRuntimeOption],
    selection: &SessionRuntimeSelection,
) -> bool {
    matching_runtime_option(options, selection)
        .is_some_and(|option| option.availability == RuntimeOptionAvailability::Available)
}

fn flatten_join<T>(outcome: Result<BackendResult<T>, gpui_tokio::JoinError>) -> BackendResult<T> {
    outcome.unwrap_or_else(|_| Err(background_task_error()))
}

fn background_task_error() -> BackendError {
    BackendError::failed(
        "mobile_workbench_task_failed",
        locale::text(
            "A mobile workspace task stopped unexpectedly.",
            "移动端工作区任务意外停止。",
            "行動端工作區工作意外停止。",
        ),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    fn runtime_option(
        selection: SessionRuntimeSelection,
        availability: RuntimeOptionAvailability,
    ) -> SessionRuntimeOption {
        SessionRuntimeOption {
            selection,
            agent_label: "Codex".to_string(),
            auth_source_label: "Profile".to_string(),
            model_label: "Model".to_string(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability,
        }
    }

    #[test]
    fn runtime_defaults_remove_only_empty_overrides_and_preserve_explicit_spacing() {
        assert_eq!(runtime_string_override("   ".to_string()).unwrap(), None);
        assert_eq!(
            runtime_string_override("  explicit value  ".to_string()).unwrap(),
            Some("  explicit value  ".to_string())
        );
        assert_eq!(
            runtime_string_override("x".repeat(RUNTIME_FEATURE_VALUE_LIMIT + 1))
                .unwrap_err()
                .code,
            "mobile_runtime_feature_value_too_long"
        );
    }

    #[test]
    fn unavailable_runtime_option_matches_for_display_but_cannot_be_applied() {
        let selection = SessionRuntimeSelection::provider(
            vibex_core::AgentId::parse("codex").unwrap(),
            vibex_core::ProviderProfileId::new(),
            "gpt-5",
        );
        let unavailable = runtime_option(
            selection.clone(),
            RuntimeOptionAvailability::RequiresConfiguration,
        );
        assert!(runtime_option_matches(&unavailable, &selection));
        assert!(!runtime_selection_is_available(
            std::slice::from_ref(&unavailable),
            &selection
        ));

        let available = runtime_option(selection.clone(), RuntimeOptionAvailability::Available);
        assert!(runtime_selection_is_available(&[available], &selection));
    }

    #[test]
    fn mobile_workbench_file_and_git_mappings_follow_desktop_labels() {
        assert_eq!(
            MobileFileSearchMode::Name.toggle(),
            MobileFileSearchMode::Content
        );
        assert_eq!(
            MobileFileSearchMode::Content.toggle(),
            MobileFileSearchMode::Name
        );
        assert_eq!(MobileFileSearchMode::Name.label(), "Name");
        assert_eq!(MobileFileSearchMode::Content.label(), "Content");
        assert_eq!(git_change_label_mobile(GitChangeKind::Added), "A");
        assert_eq!(git_change_label_mobile(GitChangeKind::Deleted), "D");
        assert_eq!(git_change_label_mobile(GitChangeKind::Untracked), "U");
        assert_eq!(git_change_label_mobile(GitChangeKind::Unmerged), "!");
        assert_eq!(file_icon_kind_for_path("src/main.rs"), FileIconKind::Rust);
        assert_eq!(file_icon_kind_for_path("README.md"), FileIconKind::Markdown);
        assert_eq!(file_icon_kind_for_path("config.toml"), FileIconKind::Config);
    }

    #[test]
    fn visible_surfaces_exclude_provider_and_runtime_entries() {
        assert_eq!(
            WorkbenchSurface::ALL,
            [
                WorkbenchSurface::Files,
                WorkbenchSurface::Git,
                WorkbenchSurface::Terminal,
            ]
        );
        assert_eq!(theme::WORKBENCH_BG, theme::SIDEBAR_BG);
    }
}

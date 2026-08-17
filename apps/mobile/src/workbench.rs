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
    AgentSessionRuntimeSelectionState, FileEntryKind, FileSearchRequest, GitChange,
    GitDiffResponse, ProviderRunHealthProbesRequest, RequestId, RuntimeOptionAvailability,
    RuntimeSelectionInteraction, SessionRuntimeFeature, SessionRuntimeFeatureKind,
    SessionRuntimeOption, SessionRuntimeOptionCatalog, SessionRuntimeSelection,
    SetDesiredAgentSessionRuntimeRequest, TerminalCreateRequest, TerminalId, TerminalSnapshot,
    VibexSessionId, WorkspaceId,
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

impl WorkbenchSurface {
    pub const ALL: [Self; 5] = [
        Self::Files,
        Self::Git,
        Self::Terminal,
        Self::Providers,
        Self::Runtime,
    ];

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
    file_editor_input: Entity<TextInput>,
    git_commit_input: Entity<TextInput>,
    terminal_input: Entity<TextInput>,
    file_editor_path: Option<String>,
    git_diff: Option<GitDiffResponse>,
    git_commit_confirmation: bool,
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
            file_editor_input: cx.new(|cx| {
                TextInput::new(locale::text("File content", "文件内容", "檔案內容"), cx).multiline()
            }),
            git_commit_input: cx.new(|cx| {
                TextInput::new(locale::text("Commit message", "提交消息", "提交訊息"), cx)
            }),
            terminal_input: cx.new(|cx| {
                TextInput::new(locale::text("Type a command", "输入命令", "輸入命令"), cx)
            }),
            file_editor_path: None,
            git_diff: None,
            git_commit_confirmation: false,
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

    fn activate_file_row(&mut self, path: String, kind: FileEntryKind, cx: &mut Context<Self>) {
        if kind == FileEntryKind::Directory {
            if self.files.state.tree.toggle_expanded(&path) {
                self.load_file_tree_path(path, cx);
            } else {
                cx.notify();
            }
        } else {
            self.select_file(path, cx);
        }
    }

    fn search_files(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
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
            include_content: true,
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
                cx.notify();
            });
        });
        self.tasks.push(task);
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
        let message = self.git_commit_input.read(cx).text().trim().to_string();
        match self.git.request_commit_confirmation(message, Vec::new()) {
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
                    .p_3()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .gap_2()
                    .child(input_shell(self.file_search_input.clone()))
                    .child(
                        action_button("file-search", "Search")
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::search_files)),
                    ),
            )
            .child(
                div()
                    .id("mobile-files-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(section_heading("Explorer"))
                    .when(rows.is_empty(), |body| {
                        body.child(empty_label("No files found"))
                    })
                    .children(rows.into_iter().map(|row| {
                        let path = row.path.clone();
                        let kind = row.kind;
                        let marker = match row.kind {
                            FileEntryKind::Directory if row.expanded => "v",
                            FileEntryKind::Directory => ">",
                            _ => "",
                        };
                        div()
                            .id(format!("file:{}", row.id))
                            .min_h(px(theme::TOUCH_TARGET))
                            .pl(px(12.0 + row.depth as f32 * 16.0))
                            .pr_3()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(row.selected, |item| item.bg(theme::bg_card()))
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(move |this, _, _, cx| {
                                    this.activate_file_row(path.clone(), kind, cx)
                                }),
                            )
                            .child(
                                div()
                                    .w(px(12.0))
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(theme::text_muted())
                                    .child(marker),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .text_color(theme::text_secondary())
                                    .child(row.name),
                            )
                            .into_any_element()
                    }))
                    .when(!search.is_empty(), |body| {
                        body.child(section_heading("Search results")).children(
                            search.into_iter().map(|result| {
                                let path = result.path.clone();
                                div()
                                    .id(format!(
                                        "file-search-result:{}:{}",
                                        result.path,
                                        result.line.unwrap_or_default()
                                    ))
                                    .min_h(px(theme::TOUCH_TARGET))
                                    .px_3()
                                    .py_2()
                                    .border_b_1()
                                    .border_color(theme::border_subtle())
                                    .flex()
                                    .flex_col()
                                    .justify_center()
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(move |this, _, _, cx| {
                                            this.select_file(path.clone(), cx)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(theme::FONT_BODY))
                                            .text_color(theme::text_secondary())
                                            .child(result.path),
                                    )
                                    .when_some(result.snippet, |row, snippet| {
                                        row.child(
                                            div()
                                                .text_size(px(theme::FONT_MICRO))
                                                .text_color(theme::text_muted())
                                                .child(snippet),
                                        )
                                    })
                                    .into_any_element()
                            }),
                        )
                    })
                    .when_some(selected_path, |body, path| {
                        body.child(section_heading("Editor")).child(
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
                    }),
            )
            .into_any_element()
    }

    fn render_git(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let status = self.git.state.model.status.clone();
        let can_commit = self
            .git
            .capabilities()
            .supports(BackendOperation::GitCommit);
        let changes = status
            .as_ref()
            .map(|status| status.changes.clone())
            .unwrap_or_default();
        let summary = status.as_ref().map(|status| {
            format!(
                "{}  {} {}  {} {}",
                status.branch.as_deref().unwrap_or("detached"),
                status.staged_count,
                locale::text("staged", "已暂存", "已暫存"),
                status.unstaged_count,
                locale::text("unstaged", "未暂存", "未暫存"),
            )
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
                    .child(summary.unwrap_or_else(|| {
                        locale::common("Loading repository status...").to_string()
                    })),
            )
            .child(
                div()
                    .id("mobile-git-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .child(section_heading("Changes"))
                    .when(changes.is_empty(), |body| {
                        body.child(empty_label("Working tree is clean"))
                    })
                    .children(
                        changes
                            .into_iter()
                            .map(|change| self.render_git_change(change, cx)),
                    )
                    .when_some(self.git_diff.clone(), |body, diff| {
                        body.child(section_heading("Diff")).child(
                            div()
                                .px_3()
                                .pb_3()
                                .flex()
                                .flex_col()
                                .gap_2()
                                .child(
                                    div()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(theme::text_secondary())
                                        .child(diff.path),
                                )
                                .children(diff.diff.lines().take(500).map(|line| {
                                    let color = if line.starts_with('+') && !line.starts_with("+++")
                                    {
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
                                            line.to_string()
                                        })
                                })),
                        )
                    })
                    .child(section_heading("Commit"))
                    .child(
                        div()
                            .px_3()
                            .pb_3()
                            .flex()
                            .flex_col()
                            .gap_2()
                            .child(input_shell(self.git_commit_input.clone()))
                            .when(self.git_commit_confirmation, |panel| {
                                panel.child(
                                    div()
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
                                            "Commit the currently staged changes?",
                                            "提交当前已暂存的更改？",
                                            "提交目前已暫存的變更？",
                                        ))
                                        .child(
                                            div()
                                                .flex()
                                                .justify_end()
                                                .gap_2()
                                                .child(
                                                    action_button("cancel-commit", "Cancel")
                                                        .on_mouse_up(
                                                            MouseButton::Left,
                                                            cx.listener(Self::cancel_git_commit),
                                                        ),
                                                )
                                                .child(
                                                    action_button("confirm-commit", "Commit")
                                                        .when(can_commit, |button| {
                                                            button.on_mouse_up(
                                                                MouseButton::Left,
                                                                cx.listener(
                                                                    Self::confirm_git_commit,
                                                                ),
                                                            )
                                                        })
                                                        .when(!can_commit, |button| {
                                                            button.opacity(0.55)
                                                        }),
                                                ),
                                        ),
                                )
                            })
                            .when(!self.git_commit_confirmation, |panel| {
                                panel.child(
                                    div().flex().justify_end().child(
                                        action_button(
                                            "request-commit",
                                            if !can_commit {
                                                "Read only"
                                            } else if self.busy {
                                                "Working..."
                                            } else {
                                                "Commit"
                                            },
                                        )
                                        .when(!self.busy && can_commit, |button| {
                                            button.on_mouse_up(
                                                MouseButton::Left,
                                                cx.listener(Self::request_git_commit),
                                            )
                                        })
                                        .when(!can_commit, |button| button.opacity(0.55)),
                                    ),
                                )
                            }),
                    ),
            )
            .into_any_element()
    }

    fn render_git_change(&self, change: GitChange, cx: &mut Context<Self>) -> gpui::AnyElement {
        let can_stage = self.git.capabilities().supports(BackendOperation::GitStage);
        let can_unstage = self
            .git
            .capabilities()
            .supports(BackendOperation::GitUnstage);
        let diff_path = change.path.clone();
        let diff_staged = change.staged && !change.unstaged;
        let stage_path = change.path.clone();
        let unstage_path = change.path.clone();
        div()
            .min_h(px(58.0))
            .px_3()
            .py_2()
            .border_b_1()
            .border_color(theme::border_subtle())
            .flex()
            .items_center()
            .gap_2()
            .child(
                div()
                    .id(format!("git-diff:{}", change.path))
                    .flex_1()
                    .min_w_0()
                    .cursor_pointer()
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.open_git_diff(diff_path.clone(), diff_staged, cx)
                        }),
                    )
                    .child(
                        div()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_size(px(theme::FONT_BODY))
                            .text_color(theme::text_secondary())
                            .child(change.path),
                    )
                    .child(
                        div()
                            .text_size(px(theme::FONT_MICRO))
                            .text_color(theme::text_muted())
                            .child(format!(
                                "{:?}  +{} -{}",
                                change.kind, change.additions, change.deletions
                            )),
                    ),
            )
            .when(change.unstaged && can_stage, |row| {
                row.child(
                    compact_action(format!("stage:{}", stage_path), locale::common("Stage"))
                        .on_mouse_up(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.mutate_git_path(stage_path.clone(), true, cx)
                            }),
                        ),
                )
            })
            .when(change.staged && can_unstage, |row| {
                row.child(
                    compact_action(
                        format!("unstage:{}", unstage_path),
                        locale::common("Unstage"),
                    )
                    .on_mouse_up(
                        MouseButton::Left,
                        cx.listener(move |this, _, _, cx| {
                            this.mutate_git_path(unstage_path.clone(), false, cx)
                        }),
                    ),
                )
            })
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
            .bg(theme::bg_primary())
            .flex()
            .flex_col()
            .child(
                div()
                    .id("mobile-workbench-tabs-scroll")
                    .flex_shrink_0()
                    .h(px(theme::TOUCH_TARGET))
                    .border_b_1()
                    .border_color(theme::border_default())
                    .overflow_x_scroll()
                    .flex()
                    .items_center()
                    .children(WorkbenchSurface::ALL.into_iter().map(|candidate| {
                        div()
                            .id(format!("workbench-tab:{}", candidate.label()))
                            .h_full()
                            .min_w(px(72.0))
                            .px_3()
                            .flex()
                            .items_center()
                            .justify_center()
                            .border_b_1()
                            .border_color(if candidate == surface {
                                rgb(theme::ACCENT_BLUE).into()
                            } else {
                                theme::bg_primary()
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
}

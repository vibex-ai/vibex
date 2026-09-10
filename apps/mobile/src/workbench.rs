use std::sync::Arc;
use std::time::Duration;

use gpui::{
    Context, Entity, FontWeight, IntoElement, MouseButton, MouseUpEvent, ParentElement as _,
    Render, ScrollDelta, ScrollWheelEvent, Styled as _, Task, WeakEntity, Window, canvas, div,
    prelude::*, px, rgb, svg,
};
use vibex_backend::{
    BackendError, BackendOperation, BackendResult, MutationRequest, TerminalBackend as _,
};
use vibex_core::{
    FileEntryKind, FileSearchRequest, GitChange, GitChangeKind, GitCommitDetail,
    GitCommitDetailRequest, GitCommitFileChange, GitCommitSummary, GitDiffResponse,
    GitHistoryRequest, GitRemoteActionKind, GitRemoteActionRequest, GitStageRequest,
    RemoteActionClass, TerminalCreateRequest, TerminalId, TerminalSnapshot, TerminalStatus,
    VibexSessionId, WorkspaceId,
};
use vibex_desktop_model::{
    FileGitSignal, FileIconDescriptor, FileIconKind, GitPathSelectionState, GitQueryKind,
    GitTreeRow, GitTreeRowKind, GitWorkbenchMode, file_icon_descriptor,
};
use vibex_remote_client::WebRemoteBackend;
use vibex_terminal_ui::{
    TerminalCellColor, TerminalCellSnapshot, TerminalCursorShape, TerminalGridPoint,
};
use vibex_ui::{
    FileEditorStatus, FileWorkflowController, GitWorkflowController, ShellKind, TerminalInput,
    TerminalKey, TerminalKeyModifiers, TerminalWorkflowCapabilities, TerminalWorkflowController,
};

use crate::input::TextInput;
use crate::locale;
use crate::theme;
use gpui_component::input::{Input, InputState};

const TERMINAL_POLL_INTERVAL: Duration = Duration::from_millis(600);
/// Cell metrics copied from the desktop terminal surface so both clients
/// render the same grid geometry at the base 13px mono font size.
const TERMINAL_FONT_SIZE: f32 = 13.0;
const TERMINAL_CELL_WIDTH: f32 = 8.0;
const TERMINAL_CELL_HEIGHT: f32 = 18.0;
const TERMINAL_HORIZONTAL_PADDING: f32 = 6.0;
const TERMINAL_VERTICAL_PADDING: f32 = 4.0;
/// Auto-fit bounds: the PTY is resized to fill the surface, clamped so extreme
/// layouts can never request a degenerate grid.
const TERMINAL_MIN_ROWS: u16 = 4;
const TERMINAL_MAX_ROWS: u16 = 100;
const TERMINAL_MIN_COLS: u16 = 20;
const TERMINAL_MAX_COLS: u16 = 240;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkbenchSurface {
    Files,
    Git,
    Terminal,
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
    file_search_input: Option<Entity<InputState>>,
    file_search_mode: MobileFileSearchMode,
    file_editor_input: Entity<TextInput>,
    /// A chosen file takes over the whole Files surface as its own screen
    /// with a back button, instead of rendering inline under the tree.
    file_screen_open: bool,
    git_commit_input: Option<Entity<InputState>>,
    git_history_query_input: Option<Entity<InputState>>,
    terminal_input: Option<Entity<InputState>>,
    // Programmatic edits need a window and the async paths below have none,
    // so a clear is recorded here and applied on the next paint.
    file_search_clear_pending: bool,
    git_commit_clear_pending: bool,
    git_history_query_clear_pending: bool,
    terminal_clear_pending: bool,
    file_editor_path: Option<String>,
    git_diff: Option<GitDiffResponse>,
    /// One commit opened from the Commits list takes over the Git surface as
    /// its own full-screen view with a back button.
    git_commit_detail: Option<GitCommitDetail>,
    git_commit_detail_loading: bool,
    git_commit_confirmation: bool,
    git_history_loading: bool,
    git_history_request_generation: u64,
    /// Next chunk sequence to feed into the render model; 0 forces a rebuild
    /// from the retained snapshot window.
    terminal_render_sequence: i64,
    terminal_close_confirmation: Option<TerminalId>,
    /// Rows/cols this client last requested, so repeated fits are no-ops.
    terminal_fit_size: Option<(u16, u16)>,
    agent_summaries: Vec<vibex_core::RemoteAgentConfigSummary>,
    terminal_poll_generation: u64,
    busy: bool,
    notice: Option<String>,
    error: Option<BackendError>,
    tasks: Vec<Task<()>>,
}

impl MobileWorkbench {
    /// The session is tracked by the app-level session-settings sheet; the
    /// workbench keeps the method for workspace lifecycle parity.
    pub fn set_session(&mut self, _session_id: Option<VibexSessionId>, _: &mut Context<Self>) {}

    pub fn new(
        backend: Arc<WebRemoteBackend>,
        workspace_id: WorkspaceId,
        _session_id: Option<VibexSessionId>,
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
        let mut workbench = Self {
            backend,
            workspace_id,
            surface: WorkbenchSurface::Files,
            files,
            git,
            terminal,
            file_search_input: None,
            file_search_mode: MobileFileSearchMode::Name,
            file_editor_input: cx.new(|cx| {
                TextInput::new(locale::text("File content", "文件内容", "檔案內容"), cx).multiline()
            }),
            file_screen_open: false,
            git_commit_input: None,
            git_history_query_input: None,
            terminal_input: None,
            file_search_clear_pending: false,
            git_commit_clear_pending: false,
            git_history_query_clear_pending: false,
            terminal_clear_pending: false,
            file_editor_path: None,
            git_diff: None,
            git_commit_detail: None,
            git_commit_detail_loading: false,
            git_commit_confirmation: false,
            git_history_loading: false,
            git_history_request_generation: 0,
            terminal_render_sequence: 0,
            terminal_close_confirmation: None,
            terminal_fit_size: None,
            agent_summaries: Vec::new(),
            terminal_poll_generation: 0,
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
        self.file_screen_open = false;
        self.git_diff = None;
        self.git_commit_detail = None;
        self.git_commit_detail_loading = false;
        self.terminal_render_sequence = 0;
        self.terminal_fit_size = None;
        self.terminal_close_confirmation = None;
        self.git_history_loading = false;
        self.git_history_request_generation = self.git_history_request_generation.wrapping_add(1);
        self.git_history_query_clear_pending = true;
        self.stop_terminal_poll();
        self.refresh_all(cx);
    }

    pub fn suspend(&mut self) {
        self.stop_terminal_poll();
    }

    pub fn resume(&mut self, cx: &mut Context<Self>) {
        self.refresh_all(cx);
    }

    fn refresh_all(&mut self, cx: &mut Context<Self>) {
        self.sync_capabilities();
        self.refresh_files(cx);
        self.refresh_git(cx);
        self.refresh_terminals(cx);
    }

    fn sync_capabilities(&mut self) {
        let capabilities = self.backend.capability_snapshot();
        self.files.set_capabilities(capabilities.file.clone());
        self.git.set_capabilities(capabilities.git.clone());
        self.terminal
            .set_capabilities(TerminalWorkflowCapabilities::from_backend(&capabilities));
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
                    this.file_screen_open = true;
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
        let query = input_value(&self.file_search_input, cx).trim().to_string();
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
        if !input_value(&self.file_search_input, cx).trim().is_empty() {
            self.start_file_search(cx);
        } else {
            cx.notify();
        }
    }

    fn clear_file_search(&mut self, _: &MouseUpEvent, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(input) = self.file_search_input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
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

    /// Returns from the full-screen file view to the file tree. The editor
    /// keeps its loaded content so re-opening the file is instant.
    fn close_file_screen(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.file_screen_open = false;
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

    /// Create the kit input states on first paint and land any clear that was
    /// requested from a path without a window.
    ///
    /// `InputState::new` and every programmatic edit take `&mut Window`, but the
    /// workbench is built and mutated from async paths that have none. Painting
    /// always has one, so the fields are born here and deferred clears apply here.
    fn ensure_inputs(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.file_search_input.is_none() {
            self.file_search_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Search files",
                    "搜索文件",
                    "搜尋檔案",
                ))
            }));
        }
        if self.git_commit_input.is_none() {
            self.git_commit_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Commit message",
                    "提交消息",
                    "提交訊息",
                ))
            }));
        }
        if self.git_history_query_input.is_none() {
            self.git_history_query_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Search commit information or code",
                    "搜索提交信息或代码",
                    "搜尋提交資訊或代碼",
                ))
            }));
        }
        if self.terminal_input.is_none() {
            self.terminal_input = Some(cx.new(|cx| {
                InputState::new(window, cx).placeholder(locale::text(
                    "Type a command",
                    "输入命令",
                    "輸入命令",
                ))
            }));
        }

        let clears = [
            (&mut self.file_search_clear_pending, &self.file_search_input),
            (&mut self.git_commit_clear_pending, &self.git_commit_input),
            (
                &mut self.git_history_query_clear_pending,
                &self.git_history_query_input,
            ),
            (&mut self.terminal_clear_pending, &self.terminal_input),
        ];
        for (pending, input) in clears {
            if !*pending {
                continue;
            }
            *pending = false;
            if let Some(input) = input.clone() {
                input.update(cx, |input, cx| input.set_value("", window, cx));
            }
        }
    }

    fn search_git_history(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let query = input_value(&self.git_history_query_input, cx)
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
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if let Some(input) = self.git_history_query_input.clone() {
            input.update(cx, |input, cx| input.set_value("", window, cx));
        }
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

    /// Opens the full-screen commit view and loads the commit detail (files
    /// and patch) from the desktop.
    fn open_git_commit(&mut self, commit_hash: String, cx: &mut Context<Self>) {
        self.git.state.model.select_commit(commit_hash.clone());
        self.git_commit_detail = None;
        self.git_commit_detail_loading = true;
        let request = GitCommitDetailRequest {
            workspace_id: self.workspace_id.clone(),
            commit_hash,
            include_patch: true,
        };
        let backend = self.backend.clone();
        let runner =
            gpui_tokio::Tokio::spawn(cx, async move { backend.git_commit_detail(request).await });
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                this.git_commit_detail_loading = false;
                match outcome {
                    Ok(detail) => this.git_commit_detail = Some(detail),
                    Err(error) => this.error = Some(error),
                }
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    /// Returns from the full-screen commit view to the Commits list.
    fn close_git_commit_screen(
        &mut self,
        _: &MouseUpEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.git_commit_detail = None;
        self.git_commit_detail_loading = false;
        cx.notify();
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
        let message = input_value(&self.git_commit_input, cx).trim().to_string();
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
                    this.git_commit_clear_pending = true;
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
                self.terminal_render_sequence = 0;
                self.terminal_fit_size = None;
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
                            this.apply_terminal_snapshot(snapshot);
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

    /// Feeds a polled snapshot into the shared render model. Chunks already
    /// applied are skipped by sequence; a sequence reset (snapshot evicted,
    /// resized, or terminal reattached) rebuilds the emulator from the
    /// retained window, mirroring the desktop raw-buffer rebuild rules.
    fn apply_terminal_snapshot(&mut self, snapshot: TerminalSnapshot) {
        let render = self
            .terminal
            .state
            .render
            .get_or_insert_with(|| vibex_ui::TerminalRenderModel::new(1, 1));
        terminal_feed_snapshot(render, &mut self.terminal_render_sequence, &snapshot);
    }

    /// Auto-fits the PTY to the available surface. The desktop grid geometry
    /// (8x18px cells at 13px) determines how many rows/columns fit, and the
    /// shared render model is resized in lock-step so the frame matches.
    fn fit_terminal_to(&mut self, width: f32, height: f32, cx: &mut Context<Self>) {
        let cols = ((((width - TERMINAL_HORIZONTAL_PADDING * 2.0) / TERMINAL_CELL_WIDTH).floor()
            as i32)
            .clamp(i32::from(TERMINAL_MIN_COLS), i32::from(TERMINAL_MAX_COLS)))
            as u16;
        let rows = ((((height - TERMINAL_VERTICAL_PADDING * 2.0) / TERMINAL_CELL_HEIGHT).floor()
            as i32)
            .clamp(i32::from(TERMINAL_MIN_ROWS), i32::from(TERMINAL_MAX_ROWS)))
            as u16;
        let Some(session) = self.terminal.state.active_session.as_ref() else {
            return;
        };
        if session.rows == rows && session.cols == cols {
            return;
        }
        if self.terminal_fit_size == Some((rows, cols)) || self.busy {
            return;
        }
        self.terminal_fit_size = Some((rows, cols));
        let terminal_id = session.id.clone();
        let operation = match self.terminal.begin_resize(rows, cols) {
            Ok(operation) => operation,
            Err(error) => {
                self.terminal_fit_size = None;
                self.error = Some(error);
                cx.notify();
                return;
            }
        };
        let runner = gpui_tokio::Tokio::spawn(cx, self.terminal.run_resize(operation.clone()));
        let task = cx.spawn(async move |entity: WeakEntity<Self>, cx| {
            let outcome = flatten_join(runner.await);
            let _ = entity.update(cx, |this, cx| {
                let current = this
                    .terminal
                    .state
                    .active_session
                    .as_ref()
                    .is_some_and(|session| session.id == terminal_id);
                if !current {
                    return;
                }
                let resized = this.terminal.apply_resize(&operation, outcome);
                if resized {
                    if let Some(session) = this.terminal.state.active_session.as_ref()
                        && let Some(render) = this.terminal.state.render.as_mut()
                    {
                        render.resize(session.rows, session.cols);
                    }
                    this.terminal_render_sequence = 0;
                    this.start_terminal_poll(terminal_id, cx);
                }
                this.error = this.terminal.state.last_error.clone();
                cx.notify();
            });
        });
        self.tasks.push(task);
        cx.notify();
    }

    fn send_terminal_input(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let value = input_value(&self.terminal_input, cx).to_string();
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

    /// Scroll-back for the terminal grid. Wheel deltas move the shared render
    /// model's viewport; a downward scroll past the history stops at the live
    /// bottom, mirroring the desktop surface.
    fn scroll_terminal(
        &mut self,
        event: &ScrollWheelEvent,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let lines = match event.delta {
            ScrollDelta::Lines(point) => point.y * 3.0,
            ScrollDelta::Pixels(point) => f32::from(point.y) / TERMINAL_CELL_HEIGHT,
        };
        if lines == 0.0 {
            return;
        }
        let Some(render) = self.terminal.state.render.as_mut() else {
            return;
        };
        let frame = &render.frame;
        if frame.modes.mouse_reporting {
            // Applications handling their own mouse events also own scroll;
            // do not fight them with local viewport movement.
            cx.stop_propagation();
            return;
        }
        if frame.display_offset == 0 && lines < 0.0 {
            return;
        }
        render.scroll(lines as i32);
        cx.stop_propagation();
        cx.notify();
    }

    fn scroll_terminal_to_bottom(&mut self, cx: &mut Context<Self>) {
        if let Some(render) = self.terminal.state.render.as_mut()
            && render.frame.display_offset != 0
        {
            render.scroll_to_bottom();
            cx.notify();
        }
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
                    this.terminal_clear_pending = true;
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
                this.terminal.state.render = None;
                this.terminal_render_sequence = 0;
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

    fn render_files(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        if self.file_screen_open {
            return self.render_file_screen(cx);
        }
        self.render_file_tree(cx)
    }

    /// Full-screen file view opened from the file panel: a header with a back
    /// button and file status, the editor filling the remaining height, and a
    /// bottom action bar (save / conflict recovery).
    fn render_file_screen(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            theme::accent_yellow()
        } else {
            file_status_color(view.status)
        };
        let has_conflict = view.status == FileEditorStatus::Conflict;
        let can_write = self
            .files
            .capabilities()
            .supports(BackendOperation::FileWrite);
        let path = view.selected_path.clone().unwrap_or_default();
        let file_name = path.rsplit('/').next().unwrap_or(&path).to_string();
        let icon = file_icon_descriptor(&file_name, FileEntryKind::File);

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::workbench_panel_bg())
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(48.0))
                    .px_1()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("file-screen-back")
                            .size(px(theme::TOUCH_TARGET))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(MouseButton::Left, cx.listener(Self::close_file_screen))
                            .child(
                                svg()
                                    .path("icons/chevron-left.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    )
                    .child(mobile_file_tree_icon(icon, false, false))
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(file_name),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child(path),
                            ),
                    )
                    .child(
                        div().flex_1().min_w_0().flex().justify_end().child(
                            div()
                                .flex_shrink_0()
                                .px_2()
                                .py(px(2.0))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .bg(theme::bg_card_dim())
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(status_color)
                                .child(status),
                        ),
                    ),
            )
            .child(
                div()
                    .id("file-screen-editor")
                    .flex_1()
                    .min_h_0()
                    .m_2()
                    .rounded(px(theme::RADIUS_CONTROL))
                    .border_1()
                    .border_color(theme::border_default())
                    .bg(theme::bg_card())
                    .overflow_hidden()
                    .child(self.file_editor_input.clone()),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .border_t_1()
                    .border_color(theme::border_subtle())
                    .p_2()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap_2()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap_2()
                            .when(has_conflict, |actions| {
                                actions.child(
                                    action_button("reload-desktop-file", "Use desktop version")
                                        .on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(Self::reload_desktop_file),
                                        ),
                                )
                            }),
                    )
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
                            button.on_mouse_up(MouseButton::Left, cx.listener(Self::save_file))
                        })
                        .when(!can_write, |button| button.opacity(0.55)),
                    ),
            )
            .into_any_element()
    }

    /// The file tree panel (search bar + rows + search results).
    fn render_file_tree(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
            theme::accent_yellow()
        } else {
            file_status_color(view.status)
        };
        let selected_path = view.selected_path.clone();
        let rows = view.rows.clone();
        let search = view.search.clone();
        let query_present = !input_value(&self.file_search_input, cx).trim().is_empty();
        let search_loading = self.files.state.search.is_loading();
        let search_has_results = !search.is_empty();

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
                                    .child(input_element(self.file_search_input.as_ref())),
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
                        // A compact summary of the chosen file; tapping it
                        // re-opens the full-screen file view.
                        body.when_some(selected_path, |body, path| {
                            body.child(
                                div()
                                    .id("mobile-file-reopen")
                                    .min_h(px(36.0))
                                    .px_3()
                                    .py_1()
                                    .flex()
                                    .items_center()
                                    .gap_2()
                                    .border_t_1()
                                    .border_color(theme::border_subtle())
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(|this, _, _, cx| {
                                            this.file_screen_open = true;
                                            cx.notify();
                                        }),
                                    )
                                    .child(
                                        svg()
                                            .path("icons/pencil.svg")
                                            .size(px(theme::ICON_SM))
                                            .text_color(theme::text_muted()),
                                    )
                                    .child(
                                        div()
                                            .min_w_0()
                                            .flex_1()
                                            .overflow_hidden()
                                            .text_ellipsis()
                                            .whitespace_nowrap()
                                            .text_size(px(theme::FONT_CAPTION))
                                            .text_color(theme::text_secondary())
                                            .child(path),
                                    )
                                    .child(
                                        div()
                                            .flex_shrink_0()
                                            .text_size(px(theme::FONT_MICRO))
                                            .text_color(status_color)
                                            .child(status),
                                    )
                                    .child(
                                        svg()
                                            .path("icons/chevron-right.svg")
                                            .size(px(theme::ICON_SM))
                                            .text_color(theme::text_muted()),
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
        if self.git_commit_detail.is_some() || self.git_commit_detail_loading {
            return self.render_git_commit_screen(cx);
        }
        self.render_git_panel(cx)
    }

    fn render_git_panel(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
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
                                    div().min_w_0().flex_1().child(input_element(
                                        self.git_history_query_input.as_ref(),
                                    )),
                                )
                                .when(
                                    !input_value(&self.git_history_query_input, cx)
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
                                    theme::accent_green()
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
                                    theme::accent_red()
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
                            theme::accent_green()
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
                            theme::accent_red()
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

    /// Full-screen commit view: header with a back button, commit metadata,
    /// the changed-file list, and the full patch body.
    fn render_git_commit_screen(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let detail = self.git_commit_detail.clone();
        let loading = self.git_commit_detail_loading;
        let subject = detail
            .as_ref()
            .map(|detail| detail.summary.subject.clone())
            .unwrap_or_default();
        let short_hash = detail
            .as_ref()
            .map(|detail| detail.summary.short_hash.clone())
            .unwrap_or_default();

        div()
            .size_full()
            .flex()
            .flex_col()
            .bg(theme::workbench_panel_bg())
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(48.0))
                    .px_1()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .id("git-commit-back")
                            .size(px(theme::TOUCH_TARGET))
                            .flex_shrink_0()
                            .flex()
                            .items_center()
                            .justify_center()
                            .cursor_pointer()
                            .active(|style| style.bg(theme::row_pressed_bg()))
                            .on_mouse_up(
                                MouseButton::Left,
                                cx.listener(Self::close_git_commit_screen),
                            )
                            .child(
                                svg()
                                    .path("icons/chevron-left.svg")
                                    .size(px(theme::ICON_SM))
                                    .text_color(theme::text_secondary()),
                            ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(subject),
                            )
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_MICRO))
                                    .font_family("IBM Plex Mono")
                                    .text_color(theme::text_muted())
                                    .child(short_hash),
                            ),
                    ),
            )
            .child(
                div()
                    .id("git-commit-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .when(loading, |body| {
                        body.child(empty_label(locale::common("Loading")))
                    })
                    .when_some(detail.clone(), |body, detail| {
                        let summary = &detail.summary;
                        let stats = detail.files.iter().fold(
                            (0u32, 0u32, 0usize),
                            |(add, del, count), file| {
                                (add + file.additions, del + file.deletions, count + 1)
                            },
                        );
                        body.child(
                            div()
                                .px_3()
                                .py_2()
                                .flex()
                                .flex_col()
                                .gap_1()
                                .child(
                                    div()
                                        .flex()
                                        .items_center()
                                        .gap_2()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::text_muted())
                                        .child(history_relative_time(summary.authored_at_ms))
                                        .child(
                                            div()
                                                .min_w_0()
                                                .flex_1()
                                                .overflow_hidden()
                                                .text_ellipsis()
                                                .whitespace_nowrap()
                                                .child(summary.author_name.clone()),
                                        )
                                        .child(
                                            div()
                                                .font_family("IBM Plex Mono")
                                                .text_color(theme::accent_green())
                                                .child(format!("+{}", stats.0)),
                                        )
                                        .child(
                                            div()
                                                .font_family("IBM Plex Mono")
                                                .text_color(theme::accent_red())
                                                .child(format!("-{}", stats.1)),
                                        ),
                                )
                                .when_some(detail.body.clone(), |meta, commit_body| {
                                    let commit_body = commit_body.trim().to_string();
                                    meta.when(!commit_body.is_empty(), |meta| {
                                        meta.child(
                                            div()
                                                .pt_1()
                                                .text_size(px(theme::FONT_CAPTION))
                                                .whitespace_normal()
                                                .text_color(theme::text_secondary())
                                                .child(commit_body),
                                        )
                                    })
                                })
                                .children(summary.refs.iter().take(4).cloned().map(|reference| {
                                    div()
                                        .mr_1()
                                        .max_w(px(120.0))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .rounded(px(3.0))
                                        .bg(theme::accent_blue().opacity(0.18))
                                        .px_1()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::accent_blue())
                                        .child(reference)
                                })),
                        )
                        .child(
                            div()
                                .h(px(30.0))
                                .px_3()
                                .flex()
                                .items_center()
                                .justify_between()
                                .border_t_1()
                                .border_b_1()
                                .border_color(theme::border_subtle())
                                .text_size(px(theme::FONT_MICRO))
                                .text_color(theme::text_muted())
                                .child(locale::text("Files", "文件", "檔案"))
                                .child(format!("{}", stats.2)),
                        )
                        .children(
                            detail
                                .files
                                .iter()
                                .map(|file| self.render_git_commit_file_row(file)),
                        )
                        .when_some(detail.patch.clone(), |body, patch| {
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
                                    .child(locale::text("Patch", "补丁", "補丁")),
                            )
                            .child(self.render_git_patch_body(&patch))
                            .when(detail.patch_truncated, |body| {
                                body.child(
                                    div()
                                        .px_3()
                                        .py_2()
                                        .text_size(px(theme::FONT_MICRO))
                                        .text_color(theme::accent_yellow())
                                        .child(locale::text(
                                            "The patch is truncated.",
                                            "补丁内容已截断。",
                                            "補丁內容已截斷。",
                                        )),
                                )
                            })
                        })
                        .when(detail.patch.is_none(), |body| {
                            body.child(
                                div()
                                    .px_3()
                                    .py_2()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .child(locale::text(
                                        "This commit has no patch content.",
                                        "该提交没有补丁内容。",
                                        "該提交沒有補丁內容。",
                                    )),
                            )
                        })
                    }),
            )
            .into_any_element()
    }

    /// One changed file in a commit, using the desktop right-rail colors and
    /// A/D/M/R/C/U labels.
    fn render_git_commit_file_row(&self, file: &GitCommitFileChange) -> gpui::AnyElement {
        let color = git_change_text_color_mobile(&GitChange {
            path: file.path.clone(),
            original_path: file.original_path.clone(),
            kind: file.kind,
            staged: false,
            unstaged: true,
            additions: file.additions,
            deletions: file.deletions,
        });
        div()
            .min_h(px(32.0))
            .px_3()
            .py_1()
            .flex()
            .items_center()
            .gap_2()
            .child(
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
                    .font_family("IBM Plex Mono")
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(color)
                    .child(git_change_label_mobile(file.kind)),
            )
            .child(
                div()
                    .min_w_0()
                    .flex_1()
                    .overflow_hidden()
                    .text_ellipsis()
                    .whitespace_nowrap()
                    .text_size(px(theme::FONT_CAPTION))
                    .text_color(theme::text_secondary())
                    .when_some(file.original_path.clone(), |row, original| {
                        row.child(format!("{original} → {}", file.path))
                    })
                    .when(file.original_path.is_none(), |row| {
                        row.child(file.path.clone())
                    }),
            )
            .child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .font_family("IBM Plex Mono")
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(if file.additions > 0 {
                        theme::accent_green()
                    } else {
                        theme::text_muted()
                    })
                    .child(format!("+{}", file.additions)),
            )
            .child(
                div()
                    .w(px(34.0))
                    .flex_none()
                    .font_family("IBM Plex Mono")
                    .text_size(px(theme::FONT_MICRO))
                    .text_color(if file.deletions > 0 {
                        theme::accent_red()
                    } else {
                        theme::text_muted()
                    })
                    .child(format!("-{}", file.deletions)),
            )
            .into_any_element()
    }

    /// The patch body with per-line add/remove coloring, mirroring the desktop
    /// diff surface.
    fn render_git_patch_body(&self, patch: &str) -> gpui::AnyElement {
        div()
            .mx_2()
            .my_1()
            .rounded(px(theme::RADIUS_CONTROL))
            .border_1()
            .border_color(theme::border_subtle())
            .bg(theme::bg_card())
            .p_2()
            .flex()
            .flex_col()
            .children(patch.lines().take(2000).map(|line| {
                let line = line.to_string();
                let color = if line.starts_with('+') && !line.starts_with("+++") {
                    theme::accent_green()
                } else if line.starts_with('-') && !line.starts_with("---") {
                    theme::accent_red()
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
                    theme::accent_green()
                } else if line.starts_with('-') && !line.starts_with("---") {
                    theme::accent_red()
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
            .child(input_shell(self.git_commit_input.as_ref()))
            .when(self.git_commit_confirmation, |panel| {
                panel.child(
                    div()
                        .p_2()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme::accent_yellow())
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
                            .bg(theme::accent_blue()),
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
                                    .bg(theme::accent_blue().opacity(0.18))
                                    .px_1()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::accent_blue())
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
                    this.open_git_commit(hash.clone(), cx);
                }),
            )
            .into_any_element()
    }

    /// Terminal surface: session chips, an auto-fitted cell grid rendered by
    /// the shared TerminalRenderModel, a touch key bar, and the input row.
    fn render_terminal(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let view = self.terminal.state.view(ShellKind::Compact);
        let sessions = self.terminal.state.sessions.clone();
        let active = self.terminal.state.active_session.clone();
        let session = self.terminal.state.active_session.clone();
        let can_create = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalCreate);
        let can_input = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalInput);
        let can_close = self
            .terminal
            .capabilities
            .supports(BackendOperation::TerminalClose);
        let control_latched = self.terminal.state.control_latched;

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(44.0))
                    .px_2()
                    .border_b_1()
                    .border_color(theme::border_subtle())
                    .flex()
                    .items_center()
                    .gap_1()
                    .child(
                        div()
                            .min_w_0()
                            .flex_1()
                            .flex()
                            .flex_col()
                            .justify_center()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .text_ellipsis()
                                    .whitespace_nowrap()
                                    .text_size(px(theme::FONT_BODY))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_color(theme::text_primary())
                                    .child(
                                        session
                                            .as_ref()
                                            .map(|session| session.title.clone())
                                            .unwrap_or_else(|| {
                                                locale::common("Terminal").to_string()
                                            }),
                                    ),
                            )
                            .child(
                                div()
                                    .flex()
                                    .items_center()
                                    .gap_1()
                                    .text_size(px(theme::FONT_MICRO))
                                    .text_color(theme::text_muted())
                                    .when_some(session.clone(), |row, session| {
                                        row.child(
                                            div()
                                                .size(px(6.0))
                                                .rounded_full()
                                                .bg(terminal_status_color(session.status)),
                                        )
                                        .child(format!(
                                            "{}  {}\u{d7}{}",
                                            terminal_status_label(session.status),
                                            session.cols,
                                            session.rows
                                        ))
                                    })
                                    .when(session.is_none(), |row| {
                                        row.child(locale::text(
                                            "No active session",
                                            "没有活动会话",
                                            "沒有活動會話",
                                        ))
                                    }),
                            ),
                    )
                    .child(
                        action_button("create-terminal", locale::common("New"))
                            .when(can_create, |button| {
                                button.on_mouse_up(
                                    MouseButton::Left,
                                    cx.listener(Self::create_terminal),
                                )
                            })
                            .when(!can_create, |button| button.opacity(0.55)),
                    )
                    .when_some(active.clone(), |bar, _| {
                        bar.child(
                            action_button("close-terminal", locale::common("Close"))
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
            .when(sessions.len() > 1, |terminal| {
                terminal.child(
                    div()
                        .id("mobile-terminal-sessions")
                        .flex_shrink_0()
                        .min_h(px(36.0))
                        .overflow_x_scroll()
                        .flex()
                        .items_center()
                        .gap_1()
                        .px_2()
                        .py_1()
                        .children(sessions.iter().map(|item| {
                            let terminal_id = item.id.clone();
                            let selected =
                                active.as_ref().is_some_and(|active| active.id == item.id);
                            div()
                                .id(format!("terminal-chip:{}", item.id))
                                .flex_shrink_0()
                                .h(px(26.0))
                                .rounded(px(theme::RADIUS_CONTROL))
                                .border_1()
                                .border_color(if selected {
                                    theme::accent_blue()
                                } else {
                                    theme::border_default()
                                })
                                .bg(if selected {
                                    theme::bg_card_dim()
                                } else {
                                    theme::bg_card()
                                })
                                .px_2()
                                .flex()
                                .items_center()
                                .gap_1()
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
                                        .size(px(6.0))
                                        .rounded_full()
                                        .bg(terminal_status_color(item.status)),
                                )
                                .child(
                                    div()
                                        .max_w(px(120.0))
                                        .overflow_hidden()
                                        .text_ellipsis()
                                        .whitespace_nowrap()
                                        .text_size(px(theme::FONT_CAPTION))
                                        .text_color(if selected {
                                            theme::text_primary()
                                        } else {
                                            theme::text_secondary()
                                        })
                                        .child(item.title.clone()),
                                )
                        })),
                )
            })
            .when_some(self.terminal_close_confirmation.clone(), |terminal, _| {
                terminal.child(
                    div()
                        .flex_shrink_0()
                        .mx_3()
                        .my_2()
                        .rounded(px(theme::RADIUS_CONTROL))
                        .border_1()
                        .border_color(theme::accent_yellow())
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
                                    action_button(
                                        "cancel-terminal-close",
                                        locale::common("Cancel"),
                                    )
                                    .on_mouse_up(
                                        MouseButton::Left,
                                        cx.listener(Self::cancel_close_terminal),
                                    ),
                                )
                                .child(
                                    action_button(
                                        "confirm-terminal-close",
                                        locale::text("Close terminal", "关闭终端", "關閉終端機"),
                                    )
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
            .child(self.render_terminal_grid(cx))
            .when_some(active.clone(), |terminal, _| {
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
                            .border_t_1()
                            .border_color(theme::border_subtle())
                            .children(view.key_bar.into_iter().map(|action| {
                                let key = action.key;
                                let latched = key == TerminalKey::Control && control_latched;
                                div()
                                    .id(format!("terminal-key:{:?}", key))
                                    .flex_shrink_0()
                                    .h(px(34.0))
                                    .min_w(px(44.0))
                                    .rounded(px(theme::RADIUS_CONTROL))
                                    .border_1()
                                    .border_color(if latched {
                                        theme::accent_blue()
                                    } else {
                                        theme::border_default()
                                    })
                                    .bg(if latched {
                                        theme::accent_blue().opacity(0.16)
                                    } else {
                                        theme::bg_card()
                                    })
                                    .px_2()
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .text_size(px(theme::FONT_CAPTION))
                                    .text_color(if latched {
                                        theme::accent_blue()
                                    } else {
                                        theme::text_secondary()
                                    })
                                    .cursor_pointer()
                                    .active(|style| style.bg(theme::row_pressed_bg()))
                                    .when(can_input, |chip| {
                                        chip.on_mouse_up(
                                            MouseButton::Left,
                                            cx.listener(move |this, _, _, cx| {
                                                this.send_terminal_key(key, cx)
                                            }),
                                        )
                                    })
                                    .when(!can_input, |chip| chip.opacity(0.55))
                                    .child(action.label)
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
                            .child(input_shell(self.terminal_input.as_ref()))
                            .child(
                                action_button("send-terminal-input", locale::common("Send"))
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

    /// The cell grid. The surface reports its bounds on paint so the PTY is
    /// auto-fitted to fill the screen, and the shared render model frame is
    /// rendered cell-by-cell with the desktop palette and cursor treatment.
    fn render_terminal_grid(&self, cx: &mut Context<Self>) -> gpui::AnyElement {
        let weak_entity = cx.weak_entity();
        let frame = self
            .terminal
            .state
            .render
            .as_ref()
            .map(|render| render.frame.clone());
        let has_session = self.terminal.state.active_session.is_some();
        let has_frame = frame.is_some();
        div()
            .id("mobile-terminal-surface")
            .flex_1()
            .min_h_0()
            .relative()
            .overflow_hidden()
            .bg(theme::workbench_bg())
            .on_scroll_wheel(cx.listener(Self::scroll_terminal))
            .child(
                canvas(
                    move |bounds, _, cx| {
                        let width = f32::from(bounds.size.width);
                        let height = f32::from(bounds.size.height);
                        let _ = weak_entity
                            .update(cx, |this, cx| this.fit_terminal_to(width, height, cx));
                    },
                    |_, _, _, _| {},
                )
                .absolute()
                .size_full(),
            )
            .when_some(frame, |surface, frame| {
                let rows = frame.rows;
                let columns = frame.columns;
                let cursor = frame.cursor;
                let mut cells = vec![None; usize::from(rows).saturating_mul(usize::from(columns))];
                for cell in frame.cells {
                    let index = usize::from(cell.row)
                        .saturating_mul(usize::from(columns))
                        .saturating_add(usize::from(cell.column));
                    if let Some(slot) = cells.get_mut(index) {
                        *slot = Some(cell);
                    }
                }
                surface.child(
                    div()
                        .id("mobile-terminal-grid")
                        .absolute()
                        .inset_0()
                        .flex()
                        .flex_col()
                        .justify_end()
                        .pt(px(TERMINAL_VERTICAL_PADDING))
                        .pb(px(TERMINAL_VERTICAL_PADDING))
                        .font_family("IBM Plex Mono")
                        .text_size(px(TERMINAL_FONT_SIZE))
                        .line_height(px(TERMINAL_CELL_HEIGHT))
                        .children((0..rows).map(|row| {
                            div()
                                .h(px(TERMINAL_CELL_HEIGHT))
                                .flex_none()
                                .flex()
                                .overflow_hidden()
                                .children((0..columns).map(|column| {
                                    let index = usize::from(row)
                                        .saturating_mul(usize::from(columns))
                                        .saturating_add(usize::from(column));
                                    self.render_terminal_cell(
                                        TerminalGridPoint { row, column },
                                        cells.get(index).cloned().flatten(),
                                        cursor,
                                    )
                                }))
                        })),
                )
            })
            .when(!has_frame, |surface| {
                surface.child(empty_label(if has_session {
                    "No output yet"
                } else {
                    "Select or create a terminal"
                }))
            })
            .into_any_element()
    }

    /// One terminal cell, using the desktop palette: indexed/RGB colors, wide
    /// glyph spacing, attribute styling, and the host cursor shape.
    fn render_terminal_cell(
        &self,
        point: TerminalGridPoint,
        cell: Option<TerminalCellSnapshot>,
        cursor: Option<vibex_terminal_ui::TerminalCursorSnapshot>,
    ) -> gpui::AnyElement {
        let cell = cell.unwrap_or_else(|| empty_terminal_cell(point));
        if cell.wide_spacer {
            return div()
                .w_0()
                .h(px(TERMINAL_CELL_HEIGHT))
                .flex_none()
                .into_any_element();
        }
        let at_cursor = cursor
            .as_ref()
            .is_some_and(|cursor| cursor.row == point.row && cursor.column == point.column);
        let default_foreground = theme::text_primary();
        let default_background = theme::workbench_bg();
        let mut foreground = terminal_cell_color(
            cell.foreground,
            default_foreground,
            default_background,
            default_foreground,
        );
        let mut background = terminal_cell_color(
            cell.background,
            default_foreground,
            default_background,
            default_background,
        );
        if cell.dim {
            foreground = foreground.opacity(0.68);
        }
        if at_cursor && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Block) {
            background = default_foreground;
            foreground = default_background;
        }
        div()
            .w(px(if cell.wide {
                TERMINAL_CELL_WIDTH * 2.0
            } else {
                TERMINAL_CELL_WIDTH
            }))
            .h(px(TERMINAL_CELL_HEIGHT))
            .flex_none()
            .overflow_hidden()
            .bg(background)
            .text_color(foreground)
            .when(cell.bold, |cell| cell.font_weight(FontWeight::BOLD))
            .when(cell.italic, |cell| cell.italic())
            .when(cell.underline || cell.hyperlink.is_some(), |cell| {
                cell.underline()
            })
            .when(cell.strikeout, |cell| cell.line_through())
            .when(cell.hidden, |cell| cell.invisible())
            .when(
                at_cursor && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Beam),
                |cell| cell.border_l_1().border_color(default_foreground),
            )
            .when(
                at_cursor
                    && cursor.is_some_and(|cursor| cursor.shape == TerminalCursorShape::Underline),
                |cell| cell.border_b_1().border_color(default_foreground),
            )
            .when(
                at_cursor
                    && cursor
                        .is_some_and(|cursor| cursor.shape == TerminalCursorShape::HollowBlock),
                |cell| cell.border_1().border_color(default_foreground),
            )
            .child(if cell.text.is_empty() {
                " ".to_string()
            } else {
                cell.text.clone()
            })
            .into_any_element()
    }
}

impl Render for MobileWorkbench {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.ensure_inputs(window, cx);
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
                                theme::accent_blue()
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
                        .text_color(theme::accent_green())
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
                        .text_color(theme::accent_red())
                        .child(error.message),
                )
            })
            .child(div().flex_1().min_h_0().child(match surface {
                WorkbenchSurface::Files => self.render_files(cx),
                WorkbenchSurface::Git => self.render_git(cx),
                WorkbenchSurface::Terminal => self.render_terminal(cx),
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

fn input_shell(input: Option<&Entity<InputState>>) -> gpui::Div {
    div()
        .h(px(theme::TOUCH_TARGET))
        .flex_1()
        .min_w_0()
        .rounded(px(theme::RADIUS_CONTROL))
        .border_1()
        .border_color(theme::border_default())
        .bg(theme::bg_card())
        .px_1()
        .child(input_element(input))
}

/// The text of a lazily-created kit input, or empty before the first paint.
fn input_value(input: &Option<Entity<InputState>>, cx: &gpui::App) -> gpui::SharedString {
    input
        .as_ref()
        .map(|input| input.read(cx).value())
        .unwrap_or_default()
}

/// The kit input for a field that is created on first paint. The container
/// already draws the frame, so the input contributes only its text surface.
fn input_element(input: Option<&Entity<InputState>>) -> gpui::AnyElement {
    match input {
        Some(input) => Input::new(input).appearance(false).into_any_element(),
        None => div().into_any_element(),
    }
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
            theme::text_primary()
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
        | FileIconKind::Svg => theme::accent_blue(),
        FileIconKind::JavaScript | FileIconKind::Script => theme::accent_yellow(),
        FileIconKind::Json => theme::accent_purple(),
        FileIconKind::Archive | FileIconKind::Config => rgb(0xf0a050).into(),
        FileIconKind::Database | FileIconKind::Spreadsheet => theme::accent_green(),
        FileIconKind::Style => rgb(0x5ed2d9).into(),
        FileIconKind::Markup => rgb(0xf0a050).into(),
        FileIconKind::Audio => rgb(0xd08ad8).into(),
        FileIconKind::Video => rgb(0xf08fc4).into(),
        FileIconKind::Symlink => rgb(0xb091f2).into(),
        FileIconKind::Lock | FileIconKind::Secret => theme::accent_yellow(),
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
        Some(FileGitSignal::Added) => theme::accent_green(),
        Some(FileGitSignal::Untracked) => theme::accent_yellow(),
        Some(FileGitSignal::Ignored) => theme::text_muted(),
        Some(_) => theme::accent_blue(),
        None => theme::text_primary(),
    }
}

fn file_git_signal_color(signal: FileGitSignal) -> gpui::Hsla {
    match signal {
        FileGitSignal::Added => theme::accent_green(),
        FileGitSignal::Untracked => theme::accent_yellow(),
        FileGitSignal::Modified
        | FileGitSignal::Deleted
        | FileGitSignal::Renamed
        | FileGitSignal::Copied
        | FileGitSignal::Conflicted => theme::accent_blue(),
        FileGitSignal::Ignored => theme::text_muted(),
    }
}

/// Same geometry and tones as the desktop `git_selection_indicator`.
fn git_selection_indicator_mobile(state: GitPathSelectionState) -> gpui::AnyElement {
    let selected = state != GitPathSelectionState::Unchecked;
    let mut selected_border = theme::accent_foreground();
    selected_border.a = 0.18;
    let mut marker_color = theme::accent_foreground();
    marker_color.a = 0.72;
    div()
        .size(px(14.0))
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .rounded(px(3.0))
        .border_1()
        .border_color(if selected {
            selected_border
        } else {
            theme::border_input()
        })
        .bg(if selected {
            theme::accent()
        } else {
            theme::workbench_bg()
        })
        .text_color(marker_color)
        .when(state == GitPathSelectionState::Checked, |item| {
            item.child(
                svg()
                    .path("icons/check.svg")
                    .size(px(10.0))
                    .relative()
                    .left(px(0.5))
                    .top(px(0.5)),
            )
        })
        .when(state == GitPathSelectionState::Indeterminate, |item| {
            item.child(
                svg()
                    .path("icons/minus.svg")
                    .size(px(10.0))
                    .relative()
                    .left(px(0.5))
                    .top(px(0.5)),
            )
        })
        .into_any_element()
}

fn file_icon_kind_for_path(path: &str) -> FileIconKind {
    file_icon_descriptor(path, FileEntryKind::File).kind
}

fn git_change_text_color_mobile(change: &GitChange) -> gpui::Hsla {
    match change.kind {
        GitChangeKind::Deleted => theme::text_muted(),
        GitChangeKind::Untracked => theme::status_untracked(),
        GitChangeKind::Added if !change.staged => theme::status_untracked(),
        GitChangeKind::Added => theme::status_added(),
        GitChangeKind::Modified
        | GitChangeKind::Renamed
        | GitChangeKind::Copied
        | GitChangeKind::TypeChanged
        | GitChangeKind::Unmerged
        | GitChangeKind::Unknown => theme::status_modified(),
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
            .border_color(theme::accent_blue())
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
        FileEditorStatus::Conflict | FileEditorStatus::Disconnected => theme::accent_red(),
        FileEditorStatus::Dirty | FileEditorStatus::Saving => theme::accent_yellow(),
        FileEditorStatus::Saved => theme::accent_green(),
        _ => theme::text_muted(),
    }
}

/// Feeds one polled snapshot into the render model. Chunks already applied
/// are skipped by sequence; a dims change, a sequence rewind (terminal
/// reattached), or a gap (retained ring evicted past the next expected chunk)
/// rebuilds the emulator from the retained window, mirroring the desktop
/// raw-buffer rebuild rules. Returns true when any bytes were applied.
fn terminal_feed_snapshot(
    render: &mut vibex_ui::TerminalRenderModel,
    applied_next_sequence: &mut i64,
    snapshot: &TerminalSnapshot,
) -> bool {
    let dims_changed =
        render.frame.rows != snapshot.session.rows || render.frame.columns != snapshot.session.cols;
    let rewound = snapshot.next_sequence < *applied_next_sequence;
    let gap = snapshot
        .chunks
        .first()
        .is_some_and(|chunk| chunk.sequence > *applied_next_sequence);
    let applied_any = *applied_next_sequence > 0;
    if dims_changed || rewound || gap || !applied_any {
        render.reset(snapshot.session.rows, snapshot.session.cols);
        *applied_next_sequence = 0;
    }
    let mut applied = false;
    for chunk in &snapshot.chunks {
        if chunk.sequence < *applied_next_sequence {
            continue;
        }
        render.apply(chunk.data.as_bytes());
        *applied_next_sequence = chunk.sequence + 1;
        applied = true;
    }
    applied
}

/// Status colors follow the desktop right-rail treatment: green for live
/// sessions, red for ended ones, yellow for a stale host connection.
fn terminal_status_color(status: TerminalStatus) -> gpui::Hsla {
    match status {
        TerminalStatus::Running => theme::accent_green(),
        TerminalStatus::Exited | TerminalStatus::Killed => theme::accent_red(),
        TerminalStatus::Stale => theme::accent_yellow(),
    }
}

fn terminal_status_label(status: TerminalStatus) -> &'static str {
    locale::common(match status {
        TerminalStatus::Running => "running",
        TerminalStatus::Exited => "exited",
        TerminalStatus::Killed => "killed",
        TerminalStatus::Stale => "stale",
    })
}

/// The blank cell used wherever the host frame has no explicit cell, matching
/// the desktop surface defaults (default foreground on default background).
fn empty_terminal_cell(point: TerminalGridPoint) -> TerminalCellSnapshot {
    TerminalCellSnapshot {
        row: point.row,
        column: point.column,
        text: " ".into(),
        foreground: TerminalCellColor::Named { index: 256 },
        background: TerminalCellColor::Named { index: 257 },
        bold: false,
        dim: false,
        italic: false,
        underline: false,
        inverse: false,
        hidden: false,
        strikeout: false,
        wide: false,
        wide_spacer: false,
        selected: false,
        hyperlink: None,
    }
}

/// Resolves a terminal cell color against the current theme, mirroring the
/// desktop `terminal_color` mapping (RGB passthrough, 256-color palette, and
/// the emulator's named defaults).
fn terminal_cell_color(
    color: TerminalCellColor,
    default_foreground: gpui::Hsla,
    default_background: gpui::Hsla,
    fallback: gpui::Hsla,
) -> gpui::Hsla {
    match color {
        TerminalCellColor::Rgb { red, green, blue } => {
            rgb((u32::from(red) << 16) | (u32::from(green) << 8) | u32::from(blue)).into()
        }
        TerminalCellColor::Indexed { index } => indexed_terminal_color(index),
        TerminalCellColor::Named { index } if index < 16 => indexed_terminal_color(index as u8),
        TerminalCellColor::Named { index: 256 } => default_foreground,
        TerminalCellColor::Named { index: 257 } => default_background,
        TerminalCellColor::Named { index: 258 } => default_foreground,
        TerminalCellColor::Named { index } if (259..=266).contains(&index) => {
            indexed_terminal_color((index - 259) as u8).opacity(0.68)
        }
        TerminalCellColor::Named { index: 267 } => default_foreground,
        TerminalCellColor::Named { index: 268 } => default_foreground.opacity(0.68),
        TerminalCellColor::Named { .. } => fallback,
    }
}

fn indexed_terminal_color(index: u8) -> gpui::Hsla {
    const ANSI: [u32; 16] = [
        0x2e3436, 0xcc0000, 0x4e9a06, 0xc4a000, 0x3465a4, 0x75507b, 0x06989a, 0xd3d7cf, 0x555753,
        0xef2929, 0x8ae234, 0xfce94f, 0x729fcf, 0xad7fa8, 0x34e2e2, 0xeeeeec,
    ];
    let value = if index < 16 {
        ANSI[usize::from(index)]
    } else if index < 232 {
        let offset = index - 16;
        let levels = [0u32, 95, 135, 175, 215, 255];
        let red = levels[usize::from(offset / 36)];
        let green = levels[usize::from((offset % 36) / 6)];
        let blue = levels[usize::from(offset % 6)];
        (red << 16) | (green << 8) | blue
    } else {
        let gray = 8 + u32::from(index - 232) * 10;
        (gray << 16) | (gray << 8) | gray
    };
    rgb(value).into()
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
    use vibex_core::{TerminalOutputChunk, TerminalSession};

    fn fixture_terminal_session(rows: u16, cols: u16) -> TerminalSession {
        TerminalSession {
            id: TerminalId::new(),
            workspace_id: WorkspaceId::new(),
            title: "Fixture".into(),
            shell: "/bin/sh".into(),
            cwd: "/fixture".into(),
            rows,
            cols,
            status: TerminalStatus::Running,
            created_at_ms: 1,
            updated_at_ms: 1,
            closed_at_ms: None,
        }
    }

    /// Assembles the visible screen text row by row for assertions.
    fn terminal_visible_text(render: &vibex_ui::TerminalRenderModel) -> String {
        let frame = &render.frame;
        let mut rows = vec![String::new(); usize::from(frame.rows)];
        for cell in &frame.cells {
            let Some(row) = rows.get_mut(usize::from(cell.row)) else {
                continue;
            };
            while row.chars().count() < usize::from(cell.column) {
                row.push(' ');
            }
            row.push_str(&cell.text);
        }
        rows.iter()
            .map(|row| row.trim_end().to_string())
            .filter(|row| !row.is_empty())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn terminal_feed_snapshot_applies_incrementally_and_rebuilds_on_gaps() {
        let terminal_id = TerminalId::new();
        let session = fixture_terminal_session(4, 12);
        let chunk = |sequence: i64, data: &str| TerminalOutputChunk {
            terminal_id: terminal_id.clone(),
            sequence,
            data: data.to_string(),
            timestamp_ms: 0,
        };
        let mut render = vibex_ui::TerminalRenderModel::new(4, 12);
        let mut applied = 0;

        // The first snapshot rebuilds from the retained window.
        let first = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "hello"), chunk(2, " world")],
            next_sequence: 3,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &first));
        assert_eq!(applied, 3);
        assert_eq!(terminal_visible_text(&render), "hello world");

        // No new output applies nothing and keeps the screen intact.
        let idle = TerminalSnapshot {
            session: session.clone(),
            chunks: first.chunks.clone(),
            next_sequence: 3,
        };
        assert!(!terminal_feed_snapshot(&mut render, &mut applied, &idle));
        assert_eq!(terminal_visible_text(&render), "hello world");

        // An incremental chunk extends the screen instead of duplicating it.
        let incremental = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "hello"), chunk(2, " world"), chunk(3, "\r\nnext")],
            next_sequence: 4,
        };
        assert!(terminal_feed_snapshot(
            &mut render,
            &mut applied,
            &incremental
        ));
        assert_eq!(applied, 4);
        assert_eq!(terminal_visible_text(&render), "hello world\nnext");

        // A sequence rewind (reattached terminal) rebuilds from scratch.
        let rewound = TerminalSnapshot {
            session: session.clone(),
            chunks: vec![chunk(1, "fresh")],
            next_sequence: 2,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &rewound));
        assert_eq!(applied, 2);
        assert_eq!(terminal_visible_text(&render), "fresh");

        // A gap (ring eviction past the next expected chunk) rebuilds from the
        // retained window only.
        let gapped = TerminalSnapshot {
            session,
            chunks: vec![chunk(9, "tail")],
            next_sequence: 10,
        };
        assert!(terminal_feed_snapshot(&mut render, &mut applied, &gapped));
        assert_eq!(applied, 10);
        assert_eq!(terminal_visible_text(&render), "tail");
    }

    #[test]
    fn terminal_cell_colors_follow_desktop_named_defaults() {
        let foreground = rgb(0xfafafa).into();
        let background = rgb(0x09090b).into();
        assert_eq!(
            terminal_cell_color(
                TerminalCellColor::Named { index: 256 },
                foreground,
                background,
                foreground,
            ),
            foreground
        );
        assert_eq!(
            terminal_cell_color(
                TerminalCellColor::Named { index: 257 },
                foreground,
                background,
                foreground,
            ),
            background
        );
        assert_ne!(
            terminal_cell_color(
                TerminalCellColor::Indexed { index: 1 },
                foreground,
                background,
                foreground,
            ),
            foreground
        );
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
        assert_eq!(theme::workbench_bg(), theme::sidebar_bg());
    }
}

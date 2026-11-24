//! The session shell's service: sessions, a transcript and the composer over
//! the connected runtime.
//!
//! It speaks the backend traits directly (`vibex-backend`): list sessions,
//! fetch a session's timeline, fetch its runtime selection and send a message.
//! The rows are the shared projection `vibex-desktop-model` computes, so the
//! phone shows what the desktop shows for the same timeline.

use std::{
    cell::{Cell, RefCell},
    collections::BTreeMap,
    rc::Rc,
    sync::Arc,
    time::Duration,
};

use base64::Engine as _;
use nsh_base::{Terminal, TerminalApi};
use nsh_core::element::ImageSrc;
use nsh_core::picker::{FilePickRequest, PickRequest, PickedStatus};
use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;
use tokio::{sync::mpsc, task::AbortHandle};
use vibex_backend::{
    AgentBackend, BackendError, BackendErrorKind, BackendEvent, BackendProjection, BackendResult,
    FileBackend, GitBackend, MutationRequest, SidebarBackend, TerminalBackend, TerminalFrameBatch,
    TerminalFrameSubscription, WorkspaceBackend, WorkspaceSummary,
};
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandEntry, AgentCommandExecuteRequest,
    AgentCommandExecuteStatus, AgentCommandExecutionBehavior, AgentCommandSourceKind,
    AgentCommandTrigger, AgentGoalControlRequest, AgentId, AgentNotificationKind, AgentSession,
    AgentSessionRuntimeSelectionState, AgentSessionState, AgentTokenUsage, ApprovalResponder,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, DelegationTaskPhase,
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationRequest,
    ElicitationRequestStatus, ElicitationResolution, ElicitationResolutionAction,
    FetchTimelineItemRequest, FetchTimelineRequest, FilePreviewKind, FileReadRequest,
    FileSearchRequest, FileTreeRequest, ForkAgentSessionRequest, GitBranchCheckoutRequest,
    GitChangeKind, GitCommitDetailRequest, GitCommitRequest, GitDiffRequest, GitHistoryRequest,
    GitManagedWorktreeRecord, GitManagedWorktreeStatus, GitRemoteActionKind,
    GitRemoteActionRequest, GitStageRequest, GitStatusSummary, GitWorktreeArchiveRequest,
    GitWorktreeConflictResolveRequest, GitWorktreeConflictVersion, GitWorktreeCreateRequest,
    GitWorktreeDiscardRequest, GitWorktreeMergeRequest, GitWorktreeOperationRequest,
    GitWorktreeOperationStatus, GitWorktreeRestoreRequest, GoalAction, GoalPhase,
    MessageAttachment, MessageProvenance, OpenWorkspaceRequest, PermissionRequest,
    PermissionRequestStatus, PermissionResolution, PermissionResponseKind, PlanStepStatus,
    ProjectId, RemoteServerKind, RemoteSidebarHierarchyMode, RemoteSidebarItemKind,
    RemoteSidebarNewSessionLocation, RemoteSidebarOrganizationMutation,
    RemoteSidebarOrganizationSnapshot, RenameAgentSessionRequest, RequestId,
    ResolveElicitationRequest, ResolvePermissionRequest, RuntimeModelSelection,
    RuntimeOptionAvailability, RuntimeSelectionInteraction, SendAgentMessageRequest,
    SessionRuntimeFeatureKind, SessionRuntimeOption, SessionRuntimeOptionCatalog,
    SessionRuntimeSelection, SetDesiredAgentSessionRuntimeRequest, TeamSnapshotRequest,
    TeamTaskAction, TeamTaskControlRequest, TerminalCreateRequest, TerminalId,
    TerminalResizeRequest, TerminalSession, TerminalStatus, TerminalWriteRequest, TimelinePage,
    TimelinePayload, UserMessageDelivery, VibexSessionId, VibexUseMention, VibexUseRef,
    WorkspaceId, unix_timestamp_ms,
};
use vibex_desktop_model::{
    FileIconKind, FileTreeLoadState, FileTreeProjection, GitPathSelectionState, GitQueryKind,
    GitTreeRowKind, GitWorkbenchState, NewSessionLocation, ReasoningDisplayMode,
    SidebarHierarchyMode, SidebarOrganizationView, SidebarProjectAppearance, SidebarProjectLogo,
    SidebarProjectLogoColor, TimelineConversationTurn, TimelineModel, TimelineRow, TimelineRowKind,
};
use vibex_mobile_pairing::MobileCredentialBundle;
use vibex_nsh_ui::{
    copy,
    screens::connecting::ConnectionState,
    screens::files::{
        FileEntry, FileGlyph, FilePreview, FilePreviewKindView, FileSearchRow, SearchOptions,
    },
    screens::git::{
        BranchRow, CommitDetailPreview, CommitRow, GitChangeRow, GitDiffPreview,
        WorktreeConflictRow, WorktreeOperationRow, WorktreeRow,
    },
    screens::new_session::{DirectoryEntry, ProjectBrowserHost, RuntimeChoice, WorkspaceChoice},
    screens::session::{
        ActivityDetail, ActivityGlyph, ActivityView, AttachmentItem, CommandEntry,
        ComposerHistoryEntry, ConversationState, EntryKind, GoalControl, GoalPhaseView, GoalView,
        PendingPermission, PendingQuestion, PermissionChoice, PermissionOption, PlanStepState,
        PlanStepView, PlanView, QuestionAnswer, QuestionField, QuestionFieldKind, QuestionOption,
        QuestionResponse, QueuedMessageView, RuntimeChoiceView, RuntimeFeatureView,
        RuntimeOptionView, RuntimeSummaryView, ScheduledSendView, SessionHost, SessionState,
        SessionSummary, SidebarRowKind, SidebarRowView, SidebarSectionKind, SidebarSectionView,
        SidebarTree, TimelineEntry, TokenUsageView, TurnFileChange,
    },
    screens::team::TeamTaskView,
    screens::terminal::TerminalChoice,
};
use vibex_ui::{
    timeline::{self as activity, Activity, ActivityKind, ActivitySummary},
    tool_detail,
};

use crate::{pairing::error_text, sidebar};

/// How many timeline items one read asks for.
const TIMELINE_LIMIT: u32 = 200;

/// Where the per-session composer drafts are kept.
const DRAFTS_KEY: &str = "vibex.mobile.drafts.v1";
/// Where the newest transcript window of the last few sessions is kept.
const TIMELINE_CACHE_KEY: &str = "vibex.mobile.timeline.v1";
/// Where the sessions the reader pinned are kept.
const PINS_KEY: &str = "vibex.mobile.pins.v1";
/// At most this many conversations are cached.
const TIMELINE_CACHE_SESSIONS: usize = 4;

/// One session's cached transcript window.
#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct CachedTimeline {
    saved_at_ms: i64,
    has_older: bool,
    items: Vec<vibex_core::TimelineItem>,
}

/// The wait before the first reconnect attempt, and its ceiling.
const RECONNECT_MIN_MS: u64 = 500;
const RECONNECT_MAX_MS: u64 = 15_000;

/// A user message the composer is replacing.
#[derive(Clone, Debug)]
struct EditState {
    user_sequence: i64,
    expected_end_sequence: i64,
}

/// The session shell's state and actions.
#[derive(Clone)]
pub struct SessionService {
    /// The credential's host id, for the app's runtime bookkeeping.
    bundle_host_id: Rc<String>,
    /// The concrete client, for the one call the traits do not expose:
    /// establishing the route before the first operation.
    web: Arc<vibex_remote_client::WebRemoteBackend>,
    backend: Arc<dyn AgentBackend>,
    terminal_backend: Arc<dyn TerminalBackend>,
    file_backend: Arc<dyn FileBackend>,
    git_backend: Arc<dyn GitBackend>,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    sidebar_backend: Arc<dyn SidebarBackend>,
    sidebar_sync: Rc<futures_util::lock::Mutex<()>>,
    sidebar_snapshot: Rc<RefCell<RemoteSidebarOrganizationSnapshot>>,
    sidebar_view: Signal<SidebarOrganizationView>,
    sidebar_detailed: Signal<bool>,
    sidebar_mode_pending: Rc<Cell<Option<bool>>>,
    sidebar_mode_request: Rc<Cell<u64>>,
    sidebar_sections: Signal<Vec<SidebarSectionView>>,
    folder_open: Signal<bool>,
    folder_name: Signal<String>,
    folder_parent: Signal<Option<String>>,
    folder_scope: Signal<(Option<String>, Option<String>)>,
    move_open: Signal<Option<String>>,
    sessions: Signal<Vec<SessionSummary>>,
    session_records: Signal<Vec<AgentSession>>,
    active: Signal<Option<String>>,
    pinned: Signal<Vec<String>>,
    unread: Signal<Vec<String>>,
    auto_continue_sessions: Signal<Vec<String>>,
    timeline: Signal<Vec<TimelineEntry>>,
    conversation: Signal<ConversationState>,
    draft: Signal<String>,
    sending: Signal<bool>,
    error: Signal<Option<SharedString>>,
    runtime: Signal<Option<SharedString>>,
    pending: Signal<Option<PendingPermission>>,
    question: Signal<Option<PendingQuestion>>,
    /// The question the card shows, kept for typed submissions.
    question_request: Rc<RefCell<Option<ElicitationRequest>>>,
    commands: Signal<Vec<CommandEntry>>,
    commands_loaded: Rc<Cell<bool>>,
    mentions: Signal<Vec<CommandEntry>>,
    mentions_loaded: Rc<Cell<bool>>,
    skills: Signal<Vec<CommandEntry>>,
    skills_loaded: Rc<Cell<bool>>,
    /// The raw entries behind the palettes, for arming a command or reference.
    entry_catalog: Rc<RefCell<BTreeMap<String, AgentCommandEntry>>>,
    armed_command: Rc<RefCell<Option<AgentCommandEntry>>>,
    armed_mentions: Rc<RefCell<Vec<(String, VibexUseMention)>>>,
    terminal_open: Signal<bool>,
    terminal_visible: Signal<bool>,
    terminal_title: Signal<Option<SharedString>>,
    terminal_error: Signal<Option<SharedString>>,
    terminal_ref: ComponentRef<Terminal>,
    terminal_id: Signal<Option<String>>,
    terminal_busy: Signal<bool>,
    terminal_output_revision: Signal<u64>,
    terminal_size: Rc<Cell<(u16, u16)>>,
    terminal_tx: Rc<RefCell<Option<mpsc::Sender<TerminalCommand>>>>,
    terminal_stream: Rc<RefCell<Option<AbortHandle>>>,
    terminal_writer: Rc<RefCell<Option<AbortHandle>>>,
    /// Bytes of a UTF-8 character split across two frames.
    terminal_pending: Rc<RefCell<Vec<u8>>>,
    terminal_choices: Signal<Vec<TerminalChoice>>,
    /// Bumped when the terminal closes, so its stream stops writing.
    terminal_generation: Rc<Cell<u64>>,
    files_open: Signal<bool>,
    file_tree: Rc<RefCell<FileTreeProjection>>,
    file_read_run: Rc<Cell<u64>>,
    file_search_run: Rc<Cell<u64>>,
    workbench_generation: Rc<Cell<u64>>,
    files_entries: Signal<Vec<FileEntry>>,
    files_search: Signal<Vec<FileSearchRow>>,
    files_searching: Signal<bool>,
    files_path: Signal<Option<String>>,
    files_preview: Signal<Option<FilePreview>>,
    files_error: Signal<Option<SharedString>>,
    files_workspace: Rc<RefCell<Option<WorkspaceId>>>,
    git_open: Signal<bool>,
    git_model: Rc<RefCell<GitWorkbenchState>>,
    git_branch: Signal<Option<SharedString>>,
    git_totals: Signal<(u32, u32)>,
    git_changes: Signal<Vec<GitChangeRow>>,
    git_diff: Signal<Option<GitDiffPreview>>,
    git_error: Signal<Option<SharedString>>,
    git_selected: Signal<Vec<String>>,
    git_message: Signal<String>,
    git_committing: Signal<bool>,
    git_amend: Signal<bool>,
    git_push_after: Signal<bool>,
    git_remote_busy: Signal<bool>,
    git_branches: Signal<Vec<BranchRow>>,
    git_branch_menu: Signal<bool>,
    git_history_ref: Rc<RefCell<Option<String>>>,
    worktrees: Signal<Vec<WorktreeRow>>,
    worktree_menu: Signal<bool>,
    worktree_busy: Signal<bool>,
    worktree_error: Signal<Option<SharedString>>,
    worktree_operations: Signal<Vec<WorktreeOperationRow>>,
    worktree_records: Rc<RefCell<Vec<GitManagedWorktreeRecord>>>,
    git_history: Signal<Vec<CommitRow>>,
    git_show_history: Signal<bool>,
    git_commit_detail: Signal<Option<CommitDetailPreview>>,
    attachments: Signal<Vec<AttachmentItem>>,
    /// Makes each picked attachment's id unique for the composer.
    attachment_seq: Rc<Cell<u64>>,
    session_actions: Signal<Option<String>>,
    session_rename: Signal<String>,
    session_busy: Signal<bool>,
    auto_continue_paused: Signal<Vec<String>>,
    scheduled_sends: Signal<Vec<ScheduledSendView>>,
    new_session_open: Signal<bool>,
    new_workspaces: Signal<Vec<WorkspaceChoice>>,
    new_runtimes: Signal<Vec<RuntimeChoice>>,
    new_runtime_options: Rc<RefCell<Vec<SessionRuntimeOption>>>,
    new_workspace: Signal<Option<String>>,
    new_runtime: Signal<Option<String>>,
    new_worktree_branch: Signal<String>,
    new_base_ref: Signal<String>,
    project_dirs: Signal<Vec<DirectoryEntry>>,
    project_browser_open: Signal<bool>,
    project_dir_path: Signal<Option<String>>,
    project_dir_parent: Signal<Option<String>>,
    project_busy: Signal<bool>,
    project_error: Signal<Option<SharedString>>,
    new_title: Signal<String>,
    new_prompt: Signal<String>,
    new_attachments: Signal<Vec<AttachmentItem>>,
    /// The folder a session created from its row menu is filed into once the
    /// runtime answers. The folder row's menu promises that destination.
    new_session_folder: Signal<Option<String>>,
    new_busy: Signal<bool>,
    new_error: Signal<Option<SharedString>>,
    workspace_records: Signal<Vec<WorkspaceSummary>>,
    resolving: Signal<bool>,
    /// The session's projection, kept for the live stream to append to.
    model: Rc<RefCell<TimelineModel>>,
    /// Where the connection to the runtime stands.
    connection: Signal<ConnectionState>,
    /// Why the last connection attempt failed, when it did.
    connection_error: Signal<Option<SharedString>>,
    /// Whether the transcript has older history, and a read of it is running.
    has_older: Signal<bool>,
    loading_older: Signal<bool>,
    /// The service lives until its runtime is replaced.
    alive: Rc<Cell<bool>>,
    /// A retry asked for by the page, consumed by the connection loop.
    retry_now: Rc<Cell<bool>>,
    /// The composer draft of each session this phone has seen.
    drafts: Rc<RefCell<BTreeMap<String, String>>>,
    attachment_drafts: Rc<RefCell<BTreeMap<String, Vec<AttachmentItem>>>>,
    /// The serialized drafts last written, so an unchanged map is not rewritten.
    drafts_snapshot: Rc<RefCell<String>>,
    /// The state the runtime last reported for each session, for turn folds.
    session_states: Rc<RefCell<BTreeMap<String, AgentSessionState>>>,
    /// The turns whose working rows the reader opened.
    turn_disclosures: Rc<RefCell<BTreeMap<String, bool>>>,
    /// The rows whose full detail the reader opened, and the detail itself.
    expanded_rows: Rc<RefCell<std::collections::BTreeSet<String>>>,
    row_details: Rc<RefCell<ActivityDetailsCache>>,
    loading_row_details: Rc<RefCell<std::collections::BTreeSet<(String, i64)>>>,
    /// The open session's runtime selection and the runtime's catalogue.
    runtime_state: Rc<RefCell<Option<AgentSessionRuntimeSelectionState>>>,
    runtime_catalog: Signal<Option<SessionRuntimeOptionCatalog>>,
    runtime_summary: Signal<Option<RuntimeSummaryView>>,
    runtime_option_views: Signal<Vec<RuntimeOptionView>>,
    runtime_reasoning: Signal<Vec<RuntimeChoiceView>>,
    runtime_modes: Signal<Vec<RuntimeChoiceView>>,
    runtime_features: Signal<Vec<RuntimeFeatureView>>,
    runtime_open: Signal<bool>,
    runtime_busy: Signal<bool>,
    token_usage: Signal<Option<TokenUsageView>>,
    needs_continue: Signal<bool>,
    team_open: Signal<bool>,
    team_tasks: Signal<Vec<TeamTaskView>>,
    team_busy: Signal<bool>,
    team_error: Signal<Option<SharedString>>,
    team_revisions: Rc<RefCell<BTreeMap<String, u64>>>,
    attachment_preview: Signal<Option<AttachmentItem>>,
    show_token_usage: Signal<bool>,
    show_plan_goal: Signal<bool>,
    show_generation_status: Signal<bool>,
    show_reasoning: Signal<bool>,
    auto_continue: Signal<bool>,
    auto_continue_armed: Rc<Cell<bool>>,
    find_open: Signal<bool>,
    plan: Signal<Option<PlanView>>,
    goal: Signal<Option<GoalView>>,
    history: Signal<Vec<ComposerHistoryEntry>>,
    history_open: Signal<bool>,
    queued: Signal<Vec<QueuedMessageView>>,
    queue_paused: Signal<bool>,
    queue_manual: Signal<bool>,
    schedule_open: Signal<bool>,
    scheduled: Signal<bool>,
    scheduled_message_id: Rc<RefCell<Option<u64>>>,
    editable_sequence: Signal<i64>,
    editing: Signal<bool>,
    edit_state: Rc<RefCell<Option<EditState>>>,
    /// Notification id -> session id, for a press that arrives later.
    notification_sessions: Rc<RefCell<BTreeMap<String, String>>>,
    /// Bumped when the service stops, so its notification reader ends.
    notification_generation: Rc<Cell<u64>>,
    on_disconnect: Rc<dyn Fn()>,
    on_switch_runtime: Rc<dyn Fn()>,
    on_open_settings: Rc<dyn Fn()>,
    on_runtime_renamed: Rc<dyn Fn(&str)>,
    on_device_renamed: Rc<dyn Fn(&str)>,
    /// Tells the pairing store what kind of runtime the peer reported.
    on_server_kind: Rc<dyn Fn(RemoteServerKind)>,
}

type ActivityDetailsCache = BTreeMap<String, (i64, Vec<ActivityDetail>)>;

impl SessionService {
    /// Builds the service for a connected runtime and loads its sessions.
    pub fn new(
        bundle: &MobileCredentialBundle,
        on_disconnect: Rc<dyn Fn()>,
        on_switch_runtime: Rc<dyn Fn()>,
        on_open_settings: Rc<dyn Fn()>,
        on_runtime_renamed: Rc<dyn Fn(&str)>,
        on_device_renamed: Rc<dyn Fn(&str)>,
        on_server_kind: Rc<dyn Fn(RemoteServerKind)>,
    ) -> BackendResult<Self> {
        let web = bundle.backend()?;
        let terminal_backend: Arc<dyn TerminalBackend> = web.clone();
        let file_backend: Arc<dyn FileBackend> = web.clone();
        let git_backend: Arc<dyn GitBackend> = web.clone();
        let workspace_backend: Arc<dyn WorkspaceBackend> = web.clone();
        let sidebar_backend: Arc<dyn SidebarBackend> = web.clone();
        let backend: Arc<dyn AgentBackend> = web.clone();
        let runtime = bundle.host_label();
        let service = Self {
            bundle_host_id: Rc::new(bundle.host_id().to_owned()),
            web,
            backend,
            terminal_backend,
            file_backend,
            git_backend,
            workspace_backend,
            sidebar_backend,
            sidebar_sync: Rc::new(futures_util::lock::Mutex::new(())),
            sidebar_snapshot: Rc::new(RefCell::new(Default::default())),
            sidebar_view: signal(SidebarOrganizationView::default()),
            sidebar_detailed: signal(false),
            sidebar_mode_pending: Rc::new(Cell::new(None)),
            sidebar_mode_request: Rc::new(Cell::new(0)),
            sidebar_sections: signal(Vec::new()),
            folder_open: signal(false),
            folder_name: signal(String::new()),
            folder_parent: signal(None),
            folder_scope: signal((None, None)),
            move_open: signal(None),
            sessions: signal(Vec::new()),
            session_records: signal(Vec::new()),
            active: signal(None),
            pinned: signal(load_pins()),
            unread: signal(Vec::new()),
            auto_continue_sessions: signal(Vec::new()),
            timeline: signal(Vec::new()),
            conversation: signal(ConversationState::Ready),
            draft: signal(String::new()),
            sending: signal(false),
            error: signal(None),
            runtime: signal(Some(runtime.into())),
            pending: signal(None),
            question: signal(None),
            question_request: Rc::new(RefCell::new(None)),
            commands: signal(Vec::new()),
            commands_loaded: Rc::new(Cell::new(false)),
            mentions: signal(Vec::new()),
            mentions_loaded: Rc::new(Cell::new(false)),
            skills: signal(Vec::new()),
            skills_loaded: Rc::new(Cell::new(false)),
            entry_catalog: Rc::new(RefCell::new(BTreeMap::new())),
            armed_command: Rc::new(RefCell::new(None)),
            armed_mentions: Rc::new(RefCell::new(Vec::new())),
            terminal_open: signal(false),
            terminal_visible: signal(false),
            terminal_title: signal(None),
            terminal_error: signal(None),
            terminal_ref: component_ref::<Terminal>(),
            terminal_id: signal(None),
            terminal_busy: signal(false),
            terminal_output_revision: signal(0),
            terminal_size: Rc::new(Cell::new((80, 24))),
            terminal_tx: Rc::new(RefCell::new(None)),
            terminal_stream: Rc::new(RefCell::new(None)),
            terminal_writer: Rc::new(RefCell::new(None)),
            terminal_pending: Rc::new(RefCell::new(Vec::new())),
            terminal_choices: signal(Vec::new()),
            terminal_generation: Rc::new(Cell::new(0)),
            files_open: signal(false),
            file_tree: Rc::new(RefCell::new(FileTreeProjection::default())),
            file_read_run: Rc::new(Cell::new(0)),
            file_search_run: Rc::new(Cell::new(0)),
            workbench_generation: Rc::new(Cell::new(0)),
            files_entries: signal(Vec::new()),
            files_search: signal(Vec::new()),
            files_searching: signal(false),
            files_path: signal(None),
            files_preview: signal(None),
            files_error: signal(None),
            files_workspace: Rc::new(RefCell::new(None)),
            git_open: signal(false),
            git_model: Rc::new(RefCell::new(GitWorkbenchState::default())),
            git_branch: signal(None),
            git_totals: signal((0, 0)),
            git_changes: signal(Vec::new()),
            git_diff: signal(None),
            git_error: signal(None),
            git_selected: signal(Vec::new()),
            git_message: signal(String::new()),
            git_committing: signal(false),
            git_amend: signal(false),
            git_push_after: signal(false),
            git_remote_busy: signal(false),
            git_branches: signal(Vec::new()),
            git_branch_menu: signal(false),
            git_history_ref: Rc::new(RefCell::new(None)),
            worktrees: signal(Vec::new()),
            worktree_menu: signal(false),
            worktree_busy: signal(false),
            worktree_error: signal(None),
            worktree_operations: signal(Vec::new()),
            worktree_records: Rc::new(RefCell::new(Vec::new())),
            git_history: signal(Vec::new()),
            git_show_history: signal(false),
            git_commit_detail: signal(None),
            attachments: signal(Vec::new()),
            attachment_seq: Rc::new(Cell::new(0)),
            session_actions: signal(None),
            session_rename: signal(String::new()),
            session_busy: signal(false),
            auto_continue_paused: signal(Vec::new()),
            scheduled_sends: signal(Vec::new()),
            new_session_open: signal(false),
            new_workspaces: signal(Vec::new()),
            new_runtimes: signal(Vec::new()),
            new_runtime_options: Rc::new(RefCell::new(Vec::new())),
            new_workspace: signal(None),
            new_runtime: signal(None),
            new_worktree_branch: signal(String::new()),
            new_base_ref: signal(String::new()),
            project_dirs: signal(Vec::new()),
            project_browser_open: signal(false),
            project_dir_path: signal(None),
            project_dir_parent: signal(None),
            project_busy: signal(false),
            project_error: signal(None),
            new_title: signal(String::new()),
            new_prompt: signal(String::new()),
            new_attachments: signal(Vec::new()),
            new_session_folder: signal(None),
            new_busy: signal(false),
            new_error: signal(None),
            workspace_records: signal(Vec::new()),
            resolving: signal(false),
            model: Rc::new(RefCell::new(TimelineModel::default())),
            connection: signal(ConnectionState::Connecting),
            connection_error: signal(None),
            has_older: signal(false),
            loading_older: signal(false),
            alive: Rc::new(Cell::new(true)),
            retry_now: Rc::new(Cell::new(false)),
            drafts: Rc::new(RefCell::new(load_drafts())),
            attachment_drafts: Rc::new(RefCell::new(BTreeMap::new())),
            drafts_snapshot: Rc::new(RefCell::new(String::new())),
            session_states: Rc::new(RefCell::new(BTreeMap::new())),
            turn_disclosures: Rc::new(RefCell::new(BTreeMap::new())),
            expanded_rows: Rc::new(RefCell::new(std::collections::BTreeSet::new())),
            row_details: Rc::new(RefCell::new(BTreeMap::new())),
            loading_row_details: Rc::new(RefCell::new(Default::default())),
            runtime_state: Rc::new(RefCell::new(None)),
            runtime_catalog: signal(None),
            runtime_summary: signal(None),
            runtime_option_views: signal(Vec::new()),
            runtime_reasoning: signal(Vec::new()),
            runtime_modes: signal(Vec::new()),
            runtime_features: signal(Vec::new()),
            runtime_open: signal(false),
            runtime_busy: signal(false),
            token_usage: signal(None),
            needs_continue: signal(false),
            team_open: signal(false),
            team_tasks: signal(Vec::new()),
            team_busy: signal(false),
            team_error: signal(None),
            team_revisions: Rc::new(RefCell::new(BTreeMap::new())),
            attachment_preview: signal(None),
            show_token_usage: signal(crate::settings::show_token_usage()),
            show_plan_goal: signal(crate::settings::show_plan_goal()),
            show_generation_status: signal(crate::settings::show_generation_status()),
            show_reasoning: signal(crate::settings::show_reasoning()),
            auto_continue: signal(crate::settings::auto_continue()),
            auto_continue_armed: Rc::new(Cell::new(false)),
            find_open: signal(false),
            plan: signal(None),
            goal: signal(None),
            history: signal(Vec::new()),
            history_open: signal(false),
            queued: signal(Vec::new()),
            queue_paused: signal(false),
            queue_manual: signal(false),
            schedule_open: signal(false),
            scheduled: signal(false),
            scheduled_message_id: Rc::new(RefCell::new(None)),
            editable_sequence: signal(0),
            editing: signal(false),
            edit_state: Rc::new(RefCell::new(None)),
            notification_sessions: Rc::new(RefCell::new(BTreeMap::new())),
            notification_generation: Rc::new(Cell::new(0)),
            on_disconnect,
            on_switch_runtime,
            on_open_settings,
            on_runtime_renamed,
            on_device_renamed,
            on_server_kind,
        };
        service.start_connection();
        service.start_draft_saver();
        service.start_screen_policy();
        service.start_notification_presses();
        Ok(service)
    }

    /// Stops the service's tasks when its runtime is replaced.
    pub fn shutdown(&self) {
        self.alive.set(false);
        self.notification_generation
            .set(self.notification_generation.get().wrapping_add(1));
        crate::host::set_keep_screen_on(false);
        self.stop_terminal_tasks();
        self.stash_draft();
        self.persist_drafts();
    }

    /// The runtime this service speaks to.
    pub fn host_id(&self) -> String {
        (*self.bundle_host_id).clone()
    }

    /// Reconnects when the app comes back to the foreground.
    pub fn resume(&self) {
        // A stream that survived the background may still have missed events;
        // read the list again and let the loop retry as soon as it can.
        self.retry_now.set(true);
        if self.connection.get_untracked() == ConnectionState::Online {
            self.refresh();
        } else {
            self.retry();
        }
    }

    /// Asks the connection loop to try again now.
    pub fn retry(&self) {
        self.connection_error.set(None);
        self.connection.set(ConnectionState::Connecting);
        self.retry_now.set(true);
    }

    /// The shell's bindings and callbacks.
    pub fn host(&self) -> SessionHost {
        SessionHost {
            sessions: self.sessions,
            active: self.active,
            pinned: self.pinned,
            auto_continue_sessions: self.auto_continue_sessions,
            sidebar: self.sidebar_host(),
            sidebar_sections: self.sidebar_sections,
            folder_open: self.folder_open,
            folder_name: self.folder_name,
            on_open_folder: {
                let service = self.clone();
                Rc::new(move || {
                    service.open_sidebar_folder(None, None, None);
                })
            },
            on_open_subfolder: {
                let service = self.clone();
                Rc::new(move |parent: &str| {
                    let scope = service.sidebar_view.with(|view| {
                        view.organization
                            .folders
                            .get(parent)
                            .map(|folder| (folder.project_id.clone(), folder.workspace_id.clone()))
                            .unwrap_or_default()
                    });
                    service.open_sidebar_folder(scope.0, scope.1, Some(parent.to_owned()));
                })
            },
            on_close_folder: {
                let service = self.clone();
                Rc::new(move || service.folder_open.set(false))
            },
            on_create_folder: {
                let service = self.clone();
                Rc::new(move || service.create_folder())
            },
            on_rename_folder: {
                let service = self.clone();
                Rc::new(move |id: &str, name: &str| service.rename_folder(id, name))
            },
            on_delete_folder: {
                let service = self.clone();
                Rc::new(move |id: &str| service.delete_folder(id))
            },
            on_rename_group: {
                let service = self.clone();
                Rc::new(move |id: &str, name: &str| service.rename_group(id, name))
            },
            on_delete_group: {
                let service = self.clone();
                Rc::new(move |id: &str| service.delete_group(id))
            },
            on_set_group_members: {
                let service = self.clone();
                Rc::new(move |id: &str, members: &[String]| service.set_group_members(id, members))
            },
            on_set_group_pinned: {
                let service = self.clone();
                Rc::new(move |id: &str, pinned: bool| service.set_group_pinned(id, pinned))
            },
            on_set_group_auto_continue: {
                let service = self.clone();
                Rc::new(move |id: &str, enabled: bool| service.set_group_auto_continue(id, enabled))
            },
            on_set_session_auto_continue: {
                let service = self.clone();
                Rc::new(move |id: &str, enabled: bool| {
                    service.set_session_auto_continue(id, enabled)
                })
            },
            on_create_group: {
                let service = self.clone();
                Rc::new(move |name: &str, members: &[String]| service.create_group(name, members))
            },
            move_open: self.move_open,
            on_open_move: {
                let service = self.clone();
                Rc::new(move || {
                    if let Some(id) = service.session_actions.get_untracked() {
                        service.move_open.set(Some(id));
                    }
                })
            },
            on_move_session: {
                let service = self.clone();
                Rc::new(move |id: &str, folder: Option<&str>| {
                    service.move_session_to_folder(id, folder)
                })
            },
            on_close_move: {
                let service = self.clone();
                Rc::new(move || service.move_open.set(None))
            },
            on_toggle_pin: {
                let service = self.clone();
                Rc::new(move |id: &str| service.toggle_pin(id))
            },
            timeline: self.timeline,
            conversation: self.conversation,
            draft: self.draft,
            sending: self.sending,
            error: self.error,
            connection: self.connection,
            connection_error: self.connection_error,
            has_older: self.has_older,
            loading_older: self.loading_older,
            on_retry: {
                let service = self.clone();
                Rc::new(move || service.retry())
            },
            runtime: self.runtime,
            pending: self.pending,
            question: self.question,
            commands: self.commands,
            mentions: self.mentions,
            skills: self.skills,
            on_skill_query: {
                let service = self.clone();
                Rc::new(move |query: &str| service.query_skills(query))
            },
            on_arm_entry: {
                let service = self.clone();
                Rc::new(move |id: &str| service.arm_entry(id))
            },
            terminal_open: self.terminal_open,
            terminal_visible: self.terminal_visible,
            terminal_title: self.terminal_title,
            terminal_error: self.terminal_error,
            terminal_ref: self.terminal_ref,
            terminal_choices: self.terminal_choices,
            terminal_id: self.terminal_id,
            terminal_busy: self.terminal_busy,
            terminal_output_revision: self.terminal_output_revision,
            files_open: self.files_open,
            files_entries: self.files_entries,
            files_search: self.files_search,
            files_searching: self.files_searching,
            files_path: self.files_path,
            on_search_files: {
                let service = self.clone();
                Rc::new(move |query: &str, options| service.search_files(query, options))
            },
            files_preview: self.files_preview,
            files_error: self.files_error,
            git_open: self.git_open,
            git_branch: self.git_branch,
            git_totals: self.git_totals,
            git_changes: self.git_changes,
            git_diff: self.git_diff,
            git_error: self.git_error,
            git_selected: self.git_selected,
            on_toggle_git_selection: {
                let service = self.clone();
                Rc::new(move |path| {
                    let mut model = service.git_model.borrow_mut();
                    let selected =
                        model.path_selection_state(path) != GitPathSelectionState::Checked;
                    model.select_path_prefix(path, selected);
                    drop(model);
                    service.publish_git_changes();
                })
            },
            on_toggle_git_directory: {
                let service = self.clone();
                Rc::new(move |paths| {
                    service
                        .git_model
                        .borrow_mut()
                        .toggle_change_directories(paths);
                    service.publish_git_changes();
                })
            },
            on_toggle_git_directories: {
                let service = self.clone();
                Rc::new(move || {
                    service
                        .git_model
                        .borrow_mut()
                        .toggle_all_change_directories();
                    service.publish_git_changes();
                })
            },
            on_revert_git_selection: {
                let service = self.clone();
                Rc::new(move |paths| service.revert_paths(paths.to_vec()))
            },
            git_message: self.git_message,
            git_committing: self.git_committing,
            git_amend: self.git_amend,
            git_push_after: self.git_push_after,
            git_remote_busy: self.git_remote_busy,
            git_branches: self.git_branches,
            git_branch_menu: self.git_branch_menu,
            on_open_git_branches: {
                let service = self.clone();
                Rc::new(move || service.open_git_branches())
            },
            on_close_git_branches: {
                let service = self.clone();
                Rc::new(move || service.git_branch_menu.set(false))
            },
            on_select_git_branch: {
                let service = self.clone();
                Rc::new(move |name: &str| service.select_git_branch(name))
            },
            worktrees: self.worktrees,
            worktree_menu: self.worktree_menu,
            worktree_busy: self.worktree_busy,
            worktree_error: self.worktree_error,
            on_open_worktrees: {
                let service = self.clone();
                Rc::new(move || service.open_worktrees())
            },
            on_close_worktrees: {
                let service = self.clone();
                Rc::new(move || service.worktree_menu.set(false))
            },
            on_archive_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.archive_worktree(id))
            },
            on_merge_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.merge_worktree(id))
            },
            on_discard_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.discard_worktree(id))
            },
            worktree_operations: self.worktree_operations,
            on_continue_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.continue_worktree_merge(id))
            },
            on_abort_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.abort_worktree_merge(id))
            },
            on_resolve_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str, path: &str, source: bool| {
                    service.resolve_worktree_conflict(id, path, source)
                })
            },
            on_restore_worktree: {
                let service = self.clone();
                Rc::new(move |id: &str| service.restore_worktree(id))
            },
            on_toggle_git_amend: {
                let service = self.clone();
                Rc::new(move || service.git_amend.update(|amend| *amend = !*amend))
            },
            on_toggle_git_push_after: {
                let service = self.clone();
                Rc::new(move || service.git_push_after.update(|push| *push = !*push))
            },
            on_git_push: {
                let service = self.clone();
                Rc::new(move || service.git_remote(GitRemoteActionKind::Push))
            },
            on_git_pull: {
                let service = self.clone();
                Rc::new(move || service.git_remote(GitRemoteActionKind::Pull))
            },
            on_git_fetch: {
                let service = self.clone();
                Rc::new(move || service.git_remote(GitRemoteActionKind::Fetch))
            },
            git_history: self.git_history,
            git_show_history: self.git_show_history,
            git_commit_detail: self.git_commit_detail,
            attachments: self.attachments,
            session_actions: self.session_actions,
            session_rename: self.session_rename,
            session_busy: self.session_busy,
            auto_continue_paused: self.auto_continue_paused,
            scheduled_sends: self.scheduled_sends,
            on_resume_auto_continue: {
                let service = self.clone();
                Rc::new(move |id: &str| service.resume_auto_continue(id))
            },
            on_cancel_scheduled_send: {
                let service = self.clone();
                Rc::new(move |id: &str, message: u64| service.cancel_scheduled_send(id, message))
            },
            on_reschedule_scheduled_send: {
                let service = self.clone();
                Rc::new(move |id: &str, message: u64, seconds: u64| {
                    service.reschedule_scheduled_send(id, message, seconds)
                })
            },
            on_set_folder_auto_archive: {
                let service = self.clone();
                Rc::new(move |id: &str, days: Option<u8>| service.set_folder_auto_archive(id, days))
            },
            on_toggle_project_new_session_location: {
                let service = self.clone();
                Rc::new(move |id: &str| service.toggle_project_new_session_location(id))
            },
            on_remove_project: {
                let service = self.clone();
                Rc::new(move |id: &str| service.remove_project(id))
            },
            new_session_open: self.new_session_open,
            new_workspaces: self.new_workspaces,
            new_runtimes: self.new_runtimes,
            new_workspace: self.new_workspace,
            new_runtime: self.new_runtime,
            new_worktree_branch: self.new_worktree_branch,
            new_base_ref: self.new_base_ref,
            new_title: self.new_title,
            new_prompt: self.new_prompt,
            new_attachments: self.new_attachments,
            on_attach_new_image: {
                let service = self.clone();
                Rc::new(move || service.attach_new_images())
            },
            on_attach_new_file: {
                let service = self.clone();
                Rc::new(move || service.attach_new_files())
            },
            on_remove_new_attachment: {
                let service = self.clone();
                Rc::new(move |id: &str| {
                    service
                        .new_attachments
                        .update(|items| items.retain(|item| item.id != id));
                })
            },
            new_busy: self.new_busy,
            new_error: self.new_error,
            resolving: self.resolving,
            on_select: {
                let service = self.clone();
                Rc::new(move |id: &str| service.open(id))
            },
            on_send: {
                let service = self.clone();
                Rc::new(move |text: &str| service.send(text))
            },
            on_refresh: {
                let service = self.clone();
                Rc::new(move || {
                    if let Some(id) = service
                        .active
                        .get_untracked()
                        .and_then(|id| VibexSessionId::parse(id).ok())
                    {
                        service.reload(&id);
                    }
                })
            },
            on_disconnect: self.on_disconnect.clone(),
            on_open_settings: self.on_open_settings.clone(),
            runtime_summary: self.runtime_summary,
            runtime_options: self.runtime_option_views,
            runtime_reasoning: self.runtime_reasoning,
            runtime_modes: self.runtime_modes,
            runtime_features: self.runtime_features,
            runtime_open: self.runtime_open,
            runtime_busy: self.runtime_busy,
            token_usage: self.token_usage,
            needs_continue: self.needs_continue,
            on_continue_turn: {
                let service = self.clone();
                Rc::new(move || service.continue_turn())
            },
            team_open: self.team_open,
            team_tasks: self.team_tasks,
            team_busy: self.team_busy,
            team_error: self.team_error,
            on_open_team: {
                let service = self.clone();
                Rc::new(move || service.open_team())
            },
            on_close_team: {
                let service = self.clone();
                Rc::new(move || service.team_open.set(false))
            },
            on_refresh_team: {
                let service = self.clone();
                Rc::new(move || service.refresh_team())
            },
            on_accept_team: {
                let service = self.clone();
                Rc::new(move |id: &str| service.control_team(id, TeamTaskAction::Accept))
            },
            on_cancel_team: {
                let service = self.clone();
                Rc::new(move |id: &str| {
                    service.control_team(id, TeamTaskAction::Cancel { cascade: true })
                })
            },
            on_interrupt_team: {
                let service = self.clone();
                Rc::new(move |id: &str| service.control_team(id, TeamTaskAction::Interrupt))
            },
            attachment_preview: self.attachment_preview,
            on_open_attachment: {
                let service = self.clone();
                Rc::new(move |item: &AttachmentItem| {
                    service.attachment_preview.set(Some(item.clone()))
                })
            },
            on_close_attachment: {
                let service = self.clone();
                Rc::new(move || service.attachment_preview.set(None))
            },
            show_token_usage: self.show_token_usage,
            show_plan_goal: self.show_plan_goal,
            show_generation_status: self.show_generation_status,
            show_reasoning: self.show_reasoning,
            auto_continue: self.auto_continue,
            on_toggle_auto_continue: {
                let service = self.clone();
                Rc::new(move || {
                    let next = !service.auto_continue.get_untracked();
                    service.auto_continue.set(next);
                })
            },
            find_open: self.find_open,
            on_toggle_find: {
                let service = self.clone();
                Rc::new(move || service.find_open.update(|open| *open = !*open))
            },
            plan: self.plan,
            goal: self.goal,
            history: self.history,
            history_open: self.history_open,
            queued: self.queued,
            queue_paused: self.queue_paused,
            queue_manual: self.queue_manual,
            on_toggle_queue_manual: {
                let service = self.clone();
                Rc::new(move || service.toggle_queue_manual())
            },
            editable_sequence: self.editable_sequence,
            editing: self.editing,
            on_edit_message: {
                let service = self.clone();
                Rc::new(move |sequence: i64, text: &str| service.edit_message(sequence, text))
            },
            on_cancel_edit: {
                let service = self.clone();
                Rc::new(move || service.cancel_edit())
            },
            on_replace_message: {
                let service = self.clone();
                Rc::new(move |text: &str| service.replace_message(text))
            },
            on_send_queued: {
                let service = self.clone();
                Rc::new(move |id: u64| service.send_queued(id))
            },
            on_remove_queued: {
                let service = self.clone();
                Rc::new(move |id: u64| service.remove_queued(id))
            },
            on_move_queued: {
                let service = self.clone();
                Rc::new(move |id: u64, delta: isize| service.move_queued(id, delta))
            },
            schedule_open: self.schedule_open,
            scheduled: self.scheduled,
            on_open_schedule: {
                let service = self.clone();
                Rc::new(move || service.schedule_open.set(true))
            },
            on_close_schedule: {
                let service = self.clone();
                Rc::new(move || service.schedule_open.set(false))
            },
            on_schedule: {
                let service = self.clone();
                Rc::new(move |seconds: u64| service.schedule_message(seconds))
            },
            on_cancel_schedule: {
                let service = self.clone();
                Rc::new(move || service.cancel_scheduled())
            },
            on_clear_queue: {
                let service = self.clone();
                Rc::new(move || service.clear_queue())
            },
            on_toggle_queue_paused: {
                let service = self.clone();
                Rc::new(move || service.toggle_queue_paused())
            },
            on_open_history: {
                let service = self.clone();
                Rc::new(move || service.open_history())
            },
            on_close_history: {
                let service = self.clone();
                Rc::new(move || service.history_open.set(false))
            },
            on_recall_history: {
                let service = self.clone();
                Rc::new(move |text: &str| {
                    service.draft.update(|draft| {
                        if draft.trim().is_empty() {
                            draft.clear();
                        }
                        draft.push_str(text);
                    });
                    service.history_open.set(false);
                })
            },
            on_goal_control: {
                let service = self.clone();
                Rc::new(move |control: GoalControl| service.control_goal(control))
            },
            on_open_runtime: {
                let service = self.clone();
                Rc::new(move || service.open_runtime())
            },
            on_close_runtime: {
                let service = self.clone();
                Rc::new(move || service.runtime_open.set(false))
            },
            on_choose_runtime: {
                let service = self.clone();
                Rc::new(move |id: &str| service.choose_runtime(id))
            },
            on_choose_reasoning: {
                let service = self.clone();
                Rc::new(move |value: &str| service.choose_reasoning(value))
            },
            on_choose_mode: {
                let service = self.clone();
                Rc::new(move |value: &str| service.choose_mode(value))
            },
            on_choose_feature: {
                let service = self.clone();
                Rc::new(move |id: &str, value: Option<&str>| service.choose_feature(id, value))
            },
            on_toggle_turn: {
                let service = self.clone();
                Rc::new(move |turn_id: &str| service.toggle_turn(turn_id))
            },
            on_reveal_row: {
                let service = self.clone();
                Rc::new(move |row_id: &str, detail| service.reveal_row(row_id, detail))
            },
            on_toggle_detail: {
                let service = self.clone();
                Rc::new(move |row_id: &str, sequence: i64| {
                    service.toggle_row_detail(row_id, sequence)
                })
            },
            on_load_older: {
                let service = self.clone();
                Rc::new(move || service.load_older())
            },
            on_fork: {
                let service = self.clone();
                Rc::new(move |sequence: i64| service.fork_at(sequence))
            },
            on_permission: {
                let service = self.clone();
                Rc::new(move |request_id: &str, choice: PermissionChoice| {
                    service.resolve_permission(request_id, choice)
                })
            },
            on_question: {
                let service = self.clone();
                Rc::new(move |request_id: &str, response: QuestionResponse| {
                    service.resolve_question(request_id, response)
                })
            },
            on_command_query: {
                let service = self.clone();
                Rc::new(move |query: &str| service.query_commands(query))
            },
            on_mention_query: {
                let service = self.clone();
                Rc::new(move |query: &str| service.query_mentions(query))
            },
            on_attach_image: {
                let service = self.clone();
                Rc::new(move || service.attach_images())
            },
            on_attach_camera: {
                let service = self.clone();
                Rc::new(move || service.attach_camera())
            },
            on_attach_file: {
                let service = self.clone();
                Rc::new(move || service.attach_files())
            },
            on_remove_attachment: {
                let service = self.clone();
                Rc::new(move |id: &str| {
                    service
                        .attachments
                        .update(|items| items.retain(|item| item.id != id));
                })
            },
            on_new_session: {
                let service = self.clone();
                Rc::new(move || service.open_new_session())
            },
            on_new_project: {
                let service = self.clone();
                Rc::new(move || {
                    service.project_browser_open.set(true);
                    service.browse_project_dirs(None);
                })
            },
            on_create_session: {
                let service = self.clone();
                Rc::new(move || service.create_new_session())
            },
            on_create_temporary_session: {
                let service = self.clone();
                Rc::new(move || service.create_temporary_session())
            },
            project_browser: ProjectBrowserHost {
                open: self.project_browser_open,
                dirs: self.project_dirs,
                current: self.project_dir_path,
                parent: self.project_dir_parent,
                busy: self.project_busy,
                error: self.project_error,
                on_open: {
                    let service = self.clone();
                    Rc::new(move |path| service.open_project(path))
                },
                on_browse: {
                    let service = self.clone();
                    Rc::new(move |path| service.browse_project_dirs(path.map(str::to_owned)))
                },
            },
            on_session_menu: {
                let service = self.clone();
                Rc::new(move |id: &str, title: &str| {
                    service.session_rename.set(title.to_owned());
                    service.session_actions.set(Some(id.to_owned()));
                })
            },
            on_pin_sessions: {
                let service = self.clone();
                Rc::new(move |ids| {
                    let pinned = service.pinned.get_untracked();
                    let target = !ids.iter().all(|id| pinned.contains(id));
                    for id in ids.iter().filter(|id| pinned.contains(id) != target) {
                        service.toggle_pin(id);
                    }
                })
            },
            on_fork_sessions: {
                let service = self.clone();
                Rc::new(move |ids| service.fork_sessions(ids))
            },
            on_archive_sessions: {
                let service = self.clone();
                Rc::new(move |ids: &[String]| service.archive_sessions(ids))
            },
            on_delete_sessions: {
                let service = self.clone();
                Rc::new(move |ids: &[String]| service.delete_sessions(ids))
            },
            on_close_session_actions: {
                let service = self.clone();
                Rc::new(move || service.session_actions.set(None))
            },
            on_rename_session: {
                let service = self.clone();
                Rc::new(move || service.rename_open_session())
            },
            on_archive_session: {
                let service = self.clone();
                Rc::new(move || service.archive_open_session())
            },
            on_delete_session: {
                let service = self.clone();
                Rc::new(move || service.delete_open_session())
            },
            on_close_new_session: {
                let service = self.clone();
                Rc::new(move || service.new_session_open.set(false))
            },
            on_create_worktree_session: {
                let service = self.clone();
                Rc::new(move || service.create_worktree_session())
            },
            on_open_terminal: {
                let service = self.clone();
                Rc::new(move || service.open_terminal())
            },
            on_terminal_data: {
                let service = self.clone();
                Rc::new(move |data: &str| service.terminal_input(data))
            },
            on_terminal_resize: {
                let service = self.clone();
                Rc::new(move |cols: u32, rows: u32| service.terminal_resize(cols, rows))
            },
            on_switch_runtime: self.on_switch_runtime.clone(),
            on_interrupt: {
                let service = self.clone();
                Rc::new(move || service.interrupt_active())
            },
            on_retry_terminal: {
                let service = self.clone();
                Rc::new(move || service.retry_terminal())
            },
            on_kill_terminal: {
                let service = self.clone();
                Rc::new(move |id| service.kill_terminal(id))
            },
            on_select_terminal: {
                let service = self.clone();
                Rc::new(move |id: &str| service.select_terminal(id))
            },
            on_new_terminal: {
                let service = self.clone();
                Rc::new(move || service.new_terminal())
            },
            on_terminal_title: {
                let service = self.clone();
                Rc::new(move |title: &str| {
                    if !title.trim().is_empty() {
                        service.terminal_title.set(Some(title.to_owned().into()));
                    }
                })
            },
            on_open_files: {
                let service = self.clone();
                Rc::new(move || service.open_files())
            },
            on_refresh_files: {
                let service = self.clone();
                Rc::new(move || service.load_tree(String::new()))
            },
            on_collapse_files: {
                let service = self.clone();
                Rc::new(move || {
                    service
                        .file_tree
                        .borrow_mut()
                        .restore_navigation_state([], None);
                    service.publish_file_tree();
                })
            },
            on_open_dir: {
                let service = self.clone();
                Rc::new(move |path: &str| service.toggle_directory(path))
            },
            on_open_file: {
                let service = self.clone();
                Rc::new(move |path: &str, line| service.open_file(path.to_owned(), line))
            },
            on_files_back: {
                let service = self.clone();
                Rc::new(move || service.files_back())
            },
            on_open_git: {
                let service = self.clone();
                Rc::new(move || service.open_git())
            },
            on_open_git_diff: {
                let service = self.clone();
                Rc::new(move |path: &str, staged: bool| service.open_git_diff(path, staged))
            },
            on_stage_git: {
                let service = self.clone();
                Rc::new(move |path: &str| service.stage_path(path, true))
            },
            on_unstage_git: {
                let service = self.clone();
                Rc::new(move |path: &str| service.stage_path(path, false))
            },
            on_commit_git: {
                let service = self.clone();
                Rc::new(move || service.commit_changes())
            },
            on_toggle_git_history: {
                let service = self.clone();
                Rc::new(move || service.toggle_git_history())
            },
            on_open_git_commit: {
                let service = self.clone();
                Rc::new(move |hash: &str| service.open_git_commit(hash))
            },
            on_revert_git: {
                let service = self.clone();
                Rc::new(move |path: &str| service.revert_paths(vec![path.to_owned()]))
            },
            on_git_back: {
                let service = self.clone();
                Rc::new(move || service.git_back())
            },
            on_refresh_git: {
                let service = self.clone();
                Rc::new(move || service.refresh_git())
            },
        }
    }

    /// Reloads the session list without disturbing the live stream.
    fn refresh(&self) {
        let service = self.clone();
        spawn(async move {
            service.reload_sessions().await;
        });
    }

    /// Opens the runtime's event stream and keeps it open.
    ///
    /// A phone that loses the desktop has to find it again by itself: the loop
    /// re-subscribes with a capped backoff, refetches the session list on every
    /// attempt, and leaves the connecting page (or the banner) up while it
    /// does. The service's own tasks stop with it when the runtime is replaced.
    fn start_connection(&self) {
        let service = self.clone();
        spawn(async move {
            service.connection_loop().await;
        });
    }

    async fn connection_loop(&self) {
        let mut delay = RECONNECT_MIN_MS;
        while self.alive.get() {
            if self.retry_now.replace(false) {
                delay = RECONNECT_MIN_MS;
            }
            self.connection.set(ConnectionState::Connecting);
            // The transport's operations reuse a selected route; only
            // `connect` opens one. Without it the first session list asks for a
            // route that was never selected and comes back
            // "remote_reselection_canceled".
            let web = self.web.clone();
            let connected = crate::runtime::handle()
                .spawn(async move { web.connect().await })
                .await;
            if let Ok(Err(error)) = &connected {
                log::warn!("session connect failed: {} ({})", error.code, error.message);
            }
            if let Ok(Ok(info)) = &connected {
                if info.server_kind != RemoteServerKind::Unknown {
                    (self.on_server_kind)(info.server_kind);
                }
                let name = info.server_display_name.trim();
                if !name.is_empty() {
                    self.runtime.set(Some(name.to_owned().into()));
                }
            }
            let failed = match &connected {
                Ok(Ok(_)) => false,
                Ok(Err(error)) => {
                    self.connection.set(ConnectionState::Offline);
                    self.connection_error.set(Some(error_text(error)));
                    true
                }
                Err(error) => {
                    self.connection.set(ConnectionState::Offline);
                    self.connection_error.set(Some(
                        format!("connecting to the runtime did not finish: {error}").into(),
                    ));
                    true
                }
            };
            if failed {
                delay = (delay * 2).min(RECONNECT_MAX_MS);
                self.wait_before_retry(delay).await;
                continue;
            }
            let subscription = match self.backend.subscribe() {
                Ok(subscription) => subscription,
                Err(error) => {
                    self.connection.set(ConnectionState::Offline);
                    self.connection_error.set(Some(error_text(&error)));
                    delay = (delay * 2).min(RECONNECT_MAX_MS);
                    self.wait_before_retry(delay).await;
                    continue;
                }
            };
            let listed = self.reload_sessions().await;
            if listed {
                self.connection.set(ConnectionState::Online);
                self.connection_error.set(None);
            }
            let mut got_event = false;
            let mut subscription = subscription;
            while self.alive.get() {
                match subscription.next().await {
                    Ok(Some(event)) => {
                        got_event = true;
                        let lost = matches!(event, BackendEvent::Disconnected);
                        self.apply_event(event);
                        if lost {
                            break;
                        }
                    }
                    _ => break,
                }
            }
            if !self.alive.get() {
                break;
            }
            self.connection.set(ConnectionState::Connecting);
            // A stream that carried events gets a fresh, short delay; one that
            // dropped at once backs off, as an unreachable runtime does.
            delay = if got_event {
                RECONNECT_MIN_MS
            } else {
                (delay * 2).min(RECONNECT_MAX_MS)
            };
            self.wait_before_retry(delay).await;
        }
    }

    /// Sleeps `delay` in short slices so a retry the page asked for is heard.
    async fn wait_before_retry(&self, delay: u64) {
        let mut waited = 0;
        while waited < delay && self.alive.get() {
            timer(Duration::from_millis(500)).await;
            waited += 500;
            if self.retry_now.get() {
                break;
            }
        }
    }

    /// Lists the runtime's sessions and keeps the open one if it still exists.
    async fn reload_sessions(&self) -> bool {
        let backend = self.backend.clone();
        let workspaces = self.workspace_backend.clone();
        let result = crate::runtime::handle()
            .spawn(async move { backend.list_sessions(false).await })
            .await;
        match result {
            Ok(Ok(sessions)) => {
                let spaces = crate::runtime::handle()
                    .spawn(async move { workspaces.list_workspaces().await })
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                self.workspace_records.set(spaces.clone());
                let _ = self.reload_sidebar().await;
                let rows = sessions
                    .iter()
                    .map(|session| summary(session, &spaces))
                    .collect::<Vec<_>>();
                let active_exists = self.active.get_untracked().is_some_and(|active| {
                    sessions.iter().any(|session| session.id.as_str() == active)
                });
                self.sessions.set(rows);
                *self.session_states.borrow_mut() = sessions
                    .iter()
                    .map(|session| (session.id.as_str().to_owned(), session.state))
                    .collect();
                self.session_records.set(sessions.clone());
                if !active_exists {
                    if let Some(first) = sessions.first() {
                        self.open(first.id.as_str());
                    } else {
                        self.active.set(None);
                        self.timeline.set(Vec::new());
                        *self.model.borrow_mut() = TimelineModel::default();
                    }
                }
                self.connection.set(ConnectionState::Online);
                self.connection_error.set(None);
                true
            }
            Ok(Err(error)) => {
                log::warn!("session connect failed: {} ({})", error.code, error.message);
                self.connection.set(ConnectionState::Offline);
                self.connection_error.set(Some(error_text(&error)));
                false
            }
            Err(error) => {
                self.connection.set(ConnectionState::Offline);
                self.connection_error.set(Some(
                    format!("listing the sessions did not finish: {error}").into(),
                ));
                false
            }
        }
    }

    /// Keeps the per-session draft map on the device.
    fn start_draft_saver(&self) {
        let service = self.clone();
        spawn(async move {
            loop {
                timer(Duration::from_secs(1)).await;
                if !service.alive.get() {
                    break;
                }
                service.stash_draft();
                service.persist_drafts();
            }
        });
    }

    /// Holds the display awake while a turn runs or the terminal is open, when
    /// the reader asked for it.
    fn start_screen_policy(&self) {
        let service = self.clone();
        spawn(async move {
            loop {
                timer(Duration::from_millis(500)).await;
                if !service.alive.get() {
                    break;
                }
                let running = service.sessions.with(|sessions| {
                    sessions
                        .iter()
                        .any(|session| session.state == SessionState::Running)
                });
                let awake = crate::settings::keep_screen_on()
                    && (running || service.terminal_visible.get_untracked());
                crate::host::set_keep_screen_on(awake);
            }
        });
    }

    /// Presents a notification and keeps its session for a later tap.
    fn present_notification(&self, intent: vibex_core::AgentNotificationIntent) {
        let kind = match intent.kind {
            AgentNotificationKind::TurnCompleted => crate::settings::NotificationKind::Completed,
            AgentNotificationKind::ApprovalRequired { .. }
            | AgentNotificationKind::InputRequired { .. } => {
                crate::settings::NotificationKind::NeedsInput
            }
            AgentNotificationKind::TurnFailed => crate::settings::NotificationKind::Failed,
        };
        if !crate::settings::notification_allowed(kind) {
            return;
        }
        let background = nsh_core::element::lifecycle() != Lifecycle::Active;
        let active = self.active.get_untracked().as_deref() == Some(intent.session_id.as_str());
        if active && !background {
            return;
        }
        if intent.expires_at_ms <= unix_timestamp_ms() {
            return;
        }
        let body = match intent.kind {
            AgentNotificationKind::ApprovalRequired { .. } => copy::text(
                "An Agent operation is waiting for approval",
                "有一项 Agent 操作等待批准",
                "有一項 Agent 操作等待批准",
            ),
            AgentNotificationKind::InputRequired { .. } => copy::text(
                "An Agent is waiting for your input",
                "Agent 正在等待你的输入",
                "Agent 正在等待你的輸入",
            ),
            AgentNotificationKind::TurnFailed => copy::text(
                "An Agent turn failed",
                "Agent 回合执行失败",
                "Agent 回合執行失敗",
            ),
            AgentNotificationKind::TurnCompleted => copy::text(
                "An Agent completed its work",
                "Agent 已完成工作",
                "Agent 已完成工作",
            ),
        };
        let id = intent.notification_id.clone();
        self.notification_sessions
            .borrow_mut()
            .insert(id.clone(), intent.session_id.as_str().to_owned());
        spawn(async move {
            let _ = request_notification_permission().await;
            let _ = show_notification(nsh_core::notifications::Notification::new(
                id, "Vibex", body,
            ))
            .await;
        });
    }

    /// Opens the session a notification tap names.
    fn start_notification_presses(&self) {
        self.notification_generation
            .set(self.notification_generation.get().wrapping_add(1));
        let generation = self.notification_generation.get();
        let service = self.clone();
        spawn(async move {
            loop {
                let id = next_notification_press().await;
                if !service.alive.get() || service.notification_generation.get() != generation {
                    break;
                }
                // A platform without notifications answers at once with no
                // id; do not spin on it.
                if id.is_empty() {
                    timer(Duration::from_secs(1)).await;
                    continue;
                }
                let session = service.notification_sessions.borrow_mut().remove(&id);
                if let Some(session) = session {
                    service.open(&session);
                }
            }
        });
    }

    /// Moves the open composer's text into the draft map.
    fn stash_draft(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let text = self.draft.get_untracked();
        let mut drafts = self.drafts.borrow_mut();
        if text.is_empty() {
            drafts.remove(&active);
        } else {
            drafts.insert(active, text);
        }
    }

    fn persist_drafts(&self) {
        let Ok(text) = serde_json::to_string(&*self.drafts.borrow()) else {
            return;
        };
        if *self.drafts_snapshot.borrow() == text {
            return;
        }
        if Storage::set_item(DRAFTS_KEY, text.clone()).is_ok() {
            *self.drafts_snapshot.borrow_mut() = text;
        }
    }

    /// Applies one backend event to what the shell shows.
    fn apply_event(&self, event: BackendEvent) {
        match event {
            BackendEvent::Timeline(live) => {
                self.note_online();
                if self.active.get_untracked().as_deref() == Some(live.session_id.as_str()) {
                    let changed = self.model.borrow_mut().apply_live(live);
                    if changed {
                        self.publish();
                    }
                }
            }
            BackendEvent::SessionUpdated(session) => {
                self.note_online();
                self.session_states
                    .borrow_mut()
                    .insert(session.id.as_str().to_owned(), session.state);
                self.record_sidebar_session(&session);
                let row = self
                    .workspace_records
                    .with(|workspaces| summary(&session, workspaces));
                self.sessions.update(|sessions| {
                    if let Some(existing) =
                        sessions.iter_mut().find(|existing| existing.id == row.id)
                    {
                        *existing = row;
                    } else {
                        sessions.insert(0, row);
                    }
                });
                if self.active.get_untracked().as_deref() == Some(session.id.as_str())
                    && let Ok(session_id) = VibexSessionId::parse(session.id.as_str())
                {
                    if session.is_terminal()
                        && self.terminal_visible.get_untracked()
                        && let Some(id) = session
                            .terminal
                            .as_ref()
                            .and_then(|facts| facts.terminal_id.clone())
                        && self.terminal_id.get_untracked().as_deref() != Some(id.as_str())
                    {
                        self.attach_terminal(id, session.title.clone());
                    }
                    self.refresh_token_usage(&session_id);
                    if !matches!(
                        session.state,
                        AgentSessionState::Running | AgentSessionState::Initializing
                    ) {
                        self.flush_queue();
                    }
                }
            }
            BackendEvent::SessionsRemoved(removed) => {
                self.note_online();
                let ids = removed
                    .iter()
                    .map(|id| id.as_str().to_owned())
                    .collect::<Vec<_>>();
                self.sessions
                    .update(|sessions| sessions.retain(|session| !ids.contains(&session.id)));
                self.session_records.update(|sessions| {
                    sessions.retain(|session| !removed.contains(&session.id));
                });
                self.session_states
                    .borrow_mut()
                    .retain(|id, _| !ids.contains(id));
                self.drafts.borrow_mut().retain(|id, _| !ids.contains(id));
                self.attachment_drafts
                    .borrow_mut()
                    .retain(|id, _| !ids.contains(id));
                self.persist_drafts();
                let active = self.active.get_untracked();
                if active.as_ref().is_some_and(|active| ids.contains(active)) {
                    self.active.set(None);
                    self.timeline.set(Vec::new());
                    *self.model.borrow_mut() = TimelineModel::default();
                }
            }
            // A gap in the stream is not a reason to trust what is on screen:
            // the projection has moved on without this phone, so both the list
            // and the open transcript are read again.
            BackendEvent::Lagged { refetch, .. } => {
                self.connection.set(ConnectionState::Connecting);
                if let Some(session_id) = refetch.session_id.clone() {
                    self.reload(&session_id);
                } else if let Some(active) = self.active.get_untracked()
                    && let Ok(session_id) = VibexSessionId::parse(active)
                {
                    self.reload(&session_id);
                }
                self.refresh();
            }
            BackendEvent::ProjectionInvalidated(projection) => match projection {
                BackendProjection::Files => {
                    if self.files_open.get_untracked() {
                        self.load_tree(String::new());
                    }
                }
                BackendProjection::Git => {
                    if self.git_open.get_untracked() {
                        self.refresh_git();
                    }
                }
                // The sidebar, management and usage projections are all
                // answered by the session list on this phone.
                _ => self.refresh(),
            },
            BackendEvent::RuntimeRenamed(renamed) => {
                let name = renamed.display_name.clone();
                self.runtime.set(Some(name.clone().into()));
                (self.on_runtime_renamed)(&name);
            }
            BackendEvent::DeviceRenamed(renamed) => {
                (self.on_device_renamed)(&renamed.display_name);
            }
            BackendEvent::Notification(intent) => {
                self.note_online();
                self.present_notification(intent);
            }
            BackendEvent::Disconnected => {
                self.connection.set(ConnectionState::Connecting);
            }
            _ => {}
        }
    }

    fn note_online(&self) {
        if self.connection.get_untracked() != ConnectionState::Online {
            self.connection.set(ConnectionState::Online);
            self.connection_error.set(None);
        }
    }

    /// Opens `id`: its transcript replaces what is shown.
    fn open(&self, id: &str) {
        let Ok(session_id) = VibexSessionId::parse(id) else {
            self.fail(&BackendError::failed(
                "session_id_invalid",
                "the session id is not a Vibex session id",
            ));
            return;
        };
        self.mark_read(id);
        // Each session owns its draft; switching saves the one being left
        // behind and brings back what the new one had.
        let previous = self.active.get_untracked();
        if previous.as_deref() != Some(session_id.as_str()) {
            self.stash_draft();
            self.attachments.set(swap_attachment_draft(
                &mut self.attachment_drafts.borrow_mut(),
                previous.as_deref(),
                id,
                self.attachments.get_untracked(),
            ));
            let stored = self
                .drafts
                .borrow()
                .get(session_id.as_str())
                .cloned()
                .unwrap_or_default();
            self.draft.set(stored);
        }
        // Files and Git belong to the session's workspace; a session in
        // another worktree must not keep the previous one's paths or open
        // surfaces.
        self.workbench_generation
            .set(self.workbench_generation.get().wrapping_add(1));
        *self.files_workspace.borrow_mut() = None;
        self.files_open.set(false);
        self.file_tree.replace(FileTreeProjection::default());
        self.file_search_run
            .set(self.file_search_run.get().wrapping_add(1));
        self.files_search.set(Vec::new());
        self.files_searching.set(false);
        self.files_entries.set(Vec::new());
        self.files_back();
        self.git_open.set(false);
        self.git_model.replace(GitWorkbenchState::default());
        self.git_changes.set(Vec::new());
        self.git_branch.set(None);
        self.git_branches.set(Vec::new());
        self.git_diff.set(None);
        self.git_error.set(None);
        self.git_selected.set(Vec::new());
        self.git_remote_busy.set(false);
        self.git_totals.set((0, 0));
        self.git_branch_menu.set(false);
        self.git_history.set(Vec::new());
        self.git_message.set(String::new());
        self.git_committing.set(false);
        self.git_commit_detail.set(None);
        self.git_show_history.set(false);
        self.close_terminal();
        self.terminal_choices.set(Vec::new());
        self.active.set(Some(session_id.as_str().to_owned()));
        *self.model.borrow_mut() = TimelineModel::default();
        self.turn_disclosures.borrow_mut().clear();
        self.expanded_rows.borrow_mut().clear();
        self.row_details.borrow_mut().clear();
        self.loading_row_details.borrow_mut().clear();
        self.conversation.set(ConversationState::Loading);
        self.timeline.set(Vec::new());
        self.has_older.set(false);
        self.loading_older.set(false);
        self.pending.set(None);
        self.question.set(None);
        *self.question_request.borrow_mut() = None;
        self.commands.set(Vec::new());
        self.commands_loaded.set(false);
        self.mentions.set(Vec::new());
        self.mentions_loaded.set(false);
        self.skills.set(Vec::new());
        self.skills_loaded.set(false);
        self.armed_command.borrow_mut().take();
        self.armed_mentions.borrow_mut().clear();
        self.error.set(None);
        self.runtime_state.replace(None);
        self.runtime_catalog.set(None);
        self.runtime_summary.set(None);
        self.runtime_option_views.set(Vec::new());
        self.runtime_reasoning.set(Vec::new());
        self.runtime_modes.set(Vec::new());
        self.runtime_features.set(Vec::new());
        self.runtime_open.set(false);
        self.runtime_busy.set(false);
        self.token_usage.set(None);
        self.editable_sequence.set(0);
        self.editing.set(false);
        *self.edit_state.borrow_mut() = None;
        self.sync_queue();
        self.load_cached_timeline(&session_id);
        self.refresh_runtime(&session_id);
        self.refresh_token_usage(&session_id);
        self.reload(&session_id);
    }

    /// Puts `text` in the composer as a replacement for `sequence`.
    fn edit_message(&self, sequence: i64, text: &str) {
        let expected = self.model.borrow().authoritative_end_sequence.unwrap_or(0);
        *self.edit_state.borrow_mut() = Some(EditState {
            user_sequence: sequence,
            expected_end_sequence: expected,
        });
        self.editing.set(true);
        self.draft.set(text.to_owned());
        self.attachments.set(Vec::new());
    }

    fn cancel_edit(&self) {
        *self.edit_state.borrow_mut() = None;
        self.editing.set(false);
        self.draft.set(String::new());
    }

    /// Replaces the message the composer was opened on.
    fn replace_message(&self, text: &str) {
        let Some(edit) = self.edit_state.borrow().clone() else {
            return;
        };
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let text = text.trim().to_owned();
        if text.is_empty() || self.sending.get_untracked() {
            return;
        }
        self.sending.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let selection_backend = backend.clone();
            let selection_id = session_id.clone();
            let selection = crate::runtime::handle()
                .spawn(async move { selection_backend.runtime_selection(selection_id).await })
                .await;
            let selection = match selection {
                Ok(Ok(state)) => state.desired,
                Ok(Err(error)) => {
                    service.sending.set(false);
                    service.fail(&error);
                    return;
                }
                Err(error) => {
                    service.sending.set(false);
                    service.fail(&BackendError::failed(
                        "session_task_failed",
                        format!("reading the runtime did not finish: {error}"),
                    ));
                    return;
                }
            };
            let request_session = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .replace_user_message(MutationRequest::new(
                            vibex_core::ReplaceUserMessagePayload {
                                user_sequence: edit.user_sequence,
                                expected_end_sequence: edit.expected_end_sequence,
                                message: SendAgentMessageRequest {
                                    session_id: request_session.clone(),
                                    message_idempotency_key: RequestId::new().into_string(),
                                    desired_runtime: selection,
                                    text,
                                    attachments: Vec::new(),
                                    mentions: Vec::new(),
                                    reasoning_effort: None,
                                    correlation_id: None,
                                    delivery: UserMessageDelivery::Prompt,
                                    prompt_context: None,
                                    provenance: MessageProvenance::HumanInput,
                                },
                            },
                        ))
                        .await
                })
                .await;
            service.sending.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.edit_state.borrow_mut().take();
                    service.editing.set(false);
                    service.draft.set(String::new());
                    service.reload(&session_id);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("replacing the message did not finish: {error}"),
                )),
            }
        });
    }

    /// Collects the session's earlier prompts and opens the history sheet.
    fn open_history(&self) {
        let model = self.model.borrow();
        let mut history = Vec::new();
        for turn in model.turns().iter().rev() {
            let Some(item) = turn.user_item.as_ref() else {
                continue;
            };
            let TimelinePayload::UserMessage(message) = &item.payload else {
                continue;
            };
            if message.text.trim().is_empty() {
                continue;
            }
            history.push(ComposerHistoryEntry {
                id: item.id.as_str().to_owned(),
                text: message.text.clone(),
                sequence: item.sequence,
            });
            if history.len() >= 50 {
                break;
            }
        }
        drop(model);
        self.history.set(history);
        self.history_open.set(true);
    }

    /// Opens the runtime sheet, loading its choices when they are not held.
    fn open_runtime(&self) {
        self.runtime_open.set(true);
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        if self
            .runtime_catalog
            .with_untracked(|catalog| catalog.is_none())
        {
            self.refresh_runtime(&session_id);
        }
    }

    /// Reads the open session's selection and the runtime's catalogue.
    fn refresh_runtime(&self, session_id: &VibexSessionId) {
        let session_id = session_id.clone();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let requested = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    let state = backend.runtime_selection(requested.clone()).await?;
                    let catalog = backend.list_runtime_options().await?;
                    Ok::<_, BackendError>((state, catalog))
                })
                .await;
            if let Ok(Ok((state, catalog))) = result {
                service.apply_runtime(&session_id, state, catalog);
            }
        });
    }

    fn apply_runtime(
        &self,
        session_id: &VibexSessionId,
        state: AgentSessionRuntimeSelectionState,
        catalog: SessionRuntimeOptionCatalog,
    ) {
        if self.active.get_untracked().as_deref() != Some(session_id.as_str()) {
            return;
        }
        self.runtime_state.replace(Some(state.clone()));
        self.runtime_catalog.set(Some(catalog.clone()));
        self.rebuild_runtime_views(&state.desired, &catalog);
    }

    fn rebuild_runtime_views(
        &self,
        selection: &SessionRuntimeSelection,
        catalog: &SessionRuntimeOptionCatalog,
    ) {
        let matches_model = |option: &SessionRuntimeOption| {
            let candidate = &option.selection;
            candidate.agent_id == selection.agent_id
                && candidate.auth_source == selection.auth_source
                && candidate.model == selection.model
        };
        let option = catalog.options.iter().find(|option| matches_model(option));
        let model = match &selection.model {
            RuntimeModelSelection::Explicit { model_id } => model_id.clone(),
            _ => copy::text("Default", "默认", "預設").to_owned(),
        };
        let reasoning_label = option
            .and_then(|option| {
                selection.reasoning_effort.as_ref().and_then(|value| {
                    option
                        .reasoning_efforts
                        .iter()
                        .find(|candidate| &candidate.value == value)
                        .map(|candidate| {
                            candidate
                                .label
                                .clone()
                                .unwrap_or_else(|| candidate.value.clone())
                        })
                })
            })
            .unwrap_or_else(|| copy::text("Default", "默认", "預設").to_owned());
        let mode_label = option
            .and_then(|option| {
                selection.mode_id.as_ref().and_then(|value| {
                    option
                        .modes
                        .iter()
                        .find(|candidate| &candidate.value == value)
                        .map(|candidate| {
                            candidate
                                .label
                                .clone()
                                .unwrap_or_else(|| candidate.value.clone())
                        })
                })
            })
            .unwrap_or_else(|| copy::text("Default", "默认", "預設").to_owned());
        self.runtime_summary.set(Some(RuntimeSummaryView {
            agent: option
                .map(|option| option.agent_label.clone())
                .unwrap_or_else(|| selection.agent_id.as_str().to_owned()),
            model: option
                .map(|option| option.model_label.clone())
                .unwrap_or(model),
            reasoning: reasoning_label,
            mode: mode_label,
            options_set: !selection.config_values.is_empty(),
            reasoning_set: selection.reasoning_effort.is_some(),
            mode_set: selection.mode_id.is_some(),
            available: option
                .is_some_and(|option| option.availability == RuntimeOptionAvailability::Available),
        }));
        self.runtime_option_views.set(
            catalog
                .options
                .iter()
                .enumerate()
                .map(|(index, option)| RuntimeOptionView {
                    id: index.to_string(),
                    agent_id: option.selection.agent_id.as_str().to_owned(),
                    agent: option.agent_label.clone(),
                    model: option.model_label.clone(),
                    detail: option.auth_source_label.clone(),
                    selected: matches_model(option),
                    available: option.availability == RuntimeOptionAvailability::Available,
                })
                .collect(),
        );
        let reasoning = option
            .filter(|option| !option.reasoning_efforts.is_empty())
            .map(|option| {
                let mut choices = vec![RuntimeChoiceView {
                    value: "default".to_owned(),
                    label: copy::text("Default", "默认", "預設").to_owned(),
                    selected: selection.reasoning_effort.is_none(),
                }];
                choices.extend(option.reasoning_efforts.iter().map(|candidate| {
                    RuntimeChoiceView {
                        value: candidate.value.clone(),
                        label: candidate
                            .label
                            .clone()
                            .unwrap_or_else(|| candidate.value.clone()),
                        selected: selection.reasoning_effort.as_deref()
                            == Some(candidate.value.as_str()),
                    }
                }));
                choices
            })
            .unwrap_or_default();
        self.runtime_reasoning.set(reasoning);
        let modes = option
            .filter(|option| !option.modes.is_empty())
            .map(|option| {
                let mut choices = vec![RuntimeChoiceView {
                    value: "default".to_owned(),
                    label: copy::text("Default", "默认", "預設").to_owned(),
                    selected: selection.mode_id.is_none(),
                }];
                choices.extend(option.modes.iter().map(|candidate| {
                    RuntimeChoiceView {
                        value: candidate.value.clone(),
                        label: candidate
                            .label
                            .clone()
                            .unwrap_or_else(|| candidate.value.clone()),
                        selected: selection.mode_id.as_deref() == Some(candidate.value.as_str()),
                    }
                }));
                choices
            })
            .unwrap_or_default();
        self.runtime_modes.set(modes);
        let features = option
            .map(|option| {
                option
                    .features
                    .iter()
                    .filter(|feature| feature.kind != SessionRuntimeFeatureKind::String)
                    .map(|feature| {
                        let current = selection
                            .config_values
                            .get(&feature.id)
                            .cloned()
                            .or_else(|| {
                                feature
                                    .current_value
                                    .as_ref()
                                    .map(|value| value.value.clone())
                            })
                            .or_else(|| {
                                feature
                                    .default_value
                                    .as_ref()
                                    .map(|value| value.value.clone())
                            });
                        RuntimeFeatureView {
                            id: feature.id.clone(),
                            label: feature.label.clone(),
                            detail: feature.description.clone().unwrap_or_default(),
                            toggle: feature.kind == SessionRuntimeFeatureKind::Toggle,
                            explicit_value: selection.config_values.get(&feature.id).cloned(),
                            values: feature
                                .values
                                .iter()
                                .map(|value| RuntimeChoiceView {
                                    value: value.value.clone(),
                                    label: value
                                        .label
                                        .clone()
                                        .unwrap_or_else(|| value.value.clone()),
                                    selected: current.as_deref() == Some(value.value.as_str()),
                                })
                                .collect(),
                        }
                    })
                    .collect()
            })
            .unwrap_or_default();
        self.runtime_features.set(features);
    }

    fn choose_runtime(&self, id: &str) {
        let Ok(index) = id.parse::<usize>() else {
            return;
        };
        let catalog = self.runtime_catalog.get_untracked();
        let Some(option) = catalog.and_then(|catalog| catalog.options.get(index).cloned()) else {
            return;
        };
        if let Some(current) = self.current_selection()
            && current.agent_id == option.selection.agent_id
            && current.auth_source == option.selection.auth_source
            && current.model == option.selection.model
        {
            return;
        }
        self.apply_selection(option.selection);
    }

    fn choose_reasoning(&self, value: &str) {
        let Some(mut selection) = self.current_selection() else {
            return;
        };
        selection.reasoning_effort = (value != "default").then(|| value.to_owned());
        self.apply_selection(selection);
    }

    fn choose_mode(&self, value: &str) {
        let Some(mut selection) = self.current_selection() else {
            return;
        };
        selection.mode_id = (value != "default").then(|| value.to_owned());
        self.apply_selection(selection);
    }

    fn choose_feature(&self, id: &str, value: Option<&str>) {
        let Some(mut selection) = self.current_selection() else {
            return;
        };
        if let Some(value) = value {
            selection
                .config_values
                .insert(id.to_owned(), value.to_owned());
        } else {
            selection.config_values.remove(id);
        }
        self.apply_selection(selection);
    }

    /// Puts text another app shared into the composer, or starts a session
    /// with it when none is open.
    pub fn receive_shared_text(&self, text: String) {
        if self.active.get_untracked().is_some() {
            self.draft.set(text);
        } else {
            // Opening the page clears what a session before it left behind,
            // so the shared text is written after.
            self.open_new_session();
            self.new_prompt.set(text);
        }
    }

    pub fn set_show_token_usage(&self, enabled: bool) {
        self.show_token_usage.set(enabled);
    }

    pub fn set_show_plan_goal(&self, enabled: bool) {
        self.show_plan_goal.set(enabled);
    }

    pub fn set_show_generation_status(&self, enabled: bool) {
        self.show_generation_status.set(enabled);
        self.publish();
    }

    pub fn set_show_reasoning(&self, enabled: bool) {
        self.show_reasoning.set(enabled);
        self.publish();
    }

    pub fn set_auto_continue(&self, enabled: bool) {
        self.auto_continue.set(enabled);
        if !enabled {
            self.auto_continue_armed.set(false);
        }
        self.publish();
    }

    fn open_team(&self) {
        self.team_open.set(true);
        self.refresh_team();
    }

    fn refresh_team(&self) {
        if self.team_busy.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.team_busy.set(true);
        self.team_error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .team_snapshot(TeamSnapshotRequest::new(session_id))
                        .await
                })
                .await;
            service.team_busy.set(false);
            match result {
                Ok(Ok(snapshot)) => {
                    let mut revisions = service.team_revisions.borrow_mut();
                    revisions.clear();
                    let tasks = snapshot
                        .tasks
                        .into_iter()
                        .map(|task| {
                            let id = task.task_ref.as_uri();
                            revisions.insert(id.clone(), task.revision);
                            TeamTaskView {
                                id,
                                title: task.title,
                                phase: team_phase_label(task.phase).to_owned(),
                                detail: if task.task_summary.trim().is_empty() {
                                    task.error_code.unwrap_or_default()
                                } else {
                                    task.task_summary
                                },
                                can_accept: task.phase == DelegationTaskPhase::AwaitingReview,
                                can_cancel: task.phase.is_active()
                                    && task.phase != DelegationTaskPhase::Cancelling,
                                can_interrupt: task.phase.is_active(),
                            }
                        })
                        .collect();
                    drop(revisions);
                    service.team_tasks.set(tasks);
                }
                Ok(Err(error)) => service.team_error.set(Some(error_text(&error))),
                Err(error) => service.team_error.set(Some(
                    format!("reading the team did not finish: {error}").into(),
                )),
            }
        });
    }

    fn control_team(&self, id: &str, action: TeamTaskAction) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let reference = VibexUseRef::parse(id);
        let Some(task_id) = reference.as_ref().and_then(VibexUseRef::task_id) else {
            return;
        };
        let revision = self.team_revisions.borrow().get(id).copied();
        if self.team_busy.get_untracked() {
            return;
        }
        self.team_busy.set(true);
        self.team_error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .control_team_task(MutationRequest::new(TeamTaskControlRequest {
                            session_id,
                            task_id,
                            action,
                            expected_revision: revision,
                        }))
                        .await
                })
                .await;
            service.team_busy.set(false);
            match result {
                Ok(Ok(_)) => service.refresh_team(),
                Ok(Err(error)) => service.team_error.set(Some(error_text(&error))),
                Err(error) => service.team_error.set(Some(
                    format!("the task action did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Starts the next turn the runtime was waiting to continue.
    fn continue_turn(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .continue_turn(MutationRequest::new(ContinueAgentTurnRequest {
                            session_id: request_session,
                            correlation_id: None,
                        }))
                        .await
                })
                .await;
            match result {
                Ok(Ok(_)) => service.reload(&session_id),
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("continuing the turn did not finish: {error}"),
                )),
            }
        });
    }

    /// Pauses, resumes or clears the session's goal.
    fn control_goal(&self, control: GoalControl) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let action = match control {
            GoalControl::Pause => GoalAction::Pause,
            GoalControl::Resume => GoalAction::Resume,
            GoalControl::Clear => GoalAction::Clear,
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let target = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .control_goal(MutationRequest::new(AgentGoalControlRequest {
                            session_id: target,
                            action,
                            objective: None,
                            expected_revision: None,
                            correlation_id: None,
                        }))
                        .await
                })
                .await;
            match result {
                Ok(Ok(result)) => {
                    if let Some(goal) = result.goal {
                        service.goal.set(goal_view_from_snapshot(&goal));
                    }
                    service.reload(&session_id);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("controlling the goal did not finish: {error}"),
                )),
            }
        });
    }

    fn current_selection(&self) -> Option<SessionRuntimeSelection> {
        self.runtime_state
            .borrow()
            .as_ref()
            .map(|state| state.desired.clone())
    }

    fn apply_selection(&self, desired: SessionRuntimeSelection) {
        if self.runtime_busy.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Some(state) = self.runtime_state.borrow().clone() else {
            return;
        };
        self.runtime_busy.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let requested = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .set_desired_runtime(MutationRequest::new(
                            SetDesiredAgentSessionRuntimeRequest {
                                session_id: requested,
                                idempotency_key: RequestId::new().into_string(),
                                expected_revision: state.session_revision,
                                expected_selection_revision: state.selection_revision,
                                desired,
                                interaction: RuntimeSelectionInteraction::Seamless,
                            },
                        ))
                        .await
                })
                .await;
            if !service.alive.get()
                || service.active.get_untracked().as_deref() != Some(session_id.as_str())
            {
                return;
            }
            service.runtime_busy.set(false);
            match result {
                Ok(Ok(state)) => {
                    service.runtime_state.replace(Some(state.clone()));
                    if let Some(catalog) = service.runtime_catalog.get_untracked() {
                        service.rebuild_runtime_views(&state.desired, &catalog);
                    }
                    service.refresh();
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("changing the runtime did not finish: {error}"),
                )),
            }
        });
    }

    /// Sends `text` to the open session, or queues it while a turn is live.
    fn send(&self, text: &str) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let text = text.trim().to_owned();
        let attachments = self.attachments.get_untracked();
        if (text.is_empty() && attachments.is_empty()) || self.sending.get_untracked() {
            return;
        }
        if let Some(command) = self.armed_command.borrow().clone()
            && parse_slash_command_invocation(&text).is_some_and(|(name, _)| {
                command
                    .command_name
                    .as_deref()
                    .is_none_or(|candidate| candidate == name)
            })
        {
            self.execute_command(command, text);
            return;
        }
        let mentions = self.armed_mentions_for(&text);
        if self.turn_live() {
            self.enqueue(active, text, attachments, mentions);
            return;
        }
        self.deliver(text, attachments, mentions, true);
    }

    fn turn_live(&self) -> bool {
        let Some(active) = self.active.get_untracked() else {
            return false;
        };
        self.session_states
            .borrow()
            .get(&active)
            .is_some_and(|state| {
                matches!(
                    state,
                    AgentSessionState::Running | AgentSessionState::Initializing
                )
            })
    }

    fn armed_mentions_for(&self, text: &str) -> Vec<VibexUseMention> {
        self.armed_mentions
            .borrow()
            .iter()
            .filter(|(insertion, _)| !insertion.is_empty() && text.contains(insertion))
            .map(|(_, mention)| mention.clone())
            .collect()
    }

    /// Runs the `/` command the draft names instead of sending it as prose.
    fn execute_command(&self, entry: AgentCommandEntry, text: String) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Some((name, arguments)) = parse_slash_command_invocation(&text) else {
            return;
        };
        self.sending.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .execute_agent_command(MutationRequest::new(AgentCommandExecuteRequest {
                            session_id: request_session,
                            command_id: Some(entry.id),
                            trigger: AgentCommandTrigger::Slash,
                            source_kind: entry.source_kind,
                            command_text: text,
                            command_name: Some(name),
                            arguments,
                            prompt_id: entry.prompt_id,
                            attachments: Vec::new(),
                            reasoning_effort: None,
                            correlation_id: None,
                        }))
                        .await
                })
                .await;
            service.sending.set(false);
            match result {
                Ok(Ok(result)) => {
                    if result.status == AgentCommandExecuteStatus::Failed {
                        service.fail_text(result.message.as_deref().unwrap_or(copy::text(
                            "The command could not run.",
                            "命令无法执行。",
                            "命令無法執行。",
                        )));
                    } else {
                        service.armed_command.borrow_mut().take();
                        service.draft.set(String::new());
                        service.attachments.set(Vec::new());
                    }
                    service.reload(&session_id);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("running the command did not finish: {error}"),
                )),
            }
        });
    }

    fn schedule_message(&self, seconds: u64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let text = self.draft.get_untracked().trim().to_owned();
        let attachments = self.attachments.get_untracked();
        if text.is_empty() && attachments.is_empty() {
            return;
        }
        let mentions = self.armed_mentions_for(&text);
        let due = unix_timestamp_ms() + (seconds as i64) * 1000;
        self.schedule_open.set(false);
        let service = self.clone();
        let session_id = active.clone();
        spawn(async move {
            let mutation = RemoteSidebarOrganizationMutation::EnqueueScheduledSend {
                session_id: session_id.clone(),
                text: text.clone(),
                attachments: attachments
                    .iter()
                    .map(|attachment| MessageAttachment {
                        label: attachment.label.clone(),
                        mime_type: Some(attachment.mime_type.clone()),
                        uri: Some(attachment.uri.clone()),
                        inline_text_offset: None,
                    })
                    .collect(),
                mentions,
                scheduled_at_ms: due,
            };
            match service.submit_sidebar(mutation, None).await {
                Ok(()) => {
                    if service.active.get_untracked().as_deref() == Some(active.as_str())
                        && service.draft.get_untracked().trim() == text
                        && service.attachments.get_untracked() == attachments
                    {
                        service.draft.set(String::new());
                        service.attachments.set(Vec::new());
                        service.armed_mentions.borrow_mut().clear();
                    }
                }
                Err(error) => service.fail(&error),
            }
        });
    }

    fn cancel_scheduled(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Some(message_id) = *self.scheduled_message_id.borrow() else {
            return;
        };
        self.cancel_scheduled_send(&active, message_id);
    }

    /// Takes the deadline off one queued message, leaving it queued.
    fn cancel_scheduled_send(&self, session_id: &str, message_id: u64) {
        let service = self.clone();
        let session_id = session_id.to_owned();
        spawn(async move {
            let mutation = RemoteSidebarOrganizationMutation::SetScheduledSend {
                session_id,
                message_id,
                scheduled_at_ms: None,
            };
            if let Err(error) = service.submit_sidebar(mutation, None).await {
                service.fail(&error);
            }
        });
    }

    /// Moves one queued message's deadline to `seconds` from now.
    fn reschedule_scheduled_send(&self, session_id: &str, message_id: u64, seconds: u64) {
        let service = self.clone();
        let session_id = session_id.to_owned();
        let due = unix_timestamp_ms() + (seconds as i64) * 1000;
        spawn(async move {
            let mutation = RemoteSidebarOrganizationMutation::SetScheduledSend {
                session_id,
                message_id,
                scheduled_at_ms: Some(due),
            };
            if let Err(error) = service.submit_sidebar(mutation, None).await {
                service.fail(&error);
            }
        });
    }

    fn set_folder_auto_archive(&self, folder_id: &str, days: Option<u8>) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetFolderAutoArchive {
            folder_id: folder_id.to_owned(),
            days,
        });
    }

    fn toggle_project_new_session_location(&self, project_id: &str) {
        let next = match self
            .sidebar_view
            .with(|view| view.project_location_preferences.get(project_id).copied())
        {
            Some(NewSessionLocation::NewWorktree) => {
                RemoteSidebarNewSessionLocation::CurrentCheckout
            }
            _ => RemoteSidebarNewSessionLocation::NewWorktree,
        };
        self.sidebar_mutation(
            RemoteSidebarOrganizationMutation::SetProjectNewSessionLocation {
                project_id: project_id.to_owned(),
                location: next,
            },
        );
    }

    fn resume_auto_continue(&self, id: &str) {
        self.sidebar_mutation(
            RemoteSidebarOrganizationMutation::SetSessionAutoContinuePaused {
                session_ids: vec![id.to_owned()],
                paused: false,
            },
        );
    }

    fn remove_project(&self, id: &str) {
        let Ok(project_id) = ProjectId::parse(id) else {
            return;
        };
        let service = self.clone();
        let backend = self.workspace_backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .delete_project(MutationRequest::new(project_id))
                        .await
                })
                .await;
            match result {
                Ok(Ok(())) => {
                    service.reload_sessions().await;
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "project_remove_task_failed",
                    format!("removing the project did not finish: {error}"),
                )),
            }
        });
    }

    fn enqueue(
        &self,
        session: String,
        text: String,
        attachments: Vec<AttachmentItem>,
        mentions: Vec<VibexUseMention>,
    ) {
        let service = self.clone();
        let sent_text = text.clone();
        spawn(async move {
            let mutation = RemoteSidebarOrganizationMutation::EnqueueQueuedSend {
                session_id: session.clone(),
                text,
                attachments: attachments
                    .iter()
                    .map(|attachment| MessageAttachment {
                        label: attachment.label.clone(),
                        mime_type: Some(attachment.mime_type.clone()),
                        uri: Some(attachment.uri.clone()),
                        inline_text_offset: None,
                    })
                    .collect(),
                mentions,
            };
            match service.submit_sidebar(mutation, None).await {
                Ok(()) => {
                    if service.active.get_untracked().as_deref() == Some(session.as_str())
                        && service.draft.get_untracked().trim() == sent_text
                        && service.attachments.get_untracked() == attachments
                    {
                        service.draft.set(String::new());
                        service.attachments.set(Vec::new());
                        service.armed_mentions.borrow_mut().clear();
                    }
                }
                Err(error) => service.fail(&error),
            }
        });
    }

    fn sync_queue(&self) {
        let Some(active) = self.active.get_untracked() else {
            self.queued.set(Vec::new());
            self.queue_paused.set(false);
            self.scheduled.set(false);
            *self.scheduled_message_id.borrow_mut() = None;
            return;
        };
        let snapshot = self.sidebar_snapshot.borrow();
        self.queued.set(queued_views(&snapshot, &active));
        self.queue_paused
            .set(snapshot.paused_queue_session_ids.contains(&active));
        self.queue_manual
            .set(snapshot.manual_queue_session_ids.contains(&active));
        let schedule = snapshot
            .scheduled_sends
            .iter()
            .find(|send| send.session_id == active);
        self.scheduled.set(schedule.is_some());
        *self.scheduled_message_id.borrow_mut() = schedule.map(|send| send.message_id);
    }

    fn send_queued(&self, id: u64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SendQueuedSendNow {
            session_id: active,
            message_id: id,
        });
    }

    fn move_queued(&self, id: u64, delta: isize) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let queued = self.queued.get_untracked();
        let Some(index) = queued.iter().position(|message| message.id == id) else {
            return;
        };
        let target = index as isize + delta;
        if target < 0 || target >= queued.len() as isize {
            return;
        }
        let before = queued.get(target as usize).map(|message| message.id);
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::MoveQueuedSend {
            session_id: active,
            message_id: id,
            before_message_id: before,
        });
    }

    fn remove_queued(&self, id: u64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::RemoveQueuedSend {
            session_id: active,
            message_id: id,
        });
    }

    fn clear_queue(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        for message in self.queued.get_untracked() {
            self.sidebar_mutation(RemoteSidebarOrganizationMutation::RemoveQueuedSend {
                session_id: active.clone(),
                message_id: message.id,
            });
        }
    }

    fn toggle_queue_manual(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let manual = !self.queue_manual.get_untracked();
        self.queue_manual.set(manual);
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetQueueManual {
            session_id: active,
            manual,
        });
    }

    fn toggle_queue_paused(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let paused = !self.queue_paused.get_untracked();
        self.queue_paused.set(paused);
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetQueuePaused {
            session_id: active,
            paused,
        });
    }

    fn flush_queue(&self) {}

    fn deliver(
        &self,
        text: String,
        attachments: Vec<AttachmentItem>,
        mentions: Vec<VibexUseMention>,
        from_composer: bool,
    ) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.sending.set(true);
        self.error.set(None);
        let sent_text = text.clone();
        let sent_attachment_ids = attachments
            .iter()
            .map(|item| item.id.clone())
            .collect::<Vec<_>>();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let sends = attachments
                .iter()
                .map(|item| MessageAttachment {
                    label: item.label.clone(),
                    mime_type: Some(item.mime_type.clone()),
                    uri: Some(item.uri.clone()),
                    inline_text_offset: None,
                })
                .collect::<Vec<_>>();
            let result = crate::runtime::handle()
                .spawn(send_and_refetch(
                    backend,
                    session_id.clone(),
                    text,
                    sends,
                    mentions,
                ))
                .await;
            service.sending.set(false);
            match result {
                Ok(Ok(page)) => {
                    if from_composer {
                        if service.draft.get_untracked() == sent_text {
                            service.draft.set(String::new());
                        }
                        service.attachments.update(|items| {
                            items.retain(|item| !sent_attachment_ids.contains(&item.id))
                        });
                    }
                    service.armed_command.borrow_mut().take();
                    service.armed_mentions.borrow_mut().clear();
                    service.stash_draft();
                    service.persist_drafts();
                    service.apply_page(&session_id, page);
                    crate::settings::haptic(nsh_core::Haptic::Notification(
                        nsh_core::HapticNotification::Success,
                    ));
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("sending the message did not finish: {error}"),
                )),
            }
        });
    }

    /// Projects a fetched page into the shell's rows.
    fn apply_page(&self, session_id: &VibexSessionId, page: TimelinePage) {
        if self.active.get_untracked().as_deref() != Some(session_id.as_str()) {
            return;
        }
        self.conversation.set(ConversationState::Ready);
        self.has_older.set(page.has_older);
        let has_older = page.has_older;
        self.model
            .borrow_mut()
            .replace_authoritative(session_id.clone(), page.items);
        self.publish();
        self.save_cached_timeline(session_id, has_older);
        self.refresh_token_usage(session_id);
    }

    fn cache_key(&self, session_id: &VibexSessionId) -> String {
        format!("{}:{}", self.host_id(), session_id.as_str())
    }

    fn load_cached_timeline(&self, session_id: &VibexSessionId) {
        let Some(cached) = load_timeline_cache().remove(&self.cache_key(session_id)) else {
            return;
        };
        self.has_older.set(cached.has_older);
        self.model
            .borrow_mut()
            .replace_authoritative(session_id.clone(), cached.items);
        self.publish();
    }

    fn save_cached_timeline(&self, session_id: &VibexSessionId, has_older: bool) {
        if self.active.get_untracked().as_deref() != Some(session_id.as_str()) {
            return;
        }
        let items = self.model.borrow().items.clone();
        if items.is_empty() {
            return;
        }
        let mut cache = load_timeline_cache();
        cache.insert(
            self.cache_key(session_id),
            CachedTimeline {
                saved_at_ms: unix_timestamp_ms(),
                has_older,
                items,
            },
        );
        let keys = cache
            .iter()
            .map(|(key, entry)| (key.clone(), entry.saved_at_ms))
            .collect::<Vec<_>>();
        let mut sorted = keys;
        sorted.sort_by_key(|(_, saved_at)| *saved_at);
        while cache.len() > TIMELINE_CACHE_SESSIONS {
            if let Some((oldest, _)) = sorted.first()
                && let Some(removed) = cache.remove(oldest)
            {
                let _ = removed;
                sorted.remove(0);
            } else {
                break;
            }
        }
        if let Ok(text) = serde_json::to_string(&cache) {
            let _ = Storage::set_item(TIMELINE_CACHE_KEY, text);
        }
    }

    /// Reads the session's live token counters.
    fn refresh_token_usage(&self, session_id: &VibexSessionId) {
        let session_id = session_id.clone();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let requested = session_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move { backend.session_token_usage(requested).await })
                .await;
            if service.active.get_untracked().as_deref() != Some(session_id.as_str()) {
                return;
            }
            match result {
                Ok(Ok(Some(usage))) => service.token_usage.set(Some(token_usage_view(usage))),
                Ok(Ok(None)) => service.token_usage.set(None),
                _ => {}
            }
        });
    }

    /// Reads the page above the transcript and splices it in.
    fn load_older(&self) {
        if self.loading_older.get_untracked() || !self.has_older.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Some(before) = self.model.borrow().items.first().map(|item| item.sequence) else {
            self.has_older.set(false);
            return;
        };
        self.loading_older.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(fetch_older(backend, session_id.clone(), before))
                .await;
            service.loading_older.set(false);
            match result {
                Ok(Ok(page)) => {
                    service.has_older.set(page.has_older);
                    let has_older = page.has_older;
                    service.model.borrow_mut().prepend_authoritative_page(
                        &session_id,
                        &page.items,
                        page.leading_turn_start,
                    );
                    service.publish();
                    service.save_cached_timeline(&session_id, has_older);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("reading older history did not finish: {error}"),
                )),
            }
        });
    }

    /// Re-projects the kept model into the shell's rows.
    fn publish(&self) {
        let active = self.active.get_untracked();
        let state = active
            .as_ref()
            .and_then(|active| self.session_states.borrow().get(active).copied());
        let model = self.model.borrow();
        let turns = model.conversation_turns_with_reasoning_mode(
            state,
            state == Some(AgentSessionState::Running),
            ReasoningDisplayMode::LatestAtBottom,
        );
        let mut entries = Vec::new();
        let show_reasoning = self.show_reasoning.get_untracked();
        let locale = copy::language().locale();
        let disclosures = self.turn_disclosures.borrow();
        for turn in &turns {
            let finished = activity::turn_finished(turn);
            let answered = turn
                .conclusion_row
                .as_ref()
                .is_some_and(|row| !row.body.trim().is_empty());
            if let Some(row) = turn.user_row.as_ref() {
                entries.push(self.entry_from_row(row, turn, &model.items));
            }
            let mut parent = None;
            if turn
                .process_rows
                .iter()
                .any(|row| show_reasoning || row.kind != TimelineRowKind::Reasoning)
            {
                let mut header = process_header(turn);
                header.expandable = answered && !turn.pending_permission;
                header.expanded =
                    !header.expandable || disclosures.get(&header.id).copied().unwrap_or(!finished);
                parent = Some(header.id.clone());
                entries.push(header);
            }
            let mut groups = turn
                .process_activity_groups_with_commands_and_file_operations
                .iter()
                .peekable();
            let mut group_parent = None::<(&str, usize)>;
            let mut files = Vec::new();
            for (index, row) in turn.process_rows.iter().enumerate() {
                if group_parent.is_some_and(|(_, end)| index >= end) {
                    group_parent = None;
                }
                if let Some(group) = groups.next_if(|group| group.start_row == index) {
                    let rows = &turn.process_rows[group.start_row..group.end_row];
                    let mut summary = ActivitySummary::default();
                    for row in rows {
                        summary.record(
                            &Activity::project(row, row_payload(row, &model.items), locale),
                            &row.id,
                        );
                    }
                    let approval = rows.iter().any(|row| row.pending_permission);
                    entries.push(TimelineEntry {
                        id: group.id.clone(),
                        kind: EntryKind::ActivityGroup,
                        title: summary.label(locale),
                        turn_id: turn.id.clone(),
                        parent_id: parent.clone(),
                        expandable: !approval,
                        expanded: approval
                            || activity::group_open(
                                turn,
                                group,
                                disclosures.get(&group.id).copied(),
                                false,
                            ),
                        streaming: rows.iter().any(|row| activity::is_running(row, !finished)),
                        ..TimelineEntry::default()
                    });
                    group_parent = Some((&group.id, group.end_row));
                }
                if row.kind == TimelineRowKind::Reasoning && !show_reasoning {
                    continue;
                }
                let mut entry = self.entry_from_row(row, turn, &model.items);
                entry.parent_id = group_parent
                    .map(|(id, _)| id.to_owned())
                    .or_else(|| parent.clone());
                if finished
                    && let Some(change) = turn_file_change(row, row_payload(row, &model.items))
                {
                    files.push(change);
                }
                entries.push(entry);
            }
            if let Some(row) = turn.conclusion_row.as_ref() {
                entries.push(self.entry_from_row(row, turn, &model.items));
            } else if !finished && !turn.pending_permission {
                if let Some(thought) = turn
                    .live_status
                    .as_deref()
                    .filter(|thought| !thought.trim().is_empty())
                    && show_reasoning
                {
                    let id = format!("reasoning-live:{}", turn.id);
                    entries.push(TimelineEntry {
                        expanded: self.expanded_rows.borrow().contains(&id),
                        id,
                        kind: EntryKind::Reasoning,
                        body: activity::reasoning_window_tail(thought).to_owned(),
                        streaming: true,
                        expandable: true,
                        turn_id: turn.id.clone(),
                        ..TimelineEntry::default()
                    });
                } else if self.show_generation_status.get_untracked() {
                    entries.push(live_turn_entry(turn));
                }
            }
            if !files.is_empty() {
                let mut seen = std::collections::BTreeSet::new();
                files.reverse();
                files.retain(|change| seen.insert(change.path.clone()));
                files.reverse();
                entries.push(changes_entry(turn, files));
            }
        }
        drop(disclosures);
        let latest_ended_normally = turns
            .last()
            .is_some_and(|turn| turn.complete && !turn.failed && !turn.pending_permission);
        let needs_continue = state.is_some_and(|state| {
            vibex_core::agent_session_turn_requires_continuation(state, Some(latest_ended_normally))
        });
        let plan = plan_view(&model.items);
        let goal = goal_view(&model.items);
        let editable = turns
            .iter()
            .rev()
            .find_map(|turn| {
                (turn.complete && !turn.pending_permission)
                    .then(|| turn.user_row.as_ref().map(|row| row.last_sequence))
                    .flatten()
            })
            .unwrap_or(0);
        drop(model);
        self.timeline.set(entries);
        self.plan.set(plan);
        self.goal.set(goal);
        self.needs_continue.set(needs_continue);
        if !needs_continue {
            self.auto_continue_armed.set(false);
        } else if self.auto_continue.get_untracked() && !self.auto_continue_armed.get() {
            self.auto_continue_armed.set(true);
            let service = self.clone();
            spawn(async move {
                timer(Duration::from_secs(2)).await;
                service.continue_turn();
            });
        }
        self.editable_sequence.set(editable);
        self.refresh_pending();
    }

    fn entry_from_row(
        &self,
        row: &TimelineRow,
        turn: &TimelineConversationTurn,
        items: &[vibex_core::TimelineItem],
    ) -> TimelineEntry {
        let expanded = self.expanded_rows.borrow().contains(&row.id);
        let kind = entry_kind(row.kind);
        let item = row_item(row, items);
        let payload = item.map(|item| &item.payload);
        let shared = (kind == EntryKind::Tool)
            .then(|| Activity::project(row, payload, copy::language().locale()));
        let finished = activity::turn_finished(turn);
        let detail_cached = self.row_details.borrow();
        let details = detail_cached
            .get(&row.id)
            .filter(|(sequence, _)| *sequence == row.last_sequence);
        let details_pending = details.is_none()
            && item.is_some_and(|item| {
                item.payload_depth == vibex_core::TimelinePayloadDepth::Summary
            });
        TimelineEntry {
            id: row.id.clone(),
            kind,
            title: shared.as_ref().map_or_else(
                || row.title.clone(),
                |activity| activity.action().to_owned(),
            ),
            body: row.body.clone(),
            meta: row_meta(payload),
            streaming: if shared.is_some() {
                activity::is_running(row, !finished)
            } else {
                row.streaming && !finished
            },
            turn_id: turn.id.clone(),
            expanded,
            expandable: shared.is_some() || kind == EntryKind::Reasoning,
            attachments: row_attachments(row, payload),
            last_sequence: row.last_sequence,
            started_at_ms: turn.started_at_ms,
            ended_at_ms: turn.ended_at_ms.unwrap_or(0),
            complete: finished,
            parent_id: None,
            changed_files: Vec::new(),
            activity: shared.map(|shared| ActivityView {
                glyph: activity_glyph(shared.icon()),
                target: shared.target().to_owned(),
                path: shared.path().map(str::to_owned),
                failed: shared.is_failed(),
                error: shared.error_line().map(str::to_owned),
                duration: activity::row_duration_ms(row, &shared, items)
                    .map(activity::format_duration),
                details_pending,
                details_loading: self
                    .loading_row_details
                    .borrow()
                    .contains(&(row.id.clone(), row.last_sequence)),
                details: details
                    .map(|(_, details)| details.clone())
                    .unwrap_or_else(|| {
                        if expanded {
                            activity_details(row, payload)
                        } else {
                            Vec::new()
                        }
                    }),
            }),
        }
    }

    fn toggle_turn(&self, row_id: &str) {
        let Some(expanded) = self.timeline.with(|entries| {
            entries
                .iter()
                .find(|entry| {
                    entry.id == row_id
                        && entry.expandable
                        && matches!(
                            entry.kind,
                            EntryKind::ProcessHeader | EntryKind::ActivityGroup
                        )
                })
                .map(|entry| entry.expanded)
        }) else {
            return;
        };
        self.turn_disclosures
            .borrow_mut()
            .insert(row_id.to_owned(), !expanded);
        self.publish();
    }

    fn reveal_row(&self, row_id: &str, detail: bool) {
        let Some((parents, open_detail)) = self.timeline.with_untracked(|entries| {
            let entry = entries.iter().find(|entry| entry.id == row_id)?;
            let open_detail =
                (detail && entry.expandable && !entry.expanded).then_some(entry.last_sequence);
            let mut id = entry.parent_id.as_deref();
            let mut parents = Vec::new();
            while let Some(parent) = id.and_then(|id| entries.iter().find(|entry| entry.id == id)) {
                if !parent.expanded {
                    parents.push(parent.id.clone());
                }
                id = parent.parent_id.as_deref();
            }
            Some((parents, open_detail))
        }) else {
            return;
        };
        let open_parents = !parents.is_empty();
        if open_parents {
            self.turn_disclosures
                .borrow_mut()
                .extend(parents.into_iter().map(|id| (id, true)));
        }
        if let Some(sequence) = open_detail {
            self.toggle_row_detail(row_id, sequence);
        } else if open_parents {
            self.publish();
        }
    }

    /// Opens or closes one row's full detail, reading it on first open.
    fn toggle_row_detail(&self, row_id: &str, sequence: i64) {
        let mut expanded = self.expanded_rows.borrow_mut();
        if expanded.remove(row_id) {
            drop(expanded);
            self.publish();
            return;
        }
        expanded.insert(row_id.to_owned());
        drop(expanded);
        let needs_read = self.timeline.with_untracked(|entries| {
            entries.iter().any(|entry| {
                entry.id == row_id
                    && entry
                        .activity
                        .as_ref()
                        .is_some_and(|activity| activity.details_pending)
            })
        });
        if !needs_read || sequence <= 0 {
            self.publish();
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let service = self.clone();
        let backend = self.backend.clone();
        let row_id = row_id.to_owned();
        let generation = self.workbench_generation.get();
        let request_key = (row_id.clone(), sequence);
        let started = self
            .loading_row_details
            .borrow_mut()
            .insert(request_key.clone());
        self.publish();
        if !started {
            return;
        }
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .fetch_timeline_item(FetchTimelineItemRequest {
                            session_id,
                            sequence,
                        })
                        .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            service
                .loading_row_details
                .borrow_mut()
                .remove(&request_key);
            if !service.timeline.with_untracked(|entries| {
                entries
                    .iter()
                    .any(|entry| entry.id == row_id && entry.last_sequence == sequence)
            }) {
                return;
            }
            match result {
                Ok(Ok(item)) => {
                    let details = vibex_desktop_model::timeline_rows(std::slice::from_ref(&item))
                        .into_iter()
                        .next()
                        .map(|row| activity_details(&row, Some(&item.payload)))
                        .unwrap_or_default();
                    service
                        .row_details
                        .borrow_mut()
                        .insert(row_id, (sequence, details));
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("reading the activity did not finish: {error}"),
                )),
            }
            service.publish();
        });
    }

    /// Forks the conversation through `through_sequence`, then opens the new
    /// session.
    fn fork_at(&self, through_sequence: i64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(source_session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let expected = self.model.borrow().authoritative_end_sequence;
        let through_sequence = if through_sequence <= 0 {
            ForkAgentSessionRequest::AT_TIP
        } else {
            through_sequence
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .fork_session(MutationRequest::new(ForkAgentSessionRequest {
                            source_session_id,
                            through_sequence,
                            expected_source_end_sequence: expected,
                        }))
                        .await
                })
                .await;
            match result {
                Ok(Ok(session)) => {
                    service.refresh();
                    service.open(session.id.as_str());
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_fork_task_failed",
                    format!("forking the session did not finish: {error}"),
                )),
            }
        });
    }

    /// Re-reads the pending permission from the projection.
    ///
    /// The newest pending request wins: a runtime blocks on one at a time per
    /// session, and an older one still marked pending cannot be answered.
    fn refresh_pending(&self) {
        let Some(active) = self.active.get_untracked() else {
            self.pending.set(None);
            self.question.set(None);
            *self.question_request.borrow_mut() = None;
            return;
        };
        let mut pending = None;
        let mut question = None;
        let mut question_request = None;
        {
            let model = self.model.borrow();
            for item in model.items.iter().rev() {
                match &item.payload {
                    TimelinePayload::PermissionRequest(request)
                        if pending.is_none()
                            && request.status == PermissionRequestStatus::Pending
                            && request.session_id.as_str() == active =>
                    {
                        pending = Some(permission_pending(request));
                    }
                    TimelinePayload::ElicitationRequest(request)
                        if question.is_none()
                            && request.status == ElicitationRequestStatus::Pending
                            && request.session_id.as_str() == active =>
                    {
                        question = Some(pending_question(request));
                        question_request = Some(request.clone());
                    }
                    _ => {}
                }
                if pending.is_some() && question.is_some() {
                    break;
                }
            }
        }
        self.pending.set(pending);
        self.question.set(question);
        *self.question_request.borrow_mut() = question_request;
    }

    /// Answers a permission card and re-reads the transcript.
    fn resolve_permission(&self, request_id: &str, choice: PermissionChoice) {
        if self.resolving.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Ok(request_id) = RequestId::parse(request_id) else {
            return;
        };
        let response = match choice {
            PermissionChoice::Allow => PermissionResponseKind::Approve,
            PermissionChoice::AlwaysAllow => PermissionResponseKind::AlwaysAllowForSession,
            PermissionChoice::Deny => PermissionResponseKind::Deny,
        };
        self.resolving.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let request_request = request_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .resolve_permission(MutationRequest::new(ResolvePermissionRequest {
                            session_id: request_session.clone(),
                            request_id: request_request.clone(),
                            resolution: PermissionResolution {
                                request_id: request_request,
                                session_id: request_session,
                                response,
                                responder_device_id: None,
                                provider_resolution_id: None,
                                note: None,
                                resolved_at_ms: unix_timestamp_ms(),
                                responder: Some(ApprovalResponder::Human),
                            },
                        }))
                        .await
                })
                .await;
            service.resolving.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.pending.set(None);
                    service.reload(&session_id);
                    crate::settings::haptic(nsh_core::Haptic::Selection);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("resolving the permission did not finish: {error}"),
                )),
            }
        });
    }

    /// Answers a question card and re-reads the transcript.
    fn resolve_question(&self, request_id: &str, response: QuestionResponse) {
        if self.resolving.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let Ok(request_id) = RequestId::parse(request_id) else {
            return;
        };
        let Some(request) = self.question_request.borrow().clone() else {
            return;
        };
        let (action, answers) = match response {
            QuestionResponse::Decline => (ElicitationResolutionAction::Decline, BTreeMap::new()),
            QuestionResponse::Submit(answers) => match answers_for(&request, &answers) {
                Ok(answers) => (ElicitationResolutionAction::Accept, answers),
                Err(message) => {
                    self.fail_text(&message);
                    return;
                }
            },
        };
        self.resolving.set(true);
        self.error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request_session = session_id.clone();
            let request_request = request_id.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .resolve_elicitation(MutationRequest::new(ResolveElicitationRequest {
                            session_id: request_session.clone(),
                            request_id: request_request.clone(),
                            resolution: ElicitationResolution {
                                request_id: request_request,
                                session_id: request_session,
                                action,
                                answers,
                                responder_device_id: None,
                                resolved_at_ms: unix_timestamp_ms(),
                            },
                        }))
                        .await
                })
                .await;
            service.resolving.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.question.set(None);
                    service.reload(&session_id);
                    crate::settings::haptic(nsh_core::Haptic::Selection);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("resolving the question did not finish: {error}"),
                )),
            }
        });
    }

    /// Keeps the raw entries behind the palettes so a picked row can arm its
    /// command or reference.
    fn remember_entries(&self, discovery: &vibex_core::AgentCommandDiscovery) {
        let mut catalog = self.entry_catalog.borrow_mut();
        for entry in discovery
            .response
            .entries
            .iter()
            .chain(discovery.quick_phrases.iter())
            .chain(discovery.delegation_entries.iter())
            .chain(discovery.session_entries.iter())
        {
            catalog.insert(entry.id.clone(), entry.clone());
        }
    }

    fn arm_entry(&self, id: &str) {
        let Some(entry) = self.entry_catalog.borrow().get(id).cloned() else {
            return;
        };
        match entry.trigger {
            AgentCommandTrigger::Slash
                if matches!(
                    entry.source_kind,
                    AgentCommandSourceKind::Provider | AgentCommandSourceKind::Prompt
                ) && entry.execution_behavior != AgentCommandExecutionBehavior::None =>
            {
                *self.armed_command.borrow_mut() = Some(entry);
            }
            AgentCommandTrigger::Mention => {
                let Some(mention) = mention_for_entry(&entry) else {
                    return;
                };
                let mut mentions = self.armed_mentions.borrow_mut();
                mentions.retain(|(_, existing)| {
                    existing.reference != mention.reference
                        || existing.kind != mention.kind
                        || existing.agent_id != mention.agent_id
                });
                mentions.push((entry.insertion_text.clone(), mention));
            }
            _ => {}
        }
    }

    /// Loads the `/` commands the open session offers.
    ///
    /// One read per session: the palette filters what came back locally as the
    /// person keeps typing, so typing does not make a request per keystroke.
    fn query_commands(&self, _query: &str) {
        if self.commands_loaded.get() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.commands_loaded.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .discover_agent_commands(AgentCommandDiscoverRequest {
                            agent_id: None,
                            provider_profile_id: None,
                            session_id: Some(session_id),
                            workspace_id: None,
                            trigger: Some(AgentCommandTrigger::Slash),
                            query: None,
                            limit: Some(30),
                        })
                        .await
                })
                .await;
            if let Ok(Ok(discovery)) = result {
                service.remember_entries(&discovery);
                service.commands.set(command_entries(discovery));
            }
        });
    }

    /// Picks photos and keeps them as attachments for the next message.
    fn attach_camera(&self) {
        self.error.set(None);
        if !crate::host::request_camera() {
            self.fail_text(copy::text(
                "The camera is unavailable on this host.",
                "当前宿主无法使用相机。",
                "目前宿主無法使用相機。",
            ));
        }
    }

    /// Reads the photo the host wrote and turns it into an attachment.
    pub fn attach_camera_photo(&self, path: String) {
        let bytes = match std::fs::read(&path) {
            Ok(bytes) => bytes,
            Err(error) => {
                self.fail_text(&format!(
                    "{}: {error}",
                    copy::text("reading the photo failed", "读取照片失败", "讀取照片失敗")
                ));
                return;
            }
        };
        let seq = self.attachment_seq.get().wrapping_add(1);
        self.attachment_seq.set(seq);
        let label = path.rsplit('/').next().unwrap_or("photo.jpg").to_owned();
        let mime_type = "image/jpeg".to_owned();
        let uri = format!(
            "data:{mime_type};base64,{}",
            base64::engine::general_purpose::STANDARD.encode(bytes)
        );
        self.attachments.update(|items| {
            items.push(AttachmentItem {
                id: format!("attachment-{seq}"),
                label,
                mime_type,
                uri,
            });
        });
    }

    fn attach_images(&self) {
        self.error.set(None);
        let active = self.active.get_untracked();
        let service = self.clone();
        spawn(async move {
            let mut request = PickRequest::new(true, 4);
            request.max_dimension = Some(nsh_core::geometry::DevicePixels(1600));
            let files = pick_images(request).await;
            if service.alive.get() && service.active.get_untracked() == active {
                service.accept_attachments(files);
            }
        });
    }

    fn attach_new_images(&self) {
        self.new_error.set(None);
        let service = self.clone();
        spawn(async move {
            let mut request = PickRequest::new(true, 4);
            request.max_dimension = Some(nsh_core::geometry::DevicePixels(1600));
            let files = pick_images(request).await;
            service.accept_new_attachments(files);
        });
    }

    fn attach_new_files(&self) {
        self.new_error.set(None);
        let service = self.clone();
        spawn(async move {
            let mut request = FilePickRequest::new(true, 4);
            request.max_size = Some(8 * 1024 * 1024);
            let files = pick_files(request).await;
            service.accept_new_attachments(files);
        });
    }

    fn accept_new_attachments(&self, files: Vec<nsh_core::picker::PickedFile>) {
        let mut too_large = None;
        for file in files {
            if file.status == PickedStatus::TooLarge {
                too_large = Some(file.name);
                continue;
            }
            let seq = self.attachment_seq.get().wrapping_add(1);
            self.attachment_seq.set(seq);
            let mime_type = file.mime.clone();
            let uri = format!(
                "data:{};base64,{}",
                mime_type,
                base64::engine::general_purpose::STANDARD.encode(file.data.bytes())
            );
            self.new_attachments.update(|items| {
                items.push(AttachmentItem {
                    id: format!("new-attachment-{seq}"),
                    label: file.name,
                    mime_type,
                    uri,
                });
            });
        }
        if let Some(name) = too_large {
            self.new_error.set(Some(
                format!(
                    "{name}: {}",
                    copy::text(
                        "too large to attach",
                        "文件太大，无法附加",
                        "檔案太大，無法附加",
                    )
                )
                .into(),
            ));
        }
    }

    /// Picks documents and keeps them as attachments for the next message.
    fn attach_files(&self) {
        self.error.set(None);
        let active = self.active.get_untracked();
        let service = self.clone();
        spawn(async move {
            let mut request = FilePickRequest::new(true, 4);
            request.max_size = Some(8 * 1024 * 1024);
            let files = pick_files(request).await;
            if service.alive.get() && service.active.get_untracked() == active {
                service.accept_attachments(files);
            }
        });
    }

    /// Encodes what the picker returned as the `data:` URLs the runtime
    /// materializes.
    fn accept_attachments(&self, files: Vec<nsh_core::picker::PickedFile>) {
        let mut too_large = None;
        for file in files {
            if file.status == PickedStatus::TooLarge {
                too_large = Some(file.name);
                continue;
            }
            let seq = self.attachment_seq.get().wrapping_add(1);
            self.attachment_seq.set(seq);
            let mime_type = file.mime.clone();
            let uri = format!(
                "data:{};base64,{}",
                mime_type,
                base64::engine::general_purpose::STANDARD.encode(file.data.bytes())
            );
            self.attachments.update(|items| {
                items.push(AttachmentItem {
                    id: format!("attachment-{seq}"),
                    label: file.name,
                    mime_type,
                    uri,
                });
            });
        }
        if let Some(name) = too_large {
            self.error.set(Some(
                format!(
                    "{name}: {}",
                    copy::text(
                        "too large to attach",
                        "文件太大，无法附加",
                        "檔案太大，無法附加",
                    )
                )
                .into(),
            ));
        }
    }

    /// Loads the `@` references the open session offers.
    ///
    /// Like the `/` list, one read per session: the palette narrows it
    /// locally while the person types.
    fn query_mentions(&self, _query: &str) {
        if self.mentions_loaded.get() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.mentions_loaded.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .discover_agent_commands(AgentCommandDiscoverRequest {
                            agent_id: None,
                            provider_profile_id: None,
                            session_id: Some(session_id),
                            workspace_id: None,
                            trigger: Some(AgentCommandTrigger::Mention),
                            query: None,
                            limit: Some(40),
                        })
                        .await
                })
                .await;
            if let Ok(Ok(discovery)) = result {
                service.remember_entries(&discovery);
                let entries = command_entries(discovery);
                service.mentions.set(entries);
            }
        });
    }

    /// Loads the `$` skills the open session offers.
    fn query_skills(&self, _query: &str) {
        if self.skills_loaded.get() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.skills_loaded.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .discover_agent_commands(AgentCommandDiscoverRequest {
                            agent_id: None,
                            provider_profile_id: None,
                            session_id: Some(session_id),
                            workspace_id: None,
                            trigger: Some(AgentCommandTrigger::Dollar),
                            query: None,
                            limit: Some(40),
                        })
                        .await
                })
                .await;
            if let Ok(Ok(discovery)) = result {
                service.remember_entries(&discovery);
                service.skills.set(command_entries(discovery));
            }
        });
    }

    /// Stops the active session's turn.
    fn interrupt_active(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(active) else {
            return;
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .interrupt(MutationRequest::new(session_id.clone()))
                        .await
                })
                .await;
            match result {
                Ok(Ok(true)) => {
                    crate::settings::haptic(nsh_core::Haptic::Selection);
                    service.refresh();
                }
                Ok(Ok(false)) => {}
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_interrupt_task_failed",
                    format!("stopping the turn did not finish: {error}"),
                )),
            }
        });
    }

    /// Creates a worktree from the chosen workspace and opens a session in it.
    fn create_worktree_session(&self) {
        if self.new_busy.get_untracked() {
            return;
        }
        let Some(workspace_id) = self.new_workspace.get_untracked() else {
            return;
        };
        let branch = self.new_worktree_branch.get_untracked().trim().to_owned();
        if branch.is_empty() {
            self.new_error.set(Some(
                copy::text(
                    "Type a branch name for the new worktree.",
                    "请为新 Worktree 输入分支名。",
                    "請為新 Worktree 輸入分支名稱。",
                )
                .into(),
            ));
            return;
        }
        let Some(runtime_id) = self.new_runtime.get_untracked() else {
            return;
        };
        let Ok(index) = runtime_id.parse::<usize>() else {
            return;
        };
        let Some(base) = self.workspace_records.with(|workspaces| {
            workspaces
                .iter()
                .find(|space| space.workspace.id.as_str() == workspace_id)
                .map(|space| space.workspace.id.clone())
        }) else {
            return;
        };
        let Some(option) = self.new_runtime_options.borrow().get(index).cloned() else {
            return;
        };
        let draft = self.new_session_draft();
        let base_ref = self.new_base_ref.get_untracked().trim().to_owned();
        self.new_busy.set(true);
        self.new_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let created = crate::runtime::handle()
                .spawn(async move {
                    git.git_worktree_create(MutationRequest::new(GitWorktreeCreateRequest {
                        workspace_id: base,
                        branch_name: branch,
                        base_ref: (!base_ref.is_empty()).then_some(base_ref),
                        name: None,
                        worktree_path: None,
                        target_workspace_id: None,
                        target_branch: None,
                    }))
                    .await
                })
                .await;
            let worktree = match created {
                Ok(Ok(created)) => created,
                Ok(Err(error)) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(error_text(&error)));
                    return;
                }
                Err(error) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(
                        format!("creating the worktree did not finish: {error}").into(),
                    ));
                    return;
                }
            };
            let request = CreateAgentSessionRequest {
                runtime: option.selection,
                workspace_root: worktree.workspace.root_path,
                workspace_mode: worktree.workspace.mode,
                title: draft.title.clone(),
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result = crate::runtime::handle()
                .spawn(async move { backend.create_session(MutationRequest::new(request)).await })
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    service.finish_new_session(session, draft);
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("creating the session did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Renames the session the actions sheet is open on.
    fn rename_open_session(&self) {
        if self.session_busy.get_untracked() {
            return;
        }
        let Some(id) = self.session_actions.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(id.as_str()) else {
            return;
        };
        let title = self.session_rename.get_untracked().trim().to_owned();
        if title.is_empty() {
            return;
        }
        self.session_busy.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .rename_session(MutationRequest::new(RenameAgentSessionRequest {
                            session_id,
                            title,
                        }))
                        .await
                })
                .await;
            service.session_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    service.record_sidebar_session(&session);
                    let row = service
                        .workspace_records
                        .with(|workspaces| summary(&session, workspaces));
                    service.sessions.update(|sessions| {
                        if let Some(existing) = sessions.iter_mut().find(|s| s.id == row.id) {
                            *existing = row;
                        }
                    });
                    service.session_actions.set(None);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_rename_task_failed",
                    format!("renaming the session did not finish: {error}"),
                )),
            }
        });
    }

    fn archive_sessions(&self, ids: &[String]) {
        let ids = ids.to_vec();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            for id in ids {
                let Ok(session_id) = VibexSessionId::parse(id.as_str()) else {
                    continue;
                };
                let call = backend.clone();
                let result = crate::runtime::handle()
                    .spawn(
                        async move { call.archive_session(MutationRequest::new(session_id)).await },
                    )
                    .await;
                match result {
                    Ok(Ok(())) => service.drop_session(&id),
                    Ok(Err(error)) => service.fail(&error),
                    Err(error) => service.fail(&BackendError::failed(
                        "session_archive_task_failed",
                        format!("archiving the session did not finish: {error}"),
                    )),
                }
            }
        });
    }

    fn fork_sessions(&self, ids: &[String]) {
        let ids = ids
            .iter()
            .filter_map(|id| VibexSessionId::parse(id.as_str()).ok())
            .collect::<Vec<_>>();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            for source_session_id in ids {
                let backend = backend.clone();
                match crate::runtime::handle()
                    .spawn(async move {
                        backend
                            .fork_session(MutationRequest::new(ForkAgentSessionRequest {
                                source_session_id,
                                through_sequence: ForkAgentSessionRequest::AT_TIP,
                                expected_source_end_sequence: None,
                            }))
                            .await
                    })
                    .await
                {
                    Ok(Ok(session)) => service.record_sidebar_session(&session),
                    Ok(Err(error)) => service.fail(&error),
                    Err(error) => service.fail(&BackendError::failed(
                        "session_fork_task_failed",
                        error.to_string(),
                    )),
                }
            }
            service.refresh();
        });
    }

    fn delete_sessions(&self, ids: &[String]) {
        let ids = ids.to_vec();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            for id in ids {
                let Ok(session_id) = VibexSessionId::parse(id.as_str()) else {
                    continue;
                };
                let call = backend.clone();
                let result = crate::runtime::handle()
                    .spawn(
                        async move { call.delete_session(MutationRequest::new(session_id)).await },
                    )
                    .await;
                match result {
                    Ok(Ok(())) => service.drop_session(&id),
                    Ok(Err(error)) => service.fail(&error),
                    Err(error) => service.fail(&BackendError::failed(
                        "session_delete_task_failed",
                        format!("deleting the session did not finish: {error}"),
                    )),
                }
            }
        });
    }

    fn set_session_auto_continue(&self, id: &str, enabled: bool) {
        self.auto_continue_sessions.update(|sessions| {
            if enabled {
                if !sessions.iter().any(|candidate| candidate == id) {
                    sessions.push(id.to_owned());
                }
            } else {
                sessions.retain(|candidate| candidate != id);
            }
        });
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetSessionAutoContinue {
            session_id: id.to_owned(),
            enabled,
        });
    }

    fn mark_read(&self, id: &str) {
        if !self
            .unread
            .get_untracked()
            .iter()
            .any(|candidate| candidate == id)
        {
            return;
        }
        self.unread
            .update(|unread| unread.retain(|candidate| candidate != id));
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetSessionsUnread {
            session_ids: vec![id.to_owned()],
            unread: false,
        });
    }

    pub fn toggle_pin(&self, id: &str) {
        let pinned = !self
            .pinned
            .get_untracked()
            .iter()
            .any(|candidate| candidate == id);
        self.pinned.update(|pinned| {
            if pinned.iter().any(|candidate| candidate == id) {
                pinned.retain(|candidate| candidate != id);
            } else {
                pinned.push(id.to_owned());
            }
        });
        self.persist_pins();
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetSessionPinned {
            session_id: id.to_owned(),
            pinned,
        });
    }

    fn create_folder(&self) {
        let name = self.folder_name.get_untracked().trim().to_owned();
        if name.is_empty() {
            return;
        }
        let parent = self.folder_parent.get_untracked();
        let (project_id, workspace_id) = self.folder_scope.get_untracked();
        let service = self.clone();
        self.sidebar_mutation_attempt(
            RemoteSidebarOrganizationMutation::CreateFolder {
                name,
                project_id,
                workspace_id,
                parent_folder_id: parent,
            },
            1,
            Some(Rc::new(move || {
                service.folder_name.set(String::new());
                service.folder_open.set(false);
            })),
        );
    }

    fn open_sidebar_folder(
        &self,
        project_id: Option<String>,
        workspace_id: Option<String>,
        parent: Option<String>,
    ) {
        self.folder_scope.set((project_id, workspace_id));
        self.folder_parent.set(parent);
        self.folder_name.set(String::new());
        self.folder_open.set(true);
    }

    async fn reload_sidebar(&self) -> Result<(), BackendError> {
        let _sync = self.sidebar_sync.lock().await;
        let backend = self.sidebar_backend.clone();
        let snapshot = crate::runtime::handle()
            .spawn(async move { backend.sidebar_organization().await })
            .await
            .map_err(|error| {
                BackendError::failed("session_sidebar_read_failed", error.to_string())
            })??;
        self.apply_sidebar(snapshot);
        Ok(())
    }

    async fn submit_sidebar(
        &self,
        mutation: RemoteSidebarOrganizationMutation,
        revision: Option<u64>,
    ) -> Result<(), BackendError> {
        let _sync = self.sidebar_sync.lock().await;
        let backend = self.sidebar_backend.clone();
        let snapshot = crate::runtime::handle()
            .spawn(async move {
                backend
                    .mutate_sidebar_organization(mutation, revision)
                    .await
            })
            .await
            .map_err(|error| {
                BackendError::failed("session_sidebar_task_failed", error.to_string())
            })??;
        self.apply_sidebar(snapshot);
        Ok(())
    }

    fn apply_sidebar(&self, snapshot: RemoteSidebarOrganizationSnapshot) {
        if !self.alive.get() {
            return;
        }
        let mut view = SidebarOrganizationView::from_remote(&snapshot);
        let detailed = self
            .sidebar_mode_pending
            .get()
            .unwrap_or(view.hierarchy_mode == SidebarHierarchyMode::Detailed);
        view.hierarchy_mode = if detailed {
            SidebarHierarchyMode::Detailed
        } else {
            SidebarHierarchyMode::Compact
        };
        self.sidebar_detailed.set(detailed);
        self.auto_continue_paused
            .set(snapshot.auto_continue_paused_session_ids.clone());
        self.scheduled_sends.set(
            snapshot
                .scheduled_sends
                .iter()
                .map(|send| ScheduledSendView {
                    session_id: send.session_id.clone(),
                    message_id: send.message_id,
                    scheduled_at_ms: send.scheduled_at_ms,
                })
                .collect(),
        );
        let group_auto_continue = snapshot
            .groups
            .iter()
            .map(|group| {
                (
                    group.id.clone(),
                    view.auto_continue_group_enabled(group.auto_continue, &group.project_id),
                )
            })
            .collect::<BTreeMap<_, _>>();
        self.sidebar_view.set(view);
        self.pinned.set(snapshot.pinned_session_ids.clone());
        self.unread.set(snapshot.unread_session_ids.clone());
        let mut auto = snapshot
            .auto_continue_session_ids
            .iter()
            .cloned()
            .collect::<std::collections::BTreeSet<_>>();
        for (id, enabled) in &snapshot.auto_continue_session_overrides {
            if *enabled {
                auto.insert(id.clone());
            } else {
                auto.remove(id);
            }
        }
        self.auto_continue_sessions.set(auto.into_iter().collect());
        let mut sections = Vec::new();
        let mut folder_parents: BTreeMap<String, String> = BTreeMap::new();
        for placement in &snapshot.placements {
            if placement.item.kind == RemoteSidebarItemKind::Folder
                && let Some(parent) = placement.parent_folder_id.as_ref()
            {
                folder_parents.insert(placement.item.id.clone(), parent.clone());
            }
        }
        let folder_depth = |id: &str| {
            let mut depth = 0;
            let mut current = id;
            while let Some(parent) = folder_parents.get(current) {
                depth += 1;
                current = parent;
                if depth > 8 {
                    break;
                }
            }
            depth
        };
        let mut folder_members: BTreeMap<String, Vec<String>> = BTreeMap::new();
        for placement in &snapshot.placements {
            if placement.item.kind == RemoteSidebarItemKind::Session
                && let Some(folder) = placement.parent_folder_id.as_ref()
            {
                folder_members
                    .entry(folder.clone())
                    .or_default()
                    .push(placement.item.id.clone());
            }
        }
        for folder in &snapshot.folders {
            sections.push(SidebarSectionView {
                kind: SidebarSectionKind::Folder,
                pinned: false,
                id: folder.id.clone(),
                project_id: folder.project_id.clone(),
                workspace_id: folder.workspace_id.clone(),
                parent_id: folder_parents.get(&folder.id).cloned(),
                depth: folder_depth(&folder.id),
                collapsed: snapshot.collapsed_folder_ids.contains(&folder.id),
                name: folder.name.clone(),
                session_ids: folder_members.remove(&folder.id).unwrap_or_default(),
                auto_archive_after_days: folder.auto_archive_after_days,
                auto_continue: false,
            });
        }
        for group in &snapshot.groups {
            sections.push(SidebarSectionView {
                kind: SidebarSectionKind::Group,
                pinned: group.pinned,
                id: group.id.clone(),
                project_id: Some(group.project_id.clone()),
                workspace_id: Some(group.workspace_id.clone()),
                parent_id: folder_parents.get(&group.id).cloned(),
                depth: 0,
                collapsed: snapshot.collapsed_group_ids.contains(&group.id),
                name: group.name.clone(),
                session_ids: group.member_session_ids.clone(),
                auto_archive_after_days: None,
                auto_continue: group_auto_continue.get(&group.id).copied().unwrap_or(false),
            });
        }
        self.sidebar_sections.set(sections);
        *self.sidebar_snapshot.borrow_mut() = snapshot;
        self.sync_queue();
        self.persist_pins();
    }

    fn record_sidebar_session(&self, session: &AgentSession) {
        self.session_records.update(|sessions| {
            if let Some(existing) = sessions
                .iter_mut()
                .find(|existing| existing.id == session.id)
            {
                *existing = session.clone();
            } else {
                sessions.push(session.clone());
            }
        });
    }

    fn sidebar_host(&self) -> SidebarTree {
        SidebarTree {
            rows: {
                let service = self.clone();
                Rc::new(move |query| service.sidebar_rows(query))
            },
            detailed: self.sidebar_detailed,
            on_toggle_hierarchy: {
                let service = self.clone();
                Rc::new(move || service.toggle_sidebar_hierarchy())
            },
            on_toggle_row: {
                let service = self.clone();
                Rc::new(move |row| {
                    if let Some(operation) = collapse_sidebar_row(row.kind, &row.id, !row.collapsed)
                    {
                        service.sidebar_mutation(operation);
                    }
                })
            },
            on_reveal_active: {
                let service = self.clone();
                Rc::new(move || service.reveal_sidebar_session())
            },
            on_collapse_all: {
                let service = self.clone();
                Rc::new(move || service.collapse_sidebar_projects())
            },
            on_move_row: {
                let service = self.clone();
                Rc::new(move |row, delta| service.move_sidebar_row(row, delta))
            },
            on_move_row_step: {
                let service = self.clone();
                Rc::new(move |row, upwards| service.move_sidebar_row_step(row, upwards))
            },
            row_move_targets: {
                let service = self.clone();
                Rc::new(move |row| service.sidebar_move_targets(row))
            },
            on_new_session: {
                let service = self.clone();
                Rc::new(move |row| {
                    let folder = (row.kind == SidebarRowKind::Folder).then(|| row.id.clone());
                    service.open_new_session_in(
                        row.workspace_id.clone(),
                        row.project_id.clone(),
                        folder,
                    )
                })
            },
            on_new_folder: {
                let service = self.clone();
                Rc::new(move |row| {
                    service.open_sidebar_folder(
                        row.project_id.clone(),
                        row.workspace_id.clone(),
                        None,
                    )
                })
            },
            batch_scope: {
                let service = self.clone();
                Rc::new(move |row| {
                    service.session_records.with(|sessions| match row.kind {
                        SidebarRowKind::Project => sessions
                            .iter()
                            .filter(|session| {
                                session.deleted_at_ms.is_none()
                                    && session.project_id.as_str() == row.id
                            })
                            .map(|session| session.id.as_str().to_owned())
                            .collect(),
                        SidebarRowKind::Folder => service.sidebar_view.with(|view| {
                            sidebar::folder_batch_ids(&view.organization, sessions, &row.id)
                        }),
                        _ => Vec::new(),
                    })
                })
            },
        }
    }

    fn projected_sidebar(
        &self,
        view: &SidebarOrganizationView,
        query: &str,
    ) -> Vec<sidebar::SidebarRow> {
        let active = self
            .active
            .get()
            .and_then(|id| VibexSessionId::parse(id).ok());
        self.workspace_records.with(|workspaces| {
            self.session_records.with(|sessions| {
                sidebar::project_rows(view, workspaces, sessions, active.as_ref(), query)
            })
        })
    }

    fn toggle_sidebar_hierarchy(&self) {
        let detailed = !self.sidebar_detailed.get_untracked();
        self.sidebar_mode_pending.set(Some(detailed));
        self.display_sidebar_hierarchy(detailed);
        let request = self.sidebar_mode_request.get().wrapping_add(1);
        self.sidebar_mode_request.set(request);
        let service = self.clone();
        spawn(async move {
            let mode = if detailed {
                RemoteSidebarHierarchyMode::Detailed
            } else {
                RemoteSidebarHierarchyMode::Compact
            };
            let result = service
                .submit_sidebar(
                    RemoteSidebarOrganizationMutation::SetHierarchyMode { mode },
                    None,
                )
                .await;
            if !service.alive.get() || service.sidebar_mode_request.get() != request {
                return;
            }
            service.sidebar_mode_pending.set(None);
            let mode = service.sidebar_snapshot.borrow().hierarchy_mode;
            service.display_sidebar_hierarchy(mode == RemoteSidebarHierarchyMode::Detailed);
            if let Err(error) = result {
                service.fail(&error);
            }
        });
    }

    fn display_sidebar_hierarchy(&self, detailed: bool) {
        self.sidebar_detailed.set(detailed);
        self.sidebar_view.update(|view| {
            view.hierarchy_mode = if detailed {
                SidebarHierarchyMode::Detailed
            } else {
                SidebarHierarchyMode::Compact
            };
        });
    }

    fn sidebar_rows(&self, query: &str) -> Vec<SidebarRowView> {
        self.sidebar_view.with(|view| {
            let rows = self.projected_sidebar(view, query);
            let cards = sidebar::workspace_cards(&rows);
            let pinned = self.pinned.get();
            let project_appearances = self.workspace_records.with(|workspaces| {
                workspaces
                    .iter()
                    .map(|space| {
                        (
                            space.project.id.as_str().to_owned(),
                            view.project_appearances
                                .get(space.project.id.as_str())
                                .cloned()
                                .unwrap_or_else(|| {
                                    SidebarProjectAppearance::default_for(&space.project)
                                }),
                        )
                    })
                    .collect::<BTreeMap<_, _>>()
            });
            self.session_records.with(|sessions| {
                let by_id = sessions
                    .iter()
                    .map(|session| (session.id.as_str(), session))
                    .collect::<BTreeMap<_, _>>();
                rows.iter()
                    .enumerate()
                    .map(|(index, row)| {
                        let id = row.id();
                        let kind = sidebar_kind(row.kind);
                        let session = (kind == SidebarRowKind::Session)
                            .then(|| by_id.get(id))
                            .flatten();
                        SidebarRowView {
                            id: id.to_owned(),
                            kind,
                            project_id: if kind == SidebarRowKind::Project {
                                Some(id.to_owned())
                            } else {
                                row.project_id.clone()
                            },
                            workspace_id: row.workspace_id.clone(),
                            title: if kind == SidebarRowKind::EmptyWorkspace {
                                copy::text("No sessions", "没有会话", "沒有工作階段").to_owned()
                            } else if kind == SidebarRowKind::Session && row.label.trim().is_empty()
                            {
                                copy::text("New session", "新会话", "新工作階段").to_owned()
                            } else {
                                row.label.clone()
                            },
                            detail: session
                                .map(|session| session.agent_id.as_str().to_owned())
                                .or_else(|| row.detail.clone())
                                .unwrap_or_default(),
                            metadata: session
                                .map(|session| {
                                    age(session.last_message_at_ms.max(session.updated_at_ms))
                                })
                                .unwrap_or_default(),
                            indent: row.indent,
                            guides: sidebar::folder_guides(&rows, index),
                            card: cards[index].as_ref().map(|card| {
                                vibex_nsh_ui::screens::session::SidebarCardSlice {
                                    indent: card.indent,
                                    top: card.edge == sidebar::SidebarCardEdge::Top,
                                    bottom: card.edge == sidebar::SidebarCardEdge::Bottom,
                                }
                            }),
                            count: row.child_count,
                            project_mark: (kind == SidebarRowKind::Project).then(|| {
                                project_mark(
                                    project_appearances
                                        .get(id)
                                        .or_else(|| view.project_appearances.get(id)),
                                )
                            }),
                            collapsed: row.collapsed,
                            active: row.selected,
                            pinned: row.pinned
                                || kind == SidebarRowKind::Session
                                    && pinned.iter().any(|pinned| pinned == id),
                            unread: row.unread
                                && !(kind == SidebarRowKind::Session && row.selected),
                            auto_continue: row.auto_continue,
                            auto_continue_paused: row.auto_continue_paused,
                            scheduled: view
                                .scheduled_sends
                                .iter()
                                .any(|send| send.session_id == id),
                            state: row.state.map(display_state),
                            project_starts_in_worktree: kind == SidebarRowKind::Project
                                && view.project_location_preferences.get(id).is_some_and(
                                    |location| *location == NewSessionLocation::NewWorktree,
                                ),
                        }
                    })
                    .collect()
            })
        })
    }

    fn collapse_sidebar_projects(&self) {
        let projects = self.workspace_records.with(|workspaces| {
            self.session_records
                .with(|sessions| sidebar::projects(workspaces, sessions))
        });
        let operations = self.sidebar_view.with(|view| {
            let collapsed = projects
                .iter()
                .any(|project| !view.collapsed_project_ids.contains(&project.id));
            projects
                .into_iter()
                .filter(|project| view.collapsed_project_ids.contains(&project.id) != collapsed)
                .map(
                    |project| RemoteSidebarOrganizationMutation::SetProjectCollapsed {
                        project_id: project.id,
                        collapsed,
                    },
                )
                .collect::<Vec<_>>()
        });
        self.sidebar_mutations(operations);
    }

    fn reveal_sidebar_session(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let view = self.sidebar_view.get_untracked();
        let mut expanded = view.clone();
        expanded.collapsed_project_ids.clear();
        expanded.collapsed_workspace_ids.clear();
        expanded.organization.collapsed_folder_ids.clear();
        expanded.organization.collapsed_group_ids.clear();
        let rows = untracked(|| self.projected_sidebar(&expanded, ""));
        let Some(index) = rows
            .iter()
            .position(|row| row.kind == sidebar::SidebarRowKind::Session && row.id() == active)
        else {
            return;
        };
        let mut depth = rows[index].depth;
        let mut operations = Vec::new();
        for row in rows[..index].iter().rev() {
            if row.depth >= depth {
                continue;
            }
            depth = row.depth;
            let id = row.id();
            let kind = sidebar_kind(row.kind);
            let collapsed = match kind {
                SidebarRowKind::Project => view.collapsed_project_ids.contains(id),
                SidebarRowKind::Workspace => view.collapsed_workspace_ids.contains(id),
                SidebarRowKind::Folder => view.organization.collapsed_folder_ids.contains(id),
                SidebarRowKind::Group => view.organization.collapsed_group_ids.contains(id),
                _ => false,
            };
            if collapsed && let Some(operation) = collapse_sidebar_row(kind, id, false) {
                operations.push(operation);
            }
        }
        operations.reverse();
        self.sidebar_mutations(operations);
    }

    fn move_sidebar_row(&self, moved: &SidebarRowView, delta: f32) {
        let rows = untracked(|| {
            self.sidebar_view
                .with(|view| self.projected_sidebar(view, ""))
        });
        let Some(index) = rows
            .iter()
            .position(|row| sidebar_kind(row.kind) == moved.kind && row.id() == moved.id)
        else {
            return;
        };
        let position =
            sidebar::row_at_position(delta, 0.0, 0.0, vibex_nsh_ui::theme::sidebar_row_height())
                + index as f32
                + 0.5;
        if let Some(operation) = sidebar::move_operation(&rows, index, position) {
            self.sidebar_mutation(operation);
        }
    }

    /// Whether one row has a sibling above and below it in its own scope.
    fn sidebar_move_targets(&self, row: &SidebarRowView) -> (bool, bool) {
        let rows = untracked(|| {
            self.sidebar_view
                .with(|view| self.projected_sidebar(view, ""))
        });
        let same_scope = |candidate: &&sidebar::SidebarRow| {
            if row.kind == SidebarRowKind::Workspace {
                candidate.kind == sidebar::SidebarRowKind::Workspace
                    && candidate.project_id == row.project_id
            } else {
                matches!(
                    candidate.kind,
                    sidebar::SidebarRowKind::Session
                        | sidebar::SidebarRowKind::Folder
                        | sidebar::SidebarRowKind::Group
                ) && candidate.project_id == row.project_id
                    && candidate.workspace_id == row.workspace_id
                    && (candidate.indent - row.indent).abs() < f32::EPSILON
            }
        };
        let siblings = rows.iter().filter(same_scope).collect::<Vec<_>>();
        let Some(index) = siblings.iter().position(|candidate| {
            sidebar_kind(candidate.kind) == row.kind && candidate.id() == row.id
        }) else {
            return (false, false);
        };
        (index > 0, index + 1 < siblings.len())
    }

    /// Moves one row one step against the sibling beside it. A phone cannot
    /// drop a row it picked up on every platform, so the sheet carries the
    /// same move the drag performs.
    fn move_sidebar_row_step(&self, row: &SidebarRowView, upwards: bool) {
        let rows = untracked(|| {
            self.sidebar_view
                .with(|view| self.projected_sidebar(view, ""))
        });
        let same_scope = |candidate: &&sidebar::SidebarRow| {
            if row.kind == SidebarRowKind::Workspace {
                candidate.kind == sidebar::SidebarRowKind::Workspace
                    && candidate.project_id == row.project_id
            } else {
                matches!(
                    candidate.kind,
                    sidebar::SidebarRowKind::Session
                        | sidebar::SidebarRowKind::Folder
                        | sidebar::SidebarRowKind::Group
                ) && candidate.project_id == row.project_id
                    && candidate.workspace_id == row.workspace_id
                    && (candidate.indent - row.indent).abs() < f32::EPSILON
            }
        };
        let siblings = rows.iter().filter(same_scope).collect::<Vec<_>>();
        let Some(index) = siblings.iter().position(|candidate| {
            sidebar_kind(candidate.kind) == row.kind && candidate.id() == row.id
        }) else {
            return;
        };
        let target = if upwards {
            index.checked_sub(1)
        } else {
            (index + 1 < siblings.len()).then_some(index + 1)
        };
        let Some(anchor) = target.and_then(|target| siblings.get(target)) else {
            return;
        };
        let position = if upwards {
            vibex_core::RemoteSidebarDropPosition::Before
        } else {
            vibex_core::RemoteSidebarDropPosition::After
        };
        if row.kind == SidebarRowKind::Workspace {
            self.sidebar_mutation(RemoteSidebarOrganizationMutation::MoveWorkspaces {
                project_id: row.project_id.clone().unwrap_or_default(),
                workspace_ids: vec![row.workspace_id.clone().unwrap_or_default()],
                anchor_workspace_id: Some(anchor.id().to_owned()),
                position,
            });
            return;
        }
        let item_ref = |candidate: &sidebar::SidebarRow| {
            let kind = match candidate.kind {
                sidebar::SidebarRowKind::Folder => RemoteSidebarItemKind::Folder,
                sidebar::SidebarRowKind::Group => RemoteSidebarItemKind::Group,
                sidebar::SidebarRowKind::Project => RemoteSidebarItemKind::Project,
                _ => RemoteSidebarItemKind::Session,
            };
            vibex_core::RemoteSidebarItemRef {
                kind,
                id: candidate.id().to_owned(),
            }
        };
        let moved_kind = match row.kind {
            SidebarRowKind::Folder => RemoteSidebarItemKind::Folder,
            SidebarRowKind::Group => RemoteSidebarItemKind::Group,
            _ => RemoteSidebarItemKind::Session,
        };
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::MoveItems {
            items: vec![vibex_core::RemoteSidebarItemRef {
                kind: moved_kind,
                id: row.id.clone(),
            }],
            anchor: Some(item_ref(anchor)),
            position,
            project_id: row.project_id.clone(),
        });
    }

    fn sidebar_mutations(&self, mut operations: Vec<RemoteSidebarOrganizationMutation>) {
        operations.reverse();
        self.next_sidebar_mutation(Rc::new(RefCell::new(operations)));
    }

    fn next_sidebar_mutation(
        &self,
        remaining: Rc<RefCell<Vec<RemoteSidebarOrganizationMutation>>>,
    ) {
        let operation = remaining.borrow_mut().pop();
        if let Some(operation) = operation {
            let service = self.clone();
            self.sidebar_mutation_attempt(
                operation,
                1,
                Some(Rc::new(move || {
                    service.next_sidebar_mutation(remaining.clone())
                })),
            );
        }
    }

    fn persist_pins(&self) {
        if let Ok(text) = serde_json::to_string(&self.pinned.get_untracked()) {
            let _ = Storage::set_item(PINS_KEY, text);
        }
    }

    fn rename_folder(&self, id: &str, name: &str) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::RenameFolder {
            folder_id: id.to_owned(),
            name,
        });
    }

    fn delete_folder(&self, id: &str) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::DeleteFolder {
            folder_id: id.to_owned(),
        });
    }

    fn move_session_to_folder(&self, id: &str, folder: Option<&str>) {
        let mutation = RemoteSidebarOrganizationMutation::MoveItems {
            items: vec![vibex_core::RemoteSidebarItemRef {
                kind: RemoteSidebarItemKind::Session,
                id: id.to_owned(),
            }],
            anchor: folder.map(|folder| vibex_core::RemoteSidebarItemRef {
                kind: RemoteSidebarItemKind::Folder,
                id: folder.to_owned(),
            }),
            position: if folder.is_some() {
                vibex_core::RemoteSidebarDropPosition::Into
            } else {
                vibex_core::RemoteSidebarDropPosition::After
            },
            project_id: None,
        };
        self.move_open.set(None);
        self.sidebar_mutation(mutation);
    }

    fn rename_group(&self, id: &str, name: &str) {
        let name = name.trim().to_owned();
        if name.is_empty() {
            return;
        }
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::RenameGroup {
            group_id: id.to_owned(),
            name,
        });
    }

    fn set_group_pinned(&self, id: &str, pinned: bool) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetGroupPinned {
            group_id: id.to_owned(),
            pinned,
        });
    }

    fn set_group_auto_continue(&self, id: &str, enabled: bool) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetGroupAutoContinue {
            group_id: id.to_owned(),
            enabled,
        });
    }

    fn set_group_members(&self, id: &str, members: &[String]) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::SetGroupMembers {
            group_id: id.to_owned(),
            session_ids: members.to_vec(),
        });
    }

    fn delete_group(&self, id: &str) {
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::DeleteGroup {
            group_id: id.to_owned(),
        });
    }

    fn create_group(&self, name: &str, members: &[String]) {
        let name = name.trim().to_owned();
        if name.is_empty() || members.is_empty() {
            return;
        }
        let first = self.sessions.with(|sessions| {
            members.iter().find_map(|id| {
                sessions
                    .iter()
                    .find(|session| &session.id == id)
                    .map(|session| (session.project_id.clone(), session.workspace_id.clone()))
            })
        });
        let Some((project_id, workspace_id)) = first else {
            return;
        };
        self.sidebar_mutation(RemoteSidebarOrganizationMutation::CreateGroup {
            name,
            project_id,
            workspace_id,
            member_session_ids: members.to_vec(),
            parent_folder_id: None,
        });
    }

    fn sidebar_mutation(&self, mutation: RemoteSidebarOrganizationMutation) {
        self.sidebar_mutation_attempt(mutation, 1, None);
    }

    fn sidebar_mutation_attempt(
        &self,
        mutation: RemoteSidebarOrganizationMutation,
        retries: u8,
        done: Option<Rc<dyn Fn()>>,
    ) {
        let revision = matches!(
            mutation,
            RemoteSidebarOrganizationMutation::MoveItems { .. }
        )
        .then(|| self.sidebar_snapshot.borrow().revision);
        let service = self.clone();
        // Under an owner of its own: the call comes from a row of a list that
        // may be rebuilt at any moment, and a task spawned under that row's
        // owner is cancelled with it before its first poll.
        Owner::detached().run(move || {
            spawn(async move {
                match service.submit_sidebar(mutation.clone(), revision).await {
                    Ok(()) => {
                        if let Some(done) = done.as_ref() {
                            done();
                        }
                    }
                    Err(error)
                        if retries > 0
                            && (error.kind == BackendErrorKind::Conflict
                                || error.code.contains("stale_revision")) =>
                    {
                        if service.reload_sidebar().await.is_ok() {
                            service.sidebar_mutation_attempt(mutation, retries - 1, done);
                        } else {
                            service.fail(&error);
                        }
                    }
                    Err(error) => service.fail(&error),
                }
            });
        });
    }

    /// Archives the session the actions sheet is open on.
    fn archive_open_session(&self) {
        let Some(id) = self.session_actions.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(id.as_str()) else {
            return;
        };
        self.session_busy.set(true);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .archive_session(MutationRequest::new(session_id))
                        .await
                })
                .await;
            service.session_busy.set(false);
            match result {
                Ok(Ok(())) => {
                    service.drop_session(&id);
                    service.session_actions.set(None);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_archive_task_failed",
                    format!("archiving the session did not finish: {error}"),
                )),
            }
        });
    }

    /// Deletes the session the actions sheet is open on, after confirming it.
    fn delete_open_session(&self) {
        let Some(id) = self.session_actions.get_untracked() else {
            return;
        };
        let Ok(session_id) = VibexSessionId::parse(id.as_str()) else {
            return;
        };
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let answer = nsh_base::alert_dialog()
                .title(copy::text(
                    "Delete this session?",
                    "删除这个会话？",
                    "刪除這個工作階段？",
                ))
                .description(copy::text(
                    "Its transcript leaves this runtime. This cannot be undone.",
                    "它的对话记录会从运行时删除，且无法撤销。",
                    "它的對話記錄會從執行環境刪除，且無法復原。",
                ))
                .cancel("cancel", copy::text("Cancel", "取消", "取消"))
                .destructive("delete", copy::text("Delete", "删除", "刪除"))
                .show()
                .await;
            if !matches!(&answer, Ok(answer) if answer.action.as_deref() == Some("delete")) {
                return;
            }
            service.session_busy.set(true);
            let result = crate::runtime::handle()
                .spawn(async move {
                    backend
                        .delete_session(MutationRequest::new(session_id))
                        .await
                })
                .await;
            service.session_busy.set(false);
            match result {
                Ok(Ok(())) => {
                    service.drop_session(&id);
                    service.session_actions.set(None);
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_delete_task_failed",
                    format!("deleting the session did not finish: {error}"),
                )),
            }
        });
    }

    /// Removes a session row and opens the next one when it was the active.
    fn drop_session(&self, id: &str) {
        self.sessions
            .update(|sessions| sessions.retain(|s| s.id != id));
        self.session_records
            .update(|sessions| sessions.retain(|session| session.id.as_str() != id));
        self.drafts.borrow_mut().remove(id);
        self.attachment_drafts.borrow_mut().remove(id);
        self.persist_drafts();
        self.pinned
            .update(|pinned| pinned.retain(|candidate| candidate != id));
        if let Ok(text) = serde_json::to_string(&self.pinned.get_untracked()) {
            let _ = Storage::set_item(PINS_KEY, text);
        }
        if self.active.get_untracked().as_deref() != Some(id) {
            return;
        }
        match self.sessions.get_untracked().first().map(|s| s.id.clone()) {
            Some(next) => {
                // The removed session is not the one whose draft the next
                // open should save; clear the pointer first.
                self.active.set(None);
                self.open(&next);
            }
            None => {
                self.active.set(None);
                self.timeline.set(Vec::new());
            }
        }
    }

    /// Starts writing a new session, with the runtime's choices.
    pub fn open_new_session(&self) {
        let workspace = self.session_records.with(|sessions| {
            let active = self.active.get_untracked()?;
            sessions
                .iter()
                .find(|session| session.id.as_str() == active)
                .map(|session| session.workspace_id.as_str().to_owned())
        });
        self.open_new_session_in(workspace, None, None);
    }

    fn open_new_session_in(
        &self,
        workspace: Option<String>,
        project: Option<String>,
        folder: Option<String>,
    ) {
        if self.new_busy.get_untracked() {
            return;
        }
        // A session already being written keeps what was written: asking
        // again only points it at the place that was asked for.
        let writing = self.new_session_open.get_untracked();
        self.new_session_open.set(true);
        self.new_session_folder.set(folder);
        self.new_error.set(None);
        if !writing {
            self.new_title.set(String::new());
            self.new_prompt.set(String::new());
            self.new_attachments.set(Vec::new());
            self.new_worktree_branch.set(String::new());
            self.new_base_ref.set(String::new());
        }
        self.new_workspaces.set(Vec::new());
        self.new_runtimes.set(Vec::new());
        self.new_workspace.set(None);
        self.new_runtime.set(None);
        self.new_runtime_options.borrow_mut().clear();
        self.new_busy.set(true);
        let service = self.clone();
        let workspaces = self.workspace_backend.clone();
        let catalog_backend = self.backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    let spaces = workspaces.list_workspaces().await?;
                    let catalog = catalog_backend.list_runtime_options().await?;
                    Ok::<_, BackendError>((spaces, catalog))
                })
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok((spaces, catalog))) => {
                    let workspace_choices = spaces
                        .iter()
                        .map(|space| WorkspaceChoice {
                            id: space.workspace.id.as_str().to_owned(),
                            name: space.project.name.clone(),
                            detail: space.workspace.root_path.clone(),
                        })
                        .collect::<Vec<_>>();
                    let available = catalog
                        .options
                        .into_iter()
                        .filter(|option| {
                            option.availability == RuntimeOptionAvailability::Available
                        })
                        .collect::<Vec<_>>();
                    let runtime_choices = available
                        .iter()
                        .enumerate()
                        .map(|(index, option)| RuntimeChoice {
                            id: index.to_string(),
                            title: format!("{} · {}", option.agent_label, option.model_label),
                            detail: option.auth_source_label.clone(),
                        })
                        .collect::<Vec<_>>();
                    if runtime_choices.is_empty() {
                        service.new_error.set(Some(
                            copy::text(
                                "No ready Agent is available on this runtime.",
                                "该运行时没有就绪的 Agent。",
                                "該執行環境沒有就緒的 Agent。",
                            )
                            .into(),
                        ));
                    }
                    let project_workspace = project.as_deref().and_then(|project| {
                        spaces
                            .iter()
                            .filter(|space| space.project.id.as_str() == project)
                            .min_by_key(|space| {
                                space.workspace.mode != vibex_core::WorkspaceMode::CurrentCheckout
                            })
                            .map(|space| space.workspace.id.as_str().to_owned())
                    });
                    service.new_workspace.set(
                        workspace
                            .or(project_workspace)
                            .filter(|id| workspace_choices.iter().any(|choice| &choice.id == id))
                            .or_else(|| workspace_choices.first().map(|choice| choice.id.clone())),
                    );
                    let preferred = service.runtime_state.borrow().as_ref().and_then(|state| {
                        available
                            .iter()
                            .position(|option| option.selection == state.desired)
                            .map(|index| index.to_string())
                    });
                    service.new_runtime.set(
                        preferred
                            .or_else(|| runtime_choices.first().map(|choice| choice.id.clone())),
                    );
                    service.new_workspaces.set(workspace_choices);
                    service.new_runtimes.set(runtime_choices);
                    service.workspace_records.set(spaces);
                    *service.new_runtime_options.borrow_mut() = available;
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("loading the new-session choices did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Creates the session the sheet describes.
    fn create_new_session(&self) {
        if self.new_busy.get_untracked() {
            return;
        }
        let Some(workspace_id) = self.new_workspace.get_untracked() else {
            return;
        };
        let Some(runtime_id) = self.new_runtime.get_untracked() else {
            return;
        };
        let Ok(index) = runtime_id.parse::<usize>() else {
            return;
        };
        let workspace = self.workspace_records.with(|workspaces| {
            workspaces
                .iter()
                .find(|space| space.workspace.id.as_str() == workspace_id)
                .cloned()
        });
        let option = self.new_runtime_options.borrow().get(index).cloned();
        let (Some(workspace), Some(option)) = (workspace, option) else {
            return;
        };
        self.new_busy.set(true);
        self.new_error.set(None);
        let draft = self.new_session_draft();
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request = CreateAgentSessionRequest {
                runtime: option.selection,
                workspace_root: workspace.workspace.root_path.clone(),
                workspace_mode: workspace.workspace.mode,
                title: draft.title.clone(),
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result = crate::runtime::handle()
                .spawn(async move { backend.create_session(MutationRequest::new(request)).await })
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    service.finish_new_session(session, draft);
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("creating the session did not finish: {error}").into(),
                )),
            }
        });
    }

    fn new_session_draft(&self) -> NewSessionDraft {
        let title = self.new_title.get_untracked().trim().to_owned();
        NewSessionDraft {
            title: (!title.is_empty()).then_some(title),
            text: self.new_prompt.get_untracked().trim().to_owned(),
            attachments: self.new_attachments.get_untracked(),
        }
    }

    fn finish_new_session(&self, session: AgentSession, draft: NewSessionDraft) {
        if !self.alive.get() {
            return;
        }
        self.new_session_open.set(false);
        self.record_sidebar_session(&session);
        if let Some(folder) = self.new_session_folder.get_untracked()
            && self.sidebar_sections.with(|sections| {
                sections.iter().any(|section| {
                    section.kind == SidebarSectionKind::Folder
                        && section.id == folder
                        && section
                            .project_id
                            .as_deref()
                            .is_none_or(|project| project == session.project_id.as_str())
                        && section
                            .workspace_id
                            .as_deref()
                            .is_none_or(|workspace| workspace == session.workspace_id.as_str())
                })
            })
        {
            self.sidebar_mutation(RemoteSidebarOrganizationMutation::MoveItems {
                items: vec![vibex_core::RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Session,
                    id: session.id.as_str().to_owned(),
                }],
                anchor: Some(vibex_core::RemoteSidebarItemRef {
                    kind: RemoteSidebarItemKind::Folder,
                    id: folder,
                }),
                position: vibex_core::RemoteSidebarDropPosition::Into,
                project_id: Some(session.project_id.as_str().to_owned()),
            });
        }
        self.new_session_folder.set(None);
        self.refresh();
        self.open(session.id.as_str());
        if !draft.text.is_empty() || !draft.attachments.is_empty() {
            self.deliver(draft.text, draft.attachments, Vec::new(), false);
        }
    }

    fn browse_project_dirs(&self, path: Option<String>) {
        if self.project_busy.get_untracked() {
            return;
        }
        self.project_busy.set(true);
        self.project_error.set(None);
        let service = self.clone();
        let workspaces = self.workspace_backend.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move { workspaces.browse_authority_directories(path).await })
                .await;
            if !service.alive.get() {
                return;
            }
            service.project_busy.set(false);
            match result {
                Ok(Ok(listing)) => {
                    service.project_dirs.set(
                        listing
                            .entries
                            .into_iter()
                            .map(|entry| DirectoryEntry {
                                name: entry.name,
                                path: entry.path,
                            })
                            .collect(),
                    );
                    service.project_dir_path.set(Some(listing.path));
                    service.project_dir_parent.set(listing.parent);
                }
                Ok(Err(error)) => service.project_error.set(Some(error_text(&error))),
                Err(error) => service.project_error.set(Some(
                    format!("browsing the directories did not finish: {error}").into(),
                )),
            }
        });
    }

    fn open_project(&self, path: &str) {
        if path.trim().is_empty() || self.project_busy.get_untracked() {
            return;
        }
        self.project_busy.set(true);
        self.project_error.set(None);
        let path = path.to_owned();
        let service = self.clone();
        let workspaces = self.workspace_backend.clone();
        spawn(async move {
            let opened = crate::runtime::handle()
                .spawn(async move {
                    workspaces
                        .open_workspace(MutationRequest::new(OpenWorkspaceRequest {
                            root_path: path,
                            mode: None,
                        }))
                        .await
                })
                .await;
            if !service.alive.get() {
                return;
            }
            service.project_busy.set(false);
            match opened {
                Ok(Ok(workspace)) => {
                    service.workspace_records.update(|spaces| {
                        if let Some(existing) = spaces
                            .iter_mut()
                            .find(|space| space.workspace.id == workspace.workspace.id)
                        {
                            *existing = workspace;
                        } else {
                            spaces.push(workspace);
                        }
                    });
                    service.project_browser_open.set(false);
                }
                Ok(Err(error)) => {
                    service.project_error.set(Some(error_text(&error)));
                }
                Err(error) => {
                    service.project_error.set(Some(
                        format!("opening the project did not finish: {error}").into(),
                    ));
                }
            }
        });
    }

    fn create_temporary_session(&self) {
        if self.new_busy.get_untracked() {
            return;
        }
        let Some(runtime_id) = self.new_runtime.get_untracked() else {
            return;
        };
        let Ok(index) = runtime_id.parse::<usize>() else {
            return;
        };
        let option = self.new_runtime_options.borrow().get(index).cloned();
        let Some(option) = option else {
            return;
        };
        self.new_busy.set(true);
        self.new_error.set(None);
        let draft = self.new_session_draft();
        let service = self.clone();
        let backend = self.backend.clone();
        let workspaces = self.workspace_backend.clone();
        spawn(async move {
            let root = crate::runtime::handle()
                .spawn(async move { workspaces.ensure_temporary_session_root().await })
                .await;
            let root = match root {
                Ok(Ok(root)) => root,
                Ok(Err(error)) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(error_text(&error)));
                    return;
                }
                Err(error) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(
                        format!("creating the temporary root did not finish: {error}").into(),
                    ));
                    return;
                }
            };
            let request = CreateAgentSessionRequest {
                runtime: option.selection,
                workspace_root: root,
                workspace_mode: vibex_core::WorkspaceMode::CurrentCheckout,
                title: draft.title.clone(),
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result = crate::runtime::handle()
                .spawn(async move { backend.create_session(MutationRequest::new(request)).await })
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    service.finish_new_session(session, draft);
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("creating the session did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Opens the workspace's terminal full-screen over the shell.
    fn open_terminal(&self) {
        if self.terminal_open.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            self.fail_text(copy::text(
                "Open a session first; its workspace owns the terminal.",
                "请先打开一个会话，终端属于它的工作区。",
                "請先開啟一個工作階段，終端屬於它的工作區。",
            ));
            return;
        };
        let Ok(active_id) = VibexSessionId::parse(active.as_str()) else {
            return;
        };
        self.terminal_open.set(true);
        self.terminal_busy.set(true);
        self.terminal_error.set(None);
        self.terminal_title
            .set(Some(copy::text("Terminal", "终端", "終端").into()));
        let service = self.clone();
        let backend = self.backend.clone();
        let terminal_backend = self.terminal_backend.clone();
        let opening = terminal_backend.clone();
        let generation = self.terminal_generation.get();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(async move {
                    let session = backend.open_session(active_id).await?;
                    let terminals = if session.is_terminal() {
                        Vec::new()
                    } else {
                        opening.list_terminals(session.workspace_id.clone()).await?
                    };
                    Ok::<_, BackendError>((session, terminals))
                })
                .await;
            if !service.alive.get() || service.terminal_generation.get() != generation {
                return;
            }
            service.terminal_busy.set(false);
            let (session, terminals) = match result {
                Ok(Ok(opened)) => opened,
                Ok(Err(error)) => {
                    service.terminal_error.set(Some(error_text(&error)));
                    return;
                }
                Err(error) => {
                    service
                        .terminal_error
                        .set(Some(format!("the terminal did not open: {error}").into()));
                    return;
                }
            };
            if session.is_terminal() {
                if let Some(id) = session.terminal.and_then(|facts| facts.terminal_id) {
                    service.attach_terminal(id, session.title);
                } else {
                    service.terminal_error.set(Some(
                        copy::text(
                            "Waiting for the session's terminal…",
                            "正在等待会话终端…",
                            "正在等待工作階段的終端機…",
                        )
                        .into(),
                    ));
                }
                return;
            }
            service.set_terminal_choices(&terminals);
            if let Some(terminal) = terminals
                .iter()
                .find(|terminal| terminal.status == TerminalStatus::Running)
            {
                service.attach_terminal(terminal.id.clone(), terminal.title.clone());
                return;
            }
            let workspace = session.workspace_id;
            let (cols, rows) = service.terminal_size.get();
            service.terminal_busy.set(true);
            let created = crate::runtime::handle()
                .spawn(async move {
                    terminal_backend
                        .create_terminal(MutationRequest::new(TerminalCreateRequest {
                            workspace_id: workspace,
                            title: None,
                            shell: None,
                            cwd: None,
                            rows,
                            cols,
                        }))
                        .await
                })
                .await;
            if !service.alive.get() || service.terminal_generation.get() != generation {
                return;
            }
            service.terminal_busy.set(false);
            match created {
                Ok(Ok(terminal)) => {
                    service
                        .terminal_choices
                        .update(|choices| push_terminal_choice(choices, &terminal));
                    service.attach_terminal(terminal.id, terminal.title);
                }
                Ok(Err(error)) => service.terminal_error.set(Some(error_text(&error))),
                Err(error) => service.terminal_error.set(Some(
                    format!("creating the terminal did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Shows a terminal's current screen and follows it.
    fn attach_terminal(&self, id: TerminalId, title: String) {
        self.stop_terminal_tasks();
        let generation = self.terminal_generation.get();
        self.reset_terminal_output();
        self.terminal_id.set(Some(id.as_str().to_owned()));
        self.terminal_title.set(Some(title.into()));
        self.terminal_error.set(None);
        self.terminal_busy.set(false);
        let service = self.clone();
        let backend = self.terminal_backend.clone();
        let stream_id = id.clone();
        let (output, mut receive) = mpsc::channel(32);
        let stream =
            spawn_terminal_stream(move || backend.subscribe_terminal(stream_id, 1), output);
        self.terminal_stream.replace(Some(stream.abort_handle()));
        let (sender, mut commands) = mpsc::channel(128);
        let backend = self.terminal_backend.clone();
        let (errors, mut command_errors) = mpsc::channel(8);
        let writer = crate::runtime::handle().spawn(async move {
            while let Some(command) = commands.recv().await {
                let result = match command {
                    TerminalCommand::Input(data) => {
                        backend
                            .write_terminal(MutationRequest::new(TerminalWriteRequest {
                                terminal_id: id.clone(),
                                data,
                            }))
                            .await
                    }
                    TerminalCommand::Resize(cols, rows) => backend
                        .resize_terminal(MutationRequest::new(TerminalResizeRequest {
                            terminal_id: id.clone(),
                            cols,
                            rows,
                        }))
                        .await
                        .map(|_| ()),
                };
                if let Err(error) = result
                    && errors.send(error).await.is_err()
                {
                    break;
                }
            }
        });
        self.terminal_writer.replace(Some(writer.abort_handle()));
        self.terminal_tx.replace(Some(sender));
        let (cols, rows) = self.terminal_size.get();
        self.queue_terminal_command(TerminalCommand::Resize(cols, rows));
        spawn({
            let service = self.clone();
            async move {
                while let Some(error) = command_errors.recv().await {
                    if service.terminal_generation.get() != generation {
                        break;
                    }
                    service.terminal_error.set(Some(error_text(&error)));
                }
            }
        });
        spawn(async move {
            while let Some(batch) = receive.recv().await {
                if service.terminal_generation.get() != generation {
                    return;
                }
                while service.terminal_ref.api::<TerminalApi>().is_none() {
                    timer(Duration::from_millis(16)).await;
                    if service.terminal_generation.get() != generation {
                        return;
                    }
                }
                if batch.reset_required {
                    service.reset_terminal_output();
                }
                for frame in batch.frames {
                    let text = service.decode_terminal_frame(&frame.bytes);
                    service.write_terminal_output(text);
                }
                service
                    .terminal_output_revision
                    .update(|revision| *revision = revision.wrapping_add(1));
            }
            let result = stream.await;
            if service.terminal_generation.get() == generation {
                service.terminal_error.set(Some(match result {
                    Ok(Err(error)) => error_text(&error),
                    Err(error) => format!("reading the terminal did not finish: {error}").into(),
                    Ok(Ok(())) => copy::text(
                        "Terminal disconnected",
                        "终端连接已断开",
                        "終端機已中斷連線",
                    )
                    .into(),
                }));
            }
        });
    }

    /// Starts another terminal in the workspace and shows it.
    fn new_terminal(&self) {
        if self.terminal_busy.get_untracked() {
            return;
        }
        self.terminal_busy.set(true);
        let generation = self.terminal_generation.get();
        let (cols, rows) = self.terminal_size.get();
        let service = self.clone();
        let terminal_backend = self.terminal_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                if service.terminal_generation.get() == generation {
                    service.terminal_busy.set(false);
                }
                return;
            };
            let created = crate::runtime::handle()
                .spawn(async move {
                    terminal_backend
                        .create_terminal(MutationRequest::new(TerminalCreateRequest {
                            workspace_id: workspace,
                            title: None,
                            shell: None,
                            cwd: None,
                            rows,
                            cols,
                        }))
                        .await
                })
                .await;
            if service.terminal_generation.get() != generation {
                return;
            }
            service.terminal_busy.set(false);
            match created {
                Ok(Ok(terminal)) => {
                    service
                        .terminal_choices
                        .update(|choices| push_terminal_choice(choices, &terminal));
                    service.attach_terminal(terminal.id, terminal.title);
                }
                Ok(Err(error)) => service.terminal_error.set(Some(error_text(&error))),
                Err(error) => service.terminal_error.set(Some(
                    format!("creating the terminal did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Shows another of the workspace's terminals.
    fn select_terminal(&self, id: &str) {
        if self.terminal_busy.get_untracked()
            || self.terminal_id.get_untracked().as_deref() == Some(id)
                && self.terminal_error.with(Option::is_none)
        {
            return;
        }
        let Ok(id) = TerminalId::parse(id) else {
            return;
        };
        let title = self
            .terminal_choices
            .with(|choices| {
                choices
                    .iter()
                    .find(|choice| choice.id == id.as_str())
                    .map(|choice| choice.title.clone())
            })
            .unwrap_or_else(|| copy::text("Terminal", "终端", "終端").to_owned());
        self.attach_terminal(id, title);
    }

    fn retry_terminal(&self) {
        if let Some(id) = self.terminal_id.get_untracked() {
            if let Ok(id) = TerminalId::parse(id) {
                let title = self.terminal_title.get_untracked().unwrap_or_default();
                self.attach_terminal(id, title.to_string());
            }
        } else {
            self.close_terminal();
            self.open_terminal();
        }
    }

    fn kill_terminal(&self, id: &str) {
        if self.terminal_busy.get_untracked() {
            return;
        }
        let Ok(id) = TerminalId::parse(id) else {
            return;
        };
        self.terminal_busy.set(true);
        let generation = self.terminal_generation.get();
        let service = self.clone();
        let backend = self.terminal_backend.clone();
        spawn(async move {
            let target = id.clone();
            let result = crate::runtime::handle()
                .spawn(async move { backend.close_terminal(MutationRequest::new(target)).await })
                .await;
            if service.terminal_generation.get() != generation {
                return;
            }
            service.terminal_busy.set(false);
            match result {
                Ok(Ok(_)) => {
                    service
                        .terminal_choices
                        .update(|choices| choices.retain(|choice| choice.id != id.as_str()));
                    if service.terminal_id.get_untracked().as_deref() == Some(id.as_str()) {
                        service.stop_terminal_tasks();
                        service.terminal_id.set(None);
                        service.terminal_error.set(None);
                        service.reset_terminal_output();
                        if let Some(next) = service.terminal_choices.with(|choices| {
                            choices
                                .iter()
                                .find(|choice| choice.status == "running")
                                .map(|choice| choice.id.clone())
                        }) {
                            service.select_terminal(&next);
                        }
                    }
                }
                Ok(Err(error)) => service.terminal_error.set(Some(error_text(&error))),
                Err(error) => service.terminal_error.set(Some(
                    format!("closing the terminal did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Replaces the terminal list from a backend listing.
    fn set_terminal_choices(&self, terminals: &[vibex_core::TerminalSession]) {
        let choices = terminals.iter().map(terminal_choice).collect::<Vec<_>>();
        self.terminal_choices.set(choices);
    }

    /// Closes the terminal surface; the PTY keeps running on the runtime.
    fn close_terminal(&self) {
        self.stop_terminal_tasks();
        self.terminal_open.set(false);
        self.terminal_busy.set(false);
        self.terminal_id.set(None);
    }

    fn stop_terminal_tasks(&self) {
        self.terminal_generation
            .set(self.terminal_generation.get().wrapping_add(1));
        self.terminal_tx.take();
        for task in [&self.terminal_stream, &self.terminal_writer] {
            if let Some(task) = task.take() {
                task.abort();
            }
        }
    }

    /// Sends what the person typed to the PTY.
    fn terminal_input(&self, data: &str) {
        if !data.is_empty() {
            self.queue_terminal_command(TerminalCommand::Input(data.to_owned()));
        }
    }

    fn queue_terminal_command(&self, command: TerminalCommand) {
        if let Some(sender) = self.terminal_tx.borrow().as_ref()
            && sender.try_send(command).is_err()
        {
            self.terminal_error.set(Some(
                copy::text("Terminal input is busy", "终端输入繁忙", "終端機輸入忙碌").into(),
            ));
        }
    }

    fn write_terminal_output(&self, text: String) {
        if let Some(api) = self.terminal_ref.api::<TerminalApi>() {
            api.write(text);
        }
    }

    fn reset_terminal_output(&self) {
        self.terminal_pending.borrow_mut().clear();
        if let Some(api) = self.terminal_ref.api::<TerminalApi>() {
            api.reset();
        }
        self.terminal_output_revision
            .update(|revision| *revision = revision.wrapping_add(1));
    }

    /// Decodes a frame, keeping a character split across frames for the next
    /// one instead of writing a replacement mark.
    fn decode_terminal_frame(&self, bytes: &[u8]) -> String {
        decode_terminal_bytes(&mut self.terminal_pending.borrow_mut(), bytes)
    }

    /// Tells the runtime the grid's new size.
    fn terminal_resize(&self, cols: u32, rows: u32) {
        if !self.terminal_visible.get_untracked() || cols <= 1 || rows <= 1 {
            return;
        }
        let size = (
            cols.min(u32::from(u16::MAX)) as u16,
            rows.min(u32::from(u16::MAX)) as u16,
        );
        if self.terminal_size.replace(size) != size {
            self.queue_terminal_command(TerminalCommand::Resize(size.0, size.1));
        }
    }

    fn open_files(&self) {
        if self.files_open.get_untracked() {
            return;
        }
        self.files_open.set(true);
        self.files_preview.set(None);
        self.files_error.set(None);
        self.load_tree(String::new());
    }

    fn search_files(&self, query: &str, options: SearchOptions) {
        let run = self.file_search_run.get().wrapping_add(1);
        self.file_search_run.set(run);
        let query = query.trim().to_owned();
        self.files_error.set(None);
        self.files_searching.set(!query.is_empty());
        self.files_search.set(Vec::new());
        if query.is_empty() {
            return;
        }
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            timer(Duration::from_millis(180)).await;
            if service.file_search_run.get() != run {
                return;
            }
            let Some(workspace) = service.workspace(&agent).await else {
                if service.file_search_run.get() == run {
                    service.files_searching.set(false);
                }
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move {
                    file_backend
                        .search_files(FileSearchRequest {
                            workspace_id: workspace,
                            query,
                            include_content: options.contents,
                            case_sensitive: options.case_sensitive,
                            whole_word: options.whole_word,
                            regex: options.regex,
                            limit: Some(50),
                        })
                        .await
                })
                .await;
            if !service.alive.get() || service.file_search_run.get() != run {
                return;
            }
            service.files_searching.set(false);
            match result {
                Ok(Ok(results)) => service.files_search.set(
                    results
                        .into_iter()
                        .map(|result| FileSearchRow {
                            path: result.path,
                            name: result.name,
                            line: result.line,
                            snippet: result.snippet.unwrap_or_default(),
                        })
                        .collect(),
                ),
                Ok(Err(error)) => service.files_error.set(Some(error_text(&error))),
                Err(error) => service.files_error.set(Some(
                    format!("searching the files did not finish: {error}").into(),
                )),
            }
        });
    }

    fn toggle_directory(&self, path: &str) {
        let load = {
            let mut tree = self.file_tree.borrow_mut();
            if !tree.toggle_expanded(path) {
                return;
            }
            tree.is_expanded(path)
                && matches!(
                    tree.load_state(path),
                    FileTreeLoadState::Unloaded | FileTreeLoadState::Error { .. }
                )
        };
        self.publish_file_tree();
        if load {
            self.load_tree(path.to_owned());
        }
    }

    fn publish_file_tree(&self) {
        self.files_entries.set(
            self.file_tree
                .borrow()
                .all_visible_rows()
                .iter()
                .filter(|row| !row.path.is_empty())
                .map(|row| FileEntry {
                    path: row.path.clone(),
                    name: row.name.clone(),
                    directory: row.kind == vibex_core::FileEntryKind::Directory,
                    depth: row.depth.saturating_sub(1),
                    expanded: row.expanded,
                    loading: row.load_state == FileTreeLoadState::Loading,
                    git: row.git.map(|git| git.signal.short_label().to_owned()),
                    ignored: row.ignored,
                    icon: match row.icon.kind {
                        FileIconKind::Code
                        | FileIconKind::Rust
                        | FileIconKind::Java
                        | FileIconKind::TypeScript
                        | FileIconKind::JavaScript
                        | FileIconKind::Markup => FileGlyph::Code,
                        FileIconKind::Markdown => FileGlyph::Markdown,
                        FileIconKind::Script => FileGlyph::Script,
                        FileIconKind::Database => FileGlyph::Database,
                        FileIconKind::Style => FileGlyph::Style,
                        FileIconKind::Image | FileIconKind::Svg => FileGlyph::Image,
                        FileIconKind::Json | FileIconKind::Spreadsheet => FileGlyph::Data,
                        FileIconKind::Archive => FileGlyph::Archive,
                        _ => FileGlyph::Text,
                    },
                })
                .collect(),
        );
    }

    fn load_tree(&self, path: String) {
        self.files_error.set(None);
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            if !service.workbench_is_current(generation) {
                return;
            }
            let Some(workspace) = service.workspace(&agent).await else {
                if !service.workbench_is_current(generation) {
                    return;
                }
                service.files_error.set(Some(
                    copy::text(
                        "Open a session first; the files belong to its workspace.",
                        "请先打开一个会话，文件属于它的工作区。",
                        "請先開啟一個工作階段，檔案屬於它的工作區。",
                    )
                    .into(),
                ));
                return;
            };
            if !service.workbench_is_current(generation) {
                return;
            }
            let run = {
                let mut tree = service.file_tree.borrow_mut();
                if tree.workspace_id() != Some(&workspace) {
                    tree.reset_workspace(workspace.clone());
                    let name = service.workspace_records.with(|spaces| {
                        spaces
                            .iter()
                            .find(|space| space.workspace.id == workspace)
                            .map(|space| space.project.name.clone())
                    });
                    if let Some(name) = name {
                        tree.set_root_name(name);
                    }
                }
                tree.begin_load(&path)
            };
            service.publish_file_tree();
            let tree_path = (!path.is_empty()).then(|| path.clone());
            let requested_workspace = workspace.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    file_backend
                        .file_tree(FileTreeRequest {
                            workspace_id: requested_workspace,
                            path: tree_path,
                            max_depth: Some(1),
                            include_hidden: false,
                        })
                        .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            let reload = match result {
                Ok(Ok(entries)) => service
                    .file_tree
                    .borrow_mut()
                    .apply_entries(&workspace, run, &path, entries),
                failed => {
                    if service
                        .file_tree
                        .borrow_mut()
                        .fail_load(run, &path, "file_tree_load_failed")
                    {
                        let error = match failed {
                            Ok(Err(error)) => error_text(&error),
                            Err(error) => {
                                format!("listing the files did not finish: {error}").into()
                            }
                            Ok(Ok(_)) => unreachable!(),
                        };
                        service.files_error.set(Some(error));
                    }
                    false
                }
            };
            service.publish_file_tree();
            if reload {
                let mut dirs = service.file_tree.borrow().expanded_directory_paths();
                dirs.retain(|directory| {
                    directory.rsplit_once('/').map_or("", |(parent, _)| parent) == path
                        && service.file_tree.borrow().load_state(directory)
                            == FileTreeLoadState::Unloaded
                });
                for dir in dirs {
                    service.load_tree(dir);
                }
            }
        });
    }

    /// Reads a file into the preview.
    fn open_file(&self, path: String, line: Option<u32>) {
        let run = self.file_read_run.get().wrapping_add(1);
        self.file_read_run.set(run);
        self.files_error.set(None);
        self.files_preview.set(None);
        self.files_path.set(Some(path.clone()));
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                if service.file_read_run.get() == run {
                    service.files_error.set(Some(
                        copy::text(
                            "The workspace is unavailable",
                            "工作区不可用",
                            "工作區無法使用",
                        )
                        .into(),
                    ));
                }
                return;
            };
            let read_workspace = workspace.clone();
            let root = service.workspace_records.with_untracked(|spaces| {
                spaces
                    .iter()
                    .find(|space| space.workspace.id == workspace)
                    .map(|space| space.workspace.root_path.clone())
            });
            let path = workspace_file_path(&path, root.as_deref());
            let result = crate::runtime::handle()
                .spawn(async move {
                    file_backend
                        .read_file(FileReadRequest {
                            workspace_id: read_workspace,
                            path,
                            max_bytes: Some(512 * 1024),
                        })
                        .await
                })
                .await;
            if !service.alive.get() || service.file_read_run.get() != run {
                return;
            }
            match result {
                Ok(Ok(file)) => {
                    let kind = match file.preview_kind {
                        FilePreviewKind::Markdown => FilePreviewKindView::Markdown,
                        FilePreviewKind::Image => FilePreviewKindView::Image,
                        FilePreviewKind::Binary => FilePreviewKindView::Binary,
                        FilePreviewKind::Text => FilePreviewKindView::Text,
                    };
                    let image_uri = if kind == FilePreviewKindView::Image {
                        let image_backend = service.file_backend.clone();
                        let image_path = file.path.clone();
                        let image_workspace = workspace;
                        crate::runtime::handle()
                            .spawn(async move {
                                image_backend
                                    .read_file_bytes(image_workspace, image_path, 8 * 1024 * 1024)
                                    .await
                            })
                            .await
                            .ok()
                            .and_then(Result::ok)
                            .map(|bytes| {
                                format!(
                                    "data:{};base64,{}",
                                    image_mime(&file.name),
                                    base64::engine::general_purpose::STANDARD.encode(bytes)
                                )
                            })
                    } else {
                        None
                    };
                    if !service.alive.get() || service.file_read_run.get() != run {
                        return;
                    }
                    if kind == FilePreviewKindView::Image && image_uri.is_none() {
                        service.files_error.set(Some(
                            copy::text(
                                "The image could not be loaded",
                                "无法加载图片",
                                "無法載入圖片",
                            )
                            .into(),
                        ));
                        return;
                    }
                    service.files_preview.set(Some(FilePreview {
                        title: file.name,
                        body: file.content.unwrap_or_default(),
                        truncated: file.truncated,
                        language: file.language.unwrap_or_default(),
                        kind,
                        image_uri,
                        line,
                    }));
                }
                Ok(Err(error)) => service.files_error.set(Some(error_text(&error))),
                Err(error) => service.files_error.set(Some(
                    format!("reading the file did not finish: {error}").into(),
                )),
            }
        });
    }

    fn files_back(&self) {
        self.file_read_run
            .set(self.file_read_run.get().wrapping_add(1));
        self.files_preview.set(None);
        self.files_path.set(None);
        self.files_error.set(None);
    }

    fn workbench_is_current(&self, generation: u64) -> bool {
        self.alive.get()
            && self.workbench_generation.get() == generation
            && self.active.get_untracked().is_some()
    }

    /// The open session's workspace, fetched once.
    async fn workspace(&self, agent: &Arc<dyn AgentBackend>) -> Option<WorkspaceId> {
        if let Some(workspace) = self.files_workspace.borrow().clone() {
            return Some(workspace);
        }
        let active = self.active.get_untracked()?;
        let session_id = VibexSessionId::parse(active.as_str()).ok()?;
        if let Some(workspace) = self.session_records.with(|sessions| {
            sessions
                .iter()
                .find(|session| session.id == session_id)
                .map(|session| session.workspace_id.clone())
        }) {
            *self.files_workspace.borrow_mut() = Some(workspace.clone());
            return Some(workspace);
        }
        let agent = agent.clone();
        let session = crate::runtime::handle()
            .spawn(async move { agent.open_session(session_id).await })
            .await
            .ok()?
            .ok()?;
        if self.active.get_untracked().as_deref() != Some(active.as_str()) {
            return None;
        }
        let workspace = session.workspace_id;
        *self.files_workspace.borrow_mut() = Some(workspace.clone());
        Some(workspace)
    }

    /// Opens the workspace's Git status over the shell.
    fn open_git(&self) {
        self.git_open.set(true);
        self.git_diff.set(None);
        self.git_amend.set(false);
        self.git_push_after.set(false);
        self.load_git_branches();
        self.refresh_git();
    }

    /// Reloads the status list.
    fn refresh_git(&self) {
        self.git_error.set(None);
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            if !service.workbench_is_current(generation) {
                return;
            }
            let Some(workspace) = service.workspace(&agent).await else {
                if !service.workbench_is_current(generation) {
                    return;
                }
                service.git_error.set(Some(
                    copy::text(
                        "Open a session first; Git belongs to its workspace.",
                        "请先打开一个会话，Git 属于它的工作区。",
                        "請先開啟一個工作階段，Git 屬於它的工作區。",
                    )
                    .into(),
                ));
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move { git.git_status(workspace).await })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(status)) => service.apply_git_status(status),
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reading Git status did not finish: {error}").into(),
                )),
            }
        });
    }

    fn apply_git_status(&self, status: GitStatusSummary) {
        let mut model = self.git_model.borrow_mut();
        if model.workspace_id() != Some(&status.workspace_id) {
            model.reset_workspace(status.workspace_id.clone());
        }
        let Some(ticket) = model.begin_query(GitQueryKind::Status, "status") else {
            return;
        };
        self.file_tree.borrow_mut().set_git_changes(&status.changes);
        self.publish_file_tree();
        self.git_totals.set(status.changes.iter().fold(
            (0_u32, 0_u32),
            |(added, removed), change| {
                (
                    added.saturating_add(change.additions),
                    removed.saturating_add(change.deletions),
                )
            },
        ));
        self.git_branch.set(
            status
                .branch
                .clone()
                .map(SharedString::from)
                .map(Some)
                .unwrap_or(None),
        );
        model.apply_status(&ticket, status);
        drop(model);
        self.publish_git_changes();
    }

    fn publish_git_changes(&self) {
        use vibex_nsh_ui::screens::git::PathSelection;
        let model = self.git_model.borrow();
        let selected = model.selected_change_paths();
        let selected_paths = selected
            .iter()
            .map(String::as_str)
            .collect::<std::collections::BTreeSet<_>>();
        let mut prefix_counts = BTreeMap::<&str, (usize, usize)>::new();
        for change in model.status.iter().flat_map(|status| &status.changes) {
            let checked = usize::from(selected_paths.contains(change.path.as_str()));
            for prefix in std::iter::once(change.path.as_str()).chain(
                change
                    .path
                    .match_indices('/')
                    .map(|(slash, _)| &change.path[..slash]),
            ) {
                let (total, selected) = prefix_counts.entry(prefix).or_default();
                *total += 1;
                *selected += checked;
            }
        }
        self.git_changes.set(
            (0..model.change_tree_row_count())
                .filter_map(|index| model.change_tree_row(index))
                .map(|(row, change)| GitChangeRow {
                    path: row.path.clone(),
                    name: if row.kind == GitTreeRowKind::Directory {
                        row.segments
                            .iter()
                            .map(|segment| segment.name.as_str())
                            .collect::<Vec<_>>()
                            .join(" / ")
                    } else {
                        row.path.rsplit('/').next().unwrap_or(&row.path).to_owned()
                    },
                    directory: row.kind == GitTreeRowKind::Directory,
                    depth: row.depth,
                    expanded: row.expanded,
                    segments: row
                        .segments
                        .iter()
                        .map(|segment| segment.path.clone())
                        .collect(),
                    selection: match prefix_counts.get(row.path.as_str()).copied() {
                        Some((_, 0)) | None => PathSelection::Unchecked,
                        Some((total, selected)) if total == selected => PathSelection::Checked,
                        Some(_) => PathSelection::Partial,
                    },
                    status: change
                        .map_or_else(String::new, |change| change_letter(change.kind).to_owned()),
                    staged: change.is_some_and(|change| change.staged),
                    unstaged: change.is_some_and(|change| change.unstaged || !change.staged),
                    additions: change.map_or(0, |change| change.additions),
                    deletions: change.map_or(0, |change| change.deletions),
                })
                .collect(),
        );
        self.git_selected.set(selected);
    }

    /// Reads a path's diff into the preview.
    fn open_git_diff(&self, path: &str, staged: bool) {
        self.git_error.set(None);
        let generation = self.workbench_generation.get();
        let path = path.to_owned();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let title = path.clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_diff(GitDiffRequest {
                        workspace_id: workspace,
                        path,
                        staged,
                    })
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(diff)) => service.git_diff.set(Some(GitDiffPreview {
                    title,
                    body: diff.diff,
                    truncated: diff.truncated,
                    path: diff.path,
                    staged: diff.staged,
                })),
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reading the diff did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Stages or unstages one path and applies the status that comes back.
    fn stage_path(&self, path: &str, stage: bool) {
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        let path = path.to_owned();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            if !service.workbench_is_current(generation) {
                return;
            }
            let result = crate::runtime::handle()
                .spawn(async move {
                    let request = MutationRequest::new(GitStageRequest {
                        workspace_id: workspace,
                        paths: vec![path],
                    });
                    if stage {
                        git.stage(request).await
                    } else {
                        git.unstage(request).await
                    }
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(status)) => service.apply_git_status(status),
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("staging the path did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Commits what is staged with the message in the composer.
    fn open_worktrees(&self) {
        self.worktree_menu.set(true);
        self.load_worktrees();
    }

    fn load_worktrees(&self) {
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move { git.git_worktree_snapshot(workspace).await })
                .await;
            service.worktree_busy.set(false);
            match result {
                Ok(Ok(snapshot)) => {
                    let rows = snapshot
                        .managed_worktrees
                        .iter()
                        .map(|worktree| WorktreeRow {
                            id: worktree.worktree_id.as_str().to_owned(),
                            branch: worktree
                                .branch
                                .clone()
                                .or_else(|| worktree.target_branch.clone())
                                .unwrap_or_default(),
                            path: worktree.worktree_path.clone(),
                            status: worktree_status_label(worktree.status).to_owned(),
                            can_archive: worktree.status == GitManagedWorktreeStatus::Active,
                            can_restore: worktree.status == GitManagedWorktreeStatus::Archived,
                            can_merge: worktree.status == GitManagedWorktreeStatus::Active
                                && worktree.target_workspace_id.is_some(),
                            can_discard: matches!(
                                worktree.status,
                                GitManagedWorktreeStatus::Archived
                                    | GitManagedWorktreeStatus::Merged
                                    | GitManagedWorktreeStatus::Failed
                            ),
                        })
                        .collect();
                    let operations = snapshot
                        .operations
                        .iter()
                        .filter(|operation| {
                            matches!(
                                operation.status,
                                GitWorktreeOperationStatus::NeedsResolution
                                    | GitWorktreeOperationStatus::Recoverable
                                    | GitWorktreeOperationStatus::NeedsAttention
                            )
                        })
                        .map(|operation| WorktreeOperationRow {
                            id: operation.operation_id.as_str().to_owned(),
                            label: operation
                                .branch
                                .clone()
                                .or_else(|| operation.worktree_path.clone())
                                .unwrap_or_default(),
                            status: worktree_operation_status_label(operation.status).to_owned(),
                            can_continue: operation.status
                                == GitWorktreeOperationStatus::NeedsResolution,
                            can_abort: true,
                            conflicts: operation
                                .detail
                                .conflicts
                                .iter()
                                .filter(|conflict| !conflict.resolved)
                                .map(|conflict| WorktreeConflictRow {
                                    path: conflict.path.clone(),
                                })
                                .collect(),
                        })
                        .collect();
                    *service.worktree_records.borrow_mut() = snapshot.managed_worktrees;
                    service.worktrees.set(rows);
                    service.worktree_operations.set(operations);
                }
                Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                Err(error) => service.worktree_error.set(Some(
                    format!("reading the worktrees did not finish: {error}").into(),
                )),
            }
        });
    }

    fn archive_worktree(&self, id: &str) {
        let Some(record) = self
            .worktree_records
            .borrow()
            .iter()
            .find(|worktree| worktree.worktree_id.as_str() == id)
            .cloned()
        else {
            return;
        };
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let archive = GitWorktreeArchiveRequest {
                workspace_id: workspace.clone(),
                worktree_path: record.worktree_path.clone(),
                expected_head: record.head.clone(),
                preflight_revision: None,
            };
            let preflight_git = git.clone();
            let preflight = crate::runtime::handle()
                .spawn(async move { preflight_git.git_worktree_archive_preflight(archive).await })
                .await;
            match preflight {
                Ok(Ok(preflight)) if preflight.allowed => {
                    let request = GitWorktreeArchiveRequest {
                        workspace_id: workspace,
                        worktree_path: record.worktree_path.clone(),
                        expected_head: record.head.clone(),
                        preflight_revision: Some(preflight.revision),
                    };
                    let result = crate::runtime::handle()
                        .spawn(async move {
                            git.git_worktree_archive(MutationRequest::new(request))
                                .await
                        })
                        .await;
                    service.worktree_busy.set(false);
                    match result {
                        Ok(Ok(_)) => service.load_worktrees(),
                        Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                        Err(error) => service.worktree_error.set(Some(
                            format!("archiving the worktree did not finish: {error}").into(),
                        )),
                    }
                }
                Ok(Ok(_)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        copy::text(
                            "The runtime did not allow archiving this worktree.",
                            "运行时不允许归档这个 Worktree。",
                            "執行環境不允許封存這個 Worktree。",
                        )
                        .into(),
                    ));
                }
                Ok(Err(error)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(error_text(&error)));
                }
                Err(error) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        format!("checking the worktree did not finish: {error}").into(),
                    ));
                }
            }
        });
    }

    fn resolve_worktree_conflict(&self, id: &str, path: &str, source: bool) {
        let Ok(operation_id) = RequestId::parse(id) else {
            return;
        };
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        let path = path.to_owned();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let request = GitWorktreeConflictResolveRequest {
                operation_id,
                workspace_id: workspace,
                path,
                version: if source {
                    GitWorktreeConflictVersion::Source
                } else {
                    GitWorktreeConflictVersion::Target
                },
            };
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_worktree_resolve_conflict(MutationRequest::new(request))
                        .await
                })
                .await;
            service.worktree_busy.set(false);
            match result {
                Ok(Ok(_)) => service.load_worktrees(),
                Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                Err(error) => service.worktree_error.set(Some(
                    format!("resolving the conflict did not finish: {error}").into(),
                )),
            }
        });
    }

    fn continue_worktree_merge(&self, id: &str) {
        let Ok(operation_id) = RequestId::parse(id) else {
            return;
        };
        self.worktree_operation(operation_id, true);
    }

    fn abort_worktree_merge(&self, id: &str) {
        let Ok(operation_id) = RequestId::parse(id) else {
            return;
        };
        self.worktree_operation(operation_id, false);
    }

    fn worktree_operation(&self, operation_id: RequestId, cont: bool) {
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let request = GitWorktreeOperationRequest {
                operation_id,
                workspace_id: workspace,
            };
            let result = crate::runtime::handle()
                .spawn(async move {
                    if cont {
                        git.git_worktree_continue_merge(MutationRequest::new(request))
                            .await
                    } else {
                        git.git_worktree_abort_merge(MutationRequest::new(request))
                            .await
                    }
                })
                .await;
            service.worktree_busy.set(false);
            match result {
                Ok(Ok(_)) => service.load_worktrees(),
                Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                Err(error) => service.worktree_error.set(Some(
                    format!("the merge action did not finish: {error}").into(),
                )),
            }
        });
    }

    fn discard_worktree(&self, id: &str) {
        let Some(record) = self
            .worktree_records
            .borrow()
            .iter()
            .find(|worktree| worktree.worktree_id.as_str() == id)
            .cloned()
        else {
            return;
        };
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let request = GitWorktreeDiscardRequest {
                workspace_id: workspace.clone(),
                worktree_path: record.worktree_path.clone(),
                force: true,
                delete_branch: false,
                expected_head: record.head.clone(),
                preflight_revision: None,
            };
            let preflight_git = git.clone();
            let preflight = crate::runtime::handle()
                .spawn(async move { preflight_git.git_worktree_discard_preflight(request).await })
                .await;
            match preflight {
                Ok(Ok(preflight)) if preflight.allowed => {
                    let request = GitWorktreeDiscardRequest {
                        workspace_id: workspace,
                        worktree_path: record.worktree_path.clone(),
                        force: true,
                        delete_branch: false,
                        expected_head: record.head.clone(),
                        preflight_revision: Some(preflight.revision),
                    };
                    let result = crate::runtime::handle()
                        .spawn(async move {
                            git.git_worktree_discard(MutationRequest::new(request))
                                .await
                        })
                        .await;
                    service.worktree_busy.set(false);
                    match result {
                        Ok(Ok(_)) => service.load_worktrees(),
                        Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                        Err(error) => service.worktree_error.set(Some(
                            format!("discarding the worktree did not finish: {error}").into(),
                        )),
                    }
                }
                Ok(Ok(_)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        copy::text(
                            "The runtime did not allow discarding this worktree.",
                            "运行时不允许丢弃这个 Worktree。",
                            "執行環境不允許丟棄這個 Worktree。",
                        )
                        .into(),
                    ));
                }
                Ok(Err(error)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(error_text(&error)));
                }
                Err(error) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        format!("checking the worktree did not finish: {error}").into(),
                    ));
                }
            }
        });
    }

    fn merge_worktree(&self, id: &str) {
        let Some(record) = self
            .worktree_records
            .borrow()
            .iter()
            .find(|worktree| worktree.worktree_id.as_str() == id)
            .cloned()
        else {
            return;
        };
        let Some(target) = record.target_workspace_id.clone() else {
            return;
        };
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let plan_request = GitWorktreeMergeRequest {
                workspace_id: workspace,
                source_path: record.worktree_path.clone(),
                target_workspace_id: Some(target),
                strategy: None,
                expected_source_head: record.head.clone(),
                expected_target_head: None,
                preflight_revision: None,
            };
            let plan_git = git.clone();
            let plan = crate::runtime::handle()
                .spawn(async move { plan_git.git_worktree_merge_plan(plan_request).await })
                .await;
            match plan {
                Ok(Ok(plan)) if plan.preflight.allowed => {
                    let request = GitWorktreeMergeRequest {
                        workspace_id: plan.source_workspace_id.clone(),
                        source_path: plan.source_path.clone(),
                        target_workspace_id: Some(plan.target_workspace_id.clone()),
                        strategy: None,
                        expected_source_head: Some(plan.source_head.clone()),
                        expected_target_head: Some(plan.target_head.clone()),
                        preflight_revision: Some(plan.preflight.revision.clone()),
                    };
                    let result = crate::runtime::handle()
                        .spawn(async move {
                            git.git_worktree_merge(MutationRequest::new(request)).await
                        })
                        .await;
                    service.worktree_busy.set(false);
                    match result {
                        Ok(Ok(_)) => service.load_worktrees(),
                        Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                        Err(error) => service.worktree_error.set(Some(
                            format!("merging the worktree did not finish: {error}").into(),
                        )),
                    }
                }
                Ok(Ok(_)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        copy::text(
                            "The runtime did not allow merging this worktree.",
                            "运行时不允许合并这个 Worktree。",
                            "執行環境不允許合併這個 Worktree。",
                        )
                        .into(),
                    ));
                }
                Ok(Err(error)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(error_text(&error)));
                }
                Err(error) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        format!("planning the merge did not finish: {error}").into(),
                    ));
                }
            }
        });
    }

    fn restore_worktree(&self, id: &str) {
        let Ok(worktree_id) = RequestId::parse(id) else {
            return;
        };
        if self.worktree_busy.get_untracked() {
            return;
        }
        self.worktree_busy.set(true);
        self.worktree_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.worktree_busy.set(false);
                return;
            };
            let restore = GitWorktreeRestoreRequest {
                workspace_id: workspace.clone(),
                worktree_id: worktree_id.clone(),
                preflight_revision: None,
            };
            let preflight_git = git.clone();
            let preflight = crate::runtime::handle()
                .spawn(async move { preflight_git.git_worktree_restore_preflight(restore).await })
                .await;
            match preflight {
                Ok(Ok(preflight)) if preflight.allowed => {
                    let request = GitWorktreeRestoreRequest {
                        workspace_id: workspace,
                        worktree_id,
                        preflight_revision: Some(preflight.revision),
                    };
                    let result = crate::runtime::handle()
                        .spawn(async move {
                            git.git_worktree_restore(MutationRequest::new(request))
                                .await
                        })
                        .await;
                    service.worktree_busy.set(false);
                    match result {
                        Ok(Ok(_)) => service.load_worktrees(),
                        Ok(Err(error)) => service.worktree_error.set(Some(error_text(&error))),
                        Err(error) => service.worktree_error.set(Some(
                            format!("restoring the worktree did not finish: {error}").into(),
                        )),
                    }
                }
                Ok(Ok(_)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        copy::text(
                            "The runtime did not allow restoring this worktree.",
                            "运行时不允许恢复这个 Worktree。",
                            "執行環境不允許還原這個 Worktree。",
                        )
                        .into(),
                    ));
                }
                Ok(Err(error)) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(error_text(&error)));
                }
                Err(error) => {
                    service.worktree_busy.set(false);
                    service.worktree_error.set(Some(
                        format!("checking the worktree did not finish: {error}").into(),
                    ));
                }
            }
        });
    }

    fn open_git_branches(&self) {
        self.git_branch_menu.set(true);
        self.load_git_branches();
    }

    fn load_git_branches(&self) {
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move { git.git_branch_list(workspace).await })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(list)) => service.git_branches.set(
                    list.branches
                        .into_iter()
                        .map(|branch| BranchRow {
                            name: branch.name,
                            current: branch.current,
                            ahead: branch.ahead,
                            behind: branch.behind,
                            detail: branch
                                .upstream
                                .filter(|upstream| !upstream.trim().is_empty())
                                .unwrap_or_default(),
                        })
                        .collect(),
                ),
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reading the branches did not finish: {error}").into(),
                )),
            }
        });
    }

    fn select_git_branch(&self, name: &str) {
        self.git_branch_menu.set(false);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        let name = name.to_owned();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_branch_checkout(MutationRequest::new(GitBranchCheckoutRequest {
                        workspace_id: workspace,
                        name,
                    }))
                    .await
                })
                .await;
            match result {
                Ok(Ok(status)) => {
                    service
                        .git_branch
                        .set(status.branch.clone().map(Into::into));
                    *service.git_history_ref.borrow_mut() = None;
                    service.load_git_branches();
                    service.refresh_git();
                }
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("switching the branch did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Fetches, pulls or pushes the open workspace's branch.
    fn git_remote(&self, kind: GitRemoteActionKind) {
        if self.git_remote_busy.get_untracked() {
            return;
        }
        self.git_remote_busy.set(true);
        self.git_error.set(None);
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                if service.workbench_is_current(generation) {
                    service.git_remote_busy.set(false);
                }
                return;
            };
            if !service.workbench_is_current(generation) {
                return;
            }
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_remote_action(MutationRequest::new(GitRemoteActionRequest {
                        workspace_id: workspace,
                        kind,
                        remote: None,
                        branch: None,
                    }))
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            service.git_remote_busy.set(false);
            match result {
                Ok(Ok(result)) => {
                    if let Some(status) = result.status_after {
                        service.apply_git_status(status);
                    }
                    service.refresh_git();
                }
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("the remote action did not finish: {error}").into(),
                )),
            }
        });
    }

    fn commit_changes(&self) {
        if self.git_committing.get_untracked() {
            return;
        }
        let message = self.git_message.get_untracked().trim().to_owned();
        let paths = self.git_model.borrow().selected_change_paths();
        if message.is_empty() || paths.is_empty() {
            return;
        }
        self.git_committing.set(true);
        self.git_error.set(None);
        let amend = self.git_amend.get_untracked();
        let push_after = self.git_push_after.get_untracked();
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                if service.workbench_is_current(generation) {
                    service.git_committing.set(false);
                }
                return;
            };
            if !service.workbench_is_current(generation) {
                return;
            }
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.commit(MutationRequest::new(GitCommitRequest {
                        workspace_id: workspace,
                        message,
                        paths,
                        amend,
                        push_after,
                    }))
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            service.git_committing.set(false);
            match result {
                Ok(Ok(_)) => {
                    service.git_message.set(String::new());
                    service.git_amend.set(false);
                    service.git_push_after.set(false);
                    service.refresh_git();
                }
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service
                    .git_error
                    .set(Some(format!("committing did not finish: {error}").into())),
            }
        });
    }

    /// Shows the history, or goes back to the changes.
    fn toggle_git_history(&self) {
        if self.git_show_history.get_untracked() {
            self.git_show_history.set(false);
            return;
        }
        self.git_show_history.set(true);
        self.git_diff.set(None);
        self.load_git_history();
    }

    fn load_git_history(&self) {
        self.git_error.set(None);
        let generation = self.workbench_generation.get();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let ref_name = service.git_history_ref.borrow().clone();
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_history(GitHistoryRequest {
                        workspace_id: workspace,
                        limit: Some(40),
                        before_commit: None,
                        ref_name,
                        author: None,
                        query: None,
                        authored_after_ms: None,
                        authored_before_ms: None,
                    })
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(history)) => service.git_history.set(
                    history
                        .commits
                        .into_iter()
                        .map(|commit| CommitRow {
                            hash: commit.hash,
                            short_hash: commit.short_hash,
                            subject: commit.subject,
                            detail: commit.author_name,
                        })
                        .collect(),
                ),
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reading the history did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Reads one commit's message and patch.
    fn open_git_commit(&self, hash: &str) {
        self.git_error.set(None);
        let generation = self.workbench_generation.get();
        let hash = hash.to_owned();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_commit_detail(GitCommitDetailRequest {
                        workspace_id: workspace,
                        commit_hash: hash,
                        include_patch: true,
                    })
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            match result {
                Ok(Ok(detail)) => {
                    let mut body = detail.summary.subject.clone();
                    if let Some(message) = detail.body {
                        body.push_str("\n\n");
                        body.push_str(&message);
                    }
                    if let Some(patch) = detail.patch {
                        body.push_str("\n\n");
                        body.push_str(&patch);
                    }
                    service.git_commit_detail.set(Some(CommitDetailPreview {
                        title: detail.summary.short_hash,
                        body,
                        truncated: detail.patch_truncated,
                    }));
                }
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reading the commit did not finish: {error}").into(),
                )),
            }
        });
    }

    fn revert_paths(&self, paths: Vec<String>) {
        if paths.is_empty() || self.git_remote_busy.get_untracked() {
            return;
        }
        let generation = self.workbench_generation.get();
        self.git_remote_busy.set(true);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                if service.workbench_is_current(generation) {
                    service.git_remote_busy.set(false);
                }
                return;
            };
            if !service.workbench_is_current(generation) {
                return;
            }
            let result = crate::runtime::handle()
                .spawn(async move {
                    git.git_revert(MutationRequest::new(GitStageRequest {
                        workspace_id: workspace,
                        paths,
                    }))
                    .await
                })
                .await;
            if !service.workbench_is_current(generation) {
                return;
            }
            service.git_remote_busy.set(false);
            match result {
                Ok(Ok(status)) => {
                    service.git_diff.set(None);
                    service.git_error.set(None);
                    service.apply_git_status(status);
                }
                Ok(Err(error)) => service.git_error.set(Some(error_text(&error))),
                Err(error) => service.git_error.set(Some(
                    format!("reverting the path did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Leaves the diff, then the surface.
    fn git_back(&self) {
        if self.git_commit_detail.get_untracked().is_some() {
            self.git_commit_detail.set(None);
            return;
        }
        if self.git_show_history.get_untracked() {
            self.git_show_history.set(false);
            return;
        }
        if self.git_diff.get_untracked().is_some() {
            self.git_diff.set(None);
        }
    }

    /// Re-reads the open session's transcript without clearing it.
    fn reload(&self, session_id: &VibexSessionId) {
        // A lagged stream may name a session the reader is not on; its page
        // must not replace the open transcript.
        if self.active.get_untracked().as_deref() != Some(session_id.as_str()) {
            return;
        }
        self.conversation.set(ConversationState::Loading);
        let service = self.clone();
        let backend = self.backend.clone();
        let session_id = session_id.clone();
        spawn(async move {
            let result = crate::runtime::handle()
                .spawn(fetch_page(backend, session_id.clone()))
                .await;
            if !service.alive.get()
                || service.active.get_untracked().as_deref() != Some(session_id.as_str())
            {
                return;
            }
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service
                    .conversation
                    .set(ConversationState::Failed(error_text(&error))),
                Err(error) => service.conversation.set(ConversationState::Failed(
                    format!("opening the session did not finish: {error}").into(),
                )),
            }
        });
    }

    fn fail(&self, error: &BackendError) {
        self.sending.set(false);
        self.resolving.set(false);
        self.error.set(Some(error_text(error)));
        crate::settings::haptic(nsh_core::Haptic::Notification(
            nsh_core::HapticNotification::Error,
        ));
    }

    fn fail_text(&self, message: &str) {
        self.sending.set(false);
        self.resolving.set(false);
        self.error.set(Some(message.to_owned().into()));
        crate::settings::haptic(nsh_core::Haptic::Notification(
            nsh_core::HapticNotification::Error,
        ));
    }
}

/// Lists the sessions and fetches one page of a session's timeline.
async fn fetch_older(
    backend: Arc<dyn AgentBackend>,
    session_id: VibexSessionId,
    before_sequence: i64,
) -> BackendResult<TimelinePage> {
    backend
        .fetch_timeline(FetchTimelineRequest {
            session_id,
            after_sequence: None,
            before_sequence: Some(before_sequence),
            limit: TIMELINE_LIMIT,
            max_turns: None,
            max_bytes: None,
        })
        .await
}

async fn fetch_page(
    backend: Arc<dyn AgentBackend>,
    session_id: VibexSessionId,
) -> BackendResult<TimelinePage> {
    backend
        .fetch_timeline(FetchTimelineRequest {
            session_id,
            after_sequence: None,
            before_sequence: None,
            limit: TIMELINE_LIMIT,
            max_turns: None,
            max_bytes: None,
        })
        .await
}

/// Sends `text` (and any attachments) and reads the transcript back.
async fn send_and_refetch(
    backend: Arc<dyn AgentBackend>,
    session_id: VibexSessionId,
    text: String,
    attachments: Vec<MessageAttachment>,
    mentions: Vec<VibexUseMention>,
) -> BackendResult<TimelinePage> {
    // The session's own selection is what the runtime expects the send to
    // carry; asking first also notices a runtime that cannot take a turn.
    let selection = backend.runtime_selection(session_id.clone()).await?.desired;
    backend
        .send_message(MutationRequest::new(SendAgentMessageRequest {
            session_id: session_id.clone(),
            message_idempotency_key: vibex_core::RequestId::new().into_string(),
            desired_runtime: selection,
            text,
            attachments,
            mentions,
            reasoning_effort: None,
            correlation_id: None,
            delivery: UserMessageDelivery::Prompt,
            prompt_context: None,
            provenance: MessageProvenance::HumanInput,
        }))
        .await?;
    fetch_page(backend, session_id).await
}

fn worktree_operation_status_label(status: GitWorktreeOperationStatus) -> &'static str {
    match status {
        GitWorktreeOperationStatus::Pending => copy::text("Pending", "等待中", "等待中"),
        GitWorktreeOperationStatus::Queued => copy::text("Queued", "排队中", "排隊中"),
        GitWorktreeOperationStatus::Running => copy::text("Running", "运行中", "執行中"),
        GitWorktreeOperationStatus::Completed => copy::text("Completed", "已完成", "已完成"),
        GitWorktreeOperationStatus::Failed => copy::text("Failed", "失败", "失敗"),
        GitWorktreeOperationStatus::Recoverable => copy::text("Recoverable", "可恢复", "可還原"),
        GitWorktreeOperationStatus::NeedsResolution => {
            copy::text("Needs resolution", "需要解决冲突", "需要解決衝突")
        }
        GitWorktreeOperationStatus::Aborting => copy::text("Aborting", "中止中", "中止中"),
        GitWorktreeOperationStatus::NeedsAttention => {
            copy::text("Needs attention", "需要处理", "需要處理")
        }
        GitWorktreeOperationStatus::Aborted => copy::text("Aborted", "已中止", "已中止"),
        GitWorktreeOperationStatus::Unknown => copy::text("Unknown", "未知", "未知"),
    }
}

fn worktree_status_label(status: GitManagedWorktreeStatus) -> &'static str {
    match status {
        GitManagedWorktreeStatus::Active => copy::text("Active", "使用中", "使用中"),
        GitManagedWorktreeStatus::Archiving => copy::text("Archiving", "归档中", "封存中"),
        GitManagedWorktreeStatus::Archived => copy::text("Archived", "已归档", "已封存"),
        GitManagedWorktreeStatus::Restoring => copy::text("Restoring", "恢复中", "還原中"),
        GitManagedWorktreeStatus::Discarding => copy::text("Discarding", "丢弃中", "丟棄中"),
        GitManagedWorktreeStatus::Merged => copy::text("Merged", "已合并", "已合併"),
        GitManagedWorktreeStatus::Discarded => copy::text("Discarded", "已丢弃", "已丟棄"),
        GitManagedWorktreeStatus::Failed => copy::text("Failed", "失败", "失敗"),
        GitManagedWorktreeStatus::NeedsAttention => {
            copy::text("Needs attention", "需要处理", "需要處理")
        }
        GitManagedWorktreeStatus::Unknown => copy::text("Unknown", "未知", "未知"),
    }
}

fn team_phase_label(phase: DelegationTaskPhase) -> &'static str {
    match phase {
        DelegationTaskPhase::Queued => copy::text("Queued", "排队中", "排隊中"),
        DelegationTaskPhase::Starting => copy::text("Starting", "启动中", "啟動中"),
        DelegationTaskPhase::Active => copy::text("Active", "运行中", "執行中"),
        DelegationTaskPhase::AwaitingReview => copy::text("Awaiting review", "待审核", "待審核"),
        DelegationTaskPhase::Cancelling => copy::text("Cancelling", "取消中", "取消中"),
        DelegationTaskPhase::Completed => copy::text("Completed", "已完成", "已完成"),
        DelegationTaskPhase::Failed => copy::text("Failed", "失败", "失敗"),
        DelegationTaskPhase::Cancelled => copy::text("Cancelled", "已取消", "已取消"),
    }
}

fn sidebar_kind(kind: sidebar::SidebarRowKind) -> SidebarRowKind {
    match kind {
        sidebar::SidebarRowKind::Folder => SidebarRowKind::Folder,
        sidebar::SidebarRowKind::Project => SidebarRowKind::Project,
        sidebar::SidebarRowKind::Workspace => SidebarRowKind::Workspace,
        sidebar::SidebarRowKind::Group => SidebarRowKind::Group,
        sidebar::SidebarRowKind::EmptyWorkspace => SidebarRowKind::EmptyWorkspace,
        sidebar::SidebarRowKind::Session => SidebarRowKind::Session,
    }
}

fn collapse_sidebar_row(
    kind: SidebarRowKind,
    id: &str,
    collapsed: bool,
) -> Option<RemoteSidebarOrganizationMutation> {
    let id = id.to_owned();
    Some(match kind {
        SidebarRowKind::Project => RemoteSidebarOrganizationMutation::SetProjectCollapsed {
            project_id: id,
            collapsed,
        },
        SidebarRowKind::Workspace => RemoteSidebarOrganizationMutation::SetWorkspaceCollapsed {
            workspace_id: id,
            collapsed,
        },
        SidebarRowKind::Folder => RemoteSidebarOrganizationMutation::SetFolderCollapsed {
            folder_id: id,
            collapsed,
        },
        SidebarRowKind::Group => RemoteSidebarOrganizationMutation::SetGroupCollapsed {
            group_id: id,
            collapsed,
        },
        _ => return None,
    })
}

fn project_mark(appearance: Option<&SidebarProjectAppearance>) -> (ImageSrc, Hsla) {
    use vibex_nsh_ui::theme;
    let default = SidebarProjectAppearance::default();
    let appearance = appearance.unwrap_or(&default);
    macro_rules! svg {
        ($stem:literal) => {
            ImageSrc::svg_markup(include_str!(concat!(
                "../../../crates/vibex-nsh-ui/assets/icons/",
                $stem,
                ".svg"
            )))
        };
    }
    let image = if appearance.custom_logo_file.is_some() {
        svg!("image")
    } else {
        match appearance.logo {
            SidebarProjectLogo::Boxes => svg!("boxes"),
            SidebarProjectLogo::Code => svg!("code-xml"),
            SidebarProjectLogo::Terminal => svg!("file-terminal"),
            SidebarProjectLogo::Database => svg!("database"),
            SidebarProjectLogo::GitBranch => svg!("git-branch"),
            SidebarProjectLogo::Hash => svg!("hash"),
            SidebarProjectLogo::Book => svg!("book-open-text"),
            SidebarProjectLogo::Sparkles => svg!("sparkles"),
            SidebarProjectLogo::Folder => svg!("folder"),
            SidebarProjectLogo::Briefcase => svg!("briefcase"),
            SidebarProjectLogo::Box => svg!("box"),
            SidebarProjectLogo::Globe => svg!("globe"),
            SidebarProjectLogo::Server => svg!("server"),
            SidebarProjectLogo::Cpu => svg!("cpu"),
            SidebarProjectLogo::Layers => svg!("layers"),
            SidebarProjectLogo::Braces => svg!("braces"),
            SidebarProjectLogo::Rocket => svg!("rocket"),
            SidebarProjectLogo::Wrench => svg!("wrench"),
            SidebarProjectLogo::Gift => svg!("gift"),
            SidebarProjectLogo::Chart => svg!("chart-column"),
            SidebarProjectLogo::Palette => svg!("palette"),
            SidebarProjectLogo::Gauge => svg!("gauge"),
            SidebarProjectLogo::Workflow => svg!("workflow"),
            SidebarProjectLogo::Package => svg!("package"),
        }
    };
    let color = match appearance.color {
        SidebarProjectLogoColor::Neutral => theme::sidebar_text_muted(),
        SidebarProjectLogoColor::Blue => theme::accent_blue(),
        SidebarProjectLogoColor::Cyan => nsh_core::color::hsla(187.0 / 360.0, 0.86, 0.54, 1.0),
        SidebarProjectLogoColor::Green => theme::accent_green(),
        SidebarProjectLogoColor::Yellow => theme::accent_yellow(),
        SidebarProjectLogoColor::Orange => nsh_core::color::hsla(24.0 / 360.0, 0.95, 0.53, 1.0),
        SidebarProjectLogoColor::Red => theme::accent_red(),
        SidebarProjectLogoColor::Magenta => nsh_core::color::hsla(292.0 / 360.0, 0.84, 0.76, 1.0),
    };
    (image, color)
}

fn display_state(state: AgentSessionState) -> SessionState {
    match state {
        AgentSessionState::Running | AgentSessionState::Initializing => SessionState::Running,
        AgentSessionState::NeedsInput => SessionState::Waiting,
        AgentSessionState::Error => SessionState::Failed,
        _ => SessionState::Idle,
    }
}

fn summary(session: &AgentSession, spaces: &[WorkspaceSummary]) -> SessionSummary {
    let space = spaces
        .iter()
        .find(|space| space.workspace.id == session.workspace_id);
    let project_name = space
        .map(|space| space.project.name.clone())
        .filter(|name| !name.trim().is_empty())
        .unwrap_or_else(|| session.project_id.as_str().to_owned());
    let workspace_label = space
        .and_then(|space| space.git_branch.clone())
        .filter(|branch| !branch.trim().is_empty())
        .or_else(|| {
            session
                .workspace_root
                .rsplit(['/', '\\'])
                .next()
                .filter(|name| !name.trim().is_empty())
                .map(str::to_owned)
        })
        .unwrap_or_default();
    SessionSummary {
        id: session.id.as_str().to_owned(),
        title: if session.title.trim().is_empty() {
            copy::text("New session", "新会话", "新工作階段").to_owned()
        } else {
            session.title.clone()
        },
        agent: session.agent_id.as_str().to_owned(),
        meta: age(session.updated_at_ms.max(session.last_message_at_ms)),
        state: display_state(session.display_state()),
        terminal: session.transport.is_terminal(),
        project_id: session.project_id.as_str().to_owned(),
        project_name,
        workspace_id: session.workspace_id.as_str().to_owned(),
        workspace_label,
    }
}

struct NewSessionDraft {
    title: Option<String>,
    text: String,
    attachments: Vec<AttachmentItem>,
}

enum TerminalCommand {
    Input(String),
    Resize(u16, u16),
}

fn spawn_terminal_stream(
    subscribe: impl FnOnce() -> BackendResult<Box<dyn TerminalFrameSubscription>> + Send + 'static,
    output: mpsc::Sender<TerminalFrameBatch>,
) -> tokio::task::JoinHandle<BackendResult<()>> {
    crate::runtime::handle().spawn(async move {
        let mut subscription = subscribe()?;
        while let Some(batch) = subscription.next().await? {
            if output.send(batch).await.is_err() {
                break;
            }
        }
        Ok(())
    })
}

/// Decodes a terminal frame, keeping an incomplete UTF-8 tail for the next
/// frame instead of showing a replacement mark for it.
fn decode_terminal_bytes(pending: &mut Vec<u8>, bytes: &[u8]) -> String {
    pending.extend_from_slice(bytes);
    let mut decoded = String::new();
    let mut consumed = 0;
    while consumed < pending.len() {
        match std::str::from_utf8(&pending[consumed..]) {
            Ok(text) => {
                decoded.push_str(text);
                consumed = pending.len();
            }
            Err(error) => {
                let valid_end = consumed + error.valid_up_to();
                decoded.push_str(
                    std::str::from_utf8(&pending[consumed..valid_end]).unwrap_or_default(),
                );
                consumed = valid_end;
                let Some(invalid_len) = error.error_len() else {
                    break;
                };
                decoded.push(char::REPLACEMENT_CHARACTER);
                consumed += invalid_len;
            }
        }
    }
    pending.drain(..consumed);
    decoded
}

/// The sheet's row for one terminal.
fn terminal_choice(terminal: &TerminalSession) -> TerminalChoice {
    TerminalChoice {
        id: terminal.id.as_str().to_owned(),
        title: terminal.title.clone(),
        status: match terminal.status {
            TerminalStatus::Running => "running",
            TerminalStatus::Exited => "exited",
            TerminalStatus::Killed => "killed",
            TerminalStatus::Stale => "stale",
        }
        .to_owned(),
    }
}

fn push_terminal_choice(choices: &mut Vec<TerminalChoice>, terminal: &TerminalSession) {
    let choice = terminal_choice(terminal);
    if let Some(existing) = choices.iter_mut().find(|existing| existing.id == choice.id) {
        *existing = choice;
    } else {
        choices.push(choice);
    }
}

fn change_letter(kind: GitChangeKind) -> &'static str {
    match kind {
        GitChangeKind::Added => "A",
        GitChangeKind::Modified => "M",
        GitChangeKind::Deleted => "D",
        GitChangeKind::Renamed => "R",
        GitChangeKind::Copied => "C",
        GitChangeKind::TypeChanged => "T",
        GitChangeKind::Untracked => "?",
        GitChangeKind::Unmerged => "U",
        GitChangeKind::Unknown => "·",
    }
}

/// The palette rows a discovery answers with, deduplicated by id.
const COMPOSER_AGENT_REFERENCE_KEY: &str = "vibexUseAgentReference";
const COMPOSER_SELECTION_REFERENCE_KEY: &str = "vibexUseSelectionReference";
const COMPOSER_SESSION_REFERENCE_KEY: &str = "vibexUseSessionReference";

fn mention_for_entry(entry: &AgentCommandEntry) -> Option<VibexUseMention> {
    let metadata = |key: &str| {
        entry
            .metadata
            .iter()
            .find(|metadata| metadata.key == key)
            .map(|metadata| metadata.value.as_str())
    };
    if let (Some(agent_id), Some(option)) = (
        metadata(COMPOSER_AGENT_REFERENCE_KEY),
        metadata(COMPOSER_SELECTION_REFERENCE_KEY),
    ) {
        let agent_id = AgentId::parse(agent_id).ok()?;
        let option = VibexUseRef::parse(option)?;
        return Some(VibexUseMention::agent(
            &agent_id,
            &option,
            Some(entry.label.as_str()),
        ));
    }
    if let Some(session) = metadata(COMPOSER_SESSION_REFERENCE_KEY) {
        let reference = VibexUseRef::parse(session)?;
        let session_id = reference.session_id()?;
        return Some(VibexUseMention::session(
            &session_id,
            Some(entry.label.as_str()),
        ));
    }
    None
}

fn parse_slash_command_invocation(text: &str) -> Option<(String, Option<String>)> {
    let rest = text.trim().strip_prefix('/')?;
    let mut parts = rest.splitn(2, char::is_whitespace);
    let name = parts.next()?.trim();
    if name.is_empty() {
        return None;
    }
    let arguments = parts
        .next()
        .map(str::trim)
        .filter(|arguments| !arguments.is_empty())
        .map(str::to_owned);
    Some((name.to_owned(), arguments))
}

fn command_entries(discovery: vibex_core::AgentCommandDiscovery) -> Vec<CommandEntry> {
    let mut entries = Vec::new();
    let mut seen = std::collections::BTreeSet::new();
    for entry in discovery
        .response
        .entries
        .into_iter()
        .chain(discovery.quick_phrases)
        .chain(discovery.delegation_entries)
        .chain(discovery.session_entries)
    {
        if seen.insert(entry.id.clone()) {
            entries.push(CommandEntry {
                id: entry.id,
                label: entry.label,
                description: entry.description.unwrap_or_default(),
                insertion: entry.insertion_text,
            });
        }
    }
    entries
}

/// The card the phone shows for a pending question.
fn pending_question(request: &ElicitationRequest) -> PendingQuestion {
    PendingQuestion {
        request_id: request.id.as_str().to_owned(),
        title: request
            .title
            .clone()
            .filter(|title| !title.trim().is_empty())
            .unwrap_or_else(|| copy::text("A question", "一个问题", "一個問題").to_owned()),
        message: request
            .description
            .clone()
            .filter(|message| !message.trim().is_empty())
            .unwrap_or_else(|| request.message.clone()),
        fields: request.fields.iter().filter_map(question_field).collect(),
    }
}

fn question_field(field: &ElicitationField) -> Option<QuestionField> {
    let kind = match &field.kind {
        ElicitationFieldKind::Text {
            default, options, ..
        } if !options.is_empty() => QuestionFieldKind::Choice {
            options: options
                .iter()
                .map(|option| QuestionOption {
                    value: option.value.clone(),
                    label: option.title.clone(),
                })
                .collect(),
            default: default.clone(),
        },
        ElicitationFieldKind::Text { default, .. } => QuestionFieldKind::Text {
            default: default.clone(),
        },
        ElicitationFieldKind::Number { default, .. } => QuestionFieldKind::Text {
            default: default.clone(),
        },
        ElicitationFieldKind::Integer { default, .. } => QuestionFieldKind::Text {
            default: default.map(|value| value.to_string()),
        },
        ElicitationFieldKind::Boolean { default } => {
            QuestionFieldKind::Boolean { default: *default }
        }
        ElicitationFieldKind::MultiSelect {
            options, default, ..
        } => QuestionFieldKind::Multi {
            options: options
                .iter()
                .map(|option| QuestionOption {
                    value: option.value.clone(),
                    label: option.title.clone(),
                })
                .collect(),
            default: default.clone(),
        },
        // A schema the phone cannot draw is left out rather than guessed at.
        ElicitationFieldKind::Unsupported { .. } => return None,
    };
    Some(QuestionField {
        id: field.id.clone(),
        title: field.title.clone(),
        description: field.description.clone(),
        required: field.required,
        kind,
    })
}

/// The typed answers a question's fields expect.
fn answers_for(
    request: &ElicitationRequest,
    answers: &[(String, QuestionAnswer)],
) -> Result<BTreeMap<String, ElicitationAnswerValue>, String> {
    let mut typed = BTreeMap::new();
    for field in &request.fields {
        let answer = answers
            .iter()
            .find(|(id, _)| id == &field.id)
            .map(|(_, answer)| answer);
        let value = match (&field.kind, answer) {
            (ElicitationFieldKind::Text { .. }, Some(QuestionAnswer::Text(text))) => {
                ElicitationAnswerValue::String(text.clone())
            }
            (ElicitationFieldKind::Number { .. }, Some(QuestionAnswer::Text(text))) => {
                ElicitationAnswerValue::Number(text.clone())
            }
            (ElicitationFieldKind::Integer { .. }, Some(QuestionAnswer::Text(text))) => {
                let value = text
                    .trim()
                    .parse::<i64>()
                    .map_err(|_| format!("`{}` is not a whole number.", field.title))?;
                ElicitationAnswerValue::Integer(value)
            }
            (ElicitationFieldKind::Boolean { .. }, Some(QuestionAnswer::Boolean(value))) => {
                ElicitationAnswerValue::Boolean(*value)
            }
            (ElicitationFieldKind::MultiSelect { .. }, Some(QuestionAnswer::Texts(values))) => {
                ElicitationAnswerValue::StringArray(values.clone())
            }
            (ElicitationFieldKind::Unsupported { .. }, _) => continue,
            (_, None) if !field.required => continue,
            _ => return Err(format!("`{}` needs an answer.", field.title)),
        };
        typed.insert(field.id.clone(), value);
    }
    Ok(typed)
}

/// The card the phone shows for a pending request.
fn permission_pending(request: &PermissionRequest) -> PendingPermission {
    let options = if request.response_options.is_empty() {
        request
            .allowed_responses
            .iter()
            .map(|kind| PermissionOption {
                id: format!("{kind:?}"),
                label: default_option_label(*kind).to_owned(),
                choice: choice_of(*kind),
            })
            .collect()
    } else {
        request
            .response_options
            .iter()
            .map(|option| PermissionOption {
                id: option.option_id.clone(),
                label: option.label.clone(),
                choice: choice_of(option.response),
            })
            .collect()
    };
    PendingPermission {
        request_id: request.id.as_str().to_owned(),
        title: request.title.clone(),
        details: request
            .details
            .iter()
            .map(|detail| {
                if detail.value.trim().is_empty() {
                    detail.label.clone()
                } else {
                    format!("{}: {}", detail.label, detail.value)
                }
            })
            .take(6)
            .collect(),
        options,
    }
}

fn choice_of(response: PermissionResponseKind) -> PermissionChoice {
    match response {
        PermissionResponseKind::Approve => PermissionChoice::Allow,
        PermissionResponseKind::AlwaysAllowForSession => PermissionChoice::AlwaysAllow,
        PermissionResponseKind::Deny => PermissionChoice::Deny,
    }
}

fn default_option_label(response: PermissionResponseKind) -> &'static str {
    match response {
        PermissionResponseKind::Approve => copy::text("Allow", "允许", "允許"),
        PermissionResponseKind::AlwaysAllowForSession => {
            copy::text("Always allow", "本会话始终允许", "本工作階段一律允許")
        }
        PermissionResponseKind::Deny => copy::text("Deny", "拒绝", "拒絕"),
    }
}

fn image_mime(name: &str) -> &'static str {
    match name
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "gif" => "image/gif",
        "webp" => "image/webp",
        "svg" => "image/svg+xml",
        _ => "image/jpeg",
    }
}

fn load_pins() -> Vec<String> {
    Storage::get_item(PINS_KEY)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn queued_views(
    snapshot: &RemoteSidebarOrganizationSnapshot,
    session_id: &str,
) -> Vec<QueuedMessageView> {
    snapshot
        .queued_sends
        .iter()
        .filter(|message| message.session_id == session_id)
        .map(|message| QueuedMessageView {
            id: message.message_id,
            text: message.text.clone(),
            attachment_count: message.attachments.len(),
        })
        .collect()
}

/// The cached transcript windows kept on this phone.
fn load_timeline_cache() -> BTreeMap<String, CachedTimeline> {
    Storage::get_item(TIMELINE_CACHE_KEY)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The per-session composer drafts kept on this phone.
fn swap_attachment_draft(
    drafts: &mut BTreeMap<String, Vec<AttachmentItem>>,
    previous: Option<&str>,
    next: &str,
    attachments: Vec<AttachmentItem>,
) -> Vec<AttachmentItem> {
    if let Some(previous) = previous {
        if attachments.is_empty() {
            drafts.remove(previous);
        } else {
            drafts.insert(previous.to_owned(), attachments);
        }
    }
    drafts.remove(next).unwrap_or_default()
}

fn load_drafts() -> BTreeMap<String, String> {
    Storage::get_item(DRAFTS_KEY)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

fn plan_view(items: &[vibex_core::TimelineItem]) -> Option<PlanView> {
    let plan = items.iter().rev().find_map(|item| match &item.payload {
        TimelinePayload::Plan(plan) => Some(plan),
        _ => None,
    })?;
    let steps = plan
        .steps
        .iter()
        .map(|step| PlanStepView {
            title: step.title.clone(),
            state: match step.status {
                PlanStepStatus::Pending => PlanStepState::Pending,
                PlanStepStatus::Running => PlanStepState::Running,
                PlanStepStatus::Completed => PlanStepState::Completed,
                PlanStepStatus::Failed => PlanStepState::Failed,
            },
        })
        .collect::<Vec<_>>();
    let completed = steps
        .iter()
        .filter(|step| step.state == PlanStepState::Completed)
        .count();
    let current = steps
        .iter()
        .position(|step| step.state == PlanStepState::Running)
        .unwrap_or(completed)
        .min(steps.len().saturating_sub(1));
    Some(PlanView {
        title: plan.title.clone(),
        steps,
        completed,
        current,
    })
}

fn goal_view(items: &[vibex_core::TimelineItem]) -> Option<GoalView> {
    let payload = items.iter().rev().find_map(|item| match &item.payload {
        TimelinePayload::Goal(payload) => Some(payload),
        _ => None,
    })?;
    payload
        .goal
        .as_ref()
        .map(|goal| goal_view_from_snapshot_with_actions(goal, &payload.actions))
}

fn goal_view_from_snapshot(goal: &vibex_core::GoalSnapshot) -> Option<GoalView> {
    Some(goal_view_from_snapshot_with_actions(goal, &[]))
}

fn goal_view_from_snapshot_with_actions(
    goal: &vibex_core::GoalSnapshot,
    actions: &[GoalAction],
) -> GoalView {
    let can_clear = actions.contains(&GoalAction::Clear);
    let progress = match (goal.tokens_used, goal.token_budget) {
        (Some(used), Some(budget)) if budget > 0 => Some(format!("{used} / {budget} tokens")),
        (Some(used), _) => Some(format!("{used} tokens")),
        _ => goal
            .time_used_seconds
            .map(|seconds| format!("{}m", seconds / 60)),
    };
    GoalView {
        objective: goal.objective.clone(),
        phase: match goal.phase {
            GoalPhase::Active => GoalPhaseView::Active,
            GoalPhase::Paused => GoalPhaseView::Paused,
            GoalPhase::Blocked => GoalPhaseView::Blocked,
            GoalPhase::UsageLimited => GoalPhaseView::UsageLimited,
            GoalPhase::BudgetLimited => GoalPhaseView::BudgetLimited,
            GoalPhase::Complete => GoalPhaseView::Complete,
            GoalPhase::Unknown => GoalPhaseView::Unknown,
        },
        progress,
        can_pause: goal.phase == GoalPhase::Active && actions.contains(&GoalAction::Pause),
        can_resume: goal.phase == GoalPhase::Paused && actions.contains(&GoalAction::Resume),
        can_clear,
    }
}

fn token_usage_view(usage: AgentTokenUsage) -> TokenUsageView {
    let percent = match (
        usage.context_window_used_tokens,
        usage.context_window_size_tokens,
    ) {
        (Some(used), Some(size)) if size > 0 => {
            Some(((used.saturating_mul(100) / size).min(100)) as u8)
        }
        _ => None,
    };
    TokenUsageView {
        percent,
        used: usage.context_window_used_tokens.unwrap_or_default(),
        size: usage.context_window_size_tokens.unwrap_or_default(),
        input: usage.input_tokens.unwrap_or_default(),
        output: usage.output_tokens.unwrap_or_default(),
    }
}

/// The one-line status a permission or question row carries.
fn row_meta(payload: Option<&TimelinePayload>) -> String {
    let Some(payload) = payload else {
        return String::new();
    };
    match payload {
        TimelinePayload::UserMessage(message) => match message.delivery {
            UserMessageDelivery::Prompt => String::new(),
            UserMessageDelivery::Steer => copy::text(
                "Steered into the running turn",
                "已插入正在运行的回合",
                "已插入正在執行的回合",
            )
            .to_owned(),
            UserMessageDelivery::Resend => copy::text(
                "Interrupted and resent",
                "已中断并重新发送",
                "已中斷並重新傳送",
            )
            .to_owned(),
        },
        TimelinePayload::PermissionRequest(request) => match request.status {
            PermissionRequestStatus::Pending => {
                copy::text("Pending", "待确认", "待確認").to_owned()
            }
            PermissionRequestStatus::Approved => {
                copy::text("Allowed", "已允许", "已允許").to_owned()
            }
            PermissionRequestStatus::Denied => copy::text("Denied", "已拒绝", "已拒絕").to_owned(),
            PermissionRequestStatus::Expired => {
                copy::text("Expired", "已过期", "已過期").to_owned()
            }
        },
        TimelinePayload::PermissionResolution(resolution) => match resolution.response {
            PermissionResponseKind::Approve => copy::text("Allowed", "已允许", "已允許").to_owned(),
            PermissionResponseKind::AlwaysAllowForSession => {
                copy::text("Allowed for session", "本会话允许", "本工作階段允許").to_owned()
            }
            PermissionResponseKind::Deny => copy::text("Denied", "已拒绝", "已拒絕").to_owned(),
        },
        TimelinePayload::ElicitationRequest(request) => match request.status {
            ElicitationRequestStatus::Pending => {
                copy::text("Pending", "待回答", "待回答").to_owned()
            }
            ElicitationRequestStatus::Accepted => {
                copy::text("Answered", "已回答", "已回答").to_owned()
            }
            ElicitationRequestStatus::Declined => {
                copy::text("Declined", "已拒绝", "已拒絕").to_owned()
            }
            ElicitationRequestStatus::Cancelled => {
                copy::text("Cancelled", "已取消", "已取消").to_owned()
            }
        },
        TimelinePayload::ElicitationResolution(resolution) => match resolution.action {
            ElicitationResolutionAction::Accept => {
                copy::text("Answered", "已回答", "已回答").to_owned()
            }
            ElicitationResolutionAction::Decline => {
                copy::text("Declined", "已拒绝", "已拒絕").to_owned()
            }
            _ => String::new(),
        },
        _ => String::new(),
    }
}

/// The attachments a user row carried, from the items behind it.
fn row_attachments(row: &TimelineRow, payload: Option<&TimelinePayload>) -> Vec<AttachmentItem> {
    let Some(TimelinePayload::UserMessage(message)) = payload else {
        return Vec::new();
    };
    message
        .attachments
        .iter()
        .enumerate()
        .map(|(index, attachment)| AttachmentItem {
            id: format!("{}:{index}", row.id),
            label: attachment.display_label().to_owned(),
            mime_type: attachment.mime_type.clone().unwrap_or_default(),
            uri: attachment.uri.clone().unwrap_or_default(),
        })
        .collect()
}

fn process_header(turn: &TimelineConversationTurn) -> TimelineEntry {
    let finished = activity::turn_finished(turn);
    TimelineEntry {
        id: format!("turn-header:{}", turn.id),
        kind: EntryKind::ProcessHeader,
        streaming: !finished,
        turn_id: turn.id.clone(),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms.unwrap_or(0),
        complete: finished,
        ..TimelineEntry::default()
    }
}

fn changes_entry(turn: &TimelineConversationTurn, files: Vec<TurnFileChange>) -> TimelineEntry {
    TimelineEntry {
        id: format!("turn-changes:{}", turn.id),
        kind: EntryKind::Changes,
        title: format!(
            "{} {}",
            files.len(),
            if files.len() == 1 {
                copy::text("file changed", "个文件已更改", "個檔案已變更")
            } else {
                copy::text("files changed", "个文件已更改", "個檔案已變更")
            }
        ),
        changed_files: files,
        turn_id: turn.id.clone(),
        ..TimelineEntry::default()
    }
}

fn turn_file_change(
    row: &TimelineRow,
    payload: Option<&TimelinePayload>,
) -> Option<TurnFileChange> {
    use vibex_core::FileOperationKind;
    let derived;
    let operation = match payload? {
        TimelinePayload::FileOperation(operation) => operation,
        TimelinePayload::ToolCall(tool) => {
            derived = tool_detail::file_operation(tool)?;
            &derived
        }
        _ => return None,
    };
    (operation.operation != FileOperationKind::Read && !operation.path.trim().is_empty()).then(
        || TurnFileChange {
            path: operation.path.clone(),
            row_id: row.id.clone(),
            deleted: operation.operation == FileOperationKind::Delete,
        },
    )
}

fn live_turn_entry(turn: &TimelineConversationTurn) -> TimelineEntry {
    TimelineEntry {
        id: format!("turn-live:{}", turn.id),
        kind: EntryKind::Generating,
        title: copy::text("Agent", "Agent", "Agent").to_owned(),
        body: copy::text("Working…", "工作中…", "工作中…").to_owned(),
        meta: String::new(),
        streaming: true,
        turn_id: turn.id.clone(),
        expanded: false,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: turn.process_rows.last().map_or(0, |row| row.last_sequence),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms.unwrap_or(0),
        complete: false,
        ..TimelineEntry::default()
    }
}

fn row_payload<'a>(
    row: &TimelineRow,
    items: &'a [vibex_core::TimelineItem],
) -> Option<&'a TimelinePayload> {
    row_item(row, items).map(|item| &item.payload)
}

fn row_item<'a>(
    row: &TimelineRow,
    items: &'a [vibex_core::TimelineItem],
) -> Option<&'a vibex_core::TimelineItem> {
    let end = items.partition_point(|item| item.sequence <= row.last_sequence);
    items[..end]
        .iter()
        .rev()
        .take_while(|item| item.sequence >= row.first_sequence)
        .find(|item| row.item_ids.iter().any(|id| id == item.id.as_str()))
}

fn workspace_file_path(path: &str, root: Option<&str>) -> String {
    let normalized = path.replace('\\', "/");
    let root = root.map(|root| root.replace('\\', "/"));
    root.as_deref()
        .and_then(|root| std::path::Path::new(&normalized).strip_prefix(root).ok())
        .filter(|path| !path.as_os_str().is_empty())
        .map_or_else(
            || path.to_owned(),
            |relative| relative.to_string_lossy().into_owned(),
        )
}

fn activity_details(row: &TimelineRow, payload: Option<&TimelinePayload>) -> Vec<ActivityDetail> {
    tool_detail::project(row, payload, None, copy::language().locale())
        .iter()
        .filter_map(|detail| {
            let text = detail.text();
            (!text.trim().is_empty()).then(|| ActivityDetail {
                label: detail.label().to_owned(),
                text,
                copy: detail.copy_text(),
            })
        })
        .collect()
}

fn activity_glyph(kind: ActivityKind) -> ActivityGlyph {
    match kind {
        ActivityKind::Command => ActivityGlyph::Command,
        ActivityKind::Search => ActivityGlyph::Search,
        ActivityKind::Directory => ActivityGlyph::Directory,
        ActivityKind::FileRead => ActivityGlyph::Read,
        ActivityKind::FileEdit => ActivityGlyph::Edit,
        ActivityKind::FileCreate => ActivityGlyph::Create,
        ActivityKind::FileDelete => ActivityGlyph::Delete,
        ActivityKind::Todo => ActivityGlyph::Plan,
        ActivityKind::Collaboration => ActivityGlyph::Agent,
        ActivityKind::Image => ActivityGlyph::Image,
        ActivityKind::Integration => ActivityGlyph::Integration,
        ActivityKind::Web => ActivityGlyph::Web,
        ActivityKind::Retry => ActivityGlyph::Retry,
        ActivityKind::Generic => ActivityGlyph::Tool,
    }
}

fn entry_kind(kind: vibex_desktop_model::TimelineRowKind) -> EntryKind {
    use vibex_desktop_model::TimelineRowKind as Row;
    match kind {
        Row::UserMessage => EntryKind::User,
        Row::AgentMessage => EntryKind::Agent,
        Row::Reasoning => EntryKind::Reasoning,
        Row::Error => EntryKind::Error,
        Row::ToolCall
        | Row::Command
        | Row::FileOperation
        | Row::WebSearch
        | Row::TodoUpdate
        | Row::Plan
        | Row::Collaboration
        | Row::Retry
        | Row::ImageGeneration => EntryKind::Tool,
        Row::PermissionRequest | Row::PermissionResolution => EntryKind::Permission,
        Row::ElicitationRequest | Row::ElicitationResolution => EntryKind::Question,
        Row::GitNotice | Row::SystemNotice => EntryKind::Notice,
    }
}

/// A short age for a row ("now", "5m", "2h", "3d").
fn age(timestamp_ms: i64) -> String {
    if timestamp_ms <= 0 {
        return String::new();
    }
    let seconds = (unix_timestamp_ms() - timestamp_ms).max(0) / 1_000;
    match seconds {
        0..=59 => copy::text("now", "刚刚", "剛剛").to_owned(),
        60..=3_599 => format!("{}m", seconds / 60),
        3_600..=86_399 => format!("{}h", seconds / 3_600),
        _ => format!("{}d", seconds / 86_400),
    }
}

#[cfg(test)]
mod tests {
    use super::{
        AttachmentItem, BTreeMap, Duration, TerminalFrameBatch, TerminalFrameSubscription,
        TerminalId, decode_terminal_bytes, mpsc, spawn_terminal_stream, swap_attachment_draft,
        workspace_file_path,
    };

    #[test]
    fn timeline_paths_are_relativized_only_against_their_workspace() {
        assert_eq!(
            workspace_file_path("/work/project/src/main.rs", Some("/work/project/")),
            "src/main.rs"
        );
        assert_eq!(
            workspace_file_path("C:\\work\\project\\src\\main.rs", Some("C:\\work\\project")),
            "src/main.rs"
        );
        assert_eq!(
            workspace_file_path("/work/project-other/main.rs", Some("/work/project")),
            "/work/project-other/main.rs"
        );
        assert_eq!(
            workspace_file_path("src/main.rs", Some("/work/project")),
            "src/main.rs"
        );
    }

    struct TimedTerminalFrames(std::vec::IntoIter<TerminalFrameBatch>);

    impl TerminalFrameSubscription for TimedTerminalFrames {
        fn next(&mut self) -> vibex_backend::BackendFuture<'_, Option<TerminalFrameBatch>> {
            Box::pin(async move {
                tokio::time::sleep(Duration::from_millis(1)).await;
                Ok(self.0.next())
            })
        }
    }

    #[test]
    fn terminal_subscription_has_a_reactor_and_forwards_frames_in_order() {
        assert!(tokio::runtime::Handle::try_current().is_err());
        let terminal_id = TerminalId::new();
        let batch = |sequence, reset_required| TerminalFrameBatch {
            terminal_id: terminal_id.clone(),
            frames: vec![vibex_backend::TerminalFrame {
                sequence,
                bytes: b"prompt".to_vec(),
            }],
            next_sequence: sequence + 1,
            dropped_frames: 0,
            reset_required,
        };
        let expected = vec![batch(1, false), batch(4, true)];
        let source = expected.clone();
        let (output, mut frames) = mpsc::channel(1);
        let task = spawn_terminal_stream(
            move || Ok(Box::new(TimedTerminalFrames(source.into_iter()))),
            output,
        );
        crate::runtime::block_on(async move {
            for batch in expected {
                assert_eq!(frames.recv().await, Some(batch));
            }
            assert_eq!(frames.recv().await, None);
            task.await.unwrap().unwrap();
        });
    }

    #[test]
    fn attachment_drafts_stay_with_the_session_they_were_added_to() {
        let mut drafts = BTreeMap::new();
        let first = AttachmentItem {
            id: "photo".to_owned(),
            label: "photo.jpg".to_owned(),
            mime_type: "image/jpeg".to_owned(),
            uri: "data:image/jpeg;base64,".to_owned(),
        };
        assert!(
            swap_attachment_draft(&mut drafts, Some("one"), "two", vec![first.clone()]).is_empty()
        );
        assert_eq!(
            swap_attachment_draft(&mut drafts, Some("two"), "one", Vec::new()),
            vec![first]
        );
        assert!(drafts.is_empty());
    }

    #[test]
    fn a_character_split_across_frames_keeps_its_bytes() {
        let mut pending = Vec::new();
        let bytes = "é".as_bytes();
        assert_eq!(decode_terminal_bytes(&mut pending, &bytes[..1]), "");
        assert_eq!(decode_terminal_bytes(&mut pending, &bytes[1..]), "é");
        assert!(pending.is_empty());
    }

    #[test]
    fn text_before_an_incomplete_tail_is_written_at_once() {
        let mut pending = Vec::new();
        let bytes = "a😀".as_bytes();
        let split = bytes.len() - 2;
        assert_eq!(decode_terminal_bytes(&mut pending, &bytes[..split]), "a");
        assert_eq!(decode_terminal_bytes(&mut pending, &bytes[split..]), "😀");
        assert!(pending.is_empty());
    }

    #[test]
    fn an_invalid_terminal_byte_does_not_hide_following_output() {
        let mut pending = Vec::new();
        assert_eq!(decode_terminal_bytes(&mut pending, b"\xffready"), "�ready");
        assert!(pending.is_empty());
    }
}

#[cfg(test)]
mod queue_view_tests {
    use super::*;

    #[test]
    fn queued_views_keep_only_the_open_session() {
        let snapshot = RemoteSidebarOrganizationSnapshot {
            queued_sends: vec![
                vibex_core::RemoteQueuedSend {
                    message_id: 1,
                    session_id: "a".to_owned(),
                    text: "first".to_owned(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: None,
                },
                vibex_core::RemoteQueuedSend {
                    message_id: 2,
                    session_id: "b".to_owned(),
                    text: "other".to_owned(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: None,
                },
                vibex_core::RemoteQueuedSend {
                    message_id: 3,
                    session_id: "a".to_owned(),
                    text: "third".to_owned(),
                    attachments: Vec::new(),
                    mentions: Vec::new(),
                    scheduled_at_ms: None,
                },
            ],
            ..RemoteSidebarOrganizationSnapshot::default()
        };
        let views = queued_views(&snapshot, "a");
        assert_eq!(views.len(), 2);
        assert_eq!(views[0].id, 1);
        assert_eq!(views[1].id, 3);
        assert_eq!(views[1].text, "third");
    }
}

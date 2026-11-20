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
use nsh_core::picker::{FilePickRequest, PickRequest, PickedStatus};
use nsh_core::platform::Lifecycle;
use nsh_core::prelude::*;
use nsh_core::storage::Storage;
use vibex_backend::{
    AgentBackend, BackendError, BackendEvent, BackendProjection, BackendResult, FileBackend,
    GitBackend, MutationRequest, TerminalBackend, WorkspaceBackend, WorkspaceSummary,
};
use vibex_core::{
    AgentCommandDiscoverRequest, AgentCommandEntry, AgentCommandExecuteRequest,
    AgentCommandExecuteStatus, AgentCommandExecutionBehavior, AgentCommandSourceKind,
    AgentCommandTrigger, AgentGoalControlRequest, AgentId, AgentNotificationKind,
    AgentSessionRuntimeSelectionState, AgentSessionState, AgentTokenUsage, ApprovalResponder,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, DelegationTaskPhase,
    ElicitationAnswerValue, ElicitationField, ElicitationFieldKind, ElicitationRequest,
    ElicitationRequestStatus, ElicitationResolution, ElicitationResolutionAction,
    FetchTimelineItemRequest, FetchTimelineRequest, FilePreviewKind, FileReadRequest,
    FileTreeRequest, ForkAgentSessionRequest, GitChangeKind, GitCommitDetailRequest,
    GitCommitRequest, GitDiffRequest, GitHistoryRequest, GitManagedWorktreeRecord,
    GitManagedWorktreeStatus, GitRemoteActionKind, GitRemoteActionRequest, GitStageRequest,
    GitStatusSummary, GitWorktreeArchiveRequest, GitWorktreeConflictResolveRequest,
    GitWorktreeConflictVersion, GitWorktreeCreateRequest, GitWorktreeDiscardRequest,
    GitWorktreeMergeRequest, GitWorktreeOperationRequest, GitWorktreeOperationStatus,
    GitWorktreeRestoreRequest, GoalAction, GoalPhase, MessageAttachment, MessageProvenance,
    OpenWorkspaceRequest, PermissionRequest, PermissionRequestStatus, PermissionResolution,
    PermissionResponseKind, PlanStepStatus, RenameAgentSessionRequest, RequestId,
    ResolveElicitationRequest, ResolvePermissionRequest, RuntimeModelSelection,
    RuntimeOptionAvailability, RuntimeSelectionInteraction, SendAgentMessageRequest,
    SessionRuntimeFeatureKind, SessionRuntimeOption, SessionRuntimeOptionCatalog,
    SessionRuntimeSelection, SetDesiredAgentSessionRuntimeRequest, TeamSnapshotRequest,
    TeamTaskAction, TeamTaskControlRequest, TerminalCreateRequest, TerminalId,
    TerminalResizeRequest, TerminalSession, TerminalStatus, TerminalWriteRequest, TimelinePage,
    TimelinePayload, UserMessageDelivery, VibexSessionId, VibexUseMention, VibexUseRef,
    WorkspaceId, unix_timestamp_ms,
};
use vibex_desktop_model::{TimelineConversationTurn, TimelineModel, TimelineRow};
use vibex_mobile_pairing::MobileCredentialBundle;
use vibex_nsh_ui::{
    copy,
    screens::connecting::ConnectionState,
    screens::files::{FileEntry, FilePreview, FilePreviewKindView},
    screens::git::{
        BranchRow, CommitDetailPreview, CommitRow, GitChangeRow, GitDiffPreview,
        WorktreeConflictRow, WorktreeOperationRow, WorktreeRow,
    },
    screens::new_session::{RuntimeChoice, WorkspaceChoice},
    screens::session::{
        AttachmentItem, CommandEntry, ComposerHistoryEntry, EntryKind, GoalControl, GoalPhaseView,
        GoalView, PendingPermission, PendingQuestion, PermissionChoice, PermissionOption,
        PlanStepState, PlanStepView, PlanView, QuestionAnswer, QuestionField, QuestionFieldKind,
        QuestionOption, QuestionResponse, QueuedMessageView, RuntimeChoiceView, RuntimeFeatureView,
        RuntimeOptionView, RuntimeSummaryView, SessionHost, SessionState, SessionSummary,
        TimelineEntry, TokenUsageView,
    },
    screens::team::TeamTaskView,
    screens::terminal::TerminalChoice,
};

use crate::pairing::error_text;

/// How many timeline items one read asks for.
const TIMELINE_LIMIT: u32 = 200;

/// Where the per-session composer drafts are kept.
const DRAFTS_KEY: &str = "vibex.mobile.drafts.v1";
/// Where the newest transcript window of the last few sessions is kept.
const TIMELINE_CACHE_KEY: &str = "vibex.mobile.timeline.v1";
/// Where the prompts written ahead of a running turn are kept.
const QUEUE_KEY: &str = "vibex.mobile.queue.v1";
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

/// A prompt held behind the running turn.
#[derive(Clone, Debug)]
struct QueuedMessage {
    id: u64,
    text: String,
    attachments: Vec<AttachmentItem>,
    mentions: Vec<VibexUseMention>,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredAttachment {
    label: String,
    mime_type: String,
    uri: String,
}

#[derive(Clone, Debug, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredQueued {
    id: u64,
    text: String,
    #[serde(default)]
    attachments: Vec<StoredAttachment>,
    #[serde(default)]
    mentions: Vec<VibexUseMention>,
}

#[derive(Clone, Debug, Default, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredQueues {
    #[serde(default)]
    sessions: BTreeMap<String, Vec<StoredQueued>>,
    #[serde(default)]
    paused: Vec<String>,
}

/// The session shell's state and actions.
#[derive(Clone)]
pub struct SessionService {
    /// The credential's host id, for the app's runtime bookkeeping.
    bundle_host_id: Rc<String>,
    backend: Arc<dyn AgentBackend>,
    terminal_backend: Arc<dyn TerminalBackend>,
    file_backend: Arc<dyn FileBackend>,
    git_backend: Arc<dyn GitBackend>,
    workspace_backend: Arc<dyn WorkspaceBackend>,
    sessions: Signal<Vec<SessionSummary>>,
    active: Signal<Option<String>>,
    pinned: Signal<Vec<String>>,
    timeline: Signal<Vec<TimelineEntry>>,
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
    terminal_title: Signal<Option<SharedString>>,
    terminal_error: Signal<Option<SharedString>>,
    terminal_ref: ComponentRef<Terminal>,
    terminal_id: Rc<RefCell<Option<TerminalId>>>,
    /// Bytes of a UTF-8 character split across two frames.
    terminal_pending: Rc<RefCell<Vec<u8>>>,
    terminal_choices: Signal<Vec<TerminalChoice>>,
    terminal_menu: Signal<bool>,
    /// Bumped when the terminal closes, so its stream stops writing.
    terminal_generation: Rc<Cell<u64>>,
    files_open: Signal<bool>,
    files_path: Signal<String>,
    files_entries: Signal<Vec<FileEntry>>,
    files_preview: Signal<Option<FilePreview>>,
    files_error: Signal<Option<SharedString>>,
    files_workspace: Rc<RefCell<Option<WorkspaceId>>>,
    files_content: Signal<String>,
    files_dirty: Signal<bool>,
    files_saving: Signal<bool>,
    files_revision: Rc<RefCell<Option<String>>>,
    files_current: Rc<RefCell<Option<String>>>,
    git_open: Signal<bool>,
    git_branch: Signal<Option<SharedString>>,
    git_changes: Signal<Vec<GitChangeRow>>,
    git_diff: Signal<Option<GitDiffPreview>>,
    git_error: Signal<Option<SharedString>>,
    git_staged: Signal<u32>,
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
    new_session_open: Signal<bool>,
    new_workspaces: Signal<Vec<WorkspaceChoice>>,
    new_runtimes: Signal<Vec<RuntimeChoice>>,
    new_workspace: Signal<Option<String>>,
    new_runtime: Signal<Option<String>>,
    new_worktree_branch: Signal<String>,
    new_base_ref: Signal<String>,
    new_project_path: Signal<String>,
    new_title: Signal<String>,
    new_prompt: Signal<String>,
    new_attachments: Signal<Vec<AttachmentItem>>,
    new_busy: Signal<bool>,
    new_error: Signal<Option<SharedString>>,
    workspace_records: Rc<RefCell<Vec<WorkspaceSummary>>>,
    runtime_options: Rc<RefCell<Vec<SessionRuntimeOption>>>,
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
    /// The serialized drafts last written, so an unchanged map is not rewritten.
    drafts_snapshot: Rc<RefCell<String>>,
    /// The state the runtime last reported for each session, for turn folds.
    session_states: Rc<RefCell<BTreeMap<String, AgentSessionState>>>,
    /// The turns whose working rows the reader opened.
    expanded_turns: Rc<RefCell<std::collections::BTreeSet<String>>>,
    /// The rows whose full detail the reader opened, and the detail itself.
    expanded_rows: Rc<RefCell<std::collections::BTreeSet<String>>>,
    row_details: Rc<RefCell<BTreeMap<String, String>>>,
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
    queue: Rc<RefCell<BTreeMap<String, Vec<QueuedMessage>>>>,
    queue_seq: Rc<Cell<u64>>,
    queue_paused_sessions: Rc<RefCell<std::collections::BTreeSet<String>>>,
    queued: Signal<Vec<QueuedMessageView>>,
    queue_paused: Signal<bool>,
    schedule_open: Signal<bool>,
    scheduled: Signal<bool>,
    schedule_generation: Rc<Cell<u64>>,
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
}

impl SessionService {
    /// Builds the service for a connected runtime and loads its sessions.
    pub fn new(
        bundle: &MobileCredentialBundle,
        on_disconnect: Rc<dyn Fn()>,
        on_switch_runtime: Rc<dyn Fn()>,
        on_open_settings: Rc<dyn Fn()>,
        on_runtime_renamed: Rc<dyn Fn(&str)>,
        on_device_renamed: Rc<dyn Fn(&str)>,
    ) -> BackendResult<Self> {
        let backend = bundle.backend()?;
        let terminal_backend: Arc<dyn TerminalBackend> = backend.clone();
        let file_backend: Arc<dyn FileBackend> = backend.clone();
        let git_backend: Arc<dyn GitBackend> = backend.clone();
        let workspace_backend: Arc<dyn WorkspaceBackend> = backend.clone();
        let backend: Arc<dyn AgentBackend> = backend;
        let runtime = bundle.host_label();
        let loaded = load_queues();
        let service = Self {
            bundle_host_id: Rc::new(bundle.host_id().to_owned()),
            backend,
            terminal_backend,
            file_backend,
            git_backend,
            workspace_backend,
            sessions: signal(Vec::new()),
            active: signal(None),
            pinned: signal(load_pins()),
            timeline: signal(Vec::new()),
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
            terminal_title: signal(None),
            terminal_error: signal(None),
            terminal_ref: component_ref::<Terminal>(),
            terminal_id: Rc::new(RefCell::new(None)),
            terminal_pending: Rc::new(RefCell::new(Vec::new())),
            terminal_choices: signal(Vec::new()),
            terminal_menu: signal(false),
            terminal_generation: Rc::new(Cell::new(0)),
            files_open: signal(false),
            files_path: signal(String::new()),
            files_entries: signal(Vec::new()),
            files_preview: signal(None),
            files_error: signal(None),
            files_workspace: Rc::new(RefCell::new(None)),
            files_content: signal(String::new()),
            files_dirty: signal(false),
            files_saving: signal(false),
            files_revision: Rc::new(RefCell::new(None)),
            files_current: Rc::new(RefCell::new(None)),
            git_open: signal(false),
            git_branch: signal(None),
            git_changes: signal(Vec::new()),
            git_diff: signal(None),
            git_error: signal(None),
            git_staged: signal(0),
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
            new_session_open: signal(false),
            new_workspaces: signal(Vec::new()),
            new_runtimes: signal(Vec::new()),
            new_workspace: signal(None),
            new_runtime: signal(None),
            new_worktree_branch: signal(String::new()),
            new_base_ref: signal(String::new()),
            new_project_path: signal(String::new()),
            new_title: signal(String::new()),
            new_prompt: signal(String::new()),
            new_attachments: signal(Vec::new()),
            new_busy: signal(false),
            new_error: signal(None),
            workspace_records: Rc::new(RefCell::new(Vec::new())),
            runtime_options: Rc::new(RefCell::new(Vec::new())),
            resolving: signal(false),
            model: Rc::new(RefCell::new(TimelineModel::default())),
            connection: signal(ConnectionState::Connecting),
            connection_error: signal(None),
            has_older: signal(false),
            loading_older: signal(false),
            alive: Rc::new(Cell::new(true)),
            retry_now: Rc::new(Cell::new(false)),
            drafts: Rc::new(RefCell::new(load_drafts())),
            drafts_snapshot: Rc::new(RefCell::new(String::new())),
            session_states: Rc::new(RefCell::new(BTreeMap::new())),
            expanded_turns: Rc::new(RefCell::new(std::collections::BTreeSet::new())),
            expanded_rows: Rc::new(RefCell::new(std::collections::BTreeSet::new())),
            row_details: Rc::new(RefCell::new(BTreeMap::new())),
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
            queue: Rc::new(RefCell::new(loaded.0)),
            queue_seq: Rc::new(Cell::new(loaded.2)),
            queue_paused_sessions: Rc::new(RefCell::new(loaded.1)),
            queued: signal(Vec::new()),
            queue_paused: signal(false),
            schedule_open: signal(false),
            scheduled: signal(false),
            schedule_generation: Rc::new(Cell::new(0)),
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
        self.terminal_generation
            .set(self.terminal_generation.get().wrapping_add(1));
        self.stash_draft();
        self.persist_drafts();
    }

    /// The runtime this service speaks to.
    pub fn host_id(&self) -> String {
        (*self.bundle_host_id).clone()
    }

    /// Whether a surface of the shell can consume the system back key.
    pub fn back_intercept(&self) -> bool {
        self.session_actions.get_untracked().is_some()
            || self.new_session_open.get_untracked()
            || self.terminal_menu.get_untracked()
            || self.terminal_open.get_untracked()
            || self.files_open.get_untracked()
            || self.git_open.get_untracked()
    }

    /// Closes the top-most surface the shell opened, if any.
    pub fn handle_back(&self) -> bool {
        if self.session_actions.get_untracked().is_some() {
            self.session_actions.set(None);
            return true;
        }
        if self.new_session_open.get_untracked() {
            self.new_session_open.set(false);
            return true;
        }
        if self.terminal_menu.get_untracked() {
            self.terminal_menu.set(false);
            return true;
        }
        if self.terminal_open.get_untracked() {
            self.close_terminal();
            return true;
        }
        if self.files_open.get_untracked() {
            self.files_back();
            return true;
        }
        if self.git_open.get_untracked() {
            self.git_back();
            return true;
        }
        false
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
            on_toggle_pin: {
                let service = self.clone();
                Rc::new(move |id: &str| service.toggle_pin(id))
            },
            timeline: self.timeline,
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
            terminal_title: self.terminal_title,
            terminal_error: self.terminal_error,
            terminal_ref: self.terminal_ref,
            terminal_choices: self.terminal_choices,
            terminal_menu: self.terminal_menu,
            files_open: self.files_open,
            files_path: self.files_path,
            files_entries: self.files_entries,
            files_preview: self.files_preview,
            files_error: self.files_error,
            files_content: self.files_content,
            files_dirty: self.files_dirty,
            files_saving: self.files_saving,
            on_save_file: {
                let service = self.clone();
                Rc::new(move || service.save_file())
            },
            git_open: self.git_open,
            git_branch: self.git_branch,
            git_changes: self.git_changes,
            git_diff: self.git_diff,
            git_error: self.git_error,
            git_staged: self.git_staged,
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
            git_history: self.git_history,
            git_show_history: self.git_show_history,
            git_commit_detail: self.git_commit_detail,
            attachments: self.attachments,
            session_actions: self.session_actions,
            session_rename: self.session_rename,
            session_busy: self.session_busy,
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
                Rc::new(move || service.refresh())
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
                    service.draft.set(text.to_owned());
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
                Rc::new(move |id: &str, value: &str| service.choose_feature(id, value))
            },
            on_toggle_turn: {
                let service = self.clone();
                Rc::new(move |turn_id: &str| service.toggle_turn(turn_id))
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
            on_create_session: {
                let service = self.clone();
                Rc::new(move || service.create_new_session())
            },
            on_create_temporary_session: {
                let service = self.clone();
                Rc::new(move || service.create_temporary_session())
            },
            new_project_path: self.new_project_path,
            on_open_project: {
                let service = self.clone();
                Rc::new(move || service.open_project())
            },
            on_session_menu: {
                let service = self.clone();
                Rc::new(move |id: &str, title: &str| {
                    service.session_rename.set(title.to_owned());
                    service.session_actions.set(Some(id.to_owned()));
                })
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
            on_close_terminal: {
                let service = self.clone();
                Rc::new(move || service.close_terminal())
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
            on_terminal_menu: {
                let service = self.clone();
                Rc::new(move || service.terminal_menu.set(true))
            },
            on_close_terminal_menu: {
                let service = self.clone();
                Rc::new(move || service.terminal_menu.set(false))
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
            on_close_files: {
                let service = self.clone();
                Rc::new(move || service.close_files())
            },
            on_open_dir: {
                let service = self.clone();
                Rc::new(move |path: &str| service.load_dir(path.to_owned()))
            },
            on_open_file: {
                let service = self.clone();
                Rc::new(move |path: &str| service.open_file(path.to_owned()))
            },
            on_files_back: {
                let service = self.clone();
                Rc::new(move || service.files_back())
            },
            on_open_git: {
                let service = self.clone();
                Rc::new(move || service.open_git())
            },
            on_close_git: {
                let service = self.clone();
                Rc::new(move || service.close_git())
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
                Rc::new(move |path: &str| service.revert_path(path))
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
        let result = background(async move { backend.list_sessions(false).await }).await;
        match result {
            Ok(Ok(sessions)) => {
                let spaces = background(async move { workspaces.list_workspaces().await })
                    .await
                    .ok()
                    .and_then(Result::ok)
                    .unwrap_or_default();
                *self.workspace_records.borrow_mut() = spaces.clone();
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
                    && (running || service.terminal_open.get_untracked());
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
                let row = summary(&session, &self.workspace_records.borrow());
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
                self.session_states
                    .borrow_mut()
                    .retain(|id, _| !ids.contains(id));
                self.drafts.borrow_mut().retain(|id, _| !ids.contains(id));
                self.queue.borrow_mut().retain(|id, _| !ids.contains(id));
                self.persist_queues();
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
                        match self.files_current.borrow().clone() {
                            Some(path) => self.open_file(path),
                            None => self.load_dir(self.files_path.get_untracked()),
                        }
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
        // Each session owns its draft; switching saves the one being left
        // behind and brings back what the new one had.
        let previous = self.active.get_untracked();
        if previous.as_deref() != Some(session_id.as_str()) {
            self.stash_draft();
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
        *self.files_workspace.borrow_mut() = None;
        self.files_open.set(false);
        self.git_open.set(false);
        self.terminal_open.set(false);
        self.terminal_menu.set(false);
        self.active.set(Some(session_id.as_str().to_owned()));
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
        self.publish_queue();
        self.load_cached_timeline(&session_id);
        self.refresh_runtime(&session_id);
        self.refresh_token_usage(&session_id);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let result = background(fetch_page(backend, session_id.clone())).await;
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("opening the session did not finish: {error}"),
                )),
            }
        });
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
            let selection =
                background(async move { selection_backend.runtime_selection(selection_id).await })
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
            let result = background(async move {
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
            let result = background(async move {
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
        let option = catalog
            .options
            .iter()
            .find(|option| &option.selection == selection);
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
                    agent: option.agent_label.clone(),
                    model: option.model_label.clone(),
                    detail: option.auth_source_label.clone(),
                    selected: &option.selection == selection,
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
                            selected: feature.kind == SessionRuntimeFeatureKind::Toggle
                                && current.as_deref() == Some("true"),
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

    fn choose_feature(&self, id: &str, value: &str) {
        let Some(mut selection) = self.current_selection() else {
            return;
        };
        selection
            .config_values
            .insert(id.to_owned(), value.to_owned());
        self.apply_selection(selection);
    }

    /// Puts text another app shared into the composer, or starts a session
    /// with it when none is open.
    pub fn receive_shared_text(&self, text: String) {
        if self.active.get_untracked().is_some() {
            self.draft.set(text);
        } else {
            self.new_prompt.set(text);
            self.open_new_session();
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
                backend
                    .set_desired_runtime(MutationRequest::new(
                        SetDesiredAgentSessionRuntimeRequest {
                            session_id: session_id.clone(),
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
            let result = background(async move {
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
        self.schedule_open.set(false);
        self.scheduled.set(true);
        let generation = self.schedule_generation.get().wrapping_add(1);
        self.schedule_generation.set(generation);
        let service = self.clone();
        spawn(async move {
            timer(Duration::from_secs(seconds)).await;
            if service.schedule_generation.get() != generation
                || service.active.get_untracked().as_deref() != Some(active.as_str())
            {
                return;
            }
            service.scheduled.set(false);
            if service.draft.get_untracked() == text {
                service.draft.set(String::new());
                service.attachments.set(Vec::new());
            }
            service.armed_mentions.borrow_mut().clear();
            service.deliver(text, attachments, mentions, false);
        });
    }

    fn cancel_scheduled(&self) {
        self.schedule_generation
            .set(self.schedule_generation.get().wrapping_add(1));
        self.scheduled.set(false);
    }

    fn enqueue(
        &self,
        session: String,
        text: String,
        attachments: Vec<AttachmentItem>,
        mentions: Vec<VibexUseMention>,
    ) {
        let id = self.queue_seq.get().wrapping_add(1);
        self.queue_seq.set(id);
        self.queue
            .borrow_mut()
            .entry(session.clone())
            .or_default()
            .push(QueuedMessage {
                id,
                text: text.clone(),
                attachments,
                mentions,
            });
        self.draft.set(String::new());
        self.attachments.set(Vec::new());
        self.publish_queue();
        self.persist_queues();
        crate::settings::haptic(nsh_core::Haptic::Selection);
    }

    fn publish_queue(&self) {
        let Some(active) = self.active.get_untracked() else {
            self.queued.set(Vec::new());
            self.queue_paused.set(false);
            return;
        };
        self.queued.set(
            self.queue
                .borrow()
                .get(&active)
                .map(|queue| {
                    queue
                        .iter()
                        .map(|message| QueuedMessageView {
                            id: message.id,
                            text: message.text.clone(),
                            attachment_count: message.attachments.len(),
                        })
                        .collect()
                })
                .unwrap_or_default(),
        );
        self.queue_paused
            .set(self.queue_paused_sessions.borrow().contains(&active));
    }

    fn persist_queues(&self) {
        let sessions = self
            .queue
            .borrow()
            .iter()
            .map(|(session, messages)| {
                (
                    session.clone(),
                    messages
                        .iter()
                        .map(|message| StoredQueued {
                            id: message.id,
                            text: message.text.clone(),
                            attachments: message
                                .attachments
                                .iter()
                                .map(|attachment| StoredAttachment {
                                    label: attachment.label.clone(),
                                    mime_type: attachment.mime_type.clone(),
                                    uri: attachment.uri.clone(),
                                })
                                .collect(),
                            mentions: message.mentions.clone(),
                        })
                        .collect(),
                )
            })
            .collect();
        let stored = StoredQueues {
            sessions,
            paused: self
                .queue_paused_sessions
                .borrow()
                .iter()
                .cloned()
                .collect(),
        };
        if let Ok(text) = serde_json::to_string(&stored) {
            let _ = Storage::set_item(QUEUE_KEY, text);
        }
    }

    fn send_queued(&self, id: u64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let message = {
            let mut queue = self.queue.borrow_mut();
            let Some(queue) = queue.get_mut(&active) else {
                return;
            };
            let Some(index) = queue.iter().position(|message| message.id == id) else {
                return;
            };
            queue.remove(index)
        };
        self.publish_queue();
        self.persist_queues();
        self.deliver(message.text, message.attachments, message.mentions, false);
    }

    fn move_queued(&self, id: u64, delta: isize) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let mut queues = self.queue.borrow_mut();
        let Some(queue) = queues.get_mut(&active) else {
            return;
        };
        let Some(index) = queue.iter().position(|message| message.id == id) else {
            return;
        };
        let target = index as isize + delta;
        if target < 0 || target >= queue.len() as isize {
            return;
        }
        queue.swap(index, target as usize);
        drop(queues);
        self.publish_queue();
        self.persist_queues();
    }

    fn remove_queued(&self, id: u64) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        if let Some(queue) = self.queue.borrow_mut().get_mut(&active) {
            queue.retain(|message| message.id != id);
        }
        self.publish_queue();
        self.persist_queues();
    }

    fn clear_queue(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        self.queue.borrow_mut().remove(&active);
        self.publish_queue();
        self.persist_queues();
    }

    fn toggle_queue_paused(&self) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let mut paused = self.queue_paused_sessions.borrow_mut();
        if !paused.remove(&active) {
            paused.insert(active);
        }
        drop(paused);
        self.publish_queue();
        self.persist_queues();
    }

    fn flush_queue(&self) {
        if self.queue_paused.get_untracked() || self.turn_live() || self.sending.get_untracked() {
            return;
        }
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let message = {
            let mut queue = self.queue.borrow_mut();
            let Some(queue) = queue.get_mut(&active) else {
                return;
            };
            if queue.is_empty() {
                return;
            }
            queue.remove(0)
        };
        self.publish_queue();
        self.persist_queues();
        self.deliver(message.text, message.attachments, message.mentions, false);
    }

    fn requeue(
        &self,
        text: String,
        attachments: Vec<AttachmentItem>,
        mentions: Vec<VibexUseMention>,
    ) {
        let Some(active) = self.active.get_untracked() else {
            return;
        };
        let id = self.queue_seq.get().wrapping_add(1);
        self.queue_seq.set(id);
        self.queue.borrow_mut().entry(active).or_default().insert(
            0,
            QueuedMessage {
                id,
                text,
                attachments,
                mentions,
            },
        );
        self.publish_queue();
        self.persist_queues();
    }

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
        let retry_text = text.clone();
        let retry_attachments = attachments.clone();
        let retry_mentions = mentions.clone();
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
            let result = background(send_and_refetch(
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
                    service.flush_queue();
                }
                Ok(Err(error)) => {
                    if !from_composer {
                        service.requeue(retry_text, retry_attachments, retry_mentions.clone());
                    }
                    service.fail(&error);
                }
                Err(error) => {
                    if !from_composer {
                        service.requeue(retry_text, retry_attachments, retry_mentions.clone());
                    }
                    service.fail(&BackendError::failed(
                        "session_task_failed",
                        format!("sending the message did not finish: {error}"),
                    ));
                }
            }
        });
    }

    /// Projects a fetched page into the shell's rows.
    fn apply_page(&self, session_id: &VibexSessionId, page: TimelinePage) {
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
            let result =
                background(async move { backend.session_token_usage(requested).await }).await;
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
            let result = background(fetch_older(backend, session_id.clone(), before)).await;
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
        let turns = model.conversation_turns(state, state == Some(AgentSessionState::Running));
        let mut entries = Vec::new();
        for turn in &turns {
            if let Some(row) = turn.user_row.as_ref() {
                entries.push(self.entry_from_row(row, turn, &model.items));
            }
            if !turn.process_rows.is_empty() {
                let expanded = self.expanded_turns.borrow().contains(&turn.id)
                    || (turn.conclusion_row.is_none() && (!turn.complete || turn.superseded));
                entries.push(process_header(turn, expanded));
                if expanded {
                    for row in &turn.process_rows {
                        entries.push(self.entry_from_row(row, turn, &model.items));
                    }
                }
            }
            if let Some(row) = turn.conclusion_row.as_ref() {
                entries.push(self.entry_from_row(row, turn, &model.items));
            } else if (!turn.complete || turn.superseded)
                && self.show_generation_status.get_untracked()
            {
                entries.push(live_turn_entry(turn));
            }
            let files = turn
                .process_rows
                .iter()
                .filter(|row| row.kind == vibex_desktop_model::TimelineRowKind::FileOperation)
                .map(|row| row.title.clone())
                .collect::<Vec<_>>();
            if !files.is_empty() {
                entries.push(changes_entry(turn, &files));
            }
        }
        let latest_ended_normally = turns
            .last()
            .is_some_and(|turn| turn.complete && !turn.failed && !turn.pending_permission);
        let needs_continue = state.is_some_and(|state| {
            vibex_core::agent_session_turn_requires_continuation(state, Some(latest_ended_normally))
        });
        let plan = plan_view(&model.items);
        let goal = goal_view(&model.items);
        entries.retain(|entry| {
            entry.kind != EntryKind::Reasoning || self.show_reasoning.get_untracked()
        });
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
        let expandable = matches!(
            row.kind,
            vibex_desktop_model::TimelineRowKind::ToolCall
                | vibex_desktop_model::TimelineRowKind::Command
                | vibex_desktop_model::TimelineRowKind::FileOperation
                | vibex_desktop_model::TimelineRowKind::WebSearch
                | vibex_desktop_model::TimelineRowKind::TodoUpdate
        );
        let expanded = self.expanded_rows.borrow().contains(&row.id);
        let body = if expanded {
            self.row_details
                .borrow()
                .get(&row.id)
                .cloned()
                .unwrap_or_else(|| row.body.clone())
        } else {
            row.body.clone()
        };
        TimelineEntry {
            id: row.id.clone(),
            kind: entry_kind(row.kind),
            title: row.title.clone(),
            body,
            meta: row_meta(row, items),
            streaming: row.streaming,
            turn_id: turn.id.clone(),
            process_count: turn.process_rows.len(),
            expanded,
            expandable,
            attachments: row_attachments(row, items),
            last_sequence: row.last_sequence,
            started_at_ms: turn.started_at_ms,
            ended_at_ms: turn.ended_at_ms.unwrap_or(0),
            complete: turn.complete,
        }
    }

    fn toggle_turn(&self, turn_id: &str) {
        let mut expanded = self.expanded_turns.borrow_mut();
        if !expanded.remove(turn_id) {
            expanded.insert(turn_id.to_owned());
        }
        drop(expanded);
        self.publish();
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
        if self.row_details.borrow().contains_key(row_id) || sequence <= 0 {
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
        spawn(async move {
            let result = background(async move {
                backend
                    .fetch_timeline_item(FetchTimelineItemRequest {
                        session_id,
                        sequence,
                    })
                    .await
            })
            .await;
            match result {
                Ok(Ok(item)) => {
                    let body = vibex_desktop_model::timeline_rows(std::slice::from_ref(&item))
                        .into_iter()
                        .next()
                        .map(|row| row.body)
                        .unwrap_or_default();
                    service.row_details.borrow_mut().insert(row_id, body);
                    service.publish();
                }
                Ok(Err(error)) => service.fail(&error),
                Err(error) => service.fail(&BackendError::failed(
                    "session_task_failed",
                    format!("reading the activity did not finish: {error}"),
                )),
            }
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
        let service = self.clone();
        spawn(async move {
            let mut request = PickRequest::new(true, 4);
            request.max_dimension = Some(nsh_core::geometry::DevicePixels(1600));
            let files = pick_images(request).await;
            service.accept_attachments(files);
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
        let service = self.clone();
        spawn(async move {
            let mut request = FilePickRequest::new(true, 4);
            request.max_size = Some(8 * 1024 * 1024);
            let files = pick_files(request).await;
            service.accept_attachments(files);
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
            let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
        let Some(base) = self
            .workspace_records
            .borrow()
            .iter()
            .find(|space| space.workspace.id.as_str() == workspace_id)
            .map(|space| space.workspace.id.clone())
        else {
            return;
        };
        let Some(option) = self
            .runtime_options
            .borrow()
            .iter()
            .filter(|option| option.availability == RuntimeOptionAvailability::Available)
            .nth(index)
            .cloned()
        else {
            return;
        };
        self.new_busy.set(true);
        self.new_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let created = background(async move {
                git.git_worktree_create(MutationRequest::new(GitWorktreeCreateRequest {
                    workspace_id: base,
                    branch_name: branch,
                    base_ref: {
                        let base = service.new_base_ref.get_untracked().trim().to_owned();
                        (!base.is_empty()).then_some(base)
                    },
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
                title: {
                    let title = service.new_title.get_untracked().trim().to_owned();
                    (!title.is_empty()).then_some(title)
                },
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result =
                background(
                    async move { backend.create_session(MutationRequest::new(request)).await },
                )
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    let prompt = service.new_prompt.get_untracked().trim().to_owned();
                    service.new_session_open.set(false);
                    service.new_worktree_branch.set(String::new());
                    service.new_base_ref.set(String::new());
                    service.refresh();
                    service.open(session.id.as_str());
                    if !prompt.is_empty() {
                        let attachments = service.new_attachments.get_untracked();
                        service.deliver(prompt, attachments, Vec::new(), false);
                    }
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
            let result = background(async move {
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
                    let row = summary(&session, &service.workspace_records.borrow());
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
                let result = background(async move {
                    call.archive_session(MutationRequest::new(session_id)).await
                })
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
                let result = background(async move {
                    call.delete_session(MutationRequest::new(session_id)).await
                })
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

    pub fn toggle_pin(&self, id: &str) {
        self.pinned.update(|pinned| {
            if let Some(index) = pinned.iter().position(|candidate| candidate == id) {
                pinned.remove(index);
            } else {
                pinned.push(id.to_owned());
            }
        });
        if let Ok(text) = serde_json::to_string(&self.pinned.get_untracked()) {
            let _ = Storage::set_item(PINS_KEY, text);
        }
        self.session_actions.set(None);
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
            let result = background(async move {
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
            let result = background(async move {
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
        self.drafts.borrow_mut().remove(id);
        self.persist_drafts();
        self.pinned
            .update(|pinned| pinned.retain(|candidate| candidate != id));
        if let Ok(text) = serde_json::to_string(&self.pinned.get_untracked()) {
            let _ = Storage::set_item(PINS_KEY, text);
        }
        self.queue.borrow_mut().remove(id);
        self.persist_queues();
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

    /// Opens the new-session sheet with the runtime's choices.
    pub fn open_new_session(&self) {
        self.files_open.set(false);
        self.git_open.set(false);
        self.terminal_open.set(false);
        self.new_session_open.set(true);
        self.new_error.set(None);
        self.new_title.set(String::new());
        self.new_prompt.set(String::new());
        self.new_attachments.set(Vec::new());
        self.new_busy.set(true);
        let service = self.clone();
        let workspaces = self.workspace_backend.clone();
        let catalog_backend = self.backend.clone();
        spawn(async move {
            let result = background(async move {
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
                    let runtime_choices = catalog
                        .options
                        .iter()
                        .filter(|option| {
                            option.availability == RuntimeOptionAvailability::Available
                        })
                        .enumerate()
                        .map(|(index, option)| RuntimeChoice {
                            id: index.to_string(),
                            title: format!("{} · {}", option.agent_label, option.model_label),
                            detail: option.auth_source_label.clone(),
                        })
                        .collect::<Vec<_>>();
                    if workspace_choices.is_empty() || runtime_choices.is_empty() {
                        service.new_error.set(Some(
                            copy::text(
                                "No workspace or ready Agent is available on this runtime.",
                                "该运行时没有可用的工作区或就绪的 Agent。",
                                "該執行環境沒有可用的工作區或就緒的 Agent。",
                            )
                            .into(),
                        ));
                    }
                    service
                        .new_workspace
                        .set(workspace_choices.first().map(|choice| choice.id.clone()));
                    service
                        .new_runtime
                        .set(runtime_choices.first().map(|choice| choice.id.clone()));
                    service.new_workspaces.set(workspace_choices);
                    service.new_runtimes.set(runtime_choices);
                    *service.workspace_records.borrow_mut() = spaces;
                    *service.runtime_options.borrow_mut() = catalog.options;
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
        let workspace = self
            .workspace_records
            .borrow()
            .iter()
            .find(|space| space.workspace.id.as_str() == workspace_id)
            .cloned();
        let option = self
            .runtime_options
            .borrow()
            .iter()
            .filter(|option| option.availability == RuntimeOptionAvailability::Available)
            .nth(index)
            .cloned();
        let (Some(workspace), Some(option)) = (workspace, option) else {
            return;
        };
        self.new_busy.set(true);
        self.new_error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        spawn(async move {
            let request = CreateAgentSessionRequest {
                runtime: option.selection,
                workspace_root: workspace.workspace.root_path.clone(),
                workspace_mode: workspace.workspace.mode,
                title: {
                    let title = service.new_title.get_untracked().trim().to_owned();
                    (!title.is_empty()).then_some(title)
                },
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result =
                background(
                    async move { backend.create_session(MutationRequest::new(request)).await },
                )
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    let prompt = service.new_prompt.get_untracked().trim().to_owned();
                    service.new_session_open.set(false);
                    service.refresh();
                    service.open(session.id.as_str());
                    if !prompt.is_empty() {
                        let attachments = service.new_attachments.get_untracked();
                        service.deliver(prompt, attachments, Vec::new(), false);
                    }
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("creating the session did not finish: {error}").into(),
                )),
            }
        });
    }

    fn open_project(&self) {
        let path = self.new_project_path.get_untracked().trim().to_owned();
        if path.is_empty() || self.new_busy.get_untracked() {
            return;
        }
        self.new_busy.set(true);
        self.new_error.set(None);
        let service = self.clone();
        let workspaces = self.workspace_backend.clone();
        spawn(async move {
            let open_workspaces = workspaces.clone();
            let opened = background(async move {
                open_workspaces
                    .open_workspace(MutationRequest::new(OpenWorkspaceRequest {
                        root_path: path,
                        mode: None,
                    }))
                    .await
            })
            .await;
            let opened = match opened {
                Ok(Ok(workspace)) => workspace,
                Ok(Err(error)) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(error_text(&error)));
                    return;
                }
                Err(error) => {
                    service.new_busy.set(false);
                    service.new_error.set(Some(
                        format!("opening the project did not finish: {error}").into(),
                    ));
                    return;
                }
            };
            let spaces = background(async move { workspaces.list_workspaces().await }).await;
            service.new_busy.set(false);
            match spaces {
                Ok(Ok(spaces)) => {
                    let choices = spaces
                        .iter()
                        .map(|space| WorkspaceChoice {
                            id: space.workspace.id.as_str().to_owned(),
                            name: space.project.name.clone(),
                            detail: space.workspace.root_path.clone(),
                        })
                        .collect::<Vec<_>>();
                    service
                        .new_workspace
                        .set(Some(opened.workspace.id.as_str().to_owned()));
                    service.new_workspaces.set(choices);
                    *service.workspace_records.borrow_mut() = spaces;
                    service.new_project_path.set(String::new());
                }
                Ok(Err(error)) => service.new_error.set(Some(error_text(&error))),
                Err(error) => service.new_error.set(Some(
                    format!("listing the projects did not finish: {error}").into(),
                )),
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
        let option = self
            .runtime_options
            .borrow()
            .iter()
            .filter(|option| option.availability == RuntimeOptionAvailability::Available)
            .nth(index)
            .cloned();
        let Some(option) = option else {
            return;
        };
        self.new_busy.set(true);
        self.new_error.set(None);
        let service = self.clone();
        let backend = self.backend.clone();
        let workspaces = self.workspace_backend.clone();
        spawn(async move {
            let root =
                background(async move { workspaces.ensure_temporary_session_root().await }).await;
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
                title: {
                    let title = service.new_title.get_untracked().trim().to_owned();
                    (!title.is_empty()).then_some(title)
                },
                safety: None,
                session_id: Some(VibexSessionId::new()),
                defer_runtime_materialization: false,
                tools: Default::default(),
            };
            let result =
                background(
                    async move { backend.create_session(MutationRequest::new(request)).await },
                )
                .await;
            service.new_busy.set(false);
            match result {
                Ok(Ok(session)) => {
                    let prompt = service.new_prompt.get_untracked().trim().to_owned();
                    service.new_session_open.set(false);
                    service.refresh();
                    service.open(session.id.as_str());
                    if !prompt.is_empty() {
                        let attachments = service.new_attachments.get_untracked();
                        service.deliver(prompt, attachments, Vec::new(), false);
                    }
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
        let Ok(active_id) = VibexSessionId::parse(active) else {
            return;
        };
        self.terminal_open.set(true);
        self.terminal_error.set(None);
        self.terminal_menu.set(false);
        self.terminal_title
            .set(Some(copy::text("Terminal", "终端", "終端").into()));
        let service = self.clone();
        let backend = self.backend.clone();
        let terminal_backend = self.terminal_backend.clone();
        let opening = terminal_backend.clone();
        spawn(async move {
            let result = background(async move {
                let session = backend.open_session(active_id).await?;
                let terminals = opening.list_terminals(session.workspace_id.clone()).await?;
                Ok::<_, BackendError>((session.workspace_id, terminals))
            })
            .await;
            let (workspace, terminals) = match result {
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
            service.set_terminal_choices(&terminals);
            if let Some(terminal) = terminals
                .iter()
                .find(|terminal| terminal.status == TerminalStatus::Running)
                .or_else(|| terminals.first())
            {
                service.attach_terminal(terminal.id.clone(), terminal.title.clone());
                return;
            }
            let created = background(async move {
                terminal_backend
                    .create_terminal(MutationRequest::new(TerminalCreateRequest {
                        workspace_id: workspace,
                        title: None,
                        shell: None,
                        cwd: None,
                        rows: 24,
                        cols: 80,
                    }))
                    .await
            })
            .await;
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
        let generation = self.terminal_generation.get().wrapping_add(1);
        self.terminal_generation.set(generation);
        self.terminal_pending.borrow_mut().clear();
        self.reset_terminal_output();
        *self.terminal_id.borrow_mut() = Some(id.clone());
        self.terminal_title.set(Some(title.into()));
        self.terminal_error.set(None);
        let service = self.clone();
        let terminal_backend = self.terminal_backend.clone();
        spawn(async move {
            let snapshot = background({
                let backend = terminal_backend.clone();
                let id = id.clone();
                async move { backend.terminal_snapshot(id).await }
            })
            .await;
            let snapshot = match snapshot {
                Ok(Ok(snapshot)) => snapshot,
                Ok(Err(error)) => {
                    service.terminal_error.set(Some(error_text(&error)));
                    return;
                }
                Err(error) => {
                    service.terminal_error.set(Some(
                        format!("reading the terminal did not finish: {error}").into(),
                    ));
                    return;
                }
            };
            if service.terminal_generation.get() != generation {
                return;
            }
            for chunk in snapshot.chunks {
                service.write_terminal_output(chunk.data);
            }
            let subscription = match terminal_backend.subscribe_terminal(id, snapshot.next_sequence)
            {
                Ok(subscription) => subscription,
                Err(error) => {
                    service.terminal_error.set(Some(error_text(&error)));
                    return;
                }
            };
            let mut subscription = subscription;
            while service.terminal_generation.get() == generation {
                match subscription.next().await {
                    Ok(Some(batch)) => {
                        if batch.reset_required {
                            service.reset_terminal_output();
                        }
                        for frame in batch.frames {
                            let text = service.decode_terminal_frame(&frame.bytes);
                            service.write_terminal_output(text);
                        }
                    }
                    Ok(None) | Err(_) => break,
                }
            }
        });
    }

    /// Starts another terminal in the workspace and shows it.
    fn new_terminal(&self) {
        let service = self.clone();
        let terminal_backend = self.terminal_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let created = background(async move {
                terminal_backend
                    .create_terminal(MutationRequest::new(TerminalCreateRequest {
                        workspace_id: workspace,
                        title: None,
                        shell: None,
                        cwd: None,
                        rows: 24,
                        cols: 80,
                    }))
                    .await
            })
            .await;
            match created {
                Ok(Ok(terminal)) => {
                    service
                        .terminal_choices
                        .update(|choices| push_terminal_choice(choices, &terminal));
                    service.attach_terminal(terminal.id, terminal.title);
                    service.terminal_menu.set(false);
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
        self.terminal_menu.set(false);
        self.attach_terminal(id, title);
    }

    /// Replaces the terminal list from a backend listing.
    fn set_terminal_choices(&self, terminals: &[vibex_core::TerminalSession]) {
        let choices = terminals.iter().map(terminal_choice).collect::<Vec<_>>();
        self.terminal_choices.set(choices);
    }

    /// Closes the terminal surface; the PTY keeps running on the runtime.
    fn close_terminal(&self) {
        self.terminal_generation
            .set(self.terminal_generation.get().wrapping_add(1));
        self.terminal_open.set(false);
        self.terminal_menu.set(false);
        *self.terminal_id.borrow_mut() = None;
    }

    /// Sends what the person typed to the PTY.
    fn terminal_input(&self, data: &str) {
        let Some(terminal_id) = self.terminal_id.borrow().clone() else {
            return;
        };
        let data = data.to_owned();
        let service = self.clone();
        let backend = self.terminal_backend.clone();
        spawn(async move {
            let result = background(async move {
                backend
                    .write_terminal(MutationRequest::new(TerminalWriteRequest {
                        terminal_id,
                        data,
                    }))
                    .await
            })
            .await;
            match result {
                Ok(Ok(())) => {}
                Ok(Err(error)) => service.terminal_error.set(Some(error_text(&error))),
                Err(error) => service.terminal_error.set(Some(
                    format!("writing to the terminal did not finish: {error}").into(),
                )),
            }
        });
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
    }

    /// Decodes a frame, keeping a character split across frames for the next
    /// one instead of writing a replacement mark.
    fn decode_terminal_frame(&self, bytes: &[u8]) -> String {
        decode_terminal_bytes(&mut self.terminal_pending.borrow_mut(), bytes)
    }

    /// Tells the runtime the grid's new size.
    fn terminal_resize(&self, cols: u32, rows: u32) {
        let Some(terminal_id) = self.terminal_id.borrow().clone() else {
            return;
        };
        let backend = self.terminal_backend.clone();
        spawn(async move {
            let _ = background(async move {
                backend
                    .resize_terminal(MutationRequest::new(TerminalResizeRequest {
                        terminal_id,
                        rows: rows.min(u32::from(u16::MAX)) as u16,
                        cols: cols.min(u32::from(u16::MAX)) as u16,
                    }))
                    .await
            })
            .await;
        });
    }

    /// Opens the workspace's files over the shell, at the root.
    fn open_files(&self) {
        if self.files_open.get_untracked() {
            return;
        }
        self.files_open.set(true);
        self.files_preview.set(None);
        self.files_error.set(None);
        self.load_dir(String::new());
    }

    fn close_files(&self) {
        self.files_open.set(false);
        self.files_preview.set(None);
        self.files_error.set(None);
        *self.files_current.borrow_mut() = None;
    }

    /// Lists `path` (the workspace root when empty).
    fn load_dir(&self, path: String) {
        self.files_path.set(path.clone());
        self.files_preview.set(None);
        self.files_error.set(None);
        *self.files_current.borrow_mut() = None;
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
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
            let tree_path = if path.is_empty() { None } else { Some(path) };
            let result = background(async move {
                file_backend
                    .file_tree(FileTreeRequest {
                        workspace_id: workspace,
                        path: tree_path,
                        max_depth: Some(1),
                        include_hidden: false,
                    })
                    .await
            })
            .await;
            match result {
                Ok(Ok(entries)) => {
                    let mut rows = entries
                        .into_iter()
                        .map(|entry| FileEntry {
                            path: entry.path,
                            name: entry.name,
                            directory: entry.kind == vibex_core::FileEntryKind::Directory,
                        })
                        .collect::<Vec<_>>();
                    rows.sort_by(|left, right| {
                        right
                            .directory
                            .cmp(&left.directory)
                            .then_with(|| left.name.cmp(&right.name))
                    });
                    service.files_entries.set(rows);
                }
                Ok(Err(error)) => service.files_error.set(Some(error_text(&error))),
                Err(error) => service.files_error.set(Some(
                    format!("listing the files did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Reads a file into the preview.
    fn open_file(&self, path: String) {
        self.files_error.set(None);
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let read_workspace = workspace.clone();
            let result = background(async move {
                file_backend
                    .read_file(FileReadRequest {
                        workspace_id: read_workspace,
                        path,
                        max_bytes: Some(512 * 1024),
                    })
                    .await
            })
            .await;
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
                        background(async move {
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
                    *service.files_current.borrow_mut() = Some(file.path);
                    *service.files_revision.borrow_mut() = Some(file.content_revision);
                    service
                        .files_content
                        .set(file.content.clone().unwrap_or_default());
                    service.files_dirty.set(false);
                    service.files_preview.set(Some(FilePreview {
                        title: file.name,
                        body: file.content.unwrap_or_default(),
                        truncated: file.truncated,
                        language: file.language.unwrap_or_default(),
                        kind,
                        image_uri,
                    }));
                }
                Ok(Err(error)) => service.files_error.set(Some(error_text(&error))),
                Err(error) => service.files_error.set(Some(
                    format!("reading the file did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Writes the edited text back, guarded by the revision it was read at.
    fn save_file(&self) {
        if self.files_saving.get_untracked() {
            return;
        }
        if self.files_preview.get_untracked().is_some_and(|file| {
            !matches!(
                file.kind,
                FilePreviewKindView::Text | FilePreviewKindView::Markdown
            )
        }) {
            return;
        }
        let Some(path) = self.files_current.borrow().clone() else {
            return;
        };
        let revision = self.files_revision.borrow().clone();
        let content = self.files_content.get_untracked();
        self.files_saving.set(true);
        self.files_error.set(None);
        let service = self.clone();
        let file_backend = self.file_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.files_saving.set(false);
                return;
            };
            let result = background(async move {
                file_backend
                    .write_file(MutationRequest::new(vibex_core::FileWriteRequest {
                        workspace_id: workspace,
                        path,
                        content,
                        create_if_missing: false,
                        expected_revision: revision,
                        encoding: None,
                        line_ending: None,
                    }))
                    .await
            })
            .await;
            service.files_saving.set(false);
            match result {
                Ok(Ok(file)) => {
                    *service.files_revision.borrow_mut() = Some(file.content_revision);
                    service.files_content.set(file.content.unwrap_or_default());
                    service.files_dirty.set(false);
                }
                Ok(Err(error)) => service.files_error.set(Some(error_text(&error))),
                Err(error) => service.files_error.set(Some(
                    format!("saving the file did not finish: {error}").into(),
                )),
            }
        });
    }

    /// Leaves the preview, then the directory, then the surface.
    fn files_back(&self) {
        if self.files_preview.get_untracked().is_some() {
            self.files_preview.set(None);
            self.files_dirty.set(false);
            *self.files_current.borrow_mut() = None;
            return;
        }
        let path = self.files_path.get_untracked();
        if path.is_empty() {
            self.close_files();
            return;
        }
        let parent = path
            .rsplit_once('/')
            .map(|(parent, _)| parent.to_owned())
            .unwrap_or_default();
        self.load_dir(parent);
    }

    /// The open session's workspace, fetched once.
    async fn workspace(&self, agent: &Arc<dyn AgentBackend>) -> Option<WorkspaceId> {
        if let Some(workspace) = self.files_workspace.borrow().clone() {
            return Some(workspace);
        }
        let active = self.active.get_untracked()?;
        let session_id = VibexSessionId::parse(active).ok()?;
        let session = agent.open_session(session_id).await.ok()?;
        let workspace = session.workspace_id;
        *self.files_workspace.borrow_mut() = Some(workspace.clone());
        Some(workspace)
    }

    /// Opens the workspace's Git status over the shell.
    fn open_git(&self) {
        self.files_open.set(false);
        self.terminal_open.set(false);
        self.git_open.set(true);
        self.git_diff.set(None);
        self.git_amend.set(false);
        self.git_push_after.set(false);
        self.load_git_branches();
        self.refresh_git();
    }

    fn close_git(&self) {
        self.git_open.set(false);
        self.git_diff.set(None);
        self.git_error.set(None);
    }

    /// Reloads the status list.
    fn refresh_git(&self) {
        self.git_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
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
            let result = background(async move { git.git_status(workspace).await }).await;
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
        self.git_staged.set(status.staged_count);
        self.git_branch.set(
            status
                .branch
                .clone()
                .map(SharedString::from)
                .map(Some)
                .unwrap_or(None),
        );
        self.git_changes.set(
            status
                .changes
                .into_iter()
                .map(|change| GitChangeRow {
                    status: change_letter(change.kind).to_owned(),
                    path: change.path,
                    staged: change.staged,
                    additions: change.additions,
                    deletions: change.deletions,
                })
                .collect(),
        );
    }

    /// Reads a path's diff into the preview.
    fn open_git_diff(&self, path: &str, staged: bool) {
        self.git_error.set(None);
        let path = path.to_owned();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let title = path.clone();
            let result = background(async move {
                git.git_diff(GitDiffRequest {
                    workspace_id: workspace,
                    path,
                    staged,
                })
                .await
            })
            .await;
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
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        let path = path.to_owned();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = background(async move {
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
            let result =
                background(async move { git.git_worktree_snapshot(workspace).await }).await;
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
            let preflight =
                background(
                    async move { preflight_git.git_worktree_archive_preflight(archive).await },
                )
                .await;
            match preflight {
                Ok(Ok(preflight)) if preflight.allowed => {
                    let request = GitWorktreeArchiveRequest {
                        workspace_id: workspace,
                        worktree_path: record.worktree_path.clone(),
                        expected_head: record.head.clone(),
                        preflight_revision: Some(preflight.revision),
                    };
                    let result = background(async move {
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
            let result = background(async move {
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
            let result = background(async move {
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
            let preflight =
                background(
                    async move { preflight_git.git_worktree_discard_preflight(request).await },
                )
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
                    let result = background(async move {
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
            let plan =
                background(async move { plan_git.git_worktree_merge_plan(plan_request).await })
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
                    let result = background(async move {
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
            let preflight =
                background(
                    async move { preflight_git.git_worktree_restore_preflight(restore).await },
                )
                .await;
            match preflight {
                Ok(Ok(preflight)) if preflight.allowed => {
                    let request = GitWorktreeRestoreRequest {
                        workspace_id: workspace,
                        worktree_id,
                        preflight_revision: Some(preflight.revision),
                    };
                    let result = background(async move {
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
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = background(async move { git.git_branch_list(workspace).await }).await;
            match result {
                Ok(Ok(list)) => service.git_branches.set(
                    list.branches
                        .into_iter()
                        .map(|branch| BranchRow {
                            name: branch.name,
                            current: branch.current,
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
        *self.git_history_ref.borrow_mut() = Some(name.to_owned());
        self.git_branch_menu.set(false);
        self.load_git_history();
    }

    /// Fetches, pulls or pushes the open workspace's branch.
    fn git_remote(&self, kind: GitRemoteActionKind) {
        if self.git_remote_busy.get_untracked() {
            return;
        }
        self.git_remote_busy.set(true);
        self.git_error.set(None);
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.git_remote_busy.set(false);
                return;
            };
            let result = background(async move {
                git.git_remote_action(MutationRequest::new(GitRemoteActionRequest {
                    workspace_id: workspace,
                    kind,
                    remote: None,
                    branch: None,
                }))
                .await
            })
            .await;
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
        if message.is_empty() {
            return;
        }
        self.git_committing.set(true);
        self.git_error.set(None);
        let amend = self.git_amend.get_untracked();
        let push_after = self.git_push_after.get_untracked();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                service.git_committing.set(false);
                return;
            };
            let result = background(async move {
                git.commit(MutationRequest::new(GitCommitRequest {
                    workspace_id: workspace,
                    message,
                    paths: Vec::new(),
                    amend,
                    push_after,
                }))
                .await
            })
            .await;
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
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let ref_name = service.git_history_ref.borrow().clone();
            let result = background(async move {
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
        let hash = hash.to_owned();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = background(async move {
                git.git_commit_detail(GitCommitDetailRequest {
                    workspace_id: workspace,
                    commit_hash: hash,
                    include_patch: true,
                })
                .await
            })
            .await;
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

    /// Reverts one path's working-tree changes.
    fn revert_path(&self, path: &str) {
        let path = path.to_owned();
        let service = self.clone();
        let git = self.git_backend.clone();
        let agent = self.backend.clone();
        spawn(async move {
            let Some(workspace) = service.workspace(&agent).await else {
                return;
            };
            let result = background(async move {
                git.git_revert(MutationRequest::new(GitStageRequest {
                    workspace_id: workspace,
                    paths: vec![path],
                }))
                .await
            })
            .await;
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
        } else {
            self.close_git();
        }
    }

    /// Re-reads the open session's transcript without clearing it.
    fn reload(&self, session_id: &VibexSessionId) {
        // A lagged stream may name a session the reader is not on; its page
        // must not replace the open transcript.
        if self.active.get_untracked().as_deref() != Some(session_id.as_str()) {
            return;
        }
        let service = self.clone();
        let backend = self.backend.clone();
        let session_id = session_id.clone();
        spawn(async move {
            let result = background(fetch_page(backend, session_id.clone())).await;
            match result {
                Ok(Ok(page)) => service.apply_page(&session_id, page),
                Ok(Err(error)) => service.fail(&error),
                Err(_) => {}
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

fn summary(session: &vibex_core::AgentSession, spaces: &[WorkspaceSummary]) -> SessionSummary {
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
        state: match session.state {
            vibex_core::AgentSessionState::Running
            | vibex_core::AgentSessionState::Initializing => SessionState::Running,
            vibex_core::AgentSessionState::NeedsInput => SessionState::Waiting,
            vibex_core::AgentSessionState::Error => SessionState::Failed,
            _ => SessionState::Idle,
        },
        project_id: session.project_id.as_str().to_owned(),
        project_name,
        workspace_id: session.workspace_id.as_str().to_owned(),
        workspace_label,
    }
}

/// Decodes a terminal frame, keeping an incomplete UTF-8 tail for the next
/// frame instead of showing a replacement mark for it.
fn decode_terminal_bytes(pending: &mut Vec<u8>, bytes: &[u8]) -> String {
    pending.extend_from_slice(bytes);
    match std::str::from_utf8(pending) {
        Ok(text) => {
            let text = text.to_owned();
            pending.clear();
            text
        }
        Err(error) => {
            let valid = error.valid_up_to();
            let text = String::from_utf8_lossy(&pending[..valid]).into_owned();
            pending.drain(..valid);
            text
        }
    }
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

fn load_queues() -> (
    BTreeMap<String, Vec<QueuedMessage>>,
    std::collections::BTreeSet<String>,
    u64,
) {
    let stored = Storage::get_item(QUEUE_KEY)
        .and_then(|text| serde_json::from_str::<StoredQueues>(&text).ok())
        .unwrap_or_default();
    let mut next = 0;
    let sessions = stored
        .sessions
        .into_iter()
        .map(|(session, messages)| {
            let messages = messages
                .into_iter()
                .map(|message| {
                    next = next.max(message.id.saturating_add(1));
                    QueuedMessage {
                        id: message.id,
                        text: message.text,
                        attachments: message
                            .attachments
                            .into_iter()
                            .map(|attachment| AttachmentItem {
                                id: format!("queued:{}", message.id),
                                label: attachment.label,
                                mime_type: attachment.mime_type,
                                uri: attachment.uri,
                            })
                            .collect(),
                        mentions: message.mentions,
                    }
                })
                .collect::<Vec<_>>();
            (session, messages)
        })
        .collect();
    (sessions, stored.paused.into_iter().collect(), next)
}

/// The cached transcript windows kept on this phone.
fn load_timeline_cache() -> BTreeMap<String, CachedTimeline> {
    Storage::get_item(TIMELINE_CACHE_KEY)
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or_default()
}

/// The per-session composer drafts kept on this phone.
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
fn row_meta(row: &TimelineRow, items: &[vibex_core::TimelineItem]) -> String {
    for id in row.item_ids.iter().rev() {
        let Some(item) = items.iter().find(|item| item.id.as_str() == id) else {
            continue;
        };
        return match &item.payload {
            TimelinePayload::PermissionRequest(request) => match request.status {
                PermissionRequestStatus::Pending => {
                    copy::text("Pending", "待确认", "待確認").to_owned()
                }
                PermissionRequestStatus::Approved => {
                    copy::text("Allowed", "已允许", "已允許").to_owned()
                }
                PermissionRequestStatus::Denied => {
                    copy::text("Denied", "已拒绝", "已拒絕").to_owned()
                }
                PermissionRequestStatus::Expired => {
                    copy::text("Expired", "已过期", "已過期").to_owned()
                }
            },
            TimelinePayload::PermissionResolution(resolution) => match resolution.response {
                PermissionResponseKind::Approve => {
                    copy::text("Allowed", "已允许", "已允許").to_owned()
                }
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
        };
    }
    String::new()
}

/// The attachments a user row carried, from the items behind it.
fn row_attachments(row: &TimelineRow, items: &[vibex_core::TimelineItem]) -> Vec<AttachmentItem> {
    for id in &row.item_ids {
        let Some(item) = items.iter().find(|item| item.id.as_str() == id) else {
            continue;
        };
        let TimelinePayload::UserMessage(message) = &item.payload else {
            continue;
        };
        return message
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| AttachmentItem {
                id: format!("{}:{index}", row.id),
                label: attachment.display_label().to_owned(),
                mime_type: attachment.mime_type.clone().unwrap_or_default(),
                uri: attachment.uri.clone().unwrap_or_default(),
            })
            .collect();
    }
    Vec::new()
}

fn process_header(turn: &TimelineConversationTurn, expanded: bool) -> TimelineEntry {
    let label = if turn.complete {
        copy::text("Worked for", "工作了", "工作了")
    } else {
        copy::text("Working for", "工作了", "工作了")
    };
    TimelineEntry {
        id: format!("turn-header:{}", turn.id),
        kind: EntryKind::ProcessHeader,
        title: format!(
            "{label} {}",
            compact_duration(turn.started_at_ms, turn.ended_at_ms)
        ),
        body: String::new(),
        meta: String::new(),
        streaming: !turn.complete,
        turn_id: turn.id.clone(),
        process_count: turn.process_rows.len(),
        expanded,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: 0,
        started_at_ms: turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms.unwrap_or(0),
        complete: turn.complete,
    }
}

fn changes_entry(turn: &TimelineConversationTurn, files: &[String]) -> TimelineEntry {
    TimelineEntry {
        id: format!("turn-changes:{}", turn.id),
        kind: EntryKind::Changes,
        title: format!(
            "{} {}",
            files.len(),
            copy::text("files changed", "个文件已更改", "個檔案已變更")
        ),
        body: files.join("\n"),
        meta: String::new(),
        streaming: false,
        turn_id: turn.id.clone(),
        process_count: 0,
        expanded: false,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: 0,
        started_at_ms: turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms.unwrap_or(0),
        complete: turn.complete,
    }
}

fn live_turn_entry(turn: &TimelineConversationTurn) -> TimelineEntry {
    TimelineEntry {
        id: format!("turn-live:{}", turn.id),
        kind: EntryKind::Agent,
        title: copy::text("Agent", "Agent", "Agent").to_owned(),
        body: turn
            .live_status
            .clone()
            .filter(|status| !status.trim().is_empty())
            .unwrap_or_else(|| copy::text("Thinking...", "思考中…", "思考中…").to_owned()),
        meta: String::new(),
        streaming: true,
        turn_id: turn.id.clone(),
        process_count: turn.process_rows.len(),
        expanded: false,
        expandable: false,
        attachments: Vec::new(),
        last_sequence: turn.process_rows.last().map_or(0, |row| row.last_sequence),
        started_at_ms: turn.started_at_ms,
        ended_at_ms: turn.ended_at_ms.unwrap_or(0),
        complete: false,
    }
}

fn compact_duration(started_at_ms: i64, ended_at_ms: Option<i64>) -> String {
    let end = ended_at_ms.unwrap_or_else(unix_timestamp_ms);
    let seconds = ((end - started_at_ms).max(0) / 1_000) as u64;
    match seconds {
        0..=59 => format!("{seconds}s"),
        60..=3_599 => format!("{}m {}s", seconds / 60, seconds % 60),
        _ => format!("{}h {}m", seconds / 3_600, (seconds % 3_600) / 60),
    }
}

fn entry_kind(kind: vibex_desktop_model::TimelineRowKind) -> EntryKind {
    use vibex_desktop_model::TimelineRowKind as Row;
    match kind {
        Row::UserMessage => EntryKind::User,
        Row::AgentMessage => EntryKind::Agent,
        Row::Reasoning => EntryKind::Reasoning,
        Row::Error | Row::Retry => EntryKind::Error,
        Row::ToolCall
        | Row::Command
        | Row::FileOperation
        | Row::WebSearch
        | Row::TodoUpdate
        | Row::Plan
        | Row::Collaboration
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
    use super::decode_terminal_bytes;

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
}

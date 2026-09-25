//! The browser domain service.
//!
//! This is the single owner of the browser process, the CDP connection, tab and
//! reference state, screencast frames, policy decisions and the action ledger.
//! Clients subscribe; they never hold a CDP connection of their own.
//!
//! ## Two channels, one target
//!
//! Humans watch `Page.startScreencast` frames. Agents read
//! `Accessibility.getFullAXTree`. Both hang off the same flattened CDP session
//! per tab, so what the agent acts on and what the user sees are the same
//! document. A third, always-on channel records console output and failed
//! requests, because for a coding agent those diagnostics are often the most
//! useful signal on the page.
//!
//! ## Frame back-pressure
//!
//! Frames use a latest-value slot, not a queue. `Page.screencastFrameAck` is
//! only sent once a consumer has taken the frame, so Chrome's encoder naturally
//! throttles to the consumer's rate: a hidden panel or a busy UI costs nothing,
//! and the runtime never accumulates decoded frames. The runtime does not
//! decode frames at all — it forwards the encoded JPEG so the same payload
//! works locally and over a remote transport.

use std::collections::{BTreeMap, HashMap, HashSet, VecDeque};
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::{Mutex, RwLock, broadcast, watch};
use vibex_core::{
    BROWSER_CDP_COMMAND_TIMEOUT_MS, BROWSER_MAX_DIAGNOSTIC_ENTRIES, BROWSER_MAX_FRAME_BYTES,
    BROWSER_MAX_SESSION_LEDGER_ITEMS, BROWSER_MAX_TABS, BrowserActionRecord, BrowserAvailability,
    BrowserConsoleEntry, BrowserDialogRequest, BrowserExecutionSource, BrowserFrame,
    BrowserFrameFormat, BrowserFrameMetadata, BrowserNetworkEntry, BrowserSession,
    BrowserSessionId, BrowserSessionSnapshot, BrowserTab, BrowserTabId, BrowserTabOwner,
    BrowserTabStatus, BrowserToolTier, BrowserUnavailableReason, VibexSessionId, WorkspaceId,
    unix_timestamp_ms,
};

use crate::ax::{PrunedElement, clamp_max_elements, prune_ax_tree, resolve_depth};
use crate::cdp::{CdpConnection, CdpEvent, CdpSession, CdpTransportKind};
use crate::discovery;
use crate::error::{BrowserError, BrowserResult};
use crate::policy;
use crate::process::{BrowserLaunchConfig, BrowserProcess};
use crate::recording::BrowserRecorder;
use crate::tools::{self, BrowserToolDefinition};

/// How long an idle browser with no sessions survives before it is closed.
pub const BROWSER_IDLE_TIMEOUT_MS: i64 = 120_000;
/// How often the idle reaper checks.
pub const BROWSER_IDLE_SWEEP: Duration = Duration::from_secs(30);
/// Default viewport when the panel has not reported its size yet.
pub const DEFAULT_VIEWPORT_WIDTH: u32 = 1280;
pub const DEFAULT_VIEWPORT_HEIGHT: u32 = 800;
/// JPEG quality for the standard screencast mode.
pub const SCREENCAST_JPEG_QUALITY: i64 = 80;
/// Short timeout for commands whose failure is not worth waiting on.
pub(crate) const SHORT_TIMEOUT_MS: u64 = 2_000;

/// Events the runtime publishes about the browser subsystem.
#[derive(Debug, Clone)]
pub enum BrowserServiceEvent {
    /// Availability changed (browser found or lost).
    Availability(BrowserAvailability),
    /// A session's tabs or execution source changed.
    SessionChanged(BrowserSessionId),
    /// A ledger entry was appended.
    Action(Box<BrowserActionRecord>),
    /// A JavaScript dialog opened and is blocking the page.
    DialogOpened(Box<BrowserDialogRequest>),
    /// A dialog was answered.
    DialogClosed(BrowserTabId),
    /// A page requested a file chooser.
    FileChooserOpened(BrowserTabId),
    /// A tab appeared because the page opened one — `target="_blank"` or
    /// `window.open` — exactly as it would in a real browser. The panel opens a
    /// preview tab for it.
    TabOpened {
        session_id: BrowserSessionId,
        tab_id: BrowserTabId,
    },
    /// A tab's URL, title or loading state changed.
    TabChanged(BrowserTabId),
    /// A tab was closed or reclaimed.
    TabClosed(BrowserTabId),
    /// A development server was detected in terminal output.
    DevServerDetected {
        workspace_id: Option<WorkspaceId>,
        origin: String,
    },
}

/// What the panel's inspector shows about one element.
///
/// Deliberately not the whole `DOM.describeNode` payload: the panel shows a
/// readable one-liner, and anything more would end up in a tooltip nobody reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserElementInspection {
    /// `tag#id` or `tag.class...`, the shortest thing that names the element.
    pub selector: String,
    pub node_name: String,
    pub id: String,
    pub classes: Vec<String>,
    pub width: i64,
    pub height: i64,
}

/// Identifies the owner of a browser session.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum BrowserSessionKey {
    /// The session belongs to an agent session.
    Agent(VibexSessionId),
    /// The session backs the human-facing panel for a workspace.
    Workspace(WorkspaceId),
    /// A session with no owner; used by probes and tests.
    Anonymous,
}

/// Input coming from the panel, in viewport CSS pixels.
#[derive(Debug, Clone)]
pub enum BrowserInput {
    MouseMove {
        x: f64,
        y: f64,
        /// Bitmask of the buttons held during the move.
        ///
        /// Chrome reads this to decide whether the move is a drag: without it a
        /// scrollbar drag, a text selection and an HTML5 drag never start.
        buttons: i32,
    },
    MouseDown {
        x: f64,
        y: f64,
        button: String,
        click_count: i32,
        modifiers: i32,
    },
    MouseUp {
        x: f64,
        y: f64,
        button: String,
        click_count: i32,
        modifiers: i32,
    },
    Wheel {
        x: f64,
        y: f64,
        delta_x: f64,
        delta_y: f64,
    },
    /// A raw key event. `event_type` is `keyDown`, `keyUp` or `rawKeyDown`.
    Key {
        event_type: String,
        key: String,
        code: String,
        text: Option<String>,
        modifiers: i32,
        windows_key_code: i32,
    },
    /// Committed IME or paste text. Composition is drawn client-side and only
    /// the committed string is sent to the page.
    InsertText { text: String },
    /// Panel resize; drives `Emulation.setDeviceMetricsOverride`.
    Resize {
        width: u32,
        height: u32,
        device_scale_factor: f64,
    },
}

/// Context for one tool call.
#[derive(Debug, Clone)]
pub struct BrowserToolContext {
    pub session_id: BrowserSessionId,
    pub agent_session_id: Option<VibexSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    /// Roots the agent is authorized to read from. Uploads and local previews
    /// are confined to these.
    pub authorized_roots: Vec<PathBuf>,
    pub tier: BrowserToolTier,
}

/// Service configuration.
#[derive(Debug, Clone)]
pub struct BrowserServiceConfig {
    /// Runtime data directory. Browser profiles live under `<home>/browser`.
    pub home_dir: PathBuf,
    /// Extra flags appended to every launch.
    pub extra_flags: Vec<String>,
    /// When false the service reports `FeatureDisabled` and never launches.
    pub enabled: bool,
    /// Domains approved for the lifetime of the runtime process.
    pub session_domain_grants: Vec<String>,
}

impl BrowserServiceConfig {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            extra_flags: Vec::new(),
            enabled: true,
            session_domain_grants: Vec::new(),
        }
    }
}

/// Per-tab diagnostics buffers.
#[derive(Debug, Default)]
pub(crate) struct TabDiagnostics {
    pub(crate) console: VecDeque<BrowserConsoleEntry>,
    pub(crate) network: VecDeque<BrowserNetworkEntry>,
    pub(crate) console_sequence: u64,
    pub(crate) network_sequence: u64,
}

impl TabDiagnostics {
    pub(crate) fn push_console(&mut self, mut entry: BrowserConsoleEntry) {
        self.console_sequence += 1;
        entry.sequence = self.console_sequence;
        self.console.push_back(entry);
        while self.console.len() > BROWSER_MAX_DIAGNOSTIC_ENTRIES {
            self.console.pop_front();
        }
    }

    pub(crate) fn push_network(&mut self, mut entry: BrowserNetworkEntry) {
        self.network_sequence += 1;
        entry.sequence = self.network_sequence;
        self.network.push_back(entry);
        while self.network.len() > BROWSER_MAX_DIAGNOSTIC_ENTRIES {
            self.network.pop_front();
        }
    }
}

/// One live tab.
pub(crate) struct TabRecord {
    pub(crate) tab_id: BrowserTabId,
    pub(crate) target_id: String,
    pub(crate) session_id: String,
    pub(crate) owner: BrowserTabOwner,
    pub(crate) agent_session_id: Option<VibexSessionId>,
    pub(crate) url: String,
    pub(crate) title: String,
    pub(crate) status: BrowserTabStatus,
    /// Incremented whenever earlier refs stop being valid.
    pub(crate) generation: u64,
    pub(crate) elements: Vec<PrunedElement>,
    pub(crate) created_at_ms: i64,
    pub(crate) last_activity_at_ms: i64,
    /// Latest encoded frame.
    pub(crate) frame: watch::Sender<Option<BrowserFrame>>,
    pub(crate) frame_sequence: Arc<AtomicU64>,
    /// The `sessionId` field Chrome expects back on `screencastFrameAck`.
    pub(crate) frame_ack_session_id: Option<i64>,
    pub(crate) screencast_active: bool,
    pub(crate) diagnostics: TabDiagnostics,
    /// In-flight requests keyed by CDP request id, valued by diagnostic
    /// sequence, so a later failure or response updates the same row.
    pub(crate) pending_requests: HashMap<String, u64>,
    /// Flattened sessions for cross-origin child frames.
    pub(crate) child_sessions: Vec<String>,
    /// Non-zero while an agent operation is running; keeps reclamation away.
    pub(crate) active_operations: Arc<AtomicU64>,
    /// Set when a human takes over; the next agent action fails explicitly.
    pub(crate) aborted: Arc<AtomicBool>,
    pub(crate) pending_dialog: Option<BrowserDialogRequest>,
    pub(crate) file_chooser_pending: bool,
    pub(crate) file_chooser_backend_node: Option<i64>,
    pub(crate) viewport: (u32, u32, f64),
    /// Navigation history position, kept for the panel's back/forward buttons.
    pub(crate) can_go_back: bool,
    pub(crate) can_go_forward: bool,
}

impl TabRecord {
    pub(crate) fn descriptors(&self) -> BrowserTab {
        BrowserTab {
            tab_id: self.tab_id.clone(),
            url: self.url.clone(),
            title: self.title.clone(),
            status: self.status,
            owner: self.owner,
            agent_session_id: self.agent_session_id.clone(),
            created_at_ms: self.created_at_ms,
            last_activity_at_ms: self.last_activity_at_ms,
            generation: self.generation,
            can_go_back: self.can_go_back,
            can_go_forward: self.can_go_forward,
        }
    }

    pub(crate) fn domain(&self) -> Option<String> {
        url::Url::parse(&self.url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_string))
    }
}

/// One browser session: a set of tabs plus an execution ledger.
pub(crate) struct SessionRecord {
    pub(crate) session_id: BrowserSessionId,
    pub(crate) key: BrowserSessionKey,
    workspace_id: Option<WorkspaceId>,
    pub(crate) tabs: Vec<BrowserTabId>,
    pub(crate) active_tab_id: Option<BrowserTabId>,
    pub(crate) agent_tab_id: Option<BrowserTabId>,
    pub(crate) execution_source: BrowserExecutionSource,
    pub(crate) user_engaged: bool,
    pub(crate) ledger: VecDeque<BrowserActionRecord>,
    pub(crate) created_at_ms: i64,
    pub(crate) last_activity_at_ms: i64,
    pub(crate) recorder: BrowserRecorder,
}

impl SessionRecord {
    pub(crate) fn snapshot(&self, tabs: &HashMap<BrowserTabId, TabRecord>) -> BrowserSession {
        BrowserSession {
            session_id: self.session_id.clone(),
            workspace_id: self.workspace_id.clone(),
            tabs: self
                .tabs
                .iter()
                .filter_map(|tab_id| tabs.get(tab_id).map(TabRecord::descriptors))
                .collect(),
            active_tab_id: self.active_tab_id.clone(),
            agent_tab_id: self.agent_tab_id.clone(),
            execution_source: self.execution_source,
            user_engaged: self.user_engaged,
            created_at_ms: self.created_at_ms,
            last_activity_at_ms: self.last_activity_at_ms,
        }
    }
}

/// Everything mutable the service owns.
///
/// Crate-visible so the tool executor can read tab state without a second
/// layer of accessors for every field.
pub(crate) struct ServiceState {
    pub(crate) process: Option<BrowserProcess>,
    pub(crate) sessions: BTreeMap<BrowserSessionId, SessionRecord>,
    pub(crate) tabs: HashMap<BrowserTabId, TabRecord>,
    pub(crate) availability: BrowserAvailability,
    pub(crate) last_activity_ms: i64,
    pub(crate) session_domain_grants: Vec<String>,
    /// Origins positively identified as this runtime's development servers.
    pub(crate) dev_server_origins: Vec<String>,
    /// Targets that existed before discovery was switched on. They are not
    /// tabs: the browser's own startup target is the only one today, and
    /// adopting it would show an empty tab the user never opened.
    pub(crate) ignored_targets: HashSet<String>,
}

impl ServiceState {
    pub(crate) fn push_ledger(&mut self, record: BrowserActionRecord) {
        if let Some(session) = self.sessions.get_mut(&record.session_id) {
            session.last_activity_at_ms = record.at_ms;
            session.ledger.push_back(record);
            while session.ledger.len() > BROWSER_MAX_SESSION_LEDGER_ITEMS {
                session.ledger.pop_front();
            }
        }
    }

    pub(crate) fn tab_by_cdp_session(&mut self, session_id: &str) -> Option<&mut TabRecord> {
        self.tabs
            .values_mut()
            .find(|tab| tab.session_id == session_id)
    }

    fn sessions_for_tab(&self, tab_id: &BrowserTabId) -> Vec<BrowserSessionId> {
        self.sessions
            .iter()
            .filter(|(_, session)| session.tabs.contains(tab_id))
            .map(|(id, _)| id.clone())
            .collect()
    }
}

/// A snapshot of the latest frame plus the ack obligation.
pub struct BrowserFrameSubscription {
    tab_id: BrowserTabId,
    receiver: watch::Receiver<Option<BrowserFrame>>,
    inner: Arc<BrowserInner>,
    last_sequence: u64,
    dropped: u64,
}

impl std::fmt::Debug for BrowserFrameSubscription {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserFrameSubscription")
            .field("tab_id", &self.tab_id)
            .field("last_sequence", &self.last_sequence)
            .field("dropped", &self.dropped)
            .finish()
    }
}

impl BrowserFrameSubscription {
    pub fn tab_id(&self) -> &BrowserTabId {
        &self.tab_id
    }

    pub fn dropped_frames(&self) -> u64 {
        self.dropped
    }

    /// Waits for the next frame and acknowledges the previous one.
    ///
    /// The acknowledgement is what lets Chrome encode the next frame, so a
    /// caller that stops calling `next` stops the stream at the source.
    pub async fn next(&mut self) -> Option<BrowserFrame> {
        loop {
            {
                let current = self.receiver.borrow().clone();
                if let Some(frame) = current
                    && frame.sequence != self.last_sequence
                {
                    if self.last_sequence != 0 && frame.sequence > self.last_sequence + 1 {
                        self.dropped += frame.sequence - self.last_sequence - 1;
                    }
                    self.last_sequence = frame.sequence;
                    let tab_id = self.tab_id.clone();
                    let inner = Arc::clone(&self.inner);
                    // The ack is fire-and-forget: blocking the consumer on a
                    // round trip would make the stream feel laggy.
                    tokio::spawn(async move { inner.ack_frame(&tab_id).await });
                    return Some(frame);
                }
            }
            if self.receiver.changed().await.is_err() {
                return None;
            }
        }
    }
}

/// One option of a page `<select>`, as the panel's fallback menu shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSelectOption {
    pub value: String,
    pub label: String,
}

/// A `<select>` the panel offers to drive itself.
///
/// `index` locates the element among the page's selects for the call that
/// applies the choice; a live element handle cannot survive between two CDP
/// evaluations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BrowserSelectMenu {
    pub index: u32,
    pub value: String,
    pub options: Vec<BrowserSelectOption>,
}

/// The outcome of one tool call.
#[derive(Debug, Clone)]
pub struct BrowserToolOutcome {
    pub text: String,
    pub is_error: bool,
    /// Images to return as MCP image content.
    pub images: Vec<BrowserImageContent>,
    /// Ledger entries produced by this call.
    pub records: Vec<BrowserActionRecord>,
    /// A dialog the page is blocked on, if one opened during the call.
    pub dialog: Option<BrowserDialogRequest>,
    /// A file chooser the panel must answer.
    pub file_chooser_tab: Option<BrowserTabId>,
}

/// One image returned to an agent.
#[derive(Debug, Clone)]
pub struct BrowserImageContent {
    pub mime_type: String,
    pub base64: String,
}

impl BrowserToolOutcome {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            images: Vec::new(),
            records: Vec::new(),
            dialog: None,
            file_chooser_tab: None,
        }
    }

    /// Renders a typed failure as a tool result.
    ///
    /// The message is the tool's own text, so the model sees an actionable
    /// explanation rather than a transport error.
    pub fn error(error: &BrowserError) -> Self {
        Self {
            text: error.message.clone(),
            is_error: true,
            images: Vec::new(),
            records: Vec::new(),
            dialog: None,
            file_chooser_tab: None,
        }
    }
}

pub(crate) struct BrowserInner {
    pub(crate) config: RwLock<BrowserServiceConfig>,
    pub(crate) state: Mutex<ServiceState>,
    events: broadcast::Sender<BrowserServiceEvent>,
    launch_lock: Mutex<()>,
    /// Non-zero while `create_tab` is waiting for `Target.createTarget`.
    ///
    /// The discovery event for a target the runtime creates itself can reach
    /// the event pump before the tab record exists; adopting it there would
    /// show the same page twice. Events are ignored while a create is in
    /// flight, and the returned target id is remembered either way.
    creating_targets: AtomicUsize,
    event_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    reaper_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    shutting_down: AtomicBool,
    /// One dev-server scanner per workspace, fed by the runtime's PTY reader.
    dev_servers: std::sync::Mutex<HashMap<WorkspaceId, crate::devserver::DevServerScanner>>,
    /// The runtime the readiness probes are spawned on. `None` when the service
    /// was built outside a Tokio runtime, where the detector stays dormant.
    runtime: Option<tokio::runtime::Handle>,
}

/// The browser domain service.
#[derive(Clone)]
pub struct BrowserService {
    inner: Arc<BrowserInner>,
}

impl std::fmt::Debug for BrowserService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserService")
            .finish_non_exhaustive()
    }
}

impl BrowserService {
    /// Creates the service. No browser is launched until it is needed.
    pub fn new(config: BrowserServiceConfig) -> Self {
        let (events, _) = broadcast::channel(256);
        let session_domain_grants = config.session_domain_grants.clone();
        let inner = Arc::new(BrowserInner {
            config: RwLock::new(config),
            state: Mutex::new(ServiceState {
                process: None,
                sessions: BTreeMap::new(),
                tabs: HashMap::new(),
                availability: discovery::availability(),
                last_activity_ms: unix_timestamp_ms(),
                session_domain_grants,
                dev_server_origins: Vec::new(),
                ignored_targets: HashSet::new(),
            }),
            events,
            launch_lock: Mutex::new(()),
            creating_targets: AtomicUsize::new(0),
            event_task: Mutex::new(None),
            reaper_task: Mutex::new(None),
            shutting_down: AtomicBool::new(false),
            dev_servers: std::sync::Mutex::new(HashMap::new()),
            runtime: tokio::runtime::Handle::try_current().ok(),
        });
        Self { inner }
    }

    /// Starts the idle reaper. Called once by the runtime composition root.
    pub async fn start_background_tasks(&self) {
        let mut guard = self.inner.reaper_task.lock().await;
        if guard.is_some() {
            return;
        }
        let inner = Arc::clone(&self.inner);
        *guard = Some(tokio::spawn(async move {
            let mut ticker = tokio::time::interval(BROWSER_IDLE_SWEEP);
            ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
            loop {
                ticker.tick().await;
                if inner.shutting_down.load(Ordering::SeqCst) {
                    return;
                }
                inner.reap_idle().await;
            }
        }));
    }

    pub fn subscribe(&self) -> broadcast::Receiver<BrowserServiceEvent> {
        self.inner.events.subscribe()
    }

    /// Current availability report.
    pub async fn availability(&self) -> BrowserAvailability {
        self.inner.state.lock().await.availability.clone()
    }

    /// Re-runs browser detection.
    pub async fn refresh_availability(&self) -> BrowserAvailability {
        let availability = discovery::availability();
        self.inner.state.lock().await.availability = availability.clone();
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::Availability(availability.clone()));
        availability
    }

    /// Records the origins the runtime identified as this workspace's
    /// development servers. Only these skip domain approval on loopback.
    pub async fn set_dev_server_origins(&self, origins: Vec<String>) {
        self.inner.state.lock().await.dev_server_origins = origins;
    }

    /// Reports a development server found in terminal output.
    pub async fn report_dev_server(&self, workspace_id: Option<WorkspaceId>, origin: String) {
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::DevServerDetected {
                workspace_id,
                origin,
            });
    }

    /// Feeds one read of terminal output to the dev-server detector.
    ///
    /// Called from the PTY reader thread, so this never awaits and never
    /// blocks: the scanner only keeps a bounded tail per workspace, and a
    /// candidate is reported once its port actually answers — the banner a dev
    /// server prints comes before it is listening, and a URL in a log line is
    /// not a server. A detected origin joins the workspace's allow-list, which
    /// is what lets the panel and the Agent reach the project's own dev server
    /// without a domain approval; public hosts never match, so the list cannot
    /// be widened by printing a URL.
    pub fn observe_terminal_output(&self, workspace_id: &WorkspaceId, chunk: &str) {
        let Some(runtime) = self.inner.runtime.clone() else {
            return;
        };
        let candidates = {
            let Ok(mut scanners) = self.inner.dev_servers.lock() else {
                return;
            };
            scanners
                .entry(workspace_id.clone())
                .or_default()
                .push(chunk)
        };
        // `push` yields `(origin, host, port)` for every URL it has not
        // reported before.
        for (origin, host, port) in candidates {
            let service = self.clone();
            let workspace_id = workspace_id.clone();
            runtime.spawn(async move {
                if crate::devserver::probe_candidate(&host, port).await {
                    service.register_dev_server(workspace_id, origin).await;
                }
            });
        }
    }

    /// Records a dev server that answered on its port.
    async fn register_dev_server(&self, workspace_id: WorkspaceId, origin: String) {
        let added = {
            let mut state = self.inner.state.lock().await;
            if state
                .dev_server_origins
                .iter()
                .any(|known| known == &origin)
            {
                false
            } else {
                state.dev_server_origins.push(origin.clone());
                true
            }
        };
        if added {
            let _ = self
                .inner
                .events
                .send(BrowserServiceEvent::DevServerDetected {
                    workspace_id: Some(workspace_id),
                    origin,
                });
        }
    }

    /// Grants a domain for the lifetime of the runtime process. The approval
    /// card's "always allow" action is what calls this; the runtime has no
    /// permission policy store of its own.
    pub async fn grant_domain(&self, domain: &str) {
        let mut state = self.inner.state.lock().await;
        let domain = domain.to_ascii_lowercase();
        if !state.session_domain_grants.contains(&domain) {
            state.session_domain_grants.push(domain);
        }
    }

    /// Revokes every domain grant.
    pub async fn clear_domain_grants(&self) {
        self.inner.state.lock().await.session_domain_grants.clear();
    }

    /// Origins positively identified as this runtime's development servers.
    ///
    /// Only these skip the private-network approval on loopback; the list is
    /// filled by the terminal detector, and an origin joins it only after its
    /// port answered.
    pub async fn dev_server_origins(&self) -> Vec<String> {
        self.inner.state.lock().await.dev_server_origins.clone()
    }

    /// Domains granted for this runtime process.
    pub async fn granted_domains(&self) -> Vec<String> {
        self.inner.state.lock().await.session_domain_grants.clone()
    }

    /// Returns the session for a key, creating it if needed.
    pub async fn ensure_session(
        &self,
        key: BrowserSessionKey,
        workspace_id: Option<WorkspaceId>,
    ) -> BrowserResult<BrowserSessionId> {
        {
            let state = self.inner.state.lock().await;
            if let Some(session) = state.sessions.values().find(|session| session.key == key) {
                return Ok(session.session_id.clone());
            }
        }
        self.ensure_process().await?;
        let now = unix_timestamp_ms();
        let session_id = BrowserSessionId::new();
        let mut state = self.inner.state.lock().await;
        state.sessions.insert(
            session_id.clone(),
            SessionRecord {
                session_id: session_id.clone(),
                key,
                workspace_id,
                tabs: Vec::new(),
                active_tab_id: None,
                agent_tab_id: None,
                execution_source: BrowserExecutionSource::User,
                user_engaged: false,
                ledger: VecDeque::new(),
                created_at_ms: now,
                last_activity_at_ms: now,
                recorder: BrowserRecorder::new(),
            },
        );
        state.last_activity_ms = now;
        Ok(session_id)
    }

    /// Drops a session and every tab it owns.
    pub async fn close_session(&self, session_id: &BrowserSessionId) -> BrowserResult<()> {
        let tab_ids = {
            let mut state = self.inner.state.lock().await;
            let Some(session) = state.sessions.remove(session_id) else {
                return Ok(());
            };
            for tab_id in &session.tabs {
                state.tabs.remove(tab_id);
            }
            session.tabs
        };
        for tab_id in &tab_ids {
            let _ = self.close_target(tab_id).await;
        }
        for tab_id in tab_ids {
            let _ = self
                .inner
                .events
                .send(BrowserServiceEvent::TabClosed(tab_id));
        }
        Ok(())
    }

    /// Snapshot of one session.
    pub async fn session_snapshot(
        &self,
        session_id: &BrowserSessionId,
    ) -> BrowserResult<BrowserSessionSnapshot> {
        let state = self.inner.state.lock().await;
        let session = state.sessions.get(session_id).ok_or_else(|| {
            BrowserError::validation(
                "browser_session_not_found",
                "the browser session was not found",
            )
        })?;
        Ok(BrowserSessionSnapshot {
            session: session.snapshot(&state.tabs),
            ledger: session.ledger.iter().cloned().collect(),
            availability: state.availability.clone(),
        })
    }

    /// Every live session.
    pub async fn sessions(&self) -> Vec<BrowserSession> {
        let state = self.inner.state.lock().await;
        state
            .sessions
            .values()
            .map(|session| session.snapshot(&state.tabs))
            .collect()
    }

    /// The session id for a key, if it already exists.
    pub async fn session_for_key(&self, key: &BrowserSessionKey) -> Option<BrowserSessionId> {
        self.inner
            .state
            .lock()
            .await
            .sessions
            .values()
            .find(|session| &session.key == key)
            .map(|session| session.session_id.clone())
    }

    /// The ledger for a session.
    pub async fn ledger(&self, session_id: &BrowserSessionId) -> Vec<BrowserActionRecord> {
        self.inner
            .state
            .lock()
            .await
            .sessions
            .get(session_id)
            .map(|session| session.ledger.iter().cloned().collect())
            .unwrap_or_default()
    }

    /// Subscribes to a tab's screencast frames and turns the stream on.
    pub async fn subscribe_frames(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserResult<BrowserFrameSubscription> {
        let (receiver, session, connection, target_id) = {
            let state = self.inner.state.lock().await;
            let tab = state.tabs.get(tab_id).ok_or_else(|| {
                BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
            })?;
            let connection = state
                .process
                .as_ref()
                .map(BrowserProcess::connection)
                .ok_or_else(browser_not_running)?;
            (
                tab.frame.subscribe(),
                CdpSession::new(
                    Arc::clone(&connection),
                    tab.session_id.clone(),
                    tab.target_id.clone(),
                ),
                connection,
                tab.target_id.clone(),
            )
        };
        // The panel is showing this tab now, so make it the browser's active
        // target the way clicking a tab in a real browser does. Chrome marks the
        // others hidden, and a page that believes it is hidden may pause its own
        // timers and animations — which is indistinguishable from a frozen tab.
        let _ = connection
            .command(
                "Target.activateTarget",
                json!({ "targetId": target_id }),
                Duration::from_millis(SHORT_TIMEOUT_MS),
            )
            .await;
        self.start_screencast(&session).await?;
        {
            let mut state = self.inner.state.lock().await;
            if let Some(tab) = state.tabs.get_mut(tab_id) {
                tab.screencast_active = true;
            }
        }
        Ok(BrowserFrameSubscription {
            tab_id: tab_id.clone(),
            receiver,
            inner: Arc::clone(&self.inner),
            last_sequence: 0,
            dropped: 0,
        })
    }

    /// Turns off screencast for a tab.
    ///
    /// The target stays alive and page state is preserved: switching panel tabs
    /// must never reload the page. Only the expensive part — encoding frames —
    /// stops.
    pub async fn stop_screencast(&self, tab_id: &BrowserTabId) {
        let session = {
            let mut state = self.inner.state.lock().await;
            let connection = state.process.as_ref().map(BrowserProcess::connection);
            let Some(tab) = state.tabs.get_mut(tab_id) else {
                return;
            };
            if !tab.screencast_active {
                return;
            }
            tab.screencast_active = false;
            connection.map(|connection| {
                CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone())
            })
        };
        if let Some(session) = session {
            let _ = cdp(&session, "Page.stopScreencast", json!({}), SHORT_TIMEOUT_MS).await;
        }
    }

    /// Applies a new viewport. Debounced by the caller.
    pub async fn set_viewport(
        &self,
        tab_id: &BrowserTabId,
        width: u32,
        height: u32,
        device_scale_factor: f64,
    ) -> BrowserResult<()> {
        let width = width.max(1);
        let height = height.max(1);
        let device_scale_factor = if device_scale_factor.is_finite() && device_scale_factor > 0.0 {
            device_scale_factor.clamp(0.25, 8.0)
        } else {
            1.0
        };
        let (session, screencast_active) = {
            let mut state = self.inner.state.lock().await;
            let connection = state
                .process
                .as_ref()
                .map(BrowserProcess::connection)
                .ok_or_else(browser_not_running)?;
            let tab = state.tabs.get_mut(tab_id).ok_or_else(|| {
                BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
            })?;
            if tab.viewport == (width, height, device_scale_factor) {
                return Ok(());
            }
            tab.viewport = (width, height, device_scale_factor);
            let screencast_active = tab.screencast_active;
            (
                CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone()),
                screencast_active,
            )
        };
        cdp(
            &session,
            "Emulation.setDeviceMetricsOverride",
            json!({
                "width": width,
                "height": height,
                "deviceScaleFactor": device_scale_factor,
                "mobile": false,
            }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        if screencast_active {
            // The encoder budget follows the panel size so nothing larger than
            // the display is ever encoded.
            self.start_screencast(&session).await?;
        }
        Ok(())
    }

    /// Dispatches a panel input event to the page.
    ///
    /// Deliberate human input takes the session over: `execution_source` flips
    /// to `User` and the current agent run is cancelled with an explicit error
    /// rather than silently racing. Pointer movement alone does not — a cursor
    /// resting over the panel is not a takeover, and treating it as one would
    /// pause the Agent the moment the frame appeared under the mouse.
    pub async fn dispatch_input(
        &self,
        tab_id: &BrowserTabId,
        input: BrowserInput,
    ) -> BrowserResult<()> {
        if let BrowserInput::Resize {
            width,
            height,
            device_scale_factor,
        } = input
        {
            return self
                .set_viewport(tab_id, width, height, device_scale_factor)
                .await;
        }
        let takes_over = input_takes_over(&input);
        let now = unix_timestamp_ms();
        let (session, changed) = {
            let mut state = self.inner.state.lock().await;
            let connection = state
                .process
                .as_ref()
                .map(BrowserProcess::connection)
                .ok_or_else(browser_not_running)?;
            let tab = state.tabs.get_mut(tab_id).ok_or_else(|| {
                BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
            })?;
            tab.last_activity_at_ms = now;
            if takes_over {
                tab.aborted.store(true, Ordering::SeqCst);
            }
            let session =
                CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone());
            let changed = if takes_over {
                let session_ids = state.sessions_for_tab(tab_id);
                for session_id in &session_ids {
                    if let Some(session) = state.sessions.get_mut(session_id) {
                        session.execution_source = BrowserExecutionSource::User;
                        session.user_engaged = true;
                        session.last_activity_at_ms = now;
                    }
                }
                session_ids
            } else {
                Vec::new()
            };
            state.last_activity_ms = now;
            (session, changed)
        };
        for session_id in changed {
            let _ = self
                .inner
                .events
                .send(BrowserServiceEvent::SessionChanged(session_id));
        }
        let (method, params) = input_to_cdp(input);
        cdp(&session, method, params, BROWSER_CDP_COMMAND_TIMEOUT_MS).await?;
        Ok(())
    }

    /// Shuts the browser down. Called from the runtime's shutdown chain.
    pub async fn shutdown(&self) {
        self.inner.shutting_down.store(true, Ordering::SeqCst);
        if let Some(task) = self.inner.event_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.inner.reaper_task.lock().await.take() {
            task.abort();
        }
        let process = {
            let mut state = self.inner.state.lock().await;
            state.tabs.clear();
            state.sessions.clear();
            state.process.take()
        };
        if let Some(process) = process {
            process.shutdown().await;
        }
    }

    /// What a click at this viewport point would open, when it lands on a
    /// single-choice `<select>`.
    ///
    /// Headless Chrome draws a select popup in browser UI, which the screencast
    /// never carries: without this the panel would swallow the click and show
    /// nothing. The element is located by its index among the page's selects so
    /// the choice can be applied by a later call.
    pub async fn select_menu_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserResult<Option<BrowserSelectMenu>> {
        let (_connection, session) = self.inner.tab_session(tab_id).await?;
        let script = format!(
            "(() => {{
               const el = document.elementFromPoint({x}, {y});
               if (!el || el.tagName !== 'SELECT' || el.multiple) return null;
               const index = Array.from(document.querySelectorAll('select')).indexOf(el);
               if (index < 0) return null;
               return {{
                 index,
                 value: el.value,
                 options: Array.from(el.options).map((option) => ({{
                   value: option.value,
                   label: option.text,
                 }})),
               }};
             }})()"
        );
        let result = cdp(
            &session,
            "Runtime.evaluate",
            json!({ "expression": script, "returnByValue": true }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        let value = result
            .get("result")
            .and_then(|result| result.get("value"))
            .cloned()
            .unwrap_or(Value::Null);
        if value.is_null() {
            return Ok(None);
        }
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0) as u32;
        let value_current = value
            .get("value")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string();
        let options = value
            .get("options")
            .and_then(Value::as_array)
            .map(|options| {
                options
                    .iter()
                    .map(|option| BrowserSelectOption {
                        value: option
                            .get("value")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                        label: option
                            .get("label")
                            .and_then(Value::as_str)
                            .unwrap_or_default()
                            .to_string(),
                    })
                    .collect()
            })
            .unwrap_or_default();
        Ok(Some(BrowserSelectMenu {
            index,
            value: value_current,
            options,
        }))
    }

    /// Chooses an option on the select a menu was opened for.
    ///
    /// The page sees the same `input` and `change` events a real pick sends, so
    /// a form that listens for them reacts exactly as it would to a human.
    pub async fn choose_select_option(
        &self,
        tab_id: &BrowserTabId,
        index: u32,
        value: &str,
    ) -> BrowserResult<()> {
        let (_connection, session) = self.inner.tab_session(tab_id).await?;
        let script = format!(
            "(() => {{
               const el = document.querySelectorAll('select')[{index}];
               if (!el) return false;
               el.focus();
               el.value = {};
               el.dispatchEvent(new Event('input', {{ bubbles: true }}));
               el.dispatchEvent(new Event('change', {{ bubbles: true }}));
               return true;
             }})()",
            serde_json::to_string(value).unwrap_or_else(|_| "''".to_string())
        );
        cdp(
            &session,
            "Runtime.evaluate",
            json!({ "expression": script, "returnByValue": true }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        Ok(())
    }

    /// The page's current selection, for the panel's copy shortcut.
    ///
    /// Headless Chrome's clipboard is not the system clipboard, so a human's
    /// Ctrl+C has to be read out of the page and written by the panel.
    pub async fn selection_text(&self, tab_id: &BrowserTabId) -> BrowserResult<String> {
        let (_connection, session) = self.inner.tab_session(tab_id).await?;
        let result = cdp(
            &session,
            "Runtime.evaluate",
            json!({
                "expression": "window.getSelection().toString()",
                "returnByValue": true,
            }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        Ok(result
            .get("result")
            .and_then(|result| result.get("value"))
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_string())
    }

    /// Answers a pending JavaScript dialog.
    pub async fn handle_dialog(
        &self,
        tab_id: &BrowserTabId,
        accept: bool,
        prompt_text: Option<&str>,
    ) -> BrowserResult<()> {
        let session = {
            let state = self.inner.state.lock().await;
            let tab = state.tabs.get(tab_id).ok_or_else(|| {
                BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
            })?;
            CdpSession::new(
                state
                    .process
                    .as_ref()
                    .map(BrowserProcess::connection)
                    .ok_or_else(browser_not_running)?,
                tab.session_id.clone(),
                tab.target_id.clone(),
            )
        };
        cdp(
            &session,
            "Page.handleJavaScriptDialog",
            json!({
                "accept": accept,
                "promptText": prompt_text.unwrap_or_default(),
            }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        {
            let mut state = self.inner.state.lock().await;
            if let Some(tab) = state.tabs.get_mut(tab_id) {
                tab.pending_dialog = None;
            }
        }
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::DialogClosed(tab_id.clone()));
        Ok(())
    }

    /// The dialog currently blocking a tab, if any.
    pub async fn pending_dialog(&self, tab_id: &BrowserTabId) -> Option<BrowserDialogRequest> {
        self.inner
            .state
            .lock()
            .await
            .tabs
            .get(tab_id)
            .and_then(|tab| tab.pending_dialog.clone())
    }

    /// Supplies the files a page requested through a file chooser.
    pub async fn resolve_file_chooser(
        &self,
        tab_id: &BrowserTabId,
        paths: &[PathBuf],
    ) -> BrowserResult<()> {
        let (session, backend_node_id) = {
            let state = self.inner.state.lock().await;
            let tab = state.tabs.get(tab_id).ok_or_else(|| {
                BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
            })?;
            (
                CdpSession::new(
                    state
                        .process
                        .as_ref()
                        .map(BrowserProcess::connection)
                        .ok_or_else(browser_not_running)?,
                    tab.session_id.clone(),
                    tab.target_id.clone(),
                ),
                tab.file_chooser_backend_node,
            )
        };
        let files: Vec<String> = paths
            .iter()
            .map(|path| path.to_string_lossy().to_string())
            .collect();
        let params = match backend_node_id {
            Some(backend_node_id) => json!({ "files": files, "backendNodeId": backend_node_id }),
            None => json!({ "files": files }),
        };
        cdp(
            &session,
            "DOM.setFileInputFiles",
            params,
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        let mut state = self.inner.state.lock().await;
        if let Some(tab) = state.tabs.get_mut(tab_id) {
            tab.file_chooser_pending = false;
            tab.file_chooser_backend_node = None;
        }
        Ok(())
    }

    /// Names of the tools available at a tier.
    pub fn available_tool_names(tier: BrowserToolTier) -> Vec<String> {
        tools::tools_for_tier(tier)
            .into_iter()
            .map(|tool| tool.name.to_string())
            .collect()
    }

    /// Looks up a tool definition.
    pub fn tool_definition(name: &str) -> Option<BrowserToolDefinition> {
        tools::tool_by_name(name)
    }

    fn require_browser(&self) -> BrowserResult<()> {
        if discovery::cached_installations().is_empty() {
            return Err(BrowserError::capability(
                "browser_unavailable",
                policy::unavailable_detail(BrowserUnavailableReason::BrowserMissing),
            )
            .with_recovery_hint("Install a Chromium-based browser and retry."));
        }
        Ok(())
    }

    /// Starts the browser if it is not already running.
    pub(crate) async fn ensure_process(&self) -> BrowserResult<()> {
        if self.inner.shutting_down.load(Ordering::SeqCst) {
            return Err(BrowserError::process(
                "browser_shutting_down",
                "the runtime is shutting down",
            ));
        }
        {
            let state = self.inner.state.lock().await;
            if state.process.as_ref().is_some_and(BrowserProcess::is_alive) {
                return Ok(());
            }
        }
        let _guard = self.inner.launch_lock.lock().await;
        {
            let state = self.inner.state.lock().await;
            if state.process.as_ref().is_some_and(BrowserProcess::is_alive) {
                return Ok(());
            }
        }
        let config = self.inner.config.read().await.clone();
        if !config.enabled {
            return Err(BrowserError::capability(
                "browser_feature_disabled",
                policy::unavailable_detail(BrowserUnavailableReason::FeatureDisabled),
            ));
        }
        self.require_browser()?;
        let Some(installation) = discovery::installation_by_id(None) else {
            let availability = BrowserAvailability::unavailable(
                BrowserUnavailableReason::BrowserMissing,
                Some(policy::missing_browser_guidance(std::env::consts::OS)),
            );
            self.inner.state.lock().await.availability = availability.clone();
            let _ = self
                .inner
                .events
                .send(BrowserServiceEvent::Availability(availability));
            return Err(BrowserError::capability(
                "browser_unavailable",
                policy::unavailable_detail(BrowserUnavailableReason::BrowserMissing),
            ));
        };
        let profile_dir = config
            .home_dir
            .join("browser")
            .join("profiles")
            .join(profile_key(&installation.id));
        let mut launch =
            BrowserLaunchConfig::new(installation.clone(), profile_dir, preferred_transport());
        for flag in &config.extra_flags {
            launch = launch.with_extra_flag(flag.clone());
        }
        let process = crate::process::launch(&launch).await?;
        let connection = process.connection();
        let availability = BrowserAvailability::available(installation);
        {
            let mut state = self.inner.state.lock().await;
            state.process = Some(process);
            state.availability = availability.clone();
            state.last_activity_ms = unix_timestamp_ms();
        }
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::Availability(availability));
        self.start_event_pump(connection).await;
        Ok(())
    }

    async fn start_event_pump(&self, connection: Arc<CdpConnection>) {
        let mut guard = self.inner.event_task.lock().await;
        if let Some(task) = guard.take() {
            task.abort();
        }
        let mut receiver = connection.subscribe();
        let inner = Arc::clone(&self.inner);
        let connection_for_events = Arc::clone(&connection);
        *guard = Some(tokio::spawn(async move {
            loop {
                match receiver.recv().await {
                    Ok(event) => handle_cdp_event(&inner, &connection_for_events, event).await,
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            target: "vibex_browser",
                            skipped,
                            "the browser event stream lagged; some diagnostics were dropped"
                        );
                    }
                    Err(broadcast::error::RecvError::Closed) => return,
                }
            }
        }));
        // Target discovery is what makes a page-opened tab a tab: clicking
        // `target="_blank"` or calling `window.open` creates a page target, and
        // without discovery the runtime never hears about it and the panel sits
        // on the old page as if the click did nothing. The targets that already
        // exist are recorded first so the initial burst does not adopt the
        // browser's own startup target as a tab.
        if let Ok(existing) = connection
            .command(
                "Target.getTargets",
                json!({}),
                Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS),
            )
            .await
        {
            let mut state = self.inner.state.lock().await;
            for info in existing
                .get("targetInfos")
                .and_then(Value::as_array)
                .into_iter()
                .flatten()
            {
                if let Some(target_id) = info.get("targetId").and_then(Value::as_str) {
                    state.ignored_targets.insert(target_id.to_string());
                }
            }
        }
        let _ = connection
            .command(
                "Target.setDiscoverTargets",
                json!({ "discover": true }),
                Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS),
            )
            .await;
    }

    async fn start_screencast(&self, session: &CdpSession) -> BrowserResult<()> {
        // Chrome answers a second `Page.startScreencast` on a tab whose
        // screencast is still running with "Screencast is already active", which
        // used to surface as a dead panel after a tab was closed and reopened.
        // Stopping first is harmless when nothing is running and makes the call
        // idempotent.
        let _ = cdp(session, "Page.stopScreencast", json!({}), SHORT_TIMEOUT_MS).await;
        let (width, height) = {
            let state = self.inner.state.lock().await;
            state
                .tabs
                .values()
                .find(|tab| tab.session_id == session.session_id)
                .map(|tab| (tab.viewport.0, tab.viewport.1))
                .unwrap_or((DEFAULT_VIEWPORT_WIDTH, DEFAULT_VIEWPORT_HEIGHT))
        };
        cdp(
            session,
            "Page.startScreencast",
            json!({
                "format": "jpeg",
                "quality": SCREENCAST_JPEG_QUALITY,
                "maxWidth": width,
                "maxHeight": height,
                "everyNthFrame": 1,
            }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await
        .map(|_| ())
    }

    /// Creates a tab in a session.
    pub async fn create_tab(
        &self,
        session_id: &BrowserSessionId,
        url: Option<&str>,
        owner: BrowserTabOwner,
    ) -> BrowserResult<BrowserTabId> {
        self.ensure_process().await?;
        let connection = self.connection().await?;
        let target_url = url.unwrap_or("about:blank");
        self.inner.creating_targets.fetch_add(1, Ordering::SeqCst);
        let created = connection
            .command(
                "Target.createTarget",
                json!({ "url": target_url }),
                Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS),
            )
            .await;
        self.inner.creating_targets.fetch_sub(1, Ordering::SeqCst);
        let created = created?;
        let target_id = created
            .get("targetId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::cdp(
                    "browser_target_create_failed",
                    "the browser did not return a target id",
                )
            })?
            .to_string();
        // Remember the target so a discovery event that is already queued does
        // not adopt it as a page-opened tab.
        self.inner
            .state
            .lock()
            .await
            .ignored_targets
            .insert(target_id.clone());
        let attached = connection
            .command(
                "Target.attachToTarget",
                json!({ "targetId": target_id, "flatten": true }),
                Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS),
            )
            .await?;
        let cdp_session_id = attached
            .get("sessionId")
            .and_then(Value::as_str)
            .ok_or_else(|| {
                BrowserError::cdp(
                    "browser_target_attach_failed",
                    "the browser did not return a session for the new tab",
                )
            })?
            .to_string();
        let session = CdpSession::new(
            Arc::clone(&connection),
            cdp_session_id.clone(),
            target_id.clone(),
        );
        prepare_tab_session(&session).await?;

        let now = unix_timestamp_ms();
        let tab_id = BrowserTabId::new();
        let (frame, _) = watch::channel(None);
        let agent_session_id = {
            let state = self.inner.state.lock().await;
            state
                .sessions
                .get(session_id)
                .and_then(|session| match &session.key {
                    BrowserSessionKey::Agent(agent) => Some(agent.clone()),
                    _ => None,
                })
        };
        let record = TabRecord {
            tab_id: tab_id.clone(),
            target_id,
            session_id: cdp_session_id,
            owner,
            agent_session_id,
            url: target_url.to_string(),
            title: String::new(),
            status: BrowserTabStatus::Loading,
            generation: 0,
            elements: Vec::new(),
            created_at_ms: now,
            last_activity_at_ms: now,
            frame,
            frame_sequence: Arc::new(AtomicU64::new(0)),
            frame_ack_session_id: None,
            screencast_active: false,
            diagnostics: TabDiagnostics::default(),
            pending_requests: HashMap::new(),
            child_sessions: Vec::new(),
            active_operations: Arc::new(AtomicU64::new(0)),
            aborted: Arc::new(AtomicBool::new(false)),
            pending_dialog: None,
            file_chooser_pending: false,
            file_chooser_backend_node: None,
            viewport: (DEFAULT_VIEWPORT_WIDTH, DEFAULT_VIEWPORT_HEIGHT, 1.0),
            can_go_back: false,
            can_go_forward: false,
        };
        {
            let mut state = self.inner.state.lock().await;
            state.tabs.insert(tab_id.clone(), record);
            let Some(session) = state.sessions.get_mut(session_id) else {
                state.tabs.remove(&tab_id);
                return Err(BrowserError::validation(
                    "browser_session_not_found",
                    "the browser session was not found",
                ));
            };
            session.tabs.push(tab_id.clone());
            if session.active_tab_id.is_none() {
                session.active_tab_id = Some(tab_id.clone());
            }
            if owner == BrowserTabOwner::Agent {
                session.agent_tab_id = Some(tab_id.clone());
            }
            session.last_activity_at_ms = now;
            state.last_activity_ms = now;
        }
        self.reclaim_tabs_if_needed(session_id).await;
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::SessionChanged(session_id.clone()));
        // A tab an Agent created is still a tab the human should be able to
        // watch: the panel owns a preview tab per runtime tab, and the binding
        // carries whichever session it belongs to. Without this the Agent's
        // pages lived in a session the panel never showed, which looks exactly
        // like an Agent that lied about opening a browser.
        let _ = self.inner.events.send(BrowserServiceEvent::TabOpened {
            session_id: session_id.clone(),
            tab_id: tab_id.clone(),
        });
        Ok(tab_id)
    }

    async fn connection(&self) -> BrowserResult<Arc<CdpConnection>> {
        let state = self.inner.state.lock().await;
        state
            .process
            .as_ref()
            .map(BrowserProcess::connection)
            .ok_or_else(browser_not_running)
    }

    /// Closes a tab.
    pub async fn close_tab(&self, tab_id: &BrowserTabId) -> BrowserResult<()> {
        let session = {
            let mut state = self.inner.state.lock().await;
            let Some(tab) = state.tabs.remove(tab_id) else {
                return Ok(());
            };
            let children = tab.child_sessions.clone();
            for record in state.sessions.values_mut() {
                record.tabs.retain(|id| id != tab_id);
                if record.active_tab_id.as_ref() == Some(tab_id) {
                    record.active_tab_id = record.tabs.first().cloned();
                }
                if record.agent_tab_id.as_ref() == Some(tab_id) {
                    // The working tab is never silently replaced: the agent has
                    // to create or select one before acting again.
                    record.agent_tab_id = None;
                }
            }
            state
                .process
                .as_ref()
                .map(BrowserProcess::connection)
                .map(|connection| {
                    (
                        CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone()),
                        children,
                    )
                })
        };
        if let Some((session, children)) = session {
            for child in children {
                let _ = session
                    .connection()
                    .command_on(
                        Some(&child),
                        "Runtime.runIfWaitingForDebugger",
                        json!({}),
                        Duration::from_millis(SHORT_TIMEOUT_MS),
                    )
                    .await;
            }
            let _ = cdp(&session, "Page.stopScreencast", json!({}), SHORT_TIMEOUT_MS).await;
            let _ = cdp(
                &session,
                "Target.closeTarget",
                json!({ "targetId": session.target_id }),
                BROWSER_CDP_COMMAND_TIMEOUT_MS,
            )
            .await;
        }
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::TabClosed(tab_id.clone()));
        Ok(())
    }

    /// Closes the CDP target backing a tab without touching session state.
    async fn close_target(&self, tab_id: &BrowserTabId) {
        let connection = self.connection().await.ok();
        let Some(connection) = connection else {
            return;
        };
        // The tab record is already gone, so the target id is not available;
        // closing every remaining target is handled by the process teardown.
        let _ = (connection, tab_id);
    }

    /// Reclaims agent-owned idle tabs when a session exceeds the tab ceiling.
    ///
    /// User tabs are never closed automatically.
    async fn reclaim_tabs_if_needed(&self, session_id: &BrowserSessionId) {
        let victim = {
            let state = self.inner.state.lock().await;
            let Some(session) = state.sessions.get(session_id) else {
                return;
            };
            if session.tabs.len() <= BROWSER_MAX_TABS {
                return;
            }
            let excess = session.tabs.len() - BROWSER_MAX_TABS;
            let mut candidates: Vec<(i64, BrowserTabId)> = session
                .tabs
                .iter()
                .filter_map(|tab_id| state.tabs.get(tab_id))
                .filter(|tab| {
                    tab.owner == BrowserTabOwner::Agent
                        && tab.active_operations.load(Ordering::SeqCst) == 0
                        && Some(&tab.tab_id) != session.agent_tab_id.as_ref()
                })
                .map(|tab| (tab.last_activity_at_ms, tab.tab_id.clone()))
                .collect();
            candidates.sort_by_key(|(at, _)| *at);
            candidates.truncate(excess);
            candidates.into_iter().map(|(_, tab_id)| tab_id).next()
        };
        if let Some(tab_id) = victim {
            tracing::info!(
                target: "vibex_browser",
                "reclaimed an idle agent-owned browser tab at the session tab ceiling"
            );
            let _ = self.close_tab(&tab_id).await;
        }
    }

    /// Cancels the current agent run on a tab.
    ///
    /// Already-dispatched DevTools commands cannot be revoked; the error the
    /// agent receives says exactly that, so "stop" never implies a rollback.
    pub async fn abort_agent_operations(&self, tab_id: &BrowserTabId) {
        let mut state = self.inner.state.lock().await;
        if let Some(tab) = state.tabs.get_mut(tab_id) {
            tab.aborted.store(true, Ordering::SeqCst);
        }
    }

    /// Lets an agent continue after a human handover.
    ///
    /// The page may have changed underneath the agent, so the caller must
    /// observe again; this only re-arms the cancellation flag.
    pub async fn resume_agent_operations(&self, session_id: &BrowserSessionId) {
        let mut state = self.inner.state.lock().await;
        let tab_ids: Vec<BrowserTabId> = state
            .sessions
            .get(session_id)
            .map(|session| session.tabs.clone())
            .unwrap_or_default();
        for tab_id in tab_ids {
            if let Some(tab) = state.tabs.get_mut(&tab_id) {
                tab.aborted.store(false, Ordering::SeqCst);
            }
        }
        if let Some(session) = state.sessions.get_mut(session_id) {
            session.execution_source = BrowserExecutionSource::Agent;
        }
        // Every client shows who is driving, so the hand-back is announced the
        // same way the takeover is.
        let _ = self
            .inner
            .events
            .send(BrowserServiceEvent::SessionChanged(session_id.clone()));
    }

    /// Navigates a tab on behalf of the human driving the panel.
    ///
    /// No approval prompt: the person typed the URL, so there is nobody left to
    /// ask. Agent-initiated navigation goes through the tool path, where the
    /// policy check lives.
    pub async fn navigate(&self, tab_id: &BrowserTabId, url: &str) -> BrowserResult<()> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        cdp(
            &session,
            "Page.navigate",
            json!({ "url": url }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        let mut state = self.inner.state.lock().await;
        if let Some(tab) = state.tabs.get_mut(tab_id) {
            tab.url = url.to_string();
            tab.status = BrowserTabStatus::Loading;
            tab.last_activity_at_ms = unix_timestamp_ms();
            tab.generation = tab.generation.saturating_add(1);
            tab.elements.clear();
        }
        state.last_activity_ms = unix_timestamp_ms();
        Ok(())
    }

    /// Moves a tab through its navigation history.
    ///
    /// This is what the panel's back and forward buttons and the mouse's side
    /// buttons call. The history entry is addressed by id, so a page that
    /// rewrote its own history between the buttons being painted and clicked
    /// cannot make the runtime jump somewhere else.
    pub async fn navigate_history(
        &self,
        tab_id: &BrowserTabId,
        forward: bool,
    ) -> BrowserResult<()> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        let history = cdp(
            &session,
            "Page.getNavigationHistory",
            json!({}),
            SHORT_TIMEOUT_MS,
        )
        .await?;
        let index = history
            .get("currentIndex")
            .and_then(Value::as_i64)
            .unwrap_or(0);
        let target_index = if forward { index + 1 } else { index - 1 };
        let entry_id = (target_index >= 0)
            .then(|| {
                history
                    .get("entries")
                    .and_then(Value::as_array)
                    .and_then(|entries| entries.get(target_index as usize))
                    .and_then(|entry| entry.get("id"))
                    .cloned()
            })
            .flatten();
        let Some(entry_id) = entry_id else {
            // Nothing that way: the panel's button should have been disabled,
            // and a stale click is not an error.
            return Ok(());
        };
        cdp(
            &session,
            "Page.navigateToHistoryEntry",
            json!({ "entryId": entry_id }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        let _ = refresh_navigation_state(&self.inner, &session).await;
        Ok(())
    }

    pub async fn go_back(&self, tab_id: &BrowserTabId) -> BrowserResult<()> {
        self.navigate_history(tab_id, false).await
    }

    pub async fn go_forward(&self, tab_id: &BrowserTabId) -> BrowserResult<()> {
        self.navigate_history(tab_id, true).await
    }

    /// Highlights the element under a viewport point, for the panel's picker.
    ///
    /// The highlight is drawn by the page (`Overlay.highlightNode`), so it
    /// follows scrolling and zooming and shows up in the screencast without any
    /// coordinate conversion on the client.
    pub async fn highlight_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserResult<Option<i64>> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        let Some(backend_node_id) = node_at_point(&session, x, y).await? else {
            let _ = cdp(
                &session,
                "Overlay.hideHighlight",
                json!({}),
                SHORT_TIMEOUT_MS,
            )
            .await;
            return Ok(None);
        };
        cdp(
            &session,
            "Overlay.highlightNode",
            json!({
                "backendNodeId": backend_node_id,
                "highlightConfig": {
                    "showInfo": false,
                    "contentColor": { "r": 111, "g": 168, "b": 220, "a": 0.25 },
                    "paddingColor": { "r": 147, "g": 196, "b": 125, "a": 0.35 },
                    "borderColor": { "r": 255, "g": 229, "b": 153, "a": 0.7 },
                    "marginColor": { "r": 246, "g": 178, "b": 107, "a": 0.35 },
                },
            }),
            SHORT_TIMEOUT_MS,
        )
        .await?;
        Ok(Some(backend_node_id))
    }

    /// Removes the picker's highlight.
    pub async fn clear_highlight(&self, tab_id: &BrowserTabId) -> BrowserResult<()> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        cdp(
            &session,
            "Overlay.hideHighlight",
            json!({}),
            SHORT_TIMEOUT_MS,
        )
        .await?;
        Ok(())
    }

    /// Describes the element under a viewport point for the panel's inspector.
    pub async fn describe_at(
        &self,
        tab_id: &BrowserTabId,
        x: f64,
        y: f64,
    ) -> BrowserResult<Option<BrowserElementInspection>> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        let Some(backend_node_id) = node_at_point(&session, x, y).await? else {
            return Ok(None);
        };
        let described = cdp(
            &session,
            "DOM.describeNode",
            json!({ "backendNodeId": backend_node_id, "depth": 0 }),
            SHORT_TIMEOUT_MS,
        )
        .await?;
        let node = &described["node"];
        let node_name = node
            .get("nodeName")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_ascii_lowercase();
        let mut id = String::new();
        let mut classes = Vec::new();
        if let Some(attributes) = node.get("attributes").and_then(Value::as_array) {
            let mut pairs = attributes.iter().filter_map(Value::as_str);
            while let (Some(name), Some(value)) = (pairs.next(), pairs.next()) {
                match name {
                    "id" => id = value.to_string(),
                    "class" => classes = value.split_whitespace().map(str::to_string).collect(),
                    _ => {}
                }
            }
        }
        // The border box is what the eye sees; a missing box model (a hidden
        // node) is not an error, the card simply has no size to show.
        let (width, height) = match cdp(
            &session,
            "DOM.getBoxModel",
            json!({ "backendNodeId": backend_node_id }),
            SHORT_TIMEOUT_MS,
        )
        .await
        {
            Ok(model) => {
                let quad = model["model"]["border"]
                    .as_array()
                    .map(|values| values.iter().filter_map(Value::as_f64).collect::<Vec<_>>())
                    .unwrap_or_default();
                if quad.len() == 8 {
                    (
                        (quad[2] - quad[0]).abs().round() as i64,
                        (quad[5] - quad[1]).abs().round() as i64,
                    )
                } else {
                    (0, 0)
                }
            }
            Err(_) => (0, 0),
        };
        let selector = if !id.is_empty() {
            format!("{node_name}#{id}")
        } else if !classes.is_empty() {
            format!("{node_name}.{}", classes.join("."))
        } else {
            node_name.clone()
        };
        Ok(Some(BrowserElementInspection {
            selector,
            node_name,
            id,
            classes,
            width,
            height,
        }))
    }

    /// Reloads a tab.
    pub async fn reload(&self, tab_id: &BrowserTabId, ignore_cache: bool) -> BrowserResult<()> {
        let (_, session) = self.inner.tab_session(tab_id).await?;
        cdp(
            &session,
            "Page.reload",
            json!({ "ignoreCache": ignore_cache }),
            BROWSER_CDP_COMMAND_TIMEOUT_MS,
        )
        .await?;
        let mut state = self.inner.state.lock().await;
        if let Some(tab) = state.tabs.get_mut(tab_id) {
            tab.status = BrowserTabStatus::Loading;
            tab.last_activity_at_ms = unix_timestamp_ms();
        }
        Ok(())
    }

    /// Reflects a panel-driven tab selection.
    pub async fn select_tab(
        &self,
        session_id: &BrowserSessionId,
        tab_id: &BrowserTabId,
    ) -> BrowserResult<()> {
        let mut state = self.inner.state.lock().await;
        let session = state.sessions.get_mut(session_id).ok_or_else(|| {
            BrowserError::validation(
                "browser_session_not_found",
                "the browser session was not found",
            )
        })?;
        if !session.tabs.contains(tab_id) {
            return Err(BrowserError::validation(
                "browser_tab_not_in_session",
                "the tab does not belong to this browser session",
            ));
        }
        session.active_tab_id = Some(tab_id.clone());
        session.user_engaged = true;
        session.last_activity_at_ms = unix_timestamp_ms();
        Ok(())
    }

    /// Emits a tool-call outcome's ledger entries.
    pub(crate) async fn publish_records(&self, records: &[BrowserActionRecord]) {
        if records.is_empty() {
            return;
        }
        let mut state = self.inner.state.lock().await;
        for record in records {
            state.push_ledger(record.clone());
        }
        drop(state);
        for record in records {
            let _ = self
                .inner
                .events
                .send(BrowserServiceEvent::Action(Box::new(record.clone())));
        }
    }

    // ---------------------------------------------------------------------
    // Accessors used by the tool executor.
    // ---------------------------------------------------------------------

    /// Shared access to the service internals for the tool executor.
    pub(crate) fn inner(&self) -> &Arc<BrowserInner> {
        &self.inner
    }
}

impl BrowserInner {
    /// Latest frames and session bookkeeping accessors for the executor.
    pub(crate) async fn tab_session(
        &self,
        tab_id: &BrowserTabId,
    ) -> BrowserResult<(Arc<CdpConnection>, CdpSession)> {
        let state = self.state.lock().await;
        let tab = state.tabs.get(tab_id).ok_or_else(|| {
            BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
        })?;
        let connection = state
            .process
            .as_ref()
            .map(BrowserProcess::connection)
            .ok_or_else(browser_not_running)?;
        Ok((
            Arc::clone(&connection),
            CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone()),
        ))
    }

    async fn ack_frame(&self, tab_id: &BrowserTabId) -> BrowserResult<()> {
        let (session, ack_session_id) = {
            let state = self.state.lock().await;
            let Some(tab) = state.tabs.get(tab_id) else {
                return Ok(());
            };
            let Some(connection) = state.process.as_ref().map(BrowserProcess::connection) else {
                return Ok(());
            };
            (
                CdpSession::new(connection, tab.session_id.clone(), tab.target_id.clone()),
                tab.frame_ack_session_id,
            )
        };
        let Some(ack_session_id) = ack_session_id else {
            return Ok(());
        };
        cdp(
            &session,
            "Page.screencastFrameAck",
            json!({ "sessionId": ack_session_id }),
            SHORT_TIMEOUT_MS,
        )
        .await?;
        Ok(())
    }

    /// Closes an idle browser that no session is using.
    async fn reap_idle(&self) {
        let idle_for = unix_timestamp_ms() - self.state.lock().await.last_activity_ms;
        if idle_for < BROWSER_IDLE_TIMEOUT_MS {
            return;
        }
        let process = {
            let mut state = self.state.lock().await;
            // A session with tabs is in use even if it has been quiet.
            if state
                .sessions
                .values()
                .any(|session| !session.tabs.is_empty())
            {
                return;
            }
            state.sessions.clear();
            state.process.take()
        };
        if let Some(process) = process {
            tracing::info!(
                target: "vibex_browser",
                "closing the idle embedded browser"
            );
            process.shutdown().await;
        }
    }
}

fn browser_not_running() -> BrowserError {
    BrowserError::process("browser_not_running", "the embedded browser is not running")
}

fn preferred_transport() -> CdpTransportKind {
    #[cfg(unix)]
    {
        CdpTransportKind::Pipe
    }
    #[cfg(not(unix))]
    {
        CdpTransportKind::WebSocket
    }
}

/// Stable, non-reversible profile directory name for a browser family.
///
/// The profile is keyed by browser family rather than by workspace because
/// Chrome supports exactly one persistent `user-data-dir` per process: a
/// per-workspace profile would mean one browser process per workspace.
fn profile_key(browser_id: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(browser_id.as_bytes());
    let digest = format!("{:x}", hasher.finalize());
    digest[..16].to_string()
}

pub(crate) async fn cdp(
    session: &CdpSession,
    method: &str,
    params: Value,
    timeout_ms: u64,
) -> BrowserResult<Value> {
    session
        .command(method, params, Duration::from_millis(timeout_ms))
        .await
}

/// Enables the domains a tab needs for observation, diagnostics and the
/// browser-shell replacements a headless browser cannot provide natively.
async fn prepare_tab_session(session: &CdpSession) -> BrowserResult<()> {
    for (method, params) in [
        ("Page.enable", json!({})),
        ("Runtime.enable", json!({})),
        ("Log.enable", json!({})),
        ("Network.enable", json!({})),
        ("DOM.enable", json!({})),
        ("Accessibility.enable", json!({})),
        ("Overlay.enable", json!({})),
        // The runtime answers the chooser itself: there is no native dialog to
        // show, and a page must never pick its own file.
        (
            "Page.setInterceptFileChooserDialog",
            json!({ "enabled": true }),
        ),
        // HTTP auth challenges and permission prompts have no headless UI to
        // answer them. Chrome declines both itself and the load settles, so no
        // `Fetch` interception is installed: `Fetch.authRequired` only fires for
        // requests the patterns match, and enabling it without patterns pauses
        // every request. The behaviour is pinned by the live transport test.
        // Downloads default to denied. A page never chooses a write path.
        (
            "Browser.setDownloadBehavior",
            json!({ "behavior": "deny", "eventsEnabled": false }),
        ),
        // A headless browser has no window focus, and Blink only runs the text
        // selection gesture in a frame it believes is focused — dragging across
        // a paragraph selected nothing without this. Puppeteer enables it for
        // the same reason.
        (
            "Emulation.setFocusEmulationEnabled",
            json!({ "enabled": true }),
        ),
        // Cross-origin iframes live in their own target. Auto-attaching on the
        // tab's own session scopes the child session to this tab, which is what
        // lets the accessibility tree reach into it.
        (
            "Target.setAutoAttach",
            json!({ "autoAttach": true, "waitForDebuggerOnStart": false, "flatten": true }),
        ),
    ] {
        cdp(session, method, params, BROWSER_CDP_COMMAND_TIMEOUT_MS).await?;
    }
    Ok(())
}

/// True when a panel input is deliberate enough to count as a human takeover.
///
/// Pointer movement is excluded: the panel forwards every hover so the page can
/// show its own cursor affordances, and a takeover per hover would pause the
/// Agent as soon as the pointer crossed the frame.
pub(crate) fn input_takes_over(input: &BrowserInput) -> bool {
    matches!(
        input,
        BrowserInput::MouseDown { .. }
            | BrowserInput::MouseUp { .. }
            | BrowserInput::Wheel { .. }
            | BrowserInput::Key { .. }
            | BrowserInput::InsertText { .. }
    )
}

/// The CDP `buttons` bitmask for one button name.
pub(crate) fn button_mask(button: &str) -> i32 {
    match button {
        "left" => 1,
        "right" => 2,
        "middle" => 4,
        _ => 0,
    }
}

/// Converts a panel input event into a CDP command.
/// The button a CDP `buttons` bitmask says is held.
///
/// CDP's masks are Left 1, Right 2, Middle 4, Back 8, Forward 16.
fn held_button(buttons: i32) -> &'static str {
    if buttons & 1 != 0 {
        "left"
    } else if buttons & 2 != 0 {
        "right"
    } else if buttons & 4 != 0 {
        "middle"
    } else if buttons & 8 != 0 {
        "back"
    } else if buttons & 16 != 0 {
        "forward"
    } else {
        "none"
    }
}

pub(crate) fn input_to_cdp(input: BrowserInput) -> (&'static str, Value) {
    match input {
        BrowserInput::MouseMove { x, y, buttons } => (
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseMoved",
                "x": x,
                "y": y,
                // While a button is held, the move has to name it: Blink only
                // runs the selection gesture — dragging across a paragraph —
                // when the moved button is the pressed one. `none` here clicked
                // and scrolled fine but selected nothing.
                "button": held_button(buttons),
                "buttons": buttons,
                "pointerType": "mouse",
            }),
        ),
        BrowserInput::MouseDown {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => (
            "Input.dispatchMouseEvent",
            json!({
                "type": "mousePressed",
                "x": x,
                "y": y,
                "button": button,
                "buttons": button_mask(&button),
                "clickCount": click_count,
                "modifiers": modifiers,
            }),
        ),
        BrowserInput::MouseUp {
            x,
            y,
            button,
            click_count,
            modifiers,
        } => (
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseReleased",
                "x": x,
                "y": y,
                "button": button,
                // Released: the button is no longer held.
                "buttons": 0,
                "clickCount": click_count,
                "modifiers": modifiers,
            }),
        ),
        BrowserInput::Wheel {
            x,
            y,
            delta_x,
            delta_y,
        } => (
            "Input.dispatchMouseEvent",
            json!({
                "type": "mouseWheel",
                "x": x,
                "y": y,
                "deltaX": delta_x,
                "deltaY": delta_y,
            }),
        ),
        BrowserInput::Key {
            event_type,
            key,
            code,
            text,
            modifiers,
            windows_key_code,
        } => {
            let mut params = json!({
                "type": event_type,
                "key": key,
                "code": code,
                "modifiers": modifiers,
                "windowsVirtualKeyCode": windows_key_code,
                "nativeVirtualKeyCode": windows_key_code,
            });
            if let Some(text) = text {
                params["text"] = Value::String(text);
            }
            ("Input.dispatchKeyEvent", params)
        }
        BrowserInput::InsertText { text } => ("Input.insertText", json!({ "text": text })),
        // Resize is handled before this point; kept total for the type system.
        BrowserInput::Resize { .. } => ("Page.bringToFront", json!({})),
    }
}

/// Routes one CDP event into the service state.
async fn handle_cdp_event(
    inner: &Arc<BrowserInner>,
    connection: &Arc<CdpConnection>,
    event: CdpEvent,
) {
    // Target lifecycle events arrive on the browser session, before any tab
    // session exists for the target they describe.
    match event.method.as_str() {
        "Target.targetCreated" if event.session_id.is_none() => {
            adopt_discovered_target(inner, connection, &event.params, true).await;
            return;
        }
        "Target.targetInfoChanged" if event.session_id.is_none() => {
            adopt_discovered_target(inner, connection, &event.params, false).await;
            return;
        }
        "Target.targetDestroyed" if event.session_id.is_none() => {
            handle_target_destroyed(inner, &event.params).await;
            return;
        }
        _ => {}
    }
    let Some(session_id) = event.session_id.clone() else {
        return;
    };
    match event.method.as_str() {
        "Page.screencastFrame" => {
            handle_screencast_frame(inner, &session_id, &event.params).await;
        }
        "Page.javascriptDialogOpening" => {
            let request = BrowserDialogRequest {
                tab_id: BrowserTabId::new(),
                dialog_type: event
                    .params
                    .get("type")
                    .and_then(Value::as_str)
                    .unwrap_or("alert")
                    .to_string(),
                message: event
                    .params
                    .get("message")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                default_prompt: event
                    .params
                    .get("defaultPrompt")
                    .and_then(Value::as_str)
                    .map(str::to_string),
                at_ms: unix_timestamp_ms(),
            };
            let mut state = inner.state.lock().await;
            let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                return;
            };
            let request = BrowserDialogRequest {
                tab_id: tab.tab_id.clone(),
                ..request
            };
            tab.pending_dialog = Some(request.clone());
            drop(state);
            let _ = inner
                .events
                .send(BrowserServiceEvent::DialogOpened(Box::new(request)));
        }
        "Page.fileChooserOpened" => {
            let backend_node_id = event.params.get("backendNodeId").and_then(Value::as_i64);
            let mut state = inner.state.lock().await;
            let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                return;
            };
            tab.file_chooser_pending = true;
            tab.file_chooser_backend_node = backend_node_id;
            let tab_id = tab.tab_id.clone();
            drop(state);
            let _ = inner
                .events
                .send(BrowserServiceEvent::FileChooserOpened(tab_id));
        }
        "Page.frameNavigated" => {
            let frame = event.params.get("frame");
            let url = frame
                .and_then(|frame| frame.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let is_main = frame
                .and_then(|frame| frame.get("parentId"))
                .map(Value::is_null)
                .unwrap_or(true);
            if !is_main {
                return;
            }
            let (tab_id, session_ids) = {
                let mut state = inner.state.lock().await;
                let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                    return;
                };
                // A failed navigation lands on Chrome's own error page. Adopting
                // its internal URL would replace what the human typed — and what
                // an Agent asked for — with `chrome-error://chromewebdata/`, so
                // the requested URL stays; the failure itself is already in the
                // network diagnostics.
                if let Some(url) = url.filter(|url| !is_chrome_error_page(url)) {
                    tab.url = url;
                }
                // A navigation invalidates every ref issued before it.
                tab.generation = tab.generation.saturating_add(1);
                tab.elements.clear();
                tab.status = BrowserTabStatus::Loading;
                let tab_id = tab.tab_id.clone();
                (tab_id.clone(), state.sessions_for_tab(&tab_id))
            };
            for session_id in session_ids {
                let _ = inner
                    .events
                    .send(BrowserServiceEvent::SessionChanged(session_id));
            }
            let _ = inner.events.send(BrowserServiceEvent::TabChanged(tab_id));
            // The back and forward buttons are enabled from the history, so a
            // navigation has to refresh it.
            let session =
                CdpSession::new(Arc::clone(connection), session_id.clone(), String::new());
            let _ = refresh_navigation_state(inner, &session).await;
        }
        "Page.loadEventFired" | "Page.domContentEventFired" => {
            let tab_id = {
                let mut state = inner.state.lock().await;
                state.tab_by_cdp_session(&session_id).map(|tab| {
                    tab.status = BrowserTabStatus::Ready;
                    tab.last_activity_at_ms = unix_timestamp_ms();
                    tab.tab_id.clone()
                })
            };
            if let Some(tab_id) = tab_id {
                let _ = inner.events.send(BrowserServiceEvent::TabChanged(tab_id));
            }
        }
        "Runtime.consoleAPICalled" => {
            let level = event
                .params
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("log")
                .to_string();
            let text = event
                .params
                .get("args")
                .and_then(Value::as_array)
                .map(|args| {
                    args.iter()
                        .filter_map(|arg| {
                            arg.get("value").map(render_console_value).or_else(|| {
                                arg.get("description")
                                    .and_then(Value::as_str)
                                    .map(str::to_string)
                            })
                        })
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .unwrap_or_default();
            push_console(inner, &session_id, level, text, None, None).await;
        }
        "Runtime.exceptionThrown" => {
            let details = event.params.get("exceptionDetails");
            let text = details
                .and_then(|details| details.get("exception"))
                .and_then(|exception| exception.get("description"))
                .and_then(Value::as_str)
                .or_else(|| {
                    details
                        .and_then(|details| details.get("text"))
                        .and_then(Value::as_str)
                })
                .unwrap_or("uncaught exception")
                .to_string();
            let url = details
                .and_then(|details| details.get("url"))
                .and_then(Value::as_str)
                .map(str::to_string);
            let line = details
                .and_then(|details| details.get("lineNumber"))
                .and_then(Value::as_u64)
                .and_then(|line| u32::try_from(line).ok());
            push_console(inner, &session_id, "error".to_string(), text, url, line).await;
        }
        "Log.entryAdded" => {
            let entry = &event.params["entry"];
            let level = entry
                .get("level")
                .and_then(Value::as_str)
                .unwrap_or("info")
                .to_string();
            let text = entry
                .get("text")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let url = entry.get("url").and_then(Value::as_str).map(str::to_string);
            let line = entry
                .get("lineNumber")
                .and_then(Value::as_u64)
                .and_then(|line| u32::try_from(line).ok());
            push_console(inner, &session_id, level, text, url, line).await;
        }
        "Network.requestWillBeSent" => {
            let request_id = event
                .params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let url = event.params["request"]
                .get("url")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let method = event.params["request"]
                .get("method")
                .and_then(Value::as_str)
                .unwrap_or("GET")
                .to_string();
            let resource_type = event
                .params
                .get("type")
                .and_then(Value::as_str)
                .map(str::to_string);
            let mut state = inner.state.lock().await;
            let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                return;
            };
            let tab_id = tab.tab_id.clone();
            tab.diagnostics.push_network(BrowserNetworkEntry {
                tab_id,
                method,
                url,
                status: None,
                failure: None,
                at_ms: unix_timestamp_ms(),
                sequence: 0,
                resource_type,
            });
            let sequence = tab.diagnostics.network_sequence;
            tab.pending_requests.insert(request_id, sequence);
        }
        "Network.loadingFailed" => {
            let request_id = event
                .params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let failure = event
                .params
                .get("errorText")
                .and_then(Value::as_str)
                .unwrap_or("request failed")
                .to_string();
            let mut state = inner.state.lock().await;
            let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                return;
            };
            if let Some(sequence) = tab.pending_requests.remove(&request_id)
                && let Some(entry) = tab
                    .diagnostics
                    .network
                    .iter_mut()
                    .find(|entry| entry.sequence == sequence)
            {
                entry.failure = Some(failure);
            }
        }
        "Network.responseReceived" => {
            let status = event.params["response"]
                .get("status")
                .and_then(Value::as_u64)
                .and_then(|status| u16::try_from(status).ok());
            if !status.is_some_and(|status| status >= 400) {
                return;
            }
            let request_id = event
                .params
                .get("requestId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            let mut state = inner.state.lock().await;
            let Some(tab) = state.tab_by_cdp_session(&session_id) else {
                return;
            };
            if let Some(sequence) = tab.pending_requests.remove(&request_id)
                && let Some(entry) = tab
                    .diagnostics
                    .network
                    .iter_mut()
                    .find(|entry| entry.sequence == sequence)
            {
                entry.status = status;
            }
        }
        "Target.attachedToTarget" => {
            // A cross-origin iframe lives in its own target. Attaching lets the
            // accessibility tree reach content the main frame cannot see.
            let child_session = event.params["sessionInfo"]
                .get("sessionId")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if child_session.is_empty() {
                return;
            }
            // A page target attached here is a tab the page opened, not a child
            // frame: target discovery adopts it as a tab of its own. Let the
            // automatic session go so the tab owns exactly one session.
            if event.params["sessionInfo"]["targetInfo"]["type"].as_str() == Some("page") {
                let _ = connection
                    .command(
                        "Target.detachFromTarget",
                        json!({ "sessionId": child_session }),
                        Duration::from_millis(SHORT_TIMEOUT_MS),
                    )
                    .await;
                return;
            }
            let child =
                CdpSession::new(Arc::clone(connection), child_session.clone(), String::new());
            for method in ["Page.enable", "Accessibility.enable", "Runtime.enable"] {
                let _ = cdp(&child, method, json!({}), BROWSER_CDP_COMMAND_TIMEOUT_MS).await;
            }
            let mut state = inner.state.lock().await;
            if let Some(tab) = state.tab_by_cdp_session(&session_id) {
                tab.child_sessions.push(child_session);
            }
        }
        _ => {}
    }
}

/// Adopts a page target the browser discovered as a tab of this runtime.
///
/// `target="_blank"` and `window.open` create a page target of their own. A real
/// browser shows it as a new tab; without adopting it the panel keeps rendering
/// the opener and the click looks like it did nothing.
async fn adopt_discovered_target(
    inner: &Arc<BrowserInner>,
    connection: &Arc<CdpConnection>,
    params: &Value,
    created: bool,
) {
    if created && inner.creating_targets.load(Ordering::SeqCst) > 0 {
        return;
    }
    let info = &params["targetInfo"];
    if info.get("type").and_then(Value::as_str) != Some("page") {
        return;
    }
    let Some(target_id) = info.get("targetId").and_then(Value::as_str) else {
        return;
    };
    let url = info
        .get("url")
        .and_then(Value::as_str)
        .unwrap_or("about:blank")
        .to_string();
    let title = info
        .get("title")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_string();
    let opener_id = info
        .get("openerId")
        .and_then(Value::as_str)
        .map(str::to_string);

    // A tab this runtime already knows: the event is a title or URL update.
    let known = {
        let mut state = inner.state.lock().await;
        if let Some(tab) = state
            .tabs
            .values_mut()
            .find(|tab| tab.target_id == target_id)
        {
            let changed = tab.url != url || tab.title != title;
            // A failed load lands on Chrome's error page; the tab keeps showing
            // what was actually requested.
            if !is_chrome_error_page(&url) {
                tab.url = url.clone();
            }
            tab.title = title.clone();
            Some((tab.tab_id.clone(), changed))
        } else {
            // Targets that existed before discovery was switched on are not
            // tabs; the initial burst must not adopt them.
            if created && state.ignored_targets.remove(target_id) {
                return;
            }
            None
        }
    };
    if let Some((tab_id, changed)) = known {
        if changed {
            let _ = inner.events.send(BrowserServiceEvent::TabChanged(tab_id));
        }
        return;
    }
    if !created {
        return;
    }

    // The opener decides which panel session the tab belongs to and how large
    // it is drawn: the popup takes the opener's place in the same panel.
    let (session_id, viewport) = {
        let state = inner.state.lock().await;
        let opener = opener_id
            .as_deref()
            .and_then(|opener| state.tabs.values().find(|tab| tab.target_id == opener));
        let session_id = opener
            .and_then(|tab| {
                state
                    .sessions
                    .iter()
                    .find(|(_, session)| session.tabs.contains(&tab.tab_id))
                    .map(|(id, _)| id.clone())
            })
            .or_else(|| {
                (state.sessions.len() == 1)
                    .then(|| state.sessions.keys().next().cloned())
                    .flatten()
            });
        let viewport = opener.map(|tab| tab.viewport).unwrap_or((
            DEFAULT_VIEWPORT_WIDTH,
            DEFAULT_VIEWPORT_HEIGHT,
            1.0,
        ));
        (session_id, viewport)
    };
    let Some(session_id) = session_id else {
        return;
    };

    let Ok(attached) = connection
        .command(
            "Target.attachToTarget",
            json!({ "targetId": target_id, "flatten": true }),
            Duration::from_millis(BROWSER_CDP_COMMAND_TIMEOUT_MS),
        )
        .await
    else {
        return;
    };
    let Some(cdp_session_id) = attached.get("sessionId").and_then(Value::as_str) else {
        return;
    };
    let cdp_session_id = cdp_session_id.to_string();
    let session = CdpSession::new(
        Arc::clone(connection),
        cdp_session_id.clone(),
        target_id.to_string(),
    );
    let _ = prepare_tab_session(&session).await;
    let _ = cdp(
        &session,
        "Emulation.setDeviceMetricsOverride",
        json!({
            "width": viewport.0,
            "height": viewport.1,
            "deviceScaleFactor": viewport.2,
            "mobile": false,
        }),
        BROWSER_CDP_COMMAND_TIMEOUT_MS,
    )
    .await;

    let now = unix_timestamp_ms();
    let tab_id = BrowserTabId::new();
    let (frame, _) = watch::channel(None);
    let agent_session_id = {
        let state = inner.state.lock().await;
        state
            .sessions
            .get(&session_id)
            .and_then(|session| match &session.key {
                BrowserSessionKey::Agent(agent) => Some(agent.clone()),
                _ => None,
            })
    };
    let record = TabRecord {
        tab_id: tab_id.clone(),
        target_id: target_id.to_string(),
        session_id: cdp_session_id,
        owner: BrowserTabOwner::User,
        agent_session_id,
        url,
        title,
        status: BrowserTabStatus::Loading,
        generation: 0,
        elements: Vec::new(),
        created_at_ms: now,
        last_activity_at_ms: now,
        frame,
        frame_sequence: Arc::new(AtomicU64::new(0)),
        frame_ack_session_id: None,
        screencast_active: false,
        diagnostics: TabDiagnostics::default(),
        pending_requests: HashMap::new(),
        child_sessions: Vec::new(),
        active_operations: Arc::new(AtomicU64::new(0)),
        aborted: Arc::new(AtomicBool::new(false)),
        pending_dialog: None,
        file_chooser_pending: false,
        file_chooser_backend_node: None,
        viewport,
        can_go_back: false,
        can_go_forward: false,
    };
    let inserted = {
        let mut state = inner.state.lock().await;
        if state.tabs.values().any(|tab| tab.target_id == target_id) {
            // A racing event already adopted it.
            false
        } else {
            state.tabs.insert(tab_id.clone(), record);
            match state.sessions.get_mut(&session_id) {
                Some(session) => {
                    session.tabs.push(tab_id.clone());
                    // The new tab is what the panel and the human look at, the
                    // same as a real browser foregrounding a link's target.
                    session.active_tab_id = Some(tab_id.clone());
                    session.last_activity_at_ms = now;
                    state.last_activity_ms = now;
                    true
                }
                None => {
                    state.tabs.remove(&tab_id);
                    false
                }
            }
        }
    };
    if !inserted {
        return;
    }
    let _ = inner
        .events
        .send(BrowserServiceEvent::TabOpened { session_id, tab_id });
}

/// Reads a tab's position in its navigation history into its record.
///
/// The panel's back and forward buttons are enabled from this, so it runs after
/// every navigation and after a history move, and only reports a change when the
/// answer actually changed.
async fn refresh_navigation_state(
    inner: &Arc<BrowserInner>,
    session: &CdpSession,
) -> BrowserResult<()> {
    let history = cdp(
        session,
        "Page.getNavigationHistory",
        json!({}),
        SHORT_TIMEOUT_MS,
    )
    .await?;
    let index = history
        .get("currentIndex")
        .and_then(Value::as_i64)
        .unwrap_or(0);
    let entries = history
        .get("entries")
        .and_then(Value::as_array)
        .map(Vec::len)
        .unwrap_or(0) as i64;
    let can_go_back = index > 0;
    let can_go_forward = index + 1 < entries;
    let changed = {
        let mut state = inner.state.lock().await;
        match state.tab_by_cdp_session(&session.session_id) {
            Some(tab) if tab.can_go_back != can_go_back || tab.can_go_forward != can_go_forward => {
                tab.can_go_back = can_go_back;
                tab.can_go_forward = can_go_forward;
                Some(tab.tab_id.clone())
            }
            _ => None,
        }
    };
    if let Some(tab_id) = changed {
        let _ = inner.events.send(BrowserServiceEvent::TabChanged(tab_id));
    }
    Ok(())
}

/// Resolves the node under a viewport point, in viewport CSS pixels.
async fn node_at_point(session: &CdpSession, x: f64, y: f64) -> BrowserResult<Option<i64>> {
    let located = cdp(
        session,
        "DOM.getNodeForLocation",
        json!({ "x": x, "y": y, "includeUserAgentShadowDOM": false }),
        SHORT_TIMEOUT_MS,
    )
    .await?;
    Ok(located.get("backendNodeId").and_then(Value::as_i64))
}

/// Removes a tab whose target went away on its own, such as `window.close()`.
async fn handle_target_destroyed(inner: &Arc<BrowserInner>, params: &Value) {
    let Some(target_id) = params.get("targetId").and_then(Value::as_str) else {
        return;
    };
    let closed = {
        let mut state = inner.state.lock().await;
        if state.ignored_targets.remove(target_id) {
            return;
        }
        let Some(tab_id) = state
            .tabs
            .values()
            .find(|tab| tab.target_id == target_id)
            .map(|tab| tab.tab_id.clone())
        else {
            return;
        };
        state.tabs.remove(&tab_id);
        for record in state.sessions.values_mut() {
            record.tabs.retain(|id| id != &tab_id);
            if record.active_tab_id.as_ref() == Some(&tab_id) {
                record.active_tab_id = record.tabs.first().cloned();
            }
            if record.agent_tab_id.as_ref() == Some(&tab_id) {
                record.agent_tab_id = None;
            }
        }
        tab_id
    };
    let _ = inner.events.send(BrowserServiceEvent::TabClosed(closed));
}

fn render_console_value(value: &Value) -> String {
    match value {
        Value::String(text) => text.clone(),
        other => other.to_string(),
    }
}

/// True for the internal URL Chrome navigates to when a page fails to load.
///
/// The error page itself is worth showing — the panel renders it as a frame —
/// but the URL is not: `chrome-error://chromewebdata/` is not something a human
/// typed or an Agent can act on.
fn is_chrome_error_page(url: &str) -> bool {
    url.starts_with("chrome-error://")
}

async fn push_console(
    inner: &Arc<BrowserInner>,
    cdp_session_id: &str,
    level: String,
    text: String,
    url: Option<String>,
    line: Option<u32>,
) {
    let mut state = inner.state.lock().await;
    if let Some(tab) = state.tab_by_cdp_session(cdp_session_id) {
        let entry = BrowserConsoleEntry {
            tab_id: tab.tab_id.clone(),
            level,
            text,
            at_ms: unix_timestamp_ms(),
            url,
            line,
            sequence: 0,
        };
        tab.diagnostics.push_console(entry);
    }
}

async fn handle_screencast_frame(inner: &Arc<BrowserInner>, cdp_session_id: &str, params: &Value) {
    use base64::Engine as _;
    let Some(data) = params.get("data").and_then(Value::as_str) else {
        return;
    };
    let Ok(bytes) = base64::engine::general_purpose::STANDARD.decode(data) else {
        return;
    };
    if bytes.len() > BROWSER_MAX_FRAME_BYTES {
        tracing::warn!(
            target: "vibex_browser",
            "dropped an oversized screencast frame"
        );
        return;
    }
    let metadata = params.get("metadata").cloned().unwrap_or(Value::Null);
    let frame_metadata = BrowserFrameMetadata {
        offset_top: metadata
            .get("offsetTop")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        page_scale_factor: metadata
            .get("pageScaleFactor")
            .and_then(Value::as_f64)
            .unwrap_or(1.0),
        device_width: metadata
            .get("deviceWidth")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        device_height: metadata
            .get("deviceHeight")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        scroll_offset_x: metadata
            .get("scrollOffsetX")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        scroll_offset_y: metadata
            .get("scrollOffsetY")
            .and_then(Value::as_f64)
            .unwrap_or(0.0),
        timestamp: metadata.get("timestamp").and_then(Value::as_f64),
    };
    let ack_session = params.get("sessionId").and_then(Value::as_i64);

    let mut state = inner.state.lock().await;
    let Some(tab) = state.tab_by_cdp_session(cdp_session_id) else {
        return;
    };
    let sequence = tab.frame_sequence.fetch_add(1, Ordering::SeqCst) + 1;
    tab.frame_ack_session_id = ack_session;
    let frame = BrowserFrame {
        tab_id: tab.tab_id.clone(),
        sequence,
        format: BrowserFrameFormat::Jpeg,
        bytes,
        metadata: frame_metadata,
    };
    // Latest-value semantics: a new frame overwrites whatever was waiting. The
    // consumer only ever gets the freshest picture, and the credit-based ack
    // means Chrome never runs ahead of it.
    let _ = tab.frame.send(Some(frame));
}

/// Reads the pruned accessibility tree for a tab, refreshing generation and
/// element refs in the process.
pub(crate) async fn observe_tab(
    inner: &Arc<BrowserInner>,
    tab_id: &BrowserTabId,
    max_elements: usize,
    depth: u16,
    filter: Option<(Option<String>, Option<String>)>,
) -> BrowserResult<(String, String, u64, Vec<PrunedElement>, bool)> {
    let (_, session) = inner.tab_session(tab_id).await?;
    let result = cdp(
        &session,
        "Accessibility.getFullAXTree",
        json!({ "depth": depth }),
        vibex_core::BROWSER_OBSERVE_TIMEOUT_MS,
    )
    .await?;
    let nodes = result
        .get("nodes")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut pruned = prune_ax_tree(&nodes, max_elements, depth);
    if let Some((role_filter, name_filter)) = filter {
        pruned.elements.retain(|element| {
            let role_matches = role_filter
                .as_deref()
                .map(|role| element.role.eq_ignore_ascii_case(role))
                .unwrap_or(true);
            let name_matches = name_filter
                .as_deref()
                .map(|name| {
                    element
                        .name
                        .to_ascii_lowercase()
                        .contains(&name.to_ascii_lowercase())
                })
                .unwrap_or(true);
            role_matches && name_matches
        });
    }

    let (url, title, generation) = {
        let mut state = inner.state.lock().await;
        let tab = state.tabs.get_mut(tab_id).ok_or_else(|| {
            BrowserError::validation("browser_tab_not_found", "the browser tab was not found")
        })?;
        tab.generation = tab.generation.saturating_add(1);
        tab.elements = pruned.elements.clone();
        tab.last_activity_at_ms = unix_timestamp_ms();
        (tab.url.clone(), tab.title.clone(), tab.generation)
    };
    Ok((url, title, generation, pruned.elements, pruned.truncated))
}

/// Refreshes a tab's title from the document.
pub(crate) async fn refresh_tab_title(inner: &Arc<BrowserInner>, tab_id: &BrowserTabId) {
    let Ok((_, session)) = inner.tab_session(tab_id).await else {
        return;
    };
    let Ok(result) = cdp(
        &session,
        "Runtime.evaluate",
        json!({ "expression": "document.title", "returnByValue": true }),
        SHORT_TIMEOUT_MS,
    )
    .await
    else {
        return;
    };
    let Some(title) = result
        .get("result")
        .and_then(|result| result.get("value"))
        .and_then(Value::as_str)
    else {
        return;
    };
    let mut state = inner.state.lock().await;
    if let Some(tab) = state.tabs.get_mut(tab_id) {
        tab.title = title.to_string();
    }
}

/// Reads the current effective observation settings, for the executor.
pub(crate) fn observation_settings(max_elements: Option<u32>, extended: bool) -> (usize, u16) {
    (clamp_max_elements(max_elements), resolve_depth(extended))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A dev server only joins the allow-list once its port answers, and the
    /// runtime is told exactly once.
    #[tokio::test]
    async fn a_listening_dev_server_is_detected_and_announced() {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("a loopback listener");
        let port = listener.local_addr().expect("the listener address").port();
        let service = BrowserService::new(BrowserServiceConfig::new(
            std::env::temp_dir().join("vibex-browser-detector-test"),
        ));
        let mut events = service.subscribe();
        let workspace = WorkspaceId::new();

        // Vite prints its banner before it listens; the scanner must not care.
        service.observe_terminal_output(&workspace, "  VITE v5.4.0  ready in 320 ms\n");
        service.observe_terminal_output(
            &workspace,
            &format!("  ➜  Local:   http://127.0.0.1:{port}/\n"),
        );

        let announced = tokio::time::timeout(Duration::from_secs(10), async {
            loop {
                match events.recv().await {
                    Ok(BrowserServiceEvent::DevServerDetected { origin, .. }) => return origin,
                    Ok(_) => continue,
                    Err(broadcast::error::RecvError::Lagged(_)) => continue,
                    Err(broadcast::error::RecvError::Closed) => panic!("the event stream closed"),
                }
            }
        })
        .await
        .expect("the dev server is announced");
        assert_eq!(announced, format!("http://127.0.0.1:{port}"));

        // The origin is what lets the panel and the Agent reach the project's
        // own server without a domain approval.
        assert!(
            service
                .dev_server_origins()
                .await
                .iter()
                .any(|known| known == &announced),
            "the detected origin joins the workspace allow-list"
        );

        service.observe_terminal_output(&workspace, &format!("http://127.0.0.1:{port}/ again\n"));
        let second = tokio::time::timeout(Duration::from_millis(300), events.recv()).await;
        assert!(
            !matches!(
                second,
                Ok(Ok(BrowserServiceEvent::DevServerDetected { .. }))
            ),
            "a reprinted banner must not announce the same server twice"
        );
        drop(listener);
    }

    #[test]
    fn profile_key_is_stable_and_bounded() {
        let key = profile_key("chrome");
        assert_eq!(key, profile_key("chrome"));
        assert_ne!(key, profile_key("edge"));
        assert_eq!(key.len(), 16);
        assert!(!key.contains("chrome"));
    }

    #[test]
    fn preferred_transport_is_the_pipe_on_unix() {
        if cfg!(unix) {
            assert_eq!(preferred_transport(), CdpTransportKind::Pipe);
        } else {
            assert_eq!(preferred_transport(), CdpTransportKind::WebSocket);
        }
    }

    #[test]
    fn input_conversion_matches_cdp_shapes() {
        let (method, params) = input_to_cdp(BrowserInput::MouseMove {
            x: 1.0,
            y: 2.0,
            buttons: 0,
        });
        assert_eq!(method, "Input.dispatchMouseEvent");
        assert_eq!(params["type"], "mouseMoved");
        assert_eq!(params["x"], 1.0);

        let (method, params) = input_to_cdp(BrowserInput::MouseDown {
            x: 3.0,
            y: 4.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        });
        assert_eq!(method, "Input.dispatchMouseEvent");
        assert_eq!(params["type"], "mousePressed");
        assert_eq!(params["button"], "left");

        let (method, params) = input_to_cdp(BrowserInput::Wheel {
            x: 0.0,
            y: 0.0,
            delta_x: 0.0,
            delta_y: -120.0,
        });
        assert_eq!(params["deltaY"], -120.0);
        assert_eq!(method, "Input.dispatchMouseEvent");

        let (method, params) = input_to_cdp(BrowserInput::Key {
            event_type: "keyDown".to_string(),
            key: "Enter".to_string(),
            code: "Enter".to_string(),
            text: Some("\r".to_string()),
            modifiers: 0,
            windows_key_code: 13,
        });
        assert_eq!(method, "Input.dispatchKeyEvent");
        assert_eq!(params["key"], "Enter");
        assert_eq!(params["windowsVirtualKeyCode"], 13);
        assert_eq!(params["text"], "\r");

        let (method, params) = input_to_cdp(BrowserInput::InsertText {
            text: "こんにちは".to_string(),
        });
        assert_eq!(method, "Input.insertText");
        assert_eq!(params["text"], "こんにちは");
    }

    #[test]
    fn mouse_events_carry_the_held_button_mask() {
        // Chrome starts a drag only when the move says a button is down; a
        // scrollbar or a text selection otherwise never receives one.
        let (_, dragging) = input_to_cdp(BrowserInput::MouseMove {
            x: 1.0,
            y: 2.0,
            buttons: 1,
        });
        assert_eq!(dragging["buttons"], 1);
        // The move has to name the button that is down: Blink only runs the
        // selection gesture when the moved button is the pressed one, so
        // `none` here dragged nothing.
        assert_eq!(dragging["button"], "left");
        let (_, released_move) = input_to_cdp(BrowserInput::MouseMove {
            x: 1.0,
            y: 2.0,
            buttons: 0,
        });
        assert_eq!(released_move["button"], "none");
        assert_eq!(held_button(2), "right");
        assert_eq!(held_button(4), "middle");
        assert_eq!(held_button(0), "none");

        let (_, pressed) = input_to_cdp(BrowserInput::MouseDown {
            x: 1.0,
            y: 2.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        });
        assert_eq!(pressed["buttons"], 1);

        let (_, released) = input_to_cdp(BrowserInput::MouseUp {
            x: 1.0,
            y: 2.0,
            button: "left".to_string(),
            click_count: 1,
            modifiers: 0,
        });
        assert_eq!(released["buttons"], 0);

        assert_eq!(button_mask("right"), 2);
        assert_eq!(button_mask("middle"), 4);
        assert_eq!(button_mask("none"), 0);
    }

    #[test]
    fn key_events_without_text_omit_the_field() {
        let (_, params) = input_to_cdp(BrowserInput::Key {
            event_type: "keyUp".to_string(),
            key: "Shift".to_string(),
            code: "ShiftLeft".to_string(),
            text: None,
            modifiers: 8,
            windows_key_code: 16,
        });
        assert!(params.get("text").is_none());
        assert_eq!(params["modifiers"], 8);
    }

    #[test]
    fn observation_settings_are_clamped() {
        let (max, depth) = observation_settings(None, false);
        assert_eq!(max, vibex_core::BROWSER_OBSERVE_DEFAULT_MAX_ELEMENTS);
        assert_eq!(depth, vibex_core::BROWSER_OBSERVE_AX_DEPTH);
        let (_, depth) = observation_settings(Some(10), true);
        assert_eq!(depth, vibex_core::BROWSER_OBSERVE_EXTENDED_AX_DEPTH);
    }

    #[test]
    fn hovering_does_not_take_the_session_over() {
        // The panel forwards every hover; a takeover per hover would pause the
        // Agent the moment the pointer crossed the frame.
        assert!(!input_takes_over(&BrowserInput::MouseMove {
            x: 1.0,
            y: 2.0,
            buttons: 1,
        }));
        assert!(!input_takes_over(&BrowserInput::Resize {
            width: 800,
            height: 600,
            device_scale_factor: 1.0,
        }));
        for deliberate in [
            BrowserInput::MouseDown {
                x: 1.0,
                y: 2.0,
                button: "left".to_string(),
                click_count: 1,
                modifiers: 0,
            },
            BrowserInput::MouseUp {
                x: 1.0,
                y: 2.0,
                button: "left".to_string(),
                click_count: 1,
                modifiers: 0,
            },
            BrowserInput::Wheel {
                x: 0.0,
                y: 0.0,
                delta_x: 0.0,
                delta_y: -120.0,
            },
            BrowserInput::InsertText {
                text: "hi".to_string(),
            },
        ] {
            assert!(
                input_takes_over(&deliberate),
                "{deliberate:?} is deliberate input"
            );
        }
    }

    #[test]
    fn console_values_render_strings_without_quotes() {
        assert_eq!(render_console_value(&json!("hello")), "hello");
        assert_eq!(render_console_value(&json!(42)), "42");
    }

    #[test]
    fn only_the_error_page_url_is_refused_as_a_tab_url() {
        // The requested URL survives a failed load; Chrome's internal error URL
        // never becomes the tab's address.
        assert!(is_chrome_error_page("chrome-error://chromewebdata/"));
        assert!(!is_chrome_error_page("https://example.com/"));
        assert!(!is_chrome_error_page("chrome://version/"));
    }
}

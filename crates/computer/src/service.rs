//! The computer-use service: sessions, observations, policy and the ledger.
//!
//! This is the runtime's authority over the desktop. Everything that decides
//! *whether* an action may run lives here, in this order:
//!
//! 1. **Session state.** A stopped or paused session refuses every action with
//!    the state in the error code, so a model can tell "the user stopped me"
//!    from "the tool failed".
//! 2. **Target resolution.** The model's `app` string is replaced by the
//!    canonical application the engine reports. An ambiguous name is an error.
//! 3. **The self-target guard.** A target that may be Vibex itself is refused
//!    before any policy question is asked.
//! 4. **Risk assessment.** Credentials are refused outright; destructive and
//!    foreground actions require a one-off approval; ordinary actions run.
//! 5. **Reference freshness.** Element references are generation-scoped and the
//!    tree digest is re-checked against a fresh observation, so a click can
//!    never land on whatever moved into that position.
//!
//! Only then does the engine get called — and everything it returns is turned
//! into an honest status: `verified` only when the engine asserted the effect,
//! `unverified(reason)` otherwise, never "success" by default.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::time::Duration;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use tokio::sync::{Mutex, broadcast, watch};

use vibex_core::{
    COMPUTER_APPROVAL_TTL_MS, COMPUTER_DISCONNECT_HARD_MS, COMPUTER_DISCONNECT_SOFT_MS,
    COMPUTER_MAX_SESSION_LEDGER_ITEMS, COMPUTER_OBSERVE_DEFAULT_MAX_ELEMENTS,
    COMPUTER_OBSERVE_MAX_MAX_ELEMENTS, COMPUTER_OBSERVE_MIN_MAX_ELEMENTS, ComputerActionKind,
    ComputerActionRecord, ComputerApplication, ComputerApprovalGranularity, ComputerAvailability,
    ComputerDeliveryMode, ComputerElement, ComputerExecutionSource, ComputerFrame,
    ComputerObservation, ComputerOperationStatus, ComputerRiskClass, ComputerScreenshot,
    ComputerSession, ComputerSessionId, ComputerSessionState, ComputerToolTier,
    ComputerUnavailableReason, ComputerUnverifiedReason, ComputerVerification, RequestId,
    VibexSessionId, WorkspaceId, unix_timestamp_ms,
};

use crate::engine::{
    ComputerEngine, EngineActionResult, EngineClick, EngineDelivery, EnginePressKey, EngineScroll,
    EngineSetValue, EngineStateRequest, EngineTypeText,
};
use crate::error::{ComputerError, ComputerResult, codes};
use crate::loopguard::{LoopGuard, LoopWarning, fingerprint_bytes};
use crate::policy::{self, GrantStore, PolicyOutcome, PolicyRequest};
use crate::screenshot::decode_base64;
use crate::selfguard::SelfTargetGuard;
use crate::tools;

/// The decisions a user can change while the runtime is running.
///
/// Kept apart from [`ComputerServiceConfig`] because these are read on every
/// action: the settings panel must be able to change the approval policy, the
/// self-target allowance and the call timeout without restarting the runtime or
/// the helper.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComputerRuntimeSettings {
    /// Whether the feature may run at all. Live, because the settings switch
    /// turns it on and off without restarting the application.
    pub enabled: bool,
    pub approval_policy: vibex_core::ComputerApprovalPolicy,
    pub allow_self_target: bool,
    pub call_timeout_ms: u64,
    /// Whether the engine's experimental Wayland backend is on for this host.
    ///
    /// `None` means the process environment decides, which is what a host with
    /// no settings page (CLI, TUI, diagnostics) does: the variable is the
    /// user's own export. A host with a settings page resolves its own answer —
    /// including the automatic detection — and stores it here, so the
    /// capability statement the page shows describes what the helper was
    /// actually started with instead of an environment variable nobody set.
    pub wayland_opt_in: Option<bool>,
}

impl ComputerRuntimeSettings {
    /// Whether the engine's Wayland backend is on for this host.
    pub fn wayland_backend_enabled(&self) -> bool {
        self.wayland_opt_in.unwrap_or_else(|| {
            std::env::var_os(vibex_core::ComputerPlatform::wayland_opt_in_variable()).is_some()
        })
    }
}

impl Default for ComputerRuntimeSettings {
    fn default() -> Self {
        Self {
            enabled: true,
            approval_policy: vibex_core::ComputerApprovalPolicy::Ask,
            allow_self_target: false,
            call_timeout_ms: vibex_core::COMPUTER_CALL_TIMEOUT_DEFAULT_MS,
            wayland_opt_in: None,
        }
    }
}

/// How the service is configured for one runtime.
#[derive(Debug, Clone)]
pub struct ComputerServiceConfig {
    /// Runtime home; screenshots written for the CLI live under it.
    pub home_dir: PathBuf,
    /// How long an approval card stays open.
    pub approval_ttl_ms: i64,
    /// Disconnect soft threshold.
    pub disconnect_soft_ms: i64,
    /// Disconnect hard threshold.
    pub disconnect_hard_ms: i64,
    /// How often the panel frame pump captures the desktop.
    pub frame_interval_ms: u64,
    /// Whether the feature is switched on at all.
    pub enabled: bool,
    /// The user's live decisions.
    pub settings: ComputerRuntimeSettings,
}

impl ComputerServiceConfig {
    pub fn new(home_dir: impl Into<PathBuf>) -> Self {
        Self {
            home_dir: home_dir.into(),
            approval_ttl_ms: COMPUTER_APPROVAL_TTL_MS,
            disconnect_soft_ms: COMPUTER_DISCONNECT_SOFT_MS,
            disconnect_hard_ms: COMPUTER_DISCONNECT_HARD_MS,
            frame_interval_ms: 700,
            enabled: true,
            settings: ComputerRuntimeSettings::default(),
        }
    }

    pub fn with_enabled(mut self, enabled: bool) -> Self {
        self.enabled = enabled;
        self
    }
}

/// Who a computer-use session belongs to.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ComputerSessionKey {
    Agent(VibexSessionId),
    Workspace(WorkspaceId),
    /// The panel's own session, which is how a human drives the desktop
    /// directly from Vibex.
    Panel,
}

/// Everything the runtime knows about the agent behind a token.
#[derive(Debug, Clone)]
pub struct ComputerToolContext {
    pub session_id: ComputerSessionId,
    pub agent_session_id: Option<VibexSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub tier: ComputerToolTier,
    pub agent_label: String,
    /// Approvals that travel with this call only.
    ///
    /// A one-off approval is attached to the retry that carries it and is never
    /// stored, so answering a card can never silently authorize a later call.
    pub approved: Vec<ApprovedAction>,
}

/// One approval carried by a retry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ApprovedAction {
    pub risk: ComputerRiskClass,
    /// The canonical application identity the approval was granted for.
    pub target: String,
    /// Whether the approval covers a foreground takeover.
    #[serde(default)]
    pub foreground: bool,
}

impl ComputerToolContext {
    pub fn new(session_id: ComputerSessionId, tier: ComputerToolTier) -> Self {
        Self {
            session_id,
            agent_session_id: None,
            workspace_id: None,
            tier,
            agent_label: "Agent".to_string(),
            approved: Vec::new(),
        }
    }

    /// Attaches one approval and returns the new context.
    pub fn with_approval(mut self, approval: ApprovedAction) -> Self {
        self.approved.push(approval);
        self
    }

    fn approval_for(
        &self,
        risk: ComputerRiskClass,
        target: &ComputerApplication,
        foreground: bool,
    ) -> bool {
        let identity = target.canonical_identity();
        self.approved.iter().any(|approval| {
            approval.risk == risk
                && approval.target == identity
                && (!foreground || approval.foreground)
        })
    }
}

/// One encoded image returned to an agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComputerImageContent {
    pub mime_type: String,
    pub base64: String,
}

/// What a tool call produced.
#[derive(Debug, Clone)]
pub struct ComputerToolOutcome {
    pub text: String,
    pub is_error: bool,
    pub images: Vec<ComputerImageContent>,
    /// The redacted ledger entry, when the call reached an action.
    pub record: Option<ComputerActionRecord>,
    /// An advisory loop warning, when the recent history looks stuck.
    pub loop_warning: Option<LoopWarning>,
    /// Set when the call needs a human answer before it can run.
    pub approval: Option<ComputerApprovalRequest>,
}

impl ComputerToolOutcome {
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            text: text.into(),
            is_error: false,
            images: Vec::new(),
            record: None,
            loop_warning: None,
            approval: None,
        }
    }

    pub fn error(error: &ComputerError) -> Self {
        let mut outcome = Self::text(error.message.clone());
        outcome.is_error = true;
        outcome
    }

    pub fn with_image(mut self, image: ComputerImageContent) -> Self {
        self.images.push(image);
        self
    }
}

/// The authorization outcome for one action.
enum Gate {
    /// The action may run, at this risk class.
    Proceed { risk: ComputerRiskClass },
    /// A human must answer first; the wrapped outcome carries the card.
    Ask { outcome: Box<ComputerToolOutcome> },
    /// The action is refused and no approval can change that.
    Deny {
        error: ComputerError,
        risk: ComputerRiskClass,
    },
}

/// What a human must answer before an action runs.
#[derive(Debug, Clone, PartialEq)]
pub struct ComputerApprovalRequest {
    pub risk: ComputerRiskClass,
    pub classes: Vec<ComputerRiskClass>,
    pub title: String,
    pub details: Vec<(String, String)>,
    pub reason: String,
    pub granularity: ComputerApprovalGranularity,
    /// A screenshot of the target, for the destructive classes where the human
    /// has to see what is about to be pressed.
    pub screenshot: Option<ComputerScreenshot>,
    pub target: ComputerApplication,
}

/// Events the panel and other subscribers consume.
#[derive(Debug, Clone)]
pub enum ComputerServiceEvent {
    SessionChanged(Box<ComputerSession>),
    Action(Box<ComputerActionRecord>),
    Frame(Box<ComputerFrame>),
    Availability(Box<ComputerAvailability>),
    Stopped {
        session_id: ComputerSessionId,
        reason: String,
    },
    Paused {
        session_id: ComputerSessionId,
        reason: String,
    },
    Resumed {
        session_id: ComputerSessionId,
    },
    /// Pending approval cards must be withdrawn: the session stopped or the
    /// control channel went silent, and a later approval would be an answer to
    /// a question whose context is gone.
    ApprovalsInvalidated {
        session_id: ComputerSessionId,
        reason: String,
    },
    LoopWarning {
        session_id: ComputerSessionId,
        warning: LoopWarning,
    },
}

/// A cached observation, keyed by the canonical application identity.
#[derive(Debug, Clone)]
struct CachedObservation {
    generation: u64,
    tree_digest: String,
    elements: Vec<ComputerElement>,
    window_id: Option<String>,
    at_ms: i64,
}

/// How long a cached observation may be reused for reference validation.
const OBSERVATION_TTL_MS: i64 = 120_000;

/// Live state for one session.
struct SessionRuntime {
    session: ComputerSession,
    snapshot: Option<CachedObservation>,
    ledger: Vec<ComputerActionRecord>,
    grants: GrantStore,
    loop_guard: LoopGuard,
    /// The last frame delivered to a subscriber, so a late subscriber does not
    /// wait a whole interval.
    last_frame: Option<ComputerFrame>,
    frame_sequence: u64,
}

impl SessionRuntime {
    fn new(session: ComputerSession) -> Self {
        Self {
            session,
            snapshot: None,
            ledger: Vec::new(),
            grants: GrantStore::new(),
            loop_guard: LoopGuard::new(),
            last_frame: None,
            frame_sequence: 0,
        }
    }

    fn push_ledger(&mut self, record: ComputerActionRecord) {
        self.ledger.push(record);
        while self.ledger.len() > COMPUTER_MAX_SESSION_LEDGER_ITEMS {
            self.ledger.remove(0);
        }
    }
}

/// The computer-use service.
#[derive(Clone)]
pub struct ComputerService {
    inner: Arc<ComputerServiceInner>,
}

struct ComputerServiceInner {
    config: ComputerServiceConfig,
    engine: std::sync::RwLock<Option<Arc<dyn ComputerEngine>>>,
    sessions: Mutex<HashMap<ComputerSessionId, SessionRuntime>>,
    events: broadcast::Sender<ComputerServiceEvent>,
    frames: Mutex<HashMap<ComputerSessionId, watch::Sender<Option<ComputerFrame>>>>,
    /// Bumped whenever pending approvals become void.
    approval_epoch: AtomicU64,
    /// Whether the helper has been stopped by the emergency stop.
    stopped: AtomicBool,
    /// The loopback endpoint URL, once one is running.
    endpoint_url: std::sync::RwLock<Option<String>>,
    /// Set when the runtime is remote and the live hop is not implemented.
    remote_unsupported: AtomicBool,
    /// Last time a client proved it was still there.
    last_heartbeat_ms: AtomicU64,
    self_guard: Mutex<SelfTargetGuard>,
    frame_tasks: Mutex<HashMap<ComputerSessionId, tokio::task::JoinHandle<()>>>,
    /// The user's live decisions, read on every action so the settings panel
    /// does not need a restart to change them.
    settings: std::sync::RwLock<ComputerRuntimeSettings>,
    /// The process-backed helper, when the engine is one. Kept beside the
    /// engine because the lifecycle commands (resume, shutdown) have to reach
    /// the helper even when every session is stopped.
    helper: std::sync::RwLock<Option<Arc<crate::helper::HelperEngine>>>,
}

impl std::fmt::Debug for ComputerService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComputerService")
            .field("enabled", &self.inner.config.enabled)
            .field("endpoint", &self.endpoint_url())
            .finish()
    }
}

impl ComputerService {
    pub fn new(config: ComputerServiceConfig) -> Self {
        let (events, _) = broadcast::channel(256);
        let mut initial_settings = config.settings;
        // The constructor's `enabled` stays the startup answer; the live copy
        // is what the settings switch changes.
        initial_settings.enabled = config.enabled;
        Self {
            inner: Arc::new(ComputerServiceInner {
                config,
                engine: std::sync::RwLock::new(None),
                sessions: Mutex::new(HashMap::new()),
                events,
                frames: Mutex::new(HashMap::new()),
                approval_epoch: AtomicU64::new(1),
                stopped: AtomicBool::new(false),
                endpoint_url: std::sync::RwLock::new(None),
                remote_unsupported: AtomicBool::new(false),
                last_heartbeat_ms: AtomicU64::new(unix_timestamp_ms().max(0) as u64),
                self_guard: Mutex::new(SelfTargetGuard::new()),
                frame_tasks: Mutex::new(HashMap::new()),
                settings: std::sync::RwLock::new(initial_settings),
                helper: std::sync::RwLock::new(None),
            }),
        }
    }

    /// Installs the engine. Called by the runtime after the helper is spawned,
    /// and by tests with a fixture.
    pub fn install_engine(&self, engine: Arc<dyn ComputerEngine>) {
        if let Ok(mut slot) = self.inner.engine.write() {
            *slot = Some(engine);
        }
    }

    /// Records the helper process, so lifecycle commands can reach it.
    pub fn install_helper(&self, helper: Arc<crate::helper::HelperEngine>) {
        if let Ok(mut slot) = self.inner.helper.write() {
            *slot = Some(helper);
        }
    }

    fn helper(&self) -> Option<Arc<crate::helper::HelperEngine>> {
        self.inner.helper.read().ok().and_then(|slot| slot.clone())
    }

    pub fn has_engine(&self) -> bool {
        self.inner
            .engine
            .read()
            .map(|slot| slot.is_some())
            .unwrap_or(false)
    }

    fn engine(&self) -> ComputerResult<Arc<dyn ComputerEngine>> {
        self.inner
            .engine
            .read()
            .ok()
            .and_then(|slot| slot.clone())
            .ok_or_else(|| {
                ComputerError::capability(
                    codes::ENGINE_MISSING,
                    "the desktop engine has not been started",
                )
                .with_recovery_hint(
                    "Install the desktop driver the product documents, then restart Vibex.",
                )
            })
    }

    pub fn config(&self) -> &ComputerServiceConfig {
        &self.inner.config
    }

    /// The user's live decisions.
    pub fn runtime_settings(&self) -> ComputerRuntimeSettings {
        self.inner
            .settings
            .read()
            .map(|settings| *settings)
            .unwrap_or_default()
    }

    /// Adopts the user's decisions without restarting anything.
    pub fn apply_runtime_settings(&self, settings: ComputerRuntimeSettings) {
        if let Ok(mut slot) = self.inner.settings.write() {
            *slot = settings;
        }
    }

    pub fn subscribe(&self) -> broadcast::Receiver<ComputerServiceEvent> {
        self.inner.events.subscribe()
    }

    /// Records the live MCP endpoint URL for the panel.
    pub fn set_endpoint_url(&self, url: Option<String>) {
        if let Ok(mut slot) = self.inner.endpoint_url.write() {
            *slot = Some(url).flatten();
        }
    }

    pub fn endpoint_url(&self) -> Option<String> {
        self.inner
            .endpoint_url
            .read()
            .ok()
            .and_then(|slot| slot.clone())
    }

    /// Marks the runtime host as remote, where the live desktop hop is not
    /// implemented yet. Explicit degradation, never a silent failure.
    pub fn set_remote_unsupported(&self, remote: bool) {
        self.inner
            .remote_unsupported
            .store(remote, Ordering::SeqCst);
    }

    /// The epoch pending approvals are valid for.
    pub fn approval_epoch(&self) -> u64 {
        self.inner.approval_epoch.load(Ordering::SeqCst)
    }

    /// True when the emergency stop is in force.
    pub fn is_stopped(&self) -> bool {
        self.inner.stopped.load(Ordering::SeqCst)
    }

    /// Registers the host's own window with the self-target guard.
    pub async fn register_host_window(
        &self,
        window_id: Option<String>,
        bounds: Option<vibex_core::ComputerRect>,
    ) {
        let mut guard = self.inner.self_guard.lock().await;
        guard.add_host_pid(std::process::id() as i32);
        if let Some(window_id) = window_id {
            guard.add_host_window(window_id, bounds);
        } else if let Some(bounds) = bounds {
            guard.add_host_bounds(bounds);
        }
    }

    /// Registers a rectangle the guard must protect even without a window id.
    pub async fn register_host_bounds(&self, bounds: vibex_core::ComputerRect) {
        self.inner.self_guard.lock().await.add_host_bounds(bounds);
    }

    /// Reads the availability report.
    ///
    /// A missing engine is not an error here: it is the reason the feature is
    /// unavailable, and the UI needs the reason more than it needs a failure.
    pub async fn availability(&self) -> ComputerAvailability {
        if !self.runtime_settings().enabled {
            return self.unavailable(ComputerUnavailableReason::FeatureDisabled, None);
        }
        if self.inner.remote_unsupported.load(Ordering::SeqCst) {
            return self.unavailable(
                ComputerUnavailableReason::RemoteRuntimeUnsupported,
                Some(
                    "The paired runtime is remote; the live desktop view over the remote transport \
                     is not implemented yet."
                        .to_string(),
                ),
            );
        }
        let Ok(engine) = self.engine() else {
            return self.unavailable(
                ComputerUnavailableReason::EngineMissing,
                Some(
                    "The desktop driver is not installed on the runtime host. Vibex does not \
                     install it for you."
                        .to_string(),
                ),
            );
        };
        match engine.probe().await {
            Ok(probe) => {
                let mut availability = ComputerAvailability {
                    unavailable_reason: probe.unavailable_reason,
                    platform: probe.platform,
                    support: vibex_core::ComputerPlatformSupport::for_platform(
                        probe.platform,
                        self.runtime_settings().wayland_backend_enabled(),
                    ),
                    permissions: probe.permissions,
                    engine: probe.engine,
                    detail: probe.detail,
                    degraded: probe.degraded,
                };
                if availability.unavailable_reason.is_none()
                    && !probe.permissions.structured_usable()
                {
                    availability.unavailable_reason =
                        Some(ComputerUnavailableReason::PermissionPending);
                }
                availability
            }
            Err(error) => self.unavailable(
                reason_for_error(&error),
                Some(format!("{}: {}", error.code, error.message)),
            ),
        }
    }

    fn unavailable(
        &self,
        reason: ComputerUnavailableReason,
        detail: Option<String>,
    ) -> ComputerAvailability {
        let platform = crate::driver::detect_platform();
        ComputerAvailability {
            unavailable_reason: Some(reason),
            platform,
            support: vibex_core::ComputerPlatformSupport::for_platform(
                platform,
                self.runtime_settings().wayland_backend_enabled(),
            ),
            permissions: vibex_core::ComputerPermissionReport::unsupported(),
            engine: None,
            detail,
            degraded: Vec::new(),
        }
    }

    /// Creates, or returns, the session for one owner.
    pub async fn ensure_session(
        &self,
        key: ComputerSessionKey,
        workspace_id: Option<WorkspaceId>,
        tier: ComputerToolTier,
    ) -> ComputerResult<ComputerSessionId> {
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(existing) = sessions
            .values()
            .find(|runtime| session_matches(&runtime.session, &key))
        {
            return Ok(existing.session.session_id.clone());
        }
        let now = unix_timestamp_ms();
        let session = ComputerSession {
            session_id: ComputerSessionId::new(),
            agent_session_id: match &key {
                ComputerSessionKey::Agent(id) => Some(id.clone()),
                _ => None,
            },
            workspace_id,
            state: if self.inner.stopped.load(Ordering::SeqCst) {
                ComputerSessionState::StoppedByUser
            } else {
                ComputerSessionState::Running
            },
            target: None,
            delivery_mode: None,
            tier,
            started_at_ms: now,
            last_activity_at_ms: now,
            paused_reason: None,
        };
        let session_id = session.session_id.clone();
        sessions.insert(session_id.clone(), SessionRuntime::new(session.clone()));
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::SessionChanged(Box::new(session)));
        Ok(session_id)
    }

    /// Creates the session for a tool context under the id that context
    /// already carries. A no-op when the session exists.
    pub async fn adopt_session(&self, context: &ComputerToolContext) {
        let mut sessions = self.inner.sessions.lock().await;
        if sessions.contains_key(&context.session_id) {
            return;
        }
        let now = unix_timestamp_ms();
        let session = ComputerSession {
            session_id: context.session_id.clone(),
            agent_session_id: context.agent_session_id.clone(),
            workspace_id: context.workspace_id.clone(),
            state: if self.inner.stopped.load(Ordering::SeqCst) {
                ComputerSessionState::StoppedByUser
            } else {
                ComputerSessionState::Running
            },
            target: None,
            delivery_mode: None,
            tier: context.tier,
            started_at_ms: now,
            last_activity_at_ms: now,
            paused_reason: None,
        };
        sessions.insert(
            context.session_id.clone(),
            SessionRuntime::new(session.clone()),
        );
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::SessionChanged(Box::new(session)));
    }

    pub async fn session(&self, session_id: &ComputerSessionId) -> Option<ComputerSession> {
        self.inner
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|runtime| runtime.session.clone())
    }

    /// The full live state for the panel.
    pub async fn session_snapshot(
        &self,
        session_id: &ComputerSessionId,
    ) -> Option<vibex_core::ComputerSessionSnapshot> {
        let (session, ledger) = {
            let sessions = self.inner.sessions.lock().await;
            let runtime = sessions.get(session_id)?;
            (runtime.session.clone(), runtime.ledger.clone())
        };
        let availability = self.availability().await;
        Some(vibex_core::ComputerSessionSnapshot {
            session,
            availability,
            engine_tools: None,
            ledger,
        })
    }

    /// The action ledger, oldest first.
    pub async fn ledger(&self, session_id: &ComputerSessionId) -> Vec<ComputerActionRecord> {
        self.inner
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|runtime| runtime.ledger.clone())
            .unwrap_or_default()
    }

    /// A receiver for the human-facing frame channel.
    ///
    /// Latest-value semantics: a frame that arrives while the panel is busy
    /// replaces the one waiting rather than queueing behind it.
    pub async fn subscribe_frames(
        &self,
        session_id: &ComputerSessionId,
    ) -> watch::Receiver<Option<ComputerFrame>> {
        let mut frames = self.inner.frames.lock().await;
        if let Some(sender) = frames.get(session_id) {
            return sender.subscribe();
        }
        let (sender, receiver) = watch::channel(None);
        frames.insert(session_id.clone(), sender);
        drop(frames);
        self.start_frame_pump(session_id.clone()).await;
        receiver
    }

    async fn start_frame_pump(&self, session_id: ComputerSessionId) {
        let mut tasks = self.inner.frame_tasks.lock().await;
        if tasks.contains_key(&session_id) {
            return;
        }
        let service = self.clone();
        let pump_session = session_id.clone();
        let task = tokio::spawn(async move {
            let interval = Duration::from_millis(service.inner.config.frame_interval_ms.max(100));
            loop {
                tokio::time::sleep(interval).await;
                let Ok(engine) = service.engine() else {
                    return;
                };
                let Ok(Some(shot)) = engine.screenshot(None, None).await else {
                    continue;
                };
                service.publish_frame(&pump_session, shot).await;
            }
        });
        tasks.insert(session_id, task);
    }

    async fn publish_frame(&self, session_id: &ComputerSessionId, shot: ComputerScreenshot) {
        let Some(bytes) = decode_base64(&shot.base64) else {
            return;
        };
        let (sequence, frame) = {
            let mut sessions = self.inner.sessions.lock().await;
            let Some(runtime) = sessions.get_mut(session_id) else {
                return;
            };
            runtime.frame_sequence += 1;
            let frame = ComputerFrame {
                session_id: session_id.clone(),
                sequence: runtime.frame_sequence,
                format: shot.mime_type.clone(),
                bytes,
                width: shot.width,
                height: shot.height,
                origin_x: 0.0,
                origin_y: 0.0,
                scale: if shot.scale > 0.0 { shot.scale } else { 1.0 },
                at_ms: unix_timestamp_ms(),
            };
            runtime.last_frame = Some(frame.clone());
            (runtime.frame_sequence, frame)
        };
        let _ = sequence;
        if let Some(sender) = self.inner.frames.lock().await.get(session_id) {
            // `send` fails only when nobody is listening; the latest value is
            // then dropped, which is exactly the wanted semantics.
            let _ = sender.send(Some(frame.clone()));
        }
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::Frame(Box::new(frame)));
    }

    /// The last frame, for a panel that just attached.
    pub async fn last_frame(&self, session_id: &ComputerSessionId) -> Option<ComputerFrame> {
        self.inner
            .sessions
            .lock()
            .await
            .get(session_id)
            .and_then(|runtime| runtime.last_frame.clone())
    }

    /// A client proved it is still connected.
    pub fn note_client_heartbeat(&self) {
        self.inner
            .last_heartbeat_ms
            .store(unix_timestamp_ms().max(0) as u64, Ordering::SeqCst);
    }

    /// Applies the disconnect contract.
    ///
    /// Soft threshold: new actions are refused with `paused_offline` and every
    /// pending approval is void. Hard threshold: queued input is cleared and
    /// held keys are released, through the same path the emergency stop uses.
    /// Returns the state it settled on, or `None` when nothing changed.
    pub async fn enforce_disconnect_policy(&self, now_ms: i64) -> Option<ComputerSessionState> {
        let last = self.inner.last_heartbeat_ms.load(Ordering::SeqCst) as i64;
        let age = now_ms.saturating_sub(last);
        if age < self.inner.config.disconnect_soft_ms {
            return None;
        }
        let mut sessions = self.inner.sessions.lock().await;
        let mut changed = Vec::new();
        for runtime in sessions.values_mut() {
            if !matches!(
                runtime.session.state,
                ComputerSessionState::Running | ComputerSessionState::PausedByUser
            ) {
                continue;
            }
            runtime.session.state = ComputerSessionState::PausedOffline;
            runtime.session.paused_reason =
                Some("the controlling client stopped answering".to_string());
            changed.push(runtime.session.clone());
        }
        drop(sessions);
        if changed.is_empty() {
            return None;
        }
        self.inner.approval_epoch.fetch_add(1, Ordering::SeqCst);
        for session in &changed {
            let _ = self.inner.events.send(ComputerServiceEvent::Paused {
                session_id: session.session_id.clone(),
                reason: "the controlling client stopped answering".to_string(),
            });
            let _ = self
                .inner
                .events
                .send(ComputerServiceEvent::ApprovalsInvalidated {
                    session_id: session.session_id.clone(),
                    reason: "the controlling client disconnected".to_string(),
                });
            let _ = self
                .inner
                .events
                .send(ComputerServiceEvent::SessionChanged(Box::new(
                    session.clone(),
                )));
        }
        if age >= self.inner.config.disconnect_hard_ms {
            // The hard threshold shares the emergency stop's release path: a
            // held key outlives every other kind of state.
            if let Ok(engine) = self.engine() {
                let _ = engine.release_all_keys().await;
            }
        }
        changed.first().map(|session| session.state)
    }

    /// The emergency stop.
    ///
    /// Reaches the terminal state in one step: refuse new calls, drop queued
    /// input, release every held key, stop the helper, record the ledger entry
    /// and require a human to re-enable.
    pub async fn stop(&self, reason: &str) -> ComputerResult<()> {
        self.inner.stopped.store(true, Ordering::SeqCst);
        self.inner.approval_epoch.fetch_add(1, Ordering::SeqCst);
        let mut sessions = self.inner.sessions.lock().await;
        let mut updated = Vec::new();
        for runtime in sessions.values_mut() {
            runtime.session.state = ComputerSessionState::StoppedByUser;
            runtime.session.paused_reason = Some(reason.to_string());
            updated.push(runtime.session.clone());
        }
        drop(sessions);
        let now = unix_timestamp_ms();
        for session in &updated {
            let record = ComputerActionRecord {
                id: RequestId::new().as_str().to_string(),
                session_id: session.session_id.clone(),
                kind: ComputerActionKind::Stop,
                summary: format!("emergency stop: {reason}"),
                at_ms: now,
                status: ComputerOperationStatus::Verified,
                verification: ComputerVerification::Verified,
                risk: ComputerRiskClass::Ordinary,
                target: None,
                delivery_mode: None,
                execution_source: ComputerExecutionSource::User,
            };
            self.publish_record(record).await;
            let _ = self.inner.events.send(ComputerServiceEvent::Stopped {
                session_id: session.session_id.clone(),
                reason: reason.to_string(),
            });
            let _ = self
                .inner
                .events
                .send(ComputerServiceEvent::ApprovalsInvalidated {
                    session_id: session.session_id.clone(),
                    reason: reason.to_string(),
                });
        }
        // Release input before stopping the helper: the helper's stop does the
        // same, and doing it twice is harmless, while doing it never leaves a
        // key down.
        if let Ok(engine) = self.engine() {
            let _ = engine.release_all_keys().await;
        }
        Ok(())
    }

    /// A human re-enabled computer use after a stop.
    pub async fn resume(&self, session_id: &ComputerSessionId) -> ComputerResult<()> {
        self.inner.stopped.store(false, Ordering::SeqCst);
        self.note_client_heartbeat();
        let mut sessions = self.inner.sessions.lock().await;
        let Some(runtime) = sessions.get_mut(session_id) else {
            return Err(ComputerError::validation(
                codes::UNKNOWN_APP,
                "no such computer-use session",
            ));
        };
        runtime.session.state = ComputerSessionState::Running;
        runtime.session.paused_reason = None;
        let session = runtime.session.clone();
        drop(sessions);
        if let Some(helper) = self.helper() {
            helper.resume().await?;
        }
        let _ = self.inner.events.send(ComputerServiceEvent::Resumed {
            session_id: session_id.clone(),
        });
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::SessionChanged(Box::new(session)));
        Ok(())
    }

    /// A human paused Agent operations from the panel.
    pub async fn pause(&self, session_id: &ComputerSessionId, reason: &str) -> ComputerResult<()> {
        let mut sessions = self.inner.sessions.lock().await;
        let Some(runtime) = sessions.get_mut(session_id) else {
            return Err(ComputerError::validation(
                codes::UNKNOWN_APP,
                "no such computer-use session",
            ));
        };
        runtime.session.state = ComputerSessionState::PausedByUser;
        runtime.session.paused_reason = Some(reason.to_string());
        let session = runtime.session.clone();
        drop(sessions);
        self.inner.approval_epoch.fetch_add(1, Ordering::SeqCst);
        let _ = self.inner.events.send(ComputerServiceEvent::Paused {
            session_id: session_id.clone(),
            reason: reason.to_string(),
        });
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::SessionChanged(Box::new(session)));
        Ok(())
    }

    /// Called on runtime shutdown: release input and let the helper exit.
    pub async fn shutdown(&self) {
        if let Ok(engine) = self.engine() {
            let _ = engine.release_all_keys().await;
        }
        if let Some(helper) = self.helper() {
            helper.shutdown().await;
        }
        let mut tasks = self.inner.frame_tasks.lock().await;
        for task in tasks.drain() {
            task.1.abort();
        }
        self.inner.frames.lock().await.clear();
    }

    /// Handles one tool call.
    pub async fn call_tool_checked(
        &self,
        context: &ComputerToolContext,
        name: &str,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        // The runtime normally creates the session when it resolves the token.
        // Adopting one here as well keeps a call from failing with "no such
        // session" when the panel drives the service directly, and it cannot
        // create authority: the tier and the owner come from the resolved
        // context either way.
        self.adopt_session(context).await;
        let state = self
            .session_state(&context.session_id)
            .await
            .unwrap_or(ComputerSessionState::Running);
        if !state.accepts_actions() {
            return Err(state_error(state, name));
        }
        if self.inner.stopped.load(Ordering::SeqCst) {
            return Err(state_error(ComputerSessionState::StoppedByUser, name));
        }
        match name {
            tools::NAMES_LIST_APPS => self.tool_list_apps(context).await,
            tools::NAMES_LAUNCH_APP => self.tool_launch_app(context, arguments).await,
            tools::NAMES_GET_APP_STATE => self.tool_get_app_state(context, arguments).await,
            tools::NAMES_CLICK => self.tool_click(context, arguments).await,
            tools::NAMES_TYPE_TEXT => self.tool_type_text(context, arguments).await,
            tools::NAMES_SET_VALUE => self.tool_set_value(context, arguments).await,
            tools::NAMES_PRESS_KEY => self.tool_press_key(context, arguments).await,
            tools::NAMES_SCROLL => self.tool_scroll(context, arguments).await,
            tools::NAMES_PERMISSIONS => self.tool_permissions(context).await,
            other => Err(ComputerError::validation(
                "computer_tool_unknown",
                format!("`{other}` is not a computer-use tool"),
            )),
        }
    }

    async fn session_state(&self, session_id: &ComputerSessionId) -> Option<ComputerSessionState> {
        self.inner
            .sessions
            .lock()
            .await
            .get(session_id)
            .map(|runtime| runtime.session.state)
    }

    // ---------------------------------------------------------------- tools

    async fn tool_list_apps(
        &self,
        context: &ComputerToolContext,
    ) -> ComputerResult<ComputerToolOutcome> {
        let engine = self.engine()?;
        let apps = engine.list_apps().await?;
        self.touch_session(context.session_id.clone()).await;
        let mut lines = Vec::new();
        for app in &apps {
            let running = if app.running { "running" } else { "stopped" };
            lines.push(format!(
                "- {} [{}] ({running}){}",
                app.display_name,
                app.app_id,
                match &app.window_count_note() {
                    note if note.is_empty() => String::new(),
                    note => format!(" {note}"),
                }
            ));
        }
        let body = if lines.is_empty() {
            "No applications were reported on this desktop.".to_string()
        } else {
            lines.join("\n")
        };
        Ok(ComputerToolOutcome::text(format!(
            "Applications on the desktop ({}):\n{}",
            apps.len(),
            vibex_core::fence_untrusted_screen_content(&body)
        )))
    }

    /// Starts an application that is installed but not running.
    ///
    /// The launch is a real action with a real card: the engine spawns a
    /// process the user did not start, and the approval names the application.
    /// The result is deliberately not `verified` unless the engine reported a
    /// live process for it — "the launcher returned" is not "the window is
    /// ready", and the model is told to observe before acting.
    async fn tool_launch_app(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let risk = match self
            .gate(
                context,
                ComputerActionKind::LaunchApp,
                &app,
                None,
                false,
                false,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::LaunchApp, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let engine = self.engine()?;
        let launched = engine.launch_app(&app.app_id).await;
        let result = launched
            .as_ref()
            .map_err(|error| error.clone())
            .map(|launched| {
                EngineActionResult {
                    // The engine resolving a live process is a post-check of the
                    // launch; anything less is a dispatch whose effect is unproven.
                    asserted: launched.pid.is_some(),
                    unverified_reason: launched
                        .pid
                        .is_none()
                        .then_some(ComputerUnverifiedReason::MissingMetadata),
                    detail: Some(match launched.pid {
                        Some(pid) => format!("started {} (pid {pid})", launched.display_name),
                        None => format!("started {}", launched.display_name),
                    }),
                    cursor: None,
                    tree_digest_after: None,
                }
            });
        self.finish_action(
            context,
            ComputerActionKind::LaunchApp,
            &app,
            ComputerDeliveryMode::Background,
            format!("started {} on the desktop", app.display_name),
            result,
            risk,
        )
        .await
    }

    async fn tool_get_app_state(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let include_screenshot = arguments
            .get("include_screenshot")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        if include_screenshot && !context.tier.includes_screenshots() {
            return Err(ComputerError::capability(
                codes::SCREENSHOT_TIER,
                "This Agent is not offered screenshots: its adapter has not been confirmed to pass \
                 image content from a tool result to its model.",
            )
            .with_recovery_hint(
                "Use the accessibility tree this tool returns; it addresses the same elements.",
            ));
        }
        let max_elements = arguments
            .get("max_elements")
            .and_then(Value::as_u64)
            .map(|value| value as usize)
            .unwrap_or(COMPUTER_OBSERVE_DEFAULT_MAX_ELEMENTS)
            .clamp(
                COMPUTER_OBSERVE_MIN_MAX_ELEMENTS,
                COMPUTER_OBSERVE_MAX_MAX_ELEMENTS,
            );
        let extended = arguments
            .get("extended")
            .and_then(Value::as_bool)
            .unwrap_or(false);
        let app_selector = required_str(arguments, "app")?;
        let window_selector = arguments
            .get("window_id")
            .and_then(Value::as_str)
            .map(str::to_string);
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let engine = self.engine()?;
        let state = engine
            .get_app_state(EngineStateRequest {
                app_id: app.app_id.clone(),
                window_id: window_selector,
                max_elements,
                extended,
                screenshot: include_screenshot,
            })
            .await?;
        let observation = self
            .record_observation(context, &state, include_screenshot)
            .await;
        let record = self
            .record_action(
                context,
                ComputerActionKind::GetAppState,
                format!(
                    "observed {} ({} element(s){})",
                    app.display_name,
                    observation.elements.len(),
                    if observation.degraded {
                        ", degraded"
                    } else {
                        ""
                    }
                ),
                &app,
                ComputerOperationStatus::Verified,
                ComputerVerification::Verified,
                ComputerRiskClass::Ordinary,
                None,
            )
            .await;
        let mut outcome = ComputerToolOutcome::text(render_observation(&observation, &state));
        if let Some(shot) = &observation.screenshot {
            outcome = outcome.with_image(ComputerImageContent {
                mime_type: shot.mime_type.clone(),
                base64: shot.base64.clone(),
            });
        }
        outcome.record = Some(record);
        self.note_screen(context, state.screenshot.as_ref()).await;
        Ok(outcome)
    }

    async fn tool_click(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let reference = arguments
            .get("element")
            .and_then(Value::as_str)
            .map(str::to_string);
        let point = point_from(arguments);
        if reference.is_none() && point.is_none() {
            return Err(ComputerError::validation(
                "computer_arguments_invalid",
                "`element` or a coordinate pair is required",
            ));
        }
        let element = match &reference {
            Some(reference) => Some(
                self.validate_reference(context, &app, reference, arguments)
                    .await?,
            ),
            None => None,
        };
        if let Some((x, y)) = point
            && let Some(reason) = self.inner.self_guard.lock().await.block_point(x, y)
        {
            return Err(ComputerError::permission(codes::SELF_TARGET, reason));
        }
        let button = arguments
            .get("button")
            .and_then(Value::as_str)
            .unwrap_or("left")
            .to_string();
        let click_count = arguments
            .get("click_count")
            .and_then(Value::as_u64)
            .unwrap_or(1)
            .clamp(1, 3) as u32;
        let requested_foreground = arguments
            .get("delivery_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground"));
        let risk = match self
            .gate(
                context,
                ComputerActionKind::Click,
                &app,
                element.clone(),
                requested_foreground,
                false,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::Click, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let delivery = self.delivery_for(risk, requested_foreground);
        let engine = self.engine()?;
        let element_index = reference.as_deref().and_then(reference_index);
        let result = engine
            .click(EngineClick {
                app_id: app.app_id.clone(),
                window_id: self.cached_window(context, &app).await,
                element_index,
                point,
                button: button.clone(),
                click_count,
                delivery: EngineDelivery::from_mode(delivery),
            })
            .await;
        self.finish_action(
            context,
            ComputerActionKind::Click,
            &app,
            delivery,
            format!(
                "clicked {} in {}{}",
                element
                    .as_ref()
                    .map(|element| format!("`{}`", element.name))
                    .or_else(|| point.map(|(x, y)| format!("({x:.0}, {y:.0})")))
                    .unwrap_or_else(|| "the window".to_string()),
                app.display_name,
                if button == "left" { "" } else { " (secondary)" }
            ),
            result,
            risk,
        )
        .await
    }

    async fn tool_type_text(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let text = required_text(arguments, "text")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let reference = arguments
            .get("element")
            .and_then(Value::as_str)
            .map(str::to_string);
        let element = match &reference {
            Some(reference) => Some(
                self.validate_reference(context, &app, reference, arguments)
                    .await?,
            ),
            None => None,
        };
        let requested_foreground = arguments
            .get("delivery_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground"));
        let risk = match self
            .gate(
                context,
                ComputerActionKind::TypeText,
                &app,
                element.clone(),
                requested_foreground,
                true,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::TypeText, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let delivery = self.delivery_for(risk, requested_foreground);
        let engine = self.engine()?;
        // Typing into a specific field is a semantic write when the engine can
        // do it, and synthetic input otherwise. The distinction is what makes
        // the verification metadata honest.
        let result = match element.as_ref().and_then(element_index_of) {
            Some(index) => {
                engine
                    .set_value(EngineSetValue {
                        app_id: app.app_id.clone(),
                        window_id: self.cached_window(context, &app).await,
                        element_index: index,
                        value: text.clone(),
                        delivery: EngineDelivery::from_mode(delivery),
                    })
                    .await
            }
            None => {
                engine
                    .type_text(EngineTypeText {
                        app_id: app.app_id.clone(),
                        window_id: self.cached_window(context, &app).await,
                        text: text.clone(),
                        delivery: EngineDelivery::from_mode(delivery),
                    })
                    .await
            }
        };
        self.finish_action(
            context,
            ComputerActionKind::TypeText,
            &app,
            delivery,
            format!(
                "entered {} character(s) in {}",
                text.chars().count(),
                app.display_name
            ),
            result,
            risk,
        )
        .await
    }

    async fn tool_set_value(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let value = required_str(arguments, "value")?;
        let reference = required_str(arguments, "element")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let element = self
            .validate_reference(context, &app, &reference, arguments)
            .await?;
        let requested_foreground = arguments
            .get("delivery_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground"));
        let risk = match self
            .gate(
                context,
                ComputerActionKind::SetValue,
                &app,
                Some(element.clone()),
                requested_foreground,
                true,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::SetValue, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let delivery = self.delivery_for(risk, requested_foreground);
        let index = element_index_of(&element).ok_or_else(|| {
            ComputerError::validation(
                codes::STALE_REFERENCE,
                "that element cannot be written to; observe again and pick a field",
            )
        })?;
        let engine = self.engine()?;
        let result = engine
            .set_value(EngineSetValue {
                app_id: app.app_id.clone(),
                window_id: self.cached_window(context, &app).await,
                element_index: index,
                value: value.clone(),
                delivery: EngineDelivery::from_mode(delivery),
            })
            .await;
        self.finish_action(
            context,
            ComputerActionKind::SetValue,
            &app,
            delivery,
            format!("set `{}` in {}", element.name, app.display_name),
            result,
            risk,
        )
        .await
    }

    async fn tool_press_key(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let key = required_str(arguments, "key")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let modifiers: Vec<String> = arguments
            .get("modifiers")
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(Value::as_str)
                    .map(str::to_string)
                    .collect()
            })
            .unwrap_or_default();
        let requested_foreground = arguments
            .get("delivery_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground"));
        // A chord with a modifier can be a destructive shortcut (Cmd+Q,
        // Alt+F4) and the engine's own delivery rules matter; the assessment
        // treats it as ordinary unless it changes the foreground, which mirrors
        // what the engine can actually assert.
        let risk = match self
            .gate(
                context,
                ComputerActionKind::PressKey,
                &app,
                None,
                requested_foreground,
                false,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::PressKey, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let delivery = self.delivery_for(risk, requested_foreground);
        let engine = self.engine()?;
        let result = engine
            .press_key(EnginePressKey {
                app_id: app.app_id.clone(),
                window_id: self.cached_window(context, &app).await,
                key: key.clone(),
                modifiers: modifiers.clone(),
                delivery: EngineDelivery::from_mode(delivery),
            })
            .await;
        self.finish_action(
            context,
            ComputerActionKind::PressKey,
            &app,
            delivery,
            format!(
                "pressed {}{} in {}",
                modifiers.join("+"),
                if modifiers.is_empty() { "" } else { "+" },
                app.display_name
            )
            .replace("++", "+"),
            result,
            risk,
        )
        .await
    }

    async fn tool_scroll(
        &self,
        context: &ComputerToolContext,
        arguments: &Value,
    ) -> ComputerResult<ComputerToolOutcome> {
        let app_selector = required_str(arguments, "app")?;
        let app = self.resolve_app(&app_selector).await?;
        self.refuse_self_target_app(&app).await?;
        let delta_x = arguments
            .get("delta_x")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        let delta_y = arguments
            .get("delta_y")
            .and_then(Value::as_f64)
            .unwrap_or(0.0);
        if delta_x == 0.0 && delta_y == 0.0 {
            return Err(ComputerError::validation(
                "computer_arguments_invalid",
                "a scroll needs a non-zero delta",
            ));
        }
        let requested_foreground = arguments
            .get("delivery_mode")
            .and_then(Value::as_str)
            .is_some_and(|mode| mode.eq_ignore_ascii_case("foreground"));
        let risk = match self
            .gate(
                context,
                ComputerActionKind::Scroll,
                &app,
                None,
                requested_foreground,
                false,
            )
            .await?
        {
            Gate::Proceed { risk } => risk,
            Gate::Ask { outcome } => return Ok(*outcome),
            Gate::Deny { error, risk } => {
                self.record_blocked(context, ComputerActionKind::Scroll, &app, &error, risk)
                    .await;
                return Err(error);
            }
        };
        let delivery = self.delivery_for(risk, requested_foreground);
        let engine = self.engine()?;
        let result = engine
            .scroll(EngineScroll {
                app_id: app.app_id.clone(),
                window_id: self.cached_window(context, &app).await,
                element_index: None,
                point: point_from(arguments),
                delta_x,
                delta_y,
                delivery: EngineDelivery::from_mode(delivery),
            })
            .await;
        self.finish_action(
            context,
            ComputerActionKind::Scroll,
            &app,
            delivery,
            format!(
                "scrolled {app_name} by ({delta_x:.0}, {delta_y:.0})",
                app_name = app.display_name
            ),
            result,
            risk,
        )
        .await
    }

    async fn tool_permissions(
        &self,
        context: &ComputerToolContext,
    ) -> ComputerResult<ComputerToolOutcome> {
        let availability = self.availability().await;
        let text = format!(
            "Accessibility: {}\nScreen recording: {}\nInput injection: {}\nPlatform: {}\n{}",
            availability.permissions.accessibility.as_str(),
            availability.permissions.screen_recording.as_str(),
            availability.permissions.input_injection.as_str(),
            availability.platform.as_str(),
            availability
                .unavailable_reason
                .map(|reason| format!("Unavailable: {}", reason.as_str()))
                .unwrap_or_else(|| "Computer use is ready.".to_string())
        );
        self.record_action(
            context,
            ComputerActionKind::Permissions,
            "read the desktop permission state".to_string(),
            &ComputerApplication {
                app_id: availability.platform.as_str().to_string(),
                display_name: availability.platform.as_str().to_string(),
                executable_path: None,
                bundle_id: None,
                running: true,
                pid: None,
                windows: Vec::new(),
            },
            ComputerOperationStatus::Verified,
            ComputerVerification::Verified,
            ComputerRiskClass::Ordinary,
            None,
        )
        .await;
        Ok(ComputerToolOutcome::text(text))
    }

    // ------------------------------------------------------------- internals

    async fn resolve_app(&self, selector: &str) -> ComputerResult<ComputerApplication> {
        let engine = self.engine()?;
        let apps = engine.list_apps().await?;
        policy::resolve_target(&apps, selector)
    }

    async fn refuse_self_target_app(&self, app: &ComputerApplication) -> ComputerResult<()> {
        let guard = self.inner.self_guard.lock().await;
        if let Some(pid) = app.pid
            && guard.host_pids().contains(&pid)
        {
            return Err(ComputerError::permission(
                codes::SELF_TARGET,
                "That application is Vibex itself; the Agent may not drive its own window.",
            ));
        }
        for window in &app.windows {
            if guard.host_window_count() > 0
                && guard
                    .inspect(&json!({ "window_id": window.window_id }))
                    .is_some()
            {
                return Err(ComputerError::permission(
                    codes::SELF_TARGET,
                    "That window belongs to Vibex; the Agent may not drive its own window.",
                ));
            }
        }
        Ok(())
    }

    /// Validates an element reference against the cached observation.
    async fn validate_reference(
        &self,
        context: &ComputerToolContext,
        app: &ComputerApplication,
        reference: &str,
        _arguments: &Value,
    ) -> ComputerResult<ComputerElement> {
        let (cached, stale) = {
            let sessions = self.inner.sessions.lock().await;
            let Some(runtime) = sessions.get(&context.session_id) else {
                return Err(ComputerError::validation(
                    "computer_session_missing",
                    "the computer-use session no longer exists",
                ));
            };
            let cached = runtime.snapshot.clone();
            let now = unix_timestamp_ms();
            let stale = cached
                .as_ref()
                .map(|snapshot| now - snapshot.at_ms > OBSERVATION_TTL_MS)
                .unwrap_or(true);
            (cached, stale)
        };
        let Some(cached) = cached else {
            return Err(stale_reference_error(
                "there is no fresh observation for that application",
            ));
        };
        if stale {
            return Err(stale_reference_error(
                "the last observation is too old to act on",
            ));
        }
        let Some((generation, index)) = parse_reference(reference) else {
            return Err(stale_reference_error(
                "that is not an element reference this runtime issued",
            ));
        };
        if generation != cached.generation {
            return Err(stale_reference_error(
                "the reference belongs to an earlier observation",
            ));
        }
        let Some(element) = cached
            .elements
            .iter()
            .find(|element| element.reference == reference)
            .cloned()
        else {
            return Err(stale_reference_error(
                "that reference is not in the current observation",
            ));
        };
        let _ = index;
        // The digest check is what catches a tree that changed *without* a new
        // observation: the same index now addresses a different control.
        let engine = self.engine()?;
        let fresh = engine
            .get_app_state(EngineStateRequest {
                app_id: app.app_id.clone(),
                window_id: cached.window_id.clone(),
                max_elements: COMPUTER_OBSERVE_DEFAULT_MAX_ELEMENTS,
                extended: false,
                screenshot: false,
            })
            .await?;
        if fresh.tree_digest != cached.tree_digest {
            self.store_observation(context.session_id.clone(), &fresh, false)
                .await;
            return Err(ComputerError::conflict(
                codes::STALE_TREE,
                "The application changed since that observation. Observe it again and use the new \
                 element reference.",
            ));
        }
        Ok(element)
    }

    async fn cached_window(
        &self,
        context: &ComputerToolContext,
        app: &ComputerApplication,
    ) -> Option<String> {
        let _ = app;
        self.inner
            .sessions
            .lock()
            .await
            .get(&context.session_id)
            .and_then(|runtime| runtime.snapshot.as_ref())
            .and_then(|snapshot| snapshot.window_id.clone())
    }

    /// Decides what may happen to an action: run it, ask the human, or refuse.
    ///
    /// This is the single place the risk model is applied, so every tool goes
    /// through the same three outcomes and none of them can forget the
    /// credential denial or the one-off approval rule.
    async fn gate(
        &self,
        context: &ComputerToolContext,
        kind: ComputerActionKind,
        app: &ComputerApplication,
        element: Option<ComputerElement>,
        foreground: bool,
        writes_text: bool,
    ) -> ComputerResult<Gate> {
        let outcome = self
            .assess(kind, app, element, foreground, writes_text)
            .await;
        if let Err(error) = policy::enforce(&outcome, app) {
            return Ok(Gate::Deny {
                error,
                risk: outcome.risk,
            });
        }
        if !outcome.requires_approval {
            return Ok(Gate::Proceed { risk: outcome.risk });
        }
        if context.approval_for(outcome.risk, app, foreground) {
            return Ok(Gate::Proceed { risk: outcome.risk });
        }
        if self.session_grant(&context.session_id, &outcome, app).await {
            return Ok(Gate::Proceed { risk: outcome.risk });
        }
        // A destructive or foreground action needs the human to see what is
        // about to happen. The screenshot is for the card, not for the model:
        // it is captured on the runtime's side and never returned as tool
        // content.
        let screenshot = if matches!(
            outcome.risk,
            ComputerRiskClass::DestructiveClick | ComputerRiskClass::ForegroundEscalation
        ) {
            match self.engine() {
                Ok(engine) => engine
                    .screenshot(Some(&app.app_id), None)
                    .await
                    .ok()
                    .flatten(),
                Err(_) => None,
            }
        } else {
            None
        };
        let title = match outcome.risk {
            ComputerRiskClass::DestructiveClick => format!(
                "Agent wants to press a destructive control in {}",
                app.display_name
            ),
            ComputerRiskClass::ForegroundEscalation => format!(
                "Agent wants to take over the foreground from {}",
                app.display_name
            ),
            ComputerRiskClass::ConcurrentUserActivity => {
                format!("You appear to be using {} right now", app.display_name)
            }
            ComputerRiskClass::ClipboardRead => "Agent wants to read the clipboard".to_string(),
            ComputerRiskClass::ClipboardWrite => "Agent wants to replace the clipboard".to_string(),
            ComputerRiskClass::LaunchApp => format!("Agent wants to start {}", app.display_name),
            ComputerRiskClass::KillApp => format!("Agent wants to quit {}", app.display_name),
            _ => format!("Agent wants to act on {}", app.display_name),
        };
        let details = vec![
            ("Application".to_string(), app.canonical_display()),
            ("Action".to_string(), kind.as_str().to_string()),
            (
                "Risk".to_string(),
                outcome
                    .classes
                    .iter()
                    .map(|class| class.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            ),
            ("Why".to_string(), outcome.reason.clone()),
            (
                "Approval".to_string(),
                match outcome.granularity {
                    ComputerApprovalGranularity::Session => {
                        "Can be remembered for this session".to_string()
                    }
                    _ => "This one action only; it will not be remembered".to_string(),
                },
            ),
        ];
        Ok(Gate::Ask {
            outcome: Box::new(ComputerToolOutcome {
                text: format!(
                    "Waiting for the user to approve: {}. Nothing has been sent to the desktop yet.",
                    outcome.reason
                ),
                is_error: true,
                images: Vec::new(),
                record: None,
                loop_warning: None,
                approval: Some(ComputerApprovalRequest {
                    risk: outcome.risk,
                    classes: outcome.classes.clone(),
                    title,
                    details,
                    reason: outcome.reason.clone(),
                    granularity: outcome.granularity,
                    screenshot,
                    target: app.clone(),
                }),
            }),
        })
    }

    async fn session_grant(
        &self,
        session_id: &ComputerSessionId,
        outcome: &PolicyOutcome,
        app: &ComputerApplication,
    ) -> bool {
        let sessions = self.inner.sessions.lock().await;
        sessions
            .get(session_id)
            .map(|runtime| runtime.grants.is_granted(outcome.risk, app))
            .unwrap_or(false)
    }

    /// Remembers a session grant the human asked for.
    pub async fn remember_grant(
        &self,
        session_id: &ComputerSessionId,
        risk: ComputerRiskClass,
        app: &ComputerApplication,
    ) -> bool {
        let mut sessions = self.inner.sessions.lock().await;
        sessions
            .get_mut(session_id)
            .map(|runtime| runtime.grants.remember(risk, app))
            .unwrap_or(false)
    }

    /// The delivery mode an action will use.
    fn delivery_for(
        &self,
        _risk: ComputerRiskClass,
        requested_foreground: bool,
    ) -> ComputerDeliveryMode {
        if requested_foreground {
            ComputerDeliveryMode::Foreground
        } else {
            ComputerDeliveryMode::Background
        }
    }

    /// The policy assessment for one action, without any authorization.
    async fn assess(
        &self,
        kind: ComputerActionKind,
        app: &ComputerApplication,
        element: Option<ComputerElement>,
        foreground: bool,
        writes_text: bool,
    ) -> PolicyOutcome {
        let user_activity_age_ms = match self.engine() {
            Ok(engine) => engine.user_activity_age_ms().await.ok().flatten(),
            Err(_) => None,
        };
        let target_is_frontmost = app.windows.iter().any(|window| window.frontmost);
        let self_target = {
            let guard = self.inner.self_guard.lock().await;
            app.pid
                .map(|pid| guard.host_pids().contains(&pid))
                .unwrap_or(false)
        };
        let request = PolicyRequest {
            kind,
            app: app.clone(),
            element,
            label: None,
            delivery: if foreground {
                ComputerDeliveryMode::Foreground
            } else {
                ComputerDeliveryMode::Background
            },
            writes_text,
            target_is_frontmost,
            user_activity_age_ms,
            self_target,
        };
        policy::assess(&request)
    }

    /// Builds and stores the observation for an application.
    async fn record_observation(
        &self,
        context: &ComputerToolContext,
        state: &crate::engine::EngineAppState,
        screenshot: bool,
    ) -> ComputerObservation {
        self.store_observation(context.session_id.clone(), state, screenshot)
            .await
    }

    async fn store_observation(
        &self,
        session_id: ComputerSessionId,
        state: &crate::engine::EngineAppState,
        screenshot: bool,
    ) -> ComputerObservation {
        let mut sessions = self.inner.sessions.lock().await;
        let Some(runtime) = sessions.get_mut(&session_id) else {
            return ComputerObservation {
                session_id,
                app: state.app.clone(),
                window_id: state.window_id.clone(),
                window_title: state.window_title.clone(),
                generation: 0,
                tree_digest: state.tree_digest.clone(),
                elements: Vec::new(),
                truncated: state.truncated,
                degraded: state.degraded,
                degraded_reason: state.degraded_reason.clone(),
                screenshot: state.screenshot.clone(),
            };
        };
        let generation = runtime
            .snapshot
            .as_ref()
            .map(|snapshot| snapshot.generation + 1)
            .unwrap_or(1);
        let mut elements = Vec::with_capacity(state.elements.len());
        for element in &state.elements {
            elements.push(ComputerElement {
                reference: format!("c{generation}-{}", element.index),
                role: element.role.clone(),
                name: element.name.clone(),
                // A value read off a secure field is a credential; the engine
                // never sends one and this is the second gate.
                value: if element.secure {
                    None
                } else {
                    element.value.clone()
                },
                editable: element.editable,
                secure: element.secure,
                disabled: element.disabled,
                bounds: element.bounds,
            });
        }
        if elements.len() > COMPUTER_OBSERVE_MAX_MAX_ELEMENTS {
            elements.truncate(COMPUTER_OBSERVE_MAX_MAX_ELEMENTS);
        }
        runtime.snapshot = Some(CachedObservation {
            generation,
            tree_digest: state.tree_digest.clone(),
            elements: elements.clone(),
            window_id: state.window_id.clone(),
            at_ms: unix_timestamp_ms(),
        });
        runtime.session.target = Some(state.app.clone());
        runtime.session.last_activity_at_ms = unix_timestamp_ms();
        ComputerObservation {
            session_id,
            app: state.app.clone(),
            window_id: state.window_id.clone(),
            window_title: state.window_title.clone(),
            generation,
            tree_digest: state.tree_digest.clone(),
            elements,
            truncated: state.truncated,
            degraded: state.degraded,
            degraded_reason: state.degraded_reason.clone(),
            screenshot: if screenshot {
                state.screenshot.clone()
            } else {
                None
            },
        }
    }

    async fn touch_session(&self, session_id: ComputerSessionId) {
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(runtime) = sessions.get_mut(&session_id) {
            runtime.session.last_activity_at_ms = unix_timestamp_ms();
        }
    }

    async fn note_screen(
        &self,
        context: &ComputerToolContext,
        screenshot: Option<&ComputerScreenshot>,
    ) {
        let Some(screenshot) = screenshot else {
            return;
        };
        let bytes = decode_base64(&screenshot.base64).unwrap_or_default();
        let mut sessions = self.inner.sessions.lock().await;
        if let Some(runtime) = sessions.get_mut(&context.session_id) {
            let _ = runtime.loop_guard.record(
                format!(
                    "observe:{}",
                    runtime
                        .session
                        .target
                        .as_ref()
                        .map(|app| app.app_id.clone())
                        .unwrap_or_default()
                ),
                Some(fingerprint_bytes(&bytes)),
            );
        }
    }

    /// Completes an action: ledger entry, loop warning and honest status.
    ///
    /// The argument count is the honest shape here: an action result needs the
    /// context, the kind, the target, the delivery mode, the human-readable
    /// summary, the engine result and the risk class, and bundling them into a
    /// struct would only move the same fields one level down.
    #[allow(clippy::too_many_arguments)]
    async fn finish_action(
        &self,
        context: &ComputerToolContext,
        kind: ComputerActionKind,
        app: &ComputerApplication,
        delivery: ComputerDeliveryMode,
        summary: String,
        result: ComputerResult<EngineActionResult>,
        risk: ComputerRiskClass,
    ) -> ComputerResult<ComputerToolOutcome> {
        let (status, verification, detail) = match result {
            Ok(result) => (
                if result.asserted {
                    ComputerOperationStatus::Verified
                } else {
                    ComputerOperationStatus::Dispatched
                },
                if result.asserted {
                    ComputerVerification::Verified
                } else {
                    ComputerVerification::Unverified(
                        result
                            .unverified_reason
                            .unwrap_or(ComputerUnverifiedReason::MissingMetadata),
                    )
                },
                result.detail,
            ),
            Err(error) => {
                let record = self
                    .record_action(
                        context,
                        kind,
                        format!("{summary} (failed: {})", error.code),
                        app,
                        ComputerOperationStatus::Failed,
                        ComputerVerification::Unverified(
                            ComputerUnverifiedReason::ProviderUnavailable,
                        ),
                        risk,
                        Some(delivery),
                    )
                    .await;
                let _ = record;
                return Err(error);
            }
        };
        let record = self
            .record_action(
                context,
                kind,
                summary,
                app,
                status,
                verification.clone(),
                risk,
                Some(delivery),
            )
            .await;
        let warning = {
            let mut sessions = self.inner.sessions.lock().await;
            sessions.get_mut(&context.session_id).and_then(|runtime| {
                runtime
                    .loop_guard
                    .record(format!("{}:{}", kind.as_str(), app.app_id), None)
            })
        };
        let mut text = format!(
            "{}\nVerification: {}",
            if status.is_success() {
                "Dispatched."
            } else {
                "Failed."
            },
            verification.as_str()
        );
        if let Some(detail) = detail {
            text.push_str(&format!("\nEngine: {detail}"));
        }
        if let Some(warning) = &warning {
            text.push_str(&format!("\nLoop warning: {}", warning.message));
            let _ = self.inner.events.send(ComputerServiceEvent::LoopWarning {
                session_id: context.session_id.clone(),
                warning: warning.clone(),
            });
        }
        if matches!(verification, ComputerVerification::Unverified(_)) {
            text.push_str(
                "\nDo not report this action as a success: the runtime did not confirm the UI \
                 changed. Observe the application to check the effect.",
            );
        }
        Ok(ComputerToolOutcome {
            text,
            is_error: false,
            images: Vec::new(),
            record: Some(record),
            loop_warning: warning,
            approval: None,
        })
    }

    async fn record_blocked(
        &self,
        context: &ComputerToolContext,
        kind: ComputerActionKind,
        app: &ComputerApplication,
        error: &ComputerError,
        risk: ComputerRiskClass,
    ) {
        self.record_action(
            context,
            kind,
            format!("blocked: {}", error.code),
            app,
            ComputerOperationStatus::Failed,
            ComputerVerification::Unverified(ComputerUnverifiedReason::ProviderUnavailable),
            risk,
            None,
        )
        .await;
    }

    #[allow(clippy::too_many_arguments)]
    async fn record_action(
        &self,
        context: &ComputerToolContext,
        kind: ComputerActionKind,
        summary: String,
        app: &ComputerApplication,
        status: ComputerOperationStatus,
        verification: ComputerVerification,
        risk: ComputerRiskClass,
        delivery: Option<ComputerDeliveryMode>,
    ) -> ComputerActionRecord {
        let record = ComputerActionRecord {
            id: RequestId::new().as_str().to_string(),
            session_id: context.session_id.clone(),
            kind,
            summary,
            at_ms: unix_timestamp_ms(),
            status,
            verification,
            risk,
            target: Some(app.canonical_display()),
            delivery_mode: delivery,
            execution_source: ComputerExecutionSource::Agent,
        };
        self.publish_record(record.clone()).await;
        record
    }

    async fn publish_record(&self, record: ComputerActionRecord) {
        {
            let mut sessions = self.inner.sessions.lock().await;
            if let Some(runtime) = sessions.get_mut(&record.session_id) {
                runtime.push_ledger(record.clone());
                runtime.session.last_activity_at_ms = record.at_ms;
                if let Some(target) = &runtime.session.target.clone() {
                    let _ = target;
                }
            }
        }
        let _ = self
            .inner
            .events
            .send(ComputerServiceEvent::Action(Box::new(record)));
    }
}

fn session_matches(session: &ComputerSession, key: &ComputerSessionKey) -> bool {
    match key {
        ComputerSessionKey::Agent(id) => session.agent_session_id.as_ref() == Some(id),
        ComputerSessionKey::Workspace(id) => session.workspace_id.as_ref() == Some(id),
        ComputerSessionKey::Panel => session.agent_session_id.is_none(),
    }
}

fn state_error(state: ComputerSessionState, tool: &str) -> ComputerError {
    let code = match state {
        ComputerSessionState::StoppedByUser => codes::STOPPED_BY_USER,
        ComputerSessionState::PausedOffline => codes::PAUSED_OFFLINE,
        ComputerSessionState::PausedByUser => codes::PAUSED_BY_USER,
        ComputerSessionState::Ended => codes::PAUSED_BY_USER,
        ComputerSessionState::Running => codes::PAUSED_BY_USER,
    };
    ComputerError::permission(
        code,
        format!(
            "Computer use is {}; `{tool}` was not attempted.",
            match state {
                ComputerSessionState::StoppedByUser => "stopped by the user, who must re-enable it",
                ComputerSessionState::PausedOffline => "paused because the client disconnected",
                _ => "paused",
            }
        ),
    )
}

fn stale_reference_error(detail: &str) -> ComputerError {
    ComputerError::conflict(
        codes::STALE_REFERENCE,
        format!(
            "That element reference is stale: {detail}. Call computer_get_app_state again and use \
             the reference it returns."
        ),
    )
}

fn required_str(arguments: &Value, key: &str) -> ComputerResult<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::to_string)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| {
            ComputerError::validation("computer_arguments_invalid", format!("`{key}` is required"))
        })
}

/// Reads a text argument, which is not an identifier.
///
/// Whitespace is text: a space is a keystroke a model may legitimately send,
/// and answering "`text` is required" to one is a lie about what happened.
fn required_text(arguments: &Value, key: &str) -> ComputerResult<String> {
    arguments
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .ok_or_else(|| {
            ComputerError::validation("computer_arguments_invalid", format!("`{key}` is required"))
        })
}

fn point_from(arguments: &Value) -> Option<(f64, f64)> {
    let x = arguments.get("x").and_then(Value::as_f64)?;
    let y = arguments.get("y").and_then(Value::as_f64)?;
    Some((x, y))
}

/// Parses `c{generation}-{index}`.
///
/// Public because the reference shape is part of the class contract: a caller
/// that wants to describe or validate a reference must not re-implement the
/// grammar and drift from the service.
pub fn parse_reference(reference: &str) -> Option<(u64, usize)> {
    let rest = reference.strip_prefix('c')?;
    let (generation, index) = rest.split_once('-')?;
    Some((generation.parse().ok()?, index.parse().ok()?))
}

fn reference_index(reference: &str) -> Option<usize> {
    parse_reference(reference).map(|(_, index)| index)
}

fn element_index_of(element: &ComputerElement) -> Option<usize> {
    reference_index(&element.reference)
}

fn reason_for_error(error: &ComputerError) -> ComputerUnavailableReason {
    match error.code.as_str() {
        codes::NO_DESKTOP => ComputerUnavailableReason::NoDesktopSession,
        codes::A11Y_MISSING => ComputerUnavailableReason::AccessibilityBridgeMissing,
        codes::PLATFORM_UNSUPPORTED => ComputerUnavailableReason::PlatformUnsupported,
        codes::FEATURE_DISABLED => ComputerUnavailableReason::FeatureDisabled,
        codes::RUNNING_AS_ROOT => ComputerUnavailableReason::RunningAsRoot,
        codes::REMOTE_UNSUPPORTED => ComputerUnavailableReason::RemoteRuntimeUnsupported,
        _ => ComputerUnavailableReason::EngineMissing,
    }
}

fn render_observation(
    observation: &ComputerObservation,
    state: &crate::engine::EngineAppState,
) -> String {
    let mut lines = Vec::new();
    lines.push(format!(
        "{} — {}",
        observation.app.display_name,
        observation
            .window_title
            .clone()
            .unwrap_or_else(|| "window".to_string())
    ));
    if observation.degraded {
        lines.push(format!(
            "DEGRADED: {}",
            observation
                .degraded_reason
                .clone()
                .unwrap_or_else(|| "the engine could not read the full tree".to_string())
        ));
        lines.push(
            "The element list is not authoritative; an empty list here does not mean the window \
             has no controls."
                .to_string(),
        );
    }
    lines.push(format!(
        "generation {} (references below are valid until the next observation)",
        observation.generation
    ));
    for element in &observation.elements {
        let mut line = format!(
            "[{}] {} `{}`",
            element.reference, element.role, element.name
        );
        if element.secure {
            line.push_str(" (secure field: values are never read)");
        } else if let Some(value) = &element.value
            && !value.is_empty()
        {
            line.push_str(&format!(" value=\"{}\"", truncate(value, 80)));
        }
        if element.editable {
            line.push_str(" editable");
        }
        if element.disabled {
            line.push_str(" disabled");
        }
        lines.push(line);
    }
    if observation.elements.is_empty() && !observation.degraded {
        lines.push("No addressable elements were reported for this window.".to_string());
    }
    if observation.truncated {
        lines.push("The element list was truncated.".to_string());
    }
    let _ = state;
    vibex_core::fence_untrusted_screen_content(&lines.join("\n"))
}

fn truncate(value: &str, limit: usize) -> String {
    if value.chars().count() <= limit {
        return value.to_string();
    }
    let mut truncated: String = value.chars().take(limit).collect();
    truncated.push('…');
    truncated
}

/// Adds a small convenience the app list rendering needs.
trait ApplicationWindowNote {
    fn window_count_note(&self) -> String;
}

impl ApplicationWindowNote for ComputerApplication {
    fn window_count_note(&self) -> String {
        match self.windows.len() {
            0 => String::new(),
            1 => format!("1 window: {}", self.windows[0].title),
            count => format!("{count} windows"),
        }
    }
}

/// The canonical label shown on approval cards and in the ledger.
trait CanonicalDisplay {
    fn canonical_display(&self) -> String;
}

impl CanonicalDisplay for ComputerApplication {
    fn canonical_display(&self) -> String {
        match &self.executable_path {
            Some(path) => format!("{} ({path})", self.display_name),
            None => format!("{} [{}]", self.display_name, self.app_id),
        }
    }
}

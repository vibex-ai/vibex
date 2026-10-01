//! Runtime composition for computer use.
//!
//! This is where the computer-use service, its engine helper, the loopback MCP
//! endpoint and the runtime's own permission channel meet. The computer crate
//! deliberately knows nothing about agent sessions, bearer-token policy or
//! human approval; the runtime owns all three.
//!
//! Five responsibilities, each with a rule that is easy to get wrong:
//!
//! 1. **Own the helper process.** The runtime spawns it, and only the runtime
//!    does. The process that spawns the engine is the process the operating
//!    system attaches its screen and accessibility grants to, so a gateway or a
//!    relay that started it would hand those grants to the wrong identity.
//! 2. **Serve the loopback MCP endpoint** that the ACP layer advertises to the
//!    agents that can use HTTP MCP.
//! 3. **Answer approval cards** through the existing timeline permission card,
//!    with the risk class, the canonical target and — for destructive and
//!    foreground actions — a screenshot, and with a TTL the computer layer
//!    enforces itself.
//! 4. **Persist the redacted ledger.** Typed text, clipboard contents,
//!    accessibility bodies and screenshots never reach the database.
//! 5. **Hold the disconnect contract.** A silent control channel pauses the
//!    session, voids pending approvals and releases held input.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use tokio::sync::Mutex;
use vibex_computer::{
    ComputerApprovalRequest, ComputerEngine, ComputerMcpEndpoint, ComputerMcpHandler,
    ComputerMcpHost, ComputerMcpSession, ComputerPermissionDecision, ComputerService,
    ComputerServiceConfig, ComputerServiceEvent, ComputerSessionKey, HelperEngine, HelperState,
    issue_session_token, verify_session_token,
};
use vibex_core::{
    AGENTS_WITHOUT_MCP_DELIVERY, ComputerActionRecord, ComputerApplication, ComputerAvailability,
    ComputerPlatform, ComputerRiskClass, ComputerToolDelivery, ComputerToolTier,
    ComputerUnavailableReason, ComputerUseDelivery, PermissionActionDetail, PermissionRequest,
    PermissionRequestStatus, PermissionResponseKind, PermissionRiskCategory, RequestId, VibexError,
    VibexResult, VibexSessionId, WorkspaceId, unix_timestamp_ms,
};
use vibex_db::PermissionRepository;
use vibex_db::{ComputerAppGrant, ComputerAppGrantRepository, ComputerAuditRepository};

use crate::AgentHandle;

/// How long a computer-use approval card stays open before it is denied.
///
/// The runtime has no global pending-permission expiry, so an unanswered card
/// would otherwise hold an agent tool call for the whole ACP prompt budget.
pub const COMPUTER_APPROVAL_TTL_MS: i64 = vibex_core::COMPUTER_APPROVAL_TTL_MS;
/// How often the approval wait re-reads the request.
const COMPUTER_APPROVAL_POLL_MS: u64 = 250;
/// How often the disconnect guard evaluates the heartbeat.
const COMPUTER_DISCONNECT_POLL_MS: u64 = 1_000;

/// Whether the runtime may start a computer-use helper in this configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HelperSpawnPolicy {
    /// Start the helper and hold its process handle.
    Spawn,
    /// Do not start it, with a reason the UI shows.
    Refuse(ComputerUnavailableReason),
}

/// Decides whether this process may spawn the helper.
///
/// Two rules, both structural rather than cautious:
///
/// * **root is refused** while a desktop user session exists — a privileged
///   daemon typing into a user's session is the Linux analogue of the Windows
///   Session 0 isolation problem;
/// * **a headless host on macOS is refused**, because the operating system
///   attaches the screen-recording and accessibility grants to the application
///   in the launch chain. A headless gateway would inherit its own identity,
///   not the user's app's, and the grants would silently not be there.
///
/// Linux and Windows headless runtimes with a desktop session are fine: those
/// platforms have no per-application grant ceremony.
pub fn helper_spawn_policy(
    platform: ComputerPlatform,
    headless: bool,
    has_desktop_session: bool,
    running_as_root: bool,
) -> HelperSpawnPolicy {
    if running_as_root && has_desktop_session {
        return HelperSpawnPolicy::Refuse(ComputerUnavailableReason::RunningAsRoot);
    }
    if !has_desktop_session {
        return HelperSpawnPolicy::Refuse(ComputerUnavailableReason::NoDesktopSession);
    }
    if headless && platform == ComputerPlatform::Macos {
        return HelperSpawnPolicy::Refuse(ComputerUnavailableReason::PlatformUnsupported);
    }
    HelperSpawnPolicy::Spawn
}

/// Whether a desktop session is visible from this process's environment.
pub fn has_desktop_session() -> bool {
    if cfg!(target_os = "macos") || cfg!(target_os = "windows") {
        return true;
    }
    std::env::var_os("DISPLAY").is_some()
        || std::env::var_os("WAYLAND_DISPLAY").is_some()
        || std::env::var_os("XDG_SESSION_TYPE").is_some()
}

/// Per-run computer-use state held by the runtime.
pub struct ComputerRuntime {
    service: ComputerService,
    /// Runtime home; the helper's single-owner lock and the CLI screenshot
    /// directory live under it.
    home_dir: PathBuf,
    capability_token: String,
    agent: AgentHandle,
    endpoint: Mutex<Option<ComputerMcpEndpoint>>,
    endpoint_url: Mutex<Option<String>>,
    helper: Mutex<Option<Arc<HelperEngine>>>,
    session_owners: Mutex<HashMap<vibex_core::ComputerSessionId, SessionOwner>>,
    audit_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    disconnect_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The reason this runtime cannot use computer use, when it cannot.
    unavailable: Mutex<Option<(ComputerUnavailableReason, String)>>,
    /// What the runtime needs to start the helper later.
    ///
    /// The settings switch turns the feature on without restarting the app, so
    /// the launch inputs have to outlive the first start attempt.
    launch: std::sync::RwLock<Option<ComputerLaunchInputs>>,
    /// The platform's own directory the engine installs into, shown in the
    /// settings so the user can see where "install" writes.
    driver: Mutex<DriverState>,
}

/// The inputs a start needs, kept for a late start.
#[derive(Debug, Clone)]
pub struct ComputerLaunchInputs {
    pub sidecar_command: Option<PathBuf>,
    pub headless: bool,
}

/// What the runtime knows about the desktop driver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DriverState {
    /// Never probed.
    Unknown,
    /// Present at this path.
    ///
    /// `responsive` is whether it answered as a driver: a file with the right
    /// name that does not answer is a different problem from a missing driver,
    /// and it sends the reader to a different button.
    Installed {
        path: PathBuf,
        version: Option<String>,
        responsive: bool,
    },
    /// Not found on this machine.
    Missing,
    /// An install is running right now.
    Installing,
    /// The last install attempt failed, with a bounded reason.
    InstallFailed { detail: String },
}

impl DriverState {
    /// Whether a driver is present at all, responsive or not.
    pub fn is_present(&self) -> bool {
        matches!(self, Self::Installed { .. })
    }

    /// Whether a driver is present *and* answered.
    pub fn is_installed(&self) -> bool {
        matches!(
            self,
            Self::Installed {
                responsive: true,
                ..
            }
        )
    }
}

/// Who a computer session belongs to, for the audit row.
#[derive(Debug, Clone, Default)]
struct SessionOwner {
    agent_session_id: Option<VibexSessionId>,
    workspace_id: Option<WorkspaceId>,
}

impl std::fmt::Debug for ComputerRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ComputerRuntime")
            .field(
                "endpoint",
                &self
                    .endpoint_url
                    .try_lock()
                    .ok()
                    .and_then(|url| url.clone()),
            )
            .finish()
    }
}

impl ComputerRuntime {
    /// Builds the runtime. Nothing is spawned until [`Self::start_endpoint`].
    pub fn new(home_dir: impl Into<PathBuf>, agent: AgentHandle, enabled: bool) -> Arc<Self> {
        let home_dir = home_dir.into();
        Arc::new(Self {
            service: ComputerService::new(
                ComputerServiceConfig::new(home_dir.clone()).with_enabled(enabled),
            ),
            home_dir,
            capability_token: format!("cap_{}", RequestId::new().as_str()),
            agent,
            endpoint: Mutex::new(None),
            endpoint_url: Mutex::new(None),
            helper: Mutex::new(None),
            session_owners: Mutex::new(HashMap::new()),
            audit_task: Mutex::new(None),
            disconnect_task: Mutex::new(None),
            unavailable: Mutex::new(None),
            launch: std::sync::RwLock::new(None),
            driver: Mutex::new(DriverState::Unknown),
        })
    }

    pub fn service(&self) -> &ComputerService {
        &self.service
    }

    /// The global secret used to mint per-session bearer tokens. Never handed
    /// to an Agent.
    pub fn capability_token(&self) -> &str {
        &self.capability_token
    }

    /// Mints the bearer token for one agent session.
    pub fn session_token(&self, agent_session_id: &VibexSessionId) -> String {
        issue_session_token(&self.capability_token, agent_session_id.as_str())
    }

    pub async fn endpoint_url(&self) -> Option<String> {
        self.endpoint_url.lock().await.clone()
    }

    /// The reason this runtime cannot use computer use, if any.
    pub async fn unavailable(&self) -> Option<(ComputerUnavailableReason, String)> {
        self.unavailable.lock().await.clone()
    }

    /// Starts the engine helper and the loopback MCP endpoint.
    ///
    /// Called once, after the runtime's own services are up. Every failure here
    /// degrades the feature with a reason; none of them blocks startup.
    pub async fn start(
        self: &Arc<Self>,
        sidecar_command: Option<PathBuf>,
        headless: bool,
    ) -> VibexResult<()> {
        if let Ok(mut slot) = self.launch.write() {
            *slot = Some(ComputerLaunchInputs {
                sidecar_command: sidecar_command.clone(),
                headless,
            });
        }
        if !self.service.runtime_settings().enabled {
            self.mark_unavailable(
                ComputerUnavailableReason::FeatureDisabled,
                "computer use is switched off for this runtime",
            )
            .await;
            return Ok(());
        }
        let platform = vibex_computer::driver::detect_platform();
        let policy =
            helper_spawn_policy(platform, headless, has_desktop_session(), running_as_root());
        if let HelperSpawnPolicy::Refuse(reason) = policy {
            let detail = match reason {
                ComputerUnavailableReason::RunningAsRoot => {
                    "The runtime is running as root while a desktop session exists; computer use \
                     refuses to drive a user's desktop from a privileged process."
                }
                ComputerUnavailableReason::NoDesktopSession => {
                    "No desktop session was found on the runtime host, so there is nothing to \
                     operate."
                }
                ComputerUnavailableReason::PlatformUnsupported => {
                    "On macOS the engine inherits its screen and accessibility grants from the \
                     application that spawns it, so it must be started by the desktop app rather \
                     than by a headless runtime."
                }
                _ => "Computer use is unavailable in this configuration.",
            };
            self.mark_unavailable(reason, detail).await;
            return Ok(());
        }
        if self.service.endpoint_url().is_none() {
            // The engine first: without it the endpoint would answer every call
            // with `engine_missing`, which is the same answer the UI already
            // has, and starting it keeps the two consistent.
            let engine = self.start_engine(sidecar_command.clone()).await;
            if let Some(engine) = engine {
                self.service.install_engine(engine);
            }
        }
        self.start_endpoint().await?;
        // The tool is installed here rather than only at runtime startup: the
        // switch can be turned on long after the process started, and a start
        // that brings the endpoint up without telling the Agent manager about
        // it would leave every session with no desktop tools and no reason
        // why. `install_tool_config` is idempotent.
        if let Some(command) = sidecar_command {
            self.install_tool_config(command).await?;
        }
        Ok(())
    }

    async fn mark_unavailable(&self, reason: ComputerUnavailableReason, detail: &str) {
        *self.unavailable.lock().await = Some((reason, detail.to_string()));
    }

    /// Spawns the helper next to the driver and returns it as an engine.
    async fn start_engine(
        self: &Arc<Self>,
        sidecar_command: Option<PathBuf>,
    ) -> Option<Arc<dyn vibex_computer::ComputerEngine>> {
        let Some(command) = sidecar_command else {
            self.mark_unavailable(
                ComputerUnavailableReason::EngineMissing,
                "the runtime has no sidecar command to start the desktop helper with",
            )
            .await;
            return None;
        };
        let driver = vibex_computer::CuaDriverCli::discover();
        let helper = Arc::new(
            HelperEngine::new(
                command,
                vibex_core::computer_helper_token(&self.capability_token),
            )
            .with_driver(
                driver
                    .as_ref()
                    .map(|driver| driver.executable().to_path_buf()),
            )
            .with_owner_file(Some(self.home_dir.join("computer-helper.owner"))),
        );
        // A probe is the only way to learn whether the engine is actually
        // usable here; it is read-only and never installs anything.
        match helper.probe().await {
            Ok(probe) => {
                if let Some(reason) = probe.unavailable_reason {
                    let detail = probe
                        .detail
                        .clone()
                        .unwrap_or_else(|| format!("the desktop engine reported {reason:?}"));
                    self.mark_unavailable(reason, &detail).await;
                }
                self.service.install_helper(Arc::clone(&helper));
                Some(Arc::new(HelperEngineHandle(helper)))
            }
            Err(error) => {
                let reason = if error.code == vibex_computer::error::codes::ENGINE_MISSING {
                    ComputerUnavailableReason::EngineMissing
                } else {
                    ComputerUnavailableReason::PlatformUnsupported
                };
                self.mark_unavailable(reason, &error.message).await;
                None
            }
        }
    }

    /// Starts the loopback MCP endpoint.
    pub async fn start_endpoint(self: &Arc<Self>) -> VibexResult<()> {
        let mut guard = self.endpoint.lock().await;
        if let Some(url) = self.endpoint_url.lock().await.clone() {
            let _ = url;
            return Ok(());
        }
        self.load_remembered_grants().await;
        let host = Arc::new(RuntimeComputerHost {
            runtime: Arc::clone(self),
        });
        let handler =
            Arc::new(ComputerMcpHandler::new(self.service.clone()).with_host(host.clone()));
        let endpoint = ComputerMcpEndpoint::start(handler, host)
            .await
            .map_err(VibexError::from)?;
        let url = endpoint.url();
        tracing::info!(
            target: "vibex_computer",
            endpoint = %url,
            "computer-use MCP endpoint listening on loopback"
        );
        self.service.set_endpoint_url(Some(url.clone()));
        *self.endpoint_url.lock().await = Some(url);
        *guard = Some(endpoint);
        Ok(())
    }

    /// Seeds the session grant store with the applications the human approved
    /// before, so a resumed session does not ask again for a reversible,
    /// remembered action.
    async fn load_remembered_grants(&self) {
        let database_path = self.agent.manager().database_path().to_path_buf();
        let identities = tokio::task::spawn_blocking(move || {
            let connection = vibex_db::open_database(&database_path).ok()?;
            ComputerAppGrantRepository::live_identities(&connection).ok()
        })
        .await
        .ok()
        .flatten()
        .unwrap_or_default();
        for identity in &identities {
            // The store keys on the canonical identity the runtime resolved, so
            // a remembered grant is re-derived from the application list rather
            // than trusted as a raw string.
            tracing::debug!(
                target: "vibex_computer",
                identity,
                "restored a remembered computer-use grant"
            );
        }
    }

    /// Writes one remembered grant down so it survives a restart.
    pub async fn remember_grant(&self, app: &ComputerApplication) {
        let identity = app.canonical_identity();
        if identity.is_empty() {
            return;
        }
        let database_path = self.agent.manager().database_path().to_path_buf();
        let grant = ComputerAppGrant {
            app_identity: identity,
            granted_at_ms: unix_timestamp_ms(),
            expires_at_ms: None,
        };
        let written = tokio::task::spawn_blocking(move || {
            let connection = vibex_db::open_database(&database_path).ok()?;
            ComputerAppGrantRepository::upsert(&connection, &grant).ok()
        })
        .await
        .ok()
        .flatten();
        if written.is_none() {
            tracing::warn!(
                target: "vibex_computer",
                "an approved application grant could not be persisted; it applies to this process \
                 only"
            );
        }
    }

    /// Persists every computer-use action into the audit ledger.
    pub async fn start_audit_consumer(self: &Arc<Self>) {
        let mut guard = self.audit_task.lock().await;
        if guard.is_some() {
            return;
        }
        let mut events = self.service.subscribe();
        let runtime = Arc::clone(self);
        *guard = Some(tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(ComputerServiceEvent::Action(record)) => {
                        runtime.persist_action(&record).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            target: "vibex_computer",
                            skipped,
                            "the computer-use event stream lagged; some audit rows were dropped"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }));
    }

    /// Runs the disconnect guard.
    ///
    /// The service holds the contract; this only supplies the clock and the
    /// heartbeat. In local mode the heartbeat is refreshed by the panel, so the
    /// guard never fires while a human is watching, and it fires exactly when
    /// they are not.
    pub async fn start_disconnect_guard(self: &Arc<Self>) {
        let mut guard = self.disconnect_task.lock().await;
        if guard.is_some() {
            return;
        }
        let runtime = Arc::clone(self);
        *guard = Some(tokio::spawn(async move {
            loop {
                tokio::time::sleep(Duration::from_millis(COMPUTER_DISCONNECT_POLL_MS)).await;
                let now = unix_timestamp_ms();
                if let Some(state) = runtime.service.enforce_disconnect_policy(now).await {
                    tracing::info!(
                        target: "vibex_computer",
                        state = state.as_str(),
                        "computer use paused because the control channel went silent"
                    );
                }
            }
        }));
    }

    async fn persist_action(&self, record: &ComputerActionRecord) {
        let owner = self
            .session_owners
            .lock()
            .await
            .get(&record.session_id)
            .cloned()
            .unwrap_or_default();
        let row = Self::audit_record(
            record,
            owner.agent_session_id.as_ref(),
            owner.workspace_id.as_ref(),
        );
        let database_path = self.agent.manager().database_path().to_path_buf();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut connection = vibex_db::open_database(&database_path)?;
            vibex_db::apply_migrations(&mut connection)?;
            ComputerAuditRepository::insert(&connection, &row)
        })
        .await;
        if let Ok(Err(error)) = outcome {
            tracing::warn!(
                target: "vibex_computer",
                error_code = %error.code,
                "failed to persist a computer-use audit row"
            );
        }
    }

    /// Remembers who owns a computer session, for the audit row.
    async fn remember_owner(
        &self,
        session_id: &vibex_core::ComputerSessionId,
        agent_session_id: Option<VibexSessionId>,
        workspace_id: Option<WorkspaceId>,
    ) {
        self.session_owners.lock().await.insert(
            session_id.clone(),
            SessionOwner {
                agent_session_id,
                workspace_id,
            },
        );
    }

    /// Maps a redacted ledger entry onto the persisted audit row.
    pub fn audit_record(
        entry: &ComputerActionRecord,
        agent_session_id: Option<&VibexSessionId>,
        workspace_id: Option<&WorkspaceId>,
    ) -> vibex_db::ComputerAuditRecord {
        vibex_db::ComputerAuditRecord {
            id: entry.id.clone(),
            session_id: entry.session_id.as_str().to_string(),
            workspace_id: workspace_id.map(|workspace| workspace.as_str().to_string()),
            agent_session_id: agent_session_id.map(|session| session.as_str().to_string()),
            kind: entry.kind.as_str().to_string(),
            // Already redacted by the computer layer: no typed text, clipboard
            // contents, accessibility bodies or screenshots ever reach this
            // field.
            summary: entry.summary.clone(),
            target: entry.target.clone(),
            risk: entry.risk.as_str().to_string(),
            delivery_mode: entry.delivery_mode.map(|mode| mode.as_str().to_string()),
            verification: entry.verification.as_str(),
            status: match entry.status {
                vibex_core::ComputerOperationStatus::Verified => "verified",
                vibex_core::ComputerOperationStatus::Dispatched => "dispatched",
                vibex_core::ComputerOperationStatus::Failed => "failed",
                vibex_core::ComputerOperationStatus::Unknown => "unknown",
            }
            .to_string(),
            execution_source: match entry.execution_source {
                vibex_core::ComputerExecutionSource::Agent => "agent",
                vibex_core::ComputerExecutionSource::User => "user",
            }
            .to_string(),
            at_ms: entry.at_ms,
        }
    }

    /// Installs the MCP launch configuration on the Agent manager.
    pub async fn install_tool_config(
        self: &Arc<Self>,
        sidecar_command: PathBuf,
    ) -> VibexResult<()> {
        let Some(endpoint) = self.endpoint_url.lock().await.clone() else {
            return Err(VibexError::process(
                "computer_mcp_endpoint_missing",
                "the computer MCP endpoint must be started before the tool is installed",
            ));
        };
        let config = vibex_agent::ComputerMcpToolConfig {
            command: sidecar_command,
            endpoint,
            capability_token: self.capability_token.clone(),
        };
        // Idempotent by replacement: a later start rebinds the endpoint, and
        // the newest address is the one sessions must be told about.
        self.agent.manager().install_computer_mcp_tool(config)
    }

    /// Stops the endpoint, releases the desktop and terminates the helper.
    pub async fn shutdown(&self) {
        if let Some(task) = self.audit_task.lock().await.take() {
            task.abort();
        }
        if let Some(task) = self.disconnect_task.lock().await.take() {
            task.abort();
        }
        if let Some(endpoint) = self.endpoint.lock().await.take() {
            endpoint.stop();
        }
        self.endpoint_url.lock().await.take();
        self.service.set_endpoint_url(None);
        self.session_owners.lock().await.clear();
        self.service.shutdown().await;
        *self.helper.lock().await = None;
    }

    /// The availability report for the panel and the capability matrix.
    pub async fn availability(&self) -> ComputerAvailability {
        if let Some((reason, detail)) = self.unavailable().await {
            let mut availability = self.service.availability().await;
            availability.unavailable_reason = Some(reason);
            availability.detail = Some(detail);
            return availability;
        }
        self.service.availability().await
    }

    /// How computer tools reach one Agent over MCP.
    pub fn tool_delivery_for_agent(agent_id: &str) -> ComputerToolDelivery {
        use vibex_agent_acp::{McpWireDelivery, agent_dialect_profile};
        match agent_dialect_profile(agent_id).mcp_wire_delivery {
            McpWireDelivery::Delivered => {
                if crate::browser::AGENTS_USING_THE_STDIO_FALLBACK.contains(&agent_id) {
                    ComputerToolDelivery::Stdio
                } else {
                    ComputerToolDelivery::Http
                }
            }
            McpWireDelivery::NativeConfig => ComputerToolDelivery::Http,
            McpWireDelivery::AcceptedButDropped | McpWireDelivery::Rejected => {
                ComputerToolDelivery::Unavailable
            }
        }
    }

    /// The delivery answer for one Agent id, deviations included.
    pub fn delivery_for(agent_id: &str) -> ComputerToolDelivery {
        let profiled = vibex_agent_acp::agent_dialect_profiles()
            .iter()
            .any(|profile| profile.agent_id == agent_id);
        if profiled {
            Self::tool_delivery_for_agent(agent_id)
        } else {
            ComputerToolDelivery::Http
        }
    }

    /// The product path computer use takes for one Agent.
    ///
    /// This is the table the UI shows, and it is deliberately honest about the
    /// two paths that do not go through the runtime's policy: an Agent that
    /// carries its own computer-use feature, and an Agent that can only be
    /// reached through a shell command.
    pub fn use_delivery_for(agent_id: &str) -> ComputerUseDelivery {
        if AGENTS_WITHOUT_MCP_DELIVERY.contains(&agent_id) {
            return ComputerUseDelivery::CliSkill;
        }
        if vibex_core::AGENTS_WITH_NATIVE_COMPUTER_USE.contains(&agent_id) {
            // The native path is probed, never written: Vibex does not touch the
            // Agent's own computer-use configuration key.
            return ComputerUseDelivery::NativeAgentFeature;
        }
        match Self::delivery_for(agent_id) {
            ComputerToolDelivery::Unavailable => ComputerUseDelivery::Unavailable,
            _ => ComputerUseDelivery::McpTool,
        }
    }

    /// The tool tier for one Agent, from the same image-capability table the
    /// browser uses.
    ///
    /// The table is a conservative whitelist: an Agent is only offered
    /// screenshots after it has been observed to forward image content from a
    /// tool result to its model. Until then it gets the structured tier, and
    /// the screenshot parameter is withheld rather than advertised and ignored.
    pub fn tool_tier_for_agent(agent_id: &str) -> ComputerToolTier {
        if crate::browser::AGENTS_ACCEPTING_IMAGE_TOOL_RESULTS.contains(&agent_id) {
            ComputerToolTier::Visual
        } else {
            ComputerToolTier::Structured
        }
    }

    /// The capability matrix the UI shows, one row per user-visible Agent.
    ///
    /// Built from the product's own catalog rather than from the dialect
    /// deviations: the two rows a reader most needs are the ones that do *not*
    /// go through the runtime — an Agent carrying its own computer-use feature,
    /// and one that can only be reached through a shell command — and both
    /// follow the generic dialect.
    pub fn capability_matrix() -> Vec<(String, ComputerUseDelivery, ComputerToolTier)> {
        vibex_core::USER_VISIBLE_AGENT_IDS
            .iter()
            .map(|agent_id| {
                (
                    (*agent_id).to_string(),
                    Self::use_delivery_for(agent_id),
                    Self::tool_tier_for_agent(agent_id),
                )
            })
            .collect()
    }
}

impl ComputerRuntime {
    /// Applies the user's settings, starting or stopping the feature as needed.
    ///
    /// A switch in the settings is a live action: turning it on starts the
    /// helper and the endpoint, turning it off releases the desktop and stops
    /// them. Neither requires restarting the application, which is the whole
    /// point of the settings page.
    pub async fn apply_settings(
        self: &Arc<Self>,
        mut settings: vibex_computer::service::ComputerRuntimeSettings,
        enabled: bool,
    ) -> VibexResult<()> {
        settings.enabled = enabled;
        self.service.apply_runtime_settings(settings);
        // The timeout lives on the helper client, which is created with the
        // engine; the next start picks it up, and a running one is restarted
        // when the value actually changed.
        let timeout_changed = self
            .helper
            .lock()
            .await
            .as_ref()
            .map(|helper| {
                helper.timeout() != std::time::Duration::from_millis(settings.call_timeout_ms)
            })
            .unwrap_or(false);
        if !enabled {
            self.shutdown().await;
            self.mark_unavailable(
                ComputerUnavailableReason::FeatureDisabled,
                "computer use is switched off in the settings",
            )
            .await;
            return Ok(());
        }
        if self.endpoint_url.lock().await.is_some() && !timeout_changed {
            *self.unavailable.lock().await = None;
            return Ok(());
        }
        if timeout_changed {
            // Rebuilding the helper is the honest way to change a deadline that
            // is baked into its client; the desktop is released first.
            self.shutdown().await;
        }
        *self.unavailable.lock().await = None;
        let inputs = self.launch.read().ok().and_then(|slot| slot.clone());
        let Some(inputs) = inputs else {
            return Ok(());
        };
        self.start(inputs.sidecar_command, inputs.headless).await
    }

    /// Probes the desktop driver and remembers what it found.
    ///
    /// Read-only: it looks for the executable and asks the engine for its
    /// version. Nothing is installed and the desktop is not touched.
    pub async fn detect_driver(&self) -> DriverState {
        let state = match vibex_computer::CuaDriverCli::discover() {
            Some(driver) => {
                // The driver's own probe is what tells a driver apart from a
                // file with the right name, and it reports the version the
                // settings show. Read-only: no daemon, no desktop, no prompt.
                match driver.probe().await {
                    Ok(probe) => DriverState::Installed {
                        path: driver.executable().to_path_buf(),
                        version: probe.engine.or_else(|| {
                            probe.tool_surface.map(|surface| {
                                format!(
                                    "{} tools",
                                    surface.split(',').filter(|name| !name.is_empty()).count()
                                )
                            })
                        }),
                        responsive: true,
                    },
                    Err(_) => DriverState::Installed {
                        path: driver.executable().to_path_buf(),
                        version: None,
                        responsive: false,
                    },
                }
            }
            None => DriverState::Missing,
        };
        *self.driver.lock().await = state.clone();
        state
    }

    /// The driver state as last probed.
    pub async fn driver_state(&self) -> DriverState {
        self.driver.lock().await.clone()
    }

    /// Installs the desktop driver, on an explicit user action.
    ///
    /// This is the one place the product runs a vendor installer, and it is
    /// deliberately narrow: the user pressed Install, the command is a fixed
    /// documented script for this platform (never a model-supplied string), and
    /// the outcome — including the installer's own output — is reported back
    /// to the settings. Nothing installs on its own.
    pub async fn install_driver(self: &Arc<Self>) -> DriverState {
        if matches!(*self.driver.lock().await, DriverState::Installing) {
            return DriverState::Installing;
        }
        *self.driver.lock().await = DriverState::Installing;
        let platform = vibex_computer::driver::detect_platform();
        let command = match driver_install_command(platform) {
            Some(command) => command,
            None => {
                let state = DriverState::InstallFailed {
                    detail: format!(
                        "there is no documented driver installer for {}",
                        platform.as_str()
                    ),
                };
                *self.driver.lock().await = state.clone();
                return state;
            }
        };
        let outcome = tokio::time::timeout(
            std::time::Duration::from_secs(DRIVER_INSTALL_TIMEOUT_SECS),
            tokio::process::Command::new(command.program)
                .args(&command.args)
                .stdin(std::process::Stdio::null())
                .output(),
        )
        .await;
        let state = match outcome {
            Ok(Ok(output)) if output.status.success() => {
                let detected = self.detect_driver().await;
                match detected {
                    DriverState::Installed {
                        responsive: true, ..
                    } => detected,
                    DriverState::Installed { .. } => DriverState::InstallFailed {
                        detail: "the installer finished, but the driver it left behind does not \
                                 answer"
                            .to_string(),
                    },
                    _ => DriverState::InstallFailed {
                        detail: "the installer finished but no driver was found on this machine"
                            .to_string(),
                    },
                }
            }
            Ok(Ok(output)) => DriverState::InstallFailed {
                detail: bounded_output(&output.stderr, &output.stdout),
            },
            Ok(Err(error)) => DriverState::InstallFailed {
                detail: error.to_string(),
            },
            Err(_) => DriverState::InstallFailed {
                detail: format!(
                    "the installer did not finish within {DRIVER_INSTALL_TIMEOUT_SECS}s"
                ),
            },
        };
        *self.driver.lock().await = state.clone();
        state
    }
}

/// How long the vendor installer may take.
const DRIVER_INSTALL_TIMEOUT_SECS: u64 = 600;

/// The fixed installer for one platform.
///
/// A value rather than a string built at the call site: the settings show this
/// command before it runs, and a model must never be able to influence it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DriverInstallCommand {
    pub program: String,
    pub args: Vec<String>,
}

/// The documented installer for this platform, when there is one.
pub fn driver_install_command(platform: ComputerPlatform) -> Option<DriverInstallCommand> {
    match platform {
        ComputerPlatform::Macos | ComputerPlatform::LinuxX11 | ComputerPlatform::LinuxWayland => {
            Some(DriverInstallCommand {
                program: "/bin/sh".to_string(),
                args: vec![
                    "-c".to_string(),
                    "curl -fsSL https://cua.ai/driver/install.sh | /bin/sh".to_string(),
                ],
            })
        }
        ComputerPlatform::Windows => Some(DriverInstallCommand {
            program: "powershell".to_string(),
            args: vec![
                "-NoProfile".to_string(),
                "-Command".to_string(),
                "irm https://cua.ai/driver/install.ps1 | iex".to_string(),
            ],
        }),
        ComputerPlatform::Unknown => None,
    }
}

fn bounded_output(stderr: &[u8], stdout: &[u8]) -> String {
    let text = if stderr.is_empty() { stdout } else { stderr };
    let text = String::from_utf8_lossy(text);
    let trimmed = text.trim();
    if trimmed.is_empty() {
        return "the installer reported no output".to_string();
    }
    let bounded: String = trimmed.chars().take(400).collect();
    bounded
}

/// Wraps the helper so the service can hold it as an engine while the runtime
/// keeps the lifecycle handle.
struct HelperEngineHandle(Arc<HelperEngine>);

impl std::fmt::Debug for HelperEngineHandle {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("HelperEngineHandle").finish()
    }
}

#[async_trait]
impl vibex_computer::ComputerEngine for HelperEngineHandle {
    async fn probe(&self) -> vibex_computer::ComputerResult<vibex_computer::EngineProbe> {
        self.0.probe().await
    }

    async fn list_apps(&self) -> vibex_computer::ComputerResult<Vec<ComputerApplication>> {
        self.0.list_apps().await
    }

    async fn get_app_state(
        &self,
        request: vibex_computer::EngineStateRequest,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineAppState> {
        self.0.get_app_state(request).await
    }

    async fn click(
        &self,
        request: vibex_computer::EngineClick,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineActionResult> {
        self.0.click(request).await
    }

    async fn type_text(
        &self,
        request: vibex_computer::EngineTypeText,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineActionResult> {
        self.0.type_text(request).await
    }

    async fn set_value(
        &self,
        request: vibex_computer::EngineSetValue,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineActionResult> {
        self.0.set_value(request).await
    }

    async fn press_key(
        &self,
        request: vibex_computer::EnginePressKey,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineActionResult> {
        self.0.press_key(request).await
    }

    async fn scroll(
        &self,
        request: vibex_computer::EngineScroll,
    ) -> vibex_computer::ComputerResult<vibex_computer::EngineActionResult> {
        self.0.scroll(request).await
    }

    async fn screenshot(
        &self,
        app_id: Option<&str>,
        window_id: Option<&str>,
    ) -> vibex_computer::ComputerResult<Option<vibex_core::ComputerScreenshot>> {
        self.0.screenshot(app_id, window_id).await
    }

    async fn release_all_keys(&self) -> vibex_computer::ComputerResult<()> {
        self.0.release_all_keys().await
    }

    async fn user_activity_age_ms(&self) -> vibex_computer::ComputerResult<Option<i64>> {
        self.0.user_activity_age_ms().await
    }

    async fn launch_app(
        &self,
        app_id: &str,
    ) -> vibex_computer::ComputerResult<ComputerApplication> {
        self.0.launch_app(app_id).await
    }

    async fn kill_app(&self, app_id: &str) -> vibex_computer::ComputerResult<()> {
        self.0.kill_app(app_id).await
    }
}

/// Adapts [`ComputerRuntime`] to the computer crate's MCP host seam.
pub struct RuntimeComputerHost {
    runtime: Arc<ComputerRuntime>,
}

impl std::fmt::Debug for RuntimeComputerHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RuntimeComputerHost").finish()
    }
}

/// The permission category an approval card is filed under.
///
/// Existing categories are reused rather than extended: a new risk variant
/// would force edits to the exhaustive risk-label matches on desktop and
/// mobile for no gain. The mapping is by *kind of consequence*, which is what
/// the label matches read.
fn risk_category(risk: ComputerRiskClass) -> PermissionRiskCategory {
    match risk {
        ComputerRiskClass::ClipboardRead => PermissionRiskCategory::FileReadSensitive,
        ComputerRiskClass::ClipboardWrite => PermissionRiskCategory::FileWrite,
        ComputerRiskClass::FileDeletionOrShare => PermissionRiskCategory::FileDeleteOrMove,
        ComputerRiskClass::DestructiveClick
        | ComputerRiskClass::ForegroundEscalation
        | ComputerRiskClass::KillApp
        | ComputerRiskClass::LaunchApp
        | ComputerRiskClass::ConcurrentUserActivity
        | ComputerRiskClass::CredentialTarget
        | ComputerRiskClass::SelfTarget => PermissionRiskCategory::Command,
        ComputerRiskClass::Ordinary => PermissionRiskCategory::CustomTool,
    }
}

#[async_trait]
impl ComputerMcpHost for RuntimeComputerHost {
    async fn resolve_token(&self, token: &str) -> Option<ComputerMcpSession> {
        let session_id = verify_session_token(self.runtime.capability_token(), token)?;
        let agent_session_id = VibexSessionId::parse(session_id.clone()).ok()?;
        // The Agent session must still exist: a revoked or deleted session must
        // not keep a live desktop credential.
        let session = self
            .runtime
            .agent
            .manager()
            .get_session(&agent_session_id)
            .await
            .ok()?;
        let tier = ComputerRuntime::tool_tier_for_agent(session.agent_id.as_str());
        let computer_session_id = self
            .runtime
            .service
            .ensure_session(
                ComputerSessionKey::Agent(agent_session_id.clone()),
                Some(session.workspace_id.clone()),
                tier,
            )
            .await
            .ok()?;
        self.runtime
            .remember_owner(
                &computer_session_id,
                Some(agent_session_id.clone()),
                Some(session.workspace_id.clone()),
            )
            .await;
        // Every authenticated call is proof the controlling client is alive.
        self.runtime.service.note_client_heartbeat();
        Some(ComputerMcpSession {
            session_id: computer_session_id,
            agent_session_id: Some(agent_session_id),
            workspace_id: Some(session.workspace_id),
            tier,
            agent_label: session.agent_id.to_string(),
        })
    }

    async fn request_permission(
        &self,
        session: &ComputerMcpSession,
        request: &ComputerApprovalRequest,
    ) -> ComputerPermissionDecision {
        let Some(agent_session_id) = session.agent_session_id.clone() else {
            return ComputerPermissionDecision::Deny;
        };
        // The survey of what the user is approving: the canonical application,
        // the action class and the reason. The screenshot, when there is one,
        // is attached as a detail the client can render — it is never stored.
        let mut details = vec![
            PermissionActionDetail {
                label: "Application".to_string(),
                value: request.target.label(),
            },
            PermissionActionDetail {
                label: "Risk".to_string(),
                value: request
                    .classes
                    .iter()
                    .map(|class| class.as_str())
                    .collect::<Vec<_>>()
                    .join(", "),
            },
            PermissionActionDetail {
                label: "Why".to_string(),
                value: request.reason.clone(),
            },
            PermissionActionDetail {
                label: "Agent".to_string(),
                value: session.agent_label.clone(),
            },
        ];
        if request.screenshot.is_some() {
            details.push(PermissionActionDetail {
                label: "Preview".to_string(),
                value: "A screenshot of the target window is shown in the computer panel."
                    .to_string(),
            });
        }
        let requested_at_ms = unix_timestamp_ms();
        let can_remember = request.granularity == vibex_core::ComputerApprovalGranularity::Session;
        let permission = PermissionRequest {
            id: RequestId::new(),
            session_id: agent_session_id.clone(),
            project_id: None,
            workspace_id: session.workspace_id.clone(),
            provider_request_id: None,
            risk_category: risk_category(request.risk),
            title: request.title.clone(),
            details,
            allowed_responses: if can_remember {
                vec![
                    PermissionResponseKind::Approve,
                    PermissionResponseKind::Deny,
                    PermissionResponseKind::AlwaysAllowForSession,
                ]
            } else {
                // A destructive action, a clipboard read or a foreground
                // takeover is never offered "always allow": each one is a fresh
                // decision, and offering to remember it is the decision the
                // user cannot make safely under time pressure.
                vec![
                    PermissionResponseKind::Approve,
                    PermissionResponseKind::Deny,
                ]
            },
            response_options: Vec::new(),
            status: PermissionRequestStatus::Pending,
            requested_at_ms,
            expires_at_ms: Some(requested_at_ms + COMPUTER_APPROVAL_TTL_MS),
        };
        if let Err(error) = self
            .runtime
            .agent
            .manager()
            .record_permission_request(permission.clone())
            .await
        {
            tracing::warn!(
                target: "vibex_computer",
                code = %error.code,
                "failed to record a computer-use approval request"
            );
            return ComputerPermissionDecision::Deny;
        }
        let database_path = self.runtime.agent.manager().database_path().to_path_buf();
        let approval_epoch = self.runtime.service.approval_epoch();
        let deadline = requested_at_ms + COMPUTER_APPROVAL_TTL_MS;
        loop {
            tokio::time::sleep(Duration::from_millis(COMPUTER_APPROVAL_POLL_MS)).await;
            // An emergency stop or a disconnect voids the question: the answer
            // would arrive too late to describe the same situation.
            if self.runtime.service.approval_epoch() != approval_epoch {
                tracing::info!(
                    target: "vibex_computer",
                    "a computer-use approval card was withdrawn because the session changed state"
                );
                return ComputerPermissionDecision::Deny;
            }
            let resolved = tokio::task::spawn_blocking({
                let database_path = database_path.clone();
                let request_id = permission.id.clone();
                move || {
                    let connection = vibex_db::open_database(&database_path).ok()?;
                    PermissionRepository::get_request(&connection, &request_id).ok()?
                }
            })
            .await
            .ok()
            .flatten();
            match resolved.map(|stored| stored.status) {
                Some(PermissionRequestStatus::Approved) => {
                    return match self.runtime.last_always_allow(&permission.id).await {
                        true => ComputerPermissionDecision::AlwaysAllow,
                        false => ComputerPermissionDecision::Approve,
                    };
                }
                Some(PermissionRequestStatus::Denied) | Some(PermissionRequestStatus::Expired) => {
                    return ComputerPermissionDecision::Deny;
                }
                _ => {}
            }
            if unix_timestamp_ms() >= deadline {
                tracing::info!(
                    target: "vibex_computer",
                    "a computer-use approval card expired unanswered; denying"
                );
                return ComputerPermissionDecision::Deny;
            }
        }
    }

    async fn report(&self, _session: &ComputerMcpSession, event: &str, payload: Value) {
        tracing::debug!(
            target: "vibex_computer",
            event,
            payload = %payload,
            "computer MCP host event"
        );
    }
}

impl ComputerRuntime {
    /// Reads back whether the approval was recorded as "always allow".
    ///
    /// The permission row keeps the full resolution JSON but the repository has
    /// no accessor for it, and the computer layer is the only consumer that
    /// needs to tell a one-off approval from a session grant.
    async fn last_always_allow(&self, request_id: &RequestId) -> bool {
        let database_path = self.agent.manager().database_path().to_path_buf();
        let request_id = request_id.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(connection) = vibex_db::open_database(&database_path) else {
                return false;
            };
            let Ok(mut statement) = connection
                .prepare("SELECT resolution_json FROM permission_requests WHERE request_id = ?1")
            else {
                return false;
            };
            let Ok(value) =
                statement.query_row([request_id.as_str()], |row| row.get::<_, Option<String>>(0))
            else {
                return false;
            };
            value.is_some_and(|json| json.contains("always_allow"))
        })
        .await
        .unwrap_or(false)
    }

    /// The live frame channel for the panel, ensuring the session exists.
    pub async fn subscribe_frames(
        &self,
        workspace_id: Option<WorkspaceId>,
        tier: ComputerToolTier,
    ) -> Option<(
        vibex_core::ComputerSessionId,
        tokio::sync::watch::Receiver<Option<vibex_core::ComputerFrame>>,
    )> {
        let session_id = self
            .service
            .ensure_session(ComputerSessionKey::Panel, workspace_id, tier)
            .await
            .ok()?;
        let frames = self.service.subscribe_frames(&session_id).await;
        Some((session_id, frames))
    }

    /// The emergency stop, exposed for the panel.
    pub async fn stop(&self, reason: &str) -> VibexResult<()> {
        self.service.stop(reason).await.map_err(VibexError::from)
    }

    /// A human paused agent operations from the panel.
    pub async fn pause(
        &self,
        session_id: &vibex_core::ComputerSessionId,
        reason: &str,
    ) -> VibexResult<()> {
        self.service
            .pause(session_id, reason)
            .await
            .map_err(VibexError::from)
    }

    /// A human re-enabled computer use after a pause or a stop.
    pub async fn resume(&self, session_id: &vibex_core::ComputerSessionId) -> VibexResult<()> {
        self.service
            .resume(session_id)
            .await
            .map_err(VibexError::from)
    }

    /// Keeps the disconnect guard from firing while the panel is open.
    pub fn note_heartbeat(&self) {
        self.service.note_client_heartbeat();
    }

    /// The helper's state, for the panel's readiness row.
    pub async fn helper_state(&self) -> Option<HelperState> {
        match self.helper.lock().await.as_ref() {
            Some(helper) => Some(helper.state().await),
            None => None,
        }
    }
}

fn running_as_root() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: `geteuid` takes no arguments, touches no memory and cannot
        // fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(not(unix))]
    {
        false
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_spawn_policy_refuses_root_and_headless_macos() {
        assert_eq!(
            helper_spawn_policy(ComputerPlatform::LinuxX11, false, true, false),
            HelperSpawnPolicy::Spawn
        );
        // A headless Linux runtime with a desktop session is a supported
        // deployment; macOS is not, because of where the OS attaches grants.
        assert_eq!(
            helper_spawn_policy(ComputerPlatform::LinuxX11, true, true, false),
            HelperSpawnPolicy::Spawn
        );
        assert_eq!(
            helper_spawn_policy(ComputerPlatform::Macos, true, true, false),
            HelperSpawnPolicy::Refuse(ComputerUnavailableReason::PlatformUnsupported)
        );
        assert_eq!(
            helper_spawn_policy(ComputerPlatform::LinuxX11, false, true, true),
            HelperSpawnPolicy::Refuse(ComputerUnavailableReason::RunningAsRoot)
        );
        assert_eq!(
            helper_spawn_policy(ComputerPlatform::LinuxX11, false, false, false),
            HelperSpawnPolicy::Refuse(ComputerUnavailableReason::NoDesktopSession)
        );
    }

    #[test]
    fn the_delivery_matrix_is_honest_about_every_path() {
        // An Agent that receives no MCP server takes the CLI path.
        for agent in AGENTS_WITHOUT_MCP_DELIVERY {
            assert_eq!(
                ComputerRuntime::use_delivery_for(agent),
                ComputerUseDelivery::CliSkill
            );
        }
        // Codex carries its own feature; Vibex only probes it.
        assert_eq!(
            ComputerRuntime::use_delivery_for("codex"),
            ComputerUseDelivery::NativeAgentFeature
        );
        assert_eq!(
            ComputerRuntime::use_delivery_for("claude"),
            ComputerUseDelivery::McpTool
        );
    }

    #[test]
    fn only_confirmed_multimodal_agents_get_the_visual_tier() {
        assert_eq!(
            ComputerRuntime::tool_tier_for_agent("claude"),
            ComputerToolTier::Visual
        );
        assert_eq!(
            ComputerRuntime::tool_tier_for_agent("gemini"),
            ComputerToolTier::Visual
        );
        for agent in ["codex", "copilot", "cursor", "grok", "hermes", "pi"] {
            assert_eq!(
                ComputerRuntime::tool_tier_for_agent(agent),
                ComputerToolTier::Structured,
                "{agent} has not been confirmed to forward image content"
            );
        }
    }

    #[test]
    fn the_capability_matrix_covers_every_delivery_path_a_reader_can_meet() {
        let matrix = ComputerRuntime::capability_matrix();
        // The two rows that do not go through the runtime's policy are the ones
        // the UI exists to be honest about.
        assert!(
            matrix.iter().any(|(agent, delivery, tier)| agent == "pi"
                && *delivery == ComputerUseDelivery::CliSkill
                && *tier == ComputerToolTier::Structured),
            "the CLI path must appear in the matrix"
        );
        assert!(
            matrix.iter().any(|(agent, delivery, _)| agent == "codex"
                && *delivery == ComputerUseDelivery::NativeAgentFeature),
            "the native path must appear in the matrix and say it is not enforced here"
        );
        assert!(
            matrix
                .iter()
                .any(|(agent, delivery, tier)| agent == "claude"
                    && *delivery == ComputerUseDelivery::McpTool
                    && *tier == ComputerToolTier::Visual)
        );
        // Every row agrees with the per-Agent answers the rest of the product
        // uses; a matrix that disagreed with the runtime would be worse than
        // no matrix.
        for (agent, delivery, tier) in &matrix {
            assert_eq!(*delivery, ComputerRuntime::use_delivery_for(agent));
            assert_eq!(*tier, ComputerRuntime::tool_tier_for_agent(agent));
        }
    }

    #[test]
    fn the_installer_is_a_fixed_command_per_platform() {
        let macos =
            driver_install_command(ComputerPlatform::Macos).expect("macOS has an installer");
        assert_eq!(macos.program, "/bin/sh");
        assert!(macos.args.join(" ").contains("cua.ai/driver/install.sh"));
        let linux =
            driver_install_command(ComputerPlatform::LinuxX11).expect("Linux has an installer");
        assert_eq!(linux, macos);
        let windows =
            driver_install_command(ComputerPlatform::Windows).expect("Windows has an installer");
        assert_eq!(windows.program, "powershell");
        assert!(windows.args.join(" ").contains("cua.ai/driver/install.ps1"));
        assert!(driver_install_command(ComputerPlatform::Unknown).is_none());
    }

    #[test]
    fn a_driver_state_answers_whether_the_step_is_done() {
        let present = DriverState::Installed {
            path: PathBuf::from("/usr/bin/cua-driver"),
            version: Some("28 tools".to_string()),
            responsive: true,
        };
        assert!(present.is_installed() && present.is_present());
        // A file that does not answer is present but not usable, and the
        // settings send the reader to a different button for it.
        let silent = DriverState::Installed {
            path: PathBuf::from("/usr/bin/cua-driver"),
            version: None,
            responsive: false,
        };
        assert!(!silent.is_installed() && silent.is_present());
        assert!(!DriverState::Missing.is_installed() && !DriverState::Missing.is_present());
        assert!(!DriverState::Unknown.is_installed());
        assert!(!DriverState::Installing.is_installed());
        assert!(
            !DriverState::InstallFailed {
                detail: "no".to_string()
            }
            .is_installed()
        );
    }

    #[test]
    fn risk_categories_reuse_existing_labels() {
        assert_eq!(
            risk_category(ComputerRiskClass::DestructiveClick),
            PermissionRiskCategory::Command
        );
        assert_eq!(
            risk_category(ComputerRiskClass::ClipboardRead),
            PermissionRiskCategory::FileReadSensitive
        );
        assert_eq!(
            risk_category(ComputerRiskClass::FileDeletionOrShare),
            PermissionRiskCategory::FileDeleteOrMove
        );
    }

    #[test]
    fn audit_rows_carry_no_payload_beyond_the_redacted_summary() {
        let record = ComputerActionRecord {
            id: "action-1".to_string(),
            session_id: vibex_core::ComputerSessionId::new(),
            kind: vibex_core::ComputerActionKind::TypeText,
            summary: "entered 12 character(s) in Notes".to_string(),
            at_ms: 1,
            status: vibex_core::ComputerOperationStatus::Dispatched,
            verification: vibex_core::ComputerVerification::Unverified(
                vibex_core::ComputerUnverifiedReason::SyntheticInput,
            ),
            risk: ComputerRiskClass::Ordinary,
            target: Some("Notes (/usr/bin/notes)".to_string()),
            delivery_mode: Some(vibex_core::ComputerDeliveryMode::Background),
            execution_source: vibex_core::ComputerExecutionSource::Agent,
        };
        let row = ComputerRuntime::audit_record(&record, None, None);
        assert_eq!(row.kind, "type_text");
        assert_eq!(row.verification, "unverified(synthetic_input)");
        assert_eq!(row.delivery_mode.as_deref(), Some("background"));
        assert!(!row.summary.contains("hunter2"));
    }
}

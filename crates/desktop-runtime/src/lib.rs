//! Shared desktop composition root used by the native GPUI shell.

mod acp_terminal;
mod agent_auth_context;
mod agent_install;
mod auth_catalog;
mod catalog;
mod events;
mod fixture;
mod home_lock;
mod management;
mod relay;
mod remote_connectivity;
mod usage;
mod workbench;
mod worktree;

use std::collections::{BTreeMap, BTreeSet};
use std::future::Future;
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

use async_trait::async_trait;
use tokio::sync::{broadcast, mpsc};
use tokio::task::JoinHandle;
use vibex_agent::{
    AgentManager, MessageSubmissionCoordinator, MessageSubmissionCoordinatorConfig,
    RuntimeLifecycleConfig, RuntimeLifecycleService, RuntimeObservability, RuntimeSelectionService,
    RuntimeSelectionServiceConfig, RuntimeSwitchCoordinator, RuntimeSwitchCoordinatorConfig,
    manager_message_dispatcher,
};
use vibex_agent_acp::{
    AcpAgentProvider, AcpRuntimeClient, AcpRuntimeLifecycleBackend, AcpRuntimeSwitchBridge,
    AgentRuntimeProbeService,
};
use vibex_config_switch::{ProviderConfigService, ProviderProfileChangeListener};
use vibex_core::{
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextId, AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest,
    AgentAuthContextMutationResult, AgentAuthContextRefreshModelsRequest,
    AgentAuthContextVerifyRequest, AgentAuthenticateRequest, AgentAuthenticateResult,
    AgentAuthenticationCancelRequest, AgentLogoutRequest, AgentRuntimeKind, AgentSession,
    FetchTimelineRequest, OpenWorkspaceRequest, ProjectId, ProjectRecord, ProviderProfileId,
    TerminalCreateRequest, TerminalId, TerminalSession, TerminalSwitchShellRequest, TimelinePage,
    TimelinePayload, VibexError, VibexResult, WorkspaceId, WorkspaceMode, WorkspaceRecord,
};
use vibex_db::{
    TerminalSessionRepository, WorkspaceRepository, apply_migrations, default_database_path,
    open_database,
};
use vibex_desktop_model::DesktopPollingPolicy;
use vibex_diagnostics::{DiagnosticBundleService, DiagnosticBundleServiceConfig};
use vibex_remote::{
    RemoteAgentRuntimeProbeSource, RemoteDispatcher, RemoteGateway, RemoteGatewayConfig,
    RemoteRouter, RemoteServiceConfig, RemoteWorkbenchRuntime, RemoteWorktreeSnapshotSource,
    build_router_with_dispatcher,
};
use vibex_terminal::TerminalManager;

use acp_terminal::DesktopAcpTerminalHost;

pub use agent_auth_context::AgentAuthContextService;
pub use agent_install::{AgentInstallService, AgentNodeRuntimeOptions, AgentUvRuntimeOptions};
pub use auth_catalog::AgentAuthCatalogService;
pub use catalog::{
    ProviderModelRuntimeOptionKey, ProviderModelRuntimeOptionProbeResult,
    RuntimeOptionCatalogService, RuntimeOptionProbeResult, RuntimeOptionSnapshotSummary,
};
pub use events::{
    AuthoritativeRefetch, DesktopEvent, DesktopEventReceiver, DesktopEventReceiverClosed,
    DesktopEventStream, ProviderConfigChangePhase, ProviderConfigChangedEvent,
};
pub use fixture::FixtureDesktopRuntime;
pub use home_lock::{DESKTOP_RUNTIME_LOCK_FILE, DesktopHomeLock};
pub use management::{
    BackupProgress, ManagementMutationGuard, ProviderManagementFacade, RightRailExternalOpen,
    validate_external_open_url,
};
pub use relay::{
    RelayClientConnectionState, RelayClientRuntime, RelayClientSettings, RelayClientSettingsUpdate,
    RelayClientStatus,
};
pub use remote_connectivity::{
    DIRECT_LOOPBACK_BIND_ADDR, DIRECT_LOOPBACK_TARGET, DirectProbeInfo, DirectProbeProxyPolicy,
    DirectPublicationProbe, DirectSettings, HttpDirectPublicationProbe, HttpRelayPublicationProbe,
    MAX_DIRECT_CANDIDATES, ProcessOutput, ProcessRunner, REMOTE_ACCESS_SETTINGS_FILE,
    REMOTE_CONNECTIVITY_SCHEMA_VERSION, RelayPublicationFeatures, RelayPublicationInfo,
    RelayPublicationProbe, RelaySettings, RemoteConnectivityController, RemoteConnectivityLoad,
    RemoteConnectivityMethod, RemoteConnectivitySettingsV1, RemoteConnectivitySnapshot,
    RemoteConnectivityStore, RemoteMethodSnapshot, RemoteMethodState, RemoteRecoveryAction,
    RemoteRouteOwnership, RemoteTransitionKind, RemoteTransitionRecord, TAILSCALE_DEFAULT_PORT,
    TAILSCALE_FALLBACK_PORTS, TailscaleCli, TailscaleInspection, TailscalePublication,
    TailscaleRoute, TailscaleSettings, TokioProcessRunner, WebAssetResolver, WebBuildDescriptor,
    normalize_https_origin, parse_tailscale_inspection,
};
pub use usage::AgentUsageService;
pub use worktree::{WorktreeCoordinator, WorktreeCreateContext};

pub const STABLE_DESKTOP_APP_ID: &str = "dev.vibex.desktop";
pub const PREVIEW_APP_ID: &str = "dev.vibex.desktop.preview";
pub const PREVIEW_HOME_DIRECTORY: &str = "desktop-preview";
pub const RC_APP_ID: &str = "dev.vibex.desktop.rc";
pub const RC_HOME_DIRECTORY: &str = "desktop-rc";
pub const RELEASE_STABLE_HOME_DIRECTORY: &str = "desktop-stable";
pub const NATIVE_TERMINAL_RING_CAPACITY: usize = 2_000;
pub const NATIVE_TERMINAL_RAW_CAPACITY_BYTES: usize = 10 * 1024 * 1024;
pub const DESKTOP_UI_STATE_FILE: &str = "desktop-ui-state.json";

fn startup_stage<T>(
    stage: &'static str,
    operation: impl FnOnce() -> VibexResult<T>,
) -> VibexResult<T> {
    let started = Instant::now();
    eprintln!("vibex-startup: stage-begin stage={stage}");
    let outcome = operation();
    log_startup_stage_end(stage, started, outcome.as_ref().err());
    outcome
}

async fn startup_stage_async<T>(
    stage: &'static str,
    operation: impl Future<Output = VibexResult<T>>,
) -> VibexResult<T> {
    let started = Instant::now();
    eprintln!("vibex-startup: stage-begin stage={stage}");
    let outcome = operation.await;
    log_startup_stage_end(stage, started, outcome.as_ref().err());
    outcome
}

fn log_startup_stage_end(stage: &str, started: Instant, error: Option<&VibexError>) {
    let duration_ms = started.elapsed().as_millis().min(u64::MAX as u128) as u64;
    match error {
        Some(error) => eprintln!(
            "vibex-startup: stage-end stage={stage} result=failed error_code={} duration_ms={duration_ms}",
            error.code
        ),
        None => eprintln!(
            "vibex-startup: stage-end stage={stage} result=success duration_ms={duration_ms}"
        ),
    }
}

#[derive(Debug, Clone)]
enum ProviderProfileMutationEvent {
    Saved(ProviderProfileId),
    Deleted(ProviderProfileId),
}

struct DesktopProviderProfileChangeListener {
    sender: mpsc::UnboundedSender<ProviderProfileMutationEvent>,
}

impl ProviderProfileChangeListener for DesktopProviderProfileChangeListener {
    fn on_provider_profile_saved(
        &self,
        provider_profile_id: &ProviderProfileId,
        _profile_updated_at_ms: i64,
    ) {
        let _ = self.sender.send(ProviderProfileMutationEvent::Saved(
            provider_profile_id.clone(),
        ));
    }

    fn on_provider_profile_deleted(&self, provider_profile_id: &ProviderProfileId) {
        let _ = self.sender.send(ProviderProfileMutationEvent::Deleted(
            provider_profile_id.clone(),
        ));
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DesktopRuntimeMode {
    Stable,
    Preview,
    ReleaseCandidate,
    ReleaseStable,
}

#[derive(Debug, Clone)]
pub struct DesktopRuntimeConfig {
    pub mode: DesktopRuntimeMode,
    pub application_id: String,
    pub home_dir: PathBuf,
    pub database_path: PathBuf,
    pub event_capacity: usize,
    pub install_managed_adapters: bool,
    pub agent_node_runtime: AgentNodeRuntimeOptions,
    pub agent_uv_runtime: AgentUvRuntimeOptions,
    pub acquire_home_lock: bool,
    pub remote_gateway: RemoteGatewayConfig,
}

impl DesktopRuntimeConfig {
    pub fn stable_default() -> VibexResult<Self> {
        let database_path = default_database_path()?;
        let home_dir = database_path
            .parent()
            .map(Path::to_path_buf)
            .ok_or_else(|| {
                VibexError::storage(
                    "desktop_runtime_home_parent_missing",
                    "desktop database path has no runtime home parent",
                )
            })?;
        Ok(Self {
            mode: DesktopRuntimeMode::Stable,
            application_id: STABLE_DESKTOP_APP_ID.to_string(),
            home_dir,
            database_path,
            event_capacity: 512,
            install_managed_adapters: true,
            agent_node_runtime: AgentNodeRuntimeOptions::from_environment(),
            agent_uv_runtime: AgentUvRuntimeOptions::from_environment(),
            acquire_home_lock: true,
            remote_gateway: RemoteGatewayConfig::default(),
        })
    }

    pub fn isolated_preview(base_home: impl AsRef<Path>) -> Self {
        let home_dir = base_home.as_ref().join(PREVIEW_HOME_DIRECTORY);
        Self {
            mode: DesktopRuntimeMode::Preview,
            application_id: PREVIEW_APP_ID.to_string(),
            database_path: home_dir.join("vibex.db"),
            home_dir,
            event_capacity: 512,
            install_managed_adapters: true,
            agent_node_runtime: AgentNodeRuntimeOptions::from_environment(),
            agent_uv_runtime: AgentUvRuntimeOptions::from_environment(),
            acquire_home_lock: true,
            remote_gateway: RemoteGatewayConfig::default(),
        }
    }

    pub fn preview_default() -> VibexResult<Self> {
        let stable = Self::stable_default()?;
        Ok(Self::isolated_preview(stable.home_dir))
    }

    /// RC is opt-in and always receives a distinct app id and home.  There is
    /// intentionally no helper that silently promotes it to the stable home.
    pub fn isolated_release_candidate(base_home: impl AsRef<Path>) -> Self {
        let home_dir = base_home.as_ref().join(RC_HOME_DIRECTORY);
        Self {
            mode: DesktopRuntimeMode::ReleaseCandidate,
            application_id: RC_APP_ID.to_string(),
            database_path: home_dir.join("vibex.db"),
            home_dir,
            event_capacity: 512,
            install_managed_adapters: true,
            agent_node_runtime: AgentNodeRuntimeOptions::from_environment(),
            agent_uv_runtime: AgentUvRuntimeOptions::from_environment(),
            acquire_home_lock: true,
            remote_gateway: RemoteGatewayConfig::default(),
        }
    }

    pub fn release_candidate_default() -> VibexResult<Self> {
        let stable = Self::stable_default()?;
        Ok(Self::isolated_release_candidate(stable.home_dir))
    }

    /// Stable GPUI owns the stable app id. Its home remains distinct from
    /// preview and RC homes so published-artifact rollback cannot race another
    /// channel over the same data.
    pub fn isolated_release_stable(base_home: impl AsRef<Path>) -> Self {
        let home_dir = base_home.as_ref().join(RELEASE_STABLE_HOME_DIRECTORY);
        Self {
            mode: DesktopRuntimeMode::ReleaseStable,
            application_id: STABLE_DESKTOP_APP_ID.to_string(),
            database_path: home_dir.join("vibex.db"),
            home_dir,
            event_capacity: 512,
            install_managed_adapters: true,
            agent_node_runtime: AgentNodeRuntimeOptions::from_environment(),
            agent_uv_runtime: AgentUvRuntimeOptions::from_environment(),
            acquire_home_lock: true,
            remote_gateway: RemoteGatewayConfig::default(),
        }
    }

    pub fn release_stable_default() -> VibexResult<Self> {
        let stable = Self::stable_default()?;
        Ok(Self::isolated_release_stable(stable.home_dir))
    }

    pub fn isolated_test(home_dir: impl Into<PathBuf>) -> Self {
        let home_dir = home_dir.into();
        Self {
            mode: DesktopRuntimeMode::Preview,
            application_id: "dev.vibex.desktop.test".to_string(),
            database_path: home_dir.join("vibex.db"),
            home_dir,
            event_capacity: 32,
            install_managed_adapters: false,
            agent_node_runtime: AgentNodeRuntimeOptions::default(),
            agent_uv_runtime: AgentUvRuntimeOptions::default(),
            acquire_home_lock: true,
            remote_gateway: RemoteGatewayConfig::default(),
        }
    }

    fn validate(&self) -> VibexResult<()> {
        if self.application_id.trim().is_empty() || self.event_capacity == 0 {
            return Err(VibexError::validation(
                "desktop_runtime_config_invalid",
                "desktop runtime application id and event capacity must be valid",
            ));
        }
        if self.home_dir.as_os_str().is_empty()
            || self
                .home_dir
                .components()
                .chain(self.database_path.components())
                .any(|component| component == Component::ParentDir)
        {
            return Err(VibexError::validation(
                "desktop_runtime_path_traversal",
                "desktop runtime paths must not contain parent traversal components",
            ));
        }
        if !self.database_path.starts_with(&self.home_dir) {
            return Err(VibexError::validation(
                "desktop_runtime_database_outside_home",
                "desktop runtime database must be contained by its selected home",
            ));
        }
        match self.mode {
            DesktopRuntimeMode::Preview
                if self.application_id == PREVIEW_APP_ID
                    && !self.home_dir.ends_with(PREVIEW_HOME_DIRECTORY) =>
            {
                return Err(VibexError::validation(
                    "desktop_runtime_preview_identity_mismatch",
                    "Preview must use its isolated application home",
                ));
            }
            DesktopRuntimeMode::ReleaseCandidate
                if self.application_id == RC_APP_ID
                    && !self.home_dir.ends_with(RC_HOME_DIRECTORY) =>
            {
                return Err(VibexError::validation(
                    "desktop_runtime_rc_identity_mismatch",
                    "RC must use its isolated application home",
                ));
            }
            DesktopRuntimeMode::ReleaseStable
                if self.application_id == STABLE_DESKTOP_APP_ID
                    && !self.home_dir.ends_with(RELEASE_STABLE_HOME_DIRECTORY) =>
            {
                return Err(VibexError::validation(
                    "desktop_runtime_release_stable_identity_mismatch",
                    "stable must use the transferred isolated home",
                ));
            }
            _ => {}
        }
        self.remote_gateway.validate()?;
        Ok(())
    }
}

#[derive(Clone)]
pub struct AgentHandle {
    manager: Arc<AgentManager>,
    runtime_selection: Arc<RuntimeSelectionService>,
    runtime_lifecycle: Arc<RuntimeLifecycleService>,
    message_submission: Arc<MessageSubmissionCoordinator>,
    runtime_catalog: Arc<RuntimeOptionCatalogService>,
    auth_catalog: Arc<AgentAuthCatalogService>,
    auth_contexts: Arc<AgentAuthContextService>,
    install_service: Arc<AgentInstallService>,
}

impl AgentHandle {
    pub fn manager(&self) -> Arc<AgentManager> {
        self.manager.clone()
    }

    pub fn runtime_selection(&self) -> Arc<RuntimeSelectionService> {
        self.runtime_selection.clone()
    }

    pub fn runtime_lifecycle(&self) -> Arc<RuntimeLifecycleService> {
        self.runtime_lifecycle.clone()
    }

    pub fn message_submission(&self) -> Arc<MessageSubmissionCoordinator> {
        self.message_submission.clone()
    }

    pub fn runtime_catalog(&self) -> Arc<RuntimeOptionCatalogService> {
        self.runtime_catalog.clone()
    }

    pub fn auth_contexts(&self) -> Arc<AgentAuthContextService> {
        self.auth_contexts.clone()
    }

    pub fn ensure_default_auth_context(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> VibexResult<AgentAuthContext> {
        self.auth_contexts.ensure_default(agent_id)
    }

    pub fn list_auth_contexts(&self) -> VibexResult<Vec<AgentAuthContext>> {
        self.auth_contexts.list()
    }

    pub async fn authenticate_context(
        &self,
        request: AgentAuthContextAuthenticateRequest,
    ) -> VibexResult<AgentAuthContextAuthenticateResult> {
        self.auth_contexts.authenticate(request).await
    }

    pub fn authentication_operation(
        &self,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
    ) -> VibexResult<vibex_core::AgentAuthenticationOperation> {
        self.auth_contexts.authentication_operation(operation_id)
    }

    pub async fn cancel_context_authentication(
        &self,
        request: AgentAuthContextCancelAuthenticationRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.auth_contexts.cancel_authentication(request).await
    }

    pub async fn verify_auth_context(
        &self,
        request: AgentAuthContextVerifyRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.auth_contexts.verify(request).await
    }

    pub async fn refresh_auth_context_models(
        &self,
        request: AgentAuthContextRefreshModelsRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.auth_contexts.refresh_models(request).await
    }

    pub fn preview_auth_context_logout(
        &self,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<AgentAuthContextLogoutPreview> {
        self.auth_contexts.logout_preview(auth_context_id)
    }

    pub async fn logout_auth_context(
        &self,
        request: AgentAuthContextLogoutRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.auth_contexts.logout(request).await
    }

    pub async fn list_auth_methods(
        &self,
        agent_id: vibex_core::AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        self.auth_catalog.list(agent_id, provider_profile_id).await
    }

    pub async fn refresh_auth_methods(
        &self,
        agent_id: vibex_core::AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        self.auth_catalog
            .refresh(agent_id, provider_profile_id)
            .await
    }

    pub fn delete_auth_catalog(&self, agent_id: &vibex_core::AgentId) -> VibexResult<()> {
        self.auth_catalog.delete_agent(agent_id)
    }

    pub async fn install_managed_agent(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> VibexResult<vibex_core::AgentManagedInstallState> {
        self.install_service.install(agent_id).await
    }

    pub async fn check_managed_agent_update(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> VibexResult<vibex_core::AgentManagedInstallState> {
        self.install_service.check_update(agent_id).await
    }

    pub async fn uninstall_managed_agent(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> VibexResult<vibex_core::AgentManagedInstallState> {
        self.install_service.uninstall(agent_id).await
    }

    pub async fn authenticate(
        &self,
        request: AgentAuthenticateRequest,
    ) -> VibexResult<AgentAuthenticateResult> {
        self.manager.authenticate_agent(request).await
    }

    pub async fn cancel_authentication(
        &self,
        request: AgentAuthenticationCancelRequest,
    ) -> VibexResult<bool> {
        self.manager.cancel_agent_authentication(request).await
    }

    pub async fn logout(&self, request: AgentLogoutRequest) -> VibexResult<()> {
        self.manager.logout_agent(request).await
    }

    pub async fn list_sessions(&self, include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        self.manager.list_sessions(include_archived).await
    }

    pub async fn fetch_timeline(&self, request: FetchTimelineRequest) -> VibexResult<TimelinePage> {
        self.manager.fetch_timeline(request).await
    }
}

#[derive(Clone)]
pub struct ProviderHandle {
    service: ProviderConfigService,
    runtime_probe: AgentRuntimeProbeService,
    mutation_guard: ManagementMutationGuard,
}

impl ProviderHandle {
    pub fn service(&self) -> ProviderConfigService {
        self.service.clone()
    }

    pub fn runtime_probe_service(&self) -> AgentRuntimeProbeService {
        self.runtime_probe.clone()
    }
}

#[async_trait]
impl RemoteAgentRuntimeProbeSource for ProviderHandle {
    async fn start_runtime_probe(
        &self,
        request: vibex_core::AgentRuntimeProbeStartRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProbeRecord> {
        self.management().start_agent_runtime_probe(request)
    }

    async fn get_runtime_probe(
        &self,
        probe_id: vibex_core::AgentRuntimeProbeId,
    ) -> VibexResult<Option<vibex_core::AgentRuntimeProbeRecord>> {
        self.management().get_agent_runtime_probe(&probe_id)
    }

    async fn list_runtime_probes(
        &self,
        request: vibex_core::AgentRuntimeProbeListRequest,
    ) -> VibexResult<Vec<vibex_core::AgentRuntimeProbeRecord>> {
        self.management().list_agent_runtime_probes(request)
    }

    async fn cancel_runtime_probe(
        &self,
        request: vibex_core::AgentRuntimeProbeCancelRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProbeRecord> {
        self.management().cancel_agent_runtime_probe(request)
    }
}

#[derive(Clone)]
pub struct WorkspaceHandle {
    db_path: PathBuf,
}

impl WorkspaceHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn list(&self) -> VibexResult<Vec<(ProjectRecord, WorkspaceRecord)>> {
        let connection = open_database(&self.db_path)?;
        WorkspaceRepository::list(&connection)
    }

    pub fn open(
        &self,
        request: &OpenWorkspaceRequest,
    ) -> VibexResult<(ProjectRecord, WorkspaceRecord)> {
        let mut connection = open_database(&self.db_path)?;
        apply_migrations(&mut connection)?;
        WorkspaceRepository::ensure(
            &connection,
            &request.root_path,
            request.mode.unwrap_or(WorkspaceMode::CurrentCheckout),
        )
    }

    pub fn get(&self, workspace_id: &WorkspaceId) -> VibexResult<(ProjectRecord, WorkspaceRecord)> {
        let connection = open_database(&self.db_path)?;
        WorkspaceRepository::get(&connection, workspace_id)?
            .ok_or_else(|| VibexError::validation("workspace_not_found", "workspace was not found"))
    }

    pub fn delete_project(&self, project_id: &ProjectId) -> VibexResult<()> {
        let mut connection = open_database(&self.db_path)?;
        WorkspaceRepository::delete_project(&mut connection, project_id)
    }
}

#[derive(Clone)]
pub struct FileHandle {
    db_path: PathBuf,
}

impl FileHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[derive(Clone)]
pub struct GitHandle {
    db_path: PathBuf,
    mutation_claims: Arc<Mutex<std::collections::BTreeSet<String>>>,
    worktrees: Arc<WorktreeCoordinator>,
}

impl GitHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[async_trait]
impl RemoteWorktreeSnapshotSource for GitHandle {
    async fn worktree_eligibility(
        &self,
        workspace_id: WorkspaceId,
    ) -> VibexResult<vibex_core::GitProjectEligibility> {
        self.project_git_eligibility(&workspace_id)
    }

    async fn worktree_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> VibexResult<vibex_core::GitWorktreeLifecycleSnapshot> {
        self.worktree_snapshot(&workspace_id)
    }
}

#[derive(Clone)]
pub struct TerminalHandle {
    db_path: PathBuf,
    manager: TerminalManager,
    lifecycle: Arc<Mutex<()>>,
}

impl TerminalHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub fn manager(&self) -> TerminalManager {
        self.manager.clone()
    }

    fn lock_lifecycle(&self) -> VibexResult<std::sync::MutexGuard<'_, ()>> {
        self.lifecycle.lock().map_err(|_| {
            VibexError::process(
                "desktop_terminal_lifecycle_lock_failed",
                "desktop terminal lifecycle is unavailable",
            )
        })
    }
}

#[derive(Clone)]
pub struct ScheduledHandle {
    db_path: PathBuf,
    mutation_guard: ManagementMutationGuard,
}

impl ScheduledHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[derive(Clone)]
pub struct AutomationHandle {
    db_path: PathBuf,
    manager: Arc<AgentManager>,
    mutation_guard: ManagementMutationGuard,
}

impl AutomationHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[derive(Clone)]
pub struct RightRailHandle {
    db_path: PathBuf,
    mutation_guard: ManagementMutationGuard,
}

impl RightRailHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[derive(Clone)]
pub struct DiagnosticsHandle {
    service: DiagnosticBundleService,
    mutation_guard: ManagementMutationGuard,
}

impl DiagnosticsHandle {
    pub fn service(&self) -> DiagnosticBundleService {
        self.service.clone()
    }
}

#[derive(Clone)]
pub struct BackupHandle {
    db_path: PathBuf,
    mutation_guard: ManagementMutationGuard,
}

impl BackupHandle {
    pub fn database_path(&self) -> &Path {
        &self.db_path
    }
}

#[derive(Clone)]
pub struct RemoteHandle {
    db_path: PathBuf,
    config: RemoteServiceConfig,
    router: RemoteRouter,
    dispatcher: RemoteDispatcher,
    gateway: RemoteGateway,
    connectivity: RemoteConnectivityController,
    mutation_guard: ManagementMutationGuard,
}

/// Typed management composition passed to desktop shells.
///
/// Keeping this aggregate at the runtime boundary makes it difficult for a
/// view to accidentally open SQLite, mutate native config, or bypass the
/// existing service implementations.  Each returned handle is cheap to clone
/// and retains the runtime's ownership/lifecycle semantics.
#[derive(Clone)]
pub struct ManagementHandle {
    home_dir: PathBuf,
    providers: ProviderHandle,
    scheduled: ScheduledHandle,
    automation: AutomationHandle,
    right_rail: RightRailHandle,
    diagnostics: DiagnosticsHandle,
    backup: BackupHandle,
    remote: RemoteHandle,
    relay: RelayClientRuntime,
}

impl ManagementHandle {
    pub fn diagnostics_destination(&self) -> PathBuf {
        self.home_dir.join("diagnostics.json")
    }

    pub fn backup_destination(&self, suffix: impl AsRef<str>) -> PathBuf {
        let safe_suffix: String = suffix
            .as_ref()
            .chars()
            .filter(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
            .take(64)
            .collect();
        let safe_suffix = if safe_suffix.is_empty() {
            "latest"
        } else {
            safe_suffix.as_str()
        };
        self.home_dir.join(format!("backup-{safe_suffix}"))
    }

    pub fn providers(&self) -> ProviderHandle {
        self.providers.clone()
    }

    pub fn scheduled(&self) -> ScheduledHandle {
        self.scheduled.clone()
    }

    pub fn automation(&self) -> AutomationHandle {
        self.automation.clone()
    }

    pub fn right_rail(&self) -> RightRailHandle {
        self.right_rail.clone()
    }

    pub fn diagnostics(&self) -> DiagnosticsHandle {
        self.diagnostics.clone()
    }

    pub fn backup(&self) -> BackupHandle {
        self.backup.clone()
    }

    pub fn remote(&self) -> RemoteHandle {
        self.remote.clone()
    }

    pub fn remote_connectivity(&self) -> RemoteConnectivityController {
        self.remote.connectivity()
    }

    pub fn relay(&self) -> RelayClientRuntime {
        self.relay.clone()
    }
}

impl RemoteHandle {
    pub fn config(&self) -> &RemoteServiceConfig {
        &self.config
    }

    pub fn router(&self) -> RemoteRouter {
        self.router.clone()
    }

    pub fn dispatcher(&self) -> RemoteDispatcher {
        self.dispatcher.clone()
    }

    pub fn gateway(&self) -> RemoteGateway {
        self.gateway.clone()
    }

    pub fn connectivity(&self) -> RemoteConnectivityController {
        self.connectivity.clone()
    }
}

#[async_trait]
pub trait DesktopRuntimeFacade: Send + Sync {
    fn subscribe(&self) -> DesktopEventReceiver;
    async fn list_sessions(&self, include_archived: bool) -> Result<Vec<AgentSession>, VibexError>;
    async fn fetch_timeline(
        &self,
        request: FetchTimelineRequest,
    ) -> Result<TimelinePage, VibexError>;
    async fn shutdown(&self) -> Result<(), VibexError>;
}

pub struct DesktopRuntime {
    config: DesktopRuntimeConfig,
    agent: AgentHandle,
    providers: ProviderHandle,
    workspace: WorkspaceHandle,
    files: FileHandle,
    git: GitHandle,
    terminals: TerminalHandle,
    scheduled: ScheduledHandle,
    automation: AutomationHandle,
    right_rail: RightRailHandle,
    diagnostics: DiagnosticsHandle,
    backup: BackupHandle,
    remote: RemoteHandle,
    relay: RelayClientRuntime,
    usage: AgentUsageService,
    polling: DesktopPollingPolicy,
    events: broadcast::Sender<DesktopEvent>,
    tasks: Mutex<Vec<JoinHandle<()>>>,
    home_lock: Mutex<Option<DesktopHomeLock>>,
    shutting_down: AtomicBool,
}

impl DesktopRuntime {
    pub async fn start(config: DesktopRuntimeConfig) -> VibexResult<Arc<Self>> {
        let runtime_started = Instant::now();
        eprintln!("vibex-startup: stage-begin stage=desktop_runtime_start");
        let result = Self::start_inner(config).await;
        log_startup_stage_end(
            "desktop_runtime_start",
            runtime_started,
            result.as_ref().err(),
        );
        result
    }

    async fn start_inner(config: DesktopRuntimeConfig) -> VibexResult<Arc<Self>> {
        startup_stage("runtime_config_validate", || config.validate())?;
        let home_lock = startup_stage("runtime_home_prepare", || {
            std::fs::create_dir_all(&config.home_dir).map_err(|error| {
                VibexError::storage(
                    "desktop_runtime_home_create_failed",
                    "failed to create desktop runtime home",
                )
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
            })?;
            if config.acquire_home_lock {
                Ok(Some(DesktopHomeLock::acquire(
                    &config.home_dir,
                    &config.application_id,
                )?))
            } else {
                Ok(None)
            }
        })?;
        let observability = Arc::new(RuntimeObservability::new());
        let (provider_change_sender, provider_change_receiver) = mpsc::unbounded_channel();
        let provider_change_listener = Arc::new(DesktopProviderProfileChangeListener {
            sender: provider_change_sender,
        });
        let terminals = TerminalManager::with_raw_observation_capacity(
            NATIVE_TERMINAL_RING_CAPACITY,
            NATIVE_TERMINAL_RAW_CAPACITY_BYTES,
        );
        let terminal_host = Arc::new(DesktopAcpTerminalHost::new(terminals.clone()));
        let (manager, provider_config_service, acp_runtime) =
            startup_stage("agent_service_initialize", || {
                build_agent_manager(
                    &config,
                    observability.clone(),
                    provider_change_listener,
                    terminal_host.clone(),
                )
            })?;
        let runtime_probe = acp_runtime.runtime_probe_service();
        let manager = Arc::new(manager);
        let db_path = manager.database_path().to_path_buf();
        let usage = AgentUsageService::new(db_path.clone())?;
        let (usage_sender, usage_receiver) = mpsc::unbounded_channel();
        manager.install_usage_telemetry_sender(usage_sender)?;
        let runtime_switch_bridge = Arc::new(AcpRuntimeSwitchBridge::new(
            &db_path,
            acp_runtime.clone(),
            manager.clone(),
        )?);
        let runtime_switch_coordinator = RuntimeSwitchCoordinator::new_with_observability(
            &db_path,
            runtime_switch_bridge.clone(),
            runtime_switch_bridge.clone(),
            RuntimeSwitchCoordinatorConfig::default(),
            observability.clone(),
        )?;
        let runtime_lifecycle = Arc::new(RuntimeLifecycleService::new(
            Arc::new(AcpRuntimeLifecycleBackend::new(
                runtime_switch_bridge.clone(),
            )),
            RuntimeLifecycleConfig::default(),
        )?);
        runtime_switch_bridge.install_runtime_lifecycle(&runtime_lifecycle)?;
        let runtime_selection = Arc::new(RuntimeSelectionService::new(
            runtime_switch_coordinator,
            runtime_switch_bridge,
            RuntimeSelectionServiceConfig::default(),
        )?);
        manager.install_runtime_selection_service(&runtime_selection)?;
        let message_submission = Arc::new(MessageSubmissionCoordinator::new_with_observability(
            &db_path,
            runtime_selection.clone(),
            manager_message_dispatcher(&manager),
            MessageSubmissionCoordinatorConfig::default(),
            observability.clone(),
        )?);
        message_submission.install_runtime_lifecycle(runtime_lifecycle.clone())?;
        manager.install_message_submission_coordinator(&message_submission)?;
        let auth_catalog = Arc::new(AgentAuthCatalogService::new(
            manager.clone(),
            provider_config_service.clone(),
        ));
        let auth_contexts = Arc::new(AgentAuthContextService::new(
            db_path.clone(),
            manager.clone(),
            acp_runtime.clone(),
            terminal_host,
            auth_catalog.clone(),
        )?);
        let runtime_catalog = Arc::new(
            RuntimeOptionCatalogService::with_live_runtime(
                manager.clone(),
                provider_config_service.clone(),
                acp_runtime.clone(),
            )
            .with_auth_context_service(auth_contexts.clone()),
        );
        let install_service = Arc::new(AgentInstallService::new_with_runtime_options(
            db_path.clone(),
            config.home_dir.join("acp-agents"),
            provider_config_service.clone(),
            config.agent_node_runtime.clone(),
            config.agent_uv_runtime.clone(),
        )?);
        let git = GitHandle {
            db_path: db_path.clone(),
            mutation_claims: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
            worktrees: Arc::new(WorktreeCoordinator::new(db_path.clone())),
        };
        let providers = ProviderHandle {
            service: provider_config_service,
            runtime_probe,
            mutation_guard: ManagementMutationGuard::default(),
        };
        let remote_config = config.remote_gateway.service.clone();
        let remote_dispatcher = RemoteDispatcher::with_agent_runtime_lifecycle_and_workbench(
            remote_config.clone(),
            manager.clone(),
            runtime_selection.clone(),
            runtime_lifecycle.clone(),
            message_submission.clone(),
            RemoteWorkbenchRuntime::new(db_path.clone(), terminals.clone())
                .with_worktree_snapshot_source(Arc::new(git.clone())),
        )
        .with_runtime_option_catalog_source(runtime_catalog.clone())
        .with_agent_auth_context_source(auth_contexts.clone())
        .with_agent_runtime_probe_source(Arc::new(providers.clone()));
        let remote_gateway = RemoteGateway::new(
            config.remote_gateway.clone(),
            remote_dispatcher.clone(),
            db_path.clone(),
            config.home_dir.join("relay/desktop-identity.json"),
        );
        let relay = RelayClientRuntime::with_remote_gateway(
            remote_dispatcher.clone(),
            remote_gateway.clone(),
        )?;
        let connectivity = RemoteConnectivityController::new(
            &config.home_dir,
            remote_gateway.clone(),
            relay.clone(),
        )?;
        let web_assets = if cfg!(debug_assertions) {
            WebAssetResolver::debug(
                PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../apps/web/dist"),
            )
        } else {
            WebAssetResolver::packaged_for_current_exe()
        };
        connectivity.set_web_asset_resolver(Some(web_assets)).await;
        let remote = RemoteHandle {
            db_path: db_path.clone(),
            config: remote_config,
            router: build_router_with_dispatcher(remote_dispatcher.clone()),
            dispatcher: remote_dispatcher.clone(),
            gateway: remote_gateway,
            connectivity,
            mutation_guard: ManagementMutationGuard::default(),
        };
        let (events, _) = broadcast::channel(config.event_capacity);
        let runtime = Arc::new(Self {
            config,
            agent: AgentHandle {
                manager: manager.clone(),
                runtime_selection,
                runtime_lifecycle,
                message_submission,
                runtime_catalog,
                auth_catalog,
                auth_contexts,
                install_service,
            },
            providers,
            workspace: WorkspaceHandle {
                db_path: db_path.clone(),
            },
            files: FileHandle {
                db_path: db_path.clone(),
            },
            git,
            terminals: TerminalHandle {
                db_path: db_path.clone(),
                manager: terminals,
                lifecycle: Arc::new(Mutex::new(())),
            },
            scheduled: ScheduledHandle {
                db_path: db_path.clone(),
                mutation_guard: ManagementMutationGuard::default(),
            },
            automation: AutomationHandle {
                db_path: db_path.clone(),
                manager: manager.clone(),
                mutation_guard: ManagementMutationGuard::default(),
            },
            right_rail: RightRailHandle {
                db_path: db_path.clone(),
                mutation_guard: ManagementMutationGuard::default(),
            },
            diagnostics: DiagnosticsHandle {
                service: DiagnosticBundleService::new(
                    DiagnosticBundleServiceConfig::new(db_path.clone())
                        .with_runtime_observability(observability),
                ),
                mutation_guard: ManagementMutationGuard::default(),
            },
            backup: BackupHandle {
                db_path: db_path.clone(),
                mutation_guard: ManagementMutationGuard::default(),
            },
            remote,
            relay,
            usage,
            polling: DesktopPollingPolicy::default(),
            events,
            tasks: Mutex::new(Vec::new()),
            home_lock: Mutex::new(home_lock),
            shutting_down: AtomicBool::new(false),
        });
        startup_stage("usage_consumer_start", || {
            runtime.spawn_usage_consumer(usage_receiver)
        })?;
        startup_stage("provider_config_consumer_start", || {
            runtime.spawn_provider_config_consumer(provider_change_receiver)
        })?;
        startup_stage("agent_auth_context_consumer_start", || {
            runtime.spawn_agent_auth_context_consumer()
        })?;
        runtime.activate().await?;
        startup_stage("startup_reconciliation_spawn", || {
            runtime.spawn_startup_reconciliation()
        })?;
        let bootstrap_started = Instant::now();
        eprintln!("vibex-startup: stage-begin stage=managed_agent_bootstrap_spawn");
        let bootstrap = (|| {
            runtime.spawn_agent_bootstrap()?;
            Ok(())
        })();
        log_startup_stage_end(
            "managed_agent_bootstrap_spawn",
            bootstrap_started,
            bootstrap.as_ref().err(),
        );
        bootstrap?;
        Ok(runtime)
    }

    async fn activate(self: &Arc<Self>) -> VibexResult<()> {
        let activate_started = Instant::now();
        eprintln!("vibex-startup: stage-begin stage=runtime_activate");
        let result = self.activate_inner().await;
        log_startup_stage_end("runtime_activate", activate_started, result.as_ref().err());
        result
    }

    async fn activate_inner(self: &Arc<Self>) -> VibexResult<()> {
        if let Err(error) = startup_stage("worktree_reconcile", || {
            self.git.reconcile_worktrees_on_startup()
        }) {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "managed worktree startup reconciliation failed"
            );
        }
        startup_stage("runtime_lifecycle_start", || {
            self.agent
                .runtime_lifecycle
                .start(&tokio::runtime::Handle::current())
        })?;
        if let Err(error) = startup_stage("runtime_probe_reconcile", || {
            self.providers.runtime_probe.reconcile_on_startup()
        }) {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "Agent runtime probe startup reconciliation failed"
            );
        }
        if let Err(error) =
            startup_stage_async("remote_gateway_start", self.remote.gateway.start()).await
        {
            let _ = self.agent.runtime_lifecycle.stop().await;
            return Err(error);
        }
        if let Err(error) = startup_stage_async(
            "remote_connectivity_reconcile",
            self.remote.connectivity.reconcile_on_startup(),
        )
        .await
        {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "remote connectivity startup reconciliation failed"
            );
        }
        if let Err(error) = startup_stage("event_bridges_start", || self.spawn_event_bridges()) {
            let _ = self.remote.gateway.stop().await;
            let _ = self.agent.runtime_lifecycle.stop().await;
            return Err(error);
        }
        Ok(())
    }

    fn spawn_startup_reconciliation(&self) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let runtime_selection = self.agent.runtime_selection.clone();
        let message_submission = self.agent.message_submission.clone();
        tasks.push(tokio::spawn(async move {
            if let Err(error) = startup_stage_async(
                "runtime_selection_reconcile",
                runtime_selection.reconcile_on_startup(),
            )
            .await
            {
                tracing::warn!(
                    target: "vibex_desktop",
                    error_code = %error.code,
                    "runtime selection background reconciliation failed"
                );
                return;
            }
            if let Err(error) = startup_stage("message_submission_reconcile", || {
                message_submission.reconcile_on_startup()
            }) {
                tracing::warn!(
                    target: "vibex_desktop",
                    error_code = %error.code,
                    "message submission background reconciliation failed"
                );
            }
        }));
        Ok(())
    }

    fn spawn_agent_bootstrap(&self) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let install_managed_adapters = self.config.install_managed_adapters;
        let install_service = self.agent.install_service.clone();
        let runtime_catalog = self.agent.runtime_catalog();
        let auth_contexts = self.agent.auth_contexts();
        let provider_config = self.providers.service();
        let runtime_option_events = self.events.clone();
        let runtime_option_gateway = self.remote.gateway.clone();
        tasks.push(tokio::spawn(async move {
            if install_managed_adapters {
                let agent_ids = match install_service.bootstrap_agent_ids() {
                    Ok(agent_ids) => agent_ids,
                    Err(error) => {
                        tracing::warn!(
                            target: "vibex_desktop",
                            error_code = %error.code,
                            "managed ACP Agent bootstrap inventory failed"
                        );
                        Vec::new()
                    }
                };
                for agent_id in agent_ids {
                    let id = agent_id.as_str().to_string();
                    if let Err(error) = install_service.ensure_installed(agent_id).await {
                        tracing::warn!(
                            target: "vibex_desktop",
                            agent_id = %id,
                            error_code = %error.code,
                            "managed ACP Agent background preparation failed"
                        );
                    }
                }
            }
            match provider_config.list_agents(vibex_core::AgentListRequest {
                include_disabled: false,
            }) {
                Ok(agents) => {
                    for agent in agents.agents.into_iter().filter(|agent| {
                        agent.added
                            && agent.enabled
                            && agent.installed
                            && auth_contexts.supports_agent_account(&agent.id)
                    }) {
                        let context = match auth_contexts.ensure_default(&agent.id) {
                            Ok(context) => context,
                            Err(error) => {
                                tracing::warn!(
                                    target: "vibex_desktop",
                                    agent_id = %agent.id,
                                    error_code = %error.code,
                                    "Agent default authentication context bootstrap failed"
                                );
                                continue;
                            }
                        };
                        if context.status != vibex_core::AgentAuthContextStatus::Unverified {
                            continue;
                        }
                        if let Err(error) = auth_contexts
                            .verify(vibex_core::AgentAuthContextVerifyRequest {
                                auth_context_id: context.id,
                                expected_context_revision: context.revision,
                                operation_id: None,
                            })
                            .await
                        {
                            tracing::warn!(
                                target: "vibex_desktop",
                                agent_id = %agent.id,
                                error_code = %error.code,
                                "Agent default authentication context verification failed"
                            );
                        }
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Agent default authentication context inventory failed"
                    );
                }
            }
            let mut runtime_options_changed = false;
            match runtime_catalog.probe_missing_enabled_agents().await {
                Ok(result) => {
                    if !result.probed_agent_ids.is_empty() {
                        runtime_options_changed = true;
                    }
                    if !result.failed_agent_ids.is_empty() {
                        tracing::warn!(
                            target: "vibex_desktop",
                            failed_agent_count = result.failed_agent_ids.len(),
                            "Agent runtime option background probing failed"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Agent runtime option bootstrap failed"
                    );
                }
            }
            match runtime_catalog.probe_missing_enabled_profile_models().await {
                Ok(result) => {
                    if !result.probed_models.is_empty() {
                        runtime_options_changed = true;
                    }
                    if !result.failed_models.is_empty() {
                        tracing::warn!(
                            target: "vibex_desktop",
                            failed_model_count = result.failed_models.len(),
                            "Provider model runtime option background probing failed"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Provider model runtime option bootstrap failed"
                    );
                }
            }
            if runtime_options_changed {
                let _ = runtime_option_events.send(DesktopEvent::ProviderConfigChanged(
                    ProviderConfigChangedEvent {
                        provider_profile_ids: Vec::new(),
                        phase: ProviderConfigChangePhase::RuntimeOptionsChanged,
                    },
                ));
                if let Err(error) = runtime_option_gateway.publish_provider_invalidation() {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Remote runtime option catalog invalidation failed"
                    );
                }
            }
        }));
        Ok(())
    }

    fn spawn_agent_auth_context_consumer(&self) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let mut receiver = self.agent.auth_contexts.subscribe_changes();
        let events = self.events.clone();
        let gateway = self.remote.gateway.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                let first = match receiver.recv().await {
                    Ok(change) => change,
                    Err(broadcast::error::RecvError::Lagged(_)) => {
                        let _ = events.send(DesktopEvent::ProviderConfigChanged(
                            ProviderConfigChangedEvent {
                                provider_profile_ids: Vec::new(),
                                phase: ProviderConfigChangePhase::RuntimeOptionsChanged,
                            },
                        ));
                        if let Err(error) = gateway.publish_provider_invalidation() {
                            tracing::warn!(
                                target: "vibex_desktop",
                                error_code = %error.code,
                                "Remote Agent authentication catalog invalidation failed"
                            );
                        }
                        continue;
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                };
                let mut agent_ids = BTreeSet::from([first.agent_id]);
                let mut auth_context_ids = BTreeSet::from([first.auth_context_id]);
                while let Ok(change) = receiver.try_recv() {
                    agent_ids.insert(change.agent_id);
                    auth_context_ids.insert(change.auth_context_id);
                }
                tracing::debug!(
                    target: "vibex_desktop",
                    agent_count = agent_ids.len(),
                    auth_context_count = auth_context_ids.len(),
                    "Agent authentication runtime options changed"
                );
                let _ = events.send(DesktopEvent::ProviderConfigChanged(
                    ProviderConfigChangedEvent {
                        provider_profile_ids: Vec::new(),
                        phase: ProviderConfigChangePhase::RuntimeOptionsChanged,
                    },
                ));
                if let Err(error) = gateway.publish_provider_invalidation() {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Remote Agent authentication catalog invalidation failed"
                    );
                }
            }
        }));
        Ok(())
    }

    fn spawn_event_bridges(&self) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let mut timeline = self.agent.manager.subscribe();
        let timeline_events = self.events.clone();
        let auth_contexts = self.agent.auth_contexts();
        tasks.push(tokio::spawn(async move {
            loop {
                match timeline.recv().await {
                    Ok(event) => {
                        let _ = timeline_events.send(DesktopEvent::Timeline(event.clone()));
                        if let TimelinePayload::Error(error) = &event.item.payload
                            && let Err(failure) = auth_contexts
                                .handle_timeline_authentication_required(
                                    &event.session_id,
                                    &error.code,
                                )
                                .await
                        {
                            tracing::warn!(
                                target: "vibex_desktop",
                                error_code = %failure.code,
                                "Agent authentication failure invalidation failed"
                            );
                        }
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = timeline_events.send(DesktopEvent::Lagged {
                            stream: DesktopEventStream::Timeline,
                            skipped,
                            refetch: AuthoritativeRefetch::for_stream(DesktopEventStream::Timeline),
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
        let mut runtime_events = self.agent.runtime_lifecycle.subscribe();
        let runtime_tx = self.events.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                match runtime_events.recv().await {
                    Ok(event) => {
                        let _ = runtime_tx.send(DesktopEvent::Runtime(event));
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = runtime_tx.send(DesktopEvent::Lagged {
                            stream: DesktopEventStream::Runtime,
                            skipped,
                            refetch: AuthoritativeRefetch::for_stream(DesktopEventStream::Runtime),
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
        let mut selection_events = self.agent.runtime_selection.subscribe();
        let selection_tx = self.events.clone();
        tasks.push(tokio::spawn(async move {
            loop {
                match selection_events.recv().await {
                    Ok(event) => {
                        let _ = selection_tx.send(DesktopEvent::RuntimeSelection(event));
                    }
                    Err(broadcast::error::RecvError::Lagged(skipped)) => {
                        let _ = selection_tx.send(DesktopEvent::Lagged {
                            stream: DesktopEventStream::RuntimeSelection,
                            skipped,
                            refetch: AuthoritativeRefetch::for_stream(
                                DesktopEventStream::RuntimeSelection,
                            ),
                        });
                    }
                    Err(broadcast::error::RecvError::Closed) => break,
                }
            }
        }));
        Ok(())
    }

    fn spawn_usage_consumer(
        &self,
        mut receiver: mpsc::UnboundedReceiver<vibex_agent::AgentUsageTelemetryEvent>,
    ) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let service = self.usage.clone();
        let events = self.events.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(event) = receiver.recv().await {
                match service.apply_telemetry_event(event) {
                    Ok(true) => {
                        let _ = events.send(DesktopEvent::UsageInvalidated);
                    }
                    Ok(false) => {}
                    Err(error) => {
                        tracing::warn!(
                            target: "vibex_desktop",
                            error_code = %error.code,
                            "Agent usage telemetry persistence failed"
                        );
                    }
                }
            }
        }));
        Ok(())
    }

    fn spawn_provider_config_consumer(
        &self,
        mut receiver: mpsc::UnboundedReceiver<ProviderProfileMutationEvent>,
    ) -> VibexResult<()> {
        let mut tasks = self.tasks.lock().map_err(|_| {
            VibexError::process(
                "desktop_runtime_task_lock_failed",
                "desktop runtime task ownership is unavailable",
            )
        })?;
        let profile_events = self.events.clone();
        let profile_gateway = self.remote.gateway.clone();
        let runtime_option_events = self.events.clone();
        let runtime_option_gateway = self.remote.gateway.clone();
        let runtime_catalog = self.agent.runtime_catalog();
        let (model_probe_sender, mut model_probe_receiver) =
            mpsc::unbounded_channel::<BTreeMap<ProviderProfileId, bool>>();
        tasks.push(tokio::spawn(async move {
            while let Some(first) = receiver.recv().await {
                let mut changes = vec![first];
                while let Ok(change) = receiver.try_recv() {
                    changes.push(change);
                }
                let mut latest_changes = BTreeMap::new();
                for change in changes {
                    match change {
                        ProviderProfileMutationEvent::Saved(provider_profile_id) => {
                            latest_changes.insert(provider_profile_id, true);
                        }
                        ProviderProfileMutationEvent::Deleted(provider_profile_id) => {
                            latest_changes.insert(provider_profile_id, false);
                        }
                    }
                }
                if !latest_changes.is_empty() {
                    let provider_profile_ids = latest_changes.keys().cloned().collect();
                    let _ = profile_events.send(DesktopEvent::ProviderConfigChanged(
                        ProviderConfigChangedEvent {
                            provider_profile_ids,
                            phase: ProviderConfigChangePhase::ProfilesChanged,
                        },
                    ));
                    if let Err(error) = profile_gateway.publish_provider_invalidation() {
                        tracing::warn!(
                            target: "vibex_desktop",
                            error_code = %error.code,
                            "Remote Provider projection invalidation failed"
                        );
                    }
                }
                let _ = model_probe_sender.send(latest_changes);
            }
        }));
        tasks.push(tokio::spawn(async move {
            while let Some(latest_changes) = model_probe_receiver.recv().await {
                let mut probed_profile_ids = BTreeSet::new();
                for (provider_profile_id, saved) in latest_changes {
                    if !saved {
                        if let Err(error) =
                            runtime_catalog.delete_profile_model_snapshots(&provider_profile_id)
                        {
                            tracing::warn!(
                                target: "vibex_desktop",
                                provider_profile_id = %provider_profile_id,
                                error_code = %error.code,
                                "Provider model runtime option cleanup failed"
                            );
                        }
                        continue;
                    }
                    match runtime_catalog
                        .probe_profile_models(&provider_profile_id)
                        .await
                    {
                        Ok(result) => {
                            if !result.failed_models.is_empty() {
                                tracing::warn!(
                                    target: "vibex_desktop",
                                    provider_profile_id = %provider_profile_id,
                                    failed_model_count = result.failed_models.len(),
                                    "Provider model runtime option background probing failed"
                                );
                            }
                            if !result.probed_models.is_empty() {
                                probed_profile_ids.insert(provider_profile_id);
                            }
                        }
                        Err(error) => {
                            tracing::warn!(
                                target: "vibex_desktop",
                                provider_profile_id = %provider_profile_id,
                                error_code = %error.code,
                                "Provider model runtime option background probe failed"
                            );
                        }
                    }
                }
                if !probed_profile_ids.is_empty() {
                    let _ = runtime_option_events.send(DesktopEvent::ProviderConfigChanged(
                        ProviderConfigChangedEvent {
                            provider_profile_ids: probed_profile_ids.into_iter().collect(),
                            phase: ProviderConfigChangePhase::RuntimeOptionsChanged,
                        },
                    ));
                    if let Err(error) = runtime_option_gateway.publish_provider_invalidation() {
                        tracing::warn!(
                            target: "vibex_desktop",
                            error_code = %error.code,
                            "Remote runtime option catalog invalidation failed"
                        );
                    }
                }
            }
        }));
        Ok(())
    }

    pub fn config(&self) -> &DesktopRuntimeConfig {
        &self.config
    }

    pub fn ui_state_path(&self) -> PathBuf {
        self.config.home_dir.join(DESKTOP_UI_STATE_FILE)
    }

    pub fn agent(&self) -> AgentHandle {
        self.agent.clone()
    }

    pub fn providers(&self) -> ProviderHandle {
        self.providers.clone()
    }

    pub fn usage(&self) -> AgentUsageService {
        self.usage.clone()
    }

    pub fn workspace(&self) -> WorkspaceHandle {
        self.workspace.clone()
    }

    pub fn files(&self) -> FileHandle {
        self.files.clone()
    }

    pub fn git(&self) -> GitHandle {
        self.git.clone()
    }

    pub fn terminals(&self) -> TerminalHandle {
        self.terminals.clone()
    }

    /// Lists persisted and currently running terminals for a workspace. Active
    /// PTYs win over their persisted snapshot. Persisted open sessions are
    /// restored before they are returned so every visible id has a live PTY.
    pub fn list_terminals(&self, workspace_id: &WorkspaceId) -> VibexResult<Vec<TerminalSession>> {
        self.ensure_accepting_actions()?;
        let _lifecycle = self.terminals.lock_lifecycle()?;
        let mut connection = open_database(&self.config.database_path)?;
        apply_migrations(&mut connection)?;
        let stored = TerminalSessionRepository::list(&connection, workspace_id)?;
        let manager = self.terminals.manager();
        let live = manager.list(workspace_id)?;
        let live_by_id = live
            .iter()
            .map(|session| (session.id.clone(), session.clone()))
            .collect::<BTreeMap<_, _>>();
        let mut workspace_root = None;
        let mut visible = Vec::new();

        for session in stored {
            if let Some(live_session) = live_by_id.get(&session.id) {
                TerminalSessionRepository::upsert(&connection, live_session)?;
                if terminal_session_visible(live_session) {
                    visible.push(live_session.clone());
                }
            } else if terminal_session_should_restore(&session) {
                if workspace_root.is_none() {
                    let (_, workspace) = WorkspaceRepository::get(&connection, workspace_id)?
                        .ok_or_else(|| {
                            VibexError::validation("workspace_not_found", "workspace was not found")
                        })?;
                    workspace_root = Some(PathBuf::from(workspace.root_path));
                }
                let restored = manager.restore(
                    workspace_root
                        .as_ref()
                        .expect("terminal workspace root initialized"),
                    session,
                )?;
                TerminalSessionRepository::upsert(&connection, &restored)?;
                visible.push(restored);
            }
        }
        for session in live {
            if terminal_session_visible(&session)
                && !visible.iter().any(|stored| stored.id == session.id)
            {
                TerminalSessionRepository::upsert(&connection, &session)?;
                visible.push(session);
            }
        }
        Ok(visible)
    }

    /// Creates a Composer-owned terminal and durably records its session
    /// metadata. Rendering and PTY interaction remain native-surface concerns.
    pub fn create_terminal(
        &self,
        workspace_root: impl AsRef<Path>,
        request: TerminalCreateRequest,
    ) -> VibexResult<TerminalSession> {
        self.ensure_accepting_actions()?;
        let _lifecycle = self.terminals.lock_lifecycle()?;
        let session = self.terminals.manager().create(workspace_root, request)?;
        let mut connection = open_database(&self.config.database_path)?;
        apply_migrations(&mut connection)?;
        TerminalSessionRepository::upsert(&connection, &session)?;
        Ok(session)
    }

    /// Stops a terminal and persists the final status so every desktop shell
    /// observes the same lifecycle on its next terminal-list refresh.
    pub fn kill_terminal(&self, terminal_id: &TerminalId) -> VibexResult<TerminalSession> {
        self.ensure_accepting_actions()?;
        let _lifecycle = self.terminals.lock_lifecycle()?;
        let session = self.terminals.manager().kill(terminal_id)?;
        let mut connection = open_database(&self.config.database_path)?;
        apply_migrations(&mut connection)?;
        TerminalSessionRepository::upsert(&connection, &session)?;
        Ok(session)
    }

    /// Restarts an existing terminal with a different shell and persists the
    /// updated session metadata for every desktop surface.
    pub fn switch_terminal_shell(
        &self,
        request: &TerminalSwitchShellRequest,
    ) -> VibexResult<TerminalSession> {
        self.ensure_accepting_actions()?;
        let _lifecycle = self.terminals.lock_lifecycle()?;
        let session = self.terminals.manager().switch_shell(request)?;
        let mut connection = open_database(&self.config.database_path)?;
        apply_migrations(&mut connection)?;
        TerminalSessionRepository::upsert(&connection, &session)?;
        Ok(session)
    }

    pub fn scheduled(&self) -> ScheduledHandle {
        self.scheduled.clone()
    }

    pub fn automation(&self) -> AutomationHandle {
        self.automation.clone()
    }

    pub fn right_rail(&self) -> RightRailHandle {
        self.right_rail.clone()
    }

    pub fn diagnostics(&self) -> DiagnosticsHandle {
        self.diagnostics.clone()
    }

    pub fn backup(&self) -> BackupHandle {
        self.backup.clone()
    }

    pub fn remote(&self) -> RemoteHandle {
        self.remote.clone()
    }

    pub fn remote_connectivity(&self) -> RemoteConnectivityController {
        self.remote.connectivity()
    }

    pub fn relay(&self) -> RelayClientRuntime {
        self.relay.clone()
    }

    /// Returns the typed facade used by the GPUI management center.
    pub fn management(&self) -> ManagementHandle {
        ManagementHandle {
            home_dir: self.config.home_dir.clone(),
            providers: self.providers.clone(),
            scheduled: self.scheduled.clone(),
            automation: self.automation.clone(),
            right_rail: self.right_rail.clone(),
            diagnostics: self.diagnostics.clone(),
            backup: self.backup.clone(),
            remote: self.remote.clone(),
            relay: self.relay.clone(),
        }
    }

    pub fn polling_policy(&self) -> DesktopPollingPolicy {
        self.polling
    }

    pub fn is_shutting_down(&self) -> bool {
        self.shutting_down.load(Ordering::Acquire)
    }

    pub fn ensure_accepting_actions(&self) -> VibexResult<()> {
        if self.is_shutting_down() {
            Err(VibexError::conflict(
                "desktop_runtime_shutting_down",
                "desktop runtime is shutting down and cannot accept new actions",
            ))
        } else {
            Ok(())
        }
    }

    async fn shutdown_inner(&self) -> VibexResult<()> {
        if self.shutting_down.swap(true, Ordering::AcqRel) {
            return Ok(());
        }
        let mut first_error = None;
        if let Err(error) = self.relay.stop().await {
            record_shutdown_error(&mut first_error, error);
        }
        if let Err(error) = self.remote.gateway.stop().await {
            record_shutdown_error(&mut first_error, error);
        }
        match self.terminals.manager.shutdown_all() {
            Ok(terminal_shutdown) => {
                if !terminal_shutdown.sessions.is_empty() {
                    let persist = (|| -> VibexResult<()> {
                        let mut connection = open_database(&self.terminals.db_path)?;
                        apply_migrations(&mut connection)?;
                        for terminal in terminal_shutdown.sessions {
                            TerminalSessionRepository::upsert(&connection, &terminal)?;
                        }
                        Ok(())
                    })();
                    if let Err(error) = persist {
                        record_shutdown_error(&mut first_error, error);
                    }
                }
                for error in terminal_shutdown.failures {
                    record_shutdown_error(&mut first_error, error);
                }
            }
            Err(error) => record_shutdown_error(&mut first_error, error),
        }
        if let Err(error) = self.agent.runtime_lifecycle.stop().await {
            record_shutdown_error(&mut first_error, error);
        }
        let _ = self.events.send(DesktopEvent::Shutdown);
        let tasks = match self.tasks.lock() {
            Ok(mut tasks) => tasks.drain(..).collect::<Vec<_>>(),
            Err(_) => {
                record_shutdown_error(
                    &mut first_error,
                    VibexError::process(
                        "desktop_runtime_task_lock_failed",
                        "desktop runtime task ownership is unavailable",
                    ),
                );
                Vec::new()
            }
        };
        for task in &tasks {
            task.abort();
        }
        for task in tasks {
            let _ = tokio::time::timeout(std::time::Duration::from_secs(2), task).await;
        }
        match self.home_lock.lock() {
            Ok(mut home_lock) => {
                home_lock.take();
            }
            Err(_) => record_shutdown_error(
                &mut first_error,
                VibexError::process(
                    "desktop_runtime_home_lock_state_failed",
                    "desktop runtime home lock state is unavailable",
                ),
            ),
        }
        if let Some(error) = first_error {
            Err(error)
        } else {
            Ok(())
        }
    }
}

fn terminal_session_should_restore(session: &TerminalSession) -> bool {
    matches!(
        session.status,
        vibex_core::TerminalStatus::Running | vibex_core::TerminalStatus::Stale
    )
}

fn terminal_session_visible(session: &TerminalSession) -> bool {
    session.status == vibex_core::TerminalStatus::Running
}

fn record_shutdown_error(first_error: &mut Option<VibexError>, error: VibexError) {
    if first_error.is_none() {
        *first_error = Some(error);
    } else {
        tracing::warn!(
            target: "vibex_desktop",
            error_code = %error.code,
            "additional desktop runtime shutdown step failed"
        );
    }
}

#[async_trait]
impl DesktopRuntimeFacade for DesktopRuntime {
    fn subscribe(&self) -> DesktopEventReceiver {
        DesktopEventReceiver::new(self.events.subscribe())
    }

    async fn list_sessions(&self, include_archived: bool) -> Result<Vec<AgentSession>, VibexError> {
        self.ensure_accepting_actions()?;
        self.agent.list_sessions(include_archived).await
    }

    async fn fetch_timeline(
        &self,
        request: FetchTimelineRequest,
    ) -> Result<TimelinePage, VibexError> {
        self.ensure_accepting_actions()?;
        self.agent.fetch_timeline(request).await
    }

    async fn shutdown(&self) -> Result<(), VibexError> {
        self.shutdown_inner().await
    }
}

impl Drop for DesktopRuntime {
    fn drop(&mut self) {
        if let Ok(tasks) = self.tasks.get_mut() {
            for task in tasks.drain(..) {
                task.abort();
            }
        }
        if let Ok(lock) = self.home_lock.get_mut() {
            lock.take();
        }
    }
}

fn build_agent_manager(
    config: &DesktopRuntimeConfig,
    observability: Arc<RuntimeObservability>,
    profile_change_listener: Arc<dyn ProviderProfileChangeListener>,
    terminal_host: Arc<dyn vibex_agent_acp::AcpTerminalHost>,
) -> VibexResult<(AgentManager, ProviderConfigService, Arc<AcpRuntimeClient>)> {
    let db_path = config.database_path.clone();
    let bootstrap_config_service = ProviderConfigService::new(&db_path);
    let mut manager = AgentManager::new(&db_path)?;
    let acp_runtime = Arc::new(AcpRuntimeClient::with_terminal_host_and_observability(
        bootstrap_config_service,
        terminal_host,
        observability,
    ));
    let acp_config_service = ProviderConfigService::new(db_path.clone())
        .with_profile_change_listener(acp_runtime.clone())
        .with_profile_change_listener(profile_change_listener);
    let acp_provider = Arc::new(AcpAgentProvider::with_config_service(
        acp_runtime.clone(),
        acp_config_service.clone(),
    ));
    for definition in vibex_core::builtin_agent_definitions() {
        if definition.runtime_kind == AgentRuntimeKind::Acp {
            manager.register_runtime(
                acp_runtime.route_key_for_agent(&definition.id),
                acp_provider.clone(),
            )?;
        }
    }
    Ok((manager, acp_config_service, acp_runtime))
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AcpAdapterId, AgentAuthContextStatus, AgentAuthModelCatalogSnapshot,
        AgentAuthModelCatalogStatus, AgentModelDiscoverySource, AgentSessionSafety,
        AgentSessionState, BindingState, NativeStateHomeId, ProviderKind, ProviderOptions,
        ProviderProfileCreateRequest, ProviderProfileDeleteRequest, RuntimeBinding,
        SessionRuntimeConfigState, SessionRuntimeSelection, TerminalStatus, TransportKind,
        WorkspaceMode,
    };
    use vibex_db::{
        AgentAuthContextRepository, AgentAuthModelCatalogRepository, AgentSessionRuntimeRepository,
        SessionRepository,
    };

    #[test]
    fn managed_agent_bootstrap_stays_off_the_runtime_ready_path() {
        let source = include_str!("lib.rs");
        let start = source
            .split_once("    pub async fn start(config: DesktopRuntimeConfig)")
            .and_then(|(_, tail)| tail.split_once("\n    async fn activate("))
            .map(|(body, _)| body)
            .expect("runtime start should remain inspectable");
        let activate = source
            .split_once("    async fn activate(")
            .and_then(|(_, tail)| tail.split_once("\n    fn spawn_agent_bootstrap("))
            .map(|(body, _)| body)
            .expect("runtime activation should remain inspectable");
        let bootstrap = source
            .split_once("    fn spawn_agent_bootstrap(")
            .and_then(|(_, tail)| tail.split_once("\n    fn spawn_event_bridges("))
            .map(|(body, _)| body)
            .expect("background Agent bootstrap should remain inspectable");
        let manager = source
            .split_once("fn build_agent_manager(")
            .and_then(|(_, tail)| tail.split_once("\n#[cfg(test)]"))
            .map(|(body, _)| body)
            .expect("agent manager construction should remain inspectable");
        let provider_consumer = source
            .split_once("    fn spawn_provider_config_consumer(")
            .and_then(|(_, tail)| tail.split_once("\n    pub fn config("))
            .map(|(body, _)| body)
            .expect("Provider mutation consumer should remain inspectable");

        assert!(start.contains("runtime.activate().await?;"));
        assert!(start.contains("runtime.spawn_agent_bootstrap()?;"));
        assert!(
            start.find("runtime.activate().await?;")
                < start.find("runtime.spawn_agent_bootstrap()?;")
        );
        assert!(!activate.contains("refresh_missing().await"));
        assert!(!manager.contains("install_service.install"));
        assert!(bootstrap.contains("install_service.bootstrap_agent_ids()"));
        assert!(bootstrap.contains("install_service.ensure_installed(agent_id)"));
        assert!(!bootstrap.contains("refresh_missing"));
        assert!(bootstrap.contains("agent.added"));
        assert!(bootstrap.contains("agent.enabled"));
        assert!(bootstrap.contains("agent.installed"));
        assert!(bootstrap.contains("auth_contexts.supports_agent_account(&agent.id)"));
        assert!(bootstrap.contains("auth_contexts.ensure_default(&agent.id)"));
        assert!(
            bootstrap.contains("context.status != vibex_core::AgentAuthContextStatus::Unverified")
        );
        assert!(
            bootstrap.find("auth_contexts.ensure_default(&agent.id)")
                < bootstrap.find("runtime_catalog.probe_missing_enabled_agents().await")
        );
        assert!(bootstrap.contains("runtime_catalog.probe_missing_enabled_agents().await"));
        assert!(bootstrap.contains("probe_missing_enabled_profile_models().await"));
        assert!(bootstrap.contains("ProviderConfigChangePhase::RuntimeOptionsChanged"));
        assert!(!provider_consumer.contains("refresh_profile"));
        assert!(!provider_consumer.contains("invalidate_profile_snapshot"));
        assert!(provider_consumer.contains("probe_profile_models"));
        assert!(provider_consumer.contains("RuntimeOptionsChanged"));
    }

    #[test]
    fn startup_logs_use_stable_stage_boundaries_and_durations() {
        let source = include_str!("lib.rs");
        assert!(source.contains("vibex-startup: stage-begin stage={stage}"));
        assert!(
            source.contains("stage-end stage={stage} result=success duration_ms={duration_ms}")
        );
        assert!(source.contains("result=failed error_code={} duration_ms={duration_ms}"));
        for stage in [
            "runtime_config_validate",
            "runtime_home_prepare",
            "agent_service_initialize",
            "runtime_activate",
            "worktree_reconcile",
            "runtime_lifecycle_start",
            "runtime_probe_reconcile",
            "remote_gateway_start",
            "remote_connectivity_reconcile",
            "runtime_selection_reconcile",
            "message_submission_reconcile",
        ] {
            assert!(source.contains(stage), "missing startup stage {stage}");
        }
    }

    #[test]
    fn agent_reconciliation_preheats_after_runtime_activation_without_blocking_ready() {
        let source = include_str!("lib.rs");
        let start = source
            .split_once("    async fn start_inner(")
            .and_then(|(_, tail)| tail.split_once("\n    async fn activate("))
            .map(|(body, _)| body)
            .expect("runtime start should remain inspectable");
        let activate = source
            .split_once("    async fn activate_inner(")
            .and_then(|(_, tail)| tail.split_once("\n    fn spawn_startup_reconciliation("))
            .map(|(body, _)| body)
            .expect("blocking runtime activation should remain inspectable");
        let background = source
            .split_once("    fn spawn_startup_reconciliation(")
            .and_then(|(_, tail)| tail.split_once("\n    fn spawn_agent_bootstrap("))
            .map(|(body, _)| body)
            .expect("background startup reconciliation should remain inspectable");

        assert!(start.contains("runtime.activate().await?;"));
        assert!(start.contains("runtime.spawn_startup_reconciliation()"));
        assert!(!activate.contains("runtime_selection.reconcile_on_startup()"));
        assert!(!activate.contains("message_submission.reconcile_on_startup()"));
        assert!(background.contains("tasks.push(tokio::spawn(async move"));
        assert!(background.contains("runtime_selection.reconcile_on_startup()"));
        assert!(background.contains("message_submission.reconcile_on_startup()"));
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn isolated_runtime_uses_one_home_lock_and_releases_it_on_shutdown() {
        let dir = tempfile::tempdir().unwrap();
        let config = DesktopRuntimeConfig::isolated_test(dir.path());
        let runtime = DesktopRuntime::start(config.clone()).await.unwrap();
        let error = match DesktopRuntime::start(config.clone()).await {
            Ok(_) => panic!("second runtime unexpectedly acquired the same home"),
            Err(error) => error,
        };
        assert_eq!(error.code, "desktop_runtime_home_locked");
        assert!(runtime.workspace().list().unwrap().is_empty());
        runtime.shutdown().await.unwrap();
        let replacement = DesktopRuntime::start(config).await.unwrap();
        replacement.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn live_authentication_failure_invalidates_only_the_current_account_revision() {
        let home = tempfile::tempdir().unwrap();
        let config = DesktopRuntimeConfig::isolated_test(home.path());
        let agent_id = vibex_core::AgentId::parse("codex").unwrap();
        ProviderConfigService::new(&config.database_path)
            .update_agent_config(vibex_core::AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(false),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        let runtime = DesktopRuntime::start(config.clone()).await.unwrap();
        let service = runtime.agent().auth_contexts();
        let initial = service.ensure_default(&agent_id).unwrap();
        let now = vibex_core::unix_timestamp_ms();
        let mut conn = open_database(&config.database_path).unwrap();
        let authenticated = AgentAuthContextRepository::compare_and_set(
            &conn,
            &initial.id,
            initial.revision,
            AgentAuthContextStatus::Authenticated,
            None,
            Some("browser-login"),
            Some(now),
            false,
        )
        .unwrap();
        let (_project, workspace) = WorkspaceRepository::ensure(
            &conn,
            home.path().join("workspace"),
            WorkspaceMode::CurrentCheckout,
        )
        .unwrap();
        let session = AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: "Authentication failure".to_string(),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: agent_id.clone(),
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        let selection =
            SessionRuntimeSelection::agent_default(agent_id.clone(), authenticated.id.clone());
        let binding = RuntimeBinding {
            binding_id: vibex_core::RuntimeBindingId::new(),
            session_id: session.id.clone(),
            agent_id,
            transport_kind: TransportKind::Acp,
            auth_source: selection.auth_source.clone(),
            auth_source_revision: authenticated.revision,
            adapter_id: AcpAdapterId::parse("codex-acp").unwrap(),
            adapter_version: "test-v1".to_string(),
            adapter_compatibility_identity: "codex-acp@test".to_string(),
            native_session_id: Some("native-test".to_string()),
            native_state_home_id: NativeStateHomeId::new(),
            provider_resume_identity: None,
            process_spawn_fingerprint: "spawn-test".to_string(),
            session_runtime_config_state: SessionRuntimeConfigState::default(),
            capability_snapshot: None,
            restore_compatibility_key: None,
            last_context_sequence: 0,
            last_summary_sequence: 0,
            context_bridge_version: 0,
            activation_generation: 1,
            binding_state: BindingState::Current,
            created_by_switch_id: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        AgentSessionRuntimeRepository::initialize_runtime_selection(
            &mut conn, &binding, &selection,
        )
        .unwrap();
        AgentAuthModelCatalogRepository::upsert(
            &conn,
            &AgentAuthModelCatalogSnapshot {
                auth_context_id: authenticated.id.clone(),
                auth_context_revision: authenticated.revision,
                runtime_fingerprint: "spawn-test".to_string(),
                discovery_source: AgentModelDiscoverySource::AgentDefault,
                status: AgentAuthModelCatalogStatus::AgentDefaultOnly,
                models: Vec::new(),
                last_success_at_ms: Some(now),
                last_attempt_at_ms: now,
                last_error_code: None,
            },
        )
        .unwrap();
        drop(conn);

        let mut changes = service.subscribe_changes();
        assert!(
            service
                .handle_timeline_authentication_required(
                    &session.id,
                    "provider_authentication_required",
                )
                .await
                .unwrap()
        );
        let changed = tokio::time::timeout(std::time::Duration::from_secs(1), async {
            loop {
                match changes.recv().await {
                    Ok(changed) if changed.auth_context_id == authenticated.id => break changed,
                    Ok(_) => continue,
                    Err(error) => panic!("authentication context change stream closed: {error}"),
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(changed.auth_context_id, authenticated.id);
        let current = service.get(&authenticated.id).unwrap();
        assert_eq!(
            current.status,
            AgentAuthContextStatus::AuthenticationRequired
        );
        assert_eq!(current.revision, authenticated.revision + 1);
        let conn = open_database(&config.database_path).unwrap();
        assert!(
            AgentAuthModelCatalogRepository::get(
                &conn,
                &authenticated.id,
                authenticated.revision,
                "spawn-test",
            )
            .unwrap()
            .is_none()
        );
        drop(conn);
        assert!(
            !service
                .handle_timeline_authentication_required(
                    &session.id,
                    "provider_authentication_required",
                )
                .await
                .unwrap()
        );
        assert_eq!(
            service.get(&authenticated.id).unwrap().revision,
            current.revision
        );

        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn provider_mutations_publish_only_profile_changes() {
        let home = tempfile::tempdir().unwrap();
        let config = DesktopRuntimeConfig::isolated_test(home.path());
        let provider_config = ProviderConfigService::new(&config.database_path);
        for agent_id in ["claude", "codex"] {
            provider_config
                .update_agent_config(vibex_core::AgentUpdateConfigRequest {
                    agent_id: vibex_core::AgentId::parse(agent_id).unwrap(),
                    added: Some(false),
                    enabled: Some(false),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                })
                .unwrap();
        }
        let runtime = DesktopRuntime::start(config).await.unwrap();
        let mut events = runtime.subscribe();
        let profile = runtime
            .management()
            .providers()
            .management()
            .create_profile(ProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse("codex").unwrap()),
                kind: ProviderKind::Codex,
                display_name: "Evented provider".to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions::empty()),
                secret_references: Vec::new(),
            })
            .unwrap();

        let profiles_changed = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let DesktopEvent::ProviderConfigChanged(event) = events.recv().await.unwrap()
                    && event.phase == ProviderConfigChangePhase::ProfilesChanged
                {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(
            profiles_changed.provider_profile_ids,
            vec![profile.id.clone()]
        );

        let runtime_options_changed =
            tokio::time::timeout(std::time::Duration::from_millis(150), async {
                loop {
                    if let DesktopEvent::ProviderConfigChanged(event) = events.recv().await.unwrap()
                        && event.phase == ProviderConfigChangePhase::RuntimeOptionsChanged
                    {
                        break event;
                    }
                }
            })
            .await;
        assert!(runtime_options_changed.is_err());

        runtime
            .management()
            .providers()
            .management()
            .delete_profile(ProviderProfileDeleteRequest {
                provider_profile_id: profile.id.clone(),
            })
            .unwrap();
        let deleted = tokio::time::timeout(std::time::Duration::from_secs(2), async {
            loop {
                if let DesktopEvent::ProviderConfigChanged(event) = events.recv().await.unwrap()
                    && event.phase == ProviderConfigChangePhase::ProfilesChanged
                {
                    break event;
                }
            }
        })
        .await
        .unwrap();
        assert_eq!(deleted.provider_profile_ids, vec![profile.id]);
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn desktop_runtime_owns_remote_gateway_listener_lifecycle() {
        let home = tempfile::tempdir().unwrap();
        let mut config = DesktopRuntimeConfig::isolated_test(home.path());
        config.remote_gateway = RemoteGatewayConfig::loopback_enabled("127.0.0.1:0");

        let runtime = DesktopRuntime::start(config).await.unwrap();
        let gateway = runtime.remote().gateway();
        let status = gateway.status();
        assert!(status.running);
        let address = status.bound_addr.unwrap();
        assert!(tokio::net::TcpStream::connect(address).await.is_ok());

        runtime.shutdown().await.unwrap();
        assert!(!gateway.status().running);
        assert!(tokio::net::TcpStream::connect(address).await.is_err());
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_facade_persists_and_lists_composer_sessions() {
        let home = tempfile::tempdir().unwrap();
        let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home.path()))
            .await
            .unwrap();
        let connection = open_database(&runtime.config.database_path).unwrap();
        let (_, workspace) =
            WorkspaceRepository::ensure(&connection, home.path(), WorkspaceMode::CurrentCheckout)
                .unwrap();
        let workspace_id = workspace.id;
        let terminal = runtime
            .create_terminal(
                home.path(),
                TerminalCreateRequest {
                    workspace_id: workspace_id.clone(),
                    title: Some("Composer terminal".into()),
                    shell: None,
                    cwd: None,
                    rows: 24,
                    cols: 80,
                },
            )
            .unwrap();
        let listed = runtime.list_terminals(&workspace_id).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, terminal.id);
        assert_eq!(listed[0].title, "Composer terminal");
        let switched = runtime
            .switch_terminal_shell(&TerminalSwitchShellRequest {
                terminal_id: terminal.id.clone(),
                shell: terminal.shell.clone(),
            })
            .unwrap();
        assert_eq!(switched.id, terminal.id);
        assert_eq!(switched.shell, terminal.shell);
        let killed = runtime.kill_terminal(&terminal.id).unwrap();
        assert_eq!(killed.status, TerminalStatus::Killed);
        let listed = runtime.list_terminals(&workspace_id).unwrap();
        assert!(listed.is_empty());
        runtime.shutdown().await.unwrap();
    }

    #[tokio::test(flavor = "multi_thread", worker_threads = 2)]
    async fn terminal_facade_restores_persisted_open_sessions_before_listing_them() {
        let home = tempfile::tempdir().unwrap();
        let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home.path()))
            .await
            .unwrap();
        let connection = open_database(&runtime.config.database_path).unwrap();
        let (_, workspace) =
            WorkspaceRepository::ensure(&connection, home.path(), WorkspaceMode::CurrentCheckout)
                .unwrap();
        #[cfg(target_os = "windows")]
        let shell = std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string());
        #[cfg(not(target_os = "windows"))]
        let shell = std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".to_string());
        let now = vibex_core::unix_timestamp_ms();
        let stored = TerminalSession {
            id: TerminalId::new(),
            workspace_id: workspace.id.clone(),
            title: "Restored Composer terminal".into(),
            shell,
            cwd: home.path().to_string_lossy().into_owned(),
            rows: 18,
            cols: 100,
            status: TerminalStatus::Running,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        TerminalSessionRepository::upsert(&connection, &stored).unwrap();

        let listed = runtime.list_terminals(&workspace.id).unwrap();

        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id, stored.id);
        assert_eq!(listed[0].status, TerminalStatus::Running);
        assert!(
            runtime
                .terminals()
                .manager()
                .raw_snapshot(&stored.id)
                .is_ok()
        );
        runtime.shutdown().await.unwrap();
    }

    #[test]
    fn preview_home_and_identity_are_isolated_from_stable() {
        let preview = DesktopRuntimeConfig::isolated_preview("/tmp/vibex-home");
        assert_eq!(preview.application_id, PREVIEW_APP_ID);
        assert!(preview.home_dir.ends_with(PREVIEW_HOME_DIRECTORY));
        assert!(preview.database_path.starts_with(&preview.home_dir));
    }

    #[test]
    fn default_preview_configuration_is_isolated_and_enables_raw_terminal_bytes() {
        let preview = DesktopRuntimeConfig::preview_default().unwrap();
        assert_eq!(preview.mode, DesktopRuntimeMode::Preview);
        assert_eq!(preview.application_id, PREVIEW_APP_ID);
        assert!(preview.home_dir.ends_with(PREVIEW_HOME_DIRECTORY));

        let manager = TerminalManager::with_raw_observation_capacity(
            NATIVE_TERMINAL_RING_CAPACITY,
            NATIVE_TERMINAL_RAW_CAPACITY_BYTES,
        );
        assert_eq!(
            manager.raw_observation_capacity(),
            Some(NATIVE_TERMINAL_RAW_CAPACITY_BYTES)
        );
    }

    #[test]
    fn release_candidate_configuration_is_explicitly_isolated() {
        let rc = DesktopRuntimeConfig::isolated_release_candidate("/tmp/vibex-home");
        assert_eq!(rc.mode, DesktopRuntimeMode::ReleaseCandidate);
        assert_eq!(rc.application_id, RC_APP_ID);
        assert!(rc.home_dir.ends_with(RC_HOME_DIRECTORY));
        assert!(!rc.home_dir.ends_with(PREVIEW_HOME_DIRECTORY));
        assert!(rc.database_path.starts_with(&rc.home_dir));
    }

    #[test]
    fn release_stable_configuration_uses_a_transferred_copy_not_tauri_home() {
        let stable = DesktopRuntimeConfig::isolated_release_stable("/tmp/vibex-home");
        assert_eq!(stable.mode, DesktopRuntimeMode::ReleaseStable);
        assert_eq!(stable.application_id, STABLE_DESKTOP_APP_ID);
        assert!(stable.home_dir.ends_with(RELEASE_STABLE_HOME_DIRECTORY));
        assert!(!stable.home_dir.ends_with(PREVIEW_HOME_DIRECTORY));
        assert!(!stable.home_dir.ends_with(RC_HOME_DIRECTORY));
        assert!(stable.database_path.starts_with(&stable.home_dir));
    }

    #[test]
    fn runtime_configuration_rejects_parent_path_traversal() {
        let mut config = DesktopRuntimeConfig::isolated_test("/tmp/vibex-foundation-home");
        config.database_path = config.home_dir.join("nested/../outside.db");
        let error = config.validate().unwrap_err();
        assert_eq!(error.code, "desktop_runtime_path_traversal");
    }
}

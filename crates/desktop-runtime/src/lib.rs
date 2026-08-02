//! Shared desktop composition root used by the native GPUI shell.

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
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

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
    AcpAgentProvider, AcpCompatibilityRegistry, AcpRuntimeClient, AcpRuntimeLifecycleBackend,
    AcpRuntimeSwitchBridge, ManagedAcpAdapterStore,
};
use vibex_config_switch::{ProviderConfigService, ProviderProfileChangeListener};
use vibex_core::{
    AgentCommandConfig, AgentRefreshSnapshotRequest, AgentRuntimeKind, AgentSession,
    FetchTimelineRequest, OpenWorkspaceRequest, ProjectId, ProjectRecord, ProviderProfileId,
    TerminalCreateRequest, TerminalId, TerminalSession, TerminalSwitchShellRequest, TimelinePage,
    VibexError, VibexResult, WorkspaceId, WorkspaceMode, WorkspaceRecord,
};
use vibex_db::{
    TerminalSessionRepository, WorkspaceRepository, apply_migrations, default_database_path,
    open_database,
};
use vibex_desktop_model::DesktopPollingPolicy;
use vibex_diagnostics::{DiagnosticBundleService, DiagnosticBundleServiceConfig};
use vibex_remote::{
    RemoteDispatcher, RemoteGateway, RemoteGatewayConfig, RemoteRouter, RemoteServiceConfig,
    RemoteWorkbenchRuntime, RemoteWorktreeSnapshotSource, build_router_with_dispatcher,
};
use vibex_terminal::TerminalManager;

pub use catalog::{
    RuntimeOptionCatalogService, RuntimeOptionRefreshResult, RuntimeOptionSnapshotSummary,
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
    DIRECT_LOOPBACK_BIND_ADDR, DIRECT_LOOPBACK_TARGET, DirectProbeInfo, DirectPublicationProbe,
    DirectSettings, HttpDirectPublicationProbe, HttpRelayPublicationProbe, MAX_DIRECT_CANDIDATES,
    ProcessOutput, ProcessRunner, REMOTE_ACCESS_SETTINGS_FILE, REMOTE_CONNECTIVITY_SCHEMA_VERSION,
    RelayPublicationFeatures, RelayPublicationInfo, RelayPublicationProbe, RelaySettings,
    RemoteConnectivityController, RemoteConnectivityLoad, RemoteConnectivityMethod,
    RemoteConnectivitySettingsV1, RemoteConnectivitySnapshot, RemoteConnectivityStore,
    RemoteMethodSnapshot, RemoteMethodState, RemoteRecoveryAction, RemoteRouteOwnership,
    RemoteTransitionKind, RemoteTransitionRecord, TAILSCALE_DEFAULT_PORT, TAILSCALE_FALLBACK_PORTS,
    TailscaleCli, TailscaleInspection, TailscalePublication, TailscaleRoute, TailscaleSettings,
    TokioProcessRunner, WebAssetResolver, WebBuildDescriptor, normalize_https_origin,
    parse_tailscale_inspection,
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
    mutation_guard: ManagementMutationGuard,
}

impl ProviderHandle {
    pub fn service(&self) -> ProviderConfigService {
        self.service.clone()
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
        config.validate()?;
        std::fs::create_dir_all(&config.home_dir).map_err(|error| {
            VibexError::storage(
                "desktop_runtime_home_create_failed",
                "failed to create desktop runtime home",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })?;
        let home_lock = if config.acquire_home_lock {
            Some(DesktopHomeLock::acquire(
                &config.home_dir,
                &config.application_id,
            )?)
        } else {
            None
        };
        let observability = Arc::new(RuntimeObservability::new());
        let (provider_change_sender, provider_change_receiver) = mpsc::unbounded_channel();
        let provider_change_listener = Arc::new(DesktopProviderProfileChangeListener {
            sender: provider_change_sender,
        });
        let (manager, provider_config_service, acp_runtime) =
            build_agent_manager(&config, observability.clone(), provider_change_listener).await?;
        let manager = Arc::new(manager);
        let db_path = manager.database_path().to_path_buf();
        let usage = AgentUsageService::new(db_path.clone())?;
        let (usage_sender, usage_receiver) = mpsc::unbounded_channel();
        manager.install_usage_telemetry_sender(usage_sender)?;
        let runtime_switch_bridge = Arc::new(AcpRuntimeSwitchBridge::new(
            &db_path,
            acp_runtime,
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
        let runtime_catalog = Arc::new(RuntimeOptionCatalogService::new(
            manager.clone(),
            provider_config_service.clone(),
        ));
        let terminals = TerminalManager::with_raw_observation_capacity(
            NATIVE_TERMINAL_RING_CAPACITY,
            NATIVE_TERMINAL_RAW_CAPACITY_BYTES,
        );
        let git = GitHandle {
            db_path: db_path.clone(),
            mutation_claims: Arc::new(Mutex::new(std::collections::BTreeSet::new())),
            worktrees: Arc::new(WorktreeCoordinator::new(db_path.clone())),
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
        .with_runtime_option_catalog_source(runtime_catalog.clone());
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
            },
            providers: ProviderHandle {
                service: provider_config_service,
                mutation_guard: ManagementMutationGuard::default(),
            },
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
        runtime.spawn_usage_consumer(usage_receiver)?;
        runtime.spawn_provider_config_consumer(provider_change_receiver)?;
        runtime.activate().await?;
        Ok(runtime)
    }

    async fn activate(self: &Arc<Self>) -> VibexResult<()> {
        if let Err(error) = self.git.reconcile_worktrees_on_startup() {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "managed worktree startup reconciliation failed"
            );
        }
        self.agent
            .runtime_lifecycle
            .start(&tokio::runtime::Handle::current())?;
        if let Err(error) = self.agent.runtime_catalog.refresh_missing().await {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "initial ACP runtime option snapshot probe failed"
            );
        }
        if let Err(error) = self.remote.gateway.start().await {
            let _ = self.agent.runtime_lifecycle.stop().await;
            return Err(error);
        }
        if let Err(error) = self.remote.connectivity.reconcile_on_startup().await {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "remote connectivity startup reconciliation failed"
            );
        }
        if let Err(error) = self.spawn_event_bridges() {
            let _ = self.remote.gateway.stop().await;
            let _ = self.agent.runtime_lifecycle.stop().await;
            return Err(error);
        }
        if let Err(error) = self.agent.runtime_selection.reconcile_on_startup().await {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "runtime selection startup reconciliation failed"
            );
        } else if let Err(error) = self.agent.message_submission.reconcile_on_startup() {
            tracing::warn!(
                target: "vibex_desktop",
                error_code = %error.code,
                "message submission startup reconciliation failed"
            );
        }
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
        tasks.push(tokio::spawn(async move {
            loop {
                match timeline.recv().await {
                    Ok(event) => {
                        let _ = timeline_events.send(DesktopEvent::Timeline(event));
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
        let (refresh_sender, mut refresh_receiver) = mpsc::unbounded_channel();
        let catalog = self.agent.runtime_catalog.clone();
        let profile_events = self.events.clone();
        let profile_gateway = self.remote.gateway.clone();
        tasks.push(tokio::spawn(async move {
            while let Some(first) = receiver.recv().await {
                let mut changes = vec![first];
                while let Ok(change) = receiver.try_recv() {
                    changes.push(change);
                }
                let mut changed_profile_ids = BTreeSet::new();
                let mut refresh_profile_ids = BTreeSet::new();
                for change in changes {
                    match change {
                        ProviderProfileMutationEvent::Saved(provider_profile_id) => {
                            refresh_profile_ids.insert(provider_profile_id.clone());
                            changed_profile_ids.insert(provider_profile_id);
                        }
                        ProviderProfileMutationEvent::Deleted(provider_profile_id) => {
                            refresh_profile_ids.remove(&provider_profile_id);
                            changed_profile_ids.insert(provider_profile_id);
                        }
                    }
                }
                for provider_profile_id in &changed_profile_ids {
                    if let Err(error) = catalog.invalidate_profile_snapshot(provider_profile_id) {
                        tracing::warn!(
                            target: "vibex_desktop",
                            provider_profile_id = %provider_profile_id,
                            error_code = %error.code,
                            "Provider runtime option snapshot invalidation failed"
                        );
                    }
                }
                if !changed_profile_ids.is_empty() {
                    let provider_profile_ids = changed_profile_ids.into_iter().collect();
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
                for provider_profile_id in refresh_profile_ids {
                    let _ = refresh_sender.send(provider_profile_id);
                }
            }
        }));

        let catalog = self.agent.runtime_catalog.clone();
        let runtime_option_events = self.events.clone();
        let runtime_option_gateway = self.remote.gateway.clone();
        tasks.push(tokio::spawn(async move {
            let mut pending = BTreeSet::new();
            loop {
                if pending.is_empty() {
                    let Some(provider_profile_id) = refresh_receiver.recv().await else {
                        break;
                    };
                    pending.insert(provider_profile_id);
                }
                tokio::task::yield_now().await;
                while let Ok(provider_profile_id) = refresh_receiver.try_recv() {
                    pending.insert(provider_profile_id);
                }
                let Some(provider_profile_id) = pending.pop_first() else {
                    continue;
                };
                if let Err(error) = catalog.refresh_profile(&provider_profile_id).await {
                    tracing::warn!(
                        target: "vibex_desktop",
                        provider_profile_id = %provider_profile_id,
                        error_code = %error.code,
                        "Provider runtime option background refresh failed"
                    );
                }
                let _ = runtime_option_events.send(DesktopEvent::ProviderConfigChanged(
                    ProviderConfigChangedEvent {
                        provider_profile_ids: vec![provider_profile_id],
                        phase: ProviderConfigChangePhase::RuntimeOptionsChanged,
                    },
                ));
                if let Err(error) = runtime_option_gateway.publish_provider_invalidation() {
                    tracing::warn!(
                        target: "vibex_desktop",
                        error_code = %error.code,
                        "Remote Provider runtime option invalidation failed"
                    );
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

async fn build_agent_manager(
    config: &DesktopRuntimeConfig,
    observability: Arc<RuntimeObservability>,
    profile_change_listener: Arc<dyn ProviderProfileChangeListener>,
) -> VibexResult<(AgentManager, ProviderConfigService, Arc<AcpRuntimeClient>)> {
    let db_path = config.database_path.clone();
    let bootstrap_config_service = ProviderConfigService::new(&db_path);
    if config.install_managed_adapters {
        prepare_managed_acp_adapters(&bootstrap_config_service, &db_path).await?;
    }
    let mut manager = AgentManager::new(&db_path)?;
    let acp_runtime = Arc::new(AcpRuntimeClient::new_with_observability(
        bootstrap_config_service,
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

async fn prepare_managed_acp_adapters(
    config_service: &ProviderConfigService,
    db_path: &Path,
) -> VibexResult<()> {
    let managed_root = db_path
        .parent()
        .ok_or_else(|| {
            VibexError::storage(
                "acp_managed_root_parent_missing",
                "Vibex database path has no parent for managed ACP adapters",
            )
        })?
        .join("acp-adapters");
    let store = ManagedAcpAdapterStore::new(managed_root)?;
    let registry = AcpCompatibilityRegistry::builtin()?;

    for descriptor in registry.descriptors() {
        let fallback = descriptor
            .command_variants
            .first()
            .map(|variant| AgentCommandConfig {
                command: variant.bin_name.clone(),
                args: variant.args.clone(),
            })
            .ok_or_else(|| {
                VibexError::validation(
                    "acp_managed_command_variant_missing",
                    "managed ACP descriptor has no launch command variant",
                )
                .with_diagnostic("agentId", descriptor.agent_id.as_str())
            })?;
        let command = match store.install(descriptor).await {
            Ok(installation) => AgentCommandConfig {
                command: installation.command.program.to_string_lossy().into_owned(),
                args: installation.command.args,
            },
            Err(error) => {
                tracing::warn!(
                    target: "vibex_desktop",
                    agent_id = %descriptor.agent_id,
                    adapter_id = %descriptor.adapter_id,
                    error_code = %error.code,
                    "managed ACP adapter installation unavailable; using PATH fallback"
                );
                fallback
            }
        };
        config_service.reconcile_agent_acp_runtime(descriptor.agent_id.clone(), command)?;
        config_service.refresh_agent_snapshot(AgentRefreshSnapshotRequest {
            agent_id: descriptor.agent_id.clone(),
            cwd_scope: None,
        })?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        ProviderKind, ProviderOptions, ProviderProfileCreateRequest, ProviderProfileDeleteRequest,
        TerminalStatus, WorkspaceMode,
    };

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
    async fn provider_mutations_publish_static_then_runtime_option_invalidations() {
        let home = tempfile::tempdir().unwrap();
        let runtime = DesktopRuntime::start(DesktopRuntimeConfig::isolated_test(home.path()))
            .await
            .unwrap();
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
            tokio::time::timeout(std::time::Duration::from_secs(2), async {
                loop {
                    if let DesktopEvent::ProviderConfigChanged(event) = events.recv().await.unwrap()
                        && event.phase == ProviderConfigChangePhase::RuntimeOptionsChanged
                    {
                        break event;
                    }
                }
            })
            .await
            .unwrap();
        assert_eq!(
            runtime_options_changed.provider_profile_ids,
            vec![profile.id.clone()]
        );

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

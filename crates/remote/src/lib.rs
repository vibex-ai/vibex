use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use async_trait::async_trait;
use axum::extract::State;
use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::response::IntoResponse;
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::StreamExt;
use sha2::{Digest, Sha256};
use vibex_agent::{
    AgentManager, MessageSubmissionCoordinator, RuntimeLifecycleService, RuntimeSelectionService,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentSessionState, DeviceId, ErrorCategory, EventId, FetchTimelineRequest,
    OpenWorkspaceRequest, ProjectWorkspaceSummary, RemoteActionClass,
    RemoteAgentAttachRuntimeResponse, RemoteAgentCancelRuntimeSwitchResponse,
    RemoteAgentCatchUpRequest, RemoteAgentCatchUpResponse, RemoteAgentContinueTurnResponse,
    RemoteAgentDeepLinkResolveResponse, RemoteAgentDetachRuntimeResponse,
    RemoteAgentInterruptResponse, RemoteAgentMessageSubmissionResponse, RemoteAgentRequest,
    RemoteAgentResolveElicitationResponse, RemoteAgentResolvePermissionResponse,
    RemoteAgentRuntimeEventsResponse, RemoteAgentRuntimeOptionsResponse,
    RemoteAgentRuntimeProcessSnapshotResponse, RemoteAgentRuntimeSelectionResponse,
    RemoteAgentRuntimeSnapshotResponse, RemoteAgentSendMessageResponse,
    RemoteAgentSessionDetailResponse, RemoteAgentSessionListResponse,
    RemoteAgentSetDesiredRuntimeResponse, RemoteAgentTimelineCursor,
    RemoteAgentTimelineFetchResponse, RemoteAuditAction, RemoteAuditOutcome, RemoteAuditRecord,
    RemoteAuditTargetKind, RemoteAuthContext, RemoteAuthProof, RemoteCapabilitySummary,
    RemoteClaimPairingCodeRequest, RemoteClaimPairingCodeResponse, RemoteCreatePairingCodeRequest,
    RemoteCreatePairingCodeResponse, RemoteDeepLinkResolution, RemoteDeepLinkResolutionStatus,
    RemoteDeviceDetail, RemoteDevicePermissionLevel, RemoteDeviceStatus, RemoteFileDeleteResponse,
    RemoteFileReadResponse, RemoteFileRenameResponse, RemoteFileSearchResponse,
    RemoteFileTreeResponse, RemoteFileWriteResponse, RemoteGitBlameResponse,
    RemoteGitBranchListResponse, RemoteGitCommitDetailResponse, RemoteGitCommitResponse,
    RemoteGitDiffResponse, RemoteGitHistoryResponse, RemoteGitRemoteActionResponse,
    RemoteGitStatusMutationResponse, RemoteGitStatusResponse, RemoteGitWorktreeEligibilityResponse,
    RemoteGitWorktreeSnapshotResponse, RemoteHandshakeResponse, RemoteHealthState,
    RemoteHealthStatus, RemoteLiveEventChannel, RemoteLiveEventEnvelope, RemoteOperationKind,
    RemotePairingCode, RemoteProtocolVersion, RemoteProviderFailoverRecommendationListResponse,
    RemoteProviderHealthSummaryListResponse, RemoteProviderInjectionPreviewResponse,
    RemoteProviderProfileListResponse, RemoteProviderRequest,
    RemoteProviderRunHealthProbesResponse, RemoteProviderUsageSummaryListResponse,
    RemoteRequestEnvelope, RemoteResponseEnvelope, RemoteRevokeDeviceRequest, RemoteServiceInfo,
    RemoteTerminalCreateResponse, RemoteTerminalKillResponse, RemoteTerminalListResponse,
    RemoteTerminalResizeResponse, RemoteTerminalSnapshotResponse, RemoteTerminalWriteResponse,
    RemoteWorkbenchListWorkspacesResponse, RemoteWorkbenchOpenWorkspaceResponse,
    RemoteWorkbenchRequest, RequestId, ResolveElicitationRequest, ResolvePermissionRequest,
    RuntimeLeaseRole, SessionRuntimeOptionCatalog, TerminalSession, TerminalStatus,
    TimelineLiveEvent, VibexError, VibexResult, WorkspaceAggregateStatus, WorkspaceId,
    WorkspaceMode, unix_timestamp_ms,
};
use vibex_db::{
    DbConnection, GitSnapshotRepository, RecentFileRepository, RemoteAuditRepository,
    RemoteDeviceRecord, RemoteDeviceRepository, RemotePairingCodeRecord,
    RemotePairingCodeRepository, SessionRepository, TerminalSessionRepository, WorkspaceRepository,
    apply_migrations, open_database,
};
use vibex_fs::WorkspaceFileService;
use vibex_terminal::TerminalManager;

mod identity;
pub use identity::{RemoteIdentity, RemoteIdentityStore};
mod gateway;
mod pairing_v2;
pub use gateway::{
    RelayAttachmentTasks, RelayRemoteOutbound, RemoteGateway, RemoteGatewayConfig,
    RemoteGatewayDeploymentMode, RemoteGatewayPairingRoutes, RemoteGatewayStatus,
    RemoteGatewayTlsPolicy, RemoteGatewayWebBuildDescriptor,
};

pub type RemoteRouter = Router;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteServiceConfig {
    pub enabled: bool,
    pub bind_addr: String,
    pub service_name: String,
    pub server_version: String,
}

impl Default for RemoteServiceConfig {
    fn default() -> Self {
        Self {
            enabled: false,
            bind_addr: "127.0.0.1:0".to_string(),
            service_name: "Vibex Remote".to_string(),
            server_version: env!("CARGO_PKG_VERSION").to_string(),
        }
    }
}

impl RemoteServiceConfig {
    pub fn loopback_disabled() -> Self {
        Self::default()
    }

    pub fn public_listener_enabled(&self) -> bool {
        self.bind_addr
            .split_once(':')
            .and_then(|(host, _)| host.parse::<IpAddr>().ok())
            .is_some_and(|ip| ip.is_unspecified())
    }
}

#[derive(Clone)]
struct RemoteRouterState {
    config: RemoteServiceConfig,
    capabilities: RemoteCapabilitySummary,
    agent_manager: Option<Arc<AgentManager>>,
    runtime_selection: Option<Arc<RuntimeSelectionService>>,
    runtime_lifecycle: Option<Arc<RuntimeLifecycleService>>,
    message_submission: Option<Arc<MessageSubmissionCoordinator>>,
    runtime_catalog: Option<Arc<dyn RemoteRuntimeOptionCatalogSource>>,
    workbench: Option<RemoteWorkbenchRuntime>,
    provider: Option<RemoteProviderRuntime>,
}

#[async_trait]
pub trait RemoteRuntimeOptionCatalogSource: Send + Sync {
    async fn list_runtime_options(&self) -> VibexResult<SessionRuntimeOptionCatalog>;
}

#[async_trait]
pub trait RemoteWorktreeSnapshotSource: Send + Sync {
    async fn worktree_eligibility(
        &self,
        workspace_id: WorkspaceId,
    ) -> VibexResult<vibex_core::GitProjectEligibility>;

    async fn worktree_snapshot(
        &self,
        workspace_id: WorkspaceId,
    ) -> VibexResult<vibex_core::GitWorktreeLifecycleSnapshot>;
}

#[derive(Clone)]
pub struct RemoteDispatcher {
    state: RemoteRouterState,
}

#[derive(Clone)]
pub struct RemoteWorkbenchRuntime {
    db_path: PathBuf,
    terminals: TerminalManager,
    worktrees: Option<Arc<dyn RemoteWorktreeSnapshotSource>>,
}

impl RemoteWorkbenchRuntime {
    pub fn new(db_path: impl Into<PathBuf>, terminals: TerminalManager) -> Self {
        Self {
            db_path: db_path.into(),
            terminals,
            worktrees: None,
        }
    }

    pub fn with_worktree_snapshot_source(
        mut self,
        source: Arc<dyn RemoteWorktreeSnapshotSource>,
    ) -> Self {
        self.worktrees = Some(source);
        self
    }
}

#[derive(Clone)]
pub struct RemoteProviderRuntime {
    db_path: PathBuf,
}

impl RemoteProviderRuntime {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
        }
    }
}

impl RemoteRouterState {
    fn new(config: RemoteServiceConfig) -> Self {
        Self {
            config,
            capabilities: RemoteCapabilitySummary::foundation(),
            agent_manager: None,
            runtime_selection: None,
            runtime_lifecycle: None,
            message_submission: None,
            runtime_catalog: None,
            workbench: None,
            provider: None,
        }
    }

    fn with_agent_manager(config: RemoteServiceConfig, agent_manager: Arc<AgentManager>) -> Self {
        Self {
            config,
            capabilities: RemoteCapabilitySummary::with_agent_sessions(),
            agent_manager: Some(agent_manager),
            runtime_selection: None,
            runtime_lifecycle: None,
            message_submission: None,
            runtime_catalog: None,
            workbench: None,
            provider: None,
        }
    }

    fn with_agent_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        let provider = RemoteProviderRuntime::new(workbench.db_path.clone());
        Self {
            config,
            capabilities: RemoteCapabilitySummary::with_agent_workbench_and_provider(),
            agent_manager: Some(agent_manager),
            runtime_selection: None,
            runtime_lifecycle: None,
            message_submission: None,
            runtime_catalog: None,
            workbench: Some(workbench),
            provider: Some(provider),
        }
    }

    fn with_agent_runtime_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
        message_submission: Arc<MessageSubmissionCoordinator>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        let provider = RemoteProviderRuntime::new(workbench.db_path.clone());
        Self {
            config,
            capabilities: RemoteCapabilitySummary::with_agent_workbench_and_provider(),
            agent_manager: Some(agent_manager),
            runtime_selection: Some(runtime_selection),
            runtime_lifecycle: None,
            message_submission: Some(message_submission),
            runtime_catalog: None,
            workbench: Some(workbench),
            provider: Some(provider),
        }
    }

    fn with_agent_runtime_lifecycle_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
        runtime_lifecycle: Arc<RuntimeLifecycleService>,
        message_submission: Arc<MessageSubmissionCoordinator>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        let mut state = Self::with_agent_runtime_and_workbench(
            config,
            agent_manager,
            runtime_selection,
            message_submission,
            workbench,
        );
        state.runtime_lifecycle = Some(runtime_lifecycle);
        state.capabilities.supports_runtime_lifecycle = true;
        state
    }

    fn health(&self) -> RemoteHealthStatus {
        RemoteHealthStatus {
            status: if self.config.enabled {
                RemoteHealthState::Ok
            } else {
                RemoteHealthState::Disabled
            },
            protocol_version: RemoteProtocolVersion::foundation(),
            service_name: self.config.service_name.clone(),
            checked_at_ms: unix_timestamp_ms(),
        }
    }

    fn info(&self) -> RemoteServiceInfo {
        RemoteServiceInfo {
            service_name: self.config.service_name.clone(),
            server_version: self.config.server_version.clone(),
            protocol_version: RemoteProtocolVersion::foundation(),
            capabilities: self.capabilities.clone(),
            remote_enabled: self.config.enabled,
            bind_addr: self.config.bind_addr.clone(),
            public_listener_enabled: self.config.public_listener_enabled(),
        }
    }
}

impl RemoteDispatcher {
    pub fn new(config: RemoteServiceConfig) -> Self {
        Self {
            state: RemoteRouterState::new(config),
        }
    }

    pub fn with_agent_manager(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
    ) -> Self {
        Self {
            state: RemoteRouterState::with_agent_manager(config, agent_manager),
        }
    }

    pub fn with_agent_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        Self {
            state: RemoteRouterState::with_agent_and_workbench(config, agent_manager, workbench),
        }
    }

    pub fn with_agent_runtime_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
        message_submission: Arc<MessageSubmissionCoordinator>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        Self {
            state: RemoteRouterState::with_agent_runtime_and_workbench(
                config,
                agent_manager,
                runtime_selection,
                message_submission,
                workbench,
            ),
        }
    }

    pub fn with_agent_runtime_lifecycle_and_workbench(
        config: RemoteServiceConfig,
        agent_manager: Arc<AgentManager>,
        runtime_selection: Arc<RuntimeSelectionService>,
        runtime_lifecycle: Arc<RuntimeLifecycleService>,
        message_submission: Arc<MessageSubmissionCoordinator>,
        workbench: RemoteWorkbenchRuntime,
    ) -> Self {
        Self {
            state: RemoteRouterState::with_agent_runtime_lifecycle_and_workbench(
                config,
                agent_manager,
                runtime_selection,
                runtime_lifecycle,
                message_submission,
                workbench,
            ),
        }
    }

    pub fn with_runtime_option_catalog_source(
        mut self,
        source: Arc<dyn RemoteRuntimeOptionCatalogSource>,
    ) -> Self {
        self.state.runtime_catalog = Some(source);
        self.state.capabilities.supports_seamless_runtime_selection =
            self.state.runtime_selection.is_some();
        self
    }

    pub fn health(&self) -> RemoteHealthStatus {
        self.state.health()
    }

    pub fn info(&self) -> RemoteServiceInfo {
        self.state.info()
    }

    pub async fn dispatch(&self, request: RemoteRequestEnvelope) -> RemoteResponseEnvelope {
        handle_request(&self.state, request).await
    }
}

pub fn build_router(config: RemoteServiceConfig) -> RemoteRouter {
    let dispatcher = RemoteDispatcher::new(config);
    build_router_with_dispatcher(dispatcher)
}

pub fn build_router_with_dispatcher(dispatcher: RemoteDispatcher) -> RemoteRouter {
    Router::new()
        .route("/health", get(health))
        .route("/api/info", get(info))
        .route("/api/agent", post(agent))
        .route("/api/workbench", post(workbench))
        .route("/api/provider", post(provider))
        .route("/ws", get(ws))
        .with_state(dispatcher)
}

pub fn build_router_with_agent(
    config: RemoteServiceConfig,
    agent_manager: Arc<AgentManager>,
) -> RemoteRouter {
    let dispatcher = RemoteDispatcher::with_agent_manager(config, agent_manager);
    build_router_with_dispatcher(dispatcher)
}

pub fn build_router_with_agent_and_workbench(
    config: RemoteServiceConfig,
    agent_manager: Arc<AgentManager>,
    workbench_runtime: RemoteWorkbenchRuntime,
) -> RemoteRouter {
    let dispatcher =
        RemoteDispatcher::with_agent_and_workbench(config, agent_manager, workbench_runtime);
    build_router_with_dispatcher(dispatcher)
}

pub fn build_router_with_agent_runtime_lifecycle_and_workbench(
    config: RemoteServiceConfig,
    agent_manager: Arc<AgentManager>,
    runtime_selection: Arc<RuntimeSelectionService>,
    runtime_lifecycle: Arc<RuntimeLifecycleService>,
    message_submission: Arc<MessageSubmissionCoordinator>,
    workbench_runtime: RemoteWorkbenchRuntime,
) -> RemoteRouter {
    let dispatcher = RemoteDispatcher::with_agent_runtime_lifecycle_and_workbench(
        config,
        agent_manager,
        runtime_selection,
        runtime_lifecycle,
        message_submission,
        workbench_runtime,
    );
    build_router_with_dispatcher(dispatcher)
}

pub fn build_default_disabled_router() -> RemoteRouter {
    build_router(RemoteServiceConfig::loopback_disabled())
}

pub struct RemoteTrustService;

impl RemoteTrustService {
    pub const DEFAULT_PAIRING_TTL_MS: u32 = 5 * 60 * 1000;
    pub const MAX_PAIRING_TTL_MS: u32 = 30 * 60 * 1000;

    pub fn create_pairing_code(
        conn: &DbConnection,
        request: RemoteCreatePairingCodeRequest,
    ) -> VibexResult<RemoteCreatePairingCodeResponse> {
        let now = unix_timestamp_ms();
        let ttl_ms = request
            .ttl_ms
            .unwrap_or(Self::DEFAULT_PAIRING_TTL_MS)
            .clamp(1, Self::MAX_PAIRING_TTL_MS);
        let pairing_code = generate_secret("pair");
        let pairing = RemotePairingCode {
            pairing_id: RequestId::new(),
            permission_level: request.permission_level,
            expires_at_ms: now + i64::from(ttl_ms),
            claimed_device_id: None,
            created_at_ms: now,
            claimed_at_ms: None,
        };

        RemotePairingCodeRepository::insert(
            conn,
            &RemotePairingCodeRecord {
                pairing: pairing.clone(),
                code_hash: hash_secret(&pairing_code),
            },
        )?;
        Self::insert_audit(
            conn,
            None,
            RemoteAuditAction::PairingCodeCreated,
            RemoteAuditTargetKind::PairingCode,
            Some(pairing.pairing_id.as_str().to_string()),
            RemoteAuditOutcome::Created,
            format!(
                "Pairing code created for {:?} device permission",
                request.permission_level
            ),
            None,
            None,
        )?;

        Ok(RemoteCreatePairingCodeResponse {
            pairing,
            pairing_code,
        })
    }

    pub fn claim_pairing_code(
        conn: &DbConnection,
        request: RemoteClaimPairingCodeRequest,
    ) -> VibexResult<RemoteClaimPairingCodeResponse> {
        if request.display_name.trim().is_empty() {
            return Err(remote_error(
                "remote_device_display_name_required",
                "remote device display name is required",
            ));
        }

        let code_hash = hash_secret(&request.pairing_code);
        let Some(pairing_record) = RemotePairingCodeRepository::get_by_hash(conn, &code_hash)?
        else {
            Self::insert_audit(
                conn,
                None,
                RemoteAuditAction::PairingCodeRejected,
                RemoteAuditTargetKind::PairingCode,
                None,
                RemoteAuditOutcome::Denied,
                "Pairing claim rejected with invalid code",
                None,
                None,
            )?;
            return Err(remote_error(
                "remote_pairing_code_invalid",
                "pairing code is invalid",
            ));
        };

        if pairing_record.pairing.claimed_at_ms.is_some() {
            Self::insert_audit(
                conn,
                pairing_record.pairing.claimed_device_id.clone(),
                RemoteAuditAction::PairingCodeRejected,
                RemoteAuditTargetKind::PairingCode,
                Some(pairing_record.pairing.pairing_id.as_str().to_string()),
                RemoteAuditOutcome::Denied,
                "Pairing claim rejected because the code was already claimed",
                None,
                None,
            )?;
            return Err(remote_error(
                "remote_pairing_code_invalid",
                "pairing code has already been claimed",
            ));
        }

        let now = unix_timestamp_ms();
        if pairing_record.pairing.expires_at_ms <= now {
            Self::insert_audit(
                conn,
                None,
                RemoteAuditAction::PairingCodeRejected,
                RemoteAuditTargetKind::PairingCode,
                Some(pairing_record.pairing.pairing_id.as_str().to_string()),
                RemoteAuditOutcome::Denied,
                "Pairing claim rejected because the code expired",
                None,
                None,
            )?;
            return Err(remote_error(
                "remote_pairing_code_expired",
                "pairing code has expired",
            ));
        }

        let auth_token = generate_secret("auth");
        let device = RemoteDeviceDetail {
            device_id: DeviceId::new(),
            display_name: request.display_name.trim().to_string(),
            public_key: request.public_key,
            grant_revision: 1,
            permission_level: pairing_record.pairing.permission_level,
            status: RemoteDeviceStatus::Active,
            paired_at_ms: Some(now),
            last_seen_at_ms: Some(now),
            revoked_at_ms: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        let transaction = conn.unchecked_transaction().map_err(|_| {
            VibexError::storage(
                "remote_pairing_transaction_failed",
                "failed to start remote pairing transaction",
            )
        })?;
        RemoteDeviceRepository::upsert(
            &transaction,
            &RemoteDeviceRecord {
                detail: device.clone(),
                auth_secret_hash: hash_secret(&auth_token),
            },
        )?;
        RemotePairingCodeRepository::mark_claimed(
            &transaction,
            &pairing_record.pairing.pairing_id,
            &device.device_id,
            now,
        )?;
        Self::insert_audit(
            &transaction,
            Some(device.device_id.clone()),
            RemoteAuditAction::PairingCodeClaimed,
            RemoteAuditTargetKind::Device,
            Some(device.device_id.as_str().to_string()),
            RemoteAuditOutcome::Allowed,
            format!("Device '{}' paired successfully", device.display_name),
            None,
            None,
        )?;
        transaction.commit().map_err(|_| {
            VibexError::storage(
                "remote_pairing_commit_failed",
                "failed to commit remote pairing claim",
            )
        })?;

        Ok(RemoteClaimPairingCodeResponse { device, auth_token })
    }

    pub fn authenticate(
        conn: &DbConnection,
        proof: RemoteAuthProof,
    ) -> VibexResult<RemoteAuthContext> {
        let Some(device) = RemoteDeviceRepository::get(conn, &proof.device_id)? else {
            return Err(remote_error(
                "remote_device_unknown",
                "remote device is unknown",
            ));
        };

        if device.detail.status == RemoteDeviceStatus::Revoked {
            Self::audit_auth_failure(conn, Some(device.detail.device_id.clone()), "revoked")?;
            return Err(remote_error(
                "remote_device_revoked",
                "remote device is revoked",
            ));
        }

        if device.auth_secret_hash != hash_secret(&proof.auth_token) {
            Self::audit_auth_failure(conn, Some(device.detail.device_id.clone()), "invalid token")?;
            return Err(remote_error(
                "remote_auth_invalid",
                "remote authentication failed",
            ));
        }

        let now = unix_timestamp_ms();
        RemoteDeviceRepository::update_last_seen(conn, &device.detail.device_id, now)?;
        Self::insert_audit(
            conn,
            Some(device.detail.device_id.clone()),
            RemoteAuditAction::DeviceAuthenticated,
            RemoteAuditTargetKind::Device,
            Some(device.detail.device_id.as_str().to_string()),
            RemoteAuditOutcome::Allowed,
            format!("Device '{}' authenticated", device.detail.display_name),
            None,
            None,
        )?;

        Ok(RemoteAuthContext {
            device_id: device.detail.device_id,
            display_name: device.detail.display_name,
            permission_level: device.detail.permission_level,
            authenticated_at_ms: now,
        })
    }

    pub fn revoke_device(
        conn: &DbConnection,
        request: RemoteRevokeDeviceRequest,
    ) -> VibexResult<RemoteDeviceDetail> {
        if RemoteDeviceRepository::get(conn, &request.device_id)?.is_none() {
            return Err(remote_error(
                "remote_device_unknown",
                "remote device is unknown",
            ));
        }

        let revoked =
            RemoteDeviceRepository::revoke(conn, &request.device_id, unix_timestamp_ms())?;
        let reason = request
            .reason
            .as_deref()
            .map(redact_summary)
            .unwrap_or_else(|| "No reason provided".to_string());
        Self::insert_audit(
            conn,
            Some(revoked.detail.device_id.clone()),
            RemoteAuditAction::DeviceRevoked,
            RemoteAuditTargetKind::Device,
            Some(revoked.detail.device_id.as_str().to_string()),
            RemoteAuditOutcome::Revoked,
            format!("Device '{}' revoked: {reason}", revoked.detail.display_name),
            None,
            None,
        )?;
        Ok(revoked.detail)
    }

    pub fn authorize_action(
        conn: &DbConnection,
        auth: &RemoteAuthContext,
        action: RemoteActionClass,
        request_id: Option<RequestId>,
        correlation_id: Option<vibex_core::CorrelationId>,
    ) -> VibexResult<()> {
        let allowed = permission_allows(auth.permission_level, action);
        Self::insert_audit(
            conn,
            Some(auth.device_id.clone()),
            if allowed {
                RemoteAuditAction::PermissionAllowed
            } else {
                RemoteAuditAction::PermissionDenied
            },
            audit_target_for_action(action),
            Some(format!("{action:?}")),
            if allowed {
                RemoteAuditOutcome::Allowed
            } else {
                RemoteAuditOutcome::Denied
            },
            format!(
                "Device '{}' requested {:?}: {}",
                auth.display_name,
                action,
                if allowed { "allowed" } else { "denied" }
            ),
            request_id,
            correlation_id,
        )?;

        if allowed {
            Ok(())
        } else {
            Err(VibexError::new(
                ErrorCategory::Permission,
                "remote_permission_denied",
                "remote device permission level does not allow this action",
            ))
        }
    }

    fn audit_auth_failure(
        conn: &DbConnection,
        device_id: Option<DeviceId>,
        reason: &str,
    ) -> VibexResult<()> {
        Self::insert_audit(
            conn,
            device_id,
            RemoteAuditAction::DeviceAuthFailed,
            RemoteAuditTargetKind::Device,
            None,
            RemoteAuditOutcome::Denied,
            format!("Remote authentication failed: {}", redact_summary(reason)),
            None,
            None,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn insert_audit(
        conn: &DbConnection,
        device_id: Option<DeviceId>,
        action: RemoteAuditAction,
        target_kind: RemoteAuditTargetKind,
        target_id: Option<String>,
        outcome: RemoteAuditOutcome,
        redacted_summary: impl Into<String>,
        request_id: Option<RequestId>,
        correlation_id: Option<vibex_core::CorrelationId>,
    ) -> VibexResult<()> {
        RemoteAuditRepository::insert(
            conn,
            &RemoteAuditRecord {
                audit_id: RequestId::new(),
                device_id,
                action,
                target_kind,
                target_id,
                outcome,
                redacted_summary: redact_summary(&redacted_summary.into()),
                request_id,
                correlation_id,
                created_at_ms: unix_timestamp_ms(),
            },
        )
    }
}

fn generate_secret(prefix: &str) -> String {
    format!("{prefix}-{}", RequestId::new().into_string())
}

fn hash_secret(secret: &str) -> String {
    let digest = Sha256::digest(secret.as_bytes());
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest {
        encoded.push_str(&format!("{byte:02x}"));
    }
    format!("sha256:{encoded}")
}

fn permission_allows(permission: RemoteDevicePermissionLevel, action: RemoteActionClass) -> bool {
    match permission {
        RemoteDevicePermissionLevel::ReadOnly => matches!(
            action,
            RemoteActionClass::ReadProject
                | RemoteActionClass::ReadAgentSession
                | RemoteActionClass::ReadProviderSettings
        ),
        RemoteDevicePermissionLevel::ApproveOnly => matches!(
            action,
            RemoteActionClass::ReadProject
                | RemoteActionClass::ReadAgentSession
                | RemoteActionClass::ResolvePermission
                | RemoteActionClass::ResolveElicitation
                | RemoteActionClass::ReadProviderSettings
        ),
        RemoteDevicePermissionLevel::FullControl => true,
    }
}

fn audit_target_for_action(action: RemoteActionClass) -> RemoteAuditTargetKind {
    match action {
        RemoteActionClass::ReadProject => RemoteAuditTargetKind::System,
        RemoteActionClass::ReadAgentSession | RemoteActionClass::MutateAgentSession => {
            RemoteAuditTargetKind::AgentSession
        }
        RemoteActionClass::ResolvePermission => RemoteAuditTargetKind::Permission,
        RemoteActionClass::ResolveElicitation => RemoteAuditTargetKind::Elicitation,
        RemoteActionClass::MutateFile => RemoteAuditTargetKind::WorkspaceFile,
        RemoteActionClass::MutateGit => RemoteAuditTargetKind::Git,
        RemoteActionClass::MutateTerminal => RemoteAuditTargetKind::Terminal,
        RemoteActionClass::ReadProviderSettings | RemoteActionClass::MutateProviderSettings => {
            RemoteAuditTargetKind::ProviderSettings
        }
        RemoteActionClass::ReadDeviceManagement | RemoteActionClass::MutateDeviceManagement => {
            RemoteAuditTargetKind::Device
        }
    }
}

fn redact_summary(summary: &str) -> String {
    let mut redacted = summary.to_string();
    for marker in ["token", "secret", "password", "pairing", "code", "auth"] {
        redacted = redacted.replace(marker, "[redacted]");
        redacted = redacted.replace(&marker.to_uppercase(), "[redacted]");
    }
    redacted
}

fn remote_error(code: &'static str, message: &'static str) -> VibexError {
    VibexError::new(ErrorCategory::Remote, code, message)
}

async fn health(State(dispatcher): State<RemoteDispatcher>) -> Json<RemoteHealthStatus> {
    Json(dispatcher.health())
}

async fn info(State(dispatcher): State<RemoteDispatcher>) -> Json<RemoteServiceInfo> {
    Json(dispatcher.info())
}

async fn agent(
    State(dispatcher): State<RemoteDispatcher>,
    Json(request): Json<RemoteRequestEnvelope>,
) -> Json<RemoteResponseEnvelope> {
    Json(dispatcher.dispatch(request).await)
}

async fn workbench(
    State(dispatcher): State<RemoteDispatcher>,
    Json(request): Json<RemoteRequestEnvelope>,
) -> Json<RemoteResponseEnvelope> {
    Json(dispatcher.dispatch(request).await)
}

async fn provider(
    State(dispatcher): State<RemoteDispatcher>,
    Json(request): Json<RemoteRequestEnvelope>,
) -> Json<RemoteResponseEnvelope> {
    Json(dispatcher.dispatch(request).await)
}

async fn ws(
    State(dispatcher): State<RemoteDispatcher>,
    upgrade: WebSocketUpgrade,
) -> impl IntoResponse {
    upgrade.on_upgrade(move |socket| handle_socket(socket, dispatcher))
}

async fn handle_socket(mut socket: WebSocket, dispatcher: RemoteDispatcher) {
    while let Some(frame) = socket.next().await {
        let response = match frame {
            Ok(Message::Text(text)) => handle_text_frame(&dispatcher, text.as_str()).await,
            Ok(Message::Binary(bytes)) => {
                handle_text_frame(&dispatcher, std::str::from_utf8(&bytes).unwrap_or("")).await
            }
            Ok(Message::Close(_)) => break,
            Ok(Message::Ping(_)) | Ok(Message::Pong(_)) => continue,
            Err(_) => break,
        };

        let Ok(encoded) = serde_json::to_string(&response) else {
            break;
        };

        if socket.send(Message::Text(encoded.into())).await.is_err() {
            break;
        }
    }
}

async fn handle_text_frame(dispatcher: &RemoteDispatcher, text: &str) -> RemoteResponseEnvelope {
    match serde_json::from_str::<RemoteRequestEnvelope>(text) {
        Ok(request) => dispatcher.dispatch(request).await,
        Err(_) => invalid_envelope_response(),
    }
}

async fn handle_request(
    state: &RemoteRouterState,
    request: RemoteRequestEnvelope,
) -> RemoteResponseEnvelope {
    match request.operation {
        RemoteOperationKind::Handshake => {
            let payload = serde_json::to_value(RemoteHandshakeResponse {
                protocol_version: RemoteProtocolVersion::foundation(),
                server_name: state.config.service_name.clone(),
                server_version: state.config.server_version.clone(),
                capabilities: state.capabilities.clone(),
                server_time_ms: unix_timestamp_ms(),
            })
            .expect("remote handshake response is serializable");

            RemoteResponseEnvelope::ok(request.request_id, request.correlation_id, payload)
        }
        RemoteOperationKind::AgentSession => handle_agent_request(state, request).await,
        RemoteOperationKind::WorkspaceFile
        | RemoteOperationKind::Git
        | RemoteOperationKind::Terminal => handle_workbench_request(state, request).await,
        RemoteOperationKind::ProviderSettings => handle_provider_request(state, request).await,
        _ => unsupported_operation_response(request),
    }
}

async fn handle_agent_request(
    state: &RemoteRouterState,
    request: RemoteRequestEnvelope,
) -> RemoteResponseEnvelope {
    let request_id = request.request_id.clone();
    let correlation_id = request.correlation_id.clone();
    let response = match decode_agent_request(&request) {
        Ok(agent_request) => {
            dispatch_agent_request(state, request_id, correlation_id, agent_request).await
        }
        Err(err) => Err(err),
    };

    match response {
        Ok(payload) => {
            RemoteResponseEnvelope::ok(request.request_id, request.correlation_id, payload)
        }
        Err(error) => {
            RemoteResponseEnvelope::error(request.request_id, request.correlation_id, error)
        }
    }
}

fn decode_agent_request(request: &RemoteRequestEnvelope) -> VibexResult<RemoteAgentRequest> {
    let payload = request.payload.clone().ok_or_else(|| {
        VibexError::validation(
            "remote_agent_payload_missing",
            "remote Agent operation requires a payload",
        )
    })?;

    serde_json::from_value(payload).map_err(|err| {
        VibexError::validation(
            "remote_agent_payload_invalid",
            "remote Agent operation payload is invalid",
        )
        .with_diagnostic("error", err.to_string())
    })
}

fn remote_worktree_path_identity(
    scope: &str,
    id: &str,
    exists: bool,
) -> vibex_core::GitPathIdentity {
    let label = format!("remote:{scope}:{id}");
    vibex_core::GitPathIdentity {
        original_path: label.clone(),
        normalized_path: label.clone(),
        canonical_path: None,
        filesystem_id: None,
        comparison_key: label,
        exists,
    }
}

fn remote_worktree_repository_identity(
    project_id: &vibex_core::ProjectId,
    identity: &vibex_core::GitRepositoryIdentity,
) -> vibex_core::GitRepositoryIdentity {
    vibex_core::GitRepositoryIdentity {
        repository_root: remote_worktree_path_identity(
            "repository",
            project_id.as_str(),
            identity.repository_root.exists,
        ),
        git_common_dir: remote_worktree_path_identity(
            "git-common-dir",
            project_id.as_str(),
            identity.git_common_dir.exists,
        ),
        comparison_key: format!("remote:repository:{}", project_id.as_str()),
    }
}

fn sanitize_remote_worktree_eligibility(
    mut eligibility: vibex_core::GitProjectEligibility,
) -> vibex_core::GitProjectEligibility {
    let project_path_exists = eligibility.project_canonical_path.exists;
    eligibility.project_canonical_path = remote_worktree_path_identity(
        "project",
        eligibility.project_id.as_str(),
        project_path_exists,
    );
    eligibility.repository_identity = eligibility
        .repository_identity
        .take()
        .map(|identity| remote_worktree_repository_identity(&eligibility.project_id, &identity));
    eligibility
}

fn sanitize_remote_worktree_snapshot(
    mut snapshot: vibex_core::GitWorktreeLifecycleSnapshot,
) -> vibex_core::GitWorktreeLifecycleSnapshot {
    snapshot.eligibility = sanitize_remote_worktree_eligibility(snapshot.eligibility);
    for managed in &mut snapshot.managed_worktrees {
        managed.repo_root = format!("remote:repository:{}", managed.project_id.as_str());
        managed.worktree_path = format!("remote:worktree:{}", managed.worktree_id.as_str());
        managed.repository_identity = managed
            .repository_identity
            .take()
            .map(|identity| remote_worktree_repository_identity(&managed.project_id, &identity));
        managed.worktree_path_identity = managed.worktree_path_identity.take().map(|identity| {
            remote_worktree_path_identity("worktree", managed.worktree_id.as_str(), identity.exists)
        });
    }
    for operation in &mut snapshot.operations {
        operation.worktree_path = operation.worktree_path.as_ref().map(|_| {
            format!(
                "remote:worktree-operation:{}",
                operation.operation_id.as_str()
            )
        });
        operation.detail.idempotency_key = None;
        operation.detail.request_fingerprint = None;
        operation.detail.repository_identity = None;
        operation.detail.source_path_identity = None;
        operation.detail.target_path_identity = None;
        operation.detail.lock_keys.clear();
        operation.detail.preflight_revision = None;
        operation.detail.lease_owner = None;
        operation.detail.lease_expires_at_ms = None;
        operation.detail.queue_key = None;
    }
    for readiness in &mut snapshot.readiness {
        for check in &mut readiness.checks {
            check.command = "recorded-check".to_string();
        }
    }
    snapshot
}

async fn handle_workbench_request(
    state: &RemoteRouterState,
    request: RemoteRequestEnvelope,
) -> RemoteResponseEnvelope {
    let request_id = request.request_id.clone();
    let correlation_id = request.correlation_id.clone();
    let response = match decode_workbench_request(&request) {
        Ok(workbench_request) => {
            dispatch_workbench_request(state, request_id, correlation_id, workbench_request).await
        }
        Err(err) => Err(err),
    };

    match response {
        Ok(payload) => {
            RemoteResponseEnvelope::ok(request.request_id, request.correlation_id, payload)
        }
        Err(error) => {
            RemoteResponseEnvelope::error(request.request_id, request.correlation_id, error)
        }
    }
}

fn decode_workbench_request(
    request: &RemoteRequestEnvelope,
) -> VibexResult<RemoteWorkbenchRequest> {
    let payload = request.payload.clone().ok_or_else(|| {
        VibexError::validation(
            "remote_workbench_payload_missing",
            "remote workbench operation requires a payload",
        )
    })?;

    serde_json::from_value(payload).map_err(|err| {
        VibexError::validation(
            "remote_workbench_payload_invalid",
            "remote workbench operation payload is invalid",
        )
        .with_diagnostic("error", err.to_string())
    })
}

async fn handle_provider_request(
    state: &RemoteRouterState,
    request: RemoteRequestEnvelope,
) -> RemoteResponseEnvelope {
    let request_id = request.request_id.clone();
    let correlation_id = request.correlation_id.clone();
    let response = match decode_provider_request(&request) {
        Ok(provider_request) => {
            dispatch_provider_request(state, request_id, correlation_id, provider_request).await
        }
        Err(err) => Err(err),
    };

    match response {
        Ok(payload) => {
            RemoteResponseEnvelope::ok(request.request_id, request.correlation_id, payload)
        }
        Err(error) => {
            RemoteResponseEnvelope::error(request.request_id, request.correlation_id, error)
        }
    }
}

fn decode_provider_request(request: &RemoteRequestEnvelope) -> VibexResult<RemoteProviderRequest> {
    let payload = request.payload.clone().ok_or_else(|| {
        VibexError::validation(
            "remote_provider_payload_missing",
            "remote Provider settings operation requires a payload",
        )
    })?;

    serde_json::from_value(payload).map_err(|err| {
        VibexError::validation(
            "remote_provider_payload_invalid",
            "remote Provider settings operation payload is invalid",
        )
        .with_diagnostic("error", err.to_string())
    })
}

async fn dispatch_provider_request(
    state: &RemoteRouterState,
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    request: RemoteProviderRequest,
) -> VibexResult<serde_json::Value> {
    let runtime = state.provider.as_ref().ok_or_else(|| {
        VibexError::capability(
            "remote_provider_settings_unavailable",
            "remote Provider settings APIs are not available on this service",
        )
    })?;
    let service = ProviderConfigService::new(runtime.db_path.clone());

    match request {
        RemoteProviderRequest::ListProfiles(request) => {
            authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProviderSettings,
                Some(request_id),
                correlation_id,
            )?;
            let profiles = service
                .list_profiles()?
                .into_iter()
                .map(|profile| profile.summary())
                .collect();
            serde_json::to_value(RemoteProviderProfileListResponse { profiles })
                .map_err(remote_payload_encode_error)
        }
        RemoteProviderRequest::PreviewInjection(mut request) => {
            authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProviderSettings,
                Some(request_id),
                correlation_id,
            )?;
            request.request.persist = false;
            let preview = service.preview_injection(request.request)?;
            serde_json::to_value(RemoteProviderInjectionPreviewResponse { preview })
                .map_err(remote_payload_encode_error)
        }
        RemoteProviderRequest::ListHealthSummaries(request) => {
            authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProviderSettings,
                Some(request_id),
                correlation_id,
            )?;
            let summaries = service.list_health_summaries()?;
            serde_json::to_value(RemoteProviderHealthSummaryListResponse { summaries })
                .map_err(remote_payload_encode_error)
        }
        RemoteProviderRequest::RunHealthProbes(request) => {
            let auth = authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateProviderSettings,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let probe_count = request
                .request
                .probe_kinds
                .as_ref()
                .map_or(0, std::vec::Vec::len);
            let profile_count = request
                .request
                .provider_profile_ids
                .as_ref()
                .map_or(0, std::vec::Vec::len);
            let result = service.run_health_probes(request.request);
            audit_provider_mutation(
                runtime,
                &auth,
                "provider_run_health_probes",
                format!(
                    "Provider health probes: {profile_count} selected profile(s), {probe_count} selected probe kind(s)"
                ),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let result = result?;
            serde_json::to_value(RemoteProviderRunHealthProbesResponse { result })
                .map_err(remote_payload_encode_error)
        }
        RemoteProviderRequest::ListUsageSummaries(request) => {
            authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProviderSettings,
                Some(request_id),
                correlation_id,
            )?;
            let summaries = service.list_usage_summaries(request.request)?;
            serde_json::to_value(RemoteProviderUsageSummaryListResponse { summaries })
                .map_err(remote_payload_encode_error)
        }
        RemoteProviderRequest::ListFailoverRecommendations(request) => {
            authorize_provider_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProviderSettings,
                Some(request_id),
                correlation_id,
            )?;
            let recommendations = service.list_failover_recommendations(request.request)?;
            serde_json::to_value(RemoteProviderFailoverRecommendationListResponse {
                recommendations,
            })
            .map_err(remote_payload_encode_error)
        }
    }
}

async fn dispatch_agent_request(
    state: &RemoteRouterState,
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    request: RemoteAgentRequest,
) -> VibexResult<serde_json::Value> {
    let manager = state.agent_manager.as_ref().cloned().ok_or_else(|| {
        VibexError::capability(
            "remote_agent_sessions_unavailable",
            "remote Agent sessions are not available on this service",
        )
    })?;

    match request {
        RemoteAgentRequest::ListSessions(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let sessions = manager
                .list_sessions(request.include_archived.unwrap_or(false))
                .await?;
            let timeline_limit = normalize_timeline_limit(request.timeline_limit);
            let mut summaries = Vec::with_capacity(sessions.len());
            for session in sessions {
                let latest_timeline = manager
                    .fetch_timeline(FetchTimelineRequest {
                        session_id: session.id.clone(),
                        after_sequence: None,
                        limit: timeline_limit,
                    })
                    .await?;
                summaries.push(vibex_core::AgentSessionSummary {
                    session,
                    latest_timeline,
                });
            }
            serde_json::to_value(RemoteAgentSessionListResponse {
                sessions: summaries,
            })
            .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetSession(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let session = manager.get_session(&request.session_id).await?;
            let latest_timeline = manager
                .fetch_timeline(FetchTimelineRequest {
                    session_id: session.id.clone(),
                    after_sequence: None,
                    limit: normalize_timeline_limit(request.timeline_limit),
                })
                .await?;
            serde_json::to_value(RemoteAgentSessionDetailResponse {
                session,
                latest_timeline,
            })
            .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::FetchTimeline(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let page = manager.fetch_timeline(request.request).await?;
            serde_json::to_value(RemoteAgentTimelineFetchResponse { page })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::ResolveOpaqueLocator(request) => {
            authorize_agent_action(
                &manager,
                request.auth.clone(),
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let resolution =
                resolve_opaque_locator(&manager, request.notification_id, request.opaque_locator)
                    .await?;
            serde_json::to_value(RemoteAgentDeepLinkResolveResponse { resolution })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::ListRuntimeOptions(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let catalog = remote_runtime_catalog(state)?
                .list_runtime_options()
                .await?;
            serde_json::to_value(RemoteAgentRuntimeOptionsResponse { catalog })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetRuntimeSelection(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let runtime_selection = remote_runtime_selection(state)?;
            let state = runtime_selection.get_selection_state(&request.session_id)?;
            serde_json::to_value(RemoteAgentRuntimeSelectionResponse { state })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::SetDesiredRuntime(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::MutateAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let state = remote_runtime_selection(state)?
                .set_desired_runtime(request.request)
                .await?;
            serde_json::to_value(RemoteAgentSetDesiredRuntimeResponse { state })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::CancelRuntimeSwitch(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::MutateAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let state = remote_runtime_selection(state)?
                .cancel_switch(request.request)
                .await?;
            serde_json::to_value(RemoteAgentCancelRuntimeSwitchResponse { state })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetRuntimeSnapshot(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            manager.get_session(&request.session_id).await?;
            let lifecycle = remote_runtime_lifecycle(state)?;
            let snapshot = lifecycle.snapshot(&request.session_id)?;
            serde_json::to_value(RemoteAgentRuntimeSnapshotResponse { snapshot })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetRuntimeProcessSnapshot(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let lifecycle = remote_runtime_lifecycle(state)?;
            let snapshot = lifecycle.process_snapshot(&request.process_id)?;
            serde_json::to_value(RemoteAgentRuntimeProcessSnapshotResponse { snapshot })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetRuntimeEvents(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            manager.get_session(&request.request.session_id).await?;
            let lifecycle = remote_runtime_lifecycle(state)?;
            let batch = lifecycle.events(
                &request.request.session_id,
                request.request.after.as_ref(),
                request.request.limit.map(|limit| limit as usize),
            )?;
            serde_json::to_value(RemoteAgentRuntimeEventsResponse { batch })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::AttachRuntime(request) => {
            if !matches!(
                request.request.role,
                RuntimeLeaseRole::Owner | RuntimeLeaseRole::Viewer
            ) {
                return Err(VibexError::validation(
                    "runtime_lease_internal_role_forbidden",
                    "internal runtime lease roles cannot be requested by clients",
                ));
            }
            let action = if request.request.role == RuntimeLeaseRole::Owner {
                RemoteActionClass::MutateAgentSession
            } else {
                RemoteActionClass::ReadAgentSession
            };
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                action,
                Some(request_id),
                correlation_id,
            )?;
            manager.get_session(&request.request.session_id).await?;
            let lifecycle = remote_runtime_lifecycle(state)?;
            let scope = format!("remote:{}", auth.device_id.as_str());
            let response = lifecycle.attach(request.request, scope).await?;
            serde_json::to_value(RemoteAgentAttachRuntimeResponse { response })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::DetachRuntime(request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            manager.get_session(&request.request.session_id).await?;
            let lifecycle = remote_runtime_lifecycle(state)?;
            let scope = format!("remote:{}", auth.device_id.as_str());
            let response = lifecycle.detach(request.request, scope).await?;
            serde_json::to_value(RemoteAgentDetachRuntimeResponse { response })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::GetMessageSubmission(request) => {
            authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let coordinator = state.message_submission.as_ref().ok_or_else(|| {
                VibexError::capability(
                    "remote_agent_message_submission_unavailable",
                    "remote Agent message submission state is not available on this service",
                )
            })?;
            let submission = coordinator.get_submission(&request.request)?;
            serde_json::to_value(RemoteAgentMessageSubmissionResponse { submission })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::SendMessage(request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::MutateAgentSession,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let session_id = request.request.session_id.clone();
            let result = manager.send_message(request.request).await;
            audit_agent_mutation(
                &manager,
                Some(auth.device_id),
                RemoteAuditTargetKind::AgentSession,
                session_id.as_str(),
                "Agent message submission",
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let appended_items = result?;
            serde_json::to_value(RemoteAgentSendMessageResponse { appended_items })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::ContinueTurn(request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::MutateAgentSession,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let session_id = request.request.session_id.clone();
            let result = manager.continue_turn(request.request).await;
            audit_agent_mutation(
                &manager,
                Some(auth.device_id),
                RemoteAuditTargetKind::AgentSession,
                session_id.as_str(),
                "Agent turn continuation",
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let appended_items = result?;
            serde_json::to_value(RemoteAgentContinueTurnResponse { appended_items })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::Interrupt(request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::MutateAgentSession,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let session_id = request.session_id.clone();
            let result = manager.interrupt(&request.session_id).await;
            audit_agent_mutation(
                &manager,
                Some(auth.device_id),
                RemoteAuditTargetKind::AgentSession,
                session_id.as_str(),
                "Agent turn interrupt",
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            result?;
            serde_json::to_value(RemoteAgentInterruptResponse { interrupted: true })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::ResolvePermission(mut request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ResolvePermission,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            validate_permission_resolution(&request.request)?;
            request.request.resolution.responder_device_id = Some(auth.device_id);
            let device_id = request.request.resolution.responder_device_id.clone();
            let permission_request_id = request.request.request_id.clone();
            let item_result = manager.resolve_permission(request.request).await;
            audit_agent_mutation(
                &manager,
                device_id,
                RemoteAuditTargetKind::Permission,
                permission_request_id.as_str(),
                "Permission resolution",
                item_result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let item = item_result?;
            serde_json::to_value(RemoteAgentResolvePermissionResponse { item })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::ResolveElicitation(mut request) => {
            let auth = authorize_agent_action(
                &manager,
                request.auth,
                RemoteActionClass::ResolveElicitation,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            validate_elicitation_resolution(&request.request)?;
            request.request.resolution.responder_device_id = Some(auth.device_id);
            let device_id = request.request.resolution.responder_device_id.clone();
            let elicitation_request_id = request.request.request_id.clone();
            let item_result = manager.resolve_elicitation(request.request).await;
            audit_agent_mutation(
                &manager,
                device_id,
                RemoteAuditTargetKind::Elicitation,
                elicitation_request_id.as_str(),
                "Elicitation resolution",
                item_result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let item = item_result?;
            serde_json::to_value(RemoteAgentResolveElicitationResponse { item })
                .map_err(remote_payload_encode_error)
        }
        RemoteAgentRequest::CatchUp(request) => {
            authorize_agent_action(
                &manager,
                request.auth.clone(),
                RemoteActionClass::ReadAgentSession,
                Some(request_id),
                correlation_id,
            )?;
            let response = catch_up_agent_timeline(&manager, request).await?;
            serde_json::to_value(response).map_err(remote_payload_encode_error)
        }
    }
}

fn remote_runtime_selection(
    state: &RemoteRouterState,
) -> VibexResult<&Arc<RuntimeSelectionService>> {
    state.runtime_selection.as_ref().ok_or_else(|| {
        VibexError::capability(
            "remote_agent_runtime_selection_unavailable",
            "remote Agent runtime selection is not available on this service",
        )
    })
}

fn remote_runtime_catalog(
    state: &RemoteRouterState,
) -> VibexResult<&Arc<dyn RemoteRuntimeOptionCatalogSource>> {
    state.runtime_catalog.as_ref().ok_or_else(|| {
        VibexError::capability(
            "remote_agent_runtime_catalog_unavailable",
            "remote Agent runtime option catalog is not available on this service",
        )
    })
}

fn remote_runtime_lifecycle(
    state: &RemoteRouterState,
) -> VibexResult<&Arc<RuntimeLifecycleService>> {
    state.runtime_lifecycle.as_ref().ok_or_else(|| {
        VibexError::capability(
            "remote_agent_runtime_lifecycle_unavailable",
            "remote Agent runtime lifecycle is not available on this service",
        )
    })
}

async fn dispatch_workbench_request(
    state: &RemoteRouterState,
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    request: RemoteWorkbenchRequest,
) -> VibexResult<serde_json::Value> {
    let runtime = state.workbench.as_ref().ok_or_else(|| {
        VibexError::capability(
            "remote_workbench_unavailable",
            "remote workbench APIs are not available on this service",
        )
    })?;

    match request {
        RemoteWorkbenchRequest::ListWorkspaces(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let mut conn = open_migrated_database(&runtime.db_path)?;
            let mut workspaces = Vec::new();
            for (project, workspace) in WorkspaceRepository::list(&conn)? {
                workspaces.push(workspace_summary(
                    &mut conn,
                    &runtime.terminals,
                    project,
                    workspace,
                )?);
            }
            serde_json::to_value(RemoteWorkbenchListWorkspacesResponse { workspaces })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::OpenWorkspace(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let summary = open_workspace_summary(runtime, request.request)?;
            serde_json::to_value(RemoteWorkbenchOpenWorkspaceResponse { summary })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileListTree(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let (_conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let entries = service.list_tree(&request.request)?;
            serde_json::to_value(RemoteFileTreeResponse { entries })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileRead(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let (conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let file = service.read_file(&request.request)?;
            RecentFileRepository::touch(&conn, &request.request.workspace_id, &file.path)?;
            serde_json::to_value(RemoteFileReadResponse { file })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileSearch(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let (_conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let results = service.search(&request.request)?;
            serde_json::to_value(RemoteFileSearchResponse { results })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileWrite(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateFile,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let (conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let result = service.write_file(&request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::WorkspaceFile,
                "file_write",
                format!("File write: {}", request.request.path),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let file = result?;
            RecentFileRepository::touch(&conn, &request.request.workspace_id, &file.path)?;
            serde_json::to_value(RemoteFileWriteResponse { file })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileDelete(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateFile,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let (_conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let result = service.delete_path(&request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::WorkspaceFile,
                "file_delete",
                format!("File delete: {}", request.request.path),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            result?;
            serde_json::to_value(RemoteFileDeleteResponse { deleted: true })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::FileRename(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateFile,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let (_conn, service) =
                file_service_for_workspace(&runtime.db_path, &request.request.workspace_id)?;
            let result = service.rename_path(&request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::WorkspaceFile,
                "file_rename",
                format!(
                    "File rename: {} -> {}",
                    request.request.path,
                    request.request.new_path.as_deref().unwrap_or("[missing]")
                ),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let entry = result?;
            serde_json::to_value(RemoteFileRenameResponse { entry })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitStatus(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let status = git_status_for_workspace(runtime, &request.workspace_id)?;
            serde_json::to_value(RemoteGitStatusResponse { status })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitDiff(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let diff = vibex_git::diff(root, &request.request)?;
            serde_json::to_value(RemoteGitDiffResponse { diff })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitStage(request) => git_mutation_status(
            runtime,
            request.auth,
            request.request.workspace_id.clone(),
            "git_stage",
            format!("Git stage {} path(s)", request.request.paths.len()),
            request_id,
            correlation_id,
            |root| vibex_git::stage(request.request.workspace_id.clone(), root, &request.request),
        ),
        RemoteWorkbenchRequest::GitUnstage(request) => git_mutation_status(
            runtime,
            request.auth,
            request.request.workspace_id.clone(),
            "git_unstage",
            format!("Git unstage {} path(s)", request.request.paths.len()),
            request_id,
            correlation_id,
            |root| vibex_git::unstage(request.request.workspace_id.clone(), root, &request.request),
        ),
        RemoteWorkbenchRequest::GitRevert(request) => git_mutation_status(
            runtime,
            request.auth,
            request.request.workspace_id.clone(),
            "git_revert",
            format!("Git revert {} path(s)", request.request.paths.len()),
            request_id,
            correlation_id,
            |root| vibex_git::revert(request.request.workspace_id.clone(), root, &request.request),
        ),
        RemoteWorkbenchRequest::GitCommit(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateGit,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let result = vibex_git::commit(
                request.request.workspace_id.clone(),
                &root,
                &request.request,
            );
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Git,
                "git_commit",
                "Git commit".to_string(),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let result = result?;
            let status_after = vibex_git::status(request.request.workspace_id.clone(), root)?;
            persist_git_snapshot_for_status(runtime, &status_after)?;
            serde_json::to_value(RemoteGitCommitResponse {
                result,
                status_after,
            })
            .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitHistory(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let history = vibex_git::history(root, &request.request)?;
            serde_json::to_value(RemoteGitHistoryResponse { history })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitCommitDetail(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let detail = vibex_git::commit_detail(root, &request.request)?;
            serde_json::to_value(RemoteGitCommitDetailResponse { detail })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitBlame(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let blame = vibex_git::blame(root, &request.request)?;
            serde_json::to_value(RemoteGitBlameResponse { blame })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitBranchList(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let root = workspace_root_for_id(runtime, &request.workspace_id)?;
            let branches = vibex_git::branch_list(request.workspace_id, root)?;
            serde_json::to_value(RemoteGitBranchListResponse { branches })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitBranchCreate(request) => git_mutation_status(
            runtime,
            request.auth,
            request.request.workspace_id.clone(),
            "git_branch_create",
            format!("Git branch create: {}", request.request.name),
            request_id,
            correlation_id,
            |root| {
                vibex_git::branch_create(
                    request.request.workspace_id.clone(),
                    root,
                    &request.request,
                )
            },
        ),
        RemoteWorkbenchRequest::GitBranchCheckout(request) => git_mutation_status(
            runtime,
            request.auth,
            request.request.workspace_id.clone(),
            "git_branch_checkout",
            format!("Git branch checkout: {}", request.request.name),
            request_id,
            correlation_id,
            |root| {
                vibex_git::branch_checkout(
                    request.request.workspace_id.clone(),
                    root,
                    &request.request,
                )
            },
        ),
        RemoteWorkbenchRequest::GitRemoteAction(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateGit,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let result = vibex_git::remote_action(
                request.request.workspace_id.clone(),
                root,
                &request.request,
            );
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Git,
                "git_remote_action",
                format!("Git remote action: {:?}", request.request.kind),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let result = result?;
            if let Some(status) = result.status_after.as_ref() {
                persist_git_snapshot_for_status(runtime, status)?;
            }
            serde_json::to_value(RemoteGitRemoteActionResponse { result })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitWorktreeEligibility(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let source = runtime.worktrees.as_ref().ok_or_else(|| {
                VibexError::validation(
                    "remote_worktree_read_unavailable",
                    "remote worktree snapshots are unavailable",
                )
            })?;
            let eligibility = sanitize_remote_worktree_eligibility(
                source.worktree_eligibility(request.workspace_id).await?,
            );
            serde_json::to_value(RemoteGitWorktreeEligibilityResponse { eligibility })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::GitWorktreeSnapshot(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let source = runtime.worktrees.as_ref().ok_or_else(|| {
                VibexError::validation(
                    "remote_worktree_read_unavailable",
                    "remote worktree snapshots are unavailable",
                )
            })?;
            let snapshot = sanitize_remote_worktree_snapshot(
                source.worktree_snapshot(request.workspace_id).await?,
            );
            serde_json::to_value(RemoteGitWorktreeSnapshotResponse { snapshot })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalList(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let terminals = terminal_list_for_workspace(runtime, &request.workspace_id)?;
            serde_json::to_value(RemoteTerminalListResponse { terminals })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalCreate(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateTerminal,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let root = workspace_root_for_id(runtime, &request.request.workspace_id)?;
            let terminal = runtime.terminals.create(root, request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Terminal,
                "terminal_create",
                "Terminal create".to_string(),
                terminal.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let terminal = terminal?;
            let conn = open_migrated_database(&runtime.db_path)?;
            TerminalSessionRepository::upsert(&conn, &terminal)?;
            serde_json::to_value(RemoteTerminalCreateResponse { terminal })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalSnapshot(request) => {
            authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::ReadProject,
                Some(request_id),
                correlation_id,
            )?;
            let snapshot = runtime.terminals.snapshot(&request.terminal_id)?;
            let conn = open_migrated_database(&runtime.db_path)?;
            TerminalSessionRepository::upsert(&conn, &snapshot.session)?;
            serde_json::to_value(RemoteTerminalSnapshotResponse { snapshot })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalWrite(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateTerminal,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let result = runtime.terminals.write(&request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Terminal,
                "terminal_write",
                format!("Terminal write: {}", request.request.terminal_id.as_str()),
                result.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            result?;
            serde_json::to_value(RemoteTerminalWriteResponse { written: true })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalResize(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateTerminal,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let terminal = runtime.terminals.resize(&request.request);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Terminal,
                "terminal_resize",
                format!("Terminal resize: {}", request.request.terminal_id.as_str()),
                terminal.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let terminal = terminal?;
            let conn = open_migrated_database(&runtime.db_path)?;
            TerminalSessionRepository::upsert(&conn, &terminal)?;
            serde_json::to_value(RemoteTerminalResizeResponse { terminal })
                .map_err(remote_payload_encode_error)
        }
        RemoteWorkbenchRequest::TerminalKill(request) => {
            let auth = authorize_workbench_action(
                runtime,
                request.auth,
                RemoteActionClass::MutateTerminal,
                Some(request_id.clone()),
                correlation_id.clone(),
            )?;
            let terminal = runtime.terminals.kill(&request.terminal_id);
            audit_workbench_mutation(
                runtime,
                &auth,
                RemoteAuditTargetKind::Terminal,
                "terminal_kill",
                format!("Terminal kill: {}", request.terminal_id.as_str()),
                terminal.is_ok(),
                Some(request_id),
                correlation_id,
            )?;
            let terminal = terminal?;
            let conn = open_migrated_database(&runtime.db_path)?;
            TerminalSessionRepository::upsert(&conn, &terminal)?;
            serde_json::to_value(RemoteTerminalKillResponse { terminal })
                .map_err(remote_payload_encode_error)
        }
    }
}

fn authorize_workbench_action(
    runtime: &RemoteWorkbenchRuntime,
    proof: RemoteAuthProof,
    action: RemoteActionClass,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<RemoteAuthContext> {
    let conn = open_migrated_database(&runtime.db_path)?;
    let auth = RemoteTrustService::authenticate(&conn, proof)?;
    RemoteTrustService::authorize_action(&conn, &auth, action, request_id, correlation_id)?;
    Ok(auth)
}

fn authorize_provider_action(
    runtime: &RemoteProviderRuntime,
    proof: RemoteAuthProof,
    action: RemoteActionClass,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<RemoteAuthContext> {
    let conn = open_migrated_database(&runtime.db_path)?;
    let auth = RemoteTrustService::authenticate(&conn, proof)?;
    RemoteTrustService::authorize_action(&conn, &auth, action, request_id, correlation_id)?;
    Ok(auth)
}

fn open_migrated_database(path: &Path) -> VibexResult<DbConnection> {
    let mut conn = open_database(path)?;
    apply_migrations(&mut conn)?;
    Ok(conn)
}

fn canonical_workspace_root(root_path: &str) -> VibexResult<PathBuf> {
    let root = PathBuf::from(root_path);
    if !root.exists() {
        return Err(VibexError::validation(
            "workspace_root_missing",
            "workspace root does not exist",
        )
        .with_diagnostic("path", root.display().to_string()));
    }
    if !root.is_dir() {
        return Err(VibexError::validation(
            "workspace_root_not_directory",
            "workspace root must be a directory",
        )
        .with_diagnostic("path", root.display().to_string()));
    }
    root.canonicalize().map_err(|err| {
        VibexError::storage(
            "workspace_root_canonicalize_failed",
            "failed to resolve workspace root",
        )
        .with_diagnostic("path", root.display().to_string())
        .with_diagnostic("error", err.to_string())
    })
}

fn workspace_root_for_id(
    runtime: &RemoteWorkbenchRuntime,
    workspace_id: &WorkspaceId,
) -> VibexResult<PathBuf> {
    let conn = open_migrated_database(&runtime.db_path)?;
    workspace_root(&conn, workspace_id)
}

fn workspace_root(conn: &DbConnection, workspace_id: &WorkspaceId) -> VibexResult<PathBuf> {
    let (_project, workspace) = WorkspaceRepository::get(conn, workspace_id)?
        .ok_or_else(|| VibexError::validation("workspace_not_found", "workspace was not found"))?;
    Ok(PathBuf::from(workspace.root_path))
}

fn file_service_for_workspace(
    db_path: &Path,
    workspace_id: &WorkspaceId,
) -> VibexResult<(DbConnection, WorkspaceFileService)> {
    let conn = open_migrated_database(db_path)?;
    let root = workspace_root(&conn, workspace_id)?;
    let service = WorkspaceFileService::new(root, workspace_id.clone())?;
    Ok((conn, service))
}

fn open_workspace_summary(
    runtime: &RemoteWorkbenchRuntime,
    request: OpenWorkspaceRequest,
) -> VibexResult<ProjectWorkspaceSummary> {
    let root = canonical_workspace_root(&request.root_path)?;
    let mut conn = open_migrated_database(&runtime.db_path)?;
    let (project, workspace) = WorkspaceRepository::ensure(
        &conn,
        &root,
        request.mode.unwrap_or(WorkspaceMode::CurrentCheckout),
    )?;
    workspace_summary(&mut conn, &runtime.terminals, project, workspace)
}

fn workspace_summary(
    conn: &mut DbConnection,
    terminals: &TerminalManager,
    project: vibex_core::ProjectRecord,
    workspace: vibex_core::WorkspaceRecord,
) -> VibexResult<ProjectWorkspaceSummary> {
    let sessions = SessionRepository::list(conn, false)?;
    let agent_running = sessions.iter().any(|session| {
        session.workspace_id == workspace.id && session.state == AgentSessionState::Running
    });
    let pending_permission = sessions.iter().any(|session| {
        session.workspace_id == workspace.id && session.state == AgentSessionState::NeedsInput
    });
    let terminal_running = terminals
        .list(&workspace.id)?
        .iter()
        .any(|session| session.status == TerminalStatus::Running);
    let git_summary = vibex_git::status(workspace.id.clone(), &workspace.root_path).ok();
    if let Some(summary) = git_summary.as_ref() {
        persist_git_snapshot(conn, summary)?;
    }
    let git_dirty = git_summary.as_ref().is_some_and(|summary| summary.dirty);
    Ok(ProjectWorkspaceSummary {
        project,
        workspace,
        aggregate_status: WorkspaceAggregateStatus {
            agent_running,
            terminal_running,
            pending_permission,
            git_dirty,
            sync_disconnected: false,
        },
        git_branch: git_summary
            .as_ref()
            .and_then(|summary| summary.branch.clone()),
        git_dirty,
    })
}

fn persist_git_snapshot_for_status(
    runtime: &RemoteWorkbenchRuntime,
    status: &vibex_core::GitStatusSummary,
) -> VibexResult<()> {
    let conn = open_migrated_database(&runtime.db_path)?;
    persist_git_snapshot(&conn, status)
}

fn persist_git_snapshot(
    conn: &DbConnection,
    status: &vibex_core::GitStatusSummary,
) -> VibexResult<()> {
    GitSnapshotRepository::upsert(
        conn,
        &status.workspace_id,
        status.branch.as_deref(),
        status.short_commit.as_deref(),
        status.dirty,
        status.changes.len() as u32,
        status.captured_at_ms,
    )
}

fn git_status_for_workspace(
    runtime: &RemoteWorkbenchRuntime,
    workspace_id: &WorkspaceId,
) -> VibexResult<vibex_core::GitStatusSummary> {
    let root = workspace_root_for_id(runtime, workspace_id)?;
    let status = vibex_git::status(workspace_id.clone(), root)?;
    persist_git_snapshot_for_status(runtime, &status)?;
    Ok(status)
}

#[allow(clippy::too_many_arguments)]
fn git_mutation_status<F>(
    runtime: &RemoteWorkbenchRuntime,
    proof: RemoteAuthProof,
    workspace_id: WorkspaceId,
    operation: &'static str,
    summary: String,
    request_id: RequestId,
    correlation_id: Option<vibex_core::CorrelationId>,
    mutate: F,
) -> VibexResult<serde_json::Value>
where
    F: FnOnce(PathBuf) -> VibexResult<vibex_core::GitStatusSummary>,
{
    let auth = authorize_workbench_action(
        runtime,
        proof,
        RemoteActionClass::MutateGit,
        Some(request_id.clone()),
        correlation_id.clone(),
    )?;
    let root = workspace_root_for_id(runtime, &workspace_id)?;
    let status = mutate(root);
    audit_workbench_mutation(
        runtime,
        &auth,
        RemoteAuditTargetKind::Git,
        operation,
        summary,
        status.is_ok(),
        Some(request_id),
        correlation_id,
    )?;
    let status = status?;
    persist_git_snapshot_for_status(runtime, &status)?;
    serde_json::to_value(RemoteGitStatusMutationResponse { status })
        .map_err(remote_payload_encode_error)
}

fn terminal_list_for_workspace(
    runtime: &RemoteWorkbenchRuntime,
    workspace_id: &WorkspaceId,
) -> VibexResult<Vec<vibex_core::TerminalSession>> {
    let conn = open_migrated_database(&runtime.db_path)?;
    let stored = TerminalSessionRepository::list(&conn, workspace_id)?;
    let live = runtime.terminals.list(workspace_id)?;
    let live_by_id: std::collections::HashMap<_, _> = live
        .iter()
        .map(|session| (session.id.clone(), session.clone()))
        .collect();
    let mut root = None;
    let mut visible = Vec::new();

    for session in stored {
        if let Some(live_session) = live_by_id.get(&session.id) {
            TerminalSessionRepository::upsert(&conn, live_session)?;
            if terminal_session_visible(live_session) {
                visible.push(live_session.clone());
            }
        } else if terminal_session_should_restore(&session) {
            if root.is_none() {
                root = Some(workspace_root(&conn, workspace_id)?);
            }
            let restored = runtime.terminals.restore(
                root.as_ref().expect("terminal workspace root initialized"),
                session,
            )?;
            TerminalSessionRepository::upsert(&conn, &restored)?;
            visible.push(restored);
        }
    }
    for session in live {
        if terminal_session_visible(&session)
            && !visible.iter().any(|stored| stored.id == session.id)
        {
            TerminalSessionRepository::upsert(&conn, &session)?;
            visible.push(session);
        }
    }
    Ok(visible)
}

fn terminal_session_should_restore(session: &TerminalSession) -> bool {
    matches!(
        session.status,
        TerminalStatus::Running | TerminalStatus::Stale
    )
}

fn terminal_session_visible(session: &TerminalSession) -> bool {
    session.status == TerminalStatus::Running
}

#[allow(clippy::too_many_arguments)]
fn audit_workbench_mutation(
    runtime: &RemoteWorkbenchRuntime,
    auth: &RemoteAuthContext,
    target_kind: RemoteAuditTargetKind,
    target_id: impl Into<String>,
    summary: impl Into<String>,
    succeeded: bool,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<()> {
    let conn = open_migrated_database(&runtime.db_path)?;
    RemoteTrustService::insert_audit(
        &conn,
        Some(auth.device_id.clone()),
        if succeeded {
            RemoteAuditAction::MutationAllowed
        } else {
            RemoteAuditAction::MutationDenied
        },
        target_kind,
        Some(target_id.into()),
        if succeeded {
            RemoteAuditOutcome::Allowed
        } else {
            RemoteAuditOutcome::Failed
        },
        summary,
        request_id,
        correlation_id,
    )
}

fn audit_provider_mutation(
    runtime: &RemoteProviderRuntime,
    auth: &RemoteAuthContext,
    target_id: impl Into<String>,
    summary: impl Into<String>,
    succeeded: bool,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<()> {
    let conn = open_migrated_database(&runtime.db_path)?;
    RemoteTrustService::insert_audit(
        &conn,
        Some(auth.device_id.clone()),
        if succeeded {
            RemoteAuditAction::MutationAllowed
        } else {
            RemoteAuditAction::MutationDenied
        },
        RemoteAuditTargetKind::ProviderSettings,
        Some(target_id.into()),
        if succeeded {
            RemoteAuditOutcome::Allowed
        } else {
            RemoteAuditOutcome::Failed
        },
        summary,
        request_id,
        correlation_id,
    )
}

fn authorize_agent_action(
    manager: &AgentManager,
    proof: RemoteAuthProof,
    action: RemoteActionClass,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<RemoteAuthContext> {
    let mut conn = open_database(manager.database_path())?;
    apply_migrations(&mut conn)?;
    let auth = RemoteTrustService::authenticate(&conn, proof)?;
    RemoteTrustService::authorize_action(&conn, &auth, action, request_id, correlation_id)?;
    Ok(auth)
}

#[allow(clippy::too_many_arguments)]
fn audit_agent_mutation(
    manager: &AgentManager,
    device_id: Option<DeviceId>,
    target_kind: RemoteAuditTargetKind,
    target_id: impl Into<String>,
    summary: impl Into<String>,
    succeeded: bool,
    request_id: Option<RequestId>,
    correlation_id: Option<vibex_core::CorrelationId>,
) -> VibexResult<()> {
    let mut conn = open_database(manager.database_path())?;
    apply_migrations(&mut conn)?;
    RemoteTrustService::insert_audit(
        &conn,
        device_id,
        if succeeded {
            RemoteAuditAction::MutationAllowed
        } else {
            RemoteAuditAction::MutationDenied
        },
        target_kind,
        Some(target_id.into()),
        if succeeded {
            RemoteAuditOutcome::Allowed
        } else {
            RemoteAuditOutcome::Failed
        },
        summary,
        request_id,
        correlation_id,
    )
}

async fn resolve_opaque_locator(
    manager: &AgentManager,
    notification_id: String,
    opaque_locator: String,
) -> VibexResult<RemoteDeepLinkResolution> {
    if notification_id.trim().is_empty()
        || notification_id.len() > 256
        || notification_id
            .chars()
            .any(|character| character.is_control())
        || opaque_locator.trim().is_empty()
        || opaque_locator.len() > 512
        || opaque_locator
            .chars()
            .any(|character| character.is_control())
    {
        return Err(VibexError::validation(
            "remote_deep_link_invalid",
            "opaque deep-link identifiers are invalid",
        ));
    }

    // The first resolver deliberately accepts only the server-issued session
    // id shape. The browser still treats this value as opaque; the PC is the
    // only side that interprets it and checks the authoritative session store.
    let Ok(session_id) = vibex_core::VibexSessionId::parse(opaque_locator) else {
        return Ok(RemoteDeepLinkResolution {
            notification_id,
            status: RemoteDeepLinkResolutionStatus::NotFound,
            session_id: None,
            permission_request_id: None,
        });
    };
    match manager.get_session(&session_id).await {
        Ok(session) if session.deleted_at_ms.is_none() => Ok(RemoteDeepLinkResolution {
            notification_id,
            status: RemoteDeepLinkResolutionStatus::Resolved,
            session_id: Some(session.id),
            permission_request_id: None,
        }),
        Ok(_) => Ok(RemoteDeepLinkResolution {
            notification_id,
            status: RemoteDeepLinkResolutionStatus::Expired,
            session_id: None,
            permission_request_id: None,
        }),
        Err(error) if error.code == "session_not_found" => Ok(RemoteDeepLinkResolution {
            notification_id,
            status: RemoteDeepLinkResolutionStatus::NotFound,
            session_id: None,
            permission_request_id: None,
        }),
        Err(error) => Err(error),
    }
}

async fn catch_up_agent_timeline(
    manager: &AgentManager,
    request: RemoteAgentCatchUpRequest,
) -> VibexResult<RemoteAgentCatchUpResponse> {
    let limit = normalize_timeline_limit(request.limit);
    let mut events = Vec::new();
    let mut next_cursors = Vec::with_capacity(request.cursors.len());

    for cursor in request.cursors {
        let page = manager
            .fetch_timeline(FetchTimelineRequest {
                session_id: cursor.session_id.clone(),
                after_sequence: Some(cursor.after_sequence),
                limit,
            })
            .await?;
        for item in &page.items {
            events.push(remote_timeline_event(TimelineLiveEvent {
                session_id: item.session_id.clone(),
                sequence: item.sequence,
                item: item.clone(),
            })?);
        }
        next_cursors.push(RemoteAgentTimelineCursor {
            session_id: cursor.session_id,
            after_sequence: page.end_sequence.unwrap_or(cursor.after_sequence),
        });
    }

    Ok(RemoteAgentCatchUpResponse {
        events,
        next_cursors,
        compacted: false,
    })
}

fn remote_timeline_event(event: TimelineLiveEvent) -> VibexResult<RemoteLiveEventEnvelope> {
    let sequence = u64::try_from(event.sequence).map_err(|_| {
        VibexError::new(
            ErrorCategory::Remote,
            "remote_agent_timeline_sequence_invalid",
            "Agent timeline sequence cannot be converted into a remote event sequence",
        )
    })?;
    let correlation_id = event.item.correlation_id.clone();
    let payload = serde_json::to_value(event).map_err(remote_payload_encode_error)?;

    Ok(RemoteLiveEventEnvelope {
        protocol_version: RemoteProtocolVersion::foundation(),
        event_id: EventId::new(),
        correlation_id,
        channel: RemoteLiveEventChannel::AgentSession,
        sequence,
        payload: Some(payload),
        emitted_at_ms: unix_timestamp_ms(),
    })
}

fn normalize_timeline_limit(limit: Option<u32>) -> u32 {
    limit.unwrap_or(50).clamp(1, 500)
}

fn validate_permission_resolution(request: &ResolvePermissionRequest) -> VibexResult<()> {
    if request.resolution.request_id != request.request_id
        || request.resolution.session_id != request.session_id
    {
        return Err(VibexError::validation(
            "remote_permission_resolution_invalid",
            "permission resolution must match the target session and request id",
        ));
    }
    Ok(())
}

fn validate_elicitation_resolution(request: &ResolveElicitationRequest) -> VibexResult<()> {
    request.validate()
}

fn remote_payload_encode_error(err: serde_json::Error) -> VibexError {
    VibexError::new(
        ErrorCategory::Remote,
        "remote_payload_encode_failed",
        "failed to encode remote payload",
    )
    .with_diagnostic("error", err.to_string())
}

fn unsupported_operation_response(request: RemoteRequestEnvelope) -> RemoteResponseEnvelope {
    let operation = serde_json::to_string(&request.operation).unwrap_or_else(|_| "unknown".into());
    RemoteResponseEnvelope::error(
        request.request_id,
        request.correlation_id,
        VibexError::capability(
            "remote_unsupported_operation",
            "remote operation is not supported by the foundation service",
        )
        .with_recovery_hint(
            "Upgrade the remote service or wait for the matching Phase 4 child task",
        )
        .with_diagnostic("operation", operation.trim_matches('"')),
    )
}

fn invalid_envelope_response() -> RemoteResponseEnvelope {
    RemoteResponseEnvelope::error(
        RequestId::new(),
        None,
        VibexError::validation(
            "remote_invalid_envelope",
            "remote frame is not a valid request envelope",
        )
        .with_recovery_hint("Send a JSON RemoteRequestEnvelope frame"),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::{Body, to_bytes};
    use axum::http::{Request, StatusCode};
    use futures_util::SinkExt;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};
    use tokio::net::TcpListener;
    use tokio_tungstenite::connect_async;
    use tokio_tungstenite::tungstenite::Message as ClientMessage;
    use tower::ServiceExt;
    use vibex_agent::{
        AgentManager, RuntimeBackendSnapshot, RuntimeLeaseTarget, RuntimeLifecycleBackend,
        RuntimeLifecycleConfig, RuntimeLifecycleService, RuntimeSweepReport,
    };
    use vibex_core::{
        AgentId, AgentSession, AttachRuntimeRequest, CancelAgentSessionRuntimeSwitchRequest,
        CorrelationId, DetachRuntimeRequest, FileReadRequest, FileWriteRequest, GitStageRequest,
        ProviderKind, RemoteAgentAttachRuntimeRequest, RemoteAgentAttachRuntimeResponse,
        RemoteAgentCancelRuntimeSwitchRequest, RemoteAgentCatchUpRequest,
        RemoteAgentCatchUpResponse, RemoteAgentDeepLinkResolveRequest,
        RemoteAgentDeepLinkResolveResponse, RemoteAgentDetachRuntimeRequest,
        RemoteAgentDetachRuntimeResponse, RemoteAgentRequest, RemoteAgentRuntimeOptionsRequest,
        RemoteAgentRuntimeOptionsResponse, RemoteAgentRuntimeSelectionRequest,
        RemoteAgentSendMessageRequest, RemoteAgentSessionListRequest,
        RemoteAgentSessionListResponse, RemoteAgentSetDesiredRuntimeRequest,
        RemoteAgentTimelineCursor, RemoteAuditListRequest, RemoteDeepLinkResolutionStatus,
        RemoteEnvelopeStatus, RemoteFileReadRequest, RemoteFileReadResponse,
        RemoteFileWriteRequest, RemoteGitStageRequest, RemoteHandshakeResponse,
        RemoteProviderHealthSummaryListRequest, RemoteProviderInjectionPreviewRequest,
        RemoteProviderProfileListRequest, RemoteProviderProfileListResponse, RemoteProviderRequest,
        RemoteProviderRunHealthProbesRequest, RemoteProviderRunHealthProbesResponse,
        RemoteTerminalWriteRequest, RemoteWorkbenchRequest, RuntimeAttachmentSnapshot,
        RuntimeAttachmentStatus, RuntimeClientId, RuntimeLeaseRole, RuntimeLeaseRoleCounts,
        RuntimeMaterializationStatus, RuntimeOptionAvailability, RuntimeProcessId,
        RuntimeProcessSnapshot, RuntimeSelectionInteraction, RuntimeSwitchId,
        SendAgentMessageRequest, SessionConfigValue, SessionRuntimeOption,
        SessionRuntimeOptionCatalog, SessionRuntimeSelection, SetDesiredAgentSessionRuntimeRequest,
        TerminalId, TerminalSession, TerminalStatus, TerminalWriteRequest, WorkspaceMode,
        unix_timestamp_ms,
    };
    use vibex_core::{
        ProviderHealthProbeKind, ProviderInjectionPreviewRequest, ProviderOptions,
        ProviderProfileCreateRequest, ProviderRunHealthProbesRequest,
    };
    use vibex_db::{
        RemoteAuditRepository, RemotePairingCodeRepository, SessionRepository,
        TerminalSessionRepository, TimelineRepository, WorkspaceRepository, apply_migrations,
        open_database,
    };

    struct TestRuntimeBackend {
        snapshot: Mutex<RuntimeBackendSnapshot>,
        materialize_calls: AtomicUsize,
    }

    struct TestRuntimeCatalogSource {
        catalog: SessionRuntimeOptionCatalog,
        calls: AtomicUsize,
    }

    struct TestWorktreeSnapshotSource {
        eligibility: vibex_core::GitProjectEligibility,
        snapshot: vibex_core::GitWorktreeLifecycleSnapshot,
    }

    impl TestRuntimeCatalogSource {
        fn new(session: &AgentSession) -> Self {
            let mut selection = remote_test_selection(session);
            selection.reasoning_effort = Some("high".to_string());
            selection.mode_id = Some("plan".to_string());
            Self {
                catalog: SessionRuntimeOptionCatalog {
                    revision: 9,
                    options: vec![SessionRuntimeOption {
                        selection,
                        agent_label: "Codex".to_string(),
                        provider_profile_label: "Work".to_string(),
                        model_label: "Mock Remote".to_string(),
                        reasoning_efforts: vec![SessionConfigValue {
                            value: "high".to_string(),
                            label: Some("High".to_string()),
                        }],
                        modes: vec![SessionConfigValue {
                            value: "plan".to_string(),
                            label: Some("Plan".to_string()),
                        }],
                        features: Vec::new(),
                        availability: RuntimeOptionAvailability::Available,
                    }],
                },
                calls: AtomicUsize::new(0),
            }
        }
    }

    #[async_trait::async_trait]
    impl RemoteRuntimeOptionCatalogSource for TestRuntimeCatalogSource {
        async fn list_runtime_options(&self) -> VibexResult<SessionRuntimeOptionCatalog> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Ok(self.catalog.clone())
        }
    }

    #[async_trait::async_trait]
    impl RemoteWorktreeSnapshotSource for TestWorktreeSnapshotSource {
        async fn worktree_eligibility(
            &self,
            _workspace_id: WorkspaceId,
        ) -> VibexResult<vibex_core::GitProjectEligibility> {
            Ok(self.eligibility.clone())
        }

        async fn worktree_snapshot(
            &self,
            _workspace_id: WorkspaceId,
        ) -> VibexResult<vibex_core::GitWorktreeLifecycleSnapshot> {
            Ok(self.snapshot.clone())
        }
    }

    impl TestRuntimeBackend {
        fn attachment() -> RuntimeAttachmentSnapshot {
            RuntimeAttachmentSnapshot {
                binding_id: vibex_core::RuntimeBindingId::new(),
                process_id: RuntimeProcessId::new(),
                activation_generation: 0,
                status: RuntimeAttachmentStatus::Ready,
                last_event_sequence: 0,
                current_model: None,
                current_mode: None,
                config_options: Vec::new(),
                active_message: None,
                active_tool_calls: Vec::new(),
                pending_permissions: Vec::new(),
                active_terminal_count: 0,
                active_background_work_count: 0,
                lease_counts: RuntimeLeaseRoleCounts::default(),
                usage: None,
            }
        }

        fn new(materialized: bool) -> Self {
            Self {
                snapshot: Mutex::new(RuntimeBackendSnapshot {
                    materialization_status: if materialized {
                        RuntimeMaterializationStatus::Available
                    } else {
                        RuntimeMaterializationStatus::NotMaterialized
                    },
                    attachment: materialized.then(Self::attachment),
                }),
                materialize_calls: AtomicUsize::new(0),
            }
        }

        fn evict(&self) {
            *self.snapshot.lock().unwrap() = RuntimeBackendSnapshot {
                materialization_status: RuntimeMaterializationStatus::NotMaterialized,
                attachment: None,
            };
        }
    }

    #[async_trait::async_trait]
    impl RuntimeLifecycleBackend for TestRuntimeBackend {
        fn snapshot(
            &self,
            _session_id: &vibex_core::VibexSessionId,
        ) -> VibexResult<RuntimeBackendSnapshot> {
            Ok(self.snapshot.lock().unwrap().clone())
        }

        fn process_snapshot(
            &self,
            _process_id: &RuntimeProcessId,
        ) -> VibexResult<RuntimeProcessSnapshot> {
            Err(VibexError::process(
                "test_runtime_process_missing",
                "test runtime process snapshot is unavailable",
            ))
        }

        async fn materialize_owner(
            &self,
            _session_id: &vibex_core::VibexSessionId,
        ) -> VibexResult<RuntimeBackendSnapshot> {
            self.materialize_calls.fetch_add(1, Ordering::SeqCst);
            let next = RuntimeBackendSnapshot {
                materialization_status: RuntimeMaterializationStatus::Available,
                attachment: Some(Self::attachment()),
            };
            *self.snapshot.lock().unwrap() = next.clone();
            Ok(next)
        }

        async fn sweep(
            &self,
            _now_ms: i64,
            _protected_targets: &[RuntimeLeaseTarget],
        ) -> VibexResult<RuntimeSweepReport> {
            Ok(RuntimeSweepReport::default())
        }
    }

    #[tokio::test]
    async fn health_returns_disabled_status_with_protocol_version() {
        let response = build_default_disabled_router()
            .oneshot(Request::get("/health").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let health: RemoteHealthStatus = serde_json::from_slice(&body).unwrap();

        assert_eq!(health.status, RemoteHealthState::Disabled);
        assert_eq!(health.protocol_version, RemoteProtocolVersion::foundation());
    }

    #[tokio::test]
    async fn info_reports_safe_loopback_disabled_defaults() {
        let response = build_default_disabled_router()
            .oneshot(Request::get("/api/info").body(Body::empty()).unwrap())
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        let info: RemoteServiceInfo = serde_json::from_slice(&body).unwrap();

        assert!(!info.remote_enabled);
        assert_eq!(info.bind_addr, "127.0.0.1:0");
        assert!(!info.public_listener_enabled);
    }

    #[tokio::test]
    async fn websocket_handshake_returns_metadata_and_preserves_ids() {
        let url = spawn_ws_server().await;
        let (mut client, _) = connect_async(url).await.unwrap();
        let correlation_id = CorrelationId::new();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::Handshake)
            .with_correlation_id(correlation_id.clone())
            .with_payload(serde_json::json!({
                "clientName": "test-web",
                "clientVersion": "0.0.0"
            }));
        let request_id = request.request_id.clone();

        client
            .send(ClientMessage::Text(
                serde_json::to_string(&request).unwrap().into(),
            ))
            .await
            .unwrap();

        let message = client.next().await.unwrap().unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_str(message.into_text().unwrap().as_ref()).unwrap();
        let payload: RemoteHandshakeResponse =
            serde_json::from_value(response.payload.unwrap()).unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(response.request_id, request_id);
        assert_eq!(response.correlation_id, Some(correlation_id));
        assert_eq!(
            payload.protocol_version,
            RemoteProtocolVersion::foundation()
        );
        assert!(payload.capabilities.supports_auth);
        assert!(payload.capabilities.supports_pairing);
    }

    #[tokio::test]
    async fn websocket_unsupported_operation_returns_structured_error() {
        let url = spawn_ws_server().await;
        let (mut client, _) = connect_async(url).await.unwrap();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::Unsupported);
        let request_id = request.request_id.clone();

        client
            .send(ClientMessage::Text(
                serde_json::to_string(&request).unwrap().into(),
            ))
            .await
            .unwrap();

        let message = client.next().await.unwrap().unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_str(message.into_text().unwrap().as_ref()).unwrap();
        let error = response.error.unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(response.request_id, request_id);
        assert_eq!(error.code, "remote_unsupported_operation");
    }

    #[test]
    fn trust_service_pairs_authenticates_authorizes_and_revokes_device() {
        let mut conn = vibex_db::DbConnection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let created = RemoteTrustService::create_pairing_code(
            &conn,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::ReadOnly,
                ttl_ms: Some(60_000),
            },
        )
        .unwrap();
        let stored_pairing =
            RemotePairingCodeRepository::get_by_hash(&conn, &hash_secret(&created.pairing_code))
                .unwrap()
                .unwrap();
        assert_ne!(stored_pairing.code_hash, created.pairing_code);

        let claimed = RemoteTrustService::claim_pairing_code(
            &conn,
            RemoteClaimPairingCodeRequest {
                pairing_code: created.pairing_code.clone(),
                display_name: "Pixel".to_string(),
                public_key: Some("pubkey".to_string()),
            },
        )
        .unwrap();
        assert_ne!(
            RemoteDeviceRepository::get(&conn, &claimed.device.device_id)
                .unwrap()
                .unwrap()
                .auth_secret_hash,
            claimed.auth_token
        );

        let auth = RemoteTrustService::authenticate(
            &conn,
            RemoteAuthProof {
                device_id: claimed.device.device_id.clone(),
                auth_token: claimed.auth_token.clone(),
            },
        )
        .unwrap();
        assert_eq!(auth.permission_level, RemoteDevicePermissionLevel::ReadOnly);
        RemoteTrustService::authorize_action(
            &conn,
            &auth,
            RemoteActionClass::ReadProject,
            None,
            None,
        )
        .unwrap();
        let denied = RemoteTrustService::authorize_action(
            &conn,
            &auth,
            RemoteActionClass::MutateGit,
            Some(RequestId::new()),
            Some(CorrelationId::new()),
        )
        .unwrap_err();
        assert_eq!(denied.code, "remote_permission_denied");

        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: Some(claimed.device.device_id.clone()),
                limit: Some(20),
            },
        )
        .unwrap();
        assert!(
            audits
                .iter()
                .any(|record| record.action == RemoteAuditAction::PermissionDenied)
        );
        assert!(
            audits
                .iter()
                .all(|record| !record.redacted_summary.contains(&claimed.auth_token))
        );

        RemoteTrustService::revoke_device(
            &conn,
            RemoteRevokeDeviceRequest {
                device_id: claimed.device.device_id.clone(),
                reason: Some("lost phone token secret".to_string()),
            },
        )
        .unwrap();
        let revoked = RemoteTrustService::authenticate(
            &conn,
            RemoteAuthProof {
                device_id: claimed.device.device_id,
                auth_token: claimed.auth_token,
            },
        )
        .unwrap_err();
        assert_eq!(revoked.code, "remote_device_revoked");
    }

    #[test]
    fn approve_only_devices_can_resolve_elicitation_with_dedicated_audit_target() {
        assert!(permission_allows(
            RemoteDevicePermissionLevel::ApproveOnly,
            RemoteActionClass::ResolveElicitation,
        ));
        assert!(!permission_allows(
            RemoteDevicePermissionLevel::ReadOnly,
            RemoteActionClass::ResolveElicitation,
        ));
        assert_eq!(
            audit_target_for_action(RemoteActionClass::ResolveElicitation),
            RemoteAuditTargetKind::Elicitation
        );
    }

    #[test]
    fn trust_service_rejects_invalid_and_expired_pairing_codes() {
        let mut conn = vibex_db::DbConnection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let invalid = RemoteTrustService::claim_pairing_code(
            &conn,
            RemoteClaimPairingCodeRequest {
                pairing_code: "invalid-code".to_string(),
                display_name: "Phone".to_string(),
                public_key: None,
            },
        )
        .unwrap_err();
        assert_eq!(invalid.code, "remote_pairing_code_invalid");

        let created = RemoteTrustService::create_pairing_code(
            &conn,
            RemoteCreatePairingCodeRequest {
                permission_level: RemoteDevicePermissionLevel::ApproveOnly,
                ttl_ms: Some(1),
            },
        )
        .unwrap();
        std::thread::sleep(std::time::Duration::from_millis(2));
        let expired = RemoteTrustService::claim_pairing_code(
            &conn,
            RemoteClaimPairingCodeRequest {
                pairing_code: created.pairing_code,
                display_name: "Phone".to_string(),
                public_key: None,
            },
        )
        .unwrap_err();
        assert_eq!(expired.code, "remote_pairing_code_expired");
    }

    #[tokio::test]
    async fn remote_agent_read_only_lists_sessions_and_catches_up_timeline() {
        let (db_path, manager) = test_agent_manager("read");
        let session = create_mock_session(&manager, "Remote read").await;
        append_mock_timeline(&manager, &session, "hello remote");
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router =
            build_router_with_agent(RemoteServiceConfig::loopback_disabled(), manager.clone());

        let list = post_agent(
            router.clone(),
            RemoteAgentRequest::ListSessions(RemoteAgentSessionListRequest {
                auth: auth.clone(),
                include_archived: Some(false),
                timeline_limit: Some(10),
            }),
        )
        .await;
        let list_payload: RemoteAgentSessionListResponse =
            serde_json::from_value(list.payload.unwrap()).unwrap();
        assert_eq!(list_payload.sessions.len(), 1);
        assert_eq!(list_payload.sessions[0].session.id, session.id);

        let deep_link = post_agent(
            router.clone(),
            RemoteAgentRequest::ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest {
                auth: auth.clone(),
                notification_id: "notification-read".to_string(),
                opaque_locator: session.id.as_str().to_string(),
            }),
        )
        .await;
        let deep_link: RemoteAgentDeepLinkResolveResponse =
            serde_json::from_value(deep_link.payload.unwrap()).unwrap();
        assert_eq!(
            deep_link.resolution.status,
            RemoteDeepLinkResolutionStatus::Resolved
        );
        assert_eq!(deep_link.resolution.session_id.as_ref(), Some(&session.id));

        let missing = post_agent(
            router.clone(),
            RemoteAgentRequest::ResolveOpaqueLocator(RemoteAgentDeepLinkResolveRequest {
                auth: auth.clone(),
                notification_id: "notification-missing".to_string(),
                opaque_locator: "opaque-missing".to_string(),
            }),
        )
        .await;
        let missing: RemoteAgentDeepLinkResolveResponse =
            serde_json::from_value(missing.payload.unwrap()).unwrap();
        assert_eq!(
            missing.resolution.status,
            RemoteDeepLinkResolutionStatus::NotFound
        );
        assert!(missing.resolution.session_id.is_none());

        let catch_up = post_agent(
            router,
            RemoteAgentRequest::CatchUp(RemoteAgentCatchUpRequest {
                auth,
                cursors: vec![RemoteAgentTimelineCursor {
                    session_id: session.id.clone(),
                    after_sequence: 0,
                }],
                limit: Some(100),
            }),
        )
        .await;
        let catch_up_payload: RemoteAgentCatchUpResponse =
            serde_json::from_value(catch_up.payload.unwrap()).unwrap();
        assert!(!catch_up_payload.events.is_empty());
        assert_eq!(
            catch_up_payload.events[0].channel,
            RemoteLiveEventChannel::AgentSession
        );
        assert_eq!(
            catch_up_payload.next_cursors[0].session_id,
            session.id.clone()
        );
        assert!(catch_up_payload.next_cursors[0].after_sequence > 0);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_agent_read_only_denies_send_and_does_not_audit_prompt_text() {
        let (db_path, manager) = test_agent_manager("deny");
        let session = create_mock_session(&manager, "Remote deny").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent(RemoteServiceConfig::loopback_disabled(), manager);

        let response = post_agent(
            router,
            RemoteAgentRequest::SendMessage(RemoteAgentSendMessageRequest {
                auth: auth.clone(),
                request: test_send_request(
                    &session,
                    "remote-denied",
                    "prompt body with token secret",
                ),
            }),
        )
        .await;
        let error = response.error.unwrap();
        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(error.code, "remote_permission_denied");

        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: Some(auth.device_id),
                limit: Some(20),
            },
        )
        .unwrap();
        assert!(
            audits
                .iter()
                .any(|record| record.action == RemoteAuditAction::PermissionDenied)
        );
        assert!(audits.iter().all(|record| {
            !record.redacted_summary.contains("prompt body")
                && !record.redacted_summary.contains("token secret")
        }));

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_runtime_query_reports_unavailable_without_production_wiring() {
        let (db_path, manager) = test_agent_manager("runtime-query-unavailable");
        let session = create_mock_session(&manager, "Runtime query unavailable").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent(RemoteServiceConfig::loopback_disabled(), manager);

        let response = post_agent(
            router,
            RemoteAgentRequest::GetRuntimeSelection(RemoteAgentRuntimeSelectionRequest {
                auth,
                session_id: session.id,
            }),
        )
        .await;

        assert_eq!(response.status, RemoteEnvelopeStatus::Error);
        assert_eq!(
            response.error.unwrap().code,
            "remote_agent_runtime_selection_unavailable"
        );
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_runtime_catalog_uses_injected_source_and_read_authorization() {
        let (db_path, manager) = test_agent_manager("runtime-catalog");
        let session = create_mock_session(&manager, "Runtime catalog").await;
        let reader = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let source = Arc::new(TestRuntimeCatalogSource::new(&session));
        let dispatcher =
            RemoteDispatcher::with_agent_manager(RemoteServiceConfig::loopback_disabled(), manager)
                .with_runtime_option_catalog_source(source.clone());
        assert!(
            !dispatcher
                .info()
                .capabilities
                .supports_seamless_runtime_selection
        );
        let router = build_router_with_dispatcher(dispatcher);

        let response = post_agent(
            router,
            RemoteAgentRequest::ListRuntimeOptions(RemoteAgentRuntimeOptionsRequest {
                auth: reader.clone(),
            }),
        )
        .await;
        let encoded = serde_json::to_string(&response).unwrap();
        let payload: RemoteAgentRuntimeOptionsResponse =
            serde_json::from_value(response.payload.unwrap()).unwrap();

        assert_eq!(payload.catalog.revision, 9);
        assert_eq!(payload.catalog.options.len(), 1);
        assert_eq!(payload.catalog.options[0].agent_label, "Codex");
        assert_eq!(source.calls.load(Ordering::SeqCst), 1);
        assert!(!encoded.contains(&reader.auth_token));
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_runtime_selection_mutations_require_full_control_before_dispatch() {
        let (db_path, manager) = test_agent_manager("runtime-selection-auth");
        let session = create_mock_session(&manager, "Runtime selection auth").await;
        let reader = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let controller = pair_device(
            &db_path,
            RemoteDevicePermissionLevel::FullControl,
            "Controller",
        );
        let router = build_router_with_agent(RemoteServiceConfig::loopback_disabled(), manager);
        let desired = test_send_request(&session, "selection-auth", "unused").desired_runtime;

        let denied_set = post_agent(
            router.clone(),
            RemoteAgentRequest::SetDesiredRuntime(RemoteAgentSetDesiredRuntimeRequest {
                auth: reader.clone(),
                request: SetDesiredAgentSessionRuntimeRequest {
                    session_id: session.id.clone(),
                    idempotency_key: "selection-auth-set".to_string(),
                    expected_revision: 0,
                    expected_selection_revision: 0,
                    desired,
                    interaction: RuntimeSelectionInteraction::Seamless,
                },
            }),
        )
        .await;
        let denied_cancel = post_agent(
            router.clone(),
            RemoteAgentRequest::CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest {
                auth: reader,
                request: CancelAgentSessionRuntimeSwitchRequest {
                    session_id: session.id.clone(),
                    switch_id: RuntimeSwitchId::new(),
                },
            }),
        )
        .await;
        let authorized_without_service = post_agent(
            router,
            RemoteAgentRequest::CancelRuntimeSwitch(RemoteAgentCancelRuntimeSwitchRequest {
                auth: controller,
                request: CancelAgentSessionRuntimeSwitchRequest {
                    session_id: session.id,
                    switch_id: RuntimeSwitchId::new(),
                },
            }),
        )
        .await;

        assert_eq!(denied_set.error.unwrap().code, "remote_permission_denied");
        assert_eq!(
            denied_cancel.error.unwrap().code,
            "remote_permission_denied"
        );
        assert_eq!(
            authorized_without_service.error.unwrap().code,
            "remote_agent_runtime_selection_unavailable"
        );
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_runtime_attach_enforces_role_permission_and_device_scoped_detach() {
        let (db_path, manager) = test_agent_manager("runtime-auth-matrix");
        let session = create_mock_session(&manager, "Runtime auth matrix").await;
        let reader = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let other_reader = pair_device(
            &db_path,
            RemoteDevicePermissionLevel::ReadOnly,
            "Other reader",
        );
        let controller = pair_device(
            &db_path,
            RemoteDevicePermissionLevel::FullControl,
            "Controller",
        );
        let backend = Arc::new(TestRuntimeBackend::new(true));
        let lifecycle = Arc::new(
            RuntimeLifecycleService::new(backend.clone(), RuntimeLifecycleConfig::default())
                .unwrap(),
        );
        let mut capabilities = RemoteCapabilitySummary::with_agent_sessions();
        capabilities.supports_runtime_lifecycle = true;
        let dispatcher = RemoteDispatcher {
            state: RemoteRouterState {
                config: RemoteServiceConfig::loopback_disabled(),
                capabilities,
                agent_manager: Some(manager),
                runtime_selection: None,
                runtime_lifecycle: Some(lifecycle.clone()),
                message_submission: None,
                runtime_catalog: None,
                workbench: None,
                provider: None,
            },
        };
        let router = build_router_with_dispatcher(dispatcher);

        let viewer_client = RuntimeClientId::new();
        let viewer = post_agent(
            router.clone(),
            RemoteAgentRequest::AttachRuntime(RemoteAgentAttachRuntimeRequest {
                auth: reader.clone(),
                request: AttachRuntimeRequest {
                    session_id: session.id.clone(),
                    client_id: viewer_client.clone(),
                    role: RuntimeLeaseRole::Viewer,
                },
            }),
        )
        .await;
        let viewer: RemoteAgentAttachRuntimeResponse =
            serde_json::from_value(viewer.payload.unwrap()).unwrap();
        assert!(viewer.response.lease_id.is_some());
        assert_eq!(backend.materialize_calls.load(Ordering::SeqCst), 0);

        backend.evict();
        let denied_owner = post_agent(
            router.clone(),
            RemoteAgentRequest::AttachRuntime(RemoteAgentAttachRuntimeRequest {
                auth: reader.clone(),
                request: AttachRuntimeRequest {
                    session_id: session.id.clone(),
                    client_id: RuntimeClientId::new(),
                    role: RuntimeLeaseRole::Owner,
                },
            }),
        )
        .await;
        assert_eq!(denied_owner.error.unwrap().code, "remote_permission_denied");
        assert_eq!(backend.materialize_calls.load(Ordering::SeqCst), 0);

        let forged = post_agent(
            router.clone(),
            RemoteAgentRequest::AttachRuntime(RemoteAgentAttachRuntimeRequest {
                auth: reader,
                request: AttachRuntimeRequest {
                    session_id: session.id.clone(),
                    client_id: RuntimeClientId::new(),
                    role: RuntimeLeaseRole::BackgroundWorker,
                },
            }),
        )
        .await;
        assert_eq!(
            forged.error.unwrap().code,
            "runtime_lease_internal_role_forbidden"
        );
        assert_eq!(backend.materialize_calls.load(Ordering::SeqCst), 0);

        let owner_client = RuntimeClientId::new();
        let owner = post_agent(
            router.clone(),
            RemoteAgentRequest::AttachRuntime(RemoteAgentAttachRuntimeRequest {
                auth: controller.clone(),
                request: AttachRuntimeRequest {
                    session_id: session.id.clone(),
                    client_id: owner_client.clone(),
                    role: RuntimeLeaseRole::Owner,
                },
            }),
        )
        .await;
        let owner: RemoteAgentAttachRuntimeResponse =
            serde_json::from_value(owner.payload.unwrap()).unwrap();
        assert!(owner.response.lease_id.is_some());
        assert_eq!(backend.materialize_calls.load(Ordering::SeqCst), 1);

        let isolated = post_agent(
            router.clone(),
            RemoteAgentRequest::DetachRuntime(RemoteAgentDetachRuntimeRequest {
                auth: other_reader,
                request: DetachRuntimeRequest {
                    session_id: session.id.clone(),
                    client_id: owner_client.clone(),
                },
            }),
        )
        .await;
        let isolated: RemoteAgentDetachRuntimeResponse =
            serde_json::from_value(isolated.payload.unwrap()).unwrap();
        assert!(!isolated.response.released);

        let detached = post_agent(
            router,
            RemoteAgentRequest::DetachRuntime(RemoteAgentDetachRuntimeRequest {
                auth: controller,
                request: DetachRuntimeRequest {
                    session_id: session.id,
                    client_id: owner_client,
                },
            }),
        )
        .await;
        let detached: RemoteAgentDetachRuntimeResponse =
            serde_json::from_value(detached.payload.unwrap()).unwrap();
        assert!(detached.response.released);
        lifecycle.stop().await.unwrap();
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn in_process_dispatcher_matches_http_agent_dispatch() {
        let (db_path, manager) = test_agent_manager("dispatcher");
        let session = create_mock_session(&manager, "Remote dispatcher").await;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let config = RemoteServiceConfig::loopback_disabled();
        let router = build_router_with_agent(config.clone(), manager.clone());
        let dispatcher = RemoteDispatcher::with_agent_manager(config, manager);
        let correlation_id = CorrelationId::new();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession)
            .with_correlation_id(correlation_id.clone())
            .with_payload(
                serde_json::to_value(RemoteAgentRequest::ListSessions(
                    RemoteAgentSessionListRequest {
                        auth,
                        include_archived: Some(false),
                        timeline_limit: Some(10),
                    },
                ))
                .unwrap(),
            );

        let http_response = post_envelope(router, "/api/agent", request.clone()).await;
        let direct_response = dispatcher.dispatch(request.clone()).await;
        let http_payload: RemoteAgentSessionListResponse =
            serde_json::from_value(http_response.payload.clone().unwrap()).unwrap();
        let direct_payload: RemoteAgentSessionListResponse =
            serde_json::from_value(direct_response.payload.clone().unwrap()).unwrap();

        assert_eq!(http_response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(direct_response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(http_response.request_id, request.request_id);
        assert_eq!(direct_response.request_id, request.request_id);
        assert_eq!(http_response.correlation_id, Some(correlation_id.clone()));
        assert_eq!(direct_response.correlation_id, Some(correlation_id));
        assert_eq!(http_payload.sessions.len(), 1);
        assert_eq!(direct_payload.sessions.len(), 1);
        assert_eq!(http_payload.sessions[0].session.id, session.id);
        assert_eq!(direct_payload.sessions[0].session.id, session.id);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn websocket_agent_operation_uses_authenticated_dispatch() {
        let (db_path, manager) = test_agent_manager("ws-agent");
        let session = create_mock_session(&manager, "Remote websocket").await;
        append_mock_timeline(&manager, &session, "hello websocket catch-up");
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Browser");
        let url = spawn_ws_agent_server(manager).await;
        let (mut client, _) = connect_async(url).await.unwrap();
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession).with_payload(
            serde_json::to_value(RemoteAgentRequest::CatchUp(RemoteAgentCatchUpRequest {
                auth,
                cursors: vec![RemoteAgentTimelineCursor {
                    session_id: session.id.clone(),
                    after_sequence: 0,
                }],
                limit: Some(100),
            }))
            .unwrap(),
        );
        let request_id = request.request_id.clone();

        client
            .send(ClientMessage::Text(
                serde_json::to_string(&request).unwrap().into(),
            ))
            .await
            .unwrap();

        let message = client.next().await.unwrap().unwrap();
        let response: RemoteResponseEnvelope =
            serde_json::from_str(message.into_text().unwrap().as_ref()).unwrap();
        let payload: RemoteAgentCatchUpResponse =
            serde_json::from_value(response.payload.unwrap()).unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(response.request_id, request_id);
        assert!(!payload.events.is_empty());
        assert_eq!(payload.next_cursors[0].session_id, session.id);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_workbench_read_only_reads_workspace_file() {
        let (db_path, manager) = test_agent_manager("workbench-read");
        let workspace_root = temp_workspace_root("read");
        std::fs::create_dir_all(workspace_root.join("src")).unwrap();
        std::fs::write(workspace_root.join("src/lib.rs"), "pub fn marker() {}\n").unwrap();
        let workspace_id = ensure_workspace(&db_path, &workspace_root).id;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent_and_workbench(
            RemoteServiceConfig::loopback_disabled(),
            manager,
            RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new()),
        );

        let response = post_workbench(
            router,
            RemoteOperationKind::WorkspaceFile,
            RemoteWorkbenchRequest::FileRead(RemoteFileReadRequest {
                auth,
                request: FileReadRequest {
                    workspace_id,
                    path: "src/lib.rs".to_string(),
                    max_bytes: Some(1024),
                },
            }),
        )
        .await;
        let payload: RemoteFileReadResponse =
            serde_json::from_value(response.payload.unwrap()).unwrap();

        assert_eq!(response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(payload.file.path, "src/lib.rs");
        assert_eq!(
            payload.file.content.as_deref(),
            Some("pub fn marker() {}\n")
        );

        cleanup_db(db_path);
        cleanup_workspace(workspace_root);
    }

    #[tokio::test]
    async fn remote_workbench_worktree_reads_use_the_injected_authority() {
        let (db_path, manager) = test_agent_manager("worktree-read");
        let workspace_root = temp_workspace_root("worktree-read");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let workspace = ensure_workspace(&db_path, &workspace_root);
        let repository_identity = vibex_core::GitRepositoryIdentity {
            repository_root: vibex_git::canonical_path_identity(workspace_root.join("repository")),
            git_common_dir: vibex_git::canonical_path_identity(workspace_root.join("private.git")),
            comparison_key: format!("private-repository:{}", workspace_root.display()),
        };
        let eligibility = vibex_core::GitProjectEligibility {
            project_id: workspace.project_id.clone(),
            project_canonical_path: vibex_git::canonical_path_identity(&workspace_root),
            state: vibex_core::GitProjectEligibilityState::Eligible,
            repository_identity: Some(repository_identity.clone()),
            current_branch: Some("main".to_string()),
            default_base_ref: Some("main".to_string()),
            selectable_base_refs: vec!["main".to_string()],
            observed_head: Some("a".repeat(40)),
            revision: "test-worktree-eligibility".to_string(),
            disabled_reason: None,
        };
        let worktree_id = RequestId::new();
        let operation_id = RequestId::new();
        let worktree_path = workspace_root.join("private-worktree");
        let managed = vibex_core::GitManagedWorktreeRecord {
            worktree_id: worktree_id.clone(),
            project_id: workspace.project_id.clone(),
            workspace_id: Some(workspace.id.clone()),
            repo_root: workspace_root.join("repository").display().to_string(),
            worktree_path: worktree_path.display().to_string(),
            repository_identity: Some(repository_identity.clone()),
            worktree_path_identity: Some(vibex_git::canonical_path_identity(&worktree_path)),
            branch: Some("feature/remote".to_string()),
            origin_workspace_id: Some(workspace.id.clone()),
            base_ref: Some("main".to_string()),
            base_head: Some("a".repeat(40)),
            target_workspace_id: Some(workspace.id.clone()),
            target_branch: Some("main".to_string()),
            head: Some("b".repeat(40)),
            status: vibex_core::GitManagedWorktreeStatus::Active,
            reconciliation_state: vibex_core::GitWorktreeReconciliationState::Consistent,
            diagnostic: None,
            created_at_ms: 1,
            updated_at_ms: 2,
            closed_at_ms: None,
        };
        let operation = vibex_core::GitWorktreeOperationRecord {
            operation_id,
            project_id: workspace.project_id.clone(),
            source_workspace_id: Some(workspace.id.clone()),
            target_workspace_id: Some(workspace.id.clone()),
            operation: vibex_core::GitWorktreeOperationKind::MergeBack,
            status: vibex_core::GitWorktreeOperationStatus::Queued,
            worktree_path: Some(worktree_path.display().to_string()),
            branch: Some("feature/remote".to_string()),
            base_ref: Some("main".to_string()),
            head_before: Some("a".repeat(40)),
            head_after: None,
            error: None,
            detail: vibex_core::GitWorktreeOperationDetail {
                idempotency_key: Some("private-idempotency".to_string()),
                request_fingerprint: Some("private-fingerprint".to_string()),
                repository_identity: Some(repository_identity),
                source_path_identity: Some(vibex_git::canonical_path_identity(&worktree_path)),
                target_path_identity: Some(vibex_git::canonical_path_identity(&workspace_root)),
                lock_keys: vec![vibex_core::GitWorktreeLockKey {
                    kind: vibex_core::GitWorktreeLockKind::Repository,
                    key: format!("private-lock:{}", workspace_root.display()),
                }],
                preflight_revision: Some("private-preflight".to_string()),
                lease_owner: Some("private-lease-owner".to_string()),
                lease_expires_at_ms: Some(99),
                queue_key: Some("private-queue-key".to_string()),
                queue_position: Some(2),
                ..vibex_core::GitWorktreeOperationDetail::default()
            },
            created_at_ms: 1,
            updated_at_ms: 2,
        };
        let snapshot = vibex_core::GitWorktreeLifecycleSnapshot {
            workspace_id: workspace.id.clone(),
            eligibility: eligibility.clone(),
            managed_worktrees: vec![managed],
            operations: vec![operation],
            readiness: vec![vibex_core::GitWorktreeReadinessRecord {
                worktree_id,
                workspace_id: workspace.id.clone(),
                state: vibex_core::GitWorktreeReadinessState::ReadyToMerge,
                source_head: "b".repeat(40),
                dirty_fingerprint: "private-dirty-fingerprint".to_string(),
                target_workspace_id: workspace.id.clone(),
                target_branch: "main".to_string(),
                checks: vec![vibex_core::GitWorktreeCheckRecord {
                    command: format!(
                        "private-check --workspace {} --token secret",
                        workspace_root.display()
                    ),
                    outcome: vibex_core::GitWorktreeCheckOutcome::Passed,
                    recorded_at_ms: 42,
                }],
                revision: "readiness-revision".to_string(),
                updated_at_ms: 43,
            }],
            diagnostics: Vec::new(),
            revision: "test-worktree-snapshot".to_string(),
        };
        let source = Arc::new(TestWorktreeSnapshotSource {
            eligibility: eligibility.clone(),
            snapshot: snapshot.clone(),
        });
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent_and_workbench(
            RemoteServiceConfig::loopback_disabled(),
            manager,
            RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new())
                .with_worktree_snapshot_source(source),
        );

        let eligibility_response = post_workbench(
            router.clone(),
            RemoteOperationKind::Git,
            RemoteWorkbenchRequest::GitWorktreeEligibility(
                vibex_core::RemoteGitWorktreeEligibilityRequest {
                    auth: auth.clone(),
                    workspace_id: workspace.id.clone(),
                },
            ),
        )
        .await;
        let eligibility_payload: vibex_core::RemoteGitWorktreeEligibilityResponse =
            serde_json::from_value(eligibility_response.payload.unwrap()).unwrap();
        assert_eq!(eligibility_response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(
            eligibility_payload.eligibility,
            sanitize_remote_worktree_eligibility(eligibility)
        );

        let snapshot_response = post_workbench(
            router,
            RemoteOperationKind::Git,
            RemoteWorkbenchRequest::GitWorktreeSnapshot(
                vibex_core::RemoteGitWorktreeSnapshotRequest {
                    auth,
                    workspace_id: workspace.id.clone(),
                },
            ),
        )
        .await;
        let snapshot_payload: vibex_core::RemoteGitWorktreeSnapshotResponse =
            serde_json::from_value(snapshot_response.payload.unwrap()).unwrap();
        assert_eq!(snapshot_response.status, RemoteEnvelopeStatus::Ok);
        assert_eq!(
            snapshot_payload.snapshot,
            sanitize_remote_worktree_snapshot(snapshot)
        );
        let encoded = serde_json::to_string(&snapshot_payload).unwrap();
        assert!(!encoded.contains(workspace_root.to_string_lossy().as_ref()));
        for private_value in [
            "private-idempotency",
            "private-fingerprint",
            "private-preflight",
            "private-lease-owner",
            "private-queue-key",
            "private-lock",
            "private-check",
            "--token secret",
        ] {
            assert!(!encoded.contains(private_value));
        }
        assert_eq!(
            snapshot_payload.snapshot.operations[0]
                .detail
                .queue_position,
            Some(2)
        );
        assert_eq!(
            snapshot_payload.snapshot.readiness[0].checks[0].command,
            "recorded-check"
        );
        assert_eq!(
            snapshot_payload.snapshot.readiness[0].checks[0].outcome,
            vibex_core::GitWorktreeCheckOutcome::Passed
        );
        assert_eq!(
            snapshot_payload.snapshot.readiness[0].checks[0].recorded_at_ms,
            42
        );
        assert!(
            !snapshot_payload.snapshot.managed_worktrees[0]
                .worktree_path_identity
                .as_ref()
                .unwrap()
                .exists
        );

        cleanup_db(db_path);
        cleanup_workspace(workspace_root);
    }

    #[test]
    fn terminal_list_restores_missing_open_terminals() {
        let (db_path, _manager) = test_agent_manager("terminal-restore");
        let workspace_root = temp_workspace_root("terminal-restore");
        let nested_cwd = workspace_root.join("src");
        std::fs::create_dir_all(&workspace_root).unwrap();
        std::fs::create_dir_all(&nested_cwd).unwrap();
        let workspace = ensure_workspace(&db_path, &workspace_root);
        let now = unix_timestamp_ms();
        let running_terminal_id = TerminalId::new();
        let stale_terminal_id = TerminalId::new();
        let stored_running_terminal = TerminalSession {
            id: running_terminal_id.clone(),
            workspace_id: workspace.id.clone(),
            title: "running shell".to_string(),
            shell: "/bin/sh".to_string(),
            cwd: nested_cwd.to_string_lossy().to_string(),
            rows: 24,
            cols: 80,
            status: TerminalStatus::Running,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: None,
        };
        let stored_stale_terminal = TerminalSession {
            id: stale_terminal_id.clone(),
            workspace_id: workspace.id.clone(),
            title: "old stale shell".to_string(),
            shell: "/bin/sh".to_string(),
            cwd: workspace.root_path.clone(),
            rows: 30,
            cols: 100,
            status: TerminalStatus::Stale,
            created_at_ms: now,
            updated_at_ms: now,
            closed_at_ms: Some(now),
        };
        {
            let mut conn = open_database(&db_path).unwrap();
            apply_migrations(&mut conn).unwrap();
            TerminalSessionRepository::upsert(&conn, &stored_running_terminal).unwrap();
            TerminalSessionRepository::upsert(&conn, &stored_stale_terminal).unwrap();
        }
        let runtime = RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new());

        let listed = terminal_list_for_workspace(&runtime, &workspace.id).unwrap();

        let recovered_running = listed
            .iter()
            .find(|session| session.id == running_terminal_id)
            .unwrap();
        assert_eq!(recovered_running.status, TerminalStatus::Running);
        assert_eq!(
            recovered_running.cwd,
            nested_cwd
                .canonicalize()
                .unwrap()
                .to_string_lossy()
                .to_string()
        );
        assert!(recovered_running.closed_at_ms.is_none());
        let recovered_stale = listed
            .iter()
            .find(|session| session.id == stale_terminal_id)
            .unwrap();
        assert_eq!(recovered_stale.status, TerminalStatus::Running);
        assert!(recovered_stale.closed_at_ms.is_none());
        runtime
            .terminals
            .snapshot(&running_terminal_id)
            .expect("restored terminal should have a live PTY");
        let conn = open_database(&db_path).unwrap();
        let persisted = TerminalSessionRepository::list(&conn, &workspace.id).unwrap();
        assert_eq!(
            persisted
                .iter()
                .filter(|session| session.status == TerminalStatus::Running)
                .count(),
            2
        );

        cleanup_db(db_path);
        cleanup_workspace(workspace_root);
    }

    #[tokio::test]
    async fn remote_workbench_read_only_denies_file_git_and_terminal_mutations_with_redacted_audit()
    {
        let (db_path, manager) = test_agent_manager("workbench-deny");
        let workspace_root = temp_workspace_root("deny");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let workspace_id = ensure_workspace(&db_path, &workspace_root).id;
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent_and_workbench(
            RemoteServiceConfig::loopback_disabled(),
            manager,
            RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new()),
        );

        let file_response = post_workbench(
            router.clone(),
            RemoteOperationKind::WorkspaceFile,
            RemoteWorkbenchRequest::FileWrite(RemoteFileWriteRequest {
                auth: auth.clone(),
                request: FileWriteRequest {
                    workspace_id: workspace_id.clone(),
                    path: "secret.txt".to_string(),
                    content: "payload token secret should not be audited".to_string(),
                    create_if_missing: true,
                    expected_revision: None,
                    encoding: None,
                    line_ending: None,
                },
            }),
        )
        .await;
        let git_response = post_workbench(
            router.clone(),
            RemoteOperationKind::Git,
            RemoteWorkbenchRequest::GitStage(RemoteGitStageRequest {
                auth: auth.clone(),
                request: GitStageRequest {
                    workspace_id: workspace_id.clone(),
                    paths: vec!["secret.txt".to_string()],
                },
            }),
        )
        .await;
        let terminal_response = post_workbench(
            router,
            RemoteOperationKind::Terminal,
            RemoteWorkbenchRequest::TerminalWrite(RemoteTerminalWriteRequest {
                auth: auth.clone(),
                request: TerminalWriteRequest {
                    terminal_id: TerminalId::new(),
                    data: "terminal token secret should not be audited".to_string(),
                },
            }),
        )
        .await;

        assert_eq!(
            file_response.error.unwrap().code,
            "remote_permission_denied"
        );
        assert_eq!(git_response.error.unwrap().code, "remote_permission_denied");
        assert_eq!(
            terminal_response.error.unwrap().code,
            "remote_permission_denied"
        );
        assert!(!workspace_root.join("secret.txt").exists());

        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: Some(auth.device_id),
                limit: Some(50),
            },
        )
        .unwrap();

        assert!(
            audits
                .iter()
                .filter(|record| record.action == RemoteAuditAction::PermissionDenied)
                .count()
                >= 3
        );
        assert!(audits.iter().all(|record| {
            !record.redacted_summary.contains("payload")
                && !record.redacted_summary.contains("terminal")
                && !record.redacted_summary.contains("should not be audited")
                && !record.redacted_summary.contains("secret")
                && !record.redacted_summary.contains(&auth.auth_token)
        }));

        cleanup_db(db_path);
        cleanup_workspace(workspace_root);
    }

    #[tokio::test]
    async fn remote_provider_read_only_lists_profiles_and_previews_redacted_injection() {
        let (db_path, manager) = test_agent_manager("provider-read");
        let profile = create_provider_profile(&db_path, "Remote Provider");
        let auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let router = build_router_with_agent_and_workbench(
            RemoteServiceConfig::loopback_disabled(),
            manager,
            RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new()),
        );

        let info_response = router
            .clone()
            .oneshot(Request::get("/api/info").body(Body::empty()).unwrap())
            .await
            .unwrap();
        let info_body = to_bytes(info_response.into_body(), usize::MAX)
            .await
            .unwrap();
        let info: RemoteServiceInfo = serde_json::from_slice(&info_body).unwrap();
        assert!(info.capabilities.supports_provider_settings);
        assert!(
            info.capabilities
                .live_event_channels
                .contains(&RemoteLiveEventChannel::Provider)
        );

        let list = post_provider(
            router.clone(),
            RemoteProviderRequest::ListProfiles(RemoteProviderProfileListRequest {
                auth: auth.clone(),
            }),
        )
        .await;
        let list_payload: RemoteProviderProfileListResponse =
            serde_json::from_value(list.payload.unwrap()).unwrap();
        assert!(
            list_payload
                .profiles
                .iter()
                .any(|summary| summary.id == profile.id)
        );

        let preview = post_provider(
            router,
            RemoteProviderRequest::PreviewInjection(RemoteProviderInjectionPreviewRequest {
                auth,
                request: ProviderInjectionPreviewRequest {
                    provider_profile_id: profile.id.clone(),
                    project_id: None,
                    workspace_id: None,
                    session_id: None,
                    persist: true,
                },
            }),
        )
        .await;
        let preview_payload: RemoteProviderInjectionPreviewResponse =
            serde_json::from_value(preview.payload.unwrap()).unwrap();
        assert_eq!(preview_payload.preview.profile.id, profile.id);
        assert!(
            preview_payload
                .preview
                .env
                .iter()
                .all(|field| field.value.contains("missing") || field.value.contains("redacted"))
        );

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn remote_provider_health_probes_require_full_control_and_audit_redacted_summary() {
        let (db_path, manager) = test_agent_manager("provider-probes");
        let profile = create_provider_profile(&db_path, "Remote Probe Provider");
        let reader_auth = pair_device(&db_path, RemoteDevicePermissionLevel::ReadOnly, "Reader");
        let full_auth = pair_device(
            &db_path,
            RemoteDevicePermissionLevel::FullControl,
            "Controller",
        );
        let router = build_router_with_agent_and_workbench(
            RemoteServiceConfig::loopback_disabled(),
            manager,
            RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new()),
        );

        let denied = post_provider(
            router.clone(),
            RemoteProviderRequest::RunHealthProbes(RemoteProviderRunHealthProbesRequest {
                auth: reader_auth.clone(),
                request: ProviderRunHealthProbesRequest {
                    provider_profile_ids: Some(vec![profile.id.clone()]),
                    probe_kinds: Some(vec![ProviderHealthProbeKind::AuthStatus]),
                },
            }),
        )
        .await;
        assert_eq!(denied.error.unwrap().code, "remote_permission_denied");

        let allowed = post_provider(
            router.clone(),
            RemoteProviderRequest::RunHealthProbes(RemoteProviderRunHealthProbesRequest {
                auth: full_auth.clone(),
                request: ProviderRunHealthProbesRequest {
                    provider_profile_ids: Some(vec![profile.id.clone()]),
                    probe_kinds: Some(vec![ProviderHealthProbeKind::AuthStatus]),
                },
            }),
        )
        .await;
        let allowed_payload: RemoteProviderRunHealthProbesResponse =
            serde_json::from_value(allowed.payload.unwrap()).unwrap();
        assert_eq!(allowed.status, RemoteEnvelopeStatus::Ok);
        assert!(!allowed_payload.result.results.is_empty());

        let summaries = post_provider(
            router,
            RemoteProviderRequest::ListHealthSummaries(RemoteProviderHealthSummaryListRequest {
                auth: reader_auth.clone(),
            }),
        )
        .await;
        let summaries_payload: RemoteProviderHealthSummaryListResponse =
            serde_json::from_value(summaries.payload.unwrap()).unwrap();
        assert!(
            summaries_payload
                .summaries
                .iter()
                .any(|summary| summary.profile.id == profile.id)
        );

        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let audits = RemoteAuditRepository::list(
            &conn,
            &RemoteAuditListRequest {
                device_id: Some(full_auth.device_id.clone()),
                limit: Some(50),
            },
        )
        .unwrap();
        assert!(audits.iter().any(|record| {
            record.target_kind == RemoteAuditTargetKind::ProviderSettings
                && record.target_id.as_deref() == Some("provider_run_health_probes")
                && record.action == RemoteAuditAction::MutationAllowed
        }));
        assert!(audits.iter().all(|record| {
            !record.redacted_summary.contains(&full_auth.auth_token)
                && !record.redacted_summary.contains("auth-token")
        }));

        cleanup_db(db_path);
    }

    async fn post_agent(
        router: RemoteRouter,
        payload: RemoteAgentRequest,
    ) -> RemoteResponseEnvelope {
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::AgentSession)
            .with_payload(serde_json::to_value(payload).unwrap());
        post_envelope(router, "/api/agent", request).await
    }

    async fn post_workbench(
        router: RemoteRouter,
        operation: RemoteOperationKind,
        payload: RemoteWorkbenchRequest,
    ) -> RemoteResponseEnvelope {
        let request = RemoteRequestEnvelope::new(operation)
            .with_payload(serde_json::to_value(payload).unwrap());
        post_envelope(router, "/api/workbench", request).await
    }

    async fn post_provider(
        router: RemoteRouter,
        payload: RemoteProviderRequest,
    ) -> RemoteResponseEnvelope {
        let request = RemoteRequestEnvelope::new(RemoteOperationKind::ProviderSettings)
            .with_payload(serde_json::to_value(payload).unwrap());
        post_envelope(router, "/api/provider", request).await
    }

    async fn post_envelope(
        router: RemoteRouter,
        path: &str,
        request: RemoteRequestEnvelope,
    ) -> RemoteResponseEnvelope {
        let response = router
            .oneshot(
                Request::post(path)
                    .header("content-type", "application/json")
                    .body(Body::from(serde_json::to_vec(&request).unwrap()))
                    .unwrap(),
            )
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::OK);
        let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        serde_json::from_slice(&body).unwrap()
    }

    fn test_agent_manager(label: &str) -> (PathBuf, Arc<AgentManager>) {
        let db_path = temp_db_path(label);
        (
            db_path.clone(),
            Arc::new(AgentManager::new(&db_path).unwrap()),
        )
    }

    async fn create_mock_session(manager: &AgentManager, title: &str) -> vibex_core::AgentSession {
        let mut conn = open_database(manager.database_path()).unwrap();
        apply_migrations(&mut conn).unwrap();
        let workspace_root = format!("/tmp/vibex-remote-agent-{title}");
        std::fs::create_dir_all(&workspace_root).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: vibex_core::VibexSessionId::new(),
            title: title.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: AgentId::parse("codex").unwrap(),
            state: vibex_core::AgentSessionState::Idle,
            safety: vibex_core::AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        session
    }

    fn test_send_request(
        session: &AgentSession,
        idempotency_key: &str,
        text: &str,
    ) -> SendAgentMessageRequest {
        SendAgentMessageRequest {
            session_id: session.id.clone(),
            message_idempotency_key: idempotency_key.to_string(),
            desired_runtime: remote_test_selection(session),
            text: text.to_string(),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        }
    }

    fn remote_test_selection(session: &AgentSession) -> SessionRuntimeSelection {
        SessionRuntimeSelection {
            agent_id: session.agent_id.clone(),
            provider_profile_id: vibex_core::ProviderProfileId::parse("provider_acp_remote_test")
                .unwrap(),
            model_id: "mock-remote".to_string(),
            reasoning_effort: None,
            mode_id: None,
            config_values: Default::default(),
        }
    }

    fn append_mock_timeline(manager: &AgentManager, session: &AgentSession, text: &str) {
        let mut conn = open_database(manager.database_path()).unwrap();
        TimelineRepository::append(
            &mut conn,
            &session.id,
            vibex_core::TimelineSource::User,
            vibex_core::TimelinePayload::UserMessage(vibex_core::UserMessagePayload {
                text: text.to_string(),
                attachments: Vec::new(),
            }),
            None,
            None,
            vibex_core::TimelineRedactionState::None,
        )
        .unwrap();
        TimelineRepository::append(
            &mut conn,
            &session.id,
            vibex_core::TimelineSource::Agent,
            vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "remote test response".to_string(),
                is_final: true,
            }),
            None,
            None,
            vibex_core::TimelineRedactionState::None,
        )
        .unwrap();
    }

    fn create_provider_profile(db_path: &Path, display_name: &str) -> vibex_core::ProviderProfile {
        ProviderConfigService::new(db_path.to_path_buf())
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: display_name.to_string(),
                account_alias: Some("remote-test".to_string()),
                base_url: None,
                default_model: Some("mock-remote".to_string()),
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
            .unwrap()
    }

    fn pair_device(
        db_path: &Path,
        permission_level: RemoteDevicePermissionLevel,
        display_name: &str,
    ) -> RemoteAuthProof {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let created = RemoteTrustService::create_pairing_code(
            &conn,
            RemoteCreatePairingCodeRequest {
                permission_level,
                ttl_ms: Some(60_000),
            },
        )
        .unwrap();
        let claimed = RemoteTrustService::claim_pairing_code(
            &conn,
            RemoteClaimPairingCodeRequest {
                pairing_code: created.pairing_code,
                display_name: display_name.to_string(),
                public_key: None,
            },
        )
        .unwrap();

        RemoteAuthProof {
            device_id: claimed.device.device_id,
            auth_token: claimed.auth_token,
        }
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-remote-{label}-{}.db",
            RequestId::new().as_str()
        ))
    }

    fn temp_workspace_root(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-remote-workspace-{label}-{}",
            RequestId::new().as_str()
        ))
    }

    fn ensure_workspace(db_path: &Path, workspace_root: &Path) -> vibex_core::WorkspaceRecord {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        WorkspaceRepository::ensure(&conn, workspace_root, WorkspaceMode::CurrentCheckout)
            .unwrap()
            .1
    }

    fn cleanup_db(path: PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn cleanup_workspace(path: PathBuf) {
        let _ = std::fs::remove_dir_all(path);
    }

    async fn spawn_ws_server() -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = build_default_disabled_router();

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        format!("ws://{addr}/ws")
    }

    async fn spawn_ws_agent_server(manager: Arc<AgentManager>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let router = build_router_with_agent(RemoteServiceConfig::loopback_disabled(), manager);

        tokio::spawn(async move {
            axum::serve(listener, router).await.unwrap();
        });

        format!("ws://{addr}/ws")
    }
}

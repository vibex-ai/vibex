use std::collections::HashMap;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex, OnceLock, Weak};

use tokio::sync::{Mutex as AsyncMutex, broadcast, mpsc};
use vibex_core::{
    AgentAuthCatalog, AgentAuthenticateRequest, AgentAuthenticateResult,
    AgentCommandDiscoverRequest, AgentCommandDiscoverResponse, AgentCommandEntry,
    AgentCommandExecuteRequest, AgentCommandExecuteResult, AgentCommandExecuteStatus,
    AgentCommandExecutionBehavior, AgentCommandSelectionBehavior, AgentCommandSourceKind,
    AgentCommandTrigger, AgentConfig, AgentId, AgentLogoutRequest, AgentModelListRequest,
    AgentModelListResponse, AgentModelListSource, AgentSession, AgentSessionConfigProbe,
    AgentSessionRestoreMethod, AgentSessionSafety, AgentSessionState, AgentUsageCounterOrigin,
    AgentUsageExecutionContext, AgentUsageStreamAttribution, BindingState,
    ContinueAgentTurnRequest, CreateAgentSessionRequest, ElicitationRequest,
    ExternalSessionContinuationStatus, ExternalSessionImportCandidate,
    ExternalSessionImportCandidateStatus, ExternalSessionImportDiagnostic,
    ExternalSessionImportPreview, ExternalSessionImportPreviewRequest,
    ExternalSessionImportRequest, ExternalSessionImportResult, ExternalSessionImportSource,
    ExternalSessionImportedTimelineCount, FetchTimelineRequest, ForkAgentSessionRequest, McpServer,
    McpServerTransportKind, MessageAttachment, MessageSubmissionId, PermissionRequest, ProjectId,
    PromptKind, PromptStatus, ProviderBinding, ProviderBindingMetadata, ProviderCapabilities,
    ProviderCapabilitiesResponse, ProviderDefaultScopeKind, ProviderKind, ProviderNativeBinding,
    ProviderProfileDefaultScope, ProviderProfileId, ProviderProfileStatus,
    RenameAgentSessionRequest, ResolveElicitationRequest, ResolvePermissionRequest,
    SendAgentMessageRequest, SessionRuntimeSelection, SessionRuntimeSelectionStatus,
    SystemNoticeLevel, SystemNoticePayload, TimelineErrorPayload, TimelineItem, TimelineLiveEvent,
    TimelinePage, TimelinePayload, TimelineRedactionState, TimelineSource, TransportKind,
    TurnExecutionAttribution, UsageExecutionId, UserMessagePayload, VibexError, VibexResult,
    VibexSessionId, WorkspaceId, agent_id_for_provider_kind,
    agent_session_turn_requires_continuation, builtin_agent_definitions,
    latest_timeline_turn_ended_normally, unix_timestamp_ms,
};
use vibex_db::{
    AgentConfigRepository, AgentDefaultModelProviderProfileRepository,
    AgentSessionRuntimeRepository, DbConnection, ElicitationRepository, McpServerRepository,
    PermissionRepository, PromptRepository, ProviderProfileRepository, RuntimeBindingRepository,
    RuntimeSwitchRepository, SessionRepository, SkillRepository, TimelineAppend,
    TimelineRepository, WorkspaceRepository, apply_migrations, open_database,
};

use crate::adapter::{
    AgentProvider, AgentUsageTelemetryEvent, ProviderElicitationResolution, ProviderEvent,
    ProviderPermissionResolution, ProviderRuntimeMcpServer, ProviderRuntimeMcpTransport,
    ProviderRuntimeResources, ProviderRuntimeSkill, ProviderSessionHandle,
    ProviderTurnExecutionIdentity, ProviderTurnRequest, ProviderTurnResult,
};
use crate::context_bridge::{ContextBridgeService, PreparedContextBridge};
use crate::message_submission::MessageSubmissionCoordinator;
use crate::runtime_selection::RuntimeSelectionService;
use crate::state_machine::validate_transition;

const CONTINUE_AGENT_TURN_PROMPT: &str = "Continue from where you stopped. Review the conversation context, avoid repeating completed work, and proceed with the remaining task.";
const CONTINUE_TURN_TIMELINE_WINDOW: u32 = 500;
pub const PROVIDER_SELECTED_MODEL_METADATA_KEY: &str = "selectedModel";
pub const PROVIDER_SELECTED_REASONING_EFFORT_METADATA_KEY: &str = "selectedReasoningEffort";

pub struct AgentManager {
    db_path: PathBuf,
    /// Route-aware online runtime registry (plan §4.1/§6.1). `ProviderKind`
    /// is no longer the dispatch key; multiple ACP agents coexist here.
    runtimes: HashMap<vibex_core::AgentRuntimeRouteKey, Arc<dyn AgentProvider>>,
    live_events: broadcast::Sender<TimelineLiveEvent>,
    runtime_selection: OnceLock<Weak<RuntimeSelectionService>>,
    message_submission: OnceLock<Weak<MessageSubmissionCoordinator>>,
    usage_telemetry: OnceLock<mpsc::UnboundedSender<AgentUsageTelemetryEvent>>,
    elicitation_resolution_locks: StdMutex<HashMap<String, Weak<AsyncMutex<()>>>>,
    context_bridge: ContextBridgeService,
}

#[derive(Clone)]
struct AgentTurnRequest {
    session_id: VibexSessionId,
    required_runtime: Option<SessionRuntimeSelection>,
    text: String,
    attachments: Vec<MessageAttachment>,
    reasoning_effort: Option<String>,
    correlation_id: Option<vibex_core::CorrelationId>,
}

impl From<SendAgentMessageRequest> for AgentTurnRequest {
    fn from(request: SendAgentMessageRequest) -> Self {
        Self {
            session_id: request.session_id,
            required_runtime: None,
            text: request.text,
            attachments: request.attachments,
            reasoning_effort: request.reasoning_effort,
            correlation_id: request.correlation_id,
        }
    }
}

#[derive(Debug, Clone)]
struct ResolvedAgent {
    agent_id: AgentId,
    provider_kind: ProviderKind,
}

#[derive(Debug)]
struct ProviderTurnAttemptSuccess {
    turn_result: ProviderTurnResult,
    appended: Vec<TimelineItem>,
    needs_input: bool,
    execution_attribution: Option<TurnExecutionAttribution>,
}

#[derive(Debug)]
struct ProviderTurnAttemptFailure {
    error: VibexError,
    appended: Vec<TimelineItem>,
    provider_output_started: bool,
    execution_attribution: Option<TurnExecutionAttribution>,
}

#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
enum ProviderTurnAttemptOutcome {
    Success(ProviderTurnAttemptSuccess),
    Failure(ProviderTurnAttemptFailure),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum InitialRuntimeMaterialization {
    WaitForReady,
    Deferred,
}

impl AgentManager {
    pub fn new(db_path: impl Into<PathBuf>) -> VibexResult<Self> {
        let db_path = db_path.into();
        let mut conn = open_database(&db_path)?;
        apply_migrations(&mut conn)?;
        let context_bridge = ContextBridgeService::new(db_path.clone())?;
        let (live_events, _) = broadcast::channel(512);
        let manager = Self {
            db_path,
            runtimes: HashMap::new(),
            live_events,
            runtime_selection: OnceLock::new(),
            message_submission: OnceLock::new(),
            usage_telemetry: OnceLock::new(),
            elicitation_resolution_locks: StdMutex::new(HashMap::new()),
            context_bridge,
        };
        manager.recover_interrupted_sessions(&mut conn)?;
        Ok(manager)
    }

    /// Registers an online runtime under an explicit route. Duplicate routes
    /// are rejected instead of silently overwritten.
    pub fn register_runtime(
        &mut self,
        route: vibex_core::AgentRuntimeRouteKey,
        provider: Arc<dyn AgentProvider>,
    ) -> VibexResult<()> {
        if route.transport_kind != TransportKind::Acp || provider.kind() != ProviderKind::Acp {
            return Err(VibexError::validation(
                "runtime_route_transport_invalid",
                "online runtimes must use an ACP route and ACP provider",
            ));
        }
        if self
            .runtimes
            .keys()
            .any(|existing| existing.agent_id == route.agent_id)
        {
            return Err(VibexError::conflict(
                "runtime_agent_already_registered",
                "an online runtime is already registered for this Agent",
            )
            .with_diagnostic("agentId", route.agent_id.as_str()));
        }
        if self.runtimes.contains_key(&route) {
            return Err(VibexError::conflict(
                "runtime_route_already_registered",
                "an online runtime is already registered for this route",
            )
            .with_diagnostic("runtimeRoute", describe_runtime_route(&route)));
        }
        self.runtimes.insert(route, provider);
        Ok(())
    }

    pub fn subscribe(&self) -> broadcast::Receiver<TimelineLiveEvent> {
        self.live_events.subscribe()
    }

    /// Publish a timeline item that was durably appended by a trusted local
    /// service. This keeps integration/import paths on the same live stream as
    /// provider-owned events without exposing the broadcast sender itself.
    pub fn publish_external_timeline_item(&self, item: TimelineItem) -> VibexResult<TimelineItem> {
        self.publish_timeline_item(item)
    }

    pub fn install_message_submission_coordinator(
        &self,
        coordinator: &Arc<MessageSubmissionCoordinator>,
    ) -> VibexResult<()> {
        self.message_submission
            .set(Arc::downgrade(coordinator))
            .map_err(|_| {
                VibexError::conflict(
                    "message_submission_coordinator_already_installed",
                    "message submission coordinator is already installed",
                )
            })
    }

    pub fn install_runtime_selection_service(
        &self,
        service: &Arc<RuntimeSelectionService>,
    ) -> VibexResult<()> {
        self.runtime_selection
            .set(Arc::downgrade(service))
            .map_err(|_| {
                VibexError::conflict(
                    "runtime_selection_service_already_installed",
                    "runtime selection service is already installed",
                )
            })
    }

    pub fn install_usage_telemetry_sender(
        &self,
        sender: mpsc::UnboundedSender<AgentUsageTelemetryEvent>,
    ) -> VibexResult<()> {
        self.usage_telemetry.set(sender).map_err(|_| {
            VibexError::conflict(
                "agent_usage_telemetry_already_installed",
                "Agent usage telemetry sender is already installed",
            )
        })
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    /// Materializes the workspace-scoped MCP and Skill inputs used by a
    /// concrete runtime. Runtime-switch executors share this path with normal
    /// create/resume calls so process fingerprints cannot drift by caller.
    pub fn runtime_resources_for_profile(
        &self,
        provider_kind: ProviderKind,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<ProviderRuntimeResources> {
        self.resolve_runtime_resources(provider_kind, provider_profile_id)
    }

    pub(crate) fn resolve_initial_runtime_selection(
        &self,
        requested_profile_id: Option<ProviderProfileId>,
        legacy_provider_kind: ProviderKind,
        project_id: Option<&ProjectId>,
        workspace_id: Option<&WorkspaceId>,
    ) -> VibexResult<SessionRuntimeSelection> {
        let conn = self.open_migrated()?;
        let (agent_id, provider_profile_id) = match requested_profile_id {
            Some(profile_id) => {
                let profile =
                    ProviderProfileRepository::get(&conn, &profile_id)?.ok_or_else(|| {
                        VibexError::validation(
                            "provider_profile_not_found",
                            "Provider Profile was not found",
                        )
                    })?;
                (profile.agent_id, profile_id)
            }
            None => {
                let agent_id = agent_id_for_provider_kind(legacy_provider_kind);
                let profile_id = resolve_provider_profile_id(
                    &conn,
                    &agent_id,
                    ProviderKind::Acp,
                    None,
                    project_id,
                    workspace_id,
                )?;
                (agent_id, profile_id)
            }
        };
        let profile =
            ProviderProfileRepository::get(&conn, &provider_profile_id)?.ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "Provider Profile was not found",
                )
            })?;
        if profile.agent_id != agent_id
            || profile.kind != ProviderKind::Acp
            || profile.status != ProviderProfileStatus::Enabled
        {
            return Err(VibexError::validation(
                "provider_profile_route_mismatch",
                "Provider Profile is not enabled for an ACP Agent",
            ));
        }
        let default_model = profile
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
            .filter(|model| {
                profile.configured_models.is_empty()
                    || profile
                        .configured_models
                        .iter()
                        .any(|configured| configured.enabled && configured.id == *model)
            })
            .map(str::to_string);
        let model_id = default_model
            .or_else(|| {
                profile
                    .configured_models
                    .iter()
                    .find(|model| model.enabled && !model.id.trim().is_empty())
                    .map(|model| model.id.trim().to_string())
            })
            .or_else(|| {
                [
                    profile.small_model.as_deref(),
                    profile.large_model.as_deref(),
                ]
                .into_iter()
                .flatten()
                .map(str::trim)
                .find(|model| !model.is_empty())
                .map(str::to_string)
            })
            .ok_or_else(|| {
                VibexError::validation(
                    "runtime_selection_model_required",
                    "ACP Agent Profile has no configured model for session creation",
                )
                .with_diagnostic("providerProfileId", provider_profile_id.as_str())
            })?;

        Ok(SessionRuntimeSelection {
            agent_id,
            provider_profile_id,
            model_id,
            reasoning_effort: normalize_reasoning_effort(profile.reasoning_effort.as_deref())?,
            mode_id: None,
            config_values: Default::default(),
        })
    }

    pub async fn create_session(
        &self,
        request: CreateAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        self.create_session_with_timeline(request, Vec::new()).await
    }

    /// Creates the durable Logical Session and queues its initial ACP runtime
    /// materialization without waiting for process startup or `session/new`.
    /// Callers that submit a first message must use the durable message queue,
    /// which waits for the runtime selection to become ready before dispatch.
    pub async fn create_session_deferred(
        &self,
        request: CreateAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        self.create_session_with_timeline_and_materialization(
            request,
            Vec::new(),
            |_| {},
            InitialRuntimeMaterialization::Deferred,
        )
        .await
    }

    async fn create_session_with_timeline(
        &self,
        request: CreateAgentSessionRequest,
        initial_timeline: Vec<TimelineAppend>,
    ) -> VibexResult<AgentSession> {
        self.create_session_with_timeline_callback(request, initial_timeline, |_| {})
            .await
    }

    async fn create_session_with_timeline_callback<F>(
        &self,
        request: CreateAgentSessionRequest,
        initial_timeline: Vec<TimelineAppend>,
        on_created: F,
    ) -> VibexResult<AgentSession>
    where
        F: FnOnce(AgentSession) + Send,
    {
        self.create_session_with_timeline_and_materialization(
            request,
            initial_timeline,
            on_created,
            InitialRuntimeMaterialization::WaitForReady,
        )
        .await
    }

    async fn create_session_with_timeline_and_materialization<F>(
        &self,
        request: CreateAgentSessionRequest,
        initial_timeline: Vec<TimelineAppend>,
        on_created: F,
        materialization: InitialRuntimeMaterialization,
    ) -> VibexResult<AgentSession>
    where
        F: FnOnce(AgentSession) + Send,
    {
        let mut desired = request.runtime;
        let resolved_agent =
            self.resolve_enabled_agent(Some(desired.agent_id.clone()), ProviderKind::Acp, true)?;
        if resolved_agent.provider_kind != ProviderKind::Acp {
            return Err(VibexError::conflict(
                "runtime_route_transport_invalid",
                "online Agent sessions require an ACP runtime",
            ));
        }
        self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        let safety = request
            .safety
            .unwrap_or_else(AgentSessionSafety::workspace_write_ask_on_risk);
        desired.model_id = desired.model_id.trim().to_string();
        if desired.model_id.is_empty() {
            return Err(VibexError::validation(
                "runtime_selection_model_required",
                "Agent session creation requires a concrete Catalog model",
            ));
        }
        desired.reasoning_effort = normalize_reasoning_effort(desired.reasoning_effort.as_deref())?;

        let mut conn = self.open_migrated()?;
        let (_project, workspace) =
            WorkspaceRepository::ensure(&conn, &request.workspace_root, request.workspace_mode)?;
        let profile = ProviderProfileRepository::get(&conn, &desired.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "Provider Profile was not found",
                )
            })?;
        if profile.agent_id != desired.agent_id
            || profile.kind != ProviderKind::Acp
            || profile.status != ProviderProfileStatus::Enabled
        {
            return Err(VibexError::validation(
                "provider_profile_route_mismatch",
                "Provider Profile is not enabled for the requested ACP Agent",
            ));
        }
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: request
                .title
                .clone()
                .unwrap_or_else(|| format!("{} session", resolved_agent.agent_id)),
            project_id: workspace.project_id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: workspace.root_path.clone(),
            workspace_mode: workspace.mode,
            agent_id: resolved_agent.agent_id.clone(),
            state: AgentSessionState::Initializing,
            safety: safety.clone(),
            created_at_ms: now,
            updated_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        let copied_items = TimelineRepository::insert_session_and_append_many(
            &mut conn,
            &session,
            &initial_timeline,
        )?;
        for item in copied_items {
            self.publish_timeline_item(item)?;
        }
        self.append_system_notice(
            &mut conn,
            &session.id,
            "Agent session is initializing",
            SystemNoticeLevel::Info,
        )?;
        drop(conn);
        on_created(session.clone());

        let runtime_selection = self
            .runtime_selection
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                VibexError::process(
                    "runtime_selection_service_unavailable",
                    "ACP runtime selection service is not installed",
                )
            })?;
        let initialization = match materialization {
            InitialRuntimeMaterialization::WaitForReady => {
                runtime_selection
                    .initialize_new_session(&session.id, desired)
                    .await
            }
            InitialRuntimeMaterialization::Deferred => {
                runtime_selection
                    .initialize_new_session_deferred(&session.id, desired)
                    .await
            }
        };
        if let Err(err) = initialization {
            let mut conn = self.open_migrated()?;
            self.transition(&conn, &session, AgentSessionState::Error)?;
            self.append_provider_error(&mut conn, &session.id, &err)?;
            return Err(err);
        }

        if materialization == InitialRuntimeMaterialization::Deferred {
            let conn = self.open_migrated()?;
            return SessionRepository::get(&conn, &session.id)?.ok_or_else(|| {
                VibexError::storage(
                    "session_missing_after_deferred_create",
                    "deferred session could not be reloaded",
                )
            });
        }

        let mut conn = self.open_migrated()?;
        self.transition(&conn, &session, AgentSessionState::Idle)?;
        self.append_system_notice(
            &mut conn,
            &session.id,
            "Agent session is ready",
            SystemNoticeLevel::Info,
        )?;

        SessionRepository::get(&conn, &session.id)?.ok_or_else(|| {
            VibexError::storage(
                "session_missing_after_create",
                "created session could not be reloaded",
            )
        })
    }

    pub async fn fork_session(
        &self,
        request: ForkAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        self.fork_session_with_created_callback(request, |_| {})
            .await
    }

    /// Reports the durable `Initializing` session before ACP runtime
    /// materialization finishes, while preserving the final result contract.
    pub async fn fork_session_with_created_callback<F>(
        &self,
        request: ForkAgentSessionRequest,
        on_created: F,
    ) -> VibexResult<AgentSession>
    where
        F: FnOnce(AgentSession) + Send,
    {
        if request.through_sequence < 0 {
            return Err(VibexError::validation(
                "session_fork_sequence_invalid",
                "Agent session fork sequence must not be negative",
            ));
        }

        let conn = self.open_migrated()?;
        let source =
            SessionRepository::get(&conn, &request.source_session_id)?.ok_or_else(|| {
                VibexError::validation(
                    "session_fork_source_not_found",
                    "Agent session fork source was not found",
                )
            })?;
        let source_end_sequence = TimelineRepository::latest_sequence(&conn, &source.id)?;
        if request
            .expected_source_end_sequence
            .is_some_and(|expected| expected != source_end_sequence)
        {
            return Err(VibexError::conflict(
                "session_fork_source_changed",
                "Agent session changed before the fork could be created",
            ));
        }
        if request.through_sequence > source_end_sequence {
            return Err(VibexError::validation(
                "session_fork_sequence_out_of_range",
                "Agent session fork sequence exceeds the source timeline",
            ));
        }

        let runtime_state = AgentSessionRuntimeRepository::get_runtime_state(&conn, &source.id)?
            .ok_or_else(|| {
                VibexError::conflict(
                    "session_fork_runtime_state_missing",
                    "Agent session fork source has no durable runtime selection",
                )
            })?;
        if runtime_state.runtime_selection_status != Some(SessionRuntimeSelectionStatus::Ready)
            || runtime_state.pending_switch_id.is_some()
            || runtime_state.desired_runtime_selection != runtime_state.effective_runtime_selection
        {
            return Err(VibexError::conflict(
                "session_fork_runtime_not_ready",
                "Agent session runtime must be ready before it can be forked",
            ));
        }
        let runtime = runtime_state.effective_runtime_selection.ok_or_else(|| {
            VibexError::conflict(
                "session_fork_runtime_selection_missing",
                "Agent session fork source has no effective runtime selection",
            )
        })?;
        let source_items = if request.through_sequence == 0 {
            Vec::new()
        } else {
            TimelineRepository::fetch_range(&conn, &source.id, 1, request.through_sequence)?
        };
        drop(conn);

        self.create_session_with_timeline_callback(
            CreateAgentSessionRequest {
                runtime,
                workspace_root: source.workspace_root,
                workspace_mode: source.workspace_mode,
                title: Some(source.title),
                safety: Some(source.safety),
            },
            fork_timeline_appends(&source_items),
            on_created,
        )
        .await
    }

    pub async fn list_sessions(&self, include_archived: bool) -> VibexResult<Vec<AgentSession>> {
        let conn = self.open_migrated()?;
        SessionRepository::list(&conn, include_archived)
    }

    pub async fn get_session(&self, session_id: &VibexSessionId) -> VibexResult<AgentSession> {
        let conn = self.open_migrated()?;
        SessionRepository::get(&conn, session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })
    }

    pub async fn fetch_timeline(&self, request: FetchTimelineRequest) -> VibexResult<TimelinePage> {
        let conn = self.open_migrated()?;
        TimelineRepository::fetch_after(
            &conn,
            &request.session_id,
            request.after_sequence,
            request.limit,
        )
    }

    pub async fn preview_external_sessions(
        &self,
        request: ExternalSessionImportPreviewRequest,
    ) -> VibexResult<ExternalSessionImportPreview> {
        let mut candidates = Vec::new();
        let mut diagnostics = Vec::new();
        if !request.sources.contains(&ExternalSessionImportSource::Acp) {
            return Ok(ExternalSessionImportPreview {
                candidates,
                diagnostics,
                correlation_id: request.correlation_id,
            });
        }

        let profiles = {
            let conn = self.open_migrated()?;
            ProviderProfileRepository::list(&conn)?
                .into_iter()
                .filter(|profile| {
                    profile.kind == ProviderKind::Acp
                        && profile.status == ProviderProfileStatus::Enabled
                })
                .collect::<Vec<_>>()
        };

        for profile in profiles {
            let provider = match self
                .route_for_agent(&profile.agent_id)
                .and_then(|route| self.runtime(&route))
            {
                Ok(provider) => provider,
                Err(error) => {
                    diagnostics.push(import_preview_diagnostic(
                        ExternalSessionImportSource::Acp,
                        Some(&profile.id),
                        error,
                    ));
                    continue;
                }
            };
            match provider
                .list_import_candidates(&profile.id, request.workspace_root.as_deref())
                .await
            {
                Ok(mut listed) => candidates.append(&mut listed),
                Err(error) => diagnostics.push(import_preview_diagnostic(
                    ExternalSessionImportSource::Acp,
                    Some(&profile.id),
                    error,
                )),
            }
        }

        Ok(ExternalSessionImportPreview {
            candidates,
            diagnostics,
            correlation_id: request.correlation_id,
        })
    }

    pub async fn import_external_sessions(
        &self,
        request: ExternalSessionImportRequest,
    ) -> VibexResult<ExternalSessionImportResult> {
        let mut sessions = Vec::new();
        let mut imported_timeline_counts = Vec::new();
        let mut diagnostics = Vec::new();

        for candidate in request.candidates {
            self.validate_import_candidate(&candidate)?;
            diagnostics.extend(candidate.diagnostics.clone());

            let mut conn = self.open_migrated()?;
            let (_project, workspace) = WorkspaceRepository::ensure(
                &conn,
                &candidate.workspace_root,
                candidate.workspace_mode,
            )?;
            let agent_id = candidate
                .provider_profile_id
                .as_ref()
                .and_then(|profile_id| {
                    ProviderProfileRepository::get(&conn, profile_id)
                        .ok()
                        .flatten()
                })
                .map(|profile| profile.agent_id)
                .unwrap_or_else(|| agent_id_for_provider_kind(candidate.provider_kind));
            let now = unix_timestamp_ms();
            let session = AgentSession {
                id: VibexSessionId::new(),
                title: candidate.title.clone(),
                project_id: workspace.project_id.clone(),
                workspace_id: workspace.id.clone(),
                workspace_root: workspace.root_path.clone(),
                workspace_mode: workspace.mode,
                agent_id,
                state: AgentSessionState::Idle,
                safety: AgentSessionSafety::workspace_write_ask_on_risk(),
                created_at_ms: now,
                updated_at_ms: now,
                archived_at_ms: None,
                deleted_at_ms: None,
            };
            let mut timeline_inputs = vec![TimelineAppend {
                source: TimelineSource::System,
                payload: TimelinePayload::SystemNotice(SystemNoticePayload {
                    level: SystemNoticeLevel::Info,
                    message: imported_session_notice(&candidate),
                }),
                timestamp_ms: None,
                correlation_id: request.correlation_id.clone(),
                provider_correlation_id: None,
                redaction_state: TimelineRedactionState::None,
                execution_attribution: None,
            }];
            for imported_item in &candidate.timeline_items {
                timeline_inputs.push(TimelineAppend {
                    source: imported_item.source,
                    payload: imported_item.payload.clone(),
                    timestamp_ms: None,
                    correlation_id: request.correlation_id.clone(),
                    provider_correlation_id: imported_item.provider_correlation_id.clone(),
                    redaction_state: imported_item.redaction_state,
                    execution_attribution: None,
                });
            }
            let appended = TimelineRepository::insert_session_and_append_many(
                &mut conn,
                &session,
                &timeline_inputs,
            )?;
            for item in &appended {
                let _ = self.live_events.send(TimelineLiveEvent {
                    session_id: session.id.clone(),
                    sequence: item.sequence,
                    item: item.clone(),
                });
            }

            let loaded = SessionRepository::get(&conn, &session.id)?.ok_or_else(|| {
                VibexError::storage(
                    "session_missing_after_import",
                    "imported session could not be reloaded",
                )
            })?;
            imported_timeline_counts.push(ExternalSessionImportedTimelineCount {
                session_id: loaded.id.clone(),
                count: appended.len() as u32,
            });
            sessions.push(loaded);
        }

        Ok(ExternalSessionImportResult {
            sessions,
            imported_timeline_counts,
            diagnostics,
        })
    }

    pub async fn send_message(
        &self,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        let coordinator = self
            .message_submission
            .get()
            .and_then(Weak::upgrade)
            .ok_or_else(|| {
                VibexError::process(
                    "message_submission_coordinator_unavailable",
                    "durable message submission coordinator is unavailable",
                )
            })?;
        coordinator.submit(request).await
    }

    pub(crate) async fn dispatch_message_direct(
        &self,
        submission_id: Option<MessageSubmissionId>,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        let required_runtime = Some(request.desired_runtime.clone());
        let mut turn_request: AgentTurnRequest = request.into();
        turn_request.required_runtime = required_runtime;
        self.run_agent_turn(
            turn_request,
            true,
            submission_id,
            |provider, handle, turn_request| async move {
                provider.send_turn(handle, turn_request).await
            },
        )
        .await
    }

    pub async fn continue_turn(
        &self,
        request: ContinueAgentTurnRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let latest_timeline = TimelineRepository::fetch_after(
            &conn,
            &session.id,
            None,
            CONTINUE_TURN_TIMELINE_WINDOW,
        )?;
        let latest_turn_ended_normally =
            latest_timeline_turn_ended_normally(&latest_timeline.items);
        if !agent_session_turn_requires_continuation(session.state, latest_turn_ended_normally) {
            return Err(VibexError::conflict(
                "agent_continue_requires_incomplete_turn",
                "Agent turn continuation is only available when the latest turn did not end normally",
            ));
        }
        drop(conn);

        self.run_agent_turn(
            AgentTurnRequest {
                session_id: request.session_id,
                required_runtime: None,
                text: CONTINUE_AGENT_TURN_PROMPT.to_string(),
                attachments: Vec::new(),
                reasoning_effort: None,
                correlation_id: request.correlation_id,
            },
            false,
            None,
            |provider, handle, turn_request| async move {
                provider.send_turn(handle, turn_request).await
            },
        )
        .await
    }

    pub async fn discover_commands(
        &self,
        request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscoverResponse> {
        let request = self.normalize_command_discover_request(request)?;
        let agent_id = required_command_discovery_agent_id(&request)?;
        let provider = self.runtime(&self.route_for_agent(&agent_id)?)?;
        let capabilities = provider.capabilities_for_profile(request.provider_profile_id.as_ref());
        let mut entries = Vec::new();
        let mut diagnostics = Vec::new();

        if command_trigger_matches(request.trigger, AgentCommandTrigger::Slash)
            && capabilities.slash_commands
        {
            let provider_response = provider.discover_commands(request.clone()).await?;
            diagnostics.extend(provider_response.diagnostics);
            entries.extend(provider_response.entries.into_iter().filter(|entry| {
                entry.trigger == AgentCommandTrigger::Slash
                    && entry.source_kind == AgentCommandSourceKind::Provider
            }));
        }

        if command_trigger_matches(request.trigger, AgentCommandTrigger::Slash) {
            let conn = self.open_migrated()?;
            for prompt in PromptRepository::list_enabled(&conn)?
                .into_iter()
                .filter(|prompt| prompt.kind == PromptKind::SlashCommand)
            {
                let command_name = command_token_from_display_name(&prompt.display_name);
                entries.push(AgentCommandEntry {
                    id: format!("prompt:{}", prompt.id.as_str()),
                    trigger: AgentCommandTrigger::Slash,
                    source_kind: AgentCommandSourceKind::Prompt,
                    label: format!("/{command_name}"),
                    description: prompt.description.clone(),
                    insertion_text: format!("/{command_name} "),
                    command_name: Some(command_name),
                    provider_kind: Some(ProviderKind::Acp),
                    prompt_id: Some(prompt.id),
                    skill_id: None,
                    reference_path: None,
                    selection_behavior: AgentCommandSelectionBehavior::Insert,
                    execution_behavior: AgentCommandExecutionBehavior::ExpandPromptAndSend,
                    destructive: false,
                    metadata: Vec::new(),
                });
            }
        }

        if command_trigger_matches(request.trigger, AgentCommandTrigger::Dollar)
            && capabilities.skills
        {
            let conn = self.open_migrated()?;
            for skill in
                SkillRepository::list_enabled_for_agent(&conn, &agent_id, ProviderKind::Acp)?
            {
                let command_name = command_token_from_display_name(&skill.display_name);
                entries.push(AgentCommandEntry {
                    id: format!("skill:{}", skill.id.as_str()),
                    trigger: AgentCommandTrigger::Dollar,
                    source_kind: AgentCommandSourceKind::Skill,
                    label: format!("${command_name}"),
                    description: skill.description.clone(),
                    insertion_text: format!("${command_name} "),
                    command_name: Some(command_name),
                    provider_kind: Some(ProviderKind::Acp),
                    prompt_id: None,
                    skill_id: Some(skill.id),
                    reference_path: None,
                    selection_behavior: AgentCommandSelectionBehavior::Insert,
                    execution_behavior: AgentCommandExecutionBehavior::None,
                    destructive: false,
                    metadata: Vec::new(),
                });
            }
        }

        filter_and_limit_command_entries(&mut entries, request.query.as_deref(), request.limit);
        Ok(AgentCommandDiscoverResponse {
            entries,
            diagnostics,
        })
    }

    pub fn command_discovery_capabilities(
        &self,
        request: &AgentCommandDiscoverRequest,
    ) -> VibexResult<ProviderCapabilities> {
        let request = self.normalize_command_discover_request(request.clone())?;
        let provider =
            self.runtime(&self.route_for_agent(&required_command_discovery_agent_id(&request)?)?)?;
        Ok(provider.capabilities_for_profile(request.provider_profile_id.as_ref()))
    }

    fn normalize_command_discover_request(
        &self,
        mut request: AgentCommandDiscoverRequest,
    ) -> VibexResult<AgentCommandDiscoverRequest> {
        if let Some(session_id) = request.session_id.clone() {
            let conn = self.open_migrated()?;
            let session = SessionRepository::get(&conn, &session_id)?.ok_or_else(|| {
                VibexError::validation("session_not_found", "Agent session was not found")
            })?;
            let (selection, _binding, _identity, _route_key) =
                self.durable_session_execution(&conn, &session)?;
            request.agent_id = Some(selection.agent_id);
            request.provider_profile_id = Some(selection.provider_profile_id);
            if request.workspace_id.is_none() {
                request.workspace_id = Some(session.workspace_id);
            }
        }

        let conn = self.open_migrated()?;
        if let Some(profile_id) = request.provider_profile_id.as_ref() {
            let profile = ProviderProfileRepository::get(&conn, profile_id)?.ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "Provider Profile was not found",
                )
            })?;
            if profile.kind != ProviderKind::Acp || profile.status != ProviderProfileStatus::Enabled
            {
                return Err(VibexError::validation(
                    "provider_profile_route_mismatch",
                    "Provider Profile is not enabled for an ACP Agent",
                ));
            }
            if request
                .agent_id
                .as_ref()
                .is_some_and(|agent_id| agent_id != &profile.agent_id)
            {
                return Err(VibexError::validation(
                    "provider_profile_agent_mismatch",
                    "Provider Profile belongs to another Agent",
                ));
            }
            request.agent_id = Some(profile.agent_id);
        } else {
            let agent_id = required_command_discovery_agent_id(&request)?;
            let workspace = match request.workspace_id.as_ref() {
                Some(workspace_id) => WorkspaceRepository::get(&conn, workspace_id)?
                    .map(|(_project, workspace)| workspace),
                None => None,
            };
            request.provider_profile_id = Some(resolve_provider_profile_id(
                &conn,
                &agent_id,
                ProviderKind::Acp,
                None,
                workspace.as_ref().map(|workspace| &workspace.project_id),
                workspace.as_ref().map(|workspace| &workspace.id),
            )?);
        }

        Ok(request)
    }

    pub async fn execute_command(
        &self,
        request: AgentCommandExecuteRequest,
    ) -> VibexResult<AgentCommandExecuteResult> {
        match request.source_kind {
            AgentCommandSourceKind::Provider => self.execute_provider_command(request).await,
            AgentCommandSourceKind::Prompt => self.execute_prompt_command(request).await,
            AgentCommandSourceKind::ClientBuiltin => Err(VibexError::capability(
                "client_builtin_command_unregistered",
                "no immediate client built-in commands are registered",
            )),
            AgentCommandSourceKind::Skill | AgentCommandSourceKind::Reference => {
                Err(VibexError::capability(
                    "agent_command_source_not_executable",
                    "this command source is insert-only and cannot be executed directly",
                ))
            }
        }
    }

    async fn execute_provider_command(
        &self,
        request: AgentCommandExecuteRequest,
    ) -> VibexResult<AgentCommandExecuteResult> {
        if request.trigger != AgentCommandTrigger::Slash {
            return Err(VibexError::validation(
                "provider_command_trigger_invalid",
                "provider commands must use slash trigger syntax",
            ));
        }
        if request.command_text.trim().is_empty() {
            return Err(VibexError::validation(
                "provider_command_empty",
                "provider command text must not be empty",
            ));
        }

        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let (selection, _binding, _identity, route_key) =
            self.durable_session_execution(&conn, &session)?;
        let provider = self.runtime(&route_key)?;
        let capabilities = provider.capabilities_for_profile(Some(&selection.provider_profile_id));
        if !capabilities.slash_commands {
            return Err(VibexError::capability(
                "acp_slash_commands_unsupported",
                "this provider profile does not support provider slash commands",
            ));
        }
        drop(conn);

        let send_request = AgentTurnRequest {
            session_id: request.session_id.clone(),
            required_runtime: None,
            text: request.command_text.clone(),
            attachments: request.attachments.clone(),
            reasoning_effort: request.reasoning_effort.clone(),
            correlation_id: request.correlation_id.clone(),
        };
        let command_request = request.clone();
        let items = self
            .run_agent_turn(
                send_request,
                true,
                None,
                move |provider, handle, turn_request| {
                    let command_request = command_request.clone();
                    async move {
                        provider
                            .execute_command(handle, command_request, turn_request)
                            .await
                    }
                },
            )
            .await?;

        Ok(AgentCommandExecuteResult {
            status: AgentCommandExecuteStatus::Completed,
            message: None,
            items,
        })
    }

    async fn execute_prompt_command(
        &self,
        request: AgentCommandExecuteRequest,
    ) -> VibexResult<AgentCommandExecuteResult> {
        if request.trigger != AgentCommandTrigger::Slash {
            return Err(VibexError::validation(
                "prompt_command_trigger_invalid",
                "prompt commands must use slash trigger syntax",
            ));
        }

        let conn = self.open_migrated()?;
        let prompt = if let Some(prompt_id) = &request.prompt_id {
            PromptRepository::get(&conn, prompt_id)?
        } else {
            let requested_name = request
                .command_name
                .as_deref()
                .map(command_token_from_display_name)
                .ok_or_else(|| {
                    VibexError::validation(
                        "prompt_command_name_missing",
                        "prompt command execution requires a command name or prompt id",
                    )
                })?;
            PromptRepository::list_enabled(&conn)?
                .into_iter()
                .find(|prompt| {
                    prompt.kind == PromptKind::SlashCommand
                        && command_token_from_display_name(&prompt.display_name) == requested_name
                })
        }
        .ok_or_else(|| {
            VibexError::validation("prompt_command_not_found", "slash prompt was not found")
        })?;

        if prompt.kind != PromptKind::SlashCommand || prompt.status != PromptStatus::Enabled {
            return Err(VibexError::capability(
                "prompt_command_disabled",
                "this prompt is not an enabled slash command",
            ));
        }
        drop(conn);

        let arguments = request.arguments.as_deref().unwrap_or("").trim();
        let expanded_text = expand_prompt_body(&prompt.body, arguments);
        let items = self
            .run_agent_turn(
                AgentTurnRequest {
                    session_id: request.session_id,
                    required_runtime: None,
                    text: expanded_text,
                    attachments: request.attachments,
                    reasoning_effort: request.reasoning_effort,
                    correlation_id: request.correlation_id,
                },
                true,
                None,
                |provider, handle, turn_request| async move {
                    provider.send_turn(handle, turn_request).await
                },
            )
            .await?;

        Ok(AgentCommandExecuteResult {
            status: AgentCommandExecuteStatus::Completed,
            message: Some(format!("Expanded slash prompt {}", prompt.display_name)),
            items,
        })
    }

    async fn run_agent_turn<F, Fut>(
        &self,
        request: AgentTurnRequest,
        display_user_message: bool,
        message_submission_id: Option<MessageSubmissionId>,
        runner: F,
    ) -> VibexResult<Vec<TimelineItem>>
    where
        F: Fn(Arc<dyn AgentProvider>, ProviderSessionHandle, ProviderTurnRequest) -> Fut,
        Fut: Future<Output = VibexResult<ProviderTurnResult>>,
    {
        if request.text.trim().is_empty() && request.attachments.is_empty() {
            return Err(VibexError::validation(
                "empty_agent_message",
                "Agent message text or attachments must not be empty",
            ));
        }

        let mut conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        if session.state == AgentSessionState::Running {
            return Err(VibexError::conflict(
                "agent_turn_already_running",
                "Agent session already has a running turn",
            ));
        }
        validate_transition(session.state, AgentSessionState::Running)?;
        let runtime_state = AgentSessionRuntimeRepository::get_runtime_state(&conn, &session.id)?
            .ok_or_else(|| {
            VibexError::conflict(
                "message_submission_runtime_state_missing",
                "durable message submission runtime state is unavailable",
            )
        })?;
        let required_runtime = request
            .required_runtime
            .clone()
            .or_else(|| runtime_state.effective_runtime_selection.clone())
            .ok_or_else(|| {
                VibexError::conflict(
                    "message_submission_runtime_selection_missing",
                    "Agent session has no effective ACP runtime selection",
                )
            })?;
        if runtime_state.desired_runtime_selection.as_ref() != Some(&required_runtime)
            || runtime_state.effective_runtime_selection.as_ref() != Some(&required_runtime)
            || runtime_state.runtime_selection_status != Some(SessionRuntimeSelectionStatus::Ready)
            || runtime_state.pending_switch_id.is_some()
            || runtime_state.current_agent_id.as_ref() != Some(&required_runtime.agent_id)
            || session.agent_id != required_runtime.agent_id
        {
            return Err(VibexError::conflict(
                "message_submission_runtime_gate_changed",
                "durable message submission runtime changed before prompt admission",
            ));
        }
        if let Some(reasoning_effort) =
            normalize_reasoning_effort(request.reasoning_effort.as_deref())?
            && required_runtime.reasoning_effort.as_deref() != Some(reasoning_effort.as_str())
        {
            return Err(VibexError::conflict(
                "message_submission_reasoning_effort_mismatch",
                "turn reasoning effort does not match the effective runtime selection",
            ));
        }
        let current_binding_id = runtime_state.current_binding_id.as_ref().ok_or_else(|| {
            VibexError::conflict(
                "message_submission_runtime_binding_missing",
                "durable message submission has no committed runtime binding",
            )
        })?;
        let usage_execution_id = message_submission_id
            .as_ref()
            .map(UsageExecutionId::from_message_submission)
            .unwrap_or_default();
        let usage_counter_origin = self.usage_counter_origin(&conn, current_binding_id)?;
        let (binding, expected_execution_identity, route_key) = self
            .durable_provider_turn_binding(
                &conn,
                &session,
                &required_runtime,
                current_binding_id,
                runtime_state.activation_generation,
            )?;
        let provider = self.runtime(&route_key)?;
        let prepared_context_bridge: Option<PreparedContextBridge> =
            self.context_bridge.pending_for_turn(
                &session.id,
                &expected_execution_identity.binding_id,
                expected_execution_identity.activation_generation,
            )?;
        SessionRepository::claim_running_turn(&conn, &session.id, session.state)?;

        let user_item = if display_user_message {
            let appended_user = match self.append_timeline_item(
                &mut conn,
                &session.id,
                TimelineSource::User,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: request.text.clone(),
                    attachments: request.attachments.clone(),
                }),
                request.correlation_id.as_ref(),
                None,
                TimelineRedactionState::None,
            ) {
                Ok(item) => item,
                Err(err) => {
                    let _ = self.finish_turn_with_error_on_conn(&mut conn, &session.id, &err);
                    return Err(err);
                }
            };
            Some(appended_user)
        } else {
            None
        };
        let startup_items = match self.append_turn_startup_notice(
            &mut conn,
            &session,
            provider.as_ref(),
            &required_runtime,
            request.correlation_id.as_ref(),
        ) {
            Ok(items) => items,
            Err(err) => {
                let _ = self.finish_turn_with_error_on_conn(&mut conn, &session.id, &err);
                return Err(err);
            }
        };

        drop(conn);

        let mut appended = user_item
            .into_iter()
            .chain(startup_items)
            .collect::<Vec<_>>();
        let coalesce_after_sequence = appended.iter().map(|item| item.sequence).max().unwrap_or(0);

        let provider_turn_text = prepared_context_bridge
            .as_ref()
            .map(|bridge| bridge.provider_text(&request.text))
            .unwrap_or_else(|| request.text.clone());
        let attempt = self
            .run_provider_turn_attempt(
                &session,
                provider,
                binding,
                provider_turn_text,
                &request.attachments,
                message_submission_id.clone(),
                usage_execution_id,
                usage_counter_origin,
                Some(required_runtime.clone()),
                Some(expected_execution_identity.clone()),
                coalesce_after_sequence,
                &runner,
            )
            .await;
        let (turn_result, mut needs_input, execution_attribution) = match attempt {
            ProviderTurnAttemptOutcome::Success(success) => {
                for item in success.appended {
                    push_or_replace_timeline_item(&mut appended, item);
                }
                (
                    success.turn_result,
                    success.needs_input,
                    success.execution_attribution,
                )
            }
            ProviderTurnAttemptOutcome::Failure(failure) => {
                let ProviderTurnAttemptFailure {
                    error,
                    appended: failed_items,
                    provider_output_started,
                    execution_attribution,
                } = failure;
                for item in failed_items {
                    push_or_replace_timeline_item(&mut appended, item);
                }
                let mut conn = self.open_migrated()?;
                let attribution = if provider_output_started {
                    execution_attribution.as_ref()
                } else {
                    None
                };
                self.finish_turn_with_error_on_conn_with_attribution(
                    &mut conn,
                    &session.id,
                    &error,
                    attribution,
                )?;
                return Err(error);
            }
        };

        let mut conn = match self.open_migrated() {
            Ok(conn) => conn,
            Err(err) => {
                let _ = self.finish_turn_with_error(&session.id, &err);
                return Err(err);
            }
        };
        let turn_completed = turn_result.completed;
        let streamed_event_indices = appended
            .iter()
            .enumerate()
            .filter_map(|(index, item)| {
                item.provider_correlation_id
                    .as_ref()
                    .map(|correlation| (correlation.clone(), index))
            })
            .collect::<HashMap<_, _>>();
        for event in turn_result.events {
            if provider_event_was_streamed(&event, &appended, &streamed_event_indices) {
                continue;
            }
            if let TimelinePayload::PermissionRequest(permission) = &event.payload {
                if let Err(err) = PermissionRepository::insert_request(&conn, permission) {
                    let _ = self.finish_turn_with_error_on_conn_with_attribution(
                        &mut conn,
                        &session.id,
                        &err,
                        execution_attribution.as_ref(),
                    );
                    return Err(err);
                }
                needs_input = true;
            }
            if let TimelinePayload::ElicitationRequest(elicitation) = &event.payload {
                if let Err(err) = ElicitationRepository::insert_request(&conn, elicitation) {
                    let _ = self.finish_turn_with_error_on_conn_with_attribution(
                        &mut conn,
                        &session.id,
                        &err,
                        execution_attribution.as_ref(),
                    );
                    return Err(err);
                }
                needs_input = true;
            }
            let item = match self.append_provider_event(
                &mut conn,
                &session.id,
                event,
                coalesce_after_sequence,
                execution_attribution.as_ref(),
            ) {
                Ok(item) => item,
                Err(err) => {
                    let _ = self.finish_turn_with_error_on_conn_with_attribution(
                        &mut conn,
                        &session.id,
                        &err,
                        execution_attribution.as_ref(),
                    );
                    return Err(err);
                }
            };
            push_or_replace_timeline_item(&mut appended, item);
        }

        if turn_completed {
            let identity = &expected_execution_identity;
            let consumed_context_sequence = appended
                .iter()
                .map(|item| item.sequence)
                .max()
                .ok_or_else(|| {
                    VibexError::storage(
                        "context_bridge_turn_result_missing",
                        "successful durable turn has no persisted timeline sequence",
                    )
                })?;
            if let Err(err) = self.context_bridge.record_successful_turn(
                &mut conn,
                &session.id,
                &identity.binding_id,
                identity.activation_generation,
                message_submission_id.as_ref(),
                consumed_context_sequence,
            ) {
                let _ = self.finish_turn_with_error_on_conn_with_attribution(
                    &mut conn,
                    &session.id,
                    &err,
                    execution_attribution.as_ref(),
                );
                return Err(err);
            }
        }

        let next_state = if needs_input {
            // User-input requests resolved while the turn was still running
            // must not park the session in NeedsInput forever; only sessions
            // with unresolved pending requests keep waiting for the user.
            let pending_permissions = PermissionRepository::pending_for_session(&conn, &session.id);
            let pending_elicitations =
                ElicitationRepository::pending_for_session(&conn, &session.id);
            match (pending_permissions, pending_elicitations) {
                (Ok(permissions), Ok(elicitations))
                    if permissions.is_empty() && elicitations.is_empty() =>
                {
                    AgentSessionState::Idle
                }
                _ => AgentSessionState::NeedsInput,
            }
        } else {
            AgentSessionState::Idle
        };
        if let Err(err) = validate_transition(AgentSessionState::Running, next_state) {
            let _ = self.finish_turn_with_error_on_conn_with_attribution(
                &mut conn,
                &session.id,
                &err,
                execution_attribution.as_ref(),
            );
            return Err(err);
        }
        if let Err(err) = SessionRepository::update_state(&conn, &session.id, next_state) {
            let _ = self.finish_turn_with_error_on_conn_with_attribution(
                &mut conn,
                &session.id,
                &err,
                execution_attribution.as_ref(),
            );
            return Err(err);
        }

        Ok(appended)
    }

    #[allow(clippy::too_many_arguments)]
    async fn run_provider_turn_attempt<F, Fut>(
        &self,
        session: &AgentSession,
        provider: Arc<dyn AgentProvider>,
        binding: ProviderBinding,
        provider_turn_text: String,
        attachments: &[MessageAttachment],
        message_submission_id: Option<MessageSubmissionId>,
        usage_execution_id: UsageExecutionId,
        usage_counter_origin: AgentUsageCounterOrigin,
        required_runtime: Option<SessionRuntimeSelection>,
        expected_execution_identity: Option<ProviderTurnExecutionIdentity>,
        coalesce_after_sequence: i64,
        runner: &F,
    ) -> ProviderTurnAttemptOutcome
    where
        F: Fn(Arc<dyn AgentProvider>, ProviderSessionHandle, ProviderTurnRequest) -> Fut,
        Fut: Future<Output = VibexResult<ProviderTurnResult>>,
    {
        let handle = match provider.resume_session(binding.clone()).await {
            Ok(handle) => handle,
            Err(error) => {
                return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                    error,
                    appended: Vec::new(),
                    provider_output_started: false,
                    execution_attribution: None,
                });
            }
        };
        let runtime_resources =
            match self.resolve_runtime_resources(ProviderKind::Acp, &binding.provider_profile_id) {
                Ok(resources) => resources,
                Err(error) => {
                    return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                        error,
                        appended: Vec::new(),
                        provider_output_started: false,
                        execution_attribution: None,
                    });
                }
            };
        let (event_sender, mut event_receiver) = mpsc::unbounded_channel();
        let mut turn_request = ProviderTurnRequest {
            session_id: session.id.clone(),
            message_submission_id,
            required_runtime,
            text: provider_turn_text,
            attachments: attachments.to_vec(),
            workspace_root: session.workspace_root.clone(),
            binding: binding.clone(),
            safety: session.safety.clone(),
            runtime_resources,
            execution_identity: expected_execution_identity.clone(),
            event_sender: Some(event_sender),
            binding_update_sender: None,
            usage_execution_context: None,
            usage_counter_origin,
            usage_event_sender: self.usage_telemetry.get().cloned(),
        };
        let execution_identity = match provider
            .prepare_turn_execution(&handle, &turn_request)
            .await
        {
            Ok(identity) => identity,
            Err(error) => {
                return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                    error,
                    appended: Vec::new(),
                    provider_output_started: false,
                    execution_attribution: None,
                });
            }
        };
        if expected_execution_identity.is_some()
            && expected_execution_identity.as_ref() != execution_identity.as_ref()
        {
            return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                error: VibexError::conflict(
                    "turn_execution_identity_mismatch",
                    "provider prepared a different runtime than the durable submission fence",
                ),
                appended: Vec::new(),
                provider_output_started: false,
                execution_attribution: None,
            });
        }
        let usage_execution_context =
            execution_identity
                .as_ref()
                .map(|identity| AgentUsageExecutionContext {
                    usage_execution_id,
                    message_submission_id: turn_request.message_submission_id.clone(),
                    project_id: session.project_id.clone(),
                    workspace_id: session.workspace_id.clone(),
                    stream: AgentUsageStreamAttribution {
                        session_id: session.id.clone(),
                        binding_id: identity.binding_id.clone(),
                        activation_generation: identity.activation_generation,
                        agent_id: session.agent_id.clone(),
                        provider_profile_id: binding.provider_profile_id.clone(),
                        model_id: identity.model_id.clone(),
                    },
                });
        let execution_attribution = match execution_identity.as_ref() {
            Some(identity) => match self.turn_execution_attribution(&binding, identity) {
                Ok(attribution) => Some(attribution),
                Err(error) => {
                    return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                        error,
                        appended: Vec::new(),
                        provider_output_started: false,
                        execution_attribution: None,
                    });
                }
            },
            None => None,
        };
        turn_request.execution_identity = execution_identity;
        turn_request.usage_execution_context = usage_execution_context;
        turn_request.usage_counter_origin = usage_counter_origin;
        let turn = runner(provider.clone(), handle, turn_request);
        tokio::pin!(turn);

        let mut appended = Vec::new();
        let mut needs_input = false;
        let mut provider_output_started = false;
        let mut event_receiver_open = true;
        let mut streamed_event_indices = HashMap::<String, usize>::new();
        let turn_result = loop {
            if event_receiver_open {
                tokio::select! {
                    event = event_receiver.recv(), if event_receiver_open => {
                        if let Some(event) = event {
                            if provider_event_was_streamed(
                                &event,
                                &appended,
                                &streamed_event_indices,
                            ) {
                                continue;
                            }
                            let item = match self.handle_streamed_provider_event(
                                &session.id,
                                event,
                                coalesce_after_sequence,
                                execution_attribution.as_ref(),
                                &mut needs_input,
                            ) {
                                Ok(item) => item,
                                Err(error) => {
                                    return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                                        error,
                                        appended,
                                        provider_output_started,
                                        execution_attribution: execution_attribution.clone(),
                                    });
                                }
                            };
                            provider_output_started = true;
                            let index = push_or_replace_timeline_item(&mut appended, item);
                            if let Some(correlation) = appended[index]
                                .provider_correlation_id
                                .as_ref()
                            {
                                streamed_event_indices.insert(correlation.clone(), index);
                            }
                        } else {
                            event_receiver_open = false;
                        }
                    }
                    result = &mut turn => break result,
                }
            } else {
                break turn.await;
            }
        };

        while let Ok(event) = event_receiver.try_recv() {
            if provider_event_was_streamed(&event, &appended, &streamed_event_indices) {
                continue;
            }
            let item = match self.handle_streamed_provider_event(
                &session.id,
                event,
                coalesce_after_sequence,
                execution_attribution.as_ref(),
                &mut needs_input,
            ) {
                Ok(item) => item,
                Err(error) => {
                    return ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                        error,
                        appended,
                        provider_output_started,
                        execution_attribution: execution_attribution.clone(),
                    });
                }
            };
            provider_output_started = true;
            let index = push_or_replace_timeline_item(&mut appended, item);
            if let Some(correlation) = appended[index].provider_correlation_id.as_ref() {
                streamed_event_indices.insert(correlation.clone(), index);
            }
        }
        match turn_result {
            Ok(turn_result) => ProviderTurnAttemptOutcome::Success(ProviderTurnAttemptSuccess {
                turn_result,
                appended,
                needs_input,
                execution_attribution,
            }),
            Err(error) => ProviderTurnAttemptOutcome::Failure(ProviderTurnAttemptFailure {
                error,
                appended,
                provider_output_started,
                execution_attribution,
            }),
        }
    }

    fn resolve_runtime_resources(
        &self,
        provider_kind: ProviderKind,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<ProviderRuntimeResources> {
        let conn = self.open_migrated()?;
        let agent_id = ProviderProfileRepository::get(&conn, provider_profile_id)?
            .map(|profile| profile.agent_id)
            .unwrap_or_else(|| agent_id_for_provider_kind(provider_kind));
        let mcp_servers =
            McpServerRepository::list_enabled_for_agent(&conn, &agent_id, provider_kind)?
                .into_iter()
                .filter_map(runtime_mcp_server_from_record)
                .collect();
        let skills = SkillRepository::list_enabled_for_agent(&conn, &agent_id, provider_kind)?
            .into_iter()
            .map(|skill| ProviderRuntimeSkill {
                id: skill.id.as_str().to_string(),
                display_name: skill.display_name,
                source_uri: skill.source_uri,
            })
            .collect();

        Ok(ProviderRuntimeResources {
            mcp_servers,
            skills,
        })
    }

    pub async fn resolve_permission(
        &self,
        request: ResolvePermissionRequest,
    ) -> VibexResult<TimelineItem> {
        let mut conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let (selection, binding, _identity, route_key) =
            self.durable_session_execution(&conn, &session)?;
        let provider = self.runtime(&route_key)?;
        let capabilities = provider.capabilities_for_profile(Some(&selection.provider_profile_id));
        if !capabilities.permission_requests {
            return Err(VibexError::capability(
                "acp_permission_resolution_unsupported",
                "this provider profile does not support permission resolution callbacks",
            ));
        }
        let existing_permission =
            PermissionRepository::get_request(&conn, &request.resolution.request_id)?.ok_or_else(
                || {
                    VibexError::validation(
                        "permission_request_not_found",
                        "permission request was not found",
                    )
                    .with_diagnostic("requestId", request.resolution.request_id.as_str())
                },
            )?;
        if existing_permission.status != vibex_core::PermissionRequestStatus::Pending {
            return self.append_timeline_item(
                &mut conn,
                &session.id,
                TimelineSource::User,
                TimelinePayload::PermissionResolution(request.resolution),
                None,
                None,
                TimelineRedactionState::None,
            );
        }
        if let Some(provider_resolution_id) = request.resolution.provider_resolution_id.as_deref()
            && !existing_permission.response_options.iter().any(|option| {
                option.option_id == provider_resolution_id
                    && option.response == request.resolution.response
            })
        {
            return Err(VibexError::validation(
                "permission_response_option_invalid",
                "the selected permission response option is not available for this request",
            ));
        }
        PermissionRepository::resolve(&conn, &request.resolution)?;
        let item = self.append_timeline_item(
            &mut conn,
            &session.id,
            TimelineSource::User,
            TimelinePayload::PermissionResolution(request.resolution.clone()),
            None,
            None,
            TimelineRedactionState::None,
        )?;
        drop(conn);

        provider
            .resolve_permission(ProviderPermissionResolution {
                session_id: session.id.clone(),
                binding,
                resolution: request.resolution,
            })
            .await?;

        // A running provider turn finalizes its own session state once the
        // turn completes; only sessions parked in NeedsInput flip back to Idle
        // here, and only when nothing else is still waiting for the user.
        let conn = self.open_migrated()?;
        let latest = SessionRepository::get(&conn, &session.id)?;
        let still_pending_permissions =
            PermissionRepository::pending_for_session(&conn, &session.id)?;
        let still_pending_elicitations =
            ElicitationRepository::pending_for_session(&conn, &session.id)?;
        if still_pending_permissions.is_empty()
            && still_pending_elicitations.is_empty()
            && latest
                .map(|session| session.state != AgentSessionState::Running)
                .unwrap_or(false)
        {
            SessionRepository::update_state(&conn, &session.id, AgentSessionState::Idle)?;
        }
        Ok(item)
    }

    pub async fn record_permission_request(
        &self,
        request: PermissionRequest,
    ) -> VibexResult<TimelineItem> {
        let mut conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        PermissionRepository::insert_request(&conn, &request)?;
        let item = self.append_timeline_item(
            &mut conn,
            &session.id,
            TimelineSource::Provider,
            TimelinePayload::PermissionRequest(request),
            None,
            None,
            TimelineRedactionState::None,
        )?;
        SessionRepository::update_state(&conn, &session.id, AgentSessionState::NeedsInput)?;
        Ok(item)
    }

    pub async fn resolve_elicitation(
        &self,
        request: ResolveElicitationRequest,
    ) -> VibexResult<TimelineItem> {
        request.validate()?;
        let resolution_lock = self.elicitation_resolution_lock(request.request_id.as_str())?;
        let _resolution_guard = resolution_lock.lock().await;
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let (selection, binding, execution_identity, route_key) =
            self.durable_session_execution(&conn, &session)?;
        let provider = self.runtime(&route_key)?;
        let capabilities = provider.capabilities_for_profile(Some(&selection.provider_profile_id));
        if !capabilities.elicitation {
            return Err(VibexError::capability(
                "acp_elicitation_resolution_unsupported",
                "this provider profile does not support elicitation callbacks",
            ));
        }
        let existing =
            ElicitationRepository::get_request(&conn, &request.request_id)?.ok_or_else(|| {
                VibexError::validation(
                    "elicitation_request_not_found",
                    "elicitation request was not found",
                )
                .with_diagnostic("requestId", request.request_id.as_str())
            })?;
        existing.validate_resolution(&request.resolution)?;
        drop(conn);

        provider
            .resolve_elicitation(ProviderElicitationResolution {
                session_id: session.id.clone(),
                binding,
                execution_identity,
                resolution: request.resolution.clone(),
            })
            .await?;

        let mut conn = self.open_migrated()?;
        ElicitationRepository::resolve(&conn, &request.resolution)?;
        let item = self.append_timeline_item(
            &mut conn,
            &session.id,
            TimelineSource::User,
            TimelinePayload::ElicitationResolution(request.resolution),
            None,
            None,
            TimelineRedactionState::None,
        )?;
        let latest = SessionRepository::get(&conn, &session.id)?;
        let pending_permissions = PermissionRepository::pending_for_session(&conn, &session.id)?;
        let pending_elicitations = ElicitationRepository::pending_for_session(&conn, &session.id)?;
        if pending_permissions.is_empty()
            && pending_elicitations.is_empty()
            && latest
                .map(|session| session.state != AgentSessionState::Running)
                .unwrap_or(false)
        {
            SessionRepository::update_state(&conn, &session.id, AgentSessionState::Idle)?;
        }
        Ok(item)
    }

    fn elicitation_resolution_lock(&self, request_id: &str) -> VibexResult<Arc<AsyncMutex<()>>> {
        let mut locks = self.elicitation_resolution_locks.lock().map_err(|_| {
            VibexError::process(
                "agent_elicitation_resolution_lock_poisoned",
                "Agent elicitation resolution lock is unavailable",
            )
        })?;
        locks.retain(|_, lock| lock.strong_count() > 0);
        if let Some(lock) = locks.get(request_id).and_then(Weak::upgrade) {
            return Ok(lock);
        }
        let lock = Arc::new(AsyncMutex::new(()));
        locks.insert(request_id.to_string(), Arc::downgrade(&lock));
        Ok(lock)
    }

    pub async fn record_elicitation_request(
        &self,
        request: ElicitationRequest,
    ) -> VibexResult<TimelineItem> {
        let mut conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, &request.session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        ElicitationRepository::insert_request(&conn, &request)?;
        let item = self.append_timeline_item(
            &mut conn,
            &session.id,
            TimelineSource::Provider,
            TimelinePayload::ElicitationRequest(request),
            None,
            None,
            TimelineRedactionState::None,
        )?;
        SessionRepository::update_state(&conn, &session.id, AgentSessionState::NeedsInput)?;
        Ok(item)
    }

    pub async fn interrupt(&self, session_id: &VibexSessionId) -> VibexResult<()> {
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, session_id)?.ok_or_else(|| {
            VibexError::validation("session_not_found", "Agent session was not found")
        })?;
        let (selection, binding, _identity, route_key) =
            self.durable_session_execution(&conn, &session)?;
        let provider = self.runtime(&route_key)?;
        let capabilities = provider.capabilities_for_profile(Some(&selection.provider_profile_id));
        if !capabilities.interrupt {
            return Err(VibexError::capability(
                "acp_interrupt_unsupported",
                "this provider profile does not support interrupt",
            ));
        }
        provider
            .interrupt(ProviderSessionHandle {
                binding,
                capabilities,
            })
            .await?;
        SessionRepository::update_state(&conn, &session.id, AgentSessionState::Idle)
    }

    pub async fn archive_session(&self, session_id: &VibexSessionId) -> VibexResult<()> {
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, session_id)?;
        let runtime = session
            .as_ref()
            .and_then(|session| self.runtime_for_close(&conn, session));
        SessionRepository::archive(&conn, session_id)?;
        drop(conn);
        self.close_provider_session(runtime).await;
        Ok(())
    }

    pub async fn archive_session_if_timeline_unchanged(
        &self,
        session_id: &VibexSessionId,
        expected_end_sequence: i64,
    ) -> VibexResult<()> {
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, session_id)?;
        let runtime = session
            .as_ref()
            .and_then(|session| self.runtime_for_close(&conn, session));
        SessionRepository::archive_if_timeline_unchanged(&conn, session_id, expected_end_sequence)?;
        drop(conn);
        self.close_provider_session(runtime).await;
        Ok(())
    }

    pub async fn rename_session(
        &self,
        request: RenameAgentSessionRequest,
    ) -> VibexResult<AgentSession> {
        let title = request.title.trim();
        if title.is_empty() {
            return Err(VibexError::validation(
                "session_title_empty",
                "session title must not be empty",
            ));
        }

        let conn = self.open_migrated()?;
        SessionRepository::update_title(&conn, &request.session_id, title)
    }

    pub async fn delete_session(&self, session_id: &VibexSessionId) -> VibexResult<()> {
        let conn = self.open_migrated()?;
        let session = SessionRepository::get(&conn, session_id)?;
        let runtime = session
            .as_ref()
            .and_then(|session| self.runtime_for_close(&conn, session));
        SessionRepository::delete(&conn, session_id)?;
        drop(conn);
        self.close_provider_session(runtime).await;
        Ok(())
    }

    /// Best-effort release of provider runtime resources (long-lived agent
    /// processes) after a session leaves active use.
    fn runtime_for_close(
        &self,
        conn: &DbConnection,
        session: &AgentSession,
    ) -> Option<(Arc<dyn AgentProvider>, ProviderBinding)> {
        let (_selection, binding, _identity, route_key) =
            self.durable_session_execution(conn, session).ok()?;
        let provider = self.runtime(&route_key).ok()?;
        Some((provider, binding))
    }

    async fn close_provider_session(
        &self,
        runtime: Option<(Arc<dyn AgentProvider>, ProviderBinding)>,
    ) {
        let Some((provider, binding)) = runtime else {
            return;
        };
        let _ = provider.close_session(binding).await;
    }

    pub async fn capabilities(&self) -> ProviderCapabilitiesResponse {
        // The same provider instance may back several routes; report each
        // instance once.
        let mut seen = Vec::new();
        let mut providers = Vec::new();
        for provider in self.runtimes.values() {
            let ptr = Arc::as_ptr(provider) as *const () as usize;
            if seen.contains(&ptr) {
                continue;
            }
            seen.push(ptr);
            providers.push(provider.capabilities());
        }
        ProviderCapabilitiesResponse { providers }
    }

    pub fn provider_capabilities(&self, kind: ProviderKind) -> VibexResult<ProviderCapabilities> {
        if kind != ProviderKind::Acp {
            return Err(VibexError::capability(
                "native_provider_runtime_removed",
                "Native provider runtimes are not available",
            )
            .with_diagnostic("providerKind", kind.to_string()));
        }
        self.runtimes
            .iter()
            .min_by(|(left, _), (right, _)| left.cmp(right))
            .map(|(_, provider)| provider.capabilities())
            .ok_or_else(|| {
                VibexError::capability(
                    "provider_unregistered",
                    "no online ACP runtime is registered",
                )
            })
    }

    pub async fn list_models(
        &self,
        mut request: AgentModelListRequest,
    ) -> VibexResult<AgentModelListResponse> {
        if let Some(session_id) = request.session_id.clone() {
            let conn = self.open_migrated()?;
            let session = SessionRepository::get(&conn, &session_id)?.ok_or_else(|| {
                VibexError::validation("session_not_found", "Agent session was not found")
            })?;
            let (selection, _binding, _identity, _route_key) =
                self.durable_session_execution(&conn, &session)?;
            request.provider_profile_id = Some(selection.provider_profile_id);
            request.agent_id = Some(selection.agent_id);
        }
        let agent_id = request.agent_id.clone().ok_or_else(|| {
            VibexError::validation(
                "agent_id_required",
                "Agent model discovery requires an Agent or Logical Session",
            )
        })?;
        let resolved_agent =
            self.resolve_enabled_agent(Some(agent_id), ProviderKind::Acp, false)?;
        let agent_id = resolved_agent.agent_id.clone();
        let conn = self.open_migrated()?;
        let provider_profile_id = resolve_provider_profile_id(
            &conn,
            &resolved_agent.agent_id,
            ProviderKind::Acp,
            request.provider_profile_id,
            None,
            None,
        )?;
        request.provider_profile_id = Some(provider_profile_id);

        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        let configured_models =
            self.configured_models_for_profile(request.provider_profile_id.as_ref())?;
        let provider_result = provider
            .list_models(request.provider_profile_id.as_ref())
            .await;

        match provider_result {
            Ok(mut response) if !response.models.is_empty() => {
                response.models = merge_model_lists(&response.models, &configured_models);
                if response.provider_profile_id.is_none() {
                    response.provider_profile_id = request.provider_profile_id;
                }
                response.agent_id = Some(agent_id);
                Ok(response)
            }
            Ok(mut response) => {
                if configured_models.is_empty() {
                    if response.provider_profile_id.is_none() {
                        response.provider_profile_id = request.provider_profile_id;
                    }
                    response.agent_id = Some(agent_id);
                    return Ok(response);
                }
                response.provider_profile_id = request.provider_profile_id;
                response.agent_id = Some(agent_id);
                response.models = configured_models;
                response.source = AgentModelListSource::Configured;
                response.diagnostics.push(ProviderBindingMetadata {
                    key: "modelListFallback".to_string(),
                    value: "provider_profile".to_string(),
                });
                Ok(response)
            }
            Err(error) if !configured_models.is_empty() => Ok(AgentModelListResponse {
                agent_id: Some(agent_id),
                provider_kind: ProviderKind::Acp,
                provider_profile_id: request.provider_profile_id,
                models: configured_models,
                reasoning_efforts: Vec::new(),
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Configured,
                diagnostics: vec![
                    ProviderBindingMetadata {
                        key: "modelListProbe".to_string(),
                        value: "failed".to_string(),
                    },
                    ProviderBindingMetadata {
                        key: "modelListProbeError".to_string(),
                        value: error.code,
                    },
                ],
            }),
            Err(error) => Ok(AgentModelListResponse {
                agent_id: Some(agent_id),
                provider_kind: ProviderKind::Acp,
                provider_profile_id: request.provider_profile_id,
                models: Vec::new(),
                reasoning_efforts: Vec::new(),
                model_capabilities: Vec::new(),
                source: AgentModelListSource::Unavailable,
                diagnostics: vec![
                    ProviderBindingMetadata {
                        key: "modelListProbe".to_string(),
                        value: "failed".to_string(),
                    },
                    ProviderBindingMetadata {
                        key: "modelListProbeError".to_string(),
                        value: error.code,
                    },
                ],
            }),
        }
    }

    /// Stateless session-config discovery for one Agent Provider Profile.
    /// Returns empty evidence when the provider has no discovery support;
    /// catalog callers layer their own registry fallbacks on top.
    pub async fn probe_session_config(
        &self,
        agent_id: AgentId,
        provider_profile_id: ProviderProfileId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(agent_id), ProviderKind::Acp, false)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider.probe_session_config(&provider_profile_id).await
    }

    /// Stateless session-config discovery for one model in an Agent Provider
    /// Profile. The model id is projected into the short-lived probe process.
    pub async fn probe_session_config_for_model(
        &self,
        agent_id: AgentId,
        provider_profile_id: ProviderProfileId,
        model_id: &str,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(agent_id), ProviderKind::Acp, false)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider
            .probe_session_config_for_model(&provider_profile_id, model_id)
            .await
    }

    /// Discovers session-level options from the Agent CLI itself. This is
    /// intentionally independent of any Provider Profile so setup can cache
    /// one Agent-owned capability snapshot.
    pub async fn probe_agent_session_config(
        &self,
        agent_id: AgentId,
    ) -> VibexResult<AgentSessionConfigProbe> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(agent_id.clone()), ProviderKind::Acp, false)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider.probe_agent_session_config(&agent_id).await
    }

    pub async fn list_agent_auth_methods(
        &self,
        agent_id: AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(agent_id.clone()), ProviderKind::Acp, true)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider
            .list_auth_methods(&agent_id, provider_profile_id.as_ref())
            .await
    }

    pub async fn authenticate_agent(
        &self,
        request: AgentAuthenticateRequest,
    ) -> VibexResult<AgentAuthenticateResult> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(request.agent_id.clone()), ProviderKind::Acp, true)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider.authenticate_agent(request).await
    }

    pub async fn logout_agent(&self, request: AgentLogoutRequest) -> VibexResult<()> {
        let resolved_agent =
            self.resolve_enabled_agent(Some(request.agent_id.clone()), ProviderKind::Acp, true)?;
        let provider = self.runtime(&self.route_for_agent(&resolved_agent.agent_id)?)?;
        provider.logout_agent(request).await
    }

    fn validate_import_candidate(
        &self,
        candidate: &ExternalSessionImportCandidate,
    ) -> VibexResult<()> {
        if candidate.candidate_id.trim().is_empty() {
            return Err(VibexError::validation(
                "external_session_import_candidate_id_empty",
                "external session import candidate id must not be empty",
            ));
        }
        if candidate.title.trim().is_empty() {
            return Err(VibexError::validation(
                "external_session_import_title_empty",
                "external session import title must not be empty",
            ));
        }
        if candidate.workspace_root.trim().is_empty() {
            return Err(VibexError::validation(
                "external_session_import_workspace_root_empty",
                "external session import workspace root must not be empty",
            ));
        }
        if candidate.status != ExternalSessionImportCandidateStatus::Importable {
            return Err(VibexError::validation(
                "external_session_import_candidate_not_importable",
                "external session import candidate is not marked importable",
            )
            .with_diagnostic("candidateId", &candidate.candidate_id));
        }
        if candidate.provider_kind != candidate.source.provider_kind() {
            return Err(VibexError::validation(
                "external_session_import_provider_mismatch",
                "external session import source does not match provider kind",
            )
            .with_diagnostic("candidateId", &candidate.candidate_id)
            .with_diagnostic("source", candidate.source.to_string())
            .with_diagnostic("providerKind", candidate.provider_kind.to_string()));
        }
        if candidate.continuation_status == ExternalSessionContinuationStatus::Resumable {
            let has_resume_handle = match candidate.provider_kind {
                ProviderKind::Codex => candidate.native_thread_id.as_deref().is_some_and(has_text),
                ProviderKind::Claude => {
                    candidate.native_session_id.as_deref().is_some_and(has_text)
                }
                ProviderKind::Acp => candidate.native_session_id.as_deref().is_some_and(has_text),
            };
            if !has_resume_handle {
                return Err(VibexError::validation(
                    "external_session_import_resumable_handle_missing",
                    "resumable external session import requires a stable native resume handle",
                )
                .with_diagnostic("candidateId", &candidate.candidate_id)
                .with_diagnostic("providerKind", candidate.provider_kind.to_string()));
            }
        }
        Ok(())
    }

    fn configured_models_for_profile(
        &self,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<Vec<String>> {
        let Some(provider_profile_id) = provider_profile_id else {
            return Ok(Vec::new());
        };
        let conn = self.open_migrated()?;
        let Some(profile) = ProviderProfileRepository::get(&conn, provider_profile_id)? else {
            return Ok(Vec::new());
        };
        let configured = normalize_model_names(
            profile
                .configured_models
                .iter()
                .filter(|model| model.enabled)
                .map(|model| Some(model.id.clone())),
        );
        if !configured.is_empty() {
            return Ok(configured);
        }
        Ok(normalize_model_names([
            profile.default_model,
            profile.small_model,
            profile.large_model,
        ]))
    }

    fn resolve_enabled_agent(
        &self,
        agent_id: Option<AgentId>,
        legacy_provider_kind: ProviderKind,
        reject_disabled: bool,
    ) -> VibexResult<ResolvedAgent> {
        let agent_id = agent_id.unwrap_or_else(|| agent_id_for_provider_kind(legacy_provider_kind));
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id == agent_id)
            .ok_or_else(|| {
                VibexError::validation("agent_not_found", "Agent was not found")
                    .with_diagnostic("agentId", agent_id.as_str())
            })?;
        let conn = self.open_migrated()?;
        let config = AgentConfigRepository::get(&conn, &agent_id)?;
        let enabled = config
            .as_ref()
            .map(|config| config.enabled)
            .unwrap_or(definition.default_enabled);
        let deleted_at_ms = config.as_ref().and_then(|config| config.deleted_at_ms);
        let runtime_kind = config
            .as_ref()
            .map(|config| config.runtime_kind)
            .unwrap_or(definition.runtime_kind);
        let provider_kind = runtime_kind.provider_kind();

        if reject_disabled && (!enabled || deleted_at_ms.is_some()) {
            return Err(disabled_agent_error(
                &agent_id,
                enabled,
                deleted_at_ms,
                config.as_ref(),
            ));
        }

        Ok(ResolvedAgent {
            agent_id,
            provider_kind,
        })
    }

    fn runtime(
        &self,
        route: &vibex_core::AgentRuntimeRouteKey,
    ) -> VibexResult<Arc<dyn AgentProvider>> {
        self.runtimes.get(route).cloned().ok_or_else(|| {
            VibexError::capability(
                "provider_unregistered",
                format!(
                    "no online runtime is registered for route {}",
                    describe_runtime_route(route)
                ),
            )
            .with_diagnostic("runtimeRoute", describe_runtime_route(route))
        })
    }

    fn route_for_agent(&self, agent_id: &AgentId) -> VibexResult<vibex_core::AgentRuntimeRouteKey> {
        self.runtimes
            .keys()
            .find(|route| route.agent_id == *agent_id && route.transport_kind == TransportKind::Acp)
            .cloned()
            .ok_or_else(|| {
                VibexError::capability(
                    "provider_unregistered",
                    "no online ACP runtime is registered for this Agent",
                )
                .with_diagnostic("agentId", agent_id.as_str())
            })
    }

    fn usage_counter_origin(
        &self,
        conn: &DbConnection,
        binding_id: &vibex_core::RuntimeBindingId,
    ) -> VibexResult<AgentUsageCounterOrigin> {
        let Some(binding) = RuntimeBindingRepository::get(conn, binding_id)? else {
            return Ok(AgentUsageCounterOrigin::Unknown);
        };
        let Some(switch_id) = binding.created_by_switch_id.as_ref() else {
            return Ok(AgentUsageCounterOrigin::Unknown);
        };
        let Some(record) = RuntimeSwitchRepository::get(conn, switch_id)? else {
            return Ok(AgentUsageCounterOrigin::Unknown);
        };
        Ok(usage_counter_origin_for_switch_method(
            record
                .restore_compatibility_result
                .and_then(|result| result.method),
        ))
    }

    pub(crate) fn open_migrated(&self) -> VibexResult<DbConnection> {
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        Ok(conn)
    }

    fn recover_interrupted_sessions(&self, conn: &mut DbConnection) -> VibexResult<()> {
        let sessions = SessionRepository::list(conn, true)?;
        for session in sessions {
            if !matches!(
                session.state,
                AgentSessionState::Initializing | AgentSessionState::Running
            ) {
                continue;
            }

            if session.state == AgentSessionState::Initializing {
                let runtime_state =
                    AgentSessionRuntimeRepository::get_runtime_state(conn, &session.id)?;
                let has_reconcilable_initial_runtime = match runtime_state.as_ref() {
                    Some(state)
                        if state.selection_revision > 0
                            && state.desired_runtime_selection.is_some() =>
                    {
                        let latest = RuntimeSwitchRepository::get_latest_for_selection_revision(
                            conn,
                            &session.id,
                            state.selection_revision,
                            false,
                        )?;
                        state.current_binding_id.is_some()
                            || latest.is_some_and(|record| !record.status.is_terminal())
                    }
                    _ => false,
                };
                if has_reconcilable_initial_runtime {
                    continue;
                }
            }

            let err = VibexError::process(
                "agent_turn_recovered_after_restart",
                "Agent session was interrupted before the provider turn completed",
            )
            .with_recovery_hint("Retry the message or continue from the failed turn.")
            .with_diagnostic("previousState", format!("{:?}", session.state));
            let _ = self.append_provider_error(conn, &session.id, &err);
            SessionRepository::update_state(conn, &session.id, AgentSessionState::Error)?;
        }

        Ok(())
    }

    fn transition(
        &self,
        conn: &DbConnection,
        session: &AgentSession,
        to: AgentSessionState,
    ) -> VibexResult<()> {
        validate_transition(session.state, to)?;
        SessionRepository::update_state(conn, &session.id, to)
    }

    fn append_system_notice(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        message: impl Into<String>,
        level: SystemNoticeLevel,
    ) -> VibexResult<TimelineItem> {
        self.append_timeline_item(
            conn,
            session_id,
            TimelineSource::System,
            TimelinePayload::SystemNotice(SystemNoticePayload {
                level,
                message: message.into(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
    }

    fn append_turn_startup_notice(
        &self,
        conn: &mut DbConnection,
        session: &AgentSession,
        provider: &dyn AgentProvider,
        required_runtime: &SessionRuntimeSelection,
        correlation_id: Option<&vibex_core::CorrelationId>,
    ) -> VibexResult<Vec<TimelineItem>> {
        let capabilities =
            provider.capabilities_for_profile(Some(&required_runtime.provider_profile_id));
        let resource_summary = turn_resource_summary(
            conn,
            &required_runtime.agent_id,
            ProviderKind::Acp,
            &capabilities,
        )?;
        let provider_label = provider_display_name(ProviderKind::Acp);
        let message = match resource_summary {
            Some(summary) => {
                format!(
                    "Starting {provider_label} agent runtime; preparing context with {summary}; waiting for first response..."
                )
            }
            None => {
                format!("Starting {provider_label} agent runtime; waiting for first response...")
            }
        };

        Ok(vec![self.append_timeline_item(
            conn,
            &session.id,
            TimelineSource::System,
            TimelinePayload::SystemNotice(SystemNoticePayload {
                level: SystemNoticeLevel::Info,
                message,
            }),
            correlation_id,
            None,
            TimelineRedactionState::None,
        )?])
    }

    fn append_provider_error(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        err: &VibexError,
    ) -> VibexResult<TimelineItem> {
        self.append_provider_error_with_attribution(conn, session_id, err, None)
    }

    fn append_provider_error_with_attribution(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        err: &VibexError,
        execution_attribution: Option<&TurnExecutionAttribution>,
    ) -> VibexResult<TimelineItem> {
        let item = TimelineRepository::append_with_attribution(
            conn,
            session_id,
            TimelineSource::Provider,
            TimelinePayload::Error(TimelineErrorPayload {
                code: err.code.clone(),
                message: err.message.clone(),
                recoverable: true,
            }),
            err.correlation_id.as_ref(),
            None,
            TimelineRedactionState::None,
            execution_attribution,
        )?;
        self.publish_timeline_item(item)
    }

    fn finish_turn_with_error(
        &self,
        session_id: &VibexSessionId,
        err: &VibexError,
    ) -> VibexResult<()> {
        let mut conn = self.open_migrated()?;
        self.finish_turn_with_error_on_conn(&mut conn, session_id, err)
    }

    fn finish_turn_with_error_on_conn(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        err: &VibexError,
    ) -> VibexResult<()> {
        self.finish_turn_with_error_on_conn_with_attribution(conn, session_id, err, None)
    }

    fn finish_turn_with_error_on_conn_with_attribution(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        err: &VibexError,
        execution_attribution: Option<&TurnExecutionAttribution>,
    ) -> VibexResult<()> {
        let _ = self.append_provider_error_with_attribution(
            conn,
            session_id,
            err,
            execution_attribution,
        );
        SessionRepository::update_state(conn, session_id, AgentSessionState::Error)
    }

    fn append_provider_event(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        event: ProviderEvent,
        coalesce_after_sequence: i64,
        execution_attribution: Option<&TurnExecutionAttribution>,
    ) -> VibexResult<TimelineItem> {
        if should_coalesce_provider_event(&event)
            && let Some(provider_correlation_id) = event.provider_correlation_id.as_deref()
        {
            let item = TimelineRepository::upsert_by_provider_correlation(
                conn,
                session_id,
                event.source,
                event.payload,
                provider_correlation_id,
                coalesce_after_sequence,
                event.redaction_state,
                execution_attribution,
            )?;
            let _ = self.live_events.send(TimelineLiveEvent {
                session_id: session_id.clone(),
                sequence: item.sequence,
                item: item.clone(),
            });
            return Ok(item);
        }
        let item = TimelineRepository::append_with_attribution(
            conn,
            session_id,
            event.source,
            event.payload,
            None,
            event.provider_correlation_id.as_deref(),
            event.redaction_state,
            execution_attribution,
        )?;
        self.publish_timeline_item(item)
    }

    fn append_streamed_provider_event(
        &self,
        session_id: &VibexSessionId,
        event: ProviderEvent,
        coalesce_after_sequence: i64,
        execution_attribution: Option<&TurnExecutionAttribution>,
        needs_input: &mut bool,
    ) -> VibexResult<TimelineItem> {
        let mut conn = self.open_migrated()?;
        if let TimelinePayload::PermissionRequest(permission) = &event.payload {
            PermissionRepository::insert_request(&conn, permission)?;
            *needs_input = true;
        }
        if let TimelinePayload::ElicitationRequest(elicitation) = &event.payload {
            ElicitationRepository::insert_request(&conn, elicitation)?;
            *needs_input = true;
        }
        self.append_provider_event(
            &mut conn,
            session_id,
            event,
            coalesce_after_sequence,
            execution_attribution,
        )
    }

    fn handle_streamed_provider_event(
        &self,
        session_id: &VibexSessionId,
        event: ProviderEvent,
        coalesce_after_sequence: i64,
        execution_attribution: Option<&TurnExecutionAttribution>,
        needs_input: &mut bool,
    ) -> VibexResult<TimelineItem> {
        self.append_streamed_provider_event(
            session_id,
            event,
            coalesce_after_sequence,
            execution_attribution,
            needs_input,
        )
    }

    fn durable_session_execution(
        &self,
        conn: &DbConnection,
        session: &AgentSession,
    ) -> VibexResult<(
        SessionRuntimeSelection,
        ProviderBinding,
        ProviderTurnExecutionIdentity,
        vibex_core::AgentRuntimeRouteKey,
    )> {
        let state = AgentSessionRuntimeRepository::get_runtime_state(conn, &session.id)?
            .ok_or_else(|| {
                VibexError::conflict(
                    "session_runtime_state_missing",
                    "Agent session has no durable runtime state",
                )
            })?;
        let selection = state.effective_runtime_selection.clone().ok_or_else(|| {
            VibexError::conflict(
                "session_runtime_selection_missing",
                "Agent session has no effective ACP runtime selection",
            )
        })?;
        if state.desired_runtime_selection.as_ref() != Some(&selection)
            || state.runtime_selection_status != Some(SessionRuntimeSelectionStatus::Ready)
            || state.pending_switch_id.is_some()
            || state.current_agent_id.as_ref() != Some(&selection.agent_id)
            || session.agent_id != selection.agent_id
        {
            return Err(VibexError::conflict(
                "session_runtime_not_ready",
                "Agent session runtime is not ready for online execution",
            ));
        }
        let current_binding_id = state.current_binding_id.as_ref().ok_or_else(|| {
            VibexError::conflict(
                "session_runtime_binding_missing",
                "Agent session has no committed runtime binding",
            )
        })?;
        let (binding, identity, route_key) = self.durable_provider_turn_binding(
            conn,
            session,
            &selection,
            current_binding_id,
            state.activation_generation,
        )?;
        Ok((selection, binding, identity, route_key))
    }

    fn durable_provider_turn_binding(
        &self,
        conn: &DbConnection,
        session: &AgentSession,
        required_runtime: &SessionRuntimeSelection,
        current_binding_id: &vibex_core::RuntimeBindingId,
        activation_generation: i64,
    ) -> VibexResult<(
        ProviderBinding,
        ProviderTurnExecutionIdentity,
        vibex_core::AgentRuntimeRouteKey,
    )> {
        let runtime_binding =
            RuntimeBindingRepository::get(conn, current_binding_id)?.ok_or_else(|| {
                VibexError::conflict(
                    "message_submission_runtime_binding_missing",
                    "durable message submission runtime binding is unavailable",
                )
            })?;
        let config = &runtime_binding.session_runtime_config_state;
        if runtime_binding.session_id != session.id
            || runtime_binding.agent_id != required_runtime.agent_id
            || runtime_binding.provider_profile_id != required_runtime.provider_profile_id
            || runtime_binding.transport_kind != TransportKind::Acp
            || runtime_binding.binding_state != BindingState::Current
            || runtime_binding.activation_generation != activation_generation
            || !required_runtime.matches_effective_config(config)
            || !config.is_applied_to_generation(activation_generation)
        {
            return Err(VibexError::conflict(
                "message_submission_runtime_binding_mismatch",
                "committed runtime binding no longer matches the durable message submission",
            ));
        }
        let native_session_id = runtime_binding.native_session_id.clone().ok_or_else(|| {
            VibexError::conflict(
                "message_submission_runtime_native_session_missing",
                "committed runtime binding has no native session",
            )
        })?;
        let mut metadata = vec![ProviderBindingMetadata {
            key: PROVIDER_SELECTED_MODEL_METADATA_KEY.to_string(),
            value: required_runtime.model_id.clone(),
        }];
        if let Some(reasoning_effort) = required_runtime.reasoning_effort.as_deref() {
            metadata.push(ProviderBindingMetadata {
                key: PROVIDER_SELECTED_REASONING_EFFORT_METADATA_KEY.to_string(),
                value: reasoning_effort.to_string(),
            });
        }
        let route_key = vibex_core::AgentRuntimeRouteKey {
            agent_id: runtime_binding.agent_id.clone(),
            transport_kind: runtime_binding.transport_kind,
            adapter_id: runtime_binding.adapter_id.clone(),
        };
        let binding = ProviderBinding {
            session_id: session.id.clone(),
            provider_kind: ProviderKind::Acp,
            provider_profile_id: runtime_binding.provider_profile_id,
            native: ProviderNativeBinding {
                native_session_id: Some(native_session_id),
                native_thread_id: None,
                native_resume_token: None,
                session_config_state: None,
                redacted_metadata: metadata,
            },
            created_at_ms: runtime_binding.created_at_ms,
            updated_at_ms: runtime_binding.updated_at_ms,
        };
        let identity = ProviderTurnExecutionIdentity {
            binding_id: runtime_binding.binding_id,
            activation_generation,
            model_id: required_runtime.model_id.clone(),
        };
        Ok((binding, identity, route_key))
    }

    #[allow(clippy::too_many_arguments)]
    fn append_timeline_item(
        &self,
        conn: &mut DbConnection,
        session_id: &VibexSessionId,
        source: TimelineSource,
        payload: TimelinePayload,
        correlation_id: Option<&vibex_core::CorrelationId>,
        provider_correlation_id: Option<&str>,
        redaction_state: TimelineRedactionState,
    ) -> VibexResult<TimelineItem> {
        let item = TimelineRepository::append(
            conn,
            session_id,
            source,
            payload,
            correlation_id,
            provider_correlation_id,
            redaction_state,
        )?;
        self.publish_timeline_item(item)
    }

    fn publish_timeline_item(&self, item: TimelineItem) -> VibexResult<TimelineItem> {
        let _ = self.live_events.send(TimelineLiveEvent {
            session_id: item.session_id.clone(),
            sequence: item.sequence,
            item: item.clone(),
        });
        Ok(item)
    }

    fn turn_execution_attribution(
        &self,
        binding: &ProviderBinding,
        identity: &ProviderTurnExecutionIdentity,
    ) -> VibexResult<TurnExecutionAttribution> {
        let conn = self.open_migrated()?;
        let profile = ProviderProfileRepository::get(&conn, &binding.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::storage(
                    "turn_execution_profile_missing",
                    "provider profile for turn execution attribution was not found",
                )
            })?;
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id == profile.agent_id)
            .ok_or_else(|| {
                VibexError::storage(
                    "turn_execution_agent_missing",
                    "Agent definition for turn execution attribution was not found",
                )
            })?;
        let config = AgentConfigRepository::get(&conn, &profile.agent_id)?;
        let agent_label = config
            .as_ref()
            .and_then(|config| config.label_override.as_deref())
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(&definition.label);
        let model_label = profile
            .configured_models
            .iter()
            .find(|model| model.id == identity.model_id)
            .and_then(|model| model.display_name.as_deref())
            .map(str::trim)
            .filter(|label| !label.is_empty())
            .unwrap_or(&identity.model_id);

        TurnExecutionAttribution::new(
            profile.agent_id,
            profile.id,
            identity.model_id.clone(),
            identity.binding_id.clone(),
            identity.activation_generation,
            agent_label,
            profile.display_name,
            model_label,
        )
        .ok_or_else(|| {
            VibexError::validation(
                "turn_execution_attribution_invalid",
                "provider returned an invalid turn execution attribution",
            )
        })
    }
}

fn usage_counter_origin_for_switch_method(
    method: Option<AgentSessionRestoreMethod>,
) -> AgentUsageCounterOrigin {
    match method {
        Some(AgentSessionRestoreMethod::Resume | AgentSessionRestoreMethod::Load) => {
            AgentUsageCounterOrigin::Resumed
        }
        Some(AgentSessionRestoreMethod::New) | None => AgentUsageCounterOrigin::KnownZero,
    }
}

fn push_or_replace_timeline_item(items: &mut Vec<TimelineItem>, item: TimelineItem) -> usize {
    if let Some((index, existing)) = items
        .iter_mut()
        .enumerate()
        .find(|(_, existing)| existing.id == item.id || existing.sequence == item.sequence)
    {
        *existing = item;
        return index;
    }
    let index = items.len();
    items.push(item);
    index
}

fn provider_event_matches_timeline_item(event: &ProviderEvent, item: &TimelineItem) -> bool {
    item.provider_correlation_id == event.provider_correlation_id
        && item.source == event.source
        && item.payload == event.payload
        && item.redaction_state == event.redaction_state
}

fn provider_event_was_streamed(
    event: &ProviderEvent,
    streamed_items: &[TimelineItem],
    streamed_event_indices: &HashMap<String, usize>,
) -> bool {
    event
        .provider_correlation_id
        .as_ref()
        .is_some_and(|correlation| {
            streamed_event_indices
                .get(correlation)
                .and_then(|index| streamed_items.get(*index))
                .is_some_and(|item| provider_event_matches_timeline_item(event, item))
        })
}

fn describe_runtime_route(route: &vibex_core::AgentRuntimeRouteKey) -> String {
    format!(
        "{}/{}/{}",
        route.agent_id, route.transport_kind, route.adapter_id
    )
}

fn should_coalesce_provider_event(event: &ProviderEvent) -> bool {
    event.provider_correlation_id.is_some()
        && matches!(
            &event.payload,
            TimelinePayload::Error(error)
                if matches!(
                    error.code.as_str(),
                    "codex_stream_reconnecting" | "opencode_model_api_retrying"
                )
        )
}

fn fork_timeline_appends(items: &[TimelineItem]) -> Vec<TimelineAppend> {
    items
        .iter()
        .filter(|item| {
            !matches!(
                &item.payload,
                TimelinePayload::SystemNotice(_)
                    | TimelinePayload::PermissionRequest(_)
                    | TimelinePayload::PermissionResolution(_)
                    | TimelinePayload::ElicitationRequest(_)
                    | TimelinePayload::ElicitationResolution(_)
            )
        })
        .map(|item| TimelineAppend {
            source: item.source,
            payload: item.payload.clone(),
            timestamp_ms: Some(item.timestamp_ms),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: item.redaction_state,
            execution_attribution: None,
        })
        .collect()
}

fn has_text(value: &str) -> bool {
    !value.trim().is_empty()
}

fn normalize_model_names(models: impl IntoIterator<Item = Option<String>>) -> Vec<String> {
    let mut normalized = Vec::new();
    for model in models.into_iter().flatten() {
        let model = model.trim();
        if !model.is_empty() && !normalized.iter().any(|existing| existing == model) {
            normalized.push(model.to_string());
        }
    }
    normalized
}

fn merge_model_lists(probed_models: &[String], configured_models: &[String]) -> Vec<String> {
    let probed = probed_models.iter().cloned().map(Some);
    let configured = configured_models.iter().cloned().map(Some);
    normalize_model_names(probed.chain(configured))
}

fn required_command_discovery_agent_id(
    request: &AgentCommandDiscoverRequest,
) -> VibexResult<AgentId> {
    request.agent_id.clone().ok_or_else(|| {
        VibexError::validation(
            "agent_id_required",
            "Agent command discovery requires an Agent, Profile, or Logical Session",
        )
    })
}

fn resolve_provider_profile_id(
    conn: &DbConnection,
    agent_id: &AgentId,
    provider_kind: ProviderKind,
    requested_profile_id: Option<ProviderProfileId>,
    project_id: Option<&ProjectId>,
    workspace_id: Option<&WorkspaceId>,
) -> VibexResult<ProviderProfileId> {
    if let Some(provider_profile_id) = requested_profile_id {
        let Some(profile) = ProviderProfileRepository::get(conn, &provider_profile_id)? else {
            return Err(VibexError::validation(
                "provider_profile_not_found",
                "provider profile was not found",
            )
            .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
        };
        if profile.agent_id != *agent_id {
            return Err(VibexError::validation(
                "provider_profile_agent_mismatch",
                "provider profile belongs to another agent",
            )
            .with_diagnostic("agentId", agent_id.as_str())
            .with_diagnostic("profileAgentId", profile.agent_id.as_str())
            .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
        }
        if profile.kind != provider_kind {
            return Err(VibexError::validation(
                "provider_profile_kind_mismatch",
                "provider profile kind does not match resolved agent runtime",
            )
            .with_diagnostic("agentId", agent_id.as_str())
            .with_diagnostic("providerKind", provider_kind.to_string())
            .with_diagnostic("profileProviderKind", profile.kind.to_string()));
        }
        return Ok(provider_profile_id);
    }

    for scope in provider_profile_default_scopes(project_id, workspace_id) {
        let default_selection =
            AgentDefaultModelProviderProfileRepository::get(conn, scope, agent_id.clone())?;
        if let Some(default_profile_id) = default_selection.provider_profile_id
            && let Some(profile) = ProviderProfileRepository::get(conn, &default_profile_id)?
            && profile.agent_id == *agent_id
            && profile.kind == provider_kind
            && profile.status == vibex_core::ProviderProfileStatus::Enabled
        {
            return Ok(default_profile_id);
        }
    }

    if let Some(profile) = ProviderProfileRepository::first_enabled_for_agent(conn, agent_id)?
        && profile.kind == provider_kind
    {
        return Ok(profile.id);
    }

    ProviderProfileId::parse(provider_kind.local_default_profile_id().to_string()).map_err(|_| {
        VibexError::storage(
            "provider_default_profile_id_invalid",
            "local default provider profile id is invalid",
        )
    })
}

fn provider_profile_default_scopes(
    project_id: Option<&ProjectId>,
    workspace_id: Option<&WorkspaceId>,
) -> Vec<ProviderProfileDefaultScope> {
    let mut scopes = Vec::new();
    if let (Some(project_id), Some(workspace_id)) = (project_id, workspace_id) {
        scopes.push(ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Workspace,
            project_id: Some(project_id.clone()),
            workspace_id: Some(workspace_id.clone()),
        });
    }
    if let Some(project_id) = project_id {
        scopes.push(ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Project,
            project_id: Some(project_id.clone()),
            workspace_id: None,
        });
    }
    scopes.push(ProviderProfileDefaultScope {
        kind: ProviderDefaultScopeKind::Global,
        project_id: None,
        workspace_id: None,
    });
    scopes
}

fn normalize_reasoning_effort(value: Option<&str>) -> VibexResult<Option<String>> {
    let Some(value) = value.map(str::trim).filter(|value| !value.is_empty()) else {
        return Ok(None);
    };
    if value.len() > 64
        || !value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
    {
        return Err(VibexError::validation(
            "reasoning_effort_invalid",
            "reasoning effort must be a short provider capability identifier",
        ));
    }
    Ok(Some(value.to_string()))
}

fn disabled_agent_error(
    agent_id: &AgentId,
    enabled: bool,
    deleted_at_ms: Option<i64>,
    config: Option<&AgentConfig>,
) -> VibexError {
    let mut error = VibexError::validation(
        "agent_disabled",
        "Agent is disabled and cannot create new sessions",
    )
    .with_diagnostic("agentId", agent_id.as_str())
    .with_diagnostic("enabled", enabled.to_string());
    if let Some(deleted_at_ms) = deleted_at_ms {
        error = error.with_diagnostic("deletedAtMs", deleted_at_ms.to_string());
    }
    if let Some(config) = config {
        error = error.with_diagnostic("updatedAtMs", config.updated_at_ms.to_string());
    }
    error
}

fn runtime_mcp_server_from_record(server: McpServer) -> Option<ProviderRuntimeMcpServer> {
    let transport = match server.transport_kind {
        McpServerTransportKind::Stdio => ProviderRuntimeMcpTransport::Stdio,
        McpServerTransportKind::Http => ProviderRuntimeMcpTransport::Http,
        McpServerTransportKind::Sse => ProviderRuntimeMcpTransport::Sse,
    };
    match transport {
        ProviderRuntimeMcpTransport::Stdio if server.command.as_deref()?.trim().is_empty() => None,
        ProviderRuntimeMcpTransport::Stdio => Some(ProviderRuntimeMcpServer {
            id: server.id.as_str().to_string(),
            display_name: server.display_name,
            transport,
            command: server.command,
            args: server.args,
            url: None,
        }),
        ProviderRuntimeMcpTransport::Http | ProviderRuntimeMcpTransport::Sse
            if server.url.as_deref()?.trim().is_empty() =>
        {
            None
        }
        ProviderRuntimeMcpTransport::Http | ProviderRuntimeMcpTransport::Sse => {
            Some(ProviderRuntimeMcpServer {
                id: server.id.as_str().to_string(),
                display_name: server.display_name,
                transport,
                command: None,
                args: Vec::new(),
                url: server.url,
            })
        }
    }
}

fn turn_resource_summary(
    conn: &DbConnection,
    agent_id: &AgentId,
    provider_kind: ProviderKind,
    capabilities: &ProviderCapabilities,
) -> VibexResult<Option<String>> {
    let mut parts = Vec::new();

    if capabilities.mcp_servers {
        let mcp_count =
            McpServerRepository::list_enabled_for_agent(conn, agent_id, provider_kind)?.len();
        if mcp_count > 0 {
            parts.push(resource_count(mcp_count, "MCP server"));
        }
    }

    if capabilities.skills {
        let skill_count =
            SkillRepository::list_enabled_for_agent(conn, agent_id, provider_kind)?.len();
        let prompt_count = PromptRepository::list_enabled(conn)?.len();
        let prompt_and_skill_count = skill_count + prompt_count;
        if prompt_and_skill_count > 0 {
            parts.push(resource_count(prompt_and_skill_count, "skill/prompt"));
        }
    }

    if parts.is_empty() {
        Ok(None)
    } else {
        Ok(Some(parts.join(" and ")))
    }
}

fn resource_count(count: usize, label: &str) -> String {
    let suffix = if count == 1 { "" } else { "s" };
    format!("{count} {label}{suffix}")
}

fn provider_display_name(kind: ProviderKind) -> &'static str {
    match kind {
        ProviderKind::Codex => "Codex",
        ProviderKind::Claude => "Claude",
        ProviderKind::Acp => "ACP",
    }
}

fn command_trigger_matches(
    requested: Option<AgentCommandTrigger>,
    candidate: AgentCommandTrigger,
) -> bool {
    requested.is_none_or(|trigger| trigger == candidate)
}

fn command_token_from_display_name(value: &str) -> String {
    let mut token = String::new();
    let mut last_was_separator = false;
    for ch in value.trim().trim_start_matches(['/', '$', '@']).chars() {
        if ch.is_whitespace() {
            if !token.is_empty() && !last_was_separator {
                token.push('-');
                last_was_separator = true;
            }
            continue;
        }
        if ch == '/' || ch.is_control() {
            continue;
        }
        token.extend(ch.to_lowercase());
        last_was_separator = false;
    }
    let token = token.trim_matches('-').to_string();
    if token.is_empty() {
        "command".to_string()
    } else {
        token
    }
}

fn expand_prompt_body(body: &str, arguments: &str) -> String {
    if arguments.is_empty() {
        return body.to_string();
    }
    if body.contains("{{input}}") {
        body.replace("{{input}}", arguments)
    } else {
        format!("{}\n\n{}", body.trim_end(), arguments)
    }
}

fn filter_and_limit_command_entries(
    entries: &mut Vec<AgentCommandEntry>,
    query: Option<&str>,
    limit: Option<u32>,
) {
    if let Some(query) = query.map(str::trim).filter(|query| !query.is_empty()) {
        let query = query.to_lowercase();
        entries.retain(|entry| {
            entry.label.to_lowercase().contains(&query)
                || entry.insertion_text.to_lowercase().contains(&query)
                || entry
                    .description
                    .as_deref()
                    .is_some_and(|description| description.to_lowercase().contains(&query))
        });
    }
    entries.sort_by(|left, right| {
        left.trigger
            .cmp(&right.trigger)
            .then_with(|| left.source_kind.cmp(&right.source_kind))
            .then_with(|| left.label.cmp(&right.label))
    });
    if let Some(limit) = limit {
        entries.truncate(limit as usize);
    }
}

fn import_preview_diagnostic(
    source: ExternalSessionImportSource,
    provider_profile_id: Option<&ProviderProfileId>,
    error: VibexError,
) -> ExternalSessionImportDiagnostic {
    let mut redacted_details = vec![
        ProviderBindingMetadata {
            key: "category".to_string(),
            value: format!("{:?}", error.category),
        },
        ProviderBindingMetadata {
            key: "code".to_string(),
            value: error.code.clone(),
        },
    ];
    if let Some(provider_profile_id) = provider_profile_id {
        redacted_details.push(ProviderBindingMetadata {
            key: "providerProfileId".to_string(),
            value: provider_profile_id.as_str().to_string(),
        });
    }
    ExternalSessionImportDiagnostic {
        code: error.code,
        message: error.message,
        source,
        redacted_details,
    }
}

fn imported_session_notice(candidate: &ExternalSessionImportCandidate) -> String {
    match candidate.continuation_status {
        ExternalSessionContinuationStatus::Resumable => format!(
            "Imported {} session history. This session can continue through the native provider.",
            candidate.source
        ),
        ExternalSessionContinuationStatus::ReadOnly => format!(
            "Imported {} session history as read-only. {}",
            candidate.source,
            candidate
                .continuation_reason
                .as_deref()
                .filter(|value| has_text(value))
                .unwrap_or("No stable native resume handle was available.")
        ),
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Mutex};

    use async_trait::async_trait;
    use vibex_core::{
        AcpAdapterId, AgentRuntimeRouteKey, AgentSessionSafety, NativeStateHomeId, RuntimeBinding,
        RuntimeBindingId, RuntimeSwitchActiveWorkPolicy, RuntimeSwitchId, RuntimeSwitchPolicy,
        SessionRuntimeConfigState, WorkspaceMode,
    };
    use vibex_db::{DesiredRuntimeSwitchEnqueueRequest, WorkspaceRepository};

    use super::*;
    use crate::adapter::{
        ProviderCreateRequest, ProviderSessionHandle, ProviderTurnRequest, ProviderTurnResult,
    };
    use crate::runtime_switch::RuntimeSwitchCoordinator;

    struct TestProvider {
        kind: ProviderKind,
    }

    struct UsageOriginProvider {
        identity: ProviderTurnExecutionIdentity,
    }

    struct RetryingElicitationProvider {
        attempts: AtomicUsize,
        identity: ProviderTurnExecutionIdentity,
        callback_started: tokio::sync::Notify,
        callback_release: tokio::sync::Notify,
        delivered: Mutex<Vec<vibex_core::ElicitationResolution>>,
    }

    #[async_trait]
    impl AgentProvider for TestProvider {
        fn kind(&self) -> ProviderKind {
            self.kind
        }

        fn capabilities(&self) -> ProviderCapabilities {
            unreachable!("route registration must not probe provider capabilities")
        }

        async fn create_session(
            &self,
            _request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("route registration must not create a provider session")
        }

        async fn resume_session(
            &self,
            _binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("route registration must not resume a provider session")
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            _request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            unreachable!("route registration must not send a provider turn")
        }
    }

    #[async_trait]
    impl AgentProvider for UsageOriginProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Acp
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::conservative(ProviderKind::Acp, "usage-origin-test")
        }

        async fn create_session(
            &self,
            _request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("usage-origin test resumes its seeded binding")
        }

        async fn resume_session(
            &self,
            binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            Ok(ProviderSessionHandle {
                binding,
                capabilities: self.capabilities(),
            })
        }

        async fn prepare_turn_execution(
            &self,
            _handle: &ProviderSessionHandle,
            _request: &ProviderTurnRequest,
        ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
            Ok(Some(self.identity.clone()))
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            _request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            unreachable!("usage-origin test injects a runner")
        }
    }

    #[async_trait]
    impl AgentProvider for RetryingElicitationProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Acp
        }

        fn capabilities(&self) -> ProviderCapabilities {
            let mut capabilities =
                ProviderCapabilities::conservative(ProviderKind::Acp, "elicitation-retry-test");
            capabilities.elicitation = true;
            capabilities
        }

        async fn create_session(
            &self,
            _request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("elicitation retry test uses a seeded binding")
        }

        async fn resume_session(
            &self,
            binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            Ok(ProviderSessionHandle {
                binding,
                capabilities: self.capabilities(),
            })
        }

        async fn prepare_turn_execution(
            &self,
            _handle: &ProviderSessionHandle,
            _request: &ProviderTurnRequest,
        ) -> VibexResult<Option<ProviderTurnExecutionIdentity>> {
            Ok(Some(self.identity.clone()))
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            _request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            unreachable!("elicitation retry test does not send a turn")
        }

        async fn resolve_elicitation(
            &self,
            request: ProviderElicitationResolution,
        ) -> VibexResult<()> {
            let attempt = self.attempts.fetch_add(1, Ordering::SeqCst);
            if attempt == 0 {
                return Err(VibexError::provider(
                    "elicitation_callback_failed",
                    "injected callback failure",
                ));
            }
            self.delivered.lock().unwrap().push(request.resolution);
            if attempt == 1 {
                self.callback_started.notify_one();
                self.callback_release.notified().await;
            }
            Ok(())
        }
    }

    #[tokio::test]
    async fn continue_turn_uses_latest_timeline_completion_for_idle_sessions() {
        let db_path = temp_db_path("continue-incomplete-idle");
        let workspace_root = temp_workspace_path("continue-incomplete-idle");
        fs::create_dir_all(&workspace_root).unwrap();
        let manager = AgentManager::new(&db_path).unwrap();
        let mut conn = manager.open_migrated().unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let agent_id = AgentId::parse("claude").unwrap();
        let incomplete = insert_session(
            &conn,
            "incomplete idle turn",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id.clone(),
            AgentSessionState::Idle,
        );
        TimelineRepository::append(
            &mut conn,
            &incomplete.id,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "finish the task".into(),
                attachments: Vec::new(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        TimelineRepository::append(
            &mut conn,
            &incomplete.id,
            TimelineSource::Agent,
            TimelinePayload::AgentMessageDelta(vibex_core::AgentMessageDeltaPayload {
                text_delta: "still working".into(),
                chunk_index: 0,
                phase: Some(vibex_core::AgentMessagePhase::Commentary),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();

        let finished = insert_session(
            &conn,
            "finished idle turn",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id,
            AgentSessionState::Idle,
        );
        TimelineRepository::append(
            &mut conn,
            &finished.id,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "finish the task".into(),
                attachments: Vec::new(),
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        TimelineRepository::append(
            &mut conn,
            &finished.id,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
            None,
            None,
            TimelineRedactionState::None,
        )
        .unwrap();
        drop(conn);

        let incomplete_error = manager
            .continue_turn(ContinueAgentTurnRequest {
                session_id: incomplete.id,
                correlation_id: None,
            })
            .await
            .unwrap_err();
        assert_ne!(
            incomplete_error.code,
            "agent_continue_requires_incomplete_turn"
        );

        let finished_error = manager
            .continue_turn(ContinueAgentTurnRequest {
                session_id: finished.id,
                correlation_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(
            finished_error.code,
            "agent_continue_requires_incomplete_turn"
        );

        cleanup_db(&db_path);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn elicitation_turn_needs_input_and_callback_failure_remains_retryable() {
        let db_path = temp_db_path("elicitation-callback-retry");
        let workspace_root = temp_workspace_path("elicitation-callback-retry");
        fs::create_dir_all(&workspace_root).unwrap();
        let mut manager = AgentManager::new(&db_path).unwrap();
        let mut conn = manager.open_migrated().unwrap();
        ProviderProfileRepository::ensure_local_defaults(&conn).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let agent_id = AgentId::parse("elicitation-test-agent").unwrap();
        let adapter_id = AcpAdapterId::parse("elicitation-test-acp").unwrap();
        let provider_profile_id =
            ProviderProfileId::parse(ProviderKind::Acp.local_default_profile_id().to_string())
                .unwrap();
        let session = insert_session(
            &conn,
            "elicitation callback retry",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id.clone(),
            AgentSessionState::Idle,
        );
        let selection = SessionRuntimeSelection {
            agent_id: agent_id.clone(),
            provider_profile_id: provider_profile_id.clone(),
            model_id: "elicitation-test-model".to_string(),
            reasoning_effort: None,
            mode_id: None,
            config_values: Default::default(),
        };
        let mut runtime_config = SessionRuntimeConfigState {
            preferred_model: Some(selection.model_id.clone()),
            effective_model: Some(selection.model_id.clone()),
            ..SessionRuntimeConfigState::default()
        };
        runtime_config.mark_generation_if_converged(0);
        let now = unix_timestamp_ms();
        let binding = RuntimeBinding {
            binding_id: RuntimeBindingId::new(),
            session_id: session.id.clone(),
            agent_id: agent_id.clone(),
            transport_kind: TransportKind::Acp,
            provider_profile_id,
            adapter_id: adapter_id.clone(),
            adapter_version: "1.0.0".to_string(),
            adapter_compatibility_identity: "elicitation-test-acp@1".to_string(),
            native_session_id: Some("native-elicitation-test".to_string()),
            native_state_home_id: NativeStateHomeId::new(),
            provider_resume_identity: None,
            process_spawn_fingerprint: "elicitation-test".to_string(),
            session_runtime_config_state: runtime_config,
            capability_snapshot: None,
            restore_compatibility_key: None,
            profile_revision: 1,
            last_context_sequence: 0,
            last_summary_sequence: 0,
            context_bridge_version: 0,
            activation_generation: 0,
            binding_state: BindingState::Current,
            created_by_switch_id: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        AgentSessionRuntimeRepository::initialize_runtime_selection(
            &mut conn, &binding, &selection,
        )
        .unwrap();
        drop(conn);

        let provider = Arc::new(RetryingElicitationProvider {
            attempts: AtomicUsize::new(0),
            identity: ProviderTurnExecutionIdentity {
                binding_id: binding.binding_id.clone(),
                activation_generation: binding.activation_generation,
                model_id: selection.model_id.clone(),
            },
            callback_started: tokio::sync::Notify::new(),
            callback_release: tokio::sync::Notify::new(),
            delivered: Mutex::new(Vec::new()),
        });
        manager
            .register_runtime(
                AgentRuntimeRouteKey {
                    agent_id,
                    transport_kind: TransportKind::Acp,
                    adapter_id,
                },
                provider.clone(),
            )
            .unwrap();
        let elicitation = ElicitationRequest {
            id: vibex_core::RequestId::new(),
            session_id: session.id.clone(),
            provider_request_id: Some("provider-request".to_string()),
            tool_call_id: None,
            message: "Continue?".to_string(),
            title: None,
            description: None,
            fields: Vec::new(),
            status: vibex_core::ElicitationRequestStatus::Pending,
            requested_at_ms: now,
        };
        manager
            .run_agent_turn(
                AgentTurnRequest {
                    session_id: session.id.clone(),
                    required_runtime: Some(selection),
                    text: "request elicitation".to_string(),
                    attachments: Vec::new(),
                    reasoning_effort: None,
                    correlation_id: None,
                },
                true,
                None,
                |_provider, _handle, _request| {
                    let elicitation = elicitation.clone();
                    async move {
                        Ok(ProviderTurnResult {
                            events: vec![ProviderEvent::provider(
                                TimelinePayload::ElicitationRequest(elicitation),
                            )],
                            binding_update: None,
                            completed: true,
                        })
                    }
                },
            )
            .await
            .unwrap();
        let conn = manager.open_migrated().unwrap();
        assert_eq!(
            SessionRepository::get(&conn, &session.id)
                .unwrap()
                .unwrap()
                .state,
            AgentSessionState::NeedsInput
        );
        assert_eq!(
            ElicitationRepository::pending_for_session(&conn, &session.id)
                .unwrap()
                .len(),
            1
        );
        drop(conn);
        let resolution = vibex_core::ElicitationResolution {
            request_id: elicitation.id.clone(),
            session_id: session.id.clone(),
            action: vibex_core::ElicitationResolutionAction::Decline,
            answers: Default::default(),
            responder_device_id: None,
            resolved_at_ms: now + 1,
        };
        let request = ResolveElicitationRequest {
            session_id: session.id.clone(),
            request_id: elicitation.id.clone(),
            resolution,
        };

        assert_eq!(
            manager
                .resolve_elicitation(request.clone())
                .await
                .unwrap_err()
                .code,
            "elicitation_callback_failed"
        );
        let conn = manager.open_migrated().unwrap();
        assert_eq!(
            ElicitationRepository::get_request(&conn, &elicitation.id)
                .unwrap()
                .unwrap()
                .status,
            vibex_core::ElicitationRequestStatus::Pending
        );
        drop(conn);

        let manager = Arc::new(manager);
        let winning_resolution = request.resolution.clone();
        let winner_manager = Arc::clone(&manager);
        let winner_request = request.clone();
        let winner =
            tokio::spawn(async move { winner_manager.resolve_elicitation(winner_request).await });
        provider.callback_started.notified().await;

        let mut competing_request = request;
        competing_request.resolution.action = vibex_core::ElicitationResolutionAction::Cancel;
        competing_request.resolution.resolved_at_ms += 1;
        let competing_manager = Arc::clone(&manager);
        let competing = tokio::spawn(async move {
            competing_manager
                .resolve_elicitation(competing_request)
                .await
        });
        tokio::task::yield_now().await;
        provider.callback_release.notify_one();

        winner.await.unwrap().unwrap();
        assert_eq!(
            competing.await.unwrap().unwrap_err().code,
            "elicitation_request_not_pending"
        );
        assert_eq!(provider.attempts.load(Ordering::SeqCst), 2);
        assert_eq!(
            provider.delivered.lock().unwrap().as_slice(),
            std::slice::from_ref(&winning_resolution)
        );
        let timeline = manager
            .fetch_timeline(FetchTimelineRequest {
                session_id: session.id.clone(),
                after_sequence: None,
                limit: 100,
            })
            .await
            .unwrap();
        assert_eq!(
            timeline
                .items
                .iter()
                .filter(|item| {
                    matches!(&item.payload, TimelinePayload::ElicitationResolution(_))
                })
                .count(),
            1
        );
        assert!(timeline.items.iter().any(|item| {
            matches!(
                &item.payload,
                TimelinePayload::ElicitationResolution(resolution)
                    if resolution == &winning_resolution
            )
        }));

        cleanup_db(&db_path);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn reasoning_effort_validation_is_forward_compatible_but_bounded() {
        assert_eq!(
            normalize_reasoning_effort(Some(" ultra ")).unwrap(),
            Some("ultra".to_string())
        );
        assert_eq!(
            normalize_reasoning_effort(Some("future-level_2")).unwrap(),
            Some("future-level_2".to_string())
        );
        let error = normalize_reasoning_effort(Some("high=value")).unwrap_err();
        assert_eq!(error.code, "reasoning_effort_invalid");
    }

    #[test]
    fn switch_usage_origin_treats_only_successful_restore_as_resumed() {
        assert_eq!(
            usage_counter_origin_for_switch_method(None),
            AgentUsageCounterOrigin::KnownZero
        );
        assert_eq!(
            usage_counter_origin_for_switch_method(Some(AgentSessionRestoreMethod::New)),
            AgentUsageCounterOrigin::KnownZero
        );
        for method in [
            AgentSessionRestoreMethod::Resume,
            AgentSessionRestoreMethod::Load,
        ] {
            assert_eq!(
                usage_counter_origin_for_switch_method(Some(method)),
                AgentUsageCounterOrigin::Resumed
            );
        }
    }

    #[tokio::test]
    async fn zero_baseline_origin_is_forwarded_without_claiming_before_dispatch() {
        let db_path = temp_db_path("usage-zero-baseline-request");
        let manager = AgentManager::new(&db_path).unwrap();
        let conn = manager.open_migrated().unwrap();
        ProviderProfileRepository::ensure_local_defaults(&conn).unwrap();
        let workspace_root = temp_workspace_path("usage-zero-baseline-request");
        fs::create_dir_all(&workspace_root).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let agent_id = AgentId::parse("opencode").unwrap();
        let session = insert_session(
            &conn,
            "usage zero baseline request",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id.clone(),
            AgentSessionState::Idle,
        );
        let provider_profile_id =
            ProviderProfileId::parse(ProviderKind::Acp.local_default_profile_id().to_string())
                .unwrap();
        let binding_id = RuntimeBindingId::new();
        let now = unix_timestamp_ms();
        RuntimeBindingRepository::insert(
            &conn,
            &RuntimeBinding {
                binding_id: binding_id.clone(),
                session_id: session.id.clone(),
                agent_id,
                transport_kind: TransportKind::Acp,
                provider_profile_id: provider_profile_id.clone(),
                adapter_id: AcpAdapterId::parse("opencode-acp").unwrap(),
                adapter_version: "1.0.0".to_string(),
                adapter_compatibility_identity: "opencode-acp@1".to_string(),
                native_session_id: Some("native-usage-origin-test".to_string()),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "usage-origin-test".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                profile_revision: 1,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: 3,
                binding_state: BindingState::Current,
                created_by_switch_id: None,
                created_at_ms: now,
                updated_at_ms: now,
            },
        )
        .unwrap();
        drop(conn);

        let identity = ProviderTurnExecutionIdentity {
            binding_id,
            activation_generation: 3,
            model_id: "usage-test-model".to_string(),
        };
        let provider_binding = ProviderBinding {
            session_id: session.id.clone(),
            provider_kind: ProviderKind::Acp,
            provider_profile_id,
            native: ProviderNativeBinding {
                native_session_id: Some("native-usage-origin-test".to_string()),
                ..ProviderNativeBinding::empty()
            },
            created_at_ms: now,
            updated_at_ms: now,
        };
        let provider = Arc::new(UsageOriginProvider {
            identity: identity.clone(),
        });
        let captured = Arc::new(Mutex::new(Vec::new()));
        let runner = {
            let captured = Arc::clone(&captured);
            move |_provider: Arc<dyn AgentProvider>,
                  _handle: ProviderSessionHandle,
                  request: ProviderTurnRequest| {
                let captured = Arc::clone(&captured);
                async move {
                    let execution_id = request
                        .usage_execution_context
                        .expect("prepared turn must carry usage execution context")
                        .usage_execution_id;
                    captured
                        .lock()
                        .unwrap()
                        .push((request.usage_counter_origin, execution_id));
                    Ok(ProviderTurnResult {
                        events: Vec::new(),
                        binding_update: None,
                        completed: true,
                    })
                }
            }
        };
        let first_execution_id = UsageExecutionId::new();
        let second_execution_id = UsageExecutionId::new();
        for usage_execution_id in [&first_execution_id, &second_execution_id] {
            let outcome = manager
                .run_provider_turn_attempt(
                    &session,
                    provider.clone(),
                    provider_binding.clone(),
                    "test usage origin".to_string(),
                    &[],
                    None,
                    usage_execution_id.clone(),
                    AgentUsageCounterOrigin::KnownZero,
                    None,
                    Some(identity.clone()),
                    0,
                    &runner,
                )
                .await;
            assert!(matches!(outcome, ProviderTurnAttemptOutcome::Success(_)));
        }
        assert_eq!(
            *captured.lock().unwrap(),
            vec![
                (AgentUsageCounterOrigin::KnownZero, first_execution_id),
                (AgentUsageCounterOrigin::KnownZero, second_execution_id),
            ]
        );
        let claim_probe = UsageExecutionId::new();
        let conn = manager.open_migrated().unwrap();
        assert!(
            RuntimeBindingRepository::claim_usage_zero_baseline(
                &conn,
                &identity.binding_id,
                identity.activation_generation,
                &claim_probe,
            )
            .unwrap(),
            "pre-dispatch turn preparation must leave the one-shot claim available"
        );
        drop(conn);

        cleanup_db(&db_path);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[test]
    fn fork_projection_keeps_message_history_and_drops_session_scoped_metadata() {
        let session_id = VibexSessionId::new();
        let item = |sequence: i64, source: TimelineSource, payload: TimelinePayload| TimelineItem {
            id: vibex_core::TimelineItemId::parse(format!("timeline_fork_{sequence}")).unwrap(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: 100 + sequence,
            source,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: Some(format!("provider-{sequence}")),
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        };
        let request_id = vibex_core::RequestId::new();
        let items = vec![
            item(
                1,
                TimelineSource::System,
                TimelinePayload::SystemNotice(SystemNoticePayload {
                    level: SystemNoticeLevel::Info,
                    message: "ready".to_string(),
                }),
            ),
            item(
                2,
                TimelineSource::User,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "question".to_string(),
                    attachments: Vec::new(),
                }),
            ),
            item(
                3,
                TimelineSource::Provider,
                TimelinePayload::PermissionResolution(vibex_core::PermissionResolution {
                    request_id,
                    session_id: session_id.clone(),
                    response: vibex_core::PermissionResponseKind::Approve,
                    responder_device_id: None,
                    provider_resolution_id: None,
                    note: None,
                    resolved_at_ms: 103,
                }),
            ),
            item(
                4,
                TimelineSource::Agent,
                TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: "answer".to_string(),
                    is_final: true,
                }),
            ),
        ];

        let projected = fork_timeline_appends(&items);
        assert_eq!(projected.len(), 2);
        assert!(matches!(
            projected[0].payload,
            TimelinePayload::UserMessage(_)
        ));
        assert!(matches!(
            projected[1].payload,
            TimelinePayload::AgentMessage(_)
        ));
        assert_eq!(projected[0].timestamp_ms, Some(102));
        assert_eq!(projected[1].timestamp_ms, Some(104));
        assert!(projected.iter().all(|item| item.correlation_id.is_none()
            && item.provider_correlation_id.is_none()
            && item.execution_attribution.is_none()));
    }

    #[test]
    fn final_turn_result_skips_only_exact_streamed_provider_events() {
        let session_id = VibexSessionId::new();
        let correlation = "provider-file-change".to_string();
        let payload = TimelinePayload::FileOperation(vibex_core::FileOperationPayload {
            operation: vibex_core::FileOperationKind::Edit,
            path: "apps/desktop/src/app.rs".into(),
            summary: "Edited app.rs".into(),
            old_text: Some("before".repeat(1024)),
            new_text: Some("after".repeat(1024)),
            raw_extension: None,
        });
        let streamed = TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id,
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: Some(correlation.clone()),
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload: payload.clone(),
        };
        let indices = HashMap::from([(correlation.clone(), 0)]);
        let duplicate = ProviderEvent {
            source: TimelineSource::Agent,
            payload,
            provider_correlation_id: Some(correlation.clone()),
            redaction_state: TimelineRedactionState::None,
        };
        assert!(provider_event_was_streamed(
            &duplicate,
            std::slice::from_ref(&streamed),
            &indices,
        ));

        let changed = ProviderEvent {
            payload: TimelinePayload::FileOperation(vibex_core::FileOperationPayload {
                operation: vibex_core::FileOperationKind::Edit,
                path: "apps/desktop/src/app.rs".into(),
                summary: "Edited app.rs again".into(),
                old_text: None,
                new_text: None,
                raw_extension: None,
            }),
            ..duplicate
        };
        assert!(!provider_event_was_streamed(
            &changed,
            &[streamed],
            &indices,
        ));
    }

    #[test]
    fn streamed_event_index_keeps_state_changes_but_drops_repeated_snapshots() {
        let session_id = VibexSessionId::new();
        let correlation = "provider-command".to_string();
        let make_event = |status| ProviderEvent {
            source: TimelineSource::Agent,
            payload: TimelinePayload::Command(vibex_core::CommandPayload {
                command: "cargo check".into(),
                cwd: Some("/workspace".into()),
                status,
                exit_code: None,
                output_summary: None,
                raw_extension: None,
            }),
            provider_correlation_id: Some(correlation.clone()),
            redaction_state: TimelineRedactionState::None,
        };
        let mut items = Vec::new();
        let mut indices = HashMap::new();
        let started = make_event(vibex_core::CommandStatus::Started);
        assert!(!provider_event_was_streamed(&started, &items, &indices));
        let started_item = TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence: 1,
            timestamp_ms: 1,
            source: started.source,
            kind: started.payload.kind(),
            correlation_id: None,
            provider_correlation_id: started.provider_correlation_id.clone(),
            redaction_state: started.redaction_state,
            execution_attribution: None,
            payload: started.payload.clone(),
        };
        let index = push_or_replace_timeline_item(&mut items, started_item);
        indices.insert(correlation.clone(), index);
        assert!(provider_event_was_streamed(&started, &items, &indices));

        let completed = make_event(vibex_core::CommandStatus::Completed);
        assert!(!provider_event_was_streamed(&completed, &items, &indices));
        let completed_item = TimelineItem {
            id: vibex_core::TimelineItemId::new(),
            session_id,
            sequence: 2,
            timestamp_ms: 2,
            source: completed.source,
            kind: completed.payload.kind(),
            correlation_id: None,
            provider_correlation_id: completed.provider_correlation_id.clone(),
            redaction_state: completed.redaction_state,
            execution_attribution: None,
            payload: completed.payload.clone(),
        };
        let index = push_or_replace_timeline_item(&mut items, completed_item);
        indices.insert(correlation, index);
        assert!(provider_event_was_streamed(&completed, &items, &indices));
        assert_eq!(items.len(), 2);
    }

    #[test]
    fn runtime_registration_is_acp_only_and_unique_per_agent() {
        let db_path = temp_db_path("runtime-registration");
        let mut manager = AgentManager::new(&db_path).unwrap();
        let route = runtime_route("claude", "claude-agent-acp");

        let native_error = manager
            .register_runtime(
                route.clone(),
                Arc::new(TestProvider {
                    kind: ProviderKind::Claude,
                }),
            )
            .unwrap_err();
        assert_eq!(native_error.code, "runtime_route_transport_invalid");

        manager
            .register_runtime(
                route.clone(),
                Arc::new(TestProvider {
                    kind: ProviderKind::Acp,
                }),
            )
            .unwrap();
        assert_eq!(
            manager
                .route_for_agent(&AgentId::parse("claude").unwrap())
                .unwrap(),
            route
        );

        let duplicate_error = manager
            .register_runtime(
                runtime_route("claude", "another-claude-adapter"),
                Arc::new(TestProvider {
                    kind: ProviderKind::Acp,
                }),
            )
            .unwrap_err();
        assert_eq!(duplicate_error.code, "runtime_agent_already_registered");

        cleanup_db(&db_path);
    }

    #[test]
    fn startup_recovery_preserves_durable_initial_switch_intent() {
        let db_path = temp_db_path("initial-switch-recovery");
        let workspace_root = temp_workspace_path("initial-switch-recovery");
        fs::create_dir_all(&workspace_root).unwrap();

        let mut conn = open_database(&db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let agent_id = AgentId::parse("claude").unwrap();
        let profile_id = ProviderProfileId::parse("provider_acp_claude").unwrap();
        let initial_session = insert_session(
            &conn,
            "initializing with durable intent",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id.clone(),
            AgentSessionState::Initializing,
        );
        let stale_running_session = insert_session(
            &conn,
            "stale running turn",
            &project.id,
            &workspace.id,
            &workspace.root_path,
            agent_id.clone(),
            AgentSessionState::Running,
        );
        let desired = SessionRuntimeSelection {
            agent_id,
            provider_profile_id: profile_id,
            model_id: "claude-test-model".to_string(),
            reasoning_effort: None,
            mode_id: None,
            config_values: Default::default(),
        };
        let switch = AgentSessionRuntimeRepository::enqueue_initial_runtime_switch(
            &mut conn,
            RuntimeSwitchId::new(),
            &DesiredRuntimeSwitchEnqueueRequest {
                session_id: initial_session.id.clone(),
                idempotency_key: format!("session-init:{}", initial_session.id.as_str()),
                expected_revision: 0,
                expected_selection_revision: 0,
                target_binding_id: RuntimeBindingId::new(),
                target_adapter_id: AcpAdapterId::parse("claude-agent-acp").unwrap(),
                desired: desired.clone(),
                requested_policy: RuntimeSwitchPolicy::Automatic,
                active_work_policy: RuntimeSwitchActiveWorkPolicy::default(),
                requested_session_config: RuntimeSwitchCoordinator::encode_requested_config(
                    &desired, None,
                )
                .unwrap(),
            },
        )
        .unwrap();
        drop(conn);

        let manager = AgentManager::new(&db_path).unwrap();
        let conn = manager.open_migrated().unwrap();
        let recovered_initial = SessionRepository::get(&conn, &initial_session.id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_initial.state, AgentSessionState::Initializing);
        let runtime_state =
            AgentSessionRuntimeRepository::get_runtime_state(&conn, &initial_session.id)
                .unwrap()
                .unwrap();
        assert_eq!(runtime_state.pending_switch_id, None);
        assert_eq!(runtime_state.desired_runtime_selection, Some(desired));
        let durable_switch = RuntimeSwitchRepository::get(&conn, &switch.switch_id)
            .unwrap()
            .unwrap();
        assert_eq!(
            durable_switch.status,
            vibex_core::RuntimeSwitchStatus::Requested
        );

        let recovered_running = SessionRepository::get(&conn, &stale_running_session.id)
            .unwrap()
            .unwrap();
        assert_eq!(recovered_running.state, AgentSessionState::Error);

        drop(conn);
        cleanup_db(&db_path);
        let _ = fs::remove_dir_all(workspace_root);
    }

    #[tokio::test]
    async fn ordinary_send_without_durable_coordinator_fails_closed() {
        let db_path = temp_db_path("missing-message-coordinator");
        let manager = AgentManager::new(&db_path).unwrap();
        let error = manager
            .send_message(SendAgentMessageRequest {
                session_id: VibexSessionId::new(),
                message_idempotency_key: "missing-coordinator".to_string(),
                desired_runtime: SessionRuntimeSelection {
                    agent_id: AgentId::parse("claude").unwrap(),
                    provider_profile_id: ProviderProfileId::parse("provider_acp_claude").unwrap(),
                    model_id: "claude-test-model".to_string(),
                    reasoning_effort: None,
                    mode_id: None,
                    config_values: Default::default(),
                },
                text: "hello".to_string(),
                attachments: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(error.code, "message_submission_coordinator_unavailable");
        cleanup_db(&db_path);
    }

    fn runtime_route(agent_id: &str, adapter_id: &str) -> AgentRuntimeRouteKey {
        AgentRuntimeRouteKey {
            agent_id: AgentId::parse(agent_id).unwrap(),
            transport_kind: TransportKind::Acp,
            adapter_id: AcpAdapterId::parse(adapter_id).unwrap(),
        }
    }

    fn insert_session(
        conn: &DbConnection,
        title: &str,
        project_id: &ProjectId,
        workspace_id: &WorkspaceId,
        workspace_root: &str,
        agent_id: AgentId,
        state: AgentSessionState,
    ) -> AgentSession {
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: title.to_string(),
            project_id: project_id.clone(),
            workspace_id: workspace_id.clone(),
            workspace_root: workspace_root.to_string(),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            agent_id,
            state,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(conn, &session).unwrap();
        session
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-{label}-{}.db",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn temp_workspace_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-{label}-{}",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: &Path) {
        let _ = fs::remove_file(path);
        let _ = fs::remove_file(path.with_extension("db-wal"));
        let _ = fs::remove_file(path.with_extension("db-shm"));
    }
}

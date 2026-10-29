use serde::{Deserialize, Serialize};
use vibex_core::{
    AcpProviderCatalogListResponse, AcpProviderConfig, AcpProviderProfileUpdateRequest,
    AgentCatalogListResponse, AgentId, AgentListRequest, AgentListResponse,
    AgentManagedInstallState, AgentModelProviderBinding, AgentModelProviderBindingCreateRequest,
    AgentModelProviderBindingListRequest, AgentModelProviderBindingUpdateRequest,
    AgentModelProviderDisplayOrderListRequest, AgentModelProviderDisplayOrderListResponse,
    AgentModelProviderDisplayOrderSetRequest, AgentModelProviderDisplayOrderSetResponse,
    AgentModelProviderDraftFetchModelsRequest, AgentModelProviderDraftFetchModelsResponse,
    AgentModelProviderProfileCreateRequest, AgentModelProviderProfileDeleteRequest,
    AgentModelProviderProfileDetailRequest, AgentModelProviderProfileFetchModelsRequest,
    AgentModelProviderProfileFetchModelsResponse, AgentModelProviderProfileSecretValueRequest,
    AgentModelProviderProfileSecretValueResponse,
    AgentModelProviderProfileSecretValueUpdateRequest, AgentModelProviderProfileTestRequest,
    AgentModelProviderProfileTestResult, AgentModelProviderProfileUpdateRequest,
    AgentProviderProjectionCapability, AgentProviderProjectionCapabilityRequest,
    AgentProviderProjectionPreview, AgentProviderProjectionPreviewRequest,
    AgentRefreshSnapshotRequest, AgentRefreshSnapshotResponse, AgentRuntimeProbeCancelRequest,
    AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord, AgentRuntimeProbeStartRequest,
    AgentRuntimeProfile, AgentRuntimeProfileCreateRequest, AgentRuntimeProfileUpdateRequest,
    AgentSnapshotEntry, AgentUpdateConfigRequest, AutomationGraph, AutomationGraphCreateRequest,
    AutomationGraphDefinitionUpdateRequest, AutomationGraphId, AutomationGraphListRequest,
    AutomationGraphStatus, AutomationGraphUpdateRequest, AutomationRun, AutomationRunCancelRequest,
    AutomationRunListRequest, AutomationRunResumeRequest, AutomationRunStartRequest,
    AutomationRunStep, AutomationRunStepListRequest, BackupCreateOutcome, BackupCreatePayload,
    BackupInspectOutcome, BackupInspectPayload, BackupRestoreOutcome, BackupRestorePayload,
    CustomAgentCreateRequest, CustomAgentDeleteRequest, DiagnosticExportOutcome,
    DiagnosticExportPayload, GatewayAgentUpdateRequest, GatewayCapabilitySuggestion,
    GatewayRequestSummary, GatewaySettingsUpdateRequest, GatewaySnapshot, Hook, HookCreateRequest,
    HookDeleteRequest, HookInstallPreview, HookInstallPreviewRequest, HookUpdateRequest,
    ManagementSnapshotPayload, McpMarketInstallRequest, McpMarketInstallResult,
    McpMarketSearchRequest, McpMarketSearchResponse, McpServer, McpServerAgentMatrix,
    McpServerAgentMatrixListRequest, McpServerCreateRequest, McpServerDeleteRequest,
    McpServerDiscoverRequest, McpServerDiscoveryResponse, McpServerImportRequest,
    McpServerImportResult, McpServerSetAgentMatrixRequest, McpServerUpdateRequest,
    McpServerValidateRequest, McpServerValidationResult, ModelCapabilitySuggestionRequest,
    ModelProviderCredentialSecretRequest, ModelProviderCredentialSecretResponse,
    ModelProviderProfile, ModelProviderProfileCreateRequest, ModelProviderProfileUpdateRequest,
    Prompt, PromptCreateRequest, PromptDeleteRequest, PromptUpdateRequest, PromptUsage,
    PromptUsageRecordRequest, PromptValidateRequest, PromptValidationResult,
    ProviderCapabilitySummary, ProviderCredentialSecretMutationRequest, ProviderHealthSummary,
    ProviderNativeExportApplyRequest, ProviderNativeExportApplyResult,
    ProviderNativeExportListRequest, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest, ProviderProfile,
    ProviderProfileId, ProviderProfileSummary, ProviderRunCapabilityProbesRequest,
    ProviderRunCapabilityProbesResult, ProviderRunHealthProbesRequest,
    ProviderRunHealthProbesResult, ProviderUsageListRequest, ProviderUsageSummary, RcImportOutcome,
    RcImportPayload, RelayPeerId, RelayRoomId, RemoteProviderManagementSnapshot,
    ScheduledTaskAttentionListRequest, ScheduledTaskAttentionSummary,
    ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord, ScheduledTaskCreateRequest,
    ScheduledTaskId, ScheduledTaskListRequest, ScheduledTaskRun, ScheduledTaskRunListRequest,
    ScheduledTaskUpdateRequest, Skill, SkillAgentMatrix, SkillAgentMatrixListRequest,
    SkillCreateRequest, SkillDeleteRequest, SkillDiscoverRequest, SkillDiscoveryResponse,
    SkillImportRequest, SkillImportResult, SkillMarketDocument, SkillMarketDocumentRequest,
    SkillMarketInstallRequest, SkillMarketInstallResult, SkillMarketSearchRequest,
    SkillMarketSearchResponse, SkillSetAgentMatrixRequest, SkillUpdateRequest,
    SkillValidateRequest, SkillValidationResult,
};

use crate::{BackendBound, BackendError, BackendFuture, MutationRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RelayConnectionState {
    Disabled,
    Disconnected,
    Connecting,
    Connected,
    Retrying,
    Degraded,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusSummary {
    pub state: RelayConnectionState,
    pub room_id: RelayRoomId,
    pub pc_peer_id: RelayPeerId,
    pub pc_public_key: String,
    pub reconnect_attempt: u32,
    pub next_retry_at_ms: Option<i64>,
    pub last_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementProfileSelectionRequest {
    pub agent_id: AgentId,
    pub provider_profile_id: ProviderProfileId,
    /// Scope the default applies to. `None` means the authority's global
    /// scope; a paired client passes the workspace it is managing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<vibex_core::ProviderProfileDefaultScope>,
}

pub trait ManagementBackend: BackendBound {
    fn list_agents(&self, request: AgentListRequest) -> BackendFuture<'_, AgentListResponse>;

    /// Refreshes the authority's Agent snapshot for one Agent.
    fn refresh_agent_snapshot(
        &self,
        request: AgentRefreshSnapshotRequest,
    ) -> BackendFuture<'_, AgentRefreshSnapshotResponse>;

    fn install_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState>;

    /// Installs the Adapter version Vibex verified for an Agent, which may be
    /// older than what is currently installed.
    fn rollback_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState>;

    fn check_managed_agent_update(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState>;

    fn uninstall_managed_agent(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentManagedInstallState>;

    /// Drops the persisted authentication catalog for an Agent.
    fn delete_agent_auth_catalog(&self, request: MutationRequest<AgentId>)
    -> BackendFuture<'_, ()>;

    fn update_agent_config(
        &self,
        request: MutationRequest<AgentUpdateConfigRequest>,
    ) -> BackendFuture<'_, AgentSnapshotEntry>;

    fn create_custom_agent(
        &self,
        request: MutationRequest<CustomAgentCreateRequest>,
    ) -> BackendFuture<'_, AgentSnapshotEntry>;

    fn delete_custom_agent(
        &self,
        request: MutationRequest<CustomAgentDeleteRequest>,
    ) -> BackendFuture<'_, ()>;

    fn list_profiles(&self) -> BackendFuture<'_, Vec<ProviderProfileSummary>>;

    fn select_profile(
        &self,
        request: MutationRequest<ManagementProfileSelectionRequest>,
    ) -> BackendFuture<'_, ProviderProfileSummary>;

    fn list_model_provider_profiles(&self) -> BackendFuture<'_, Vec<ModelProviderProfile>>;

    /// Every reusable route.
    fn list_model_routes(&self) -> BackendFuture<'_, Vec<vibex_core::ModelRoute>>;

    /// Every Agent's route bindings, for a page that renders them all.
    fn list_all_agent_model_route_bindings(
        &self,
    ) -> BackendFuture<'_, Vec<vibex_core::AgentModelRouteBinding>>;

    fn create_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRoute>,
    ) -> BackendFuture<'_, vibex_core::ModelRoute>;

    fn update_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRouteUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ModelRoute>;

    fn delete_model_route(
        &self,
        request: MutationRequest<vibex_core::ModelRouteDeleteRequest>,
    ) -> BackendFuture<'_, bool>;

    fn bind_agent_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteBindRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelRouteBinding>;

    fn unbind_agent_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteUnbindRequest>,
    ) -> BackendFuture<'_, bool>;

    fn set_agent_default_model_route(
        &self,
        request: MutationRequest<vibex_core::AgentModelRouteDefaultRequest>,
    ) -> BackendFuture<'_, ()>;

    /// The Agent's resolved default model source for one scope.
    fn get_agent_model_source_default(
        &self,
        request: vibex_core::AgentModelSourceDefaultRequest,
    ) -> BackendFuture<'_, vibex_core::AgentModelSourceDefault>;

    /// Sets the Agent's default model source, clearing the conflicting legacy
    /// default in the same transaction.
    fn set_agent_model_source_default(
        &self,
        request: MutationRequest<vibex_core::SetAgentModelSourceDefaultRequest>,
    ) -> BackendFuture<'_, vibex_core::AgentModelSourceDefault>;

    /// Removes the explicit unified default; the legacy fallbacks remain.
    fn clear_agent_model_source_default(
        &self,
        request: MutationRequest<vibex_core::ClearAgentModelSourceDefaultRequest>,
    ) -> BackendFuture<'_, bool>;

    /// Reports the advisory models.dev catalogue's freshness.
    fn model_catalog_status(&self) -> BackendFuture<'_, vibex_core::ModelCatalogStatus>;

    /// Refreshes the advisory models.dev catalogue, returning its new status.
    fn refresh_model_catalog(&self) -> BackendFuture<'_, vibex_core::ModelCatalogStatus>;

    /// The catalogue's suggestion for one model, when a catalogue is cached.
    ///
    /// This never blocks on the network: a missing or stale catalogue answers
    /// `None`, and the editor simply shows no suggestions.
    fn model_capability_suggestion(
        &self,
        request: ModelCapabilitySuggestionRequest,
    ) -> BackendFuture<'_, Option<GatewayCapabilitySuggestion>>;

    /// The catalogue's vendor-independent id for several models at once.
    ///
    /// A same-model suggestion compares every configured model across
    /// Providers, so this answers the whole list in one call and in the
    /// request's order. A model the catalogue does not know comes back with
    /// `canonical_model_id: None`.
    fn model_canonical_ids(
        &self,
        request: vibex_core::ModelCanonicalIdsRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::ModelCanonicalId>>;

    /// Reads the whole gateway surface: its settings, the listener state, and
    /// the Providers and Keys its enabled route groups name.
    ///
    /// There is no Agent list and no upstream list of its own: an Agent reaches
    /// the gateway by binding a route group, and the Providers it may call are
    /// that group's members.
    fn model_gateway_snapshot(&self) -> BackendFuture<'_, GatewaySnapshot>;

    /// Writes the gateway's Key-level policy.
    ///
    /// The member strategy and conversation pin belong to a route group, so
    /// neither is part of this write.
    fn update_model_gateway_settings(
        &self,
        request: MutationRequest<GatewaySettingsUpdateRequest>,
    ) -> BackendFuture<'_, GatewaySnapshot>;

    /// Adds one API key to a shared Provider.
    fn add_model_provider_credential(
        &self,
        request: MutationRequest<vibex_core::AddModelProviderCredentialRequest>,
    ) -> BackendFuture<'_, ModelProviderProfile>;

    /// Removes one API key from a shared Provider.
    fn remove_model_provider_credential(
        &self,
        request: MutationRequest<vibex_core::RemoveModelProviderCredentialRequest>,
    ) -> BackendFuture<'_, ModelProviderProfile>;

    /// Turns the gateway on or off.
    ///
    /// Off stops the listener, so every bound route group becomes unusable and
    /// each Agent falls back to its direct Provider; on re-binds the persisted
    /// port and restores the bindings.
    fn set_model_gateway_enabled(&self, enabled: bool) -> BackendFuture<'_, GatewaySnapshot>;

    /// The retired per-Agent gateway switch.
    ///
    /// An Agent reaches the gateway by binding a route group, so this entry
    /// point survives only to answer an older client with a readable refusal
    /// instead of an unknown-variant decode failure.
    fn set_model_gateway_agent(
        &self,
        request: MutationRequest<GatewayAgentUpdateRequest>,
    ) -> BackendFuture<'_, GatewaySnapshot>;

    /// Answers a route's failover prompt.
    ///
    /// The request that raised the prompt is parked until this arrives or the
    /// prompt's own countdown expires, so this is what turns a click in the
    /// session into a decision the gateway can act on.
    fn answer_model_gateway_failover(
        &self,
        prompt_id: vibex_core::RequestId,
        answer: vibex_core::GatewayFailoverAnswer,
    ) -> BackendFuture<'_, bool>;

    /// Lists the metadata-only ledger for the diagnostics view.
    fn model_gateway_requests(&self) -> BackendFuture<'_, Vec<GatewayRequestSummary>>;

    /// The requests the gateway is serving right now.
    fn model_gateway_inflight(&self) -> BackendFuture<'_, Vec<vibex_core::GatewayInflight>>;

    /// Deletes a shared Provider and the bindings that point at it.
    fn delete_model_provider_profile(
        &self,
        request: MutationRequest<vibex_core::ModelProviderProfileId>,
    ) -> BackendFuture<'_, ()>;

    /// Clears the gateway ledger.
    fn clear_model_gateway_requests(&self) -> BackendFuture<'_, usize>;

    fn create_model_provider_profile(
        &self,
        request: MutationRequest<ModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, ModelProviderProfile>;

    fn update_model_provider_profile(
        &self,
        request: MutationRequest<ModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, ModelProviderProfile>;

    fn list_agent_runtime_profiles(
        &self,
        agent_id: AgentId,
    ) -> BackendFuture<'_, Vec<AgentRuntimeProfile>>;

    fn create_agent_runtime_profile(
        &self,
        request: MutationRequest<AgentRuntimeProfileCreateRequest>,
    ) -> BackendFuture<'_, AgentRuntimeProfile>;

    fn update_agent_runtime_profile(
        &self,
        request: MutationRequest<AgentRuntimeProfileUpdateRequest>,
    ) -> BackendFuture<'_, AgentRuntimeProfile>;

    fn list_agent_model_provider_bindings(
        &self,
        request: AgentModelProviderBindingListRequest,
    ) -> BackendFuture<'_, Vec<AgentModelProviderBinding>>;

    fn create_agent_model_provider_binding(
        &self,
        request: MutationRequest<AgentModelProviderBindingCreateRequest>,
    ) -> BackendFuture<'_, AgentModelProviderBinding>;

    fn update_agent_model_provider_binding(
        &self,
        request: MutationRequest<AgentModelProviderBindingUpdateRequest>,
    ) -> BackendFuture<'_, AgentModelProviderBinding>;

    fn agent_provider_projection_capability(
        &self,
        request: AgentProviderProjectionCapabilityRequest,
    ) -> BackendFuture<'_, AgentProviderProjectionCapability>;

    fn preview_agent_provider_projection(
        &self,
        request: AgentProviderProjectionPreviewRequest,
    ) -> BackendFuture<'_, AgentProviderProjectionPreview>;

    fn start_agent_runtime_probe(
        &self,
        request: MutationRequest<AgentRuntimeProbeStartRequest>,
    ) -> BackendFuture<'_, AgentRuntimeProbeRecord>;

    fn get_agent_runtime_probe(
        &self,
        probe_id: vibex_core::AgentRuntimeProbeId,
    ) -> BackendFuture<'_, Option<AgentRuntimeProbeRecord>>;

    fn list_agent_runtime_probes(
        &self,
        request: AgentRuntimeProbeListRequest,
    ) -> BackendFuture<'_, Vec<AgentRuntimeProbeRecord>>;

    fn cancel_agent_runtime_probe(
        &self,
        request: MutationRequest<AgentRuntimeProbeCancelRequest>,
    ) -> BackendFuture<'_, AgentRuntimeProbeRecord>;

    fn mutate_provider_credential_secret(
        &self,
        request: MutationRequest<ProviderCredentialSecretMutationRequest>,
    ) -> BackendFuture<'_, ModelProviderProfile>;

    /// Reads one shared Provider credential's stored Secret for an explicit
    /// reveal. Ordinary reads carry only the masked hint.
    fn get_model_provider_credential_secret(
        &self,
        request: ModelProviderCredentialSecretRequest,
    ) -> BackendFuture<'_, ModelProviderCredentialSecretResponse>;

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>>;

    fn run_health_probes(
        &self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult>;

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary>;

    /// Deletes a per-Agent model provider profile.
    fn mutate_agent_model_provider_profile_secret(
        &self,
        request: MutationRequest<AgentModelProviderProfileSecretValueUpdateRequest>,
    ) -> BackendFuture<'_, AgentModelProviderProfileSecretValueResponse>;

    /// Reads the stored Secret back so the editor can reveal it on request.
    fn get_agent_model_provider_profile_secret_value(
        &self,
        request: AgentModelProviderProfileSecretValueRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileSecretValueResponse>;

    /// One profile as the authority holds it.
    ///
    /// The list answers with the summary published to any client, which leaves
    /// out the endpoint and the request options. An editor that writes a profile
    /// back needs them: a save that cannot read a field would quietly clear it.
    /// A runtime that does not serve this answers with a refusal, and the client
    /// falls back to editing what the summary carries.
    fn agent_model_provider_profile_detail(
        &self,
        request: MutationRequest<AgentModelProviderProfileDetailRequest>,
    ) -> BackendFuture<'_, ProviderProfile>;

    fn create_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileCreateRequest>,
    ) -> BackendFuture<'_, ProviderProfile>;

    fn update_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile>;

    fn delete_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileDeleteRequest>,
    ) -> BackendFuture<'_, ()>;

    /// Gives one Agent its own Profile for a Provider another Agent holds.
    fn bind_agent_model_provider(
        &self,
        request: MutationRequest<vibex_core::AgentModelProviderBindRequest>,
    ) -> BackendFuture<'_, vibex_core::ProviderProfile>;

    fn agent_model_provider_display_order(
        &self,
        request: AgentModelProviderDisplayOrderListRequest,
    ) -> BackendFuture<'_, AgentModelProviderDisplayOrderListResponse>;

    fn set_agent_model_provider_display_order(
        &self,
        request: MutationRequest<AgentModelProviderDisplayOrderSetRequest>,
    ) -> BackendFuture<'_, AgentModelProviderDisplayOrderSetResponse>;

    /// Probes a provider profile's live endpoint without persisting anything.
    fn test_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileTestRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileTestResult>;

    /// Fetches the model catalogue a provider profile exposes.
    fn fetch_agent_model_provider_profile_models(
        &self,
        request: AgentModelProviderProfileFetchModelsRequest,
    ) -> BackendFuture<'_, AgentModelProviderProfileFetchModelsResponse>;

    /// Probes one shared Provider Key by its credential id.
    fn test_model_provider_credential(
        &self,
        request: vibex_core::ModelProviderKeyTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult>;

    /// Probes a per-Key proxy's route to the Provider endpoint.
    fn test_model_provider_proxy(
        &self,
        request: vibex_core::ModelProviderProxyTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult>;

    /// Probes every saved Provider endpoint, ordered fastest first.
    fn probe_model_provider_endpoints(
        &self,
        request: vibex_core::ModelProviderEndpointProbeRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderEndpointProbeResponse>;

    /// Fetches the model catalogue a saved shared Provider exposes.
    fn fetch_model_provider_profile_models(
        &self,
        request: vibex_core::ModelProviderFetchModelsRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderFetchModelsResponse>;

    /// Probes a saved shared Provider's address and credential.
    fn test_model_provider_profile(
        &self,
        request: vibex_core::ModelProviderTestRequest,
    ) -> BackendFuture<'_, vibex_core::ModelProviderTestResult>;

    /// Fetches the model catalogue an endpoint the editor has not saved yet
    /// exposes. The request carries the typed address and key; nothing is
    /// persisted and no Profile row is required.
    fn fetch_agent_model_provider_draft_models(
        &self,
        request: AgentModelProviderDraftFetchModelsRequest,
    ) -> BackendFuture<'_, AgentModelProviderDraftFetchModelsResponse>;

    fn capability_summaries(&self) -> BackendFuture<'_, Vec<ProviderCapabilitySummary>>;

    fn usage_summaries(
        &self,
        request: ProviderUsageListRequest,
    ) -> BackendFuture<'_, Vec<ProviderUsageSummary>>;

    fn management_snapshot(
        &self,
        request: ManagementSnapshotPayload,
    ) -> BackendFuture<'_, RemoteProviderManagementSnapshot>;

    /// Writes a redacted diagnostic bundle on the authority.
    fn export_diagnostics(
        &self,
        request: MutationRequest<DiagnosticExportPayload>,
    ) -> BackendFuture<'_, DiagnosticExportOutcome>;

    /// Creates a database backup on the authority.
    fn backup_create(
        &self,
        request: MutationRequest<BackupCreatePayload>,
    ) -> BackendFuture<'_, BackupCreateOutcome>;

    /// Inspects a backup directory on the authority.
    fn backup_inspect(
        &self,
        request: BackupInspectPayload,
    ) -> BackendFuture<'_, BackupInspectOutcome>;

    /// Restores a backup into a new database on the authority.
    fn backup_restore(
        &self,
        request: MutationRequest<BackupRestorePayload>,
    ) -> BackendFuture<'_, BackupRestoreOutcome>;

    /// Stages an import of the RC channel home into the active home.
    ///
    /// Only the local authority can answer this: the import moves files inside
    /// the authority's own data directory and must run before the runtime that
    /// owns them opens its database. A client that reaches the authority over
    /// the wire inherits the unsupported default below.
    fn rc_import(
        &self,
        request: MutationRequest<RcImportPayload>,
    ) -> BackendFuture<'_, RcImportOutcome> {
        let _ = request;
        Box::pin(async {
            Err(BackendError::unsupported(
                "management",
                "RC data can only be imported by the local authority",
            ))
        })
    }

    fn run_capability_probes(
        &self,
        request: MutationRequest<ProviderRunCapabilityProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunCapabilityProbesResult>;

    fn mcp_servers(&self) -> BackendFuture<'_, Vec<McpServer>>;

    fn create_mcp_server(
        &self,
        request: MutationRequest<McpServerCreateRequest>,
    ) -> BackendFuture<'_, McpServer>;

    fn update_mcp_server(
        &self,
        request: MutationRequest<McpServerUpdateRequest>,
    ) -> BackendFuture<'_, McpServer>;

    fn delete_mcp_server(
        &self,
        request: MutationRequest<McpServerDeleteRequest>,
    ) -> BackendFuture<'_, ()>;

    fn set_mcp_server_agent_matrix(
        &self,
        request: MutationRequest<McpServerSetAgentMatrixRequest>,
    ) -> BackendFuture<'_, McpServer>;

    fn mcp_server_agent_matrix(
        &self,
        request: McpServerAgentMatrixListRequest,
    ) -> BackendFuture<'_, Vec<McpServerAgentMatrix>>;

    fn discover_mcp_sources(
        &self,
        request: McpServerDiscoverRequest,
    ) -> BackendFuture<'_, McpServerDiscoveryResponse>;

    fn import_mcp_servers(
        &self,
        request: MutationRequest<McpServerImportRequest>,
    ) -> BackendFuture<'_, McpServerImportResult>;

    fn scheduled_tasks(
        &self,
        request: ScheduledTaskListRequest,
    ) -> BackendFuture<'_, Vec<vibex_core::ScheduledTask>>;

    fn create_scheduled_task(
        &self,
        request: MutationRequest<ScheduledTaskCreateRequest>,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask>;

    fn update_scheduled_task(
        &self,
        request: MutationRequest<ScheduledTaskUpdateRequest>,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask>;

    fn set_scheduled_task_status(
        &self,
        task_id: ScheduledTaskId,
        paused: bool,
    ) -> BackendFuture<'_, vibex_core::ScheduledTask>;

    fn delete_scheduled_task(&self, task_id: ScheduledTaskId) -> BackendFuture<'_, ()>;

    fn scheduled_task_runs(
        &self,
        request: ScheduledTaskRunListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskRun>>;

    fn scheduled_task_attention(
        &self,
        request: ScheduledTaskAttentionListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskAttentionSummary>>;

    fn scheduled_task_audit(
        &self,
        request: ScheduledTaskAuditListRequest,
    ) -> BackendFuture<'_, Vec<ScheduledTaskAuditRecord>>;

    fn claim_due_scheduled_task(
        &self,
        task_id: ScheduledTaskId,
        now_ms: i64,
    ) -> BackendFuture<'_, Option<ScheduledTaskRun>>;

    fn automation_graphs(
        &self,
        request: AutomationGraphListRequest,
    ) -> BackendFuture<'_, Vec<AutomationGraph>>;

    fn create_automation_graph(
        &self,
        request: MutationRequest<AutomationGraphCreateRequest>,
    ) -> BackendFuture<'_, AutomationGraph>;

    fn update_automation_graph(
        &self,
        request: MutationRequest<AutomationGraphUpdateRequest>,
    ) -> BackendFuture<'_, AutomationGraph>;

    fn replace_automation_definition(
        &self,
        request: MutationRequest<AutomationGraphDefinitionUpdateRequest>,
    ) -> BackendFuture<'_, AutomationGraph>;

    fn set_automation_graph_status(
        &self,
        graph_id: AutomationGraphId,
        status: AutomationGraphStatus,
    ) -> BackendFuture<'_, AutomationGraph>;

    fn archive_automation_graph(
        &self,
        graph_id: AutomationGraphId,
    ) -> BackendFuture<'_, AutomationGraph>;

    fn automation_runs(
        &self,
        request: AutomationRunListRequest,
    ) -> BackendFuture<'_, Vec<AutomationRun>>;

    fn automation_run_steps(
        &self,
        request: AutomationRunStepListRequest,
    ) -> BackendFuture<'_, Vec<AutomationRunStep>>;

    fn start_automation_run(
        &self,
        request: MutationRequest<AutomationRunStartRequest>,
    ) -> BackendFuture<'_, AutomationRun>;

    fn resume_automation_run(
        &self,
        request: MutationRequest<AutomationRunResumeRequest>,
    ) -> BackendFuture<'_, AutomationRun>;

    fn cancel_automation_run(
        &self,
        request: MutationRequest<AutomationRunCancelRequest>,
    ) -> BackendFuture<'_, AutomationRun>;

    fn validate_mcp_server(
        &self,
        request: McpServerValidateRequest,
    ) -> BackendFuture<'_, McpServerValidationResult>;

    fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> BackendFuture<'_, McpMarketSearchResponse>;

    fn install_mcp_market_entry(
        &self,
        request: MutationRequest<McpMarketInstallRequest>,
    ) -> BackendFuture<'_, McpMarketInstallResult>;

    fn skills(&self) -> BackendFuture<'_, Vec<Skill>>;

    fn create_skill(
        &self,
        request: MutationRequest<SkillCreateRequest>,
    ) -> BackendFuture<'_, Skill>;

    fn update_skill(
        &self,
        request: MutationRequest<SkillUpdateRequest>,
    ) -> BackendFuture<'_, Skill>;

    fn delete_skill(&self, request: MutationRequest<SkillDeleteRequest>) -> BackendFuture<'_, ()>;

    fn set_skill_agent_matrix(
        &self,
        request: MutationRequest<SkillSetAgentMatrixRequest>,
    ) -> BackendFuture<'_, Skill>;

    fn skill_agent_matrix(
        &self,
        request: SkillAgentMatrixListRequest,
    ) -> BackendFuture<'_, Vec<SkillAgentMatrix>>;

    fn discover_skill_sources(
        &self,
        request: SkillDiscoverRequest,
    ) -> BackendFuture<'_, SkillDiscoveryResponse>;

    fn import_skills(
        &self,
        request: MutationRequest<SkillImportRequest>,
    ) -> BackendFuture<'_, SkillImportResult>;

    fn validate_skill(
        &self,
        request: SkillValidateRequest,
    ) -> BackendFuture<'_, SkillValidationResult>;

    fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> BackendFuture<'_, SkillMarketSearchResponse>;

    fn skill_market_document(
        &self,
        request: SkillMarketDocumentRequest,
    ) -> BackendFuture<'_, SkillMarketDocument>;

    fn install_skill_market_entry(
        &self,
        request: MutationRequest<SkillMarketInstallRequest>,
    ) -> BackendFuture<'_, SkillMarketInstallResult>;

    fn prompts(&self) -> BackendFuture<'_, Vec<Prompt>>;

    fn create_prompt(
        &self,
        request: MutationRequest<PromptCreateRequest>,
    ) -> BackendFuture<'_, Prompt>;

    fn update_prompt(
        &self,
        request: MutationRequest<PromptUpdateRequest>,
    ) -> BackendFuture<'_, Prompt>;

    fn delete_prompt(&self, request: MutationRequest<PromptDeleteRequest>)
    -> BackendFuture<'_, ()>;

    fn validate_prompt(
        &self,
        request: PromptValidateRequest,
    ) -> BackendFuture<'_, PromptValidationResult>;

    /// Records one composer insertion of a reusable Prompt.
    ///
    /// The composer orders its quick phrases by this counter, so a paired
    /// client reports the insertion to the authority that owns the Prompt.
    fn record_prompt_usage(
        &self,
        request: MutationRequest<PromptUsageRecordRequest>,
    ) -> BackendFuture<'_, PromptUsage>;

    fn hooks(&self) -> BackendFuture<'_, Vec<Hook>>;

    fn create_hook(&self, request: MutationRequest<HookCreateRequest>) -> BackendFuture<'_, Hook>;

    fn update_hook(&self, request: MutationRequest<HookUpdateRequest>) -> BackendFuture<'_, Hook>;

    fn delete_hook(&self, request: MutationRequest<HookDeleteRequest>) -> BackendFuture<'_, ()>;

    fn preview_hook_install(
        &self,
        request: HookInstallPreviewRequest,
    ) -> BackendFuture<'_, HookInstallPreview>;

    fn refresh_detected_agent_versions(&self) -> BackendFuture<'_, usize>;

    fn agent_catalog(&self) -> BackendFuture<'_, AgentCatalogListResponse>;

    fn acp_catalog_presets(&self) -> BackendFuture<'_, AcpProviderCatalogListResponse>;

    fn acp_profile_config(
        &self,
        provider_profile_id: ProviderProfileId,
    ) -> BackendFuture<'_, AcpProviderConfig>;

    fn update_acp_profile_config(
        &self,
        request: MutationRequest<AcpProviderProfileUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile>;

    fn preview_native_import(
        &self,
        request: ProviderNativeImportPreviewRequest,
    ) -> BackendFuture<'_, ProviderNativeImportPreview>;

    fn create_profile_from_import(
        &self,
        request: MutationRequest<ProviderNativeImportCreateRequest>,
    ) -> BackendFuture<'_, ProviderNativeImportCreateResult>;

    fn preview_native_export(
        &self,
        request: ProviderNativeExportPreviewRequest,
    ) -> BackendFuture<'_, ProviderNativeExportPreview>;

    fn apply_native_export(
        &self,
        request: MutationRequest<ProviderNativeExportApplyRequest>,
    ) -> BackendFuture<'_, ProviderNativeExportApplyResult>;

    fn rollback_native_export(
        &self,
        request: MutationRequest<ProviderNativeExportRollbackRequest>,
    ) -> BackendFuture<'_, ProviderNativeExportRollbackResult>;

    fn native_exports(
        &self,
        request: ProviderNativeExportListRequest,
    ) -> BackendFuture<'_, Vec<ProviderNativeExportRecordSummary>>;
}

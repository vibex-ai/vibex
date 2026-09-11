use serde::{Deserialize, Serialize};
use vibex_core::{
    AcpProviderCatalogListResponse, AcpProviderConfig, AcpProviderProfileUpdateRequest,
    AgentCatalogListResponse, AgentId, AgentListRequest, AgentListResponse,
    AgentModelProviderBinding, AgentModelProviderBindingCreateRequest,
    AgentModelProviderBindingListRequest, AgentModelProviderBindingUpdateRequest,
    AgentModelProviderDisplayOrderListRequest, AgentModelProviderDisplayOrderListResponse,
    AgentModelProviderDisplayOrderSetRequest, AgentModelProviderDisplayOrderSetResponse,
    AgentModelProviderProfileDeleteRequest, AgentModelProviderProfileFetchModelsRequest,
    AgentModelProviderProfileFetchModelsResponse, AgentModelProviderProfileTestRequest,
    AgentModelProviderProfileTestResult, AgentProviderProjectionCapability,
    AgentProviderProjectionCapabilityRequest, AgentProviderProjectionPreview,
    AgentProviderProjectionPreviewRequest, AgentRuntimeProbeCancelRequest,
    AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord, AgentRuntimeProbeStartRequest,
    AgentRuntimeProfile, AgentRuntimeProfileCreateRequest, AgentRuntimeProfileUpdateRequest,
    AgentSnapshotEntry, AutomationGraph, AutomationGraphCreateRequest,
    AutomationGraphDefinitionUpdateRequest, AutomationGraphId, AutomationGraphListRequest,
    AutomationGraphStatus, AutomationGraphUpdateRequest, AutomationRun, AutomationRunCancelRequest,
    AutomationRunListRequest, AutomationRunResumeRequest, AutomationRunStartRequest,
    AutomationRunStep, AutomationRunStepListRequest, CustomAgentCreateRequest,
    CustomAgentDeleteRequest, Hook, HookCreateRequest, HookDeleteRequest, HookInstallPreview,
    HookInstallPreviewRequest, HookUpdateRequest, McpServer, McpServerAgentMatrix,
    McpServerAgentMatrixListRequest, McpServerCreateRequest, McpServerDeleteRequest,
    McpServerDiscoverRequest, McpServerDiscoveryResponse, McpServerImportRequest,
    McpServerImportResult, McpServerSetAgentMatrixRequest, McpServerUpdateRequest,
    McpServerValidateRequest, McpServerValidationResult, ModelProviderProfile,
    ModelProviderProfileCreateRequest, ModelProviderProfileUpdateRequest, Prompt,
    PromptCreateRequest, PromptDeleteRequest, PromptUpdateRequest, PromptValidateRequest,
    PromptValidationResult, ProviderCapabilitySummary, ProviderCredentialSecretMutationRequest,
    ProviderHealthSummary, ProviderNativeExportApplyRequest, ProviderNativeExportApplyResult,
    ProviderNativeExportListRequest, ProviderNativeExportPreview,
    ProviderNativeExportPreviewRequest, ProviderNativeExportRecordSummary,
    ProviderNativeExportRollbackRequest, ProviderNativeExportRollbackResult,
    ProviderNativeImportCreateRequest, ProviderNativeImportCreateResult,
    ProviderNativeImportPreview, ProviderNativeImportPreviewRequest, ProviderProfile,
    ProviderProfileId, ProviderProfileSummary, ProviderRunCapabilityProbesRequest,
    ProviderRunCapabilityProbesResult, ProviderRunHealthProbesRequest,
    ProviderRunHealthProbesResult, RelayPeerId, RelayRoomId, ScheduledTaskAttentionListRequest,
    ScheduledTaskAttentionSummary, ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord,
    ScheduledTaskCreateRequest, ScheduledTaskId, ScheduledTaskListRequest, ScheduledTaskRun,
    ScheduledTaskRunListRequest, ScheduledTaskUpdateRequest, Skill, SkillAgentMatrix,
    SkillAgentMatrixListRequest, SkillCreateRequest, SkillDeleteRequest, SkillDiscoverRequest,
    SkillDiscoveryResponse, SkillImportRequest, SkillImportResult, SkillSetAgentMatrixRequest,
    SkillUpdateRequest, SkillValidateRequest, SkillValidationResult,
};

use crate::{BackendBound, BackendFuture, MutationRequest};

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
}

pub trait ManagementBackend: BackendBound {
    fn list_agents(&self, request: AgentListRequest) -> BackendFuture<'_, AgentListResponse>;

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

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>>;

    fn run_health_probes(
        &self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult>;

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary>;

    /// Deletes a per-Agent model provider profile.
    fn delete_agent_model_provider_profile(
        &self,
        request: MutationRequest<AgentModelProviderProfileDeleteRequest>,
    ) -> BackendFuture<'_, ()>;

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

    fn capability_summaries(&self) -> BackendFuture<'_, Vec<ProviderCapabilitySummary>>;

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

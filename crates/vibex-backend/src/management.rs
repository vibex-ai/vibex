use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, AgentListRequest, AgentListResponse, AgentModelProviderBinding,
    AgentModelProviderBindingCreateRequest, AgentModelProviderBindingListRequest,
    AgentModelProviderBindingUpdateRequest, AgentModelProviderDisplayOrderListRequest,
    AgentModelProviderDisplayOrderListResponse, AgentModelProviderDisplayOrderSetRequest,
    AgentModelProviderDisplayOrderSetResponse, AgentModelProviderProfileDeleteRequest,
    AgentModelProviderProfileFetchModelsRequest, AgentModelProviderProfileFetchModelsResponse,
    AgentModelProviderProfileTestRequest, AgentModelProviderProfileTestResult,
    AgentProviderProjectionCapability, AgentProviderProjectionCapabilityRequest,
    AgentProviderProjectionPreview, AgentProviderProjectionPreviewRequest,
    AgentRuntimeProbeCancelRequest, AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord,
    AgentRuntimeProbeStartRequest, AgentRuntimeProfile, AgentRuntimeProfileCreateRequest,
    AgentRuntimeProfileUpdateRequest, AgentSnapshotEntry, CustomAgentCreateRequest,
    CustomAgentDeleteRequest, McpServer, McpServerAgentMatrix, McpServerAgentMatrixListRequest,
    McpServerCreateRequest, McpServerDeleteRequest, McpServerDiscoverRequest,
    McpServerDiscoveryResponse, McpServerImportRequest, McpServerImportResult,
    McpServerSetAgentMatrixRequest, McpServerUpdateRequest, McpServerValidateRequest,
    McpServerValidationResult, ModelProviderProfile, ModelProviderProfileCreateRequest,
    ModelProviderProfileUpdateRequest, ProviderCapabilitySummary,
    ProviderCredentialSecretMutationRequest, ProviderHealthSummary, ProviderProfileId,
    ProviderProfileSummary, ProviderRunCapabilityProbesRequest, ProviderRunCapabilityProbesResult,
    ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult, RelayPeerId, RelayRoomId,
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

    fn validate_mcp_server(
        &self,
        request: McpServerValidateRequest,
    ) -> BackendFuture<'_, McpServerValidationResult>;
}

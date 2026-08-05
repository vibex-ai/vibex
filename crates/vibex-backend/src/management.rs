use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, AgentListRequest, AgentListResponse, AgentModelProviderBinding,
    AgentModelProviderBindingCreateRequest, AgentModelProviderBindingListRequest,
    AgentModelProviderBindingUpdateRequest, AgentProviderProjectionCapability,
    AgentProviderProjectionCapabilityRequest, AgentProviderProjectionPreview,
    AgentProviderProjectionPreviewRequest, AgentRuntimeProfile, AgentRuntimeProfileCreateRequest,
    AgentRuntimeProfileUpdateRequest, ModelProviderProfile, ModelProviderProfileCreateRequest,
    ModelProviderProfileUpdateRequest, ProviderCredentialSecretMutationRequest,
    ProviderHealthSummary, ProviderProfileId, ProviderProfileSummary,
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
}

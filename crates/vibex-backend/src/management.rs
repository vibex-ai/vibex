use serde::{Deserialize, Serialize};
use vibex_core::{
    AgentId, AgentListRequest, AgentListResponse, ProviderHealthSummary, ProviderProfileId,
    ProviderProfileSummary, ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult,
    RelayPeerId, RelayRoomId,
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

    fn health_summaries(&self) -> BackendFuture<'_, Vec<ProviderHealthSummary>>;

    fn run_health_probes(
        &self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendFuture<'_, ProviderRunHealthProbesResult>;

    fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary>;
}

//! Shared, capability-filtered ManagementCenter projection.
//!
//! This module intentionally contains display-safe summaries only.  Provider
//! secrets, raw configuration files, pairing challenges, and durable device
//! records remain in the authoritative backend.

use std::collections::BTreeSet;
use std::fmt;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vibex_backend::{
    BackendCapabilitySnapshot, BackendError, BackendErrorKind, BackendOperation, BackendResult,
    DeviceBackend, DomainCapabilities, ManagementBackend, ManagementProfileSelectionRequest,
    MutationRequest, RelayConnectionState, RelayStatusSummary,
};
use vibex_core::{
    AgentListRequest, ProviderHealthStatus, ProviderProfileSummary, ProviderRunHealthProbesRequest,
    ProviderRunHealthProbesResult, RemoteAuditListRequest, RemoteAuditRecord,
    RemoteCancelPairingOfferRequest, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeviceDetail, RemoteDevicePermissionLevel,
    RemoteDeviceStatus, RemotePairingOfferSummary, RemoteRevokeDeviceRequest, RequestId,
};

use crate::{MIN_TOUCH_TARGET_PX, ShellKind};

pub const MANAGEMENT_WORKFLOW_SCHEMA_VERSION: &str = "vibex-management-workflow.v1";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementSection {
    Overview,
    Providers,
    Health,
    Relay,
    Devices,
}

impl ManagementSection {
    pub const ALL: [Self; 5] = [
        Self::Overview,
        Self::Providers,
        Self::Health,
        Self::Relay,
        Self::Devices,
    ];

    pub const fn label(self) -> &'static str {
        match self {
            Self::Overview => "ManagementCenter",
            Self::Providers => "Providers",
            Self::Health => "Provider health",
            Self::Relay => "Relay",
            Self::Devices => "Devices",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ManagementNavigation {
    pub active: ManagementSection,
    pub generation: u64,
    dirty: BTreeSet<ManagementSection>,
}

impl Default for ManagementNavigation {
    fn default() -> Self {
        Self {
            active: ManagementSection::Overview,
            generation: 0,
            dirty: BTreeSet::new(),
        }
    }
}

impl ManagementNavigation {
    pub fn mark_dirty(&mut self, section: ManagementSection, dirty: bool) {
        if dirty {
            self.dirty.insert(section);
        } else {
            self.dirty.remove(&section);
        }
    }

    pub fn is_dirty(&self, section: ManagementSection) -> bool {
        self.dirty.contains(&section)
    }

    pub fn switch(&mut self, next: ManagementSection, discard_dirty_current: bool) -> bool {
        if next == self.active {
            return true;
        }
        if self.is_dirty(self.active) && !discard_dirty_current {
            return false;
        }
        if discard_dirty_current {
            self.mark_dirty(self.active, false);
        }
        self.active = next;
        self.generation = self.generation.saturating_add(1);
        true
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentStatusProjection {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    pub installed: bool,
    pub configured: bool,
    pub runtime_status: String,
    pub model_count: usize,
    pub updated_at_ms: Option<i64>,
}

impl AgentStatusProjection {
    fn from_entry(entry: &vibex_core::AgentSnapshotEntry) -> Self {
        Self {
            id: entry.id.as_str().to_string(),
            label: entry.label.clone(),
            enabled: entry.enabled,
            installed: entry.installed,
            configured: entry.configured,
            runtime_status: format!("{:?}", entry.runtime_status),
            model_count: entry.models.len(),
            updated_at_ms: entry.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderProfileProjection {
    pub id: String,
    pub agent_id: String,
    pub display_name: String,
    pub kind: vibex_core::ProviderKind,
    pub status: vibex_core::ProviderProfileStatus,
    pub account_alias: Option<String>,
    pub default_model: Option<String>,
    pub configured_model_count: usize,
    pub secret_setup_state: vibex_core::ProviderSecretSetupState,
    pub updated_at_ms: i64,
}

impl ProviderProfileProjection {
    fn from_summary(profile: &ProviderProfileSummary) -> Self {
        Self {
            id: profile.id.as_str().to_string(),
            agent_id: profile.agent_id.as_str().to_string(),
            display_name: profile.display_name.clone(),
            kind: profile.kind,
            status: profile.status,
            account_alias: profile.account_alias.clone(),
            default_model: profile.default_model.clone(),
            configured_model_count: profile.configured_models.len(),
            secret_setup_state: profile.secret_setup_state,
            updated_at_ms: profile.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderHealthProjection {
    pub profile_id: String,
    pub display_name: String,
    pub status: ProviderHealthStatus,
    pub probe_count: usize,
    pub last_checked_at_ms: Option<i64>,
    pub expires_at_ms: Option<i64>,
}

impl ProviderHealthProjection {
    fn from_summary(summary: &vibex_core::ProviderHealthSummary) -> Self {
        Self {
            profile_id: summary.profile.id.as_str().to_string(),
            display_name: summary.profile.display_name.clone(),
            status: summary.overall_status,
            probe_count: summary.probe_results.len(),
            last_checked_at_ms: summary.last_checked_at_ms,
            expires_at_ms: summary.expires_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RelayStatusProjection {
    pub state: RelayConnectionState,
    pub reconnect_attempt: u32,
    pub next_retry_at_ms: Option<i64>,
    pub has_error: bool,
}

impl RelayStatusProjection {
    fn from_summary(summary: &RelayStatusSummary) -> Self {
        Self {
            state: summary.state,
            reconnect_attempt: summary.reconnect_attempt,
            next_retry_at_ms: summary.next_retry_at_ms,
            has_error: summary
                .last_error
                .as_ref()
                .is_some_and(|value| !value.is_empty()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceProjection {
    pub device_id: String,
    pub display_name: String,
    pub permission_level: RemoteDevicePermissionLevel,
    pub status: RemoteDeviceStatus,
    pub grant_revision: u64,
    pub paired_at_ms: Option<i64>,
    pub last_seen_at_ms: Option<i64>,
    pub revoked_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

impl DeviceProjection {
    fn from_detail(device: &RemoteDeviceDetail) -> Self {
        Self {
            device_id: device.device_id.as_str().to_string(),
            display_name: device.display_name.clone(),
            permission_level: device.permission_level,
            status: device.status,
            grant_revision: device.grant_revision,
            paired_at_ms: device.paired_at_ms,
            last_seen_at_ms: device.last_seen_at_ms,
            revoked_at_ms: device.revoked_at_ms,
            updated_at_ms: device.updated_at_ms,
        }
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingOfferProjection {
    pub offer_id: String,
    pub expires_at_ms: i64,
    pub permission_level: RemoteDevicePermissionLevel,
    pub direct_candidate_count: usize,
    pub has_relay_candidate: bool,
    pub granted_permission_count: usize,
    pub canceled: bool,
    pub claimed_device_id: Option<String>,
    pub launch_fragment: Option<String>,
}

impl fmt::Debug for PairingOfferProjection {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingOfferProjection")
            .field("offer_id", &self.offer_id)
            .field("expires_at_ms", &self.expires_at_ms)
            .field("permission_level", &self.permission_level)
            .field("direct_candidate_count", &self.direct_candidate_count)
            .field("has_relay_candidate", &self.has_relay_candidate)
            .field("granted_permission_count", &self.granted_permission_count)
            .field("canceled", &self.canceled)
            .field("has_claimed_device", &self.claimed_device_id.is_some())
            .field("has_launch_fragment", &self.launch_fragment.is_some())
            .finish()
    }
}

impl PairingOfferProjection {
    fn from_response(response: &RemoteCreatePairingOfferResponse) -> Self {
        Self::from_summary(
            &response.offer.summary,
            Some(response.launch_fragment.clone()),
        )
    }

    fn from_summary(summary: &RemotePairingOfferSummary, launch_fragment: Option<String>) -> Self {
        Self {
            offer_id: summary.offer_id.as_str().to_string(),
            expires_at_ms: summary.expires_at_ms,
            permission_level: summary.permission_level,
            direct_candidate_count: summary.direct_candidates.len(),
            has_relay_candidate: summary.relay_candidate.is_some(),
            granted_permission_count: summary.granted_permissions.len(),
            canceled: summary.canceled,
            claimed_device_id: summary
                .claimed_device_id
                .as_ref()
                .map(|device| device.as_str().to_string()),
            launch_fragment,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ManagementLoadState {
    Idle,
    Loading,
    Ready,
    Partial,
    Offline,
    Error,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ManagementWorkflowState {
    pub generation: u64,
    pub navigation: ManagementNavigation,
    pub load_state: ManagementLoadState,
    pub agents: Vec<AgentStatusProjection>,
    pub profiles: Vec<ProviderProfileProjection>,
    pub health: Vec<ProviderHealthProjection>,
    pub relay: Option<RelayStatusProjection>,
    pub devices: Vec<DeviceProjection>,
    pub pairing_offer: Option<PairingOfferProjection>,
    pub audit_count: usize,
    pub last_operation_id: u64,
    pub last_error: Option<BackendError>,
}

impl fmt::Debug for ManagementWorkflowState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementWorkflowState")
            .field("generation", &self.generation)
            .field("active_section", &self.navigation.active)
            .field("load_state", &self.load_state)
            .field("agent_count", &self.agents.len())
            .field("profile_count", &self.profiles.len())
            .field("health_count", &self.health.len())
            .field("has_relay", &self.relay.is_some())
            .field("device_count", &self.devices.len())
            .field("has_pairing_offer", &self.pairing_offer.is_some())
            .field("audit_count", &self.audit_count)
            .field("last_operation_id", &self.last_operation_id)
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .finish()
    }
}

impl Default for ManagementWorkflowState {
    fn default() -> Self {
        Self {
            generation: 0,
            navigation: ManagementNavigation::default(),
            load_state: ManagementLoadState::Idle,
            agents: Vec::new(),
            profiles: Vec::new(),
            health: Vec::new(),
            relay: None,
            devices: Vec::new(),
            pairing_offer: None,
            audit_count: 0,
            last_operation_id: 0,
            last_error: None,
        }
    }
}

impl ManagementWorkflowState {
    fn next_generation(&mut self) -> u64 {
        self.generation = self.generation.saturating_add(1).max(1);
        self.generation
    }

    fn next_operation(&mut self) -> u64 {
        self.last_operation_id = self.last_operation_id.saturating_add(1).max(1);
        self.last_operation_id
    }

    fn ensure_current_operation(&self, operation: u64) -> BackendResult<()> {
        if operation == self.last_operation_id {
            Ok(())
        } else {
            Err(BackendError::conflict(
                "management_operation_stale",
                "a newer management operation replaced this result",
            ))
        }
    }
}

#[derive(Clone)]
pub struct ManagementWorkflowCapabilities {
    pub schema_version: String,
    pub backend_revision: u64,
    pub management: DomainCapabilities,
    pub device: DomainCapabilities,
}

impl fmt::Debug for ManagementWorkflowCapabilities {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementWorkflowCapabilities")
            .field("schema_version", &self.schema_version)
            .field("backend_revision", &self.backend_revision)
            .field("management_availability", &self.management.availability)
            .field(
                "management_operation_count",
                &self.management.operations.len(),
            )
            .field("device_availability", &self.device.availability)
            .field("device_operation_count", &self.device.operations.len())
            .finish()
    }
}

impl ManagementWorkflowCapabilities {
    pub fn from_backend(snapshot: &BackendCapabilitySnapshot) -> Self {
        use BackendOperation::*;
        let management_allowed = [
            ManagementAgents,
            ManagementProfiles,
            ManagementProfileSelect,
            ManagementHealth,
            ManagementRelay,
        ]
        .into_iter()
        .collect::<BTreeSet<_>>();
        let device_allowed = [DevicePairing, DeviceList, DeviceRevoke, DeviceAudit]
            .into_iter()
            .collect::<BTreeSet<_>>();
        Self {
            schema_version: MANAGEMENT_WORKFLOW_SCHEMA_VERSION.to_string(),
            backend_revision: snapshot.revision,
            management: filter_domain(&snapshot.management, &management_allowed),
            device: filter_domain(&snapshot.device, &device_allowed),
        }
    }

    pub fn supports(&self, operation: BackendOperation) -> bool {
        let domain = match operation {
            BackendOperation::ManagementAgents
            | BackendOperation::ManagementProfiles
            | BackendOperation::ManagementProfileSelect
            | BackendOperation::ManagementHealth
            | BackendOperation::ManagementRelay => &self.management,
            BackendOperation::DevicePairing
            | BackendOperation::DeviceList
            | BackendOperation::DeviceRevoke
            | BackendOperation::DeviceAudit => &self.device,
            _ => return false,
        };
        domain.supports(operation)
    }

    pub fn require(&self, operation: BackendOperation) -> BackendResult<()> {
        if self.supports(operation) {
            return Ok(());
        }
        let domain = match operation {
            BackendOperation::ManagementAgents
            | BackendOperation::ManagementProfiles
            | BackendOperation::ManagementProfileSelect
            | BackendOperation::ManagementHealth
            | BackendOperation::ManagementRelay => &self.management,
            _ => &self.device,
        };
        let label = management_operation_label(operation);
        let error = match domain.availability {
            vibex_backend::CapabilityAvailability::Offline => BackendError::offline(
                format!("{label}_offline"),
                "the management authority is offline",
            ),
            vibex_backend::CapabilityAvailability::RequiresPermission => BackendError::permission(
                format!("{label}_permission_required"),
                "the paired device does not permit this management action",
            ),
            vibex_backend::CapabilityAvailability::Degraded => BackendError::loading(
                format!("{label}_degraded"),
                "the management service is temporarily degraded",
            ),
            vibex_backend::CapabilityAvailability::Available
            | vibex_backend::CapabilityAvailability::Unsupported => BackendError::unsupported(
                format!("{label}_unsupported"),
                "the requested management action is outside v1",
            ),
        };
        Err(error)
    }
}

fn filter_domain(
    source: &DomainCapabilities,
    allowed: &BTreeSet<BackendOperation>,
) -> DomainCapabilities {
    DomainCapabilities {
        availability: source.availability,
        operations: source.operations.intersection(allowed).copied().collect(),
    }
}

fn management_operation_label(operation: BackendOperation) -> &'static str {
    use BackendOperation::*;
    match operation {
        ManagementAgents => "management_agents",
        ManagementProfiles => "management_profiles",
        ManagementProfileSelect => "management_profile_select",
        ManagementHealth => "management_health",
        ManagementRelay => "management_relay",
        DevicePairing => "device_pairing",
        DeviceList => "device_list",
        DeviceRevoke => "device_revoke",
        DeviceAudit => "device_audit",
        _ => "management_operation",
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct ManagementWorkflowView {
    pub schema_version: &'static str,
    pub generation: u64,
    pub active_section: ManagementSection,
    pub load_state: ManagementLoadState,
    pub agents: Vec<AgentStatusProjection>,
    pub profiles: Vec<ProviderProfileProjection>,
    pub health: Vec<ProviderHealthProjection>,
    pub relay: Option<RelayStatusProjection>,
    pub devices: Vec<DeviceProjection>,
    pub pairing_offer: Option<PairingOfferProjection>,
    pub audit_count: usize,
    pub presentation: crate::PanelPresentation,
    pub action_touch_target_px: u16,
    pub hover_required: bool,
    pub last_error: Option<BackendError>,
}

impl fmt::Debug for ManagementWorkflowView {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ManagementWorkflowView")
            .field("schema_version", &self.schema_version)
            .field("generation", &self.generation)
            .field("active_section", &self.active_section)
            .field("load_state", &self.load_state)
            .field("agent_count", &self.agents.len())
            .field("profile_count", &self.profiles.len())
            .field("health_count", &self.health.len())
            .field("has_relay", &self.relay.is_some())
            .field("device_count", &self.devices.len())
            .field("has_pairing_offer", &self.pairing_offer.is_some())
            .field("audit_count", &self.audit_count)
            .field("presentation", &self.presentation)
            .field("action_touch_target_px", &self.action_touch_target_px)
            .field("hover_required", &self.hover_required)
            .field(
                "last_error_code",
                &self.last_error.as_ref().map(|error| error.code.as_str()),
            )
            .finish()
    }
}

impl ManagementWorkflowState {
    pub fn view(&self, shell: ShellKind) -> ManagementWorkflowView {
        ManagementWorkflowView {
            schema_version: MANAGEMENT_WORKFLOW_SCHEMA_VERSION,
            generation: self.generation,
            active_section: self.navigation.active,
            load_state: self.load_state,
            agents: self.agents.clone(),
            profiles: self.profiles.clone(),
            health: self.health.clone(),
            relay: self.relay.clone(),
            devices: self.devices.clone(),
            pairing_offer: self.pairing_offer.clone(),
            audit_count: self.audit_count,
            presentation: match shell {
                ShellKind::Wide => crate::PanelPresentation::Docked,
                ShellKind::Medium => crate::PanelPresentation::Drawer,
                ShellKind::Compact => crate::PanelPresentation::Sheet,
            },
            action_touch_target_px: MIN_TOUCH_TARGET_PX,
            hover_required: false,
            last_error: self.last_error.clone(),
        }
    }
}

#[derive(Clone)]
pub struct ManagementWorkflowController {
    management: Arc<dyn ManagementBackend>,
    device: Arc<dyn DeviceBackend>,
    pub capabilities: ManagementWorkflowCapabilities,
    pub state: ManagementWorkflowState,
}

impl ManagementWorkflowController {
    pub fn new(
        management: Arc<dyn ManagementBackend>,
        device: Arc<dyn DeviceBackend>,
        capabilities: ManagementWorkflowCapabilities,
    ) -> Self {
        Self {
            management,
            device,
            capabilities,
            state: ManagementWorkflowState::default(),
        }
    }

    pub fn set_capabilities(&mut self, capabilities: ManagementWorkflowCapabilities) {
        self.capabilities = capabilities;
    }

    pub fn switch_section(
        &mut self,
        section: ManagementSection,
        discard_dirty_current: bool,
    ) -> bool {
        if self.state.navigation.switch(section, discard_dirty_current) {
            self.state.next_generation();
            true
        } else {
            false
        }
    }

    pub fn mark_dirty(&mut self, section: ManagementSection, dirty: bool) {
        self.state.navigation.mark_dirty(section, dirty);
    }

    pub async fn refresh(&mut self) -> BackendResult<()> {
        let generation = self.state.next_generation();
        self.state.load_state = ManagementLoadState::Loading;
        self.state.last_error = None;
        let mut failures = Vec::new();

        if self
            .capabilities
            .supports(BackendOperation::ManagementAgents)
        {
            match self
                .management
                .list_agents(AgentListRequest {
                    include_disabled: true,
                })
                .await
            {
                Ok(response) if generation == self.state.generation => {
                    self.state.agents = response
                        .agents
                        .iter()
                        .map(AgentStatusProjection::from_entry)
                        .collect();
                }
                Err(error) => failures.push(error),
                _ => return Ok(()),
            }
        }
        if self
            .capabilities
            .supports(BackendOperation::ManagementProfiles)
        {
            match self.management.list_profiles().await {
                Ok(profiles) if generation == self.state.generation => {
                    self.state.profiles = profiles
                        .iter()
                        .map(ProviderProfileProjection::from_summary)
                        .collect();
                }
                Err(error) => failures.push(error),
                _ => return Ok(()),
            }
        }
        if self
            .capabilities
            .supports(BackendOperation::ManagementHealth)
        {
            match self.management.health_summaries().await {
                Ok(summaries) if generation == self.state.generation => {
                    self.state.health = summaries
                        .iter()
                        .map(ProviderHealthProjection::from_summary)
                        .collect();
                }
                Err(error) => failures.push(error),
                _ => return Ok(()),
            }
        }
        if self
            .capabilities
            .supports(BackendOperation::ManagementRelay)
        {
            match self.management.relay_status().await {
                Ok(status) if generation == self.state.generation => {
                    self.state.relay = Some(RelayStatusProjection::from_summary(&status));
                }
                Err(error) => failures.push(error),
                _ => return Ok(()),
            }
        }
        if self.capabilities.supports(BackendOperation::DeviceList) {
            match self.device.list_devices().await {
                Ok(devices) if generation == self.state.generation => {
                    self.state.devices =
                        devices.iter().map(DeviceProjection::from_detail).collect();
                }
                Err(error) => failures.push(error),
                _ => return Ok(()),
            }
        }

        if generation != self.state.generation {
            return Ok(());
        }
        if let Some(error) = failures.into_iter().next() {
            let offline = error.kind == BackendErrorKind::Offline;
            self.state.load_state = if offline {
                ManagementLoadState::Offline
            } else if self.state.agents.is_empty()
                && self.state.profiles.is_empty()
                && self.state.health.is_empty()
                && self.state.devices.is_empty()
            {
                ManagementLoadState::Error
            } else {
                ManagementLoadState::Partial
            };
            self.state.last_error = Some(error.clone());
            Err(error)
        } else {
            self.state.load_state = ManagementLoadState::Ready;
            Ok(())
        }
    }

    pub async fn select_profile(
        &mut self,
        request: MutationRequest<ManagementProfileSelectionRequest>,
    ) -> BackendResult<ProviderProfileProjection> {
        self.capabilities
            .require(BackendOperation::ManagementProfileSelect)?;
        let operation = self.state.next_operation();
        let result = self.management.select_profile(request).await;
        self.state.ensure_current_operation(operation)?;
        let profile = result.map(|summary| ProviderProfileProjection::from_summary(&summary));
        match profile {
            Ok(profile) => {
                self.state.profiles.retain(|item| item.id != profile.id);
                self.state.profiles.push(profile.clone());
                Ok(profile)
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn run_health_probes(
        &mut self,
        request: MutationRequest<ProviderRunHealthProbesRequest>,
    ) -> BackendResult<ProviderRunHealthProbesResult> {
        self.capabilities
            .require(BackendOperation::ManagementHealth)?;
        let operation = self.state.next_operation();
        let result = self.management.run_health_probes(request).await;
        self.state.ensure_current_operation(operation)?;
        if let Ok(result) = &result {
            self.state.health = result
                .summaries
                .iter()
                .map(ProviderHealthProjection::from_summary)
                .collect();
        } else if let Err(error) = &result {
            self.state.last_error = Some(error.clone());
        }
        result
    }

    pub async fn create_pairing_offer(
        &mut self,
        request: MutationRequest<RemoteCreatePairingOfferRequest>,
    ) -> BackendResult<RemoteCreatePairingOfferResponse> {
        self.capabilities.require(BackendOperation::DevicePairing)?;
        let operation = self.state.next_operation();
        let result = self.device.create_pairing_offer_v2(request).await;
        self.state.ensure_current_operation(operation)?;
        match &result {
            Ok(response) => {
                self.state.pairing_offer = Some(PairingOfferProjection::from_response(response))
            }
            Err(error) => self.state.last_error = Some(error.clone()),
        }
        result
    }

    pub async fn cancel_pairing_offer(
        &mut self,
        offer_id: RequestId,
    ) -> BackendResult<RemotePairingOfferSummary> {
        self.capabilities.require(BackendOperation::DevicePairing)?;
        let operation = self.state.next_operation();
        let result = self
            .device
            .cancel_pairing_offer(MutationRequest::new(RemoteCancelPairingOfferRequest {
                offer_id,
            }))
            .await;
        self.state.ensure_current_operation(operation)?;
        match &result {
            Ok(summary) => {
                self.state.pairing_offer =
                    Some(PairingOfferProjection::from_summary(summary, None));
            }
            Err(error) => self.state.last_error = Some(error.clone()),
        }
        result
    }

    pub async fn revoke_device(
        &mut self,
        request: MutationRequest<RemoteRevokeDeviceRequest>,
    ) -> BackendResult<DeviceProjection> {
        self.capabilities.require(BackendOperation::DeviceRevoke)?;
        let operation = self.state.next_operation();
        let result = self.device.revoke_device(request).await;
        self.state.ensure_current_operation(operation)?;
        match result {
            Ok(device) => {
                let projection = DeviceProjection::from_detail(&device);
                self.state
                    .devices
                    .retain(|item| item.device_id != projection.device_id);
                self.state.devices.push(projection.clone());
                Ok(projection)
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn refresh_audit(&mut self, limit: u32) -> BackendResult<Vec<RemoteAuditRecord>> {
        self.capabilities.require(BackendOperation::DeviceAudit)?;
        let records = self
            .device
            .audit_records(RemoteAuditListRequest {
                device_id: None,
                limit: Some(limit.min(200)),
            })
            .await?;
        self.state.audit_count = records.len();
        Ok(records)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::Mutex;

    use super::*;
    use vibex_backend::BackendFuture;
    use vibex_core::{
        AgentId, ProviderKind, ProviderProfileId, ProviderProfileStatus, ProviderSecretSetupState,
    };

    fn error_future<T: 'static>() -> BackendFuture<'static, T> {
        Box::pin(async { Err(BackendError::unsupported("mock", "unused mock operation")) })
    }

    #[derive(Clone)]
    struct MockManagementBackend {
        selected_profile: ProviderProfileSummary,
        selection_requests: Arc<Mutex<Vec<ManagementProfileSelectionRequest>>>,
    }

    impl ManagementBackend for MockManagementBackend {
        fn list_agents(
            &self,
            _request: AgentListRequest,
        ) -> BackendFuture<'_, vibex_core::AgentListResponse> {
            error_future()
        }

        fn list_profiles(&self) -> BackendFuture<'_, Vec<ProviderProfileSummary>> {
            error_future()
        }

        fn select_profile(
            &self,
            request: MutationRequest<ManagementProfileSelectionRequest>,
        ) -> BackendFuture<'_, ProviderProfileSummary> {
            let selected_profile = self.selected_profile.clone();
            let selection_requests = self.selection_requests.clone();
            Box::pin(async move {
                selection_requests
                    .lock()
                    .map_err(|_| BackendError::failed("mock", "mock selections poisoned"))?
                    .push(request.payload);
                Ok(selected_profile)
            })
        }

        fn health_summaries(&self) -> BackendFuture<'_, Vec<vibex_core::ProviderHealthSummary>> {
            error_future()
        }

        fn run_health_probes(
            &self,
            _request: MutationRequest<ProviderRunHealthProbesRequest>,
        ) -> BackendFuture<'_, ProviderRunHealthProbesResult> {
            error_future()
        }

        fn relay_status(&self) -> BackendFuture<'_, RelayStatusSummary> {
            error_future()
        }
    }

    struct UnsupportedDeviceBackend;

    impl DeviceBackend for UnsupportedDeviceBackend {
        fn create_pairing_offer(
            &self,
            _request: MutationRequest<vibex_core::RemoteCreatePairingCodeRequest>,
        ) -> BackendFuture<'_, vibex_core::RemoteCreatePairingCodeResponse> {
            error_future()
        }

        fn create_pairing_offer_v2(
            &self,
            _request: MutationRequest<RemoteCreatePairingOfferRequest>,
        ) -> BackendFuture<'_, RemoteCreatePairingOfferResponse> {
            error_future()
        }

        fn cancel_pairing_offer(
            &self,
            _request: MutationRequest<RemoteCancelPairingOfferRequest>,
        ) -> BackendFuture<'_, RemotePairingOfferSummary> {
            error_future()
        }

        fn list_devices(&self) -> BackendFuture<'_, Vec<RemoteDeviceDetail>> {
            error_future()
        }

        fn revoke_device(
            &self,
            _request: MutationRequest<RemoteRevokeDeviceRequest>,
        ) -> BackendFuture<'_, RemoteDeviceDetail> {
            error_future()
        }

        fn audit_records(
            &self,
            _request: RemoteAuditListRequest,
        ) -> BackendFuture<'_, Vec<RemoteAuditRecord>> {
            error_future()
        }
    }

    fn selected_profile() -> ProviderProfileSummary {
        ProviderProfileSummary {
            id: ProviderProfileId::new(),
            agent_id: AgentId::parse("codex").unwrap(),
            kind: ProviderKind::Acp,
            display_name: "Redacted profile".into(),
            status: ProviderProfileStatus::Enabled,
            account_alias: Some("work".into()),
            default_model: Some("gpt-5".into()),
            configured_models: Vec::new(),
            secret_setup_state: ProviderSecretSetupState::Available,
            updated_at_ms: 7,
        }
    }

    #[test]
    fn navigation_blocks_dirty_switch_until_explicit_discard() {
        let mut navigation = ManagementNavigation::default();
        navigation.mark_dirty(ManagementSection::Providers, true);
        navigation.active = ManagementSection::Providers;
        assert!(!navigation.switch(ManagementSection::Devices, false));
        assert!(navigation.switch(ManagementSection::Devices, true));
        assert_eq!(navigation.active, ManagementSection::Devices);
        assert!(!navigation.is_dirty(ManagementSection::Providers));
        assert_eq!(navigation.generation, 1);
    }

    #[test]
    fn capabilities_expose_only_the_reviewed_management_surface() {
        let snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        let capabilities = ManagementWorkflowCapabilities::from_backend(&snapshot);
        assert!(capabilities.supports(BackendOperation::ManagementProfiles));
        assert!(capabilities.supports(BackendOperation::ManagementProfileSelect));
        assert!(capabilities.supports(BackendOperation::DevicePairing));
        assert!(capabilities.supports(BackendOperation::DeviceRevoke));
        assert!(!capabilities.supports(BackendOperation::FileDelete));
    }

    #[test]
    fn capability_refresh_drops_permissions_removed_by_the_backend() {
        let management = Arc::new(MockManagementBackend {
            selected_profile: selected_profile(),
            selection_requests: Arc::new(Mutex::new(Vec::new())),
        });
        let mut snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        let mut controller = ManagementWorkflowController::new(
            management,
            Arc::new(UnsupportedDeviceBackend),
            ManagementWorkflowCapabilities::from_backend(&snapshot),
        );
        assert!(
            controller
                .capabilities
                .supports(BackendOperation::DevicePairing)
        );

        snapshot
            .device
            .operations
            .remove(&BackendOperation::DevicePairing);
        controller.set_capabilities(ManagementWorkflowCapabilities::from_backend(&snapshot));

        assert!(
            !controller
                .capabilities
                .supports(BackendOperation::DevicePairing)
        );
    }

    #[test]
    fn pairing_debug_never_contains_launch_fragment_or_challenge() {
        let projection = PairingOfferProjection {
            offer_id: "offer-1".into(),
            expires_at_ms: 10,
            permission_level: RemoteDevicePermissionLevel::ReadOnly,
            direct_candidate_count: 1,
            has_relay_candidate: false,
            granted_permission_count: 1,
            canceled: false,
            claimed_device_id: None,
            launch_fragment: Some("#/pair/secret-challenge".into()),
        };
        let debug = format!("{projection:?}");
        assert!(!debug.contains("secret-challenge"));
        assert!(debug.contains("has_launch_fragment"));
    }

    #[test]
    fn state_debug_is_metadata_only() {
        let state = ManagementWorkflowState {
            pairing_offer: Some(PairingOfferProjection {
                offer_id: "offer-secret".into(),
                expires_at_ms: 1,
                permission_level: RemoteDevicePermissionLevel::FullControl,
                direct_candidate_count: 0,
                has_relay_candidate: false,
                granted_permission_count: 1,
                canceled: false,
                claimed_device_id: None,
                launch_fragment: Some("private-fragment".into()),
            }),
            ..Default::default()
        };
        let debug = format!("{state:?}");
        assert!(!debug.contains("private-fragment"));
        assert!(debug.contains("has_pairing_offer"));
    }

    #[test]
    fn compact_management_view_has_explicit_touch_actions() {
        let view = ManagementWorkflowState::default().view(ShellKind::Compact);
        assert_eq!(view.presentation, crate::PanelPresentation::Sheet);
        assert_eq!(view.action_touch_target_px, MIN_TOUCH_TARGET_PX);
        assert!(!view.hover_required);
    }

    #[test]
    fn stale_management_operation_is_rejected() {
        let mut state = ManagementWorkflowState::default();
        let stale = state.next_operation();
        state.next_operation();

        let error = state.ensure_current_operation(stale).unwrap_err();

        assert_eq!(error.code, "management_operation_stale");
    }

    #[tokio::test]
    async fn profile_selection_uses_redacted_summary_and_updates_projection() {
        let selected_profile = selected_profile();
        let selection_requests = Arc::new(Mutex::new(Vec::new()));
        let management = Arc::new(MockManagementBackend {
            selected_profile: selected_profile.clone(),
            selection_requests: selection_requests.clone(),
        });
        let capabilities = ManagementWorkflowCapabilities::from_backend(
            &BackendCapabilitySnapshot::desktop_native_v1(),
        );
        let mut controller = ManagementWorkflowController::new(
            management,
            Arc::new(UnsupportedDeviceBackend),
            capabilities,
        );

        let projection = controller
            .select_profile(MutationRequest::new(ManagementProfileSelectionRequest {
                agent_id: selected_profile.agent_id.clone(),
                provider_profile_id: selected_profile.id.clone(),
            }))
            .await
            .unwrap();

        assert_eq!(projection.id, selected_profile.id.as_str());
        assert_eq!(projection.display_name, "Redacted profile");
        assert_eq!(controller.state.profiles, vec![projection]);
        assert_eq!(controller.state.last_operation_id, 1);
        let requests = selection_requests.lock().unwrap();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].provider_profile_id, selected_profile.id);
    }
}

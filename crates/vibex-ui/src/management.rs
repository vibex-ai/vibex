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
    AgentListRequest, AgentRuntimeProbeCancelRequest, AgentRuntimeProbeFactStatus,
    AgentRuntimeProbeListRequest, AgentRuntimeProbeRecord, AgentRuntimeProbeStartRequest,
    AgentRuntimeProbeStatus, ProviderHealthStatus, ProviderProfileSummary,
    ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult, RemoteAuditListRequest,
    RemoteAuditRecord, RemoteCancelPairingOfferRequest, RemoteCreatePairingOfferRequest,
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
pub struct AgentRuntimeProbeFactProjection {
    pub capability: vibex_core::AgentRuntimeProbeCapability,
    pub status: AgentRuntimeProbeFactStatus,
    pub diagnostic_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRuntimeProbeProjection {
    pub id: String,
    pub runtime_profile_id: String,
    pub binding_id: Option<String>,
    pub agent_id: String,
    pub adapter_id: String,
    pub descriptor_id: String,
    pub descriptor_version: String,
    pub status: AgentRuntimeProbeStatus,
    pub stage: vibex_core::AgentRuntimeProbeStage,
    pub facts: Vec<AgentRuntimeProbeFactProjection>,
    pub diagnostic_code: Option<String>,
    pub provider_projection_verified: bool,
    pub live_switch_verified: bool,
    pub revision: i64,
    pub updated_at_ms: i64,
    pub finished_at_ms: Option<i64>,
}

impl AgentRuntimeProbeProjection {
    pub fn from_record(record: &AgentRuntimeProbeRecord) -> Self {
        Self {
            id: record.id.as_str().to_string(),
            runtime_profile_id: record.request.runtime_profile_id.as_str().to_string(),
            binding_id: record
                .request
                .binding_id
                .as_ref()
                .map(|id| id.as_str().to_string()),
            agent_id: record.agent_id.as_str().to_string(),
            adapter_id: record.adapter_id.as_str().to_string(),
            descriptor_id: record.descriptor_id.as_str().to_string(),
            descriptor_version: record.descriptor_version.clone(),
            status: record.status,
            stage: record.stage,
            facts: record
                .facts
                .iter()
                .map(|fact| AgentRuntimeProbeFactProjection {
                    capability: fact.capability,
                    status: fact.status,
                    diagnostic_code: fact.diagnostic_code.clone(),
                })
                .collect(),
            diagnostic_code: record.diagnostic_code.clone(),
            provider_projection_verified: record
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.provider_projection_verified()),
            live_switch_verified: record
                .evidence
                .as_ref()
                .is_some_and(|evidence| evidence.live_switch_verified()),
            revision: record.revision,
            updated_at_ms: record.updated_at_ms,
            finished_at_ms: record.finished_at_ms,
        }
    }

    pub fn can_cancel(&self) -> bool {
        matches!(
            self.status,
            AgentRuntimeProbeStatus::Requested | AgentRuntimeProbeStatus::Running
        )
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProjectionCredentialSurface {
    ApiKey,
    OAuth,
    Cloud,
    AgentManaged,
    Local,
    ServiceMarketplace,
    Unsupported,
}

/// Shared Desktop/Web/Mobile state for a descriptor-driven Agent binding
/// editor. It contains no Secret value; `secret_touched` and `secret_clear`
/// are independent intent flags consumed only by the authoritative desktop.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AgentProviderBindingEditorState {
    pub capability: Option<vibex_core::AgentProviderProjectionCapability>,
    pub preview: Option<vibex_core::AgentProviderProjectionPreview>,
    pub draft_revision: u64,
    pub secret_touched: bool,
    pub secret_clear: bool,
}

impl AgentProviderBindingEditorState {
    pub fn replace_capability(
        &mut self,
        capability: vibex_core::AgentProviderProjectionCapability,
    ) {
        // Capability refreshes must not erase an in-progress draft or convert
        // an untouched blank Secret control into a clear mutation.
        self.capability = Some(capability);
    }

    pub fn replace_preview(&mut self, preview: vibex_core::AgentProviderProjectionPreview) {
        self.preview = Some(preview);
    }

    pub fn mark_draft_changed(&mut self) {
        self.draft_revision = self.draft_revision.saturating_add(1);
    }

    pub fn set_secret_intent(&mut self, touched: bool, clear: bool) {
        self.secret_touched = touched;
        self.secret_clear = touched && clear;
    }

    pub fn shows(&self, control: vibex_core::AgentProjectionFormControl) -> bool {
        self.capability
            .as_ref()
            .is_some_and(|capability| capability.form_controls.contains(&control))
    }

    pub fn credential_surface(&self) -> ProjectionCredentialSurface {
        use vibex_core::AgentProjectionFormControl as Control;
        if self.shows(Control::ApiKey) {
            ProjectionCredentialSurface::ApiKey
        } else if self.shows(Control::OAuth) {
            ProjectionCredentialSurface::OAuth
        } else if [
            Control::Aws,
            Control::Gcp,
            Control::Azure,
            Control::Snowflake,
        ]
        .into_iter()
        .any(|control| self.shows(control))
        {
            ProjectionCredentialSurface::Cloud
        } else if self.shows(Control::AgentManagedStatus) {
            ProjectionCredentialSurface::AgentManaged
        } else if self.shows(Control::LocalRuntime) {
            ProjectionCredentialSurface::Local
        } else if self.shows(Control::ServiceMarketplace) {
            ProjectionCredentialSurface::ServiceMarketplace
        } else {
            ProjectionCredentialSurface::Unsupported
        }
    }

    pub fn wire_api_choices(&self) -> Vec<vibex_core::ProviderModelWireApi> {
        let Some(capability) = self.capability.as_ref() else {
            return Vec::new();
        };
        capability
            .model_interfaces
            .iter()
            .filter(|interface| interface.user_selectable)
            .filter_map(|interface| {
                vibex_core::ProviderModelWireApi::from_wire_protocol_id(&interface.wire_protocol_id)
            })
            .collect()
    }

    pub fn supported_wire_apis(&self) -> Vec<vibex_core::ProviderModelWireApi> {
        let Some(capability) = self.capability.as_ref() else {
            return Vec::new();
        };
        capability
            .model_interfaces
            .iter()
            .filter_map(|interface| {
                vibex_core::ProviderModelWireApi::from_wire_protocol_id(&interface.wire_protocol_id)
            })
            .collect()
    }

    pub fn accepts_wire_api(&self, wire_api: vibex_core::ProviderModelWireApi) -> bool {
        self.capability.as_ref().is_some_and(|capability| {
            capability
                .model_interfaces
                .iter()
                .any(|interface| interface.wire_protocol_id == wire_api.wire_protocol_id())
        })
    }

    pub fn wire_api_integration_kind(
        &self,
        wire_api: vibex_core::ProviderModelWireApi,
    ) -> Option<vibex_core::AgentModelInterfaceIntegrationKind> {
        self.capability.as_ref().and_then(|capability| {
            capability
                .model_interfaces
                .iter()
                .find(|interface| interface.wire_protocol_id == wire_api.wire_protocol_id())
                .map(|interface| interface.integration_kind)
        })
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
    pub runtime_probes: Vec<AgentRuntimeProbeProjection>,
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
            .field("runtime_probe_count", &self.runtime_probes.len())
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
            runtime_probes: Vec::new(),
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
            ManagementRuntimeProbeRead,
            ManagementRuntimeProbeMutate,
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
            | BackendOperation::ManagementRuntimeProbeRead
            | BackendOperation::ManagementRuntimeProbeMutate
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
            | BackendOperation::ManagementRuntimeProbeRead
            | BackendOperation::ManagementRuntimeProbeMutate
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
        ManagementRuntimeProbeRead => "management_runtime_probe_read",
        ManagementRuntimeProbeMutate => "management_runtime_probe_mutate",
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
    pub runtime_probes: Vec<AgentRuntimeProbeProjection>,
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
            .field("runtime_probe_count", &self.runtime_probes.len())
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
            runtime_probes: self.runtime_probes.clone(),
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
                        .filter(|agent| vibex_core::is_user_visible_agent(&agent.id))
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
            .supports(BackendOperation::ManagementRuntimeProbeRead)
        {
            match self
                .management
                .list_agent_runtime_probes(AgentRuntimeProbeListRequest::default())
                .await
            {
                Ok(probes) if generation == self.state.generation => {
                    self.state.runtime_probes = probes
                        .iter()
                        .map(AgentRuntimeProbeProjection::from_record)
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
                && self.state.runtime_probes.is_empty()
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

    pub async fn run_agent_runtime_probe(
        &mut self,
        request: MutationRequest<AgentRuntimeProbeStartRequest>,
    ) -> BackendResult<AgentRuntimeProbeProjection> {
        self.capabilities
            .require(BackendOperation::ManagementRuntimeProbeMutate)?;
        let operation = self.state.next_operation();
        let result = self.management.start_agent_runtime_probe(request).await;
        self.state.ensure_current_operation(operation)?;
        match result {
            Ok(record) => {
                let projection = AgentRuntimeProbeProjection::from_record(&record);
                self.state
                    .runtime_probes
                    .retain(|probe| probe.id != projection.id);
                self.state.runtime_probes.insert(0, projection.clone());
                Ok(projection)
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
    }

    pub async fn cancel_agent_runtime_probe(
        &mut self,
        request: MutationRequest<AgentRuntimeProbeCancelRequest>,
    ) -> BackendResult<AgentRuntimeProbeProjection> {
        self.capabilities
            .require(BackendOperation::ManagementRuntimeProbeMutate)?;
        let operation = self.state.next_operation();
        let result = self.management.cancel_agent_runtime_probe(request).await;
        self.state.ensure_current_operation(operation)?;
        match result {
            Ok(record) => {
                let projection = AgentRuntimeProbeProjection::from_record(&record);
                self.state
                    .runtime_probes
                    .retain(|probe| probe.id != projection.id);
                self.state.runtime_probes.insert(0, projection.clone());
                Ok(projection)
            }
            Err(error) => {
                self.state.last_error = Some(error.clone());
                Err(error)
            }
        }
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
    use std::sync::atomic::{AtomicUsize, Ordering};

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
        runtime_probe_record: Option<vibex_core::AgentRuntimeProbeRecord>,
        runtime_probe_start_calls: Arc<AtomicUsize>,
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

        fn list_model_provider_profiles(
            &self,
        ) -> BackendFuture<'_, Vec<vibex_core::ModelProviderProfile>> {
            error_future()
        }

        fn create_model_provider_profile(
            &self,
            _request: MutationRequest<vibex_core::ModelProviderProfileCreateRequest>,
        ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
            error_future()
        }

        fn update_model_provider_profile(
            &self,
            _request: MutationRequest<vibex_core::ModelProviderProfileUpdateRequest>,
        ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
            error_future()
        }

        fn list_agent_runtime_profiles(
            &self,
            _agent_id: AgentId,
        ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProfile>> {
            error_future()
        }

        fn create_agent_runtime_profile(
            &self,
            _request: MutationRequest<vibex_core::AgentRuntimeProfileCreateRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
            error_future()
        }

        fn update_agent_runtime_profile(
            &self,
            _request: MutationRequest<vibex_core::AgentRuntimeProfileUpdateRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentRuntimeProfile> {
            error_future()
        }

        fn list_agent_model_provider_bindings(
            &self,
            _request: vibex_core::AgentModelProviderBindingListRequest,
        ) -> BackendFuture<'_, Vec<vibex_core::AgentModelProviderBinding>> {
            error_future()
        }

        fn create_agent_model_provider_binding(
            &self,
            _request: MutationRequest<vibex_core::AgentModelProviderBindingCreateRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
            error_future()
        }

        fn update_agent_model_provider_binding(
            &self,
            _request: MutationRequest<vibex_core::AgentModelProviderBindingUpdateRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentModelProviderBinding> {
            error_future()
        }

        fn agent_provider_projection_capability(
            &self,
            _request: vibex_core::AgentProviderProjectionCapabilityRequest,
        ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionCapability> {
            error_future()
        }

        fn preview_agent_provider_projection(
            &self,
            _request: vibex_core::AgentProviderProjectionPreviewRequest,
        ) -> BackendFuture<'_, vibex_core::AgentProviderProjectionPreview> {
            error_future()
        }

        fn start_agent_runtime_probe(
            &self,
            _request: MutationRequest<vibex_core::AgentRuntimeProbeStartRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
            let record = self.runtime_probe_record.clone();
            let calls = self.runtime_probe_start_calls.clone();
            Box::pin(async move {
                let record = record.ok_or_else(|| {
                    BackendError::unsupported("mock", "mock runtime probe is unavailable")
                })?;
                calls.fetch_add(1, Ordering::SeqCst);
                Ok(record)
            })
        }

        fn get_agent_runtime_probe(
            &self,
            probe_id: vibex_core::AgentRuntimeProbeId,
        ) -> BackendFuture<'_, Option<vibex_core::AgentRuntimeProbeRecord>> {
            let record = self.runtime_probe_record.clone();
            Box::pin(async move { Ok(record.filter(|record| record.id == probe_id)) })
        }

        fn list_agent_runtime_probes(
            &self,
            _request: vibex_core::AgentRuntimeProbeListRequest,
        ) -> BackendFuture<'_, Vec<vibex_core::AgentRuntimeProbeRecord>> {
            let record = self.runtime_probe_record.clone();
            Box::pin(async move { Ok(record.into_iter().collect()) })
        }

        fn cancel_agent_runtime_probe(
            &self,
            _request: MutationRequest<vibex_core::AgentRuntimeProbeCancelRequest>,
        ) -> BackendFuture<'_, vibex_core::AgentRuntimeProbeRecord> {
            let record = self.runtime_probe_record.clone();
            Box::pin(async move {
                let mut record = record.ok_or_else(|| {
                    BackendError::unsupported("mock", "mock runtime probe is unavailable")
                })?;
                record.status = vibex_core::AgentRuntimeProbeStatus::Cancelled;
                record.stage = vibex_core::AgentRuntimeProbeStage::Completed;
                Ok(record)
            })
        }

        fn mutate_provider_credential_secret(
            &self,
            _request: MutationRequest<vibex_core::ProviderCredentialSecretMutationRequest>,
        ) -> BackendFuture<'_, vibex_core::ModelProviderProfile> {
            error_future()
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

    fn runtime_probe_record() -> vibex_core::AgentRuntimeProbeRecord {
        let facts = [
            vibex_core::AgentRuntimeProbeCapability::BinaryIdentity,
            vibex_core::AgentRuntimeProbeCapability::AcpHandshake,
            vibex_core::AgentRuntimeProbeCapability::Authentication,
            vibex_core::AgentRuntimeProbeCapability::Session,
            vibex_core::AgentRuntimeProbeCapability::ModelSelection,
            vibex_core::AgentRuntimeProbeCapability::ProviderProjection,
            vibex_core::AgentRuntimeProbeCapability::Redaction,
        ]
        .into_iter()
        .map(vibex_core::AgentRuntimeProbeFact::passed)
        .collect::<Vec<_>>();
        let mut record = vibex_core::AgentRuntimeProbeRecord::requested(
            vibex_core::AgentRuntimeProbeId::new(),
            vibex_core::AgentRuntimeProbeRequest {
                runtime_profile_id: vibex_core::AgentRuntimeProfileId::new(),
                binding_id: Some(vibex_core::AgentModelProviderBindingId::new()),
                workspace_key: "workspace-secret-sentinel".to_string(),
                timeout_ms: vibex_core::MIN_PROBE_TIMEOUT_MS,
                minimal_prompt: false,
            },
            AgentId::parse("codex").unwrap(),
            vibex_core::AcpAdapterId::parse("codex-acp").unwrap(),
            vibex_core::AgentProviderProjectionDescriptorId::parse(
                vibex_core::CODEX_PROJECTION_DESCRIPTOR_ID,
            )
            .unwrap(),
            "1".to_string(),
            1,
        )
        .unwrap();
        record.status = vibex_core::AgentRuntimeProbeStatus::Passed;
        record.stage = vibex_core::AgentRuntimeProbeStage::Completed;
        record.facts = facts.clone();
        record.evidence = Some(vibex_core::AgentRuntimeProbeEvidence {
            schema_version: vibex_core::AGENT_RUNTIME_PROBE_SCHEMA_VERSION,
            agent_id: record.agent_id.clone(),
            agent_version: Some("native-session-id-sentinel".to_string()),
            adapter_id: record.adapter_id.clone(),
            adapter_version: Some("command-env-sentinel".to_string()),
            descriptor_id: record.descriptor_id.clone(),
            descriptor_version: record.descriptor_version.clone(),
            descriptor_match: vibex_core::ProjectionDescriptorMatch::Exact,
            projection_fingerprint: Some("sha256:0123456789abcdef".to_string()),
            source_revision: "secret_sentinel_revision".to_string(),
            platform_os: "linux".to_string(),
            platform_arch: "x86_64".to_string(),
            facts,
            switch_behavior: vibex_core::ProviderSwitchBehavior::RestartAndResume,
            source_survived_prepare_failure: false,
            redaction_passed: true,
            recorded_at_ms: 2,
        });
        record.finished_at_ms = Some(2);
        record.updated_at_ms = 2;
        record.validate().unwrap();
        record
    }

    fn projection_capability(
        controls: Vec<vibex_core::AgentProjectionFormControl>,
        model_interfaces: Vec<vibex_core::AgentModelInterfaceDescriptor>,
    ) -> vibex_core::AgentProviderProjectionCapability {
        vibex_core::AgentProviderProjectionCapability {
            schema_version: vibex_core::PROVIDER_PROJECTION_SCHEMA_VERSION,
            agent_id: AgentId::parse("codex").unwrap(),
            adapter_id: vibex_core::AcpAdapterId::parse("codex-acp").unwrap(),
            descriptor_id: Some(
                vibex_core::AgentProviderProjectionDescriptorId::parse(
                    vibex_core::CODEX_PROJECTION_DESCRIPTOR_ID,
                )
                .unwrap(),
            ),
            descriptor_version: "1".to_string(),
            detected_agent_version: Some("0.146.0".to_string()),
            detected_adapter_version: Some("1.1.9".to_string()),
            match_kind: vibex_core::ProjectionDescriptorMatch::Exact,
            evidence_state: vibex_core::ProjectionEvidenceState::Verified,
            auth_state: vibex_core::ProjectionAuthState::Ready,
            provider_control: vibex_core::AgentProviderControl::Unsupported,
            credential_control: vibex_core::AgentCredentialControl::Unsupported,
            model_control: vibex_core::AgentModelControl::Unsupported,
            credential_kinds: Vec::new(),
            model_interfaces,
            switch_behavior: vibex_core::ProviderSwitchBehavior::RestartAndResume,
            form_controls: controls,
            diagnostics: Vec::new(),
        }
    }

    #[test]
    fn binding_editor_maps_all_eight_typed_credentials_to_semantic_surfaces() {
        use vibex_core::AgentCredentialKind as Kind;
        use vibex_core::AgentProjectionFormControl as Control;

        for (kind, control, expected) in [
            (
                Kind::ApiKey,
                Control::ApiKey,
                ProjectionCredentialSurface::ApiKey,
            ),
            (
                Kind::OAuth,
                Control::OAuth,
                ProjectionCredentialSurface::OAuth,
            ),
            (Kind::Aws, Control::Aws, ProjectionCredentialSurface::Cloud),
            (Kind::Gcp, Control::Gcp, ProjectionCredentialSurface::Cloud),
            (
                Kind::Azure,
                Control::Azure,
                ProjectionCredentialSurface::Cloud,
            ),
            (
                Kind::Snowflake,
                Control::Snowflake,
                ProjectionCredentialSurface::Cloud,
            ),
            (
                Kind::Local,
                Control::LocalRuntime,
                ProjectionCredentialSurface::Local,
            ),
            (
                Kind::ManagedSubscription,
                Control::AgentManagedStatus,
                ProjectionCredentialSurface::AgentManaged,
            ),
        ] {
            let mut capability = projection_capability(vec![control], Vec::new());
            capability.credential_kinds = vec![kind];
            let mut editor = AgentProviderBindingEditorState::default();
            editor.replace_capability(capability);
            assert_eq!(editor.credential_surface(), expected, "{kind:?}");
        }
    }

    #[test]
    fn opencode_detected_range_uses_api_key_credential_surface() {
        let registry = vibex_core::AgentProviderProjectionRegistry::builtin().unwrap();
        let identity = vibex_core::AgentRuntimeVersionIdentity {
            route: vibex_core::AgentRuntimeRouteKey {
                agent_id: AgentId::parse("opencode").unwrap(),
                transport_kind: vibex_core::TransportKind::Acp,
                adapter_id: vibex_core::AcpAdapterId::parse("opencode-acp").unwrap(),
            },
            adapter_version: None,
            agent_version: Some(vibex_core::OPENCODE_LAST_VERIFIED_VERSION.to_string()),
            runtime_dependencies: std::collections::BTreeMap::new(),
            source: vibex_core::AgentVersionSource::Detected,
        };
        let resolution = registry.resolve(&identity).unwrap();
        assert_eq!(
            resolution.match_kind,
            vibex_core::ProjectionDescriptorMatch::SemverRange
        );
        let capability = vibex_core::AgentProviderProjectionCapability::from_resolution(
            &identity,
            &resolution,
            vibex_core::ProjectionAuthState::Missing,
        );
        let mut editor = AgentProviderBindingEditorState::default();
        editor.replace_capability(capability);

        assert_eq!(
            editor.credential_surface(),
            ProjectionCredentialSurface::ApiKey
        );
        assert!(editor.shows(vibex_core::AgentProjectionFormControl::ApiKey));
        assert!(editor.shows(vibex_core::AgentProjectionFormControl::Endpoint));
        assert!(editor.shows(vibex_core::AgentProjectionFormControl::Model));
    }

    #[test]
    fn runtime_probe_projection_is_display_safe_and_keeps_independent_facts() {
        let record = runtime_probe_record();
        let projection = AgentRuntimeProbeProjection::from_record(&record);
        let json = serde_json::to_string(&projection).unwrap();

        assert!(projection.provider_projection_verified);
        assert!(!projection.live_switch_verified);
        assert_eq!(projection.facts.len(), record.facts.len());
        assert!(projection.facts.iter().any(|fact| {
            fact.capability == vibex_core::AgentRuntimeProbeCapability::ProviderProjection
                && fact.status == vibex_core::AgentRuntimeProbeFactStatus::Passed
        }));
        for sentinel in [
            "workspace-secret-sentinel",
            "sha256:0123456789abcdef",
            "secret_sentinel_revision",
            "native-session-id-sentinel",
            "command-env-sentinel",
        ] {
            assert!(
                !json.contains(sentinel),
                "display projection leaked {sentinel}"
            );
        }
        assert!(!json.contains("workspaceKey"));
        assert!(!json.contains("projectionFingerprint"));
        assert!(!json.contains("sourceRevision"));
    }

    #[tokio::test]
    async fn runtime_probe_controller_checks_mutation_capability_before_dispatch() {
        let record = runtime_probe_record();
        let calls = Arc::new(AtomicUsize::new(0));
        let management = Arc::new(MockManagementBackend {
            selected_profile: selected_profile(),
            selection_requests: Arc::new(Mutex::new(Vec::new())),
            runtime_probe_record: Some(record.clone()),
            runtime_probe_start_calls: calls.clone(),
        });
        let mut snapshot = BackendCapabilitySnapshot::desktop_native_v1();
        snapshot
            .management
            .operations
            .remove(&BackendOperation::ManagementRuntimeProbeMutate);
        let mut controller = ManagementWorkflowController::new(
            management,
            Arc::new(UnsupportedDeviceBackend),
            ManagementWorkflowCapabilities::from_backend(&snapshot),
        );
        let request = AgentRuntimeProbeStartRequest {
            runtime_profile_id: record.request.runtime_profile_id.clone(),
            binding_id: record.request.binding_id.clone(),
            workspace_key: "runtime-probe-controller".to_string(),
            timeout_ms: vibex_core::MIN_PROBE_TIMEOUT_MS,
            minimal_prompt: false,
        };

        let error = controller
            .run_agent_runtime_probe(MutationRequest::new(request.clone()))
            .await
            .unwrap_err();
        assert_eq!(error.code, "management_runtime_probe_mutate_unsupported");
        assert_eq!(calls.load(Ordering::SeqCst), 0);

        controller.set_capabilities(ManagementWorkflowCapabilities::from_backend(
            &BackendCapabilitySnapshot::desktop_native_v1(),
        ));
        let projection = controller
            .run_agent_runtime_probe(MutationRequest::new(request))
            .await
            .unwrap();
        assert_eq!(projection.id, record.id.as_str());
        assert_eq!(calls.load(Ordering::SeqCst), 1);
        assert_eq!(controller.state.runtime_probes, vec![projection]);
    }

    #[test]
    fn codex_binding_editor_accepts_responses_and_rejects_chat() {
        let capability = projection_capability(
            vec![vibex_core::AgentProjectionFormControl::Model],
            vec![vibex_core::AgentModelInterfaceDescriptor {
                wire_protocol_id: vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
                sdk_adapter_id: None,
                transport: "https".to_string(),
                integration_kind: vibex_core::AgentModelInterfaceIntegrationKind::Direct,
                user_selectable: false,
                process_scoped: false,
            }],
        );
        let mut editor = AgentProviderBindingEditorState::default();
        editor.replace_capability(capability);

        assert!(editor.accepts_wire_api(vibex_core::ProviderModelWireApi::OpenaiResponses));
        assert!(!editor.accepts_wire_api(vibex_core::ProviderModelWireApi::OpenaiChatCompletions));
        assert!(editor.wire_api_choices().is_empty());
    }

    #[test]
    fn binding_editor_exposes_google_and_bedrock_direct_interfaces() {
        let capability = projection_capability(
            vec![vibex_core::AgentProjectionFormControl::WireProtocol],
            vec![
                vibex_core::AgentModelInterfaceDescriptor {
                    wire_protocol_id: vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI.to_string(),
                    sdk_adapter_id: Some("@ai-sdk/google".to_string()),
                    transport: "https".to_string(),
                    integration_kind: vibex_core::AgentModelInterfaceIntegrationKind::Direct,
                    user_selectable: true,
                    process_scoped: true,
                },
                vibex_core::AgentModelInterfaceDescriptor {
                    wire_protocol_id: vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE.to_string(),
                    sdk_adapter_id: Some("@ai-sdk/amazon-bedrock".to_string()),
                    transport: "https".to_string(),
                    integration_kind: vibex_core::AgentModelInterfaceIntegrationKind::Direct,
                    user_selectable: true,
                    process_scoped: true,
                },
            ],
        );
        let mut editor = AgentProviderBindingEditorState::default();
        editor.replace_capability(capability);

        assert_eq!(
            editor.wire_api_choices(),
            vec![
                vibex_core::ProviderModelWireApi::GoogleGenerativeAi,
                vibex_core::ProviderModelWireApi::AwsBedrockConverse,
            ]
        );
        assert_eq!(editor.supported_wire_apis(), editor.wire_api_choices());
        assert_eq!(
            editor.wire_api_integration_kind(vibex_core::ProviderModelWireApi::GoogleGenerativeAi),
            Some(vibex_core::AgentModelInterfaceIntegrationKind::Direct)
        );
    }

    #[test]
    fn capability_refresh_preserves_draft_and_secret_intent() {
        let mut editor = AgentProviderBindingEditorState::default();
        editor.mark_draft_changed();
        editor.mark_draft_changed();
        editor.set_secret_intent(true, true);
        editor.replace_capability(projection_capability(
            vec![vibex_core::AgentProjectionFormControl::ApiKey],
            Vec::new(),
        ));

        assert_eq!(editor.draft_revision, 2);
        assert!(editor.secret_touched);
        assert!(editor.secret_clear);
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
            runtime_probe_record: None,
            runtime_probe_start_calls: Arc::new(AtomicUsize::new(0)),
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
            runtime_probe_record: None,
            runtime_probe_start_calls: Arc::new(AtomicUsize::new(0)),
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

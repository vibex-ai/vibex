//! Typed management facades used by the GPUI desktop shell.
//!
//! The GPUI surface deliberately talks to these methods instead of opening a
//! database or touching native configuration files itself. The methods are
//! small projections over the existing repositories/services so the existing
//! scheduler, automation runner, Relay, diagnostics, and backup semantics stay
//! authoritative.

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use serde::Serialize;
use sha2::{Digest, Sha256};
use url::Url;
use vibex_agent::AutomationGraphRunner;
use vibex_backup::{
    BackupCreateRequest, BackupCreateResult, BackupInspection, BackupRestoreRequest,
    BackupRestoreResult, create_backup, inspect_backup, restore_backup,
};
use vibex_core::{
    AutomationGraph, AutomationGraphCreateRequest, AutomationGraphDefinitionUpdateRequest,
    AutomationGraphId, AutomationGraphListRequest, AutomationGraphUpdateRequest, AutomationRun,
    AutomationRunCancelRequest, AutomationRunListRequest, AutomationRunResumeRequest,
    AutomationRunStartRequest, AutomationRunStep, AutomationRunStepListRequest, DiagnosticBundle,
    DiagnosticBundleRequest, RemoteAuditListRequest, RemoteAuditRecord,
    RemoteCancelPairingOfferRequest, RemoteCreatePairingCodeRequest,
    RemoteCreatePairingCodeResponse, RemoteCreatePairingOfferRequest,
    RemoteCreatePairingOfferResponse, RemoteDeviceDetail, RemotePairingOfferSummary,
    RemoteRevokeDeviceRequest, ScheduledTask, ScheduledTaskAttentionListRequest,
    ScheduledTaskAttentionSummary, ScheduledTaskAuditListRequest, ScheduledTaskAuditRecord,
    ScheduledTaskCreateRequest, ScheduledTaskId, ScheduledTaskListRequest, ScheduledTaskRun,
    ScheduledTaskRunListRequest, ScheduledTaskUpdateRequest, VibexError, VibexResult,
};
use vibex_db::{
    AutomationGraphRepository, RemoteAuditRepository, RemoteDeviceRepository,
    ScheduledTaskRepository, apply_migrations, open_database,
};
use vibex_diagnostics::assert_no_sensitive_sentinels;
use vibex_remote::RemoteTrustService;

use crate::{AutomationHandle, BackupHandle, DiagnosticsHandle, ProviderHandle, ScheduledHandle};

/// Provider/config-switch operations exposed as a typed desktop boundary.
/// The GPUI layer can use this facade without depending on the config-switch
/// implementation type or opening a database itself.
#[derive(Clone)]
pub struct ProviderManagementFacade {
    service: vibex_config_switch::ProviderConfigService,
    runtime_probe: vibex_agent_acp::AgentRuntimeProbeService,
    mutation_guard: ManagementMutationGuard,
}

impl ProviderManagementFacade {
    pub fn list_agents(
        &self,
        request: vibex_core::AgentListRequest,
    ) -> VibexResult<vibex_core::AgentListResponse> {
        self.service.list_agents(request)
    }

    pub fn create_custom_agent(
        &self,
        request: vibex_core::CustomAgentCreateRequest,
    ) -> VibexResult<vibex_core::AgentSnapshotEntry> {
        let _claim = self
            .mutation_guard
            .claim(format!("agent:custom:create:{}", request.agent_id))?;
        self.service.create_custom_agent(request)
    }

    pub fn delete_custom_agent(
        &self,
        request: vibex_core::CustomAgentDeleteRequest,
    ) -> VibexResult<()> {
        let _claim = self
            .mutation_guard
            .claim(format!("agent:custom:delete:{}", request.agent_id))?;
        self.service.delete_custom_agent(request)
    }

    pub fn refresh_detected_agent_versions(&self) -> VibexResult<usize> {
        self.service.refresh_detected_agent_versions()
    }

    pub fn list_agent_catalog(&self) -> VibexResult<vibex_core::AgentCatalogListResponse> {
        self.service.list_agent_catalog()
    }

    pub fn update_agent_config(
        &self,
        request: vibex_core::AgentUpdateConfigRequest,
    ) -> VibexResult<vibex_core::AgentSnapshotEntry> {
        let _claim = self
            .mutation_guard
            .claim(format!("agent:update:{}", request.agent_id))?;
        self.service.update_agent_config(request)
    }

    pub fn refresh_agent_snapshot(
        &self,
        request: vibex_core::AgentRefreshSnapshotRequest,
    ) -> VibexResult<vibex_core::AgentRefreshSnapshotResponse> {
        self.service.refresh_agent_snapshot(request)
    }

    pub fn list_profiles(&self) -> VibexResult<Vec<vibex_core::ProviderProfile>> {
        self.service.list_profiles()
    }

    pub fn update_agent_auth_environment(
        &self,
        request: vibex_core::AgentAuthEnvironmentUpdateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "agent:auth-env:{}:{}",
            request.agent_id, request.provider_profile_id
        ))?;
        self.service.update_agent_auth_environment(request)
    }

    pub fn list_model_provider_profiles(
        &self,
    ) -> VibexResult<Vec<vibex_core::ModelProviderProfile>> {
        self.service.list_model_provider_profiles()
    }

    pub fn create_model_provider_profile(
        &self,
        request: vibex_core::ModelProviderProfileCreateRequest,
    ) -> VibexResult<vibex_core::ModelProviderProfile> {
        let _claim = self.mutation_guard.claim("provider:model-profile:create")?;
        self.service.create_model_provider_profile(request)
    }

    pub fn update_model_provider_profile(
        &self,
        request: vibex_core::ModelProviderProfileUpdateRequest,
    ) -> VibexResult<vibex_core::ModelProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:model-profile:update:{}",
            request.profile.id
        ))?;
        self.service.update_model_provider_profile(request)
    }

    pub fn list_agent_runtime_profiles(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> VibexResult<Vec<vibex_core::AgentRuntimeProfile>> {
        self.service.list_agent_runtime_profiles(agent_id)
    }

    pub fn create_agent_runtime_profile(
        &self,
        request: vibex_core::AgentRuntimeProfileCreateRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProfile> {
        let _claim = self.mutation_guard.claim("provider:agent-runtime:create")?;
        self.service.create_agent_runtime_profile(request)
    }

    pub fn update_agent_runtime_profile(
        &self,
        request: vibex_core::AgentRuntimeProfileUpdateRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-runtime:update:{}",
            request.profile.id
        ))?;
        self.service.update_agent_runtime_profile(request)
    }

    pub fn list_agent_model_provider_bindings(
        &self,
        request: vibex_core::AgentModelProviderBindingListRequest,
    ) -> VibexResult<Vec<vibex_core::AgentModelProviderBinding>> {
        self.service.list_agent_model_provider_bindings(request)
    }

    pub fn create_agent_model_provider_binding(
        &self,
        request: vibex_core::AgentModelProviderBindingCreateRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderBinding> {
        let _claim = self.mutation_guard.claim("provider:agent-binding:create")?;
        self.service.create_agent_model_provider_binding(request)
    }

    pub fn update_agent_model_provider_binding(
        &self,
        request: vibex_core::AgentModelProviderBindingUpdateRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderBinding> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-binding:update:{}",
            request.binding.id
        ))?;
        self.service.update_agent_model_provider_binding(request)
    }

    pub fn agent_provider_projection_capability(
        &self,
        request: vibex_core::AgentProviderProjectionCapabilityRequest,
    ) -> VibexResult<vibex_core::AgentProviderProjectionCapability> {
        self.service.agent_provider_projection_capability(request)
    }

    pub fn preview_agent_provider_projection(
        &self,
        request: vibex_core::AgentProviderProjectionPreviewRequest,
    ) -> VibexResult<vibex_core::AgentProviderProjectionPreview> {
        self.service.preview_agent_provider_projection(request)
    }

    pub fn start_agent_runtime_probe(
        &self,
        request: vibex_core::AgentRuntimeProbeStartRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProbeRecord> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:runtime-probe:start:{}",
            request.runtime_profile_id
        ))?;
        let record = self.runtime_probe.request(request)?;
        self.runtime_probe.spawn(record.id.clone())?;
        Ok(record)
    }

    pub fn get_agent_runtime_probe(
        &self,
        probe_id: &vibex_core::AgentRuntimeProbeId,
    ) -> VibexResult<Option<vibex_core::AgentRuntimeProbeRecord>> {
        self.runtime_probe.get(probe_id)
    }

    pub fn list_agent_runtime_probes(
        &self,
        request: vibex_core::AgentRuntimeProbeListRequest,
    ) -> VibexResult<Vec<vibex_core::AgentRuntimeProbeRecord>> {
        self.runtime_probe.list(request)
    }

    pub fn cancel_agent_runtime_probe(
        &self,
        request: vibex_core::AgentRuntimeProbeCancelRequest,
    ) -> VibexResult<vibex_core::AgentRuntimeProbeRecord> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:runtime-probe:cancel:{}",
            request.probe_id
        ))?;
        self.runtime_probe
            .cancel(&request.probe_id, request.expected_revision)
    }

    pub fn mutate_provider_credential_secret(
        &self,
        request: vibex_core::ProviderCredentialSecretMutationRequest,
    ) -> VibexResult<vibex_core::ModelProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:credential-secret:{}",
            request.model_provider_profile_id
        ))?;
        self.service.mutate_provider_credential_secret(request)
    }

    pub fn get_profile(
        &self,
        provider_profile_id: &vibex_core::ProviderProfileId,
    ) -> VibexResult<Option<vibex_core::ProviderProfile>> {
        self.service.get_profile(provider_profile_id)
    }

    pub fn list_agent_model_provider_profiles(
        &self,
        request: vibex_core::AgentModelProviderProfileListRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderProfileListResponse> {
        self.service.list_agent_model_provider_profiles(request)
    }

    pub fn create_agent_model_provider_profile(
        &self,
        request: vibex_core::AgentModelProviderProfileCreateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim("provider:agent-profile:create")?;
        self.service.create_agent_model_provider_profile(request)
    }

    pub fn update_agent_model_provider_profile(
        &self,
        request: vibex_core::AgentModelProviderProfileUpdateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:update:{}",
            request.provider_profile_id
        ))?;
        self.service.update_agent_model_provider_profile(request)
    }

    pub fn delete_agent_model_provider_profile(
        &self,
        request: vibex_core::AgentModelProviderProfileDeleteRequest,
    ) -> VibexResult<()> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:delete:{}",
            request.provider_profile_id
        ))?;
        self.service.delete_agent_model_provider_profile(request)
    }

    pub fn fetch_agent_model_provider_profile_models(
        &self,
        request: vibex_core::AgentModelProviderProfileFetchModelsRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderProfileFetchModelsResponse> {
        self.service
            .fetch_agent_model_provider_profile_models(request)
    }

    pub fn test_agent_model_provider_profile(
        &self,
        request: vibex_core::AgentModelProviderProfileTestRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderProfileTestResult> {
        self.service.test_agent_model_provider_profile(request)
    }

    pub fn get_agent_model_provider_profile_secret_value(
        &self,
        request: vibex_core::AgentModelProviderProfileSecretValueRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderProfileSecretValueResponse> {
        self.service
            .get_agent_model_provider_profile_secret_value(request)
    }

    pub fn update_agent_model_provider_profile_secret_value(
        &self,
        request: vibex_core::AgentModelProviderProfileSecretValueUpdateRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderProfileSecretValueResponse> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:secret:{}",
            request.provider_profile_id
        ))?;
        self.service
            .update_agent_model_provider_profile_secret_value(request)
    }

    pub fn get_agent_model_provider_default(
        &self,
        request: vibex_core::AgentModelProviderDefaultRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderDefaultSelection> {
        self.service.get_agent_model_provider_default(request)
    }

    pub fn set_agent_model_provider_default(
        &self,
        request: vibex_core::AgentModelProviderSetDefaultRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderDefaultSelection> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:default:{}",
            request.provider_profile_id
        ))?;
        self.service.set_agent_model_provider_default(request)
    }

    pub fn get_agent_model_provider_failover(
        &self,
        request: vibex_core::AgentModelProviderFailoverListRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderFailoverListResponse> {
        self.service.get_agent_model_provider_failover(request)
    }

    pub fn get_agent_model_provider_display_order(
        &self,
        request: vibex_core::AgentModelProviderDisplayOrderListRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderDisplayOrderListResponse> {
        self.service.get_agent_model_provider_display_order(request)
    }

    pub fn set_agent_model_provider_display_order(
        &self,
        request: vibex_core::AgentModelProviderDisplayOrderSetRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderDisplayOrderSetResponse> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:display-order:{}",
            request.agent_id
        ))?;
        self.service.set_agent_model_provider_display_order(request)
    }

    pub fn set_agent_model_provider_failover(
        &self,
        request: vibex_core::AgentModelProviderFailoverSetRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderFailoverListResponse> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:agent-profile:failover:{}",
            request.agent_id
        ))?;
        self.service.set_agent_model_provider_failover(request)
    }

    pub fn list_acp_catalog_presets(
        &self,
    ) -> VibexResult<vibex_core::AcpProviderCatalogListResponse> {
        self.service.list_acp_catalog_presets()
    }

    pub fn create_acp_profile(
        &self,
        request: vibex_core::AcpProviderProfileCreateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim("provider:acp-profile:create")?;
        self.service.create_acp_profile(request)
    }

    pub fn get_acp_profile_config(
        &self,
        provider_profile_id: vibex_core::ProviderProfileId,
    ) -> VibexResult<vibex_core::AcpProviderConfig> {
        self.service.get_acp_profile_config(provider_profile_id)
    }

    pub fn update_acp_profile_config(
        &self,
        request: vibex_core::AcpProviderProfileUpdateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:acp-profile:config:{}",
            request.provider_profile_id
        ))?;
        self.service.update_acp_profile_config(request)
    }

    pub fn preview_injection(
        &self,
        request: vibex_core::ProviderInjectionPreviewRequest,
    ) -> VibexResult<vibex_core::ProviderInjectionPreview> {
        self.service.preview_injection(request)
    }

    pub fn create_profile(
        &self,
        request: vibex_core::ProviderProfileCreateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim("provider:profile:create")?;
        self.service.create_profile(request)
    }

    pub fn update_profile(
        &self,
        request: vibex_core::ProviderProfileUpdateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:profile:update:{}",
            request.provider_profile_id
        ))?;
        self.service.update_profile(request)
    }

    pub fn duplicate_profile(
        &self,
        request: vibex_core::ProviderProfileDuplicateRequest,
    ) -> VibexResult<vibex_core::ProviderProfile> {
        let _claim = self.mutation_guard.claim("provider:profile:duplicate")?;
        self.service.duplicate_profile(request)
    }

    pub fn delete_profile(
        &self,
        request: vibex_core::ProviderProfileDeleteRequest,
    ) -> VibexResult<()> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:profile:delete:{}",
            request.provider_profile_id
        ))?;
        self.service.delete_profile(request)
    }

    pub fn list_mcp_servers(&self) -> VibexResult<Vec<vibex_core::McpServer>> {
        self.service.list_mcp_servers()
    }

    pub fn create_mcp_server(
        &self,
        request: vibex_core::McpServerCreateRequest,
    ) -> VibexResult<vibex_core::McpServer> {
        let _claim = self.mutation_guard.claim("provider:mcp:create")?;
        self.service.create_mcp_server(request)
    }

    pub fn update_mcp_server(
        &self,
        request: vibex_core::McpServerUpdateRequest,
    ) -> VibexResult<vibex_core::McpServer> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:mcp:update:{}", request.mcp_server_id))?;
        self.service.update_mcp_server(request)
    }

    pub fn delete_mcp_server(
        &self,
        request: vibex_core::McpServerDeleteRequest,
    ) -> VibexResult<()> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:mcp:delete:{}", request.mcp_server_id))?;
        self.service.delete_mcp_server(request)
    }

    pub fn set_mcp_server_provider_matrix(
        &self,
        request: vibex_core::McpServerSetProviderMatrixRequest,
    ) -> VibexResult<vibex_core::McpServer> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:mcp:provider-matrix:{}",
            request.mcp_server_id
        ))?;
        self.service.set_mcp_server_provider_matrix(request)
    }

    pub fn set_mcp_server_agent_matrix(
        &self,
        request: vibex_core::McpServerSetAgentMatrixRequest,
    ) -> VibexResult<vibex_core::McpServer> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:mcp:agent-matrix:{}",
            request.mcp_server_id
        ))?;
        self.service.set_mcp_server_agent_matrix(request)
    }

    pub fn list_mcp_server_agent_matrix(
        &self,
        request: vibex_core::McpServerAgentMatrixListRequest,
    ) -> VibexResult<Vec<vibex_core::McpServerAgentMatrix>> {
        self.service.list_mcp_server_agent_matrix(request)
    }

    pub fn list_mcp_servers_for_agent(
        &self,
        request: vibex_core::McpServerForAgentListRequest,
    ) -> VibexResult<Vec<vibex_core::McpServer>> {
        self.service.list_mcp_servers_for_agent(request)
    }

    pub fn discover_mcp_sources(
        &self,
        request: vibex_core::McpServerDiscoverRequest,
    ) -> VibexResult<vibex_core::McpServerDiscoveryResponse> {
        self.service.discover_mcp_sources(request)
    }

    pub fn import_mcp_servers(
        &self,
        request: vibex_core::McpServerImportRequest,
    ) -> VibexResult<vibex_core::McpServerImportResult> {
        let _claim = self.mutation_guard.claim("provider:mcp:import")?;
        self.service.import_mcp_servers(request)
    }

    pub fn validate_mcp_server(
        &self,
        request: vibex_core::McpServerValidateRequest,
    ) -> VibexResult<vibex_core::McpServerValidationResult> {
        self.service.validate_mcp_server(request)
    }

    pub fn list_skills(&self) -> VibexResult<Vec<vibex_core::Skill>> {
        self.service.list_skills()
    }

    pub fn create_skill(
        &self,
        request: vibex_core::SkillCreateRequest,
    ) -> VibexResult<vibex_core::Skill> {
        let _claim = self.mutation_guard.claim("provider:skill:create")?;
        self.service.create_skill(request)
    }

    pub fn update_skill(
        &self,
        request: vibex_core::SkillUpdateRequest,
    ) -> VibexResult<vibex_core::Skill> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:skill:update:{}", request.skill_id))?;
        self.service.update_skill(request)
    }

    pub fn delete_skill(&self, request: vibex_core::SkillDeleteRequest) -> VibexResult<()> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:skill:delete:{}", request.skill_id))?;
        self.service.delete_skill(request)
    }

    pub fn set_skill_provider_matrix(
        &self,
        request: vibex_core::SkillSetProviderMatrixRequest,
    ) -> VibexResult<vibex_core::Skill> {
        let _claim = self.mutation_guard.claim(format!(
            "provider:skill:provider-matrix:{}",
            request.skill_id
        ))?;
        self.service.set_skill_provider_matrix(request)
    }

    pub fn set_skill_agent_matrix(
        &self,
        request: vibex_core::SkillSetAgentMatrixRequest,
    ) -> VibexResult<vibex_core::Skill> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:skill:agent-matrix:{}", request.skill_id))?;
        self.service.set_skill_agent_matrix(request)
    }

    pub fn list_skill_agent_matrix(
        &self,
        request: vibex_core::SkillAgentMatrixListRequest,
    ) -> VibexResult<Vec<vibex_core::SkillAgentMatrix>> {
        self.service.list_skill_agent_matrix(request)
    }

    pub fn list_skills_for_agent(
        &self,
        request: vibex_core::SkillForAgentListRequest,
    ) -> VibexResult<Vec<vibex_core::Skill>> {
        self.service.list_skills_for_agent(request)
    }

    pub fn discover_skill_sources(
        &self,
        request: vibex_core::SkillDiscoverRequest,
    ) -> VibexResult<vibex_core::SkillDiscoveryResponse> {
        self.service.discover_skill_sources(request)
    }

    pub fn import_skills(
        &self,
        request: vibex_core::SkillImportRequest,
    ) -> VibexResult<vibex_core::SkillImportResult> {
        let _claim = self.mutation_guard.claim("provider:skill:import")?;
        self.service.import_skills(request)
    }

    pub fn validate_skill(
        &self,
        request: vibex_core::SkillValidateRequest,
    ) -> VibexResult<vibex_core::SkillValidationResult> {
        self.service.validate_skill(request)
    }

    pub fn list_prompts(&self) -> VibexResult<Vec<vibex_core::Prompt>> {
        self.service.list_prompts()
    }

    pub fn create_prompt(
        &self,
        request: vibex_core::PromptCreateRequest,
    ) -> VibexResult<vibex_core::Prompt> {
        let _claim = self.mutation_guard.claim("provider:prompt:create")?;
        self.service.create_prompt(request)
    }

    pub fn update_prompt(
        &self,
        request: vibex_core::PromptUpdateRequest,
    ) -> VibexResult<vibex_core::Prompt> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:prompt:update:{}", request.prompt_id))?;
        self.service.update_prompt(request)
    }

    pub fn delete_prompt(&self, request: vibex_core::PromptDeleteRequest) -> VibexResult<()> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:prompt:delete:{}", request.prompt_id))?;
        self.service.delete_prompt(request)
    }

    pub fn validate_prompt(
        &self,
        request: vibex_core::PromptValidateRequest,
    ) -> VibexResult<vibex_core::PromptValidationResult> {
        self.service.validate_prompt(request)
    }

    pub fn list_hooks(&self) -> VibexResult<Vec<vibex_core::Hook>> {
        self.service.list_hooks()
    }

    pub fn create_hook(
        &self,
        request: vibex_core::HookCreateRequest,
    ) -> VibexResult<vibex_core::Hook> {
        let _claim = self.mutation_guard.claim("provider:hook:create")?;
        self.service.create_hook(request)
    }

    pub fn update_hook(
        &self,
        request: vibex_core::HookUpdateRequest,
    ) -> VibexResult<vibex_core::Hook> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:hook:update:{}", request.hook_id))?;
        self.service.update_hook(request)
    }

    pub fn delete_hook(&self, request: vibex_core::HookDeleteRequest) -> VibexResult<()> {
        let _claim = self
            .mutation_guard
            .claim(format!("provider:hook:delete:{}", request.hook_id))?;
        self.service.delete_hook(request)
    }

    pub fn preview_hook_install(
        &self,
        request: vibex_core::HookInstallPreviewRequest,
    ) -> VibexResult<vibex_core::HookInstallPreview> {
        let _claim = self.mutation_guard.claim("provider:hook:preview-install")?;
        self.service.preview_hook_install(request)
    }

    pub fn list_health_summaries(&self) -> VibexResult<Vec<vibex_core::ProviderHealthSummary>> {
        self.service.list_health_summaries()
    }

    pub fn run_health_probes(
        &self,
        request: vibex_core::ProviderRunHealthProbesRequest,
    ) -> VibexResult<vibex_core::ProviderRunHealthProbesResult> {
        let _claim = self.mutation_guard.claim("provider:health:probe")?;
        self.service.run_health_probes(request)
    }

    pub fn list_capability_summaries(
        &self,
    ) -> VibexResult<Vec<vibex_core::ProviderCapabilitySummary>> {
        self.service.list_capability_summaries()
    }

    pub fn run_capability_probes(
        &self,
        request: vibex_core::ProviderRunCapabilityProbesRequest,
    ) -> VibexResult<vibex_core::ProviderRunCapabilityProbesResult> {
        let _claim = self.mutation_guard.claim("provider:capability:probe")?;
        self.service.run_capability_probes(request)
    }

    pub fn list_usage_summaries(
        &self,
        request: vibex_core::ProviderUsageListRequest,
    ) -> VibexResult<Vec<vibex_core::ProviderUsageSummary>> {
        self.service.list_usage_summaries(request)
    }

    pub fn list_failover_recommendations(
        &self,
        request: vibex_core::ProviderFailoverRecommendationRequest,
    ) -> VibexResult<Vec<vibex_core::ProviderFailoverRecommendation>> {
        self.service.list_failover_recommendations(request)
    }

    pub fn preview_native_import(
        &self,
        request: vibex_core::ProviderNativeImportPreviewRequest,
    ) -> VibexResult<vibex_core::ProviderNativeImportPreview> {
        self.service.preview_native_import(request)
    }

    pub fn create_profile_from_import(
        &self,
        request: vibex_core::ProviderNativeImportCreateRequest,
    ) -> VibexResult<vibex_core::ProviderNativeImportCreateResult> {
        let _claim = self.mutation_guard.claim("provider:native-import:create")?;
        self.service.create_profile_from_import(request)
    }

    pub fn preview_native_export(
        &self,
        request: vibex_core::ProviderNativeExportPreviewRequest,
    ) -> VibexResult<vibex_core::ProviderNativeExportPreview> {
        self.service.preview_native_export(request)
    }

    pub fn apply_native_export(
        &self,
        request: vibex_core::ProviderNativeExportApplyRequest,
    ) -> VibexResult<vibex_core::ProviderNativeExportApplyResult> {
        let _claim = self.mutation_guard.claim("provider:native-export:apply")?;
        self.service.apply_native_export(request)
    }

    pub fn rollback_native_export(
        &self,
        request: vibex_core::ProviderNativeExportRollbackRequest,
    ) -> VibexResult<vibex_core::ProviderNativeExportRollbackResult> {
        let _claim = self
            .mutation_guard
            .claim("provider:native-export:rollback")?;
        self.service.rollback_native_export(request)
    }

    pub fn list_native_exports(
        &self,
        request: vibex_core::ProviderNativeExportListRequest,
    ) -> VibexResult<Vec<vibex_core::ProviderNativeExportRecordSummary>> {
        self.service.list_native_exports(request)
    }
}

impl ProviderHandle {
    pub fn management(&self) -> ProviderManagementFacade {
        ProviderManagementFacade {
            service: self.service(),
            runtime_probe: self.runtime_probe_service(),
            mutation_guard: self.mutation_guard.clone(),
        }
    }
}

/// A tiny process-local idempotency guard for actions that can be triggered by
/// several UI surfaces at once. Durable repository transactions remain the
/// source of truth; this guard only coalesces duplicate local clicks.
#[derive(Clone, Default)]
pub struct ManagementMutationGuard {
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl ManagementMutationGuard {
    pub fn claim(&self, key: impl Into<String>) -> VibexResult<ManagementMutationClaim> {
        let key = key.into();
        let mut active = self.active.lock().map_err(|_| {
            VibexError::process(
                "management_mutation_guard_unavailable",
                "management mutation guard is unavailable",
            )
        })?;
        if !active.insert(key.clone()) {
            return Err(VibexError::conflict(
                "management_mutation_in_progress",
                "the same management mutation is already in progress",
            ));
        }
        Ok(ManagementMutationClaim {
            key,
            active: self.active.clone(),
        })
    }
}

#[derive(Debug)]
pub struct ManagementMutationClaim {
    key: String,
    active: Arc<Mutex<BTreeSet<String>>>,
}

impl Drop for ManagementMutationClaim {
    fn drop(&mut self) {
        if let Ok(mut active) = self.active.lock() {
            active.remove(&self.key);
        }
    }
}

fn migrated(path: &Path) -> VibexResult<rusqlite::Connection> {
    let mut connection = open_database(path)?;
    apply_migrations(&mut connection)?;
    Ok(connection)
}

impl ScheduledHandle {
    pub fn list(&self, request: ScheduledTaskListRequest) -> VibexResult<Vec<ScheduledTask>> {
        ScheduledTaskRepository::list(&migrated(&self.db_path)?, request)
    }

    pub fn create(&self, request: ScheduledTaskCreateRequest) -> VibexResult<ScheduledTask> {
        let _claim = self.mutation_guard.claim("scheduled:create")?;
        ScheduledTaskRepository::create(&migrated(&self.db_path)?, request)
    }

    pub fn update(&self, request: ScheduledTaskUpdateRequest) -> VibexResult<ScheduledTask> {
        let _claim = self
            .mutation_guard
            .claim(format!("scheduled:update:{}", request.id))?;
        ScheduledTaskRepository::update(&migrated(&self.db_path)?, request)
    }

    pub fn pause(&self, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        let _claim = self
            .mutation_guard
            .claim(format!("scheduled:pause:{task_id}"))?;
        ScheduledTaskRepository::pause(&migrated(&self.db_path)?, task_id)
    }

    pub fn resume(&self, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        let _claim = self
            .mutation_guard
            .claim(format!("scheduled:resume:{task_id}"))?;
        ScheduledTaskRepository::resume(&migrated(&self.db_path)?, task_id)
    }

    pub fn delete(&self, task_id: &ScheduledTaskId) -> VibexResult<ScheduledTask> {
        let _claim = self
            .mutation_guard
            .claim(format!("scheduled:delete:{task_id}"))?;
        ScheduledTaskRepository::soft_delete(&migrated(&self.db_path)?, task_id)
    }

    pub fn list_runs(
        &self,
        request: ScheduledTaskRunListRequest,
    ) -> VibexResult<Vec<ScheduledTaskRun>> {
        ScheduledTaskRepository::list_runs(&migrated(&self.db_path)?, request)
    }

    pub fn list_attention(
        &self,
        request: ScheduledTaskAttentionListRequest,
    ) -> VibexResult<Vec<ScheduledTaskAttentionSummary>> {
        ScheduledTaskRepository::list_attention(&migrated(&self.db_path)?, request)
    }

    pub fn list_audit(
        &self,
        request: ScheduledTaskAuditListRequest,
    ) -> VibexResult<Vec<ScheduledTaskAuditRecord>> {
        ScheduledTaskRepository::list_audit(&migrated(&self.db_path)?, request)
    }

    /// Claims one due task through the same atomic path used by the scheduler.
    /// This is intentionally the only manual-run primitive exposed to GPUI.
    pub fn claim_due(
        &self,
        task_id: &ScheduledTaskId,
        now_ms: i64,
    ) -> VibexResult<Option<ScheduledTaskRun>> {
        let _claim = self
            .mutation_guard
            .claim(format!("scheduled:claim:{task_id}"))?;
        let mut connection = migrated(&self.db_path)?;
        Ok(
            ScheduledTaskRepository::claim_due(&mut connection, task_id, now_ms)?
                .map(|(_, run)| run),
        )
    }
}

impl AutomationHandle {
    pub fn list(&self, request: AutomationGraphListRequest) -> VibexResult<Vec<AutomationGraph>> {
        AutomationGraphRepository::list(&migrated(&self.db_path)?, request)
    }

    pub fn create(&self, request: AutomationGraphCreateRequest) -> VibexResult<AutomationGraph> {
        let _claim = self.mutation_guard.claim("automation:create")?;
        let mut connection = migrated(&self.db_path)?;
        AutomationGraphRepository::create(&mut connection, request)
    }

    pub fn update(&self, request: AutomationGraphUpdateRequest) -> VibexResult<AutomationGraph> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:update:{}", request.id))?;
        AutomationGraphRepository::update(&migrated(&self.db_path)?, request)
    }

    pub fn replace_definition(
        &self,
        request: AutomationGraphDefinitionUpdateRequest,
    ) -> VibexResult<AutomationGraph> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:replace:{}", request.graph_id))?;
        let mut connection = migrated(&self.db_path)?;
        AutomationGraphRepository::replace_definition(
            &mut connection,
            &request.graph_id,
            request.nodes,
            request.edges,
            request.expected_version,
        )
    }

    pub fn pause(&self, graph_id: &AutomationGraphId) -> VibexResult<AutomationGraph> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:pause:{graph_id}"))?;
        self.update_status(graph_id, vibex_core::AutomationGraphStatus::Paused)
    }

    pub fn resume(&self, graph_id: &AutomationGraphId) -> VibexResult<AutomationGraph> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:resume:{graph_id}"))?;
        self.update_status(graph_id, vibex_core::AutomationGraphStatus::Active)
    }

    pub fn archive(&self, graph_id: &AutomationGraphId) -> VibexResult<AutomationGraph> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:archive:{graph_id}"))?;
        AutomationGraphRepository::soft_delete(&migrated(&self.db_path)?, graph_id)
    }

    pub async fn start_run(
        &self,
        request: AutomationRunStartRequest,
    ) -> VibexResult<AutomationRun> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:run:{}", request.graph_id))?;
        AutomationGraphRunner::new(&self.manager)
            .start_graph(request)
            .await
    }

    pub async fn resume_run(
        &self,
        request: AutomationRunResumeRequest,
    ) -> VibexResult<AutomationRun> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:resume-run:{}", request.run_id))?;
        AutomationGraphRunner::new(&self.manager)
            .resume_run(request)
            .await
    }

    pub fn cancel_run(&self, request: AutomationRunCancelRequest) -> VibexResult<AutomationRun> {
        let _claim = self
            .mutation_guard
            .claim(format!("automation:cancel-run:{}", request.run_id))?;
        AutomationGraphRunner::new(&self.manager).cancel_run(request)
    }

    pub fn list_runs(&self, request: AutomationRunListRequest) -> VibexResult<Vec<AutomationRun>> {
        AutomationGraphRepository::list_runs(&migrated(&self.db_path)?, request)
    }

    pub fn list_steps(
        &self,
        request: AutomationRunStepListRequest,
    ) -> VibexResult<Vec<AutomationRunStep>> {
        AutomationGraphRepository::list_run_steps(&migrated(&self.db_path)?, request)
    }

    fn update_status(
        &self,
        graph_id: &AutomationGraphId,
        status: vibex_core::AutomationGraphStatus,
    ) -> VibexResult<AutomationGraph> {
        AutomationGraphRepository::update(
            &migrated(&self.db_path)?,
            AutomationGraphUpdateRequest {
                id: graph_id.clone(),
                title: None,
                description: None,
                clear_description: false,
                project_id: None,
                clear_project_id: false,
                workspace_id: None,
                clear_workspace_id: false,
                workspace_root: None,
                workspace_mode: None,
                provider_kind: None,
                clear_provider_kind: false,
                provider_profile_id: None,
                clear_provider_profile_id: false,
                trigger: None,
                status: Some(status),
            },
        )
    }
}

impl DiagnosticsHandle {
    pub fn export(&self, request: DiagnosticBundleRequest) -> VibexResult<DiagnosticBundle> {
        self.service.export_bundle(request)
    }

    /// Export through an atomic temp-file rename and verify the same redaction
    /// sentinel gate used by the diagnostics smoke command.
    pub fn export_to_path(
        &self,
        request: DiagnosticBundleRequest,
        destination: impl AsRef<Path>,
    ) -> VibexResult<DiagnosticBundle> {
        let destination = destination.as_ref();
        let _claim = self.mutation_guard.claim(format!(
            "diagnostics:export:{}",
            redacted_management_fingerprint(&destination.file_name())
        ))?;
        let bundle = self.export(request)?;
        let serialized = serde_json::to_string_pretty(&bundle).map_err(|error| {
            VibexError::storage(
                "diagnostics_export_serialize_failed",
                "failed to serialize diagnostic bundle",
            )
            .with_diagnostic("errorKind", format!("{:?}", error.classify()))
        })?;
        assert_no_sensitive_sentinels(&serialized)?;
        let _bundle_fingerprint = redacted_management_fingerprint(&bundle.metadata);
        let parent = destination.parent().ok_or_else(|| {
            VibexError::validation(
                "diagnostics_export_destination_invalid",
                "diagnostic destination must have a parent directory",
            )
        })?;
        fs::create_dir_all(parent).map_err(|_| {
            VibexError::storage(
                "diagnostics_export_destination_create_failed",
                "failed to create diagnostic destination directory",
            )
        })?;
        let temp = destination.with_extension("tmp");
        fs::write(&temp, format!("{serialized}\n")).map_err(|_| {
            VibexError::storage(
                "diagnostics_export_write_failed",
                "failed to write diagnostic bundle",
            )
        })?;
        fs::rename(&temp, destination).map_err(|_| {
            let _ = fs::remove_file(&temp);
            VibexError::storage(
                "diagnostics_export_atomic_rename_failed",
                "failed to publish diagnostic bundle",
            )
        })?;
        Ok(bundle)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum BackupProgress {
    Validating,
    Copying,
    Verifying,
    Restoring,
    Succeeded,
}

impl BackupHandle {
    pub fn create(&self, backup_dir: impl Into<PathBuf>) -> VibexResult<BackupCreateResult> {
        let backup_dir = backup_dir.into();
        let _claim = self.mutation_guard.claim(format!(
            "backup:create:{}",
            redacted_management_fingerprint(&backup_dir.file_name())
        ))?;
        create_backup(BackupCreateRequest {
            source_db_path: self.db_path.clone(),
            backup_dir,
        })
    }

    pub fn inspect(&self, backup_dir: impl AsRef<Path>) -> VibexResult<BackupInspection> {
        inspect_backup(backup_dir.as_ref())
    }

    pub fn restore(
        &self,
        backup_dir: impl Into<PathBuf>,
        target_db_path: impl Into<PathBuf>,
    ) -> VibexResult<BackupRestoreResult> {
        let backup_dir = backup_dir.into();
        let target_db_path = target_db_path.into();
        let _claim = self.mutation_guard.claim(format!(
            "backup:restore:{}:{}",
            redacted_management_fingerprint(&backup_dir.file_name()),
            redacted_management_fingerprint(&target_db_path.file_name())
        ))?;
        restore_backup(BackupRestoreRequest {
            backup_dir,
            target_db_path,
        })
    }
}

impl crate::RemoteHandle {
    pub fn create_pairing_code(
        &self,
        request: RemoteCreatePairingCodeRequest,
    ) -> VibexResult<RemoteCreatePairingCodeResponse> {
        let _claim = self.mutation_guard.claim("remote:pairing:create")?;
        let connection = migrated(&self.db_path)?;
        RemoteTrustService::create_pairing_code(&connection, request)
    }

    pub fn create_pairing_offer(
        &self,
        request: RemoteCreatePairingOfferRequest,
    ) -> VibexResult<RemoteCreatePairingOfferResponse> {
        let _claim = self.mutation_guard.claim("remote:pairing-offer:create")?;
        self.gateway.create_pairing_offer(request)
    }

    pub fn cancel_pairing_offer(
        &self,
        request: RemoteCancelPairingOfferRequest,
    ) -> VibexResult<RemotePairingOfferSummary> {
        let _claim = self
            .mutation_guard
            .claim(format!("remote:pairing-offer:cancel:{}", request.offer_id))?;
        let connection = migrated(&self.db_path)?;
        RemoteTrustService::cancel_pairing_offer(&connection, request)
    }

    pub fn list_devices(&self) -> VibexResult<Vec<RemoteDeviceDetail>> {
        let connection = migrated(&self.db_path)?;
        Ok(RemoteDeviceRepository::list(&connection)?
            .into_iter()
            .map(|record| record.detail)
            .collect())
    }

    pub fn revoke_device(
        &self,
        request: RemoteRevokeDeviceRequest,
    ) -> VibexResult<RemoteDeviceDetail> {
        let _claim = self
            .mutation_guard
            .claim(format!("remote:revoke:{}", request.device_id))?;
        let connection = migrated(&self.db_path)?;
        let device_id = request.device_id.clone();
        let revoked = RemoteTrustService::revoke_device(&connection, request)?;
        self.gateway.disconnect_device(&device_id);
        Ok(revoked)
    }

    pub fn list_audit(
        &self,
        request: RemoteAuditListRequest,
    ) -> VibexResult<Vec<RemoteAuditRecord>> {
        let connection = migrated(&self.db_path)?;
        RemoteAuditRepository::list(&connection, &request)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExternalOpenUrl {
    pub url: String,
}

/// Validates the v1 GPUI external-open boundary. No network probe or embedded
/// surface is allocated here; the caller may pass the validated URL to the OS.
pub fn validate_external_open_url(value: &str) -> VibexResult<ExternalOpenUrl> {
    let url = Url::parse(value.trim()).map_err(|_| {
        VibexError::validation(
            "external_url_invalid",
            "URL must be a valid HTTP or HTTPS URL",
        )
    })?;
    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return Err(VibexError::validation(
            "external_url_invalid",
            "URL must use HTTP or HTTPS and include a host",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(VibexError::validation(
            "external_url_credentials_rejected",
            "URLs must not contain embedded credentials",
        ));
    }
    Ok(ExternalOpenUrl {
        url: url.to_string(),
    })
}

/// A bounded, deterministic fingerprint useful for management mutation traces.
/// It intentionally accepts only redacted metadata and never secret values.
pub fn redacted_management_fingerprint<T: Serialize>(value: &T) -> String {
    let bytes = serde_json::to_vec(value).unwrap_or_default();
    let digest = Sha256::digest(bytes);
    digest[..8]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn external_open_accepts_http_and_rejects_credentials_or_non_http() {
        assert_eq!(
            validate_external_open_url("https://example.invalid/docs")
                .unwrap()
                .url,
            "https://example.invalid/docs"
        );
        assert_eq!(
            validate_external_open_url("file:///tmp/secret")
                .unwrap_err()
                .code,
            "external_url_invalid"
        );
        assert_eq!(
            validate_external_open_url("https://user:pass@example.invalid/")
                .unwrap_err()
                .code,
            "external_url_credentials_rejected"
        );
    }

    #[test]
    fn mutation_guard_coalesces_duplicate_claims_and_releases_on_drop() {
        let guard = ManagementMutationGuard::default();
        let claim = guard.claim("scheduled:pause:test").unwrap();
        assert_eq!(
            guard.claim("scheduled:pause:test").unwrap_err().code,
            "management_mutation_in_progress"
        );
        drop(claim);
        assert!(guard.claim("scheduled:pause:test").is_ok());
    }

    #[test]
    fn redacted_fingerprint_is_stable_and_bounded() {
        let first = redacted_management_fingerprint(&serde_json::json!({
            "status": "ready",
            "secret": "must-not-be-logged"
        }));
        let second = redacted_management_fingerprint(&serde_json::json!({
            "status": "ready",
            "secret": "must-not-be-logged"
        }));
        assert_eq!(first, second);
        assert_eq!(first.len(), 16);
        assert!(!first.contains("must-not"));
    }

    #[test]
    fn provider_facade_exposes_exact_projection_capability_and_preview() {
        let directory = tempfile::tempdir().unwrap();
        let service =
            vibex_config_switch::ProviderConfigService::new(directory.path().join("vibex.db"));
        let runtime_probe = Arc::new(vibex_agent_acp::AcpRuntimeClient::new(service.clone()))
            .runtime_probe_service();
        let facade = ProviderManagementFacade {
            service,
            runtime_probe,
            mutation_guard: ManagementMutationGuard::default(),
        };
        let agent_id = vibex_core::AgentId::parse("codex").unwrap();
        let legacy = facade
            .service
            .create_profile(vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: vibex_core::ProviderKind::Codex,
                display_name: "Facade projection".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid/v1".to_string()),
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(vibex_core::ProviderOptions::empty()),
                secret_references: Vec::new(),
            })
            .unwrap();
        let binding = facade
            .list_agent_model_provider_bindings(vibex_core::AgentModelProviderBindingListRequest {
                agent_id: Some(agent_id.clone()),
                model_provider_profile_id: None,
            })
            .unwrap()
            .into_iter()
            .find(|binding| binding.legacy_provider_profile_id.as_ref() == Some(&legacy.id))
            .unwrap();
        let runtime = facade
            .list_agent_runtime_profiles(&agent_id)
            .unwrap()
            .into_iter()
            .find(|runtime| runtime.id == binding.runtime_profile_id)
            .unwrap();

        let capability = facade
            .agent_provider_projection_capability(
                vibex_core::AgentProviderProjectionCapabilityRequest {
                    runtime_profile_id: runtime.id,
                    binding_id: Some(binding.id.clone()),
                },
            )
            .unwrap();
        assert_eq!(
            capability.descriptor_id.as_ref().map(|id| id.as_str()),
            Some(vibex_core::CODEX_PROJECTION_DESCRIPTOR_ID)
        );
        assert_eq!(capability.descriptor_version, "1");

        let preview = facade
            .preview_agent_provider_projection(vibex_core::AgentProviderProjectionPreviewRequest {
                binding_id: binding.id,
                workspace_key: "facade-workspace".to_string(),
            })
            .unwrap();
        assert_eq!(
            preview.descriptor_id.as_str(),
            vibex_core::CODEX_PROJECTION_DESCRIPTOR_ID
        );
        assert_eq!(preview.overlay_files[0].relative_path, "config.toml");
    }
}

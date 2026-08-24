use std::collections::{HashMap, HashSet};
use std::env;
use std::fs;
use std::path::Component;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::Arc;
use std::time::{Duration, Instant};

use semver::Version;
use vibex_core::{
    AcpAgentCatalogEntry, AcpProcessStrategy, AcpProviderCatalogListResponse,
    AcpProviderCatalogPreset, AcpProviderConfig, AcpProviderEnvReference, AcpProviderEnvSource,
    AcpProviderProfileCreateRequest, AcpProviderProfileUpdateRequest,
    AgentAuthEnvironmentUpdateRequest, AgentCatalogListResponse, AgentCommandConfig, AgentConfig,
    AgentConfigStatus, AgentDefinition, AgentDiscoveryRecord, AgentId, AgentInstallStatus,
    AgentListRequest, AgentListResponse, AgentModelProviderDefaultRequest,
    AgentModelProviderDefaultSelection, AgentModelProviderDisplayOrderEntry,
    AgentModelProviderDisplayOrderSetRequest, AgentModelProviderDisplayOrderSetResponse,
    AgentModelProviderFailoverEntry, AgentModelProviderFailoverListRequest,
    AgentModelProviderFailoverListResponse, AgentModelProviderFailoverSetRequest,
    AgentModelProviderProfile, AgentModelProviderProfileCreateRequest,
    AgentModelProviderProfileDeleteRequest, AgentModelProviderProfileFetchModelsRequest,
    AgentModelProviderProfileFetchModelsResponse, AgentModelProviderProfileListRequest,
    AgentModelProviderProfileListResponse, AgentModelProviderProfileSecretValueRequest,
    AgentModelProviderProfileSecretValueResponse,
    AgentModelProviderProfileSecretValueUpdateRequest, AgentModelProviderProfileTestRequest,
    AgentModelProviderProfileTestResult, AgentModelProviderProfileUpdateRequest,
    AgentModelProviderSetDefaultRequest, AgentModelProviderTestStatus, AgentRefreshSnapshotRequest,
    AgentRefreshSnapshotResponse, AgentRuntimeKind, AgentRuntimeStatus, AgentSnapshotEntry,
    AgentUpdateConfigRequest, Hook, HookCreateRequest, HookDeleteRequest, HookInstallPreview,
    HookInstallPreviewRequest, HookInstallState, HookUpdateRequest, McpSecretTarget, McpServer,
    McpServerAgentMatrix, McpServerAgentMatrixListRequest, McpServerCreateRequest,
    McpServerDeleteRequest, McpServerDiscoverRequest, McpServerDiscovery,
    McpServerDiscoveryResponse, McpServerEnvEntry, McpServerForAgentListRequest,
    McpServerHeaderEntry, McpServerImportRequest, McpServerImportResult, McpServerProviderMatrix,
    McpServerSecretReferenceCreateRequest, McpServerSetAgentMatrixRequest,
    McpServerSetProviderMatrixRequest, McpServerTransportKind, McpServerUpdateRequest,
    McpServerValidateRequest, McpServerValidationResult, McpServerValidationStatus, Prompt,
    PromptCreateRequest, PromptDeleteRequest, PromptUpdateRequest, PromptValidateRequest,
    PromptValidationResult, PromptValidationStatus, ProviderBindingMetadata, ProviderCapabilities,
    ProviderCapabilityProbeResult, ProviderCapabilityProbeStatus, ProviderCapabilitySummary,
    ProviderConfiguredModel, ProviderDefaultScopeKind, ProviderFailoverRecommendation,
    ProviderFailoverRecommendationReason, ProviderFailoverRecommendationRequest,
    ProviderFailoverRecommendationStatus, ProviderHealthProbeKind, ProviderHealthProbeResult,
    ProviderHealthStatus, ProviderHealthSummary, ProviderInjectionField,
    ProviderInjectionOverlayFile, ProviderInjectionPreview, ProviderInjectionPreviewRequest,
    ProviderInjectionStrategy, ProviderKind, ProviderOptions, ProviderProfile,
    ProviderProfileCreateRequest, ProviderProfileDefaultScope, ProviderProfileDefaultSelection,
    ProviderProfileDeleteRequest, ProviderProfileDuplicateRequest, ProviderProfileId,
    ProviderProfileSetDefaultRequest, ProviderProfileStatus, ProviderProfileUpdateRequest,
    ProviderRunCapabilityProbesRequest, ProviderRunCapabilityProbesResult,
    ProviderRunHealthProbesRequest, ProviderRunHealthProbesResult, ProviderSecretBackend,
    ProviderSecretKind, ProviderSecretReference, ProviderSecretReferenceCreateRequest,
    ProviderSecretSetupState, ProviderUsageBalance, ProviderUsageListRequest, ProviderUsageRecord,
    ProviderUsageSummary, RequestId, ResourceAgentMatrixSourceKind, ResourceDiscoveryStatus, Skill,
    SkillAgentMatrix, SkillAgentMatrixListRequest, SkillCreateRequest, SkillDeleteRequest,
    SkillDiscoverRequest, SkillDiscovery, SkillDiscoveryResponse, SkillForAgentListRequest,
    SkillImportRequest, SkillImportResult, SkillProviderMatrix, SkillSetAgentMatrixRequest,
    SkillSetProviderMatrixRequest, SkillSourceKind, SkillUpdateRequest, SkillValidateRequest,
    SkillValidationResult, SkillValidationStatus, VibexError, VibexResult,
    acp_agent_catalog_entries, builtin_agent_definitions, unix_timestamp_ms,
};
use vibex_db::{
    AgentAuthCatalogSnapshotRepository, AgentConfigRepository,
    AgentDefaultModelProviderProfileRepository, AgentDiscoveryRepository,
    AgentManagedInstallationRepository, AgentModelProviderDisplayOrderRepository,
    AgentModelProviderFailoverRepository, AgentRuntimeOptionSnapshotRepository, HookRepository,
    McpServerRepository, PromptRepository, ProviderCapabilityRepository,
    ProviderDefaultProfileRepository, ProviderHealthRepository, ProviderInjectionPreviewRepository,
    ProviderProfileRepository, ProviderSecretReferenceRepository, ProviderUsageRepository,
    SkillRepository, apply_migrations, open_database,
};

mod native_export;
mod native_import;
mod provider_projection;
pub use provider_projection::*;
pub mod secrets;
pub mod skills;

pub const CODEX_MODEL_PROVIDER_ID_OPTION_KEY: &str = "codexModelProviderId";
pub const CODEX_MODEL_PROVIDER_CONFIG_TOML_OPTION_KEY: &str = "codexModelProviderConfigToml";
pub const CODEX_API_KEY_ENV_OPTION_KEY: &str = "codexApiKeyEnvKey";
pub const CODEX_NATIVE_MODEL_PROVIDER_OPTION_KEY: &str = "nativeModelProvider";

#[derive(Clone)]
pub struct CodexProviderRuntimeConfig {
    pub model: Option<String>,
    pub model_provider_id: String,
    pub provider_config_toml: Option<String>,
    pub provider_config_toml_keys: Vec<String>,
    pub base_url: Option<String>,
    pub wire_api: Option<String>,
    pub api_key_env_key: String,
    pub api_key: Option<String>,
}

/// Runtime listener invoked after a Provider Profile create/update readback or
/// delete commit succeeds. Implementations must be non-blocking and must not
/// attempt to participate in the completed persistence operation.
pub trait ProviderProfileChangeListener: Send + Sync {
    fn on_provider_profile_saved(
        &self,
        provider_profile_id: &ProviderProfileId,
        profile_updated_at_ms: i64,
    );

    fn on_provider_profile_deleted(&self, _provider_profile_id: &ProviderProfileId) {}
}

#[derive(Clone)]
pub struct ProviderConfigService {
    db_path: PathBuf,
    profile_change_listeners: Vec<Arc<dyn ProviderProfileChangeListener>>,
}

impl std::fmt::Debug for ProviderConfigService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ProviderConfigService")
            .field("db_path", &self.db_path)
            .field(
                "profile_change_listener_count",
                &self.profile_change_listeners.len(),
            )
            .finish()
    }
}

impl ProviderConfigService {
    pub fn new(db_path: impl Into<PathBuf>) -> Self {
        Self {
            db_path: db_path.into(),
            profile_change_listeners: Vec::new(),
        }
    }

    pub fn with_profile_change_listener(
        mut self,
        listener: Arc<dyn ProviderProfileChangeListener>,
    ) -> Self {
        self.profile_change_listeners.push(listener);
        self
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    fn notify_profile_saved(&self, profile: &ProviderProfile) {
        for listener in &self.profile_change_listeners {
            listener.on_provider_profile_saved(&profile.id, profile.updated_at_ms);
        }
    }

    fn notify_profile_deleted(&self, provider_profile_id: &ProviderProfileId) {
        for listener in &self.profile_change_listeners {
            listener.on_provider_profile_deleted(provider_profile_id);
        }
    }

    pub fn list_agents(&self, request: AgentListRequest) -> VibexResult<AgentListResponse> {
        let conn = self.open_connection()?;
        let definitions = builtin_agent_definitions();
        let configs = AgentConfigRepository::list(&conn)?;
        let managed_installations = AgentManagedInstallationRepository::list(&conn)?
            .into_iter()
            .map(|record| (record.agent_id.clone(), record.state))
            .collect::<HashMap<_, _>>();
        let mut discoveries =
            AgentDiscoveryRepository::latest_by_agent(&conn, DEFAULT_AGENT_CWD_SCOPE)?;
        let snapshots = build_agent_snapshots(
            definitions.clone(),
            configs.clone(),
            discoveries.clone(),
            &managed_installations,
        );
        refresh_changed_agent_discoveries(&conn, &snapshots, &mut discoveries)?;
        let snapshots =
            build_agent_snapshots(definitions, configs, discoveries, &managed_installations);
        let agents = if request.include_disabled {
            snapshots
        } else {
            snapshots
                .into_iter()
                .filter(|agent| agent.added && agent.enabled && agent.installed)
                .collect()
        };
        Ok(AgentListResponse { agents })
    }

    /// Refreshes binary identities needed by descriptor-driven provider forms.
    ///
    /// This is deliberately separate from `list_agents`: ordinary Agent
    /// catalog reads remain process-free, while an explicit Config Center
    /// snapshot may verify installed versioned runtimes before resolving their
    /// provider capabilities.
    pub fn refresh_detected_agent_versions(&self) -> VibexResult<usize> {
        let agents = self.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let versioned_agent_ids = vibex_core::agent_provider_rollout_manifest()?
            .into_iter()
            .filter(|entry| {
                entry.capability_mode
                    == vibex_core::AgentProviderCapabilityMode::ReplaceableProvider
                    && entry.evidence_state == vibex_core::ProjectionEvidenceState::Documented
            })
            .map(|entry| entry.agent_id)
            .collect::<HashSet<_>>();
        let mut refreshed = 0;
        for agent in agents.agents {
            if !agent.added || !agent.installed || !versioned_agent_ids.contains(&agent.id) {
                continue;
            }
            self.refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                agent_id: agent.id,
                cwd_scope: None,
            })?;
            refreshed += 1;
        }
        Ok(refreshed)
    }

    pub fn list_agent_catalog(&self) -> VibexResult<AgentCatalogListResponse> {
        Ok(AgentCatalogListResponse {
            agents: builtin_agent_definitions(),
        })
    }

    pub fn update_agent_config(
        &self,
        request: AgentUpdateConfigRequest,
    ) -> VibexResult<AgentSnapshotEntry> {
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id == request.agent_id)
            .ok_or_else(|| {
                VibexError::validation("agent_not_found", "Agent was not found")
                    .with_diagnostic("agentId", request.agent_id.as_str())
            })?;
        if let Some(label) = request.label_override.as_deref() {
            validate_agent_label(label)?;
        }
        let conn = self.open_connection()?;
        let now = unix_timestamp_ms();
        let existing = AgentConfigRepository::get(&conn, &request.agent_id)?;
        let created_at_ms = existing
            .as_ref()
            .map(|config| config.created_at_ms)
            .unwrap_or(now);
        let added = request.added.unwrap_or_else(|| {
            existing
                .as_ref()
                .is_none_or(|config| config.deleted_at_ms.is_none())
        });
        let config = AgentConfig {
            agent_id: definition.id.clone(),
            runtime_kind: definition.runtime_kind,
            source_kind: definition.source_kind,
            label_override: request.label_override.or_else(|| {
                existing
                    .as_ref()
                    .and_then(|config| config.label_override.clone())
            }),
            description_override: request.description_override.or_else(|| {
                existing
                    .as_ref()
                    .and_then(|config| config.description_override.clone())
            }),
            enabled: request
                .enabled
                .or_else(|| existing.as_ref().map(|config| config.enabled))
                .unwrap_or(definition.default_enabled),
            order_index: request
                .order_index
                .or_else(|| existing.as_ref().map(|config| config.order_index))
                .unwrap_or(definition.order_index),
            command: request
                .command
                .or_else(|| existing.as_ref().and_then(|config| config.command.clone()))
                .or_else(|| definition.command.clone()),
            env: request
                .env
                .or_else(|| existing.as_ref().map(|config| config.env.clone()))
                .unwrap_or_else(|| definition.env.clone()),
            params: request
                .params
                .or_else(|| existing.as_ref().map(|config| config.params.clone()))
                .unwrap_or_else(|| definition.params.clone()),
            created_at_ms,
            updated_at_ms: now,
            deleted_at_ms: if added { None } else { Some(now) },
        };
        AgentConfigRepository::upsert(&conn, &config)?;
        if !added {
            AgentRuntimeOptionSnapshotRepository::delete(&conn, &config.agent_id)?;
            AgentAuthCatalogSnapshotRepository::delete_agent(&conn, &config.agent_id)?;
        }
        if added && config.enabled && definition.runtime_kind == AgentRuntimeKind::Acp {
            self.ensure_default_acp_profile_for_agent(&conn, &definition, &config)?;
        }
        let discovery = AgentDiscoveryRepository::latest_for_agent(
            &conn,
            &config.agent_id,
            DEFAULT_AGENT_CWD_SCOPE,
        )?;
        let mut snapshot =
            AgentSnapshotEntry::from_definition(&definition, Some(&config), discovery.as_ref());
        if let Some(record) = AgentManagedInstallationRepository::get(&conn, &config.agent_id)? {
            snapshot.apply_managed_install_state(record.state);
        }
        Ok(snapshot)
    }

    /// Returns the version of a Vibex-managed Agent only when the persisted
    /// installation is the exact command selected by this runtime config.
    pub fn managed_agent_runtime_version(
        &self,
        agent_id: &AgentId,
        command: &AgentCommandConfig,
    ) -> VibexResult<Option<String>> {
        let conn = self.open_connection()?;
        Ok(AgentManagedInstallationRepository::get(&conn, agent_id)?
            .filter(|record| record.command.as_ref() == Some(command))
            .and_then(|record| record.state.installed_version))
    }

    /// Converges one built-in online Agent onto its ACP command while keeping
    /// the user's model-provider profile ids, defaults, models and secrets.
    pub fn reconcile_agent_acp_runtime(
        &self,
        agent_id: AgentId,
        command: AgentCommandConfig,
    ) -> VibexResult<usize> {
        if command.command.trim().is_empty() {
            return Err(VibexError::validation(
                "agent_acp_runtime_command_empty",
                "managed ACP runtime command must not be empty",
            )
            .with_diagnostic("agentId", agent_id.as_str()));
        }
        require_agent_definition(&agent_id)?;
        self.update_agent_config(AgentUpdateConfigRequest {
            agent_id: agent_id.clone(),
            added: None,
            enabled: None,
            label_override: None,
            description_override: None,
            order_index: None,
            command: Some(command.clone()),
            env: None,
            params: None,
        })?;

        let conn = self.open_connection()?;
        let profiles = ProviderProfileRepository::list_by_agent(&conn, &agent_id, true)?;
        let configuration_kind = agent_model_provider_kind(&agent_id);
        let default_runtime_config = default_acp_runtime_config_for_agent(&conn, &agent_id)?;
        let mut reconciled = Vec::new();
        for mut profile in profiles {
            if is_local_default_profile(&profile.id)
                || (profile.kind != ProviderKind::Acp && profile.kind != configuration_kind)
            {
                continue;
            }
            let mut runtime_config = acp_config_from_options(&profile.provider_options)?
                .unwrap_or_else(|| default_runtime_config.clone());
            runtime_config.command = command.command.clone();
            runtime_config.args = command.args.clone();
            if runtime_config.modes.is_empty() {
                runtime_config.modes = default_runtime_config.modes.clone();
            }
            if runtime_config.features.is_empty() {
                runtime_config.features = default_runtime_config.features.clone();
            }
            runtime_config.models = configured_acp_model_ids(
                &profile.configured_models,
                profile.default_model.as_deref(),
            );
            let provider_options =
                merge_acp_runtime_options(profile.provider_options.clone(), runtime_config)?;
            if profile.kind == ProviderKind::Acp && profile.provider_options == provider_options {
                continue;
            }
            profile.kind = ProviderKind::Acp;
            profile.provider_options = provider_options;
            profile.updated_at_ms = unix_timestamp_ms();
            ProviderProfileRepository::update(&conn, &profile)?;
            let updated = ProviderProfileRepository::get(&conn, &profile.id)?.ok_or_else(|| {
                VibexError::storage(
                    "agent_acp_profile_reconcile_readback_failed",
                    "failed to read ACP provider profile after runtime reconciliation",
                )
            })?;
            self.sync_legacy_projection(&conn, &updated)?;
            reconciled.push(updated);
        }
        drop(conn);
        for profile in &reconciled {
            self.notify_profile_saved(profile);
        }
        Ok(reconciled.len())
    }

    /// Enabling an ACP agent without any provider profile would leave sessions
    /// unable to start (the runtime needs a typed command). Seed one enabled
    /// profile from the agent's bundled preset or command config so that
    /// "enable agent, start chatting" works out of the box.
    fn ensure_default_acp_profile_for_agent(
        &self,
        conn: &vibex_db::DbConnection,
        definition: &AgentDefinition,
        config: &AgentConfig,
    ) -> VibexResult<()> {
        // Local-default placeholder profiles carry no ACP command and must not
        // suppress seeding a real profile.
        let existing = ProviderProfileRepository::list_by_agent(conn, &definition.id, true)?
            .into_iter()
            .filter(|profile| profile.id.as_str() != profile.kind.local_default_profile_id())
            .count();
        if existing > 0 {
            return Ok(());
        }

        let preset_config = config
            .params
            .get("preset")
            .and_then(serde_json::Value::as_str)
            .and_then(|preset_id| {
                bundled_acp_catalog_presets()
                    .into_iter()
                    .find(|preset| preset.preset_id == preset_id)
            })
            .map(|preset| preset.default_config);
        let mut acp_config = preset_config.unwrap_or_else(|| AcpProviderConfig {
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: Vec::new(),
            modes: Vec::new(),
            features: default_acp_preset_features(),
            disabled_tools: Vec::new(),
        });
        if let Some(command) = config.command.as_ref().or(definition.command.as_ref()) {
            acp_config.command = command.command.clone();
            acp_config.args = command.args.clone();
        }
        if acp_config.command.trim().is_empty() {
            return Ok(());
        }

        self.create_acp_profile_from_config(
            Some(definition.id.clone()),
            format!("{} ACP", definition.label),
            None,
            acp_config,
            true,
        )?;
        Ok(())
    }

    pub fn refresh_agent_snapshot(
        &self,
        request: AgentRefreshSnapshotRequest,
    ) -> VibexResult<AgentRefreshSnapshotResponse> {
        let definition = builtin_agent_definitions()
            .into_iter()
            .find(|definition| definition.id == request.agent_id)
            .ok_or_else(|| {
                VibexError::validation("agent_not_found", "Agent was not found")
                    .with_diagnostic("agentId", request.agent_id.as_str())
            })?;
        let conn = self.open_connection()?;
        let config = AgentConfigRepository::get(&conn, &definition.id)?;
        let snapshot = AgentSnapshotEntry::from_definition(&definition, config.as_ref(), None);
        let cwd_scope = request
            .cwd_scope
            .filter(|scope| !scope.trim().is_empty())
            .unwrap_or_else(|| DEFAULT_AGENT_CWD_SCOPE.to_string());
        let mut discovery = low_cost_agent_discovery(&snapshot, &cwd_scope);
        probe_explicit_agent_version(&snapshot, &mut discovery);
        AgentDiscoveryRepository::insert(&conn, &discovery)?;

        // A detected version changes the runtime identity used by the legacy
        // ACP compatibility rows. Reconcile those rows in the same refresh so
        // Config Center immediately exposes the typed projector and computes
        // the new launch fingerprint. Collect first to avoid holding a DB
        // iterator while each sync opens its own transaction.
        let legacy_profiles =
            ProviderProfileRepository::list_by_agent(&conn, &definition.id, true)?
                .into_iter()
                .filter(|profile| {
                    profile.kind == ProviderKind::Acp && !is_local_default_profile(&profile.id)
                })
                .collect::<Vec<_>>();
        let mut projection_changed = false;
        for profile in &legacy_profiles {
            projection_changed |= self.sync_legacy_projection(&conn, profile)?;
        }
        let mut agent =
            AgentSnapshotEntry::from_definition(&definition, config.as_ref(), Some(&discovery));
        if let Some(record) = AgentManagedInstallationRepository::get(&conn, &definition.id)? {
            agent.apply_managed_install_state(record.state);
        }
        // Version discovery is normally idempotent. Only publish a profile
        // event when reconciling the detected identity actually changed the
        // effective projection; otherwise Config Center refresh would call
        // this method again and create a probe/event feedback loop.
        if projection_changed {
            for profile in &legacy_profiles {
                self.notify_profile_saved(profile);
            }
        }
        Ok(AgentRefreshSnapshotResponse { agent })
    }

    pub fn list_profiles(&self) -> VibexResult<Vec<ProviderProfile>> {
        let conn = self.open_connection()?;
        visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)
    }

    /// Returns all ACP-compatible profiles needed by runtime option catalog
    /// construction, including Agent-owned profiles that are intentionally
    /// hidden from the model-provider Config Center surface.
    pub fn list_runtime_profiles(&self) -> VibexResult<Vec<ProviderProfile>> {
        let conn = self.open_connection()?;
        ProviderProfileRepository::list_all(&conn)
    }

    pub fn get_profile(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> VibexResult<Option<ProviderProfile>> {
        let conn = self.open_connection()?;
        ProviderProfileRepository::get(&conn, provider_profile_id)
    }

    pub fn list_agent_model_provider_profiles(
        &self,
        request: AgentModelProviderProfileListRequest,
    ) -> VibexResult<AgentModelProviderProfileListResponse> {
        let conn = self.open_connection()?;
        let definition = require_agent_definition(&request.agent_id)?;
        let profiles = visible_model_provider_profiles(
            &conn,
            ProviderProfileRepository::list_by_agent(
                &conn,
                &request.agent_id,
                request.include_disabled,
            )?,
        )?;
        let default = AgentDefaultModelProviderProfileRepository::get(
            &conn,
            global_default_scope(),
            request.agent_id.clone(),
        )?;
        let failover = AgentModelProviderFailoverRepository::list(&conn, &request.agent_id)?;
        let display_order =
            AgentModelProviderDisplayOrderRepository::list(&conn, &request.agent_id)?;
        Ok(AgentModelProviderProfileListResponse {
            profiles: build_agent_model_provider_profiles(
                definition.id,
                profiles,
                default.provider_profile_id.as_ref(),
                &failover,
                &display_order,
            ),
        })
    }

    pub fn set_agent_model_provider_display_order(
        &self,
        request: AgentModelProviderDisplayOrderSetRequest,
    ) -> VibexResult<AgentModelProviderDisplayOrderSetResponse> {
        require_agent_definition(&request.agent_id)?;
        let mut seen = HashSet::new();
        let mut conn = self.open_connection()?;
        let profiles = visible_model_provider_profiles(
            &conn,
            ProviderProfileRepository::list_by_agent(&conn, &request.agent_id, true)?,
        )?;
        let profile_ids = profiles
            .iter()
            .map(|profile| profile.id.clone())
            .collect::<HashSet<_>>();
        let mut entries = Vec::with_capacity(request.entries.len());
        for (order_index, entry) in request.entries.into_iter().enumerate() {
            if !seen.insert(entry.provider_profile_id.clone()) {
                return Err(VibexError::validation(
                    "provider_display_order_duplicate",
                    "provider display order contains a duplicate profile",
                ));
            }
            if !profile_ids.contains(&entry.provider_profile_id) {
                return Err(VibexError::validation(
                    "provider_display_order_profile_not_found",
                    "provider display order contains a profile outside the Agent",
                )
                .with_diagnostic("agentId", request.agent_id.as_str())
                .with_diagnostic("providerProfileId", entry.provider_profile_id.as_str()));
            }
            entries.push(AgentModelProviderDisplayOrderEntry {
                agent_id: request.agent_id.clone(),
                provider_profile_id: entry.provider_profile_id,
                order_index: order_index as i64,
                updated_at_ms: unix_timestamp_ms(),
            });
        }
        if seen != profile_ids {
            return Err(VibexError::validation(
                "provider_display_order_incomplete",
                "provider display order must include every profile belonging to the Agent",
            )
            .with_diagnostic("agentId", request.agent_id.as_str()));
        }
        Ok(AgentModelProviderDisplayOrderSetResponse {
            entries: AgentModelProviderDisplayOrderRepository::replace(
                &mut conn,
                &request.agent_id,
                &entries,
            )?,
        })
    }

    pub fn get_agent_model_provider_display_order(
        &self,
        request: vibex_core::AgentModelProviderDisplayOrderListRequest,
    ) -> VibexResult<vibex_core::AgentModelProviderDisplayOrderListResponse> {
        require_agent_definition(&request.agent_id)?;
        let conn = self.open_connection()?;
        Ok(vibex_core::AgentModelProviderDisplayOrderListResponse {
            entries: AgentModelProviderDisplayOrderRepository::list(&conn, &request.agent_id)?,
        })
    }

    pub fn create_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileCreateRequest,
    ) -> VibexResult<ProviderProfile> {
        validate_display_name(&request.display_name)?;
        require_agent_definition(&request.agent_id)?;
        validate_agent_model_interfaces(
            &request.agent_id,
            &request.configured_models,
            request.provider_options.as_ref(),
        )?;
        let provider_kind = agent_configuration_provider_kind(&request.agent_id);
        let conn = self.open_connection()?;
        let configured_acp_models =
            configured_acp_model_ids(&request.configured_models, request.default_model.as_deref());
        let requested_provider_options =
            request.provider_options.map(without_internal_profile_role);
        let provider_options = if provider_kind == ProviderKind::Acp {
            let options = inherit_agent_acp_runtime_options(
                &conn,
                &request.agent_id,
                requested_provider_options,
            )?;
            Some(with_acp_configured_models(options, configured_acp_models)?)
        } else {
            requested_provider_options
        };
        let profile =
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: Some(request.agent_id),
                kind: provider_kind,
                display_name: request.display_name,
                account_alias: request.account_alias,
                base_url: request.base_url,
                default_model: request.default_model,
                small_model: request.small_model,
                large_model: request.large_model,
                configured_models: request.configured_models,
                reasoning_effort: request.reasoning_effort,
                sandbox_defaults: request.sandbox_defaults,
                network_defaults: request.network_defaults,
                permission_defaults: request.permission_defaults,
                provider_options,
                secret_references: request.secret_references,
            });
        ProviderProfileRepository::insert(&conn, &profile)?;
        let created = ProviderProfileRepository::get(&conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_model_provider_profile_create_readback_failed",
                "failed to read agent model provider profile after create",
            )
        })?;
        self.sync_legacy_projection(&conn, &created)?;
        self.notify_profile_saved(&created);
        Ok(created)
    }

    pub fn update_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileUpdateRequest,
    ) -> VibexResult<ProviderProfile> {
        let conn = self.open_connection()?;
        let profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        let models_changed = request.configured_models.is_some() || request.default_model.is_some();
        let configured_acp_models = configured_acp_model_ids(
            request
                .configured_models
                .as_deref()
                .unwrap_or(&profile.configured_models),
            request
                .default_model
                .as_deref()
                .or(profile.default_model.as_deref()),
        );
        let requested_provider_options =
            request.provider_options.map(without_internal_profile_role);
        let provider_options = match (profile.kind, requested_provider_options) {
            (ProviderKind::Acp, options) if options.is_some() || models_changed => {
                let options = options.unwrap_or_else(|| {
                    without_internal_profile_role(profile.provider_options.clone())
                });
                let mut runtime_config = acp_config_from_options(&profile.provider_options)?
                    .ok_or_else(|| {
                        VibexError::validation(
                            "acp_config_missing",
                            "ACP model provider profile is missing its inherited runtime configuration",
                        )
                        .with_diagnostic("providerProfileId", profile.id.as_str())
                    })?;
                runtime_config.models = configured_acp_models;
                Some(merge_acp_runtime_options(options, runtime_config)?)
            }
            (_, options) => options,
        };
        validate_agent_model_interfaces(
            &request.agent_id,
            request
                .configured_models
                .as_deref()
                .unwrap_or(&profile.configured_models),
            provider_options
                .as_ref()
                .or(Some(&profile.provider_options)),
        )?;
        drop(conn);
        self.update_profile(ProviderProfileUpdateRequest {
            provider_profile_id: profile.id,
            display_name: request.display_name,
            status: request.status,
            account_alias: request.account_alias,
            base_url: request.base_url,
            default_model: request.default_model,
            small_model: request.small_model,
            large_model: request.large_model,
            configured_models: request.configured_models,
            reasoning_effort: request.reasoning_effort,
            sandbox_defaults: request.sandbox_defaults,
            network_defaults: request.network_defaults,
            permission_defaults: request.permission_defaults,
            provider_options,
        })
    }

    pub fn delete_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileDeleteRequest,
    ) -> VibexResult<()> {
        let conn = self.open_connection()?;
        require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        drop(conn);
        self.delete_profile(ProviderProfileDeleteRequest {
            provider_profile_id: request.provider_profile_id,
        })
    }

    pub fn fetch_agent_model_provider_profile_models(
        &self,
        request: AgentModelProviderProfileFetchModelsRequest,
    ) -> VibexResult<AgentModelProviderProfileFetchModelsResponse> {
        let conn = self.open_connection()?;
        let profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        let (models, diagnostics) = fetch_provider_profile_models(&profile)?;
        Ok(AgentModelProviderProfileFetchModelsResponse {
            agent_id: request.agent_id,
            provider_profile_id: request.provider_profile_id,
            models: models
                .into_iter()
                .map(|model| ProviderConfiguredModel {
                    id: model,
                    display_name: None,
                    enabled: true,
                    wire_api: None,
                    capabilities: Default::default(),
                })
                .collect(),
            diagnostics,
        })
    }

    pub fn test_agent_model_provider_profile(
        &self,
        request: AgentModelProviderProfileTestRequest,
    ) -> VibexResult<AgentModelProviderProfileTestResult> {
        let conn = self.open_connection()?;
        let profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        let mut diagnostics = vec![
            ProviderBindingMetadata {
                key: "providerKind".to_string(),
                value: profile.kind.to_string(),
            },
            ProviderBindingMetadata {
                key: "secretSetupState".to_string(),
                value: format!("{:?}", profile.summary().secret_setup_state).to_lowercase(),
            },
        ];
        if let Some(base_url) = profile.base_url.as_deref() {
            diagnostics.push(ProviderBindingMetadata {
                key: "baseUrl".to_string(),
                value: redact_url_for_diagnostics(base_url),
            });
        }
        let (status, code, message, latency_ms, probe_diagnostics) =
            if profile.status == ProviderProfileStatus::Disabled {
                (
                    AgentModelProviderTestStatus::Warn,
                    "agent_model_provider_profile_disabled".to_string(),
                    "Provider profile is disabled; live API probe was not executed".to_string(),
                    None,
                    Vec::new(),
                )
            } else {
                let outcome = run_provider_api_probe(&profile, ProviderApiProbeKind::SimplePrompt)?;
                (
                    if outcome.passed {
                        AgentModelProviderTestStatus::Pass
                    } else {
                        AgentModelProviderTestStatus::Fail
                    },
                    outcome.code,
                    outcome.message,
                    outcome.latency_ms,
                    outcome.diagnostics,
                )
            };
        diagnostics.extend(probe_diagnostics);
        if let Some(latency_ms) = latency_ms {
            diagnostics.push(ProviderBindingMetadata {
                key: "latencyMs".to_string(),
                value: latency_ms.to_string(),
            });
        }
        Ok(AgentModelProviderProfileTestResult {
            agent_id: request.agent_id,
            provider_profile_id: request.provider_profile_id,
            status,
            code,
            message,
            diagnostics,
            checked_at_ms: unix_timestamp_ms(),
        })
    }

    pub fn get_agent_model_provider_profile_secret_value(
        &self,
        request: AgentModelProviderProfileSecretValueRequest,
    ) -> VibexResult<AgentModelProviderProfileSecretValueResponse> {
        let conn = self.open_connection()?;
        let profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        build_agent_model_provider_secret_value_response(request.agent_id, &profile)
    }

    pub fn update_agent_model_provider_profile_secret_value(
        &self,
        request: AgentModelProviderProfileSecretValueUpdateRequest,
    ) -> VibexResult<AgentModelProviderProfileSecretValueResponse> {
        let conn = self.open_connection()?;
        let mut profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        let secret_kind = editable_profile_secret_kind(&profile);
        let display_label = editable_profile_secret_display_label(&profile, secret_kind);
        let next_value = request
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(ToOwned::to_owned);

        // Empty controls are the normal representation of an untouched
        // Secret field. Clearing is a separate, explicit operation.
        if !request.clear && next_value.is_none() {
            return build_agent_model_provider_secret_value_response(request.agent_id, &profile);
        }

        let matching_secrets: Vec<_> = profile
            .secrets
            .iter()
            .filter(|secret| secret.secret_kind == secret_kind)
            .cloned()
            .collect();
        let now = unix_timestamp_ms().max(
            matching_secrets
                .iter()
                .map(|secret| secret.updated_at_ms.saturating_add(1))
                .max()
                .unwrap_or_default(),
        );
        let reusable_lookup_key = matching_secrets
            .iter()
            .find(|secret| secret.backend == ProviderSecretBackend::OsKeychain)
            .map(|secret| secret.lookup_key.clone())
            .filter(|lookup_key| !lookup_key.trim().is_empty());

        let replacement = if request.clear {
            None
        } else {
            let lookup_key = reusable_lookup_key
                .clone()
                .unwrap_or_else(|| format!("vibex-provider-secret-{}", RequestId::new().as_str()));
            let value = next_value.as_deref().expect("value checked above");
            secrets::store_provider_secret(&lookup_key, value)?;
            Some(ProviderSecretReference {
                id: matching_secrets
                    .first()
                    .map(|secret| secret.id.clone())
                    .unwrap_or_else(RequestId::new),
                provider_profile_id: profile.id.clone(),
                secret_kind,
                backend: ProviderSecretBackend::OsKeychain,
                setup_state: ProviderSecretSetupState::Available,
                lookup_key,
                display_label: display_label.clone(),
                redacted_hint: "stored in Vibex OS keychain".to_string(),
                created_at_ms: matching_secrets
                    .first()
                    .map(|secret| secret.created_at_ms)
                    .unwrap_or(now),
                updated_at_ms: now,
            })
        };

        for secret in &matching_secrets {
            let replacement_reuses_lookup = replacement
                .as_ref()
                .is_some_and(|next| next.lookup_key == secret.lookup_key);
            if secret.backend == ProviderSecretBackend::OsKeychain && !replacement_reuses_lookup {
                secrets::delete_provider_secret(&secret.lookup_key)?;
            }
        }

        profile
            .secrets
            .retain(|secret| secret.secret_kind != secret_kind);
        if let Some(secret) = replacement {
            profile.secrets.push(secret);
        }
        profile.updated_at_ms = now;

        ProviderProfileRepository::update(&conn, &profile)?;
        ProviderSecretReferenceRepository::replace_for_profile(
            &conn,
            &profile.id,
            &profile.secrets,
        )?;
        let updated = ProviderProfileRepository::get(&conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_model_provider_secret_update_readback_failed",
                "failed to read provider profile after secret update",
            )
        })?;
        self.sync_legacy_projection(&conn, &updated)?;
        self.notify_profile_saved(&updated);
        build_agent_model_provider_secret_value_response(request.agent_id, &updated)
    }

    pub fn get_agent_model_provider_default(
        &self,
        request: AgentModelProviderDefaultRequest,
    ) -> VibexResult<AgentModelProviderDefaultSelection> {
        validate_default_scope(&request.scope)?;
        require_agent_definition(&request.agent_id)?;
        let conn = self.open_connection()?;
        AgentDefaultModelProviderProfileRepository::get(&conn, request.scope, request.agent_id)
    }

    pub fn set_agent_model_provider_default(
        &self,
        request: AgentModelProviderSetDefaultRequest,
    ) -> VibexResult<AgentModelProviderDefaultSelection> {
        validate_default_scope(&request.scope)?;
        let conn = self.open_connection()?;
        let profile =
            require_agent_profile(&conn, &request.agent_id, &request.provider_profile_id)?;
        if profile.status != ProviderProfileStatus::Enabled {
            return Err(VibexError::validation(
                "agent_model_provider_default_disabled",
                "default model provider profile must be enabled",
            )
            .with_diagnostic("agentId", request.agent_id.as_str())
            .with_diagnostic("providerProfileId", request.provider_profile_id.as_str()));
        }
        AgentDefaultModelProviderProfileRepository::set(
            &conn,
            request.scope,
            request.agent_id,
            request.provider_profile_id,
        )
    }

    pub fn get_agent_model_provider_failover(
        &self,
        request: AgentModelProviderFailoverListRequest,
    ) -> VibexResult<AgentModelProviderFailoverListResponse> {
        require_agent_definition(&request.agent_id)?;
        let conn = self.open_connection()?;
        Ok(AgentModelProviderFailoverListResponse {
            entries: AgentModelProviderFailoverRepository::list(&conn, &request.agent_id)?,
        })
    }

    pub fn set_agent_model_provider_failover(
        &self,
        request: AgentModelProviderFailoverSetRequest,
    ) -> VibexResult<AgentModelProviderFailoverListResponse> {
        require_agent_definition(&request.agent_id)?;
        let mut seen = HashSet::new();
        let mut conn = self.open_connection()?;
        let mut entries = Vec::new();
        for (order_index, entry) in request.entries.into_iter().enumerate() {
            if !seen.insert(entry.provider_profile_id.clone()) {
                return Err(VibexError::validation(
                    "failover_profile_duplicate",
                    "failover queue contains a duplicate provider profile",
                )
                .with_diagnostic("providerProfileId", entry.provider_profile_id.as_str()));
            }
            let profile = require_failover_agent_profile(
                &conn,
                &request.agent_id,
                &entry.provider_profile_id,
            )?;
            if profile.status != ProviderProfileStatus::Enabled {
                return Err(VibexError::validation(
                    "failover_profile_disabled",
                    "failover candidate must be enabled",
                )
                .with_diagnostic("agentId", request.agent_id.as_str())
                .with_diagnostic("providerProfileId", entry.provider_profile_id.as_str()));
            }
            entries.push(AgentModelProviderFailoverEntry {
                agent_id: request.agent_id.clone(),
                provider_profile_id: profile.id,
                display_name: profile.display_name,
                status: profile.status,
                order_index: order_index as i64,
                enabled: entry.enabled,
                updated_at_ms: unix_timestamp_ms(),
            });
        }
        Ok(AgentModelProviderFailoverListResponse {
            entries: AgentModelProviderFailoverRepository::replace(
                &mut conn,
                &request.agent_id,
                &entries,
            )?,
        })
    }

    pub fn create_profile(
        &self,
        request: ProviderProfileCreateRequest,
    ) -> VibexResult<ProviderProfile> {
        validate_display_name(&request.display_name)?;
        validate_profile_agent_kind(request.agent_id.as_ref(), request.kind)?;
        if let Some(agent_id) = request.agent_id.as_ref() {
            validate_agent_model_interfaces(
                agent_id,
                &request.configured_models,
                request.provider_options.as_ref(),
            )?;
        }
        if request.kind == ProviderKind::Acp {
            validate_acp_profile_options(request.provider_options.as_ref())?;
        }
        let conn = self.open_connection()?;
        let profile = ProviderProfileRepository::from_create_request(request);
        ProviderProfileRepository::insert(&conn, &profile)?;
        let created = ProviderProfileRepository::get(&conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "provider_profile_create_readback_failed",
                "failed to read provider profile after create",
            )
        })?;
        self.sync_legacy_projection(&conn, &created)?;
        self.notify_profile_saved(&created);
        Ok(created)
    }

    pub fn update_profile(
        &self,
        request: ProviderProfileUpdateRequest,
    ) -> VibexResult<ProviderProfile> {
        let conn = self.open_connection()?;
        let mut profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;

        if let Some(display_name) = request.display_name {
            validate_display_name(&display_name)?;
            profile.display_name = display_name;
        }
        if let Some(status) = request.status {
            profile.status = status;
        }
        if request.account_alias.is_some() {
            profile.account_alias = request.account_alias;
        }
        if request.base_url.is_some() {
            profile.base_url = request.base_url;
        }
        if request.default_model.is_some() {
            profile.default_model = request.default_model;
        }
        if request.small_model.is_some() {
            profile.small_model = request.small_model;
        }
        if request.large_model.is_some() {
            profile.large_model = request.large_model;
        }
        if let Some(configured_models) = request.configured_models {
            profile.configured_models = configured_models;
        }
        if request.reasoning_effort.is_some() {
            profile.reasoning_effort = request.reasoning_effort;
        }
        if let Some(sandbox_defaults) = request.sandbox_defaults {
            profile.sandbox_defaults = sandbox_defaults;
        }
        if let Some(network_defaults) = request.network_defaults {
            profile.network_defaults = network_defaults;
        }
        if let Some(permission_defaults) = request.permission_defaults {
            profile.permission_defaults = permission_defaults;
        }
        if let Some(provider_options) = request.provider_options {
            if profile.kind == ProviderKind::Acp {
                validate_acp_profile_options(Some(&provider_options))?;
            }
            profile.provider_options = provider_options;
        }
        validate_agent_model_interfaces(
            &profile.agent_id,
            &profile.configured_models,
            Some(&profile.provider_options),
        )?;
        profile.updated_at_ms = unix_timestamp_ms();

        ProviderProfileRepository::update(&conn, &profile)?;
        let updated = ProviderProfileRepository::get(&conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "provider_profile_update_readback_failed",
                "failed to read provider profile after update",
            )
        })?;
        self.sync_legacy_projection(&conn, &updated)?;
        self.notify_profile_saved(&updated);
        Ok(updated)
    }

    pub fn list_acp_catalog_presets(&self) -> VibexResult<AcpProviderCatalogListResponse> {
        Ok(AcpProviderCatalogListResponse {
            presets: bundled_acp_catalog_presets(),
        })
    }

    pub fn create_acp_profile(
        &self,
        request: AcpProviderProfileCreateRequest,
    ) -> VibexResult<ProviderProfile> {
        validate_display_name(&request.display_name)?;
        let config = resolve_acp_create_config(request.preset_id.as_deref(), request.config)?;
        self.create_acp_profile_from_config(
            request.agent_id,
            request.display_name,
            request.account_alias,
            config,
            false,
        )
    }

    fn create_acp_profile_from_config(
        &self,
        agent_id: Option<AgentId>,
        display_name: String,
        account_alias: Option<String>,
        config: AcpProviderConfig,
        internal_runtime: bool,
    ) -> VibexResult<ProviderProfile> {
        let mut provider_options = acp_config_to_options(&config)?;
        if internal_runtime {
            provider_options.entries.push(option_entry(
                INTERNAL_PROFILE_ROLE_OPTION_KEY,
                INTERNAL_AGENT_RUNTIME_PROFILE_ROLE,
            ));
        }
        self.create_profile(ProviderProfileCreateRequest {
            agent_id,
            kind: ProviderKind::Acp,
            display_name,
            account_alias,
            base_url: None,
            default_model: config.models.first().cloned(),
            small_model: None,
            large_model: None,
            configured_models: config
                .models
                .iter()
                .map(|model| vibex_core::ProviderConfiguredModel {
                    id: model.clone(),
                    display_name: None,
                    enabled: true,
                    wire_api: None,
                    capabilities: Default::default(),
                })
                .collect(),
            reasoning_effort: None,
            sandbox_defaults: None,
            network_defaults: None,
            permission_defaults: None,
            provider_options: Some(provider_options),
            secret_references: Vec::new(),
        })
    }

    pub fn get_acp_profile_config(
        &self,
        provider_profile_id: ProviderProfileId,
    ) -> VibexResult<AcpProviderConfig> {
        let conn = self.open_connection()?;
        let mut profile =
            ProviderProfileRepository::get(&conn, &provider_profile_id)?.ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", provider_profile_id.as_str())
            })?;
        if profile.kind != ProviderKind::Acp {
            return Err(VibexError::validation(
                "acp_profile_kind_mismatch",
                "ACP config is only available for ACP provider profiles",
            )
            .with_diagnostic("providerProfileId", profile.id.as_str())
            .with_diagnostic("providerKind", profile.kind.to_string()));
        }
        let mut config = acp_config_from_options(&profile.provider_options)?.ok_or_else(|| {
            VibexError::validation(
                "acp_config_missing",
                "ACP provider profile is missing typed ACP configuration",
            )
            .with_diagnostic("providerProfileId", profile.id.as_str())
        })?;
        if native_import::hydrate_cc_switch_claude_profile_config(&mut profile, &mut config)? {
            profile.updated_at_ms =
                unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
            ProviderProfileRepository::update(&conn, &profile)?;
            self.sync_legacy_projection(&conn, &profile)?;
            self.notify_profile_saved(&profile);
        }
        Ok(config)
    }

    /// Persists the exact environment variables advertised by one ACP auth
    /// method. Secret values live only in the OS keychain; the profile stores
    /// opaque references so multiple profiles can switch independently.
    pub fn update_agent_auth_environment(
        &self,
        request: AgentAuthEnvironmentUpdateRequest,
    ) -> VibexResult<ProviderProfile> {
        let conn = self.open_connection()?;
        let mut profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "Provider Profile was not found for Agent authentication",
                )
            })?;
        if profile.agent_id != request.agent_id {
            return Err(VibexError::validation(
                "agent_auth_profile_mismatch",
                "Provider Profile belongs to another Agent",
            ));
        }
        if profile.kind != ProviderKind::Acp {
            return Err(VibexError::validation(
                "agent_auth_profile_kind_invalid",
                "Agent environment authentication requires an ACP Provider Profile",
            ));
        }
        let method_id = request.method_id.as_str();
        if method_id.is_empty()
            || method_id.trim().is_empty()
            || method_id.chars().count() > 512
            || method_id.chars().any(char::is_control)
        {
            return Err(VibexError::validation(
                "agent_auth_method_id_invalid",
                "Agent authentication method id is invalid",
            ));
        }
        let mut config = acp_config_from_options(&profile.provider_options)?.ok_or_else(|| {
            VibexError::validation(
                "acp_config_missing",
                "ACP Provider Profile is missing its typed runtime configuration",
            )
        })?;
        let projected_secrets = self
            .plan_legacy_agent_provider_projection(&profile.id, "agent-auth")
            .ok()
            .into_iter()
            .flat_map(|plan| plan.secret_env)
            .filter_map(|entry| {
                entry
                    .secret_reference
                    .legacy_secret_reference_id
                    .map(|secret_id| {
                        (
                            entry.key,
                            (
                                secret_id,
                                entry.secret_reference.lookup_key,
                                entry.secret_reference.backend,
                            ),
                        )
                    })
            })
            .collect::<HashMap<_, _>>();
        let mut seen = HashSet::new();
        let mut secrets_to_delete = HashSet::new();
        let mut secret_writes = Vec::new();

        for input in request.values {
            let name = input.name.trim();
            if !is_valid_env_key(name) || !seen.insert(name.to_string()) {
                return Err(VibexError::validation(
                    "agent_auth_env_key_invalid",
                    "Agent authentication environment variable is invalid or duplicated",
                )
                .with_diagnostic("envKey", name));
            }
            let value = input
                .value
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty());
            if input.clear && value.is_some() {
                return Err(VibexError::validation(
                    "agent_auth_env_clear_value_conflict",
                    "Agent authentication value cannot be set and cleared together",
                )
                .with_diagnostic("envKey", name));
            }

            let existing_reference = config
                .env
                .iter()
                .find(|reference| reference.key == name)
                .cloned();
            let projected_secret = projected_secrets.get(name).cloned();
            let projected_secret_is_existing =
                projected_secret.as_ref().is_some_and(|(_, lookup_key, _)| {
                    existing_reference
                        .as_ref()
                        .and_then(|reference| reference.secret_lookup_key.as_deref())
                        == Some(lookup_key.as_str())
                });
            // A blank masked input means "keep the configured value". Only an
            // explicit clear intent removes a reference from the Profile.
            if value.is_none()
                && !input.clear
                && (existing_reference.is_some() || projected_secret.is_some())
            {
                continue;
            }
            if value.is_none() && !input.optional && !input.clear {
                return Err(VibexError::validation(
                    "agent_auth_env_value_required",
                    "A required Agent authentication value is missing",
                )
                .with_diagnostic("envKey", name));
            }

            config.env.retain(|reference| reference.key != name);
            if let Some(old_lookup) = existing_reference
                .as_ref()
                .and_then(|reference| reference.secret_lookup_key.as_ref())
                .filter(|_| !input.secret || value.is_none() || input.clear)
            {
                secrets_to_delete.insert(old_lookup.clone());
                profile
                    .secrets
                    .retain(|secret| secret.lookup_key != *old_lookup);
            }
            if let Some((secret_id, lookup_key, backend)) = projected_secret
                && (input.clear || value.is_some())
                && !projected_secret_is_existing
            {
                profile.secrets.retain(|secret| secret.id != secret_id);
                if backend == ProviderSecretBackend::OsKeychain {
                    secrets_to_delete.insert(lookup_key);
                }
            }

            let Some(value) = value else {
                continue;
            };
            if input.secret {
                let lookup_key = existing_reference
                    .as_ref()
                    .and_then(|reference| reference.secret_lookup_key.clone())
                    .filter(|lookup| !lookup.trim().is_empty())
                    .unwrap_or_else(|| {
                        format!("vibex-agent-auth-{}-{}", profile.id.as_str(), name)
                    });
                secret_writes.push((lookup_key.clone(), value.to_string()));
                let now = unix_timestamp_ms();
                if let Some(secret) = profile
                    .secrets
                    .iter_mut()
                    .find(|secret| secret.lookup_key == lookup_key)
                {
                    secret.backend = ProviderSecretBackend::OsKeychain;
                    secret.setup_state = ProviderSecretSetupState::Available;
                    secret.display_label = name.to_string();
                    secret.redacted_hint = "stored in Vibex OS keychain".to_string();
                    secret.updated_at_ms = now.max(secret.updated_at_ms.saturating_add(1));
                } else {
                    profile.secrets.push(ProviderSecretReference {
                        id: RequestId::new(),
                        provider_profile_id: profile.id.clone(),
                        secret_kind: ProviderSecretKind::Environment,
                        backend: ProviderSecretBackend::OsKeychain,
                        setup_state: ProviderSecretSetupState::Available,
                        lookup_key: lookup_key.clone(),
                        display_label: name.to_string(),
                        redacted_hint: "stored in Vibex OS keychain".to_string(),
                        created_at_ms: now,
                        updated_at_ms: now,
                    });
                }
                config.env.push(AcpProviderEnvReference {
                    key: name.to_string(),
                    source: AcpProviderEnvSource::SecretReference,
                    value: None,
                    secret_lookup_key: Some(lookup_key),
                    redacted_hint: "stored in Vibex OS keychain".to_string(),
                });
            } else {
                config.env.push(AcpProviderEnvReference {
                    key: name.to_string(),
                    source: AcpProviderEnvSource::Literal,
                    value: Some(value.to_string()),
                    secret_lookup_key: None,
                    redacted_hint: "configured".to_string(),
                });
            }
        }

        config.env.sort_by(|left, right| left.key.cmp(&right.key));
        validate_acp_config(&config)?;
        profile.provider_options = acp_config_to_options(&config)?;
        profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
        let applied_secret_writes = apply_agent_auth_secret_writes(&secret_writes)?;
        let persisted = (|| -> VibexResult<ProviderProfile> {
            let transaction = conn.unchecked_transaction().map_err(|error| {
                VibexError::storage(
                    "agent_auth_environment_transaction_failed",
                    "failed to start Agent authentication environment update",
                )
                .with_diagnostic("error", error.to_string())
            })?;
            ProviderProfileRepository::update(&transaction, &profile)?;
            ProviderSecretReferenceRepository::replace_for_profile(
                &transaction,
                &profile.id,
                &profile.secrets,
            )?;
            let updated =
                ProviderProfileRepository::get(&transaction, &profile.id)?.ok_or_else(|| {
                    VibexError::storage(
                        "agent_auth_environment_readback_failed",
                        "failed to read Provider Profile after Agent authentication update",
                    )
                })?;
            transaction.commit().map_err(|error| {
                VibexError::storage(
                    "agent_auth_environment_commit_failed",
                    "failed to commit Agent authentication environment update",
                )
                .with_diagnostic("error", error.to_string())
            })?;
            Ok(updated)
        })();
        let updated = match persisted {
            Ok(updated) => updated,
            Err(error) => {
                let rollback_failures = rollback_agent_auth_secret_writes(&applied_secret_writes);
                return Err(if rollback_failures == 0 {
                    error
                } else {
                    error.with_diagnostic("keychainRollbackFailures", rollback_failures.to_string())
                });
            }
        };
        let projection_result = self.sync_legacy_projection(&conn, &updated);
        self.notify_profile_saved(&updated);
        let retained_secret_lookups = updated
            .secrets
            .iter()
            .map(|secret| secret.lookup_key.as_str())
            .collect::<HashSet<_>>();
        for lookup_key in secrets_to_delete {
            if !retained_secret_lookups.contains(lookup_key.as_str()) {
                let _ = secrets::delete_provider_secret(&lookup_key);
            }
        }
        projection_result?;
        Ok(updated)
    }

    /// Returns the typed ACP command configuration owned by an Agent. This
    /// deliberately does not inspect or require a model Provider Profile and
    /// is used for Agent-level runtime capability discovery.
    pub fn get_agent_acp_runtime_config(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<AcpProviderConfig> {
        let conn = self.open_connection()?;
        default_acp_runtime_config_for_agent(&conn, agent_id)
    }

    pub fn get_codex_runtime_config(
        &self,
        provider_profile_id: ProviderProfileId,
        model_override: Option<String>,
    ) -> VibexResult<CodexProviderRuntimeConfig> {
        let conn = self.open_connection()?;
        let profile =
            ProviderProfileRepository::get(&conn, &provider_profile_id)?.ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", provider_profile_id.as_str())
            })?;
        if agent_model_provider_kind(&profile.agent_id) != ProviderKind::Codex {
            return Err(VibexError::validation(
                "codex_profile_kind_mismatch",
                "Codex runtime config requires a Codex Agent provider profile",
            )
            .with_diagnostic("providerProfileId", profile.id.as_str())
            .with_diagnostic("providerKind", profile.kind.to_string()));
        }
        codex_runtime_config_from_profile(&profile, model_override)
    }

    pub fn update_acp_profile_config(
        &self,
        request: AcpProviderProfileUpdateRequest,
    ) -> VibexResult<ProviderProfile> {
        let provider_options = acp_config_to_options(&request.config)?;
        let conn = self.open_connection()?;
        let profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;
        if profile.kind != ProviderKind::Acp {
            return Err(VibexError::validation(
                "acp_profile_kind_mismatch",
                "ACP config updates require an ACP provider profile",
            )
            .with_diagnostic("providerProfileId", profile.id.as_str())
            .with_diagnostic("providerKind", profile.kind.to_string()));
        }
        drop(conn);

        self.update_profile(ProviderProfileUpdateRequest {
            provider_profile_id: request.provider_profile_id,
            display_name: None,
            status: None,
            account_alias: None,
            base_url: None,
            default_model: request.config.models.first().cloned(),
            small_model: None,
            large_model: None,
            configured_models: Some(
                request
                    .config
                    .models
                    .iter()
                    .map(|model| vibex_core::ProviderConfiguredModel {
                        id: model.clone(),
                        display_name: None,
                        enabled: true,
                        wire_api: None,
                        capabilities: Default::default(),
                    })
                    .collect(),
            ),
            reasoning_effort: None,
            sandbox_defaults: None,
            network_defaults: None,
            permission_defaults: None,
            provider_options: Some(provider_options),
        })
    }

    pub fn duplicate_profile(
        &self,
        request: ProviderProfileDuplicateRequest,
    ) -> VibexResult<ProviderProfile> {
        validate_display_name(&request.display_name)?;
        let conn = self.open_connection()?;
        let source = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;
        let now = unix_timestamp_ms();
        let id = ProviderProfileId::new();
        let mut duplicate = ProviderProfile {
            id: id.clone(),
            display_name: request.display_name,
            status: ProviderProfileStatus::Enabled,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
            secrets: Vec::new(),
            ..source
        };
        duplicate.secrets = source
            .secrets
            .into_iter()
            .map(|secret| vibex_core::ProviderSecretReference {
                id: RequestId::new(),
                provider_profile_id: id.clone(),
                created_at_ms: now,
                updated_at_ms: now,
                ..secret
            })
            .collect();
        ProviderProfileRepository::insert(&conn, &duplicate)?;
        let created = ProviderProfileRepository::get(&conn, &duplicate.id)?.ok_or_else(|| {
            VibexError::storage(
                "provider_profile_duplicate_readback_failed",
                "failed to read provider profile after duplicate",
            )
        })?;
        self.sync_legacy_projection(&conn, &created)?;
        self.notify_profile_saved(&created);
        Ok(created)
    }

    pub fn delete_profile(&self, request: ProviderProfileDeleteRequest) -> VibexResult<()> {
        if is_local_default_profile(&request.provider_profile_id) {
            return Err(VibexError::validation(
                "provider_profile_default_delete_rejected",
                "local default provider profiles cannot be deleted",
            )
            .with_diagnostic("providerProfileId", request.provider_profile_id.as_str()));
        }
        let mut conn = self.open_connection()?;
        ProviderProfileRepository::soft_delete(&mut conn, &request.provider_profile_id)?;
        self.mark_legacy_projection_deleted(
            &conn,
            &request.provider_profile_id,
            unix_timestamp_ms(),
        )?;
        drop(conn);
        self.notify_profile_deleted(&request.provider_profile_id);
        Ok(())
    }

    pub fn get_default(
        &self,
        scope: ProviderProfileDefaultScope,
        provider_kind: ProviderKind,
    ) -> VibexResult<ProviderProfileDefaultSelection> {
        validate_default_scope(&scope)?;
        let conn = self.open_connection()?;
        ProviderDefaultProfileRepository::get(&conn, scope, provider_kind)
    }

    pub fn set_default(
        &self,
        request: ProviderProfileSetDefaultRequest,
    ) -> VibexResult<ProviderProfileDefaultSelection> {
        validate_default_scope(&request.scope)?;
        let conn = self.open_connection()?;
        ProviderDefaultProfileRepository::set(&conn, request)
    }

    pub fn preview_injection(
        &self,
        request: ProviderInjectionPreviewRequest,
    ) -> VibexResult<ProviderInjectionPreview> {
        let conn = self.open_connection()?;
        let profile = ProviderProfileRepository::get(&conn, &request.provider_profile_id)?
            .ok_or_else(|| {
                VibexError::validation(
                    "provider_profile_not_found",
                    "provider profile was not found",
                )
                .with_diagnostic("providerProfileId", request.provider_profile_id.as_str())
            })?;
        let mcp_servers =
            McpServerRepository::list_enabled_for_agent(&conn, &profile.agent_id, profile.kind)?;
        let skills =
            SkillRepository::list_enabled_for_agent(&conn, &profile.agent_id, profile.kind)?;
        let prompts = PromptRepository::list_enabled(&conn)?;
        let preview = build_preview(&profile, &mcp_servers, &skills, &prompts);
        if request.persist {
            ProviderInjectionPreviewRepository::insert(&conn, &request, &preview)?;
        }
        Ok(preview)
    }

    pub fn list_mcp_servers(&self) -> VibexResult<Vec<McpServer>> {
        let conn = self.open_connection()?;
        McpServerRepository::list(&conn)
    }

    pub fn create_mcp_server(&self, request: McpServerCreateRequest) -> VibexResult<McpServer> {
        validate_mcp_create_request(&request)?;
        let conn = self.open_connection()?;
        let server =
            McpServerRepository::from_create_request(normalize_mcp_create_request(request));
        McpServerRepository::insert(&conn, &server)?;
        McpServerRepository::get(&conn, &server.id)?.ok_or_else(|| {
            VibexError::storage(
                "mcp_server_create_readback_failed",
                "failed to read MCP server after create",
            )
        })
    }

    pub fn update_mcp_server(&self, request: McpServerUpdateRequest) -> VibexResult<McpServer> {
        let conn = self.open_connection()?;
        let mut server =
            McpServerRepository::get(&conn, &request.mcp_server_id)?.ok_or_else(|| {
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", request.mcp_server_id.as_str())
            })?;

        if let Some(display_name) = request.display_name {
            validate_mcp_display_name(&display_name)?;
            server.display_name = display_name;
        }
        if let Some(transport_kind) = request.transport_kind {
            server.transport_kind = transport_kind;
        }
        if let Some(status) = request.status {
            server.status = status;
        }
        if let Some(scope_kind) = request.scope_kind {
            server.scope_kind = scope_kind;
        }
        if request.project_id.is_some() {
            server.project_id = request.project_id;
        }
        if request.workspace_id.is_some() {
            server.workspace_id = request.workspace_id;
        }
        if request.command.is_some() {
            server.command = request.command;
        }
        if let Some(args) = request.args {
            server.args = args;
        }
        if request.url.is_some() {
            server.url = request.url;
        }
        if request.description.is_some() {
            server.description = request.description;
        }
        if let Some(tags) = request.tags {
            server.tags = tags;
        }
        validate_mcp_server_record(&server)?;
        server.updated_at_ms = unix_timestamp_ms();
        McpServerRepository::update(&conn, &server)?;
        McpServerRepository::get(&conn, &server.id)?.ok_or_else(|| {
            VibexError::storage(
                "mcp_server_update_readback_failed",
                "failed to read MCP server after update",
            )
        })
    }

    pub fn delete_mcp_server(&self, request: McpServerDeleteRequest) -> VibexResult<()> {
        let conn = self.open_connection()?;
        if McpServerRepository::get(&conn, &request.mcp_server_id)?.is_none() {
            return Err(
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", request.mcp_server_id.as_str()),
            );
        }
        McpServerRepository::soft_delete(&conn, &request.mcp_server_id)
    }

    pub fn set_mcp_server_provider_matrix(
        &self,
        request: McpServerSetProviderMatrixRequest,
    ) -> VibexResult<McpServer> {
        let conn = self.open_connection()?;
        if McpServerRepository::get(&conn, &request.mcp_server_id)?.is_none() {
            return Err(
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", request.mcp_server_id.as_str()),
            );
        }
        let now = unix_timestamp_ms();
        let matrix: Vec<_> = request
            .provider_matrix
            .into_iter()
            .map(|entry| McpServerProviderMatrix {
                provider_kind: entry.provider_kind,
                enabled: entry.enabled,
                updated_at_ms: now,
            })
            .collect();
        McpServerRepository::replace_provider_matrix(&conn, &request.mcp_server_id, &matrix)?;
        McpServerRepository::get(&conn, &request.mcp_server_id)?.ok_or_else(|| {
            VibexError::storage(
                "mcp_server_matrix_readback_failed",
                "failed to read MCP server after matrix update",
            )
        })
    }

    pub fn set_mcp_server_agent_matrix(
        &self,
        request: McpServerSetAgentMatrixRequest,
    ) -> VibexResult<McpServer> {
        let conn = self.open_connection()?;
        if McpServerRepository::get(&conn, &request.mcp_server_id)?.is_none() {
            return Err(
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", request.mcp_server_id.as_str()),
            );
        }
        let now = unix_timestamp_ms();
        let matrix = request
            .agent_matrix
            .into_iter()
            .map(|entry| McpServerAgentMatrix {
                agent_id: entry.agent_id,
                enabled: entry.enabled,
                source_kind: entry.source_kind,
                updated_at_ms: now,
            })
            .collect::<Vec<_>>();
        McpServerRepository::replace_agent_matrix(&conn, &request.mcp_server_id, &matrix)?;
        McpServerRepository::get(&conn, &request.mcp_server_id)?.ok_or_else(|| {
            VibexError::storage(
                "mcp_server_agent_matrix_readback_failed",
                "failed to read MCP server after agent matrix update",
            )
        })
    }

    pub fn list_mcp_server_agent_matrix(
        &self,
        request: McpServerAgentMatrixListRequest,
    ) -> VibexResult<Vec<McpServerAgentMatrix>> {
        let conn = self.open_connection()?;
        if McpServerRepository::get(&conn, &request.mcp_server_id)?.is_none() {
            return Err(
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", request.mcp_server_id.as_str()),
            );
        }
        McpServerRepository::list_agent_matrix(&conn, &request.mcp_server_id)
    }

    pub fn list_mcp_servers_for_agent(
        &self,
        request: McpServerForAgentListRequest,
    ) -> VibexResult<Vec<McpServer>> {
        let conn = self.open_connection()?;
        McpServerRepository::list_enabled_for_agent(&conn, &request.agent_id, request.provider_kind)
    }

    pub fn discover_mcp_sources(
        &self,
        request: McpServerDiscoverRequest,
    ) -> VibexResult<McpServerDiscoveryResponse> {
        let conn = self.open_connection()?;
        let existing = McpServerRepository::list(&conn)?;
        let mut discoveries = Vec::new();
        let mut diagnostics = Vec::new();
        for agent in self.import_scan_agents(request.source_agent_id)? {
            let response = discover_mcp_sources_for_agent(&agent, &existing);
            discoveries.extend(response.discoveries);
            diagnostics.extend(response.diagnostics);
        }
        Ok(McpServerDiscoveryResponse {
            discoveries,
            diagnostics,
        })
    }

    pub fn import_mcp_servers(
        &self,
        request: McpServerImportRequest,
    ) -> VibexResult<McpServerImportResult> {
        let conn = self.open_connection()?;
        let now = unix_timestamp_ms();
        let mut imported = Vec::new();
        let mut created_count = 0;
        let mut updated_count = 0;
        let mut diagnostics = Vec::new();

        for selection in request.selections {
            validate_mcp_create_request(&selection.candidate)?;
            let existing = find_existing_mcp_server(&conn, &selection.candidate)?;
            let mut server = if let Some(existing) = existing {
                updated_count += 1;
                merge_mcp_secret_references(existing, &selection.candidate)
            } else {
                created_count += 1;
                McpServerRepository::from_create_request(normalize_mcp_create_request(
                    selection.candidate.clone(),
                ))
            };
            merge_mcp_agent_matrix(
                &mut server.agent_matrix,
                import_enabled_agents(selection.source_agent_id, selection.enable_agent_ids),
                now,
            );
            if McpServerRepository::get(&conn, &server.id)?.is_some() {
                McpServerRepository::update(&conn, &server)?;
                McpServerRepository::replace_agent_matrix(&conn, &server.id, &server.agent_matrix)?;
            } else {
                McpServerRepository::insert(&conn, &server)?;
            }
            if let Some(readback) = McpServerRepository::get(&conn, &server.id)? {
                imported.push(readback);
            } else {
                diagnostics.push(diagnostic("mcpImportReadbackMissing", server.id.as_str()));
            }
        }

        Ok(McpServerImportResult {
            imported,
            created_count,
            updated_count,
            diagnostics,
        })
    }

    pub fn validate_mcp_server(
        &self,
        request: McpServerValidateRequest,
    ) -> VibexResult<McpServerValidationResult> {
        let conn = self.open_connection()?;
        let server = match (request.mcp_server_id, request.candidate) {
            (Some(id), _) => McpServerRepository::get(&conn, &id)?.ok_or_else(|| {
                VibexError::validation("mcp_server_not_found", "MCP server was not found")
                    .with_diagnostic("mcpServerId", id.as_str())
            })?,
            (None, Some(candidate)) => {
                McpServerRepository::from_create_request(normalize_mcp_create_request(candidate))
            }
            (None, None) => {
                return Err(VibexError::validation(
                    "mcp_server_validation_target_missing",
                    "MCP validation requires a server id or candidate",
                ));
            }
        };
        Ok(validate_mcp_server_result(&server))
    }

    pub fn list_skills(&self) -> VibexResult<Vec<Skill>> {
        let conn = self.open_connection()?;
        SkillRepository::list(&conn)
    }

    pub fn create_skill(&self, request: SkillCreateRequest) -> VibexResult<Skill> {
        validate_skill_create_request(&request)?;
        let conn = self.open_connection()?;
        let skill = SkillRepository::from_create_request(normalize_skill_create_request(request));
        SkillRepository::insert(&conn, &skill)?;
        SkillRepository::get(&conn, &skill.id)?.ok_or_else(|| {
            VibexError::storage(
                "skill_create_readback_failed",
                "failed to read Skill after create",
            )
        })
    }

    pub fn update_skill(&self, request: SkillUpdateRequest) -> VibexResult<Skill> {
        let conn = self.open_connection()?;
        let mut skill = SkillRepository::get(&conn, &request.skill_id)?.ok_or_else(|| {
            VibexError::validation("skill_not_found", "Skill was not found")
                .with_diagnostic("skillId", request.skill_id.as_str())
        })?;

        if let Some(display_name) = request.display_name {
            validate_skill_display_name(&display_name)?;
            skill.display_name = display_name;
        }
        if let Some(source_kind) = request.source_kind {
            skill.source_kind = source_kind;
        }
        if let Some(status) = request.status {
            skill.status = status;
        }
        if let Some(scope_kind) = request.scope_kind {
            skill.scope_kind = scope_kind;
        }
        if request.project_id.is_some() {
            skill.project_id = request.project_id;
        }
        if request.workspace_id.is_some() {
            skill.workspace_id = request.workspace_id;
        }
        if request.source_uri.is_some() {
            skill.source_uri = request.source_uri;
        }
        if request.description.is_some() {
            skill.description = request.description;
        }
        if let Some(tags) = request.tags {
            skill.tags = tags;
        }
        if request.content_preview.is_some() {
            skill.content_preview = request.content_preview;
        }
        validate_skill_record(&skill)?;
        skill.updated_at_ms = unix_timestamp_ms();
        SkillRepository::update(&conn, &skill)?;
        SkillRepository::get(&conn, &skill.id)?.ok_or_else(|| {
            VibexError::storage(
                "skill_update_readback_failed",
                "failed to read Skill after update",
            )
        })
    }

    pub fn delete_skill(&self, request: SkillDeleteRequest) -> VibexResult<()> {
        let conn = self.open_connection()?;
        if SkillRepository::get(&conn, &request.skill_id)?.is_none() {
            return Err(
                VibexError::validation("skill_not_found", "Skill was not found")
                    .with_diagnostic("skillId", request.skill_id.as_str()),
            );
        }
        SkillRepository::soft_delete(&conn, &request.skill_id)
    }

    pub fn set_skill_provider_matrix(
        &self,
        request: SkillSetProviderMatrixRequest,
    ) -> VibexResult<Skill> {
        let conn = self.open_connection()?;
        if SkillRepository::get(&conn, &request.skill_id)?.is_none() {
            return Err(
                VibexError::validation("skill_not_found", "Skill was not found")
                    .with_diagnostic("skillId", request.skill_id.as_str()),
            );
        }
        let now = unix_timestamp_ms();
        let matrix = request
            .provider_matrix
            .into_iter()
            .map(|entry| SkillProviderMatrix {
                provider_kind: entry.provider_kind,
                enabled: entry.enabled,
                updated_at_ms: now,
            })
            .collect::<Vec<_>>();
        SkillRepository::replace_provider_matrix(&conn, &request.skill_id, &matrix)?;
        SkillRepository::get(&conn, &request.skill_id)?.ok_or_else(|| {
            VibexError::storage(
                "skill_matrix_readback_failed",
                "failed to read Skill after matrix update",
            )
        })
    }

    pub fn set_skill_agent_matrix(
        &self,
        request: SkillSetAgentMatrixRequest,
    ) -> VibexResult<Skill> {
        let conn = self.open_connection()?;
        if SkillRepository::get(&conn, &request.skill_id)?.is_none() {
            return Err(
                VibexError::validation("skill_not_found", "Skill was not found")
                    .with_diagnostic("skillId", request.skill_id.as_str()),
            );
        }
        let now = unix_timestamp_ms();
        let matrix = request
            .agent_matrix
            .into_iter()
            .map(|entry| SkillAgentMatrix {
                agent_id: entry.agent_id,
                enabled: entry.enabled,
                source_kind: entry.source_kind,
                updated_at_ms: now,
            })
            .collect::<Vec<_>>();
        SkillRepository::replace_agent_matrix(&conn, &request.skill_id, &matrix)?;
        SkillRepository::get(&conn, &request.skill_id)?.ok_or_else(|| {
            VibexError::storage(
                "skill_agent_matrix_readback_failed",
                "failed to read Skill after agent matrix update",
            )
        })
    }

    pub fn list_skill_agent_matrix(
        &self,
        request: SkillAgentMatrixListRequest,
    ) -> VibexResult<Vec<SkillAgentMatrix>> {
        let conn = self.open_connection()?;
        if SkillRepository::get(&conn, &request.skill_id)?.is_none() {
            return Err(
                VibexError::validation("skill_not_found", "Skill was not found")
                    .with_diagnostic("skillId", request.skill_id.as_str()),
            );
        }
        SkillRepository::list_agent_matrix(&conn, &request.skill_id)
    }

    pub fn list_skills_for_agent(
        &self,
        request: SkillForAgentListRequest,
    ) -> VibexResult<Vec<Skill>> {
        let conn = self.open_connection()?;
        SkillRepository::list_enabled_for_agent(&conn, &request.agent_id, request.provider_kind)
    }

    pub fn discover_skill_sources(
        &self,
        request: SkillDiscoverRequest,
    ) -> VibexResult<SkillDiscoveryResponse> {
        let conn = self.open_connection()?;
        let existing = SkillRepository::list(&conn)?;
        let entries = self.scan_local_skills(skills::LocalSkillScanRequest {
            source_agent_id: request.source_agent_id,
            workspace_id: request.workspace_id,
        })?;
        let discoveries = entries
            .into_iter()
            .map(|entry| {
                let source_path = entry.manifest_path.display().to_string();
                let existing_skill_id = existing
                    .iter()
                    .find(|skill| skill.source_uri.as_deref() == Some(source_path.as_str()))
                    .map(|skill| skill.id.clone());
                SkillDiscovery {
                    discovery_id: format!(
                        "skill:local:{}:{}",
                        entry.root_source, entry.source_hash
                    ),
                    source_agent_id: entry.source_agent_id,
                    source_path: source_path.clone(),
                    import_key: source_path,
                    status: if existing_skill_id.is_some() {
                        ResourceDiscoveryStatus::AlreadyImported
                    } else {
                        ResourceDiscoveryStatus::Importable
                    },
                    display_name: entry
                        .name
                        .clone()
                        .unwrap_or_else(|| entry.command_name.clone()),
                    command_name: entry.command_name,
                    description: entry.description,
                    content_preview: entry.content_preview,
                    existing_skill_id,
                    diagnostics: Vec::new(),
                }
            })
            .collect();
        Ok(SkillDiscoveryResponse {
            discoveries,
            diagnostics: Vec::new(),
        })
    }

    pub fn import_skills(&self, request: SkillImportRequest) -> VibexResult<SkillImportResult> {
        let conn = self.open_connection()?;
        let now = unix_timestamp_ms();
        let mut imported = Vec::new();
        let mut created_count = 0;
        let mut updated_count = 0;

        for selection in request.selections {
            let mut skill = find_existing_skill_by_source_uri(&conn, &selection.source_path)?
                .unwrap_or_else(|| {
                    created_count += 1;
                    SkillRepository::from_create_request(normalize_skill_create_request(
                        SkillCreateRequest {
                            display_name: selection.display_name.clone(),
                            source_kind: SkillSourceKind::LocalFolder,
                            status: vibex_core::SkillStatus::Enabled,
                            scope_kind: vibex_core::SkillScopeKind::User,
                            project_id: None,
                            workspace_id: None,
                            source_uri: Some(selection.source_path.clone()),
                            description: selection.description.clone(),
                            tags: vec!["imported".to_string(), "local".to_string()],
                            content_preview: selection.content_preview.clone(),
                            provider_matrix: Vec::new(),
                        },
                    ))
                });
            if SkillRepository::get(&conn, &skill.id)?.is_some() {
                updated_count += 1;
                SkillRepository::update(&conn, &skill)?;
            } else {
                SkillRepository::insert(&conn, &skill)?;
            }
            merge_skill_agent_matrix(
                &mut skill.agent_matrix,
                import_enabled_agents(selection.source_agent_id, selection.enable_agent_ids),
                now,
            );
            SkillRepository::replace_agent_matrix(&conn, &skill.id, &skill.agent_matrix)?;
            if let Some(readback) = SkillRepository::get(&conn, &skill.id)? {
                imported.push(readback);
            }
        }

        Ok(SkillImportResult {
            imported,
            created_count,
            updated_count,
            diagnostics: Vec::new(),
        })
    }

    pub fn validate_skill(
        &self,
        request: SkillValidateRequest,
    ) -> VibexResult<SkillValidationResult> {
        let conn = self.open_connection()?;
        let skill = match (request.skill_id, request.candidate) {
            (Some(id), _) => SkillRepository::get(&conn, &id)?.ok_or_else(|| {
                VibexError::validation("skill_not_found", "Skill was not found")
                    .with_diagnostic("skillId", id.as_str())
            })?,
            (None, Some(candidate)) => {
                SkillRepository::from_create_request(normalize_skill_create_request(candidate))
            }
            (None, None) => {
                return Err(VibexError::validation(
                    "skill_validation_target_missing",
                    "Skill validation requires a skill id or candidate",
                ));
            }
        };
        Ok(validate_skill_result(&skill))
    }

    pub fn list_prompts(&self) -> VibexResult<Vec<Prompt>> {
        let conn = self.open_connection()?;
        PromptRepository::list(&conn)
    }

    pub fn create_prompt(&self, request: PromptCreateRequest) -> VibexResult<Prompt> {
        validate_prompt_create_request(&request)?;
        let conn = self.open_connection()?;
        let prompt = PromptRepository::from_create_request(request);
        PromptRepository::insert(&conn, &prompt)?;
        PromptRepository::get(&conn, &prompt.id)?.ok_or_else(|| {
            VibexError::storage(
                "prompt_create_readback_failed",
                "failed to read Prompt after create",
            )
        })
    }

    pub fn update_prompt(&self, request: PromptUpdateRequest) -> VibexResult<Prompt> {
        let conn = self.open_connection()?;
        let mut prompt = PromptRepository::get(&conn, &request.prompt_id)?.ok_or_else(|| {
            VibexError::validation("prompt_not_found", "Prompt was not found")
                .with_diagnostic("promptId", request.prompt_id.as_str())
        })?;

        if let Some(display_name) = request.display_name {
            validate_prompt_display_name(&display_name)?;
            prompt.display_name = display_name;
        }
        if let Some(kind) = request.kind {
            prompt.kind = kind;
        }
        if let Some(status) = request.status {
            prompt.status = status;
        }
        if let Some(scope_kind) = request.scope_kind {
            prompt.scope_kind = scope_kind;
        }
        if request.project_id.is_some() {
            prompt.project_id = request.project_id;
        }
        if request.workspace_id.is_some() {
            prompt.workspace_id = request.workspace_id;
        }
        if let Some(body) = request.body {
            validate_prompt_body(&body)?;
            prompt.body = body;
        }
        if request.description.is_some() {
            prompt.description = request.description;
        }
        if let Some(tags) = request.tags {
            prompt.tags = tags;
        }
        validate_prompt_record(&prompt)?;
        prompt.updated_at_ms = unix_timestamp_ms();
        PromptRepository::update(&conn, &prompt)?;
        PromptRepository::get(&conn, &prompt.id)?.ok_or_else(|| {
            VibexError::storage(
                "prompt_update_readback_failed",
                "failed to read Prompt after update",
            )
        })
    }

    pub fn delete_prompt(&self, request: PromptDeleteRequest) -> VibexResult<()> {
        let conn = self.open_connection()?;
        if PromptRepository::get(&conn, &request.prompt_id)?.is_none() {
            return Err(
                VibexError::validation("prompt_not_found", "Prompt was not found")
                    .with_diagnostic("promptId", request.prompt_id.as_str()),
            );
        }
        PromptRepository::soft_delete(&conn, &request.prompt_id)
    }

    pub fn validate_prompt(
        &self,
        request: PromptValidateRequest,
    ) -> VibexResult<PromptValidationResult> {
        let conn = self.open_connection()?;
        let prompt = match (request.prompt_id, request.candidate) {
            (Some(id), _) => PromptRepository::get(&conn, &id)?.ok_or_else(|| {
                VibexError::validation("prompt_not_found", "Prompt was not found")
                    .with_diagnostic("promptId", id.as_str())
            })?,
            (None, Some(candidate)) => PromptRepository::from_create_request(candidate),
            (None, None) => {
                return Err(VibexError::validation(
                    "prompt_validation_target_missing",
                    "Prompt validation requires a prompt id or candidate",
                ));
            }
        };
        Ok(validate_prompt_result(&prompt))
    }

    pub fn list_hooks(&self) -> VibexResult<Vec<Hook>> {
        let conn = self.open_connection()?;
        HookRepository::list(&conn)
    }

    pub fn create_hook(&self, request: HookCreateRequest) -> VibexResult<Hook> {
        validate_hook_display_name(&request.display_name)?;
        let conn = self.open_connection()?;
        let hook = HookRepository::from_create_request(request);
        HookRepository::insert(&conn, &hook)?;
        HookRepository::get(&conn, &hook.id)?.ok_or_else(|| {
            VibexError::storage(
                "hook_create_readback_failed",
                "failed to read Hook after create",
            )
        })
    }

    pub fn update_hook(&self, request: HookUpdateRequest) -> VibexResult<Hook> {
        let conn = self.open_connection()?;
        let mut hook = HookRepository::get(&conn, &request.hook_id)?.ok_or_else(|| {
            VibexError::validation("hook_not_found", "Hook was not found")
                .with_diagnostic("hookId", request.hook_id.as_str())
        })?;

        if let Some(display_name) = request.display_name {
            validate_hook_display_name(&display_name)?;
            hook.display_name = display_name;
        }
        if let Some(provider_kind) = request.provider_kind {
            hook.provider_kind = provider_kind;
        }
        if let Some(event_kind) = request.event_kind {
            hook.event_kind = event_kind;
        }
        if let Some(status) = request.status {
            hook.status = status;
        }
        if let Some(install_state) = request.install_state {
            hook.install_state = install_state;
        }
        if request.command_preview.is_some() {
            hook.command_preview = request.command_preview;
        }
        if let Some(managed_marker) = request.managed_marker {
            hook.managed_marker = managed_marker;
        }
        if request.description.is_some() {
            hook.description = request.description;
        }
        hook.updated_at_ms = unix_timestamp_ms();
        HookRepository::update(&conn, &hook)?;
        HookRepository::get(&conn, &hook.id)?.ok_or_else(|| {
            VibexError::storage(
                "hook_update_readback_failed",
                "failed to read Hook after update",
            )
        })
    }

    pub fn delete_hook(&self, request: HookDeleteRequest) -> VibexResult<()> {
        let conn = self.open_connection()?;
        if HookRepository::get(&conn, &request.hook_id)?.is_none() {
            return Err(
                VibexError::validation("hook_not_found", "Hook was not found")
                    .with_diagnostic("hookId", request.hook_id.as_str()),
            );
        }
        HookRepository::soft_delete(&conn, &request.hook_id)
    }

    pub fn preview_hook_install(
        &self,
        request: HookInstallPreviewRequest,
    ) -> VibexResult<HookInstallPreview> {
        let conn = self.open_connection()?;
        let mut hook = HookRepository::get(&conn, &request.hook_id)?.ok_or_else(|| {
            VibexError::validation("hook_not_found", "Hook was not found")
                .with_diagnostic("hookId", request.hook_id.as_str())
        })?;
        let preview = build_hook_install_preview(&hook, request.target_path);
        HookRepository::insert_install_preview(&conn, &preview)?;
        hook.install_state = HookInstallState::PreviewOnly;
        hook.updated_at_ms = unix_timestamp_ms();
        HookRepository::update(&conn, &hook)?;
        Ok(preview)
    }

    pub fn list_health_summaries(&self) -> VibexResult<Vec<ProviderHealthSummary>> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let records = ProviderHealthRepository::list_latest(&conn)?;
        Ok(build_health_summaries(&profiles, &records))
    }

    pub fn run_health_probes(
        &self,
        request: ProviderRunHealthProbesRequest,
    ) -> VibexResult<ProviderRunHealthProbesResult> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let selected_profiles = filter_profiles(&profiles, request.provider_profile_ids.as_ref());
        let probe_kinds = request
            .probe_kinds
            .unwrap_or_else(|| ProviderHealthProbeKind::all().to_vec());
        let now = unix_timestamp_ms();
        let mut results = Vec::new();

        for profile in selected_profiles {
            for probe_kind in &probe_kinds {
                let result = provider_health_probe_result(profile, *probe_kind, now)?;
                ProviderHealthRepository::insert(&conn, &result)?;
                results.push(result);
            }
        }

        let latest = ProviderHealthRepository::list_latest(&conn)?;
        let summaries = build_health_summaries(&profiles, &latest);
        Ok(ProviderRunHealthProbesResult {
            results,
            summaries,
            created_at_ms: now,
        })
    }

    pub fn list_capability_summaries(&self) -> VibexResult<Vec<ProviderCapabilitySummary>> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let records = ProviderCapabilityRepository::list_latest(&conn)?;
        Ok(build_capability_summaries(
            &profiles,
            &records,
            unix_timestamp_ms(),
        ))
    }

    pub fn run_capability_probes(
        &self,
        request: ProviderRunCapabilityProbesRequest,
    ) -> VibexResult<ProviderRunCapabilityProbesResult> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let selected_profiles = filter_profiles(&profiles, request.provider_profile_ids.as_ref());
        let now = unix_timestamp_ms();
        let mut results = Vec::new();

        for profile in selected_profiles {
            let result = deterministic_capability_probe_result(profile, now);
            ProviderCapabilityRepository::insert(&conn, &result)?;
            results.push(result);
        }

        let latest = ProviderCapabilityRepository::list_latest(&conn)?;
        let summaries = build_capability_summaries(&profiles, &latest, now);
        Ok(ProviderRunCapabilityProbesResult {
            results,
            summaries,
            created_at_ms: now,
        })
    }

    pub fn list_usage_summaries(
        &self,
        request: ProviderUsageListRequest,
    ) -> VibexResult<Vec<ProviderUsageSummary>> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let selected_profiles = filter_profiles(&profiles, request.provider_profile_ids.as_ref());
        let records = ProviderUsageRepository::list_latest(&conn)?;
        Ok(build_usage_summaries(
            &selected_profiles,
            &records,
            request.include_empty,
        ))
    }

    pub fn list_failover_recommendations(
        &self,
        request: ProviderFailoverRecommendationRequest,
    ) -> VibexResult<Vec<ProviderFailoverRecommendation>> {
        let conn = self.open_connection()?;
        let profiles =
            visible_model_provider_profiles(&conn, ProviderProfileRepository::list(&conn)?)?;
        let selected_profiles = filter_profiles(&profiles, request.provider_profile_ids.as_ref());
        let health_records = ProviderHealthRepository::list_latest(&conn)?;
        let usage_records = ProviderUsageRepository::list_latest(&conn)?;
        let health = build_health_summaries(&profiles, &health_records);
        let all_profile_refs: Vec<_> = profiles.iter().collect();
        let usage = build_usage_summaries(&all_profile_refs, &usage_records, true);
        Ok(build_failover_recommendations(
            &selected_profiles,
            &profiles,
            &health,
            &usage,
        ))
    }

    fn open_connection(&self) -> VibexResult<vibex_db::DbConnection> {
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        Ok(conn)
    }
}

const ACP_CONFIG_OPTION_KEY: &str = "acp.config.v1";
const INTERNAL_PROFILE_ROLE_OPTION_KEY: &str = "vibex.internal.profileRole";
const INTERNAL_AGENT_RUNTIME_PROFILE_ROLE: &str = "agent_runtime";
const OPENCODE_PRESET_ID: &str = "opencode";
const DEFAULT_AGENT_CWD_SCOPE: &str = "home";

fn import_enabled_agents(
    source_agent_id: AgentId,
    explicit_agent_ids: Vec<AgentId>,
) -> Vec<AgentId> {
    if explicit_agent_ids.is_empty() {
        vec![source_agent_id]
    } else {
        let mut seen = HashSet::new();
        explicit_agent_ids
            .into_iter()
            .filter(|agent_id| seen.insert(agent_id.clone()))
            .collect()
    }
}

fn merge_mcp_agent_matrix(
    matrix: &mut Vec<McpServerAgentMatrix>,
    enabled_agent_ids: Vec<AgentId>,
    now: i64,
) {
    for agent_id in enabled_agent_ids {
        if let Some(entry) = matrix.iter_mut().find(|entry| entry.agent_id == agent_id) {
            entry.enabled = true;
            entry.updated_at_ms = now;
        } else {
            matrix.push(McpServerAgentMatrix {
                agent_id,
                enabled: true,
                source_kind: ResourceAgentMatrixSourceKind::NativeImport,
                updated_at_ms: now,
            });
        }
    }
}

fn merge_skill_agent_matrix(
    matrix: &mut Vec<SkillAgentMatrix>,
    enabled_agent_ids: Vec<AgentId>,
    now: i64,
) {
    for agent_id in enabled_agent_ids {
        if let Some(entry) = matrix.iter_mut().find(|entry| entry.agent_id == agent_id) {
            entry.enabled = true;
            entry.updated_at_ms = now;
        } else {
            matrix.push(SkillAgentMatrix {
                agent_id,
                enabled: true,
                source_kind: ResourceAgentMatrixSourceKind::NativeImport,
                updated_at_ms: now,
            });
        }
    }
}

impl ProviderConfigService {
    fn import_scan_agents(
        &self,
        source_agent_id: Option<AgentId>,
    ) -> VibexResult<Vec<AgentSnapshotEntry>> {
        let conn = self.open_connection()?;
        let definitions = builtin_agent_definitions();
        let configs = AgentConfigRepository::list(&conn)?;
        let discoveries =
            AgentDiscoveryRepository::latest_by_agent(&conn, DEFAULT_AGENT_CWD_SCOPE)?;
        let managed_installations = AgentManagedInstallationRepository::list(&conn)?
            .into_iter()
            .map(|record| (record.agent_id.clone(), record.state))
            .collect::<HashMap<_, _>>();
        let snapshots =
            build_agent_snapshots(definitions, configs, discoveries, &managed_installations);
        if let Some(source_agent_id) = source_agent_id {
            let agent = snapshots
                .into_iter()
                .find(|agent| agent.id == source_agent_id)
                .ok_or_else(|| {
                    VibexError::validation("agent_not_found", "Agent was not found")
                        .with_diagnostic("agentId", source_agent_id.as_str())
                })?;
            return Ok(vec![agent]);
        }
        Ok(snapshots.into_iter().filter(|agent| agent.added).collect())
    }
}

fn discover_mcp_sources_for_agent(
    agent: &AgentSnapshotEntry,
    existing: &[McpServer],
) -> McpServerDiscoveryResponse {
    let mut discoveries = Vec::new();
    let mut diagnostics = Vec::new();
    for path in mcp_existing_config_paths_for_agent(agent) {
        let source_path = path.to_string_lossy().to_string();
        let Ok(content) = fs::read_to_string(&path) else {
            diagnostics.push(diagnostic("mcpSourceUnreadable", source_path));
            continue;
        };
        let candidates = parse_mcp_candidates_for_path(
            &agent.id,
            &source_path,
            &path,
            &content,
            &mut diagnostics,
        );
        for candidate in candidates {
            let existing_mcp_server_id = existing
                .iter()
                .find(|server| mcp_candidate_matches_existing(server, &candidate))
                .map(|server| server.id.clone());
            discoveries.push(McpServerDiscovery {
                discovery_id: format!(
                    "mcp:{}:{}",
                    agent.id.as_str(),
                    skills::stable_hash_hex(format!("{}:{}", source_path, candidate.display_name))
                ),
                source_agent_id: agent.id.clone(),
                source_path: source_path.clone(),
                import_key: format!("{}:{}", source_path, candidate.display_name),
                status: if existing_mcp_server_id.is_some() {
                    ResourceDiscoveryStatus::AlreadyImported
                } else {
                    ResourceDiscoveryStatus::Importable
                },
                candidate: Some(candidate),
                existing_mcp_server_id,
                diagnostics: Vec::new(),
            });
        }
    }
    McpServerDiscoveryResponse {
        discoveries,
        diagnostics,
    }
}

pub(crate) fn import_scan_agent_skill_roots(agent: &AgentSnapshotEntry) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let mut seen = HashSet::new();
    for entry in configured_agent_paths(agent) {
        match entry.kind {
            ConfiguredAgentPathKind::SkillRoot => {
                push_path_unique_raw(&mut roots, &mut seen, entry.path);
                continue;
            }
            ConfiguredAgentPathKind::McpConfig => continue,
            ConfiguredAgentPathKind::AgentRoot => {}
        }
        let Some(root) = agent_home_from_config_path(&agent.id, &entry.path) else {
            continue;
        };
        push_skill_root_candidate(&mut roots, &mut seen, root);
    }
    for root in fallback_agent_roots(&agent.id) {
        push_skill_root_candidate(&mut roots, &mut seen, root);
    }
    roots
}

#[derive(Debug, Clone)]
struct ConfiguredAgentPath {
    path: PathBuf,
    kind: ConfiguredAgentPathKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ConfiguredAgentPathKind {
    AgentRoot,
    McpConfig,
    SkillRoot,
}

fn agent_home_from_config_path(agent_id: &AgentId, path: &Path) -> Option<PathBuf> {
    if !path.extension().is_some() {
        return Some(path.to_path_buf());
    }
    match agent_id.as_str() {
        "claude" if path.file_name().and_then(|name| name.to_str()) == Some(".claude.json") => {
            path.parent().map(|parent| parent.join(".claude"))
        }
        _ => path.parent().map(Path::to_path_buf),
    }
}

fn mcp_existing_config_paths_for_agent(agent: &AgentSnapshotEntry) -> Vec<PathBuf> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for entry in configured_agent_paths(agent) {
        if entry.kind == ConfiguredAgentPathKind::SkillRoot {
            continue;
        }
        let path = entry.path;
        if path.extension().is_some() {
            push_path_unique_raw(&mut paths, &mut seen, path);
        } else {
            push_mcp_config_candidates_for_root(&mut paths, &mut seen, &agent.id, &path);
        }
    }
    for root in fallback_agent_roots(&agent.id) {
        push_mcp_config_candidates_for_root(&mut paths, &mut seen, &agent.id, &root);
    }
    if agent.id.as_str() == "claude"
        && let Some(home) = dirs::home_dir()
    {
        push_path_unique_raw(&mut paths, &mut seen, home.join(".claude.json"));
    }
    paths
        .into_iter()
        .filter(|path| {
            fs::metadata(path)
                .map(|metadata| metadata.is_file())
                .unwrap_or(false)
        })
        .collect()
}

fn configured_agent_paths(agent: &AgentSnapshotEntry) -> Vec<ConfiguredAgentPath> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    for path in &agent.native_config_paths {
        push_non_empty_path(
            &mut paths,
            &mut seen,
            path,
            ConfiguredAgentPathKind::AgentRoot,
        );
    }
    for key in [
        "CODEX_HOME",
        "AGENTS_HOME",
        "CLAUDE_HOME",
        "OPENCODE_HOME",
        "OPENCODE_CONFIG_DIR",
        "GEMINI_HOME",
        "GEMINI_CONFIG_DIR",
        "QWEN_CODE_HOME",
        "QWEN_HOME",
        "GOOSE_HOME",
        "COPILOT_HOME",
        "HERMES_HOME",
        "KIMI_CODE_HOME",
        "KIMI_HOME",
        "CLINE_HOME",
        "CODEBUDDY_HOME",
        "OPENCLAW_HOME",
        "XDG_CONFIG_HOME",
    ] {
        if let Some(value) = agent.env.get(key)
            && let Some(path) = agent_env_config_root(&agent.id, key, value)
        {
            push_path_unique(
                &mut paths,
                &mut seen,
                path,
                ConfiguredAgentPathKind::AgentRoot,
            );
        }
    }
    let dynamic_home_key = format!("{}_HOME", agent_env_prefix(agent.id.as_str()));
    if let Some(value) = agent.env.get(&dynamic_home_key)
        && let Some(path) = agent_env_config_root(&agent.id, &dynamic_home_key, value)
    {
        push_path_unique(
            &mut paths,
            &mut seen,
            path,
            ConfiguredAgentPathKind::AgentRoot,
        );
    }
    push_paths_from_json_value(&mut paths, &mut seen, &agent.params);
    paths
}

fn agent_env_config_root(agent_id: &AgentId, key: &str, value: &str) -> Option<PathBuf> {
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    let path = expand_home_path(value);
    if key == "XDG_CONFIG_HOME" {
        Some(path.join(xdg_agent_config_dir_name(agent_id.as_str())))
    } else {
        Some(path)
    }
}

fn xdg_agent_config_dir_name(agent_id: &str) -> &str {
    match agent_id {
        "copilot" => "github-copilot",
        "qwen-code" => "qwen",
        other => other,
    }
}

fn push_paths_from_json_value(
    paths: &mut Vec<ConfiguredAgentPath>,
    seen: &mut HashSet<String>,
    value: &serde_json::Value,
) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in [
        "agentDir",
        "agentDirectory",
        "configDir",
        "configDirectory",
        "homeDir",
        "homeDirectory",
        "nativeConfigPath",
        "nativeConfigDir",
    ] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::AgentRoot);
        }
    }
    for key in [
        "agentDirs",
        "configDirs",
        "homeDirs",
        "nativeConfigPaths",
        "nativeConfigDirs",
    ] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::AgentRoot);
        }
    }
    for key in ["mcpConfigPath", "mcpConfigDir"] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::McpConfig);
        }
    }
    for key in ["mcpConfigPaths", "mcpConfigDirs"] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::McpConfig);
        }
    }
    for key in ["skillsDir", "skillDir", "skillsPath", "skillPath"] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::SkillRoot);
        }
    }
    for key in ["skillsDirs", "skillDirs", "skillsPaths", "skillPaths"] {
        if let Some(value) = object.get(key) {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::SkillRoot);
        }
    }
    for key in ["paths", "directories"] {
        if let Some(value) = object.get(key) {
            push_paths_from_nested_json(paths, seen, value);
        }
    }
}

fn push_paths_from_nested_json(
    paths: &mut Vec<ConfiguredAgentPath>,
    seen: &mut HashSet<String>,
    value: &serde_json::Value,
) {
    match value {
        serde_json::Value::Array(values) => {
            for value in values {
                push_path_value(paths, seen, value, ConfiguredAgentPathKind::AgentRoot);
            }
        }
        serde_json::Value::Object(object) => {
            for value in object.values() {
                match value {
                    serde_json::Value::Array(_) | serde_json::Value::String(_) => {
                        push_path_value(paths, seen, value, ConfiguredAgentPathKind::AgentRoot);
                    }
                    serde_json::Value::Object(_) => push_paths_from_nested_json(paths, seen, value),
                    _ => {}
                }
            }
        }
        serde_json::Value::String(_) => {
            push_path_value(paths, seen, value, ConfiguredAgentPathKind::AgentRoot);
        }
        _ => {}
    }
}

fn push_path_value(
    paths: &mut Vec<ConfiguredAgentPath>,
    seen: &mut HashSet<String>,
    value: &serde_json::Value,
    kind: ConfiguredAgentPathKind,
) {
    match value {
        serde_json::Value::String(value) => push_non_empty_path(paths, seen, value, kind),
        serde_json::Value::Array(values) => {
            for value in values {
                push_path_value(paths, seen, value, kind);
            }
        }
        _ => {}
    }
}

fn push_non_empty_path(
    paths: &mut Vec<ConfiguredAgentPath>,
    seen: &mut HashSet<String>,
    value: &str,
    kind: ConfiguredAgentPathKind,
) {
    let value = value.trim();
    if value.is_empty() {
        return;
    }
    push_path_unique(paths, seen, expand_home_path(value), kind);
}

fn expand_home_path(value: &str) -> PathBuf {
    if value == "~" {
        if let Some(home) = dirs::home_dir() {
            return home;
        }
    } else if let Some(rest) = value.strip_prefix("~/")
        && let Some(home) = dirs::home_dir()
    {
        return home.join(rest);
    }
    PathBuf::from(value)
}

fn push_path_unique(
    paths: &mut Vec<ConfiguredAgentPath>,
    seen: &mut HashSet<String>,
    path: PathBuf,
    kind: ConfiguredAgentPathKind,
) {
    let key = comparable_path_key(&path);
    if seen.insert(key) {
        paths.push(ConfiguredAgentPath { path, kind });
    }
}

fn push_path_unique_raw(paths: &mut Vec<PathBuf>, seen: &mut HashSet<String>, path: PathBuf) {
    let key = comparable_path_key(&path);
    if seen.insert(key) {
        paths.push(path);
    }
}

fn push_skill_root_candidate(roots: &mut Vec<PathBuf>, seen: &mut HashSet<String>, root: PathBuf) {
    let skill_root = if root.file_name().and_then(|name| name.to_str()) == Some("skills") {
        root
    } else {
        root.join("skills")
    };
    push_path_unique_raw(roots, seen, skill_root);
}

fn fallback_agent_roots(agent_id: &AgentId) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    let Some(home) = dirs::home_dir() else {
        return roots;
    };
    match agent_id.as_str() {
        "codex" => {
            roots.push(
                env::var_os("CODEX_HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|| home.join(".codex")),
            );
        }
        "claude" => {
            roots.push(home.join(".claude"));
        }
        "gemini" => {
            roots.push(
                non_empty_env_path("GEMINI_HOME")
                    .or_else(|| non_empty_env_path("GEMINI_CONFIG_DIR"))
                    .unwrap_or_else(|| home.join(".gemini")),
            );
        }
        "opencode" => {
            roots.push(
                non_empty_env_path("OPENCODE_HOME")
                    .or_else(|| non_empty_env_path("OPENCODE_CONFIG_DIR"))
                    .or_else(|| {
                        non_empty_env_path("XDG_CONFIG_HOME").map(|path| path.join("opencode"))
                    })
                    .unwrap_or_else(|| home.join(".config").join("opencode")),
            );
        }
        "qwen-code" | "qwen" => {
            roots.push(
                non_empty_env_path("QWEN_CODE_HOME")
                    .or_else(|| non_empty_env_path("QWEN_HOME"))
                    .unwrap_or_else(|| home.join(".qwen")),
            );
        }
        "goose" => {
            roots.push(
                non_empty_env_path("GOOSE_HOME")
                    .or_else(|| {
                        non_empty_env_path("XDG_CONFIG_HOME").map(|path| path.join("goose"))
                    })
                    .unwrap_or_else(|| home.join(".config").join("goose")),
            );
            roots.push(home.join(".goose"));
        }
        "copilot" => {
            roots.push(
                non_empty_env_path("COPILOT_HOME")
                    .or_else(|| {
                        non_empty_env_path("XDG_CONFIG_HOME")
                            .map(|path| path.join("github-copilot"))
                    })
                    .unwrap_or_else(|| home.join(".config").join("github-copilot")),
            );
            roots.push(home.join(".copilot"));
        }
        "hermes" => {
            roots.push(non_empty_env_path("HERMES_HOME").unwrap_or_else(|| home.join(".hermes")));
        }
        "kimi-code" | "kimi" => {
            roots.push(
                non_empty_env_path("KIMI_CODE_HOME")
                    .or_else(|| non_empty_env_path("KIMI_HOME"))
                    .unwrap_or_else(|| home.join(".kimi-code")),
            );
        }
        "cline" => {
            roots.push(
                non_empty_env_path("CLINE_HOME")
                    .or_else(|| {
                        non_empty_env_path("XDG_CONFIG_HOME").map(|path| path.join("cline"))
                    })
                    .unwrap_or_else(|| home.join(".cline")),
            );
        }
        "codebuddy" => {
            roots.push(
                non_empty_env_path("CODEBUDDY_HOME").unwrap_or_else(|| home.join(".codebuddy")),
            );
        }
        "openclaw" => {
            roots.push(
                non_empty_env_path("OPENCLAW_HOME")
                    .or_else(|| {
                        non_empty_env_path("XDG_CONFIG_HOME").map(|path| path.join("openclaw"))
                    })
                    .unwrap_or_else(|| home.join(".config").join("openclaw")),
            );
        }
        _ => {
            roots.push(
                non_empty_env_path(&format!("{}_HOME", agent_env_prefix(agent_id.as_str())))
                    .unwrap_or_else(|| home.join(format!(".{}", agent_id.as_str()))),
            );
            roots.push(
                non_empty_env_path("XDG_CONFIG_HOME")
                    .map(|path| path.join(agent_id.as_str()))
                    .unwrap_or_else(|| home.join(".config").join(agent_id.as_str())),
            );
        }
    }
    roots
}

fn agent_env_prefix(agent_id: &str) -> String {
    agent_id
        .chars()
        .map(|ch| if ch == '-' { '_' } else { ch })
        .collect::<String>()
        .to_ascii_uppercase()
}

fn push_mcp_config_candidates_for_root(
    paths: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    agent_id: &AgentId,
    root: &Path,
) {
    let file_names: &[&str] = match agent_id.as_str() {
        "codex" => &["config.toml", "mcp.json"],
        "claude" => &["settings.json", "claude.json", "mcp.json"],
        "gemini" => &["settings.json", "mcp.json", "config.json"],
        "opencode" => &["opencode.json", "config.json", "mcp.json", "settings.json"],
        "qwen-code" | "qwen" => &["settings.json", "config.json", "mcp.json"],
        "goose" => &[
            "config.yaml",
            "config.yml",
            "settings.json",
            "config.json",
            "mcp.json",
        ],
        "copilot" => &["settings.json", "config.json", "mcp.json"],
        "hermes" => &["config.yaml", "config.yml", "settings.json", "config.json"],
        "kimi-code" | "kimi" => &["mcp.json", "settings.json", "config.json"],
        "cline" => &[
            "cline_mcp_settings.json",
            "settings.json",
            "config.json",
            "mcp.json",
        ],
        "codebuddy" => &["settings.json", "config.json", "mcp.json"],
        "openclaw" => &["openclaw.json", "config.json", "settings.json", "mcp.json"],
        _ => &[
            "config.toml",
            "settings.toml",
            "mcp.json",
            "settings.json",
            "config.json",
            "config.yaml",
            "config.yml",
        ],
    };
    for file_name in file_names {
        push_path_unique_raw(paths, seen, root.join(file_name));
    }
}

fn parse_mcp_candidates_for_path(
    agent_id: &AgentId,
    source_path: &str,
    path: &Path,
    content: &str,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> Vec<McpServerCreateRequest> {
    match path.extension().and_then(|value| value.to_str()) {
        Some("toml") => parse_toml_mcp_candidates(agent_id, source_path, content, diagnostics),
        Some("yaml") | Some("yml") => {
            parse_yaml_mcp_candidates(agent_id, source_path, content, diagnostics)
        }
        _ => parse_json_mcp_candidates(agent_id, source_path, content, diagnostics),
    }
}

fn parse_json_mcp_candidates(
    agent_id: &AgentId,
    source_path: &str,
    content: &str,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> Vec<McpServerCreateRequest> {
    let value = match serde_json::from_str::<serde_json::Value>(content) {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(diagnostic(
                "mcpJsonParseFailed",
                format!("{source_path}: {error}"),
            ));
            return Vec::new();
        }
    };
    let servers = mcp_servers_json_objects(&value);
    if servers.is_empty() {
        diagnostics.push(diagnostic("mcpJsonServersMissing", source_path.to_string()));
        return Vec::new();
    }
    servers
        .iter()
        .flat_map(|servers| servers.iter())
        .filter_map(|(name, config)| mcp_candidate_from_value(agent_id, name, config))
        .collect()
}

fn mcp_servers_json_objects(
    value: &serde_json::Value,
) -> Vec<&serde_json::Map<String, serde_json::Value>> {
    let mut objects = Vec::new();
    let mut saw_container = false;
    for key in ["mcpServers", "mcp_servers", "servers"] {
        if let Some(container) = value.get(key) {
            saw_container = true;
            if let Some(object) = container.as_object() {
                objects.push(object);
            }
        }
    }
    if let Some(container) = value.get("mcp") {
        saw_container = true;
        if let Some(object) = container.as_object() {
            if let Some(servers) = object.get("servers").and_then(serde_json::Value::as_object) {
                objects.push(servers);
            } else {
                objects.push(object);
            }
        }
    }
    if objects.is_empty()
        && !saw_container
        && let Some(object) = value.as_object()
    {
        objects.push(object);
    }
    objects
}

fn parse_toml_mcp_candidates(
    agent_id: &AgentId,
    source_path: &str,
    content: &str,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> Vec<McpServerCreateRequest> {
    let value = match content.parse::<toml::Value>() {
        Ok(value) => value,
        Err(error) => {
            diagnostics.push(diagnostic(
                "mcpTomlParseFailed",
                format!("{source_path}: {error}"),
            ));
            return Vec::new();
        }
    };
    let tables = mcp_servers_toml_tables(&value);
    if tables.is_empty() {
        diagnostics.push(diagnostic("mcpTomlServersMissing", source_path.to_string()));
        return Vec::new();
    }
    tables
        .iter()
        .flat_map(|table| table.iter())
        .filter_map(|(name, config)| {
            let value = toml_value_to_json(config);
            mcp_candidate_from_value(agent_id, name, &value)
        })
        .collect()
}

fn toml_value_to_json(value: &toml::Value) -> serde_json::Value {
    match value {
        toml::Value::String(value) => serde_json::Value::String(value.clone()),
        toml::Value::Integer(value) => serde_json::Value::Number((*value).into()),
        toml::Value::Float(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        toml::Value::Boolean(value) => serde_json::Value::Bool(*value),
        toml::Value::Datetime(value) => serde_json::Value::String(value.to_string()),
        toml::Value::Array(values) => {
            serde_json::Value::Array(values.iter().map(toml_value_to_json).collect())
        }
        toml::Value::Table(table) => serde_json::Value::Object(
            table
                .iter()
                .map(|(key, value)| (key.clone(), toml_value_to_json(value)))
                .collect(),
        ),
    }
}

fn mcp_servers_toml_tables(value: &toml::Value) -> Vec<&toml::value::Table> {
    let mut tables = Vec::new();
    if let Some(table) = value.get("mcp_servers").and_then(toml::Value::as_table) {
        tables.push(table);
    }
    if let Some(table) = value.get("mcpServers").and_then(toml::Value::as_table) {
        tables.push(table);
    }
    if let Some(mcp_table) = value.get("mcp").and_then(toml::Value::as_table) {
        if let Some(servers_table) = mcp_table.get("servers").and_then(toml::Value::as_table) {
            tables.push(servers_table);
        } else {
            tables.push(mcp_table);
        }
    }
    tables
}

fn parse_yaml_mcp_candidates(
    agent_id: &AgentId,
    source_path: &str,
    content: &str,
    diagnostics: &mut Vec<ProviderBindingMetadata>,
) -> Vec<McpServerCreateRequest> {
    let value = match serde_yaml::from_str::<serde_yaml::Value>(content) {
        Ok(value) => match serde_json::to_value(value) {
            Ok(value) => value,
            Err(error) => {
                diagnostics.push(diagnostic(
                    "mcpYamlConvertFailed",
                    format!("{source_path}: {error}"),
                ));
                return Vec::new();
            }
        },
        Err(error) => {
            diagnostics.push(diagnostic(
                "mcpYamlParseFailed",
                format!("{source_path}: {error}"),
            ));
            return Vec::new();
        }
    };
    let servers = mcp_servers_json_objects(&value);
    if servers.is_empty() {
        diagnostics.push(diagnostic("mcpYamlServersMissing", source_path.to_string()));
        return Vec::new();
    }
    servers
        .iter()
        .flat_map(|servers| servers.iter())
        .filter_map(|(name, config)| mcp_candidate_from_value(agent_id, name, config))
        .collect()
}

fn mcp_candidate_from_value(
    agent_id: &AgentId,
    name: &str,
    value: &serde_json::Value,
) -> Option<McpServerCreateRequest> {
    let object = value.as_object()?;
    let (command, args) = command_and_args_fields(object);
    let url = string_field(object, &["url", "endpoint", "httpUrl", "http_url"]);
    let transport_kind = mcp_transport_kind(object, command.is_some(), url.is_some())?;
    Some(McpServerCreateRequest {
        display_name: string_field(object, &["name", "displayName"])
            .unwrap_or_else(|| name.to_string()),
        transport_kind,
        status: vibex_core::McpServerStatus::Enabled,
        scope_kind: vibex_core::McpServerScopeKind::User,
        project_id: None,
        workspace_id: None,
        command,
        args,
        // Values that carry credentials stay out of the database: they are
        // imported as secret references instead, and resolved at forwarding
        // time from the configured backend.
        env: non_secret_entries(object, &["env", "environment"])
            .into_iter()
            .map(|(name, value)| McpServerEnvEntry { name, value })
            .collect(),
        url,
        headers: non_secret_entries(object, HEADER_CONFIG_KEYS)
            .into_iter()
            .map(|(name, value)| McpServerHeaderEntry { name, value })
            .collect(),
        description: Some(format!(
            "Imported from existing {} MCP config",
            agent_id.as_str()
        )),
        tags: vec!["imported".to_string(), agent_id.as_str().to_string()],
        secret_references: header_secret_references(object),
        provider_matrix: Vec::new(),
    })
}

fn mcp_transport_kind(
    object: &serde_json::Map<String, serde_json::Value>,
    has_command: bool,
    has_url: bool,
) -> Option<McpServerTransportKind> {
    match string_field(object, &["type", "transport", "transportKind"])
        .as_deref()
        .map(str::to_ascii_lowercase)
        .as_deref()
    {
        Some("stdio") | Some("local") if has_command => Some(McpServerTransportKind::Stdio),
        Some("http") | Some("streamable_http") | Some("remote") if has_url => {
            Some(McpServerTransportKind::Http)
        }
        Some("sse") if has_url => Some(McpServerTransportKind::Sse),
        _ if has_command => Some(McpServerTransportKind::Stdio),
        _ if has_url => Some(McpServerTransportKind::Http),
        _ => None,
    }
}

fn command_and_args_fields(
    object: &serde_json::Map<String, serde_json::Value>,
) -> (Option<String>, Vec<String>) {
    let explicit_args = string_array_field(object, &["args", "arguments"]);
    let Some(command_value) = object.get("command").or_else(|| object.get("cmd")) else {
        return (None, explicit_args);
    };
    if let Some(command) = command_value
        .as_str()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
    {
        return (Some(command), explicit_args);
    }
    let Some(command_array) = command_value.as_array() else {
        return (None, explicit_args);
    };
    let mut parts = command_array
        .iter()
        .filter_map(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string);
    let command = parts.next();
    let args = if explicit_args.is_empty() {
        parts.collect()
    } else {
        explicit_args
    };
    (command, args)
}

/// Header keys used by the native configs Vibex imports from.
const HEADER_CONFIG_KEYS: &[&str] = &["headers", "http_headers", "httpHeaders", "requestHeaders"];

/// Entries from a native config that are safe to store verbatim.
///
/// Anything that looks like a credential is skipped: those are imported as
/// secret references and resolved from the configured backend at forwarding
/// time, so the value never reaches the database.
fn non_secret_entries(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Vec<(String, String)> {
    let mut entries: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for key in keys {
        let Some(map) = object.get(*key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for (name, value) in map {
            let name = name.trim();
            let Some(value) = value.as_str().map(str::trim) else {
                continue;
            };
            if name.is_empty()
                || name.contains('\0')
                || value.is_empty()
                || looks_like_secret_name(name)
            {
                continue;
            }
            if seen.insert(name.to_ascii_lowercase()) {
                entries.push((name.to_string(), value.to_string()));
            }
        }
    }
    entries.sort_by(|left, right| {
        left.0
            .to_ascii_lowercase()
            .cmp(&right.0.to_ascii_lowercase())
    });
    entries
}

fn looks_like_secret_name(name: &str) -> bool {
    let normalized = name.to_ascii_lowercase();
    [
        "authorization",
        "token",
        "secret",
        "password",
        "passwd",
        "api_key",
        "apikey",
        "api-key",
        "credential",
        "cookie",
        "session",
        "private",
        "signature",
    ]
    .iter()
    .any(|marker| normalized.contains(marker))
}

fn header_secret_references(
    object: &serde_json::Map<String, serde_json::Value>,
) -> Vec<McpServerSecretReferenceCreateRequest> {
    let mut headers = Vec::new();
    let mut seen = HashSet::new();
    for key in HEADER_CONFIG_KEYS {
        let Some(header_object) = object.get(*key).and_then(serde_json::Value::as_object) else {
            continue;
        };
        for header_name in header_object.keys() {
            let header_name = header_name.trim();
            if header_name.is_empty() || header_name.contains('\0') {
                continue;
            }
            if seen.insert(header_name.to_ascii_lowercase()) {
                headers.push(header_name.to_string());
            }
        }
    }
    headers.sort_by_key(|header| header.to_ascii_lowercase());
    headers
        .into_iter()
        .map(|header| McpServerSecretReferenceCreateRequest {
            secret_kind: ProviderSecretKind::Header,
            backend: ProviderSecretBackend::Placeholder,
            setup_state: ProviderSecretSetupState::Missing,
            lookup_key: header.clone(),
            display_label: format!("MCP header {header}"),
            redacted_hint: "present in native config; configure in Vibex".to_string(),
            target: McpSecretTarget::Header,
        })
        .collect()
}

fn string_field(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Option<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(ToString::to_string)
}

fn string_array_field(
    object: &serde_json::Map<String, serde_json::Value>,
    keys: &[&str],
) -> Vec<String> {
    keys.iter()
        .find_map(|key| object.get(*key))
        .and_then(serde_json::Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect()
        })
        .unwrap_or_default()
}

fn find_existing_mcp_server(
    conn: &vibex_db::DbConnection,
    candidate: &McpServerCreateRequest,
) -> VibexResult<Option<McpServer>> {
    Ok(McpServerRepository::list(conn)?
        .into_iter()
        .find(|server| mcp_candidate_matches_existing(server, candidate)))
}

fn mcp_candidate_matches_existing(server: &McpServer, candidate: &McpServerCreateRequest) -> bool {
    server.display_name == candidate.display_name
        && server.transport_kind == candidate.transport_kind
        && server.command == candidate.command
        && server.url == candidate.url
}

fn merge_mcp_secret_references(
    mut server: McpServer,
    candidate: &McpServerCreateRequest,
) -> McpServer {
    let now = unix_timestamp_ms();
    for secret in &candidate.secret_references {
        let already_present = server.secret_references.iter().any(|existing| {
            existing.target == secret.target
                && existing.lookup_key == secret.lookup_key
                && existing.secret_kind == secret.secret_kind
        });
        if already_present {
            continue;
        }
        server
            .secret_references
            .push(vibex_core::McpServerSecretReference {
                id: vibex_core::RequestId::new(),
                mcp_server_id: server.id.clone(),
                secret_kind: secret.secret_kind,
                backend: secret.backend,
                setup_state: secret.setup_state,
                lookup_key: secret.lookup_key.clone(),
                display_label: secret.display_label.clone(),
                redacted_hint: secret.redacted_hint.clone(),
                target: secret.target,
                created_at_ms: now,
                updated_at_ms: now,
            });
    }
    server
}

fn find_existing_skill_by_source_uri(
    conn: &vibex_db::DbConnection,
    source_uri: &str,
) -> VibexResult<Option<Skill>> {
    Ok(SkillRepository::list(conn)?
        .into_iter()
        .find(|skill| skill.source_uri.as_deref() == Some(source_uri)))
}

fn build_agent_snapshots(
    definitions: Vec<AgentDefinition>,
    configs: Vec<AgentConfig>,
    discoveries: HashMap<AgentId, AgentDiscoveryRecord>,
    managed_installations: &HashMap<AgentId, vibex_core::AgentManagedInstallState>,
) -> Vec<AgentSnapshotEntry> {
    let configs_by_id = configs
        .iter()
        .map(|config| (config.agent_id.clone(), config))
        .collect::<HashMap<_, _>>();
    let mut snapshots = definitions
        .iter()
        .map(|definition| {
            let mut snapshot = AgentSnapshotEntry::from_definition(
                definition,
                configs_by_id.get(&definition.id).copied(),
                discoveries.get(&definition.id),
            );
            if let Some(state) = managed_installations.get(&definition.id) {
                snapshot.apply_managed_install_state(state.clone());
            }
            snapshot
        })
        .collect::<Vec<_>>();
    snapshots.sort_by(|left, right| {
        left.order_index
            .cmp(&right.order_index)
            .then_with(|| left.label.cmp(&right.label))
    });
    snapshots
}

fn refresh_changed_agent_discoveries(
    conn: &vibex_db::DbConnection,
    snapshots: &[AgentSnapshotEntry],
    discoveries: &mut HashMap<AgentId, AgentDiscoveryRecord>,
) -> VibexResult<()> {
    for snapshot in snapshots {
        let existing = discoveries.get(&snapshot.id);
        let mut discovery = low_cost_agent_discovery(snapshot, DEFAULT_AGENT_CWD_SCOPE);
        preserve_cached_agent_version(existing, &mut discovery);
        if agent_discovery_changed(existing, &discovery) {
            AgentDiscoveryRepository::insert(conn, &discovery)?;
            discoveries.insert(discovery.agent_id.clone(), discovery);
        }
    }
    Ok(())
}

fn preserve_cached_agent_version(
    existing: Option<&AgentDiscoveryRecord>,
    candidate: &mut AgentDiscoveryRecord,
) {
    let Some(existing) = existing else {
        return;
    };
    if candidate.version.is_none()
        && candidate.install_status == AgentInstallStatus::Installed
        && candidate.binary_path == existing.binary_path
    {
        candidate.version.clone_from(&existing.version);
    }
}

fn agent_discovery_changed(
    existing: Option<&AgentDiscoveryRecord>,
    candidate: &AgentDiscoveryRecord,
) -> bool {
    let Some(existing) = existing else {
        return true;
    };
    existing.install_status == AgentInstallStatus::Unknown
        || existing.config_status == AgentConfigStatus::Unknown
        || existing.runtime_status == AgentRuntimeStatus::Unknown
        || existing.install_status != candidate.install_status
        || existing.config_status != candidate.config_status
        || existing.runtime_status != candidate.runtime_status
        || existing.binary_path != candidate.binary_path
        || existing.version != candidate.version
        || existing.native_config_paths != candidate.native_config_paths
        || existing.models != candidate.models
        || existing.modes != candidate.modes
}

fn low_cost_agent_discovery(
    snapshot: &AgentSnapshotEntry,
    cwd_scope: &str,
) -> AgentDiscoveryRecord {
    let now = unix_timestamp_ms();
    if !snapshot.added {
        return AgentDiscoveryRecord {
            discovery_record_id: format!("agent_discovery_{}_{}", snapshot.id, now),
            agent_id: snapshot.id.clone(),
            cwd_scope: cwd_scope.to_string(),
            install_status: AgentInstallStatus::Disabled,
            config_status: AgentConfigStatus::Disabled,
            runtime_status: AgentRuntimeStatus::Disabled,
            binary_path: None,
            version: None,
            native_config_paths: Vec::new(),
            models: Vec::new(),
            modes: snapshot.modes.clone(),
            diagnostics: vec![ProviderBindingMetadata {
                key: "probe".to_string(),
                value: "removed agents are not probed".to_string(),
            }],
            discovered_at_ms: now,
        };
    }

    let runtime_binary = snapshot
        .command
        .as_ref()
        .and_then(|command| resolve_binary_path(&command.command));
    let agent_binary = resolve_agent_cli_binary(snapshot, runtime_binary.as_deref());
    let install_status = if agent_binary.is_some() {
        AgentInstallStatus::Installed
    } else {
        AgentInstallStatus::Missing
    };
    let native_config_paths = native_config_paths_for_agent(&snapshot.id);
    let config_status =
        if !native_config_paths.is_empty() || install_status == AgentInstallStatus::Installed {
            AgentConfigStatus::Configured
        } else {
            AgentConfigStatus::NeedsConfiguration
        };
    let runtime_status = if !snapshot.enabled {
        AgentRuntimeStatus::Disabled
    } else if runtime_binary.is_some() {
        AgentRuntimeStatus::Ready
    } else {
        AgentRuntimeStatus::Unavailable
    };
    let mut diagnostics = Vec::new();
    diagnostics.push(ProviderBindingMetadata {
        key: "probe".to_string(),
        value: "low_cost_no_runtime_spawn".to_string(),
    });
    if let Some(path) = agent_binary.as_deref() {
        diagnostics.push(ProviderBindingMetadata {
            key: "binaryPath".to_string(),
            value: path.to_string(),
        });
    }
    if let Some(path) = runtime_binary.as_deref()
        && agent_binary.as_deref() != Some(path)
    {
        diagnostics.push(ProviderBindingMetadata {
            key: "runtimeBinaryPath".to_string(),
            value: path.to_string(),
        });
    } else if runtime_binary.is_none() && agent_binary.is_some() {
        diagnostics.push(ProviderBindingMetadata {
            key: "runtime".to_string(),
            value: "acp_runtime_command_missing".to_string(),
        });
    }

    AgentDiscoveryRecord {
        discovery_record_id: format!("agent_discovery_{}_{}", snapshot.id, now),
        agent_id: snapshot.id.clone(),
        cwd_scope: cwd_scope.to_string(),
        install_status,
        config_status,
        runtime_status,
        binary_path: agent_binary,
        version: None,
        native_config_paths,
        models: Vec::new(),
        modes: snapshot.modes.clone(),
        diagnostics,
        discovered_at_ms: now,
    }
}

/// Explicit Config Center refreshes may identify a trusted PATH-launched Agent
/// CLI without starting its ACP runtime or creating a session. Ordinary list
/// and cached-discovery paths never call this helper.
fn probe_explicit_agent_version(
    snapshot: &AgentSnapshotEntry,
    discovery: &mut AgentDiscoveryRecord,
) {
    if discovery.install_status != AgentInstallStatus::Installed {
        return;
    }
    let Some(trusted_binary_names) = trusted_version_probe_binary_names(snapshot.id.as_str())
    else {
        return;
    };
    let Some(binary_path) = discovery.binary_path.as_deref() else {
        return;
    };
    let trusted = Path::new(binary_path)
        .file_stem()
        .and_then(|name| name.to_str())
        .is_some_and(|name| {
            trusted_binary_names
                .iter()
                .any(|trusted| name.eq_ignore_ascii_case(trusted))
        });
    if !trusted {
        discovery.diagnostics.push(ProviderBindingMetadata {
            key: "versionProbe".to_string(),
            value: "agent_version_probe_command_untrusted".to_string(),
        });
        return;
    }
    match probe_cli_version(Path::new(binary_path)) {
        Ok(version) => {
            discovery.version = Some(version.clone());
            discovery.diagnostics.push(ProviderBindingMetadata {
                key: "version".to_string(),
                value: version,
            });
        }
        Err(code) => discovery.diagnostics.push(ProviderBindingMetadata {
            key: "versionProbe".to_string(),
            value: code.to_string(),
        }),
    }
}

fn trusted_version_probe_binary_names(agent_id: &str) -> Option<&'static [&'static str]> {
    match agent_id {
        "opencode" => Some(&["opencode"]),
        "copilot" => Some(&["copilot"]),
        "codewhale" => Some(&["codewhale"]),
        "crow-cli" => Some(&["crow-cli"]),
        "goose" => Some(&["goose"]),
        "grok" => Some(&["grok"]),
        "hermes" => Some(&["hermes"]),
        "kilo" => Some(&["kilo"]),
        "kimi" => Some(&["kimi"]),
        "mistral-vibe" => Some(&["vibe-acp"]),
        "poolside" => Some(&["pool"]),
        "stakpak" => Some(&["stakpak"]),
        "vtcode" => Some(&["vtcode"]),
        _ => None,
    }
}

const CLI_VERSION_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

fn probe_cli_version(binary_path: &Path) -> Result<String, &'static str> {
    let mut command = Command::new(binary_path);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        command.creation_flags(0x0800_0000);
    }
    let mut child = command
        .arg("--version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|_| "agent_version_probe_spawn_failed")?;
    let deadline = Instant::now() + CLI_VERSION_PROBE_TIMEOUT;
    loop {
        match child.try_wait() {
            Ok(Some(_)) => {
                let output = child
                    .wait_with_output()
                    .map_err(|_| "agent_version_probe_read_failed")?;
                if !output.status.success() {
                    return Err("agent_version_probe_exit_failed");
                }
                return parse_cli_version(&output.stdout)
                    .or_else(|| parse_cli_version(&output.stderr))
                    .ok_or("agent_version_probe_output_invalid");
            }
            Ok(None) if Instant::now() >= deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("agent_version_probe_timeout");
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(10)),
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err("agent_version_probe_wait_failed");
            }
        }
    }
}

fn parse_cli_version(output: &[u8]) -> Option<String> {
    String::from_utf8_lossy(output)
        .split_whitespace()
        .map(|token| {
            token.trim_matches(|character: char| {
                !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
            })
        })
        .map(|token| token.strip_prefix('v').unwrap_or(token))
        .find(|token| Version::parse(token).is_ok())
        .map(ToString::to_string)
}

fn resolve_agent_cli_binary(
    snapshot: &AgentSnapshotEntry,
    runtime_binary: Option<&str>,
) -> Option<String> {
    let native_commands: &[&str] = match snapshot.id.as_str() {
        "claude" => &["claude"],
        "codex" => &["codex"],
        _ => &[],
    };
    native_commands
        .iter()
        .find_map(|command| resolve_binary_path(command))
        .or_else(|| runtime_binary.map(ToString::to_string))
}

/// Resolves an Agent command through the same PATH and runtime-manager search
/// used by Provider discovery.
pub fn resolve_binary_path(command: &str) -> Option<String> {
    let command = command.trim();
    if command.is_empty() {
        return None;
    }
    binary_path_candidates(command)
        .into_iter()
        .find(|candidate| executable_file_exists(candidate))
        .map(|candidate| candidate.to_string_lossy().to_string())
}

fn binary_path_candidates(command: &str) -> Vec<PathBuf> {
    let mut candidates = Vec::new();
    let mut seen = HashSet::new();
    if has_path_separator(command) {
        push_executable_candidate_variants(&mut candidates, &mut seen, PathBuf::from(command));
        return candidates;
    }
    push_path_binary_candidates(&mut candidates, &mut seen, command);
    push_env_manager_binary_candidates(&mut candidates, &mut seen, command);
    push_home_manager_binary_candidates(&mut candidates, &mut seen, command);
    push_fixed_binary_candidates(&mut candidates, &mut seen, command);
    push_windows_binary_candidates(&mut candidates, &mut seen, command);
    candidates
}

fn has_path_separator(value: &str) -> bool {
    value.contains('/') || value.contains('\\')
}

fn push_path_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
) {
    let Some(path_var) = env::var_os("PATH") else {
        return;
    };
    for dir in env::split_paths(&path_var).filter(|dir| !dir.as_os_str().is_empty()) {
        push_executable_candidate_variants(candidates, seen, dir.join(command));
    }
}

fn push_env_manager_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
) {
    for (env_key, suffix) in [
        ("NPM_CONFIG_PREFIX", &["bin"][..]),
        ("VOLTA_HOME", &["bin"][..]),
        ("ASDF_DATA_DIR", &["shims"][..]),
        ("MISE_DATA_DIR", &["shims"][..]),
        ("PNPM_HOME", &[][..]),
    ] {
        let Some(mut candidate) = non_empty_env_path(env_key) else {
            continue;
        };
        for component in suffix {
            candidate.push(component);
        }
        candidate.push(command);
        push_executable_candidate_variants(candidates, seen, candidate);
    }

    if let Some(nvm_dir) = non_empty_env_path("NVM_DIR") {
        push_versioned_binary_candidates(
            candidates,
            seen,
            nvm_dir.join("versions").join("node"),
            &["bin"],
            command,
        );
    }
    if let Some(fnm_dir) = non_empty_env_path("FNM_DIR") {
        push_versioned_binary_candidates(
            candidates,
            seen,
            fnm_dir.join("node-versions"),
            &["installation", "bin"],
            command,
        );
    }
}

fn push_home_manager_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
) {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    for prefix in [
        &[".nvm", "current", "bin"][..],
        &[".volta", "bin"][..],
        &[".asdf", "shims"][..],
        &[".local", "share", "mise", "shims"][..],
        &[".config", "mise", "shims"][..],
        &[".local", "bin"][..],
        &[".npm-global", "bin"][..],
        &[".npm-packages", "bin"][..],
        &[".local", "share", "pnpm"][..],
        &["Library", "pnpm"][..],
    ] {
        let mut candidate = home.clone();
        for component in prefix {
            candidate.push(component);
        }
        candidate.push(command);
        push_executable_candidate_variants(candidates, seen, candidate);
    }

    push_versioned_binary_candidates(
        candidates,
        seen,
        home.join(".nvm").join("versions").join("node"),
        &["bin"],
        command,
    );
    push_versioned_binary_candidates(
        candidates,
        seen,
        home.join(".local")
            .join("share")
            .join("fnm")
            .join("node-versions"),
        &["installation", "bin"],
        command,
    );
    push_versioned_binary_candidates(
        candidates,
        seen,
        home.join("Library")
            .join("Application Support")
            .join("fnm")
            .join("node-versions"),
        &["installation", "bin"],
        command,
    );
}

fn push_versioned_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    versions_dir: PathBuf,
    suffix: &[&str],
    command: &str,
) {
    let Ok(entries) = fs::read_dir(versions_dir) else {
        return;
    };
    let mut discovered = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| {
            let mut candidate = entry.path();
            for component in suffix {
                candidate.push(component);
            }
            candidate.push(command);
            candidate
        })
        .collect::<Vec<_>>();
    discovered.sort_by(|left, right| right.cmp(left));
    for candidate in discovered {
        push_executable_candidate_variants(candidates, seen, candidate);
    }
}

fn push_fixed_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
) {
    #[cfg(not(windows))]
    {
        for prefix in [
            "/opt/homebrew/bin",
            "/usr/local/bin",
            "/home/linuxbrew/.linuxbrew/bin",
            "/usr/bin",
            "/bin",
        ] {
            push_executable_candidate_variants(candidates, seen, Path::new(prefix).join(command));
        }
    }
    #[cfg(windows)]
    {
        let _ = (candidates, seen, command);
    }
}

fn push_windows_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
) {
    #[cfg(windows)]
    {
        if let Some(appdata) = non_empty_env_path("APPDATA") {
            push_executable_candidate_variants(candidates, seen, appdata.join("npm").join(command));
        }
        if let Some(local_appdata) = non_empty_env_path("LOCALAPPDATA") {
            push_winget_binary_candidates(candidates, seen, command, &local_appdata);
            if command.eq_ignore_ascii_case("codex") {
                push_codex_microsoft_store_candidates(candidates, seen, &local_appdata);
            }
        }
    }
    #[cfg(not(windows))]
    {
        let _ = (candidates, seen, command);
    }
}

#[cfg(windows)]
fn push_winget_binary_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    command: &str,
    local_appdata: &Path,
) {
    let package_root = local_appdata
        .join("Microsoft")
        .join("WinGet")
        .join("Packages");
    let Ok(entries) = fs::read_dir(package_root) else {
        return;
    };
    let mut package_dirs = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    package_dirs.sort();
    for package_dir in package_dirs {
        push_executable_candidate_variants(candidates, seen, package_dir.join(command));
    }
}

#[cfg(windows)]
fn push_codex_microsoft_store_candidates(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    local_appdata: &Path,
) {
    let package_root = local_appdata.join("Packages");
    let Ok(entries) = fs::read_dir(package_root) else {
        return;
    };
    let mut codex_packages = entries
        .filter_map(Result::ok)
        .filter(|entry| entry.file_type().map(|kind| kind.is_dir()).unwrap_or(false))
        .filter(|entry| {
            entry
                .file_name()
                .to_string_lossy()
                .starts_with("OpenAI.Codex_")
        })
        .map(|entry| entry.path())
        .collect::<Vec<_>>();
    codex_packages.sort();
    for package in codex_packages {
        push_executable_candidate_variants(
            candidates,
            seen,
            package
                .join("LocalCache")
                .join("Local")
                .join("OpenAI")
                .join("Codex")
                .join("bin")
                .join("codex.exe"),
        );
    }
}

fn push_executable_candidate_variants(
    candidates: &mut Vec<PathBuf>,
    seen: &mut HashSet<String>,
    candidate: PathBuf,
) {
    for variant in executable_candidate_variants(candidate) {
        let key = comparable_path_key(&variant);
        if seen.insert(key) {
            candidates.push(variant);
        }
    }
}

fn executable_candidate_variants(candidate: PathBuf) -> Vec<PathBuf> {
    #[cfg(windows)]
    {
        if candidate.extension().is_some() {
            return vec![candidate];
        }
        let mut variants = vec![candidate.clone()];
        let base = candidate.to_string_lossy();
        for extension in windows_executable_extensions() {
            variants.push(PathBuf::from(format!("{base}{extension}")));
        }
        variants
    }
    #[cfg(not(windows))]
    {
        vec![candidate]
    }
}

#[cfg(windows)]
fn windows_executable_extensions() -> Vec<String> {
    let mut extensions = Vec::new();
    if let Some(pathext) = env::var_os("PATHEXT") {
        for extension in pathext.to_string_lossy().split(';') {
            let extension = extension.trim();
            if extension.is_empty() {
                continue;
            }
            let extension = if extension.starts_with('.') {
                extension.to_string()
            } else {
                format!(".{extension}")
            };
            if !extensions
                .iter()
                .any(|existing: &String| existing.eq_ignore_ascii_case(&extension))
            {
                extensions.push(extension);
            }
        }
    }
    for extension in [".exe", ".cmd", ".bat", ".com"] {
        if !extensions
            .iter()
            .any(|existing| existing.eq_ignore_ascii_case(extension))
        {
            extensions.push(extension.to_string());
        }
    }
    extensions
}

fn comparable_path_key(path: &Path) -> String {
    #[cfg(windows)]
    {
        let mut key = path.to_string_lossy().to_string();
        key = key.replace('\\', "/");
        key.make_ascii_lowercase();
        key
    }
    #[cfg(not(windows))]
    {
        path.to_string_lossy().to_string()
    }
}

fn executable_file_exists(path: &Path) -> bool {
    let Ok(metadata) = fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

fn native_config_paths_for_agent(agent_id: &AgentId) -> Vec<String> {
    let mut paths = Vec::new();
    let mut seen = HashSet::new();
    let home = dirs::home_dir();
    match agent_id.as_str() {
        "codex" => {
            if let Some(codex_home) = non_empty_env_path("CODEX_HOME")
                .or_else(|| home.as_ref().map(|home| home.join(".codex")))
            {
                for file_name in ["auth.json", "config.toml", "models_cache.json", "mcp.json"] {
                    push_existing_path(&mut paths, &mut seen, codex_home.join(file_name));
                }
            }
        }
        "claude" => {
            if let Some(home) = home.as_ref() {
                let claude_home = home.join(".claude");
                for path in [
                    claude_home.join("settings.json"),
                    claude_home.join("claude.json"),
                    claude_home.join("mcp.json"),
                    home.join(".claude.json"),
                ] {
                    push_existing_path(&mut paths, &mut seen, path);
                }
            }
        }
        "opencode" => {
            if let Some(config_dir) = non_empty_env_path("XDG_CONFIG_HOME")
                .map(|path| path.join("opencode"))
                .or_else(|| {
                    home.as_ref()
                        .map(|home| home.join(".config").join("opencode"))
                })
            {
                for file_name in ["opencode.json", ".env"] {
                    push_existing_path(&mut paths, &mut seen, config_dir.join(file_name));
                }
            }
            let data_dir = non_empty_env_path("XDG_DATA_HOME")
                .map(|path| path.join("opencode"))
                .or_else(|| {
                    home.as_ref()
                        .map(|home| home.join(".local").join("share").join("opencode"))
                });
            if let Some(db_path) = non_empty_env_path("OPENCODE_DB") {
                let db_path = if db_path.is_absolute() {
                    db_path
                } else if let Some(data_dir) = data_dir.as_ref() {
                    data_dir.join(db_path)
                } else {
                    db_path
                };
                push_existing_path(&mut paths, &mut seen, db_path);
            }
            if let Some(data_dir) = data_dir {
                push_existing_path(&mut paths, &mut seen, data_dir.join("opencode.db"));
            }
        }
        _ => {}
    }
    paths
}

fn non_empty_env_path(key: &str) -> Option<PathBuf> {
    env::var_os(key).and_then(|value| {
        if value.is_empty() {
            None
        } else {
            Some(PathBuf::from(value))
        }
    })
}

fn push_existing_path(paths: &mut Vec<String>, seen: &mut HashSet<String>, path: PathBuf) {
    if fs::metadata(&path).is_ok() {
        let key = comparable_path_key(&path);
        if seen.insert(key) {
            paths.push(path.to_string_lossy().to_string());
        }
    }
}

fn bundled_acp_catalog_presets() -> Vec<AcpProviderCatalogPreset> {
    let mut presets = vec![
        opencode_acp_preset(),
        claude_acp_preset(),
        codex_acp_preset(),
    ];
    presets.extend(
        acp_agent_catalog_entries()
            .iter()
            .map(generic_acp_catalog_preset),
    );
    presets
}

/// Feature tokens granted to bundled ACP presets. They project onto
/// `ProviderCapabilities` via `acp_capabilities_from_config`; the generic ACP
/// runtime supports all of them for any conforming ACP CLI.
fn default_acp_preset_features() -> Vec<String> {
    [
        "agent_messages",
        "streaming",
        "tool_calls",
        "permission_requests",
        "reasoning",
        "plan",
        "interrupt",
        "slash_commands",
        "skills",
        "image_input",
        "file_attachments",
        "session_persistence",
        // Stdio MCP forwarding is mandatory for every conforming ACP agent, so
        // the preset grants it by default. Agents that reject forwarded
        // servers opt out through `without_mcp_server_support()` in the
        // catalog, and HTTP/SSE entries stay gated on the agent's advertised
        // `mcpCapabilities` at session open.
        "mcp_servers",
    ]
    .into_iter()
    .map(ToString::to_string)
    .collect()
}

fn acp_catalog_preset(
    preset_id: &str,
    display_name: &str,
    description: &str,
    command: &str,
    args: &[&str],
    tags: &[&str],
) -> AcpProviderCatalogPreset {
    AcpProviderCatalogPreset {
        preset_id: preset_id.to_string(),
        display_name: display_name.to_string(),
        description: description.to_string(),
        default_config: AcpProviderConfig {
            command: command.to_string(),
            args: args.iter().map(ToString::to_string).collect(),
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: Vec::new(),
            modes: Vec::new(),
            features: default_acp_preset_features(),
            disabled_tools: Vec::new(),
        },
        tags: tags.iter().map(ToString::to_string).collect(),
        editable: true,
    }
}

fn generic_acp_catalog_preset(entry: &AcpAgentCatalogEntry) -> AcpProviderCatalogPreset {
    let (command, args) = entry
        .command
        .split_first()
        .expect("bundled ACP Agent commands are non-empty");
    let mut preset = acp_catalog_preset(
        entry.preset_id,
        entry.label,
        entry.description,
        command,
        args,
        &["local", "acp", entry.id],
    );
    preset.default_config.env = entry
        .env
        .iter()
        .map(|(key, value)| AcpProviderEnvReference {
            key: (*key).to_string(),
            source: AcpProviderEnvSource::Literal,
            value: Some((*value).to_string()),
            secret_lookup_key: None,
            redacted_hint: "bundled catalog value".to_string(),
        })
        .collect();
    if entry.supports_mcp_servers == Some(false) {
        preset
            .default_config
            .features
            .retain(|feature| !matches!(feature.as_str(), "mcp" | "mcp_servers"));
    }
    preset
}

fn opencode_acp_preset() -> AcpProviderCatalogPreset {
    AcpProviderCatalogPreset {
        preset_id: OPENCODE_PRESET_ID.to_string(),
        display_name: "OpenCode ACP".to_string(),
        description:
            "Command-based OpenCode ACP provider profile. Validation does not start opencode."
                .to_string(),
        default_config: AcpProviderConfig {
            command: "/usr/bin/opencode".to_string(),
            args: vec!["acp".to_string()],
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: Vec::new(),
            modes: vec!["default".to_string()],
            features: default_acp_preset_features(),
            disabled_tools: Vec::new(),
        },
        tags: vec![
            "local".to_string(),
            "acp".to_string(),
            "opencode".to_string(),
        ],
        editable: true,
    }
}

fn claude_acp_preset() -> AcpProviderCatalogPreset {
    let mut preset = acp_catalog_preset(
        "claude-agent-acp",
        "Claude Code (ACP)",
        "Claude Code through the fixed @agentclientprotocol/claude-agent-acp@0.64.2 Adapter managed by the Compatibility Registry.",
        "claude-agent-acp",
        &[],
        &["local", "acp", "claude"],
    );
    // Session modes the pinned adapter advertises unconditionally; gated
    // modes (auto, bypassPermissions) are discovered live by the runtime
    // option catalog probe instead of being promised here.
    preset.default_config.modes = ["default", "acceptEdits", "plan", "dontAsk"]
        .into_iter()
        .map(ToString::to_string)
        .collect();
    preset
}

fn codex_acp_preset() -> AcpProviderCatalogPreset {
    let mut preset = acp_catalog_preset(
        "codex-acp",
        "Codex (ACP)",
        "Codex through the fixed @agentclientprotocol/codex-acp@1.1.9 Adapter managed by the Compatibility Registry.",
        "codex-acp",
        &[],
        &["local", "acp", "codex"],
    );
    preset.default_config.modes = ["read-only", "agent", "agent-full-access"]
        .into_iter()
        .map(ToString::to_string)
        .collect();
    preset
}

fn resolve_acp_create_config(
    preset_id: Option<&str>,
    config: Option<AcpProviderConfig>,
) -> VibexResult<AcpProviderConfig> {
    if let Some(preset_id) = preset_id {
        let preset = bundled_acp_catalog_presets()
            .into_iter()
            .find(|preset| preset.preset_id == preset_id)
            .ok_or_else(|| {
                VibexError::validation("acp_preset_not_found", "ACP catalog preset was not found")
                    .with_diagnostic("presetId", preset_id)
            })?;
        return Ok(config.unwrap_or(preset.default_config));
    }
    config.ok_or_else(|| {
        VibexError::validation(
            "acp_config_missing",
            "custom ACP profile creation requires typed ACP configuration",
        )
    })
}

fn apply_agent_auth_secret_writes(
    writes: &[(String, String)],
) -> VibexResult<Vec<(String, Option<String>)>> {
    let mut applied = Vec::with_capacity(writes.len());
    for (lookup_key, value) in writes {
        let previous = match secrets::resolve_provider_secret_reference(
            ProviderSecretBackend::OsKeychain,
            ProviderSecretSetupState::Available,
            lookup_key,
        ) {
            Ok(previous) => previous,
            Err(error) => {
                return Err(agent_auth_secret_rollback_error(error, &applied));
            }
        };
        if let Err(error) = secrets::store_provider_secret(lookup_key, value) {
            return Err(agent_auth_secret_rollback_error(error, &applied));
        }
        applied.push((lookup_key.clone(), previous));
    }
    Ok(applied)
}

fn agent_auth_secret_rollback_error(
    error: VibexError,
    applied: &[(String, Option<String>)],
) -> VibexError {
    let failures = rollback_agent_auth_secret_writes(applied);
    if failures == 0 {
        error
    } else {
        error.with_diagnostic("keychainRollbackFailures", failures.to_string())
    }
}

fn rollback_agent_auth_secret_writes(applied: &[(String, Option<String>)]) -> usize {
    applied
        .iter()
        .rev()
        .filter(|(lookup_key, previous)| {
            let result = match previous {
                Some(previous) => secrets::store_provider_secret(lookup_key, previous),
                None => secrets::delete_provider_secret(lookup_key),
            };
            result.is_err()
        })
        .count()
}

fn inherit_agent_acp_runtime_options(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
    options: Option<ProviderOptions>,
) -> VibexResult<ProviderOptions> {
    let options = options.unwrap_or_else(ProviderOptions::empty);
    if acp_config_from_options(&options)?.is_some() {
        return Ok(options);
    }

    let mut runtime_config = None;
    for profile in ProviderProfileRepository::list_by_agent(conn, agent_id, true)? {
        if profile.kind != ProviderKind::Acp {
            continue;
        }
        if let Some(config) = acp_config_from_options(&profile.provider_options)? {
            runtime_config = Some(config);
            break;
        }
    }
    let runtime_config = match runtime_config {
        Some(config) => config,
        None => default_acp_runtime_config_for_agent(conn, agent_id)?,
    };
    merge_acp_runtime_options(options, runtime_config)
}

fn without_internal_profile_role(mut options: ProviderOptions) -> ProviderOptions {
    options
        .entries
        .retain(|entry| entry.key.trim() != INTERNAL_PROFILE_ROLE_OPTION_KEY);
    options
}

fn default_acp_runtime_config_for_agent(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
) -> VibexResult<AcpProviderConfig> {
    let definition = require_agent_definition(agent_id)?;
    let agent_config = AgentConfigRepository::get(conn, agent_id)?;
    let preset_id = agent_config
        .as_ref()
        .map(|config| &config.params)
        .unwrap_or(&definition.params)
        .get("preset")
        .and_then(serde_json::Value::as_str);
    let mut config = preset_id
        .and_then(|preset_id| {
            bundled_acp_catalog_presets()
                .into_iter()
                .find(|preset| preset.preset_id == preset_id)
        })
        .map(|preset| preset.default_config)
        .unwrap_or_else(|| AcpProviderConfig {
            command: String::new(),
            args: Vec::new(),
            env: Vec::new(),
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: Vec::new(),
            modes: Vec::new(),
            features: default_acp_preset_features(),
            disabled_tools: Vec::new(),
        });
    if let Some(command) = agent_config
        .as_ref()
        .and_then(|agent| agent.command.as_ref())
        .or(definition.command.as_ref())
    {
        config.command = command.command.clone();
        config.args = command.args.clone();
    }
    let agent_env = agent_config
        .as_ref()
        .map(|agent| &agent.env)
        .unwrap_or(&definition.env);
    for (key, value) in agent_env {
        config
            .env
            .retain(|reference| reference.key.trim() != key.as_str());
        config.env.push(AcpProviderEnvReference {
            key: key.clone(),
            source: AcpProviderEnvSource::Literal,
            value: Some(value.clone()),
            secret_lookup_key: None,
            redacted_hint: "Agent configuration value".to_string(),
        });
    }
    validate_acp_config(&config).map_err(|error| {
        error
            .with_diagnostic("agentId", agent_id.as_str())
            .with_recovery_hint(
                "Configure a valid ACP command for this Agent before adding model provider profiles.",
            )
    })?;
    Ok(config)
}

fn merge_acp_runtime_options(
    mut options: ProviderOptions,
    config: AcpProviderConfig,
) -> VibexResult<ProviderOptions> {
    validate_provider_option_keys(&options)?;
    options
        .entries
        .retain(|entry| entry.key.trim() != ACP_CONFIG_OPTION_KEY);
    let encoded = acp_config_to_options(&config)?
        .entries
        .into_iter()
        .next()
        .expect("typed ACP options always contain the ACP config entry");
    options.entries.push(encoded);
    validate_acp_profile_options(Some(&options))?;
    Ok(options)
}

fn with_acp_configured_models(
    options: ProviderOptions,
    models: Vec<String>,
) -> VibexResult<ProviderOptions> {
    let mut config = acp_config_from_options(&options)?.ok_or_else(|| {
        VibexError::validation(
            "acp_config_missing",
            "ACP model provider profile is missing its inherited runtime configuration",
        )
    })?;
    config.models = models;
    merge_acp_runtime_options(options, config)
}

fn configured_acp_model_ids(
    configured_models: &[ProviderConfiguredModel],
    default_model: Option<&str>,
) -> Vec<String> {
    let mut models = Vec::new();
    for model in configured_models.iter().filter(|model| model.enabled) {
        let model = model.id.trim();
        if !model.is_empty() && !models.iter().any(|existing| existing == model) {
            models.push(model.to_string());
        }
    }
    if models.is_empty()
        && let Some(model) = default_model
            .map(str::trim)
            .filter(|model| !model.is_empty())
    {
        models.push(model.to_string());
    }
    models
}

pub fn acp_config_to_options(config: &AcpProviderConfig) -> VibexResult<ProviderOptions> {
    validate_acp_config(config)?;
    let encoded = serde_json::to_string(config).map_err(|error| {
        VibexError::validation(
            "acp_config_encode_failed",
            "ACP provider config could not be encoded",
        )
        .with_diagnostic("serde", error.to_string())
    })?;
    Ok(ProviderOptions {
        schema_version: 1,
        entries: vec![option_entry(ACP_CONFIG_OPTION_KEY, encoded)],
    })
}

pub fn acp_config_from_options(
    options: &ProviderOptions,
) -> VibexResult<Option<AcpProviderConfig>> {
    validate_provider_option_keys(options)?;
    let Some(entry) = options
        .entries
        .iter()
        .find(|entry| entry.key == ACP_CONFIG_OPTION_KEY)
    else {
        return Ok(None);
    };
    let config = serde_json::from_str::<AcpProviderConfig>(&entry.value).map_err(|error| {
        VibexError::validation(
            "acp_config_decode_failed",
            "ACP provider config could not be decoded",
        )
        .with_diagnostic("serde", error.to_string())
    })?;
    validate_acp_config(&config)?;
    Ok(Some(config))
}

pub fn codex_runtime_config_from_profile(
    profile: &ProviderProfile,
    model_override: Option<String>,
) -> VibexResult<CodexProviderRuntimeConfig> {
    if agent_model_provider_kind(&profile.agent_id) != ProviderKind::Codex {
        return Err(VibexError::validation(
            "codex_profile_kind_mismatch",
            "Codex runtime config requires a Codex Agent provider profile",
        )
        .with_diagnostic("providerProfileId", profile.id.as_str())
        .with_diagnostic("providerKind", profile.kind.to_string()));
    }
    let provider_config_toml = provider_runtime_option_value(
        &profile.provider_options,
        CODEX_MODEL_PROVIDER_CONFIG_TOML_OPTION_KEY,
    )?;
    let explicit_model_provider_id = provider_runtime_option_value(
        &profile.provider_options,
        CODEX_MODEL_PROVIDER_ID_OPTION_KEY,
    )?;
    let native_model_provider_id = if explicit_model_provider_id.is_none() {
        provider_runtime_option_value(
            &profile.provider_options,
            CODEX_NATIVE_MODEL_PROVIDER_OPTION_KEY,
        )?
    } else {
        None
    };
    let model_provider_id = explicit_model_provider_id
        .or(native_model_provider_id)
        .or_else(|| profile.account_alias.clone())
        .map(|value| sanitize_codex_model_provider_id(&value))
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "vibex".to_string());

    let provider_config_toml_keys = provider_config_toml
        .as_deref()
        .map(codex_provider_toml_keys)
        .unwrap_or_default();
    let api_key_env_key =
        provider_runtime_option_value(&profile.provider_options, CODEX_API_KEY_ENV_OPTION_KEY)?
            .or_else(|| {
                provider_config_toml
                    .as_deref()
                    .and_then(codex_provider_toml_env_key)
            })
            .unwrap_or_else(|| "OPENAI_API_KEY".to_string());
    let wire_api =
        provider_runtime_option_value(&profile.provider_options, "wireApi")?.or_else(|| {
            provider_config_toml
                .as_deref()
                .and_then(codex_provider_toml_wire_api)
        });
    if let Some(wire_api) = wire_api.as_deref() {
        validate_codex_wire_api(wire_api)?;
    }
    let api_key = secrets::preferred_api_key_reference(&profile.secrets, &api_key_env_key)
        .map(secrets::resolve_provider_secret)
        .transpose()?
        .flatten();

    let model = model_override
        .map(|model| model.trim().to_string())
        .filter(|model| !model.is_empty())
        .or_else(|| profile.default_model.clone());

    Ok(CodexProviderRuntimeConfig {
        model,
        model_provider_id,
        provider_config_toml,
        provider_config_toml_keys,
        base_url: profile.base_url.clone(),
        wire_api,
        api_key_env_key,
        api_key,
    })
}

fn validate_agent_model_interfaces(
    agent_id: &AgentId,
    models: &[ProviderConfiguredModel],
    options: Option<&ProviderOptions>,
) -> VibexResult<()> {
    let registry = vibex_core::AgentProviderProjectionRegistry::builtin()?;
    let supported_protocols = registry
        .descriptors_for_agent(agent_id)
        .flat_map(|descriptor| descriptor.model_interfaces.iter())
        .map(|interface| interface.wire_protocol_id.as_str())
        .collect::<HashSet<_>>();
    if !supported_protocols.is_empty() {
        for model in models {
            if let Some(wire_api) = model.wire_api
                && !supported_protocols.contains(wire_api.wire_protocol_id())
            {
                return Err(unsupported_model_interface(
                    agent_id,
                    wire_api.wire_protocol_id(),
                ));
            }
        }
    }

    if agent_id.as_str() == "codex"
        && let Some(options) = options
    {
        if let Some(wire_api) = provider_runtime_option_value(options, "wireApi")? {
            validate_codex_wire_api(&wire_api)?;
        }
        if let Some(fragment) =
            provider_runtime_option_value(options, CODEX_MODEL_PROVIDER_CONFIG_TOML_OPTION_KEY)?
            && let Some(wire_api) = codex_provider_toml_wire_api(&fragment)
        {
            validate_codex_wire_api(&wire_api)?;
        }
    }
    Ok(())
}

fn validate_codex_wire_api(value: &str) -> VibexResult<()> {
    let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if matches!(normalized.as_str(), "responses" | "openai_responses") {
        return Ok(());
    }
    Err(unsupported_model_interface(
        &AgentId::parse("codex")?,
        value,
    ))
}

fn unsupported_model_interface(agent_id: &AgentId, wire_api: &str) -> VibexError {
    VibexError::validation(
        "agent_model_interface_unsupported",
        "model interface is not supported by the selected Agent projection descriptor",
    )
    .with_diagnostic("agentId", agent_id.as_str())
    .with_diagnostic("wireProtocolId", wire_api)
}

pub fn provider_option_value(options: &ProviderOptions, key: &str) -> Option<String> {
    options
        .entries
        .iter()
        .find(|entry| entry.key == key)
        .map(|entry| entry.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn provider_runtime_option_value(
    options: &ProviderOptions,
    key: &str,
) -> VibexResult<Option<String>> {
    let mut resolved: Option<String> = None;
    for entry in options
        .entries
        .iter()
        .filter(|entry| entry.key.trim() == key)
    {
        let value = entry.value.trim().to_string();
        if value.is_empty() {
            continue;
        }
        if let Some(existing) = resolved.as_ref() {
            if existing != &value {
                return Err(VibexError::validation(
                    "provider_option_key_conflict",
                    "provider option key has conflicting runtime values",
                )
                .with_diagnostic("key", key));
            }
            continue;
        }
        resolved = Some(value);
    }
    Ok(resolved)
}

fn codex_provider_toml_env_key(toml_fragment: &str) -> Option<String> {
    codex_provider_toml_string(toml_fragment, "env_key")
}

fn codex_provider_toml_wire_api(toml_fragment: &str) -> Option<String> {
    codex_provider_toml_string(toml_fragment, "wire_api")
}

fn codex_provider_toml_string(toml_fragment: &str, key: &str) -> Option<String> {
    toml_fragment
        .parse::<toml::Value>()
        .ok()
        .and_then(|value| {
            value
                .get(key)
                .and_then(toml::Value::as_str)
                .map(str::trim)
                .map(str::to_string)
        })
        .filter(|value| !value.is_empty())
}

fn codex_provider_toml_keys(toml_fragment: &str) -> Vec<String> {
    toml_fragment
        .parse::<toml::Value>()
        .ok()
        .and_then(|value| {
            value
                .as_table()
                .map(|table| table.keys().cloned().collect::<Vec<_>>())
        })
        .unwrap_or_default()
}

fn sanitize_codex_model_provider_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-' | '.') {
                ch
            } else {
                '-'
            }
        })
        .collect();
    sanitized.trim_matches('-').chars().take(80).collect()
}

fn validate_acp_profile_options(options: Option<&ProviderOptions>) -> VibexResult<()> {
    let options = options.ok_or_else(|| {
        VibexError::validation(
            "acp_config_missing",
            "ACP provider profiles require typed ACP configuration",
        )
    })?;
    if acp_config_from_options(options)?.is_none() {
        return Err(VibexError::validation(
            "acp_config_missing",
            "ACP provider profiles require typed ACP configuration",
        ));
    }
    Ok(())
}

fn validate_provider_option_keys(options: &ProviderOptions) -> VibexResult<()> {
    let mut keys = HashSet::new();
    for entry in &options.entries {
        let key = entry.key.trim();
        if key.is_empty() {
            return Err(VibexError::validation(
                "provider_option_key_empty",
                "provider option keys must not be empty",
            ));
        }
        if !keys.insert(key.to_string()) {
            return Err(VibexError::validation(
                "provider_option_key_duplicate",
                "provider option keys must be unique",
            )
            .with_diagnostic("key", key));
        }
    }
    Ok(())
}

fn validate_acp_config(config: &AcpProviderConfig) -> VibexResult<()> {
    if config.command.trim().is_empty() {
        return Err(VibexError::validation(
            "acp_command_empty",
            "ACP provider command must not be empty",
        ));
    }
    validate_no_control_chars("acp_command_invalid", "command", &config.command)?;
    validate_string_list("args", &config.args, true)?;
    validate_acp_env(&config.env)?;
    if let Some(cwd_template) = config.cwd_template.as_deref() {
        validate_acp_cwd_template(cwd_template)?;
    }
    validate_string_list("models", &config.models, false)?;
    validate_string_list("modes", &config.modes, false)?;
    validate_string_list("features", &config.features, false)?;
    validate_string_list("disabledTools", &config.disabled_tools, false)?;
    Ok(())
}

fn validate_no_control_chars(code: &str, field: &str, value: &str) -> VibexResult<()> {
    if value.contains('\0') || value.contains('\n') || value.contains('\r') {
        return Err(VibexError::validation(
            code,
            "ACP provider config fields must not contain control characters",
        )
        .with_diagnostic("field", field));
    }
    Ok(())
}

fn validate_string_list(field: &str, values: &[String], allow_duplicates: bool) -> VibexResult<()> {
    let mut seen = HashSet::new();
    for value in values {
        if value.trim().is_empty() {
            return Err(VibexError::validation(
                "acp_list_value_empty",
                "ACP provider list values must not be empty",
            )
            .with_diagnostic("field", field));
        }
        validate_no_control_chars("acp_list_value_invalid", field, value)?;
        if !allow_duplicates && !seen.insert(value.trim().to_string()) {
            return Err(VibexError::validation(
                "acp_list_value_duplicate",
                "ACP provider list values must be unique",
            )
            .with_diagnostic("field", field)
            .with_diagnostic("value", value.trim()));
        }
    }
    Ok(())
}

fn validate_acp_env(env: &[AcpProviderEnvReference]) -> VibexResult<()> {
    let mut keys = HashSet::new();
    for reference in env {
        let key = reference.key.trim();
        if !is_valid_env_key(key) {
            return Err(VibexError::validation(
                "acp_env_key_invalid",
                "ACP environment keys must use shell environment variable syntax",
            )
            .with_diagnostic("key", key));
        }
        if !keys.insert(key.to_string()) {
            return Err(VibexError::validation(
                "acp_env_key_duplicate",
                "ACP environment keys must be unique",
            )
            .with_diagnostic("key", key));
        }
        validate_no_control_chars("acp_env_key_invalid", "env.key", key)?;
        validate_acp_env_reference(reference)?;
    }
    Ok(())
}

fn validate_acp_env_reference(reference: &AcpProviderEnvReference) -> VibexResult<()> {
    match reference.source {
        AcpProviderEnvSource::ProcessEnvironment => {
            if reference.value.is_some() || reference.secret_lookup_key.is_some() {
                return Err(VibexError::validation(
                    "acp_env_reference_invalid",
                    "process environment references must not carry literal or secret values",
                )
                .with_diagnostic("key", reference.key.trim()));
            }
        }
        AcpProviderEnvSource::SecretReference => {
            let lookup_key = reference.secret_lookup_key.as_deref().unwrap_or("").trim();
            if lookup_key.is_empty()
                || lookup_key.contains(['\0', '\n', '\r'])
                || reference.value.is_some()
            {
                return Err(VibexError::validation(
                    "acp_env_secret_reference_invalid",
                    "secret ACP environment references require a lookup key and no literal value",
                )
                .with_diagnostic("key", reference.key.trim()));
            }
        }
        AcpProviderEnvSource::Literal => {
            let value = reference.value.as_deref().unwrap_or("");
            validate_no_control_chars("acp_env_literal_invalid", "env.value", value)?;
            if looks_secret_like(&reference.key, value) {
                return Err(VibexError::validation(
                    "acp_env_literal_secret_rejected",
                    "credential-like ACP environment values must use secret references",
                )
                .with_diagnostic("key", reference.key.trim()));
            }
        }
    }
    Ok(())
}

fn is_valid_env_key(value: &str) -> bool {
    let mut chars = value.chars();
    let Some(first) = chars.next() else {
        return false;
    };
    (first == '_' || first.is_ascii_alphabetic())
        && chars.all(|ch| ch == '_' || ch.is_ascii_alphanumeric())
}

fn looks_secret_like(key: &str, value: &str) -> bool {
    let upper = key.to_ascii_uppercase();
    upper.contains("TOKEN")
        || upper.contains("SECRET")
        || upper.contains("PASSWORD")
        || upper.contains("API_KEY")
        || (upper.ends_with("_KEY") && value.len() > 8)
}

fn validate_acp_cwd_template(cwd_template: &str) -> VibexResult<()> {
    let trimmed = cwd_template.trim();
    if trimmed.is_empty() {
        return Err(VibexError::validation(
            "acp_cwd_template_empty",
            "ACP cwd template must not be empty when provided",
        ));
    }
    validate_no_control_chars("acp_cwd_template_invalid", "cwdTemplate", trimmed)?;
    let normalized = trimmed
        .replace("{workspaceRoot}", "workspace-root")
        .replace("{projectRoot}", "project-root");
    if Path::new(&normalized)
        .components()
        .any(|component| matches!(component, Component::ParentDir))
    {
        return Err(VibexError::validation(
            "acp_cwd_template_traversal",
            "ACP cwd template must not contain path traversal",
        )
        .with_diagnostic("cwdTemplate", trimmed));
    }
    Ok(())
}

fn build_preview(
    profile: &ProviderProfile,
    mcp_servers: &[McpServer],
    skills: &[Skill],
    prompts: &[Prompt],
) -> ProviderInjectionPreview {
    let created_at_ms = unix_timestamp_ms();
    let acp_config = if profile.kind == ProviderKind::Acp {
        acp_config_from_options(&profile.provider_options)
            .ok()
            .flatten()
    } else {
        None
    };
    let model = profile.default_model.clone().or_else(|| {
        profile
            .configured_models
            .iter()
            .find(|model| model.enabled && !model.id.trim().is_empty())
            .map(|model| model.id.trim().to_string())
    });
    let endpoint = profile
        .base_url
        .clone()
        .or_else(|| default_endpoint(agent_model_provider_kind(&profile.agent_id)));
    let mut sdk_options = Vec::new();
    push_optional_field(&mut sdk_options, "model", model.clone(), false, "profile");
    push_optional_field(
        &mut sdk_options,
        "endpoint",
        endpoint.clone(),
        false,
        "profile",
    );
    sdk_options.extend(
        profile
            .provider_options
            .entries
            .iter()
            .filter(|entry| entry.key != ACP_CONFIG_OPTION_KEY)
            .map(|entry| ProviderInjectionField {
                key: entry.key.clone(),
                value: entry.value.clone(),
                secret: false,
                source: "provider_options".to_string(),
            }),
    );
    if let Some(config) = acp_config.as_ref() {
        sdk_options.push(ProviderInjectionField {
            key: "processStrategy".to_string(),
            value: serde_json::to_string(&config.process_strategy)
                .unwrap_or_else(|_| "\"per_session\"".to_string())
                .trim_matches('"')
                .to_string(),
            secret: false,
            source: "acp_config".to_string(),
        });
        push_optional_field(
            &mut sdk_options,
            "cwdTemplate",
            config.cwd_template.clone(),
            false,
            "acp_config",
        );
        for model in &config.models {
            sdk_options.push(ProviderInjectionField {
                key: "acpModel".to_string(),
                value: model.clone(),
                secret: false,
                source: "acp_config".to_string(),
            });
        }
        for mode in &config.modes {
            sdk_options.push(ProviderInjectionField {
                key: "acpMode".to_string(),
                value: mode.clone(),
                secret: false,
                source: "acp_config".to_string(),
            });
        }
        for feature in &config.features {
            sdk_options.push(ProviderInjectionField {
                key: "acpFeature".to_string(),
                value: feature.clone(),
                secret: false,
                source: "acp_config".to_string(),
            });
        }
        for tool in &config.disabled_tools {
            sdk_options.push(ProviderInjectionField {
                key: "disabledTool".to_string(),
                value: tool.clone(),
                secret: false,
                source: "acp_config".to_string(),
            });
        }
    }

    let mut cli_args = Vec::new();
    if let Some(config) = acp_config.as_ref() {
        cli_args.extend(acp_cli_fields(config));
    } else {
        if let Some(model) = model.as_ref() {
            cli_args.push(ProviderInjectionField {
                key: "--model".to_string(),
                value: model.clone(),
                secret: false,
                source: "profile".to_string(),
            });
        }
        if let Some(endpoint) = endpoint.as_ref() {
            cli_args.push(ProviderInjectionField {
                key: "--base-url".to_string(),
                value: endpoint.clone(),
                secret: false,
                source: "profile".to_string(),
            });
        }
    }

    let mut env = secret_env_fields(profile);
    if let Some(config) = acp_config.as_ref() {
        env.extend(acp_env_fields(config));
    }
    let overlay_files = vec![ProviderInjectionOverlayFile {
        path: format!("~/.vibex/runtime/{}/provider.json", profile.id.as_str()),
        description: "temporary Vibex-scoped provider overlay".to_string(),
        redacted_preview: provider_overlay_preview(profile, acp_config.as_ref()),
    }];

    ProviderInjectionPreview {
        preview_id: RequestId::new(),
        profile: profile.summary(),
        strategy_order: vec![
            ProviderInjectionStrategy::SdkParameters,
            ProviderInjectionStrategy::CliArgs,
            ProviderInjectionStrategy::ProcessEnvironment,
            ProviderInjectionStrategy::TemporaryConfigOverlay,
        ],
        endpoint,
        model,
        sdk_options,
        cli_args,
        env,
        overlay_files,
        mcp_servers: mcp_servers
            .iter()
            .map(|server| format_mcp_preview_entry(server, profile.kind))
            .collect(),
        skills: skills
            .iter()
            .map(|skill| format_skill_preview_entry(skill, profile.kind))
            .chain(prompts.iter().map(format_prompt_preview_entry))
            .collect(),
        sandbox_defaults: profile.sandbox_defaults.clone(),
        network_defaults: profile.network_defaults.clone(),
        permission_defaults: profile.permission_defaults.clone(),
        created_at_ms,
    }
}

fn filter_profiles<'a>(
    profiles: &'a [ProviderProfile],
    provider_profile_ids: Option<&Vec<ProviderProfileId>>,
) -> Vec<&'a ProviderProfile> {
    let selected: Option<HashSet<&str>> =
        provider_profile_ids.map(|ids| ids.iter().map(ProviderProfileId::as_str).collect());
    profiles
        .iter()
        .filter(|profile| {
            selected
                .as_ref()
                .is_none_or(|ids| ids.contains(profile.id.as_str()))
        })
        .collect()
}

#[derive(Clone, Copy)]
enum ProviderApiProbeKind {
    ModelList,
    SimplePrompt,
}

struct ProviderApiProbeOutcome {
    passed: bool,
    code: String,
    message: String,
    latency_ms: Option<u32>,
    diagnostics: Vec<ProviderBindingMetadata>,
}

fn run_provider_api_probe(
    profile: &ProviderProfile,
    probe_kind: ProviderApiProbeKind,
) -> VibexResult<ProviderApiProbeOutcome> {
    let Some(wire_api) = effective_profile_wire_api(profile)? else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_protocol_missing",
            "Provider profile does not select a supported model API protocol",
            vec![diagnostic("wireProtocolId", "missing")],
        ));
    };
    match wire_api {
        vibex_core::ProviderModelWireApi::OpenaiResponses
        | vibex_core::ProviderModelWireApi::OpenaiChatCompletions => {
            run_openai_compatible_api_probe(profile, probe_kind, wire_api)
        }
        vibex_core::ProviderModelWireApi::AnthropicMessages => {
            run_anthropic_api_probe(profile, probe_kind)
        }
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi => {
            run_google_generative_ai_probe(profile, probe_kind)
        }
        vibex_core::ProviderModelWireApi::AwsBedrockConverse => {
            run_bedrock_converse_probe(profile, probe_kind)
        }
    }
}

fn run_openai_compatible_api_probe(
    profile: &ProviderProfile,
    probe_kind: ProviderApiProbeKind,
    wire_api: vibex_core::ProviderModelWireApi,
) -> VibexResult<ProviderApiProbeOutcome> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
            vec![diagnostic("secret", "missing")],
        ));
    };
    let Some(base_url) = profile_protocol_base_url(profile, wire_api) else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
            vec![diagnostic("endpoint", "missing")],
        ));
    };

    let client = provider_probe_http_client()?;
    match probe_kind {
        ProviderApiProbeKind::ModelList => {
            let endpoints = provider_api_endpoint_candidates(
                base_url,
                "models",
                profile_uses_full_api_url(profile),
                false,
            );
            probe_get_json(
                &client,
                &endpoints,
                |request| request.bearer_auth(&api_key),
                "agent_model_provider_model_list_probe_passed",
                "Model list API request succeeded",
            )
        }
        ProviderApiProbeKind::SimplePrompt => {
            let Some(model) = profile
                .default_model
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(provider_api_probe_fail(
                    "agent_model_provider_model_missing",
                    "Provider profile is missing a default model",
                    vec![diagnostic("model", "missing")],
                ));
            };
            let (path, body) =
                if wire_api == vibex_core::ProviderModelWireApi::OpenaiChatCompletions {
                    (
                        "chat/completions",
                        serde_json::json!({
                            "model": model,
                            "messages": [{ "role": "user", "content": "ping" }],
                            "max_tokens": 1,
                            "stream": false
                        }),
                    )
                } else {
                    (
                        "responses",
                        serde_json::json!({
                            "model": model,
                            "input": "ping",
                            "max_output_tokens": 1,
                            "stream": false
                        }),
                    )
                };
            let endpoints = provider_api_endpoint_candidates(
                base_url,
                path,
                profile_uses_full_api_url(profile),
                false,
            );
            probe_post_json(
                &client,
                &endpoints,
                body,
                |request| request.bearer_auth(&api_key),
                "agent_model_provider_simple_prompt_probe_passed",
                "Simple prompt API request succeeded",
            )
        }
    }
}

fn run_anthropic_api_probe(
    profile: &ProviderProfile,
    probe_kind: ProviderApiProbeKind,
) -> VibexResult<ProviderApiProbeOutcome> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available auth token",
            vec![diagnostic("secret", "missing")],
        ));
    };
    let Some(base_url) =
        profile_protocol_base_url(profile, vibex_core::ProviderModelWireApi::AnthropicMessages)
    else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
            vec![diagnostic("endpoint", "missing")],
        ));
    };

    let client = provider_probe_http_client()?;
    match probe_kind {
        ProviderApiProbeKind::ModelList => {
            let endpoints = provider_api_endpoint_candidates(
                base_url,
                "models",
                profile_uses_full_api_url(profile),
                true,
            );
            probe_get_json(
                &client,
                &endpoints,
                |request| {
                    request
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", "2023-06-01")
                },
                "agent_model_provider_model_list_probe_passed",
                "Model list API request succeeded",
            )
        }
        ProviderApiProbeKind::SimplePrompt => {
            let Some(model) = profile
                .default_model
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(provider_api_probe_fail(
                    "agent_model_provider_model_missing",
                    "Provider profile is missing a default model",
                    vec![diagnostic("model", "missing")],
                ));
            };
            let endpoints = provider_api_endpoint_candidates(
                base_url,
                "messages",
                profile_uses_full_api_url(profile),
                true,
            );
            let body = serde_json::json!({
                "model": model,
                "max_tokens": 1,
                "messages": [{ "role": "user", "content": "ping" }]
            });
            probe_post_json(
                &client,
                &endpoints,
                body,
                |request| {
                    request
                        .header("x-api-key", &api_key)
                        .header("anthropic-version", "2023-06-01")
                },
                "agent_model_provider_simple_prompt_probe_passed",
                "Simple prompt API request succeeded",
            )
        }
    }
}

fn run_google_generative_ai_probe(
    profile: &ProviderProfile,
    probe_kind: ProviderApiProbeKind,
) -> VibexResult<ProviderApiProbeOutcome> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
            vec![diagnostic("secret", "missing")],
        ));
    };
    let Some(base_url) = profile_protocol_base_url(
        profile,
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi,
    ) else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
            vec![diagnostic("endpoint", "missing")],
        ));
    };
    let client = provider_probe_http_client()?;
    match probe_kind {
        ProviderApiProbeKind::ModelList => probe_get_json(
            &client,
            &google_api_endpoint_candidates(base_url, "models", profile_uses_full_api_url(profile)),
            |request| request.header("x-goog-api-key", &api_key),
            "agent_model_provider_model_list_probe_passed",
            "Google model list API request succeeded",
        ),
        ProviderApiProbeKind::SimplePrompt => {
            let Some(model) = profile
                .default_model
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(provider_api_probe_fail(
                    "agent_model_provider_model_missing",
                    "Provider profile is missing a default model",
                    vec![diagnostic("model", "missing")],
                ));
            };
            let model = model.strip_prefix("models/").unwrap_or(model);
            let model = encoded_url_path_segment(model)?;
            probe_post_json(
                &client,
                &google_api_endpoint_candidates(
                    base_url,
                    &format!("models/{model}:generateContent"),
                    profile_uses_full_api_url(profile),
                ),
                serde_json::json!({
                    "contents": [{"role": "user", "parts": [{"text": "ping"}]}],
                    "generationConfig": {"maxOutputTokens": 1}
                }),
                |request| request.header("x-goog-api-key", &api_key),
                "agent_model_provider_simple_prompt_probe_passed",
                "Google Generative AI prompt request succeeded",
            )
        }
    }
}

fn run_bedrock_converse_probe(
    profile: &ProviderProfile,
    probe_kind: ProviderApiProbeKind,
) -> VibexResult<ProviderApiProbeOutcome> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
            vec![diagnostic("secret", "missing")],
        ));
    };
    let Some(base_url) = profile_protocol_base_url(
        profile,
        vibex_core::ProviderModelWireApi::AwsBedrockConverse,
    ) else {
        return Ok(provider_api_probe_fail(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
            vec![diagnostic("endpoint", "missing")],
        ));
    };
    let client = provider_probe_http_client()?;
    match probe_kind {
        ProviderApiProbeKind::ModelList => probe_get_json(
            &client,
            &provider_api_endpoint_candidates(
                base_url,
                "models",
                profile_uses_full_api_url(profile),
                false,
            ),
            |request| request.bearer_auth(&api_key),
            "agent_model_provider_model_list_probe_passed",
            "Bedrock-compatible model list API request succeeded",
        ),
        ProviderApiProbeKind::SimplePrompt => {
            let Some(model) = profile
                .default_model
                .as_deref()
                .filter(|value| !value.trim().is_empty())
            else {
                return Ok(provider_api_probe_fail(
                    "agent_model_provider_model_missing",
                    "Provider profile is missing a default model",
                    vec![diagnostic("model", "missing")],
                ));
            };
            let model = encoded_url_path_segment(model)?;
            probe_post_json(
                &client,
                &provider_api_endpoint_candidates(
                    base_url,
                    &format!("model/{model}/converse"),
                    profile_uses_full_api_url(profile),
                    false,
                ),
                serde_json::json!({
                    "messages": [{
                        "role": "user",
                        "content": [{"text": "ping"}]
                    }],
                    "inferenceConfig": {"maxTokens": 1}
                }),
                |request| request.bearer_auth(&api_key),
                "agent_model_provider_simple_prompt_probe_passed",
                "Bedrock Converse prompt request succeeded",
            )
        }
    }
}

fn effective_profile_wire_api(
    profile: &ProviderProfile,
) -> VibexResult<Option<vibex_core::ProviderModelWireApi>> {
    let configured = profile
        .default_model
        .as_deref()
        .and_then(|default_model| {
            profile
                .configured_models
                .iter()
                .find(|model| model.id == default_model && model.enabled)
        })
        .and_then(|model| model.wire_api)
        .or_else(|| {
            profile
                .configured_models
                .iter()
                .find(|model| model.enabled && model.wire_api.is_some())
                .and_then(|model| model.wire_api)
        });
    if configured.is_some() {
        return Ok(configured);
    }
    if let Some(value) = provider_option_value(&profile.provider_options, "wireApi")
        && let Some(wire_api) = provider_model_wire_api_from_alias(&value)
    {
        return Ok(Some(wire_api));
    }
    let registry = vibex_core::AgentProviderProjectionRegistry::builtin()?;
    Ok(registry
        .descriptors_for_agent(&profile.agent_id)
        .flat_map(|descriptor| descriptor.model_interfaces.iter())
        .find_map(|interface| {
            vibex_core::ProviderModelWireApi::from_wire_protocol_id(&interface.wire_protocol_id)
        }))
}

fn profile_protocol_base_url(
    profile: &ProviderProfile,
    wire_api: vibex_core::ProviderModelWireApi,
) -> Option<&str> {
    let option_key = wire_api.protocol_base_url_option_key();
    profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == option_key)
        .map(|entry| entry.value.trim())
        .filter(|value| !value.is_empty())
        .or_else(|| {
            profile
                .base_url
                .as_deref()
                .map(str::trim)
                .filter(|value| !value.is_empty())
        })
}

fn provider_model_wire_api_from_alias(value: &str) -> Option<vibex_core::ProviderModelWireApi> {
    let normalized = value.trim().to_ascii_lowercase().replace(['-', ' '], "_");
    if normalized.contains("bedrock") || normalized == "converse" {
        Some(vibex_core::ProviderModelWireApi::AwsBedrockConverse)
    } else if normalized.contains("google") || normalized.contains("gemini") {
        Some(vibex_core::ProviderModelWireApi::GoogleGenerativeAi)
    } else if normalized.contains("anthropic") || normalized == "messages" {
        Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
    } else if normalized.contains("chat") || normalized == "completions" {
        Some(vibex_core::ProviderModelWireApi::OpenaiChatCompletions)
    } else if normalized.contains("response") {
        Some(vibex_core::ProviderModelWireApi::OpenaiResponses)
    } else {
        None
    }
}

fn fetch_provider_profile_models(
    profile: &ProviderProfile,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)> {
    match effective_profile_wire_api(profile)? {
        Some(vibex_core::ProviderModelWireApi::OpenaiResponses) => {
            fetch_openai_compatible_profile_models(
                profile,
                vibex_core::ProviderModelWireApi::OpenaiResponses,
            )
        }
        Some(vibex_core::ProviderModelWireApi::OpenaiChatCompletions) => {
            fetch_openai_compatible_profile_models(
                profile,
                vibex_core::ProviderModelWireApi::OpenaiChatCompletions,
            )
        }
        Some(vibex_core::ProviderModelWireApi::AnthropicMessages) => {
            fetch_anthropic_profile_models(profile)
        }
        Some(vibex_core::ProviderModelWireApi::GoogleGenerativeAi) => {
            fetch_google_profile_models(profile)
        }
        Some(vibex_core::ProviderModelWireApi::AwsBedrockConverse) => {
            fetch_bedrock_profile_models(profile)
        }
        None => Err(VibexError::validation(
            "agent_model_provider_protocol_missing",
            "Provider profile does not select a supported model API protocol",
        )),
    }
}

fn fetch_openai_compatible_profile_models(
    profile: &ProviderProfile,
    wire_api: vibex_core::ProviderModelWireApi,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Err(VibexError::validation(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
        ));
    };
    let Some(base_url) = profile_protocol_base_url(profile, wire_api) else {
        return Err(VibexError::validation(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
        ));
    };

    let client = provider_probe_http_client()?;
    fetch_models_from_endpoints(
        &client,
        &model_list_endpoint_candidates(base_url, profile_uses_full_api_url(profile), false),
        |request| request.bearer_auth(&api_key),
    )
}

fn fetch_google_profile_models(
    profile: &ProviderProfile,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Err(VibexError::validation(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
        ));
    };
    let Some(base_url) = profile_protocol_base_url(
        profile,
        vibex_core::ProviderModelWireApi::GoogleGenerativeAi,
    ) else {
        return Err(VibexError::validation(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
        ));
    };
    let client = provider_probe_http_client()?;
    fetch_models_from_endpoints(
        &client,
        &google_api_endpoint_candidates(base_url, "models", profile_uses_full_api_url(profile)),
        |request| request.header("x-goog-api-key", &api_key),
    )
}

fn fetch_bedrock_profile_models(
    profile: &ProviderProfile,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Err(VibexError::validation(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available API key",
        ));
    };
    let Some(base_url) = profile_protocol_base_url(
        profile,
        vibex_core::ProviderModelWireApi::AwsBedrockConverse,
    ) else {
        return Err(VibexError::validation(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
        ));
    };
    let client = provider_probe_http_client()?;
    fetch_models_from_endpoints(
        &client,
        &model_list_endpoint_candidates(base_url, profile_uses_full_api_url(profile), false),
        |request| request.bearer_auth(&api_key),
    )
}

fn fetch_anthropic_profile_models(
    profile: &ProviderProfile,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)> {
    let Some(api_key) = resolved_profile_secret_value(profile)? else {
        return Err(VibexError::validation(
            "agent_model_provider_secret_missing",
            "Provider profile is missing an available auth token",
        ));
    };
    let Some(base_url) =
        profile_protocol_base_url(profile, vibex_core::ProviderModelWireApi::AnthropicMessages)
    else {
        return Err(VibexError::validation(
            "agent_model_provider_endpoint_missing",
            "Provider profile is missing an API request URL",
        ));
    };

    let client = provider_probe_http_client()?;
    fetch_models_from_endpoints(
        &client,
        &model_list_endpoint_candidates(base_url, profile_uses_full_api_url(profile), true),
        |request| {
            request
                .header("x-api-key", &api_key)
                .header("anthropic-version", "2023-06-01")
        },
    )
}

fn fetch_models_from_endpoints<F>(
    client: &reqwest::blocking::Client,
    endpoints: &[String],
    apply_auth: F,
) -> VibexResult<(Vec<String>, Vec<ProviderBindingMetadata>)>
where
    F: Fn(reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder + Copy,
{
    let mut last_error: Option<VibexError> = None;
    for endpoint in endpoints {
        let started = Instant::now();
        let response = apply_auth(client.get(endpoint).header("accept", "application/json")).send();
        let latency_ms = elapsed_ms(started);
        match response {
            Ok(response) if response.status().is_success() => {
                let status = response.status().as_u16();
                let body = response.text().map_err(|error| {
                    VibexError::capability(
                        "agent_model_provider_model_fetch_body_failed",
                        "failed to read provider model list response",
                    )
                    .with_diagnostic("endpoint", redact_url_for_diagnostics(endpoint))
                    .with_diagnostic("error", error.to_string())
                })?;
                let models = parse_provider_model_ids(&body)?;
                if models.is_empty() {
                    return Err(VibexError::capability(
                        "agent_model_provider_model_fetch_empty",
                        "provider model list response did not contain model ids",
                    )
                    .with_diagnostic("endpoint", redact_url_for_diagnostics(endpoint)));
                }
                return Ok((
                    models,
                    vec![
                        diagnostic("endpoint", redact_url_for_diagnostics(endpoint)),
                        diagnostic("httpStatus", status.to_string()),
                        diagnostic("latencyMs", latency_ms.to_string()),
                        diagnostic("redacted", "true"),
                    ],
                ));
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().unwrap_or_default();
                let error = VibexError::capability(
                    "agent_model_provider_model_fetch_http_failed",
                    format!("Provider model list request failed with HTTP {status}"),
                )
                .with_diagnostic("endpoint", redact_url_for_diagnostics(endpoint))
                .with_diagnostic("httpStatus", status.to_string())
                .with_diagnostic("response", truncate_diagnostic_value(&body))
                .with_diagnostic("latencyMs", latency_ms.to_string())
                .with_diagnostic("redacted", "true");
                if !matches!(status, 404 | 405) {
                    return Err(error);
                }
                last_error = Some(error);
            }
            Err(error) => {
                last_error = Some(
                    VibexError::capability(
                        "agent_model_provider_model_fetch_transport_failed",
                        "Provider model list request failed before receiving an HTTP response",
                    )
                    .with_diagnostic("endpoint", redact_url_for_diagnostics(endpoint))
                    .with_diagnostic("error", truncate_diagnostic_value(&error.to_string()))
                    .with_diagnostic("latencyMs", latency_ms.to_string())
                    .with_diagnostic("redacted", "true"),
                );
            }
        }
    }

    Err(last_error.unwrap_or_else(|| {
        VibexError::validation(
            "agent_model_provider_endpoint_missing",
            "Provider profile did not produce an API endpoint to fetch models",
        )
    }))
}

fn parse_provider_model_ids(body: &str) -> VibexResult<Vec<String>> {
    let value: serde_json::Value = serde_json::from_str(body).map_err(|error| {
        VibexError::capability(
            "agent_model_provider_model_fetch_parse_failed",
            "failed to parse provider model list response",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let mut models = Vec::new();
    collect_model_ids_from_value(&value, &mut models);
    models.sort();
    models.dedup();
    Ok(models)
}

fn collect_model_ids_from_value(value: &serde_json::Value, models: &mut Vec<String>) {
    match value {
        serde_json::Value::Array(items) => {
            for item in items {
                collect_model_ids_from_value(item, models);
            }
        }
        serde_json::Value::Object(map) => {
            if let Some(id) = map
                .get("id")
                .or_else(|| map.get("name"))
                .and_then(serde_json::Value::as_str)
                .map(str::trim)
                .filter(|id| !id.is_empty())
                && !models.iter().any(|existing| existing == id)
            {
                models.push(id.to_string());
            }
            if let Some(data) = map
                .get("data")
                .or_else(|| map.get("models"))
                .or_else(|| map.get("availableModels"))
            {
                collect_model_ids_from_value(data, models);
            }
        }
        _ => {}
    }
}

fn resolved_profile_secret_value(profile: &ProviderProfile) -> VibexResult<Option<String>> {
    let secret_kind = editable_profile_secret_kind(profile);
    preferred_editable_profile_secret(profile, secret_kind)
        .map(secrets::resolve_provider_secret)
        .transpose()
        .map(Option::flatten)
}

fn profile_uses_full_api_url(profile: &ProviderProfile) -> bool {
    provider_option_value(&profile.provider_options, "apiRequestFullUrl").as_deref() == Some("true")
}

fn provider_probe_http_client() -> VibexResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(Duration::from_secs(20))
        .user_agent("Vibex/0.1 provider-probe")
        .build()
        .map_err(|error| {
            VibexError::capability(
                "agent_model_provider_probe_client_failed",
                "failed to build provider API probe client",
            )
            .with_diagnostic("error", error.to_string())
        })
}

fn provider_api_endpoint_candidates(
    base_url: &str,
    path: &str,
    full_url: bool,
    prefer_v1: bool,
) -> Vec<String> {
    let base = base_url.trim().trim_end_matches('/');
    if full_url {
        return vec![base.to_string()];
    }
    let path = path.trim_start_matches('/');
    if base.ends_with("/v1") {
        return vec![format!("{base}/{path}")];
    }
    let direct = format!("{base}/{path}");
    let v1 = format!("{base}/v1/{path}");
    if prefer_v1 {
        vec![v1, direct]
    } else {
        vec![direct, v1]
    }
}

fn google_api_endpoint_candidates(base_url: &str, path: &str, full_url: bool) -> Vec<String> {
    let base = base_url.trim().trim_end_matches('/');
    if full_url {
        return vec![base.to_string()];
    }
    let path = path.trim_start_matches('/');
    if base.ends_with("/v1beta") || base.ends_with("/v1") {
        return vec![format!("{base}/{path}")];
    }
    vec![
        format!("{base}/v1beta/{path}"),
        format!("{base}/v1/{path}"),
        format!("{base}/{path}"),
    ]
}

fn encoded_url_path_segment(value: &str) -> VibexResult<String> {
    let mut url = reqwest::Url::parse("https://vibex.invalid/").map_err(|error| {
        VibexError::validation(
            "agent_model_provider_model_url_invalid",
            "failed to prepare the provider model request path",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    url.path_segments_mut()
        .map_err(|_| {
            VibexError::validation(
                "agent_model_provider_model_url_invalid",
                "failed to prepare the provider model request path",
            )
        })?
        .push(value);
    Ok(url.path().trim_start_matches('/').to_string())
}

fn model_list_endpoint_candidates(base_url: &str, full_url: bool, prefer_v1: bool) -> Vec<String> {
    let mut candidates = provider_api_endpoint_candidates(base_url, "models", full_url, prefer_v1);
    if full_url {
        return candidates;
    }

    let trimmed = base_url.trim().trim_end_matches('/');
    const COMPAT_SUFFIXES: &[&str] = &[
        "/api/claudecode",
        "/api/anthropic",
        "/apps/anthropic",
        "/api/coding",
        "/claudecode",
        "/anthropic",
        "/step_plan",
        "/coding",
        "/claude",
    ];
    for suffix in COMPAT_SUFFIXES {
        if let Some(root) = trimmed.strip_suffix(suffix) {
            for candidate in provider_api_endpoint_candidates(root, "models", false, prefer_v1) {
                if !candidates.iter().any(|existing| existing == &candidate) {
                    candidates.push(candidate);
                }
            }
        }
    }
    candidates
}

fn probe_get_json<F>(
    client: &reqwest::blocking::Client,
    endpoints: &[String],
    apply_auth: F,
    success_code: &str,
    success_message: &str,
) -> VibexResult<ProviderApiProbeOutcome>
where
    F: Fn(reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder + Copy,
{
    let mut last_failure = provider_api_probe_fail(
        "agent_model_provider_probe_endpoint_missing",
        "Provider profile did not produce an API endpoint to probe",
        vec![diagnostic("endpoint", "missing")],
    );
    for endpoint in endpoints {
        let started = Instant::now();
        let response = apply_auth(client.get(endpoint).header("accept", "application/json")).send();
        let latency_ms = elapsed_ms(started);
        match response {
            Ok(response) if response.status().is_success() => {
                return Ok(provider_api_probe_pass(
                    success_code,
                    success_message,
                    endpoint,
                    response.status().as_u16(),
                    latency_ms,
                ));
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().unwrap_or_default();
                last_failure = provider_api_probe_http_failure(endpoint, status, body, latency_ms);
                if !matches!(status, 404 | 405) {
                    return Ok(last_failure);
                }
            }
            Err(error) => {
                last_failure = provider_api_probe_transport_failure(endpoint, error, latency_ms);
            }
        }
    }
    Ok(last_failure)
}

fn probe_post_json<F>(
    client: &reqwest::blocking::Client,
    endpoints: &[String],
    body: serde_json::Value,
    apply_auth: F,
    success_code: &str,
    success_message: &str,
) -> VibexResult<ProviderApiProbeOutcome>
where
    F: Fn(reqwest::blocking::RequestBuilder) -> reqwest::blocking::RequestBuilder + Copy,
{
    let mut last_failure = provider_api_probe_fail(
        "agent_model_provider_probe_endpoint_missing",
        "Provider profile did not produce an API endpoint to probe",
        vec![diagnostic("endpoint", "missing")],
    );
    for endpoint in endpoints {
        let started = Instant::now();
        let response = apply_auth(client.post(endpoint).json(&body)).send();
        let latency_ms = elapsed_ms(started);
        match response {
            Ok(response) if response.status().is_success() => {
                return Ok(provider_api_probe_pass(
                    success_code,
                    success_message,
                    endpoint,
                    response.status().as_u16(),
                    latency_ms,
                ));
            }
            Ok(response) => {
                let status = response.status().as_u16();
                let body = response.text().unwrap_or_default();
                last_failure = provider_api_probe_http_failure(endpoint, status, body, latency_ms);
                if !matches!(status, 404 | 405) {
                    return Ok(last_failure);
                }
            }
            Err(error) => {
                last_failure = provider_api_probe_transport_failure(endpoint, error, latency_ms);
            }
        }
    }
    Ok(last_failure)
}

fn provider_api_probe_pass(
    code: &str,
    message: &str,
    endpoint: &str,
    http_status: u16,
    latency_ms: u32,
) -> ProviderApiProbeOutcome {
    ProviderApiProbeOutcome {
        passed: true,
        code: code.to_string(),
        message: message.to_string(),
        latency_ms: Some(latency_ms),
        diagnostics: vec![
            diagnostic("endpoint", redact_url_for_diagnostics(endpoint)),
            diagnostic("httpStatus", http_status.to_string()),
            diagnostic("redacted", "true"),
        ],
    }
}

fn provider_api_probe_http_failure(
    endpoint: &str,
    http_status: u16,
    body: String,
    latency_ms: u32,
) -> ProviderApiProbeOutcome {
    provider_api_probe_fail(
        "agent_model_provider_api_request_failed",
        format!("Provider API request failed with HTTP {http_status}"),
        vec![
            diagnostic("endpoint", redact_url_for_diagnostics(endpoint)),
            diagnostic("httpStatus", http_status.to_string()),
            diagnostic("response", truncate_diagnostic_value(&body)),
            diagnostic("latencyMs", latency_ms.to_string()),
            diagnostic("redacted", "true"),
        ],
    )
}

fn provider_api_probe_transport_failure(
    endpoint: &str,
    error: reqwest::Error,
    latency_ms: u32,
) -> ProviderApiProbeOutcome {
    provider_api_probe_fail(
        "agent_model_provider_api_transport_failed",
        "Provider API request failed before receiving an HTTP response",
        vec![
            diagnostic("endpoint", redact_url_for_diagnostics(endpoint)),
            diagnostic("error", truncate_diagnostic_value(&error.to_string())),
            diagnostic("latencyMs", latency_ms.to_string()),
            diagnostic("redacted", "true"),
        ],
    )
}

fn provider_api_probe_fail(
    code: impl Into<String>,
    message: impl Into<String>,
    diagnostics: Vec<ProviderBindingMetadata>,
) -> ProviderApiProbeOutcome {
    ProviderApiProbeOutcome {
        passed: false,
        code: code.into(),
        message: message.into(),
        latency_ms: None,
        diagnostics,
    }
}

fn elapsed_ms(started: Instant) -> u32 {
    started.elapsed().as_millis().min(u128::from(u32::MAX)) as u32
}

fn truncate_diagnostic_value(value: &str) -> String {
    let value = value.trim();
    const MAX_LEN: usize = 300;
    if value.len() <= MAX_LEN {
        value.to_string()
    } else {
        let end = value
            .char_indices()
            .map(|(index, _)| index)
            .take_while(|index| *index <= MAX_LEN)
            .last()
            .unwrap_or(0);
        format!("{}...", &value[..end])
    }
}

fn provider_health_probe_result(
    profile: &ProviderProfile,
    probe_kind: ProviderHealthProbeKind,
    checked_at_ms: i64,
) -> VibexResult<ProviderHealthProbeResult> {
    let (status, summary, diagnostics, latency_ms) = match (profile.kind, probe_kind) {
        (_, ProviderHealthProbeKind::BinaryExists) | (_, ProviderHealthProbeKind::Version) => (
            ProviderHealthStatus::Skipped,
            "Local binary discovery is not run by default checks".to_string(),
            vec![diagnostic("providerFree", "true")],
            Some(0),
        ),
        (_, ProviderHealthProbeKind::AuthStatus) | (_, ProviderHealthProbeKind::ModelList) => {
            let outcome = run_provider_api_probe(profile, ProviderApiProbeKind::ModelList)?;
            (
                if outcome.passed {
                    ProviderHealthStatus::Pass
                } else {
                    ProviderHealthStatus::Fail
                },
                outcome.message,
                outcome.diagnostics,
                outcome.latency_ms,
            )
        }
        (_, ProviderHealthProbeKind::StreamingFirstByte)
        | (_, ProviderHealthProbeKind::SimplePrompt) => {
            let outcome = run_provider_api_probe(profile, ProviderApiProbeKind::SimplePrompt)?;
            (
                if outcome.passed {
                    ProviderHealthStatus::Pass
                } else {
                    ProviderHealthStatus::Fail
                },
                outcome.message,
                outcome.diagnostics,
                outcome.latency_ms,
            )
        }
    };

    Ok(ProviderHealthProbeResult {
        health_record_id: RequestId::new(),
        provider_profile_id: profile.id.clone(),
        provider_kind: profile.kind,
        probe_kind,
        status,
        summary,
        latency_ms,
        checked_at_ms,
        expires_at_ms: Some(checked_at_ms + 15 * 60 * 1_000),
        diagnostics,
    })
}

fn build_health_summaries(
    profiles: &[ProviderProfile],
    records: &[ProviderHealthProbeResult],
) -> Vec<ProviderHealthSummary> {
    profiles
        .iter()
        .map(|profile| {
            let mut probe_results: Vec<_> = records
                .iter()
                .filter(|record| record.provider_profile_id == profile.id)
                .cloned()
                .collect();
            probe_results.sort_by_key(|record| probe_rank(record.probe_kind));
            let last_checked_at_ms = probe_results
                .iter()
                .map(|record| record.checked_at_ms)
                .max();
            let expires_at_ms = probe_results
                .iter()
                .filter_map(|record| record.expires_at_ms)
                .min();
            ProviderHealthSummary {
                profile: profile.summary(),
                overall_status: overall_health_status(&probe_results),
                last_checked_at_ms,
                expires_at_ms,
                probe_results,
            }
        })
        .collect()
}

fn deterministic_capability_probe_result(
    profile: &ProviderProfile,
    checked_at_ms: i64,
) -> ProviderCapabilityProbeResult {
    let (status, summary, capabilities, source, diagnostics) = match profile.kind {
        ProviderKind::Acp => match acp_config_from_options(&profile.provider_options) {
            Ok(Some(config)) => {
                let capabilities = acp_capabilities_from_config(&config);
                (
                    ProviderCapabilityProbeStatus::Pass,
                    "ACP capabilities projected from typed profile config without starting a provider process".to_string(),
                    capabilities,
                    "acp_profile_config".to_string(),
                    vec![
                        diagnostic("providerFree", "true"),
                        diagnostic("rawProviderPayloadStored", "false"),
                    ],
                )
            }
            Ok(None) => (
                ProviderCapabilityProbeStatus::Fail,
                "ACP profile is missing typed ACP configuration".to_string(),
                ProviderCapabilities::conservative(ProviderKind::Acp, "acp-capability-fallback"),
                "acp_config_missing".to_string(),
                vec![diagnostic("redacted", "true")],
            ),
            Err(error) => (
                ProviderCapabilityProbeStatus::Fail,
                "ACP profile configuration is invalid; conservative capabilities are in effect"
                    .to_string(),
                ProviderCapabilities::conservative(ProviderKind::Acp, "acp-capability-fallback"),
                "acp_config_invalid".to_string(),
                vec![
                    diagnostic("redacted", "true"),
                    diagnostic("errorCode", &error.code),
                ],
            ),
        },
        kind => (
            ProviderCapabilityProbeStatus::Unsupported,
            "Capability probing is currently implemented for ACP provider profiles only"
                .to_string(),
            ProviderCapabilities::conservative(kind, "capability-probe-unsupported"),
            "non_acp_static".to_string(),
            vec![diagnostic("providerFree", "true")],
        ),
    };

    ProviderCapabilityProbeResult {
        capability_record_id: RequestId::new(),
        provider_profile_id: profile.id.clone(),
        provider_kind: profile.kind,
        status,
        summary,
        capabilities,
        source,
        checked_at_ms,
        expires_at_ms: Some(checked_at_ms + 15 * 60 * 1_000),
        diagnostics,
    }
}

fn build_capability_summaries(
    profiles: &[ProviderProfile],
    records: &[ProviderCapabilityProbeResult],
    now_ms: i64,
) -> Vec<ProviderCapabilitySummary> {
    profiles
        .iter()
        .map(|profile| {
            let latest = records
                .iter()
                .find(|record| record.provider_profile_id == profile.id);
            let (status, effective_capabilities, capability_source, fresh, diagnostics) =
                effective_capability_state(profile, latest, now_ms);
            ProviderCapabilitySummary {
                profile: profile.summary(),
                status,
                effective_capabilities,
                capability_source,
                fresh,
                last_checked_at_ms: latest.map(|record| record.checked_at_ms),
                expires_at_ms: latest.and_then(|record| record.expires_at_ms),
                diagnostics,
            }
        })
        .collect()
}

fn effective_capability_state(
    profile: &ProviderProfile,
    latest: Option<&ProviderCapabilityProbeResult>,
    now_ms: i64,
) -> (
    ProviderCapabilityProbeStatus,
    ProviderCapabilities,
    String,
    bool,
    Vec<ProviderBindingMetadata>,
) {
    let fallback_source = if profile.kind == ProviderKind::Acp {
        "acp-foundation-static"
    } else {
        "capability-probe-unsupported"
    };
    let fallback = ProviderCapabilities::conservative(profile.kind, fallback_source);
    let Some(record) = latest else {
        return (
            ProviderCapabilityProbeStatus::Unknown,
            fallback,
            fallback_source.to_string(),
            false,
            vec![diagnostic("probeRequired", "true")],
        );
    };
    let fresh = record
        .expires_at_ms
        .is_none_or(|expires_at_ms| expires_at_ms > now_ms);
    if record.status == ProviderCapabilityProbeStatus::Pass && fresh {
        return (
            record.status,
            record.capabilities.clone(),
            record.source.clone(),
            true,
            record.diagnostics.clone(),
        );
    }
    let status = if !fresh {
        ProviderCapabilityProbeStatus::Stale
    } else {
        record.status
    };
    (
        status,
        fallback,
        fallback_source.to_string(),
        false,
        record.diagnostics.clone(),
    )
}

pub fn acp_capabilities_from_config(config: &AcpProviderConfig) -> ProviderCapabilities {
    let mut capabilities =
        ProviderCapabilities::conservative(ProviderKind::Acp, "acp_profile_config");
    capabilities.model_list = !config.models.is_empty();
    capabilities.dynamic_modes = !config.modes.is_empty();
    capabilities.session_persistence =
        acp_feature_enabled(config, &["session_persistence", "resume"]);
    capabilities.session_listing = acp_feature_enabled(config, &["session_listing", "sessions"]);
    capabilities.streaming = acp_feature_enabled(config, &["streaming", "agent_deltas"]);
    capabilities.mcp_servers = acp_feature_enabled(config, &["mcp_servers", "mcp"]);
    capabilities.slash_commands = acp_feature_enabled(config, &["slash_commands"]);
    capabilities.skills = acp_feature_enabled(config, &["skills"]);
    capabilities.reasoning_stream = acp_feature_enabled(config, &["reasoning", "reasoning_stream"]);
    capabilities.plan = acp_feature_enabled(config, &["plan", "plans"]);
    capabilities.tool_invocations =
        acp_feature_enabled(config, &["tool_calls", "tool_invocations", "tools"])
            && !all_tools_disabled(config);
    capabilities.permission_requests =
        acp_feature_enabled(config, &["permission_requests", "permissions"]);
    // Vibex hosts ACP form elicitation for every profile through the shared
    // runtime callback; this capability is not an agent-specific feature flag.
    capabilities.elicitation = true;
    capabilities.image_input = acp_feature_enabled(config, &["image_input", "images"]);
    capabilities.file_attachments =
        acp_feature_enabled(config, &["file_attachments", "attachments"]);
    capabilities.fork_rollback = acp_feature_enabled(config, &["fork_rollback"]);
    capabilities.interrupt = acp_feature_enabled(config, &["interrupt"]);
    capabilities.terminal_tools = config.terminal_tools
        || acp_feature_enabled(config, &["terminal_tools", "terminal", "terminals"]);
    capabilities.terminal_auth = config.terminal_auth
        || acp_feature_enabled(
            config,
            &["terminal_auth", "auth_terminal", "login_terminal"],
        );
    capabilities.terminal_activity_hooks =
        acp_feature_enabled(config, &["terminal_activity_hooks", "terminal_hooks"]);
    capabilities
}

fn acp_feature_enabled(config: &AcpProviderConfig, aliases: &[&str]) -> bool {
    config.features.iter().any(|feature| {
        let normalized = normalize_capability_token(feature);
        aliases
            .iter()
            .any(|alias| normalized == normalize_capability_token(alias))
    })
}

fn all_tools_disabled(config: &AcpProviderConfig) -> bool {
    config.disabled_tools.iter().any(|tool| {
        let normalized = normalize_capability_token(tool);
        matches!(normalized.as_str(), "*" | "all" | "tools" | "tool_calls")
    })
}

fn normalize_capability_token(value: &str) -> String {
    value.trim().to_ascii_lowercase().replace(['-', ' '], "_")
}

fn overall_health_status(records: &[ProviderHealthProbeResult]) -> ProviderHealthStatus {
    if records.is_empty() {
        return ProviderHealthStatus::Unknown;
    }
    if records
        .iter()
        .any(|record| record.status == ProviderHealthStatus::Fail)
    {
        return ProviderHealthStatus::Fail;
    }
    if records
        .iter()
        .any(|record| record.status == ProviderHealthStatus::Warn)
    {
        return ProviderHealthStatus::Warn;
    }
    if records
        .iter()
        .any(|record| record.status == ProviderHealthStatus::Unknown)
    {
        return ProviderHealthStatus::Unknown;
    }
    if records
        .iter()
        .any(|record| record.status == ProviderHealthStatus::Pass)
    {
        return ProviderHealthStatus::Pass;
    }
    ProviderHealthStatus::Unsupported
}

fn build_usage_summaries(
    profiles: &[&ProviderProfile],
    records: &[ProviderUsageRecord],
    include_empty: bool,
) -> Vec<ProviderUsageSummary> {
    profiles
        .iter()
        .filter_map(|profile| {
            let balances: Vec<_> = records
                .iter()
                .filter(|record| record.provider_profile_id == profile.id)
                .map(|record| ProviderUsageBalance {
                    unit: record.unit,
                    label: record.label.clone(),
                    used: record.used,
                    limit_value: record.limit_value,
                    remaining: record.remaining,
                    window: record.window.clone(),
                    recorded_at_ms: record.recorded_at_ms,
                })
                .collect();
            if balances.is_empty() && !include_empty {
                return None;
            }
            let latest_recorded_at_ms = balances.iter().map(|balance| balance.recorded_at_ms).max();
            Some(ProviderUsageSummary {
                profile: profile.summary(),
                balances,
                latest_recorded_at_ms,
            })
        })
        .collect()
}

fn build_failover_recommendations(
    selected_profiles: &[&ProviderProfile],
    all_profiles: &[ProviderProfile],
    health: &[ProviderHealthSummary],
    usage: &[ProviderUsageSummary],
) -> Vec<ProviderFailoverRecommendation> {
    let health_by_profile: HashMap<_, _> = health
        .iter()
        .map(|summary| (summary.profile.id.as_str(), summary))
        .collect();
    let usage_by_profile: HashMap<_, _> = usage
        .iter()
        .map(|summary| (summary.profile.id.as_str(), summary))
        .collect();
    let now = unix_timestamp_ms();

    selected_profiles
        .iter()
        .map(|profile| {
            let source_health = health_by_profile.get(profile.id.as_str()).copied();
            let source_usage = usage_by_profile.get(profile.id.as_str()).copied();
            let mut reasons = recommendation_reasons(profile, source_health, source_usage, now);

            if reasons.is_empty() {
                return ProviderFailoverRecommendation {
                    recommendation_id: RequestId::new(),
                    source_profile: profile.summary(),
                    candidate_profile: None,
                    status: ProviderFailoverRecommendationStatus::NoAction,
                    reasons,
                    confidence: 0.0,
                    message: "Current profile has no failover recommendation".to_string(),
                    created_at_ms: now,
                };
            }

            let candidate = all_profiles.iter().find(|candidate| {
                candidate.id != profile.id
                    && candidate.kind == profile.kind
                    && candidate.status == ProviderProfileStatus::Enabled
                    && is_better_candidate(
                        health_by_profile.get(candidate.id.as_str()).copied(),
                        usage_by_profile.get(candidate.id.as_str()).copied(),
                    )
            });

            if let Some(candidate) = candidate {
                reasons.push(ProviderFailoverRecommendationReason::CandidateAvailable);
                ProviderFailoverRecommendation {
                    recommendation_id: RequestId::new(),
                    source_profile: profile.summary(),
                    candidate_profile: Some(candidate.summary()),
                    status: ProviderFailoverRecommendationStatus::Recommended,
                    reasons,
                    confidence: 0.72,
                    message: format!(
                        "Consider switching to {} after user review; automatic failover is disabled",
                        candidate.display_name
                    ),
                    created_at_ms: now,
                }
            } else {
                reasons.push(ProviderFailoverRecommendationReason::NoCandidate);
                ProviderFailoverRecommendation {
                    recommendation_id: RequestId::new(),
                    source_profile: profile.summary(),
                    candidate_profile: None,
                    status: ProviderFailoverRecommendationStatus::Blocked,
                    reasons,
                    confidence: 0.25,
                    message: "Profile has risk signals, but no same-kind enabled candidate is ready".to_string(),
                    created_at_ms: now,
                }
            }
        })
        .collect()
}

fn recommendation_reasons(
    profile: &ProviderProfile,
    health: Option<&ProviderHealthSummary>,
    usage: Option<&ProviderUsageSummary>,
    now: i64,
) -> Vec<ProviderFailoverRecommendationReason> {
    let mut reasons = Vec::new();
    if profile.status == ProviderProfileStatus::Disabled {
        reasons.push(ProviderFailoverRecommendationReason::DisabledProfile);
    }
    if health.is_none_or(|summary| summary.overall_status == ProviderHealthStatus::Unknown) {
        reasons.push(ProviderFailoverRecommendationReason::StaleHealth);
    }
    if health.is_some_and(|summary| summary.overall_status == ProviderHealthStatus::Fail) {
        reasons.push(ProviderFailoverRecommendationReason::FailingHealth);
    }
    if health.is_some_and(|summary| {
        summary.probe_results.iter().any(|record| {
            record.probe_kind == ProviderHealthProbeKind::AuthStatus
                && record.status == ProviderHealthStatus::Fail
        })
    }) {
        reasons.push(ProviderFailoverRecommendationReason::MissingAuth);
    }
    if health
        .and_then(|summary| summary.expires_at_ms)
        .is_some_and(|expires| expires < now)
    {
        reasons.push(ProviderFailoverRecommendationReason::StaleHealth);
    }
    if usage.is_some_and(usage_exhausted) {
        reasons.push(ProviderFailoverRecommendationReason::UsageExhausted);
    }
    reasons.sort_by_key(|reason| format!("{reason:?}"));
    reasons.dedup();
    reasons
}

fn is_better_candidate(
    health: Option<&ProviderHealthSummary>,
    usage: Option<&ProviderUsageSummary>,
) -> bool {
    let healthy = health.is_some_and(|summary| {
        matches!(
            summary.overall_status,
            ProviderHealthStatus::Pass | ProviderHealthStatus::Warn
        )
    });
    healthy && !usage.is_some_and(usage_exhausted)
}

fn usage_exhausted(summary: &ProviderUsageSummary) -> bool {
    summary.balances.iter().any(|balance| {
        balance.remaining.is_some_and(|remaining| remaining <= 0.0)
            || balance
                .used
                .zip(balance.limit_value)
                .is_some_and(|(used, limit)| limit > 0.0 && used >= limit)
    })
}

fn probe_rank(kind: ProviderHealthProbeKind) -> u8 {
    match kind {
        ProviderHealthProbeKind::BinaryExists => 0,
        ProviderHealthProbeKind::Version => 1,
        ProviderHealthProbeKind::AuthStatus => 2,
        ProviderHealthProbeKind::ModelList => 3,
        ProviderHealthProbeKind::StreamingFirstByte => 4,
        ProviderHealthProbeKind::SimplePrompt => 5,
    }
}

fn diagnostic(key: impl Into<String>, value: impl Into<String>) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.into(),
        value: value.into(),
    }
}

fn secret_env_fields(profile: &ProviderProfile) -> Vec<ProviderInjectionField> {
    let mut fields: Vec<_> = profile
        .secrets
        .iter()
        .map(|secret| ProviderInjectionField {
            key: secret.lookup_key.clone(),
            value: secret.redacted_hint.clone(),
            secret: true,
            source: format!("{:?}", secret.backend).to_lowercase(),
        })
        .collect();

    if fields.is_empty()
        && let Some((key, label)) = default_secret_env(agent_model_provider_kind(&profile.agent_id))
    {
        fields.push(ProviderInjectionField {
            key: key.to_string(),
            value: format!("<{} placeholder>", label),
            secret: true,
            source: "placeholder".to_string(),
        });
    }
    fields
}

fn acp_cli_fields(config: &AcpProviderConfig) -> Vec<ProviderInjectionField> {
    let mut fields = vec![ProviderInjectionField {
        key: "command".to_string(),
        value: config.command.clone(),
        secret: false,
        source: "acp_config".to_string(),
    }];
    fields.extend(
        config
            .args
            .iter()
            .enumerate()
            .map(|(index, arg)| ProviderInjectionField {
                key: format!("arg[{index}]"),
                value: arg.clone(),
                secret: false,
                source: "acp_config".to_string(),
            }),
    );
    fields
}

fn acp_env_fields(config: &AcpProviderConfig) -> Vec<ProviderInjectionField> {
    config
        .env
        .iter()
        .map(|reference| {
            let (value, secret, source) = match reference.source {
                AcpProviderEnvSource::Literal => (
                    reference.value.clone().unwrap_or_default(),
                    false,
                    "acp_literal",
                ),
                AcpProviderEnvSource::ProcessEnvironment => {
                    ("<process environment>".to_string(), true, "acp_process_env")
                }
                AcpProviderEnvSource::SecretReference => (
                    if reference.redacted_hint.trim().is_empty() {
                        "<secret reference>".to_string()
                    } else {
                        reference.redacted_hint.clone()
                    },
                    true,
                    "acp_secret_reference",
                ),
            };
            ProviderInjectionField {
                key: reference.key.clone(),
                value,
                secret,
                source: source.to_string(),
            }
        })
        .collect()
}

fn provider_overlay_preview(
    profile: &ProviderProfile,
    acp_config: Option<&AcpProviderConfig>,
) -> String {
    if let Some(config) = acp_config {
        return serde_json::json!({
            "provider": profile.kind.to_string(),
            "profileId": profile.id.as_str(),
            "acp": {
                "command": config.command,
                "args": config.args.len(),
                "cwdTemplate": config.cwd_template,
                "env": "redacted",
            }
        })
        .to_string();
    }
    serde_json::json!({
        "provider": profile.kind.to_string(),
        "profileId": profile.id.as_str(),
        "secrets": "redacted",
    })
    .to_string()
}

fn default_secret_env(kind: ProviderKind) -> Option<(&'static str, &'static str)> {
    match kind {
        ProviderKind::Codex => Some(("OPENAI_API_KEY", "missing api key")),
        ProviderKind::Claude => Some(("ANTHROPIC_API_KEY", "missing auth token")),
        ProviderKind::Acp => None,
    }
}

fn default_endpoint(kind: ProviderKind) -> Option<String> {
    match kind {
        ProviderKind::Codex => Some("https://api.openai.invalid/v1".to_string()),
        ProviderKind::Claude => Some("https://api.anthropic.invalid".to_string()),
        ProviderKind::Acp => None,
    }
}

fn push_optional_field(
    fields: &mut Vec<ProviderInjectionField>,
    key: &str,
    value: Option<String>,
    secret: bool,
    source: &str,
) {
    if let Some(value) = value {
        fields.push(ProviderInjectionField {
            key: key.to_string(),
            value,
            secret,
            source: source.to_string(),
        });
    }
}

fn normalize_mcp_create_request(mut request: McpServerCreateRequest) -> McpServerCreateRequest {
    let now = unix_timestamp_ms();
    request.provider_matrix = request
        .provider_matrix
        .into_iter()
        .map(|entry| McpServerProviderMatrix {
            provider_kind: entry.provider_kind,
            enabled: entry.enabled,
            updated_at_ms: now,
        })
        .collect();
    request
}

fn validate_mcp_create_request(request: &McpServerCreateRequest) -> VibexResult<()> {
    validate_mcp_display_name(&request.display_name)?;
    match request.transport_kind {
        McpServerTransportKind::Stdio => {
            if request
                .command
                .as_deref()
                .is_none_or(|command| command.trim().is_empty())
            {
                return Err(VibexError::validation(
                    "mcp_server_stdio_command_missing",
                    "stdio MCP servers require a command",
                ));
            }
        }
        McpServerTransportKind::Http | McpServerTransportKind::Sse => {
            validate_mcp_url(request.url.as_deref())?;
        }
    }
    Ok(())
}

fn validate_mcp_server_record(server: &McpServer) -> VibexResult<()> {
    validate_mcp_display_name(&server.display_name)?;
    match server.transport_kind {
        McpServerTransportKind::Stdio => {
            if server
                .command
                .as_deref()
                .is_none_or(|command| command.trim().is_empty())
            {
                return Err(VibexError::validation(
                    "mcp_server_stdio_command_missing",
                    "stdio MCP servers require a command",
                ));
            }
        }
        McpServerTransportKind::Http | McpServerTransportKind::Sse => {
            validate_mcp_url(server.url.as_deref())?;
        }
    }
    Ok(())
}

fn validate_mcp_server_result(server: &McpServer) -> McpServerValidationResult {
    let checked_at_ms = unix_timestamp_ms();
    if server.display_name.trim().is_empty() {
        return mcp_validation_result(
            McpServerValidationStatus::Fail,
            "mcp_server_name_empty",
            "MCP server display name must not be empty",
            checked_at_ms,
        );
    }
    match server.transport_kind {
        McpServerTransportKind::Stdio
            if server
                .command
                .as_deref()
                .is_none_or(|command| command.trim().is_empty()) =>
        {
            mcp_validation_result(
                McpServerValidationStatus::Fail,
                "mcp_server_stdio_command_missing",
                "stdio MCP server command is missing; no process was started",
                checked_at_ms,
            )
        }
        McpServerTransportKind::Http | McpServerTransportKind::Sse
            if !mcp_url_shape_is_valid(server.url.as_deref()) =>
        {
            mcp_validation_result(
                McpServerValidationStatus::Fail,
                "mcp_server_url_invalid",
                "HTTP/SSE MCP server URL must start with http:// or https://; no network request was made",
                checked_at_ms,
            )
        }
        _ => {
            let mut result = mcp_validation_result(
                McpServerValidationStatus::Pass,
                "mcp_server_validation_passed",
                "MCP server metadata passed deterministic local validation; no process or network was used",
                checked_at_ms,
            );
            result
                .diagnostics
                .push(diagnostic("noProcessOrNetwork", "true"));
            result
        }
    }
}

fn mcp_validation_result(
    status: McpServerValidationStatus,
    code: impl Into<String>,
    message: impl Into<String>,
    checked_at_ms: i64,
) -> McpServerValidationResult {
    McpServerValidationResult {
        status,
        code: code.into(),
        message: message.into(),
        diagnostics: vec![diagnostic("deterministic", "true")],
        checked_at_ms,
    }
}

fn validate_mcp_display_name(display_name: &str) -> VibexResult<()> {
    if display_name.trim().is_empty() {
        return Err(VibexError::validation(
            "mcp_server_name_empty",
            "MCP server display name must not be empty",
        ));
    }
    Ok(())
}

fn validate_mcp_url(url: Option<&str>) -> VibexResult<()> {
    if mcp_url_shape_is_valid(url) {
        return Ok(());
    }
    Err(VibexError::validation(
        "mcp_server_url_invalid",
        "HTTP/SSE MCP servers require an http:// or https:// URL",
    ))
}

fn mcp_url_shape_is_valid(url: Option<&str>) -> bool {
    let Some(url) = url.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    let rest = url
        .strip_prefix("http://")
        .or_else(|| url.strip_prefix("https://"));
    rest.is_some_and(|value| !value.trim_matches('/').is_empty() && !value.contains(' '))
}

fn format_mcp_preview_entry(server: &McpServer, provider_kind: ProviderKind) -> String {
    let secret_refs = if server.secret_references.is_empty() {
        "no secret references".to_string()
    } else {
        server
            .secret_references
            .iter()
            .map(|secret| {
                format!(
                    "{}:{}={}",
                    format!("{:?}", secret.target).to_lowercase(),
                    secret.lookup_key,
                    secret.redacted_hint
                )
            })
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!(
        "{} ({}) -> {}; {}",
        server.display_name, server.transport_kind, provider_kind, secret_refs
    )
}

fn normalize_skill_create_request(mut request: SkillCreateRequest) -> SkillCreateRequest {
    let now = unix_timestamp_ms();
    request.provider_matrix = request
        .provider_matrix
        .into_iter()
        .map(|entry| SkillProviderMatrix {
            provider_kind: entry.provider_kind,
            enabled: entry.enabled,
            updated_at_ms: now,
        })
        .collect();
    request
}

fn validate_skill_create_request(request: &SkillCreateRequest) -> VibexResult<()> {
    validate_skill_display_name(&request.display_name)?;
    validate_skill_metadata(
        request.source_kind,
        request.source_uri.as_deref(),
        request.description.as_deref(),
        request.content_preview.as_deref(),
    )
}

fn validate_skill_record(skill: &Skill) -> VibexResult<()> {
    validate_skill_display_name(&skill.display_name)?;
    validate_skill_metadata(
        skill.source_kind,
        skill.source_uri.as_deref(),
        skill.description.as_deref(),
        skill.content_preview.as_deref(),
    )
}

fn validate_skill_metadata(
    source_kind: SkillSourceKind,
    source_uri: Option<&str>,
    description: Option<&str>,
    content_preview: Option<&str>,
) -> VibexResult<()> {
    if source_kind == SkillSourceKind::Manual
        && description.is_none_or(|value| value.trim().is_empty())
        && content_preview.is_none_or(|value| value.trim().is_empty())
    {
        return Err(VibexError::validation(
            "skill_manual_content_missing",
            "manual Skills require a description or content preview",
        ));
    }
    if source_kind != SkillSourceKind::Manual && !skill_source_uri_shape_is_valid(source_uri) {
        return Err(VibexError::validation(
            "skill_source_uri_invalid",
            "Skill source URI shape is invalid; no network or filesystem read was used",
        ));
    }
    Ok(())
}

fn validate_skill_result(skill: &Skill) -> SkillValidationResult {
    let checked_at_ms = unix_timestamp_ms();
    if skill.display_name.trim().is_empty() {
        return skill_validation_result(
            SkillValidationStatus::Fail,
            "skill_name_empty",
            "Skill display name must not be empty",
            checked_at_ms,
        );
    }
    if skill.source_kind == SkillSourceKind::Manual
        && skill
            .description
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
        && skill
            .content_preview
            .as_deref()
            .is_none_or(|value| value.trim().is_empty())
    {
        return skill_validation_result(
            SkillValidationStatus::Fail,
            "skill_manual_content_missing",
            "Manual Skill is missing description/content preview; no native file was written",
            checked_at_ms,
        );
    }
    if skill.source_kind != SkillSourceKind::Manual
        && !skill_source_uri_shape_is_valid(skill.source_uri.as_deref())
    {
        return skill_validation_result(
            SkillValidationStatus::Fail,
            "skill_source_uri_invalid",
            "Skill source URI must be metadata-shaped; no fetch, clone, or folder scan was run",
            checked_at_ms,
        );
    }
    let mut result = skill_validation_result(
        SkillValidationStatus::Pass,
        "skill_validation_passed",
        "Skill metadata passed deterministic local validation; no network, clone, folder read, or native write was used",
        checked_at_ms,
    );
    result
        .diagnostics
        .push(diagnostic("noNetworkOrNativeWrite", "true"));
    result
}

fn skill_validation_result(
    status: SkillValidationStatus,
    code: impl Into<String>,
    message: impl Into<String>,
    checked_at_ms: i64,
) -> SkillValidationResult {
    SkillValidationResult {
        status,
        code: code.into(),
        message: message.into(),
        diagnostics: vec![diagnostic("deterministic", "true")],
        checked_at_ms,
    }
}

fn validate_skill_display_name(display_name: &str) -> VibexResult<()> {
    if display_name.trim().is_empty() {
        return Err(VibexError::validation(
            "skill_name_empty",
            "Skill display name must not be empty",
        ));
    }
    Ok(())
}

fn skill_source_uri_shape_is_valid(source_uri: Option<&str>) -> bool {
    let Some(source_uri) = source_uri.map(str::trim).filter(|value| !value.is_empty()) else {
        return false;
    };
    if source_uri.contains('\n') || source_uri.contains('\r') {
        return false;
    }
    source_uri.starts_with("https://")
        || source_uri.starts_with("http://")
        || source_uri.starts_with("git@")
        || source_uri.starts_with("file://")
        || source_uri.starts_with("/")
        || source_uri.starts_with("./")
        || source_uri.starts_with("../")
        || source_uri.contains('/')
}

fn validate_prompt_create_request(request: &PromptCreateRequest) -> VibexResult<()> {
    validate_prompt_display_name(&request.display_name)?;
    validate_prompt_body(&request.body)
}

fn validate_prompt_record(prompt: &Prompt) -> VibexResult<()> {
    validate_prompt_display_name(&prompt.display_name)?;
    validate_prompt_body(&prompt.body)
}

fn validate_prompt_result(prompt: &Prompt) -> PromptValidationResult {
    let checked_at_ms = unix_timestamp_ms();
    if prompt.display_name.trim().is_empty() {
        return prompt_validation_result(
            PromptValidationStatus::Fail,
            "prompt_name_empty",
            "Prompt display name must not be empty",
            checked_at_ms,
        );
    }
    if prompt.body.trim().is_empty() {
        return prompt_validation_result(
            PromptValidationStatus::Fail,
            "prompt_body_empty",
            "Prompt body must not be empty",
            checked_at_ms,
        );
    }
    let mut result = prompt_validation_result(
        PromptValidationStatus::Pass,
        "prompt_validation_passed",
        "Prompt metadata passed deterministic local validation",
        checked_at_ms,
    );
    result
        .diagnostics
        .push(diagnostic("noNativePromptWrite", "true"));
    result
}

fn prompt_validation_result(
    status: PromptValidationStatus,
    code: impl Into<String>,
    message: impl Into<String>,
    checked_at_ms: i64,
) -> PromptValidationResult {
    PromptValidationResult {
        status,
        code: code.into(),
        message: message.into(),
        diagnostics: vec![diagnostic("deterministic", "true")],
        checked_at_ms,
    }
}

fn validate_prompt_display_name(display_name: &str) -> VibexResult<()> {
    if display_name.trim().is_empty() {
        return Err(VibexError::validation(
            "prompt_name_empty",
            "Prompt display name must not be empty",
        ));
    }
    Ok(())
}

fn validate_prompt_body(body: &str) -> VibexResult<()> {
    if body.trim().is_empty() {
        return Err(VibexError::validation(
            "prompt_body_empty",
            "Prompt body must not be empty",
        ));
    }
    Ok(())
}

fn validate_hook_display_name(display_name: &str) -> VibexResult<()> {
    if display_name.trim().is_empty() {
        return Err(VibexError::validation(
            "hook_name_empty",
            "Hook display name must not be empty",
        ));
    }
    Ok(())
}

fn build_hook_install_preview(hook: &Hook, target_path: Option<String>) -> HookInstallPreview {
    let target_path = target_path.unwrap_or_else(|| match hook.provider_kind {
        ProviderKind::Claude => "~/.claude/settings.json".to_string(),
        ProviderKind::Codex => "~/.codex/config.toml".to_string(),
        ProviderKind::Acp => "~/.vibex/acp-hooks.json".to_string(),
    });
    let marker = hook.managed_marker.clone();
    HookInstallPreview {
        preview_id: RequestId::new(),
        hook_id: hook.id.clone(),
        target_path: target_path.clone(),
        marker: marker.clone(),
        redacted_preview: format!(
            "preview only for {} {} hook at {}; future install is marker-based and removes only {} blocks; command={}",
            hook.provider_kind,
            format!("{:?}", hook.event_kind).to_lowercase(),
            target_path,
            marker,
            hook.command_preview
                .as_deref()
                .unwrap_or("<not configured>")
        ),
        created_at_ms: unix_timestamp_ms(),
    }
}

fn format_skill_preview_entry(skill: &Skill, provider_kind: ProviderKind) -> String {
    format!(
        "Skill: {} ({:?}) -> {}; scope={:?}",
        skill.display_name, skill.source_kind, provider_kind, skill.scope_kind
    )
}

fn format_prompt_preview_entry(prompt: &Prompt) -> String {
    format!(
        "Prompt: {} ({:?}); scope={:?}; content stored in Vibex",
        prompt.display_name, prompt.kind, prompt.scope_kind
    )
}

fn validate_display_name(display_name: &str) -> VibexResult<()> {
    if display_name.trim().is_empty() {
        return Err(VibexError::validation(
            "provider_profile_name_empty",
            "provider profile display name must not be empty",
        ));
    }
    Ok(())
}

fn require_agent_definition(agent_id: &AgentId) -> VibexResult<AgentDefinition> {
    builtin_agent_definitions()
        .into_iter()
        .find(|definition| definition.id == *agent_id)
        .ok_or_else(|| {
            VibexError::validation("agent_not_found", "Agent was not found")
                .with_diagnostic("agentId", agent_id.as_str())
        })
}

fn validate_profile_agent_kind(
    agent_id: Option<&AgentId>,
    provider_kind: ProviderKind,
) -> VibexResult<()> {
    let Some(agent_id) = agent_id else {
        return Ok(());
    };
    require_agent_definition(agent_id)?;
    let configuration_kind = agent_model_provider_kind(agent_id);
    if provider_kind != ProviderKind::Acp && provider_kind != configuration_kind {
        return Err(VibexError::validation(
            "provider_profile_kind_mismatch",
            "provider profile kind is not supported by the Agent configuration",
        )
        .with_diagnostic("agentId", agent_id.as_str())
        .with_diagnostic("providerKind", provider_kind.to_string())
        .with_diagnostic("configurationProviderKind", configuration_kind.to_string()));
    }
    Ok(())
}

fn agent_configuration_provider_kind(_agent_id: &AgentId) -> ProviderKind {
    ProviderKind::Acp
}

fn agent_model_provider_kind(agent_id: &AgentId) -> ProviderKind {
    match agent_id.as_str() {
        "claude" => ProviderKind::Claude,
        "codex" => ProviderKind::Codex,
        _ => ProviderKind::Acp,
    }
}

fn require_agent_profile(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
    provider_profile_id: &ProviderProfileId,
) -> VibexResult<ProviderProfile> {
    require_agent_definition(agent_id)?;
    let profile = ProviderProfileRepository::get(conn, provider_profile_id)?.ok_or_else(|| {
        VibexError::validation(
            "provider_profile_not_found",
            "provider profile was not found",
        )
        .with_diagnostic("providerProfileId", provider_profile_id.as_str())
    })?;
    if profile.agent_id != *agent_id {
        return Err(VibexError::validation(
            "provider_profile_agent_mismatch",
            "provider profile belongs to another agent",
        )
        .with_diagnostic("agentId", agent_id.as_str())
        .with_diagnostic("profileAgentId", profile.agent_id.as_str())
        .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
    }
    Ok(profile)
}

fn editable_profile_secret_kind(profile: &ProviderProfile) -> ProviderSecretKind {
    profile
        .secrets
        .iter()
        .find(|secret| secret.backend != ProviderSecretBackend::Placeholder)
        .or_else(|| profile.secrets.first())
        .map(|secret| secret.secret_kind)
        .unwrap_or_else(|| {
            default_editable_secret_kind(agent_model_provider_kind(&profile.agent_id))
        })
}

fn default_editable_secret_kind(kind: ProviderKind) -> ProviderSecretKind {
    match kind {
        ProviderKind::Codex => ProviderSecretKind::ApiKey,
        ProviderKind::Claude | ProviderKind::Acp => ProviderSecretKind::AuthToken,
    }
}

fn editable_profile_secret_display_label(
    profile: &ProviderProfile,
    secret_kind: ProviderSecretKind,
) -> String {
    profile
        .secrets
        .iter()
        .find(|secret| secret.secret_kind == secret_kind && !secret.display_label.trim().is_empty())
        .map(|secret| secret.display_label.clone())
        .or_else(|| {
            if agent_model_provider_kind(&profile.agent_id) == ProviderKind::Codex {
                provider_option_value(&profile.provider_options, CODEX_API_KEY_ENV_OPTION_KEY)
            } else {
                None
            }
        })
        .or_else(|| {
            default_editable_secret_label(agent_model_provider_kind(&profile.agent_id))
                .map(ToOwned::to_owned)
        })
        .unwrap_or_else(|| "API Key".to_string())
}

fn default_editable_secret_label(kind: ProviderKind) -> Option<&'static str> {
    match kind {
        ProviderKind::Codex => Some("OPENAI_API_KEY"),
        ProviderKind::Claude => Some("ANTHROPIC_API_KEY"),
        ProviderKind::Acp => Some("OPENCODE_AUTH_TOKEN"),
    }
}

fn preferred_editable_profile_secret(
    profile: &ProviderProfile,
    secret_kind: ProviderSecretKind,
) -> Option<&ProviderSecretReference> {
    profile
        .secrets
        .iter()
        .find(|secret| {
            secret.secret_kind == secret_kind
                && secret.backend != ProviderSecretBackend::Placeholder
        })
        .or_else(|| {
            profile
                .secrets
                .iter()
                .find(|secret| secret.secret_kind == secret_kind)
        })
}

fn build_agent_model_provider_secret_value_response(
    agent_id: AgentId,
    profile: &ProviderProfile,
) -> VibexResult<AgentModelProviderProfileSecretValueResponse> {
    let secret_kind = editable_profile_secret_kind(profile);
    let display_label = editable_profile_secret_display_label(profile, secret_kind);
    let reference = preferred_editable_profile_secret(profile, secret_kind);
    let value = reference
        .map(secrets::resolve_provider_secret)
        .transpose()?
        .flatten();

    Ok(AgentModelProviderProfileSecretValueResponse {
        agent_id,
        provider_profile_id: profile.id.clone(),
        secret_kind,
        backend: reference
            .map(|secret| secret.backend)
            .unwrap_or(ProviderSecretBackend::Placeholder),
        setup_state: reference
            .map(|secret| secret.setup_state)
            .unwrap_or(ProviderSecretSetupState::Missing),
        lookup_key: reference.map(|secret| secret.lookup_key.clone()),
        display_label,
        redacted_hint: reference
            .map(|secret| secret.redacted_hint.clone())
            .unwrap_or_else(|| "not configured".to_string()),
        value,
        updated_at_ms: reference
            .map(|secret| secret.updated_at_ms)
            .unwrap_or(profile.updated_at_ms),
    })
}

fn require_failover_agent_profile(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
    provider_profile_id: &ProviderProfileId,
) -> VibexResult<ProviderProfile> {
    require_agent_definition(agent_id)?;
    let profile = ProviderProfileRepository::get(conn, provider_profile_id)?.ok_or_else(|| {
        VibexError::validation(
            "provider_profile_not_found",
            "provider profile was not found",
        )
        .with_diagnostic("providerProfileId", provider_profile_id.as_str())
    })?;
    if profile.agent_id != *agent_id {
        return Err(VibexError::validation(
            "failover_profile_agent_mismatch",
            "failover candidate belongs to another agent",
        )
        .with_diagnostic("agentId", agent_id.as_str())
        .with_diagnostic("profileAgentId", profile.agent_id.as_str())
        .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
    }
    Ok(profile)
}

fn visible_model_provider_profiles(
    conn: &vibex_db::DbConnection,
    profiles: Vec<ProviderProfile>,
) -> VibexResult<Vec<ProviderProfile>> {
    Ok(profiles
        .into_iter()
        .filter(|profile| !is_internal_provider_profile(conn, profile))
        .collect())
}

fn is_internal_provider_profile(conn: &vibex_db::DbConnection, profile: &ProviderProfile) -> bool {
    is_local_default_profile(&profile.id)
        || provider_option_value(&profile.provider_options, INTERNAL_PROFILE_ROLE_OPTION_KEY)
            .as_deref()
            == Some(INTERNAL_AGENT_RUNTIME_PROFILE_ROLE)
        || is_legacy_seeded_agent_runtime_profile(conn, profile)
}

fn is_legacy_seeded_agent_runtime_profile(
    conn: &vibex_db::DbConnection,
    profile: &ProviderProfile,
) -> bool {
    if profile.kind != ProviderKind::Acp
        || profile.status != ProviderProfileStatus::Enabled
        || profile.account_alias.is_some()
        || profile.base_url.is_some()
        || profile.default_model.is_some()
        || !profile.configured_models.is_empty()
        || !profile.secrets.is_empty()
        || profile.provider_options.entries.len() != 1
        || profile.provider_options.entries[0].key.trim() != ACP_CONFIG_OPTION_KEY
    {
        return false;
    }
    let Ok(definition) = require_agent_definition(&profile.agent_id) else {
        return false;
    };
    profile.display_name == format!("{} ACP", definition.label)
        && acp_config_from_options(&profile.provider_options)
            .ok()
            .flatten()
            .is_some_and(|config| {
                default_acp_runtime_config_for_agent(conn, &profile.agent_id)
                    .is_ok_and(|default| same_acp_runtime_command(&config, &default))
            })
}

fn same_acp_runtime_command(left: &AcpProviderConfig, right: &AcpProviderConfig) -> bool {
    left.command == right.command && left.args == right.args
}

fn build_agent_model_provider_profiles(
    agent_id: AgentId,
    profiles: Vec<ProviderProfile>,
    default_profile_id: Option<&ProviderProfileId>,
    failover: &[AgentModelProviderFailoverEntry],
    display_order: &[AgentModelProviderDisplayOrderEntry],
) -> Vec<AgentModelProviderProfile> {
    let failover_by_profile = failover
        .iter()
        .map(|entry| (entry.provider_profile_id.clone(), entry))
        .collect::<HashMap<_, _>>();
    let display_order_by_profile = display_order
        .iter()
        .map(|entry| (entry.provider_profile_id.clone(), entry.order_index))
        .collect::<HashMap<_, _>>();
    let mut profiles = profiles
        .into_iter()
        .filter(|profile| profile.agent_id == agent_id)
        .map(|profile| {
            let failover_entry = failover_by_profile.get(&profile.id);
            let display_order_index = display_order_by_profile.get(&profile.id).copied();
            AgentModelProviderProfile {
                is_default: default_profile_id.is_some_and(|id| id == &profile.id),
                failover_order_index: failover_entry.map(|entry| entry.order_index),
                in_failover_queue: failover_entry.is_some_and(|entry| entry.enabled),
                display_order_index,
                profile,
            }
        })
        .collect::<Vec<_>>();
    profiles.sort_by_key(|entry| {
        (
            entry.display_order_index.is_none(),
            entry.display_order_index.unwrap_or(i64::MAX),
            std::cmp::Reverse(entry.profile.updated_at_ms),
            entry.profile.id.as_str().to_string(),
        )
    });
    profiles
}

fn global_default_scope() -> ProviderProfileDefaultScope {
    ProviderProfileDefaultScope {
        kind: ProviderDefaultScopeKind::Global,
        project_id: None,
        workspace_id: None,
    }
}

fn redact_url_for_diagnostics(value: &str) -> String {
    let Some((scheme, rest)) = value.split_once("://") else {
        return "<redacted-url>".to_string();
    };
    let host = rest.split('/').next().unwrap_or_default();
    if host.is_empty() {
        format!("{scheme}://<redacted>")
    } else {
        format!("{scheme}://{host}/...")
    }
}

fn validate_agent_label(label: &str) -> VibexResult<()> {
    let label = label.trim();
    if label.is_empty() {
        return Err(VibexError::validation(
            "agent_label_empty",
            "agent label must not be empty",
        ));
    }
    if label.len() > 80 {
        return Err(VibexError::validation(
            "agent_label_too_long",
            "agent label must be 80 characters or fewer",
        ));
    }
    Ok(())
}

fn validate_default_scope(scope: &ProviderProfileDefaultScope) -> VibexResult<()> {
    match scope.kind {
        ProviderDefaultScopeKind::Global => Ok(()),
        ProviderDefaultScopeKind::Project if scope.project_id.is_some() => Ok(()),
        ProviderDefaultScopeKind::Workspace if scope.workspace_id.is_some() => Ok(()),
        ProviderDefaultScopeKind::Project => Err(VibexError::validation(
            "provider_default_project_missing",
            "project provider default requires projectId",
        )),
        ProviderDefaultScopeKind::Workspace => Err(VibexError::validation(
            "provider_default_workspace_missing",
            "workspace provider default requires workspaceId",
        )),
    }
}

fn is_local_default_profile(provider_profile_id: &ProviderProfileId) -> bool {
    [ProviderKind::Codex, ProviderKind::Claude, ProviderKind::Acp]
        .iter()
        .any(|kind| provider_profile_id.as_str() == kind.local_default_profile_id())
}

pub fn placeholder_secret(
    secret_kind: ProviderSecretKind,
    lookup_key: impl Into<String>,
    display_label: impl Into<String>,
) -> ProviderSecretReferenceCreateRequest {
    ProviderSecretReferenceCreateRequest {
        secret_kind,
        backend: ProviderSecretBackend::Placeholder,
        setup_state: ProviderSecretSetupState::Missing,
        lookup_key: lookup_key.into(),
        display_label: display_label.into(),
        redacted_hint: "not configured".to_string(),
    }
}

pub fn option_entry(key: impl Into<String>, value: impl Into<String>) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.into(),
        value: value.into(),
    }
}

#[cfg(test)]
mod tests {
    use std::io::Read;
    use std::sync::{Arc, Mutex, OnceLock};

    use tempfile::tempdir;

    #[test]
    #[ignore = "temp real-db verify"]
    fn temp_verify_real_db_list() {
        let Ok(db_path) = std::env::var("VIBEX_REAL_DB_COPY") else {
            return;
        };
        let service = ProviderConfigService::new(std::path::PathBuf::from(db_path));
        let listed = service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap();
        for agent in listed.agents {
            eprintln!(
                "LIST agent={} enabled={} install={:?}",
                agent.id, agent.enabled, agent.install_status
            );
        }
    }

    #[test]
    fn enabling_acp_catalog_agents_seeds_hidden_runtime_profiles() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        for agent_id in [
            "gemini",
            "glm-acp-agent",
            "copilot",
            "qwen-code",
            "goose",
            "opencode",
        ] {
            let snapshot = service
                .update_agent_config(AgentUpdateConfigRequest {
                    agent_id: AgentId::parse(agent_id).unwrap(),
                    added: Some(true),
                    enabled: Some(true),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                })
                .unwrap_or_else(|err| panic!("enable {agent_id} failed: {err:?}"));
            assert!(snapshot.enabled, "{agent_id} should be enabled");

            let visible_profiles = service
                .list_agent_model_provider_profiles(AgentModelProviderProfileListRequest {
                    agent_id: AgentId::parse(agent_id).unwrap(),
                    include_disabled: true,
                })
                .unwrap();
            let listed = service
                .list_agents(AgentListRequest {
                    include_disabled: true,
                })
                .unwrap();
            let listed_agent = listed
                .agents
                .iter()
                .find(|agent| agent.id.as_str() == agent_id)
                .unwrap_or_else(|| panic!("{agent_id} missing from agent list"));
            assert!(
                listed_agent.enabled,
                "{agent_id} must be listed as enabled after the toggle"
            );

            assert!(
                visible_profiles.profiles.is_empty(),
                "{agent_id} runtime bootstrap must not appear as a model provider"
            );
            let seeded = service
                .list_runtime_profiles()
                .unwrap()
                .into_iter()
                .filter(|profile| {
                    profile.agent_id.as_str() == agent_id
                        && provider_option_value(
                            &profile.provider_options,
                            INTERNAL_PROFILE_ROLE_OPTION_KEY,
                        )
                        .as_deref()
                            == Some(INTERNAL_AGENT_RUNTIME_PROFILE_ROLE)
                })
                .collect::<Vec<_>>();
            assert_eq!(
                seeded.len(),
                1,
                "{agent_id} should have one internal runtime profile"
            );
            let profile = &seeded[0];
            assert_eq!(profile.kind, ProviderKind::Acp);
            assert_eq!(profile.status, ProviderProfileStatus::Enabled);
            assert!(
                service
                    .list_profiles()
                    .unwrap()
                    .iter()
                    .all(|visible| visible.id != profile.id),
                "{agent_id} runtime profile must stay out of the global model-provider list"
            );
            let config = service.get_acp_profile_config(profile.id.clone()).unwrap();
            assert!(
                !config.command.trim().is_empty(),
                "{agent_id} profile must have a command"
            );
            if agent_id == "glm-acp-agent" {
                assert_eq!(config.command, "npx");
                assert_eq!(config.args, ["-y", "glm-acp-agent@1.1.4"]);
            }
        }
    }

    #[test]
    fn agent_definitions_and_acp_presets_stay_synchronized() {
        let definition_presets = builtin_agent_definitions()
            .into_iter()
            .map(|definition| {
                definition
                    .params
                    .get("preset")
                    .and_then(serde_json::Value::as_str)
                    .unwrap_or_else(|| panic!("{} is missing an ACP preset", definition.id))
                    .to_string()
            })
            .collect::<std::collections::BTreeSet<_>>();
        let catalog_presets = bundled_acp_catalog_presets()
            .into_iter()
            .map(|preset| preset.preset_id)
            .collect::<std::collections::BTreeSet<_>>();

        assert_eq!(
            definition_presets.len(),
            acp_agent_catalog_entries().len() + 3
        );
        assert_eq!(definition_presets, catalog_presets);
    }

    #[test]
    fn generic_acp_presets_preserve_commands_and_literal_environment() {
        let presets = bundled_acp_catalog_presets();
        for (preset_id, command, args) in [
            ("glm-acp-agent", "npx", &["-y", "glm-acp-agent@1.1.4"][..]),
            (
                "gemini",
                "npx",
                &["-y", "@google/gemini-cli@0.47.0", "--acp"][..],
            ),
            (
                "qwen-code",
                "npx",
                &[
                    "-y",
                    "@qwen-code/qwen-code@0.18.4",
                    "--acp",
                    "--experimental-skills",
                ][..],
            ),
            ("cursor", "cursor-agent", &["acp"][..]),
            ("kimi-cli", "kimi", &["acp"][..]),
        ] {
            let config = &presets
                .iter()
                .find(|preset| preset.preset_id == preset_id)
                .unwrap_or_else(|| panic!("missing ACP preset {preset_id}"))
                .default_config;
            assert_eq!(config.command, command);
            assert_eq!(config.args, args);
            assert_eq!(config.process_strategy, AcpProcessStrategy::PerSession);
            assert_eq!(config.cwd_template.as_deref(), Some("{workspaceRoot}"));
        }

        let auggie = presets
            .iter()
            .find(|preset| preset.preset_id == "auggie")
            .unwrap();
        assert_eq!(
            auggie
                .default_config
                .env
                .iter()
                .map(|reference| (reference.key.as_str(), reference.value.as_deref()))
                .collect::<Vec<_>>(),
            [("AUGMENT_DISABLE_AUTO_UPDATE", Some("1"))]
        );

        let factory = presets
            .iter()
            .find(|preset| preset.preset_id == "factory-droid")
            .unwrap();
        let factory_env = factory
            .default_config
            .env
            .iter()
            .map(|reference| {
                (
                    reference.key.as_str(),
                    reference.source,
                    reference.value.as_deref(),
                )
            })
            .collect::<Vec<_>>();
        assert_eq!(
            factory_env,
            [
                (
                    "DROID_DISABLE_AUTO_UPDATE",
                    AcpProviderEnvSource::Literal,
                    Some("true")
                ),
                (
                    "FACTORY_DROID_AUTO_UPDATE_ENABLED",
                    AcpProviderEnvSource::Literal,
                    Some("false")
                ),
            ]
        );

        let vtcode = presets
            .iter()
            .find(|preset| preset.preset_id == "vtcode")
            .unwrap();
        assert_eq!(
            vtcode
                .default_config
                .env
                .iter()
                .map(|reference| (reference.key.as_str(), reference.value.as_deref()))
                .collect::<Vec<_>>(),
            [
                ("VT_ACP_ENABLED", Some("1")),
                ("VT_ACP_ZED_ENABLED", Some("1")),
            ]
        );
    }
    use vibex_core::{
        AcpProcessStrategy, AcpProviderConfig, AcpProviderEnvSource,
        AcpProviderProfileCreateRequest, AgentAuthEnvironmentUpdateRequest,
        AgentAuthEnvironmentValue, AgentId, AgentInstallStatus, AgentListRequest,
        AgentModelProviderFailoverSetEntry, AgentModelProviderFailoverSetRequest,
        AgentModelProviderProfileCreateRequest, AgentModelProviderProfileListRequest,
        AgentModelProviderProfileUpdateRequest, AgentModelProviderSetDefaultRequest,
        AgentRefreshSnapshotRequest, AgentRuntimeStatus, AgentUpdateConfigRequest,
        HookCreateRequest, HookEventKind, HookInstallPreviewRequest, HookInstallState, HookStatus,
        McpSecretTarget, McpServerCreateRequest, McpServerDiscoverRequest, McpServerImportRequest,
        McpServerImportSelection, McpServerProviderMatrix, McpServerScopeKind,
        McpServerSecretReferenceCreateRequest, McpServerStatus, McpServerTransportKind,
        McpServerValidateRequest, McpServerValidationStatus, PromptCreateRequest, PromptKind,
        PromptScopeKind, PromptStatus, PromptValidateRequest, PromptValidationStatus,
        ProviderCapabilityProbeStatus, ProviderConfiguredModel, ProviderHealthProbeKind,
        ProviderHealthProbeResult, ProviderHealthStatus, ProviderInjectionPreviewRequest,
        ProviderOptions, ProviderProfileCreateRequest, ProviderProfileId,
        ProviderProfileUpdateRequest, ProviderRunCapabilityProbesRequest,
        ProviderRunHealthProbesRequest, ProviderUsageListRequest, ProviderUsageRecord,
        ProviderUsageUnit, ProviderUsageWindow, SkillCreateRequest, SkillDiscoverRequest,
        SkillImportRequest, SkillImportSelection, SkillProviderMatrix, SkillScopeKind,
        SkillSourceKind, SkillStatus, SkillValidateRequest, SkillValidationStatus,
    };
    use vibex_db::{ProviderHealthRepository, ProviderUsageRepository};

    use super::*;

    #[derive(Default)]
    struct RecordingProfileListener {
        calls: Mutex<Vec<(String, i64)>>,
        deleted_calls: Mutex<Vec<String>>,
    }

    impl ProviderProfileChangeListener for RecordingProfileListener {
        fn on_provider_profile_saved(
            &self,
            provider_profile_id: &ProviderProfileId,
            profile_updated_at_ms: i64,
        ) {
            self.calls.lock().unwrap().push((
                provider_profile_id.as_str().to_string(),
                profile_updated_at_ms,
            ));
        }

        fn on_provider_profile_deleted(&self, provider_profile_id: &ProviderProfileId) {
            self.deleted_calls
                .lock()
                .unwrap()
                .push(provider_profile_id.as_str().to_string());
        }
    }

    #[test]
    fn profile_listener_runs_after_successful_profile_mutations() {
        let dir = tempdir().unwrap();
        let listener = Arc::new(RecordingProfileListener::default());
        let second_listener = Arc::new(RecordingProfileListener::default());
        let service = ProviderConfigService::new(dir.path().join("vibex.db"))
            .with_profile_change_listener(listener.clone())
            .with_profile_change_listener(second_listener.clone());
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(AgentId::parse("opencode").unwrap()),
                display_name: "ACP test".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(AcpProviderConfig {
                    command: "opencode".to_string(),
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd_template: Some("{workspaceRoot}".to_string()),
                    process_strategy: AcpProcessStrategy::PerSession,
                    terminal_tools: false,
                    terminal_auth: false,
                    models: Vec::new(),
                    modes: Vec::new(),
                    features: Vec::new(),
                    disabled_tools: Vec::new(),
                }),
            })
            .unwrap();
        assert_eq!(listener.calls.lock().unwrap().len(), 1);
        assert_eq!(second_listener.calls.lock().unwrap().len(), 1);

        let invalid = service.update_profile(ProviderProfileUpdateRequest {
            provider_profile_id: profile.id.clone(),
            display_name: Some("   ".to_string()),
            status: None,
            account_alias: None,
            base_url: None,
            default_model: None,
            small_model: None,
            large_model: None,
            configured_models: None,
            reasoning_effort: None,
            sandbox_defaults: None,
            network_defaults: None,
            permission_defaults: None,
            provider_options: None,
        });
        assert!(invalid.is_err());
        assert_eq!(listener.calls.lock().unwrap().len(), 1);
        assert_eq!(second_listener.calls.lock().unwrap().len(), 1);

        let updated = service
            .update_profile(ProviderProfileUpdateRequest {
                provider_profile_id: profile.id.clone(),
                display_name: None,
                status: None,
                account_alias: None,
                base_url: Some("https://api.example.test".to_string()),
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: None,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .unwrap();
        let calls = listener.calls.lock().unwrap();
        assert_eq!(calls.len(), 2);
        assert_eq!(calls[1].0, updated.id.as_str());
        assert_eq!(calls[1].1, updated.updated_at_ms);
        let second_calls = second_listener.calls.lock().unwrap();
        assert_eq!(second_calls.len(), 2);
        assert_eq!(second_calls[1].0, updated.id.as_str());
        drop(calls);
        drop(second_calls);

        service
            .delete_profile(ProviderProfileDeleteRequest {
                provider_profile_id: profile.id.clone(),
            })
            .unwrap();
        assert_eq!(
            listener.deleted_calls.lock().unwrap().as_slice(),
            [profile.id.as_str()]
        );
        assert_eq!(
            second_listener.deleted_calls.lock().unwrap().as_slice(),
            [profile.id.as_str()]
        );
    }

    struct EnvVarGuard {
        key: &'static str,
        previous: Option<std::ffi::OsString>,
    }

    impl EnvVarGuard {
        fn set(key: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let previous = std::env::var_os(key);
            unsafe {
                std::env::set_var(key, value);
            }
            Self { key, previous }
        }
    }

    impl Drop for EnvVarGuard {
        fn drop(&mut self) {
            unsafe {
                if let Some(value) = self.previous.as_ref() {
                    std::env::set_var(self.key, value);
                } else {
                    std::env::remove_var(self.key);
                }
            }
        }
    }

    fn env_mutex() -> &'static Mutex<()> {
        static ENV_MUTEX: OnceLock<Mutex<()>> = OnceLock::new();
        ENV_MUTEX.get_or_init(|| Mutex::new(()))
    }

    fn write_fake_executable(dir: &Path, name: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = fake_executable_path(dir, name);
        #[cfg(windows)]
        fs::write(&path, "@echo off\r\nexit /b 0\r\n").unwrap();
        #[cfg(not(windows))]
        fs::write(&path, "#!/bin/sh\nexit 0\n").unwrap();
        make_executable(&path);
        path
    }

    fn write_fake_executable_with_version(dir: &Path, name: &str, version: &str) -> PathBuf {
        fs::create_dir_all(dir).unwrap();
        let path = fake_executable_path(dir, name);
        #[cfg(windows)]
        fs::write(
            &path,
            format!("@echo off\r\necho {version}\r\nexit /b 0\r\n"),
        )
        .unwrap();
        #[cfg(not(windows))]
        fs::write(&path, format!("#!/bin/sh\nprintf '%s\\n' '{version}'\n")).unwrap();
        make_executable(&path);
        path
    }

    fn fake_executable_path(dir: &Path, name: &str) -> PathBuf {
        #[cfg(windows)]
        {
            dir.join(format!("{name}.cmd"))
        }
        #[cfg(not(windows))]
        {
            dir.join(name)
        }
    }

    fn literal_command_path(dir: &Path, name: &str) -> PathBuf {
        #[cfg(windows)]
        {
            dir.join(name)
        }
        #[cfg(not(windows))]
        {
            dir.join(name)
        }
    }

    #[cfg(unix)]
    fn make_executable(path: &Path) {
        use std::os::unix::fs::PermissionsExt;
        let mut permissions = fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(not(unix))]
    fn make_executable(_path: &Path) {}

    #[test]
    fn creates_profile_and_redacted_preview_without_plaintext_secret() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex work".to_string(),
                account_alias: Some("work".to_string()),
                base_url: Some("https://api.openai.invalid/v1".to_string()),
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: Some("medium".to_string()),
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![option_entry("reasoningEffort", "medium")],
                }),
                secret_references: vec![placeholder_secret(
                    ProviderSecretKind::ApiKey,
                    "OPENAI_API_KEY",
                    "OpenAI API key",
                )],
            })
            .unwrap();

        let preview = service
            .preview_injection(ProviderInjectionPreviewRequest {
                provider_profile_id: profile.id,
                project_id: None,
                workspace_id: None,
                session_id: None,
                persist: true,
            })
            .unwrap();

        assert_eq!(preview.profile.display_name, "Codex work");
        assert!(preview.env.iter().all(|field| field.secret));
        assert!(!format!("{preview:?}").contains("sk-"));
    }

    #[test]
    fn codex_runtime_config_allows_duplicate_cc_switch_metadata() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: Some(AgentId::parse("codex").unwrap()),
                kind: ProviderKind::Codex,
                display_name: "Imported Codex".to_string(),
                account_alias: Some("cc-switch-alpha".to_string()),
                base_url: Some("https://api.example.invalid/v1".to_string()),
                default_model: Some("gpt-5.5".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![
                        option_entry("nativeSource", "cc-switch"),
                        option_entry("nativeSource", "legacy-duplicate"),
                        option_entry("ccSwitchProviderId", "alpha"),
                        option_entry("ccSwitchProviderId", "alpha-copy"),
                        option_entry(CODEX_MODEL_PROVIDER_ID_OPTION_KEY, "active-alpha"),
                        option_entry(CODEX_NATIVE_MODEL_PROVIDER_OPTION_KEY, "legacy-alpha"),
                        option_entry(CODEX_NATIVE_MODEL_PROVIDER_OPTION_KEY, "legacy-beta"),
                        option_entry(CODEX_API_KEY_ENV_OPTION_KEY, "OPENAI_API_KEY"),
                        option_entry(CODEX_API_KEY_ENV_OPTION_KEY, "OPENAI_API_KEY"),
                        option_entry("wireApi", "responses"),
                        option_entry("wireApi", "responses"),
                    ],
                }),
                secret_references: Vec::new(),
            })
            .unwrap();

        let config = codex_runtime_config_from_profile(&profile, None).unwrap();

        assert_eq!(config.model_provider_id, "active-alpha");
        assert_eq!(config.api_key_env_key, "OPENAI_API_KEY");
        assert_eq!(config.wire_api.as_deref(), Some("responses"));
    }

    #[test]
    fn codex_runtime_config_rejects_conflicting_used_runtime_option() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: Some(AgentId::parse("codex").unwrap()),
                kind: ProviderKind::Codex,
                display_name: "Conflicting Codex".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid/v1".to_string()),
                default_model: Some("gpt-5.5".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![
                        option_entry(CODEX_MODEL_PROVIDER_ID_OPTION_KEY, "alpha"),
                        option_entry(CODEX_MODEL_PROVIDER_ID_OPTION_KEY, "beta"),
                    ],
                }),
                secret_references: Vec::new(),
            })
            .unwrap();

        let error = match codex_runtime_config_from_profile(&profile, None) {
            Ok(_) => panic!("expected conflicting runtime provider option to fail"),
            Err(error) => error,
        };

        assert_eq!(error.code, "provider_option_key_conflict");
    }

    #[test]
    fn codex_chat_is_rejected_with_one_stable_error_at_create_update_and_projection() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("codex").unwrap();
        let chat_model = ProviderConfiguredModel {
            id: "gpt-test".to_string(),
            display_name: None,
            enabled: true,
            wire_api: Some(vibex_core::ProviderModelWireApi::OpenaiChatCompletions),
            capabilities: Default::default(),
        };

        let create_error = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: "Rejected Codex Chat".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![chat_model.clone()],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(create_error.code, "agent_model_interface_unsupported");

        let profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: "Valid Codex Responses".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    wire_api: Some(vibex_core::ProviderModelWireApi::OpenaiResponses),
                    ..chat_model.clone()
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();
        let update_error = service
            .update_agent_model_provider_profile(AgentModelProviderProfileUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                display_name: None,
                status: None,
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Some(vec![chat_model]),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .unwrap_err();
        assert_eq!(update_error.code, "agent_model_interface_unsupported");

        let mut projector_profile = profile;
        projector_profile.provider_options = ProviderOptions {
            schema_version: 1,
            entries: vec![option_entry("wireApi", "chat_completions")],
        };
        let projection_error = match codex_runtime_config_from_profile(&projector_profile, None) {
            Ok(_) => panic!("Codex Chat must not reach runtime projection"),
            Err(error) => error,
        };
        assert_eq!(projection_error.code, "agent_model_interface_unsupported");
    }

    #[test]
    fn agent_registry_merges_enablement_and_filters_disabled() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        write_fake_executable(&bin_dir, "claude-agent-acp");
        write_fake_executable(&bin_dir, "codex-acp");
        let _path_guard = EnvVarGuard::set("PATH", &bin_dir);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));

        let all = service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap();
        let claude = all
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == "claude")
            .unwrap();
        assert_eq!(claude.install_status, AgentInstallStatus::Installed);
        assert_eq!(claude.runtime_status, AgentRuntimeStatus::Ready);
        assert!(claude.discovered_at_ms.is_some());
        let codex = all
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == "codex")
            .unwrap();
        assert_eq!(codex.install_status, AgentInstallStatus::Installed);
        assert_eq!(codex.runtime_status, AgentRuntimeStatus::Ready);
        assert!(codex.discovered_at_ms.is_some());
        let opencode = all
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == "opencode")
            .unwrap();
        assert_eq!(opencode.label, "OpenCode");
        assert_eq!(opencode.runtime_kind, vibex_core::AgentRuntimeKind::Acp);
        assert!(!all.agents.iter().any(|agent| agent.id.as_str() == "acp"));

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: AgentId::parse("codex").unwrap(),
                added: None,
                enabled: Some(false),
                label_override: Some("Codex disabled".to_string()),
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let enabled = service
            .list_agents(AgentListRequest {
                include_disabled: false,
            })
            .unwrap();
        assert!(
            !enabled
                .agents
                .iter()
                .any(|agent| agent.id.as_str() == "codex")
        );

        let all = service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap();
        let codex = all
            .agents
            .iter()
            .find(|agent| agent.id.as_str() == "codex")
            .unwrap();
        assert!(!codex.enabled);
        assert_eq!(codex.label, "Codex disabled");
    }

    #[test]
    fn resolves_manager_binary_outside_process_path() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let empty_path = dir.path().join("empty-path");
        fs::create_dir_all(&empty_path).unwrap();
        let volta_home = dir.path().join("volta");
        let executable = write_fake_executable(&volta_home.join("bin"), "vibex-agent-test");
        let _path_guard = EnvVarGuard::set("PATH", &empty_path);
        let _volta_guard = EnvVarGuard::set("VOLTA_HOME", &volta_home);

        let resolved = resolve_binary_path("vibex-agent-test").unwrap();

        assert_eq!(PathBuf::from(resolved), executable);
    }

    #[test]
    fn resolves_literal_binary_path_with_platform_extension() {
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        let executable = write_fake_executable(&bin_dir, "vibex-literal-agent");
        let command_path = literal_command_path(&bin_dir, "vibex-literal-agent");

        let resolved = resolve_binary_path(&command_path.to_string_lossy()).unwrap();

        assert_eq!(PathBuf::from(resolved), executable);
    }

    #[test]
    fn agent_model_provider_profiles_default_and_failover_are_agent_scoped() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let claude_agent = AgentId::parse("claude").unwrap();
        let codex_agent = AgentId::parse("codex").unwrap();

        let claude_profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: claude_agent.clone(),
                display_name: "Claude custom".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("claude-custom".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();
        let codex_profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: codex_agent.clone(),
                display_name: "Codex custom".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();

        let claude_profiles = service
            .list_agent_model_provider_profiles(AgentModelProviderProfileListRequest {
                agent_id: claude_agent.clone(),
                include_disabled: true,
            })
            .unwrap();
        assert!(
            claude_profiles
                .profiles
                .iter()
                .any(|entry| entry.profile.id == claude_profile.id)
        );
        assert!(
            !claude_profiles
                .profiles
                .iter()
                .any(|entry| entry.profile.id == codex_profile.id)
        );

        let default = service
            .set_agent_model_provider_default(AgentModelProviderSetDefaultRequest {
                scope: global_default_scope(),
                agent_id: claude_agent.clone(),
                provider_profile_id: claude_profile.id.clone(),
            })
            .unwrap();
        assert_eq!(
            default.provider_profile_id.as_ref(),
            Some(&claude_profile.id)
        );

        let error = service
            .set_agent_model_provider_failover(AgentModelProviderFailoverSetRequest {
                agent_id: claude_agent.clone(),
                entries: vec![AgentModelProviderFailoverSetEntry {
                    provider_profile_id: codex_profile.id.clone(),
                    enabled: true,
                }],
            })
            .unwrap_err();
        assert_eq!(error.code, "failover_profile_agent_mismatch");

        let failover = service
            .set_agent_model_provider_failover(AgentModelProviderFailoverSetRequest {
                agent_id: claude_agent,
                entries: vec![AgentModelProviderFailoverSetEntry {
                    provider_profile_id: claude_profile.id.clone(),
                    enabled: true,
                }],
            })
            .unwrap();
        assert_eq!(failover.entries.len(), 1);
        assert_eq!(failover.entries[0].provider_profile_id, claude_profile.id);
    }

    #[test]
    fn agent_model_provider_display_order_round_trips_without_mutating_failover() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("claude").unwrap();
        for display_name in ["Provider Alpha", "Provider Beta"] {
            service
                .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                    agent_id: agent_id.clone(),
                    display_name: display_name.to_string(),
                    account_alias: None,
                    base_url: None,
                    default_model: None,
                    small_model: None,
                    large_model: None,
                    configured_models: Vec::new(),
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: None,
                    secret_references: Vec::new(),
                })
                .unwrap();
        }
        let mut expected_ids = service
            .list_agent_model_provider_profiles(AgentModelProviderProfileListRequest {
                agent_id: agent_id.clone(),
                include_disabled: true,
            })
            .unwrap()
            .profiles
            .into_iter()
            .map(|entry| entry.profile.id)
            .collect::<Vec<_>>();
        expected_ids.reverse();

        let response = service
            .set_agent_model_provider_display_order(
                vibex_core::AgentModelProviderDisplayOrderSetRequest {
                    agent_id: agent_id.clone(),
                    entries: expected_ids
                        .iter()
                        .cloned()
                        .map(|provider_profile_id| {
                            vibex_core::AgentModelProviderDisplayOrderSetEntry {
                                provider_profile_id,
                            }
                        })
                        .collect(),
                },
            )
            .unwrap();
        assert_eq!(response.entries.len(), expected_ids.len());
        assert_eq!(
            response
                .entries
                .iter()
                .map(|entry| entry.provider_profile_id.clone())
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert_eq!(
            service
                .list_agent_model_provider_profiles(AgentModelProviderProfileListRequest {
                    agent_id: agent_id.clone(),
                    include_disabled: true,
                })
                .unwrap()
                .profiles
                .into_iter()
                .map(|entry| entry.profile.id)
                .collect::<Vec<_>>(),
            expected_ids
        );
        assert!(
            service
                .get_agent_model_provider_failover(AgentModelProviderFailoverListRequest {
                    agent_id: agent_id.clone(),
                })
                .unwrap()
                .entries
                .is_empty()
        );

        let error = service
            .set_agent_model_provider_display_order(
                vibex_core::AgentModelProviderDisplayOrderSetRequest {
                    agent_id,
                    entries: expected_ids
                        .into_iter()
                        .skip(1)
                        .map(|provider_profile_id| {
                            vibex_core::AgentModelProviderDisplayOrderSetEntry {
                                provider_profile_id,
                            }
                        })
                        .collect(),
                },
            )
            .unwrap_err();
        assert_eq!(error.code, "provider_display_order_incomplete");
    }

    #[test]
    fn acp_agent_model_provider_profiles_inherit_and_preserve_runtime_config() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let created = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: "OpenCode relay".to_string(),
                account_alias: None,
                base_url: Some("https://relay.example.invalid/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "gpt-test".to_string(),
                    display_name: Some("GPT Test".to_string()),
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![
                        option_entry(CODEX_MODEL_PROVIDER_ID_OPTION_KEY, "relay"),
                        option_entry("wireApi", "responses"),
                    ],
                }),
                secret_references: Vec::new(),
            })
            .unwrap();

        assert_eq!(created.kind, ProviderKind::Acp);
        assert_eq!(
            created.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
        assert_eq!(
            provider_option_value(
                &created.provider_options,
                CODEX_MODEL_PROVIDER_ID_OPTION_KEY
            )
            .as_deref(),
            Some("relay")
        );
        let inherited = service.get_acp_profile_config(created.id.clone()).unwrap();
        assert!(!inherited.command.trim().is_empty());
        assert!(inherited.args.iter().any(|arg| arg == "acp"));
        assert_eq!(inherited.models, vec!["gpt-test"]);

        let updated = service
            .update_agent_model_provider_profile(AgentModelProviderProfileUpdateRequest {
                agent_id,
                provider_profile_id: created.id.clone(),
                display_name: Some("OpenCode relay updated".to_string()),
                status: None,
                account_alias: None,
                base_url: Some("https://relay-2.example.invalid/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: None,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![option_entry(
                        CODEX_MODEL_PROVIDER_ID_OPTION_KEY,
                        "relay-updated",
                    )],
                }),
            })
            .unwrap();

        assert_eq!(
            provider_option_value(
                &updated.provider_options,
                CODEX_MODEL_PROVIDER_ID_OPTION_KEY
            )
            .as_deref(),
            Some("relay-updated")
        );
        assert_eq!(
            service.get_acp_profile_config(updated.id).unwrap().command,
            inherited.command
        );
        assert_eq!(
            updated.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
    }

    #[test]
    fn managed_acp_reconciliation_preserves_existing_codex_profile_identity_and_defaults() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("codex").unwrap();
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Codex,
                display_name: "Imported Codex relay".to_string(),
                account_alias: Some("work".to_string()),
                base_url: Some("https://relay.example.invalid/v1".to_string()),
                default_model: Some("gpt-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "gpt-test".to_string(),
                    display_name: Some("GPT Test".to_string()),
                    enabled: true,
                    wire_api: None,
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![
                        option_entry(CODEX_MODEL_PROVIDER_ID_OPTION_KEY, "relay"),
                        option_entry("wireApi", "responses"),
                    ],
                }),
                secret_references: vec![placeholder_secret(
                    ProviderSecretKind::ApiKey,
                    "OPENAI_API_KEY",
                    "OPENAI_API_KEY",
                )],
            })
            .unwrap();
        service
            .set_agent_model_provider_default(AgentModelProviderSetDefaultRequest {
                scope: global_default_scope(),
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
            })
            .unwrap();

        let reconciled = service
            .reconcile_agent_acp_runtime(
                agent_id.clone(),
                AgentCommandConfig {
                    command: "/usr/bin/node".to_string(),
                    args: vec!["/managed/codex-acp.js".to_string()],
                },
            )
            .unwrap();

        assert_eq!(reconciled, 1);
        let migrated = service.get_profile(&profile.id).unwrap().unwrap();
        assert_eq!(migrated.id, profile.id);
        assert_eq!(migrated.kind, ProviderKind::Acp);
        assert_eq!(migrated.default_model.as_deref(), Some("gpt-test"));
        assert_eq!(migrated.configured_models, profile.configured_models);
        assert_eq!(migrated.secrets, profile.secrets);
        assert_eq!(
            provider_option_value(
                &migrated.provider_options,
                CODEX_MODEL_PROVIDER_ID_OPTION_KEY
            )
            .as_deref(),
            Some("relay")
        );
        let runtime = service.get_acp_profile_config(profile.id.clone()).unwrap();
        assert_eq!(runtime.command, "/usr/bin/node");
        assert_eq!(runtime.args, vec!["/managed/codex-acp.js"]);
        assert_eq!(runtime.models, vec!["gpt-test"]);
        assert_eq!(
            runtime.modes,
            vec!["read-only", "agent", "agent-full-access"]
        );
        let default = service
            .get_agent_model_provider_default(AgentModelProviderDefaultRequest {
                scope: global_default_scope(),
                agent_id,
            })
            .unwrap();
        assert_eq!(default.provider_profile_id.as_ref(), Some(&profile.id));
        assert_eq!(
            service
                .reconcile_agent_acp_runtime(
                    AgentId::parse("codex").unwrap(),
                    AgentCommandConfig {
                        command: "/usr/bin/node".to_string(),
                        args: vec!["/managed/codex-acp.js".to_string()],
                    },
                )
                .unwrap(),
            0
        );
    }

    #[test]
    fn managed_acp_reconciliation_preserves_imported_claude_environment() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("claude").unwrap();
        let config = AcpProviderConfig {
            command: "claude-agent-acp".to_string(),
            args: Vec::new(),
            env: vec![AcpProviderEnvReference {
                key: "ANTHROPIC_MODEL".to_string(),
                source: AcpProviderEnvSource::Literal,
                value: Some("claude-opus-5[1m]".to_string()),
                secret_lookup_key: None,
                redacted_hint: "imported from cc-switch".to_string(),
            }],
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: vec!["claude-fable-5[1m]".to_string()],
            modes: Vec::new(),
            features: Vec::new(),
            disabled_tools: Vec::new(),
        };
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Claude,
                display_name: "Imported Claude".to_string(),
                account_alias: Some("work".to_string()),
                base_url: Some("https://claude.example.invalid".to_string()),
                default_model: Some("claude-fable-5[1m]".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "claude-fable-5[1m]".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: None,
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(acp_config_to_options(&config).unwrap()),
                secret_references: Vec::new(),
            })
            .unwrap();

        service
            .reconcile_agent_acp_runtime(
                agent_id,
                AgentCommandConfig {
                    command: "/managed/claude-agent-acp".to_string(),
                    args: vec!["/managed/adapter.js".to_string()],
                },
            )
            .unwrap();

        let migrated = service.get_profile(&profile.id).unwrap().unwrap();
        assert_eq!(migrated.kind, ProviderKind::Acp);
        let runtime = service.get_acp_profile_config(profile.id).unwrap();
        assert!(runtime.env.iter().any(|reference| {
            reference.key == "ANTHROPIC_MODEL"
                && reference.value.as_deref() == Some("claude-opus-5[1m]")
        }));
        assert_eq!(runtime.command, "/managed/claude-agent-acp");
        assert_eq!(runtime.args, vec!["/managed/adapter.js"]);
    }

    #[test]
    fn agent_model_provider_secret_value_round_trips_plaintext_locally() {
        let dir = tempdir().unwrap();
        let listener = Arc::new(RecordingProfileListener::default());
        let service = ProviderConfigService::new(dir.path().join("vibex.db"))
            .with_profile_change_listener(listener.clone());
        let agent_id = AgentId::parse("codex").unwrap();
        let api_base_url =
            spawn_http_probe_server("/v1/responses", r#"{"id":"response_test","output":[]}"#);
        let profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: "Codex visible key".to_string(),
                account_alias: None,
                base_url: Some(format!("{api_base_url}/v1")),
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();

        let initial = service
            .get_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: profile.id.clone(),
                },
            )
            .unwrap();
        assert_eq!(initial.value, None);
        assert_eq!(initial.setup_state, ProviderSecretSetupState::Missing);

        let updated = service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: profile.id.clone(),
                    value: Some("sk-visible-local".to_string()),
                    clear: false,
                },
            )
            .unwrap();
        assert_eq!(updated.value.as_deref(), Some("sk-visible-local"));
        assert_eq!(updated.setup_state, ProviderSecretSetupState::Available);
        assert_eq!(updated.backend, ProviderSecretBackend::OsKeychain);
        assert_eq!(listener.calls.lock().unwrap().len(), 2);

        let untouched = service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: profile.id.clone(),
                    value: Some("   ".to_string()),
                    clear: false,
                },
            )
            .unwrap();
        assert_eq!(untouched.value.as_deref(), Some("sk-visible-local"));
        assert_eq!(untouched.setup_state, ProviderSecretSetupState::Available);
        assert_eq!(listener.calls.lock().unwrap().len(), 2);

        let test_result = service
            .test_agent_model_provider_profile(AgentModelProviderProfileTestRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
            })
            .unwrap();
        assert_eq!(
            test_result.status,
            AgentModelProviderTestStatus::Pass,
            "{test_result:#?}"
        );
        assert_eq!(
            test_result.code,
            "agent_model_provider_simple_prompt_probe_passed"
        );

        let cleared = service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id,
                    provider_profile_id: profile.id,
                    value: None,
                    clear: true,
                },
            )
            .unwrap();
        assert_eq!(cleared.value, None);
        assert_eq!(cleared.setup_state, ProviderSecretSetupState::Missing);
        assert_eq!(listener.calls.lock().unwrap().len(), 3);
    }

    #[test]
    fn google_and_bedrock_acp_profiles_send_protocol_correct_prompt_requests() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));

        let (google_base_url, google_request) = spawn_recording_http_probe_server();
        let google_agent = AgentId::parse("gemini").unwrap();
        let google_profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: google_agent.clone(),
                display_name: "Google compatible".to_string(),
                account_alias: None,
                base_url: Some("http://127.0.0.1:1".to_string()),
                default_model: Some("gemini-test".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "gemini-test".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::GoogleGenerativeAi),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![option_entry(
                        vibex_core::ProviderModelWireApi::GoogleGenerativeAi
                            .protocol_base_url_option_key(),
                        google_base_url,
                    )],
                }),
                secret_references: Vec::new(),
            })
            .unwrap();
        service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id: google_agent.clone(),
                    provider_profile_id: google_profile.id.clone(),
                    value: Some("google-secret".to_string()),
                    clear: false,
                },
            )
            .unwrap();
        let result = service
            .test_agent_model_provider_profile(AgentModelProviderProfileTestRequest {
                agent_id: google_agent.clone(),
                provider_profile_id: google_profile.id.clone(),
            })
            .unwrap();
        assert_eq!(
            result.status,
            AgentModelProviderTestStatus::Pass,
            "{result:#?}"
        );
        let request = google_request
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /v1beta/models/gemini-test:generateContent HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("x-goog-api-key: google-secret")
        );
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["contents"][0]["parts"][0]["text"], "ping");
        assert_eq!(body["generationConfig"]["maxOutputTokens"], 1);

        let (bedrock_base_url, bedrock_request) = spawn_recording_http_probe_server();
        let bedrock_agent = AgentId::parse("opencode").unwrap();
        let bedrock_profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: bedrock_agent.clone(),
                display_name: "Bedrock compatible".to_string(),
                account_alias: None,
                base_url: Some("http://127.0.0.1:1".to_string()),
                default_model: Some("anthropic.claude-v1".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "anthropic.claude-v1".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AwsBedrockConverse),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![option_entry(
                        vibex_core::ProviderModelWireApi::AwsBedrockConverse
                            .protocol_base_url_option_key(),
                        bedrock_base_url,
                    )],
                }),
                secret_references: Vec::new(),
            })
            .unwrap();
        service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id: bedrock_agent.clone(),
                    provider_profile_id: bedrock_profile.id.clone(),
                    value: Some("bedrock-secret".to_string()),
                    clear: false,
                },
            )
            .unwrap();
        let result = service
            .test_agent_model_provider_profile(AgentModelProviderProfileTestRequest {
                agent_id: bedrock_agent,
                provider_profile_id: bedrock_profile.id,
            })
            .unwrap();
        assert_eq!(
            result.status,
            AgentModelProviderTestStatus::Pass,
            "{result:#?}"
        );
        let request = bedrock_request
            .recv_timeout(std::time::Duration::from_secs(2))
            .unwrap();
        let (headers, body) = request.split_once("\r\n\r\n").unwrap();
        assert!(headers.starts_with("POST /model/anthropic.claude-v1/converse HTTP/1.1"));
        assert!(
            headers
                .to_ascii_lowercase()
                .contains("authorization: bearer bedrock-secret")
        );
        let body: serde_json::Value = serde_json::from_str(body).unwrap();
        assert_eq!(body["messages"][0]["content"][0]["text"], "ping");
        assert_eq!(body["inferenceConfig"]["maxTokens"], 1);
    }

    #[test]
    fn agent_auth_environment_preserves_blank_values_and_requires_explicit_clear() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "Auth profile".to_string(),
                account_alias: None,
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();

        let saved = service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "OPENCODE_API_KEY".to_string(),
                    value: Some("secret-value".to_string()),
                    secret: true,
                    optional: false,
                    clear: false,
                }],
            })
            .unwrap();
        let saved_config = service.get_acp_profile_config(saved.id.clone()).unwrap();
        let saved_reference = saved_config
            .env
            .iter()
            .find(|reference| reference.key == "OPENCODE_API_KEY")
            .unwrap();
        assert_eq!(
            saved_reference.source,
            AcpProviderEnvSource::SecretReference
        );
        let lookup_key = saved_reference.secret_lookup_key.clone().unwrap();
        assert_eq!(saved.secrets.len(), 1);
        assert!(!format!("{saved:?}").contains("secret-value"));

        let preserved = service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: saved.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "OPENCODE_API_KEY".to_string(),
                    value: Some("   ".to_string()),
                    secret: true,
                    optional: false,
                    clear: false,
                }],
            })
            .unwrap();
        let preserved_config = service
            .get_acp_profile_config(preserved.id.clone())
            .unwrap();
        assert_eq!(
            preserved_config
                .env
                .iter()
                .find(|reference| reference.key == "OPENCODE_API_KEY")
                .and_then(|reference| reference.secret_lookup_key.as_deref()),
            Some(lookup_key.as_str())
        );

        let cleared = service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id,
                provider_profile_id: preserved.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "OPENCODE_API_KEY".to_string(),
                    value: None,
                    secret: true,
                    optional: false,
                    clear: true,
                }],
            })
            .unwrap();
        assert!(
            service
                .get_acp_profile_config(cleared.id.clone())
                .unwrap()
                .env
                .iter()
                .all(|reference| reference.key != "OPENCODE_API_KEY")
        );
        assert!(cleared.secrets.is_empty());
    }

    #[test]
    fn agent_auth_environment_preserves_and_clears_projected_profile_secrets() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("codex").unwrap();
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "Projected auth profile".to_string(),
                account_alias: None,
                preset_id: Some("codex-acp".to_string()),
                config: None,
            })
            .unwrap();
        service
            .update_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueUpdateRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: profile.id.clone(),
                    value: Some("projected-secret".to_string()),
                    clear: false,
                },
            )
            .unwrap();

        let preserved = service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "CODEX_API_KEY".to_string(),
                    value: None,
                    secret: true,
                    optional: false,
                    clear: false,
                }],
            })
            .unwrap();
        assert!(
            service
                .get_acp_profile_config(preserved.id.clone())
                .unwrap()
                .env
                .iter()
                .all(|reference| reference.key != "CODEX_API_KEY")
        );
        assert_eq!(
            service
                .get_agent_model_provider_profile_secret_value(
                    AgentModelProviderProfileSecretValueRequest {
                        agent_id: agent_id.clone(),
                        provider_profile_id: profile.id.clone(),
                    }
                )
                .unwrap()
                .value
                .as_deref(),
            Some("projected-secret")
        );

        service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "CODEX_API_KEY".to_string(),
                    value: None,
                    secret: true,
                    optional: false,
                    clear: true,
                }],
            })
            .unwrap();
        let cleared = service
            .get_agent_model_provider_profile_secret_value(
                AgentModelProviderProfileSecretValueRequest {
                    agent_id,
                    provider_profile_id: profile.id,
                },
            )
            .unwrap();
        assert_eq!(cleared.value, None);
        assert_eq!(cleared.setup_state, ProviderSecretSetupState::Missing);
    }

    #[test]
    fn agent_auth_environment_rolls_back_keychain_writes_when_database_commit_fails() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "Transactional auth profile".to_string(),
                account_alias: None,
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();
        service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                method_id: "api-key".to_string(),
                values: vec![AgentAuthEnvironmentValue {
                    name: "OPENCODE_API_KEY".to_string(),
                    value: Some("original-secret".to_string()),
                    secret: true,
                    optional: false,
                    clear: false,
                }],
            })
            .unwrap();
        let config = service.get_acp_profile_config(profile.id.clone()).unwrap();
        let original_lookup = config
            .env
            .iter()
            .find(|reference| reference.key == "OPENCODE_API_KEY")
            .and_then(|reference| reference.secret_lookup_key.clone())
            .unwrap();
        let new_lookup = format!("vibex-agent-auth-{}-SECOND_API_TOKEN", profile.id.as_str());
        {
            let conn = open_database(service.database_path()).unwrap();
            conn.execute_batch(
                "CREATE TRIGGER fail_agent_auth_profile_update \
                 BEFORE UPDATE ON provider_profiles \
                 BEGIN SELECT RAISE(FAIL, 'forced agent auth persistence failure'); END;",
            )
            .unwrap();
        }

        service
            .update_agent_auth_environment(AgentAuthEnvironmentUpdateRequest {
                agent_id,
                provider_profile_id: profile.id,
                method_id: "api-key".to_string(),
                values: vec![
                    AgentAuthEnvironmentValue {
                        name: "OPENCODE_API_KEY".to_string(),
                        value: Some("replacement-secret".to_string()),
                        secret: true,
                        optional: false,
                        clear: false,
                    },
                    AgentAuthEnvironmentValue {
                        name: "SECOND_API_TOKEN".to_string(),
                        value: Some("new-secret".to_string()),
                        secret: true,
                        optional: false,
                        clear: false,
                    },
                ],
            })
            .unwrap_err();

        assert_eq!(
            secrets::resolve_provider_secret_reference(
                ProviderSecretBackend::OsKeychain,
                ProviderSecretSetupState::Available,
                &original_lookup,
            )
            .unwrap()
            .as_deref(),
            Some("original-secret")
        );
        assert_eq!(
            secrets::resolve_provider_secret_reference(
                ProviderSecretBackend::OsKeychain,
                ProviderSecretSetupState::Available,
                &new_lookup,
            )
            .unwrap(),
            None
        );
        secrets::delete_provider_secret(&original_lookup).unwrap();
    }

    fn spawn_http_probe_server(expected_path: &'static str, response_body: &'static str) -> String {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut buffer = [0_u8; 4096];
            let bytes_read = stream.read(&mut buffer).unwrap_or(0);
            let request = String::from_utf8_lossy(&buffer[..bytes_read]);
            let path_marker = format!(" {expected_path} ");
            let (status, body) = if request.contains(&path_marker) {
                ("200 OK", response_body)
            } else {
                ("404 Not Found", r#"{"error":"unexpected path"}"#)
            };
            let response = format!(
                "HTTP/1.1 {status}\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        format!("http://{address}")
    }

    fn spawn_recording_http_probe_server() -> (String, std::sync::mpsc::Receiver<String>) {
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let (sender, receiver) = std::sync::mpsc::sync_channel(1);
        std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            stream
                .set_read_timeout(Some(std::time::Duration::from_secs(2)))
                .unwrap();
            let mut request = Vec::new();
            let mut buffer = [0_u8; 4096];
            loop {
                let bytes_read = stream.read(&mut buffer).unwrap_or(0);
                if bytes_read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..bytes_read]);
                let Some(header_end) = request.windows(4).position(|part| part == b"\r\n\r\n")
                else {
                    continue;
                };
                let headers = String::from_utf8_lossy(&request[..header_end]);
                let content_length = headers
                    .lines()
                    .find_map(|line| {
                        let (name, value) = line.split_once(':')?;
                        name.eq_ignore_ascii_case("content-length")
                            .then(|| value.trim().parse::<usize>().ok())
                            .flatten()
                    })
                    .unwrap_or(0);
                if request.len() >= header_end + 4 + content_length {
                    break;
                }
            }
            sender.send(String::from_utf8(request).unwrap()).unwrap();
            let body = r#"{"ok":true}"#;
            let response = format!(
                "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nconnection: close\r\n\r\n{body}",
                body.len()
            );
            std::io::Write::write_all(&mut stream, response.as_bytes()).unwrap();
        });
        (format!("http://{address}"), receiver)
    }

    #[test]
    fn agent_refresh_probes_added_disabled_agent_without_runtime_spawn() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        write_fake_executable(&bin_dir, "opencode");
        let _path_guard = EnvVarGuard::set("PATH", &bin_dir);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: AgentId::parse("opencode").unwrap(),
                added: Some(true),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        let refreshed = service
            .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                agent_id: AgentId::parse("opencode").unwrap(),
                cwd_scope: None,
            })
            .unwrap();
        assert_eq!(
            refreshed.agent.install_status,
            AgentInstallStatus::Installed
        );
        assert_eq!(refreshed.agent.runtime_status, AgentRuntimeStatus::Disabled);
        assert!(
            refreshed
                .agent
                .diagnostics
                .iter()
                .any(|entry| entry.value == "low_cost_no_runtime_spawn")
        );

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: AgentId::parse("opencode").unwrap(),
                added: Some(false),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        let removed = service
            .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                agent_id: AgentId::parse("opencode").unwrap(),
                cwd_scope: None,
            })
            .unwrap();
        assert_eq!(removed.agent.install_status, AgentInstallStatus::Disabled);
        assert!(
            removed
                .agent
                .diagnostics
                .iter()
                .any(|entry| entry.value == "removed agents are not probed")
        );
    }

    #[test]
    fn explicit_opencode_refresh_detects_cli_version_for_provider_projection() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        write_fake_executable_with_version(&bin_dir, "opencode", "1.18.11");
        let _path_guard = EnvVarGuard::set("PATH", &bin_dir);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        assert!(service.refresh_detected_agent_versions().unwrap() >= 1);
        let refreshed = service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap()
            .agents
            .into_iter()
            .find(|agent| agent.id == agent_id)
            .unwrap();
        assert!(
            refreshed
                .diagnostics
                .iter()
                .any(|entry| { entry.key == "version" && entry.value == "1.18.11" })
        );

        let runtime = service
            .list_agent_runtime_profiles(&agent_id)
            .unwrap()
            .into_iter()
            .next()
            .expect("enabled OpenCode should have a default ACP runtime");
        assert_eq!(
            runtime.version_identity.agent_version.as_deref(),
            Some("1.18.11")
        );
        let capability = service
            .agent_provider_projection_capability(
                vibex_core::AgentProviderProjectionCapabilityRequest {
                    runtime_profile_id: runtime.id,
                    binding_id: None,
                },
            )
            .unwrap();
        assert_eq!(
            capability.match_kind,
            vibex_core::ProjectionDescriptorMatch::SemverRange
        );
        assert!(
            capability
                .form_controls
                .contains(&vibex_core::AgentProjectionFormControl::ApiKey)
        );

        service
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap();
        assert_eq!(
            service.list_agent_runtime_profiles(&agent_id).unwrap()[0]
                .version_identity
                .agent_version
                .as_deref(),
            Some("1.18.11")
        );

        write_fake_executable(&bin_dir, "opencode");
        let refreshed = service
            .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                agent_id: agent_id.clone(),
                cwd_scope: None,
            })
            .unwrap();
        assert!(refreshed.agent.diagnostics.iter().any(|entry| {
            entry.key == "versionProbe" && entry.value == "agent_version_probe_output_invalid"
        }));
        let runtime = service
            .list_agent_runtime_profiles(&agent_id)
            .unwrap()
            .into_iter()
            .next()
            .unwrap();
        assert_eq!(runtime.version_identity.agent_version, None);
        let capability = service
            .agent_provider_projection_capability(
                vibex_core::AgentProviderProjectionCapabilityRequest {
                    runtime_profile_id: runtime.id,
                    binding_id: None,
                },
            )
            .unwrap();
        assert_eq!(
            capability.match_kind,
            vibex_core::ProjectionDescriptorMatch::Conservative
        );
        assert!(
            !capability
                .form_controls
                .contains(&vibex_core::AgentProjectionFormControl::ApiKey)
        );
    }

    #[test]
    fn repeated_detected_agent_refresh_is_silent_when_projection_is_unchanged() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        write_fake_executable_with_version(&bin_dir, "opencode", "1.18.11");
        let _path_guard = EnvVarGuard::set("PATH", &bin_dir);
        let listener = Arc::new(RecordingProfileListener::default());
        let service = ProviderConfigService::new(dir.path().join("vibex.db"))
            .with_profile_change_listener(listener.clone());
        let agent_id = AgentId::parse("opencode").unwrap();

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id),
                display_name: "OpenCode ACP".to_string(),
                account_alias: None,
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();
        let profile_mutation_calls = listener.calls.lock().unwrap().len();

        service.refresh_detected_agent_versions().unwrap();
        let calls_after_first_refresh = listener.calls.lock().unwrap().len();
        assert!(
            calls_after_first_refresh > profile_mutation_calls,
            "the first detected version must invalidate the changed projection"
        );

        service.refresh_detected_agent_versions().unwrap();
        assert_eq!(
            listener.calls.lock().unwrap().len(),
            calls_after_first_refresh,
            "an idempotent version refresh must not publish ProfilesChanged"
        );
    }

    #[test]
    fn typed_catalog_system_agents_have_trusted_version_probe_binaries() {
        let expected = [
            ("copilot", "copilot"),
            ("codewhale", "codewhale"),
            ("crow-cli", "crow-cli"),
            ("goose", "goose"),
            ("grok", "grok"),
            ("hermes", "hermes"),
            ("kilo", "kilo"),
            ("kimi", "kimi"),
            ("mistral-vibe", "vibe-acp"),
            ("poolside", "pool"),
            ("stakpak", "stakpak"),
            ("vtcode", "vtcode"),
        ];

        for (agent_id, binary_name) in expected {
            assert_eq!(
                trusted_version_probe_binary_names(agent_id),
                Some(&[binary_name][..]),
                "{agent_id} must only probe its code-owned executable name"
            );
        }
        for pinned_agent in ["dirac", "factory-droid", "pi", "qwen-code"] {
            assert_eq!(trusted_version_probe_binary_names(pinned_agent), None);
        }
    }

    #[test]
    fn agent_refresh_distinguishes_native_cli_from_acp_runtime() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let bin_dir = dir.path().join("bin");
        write_fake_executable(&bin_dir, "claude");
        write_fake_executable(&bin_dir, "codex");
        let _path_guard = EnvVarGuard::set("PATH", &bin_dir);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));

        for agent_id in ["claude", "codex"] {
            let refreshed = service
                .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                    agent_id: AgentId::parse(agent_id).unwrap(),
                    cwd_scope: None,
                })
                .unwrap();
            assert_eq!(
                refreshed.agent.install_status,
                AgentInstallStatus::Installed
            );
            assert_eq!(
                refreshed.agent.runtime_status,
                AgentRuntimeStatus::Unavailable
            );
            assert!(
                refreshed
                    .agent
                    .diagnostics
                    .iter()
                    .any(|entry| entry.key == "binaryPath")
            );
            assert!(
                refreshed
                    .agent
                    .diagnostics
                    .iter()
                    .any(|entry| entry.value == "acp_runtime_command_missing")
            );
        }

        write_fake_executable(&bin_dir, "codex-acp");
        let refreshed = service
            .refresh_agent_snapshot(AgentRefreshSnapshotRequest {
                agent_id: AgentId::parse("codex").unwrap(),
                cwd_scope: None,
            })
            .unwrap();
        assert_eq!(
            refreshed.agent.install_status,
            AgentInstallStatus::Installed
        );
        assert_eq!(refreshed.agent.runtime_status, AgentRuntimeStatus::Ready);
        assert!(
            refreshed
                .agent
                .diagnostics
                .iter()
                .any(|entry| entry.key == "runtimeBinaryPath")
        );
    }

    #[test]
    fn acp_catalog_profile_config_and_preview_are_typed_and_redacted() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let catalog = service.list_acp_catalog_presets().unwrap();
        assert!(
            catalog
                .presets
                .iter()
                .any(|preset| preset.preset_id == "opencode")
        );

        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: None,
                display_name: "OpenCode ACP".to_string(),
                account_alias: Some("local opencode".to_string()),
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();
        assert_eq!(profile.kind, ProviderKind::Acp);
        assert!(profile.default_model.is_none());

        let mut config = service.get_acp_profile_config(profile.id.clone()).unwrap();
        assert_eq!(config.process_strategy, AcpProcessStrategy::PerSession);
        config.env.push(AcpProviderEnvReference {
            key: "OPENCODE_AUTH_TOKEN".to_string(),
            source: AcpProviderEnvSource::SecretReference,
            value: None,
            secret_lookup_key: Some("OPENCODE_AUTH_TOKEN".to_string()),
            redacted_hint: "configured in environment".to_string(),
        });
        config.features.push("session_resume".to_string());
        let updated = service
            .update_acp_profile_config(AcpProviderProfileUpdateRequest {
                provider_profile_id: profile.id.clone(),
                config,
            })
            .unwrap();

        let preview = service
            .preview_injection(ProviderInjectionPreviewRequest {
                provider_profile_id: updated.id,
                project_id: None,
                workspace_id: None,
                session_id: None,
                persist: false,
            })
            .unwrap();
        assert!(
            preview
                .cli_args
                .iter()
                .any(|field| { field.key == "command" && field.value == "/usr/bin/opencode" })
        );
        assert!(
            preview
                .cli_args
                .iter()
                .any(|field| field.key == "arg[0]" && field.value == "acp")
        );
        assert!(
            preview
                .sdk_options
                .iter()
                .any(|field| field.key == "cwdTemplate" && field.value == "{workspaceRoot}")
        );
        assert!(
            preview
                .sdk_options
                .iter()
                .any(|field| field.key == "processStrategy" && field.value == "per_session")
        );
        assert!(
            preview
                .env
                .iter()
                .any(|field| field.key == "OPENCODE_AUTH_TOKEN" && field.secret)
        );
        assert!(!format!("{preview:?}").contains("super-secret"));
        assert!(
            !preview
                .sdk_options
                .iter()
                .any(|field| field.key == ACP_CONFIG_OPTION_KEY)
        );
    }

    #[test]
    fn agent_acp_runtime_config_uses_agent_environment_without_a_provider_profile() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: Some(AgentCommandConfig {
                    command: "/bin/opencode-fixture".to_string(),
                    args: vec!["acp".to_string()],
                }),
                env: Some(std::collections::BTreeMap::from([(
                    "OPENCODE_CONFIG_DIR".to_string(),
                    "/tmp/opencode-fixture".to_string(),
                )])),
                params: None,
            })
            .unwrap();

        let config = service.get_agent_acp_runtime_config(&agent_id).unwrap();
        assert_eq!(config.command, "/bin/opencode-fixture");
        assert_eq!(config.args, vec!["acp"]);
        let env = config
            .env
            .iter()
            .find(|reference| reference.key == "OPENCODE_CONFIG_DIR")
            .unwrap();
        assert_eq!(env.source, AcpProviderEnvSource::Literal);
        assert_eq!(env.value.as_deref(), Some("/tmp/opencode-fixture"));
        assert!(env.secret_lookup_key.is_none());
    }

    #[test]
    fn removing_agent_deletes_runtime_and_auth_snapshots() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();
        let conn = service.open_connection().unwrap();
        AgentRuntimeOptionSnapshotRepository::upsert_success(
            &conn,
            &vibex_db::AgentRuntimeOptionSnapshotRecord {
                agent_id: agent_id.clone(),
                session_config: Some(vibex_core::AgentSessionConfigProbe::default()),
                last_success_at_ms: Some(100),
                last_attempt_at_ms: 100,
                last_error_code: None,
            },
        )
        .unwrap();
        vibex_db::AgentAuthCatalogSnapshotRepository::upsert(
            &conn,
            &vibex_db::AgentAuthCatalogSnapshotRecord {
                agent_id: agent_id.clone(),
                provider_profile_id: None,
                catalog: vibex_core::AgentAuthCatalog {
                    agent_id: agent_id.clone(),
                    methods: Vec::new(),
                    supports_logout: false,
                    status: vibex_core::AgentAuthStatus::Unknown,
                    refreshed_at_ms: 100,
                },
                refreshed_at_ms: 100,
            },
        )
        .unwrap();
        drop(conn);

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(false),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let conn = service.open_connection().unwrap();
        assert!(
            AgentRuntimeOptionSnapshotRepository::list(&conn)
                .unwrap()
                .into_iter()
                .all(|snapshot| snapshot.agent_id != agent_id)
        );
        assert!(
            vibex_db::AgentAuthCatalogSnapshotRepository::get(&conn, &agent_id, None)
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn managed_runtime_version_requires_the_selected_command() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("claude").unwrap();
        let command = AgentCommandConfig {
            command: "/managed/node".to_string(),
            args: vec!["/managed/claude-agent-acp.js".to_string()],
        };
        let conn = service.open_connection().unwrap();
        vibex_db::AgentManagedInstallationRepository::upsert(
            &conn,
            &vibex_db::AgentManagedInstallationRecord {
                agent_id: agent_id.clone(),
                registry_agent_id: "claude-acp".to_string(),
                state: vibex_core::AgentManagedInstallState {
                    managed: true,
                    status: vibex_core::AgentManagedInstallStatus::Installed,
                    distribution_kind: Some(vibex_core::AgentManagedDistributionKind::Npm),
                    installed_version: Some("0.65.0".to_string()),
                    available_version: Some("0.65.0".to_string()),
                    last_error_code: None,
                    last_error_message: None,
                    updated_at_ms: Some(100),
                },
                command: Some(command.clone()),
                install_root: Some("/managed/claude".to_string()),
                updated_at_ms: 100,
            },
        )
        .unwrap();
        drop(conn);

        assert_eq!(
            service
                .managed_agent_runtime_version(&agent_id, &command)
                .unwrap()
                .as_deref(),
            Some("0.65.0")
        );
        assert!(
            service
                .managed_agent_runtime_version(
                    &agent_id,
                    &AgentCommandConfig {
                        command: "claude-agent-acp".to_string(),
                        args: Vec::new(),
                    }
                )
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn acp_capability_probe_projects_typed_config_without_provider_process() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: None,
                display_name: "OpenCode ACP".to_string(),
                account_alias: Some("local opencode".to_string()),
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();

        let result = service
            .run_capability_probes(ProviderRunCapabilityProbesRequest {
                provider_profile_ids: Some(vec![profile.id.clone()]),
                force_refresh: true,
            })
            .unwrap();

        assert_eq!(result.results.len(), 1);
        let probe = &result.results[0];
        assert_eq!(probe.status, ProviderCapabilityProbeStatus::Pass);
        assert_eq!(probe.source, "acp_profile_config");
        // Provider-free projection cannot claim a model list when the preset
        // intentionally carries no synthetic placeholder models.
        assert!(!probe.capabilities.model_list);
        assert!(probe.capabilities.dynamic_modes);
        assert!(probe.capabilities.tool_invocations);
        assert!(probe.capabilities.permission_requests);
        assert!(probe.capabilities.slash_commands);
        assert!(probe.capabilities.skills);
        // The generic ACP runtime implements session/cancel, so bundled
        // presets now grant the interrupt capability.
        assert!(probe.capabilities.interrupt);
        assert!(
            probe
                .diagnostics
                .iter()
                .any(|entry| entry.key == "rawProviderPayloadStored" && entry.value == "false")
        );

        let summaries = service.list_capability_summaries().unwrap();
        let summary = summaries
            .iter()
            .find(|summary| summary.profile.id == profile.id)
            .unwrap();
        assert_eq!(summary.status, ProviderCapabilityProbeStatus::Pass);
        assert!(summary.fresh);
        assert_eq!(summary.capability_source, "acp_profile_config");
        assert!(summary.effective_capabilities.tool_invocations);
        assert!(summary.effective_capabilities.slash_commands);
        assert!(summary.effective_capabilities.skills);
    }

    #[test]
    fn acp_validation_rejects_missing_command_secret_literals_and_duplicate_options() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));

        let missing_config = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Acp,
                display_name: "Broken ACP".to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(missing_config.code, "acp_config_missing");

        let secret_literal = AcpProviderConfig {
            command: "/usr/bin/opencode".to_string(),
            args: vec!["acp".to_string()],
            env: vec![AcpProviderEnvReference {
                key: "OPENCODE_AUTH_TOKEN".to_string(),
                source: AcpProviderEnvSource::Literal,
                value: Some("super-secret".to_string()),
                secret_lookup_key: None,
                redacted_hint: "redacted".to_string(),
            }],
            cwd_template: Some("{workspaceRoot}".to_string()),
            process_strategy: AcpProcessStrategy::default(),
            terminal_tools: false,
            terminal_auth: false,
            models: vec!["opencode-default".to_string()],
            modes: vec!["default".to_string()],
            features: Vec::new(),
            disabled_tools: Vec::new(),
        };
        let error = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: None,
                display_name: "Unsafe ACP".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(secret_literal),
            })
            .unwrap_err();
        assert_eq!(error.code, "acp_env_literal_secret_rejected");

        let duplicate_options = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Acp,
                display_name: "Duplicate ACP".to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(ProviderOptions {
                    schema_version: 1,
                    entries: vec![
                        option_entry(ACP_CONFIG_OPTION_KEY, "{}"),
                        option_entry(ACP_CONFIG_OPTION_KEY, "{}"),
                    ],
                }),
                secret_references: Vec::new(),
            })
            .unwrap_err();
        assert_eq!(duplicate_options.code, "provider_option_key_duplicate");
    }

    #[test]
    fn mcp_validation_is_deterministic_and_preview_includes_enabled_servers() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex MCP target".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();

        let invalid = service
            .validate_mcp_server(McpServerValidateRequest {
                mcp_server_id: None,
                candidate: Some(McpServerCreateRequest {
                    display_name: "Broken stdio".to_string(),
                    transport_kind: McpServerTransportKind::Stdio,
                    status: McpServerStatus::Enabled,
                    scope_kind: McpServerScopeKind::Global,
                    project_id: None,
                    workspace_id: None,
                    command: None,
                    args: Vec::new(),
                    env: Vec::new(),
                    url: None,
                    headers: Vec::new(),
                    description: None,
                    tags: Vec::new(),
                    secret_references: Vec::new(),
                    provider_matrix: Vec::new(),
                }),
            })
            .unwrap();
        assert_eq!(invalid.status, McpServerValidationStatus::Fail);
        assert_eq!(invalid.code, "mcp_server_stdio_command_missing");

        let server = service
            .create_mcp_server(McpServerCreateRequest {
                display_name: "Filesystem tools".to_string(),
                transport_kind: McpServerTransportKind::Stdio,
                status: McpServerStatus::Enabled,
                scope_kind: McpServerScopeKind::Workspace,
                project_id: None,
                workspace_id: None,
                command: Some("mcp-filesystem".to_string()),
                args: vec!["--root".to_string(), "/tmp/workspace".to_string()],
                env: Vec::new(),
                url: None,
                headers: Vec::new(),
                description: None,
                tags: vec!["filesystem".to_string()],
                secret_references: vec![McpServerSecretReferenceCreateRequest {
                    secret_kind: ProviderSecretKind::Environment,
                    backend: ProviderSecretBackend::Placeholder,
                    setup_state: ProviderSecretSetupState::Missing,
                    lookup_key: "MCP_TOKEN".to_string(),
                    display_label: "MCP token".to_string(),
                    redacted_hint: "not configured".to_string(),
                    target: McpSecretTarget::Environment,
                }],
                provider_matrix: vec![McpServerProviderMatrix {
                    provider_kind: ProviderKind::Codex,
                    enabled: true,
                    updated_at_ms: unix_timestamp_ms(),
                }],
            })
            .unwrap();
        let valid = service
            .validate_mcp_server(McpServerValidateRequest {
                mcp_server_id: Some(server.id),
                candidate: None,
            })
            .unwrap();
        assert_eq!(valid.status, McpServerValidationStatus::Pass);
        assert!(
            valid
                .diagnostics
                .iter()
                .any(|entry| entry.key == "noProcessOrNetwork")
        );

        let preview = service
            .preview_injection(ProviderInjectionPreviewRequest {
                provider_profile_id: profile.id,
                project_id: None,
                workspace_id: None,
                session_id: None,
                persist: false,
            })
            .unwrap();
        assert!(
            preview
                .mcp_servers
                .iter()
                .any(|entry| entry.contains("Filesystem tools (stdio) -> codex"))
        );
        assert!(!format!("{preview:?}").contains("secret-token"));
    }

    #[test]
    fn mcp_discovery_import_is_read_only_and_enables_source_agent() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex-home");
        fs::create_dir_all(&codex_home).unwrap();
        let native_config = r#"
[mcp_servers.workspace]
command = "mcp-workspace"
args = ["--root", "/tmp/workspace"]
"#;
        fs::write(codex_home.join("config.toml"), native_config).unwrap();
        let _guard = EnvVarGuard::set("CODEX_HOME", &codex_home);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let codex_agent = AgentId::parse("codex").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: codex_agent.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigDir": codex_home })),
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let source_path = codex_home.join("config.toml").display().to_string();
        let discovery = discovered
            .discoveries
            .into_iter()
            .find(|discovery| discovery.source_path == source_path)
            .expect("configured Codex MCP source should be discovered");
        assert_eq!(discovery.source_agent_id, codex_agent);
        let result = service
            .import_mcp_servers(McpServerImportRequest {
                selections: vec![McpServerImportSelection {
                    discovery_id: discovery.discovery_id,
                    source_agent_id: codex_agent.clone(),
                    candidate: discovery.candidate.unwrap(),
                    enable_agent_ids: Vec::new(),
                }],
            })
            .unwrap();

        assert_eq!(
            fs::read_to_string(codex_home.join("config.toml")).unwrap(),
            native_config
        );
        assert_eq!(result.created_count, 1);
        let imported = result.imported.first().unwrap();
        assert_eq!(imported.agent_matrix.len(), 1);
        assert_eq!(imported.agent_matrix[0].agent_id, codex_agent);
        assert!(imported.agent_matrix[0].enabled);
    }

    #[test]
    fn mcp_import_existing_server_only_enables_source_agent() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let claude_agent = AgentId::parse("claude").unwrap();
        let codex_agent = AgentId::parse("codex").unwrap();
        let existing = service
            .create_mcp_server(McpServerCreateRequest {
                display_name: "Workspace MCP".to_string(),
                transport_kind: McpServerTransportKind::Stdio,
                status: McpServerStatus::Enabled,
                scope_kind: McpServerScopeKind::User,
                project_id: None,
                workspace_id: None,
                command: Some("mcp-workspace".to_string()),
                args: vec!["--old".to_string()],
                env: Vec::new(),
                url: None,
                headers: Vec::new(),
                description: Some("User curated description".to_string()),
                tags: vec!["curated".to_string()],
                secret_references: Vec::new(),
                provider_matrix: Vec::new(),
            })
            .unwrap();
        service
            .set_mcp_server_agent_matrix(McpServerSetAgentMatrixRequest {
                mcp_server_id: existing.id.clone(),
                agent_matrix: vec![McpServerAgentMatrix {
                    agent_id: claude_agent.clone(),
                    enabled: true,
                    source_kind: ResourceAgentMatrixSourceKind::Manual,
                    updated_at_ms: unix_timestamp_ms(),
                }],
            })
            .unwrap();

        let result = service
            .import_mcp_servers(McpServerImportRequest {
                selections: vec![McpServerImportSelection {
                    discovery_id: "mcp:test".to_string(),
                    source_agent_id: codex_agent.clone(),
                    candidate: McpServerCreateRequest {
                        display_name: "Workspace MCP".to_string(),
                        transport_kind: McpServerTransportKind::Stdio,
                        status: McpServerStatus::Enabled,
                        scope_kind: McpServerScopeKind::User,
                        project_id: None,
                        workspace_id: None,
                        command: Some("mcp-workspace".to_string()),
                        args: vec!["--new".to_string()],
                        env: Vec::new(),
                        url: None,
                        headers: Vec::new(),
                        description: Some("Imported description".to_string()),
                        tags: vec!["imported".to_string()],
                        secret_references: Vec::new(),
                        provider_matrix: Vec::new(),
                    },
                    enable_agent_ids: Vec::new(),
                }],
            })
            .unwrap();

        assert_eq!(result.created_count, 0);
        assert_eq!(result.updated_count, 1);
        let imported = result.imported.first().unwrap();
        assert_eq!(
            imported.description.as_deref(),
            Some("User curated description")
        );
        assert_eq!(imported.args, vec!["--old".to_string()]);
        assert!(
            imported
                .agent_matrix
                .iter()
                .any(|entry| entry.agent_id == claude_agent && entry.enabled)
        );
        assert!(
            imported
                .agent_matrix
                .iter()
                .any(|entry| entry.agent_id == codex_agent && entry.enabled)
        );
    }

    #[test]
    fn mcp_discovery_scans_added_agents_and_skips_removed_agents() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex-disabled-home");
        fs::create_dir_all(&codex_home).unwrap();
        fs::write(
            codex_home.join("config.toml"),
            r#"
[mcp_servers.disabled]
command = "mcp-disabled"
"#,
        )
        .unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let codex_agent = AgentId::parse("codex").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: codex_agent.clone(),
                added: Some(true),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigDir": codex_home })),
            })
            .unwrap();

        let all_agents = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let source_path = codex_home.join("config.toml").display().to_string();
        assert!(
            all_agents
                .discoveries
                .iter()
                .any(|discovery| discovery.source_path == source_path)
        );

        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: codex_agent.clone(),
                added: Some(false),
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigDir": codex_home })),
            })
            .unwrap();
        let removed_agents = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        assert!(
            !removed_agents
                .discoveries
                .iter()
                .any(|discovery| discovery.source_path == source_path)
        );
    }

    #[test]
    fn mcp_discovery_reads_opencode_mcp_container() {
        let dir = tempdir().unwrap();
        let opencode_home = dir.path().join("opencode-home");
        fs::create_dir_all(&opencode_home).unwrap();
        fs::write(
            opencode_home.join("opencode.json"),
            r#"
{
  "mcp": {
    "deepwiki": {
      "type": "remote",
      "url": "https://mcp.deepwiki.com/mcp",
      "enabled": true
    }
  }
}
"#,
        )
        .unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let opencode_agent = AgentId::parse("opencode").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: opencode_agent.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigDir": opencode_home })),
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let discovery = discovered
            .discoveries
            .into_iter()
            .find(|discovery| {
                discovery
                    .candidate
                    .as_ref()
                    .is_some_and(|candidate| candidate.display_name == "deepwiki")
            })
            .expect("OpenCode mcp container should be discovered");
        let candidate = discovery.candidate.unwrap();
        assert_eq!(candidate.transport_kind, McpServerTransportKind::Http);
        assert_eq!(
            candidate.url.as_deref(),
            Some("https://mcp.deepwiki.com/mcp")
        );
    }

    #[test]
    fn mcp_discovery_reads_added_gemini_default_home() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let gemini_home = dir.path().join("gemini-home");
        fs::create_dir_all(&gemini_home).unwrap();
        fs::write(
            gemini_home.join("settings.json"),
            r#"
{
  "mcpServers": {
    "gemini-http": {
      "httpUrl": "https://example.invalid/mcp"
    },
    "gemini-stdio": {
      "command": "uvx",
      "args": ["mcp-server-fetch"]
    }
  }
}
"#,
        )
        .unwrap();
        let _gemini_guard = EnvVarGuard::set("GEMINI_HOME", &gemini_home);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let gemini_agent = AgentId::parse("gemini").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: gemini_agent,
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let candidates = discovered
            .discoveries
            .iter()
            .filter_map(|discovery| discovery.candidate.as_ref())
            .collect::<Vec<_>>();
        let http = candidates
            .iter()
            .find(|candidate| candidate.display_name == "gemini-http")
            .expect("Gemini HTTP MCP should be discovered from default home");
        assert_eq!(http.transport_kind, McpServerTransportKind::Http);
        assert_eq!(http.url.as_deref(), Some("https://example.invalid/mcp"));
        let stdio = candidates
            .iter()
            .find(|candidate| candidate.display_name == "gemini-stdio")
            .expect("Gemini stdio MCP should be discovered from default home");
        assert_eq!(stdio.transport_kind, McpServerTransportKind::Stdio);
        assert_eq!(stdio.command.as_deref(), Some("uvx"));
        assert_eq!(stdio.args, vec!["mcp-server-fetch".to_string()]);
    }

    #[test]
    fn mcp_discovery_reads_agent_env_config_home() {
        let dir = tempdir().unwrap();
        let xdg_home = dir.path().join("xdg-config");
        let goose_home = xdg_home.join("goose");
        fs::create_dir_all(&goose_home).unwrap();
        fs::write(
            goose_home.join("config.json"),
            r#"
{
  "mcpServers": {
    "goose-env-home": {
      "command": "node",
      "args": ["server.js"]
    }
  }
}
"#,
        )
        .unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let goose_agent = AgentId::parse("goose").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: goose_agent,
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: Some(std::collections::BTreeMap::from([(
                    "XDG_CONFIG_HOME".to_string(),
                    xdg_home.display().to_string(),
                )])),
                params: None,
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let candidate = discovered
            .discoveries
            .iter()
            .filter_map(|discovery| discovery.candidate.as_ref())
            .find(|candidate| candidate.display_name == "goose-env-home")
            .expect("MCP should be discovered from agent env config home");
        assert_eq!(candidate.transport_kind, McpServerTransportKind::Stdio);
        assert_eq!(candidate.command.as_deref(), Some("node"));
        assert_eq!(candidate.args, vec!["server.js".to_string()]);
    }

    #[test]
    fn mcp_discovery_reads_json_mcp_servers_nested_container() {
        let dir = tempdir().unwrap();
        let config_home = dir.path().join("config-home");
        fs::create_dir_all(&config_home).unwrap();
        fs::write(
            config_home.join("settings.json"),
            r#"
{
  "mcp": {
    "servers": {
      "nested-server": {
        "command": "nested-mcp",
        "args": ["--ok"]
      }
    }
  }
}
"#,
        )
        .unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let codex_agent = AgentId::parse("codex").unwrap();
        let config_path = config_home.join("settings.json");
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: codex_agent,
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigPath": config_path })),
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let candidate = discovered
            .discoveries
            .iter()
            .filter_map(|discovery| discovery.candidate.as_ref())
            .find(|candidate| candidate.display_name == "nested-server")
            .expect("nested mcp.servers container should be discovered");
        assert_eq!(candidate.transport_kind, McpServerTransportKind::Stdio);
        assert_eq!(candidate.command.as_deref(), Some("nested-mcp"));
        assert_eq!(candidate.args, vec!["--ok".to_string()]);
    }

    #[test]
    fn mcp_discovery_reads_yaml_from_added_agent_home() {
        let _env_lock = env_mutex().lock().unwrap();
        let dir = tempdir().unwrap();
        let goose_home = dir.path().join("goose-home");
        fs::create_dir_all(&goose_home).unwrap();
        fs::write(
            goose_home.join("config.yaml"),
            r#"
mcp_servers:
  goose-stdio:
    command: uvx
    args:
      - mcp-server-fetch
  goose-http:
    url: https://example.invalid/hermes/mcp
    headers:
      Authorization: Bearer should-not-be-stored
"#,
        )
        .unwrap();
        let _goose_guard = EnvVarGuard::set("GOOSE_HOME", &goose_home);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let goose_agent = AgentId::parse("goose").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: goose_agent,
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let candidates = discovered
            .discoveries
            .iter()
            .filter_map(|discovery| discovery.candidate.as_ref())
            .collect::<Vec<_>>();
        let stdio = candidates
            .iter()
            .find(|candidate| candidate.display_name == "goose-stdio")
            .expect("YAML stdio MCP should be discovered from added agent home");
        assert_eq!(stdio.transport_kind, McpServerTransportKind::Stdio);
        assert_eq!(stdio.command.as_deref(), Some("uvx"));
        let http = candidates
            .iter()
            .find(|candidate| candidate.display_name == "goose-http")
            .expect("YAML HTTP MCP should be discovered from added agent home");
        assert_eq!(http.transport_kind, McpServerTransportKind::Http);
        assert_eq!(
            http.url.as_deref(),
            Some("https://example.invalid/hermes/mcp")
        );
        assert_eq!(http.secret_references.len(), 1);
        assert_eq!(http.secret_references[0].lookup_key, "Authorization");
        assert!(!format!("{http:?}").contains("should-not-be-stored"));
    }

    #[test]
    fn mcp_discovery_imports_header_mcp_without_plaintext_secret() {
        let dir = tempdir().unwrap();
        let codex_home = dir.path().join("codex-header-home");
        fs::create_dir_all(&codex_home).unwrap();
        let source_path = codex_home.join("config.toml");
        fs::write(
            &source_path,
            r#"
[mcp_servers.stitch]
type = "http"
url = "https://stitch.googleapis.com/mcp"

[mcp_servers.stitch.http_headers]
Authorization = "Bearer should-not-be-stored"
"#,
        )
        .unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let codex_agent = AgentId::parse("codex").unwrap();
        let expected_source_path = source_path.display().to_string();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: codex_agent.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "mcpConfigDir": codex_home })),
            })
            .unwrap();

        let discovered = service
            .discover_mcp_sources(McpServerDiscoverRequest {
                source_agent_id: None,
            })
            .unwrap();
        let discovery = discovered
            .discoveries
            .into_iter()
            .find(|discovery| {
                discovery.source_path == expected_source_path
                    && discovery
                        .candidate
                        .as_ref()
                        .is_some_and(|candidate| candidate.display_name == "stitch")
            })
            .expect("header-backed HTTP MCP should be discovered");
        let candidate = discovery.candidate.unwrap();
        assert_eq!(candidate.transport_kind, McpServerTransportKind::Http);
        assert_eq!(
            candidate.url.as_deref(),
            Some("https://stitch.googleapis.com/mcp")
        );
        assert_eq!(candidate.secret_references.len(), 1);
        assert_eq!(
            candidate.secret_references[0].secret_kind,
            ProviderSecretKind::Header
        );
        assert_eq!(candidate.secret_references[0].lookup_key, "Authorization");
        assert_eq!(
            candidate.secret_references[0].target,
            McpSecretTarget::Header
        );

        let result = service
            .import_mcp_servers(McpServerImportRequest {
                selections: vec![McpServerImportSelection {
                    discovery_id: discovery.discovery_id,
                    source_agent_id: codex_agent,
                    candidate,
                    enable_agent_ids: Vec::new(),
                }],
            })
            .unwrap();
        let imported = result.imported.first().unwrap();
        assert_eq!(imported.display_name, "stitch");
        assert_eq!(imported.secret_references.len(), 1);
        assert_eq!(imported.secret_references[0].lookup_key, "Authorization");
        assert_eq!(
            imported.secret_references[0].redacted_hint,
            "present in native config; configure in Vibex"
        );
        assert!(!format!("{imported:?}").contains("should-not-be-stored"));
    }

    #[test]
    fn skill_prompt_validation_and_preview_are_provider_free() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex skill target".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();

        let invalid_skill = service
            .validate_skill(SkillValidateRequest {
                skill_id: None,
                candidate: Some(SkillCreateRequest {
                    display_name: "Broken manual skill".to_string(),
                    source_kind: SkillSourceKind::Manual,
                    status: SkillStatus::Enabled,
                    scope_kind: SkillScopeKind::Global,
                    project_id: None,
                    workspace_id: None,
                    source_uri: None,
                    description: None,
                    tags: Vec::new(),
                    content_preview: None,
                    provider_matrix: Vec::new(),
                }),
            })
            .unwrap();
        assert_eq!(invalid_skill.status, SkillValidationStatus::Fail);
        assert_eq!(invalid_skill.code, "skill_manual_content_missing");

        let skill = service
            .create_skill(SkillCreateRequest {
                display_name: "Rust quality guide".to_string(),
                source_kind: SkillSourceKind::Manual,
                status: SkillStatus::Enabled,
                scope_kind: SkillScopeKind::Workspace,
                project_id: None,
                workspace_id: None,
                source_uri: None,
                description: Some("Prefer package-scoped cargo checks.".to_string()),
                tags: vec!["rust".to_string()],
                content_preview: Some("cargo test -p vibex-db skill".to_string()),
                provider_matrix: vec![SkillProviderMatrix {
                    provider_kind: ProviderKind::Codex,
                    enabled: true,
                    updated_at_ms: unix_timestamp_ms(),
                }],
            })
            .unwrap();
        let valid_skill = service
            .validate_skill(SkillValidateRequest {
                skill_id: Some(skill.id),
                candidate: None,
            })
            .unwrap();
        assert_eq!(valid_skill.status, SkillValidationStatus::Pass);
        assert!(
            valid_skill
                .diagnostics
                .iter()
                .any(|entry| entry.key == "noNetworkOrNativeWrite")
        );

        let invalid_prompt = service
            .validate_prompt(PromptValidateRequest {
                prompt_id: None,
                candidate: Some(PromptCreateRequest {
                    display_name: "Empty body".to_string(),
                    kind: PromptKind::ReusablePrompt,
                    status: PromptStatus::Enabled,
                    scope_kind: PromptScopeKind::User,
                    project_id: None,
                    workspace_id: None,
                    body: " ".to_string(),
                    description: None,
                    tags: Vec::new(),
                }),
            })
            .unwrap();
        assert_eq!(invalid_prompt.status, PromptValidationStatus::Fail);
        assert_eq!(invalid_prompt.code, "prompt_body_empty");

        let prompt = service
            .create_prompt(PromptCreateRequest {
                display_name: "Release digest".to_string(),
                kind: PromptKind::ReusablePrompt,
                status: PromptStatus::Enabled,
                scope_kind: PromptScopeKind::User,
                project_id: None,
                workspace_id: None,
                body: "Summarize changes and residual risks.".to_string(),
                description: Some("Reusable release prompt".to_string()),
                tags: vec!["release".to_string()],
            })
            .unwrap();
        let valid_prompt = service
            .validate_prompt(PromptValidateRequest {
                prompt_id: Some(prompt.id),
                candidate: None,
            })
            .unwrap();
        assert_eq!(valid_prompt.status, PromptValidationStatus::Pass);

        let preview = service
            .preview_injection(ProviderInjectionPreviewRequest {
                provider_profile_id: profile.id,
                project_id: None,
                workspace_id: None,
                session_id: None,
                persist: false,
            })
            .unwrap();
        assert!(
            preview
                .skills
                .iter()
                .any(|entry| entry.contains("Skill: Rust quality guide"))
        );
        assert!(
            preview
                .skills
                .iter()
                .any(|entry| entry.contains("Prompt: Release digest"))
        );
    }

    #[test]
    fn skill_discovery_import_is_read_only_and_enables_source_agent() {
        let dir = tempdir().unwrap();
        let agents_home = dir.path().join("agents-home");
        let skill_dir = agents_home.join("skills").join("rust-quality");
        fs::create_dir_all(&skill_dir).unwrap();
        let manifest = "---\nname: Rust Quality\ndescription: Keep checks scoped.\n---\n# Rust Quality\nRun cargo checks.";
        let manifest_path = skill_dir.join("SKILL.md");
        fs::write(&manifest_path, manifest).unwrap();
        let _guard = EnvVarGuard::set("AGENTS_HOME", &agents_home);
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let claude_agent = AgentId::parse("claude").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: claude_agent.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: Some(serde_json::json!({ "skillsDir": agents_home.join("skills") })),
            })
            .unwrap();

        let discovered = service
            .discover_skill_sources(SkillDiscoverRequest {
                source_agent_id: None,
                workspace_id: None,
            })
            .unwrap();
        let source_path = manifest_path.display().to_string();
        let discovery = discovered
            .discoveries
            .into_iter()
            .find(|discovery| discovery.source_path == source_path)
            .expect("configured Skill source should be discovered");
        assert_eq!(discovery.source_agent_id, claude_agent);
        let result = service
            .import_skills(SkillImportRequest {
                selections: vec![SkillImportSelection {
                    discovery_id: discovery.discovery_id,
                    source_agent_id: claude_agent.clone(),
                    source_path: discovery.source_path,
                    display_name: discovery.display_name,
                    command_name: discovery.command_name,
                    description: discovery.description,
                    content_preview: discovery.content_preview,
                    enable_agent_ids: Vec::new(),
                }],
            })
            .unwrap();

        assert_eq!(fs::read_to_string(manifest_path).unwrap(), manifest);
        assert_eq!(result.created_count, 1);
        let imported = result.imported.first().unwrap();
        assert_eq!(imported.agent_matrix.len(), 1);
        assert_eq!(imported.agent_matrix[0].agent_id, claude_agent);
        assert!(imported.agent_matrix[0].enabled);
    }

    #[test]
    fn hook_install_preview_is_preview_only_and_persisted() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let hook = service
            .create_hook(HookCreateRequest {
                display_name: "Terminal activity audit".to_string(),
                provider_kind: ProviderKind::Claude,
                event_kind: HookEventKind::TerminalActivity,
                status: HookStatus::Draft,
                command_preview: Some("vibex hook terminal-activity".to_string()),
                managed_marker: Some("VIBEX-MANAGED-HOOK:test".to_string()),
                description: Some("Preview-only hook intent".to_string()),
            })
            .unwrap();

        let preview = service
            .preview_hook_install(HookInstallPreviewRequest {
                hook_id: hook.id.clone(),
                target_path: Some("~/.claude/settings.json".to_string()),
            })
            .unwrap();
        assert_eq!(preview.hook_id, hook.id);
        assert_eq!(preview.marker, "VIBEX-MANAGED-HOOK:test");
        assert!(preview.redacted_preview.contains("preview only"));
        assert!(
            service
                .list_hooks()
                .unwrap()
                .iter()
                .any(|hook| hook.install_state == HookInstallState::PreviewOnly)
        );
    }

    #[test]
    fn seeds_local_defaults_for_runtime_compatibility_only() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let runtime_profiles = service.list_runtime_profiles().unwrap();
        let visible_profiles = service.list_profiles().unwrap();

        for kind in [ProviderKind::Codex, ProviderKind::Claude, ProviderKind::Acp] {
            assert!(
                runtime_profiles
                    .iter()
                    .any(|profile| { profile.id.as_str() == kind.local_default_profile_id() })
            );
            assert!(
                visible_profiles
                    .iter()
                    .all(|profile| { profile.id.as_str() != kind.local_default_profile_id() })
            );
        }
    }

    #[test]
    fn model_provider_views_hide_internal_profiles_and_keep_user_profiles() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let agent_id = AgentId::parse("opencode").unwrap();
        service
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            })
            .unwrap();

        let runtime_profile = service
            .list_runtime_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| {
                profile.agent_id == agent_id
                    && provider_option_value(
                        &profile.provider_options,
                        INTERNAL_PROFILE_ROLE_OPTION_KEY,
                    )
                    .as_deref()
                        == Some(INTERNAL_AGENT_RUNTIME_PROFILE_ROLE)
            })
            .expect("enabling OpenCode should seed an internal runtime profile");
        let user_model_profile = service
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: "OpenCode ACP".to_string(),
                account_alias: None,
                base_url: Some("https://relay.example.invalid/v1".to_string()),
                default_model: Some("user-model".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();
        assert_eq!(
            provider_option_value(
                &user_model_profile.provider_options,
                INTERNAL_PROFILE_ROLE_OPTION_KEY,
            ),
            None,
            "a user model-provider profile must not inherit the runtime-only role marker"
        );

        let legacy_config = service.get_agent_acp_runtime_config(&agent_id).unwrap();
        let legacy_runtime_profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "OpenCode ACP".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(legacy_config),
            })
            .unwrap();
        assert_eq!(
            provider_option_value(
                &legacy_runtime_profile.provider_options,
                INTERNAL_PROFILE_ROLE_OPTION_KEY,
            ),
            None,
            "legacy seeded profiles predate the explicit role marker"
        );

        let user_acp_profile = service
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "My OpenCode runtime".to_string(),
                account_alias: None,
                preset_id: Some("opencode".to_string()),
                config: None,
            })
            .unwrap();
        let visible_ids = service
            .list_agent_model_provider_profiles(AgentModelProviderProfileListRequest {
                agent_id: agent_id.clone(),
                include_disabled: true,
            })
            .unwrap()
            .profiles
            .into_iter()
            .map(|entry| entry.profile.id)
            .collect::<HashSet<_>>();
        assert!(!visible_ids.contains(&runtime_profile.id));
        assert!(!visible_ids.contains(&legacy_runtime_profile.id));
        assert!(visible_ids.contains(&user_acp_profile.id));
        assert!(visible_ids.contains(&user_model_profile.id));
        assert_eq!(
            visible_ids,
            HashSet::from([user_acp_profile.id.clone(), user_model_profile.id.clone()])
        );

        let global_visible_ids = service
            .list_profiles()
            .unwrap()
            .into_iter()
            .map(|profile| profile.id)
            .collect::<HashSet<_>>();
        assert_eq!(global_visible_ids, visible_ids);

        let hidden_ids = HashSet::from([
            runtime_profile.id.clone(),
            legacy_runtime_profile.id.clone(),
        ]);
        assert!(
            service
                .list_health_summaries()
                .unwrap()
                .iter()
                .all(|summary| !hidden_ids.contains(&summary.profile.id))
        );
        assert!(
            service
                .list_capability_summaries()
                .unwrap()
                .iter()
                .all(|summary| !hidden_ids.contains(&summary.profile.id))
        );
        assert!(
            service
                .list_usage_summaries(ProviderUsageListRequest {
                    provider_profile_ids: None,
                    include_empty: true,
                })
                .unwrap()
                .iter()
                .all(|summary| !hidden_ids.contains(&summary.profile.id))
        );

        assert!(
            service
                .run_health_probes(ProviderRunHealthProbesRequest {
                    provider_profile_ids: Some(vec![runtime_profile.id.clone()]),
                    probe_kinds: Some(vec![ProviderHealthProbeKind::AuthStatus]),
                })
                .unwrap()
                .results
                .is_empty()
        );
        assert!(
            service
                .run_capability_probes(ProviderRunCapabilityProbesRequest {
                    provider_profile_ids: Some(vec![legacy_runtime_profile.id]),
                    force_refresh: true,
                })
                .unwrap()
                .results
                .is_empty()
        );
    }

    #[test]
    fn health_probe_is_provider_free_and_redacted() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex missing secret".to_string(),
                account_alias: None,
                base_url: Some("https://api.openai.invalid/v1".to_string()),
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: vec![placeholder_secret(
                    ProviderSecretKind::ApiKey,
                    "OPENAI_API_KEY",
                    "OpenAI API key",
                )],
            })
            .unwrap();

        let result = service
            .run_health_probes(ProviderRunHealthProbesRequest {
                provider_profile_ids: Some(vec![profile.id.clone()]),
                probe_kinds: Some(vec![ProviderHealthProbeKind::AuthStatus]),
            })
            .unwrap();

        assert_eq!(result.results.len(), 1);
        assert_eq!(result.results[0].status, ProviderHealthStatus::Fail);
        assert!(!format!("{result:?}").contains("sk-"));
        assert!(
            service
                .list_health_summaries()
                .unwrap()
                .iter()
                .any(|summary| {
                    summary.profile.id == profile.id
                        && summary.overall_status == ProviderHealthStatus::Fail
                })
        );
    }

    #[test]
    fn usage_summary_reads_separate_provider_usage_records() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let codex_profile = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex usage profile".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();
        let conn = service.open_connection().unwrap();
        let now = unix_timestamp_ms();
        ProviderUsageRepository::insert(
            &conn,
            &ProviderUsageRecord {
                usage_record_id: RequestId::new(),
                provider_profile_id: codex_profile.id.clone(),
                provider_kind: ProviderKind::Codex,
                source: "test".to_string(),
                unit: ProviderUsageUnit::Percent,
                label: "Codex quota".to_string(),
                used: Some(25.0),
                limit_value: Some(100.0),
                remaining: Some(75.0),
                window: Some(ProviderUsageWindow {
                    label: "daily".to_string(),
                    started_at_ms: None,
                    ends_at_ms: None,
                }),
                recorded_at_ms: now,
                metadata: Vec::new(),
            },
        )
        .unwrap();

        let summaries = service
            .list_usage_summaries(ProviderUsageListRequest {
                provider_profile_ids: Some(vec![codex_profile.id.clone()]),
                include_empty: true,
            })
            .unwrap();
        let summary = summaries
            .iter()
            .find(|summary| summary.profile.id == codex_profile.id)
            .expect("the user-created profile should remain visible in usage summaries");
        assert_eq!(summary.balances[0].label, "Codex quota");
        assert_eq!(summary.balances[0].remaining, Some(75.0));
    }

    #[test]
    fn failover_recommendation_is_advisory_only() {
        let dir = tempdir().unwrap();
        let service = ProviderConfigService::new(dir.path().join("vibex.db"));
        let source = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex source".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();
        let candidate = service
            .create_profile(ProviderProfileCreateRequest {
                agent_id: None,
                kind: ProviderKind::Codex,
                display_name: "Codex candidate".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("gpt-5-codex".to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap();

        let conn = service.open_connection().unwrap();
        let now = unix_timestamp_ms();
        for (profile, status) in [
            (&source, ProviderHealthStatus::Fail),
            (&candidate, ProviderHealthStatus::Pass),
        ] {
            ProviderHealthRepository::insert(
                &conn,
                &ProviderHealthProbeResult {
                    health_record_id: RequestId::new(),
                    provider_profile_id: profile.id.clone(),
                    provider_kind: profile.kind,
                    probe_kind: ProviderHealthProbeKind::AuthStatus,
                    status,
                    summary: "test signal".to_string(),
                    latency_ms: Some(0),
                    checked_at_ms: now,
                    expires_at_ms: Some(now + 60_000),
                    diagnostics: Vec::new(),
                },
            )
            .unwrap();
        }

        let recommendations = service
            .list_failover_recommendations(ProviderFailoverRecommendationRequest {
                provider_profile_ids: Some(vec![source.id.clone()]),
                max_candidates_per_profile: Some(1),
            })
            .unwrap();
        assert_eq!(
            recommendations[0].status,
            ProviderFailoverRecommendationStatus::Recommended
        );
        assert_eq!(
            recommendations[0]
                .candidate_profile
                .as_ref()
                .map(|profile| profile.id.clone()),
            Some(candidate.id)
        );
    }
}

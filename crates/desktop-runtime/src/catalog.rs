use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use async_trait::async_trait;
use vibex_agent::AgentManager;
use vibex_agent_acp::{
    RuntimeOptionCatalogProfileEvidence, SessionModelCatalogEntry, SessionModelCatalogSource,
    build_runtime_option_catalog, fallback_reasoning_efforts, fallback_session_modes,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentId, AgentListRequest, AgentModelListResponse, AgentModelListSource, AgentReasoningEffort,
    AgentSessionConfigProbe, ProviderKind, ProviderProfile, ProviderProfileId,
    ProviderProfileStatus, ProviderSessionConfigValue, SessionRuntimeOptionCatalog, VibexError,
};
use vibex_db::{
    ProviderProfileRepository, ProviderRuntimeOptionSnapshotRecord,
    ProviderRuntimeOptionSnapshotRepository, apply_migrations, open_database,
};
use vibex_remote::RemoteRuntimeOptionCatalogSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptionSnapshotSummary {
    pub provider_profile_id: ProviderProfileId,
    pub agent_id: AgentId,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOptionRefreshResult {
    pub refreshed_profile_ids: Vec<ProviderProfileId>,
    pub failed_profile_ids: Vec<ProviderProfileId>,
}

#[derive(Clone)]
pub struct RuntimeOptionCatalogService {
    manager: Arc<AgentManager>,
    provider_config: ProviderConfigService,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

impl RuntimeOptionCatalogService {
    pub fn new(manager: Arc<AgentManager>, provider_config: ProviderConfigService) -> Self {
        Self {
            manager,
            provider_config,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn list(&self) -> Result<SessionRuntimeOptionCatalog, VibexError> {
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let profiles = self.provider_config.list_profiles()?;
        let snapshots = self.snapshot_map()?;
        let mut evidence = BTreeMap::new();

        for profile in &profiles {
            if profile.kind != ProviderKind::Acp || profile.status != ProviderProfileStatus::Enabled
            {
                continue;
            }
            let snapshot = snapshots.get(&profile.id);
            let response = snapshot.and_then(|snapshot| snapshot.model_response.as_ref());
            let session_probe = snapshot.and_then(|snapshot| snapshot.session_config.as_ref());
            let probe_efforts = session_probe
                .map(|probe| probe.reasoning_efforts.as_slice())
                .unwrap_or_default();
            let fallback_efforts = fallback_reasoning_efforts(&profile.agent_id);
            let model_capabilities = response
                .into_iter()
                .flat_map(|response| response.model_capabilities.iter())
                .map(|capability| (capability.model.as_str(), capability))
                .collect::<HashMap<_, _>>();
            let configured_models = configured_model_ids(profile);
            let model_ids = if has_explicit_model_configuration(profile) {
                configured_models
            } else {
                response
                    .map(|response| response.models.clone())
                    .unwrap_or_default()
            };
            let models = model_ids
                .iter()
                .map(|model| {
                    let capability = model_capabilities.get(model.as_str()).copied();
                    SessionModelCatalogEntry {
                        model_id: model.clone(),
                        reasoning_efforts: first_non_empty_efforts([
                            capability
                                .map(|value| value.reasoning_efforts.as_slice())
                                .unwrap_or_default(),
                            response
                                .map(|value| value.reasoning_efforts.as_slice())
                                .unwrap_or_default(),
                            probe_efforts,
                            &fallback_efforts,
                        ]),
                        default_reasoning_effort: capability
                            .and_then(|value| value.default_reasoning_effort.clone()),
                        source: match response.map(|value| &value.source) {
                            Some(AgentModelListSource::Configured) => {
                                SessionModelCatalogSource::Profile
                            }
                            _ => SessionModelCatalogSource::Probe,
                        },
                    }
                })
                .collect();
            let configured_modes = self
                .provider_config
                .get_acp_profile_config(profile.id.clone())
                .map(|config| config.modes)
                .unwrap_or_default();
            let modes = merged_session_modes(
                session_probe
                    .map(|probe| probe.modes.clone())
                    .unwrap_or_default(),
                configured_modes,
                fallback_session_modes(&profile.agent_id),
            );
            let options = session_probe
                .map(|probe| probe.options.clone())
                .unwrap_or_default();
            evidence.insert(
                profile.id.clone(),
                RuntimeOptionCatalogProfileEvidence {
                    models,
                    modes,
                    options,
                    temporarily_unavailable: snapshot.is_some_and(|snapshot| {
                        snapshot.last_success_at_ms.is_none() && snapshot.last_error_code.is_some()
                    }) || response.is_some_and(|response| {
                        response.source == AgentModelListSource::Unavailable
                    }),
                },
            );
        }

        Ok(build_runtime_option_catalog(
            &agents.agents,
            &profiles
                .iter()
                .map(vibex_core::ProviderProfile::summary)
                .collect::<Vec<_>>(),
            &evidence,
        ))
    }

    /// Refreshes only Profiles belonging to one enabled Agent. This is the
    /// explicit slow path used by Agent setup and the manual configuration
    /// center action; ordinary Catalog reads never call an Agent process.
    pub async fn refresh_agent(
        &self,
        agent_id: &AgentId,
    ) -> Result<RuntimeOptionRefreshResult, VibexError> {
        let profiles = self.provider_config.list_profiles()?;
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: false,
        })?;
        if !agents
            .agents
            .iter()
            .any(|agent| agent.id == *agent_id && agent.added && agent.enabled)
        {
            return Ok(RuntimeOptionRefreshResult::default());
        }
        let profile_ids = profiles
            .iter()
            .filter(|profile| {
                profile.kind == ProviderKind::Acp
                    && profile.status == ProviderProfileStatus::Enabled
                    && profile.agent_id == *agent_id
            })
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        self.refresh_profile_ids(profile_ids).await
    }

    /// Refreshes one changed Profile without probing every configuration owned
    /// by the same Agent.
    pub async fn refresh_profile(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> Result<RuntimeOptionRefreshResult, VibexError> {
        let Some(profile) = self.provider_config.get_profile(provider_profile_id)? else {
            return Ok(RuntimeOptionRefreshResult::default());
        };
        if profile.kind != ProviderKind::Acp || profile.status != ProviderProfileStatus::Enabled {
            return Ok(RuntimeOptionRefreshResult::default());
        }
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: false,
        })?;
        if !agents
            .agents
            .iter()
            .any(|agent| agent.id == profile.agent_id && agent.added && agent.enabled)
        {
            return Ok(RuntimeOptionRefreshResult::default());
        }
        self.refresh_profile_ids(vec![provider_profile_id.clone()])
            .await
    }

    /// Performs the one-time bootstrap for enabled ACP Profiles that have no
    /// snapshot row yet. A failed attempt is persisted, so application starts
    /// do not repeatedly block on an unavailable local Agent.
    pub async fn refresh_missing(&self) -> Result<RuntimeOptionRefreshResult, VibexError> {
        let profiles = self.provider_config.list_profiles()?;
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: false,
        })?;
        let snapshots = self.snapshot_map()?;
        let profile_ids = profiles
            .iter()
            .filter(|profile| {
                profile.kind == ProviderKind::Acp
                    && profile.status == ProviderProfileStatus::Enabled
                    && agents
                        .agents
                        .iter()
                        .any(|agent| agent.id == profile.agent_id && agent.added && agent.enabled)
                    && !snapshots.contains_key(&profile.id)
            })
            .map(|profile| profile.id.clone())
            .collect::<Vec<_>>();
        self.refresh_profile_ids(profile_ids).await
    }

    pub fn invalidate_profile_snapshot(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> Result<(), VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        ProviderRuntimeOptionSnapshotRepository::delete(&connection, provider_profile_id)
    }

    pub fn snapshot_summaries(&self) -> Result<Vec<RuntimeOptionSnapshotSummary>, VibexError> {
        Ok(self
            .snapshot_records()?
            .into_iter()
            .map(|record| RuntimeOptionSnapshotSummary {
                provider_profile_id: record.provider_profile_id,
                agent_id: record.agent_id,
                last_success_at_ms: record.last_success_at_ms,
                last_attempt_at_ms: record.last_attempt_at_ms,
                last_error_code: record.last_error_code,
            })
            .collect())
    }

    async fn refresh_profile_ids(
        &self,
        profile_ids: Vec<ProviderProfileId>,
    ) -> Result<RuntimeOptionRefreshResult, VibexError> {
        let _refresh_guard = self.refresh_lock.lock().await;
        let mut result = RuntimeOptionRefreshResult::default();
        for profile_id in profile_ids {
            let Some(profile) = self.provider_config.get_profile(&profile_id)? else {
                continue;
            };
            if profile.kind != ProviderKind::Acp || profile.status != ProviderProfileStatus::Enabled
            {
                continue;
            }
            let attempted_at_ms = vibex_core::unix_timestamp_ms();
            let session_probe = self
                .manager
                .probe_session_config(profile.agent_id.clone(), profile.id.clone())
                .await;
            let (model_response, session_config) = match session_probe {
                Ok(probe) => {
                    let has_configured_models = !configured_model_ids(&profile).is_empty();
                    let response = if has_configured_models {
                        self.manager
                            .list_models(vibex_core::AgentModelListRequest {
                                agent_id: Some(profile.agent_id.clone()),
                                provider_profile_id: Some(profile.id.clone()),
                                session_id: None,
                            })
                            .await
                            .unwrap_or_else(|_| configured_model_response(&profile))
                    } else {
                        probed_model_response(&profile, &probe)
                    };
                    (response, probe)
                }
                Err(error) => {
                    if self.record_snapshot_failure_if_current(
                        &profile,
                        attempted_at_ms,
                        &error.code,
                    )? {
                        result.failed_profile_ids.push(profile.id.clone());
                    }
                    continue;
                }
            };
            let record = ProviderRuntimeOptionSnapshotRecord {
                provider_profile_id: profile.id.clone(),
                agent_id: profile.agent_id.clone(),
                model_response: Some(model_response),
                session_config: Some(session_config),
                last_success_at_ms: Some(attempted_at_ms),
                last_attempt_at_ms: attempted_at_ms,
                last_error_code: None,
            };
            if self.persist_snapshot_success_if_current(&profile, &record)? {
                result.refreshed_profile_ids.push(profile.id.clone());
            }
        }
        Ok(result)
    }

    fn snapshot_records(&self) -> Result<Vec<ProviderRuntimeOptionSnapshotRecord>, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        ProviderRuntimeOptionSnapshotRepository::list(&connection)
    }

    fn snapshot_map(
        &self,
    ) -> Result<BTreeMap<ProviderProfileId, ProviderRuntimeOptionSnapshotRecord>, VibexError> {
        Ok(self
            .snapshot_records()?
            .into_iter()
            .map(|record| (record.provider_profile_id.clone(), record))
            .collect())
    }

    #[cfg(test)]
    fn persist_snapshot_success(
        &self,
        record: &ProviderRuntimeOptionSnapshotRecord,
    ) -> Result<(), VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        ProviderRuntimeOptionSnapshotRepository::upsert_success(&connection, record)
    }

    fn persist_snapshot_success_if_current(
        &self,
        expected: &ProviderProfile,
        record: &ProviderRuntimeOptionSnapshotRecord,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "runtime_option_snapshot_transaction_failed",
                "failed to begin runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if ProviderProfileRepository::get(&transaction, &expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        ProviderRuntimeOptionSnapshotRepository::upsert_success(&transaction, record)?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "runtime_option_snapshot_commit_failed",
                "failed to commit runtime option snapshot",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
    }

    fn record_snapshot_failure_if_current(
        &self,
        expected: &ProviderProfile,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "runtime_option_snapshot_transaction_failed",
                "failed to begin runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if ProviderProfileRepository::get(&transaction, &expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        ProviderRuntimeOptionSnapshotRepository::record_failure(
            &transaction,
            &expected.id,
            &expected.agent_id,
            attempted_at_ms,
            error_code,
        )?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "runtime_option_snapshot_commit_failed",
                "failed to commit runtime option snapshot",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
    }
}

fn configured_model_ids(profile: &ProviderProfile) -> Vec<String> {
    let mut models = profile
        .configured_models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| model.id.trim().to_string())
        .filter(|model| !model.is_empty())
        .collect::<Vec<_>>();
    if models.is_empty()
        && let Some(model) = profile
            .default_model
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
    {
        models.push(model.to_string());
    }
    models.sort();
    models.dedup();
    models
}

fn has_explicit_model_configuration(profile: &ProviderProfile) -> bool {
    !profile.configured_models.is_empty()
        || profile
            .default_model
            .as_deref()
            .is_some_and(|model| !model.trim().is_empty())
}

fn configured_model_response(profile: &ProviderProfile) -> AgentModelListResponse {
    AgentModelListResponse {
        agent_id: Some(profile.agent_id.clone()),
        provider_kind: ProviderKind::Acp,
        provider_profile_id: Some(profile.id.clone()),
        models: configured_model_ids(profile),
        reasoning_efforts: Vec::new(),
        model_capabilities: Vec::new(),
        source: AgentModelListSource::Configured,
        diagnostics: Vec::new(),
    }
}

fn probed_model_response(
    profile: &ProviderProfile,
    probe: &AgentSessionConfigProbe,
) -> AgentModelListResponse {
    AgentModelListResponse {
        agent_id: Some(profile.agent_id.clone()),
        provider_kind: ProviderKind::Acp,
        provider_profile_id: Some(profile.id.clone()),
        models: probe.models.clone(),
        reasoning_efforts: probe.reasoning_efforts.clone(),
        model_capabilities: Vec::new(),
        source: AgentModelListSource::Probed,
        diagnostics: Vec::new(),
    }
}

/// Effort evidence priority: per-model capability, response-level discovery,
/// live session probe, then the registry fallback for the agent.
fn first_non_empty_efforts(candidates: [&[AgentReasoningEffort]; 4]) -> Vec<AgentReasoningEffort> {
    candidates
        .into_iter()
        .find(|candidate| !candidate.is_empty())
        .map(<[AgentReasoningEffort]>::to_vec)
        .unwrap_or_default()
}

/// Probe evidence first so live labels win the value-level dedup; statically
/// configured mode ids stay authoritative additions. The registry fallback
/// applies only when both live and configured sources are empty.
fn merged_session_modes(
    probe_modes: Vec<ProviderSessionConfigValue>,
    configured_modes: Vec<String>,
    fallback: Vec<ProviderSessionConfigValue>,
) -> Vec<ProviderSessionConfigValue> {
    let mut modes = probe_modes;
    for value in configured_modes {
        if !modes.iter().any(|mode| mode.value == value) {
            modes.push(ProviderSessionConfigValue { value, label: None });
        }
    }
    if modes.is_empty() { fallback } else { modes }
}

#[async_trait]
impl RemoteRuntimeOptionCatalogSource for RuntimeOptionCatalogService {
    async fn list_runtime_options(&self) -> Result<SessionRuntimeOptionCatalog, VibexError> {
        self.list().await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;
    use vibex_core::{
        AgentCommandConfig, AgentModelProviderProfileCreateRequest, AgentUpdateConfigRequest,
        ProviderConfiguredModel, ProviderSessionConfigOption, ProviderSessionConfigOptionKind,
    };

    fn catalog_fixture() -> (
        TempDir,
        RuntimeOptionCatalogService,
        ProviderConfigService,
        AgentId,
    ) {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("vibex.db");
        let provider_config = ProviderConfigService::new(&database_path);
        let agent_id = AgentId::parse("opencode").unwrap();
        provider_config
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: Some(AgentCommandConfig {
                    command: "/bin/true".to_string(),
                    args: Vec::new(),
                }),
                env: None,
                params: None,
            })
            .unwrap();
        let manager = Arc::new(AgentManager::new(&database_path).unwrap());
        let catalog = RuntimeOptionCatalogService::new(manager, provider_config.clone());
        (directory, catalog, provider_config, agent_id)
    }

    fn create_profile(
        provider_config: &ProviderConfigService,
        agent_id: &AgentId,
        label: &str,
        configured_models: Vec<ProviderConfiguredModel>,
    ) -> ProviderProfile {
        provider_config
            .create_agent_model_provider_profile(AgentModelProviderProfileCreateRequest {
                agent_id: agent_id.clone(),
                display_name: label.to_string(),
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap()
    }

    fn mode(value: &str, label: Option<&str>) -> ProviderSessionConfigValue {
        ProviderSessionConfigValue {
            value: value.to_string(),
            label: label.map(ToString::to_string),
        }
    }

    fn effort(value: &str) -> AgentReasoningEffort {
        AgentReasoningEffort {
            value: value.to_string(),
            description: None,
        }
    }

    #[test]
    fn probe_modes_keep_labels_and_merge_with_configured_ids() {
        let merged = merged_session_modes(
            vec![
                mode("default", Some("Manual")),
                mode("plan", Some("Plan Mode")),
            ],
            vec!["plan".to_string(), "acceptEdits".to_string()],
            vec![mode("fallback", None)],
        );
        assert_eq!(
            merged
                .iter()
                .map(|entry| (entry.value.as_str(), entry.label.as_deref()))
                .collect::<Vec<_>>(),
            vec![
                ("default", Some("Manual")),
                ("plan", Some("Plan Mode")),
                ("acceptEdits", None),
            ]
        );
    }

    #[test]
    fn mode_fallback_applies_only_when_probe_and_config_are_empty() {
        let merged = merged_session_modes(
            Vec::new(),
            Vec::new(),
            vec![mode("default", Some("Manual"))],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, "default");

        let merged = merged_session_modes(
            Vec::new(),
            vec!["read-only".to_string()],
            vec![mode("default", Some("Manual"))],
        );
        assert_eq!(merged.len(), 1);
        assert_eq!(merged[0].value, "read-only");
    }

    #[test]
    fn effort_evidence_priority_prefers_capability_then_probe_then_fallback() {
        let capability = vec![effort("high")];
        let probe = vec![effort("low"), effort("medium")];
        let fallback = vec![effort("max")];

        assert_eq!(
            first_non_empty_efforts([&capability, &[], &probe, &fallback]),
            capability
        );
        assert_eq!(
            first_non_empty_efforts([&[], &[], &probe, &fallback]),
            probe
        );
        assert_eq!(
            first_non_empty_efforts([&[], &[], &[], &fallback]),
            fallback
        );
        assert!(first_non_empty_efforts([&[], &[], &[], &[]]).is_empty());
    }

    #[tokio::test]
    async fn list_uses_persisted_snapshot_and_never_requires_a_runtime_probe() {
        let (_directory, catalog, provider_config, agent_id) = catalog_fixture();
        let dynamic_profile =
            create_profile(&provider_config, &agent_id, "Dynamic options", Vec::new());
        let configured_profile = create_profile(
            &provider_config,
            &agent_id,
            "Configured fallback",
            vec![ProviderConfiguredModel {
                id: "configured-model".to_string(),
                display_name: Some("Configured model".to_string()),
                enabled: true,
                wire_api: None,
            }],
        );
        let probed_value = ProviderSessionConfigValue {
            value: "true".to_string(),
            label: Some("Enabled".to_string()),
        };
        let session_config = AgentSessionConfigProbe {
            models: vec!["dynamic-model".to_string()],
            modes: vec![mode("plan", Some("Plan mode"))],
            reasoning_efforts: vec![effort("high")],
            options: vec![ProviderSessionConfigOption {
                id: "auto_approve".to_string(),
                label: "Auto approve".to_string(),
                category: Some("features".to_string()),
                description: None,
                kind: ProviderSessionConfigOptionKind::Boolean,
                current_value: Some(probed_value.clone()),
                default_value: Some(probed_value),
                values: Vec::new(),
            }],
        };
        let record = ProviderRuntimeOptionSnapshotRecord {
            provider_profile_id: dynamic_profile.id.clone(),
            agent_id: agent_id.clone(),
            model_response: Some(probed_model_response(&dynamic_profile, &session_config)),
            session_config: Some(session_config),
            last_success_at_ms: Some(100),
            last_attempt_at_ms: 100,
            last_error_code: None,
        };
        catalog.persist_snapshot_success(&record).unwrap();
        let stale_config = AgentSessionConfigProbe {
            models: vec!["stale-probed-model".to_string()],
            modes: Vec::new(),
            reasoning_efforts: vec![effort("high")],
            options: Vec::new(),
        };
        catalog
            .persist_snapshot_success(&ProviderRuntimeOptionSnapshotRecord {
                provider_profile_id: configured_profile.id.clone(),
                agent_id: agent_id.clone(),
                model_response: Some(probed_model_response(&configured_profile, &stale_config)),
                session_config: Some(stale_config),
                last_success_at_ms: Some(90),
                last_attempt_at_ms: 90,
                last_error_code: None,
            })
            .unwrap();

        // The fixture intentionally registers no Agent runtime. A live probe
        // from list() would fail, while persisted evidence remains available.
        let options = catalog.list().await.unwrap().options;
        let dynamic = options
            .iter()
            .find(|option| option.selection.provider_profile_id == dynamic_profile.id)
            .unwrap();
        assert_eq!(dynamic.selection.model_id, "dynamic-model");
        assert!(
            dynamic
                .reasoning_efforts
                .iter()
                .any(|effort| effort.value == "high")
        );
        assert!(dynamic.modes.iter().any(|mode| mode.value == "plan"));
        assert!(
            dynamic
                .features
                .iter()
                .any(|feature| feature.id == "auto_approve")
        );

        let configured = options
            .iter()
            .find(|option| option.selection.provider_profile_id == configured_profile.id)
            .unwrap();
        assert_eq!(configured.selection.model_id, "configured-model");
        assert!(
            configured
                .reasoning_efforts
                .iter()
                .any(|effort| effort.value == "high")
        );
        assert!(configured.features.is_empty());
    }

    #[tokio::test]
    async fn targeted_refresh_only_probes_and_invalidates_requested_profile() {
        let (_directory, catalog, provider_config, agent_id) = catalog_fixture();
        let first = create_profile(&provider_config, &agent_id, "First", Vec::new());
        let second = create_profile(&provider_config, &agent_id, "Second", Vec::new());

        let result = catalog.refresh_profile(&first.id).await.unwrap();

        assert!(
            result
                .failed_profile_ids
                .iter()
                .any(|provider_profile_id| provider_profile_id == &first.id)
        );
        assert!(
            result
                .failed_profile_ids
                .iter()
                .all(|provider_profile_id| provider_profile_id != &second.id)
        );
        let summaries = catalog.snapshot_summaries().unwrap();
        assert!(
            summaries
                .iter()
                .any(|summary| summary.provider_profile_id == first.id)
        );
        assert!(
            summaries
                .iter()
                .all(|summary| summary.provider_profile_id != second.id)
        );

        catalog.invalidate_profile_snapshot(&first.id).unwrap();
        assert!(
            catalog
                .snapshot_summaries()
                .unwrap()
                .iter()
                .all(|summary| summary.provider_profile_id != first.id)
        );
    }

    #[tokio::test]
    async fn failed_missing_snapshot_is_attempted_only_once() {
        let (_directory, catalog, provider_config, agent_id) = catalog_fixture();
        let profile = create_profile(&provider_config, &agent_id, "Unavailable", Vec::new());

        let first = catalog.refresh_missing().await.unwrap();
        assert!(
            first
                .failed_profile_ids
                .iter()
                .any(|profile_id| profile_id == &profile.id)
        );
        let first_attempt = catalog
            .snapshot_summaries()
            .unwrap()
            .into_iter()
            .find(|summary| summary.provider_profile_id == profile.id)
            .unwrap();
        assert!(first_attempt.last_success_at_ms.is_none());
        assert!(first_attempt.last_error_code.is_some());

        let second = catalog.refresh_missing().await.unwrap();
        assert!(second.refreshed_profile_ids.is_empty());
        assert!(second.failed_profile_ids.is_empty());
        let second_attempt = catalog
            .snapshot_summaries()
            .unwrap()
            .into_iter()
            .find(|summary| summary.provider_profile_id == profile.id)
            .unwrap();
        assert_eq!(second_attempt, first_attempt);
    }
}

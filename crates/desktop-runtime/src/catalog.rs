use std::collections::BTreeMap;
use std::sync::Arc;

use async_trait::async_trait;
use vibex_agent::AgentManager;
use vibex_agent_acp::{
    AcpRuntimeClient, RuntimeOptionCatalogProfileEvidence, build_runtime_option_catalog,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{AgentId, AgentListRequest, SessionRuntimeOptionCatalog, VibexError};
use vibex_db::{
    AgentConfigRepository, AgentRuntimeOptionSnapshotRecord, AgentRuntimeOptionSnapshotRepository,
    apply_migrations, open_database,
};
use vibex_remote::RemoteRuntimeOptionCatalogSource;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeOptionSnapshotSummary {
    pub agent_id: AgentId,
    pub last_success_at_ms: Option<i64>,
    pub last_attempt_at_ms: i64,
    pub last_error_code: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct RuntimeOptionProbeResult {
    pub probed_agent_ids: Vec<AgentId>,
    pub failed_agent_ids: Vec<AgentId>,
    pub cached_agent_ids: Vec<AgentId>,
}

#[derive(Clone)]
pub struct RuntimeOptionCatalogService {
    manager: Arc<AgentManager>,
    provider_config: ProviderConfigService,
    live_runtime: Option<Arc<AcpRuntimeClient>>,
    probe_lock: Arc<tokio::sync::Mutex<()>>,
}

impl RuntimeOptionCatalogService {
    pub fn new(manager: Arc<AgentManager>, provider_config: ProviderConfigService) -> Self {
        Self {
            manager,
            provider_config,
            live_runtime: None,
            probe_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub fn with_live_runtime(
        manager: Arc<AgentManager>,
        provider_config: ProviderConfigService,
        live_runtime: Arc<AcpRuntimeClient>,
    ) -> Self {
        Self {
            manager,
            provider_config,
            live_runtime: Some(live_runtime),
            probe_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn list(&self) -> Result<SessionRuntimeOptionCatalog, VibexError> {
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let profiles = self.provider_config.list_profiles()?;
        let snapshots = self.snapshot_map()?;
        let mut fallback_by_agent = BTreeMap::new();

        for agent in &agents.agents {
            if !agent.added || !agent.enabled {
                continue;
            }
            let snapshot = snapshots.get(&agent.id);
            let session_probe = snapshot.and_then(|snapshot| snapshot.session_config.as_ref());
            fallback_by_agent.insert(
                agent.id.clone(),
                RuntimeOptionCatalogProfileEvidence {
                    models: Vec::new(),
                    modes: session_probe
                        .map(|probe| probe.modes.clone())
                        .unwrap_or_default(),
                    reasoning_efforts: session_probe
                        .map(|probe| probe.reasoning_efforts.clone())
                        .unwrap_or_default(),
                    options: session_probe
                        .map(|probe| probe.options.clone())
                        .unwrap_or_default(),
                    temporarily_unavailable: snapshot.is_some_and(|snapshot| {
                        snapshot.last_success_at_ms.is_none() && snapshot.last_error_code.is_some()
                    }),
                },
            );
        }

        // Fast startup uses the persisted Agent snapshot. As soon as a real
        // session has opened, its Profile-scoped evidence replaces that
        // fallback and ConfigOptionUpdate keeps it current.
        let live_by_profile = self
            .live_runtime
            .as_ref()
            .map(|runtime| runtime.profile_session_config_evidence())
            .transpose()?
            .unwrap_or_default();
        let evidence_by_profile =
            layer_profile_session_evidence(&profiles, &fallback_by_agent, live_by_profile);

        Ok(build_runtime_option_catalog(
            &agents.agents,
            &profiles
                .iter()
                .map(vibex_core::ProviderProfile::summary)
                .collect::<Vec<_>>(),
            &evidence_by_profile,
        ))
    }

    /// Performs the one-time Agent-owned runtime option probe. A successful
    /// snapshot is immutable until the Agent is removed; adding or editing a
    /// Provider Profile never reaches this method.
    pub async fn probe_agent(
        &self,
        agent_id: &AgentId,
    ) -> Result<RuntimeOptionProbeResult, VibexError> {
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let Some(agent) = agents.agents.iter().find(|agent| agent.id == *agent_id) else {
            return Ok(RuntimeOptionProbeResult::default());
        };
        if !agent.added || !agent.enabled {
            return Ok(RuntimeOptionProbeResult::default());
        }
        if let Some(snapshot) = self.snapshot_map()?.get(agent_id)
            && snapshot.last_success_at_ms.is_some()
        {
            return Ok(RuntimeOptionProbeResult {
                cached_agent_ids: vec![agent_id.clone()],
                ..Default::default()
            });
        }

        let _probe_guard = self.probe_lock.lock().await;
        // Re-check after waiting for another setup probe to finish.
        if let Some(snapshot) = self.snapshot_map()?.get(agent_id)
            && snapshot.last_success_at_ms.is_some()
        {
            return Ok(RuntimeOptionProbeResult {
                cached_agent_ids: vec![agent_id.clone()],
                ..Default::default()
            });
        }
        let attempted_at_ms = vibex_core::unix_timestamp_ms();
        let mut session_config = match self
            .manager
            .probe_agent_session_config(agent_id.clone())
            .await
        {
            Ok(probe) => probe,
            Err(error) => {
                if self.record_agent_snapshot_failure_if_current(
                    agent_id,
                    agent.updated_at_ms.unwrap_or_default(),
                    attempted_at_ms,
                    &error.code,
                )? {
                    return Ok(RuntimeOptionProbeResult {
                        failed_agent_ids: vec![agent_id.clone()],
                        ..Default::default()
                    });
                }
                return Ok(RuntimeOptionProbeResult::default());
            }
        };
        // Provider model choices never cross the Agent-owned snapshot
        // boundary, even if an Adapter accidentally reports them here.
        session_config.models.clear();
        let record = AgentRuntimeOptionSnapshotRecord {
            agent_id: agent_id.clone(),
            session_config: Some(session_config),
            last_success_at_ms: Some(attempted_at_ms),
            last_attempt_at_ms: attempted_at_ms,
            last_error_code: None,
        };
        if self.persist_agent_snapshot_success_if_current(
            agent_id,
            agent.updated_at_ms.unwrap_or_default(),
            &record,
        )? {
            Ok(RuntimeOptionProbeResult {
                probed_agent_ids: vec![agent_id.clone()],
                ..Default::default()
            })
        } else {
            Ok(RuntimeOptionProbeResult::default())
        }
    }

    pub fn delete_agent_snapshot(&self, agent_id: &AgentId) -> Result<(), VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        AgentRuntimeOptionSnapshotRepository::delete(&connection, agent_id)
    }

    pub fn snapshot_summaries(&self) -> Result<Vec<RuntimeOptionSnapshotSummary>, VibexError> {
        Ok(self
            .snapshot_records()?
            .into_iter()
            .map(|record| RuntimeOptionSnapshotSummary {
                agent_id: record.agent_id,
                last_success_at_ms: record.last_success_at_ms,
                last_attempt_at_ms: record.last_attempt_at_ms,
                last_error_code: record.last_error_code,
            })
            .collect())
    }

    fn snapshot_records(&self) -> Result<Vec<AgentRuntimeOptionSnapshotRecord>, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        AgentRuntimeOptionSnapshotRepository::list(&connection)
    }

    fn snapshot_map(
        &self,
    ) -> Result<BTreeMap<AgentId, AgentRuntimeOptionSnapshotRecord>, VibexError> {
        Ok(self
            .snapshot_records()?
            .into_iter()
            .map(|record| (record.agent_id.clone(), record))
            .collect())
    }

    fn persist_agent_snapshot_success_if_current(
        &self,
        agent_id: &AgentId,
        expected_updated_at_ms: i64,
        record: &AgentRuntimeOptionSnapshotRecord,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "agent_runtime_option_snapshot_transaction_failed",
                "failed to begin Agent runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let current = AgentConfigRepository::get(&transaction, agent_id)?;
        if current
            .as_ref()
            .map(|config| config.updated_at_ms)
            .unwrap_or_default()
            != expected_updated_at_ms
        {
            return Ok(false);
        }
        AgentRuntimeOptionSnapshotRepository::upsert_success(&transaction, record)?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "agent_runtime_option_snapshot_commit_failed",
                "failed to commit Agent runtime option snapshot",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
    }

    fn record_agent_snapshot_failure_if_current(
        &self,
        agent_id: &AgentId,
        expected_updated_at_ms: i64,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "agent_runtime_option_snapshot_transaction_failed",
                "failed to begin Agent runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let current = AgentConfigRepository::get(&transaction, agent_id)?;
        if current
            .as_ref()
            .map(|config| config.updated_at_ms)
            .unwrap_or_default()
            != expected_updated_at_ms
        {
            return Ok(false);
        }
        AgentRuntimeOptionSnapshotRepository::record_failure(
            &transaction,
            agent_id,
            attempted_at_ms,
            error_code,
        )?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "agent_runtime_option_snapshot_commit_failed",
                "failed to commit Agent runtime option snapshot",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
    }
}

fn layer_profile_session_evidence(
    profiles: &[vibex_core::ProviderProfile],
    fallback_by_agent: &BTreeMap<AgentId, RuntimeOptionCatalogProfileEvidence>,
    live_by_profile: BTreeMap<vibex_core::ProviderProfileId, vibex_core::AgentSessionConfigProbe>,
) -> BTreeMap<vibex_core::ProviderProfileId, RuntimeOptionCatalogProfileEvidence> {
    let mut evidence_by_profile = profiles
        .iter()
        .filter_map(|profile| {
            fallback_by_agent
                .get(&profile.agent_id)
                .cloned()
                .map(|evidence| (profile.id.clone(), evidence))
        })
        .collect::<BTreeMap<_, _>>();
    for (profile_id, probe) in live_by_profile {
        evidence_by_profile.insert(
            profile_id,
            RuntimeOptionCatalogProfileEvidence {
                // Models are always owned by the Provider Profile. A live
                // Agent session calibrates only Agent-owned controls.
                models: Vec::new(),
                modes: probe.modes,
                reasoning_efforts: probe.reasoning_efforts,
                options: probe.options,
                temporarily_unavailable: false,
            },
        );
    }
    evidence_by_profile
}

#[async_trait]
impl RemoteRuntimeOptionCatalogSource for RuntimeOptionCatalogService {
    async fn list_runtime_options(&self) -> Result<SessionRuntimeOptionCatalog, VibexError> {
        self.list().await
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use tempfile::TempDir;
    use vibex_agent::{
        AgentProvider, ProviderCreateRequest, ProviderSessionHandle, ProviderTurnRequest,
        ProviderTurnResult,
    };
    use vibex_core::{
        AgentCommandConfig, AgentModelProviderProfileCreateRequest, AgentReasoningEffort,
        AgentRuntimeRouteKey, AgentSessionConfigProbe, AgentUpdateConfigRequest, ProviderBinding,
        ProviderCapabilities, ProviderConfiguredModel, ProviderProfile,
        ProviderSessionConfigOption, ProviderSessionConfigOptionKind, ProviderSessionConfigValue,
        TransportKind, VibexResult,
    };

    struct CountingProvider {
        calls: AtomicUsize,
        fail_probe: bool,
    }

    #[async_trait]
    impl AgentProvider for CountingProvider {
        fn kind(&self) -> vibex_core::ProviderKind {
            vibex_core::ProviderKind::Acp
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::conservative(
                vibex_core::ProviderKind::Acp,
                "runtime-option-catalog-test",
            )
        }

        async fn probe_agent_session_config(
            &self,
            _agent_id: &AgentId,
        ) -> VibexResult<AgentSessionConfigProbe> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            if self.fail_probe {
                return Err(VibexError::process(
                    "agent_option_probe_failed",
                    "fixture Agent option probe failed",
                ));
            }
            Ok(agent_session_config())
        }

        async fn create_session(
            &self,
            _request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("runtime option tests do not create sessions")
        }

        async fn resume_session(
            &self,
            _binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            unreachable!("runtime option tests do not resume sessions")
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            _request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            unreachable!("runtime option tests do not send turns")
        }
    }

    fn catalog_fixture(
        fail_probe: bool,
    ) -> (
        TempDir,
        RuntimeOptionCatalogService,
        ProviderConfigService,
        AgentId,
        Arc<CountingProvider>,
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
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            fail_probe,
        });
        let mut manager = AgentManager::new(&database_path).unwrap();
        manager
            .register_runtime(
                AgentRuntimeRouteKey {
                    agent_id: agent_id.clone(),
                    transport_kind: TransportKind::Acp,
                    adapter_id: vibex_core::default_acp_adapter_id(&agent_id),
                },
                provider.clone(),
            )
            .unwrap();
        let manager = Arc::new(manager);
        let catalog = RuntimeOptionCatalogService::new(manager, provider_config.clone());
        (directory, catalog, provider_config, agent_id, provider)
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

    fn configured_model(id: &str) -> ProviderConfiguredModel {
        ProviderConfiguredModel {
            id: id.to_string(),
            display_name: Some(format!("{id} label")),
            enabled: true,
            wire_api: None,
        }
    }

    fn agent_session_config() -> AgentSessionConfigProbe {
        let enabled = ProviderSessionConfigValue {
            value: "true".to_string(),
            label: Some("Enabled".to_string()),
        };
        AgentSessionConfigProbe {
            models: vec!["model-from-agent-must-not-be-used".to_string()],
            modes: vec![mode("plan", Some("Plan mode"))],
            reasoning_efforts: vec![effort("high")],
            options: vec![ProviderSessionConfigOption {
                id: "auto_approve".to_string(),
                label: "Auto approve".to_string(),
                category: Some("features".to_string()),
                description: None,
                kind: ProviderSessionConfigOptionKind::Boolean,
                current_value: Some(enabled.clone()),
                default_value: Some(enabled),
                values: Vec::new(),
            }],
        }
    }

    #[tokio::test]
    async fn one_agent_snapshot_is_shared_by_all_provider_profiles_and_reused() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(false);
        let first_profile = create_profile(
            &provider_config,
            &agent_id,
            "First profile",
            vec![configured_model("first-model")],
        );
        let second_profile = create_profile(
            &provider_config,
            &agent_id,
            "Second profile",
            vec![configured_model("second-model")],
        );

        let first_probe = catalog.probe_agent(&agent_id).await.unwrap();
        assert_eq!(first_probe.probed_agent_ids, vec![agent_id.clone()]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let snapshots = catalog.snapshot_records().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert!(
            snapshots[0]
                .session_config
                .as_ref()
                .is_some_and(|config| config.models.is_empty())
        );
        let cached_probe = catalog.probe_agent(&agent_id).await.unwrap();
        assert_eq!(cached_probe.cached_agent_ids, vec![agent_id.clone()]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let options = catalog.list().await.unwrap().options;
        let profile_options = options
            .iter()
            .filter(|option| {
                option.selection.provider_profile_id == first_profile.id
                    || option.selection.provider_profile_id == second_profile.id
            })
            .collect::<Vec<_>>();
        assert_eq!(profile_options.len(), 2);
        assert!(profile_options.iter().any(|option| {
            option.selection.provider_profile_id == first_profile.id
                && option.selection.model_id == "first-model"
        }));
        assert!(profile_options.iter().any(|option| {
            option.selection.provider_profile_id == second_profile.id
                && option.selection.model_id == "second-model"
        }));
        for option in profile_options {
            assert!(option.modes.iter().any(|mode| mode.value == "plan"));
            assert!(
                option
                    .reasoning_efforts
                    .iter()
                    .any(|effort| effort.value == "high")
            );
            assert!(
                option
                    .features
                    .iter()
                    .any(|feature| feature.id == "auto_approve")
            );
            assert_ne!(
                option.selection.model_id,
                "model-from-agent-must-not-be-used"
            );
        }

        let third_profile = create_profile(
            &provider_config,
            &agent_id,
            "Third profile",
            vec![configured_model("third-model")],
        );
        let options = catalog.list().await.unwrap().options;
        assert!(options.iter().any(|option| {
            option.selection.provider_profile_id == third_profile.id
                && option.modes.iter().any(|mode| mode.value == "plan")
        }));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.snapshot_summaries().unwrap().len(), 1);
    }

    #[test]
    fn live_profile_evidence_overrides_agent_fallback_without_overriding_models() {
        let (_directory, _catalog, provider_config, agent_id, _provider) = catalog_fixture(false);
        let first_profile = create_profile(
            &provider_config,
            &agent_id,
            "First profile",
            vec![configured_model("configured-first")],
        );
        let second_profile = create_profile(
            &provider_config,
            &agent_id,
            "Second profile",
            vec![configured_model("configured-second")],
        );
        let fallback = BTreeMap::from([(
            agent_id.clone(),
            RuntimeOptionCatalogProfileEvidence {
                models: Vec::new(),
                modes: vec![mode("plan", Some("Plan"))],
                reasoning_efforts: vec![effort("high")],
                options: Vec::new(),
                temporarily_unavailable: false,
            },
        )]);
        let live = BTreeMap::from([(
            first_profile.id.clone(),
            AgentSessionConfigProbe {
                models: vec!["agent-model-must-not-win".to_string()],
                modes: vec![mode("review", Some("Review"))],
                reasoning_efforts: vec![effort("low")],
                options: Vec::new(),
            },
        )]);
        let profiles = provider_config.list_profiles().unwrap();
        let layered = layer_profile_session_evidence(&profiles, &fallback, live);
        let first_evidence = layered.get(&first_profile.id).unwrap();
        assert!(first_evidence.models.is_empty());
        assert_eq!(first_evidence.modes[0].value, "review");
        assert_eq!(first_evidence.reasoning_efforts[0].value, "low");
        let second_evidence = layered.get(&second_profile.id).unwrap();
        assert_eq!(second_evidence.modes[0].value, "plan");
        assert_eq!(second_evidence.reasoning_efforts[0].value, "high");

        let agents = provider_config
            .list_agents(AgentListRequest {
                include_disabled: true,
            })
            .unwrap();
        let catalog = build_runtime_option_catalog(
            &agents.agents,
            &profiles
                .iter()
                .map(ProviderProfile::summary)
                .collect::<Vec<_>>(),
            &layered,
        );
        let first = catalog
            .options
            .iter()
            .find(|option| option.selection.provider_profile_id == first_profile.id)
            .unwrap();
        assert_eq!(first.selection.model_id, "configured-first");
        assert_eq!(first.modes[0].value, "review");
        assert_eq!(first.reasoning_efforts[0].value, "low");
        let second = catalog
            .options
            .iter()
            .find(|option| option.selection.provider_profile_id == second_profile.id)
            .unwrap();
        assert_eq!(second.selection.model_id, "configured-second");
        assert_eq!(second.modes[0].value, "plan");
        assert_eq!(second.reasoning_efforts[0].value, "high");
    }

    #[tokio::test]
    async fn agent_probe_does_not_require_a_provider_profile() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(false);
        for profile in provider_config.list_profiles().unwrap() {
            if profile.agent_id == agent_id
                && profile.id.as_str() != profile.kind.local_default_profile_id()
            {
                provider_config
                    .delete_profile(vibex_core::ProviderProfileDeleteRequest {
                        provider_profile_id: profile.id,
                    })
                    .unwrap();
            }
        }
        assert!(
            !provider_config
                .list_profiles()
                .unwrap()
                .iter()
                .any(|profile| {
                    profile.agent_id == agent_id
                        && profile.kind == vibex_core::ProviderKind::Acp
                        && profile.status == vibex_core::ProviderProfileStatus::Enabled
                })
        );

        let result = catalog.probe_agent(&agent_id).await.unwrap();

        assert_eq!(result.probed_agent_ids, vec![agent_id.clone()]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.snapshot_summaries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn provider_changes_never_probe_or_invalidate_agent_options() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(false);
        create_profile(
            &provider_config,
            &agent_id,
            "First",
            vec![configured_model("first-model")],
        );
        catalog.probe_agent(&agent_id).await.unwrap();
        let before = catalog.snapshot_summaries().unwrap();

        create_profile(
            &provider_config,
            &agent_id,
            "Second",
            vec![configured_model("second-model")],
        );
        let _ = catalog.list().await.unwrap();

        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.snapshot_summaries().unwrap(), before);
    }

    #[tokio::test]
    async fn failed_probe_is_recorded_by_agent_and_only_explicitly_retried() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(true);
        create_profile(
            &provider_config,
            &agent_id,
            "Unavailable",
            vec![configured_model("configured-model")],
        );

        let result = catalog.probe_agent(&agent_id).await.unwrap();
        assert_eq!(result.failed_agent_ids, vec![agent_id.clone()]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        let first_attempt = catalog.snapshot_summaries().unwrap();
        assert_eq!(first_attempt.len(), 1);
        assert_eq!(first_attempt[0].agent_id, agent_id);
        assert!(first_attempt[0].last_success_at_ms.is_none());
        assert_eq!(
            first_attempt[0].last_error_code.as_deref(),
            Some("agent_option_probe_failed")
        );

        let _ = catalog.list().await.unwrap();
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.snapshot_summaries().unwrap(), first_attempt);

        catalog.delete_agent_snapshot(&agent_id).unwrap();
        assert!(catalog.snapshot_summaries().unwrap().is_empty());
    }
}

use std::collections::BTreeMap;
use std::sync::{Arc, Weak};

use async_trait::async_trait;
use vibex_agent::AgentManager;
use vibex_agent_acp::{
    AcpRuntimeClient, RuntimeOptionCatalogProfileEvidence, SessionModelCatalogEntry,
    SessionModelCatalogSource, append_agent_account_runtime_options, build_runtime_option_catalog,
    refresh_runtime_option_catalog_revision,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AgentId, AgentListRequest, ProviderKind, ProviderProfile, ProviderProfileId,
    ProviderProfileStatus, SessionRuntimeOptionCatalog, VibexError,
};
use vibex_db::{
    AgentAuthModelCatalogRepository, AgentConfigRepository, AgentRuntimeOptionSnapshotRecord,
    AgentRuntimeOptionSnapshotRepository, ProviderModelRuntimeOptionSnapshotRecord,
    ProviderModelRuntimeOptionSnapshotRepository, ProviderProfileRepository, apply_migrations,
    open_database,
};
use vibex_remote::RemoteRuntimeOptionCatalogSource;

use crate::AgentAuthContextService;

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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ProviderModelRuntimeOptionKey {
    pub provider_profile_id: ProviderProfileId,
    pub model_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderModelRuntimeOptionProbeResult {
    pub probed_models: Vec<ProviderModelRuntimeOptionKey>,
    pub failed_models: Vec<ProviderModelRuntimeOptionKey>,
    pub cached_models: Vec<ProviderModelRuntimeOptionKey>,
}

#[derive(Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RuntimeOptionProbeKey {
    Agent(AgentId),
    Profile(ProviderProfileId),
}

#[derive(Clone)]
pub struct RuntimeOptionCatalogService {
    manager: Arc<AgentManager>,
    provider_config: ProviderConfigService,
    live_runtime: Option<Arc<AcpRuntimeClient>>,
    auth_contexts: Option<Arc<AgentAuthContextService>>,
    probe_locks:
        Arc<tokio::sync::Mutex<BTreeMap<RuntimeOptionProbeKey, Weak<tokio::sync::Mutex<()>>>>>,
}

impl RuntimeOptionCatalogService {
    pub fn new(manager: Arc<AgentManager>, provider_config: ProviderConfigService) -> Self {
        Self {
            manager,
            provider_config,
            live_runtime: None,
            auth_contexts: None,
            probe_locks: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
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
            auth_contexts: None,
            probe_locks: Arc::new(tokio::sync::Mutex::new(BTreeMap::new())),
        }
    }

    pub fn with_auth_context_service(
        mut self,
        auth_contexts: Arc<AgentAuthContextService>,
    ) -> Self {
        self.auth_contexts = Some(auth_contexts);
        self
    }

    pub async fn list(&self) -> Result<SessionRuntimeOptionCatalog, VibexError> {
        let mut agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        agents
            .agents
            .retain(|agent| vibex_core::is_user_visible_agent(&agent.id));
        let profiles = self.provider_config.list_runtime_profiles()?;
        let snapshots = self.snapshot_map()?;
        let model_snapshots = self.model_snapshot_records()?;
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
        let model_evidence_by_profile = model_snapshot_evidence(model_snapshots);
        let evidence_by_profile = layer_profile_session_evidence(
            &profiles,
            &fallback_by_agent,
            model_evidence_by_profile,
            live_by_profile,
        );

        let mut catalog = build_runtime_option_catalog(
            &agents.agents,
            &profiles
                .iter()
                .map(vibex_core::ProviderProfile::summary)
                .collect::<Vec<_>>(),
            &evidence_by_profile,
        );
        let account_fingerprints =
            self.merge_agent_account_sources(&mut catalog, &agents.agents)?;
        catalog.auth_sources.sort_by(|left, right| {
            left.agent_id
                .cmp(&right.agent_id)
                .then_with(|| left.kind.cmp(&right.kind))
                .then_with(|| left.label.cmp(&right.label))
                .then_with(|| left.source.cmp(&right.source))
        });
        catalog.options.sort_by(|left, right| {
            left.selection
                .agent_id
                .cmp(&right.selection.agent_id)
                .then_with(|| left.selection.auth_source.cmp(&right.selection.auth_source))
                .then_with(|| left.model_label.cmp(&right.model_label))
        });
        refresh_runtime_option_catalog_revision(
            &mut catalog,
            account_fingerprints.iter().map(String::as_bytes),
        );
        Ok(catalog)
    }

    fn merge_agent_account_sources(
        &self,
        catalog: &mut SessionRuntimeOptionCatalog,
        agents: &[vibex_core::AgentSnapshotEntry],
    ) -> Result<Vec<String>, VibexError> {
        let (Some(runtime), Some(auth_contexts)) =
            (self.live_runtime.as_ref(), self.auth_contexts.as_ref())
        else {
            return Ok(Vec::new());
        };
        let contexts = auth_contexts.list()?;
        let connection = open_database(self.provider_config.database_path())?;
        let snapshots = AgentAuthModelCatalogRepository::list_current(&connection, &contexts)?;
        let mut latest_snapshot_by_context = BTreeMap::new();
        for snapshot in snapshots {
            let replace = latest_snapshot_by_context
                .get(&snapshot.auth_context_id)
                .is_none_or(|current: &vibex_core::AgentAuthModelCatalogSnapshot| {
                    snapshot.last_attempt_at_ms > current.last_attempt_at_ms
                });
            if replace {
                latest_snapshot_by_context.insert(snapshot.auth_context_id.clone(), snapshot);
            }
        }
        let contexts_by_agent = contexts
            .iter()
            .map(|context| (context.agent_id.clone(), context))
            .collect::<BTreeMap<_, _>>();
        let mut fingerprints = Vec::new();
        for agent in agents.iter().filter(|agent| agent.added && agent.enabled) {
            if !runtime.supports_agent_account(&agent.id) {
                continue;
            }
            let Some(context) = contexts_by_agent.get(&agent.id).copied() else {
                continue;
            };
            let snapshot = latest_snapshot_by_context.get(&context.id);
            if let Some(snapshot) = snapshot {
                fingerprints.push(format!(
                    "{}:{}:{}",
                    context.id, context.revision, snapshot.runtime_fingerprint
                ));
            }
            append_agent_account_runtime_options(
                catalog,
                agent,
                context,
                snapshot,
                runtime.supports_agent_account_logout(&agent.id),
            );
        }
        fingerprints.sort();
        Ok(fingerprints)
    }

    /// Fills missing Agent-owned option snapshots after the runtime is ready.
    /// Successful snapshots are skipped, while failed or absent probes are retried.
    pub(crate) async fn probe_missing_enabled_agents(
        &self,
    ) -> Result<RuntimeOptionProbeResult, VibexError> {
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let snapshots = self.snapshot_map()?;
        let agent_ids = agents
            .agents
            .into_iter()
            .filter(|agent| {
                agent.added
                    && agent.enabled
                    && agent.installed
                    && snapshots
                        .get(&agent.id)
                        .is_none_or(|snapshot| !agent_snapshot_is_reusable(&agent.id, snapshot))
            })
            .map(|agent| agent.id)
            .collect::<Vec<_>>();
        let mut result = RuntimeOptionProbeResult::default();
        for agent_id in agent_ids {
            let probe = self.probe_agent(&agent_id).await?;
            result.probed_agent_ids.extend(probe.probed_agent_ids);
            result.failed_agent_ids.extend(probe.failed_agent_ids);
            result.cached_agent_ids.extend(probe.cached_agent_ids);
        }
        Ok(result)
    }

    /// Fills missing model-owned option snapshots after startup. Catalog reads
    /// remain process-free; a successful `(Profile, model)` cache is reused.
    pub(crate) async fn probe_missing_enabled_profile_models(
        &self,
    ) -> Result<ProviderModelRuntimeOptionProbeResult, VibexError> {
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        let snapshots = self.model_snapshot_map()?;
        let profile_ids = self
            .provider_config
            .list_runtime_profiles()?
            .into_iter()
            .filter(|profile| {
                profile.kind == ProviderKind::Acp
                    && profile.status == ProviderProfileStatus::Enabled
                    && agents.agents.iter().any(|agent| {
                        agent.id == profile.agent_id
                            && agent.added
                            && agent.enabled
                            && agent.installed
                    })
            })
            .filter(|profile| {
                let model_ids = configured_model_ids(profile);
                !model_ids.is_empty()
                    && (model_ids.iter().any(|model_id| {
                        snapshots
                            .get(&(profile.id.clone(), model_id.clone()))
                            .is_none_or(|snapshot| {
                                !model_snapshot_is_reusable(&profile.agent_id, snapshot)
                            })
                    }) || snapshots.keys().any(|(profile_id, model_id)| {
                        profile_id == &profile.id && !model_ids.contains(model_id)
                    }))
            })
            .map(|profile| profile.id)
            .collect::<Vec<_>>();
        let mut result = ProviderModelRuntimeOptionProbeResult::default();
        for profile_id in profile_ids {
            let probe = self.probe_profile_models(&profile_id).await?;
            result.probed_models.extend(probe.probed_models);
            result.failed_models.extend(probe.failed_models);
            result.cached_models.extend(probe.cached_models);
        }
        Ok(result)
    }

    /// Probes only models without a successful persistent cache. Removed
    /// models are cleaned up, while unchanged model ids keep their evidence.
    pub async fn probe_profile_models(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> Result<ProviderModelRuntimeOptionProbeResult, VibexError> {
        let _probe_guard = self
            .acquire_probe(RuntimeOptionProbeKey::Profile(provider_profile_id.clone()))
            .await;
        let Some(profile) = self.provider_config.get_profile(provider_profile_id)? else {
            self.delete_profile_model_snapshots(provider_profile_id)?;
            return Ok(ProviderModelRuntimeOptionProbeResult::default());
        };
        let model_ids = configured_model_ids(&profile);
        self.delete_stale_profile_model_snapshots(&profile.id, &model_ids)?;
        if profile.kind != ProviderKind::Acp || profile.status != ProviderProfileStatus::Enabled {
            return Ok(ProviderModelRuntimeOptionProbeResult::default());
        }
        let agents = self.provider_config.list_agents(AgentListRequest {
            include_disabled: true,
        })?;
        if !agents.agents.iter().any(|agent| {
            agent.id == profile.agent_id && agent.added && agent.enabled && agent.installed
        }) {
            return Ok(ProviderModelRuntimeOptionProbeResult::default());
        }

        let mut snapshots = self.model_snapshot_map()?;
        let mut result = ProviderModelRuntimeOptionProbeResult::default();
        for model_id in model_ids {
            let key = ProviderModelRuntimeOptionKey {
                provider_profile_id: profile.id.clone(),
                model_id: model_id.clone(),
            };
            if snapshots
                .get(&(profile.id.clone(), model_id.clone()))
                .is_some_and(|snapshot| model_snapshot_is_reusable(&profile.agent_id, snapshot))
            {
                result.cached_models.push(key);
                continue;
            }

            let attempted_at_ms = vibex_core::unix_timestamp_ms();
            let mut session_config = match self
                .manager
                .probe_session_config_for_model(
                    profile.agent_id.clone(),
                    profile.id.clone(),
                    &model_id,
                )
                .await
            {
                Ok(probe) => probe,
                Err(error) => {
                    if self.record_model_snapshot_failure_if_current(
                        &profile,
                        &model_id,
                        attempted_at_ms,
                        &error.code,
                    )? {
                        result.failed_models.push(key);
                    }
                    continue;
                }
            };
            session_config.models = vec![model_id.clone()];
            let record = ProviderModelRuntimeOptionSnapshotRecord {
                provider_profile_id: profile.id.clone(),
                model_id: model_id.clone(),
                agent_id: profile.agent_id.clone(),
                session_config: Some(session_config),
                last_success_at_ms: Some(attempted_at_ms),
                last_attempt_at_ms: attempted_at_ms,
                last_error_code: None,
            };
            if self.persist_model_snapshot_success_if_current(&profile, &record)? {
                snapshots.insert((profile.id.clone(), model_id), record);
                result.probed_models.push(key);
            }
        }
        Ok(result)
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
            && agent_snapshot_is_reusable(agent_id, snapshot)
        {
            return Ok(RuntimeOptionProbeResult {
                cached_agent_ids: vec![agent_id.clone()],
                ..Default::default()
            });
        }

        let _probe_guard = self
            .acquire_probe(RuntimeOptionProbeKey::Agent(agent_id.clone()))
            .await;
        // Re-check after waiting for another setup probe to finish.
        if let Some(snapshot) = self.snapshot_map()?.get(agent_id)
            && agent_snapshot_is_reusable(agent_id, snapshot)
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

    async fn acquire_probe(&self, key: RuntimeOptionProbeKey) -> tokio::sync::OwnedMutexGuard<()> {
        let lock = {
            let mut locks = self.probe_locks.lock().await;
            locks.retain(|_, lock| lock.strong_count() > 0);
            if let Some(lock) = locks.get(&key).and_then(Weak::upgrade) {
                lock
            } else {
                let lock = Arc::new(tokio::sync::Mutex::new(()));
                locks.insert(key, Arc::downgrade(&lock));
                lock
            }
        };
        lock.lock_owned().await
    }

    pub fn delete_profile_model_snapshots(
        &self,
        provider_profile_id: &ProviderProfileId,
    ) -> Result<(), VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        ProviderModelRuntimeOptionSnapshotRepository::delete_profile(
            &connection,
            provider_profile_id,
        )
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

    fn model_snapshot_records(
        &self,
    ) -> Result<Vec<ProviderModelRuntimeOptionSnapshotRecord>, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        ProviderModelRuntimeOptionSnapshotRepository::list(&connection)
    }

    fn model_snapshot_map(
        &self,
    ) -> Result<
        BTreeMap<(ProviderProfileId, String), ProviderModelRuntimeOptionSnapshotRecord>,
        VibexError,
    > {
        Ok(self
            .model_snapshot_records()?
            .into_iter()
            .map(|record| {
                (
                    (record.provider_profile_id.clone(), record.model_id.clone()),
                    record,
                )
            })
            .collect())
    }

    fn delete_stale_profile_model_snapshots(
        &self,
        provider_profile_id: &ProviderProfileId,
        model_ids: &[String],
    ) -> Result<(), VibexError> {
        let stale_models = self
            .model_snapshot_records()?
            .into_iter()
            .filter(|snapshot| {
                snapshot.provider_profile_id == *provider_profile_id
                    && !model_ids.contains(&snapshot.model_id)
            })
            .map(|snapshot| snapshot.model_id)
            .collect::<Vec<_>>();
        if stale_models.is_empty() {
            return Ok(());
        }
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_transaction_failed",
                "failed to begin model runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        for model_id in stale_models {
            ProviderModelRuntimeOptionSnapshotRepository::delete_model(
                &transaction,
                provider_profile_id,
                &model_id,
            )?;
        }
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_commit_failed",
                "failed to commit model runtime option snapshot cleanup",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(())
    }

    fn persist_model_snapshot_success_if_current(
        &self,
        expected: &ProviderProfile,
        record: &ProviderModelRuntimeOptionSnapshotRecord,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_transaction_failed",
                "failed to begin model runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if ProviderProfileRepository::get(&transaction, &expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        ProviderModelRuntimeOptionSnapshotRepository::upsert_success(&transaction, record)?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_commit_failed",
                "failed to commit model runtime option snapshot",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
    }

    fn record_model_snapshot_failure_if_current(
        &self,
        expected: &ProviderProfile,
        model_id: &str,
        attempted_at_ms: i64,
        error_code: &str,
    ) -> Result<bool, VibexError> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        let transaction = connection.transaction().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_transaction_failed",
                "failed to begin model runtime option snapshot transaction",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        if ProviderProfileRepository::get(&transaction, &expected.id)?.as_ref() != Some(expected) {
            return Ok(false);
        }
        ProviderModelRuntimeOptionSnapshotRepository::record_failure(
            &transaction,
            &expected.id,
            model_id,
            &expected.agent_id,
            attempted_at_ms,
            error_code,
        )?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "provider_model_runtime_option_snapshot_commit_failed",
                "failed to commit model runtime option snapshot failure",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(true)
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
    profiles: &[ProviderProfile],
    fallback_by_agent: &BTreeMap<AgentId, RuntimeOptionCatalogProfileEvidence>,
    mut model_evidence_by_profile: BTreeMap<ProviderProfileId, Vec<SessionModelCatalogEntry>>,
    live_by_profile: BTreeMap<ProviderProfileId, vibex_core::AgentSessionConfigProbe>,
) -> BTreeMap<ProviderProfileId, RuntimeOptionCatalogProfileEvidence> {
    let mut evidence_by_profile = profiles
        .iter()
        .map(|profile| {
            let mut evidence = fallback_by_agent
                .get(&profile.agent_id)
                .cloned()
                .unwrap_or_default();
            evidence.models = model_evidence_by_profile
                .remove(&profile.id)
                .unwrap_or_default();
            if !evidence.models.is_empty() {
                evidence.temporarily_unavailable = false;
            }
            (profile.id.clone(), evidence)
        })
        .collect::<BTreeMap<_, _>>();
    for (profile_id, probe) in live_by_profile {
        let evidence = evidence_by_profile.entry(profile_id).or_default();
        // A live session refreshes Profile-wide fallback controls. Persisted
        // model evidence remains authoritative for its concrete model.
        evidence.modes = probe.modes;
        evidence.reasoning_efforts = probe.reasoning_efforts;
        evidence.options = probe.options;
        evidence.temporarily_unavailable = false;
    }
    evidence_by_profile
}

fn model_snapshot_evidence(
    snapshots: Vec<ProviderModelRuntimeOptionSnapshotRecord>,
) -> BTreeMap<ProviderProfileId, Vec<SessionModelCatalogEntry>> {
    let mut evidence = BTreeMap::<ProviderProfileId, Vec<SessionModelCatalogEntry>>::new();
    for snapshot in snapshots {
        if snapshot.last_success_at_ms.is_none() {
            continue;
        }
        let Some(session_config) = snapshot.session_config else {
            continue;
        };
        evidence
            .entry(snapshot.provider_profile_id)
            .or_default()
            .push(SessionModelCatalogEntry {
                model_id: snapshot.model_id,
                reasoning_efforts: session_config.reasoning_efforts,
                default_reasoning_effort: None,
                modes: session_config.modes,
                options: session_config.options,
                runtime_options_complete: true,
                source: SessionModelCatalogSource::Probe,
            });
    }
    evidence
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

fn agent_snapshot_is_reusable(
    agent_id: &AgentId,
    snapshot: &AgentRuntimeOptionSnapshotRecord,
) -> bool {
    snapshot.last_success_at_ms.is_some()
        && (agent_id.as_str() != "opencode"
            || snapshot
                .session_config
                .as_ref()
                .is_some_and(session_config_has_mode))
}

fn model_snapshot_is_reusable(
    agent_id: &AgentId,
    snapshot: &ProviderModelRuntimeOptionSnapshotRecord,
) -> bool {
    snapshot.last_success_at_ms.is_some()
        && (agent_id.as_str() != "opencode"
            || snapshot
                .session_config
                .as_ref()
                .is_some_and(session_config_has_mode))
}

fn session_config_has_mode(config: &vibex_core::AgentSessionConfigProbe) -> bool {
    !config.modes.is_empty()
        || config.options.iter().any(|option| {
            option.id.eq_ignore_ascii_case("mode")
                || option
                    .category
                    .as_deref()
                    .is_some_and(|category| category.eq_ignore_ascii_case("mode"))
        })
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
    use crate::AgentAuthCatalogService;
    use tempfile::TempDir;
    use vibex_agent::{
        AgentProvider, ProviderCreateRequest, ProviderSessionHandle, ProviderTurnRequest,
        ProviderTurnResult,
    };
    use vibex_core::{
        AcpProcessStrategy, AcpProviderConfig, AcpProviderProfileCreateRequest, AgentCommandConfig,
        AgentModelProviderProfileCreateRequest, AgentModelProviderProfileUpdateRequest,
        AgentReasoningEffort, AgentRuntimeRouteKey, AgentSessionConfigProbe,
        AgentUpdateConfigRequest, ProviderBinding, ProviderCapabilities, ProviderConfiguredModel,
        ProviderProfile, ProviderSessionConfigOption, ProviderSessionConfigOptionKind,
        ProviderSessionConfigValue, TransportKind, VibexResult,
    };

    struct CountingProvider {
        calls: AtomicUsize,
        model_calls: AtomicUsize,
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

        async fn probe_session_config_for_model(
            &self,
            _provider_profile_id: &vibex_core::ProviderProfileId,
            model_id: &str,
        ) -> VibexResult<AgentSessionConfigProbe> {
            self.model_calls.fetch_add(1, Ordering::SeqCst);
            Ok(model_session_config(model_id))
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
        catalog_fixture_for_agent("opencode", fail_probe)
    }

    fn catalog_fixture_for_agent(
        agent_name: &str,
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
        let agent_id = AgentId::parse(agent_name).unwrap();
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
        provider_config
            .refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                agent_id: agent_id.clone(),
                cwd_scope: None,
            })
            .unwrap();
        for disabled_agent_id in ["claude", "codex"] {
            provider_config
                .update_agent_config(AgentUpdateConfigRequest {
                    agent_id: AgentId::parse(disabled_agent_id).unwrap(),
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
        }
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            model_calls: AtomicUsize::new(0),
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

    #[tokio::test]
    async fn probe_locks_are_scoped_to_one_agent_or_profile() {
        let (_directory, catalog, _provider_config, agent_id, _provider) = catalog_fixture(false);
        let first_profile = ProviderProfileId::parse("provider_first").unwrap();
        let second_profile = ProviderProfileId::parse("provider_second").unwrap();

        let first = catalog
            .acquire_probe(RuntimeOptionProbeKey::Profile(first_profile.clone()))
            .await;
        let second = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            catalog.acquire_probe(RuntimeOptionProbeKey::Profile(second_profile)),
        )
        .await
        .expect("different profiles must not share a probe lock");
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(20),
                catalog.acquire_probe(RuntimeOptionProbeKey::Profile(first_profile)),
            )
            .await
            .is_err(),
            "the same profile must serialize probes"
        );
        let agent = tokio::time::timeout(
            std::time::Duration::from_secs(1),
            catalog.acquire_probe(RuntimeOptionProbeKey::Agent(agent_id)),
        )
        .await
        .expect("Agent and profile probes must not block each other");
        drop((first, second, agent));
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
            capabilities: Default::default(),
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
            reasoning_efforts: vec![effort("none"), effort("high"), effort("max")],
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

    fn model_session_config(model_id: &str) -> AgentSessionConfigProbe {
        if model_id == "model-without-runtime-options" {
            return AgentSessionConfigProbe {
                models: vec![model_id.to_string()],
                ..Default::default()
            };
        }
        let reasoning_efforts = match model_id {
            "gpt-5.6-sol" => vec![effort("none"), effort("on")],
            "glm-5.2" => vec![effort("none"), effort("high"), effort("max")],
            _ => vec![effort("high")],
        };
        AgentSessionConfigProbe {
            models: vec![model_id.to_string()],
            modes: vec![mode(
                if model_id == "gpt-5.6-sol" {
                    "accept_edits"
                } else {
                    "plan"
                },
                None,
            )],
            reasoning_efforts,
            options: Vec::new(),
        }
    }

    #[tokio::test]
    async fn catalog_read_does_not_create_an_account_context_or_probe_an_agent() {
        let directory = tempfile::tempdir().unwrap();
        let database_path = directory.path().join("vibex.db");
        let provider_config = ProviderConfigService::new(&database_path);
        let agent_id = AgentId::parse("codex").unwrap();
        provider_config
            .update_agent_config(AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                enabled: Some(true),
                label_override: None,
                description_override: None,
                order_index: None,
                command: Some(AgentCommandConfig {
                    command: "/path/that/must/not/be/spawned/by/catalog-read".to_string(),
                    args: Vec::new(),
                }),
                env: None,
                params: None,
            })
            .unwrap();
        let provider = Arc::new(CountingProvider {
            calls: AtomicUsize::new(0),
            model_calls: AtomicUsize::new(0),
            fail_probe: false,
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
        let acp_runtime = Arc::new(AcpRuntimeClient::new(provider_config.clone()));
        let auth_catalog = Arc::new(AgentAuthCatalogService::new(
            manager.clone(),
            provider_config.clone(),
        ));
        let auth_contexts = Arc::new(
            AgentAuthContextService::new(
                database_path.clone(),
                manager.clone(),
                acp_runtime.clone(),
                Arc::new(vibex_agent_acp::DisabledAcpTerminalHost),
                auth_catalog,
            )
            .unwrap(),
        );
        let catalog =
            RuntimeOptionCatalogService::with_live_runtime(manager, provider_config, acp_runtime)
                .with_auth_context_service(auth_contexts);

        let connection = open_database(&database_path).unwrap();
        assert!(
            vibex_db::AgentAuthContextRepository::list(&connection)
                .unwrap()
                .is_empty()
        );
        catalog.list().await.unwrap();
        assert!(
            vibex_db::AgentAuthContextRepository::list(&connection)
                .unwrap()
                .is_empty(),
            "ordinary catalog reads must not bootstrap durable account state"
        );
        assert_eq!(provider.calls.load(Ordering::SeqCst), 0);
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn runtime_catalog_keeps_agent_owned_profiles_visible() {
        let (_directory, catalog, provider_config, agent_id, _provider) =
            catalog_fixture_for_agent("gemini", false);
        let profile = provider_config
            .create_acp_profile(AcpProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                display_name: "Gemini ACP".to_string(),
                account_alias: None,
                preset_id: None,
                config: Some(AcpProviderConfig {
                    command: "/bin/true".to_string(),
                    args: Vec::new(),
                    env: Vec::new(),
                    cwd_template: Some("{workspaceRoot}".to_string()),
                    process_strategy: AcpProcessStrategy::default(),
                    terminal_tools: false,
                    terminal_auth: false,
                    models: vec!["gemini-pro".to_string()],
                    modes: Vec::new(),
                    features: Vec::new(),
                    disabled_tools: Vec::new(),
                }),
            })
            .unwrap();

        let options = catalog.list().await.unwrap().options;
        assert!(
            options
                .iter()
                .any(|option| option.selection.provider_profile_id() == Some(&profile.id)),
            "runtime catalog must retain Agent-owned profiles hidden from Config Center"
        );
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
        let bootstrap_probe = catalog.probe_missing_enabled_agents().await.unwrap();
        assert_eq!(bootstrap_probe, RuntimeOptionProbeResult::default());
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let options = catalog.list().await.unwrap().options;
        let profile_options = options
            .iter()
            .filter(|option| {
                option.selection.provider_profile_id() == Some(&first_profile.id)
                    || option.selection.provider_profile_id() == Some(&second_profile.id)
            })
            .collect::<Vec<_>>();
        assert_eq!(profile_options.len(), 2);
        assert!(profile_options.iter().any(|option| {
            option.selection.provider_profile_id() == Some(&first_profile.id)
                && option.selection.model_id() == Some("first-model")
        }));
        assert!(profile_options.iter().any(|option| {
            option.selection.provider_profile_id() == Some(&second_profile.id)
                && option.selection.model_id() == Some("second-model")
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
                option.selection.model_id(),
                Some("model-from-agent-must-not-be-used")
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
            option.selection.provider_profile_id() == Some(&third_profile.id)
                && option.modes.iter().any(|mode| mode.value == "plan")
        }));
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);
        assert_eq!(catalog.snapshot_summaries().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn model_snapshots_override_agent_fallback_and_are_reused_by_model() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(false);
        let profile = create_profile(
            &provider_config,
            &agent_id,
            "GLM model provider",
            vec![configured_model("glm-5.2"), configured_model("gpt-5.6-sol")],
        );

        catalog.probe_agent(&agent_id).await.unwrap();
        let fallback = catalog.list().await.unwrap();
        let fallback_gpt = fallback
            .options
            .iter()
            .find(|option| {
                option.selection.provider_profile_id() == Some(&profile.id)
                    && option.selection.model_id() == Some("gpt-5.6-sol")
            })
            .unwrap();
        assert!(
            fallback_gpt
                .reasoning_efforts
                .iter()
                .any(|effort| effort.value == "max")
        );

        let result = catalog.probe_profile_models(&profile.id).await.unwrap();
        assert_eq!(result.probed_models.len(), 2);
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 2);
        let options = catalog.list().await.unwrap().options;
        let gpt = options
            .iter()
            .find(|option| {
                option.selection.provider_profile_id() == Some(&profile.id)
                    && option.selection.model_id() == Some("gpt-5.6-sol")
            })
            .unwrap();
        assert_eq!(
            gpt.reasoning_efforts
                .iter()
                .map(|effort| effort.value.as_str())
                .collect::<Vec<_>>(),
            vec!["none", "on"]
        );
        assert!(
            !gpt.reasoning_efforts
                .iter()
                .any(|effort| effort.value == "max")
        );
        assert_eq!(gpt.modes[0].value, "accept_edits");

        let glm = options
            .iter()
            .find(|option| {
                option.selection.provider_profile_id() == Some(&profile.id)
                    && option.selection.model_id() == Some("glm-5.2")
            })
            .unwrap();
        assert!(
            glm.reasoning_efforts
                .iter()
                .any(|effort| effort.value == "max")
        );
        assert_eq!(glm.modes[0].value, "plan");

        let cached = catalog.probe_profile_models(&profile.id).await.unwrap();
        assert_eq!(cached.probed_models.len(), 0);
        assert_eq!(cached.cached_models.len(), 2);
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 2);
    }

    #[tokio::test]
    async fn successful_empty_model_snapshot_suppresses_agent_fallback_controls() {
        let (_directory, catalog, provider_config, agent_id, _provider) = catalog_fixture(false);
        let profile = create_profile(
            &provider_config,
            &agent_id,
            "Model without runtime options",
            vec![configured_model("model-without-runtime-options")],
        );

        catalog.probe_agent(&agent_id).await.unwrap();
        catalog.probe_profile_models(&profile.id).await.unwrap();

        let options = catalog.list().await.unwrap().options;
        let model = options
            .iter()
            .find(|option| option.selection.provider_profile_id() == Some(&profile.id))
            .unwrap();
        assert!(model.reasoning_efforts.is_empty());
        assert!(model.modes.is_empty());
        assert!(model.features.is_empty());
        assert!(model.selection.config_values.is_empty());
    }

    #[tokio::test]
    async fn unchanged_model_keeps_cache_while_replaced_model_is_probed() {
        let (_directory, catalog, provider_config, agent_id, provider) = catalog_fixture(false);
        let profile = create_profile(
            &provider_config,
            &agent_id,
            "Editable provider",
            vec![configured_model("gpt-5.6-sol")],
        );
        catalog.probe_profile_models(&profile.id).await.unwrap();
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 1);

        provider_config
            .update_agent_model_provider_profile(AgentModelProviderProfileUpdateRequest {
                agent_id: agent_id.clone(),
                provider_profile_id: profile.id.clone(),
                display_name: Some("Renamed provider".to_string()),
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
            })
            .unwrap();
        let cached = catalog.probe_profile_models(&profile.id).await.unwrap();
        assert_eq!(cached.cached_models.len(), 1);
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 1);

        provider_config
            .update_agent_model_provider_profile(AgentModelProviderProfileUpdateRequest {
                agent_id,
                provider_profile_id: profile.id.clone(),
                display_name: None,
                status: None,
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: Some(vec![configured_model("glm-5.2")]),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            })
            .unwrap();
        let replaced = catalog.probe_profile_models(&profile.id).await.unwrap();
        assert_eq!(replaced.probed_models.len(), 1);
        assert_eq!(replaced.probed_models[0].model_id, "glm-5.2");
        assert_eq!(provider.model_calls.load(Ordering::SeqCst), 2);
        let snapshots = catalog.model_snapshot_records().unwrap();
        assert_eq!(snapshots.len(), 1);
        assert_eq!(snapshots[0].model_id, "glm-5.2");
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
        let layered = layer_profile_session_evidence(&profiles, &fallback, BTreeMap::new(), live);
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
            .find(|option| option.selection.provider_profile_id() == Some(&first_profile.id))
            .unwrap();
        assert_eq!(first.selection.model_id(), Some("configured-first"));
        assert_eq!(first.modes[0].value, "review");
        assert_eq!(first.reasoning_efforts[0].value, "low");
        let second = catalog
            .options
            .iter()
            .find(|option| option.selection.provider_profile_id() == Some(&second_profile.id))
            .unwrap();
        assert_eq!(second.selection.model_id(), Some("configured-second"));
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
    async fn failed_probe_is_recorded_by_agent_and_retried_by_startup_bootstrap() {
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

        let bootstrap_result = catalog.probe_missing_enabled_agents().await.unwrap();
        assert_eq!(bootstrap_result.failed_agent_ids, vec![agent_id.clone()]);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);

        catalog.delete_agent_snapshot(&agent_id).unwrap();
        assert!(catalog.snapshot_summaries().unwrap().is_empty());
    }

    #[test]
    fn opencode_successful_snapshots_without_mode_evidence_are_reprobed() {
        let opencode = AgentId::parse("opencode").unwrap();
        let other = AgentId::parse("other-agent").unwrap();
        let empty_config = AgentSessionConfigProbe {
            models: Vec::new(),
            modes: Vec::new(),
            reasoning_efforts: Vec::new(),
            options: Vec::new(),
        };
        let mode_config = AgentSessionConfigProbe {
            modes: vec![mode("build", Some("Build"))],
            ..empty_config.clone()
        };
        let agent_snapshot = |config| AgentRuntimeOptionSnapshotRecord {
            agent_id: opencode.clone(),
            session_config: Some(config),
            last_success_at_ms: Some(1),
            last_attempt_at_ms: 1,
            last_error_code: None,
        };
        let model_snapshot = |config| ProviderModelRuntimeOptionSnapshotRecord {
            provider_profile_id: ProviderProfileId::parse("provider_opencode").unwrap(),
            model_id: "provider/model".to_string(),
            agent_id: opencode.clone(),
            session_config: Some(config),
            last_success_at_ms: Some(1),
            last_attempt_at_ms: 1,
            last_error_code: None,
        };

        assert!(!agent_snapshot_is_reusable(
            &opencode,
            &agent_snapshot(empty_config.clone())
        ));
        assert!(agent_snapshot_is_reusable(
            &opencode,
            &agent_snapshot(mode_config.clone())
        ));
        assert!(agent_snapshot_is_reusable(
            &other,
            &agent_snapshot(empty_config.clone())
        ));
        assert!(!model_snapshot_is_reusable(
            &opencode,
            &model_snapshot(empty_config)
        ));
        assert!(model_snapshot_is_reusable(
            &opencode,
            &model_snapshot(mode_config)
        ));
    }
}

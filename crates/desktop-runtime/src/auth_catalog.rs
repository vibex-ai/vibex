use std::sync::Arc;

use vibex_agent::AgentManager;
use vibex_config_switch::ProviderConfigService;
use vibex_core::{AgentAuthCatalog, AgentId, ProviderProfileId, VibexResult, unix_timestamp_ms};
use vibex_db::{
    AgentAuthCatalogSnapshotRecord, AgentAuthCatalogSnapshotRepository, apply_migrations,
    open_database,
};

#[derive(Clone)]
pub struct AgentAuthCatalogService {
    manager: Arc<AgentManager>,
    provider_config: ProviderConfigService,
    refresh_lock: Arc<tokio::sync::Mutex<()>>,
}

impl AgentAuthCatalogService {
    pub fn new(manager: Arc<AgentManager>, provider_config: ProviderConfigService) -> Self {
        Self {
            manager,
            provider_config,
            refresh_lock: Arc::new(tokio::sync::Mutex::new(())),
        }
    }

    pub async fn list(
        &self,
        agent_id: AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        if let Some(cached) = self.read_cached(&agent_id, provider_profile_id.as_ref())? {
            return Ok(cached);
        }
        let _guard = self.refresh_lock.lock().await;
        if let Some(cached) = self.read_cached(&agent_id, provider_profile_id.as_ref())? {
            return Ok(cached);
        }
        self.refresh_unlocked(agent_id, provider_profile_id).await
    }

    pub async fn refresh(
        &self,
        agent_id: AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        let _guard = self.refresh_lock.lock().await;
        self.refresh_unlocked(agent_id, provider_profile_id).await
    }

    async fn refresh_unlocked(
        &self,
        agent_id: AgentId,
        provider_profile_id: Option<ProviderProfileId>,
    ) -> VibexResult<AgentAuthCatalog> {
        let catalog = self
            .manager
            .list_agent_auth_methods(agent_id.clone(), provider_profile_id.clone())
            .await?;
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        AgentAuthCatalogSnapshotRepository::upsert(
            &connection,
            &AgentAuthCatalogSnapshotRecord {
                agent_id,
                provider_profile_id,
                refreshed_at_ms: unix_timestamp_ms(),
                catalog: catalog.clone(),
            },
        )?;
        Ok(catalog)
    }

    fn read_cached(
        &self,
        agent_id: &AgentId,
        provider_profile_id: Option<&ProviderProfileId>,
    ) -> VibexResult<Option<AgentAuthCatalog>> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        Ok(
            AgentAuthCatalogSnapshotRepository::get(&connection, agent_id, provider_profile_id)?
                .map(|record| record.catalog),
        )
    }

    pub fn delete_agent(&self, agent_id: &AgentId) -> VibexResult<()> {
        let mut connection = open_database(self.provider_config.database_path())?;
        apply_migrations(&mut connection)?;
        AgentAuthCatalogSnapshotRepository::delete_agent(&connection, agent_id)
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use async_trait::async_trait;
    use vibex_agent::{
        AgentProvider, ProviderCreateRequest, ProviderSessionHandle, ProviderTurnRequest,
        ProviderTurnResult,
    };
    use vibex_core::{
        AgentAuthStatus, AgentCommandConfig, AgentRuntimeRouteKey, AgentUpdateConfigRequest,
        ProviderBinding, ProviderCapabilities, ProviderKind, TransportKind, VibexError,
    };

    use super::*;

    struct CountingAuthProvider {
        calls: AtomicUsize,
    }

    #[async_trait]
    impl AgentProvider for CountingAuthProvider {
        fn kind(&self) -> ProviderKind {
            ProviderKind::Acp
        }

        fn capabilities(&self) -> ProviderCapabilities {
            ProviderCapabilities::conservative(ProviderKind::Acp, "auth-catalog-test")
        }

        async fn list_auth_methods(
            &self,
            agent_id: &AgentId,
            _provider_profile_id: Option<&ProviderProfileId>,
        ) -> VibexResult<AgentAuthCatalog> {
            let call = self.calls.fetch_add(1, Ordering::SeqCst) + 1;
            Ok(AgentAuthCatalog {
                agent_id: agent_id.clone(),
                methods: Vec::new(),
                supports_logout: call > 1,
                status: AgentAuthStatus::Unknown,
                refreshed_at_ms: call as i64,
            })
        }

        async fn create_session(
            &self,
            _request: ProviderCreateRequest,
        ) -> VibexResult<ProviderSessionHandle> {
            Err(VibexError::capability("unused", "unused"))
        }

        async fn resume_session(
            &self,
            _binding: ProviderBinding,
        ) -> VibexResult<ProviderSessionHandle> {
            Err(VibexError::capability("unused", "unused"))
        }

        async fn send_turn(
            &self,
            _handle: ProviderSessionHandle,
            _request: ProviderTurnRequest,
        ) -> VibexResult<ProviderTurnResult> {
            Err(VibexError::capability("unused", "unused"))
        }
    }

    #[tokio::test]
    async fn ordinary_reads_use_cache_and_manual_refresh_replaces_it() {
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
        let provider = Arc::new(CountingAuthProvider {
            calls: AtomicUsize::new(0),
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
        let service = AgentAuthCatalogService::new(Arc::new(manager), provider_config);

        let first = service.list(agent_id.clone(), None).await.unwrap();
        let cached = service.list(agent_id.clone(), None).await.unwrap();
        assert_eq!(first, cached);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 1);

        let refreshed = service.refresh(agent_id.clone(), None).await.unwrap();
        assert!(refreshed.supports_logout);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
        assert_eq!(service.list(agent_id, None).await.unwrap(), refreshed);
        assert_eq!(provider.calls.load(Ordering::SeqCst), 2);
    }
}

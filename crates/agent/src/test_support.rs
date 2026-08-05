use std::ops::Deref;
use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use vibex_core::{
    AcpAdapterId, ActiveWorkKind, AgentId, BindingState, NativeStateHomeId,
    ProviderConfiguredModel, ProviderDefaultScopeKind, ProviderKind, ProviderProfileCreateRequest,
    ProviderProfileDefaultScope, RuntimeBinding, SessionRuntimeConfigState, TransportKind,
    VibexResult, VibexSessionId, unix_timestamp_ms,
};
use vibex_db::{
    AgentDefaultModelProviderProfileRepository, ProviderProfileRepository, SwitchOperationRecord,
};

use crate::adapter::AgentProvider;
use crate::manager::AgentManager;
use crate::message_submission::{
    MessageSubmissionCoordinator, MessageSubmissionCoordinatorConfig, manager_message_dispatcher,
};
use crate::runtime_route::default_adapter_for_agent;
use crate::runtime_selection::{
    ResolvedRuntimeSelection, RuntimeSelectionResolver, RuntimeSelectionService,
    RuntimeSelectionServiceConfig,
};
use crate::runtime_switch::{
    ActiveWorkGate, ActiveWorkSnapshot, JournaledOperation, OperationReconcileOutcome,
    PreparedAttachment, PreparedProcess, RestoreAssessment, RuntimeSwitchCoordinator,
    RuntimeSwitchCoordinatorConfig, RuntimeSwitchStrategy, SwitchIntent, SwitchTargetAssessment,
    SwitchTargetExecutor,
};

pub(crate) struct TestRuntimeHarness {
    manager: Arc<AgentManager>,
    _runtime_selection: Arc<RuntimeSelectionService>,
    _message_submission: Arc<MessageSubmissionCoordinator>,
}

impl TestRuntimeHarness {
    pub(crate) fn new(db_path: &Path, agent_id: AgentId, provider: Arc<dyn AgentProvider>) -> Self {
        let adapter_id = default_adapter_for_agent(&agent_id);
        let mut manager = AgentManager::new(db_path).unwrap();
        let conn = manager.open_migrated().unwrap();
        let profile =
            ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                agent_id: Some(agent_id.clone()),
                kind: ProviderKind::Acp,
                display_name: "Test ACP profile".to_string(),
                account_alias: None,
                base_url: None,
                default_model: Some("test-model".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![ProviderConfiguredModel {
                    id: "test-model".to_string(),
                    display_name: Some("Test model".to_string()),
                    enabled: true,
                    wire_api: None,
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            });
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        AgentDefaultModelProviderProfileRepository::set(
            &conn,
            ProviderProfileDefaultScope {
                kind: ProviderDefaultScopeKind::Global,
                project_id: None,
                workspace_id: None,
            },
            agent_id.clone(),
            profile.id,
        )
        .unwrap();
        drop(conn);

        manager
            .register_runtime(
                vibex_core::AgentRuntimeRouteKey {
                    agent_id,
                    transport_kind: TransportKind::Acp,
                    adapter_id: adapter_id.clone(),
                },
                provider,
            )
            .unwrap();
        let manager = Arc::new(manager);
        let runtime = Arc::new(TestSwitchRuntime { adapter_id });
        let coordinator = RuntimeSwitchCoordinator::new(
            db_path,
            runtime.clone(),
            runtime.clone(),
            RuntimeSwitchCoordinatorConfig {
                lease_duration_ms: 1_000,
                idle_poll_interval: Duration::from_millis(1),
            },
        )
        .unwrap();
        let runtime_selection = Arc::new(
            RuntimeSelectionService::new(
                coordinator,
                runtime,
                RuntimeSelectionServiceConfig {
                    seamless_wait_deadline_ms: 1_000,
                    poll_interval: Duration::from_millis(1),
                    broadcast_capacity: 16,
                },
            )
            .unwrap(),
        );
        manager
            .install_runtime_selection_service(&runtime_selection)
            .unwrap();
        let message_submission = Arc::new(
            MessageSubmissionCoordinator::new(
                db_path,
                runtime_selection.clone(),
                manager_message_dispatcher(&manager),
                MessageSubmissionCoordinatorConfig {
                    poll_interval: Duration::from_millis(1),
                },
            )
            .unwrap(),
        );
        manager
            .install_message_submission_coordinator(&message_submission)
            .unwrap();

        Self {
            manager,
            _runtime_selection: runtime_selection,
            _message_submission: message_submission,
        }
    }
}

impl Deref for TestRuntimeHarness {
    type Target = AgentManager;

    fn deref(&self) -> &Self::Target {
        &self.manager
    }
}

#[derive(Clone)]
struct TestSwitchRuntime {
    adapter_id: AcpAdapterId,
}

impl TestSwitchRuntime {
    fn attachment(intent: &SwitchIntent) -> PreparedAttachment {
        let generation = 1;
        let mut config = SessionRuntimeConfigState {
            preferred_model: Some(intent.target_selection.model_id.clone()),
            effective_model: Some(intent.target_selection.model_id.clone()),
            preferred_mode: intent.target_selection.mode_id.clone(),
            effective_mode: intent.target_selection.mode_id.clone(),
            preferred_reasoning_effort: intent.target_selection.reasoning_effort.clone(),
            effective_reasoning_effort: intent.target_selection.reasoning_effort.clone(),
            state_revision: 1,
            ..SessionRuntimeConfigState::default()
        };
        config.mark_generation_if_converged(generation);
        let now = unix_timestamp_ms();
        let binding_id = intent
            .target_binding_id
            .clone()
            .expect("initial switch reserves a target binding");
        PreparedAttachment {
            binding: RuntimeBinding {
                binding_id: binding_id.clone(),
                session_id: intent.session_id.clone(),
                agent_id: intent.target_selection.agent_id.clone(),
                transport_kind: TransportKind::Acp,
                provider_profile_id: intent.target_selection.provider_profile_id.clone(),
                adapter_id: intent.target_adapter_id.clone(),
                adapter_version: "test-adapter-v1".to_string(),
                adapter_compatibility_identity: "test-adapter-compatible-v1".to_string(),
                native_session_id: Some(format!("test-native-{}", binding_id.as_str())),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: None,
                process_spawn_fingerprint: "test-process-fingerprint".to_string(),
                session_runtime_config_state: config,
                capability_snapshot: None,
                restore_compatibility_key: None,
                profile_revision: 1,
                last_context_sequence: 0,
                last_summary_sequence: 0,
                context_bridge_version: 0,
                activation_generation: generation,
                binding_state: BindingState::Preparing,
                created_by_switch_id: Some(intent.switch_id.clone()),
                created_at_ms: now,
                updated_at_ms: now,
            },
            opaque_handle: "test-attachment".to_string(),
            restore_result: None,
        }
    }
}

#[async_trait]
impl RuntimeSelectionResolver for TestSwitchRuntime {
    async fn resolve(
        &self,
        _session_id: &VibexSessionId,
        _selection: &vibex_core::SessionRuntimeSelection,
        preferred_adapter_id: Option<&AcpAdapterId>,
    ) -> VibexResult<ResolvedRuntimeSelection> {
        Ok(ResolvedRuntimeSelection {
            adapter_id: preferred_adapter_id
                .cloned()
                .unwrap_or_else(|| self.adapter_id.clone()),
            session_config: None,
        })
    }
}

#[async_trait]
impl SwitchTargetExecutor for TestSwitchRuntime {
    async fn assess_target(&self, _intent: &SwitchIntent) -> VibexResult<SwitchTargetAssessment> {
        Ok(SwitchTargetAssessment {
            same_route: false,
            process_config_changed: true,
            session_scoped_changes_only: false,
            live_ops_supported: false,
            exact_descriptor: false,
            runtime_evidence_verified: false,
            projection_fingerprint_matches: false,
            active_turn: false,
            restore: RestoreAssessment::Incompatible,
            resumable_historical_binding: false,
            supports_client_idempotency: true,
        })
    }

    async fn ensure_process(
        &self,
        _intent: &SwitchIntent,
        _operation: &JournaledOperation,
    ) -> VibexResult<PreparedProcess> {
        Ok(PreparedProcess {
            opaque_handle: "test-process".to_string(),
        })
    }

    async fn reacquire_process(&self, _intent: &SwitchIntent) -> VibexResult<PreparedProcess> {
        Ok(PreparedProcess {
            opaque_handle: "test-process".to_string(),
        })
    }

    async fn restore_or_create_session(
        &self,
        intent: &SwitchIntent,
        _process: &PreparedProcess,
        _strategy: RuntimeSwitchStrategy,
        _operation: &JournaledOperation,
    ) -> VibexResult<PreparedAttachment> {
        Ok(Self::attachment(intent))
    }

    async fn recover_attachment(
        &self,
        intent: &SwitchIntent,
        _operation: &SwitchOperationRecord,
    ) -> VibexResult<PreparedAttachment> {
        Ok(Self::attachment(intent))
    }

    async fn acquire_prepared(
        &self,
        _intent: &SwitchIntent,
        binding: &RuntimeBinding,
    ) -> VibexResult<PreparedAttachment> {
        Ok(PreparedAttachment {
            binding: binding.clone(),
            opaque_handle: "test-attachment".to_string(),
            restore_result: None,
        })
    }

    async fn apply_session_config(
        &self,
        _intent: &SwitchIntent,
        _attachment: &PreparedAttachment,
        _operation: &JournaledOperation,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn apply_live_mutation(
        &self,
        _intent: &SwitchIntent,
        _attachment: &PreparedAttachment,
        _operation: &JournaledOperation,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn revalidate_prepared(
        &self,
        _intent: &SwitchIntent,
        attachment: &PreparedAttachment,
    ) -> VibexResult<()> {
        assert!(
            attachment
                .binding
                .session_runtime_config_state
                .is_applied_to_generation(attachment.binding.activation_generation)
        );
        Ok(())
    }

    async fn activate(
        &self,
        _intent: &SwitchIntent,
        _attachment: &PreparedAttachment,
        _activation_generation: i64,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn cleanup_target(
        &self,
        _intent: &SwitchIntent,
        _attachment: Option<&PreparedAttachment>,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn reconcile_operation(
        &self,
        _intent: &SwitchIntent,
        _operation: &SwitchOperationRecord,
    ) -> VibexResult<OperationReconcileOutcome> {
        Ok(OperationReconcileOutcome::NotFound)
    }
}

#[async_trait]
impl ActiveWorkGate for TestSwitchRuntime {
    async fn probe(&self, _session_id: &VibexSessionId) -> VibexResult<ActiveWorkSnapshot> {
        Ok(ActiveWorkSnapshot::default())
    }

    async fn set_prompt_gate(
        &self,
        _session_id: &VibexSessionId,
        _closed: bool,
    ) -> VibexResult<()> {
        Ok(())
    }

    async fn cancel(
        &self,
        _session_id: &VibexSessionId,
        _kind: ActiveWorkKind,
        _operation: &JournaledOperation,
    ) -> VibexResult<()> {
        Ok(())
    }
}

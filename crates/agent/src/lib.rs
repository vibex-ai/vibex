//! Provider-neutral Agent session orchestration.

pub mod adapter;
pub mod automation;
pub mod context_bridge;
pub mod delegation;
pub mod local_history;
pub mod manager;
pub mod message_submission;
pub mod observability;
pub mod runtime_lifecycle;
pub mod runtime_route;
pub mod runtime_selection;
pub mod runtime_switch;
pub mod scheduler;
pub mod smoke_workspace;
pub mod state_machine;
#[cfg(test)]
pub(crate) mod test_support;

pub use adapter::{
    AgentProvider, AgentUsageTelemetryEvent, ProviderCreateRequest, ProviderElicitationResolution,
    ProviderEvent, ProviderPermissionResolution, ProviderRuntimeMcpServer,
    ProviderRuntimeMcpTransport, ProviderRuntimeResources, ProviderRuntimeSkill,
    ProviderSessionHandle, ProviderTurnAttachment, ProviderTurnExecutionIdentity,
    ProviderTurnRequest, ProviderTurnResult, legacy_provider_runtime_binding_id,
    materialize_provider_attachments, validate_legacy_provider_turn_execution_identity,
};
pub use automation::{AutomationGraphRunner, DEFAULT_AUTOMATION_STALE_AFTER_MS};
pub use context_bridge::{CONTEXT_BRIDGE_VERSION, ContextBridgeService, PreparedContextBridge};
pub use delegation::{
    AGENT_DELEGATION_MCP_SERVER_ID, run_delegation_mcp_stdio, start_delegation_broker,
};
pub use local_history::{
    LocalHistorySourceRoot, local_history_source_roots, materialize_local_history,
    materialize_local_history_from, scan_local_history, scan_local_history_from,
    session_shell_for_materialized,
};
pub use manager::{
    AgentDelegationToolConfig, AgentManager, PROVIDER_SELECTED_MODEL_METADATA_KEY,
    PROVIDER_SELECTED_REASONING_EFFORT_METADATA_KEY,
};
pub use message_submission::{
    DEFAULT_MESSAGE_SUBMISSION_POLL_INTERVAL, MessageDispatchExecutor, MessageRuntimeSelection,
    MessageSubmissionCoordinator, MessageSubmissionCoordinatorConfig,
    MessageSubmissionReconcileReport, ReplaceUserMessageRequest, manager_message_dispatcher,
};
pub use observability::{
    RUNTIME_METRIC_SERIES_LIMIT, RuntimeLogContext, RuntimeLogLevel, RuntimeMetricName,
    RuntimeMetricOperation, RuntimeMetricResult, RuntimeMetricSeries, RuntimeMetricSnapshot,
    RuntimeObservability,
};
pub use runtime_lifecycle::{
    DEFAULT_RUNTIME_CLIENT_HEARTBEAT, DEFAULT_RUNTIME_CLIENT_TTL,
    DEFAULT_RUNTIME_EVENT_BATCH_LIMIT, DEFAULT_RUNTIME_EVENT_CAPACITY,
    DEFAULT_RUNTIME_SWEEP_INTERVAL, RuntimeBackendSnapshot, RuntimeLeaseGuard, RuntimeLeaseTarget,
    RuntimeLifecycleBackend, RuntimeLifecycleClock, RuntimeLifecycleConfig,
    RuntimeLifecyclePublisher, RuntimeLifecycleService, RuntimeSweepReport,
    SystemRuntimeLifecycleClock,
};
pub use runtime_route::default_adapter_for_agent;
pub use runtime_selection::{
    DEFAULT_RUNTIME_SELECTION_POLL_INTERVAL, DEFAULT_SEAMLESS_RUNTIME_SWITCH_WAIT_DEADLINE_MS,
    ResolvedInitialRuntimeSelection, ResolvedRuntimeSelection, RuntimeSelectionResolver,
    RuntimeSelectionService, RuntimeSelectionServiceConfig,
};
pub use runtime_switch::{
    ActiveWorkGate, ActiveWorkSnapshot, JournaledOperation, OperationReconcileOutcome,
    PreparedAttachment, PreparedProcess, RestoreAssessment, RuntimeSwitchCoordinator,
    RuntimeSwitchCoordinatorConfig, RuntimeSwitchReconcileError, RuntimeSwitchReconcileReport,
    RuntimeSwitchRequest, RuntimeSwitchStrategy, SwitchIntent, SwitchOutcome,
    SwitchTargetAssessment, SwitchTargetExecutor, decide_switch_strategy,
};
pub use scheduler::{
    DEFAULT_SCHEDULED_TASK_DUE_LIMIT, DEFAULT_SCHEDULED_TASK_STALE_AFTER_MS,
    ScheduledTaskRunOutcome, ScheduledTaskRunner, ScheduledTaskTickResult, next_run_after,
};
pub use smoke_workspace::{
    AGENT_SMOKE_FORBIDDEN_ROOT_ENV, AGENT_SMOKE_WORKSPACE_ENV, forbidden_agent_smoke_root,
    reject_forbidden_agent_smoke_workspace, resolve_agent_smoke_workspace,
};
pub use state_machine::{is_transition_allowed, validate_transition};

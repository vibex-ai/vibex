use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::Duration;

use async_trait::async_trait;
use tokio::time::sleep;
use vibex_core::{
    AgentSessionRuntimeSelectionState, GetMessageSubmissionRequest, MessageSubmissionId,
    MessageSubmissionState, MessageSubmissionStatus, RuntimeLeaseRole, RuntimeSelectionInteraction,
    RuntimeSwitchId, RuntimeSwitchStatus, SendAgentMessageRequest, SessionRuntimeSelectionStatus,
    SetDesiredAgentSessionRuntimeRequest, TimelineItem, TimelinePayload, VibexError, VibexResult,
    VibexSessionId, unix_timestamp_ms,
};
use vibex_db::{
    MessageSubmissionRecord, MessageSubmissionRepository, RuntimeSwitchRepository,
    TimelineRepository, apply_migrations, open_database,
};

use crate::manager::AgentManager;
use crate::runtime_lifecycle::{RuntimeLeaseGuard, RuntimeLifecycleService};
use crate::runtime_selection::RuntimeSelectionService;
use crate::{
    RuntimeLogContext, RuntimeLogLevel, RuntimeMetricName, RuntimeMetricResult,
    RuntimeObservability,
};

pub const DEFAULT_MESSAGE_SUBMISSION_POLL_INTERVAL: Duration = Duration::from_millis(25);

const AMBIGUOUS_PROMPT_ERROR_DETAIL: &str =
    "prompt dispatch began, but no durable provider result was recorded";
const PRE_DISPATCH_ERROR_DETAIL: &str = "message submission could not be prepared for dispatch";
const RUNTIME_PREPARATION_ERROR_DETAIL: &str = "required runtime could not be prepared";

#[derive(Debug, Clone)]
pub struct MessageSubmissionCoordinatorConfig {
    pub poll_interval: Duration,
}

impl Default for MessageSubmissionCoordinatorConfig {
    fn default() -> Self {
        Self {
            poll_interval: DEFAULT_MESSAGE_SUBMISSION_POLL_INTERVAL,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageSubmissionReconcileReport {
    pub resumed_sessions: usize,
    pub completed: usize,
    pub ambiguous: usize,
}

#[async_trait]
pub trait MessageRuntimeSelection: Send + Sync {
    fn get_selection_state(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState>;

    async fn set_desired_runtime(
        &self,
        request: SetDesiredAgentSessionRuntimeRequest,
    ) -> VibexResult<AgentSessionRuntimeSelectionState>;
}

#[async_trait]
impl MessageRuntimeSelection for RuntimeSelectionService {
    fn get_selection_state(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        RuntimeSelectionService::get_selection_state(self, session_id)
    }

    async fn set_desired_runtime(
        &self,
        request: SetDesiredAgentSessionRuntimeRequest,
    ) -> VibexResult<AgentSessionRuntimeSelectionState> {
        RuntimeSelectionService::set_desired_runtime(self, request).await
    }
}

#[async_trait]
pub trait MessageDispatchExecutor: Send + Sync {
    async fn dispatch_message(
        &self,
        submission_id: MessageSubmissionId,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>>;
}

#[async_trait]
impl MessageDispatchExecutor for AgentManager {
    async fn dispatch_message(
        &self,
        submission_id: MessageSubmissionId,
        request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        self.dispatch_message_direct(Some(submission_id), request)
            .await
    }
}

pub fn manager_message_dispatcher(
    manager: &Arc<AgentManager>,
) -> Weak<dyn MessageDispatchExecutor> {
    let executor: Arc<dyn MessageDispatchExecutor> = manager.clone();
    Arc::downgrade(&executor)
}

pub struct MessageSubmissionCoordinator {
    db_path: PathBuf,
    runtime_selection: Arc<dyn MessageRuntimeSelection>,
    dispatcher: Weak<dyn MessageDispatchExecutor>,
    config: MessageSubmissionCoordinatorConfig,
    observability: Arc<RuntimeObservability>,
    watched_sessions: Mutex<HashSet<VibexSessionId>>,
    runtime_lifecycle: Mutex<Option<Arc<RuntimeLifecycleService>>>,
}

impl MessageSubmissionCoordinator {
    pub fn new(
        db_path: impl Into<PathBuf>,
        runtime_selection: Arc<dyn MessageRuntimeSelection>,
        dispatcher: Weak<dyn MessageDispatchExecutor>,
        config: MessageSubmissionCoordinatorConfig,
    ) -> VibexResult<Self> {
        Self::new_with_observability(
            db_path,
            runtime_selection,
            dispatcher,
            config,
            Arc::new(RuntimeObservability::new()),
        )
    }

    pub fn new_with_observability(
        db_path: impl Into<PathBuf>,
        runtime_selection: Arc<dyn MessageRuntimeSelection>,
        dispatcher: Weak<dyn MessageDispatchExecutor>,
        config: MessageSubmissionCoordinatorConfig,
        observability: Arc<RuntimeObservability>,
    ) -> VibexResult<Self> {
        if config.poll_interval.is_zero() {
            return Err(VibexError::validation(
                "message_submission_config_invalid",
                "message submission poll interval must be positive",
            ));
        }
        let db_path = db_path.into();
        let mut conn = open_database(&db_path)?;
        apply_migrations(&mut conn)?;
        Ok(Self {
            db_path,
            runtime_selection,
            dispatcher,
            config,
            observability,
            watched_sessions: Mutex::new(HashSet::new()),
            runtime_lifecycle: Mutex::new(None),
        })
    }

    pub fn install_runtime_lifecycle(
        &self,
        lifecycle: Arc<RuntimeLifecycleService>,
    ) -> VibexResult<()> {
        self.runtime_lifecycle
            .lock()
            .map_err(|_| {
                VibexError::process(
                    "message_submission_lock_poisoned",
                    "message submission lifecycle lock is poisoned",
                )
            })?
            .replace(lifecycle);
        Ok(())
    }

    pub fn database_path(&self) -> &Path {
        &self.db_path
    }

    pub async fn submit(
        self: &Arc<Self>,
        mut request: SendAgentMessageRequest,
    ) -> VibexResult<Vec<TimelineItem>> {
        request.message_idempotency_key = request.message_idempotency_key.trim().to_string();
        if request.text.trim().is_empty() && request.attachments.is_empty() {
            return Err(VibexError::validation(
                "empty_agent_message",
                "Agent message text or attachments must not be empty",
            ));
        }
        if request.reasoning_effort != request.desired_runtime.reasoning_effort {
            return Err(VibexError::validation(
                "message_submission_runtime_config_mismatch",
                "message reasoning effort must match the desired runtime selection",
            ));
        }

        let proposed_id = MessageSubmissionId::new();
        let record = {
            let mut conn = self.open_connection()?;
            MessageSubmissionRepository::enqueue(&mut conn, proposed_id.clone(), &request)?
        };
        if record.submission_id != proposed_id {
            self.observability.increment(
                RuntimeMetricName::DuplicateSubmissionPrevented,
                None,
                RuntimeMetricResult::Prevented,
            );
            RuntimeLogContext::new("message_submission_enqueue")
                .with_logical_session_id(&record.session_id)
                .emit(
                    RuntimeLogLevel::Info,
                    "runtime_duplicate_submission_prevented",
                    RuntimeMetricResult::Prevented,
                    None,
                    None,
                );
        }
        self.start_session_worker(record.session_id.clone())?;
        self.wait_for_terminal(&record.submission_id).await
    }

    pub fn get_submission(
        &self,
        request: &GetMessageSubmissionRequest,
    ) -> VibexResult<MessageSubmissionState> {
        let conn = self.open_connection()?;
        MessageSubmissionRepository::get_by_key(
            &conn,
            &request.session_id,
            request.message_idempotency_key.trim(),
        )?
        .map(|record| record.state())
        .ok_or_else(|| {
            VibexError::validation(
                "message_submission_not_found",
                "message submission was not found",
            )
        })
    }

    pub fn reconcile_on_startup(self: &Arc<Self>) -> VibexResult<MessageSubmissionReconcileReport> {
        let records = {
            let conn = self.open_connection()?;
            MessageSubmissionRepository::list_non_terminal(&conn)?
        };
        let mut report = MessageSubmissionReconcileReport::default();
        let mut sessions = HashSet::new();
        for record in records {
            sessions.insert(record.session_id.clone());
            match record.status {
                MessageSubmissionStatus::AboutToPrompt => {
                    let conn = self.open_connection()?;
                    if record.result_first_sequence.is_some()
                        && record.result_last_sequence.is_some()
                        && record.user_message_timeline_item_id.is_some()
                    {
                        MessageSubmissionRepository::advance_status(
                            &conn,
                            &record.submission_id,
                            MessageSubmissionStatus::AboutToPrompt,
                            MessageSubmissionStatus::Dispatched,
                        )?;
                        MessageSubmissionRepository::advance_status(
                            &conn,
                            &record.submission_id,
                            MessageSubmissionStatus::Dispatched,
                            MessageSubmissionStatus::Completed,
                        )?;
                        report.completed += 1;
                    } else {
                        MessageSubmissionRepository::mark_ambiguous(
                            &conn,
                            &record.submission_id,
                            Some("application restarted after prompt dispatch began"),
                        )?;
                        self.record_ambiguous(&record);
                        report.ambiguous += 1;
                    }
                }
                MessageSubmissionStatus::Dispatched => {
                    let conn = self.open_connection()?;
                    MessageSubmissionRepository::advance_status(
                        &conn,
                        &record.submission_id,
                        MessageSubmissionStatus::Dispatched,
                        MessageSubmissionStatus::Completed,
                    )?;
                    report.completed += 1;
                }
                MessageSubmissionStatus::AwaitingRuntime
                | MessageSubmissionStatus::ReadyToDispatch => {}
                MessageSubmissionStatus::Completed
                | MessageSubmissionStatus::Failed
                | MessageSubmissionStatus::Cancelled
                | MessageSubmissionStatus::AmbiguousPromptDispatch => unreachable!(),
            }
        }
        for session_id in sessions {
            if self.has_non_terminal(&session_id)? {
                self.start_session_worker(session_id)?;
                report.resumed_sessions += 1;
            }
        }
        Ok(report)
    }

    fn start_session_worker(self: &Arc<Self>, session_id: VibexSessionId) -> VibexResult<()> {
        {
            let mut watched = self.watched_sessions.lock().map_err(|_| {
                VibexError::process(
                    "message_submission_worker_lock_poisoned",
                    "message submission worker registry is unavailable",
                )
            })?;
            if !watched.insert(session_id.clone()) {
                return Ok(());
            }
        }
        let coordinator = self.clone();
        tokio::spawn(async move {
            coordinator.drain_session(session_id).await;
        });
        Ok(())
    }

    async fn drain_session(self: Arc<Self>, session_id: VibexSessionId) {
        loop {
            let record = match self.head_non_terminal(&session_id) {
                Ok(Some(record)) => record,
                Ok(None) => {
                    self.finish_session_worker(&session_id);
                    return;
                }
                Err(_) => {
                    sleep(self.config.poll_interval).await;
                    continue;
                }
            };
            if let Err(error) = self.drive_submission(&record).await {
                self.terminalize_drive_error(&record.submission_id, &error);
            }
        }
    }

    async fn drive_submission(&self, record: &MessageSubmissionRecord) -> VibexResult<()> {
        match record.status {
            MessageSubmissionStatus::AwaitingRuntime => self.await_runtime(record).await,
            MessageSubmissionStatus::ReadyToDispatch => self.dispatch(record).await,
            MessageSubmissionStatus::AboutToPrompt => {
                let conn = self.open_connection()?;
                MessageSubmissionRepository::mark_ambiguous(
                    &conn,
                    &record.submission_id,
                    Some("prompt dispatch result is not durably known"),
                )?;
                self.record_ambiguous(record);
                Ok(())
            }
            MessageSubmissionStatus::Dispatched => {
                let conn = self.open_connection()?;
                MessageSubmissionRepository::advance_status(
                    &conn,
                    &record.submission_id,
                    MessageSubmissionStatus::Dispatched,
                    MessageSubmissionStatus::Completed,
                )
            }
            MessageSubmissionStatus::Completed
            | MessageSubmissionStatus::Failed
            | MessageSubmissionStatus::Cancelled
            | MessageSubmissionStatus::AmbiguousPromptDispatch => Ok(()),
        }
    }

    async fn await_runtime(&self, record: &MessageSubmissionRecord) -> VibexResult<()> {
        loop {
            let current = self.required_submission(&record.submission_id)?;
            if current.status != MessageSubmissionStatus::AwaitingRuntime {
                return Ok(());
            }
            let state = self
                .runtime_selection
                .get_selection_state(&current.session_id)?;
            if state.effective == current.desired_runtime_selection
                && state.status == SessionRuntimeSelectionStatus::Ready
            {
                let conn = self.open_connection()?;
                MessageSubmissionRepository::advance_status(
                    &conn,
                    &current.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    MessageSubmissionStatus::ReadyToDispatch,
                )?;
                self.record_queue_wait(&current, RuntimeMetricResult::Success);
                return Ok(());
            }

            if let Some(switch_id) = current.required_switch_id.as_ref() {
                if self.handle_terminal_switch(&current, switch_id)? {
                    return Ok(());
                }
            } else if state.status == SessionRuntimeSelectionStatus::FailedUsingPrevious
                && state.pending_switch_id.is_none()
            {
                let error_code = state
                    .actionable_error
                    .as_ref()
                    .map(|error| error.code.as_str())
                    .unwrap_or("message_submission_runtime_unavailable");
                let error_detail = state
                    .actionable_error
                    .as_ref()
                    .map(|error| error.message.as_str())
                    .unwrap_or("required runtime did not become effective");
                let conn = self.open_connection()?;
                MessageSubmissionRepository::fail(
                    &conn,
                    &current.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    &safe_error_code(error_code),
                    Some(&safe_error_detail(error_detail)),
                )?;
                self.record_queue_wait(&current, RuntimeMetricResult::Failure);
                return Ok(());
            } else if state.desired == current.desired_runtime_selection {
                if let Some(switch_id) = state.pending_switch_id.as_ref() {
                    let conn = self.open_connection()?;
                    MessageSubmissionRepository::associate_required_switch(
                        &conn,
                        &current.submission_id,
                        switch_id,
                    )?;
                }
            } else {
                let result = self
                    .runtime_selection
                    .set_desired_runtime(SetDesiredAgentSessionRuntimeRequest {
                        session_id: current.session_id.clone(),
                        idempotency_key: format!(
                            "message-runtime:{}",
                            current.submission_id.as_str()
                        ),
                        expected_revision: state.session_revision,
                        expected_selection_revision: state.selection_revision,
                        desired: current.desired_runtime_selection.clone(),
                        interaction: RuntimeSelectionInteraction::Seamless,
                    })
                    .await;
                match result {
                    Ok(next) => {
                        if let Some(switch_id) = next.pending_switch_id.as_ref() {
                            let conn = self.open_connection()?;
                            MessageSubmissionRepository::associate_required_switch(
                                &conn,
                                &current.submission_id,
                                switch_id,
                            )?;
                        }
                    }
                    Err(error) if is_runtime_revision_conflict(&error) => {}
                    Err(error) => return Err(error),
                }
            }
            sleep(self.config.poll_interval).await;
        }
    }

    fn handle_terminal_switch(
        &self,
        record: &MessageSubmissionRecord,
        switch_id: &RuntimeSwitchId,
    ) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        let Some(runtime_switch) = RuntimeSwitchRepository::get(&conn, switch_id)? else {
            return Err(VibexError::storage(
                "message_submission_runtime_switch_missing",
                "required runtime switch was not found",
            ));
        };
        match runtime_switch.status {
            RuntimeSwitchStatus::Cancelled => {
                MessageSubmissionRepository::advance_status(
                    &conn,
                    &record.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    MessageSubmissionStatus::Cancelled,
                )?;
                self.record_queue_wait(record, RuntimeMetricResult::Cancelled);
                Ok(true)
            }
            RuntimeSwitchStatus::Failed
            | RuntimeSwitchStatus::Superseded
            | RuntimeSwitchStatus::AmbiguousExternalEffect => {
                MessageSubmissionRepository::fail(
                    &conn,
                    &record.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    &safe_error_code(
                        runtime_switch
                            .error_code
                            .as_deref()
                            .unwrap_or("message_submission_runtime_unavailable"),
                    ),
                    Some("required runtime did not become effective"),
                )?;
                self.record_queue_wait(
                    record,
                    if runtime_switch.status == RuntimeSwitchStatus::AmbiguousExternalEffect {
                        RuntimeMetricResult::Ambiguous
                    } else {
                        RuntimeMetricResult::Failure
                    },
                );
                Ok(true)
            }
            RuntimeSwitchStatus::Committed => {
                // The caller's selection snapshot predates this switch read. A
                // commit may have made the required runtime effective between
                // those two reads, so classify only from a fresh snapshot.
                let state = self
                    .runtime_selection
                    .get_selection_state(&record.session_id)?;
                if state.effective == record.desired_runtime_selection
                    && state.status == SessionRuntimeSelectionStatus::Ready
                {
                    MessageSubmissionRepository::advance_status(
                        &conn,
                        &record.submission_id,
                        MessageSubmissionStatus::AwaitingRuntime,
                        MessageSubmissionStatus::ReadyToDispatch,
                    )?;
                    self.record_queue_wait(record, RuntimeMetricResult::Success);
                    return Ok(true);
                }
                MessageSubmissionRepository::fail(
                    &conn,
                    &record.submission_id,
                    MessageSubmissionStatus::AwaitingRuntime,
                    "message_submission_runtime_changed_after_commit",
                    Some("required runtime is no longer effective"),
                )?;
                self.record_queue_wait(record, RuntimeMetricResult::Failure);
                Ok(true)
            }
            RuntimeSwitchStatus::Requested
            | RuntimeSwitchStatus::Reserved
            | RuntimeSwitchStatus::WaitingForIdle
            | RuntimeSwitchStatus::Preparing
            | RuntimeSwitchStatus::Prepared
            | RuntimeSwitchStatus::Committing => Ok(false),
        }
    }

    async fn dispatch(&self, record: &MessageSubmissionRecord) -> VibexResult<()> {
        let runtime_state = self
            .runtime_selection
            .get_selection_state(&record.session_id)?;
        if runtime_state.status != SessionRuntimeSelectionStatus::Ready
            || runtime_state.effective != record.desired_runtime_selection
        {
            let conn = self.open_connection()?;
            MessageSubmissionRepository::advance_status(
                &conn,
                &record.submission_id,
                MessageSubmissionStatus::ReadyToDispatch,
                MessageSubmissionStatus::AwaitingRuntime,
            )?;
            return Ok(());
        }
        let payload = {
            let conn = self.open_connection()?;
            MessageSubmissionRepository::get_payload(&conn, &record.submission_id)?.ok_or_else(
                || {
                    VibexError::storage(
                        "message_submission_payload_missing",
                        "durable message submission payload was not found",
                    )
                },
            )?
        };
        if payload.submission_id != record.submission_id
            || payload.session_id != record.session_id
            || payload.submission_sequence != record.submission_sequence
            || payload.request.session_id != record.session_id
            || payload.request.message_idempotency_key != record.message_idempotency_key
            || payload.request.desired_runtime != record.desired_runtime_selection
        {
            return Err(VibexError::storage(
                "message_submission_payload_mismatch",
                "durable message submission payload does not match its submission",
            ));
        }
        let expected_user_text = payload.request.text.clone();
        let expected_user_attachments = payload.request.attachments.clone();
        let dispatch_start_sequence = {
            let conn = self.open_connection()?;
            TimelineRepository::latest_sequence(&conn, &record.session_id)?
        };
        let lifecycle = self
            .runtime_lifecycle
            .lock()
            .map_err(|_| {
                VibexError::process(
                    "message_submission_lock_poisoned",
                    "message submission lifecycle lock is poisoned",
                )
            })?
            .clone();
        let _runtime_lease: Option<RuntimeLeaseGuard> = match lifecycle {
            Some(lifecycle) => Some(
                lifecycle
                    .materialize_internal(
                        record.session_id.clone(),
                        RuntimeLeaseRole::BackgroundWorker,
                        format!("submission:{}", record.submission_id.as_str()),
                    )
                    .await?,
            ),
            None => None,
        };
        let dispatcher = self.dispatcher.upgrade().ok_or_else(|| {
            VibexError::process(
                "message_submission_dispatcher_unavailable",
                "message submission dispatcher is unavailable",
            )
        })?;
        {
            let conn = self.open_connection()?;
            MessageSubmissionRepository::mark_about_to_prompt(&conn, &record.submission_id)?;
        }
        let result = dispatcher
            .dispatch_message(record.submission_id.clone(), payload.request)
            .await;
        let items = match result {
            Ok(items) => items,
            Err(_) => {
                if let Some(items) = self.persisted_dispatch_result(
                    record,
                    dispatch_start_sequence,
                    &expected_user_text,
                    &expected_user_attachments,
                )? {
                    return self.complete_dispatch(
                        record,
                        &expected_user_text,
                        &expected_user_attachments,
                        &items,
                    );
                }
                let conn = self.open_connection()?;
                MessageSubmissionRepository::mark_ambiguous(
                    &conn,
                    &record.submission_id,
                    Some(AMBIGUOUS_PROMPT_ERROR_DETAIL),
                )?;
                self.record_ambiguous(record);
                return Ok(());
            }
        };
        self.complete_dispatch(
            record,
            &expected_user_text,
            &expected_user_attachments,
            &items,
        )
    }

    fn persisted_dispatch_result(
        &self,
        record: &MessageSubmissionRecord,
        dispatch_start_sequence: i64,
        expected_user_text: &str,
        expected_user_attachments: &[vibex_core::MessageAttachment],
    ) -> VibexResult<Option<Vec<TimelineItem>>> {
        let conn = self.open_connection()?;
        let last_sequence = TimelineRepository::latest_sequence(&conn, &record.session_id)?;
        if last_sequence <= dispatch_start_sequence {
            return Ok(None);
        }
        let items = TimelineRepository::fetch_range(
            &conn,
            &record.session_id,
            dispatch_start_sequence + 1,
            last_sequence,
        )?;
        let matching_user_index = items.iter().position(|item| {
            matches!(
                &item.payload,
                TimelinePayload::UserMessage(payload)
                    if payload.text == expected_user_text
                        && payload.attachments == expected_user_attachments
            )
        });
        let Some(matching_user_index) = matching_user_index else {
            return Ok(None);
        };
        let provider_output_persisted = items[matching_user_index + 1..].iter().any(|item| {
            matches!(
                item.source,
                vibex_core::TimelineSource::Agent | vibex_core::TimelineSource::Provider
            ) && !matches!(item.payload, TimelinePayload::Error(_))
        });
        Ok(provider_output_persisted.then_some(items))
    }

    fn complete_dispatch(
        &self,
        record: &MessageSubmissionRecord,
        expected_user_text: &str,
        expected_user_attachments: &[vibex_core::MessageAttachment],
        items: &[TimelineItem],
    ) -> VibexResult<()> {
        if items
            .iter()
            .any(|item| item.session_id != record.session_id)
        {
            return Err(VibexError::storage(
                "message_submission_result_session_mismatch",
                "message dispatch returned Timeline items for another session",
            ));
        }
        let mut user_items = items
            .iter()
            .filter(|item| matches!(item.payload, TimelinePayload::UserMessage(_)));
        let user_item = user_items.next().ok_or_else(|| {
            VibexError::storage(
                "message_submission_user_timeline_item_missing",
                "message dispatch did not produce a user Timeline item",
            )
        })?;
        if user_items.next().is_some() {
            return Err(VibexError::storage(
                "message_submission_user_timeline_item_ambiguous",
                "message dispatch returned more than one user Timeline item",
            ));
        }
        let TimelinePayload::UserMessage(user_payload) = &user_item.payload else {
            return Err(VibexError::storage(
                "message_submission_user_timeline_item_mismatch",
                "message dispatch user Timeline item has an invalid payload kind",
            ));
        };
        if user_payload.text != expected_user_text
            || user_payload.attachments != expected_user_attachments
        {
            return Err(VibexError::storage(
                "message_submission_user_timeline_item_mismatch",
                "message dispatch user Timeline item does not match the durable payload",
            ));
        }
        let first_sequence = items
            .iter()
            .map(|item| item.sequence)
            .min()
            .ok_or_else(|| {
                VibexError::storage(
                    "message_submission_result_empty",
                    "message dispatch returned no Timeline items",
                )
            })?;
        let last_sequence = items
            .iter()
            .map(|item| item.sequence)
            .max()
            .unwrap_or(first_sequence);
        let provider_correlation_id = items
            .iter()
            .find_map(|item| item.provider_correlation_id.as_deref());
        let mut conn = self.open_connection()?;
        MessageSubmissionRepository::record_dispatch_result(
            &mut conn,
            &record.submission_id,
            &user_item.id,
            provider_correlation_id,
            first_sequence,
            last_sequence,
        )?;
        MessageSubmissionRepository::advance_status(
            &conn,
            &record.submission_id,
            MessageSubmissionStatus::Dispatched,
            MessageSubmissionStatus::Completed,
        )
    }

    async fn wait_for_terminal(
        &self,
        submission_id: &MessageSubmissionId,
    ) -> VibexResult<Vec<TimelineItem>> {
        loop {
            let record = self.required_submission(submission_id)?;
            match record.status {
                MessageSubmissionStatus::Completed => return self.load_result(&record),
                MessageSubmissionStatus::Failed => {
                    return Err(VibexError::provider(
                        record
                            .error_code
                            .unwrap_or_else(|| "message_submission_failed".to_string()),
                        record.error_detail_redacted.unwrap_or_else(|| {
                            "message submission could not be dispatched".to_string()
                        }),
                    ));
                }
                MessageSubmissionStatus::Cancelled => {
                    return Err(VibexError::conflict(
                        record
                            .error_code
                            .unwrap_or_else(|| "message_submission_cancelled".to_string()),
                        "message submission was cancelled before dispatch",
                    ));
                }
                MessageSubmissionStatus::AmbiguousPromptDispatch => {
                    return Err(VibexError::provider(
                        record.error_code.unwrap_or_else(|| {
                            "message_submission_prompt_dispatch_ambiguous".to_string()
                        }),
                        "the provider may have received this message; it was not sent again",
                    )
                    .with_recovery_hint(
                        "Inspect the authoritative timeline before submitting the message again",
                    ));
                }
                MessageSubmissionStatus::AwaitingRuntime
                | MessageSubmissionStatus::ReadyToDispatch
                | MessageSubmissionStatus::AboutToPrompt
                | MessageSubmissionStatus::Dispatched => {
                    sleep(self.config.poll_interval).await;
                }
            }
        }
    }

    fn load_result(&self, record: &MessageSubmissionRecord) -> VibexResult<Vec<TimelineItem>> {
        let (Some(first), Some(last)) = (record.result_first_sequence, record.result_last_sequence)
        else {
            return Err(VibexError::storage(
                "message_submission_result_missing",
                "completed message submission has no durable Timeline result range",
            ));
        };
        let conn = self.open_connection()?;
        TimelineRepository::fetch_range(&conn, &record.session_id, first, last)
    }

    fn terminalize_drive_error(&self, submission_id: &MessageSubmissionId, error: &VibexError) {
        let Ok(record) = self.required_submission(submission_id) else {
            return;
        };
        let Ok(conn) = self.open_connection() else {
            return;
        };
        match record.status {
            MessageSubmissionStatus::AwaitingRuntime | MessageSubmissionStatus::ReadyToDispatch => {
                let detail = if record.status == MessageSubmissionStatus::AwaitingRuntime {
                    RUNTIME_PREPARATION_ERROR_DETAIL
                } else {
                    PRE_DISPATCH_ERROR_DETAIL
                };
                let _ = MessageSubmissionRepository::fail(
                    &conn,
                    submission_id,
                    record.status,
                    &safe_error_code(&error.code),
                    Some(detail),
                );
                self.record_queue_wait(&record, RuntimeMetricResult::Failure);
            }
            MessageSubmissionStatus::AboutToPrompt => {
                if MessageSubmissionRepository::mark_ambiguous(
                    &conn,
                    submission_id,
                    Some(AMBIGUOUS_PROMPT_ERROR_DETAIL),
                )
                .is_ok()
                {
                    self.record_ambiguous(&record);
                }
            }
            MessageSubmissionStatus::Dispatched => {
                let _ = MessageSubmissionRepository::advance_status(
                    &conn,
                    submission_id,
                    MessageSubmissionStatus::Dispatched,
                    MessageSubmissionStatus::Completed,
                );
            }
            MessageSubmissionStatus::Completed
            | MessageSubmissionStatus::Failed
            | MessageSubmissionStatus::Cancelled
            | MessageSubmissionStatus::AmbiguousPromptDispatch => {}
        }
    }

    fn finish_session_worker(self: &Arc<Self>, session_id: &VibexSessionId) {
        if let Ok(mut watched) = self.watched_sessions.lock() {
            watched.remove(session_id);
        }
        if self.has_non_terminal(session_id).unwrap_or(false) {
            let _ = self.start_session_worker(session_id.clone());
        }
    }

    fn record_queue_wait(&self, record: &MessageSubmissionRecord, result: RuntimeMetricResult) {
        let duration_ms = unix_timestamp_ms()
            .saturating_sub(record.created_at_ms)
            .max(0) as u64;
        self.observability.observe_duration_ms(
            RuntimeMetricName::QueuedMessageWait,
            None,
            result,
            duration_ms,
        );
        RuntimeLogContext::new("queued_message_wait")
            .with_logical_session_id(&record.session_id)
            .emit(
                if result == RuntimeMetricResult::Success {
                    RuntimeLogLevel::Debug
                } else {
                    RuntimeLogLevel::Warn
                },
                "runtime_queued_message_wait",
                result,
                record.error_code.as_deref(),
                Some(duration_ms),
            );
    }

    fn record_ambiguous(&self, record: &MessageSubmissionRecord) {
        self.observability.increment(
            RuntimeMetricName::AmbiguousPromptDispatch,
            None,
            RuntimeMetricResult::Ambiguous,
        );
        RuntimeLogContext::new("prompt_dispatch")
            .with_logical_session_id(&record.session_id)
            .emit(
                RuntimeLogLevel::Warn,
                "runtime_prompt_dispatch_ambiguous",
                RuntimeMetricResult::Ambiguous,
                Some("message_submission_prompt_dispatch_ambiguous"),
                None,
            );
    }

    fn has_non_terminal(&self, session_id: &VibexSessionId) -> VibexResult<bool> {
        Ok(self.head_non_terminal(session_id)?.is_some())
    }

    fn head_non_terminal(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<MessageSubmissionRecord>> {
        let conn = self.open_connection()?;
        MessageSubmissionRepository::get_head_non_terminal(&conn, session_id)
    }

    fn required_submission(
        &self,
        submission_id: &MessageSubmissionId,
    ) -> VibexResult<MessageSubmissionRecord> {
        let conn = self.open_connection()?;
        MessageSubmissionRepository::get(&conn, submission_id)?.ok_or_else(|| {
            VibexError::storage(
                "message_submission_missing",
                "durable message submission was not found",
            )
        })
    }

    fn open_connection(&self) -> VibexResult<vibex_db::DbConnection> {
        open_database(&self.db_path)
    }
}

fn is_runtime_revision_conflict(error: &VibexError) -> bool {
    matches!(
        error.code.as_str(),
        "runtime_switch_revision_conflict" | "desired_selection_revision_conflict"
    )
}

fn safe_error_code(code: &str) -> String {
    let code = code
        .chars()
        .filter(|character| !character.is_control())
        .take(160)
        .collect::<String>();
    if code.is_empty() {
        "message_submission_failed".to_string()
    } else {
        code
    }
}

fn safe_error_detail(message: &str) -> String {
    message
        .chars()
        .map(|character| {
            if character.is_control() {
                ' '
            } else {
                character
            }
        })
        .take(512)
        .collect()
}

#[cfg(test)]
mod tests {
    use std::collections::{HashMap, VecDeque};
    use std::fs;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    use tokio::sync::Notify;
    use vibex_core::{
        AcpAdapterId, AgentId, AgentMessagePayload, AgentSession, AgentSessionSafety,
        AgentSessionState, MAX_MESSAGE_IDEMPOTENCY_KEY_LEN, ProviderProfileId, RequestId,
        RuntimeSelectionActionableError, SessionRuntimeSelection, TimelineRedactionState,
        TimelineSource, UserMessagePayload, WorkspaceMode, unix_timestamp_ms,
    };
    use vibex_db::{RuntimeSwitchReserveRequest, SessionRepository, WorkspaceRepository};

    use super::*;

    struct MockRuntimeSelection {
        states: Mutex<HashMap<VibexSessionId, AgentSessionRuntimeSelectionState>>,
        scripted_reads: Mutex<HashMap<VibexSessionId, VecDeque<AgentSessionRuntimeSelectionState>>>,
        set_calls: AtomicUsize,
    }

    #[async_trait]
    impl MessageRuntimeSelection for MockRuntimeSelection {
        fn get_selection_state(
            &self,
            session_id: &VibexSessionId,
        ) -> VibexResult<AgentSessionRuntimeSelectionState> {
            if let Some(state) = self
                .scripted_reads
                .lock()
                .unwrap()
                .get_mut(session_id)
                .and_then(VecDeque::pop_front)
            {
                return Ok(state);
            }
            self.states
                .lock()
                .unwrap()
                .get(session_id)
                .cloned()
                .ok_or_else(|| {
                    VibexError::validation("session_not_found", "Agent session was not found")
                })
        }

        async fn set_desired_runtime(
            &self,
            request: SetDesiredAgentSessionRuntimeRequest,
        ) -> VibexResult<AgentSessionRuntimeSelectionState> {
            self.set_calls.fetch_add(1, Ordering::SeqCst);
            let mut states = self.states.lock().unwrap();
            let state = states.get_mut(&request.session_id).ok_or_else(|| {
                VibexError::validation("session_not_found", "Agent session was not found")
            })?;
            state.desired = request.desired.clone();
            state.effective = request.desired;
            state.status = SessionRuntimeSelectionStatus::Ready;
            state.selection_revision += 1;
            state.pending_switch_id = None;
            Ok(state.clone())
        }
    }

    struct MockDispatcher {
        db_path: PathBuf,
        calls: AtomicUsize,
        block: AtomicBool,
        started: Notify,
        release: Notify,
        dispatched: Mutex<Vec<(VibexSessionId, String, String)>>,
        failure_message: Mutex<Option<String>>,
        fail_after_output: AtomicBool,
    }

    #[async_trait]
    impl MessageDispatchExecutor for MockDispatcher {
        async fn dispatch_message(
            &self,
            _submission_id: MessageSubmissionId,
            request: SendAgentMessageRequest,
        ) -> VibexResult<Vec<TimelineItem>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.dispatched.lock().unwrap().push((
                request.session_id.clone(),
                request.message_idempotency_key.clone(),
                request.desired_runtime.model_id.clone(),
            ));
            self.started.notify_one();
            if self.block.swap(false, Ordering::SeqCst) {
                self.release.notified().await;
            }
            let failure_message = self.failure_message.lock().unwrap().take();
            if let Some(message) = failure_message.as_ref()
                && !self.fail_after_output.load(Ordering::SeqCst)
            {
                return Err(VibexError::provider("mock_dispatch_failed", message));
            }
            let mut conn = open_database(&self.db_path)?;
            let user = TimelineRepository::append(
                &mut conn,
                &request.session_id,
                TimelineSource::User,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: request.text,
                    attachments: request.attachments,
                }),
                request.correlation_id.as_ref(),
                None,
                TimelineRedactionState::None,
            )?;
            let agent = TimelineRepository::append(
                &mut conn,
                &request.session_id,
                TimelineSource::Agent,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "completed".to_string(),
                    is_final: true,
                }),
                request.correlation_id.as_ref(),
                Some("mock-dispatch"),
                TimelineRedactionState::None,
            )?;
            if let Some(message) = failure_message {
                return Err(VibexError::provider("mock_dispatch_failed", message));
            }
            Ok(vec![user, agent])
        }
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-message-submission-{label}-{}.db",
            RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: PathBuf) {
        let _ = fs::remove_file(&path);
        let _ = fs::remove_file(path.with_extension("db-wal"));
        let _ = fs::remove_file(path.with_extension("db-shm"));
    }

    fn selection(model: &str) -> SessionRuntimeSelection {
        SessionRuntimeSelection {
            agent_id: AgentId::parse("codex").unwrap(),
            provider_profile_id: ProviderProfileId::parse("provider_codex_local").unwrap(),
            model_id: model.to_string(),
            reasoning_effort: None,
            mode_id: None,
            config_values: Default::default(),
        }
    }

    fn seed_session(
        db_path: &Path,
        label: &str,
        effective: SessionRuntimeSelection,
    ) -> VibexSessionId {
        let mut conn = open_database(db_path).unwrap();
        apply_migrations(&mut conn).unwrap();
        let workspace_root = std::env::temp_dir().join(format!(
            "vibex-message-workspace-{label}-{}",
            RequestId::new().as_str()
        ));
        fs::create_dir_all(&workspace_root).unwrap();
        let (project, workspace) =
            WorkspaceRepository::ensure(&conn, &workspace_root, WorkspaceMode::CurrentCheckout)
                .unwrap();
        let now = unix_timestamp_ms();
        let session = AgentSession {
            id: VibexSessionId::new(),
            title: label.to_string(),
            project_id: project.id,
            workspace_id: workspace.id,
            workspace_root: workspace.root_path,
            workspace_mode: workspace.mode,
            agent_id: effective.agent_id,
            state: AgentSessionState::Idle,
            safety: AgentSessionSafety::workspace_write_ask_on_risk(),
            created_at_ms: now,
            updated_at_ms: now,
            last_message_at_ms: now,
            archived_at_ms: None,
            deleted_at_ms: None,
        };
        SessionRepository::insert(&conn, &session).unwrap();
        session.id
    }

    fn harness(
        db_path: &Path,
        session_id: &VibexSessionId,
        initial: SessionRuntimeSelection,
        block_dispatch: bool,
    ) -> (
        Arc<MessageSubmissionCoordinator>,
        Arc<MockRuntimeSelection>,
        Arc<MockDispatcher>,
    ) {
        let runtime = Arc::new(MockRuntimeSelection {
            states: Mutex::new(HashMap::from([(
                session_id.clone(),
                AgentSessionRuntimeSelectionState {
                    desired: initial.clone(),
                    effective: initial,
                    status: SessionRuntimeSelectionStatus::Ready,
                    session_revision: 0,
                    selection_revision: 0,
                    current_binding_id: None,
                    activation_generation: 0,
                    pending_switch_id: None,
                    actionable_error: None,
                },
            )])),
            scripted_reads: Mutex::new(HashMap::new()),
            set_calls: AtomicUsize::new(0),
        });
        let dispatcher = Arc::new(MockDispatcher {
            db_path: db_path.to_path_buf(),
            calls: AtomicUsize::new(0),
            block: AtomicBool::new(block_dispatch),
            started: Notify::new(),
            release: Notify::new(),
            dispatched: Mutex::new(Vec::new()),
            failure_message: Mutex::new(None),
            fail_after_output: AtomicBool::new(false),
        });
        let dispatcher_trait: Arc<dyn MessageDispatchExecutor> = dispatcher.clone();
        let coordinator = Arc::new(
            MessageSubmissionCoordinator::new(
                db_path,
                runtime.clone(),
                Arc::downgrade(&dispatcher_trait),
                MessageSubmissionCoordinatorConfig {
                    poll_interval: Duration::from_millis(2),
                },
            )
            .unwrap(),
        );
        (coordinator, runtime, dispatcher)
    }

    fn request(
        session_id: &VibexSessionId,
        key: &str,
        desired_runtime: SessionRuntimeSelection,
    ) -> SendAgentMessageRequest {
        SendAgentMessageRequest {
            session_id: session_id.clone(),
            message_idempotency_key: key.to_string(),
            desired_runtime,
            text: "durable message".to_string(),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        }
    }

    fn enqueue_with_committed_switch(
        db_path: &Path,
        session_id: &VibexSessionId,
        key: &str,
        desired: &SessionRuntimeSelection,
    ) -> (MessageSubmissionRecord, RuntimeSwitchId) {
        let mut conn = open_database(db_path).unwrap();
        let submission = MessageSubmissionRepository::enqueue(
            &mut conn,
            MessageSubmissionId::new(),
            &request(session_id, key, desired.clone()),
        )
        .unwrap();
        let runtime_switch = RuntimeSwitchRepository::reserve(
            &mut conn,
            RuntimeSwitchId::new(),
            &RuntimeSwitchReserveRequest {
                session_id: session_id.clone(),
                idempotency_key: format!("{key}-switch"),
                expected_revision: 0,
                expected_current_binding_id: None,
                desired_selection_revision: 1,
                target_binding_id: None,
                target_agent_id: desired.agent_id.clone(),
                target_adapter_id: AcpAdapterId::parse("test-acp-adapter").unwrap(),
                target_profile_id: desired.provider_profile_id.clone(),
                requested_policy: None,
                active_work_policy: None,
                requested_session_config: None,
            },
        )
        .unwrap();
        for (from, to) in [
            (
                RuntimeSwitchStatus::Reserved,
                RuntimeSwitchStatus::Preparing,
            ),
            (
                RuntimeSwitchStatus::Preparing,
                RuntimeSwitchStatus::Prepared,
            ),
            (
                RuntimeSwitchStatus::Prepared,
                RuntimeSwitchStatus::Committing,
            ),
            (
                RuntimeSwitchStatus::Committing,
                RuntimeSwitchStatus::Committed,
            ),
        ] {
            RuntimeSwitchRepository::advance_status(&conn, &runtime_switch.switch_id, from, to)
                .unwrap();
        }
        MessageSubmissionRepository::associate_required_switch(
            &conn,
            &submission.submission_id,
            &runtime_switch.switch_id,
        )
        .unwrap();
        (submission, runtime_switch.switch_id)
    }

    #[tokio::test]
    async fn concurrent_same_key_dispatches_once_and_reuses_timeline_range() {
        let db_path = temp_db_path("idempotent");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "idempotent", initial.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        let first = {
            let coordinator = coordinator.clone();
            let request = request(&session_id, "same-key", initial.clone());
            tokio::spawn(async move { coordinator.submit(request).await })
        };
        let second = {
            let coordinator = coordinator.clone();
            let request = request(&session_id, "same-key", initial);
            tokio::spawn(async move { coordinator.submit(request).await })
        };

        let first_items = first.await.unwrap().unwrap();
        let second_items = second.await.unwrap().unwrap();
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        assert_eq!(runtime.set_calls.load(Ordering::SeqCst), 0);
        assert_eq!(first_items, second_items);
        assert_eq!(first_items.len(), 2);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn caller_drop_does_not_cancel_detached_dispatch() {
        let db_path = temp_db_path("caller-drop");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "caller-drop", initial.clone());
        let (coordinator, _runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), true);
        let handle = {
            let coordinator = coordinator.clone();
            let request = request(&session_id, "drop-key", initial);
            tokio::spawn(async move { coordinator.submit(request).await })
        };
        dispatcher.started.notified().await;
        handle.abort();
        dispatcher.release.notify_waiters();

        let record = tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let conn = open_database(&db_path).unwrap();
                let record =
                    MessageSubmissionRepository::get_by_key(&conn, &session_id, "drop-key")
                        .unwrap()
                        .unwrap();
                if record.status == MessageSubmissionStatus::Completed {
                    break record;
                }
                sleep(Duration::from_millis(2)).await;
            }
        })
        .await
        .unwrap();
        assert_eq!(record.status, MessageSubmissionStatus::Completed);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn same_session_submissions_dispatch_in_durable_sequence_order() {
        let db_path = temp_db_path("ordered");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "ordered", initial.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), true);
        let first_started = dispatcher.started.notified();
        let first = {
            let coordinator = coordinator.clone();
            let request = request(&session_id, "ordered-1", selection("gpt-5.1"));
            tokio::spawn(async move { coordinator.submit(request).await })
        };
        first_started.await;
        let second = {
            let coordinator = coordinator.clone();
            let request = request(&session_id, "ordered-2", selection("gpt-5.2"));
            tokio::spawn(async move { coordinator.submit(request).await })
        };

        sleep(Duration::from_millis(30)).await;
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        dispatcher.release.notify_one();
        first.await.unwrap().unwrap();
        second.await.unwrap().unwrap();

        assert_eq!(runtime.set_calls.load(Ordering::SeqCst), 2);
        let dispatched = dispatcher.dispatched.lock().unwrap();
        assert_eq!(
            dispatched
                .iter()
                .map(|(_, key, model)| (key.as_str(), model.as_str()))
                .collect::<Vec<_>>(),
            vec![("ordered-1", "gpt-5.1"), ("ordered-2", "gpt-5.2")]
        );
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn different_sessions_dispatch_in_parallel() {
        let db_path = temp_db_path("parallel");
        let initial = selection("gpt-5");
        let first_session = seed_session(&db_path, "parallel-1", initial.clone());
        let second_session = seed_session(&db_path, "parallel-2", initial.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &first_session, initial.clone(), true);
        runtime.states.lock().unwrap().insert(
            second_session.clone(),
            AgentSessionRuntimeSelectionState {
                desired: initial.clone(),
                effective: initial.clone(),
                status: SessionRuntimeSelectionStatus::Ready,
                session_revision: 0,
                selection_revision: 0,
                current_binding_id: None,
                activation_generation: 0,
                pending_switch_id: None,
                actionable_error: None,
            },
        );
        let first_started = dispatcher.started.notified();
        let first = {
            let coordinator = coordinator.clone();
            let request = request(&first_session, "parallel-1", initial.clone());
            tokio::spawn(async move { coordinator.submit(request).await })
        };
        first_started.await;

        let second_items = tokio::time::timeout(
            Duration::from_secs(1),
            coordinator.submit(request(&second_session, "parallel-2", initial)),
        )
        .await
        .expect("second session should not wait for the first session")
        .unwrap();
        assert_eq!(second_items.len(), 2);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 2);

        dispatcher.release.notify_one();
        first.await.unwrap().unwrap();
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn dispatch_rechecks_runtime_gate_before_prompt_boundary() {
        let db_path = temp_db_path("dispatch-runtime-recheck");
        let desired = selection("gpt-5");
        let changed = selection("gpt-5.2");
        let session_id = seed_session(&db_path, "dispatch-runtime-recheck", desired.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &session_id, desired.clone(), false);
        let ready = {
            let mut conn = open_database(&db_path).unwrap();
            let record = MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &request(&session_id, "dispatch-runtime-recheck", desired),
            )
            .unwrap();
            MessageSubmissionRepository::advance_status(
                &conn,
                &record.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            coordinator
                .required_submission(&record.submission_id)
                .unwrap()
        };
        runtime.states.lock().unwrap().insert(
            session_id,
            AgentSessionRuntimeSelectionState {
                desired: changed.clone(),
                effective: changed,
                status: SessionRuntimeSelectionStatus::Ready,
                session_revision: 1,
                selection_revision: 1,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            },
        );

        coordinator.drive_submission(&ready).await.unwrap();
        let record = coordinator
            .required_submission(&ready.submission_id)
            .unwrap();
        assert_eq!(record.status, MessageSubmissionStatus::AwaitingRuntime);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn failed_runtime_without_pending_switch_fails_submission() {
        let db_path = temp_db_path("runtime-failed");
        let initial = selection("gpt-5");
        let desired = selection("gpt-5.2");
        let session_id = seed_session(&db_path, "runtime-failed", initial.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        runtime.states.lock().unwrap().insert(
            session_id.clone(),
            AgentSessionRuntimeSelectionState {
                desired: desired.clone(),
                effective: initial,
                status: SessionRuntimeSelectionStatus::FailedUsingPrevious,
                session_revision: 0,
                selection_revision: 1,
                current_binding_id: None,
                activation_generation: 0,
                pending_switch_id: None,
                actionable_error: Some(
                    RuntimeSelectionActionableError::new(
                        "runtime_switch_configuration_unavailable",
                        "selected runtime could not be configured",
                        None,
                    )
                    .unwrap(),
                ),
            },
        );

        let error = coordinator
            .submit(request(&session_id, "runtime-failed", desired))
            .await
            .unwrap_err();
        assert_eq!(error.code, "runtime_switch_configuration_unavailable");
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn committed_switch_that_no_longer_matches_fails_without_waiting() {
        let db_path = temp_db_path("committed-runtime-diverged");
        let initial = selection("gpt-5");
        let desired = selection("gpt-5.1");
        let external = selection("gpt-5.2");
        let session_id = seed_session(&db_path, "committed-runtime-diverged", initial.clone());
        let (coordinator, runtime, dispatcher) = harness(&db_path, &session_id, initial, false);
        let (submission, _) = enqueue_with_committed_switch(
            &db_path,
            &session_id,
            "committed-runtime-diverged",
            &desired,
        );
        runtime.states.lock().unwrap().insert(
            session_id.clone(),
            AgentSessionRuntimeSelectionState {
                desired: external.clone(),
                effective: external,
                status: SessionRuntimeSelectionStatus::Ready,
                session_revision: 1,
                selection_revision: 2,
                current_binding_id: None,
                activation_generation: 1,
                pending_switch_id: None,
                actionable_error: None,
            },
        );

        tokio::time::timeout(
            Duration::from_secs(1),
            coordinator.drive_submission(&submission),
        )
        .await
        .expect("a committed but ineffective switch must not wait forever")
        .unwrap();
        let record = coordinator
            .required_submission(&submission.submission_id)
            .unwrap();
        assert_eq!(record.status, MessageSubmissionStatus::Failed);
        assert_eq!(
            record.error_code.as_deref(),
            Some("message_submission_runtime_changed_after_commit")
        );
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn committed_switch_rechecks_selection_after_stale_snapshot() {
        let db_path = temp_db_path("committed-runtime-converged");
        let initial = selection("gpt-5");
        let desired = selection("gpt-5.1");
        let session_id = seed_session(&db_path, "committed-runtime-converged", initial.clone());
        let (coordinator, runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        let (submission, switch_id) = enqueue_with_committed_switch(
            &db_path,
            &session_id,
            "committed-runtime-converged",
            &desired,
        );
        let ready = AgentSessionRuntimeSelectionState {
            desired: desired.clone(),
            effective: desired.clone(),
            status: SessionRuntimeSelectionStatus::Ready,
            session_revision: 1,
            selection_revision: 1,
            current_binding_id: None,
            activation_generation: 1,
            pending_switch_id: None,
            actionable_error: None,
        };
        runtime
            .states
            .lock()
            .unwrap()
            .insert(session_id.clone(), ready.clone());
        runtime.scripted_reads.lock().unwrap().insert(
            session_id.clone(),
            VecDeque::from([
                AgentSessionRuntimeSelectionState {
                    desired: desired.clone(),
                    effective: initial,
                    status: SessionRuntimeSelectionStatus::Preparing,
                    session_revision: 0,
                    selection_revision: 1,
                    current_binding_id: None,
                    activation_generation: 0,
                    pending_switch_id: Some(switch_id),
                    actionable_error: None,
                },
                ready,
            ]),
        );

        coordinator.drive_submission(&submission).await.unwrap();
        let ready_submission = coordinator
            .required_submission(&submission.submission_id)
            .unwrap();
        assert_eq!(
            ready_submission.status,
            MessageSubmissionStatus::ReadyToDispatch
        );
        assert!(ready_submission.error_code.is_none());
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);

        coordinator
            .drive_submission(&ready_submission)
            .await
            .unwrap();
        let completed = coordinator
            .required_submission(&submission.submission_id)
            .unwrap();
        assert_eq!(completed.status, MessageSubmissionStatus::Completed);
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 1);
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn ambiguous_dispatch_errors_never_persist_provider_text() {
        const SENSITIVE_SENTINEL: &str = "prompt-secret-SHOULD-NOT-PERSIST";

        let db_path = temp_db_path("ambiguous-redaction");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "ambiguous-redaction", initial.clone());
        let (coordinator, _runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        *dispatcher.failure_message.lock().unwrap() = Some(format!(
            "provider echoed sensitive input: {SENSITIVE_SENTINEL}"
        ));

        let error = coordinator
            .submit(request(
                &session_id,
                "ambiguous-provider-error",
                initial.clone(),
            ))
            .await
            .unwrap_err();
        assert_eq!(error.code, "message_submission_prompt_dispatch_ambiguous");
        let state = coordinator
            .get_submission(&GetMessageSubmissionRequest {
                session_id: session_id.clone(),
                message_idempotency_key: "ambiguous-provider-error".to_string(),
            })
            .unwrap();
        assert_eq!(
            state.error_detail_redacted.as_deref(),
            Some(AMBIGUOUS_PROMPT_ERROR_DETAIL)
        );
        assert!(!format!("{state:?}").contains(SENSITIVE_SENTINEL));

        let terminalized = {
            let mut conn = open_database(&db_path).unwrap();
            let record = MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &request(&session_id, "ambiguous-drive-error", initial),
            )
            .unwrap();
            MessageSubmissionRepository::advance_status(
                &conn,
                &record.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            MessageSubmissionRepository::mark_about_to_prompt(&conn, &record.submission_id)
                .unwrap();
            record
        };
        coordinator.terminalize_drive_error(
            &terminalized.submission_id,
            &VibexError::provider(
                "mock_terminalize_failed",
                format!("another provider error: {SENSITIVE_SENTINEL}"),
            ),
        );
        let record = coordinator
            .required_submission(&terminalized.submission_id)
            .unwrap();
        assert_eq!(
            record.status,
            MessageSubmissionStatus::AmbiguousPromptDispatch
        );
        assert_eq!(
            record.error_detail_redacted.as_deref(),
            Some(AMBIGUOUS_PROMPT_ERROR_DETAIL)
        );
        assert!(!format!("{:?}", record.state()).contains(SENSITIVE_SENTINEL));
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn persisted_agent_output_completes_submission_after_dispatch_error() {
        let db_path = temp_db_path("output-before-dispatch-error");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "output-before-dispatch-error", initial.clone());
        let (coordinator, _runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        dispatcher.fail_after_output.store(true, Ordering::SeqCst);
        *dispatcher.failure_message.lock().unwrap() =
            Some("turn cleanup failed after output".to_string());

        let items = coordinator
            .submit(request(
                &session_id,
                "output-before-dispatch-error",
                initial,
            ))
            .await
            .unwrap();

        assert_eq!(items.len(), 2);
        assert!(items.iter().any(|item| {
            matches!(
                &item.payload,
                TimelinePayload::AgentMessage(message)
                    if message.text == "completed" && message.is_final
            )
        }));
        let state = coordinator
            .get_submission(&GetMessageSubmissionRequest {
                session_id,
                message_idempotency_key: "output-before-dispatch-error".to_string(),
            })
            .unwrap();
        assert_eq!(state.status, MessageSubmissionStatus::Completed);
        assert!(state.error_code.is_none());
        cleanup_db(db_path);
    }

    #[test]
    fn submission_query_validates_idempotency_key() {
        let db_path = temp_db_path("query-key-validation");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "query-key-validation", initial.clone());
        let (coordinator, _runtime, _dispatcher) = harness(&db_path, &session_id, initial, false);

        let empty = coordinator
            .get_submission(&GetMessageSubmissionRequest {
                session_id: session_id.clone(),
                message_idempotency_key: "   ".to_string(),
            })
            .unwrap_err();
        assert_eq!(empty.code, "message_submission_idempotency_key_required");
        let oversized = coordinator
            .get_submission(&GetMessageSubmissionRequest {
                session_id,
                message_idempotency_key: "x".repeat(MAX_MESSAGE_IDEMPOTENCY_KEY_LEN + 1),
            })
            .unwrap_err();
        assert_eq!(oversized.code, "message_submission_idempotency_key_invalid");
        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn startup_marks_about_to_prompt_ambiguous_without_redispatch() {
        let db_path = temp_db_path("ambiguous");
        let initial = selection("gpt-5");
        let session_id = seed_session(&db_path, "ambiguous", initial.clone());
        let (coordinator, _runtime, dispatcher) =
            harness(&db_path, &session_id, initial.clone(), false);
        let submission_id = {
            let mut conn = open_database(&db_path).unwrap();
            let record = MessageSubmissionRepository::enqueue(
                &mut conn,
                MessageSubmissionId::new(),
                &request(&session_id, "ambiguous-key", initial),
            )
            .unwrap();
            MessageSubmissionRepository::advance_status(
                &conn,
                &record.submission_id,
                MessageSubmissionStatus::AwaitingRuntime,
                MessageSubmissionStatus::ReadyToDispatch,
            )
            .unwrap();
            MessageSubmissionRepository::mark_about_to_prompt(&conn, &record.submission_id)
                .unwrap();
            record.submission_id
        };

        let report = coordinator.reconcile_on_startup().unwrap();
        let conn = open_database(&db_path).unwrap();
        let record = MessageSubmissionRepository::get(&conn, &submission_id)
            .unwrap()
            .unwrap();
        assert_eq!(report.ambiguous, 1);
        assert_eq!(
            record.status,
            MessageSubmissionStatus::AmbiguousPromptDispatch
        );
        assert_eq!(dispatcher.calls.load(Ordering::SeqCst), 0);
        cleanup_db(db_path);
    }
}

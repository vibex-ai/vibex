//! The Vibex-use domain service.
//!
//! This is one service behind two front doors: the Agent-facing MCP sidecar
//! and the product's own user intents. Both go through the same identity,
//! scope, idempotency and orchestration rules, so an Agent cannot reach a
//! session state a user could not, and a user action cannot bypass the task
//! ledger the Agent reads.
//!
//! What the service is not: a second scheduler. Sessions, turns and message
//! delivery stay owned by [`AgentManager`] and the message-submission
//! coordinator; this module decides *what a caller may ask for*, records the
//! acceptance, and projects the resulting facts back out.

use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::Notify;
use vibex_agent::AgentManager;
use vibex_agent::manager::DelegationStartOutcome;
use vibex_core::{
    AgentDelegation, AgentDelegationId, AgentDelegationStatus, AgentId, AgentSession,
    AgentSessionState, CreateAgentDelegationRequest, CreateAgentSessionRequest, DelegationAccepted,
    DelegationBlockedOn, DelegationCompletionPolicy, DelegationContextRef, DelegationExecution,
    DelegationOwnershipKind, DelegationRuntimeSummary, DelegationTaskEventKind,
    DelegationTaskPhase, DelegationTaskView, DiscoverResponse, ErrorCategory, ExecutionOutcome,
    ExecutionUsageState, GroupPresentationCommand, MessageAccepted, MessageProvenance,
    MessageSubmissionId, PresentationActivationPolicy, PresentationOutcome, PresentationState,
    SendAgentMessageRequest, SessionGroupId, SessionGroupLayoutIntent, SessionGroupLayoutPreset,
    SessionGroupScope, SessionListPage, SessionReadAnchor, SessionReadCursor, SessionReadEntry,
    SessionReadPage, SessionReadView, SessionRuntimeOption, SessionRuntimeSelection,
    SessionTreeNode, TaskListPage, TimelineItem, TimelineItemKind, TimelinePage, TimelinePayload,
    UserMessageDelivery, VIBEX_USE_DEFAULT_READ_CHARS, VIBEX_USE_DEFAULT_READ_ITEMS,
    VIBEX_USE_IDEMPOTENCY_KEY_CHARS, VIBEX_USE_MAX_BATCH_REFS, VIBEX_USE_MAX_CONTEXT_REFS,
    VIBEX_USE_MAX_DEPTH, VIBEX_USE_MAX_GROUP_MEMBERS, VIBEX_USE_MAX_LIVE_PANES,
    VIBEX_USE_MAX_READ_CHARS, VIBEX_USE_MAX_READ_ITEMS, VIBEX_USE_MAX_WAIT_MS,
    VIBEX_USE_ROOT_EXECUTION_BUDGET, VIBEX_USE_SUMMARY_CHARS, VIBEX_USE_TITLE_CHARS, VibexError,
    VibexExecutionId, VibexOperationId, VibexResult, VibexSessionId, VibexUseActor,
    VibexUseCapabilitySnapshot, VibexUseDelivery, VibexUseGroupSummary, VibexUseOperation,
    VibexUseOperationResource, VibexUseOperationState, VibexUsePresentationCapability, VibexUseRef,
    VibexUseResourceKind, VibexUseRuntimeOption, VibexUseScope, VibexUseSessionSummary,
    VibexUseTool, VibexUseToolDefinition, VibexUseToolFuture, VibexUseToolHost,
    VibexUseUnavailableReason, VibexUseWorkspaceOption, WaitOutcome, WaitResponse, WorkspaceRecord,
    bounded_chars, unix_timestamp_ms, vibex_use_codes as use_codes,
};
use vibex_db::{
    AgentDelegationRepository, AgentSessionRuntimeRepository, DbConnection, ElicitationRepository,
    GroupPresentationRecord, GroupPresentationRepository, GroupPresentationReservation,
    GroupPresentationState, PermissionRepository, SessionControllerClaim,
    SessionControllerRepository, SessionGrantRepository, SessionOwnershipRepository,
    SessionRepository, TimelineRepository, VibexUseEventRepository, VibexUseExecutionRepository,
    VibexUseOperationRepository, VibexUseOperationReservation, WorkspaceRepository,
    apply_migrations, open_database, transition_delegation,
};
use vibex_remote::RemoteSidebarOrganizationSource;

use crate::catalog::RuntimeOptionCatalogService;
use crate::sidebar_organization::SessionGroupPresentationBridge;

/// Where a Vibex-use reference is resolved. Only the authority that minted a
/// reference may resolve it.
pub const LOCAL_AUTHORITY: &str = "local";

/// Backstop cadence for a bounded wait whose fact was written by another
/// process. The wait itself is notification-driven; this keeps a missed
/// in-process signal from turning into a silent timeout.
const WAIT_FALLBACK_POLL: std::time::Duration = std::time::Duration::from_millis(250);

pub struct VibexUseService {
    db_path: PathBuf,
    authority: String,
    manager: Arc<AgentManager>,
    runtime_catalog: Arc<RuntimeOptionCatalogService>,
    presentation: Arc<SessionGroupPresentationBridge>,
    /// Kept so a future typed sidebar capability can share the same single
    /// writer as the group bridge instead of opening a second path.
    #[allow(dead_code)]
    sidebar: Arc<dyn RemoteSidebarOrganizationSource>,
    activation_revision: u64,
    /// Wakes a bounded `vibex_wait` when any task fact changes.
    progress: Arc<Notify>,
}

impl VibexUseService {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        db_path: impl Into<PathBuf>,
        authority: impl Into<String>,
        manager: Arc<AgentManager>,
        runtime_catalog: Arc<RuntimeOptionCatalogService>,
        presentation: Arc<SessionGroupPresentationBridge>,
        sidebar: Arc<dyn RemoteSidebarOrganizationSource>,
        activation_revision: u64,
    ) -> Arc<Self> {
        Arc::new(Self {
            db_path: db_path.into(),
            authority: authority.into(),
            manager,
            runtime_catalog,
            presentation,
            sidebar,
            activation_revision: activation_revision.max(1),
            progress: Arc::new(Notify::new()),
        })
    }

    /// Offers groups whose display was never confirmed to the attached client.
    ///
    /// It runs when a shell connects, not on every frame, and it asks in
    /// `when_user_returns` mode: recovery must not pull somebody out of the
    /// workspace they are working in. The request is presentation-only, so it
    /// can never rewrite a definition the user has since rearranged.
    pub async fn recover_presentations(&self) -> VibexResult<usize> {
        if !self.presentation.is_attached() {
            return Ok(0);
        }
        let pending = {
            let conn = self.open()?;
            GroupPresentationRepository::list_unpresented(&conn, 50)?
        };
        let mut offered = 0;
        for record in pending {
            let mut command = record_group_command(&record, true);
            command.activation_policy = PresentationActivationPolicy::WhenUserReturns;
            command.presentation_only = true;
            let outcome = self.present_record(command).await;
            if matches!(
                outcome.state,
                PresentationState::Applied | PresentationState::Presented
            ) {
                let conn = self.open()?;
                let applied = outcome
                    .applied_layout
                    .as_ref()
                    .and_then(|layout| serde_json::to_value(layout).ok());
                let _ = GroupPresentationRepository::record_presentation(
                    &conn,
                    &record.group_id,
                    presentation_state_to_db(outcome.state),
                    applied.as_ref(),
                    outcome.reason.map(|reason| reason.as_str()),
                    Some(outcome.revision),
                );
                offered += 1;
            }
        }
        Ok(offered)
    }

    /// Settles executions whose prompt crossed the dispatch boundary and whose
    /// outcome can no longer be established.
    ///
    /// Only executions no watcher owns are touched: a task that is still active
    /// has a watcher that will settle it from real evidence, and marking those
    /// ambiguous would replace a fact with a guess.
    pub fn recover_ambiguous_executions(&self) -> VibexResult<usize> {
        let conn = self.open()?;
        let open = VibexUseExecutionRepository::list_open(&conn)?;
        let mut marked = 0;
        for execution in open {
            let submission =
                vibex_db::MessageSubmissionRepository::get(&conn, &execution.submission_id)?;
            let Some(submission) = submission else {
                continue;
            };
            if !matches!(
                submission.status,
                vibex_core::MessageSubmissionStatus::AboutToPrompt
                    | vibex_core::MessageSubmissionStatus::AmbiguousPromptDispatch
            ) {
                continue;
            }
            if let Some(task_ref) = execution.task_ref.as_ref()
                && let Some(task_id) = task_ref.task_id()
                && let Some(task) = AgentDelegationRepository::get(&conn, &task_id)?
                && task.phase().is_active()
            {
                continue;
            }
            let settled = vibex_db::mark_execution_ambiguous(
                &conn,
                &execution.id,
                use_codes::DISPATCH_AMBIGUOUS,
            )?;
            if let Some(settled) = settled {
                if let Some(task_ref) = settled.task_ref.as_ref()
                    && let Some(task_id) = task_ref.task_id()
                    && let Some(task) = AgentDelegationRepository::get(&conn, &task_id)?
                {
                    let _ = self.append_execution_event(
                        &conn,
                        &task,
                        DelegationTaskEventKind::ExecutionAmbiguous,
                        serde_json::json!({
                            "executionRef": settled.execution_ref.as_uri(),
                            "stopReason": "prompt_dispatch_ambiguous",
                        }),
                    );
                }
                marked += 1;
            }
        }
        Ok(marked)
    }

    /// Appends one task event with a deterministic id, so recovery re-announces
    /// a fact instead of duplicating it.
    fn append_execution_event(
        &self,
        conn: &DbConnection,
        task: &AgentDelegation,
        kind: DelegationTaskEventKind,
        payload: serde_json::Value,
    ) -> VibexResult<()> {
        let root = task
            .root_session_id
            .clone()
            .unwrap_or_else(|| task.parent_session_id.clone());
        let event_id = format!("event_{}_{}", kind.as_str(), task.id.as_str());
        VibexUseEventRepository::append(
            conn,
            &event_id,
            Some(&root),
            kind,
            Some(&task.id),
            task.child_session_id.as_ref(),
            task.revision,
            &payload,
        )?;
        Ok(())
    }

    /// The signal a bounded wait listens on.
    pub fn progress_signal(&self) -> Arc<Notify> {
        self.progress.clone()
    }

    pub fn authority(&self) -> &str {
        &self.authority
    }

    /// The live runtime option catalogue this service resolves against.
    pub fn runtime_option_catalog(&self) -> Arc<RuntimeOptionCatalogService> {
        self.runtime_catalog.clone()
    }

    pub fn activation_revision(&self) -> u64 {
        self.activation_revision
    }

    fn open(&self) -> VibexResult<DbConnection> {
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        Ok(conn)
    }

    fn notify_progress(&self) {
        self.progress.notify_waiters();
    }

    /// The durable send path, or a failure that says why it is unavailable.
    ///
    /// Failing closed matters here: an Agent must never be told a message was
    /// queued when this runtime has no durable coordinator to queue it into.
    fn require_coordinator(
        &self,
    ) -> VibexResult<Arc<vibex_agent::message_submission::MessageSubmissionCoordinator>> {
        self.manager
            .message_submission_coordinator()
            .ok_or_else(|| {
                VibexError::capability(
                    "message_submission_coordinator_unavailable",
                    "this runtime cannot accept durable messages",
                )
            })
    }

    // -----------------------------------------------------------------------
    // Scope
    // -----------------------------------------------------------------------

    fn actor_root(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
    ) -> VibexResult<VibexSessionId> {
        SessionOwnershipRepository::root_of(conn, &actor.session_id)
    }

    /// How the caller relates to one target session.
    ///
    /// A session the caller created — directly or through a task it started —
    /// is owned. A session it was handed is controlled. A session the user
    /// referenced is readable. Anything else stays invisible, and the caller is
    /// told "not found or not authorized" rather than learning that the session
    /// exists at all.
    fn resolve_scope(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        target: &VibexSessionId,
    ) -> VibexResult<VibexUseScope> {
        if target == &actor.session_id {
            return Ok(VibexUseScope::Owned);
        }
        if let Some(grant) = SessionGrantRepository::get(conn, &actor.session_id, target)? {
            return Ok(if grant.scope == "controlled" {
                VibexUseScope::Controlled
            } else {
                VibexUseScope::Referenced
            });
        }
        let mut current = target.clone();
        for _ in 0..32 {
            let Some(parent) = SessionOwnershipRepository::parent_of(conn, &current)? else {
                break;
            };
            if parent == actor.session_id {
                return Ok(VibexUseScope::Owned);
            }
            current = parent;
        }
        if SessionOwnershipRepository::root_of(conn, target)? == actor.session_id {
            return Ok(VibexUseScope::Owned);
        }
        // A live `controlled_existing` task is itself an explicit control grant.
        for task in vibex_db::list_delegations_for_child(conn, target)? {
            if task.parent_session_id == actor.session_id
                && task.ownership_kind == DelegationOwnershipKind::ControlledExisting
                && !task.phase().is_terminal()
            {
                return Ok(VibexUseScope::Controlled);
            }
        }
        Ok(VibexUseScope::ReadOnly)
    }

    fn require_readable(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        target: &VibexSessionId,
    ) -> VibexResult<VibexUseScope> {
        let scope = self.resolve_scope(conn, actor, target)?;
        if !scope.can_read_content() {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "that session is not visible to this caller",
            ));
        }
        Ok(scope)
    }

    fn require_writable(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        target: &VibexSessionId,
    ) -> VibexResult<VibexUseScope> {
        let scope = self.require_readable(conn, actor, target)?;
        if !scope.can_write() {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::SCOPE_DENIED,
                "this caller may read that session but not write to it",
            ));
        }
        Ok(scope)
    }

    // -----------------------------------------------------------------------
    // Capability
    // -----------------------------------------------------------------------

    pub fn capability(&self, actor: &VibexUseActor) -> VibexResult<VibexUseCapabilitySnapshot> {
        let conn = self.open()?;
        self.capability_on(&conn, actor)
    }

    fn capability_on(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
    ) -> VibexResult<VibexUseCapabilitySnapshot> {
        let parent = SessionRepository::get(conn, &actor.session_id)?.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "session not found",
            )
        })?;
        let root_session_id = self.actor_root(conn, actor)?;
        let depth = SessionOwnershipRepository::ancestor_depth(conn, &actor.session_id)?;
        let remaining_depth = VIBEX_USE_MAX_DEPTH.saturating_sub(depth);
        let active = vibex_db::count_active_executions_for_root(conn, &root_session_id)?;
        let remaining_executions = VIBEX_USE_ROOT_EXECUTION_BUDGET.saturating_sub(active);
        let current_task = self.current_task_for_session(conn, &actor.session_id)?;
        let presentation = self.presentation_capability();
        // A cancel that is still being confirmed fences the whole subtree. The
        // catalogue omits the writing tools instead of advertising work that the
        // fence would refuse a moment later.
        let fenced = vibex_db::delegation_cancellation_fence(conn, &actor.session_id)?.is_some();
        let mut capability = VibexUseCapabilitySnapshot {
            delivery: VibexUseDelivery::Mcp,
            delivery_reason: None,
            authority: self.authority.clone(),
            caller_session_ref: VibexUseRef::session(&actor.session_id),
            root_session_ref: VibexUseRef::session(&root_session_id),
            current_task_ref: current_task
                .as_ref()
                .map(|task| VibexUseRef::task(&task.id)),
            current_workspace_ref: VibexUseRef::workspace(parent.workspace_id.as_str()),
            catalog_revision: 0,
            activation_revision: actor.activation_revision.max(1),
            remaining_executions,
            max_depth: VIBEX_USE_MAX_DEPTH,
            remaining_depth,
            can_delegate: remaining_depth > 0 && remaining_executions > 0 && !fenced,
            delegation_blocked_by: fenced.then(|| use_codes::TASK_CANCELLING.to_string()),
            can_read_any_session: false,
            presentation,
            available_tools: Vec::new(),
        };
        capability.available_tools = vibex_core::vibex_use_tool_definitions(&capability)
            .into_iter()
            .map(|definition| definition.name)
            .collect();
        Ok(capability)
    }

    fn presentation_capability(&self) -> VibexUsePresentationCapability {
        let mut capability = self.presentation.capability();
        if !self.presentation.is_attached() {
            capability.available = false;
            capability.reason = Some(VibexUseUnavailableReason::NoShell);
            capability.supports_layout = false;
            capability.supports_focus = false;
        }
        capability.max_live_panes = capability.max_live_panes.min(VIBEX_USE_MAX_LIVE_PANES);
        capability.max_members = capability.max_members.min(VIBEX_USE_MAX_GROUP_MEMBERS);
        capability
    }

    fn current_task_for_session(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<AgentDelegation>> {
        Ok(vibex_db::list_delegations_for_child(conn, session_id)?
            .into_iter()
            .rfind(|task| !task.phase().is_terminal()))
    }

    fn pending_attention(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> Option<DelegationBlockedOn> {
        if let Ok(pending) = PermissionRepository::pending_for_session(conn, session_id)
            && let Some(request) = pending.first()
        {
            return Some(DelegationBlockedOn::Permission {
                request_id: request.id.as_str().to_string(),
            });
        }
        if let Ok(pending) = ElicitationRepository::pending_for_session(conn, session_id)
            && let Some(request) = pending.first()
        {
            return Some(DelegationBlockedOn::Question {
                request_id: request.id.as_str().to_string(),
            });
        }
        None
    }

    // -----------------------------------------------------------------------
    // Discovery
    // -----------------------------------------------------------------------

    async fn discover(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let mut capability = {
            let conn = self.open()?;
            self.capability_on(&conn, actor)?
        };
        let agent_filter = optional_string(arguments, "agentId", 256)
            .map(AgentId::parse)
            .transpose()?;
        let include_unavailable = optional_bool(arguments, "includeUnavailable", false);

        let catalog = self.runtime_catalog.list().await?;
        capability.catalog_revision = u64::try_from(catalog.revision).unwrap_or_default();
        // The catalogue is recomputed against the revision just observed, so a
        // caller that stores it knows exactly what it saw.
        capability.available_tools = vibex_core::vibex_use_tool_definitions(&capability)
            .into_iter()
            .map(|definition| definition.name)
            .collect();

        let options: Vec<VibexUseRuntimeOption> = catalog
            .options
            .iter()
            .filter(|option| {
                agent_filter
                    .as_ref()
                    .is_none_or(|agent_id| &option.selection.agent_id == agent_id)
            })
            .filter_map(|option| {
                let unavailable = runtime_option_unavailable(option);
                if unavailable.is_some() && !include_unavailable {
                    return None;
                }
                Some(runtime_option(option, unavailable))
            })
            .collect();
        // A catalogue can be long, so the option list pages like every other
        // list instead of growing without a bound the caller can plan around.
        let offset = cursor_offset(optional_string(arguments, "cursor", 64).as_deref());
        let max_items = bounded_usize(
            arguments,
            "maxItems",
            VIBEX_USE_DEFAULT_READ_ITEMS,
            VIBEX_USE_MAX_READ_ITEMS,
        );
        let has_more = options.len() > offset.saturating_add(max_items);
        let runtime_options: Vec<VibexUseRuntimeOption> =
            options.into_iter().skip(offset).take(max_items).collect();

        let (workspaces, groups) = {
            let conn = self.open()?;
            (
                self.workspace_options(&conn, &capability)?,
                self.group_summaries(&conn, actor)?,
            )
        };
        serde_json::to_value(DiscoverResponse {
            capability,
            runtime_options,
            workspaces,
            groups,
            guidance: vec![
                "Use one idempotency key per task. Reuse a key to retry the same request; never to start different work.".to_string(),
                "A settled phase is not the result: read it with vibex_get_tasks and vibex_read_session, then accept it with vibex_finish_task.".to_string(),
                "A presentation failure never fails the task. Keep the group reference and retry the display later.".to_string(),
                format!(
                    "vibex_wait is bounded to {VIBEX_USE_MAX_WAIT_MS} ms; timeoutMs 0 returns an immediate snapshot, and a timeout is not a task failure."
                ),
            ],
            next_cursor: has_more.then(|| format!("offset:{}", offset + max_items)),
            has_more,
        })
        .map_err(internal_encode_error)
    }

    fn workspace_options(
        &self,
        conn: &DbConnection,
        capability: &VibexUseCapabilitySnapshot,
    ) -> VibexResult<Vec<VibexUseWorkspaceOption>> {
        let caller_workspace = capability.current_workspace_ref.id.clone();
        Ok(WorkspaceRepository::list(conn)?
            .into_iter()
            .map(|(project, workspace)| VibexUseWorkspaceOption {
                reference: VibexUseRef::workspace(workspace.id.as_str()),
                project_id: project.id.as_str().to_string(),
                label: workspace.root_path.clone(),
                workspace_mode: workspace_mode_name(workspace.mode).to_string(),
                shareable_with_caller: workspace.id.as_str() == caller_workspace,
            })
            .collect())
    }

    // -----------------------------------------------------------------------
    // Session reads
    // -----------------------------------------------------------------------

    fn session_summary(
        &self,
        conn: &DbConnection,
        session: &AgentSession,
        scope: VibexUseScope,
    ) -> VibexResult<VibexUseSessionSummary> {
        let parent = SessionOwnershipRepository::parent_of(conn, &session.id)?;
        let created_by_delegation = parent.is_some();
        let children = SessionOwnershipRepository::child_count(conn, &session.id)?;
        let current_task = self.current_task_for_session(conn, &session.id)?;
        let attention = current_task
            .as_ref()
            .and_then(|task| task.blocked_on.clone())
            .or_else(|| self.pending_attention(conn, &session.id));
        let parent_session_ref = parent.map(|parent| VibexUseRef::session(&parent));
        Ok(VibexUseSessionSummary {
            session_ref: VibexUseRef::session(&session.id),
            title: session.title.clone(),
            state: agent_session_state_name(session.state).to_string(),
            scope,
            agent_id: Some(session.agent_id.clone()),
            workspace_ref: VibexUseRef::workspace(session.workspace_id.as_str()),
            project_id: session.project_id.as_str().to_string(),
            parent_session_ref,
            child_count: children,
            has_more_children: false,
            current_task_ref: current_task
                .as_ref()
                .map(|task| VibexUseRef::task(&task.id)),
            current_execution_ref: current_task
                .as_ref()
                .and_then(|task| task.current_execution_id.as_ref())
                .map(VibexUseRef::execution),
            blocked_on: attention,
            attention_count: usize::from(
                current_task
                    .as_ref()
                    .is_some_and(|task| task.blocked_on.is_some()),
            ),
            created_by_delegation,
            archived: session.archived_at_ms.is_some(),
            updated_at_ms: session.updated_at_ms,
        })
    }

    fn list_sessions(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let roots_only = optional_bool(arguments, "rootsOnly", false);
        let include_archived = optional_bool(arguments, "includeArchived", false);
        let max_items = bounded_usize(
            arguments,
            "maxItems",
            VIBEX_USE_DEFAULT_READ_ITEMS,
            VIBEX_USE_MAX_READ_ITEMS,
        );
        let offset = cursor_offset(optional_string(arguments, "cursor", 128).as_deref());
        let parent =
            parse_optional_ref(arguments, "parentSessionRef", VibexUseResourceKind::Session)?
                .map(|reference| session_reference(&reference))
                .transpose()?;

        let sessions: Vec<AgentSession> = match parent.as_ref() {
            Some(parent) => {
                self.require_readable(&conn, actor, parent)?;
                let mut sessions = Vec::new();
                for child_id in SessionOwnershipRepository::child_ids(&conn, parent)? {
                    if let Some(session) = SessionRepository::get(&conn, &child_id)? {
                        sessions.push(session);
                    }
                }
                sessions
            }
            None if roots_only => SessionRepository::list_root_sessions(&conn, include_archived)?,
            None => {
                // Even without `roots_only`, a caller receives only sessions it
                // may read: its own subtree plus anything explicitly shared.
                let mut visible = Vec::new();
                for session in SessionRepository::list(&conn, include_archived)? {
                    if self
                        .resolve_scope(&conn, actor, &session.id)?
                        .can_read_content()
                    {
                        visible.push(session);
                    }
                }
                visible
            }
        };

        let page: Vec<&AgentSession> = sessions.iter().skip(offset).take(max_items).collect();
        let mut summaries = Vec::new();
        for session in &page {
            let scope = self.resolve_scope(&conn, actor, &session.id)?;
            summaries.push(self.session_summary(&conn, session, scope)?);
        }
        let consumed = offset + page.len();
        let has_more = sessions.len() > consumed;
        serde_json::to_value(SessionListPage {
            sessions: summaries,
            next_cursor: has_more.then(|| format!("offset:{consumed}")),
            has_more,
        })
        .map_err(internal_encode_error)
    }

    fn get_session(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let session_ref = parse_ref(arguments, "sessionRef", VibexUseResourceKind::Session)?;
        let session_id = session_reference(&session_ref)?;
        let scope = self.require_readable(&conn, actor, &session_id)?;
        let session = SessionRepository::get(&conn, &session_id)?.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "session not found",
            )
        })?;
        let summary = self.session_summary(&conn, &session, scope)?;
        let mut value = serde_json::to_value(&summary).map_err(internal_encode_error)?;
        let tasks: VibexResult<Vec<DelegationTaskView>> =
            vibex_db::list_delegations_for_child(&conn, &session_id)?
                .iter()
                .map(|task| self.task_view(&conn, task))
                .collect();
        let runtime_state = AgentSessionRuntimeRepository::get_runtime_state(&conn, &session_id)?;
        if let Some(object) = value.as_object_mut() {
            object.insert(
                "tasks".to_string(),
                serde_json::to_value(tasks?).map_err(internal_encode_error)?,
            );
            object.insert(
                "desiredRuntime".to_string(),
                runtime_state
                    .as_ref()
                    .and_then(|state| state.desired_runtime_selection.clone())
                    .map(|selection| {
                        serde_json::to_value(delegation_runtime_summary(&selection))
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .unwrap_or(serde_json::Value::Null),
            );
            object.insert(
                "effectiveRuntime".to_string(),
                runtime_state
                    .and_then(|state| state.effective_runtime_selection)
                    .map(|selection| {
                        serde_json::to_value(delegation_runtime_summary(&selection))
                            .unwrap_or(serde_json::Value::Null)
                    })
                    .unwrap_or(serde_json::Value::Null),
            );
        }
        Ok(value)
    }

    fn read_session(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let session_ref = parse_ref(arguments, "sessionRef", VibexUseResourceKind::Session)?;
        let session_id = session_reference(&session_ref)?;
        self.require_readable(&conn, actor, &session_id)?;
        let cursor = parse_read_cursor(&conn, arguments, &session_ref)?;
        let page = self.session_read_page(&conn, &session_ref, cursor, arguments)?;
        serde_json::to_value(page).map_err(internal_encode_error)
    }

    /// Builds one bounded page of product-visible session content.
    ///
    /// Raw provider logs, credentials and private authentication paths never
    /// travel here. What is returned is what the product already shows, cut on
    /// character boundaries with the cut reported.
    fn session_read_page(
        &self,
        conn: &DbConnection,
        session_ref: &VibexUseRef,
        cursor: SessionReadCursor,
        arguments: &serde_json::Value,
    ) -> VibexResult<SessionReadPage> {
        let session_id = session_reference(session_ref)?;
        let view = cursor.view;
        let max_items = bounded_usize(
            arguments,
            "maxItems",
            VIBEX_USE_DEFAULT_READ_ITEMS,
            VIBEX_USE_MAX_READ_ITEMS,
        );
        let max_chars = bounded_usize(
            arguments,
            "maxChars",
            VIBEX_USE_DEFAULT_READ_CHARS,
            VIBEX_USE_MAX_READ_CHARS,
        );
        let limit = u32::try_from(max_items.saturating_mul(4).clamp(1, 400)).unwrap_or(200);
        let page: TimelinePage = match cursor.anchor {
            // `fetch_after(None, n)` is the newest window, not the first n items
            // of history. The distinction matters for a long session, which is
            // why the anchor is explicit instead of implied.
            SessionReadAnchor::Latest => {
                TimelineRepository::fetch_after(conn, &session_id, None, limit)?
            }
            SessionReadAnchor::After { sequence } => {
                TimelineRepository::fetch_after(conn, &session_id, Some(sequence), limit)?
            }
            SessionReadAnchor::Before { sequence } => {
                TimelineRepository::fetch_before(conn, &session_id, Some(sequence), limit)?
            }
        };
        let snapshot_end_sequence = page
            .items
            .last()
            .map(|item| item.sequence)
            .or(page.end_sequence)
            .or(page.start_sequence)
            .unwrap_or_default();

        let mut items = Vec::new();
        let mut budget = max_chars;
        let mut truncated = false;
        // Whether the window held entries this page did not return. It is what
        // makes the next cursor necessary, and it is not the same question as
        // whether any single entry's text had to be cut.
        let mut entries_dropped = false;
        let mut notices = Vec::new();
        for item in page.items.iter() {
            if !view_includes(view, item) {
                continue;
            }
            if items.len() >= max_items {
                truncated = true;
                entries_dropped = true;
                notices.push(format!(
                    "at most {max_items} entries are returned per page; continue with the next cursor"
                ));
                break;
            }
            let Some(text) = item_text(item) else {
                continue;
            };
            if text.trim().is_empty() {
                continue;
            }
            let (bounded, cut) = bounded_chars(&text, budget);
            budget = budget.saturating_sub(bounded.chars().count());
            truncated |= cut;
            items.push(SessionReadEntry {
                sequence: item.sequence,
                kind: timeline_item_kind_name(item.kind).to_string(),
                source: timeline_source_name(item.source).to_string(),
                text: bounded,
                truncated: cut,
                artifact_refs: Vec::new(),
                timestamp_ms: item.timestamp_ms,
            });
            if budget == 0 {
                truncated = true;
                entries_dropped = true;
                notices.push("the character budget for this page is used up".to_string());
                break;
            }
        }
        // The cursor continues from the last entry actually returned and in the
        // direction the caller was already walking. A `latest` or `before` page
        // is a step *back* through history, so its continuation points at older
        // sequences; only an `after` page walks forward. Pointing every page
        // forward made the older half of a long session unreachable while the
        // first page honestly reported that more existed.
        let forward = matches!(cursor.anchor, SessionReadAnchor::After { .. });
        let more_in_window = if forward {
            page.has_newer
        } else {
            page.has_older
        };
        let has_more = more_in_window || entries_dropped;
        let next_cursor = if has_more {
            let anchor = if forward {
                items.last().map(|entry| SessionReadAnchor::After {
                    sequence: entry.sequence,
                })
            } else {
                items.first().map(|entry| SessionReadAnchor::Before {
                    sequence: entry.sequence,
                })
            };
            anchor.map(|anchor| SessionReadCursor {
                session_ref: session_ref.clone(),
                view,
                anchor,
            })
        } else {
            None
        };
        let has_more = next_cursor.is_some();
        Ok(SessionReadPage {
            session_ref: session_ref.clone(),
            view,
            items,
            snapshot_end_sequence,
            next_cursor,
            has_more,
            truncated,
            notices,
        })
    }

    // -----------------------------------------------------------------------
    // Operations
    // -----------------------------------------------------------------------

    /// Reserves one write request under its caller key.
    ///
    /// A retry with the same key and the same payload resolves to the original
    /// operation. A different payload under the same key is refused rather than
    /// silently reusing somebody else's work.
    fn reserve_operation(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        tool: VibexUseTool,
        caller_key: &str,
        fingerprint: &str,
    ) -> VibexResult<(VibexUseOperation, bool)> {
        if caller_key.trim().is_empty()
            || caller_key.chars().count() > VIBEX_USE_IDEMPOTENCY_KEY_CHARS
        {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("idempotencyKey must be 1..={VIBEX_USE_IDEMPOTENCY_KEY_CHARS} characters"),
            ));
        }
        let operation = VibexUseOperation {
            id: VibexOperationId::new(),
            operation_ref: VibexUseRef::new(VibexUseResourceKind::Operation, ""),
            authority: actor.authority.clone(),
            actor_key: actor.session_id.as_str().to_string(),
            tool: tool.name().to_string(),
            caller_key: caller_key.to_string(),
            payload_fingerprint: fingerprint.to_string(),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: BTreeMap::new(),
            created_at_ms: unix_timestamp_ms(),
            updated_at_ms: unix_timestamp_ms(),
        };
        let operation = VibexUseOperation {
            operation_ref: VibexUseRef::operation(&operation.id),
            ..operation
        };
        match VibexUseOperationRepository::reserve(conn, &operation, caller_key)? {
            VibexUseOperationReservation::Claimed(operation) => Ok((operation, true)),
            VibexUseOperationReservation::Existing(operation) => Ok((operation, false)),
            VibexUseOperationReservation::Conflict(existing) => Err(VibexError::conflict(
                use_codes::IDEMPOTENCY_PAYLOAD_CONFLICT,
                "this idempotency key was already used for a different request",
            )
            .with_diagnostic("operationRef", existing.operation_ref.as_uri())),
        }
    }

    fn record_operation_resource(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
        kind: &str,
        reference: VibexUseRef,
    ) {
        let _ = VibexUseOperationRepository::append_resource(
            conn,
            &operation.id,
            &VibexUseOperationResource {
                kind: kind.to_string(),
                reference,
            },
        );
    }

    fn settle_operation(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
        state: VibexUseOperationState,
        error: Option<&VibexError>,
    ) {
        let _ = VibexUseOperationRepository::update_state(
            conn,
            &operation.id,
            state,
            error.map(|error| error.code.as_str()),
            error.map(|error| error.message.as_str()),
            error.is_some_and(|error| error.category == ErrorCategory::Conflict),
        );
    }

    /// Renders the durable resources of an accepted operation.
    ///
    /// A retry must never start a second team, so it answers with what the
    /// first request already produced.
    fn operation_view(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
    ) -> VibexResult<serde_json::Value> {
        let mut tasks = Vec::new();
        let mut sessions = Vec::new();
        let mut groups = Vec::new();
        let mut executions = Vec::new();
        let mut submissions = Vec::new();
        for resource in &operation.resources {
            match resource.reference.kind {
                VibexUseResourceKind::Task => {
                    if let Some(task_id) = resource.reference.task_id()
                        && let Some(task) = AgentDelegationRepository::get(conn, &task_id)?
                    {
                        tasks.push(self.task_view(conn, &task)?);
                    }
                }
                VibexUseResourceKind::Session => sessions.push(resource.reference.as_uri()),
                VibexUseResourceKind::Group => groups.push(resource.reference.as_uri()),
                VibexUseResourceKind::Execution => {
                    executions.push(resource.reference.as_uri());
                    // A retried send has to answer with the submission the first
                    // call enqueued: "return the original submission" is the
                    // promise that makes the retry safe, and an execution ref
                    // alone does not let the caller watch it.
                    if let Some(execution_id) = resource.reference.execution_id()
                        && let Some(execution) =
                            VibexUseExecutionRepository::get(conn, &execution_id)?
                    {
                        submissions.push(execution.submission_id);
                    }
                }
                _ => {}
            }
        }
        Ok(serde_json::json!({
            "operationRef": operation.operation_ref.as_uri(),
            "state": operation.state,
            "tool": operation.tool,
            "authority": operation.authority,
            "tasks": tasks,
            "sessionRefs": sessions,
            "groupRefs": groups,
            "executionRefs": executions,
            "submissionIds": submissions,
            "errorCode": operation.error_code,
            "errorMessage": operation.error_message,
            "retryable": operation.retryable,
            "replayed": true,
        }))
    }

    fn get_operation(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let operation =
            match parse_optional_ref(arguments, "operationRef", VibexUseResourceKind::Operation)? {
                Some(reference) => {
                    let operation_id = reference.operation_id().ok_or_else(|| {
                        VibexError::validation(
                            use_codes::REQUEST_INVALID,
                            "operationRef is invalid",
                        )
                    })?;
                    VibexUseOperationRepository::get(&conn, &operation_id)?
                }
                None => {
                    let tool_name = required_string(arguments, "tool", 64)?;
                    let tool = VibexUseTool::parse(&tool_name).ok_or_else(|| {
                        VibexError::validation(
                            use_codes::REQUEST_INVALID,
                            "tool is not a Vibex-use tool",
                        )
                    })?;
                    let caller_key = required_string(
                        arguments,
                        "idempotencyKey",
                        VIBEX_USE_IDEMPOTENCY_KEY_CHARS,
                    )?;
                    VibexUseOperationRepository::get_by_key(
                        &conn,
                        &actor.key(),
                        tool.name(),
                        &caller_key,
                    )?
                }
            };
        let operation = operation.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "operation not found",
            )
        })?;
        if operation.actor_key != actor.session_id.as_str()
            || operation.authority != actor.authority
        {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "operation not found",
            ));
        }
        self.operation_view(&conn, &operation)
    }

    // -----------------------------------------------------------------------
    // Task projection
    // -----------------------------------------------------------------------

    fn task_view(
        &self,
        conn: &DbConnection,
        task: &AgentDelegation,
    ) -> VibexResult<DelegationTaskView> {
        let root = task
            .root_session_id
            .clone()
            .unwrap_or_else(|| task.parent_session_id.clone());
        let _ = conn;
        Ok(DelegationTaskView {
            task_ref: VibexUseRef::task(&task.id),
            session_ref: task.child_session_id.as_ref().map(VibexUseRef::session),
            parent_session_ref: VibexUseRef::session(&task.parent_session_id),
            root_session_ref: VibexUseRef::session(&root),
            title: task.title.clone(),
            task_summary: task.task_summary.clone(),
            phase: task.phase(),
            legacy_status: legacy_status_name(task.status).to_string(),
            ownership_kind: task.ownership_kind,
            completion_policy: task.completion_policy,
            parent_task_ref: None,
            follows_task_ref: task.follows_task_id.as_ref().map(VibexUseRef::task),
            blocked_on: task.blocked_on.clone(),
            current_execution_ref: task
                .current_execution_id
                .as_ref()
                .map(VibexUseRef::execution),
            result_refs: task.result_refs.clone(),
            acceptance_criteria: task.acceptance_criteria.clone(),
            requested_runtime: task.requested_runtime.clone(),
            effective_runtime: task.effective_runtime.clone(),
            error_code: task.error_code.clone(),
            revision: task.revision,
            created_at_ms: task.created_at_ms,
            updated_at_ms: task.updated_at_ms,
            completed_at_ms: task.completed_at_ms,
            cancellation_requested_at_ms: task.cancellation_requested_at_ms,
        })
    }

    fn get_tasks(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let include_finished = optional_bool(arguments, "includeFinished", false);
        let mut tasks: Vec<AgentDelegation> = Vec::new();
        if let Some(list) = arguments
            .get("taskRefs")
            .and_then(serde_json::Value::as_array)
        {
            if list.len() > VIBEX_USE_MAX_BATCH_REFS {
                return Err(VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    format!("at most {VIBEX_USE_MAX_BATCH_REFS} task references are accepted"),
                ));
            }
            for entry in list {
                let raw = entry.as_str().ok_or_else(|| {
                    VibexError::validation(
                        use_codes::REQUEST_INVALID,
                        "taskRefs entries must be strings",
                    )
                })?;
                let reference = VibexUseRef::parse(raw).ok_or_else(|| {
                    VibexError::validation(use_codes::REQUEST_INVALID, "taskRefs entry is invalid")
                })?;
                let task_id = reference.task_id().ok_or_else(|| {
                    VibexError::validation(
                        use_codes::REQUEST_INVALID,
                        "taskRefs entry is not a task",
                    )
                })?;
                let Some(task) = AgentDelegationRepository::get(&conn, &task_id)? else {
                    continue;
                };
                self.require_readable(&conn, actor, &task.parent_session_id)?;
                tasks.push(task);
            }
        } else {
            let root = self.actor_root(&conn, actor)?;
            tasks = vibex_db::list_delegations_for_root(&conn, &root, include_finished)?;
        }
        // `includeTeam` widens an explicit list into the whole team instead of
        // replacing it, so a caller can ask about two tasks and still see what
        // else its own team is running.
        if optional_bool(arguments, "includeTeam", false) && arguments.get("taskRefs").is_some() {
            let root = self.actor_root(&conn, actor)?;
            for task in vibex_db::list_delegations_for_root(&conn, &root, include_finished)? {
                if !tasks.iter().any(|known| known.id == task.id) {
                    tasks.push(task);
                }
            }
        }
        let mut views = Vec::new();
        for task in tasks
            .iter()
            .filter(|task| include_finished || !task.phase().is_terminal())
        {
            views.push(self.task_view(&conn, task)?);
        }
        // A long-lived root can accumulate far more tasks than one page. The
        // order is stable, so an offset cursor continues exactly where the
        // previous page stopped.
        let offset = cursor_offset(optional_string(arguments, "cursor", 64).as_deref());
        let max_items = bounded_usize(
            arguments,
            "maxItems",
            VIBEX_USE_DEFAULT_READ_ITEMS,
            VIBEX_USE_MAX_READ_ITEMS,
        );
        let has_more = views.len() > offset.saturating_add(max_items);
        let tasks: Vec<DelegationTaskView> =
            views.into_iter().skip(offset).take(max_items).collect();
        serde_json::to_value(TaskListPage {
            tasks,
            next_cursor: has_more.then(|| format!("offset:{}", offset + max_items)),
            has_more,
        })
        .map_err(internal_encode_error)
    }

    // -----------------------------------------------------------------------
    // create_session
    // -----------------------------------------------------------------------

    async fn create_session(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        // `selectionRef` is read by `resolve_selection` straight off the
        // arguments; the workspace is the only other reference this tool
        // resolves itself.
        let workspace_ref = optional_string(arguments, "workspaceRef", 512);
        let title = optional_string(arguments, "title", VIBEX_USE_TITLE_CHARS);
        let first_message = optional_string(arguments, "firstMessage", 64 * 1024);
        let fingerprint = arguments_fingerprint(arguments);
        let (operation, claimed) = {
            let conn = self.open()?;
            self.reserve_operation(
                &conn,
                actor,
                VibexUseTool::CreateSession,
                &caller_key,
                &fingerprint,
            )?
        };
        if !claimed {
            let conn = self.open()?;
            return self.operation_view(&conn, &operation);
        }
        let workspace = {
            let conn = self.open()?;
            let capability = self.capability_on(&conn, actor)?;
            if !capability.can_delegate {
                let error = budget_error(&capability);
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
            self.resolve_workspace(&conn, workspace_ref.as_deref(), &capability)?
        };
        // A new session is a child of the caller in the permission sense too.
        // Handing it the product default instead of the parent's own safety
        // would let a strictly restricted Agent open a looser sibling and do
        // the work it was just denied.
        let parent_safety = {
            let conn = self.open()?;
            SessionRepository::get(&conn, &actor.session_id)?
                .ok_or_else(|| {
                    VibexError::new(
                        ErrorCategory::Permission,
                        use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                        "session not found",
                    )
                })?
                .safety
        };
        let selection = self.resolve_selection(actor, arguments).await?;
        // A session created with a first message takes the same declared
        // context a delegation would: creating a session and delegating work
        // are one workflow with one context rule, not two.
        let context_refs = {
            let conn = self.open()?;
            match self.resolve_context(&conn, actor, arguments) {
                Ok(context_refs) => context_refs,
                Err(error) => {
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
            }
        };
        let session_id = VibexSessionId::new();
        let session = self
            .manager
            .create_session_deferred_with_id(
                CreateAgentSessionRequest {
                    session_id: None,
                    defer_runtime_materialization: false,
                    runtime: selection.clone(),
                    workspace_root: workspace.root_path.clone(),
                    workspace_mode: workspace.mode,
                    title: Some(title.unwrap_or_else(|| "New Agent session".to_string())),
                    safety: Some(parent_safety),
                },
                session_id.clone(),
            )
            .await;
        let session = match session {
            Ok(session) => session,
            Err(error) => {
                let conn = self.open()?;
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        {
            // A session an Agent created belongs under the Agent's session in
            // the ownership tree. It is not a task, so no task row is written.
            let conn = self.open()?;
            SessionOwnershipRepository::upsert(&conn, &session.id, &actor.session_id, None)?;
            self.record_operation_resource(
                &conn,
                &operation,
                "session",
                VibexUseRef::session(&session.id),
            );
        }

        let mut execution_ref = None;
        if let Some(text) = first_message {
            let prompt_context = {
                let conn = self.open()?;
                self.render_context_block(&conn, &context_refs)?
            };
            let message_key = format!("vibex-use:{}:first", operation.id.as_str());
            let submission_id =
                match self
                    .require_coordinator()?
                    .prepare_submission(SendAgentMessageRequest {
                        session_id: session.id.clone(),
                        message_idempotency_key: message_key.clone(),
                        desired_runtime: selection.clone(),
                        text,
                        attachments: Vec::new(),
                        reasoning_effort: selection.reasoning_effort.clone(),
                        correlation_id: None,
                        delivery: UserMessageDelivery::Prompt,
                        prompt_context,
                        // The authority fills this in; the caller cannot.
                        provenance: vibex_core::MessageProvenance::DelegatedInput {
                            actor_session_ref: VibexUseRef::session(&actor.session_id),
                            task_ref: None,
                            operation_ref: operation.operation_ref.clone(),
                        },
                    }) {
                    Ok(submission_id) => submission_id,
                    Err(error) => {
                        let conn = self.open()?;
                        self.settle_operation(
                            &conn,
                            &operation,
                            VibexUseOperationState::Failed,
                            Some(&error),
                        );
                        return Err(error);
                    }
                };
            let conn = self.open()?;
            let execution = self.record_execution(
                &conn,
                None,
                &session.id,
                &submission_id,
                &message_key,
                MessageProvenance::DelegatedInput {
                    actor_session_ref: VibexUseRef::session(&actor.session_id),
                    task_ref: None,
                    operation_ref: operation.operation_ref.clone(),
                },
            )?;
            execution_ref = Some(execution.execution_ref);
        }
        {
            let conn = self.open()?;
            self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        }
        self.notify_progress();
        Ok(serde_json::json!({
            "status": "accepted",
            "sessionRef": VibexUseRef::session(&session.id).as_uri(),
            "operationRef": operation.operation_ref.as_uri(),
            "executionRef": execution_ref.map(|reference| reference.as_uri()),
            "effectiveTarget": serde_json::to_value(delegation_runtime_summary(&selection))
                .map_err(internal_encode_error)?,
        }))
    }

    // -----------------------------------------------------------------------
    // delegate
    // -----------------------------------------------------------------------

    async fn delegate(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let task_object = arguments.get("task").cloned().unwrap_or_default();
        let prompt = required_string(&task_object, "prompt", 64 * 1024)?;
        let title = optional_string(&task_object, "title", VIBEX_USE_TITLE_CHARS)
            .unwrap_or_else(|| "Delegated task".to_string());
        let criteria = string_array(&task_object, "acceptanceCriteria", 16, 512);
        let completion_policy =
            match optional_string(&task_object, "completionPolicy", 32).as_deref() {
                Some("single_turn_legacy") => DelegationCompletionPolicy::SingleTurnLegacy,
                _ => DelegationCompletionPolicy::OwnerReview,
            };
        // Target, session kind, task policy, context window and acceptance
        // criteria all change what the request does; the whole payload is
        // fingerprinted so none of them can silently reuse an earlier task.
        let fingerprint = arguments_fingerprint(arguments);

        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::Delegate,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let capability = self.capability_on(&conn, actor)?;
        if !capability.can_delegate {
            let error = budget_error(&capability);
            self.settle_operation(
                &conn,
                &operation,
                VibexUseOperationState::Failed,
                Some(&error),
            );
            return Err(error);
        }
        let selection = match self.resolve_selection(actor, arguments).await {
            Ok(selection) => selection,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let context_refs = match self.resolve_context(&conn, actor, arguments) {
            Ok(context_refs) => context_refs,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let follows_task_id =
            parse_optional_ref(arguments, "followsTaskRef", VibexUseResourceKind::Task)?
                .map(|reference| {
                    reference.task_id().ok_or_else(|| {
                        VibexError::validation(
                            use_codes::REQUEST_INVALID,
                            "followsTaskRef is invalid",
                        )
                    })
                })
                .transpose()?;

        let session_spec = arguments.get("session").cloned().unwrap_or_default();
        let kind = optional_string(&session_spec, "kind", 32).unwrap_or_else(|| "new".to_string());
        let existing = match kind.as_str() {
            "existing" | "controlled_existing" | "controlledExisting" => {
                let reference =
                    parse_ref(&session_spec, "sessionRef", VibexUseResourceKind::Session)?;
                let session_id = session_reference(&reference)?;
                let scope = match self.require_writable(&conn, actor, &session_id) {
                    Ok(scope) => scope,
                    Err(error) => {
                        self.settle_operation(
                            &conn,
                            &operation,
                            VibexUseOperationState::Failed,
                            Some(&error),
                        );
                        return Err(error);
                    }
                };
                if scope == VibexUseScope::Owned {
                    let error = VibexError::validation(
                        use_codes::REQUEST_INVALID,
                        "that session already belongs to the caller; delegate a new session instead",
                    );
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
                Some(session_id)
            }
            "new" => None,
            other => {
                let error = VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    format!("session.kind must be new or existing, not {other}"),
                );
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };

        let started = if let Some(session_id) = existing {
            match self.delegate_into_existing_session(
                &conn,
                actor,
                &operation,
                &session_id,
                &prompt,
                &title,
                &selection,
                completion_policy,
                criteria,
                context_refs.clone(),
                follows_task_id,
            ) {
                Ok(started) => started,
                Err(error) => {
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
            }
        } else {
            let request = CreateAgentDelegationRequest {
                parent_session_id: actor.session_id.clone(),
                idempotency_key: format!("{}:{}", operation.id.as_str(), caller_key),
                task: prompt,
                title: Some(title),
                agent_id: Some(selection.agent_id.clone()),
                provider_profile_id: selection.provider_profile_id().cloned(),
                model: selection.model_id().map(ToString::to_string),
                reasoning_effort: selection.reasoning_effort.clone(),
                mode_id: selection.mode_id.clone(),
                completion_policy,
                ownership_kind: DelegationOwnershipKind::OwnedChild,
                context_refs,
                acceptance_criteria: criteria,
                follows_task_id,
                existing_session_id: None,
            };
            match self.manager.create_task_delegation(request).await {
                Ok(started) => started,
                Err(error) => {
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
            }
        };

        self.record_operation_resource(
            &conn,
            &operation,
            "task",
            VibexUseRef::task(&started.delegation.id),
        );
        self.record_operation_resource(
            &conn,
            &operation,
            "session",
            VibexUseRef::session(&started.child_session_id),
        );
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        let _ = self.record_accepted_event(&conn, &started.delegation);
        let presentation = self.apply_presentation(actor, &operation, arguments).await;
        self.notify_progress();
        Ok(self.acceptance_value(&conn, &operation, &started.delegation, presentation))
    }

    /// Registers a task over a session that already exists.
    ///
    /// The session is not adopted: the task records that it was granted
    /// control, so it never enters the creation tree and never cascades a
    /// delete over somebody else's session.
    #[allow(clippy::too_many_arguments)]
    fn delegate_into_existing_session(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        operation: &VibexUseOperation,
        session_id: &VibexSessionId,
        prompt: &str,
        title: &str,
        selection: &SessionRuntimeSelection,
        completion_policy: DelegationCompletionPolicy,
        criteria: Vec<String>,
        context_refs: Vec<DelegationContextRef>,
        follows_task_id: Option<AgentDelegationId>,
    ) -> VibexResult<DelegationStartOutcome> {
        // The task belongs to the team that registered it, not to the team the
        // target session happens to live in. Filing it under the target's root
        // would hide it from the caller's own `get_tasks`, `get_events` and
        // `wait`, and would spend somebody else's execution budget.
        let root = self.actor_root(conn, actor)?;
        let now = unix_timestamp_ms();
        let mut task = AgentDelegation::single_turn_legacy(
            actor.session_id.clone(),
            format!("{}:{}", operation.id.as_str(), operation.caller_key),
            title,
            bounded_chars(prompt, VIBEX_USE_SUMMARY_CHARS).0,
            Some(selection.agent_id.clone()),
            AgentDelegationStatus::Running,
            now,
        );
        task.child_session_id = Some(session_id.clone());
        task.completion_policy = completion_policy;
        task.phase = DelegationTaskPhase::Active;
        task.ownership_kind = DelegationOwnershipKind::ControlledExisting;
        task.root_session_id = Some(root);
        task.follows_task_id = follows_task_id;
        task.context_refs = context_refs.clone();
        task.acceptance_criteria = criteria;
        task.requested_runtime = Some(delegation_runtime_summary(selection));
        task.payload_fingerprint = Some(operation.payload_fingerprint.clone());
        let mut writable = self.open()?;
        let task = match AgentDelegationRepository::reserve_or_get(&mut writable, &task, u32::MAX)?
        {
            vibex_db::AgentDelegationReservation::Claimed(task)
            | vibex_db::AgentDelegationReservation::Existing(task) => task,
        };
        let prompt_context = self.render_context_block(&self.open()?, &context_refs)?;
        let message_key = format!("vibex-use:{}:first", operation.id.as_str());
        let submission_id =
            self.require_coordinator()?
                .prepare_submission(SendAgentMessageRequest {
                    session_id: session_id.clone(),
                    message_idempotency_key: message_key.clone(),
                    desired_runtime: selection.clone(),
                    text: prompt.to_string(),
                    attachments: Vec::new(),
                    reasoning_effort: selection.reasoning_effort.clone(),
                    correlation_id: None,
                    delivery: UserMessageDelivery::Prompt,
                    prompt_context,
                    provenance: vibex_core::MessageProvenance::DelegatedInput {
                        actor_session_ref: VibexUseRef::session(&actor.session_id),
                        task_ref: Some(VibexUseRef::task(&task.id)),
                        operation_ref: operation.operation_ref.clone(),
                    },
                })?;
        let execution = self.record_execution(
            conn,
            Some(&task),
            session_id,
            &submission_id,
            &message_key,
            MessageProvenance::DelegatedInput {
                actor_session_ref: VibexUseRef::session(&actor.session_id),
                task_ref: Some(VibexUseRef::task(&task.id)),
                operation_ref: operation.operation_ref.clone(),
            },
        )?;
        let _ = SessionControllerRepository::claim(
            conn,
            session_id,
            &task.id,
            &actor.session_id,
            None,
        )?;
        Ok(DelegationStartOutcome {
            delegation: task,
            child_session_id: session_id.clone(),
            submission_id,
            execution_id: execution.id,
        })
    }

    /// Enqueues one execution record for a submission that is already durable.
    fn record_execution(
        &self,
        conn: &DbConnection,
        task: Option<&AgentDelegation>,
        session_id: &VibexSessionId,
        submission_id: &MessageSubmissionId,
        input_idempotency_key: &str,
        provenance: MessageProvenance,
    ) -> VibexResult<DelegationExecution> {
        let now = unix_timestamp_ms();
        // The runtime snapshot a message is admitted under is recorded with the
        // execution, so an execution can be explained later even after the
        // session has been switched to another configuration.
        let runtime_selection_revision = self
            .runtime_catalog_revision(conn, session_id)
            .unwrap_or_default();
        let execution = DelegationExecution {
            id: VibexExecutionId::new(),
            execution_ref: VibexUseRef::new(VibexUseResourceKind::Execution, ""),
            task_ref: task.map(|task| VibexUseRef::task(&task.id)),
            session_ref: VibexUseRef::session(session_id),
            submission_id: submission_id.clone(),
            input_idempotency_key: input_idempotency_key.to_string(),
            provenance,
            start_sequence: None,
            end_sequence: None,
            runtime_selection_revision,
            outcome: ExecutionOutcome::Queued,
            error_code: None,
            stop_reason: None,
            summary: None,
            result_ranges: Vec::new(),
            artifact_refs: Vec::new(),
            usage: ExecutionUsageState::Unknown,
            truncated: false,
            blocked_on: None,
            created_at_ms: now,
            updated_at_ms: now,
            finished_at_ms: None,
        };
        let execution = DelegationExecution {
            execution_ref: VibexUseRef::execution(&execution.id),
            ..execution
        };
        let (stored, _) = VibexUseExecutionRepository::insert_or_get(conn, &execution)?;
        if let Some(task) = task {
            let mut task = task.clone();
            task.current_execution_id = Some(stored.id.clone());
            vibex_db::update_delegation_vibex_use_fields(conn, &task)?;
        }
        Ok(stored)
    }

    fn record_accepted_event(
        &self,
        conn: &DbConnection,
        task: &AgentDelegation,
    ) -> VibexResult<()> {
        let root = task
            .root_session_id
            .clone()
            .unwrap_or_else(|| task.parent_session_id.clone());
        VibexUseEventRepository::append(
            conn,
            &format!("task_accepted_{}", vibex_core::EventId::new().as_str()),
            Some(&root),
            DelegationTaskEventKind::TaskAccepted,
            Some(&task.id),
            task.child_session_id.as_ref(),
            task.revision,
            &serde_json::json!({
                "taskRef": VibexUseRef::task(&task.id).as_uri(),
                "sessionRef": task
                    .child_session_id
                    .as_ref()
                    .map(VibexUseRef::session)
                    .map(|reference| reference.as_uri()),
            }),
        )?;
        Ok(())
    }

    fn acceptance_value(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
        task: &AgentDelegation,
        presentation: Option<PresentationOutcome>,
    ) -> serde_json::Value {
        let mut warnings = Vec::new();
        if let Some(outcome) = presentation.as_ref()
            && !matches!(
                outcome.state,
                PresentationState::Applied | PresentationState::Presented
            )
        {
            warnings
                .push("the task is accepted and running; the group is not visible yet".to_string());
        }
        let execution_ref = task
            .current_execution_id
            .as_ref()
            .map(VibexUseRef::execution)
            .or_else(|| {
                task.child_session_id.as_ref().and_then(|child| {
                    VibexUseExecutionRepository::get_by_input(
                        conn,
                        Some(child),
                        &format!("delegation:{}", task.id.as_str()),
                    )
                    .ok()
                    .flatten()
                    .map(|execution| execution.execution_ref)
                })
            });
        serde_json::to_value(DelegationAccepted {
            status: DelegationAccepted::ACCEPTED,
            task_ref: Some(VibexUseRef::task(&task.id)),
            session_ref: task.child_session_id.as_ref().map(VibexUseRef::session),
            operation_ref: operation.operation_ref.clone(),
            execution_ref,
            phase: task.phase(),
            effective_target: task.effective_runtime.clone(),
            presentation,
            warnings,
            context: task.context_refs.clone(),
        })
        .unwrap_or(serde_json::Value::Null)
    }

    // -----------------------------------------------------------------------
    // Presentation
    // -----------------------------------------------------------------------

    /// Applies the optional `presentation` block of one delegate request.
    ///
    /// A presentation failure never fails the task. The group reference is kept
    /// so a caller can retry the display without starting a second team.
    async fn apply_presentation(
        &self,
        actor: &VibexUseActor,
        operation: &VibexUseOperation,
        arguments: &serde_json::Value,
    ) -> Option<PresentationOutcome> {
        let command = self.presentation_command(actor, operation, arguments)?;
        Some(self.present_record(command).await)
    }

    /// Builds the display request for one delegate call, if it asked for one.
    ///
    /// The database work finishes before anything is awaited, so the caller's
    /// future never holds a connection across the bridge round-trip.
    fn presentation_command(
        &self,
        actor: &VibexUseActor,
        operation: &VibexUseOperation,
        arguments: &serde_json::Value,
    ) -> Option<GroupPresentationCommand> {
        let conn = self.open().ok()?;
        let presentation = arguments.get("presentation")?;
        let group_ref = optional_string(presentation, "groupRef", 256);
        let group_name = optional_string(presentation, "groupName", VIBEX_USE_TITLE_CHARS);
        let present = optional_bool(presentation, "present", false);
        if group_ref.is_none() && group_name.is_none() {
            return None;
        }
        let mut session_ids = vec![actor.session_id.clone()];
        if let Some(task_id) = operation
            .resources
            .iter()
            .find(|resource| resource.kind == "task")
            .and_then(|resource| resource.reference.task_id())
            && let Ok(Some(task)) = AgentDelegationRepository::get(&conn, &task_id)
            && let Some(child) = task.child_session_id
        {
            session_ids.push(child);
        }
        match group_ref {
            Some(reference) => {
                let parsed = VibexUseRef::parse(&reference)?;
                let group_id = parsed.group_id()?;
                let record = GroupPresentationRepository::get(&conn, group_id.as_str())
                    .ok()
                    .flatten()?;
                let mut members = record.member_session_ids.clone();
                for session_id in session_ids {
                    if !members.contains(&session_id) {
                        members.push(session_id);
                    }
                }
                let updated = GroupPresentationRepository::update(
                    &conn,
                    group_id.as_str(),
                    None,
                    None,
                    Some(&members),
                    None,
                )
                .ok()
                .flatten()?;
                Some(record_group_command(&updated, present))
            }
            None => {
                let name = group_name?;
                let workspace = self
                    .capability_on(&conn, actor)
                    .ok()
                    .map(|capability| capability.current_workspace_ref)?;
                let layout = SessionGroupLayoutIntent {
                    preset: SessionGroupLayoutPreset::LeadAndWorkers,
                    lead_session_ref: Some(VibexUseRef::session(&actor.session_id)),
                    preferred_live_panes: Some(session_ids.len().min(VIBEX_USE_MAX_LIVE_PANES)),
                };
                let scope = SessionGroupScope::Workspace {
                    workspace_ref: workspace,
                };
                let fingerprint = fingerprint_of(&[
                    operation.id.as_str(),
                    &name,
                    &session_ids
                        .iter()
                        .map(|id| id.as_str().to_string())
                        .collect::<Vec<String>>()
                        .join(","),
                ]);
                let group_id = SessionGroupId::new();
                let record = GroupPresentationRepository::reserve_or_get(
                    &conn,
                    Some(&operation.id),
                    &operation.actor_key,
                    group_id.as_str(),
                    &scope,
                    &name,
                    &session_ids,
                    &layout,
                    &fingerprint,
                )
                .ok()?;
                let record = match record {
                    GroupPresentationReservation::Claimed(record)
                    | GroupPresentationReservation::Existing(record) => record,
                    GroupPresentationReservation::Conflict(existing) => existing,
                };
                Some(record_group_command(&record, present))
            }
        }
    }

    async fn present_record(&self, command: GroupPresentationCommand) -> PresentationOutcome {
        let group_ref = VibexUseRef::group(command.group_id.as_str());
        if !self.presentation.is_attached() {
            return PresentationOutcome {
                group_ref,
                state: PresentationState::Prepared,
                reason: Some(VibexUseUnavailableReason::NoShell),
                message: Some(
                    "no display client is connected; the group is stored and can be shown later"
                        .to_string(),
                ),
                applied_layout: None,
                revision: 0,
            };
        }
        match self.presentation.apply(command).await {
            Ok(reply) => PresentationOutcome {
                group_ref,
                state: reply.state,
                reason: reply.reason,
                message: reply.message,
                applied_layout: reply.applied_layout,
                revision: reply.revision,
            },
            Err(error) => PresentationOutcome {
                group_ref,
                state: PresentationState::Deferred,
                reason: Some(VibexUseUnavailableReason::NoShell),
                message: Some(error.message),
                applied_layout: None,
                revision: 0,
            },
        }
    }

    fn group_summaries(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
    ) -> VibexResult<Vec<VibexUseGroupSummary>> {
        let records =
            GroupPresentationRepository::list_for_actor(conn, actor.session_id.as_str(), 100)?;
        let capability = self.presentation_capability();
        Ok(records
            .into_iter()
            .map(|record| group_summary(&record, &capability))
            .collect())
    }

    fn list_groups(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let capability = self.presentation_capability();
        let groups = match parse_optional_ref(arguments, "groupRef", VibexUseResourceKind::Group)? {
            Some(reference) => {
                let group_id = reference.group_id().ok_or_else(|| {
                    VibexError::validation(use_codes::REQUEST_INVALID, "groupRef is invalid")
                })?;
                let record = GroupPresentationRepository::get(&conn, group_id.as_str())?
                    .ok_or_else(|| {
                        VibexError::new(
                            ErrorCategory::Permission,
                            use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                            "group not found",
                        )
                    })?;
                if record.actor_key != actor.session_id.as_str() {
                    return Err(VibexError::new(
                        ErrorCategory::Permission,
                        use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                        "group not found",
                    ));
                }
                vec![group_summary(&record, &capability)]
            }
            None => self.group_summaries(&conn, actor)?,
        };
        // Members are the expensive part of a long group list, so a client that
        // only wants the overview can leave them out.
        let groups = if optional_bool(arguments, "includeMembers", true) {
            groups
        } else {
            groups
                .into_iter()
                .map(|mut group| {
                    group.member_session_refs.clear();
                    group
                })
                .collect()
        };
        Ok(serde_json::json!({
            "capability": capability,
            "groups": groups,
        }))
    }

    async fn create_group(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let name = required_string(arguments, "name", VIBEX_USE_TITLE_CHARS)?;
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::CreateGroup,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let members = match self.resolve_group_members(&conn, actor, arguments) {
            Ok(members) => members,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let scope = match self.resolve_group_scope(&conn, actor, arguments) {
            Ok(scope) => scope,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let layout = parse_layout_intent(arguments)?;
        let group_id = SessionGroupId::new();
        let reservation = GroupPresentationRepository::reserve_or_get(
            &conn,
            Some(&operation.id),
            &operation.actor_key,
            group_id.as_str(),
            &scope,
            &name,
            &members,
            &layout,
            &fingerprint,
        )?;
        let record = match reservation {
            GroupPresentationReservation::Claimed(record) => record,
            GroupPresentationReservation::Existing(record) => record,
            GroupPresentationReservation::Conflict(existing) => {
                return Err(VibexError::conflict(
                    use_codes::IDEMPOTENCY_PAYLOAD_CONFLICT,
                    "this idempotency key was already used for a different group",
                )
                .with_diagnostic("groupRef", VibexUseRef::group(&existing.group_id).as_uri()));
            }
        };
        self.record_operation_resource(
            &conn,
            &operation,
            "group",
            VibexUseRef::group(&record.group_id),
        );
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);

        // Without a display client the definition is still stored: a headless
        // runtime must not claim it drew anything, and it must not lose the
        // team either.
        let outcome = if self.presentation.is_attached() {
            self.present_record(record_group_command(&record, false))
                .await
        } else {
            PresentationOutcome {
                group_ref: VibexUseRef::group(&record.group_id),
                state: PresentationState::Prepared,
                reason: Some(VibexUseUnavailableReason::NoShell),
                message: Some(
                    "the group is stored; connect a display client to show it".to_string(),
                ),
                applied_layout: None,
                revision: record.revision,
            }
        };
        let record = GroupPresentationRepository::record_presentation(
            &conn,
            &record.group_id,
            presentation_state_to_db(outcome.state),
            None,
            outcome.reason.map(|reason| reason.as_str()),
            (outcome.state == PresentationState::Applied
                || outcome.state == PresentationState::Presented)
                .then_some(outcome.revision),
        )?
        .unwrap_or(record);
        Ok(serde_json::json!({
            "groupRef": VibexUseRef::group(&record.group_id).as_uri(),
            "state": outcome.state,
            "reason": outcome.reason,
            "message": outcome.message,
            "members": record
                .member_session_ids
                .iter()
                .map(VibexUseRef::session)
                .map(|reference| reference.as_uri())
                .collect::<Vec<String>>(),
            "layout": record.layout,
            "revision": record.revision,
            "operationRef": operation.operation_ref.as_uri(),
        }))
    }

    async fn update_group(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let group_ref = parse_ref(arguments, "groupRef", VibexUseResourceKind::Group)?;
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::UpdateGroup,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let group_id = group_ref.group_id().ok_or_else(|| {
            VibexError::validation(use_codes::REQUEST_INVALID, "groupRef is invalid")
        })?;
        let record =
            GroupPresentationRepository::get(&conn, group_id.as_str())?.ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Permission,
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "group not found",
                )
            })?;
        if record.actor_key != actor.session_id.as_str() || !record.created_by_caller {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::SCOPE_DENIED,
                "this group belongs to the user and cannot be rewritten by an Agent",
            ));
        }
        let expected_revision = optional_u64(arguments, "expectedRevision");
        let name = optional_string(arguments, "name", VIBEX_USE_TITLE_CHARS);
        let members = if arguments.get("memberSessionRefs").is_some() {
            Some(self.resolve_group_members(&conn, actor, arguments)?)
        } else {
            None
        };
        let layout = if arguments.get("layout").is_some() {
            Some(parse_layout_intent(arguments)?)
        } else {
            None
        };
        let updated = GroupPresentationRepository::update(
            &conn,
            group_id.as_str(),
            expected_revision,
            name.as_deref(),
            members.as_deref(),
            layout.as_ref(),
        )?
        .ok_or_else(|| {
            VibexError::conflict(
                use_codes::PRESENTATION_LAYOUT_CONFLICT,
                "the group changed since it was read; read it again before updating",
            )
        })?;
        if optional_bool(arguments, "releaseOwnership", false) {
            GroupPresentationRepository::release_to_user(&conn, group_id.as_str())?;
        }
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        let state = if self.presentation.is_attached() {
            let outcome = self
                .present_record(record_group_command(&updated, false))
                .await;
            outcome.state
        } else {
            PresentationState::Prepared
        };
        Ok(serde_json::json!({
            "groupRef": VibexUseRef::group(&updated.group_id).as_uri(),
            "state": state,
            "revision": updated.revision,
            "members": updated
                .member_session_ids
                .iter()
                .map(VibexUseRef::session)
                .map(|reference| reference.as_uri())
                .collect::<Vec<String>>(),
        }))
    }

    async fn present_group(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let group_ref = parse_ref(arguments, "groupRef", VibexUseResourceKind::Group)?;
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::PresentGroup,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let group_id = group_ref.group_id().ok_or_else(|| {
            VibexError::validation(use_codes::REQUEST_INVALID, "groupRef is invalid")
        })?;
        let record =
            GroupPresentationRepository::get(&conn, group_id.as_str())?.ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Permission,
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "group not found",
                )
            })?;
        if record.actor_key != actor.session_id.as_str() {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "group not found",
            ));
        }
        if !record.created_by_caller {
            // The group was handed back to the user. Showing it is still a
            // write to the live layout, so it needs the same ownership the other
            // mutations do.
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::SCOPE_DENIED,
                "this group belongs to the user; it can be read but not rearranged by an Agent",
            ));
        }
        let activation_policy = match optional_string(arguments, "activationPolicy", 32).as_deref()
        {
            Some("when_user_returns") => PresentationActivationPolicy::WhenUserReturns,
            _ => PresentationActivationPolicy::IfCurrentTeam,
        };
        let command = present_only_group_command(
            &record,
            activation_policy,
            parse_optional_ref(arguments, "focusSessionRef", VibexUseResourceKind::Session)?,
        );
        let outcome = self.present_record(command).await;
        let applied = outcome
            .applied_layout
            .as_ref()
            .and_then(|layout| serde_json::to_value(layout).ok());
        GroupPresentationRepository::record_presentation(
            &conn,
            &record.group_id,
            presentation_state_to_db(outcome.state),
            applied.as_ref(),
            outcome.reason.map(|reason| reason.as_str()),
            (outcome.state == PresentationState::Applied
                || outcome.state == PresentationState::Presented)
                .then_some(outcome.revision),
        )?;
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        serde_json::to_value(outcome).map_err(internal_encode_error)
    }

    async fn dissolve_group(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let group_ref = parse_ref(arguments, "groupRef", VibexUseResourceKind::Group)?;
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::DissolveGroup,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let group_id = group_ref.group_id().ok_or_else(|| {
            VibexError::validation(use_codes::REQUEST_INVALID, "groupRef is invalid")
        })?;
        let record =
            GroupPresentationRepository::get(&conn, group_id.as_str())?.ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Permission,
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "group not found",
                )
            })?;
        if record.actor_key != actor.session_id.as_str() || !record.created_by_caller {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::SCOPE_DENIED,
                "only the group's creator may dissolve it",
            ));
        }
        if let Some(expected) = optional_u64(arguments, "expectedRevision")
            && expected != record.revision
        {
            return Err(VibexError::conflict(
                use_codes::PRESENTATION_LAYOUT_CONFLICT,
                "the group changed since it was read",
            ));
        }
        GroupPresentationRepository::delete(&conn, group_id.as_str())?;
        if self.presentation.is_attached() {
            let _ = self.presentation.dissolve(&group_id).await;
        }
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        Ok(serde_json::json!({
            "groupRef": group_ref.as_uri(),
            "dissolved": true,
            "message": "the group is gone; its sessions and tasks are unchanged",
        }))
    }

    fn resolve_group_members(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<Vec<VibexSessionId>> {
        let entries = arguments
            .get("memberSessionRefs")
            .and_then(serde_json::Value::as_array)
            .ok_or_else(|| {
                VibexError::validation(use_codes::REQUEST_INVALID, "memberSessionRefs is required")
            })?;
        if entries.is_empty() || entries.len() > VIBEX_USE_MAX_GROUP_MEMBERS {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("memberSessionRefs must hold 1..={VIBEX_USE_MAX_GROUP_MEMBERS} entries"),
            ));
        }
        let mut members = Vec::new();
        for entry in entries {
            let raw = entry.as_str().ok_or_else(|| {
                VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "memberSessionRefs entries must be strings",
                )
            })?;
            let reference = VibexUseRef::parse(raw).ok_or_else(|| {
                VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "memberSessionRefs entry is invalid",
                )
            })?;
            let session_id = session_reference(&reference)?;
            // Being able to see a session is what allows showing it. Group
            // membership never grants control over a member.
            self.require_readable(conn, actor, &session_id)?;
            if !members.contains(&session_id) {
                members.push(session_id);
            }
        }
        Ok(members)
    }

    fn resolve_group_scope(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<SessionGroupScope> {
        let scope = arguments.get("scope").cloned().unwrap_or_default();
        let kind = optional_string(&scope, "kind", 32);
        if matches!(kind.as_deref(), Some("project_team") | Some("projectTeam")) {
            // The first version groups one Worktree. Reporting the limit is
            // better than silently dropping members that live elsewhere.
            return Err(VibexError::capability(
                use_codes::CAPABILITY_UNAVAILABLE,
                "a group spanning several Worktrees is not supported yet",
            )
            .with_diagnostic(
                "reason",
                VibexUseUnavailableReason::CrossWorkspaceUnsupported.as_str(),
            ));
        }
        let reference =
            match parse_optional_ref(&scope, "workspaceRef", VibexUseResourceKind::Workspace)? {
                Some(reference) => reference,
                None => {
                    let parent =
                        SessionRepository::get(conn, &actor.session_id)?.ok_or_else(|| {
                            VibexError::new(
                                ErrorCategory::Permission,
                                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                                "session not found",
                            )
                        })?;
                    VibexUseRef::workspace(parent.workspace_id.as_str())
                }
            };
        // Members and scope have to agree *before* the request is accepted. A
        // client can only drop a foreign-workspace member silently, and a group
        // whose stored members are not the ones on screen is a lie the caller
        // would keep reading back.
        if let Some(members) = arguments
            .get("memberSessionRefs")
            .and_then(|value| value.as_array())
        {
            let mut foreign = Vec::new();
            for entry in members.iter().take(VIBEX_USE_MAX_GROUP_MEMBERS) {
                let Ok(session_ref) = parse_ref(entry, "sessionRef", VibexUseResourceKind::Session)
                else {
                    continue;
                };
                let Ok(session_id) = session_reference(&session_ref) else {
                    continue;
                };
                let Some(session) = SessionRepository::get(conn, &session_id)? else {
                    continue;
                };
                if session.workspace_id.as_str() != reference.id {
                    foreign.push(session_ref.as_uri());
                }
            }
            if !foreign.is_empty() {
                return Err(VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "every member of a group must belong to the group's workspace",
                )
                .with_diagnostic("reason", "cross_workspace_unsupported")
                .with_diagnostic("members", foreign.join(",")));
            }
        }
        Ok(SessionGroupScope::Workspace {
            workspace_ref: reference,
        })
    }

    // -----------------------------------------------------------------------
    // Selection, context and workspace resolution
    // -----------------------------------------------------------------------

    /// Resolves the runtime a new task should start on.
    ///
    /// It deliberately owns no connection: a check that needs the database
    /// opens one for the synchronous part only, so this future stays `Send`.
    async fn resolve_selection(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<SessionRuntimeSelection> {
        let target = arguments.get("target").cloned().unwrap_or_default();
        let selection_ref = optional_string(&target, "selectionRef", 512)
            .or_else(|| optional_string(arguments, "selectionRef", 512));
        let Some(selection_ref) = selection_ref else {
            // No explicit option: the caller may still have named an Agent and
            // a configuration. That is a *typed* selection, so it resolves
            // against the same catalogue instead of guessing an account.
            let agent_id = optional_string(&target, "agentId", 128);
            let model = optional_string(&target, "model", 256);
            let reasoning_effort = optional_string(&target, "reasoningEffort", 128);
            let mode_id = optional_string(&target, "modeId", 128);
            if agent_id.is_none()
                && model.is_none()
                && reasoning_effort.is_none()
                && mode_id.is_none()
            {
                // A legacy-shaped call inherits the caller's effective runtime.
                let conn = self.open()?;
                return self.effective_selection(&conn, &actor.session_id);
            }
            return self
                .resolve_typed_selection(
                    &target,
                    agent_id.as_deref(),
                    model.as_deref(),
                    reasoning_effort.as_deref(),
                    mode_id.as_deref(),
                )
                .await;
        };
        let reference = VibexUseRef::parse(&selection_ref).ok_or_else(|| {
            VibexError::validation(
                use_codes::REQUEST_INVALID,
                "selectionRef is not a vibex resource reference",
            )
        })?;
        if reference.kind != VibexUseResourceKind::RuntimeOption {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                "selectionRef must reference a runtime option",
            ));
        }
        let catalog = self.runtime_catalog.list().await?;
        if let Some(expected) = optional_u64(&target, "catalogRevision")
            .or_else(|| optional_u64(arguments, "catalogRevision"))
            && i64::try_from(expected).unwrap_or(i64::MAX) != catalog.revision
        {
            return Err(VibexError::conflict(
                use_codes::CATALOG_STALE,
                "the runtime option catalogue changed; discover the available targets again",
            )
            .with_diagnostic("catalogRevision", catalog.revision.to_string()));
        }
        let option = catalog
            .options
            .iter()
            .find(|option| runtime_option_ref(option) == reference.as_uri())
            .ok_or_else(|| {
                VibexError::validation(
                    use_codes::TARGET_UNAVAILABLE,
                    "that runtime option is no longer offered by this runtime",
                )
            })?;
        if let Some(reason) = runtime_option_unavailable(option) {
            return Err(VibexError::capability(
                use_codes::TARGET_UNAVAILABLE,
                "that runtime option cannot start a session right now",
            )
            .with_diagnostic("reason", reason.as_str()));
        }
        Ok(option.selection.clone())
    }

    /// Resolves a selection the caller named field by field.
    ///
    /// Only the catalogue decides what is real, and an ambiguous match is
    /// refused rather than resolved: picking one of two accounts on the user's
    /// behalf could silently move the work onto a paid plan the caller never
    /// asked for. When more than one configuration matches, the caller is told
    /// to choose with `selectionRef`.
    async fn resolve_typed_selection(
        &self,
        target: &serde_json::Value,
        agent_id: Option<&str>,
        model: Option<&str>,
        reasoning_effort: Option<&str>,
        mode_id: Option<&str>,
    ) -> VibexResult<SessionRuntimeSelection> {
        let catalog = self.runtime_catalog.list().await?;
        if let Some(expected) = optional_u64(target, "catalogRevision")
            && i64::try_from(expected).unwrap_or(i64::MAX) != catalog.revision
        {
            return Err(VibexError::conflict(
                use_codes::CATALOG_STALE,
                "the runtime option catalogue changed; discover the available targets again",
            )
            .with_diagnostic("catalogRevision", catalog.revision.to_string()));
        }
        let agent_id = agent_id.map(|value| {
            AgentId::parse(value).map_err(|_| {
                VibexError::validation(use_codes::REQUEST_INVALID, "target.agentId is invalid")
            })
        });
        let agent_id = agent_id.transpose()?;
        let mut candidates = Vec::new();
        for option in &catalog.options {
            if let Some(agent_id) = agent_id.as_ref()
                && &option.selection.agent_id != agent_id
            {
                continue;
            }
            if let Some(model) = model
                && option.selection.model_id() != Some(model)
                && !(model == "agent-default" && option.selection.model_id().is_none())
            {
                continue;
            }
            if let Some(reasoning) = reasoning_effort
                && option.selection.reasoning_effort.as_deref() != Some(reasoning)
            {
                continue;
            }
            if let Some(mode) = mode_id
                && option.selection.mode_id.as_deref() != Some(mode)
            {
                continue;
            }
            if runtime_option_unavailable(option).is_none() {
                candidates.push(option);
            }
        }
        match candidates.len() {
            0 => Err(VibexError::new(
                ErrorCategory::Capability,
                use_codes::TARGET_UNAVAILABLE,
                "no enabled configuration matches that target",
            )),
            1 => Ok(candidates[0].selection.clone()),
            _ => Err(VibexError::conflict(
                use_codes::TARGET_UNAVAILABLE,
                "several configurations match that target; choose one with selectionRef",
            )
            .with_diagnostic(
                "candidates",
                candidates
                    .iter()
                    .map(|option| runtime_option_ref(option))
                    .collect::<Vec<String>>()
                    .join(","),
            )),
        }
    }

    /// The runtime selection revision a session was last resolved to.
    ///
    /// It comes from the durable runtime state rather than from a live
    /// catalogue read: an execution records the revision it actually ran under,
    /// not whichever revision happens to be current when somebody looks.
    fn runtime_catalog_revision(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> Option<u64> {
        let state = AgentSessionRuntimeRepository::get_runtime_state(conn, session_id).ok()??;
        Some(u64::try_from(state.selection_revision).unwrap_or_default())
    }

    /// The runtime option reference of a session's *effective* configuration.
    ///
    /// Desired and effective are different facts while a switch is in flight;
    /// only the effective one describes what a message would actually run
    /// under.
    fn effective_selection_ref(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> Option<String> {
        let state = AgentSessionRuntimeRepository::get_runtime_state(conn, session_id).ok()??;
        let selection = state.effective_runtime_selection?;
        Some(VibexUseRef::runtime_option(&selection_fingerprint(&selection)).as_uri())
    }

    /// Renders the declared context windows into the provider-only preamble.
    ///
    /// Only the declared range travels: the referenced session's remaining
    /// history is not shared, which is what makes "reference a range" different
    /// from "hand over the conversation".
    fn render_context_block(
        &self,
        conn: &DbConnection,
        refs: &[DelegationContextRef],
    ) -> VibexResult<Option<String>> {
        if refs.is_empty() {
            return Ok(None);
        }
        let mut rendered = vec![
            "The user declared the context below for this request. It is the whole of what you \
             were given from those conversations."
                .to_string(),
        ];
        for reference in refs.iter().take(VIBEX_USE_MAX_CONTEXT_REFS) {
            let session_id = session_reference(&reference.session_ref)?;
            let session = SessionRepository::get(conn, &session_id)?;
            let label = session
                .as_ref()
                .map(|session| session.title.clone())
                .unwrap_or_else(|| session_id.as_str().to_string());
            let window = match (reference.from_sequence, reference.through_sequence) {
                (Some(from), Some(through)) if through >= from && from > 0 => {
                    TimelineRepository::fetch_range(conn, &session_id, from, through)?
                }
                (Some(from), _) => {
                    TimelineRepository::fetch_after(conn, &session_id, Some(from), 200)?.items
                }
                (None, Some(through)) => {
                    TimelineRepository::fetch_before(conn, &session_id, Some(through), 200)?.items
                }
                (None, None) => TimelineRepository::fetch_after(conn, &session_id, None, 50)?.items,
            };
            let view = reference.view.unwrap_or(SessionReadView::Conversation);
            rendered.push(format!(
                "--- {} ({}..{}) ---",
                label,
                reference
                    .from_sequence
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "start".to_string()),
                reference
                    .through_sequence
                    .map(|value| value.to_string())
                    .unwrap_or_else(|| "end".to_string()),
            ));
            let mut budget = VIBEX_USE_MAX_READ_CHARS;
            for item in window.iter() {
                if !view_includes(view, item) {
                    continue;
                }
                let Some(text) = item_text(item) else {
                    continue;
                };
                if text.trim().is_empty() {
                    continue;
                }
                let (bounded, cut) = bounded_chars(&text, budget.min(8_000));
                budget = budget.saturating_sub(bounded.chars().count());
                rendered.push(format!(
                    "[{}] {}{}",
                    item.sequence,
                    bounded,
                    if cut { " […truncated]" } else { "" }
                ));
                if budget == 0 {
                    rendered.push("[the declared window was truncated]".to_string());
                    break;
                }
            }
        }
        Ok(Some(rendered.join("\n")))
    }

    fn effective_selection(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> VibexResult<SessionRuntimeSelection> {
        let state = AgentSessionRuntimeRepository::get_runtime_state(conn, session_id)?
            .ok_or_else(|| {
                VibexError::conflict(
                    "agent_delegation_parent_runtime_missing",
                    "the session has no durable runtime selection",
                )
            })?;
        state.effective_runtime_selection.ok_or_else(|| {
            VibexError::conflict(
                "agent_delegation_parent_runtime_missing",
                "the session has no effective runtime selection",
            )
        })
    }

    fn resolve_workspace(
        &self,
        conn: &DbConnection,
        workspace_ref: Option<&str>,
        capability: &VibexUseCapabilitySnapshot,
    ) -> VibexResult<WorkspaceRecord> {
        let workspace_id = match workspace_ref {
            Some(value) => {
                let reference = VibexUseRef::parse(value).ok_or_else(|| {
                    VibexError::validation(use_codes::REQUEST_INVALID, "workspaceRef is invalid")
                })?;
                if reference.kind != VibexUseResourceKind::Workspace {
                    return Err(VibexError::validation(
                        use_codes::REQUEST_INVALID,
                        "workspaceRef must reference a workspace",
                    ));
                }
                reference.id
            }
            None => capability.current_workspace_ref.id.clone(),
        };
        WorkspaceRepository::list(conn)?
            .into_iter()
            .map(|(_, workspace)| workspace)
            .find(|workspace| workspace.id.as_str() == workspace_id)
            .filter(|workspace| {
                // Discovery only marks the caller's own checkout as shareable.
                // Accepting any known workspace would let an Agent reach a
                // different project's files just by naming its id, which is the
                // jump the workspace reference exists to prevent.
                workspace.id.as_str() == capability.current_workspace_ref.id
            })
            .ok_or_else(|| {
                VibexError::new(
                    ErrorCategory::Permission,
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "workspace not found",
                )
            })
    }

    /// Builds the explicit context windows a child receives.
    ///
    /// Only the declared range travels: the parent transcript is never copied
    /// wholesale, and a reference the caller may not read is refused rather
    /// than quietly emptied.
    fn resolve_context(
        &self,
        conn: &DbConnection,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<Vec<DelegationContextRef>> {
        let Some(entries) = arguments
            .get("context")
            .and_then(serde_json::Value::as_array)
        else {
            return Ok(Vec::new());
        };
        if entries.len() > VIBEX_USE_MAX_CONTEXT_REFS {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("at most {VIBEX_USE_MAX_CONTEXT_REFS} context windows are accepted"),
            ));
        }
        let mut refs = Vec::new();
        for entry in entries {
            let session_ref = parse_ref(entry, "sessionRef", VibexUseResourceKind::Session)?;
            let session_id = session_reference(&session_ref)?;
            self.require_readable(conn, actor, &session_id)?;
            let cursor = parse_read_cursor(conn, entry, &session_ref)?;
            let page = self.session_read_page(conn, &session_ref, cursor.clone(), entry)?;
            let (from, through) = match cursor.anchor {
                SessionReadAnchor::After { sequence } => {
                    (Some(sequence), Some(page.snapshot_end_sequence))
                }
                SessionReadAnchor::Before { sequence } => (None, Some(sequence)),
                SessionReadAnchor::Latest => (
                    page.items.first().map(|item| item.sequence),
                    page.items.last().map(|item| item.sequence),
                ),
            };
            refs.push(DelegationContextRef {
                session_ref,
                from_sequence: from,
                through_sequence: through,
                view: Some(cursor.view),
                included_chars: page
                    .items
                    .iter()
                    .map(|item| item.text.chars().count())
                    .sum(),
                truncated: page.truncated,
            });
        }
        Ok(refs)
    }

    // -----------------------------------------------------------------------
    // send_message
    // -----------------------------------------------------------------------

    fn send_message(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let session_ref = parse_ref(arguments, "sessionRef", VibexUseResourceKind::Session)?;
        let session_id = session_reference(&session_ref)?;
        let text = required_string(arguments, "text", 64 * 1024)?;
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::SendMessage,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        if let Err(error) = self.require_writable(&conn, actor, &session_id) {
            self.settle_operation(
                &conn,
                &operation,
                VibexUseOperationState::Failed,
                Some(&error),
            );
            return Err(error);
        }
        let session = SessionRepository::get(&conn, &session_id)?.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "session not found",
            )
        })?;
        if matches!(
            session.state,
            AgentSessionState::Closed | AgentSessionState::Archived
        ) {
            let error = VibexError::conflict(
                use_codes::SCOPE_DENIED,
                "that session is closed and cannot accept messages",
            );
            self.settle_operation(
                &conn,
                &operation,
                VibexUseOperationState::Failed,
                Some(&error),
            );
            return Err(error);
        }
        let task = match parse_optional_ref(arguments, "taskRef", VibexUseResourceKind::Task)? {
            Some(reference) => {
                let task_id = reference.task_id().ok_or_else(|| {
                    VibexError::validation(use_codes::REQUEST_INVALID, "taskRef is invalid")
                })?;
                let task = AgentDelegationRepository::get(&conn, &task_id)?.ok_or_else(|| {
                    VibexError::new(
                        ErrorCategory::Permission,
                        use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                        "task not found",
                    )
                })?;
                if task.child_session_id.as_ref() != Some(&session_id) {
                    return Err(VibexError::validation(
                        use_codes::REQUEST_INVALID,
                        "taskRef does not belong to that session",
                    ));
                }
                if task.phase().is_terminal() {
                    return Err(VibexError::conflict(
                        use_codes::TASK_TERMINAL,
                        "that task is already finished; start a new task that follows it",
                    ));
                }
                // A cancel that has already been accepted is a fence. Accepting
                // a follow-up here would silently undo the stop the caller was
                // promised, and would let a cancelled task drift back to active.
                if task.phase() == DelegationTaskPhase::Cancelling {
                    return Err(VibexError::conflict(
                        use_codes::TASK_CANCELLING,
                        "that task is stopping and cannot accept new messages",
                    ));
                }
                Some(task)
            }
            None => self.current_task_for_session(&conn, &session_id)?,
        };

        // A caller that names the execution it is answering must be told when
        // that execution is already gone. Silently appending to whatever is
        // running now would attach the follow-up to a different round.
        if let Some(expected) = parse_optional_ref(
            arguments,
            "expectedExecutionRef",
            VibexUseResourceKind::Execution,
        )? {
            let expected_id = expected.execution_id().ok_or_else(|| {
                VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "expectedExecutionRef is invalid",
                )
            })?;
            let current = task
                .as_ref()
                .and_then(|task| task.current_execution_id.clone());
            if current.as_ref() != Some(&expected_id) {
                let error = VibexError::conflict(
                    use_codes::REVISION_CONFLICT,
                    "the execution this message answers is no longer the current one",
                )
                .with_diagnostic("expectedExecutionRef", expected.as_uri())
                .with_diagnostic(
                    "currentExecutionRef",
                    current
                        .as_ref()
                        .map(VibexUseRef::execution)
                        .map(|reference| reference.as_uri())
                        .unwrap_or_default(),
                );
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        }
        // The declared context window is authorized and bounded here exactly as
        // it is for a delegation; a follow-up must not be able to widen it.
        let context_refs = match self.resolve_context(&conn, actor, arguments) {
            Ok(context_refs) => context_refs,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        if let Some(requested) = optional_string(arguments, "selectionRef", 512) {
            // Choosing the configuration a message runs under is a real runtime
            // switch, not a message field. Refusing it by name is honest;
            // ignoring it would send the message under a configuration the
            // caller did not ask for.
            let requested = VibexUseRef::parse(&requested).ok_or_else(|| {
                VibexError::validation(use_codes::REQUEST_INVALID, "selectionRef is invalid")
            })?;
            if requested.kind != VibexUseResourceKind::RuntimeOption {
                return Err(VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "selectionRef must reference a runtime option",
                ));
            }
            if Some(requested.as_uri()) != self.effective_selection_ref(&conn, &session_id) {
                let error = VibexError::new(
                    ErrorCategory::Capability,
                    use_codes::CAPABILITY_UNAVAILABLE,
                    "changing the configuration of a running task is not available; \
                     switch the session runtime first, then send the message",
                )
                .with_diagnostic("selectionRef", requested.as_uri());
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        }
        // The controller is what keeps two writers from interleaving in one
        // session. A user takeover is respected rather than overridden.
        if let Some(task) = task.as_ref() {
            let expected = optional_u64(arguments, "expectedControllerRevision");
            let claim = SessionControllerRepository::claim(
                &conn,
                &session_id,
                &task.id,
                &task.parent_session_id,
                expected,
            )?;
            let conflict = match claim {
                SessionControllerClaim::Claimed(_) => None,
                SessionControllerClaim::Denied(controller) => Some((
                    "another task currently owns this session's message queue",
                    controller.revision,
                )),
                SessionControllerClaim::HumanControlled(controller) => {
                    Some(("the user took this session over", controller.revision))
                }
                SessionControllerClaim::StaleRevision(controller) => Some((
                    "the session controller changed since it was read",
                    controller.revision,
                )),
            };
            if let Some((message, revision)) = conflict {
                let error = VibexError::conflict(use_codes::CONTROLLER_CHANGED, message)
                    .with_diagnostic("controllerRevision", revision.to_string());
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        }

        // A follow-up keeps the target session's own effective configuration.
        let desired_runtime = match self.effective_selection(&conn, &session_id) {
            Ok(selection) => selection,
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        };
        let prompt_context = self.render_context_block(&conn, &context_refs)?;
        let execution_provenance = MessageProvenance::DelegatedInput {
            actor_session_ref: VibexUseRef::session(&actor.session_id),
            task_ref: task.as_ref().map(|task| VibexUseRef::task(&task.id)),
            operation_ref: operation.operation_ref.clone(),
        };
        let message_key = format!("vibex-use:{}", operation.id.as_str());
        let submission_id =
            match self
                .require_coordinator()?
                .prepare_submission(SendAgentMessageRequest {
                    session_id: session_id.clone(),
                    message_idempotency_key: message_key.clone(),
                    desired_runtime,
                    text,
                    attachments: Vec::new(),
                    reasoning_effort: None,
                    correlation_id: None,
                    delivery: UserMessageDelivery::Prompt,
                    prompt_context,
                    provenance: execution_provenance.clone(),
                }) {
                Ok(submission_id) => submission_id,
                Err(error) => {
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
            };
        let execution = self.record_execution(
            &conn,
            task.as_ref(),
            &session_id,
            &submission_id,
            &message_key,
            execution_provenance,
        )?;
        if let Some(task) = task.as_ref() {
            let _ = transition_delegation(&conn, &task.id, DelegationTaskPhase::Active, None, None);
            let root = task
                .root_session_id
                .clone()
                .unwrap_or_else(|| task.parent_session_id.clone());
            let _ = VibexUseEventRepository::append(
                &conn,
                &format!("execution_accepted_{}", execution.id.as_str()),
                Some(&root),
                DelegationTaskEventKind::ExecutionAccepted,
                Some(&task.id),
                task.child_session_id.as_ref(),
                task.revision,
                &serde_json::json!({ "executionRef": execution.execution_ref.as_uri() }),
            );
        }
        self.record_operation_resource(
            &conn,
            &operation,
            "execution",
            execution.execution_ref.clone(),
        );
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        self.notify_progress();
        let queue_position = usize::try_from(
            vibex_db::count_active_executions_for_root(&conn, &self.actor_root(&conn, actor)?)
                .unwrap_or(0),
        )
        .unwrap_or_default();
        serde_json::to_value(MessageAccepted {
            status: MessageAccepted::ENQUEUED,
            session_ref: VibexUseRef::session(&session_id),
            submission_id,
            operation_ref: operation.operation_ref.clone(),
            execution_ref: execution.execution_ref,
            queued: true,
            queue_position,
            provenance: execution.provenance,
        })
        .map_err(internal_encode_error)
    }

    // -----------------------------------------------------------------------
    // wait
    // -----------------------------------------------------------------------

    async fn wait(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let timeout_ms = optional_u64(arguments, "timeoutMs")
            .unwrap_or(0)
            .min(VIBEX_USE_MAX_WAIT_MS);
        let after_cursor = optional_i64(arguments, "afterEventCursor").unwrap_or(0);
        let task_refs = parse_ref_list(arguments, "taskRefs", VibexUseResourceKind::Task)?;
        let session_refs = parse_ref_list(arguments, "sessionRefs", VibexUseResourceKind::Session)?;
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(timeout_ms);
        loop {
            // Registering before reading is what makes this correct: a result
            // that settles between the snapshot and the await still wakes the
            // loop instead of being missed until the timeout.
            let notified = self.progress.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();

            let response =
                self.wait_snapshot(actor, &actor.key(), after_cursor, &task_refs, &session_refs)?;
            if response.outcome != WaitOutcome::TimedOut {
                return serde_json::to_value(response).map_err(internal_encode_error);
            }
            let now = std::time::Instant::now();
            if timeout_ms == 0 || now >= deadline {
                return serde_json::to_value(response).map_err(internal_encode_error);
            }
            let remaining = deadline
                .saturating_duration_since(now)
                .min(WAIT_FALLBACK_POLL);
            tokio::select! {
                _ = &mut notified => {}
                _ = tokio::time::sleep(remaining) => {}
            }
        }
    }

    fn wait_snapshot(
        &self,
        actor: &VibexUseActor,
        consumer_key: &str,
        after_cursor: i64,
        task_refs: &[VibexUseRef],
        session_refs: &[VibexUseRef],
    ) -> VibexResult<WaitResponse> {
        let conn = self.open()?;
        let root = self.actor_root(&conn, actor)?;
        let events = VibexUseEventRepository::unacknowledged_for_root(
            &conn,
            consumer_key,
            &root,
            after_cursor,
            50,
            false,
        )?;
        let mut tasks = Vec::new();
        let mut seen = std::collections::BTreeSet::new();
        for reference in session_refs {
            let Some(session_id) = reference.session_id() else {
                continue;
            };
            // A session the caller cannot read is not a session the caller may
            // watch. Skipping the check here would answer with the target's
            // title, phase and error, which is exactly the disclosure a
            // refused read prevents.
            self.require_readable(&conn, actor, &session_id)?;
            if let Some(task) = self.current_task_for_session(&conn, &session_id)?
                && seen.insert(task.id.as_str().to_string())
            {
                tasks.push(self.task_view(&conn, &task)?);
            }
        }
        if !task_refs.is_empty() {
            for reference in task_refs.iter().take(VIBEX_USE_MAX_BATCH_REFS) {
                let Some(task_id) = reference.task_id() else {
                    continue;
                };
                let Some(task) = AgentDelegationRepository::get(&conn, &task_id)? else {
                    continue;
                };
                if let Some(child) = task.child_session_id.as_ref() {
                    self.require_readable(&conn, actor, child)?;
                } else {
                    self.require_readable(&conn, actor, &task.parent_session_id)?;
                }
                if seen.insert(task.id.as_str().to_string()) {
                    tasks.push(self.task_view(&conn, &task)?);
                }
            }
        } else if session_refs.is_empty() || tasks.is_empty() {
            for task in vibex_db::list_delegations_for_root(&conn, &root, true)? {
                // The root is the caller's own team, but a task in it may have
                // been created for a session the caller was only granted a
                // narrow view of; the per-task check keeps that grant honest.
                let reachable = match task.child_session_id.as_ref() {
                    Some(child) => self.require_readable(&conn, actor, child).is_ok(),
                    None => self
                        .require_readable(&conn, actor, &task.parent_session_id)
                        .is_ok(),
                };
                if !reachable {
                    continue;
                }
                if seen.insert(task.id.as_str().to_string()) {
                    tasks.push(self.task_view(&conn, &task)?);
                }
            }
        }

        let mut outcome = WaitOutcome::TimedOut;
        // `Settled` means a watcher reached a phase the caller does not have to
        // wait through any more. Acceptance events are progress, not settlement;
        // treating them as settlement woke a caller on its own first request and
        // then told it nothing had finished.
        if tasks.iter().any(|task| {
            matches!(
                task.phase,
                DelegationTaskPhase::AwaitingReview
                    | DelegationTaskPhase::Completed
                    | DelegationTaskPhase::Failed
                    | DelegationTaskPhase::Cancelled
            )
        }) || events.iter().any(|event| event.kind.settles_a_wait())
        {
            outcome = WaitOutcome::Settled;
        }
        if events.iter().any(|event| event.kind.is_attention())
            || tasks.iter().any(|task| task.blocked_on.is_some())
        {
            outcome = WaitOutcome::Attention;
        } else if outcome == WaitOutcome::TimedOut
            && tasks.iter().any(|task| {
                matches!(
                    task.phase,
                    DelegationTaskPhase::Failed | DelegationTaskPhase::Cancelled
                )
            })
        {
            outcome = WaitOutcome::Failed;
        }
        let event_cursor = events
            .last()
            .map(|event| event.cursor)
            .unwrap_or(after_cursor);
        let more_expected = tasks.iter().any(|task| task.phase.is_active());
        Ok(WaitResponse {
            outcome,
            events,
            tasks,
            event_cursor,
            more_expected,
        })
    }

    // -----------------------------------------------------------------------
    // finish / interrupt / cancel
    // -----------------------------------------------------------------------

    fn finish_task(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let task_ref = parse_ref(arguments, "taskRef", VibexUseResourceKind::Task)?;
        let task_id = task_ref.task_id().ok_or_else(|| {
            VibexError::validation(use_codes::REQUEST_INVALID, "taskRef is invalid")
        })?;
        let conn = self.open()?;
        let task = AgentDelegationRepository::get(&conn, &task_id)?.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "task not found",
            )
        })?;
        self.require_writable(&conn, actor, &task.parent_session_id)?;
        if task.phase().is_terminal() {
            return Err(VibexError::conflict(
                use_codes::TASK_TERMINAL,
                "that task already reached a terminal state",
            )
            .with_diagnostic("phase", task.phase().as_str()));
        }
        if let Some(expected) = optional_u64(arguments, "expectedRevision")
            && expected != task.revision
        {
            return Err(VibexError::conflict(
                use_codes::REVISION_CONFLICT,
                "the task changed since it was read",
            )
            .with_diagnostic("revision", task.revision.to_string()));
        }
        let accepted = optional_string(arguments, "outcome", 32).as_deref() != Some("rejected");
        let summary = optional_string(arguments, "summary", VIBEX_USE_SUMMARY_CHARS);
        let phase = if accepted {
            DelegationTaskPhase::Completed
        } else {
            DelegationTaskPhase::Failed
        };
        let finished = transition_delegation(
            &conn,
            &task.id,
            phase,
            summary.as_deref(),
            (!accepted).then_some("task_rejected"),
        )?
        .ok_or_else(|| {
            VibexError::conflict(use_codes::TASK_TERMINAL, "that task is already terminal")
        })?;
        self.close_open_execution(&conn, &finished)?;
        let root = finished
            .root_session_id
            .clone()
            .unwrap_or_else(|| finished.parent_session_id.clone());
        VibexUseEventRepository::append(
            &conn,
            &format!("task_finished_{}", vibex_core::EventId::new().as_str()),
            Some(&root),
            DelegationTaskEventKind::TaskFinished,
            Some(&finished.id),
            finished.child_session_id.as_ref(),
            finished.revision,
            &serde_json::json!({
                "taskRef": VibexUseRef::task(&finished.id).as_uri(),
                "outcome": if accepted { "accepted" } else { "rejected" },
            }),
        )?;
        if let Some(child) = finished.child_session_id.as_ref() {
            let _ = SessionControllerRepository::release(&conn, child, &finished.id);
        }
        self.notify_progress();
        Ok(serde_json::json!({
            "taskRef": VibexUseRef::task(&finished.id).as_uri(),
            "phase": finished.phase(),
            "revision": finished.revision,
            "completedAtMs": finished.completed_at_ms,
            "resultRefs": finished.result_refs,
        }))
    }

    /// Closes the execution a task was waiting on when its owner accepts it.
    fn close_open_execution(&self, conn: &DbConnection, task: &AgentDelegation) -> VibexResult<()> {
        let Some(execution_id) = task.current_execution_id.as_ref() else {
            return Ok(());
        };
        let Some(execution) = VibexUseExecutionRepository::get(conn, execution_id)? else {
            return Ok(());
        };
        if execution.is_settled() {
            return Ok(());
        }
        VibexUseExecutionRepository::settle(
            conn,
            &execution.id,
            ExecutionOutcome::Completed,
            Some("task_finished"),
            execution.summary.as_deref(),
            &execution.result_ranges,
            &execution.artifact_refs,
            execution.usage,
            execution.truncated,
            execution.end_sequence,
            unix_timestamp_ms(),
        )?;
        Ok(())
    }

    async fn interrupt(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let session_ref = parse_ref(arguments, "sessionRef", VibexUseResourceKind::Session)?;
        let session_id = session_reference(&session_ref)?;
        let expected_execution = optional_string(arguments, "expectedExecutionRef", 512);
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::Interrupt,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        if let Err(error) = self.require_writable(&conn, actor, &session_id) {
            self.settle_operation(
                &conn,
                &operation,
                VibexUseOperationState::Failed,
                Some(&error),
            );
            return Err(error);
        }
        if let Some(expected) = expected_execution {
            let reference = VibexUseRef::parse(&expected).ok_or_else(|| {
                VibexError::validation(
                    use_codes::REQUEST_INVALID,
                    "expectedExecutionRef is invalid",
                )
            })?;
            let current = self
                .current_task_for_session(&conn, &session_id)?
                .and_then(|task| task.current_execution_id);
            if current.as_ref().map(VibexUseRef::execution) != Some(reference) {
                let error = VibexError::conflict(
                    use_codes::CONTROLLER_CHANGED,
                    "another execution started in that session",
                );
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                return Err(error);
            }
        }
        match self.manager.interrupt(&session_id).await {
            Ok(()) => {
                self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
                self.notify_progress();
                Ok(serde_json::json!({
                    "status": "accepted",
                    "sessionRef": session_ref.as_uri(),
                    "operationRef": operation.operation_ref.as_uri(),
                    "message": "the current execution was aborted; the task keeps its phase",
                }))
            }
            Err(error) => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(&error),
                );
                Err(error)
            }
        }
    }

    async fn cancel_task(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let caller_key =
            required_string(arguments, "idempotencyKey", VIBEX_USE_IDEMPOTENCY_KEY_CHARS)?;
        let task_ref = parse_ref(arguments, "taskRef", VibexUseResourceKind::Task)?;
        let cascade = optional_bool(arguments, "cascade", false);
        let fingerprint = arguments_fingerprint(arguments);
        let conn = self.open()?;
        let (operation, claimed) = self.reserve_operation(
            &conn,
            actor,
            VibexUseTool::CancelTask,
            &caller_key,
            &fingerprint,
        )?;
        if !claimed {
            return self.operation_view(&conn, &operation);
        }
        let task_id = task_ref.task_id().ok_or_else(|| {
            VibexError::validation(use_codes::REQUEST_INVALID, "taskRef is invalid")
        })?;
        let task = AgentDelegationRepository::get(&conn, &task_id)?.ok_or_else(|| {
            VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "task not found",
            )
        })?;
        self.require_writable(&conn, actor, &task.parent_session_id)?;
        if task.phase().is_terminal() {
            return Ok(serde_json::json!({
                "taskRef": task_ref.as_uri(),
                "phase": task.phase(),
                "cancelled": false,
                "message": "the task had already finished; cancellation is a no-op",
            }));
        }
        let mut affected = vec![task.id.clone()];
        if cascade && let Some(child) = task.child_session_id.as_ref() {
            for descendant_id in SessionOwnershipRepository::descendant_ids(&conn, child)? {
                for descendant in vibex_db::list_delegations_for_child(&conn, &descendant_id)? {
                    if !descendant.phase().is_terminal() {
                        affected.push(descendant.id);
                    }
                }
            }
        }
        // Descendants first, so a cancelled parent never leaves a child that
        // still believes it is running.
        affected.reverse();
        affected.dedup();
        let mut cancelled = Vec::new();
        for delegation_id in affected {
            match self
                .manager
                .cancel_agent_delegation(vibex_core::CancelAgentDelegationRequest {
                    parent_session_id: task.parent_session_id.clone(),
                    delegation_id,
                })
                .await
            {
                Ok(cancelled_task) => cancelled.push(cancelled_task),
                Err(error) => {
                    self.settle_operation(
                        &conn,
                        &operation,
                        VibexUseOperationState::Failed,
                        Some(&error),
                    );
                    return Err(error);
                }
            }
        }
        self.settle_operation(&conn, &operation, VibexUseOperationState::Succeeded, None);
        self.notify_progress();
        Ok(serde_json::json!({
            "taskRef": task_ref.as_uri(),
            "cancelled": true,
            "tasks": cancelled
                .iter()
                .map(|task| serde_json::json!({
                    "taskRef": VibexUseRef::task(&task.id).as_uri(),
                    "phase": task.phase(),
                }))
                .collect::<Vec<serde_json::Value>>(),
        }))
    }

    // -----------------------------------------------------------------------
    // events
    // -----------------------------------------------------------------------

    fn get_events(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let root = self.actor_root(&conn, actor)?;
        let after_cursor = optional_i64(arguments, "afterEventCursor").unwrap_or(0);
        let max_items = bounded_usize(arguments, "maxItems", 50, 200);
        let events = VibexUseEventRepository::unacknowledged_for_root(
            &conn,
            &actor.key(),
            &root,
            after_cursor,
            max_items,
            optional_bool(arguments, "includeAcknowledged", false),
        )?;
        let next_cursor = events
            .last()
            .map(|event| event.cursor)
            .unwrap_or(after_cursor);
        Ok(serde_json::json!({
            "events": events,
            "nextEventCursor": next_cursor,
            "hasMore": events.len() >= max_items,
            "message": "events repeat until acknowledged; deduplicate by eventId",
        }))
    }

    fn ack_events(
        &self,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<serde_json::Value> {
        let conn = self.open()?;
        let consumer_key = actor.key();
        let mut acknowledged = 0;
        if let Some(through) = optional_i64(arguments, "throughEventCursor") {
            acknowledged +=
                VibexUseEventRepository::acknowledge_through(&conn, &consumer_key, through)?;
        }
        if let Some(ids) = arguments
            .get("eventIds")
            .and_then(serde_json::Value::as_array)
        {
            let ids: Vec<String> = ids
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(ToString::to_string)
                .collect();
            acknowledged += VibexUseEventRepository::acknowledge(&conn, &consumer_key, &ids)?;
        }
        Ok(serde_json::json!({
            "acknowledged": acknowledged,
            "message": "acknowledgement is a transport fact; it does not claim the content was understood",
        }))
    }

    // -----------------------------------------------------------------------
    // User-facing ownership tree
    // -----------------------------------------------------------------------

    /// Direct children of one session, with the task that created each.
    ///
    /// The list is read from the ownership table rather than from a loaded
    /// parent timeline, so a child that arrived while the parent card was off
    /// screen still shows up.
    pub fn session_tree_children(
        &self,
        parent_session_id: &VibexSessionId,
    ) -> VibexResult<Vec<SessionTreeNode>> {
        let conn = self.open()?;
        let mut nodes = Vec::new();
        for child in SessionOwnershipRepository::child_ids(&conn, parent_session_id)? {
            if let Some(node) = self.session_tree_node_on(&conn, &child)? {
                nodes.push(node);
            }
        }
        Ok(nodes)
    }

    /// Every delegated descendant of the given roots, in one read.
    ///
    /// The shell needs the whole forest to draw the ownership tree and to
    /// register group members, and reading it once keeps that off the render
    /// path's per-parent query cost.
    pub fn delegation_forest(&self, roots: &[VibexSessionId]) -> VibexResult<Vec<SessionTreeNode>> {
        let conn = self.open()?;
        let mut nodes = Vec::new();
        let mut frontier: Vec<VibexSessionId> = roots.to_vec();
        let mut seen: std::collections::BTreeSet<String> =
            roots.iter().map(|root| root.as_str().to_string()).collect();
        for _ in 0..32 {
            if frontier.is_empty() {
                break;
            }
            let mut next = Vec::new();
            for parent in frontier {
                for child in SessionOwnershipRepository::child_ids(&conn, &parent)? {
                    if !seen.insert(child.as_str().to_string()) {
                        continue;
                    }
                    if let Some(node) = self.session_tree_node_on(&conn, &child)? {
                        nodes.push(node);
                    }
                    next.push(child);
                }
            }
            frontier = next;
        }
        Ok(nodes)
    }

    /// The node for one session, regardless of who its parent is.
    pub fn session_tree_node(
        &self,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionTreeNode>> {
        let conn = self.open()?;
        self.session_tree_node_on(&conn, session_id)
    }

    fn session_tree_node_on(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> VibexResult<Option<SessionTreeNode>> {
        let Some(session) = SessionRepository::get(conn, session_id)? else {
            return Ok(None);
        };
        if session.deleted_at_ms.is_some() {
            return Ok(None);
        }
        let parent = SessionOwnershipRepository::parent_of(conn, session_id)?;
        // The newest task that used this session is the one the row summarises;
        // older ones stay readable in the detail view.
        let tasks = vibex_db::list_delegations_for_child(conn, session_id)?;
        let current = tasks
            .iter()
            .rev()
            .find(|task| !task.phase().is_terminal())
            .or_else(|| tasks.last());
        let child_count = SessionOwnershipRepository::child_count(conn, session_id)?;
        let (active_descendants, blocked_descendants) = self.subtree_activity(conn, session_id)?;
        Ok(Some(SessionTreeNode {
            session_ref: VibexUseRef::session(&session.id),
            parent_session_ref: parent.map(|parent| VibexUseRef::session(&parent)),
            title: session.title.clone(),
            agent_id: Some(session.agent_id.clone()),
            agent_label: Some(session.agent_id.to_string()),
            task_ref: current.map(|task| VibexUseRef::task(&task.id)),
            task_title: current.map(|task| task.title.clone()),
            task_phase: current.map(AgentDelegation::phase),
            completion_policy: current.map(|task| task.completion_policy),
            child_count,
            has_more_children: false,
            blocked_on: current
                .and_then(|task| task.blocked_on.clone())
                .or_else(|| self.pending_attention(conn, session_id)),
            active_descendants,
            blocked_descendants,
            current_task_ref: current.map(|task| VibexUseRef::task(&task.id)),
            updated_at_ms: session
                .updated_at_ms
                .max(current.map(|task| task.updated_at_ms).unwrap_or_default()),
        }))
    }

    /// Running and blocked counts across a node's whole subtree.
    ///
    /// These counts are how a collapsed branch keeps showing that something
    /// inside it needs the user, instead of hiding a pending permission request
    /// behind a closed disclosure.
    fn subtree_activity(
        &self,
        conn: &DbConnection,
        session_id: &VibexSessionId,
    ) -> VibexResult<(usize, usize)> {
        let mut active = 0;
        let mut blocked = 0;
        let mut stack = vec![session_id.clone()];
        let mut seen = std::collections::BTreeSet::new();
        while let Some(current) = stack.pop() {
            if !seen.insert(current.as_str().to_string()) {
                continue;
            }
            if let Some(task) = self.current_task_for_session(conn, &current)?
                && task.phase() == DelegationTaskPhase::Active
            {
                active += 1;
            }
            if self.pending_attention(conn, &current).is_some() {
                blocked += 1;
            }
            for child in SessionOwnershipRepository::child_ids(conn, &current)? {
                stack.push(child);
            }
        }
        Ok((active, blocked))
    }

    /// Sessions a group needs the shell's own registry to know about.
    ///
    /// A delegated child is deliberately absent from the root session list, so
    /// the shell has to be handed its metadata before a pane can render it.
    pub fn group_member_registry(
        &self,
        session_ids: &[VibexSessionId],
    ) -> VibexResult<Vec<AgentSession>> {
        let conn = self.open()?;
        let mut sessions = Vec::new();
        for session_id in session_ids {
            if let Some(session) = SessionRepository::get(&conn, session_id)?
                && session.deleted_at_ms.is_none()
            {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    /// Sessions the user may reference from the composer.
    ///
    /// This is the product's own discovery, not an Agent tool: the user can
    /// reference any session they can see, and referencing one grants read
    /// scope rather than control.
    pub fn referenceable_sessions(&self, limit: usize) -> VibexResult<Vec<AgentSession>> {
        let conn = self.open()?;
        let mut sessions = SessionRepository::list_root_sessions(&conn, false)?;
        sessions.sort_by_key(|session| std::cmp::Reverse(session.updated_at_ms));
        sessions.truncate(limit);
        Ok(sessions)
    }

    /// Grants a caller read or control access to a session it did not create.
    ///
    /// The product calls this when the user references a session or hands one
    /// to another Agent. An Agent cannot grant itself a scope.
    pub fn grant_session_scope(
        &self,
        grantee: &VibexSessionId,
        target: &VibexSessionId,
        scope: VibexUseScope,
        granted_by: &str,
    ) -> VibexResult<()> {
        let scope = match scope {
            VibexUseScope::Controlled => "controlled",
            _ => "referenced",
        };
        let conn = self.open()?;
        SessionGrantRepository::grant(&conn, grantee, target, scope, granted_by)?;
        Ok(())
    }

    pub fn revoke_session_scope(
        &self,
        grantee: &VibexSessionId,
        target: &VibexSessionId,
    ) -> VibexResult<bool> {
        let conn = self.open()?;
        SessionGrantRepository::revoke(&conn, grantee, target)
    }
}

// ---------------------------------------------------------------------------
// Tool host
// ---------------------------------------------------------------------------

impl VibexUseToolHost for VibexUseService {
    fn call(
        &self,
        actor: VibexUseActor,
        tool: VibexUseTool,
        arguments: serde_json::Value,
    ) -> VibexUseToolFuture<'_> {
        Box::pin(async move {
            // The activation the sidecar was launched under must still be the
            // live one, so a revoked delivery stops working without waiting for
            // the Agent to restart.
            if actor.activation_revision < self.activation_revision {
                return Err(VibexError::capability(
                    use_codes::CAPABILITY_UNAVAILABLE,
                    "this tool delivery has been superseded; restart the session",
                ));
            }
            match tool {
                VibexUseTool::Discover => self.discover(&actor, &arguments).await,
                VibexUseTool::ListSessions => self.list_sessions(&actor, &arguments),
                VibexUseTool::GetSession => self.get_session(&actor, &arguments),
                VibexUseTool::ReadSession => self.read_session(&actor, &arguments),
                VibexUseTool::CreateSession => self.create_session(&actor, &arguments).await,
                VibexUseTool::Delegate => self.delegate(&actor, &arguments).await,
                VibexUseTool::SendMessage => self.send_message(&actor, &arguments),
                VibexUseTool::GetOperation => self.get_operation(&actor, &arguments),
                VibexUseTool::GetTasks => self.get_tasks(&actor, &arguments),
                VibexUseTool::Wait => self.wait(&actor, &arguments).await,
                VibexUseTool::FinishTask => self.finish_task(&actor, &arguments),
                VibexUseTool::Interrupt => self.interrupt(&actor, &arguments).await,
                VibexUseTool::CancelTask => self.cancel_task(&actor, &arguments).await,
                VibexUseTool::GetEvents => self.get_events(&actor, &arguments),
                VibexUseTool::AckEvents => self.ack_events(&actor, &arguments),
                VibexUseTool::ListGroups => self.list_groups(&actor, &arguments),
                VibexUseTool::CreateGroup => self.create_group(&actor, &arguments).await,
                VibexUseTool::UpdateGroup => self.update_group(&actor, &arguments).await,
                VibexUseTool::PresentGroup => self.present_group(&actor, &arguments).await,
                VibexUseTool::DissolveGroup => self.dissolve_group(&actor, &arguments).await,
            }
        })
    }

    fn tool_definitions(&self, actor: &VibexUseActor) -> Vec<VibexUseToolDefinition> {
        let Ok(capability) = self.capability(actor) else {
            // Without a readable caller there is no honest catalogue to offer.
            return Vec::new();
        };
        vibex_core::vibex_use_tool_definitions(&capability)
    }
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn internal_encode_error(error: serde_json::Error) -> VibexError {
    VibexError::process(
        "vibex_use_encode_failed",
        "failed to encode a Vibex-use response",
    )
    .with_diagnostic("error", error.to_string())
}

/// The reference of one catalogue entry.
///
/// It is derived from the typed selection rather than from the catalogue's
/// position, so the same target keeps the same reference across a refresh and a
/// stale reference fails loudly instead of silently selecting a neighbour.
pub fn runtime_option_ref(option: &SessionRuntimeOption) -> String {
    VibexUseRef::runtime_option(&selection_fingerprint(&option.selection)).as_uri()
}

fn selection_fingerprint(selection: &SessionRuntimeSelection) -> String {
    fingerprint_of(&[
        selection.agent_id.as_str(),
        &serde_json::to_string(&selection.auth_source).unwrap_or_default(),
        selection.model_id().unwrap_or("agent-default"),
        selection.reasoning_effort.as_deref().unwrap_or_default(),
        selection.mode_id.as_deref().unwrap_or_default(),
    ])
}

fn runtime_option(
    option: &SessionRuntimeOption,
    unavailable: Option<VibexUseUnavailableReason>,
) -> VibexUseRuntimeOption {
    VibexUseRuntimeOption {
        reference: VibexUseRef::parse(&runtime_option_ref(option))
            .unwrap_or_else(|| VibexUseRef::runtime_option("unknown")),
        agent_id: option.selection.agent_id.clone(),
        label: option.agent_label.clone(),
        configuration_label: if option.model_label.is_empty() {
            option.auth_source_label.clone()
        } else {
            format!("{} · {}", option.auth_source_label, option.model_label)
        },
        provider_profile_id: option.selection.provider_profile_id().cloned(),
        model_id: option.selection.model_id().map(ToString::to_string),
        reasoning_effort: option.selection.reasoning_effort.clone(),
        mode_id: option.selection.mode_id.clone(),
        config_values: option.selection.config_values.clone(),
        unavailable_reason: unavailable,
        selection: option.selection.clone(),
    }
}

fn runtime_option_unavailable(option: &SessionRuntimeOption) -> Option<VibexUseUnavailableReason> {
    match option.availability {
        vibex_core::RuntimeOptionAvailability::Available => None,
        vibex_core::RuntimeOptionAvailability::RequiresConfiguration => {
            Some(VibexUseUnavailableReason::RequiresConfiguration)
        }
        vibex_core::RuntimeOptionAvailability::TemporarilyUnavailable => {
            Some(VibexUseUnavailableReason::ModelUnavailable)
        }
    }
}

fn group_summary(
    record: &GroupPresentationRecord,
    capability: &VibexUsePresentationCapability,
) -> VibexUseGroupSummary {
    VibexUseGroupSummary {
        group_ref: VibexUseRef::group(&record.group_id),
        name: record.name.clone(),
        revision: record.revision,
        member_session_refs: record
            .member_session_ids
            .iter()
            .map(VibexUseRef::session)
            .collect(),
        workspace_ref: VibexUseRef::workspace(&record.workspace_id),
        layout: record.layout.clone(),
        owned_by_caller: record.created_by_caller,
        presentable: capability.available && capability.supports_layout,
        presentation_reason: capability.reason.or_else(|| {
            (!capability.supports_layout).then_some(VibexUseUnavailableReason::NoPresentationClient)
        }),
        applied: matches!(
            record.state,
            GroupPresentationState::Applied | GroupPresentationState::Presented
        ),
        presented: record.state == GroupPresentationState::Presented,
        created_at_ms: record.created_at_ms,
        updated_at_ms: record.updated_at_ms,
    }
}

fn record_group_command(
    record: &GroupPresentationRecord,
    present: bool,
) -> GroupPresentationCommand {
    let group_id =
        SessionGroupId::parse(record.group_id.clone()).unwrap_or_else(|_| SessionGroupId::new());
    GroupPresentationCommand {
        group_id,
        operation_id: record.operation_id.clone(),
        name: record.name.clone(),
        workspace_ref: VibexUseRef::workspace(&record.workspace_id),
        member_session_refs: record
            .member_session_ids
            .iter()
            .map(VibexUseRef::session)
            .collect(),
        layout: record.layout.clone(),
        // The client compares this against the layout it is actually showing, so
        // a request written before the user rearranged the panes is deferred
        // instead of overwriting the new arrangement.
        expected_revision: Some(record.presentation_revision()),
        activation_policy: PresentationActivationPolicy::IfCurrentTeam,
        focus_session_ref: None,
        present,
        presentation_only: false,
    }
}

/// A command that only asks a client to show a group it already has.
///
/// Presenting must never rewrite the definition: a retry of "show me the team"
/// that re-sent the original members and layout would silently undo whatever
/// the user changed in between.
fn present_only_group_command(
    record: &GroupPresentationRecord,
    activation_policy: PresentationActivationPolicy,
    focus_session_ref: Option<VibexUseRef>,
) -> GroupPresentationCommand {
    let mut command = record_group_command(record, true);
    command.activation_policy = activation_policy;
    command.focus_session_ref = focus_session_ref;
    command.presentation_only = true;
    command
}

fn presentation_state_to_db(state: PresentationState) -> GroupPresentationState {
    match state {
        PresentationState::Created => GroupPresentationState::Created,
        PresentationState::Prepared => GroupPresentationState::Prepared,
        PresentationState::Applied => GroupPresentationState::Applied,
        PresentationState::Presented => GroupPresentationState::Presented,
        PresentationState::Deferred => GroupPresentationState::Deferred,
        PresentationState::Unavailable => GroupPresentationState::Unavailable,
    }
}

fn delegation_runtime_summary(selection: &SessionRuntimeSelection) -> DelegationRuntimeSummary {
    let (auth_kind, provider_profile_id) = match &selection.auth_source {
        vibex_core::RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } => ("provider_profile", Some(provider_profile_id.clone())),
        vibex_core::RuntimeAuthSource::AgentAccount { .. } => ("agent_account", None),
    };
    DelegationRuntimeSummary {
        agent_id: selection.agent_id.clone(),
        auth_kind: auth_kind.to_string(),
        configuration_label: selection
            .model_id()
            .or(selection.mode_id.as_deref())
            .map(ToString::to_string)
            .unwrap_or_else(|| "Agent default".to_string()),
        provider_profile_id,
        model_id: selection.model_id().map(ToString::to_string),
        reasoning_effort: selection.reasoning_effort.clone(),
        mode_id: selection.mode_id.clone(),
        config_values: selection.config_values.clone(),
        catalog_revision: 0,
    }
}

fn agent_session_state_name(state: AgentSessionState) -> &'static str {
    match state {
        AgentSessionState::Initializing => "initializing",
        AgentSessionState::Running => "running",
        AgentSessionState::NeedsInput => "needs_input",
        AgentSessionState::Idle => "idle",
        AgentSessionState::Error => "error",
        AgentSessionState::Closed => "closed",
        AgentSessionState::Archived => "archived",
    }
}

fn legacy_status_name(status: AgentDelegationStatus) -> &'static str {
    match status {
        AgentDelegationStatus::Queued => "queued",
        AgentDelegationStatus::Starting => "starting",
        AgentDelegationStatus::Running => "running",
        AgentDelegationStatus::NeedsInput => "needs_input",
        AgentDelegationStatus::Completed => "completed",
        AgentDelegationStatus::Failed => "failed",
        AgentDelegationStatus::Cancelled => "cancelled",
    }
}

fn workspace_mode_name(mode: vibex_core::WorkspaceMode) -> &'static str {
    match mode {
        vibex_core::WorkspaceMode::CurrentCheckout => "current_checkout",
        vibex_core::WorkspaceMode::VibexWorktree => "vibex_worktree",
    }
}

fn timeline_item_kind_name(kind: TimelineItemKind) -> &'static str {
    use TimelineItemKind as Kind;
    match kind {
        Kind::UserMessage => "user_message",
        Kind::AgentMessageDelta => "agent_message_delta",
        Kind::AgentMessage => "agent_message",
        Kind::Reasoning => "reasoning",
        Kind::Plan => "plan",
        Kind::Goal => "goal",
        Kind::ToolCall => "tool_call",
        Kind::Command => "command",
        Kind::FileOperation => "file_operation",
        Kind::WebSearch => "web_search",
        Kind::TodoUpdate => "todo_update",
        Kind::Collaboration => "collaboration",
        Kind::ImageGeneration => "image_generation",
        Kind::GitNotice => "git_notice",
        Kind::SystemNotice => "system_notice",
        Kind::PermissionRequest => "permission_request",
        Kind::PermissionResolution => "permission_resolution",
        Kind::ElicitationRequest => "elicitation_request",
        Kind::ElicitationResolution => "elicitation_resolution",
        Kind::Retry => "retry",
        Kind::Error => "error",
    }
}

fn timeline_source_name(source: vibex_core::TimelineSource) -> &'static str {
    match source {
        vibex_core::TimelineSource::User => "user",
        vibex_core::TimelineSource::Agent => "agent",
        vibex_core::TimelineSource::System => "system",
        vibex_core::TimelineSource::Provider => "provider",
    }
}

/// Which items one read view exposes.
///
/// `timeline` is the only view that carries raw process steps, `conversation`
/// stays at the level a colleague would read, and `summary` is conclusions and
/// blocking only.
fn view_includes(view: SessionReadView, item: &TimelineItem) -> bool {
    match view {
        SessionReadView::Timeline => !matches!(
            item.kind,
            TimelineItemKind::AgentMessageDelta | TimelineItemKind::Reasoning
        ),
        SessionReadView::Conversation => matches!(
            item.kind,
            TimelineItemKind::UserMessage
                | TimelineItemKind::AgentMessage
                | TimelineItemKind::Plan
                | TimelineItemKind::Goal
                | TimelineItemKind::Collaboration
                | TimelineItemKind::SystemNotice
                | TimelineItemKind::GitNotice
                | TimelineItemKind::Error
                | TimelineItemKind::Retry
                | TimelineItemKind::PermissionRequest
                | TimelineItemKind::ElicitationRequest
        ),
        SessionReadView::Summary => matches!(
            item.kind,
            TimelineItemKind::UserMessage
                | TimelineItemKind::AgentMessage
                | TimelineItemKind::Collaboration
                | TimelineItemKind::Error
                | TimelineItemKind::PermissionRequest
                | TimelineItemKind::ElicitationRequest
        ),
    }
}

/// The bounded, already-redacted text of one timeline item.
///
/// Provider logs, credentials and private authentication paths never appear
/// here: only what the product already renders in a timeline.
fn item_text(item: &TimelineItem) -> Option<String> {
    Some(match &item.payload {
        TimelinePayload::UserMessage(payload) => payload.text.clone(),
        TimelinePayload::AgentMessage(payload) => payload.text.clone(),
        TimelinePayload::Reasoning(payload) => payload.text.clone(),
        TimelinePayload::Plan(payload) => {
            let steps: Vec<String> = payload
                .steps
                .iter()
                .map(|step| format!("- [{}] {}", plan_step_name(step.status), step.title))
                .collect();
            format!("{}\n{}", payload.title, steps.join("\n"))
        }
        TimelinePayload::ToolCall(payload) => {
            format!("{} {}", payload.tool_name, payload.summary)
        }
        TimelinePayload::Command(payload) => payload.command.clone(),
        TimelinePayload::WebSearch(payload) => payload.query.clone(),
        TimelinePayload::TodoUpdate(payload) => {
            let items: Vec<String> = payload
                .items
                .iter()
                .map(|entry| format!("- [{}] {}", plan_step_name(entry.status), entry.title))
                .collect();
            format!("{}\n{}", payload.title, items.join("\n"))
        }
        TimelinePayload::Collaboration(payload) => payload.summary.clone(),
        TimelinePayload::GitNotice(payload) => payload.summary.clone(),
        TimelinePayload::SystemNotice(payload) => payload.message.clone(),
        TimelinePayload::PermissionRequest(payload) => {
            format!("{} ({:?})", payload.title, payload.risk_category)
        }
        TimelinePayload::PermissionResolution(payload) => {
            format!("{:?}", payload.response)
        }
        TimelinePayload::ElicitationRequest(payload) => payload.message.clone(),
        TimelinePayload::ElicitationResolution(payload) => format!("{:?}", payload.action),
        TimelinePayload::Retry(payload) => payload.reason.clone().unwrap_or_else(|| {
            format!(
                "retry attempt {}/{}",
                payload.attempt.unwrap_or(1),
                payload.max_attempts.unwrap_or(1)
            )
        }),
        TimelinePayload::Error(payload) => payload.message.clone(),
        TimelinePayload::Goal(payload) => payload
            .goal
            .as_ref()
            .map(|goal| goal.objective.clone())
            .or_else(|| payload.message.clone())
            .unwrap_or_else(|| "Goal updated".to_string()),
        TimelinePayload::FileOperation(payload) => payload.path.clone(),
        TimelinePayload::ImageGeneration(payload) => payload.summary.clone(),
        TimelinePayload::AgentMessageDelta(_) => return None,
    })
}

fn plan_step_name(status: vibex_core::PlanStepStatus) -> &'static str {
    match status {
        vibex_core::PlanStepStatus::Pending => "pending",
        vibex_core::PlanStepStatus::Running => "running",
        vibex_core::PlanStepStatus::Completed => "completed",
        vibex_core::PlanStepStatus::Failed => "failed",
    }
}

/// Parses exactly one page anchor.
///
/// The historical facade let `after` win when both cursors were supplied. A new
/// contract must not inherit that: two anchors is an ambiguous request and is
/// rejected rather than silently resolved.
fn parse_read_cursor(
    conn: &DbConnection,
    arguments: &serde_json::Value,
    session_ref: &VibexUseRef,
) -> VibexResult<SessionReadCursor> {
    // A cursor is bound to the session and view that produced it, so a caller
    // cannot carry one across sessions and quietly read the wrong range.
    if let Some(cursor) = arguments.get("cursor") {
        let parsed: SessionReadCursor = serde_json::from_value(cursor.clone())
            .map_err(|_| VibexError::validation(use_codes::REQUEST_INVALID, "cursor is invalid"))?;
        if &parsed.session_ref != session_ref {
            return Err(VibexError::validation(
                use_codes::CURSOR_SCOPE_MISMATCH,
                "that cursor belongs to another session",
            ));
        }
        let session_id = session_reference(session_ref)?;
        if SessionRepository::get(conn, &session_id)?.is_none() {
            return Err(VibexError::new(
                ErrorCategory::Permission,
                use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                "session not found",
            ));
        }
        return Ok(parsed);
    }
    let view = parse_read_view(arguments)?;
    let after = optional_i64(arguments, "after");
    let before = optional_i64(arguments, "before");
    let latest = optional_bool(arguments, "latest", false);
    let supplied =
        usize::from(after.is_some()) + usize::from(before.is_some()) + usize::from(latest);
    if supplied > 1 {
        return Err(VibexError::validation(
            use_codes::READ_ANCHOR_AMBIGUOUS,
            "exactly one of latest, after or before may be given",
        ));
    }
    let anchor = match (latest, after, before) {
        (true, _, _) => SessionReadAnchor::Latest,
        (_, Some(sequence), _) => SessionReadAnchor::After { sequence },
        (_, _, Some(sequence)) => SessionReadAnchor::Before { sequence },
        _ => SessionReadAnchor::Latest,
    };
    Ok(SessionReadCursor {
        session_ref: session_ref.clone(),
        view,
        anchor,
    })
}

fn parse_read_view(arguments: &serde_json::Value) -> VibexResult<SessionReadView> {
    Ok(match optional_string(arguments, "view", 32).as_deref() {
        None | Some("conversation") => SessionReadView::Conversation,
        Some("summary") => SessionReadView::Summary,
        Some("timeline") => SessionReadView::Timeline,
        Some(other) => {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("unknown read view {other}"),
            ));
        }
    })
}

fn parse_ref_list(
    arguments: &serde_json::Value,
    key: &str,
    kind: VibexUseResourceKind,
) -> VibexResult<Vec<VibexUseRef>> {
    let Some(entries) = arguments.get(key).and_then(serde_json::Value::as_array) else {
        return Ok(Vec::new());
    };
    if entries.len() > VIBEX_USE_MAX_BATCH_REFS {
        return Err(VibexError::validation(
            use_codes::REQUEST_INVALID,
            format!("at most {VIBEX_USE_MAX_BATCH_REFS} references are accepted"),
        ));
    }
    let mut refs = Vec::new();
    for entry in entries {
        let raw = entry.as_str().ok_or_else(|| {
            VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("{key} entries must be strings"),
            )
        })?;
        let reference = VibexUseRef::parse(raw).ok_or_else(|| {
            VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("{key} entry is invalid"),
            )
        })?;
        if reference.kind != kind {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("{key} entries must reference a {}", kind.as_str()),
            ));
        }
        refs.push(reference);
    }
    Ok(refs)
}

fn session_reference(reference: &VibexUseRef) -> VibexResult<VibexSessionId> {
    reference.session_id().ok_or_else(|| {
        VibexError::validation(use_codes::REQUEST_INVALID, "the reference is not a session")
    })
}

fn budget_error(capability: &VibexUseCapabilitySnapshot) -> VibexError {
    if capability.delegation_blocked_by.as_deref() == Some(use_codes::TASK_CANCELLING) {
        return VibexError::conflict(
            use_codes::TASK_CANCELLING,
            "a cancel is in progress for this task or one of its ancestors; no new work is accepted",
        );
    }
    if capability.remaining_depth == 0 {
        return VibexError::conflict(
            use_codes::DEPTH_EXCEEDED,
            "this caller may not delegate any deeper",
        )
        .with_diagnostic("maxDepth", capability.max_depth.to_string());
    }
    VibexError::conflict(
        use_codes::BUDGET_EXHAUSTED,
        "the root session has no execution slots left",
    )
    .with_diagnostic(
        "remainingExecutions",
        capability.remaining_executions.to_string(),
    )
}

fn parse_layout_intent(arguments: &serde_json::Value) -> VibexResult<SessionGroupLayoutIntent> {
    let Some(layout) = arguments.get("layout") else {
        return Ok(SessionGroupLayoutIntent::default());
    };
    let preset = match optional_string(layout, "preset", 32).as_deref() {
        None | Some("lead_and_workers") => SessionGroupLayoutPreset::LeadAndWorkers,
        Some("single") => SessionGroupLayoutPreset::Single,
        Some("columns") => SessionGroupLayoutPreset::Columns,
        Some("grid") => SessionGroupLayoutPreset::Grid,
        Some("tabs") => SessionGroupLayoutPreset::Tabs,
        Some(other) => {
            return Err(VibexError::validation(
                use_codes::REQUEST_INVALID,
                format!("unknown layout preset {other}"),
            ));
        }
    };
    let preferred_live_panes = optional_u64(layout, "preferredLivePanes").map(|value| {
        usize::try_from(value)
            .unwrap_or(1)
            .clamp(1, VIBEX_USE_MAX_LIVE_PANES)
    });
    Ok(SessionGroupLayoutIntent {
        preset,
        lead_session_ref: parse_optional_ref(
            layout,
            "leadSessionRef",
            VibexUseResourceKind::Session,
        )?,
        preferred_live_panes,
    })
}

fn optional_string(arguments: &serde_json::Value, key: &str, limit: usize) -> Option<String> {
    vibex_core::optional_string(arguments, key, limit)
}

fn required_string(arguments: &serde_json::Value, key: &str, limit: usize) -> VibexResult<String> {
    vibex_core::required_string(arguments, key, limit)
}

fn optional_bool(arguments: &serde_json::Value, key: &str, default: bool) -> bool {
    vibex_core::optional_bool(arguments, key, default)
}

fn optional_u64(arguments: &serde_json::Value, key: &str) -> Option<u64> {
    arguments.get(key).and_then(serde_json::Value::as_u64)
}

fn optional_i64(arguments: &serde_json::Value, key: &str) -> Option<i64> {
    arguments.get(key).and_then(serde_json::Value::as_i64)
}

fn bounded_usize(arguments: &serde_json::Value, key: &str, default: usize, max: usize) -> usize {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_u64)
        .map(|value| usize::try_from(value).unwrap_or(default).clamp(1, max))
        .unwrap_or(default)
}

fn cursor_offset(cursor: Option<&str>) -> usize {
    cursor
        .and_then(|cursor| cursor.strip_prefix("offset:"))
        .and_then(|value| value.parse::<usize>().ok())
        .unwrap_or(0)
}

fn string_array(
    arguments: &serde_json::Value,
    key: &str,
    max_items: usize,
    max_chars: usize,
) -> Vec<String> {
    arguments
        .get(key)
        .and_then(serde_json::Value::as_array)
        .map(|entries| {
            entries
                .iter()
                .filter_map(serde_json::Value::as_str)
                .map(|value| value.chars().take(max_chars).collect::<String>())
                .take(max_items)
                .collect()
        })
        .unwrap_or_default()
}

fn parse_optional_ref(
    arguments: &serde_json::Value,
    key: &str,
    kind: VibexUseResourceKind,
) -> VibexResult<Option<VibexUseRef>> {
    vibex_core::parse_optional_ref(arguments, key, kind)
}

fn parse_ref(
    arguments: &serde_json::Value,
    key: &str,
    kind: VibexUseResourceKind,
) -> VibexResult<VibexUseRef> {
    vibex_core::parse_ref(arguments, key, kind)
}

/// Fingerprint of an ordered list of intent-bearing strings.
pub fn fingerprint_of(parts: &[&str]) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    for part in parts {
        hasher.update(part.as_bytes());
        hasher.update([0]);
    }
    let digest = hasher.finalize();
    let mut output = String::with_capacity(digest.len() * 2);
    for byte in digest.iter().take(16) {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

/// Fingerprint of one tool call's whole intent.
///
/// Every argument the caller sent is part of the identity. An enumerated list
/// of "fields that matter" is how a later argument silently joins a request
/// without joining its fingerprint, so the whole payload is canonicalised
/// instead; a field added tomorrow is covered without anyone remembering to
/// add it here.
///
/// The idempotency key is the one exclusion: it is already the lookup key, so
/// echoing it into the fingerprint would say nothing new.
pub fn arguments_fingerprint(arguments: &serde_json::Value) -> String {
    fingerprint_of(&[&canonical_arguments(arguments)])
}

/// A deterministic, order-independent rendering of a JSON argument object.
fn canonical_arguments(value: &serde_json::Value) -> String {
    match value {
        serde_json::Value::Object(map) => {
            let mut keys: Vec<&String> = map
                .keys()
                .filter(|key| key.as_str() != "idempotencyKey")
                .collect();
            keys.sort();
            let mut rendered = String::from("{");
            for key in keys {
                rendered.push_str(&serde_json::to_string(key).unwrap_or_default());
                rendered.push(':');
                rendered.push_str(&canonical_arguments(&map[key]));
                rendered.push(',');
            }
            rendered.push('}');
            rendered
        }
        serde_json::Value::Array(items) => {
            let mut rendered = String::from("[");
            for item in items {
                rendered.push_str(&canonical_arguments(item));
                rendered.push(',');
            }
            rendered.push(']');
            rendered
        }
        other => serde_json::to_string(other).unwrap_or_default(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_idempotency_retry_covers_every_argument_that_changes_behaviour() {
        let base = serde_json::json!({
            "idempotencyKey": "review-1",
            "task": { "prompt": "review the storage layer", "completionPolicy": "owner_review" },
            "target": { "selectionRef": "vibex://runtime-option/a" }
        });
        let fingerprint = arguments_fingerprint(&base);
        // The key itself is the lookup key, so re-sending it alone is the same
        // request.
        let mut same = base.clone();
        same["idempotencyKey"] = serde_json::json!("review-1");
        assert_eq!(fingerprint, arguments_fingerprint(&same));
        // Every other field is intent: a change in any of them must not reuse
        // the first operation.
        let mut policy = base.clone();
        policy["task"]["completionPolicy"] = serde_json::json!("single_turn_legacy");
        assert_ne!(fingerprint, arguments_fingerprint(&policy));
        let mut criteria = base.clone();
        criteria["task"]["acceptanceCriteria"] = serde_json::json!(["list the failures"]);
        assert_ne!(fingerprint, arguments_fingerprint(&criteria));
        let mut selection = base.clone();
        selection["target"]["selectionRef"] = serde_json::json!("vibex://runtime-option/b");
        assert_ne!(fingerprint, arguments_fingerprint(&selection));
        let mut key_order_swapped = serde_json::json!({
            "target": { "selectionRef": "vibex://runtime-option/a" },
            "task": { "completionPolicy": "owner_review", "prompt": "review the storage layer" },
            "idempotencyKey": "another"
        });
        // Field order is not intent, but the key is: this one differs only by key.
        key_order_swapped["idempotencyKey"] = serde_json::json!("review-1");
        assert_eq!(fingerprint, arguments_fingerprint(&key_order_swapped));
    }

    #[test]
    fn a_fingerprint_separates_parts_and_is_stable() {
        let one = fingerprint_of(&["a", "b"]);
        assert_eq!(one, fingerprint_of(&["a", "b"]));
        // Concatenation without a separator would make these collide, which is
        // exactly the bug that would let a changed request reuse an operation.
        assert_ne!(one, fingerprint_of(&["ab"]));
        assert_ne!(one, fingerprint_of(&["a", "c"]));
    }

    #[test]
    fn the_same_selection_keeps_the_same_reference_across_a_catalog_refresh() {
        let selection = SessionRuntimeSelection::provider(
            AgentId::parse("codex").unwrap(),
            vibex_core::ProviderProfileId::parse("provider_local").unwrap(),
            "gpt-5",
        );
        let option = SessionRuntimeOption {
            selection: selection.clone(),
            agent_label: "Codex".to_string(),
            auth_source_label: "Local".to_string(),
            model_label: "gpt-5".to_string(),
            reasoning_efforts: Vec::new(),
            modes: Vec::new(),
            features: Vec::new(),
            availability: vibex_core::RuntimeOptionAvailability::Available,
        };
        let first = runtime_option_ref(&option);
        // The reference is derived from the typed selection, not from a
        // position, so a relabelled catalogue entry still resolves.
        let relabelled = SessionRuntimeOption {
            selection,
            agent_label: "Codex (renamed)".to_string(),
            model_label: "gpt-5 latest".to_string(),
            ..option
        };
        assert_eq!(first, runtime_option_ref(&relabelled));
        assert!(
            VibexUseRef::parse(&first)
                .is_some_and(|reference| { reference.kind == VibexUseResourceKind::RuntimeOption })
        );
    }

    fn temporary_connection(label: &str) -> (DbConnection, tempfile::TempDir) {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join(format!("{label}.db"));
        let mut conn = open_database(&path).unwrap();
        apply_migrations(&mut conn).unwrap();
        (conn, directory)
    }

    #[test]
    fn an_ambiguous_read_anchor_is_rejected_instead_of_resolved() {
        let (conn, _directory) = temporary_connection("read-anchor");
        let session_ref = VibexUseRef::session(&VibexSessionId::new());
        // The historical facade let `after` win when both cursors were
        // supplied. The new contract refuses to guess which one was meant.
        let error = parse_read_cursor(
            &conn,
            &serde_json::json!({ "latest": true, "after": 4 }),
            &session_ref,
        )
        .unwrap_err();
        assert_eq!(error.code, use_codes::READ_ANCHOR_AMBIGUOUS);

        let error = parse_read_cursor(
            &conn,
            &serde_json::json!({ "after": 4, "before": 9 }),
            &session_ref,
        )
        .unwrap_err();
        assert_eq!(error.code, use_codes::READ_ANCHOR_AMBIGUOUS);

        // A cursor minted for another session is refused instead of reading the
        // wrong range.
        let other = VibexUseRef::session(&VibexSessionId::new());
        let cursor = serde_json::to_value(SessionReadCursor {
            session_ref: other.clone(),
            view: SessionReadView::Conversation,
            anchor: SessionReadAnchor::Latest,
        })
        .unwrap();
        let error = parse_read_cursor(
            &conn,
            &serde_json::json!({ "cursor": cursor }),
            &session_ref,
        )
        .unwrap_err();
        assert_eq!(error.code, use_codes::CURSOR_SCOPE_MISMATCH);
    }

    #[test]
    fn read_views_expose_different_levels_of_detail() {
        use vibex_core::{
            AgentMessageDeltaPayload, PlanPayload, PlanStepPayload, PlanStepStatus, TimelineItem,
            TimelineItemId, TimelineItemKind, TimelinePayload, TimelineRedactionState,
            TimelineSource, UserMessagePayload,
        };
        let item = |kind: TimelineItemKind, payload: TimelinePayload| TimelineItem {
            id: TimelineItemId::new(),
            session_id: VibexSessionId::new(),
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Agent,
            kind,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        };
        let user = item(
            TimelineItemKind::UserMessage,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "Please review".to_string(),
                attachments: Vec::new(),
                delivery: UserMessageDelivery::Prompt,
                provenance: vibex_core::MessageProvenance::LegacyUnknown,
            }),
        );
        let delta = item(
            TimelineItemKind::AgentMessageDelta,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "partial".to_string(),
                chunk_index: 0,
                phase: None,
            }),
        );
        // A conversation read is what a colleague would read: no token deltas.
        assert!(view_includes(SessionReadView::Conversation, &user));
        assert!(!view_includes(SessionReadView::Conversation, &delta));
        assert!(view_includes(SessionReadView::Timeline, &user));
        assert!(!view_includes(SessionReadView::Timeline, &delta));

        let plan = item(
            TimelineItemKind::Plan,
            TimelinePayload::Plan(PlanPayload {
                title: "Plan".to_string(),
                steps: vec![PlanStepPayload {
                    title: "Check".to_string(),
                    status: PlanStepStatus::Pending,
                }],
            }),
        );
        let text = item_text(&plan).unwrap();
        assert!(text.contains("Plan"));
        assert!(text.contains("pending"));
    }

    #[test]
    fn an_item_without_text_reports_nothing_rather_than_an_empty_reply() {
        use vibex_core::{AgentMessageDeltaPayload, TimelineItemId, TimelineRedactionState};
        let item = TimelineItem {
            id: TimelineItemId::new(),
            session_id: VibexSessionId::new(),
            sequence: 1,
            timestamp_ms: 1,
            source: vibex_core::TimelineSource::Agent,
            kind: TimelineItemKind::AgentMessageDelta,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload: TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "x".to_string(),
                chunk_index: 0,
                phase: None,
            }),
        };
        assert!(item_text(&item).is_none());
    }
}

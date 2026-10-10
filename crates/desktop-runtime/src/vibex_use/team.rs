//! Human team capabilities. The gateway authenticates device permissions;
//! NativeBackend supplies the local human identity. Agent tools cannot enter
//! this boundary or manufacture a grant by including a URI in message text.

use super::*;
use async_trait::async_trait;
use rusqlite::{Connection, TransactionBehavior, params};
use vibex_core::VibexUseMentionKind;
use vibex_core::team::*;

const HUMAN_INBOX: &str = "human:team-inbox";

fn storage_error(_: rusqlite::Error) -> VibexError {
    VibexError::storage("team_storage_failed", "failed to read or update the team")
}

fn visible_session(conn: &Connection, id: &VibexSessionId) -> VibexResult<AgentSession> {
    SessionRepository::get(conn, id)?
        .filter(|session| session.deleted_at_ms.is_none())
        .ok_or_else(|| VibexError::validation("session_not_found", "session was not found"))
}

fn human_identity(identity: &str) -> VibexResult<()> {
    if !identity.starts_with("human:")
        || identity.len() > 256
        || identity.chars().any(char::is_control)
    {
        return Err(VibexError::new(
            ErrorCategory::Permission,
            "team_human_identity_required",
            "a trusted human identity is required",
        ));
    }
    Ok(())
}

fn page_limit(limit: Option<usize>) -> usize {
    limit.unwrap_or(40).clamp(1, TEAM_PAGE_LIMIT)
}

fn selected_delegation_context(request: &mut HumanDelegationRequest) -> VibexResult<()> {
    let mut runtime_refs = std::collections::BTreeSet::new();
    for mention in &request.mentions {
        match mention.kind {
            VibexUseMentionKind::Session => {
                let session_id = mention.reference.session_id().ok_or_else(|| {
                    VibexError::validation("team_mentions_invalid", "session mention is invalid")
                })?;
                if !request
                    .context_refs
                    .iter()
                    .any(|context| context.session_id == session_id)
                {
                    request.context_refs.push(TeamContextReference {
                        session_id,
                        from_sequence: None,
                        through_sequence: None,
                    });
                }
            }
            VibexUseMentionKind::Agent => {
                if mention.agent_id.is_none()
                    || mention.reference.kind != VibexUseResourceKind::RuntimeOption
                {
                    return Err(VibexError::validation(
                        "team_mentions_invalid",
                        "Agent mention is invalid",
                    ));
                }
                runtime_refs.insert(mention.reference.clone());
            }
        }
    }
    if request.runtime_option_ref.is_none() {
        if runtime_refs.len() > 1 {
            return Err(VibexError::validation(
                "team_target_ambiguous",
                "select one Agent configuration for this delegation",
            ));
        }
        request.runtime_option_ref = runtime_refs.into_iter().next();
    }
    if request
        .runtime_option_ref
        .as_ref()
        .is_some_and(|reference| reference.kind != VibexUseResourceKind::RuntimeOption)
    {
        return Err(VibexError::validation(
            "team_target_invalid",
            "selected target must be an Agent configuration",
        ));
    }
    Ok(())
}

fn cursor(value: &Option<String>) -> VibexResult<&str> {
    if let Some(value) = value {
        if value.len() > 256 || value.chars().any(char::is_control) {
            return Err(VibexError::validation(
                "team_cursor_invalid",
                "team cursor is invalid",
            ));
        }
        Ok(value)
    } else {
        Ok("")
    }
}

fn bounded_ids(
    conn: &Connection,
    sql: &str,
    parameters: impl rusqlite::Params,
) -> VibexResult<Vec<String>> {
    let mut statement = conn.prepare(sql).map_err(storage_error)?;
    statement
        .query_map(parameters, |row| row.get(0))
        .map_err(storage_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(storage_error)
}

impl VibexUseService {
    pub(super) fn human_referenceable_sessions(
        &self,
        query: &str,
        limit: usize,
    ) -> VibexResult<Vec<AgentSession>> {
        let conn = self.open()?;
        let ids = bounded_ids(
            &conn,
            "SELECT session_id FROM agent_sessions
             WHERE deleted_at_ms IS NULL AND archived_at_ms IS NULL
               AND (?1 = '' OR instr(lower(title), lower(?1)) > 0
                    OR instr(lower(session_id), lower(?1)) > 0
                    OR instr(lower(current_agent_id), lower(?1)) > 0)
             ORDER BY updated_at_ms DESC, session_id DESC LIMIT ?2",
            params![query.trim(), limit.min(TEAM_PAGE_LIMIT)],
        )?;
        let mut sessions = Vec::with_capacity(ids.len());
        for id in ids {
            if let Some(session) = SessionRepository::get(&conn, &VibexSessionId::parse(id)?)? {
                sessions.push(session);
            }
        }
        Ok(sessions)
    }

    fn human_actor(
        &self,
        conn: &Connection,
        session_id: &VibexSessionId,
    ) -> VibexResult<VibexUseActor> {
        visible_session(conn, session_id)?;
        Ok(VibexUseActor::new(
            &self.authority,
            session_id.clone(),
            self.activation_revision(),
        ))
    }

    pub async fn human_session_tree(
        &self,
        request: SessionTreeRequest,
    ) -> VibexResult<SessionTreePage> {
        let conn = self.open()?;
        self.human_session_tree_on(&conn, &request)
    }

    fn human_session_tree_on(
        &self,
        conn: &Connection,
        request: &SessionTreeRequest,
    ) -> VibexResult<SessionTreePage> {
        if let Some(parent) = &request.parent_session_id {
            visible_session(conn, parent)?;
        }
        let limit = page_limit(request.limit);
        let ids = bounded_ids(
            conn,
            "SELECT s.session_id FROM agent_sessions s
             LEFT JOIN session_ownership_edges edge ON edge.child_session_id = s.session_id
             WHERE s.deleted_at_ms IS NULL AND (?3 OR s.archived_at_ms IS NULL)
               AND ((?1 IS NULL AND edge.parent_session_id IS NULL) OR edge.parent_session_id = ?1)
               AND s.session_id > ?2 ORDER BY s.session_id LIMIT ?4",
            params![
                request
                    .parent_session_id
                    .as_ref()
                    .map(VibexSessionId::as_str),
                cursor(&request.cursor)?,
                request.include_archived,
                limit + 1
            ],
        )?;
        let has_more = ids.len() > limit;
        let mut page = SessionTreePage {
            has_more,
            ..Default::default()
        };
        for id in ids.into_iter().take(limit) {
            let id = VibexSessionId::parse(id)?;
            if let Some(mut node) = self.session_tree_node_on(conn, &id)? {
                node.has_more_children = node.child_count > 0;
                page.nodes.push(node);
                page.registry.push(visible_session(conn, &id)?);
            }
        }
        page.next_cursor = has_more
            .then(|| page.registry.last().map(|s| s.id.to_string()))
            .flatten();
        let mut ancestor = request.parent_session_id.clone();
        let mut seen = std::collections::BTreeSet::new();
        while let Some(id) = ancestor {
            if !seen.insert(id.clone()) || page.ancestors.len() == 16 {
                return Err(VibexError::validation(
                    "team_ownership_path_invalid",
                    "session ownership path is invalid",
                ));
            }
            if let Some(mut node) = self.session_tree_node_on(conn, &id)? {
                node.has_more_children = node.child_count > 0;
                page.ancestors.push(node);
                page.registry.push(visible_session(conn, &id)?);
            }
            ancestor = SessionOwnershipRepository::parent_of(conn, &id)?;
        }
        page.ancestors.reverse();
        Ok(page)
    }

    pub async fn human_team_snapshot(
        &self,
        request: TeamSnapshotRequest,
    ) -> VibexResult<TeamSnapshot> {
        let conn = self.open()?;
        let actor = self.human_actor(&conn, &request.session_id)?;
        let root = self.actor_root(&conn, &actor)?;
        visible_session(&conn, &root)?;
        let limit = page_limit(request.limit);
        let mut task_ids = bounded_ids(
            &conn,
            "SELECT delegation_id FROM agent_delegations
             WHERE COALESCE(root_session_id, parent_session_id) = ?1 AND delegation_id > ?2
             ORDER BY delegation_id LIMIT ?3",
            params![root.as_str(), cursor(&request.task_cursor)?, limit + 1],
        )?;
        let has_more_tasks = task_ids.len() > limit;
        task_ids.truncate(limit);
        let next_task_cursor = has_more_tasks.then(|| task_ids.last().cloned()).flatten();
        let mut tasks = Vec::with_capacity(task_ids.len());
        for id in task_ids {
            if let Some(task) =
                AgentDelegationRepository::get(&conn, &AgentDelegationId::parse(id)?)?
            {
                let mut view = self.task_view(&conn, &task)?;
                if let Some(session_id) = &task.child_session_id {
                    view.blocked_on = self
                        .pending_attention(&conn, session_id)
                        .or(view.blocked_on);
                }
                tasks.push(view);
            }
        }
        let mut execution_ids = bounded_ids(&conn,
            "WITH RECURSIVE owned(session_id) AS (
                SELECT ?1 UNION SELECT e.child_session_id FROM session_ownership_edges e JOIN owned o ON e.parent_session_id = o.session_id
             ) SELECT execution_id FROM vibex_use_executions x
             LEFT JOIN agent_delegations task ON task.delegation_id = x.task_id
             WHERE (COALESCE(task.root_session_id, task.parent_session_id) = ?1
                    OR (x.task_id IS NULL AND x.session_id IN (SELECT session_id FROM owned)))
               AND x.execution_id > ?2 ORDER BY x.execution_id LIMIT ?3",
            params![root.as_str(), cursor(&request.execution_cursor)?, limit + 1])?;
        let has_more_executions = execution_ids.len() > limit;
        execution_ids.truncate(limit);
        let next_execution_cursor = has_more_executions
            .then(|| execution_ids.last().cloned())
            .flatten();
        let mut executions = Vec::with_capacity(execution_ids.len());
        for id in execution_ids {
            if let Some(execution) =
                VibexUseExecutionRepository::get(&conn, &VibexExecutionId::parse(id)?)?
            {
                executions.push(execution);
            }
        }
        let after = request.after_event_cursor.unwrap_or(0);
        if after < 0 {
            return Err(VibexError::validation(
                "team_cursor_invalid",
                "event cursor must not be negative",
            ));
        }
        // The compact inbox pages pending events. Acknowledged history remains
        // in execution records and cannot crowd new results out of the inbox.
        let mut events = VibexUseEventRepository::unacknowledged_for_root(
            &conn,
            HUMAN_INBOX,
            &root,
            after,
            limit + 1,
            false,
        )?;
        let has_more = events.len() > limit;
        events.truncate(limit);
        let next_cursor = events.last().map(|event| event.cursor).unwrap_or(after);
        let capability = self.capability_on(&conn, &actor)?;
        let tree = self.human_session_tree_on(
            &conn,
            &SessionTreeRequest {
                parent_session_id: Some(root.clone()),
                limit: Some(limit),
                ..Default::default()
            },
        )?;
        let groups =
            GroupPresentationRepository::list_for_actor(&conn, root.as_str(), TEAM_PAGE_LIMIT)?
                .iter()
                .map(|record| group_summary(record, &capability.presentation))
                .collect();
        Ok(TeamSnapshot {
            root_session_id: root,
            tasks,
            executions,
            inbox: TeamInbox {
                events,
                next_cursor,
                has_more,
            },
            tree,
            groups,
            capability,
            next_task_cursor,
            has_more_tasks,
            next_execution_cursor,
            has_more_executions,
        })
    }

    pub async fn human_send_message_with_mentions(
        &self,
        mut request: HumanAgentMessageRequest,
        granted_by: String,
    ) -> VibexResult<Vec<TimelineItem>> {
        human_identity(&granted_by)?;
        visible_session(&self.open()?, &request.message.session_id)?;
        request.message.provenance = MessageProvenance::HumanInput;
        // The runtime constructs routing instructions from the typed selection.
        // Text, history and arbitrary client-supplied prompt context do not
        // become an authorization credential.
        request.message.prompt_context = vibex_core::vibex_use_route_note(&request.mentions);
        let coordinator = self.require_coordinator()?;
        let submission_id = coordinator.prepare_submission_with_mentions(
            request.message,
            request.mentions,
            &granted_by,
        )?;
        coordinator.wait_for_submission(&submission_id).await
    }

    pub async fn human_replace_user_message(
        &self,
        request: vibex_core::ReplaceUserMessagePayload,
        granted_by: String,
    ) -> VibexResult<Vec<TimelineItem>> {
        human_identity(&granted_by)?;
        self.require_coordinator()?
            .replace_user_message_with_mentions(
                vibex_agent::ReplaceUserMessageRequest {
                    user_sequence: request.user_sequence,
                    expected_end_sequence: request.expected_end_sequence,
                    message: request.message,
                },
                &granted_by,
            )
            .await
    }

    /// Uses the durable operation journal for the grant itself. The grant and
    /// its result commit together, so retrying an older Control cannot undo a
    /// more recent Revoke even after a process restart.
    pub async fn human_set_session_access(
        &self,
        request: SessionAccessRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<SessionAccessResult> {
        human_identity(&granted_by)?;
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        visible_session(&tx, &request.grantee_session_id)?;
        visible_session(&tx, &request.target_session_id)?;
        if request.grantee_session_id == request.target_session_id {
            return Err(VibexError::validation(
                "team_access_self",
                "a session's own access cannot be changed",
            ));
        }
        let value = serde_json::to_value(&request).map_err(internal_encode_error)?;
        let (operation, claimed) =
            self.human_operation(&tx, "human_set_session_access", &key, &granted_by, &value)?;
        if !claimed {
            return decode_result(&operation);
        }
        let old = SessionGrantRepository::get(
            &tx,
            &request.grantee_session_id,
            &request.target_session_id,
        )?;
        let mut changed = match request.access {
            SessionAccess::Revoke => SessionGrantRepository::revoke(
                &tx,
                &request.grantee_session_id,
                &request.target_session_id,
            )?,
            SessionAccess::Read | SessionAccess::Control => {
                let scope = if request.access == SessionAccess::Control {
                    "controlled"
                } else {
                    "referenced"
                };
                let changed = old.as_ref().is_none_or(|old| old.scope != scope);
                if changed {
                    SessionGrantRepository::grant(
                        &tx,
                        &request.grantee_session_id,
                        &request.target_session_id,
                        scope,
                        &granted_by,
                    )?;
                }
                changed
            }
        };
        if request.access == SessionAccess::Control {
            // A new human intent can hand control back even when the grant
            // already exists. Cached operation replay returned before this
            // point and cannot undo a later human takeover.
            changed |= SessionControllerRepository::return_to_agent(
                &tx,
                &request.target_session_id,
                &request.grantee_session_id,
            )?;
        }
        let result = SessionAccessResult {
            grantee_session_id: request.grantee_session_id,
            target_session_id: request.target_session_id,
            access: request.access,
            changed,
        };
        self.finish_human_operation(&tx, &operation, &result)?;
        tx.commit().map_err(storage_error)?;
        self.notify_progress();
        Ok(result)
    }

    fn human_operation(
        &self,
        conn: &Connection,
        tool: &str,
        key: &str,
        identity: &str,
        request: &serde_json::Value,
    ) -> VibexResult<(VibexUseOperation, bool)> {
        if key.trim().is_empty() || key.len() > 256 || key.chars().any(char::is_control) {
            return Err(VibexError::validation(
                "team_idempotency_invalid",
                "idempotency key must be non-empty and bounded",
            ));
        }
        let id = VibexOperationId::new();
        let operation = VibexUseOperation {
            operation_ref: VibexUseRef::operation(&id),
            id,
            authority: self.authority.clone(),
            actor_key: identity.to_string(),
            tool: tool.to_string(),
            caller_key: key.to_string(),
            payload_fingerprint: arguments_fingerprint(request),
            state: VibexUseOperationState::Accepted,
            error_code: None,
            error_message: None,
            retryable: false,
            resources: Vec::new(),
            checkpoint: BTreeMap::from([(
                "request".to_string(),
                serde_json::to_string(request).map_err(internal_encode_error)?,
            )]),
            created_at_ms: unix_timestamp_ms(),
            updated_at_ms: unix_timestamp_ms(),
        };
        match VibexUseOperationRepository::reserve(conn, &operation, key)? {
            VibexUseOperationReservation::Claimed(operation) => Ok((operation, true)),
            VibexUseOperationReservation::Existing(operation) => Ok((operation, false)),
            VibexUseOperationReservation::Conflict(_) => Err(VibexError::conflict(
                use_codes::IDEMPOTENCY_PAYLOAD_CONFLICT,
                "this request key was used with different content",
            )),
        }
    }

    fn finish_human_operation<T: serde::Serialize>(
        &self,
        conn: &Connection,
        operation: &VibexUseOperation,
        result: &T,
    ) -> VibexResult<()> {
        let value = serde_json::to_string(result).map_err(internal_encode_error)?;
        VibexUseOperationRepository::set_checkpoint(conn, &operation.id, "result", &value)?;
        VibexUseOperationRepository::update_state(
            conn,
            &operation.id,
            VibexUseOperationState::Succeeded,
            None,
            None,
            false,
        )?;
        Ok(())
    }

    fn selected_team_references(
        &self,
        request: &HumanDelegationRequest,
        key: &str,
        identity: &str,
    ) -> VibexResult<()> {
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        visible_session(&tx, &request.parent_session_id)?;
        if request.mentions.len() > 32 || request.context_refs.len() > VIBEX_USE_MAX_CONTEXT_REFS {
            return Err(VibexError::validation(
                "team_mentions_invalid",
                "too many selected references",
            ));
        }
        let value = serde_json::to_value(request).map_err(internal_encode_error)?;
        let (operation, claimed) =
            self.human_operation(&tx, "human_select_team_references", key, identity, &value)?;
        if !claimed {
            return Ok(());
        }
        let mut targets = std::collections::BTreeSet::new();
        for mention in &request.mentions {
            if mention.kind == VibexUseMentionKind::Session {
                targets.insert(mention.reference.session_id().ok_or_else(|| {
                    VibexError::validation("team_mentions_invalid", "session mention is invalid")
                })?);
            } else if mention.agent_id.is_none()
                || mention.reference.kind != VibexUseResourceKind::RuntimeOption
            {
                return Err(VibexError::validation(
                    "team_mentions_invalid",
                    "Agent mention is invalid",
                ));
            }
        }
        for context in &request.context_refs {
            if context.from_sequence.is_some_and(|from| from <= 0)
                || context.through_sequence.is_some_and(|through| through <= 0)
                || matches!((context.from_sequence, context.through_sequence), (Some(from), Some(through)) if from > through)
            {
                return Err(VibexError::validation(
                    "team_context_invalid",
                    "context range is invalid",
                ));
            }
            targets.insert(context.session_id.clone());
        }
        for target in targets {
            visible_session(&tx, &target)?;
            if target != request.parent_session_id
                && SessionGrantRepository::get(&tx, &request.parent_session_id, &target)?.is_none()
            {
                SessionGrantRepository::grant(
                    &tx,
                    &request.parent_session_id,
                    &target,
                    "referenced",
                    identity,
                )?;
            }
        }
        self.finish_human_operation(&tx, &operation, &true)?;
        tx.commit().map_err(storage_error)?;
        Ok(())
    }

    pub async fn human_delegate_session(
        &self,
        mut request: HumanDelegationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<HumanDelegationResult> {
        human_identity(&granted_by)?;
        if request.task.trim().is_empty() || request.task.chars().count() > 64 * 1024 {
            return Err(VibexError::validation(
                "team_task_invalid",
                "task text must be non-empty and bounded",
            ));
        }
        selected_delegation_context(&mut request)?;
        self.selected_team_references(&request, &key, &granted_by)?;
        let actor = self.human_actor(&self.open()?, &request.parent_session_id)?;
        let target = request
            .runtime_option_ref
            .as_ref()
            .map(|reference| serde_json::json!({"selectionRef": reference.as_uri()}))
            .unwrap_or_else(|| serde_json::json!({}));
        let session = request.existing_session_id.as_ref().map(|id| serde_json::json!({"kind": "existing", "sessionRef": VibexUseRef::session(id).as_uri()})).unwrap_or_else(|| serde_json::json!({"kind": "new"}));
        let context: Vec<_> = request.context_refs.iter().map(|reference| serde_json::json!({"sessionRef": VibexUseRef::session(&reference.session_id).as_uri(), "fromSequence": reference.from_sequence, "throughSequence": reference.through_sequence})).collect();
        let args = serde_json::json!({"idempotencyKey": human_key(&granted_by, &key), "target": target, "session": session,
            "task": {"prompt": request.task, "title": request.title, "acceptanceCriteria": request.acceptance_criteria, "completionPolicy": request.completion_policy},
            "attachments": request.attachments, "context": context, "mentions": request.mentions});
        let value = self.call(actor, VibexUseTool::Delegate, args).await?;
        let operation_ref = value
            .get("operationRef")
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| {
                VibexError::storage("team_result_invalid", "delegation result has no operation")
            })?;
        let operation_ref = VibexUseRef::parse(operation_ref).ok_or_else(|| {
            VibexError::storage("team_result_invalid", "delegation result is invalid")
        })?;
        let conn = self.open()?;
        let operation_id = operation_ref.operation_id().ok_or_else(|| {
            VibexError::storage("team_result_invalid", "delegation result is invalid")
        })?;
        let operation =
            VibexUseOperationRepository::get(&conn, &operation_id)?.ok_or_else(|| {
                VibexError::storage("team_result_invalid", "delegation operation was not found")
            })?;
        let task_id = operation
            .resources
            .iter()
            .find_map(|resource| resource.reference.task_id())
            .ok_or_else(|| {
                VibexError::storage(
                    "team_result_pending",
                    "delegation is still preparing its task",
                )
            })?;
        let task = AgentDelegationRepository::get(&conn, &task_id)?.ok_or_else(|| {
            VibexError::storage("team_result_invalid", "delegation task was not found")
        })?;
        Ok(HumanDelegationResult {
            task: self.task_view(&conn, &task)?,
            operation_ref,
            presentation: value
                .get("presentation")
                .filter(|v| !v.is_null())
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(internal_encode_error)?,
            warnings: value
                .get("warnings")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(internal_encode_error)?
                .unwrap_or_default(),
        })
    }

    pub async fn human_control_team_task(
        &self,
        request: TeamTaskControlRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<DelegationTaskView> {
        human_identity(&granted_by)?;
        let (actor, task) = {
            let conn = self.open()?;
            visible_session(&conn, &request.session_id)?;
            let task = AgentDelegationRepository::get(&conn, &request.task_id)?
                .ok_or_else(|| VibexError::validation("task_not_found", "task was not found"))?;
            if vibex_db::vibex_use_root_session(&conn, &request.session_id)?
                != task
                    .root_session_id
                    .as_ref()
                    .unwrap_or(&task.parent_session_id)
                    .clone()
            {
                return Err(VibexError::new(
                    ErrorCategory::Permission,
                    "team_task_scope_denied",
                    "task belongs to another team",
                ));
            }
            (self.human_actor(&conn, &task.parent_session_id)?, task)
        };
        let tool = match request.action {
            TeamTaskAction::Accept => VibexUseTool::FinishTask,
            TeamTaskAction::Cancel { .. } => VibexUseTool::CancelTask,
            TeamTaskAction::Interrupt => VibexUseTool::Interrupt,
        };
        let mut args = serde_json::json!({"idempotencyKey": human_key(&granted_by, &key), "taskRef": VibexUseRef::task(&task.id).as_uri(), "expectedRevision": request.expected_revision});
        match request.action {
            TeamTaskAction::Accept => {
                args["outcome"] = serde_json::json!("completed");
            }
            TeamTaskAction::Cancel { cascade } => {
                args["cascade"] = serde_json::json!(cascade);
            }
            TeamTaskAction::Interrupt => {
                args["sessionRef"] = serde_json::json!(
                    task.child_session_id
                        .as_ref()
                        .map(VibexUseRef::session)
                        .map(|r| r.as_uri())
                );
            }
        }
        self.call(actor, tool, args).await?;
        let conn = self.open()?;
        let task = AgentDelegationRepository::get(&conn, &request.task_id)?
            .ok_or_else(|| VibexError::validation("task_not_found", "task was not found"))?;
        self.task_view(&conn, &task)
    }

    pub async fn human_present_team(
        &self,
        request: TeamPresentationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<PresentationOutcome> {
        self.human_present_team_inner(request, key, granted_by, false)
            .await
    }

    async fn human_present_team_inner(
        &self,
        request: TeamPresentationRequest,
        key: String,
        granted_by: String,
        recovering: bool,
    ) -> VibexResult<PresentationOutcome> {
        human_identity(&granted_by)?;
        let actor = self.human_actor(&self.open()?, &request.session_id)?;
        let lock_args = serde_json::json!({"idempotencyKey": human_key(&granted_by, &key)});
        let _guard = self
            .lock_operation(&actor, VibexUseTool::PresentGroup, &lock_args)
            .await?;
        let (operation, record) = {
            let mut conn = self.open()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            let value = serde_json::to_value(&request).map_err(internal_encode_error)?;
            let (operation, claimed) =
                self.human_operation(&tx, "human_present_team", &key, &granted_by, &value)?;
            if !claimed && operation.state == VibexUseOperationState::Succeeded {
                return decode_result(&operation);
            }
            if operation.state.is_terminal() {
                return Err(VibexError::conflict(
                    operation
                        .error_code
                        .as_deref()
                        .unwrap_or("team_presentation_failed"),
                    operation
                        .error_message
                        .as_deref()
                        .unwrap_or("team presentation failed"),
                ));
            }
            let group_id = request.group_ref.group_id().ok_or_else(|| {
                VibexError::validation("team_group_invalid", "a group reference is required")
            })?;
            let record =
                GroupPresentationRepository::get(&tx, group_id.as_str())?.ok_or_else(|| {
                    VibexError::validation("team_group_not_found", "team group was not found")
                })?;
            let owner = VibexSessionId::parse(&record.actor_key)?;
            if vibex_db::vibex_use_root_session(&tx, &owner)? != self.actor_root(&tx, &actor)? {
                return Err(VibexError::new(
                    ErrorCategory::Permission,
                    "team_group_scope_denied",
                    "group belongs to another team",
                ));
            }
            if request
                .expected_revision
                .is_some_and(|revision| revision != record.revision)
            {
                return Err(group_revision_conflict());
            }
            if let Some(revision) = operation.checkpoint.get("presentation_revision") {
                let revision = revision.parse::<u64>().map_err(|_| {
                    VibexError::storage(
                        "team_presentation_checkpoint_invalid",
                        "saved team presentation revision is invalid",
                    )
                })?;
                if revision != record.revision {
                    return Err(group_revision_conflict());
                }
            } else {
                // Even a request without a client revision is fenced to the
                // definition accepted before the display client was awaited.
                VibexUseOperationRepository::set_checkpoint(
                    &tx,
                    &operation.id,
                    "presentation_revision",
                    &record.revision.to_string(),
                )?;
            }
            if request
                .focus_session_id
                .as_ref()
                .is_some_and(|id| !record.member_session_ids.contains(id))
            {
                return Err(VibexError::validation(
                    "team_focus_invalid",
                    "focused session must belong to the group",
                ));
            }
            tx.commit().map_err(storage_error)?;
            (operation, record)
        };
        // A human may show a group after taking ownership of its layout. The
        // saved definition and client revision still fence the presentation;
        // this boundary never transfers group ownership back to an Agent.
        let command = present_only_group_command(
            &record,
            if recovering {
                PresentationActivationPolicy::WhenUserReturns
            } else {
                request.activation_policy
            },
            request.focus_session_id.as_ref().map(VibexUseRef::session),
        );
        let outcome = self.present_record(command).await;
        let outcome = self.record_presentation_outcome(&record, &outcome)?;
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        self.finish_human_operation(&tx, &operation, &outcome)?;
        tx.commit().map_err(storage_error)?;
        Ok(outcome)
    }

    /// Resumes the saved human intent without reclaiming ownership or applying
    /// a presentation after a newer layout edit. Atomic grant operations never
    /// leave pending work and do not need a separate replay path.
    pub(super) async fn recover_human_presentation(
        &self,
        operation: &VibexUseOperation,
    ) -> VibexResult<()> {
        let result = async {
            if operation.authority != self.authority {
                return Err(VibexError::new(
                    ErrorCategory::Permission,
                    use_codes::NOT_FOUND_OR_NOT_AUTHORIZED,
                    "the saved presentation belongs to another authority",
                ));
            }
            let request = operation.checkpoint.get("request").ok_or_else(|| {
                VibexError::storage(
                    "team_presentation_checkpoint_missing",
                    "saved team presentation request is missing",
                )
            })?;
            let request = serde_json::from_str(request).map_err(internal_encode_error)?;
            self.human_present_team_inner(
                request,
                operation.caller_key.clone(),
                operation.actor_key.clone(),
                true,
            )
            .await
        }
        .await;
        if let Err(error) = &result {
            let mut conn = self.open()?;
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(storage_error)?;
            if let Some(current) = VibexUseOperationRepository::get(&tx, &operation.id)?
                && !current.state.is_terminal()
            {
                self.settle_operation(&tx, &current, VibexUseOperationState::Failed, Some(error))?;
            }
            tx.commit().map_err(storage_error)?;
        }
        result.map(|_| ())
    }

    pub async fn human_acknowledge_team_events(
        &self,
        request: TeamAcknowledgeRequest,
        granted_by: String,
    ) -> VibexResult<usize> {
        human_identity(&granted_by)?;
        if request.event_ids.len() > TEAM_PAGE_LIMIT
            || request.event_ids.iter().any(|id| id.len() > 256)
        {
            return Err(VibexError::validation(
                "team_ack_invalid",
                "event acknowledgement exceeds its bound",
            ));
        }
        let mut conn = self.open()?;
        let tx = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(storage_error)?;
        visible_session(&tx, &request.session_id)?;
        let root = vibex_db::vibex_use_root_session(&tx, &request.session_id)?;
        for id in &request.event_ids {
            let event = VibexUseEventRepository::get(&tx, id)?.ok_or_else(|| {
                VibexError::validation("team_event_not_found", "team event was not found")
            })?;
            if event
                .root_session_ref
                .as_ref()
                .and_then(VibexUseRef::session_id)
                .as_ref()
                != Some(&root)
            {
                return Err(VibexError::new(
                    ErrorCategory::Permission,
                    "team_event_scope_denied",
                    "event belongs to another team",
                ));
            }
        }
        // An explicit human acknowledgement witnesses receipt of these exact
        // events. It never acknowledges the Agent's independent consumer.
        VibexUseEventRepository::mark_delivered(&tx, HUMAN_INBOX, &request.event_ids)?;
        let changed = VibexUseEventRepository::acknowledge(&tx, HUMAN_INBOX, &request.event_ids)?;
        tx.commit().map_err(storage_error)?;
        Ok(changed)
    }
}

fn human_key(identity: &str, key: &str) -> String {
    use sha2::Digest;
    format!(
        "human:{:x}",
        sha2::Sha256::digest(format!("{identity}\u{1f}{key}"))
    )
}

fn decode_result<T: serde::de::DeserializeOwned>(operation: &VibexUseOperation) -> VibexResult<T> {
    let result = operation.checkpoint.get("result").ok_or_else(|| {
        VibexError::storage("team_result_pending", "team mutation has not completed")
    })?;
    serde_json::from_str(result).map_err(internal_encode_error)
}

#[async_trait]
impl vibex_remote::RemoteTeamService for VibexUseService {
    async fn team_snapshot(&self, request: TeamSnapshotRequest) -> VibexResult<TeamSnapshot> {
        self.human_team_snapshot(request).await
    }
    async fn session_tree(&self, request: SessionTreeRequest) -> VibexResult<SessionTreePage> {
        self.human_session_tree(request).await
    }
    async fn send_message_with_mentions(
        &self,
        request: HumanAgentMessageRequest,
        granted_by: String,
    ) -> VibexResult<Vec<TimelineItem>> {
        self.human_send_message_with_mentions(request, granted_by)
            .await
    }
    async fn set_session_access(
        &self,
        request: SessionAccessRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<SessionAccessResult> {
        self.human_set_session_access(request, key, granted_by)
            .await
    }
    async fn delegate_session(
        &self,
        request: HumanDelegationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<HumanDelegationResult> {
        self.human_delegate_session(request, key, granted_by).await
    }
    async fn control_team_task(
        &self,
        request: TeamTaskControlRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<DelegationTaskView> {
        self.human_control_team_task(request, key, granted_by).await
    }
    async fn present_team(
        &self,
        request: TeamPresentationRequest,
        key: String,
        granted_by: String,
    ) -> VibexResult<PresentationOutcome> {
        self.human_present_team(request, key, granted_by).await
    }
    async fn acknowledge_team_events(
        &self,
        request: TeamAcknowledgeRequest,
        granted_by: String,
    ) -> VibexResult<usize> {
        self.human_acknowledge_team_events(request, granted_by)
            .await
    }
}

#[cfg(test)]
#[path = "team_tests.rs"]
mod tests;

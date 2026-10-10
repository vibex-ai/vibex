use super::*;

impl VibexUseService {
    pub(super) fn refresh_cancel_operation(
        &self,
        conn: &DbConnection,
        operation: VibexUseOperation,
    ) -> VibexResult<VibexUseOperation> {
        if operation.tool != VibexUseTool::CancelTask.name() || operation.state.is_terminal() {
            return Ok(operation);
        }
        let tasks = operation
            .resources
            .iter()
            .filter_map(|resource| resource.reference.task_id())
            .map(|id| AgentDelegationRepository::get(conn, &id))
            .collect::<VibexResult<Vec<_>>>()?;
        let executions = operation
            .resources
            .iter()
            .filter_map(|resource| resource.reference.execution_id())
            .map(|id| VibexUseExecutionRepository::get(conn, &id))
            .collect::<VibexResult<Vec<_>>>()?;
        if !tasks.is_empty()
            && tasks
                .iter()
                .all(|task| task.as_ref().is_none_or(|task| task.phase().is_terminal()))
            && executions.iter().all(|execution| {
                execution.as_ref().is_none_or(|execution| {
                    execution.is_settled() && execution.outcome != ExecutionOutcome::Ambiguous
                })
            })
        {
            self.settle_operation(conn, &operation, VibexUseOperationState::Succeeded, None)?;
            return Ok(VibexUseOperationRepository::get(conn, &operation.id)?.unwrap_or(operation));
        }
        Ok(operation)
    }

    pub(super) async fn operation_selection(
        &self,
        operation: &VibexUseOperation,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<SessionRuntimeSelection> {
        if let Some(value) = VibexUseOperationRepository::get(&self.open()?, &operation.id)?
            .and_then(|op| op.checkpoint.get("selection").cloned())
        {
            return serde_json::from_str(&value).map_err(internal_encode_error);
        }
        let selection = self.resolve_selection(actor, arguments).await?;
        let saved = VibexUseOperationRepository::set_checkpoint_once(
            &self.open()?,
            &operation.id,
            "selection",
            &serde_json::to_string(&selection).map_err(internal_encode_error)?,
        )?;
        serde_json::from_str(&saved.checkpoint["selection"]).map_err(internal_encode_error)
    }

    pub(super) fn operation_context(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
        actor: &VibexUseActor,
        arguments: &serde_json::Value,
    ) -> VibexResult<Vec<DelegationContextRef>> {
        let refs: Vec<DelegationContextRef> = if let Some(value) =
            VibexUseOperationRepository::get(conn, &operation.id)?
                .and_then(|op| op.checkpoint.get("context").cloned())
        {
            serde_json::from_str(&value).map_err(internal_encode_error)?
        } else {
            let refs = self.resolve_context(conn, actor, arguments)?;
            let saved = VibexUseOperationRepository::set_checkpoint_once(
                conn,
                &operation.id,
                "context",
                &serde_json::to_string(&refs).map_err(internal_encode_error)?,
            )?;
            serde_json::from_str(&saved.checkpoint["context"]).map_err(internal_encode_error)?
        };
        for reference in &refs {
            self.require_readable(conn, actor, &session_reference(&reference.session_ref)?)?;
        }
        Ok(refs)
    }

    pub(super) fn operation_prompt_context(
        &self,
        conn: &DbConnection,
        operation: &VibexUseOperation,
        refs: &[DelegationContextRef],
    ) -> VibexResult<Option<String>> {
        if let Some(value) = VibexUseOperationRepository::get(conn, &operation.id)?
            .and_then(|op| op.checkpoint.get("prompt_context").cloned())
        {
            return serde_json::from_str(&value).map_err(internal_encode_error);
        }
        let context = self.render_context_block(conn, refs)?;
        let saved = VibexUseOperationRepository::set_checkpoint_once(
            conn,
            &operation.id,
            "prompt_context",
            &serde_json::to_string(&context).map_err(internal_encode_error)?,
        )?;
        serde_json::from_str(&saved.checkpoint["prompt_context"]).map_err(internal_encode_error)
    }

    pub(super) async fn lock_operation(
        &self,
        actor: &VibexUseActor,
        tool: VibexUseTool,
        arguments: &serde_json::Value,
    ) -> VibexResult<Option<tokio::sync::OwnedMutexGuard<()>>> {
        let Some(key) = arguments
            .get("idempotencyKey")
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(None);
        };
        let key = format!("{}\u{1f}{}\u{1f}{}", actor.key(), tool.name(), key);
        let gate = {
            let mut gates = self.operation_gates.lock().map_err(|_| {
                VibexError::process(
                    "vibex_use_operation_lock_failed",
                    "operation registry is unavailable",
                )
            })?;
            gates.retain(|_, gate| gate.strong_count() > 0);
            if let Some(gate) = gates.get(&key).and_then(std::sync::Weak::upgrade) {
                gate
            } else {
                let gate = Arc::new(tokio::sync::Mutex::new(()));
                gates.insert(key, Arc::downgrade(&gate));
                gate
            }
        };
        Ok(Some(gate.lock_owned().await))
    }

    pub(super) fn record_operation_response(
        &self,
        actor: &VibexUseActor,
        tool: VibexUseTool,
        arguments: &serde_json::Value,
        result: &VibexResult<serde_json::Value>,
    ) -> VibexResult<()> {
        let Some(key) = arguments
            .get("idempotencyKey")
            .and_then(serde_json::Value::as_str)
        else {
            return Ok(());
        };
        let conn = self.open()?;
        let Some(operation) =
            VibexUseOperationRepository::get_by_key(&conn, &actor.key(), tool.name(), key)?
        else {
            return Ok(());
        };
        if operation.payload_fingerprint != arguments_fingerprint(arguments) {
            return Ok(());
        }
        match result {
            Ok(value) => {
                VibexUseOperationRepository::set_checkpoint(
                    &conn,
                    &operation.id,
                    "response",
                    &serde_json::to_string(value).map_err(internal_encode_error)?,
                )?;
            }
            Err(error) if !operation.state.is_terminal() => {
                self.settle_operation(
                    &conn,
                    &operation,
                    VibexUseOperationState::Failed,
                    Some(error),
                )?;
            }
            _ => {}
        }
        Ok(())
    }

    pub(super) fn operation_session_id(
        &self,
        operation: &VibexUseOperation,
    ) -> VibexResult<VibexSessionId> {
        let conn = self.open()?;
        let current = VibexUseOperationRepository::get(&conn, &operation.id)?
            .unwrap_or_else(|| operation.clone());
        if let Some(id) = current.checkpoint.get("session_id") {
            return VibexSessionId::parse(id);
        }
        let id = VibexSessionId::new();
        let saved = VibexUseOperationRepository::set_checkpoint_once(
            &conn,
            &operation.id,
            "session_id",
            id.as_str(),
        )?;
        VibexSessionId::parse(&saved.checkpoint["session_id"])
    }

    /// Replays durable workflow checkpoints; the persisted identities and
    /// submission keys prevent repeating any already accepted side effect.
    pub async fn recover_operations(&self) -> VibexResult<usize> {
        let operations = VibexUseOperationRepository::list_pending(&self.open()?)?;
        let mut resumed = 0;
        for operation in operations {
            if operation.tool == "human_present_team" {
                if self.recover_human_presentation(&operation).await.is_ok() {
                    resumed += 1;
                }
                continue;
            }
            let Some(request) = operation.checkpoint.get("request") else {
                continue;
            };
            let Some(tool) = VibexUseTool::parse(&operation.tool) else {
                continue;
            };
            let actor = VibexUseActor::new(
                operation.authority.clone(),
                VibexSessionId::parse(&operation.actor_key)?,
                self.activation_revision(),
            );
            let arguments: serde_json::Value =
                serde_json::from_str(request).map_err(internal_encode_error)?;
            let result = self.call(actor, tool, arguments).await;
            if result.is_ok() {
                resumed += 1;
            }
        }
        Ok(resumed)
    }
}

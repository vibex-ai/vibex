use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use tokio::sync::broadcast;
use vibex_agent::AgentManager;
use vibex_agent_acp::{AcpRuntimeClient, AcpTerminalExitStatus, AcpTerminalHost};
use vibex_core::{
    AgentAuthContext, AgentAuthContextAuthenticateRequest, AgentAuthContextAuthenticateResult,
    AgentAuthContextCancelAuthenticationRequest, AgentAuthContextId, AgentAuthContextLogoutPreview,
    AgentAuthContextLogoutRequest, AgentAuthContextMutationResult,
    AgentAuthContextRefreshModelsRequest, AgentAuthContextStatus, AgentAuthContextVerifyRequest,
    AgentAuthExecutionLocation, AgentAuthMethodEffect, AgentAuthMethodKind,
    AgentAuthModelCatalogSnapshot, AgentAuthenticateRequest, AgentAuthenticationCancelRequest,
    AgentAuthenticationCompleteRequest, AgentAuthenticationOperation,
    AgentAuthenticationOperationState, AgentLogoutRequest, RuntimeAuthSource,
    TerminalAuthActionDescriptor, VibexError, VibexResult, VibexSessionId, unix_timestamp_ms,
};
use vibex_db::{
    AgentAuthContextRepository, AgentAuthModelCatalogRepository,
    AgentAuthenticationOperationRepository, AgentSessionRuntimeRepository,
    RuntimeBindingRepository, apply_migrations, open_database,
};
use vibex_remote::RemoteAgentAuthContextSource;

use crate::AgentAuthCatalogService;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct AgentAuthContextChanged {
    pub agent_id: vibex_core::AgentId,
    pub auth_context_id: AgentAuthContextId,
}

#[derive(Clone)]
pub struct AgentAuthContextService {
    db_path: PathBuf,
    manager: Arc<AgentManager>,
    acp_runtime: Arc<AcpRuntimeClient>,
    terminal_host: Arc<dyn AcpTerminalHost>,
    auth_catalog: Arc<AgentAuthCatalogService>,
    operation_locks: Arc<Mutex<BTreeMap<AgentAuthContextId, Arc<tokio::sync::Mutex<()>>>>>,
    changes: broadcast::Sender<AgentAuthContextChanged>,
}

impl AgentAuthContextService {
    pub fn new(
        db_path: PathBuf,
        manager: Arc<AgentManager>,
        acp_runtime: Arc<AcpRuntimeClient>,
        terminal_host: Arc<dyn AcpTerminalHost>,
        auth_catalog: Arc<AgentAuthCatalogService>,
    ) -> VibexResult<Self> {
        let mut conn = open_database(&db_path)?;
        apply_migrations(&mut conn)?;
        AgentAuthenticationOperationRepository::cancel_incomplete_on_startup(&conn)?;
        let (changes, _) = broadcast::channel(64);
        Ok(Self {
            db_path,
            manager,
            acp_runtime,
            terminal_host,
            auth_catalog,
            operation_locks: Arc::new(Mutex::new(BTreeMap::new())),
            changes,
        })
    }

    pub fn ensure_default(&self, agent_id: &vibex_core::AgentId) -> VibexResult<AgentAuthContext> {
        self.acp_runtime.validate_agent_account(agent_id)?;
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        let existed = AgentAuthContextRepository::get_by_agent(&conn, agent_id)?.is_some();
        let context = AgentAuthContextRepository::ensure_default(&conn, agent_id)?;
        if !existed {
            self.notify_changed(&context);
        }
        Ok(context)
    }

    pub fn supports_agent_account(&self, agent_id: &vibex_core::AgentId) -> bool {
        self.acp_runtime.supports_agent_account(agent_id)
    }

    pub(crate) fn subscribe_changes(&self) -> broadcast::Receiver<AgentAuthContextChanged> {
        self.changes.subscribe()
    }

    pub fn get(&self, auth_context_id: &AgentAuthContextId) -> VibexResult<AgentAuthContext> {
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        AgentAuthContextRepository::get_by_id(&conn, auth_context_id)?.ok_or_else(|| {
            VibexError::validation(
                "agent_auth_context_not_found",
                "Agent authentication context was not found",
            )
        })
    }

    pub fn list(&self) -> VibexResult<Vec<AgentAuthContext>> {
        let mut conn = open_database(&self.db_path)?;
        apply_migrations(&mut conn)?;
        AgentAuthContextRepository::list(&conn)
    }

    /// Refreshes authenticated account catalogs created before model-scoped
    /// runtime controls were persisted. Current snapshots remain process-free.
    pub(crate) async fn refresh_incomplete_model_catalogs(&self) -> VibexResult<usize> {
        let contexts = self.list()?;
        let conn = open_database(&self.db_path)?;
        let snapshots = AgentAuthModelCatalogRepository::list_current(&conn, &contexts)?;
        drop(conn);
        let mut current_by_context = BTreeMap::new();
        for snapshot in snapshots {
            let replace = current_by_context
                .get(&snapshot.auth_context_id)
                .is_none_or(|current: &AgentAuthModelCatalogSnapshot| {
                    snapshot.last_attempt_at_ms > current.last_attempt_at_ms
                });
            if replace {
                current_by_context.insert(snapshot.auth_context_id.clone(), snapshot);
            }
        }
        let mut refreshed = 0;
        for context in contexts.into_iter().filter(|context| {
            context.status == AgentAuthContextStatus::Authenticated
                && current_by_context
                    .get(&context.id)
                    .is_none_or(|snapshot| !snapshot.runtime_options_complete)
        }) {
            let operation_lock = self.operation_lock(&context.id)?;
            let _guard = operation_lock.lock().await;
            let current = self.require_revision(&context.id, context.revision)?;
            if current.status != AgentAuthContextStatus::Authenticated {
                continue;
            }
            let snapshot = self
                .acp_runtime
                .discover_agent_auth_model_catalog(&current)
                .await?;
            let conn = open_database(&self.db_path)?;
            AgentAuthModelCatalogRepository::upsert(&conn, &snapshot)?;
            self.notify_changed(&current);
            refreshed += 1;
        }
        Ok(refreshed)
    }

    pub fn authentication_operation(
        &self,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
    ) -> VibexResult<AgentAuthenticationOperation> {
        self.operation(operation_id)
    }

    pub async fn list_auth_methods(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> VibexResult<vibex_core::AgentAuthCatalog> {
        self.auth_catalog.list(agent_id, None).await
    }

    pub async fn authenticate(
        &self,
        request: AgentAuthContextAuthenticateRequest,
    ) -> VibexResult<AgentAuthContextAuthenticateResult> {
        let operation_lock = self.operation_lock(&request.auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context =
            self.require_revision(&request.auth_context_id, request.expected_context_revision)?;
        let now = unix_timestamp_ms();
        let operation = AgentAuthenticationOperation {
            operation_id: request.operation_id.clone(),
            auth_context_id: context.id.clone(),
            expected_context_revision: context.revision,
            method_id: request.method_id.clone(),
            state: AgentAuthenticationOperationState::Queued,
            error_code: None,
            created_at_ms: now,
            updated_at_ms: now,
        };
        {
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::insert(&conn, &operation)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &request.operation_id,
                AgentAuthenticationOperationState::Queued,
                AgentAuthenticationOperationState::DiscoveringMethods,
                None,
            )?;
        }
        let catalog = match self
            .auth_catalog
            .refresh(context.agent_id.clone(), None)
            .await
        {
            Ok(catalog) => catalog,
            Err(error) => {
                self.fail_operation(
                    &request.operation_id,
                    AgentAuthenticationOperationState::DiscoveringMethods,
                    &error.code,
                )?;
                return Err(error);
            }
        };
        let method = catalog
            .methods
            .iter()
            .find(|method| method.id == request.method_id)
            .ok_or_else(|| {
                VibexError::validation(
                    "agent_auth_method_not_advertised",
                    "Authentication method is no longer advertised by the Agent",
                )
            });
        let method = match method {
            Ok(method) if method.effect != AgentAuthMethodEffect::RequiresProviderProfile => method,
            Ok(_) => {
                let error = VibexError::validation(
                    "agent_auth_method_requires_provider_profile",
                    "This authentication method belongs to a Provider configuration",
                );
                self.fail_operation(
                    &request.operation_id,
                    AgentAuthenticationOperationState::DiscoveringMethods,
                    &error.code,
                )?;
                return Err(error);
            }
            Err(error) => {
                self.fail_operation(
                    &request.operation_id,
                    AgentAuthenticationOperationState::DiscoveringMethods,
                    &error.code,
                )?;
                return Err(error);
            }
        };
        let execution_location = match method.kind {
            AgentAuthMethodKind::Terminal => AgentAuthExecutionLocation::RemoteAttachableTerminal,
            AgentAuthMethodKind::Agent => AgentAuthExecutionLocation::HostBrowser,
            AgentAuthMethodKind::Environment => AgentAuthExecutionLocation::CompletedOnHost,
        };
        {
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &request.operation_id,
                AgentAuthenticationOperationState::DiscoveringMethods,
                AgentAuthenticationOperationState::Authenticating,
                None,
            )?;
        }
        let result = match self
            .manager
            .authenticate_agent(AgentAuthenticateRequest {
                operation_id: request.operation_id.clone(),
                agent_id: context.agent_id.clone(),
                provider_profile_id: None,
                method_id: request.method_id.clone(),
            })
            .await
        {
            Ok(result) => result,
            Err(error) => {
                self.fail_operation(
                    &request.operation_id,
                    AgentAuthenticationOperationState::Authenticating,
                    &error.code,
                )?;
                return Err(error);
            }
        };
        if result.terminal.is_some() {
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &request.operation_id,
                AgentAuthenticationOperationState::Authenticating,
                AgentAuthenticationOperationState::AwaitingUser,
                None,
            )?;
            let terminal = result
                .terminal
                .map(|terminal| safe_terminal_descriptor(terminal, &request.method_id));
            let terminal_id = terminal
                .as_ref()
                .and_then(|terminal| terminal.terminal_id.clone())
                .ok_or_else(|| {
                    VibexError::process(
                        "agent_terminal_auth_terminal_missing",
                        "Interactive authentication terminal was not created",
                    )
                });
            let terminal_id = match terminal_id {
                Ok(terminal_id) => terminal_id,
                Err(error) => {
                    self.fail_operation(
                        &request.operation_id,
                        AgentAuthenticationOperationState::AwaitingUser,
                        &error.code,
                    )?;
                    return Err(error);
                }
            };
            self.spawn_terminal_authentication_monitor(
                context.clone(),
                request.operation_id.clone(),
                terminal_id,
            );
            return Ok(AgentAuthContextAuthenticateResult {
                context,
                operation: self.operation(&request.operation_id)?,
                execution_location,
                terminal,
                model_catalog: None,
            });
        }

        {
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &request.operation_id,
                AgentAuthenticationOperationState::Authenticating,
                AgentAuthenticationOperationState::Verifying,
                None,
            )?;
        }
        let (context, model_catalog) = self
            .verify_after_credential_change(
                context,
                Some(request.method_id),
                true,
                Some((
                    request.operation_id.clone(),
                    AgentAuthenticationOperationState::Verifying,
                )),
            )
            .await?;
        Ok(AgentAuthContextAuthenticateResult {
            context,
            operation: self.operation(&request.operation_id)?,
            execution_location,
            terminal: None,
            model_catalog: Some(model_catalog),
        })
    }

    pub async fn cancel_authentication(
        &self,
        request: AgentAuthContextCancelAuthenticationRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        let operation_lock = self.operation_lock(&request.auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context =
            self.require_revision(&request.auth_context_id, request.expected_context_revision)?;
        let operation = self.operation(&request.operation_id)?;
        if operation.auth_context_id != context.id
            || operation.expected_context_revision != context.revision
            || !matches!(
                operation.state,
                AgentAuthenticationOperationState::DiscoveringMethods
                    | AgentAuthenticationOperationState::Authenticating
                    | AgentAuthenticationOperationState::AwaitingUser
                    | AgentAuthenticationOperationState::Verifying
            )
        {
            return Err(VibexError::conflict(
                "agent_authentication_operation_state_conflict",
                "Authentication operation can no longer be cancelled",
            ));
        }
        {
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &operation.operation_id,
                operation.state,
                AgentAuthenticationOperationState::Cancelling,
                None,
            )?;
        }
        let cancel_request = AgentAuthenticationCancelRequest {
            operation_id: operation.operation_id.clone(),
            agent_id: context.agent_id.clone(),
        };
        let mut cancelled = false;
        for attempt in 0..40 {
            if self
                .manager
                .cancel_agent_authentication(cancel_request.clone())
                .await?
            {
                cancelled = true;
                break;
            }
            if attempt < 39 {
                tokio::time::sleep(std::time::Duration::from_millis(50)).await;
            }
        }
        let conn = open_database(&self.db_path)?;
        AgentAuthenticationOperationRepository::update_state(
            &conn,
            &operation.operation_id,
            AgentAuthenticationOperationState::Cancelling,
            AgentAuthenticationOperationState::Cancelled,
            (!cancelled).then_some("authentication_process_already_finished"),
        )?;
        Ok(AgentAuthContextMutationResult {
            affected_session_ids: self.affected_sessions(&context.id)?,
            context,
            model_catalog: None,
        })
    }

    pub async fn verify(
        &self,
        request: AgentAuthContextVerifyRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        let operation_lock = self.operation_lock(&request.auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context =
            self.require_revision(&request.auth_context_id, request.expected_context_revision)?;
        let operation = request
            .operation_id
            .as_ref()
            .map(|operation_id| self.operation(operation_id))
            .transpose()?;
        if operation.is_none() {
            self.ensure_no_active_operation(&context.id)?;
        }
        if let Some(operation) = operation.as_ref() {
            if operation.auth_context_id != context.id
                || operation.expected_context_revision != context.revision
                || operation.state != AgentAuthenticationOperationState::AwaitingUser
            {
                return Err(VibexError::conflict(
                    "agent_authentication_operation_state_conflict",
                    "Authentication operation is not waiting for verification",
                ));
            }
            let conn = open_database(&self.db_path)?;
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &operation.operation_id,
                AgentAuthenticationOperationState::AwaitingUser,
                AgentAuthenticationOperationState::Verifying,
                None,
            )?;
            let _ = self
                .manager
                .complete_agent_authentication(AgentAuthenticationCompleteRequest {
                    operation_id: operation.operation_id.clone(),
                    agent_id: context.agent_id.clone(),
                })
                .await?;
        }
        let authenticated_via_method = operation
            .as_ref()
            .map(|operation| operation.method_id.clone());
        let increments_revision =
            operation.is_some() || context.status != AgentAuthContextStatus::Unverified;
        let operation_fence = operation.as_ref().map(|operation| {
            (
                operation.operation_id.clone(),
                AgentAuthenticationOperationState::Verifying,
            )
        });
        let (context, model_catalog) = self
            .verify_after_credential_change(
                context,
                authenticated_via_method,
                increments_revision,
                operation_fence,
            )
            .await?;
        let affected_session_ids = self.affected_sessions(&context.id)?;
        Ok(AgentAuthContextMutationResult {
            context,
            model_catalog: Some(model_catalog),
            affected_session_ids,
        })
    }

    pub async fn refresh_models(
        &self,
        request: AgentAuthContextRefreshModelsRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        let operation_lock = self.operation_lock(&request.auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context =
            self.require_revision(&request.auth_context_id, request.expected_context_revision)?;
        self.ensure_no_active_operation(&context.id)?;
        if context.status != AgentAuthContextStatus::Authenticated {
            return Err(VibexError::conflict(
                "agent_authentication_required",
                "Agent account must be authenticated before refreshing models",
            ));
        }
        let snapshot = match self
            .acp_runtime
            .discover_agent_auth_model_catalog(&context)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) if is_authentication_required(&error) => {
                let context = self
                    .mark_authentication_required(context, error.code.as_str())
                    .await?;
                return Err(VibexError::conflict(
                    "agent_authentication_required",
                    "Agent account authentication is no longer valid",
                )
                .with_diagnostic("contextRevision", context.revision.to_string()));
            }
            Err(error) => return Err(error),
        };
        let conn = open_database(&self.db_path)?;
        AgentAuthModelCatalogRepository::upsert(&conn, &snapshot)?;
        self.notify_changed(&context);
        let affected_session_ids = self.affected_sessions(&context.id)?;
        Ok(AgentAuthContextMutationResult {
            context,
            model_catalog: Some(snapshot),
            affected_session_ids,
        })
    }

    /// Invalidates the exact default-account revision that produced a
    /// structured authentication failure on a live turn. Replayed or late
    /// timeline events become no-ops once the binding or context has changed.
    pub async fn handle_timeline_authentication_required(
        &self,
        session_id: &VibexSessionId,
        error_code: &str,
    ) -> VibexResult<bool> {
        if !is_authentication_required_code(error_code) {
            return Ok(false);
        }
        let (auth_context_id, auth_source_revision) = {
            let conn = open_database(&self.db_path)?;
            let Some(runtime_state) =
                AgentSessionRuntimeRepository::get_runtime_state(&conn, session_id)?
            else {
                return Ok(false);
            };
            let Some(effective) = runtime_state.effective_runtime_selection else {
                return Ok(false);
            };
            let RuntimeAuthSource::AgentAccount { auth_context_id } = effective.auth_source else {
                return Ok(false);
            };
            let Some(binding_id) = runtime_state.current_binding_id else {
                return Ok(false);
            };
            let Some(binding) = RuntimeBindingRepository::get(&conn, &binding_id)? else {
                return Ok(false);
            };
            if binding.session_id != *session_id
                || binding.agent_id != effective.agent_id
                || binding.auth_source != RuntimeAuthSource::agent_account(auth_context_id.clone())
            {
                return Ok(false);
            }
            (auth_context_id, binding.auth_source_revision)
        };

        let operation_lock = self.operation_lock(&auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context = self.get(&auth_context_id)?;
        if context.status != AgentAuthContextStatus::Authenticated
            || context.revision != auth_source_revision
        {
            return Ok(false);
        }
        let conn = open_database(&self.db_path)?;
        if AgentAuthenticationOperationRepository::get_active_for_context(&conn, &auth_context_id)?
            .is_some()
        {
            return Ok(false);
        }
        drop(conn);

        self.mark_authentication_required(context, error_code)
            .await?;
        Ok(true)
    }

    pub fn logout_preview(
        &self,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<AgentAuthContextLogoutPreview> {
        let context = self.get(auth_context_id)?;
        Ok(AgentAuthContextLogoutPreview {
            affected_session_ids: self.affected_sessions(auth_context_id)?,
            context,
        })
    }

    pub async fn logout(
        &self,
        request: AgentAuthContextLogoutRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        let operation_lock = self.operation_lock(&request.auth_context_id)?;
        let _guard = operation_lock.lock().await;
        let context =
            self.require_revision(&request.auth_context_id, request.expected_context_revision)?;
        self.ensure_no_active_operation(&context.id)?;
        let affected_session_ids = self.affected_sessions(&context.id)?;
        if affected_session_ids.len() != request.confirmed_affected_session_count {
            return Err(VibexError::conflict(
                "agent_auth_context_in_use_changed",
                "Sessions using the Agent account changed; review logout impact again",
            ));
        }
        let auth_source = RuntimeAuthSource::agent_account(context.id.clone());
        shutdown_account_processes_before_logout(
            self.acp_runtime
                .shutdown_auth_source_processes(&auth_source),
            self.manager.logout_agent(AgentLogoutRequest {
                agent_id: context.agent_id.clone(),
                provider_profile_id: None,
            }),
        )
        .await?;
        let conn = open_database(&self.db_path)?;
        let context = AgentAuthContextRepository::compare_and_set(
            &conn,
            &context.id,
            context.revision,
            AgentAuthContextStatus::AuthenticationRequired,
            None,
            None,
            None,
            true,
        )?;
        self.notify_changed(&context);
        Ok(AgentAuthContextMutationResult {
            context,
            model_catalog: None,
            affected_session_ids,
        })
    }

    async fn verify_after_credential_change(
        &self,
        context: AgentAuthContext,
        authenticated_via_method: Option<String>,
        increment_revision: bool,
        operation: Option<(
            vibex_core::AgentAuthenticationOperationId,
            AgentAuthenticationOperationState,
        )>,
    ) -> VibexResult<(AgentAuthContext, AgentAuthModelCatalogSnapshot)> {
        let auth_source = RuntimeAuthSource::agent_account(context.id.clone());
        self.acp_runtime
            .shutdown_auth_source_processes(&auth_source)
            .await?;
        let conn = open_database(&self.db_path)?;
        let verifying = AgentAuthContextRepository::compare_and_set(
            &conn,
            &context.id,
            context.revision,
            AgentAuthContextStatus::Verifying,
            context.account_hint.as_deref(),
            authenticated_via_method.as_deref(),
            None,
            increment_revision,
        )?;
        self.notify_changed(&verifying);
        let snapshot = match self
            .acp_runtime
            .discover_agent_auth_model_catalog(&verifying)
            .await
        {
            Ok(snapshot) => snapshot,
            Err(error) => {
                let status = if is_authentication_required(&error) {
                    AgentAuthContextStatus::AuthenticationRequired
                } else {
                    AgentAuthContextStatus::Unavailable
                };
                let conn = open_database(&self.db_path)?;
                if let Ok(failed) = AgentAuthContextRepository::compare_and_set(
                    &conn,
                    &verifying.id,
                    verifying.revision,
                    status,
                    verifying.account_hint.as_deref(),
                    verifying.authenticated_via_method.as_deref(),
                    None,
                    !increment_revision,
                ) {
                    self.notify_changed(&failed);
                }
                if let Some((operation_id, expected_state)) = operation.as_ref() {
                    let _ = AgentAuthenticationOperationRepository::update_state(
                        &conn,
                        operation_id,
                        *expected_state,
                        AgentAuthenticationOperationState::Failed,
                        Some(error.code.as_str()),
                    );
                }
                return Err(error);
            }
        };
        let conn = open_database(&self.db_path)?;
        AgentAuthModelCatalogRepository::upsert(&conn, &snapshot)?;
        let authenticated = AgentAuthContextRepository::compare_and_set(
            &conn,
            &verifying.id,
            verifying.revision,
            AgentAuthContextStatus::Authenticated,
            verifying.account_hint.as_deref(),
            verifying.authenticated_via_method.as_deref(),
            Some(unix_timestamp_ms()),
            false,
        )?;
        self.notify_changed(&authenticated);
        if let Some((operation_id, expected_state)) = operation {
            AgentAuthenticationOperationRepository::update_state(
                &conn,
                &operation_id,
                expected_state,
                AgentAuthenticationOperationState::Succeeded,
                None,
            )?;
        }
        Ok((authenticated, snapshot))
    }

    async fn mark_authentication_required(
        &self,
        context: AgentAuthContext,
        _error_code: &str,
    ) -> VibexResult<AgentAuthContext> {
        let auth_source = RuntimeAuthSource::agent_account(context.id.clone());
        self.acp_runtime
            .shutdown_auth_source_processes(&auth_source)
            .await?;
        let conn = open_database(&self.db_path)?;
        let context = AgentAuthContextRepository::compare_and_set(
            &conn,
            &context.id,
            context.revision,
            AgentAuthContextStatus::AuthenticationRequired,
            context.account_hint.as_deref(),
            context.authenticated_via_method.as_deref(),
            None,
            true,
        )?;
        self.notify_changed(&context);
        Ok(context)
    }

    fn spawn_terminal_authentication_monitor(
        &self,
        context: AgentAuthContext,
        operation_id: vibex_core::AgentAuthenticationOperationId,
        terminal_id: vibex_core::TerminalId,
    ) {
        let service = self.clone();
        tokio::spawn(async move {
            let completion = service.terminal_host.wait_for_exit(&terminal_id).await;
            let result = match completion {
                Ok(status) if terminal_auth_succeeded(&status) => service
                    .verify(AgentAuthContextVerifyRequest {
                        auth_context_id: context.id,
                        expected_context_revision: context.revision,
                        operation_id: Some(operation_id),
                    })
                    .await
                    .map(|_| ()),
                Ok(_) => {
                    service
                        .fail_terminal_authentication(
                            &context,
                            &operation_id,
                            "agent_terminal_auth_failed",
                        )
                        .await
                }
                Err(_) => {
                    service
                        .fail_terminal_authentication(
                            &context,
                            &operation_id,
                            "agent_terminal_auth_monitor_failed",
                        )
                        .await
                }
            };
            if let Err(error) = result {
                tracing::warn!(
                    target: "vibex_desktop",
                    error_code = %error.code,
                    "Interactive Agent authentication finalization failed"
                );
            }
        });
    }

    async fn fail_terminal_authentication(
        &self,
        context: &AgentAuthContext,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
        error_code: &str,
    ) -> VibexResult<()> {
        let operation_lock = self.operation_lock(&context.id)?;
        let _guard = operation_lock.lock().await;
        let operation = self.operation(operation_id)?;
        if matches!(
            operation.state,
            AgentAuthenticationOperationState::Failed
                | AgentAuthenticationOperationState::Cancelled
                | AgentAuthenticationOperationState::Succeeded
        ) {
            return Ok(());
        }
        if operation.auth_context_id != context.id
            || operation.expected_context_revision != context.revision
            || operation.state != AgentAuthenticationOperationState::AwaitingUser
        {
            return Err(VibexError::conflict(
                "agent_authentication_operation_state_conflict",
                "Interactive authentication operation changed before it completed",
            ));
        }
        let _ = self
            .manager
            .complete_agent_authentication(AgentAuthenticationCompleteRequest {
                operation_id: operation.operation_id.clone(),
                agent_id: context.agent_id.clone(),
            })
            .await?;
        let conn = open_database(&self.db_path)?;
        AgentAuthenticationOperationRepository::update_state(
            &conn,
            &operation.operation_id,
            AgentAuthenticationOperationState::AwaitingUser,
            AgentAuthenticationOperationState::Failed,
            Some(error_code),
        )?;
        if let Ok(current) = self.get(&context.id) {
            self.notify_changed(&current);
        }
        Ok(())
    }

    pub async fn wait_for_authentication_operation(
        &self,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        const MAX_POLLS: usize = 2_400;
        for _ in 0..MAX_POLLS {
            let operation = self.operation(operation_id)?;
            match operation.state {
                AgentAuthenticationOperationState::Succeeded => {
                    let context = self.get(&operation.auth_context_id)?;
                    let conn = open_database(&self.db_path)?;
                    let model_catalog = AgentAuthModelCatalogRepository::list_current(
                        &conn,
                        std::slice::from_ref(&context),
                    )?
                    .into_iter()
                    .max_by_key(|snapshot| snapshot.last_attempt_at_ms);
                    return Ok(AgentAuthContextMutationResult {
                        affected_session_ids: self.affected_sessions(&context.id)?,
                        context,
                        model_catalog,
                    });
                }
                AgentAuthenticationOperationState::Failed => {
                    return Err(VibexError::conflict(
                        operation
                            .error_code
                            .as_deref()
                            .unwrap_or("agent_authentication_verification_failed"),
                        "Interactive Agent authentication failed",
                    ));
                }
                AgentAuthenticationOperationState::Cancelled => {
                    return Err(agent_authentication_cancelled_error());
                }
                _ => tokio::time::sleep(std::time::Duration::from_millis(50)).await,
            }
        }
        Err(VibexError::process(
            "agent_authentication_completion_timeout",
            "Interactive Agent authentication did not reach a final state",
        ))
    }

    fn require_revision(
        &self,
        auth_context_id: &AgentAuthContextId,
        expected_revision: i64,
    ) -> VibexResult<AgentAuthContext> {
        let context = self.get(auth_context_id)?;
        if context.revision != expected_revision {
            return Err(VibexError::conflict(
                "agent_auth_context_revision_conflict",
                "Agent authentication context changed concurrently",
            ));
        }
        Ok(context)
    }

    fn affected_sessions(
        &self,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<Vec<vibex_core::VibexSessionId>> {
        let conn = open_database(&self.db_path)?;
        AgentAuthContextRepository::referencing_session_ids(&conn, auth_context_id)
    }

    fn operation(
        &self,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
    ) -> VibexResult<AgentAuthenticationOperation> {
        let conn = open_database(&self.db_path)?;
        AgentAuthenticationOperationRepository::get(&conn, operation_id)?.ok_or_else(|| {
            VibexError::validation(
                "agent_authentication_operation_not_found",
                "Agent authentication operation was not found",
            )
        })
    }

    fn ensure_no_active_operation(&self, auth_context_id: &AgentAuthContextId) -> VibexResult<()> {
        let conn = open_database(&self.db_path)?;
        if AgentAuthenticationOperationRepository::get_active_for_context(&conn, auth_context_id)?
            .is_some()
        {
            return Err(VibexError::conflict(
                "agent_authentication_operation_in_progress",
                "Agent account authentication is already in progress",
            ));
        }
        Ok(())
    }

    fn fail_operation(
        &self,
        operation_id: &vibex_core::AgentAuthenticationOperationId,
        expected_state: AgentAuthenticationOperationState,
        error_code: &str,
    ) -> VibexResult<()> {
        let conn = open_database(&self.db_path)?;
        AgentAuthenticationOperationRepository::update_state(
            &conn,
            operation_id,
            expected_state,
            AgentAuthenticationOperationState::Failed,
            Some(error_code),
        )
    }

    fn operation_lock(
        &self,
        auth_context_id: &AgentAuthContextId,
    ) -> VibexResult<Arc<tokio::sync::Mutex<()>>> {
        let mut locks = self.operation_locks.lock().map_err(|_| {
            VibexError::process(
                "agent_auth_context_operation_lock_failed",
                "Agent authentication operation lock is unavailable",
            )
        })?;
        Ok(locks
            .entry(auth_context_id.clone())
            .or_insert_with(|| Arc::new(tokio::sync::Mutex::new(())))
            .clone())
    }

    fn notify_changed(&self, context: &AgentAuthContext) {
        let _ = self.changes.send(AgentAuthContextChanged {
            agent_id: context.agent_id.clone(),
            auth_context_id: context.id.clone(),
        });
    }
}

#[async_trait::async_trait]
impl RemoteAgentAuthContextSource for AgentAuthContextService {
    async fn list_auth_contexts(&self) -> VibexResult<Vec<AgentAuthContext>> {
        self.list()
    }

    async fn list_auth_methods(
        &self,
        agent_id: vibex_core::AgentId,
    ) -> VibexResult<vibex_core::AgentAuthCatalog> {
        self.list_auth_methods(agent_id).await
    }

    async fn authenticate_context(
        &self,
        request: AgentAuthContextAuthenticateRequest,
    ) -> VibexResult<AgentAuthContextAuthenticateResult> {
        self.authenticate(request).await
    }

    async fn get_authentication_operation(
        &self,
        operation_id: vibex_core::AgentAuthenticationOperationId,
    ) -> VibexResult<AgentAuthenticationOperation> {
        self.authentication_operation(&operation_id)
    }

    async fn cancel_authentication(
        &self,
        request: AgentAuthContextCancelAuthenticationRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.cancel_authentication(request).await
    }

    async fn verify_context(
        &self,
        request: AgentAuthContextVerifyRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.verify(request).await
    }

    async fn refresh_models(
        &self,
        request: AgentAuthContextRefreshModelsRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.refresh_models(request).await
    }

    async fn logout_preview(
        &self,
        auth_context_id: AgentAuthContextId,
    ) -> VibexResult<AgentAuthContextLogoutPreview> {
        self.logout_preview(&auth_context_id)
    }

    async fn logout(
        &self,
        request: AgentAuthContextLogoutRequest,
    ) -> VibexResult<AgentAuthContextMutationResult> {
        self.logout(request).await
    }
}

fn is_authentication_required(error: &VibexError) -> bool {
    is_authentication_required_code(&error.code)
}

fn is_authentication_required_code(code: &str) -> bool {
    matches!(
        code,
        "agent_authentication_required"
            | "authentication_required"
            | "provider_authentication_required"
            | "unauthorized"
    ) || code.ends_with("_authentication_required")
}

async fn shutdown_account_processes_before_logout<S, L>(shutdown: S, logout: L) -> VibexResult<()>
where
    S: std::future::Future<Output = VibexResult<usize>>,
    L: std::future::Future<Output = VibexResult<()>>,
{
    shutdown.await?;
    logout.await
}

fn terminal_auth_succeeded(status: &AcpTerminalExitStatus) -> bool {
    status.exit_code == Some(0) && status.signal.is_none()
}

fn agent_authentication_cancelled_error() -> VibexError {
    VibexError::conflict(
        "agent_authentication_cancelled",
        "Agent authentication was stopped",
    )
}

fn safe_terminal_descriptor(
    terminal: TerminalAuthActionDescriptor,
    method_id: &str,
) -> TerminalAuthActionDescriptor {
    TerminalAuthActionDescriptor {
        id: method_id.to_string(),
        provider_profile_id: String::new(),
        terminal_id: terminal.terminal_id,
        title: terminal.title,
        command: String::new(),
        args: Vec::new(),
        cwd: None,
        env_keys: Vec::new(),
        redacted_env_summary: Vec::new(),
    }
}

#[cfg(test)]
mod tests {
    use std::sync::{Arc, Mutex};

    use super::*;

    #[tokio::test]
    async fn logout_wire_is_polled_only_after_account_process_shutdown() {
        let events = Arc::new(Mutex::new(Vec::new()));
        let shutdown_events = events.clone();
        let logout_events = events.clone();

        shutdown_account_processes_before_logout(
            async move {
                shutdown_events.lock().unwrap().push("shutdown_complete");
                Ok(1)
            },
            async move {
                let mut events = logout_events.lock().unwrap();
                assert_eq!(events.as_slice(), &["shutdown_complete"]);
                events.push("logout_wire");
                Ok(())
            },
        )
        .await
        .unwrap();

        assert_eq!(
            events.lock().unwrap().as_slice(),
            &["shutdown_complete", "logout_wire"]
        );
    }
}

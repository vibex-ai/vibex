use vibex_core::{
    AgentAuthCatalog, AgentAuthContext, AgentAuthContextAuthenticateRequest,
    AgentAuthContextAuthenticateResult, AgentAuthContextCancelAuthenticationRequest,
    AgentAuthContextId, AgentAuthContextLogoutPreview, AgentAuthContextLogoutRequest,
    AgentAuthContextMutationResult, AgentAuthContextRefreshModelsRequest,
    AgentAuthContextVerifyRequest, AgentAuthEnvironmentUpdateRequest, AgentAuthenticateRequest,
    AgentAuthenticateResult, AgentAuthenticationCancelRequest, AgentAuthenticationOperation,
    AgentAuthenticationOperationId, AgentCommandDiscoverRequest, AgentCommandDiscovery, AgentId,
    AgentNotificationIntent, AgentRuntimeOptionProbeRequest, AgentRuntimeOptionProbeResult,
    AgentSession, AgentSessionRuntimeSelectionEvent, AgentSessionRuntimeSelectionState,
    AgentTimelineDisplaySettings, AgentUsageStatistics, AgentUsageStatisticsRequest,
    CancelAgentSessionRuntimeSwitchRequest, ContinueAgentTurnRequest, CreateAgentSessionRequest,
    FetchTimelineRequest, ForkAgentSessionRequest, GetMessageSubmissionRequest,
    MessageSubmissionState, ProviderProfile, RemoteDeepLinkResolution, RenameAgentSessionRequest,
    ReplaceUserMessagePayload, ResolveElicitationRequest, ResolvePermissionRequest,
    RuntimeSessionEvent, SendAgentMessageRequest, SessionRuntimeOptionCatalog,
    SetDesiredAgentSessionRuntimeRequest, TimelineItem, TimelineLiveEvent, TimelinePage,
    VibexSessionId,
};

use crate::{BackendBound, BackendFuture, BackendResult, MutationRequest};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendEventStream {
    Timeline,
    Runtime,
    RuntimeSelection,
    Usage,
    Fanout,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackendProjection {
    Files,
    Git,
    Sidebar,
    Management,
    Usage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendRefetch {
    pub session_id: Option<VibexSessionId>,
    pub timeline: bool,
    pub runtime: bool,
    pub runtime_selection: bool,
    pub projection: Option<BackendProjection>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackendEvent {
    Timeline(TimelineLiveEvent),
    SessionUpdated(AgentSession),
    Notification(AgentNotificationIntent),
    Runtime(RuntimeSessionEvent),
    RuntimeSelection(AgentSessionRuntimeSelectionEvent),
    ProjectionInvalidated(BackendProjection),
    Lagged {
        stream: BackendEventStream,
        skipped: u64,
        refetch: BackendRefetch,
        observed_live: bool,
    },
    Disconnected,
}

pub trait BackendEventSubscription: BackendBound {
    fn next(&mut self) -> BackendFuture<'_, Option<BackendEvent>>;
}

pub trait AgentBackend: BackendBound {
    fn subscribe(&self) -> BackendResult<Box<dyn BackendEventSubscription>>;

    fn list_sessions(&self, include_archived: bool) -> BackendFuture<'_, Vec<AgentSession>>;

    fn open_session(&self, session_id: VibexSessionId) -> BackendFuture<'_, AgentSession>;

    fn create_session(
        &self,
        request: MutationRequest<CreateAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession>;

    fn fetch_timeline(&self, request: FetchTimelineRequest) -> BackendFuture<'_, TimelinePage>;

    /// Reads the desktop-owned timeline presentation preferences. Mobile uses
    /// this as the inherited base for its local overrides.
    fn get_timeline_display_settings(&self) -> BackendFuture<'_, AgentTimelineDisplaySettings> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_timeline_display_settings_unavailable",
                "Agent timeline display settings are unavailable on this backend",
            ))
        })
    }

    fn fork_session(
        &self,
        _request: MutationRequest<ForkAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_session_fork_unavailable",
                "Agent session forking is unavailable on this backend",
            ))
        })
    }

    fn usage_statistics(
        &self,
        _request: AgentUsageStatisticsRequest,
    ) -> BackendFuture<'_, AgentUsageStatistics> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_usage_statistics_unavailable",
                "Agent usage statistics are unavailable on this backend",
            ))
        })
    }

    /// Resolve a short-lived push/deep-link locator on the authoritative PC.
    /// Native backends that do not expose push routing keep the explicit
    /// unsupported default; remote backends override it with the typed RPC.
    fn resolve_opaque_locator(
        &self,
        _notification_id: String,
        _opaque_locator: String,
    ) -> BackendFuture<'_, RemoteDeepLinkResolution> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "remote_deep_link_unavailable",
                "opaque deep-link resolution is unavailable on this backend",
            ))
        })
    }

    fn send_message(
        &self,
        request: MutationRequest<SendAgentMessageRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>>;

    fn continue_turn(
        &self,
        request: MutationRequest<ContinueAgentTurnRequest>,
    ) -> BackendFuture<'_, Vec<TimelineItem>>;

    fn interrupt(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, bool>;

    fn resolve_permission(
        &self,
        request: MutationRequest<ResolvePermissionRequest>,
    ) -> BackendFuture<'_, TimelineItem>;

    fn resolve_elicitation(
        &self,
        request: MutationRequest<ResolveElicitationRequest>,
    ) -> BackendFuture<'_, TimelineItem>;

    /// Rewrites the latest user message of a session and re-runs its turn.
    fn replace_user_message(
        &self,
        request: MutationRequest<ReplaceUserMessagePayload>,
    ) -> BackendFuture<'_, Vec<TimelineItem>>;

    fn rename_session(
        &self,
        request: MutationRequest<RenameAgentSessionRequest>,
    ) -> BackendFuture<'_, AgentSession>;

    fn archive_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()>;

    fn delete_session(&self, request: MutationRequest<VibexSessionId>) -> BackendFuture<'_, ()>;

    fn list_runtime_options(&self) -> BackendFuture<'_, SessionRuntimeOptionCatalog>;

    /// Runs the one-time Agent-owned runtime option probe that populates the
    /// Agent's runtime option snapshot.
    fn probe_agent_runtime_options(
        &self,
        request: MutationRequest<AgentRuntimeOptionProbeRequest>,
    ) -> BackendFuture<'_, AgentRuntimeOptionProbeResult>;

    fn list_agent_auth_contexts(&self) -> BackendFuture<'_, Vec<AgentAuthContext>> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn list_agent_auth_methods(&self, _agent_id: AgentId) -> BackendFuture<'_, AgentAuthCatalog> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    /// Seeds the per-Agent authentication context when the authority has none.
    fn ensure_default_agent_auth_context(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentAuthContext>;

    /// Re-probes the authentication methods an Agent advertises.
    fn refresh_agent_auth_methods(
        &self,
        request: MutationRequest<AgentId>,
    ) -> BackendFuture<'_, AgentAuthCatalog>;

    /// Resolves one composer trigger against the authority's Agent catalogue,
    /// workspace file tree and Skills.
    fn discover_agent_commands(
        &self,
        _request: AgentCommandDiscoverRequest,
    ) -> BackendFuture<'_, AgentCommandDiscovery> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "composer_commands_unavailable",
                "composer command discovery is unavailable on this backend",
            ))
        })
    }

    /// Reads the durable state of one submitted message.
    ///
    /// The submission record lives with the authority that accepted it, so a
    /// paired client polls it through the same operation instead of giving up
    /// on delivery confirmation.
    fn agent_message_submission(
        &self,
        _request: GetMessageSubmissionRequest,
    ) -> BackendFuture<'_, MessageSubmissionState> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_message_submission_unavailable",
                "durable message submission is unavailable on this backend",
            ))
        })
    }

    /// Runs the legacy per-Agent interactive sign-in for a Provider profile.
    fn authenticate_agent(
        &self,
        _request: MutationRequest<AgentAuthenticateRequest>,
    ) -> BackendFuture<'_, AgentAuthenticateResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    /// Cancels a legacy per-Agent interactive sign-in.
    fn cancel_agent_authentication(
        &self,
        _request: MutationRequest<AgentAuthenticationCancelRequest>,
    ) -> BackendFuture<'_, bool> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    /// Stores the credentials an Agent sign-in method collected for the
    /// Agent's Provider profile.
    fn update_agent_auth_environment(
        &self,
        _request: MutationRequest<AgentAuthEnvironmentUpdateRequest>,
    ) -> BackendFuture<'_, ProviderProfile> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn authenticate_agent_context(
        &self,
        _request: MutationRequest<AgentAuthContextAuthenticateRequest>,
    ) -> BackendFuture<'_, AgentAuthContextAuthenticateResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn get_agent_authentication_operation(
        &self,
        _operation_id: AgentAuthenticationOperationId,
    ) -> BackendFuture<'_, AgentAuthenticationOperation> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn cancel_agent_context_authentication(
        &self,
        _request: MutationRequest<AgentAuthContextCancelAuthenticationRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn verify_agent_auth_context(
        &self,
        _request: MutationRequest<AgentAuthContextVerifyRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn refresh_agent_auth_models(
        &self,
        _request: MutationRequest<AgentAuthContextRefreshModelsRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn preview_agent_auth_logout(
        &self,
        _auth_context_id: AgentAuthContextId,
    ) -> BackendFuture<'_, AgentAuthContextLogoutPreview> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn logout_agent_auth_context(
        &self,
        _request: MutationRequest<AgentAuthContextLogoutRequest>,
    ) -> BackendFuture<'_, AgentAuthContextMutationResult> {
        Box::pin(async {
            Err(crate::BackendError::unsupported(
                "agent_account_auth_unavailable",
                "Agent account authentication is unavailable on this backend",
            ))
        })
    }

    fn runtime_selection(
        &self,
        session_id: VibexSessionId,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;

    fn set_desired_runtime(
        &self,
        request: MutationRequest<SetDesiredAgentSessionRuntimeRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;

    fn cancel_runtime_switch(
        &self,
        request: MutationRequest<CancelAgentSessionRuntimeSwitchRequest>,
    ) -> BackendFuture<'_, AgentSessionRuntimeSelectionState>;
}

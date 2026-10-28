//! The read and write paths for an Agent's one default model source.
//!
//! Waves before this one kept two independent defaults: a scoped Provider
//! default in `agent_default_model_provider_profiles`, and a route default on
//! `agent_model_route_bindings.is_default`. Both could be set, and a session
//! start had no rule for which one won. The unified record resolves that, and
//! its read path deliberately keeps reading the old stores so a database that
//! has not run the backfill still answers with the same precedence.
//!
//! The write path is additive: setting a unified source clears the conflicting
//! legacy default, but does not delete or rewrite the legacy APIs themselves.
//! Those remain the second wave's compatibility contract.

use rusqlite::TransactionBehavior;
use vibex_core::{
    AgentModelSourceDefault, AgentModelSourceDefaultRequest, AgentModelSourceRef,
    ClearAgentModelSourceDefaultRequest, ProviderProfileStatus, RouteStatus,
    SetAgentModelSourceDefaultRequest, VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_db::{
    AgentDefaultModelProviderProfileRepository, AgentModelRouteBindingRepository,
    AgentModelSourceDefaultRepository, ModelRouteRepository,
};

use crate::{
    INTERNAL_PROFILE_ROLE_OPTION_KEY, ProviderConfigService, provider_option_value,
    require_agent_definition, require_agent_profile, validate_default_scope,
};

impl ProviderConfigService {
    /// Resolves the Agent's default source for one scope.
    ///
    /// The answer is computed, not just read: the explicit unified row wins,
    /// then the Agent's route default, then the scoped Provider default, then
    /// the sole binding. `None` means the Agent has no source to start from.
    pub fn get_agent_model_source_default(
        &self,
        request: AgentModelSourceDefaultRequest,
    ) -> VibexResult<AgentModelSourceDefault> {
        validate_default_scope(&request.scope)?;
        require_agent_definition(&request.agent_id)?;
        let conn = self.open_connection()?;
        AgentModelSourceDefaultRepository::resolve(&conn, request.scope, request.agent_id)
    }

    /// Makes one Provider or route group the Agent's default in one scope.
    ///
    /// A Provider source clears the Agent's route default; a route source
    /// clears the scoped Provider default. Both happen in the same transaction
    /// as the unified write, so a reader never sees the two stores disagree.
    pub fn set_agent_model_source_default(
        &self,
        request: SetAgentModelSourceDefaultRequest,
    ) -> VibexResult<AgentModelSourceDefault> {
        validate_default_scope(&request.scope)?;
        require_agent_definition(&request.agent_id)?;
        let mut conn = self.open_connection()?;
        let transaction = conn
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .map_err(|error| {
                VibexError::storage(
                    "agent_model_source_default_transaction_failed",
                    "failed to start the Agent default model source transaction",
                )
                .with_diagnostic("error", error.to_string())
            })?;

        match &request.source {
            AgentModelSourceRef::Provider(provider_profile_id) => {
                let profile =
                    require_agent_profile(&transaction, &request.agent_id, provider_profile_id)?;
                if profile.status != ProviderProfileStatus::Enabled {
                    return Err(VibexError::validation(
                        "agent_model_source_default_provider_disabled",
                        "the Provider named as the default is disabled",
                    )
                    .with_diagnostic("agentId", request.agent_id.as_str())
                    .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
                }
                if provider_profile_id
                    .as_str()
                    .starts_with("provider_local_default_")
                    || provider_option_value(
                        &profile.provider_options,
                        INTERNAL_PROFILE_ROLE_OPTION_KEY,
                    )
                    .is_some()
                {
                    return Err(VibexError::validation(
                        "agent_model_source_default_provider_internal",
                        "an internal Provider cannot be an Agent's default",
                    )
                    .with_diagnostic("agentId", request.agent_id.as_str())
                    .with_diagnostic("providerProfileId", provider_profile_id.as_str()));
                }
                AgentModelRouteBindingRepository::clear_default_for_agent(
                    &transaction,
                    &request.agent_id,
                )?;
            }
            AgentModelSourceRef::RouteGroup(model_route_id) => {
                let route =
                    ModelRouteRepository::get(&transaction, model_route_id)?.ok_or_else(|| {
                        VibexError::validation(
                            "agent_model_source_default_route_not_found",
                            "the route group named as the default was not found",
                        )
                        .with_diagnostic("modelRouteId", model_route_id.as_str())
                    })?;
                if route.status != RouteStatus::Enabled {
                    return Err(VibexError::validation(
                        "agent_model_source_default_route_disabled",
                        "the route group named as the default is disabled",
                    )
                    .with_diagnostic("modelRouteId", model_route_id.as_str()));
                }
                if route.enabled_members().next().is_none() {
                    return Err(VibexError::validation(
                        "agent_model_source_default_route_empty",
                        "the route group named as the default has no enabled member",
                    )
                    .with_diagnostic("modelRouteId", model_route_id.as_str()));
                }
                let bound = AgentModelRouteBindingRepository::list_for_agent(
                    &transaction,
                    &request.agent_id,
                )?
                .into_iter()
                .any(|binding| &binding.model_route_id == model_route_id);
                if !bound {
                    return Err(VibexError::validation(
                        "agent_model_source_default_route_not_bound",
                        "the route group is not bound to this Agent",
                    )
                    .with_diagnostic("agentId", request.agent_id.as_str())
                    .with_diagnostic("modelRouteId", model_route_id.as_str()));
                }
                AgentDefaultModelProviderProfileRepository::clear(
                    &transaction,
                    request.scope.clone(),
                    request.agent_id.clone(),
                )?;
            }
        }

        let default = AgentModelSourceDefault {
            scope: request.scope.clone(),
            agent_id: request.agent_id.clone(),
            source: Some(request.source.clone()),
            updated_at_ms: unix_timestamp_ms().max(1),
        };
        AgentModelSourceDefaultRepository::set(&transaction, &default)?;
        transaction.commit().map_err(|error| {
            VibexError::storage(
                "agent_model_source_default_commit_failed",
                "failed to commit the Agent default model source",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        Ok(default)
    }

    /// Removes the explicit unified default. The legacy fallbacks stay.
    pub fn clear_agent_model_source_default(
        &self,
        request: ClearAgentModelSourceDefaultRequest,
    ) -> VibexResult<bool> {
        validate_default_scope(&request.scope)?;
        require_agent_definition(&request.agent_id)?;
        let conn = self.open_connection()?;
        AgentModelSourceDefaultRepository::clear(&conn, request.scope, request.agent_id)
    }

    /// Every explicit unified default row, for the management bundle.
    pub fn list_agent_model_source_defaults(&self) -> VibexResult<Vec<AgentModelSourceDefault>> {
        let conn = self.open_connection()?;
        AgentModelSourceDefaultRepository::list_all(&conn)
    }

    /// Every explicit unified default row for one Agent, in scope order.
    pub fn list_agent_model_source_defaults_for_agent(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> VibexResult<Vec<AgentModelSourceDefault>> {
        let conn = self.open_connection()?;
        AgentModelSourceDefaultRepository::list_for_agent(&conn, agent_id)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentId, AgentModelProviderDefaultRequest, AgentModelProviderSetDefaultRequest,
        ModelProviderCatalogEntry, ModelProviderProfileCreateRequest, ModelProviderProfileStatus,
        ModelProviderProxyPolicy, ModelRoute, ModelRouteMember, ProviderDefaultScopeKind,
        ProviderKind, ProviderProfile, ProviderProfileDefaultScope, ProviderProfileId, RouteKind,
        RouteStrategy,
    };
    use vibex_db::{ProviderProfileRepository, ProviderProjectionCompatibilityRepository};

    fn global_scope() -> ProviderProfileDefaultScope {
        ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        }
    }

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-agent-model-source-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let _ = std::fs::remove_file(&path);
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).unwrap()
    }

    /// A legacy Provider row owned by the Agent, which also gives it a binding.
    ///
    /// It is inserted directly rather than through `create_profile`: that path
    /// demands a typed ACP configuration, which is unrelated to the default
    /// source this test exercises.
    fn provider(
        service: &ProviderConfigService,
        agent_id: &AgentId,
        name: &str,
    ) -> ProviderProfile {
        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.id = ProviderProfileId::new();
        profile.agent_id = agent_id.clone();
        profile.display_name = name.to_string();
        profile.status = vibex_core::ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://api.example.test/v1".to_string());
        profile.default_model = Some("test-model".to_string());
        let conn = service.open_connection().unwrap();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile).unwrap();
        profile
    }

    fn shared_provider(
        service: &ProviderConfigService,
        name: &str,
    ) -> vibex_core::ModelProviderProfile {
        service
            .create_model_provider_profile(ModelProviderProfileCreateRequest {
                display_name: name.to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![ModelProviderCatalogEntry {
                    id: "route-model".to_string(),
                    display_name: None,
                    enabled: true,
                    capabilities: Default::default(),
                    metadata: Vec::new(),
                }],
                default_model_id: Some("route-model".to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
    }

    fn route(
        service: &ProviderConfigService,
        provider: &vibex_core::ModelProviderProfile,
    ) -> ModelRoute {
        service
            .create_model_route(ModelRoute {
                display_name: "Route".to_string(),
                kind: RouteKind::ModelGroup,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![ModelRouteMember::new(provider.id.clone(), "route-model", 0)],
                ..Default::default()
            })
            .unwrap()
    }

    #[test]
    fn agent_model_source_reads_none_when_nothing_is_bound() {
        let (path, service) = service();
        let default = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(default.source, None);
        assert_eq!(default.updated_at_ms, 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_falls_back_to_a_sole_provider_binding() {
        let (path, service) = service();
        let bounded = provider(&service, &agent("claude"), "Only Provider");
        let default = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(
            default.source,
            Some(AgentModelSourceRef::Provider(bounded.id))
        );
        assert!(default.updated_at_ms > 0);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_prefers_route_default_over_provider_default_over_sole_binding() {
        let (path, service) = service();
        let first = provider(&service, &agent("claude"), "First");
        let second = provider(&service, &agent("claude"), "Second");
        let shared = shared_provider(&service, "Shared");
        let route = route(&service, &shared);
        service
            .bind_model_route(&agent("claude"), &route.id, false)
            .unwrap();

        service
            .set_agent_model_provider_default(AgentModelProviderSetDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                provider_profile_id: second.id.clone(),
            })
            .unwrap();
        let provider_default = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(
            provider_default.source,
            Some(AgentModelSourceRef::Provider(second.id.clone())),
            "the scoped Provider default beats two bindings"
        );

        service
            .set_agent_default_model_route(&agent("claude"), &route.id)
            .unwrap();
        let route_default = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(
            route_default.source,
            Some(AgentModelSourceRef::RouteGroup(route.id.clone())),
            "a route default beats the Provider default"
        );

        let explicit = service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::Provider(first.id.clone()),
            })
            .unwrap();
        assert_eq!(
            explicit.source,
            Some(AgentModelSourceRef::Provider(first.id.clone()))
        );
        let read = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(read.source, explicit.source, "the explicit row wins");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_setting_a_provider_clears_the_route_default() {
        let (path, service) = service();
        let bounded = provider(&service, &agent("claude"), "Provider");
        let shared = shared_provider(&service, "Shared");
        let route = route(&service, &shared);
        service
            .bind_model_route(&agent("claude"), &route.id, true)
            .unwrap();
        assert!(
            service
                .list_agent_model_routes(&agent("claude"))
                .unwrap()
                .iter()
                .any(|binding| binding.is_default)
        );

        service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::Provider(bounded.id.clone()),
            })
            .unwrap();

        assert!(
            service
                .list_agent_model_routes(&agent("claude"))
                .unwrap()
                .iter()
                .all(|binding| !binding.is_default),
            "setting a Provider must clear the Agent's route default"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_setting_a_route_clears_the_scoped_provider_default() {
        let (path, service) = service();
        let bounded = provider(&service, &agent("claude"), "Provider");
        let shared = shared_provider(&service, "Shared");
        let route = route(&service, &shared);
        service
            .bind_model_route(&agent("claude"), &route.id, false)
            .unwrap();
        service
            .set_agent_model_provider_default(AgentModelProviderSetDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                provider_profile_id: bounded.id.clone(),
            })
            .unwrap();

        service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::RouteGroup(route.id.clone()),
            })
            .unwrap();

        let legacy = service
            .get_agent_model_provider_default(vibex_core::AgentModelProviderDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(
            legacy.provider_profile_id, None,
            "setting a route must clear the scoped Provider default"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_set_validates_ownership_and_usability() {
        let (path, service) = service();
        let other = provider(&service, &agent("codex"), "Other Agent");
        let error = service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::Provider(other.id),
            })
            .unwrap_err();
        assert_eq!(error.code, "provider_profile_agent_mismatch");

        let disabled = provider(&service, &agent("claude"), "Disabled");
        service
            .update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: disabled.id.clone(),
                display_name: None,
                status: Some(vibex_core::ProviderProfileStatus::Disabled),
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
        let error = service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::Provider(disabled.id),
            })
            .unwrap_err();
        assert_eq!(error.code, "agent_model_source_default_provider_disabled");

        let shared = shared_provider(&service, "Shared");
        let route = route(&service, &shared);
        let error = service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::RouteGroup(route.id),
            })
            .unwrap_err();
        assert_eq!(error.code, "agent_model_source_default_route_not_bound");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn agent_model_source_clear_is_idempotent() {
        let (path, service) = service();
        let bounded = provider(&service, &agent("claude"), "Provider");
        service
            .set_agent_model_source_default(SetAgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                source: AgentModelSourceRef::Provider(bounded.id.clone()),
            })
            .unwrap();
        assert!(
            service
                .clear_agent_model_source_default(ClearAgentModelSourceDefaultRequest {
                    scope: global_scope(),
                    agent_id: agent("claude"),
                })
                .unwrap()
        );
        assert!(
            !service
                .clear_agent_model_source_default(ClearAgentModelSourceDefaultRequest {
                    scope: global_scope(),
                    agent_id: agent("claude"),
                })
                .unwrap()
        );
        // With the explicit row gone the sole binding is the answer again, not
        // an error and not the removed Provider by accident.
        let read = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(read.source, Some(AgentModelSourceRef::Provider(bounded.id)));
        let _ = std::fs::remove_file(path);
    }

    /// The retired Provider-default API reads and writes the unified source.
    ///
    /// An older client must not be able to create a second default, and a
    /// Provider it sets must be visible through the new read.
    #[test]
    fn the_legacy_provider_default_api_uses_the_unified_source() {
        let (path, service) = service();
        let first = provider(&service, &agent("claude"), "First");
        let second = provider(&service, &agent("claude"), "Second");
        let selection = service
            .set_agent_model_provider_default(AgentModelProviderSetDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
                provider_profile_id: second.id.clone(),
            })
            .unwrap();
        assert_eq!(selection.provider_profile_id, Some(second.id.clone()));
        let unified = service
            .get_agent_model_source_default(AgentModelSourceDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(
            unified.source,
            Some(AgentModelSourceRef::Provider(second.id.clone()))
        );
        let legacy = service
            .get_agent_model_provider_default(AgentModelProviderDefaultRequest {
                scope: global_scope(),
                agent_id: agent("claude"),
            })
            .unwrap();
        assert_eq!(legacy.provider_profile_id, Some(second.id.clone()));
        let _ = first;
        let _ = std::fs::remove_file(path);
    }
}

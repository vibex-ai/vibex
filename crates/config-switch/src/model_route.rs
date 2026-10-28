//! Route management on the Provider configuration service.
//!
//! The repository owns the rows; this layer owns the rules that span them: a
//! route may only name a model its Provider actually serves, a route list is
//! bounded, and binding a route to an Agent must leave that Agent with at most
//! one default.
//!
//! Keeping these here rather than in the UI means the same checks apply to the
//! Config Center, the remote client, and anything added later.
//!
//! # Which layer to call
//!
//! The read methods mirror the repository on purpose, because every other
//! Provider surface does the same: a client never opens a database connection
//! for a Config Center page. The distinction that matters is *writes*.
//!
//! - **Reads** may go through either layer; this one is the one clients use,
//!   because it does not hand them a `Connection`.
//! - **Writes must go through this layer.** `create_model_route` stamps the
//!   revision and checks the member models exist; `update_model_route` sorts and
//!   validates; `bind_model_route` refuses a disabled route and keeps exactly
//!   one default. Calling `ModelRouteRepository` directly skips all of it — the
//!   only reason that repository is public is so migrations can seed rows
//!   without going through a service that is still being constructed.
//!
//! A read that is genuinely a repository query with no rule attached (say,
//! listing routes for a migration) may call the repository; a read under a rule
//! belongs here next to the rule.

use vibex_core::{
    AgentId, AgentModelRouteBinding, MAX_ROUTES, ModelProviderProfile, ModelRoute, ModelRouteId,
    RouteStatus, VibexError, VibexResult,
};
use vibex_db::{
    AgentModelRouteBindingRepository, ModelProviderProfileRepository, ModelRouteRepository,
};

use crate::ProviderConfigService;

impl ProviderConfigService {
    /// Every live route, most recently changed first.
    pub fn list_model_routes(&self) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list(&conn)
    }

    /// Every live, enabled route.
    pub fn list_enabled_model_routes(&self) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list_enabled(&conn)
    }

    pub fn get_model_route(&self, id: &ModelRouteId) -> VibexResult<Option<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::get(&conn, id)
    }

    /// Every route that names this Provider.
    ///
    /// The delete and edit paths read this first, so a Provider change can name
    /// what it would break instead of failing inside a projection later.
    pub fn list_model_routes_naming_provider(
        &self,
        provider: &vibex_core::ModelProviderProfileId,
    ) -> VibexResult<Vec<ModelRoute>> {
        let conn = self.open_connection()?;
        ModelRouteRepository::list_naming_provider(&conn, provider)
    }

    /// Every route bound to this Agent, default first.
    pub fn list_agent_model_routes(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)
    }

    /// Every route an Agent may select, gateway or direct.
    ///
    /// Separate from `gateway_model_catalog` on purpose. That one answers "what
    /// can the gateway serve", and a direct route must never appear in it: its
    /// whole point is not going through the gateway. This one answers "what can
    /// this Agent pick", where both kinds belong, because both resolve to a
    /// launchable Profile.
    pub fn selectable_routes(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<vibex_core::ModelRoute>> {
        let conn = self.open_connection()?;
        let bound = AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?;
        Ok(bound
            .into_iter()
            .filter_map(|binding| {
                ModelRouteRepository::get(&conn, &binding.model_route_id)
                    .ok()
                    .flatten()
            })
            .filter(|route| route.status == vibex_core::RouteStatus::Enabled)
            .filter(|route| !route.unlists && !route.sink)
            .filter(|route| !route.enabled_members().collect::<Vec<_>>().is_empty())
            .collect())
    }

    /// The legacy Profile an Agent launches through for one route.
    ///
    /// A route is not a launch contract; it describes which sources to try. The
    /// ACP projection still starts an Agent from a `ProviderProfile`, so a
    /// session that selects a route group resolves to the gateway Profile that
    /// serves it: each of the group's models is written into that Profile's
    /// model list as `group/<id>/<model>`, and the gateway picks the member.
    ///
    /// A direct route resolves to the Profile of the one Provider it names,
    /// because it never reaches the gateway.
    pub fn legacy_profile_for_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<Option<vibex_core::ProviderProfile>> {
        let conn = self.open_connection()?;
        let Some(route) = ModelRouteRepository::get(&conn, route_id)? else {
            return Ok(None);
        };
        // The route must be one this Agent may use; a route another Agent holds
        // is not a launchable source for this one.
        let bound = AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?
            .iter()
            .any(|binding| &binding.model_route_id == route_id);
        if !bound {
            return Ok(None);
        }
        match route.kind {
            vibex_core::RouteKind::Direct => {
                // A direct route never enters the gateway: it launches through
                // the row the Provider page materialised for this Agent. That
                // row is found by the Provider's display name matching the
                // route's single member, which is how the direct migration
                // named it.
                let Some(member) = route.enabled_members().next() else {
                    return Ok(None);
                };
                let shared = vibex_db::ModelProviderProfileRepository::get(
                    &conn,
                    &member.model_provider_profile_id,
                )?;
                let Some(shared) = shared else {
                    return Ok(None);
                };
                // The compatibility layer records which legacy row a shared
                // Provider came from; matching on it is exact, where a display
                // name would collide as soon as two Agents name theirs alike.
                let Some(legacy_id) = shared.legacy_provider_profile_id else {
                    return Ok(None);
                };
                let profile = vibex_db::ProviderProfileRepository::get(&conn, &legacy_id)?;
                Ok(profile.filter(|profile| &profile.agent_id == agent_id))
            }
            vibex_core::RouteKind::ModelGroup => {
                // A model group contributes one catalogue entry, so the
                // Profile carries the group's prefix rather than one id.
                let prefix = route.model_id();
                Ok(self.gateway_legacy_profile(agent_id)?.filter(|profile| {
                    profile
                        .configured_models
                        .iter()
                        .any(|model| model.id.starts_with(&prefix))
                }))
            }
        }
    }

    /// The gateway's hidden Profile for one Agent, when it has one.
    pub fn gateway_legacy_profile(
        &self,
        agent_id: &AgentId,
    ) -> VibexResult<Option<vibex_core::ProviderProfile>> {
        let conn = self.open_connection()?;
        let profiles = vibex_db::ProviderProfileRepository::list_by_agent(&conn, agent_id, true)?;
        Ok(profiles
            .into_iter()
            .find(crate::model_gateway::is_gateway_profile))
    }

    /// Every live route binding, for a bundle that renders every Agent.
    pub fn list_all_agent_model_route_bindings(&self) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::list_all(&conn)
    }

    /// The Agents that reference this route.
    pub fn list_agent_model_route_users(
        &self,
        route_id: &ModelRouteId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::list_for_route(&conn, route_id)
    }

    pub fn create_model_route(&self, mut route: ModelRoute) -> VibexResult<ModelRoute> {
        let now = vibex_core::unix_timestamp_ms().max(1);
        route.revision = 1;
        route.created_at_ms = now;
        route.updated_at_ms = now;
        route.deleted_at_ms = None;
        route.sync_agent_model_id();
        route.sort_members();
        route.validate()?;
        let conn = self.open_connection()?;
        // Direct routes are materialised from the Provider Profiles an
        // installation already holds, so they are not part of the budget the
        // user spends when composing routes. Counting them here let a large
        // Provider list exhaust the budget before a single group was saved.
        if ModelRouteRepository::count_user_authored(&conn)? >= MAX_ROUTES {
            return Err(VibexError::validation(
                "model_route_limit_exceeded",
                "there are already as many routes as this build supports",
            ));
        }
        self.require_members_are_served(&conn, &route)?;
        self.require_model_group_identity(&conn, &route)?;
        ModelRouteRepository::insert(&conn, &route)?;
        self.get_model_route(&route.id)?.ok_or_else(|| {
            VibexError::storage(
                "model_route_create_readback_failed",
                "failed to read the model route after create",
            )
        })
    }

    /// Replaces a route, including its member order.
    pub fn update_model_route(
        &self,
        mut route: ModelRoute,
        expected_revision: i64,
    ) -> VibexResult<ModelRoute> {
        route.sync_agent_model_id();
        route.sort_members();
        route.validate()?;
        let conn = self.open_connection()?;
        self.require_members_are_served(&conn, &route)?;
        self.require_model_group_identity(&conn, &route)?;
        ModelRouteRepository::update(&conn, &route, expected_revision)
    }

    /// Deletes a route and every binding that pointed at it.
    pub fn delete_model_route(&self, id: &ModelRouteId) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        ModelRouteRepository::delete(&conn, id)
    }

    /// Binds a route to an Agent, optionally making it the Agent's default.
    pub fn bind_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
        make_default: bool,
    ) -> VibexResult<AgentModelRouteBinding> {
        let conn = self.open_connection()?;
        let route = ModelRouteRepository::get(&conn, route_id)?.ok_or_else(|| {
            VibexError::validation("model_route_not_found", "the route was not found")
                .with_diagnostic("modelRouteId", route_id.as_str())
        })?;
        if route.status != RouteStatus::Enabled {
            return Err(VibexError::validation(
                "model_route_disabled",
                "a disabled route cannot be bound to an Agent",
            )
            .with_diagnostic("modelRouteId", route_id.as_str()));
        }
        let existing = AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?
            .into_iter()
            .find(|binding| &binding.model_route_id == route_id);
        if let Some(binding) = existing {
            // Binding twice is a no-op rather than a duplicate row; only the
            // default may still need to move.
            if make_default && !binding.is_default {
                AgentModelRouteBindingRepository::set_default(&conn, agent_id, route_id)?;
            }
            return Ok(
                AgentModelRouteBindingRepository::list_for_agent(&conn, agent_id)?
                    .into_iter()
                    .find(|binding| &binding.model_route_id == route_id)
                    .unwrap_or(binding),
            );
        }
        let now = vibex_core::unix_timestamp_ms().max(1);
        let mut binding = AgentModelRouteBinding::new(agent_id.clone(), route_id.clone());
        binding.is_default = make_default;
        binding.created_at_ms = now;
        binding.updated_at_ms = now;
        AgentModelRouteBindingRepository::insert(&conn, &binding)?;
        Ok(binding)
    }

    pub fn set_agent_default_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<()> {
        // The retired entry point now writes the unified default as well. If
        // it only moved the legacy binding, an explicit Provider source in the
        // unified record would keep winning and the route the user just chose
        // would never be used.
        self.set_agent_model_source_default(vibex_core::SetAgentModelSourceDefaultRequest {
            scope: vibex_core::ProviderProfileDefaultScope {
                kind: vibex_core::ProviderDefaultScopeKind::Global,
                project_id: None,
                workspace_id: None,
            },
            agent_id: agent_id.clone(),
            source: vibex_core::AgentModelSourceRef::RouteGroup(route_id.clone()),
        })?;
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::set_default(&conn, agent_id, route_id)
    }

    pub fn unbind_model_route(
        &self,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<bool> {
        let conn = self.open_connection()?;
        AgentModelRouteBindingRepository::delete(&conn, agent_id, route_id)
    }

    /// Refuses a model group whose Agent-facing id another group already uses.
    ///
    /// The id is not the database key — the route id is — but it is the name an
    /// Agent sees and the suffix of the callable `group/<id>/<model>` address.
    /// Two live groups publishing the same name would make two picker rows look
    /// identical, so the write path refuses it with a readable error before the
    /// database's partial unique index reports a constraint.
    fn require_model_group_identity(
        &self,
        conn: &vibex_db::DbConnection,
        route: &ModelRoute,
    ) -> VibexResult<()> {
        if !route.is_model_group() {
            return Ok(());
        }
        let Some(agent_model) = route.agent_model() else {
            return Ok(());
        };
        for existing in ModelRouteRepository::list(conn)? {
            if existing.id == route.id || !existing.is_model_group() {
                continue;
            }
            if existing.agent_model() == Some(agent_model) {
                return Err(VibexError::validation(
                    "model_route_agent_model_conflict",
                    "another model group already publishes this model id",
                )
                .with_diagnostic("agentModelId", agent_model)
                .with_diagnostic("modelRouteId", existing.id.as_str()));
            }
        }
        Ok(())
    }

    /// Refuses a member that names a model its Provider does not serve.
    ///
    /// This is the check a typo meets first. Without it the mistake surfaces as
    /// a 404 from the gateway at request time, long after the route was saved
    /// and attributed to a Provider that never had the model.
    ///
    /// Every group member names a model its Provider serves; a member whose
    /// Provider has no catalogue is treated as open, because the catalogue may
    /// not have been fetched yet.
    fn require_members_are_served(
        &self,
        conn: &vibex_db::DbConnection,
        route: &ModelRoute,
    ) -> VibexResult<()> {
        let profiles = ModelProviderProfileRepository::list(conn)?;
        for member in &route.members {
            let profile = profiles
                .iter()
                .find(|profile| profile.id == member.model_provider_profile_id)
                .ok_or_else(|| {
                    VibexError::validation(
                        "model_route_member_provider_missing",
                        "a route member names a Provider that does not exist",
                    )
                    .with_diagnostic(
                        "modelProviderProfileId",
                        member.model_provider_profile_id.as_str(),
                    )
                })?;
            if !member.names_model() {
                continue;
            }
            if !model_is_served(profile, &member.model_id) {
                return Err(VibexError::validation(
                    "model_route_member_model_unknown",
                    "a route member names a model its Provider does not serve",
                )
                .with_diagnostic("modelProviderProfileId", profile.id.as_str())
                .with_diagnostic("modelId", member.model_id.trim()));
            }
        }
        Ok(())
    }
}

/// Whether a Provider's catalogue lists this model.
///
/// A Provider with no configured models is treated as open: it may serve
/// anything its endpoint accepts, which is the same assumption the gateway
/// already makes when it forwards a model id it was not told about. Refusing
/// here would block a route against a Provider whose catalogue has not been
/// fetched yet.
fn model_is_served(profile: &ModelProviderProfile, model_id: &str) -> bool {
    let model_id = model_id.trim();
    if profile.configured_models.is_empty() {
        return !model_id.is_empty();
    }
    profile.configured_models.iter().any(|entry| {
        entry.enabled
            && (entry.id == model_id
                || profile
                    .metadata
                    .model_aliases
                    .iter()
                    .any(|alias| alias.alias == model_id && alias.upstream == entry.id))
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileId, ModelProviderProfileStatus,
        ModelRouteMember, RouteKind, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-service-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let _ = std::fs::remove_file(&path);
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// A shared Provider with one model id per entry.
    fn provider(
        service: &ProviderConfigService,
        name: &str,
        models: &[&str],
    ) -> ModelProviderProfileId {
        let mut configured_models = models
            .iter()
            .map(|model| ModelProviderCatalogEntry {
                id: (*model).to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            })
            .collect::<Vec<_>>();
        if !configured_models.is_empty()
            && !configured_models.iter().any(|entry| entry.id == "model")
        {
            configured_models.push(ModelProviderCatalogEntry {
                id: "model".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            });
        }
        let profile = ModelProviderProfile {
            display_name: name.to_string(),
            status: ModelProviderProfileStatus::Enabled,
            configured_models,
            ..Default::default()
        };
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: profile.display_name.clone(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: profile.configured_models.clone(),
                default_model_id: models.first().map(|model| (*model).to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// A model group naming one Provider and the shared test model.
    fn group(provider: ModelProviderProfileId) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Standard".to_string()),
            strategy: RouteStrategy::Ordered,
            status: RouteStatus::Enabled,
            members: vec![ModelRouteMember::new(provider, "model", 0)],
            ..Default::default()
        }
    }

    /// A direct route naming one Provider and one model.
    fn direct(provider: ModelProviderProfileId, model: &str) -> ModelRoute {
        ModelRoute {
            display_name: "Direct".to_string(),
            kind: RouteKind::Direct,
            strategy: RouteStrategy::Ordered,
            status: RouteStatus::Enabled,
            members: vec![ModelRouteMember::new(provider, model, 0)],
            ..Default::default()
        }
    }

    #[test]
    fn creating_a_route_stamps_it_and_returns_the_stored_shape() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let mut route = group(deepseek);
        route.revision = 99; // the service owns the revision
        let created = service.create_model_route(route).unwrap();
        assert_eq!(created.revision, 1);
        assert!(created.created_at_ms > 0);
        assert_eq!(created.members.len(), 1);
        let _ = std::fs::remove_file(path);
    }

    /// A group member names the upstream model it calls.
    #[test]
    fn a_group_member_must_name_a_model() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let mut route = group(deepseek);
        route.members[0].model_id = String::new();
        let error = service.create_model_route(route).unwrap_err();
        assert_eq!(error.code, "route_member_model_invalid");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_member_naming_a_missing_provider_is_refused() {
        let (path, service) = service();
        let route = group(ModelProviderProfileId::new());
        let error = service.create_model_route(route).unwrap_err();
        assert_eq!(error.code, "model_route_member_provider_missing");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_provider_with_no_catalogue_does_not_block_the_route() {
        let (path, service) = service();
        // No configured models: the catalogue has not been fetched yet. The
        // group must still be saveable, or the user could never compose one.
        let open = provider(&service, "Open", &[]);
        assert!(service.create_model_route(group(open)).is_ok());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn binding_twice_is_a_no_op_and_the_default_can_still_move() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let first = service.create_model_route(group(deepseek.clone())).unwrap();
        let mut second_route = group(deepseek);
        second_route.display_name = "Second".to_string();
        let second = service.create_model_route(second_route).unwrap();
        let agent = AgentId::parse("codex").unwrap();

        let bound = service.bind_model_route(&agent, &first.id, true).unwrap();
        assert!(bound.is_default);
        // Binding the same route again must not create a second row.
        let again = service.bind_model_route(&agent, &first.id, false).unwrap();
        assert!(again.is_default, "the existing default is left alone");

        service.bind_model_route(&agent, &second.id, false).unwrap();
        assert_eq!(service.list_agent_model_routes(&agent).unwrap().len(), 2);

        service
            .set_agent_default_model_route(&agent, &second.id)
            .unwrap();
        let bindings = service.list_agent_model_routes(&agent).unwrap();
        assert_eq!(bindings[0].model_route_id, second.id);
        assert!(bindings[0].is_default);
        let _ = std::fs::remove_file(path);
    }

    /// An Agent may hold several groups at once; only one is its default.
    #[test]
    fn an_agent_may_bind_several_groups() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let kimi = provider(&service, "Kimi", &["a"]);
        let mut first = group(deepseek);
        first.display_name = "First".to_string();
        let first = service.create_model_route(first).unwrap();
        let mut second = group(kimi);
        second.display_name = "Second".to_string();
        let second = service.create_model_route(second).unwrap();
        let agent = AgentId::parse("codex").unwrap();

        service.bind_model_route(&agent, &first.id, true).unwrap();
        service.bind_model_route(&agent, &second.id, false).unwrap();

        let bindings = service.list_agent_model_routes(&agent).unwrap();
        assert_eq!(bindings.len(), 2, "both groups stay bound");
        assert_eq!(
            bindings.iter().filter(|binding| binding.is_default).count(),
            1,
            "exactly one default"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn updating_a_route_keeps_the_order_it_was_given() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let kimi = provider(&service, "Kimi", &["a"]);
        let mut route = group(deepseek.clone());
        route
            .members
            .push(ModelRouteMember::new(kimi.clone(), "model", 1));
        let route = service.create_model_route(route).unwrap();

        let mut next = route.clone();
        next.members.reverse();
        for (index, member) in next.members.iter_mut().enumerate() {
            member.order_index = index as i64;
        }
        let updated = service.update_model_route(next, route.revision).unwrap();
        assert_eq!(updated.revision, route.revision + 1);
        assert_eq!(
            updated
                .members
                .iter()
                .map(|member| member.model_provider_profile_id.clone())
                .collect::<Vec<_>>(),
            vec![kimi, deepseek]
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn updating_to_an_unknown_model_is_refused_and_the_route_is_untouched() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service.create_model_route(group(deepseek)).unwrap();

        let mut next = route.clone();
        next.members[0].model_id = "not-a-model".to_string();
        let error = service
            .update_model_route(next, route.revision)
            .unwrap_err();
        assert_eq!(error.code, "model_route_member_model_unknown");

        // The rejected write must not have landed.
        let stored = service.get_model_route(&route.id).unwrap().unwrap();
        assert_eq!(stored.members[0].model_id, "model");
        assert_eq!(stored.revision, route.revision);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn deleting_a_route_unbinds_it_and_removes_it_from_the_list() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service.create_model_route(group(deepseek)).unwrap();
        let agent = AgentId::parse("codex").unwrap();
        service.bind_model_route(&agent, &route.id, true).unwrap();

        assert!(service.delete_model_route(&route.id).unwrap());
        assert!(service.get_model_route(&route.id).unwrap().is_none());
        assert!(service.list_model_routes().unwrap().is_empty());
        assert!(
            service.list_agent_model_routes(&agent).unwrap().is_empty(),
            "a deleted route must not stay bound"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn unbinding_removes_only_that_agents_binding() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service.create_model_route(group(deepseek)).unwrap();
        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();
        service.bind_model_route(&codex, &route.id, true).unwrap();
        service.bind_model_route(&claude, &route.id, true).unwrap();

        assert!(service.unbind_model_route(&codex, &route.id).unwrap());
        assert!(service.list_agent_model_routes(&codex).unwrap().is_empty());
        assert_eq!(
            service.list_agent_model_routes(&claude).unwrap().len(),
            1,
            "the other Agent keeps its binding"
        );
        assert!(
            !service.unbind_model_route(&codex, &route.id).unwrap(),
            "unbinding twice reports that there was nothing left to remove"
        );
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn one_route_can_serve_two_agents_and_reports_both() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let route = service.create_model_route(group(deepseek)).unwrap();
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            service.bind_model_route(&agent, &route.id, false).unwrap();
        }

        let users = service.list_agent_model_route_users(&route.id).unwrap();
        assert_eq!(
            users
                .iter()
                .map(|binding| binding.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude", "codex"]
        );
        let _ = std::fs::remove_file(path);
    }

    /// Two groups with different Provider orders, two Agents bound to different
    /// ones, and both read back exactly as written.
    ///
    /// Nothing here goes through the repository, so this is also the test that
    /// would catch the service and the repository drifting apart.
    #[test]
    fn two_routes_bound_to_two_agents_read_back_in_their_own_order() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["ds-chat", "shared"]);
        let kimi = provider(&service, "Kimi", &["kimi-k3", "shared"]);

        // Route A: DeepSeek first, Kimi second.
        let mut route_a = group(deepseek.clone());
        route_a.display_name = "Standard".to_string();
        route_a
            .members
            .push(ModelRouteMember::new(kimi.clone(), "model", 1));
        let route_a = service.create_model_route(route_a).unwrap();

        // Route B: Kimi first, DeepSeek second.
        let mut route_b = group(kimi.clone());
        route_b.display_name = "Kimi first".to_string();
        route_b
            .members
            .push(ModelRouteMember::new(deepseek.clone(), "model", 1));
        let route_b = service.create_model_route(route_b).unwrap();

        let codex = AgentId::parse("codex").unwrap();
        let claude = AgentId::parse("claude").unwrap();
        service.bind_model_route(&codex, &route_a.id, true).unwrap();
        service
            .bind_model_route(&claude, &route_b.id, true)
            .unwrap();

        let codex_bindings = service.list_agent_model_routes(&codex).unwrap();
        assert_eq!(codex_bindings.len(), 1);
        assert_eq!(codex_bindings[0].model_route_id, route_a.id);
        assert!(codex_bindings[0].is_default);

        let claude_bindings = service.list_agent_model_routes(&claude).unwrap();
        assert_eq!(claude_bindings[0].model_route_id, route_b.id);

        let read_a = service.get_model_route(&route_a.id).unwrap().unwrap();
        assert_eq!(
            read_a
                .members
                .iter()
                .map(|member| member.model_provider_profile_id.clone())
                .collect::<Vec<_>>(),
            vec![deepseek.clone(), kimi.clone()]
        );
        let read_b = service.get_model_route(&route_b.id).unwrap().unwrap();
        assert_eq!(
            read_b
                .members
                .iter()
                .map(|member| member.model_provider_profile_id.clone())
                .collect::<Vec<_>>(),
            vec![kimi, deepseek.clone()],
            "the two routes keep independent orders over the same Providers"
        );

        // One group publishes its own model ids, whatever its members are.
        assert_ne!(read_a.model_id(), read_b.model_id());
        assert!(read_a.model_id().starts_with("group/"));
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_route_may_hold_at_most_sixteen_members() {
        let (path, service) = service();
        let providers = (0..17)
            .map(|index| provider(&service, &format!("Provider {index}"), &["m"]))
            .collect::<Vec<_>>();

        let mut at_limit = group(providers[0].clone());
        for (index, provider) in providers.iter().enumerate().skip(1).take(15) {
            at_limit.members.push(ModelRouteMember::new(
                provider.clone(),
                "model",
                index as i64,
            ));
        }
        assert!(service.create_model_route(at_limit).is_ok());

        let mut over = group(providers[0].clone());
        for (index, provider) in providers.iter().enumerate().skip(1) {
            over.members.push(ModelRouteMember::new(
                provider.clone(),
                "model",
                index as i64,
            ));
        }
        over.display_name = "Too many".to_string();
        let error = service.create_model_route(over).unwrap_err();
        assert_eq!(error.code, "route_member_limit_exceeded");
        let _ = std::fs::remove_file(path);
    }

    /// The route budget counts what the user composed, not what the backfill
    /// materialised.
    ///
    /// An installation with many direct Providers used to exhaust `MAX_ROUTES`
    /// before a single group was saved, because every direct Profile had become a
    /// `Direct` route. Those are derived from the Provider list, so they must not
    /// spend the budget the user needs for the routes they actually author.
    #[test]
    fn materialised_direct_routes_do_not_spend_the_route_budget() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let conn = service.open_connection().unwrap();
        for index in 0..MAX_ROUTES + 8 {
            let mut route = direct(deepseek.clone(), "a");
            route.display_name = format!("Direct {index}");
            ModelRouteRepository::insert(&conn, &route).unwrap();
        }
        drop(conn);

        // The direct rows are already over the limit, but a group must still be
        // saveable because none of them was composed by the user.
        assert!(service.create_model_route(group(deepseek)).is_ok());
        let _ = std::fs::remove_file(path);
    }

    /// A group saves when its members name the same model.
    #[test]
    fn a_model_group_accepts_one_model_at_two_providers() {
        let (path, service) = service();
        let left = provider(&service, "DeepSeek", &["shared", "left-only"]);
        let right = provider(&service, "Kimi", &["shared"]);
        let group = ModelRoute {
            display_name: "Team".to_string(),
            kind: RouteKind::ModelGroup,
            strategy: RouteStrategy::RoundRobin,
            status: RouteStatus::Enabled,
            members: vec![
                ModelRouteMember::new(left, "shared", 0),
                ModelRouteMember::new(right, "shared", 1),
            ],
            ..Default::default()
        };
        let created = service.create_model_route(group).unwrap();
        assert!(created.is_model_group());
        assert_eq!(created.members.len(), 2);
        let _ = std::fs::remove_file(path);
    }

    /// A group still has to name Providers that exist.
    #[test]
    fn a_group_member_naming_a_missing_provider_is_still_refused() {
        let (path, service) = service();
        let error = service
            .create_model_route(group(ModelProviderProfileId::new()))
            .unwrap_err();
        assert_eq!(error.code, "model_route_member_provider_missing");
        let _ = std::fs::remove_file(path);
    }

    /// The budget still bounds what the user authors: the thirty-third group is
    /// refused.
    #[test]
    fn the_route_budget_still_bounds_user_authored_routes() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        for index in 0..MAX_ROUTES {
            let mut route = group(deepseek.clone());
            route.display_name = format!("Route {index}");
            service.create_model_route(route).unwrap();
        }

        let mut over = group(deepseek);
        over.display_name = "One too many".to_string();
        let error = service.create_model_route(over).unwrap_err();
        assert_eq!(error.code, "model_route_limit_exceeded");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_disabled_route_cannot_be_bound() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let mut route = group(deepseek);
        route.status = RouteStatus::Disabled;
        let created = service.create_model_route(route).unwrap();
        let agent = AgentId::parse("claude").unwrap();
        let error = service
            .bind_model_route(&agent, &created.id, false)
            .unwrap_err();
        assert_eq!(error.code, "model_route_disabled");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_provider_lookup_reports_the_routes_it_would_break() {
        let (path, service) = service();
        let deepseek = provider(&service, "DeepSeek", &["a"]);
        let kimi = provider(&service, "Kimi", &["k3"]);
        let mut deepseek_only = group(deepseek.clone());
        deepseek_only.display_name = "DeepSeek only".to_string();
        service.create_model_route(deepseek_only).unwrap();
        let mut mixed = group(deepseek.clone());
        mixed
            .members
            .push(ModelRouteMember::new(kimi.clone(), "model", 1));
        mixed.display_name = "Mixed".to_string();
        service.create_model_route(mixed).unwrap();

        assert_eq!(
            service
                .list_model_routes_naming_provider(&deepseek)
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            service
                .list_model_routes_naming_provider(&kimi)
                .unwrap()
                .len(),
            1
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod catalog_tests {
    use super::super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileId, ModelProviderProfileStatus, ModelRoute,
        ModelRouteMember, RouteKind, RouteStatus, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-catalog-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn provider(
        service: &ProviderConfigService,
        name: &str,
        models: &[&str],
    ) -> ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: name.to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: models
                    .iter()
                    .map(|model| ModelProviderCatalogEntry {
                        id: (*model).to_string(),
                        display_name: None,
                        enabled: true,
                        metadata: Vec::new(),
                        capabilities: Default::default(),
                    })
                    .collect(),
                default_model_id: models.first().map(|model| (*model).to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// The catalog lists one entry per route, and an entry's id carries no
    /// Provider prefix: the choice of Provider belongs to the route.
    #[test]
    fn the_catalog_lists_one_entry_per_route_without_a_provider_name() {
        let (path, service) = service();
        let first = provider(&service, "DeepSeek", &["flash"]);
        let second = provider(&service, "OpenRouter", &["flash"]);
        service
            .create_model_route(ModelRoute {
                display_name: "Standard".to_string(),
                kind: RouteKind::ModelGroup,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![
                    ModelRouteMember::new(first.clone(), "flash", 0),
                    ModelRouteMember::new(second.clone(), "flash", 1),
                ],
                ..Default::default()
            })
            .unwrap();

        let catalog = service.gateway_model_catalog().unwrap();
        let entry = catalog
            .iter()
            .find(|entry| entry.display_name == "Standard")
            .expect("the route is listed");
        assert_eq!(entry.source_count, 2, "two sources back it");
        assert_eq!(entry.model_count, 1, "but only one distinct model name");
        assert!(
            entry.display_id().starts_with("group/"),
            "a group model id is not a Provider-qualified model: {}",
            entry.display_id()
        );
        assert!(
            !entry.display_id().contains("DeepSeek") && !entry.display_id().contains("OpenRouter"),
            "the id must not name either Provider: {}",
            entry.display_id()
        );
        let _ = std::fs::remove_file(path);
    }

    /// A direct route is not served by the gateway.
    ///
    /// It exists to describe a connection that bypasses the gateway entirely.
    /// Publishing it as a gateway model would invite an Agent to call a route
    /// whose whole purpose is not to be called here.
    #[test]
    fn a_direct_route_is_not_published_by_the_gateway() {
        let (path, service) = service();
        let provider_id = provider(&service, "DeepSeek", &["flash"]);
        service
            .create_model_route(ModelRoute {
                display_name: "Direct".to_string(),
                kind: RouteKind::Direct,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![ModelRouteMember::new(provider_id, "flash", 0)],
                ..Default::default()
            })
            .unwrap();

        let catalog = service.gateway_model_catalog().unwrap();
        assert!(
            !catalog.iter().any(|entry| entry.display_name == "Direct"),
            "a direct route must not be published as a gateway model"
        );
        let _ = std::fs::remove_file(path);
    }

    /// A disabled route is not offered, and a route whose Providers are gone is
    /// dropped rather than listed as a model nothing can serve.
    #[test]
    fn a_disabled_route_is_not_listed() {
        let (path, service) = service();
        let provider_id = provider(&service, "DeepSeek", &["flash"]);
        service
            .create_model_route(ModelRoute {
                display_name: "Off".to_string(),
                kind: RouteKind::ModelGroup,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Disabled,
                members: vec![ModelRouteMember::new(provider_id, "flash", 0)],
                ..Default::default()
            })
            .unwrap();

        let catalog = service.gateway_model_catalog().unwrap();
        assert!(
            !catalog.iter().any(|entry| entry.display_name == "Off"),
            "a disabled route must not be offered"
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod capability_tests {
    use super::super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileStatus, ModelRoute, ModelRouteMember,
        ProviderModelCapabilities, RouteKind, RouteStatus, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-caps-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let _ = std::fs::remove_file(&path);
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn provider_with(
        service: &ProviderConfigService,
        name: &str,
        model: &str,
        capabilities: ProviderModelCapabilities,
    ) -> vibex_core::ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: name.to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![ModelProviderCatalogEntry {
                    id: model.to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities,
                }],
                default_model_id: Some(model.to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// An Agent is told what the route can do, not what its best member can do.
    ///
    /// The window is the smallest member's; a boolean feature is claimed only
    /// when every member has it. Otherwise the Agent would send an image to a
    /// route that may route it to a member which cannot read one.
    #[test]
    fn a_route_claims_only_what_all_its_members_share() {
        let (path, service) = service();
        let wide = provider_with(
            &service,
            "Wide",
            "m",
            ProviderModelCapabilities {
                context_tokens: Some(200_000),
                image_input: Some(true),
                reasoning: Some(true),
                ..Default::default()
            },
        );
        let narrow = provider_with(
            &service,
            "Narrow",
            "m",
            ProviderModelCapabilities {
                context_tokens: Some(32_000),
                image_input: Some(false),
                reasoning: Some(true),
                ..Default::default()
            },
        );
        service
            .create_model_route(ModelRoute {
                display_name: "Mixed".to_string(),
                kind: RouteKind::ModelGroup,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![
                    ModelRouteMember::new(wide, "m", 0),
                    ModelRouteMember::new(narrow, "m", 1),
                ],
                ..Default::default()
            })
            .unwrap();

        let conn = service.open_connection().unwrap();
        let profiles = vibex_db::ModelProviderProfileRepository::list(&conn).unwrap();
        let route = vibex_db::ModelRouteRepository::list(&conn)
            .unwrap()
            .remove(0);
        let merged = vibex_core::intersect_route_capabilities(route.enabled_members().filter_map(
            |member| {
                profiles
                    .iter()
                    .find(|profile| profile.id == member.model_provider_profile_id)
                    .and_then(|profile| {
                        profile
                            .configured_models
                            .iter()
                            .find(|entry| entry.enabled && entry.id == member.model_id)
                    })
                    .map(|entry| entry.capabilities.clone())
            },
        ));
        assert_eq!(
            merged.context_tokens,
            Some(32_000),
            "the smallest window is the one the Agent must respect"
        );
        assert_eq!(merged.image_input, None, "one member cannot read an image");
        assert_eq!(merged.reasoning, Some(true), "both can reason");
        let _ = std::fs::remove_file(path);
    }

    /// An explicit route window wins over the smallest member.
    #[test]
    fn an_explicit_route_window_overrides_the_smallest_member() {
        let (path, service) = service();
        let small = provider_with(
            &service,
            "Small",
            "m",
            ProviderModelCapabilities {
                context_tokens: Some(8_000),
                ..Default::default()
            },
        );
        service
            .create_model_route(ModelRoute {
                display_name: "Declared".to_string(),
                kind: RouteKind::ModelGroup,
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                context_tokens: Some(64_000),
                members: vec![ModelRouteMember::new(small, "m", 0)],
                ..Default::default()
            })
            .unwrap();

        let conn = service.open_connection().unwrap();
        let profiles = vibex_db::ModelProviderProfileRepository::list(&conn).unwrap();
        let route = vibex_db::ModelRouteRepository::list(&conn)
            .unwrap()
            .remove(0);
        // A group's explicit window overrides the smallest-member rule when the
        // group is projected into an Agent's Profile.
        let merged = vibex_core::intersect_route_capabilities(route.enabled_members().filter_map(
            |member| {
                profiles
                    .iter()
                    .find(|profile| profile.id == member.model_provider_profile_id)
                    .and_then(|profile| {
                        profile
                            .configured_models
                            .iter()
                            .find(|entry| entry.enabled && entry.id == member.model_id)
                    })
                    .map(|entry| entry.capabilities.clone())
            },
        ));
        let declared = route.context_tokens.or(merged.context_tokens);
        assert_eq!(declared, Some(64_000));
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod model_group_tests {
    use super::*;
    use vibex_core::{
        ModelProviderCatalogEntry, ModelProviderProfileId, ModelProviderProfileStatus,
        ModelRouteMember, RouteKind, RouteStrategy,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-route-model-group-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let _ = std::fs::remove_file(&path);
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn provider(
        service: &ProviderConfigService,
        name: &str,
        models: &[&str],
    ) -> ModelProviderProfileId {
        let profile = ModelProviderProfile {
            display_name: name.to_string(),
            status: ModelProviderProfileStatus::Enabled,
            configured_models: models
                .iter()
                .map(|model| ModelProviderCatalogEntry {
                    id: (*model).to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                })
                .collect(),
            ..Default::default()
        };
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: profile.display_name.clone(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: profile.configured_models.clone(),
                default_model_id: models.first().map(|model| (*model).to_string()),
                headers: Vec::new(),
                status: ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    fn model_group(
        left: ModelProviderProfileId,
        left_model: &str,
        right: ModelProviderProfileId,
        right_model: &str,
        agent_model: &str,
    ) -> ModelRoute {
        ModelRoute {
            display_name: "Fast".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some(agent_model.to_string()),
            strategy: RouteStrategy::Ordered,
            status: RouteStatus::Enabled,
            members: vec![
                ModelRouteMember::new(left, left_model, 0),
                ModelRouteMember::new(right, right_model, 1),
            ],
            ..Default::default()
        }
    }

    #[test]
    fn a_model_group_saves_across_different_upstream_models() {
        let (path, service) = service();
        let left = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let right = provider(&service, "Kimi", &["kimi-k2"]);
        let route = service
            .create_model_route(model_group(
                left,
                "deepseek-chat",
                right,
                "kimi-k2",
                "vibex-fast",
            ))
            .unwrap();
        assert_eq!(route.kind, RouteKind::ModelGroup);
        assert_eq!(route.agent_model_id.as_deref(), Some("Fast"));
        assert_eq!(route.members.len(), 2);
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_second_live_model_group_cannot_publish_the_same_model() {
        let (path, service) = service();
        let left = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let right = provider(&service, "Kimi", &["kimi-k2"]);
        service
            .create_model_route(model_group(
                left.clone(),
                "deepseek-chat",
                right.clone(),
                "kimi-k2",
                "vibex-fast",
            ))
            .unwrap();
        let error = service
            .create_model_route(model_group(
                left,
                "deepseek-chat",
                right,
                "kimi-k2",
                "vibex-fast",
            ))
            .unwrap_err();
        assert_eq!(error.code, "model_route_agent_model_conflict");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_model_group_member_must_name_a_served_model() {
        let (path, service) = service();
        let left = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let right = provider(&service, "Kimi", &["kimi-k2"]);
        let error = service
            .create_model_route(model_group(
                left,
                "not-served",
                right,
                "kimi-k2",
                "vibex-fast",
            ))
            .unwrap_err();
        assert_eq!(error.code, "model_route_member_model_unknown");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn a_model_group_member_may_name_a_local_alias() {
        let (path, service) = service();
        let provider_id = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let mut profile = service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| profile.id == provider_id)
            .unwrap();
        profile.metadata.model_aliases = vec![vibex_core::ProviderModelAlias {
            alias: "fast".to_string(),
            upstream: "deepseek-chat".to_string(),
        }];
        let revision = profile.revision;
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile,
                expected_revision: revision,
            })
            .unwrap();
        let route = service
            .create_model_route(ModelRoute {
                display_name: "Alias".to_string(),
                kind: RouteKind::ModelGroup,
                agent_model_id: Some("vibex-alias".to_string()),
                strategy: RouteStrategy::Ordered,
                status: RouteStatus::Enabled,
                members: vec![ModelRouteMember::new(provider_id, "fast", 0)],
                ..Default::default()
            })
            .unwrap();
        assert_eq!(route.members[0].model_id, "fast");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_unlisted_model_group_is_not_selectable() {
        let (path, service) = service();
        let left = provider(&service, "DeepSeek", &["deepseek-chat"]);
        let right = provider(&service, "Kimi", &["kimi-k2"]);
        let mut route = model_group(left, "deepseek-chat", right, "kimi-k2", "vibex-fast");
        route.unlists = true;
        let route = service.create_model_route(route).unwrap();
        let agent = AgentId::parse("codex").unwrap();
        service.bind_model_route(&agent, &route.id, true).unwrap();
        assert!(
            service.selectable_routes(&agent).unwrap().is_empty(),
            "an unlisted group must not appear in a model picker"
        );
        let _ = std::fs::remove_file(path);
    }
}

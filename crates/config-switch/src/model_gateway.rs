//! Wiring the local model gateway into the Agents that use a route group.
//!
//! The gateway is one shared endpoint for the whole installation, and it has no
//! Provider list of its own: the Providers it may call are the members of the
//! enabled route groups. An Agent reaches it by binding a route group, so the
//! binding is the only switch — there is no per-Agent gateway toggle.
//!
//! While the listener is up, this module keeps one hidden legacy Profile for
//! each Agent that has at least one bound, enabled group. The Profile's address
//! is the gateway and its model list is exactly the bound groups' model ids, so
//! an Agent can only ask for something it was actually bound to. Behaviour of
//! the whole feature follows from two switches:
//!
//! - switching the gateway off removes those Profiles, so no Agent can reach
//!   the groups; direct Providers are untouched because they never used it;
//! - unbinding the last group from an Agent removes its Profile, so it falls
//!   back to its own direct Provider.
//!
//! The Profiles are marked with the internal role so they never appear in an
//! Agent's provider list.

use std::collections::BTreeSet;

use vibex_core::{
    AgentId, ProviderConfiguredModel, ProviderKind, ProviderModelCapabilities,
    ProviderModelWireApi, ProviderOptions, ProviderProfile, ProviderProfileCreateRequest,
    ProviderProfileId, ProviderProfileStatus, ProviderSecretBackend, ProviderSecretKind,
    ProviderSecretReferenceCreateRequest, ProviderSecretSetupState, VibexError, VibexResult,
};
use vibex_db::{
    AgentDefaultModelProviderProfileRepository, ModelProviderProfileRepository,
    ProviderProfileRepository,
};

use crate::{ProviderConfigService, provider_option_value};

/// The internal role that keeps a Profile out of the Config Center's lists.
pub const GATEWAY_PROFILE_ROLE_OPTION_KEY: &str = "vibex.internal.profileRole";
pub const GATEWAY_PROFILE_ROLE: &str = "model_gateway";
/// The option a gateway Profile carries its owning Agent in, so it can be
/// found without guessing from its display name.
pub const GATEWAY_AGENT_OPTION_KEY: &str = "vibex.internal.gatewayAgent";

/// The placeholder the Agent sends as its API Key. The gateway ignores inbound
/// credentials: it authenticates to the upstream with the Key it selects, so a
/// local process never needs to hold one.
///
/// It is spelled as a single printable-ASCII token on purpose. Agents validate
/// their credential *shape* before building a request, and DeepSeek Harness
/// refuses any Key carrying a character an HTTP header cannot carry — a space is
/// one. `managed by Vibex` therefore never reached the gateway at all: the
/// launch died inside the Agent with `INVALID_CREDENTIAL`, which the ACP layer
/// recognised through the "api key" vocabulary and reported as "Agent
/// authentication is required". Nothing interprets the value, so it only has to
/// be legal.
pub const GATEWAY_LOCAL_KEY: &str = "vibex-local-gateway";

/// The human-readable description of [`GATEWAY_LOCAL_KEY`] carried by the
/// gateway Profile's stored credential summary. Display text only — never
/// injected, because the Agent rejects a Key containing a space.
pub const GATEWAY_LOCAL_KEY_HINT: &str = "managed by Vibex";

impl ProviderConfigService {
    /// The Agents the gateway serves: the ones bound to an enabled group.
    ///
    /// This is derived, not chosen. An Agent with no group binding is not on
    /// the gateway at all; an Agent that manages its own provider cannot be
    /// bound to a group and so never appears.
    ///
    /// The hidden Profiles are only materialised while the listener is up. This
    /// answer therefore describes intent, which is what a Config Center needs:
    /// it stays the same across a gateway restart instead of blinking empty.
    pub fn gateway_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        let conn = self.open_connection()?;
        if !vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?
            .policy
            .enabled
        {
            return Ok(BTreeSet::new());
        }
        Ok(agents_with_gateway_routes(&conn)?.into_iter().collect())
    }

    /// The Agents currently pointed at the gateway's address.
    ///
    /// A configured Agent whose Profile has been removed is *paused*, not
    /// active: its requests use the direct Provider until the listener returns.
    pub fn gateway_active_agents(&self) -> VibexResult<BTreeSet<AgentId>> {
        let conn = self.open_connection()?;
        let profiles = ProviderProfileRepository::list_all(&conn)?;
        let mut agents = BTreeSet::new();
        for profile in profiles {
            if !is_gateway_profile(&profile) {
                continue;
            }
            if let Some(agent_id) = gateway_profile_agent(&profile) {
                agents.insert(agent_id);
            }
        }
        Ok(agents)
    }

    /// Points every Agent that is bound to a route group at the gateway.
    ///
    /// This is the enabling half of the gateway switch. `model_catalog` is the
    /// full set of group model ids the listener serves; each Agent's Profile
    /// receives only the entries belonging to the groups it is bound to, so a
    /// binding is what grants the model, not merely what grants the endpoint.
    /// Returns the Agents whose hidden Profile actually changed.
    pub fn attach_gateway_to_agents(
        &self,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<Vec<AgentId>> {
        let base_url = base_url.trim();
        if base_url.is_empty() {
            return Err(VibexError::validation(
                "model_gateway_unavailable",
                "the gateway is not running, so no Agent can be pointed at it",
            ));
        }
        // The switch is global, so a disabled gateway must never leave a
        // Profile pointing at it — even when a listener happens to be bound.
        let conn = self.open_connection()?;
        if !vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn)?
            .policy
            .enabled
        {
            drop(conn);
            return self.deactivate_gateway_profiles();
        }
        let bindings = vibex_db::AgentModelRouteBindingRepository::list_all(&conn)?;
        let routes = vibex_db::ModelRouteRepository::list_enabled(&conn)?;
        drop(conn);

        // Every Agent gets only the groups it references, and only while that
        // group is enabled: an Agent whose groups were all disabled or deleted
        // must lose its Profile rather than keep a catalogue it cannot call.
        let mut per_agent: std::collections::BTreeMap<AgentId, Vec<String>> =
            std::collections::BTreeMap::new();
        for binding in &bindings {
            if binding.deleted_at_ms.is_some() {
                continue;
            }
            let Some(route) = routes
                .iter()
                .find(|route| route.id == binding.model_route_id)
            else {
                continue;
            };
            if !route.kind.via_gateway() {
                continue;
            }
            let entries = per_agent.entry(binding.agent_id.clone()).or_default();
            for model in model_catalog {
                if group_catalog_belongs_to(model, route) && !entries.contains(model) {
                    entries.push(model.clone());
                }
            }
        }

        let mut attached = Vec::new();
        let bound: BTreeSet<AgentId> = per_agent.keys().cloned().collect();
        for agent_id in &bound {
            if gateway_wire_api_for(agent_id).is_none() {
                // The Agent speaks a protocol the gateway does not serve, so it
                // cannot be pointed at it. Its binding still exists; it simply
                // launches through a direct Provider.
                continue;
            }
            let catalog = per_agent.remove(agent_id).unwrap_or_default();
            // One Agent failing must not deny the gateway to the rest. That is
            // exactly the shape of an Agent that is merely unconfigured — an
            // ACP Agent with no runtime config yet. The skip is silent on
            // purpose: the Agent simply keeps its direct Provider, which is
            // also what `gateway_active_agents` reports.
            if let Ok((_, changed)) = self.ensure_gateway_profile(agent_id, base_url, &catalog)
                && changed
            {
                attached.push(agent_id.clone());
            }
        }
        // An Agent that lost its last binding must lose its Profile too.
        for profile in ProviderProfileRepository::list_all(&self.open_connection()?)? {
            if !is_gateway_profile(&profile) {
                continue;
            }
            let Some(agent_id) = gateway_profile_agent(&profile) else {
                continue;
            };
            if bound.contains(&agent_id) {
                continue;
            }
            self.delete_profile(vibex_core::ProviderProfileDeleteRequest {
                provider_profile_id: profile.id,
            })?;
            attached.push(agent_id);
        }
        Ok(attached)
    }

    /// Removes every hidden gateway Profile, returning the Agents affected.
    ///
    /// This is the disabling half of the global switch. Deleting the Profile is
    /// what actually moves an Agent back to its direct Provider; the durable
    /// per-Agent rows that earlier builds kept are no longer consulted, so
    /// nothing has to be remembered per Agent.
    pub fn detach_gateway_from_agents(&self) -> VibexResult<Vec<AgentId>> {
        self.deactivate_gateway_profiles()
    }

    /// Removes every hidden gateway Profile.
    ///
    /// This is what a stopped or disabled listener has to do: an Agent whose
    /// Profile still points at a dead loopback port cannot fall back, so the
    /// Profile goes away and the Agent returns to its direct default.
    ///
    /// The scan is over the existing hidden Profiles rather than over the
    /// capable Agents, because it has to work precisely when the gateway is
    /// off — the state in which the intent list is empty.
    pub fn deactivate_gateway_profiles(&self) -> VibexResult<Vec<AgentId>> {
        let conn = self.open_connection()?;
        let mut deactivated = Vec::new();
        for profile in ProviderProfileRepository::list_all(&conn)? {
            if !is_gateway_profile(&profile) {
                continue;
            }
            let Some(agent_id) = gateway_profile_agent(&profile) else {
                continue;
            };
            self.delete_profile(vibex_core::ProviderProfileDeleteRequest {
                provider_profile_id: profile.id,
            })?;
            deactivated.push(agent_id);
        }
        Ok(deactivated)
    }

    /// Creates or refreshes one hidden gateway Profile.
    ///
    /// Returns whether the Profile's projected content changed. A no-op leaves
    /// the revision alone, which is what keeps a snapshot refresh from
    /// restaling every Agent merely because it ran.
    fn ensure_gateway_profile(
        &self,
        agent_id: &AgentId,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<(ProviderProfileId, bool)> {
        let models = gateway_models(agent_id, model_catalog);
        let conn = self.open_connection()?;
        let models = self.with_route_capabilities(&conn, models)?;
        let existing = gateway_profile_for(&conn, agent_id)?;
        match existing {
            Some(profile) => {
                let unchanged = profile.base_url.as_deref() == Some(base_url)
                    && profile.configured_models == models
                    && profile.status == ProviderProfileStatus::Enabled;
                let profile_id = profile.id.clone();
                drop(conn);
                if !unchanged {
                    // An ACP Agent's Profile must carry typed ACP configuration:
                    // every write validates that, and a gateway Profile had none,
                    // so `acp_config_missing` refused the update and the per-Agent
                    // `if let Ok` swallowed it. The catalogue then never reached
                    // the Profile and the Agent's picker stayed empty. The same
                    // inheritance the shared-Provider create path uses fills it
                    // in, and it keeps the internal role keys the gateway is
                    // identified by — the agent-scoped update strips those, which
                    // is why this stays on the generic update.
                    let conn = self.open_connection()?;
                    let provider_options = crate::inherit_agent_acp_runtime_options(
                        &conn,
                        agent_id,
                        Some(gateway_provider_options(agent_id, profile_id.as_str())),
                    )?;
                    let provider_options = crate::with_acp_configured_models(
                        provider_options,
                        crate::configured_acp_model_ids(
                            &models,
                            models.first().map(|model| model.id.as_str()),
                        ),
                    )?;
                    drop(conn);
                    self.update_profile(vibex_core::ProviderProfileUpdateRequest {
                        provider_profile_id: profile_id.clone(),
                        display_name: Some(gateway_display_name()),
                        status: Some(ProviderProfileStatus::Enabled),
                        account_alias: None,
                        base_url: Some(base_url.to_string()),
                        default_model: models.first().map(|model| model.id.clone()),
                        small_model: None,
                        large_model: None,
                        configured_models: Some(models),
                        reasoning_effort: None,
                        sandbox_defaults: None,
                        network_defaults: None,
                        permission_defaults: None,
                        provider_options: Some(provider_options),
                    })?;
                }
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, !unchanged))
            }
            None => {
                drop(conn);
                // The creation write validates ACP configuration too, and an ACP
                // Agent has none until it is inherited here. Without this the
                // Profile was never created at all, so every ACP Agent —
                // `deepseek-harness` among them — silently stayed off the
                // gateway and could never see a route, group or otherwise.
                let conn = self.open_connection()?;
                let provider_options = crate::inherit_agent_acp_runtime_options(
                    &conn,
                    agent_id,
                    Some(gateway_provider_options(agent_id, "")),
                )?;
                let provider_options = crate::with_acp_configured_models(
                    provider_options,
                    crate::configured_acp_model_ids(
                        &models,
                        models.first().map(|model| model.id.as_str()),
                    ),
                )?;
                drop(conn);
                let created = self.create_profile(ProviderProfileCreateRequest {
                    agent_id: Some(agent_id.clone()),
                    kind: gateway_provider_kind(agent_id),
                    display_name: gateway_display_name(),
                    account_alias: None,
                    base_url: Some(base_url.to_string()),
                    default_model: models.first().map(|model| model.id.clone()),
                    small_model: None,
                    large_model: None,
                    configured_models: models,
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: Some(provider_options),
                    secret_references: vec![ProviderSecretReferenceCreateRequest {
                        secret_kind: ProviderSecretKind::ApiKey,
                        backend: ProviderSecretBackend::Placeholder,
                        setup_state: ProviderSecretSetupState::Referenced,
                        lookup_key: format!("vibex-model-gateway-{}", agent_id.as_str()),
                        display_label: "Vibex Gateway".to_string(),
                        redacted_hint: GATEWAY_LOCAL_KEY_HINT.to_string(),
                    }],
                })?;
                let profile_id = created.id.clone();
                // Rewrite the options now that the Profile id exists, keeping the
                // ACP configuration the create inherited: this write validates
                // an ACP Profile, and replacing its options with the bare gateway
                // keys would be refused.
                let conn = self.open_connection()?;
                let provider_options = crate::inherit_agent_acp_runtime_options(
                    &conn,
                    agent_id,
                    Some(gateway_provider_options(agent_id, profile_id.as_str())),
                )?;
                let provider_options = crate::with_acp_configured_models(
                    provider_options,
                    crate::configured_acp_model_ids(
                        &created.configured_models,
                        created.default_model.as_deref(),
                    ),
                )?;
                drop(conn);
                self.update_profile(vibex_core::ProviderProfileUpdateRequest {
                    provider_profile_id: profile_id.clone(),
                    display_name: None,
                    status: None,
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
                    provider_options: Some(provider_options),
                })?;
                self.set_gateway_default(agent_id, &profile_id)?;
                Ok((profile_id, true))
            }
        }
    }

    /// Fills each route-bearing model in with the capabilities its members
    /// share.
    ///
    /// `gateway_models` builds entries from model ids alone, because the
    /// catalog is a list of strings. A route's capabilities are a property of
    /// its members, so they are merged here, where the Profiles are in hand.
    fn with_route_capabilities(
        &self,
        conn: &vibex_db::DbConnection,
        models: Vec<ProviderConfiguredModel>,
    ) -> VibexResult<Vec<ProviderConfiguredModel>> {
        let routes = vibex_db::ModelRouteRepository::list_enabled(conn)?;
        if routes.is_empty() {
            return Ok(models);
        }
        let profiles = vibex_db::ModelProviderProfileRepository::list(conn)?;
        Ok(models
            .into_iter()
            .map(|model| {
                // Every gateway model belongs to a group and publishes
                // `group/<id>/<model>`, so its capabilities come from the
                // Providers that serve that one model.
                let Some(parsed) = vibex_core::route_group_model_ref(&model.id) else {
                    return model;
                };
                let Some(route) = routes
                    .iter()
                    .find(|route| route.id.as_str() == parsed.group_id)
                else {
                    return model;
                };
                // A model group's upstream models may differ, so the
                // capabilities of its virtual model are what every reachable
                // member promises.
                let caps = route
                    .enabled_members()
                    .filter_map(|member| {
                        profiles
                            .iter()
                            .find(|profile| profile.id == member.model_provider_profile_id)
                            .and_then(|profile| member_catalog_entry(profile, &member.model_id))
                            .map(|entry| entry.capabilities.clone())
                    })
                    .collect::<Vec<_>>();
                let caps = vibex_core::intersect_route_capabilities(caps);
                ProviderConfiguredModel {
                    capabilities: vibex_core::ProviderModelCapabilities {
                        context_tokens: route.context_tokens.or(caps.context_tokens),
                        ..caps
                    },
                    ..model
                }
            })
            .collect())
    }

    /// Rewrites every gateway Profile's address after the listener moved.
    ///
    /// Each write advances the Profile revision, so the existing projection
    /// machinery marks exactly the affected bindings stale and no others.
    pub fn refresh_gateway_profiles(
        &self,
        base_url: &str,
        model_catalog: &[String],
    ) -> VibexResult<Vec<AgentId>> {
        self.attach_gateway_to_agents(base_url, model_catalog)
    }

    fn set_gateway_default(
        &self,
        agent_id: &AgentId,
        profile_id: &ProviderProfileId,
    ) -> VibexResult<()> {
        self.set_agent_model_provider_default(vibex_core::AgentModelProviderSetDefaultRequest {
            scope: gateway_default_scope(),
            agent_id: agent_id.clone(),
            provider_profile_id: profile_id.clone(),
        })?;
        Ok(())
    }

    /// The durable per-Key routing state of every Provider the groups name.
    pub fn gateway_account_states(
        &self,
    ) -> VibexResult<Vec<vibex_core::ModelProviderAccountState>> {
        let conn = self.open_connection()?;
        let mut states = Vec::new();
        for provider in gateway_route_members_in(&conn)? {
            states.extend(vibex_db::ModelProviderAccountStateRepository::list(
                &conn, &provider,
            )?);
        }
        Ok(states)
    }

    /// Every Provider the enabled route groups name.
    ///
    /// The Config Center reads this to show whose Keys back the routes; it is
    /// not a configuration surface, because adding a Provider here would mean
    /// adding it to a group.
    pub fn gateway_route_members(&self) -> VibexResult<Vec<vibex_core::ModelProviderProfileId>> {
        let conn = self.open_connection()?;
        gateway_route_members_in(&conn)
    }

    /// The models the gateway currently exposes, for the Config Center.
    ///
    /// One entry per shared group model, not per member: a group is a set of
    /// Providers, and the model is what a session actually picks. The Providers
    /// behind it are the group's business, and listing them separately would
    /// ask the Agent to choose a source the group exists to choose.
    ///
    /// A group with no reachable Provider, or with no model every Provider
    /// serves, contributes nothing: it has nothing to answer with.
    pub fn gateway_model_catalog(&self) -> VibexResult<Vec<GatewayCatalogModel>> {
        let conn = self.open_connection()?;
        let profiles = ModelProviderProfileRepository::list(&conn)?;
        let routes = vibex_db::ModelRouteRepository::list_enabled(&conn)?;
        let mut catalog = Vec::new();
        for route in routes {
            // A direct route describes a connection that bypasses the gateway.
            // Publishing it here would invite an Agent to call the gateway for
            // something whose whole point is not to go through it.
            if !route.kind.via_gateway() {
                continue;
            }
            // A hidden composition is still callable by its id, but it must not
            // appear in the list an Agent's model picker reads.
            if route.unlists || route.sink {
                continue;
            }
            let Some(agent_model) = route.agent_model() else {
                continue;
            };
            let source_count = route
                .enabled_members()
                .filter(|member| {
                    profiles.iter().any(|profile| {
                        profile.id == member.model_provider_profile_id
                            && (profile.configured_models.is_empty()
                                || member_catalog_entry(profile, &member.model_id).is_some())
                    })
                })
                .count();
            if source_count == 0 {
                continue;
            }
            catalog.push(GatewayCatalogModel {
                route_id: Some(route.id.as_str().to_string()),
                display_name: route.display_name.clone(),
                model: route.group_model_id(agent_model),
                kind: route.kind,
                source_count,
                model_count: 1,
            });
        }
        Ok(catalog)
    }
}

/// One model the gateway serves.
///
/// The id an Agent asks for is `model`; `display_name` is what a picker groups
/// it under. It never carries a Provider name, because the Provider is the
/// group's business, not the Agent's.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayCatalogModel {
    /// The group this model belongs to.
    pub route_id: Option<String>,
    pub display_name: String,
    pub model: String,
    pub kind: vibex_core::RouteKind,
    /// How many members can answer.
    pub source_count: usize,
    /// How many distinct model names the route offers.
    pub model_count: usize,
}

impl GatewayCatalogModel {
    /// The id an Agent's model list shows.
    ///
    /// It is already `group/<id>/<model>`; it must not gain a Provider prefix,
    /// or the Agent would appear to have picked a vendor.
    pub fn display_id(&self) -> String {
        self.model.clone()
    }
}

/// The most models one Profile may carry.
const MAX_GATEWAY_MODELS: usize = 512;

/// Every Agent bound to at least one enabled group.
///
/// This is the gateway's whole Agent set: a binding is the only way onto it.
fn agents_with_gateway_routes(conn: &vibex_db::DbConnection) -> VibexResult<BTreeSet<AgentId>> {
    let routes = vibex_db::ModelRouteRepository::list_enabled(conn)?;
    let mut agents = BTreeSet::new();
    for binding in vibex_db::AgentModelRouteBindingRepository::list_all(conn)? {
        if binding.deleted_at_ms.is_some() {
            continue;
        }
        let served = routes
            .iter()
            .any(|route| route.id == binding.model_route_id && route.kind.via_gateway());
        if served {
            agents.insert(binding.agent_id);
        }
    }
    Ok(agents)
}

/// Every Provider the enabled groups name, deduplicated.
///
/// This is the gateway's upstream set. It is read fresh rather than stored,
/// which is what keeps the gateway from owning a Provider list.
fn gateway_route_members_in(
    conn: &vibex_db::DbConnection,
) -> VibexResult<Vec<vibex_core::ModelProviderProfileId>> {
    let mut members = Vec::new();
    for route in vibex_db::ModelRouteRepository::list_enabled(conn)? {
        if !route.kind.via_gateway() {
            continue;
        }
        for member in route.enabled_members() {
            if !members.contains(&member.model_provider_profile_id) {
                members.push(member.model_provider_profile_id.clone());
            }
        }
    }
    Ok(members)
}

/// Whether a published model id belongs to one group.
///
/// A group publishes `group/<id>/<model>`, so membership is a prefix test on
/// the group id followed by a non-empty model. The slash matters: without it
/// `group/abc` would match `group/abcdef/model`.
fn group_catalog_belongs_to(model: &str, route: &vibex_core::ModelRoute) -> bool {
    let Some(parsed) = vibex_core::route_group_model_ref(model) else {
        return false;
    };
    parsed.group_id == route.id.as_str()
}

/// The catalogue entry a model-group member names.
///
/// A member may name the entry id directly or the local alias the Provider's
/// metadata maps to it. The upstream id is what capabilities and the gateway
/// request use, so the alias is resolved here rather than at call time.
fn member_catalog_entry<'a>(
    profile: &'a vibex_core::ModelProviderProfile,
    model: &str,
) -> Option<&'a vibex_core::ModelProviderCatalogEntry> {
    let model = model.trim();
    profile
        .configured_models
        .iter()
        .find(|entry| entry.enabled && entry.id == model)
        .or_else(|| {
            let upstream = profile
                .metadata
                .model_aliases
                .iter()
                .find(|alias| alias.alias == model)?
                .upstream
                .as_str();
            profile
                .configured_models
                .iter()
                .find(|entry| entry.enabled && entry.id == upstream)
        })
}

fn gateway_default_scope() -> vibex_core::ProviderProfileDefaultScope {
    vibex_core::ProviderProfileDefaultScope {
        kind: vibex_core::ProviderDefaultScopeKind::Global,
        project_id: None,
        workspace_id: None,
    }
}

fn gateway_display_name() -> String {
    "Vibex Gateway".to_string()
}

fn gateway_models(agent_id: &AgentId, catalog: &[String]) -> Vec<ProviderConfiguredModel> {
    // The gateway is the endpoint, so the Agent still speaks its own protocol
    // to it. The protocol is taken from the Agent's descriptor, which is the
    // only declaration of what it can actually send; asserting a fixed one here
    // would be refused by the same validation the editor uses.
    let wire_api = gateway_wire_api_for(agent_id);
    catalog
        .iter()
        .filter(|model| !model.trim().is_empty())
        .take(MAX_GATEWAY_MODELS)
        .map(|model| ProviderConfiguredModel {
            id: model.clone(),
            display_name: None,
            enabled: true,
            wire_api,
            capabilities: ProviderModelCapabilities::default(),
        })
        .collect()
}

/// The protocol an Agent should keep speaking after it is pointed at the
/// gateway, preferring the Responses surface where an Agent offers it.
fn gateway_wire_api_for(agent_id: &AgentId) -> Option<ProviderModelWireApi> {
    let registry = vibex_core::AgentProviderProjectionRegistry::builtin().ok()?;
    let supported = registry
        .descriptors_for_agent(agent_id)
        .flat_map(|descriptor| descriptor.model_interfaces.iter())
        .filter_map(|interface| {
            ProviderModelWireApi::from_wire_protocol_id(&interface.wire_protocol_id)
        })
        // The gateway serves exactly four inbound protocols. An Agent whose
        // only interface is Mistral, Bedrock, or Vertex cannot be pointed at
        // it: the address would accept a request shape it does not serve.
        .filter(|wire_api| {
            matches!(
                wire_api,
                ProviderModelWireApi::OpenaiResponses
                    | ProviderModelWireApi::OpenaiChatCompletions
                    | ProviderModelWireApi::AnthropicMessages
                    | ProviderModelWireApi::GoogleGenerativeAi
            )
        })
        .collect::<Vec<_>>();
    [
        ProviderModelWireApi::OpenaiResponses,
        ProviderModelWireApi::OpenaiChatCompletions,
    ]
    .into_iter()
    .into_iter()
    .find(|wire_api| supported.contains(wire_api))
    .or_else(|| supported.first().copied())
}

fn gateway_provider_options(agent_id: &AgentId, profile_id: &str) -> ProviderOptions {
    let mut options = ProviderOptions::empty();
    for (key, value) in [
        (GATEWAY_PROFILE_ROLE_OPTION_KEY, GATEWAY_PROFILE_ROLE),
        (GATEWAY_AGENT_OPTION_KEY, agent_id.as_str()),
        ("vibex.internal.gatewayProfileId", profile_id),
        // The gateway speaks every protocol, so the Agent's own protocol is
        // the one the projection should keep writing.
        ("modelGateway", "true"),
    ] {
        if value.is_empty() {
            continue;
        }
        options.entries.push(vibex_core::ProviderBindingMetadata {
            key: key.to_string(),
            value: value.to_string(),
        });
    }
    options
}

/// The kind every gateway Profile is written with.
///
/// Always `Acp`, never the Agent's own kind. `ProviderKind::Claude` and
/// `Codex` name the config file an Agent owns (`~/.claude/settings.json`,
/// `~/.codex/config.toml`), which is what the native importer needs; they are
/// not a launch contract. Both the option catalogue and the ACP launch path
/// accept `Acp` only — `build_runtime_option_catalog` skips every other kind,
/// and `runtime.rs` refuses a route Profile with
/// `provider_profile_route_unavailable` — so a gateway Profile written with
/// `Claude`/`Codex` was invisible: its route group reached the picker with no
/// models at all, and could not have started a session if it had.
///
/// This is the same invariant the shared-Provider binding path states
/// (`materialize_legacy_profile_for_agent` in `lib.rs`), and the reason adding a
/// Provider from either entry point has always produced `Acp`.
fn gateway_provider_kind(_agent_id: &AgentId) -> ProviderKind {
    ProviderKind::Acp
}

pub(crate) fn is_gateway_profile(profile: &ProviderProfile) -> bool {
    provider_option_value(&profile.provider_options, GATEWAY_PROFILE_ROLE_OPTION_KEY).as_deref()
        == Some(GATEWAY_PROFILE_ROLE)
}

fn gateway_profile_agent(profile: &ProviderProfile) -> Option<AgentId> {
    let agent_id = provider_option_value(&profile.provider_options, GATEWAY_AGENT_OPTION_KEY)?;
    AgentId::parse(agent_id).ok()
}

fn gateway_profile_for(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
) -> VibexResult<Option<ProviderProfile>> {
    let profiles = ProviderProfileRepository::list_by_agent(conn, agent_id, true)?;
    Ok(profiles.into_iter().find(is_gateway_profile))
}

/// The default Profile id an Agent is currently pointed at, resolved through
/// the same read the runtime uses.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn default_profile_for(
    conn: &vibex_db::DbConnection,
    agent_id: &AgentId,
) -> VibexResult<Option<ProviderProfileId>> {
    Ok(AgentDefaultModelProviderProfileRepository::get(
        conn,
        gateway_default_scope(),
        agent_id.clone(),
    )?
    .provider_profile_id)
}

/// Opens a connection for the gateway wiring in tests.
#[cfg(test)]
pub(crate) fn test_connection(path: &std::path::Path) -> VibexResult<vibex_db::DbConnection> {
    let mut conn = vibex_db::open_database(path)?;
    vibex_db::apply_migrations(&mut conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::RequestId;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-wiring-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn agent(id: &str) -> AgentId {
        AgentId::parse(id).unwrap()
    }

    /// Turns the installation-wide gateway switch on or off.
    fn set_gateway_switch(service: &ProviderConfigService, enabled: bool) {
        let conn = service.open_connection().unwrap();
        let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        let mut next = current.clone();
        next.policy.enabled = enabled;
        next.revision = current.revision + 1;
        vibex_db::ModelGatewaySettingsRepository::update(&conn, &next, current.revision).unwrap();
    }

    fn enable_gateway(service: &ProviderConfigService) {
        set_gateway_switch(service, true);
    }

    fn disable_gateway(service: &ProviderConfigService) {
        set_gateway_switch(service, false);
    }

    /// A shared Provider with one model, for a group to name.
    fn provider(service: &ProviderConfigService, name: &str) -> vibex_core::ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: name.to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// A group over the given Providers, bound to the given Agents.
    fn group_for(
        service: &ProviderConfigService,
        name: &str,
        providers: Vec<vibex_core::ModelProviderProfileId>,
        agents: &[&str],
    ) -> vibex_core::ModelRouteId {
        let route = service
            .create_model_route(vibex_core::ModelRoute {
                display_name: name.to_string(),
                kind: vibex_core::RouteKind::ModelGroup,
                status: vibex_core::RouteStatus::Enabled,
                members: providers
                    .into_iter()
                    .enumerate()
                    .map(|(index, provider)| {
                        vibex_core::ModelRouteMember::new(provider, "deepseek-chat", index as i64)
                    })
                    .collect(),
                ..Default::default()
            })
            .unwrap();
        for agent_id in agents {
            service
                .bind_model_route(&agent(agent_id), &route.id, false)
                .unwrap();
        }
        route.id
    }

    /// The catalogue a listener would publish, as display ids.
    fn catalog(service: &ProviderConfigService) -> Vec<String> {
        service
            .gateway_model_catalog()
            .unwrap()
            .into_iter()
            .map(|entry| entry.display_id())
            .collect()
    }

    /// Replacing a Key's secret revives it; clearing the secret does not.
    ///
    /// An authentication failure has no expiry, so the only way back for a
    /// rejected Key is a new value. The same write that stores it has to clear
    /// the state that keeps the Key out of rotation.
    #[test]
    fn replacing_a_key_clears_its_gateway_cooldown() {
        let (path, service) = service();
        let profile = vibex_core::ModelProviderProfile {
            display_name: "DeepSeek".to_string(),
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        {
            let conn = test_connection(&path).unwrap();
            vibex_db::ModelProviderProfileRepository::insert(&conn, &profile).unwrap();
        }
        let added = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: profile.id.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-original".to_string(),
            })
            .unwrap();
        assert_eq!(
            added.credentials[0].masked_hint,
            vibex_core::masked_secret_hint("sk-original")
        );
        let credential = added.credentials[0].id.clone();
        let conn = test_connection(&path).unwrap();
        let mut state =
            vibex_core::ModelProviderAccountState::ready(profile.id.clone(), credential.clone());
        state.cooldown_reason = Some(vibex_core::GatewayFailureKind::Auth);
        state.consecutive_failures = 1;
        vibex_db::ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();

        let replaced = service
            .mutate_provider_credential_secret(
                vibex_core::ProviderCredentialSecretMutationRequest {
                    model_provider_profile_id: profile.id.clone(),
                    credential_id: credential.clone(),
                    touched: true,
                    clear: false,
                    value: Some("sk-replacement".to_string()),
                },
            )
            .unwrap();
        assert_eq!(
            replaced.credentials[0].masked_hint,
            vibex_core::masked_secret_hint("sk-replacement")
        );
        let states =
            vibex_db::ModelProviderAccountStateRepository::list(&conn, &profile.id).unwrap();
        assert!(
            states[0].is_ready_at(vibex_core::unix_timestamp_ms()),
            "a replaced Key must be tried again: {:?}",
            states[0]
        );

        // Clearing is not a replacement: a Key with no value should not look
        // ready just because the cooldown was reset.
        let mut state =
            vibex_core::ModelProviderAccountState::ready(profile.id.clone(), credential.clone());
        state.cooldown_reason = Some(vibex_core::GatewayFailureKind::Auth);
        vibex_db::ModelProviderAccountStateRepository::upsert(&conn, &state).unwrap();
        let cleared = service
            .mutate_provider_credential_secret(
                vibex_core::ProviderCredentialSecretMutationRequest {
                    model_provider_profile_id: profile.id.clone(),
                    credential_id: credential.clone(),
                    touched: true,
                    clear: true,
                    value: None,
                },
            )
            .unwrap();
        assert!(cleared.credentials[0].masked_hint.is_empty());
        let states =
            vibex_db::ModelProviderAccountStateRepository::list(&conn, &profile.id).unwrap();
        assert!(!states[0].is_ready_at(vibex_core::unix_timestamp_ms()));
        let _ = std::fs::remove_file(path);
    }

    /// A Key's value leaves the runtime only through the explicit reveal.
    #[test]
    fn a_key_value_is_only_read_through_the_explicit_reveal() {
        let (path, service) = service();
        let profile = vibex_core::ModelProviderProfile {
            display_name: "DeepSeek".to_string(),
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        {
            let conn = test_connection(&path).unwrap();
            vibex_db::ModelProviderProfileRepository::insert(&conn, &profile).unwrap();
        }
        let secret = "sk-reveal-sentinel-1234";
        let added = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: profile.id.clone(),
                display_name: None,
                value: secret.to_string(),
            })
            .unwrap();
        let credential = added.credentials[0].id.clone();

        let listed = service.list_model_provider_profiles().unwrap();
        let listed_key = &listed[0].credentials[0];
        assert_eq!(
            listed_key.masked_hint,
            vibex_core::masked_secret_hint(secret)
        );
        assert!(
            !serde_json::to_string(&listed).unwrap().contains(secret),
            "an ordinary profile read must not carry the value"
        );

        let revealed = service
            .get_model_provider_credential_secret(
                vibex_core::ModelProviderCredentialSecretRequest {
                    model_provider_profile_id: profile.id.clone(),
                    credential_id: credential,
                },
            )
            .unwrap();
        assert_eq!(revealed.value.as_deref(), Some(secret));
        assert_eq!(revealed.masked_hint, listed_key.masked_hint);
        assert!(
            !format!("{revealed:?}").contains(secret),
            "the reveal response must not print its value in Debug"
        );
        let _ = std::fs::remove_file(path);
    }

    /// No binding means no Profile: the gateway serves nobody until an Agent is
    /// bound to a group.
    #[test]
    fn an_agent_without_a_group_binding_is_not_attached() {
        let (path, service) = service();
        enable_gateway(&service);
        provider(&service, "DeepSeek");

        let attached = service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(
            attached.is_empty(),
            "an installation with no group binding must attach nothing"
        );
        assert!(service.gateway_agents().unwrap().is_empty());
        assert!(service.gateway_active_agents().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A binding is what puts an Agent on the gateway, and the Profile carries
    /// only that Agent's groups.
    #[test]
    fn a_binding_attaches_only_its_own_agent_with_only_its_own_groups() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let kimi = provider(&service, "Kimi");
        let claude_group = group_for(&service, "Claude group", vec![deepseek], &["claude"]);
        let codex_group = group_for(&service, "Codex group", vec![kimi], &["codex"]);

        let attached = service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(attached.contains(&agent("claude")));
        assert!(attached.contains(&agent("codex")));
        assert!(
            !attached.contains(&agent("deepseek-harness")),
            "an unbound Agent must not be attached"
        );

        let conn = test_connection(&path).unwrap();
        let claude_profile = gateway_profile_for(&conn, &agent("claude"))
            .unwrap()
            .unwrap();
        let models = claude_profile
            .configured_models
            .iter()
            .map(|model| model.id.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            models,
            vec![format!("group/{}/Claude group", claude_group.as_str()).as_str()],
            "a Profile carries only the groups its Agent is bound to"
        );
        assert!(
            !models
                .iter()
                .any(|model| model.contains(codex_group.as_str())),
            "another Agent's group must not leak in"
        );
        assert_eq!(
            claude_profile.base_url.as_deref(),
            Some("http://127.0.0.1:3425")
        );
        assert_eq!(
            default_profile_for(&conn, &agent("claude")).unwrap(),
            Some(claude_profile.id.clone()),
            "the gateway Profile is the bound Agent's default"
        );
        drop(conn);

        // A hidden Profile must not appear in the Agent's visible provider list.
        let visible = service
            .list_agent_model_provider_profiles(vibex_core::AgentModelProviderProfileListRequest {
                agent_id: agent("claude"),
                include_disabled: true,
            })
            .unwrap();
        assert!(
            visible
                .profiles
                .iter()
                .all(|entry| !is_gateway_profile(&entry.profile))
        );
        let _ = std::fs::remove_file(path);
    }

    /// Binding a second group grows the same Profile rather than making a new
    /// one, and the Agent may hold several.
    #[test]
    fn binding_a_second_group_adds_it_to_the_same_profile() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let kimi = provider(&service, "Kimi");
        group_for(&service, "First", vec![deepseek], &["claude"]);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let conn = test_connection(&path).unwrap();
        let first = gateway_profile_for(&conn, &agent("claude"))
            .unwrap()
            .unwrap();
        drop(conn);

        group_for(&service, "Second", vec![kimi], &["claude"]);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let conn = test_connection(&path).unwrap();
        let second = gateway_profile_for(&conn, &agent("claude"))
            .unwrap()
            .unwrap();
        assert_eq!(first.id, second.id, "one hidden Profile per Agent");
        assert_eq!(
            second.configured_models.len(),
            2,
            "both bound groups are listed"
        );
        assert_eq!(
            service
                .list_agent_model_routes(&agent("claude"))
                .unwrap()
                .len(),
            2
        );
        let _ = std::fs::remove_file(path);
    }

    /// Unbinding an Agent's last group removes its Profile, so it falls back to
    /// its direct Provider instead of pointing at a gateway that no longer
    /// serves it.
    #[test]
    fn unbinding_the_last_group_removes_the_profile() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let route = group_for(&service, "Only", vec![deepseek], &["claude"]);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let conn = test_connection(&path).unwrap();
        assert!(
            gateway_profile_for(&conn, &agent("claude"))
                .unwrap()
                .is_some()
        );
        drop(conn);

        service
            .unbind_model_route(&agent("claude"), &route)
            .unwrap();
        let attached = service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert_eq!(attached, vec![agent("claude")]);
        assert!(service.gateway_active_agents().unwrap().is_empty());
        let conn = test_connection(&path).unwrap();
        assert_eq!(default_profile_for(&conn, &agent("claude")).unwrap(), None);
        let _ = std::fs::remove_file(path);
    }

    /// Disabling a group unpublishes it, and the bound Agent loses its Profile
    /// entry rather than keeping a model the gateway will refuse.
    #[test]
    fn a_disabled_group_leaves_the_agents_profile() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let route_id = group_for(&service, "Team", vec![deepseek], &["claude"]);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();

        let mut route = service.get_model_route(&route_id).unwrap().unwrap();
        route.status = vibex_core::RouteStatus::Disabled;
        let revision = route.revision;
        service.update_model_route(route, revision).unwrap();

        assert!(
            catalog(&service).is_empty(),
            "a disabled group offers no model"
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(service.gateway_active_agents().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A second attach updates the same hidden Profile and address.
    #[test]
    fn attaching_twice_updates_the_same_profiles() {
        let (path, service) = service();
        enable_gateway(&service);
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["codex"],
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let agent_id = agent("codex");
        let first = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        service
            .attach_gateway_to_agents("http://127.0.0.1:3426", &catalog(&service))
            .unwrap();
        let second = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(first.id, second.id);
        assert_eq!(second.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        let _ = std::fs::remove_file(path);
    }

    /// Detaching removes every hidden Profile.
    #[test]
    fn detaching_removes_every_hidden_profile() {
        let (path, service) = service();
        enable_gateway(&service);
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["codex", "claude"],
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(!service.gateway_active_agents().unwrap().is_empty());

        service.detach_gateway_from_agents().unwrap();
        assert!(service.gateway_active_agents().unwrap().is_empty());
        let conn = test_connection(&path).unwrap();
        let remaining = ProviderProfileRepository::list_all(&conn)
            .unwrap()
            .into_iter()
            .filter(is_gateway_profile)
            .count();
        assert_eq!(remaining, 0, "no hidden Profile may survive the switch");
        let _ = std::fs::remove_file(path);
    }

    /// The gateway's intent is global: off means no Agent reports using it, and
    /// the bindings alone decide who is on it when it is on.
    #[test]
    fn the_gateway_intent_follows_the_global_switch() {
        let (path, service) = service();
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["codex"],
        );

        disable_gateway(&service);
        assert!(
            service.gateway_agents().unwrap().is_empty(),
            "a disabled gateway serves no Agent"
        );
        enable_gateway(&service);
        assert_eq!(
            service
                .gateway_agents()
                .unwrap()
                .into_iter()
                .map(|agent| agent.as_str().to_string())
                .collect::<Vec<_>>(),
            vec!["codex".to_string()],
            "only the bound Agent is served"
        );
        disable_gateway(&service);
        assert!(service.gateway_agents().unwrap().is_empty());
        let _ = std::fs::remove_file(path);
    }

    /// A stopped listener removes the Profiles while the bindings stay, so a
    /// working listener restores them without asking again.
    #[test]
    fn a_stopped_gateway_removes_profiles_but_keeps_the_bindings() {
        let (path, service) = service();
        enable_gateway(&service);
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["codex"],
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let agent_id = agent("codex");
        let conn = test_connection(&path).unwrap();
        assert!(default_profile_for(&conn, &agent_id).unwrap().is_some());
        drop(conn);

        let deactivated = service.deactivate_gateway_profiles().unwrap();
        assert!(deactivated.contains(&agent_id));
        assert!(service.gateway_active_agents().unwrap().is_empty());
        // The binding is still there, so a working listener restores it.
        assert!(!service.gateway_agents().unwrap().is_empty());
        let conn = test_connection(&path).unwrap();
        assert_eq!(default_profile_for(&conn, &agent_id).unwrap(), None);
        drop(conn);

        service
            .refresh_gateway_profiles("http://127.0.0.1:3426", &catalog(&service))
            .unwrap();
        assert!(service.gateway_active_agents().unwrap().contains(&agent_id));
        let conn = test_connection(&path).unwrap();
        let default = default_profile_for(&conn, &agent_id).unwrap().unwrap();
        let profile = service.get_profile(&default).unwrap().unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        let _ = std::fs::remove_file(path);
    }

    /// An Agent that owns its provider is never attached, even when a group
    /// names it.
    #[test]
    fn an_agent_that_owns_its_provider_is_never_attached() {
        let (path, service) = service();
        enable_gateway(&service);
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["cursor"],
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(
            !service
                .gateway_active_agents()
                .unwrap()
                .contains(&agent("cursor")),
            "an Agent that manages its own provider cannot use the gateway"
        );
        let _ = std::fs::remove_file(path);
    }

    /// Attaching without a listener address is refused.
    #[test]
    fn attaching_without_an_address_is_refused() {
        let (path, service) = service();
        enable_gateway(&service);
        let error = service.attach_gateway_to_agents("", &[]).unwrap_err();
        assert_eq!(error.code, "model_gateway_unavailable");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn refreshing_tracks_the_listener_address() {
        let (path, service) = service();
        let agent_id = agent("codex");
        enable_gateway(&service);
        provider(&service, "DeepSeek");
        group_for(
            &service,
            "Team",
            vec![provider(&service, "Kimi")],
            &["codex"],
        );
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3426", &catalog(&service))
            .unwrap();
        assert!(refreshed.contains(&agent_id));
        let profile = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .unwrap();
        assert_eq!(profile.base_url.as_deref(), Some("http://127.0.0.1:3426"));
        // A second call with the same address is a no-op, so it cannot churn
        // the projection fingerprint.
        assert!(
            service
                .refresh_gateway_profiles("http://127.0.0.1:3426", &catalog(&service))
                .unwrap()
                .is_empty()
        );
        let _ = std::fs::remove_file(path);
    }

    /// Publishing a route group is what puts it in an Agent's model picker.
    ///
    /// A session reads its selectable models from the Agent's gateway Profile,
    /// which is a copy of the catalogue taken when the Profile is refreshed. A
    /// group that is created and bound but never republished therefore exists,
    /// is bound, and still does not appear in the picker — the failure this test
    /// pins down.
    #[test]
    fn a_bound_group_appears_in_the_agents_profile_only_after_republishing() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let agent_id = agent("claude");

        let group = service
            .create_model_route(vibex_core::ModelRoute {
                display_name: "Team".to_string(),
                kind: vibex_core::RouteKind::ModelGroup,
                status: vibex_core::RouteStatus::Enabled,
                members: vec![vibex_core::ModelRouteMember::new(
                    deepseek,
                    "deepseek-chat",
                    0,
                )],
                ..Default::default()
            })
            .unwrap();

        // The group is in the catalogue the gateway would serve...
        let published = catalog(&service);
        assert_eq!(published, vec![format!("group/{}/Team", group.id.as_str())]);

        // ...but until it is bound and republished, the Agent has no Profile at
        // all, which is exactly what made the picker look empty.
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &published)
            .unwrap();
        assert!(
            gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
                .unwrap()
                .is_none(),
            "an unbound Agent has no gateway Profile"
        );

        // Binding is what grants the model.
        service
            .bind_model_route(&agent_id, &group.id, false)
            .unwrap();
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(
            refreshed.contains(&agent_id),
            "the refresh must report the Agent it changed, not silently skip it"
        );
        let conn = test_connection(&path).unwrap();
        let profile_id = default_profile_for(&conn, &agent_id).unwrap().unwrap();
        let after = service.get_profile(&profile_id).unwrap().unwrap();
        assert!(
            after
                .configured_models
                .iter()
                .any(|model| model.id == format!("group/{}/Team", group.id.as_str())),
            "the republished Profile is what a session reads its models from"
        );
        let _ = std::fs::remove_file(path);
    }

    /// A gateway Profile is always `Acp`, whatever the Agent calls itself.
    ///
    /// `ProviderKind::Claude`/`Codex` name an Agent's own config file, not a
    /// launch contract: the option catalogue skips every Profile that is not
    /// `Acp`, and the ACP runtime refuses to launch one. Writing the Agent's
    /// own kind here is what made a bound route group show no models for claude
    /// and codex while the same group worked for deepseek-harness.
    #[test]
    fn a_gateway_profile_is_written_with_the_acp_kind_for_every_agent() {
        let (path, service) = service();
        enable_gateway(&service);
        let deepseek = provider(&service, "DeepSeek");
        let published = catalog(&service);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &published)
            .unwrap();

        for agent_id in ["claude", "codex", "deepseek-harness"] {
            let agent_id = agent(agent_id);
            let group_name = format!("Team {}", agent_id.as_str());
            group_for(
                &service,
                &group_name,
                vec![deepseek.clone()],
                &[agent_id.as_str()],
            );
            service
                .refresh_gateway_profiles("http://127.0.0.1:3425", &catalog(&service))
                .unwrap();
            let conn = test_connection(&path).unwrap();
            let profile_id = default_profile_for(&conn, &agent_id)
                .unwrap()
                .expect("a bound Agent has a gateway Profile");
            let profile = service.get_profile(&profile_id).unwrap().unwrap();
            assert_eq!(
                profile.kind,
                ProviderKind::Acp,
                "{} must get a launchable kind, not its config-file kind",
                agent_id.as_str()
            );
        }
        let _ = std::fs::remove_file(path);
    }

    /// An ACP Agent's gateway Profile accepts a new catalogue.
    ///
    /// `update_profile` refuses an ACP Profile whose runtime configuration was
    /// not supplied, and a gateway Profile's options never carry one — it is
    /// derived. Refreshing through the generic update therefore failed with
    /// `acp_config_missing`, the per-Agent `if let Ok` swallowed it, and the
    /// catalogue never reached the Profile. Only an ACP-kind Agent (anything
    /// but `claude`/`codex`) exercises that path.
    #[test]
    fn an_acp_agents_gateway_profile_accepts_a_new_catalogue() {
        let (path, service) = service();
        enable_gateway(&service);
        let agent_id = agent("deepseek-harness");
        let deepseek = provider(&service, "DeepSeek");
        let first = group_for(&service, "First", vec![deepseek], &["deepseek-harness"]);
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        let conn = test_connection(&path).unwrap();
        let profile_id = default_profile_for(&conn, &agent_id)
            .unwrap()
            .expect("the ACP Agent is on the gateway");
        drop(conn);
        let before = service.get_profile(&profile_id).unwrap().unwrap();
        assert_eq!(
            before
                .configured_models
                .iter()
                .map(|model| model.id.as_str())
                .collect::<Vec<_>>(),
            vec![format!("group/{}/First", first.as_str()).as_str()],
            "the gateway Profile starts with what was published"
        );

        // A second group arrives, so the catalogue changes.
        let kimi = provider(&service, "Kimi");
        group_for(&service, "Second", vec![kimi], &["deepseek-harness"]);
        let refreshed = service
            .refresh_gateway_profiles("http://127.0.0.1:3425", &catalog(&service))
            .unwrap();
        assert!(
            refreshed.contains(&agent_id),
            "an ACP Agent must be updated, not skipped for a missing ACP config"
        );
        let after = service.get_profile(&profile_id).unwrap().unwrap();
        assert_eq!(
            after.configured_models.len(),
            2,
            "the Profile now lists both groups' models"
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod fingerprint_tests {
    use super::*;
    use vibex_core::{
        AgentCredential, AgentCredentialStatus, ProjectionSecretReference, RequestId,
    };

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-fingerprint-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// Turns the installation-wide gateway switch on.
    fn enable_gateway(service: &ProviderConfigService) {
        let conn = service.open_connection().unwrap();
        let current = vibex_db::ModelGatewaySettingsRepository::get_or_create(&conn).unwrap();
        let mut next = current.clone();
        next.policy.enabled = true;
        next.revision = current.revision + 1;
        vibex_db::ModelGatewaySettingsRepository::update(&conn, &next, current.revision).unwrap();
    }

    /// Creates a shared upstream with one stored Key and returns its id.
    fn upstream(
        service: &ProviderConfigService,
        display_name: &str,
    ) -> vibex_core::ModelProviderProfileId {
        let lookup_key = format!("vibex-test-secret-{}", RequestId::new().as_str());
        crate::secrets::store_provider_secret(&lookup_key, "sk-test").unwrap();
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: display_name.to_string(),
                vendor_hint: None,
                endpoints: vec![vibex_core::ModelProviderEndpoint {
                    id: "endpoint-api".to_string(),
                    kind: vibex_core::ModelProviderEndpointKind::Api,
                    url: "https://api.example.invalid".to_string(),
                    wire_protocol_id: Some(
                        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    ),
                }],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: vec![vibex_core::ModelProviderCredentialReference {
                    id: RequestId::new(),
                    display_name: "Primary".to_string(),
                    masked_hint: String::new(),
                    status: AgentCredentialStatus::Ready,
                    credential: AgentCredential::ApiKey {
                        secret: ProjectionSecretReference {
                            id: RequestId::new(),
                            backend: crate::secrets::provider_secret_write_backend(),
                            setup_state: vibex_core::ProviderSecretSetupState::Available,
                            lookup_key,
                            redacted_hint: "stored".to_string(),
                            revision: 1,
                            legacy_secret_reference_id: None,
                        },
                        target_hint: None,
                    },
                    revision: 1,

                    models: Vec::new(),
                    proxy_url: None,
                }],
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    /// The promise the whole gateway rests on: changing an **upstream's** Key
    /// must not move the gateway-bound Agent's launch contract, so it does not
    /// restart that Agent's process.
    ///
    /// The Agent's own Profile holds only the gateway address, so an upstream
    /// Key rotation never reaches it. This test asserts exactly that: the
    /// upstream's credential revision moves, the Agent Profile's launch
    /// revision does not.
    #[test]
    fn rotating_an_upstream_key_does_not_move_the_agent_launch_revision() {
        let (path, service) = service();
        let agent_id = vibex_core::AgentId::parse("codex").unwrap();
        let shared = upstream(&service, "DeepSeek");

        // Point the Agent at the gateway through a group over that Provider.
        enable_gateway(&service);
        service
            .create_model_route(vibex_core::ModelRoute {
                display_name: "Team".to_string(),
                kind: vibex_core::RouteKind::ModelGroup,
                status: vibex_core::RouteStatus::Enabled,
                members: vec![vibex_core::ModelRouteMember::new(
                    shared.clone(),
                    "deepseek-chat",
                    0,
                )],
                ..Default::default()
            })
            .map(|route| {
                service
                    .bind_model_route(&agent_id, &route.id, false)
                    .unwrap()
            })
            .unwrap();
        let catalog = service
            .gateway_model_catalog()
            .unwrap()
            .into_iter()
            .map(|entry| entry.display_id())
            .collect::<Vec<_>>();
        service
            .attach_gateway_to_agents("http://127.0.0.1:3425", &catalog)
            .unwrap();
        let agent_profile = gateway_profile_for(&test_connection(&path).unwrap(), &agent_id)
            .unwrap()
            .expect("a gateway profile")
            .id;
        let before = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();

        // Rotate every Key on the shared upstream, which is what a user does
        // when a Key is exhausted.
        let mut shared_profile = service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| profile.id == shared)
            .unwrap();
        for credential in &mut shared_profile.credentials {
            credential.revision = credential.revision.saturating_add(1);
            if let Some(reference) = credential.credential.secret_reference().cloned() {
                let mut next = reference;
                next.revision = next.revision.saturating_add(1);
                // The secret value is untouched; only its reference revision
                // moves, exactly as a Key replacement would move it.
                credential.credential = AgentCredential::ApiKey {
                    secret: next,
                    target_hint: None,
                };
            }
        }
        let expected = shared_profile.revision;
        shared_profile.revision = expected + 1;
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: shared_profile,
                expected_revision: expected,
            })
            .unwrap();

        let after = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();
        assert_eq!(
            before, after,
            "an upstream Key change moved the gateway-bound Agent's launch revision, \
             which would restart its process"
        );

        // And the gateway address itself does move it: moving the listener port
        // is a relaunch, which is why the port is persisted rather than re-picked.
        service
            .attach_gateway_to_agents(
                "http://127.0.0.1:3426",
                &service
                    .gateway_model_catalog()
                    .unwrap()
                    .into_iter()
                    .map(|entry| entry.display_id())
                    .collect::<Vec<_>>(),
            )
            .unwrap();
        let moved = service
            .get_profile(&agent_profile)
            .unwrap()
            .unwrap()
            .launch_revision();
        assert_ne!(
            before, moved,
            "moving the gateway's address must count as a relaunch"
        );

        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod credential_tests {
    use super::*;
    use vibex_core::RequestId;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-gateway-credential-{}.db",
            RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn profile_id(service: &ProviderConfigService) -> vibex_core::ModelProviderProfileId {
        service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: vec![vibex_core::ModelProviderEndpoint {
                    id: "endpoint-api".to_string(),
                    kind: vibex_core::ModelProviderEndpointKind::Api,
                    url: "https://api.example.invalid".to_string(),
                    wire_protocol_id: Some(
                        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
                    ),
                }],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap()
            .id
    }

    #[test]
    fn a_key_can_be_added_and_removed_without_storing_plaintext() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let profile = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-secret-value".to_string(),
            })
            .unwrap();
        assert_eq!(profile.credentials.len(), 1);
        assert_eq!(profile.credentials[0].display_name, "Primary");
        assert_eq!(
            profile.credentials[0].status,
            vibex_core::AgentCredentialStatus::Ready
        );
        // The row holds a reference, never the value.
        let encoded = serde_json::to_string(&profile).unwrap();
        assert!(
            !encoded.contains("sk-secret-value"),
            "the profile row carried the key value"
        );

        let credential_id = profile.credentials[0].id.clone();
        let profile = service
            .remove_model_provider_credential(vibex_core::RemoveModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                credential_id,
            })
            .unwrap();
        assert!(profile.credentials.is_empty());
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn an_empty_key_is_refused() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let error = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                display_name: None,
                value: "   ".to_string(),
            })
            .unwrap_err();
        assert_eq!(error.code, "model_provider_credential_empty");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn removing_an_unknown_key_is_a_named_error() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let error = service
            .remove_model_provider_credential(vibex_core::RemoveModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                credential_id: RequestId::new(),
            })
            .unwrap_err();
        assert_eq!(error.code, "model_provider_credential_not_found");
        let _ = std::fs::remove_file(path);
    }

    #[test]
    fn each_added_key_advances_the_revision_so_bindings_restale() {
        let (path, service) = service();
        let provider = profile_id(&service);
        let first = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider.clone(),
                display_name: None,
                value: "sk-one".to_string(),
            })
            .unwrap();
        let second = service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: provider,
                display_name: None,
                value: "sk-two".to_string(),
            })
            .unwrap();
        assert!(second.revision > first.revision);
        assert_eq!(second.credentials.len(), 2);
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod legacy_mirror_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-legacy-mirror-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// A legacy Profile with no ACP config: Claude's kind, so the projection
    /// accepts it without the typed ACP contract an `Acp` Profile requires.
    fn legacy_profile(service: &ProviderConfigService) -> ProviderProfileId {
        service
            .create_profile(vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse("claude").unwrap()),
                kind: ProviderKind::Claude,
                display_name: "Mirror Probe".to_string(),
                account_alias: None,
                base_url: Some("https://api.example.invalid".to_string()),
                default_model: Some("m1".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![vibex_core::ProviderConfiguredModel {
                    id: "m1".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: Some(vibex_core::ProviderOptions {
                    schema_version: 1,
                    entries: vec![vibex_core::ProviderBindingMetadata {
                        key: "wireApi".to_string(),
                        value: "anthropic_messages".to_string(),
                    }],
                }),
                secret_references: Vec::new(),
            })
            .unwrap()
            .id
    }

    fn shared(
        service: &ProviderConfigService,
        legacy: &ProviderProfileId,
    ) -> vibex_core::ModelProviderProfile {
        service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|profile| profile.legacy_provider_profile_id.as_ref() == Some(legacy))
            .expect("a legacy create projects a shared Profile")
    }

    /// The regression this module exists for: a Key added through v2 must not
    /// vanish when the same Provider is next written through v1.
    #[test]
    fn a_v2_key_survives_a_later_v1_write() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        service
            .add_model_provider_credential(vibex_core::AddModelProviderCredentialRequest {
                model_provider_profile_id: profile.id.clone(),
                display_name: Some("Primary".to_string()),
                value: "sk-mirror-value".to_string(),
            })
            .unwrap();

        // The legacy row is what the launch path resolves a Key from.
        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(
            mirrored.secrets.len(),
            1,
            "a v2 Key must reach the legacy secret table"
        );

        service
            .update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: legacy_id.clone(),
                display_name: Some("Renamed from the Agent page".to_string()),
                status: None,
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

        let after = service
            .list_model_provider_profiles()
            .unwrap()
            .into_iter()
            .find(|candidate| candidate.id == profile.id)
            .unwrap();
        assert_eq!(
            after.credentials.len(),
            1,
            "a v1 write dropped the Key the v2 API added"
        );
        let _ = std::fs::remove_file(path);
    }

    /// A rename and a status change made on the global page must reach the
    /// record the launch path reads, and must not be undone by a v1 write.
    #[test]
    fn a_v2_rename_and_status_reach_the_legacy_record() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        let mut next = profile.clone();
        next.display_name = "Renamed on the global page".to_string();
        next.status = vibex_core::ModelProviderProfileStatus::Disabled;
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: next,
                expected_revision: profile.revision,
            })
            .unwrap();

        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(mirrored.display_name, "Renamed on the global page");
        assert_eq!(mirrored.status, ProviderProfileStatus::Disabled);

        // A v1 write restates the same values, so the name survives it rather
        // than snapping back to whatever v1 last held.
        service
            .update_profile(vibex_core::ProviderProfileUpdateRequest {
                provider_profile_id: legacy_id.clone(),
                display_name: None,
                status: None,
                account_alias: None,
                base_url: Some("https://moved.example.invalid".to_string()),
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
        let after = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(after.display_name, "Renamed on the global page");
        assert_eq!(after.status, ProviderProfileStatus::Disabled);
        let _ = std::fs::remove_file(path);
    }

    /// The projection must not strip what v2 cannot express. The protocol pin
    /// and the ACP config live only in `provider_options`, and losing them
    /// would silently re-point the Agent at the wrong API.
    #[test]
    fn the_projection_keeps_the_options_v2_cannot_express() {
        let (path, service) = service();
        let legacy_id = legacy_profile(&service);
        let profile = shared(&service, &legacy_id);

        let mut next = profile.clone();
        next.display_name = "Only the name moves".to_string();
        service
            .update_model_provider_profile(vibex_core::ModelProviderProfileUpdateRequest {
                profile: next,
                expected_revision: profile.revision,
            })
            .unwrap();

        let mirrored = service.get_profile(&legacy_id).unwrap().unwrap();
        assert_eq!(
            provider_option_value(&mirrored.provider_options, "wireApi").as_deref(),
            Some("anthropic_messages"),
            "the protocol pin was lost"
        );
        // And the Models keep their declared protocol rather than reverting to
        // "inherit", which the projection would read as the Provider default.
        assert_eq!(
            mirrored.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod bind_to_agent_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-bind-agent-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn profile(service: &ProviderConfigService, agent: &str) -> ProviderProfile {
        service
            .create_profile(vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(vibex_core::AgentId::parse(agent).unwrap()),
                kind: ProviderKind::Claude,
                display_name: format!("Shared for {agent}"),
                account_alias: None,
                base_url: Some("https://api.example.invalid".to_string()),
                default_model: Some("m1".to_string()),
                small_model: None,
                large_model: None,
                configured_models: vec![vibex_core::ProviderConfiguredModel {
                    id: "m1".to_string(),
                    display_name: None,
                    enabled: true,
                    wire_api: Some(vibex_core::ProviderModelWireApi::AnthropicMessages),
                    capabilities: Default::default(),
                }],
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
                secret_references: Vec::new(),
            })
            .unwrap()
    }

    /// One Provider reaches a second Agent without re-entering its address,
    /// protocol or Key, and each Agent keeps its own launch row.
    #[test]
    fn a_provider_can_be_given_to_a_second_agent() {
        let (path, service) = service();
        let source = profile(&service, "claude");
        assert_eq!(source.agent_id.as_str(), "claude");

        let bound = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: source.id.clone(),
                model_provider_profile_id: None,
                agent_id: vibex_core::AgentId::parse("deepseek-harness").unwrap(),
                make_default: false,
            })
            .unwrap();

        assert_eq!(bound.agent_id.as_str(), "deepseek-harness");
        assert_ne!(bound.id, source.id, "the copy must be its own Profile");
        assert_eq!(bound.base_url, source.base_url);
        assert_eq!(
            bound.configured_models.len(),
            source.configured_models.len()
        );
        // The two rows keep independent identities, which is what lets each
        // Agent launch with its own protocol.
        assert!(bound.secrets.iter().all(|bound_secret| {
            source
                .secrets
                .iter()
                .all(|source_secret| source_secret.id != bound_secret.id)
        }));
        let _ = std::fs::remove_file(path);
    }

    /// Binding the Provider an Agent already holds is refused rather than
    /// silently creating a duplicate row for the same Agent.
    #[test]
    fn binding_a_provider_to_its_own_agent_is_refused() {
        let (path, service) = service();
        let source = profile(&service, "claude");
        let error = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: source.id.clone(),
                model_provider_profile_id: None,
                agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                make_default: false,
            })
            .unwrap_err();
        assert_eq!(error.code, "agent_model_provider_binding_same_agent");
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod shared_provider_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-shared-provider-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    fn endpoint(id: &str, url: &str, protocol: Option<&str>) -> vibex_core::ModelProviderEndpoint {
        vibex_core::ModelProviderEndpoint {
            id: id.to_string(),
            kind: vibex_core::ModelProviderEndpointKind::Api,
            url: url.to_string(),
            wire_protocol_id: protocol.map(str::to_string),
        }
    }

    /// A shared Provider is created without naming an Agent, and it carries one
    /// address per protocol.
    ///
    /// This is the regression the global Providers page hit: creating there
    /// borrowed an Agent, so a Provider whose protocol that Agent could not
    /// speak was refused at save time with
    /// `agent_model_interface_unsupported`, even though nothing about the
    /// Provider itself was wrong.
    #[test]
    fn a_shared_provider_is_created_without_an_agent() {
        let (path, service) = service();
        let created = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: vec![
                    endpoint("api", "https://api.deepseek.com/v1", None),
                    endpoint(
                        "api-anthropic",
                        "https://api.deepseek.com/anthropic",
                        Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES),
                    ),
                ],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "deepseek-chat".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("deepseek-chat".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();

        // No Agent, and both protocols are addressable on the one record.
        assert!(created.legacy_provider_profile_id.is_none());
        assert_eq!(created.endpoints.len(), 2);
        let protocols = created
            .endpoints
            .iter()
            .filter_map(|endpoint| endpoint.wire_protocol_id.as_deref())
            .collect::<Vec<_>>();
        assert_eq!(
            protocols,
            vec![vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES]
        );
        let _ = std::fs::remove_file(path);
    }

    /// The protocol check moves to the binding, which is the first moment an
    /// Agent exists to have the limitation.
    #[test]
    fn a_shared_provider_is_checked_against_the_agent_that_binds_it() {
        let (path, service) = service();
        let created = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared Anthropic".to_string(),
                vendor_hint: None,
                endpoints: vec![endpoint(
                    "api-anthropic",
                    "https://api.anthropic.com",
                    Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES),
                )],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();
        // Creating it needed no Agent and was accepted.
        assert_eq!(created.display_name, "Shared Anthropic");
        let _ = std::fs::remove_file(path);
    }
}

#[cfg(test)]
mod shared_bind_tests {
    use super::*;

    fn service() -> (std::path::PathBuf, ProviderConfigService) {
        let path = std::env::temp_dir().join(format!(
            "vibex-shared-bind-{}.db",
            vibex_core::RequestId::new().as_str()
        ));
        let service = ProviderConfigService::new(&path);
        (path, service)
    }

    /// A Provider created on the global page can be given to an Agent, which
    /// materialises the legacy row the launch path needs.
    #[test]
    fn a_shared_provider_materialises_for_the_agent_that_binds_it() {
        let (path, service) = service();
        let shared = service
            .create_model_provider_profile(vibex_core::ModelProviderProfileCreateRequest {
                display_name: "Shared Messages".to_string(),
                vendor_hint: None,
                endpoints: vec![
                    vibex_core::ModelProviderEndpoint {
                        id: "api".to_string(),
                        kind: vibex_core::ModelProviderEndpointKind::Api,
                        url: "https://api.example.invalid".to_string(),
                        wire_protocol_id: None,
                    },
                    vibex_core::ModelProviderEndpoint {
                        id: "api-anthropic".to_string(),
                        kind: vibex_core::ModelProviderEndpointKind::Api,
                        url: "https://api.example.invalid/anthropic".to_string(),
                        wire_protocol_id: Some(
                            vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string(),
                        ),
                    },
                ],
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: vec![vibex_core::ModelProviderCatalogEntry {
                    id: "claude-sonnet-4-5".to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                }],
                default_model_id: Some("claude-sonnet-4-5".to_string()),
                headers: Vec::new(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
            })
            .unwrap();

        let bound = service
            .bind_provider_to_agent(vibex_core::AgentModelProviderBindRequest {
                provider_profile_id: vibex_core::ProviderProfileId::new(),
                model_provider_profile_id: Some(shared.id.clone()),
                agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                make_default: false,
            })
            .unwrap();

        assert_eq!(bound.agent_id.as_str(), "claude");
        // The protocol the shared record published is what the Agent launches
        // with, and the per-protocol address survived as an option.
        assert_eq!(
            provider_option_value(&bound.provider_options, "wireApi").as_deref(),
            Some(vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES)
        );
        assert_eq!(
            provider_option_value(
                &bound.provider_options,
                &vibex_core::ProviderModelWireApi::AnthropicMessages.protocol_base_url_option_key()
            )
            .as_deref(),
            Some("https://api.example.invalid/anthropic")
        );
        assert_eq!(
            bound.configured_models[0].wire_api,
            Some(vibex_core::ProviderModelWireApi::AnthropicMessages)
        );
        let _ = std::fs::remove_file(path);
    }
}

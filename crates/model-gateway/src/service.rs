//! The gateway service: configuration, listener lifecycle, and one request.
//!
//! The service is owned by the desktop runtime. It reads durable configuration
//! from the same database the Config Center writes, keeps only request-level
//! state in memory, and binds one loopback port. Binding failure never fails
//! runtime startup: the status becomes [`ModelGatewayStatus::Unavailable`], the
//! UI says so, and Agents keep their direct Provider configuration.

use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, RwLock};

use axum::body::Body;
use axum::http::{HeaderMap, StatusCode, header};
use axum::response::Response;
use chrono::{Datelike, Timelike};
use futures_util::StreamExt;
use serde_json::Value;
use tokio::sync::mpsc;

use vibex_config_switch::{ProviderConfigService, secrets};
use vibex_core::{
    GatewayConversationPin, GatewayFailureKind, GatewayPinScope, GatewayPolicy,
    GatewayRequestOutcome, GatewayRouteAttempt, GatewayRouteTrace, GatewayRoutingStrategy,
    GatewayTokenWindow, GatewayTokenWindowUsage, ModelGatewayRequestLog, ModelGatewayRuntimeState,
    ModelGatewaySettings, ModelGatewayStatus, ModelProviderAccountState,
    ModelProviderCredentialReference, ModelProviderEndpoint, ModelProviderEndpointKind,
    ModelProviderProfileId, ModelProviderProfileStatus, RequestId,
};
use vibex_db::{
    DbConnection, GatewayConversationPinRepository, GatewayEndpointProtocolRepository,
    GatewayTokenWindowRepository, ModelGatewayRequestLogRepository, ModelGatewaySettingsRepository,
    ModelProviderAccountStateRepository, apply_migrations, open_database,
};

use crate::error::{GatewayError, GatewayResult};
use crate::ir::{CanonicalEvent, CanonicalRequest, CanonicalUsage, ReasoningRequest};
use crate::route::{
    CandidateKey, KeyHealth, RouteRequest, classify_failure, conversation_key,
    is_protocol_mismatch, plan_route,
};
use crate::stream::{ReplyAccumulator, SseBuffer, UsageSniffer};
use crate::tool_names::ToolNameMap;
use crate::wire::parse_reply;
use crate::wire::{
    StreamDecoder, StreamEncoder, WireProtocol, build_upstream_request, decoder, encoder,
    parse_request, render_response, sse_frame,
};

/// A resolved upstream: one shared Provider Profile the gateway may call.
#[derive(Debug, Clone)]
pub struct GatewayUpstream {
    pub profile_id: ModelProviderProfileId,
    pub display_name: String,
    pub endpoints: Vec<ModelProviderEndpoint>,
    pub models: Vec<String>,
    pub metadata: vibex_core::ModelProviderMetadata,
    pub credentials: Vec<ModelProviderCredentialReference>,
    pub keys: Vec<GatewayKeySummary>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeySummary {
    pub credential_id: RequestId,
    pub display_name: String,
    pub masked_hint: String,
    pub state: vibex_core::GatewayKeyState,
    pub used_tokens: u64,
    pub models: Vec<String>,
    pub proxy_url: Option<String>,
}

impl GatewayUpstream {
    /// The endpoint this protocol should call, if the upstream publishes one.
    ///
    /// Only an endpoint that names this protocol counts. An endpoint that
    /// declares none is a single address whose API is unknown, and returning it
    /// for every protocol turned "I do not know" into "I speak all four" — the
    /// bug that sent an OpenAI Responses body to a Chat Completions endpoint and
    /// then cooled the Key when the vendor said so.
    pub fn endpoint_for(&self, wire_protocol_id: &str) -> Option<&ModelProviderEndpoint> {
        self.endpoints.iter().find(|endpoint| {
            endpoint.kind == ModelProviderEndpointKind::Api
                && endpoint.wire_protocol_id.as_deref() == Some(wire_protocol_id)
        })
    }

    /// The untyped address a protocol should be *attempted* on.
    ///
    /// It is a guess the caller has to verify, never a statement of support.
    pub fn untyped_endpoint(&self) -> Option<&ModelProviderEndpoint> {
        self.endpoints.iter().find(|endpoint| {
            endpoint.kind == ModelProviderEndpointKind::Api && endpoint.wire_protocol_id.is_none()
        })
    }

    /// The endpoint to call for one protocol.
    ///
    /// A typed endpoint decides; an untyped one is the last resort, and the only
    /// caller that may use it is the one that verifies the guess by trying.
    pub fn endpoint_for_call(&self, protocol: WireProtocol) -> Option<&ModelProviderEndpoint> {
        self.endpoint_for(protocol.wire_protocol_id())
            .or_else(|| self.untyped_endpoint())
    }

    /// Whether this upstream states that it speaks `protocol`.
    ///
    /// An untyped endpoint is not a statement: it is an address whose protocol
    /// the gateway has yet to observe, so it never counts here.
    pub fn supports(&self, protocol: WireProtocol) -> bool {
        self.endpoint_for(protocol.wire_protocol_id()).is_some()
    }
}

/// Everything the gateway needs to answer one request, read fresh.
#[derive(Debug, Clone)]
pub struct GatewaySnapshot {
    pub settings: ModelGatewaySettings,
    pub upstreams: Vec<GatewayUpstream>,
    pub gateway_used_tokens: u64,
    /// Enabled groups, by id. Every gateway model is addressed through one.
    pub groups: BTreeMap<String, GatewayRouteGroup>,
}

/// One route group as the gateway serves it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRouteGroup {
    pub group_id: String,
    pub display_name: String,
    pub strategy: vibex_core::RouteStrategy,
    /// How this group wants one Provider's Keys tried.
    ///
    /// It is the group's strategy, applied inside the chosen member: a group
    /// that rotates its Providers should rotate their Keys too, and one that
    /// prefers a fixed order should keep it at both levels. Making it a second
    /// setting would let a group claim an order it does not honour.
    pub key_strategy: GatewayRoutingStrategy,
    pub first_token_timeout_ms: Option<u64>,
    pub pin: vibex_core::GatewayPinScope,
    /// What happens when the member a conversation is pinned to fails.
    pub failover: vibex_core::GatewayFailover,
    pub token_limit: Option<vibex_core::GatewayTokenLimit>,
    /// The members in attempt order, already filtered to the ones that exist.
    ///
    /// A member carries the upstream model it calls, so two members of one
    /// Provider may name different models.
    pub providers: Vec<crate::route::RouteMemberRef>,
    /// The one Agent-facing model the group publishes.
    pub models: Vec<GatewayGroupModel>,
    /// The single Agent-facing id of the group.
    pub agent_model_id: Option<String>,
    /// Ordered rules that may override the member order for one turn.
    pub rules: Vec<vibex_core::ModelRouteRule>,
    pub classifier: Option<String>,
    /// `auto` asks the classifier for an effort level.
    pub effort: Option<String>,
    /// Retry budget after every member failed and nothing was sent.
    pub patience_ms: Option<u64>,
    pub sink: bool,
    pub unlists: bool,
    pub levels: Vec<String>,
    pub pick_member_id: Option<vibex_core::ModelRouteMemberId>,
}

/// One model a provider group offers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayGroupModel {
    pub model: String,
    /// The Providers serving the model, in the group's attempt order.
    pub providers: Vec<ModelProviderProfileId>,
}

impl GatewayRouteGroup {
    /// The id a request names for one of this group's models.
    pub fn model_id(&self, model: &str) -> String {
        format!(
            "{}{}/{}",
            vibex_core::ROUTE_GROUP_MODEL_PREFIX,
            self.group_id,
            model
        )
    }

    /// The model this request names, when the request names this group.
    pub fn model_of<'a>(&self, requested: &'a str) -> Option<&'a str> {
        vibex_core::route_group_model_ref(requested)
            .filter(|parsed| parsed.group_id == self.group_id)
            .map(|parsed| parsed.model_id)
    }

    /// The Providers that may answer for one model.
    pub fn providers_for(&self, model: &str) -> Vec<ModelProviderProfileId> {
        self.models
            .iter()
            .find(|entry| entry.model == model)
            .map(|entry| entry.providers.clone())
            .unwrap_or_default()
    }
}

impl GatewaySnapshot {
    /// The group a model id names, when it names a model the group offers.
    ///
    /// The membership check is what makes the intersection real: a model only
    /// one of the group's Providers serves is not addressable through the
    /// group, and resolving it would let the strategy pick a Provider that has
    /// never heard of it.
    pub fn group_for_model<'a>(
        &'a self,
        requested: &'a str,
    ) -> Option<(&'a GatewayRouteGroup, &'a str)> {
        let parsed = vibex_core::route_group_model_ref(requested)?;
        let group = self.groups.get(parsed.group_id)?;
        if let Some(model) = group
            .models
            .iter()
            .find(|model| model.model == parsed.model_id)
        {
            return Some((group, model.model.as_str()));
        }
        // An Agent that states its reasoning depth as part of the model id
        // asks for `group/<id>/<model>-<level>` (Antigravity's picker works
        // that way). The group holds the base model, because a depth is a
        // property of one model rather than a second model, so the suffix is
        // resolved here instead of making every member declare each level.
        // The level still reaches the upstream: `build_member_body` copies the
        // request's reasoning through.
        let base = reasoning_variant_base(parsed.model_id)?;
        group
            .models
            .iter()
            .find(|model| model.model == base)
            .map(|model| (group, model.model.as_str()))
    }

    pub fn upstream(&self, profile: &ModelProviderProfileId) -> Option<&GatewayUpstream> {
        self.upstreams
            .iter()
            .find(|upstream| &upstream.profile_id == profile)
    }

    pub fn models_by_upstream(&self, profile: &ModelProviderProfileId) -> Vec<String> {
        self.upstream(profile)
            .map(|upstream| upstream.models.clone())
            .unwrap_or_default()
    }

    /// Every model the gateway can serve.
    ///
    /// Groups are the only source: one entry per shared model, named
    /// `group/<id>/<model>`. There is no per-upstream entry, because the
    /// gateway has no upstream list of its own to list.
    pub fn model_catalog(&self) -> Vec<GatewayModelEntry> {
        let mut entries = Vec::new();
        for group in self.groups.values() {
            // `Unlists` keeps the group addressable through its id but out of
            // every list an Agent reads. A sink is the same kind of hidden
            // composition: it exists to be called by something that already
            // knows its id.
            if group.unlists || group.sink {
                continue;
            }
            for model in &group.models {
                entries.push(GatewayModelEntry {
                    id: group.model_id(&model.model),
                    model: model.model.clone(),
                    provider: ModelProviderProfileId::new(),
                    provider_name: group.display_name.clone(),
                });
            }
        }
        entries
    }

    /// The namespaces model ids are resolved against.
    ///
    /// Two upstreams may share a display name. Their short alias would then be
    /// ambiguous, so both fall back to their unique Provider Profile id: a
    /// listing must never publish an id the resolver would refuse.
    pub fn model_namespaces(&self) -> Vec<crate::route::ModelNamespace> {
        let aliases = self
            .upstreams
            .iter()
            .map(|upstream| {
                (
                    upstream.profile_id.clone(),
                    crate::route::provider_alias(&upstream.display_name),
                )
            })
            .collect::<Vec<_>>();
        self.upstreams
            .iter()
            .map(|upstream| {
                let alias = aliases
                    .iter()
                    .find(|(profile, _)| profile == &upstream.profile_id)
                    .map(|(_, alias)| alias.clone())
                    .unwrap_or_else(|| upstream.profile_id.as_str().to_string());
                let alias = if aliases
                    .iter()
                    .filter(|(_, candidate)| candidate == &alias)
                    .count()
                    > 1
                {
                    upstream.profile_id.as_str().to_string()
                } else {
                    alias
                };
                crate::route::ModelNamespace {
                    profile: upstream.profile_id.clone(),
                    alias,
                    models: upstream.models.clone(),
                }
            })
            .collect()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayModelEntry {
    pub id: String,
    pub model: String,
    pub provider: ModelProviderProfileId,
    pub provider_name: String,
}

/// Limits the service applies while serving.
#[derive(Debug, Clone)]
pub struct GatewayLimits {
    /// The largest request body accepted, in bytes.
    pub max_body_bytes: usize,
    /// How long an upstream may take to send its headers.
    pub connect_timeout: std::time::Duration,
    /// How long the settings snapshot is reused before a re-read.
    pub refresh_interval: std::time::Duration,
    /// Network proxy inherited from system settings.
    pub proxy_url: Option<String>,
}

impl Default for GatewayLimits {
    fn default() -> Self {
        Self {
            // A coding Agent sends a long transcript, occasionally with images.
            max_body_bytes: 64 * 1024 * 1024,
            connect_timeout: std::time::Duration::from_secs(120),
            refresh_interval: std::time::Duration::from_secs(5),
            proxy_url: None,
        }
    }
}

/// Where the gateway sends request events.
///
/// A boxed callback rather than a channel, so the gateway crate needs no
/// knowledge of the runtime's event bus: the runtime decides what a subscriber
/// is, and the gateway only says what happened.
pub type GatewayEventSink = Arc<dyn Fn(vibex_core::GatewayEvent) + Send + Sync>;

/// The listener's own state, readable by the Config Center.
pub type GatewayRuntime = Arc<RwLock<ModelGatewayRuntimeState>>;

/// The gateway service.
pub struct ModelGatewayService {
    db_path: std::path::PathBuf,
    config: ProviderConfigService,
    client: reqwest::Client,
    limits: GatewayLimits,
    runtime: GatewayRuntime,
    rotation: AtomicU64,
    /// Rotation for the choice *between* route members.
    ///
    /// It is deliberately not the Key counter: one request picks a member and
    /// then a Key, and sharing a counter would advance the Key rotation by two
    /// every time a route had more than one member.
    member_rotation: AtomicU64,
    /// Per-account concurrency and request-rate guards.
    limiter: std::sync::Arc<crate::limit::Limiter>,
    listener: Mutex<Option<ListenerHandle>>,
    snapshot: RwLock<Option<(GatewaySnapshot, std::time::Instant)>>,
    /// Set while the service is shutting down, so an in-flight request stops
    /// instead of starting a new upstream call.
    shutting_down: RwLock<bool>,
    /// Consecutive failures per route group, the breaker's whole state.
    ///
    /// The scope is the group, not the gateway. Groups share nothing but the
    /// listener: one group whose Providers are all broken must not turn away
    /// requests for a group whose Providers are fine, which is exactly what a
    /// single counter did.
    breakers: Mutex<BTreeMap<String, RouteBreaker>>,
    /// Recent completed latency per Provider, newest last.
    ///
    /// The `Smart` and `Lowest latency` strategies need a recent figure, and
    /// re-reading the ledger on every request would put a scan in the request
    /// path. This is bounded request state, not configuration: a restart simply
    /// starts with no history and the strategies fall back to configured order.
    recent_latency: Mutex<BTreeMap<ModelProviderProfileId, VecDeque<i64>>>,
    /// Requests currently being served, newest last.
    ///
    /// Bounded, because the graph draws a handful: a long-running gateway under
    /// load would otherwise grow this without limit. The oldest entry is
    /// dropped when the bound is reached, which is also what stops one stuck
    /// request from pinning memory forever.
    inflight: RwLock<Vec<vibex_core::GatewayInflight>>,
    /// Where request events go. A sink rather than a direct broadcast so the
    /// gateway crate stays unaware of the runtime's event bus.
    events: RwLock<Option<GatewayEventSink>>,
    /// Protocol verdicts per untyped endpoint, keyed by Provider and endpoint.
    ///
    /// It is a cache of what the endpoint answered, not configuration: the
    /// durable copy lives in `model_gateway_endpoint_protocol`, and this map
    /// only saves a database read per request.
    lessons: RwLock<BTreeMap<(ModelProviderProfileId, String), WireProtocol>>,
    /// Failover prompts waiting for an answer, by prompt id.
    ///
    /// A request that has to ask parks itself on the entry's sender: the ask is
    /// synchronous from the caller's point of view, so the answer has to arrive
    /// through something the request can await. Entries are removed as they are
    /// settled, so a prompt that was never drawn cannot leak.
    failover_asks: Mutex<BTreeMap<String, FailoverSender>>,
}

/// What the optional classifier said about one turn.
#[derive(Debug, Clone, Default)]
struct ClassifierOutcome {
    intent: Option<String>,
    effort: Option<String>,
    error: Option<String>,
}

/// The waiting end of one failover prompt.
type FailoverSender = tokio::sync::oneshot::Sender<vibex_core::GatewayFailoverAnswer>;

/// One route group's breaker.
///
/// It carries the same three facts the service used to hold once for the whole
/// gateway, plus the group they belong to.
#[derive(Debug, Clone, Default)]
struct RouteBreaker {
    consecutive_failures: u64,
    last_failure_at_ms: i64,
    /// Set while one request is being let through this degraded group.
    ///
    /// A degraded group is not closed: it forwards one request at a time so a
    /// recovered upstream can prove itself. Without that, the refusal path
    /// could never reach an upstream, no success could be recorded, and the
    /// group would only recover on a restart.
    probe_in_flight: bool,
}

impl RouteBreaker {
    fn is_degraded(&self) -> bool {
        self.consecutive_failures >= ModelGatewayService::DEGRADE_AFTER_FAILURES
    }
}

struct ListenerHandle {
    task: tokio::task::JoinHandle<()>,
    #[allow(dead_code)]
    port: u16,
}

impl std::fmt::Debug for ModelGatewayService {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ModelGatewayService")
            .field("db_path", &self.db_path)
            .field("status", &self.runtime_state().status)
            .finish()
    }
}

impl ModelGatewayService {
    /// How many consecutive upstream failures degrade the gateway.
    ///
    /// Three is enough to tell a transient blip from a broken route, and small
    /// enough that a user does not sit through many failing turns first.
    pub const DEGRADE_AFTER_FAILURES: u64 = 3;
    /// A failure this old no longer contributes to the count.
    pub const FAILURE_MEMORY_MS: i64 = 2 * 60 * 1000;
    /// The most in-flight requests retained for a client to draw.
    ///
    /// A graph that drew more would be unreadable, and a request that never
    /// finished must not pin memory indefinitely.
    pub const MAX_INFLIGHT: usize = 8;

    pub fn new(
        db_path: impl Into<std::path::PathBuf>,
        config: ProviderConfigService,
        limits: GatewayLimits,
    ) -> Arc<Self> {
        let mut builder = reqwest::Client::builder()
            .connect_timeout(limits.connect_timeout)
            .timeout(std::time::Duration::from_secs(600))
            .user_agent("vibex-model-gateway/1");
        if let Some(proxy) = limits.proxy_url.as_deref() {
            match reqwest::Proxy::all(proxy) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(_) => tracing::warn!(
                    target: "vibex.model_gateway",
                    "ignoring an unusable proxy url for the model gateway"
                ),
            }
        }
        let client = builder.build().unwrap_or_default();
        Arc::new(Self {
            db_path: db_path.into(),
            config,
            client,
            limits,
            runtime: Arc::new(RwLock::new(ModelGatewayRuntimeState::default())),
            rotation: AtomicU64::new(0),
            member_rotation: AtomicU64::new(0),
            limiter: crate::limit::Limiter::new(),
            listener: Mutex::new(None),
            snapshot: RwLock::new(None),
            shutting_down: RwLock::new(false),
            breakers: Mutex::new(BTreeMap::new()),
            recent_latency: Mutex::new(BTreeMap::new()),
            lessons: RwLock::new(BTreeMap::new()),
            failover_asks: Mutex::new(BTreeMap::new()),
            inflight: RwLock::new(Vec::new()),
            events: RwLock::new(None),
        })
    }

    pub fn runtime(&self) -> GatewayRuntime {
        Arc::clone(&self.runtime)
    }

    pub fn runtime_state(&self) -> ModelGatewayRuntimeState {
        self.runtime
            .read()
            .map(|state| state.clone())
            .unwrap_or_default()
    }

    /// The base URL an Agent should point at, when the service is running.
    pub fn base_url(&self) -> Option<String> {
        self.runtime_state().base_url()
    }

    pub fn max_body_bytes(&self) -> usize {
        self.limits.max_body_bytes
    }

    pub fn database_path(&self) -> &std::path::Path {
        &self.db_path
    }

    fn connection(&self) -> GatewayResult<DbConnection> {
        let mut connection = open_database(&self.db_path).map_err(GatewayError::from)?;
        apply_migrations(&mut connection).map_err(GatewayError::from)?;
        Ok(connection)
    }

    /// Reads the durable settings, creating the row on first use.
    pub fn settings(&self) -> GatewayResult<ModelGatewaySettings> {
        let connection = self.connection()?;
        ModelGatewaySettingsRepository::get_or_create(&connection).map_err(GatewayError::from)
    }

    /// Reads a fresh snapshot of the configuration the gateway routes over.
    pub fn read_snapshot(&self) -> GatewayResult<GatewaySnapshot> {
        let connection = self.connection()?;
        let settings = ModelGatewaySettingsRepository::get_or_create(&connection)?;
        let now = vibex_core::unix_timestamp_ms();
        let profiles = ProviderConfigService::list_model_provider_profiles(&self.config)?;
        let profiles = profiles
            .into_iter()
            .filter(|profile| profile.status == ModelProviderProfileStatus::Enabled)
            .map(|profile| (profile.id.clone(), profile))
            .collect::<BTreeMap<_, _>>();

        let window = settings
            .policy
            .token_limit
            .map(|limit| limit.window)
            .unwrap_or(GatewayTokenWindow::Day);
        // The gateway owns no Provider list: the Providers it may call are
        // exactly the members of the enabled route groups. Read fresh on every
        // snapshot, so adding or removing a member reaches the router without a
        // restart and the gateway has nothing of its own to keep in sync.
        let mut referenced: Vec<ModelProviderProfileId> = Vec::new();
        let enabled_routes = vibex_db::ModelRouteRepository::list_enabled(&connection)?;
        for route in &enabled_routes {
            // A direct route is not served here, so its Providers are not
            // upstreams of this listener.
            if !route.kind.via_gateway() {
                continue;
            }
            for member in route.enabled_members() {
                if !referenced.contains(&member.model_provider_profile_id) {
                    referenced.push(member.model_provider_profile_id.clone());
                }
            }
            // A classifier is a routing-side call to a model that need not be
            // one of the route's members. It still has to be in the snapshot,
            // because that is where its endpoint and Keys live.
            if let Some(classifier) = route.classifier.as_deref()
                && let Some(profile_id) = classifier_provider(classifier, route, &profiles)
                && !referenced.contains(&profile_id)
            {
                referenced.push(profile_id);
            }
        }

        let mut gateway_used_tokens = 0;
        let mut upstreams = Vec::with_capacity(referenced.len());
        for profile_id in &referenced {
            let Some(profile) = profiles.get(profile_id) else {
                continue;
            };
            let usage =
                GatewayTokenWindowRepository::current(&connection, profile_id, window, now)?;
            let used = usage.as_ref().map(|usage| usage.used_tokens).unwrap_or(0);
            gateway_used_tokens += used;
            let states = ModelProviderAccountStateRepository::list(&connection, profile_id)?;
            let state_by_key = states
                .into_iter()
                .map(|state| (state.credential_id.clone(), state))
                .collect::<BTreeMap<_, _>>();
            let keys = profile
                .credentials
                .iter()
                .map(|credential| {
                    let state = state_by_key
                        .get(&credential.id)
                        .cloned()
                        .unwrap_or_else(|| {
                            ModelProviderAccountState::ready(
                                profile_id.clone(),
                                credential.id.clone(),
                            )
                        });
                    GatewayKeySummary {
                        credential_id: credential.id.clone(),
                        display_name: credential.display_label(),
                        masked_hint: credential.masked_hint.clone(),
                        state: state.visibility_at(now),
                        used_tokens: used,
                        models: credential.models.clone(),
                        proxy_url: credential.proxy_url.clone(),
                    }
                })
                .collect::<Vec<_>>();
            upstreams.push(GatewayUpstream {
                profile_id: profile_id.clone(),
                display_name: profile.display_name.clone(),
                endpoints: profile.endpoints.clone(),
                metadata: profile.metadata.clone(),
                models: profile
                    .configured_models
                    .iter()
                    .filter(|model| model.enabled)
                    .map(|model| model.id.clone())
                    .collect(),
                credentials: profile.credentials.clone(),
                keys,
            });
        }
        // A model group publishes one Agent-facing model. The members are
        // filtered here, once, from the same profiles the request will be
        // prepared against: computing them at request time would let the
        // catalogue and the router disagree about which member is reachable.
        let groups = enabled_routes
            .iter()
            .filter(|route| route.kind.via_gateway())
            .filter_map(|route| {
                let member_ref = |member: &vibex_core::ModelRouteMember, model: &str| {
                    profiles
                        .get(&member.model_provider_profile_id)
                        .map(|profile| crate::route::RouteMemberRef {
                            member_id: Some(member.id.clone()),
                            profile: profile.id.clone(),
                            model: model.to_string(),
                            reasoning_effort: member.reasoning_effort.clone(),
                            weight: member.weight.max(vibex_core::DEFAULT_MEMBER_WEIGHT),
                            context_tokens: member.context_tokens,
                            first_token_timeout_ms: member.first_token_timeout_ms,
                        })
                };
                let agent_model_id = route.agent_model()?.to_string();
                let providers = route
                    .enabled_members()
                    .filter(|member| {
                        profiles
                            .get(&member.model_provider_profile_id)
                            .map(|profile| member_is_served(profile, &member.model_id))
                            .unwrap_or(false)
                    })
                    .filter_map(|member| member_ref(member, member.model_id.trim()))
                    .collect::<Vec<_>>();
                if providers.is_empty() {
                    return None;
                }
                // The catalogue names each source once, while the router keeps
                // two entries of the same Provider distinct.
                let mut serving = Vec::new();
                for member in &providers {
                    if !serving.contains(&member.profile) {
                        serving.push(member.profile.clone());
                    }
                }
                Some((
                    route.id.as_str().to_string(),
                    GatewayRouteGroup {
                        group_id: route.id.as_str().to_string(),
                        display_name: route.display_name.clone(),
                        strategy: route.strategy,
                        // The group's own strategy, applied to its Keys as well
                        // as to its members.
                        key_strategy: match route.strategy {
                            vibex_core::RouteStrategy::Ordered
                            | vibex_core::RouteStrategy::LowestLatency
                            | vibex_core::RouteStrategy::Weighted
                            | vibex_core::RouteStrategy::Manual => GatewayRoutingStrategy::Ordered,
                            vibex_core::RouteStrategy::RoundRobin => {
                                GatewayRoutingStrategy::RoundRobin
                            }
                            vibex_core::RouteStrategy::LeastUsed
                            | vibex_core::RouteStrategy::Smart => GatewayRoutingStrategy::LeastUsed,
                        },
                        first_token_timeout_ms: route.first_token_timeout_ms,
                        pin: route.pin,
                        failover: route.failover,
                        token_limit: route.token_limit,
                        providers,
                        models: vec![GatewayGroupModel {
                            model: agent_model_id.clone(),
                            providers: serving,
                        }],
                        agent_model_id: Some(agent_model_id),
                        rules: route.rules.clone(),
                        classifier: route.classifier.clone(),
                        effort: route.effort.clone(),
                        patience_ms: route.patience_ms,
                        sink: route.sink,
                        unlists: route.unlists,
                        levels: route.levels.clone(),
                        pick_member_id: route.pick_member_id.clone(),
                    },
                ))
            })
            .collect::<BTreeMap<_, _>>();

        Ok(GatewaySnapshot {
            settings,
            upstreams,
            gateway_used_tokens,
            groups,
        })
    }

    pub(crate) fn cached_snapshot(&self) -> GatewayResult<GatewaySnapshot> {
        if let Ok(guard) = self.snapshot.read()
            && let Some((snapshot, read_at)) = guard.as_ref()
            && read_at.elapsed() < self.limits.refresh_interval
        {
            return Ok(snapshot.clone());
        }
        let snapshot = self.read_snapshot()?;
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = Some((snapshot.clone(), std::time::Instant::now()));
        }
        Ok(snapshot)
    }

    /// Invalidates the cached snapshot after a settings mutation.
    pub fn invalidate_snapshot(&self) {
        if let Ok(mut guard) = self.snapshot.write() {
            *guard = None;
        }
    }

    /// Applies a settings change to the running listener.
    ///
    /// Turning the gateway off must actually stop the listener, not just flip
    /// a flag: an Agent pointing at a dead port is the honest state, and every
    /// route group becomes unusable. Turning it back on re-binds the persisted
    /// port.
    pub async fn apply_policy(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        let settings = self.settings()?;
        // A degraded listener is already bound: restarting it would move the
        // port, rewrite every Agent's base URL, and throw away the breakers
        // that were about to prove the upstream had recovered.
        let running = self.runtime_state().status.is_listening();
        match (settings.policy.enabled, running) {
            (false, true) => {
                self.stop_listener().await;
                let state = ModelGatewayRuntimeState {
                    status: ModelGatewayStatus::Disabled,
                    notice: Some("The gateway is turned off".to_string()),
                    ..ModelGatewayRuntimeState::default()
                };
                self.publish(state.clone());
                Ok(state)
            }
            (true, false) => self.start().await,
            _ => {
                self.invalidate_snapshot();
                Ok(self.runtime_state())
            }
        }
    }

    /// Stops the listener but leaves the service usable, so it can be started
    /// again without rebuilding it.
    async fn stop_listener(&self) {
        let handle = self.listener.lock().ok().and_then(|mut guard| guard.take());
        if let Some(handle) = handle {
            handle.task.abort();
            let _ = handle.task.await;
        }
    }

    /// Starts the listener, persisting the port that actually bound.
    ///
    /// A port collision probes upward from the persisted port and stores the
    /// result, so the next start does not move again and Agents keep the same
    /// base URL.
    pub async fn start(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        let result = self.start_inner().await;
        if let Err(error) = &result {
            // Every start failure is a terminal state for this attempt. Without
            // this, a settings or snapshot read that failed before the bind
            // would leave the Config Center saying "starting" forever.
            let current = self.runtime_state();
            if current.status == ModelGatewayStatus::Starting {
                self.publish(ModelGatewayRuntimeState::unavailable(error.message()));
            }
        }
        result
    }

    async fn start_inner(self: &Arc<Self>) -> GatewayResult<ModelGatewayRuntimeState> {
        if let Ok(mut shutting_down) = self.shutting_down.write() {
            *shutting_down = false;
        }
        let settings = self.settings()?;
        if !settings.policy.enabled {
            let state = ModelGatewayRuntimeState {
                status: ModelGatewayStatus::Disabled,
                notice: Some("The gateway is turned off".to_string()),
                ..ModelGatewayRuntimeState::default()
            };
            self.publish(state.clone());
            return Ok(state);
        }
        // The gateway is independent of whether any route exists. It owns no
        // Provider list, so "no upstreams" is not a configuration error: a
        // fresh installation has no groups yet and the listener simply serves
        // an empty model list until one is created. Refusing to bind here was
        // what made the gateway look like a feature of its upstream list.
        let snapshot = self.read_snapshot()?;

        let listener = match self.bind(settings.port).await {
            Ok(listener) => listener,
            Err(error) => {
                // A bind failure is a terminal state, not a pending one: the
                // Config Center must stop saying "starting" and tell the user
                // that route groups cannot be served.
                self.publish(ModelGatewayRuntimeState::unavailable(error.message()));
                return Err(error);
            }
        };
        let port = listener
            .local_addr()
            .map_err(|_| GatewayError::Internal("the gateway listener has no address".to_string()))?
            .port();
        if settings.port != port
            && let Ok(connection) = self.connection()
        {
            let _ = ModelGatewaySettingsRepository::store_bound_port(&connection, port);
        }
        self.invalidate_snapshot();

        let router = crate::server::build_router(Arc::clone(self));
        let task = tokio::spawn(async move {
            if let Err(error) = axum::serve(listener, router).await {
                tracing::warn!(
                    target: "vibex.model_gateway",
                    %error,
                    "the model gateway stopped serving"
                );
            }
        });
        if let Ok(mut guard) = self.listener.lock() {
            *guard = Some(ListenerHandle { task, port });
        }
        let state = ModelGatewayRuntimeState::running(port, snapshot.groups.len());
        self.publish(state.clone());
        self.prune_ledgers();
        Ok(state)
    }

    async fn bind(&self, preferred: u16) -> GatewayResult<tokio::net::TcpListener> {
        if preferred == 0 {
            return tokio::net::TcpListener::bind(("127.0.0.1", 0))
                .await
                .map_err(|error| self.bind_error(0, error));
        }
        let mut last_error = None;
        for offset in 0..=20u16 {
            let Some(port) = preferred.checked_add(offset) else {
                break;
            };
            match tokio::net::TcpListener::bind(("127.0.0.1", port)).await {
                Ok(listener) => return Ok(listener),
                Err(error) => last_error = Some((port, error)),
            }
        }
        let (port, error) =
            last_error.unwrap_or((preferred, std::io::Error::other("no port available")));
        Err(self.bind_error(port, error))
    }

    fn bind_error(&self, port: u16, error: std::io::Error) -> GatewayError {
        GatewayError::NotConfigured(format!(
            "could not listen on 127.0.0.1:{port} ({error}). Route groups cannot be served; direct Providers are unaffected."
        ))
    }

    /// Installs the sink request events are delivered to.
    ///
    /// The runtime owns the event bus, so the gateway is handed a way to send
    /// rather than a bus of its own: one listener, one place events originate.
    pub fn set_event_sink(&self, sink: GatewayEventSink) {
        if let Ok(mut guard) = self.events.write() {
            *guard = Some(sink);
        }
    }

    /// The requests being served right now.
    ///
    /// This is what a graph reads on first paint, before any event arrives; the
    /// event stream then keeps it current.
    pub fn inflight_requests(&self) -> Vec<vibex_core::GatewayInflight> {
        self.inflight
            .read()
            .map(|guard| guard.clone())
            .unwrap_or_default()
    }

    /// Records that a request started, or moved to another member.
    pub(crate) fn note_inflight(&self, inflight: vibex_core::GatewayInflight) {
        let request_id = inflight.request_id.clone();
        if let Ok(mut guard) = self.inflight.write() {
            guard.retain(|existing| existing.request_id != request_id);
            guard.push(inflight.clone());
            // Keep the newest `MAX_INFLIGHT`; a graph that cannot draw them all
            // does not need them all retained.
            while guard.len() > Self::MAX_INFLIGHT {
                guard.remove(0);
            }
        }
        self.publish_event(vibex_core::GatewayEvent::Began(inflight));
    }

    /// Records the first token of a request, which ends its deadline.
    pub(crate) fn note_first_token(&self, request_id: &RequestId, at_ms: i64) {
        let updated = {
            let Ok(mut guard) = self.inflight.write() else {
                return;
            };
            let Some(entry) = guard
                .iter_mut()
                .find(|entry| &entry.request_id == request_id)
            else {
                return;
            };
            if entry.first_token_at_ms.is_some() {
                return;
            }
            entry.first_token_at_ms = Some(at_ms);
            true
        };
        if updated {
            self.publish_event(vibex_core::GatewayEvent::FirstToken {
                request_id: request_id.clone(),
                at_ms,
            });
        }
    }

    /// Drops a finished request from the in-flight table.
    pub(crate) fn note_finished(
        &self,
        request_id: &RequestId,
        outcome: vibex_core::GatewayRequestOutcome,
    ) {
        if let Ok(mut guard) = self.inflight.write() {
            guard.retain(|entry| &entry.request_id != request_id);
        }
        self.publish_event(vibex_core::GatewayEvent::Ended {
            request_id: request_id.clone(),
            outcome,
        });
    }

    fn publish_event(&self, event: vibex_core::GatewayEvent) {
        let sink = self.events.read().ok().and_then(|guard| guard.clone());
        if let Some(sink) = sink {
            sink(event);
        }
    }

    /// Publishes a failover prompt and waits for its answer.
    ///
    /// Returns `None` when nothing is listening (a headless runtime), which the
    /// caller reads as "the user cannot be asked" — the request then does what
    /// the route's setting says without a prompt, rather than blocking on an
    /// answer that cannot arrive.
    ///
    /// The wait is bounded by the prompt's own window. An unanswered prompt is
    /// an accepted switch, so the one outcome that must never happen is waiting
    /// forever: the request is holding a live Agent turn.
    async fn ask_failover(
        &self,
        prompt: vibex_core::GatewayFailoverPrompt,
    ) -> Option<vibex_core::GatewayFailoverAnswer> {
        let (sender, receiver) = tokio::sync::oneshot::channel();
        if let Ok(mut asks) = self.failover_asks.lock() {
            asks.insert(prompt.id.as_str().to_string(), sender);
        } else {
            return None;
        }
        // Registered before the event is published: a client that answers the
        // instant it draws the prompt must find the sender already waiting.
        if self
            .events
            .read()
            .ok()
            .and_then(|guard| guard.clone())
            .is_none()
        {
            if let Ok(mut asks) = self.failover_asks.lock() {
                asks.remove(prompt.id.as_str());
            }
            return None;
        }
        self.publish_event(vibex_core::GatewayEvent::FailoverAsked(prompt.clone()));
        let window = std::time::Duration::from_millis(
            u64::try_from(prompt.remaining_ms(vibex_core::unix_timestamp_ms())).unwrap_or(0),
        );
        let answer = match tokio::time::timeout(window, receiver).await {
            Ok(Ok(answer)) => answer,
            // The window closed, or the sender was dropped because the client
            // went away. Both mean nobody overrode the default.
            Ok(Err(_)) | Err(_) => {
                if let Ok(mut asks) = self.failover_asks.lock() {
                    asks.remove(prompt.id.as_str());
                }
                vibex_core::GatewayFailoverAnswer::Switch
            }
        };
        self.publish_event(vibex_core::GatewayEvent::FailoverSettled {
            prompt_id: prompt.id.clone(),
            answer,
        });
        Some(answer)
    }

    /// Answers an outstanding failover prompt.
    ///
    /// Returns whether a prompt was actually waiting: an answer that arrives
    /// after its window closed is not an error, it is simply too late, and
    /// pretending otherwise would let a stale click redirect a later request.
    pub fn answer_failover(
        &self,
        prompt_id: &vibex_core::RequestId,
        answer: vibex_core::GatewayFailoverAnswer,
    ) -> bool {
        let sender = self
            .failover_asks
            .lock()
            .ok()
            .and_then(|mut asks| asks.remove(prompt_id.as_str()));
        match sender {
            Some(sender) => sender.send(answer).is_ok(),
            None => false,
        }
    }

    /// Whether any route group is currently stepping aside.
    pub fn is_degraded(&self) -> bool {
        self.runtime_state().degraded_routes > 0
    }

    /// Whether one group is stepping aside.
    pub fn group_is_degraded(&self, group_id: &str) -> bool {
        self.breakers
            .lock()
            .map(|breakers| {
                breakers
                    .get(group_id)
                    .is_some_and(RouteBreaker::is_degraded)
            })
            .unwrap_or(false)
    }

    /// Decides whether this request may consult an upstream, and claims the
    /// group's probe slot when it is the one its degraded group lets through.
    ///
    /// A degraded group is not closed. It admits exactly one request at a
    /// time: if that request reaches an upstream and succeeds,
    /// [`Self::note_success`] clears the group; if it fails, the refusal keeps
    /// every other caller on their direct Provider instead of queueing them
    /// behind a broken member.
    ///
    /// Returns `true` when this request claimed a probe slot and must release
    /// it with [`Self::finish_attempt`], including on every error path.
    pub(crate) fn begin_attempt(&self, group_id: &str) -> GatewayResult<bool> {
        let Ok(mut breakers) = self.breakers.lock() else {
            return Err(GatewayError::Internal(
                "the gateway breaker state is poisoned".to_string(),
            ));
        };
        let breaker = breakers.entry(group_id.to_string()).or_default();
        if !breaker.is_degraded() {
            return Ok(false);
        }
        if breaker.probe_in_flight {
            return Err(GatewayError::NotConfigured(Self::STEPPED_ASIDE.to_string()));
        }
        breaker.probe_in_flight = true;
        Ok(true)
    }

    /// Releases the slot [`Self::begin_attempt`] may have claimed.
    pub(crate) fn finish_attempt(&self, group_id: &str, probe: bool) {
        if !probe {
            return;
        }
        if let Ok(mut breakers) = self.breakers.lock()
            && let Some(breaker) = breakers.get_mut(group_id)
        {
            breaker.probe_in_flight = false;
        }
    }

    /// The notice and error text a degraded group returns to callers it is
    /// not currently letting through. Kept in one place so the server can
    /// recognise its own refusal and never count it as an upstream failure.
    pub(crate) const STEPPED_ASIDE: &'static str = "this route group stepped aside after repeated upstream failures; let one request through at a time until an upstream answers, or use a direct Provider";

    /// Records one upstream success for a group, clearing its failure count
    /// and its degraded state.
    pub fn note_success(&self, group_id: &str) {
        let Ok(mut breakers) = self.breakers.lock() else {
            return;
        };
        let breaker = breakers.entry(group_id.to_string()).or_default();
        breaker.consecutive_failures = 0;
        breaker.last_failure_at_ms = 0;
        breaker.probe_in_flight = false;
        let degraded = breakers
            .values()
            .filter(|breaker| breaker.is_degraded())
            .count();
        drop(breakers);
        self.publish_degraded_routes(degraded);
    }

    /// Records one failed request for a group and degrades that group once its
    /// count is reached.
    ///
    /// This counts *requests the caller saw fail*, not upstream attempts: one
    /// dead upstream cools its only Key, and every later request is refused
    /// before it can attempt anything. Counting attempts would never reach the
    /// threshold in exactly the case that matters.
    ///
    /// A failure observed while the group is already degraded only advances
    /// the record; it never re-degrades or rewrites the notice, because
    /// [`Self::begin_attempt`] is what decides whether an upstream is even
    /// consulted.
    pub fn note_failure(&self, group_id: &str) {
        let now = vibex_core::unix_timestamp_ms();
        let Ok(mut breakers) = self.breakers.lock() else {
            return;
        };
        let breaker = breakers.entry(group_id.to_string()).or_default();
        // A failure older than the memory window starts a fresh count rather
        // than adding to a tally from an unrelated outage.
        if breaker.last_failure_at_ms != 0
            && now.saturating_sub(breaker.last_failure_at_ms) > Self::FAILURE_MEMORY_MS
        {
            breaker.consecutive_failures = 1;
        } else {
            breaker.consecutive_failures = breaker.consecutive_failures.saturating_add(1);
        }
        breaker.last_failure_at_ms = now;
        let count = breaker.consecutive_failures;
        let degraded = breakers
            .values()
            .filter(|breaker| breaker.is_degraded())
            .count();
        drop(breakers);
        if count < Self::DEGRADE_AFTER_FAILURES {
            return;
        }
        self.publish_degraded_routes(degraded);
    }

    /// Rewrites the runtime state to match the per-group breaker set.
    ///
    /// The status is derived, never stored separately: `Degraded` means "the
    /// listener is up and at least one group is stepping aside", so it cannot
    /// disagree with the map that decides whether a group refuses.
    fn publish_degraded_routes(&self, degraded: usize) {
        let mut state = self.runtime_state();
        if !state.status.is_listening() {
            return;
        }
        state.degraded_routes = degraded;
        state.status = if degraded == 0 {
            ModelGatewayStatus::Running
        } else {
            ModelGatewayStatus::Degraded
        };
        state.notice = (degraded > 0).then(|| {
            format!(
                "{degraded} of {} route groups stepped aside after repeated upstream failures: each lets one request through at a time until an upstream answers.",
                state.route_count.unwrap_or(0)
            )
        });
        state.updated_at_ms = vibex_core::unix_timestamp_ms();
        self.publish(state);
    }

    pub fn publish(&self, mut state: ModelGatewayRuntimeState) {
        if let Ok(mut guard) = self.runtime.write() {
            // A state transition that does not carry a start time keeps the one
            // the listener already had: a degraded gateway is still the same
            // uptime, and reporting "up since now" on every transition would
            // make the number meaningless.
            if state.started_at_ms.is_none() && state.status.is_listening() {
                state.started_at_ms = guard
                    .started_at_ms
                    .or_else(|| Some(vibex_core::unix_timestamp_ms()));
            }
            *guard = state;
        }
    }

    /// Stops the listener. Idempotent.
    pub async fn shutdown(&self) {
        if let Ok(mut shutting_down) = self.shutting_down.write() {
            *shutting_down = true;
        }
        self.stop_listener().await;
        let mut state = self.runtime_state();
        state.status = ModelGatewayStatus::Unavailable;
        state.port = None;
        state.notice = Some("The gateway is stopped".to_string());
        self.publish(state);
    }

    fn prune_ledgers(&self) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let _ = ModelGatewayRequestLogRepository::prune(&connection, now);
        let _ = GatewayConversationPinRepository::prune(&connection, now, 512);
    }

    /// Resolves an inbound body into a prepared upstream call.
    pub async fn prepare(
        &self,
        inbound: WireProtocol,
        headers: &HeaderMap,
        body: &[u8],
    ) -> GatewayResult<PreparedCall> {
        let request = parse_request(inbound, body)?;
        let body_value: Value = serde_json::from_slice(body).map_err(|_| {
            GatewayError::MalformedRequest("the body is not valid JSON".to_string())
        })?;
        let snapshot = self.cached_snapshot()?;
        // A group may set its own ceiling. It is checked instead of the
        // gateway's rather than in addition: a group that composes three
        // Providers needs one ceiling over the composition, and the per-Key
        // limits do not compose — three Keys with a limit each still allow
        // three times the spend.
        let route_token_limit = resolved_group_id(&request.model)
            .and_then(|group_id| snapshot.groups.get(group_id))
            .and_then(|group| group.token_limit);
        let ceiling = route_token_limit
            .as_ref()
            .or(snapshot.settings.policy.token_limit.as_ref())
            .map(|limit| limit.tokens);
        if let Some(tokens) = ceiling
            && snapshot.gateway_used_tokens >= tokens
        {
            return Err(GatewayError::TokenCeilingReached {
                provider: snapshot.settings.display_name.clone(),
            });
        }

        // Classifier: one small request at the start of the turn, never on a
        // retry of the turn's own answer. It is skipped unless a rule can use
        // an intent or the route asked for automatic effort.
        let route_group =
            resolved_group_id(&request.model).and_then(|group_id| snapshot.groups.get(group_id));
        let mut classifier_outcome = ClassifierOutcome::default();
        let mut forced_effort: Option<String> = None;
        if let Some(group) = route_group
            && group.classifier.is_some()
            && (group.effort.as_deref() == Some("auto")
                || group.rules.iter().any(|rule| rule.intent.is_some()))
        {
            classifier_outcome = self.classify_turn(&snapshot, group, &request).await;
            if group.effort.as_deref() == Some("auto") {
                forced_effort = classifier_outcome.effort.clone();
            }
        }

        let now = vibex_core::unix_timestamp_ms();
        let connection = self.connection()?;
        let agent_id = agent_id_from_headers(headers);
        let header_pairs = headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_string(), value.to_string()))
            })
            .collect::<Vec<_>>();
        // Pinning is a property of the composition, so the group decides it.
        // There is no gateway-wide default: a model that does not belong to a
        // group is not a gateway model at all.
        //
        // It is resolved *before* the members are ordered, because the pin is
        // what decides the order: a conversation already on one Provider has to
        // be offered that Provider first, or a round-robin group would walk away
        // from the member holding its prompt cache on every single turn and the
        // pin would never hold anything.
        let pin = resolved_group_id(&request.model)
            .and_then(|group_id| snapshot.groups.get(group_id))
            .map(|group| group.pin)
            .unwrap_or(GatewayPinScope::Off);
        let conversation = (pin == GatewayPinScope::Conversation)
            .then(|| conversation_key(agent_id.as_deref(), &header_pairs, &body_value))
            .flatten();
        let pinned = conversation
            .as_deref()
            .and_then(|key| {
                GatewayConversationPinRepository::get(&connection, key)
                    .ok()
                    .flatten()
            })
            .filter(|pin| pin.is_live_at(now))
            .map(|pin| (pin.model_provider_profile_id, pin.credential_id));

        // Resolve the request into the list of upstreams it may call. A route
        // expands to its members; anything else is a one-member list. Both then
        // take the same path, which is what keeps a direct Provider and a
        // route from growing two sets of routing rules.
        let (resolved_members, mut route_decision) = self.resolve_members(
            &snapshot,
            &request,
            pinned.as_ref().map(|(profile, _)| profile),
            agent_id.as_deref(),
            headers,
            classifier_outcome.intent.as_deref(),
            forced_effort.as_deref(),
        )?;
        if let Some(group) = route_group
            && group.classifier.is_some()
        {
            route_decision.classifier = group.classifier.clone();
            route_decision.classifier_error = classifier_outcome.error.clone();
            route_decision.intent = classifier_outcome.intent.clone();
            if let Some(effort) = forced_effort.as_deref() {
                route_decision.effort = Some(effort.to_string());
            }
            if route_decision.reason.is_none() {
                route_decision.reason = Some(if classifier_outcome.error.is_some() {
                    "classifier_failed".to_string()
                } else {
                    "classifier".to_string()
                });
            }
        }
        if resolved_members.is_empty() {
            return Err(GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest: snapshot
                    .model_catalog()
                    .into_iter()
                    .map(|entry| entry.id)
                    .collect(),
            });
        }

        // The breaker is per route group, so the group has to be known before
        // it can be consulted: a degraded group refuses an extra request, and
        // a healthy one must not pay for a neighbour's failures.
        let group_id = resolved_group_id(&request.model).map(str::to_string);
        let mut probe = ProbeGuard {
            service: self,
            group: group_id.clone().unwrap_or_default(),
            armed: match group_id.as_deref() {
                Some(group_id) => self.begin_attempt(group_id)?,
                None => false,
            },
        };

        let mut members = Vec::new();
        let mut last_error: Option<GatewayError> = None;
        // The tool names as the client spelled them, and the form the strict
        // protocols accept. Built once for the whole request because it is a
        // property of the request: every member and every retry must send the
        // same names, and the reply has to name the client's own tools.
        let tool_names = ToolNameMap::build(&request);
        for resolved in resolved_members {
            match self.prepare_member(
                &snapshot,
                &request,
                inbound,
                &body_value,
                &resolved,
                &connection,
                pinned.as_ref(),
                now,
                &tool_names,
            ) {
                Ok(member) => members.push(member),
                // A member that cannot be called at all is skipped so the next
                // one can answer. Its reason is kept in case none can, so the
                // caller reads the real cause instead of a bare 404.
                Err(error) => last_error = Some(error),
            }
        }
        if members.is_empty() {
            return Err(last_error.unwrap_or_else(|| GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest: Vec::new(),
            }));
        }

        // The route's failover policy travels with the request, not with the
        // snapshot: a prompt answered with "always" rewrites the route while
        // this request is still in flight, and it must keep the answer it was
        // given rather than start asking again mid-flight.
        let (route_name, failover, patience_ms) = group_id
            .as_deref()
            .and_then(|group_id| snapshot.groups.get(group_id))
            .map(|group| {
                (
                    group.display_name.clone(),
                    group.failover,
                    group.patience_ms,
                )
            })
            .unwrap_or((String::new(), vibex_core::GatewayFailover::Auto, None));
        let prepared = PreparedCall {
            inbound,
            requested_model: request.model.clone(),
            members,
            agent_id,
            conversation,
            stream: request.stream,
            user_agent: headers
                .get(reqwest::header::USER_AGENT)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string),
            group_id,
            probe: probe.armed,
            request_id: RequestId::new(),
            route_name,
            failover,
            patience_ms,
            route_decision,
            retries: 0,
            // A pin only means "this member was chosen on purpose" when a live
            // conversation pin actually named it. The setting alone is not
            // enough: a first turn in a conversation has the setting but no
            // pin yet, and that turn has nothing to be disloyal to.
            pinned: pinned.is_some() && pin == GatewayPinScope::Conversation,
            tool_names,
        };
        // The slot now belongs to the caller of `open_attempt`.
        probe.armed = false;
        Ok(prepared)
    }

    /// Expands the request into the Providers it may call, in attempt order.
    ///
    /// A gateway model is always `group/<id>/<model>`, because route groups are
    /// the gateway's only input. Anything else is refused rather than resolved
    /// against a Provider list the gateway no longer owns: a bare vendor model
    /// id has no meaning here, and pretending otherwise was what made the
    /// listener look like it had upstreams of its own.
    /// Expands a request into the members that may serve it, in attempt order.
    ///
    /// `pinned` is the Provider this conversation is already on, when there is
    /// one. It is moved to the front: the plan's strategy decides how the
    /// remaining members are ordered, but it must not decide that a conversation
    /// leaves the member whose prompt cache it is paying for. Without this the
    /// pin only ever reordered Keys *inside* whichever member the strategy
    /// happened to pick, so a round-robin group re-pinned the conversation on
    /// every turn and the record never meant anything.
    #[allow(clippy::too_many_arguments)]
    fn resolve_members(
        &self,
        snapshot: &GatewaySnapshot,
        request: &CanonicalRequest,
        pinned: Option<&ModelProviderProfileId>,
        agent_id: Option<&str>,
        headers: &HeaderMap,
        intent: Option<&str>,
        forced_effort: Option<&str>,
    ) -> GatewayResult<(Vec<ResolvedMember>, vibex_core::GatewayRouteDecisionTrace)> {
        let Some((group, _model)) = snapshot.group_for_model(&request.model) else {
            let parsed = vibex_core::route_group_model_ref(&request.model);
            let closest = parsed
                .filter(|parsed| snapshot.groups.contains_key(parsed.group_id))
                .map(|parsed| {
                    snapshot
                        .groups
                        .get(parsed.group_id)
                        .map(|group| {
                            group
                                .models
                                .iter()
                                .map(|entry| group.model_id(&entry.model))
                                .collect::<Vec<_>>()
                        })
                        .unwrap_or_default()
                })
                .unwrap_or_else(|| {
                    snapshot
                        .model_catalog()
                        .into_iter()
                        .map(|entry| entry.id)
                        .collect()
                });
            return Err(GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest,
            });
        };
        // A model group names its members directly: every enabled member is a
        // candidate, and the upstream model is the member's own.
        let mut candidates = group.providers.clone();
        if candidates.is_empty() {
            return Err(GatewayError::ModelNotFound {
                model: request.model.clone(),
                closest: group
                    .models
                    .iter()
                    .map(|entry| group.model_id(&entry.model))
                    .collect(),
            });
        }
        let mut plans = crate::route::plan_members(
            &candidates,
            group.strategy,
            self.member_rotation.fetch_add(1, Ordering::Relaxed),
            &self.member_observations(snapshot),
            group.pick_member_id.as_ref(),
        );
        // A default ordering is not worth a sentence; any strategy that is not
        // "in order", a matched rule, or a manual pick is.
        let mut decision = vibex_core::GatewayRouteDecisionTrace::default();
        if group.strategy != vibex_core::RouteStrategy::Ordered {
            decision.strategy = Some(group.strategy.id().to_string());
        }
        decision.patience_ms = group.patience_ms;
        decision.intent = intent.map(str::to_string);
        decision.effort = forced_effort.map(str::to_string);
        if group.strategy == vibex_core::RouteStrategy::Manual {
            decision.reason = Some("manual".to_string());
        }
        // Rules outrank the strategy for this turn. The first matching rule
        // names the member to try first and any explicit order behind it; it
        // does not remove the remaining members, so failover still has every
        // source the group declared. A pause rule stops before any call.
        if !group.rules.is_empty() {
            let context = route_rule_context(request, agent_id, headers, intent);
            if let Some((index, rule)) = vibex_core::first_matching_rule(&group.rules, &context) {
                if rule.pause {
                    return Err(GatewayError::RoutePaused {
                        group: group.display_name.clone(),
                        reason: format!("rule {} paused routing for this request", index + 1),
                    });
                }
                decision.rule_index = Some(index);
                decision.rule_id = Some(rule.id.clone());
                decision.reason = Some("rule".to_string());
                if let Some(effort) = rule.use_effort.as_deref() {
                    decision.effort = Some(effort.trim().to_string());
                }
                reorder_for_rule(&mut plans, rule);
                if let Some(effort) = rule.use_effort.as_deref()
                    && let Some(first) = plans.first_mut()
                {
                    first.reasoning_effort = Some(effort.trim().to_string());
                }
            }
        }
        // The pinned member goes first, ahead of whatever the strategy chose.
        // The strategy still orders everything else, so a group that pins and
        // then has to fail over keeps its preferred fallback order.
        hoist_pinned(&mut plans, pinned, |plan| &plan.profile);
        for candidate in &mut candidates {
            if let Some(plan) = plans.iter().find(|plan| plan.profile == candidate.profile) {
                candidate.reasoning_effort = plan.reasoning_effort.clone();
            }
        }
        Ok((
            plans
                .into_iter()
                .map(|member| ResolvedMember {
                    profile: member.profile,
                    model: member.model,
                    reasoning_effort: member
                        .reasoning_effort
                        .or_else(|| forced_effort.map(str::to_string)),
                    first_token_timeout_ms: member
                        .first_token_timeout_ms
                        .or(group.first_token_timeout_ms),
                    key_strategy: group.key_strategy,
                })
                .collect(),
            decision,
        ))
    }

    /// Asks the route's classifier for the turn's intent and effort.
    ///
    /// The classifier is one real, small Chat request to a member's upstream.
    /// It never enters the ledger or the inflight table: it is routing
    /// metadata, not a request the Agent made, and drawing it would make the
    /// live card claim traffic nobody sent. Every failure is returned as text
    /// so the decision trace can say why the default order was used.
    async fn classify_turn(
        &self,
        snapshot: &GatewaySnapshot,
        group: &GatewayRouteGroup,
        request: &CanonicalRequest,
    ) -> ClassifierOutcome {
        let Some(classifier) = group
            .classifier
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            return ClassifierOutcome::default();
        };
        let Some((profile_id, model)) = resolve_classifier_target(snapshot, group, classifier)
        else {
            return ClassifierOutcome {
                error: Some("the classifier names no reachable model".to_string()),
                ..Default::default()
            };
        };
        let Some(upstream) = snapshot.upstream(&profile_id) else {
            return ClassifierOutcome {
                error: Some("the classifier Provider is not in the snapshot".to_string()),
                ..Default::default()
            };
        };
        let protocol = WireProtocol::ChatCompletions;
        let Some(endpoint) = upstream
            .endpoint_for(protocol.wire_protocol_id())
            .or_else(|| upstream.untyped_endpoint())
        else {
            return ClassifierOutcome {
                error: Some("the classifier Provider has no Chat endpoint".to_string()),
                ..Default::default()
            };
        };
        let Some(key) = upstream.keys.iter().find(|key| key.state.is_ready()) else {
            return ClassifierOutcome {
                error: Some("the classifier Provider has no ready Key".to_string()),
                ..Default::default()
            };
        };
        let Some(credential) = upstream
            .credentials
            .iter()
            .find(|credential| credential.id == key.credential_id)
        else {
            return ClassifierOutcome {
                error: Some("the classifier Key is no longer configured".to_string()),
                ..Default::default()
            };
        };
        let secret = match resolve_key_secret(credential) {
            Ok(Some(secret)) => secret,
            Ok(None) => {
                return ClassifierOutcome {
                    error: Some("the classifier Key has no Secret".to_string()),
                    ..Default::default()
                };
            }
            Err(error) => {
                return ClassifierOutcome {
                    error: Some(format!(
                        "the classifier Key could not be read: {}",
                        error.code
                    )),
                    ..Default::default()
                };
            }
        };
        let user = request
            .first_user_text()
            .map(str::trim)
            .filter(|text| !text.is_empty())
            .unwrap_or("(the turn has no user text)");
        let user = if user.chars().count() > 4_000 {
            user.chars().take(4_000).collect::<String>()
        } else {
            user.to_string()
        };
        let body = serde_json::json!({
            "model": model,
            "messages": [
                {
                    "role": "system",
                    "content": "Classify this coding-agent turn. Reply with JSON only: {\"intent\":\"code|debug|refactor|explain|test|docs|plan|other\",\"effort\":\"low|medium|high\"}. Use the effort the task itself needs, not the agent's default."
                },
                {"role": "user", "content": user}
            ],
            "max_tokens": 64,
            "temperature": 0,
            "stream": false
        });
        let url = match endpoint_url(&endpoint.url, protocol, &model, false) {
            Ok(url) => url,
            Err(error) => {
                return ClassifierOutcome {
                    error: Some(format!(
                        "the classifier address is unusable: {}",
                        error.message()
                    )),
                    ..Default::default()
                };
            }
        };
        let client = self.client_for_key(credential.proxy_url.as_deref(), &upstream.metadata);
        let http_request = apply_auth(
            client.post(&url).json(&body),
            protocol,
            &secret,
            upstream.metadata.auth_header_mode.as_deref(),
        );
        let sent = tokio::time::timeout(std::time::Duration::from_secs(8), async {
            let response = http_request
                .send()
                .await
                .map_err(|error| error.to_string())?;
            let status = response.status();
            let bytes = response.bytes().await.map_err(|error| error.to_string())?;
            Ok::<_, String>((status, bytes))
        })
        .await;
        let (status, bytes) = match sent {
            Ok(Ok(result)) => result,
            Ok(Err(error)) => {
                return ClassifierOutcome {
                    error: Some(format!("the classifier request failed: {error}")),
                    ..Default::default()
                };
            }
            Err(_) => {
                return ClassifierOutcome {
                    error: Some("the classifier timed out after 8 seconds".to_string()),
                    ..Default::default()
                };
            }
        };
        if !status.is_success() {
            return ClassifierOutcome {
                error: Some(format!(
                    "the classifier answered HTTP {}: {}",
                    status.as_u16(),
                    truncate(&upstream_message(&String::from_utf8_lossy(&bytes)))
                )),
                ..Default::default()
            };
        }
        let Some(text) = classifier_reply_text(protocol, &bytes) else {
            return ClassifierOutcome {
                error: Some("the classifier reply had no text".to_string()),
                ..Default::default()
            };
        };
        let parsed = classifier_decision(&text);
        if parsed.intent.is_none() && parsed.effort.is_none() {
            return ClassifierOutcome {
                error: Some("the classifier reply was not usable".to_string()),
                ..Default::default()
            };
        }
        parsed
    }

    /// What the router knows about each upstream right now.
    ///
    /// Usage is the same figure the token ceiling uses; readiness comes from
    /// the account states the snapshot already holds; latency is the bounded
    /// in-memory history of completed requests; inflight is read from the
    /// request table. A Provider missing from the map counts as unspent and
    /// healthy enough to try, which is how a newly added member gets a turn.
    fn member_observations(
        &self,
        snapshot: &GatewaySnapshot,
    ) -> Vec<(ModelProviderProfileId, crate::route::MemberObservation)> {
        let inflight = self
            .inflight
            .read()
            .map(|entries| {
                let mut counts = BTreeMap::<ModelProviderProfileId, u32>::new();
                for entry in entries.iter() {
                    *counts
                        .entry(entry.model_provider_profile_id.clone())
                        .or_insert(0) += 1;
                }
                counts
            })
            .unwrap_or_default();
        let latencies = self
            .recent_latency
            .lock()
            .map(|entries| entries.clone())
            .unwrap_or_default();
        snapshot
            .upstreams
            .iter()
            .map(|upstream| {
                let used = upstream
                    .keys
                    .first()
                    .map(|key| key.used_tokens)
                    .unwrap_or(0);
                let samples = latencies.get(&upstream.profile_id);
                let (p50_ms, p95_ms) = samples
                    .map(|samples| (percentile(samples, 0.5), percentile(samples, 0.95)))
                    .unwrap_or((None, None));
                let cooling = !upstream.keys.is_empty()
                    && !upstream.keys.iter().any(|key| key.state.is_ready());
                (
                    upstream.profile_id.clone(),
                    crate::route::MemberObservation {
                        used_tokens: used,
                        p50_ms,
                        p95_ms,
                        cooling,
                        inflight: inflight.get(&upstream.profile_id).copied().unwrap_or(0),
                    },
                )
            })
            .collect()
    }

    /// Remembers one completed request's latency, keeping the window bounded.
    fn note_latency(&self, profile: &ModelProviderProfileId, latency_ms: i64) {
        if latency_ms <= 0 {
            return;
        }
        if let Ok(mut entries) = self.recent_latency.lock() {
            let window = entries.entry(profile.clone()).or_default();
            window.push_back(latency_ms);
            while window.len() > 32 {
                window.pop_front();
            }
        }
    }

    /// Resolves one member into everything needed to call it.
    #[allow(clippy::too_many_arguments)]
    fn prepare_member(
        &self,
        snapshot: &GatewaySnapshot,
        request: &CanonicalRequest,
        inbound: WireProtocol,
        original_body: &Value,
        resolved: &ResolvedMember,
        connection: &DbConnection,
        pinned: Option<&(ModelProviderProfileId, RequestId)>,
        now: i64,
        tool_names: &ToolNameMap,
    ) -> GatewayResult<PreparedMember> {
        let profile_id = resolved.profile.clone();
        let upstream =
            snapshot
                .upstream(&profile_id)
                .cloned()
                .ok_or_else(|| GatewayError::ModelNotFound {
                    model: request.model.clone(),
                    closest: Vec::new(),
                })?;

        // What this endpoint was seen to answer outranks a guess. Only an
        // endpoint that declares nothing can have a lesson: a typed one has
        // already stated its protocol.
        let untyped = upstream
            .untyped_endpoint()
            .map(|endpoint| endpoint.id.clone());
        let learned = untyped.as_deref().and_then(|endpoint_id| {
            self.learned_protocol(connection, &profile_id, endpoint_id)
                .ok()
                .flatten()
        });
        // A vendor file reference only means anything to the Responses API, and
        // the check has to know what this endpoint really speaks: an untyped one
        // that was observed to answer Responses does support it, and refusing
        // would take away a call it has already served.
        let speaks_responses =
            upstream.supports(WireProtocol::Responses) || learned == Some(WireProtocol::Responses);
        if request.has_vendor_file_image() && !speaks_responses {
            return Err(GatewayError::UnsupportedImageReference);
        }
        let plan = choose_target(inbound, &upstream, learned, &snapshot.settings.policy)?;
        let target = plan.target;
        let passthrough = plan.passthrough;
        let endpoint = upstream.endpoint_for_call(target).cloned().ok_or_else(|| {
            GatewayError::TranslationDisabled {
                wire_protocol_id: target.wire_protocol_id().to_string(),
            }
        })?;

        let policy = &snapshot.settings.policy;
        let states = ModelProviderAccountStateRepository::list(connection, &profile_id)?;
        let state_by_key = states
            .into_iter()
            .map(|state| (state.credential_id.clone(), state))
            .collect::<BTreeMap<_, _>>();

        let mut candidates = Vec::new();
        let preferred = policy.preferred_keys.get(&profile_id);
        let bare_model = bare_model_name(&request.model);
        for (index, credential) in upstream.credentials.iter().enumerate() {
            // A Key with a model scope serves only those ids. An empty scope
            // keeps the pre-existing "this Key serves the Provider" meaning.
            if !credential.models.is_empty()
                && !credential.models.iter().any(|model| model == &bare_model)
            {
                continue;
            }
            let state = state_by_key
                .get(&credential.id)
                .cloned()
                .unwrap_or_else(|| {
                    ModelProviderAccountState::ready(profile_id.clone(), credential.id.clone())
                });
            // A preferred Key leads the pool; every other Key keeps the
            // position the Provider gave it.
            let mut candidate =
                CandidateKey::new(profile_id.clone(), credential.id.clone(), index as i64);
            if preferred == Some(&credential.id) {
                candidate = candidate.preferred();
            }
            if let Some(limit) = policy.token_limit {
                candidate = candidate.with_token_limit(limit, snapshot.gateway_used_tokens);
            }
            candidate = match state.visibility_at(now) {
                vibex_core::GatewayKeyState::Ready => candidate,
                vibex_core::GatewayKeyState::Cooling { kind, .. } => candidate.cooling(kind),
                vibex_core::GatewayKeyState::Invalid => candidate.cooling(GatewayFailureKind::Auth),
                vibex_core::GatewayKeyState::Disabled => {
                    candidate.cooling(GatewayFailureKind::Other)
                }
            };
            candidates.push((candidate, credential.clone()));
        }
        if candidates.is_empty() {
            return Err(GatewayError::NotConfigured(format!(
                "`{}` has no API Key",
                upstream.display_name
            )));
        }

        // A pin only applies to the Provider it names. Another member of the
        // same route must not inherit it, or a conversation would land on a Key
        // that belongs to a different vendor.
        let member_pin = pinned
            .filter(|(profile, _)| profile == &profile_id)
            .cloned();
        let key_plan = plan_route(RouteRequest {
            candidates: candidates
                .iter()
                .map(|(candidate, _)| candidate.clone())
                .collect(),
            // The member order is the group's strategy, applied by the caller;
            // within the chosen member the Keys follow the group's own Key
            // strategy. Neither is a gateway-wide setting any more.
            strategy: resolved.key_strategy,
            rotation: self.rotation.fetch_add(1, Ordering::Relaxed),
            pinned: member_pin,
        });
        if key_plan.attemptable().count() == 0 {
            return Err(GatewayError::AllKeysCooling {
                model: request.model.clone(),
                retry_after_ms: None,
            });
        }
        let keys = key_plan
            .attemptable()
            .filter_map(|candidate| {
                candidates
                    .iter()
                    .find(|(existing, _)| existing.credential == candidate.credential)
                    .cloned()
            })
            .collect::<Vec<_>>();

        // A member may pin the reasoning effort, in which case it replaces what
        // the Agent asked for rather than adding to it.
        let member_request = match resolved.reasoning_effort.as_deref() {
            Some(effort) => {
                let mut member_request = request.clone();
                member_request.reasoning = ReasoningRequest {
                    disabled: effort.eq_ignore_ascii_case("off"),
                    effort: Some(effort.to_string()),
                };
                member_request
            }
            None => request.clone(),
        };
        let upstream_stream = request.stream || !passthrough;
        let mut body = build_member_body(
            target,
            passthrough,
            inbound,
            original_body,
            request,
            &member_request,
            &resolved.model,
            upstream_stream,
        )?;
        // A Gemini client may name a tool with `.` or `:`, which the OpenAI and
        // Anthropic protocols refuse; the whole request fails on it, not just
        // that tool. Only a translated call is rewritten: a passthrough has to
        // stay the client's own bytes, and its reply is not re-encoded, so a
        // rewritten name could never be restored on the way back. Every protocol
        // that admits such a name (Gemini is the only one) is translated, so
        // this covers the real cases without touching byte fidelity.
        if !passthrough {
            tool_names.rewrite_body(&mut body, target);
        }
        // The other calls this member may make. They exist only when the first
        // one was a guess at an endpoint that declares nothing, so a wrong
        // guess costs one extra request instead of every request. A corrected
        // call is always a translation of a body the endpoint refused, never a
        // passthrough of it.
        let mut alternates = Vec::new();
        for (alternate_target, alternate_passthrough) in &plan.alternates {
            let Some(endpoint) = upstream.endpoint_for_call(*alternate_target).cloned() else {
                continue;
            };
            let mut body = build_member_body(
                *alternate_target,
                *alternate_passthrough,
                inbound,
                original_body,
                request,
                &member_request,
                &resolved.model,
                request.stream || !alternate_passthrough,
            )?;
            tool_names.rewrite_body(&mut body, *alternate_target);
            alternates.push(MemberCall {
                target: *alternate_target,
                passthrough: *alternate_passthrough,
                endpoint,
                body,
            });
        }
        Ok(PreparedMember {
            profile_id,
            provider_name: upstream.display_name.clone(),
            model: resolved.model.clone(),
            target,
            passthrough,
            endpoint,
            metadata: upstream.metadata.clone(),
            body,
            alternates,
            keys,
            limits: policy.account_limits.clone(),
            // The ledger keeps this much of the routing decision even when the
            // first attempt answers: "the gateway had to step over a cooling
            // Key" is exactly what a later failover investigation asks for.
            skipped_cooling: key_plan.skipped_cooling,
            skipped_over_limit: key_plan.skipped_over_limit,
            reasoning_effort: resolved.reasoning_effort.clone(),
            first_token_timeout_ms: resolved.first_token_timeout_ms,
        })
    }

    /// What this untyped endpoint was seen to answer, if it was ever tried.
    fn learned_protocol(
        &self,
        connection: &DbConnection,
        profile: &ModelProviderProfileId,
        endpoint_id: &str,
    ) -> GatewayResult<Option<WireProtocol>> {
        let key = (profile.clone(), endpoint_id.to_string());
        if let Ok(cache) = self.lessons.read()
            && let Some(protocol) = cache.get(&key)
        {
            return Ok(Some(*protocol));
        }
        let stored = GatewayEndpointProtocolRepository::get(connection, profile, endpoint_id)
            .map_err(GatewayError::from)?;
        let Some(protocol) = stored
            .as_deref()
            .and_then(WireProtocol::from_wire_protocol_id)
        else {
            return Ok(None);
        };
        if let Ok(mut cache) = self.lessons.write() {
            cache.insert(key, protocol);
        }
        Ok(Some(protocol))
    }

    /// Records which protocol an untyped endpoint actually answered.
    ///
    /// This is the "remember" half of trying once: the next request goes
    /// straight to the protocol that worked, so a guess is paid for at most
    /// once per endpoint rather than on every call.
    fn remember_protocol(
        &self,
        profile: &ModelProviderProfileId,
        endpoint_id: &str,
        protocol: WireProtocol,
    ) {
        if let Ok(mut cache) = self.lessons.write() {
            cache.insert((profile.clone(), endpoint_id.to_string()), protocol);
        }
        let Ok(connection) = self.connection() else {
            return;
        };
        let _ = GatewayEndpointProtocolRepository::remember(
            &connection,
            profile,
            endpoint_id,
            protocol.wire_protocol_id(),
            vibex_core::unix_timestamp_ms(),
        );
    }

    /// Sends one attempt, trying the prepared candidates in order until one
    /// answers with a success status. A failure before any byte reaches the
    /// client may fail over; the caller decides whether that is still allowed.
    pub(crate) async fn open_attempt(
        &self,
        prepared: &PreparedCall,
    ) -> GatewayResult<(UpstreamAttempt, KeyReference)> {
        if self.shutting_down.read().map(|flag| *flag).unwrap_or(false) {
            self.finish_attempt(
                prepared.group_id.as_deref().unwrap_or_default(),
                prepared.probe,
            );
            return Err(GatewayError::NotConfigured(
                "the gateway is shutting down".to_string(),
            ));
        }
        let mut last_error: Option<GatewayError> = None;
        // Every Key this loop walks past, with the reason it did not answer.
        // This is what the ledger's route trace is built from, so it records
        // the decision rather than a formatted sentence about it.
        let mut failed_attempts: Vec<GatewayRouteAttempt> = Vec::new();
        let mut skipped_members: Vec<vibex_core::GatewaySkippedMember> = Vec::new();
        // Members are tried in the order the route (or the single upstream)
        // gave them. A member that fails before any byte reaches the client
        // hands over to the next one; once a stream has started, the caller
        // decides and this loop never retries.
        //
        // A failure on the member the conversation was *pinned* to is different
        // from any other failure: that member was chosen on purpose, so leaving
        // it is a decision. Which decision it is belongs to the route, and
        // asking is what `prepared.failover` reports.
        for (index, member) in prepared.members.iter().enumerate() {
            let pinned_member = index == 0 && prepared.pinned;
            // Announce the attempt before it is sent: a client draws the dot
            // moving to this member while the call is in flight, which is the
            // only moment the information is useful.
            self.note_inflight(vibex_core::GatewayInflight {
                request_id: prepared.request_id.clone(),
                agent_id: prepared.agent_id.clone(),
                model_route_id: prepared.group_id.clone(),
                requested_model: prepared.requested_model.clone(),
                model_provider_profile_id: member.profile_id.clone(),
                // One-based: the graph numbers sources as "1st, 2nd".
                attempt: (index as u32).saturating_add(1),
                credential_id: member
                    .keys
                    .first()
                    .map(|(_, credential)| credential.id.clone()),
                wire_protocol_id: member.target.wire_protocol_id().to_string(),
                started_at_ms: vibex_core::unix_timestamp_ms(),
                first_token_at_ms: None,
            });
            match self
                .open_member_attempt(prepared, member, &mut failed_attempts, &mut skipped_members)
                .await
            {
                Ok((response, target, passthrough, credential)) => {
                    // Prove the first token *before* committing to this member
                    // when another member is left to try. A vendor may accept
                    // the connection and then stall; waiting for the caller's
                    // stream to notice would mean the reply had already
                    // started, and failover is only possible before the first
                    // byte reaches the client. The last member is deliberately
                    // excepted: there is nowhere to fail over to, so it waits.
                    let mut body = response.bytes_stream().boxed();
                    let mut prefetched = Vec::new();
                    if prepared.stream
                        && prepared.may_try_next(index)
                        && let Some(timeout_ms) = member.first_token_timeout_ms
                    {
                        let deadline = std::time::Duration::from_millis(timeout_ms);
                        match tokio::time::timeout(deadline, body.next()).await {
                            Ok(Some(Ok(chunk))) => prefetched.push(chunk),
                            Ok(Some(Err(_))) | Ok(None) => {
                                skipped_members.push(vibex_core::GatewaySkippedMember {
                                    model_provider_profile_id: member.profile_id.clone(),
                                    wire_protocol_id: member.target.wire_protocol_id().to_string(),
                                    failure: GatewayFailureKind::Upstream,
                                });
                                last_error = Some(GatewayError::Upstream {
                                    status: 502,
                                    message: "the upstream reply ended before its first token"
                                        .to_string(),
                                });
                                continue;
                            }
                            Err(_) => {
                                skipped_members.push(vibex_core::GatewaySkippedMember {
                                    model_provider_profile_id: member.profile_id.clone(),
                                    wire_protocol_id: member.target.wire_protocol_id().to_string(),
                                    failure: GatewayFailureKind::Upstream,
                                });
                                last_error = Some(GatewayError::Upstream {
                                    status: 504,
                                    message: "the member produced no first token in time"
                                        .to_string(),
                                });
                                continue;
                            }
                        }
                    }
                    if let Some(group_id) = prepared.group_id.as_deref() {
                        self.note_success(group_id);
                    }
                    self.clear_failure(&member.profile_id, &credential);
                    return Ok((
                        UpstreamAttempt {
                            body,
                            prefetched,
                            target,
                            passthrough,
                            profile_id: member.profile_id.clone(),
                            failed_attempts: failed_attempts.clone(),
                            skipped_members: skipped_members.clone(),
                        },
                        KeyReference { id: credential },
                    ));
                }
                Err(error) => {
                    // A member that could not answer is recorded so the ledger
                    // can explain the walk. Only a failure before the reply
                    // started may fail over, which is what `open_attempt`'s
                    // caller enforces; here every error is pre-reply.
                    skipped_members.push(vibex_core::GatewaySkippedMember {
                        model_provider_profile_id: member.profile_id.clone(),
                        wire_protocol_id: member.target.wire_protocol_id().to_string(),
                        failure: member_failure_kind(&error),
                    });
                    let can_try_next = prepared.may_try_next(index);
                    if !can_try_next {
                        last_error = Some(error);
                        break;
                    }
                    // The pinned member failed. Either the route already said
                    // to move on, or it asked, and the answer decides.
                    if pinned_member && let Some(next) = prepared.members.get(index + 1) {
                        match self.decide_failover(prepared, member, next, &error).await {
                            FailoverDecision::Switch { announce } => {
                                if announce {
                                    self.publish_event(
                                        vibex_core::GatewayEvent::FailoverSwitched {
                                            request_id: prepared.request_id.clone(),
                                            model_route_id: prepared.group_id.clone(),
                                            model: bare_model_name(&prepared.requested_model),
                                            from_name: member.provider_name.clone(),
                                            to_name: next.provider_name.clone(),
                                        },
                                    );
                                }
                            }
                            FailoverDecision::Stay => {
                                last_error = Some(error);
                                break;
                            }
                        }
                    }
                    last_error = Some(error);
                }
            }
        }
        let error = last_error.unwrap_or_else(|| GatewayError::AllKeysCooling {
            model: prepared.requested_model.clone(),
            retry_after_ms: None,
        });
        // A request that never reached an upstream still reaches the ledger.
        // Without this row the only record of the failure was the message the
        // Agent showed, and the ledger said every request had succeeded — which
        // is exactly the state an investigation cannot start from.
        let first_member = prepared.first_member();
        let outcome = RequestOutcome {
            kind: GatewayRequestOutcome::Failed,
            profile_id: first_member.map(|member| member.profile_id.clone()),
            target: first_member.map(|member| member.target),
            skipped_cooling: first_member
                .map(|member| member.skipped_cooling)
                .unwrap_or(0),
            skipped_over_limit: first_member
                .map(|member| member.skipped_over_limit)
                .unwrap_or(0),
            credential: None,
            status: Some(error.status()),
            usage: CanonicalUsage::default(),
            latency_ms: None,
            served: None,
            failed_attempts: failed_attempts.clone(),
            skipped_members: skipped_members.clone(),
            error_code: Some(error.code().to_string()),
            error_message: Some(vibex_core::gateway_diagnostic_text(&error.message())),
        };
        self.record(prepared, &outcome);
        Err(error)
    }

    /// Decides what a pinned member's failure means for the conversation.
    ///
    /// `Auto` moves on and says so afterwards. `Ask` puts the question to the
    /// runtime and waits, bounded by the prompt's own countdown; the answer can
    /// also settle the question for the whole route from then on.
    async fn decide_failover(
        &self,
        prepared: &PreparedCall,
        from: &PreparedMember,
        to: &PreparedMember,
        error: &GatewayError,
    ) -> FailoverDecision {
        // Only a failure that blames the member justifies leaving it. A wrong
        // protocol or an unusable Key is the gateway's own problem, and moving
        // the conversation would punish a Provider for the gateway's mistake.
        if !error.blames_upstream() {
            return FailoverDecision::Switch { announce: false };
        }
        if prepared.failover == vibex_core::GatewayFailover::Auto {
            return FailoverDecision::Switch { announce: true };
        }
        let now = vibex_core::unix_timestamp_ms();
        let prompt = vibex_core::GatewayFailoverPrompt {
            id: vibex_core::RequestId::new(),
            model_route_id: prepared.group_id.clone().unwrap_or_default(),
            route_name: prepared.route_name.clone(),
            model: prepared.requested_model.clone(),
            from_profile_id: from.profile_id.clone(),
            from_name: from.provider_name.clone(),
            to_profile_id: to.profile_id.clone(),
            to_name: to.provider_name.clone(),
            expires_at_ms: now + vibex_core::GatewayFailoverPrompt::ANSWER_WINDOW_MS,
            asked_at_ms: now,
        };
        match self.ask_failover(prompt.clone()).await {
            // Nobody to ask: behave like the automatic setting, because the
            // request still has to be served and there is no one to overrule.
            None => FailoverDecision::Switch { announce: true },
            Some(vibex_core::GatewayFailoverAnswer::Stay) => FailoverDecision::Stay,
            Some(vibex_core::GatewayFailoverAnswer::AlwaysSwitch) => {
                self.remember_route_failover(&prompt.model_route_id);
                FailoverDecision::Switch { announce: true }
            }
            Some(vibex_core::GatewayFailoverAnswer::Switch) => {
                FailoverDecision::Switch { announce: true }
            }
        }
    }

    /// Turns off asking for one route, durably.
    ///
    /// The prompt offers this as a one-click answer, so the write has to be the
    /// same one the route's own settings page performs: a choice made under
    /// pressure must survive the next restart, or it was not really a setting.
    fn remember_route_failover(&self, model_route_id: &str) {
        if model_route_id.is_empty() {
            return;
        }
        let Ok(connection) = self.connection() else {
            return;
        };
        let Ok(route_id) = vibex_core::ModelRouteId::parse(model_route_id) else {
            return;
        };
        let Ok(Some(mut route)) = vibex_db::ModelRouteRepository::get(&connection, &route_id)
        else {
            return;
        };
        if route.failover == vibex_core::GatewayFailover::Auto {
            return;
        }
        let revision = route.revision;
        route.failover = vibex_core::GatewayFailover::Auto;
        route.revision = revision.saturating_add(1);
        route.updated_at_ms = vibex_core::unix_timestamp_ms();
        if vibex_db::ModelRouteRepository::update(&connection, &route, revision).is_ok() {
            // The snapshot carries the route's policy, so it has to be rebuilt
            // before the next request reads the old answer.
            self.invalidate_snapshot();
        }
    }

    /// Tries every Key of one member until one answers.
    ///
    /// Returns the response and the credential that produced it. The caller
    /// owns the decision to move to another member, so this function never
    /// retries across members itself.
    fn client_for_key(
        &self,
        proxy: Option<&str>,
        metadata: &vibex_core::ModelProviderMetadata,
    ) -> reqwest::Client {
        let proxy = proxy.map(str::trim).filter(|value| !value.is_empty());
        if proxy.is_none()
            && !metadata.tls_accept_invalid
            && metadata.connect_timeout_ms.is_none()
            && metadata.request_timeout_ms.is_none()
        {
            return self.client.clone();
        }
        let mut builder = reqwest::Client::builder()
            .connect_timeout(
                metadata
                    .connect_timeout_ms
                    .map(std::time::Duration::from_millis)
                    .unwrap_or(self.limits.connect_timeout),
            )
            .timeout(
                metadata
                    .request_timeout_ms
                    .map(std::time::Duration::from_millis)
                    .unwrap_or(std::time::Duration::from_secs(600)),
            )
            .danger_accept_invalid_certs(metadata.tls_accept_invalid)
            .user_agent("vibex-model-gateway/1");
        if let Some(proxy) = proxy {
            match reqwest::Proxy::all(proxy) {
                Ok(proxy) => builder = builder.proxy(proxy),
                Err(_) => {
                    tracing::warn!(
                        target: "vibex.model_gateway",
                        "ignoring an unusable per-Key proxy url"
                    );
                }
            }
        }
        builder.build().unwrap_or_else(|_| self.client.clone())
    }

    async fn open_member_attempt(
        &self,
        prepared: &PreparedCall,
        member: &PreparedMember,
        attempts: &mut Vec<GatewayRouteAttempt>,
        _members: &mut Vec<vibex_core::GatewaySkippedMember>,
    ) -> GatewayResult<(reqwest::Response, WireProtocol, bool, RequestId)> {
        let mut last_error: Option<GatewayError> = None;
        // The calls this member may make, in order. Usually one; a member on an
        // endpoint that declares nothing gets the rest of the protocols as
        // well, and those exist only to correct a wrong guess.
        let calls = std::iter::once(MemberCall {
            target: member.target,
            passthrough: member.passthrough,
            endpoint: member.endpoint.clone(),
            body: member.body.clone(),
        })
        .chain(member.alternates.iter().cloned())
        .collect::<Vec<_>>();
        // More than one call means the first is a guess, and a guess that pays
        // off is worth writing down.
        let guessing = calls.len() > 1;
        for (candidate, credential) in member.keys.iter() {
            let secret = match resolve_key_secret(credential) {
                Ok(Some(secret)) => secret,
                Ok(None) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Auth,
                    });
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key",
                        member.provider_name
                    )));
                    continue;
                }
                Err(error) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Auth,
                    });
                    last_error = Some(GatewayError::NotConfigured(format!(
                        "`{}` has no usable Key: {}",
                        member.provider_name, error.code
                    )));
                    continue;
                }
            };
            // Hold this account's slot for the whole attempt, and take its
            // minute before anything leaves. A permit is released on drop, so a
            // cancelled or failed request cannot leak capacity.
            let limits = member
                .limits
                .get(&candidate.credential)
                .copied()
                .unwrap_or_default();
            let _permit = match self
                .limiter
                .acquire(
                    candidate.credential.as_str(),
                    limits.max_concurrency,
                    limits.queue_limit,
                    limits.queue_wait(),
                    std::time::Instant::now(),
                )
                .await
            {
                Ok(permit) => permit,
                Err(error) => {
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Rate,
                    });
                    last_error = Some(error);
                    continue;
                }
            };
            if let Err(error) = self
                .limiter
                .reserve_minute(
                    candidate.credential.as_str(),
                    limits.max_rpm,
                    limits.queue_wait(),
                )
                .await
            {
                attempts.push(GatewayRouteAttempt {
                    credential_id: candidate.credential.clone(),
                    failure: GatewayFailureKind::Rate,
                });
                last_error = Some(error);
                continue;
            }
            let mut call_error: Option<GatewayError> = None;
            for (call_index, call) in calls.iter().enumerate() {
                let send_model = member
                    .metadata
                    .model_aliases
                    .iter()
                    .find(|alias| alias.alias == member.model)
                    .map(|alias| alias.upstream.clone())
                    .unwrap_or_else(|| member.model.clone());
                let url = endpoint_url(
                    &call.endpoint.url,
                    call.target,
                    &send_model,
                    prepared.stream,
                )?;
                let mut body = call.body.clone();
                if let Some(extra) = member
                    .metadata
                    .extra_body_json
                    .as_deref()
                    .and_then(|json| serde_json::from_str::<serde_json::Value>(json).ok())
                    .and_then(|value| value.as_object().cloned())
                    && let Some(target) = body.as_object_mut()
                {
                    for (key, value) in extra {
                        target.insert(key, value);
                    }
                }
                if send_model != member.model
                    && let Some(target) = body.as_object_mut()
                {
                    target.insert(
                        "model".to_string(),
                        serde_json::Value::String(send_model.clone()),
                    );
                }
                let client = self.client_for_key(credential.proxy_url.as_deref(), &member.metadata);
                let mut request = client.post(&url).json(&body);
                if member.metadata.user_agent_mode.as_deref() == Some("pass_through")
                    && let Some(user_agent) = prepared.user_agent.as_deref()
                {
                    request = request.header(reqwest::header::USER_AGENT, user_agent);
                }
                let mut wire = apply_auth(
                    request,
                    call.target,
                    &secret,
                    member.metadata.auth_header_mode.as_deref(),
                );
                if let Some(beta) = beta_header(&call.target, &call.body) {
                    wire = wire.header("anthropic-beta", beta);
                }
                // A stalled vendor may not answer the response headers at
                // all. The route's deadline has to bound that wait too, or the
                // deadline only covers the stream and the request hangs before
                // it starts.
                let sent = match member.first_token_timeout_ms {
                    Some(timeout) => match tokio::time::timeout(
                        std::time::Duration::from_millis(timeout),
                        wire.send(),
                    )
                    .await
                    {
                        Ok(result) => result,
                        Err(_) => {
                            attempts.push(GatewayRouteAttempt {
                                credential_id: candidate.credential.clone(),
                                failure: GatewayFailureKind::Upstream,
                            });
                            call_error = Some(GatewayError::Upstream {
                                status: 504,
                                message: format!(
                                    "{} did not answer within {} ms",
                                    member.provider_name, timeout
                                ),
                            });
                            // The endpoint was reached; it is just slow. That
                            // says nothing about the protocol, so the alternate
                            // would only wait again.
                            break;
                        }
                    },
                    None => wire.send().await,
                };
                let response = match sent {
                    Ok(response) => response,
                    Err(error) => {
                        let kind = if error.is_timeout() {
                            GatewayFailureKind::Upstream
                        } else {
                            GatewayFailureKind::Proxy
                        };
                        self.mark_failure(&member.profile_id, &candidate.credential, kind);
                        attempts.push(GatewayRouteAttempt {
                            credential_id: candidate.credential.clone(),
                            failure: kind,
                        });
                        call_error = Some(GatewayError::Upstream {
                            status: 502,
                            message: format!(
                                "{}: {}",
                                member.provider_name,
                                redact_transport(&error)
                            ),
                        });
                        break;
                    }
                };
                if response.status().is_success() {
                    // A guess that answered is a fact worth keeping: the next
                    // request starts on the protocol that worked instead of
                    // paying for the same correction again.
                    if guessing {
                        self.remember_protocol(&member.profile_id, &call.endpoint.id, call.target);
                    }
                    return Ok((
                        response,
                        call.target,
                        call.passthrough,
                        candidate.credential.clone(),
                    ));
                }
                let status = response.status().as_u16();
                let body = response.text().await.unwrap_or_default();
                // A shape refusal only means "wrong protocol" when the protocol
                // is still a guess. Once the endpoint's protocol is settled —
                // declared, remembered, or simply the one the client spoke on a
                // passthrough — a body the vendor cannot read is the gateway's
                // own mistake, and every protocol left to try speaks a different
                // shape, so walking them would only fail again and could record
                // the wrong verdict for good.
                let settled = !guessing;
                // A 404 only means "wrong protocol" when another protocol is
                // left to try: on its own it is the address being wrong, and
                // blaming the protocol would send the next request on the same
                // hopeless walk.
                let wrong_shape = is_protocol_mismatch(status, &body)
                    || (status == 404 && call_index + 1 < calls.len());
                if wrong_shape && settled {
                    // The Key worked and the protocol was right; the body the
                    // gateway built was not something the endpoint can read.
                    // Neither the Key nor the group is to blame, and no later
                    // request should be told this endpoint speaks another
                    // protocol, so nothing is remembered here.
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Other,
                    });
                    call_error = Some(GatewayError::TranslationFailed {
                        wire_protocol_id: call.target.wire_protocol_id().to_string(),
                        status,
                        message: upstream_message(&body),
                    });
                    // Another protocol cannot fix a body this one cannot read.
                    break;
                }
                if wrong_shape {
                    // The Key worked; the body was addressed to the wrong API.
                    // Cooling the Key here is what turned one wrong guess into
                    // a gateway-wide outage.
                    attempts.push(GatewayRouteAttempt {
                        credential_id: candidate.credential.clone(),
                        failure: GatewayFailureKind::Other,
                    });
                    call_error = Some(GatewayError::ProtocolMismatch {
                        wire_protocol_id: call.target.wire_protocol_id().to_string(),
                        status,
                        message: upstream_message(&body),
                    });
                    // Another protocol may still answer, so try the alternate;
                    // the same protocol on another Key cannot.
                    if call_index + 1 < calls.len() {
                        continue;
                    }
                    break;
                }
                let kind = classify_failure(status, &body);
                self.mark_failure(&member.profile_id, &candidate.credential, kind);
                attempts.push(GatewayRouteAttempt {
                    credential_id: candidate.credential.clone(),
                    failure: kind,
                });
                call_error = Some(GatewayError::Upstream {
                    status,
                    message: upstream_message(&body),
                });
                // A Key-level failure is not a protocol problem: the alternate
                // would repeat the same rejection on a different body shape.
                if !kind.should_fail_over() {
                    break;
                }
                break;
            }
            if let Some(error) = call_error {
                last_error = Some(error);
            }
        }
        Err(last_error.unwrap_or_else(|| GatewayError::AllKeysCooling {
            model: prepared.requested_model.clone(),
            retry_after_ms: None,
        }))
    }

    fn mark_failure(
        &self,
        profile: &ModelProviderProfileId,
        credential: &RequestId,
        kind: GatewayFailureKind,
    ) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let mut state = ModelProviderAccountStateRepository::list(&connection, profile)
            .unwrap_or_default()
            .into_iter()
            .find(|state| &state.credential_id == credential)
            .unwrap_or_else(|| {
                ModelProviderAccountState::ready(profile.clone(), credential.clone())
            });
        state.consecutive_failures = state.consecutive_failures.saturating_add(1);
        state.updated_at_ms = now;
        let override_ms = ModelGatewaySettingsRepository::get_or_create(&connection)
            .ok()
            .and_then(|settings| {
                settings
                    .policy
                    .cooldown_overrides_ms
                    .get(kind.id())
                    .copied()
            });
        match KeyHealth::from_failure(kind, now) {
            KeyHealth::Invalid => {
                state.cooldown_reason = Some(GatewayFailureKind::Auth);
                state.cooldown_until_ms = None;
            }
            KeyHealth::Cooling { kind, until_ms } => {
                state.cooldown_reason = Some(kind);
                state.cooldown_until_ms = Some(
                    override_ms
                        .map(|milliseconds| now.saturating_add(milliseconds as i64))
                        .unwrap_or(until_ms),
                );
            }
            KeyHealth::Ready => {
                state.cooldown_reason = None;
                state.cooldown_until_ms = None;
            }
        }
        let _ = ModelProviderAccountStateRepository::upsert(&connection, &state);
    }

    fn clear_failure(&self, profile: &ModelProviderProfileId, credential: &RequestId) {
        if let Ok(connection) = self.connection() {
            let _ = ModelProviderAccountStateRepository::clear_cooldown(
                &connection,
                profile,
                credential,
            );
        }
    }

    /// Records one finished request in the ledger and in the token window.
    pub(crate) fn record(&self, prepared: &PreparedCall, outcome: &RequestOutcome) {
        // Every finished request lands here, including the ones whose reply was
        // cut. Releasing the probe first keeps a ledger write failure from
        // wedging the one slot a degraded group has.
        if let Some(group_id) = prepared.group_id.as_deref() {
            self.finish_attempt(group_id, prepared.probe);
        }
        // The in-flight table and the event stream are cleared here rather than
        // at each return site: this is the single funnel every finished request
        // passes through, so nothing can be left drawn as still running.
        self.note_finished(&prepared.request_id, outcome.kind);
        if let (Some(profile), Some(latency)) = (outcome.profile_id.as_ref(), outcome.latency_ms) {
            self.note_latency(profile, latency);
        }
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        // A request that answered names the member that did; one that never
        // reached an upstream records the first member it would have tried, so
        // the row is still attributable to something the user can find.
        let answered = outcome.profile_id.clone().or_else(|| {
            prepared
                .first_member()
                .map(|member| member.profile_id.clone())
        });
        let wire_out = outcome
            .target
            .or_else(|| prepared.first_member().map(|member| member.target));
        let log = ModelGatewayRequestLog {
            request_id: prepared.request_id.clone(),
            agent_id: prepared.agent_id.clone(),
            model_provider_profile_id: answered.clone().unwrap_or_else(|| {
                // A request that matched no member at all has no Provider to
                // blame; the ledger requires one, so the requested model stands
                // in as an unreadable id.
                ModelProviderProfileId::new()
            }),
            credential_id: outcome.credential.clone(),
            model_requested: prepared.requested_model.clone(),
            model_served: outcome.served.clone(),
            wire_in: prepared.inbound.wire_protocol_id().to_string(),
            wire_out: wire_out
                .unwrap_or(WireProtocol::ChatCompletions)
                .wire_protocol_id()
                .to_string(),
            status: outcome.status,
            input_tokens: outcome.usage.input_tokens,
            output_tokens: outcome.usage.output_tokens,
            cached_read_tokens: outcome.usage.cached_read_tokens,
            cached_write_tokens: outcome.usage.cached_write_tokens,
            reasoning_tokens: outcome.usage.reasoning_tokens,
            latency_ms: outcome.latency_ms,
            outcome: outcome.kind,
            route: Some(GatewayRouteTrace {
                failed_attempts: outcome.failed_attempts.clone(),
                skipped_members: outcome.skipped_members.clone(),
                error_code: outcome.error_code.clone(),
                error_message: outcome.error_message.clone(),
                decision: (!prepared.route_decision.is_unremarkable())
                    .then(|| prepared.route_decision.clone()),
                ..GatewayRouteTrace::new(outcome.skipped_cooling, outcome.skipped_over_limit)
            }),
            created_at_ms: now,
        };
        let _ = ModelGatewayRequestLogRepository::insert(&connection, &log);
        if outcome.usage.total() > 0
            && let Some(profile_id) = answered.as_ref()
            && let Ok(settings) = ModelGatewaySettingsRepository::get_or_create(&connection)
            && let Some(limit) = settings.policy.token_limit
        {
            let _ = GatewayTokenWindowRepository::record(
                &connection,
                profile_id,
                limit.window,
                outcome.usage.total(),
                now,
            );
        }
    }

    /// Records a request that `prepare` refused.
    ///
    /// A refusal before an upstream is reached is exactly the kind of failure a
    /// user comes to the ledger to find: the Agent showed an error, and without
    /// this row the ledger is empty at that moment, which reads as "the request
    /// never happened". The row names the inbound protocol and the gateway's
    /// own error code; it blames no Provider, because none was consulted.
    pub(crate) fn record_refusal(
        &self,
        inbound: WireProtocol,
        headers: &HeaderMap,
        body: &[u8],
        error: &GatewayError,
    ) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let requested_model = serde_json::from_slice::<Value>(body)
            .ok()
            .and_then(|value| {
                value
                    .get("model")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .unwrap_or_default();
        let log = ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: agent_id_from_headers(headers),
            // A refusal that never matched a member has no Provider to blame;
            // the ledger requires one, so an unreadable id stands in.
            model_provider_profile_id: ModelProviderProfileId::new(),
            credential_id: None,
            model_requested: requested_model,
            model_served: None,
            wire_in: inbound.wire_protocol_id().to_string(),
            wire_out: inbound.wire_protocol_id().to_string(),
            status: Some(error.status()),
            input_tokens: 0,
            output_tokens: 0,
            cached_read_tokens: 0,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: None,
            outcome: GatewayRequestOutcome::Rejected,
            route: Some(GatewayRouteTrace {
                error_code: Some(error.code().to_string()),
                ..GatewayRouteTrace::new(0, 0)
            }),
            created_at_ms: vibex_core::unix_timestamp_ms(),
        };
        let _ = ModelGatewayRequestLogRepository::insert(&connection, &log);
    }

    /// Lists the most recent ledger rows.
    pub fn recent_requests(&self, limit: usize) -> GatewayResult<Vec<ModelGatewayRequestLog>> {
        let connection = self.connection()?;
        ModelGatewayRequestLogRepository::recent(&connection, limit).map_err(GatewayError::from)
    }

    /// Clears the request ledger.
    pub fn clear_requests(&self) -> GatewayResult<usize> {
        let connection = self.connection()?;
        ModelGatewayRequestLogRepository::clear(&connection).map_err(GatewayError::from)
    }

    /// Stores a conversation pin.
    pub(crate) fn pin_conversation(
        &self,
        key: &str,
        profile: &ModelProviderProfileId,
        credential: &RequestId,
    ) {
        let Ok(connection) = self.connection() else {
            return;
        };
        let now = vibex_core::unix_timestamp_ms();
        let _ = GatewayConversationPinRepository::upsert(
            &connection,
            &GatewayConversationPin {
                conversation_key: key.to_string(),
                model_provider_profile_id: profile.clone(),
                credential_id: credential.clone(),
                chosen_at_ms: now,
                expires_at_ms: now + GatewayConversationPin::TTL_MS,
            },
        );
    }

    /// Adds tokens to one Provider's window.
    pub fn record_tokens(
        &self,
        profile: &ModelProviderProfileId,
        window: GatewayTokenWindow,
        tokens: u64,
    ) -> GatewayResult<GatewayTokenWindowUsage> {
        let connection = self.connection()?;
        GatewayTokenWindowRepository::record(
            &connection,
            profile,
            window,
            tokens,
            vibex_core::unix_timestamp_ms(),
        )
        .map_err(GatewayError::from)
    }

    /// The reply a client receives when the service itself refuses a request.
    pub fn error_response(&self, protocol: WireProtocol, error: &GatewayError) -> Response {
        let message = error.message();
        let body = match protocol {
            WireProtocol::ChatCompletions | WireProtocol::Responses => serde_json::json!({
                "error": {"message": message, "type": error.code(), "code": error.code()}
            }),
            WireProtocol::AnthropicMessages => serde_json::json!({
                "type": "error",
                "error": {"type": error.code(), "message": message},
            }),
            WireProtocol::GeminiGenerateContent => serde_json::json!({
                "error": {"code": error.status(), "message": message, "status": error.code()}
            }),
        };
        let mut response = Response::new(Body::from(body.to_string()));
        *response.status_mut() =
            StatusCode::from_u16(error.status()).unwrap_or(StatusCode::BAD_REQUEST);
        response.headers_mut().insert(
            header::CONTENT_TYPE,
            header::HeaderValue::from_static("application/json"),
        );
        if let Some(retry_after) = error.retry_after_ms() {
            let seconds = (retry_after / 1000).max(1).to_string();
            if let Ok(value) = header::HeaderValue::from_str(&seconds) {
                response.headers_mut().insert(header::RETRY_AFTER, value);
            }
        }
        response
    }
}

/// Releases a degraded gateway's probe slot unless the request is handed on.
///
/// `prepare` can leave through a dozen early returns. Holding the slot in a
/// value that releases on drop means none of them has to remember to do it.
struct ProbeGuard<'a> {
    service: &'a ModelGatewayService,
    group: String,
    armed: bool,
}

impl Drop for ProbeGuard<'_> {
    fn drop(&mut self) {
        if self.armed {
            self.service.finish_attempt(&self.group, true);
        }
    }
}

/// One upstream the request may call, before its Keys are planned.
#[derive(Debug, Clone, PartialEq, Eq)]
struct ResolvedMember {
    profile: ModelProviderProfileId,
    model: String,
    reasoning_effort: Option<String>,
    first_token_timeout_ms: Option<u64>,
    /// How this group wants the Keys of one Provider tried.
    ///
    /// It is the group's own choice, not a gateway-wide default: two groups
    /// over the same Provider may want one held Key and one rotating pool.
    key_strategy: GatewayRoutingStrategy,
}

/// The base model of a reasoning-depth model id.
///
/// An Agent may publish one selectable model per depth (`<model>-high`,
/// `<model>-medium`, ...) rather than a separate depth control. Such an id
/// names the *base* model plus a depth, so a group that holds the base model
/// can still serve it. Only the levels those pickers use are stripped, and the
/// result must be non-empty, so an ordinary model called `plan-high` keeps its
/// own name when the group declares it.
pub(crate) fn reasoning_variant_base(model: &str) -> Option<&str> {
    ["-high", "-medium", "-low"]
        .into_iter()
        .find_map(|suffix| model.strip_suffix(suffix).filter(|base| !base.is_empty()))
}

/// The group a request's model id names, when it names one.
///
/// A gateway model is always `group/<id>/<model>`; anything else is not a
/// model this listener serves.
fn resolved_group_id(model: &str) -> Option<&str> {
    vibex_core::route_group_model_ref(model).map(|parsed| parsed.group_id)
}

/// A request that has been resolved but not yet sent.
#[derive(Clone)]
/// What a pinned member's failure means for the conversation.
enum FailoverDecision {
    /// Leave the pinned member. `announce` is false when the failure was not
    /// the member's fault, which is not worth reporting as a switch.
    Switch { announce: bool },
    /// Keep the conversation where it is and let the request fail.
    Stay,
}

#[derive(Clone)]
pub struct PreparedCall {
    pub inbound: WireProtocol,
    pub requested_model: String,
    /// The members to try, in attempt order. A group may hold several
    /// Providers; everything downstream walks this list.
    pub members: Vec<PreparedMember>,
    pub agent_id: Option<String>,
    pub conversation: Option<String>,
    pub stream: bool,
    pub user_agent: Option<String>,
    /// The group that produced this call. A gateway request always names one:
    /// groups are the only models the listener serves.
    pub group_id: Option<String>,
    /// This request holds a degraded gateway's single probe slot and must
    /// release it once an upstream has answered or refused.
    pub probe: bool,
    /// The id this request is traced and recorded under.
    ///
    /// One id for the whole request, not one per attempt: the inflight entry,
    /// the ledger row, and the events a client draws all have to name the same
    /// thing.
    pub request_id: RequestId,
    /// The route's display name, for prompts that must not re-read the route.
    pub route_name: String,
    /// What the route does when the pinned member fails.
    pub failover: vibex_core::GatewayFailover,
    /// How long the route is willing to retry after every member failed
    /// before anything reached the client. `None` means no retry.
    pub patience_ms: Option<u64>,
    /// Why the member order was chosen, for the ledger and the simulator.
    pub route_decision: vibex_core::GatewayRouteDecisionTrace,
    /// Whole-walk retries performed under patience before this attempt.
    pub retries: u32,
    /// Whether the first member is here because the conversation was pinned to
    /// it, rather than because the strategy happened to pick it.
    ///
    /// It gates the failover prompt: only a member that was kept on purpose is
    /// worth asking about, because only that switch is a decision the user made.
    pub pinned: bool,
    /// The tool names as the client spelled them, and the form the strict
    /// protocols accept. A reply that calls a tool names it in the rewritten
    /// form, so translating it back needs this map.
    pub(crate) tool_names: ToolNameMap,
}

/// One upstream this request may call, with everything needed to call it.
///
/// Each member is resolved independently: it has its own endpoint, its own
/// protocol pair, and its own Key order. Two members of one route may speak
/// different protocols to different vendors, so nothing here can be shared.
#[derive(Debug, Clone)]
pub struct PreparedMember {
    pub profile_id: ModelProviderProfileId,
    pub provider_name: String,
    /// The model id this member's upstream is asked for.
    pub model: String,
    pub target: WireProtocol,
    pub passthrough: bool,
    pub endpoint: ModelProviderEndpoint,
    pub metadata: vibex_core::ModelProviderMetadata,
    pub body: Value,
    /// Ordered attempts: the candidate and the credential record it uses.
    pub keys: Vec<(CandidateKey, ModelProviderCredentialReference)>,
    /// Per-credential concurrency and rate limits.
    pub limits: BTreeMap<RequestId, vibex_core::GatewayAccountLimits>,
    /// Keys the plan set aside before the first attempt, and why.
    pub skipped_cooling: usize,
    pub skipped_over_limit: usize,
    /// Reasoning effort this member asks for, overriding the Agent's request.
    pub reasoning_effort: Option<String>,
    /// How long this attempt may take to produce its first token.
    pub first_token_timeout_ms: Option<u64>,
    /// The other protocols to try when the first was the wrong shape.
    ///
    /// Only an endpoint that declares no protocol produces these: the gateway
    /// guessed, and each guess is allowed one correction.
    pub alternates: Vec<MemberCall>,
}

/// One wire call a member may make: the protocol, the address, and the body.
///
/// A member normally has one. A member whose endpoint declares no protocol has
/// two, because the first is a guess and the vendor's refusal is what names the
/// right answer.
#[derive(Debug, Clone)]
pub struct MemberCall {
    pub target: WireProtocol,
    pub passthrough: bool,
    pub endpoint: ModelProviderEndpoint,
    pub body: Value,
}

/// The model name as a reader knows it, without the group prefix.
///
/// A gateway id is `group/<route>/<model>`, which is an address, not a name.
/// The prompt and the switch notice name the model, so they strip the routing
/// prefix: `group/abc/deepseek-chat` reads as `deepseek-chat`.
fn bare_model_name(requested: &str) -> String {
    vibex_core::route_group_model_ref(requested)
        .map(|parsed| parsed.model_id.to_string())
        .unwrap_or_else(|| requested.to_string())
}

/// Moves the pinned member to the front, if it is in the plan at all.
///
/// The strategy still orders the rest, so this only ever changes which member
/// is tried first: a conversation that has to leave the pinned member inherits
/// the group's preferred fallback order rather than an arbitrary one.
fn hoist_pinned<T>(
    plans: &mut Vec<T>,
    pinned: Option<&ModelProviderProfileId>,
    profile: impl Fn(&T) -> &ModelProviderProfileId,
) {
    let Some(pinned) = pinned else {
        return;
    };
    let Some(index) = plans.iter().position(|plan| profile(plan) == pinned) else {
        return;
    };
    if index > 0 {
        let plan = plans.remove(index);
        plans.insert(0, plan);
    }
}

/// Applies a matched rule's explicit member order without dropping fallbacks.
///
/// The starter goes first, `more` follows in the rule's order, and the members
/// the rule did not name keep the order the strategy produced. A rule therefore
/// changes the preference for this turn, not the set of sources available.
fn reorder_for_rule(
    plans: &mut Vec<crate::route::RouteMemberRef>,
    rule: &vibex_core::ModelRouteRule,
) {
    let mut wanted = Vec::new();
    if let Some(member) = &rule.use_member_id {
        wanted.push(member.clone());
    }
    wanted.extend(rule.more_member_ids.iter().cloned());
    if wanted.is_empty() {
        return;
    }
    let mut ordered = Vec::with_capacity(plans.len());
    for member_id in &wanted {
        if let Some(index) = plans
            .iter()
            .position(|plan| plan.member_id.as_ref() == Some(member_id))
            && let Some(plan) = plans.get(index).cloned()
            && !ordered.contains(&plan)
        {
            ordered.push(plan);
        }
    }
    for plan in plans.iter() {
        if !ordered.contains(plan) {
            ordered.push(plan.clone());
        }
    }
    *plans = ordered;
}

/// Everything a rule may be matched against for this inbound request.
fn route_rule_context(
    request: &CanonicalRequest,
    agent_id: Option<&str>,
    headers: &HeaderMap,
    intent: Option<&str>,
) -> vibex_core::RouteRuleContext {
    let now = chrono::Local::now();
    let header = |name: &str| {
        headers
            .get(name)
            .and_then(|value| value.to_str().ok())
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    vibex_core::RouteRuleContext {
        agent_id: agent_id.map(str::to_string),
        context_tokens: Some(estimate_request_tokens(request)),
        has_images: request_has_images(request),
        requested_effort: request.reasoning.effort.clone(),
        compact: header("x-vibex-compact").is_some_and(|value| value.eq_ignore_ascii_case("true")),
        intent: intent
            .map(str::to_string)
            .or_else(|| header("x-vibex-intent")),
        workspace_root: header("x-vibex-workspace"),
        local_weekday: Some(now.weekday().number_from_monday() as u8),
        local_minute: Some((now.hour() * 60 + now.minute()) as u16),
    }
}

/// A deliberately cheap upper-bound estimate of the turn's context.
///
/// The gateway has no tokenizer and must not import one: rules only need a
/// threshold, and four characters per token is close enough to distinguish
/// "short turn" from "a 100k-token compaction". The trace labels it as an
/// estimate rather than presenting it as the vendor's count.
fn estimate_request_tokens(request: &CanonicalRequest) -> u64 {
    let mut chars = 0u64;
    let mut add_parts = |parts: &[crate::ir::ContentPart]| {
        for part in parts {
            if let crate::ir::ContentPart::Text(text) = part {
                chars = chars.saturating_add(text.chars().count() as u64);
            }
        }
    };
    add_parts(&request.system);
    for message in &request.messages {
        add_parts(&message.parts);
    }
    chars.saturating_add(request.tools.len() as u64 * 180) / 4
}

fn request_has_images(request: &CanonicalRequest) -> bool {
    fn any_in(parts: &[crate::ir::ContentPart]) -> bool {
        parts.iter().any(|part| match part {
            crate::ir::ContentPart::Image { .. } => true,
            crate::ir::ContentPart::ToolResult { content, .. } => any_in(content),
            _ => false,
        })
    }
    any_in(&request.system)
        || request
            .messages
            .iter()
            .any(|message| any_in(&message.parts))
}

/// A nearest-rank percentile from a bounded sample window.
///
/// The sample size is small (32), so sorting a copy is cheaper than maintaining
/// an ordered structure and cannot drift from what was observed.
fn percentile(samples: &VecDeque<i64>, fraction: f64) -> Option<u64> {
    if samples.is_empty() {
        return None;
    }
    let mut sorted = samples.iter().copied().collect::<Vec<_>>();
    sorted.sort_unstable();
    let index = ((sorted.len() as f64 - 1.0) * fraction).round() as usize;
    sorted
        .get(index.min(sorted.len() - 1))
        .and_then(|value| u64::try_from(*value).ok())
}

impl PreparedCall {
    /// The member that should answer first.
    pub fn first_member(&self) -> Option<&PreparedMember> {
        self.members.first()
    }

    /// Whether a failure on this member may move to the next one.
    pub fn may_try_next(&self, index: usize) -> bool {
        index + 1 < self.members.len()
    }

    /// The member belonging to a Provider, once one has answered.
    pub fn member(&self, profile: &ModelProviderProfileId) -> Option<&PreparedMember> {
        self.members
            .iter()
            .find(|member| &member.profile_id == profile)
    }
}

impl std::fmt::Debug for PreparedCall {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PreparedCall")
            .field("inbound", &self.inbound)
            .field("requested_model", &self.requested_model)
            .field("members", &self.members.len())
            .field("agent_id", &self.agent_id)
            .field("group_id", &self.group_id)
            .field("stream", &self.stream)
            .finish()
    }
}

/// What one finished request produced, as the ledger records it.
pub(crate) struct RequestOutcome {
    pub kind: GatewayRequestOutcome,
    /// The Provider that answered. Empty when nothing answered.
    pub profile_id: Option<ModelProviderProfileId>,
    /// The protocol the answering member was called with.
    pub target: Option<WireProtocol>,
    /// Keys set aside before the first attempt of the answering member.
    pub skipped_cooling: usize,
    pub skipped_over_limit: usize,
    pub credential: Option<RequestId>,
    pub status: Option<u16>,
    pub usage: CanonicalUsage,
    pub latency_ms: Option<i64>,
    /// The model the upstream said it served, when it said one.
    pub served: Option<String>,
    /// Keys tried before the answering one, in order.
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members walked past before one answered.
    pub skipped_members: Vec<vibex_core::GatewaySkippedMember>,
    /// The gateway's own failure code, when no reply was produced.
    ///
    /// This is what makes a failed request findable: the ledger row otherwise
    /// says only "failed", with nothing linking it to the message the Agent
    /// showed.
    pub error_code: Option<String>,
    /// What the upstream said when it refused, when it said anything.
    pub error_message: Option<String>,
}

/// The credential a request authenticates with.
///
/// Only the id is kept: the resolved Secret exists for the duration of one
/// outbound request and never reaches a log line, a ledger row, or an error.
pub(crate) struct KeyReference {
    pub id: RequestId,
}

pub(crate) struct UpstreamAttempt {
    /// The upstream body as a stream, already advanced past any bytes read
    /// while proving the first token.
    pub body: futures_util::stream::BoxStream<'static, Result<bytes::Bytes, reqwest::Error>>,
    /// Bytes read during the first-token wait, replayed to the caller before
    /// `body` continues.
    pub prefetched: Vec<bytes::Bytes>,
    pub target: WireProtocol,
    pub passthrough: bool,
    /// The member that answered.
    pub profile_id: ModelProviderProfileId,
    /// Keys that did not answer before this one did.
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members walked past before this one answered.
    pub skipped_members: Vec<vibex_core::GatewaySkippedMember>,
}

/// The protocols one member may be called with, best first.
#[derive(Debug, Clone, PartialEq, Eq)]
struct TargetPlan {
    target: WireProtocol,
    passthrough: bool,
    /// The protocols to try when the first one is refused as the wrong shape
    /// for the endpoint, best first.
    ///
    /// It exists only for an address that declares no protocol: the gateway
    /// does not know what the endpoint speaks, so it tries one and, when the
    /// vendor answers that the body belongs to another API, walks the remaining
    /// ones once each. A proven answer is remembered, so the whole walk happens
    /// at most once per endpoint.
    alternates: Vec<(WireProtocol, bool)>,
}

/// The translation order, best fidelity first.
const TRANSLATION_PREFERENCE: [WireProtocol; 4] = [
    // Anthropic first: the translation into Messages keeps cache breakpoints
    // and thinking signatures, which the others cannot carry.
    WireProtocol::AnthropicMessages,
    WireProtocol::ChatCompletions,
    WireProtocol::Responses,
    WireProtocol::GeminiGenerateContent,
];

/// Builds the body one call sends, honouring a passthrough where there is one.
///
/// A same-protocol call must be the client's own bytes: the vendor's own
/// fields (`store`, `previous_response_id`, `prompt_cache_key`, `metadata`,
/// `text.format`, …) are what the prompt cache and the vendor's own state key
/// on, and re-encoding the body silently drops every one this IR does not
/// model. Re-encoding is for translation, where the body has to be rewritten
/// anyway; a passthrough only rewrites `model`, which is the one field the
/// gateway owns.
///
/// A member that overrides the reasoning effort has to be rebuilt: that
/// override is a route setting the client did not send.
#[allow(clippy::too_many_arguments)]
fn build_member_body(
    target: WireProtocol,
    passthrough: bool,
    inbound: WireProtocol,
    original_body: &Value,
    request: &CanonicalRequest,
    member_request: &CanonicalRequest,
    model: &str,
    stream: bool,
) -> GatewayResult<Value> {
    let rewritten_effort = request.reasoning != member_request.reasoning;
    if !passthrough || target != inbound || rewritten_effort {
        return build_upstream_request(target, member_request, model, stream);
    }
    let mut body = original_body.clone();
    let Some(fields) = body.as_object_mut() else {
        return build_upstream_request(target, member_request, model, stream);
    };
    fields.insert("model".to_string(), Value::String(model.to_string()));
    drop_tools_the_target_cannot_express(fields, target, inbound);
    // The stream flag is the gateway's decision, not vendor state: a
    // translated call always asks for a stream so the terminal usage frame
    // exists, and a passthrough keeps whatever the client asked for.
    if fields.get("stream").and_then(Value::as_bool) != Some(stream) {
        fields.insert("stream".to_string(), Value::Bool(stream));
    }
    Ok(body)
}

/// Removes vendor tool types the target protocol cannot express.
///
/// A Responses client may offer built-in tools (`web_search`,
/// `web_search_preview`, `file_search`, ...) that only OpenAI serves. A
/// same-protocol passthrough forwards them verbatim and the upstream answers
/// `400 [code=unsupported_tool] unsupported Responses tool type "web_search"`,
/// which fails the whole turn — including every tool call the Agent could
/// actually make. The gateway's job here is the same as for an unknown vendor
/// field: keep the call the upstream *can* serve rather than lose it. Dropping
/// the tool is recorded as a diagnostic notice so the missing capability is
/// visible instead of silent.
fn drop_tools_the_target_cannot_express(
    fields: &mut serde_json::Map<String, Value>,
    _target: WireProtocol,
    _inbound: WireProtocol,
) {
    let Some(tools) = fields.get_mut("tools").and_then(Value::as_array_mut) else {
        return;
    };
    let mut dropped = Vec::new();
    tools.retain(|tool| {
        // Only the Responses shape tags a tool with `type`. A Chat-shaped
        // `function` object has no `type` at all and is always kept.
        match tool.get("type").and_then(Value::as_str) {
            None | Some("function") => true,
            Some(kind) => {
                // `custom` carries a free-form grammar the Chat route cannot
                // express either; every other built-in is vendor-owned.
                dropped.push(kind.to_string());
                false
            }
        }
    });
    if dropped.is_empty() {
        return;
    }
    tracing::debug!(
        target: "model_gateway.tools",
        dropped = ?dropped,
        "dropped tool types the upstream cannot serve"
    );
    // An empty array is not the same as no `tools` key: some vendors treat the
    // key's presence as "tools are supported" and reject an empty list.
    if tools.is_empty() {
        fields.remove("tools");
    }
}

/// Chooses the protocol the upstream is called with.
///
/// Two statements decide it, strongest first: an endpoint that names its
/// protocol, and what an untyped endpoint was observed to accept. Passthrough
/// wins whenever the upstream states that it speaks what the client spoke: a
/// same-protocol call is forwarded byte for byte, which is what keeps a
/// vendor's prompt cache valid. An untyped endpoint states nothing, so a
/// passthrough there is only a guess and comes back with an `alternate` to fall
/// back on.
fn choose_target(
    inbound: WireProtocol,
    upstream: &GatewayUpstream,
    learned: Option<WireProtocol>,
    policy: &GatewayPolicy,
) -> GatewayResult<TargetPlan> {
    let allowed = |protocol: WireProtocol| policy.translate.allows(protocol.wire_protocol_id());
    // What this endpoint was seen to answer is the strongest statement there
    // is, because it was observed rather than declared.
    if let Some(learned) = learned {
        return Ok(TargetPlan {
            target: learned,
            passthrough: learned == inbound,
            alternates: Vec::new(),
        });
    }
    if upstream.supports(inbound) {
        return Ok(TargetPlan {
            target: inbound,
            passthrough: true,
            alternates: Vec::new(),
        });
    }
    if upstream.untyped_endpoint().is_some() {
        // Nothing declares anything. A passthrough is the cheapest first try
        // and the one that preserves the vendor's prompt cache, so it goes
        // first even though the endpoint might speak another protocol. Every
        // other protocol the policy allows follows, so a wrong guess is
        // corrected instead of becoming a refusal.
        let alternates = TRANSLATION_PREFERENCE
            .into_iter()
            .filter(|protocol| *protocol != inbound && allowed(*protocol))
            .map(|protocol| (protocol, false))
            .collect();
        return Ok(TargetPlan {
            target: inbound,
            passthrough: true,
            alternates,
        });
    }
    for protocol in TRANSLATION_PREFERENCE {
        if upstream.supports(protocol) && allowed(protocol) {
            return Ok(TargetPlan {
                target: protocol,
                passthrough: false,
                alternates: Vec::new(),
            });
        }
    }
    Err(GatewayError::TranslationDisabled {
        wire_protocol_id: inbound.wire_protocol_id().to_string(),
    })
}

/// How a member-level failure should be described in the route trace.
///
/// The distinction the ledger needs is "was this the Key or the whole source".
/// An upstream error keeps its classified kind so the trace still says "rate
/// limited" rather than a generic failure.
fn member_failure_kind(error: &GatewayError) -> GatewayFailureKind {
    match error {
        GatewayError::Upstream { status, message } => classify_failure(*status, message),
        GatewayError::AllKeysCooling { .. } => GatewayFailureKind::Rate,
        GatewayError::TokenCeilingReached { .. } => GatewayFailureKind::Quota,
        // A wrong protocol is not a Key problem, so it must not be described
        // as one: `Auth` would retire the Key for the mistake of the address.
        // A body the gateway built wrongly is the same kind of not-a-Key
        // problem, and reporting it as `Upstream` would cool the Key for it.
        GatewayError::ProtocolMismatch { .. } | GatewayError::TranslationFailed { .. } => {
            GatewayFailureKind::Other
        }
        GatewayError::NotConfigured(_) => GatewayFailureKind::Auth,
        GatewayError::TranslationDisabled { .. }
        | GatewayError::UnsupportedImageReference
        | GatewayError::MalformedRequest(_)
        | GatewayError::RoutePaused { .. } => GatewayFailureKind::Other,
        GatewayError::ModelNotFound { .. } | GatewayError::Internal(_) => GatewayFailureKind::Other,
    }
}

fn agent_id_from_headers(headers: &HeaderMap) -> Option<String> {
    for name in ["x-vibex-agent", "x-agent-id", "user-agent"] {
        if let Some(value) = headers.get(name).and_then(|value| value.to_str().ok()) {
            let value = value.trim();
            if !value.is_empty() && value.len() <= 64 && !value.contains('/') {
                return Some(value.to_string());
            }
        }
    }
    None
}

fn resolve_key_secret(
    credential: &ModelProviderCredentialReference,
) -> vibex_core::VibexResult<Option<String>> {
    let Some(reference) = credential.credential.secret_reference() else {
        return Ok(None);
    };
    secrets::resolve_provider_secret_reference(
        reference.backend,
        reference.setup_state,
        &reference.lookup_key,
    )
}

fn apply_auth(
    request: reqwest::RequestBuilder,
    protocol: WireProtocol,
    secret: &str,
    mode: Option<&str>,
) -> reqwest::RequestBuilder {
    let request = if protocol == WireProtocol::AnthropicMessages {
        request.header("anthropic-version", "2023-06-01")
    } else {
        request
    };
    match mode {
        Some("x-api-key") => request.header("x-api-key", secret),
        Some("bearer") => request.bearer_auth(secret),
        Some("both") => request.bearer_auth(secret).header("x-api-key", secret),
        _ => match protocol {
            WireProtocol::ChatCompletions | WireProtocol::Responses => request.bearer_auth(secret),
            WireProtocol::AnthropicMessages => request.header("x-api-key", secret),
            WireProtocol::GeminiGenerateContent => request.header("x-goog-api-key", secret),
        },
    }
}

/// Anthropic requires the 1-hour cache TTL to be opted into with a beta header.
fn beta_header(protocol: &WireProtocol, body: &Value) -> Option<&'static str> {
    if *protocol != WireProtocol::AnthropicMessages {
        return None;
    }
    body.to_string()
        .contains("\"ttl\":\"1h\"")
        .then_some("extended-cache-ttl-2025-04-11")
}

/// Joins an endpoint base with a protocol path without doubling a version
/// prefix the user already typed into the address.
fn endpoint_url(
    base: &str,
    protocol: WireProtocol,
    model: &str,
    stream: bool,
) -> GatewayResult<String> {
    let base = base.trim().trim_end_matches('/');
    if base.is_empty() {
        return Err(GatewayError::NotConfigured(
            "the upstream Provider has an empty address".to_string(),
        ));
    }
    let path = protocol.path(model, stream);
    for prefix in ["/v1beta", "/v1"] {
        if let Some(rest) = path.strip_prefix(prefix) {
            return Ok(if base.ends_with(prefix) {
                format!("{base}{rest}")
            } else {
                format!("{base}{path}")
            });
        }
    }
    Ok(format!("{base}{path}"))
}

/// Whether a Provider serves a model id or a local alias for one.
fn member_is_served(profile: &vibex_core::ModelProviderProfile, model: &str) -> bool {
    let model = model.trim();
    if profile.configured_models.is_empty() {
        return !model.is_empty();
    }
    profile.configured_models.iter().any(|entry| {
        entry.enabled
            && (entry.id == model
                || profile
                    .metadata
                    .model_aliases
                    .iter()
                    .any(|alias| alias.alias == model && alias.upstream == entry.id))
    })
}

/// The Provider a classifier string names, if it names one in the snapshot.
fn classifier_provider(
    classifier: &str,
    route: &vibex_core::ModelRoute,
    profiles: &BTreeMap<ModelProviderProfileId, vibex_core::ModelProviderProfile>,
) -> Option<ModelProviderProfileId> {
    let classifier = classifier.trim();
    if let Some(member) = route
        .members
        .iter()
        .find(|member| member.id.as_str() == classifier)
    {
        return Some(member.model_provider_profile_id.clone());
    }
    let (prefix, _) = classifier.split_once('/')?;
    profiles
        .values()
        .find(|profile| {
            profile.id.as_str() == prefix
                || crate::route::provider_alias(&profile.display_name) == prefix
        })
        .map(|profile| profile.id.clone())
}

/// Resolves the classifier string to a Provider and upstream model.
///
/// A classifier is either a member id of this route or a `provider/model`
/// pair. Resolving the member id first matters: two members of one Provider may
/// name different models, and the id is the only unambiguous handle.
fn resolve_classifier_target(
    snapshot: &GatewaySnapshot,
    group: &GatewayRouteGroup,
    classifier: &str,
) -> Option<(ModelProviderProfileId, String)> {
    let classifier = classifier.trim();
    if let Some(member) = group.providers.iter().find(|member| {
        member
            .member_id
            .as_ref()
            .is_some_and(|id| id.as_str() == classifier)
    }) {
        return Some((member.profile.clone(), member.model.clone()));
    }
    let (prefix, model) = classifier.split_once('/')?;
    let model = model.trim();
    if model.is_empty() {
        return None;
    }
    let upstream = snapshot.upstreams.iter().find(|upstream| {
        upstream.profile_id.as_str() == prefix
            || crate::route::provider_alias(&upstream.display_name) == prefix
    })?;
    let model = upstream
        .metadata
        .model_aliases
        .iter()
        .find(|alias| alias.alias.as_str() == model)
        .map(|alias| alias.upstream.clone())
        .unwrap_or_else(|| model.to_string());
    if !upstream.models.is_empty() && !upstream.models.iter().any(|served| served == &model) {
        return None;
    }
    Some((upstream.profile_id.clone(), model.to_string()))
}

/// Reads the text out of a classifier reply in any of the four wire shapes.
fn classifier_reply_text(protocol: WireProtocol, bytes: &[u8]) -> Option<String> {
    let value: Value = serde_json::from_slice(bytes).ok()?;
    let texts = match protocol {
        WireProtocol::ChatCompletions => value
            .pointer("/choices/0/message/content")
            .map(content_text)
            .unwrap_or_default(),
        WireProtocol::Responses => value
            .pointer("/output/0/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .or_else(|| {
                value
                    .pointer("/output_text")
                    .and_then(Value::as_str)
                    .map(str::to_string)
            })
            .into_iter()
            .collect(),
        WireProtocol::AnthropicMessages => value
            .pointer("/content/0/text")
            .and_then(Value::as_str)
            .map(str::to_string)
            .into_iter()
            .collect(),
        WireProtocol::GeminiGenerateContent => value
            .pointer("/candidates/0/content/parts")
            .and_then(Value::as_array)
            .map(|parts| {
                parts
                    .iter()
                    .filter_map(|part| part.get("text").and_then(Value::as_str))
                    .collect::<Vec<_>>()
                    .join("")
            })
            .into_iter()
            .collect(),
    };
    let text = texts.join("").trim().to_string();
    (!text.is_empty()).then_some(text)
}

fn content_text(value: &Value) -> Vec<String> {
    match value {
        Value::String(text) => vec![text.clone()],
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .map(str::to_string)
            .collect(),
        _ => Vec::new(),
    }
}

/// Extracts `intent` and `effort` from the classifier's text.
///
/// The prompt asks for JSON, but a small model may wrap it in prose or fences;
/// both are trimmed before parsing. A plain single word is accepted as an
/// intent rather than turning a useful answer into an error.
fn classifier_decision(text: &str) -> ClassifierOutcome {
    let trimmed = text.trim();
    let json_slice = trimmed
        .find('{')
        .zip(trimmed.rfind('}'))
        .and_then(|(start, end)| (start <= end).then(|| &trimmed[start..=end]));
    if let Some(json) = json_slice
        && let Ok(value) = serde_json::from_str::<Value>(json)
    {
        let intent = value
            .get("intent")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|intent| !intent.is_empty() && intent.chars().count() <= 64)
            .map(str::to_string);
        let effort = value
            .get("effort")
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|effort| !effort.is_empty() && effort.chars().count() <= 32)
            .map(normalize_classifier_effort);
        if intent.is_some() || effort.is_some() {
            return ClassifierOutcome {
                intent,
                effort,
                error: None,
            };
        }
    }
    let word = trimmed.split_whitespace().next().unwrap_or_default();
    let intent = (!word.is_empty() && word.chars().count() <= 64).then(|| word.to_string());
    ClassifierOutcome {
        intent,
        effort: None,
        error: None,
    }
}

fn normalize_classifier_effort(effort: &str) -> String {
    match effort.to_ascii_lowercase().as_str() {
        "minimal" => "low".to_string(),
        "extra" | "xhigh" | "maximum" | "max" => "high".to_string(),
        other => other.to_string(),
    }
}

fn upstream_message(body: &str) -> String {
    let trimmed = body.trim();
    if let Ok(value) = serde_json::from_str::<Value>(trimmed) {
        for pointer in [
            "/error/message",
            "/error/msg",
            "/message",
            "/error",
            "/detail",
        ] {
            if let Some(message) = value.pointer(pointer).and_then(Value::as_str) {
                return truncate(message);
            }
        }
    }
    if trimmed.is_empty() {
        "the upstream refused the request without a message".to_string()
    } else {
        truncate(trimmed)
    }
}

fn truncate(value: &str) -> String {
    const MAX: usize = 512;
    if value.chars().count() <= MAX {
        return value.to_string();
    }
    let mut truncated = value.chars().take(MAX).collect::<String>();
    truncated.push('…');
    truncated
}

fn redact_transport(error: &reqwest::Error) -> String {
    // A transport error can carry the full URL, including a query string some
    // vendors put a Key into. Only the reason is reported.
    if error.is_timeout() {
        "the upstream did not answer in time".to_string()
    } else if error.is_connect() {
        "the upstream could not be reached".to_string()
    } else if error.is_redirect() {
        "the upstream redirected the request".to_string()
    } else {
        "the upstream connection failed".to_string()
    }
}

/// Streams one response to the client, recording what it carried.
///
/// `adopt_pin` stores the Key this request used so the next turn of the same
/// conversation stays on it.
pub(crate) async fn forward(
    service: Arc<ModelGatewayService>,
    prepared: PreparedCall,
    attempt: UpstreamAttempt,
    key: &KeyReference,
    sender: mpsc::Sender<Result<bytes::Bytes, GatewayError>>,
) {
    let tool_names = prepared.tool_names.clone();
    let UpstreamAttempt {
        body,
        prefetched,
        target,
        passthrough,
        profile_id,
        failed_attempts,
        skipped_members,
    } = attempt;
    // The pin follows the member that actually answered, not the first one the
    // plan preferred: a conversation that failed over must keep using the Key
    // that worked.
    if let (Some(conversation), Some(key)) = (prepared.conversation.as_deref(), Some(&key.id)) {
        service.pin_conversation(conversation, &profile_id, key);
    }
    let answered_model = prepared
        .member(&profile_id)
        .map(|member| member.model.clone())
        .unwrap_or_else(|| prepared.requested_model.clone());
    let started = std::time::Instant::now();
    let mut sniffer = UsageSniffer::new(target);
    let mut buffer = SseBuffer::default();
    let request_for_encoder = CanonicalRequest::for_model(answered_model.clone());
    // One decoder per upstream stream, not per chunk: its state is what tells
    // the client's protocol where the reply stands. A fresh decoder reads every
    // chunk as the first frame of a new message, so it re-announces the message
    // start mid-reply and re-opens the terminal event at the end. Anthropic
    // clients treat a second `message_start` as a new message — the Claude
    // adapter drops its record of what already streamed and prints the whole
    // answer again when the consolidated message arrives.
    let mut decoder: Option<Box<dyn StreamDecoder>> = (!passthrough).then(|| decoder(target));
    let mut encoder: Option<Box<dyn StreamEncoder>> =
        (!passthrough).then(|| encoder(prepared.inbound, &request_for_encoder));
    let mut accumulator = ReplyAccumulator::default();
    // The bytes read while proving the first token are replayed first, then the
    // rest of the upstream body follows.
    let mut stream = futures_util::stream::iter(
        prefetched
            .into_iter()
            .map(Ok::<bytes::Bytes, reqwest::Error>),
    )
    .chain(body);
    let mut failed = false;
    let mut last_output = std::time::Instant::now();
    // A vendor may accept the connection and then say nothing. No error arrives,
    // so the error-based failover never fires and the turn hangs. The first
    // token is the deadline this route set; afterwards a slow reply is the
    // model thinking, which is legitimate and must not be cut.
    //
    // The last member waits forever on purpose: there is no next source to
    // fail over to, and cutting it would turn a slow answer into a failure.
    let has_later_member = prepared
        .members
        .iter()
        .position(|member| member.profile_id == profile_id)
        .is_some_and(|index| index + 1 < prepared.members.len());
    let first_token_deadline = has_later_member
        .then(|| {
            prepared
                .member(&profile_id)
                .and_then(|member| member.first_token_timeout_ms)
        })
        .flatten()
        .map(|timeout| started + std::time::Duration::from_millis(timeout));
    let mut first_token_seen = false;

    loop {
        let next = match first_token_deadline.filter(|_| !first_token_seen) {
            Some(deadline) => {
                let remaining = deadline.saturating_duration_since(std::time::Instant::now());
                match tokio::time::timeout(remaining, stream.next()).await {
                    Ok(chunk) => chunk,
                    Err(_) => {
                        // Nothing streamed in time. The client has already
                        // received nothing, so this is an in-stream error
                        // rather than a silent stall.
                        failed = true;
                        break;
                    }
                }
            }
            None => stream.next().await,
        };
        let Some(chunk) = next else {
            break;
        };
        let Ok(chunk) = chunk else {
            failed = true;
            break;
        };
        if !first_token_seen {
            first_token_seen = true;
            // The deadline only guards the wait for the first token; the event
            // is what lets a client stop drawing the member as "starting".
            service.note_first_token(&prepared.request_id, vibex_core::unix_timestamp_ms());
        }
        if passthrough {
            sniffer.observe(&buffer.push(&chunk));
            if sender.send(Ok(chunk)).await.is_err() {
                return;
            }
            continue;
        }
        let frames = buffer.push(&chunk);
        let (Some(encoder), Some(decoded)) = (encoder.as_mut(), decoder.as_mut()) else {
            return;
        };
        let mut translated = String::new();
        let mut decode_error = None;
        for (_, data) in frames {
            if data.trim() == "[DONE]" {
                continue;
            }
            match decoded.decode(&data) {
                Ok(events) => {
                    for mut event in events {
                        // The upstream answers with the name it was given; the
                        // client only knows its own. Restoring it before the
                        // encoder runs means every protocol's reply names the
                        // tool the client can actually call.
                        tool_names.restore_event(&mut event);
                        accumulator.apply(&event);
                        match encoder.encode(&event) {
                            Ok(bytes) => translated.push_str(&bytes),
                            Err(error) => decode_error = Some(error),
                        }
                    }
                }
                Err(error) => decode_error = Some(error),
            }
        }
        if !translated.is_empty() {
            last_output = std::time::Instant::now();
            if sender
                .send(Ok(bytes::Bytes::from(translated)))
                .await
                .is_err()
            {
                return;
            }
        } else if last_output.elapsed() >= std::time::Duration::from_secs(10)
            && let Some(keepalive) = encoder.keepalive()
        {
            // A long tool call can stall the upstream for a while. Protocols
            // that tolerate a comment get one so the Agent's read timeout does
            // not fire mid-reply.
            last_output = std::time::Instant::now();
            if sender
                .send(Ok(bytes::Bytes::from_static(keepalive.as_bytes())))
                .await
                .is_err()
            {
                return;
            }
        }
        if let Some(error) = decode_error {
            let frame = encoder
                .encode(&CanonicalEvent::Error {
                    status: 502,
                    message: error.message(),
                })
                .unwrap_or_default();
            let _ = sender.send(Ok(bytes::Bytes::from(frame))).await;
            failed = true;
            break;
        }
    }

    if !passthrough && let (Some(encoder), Some(decoded)) = (encoder.as_mut(), decoder.as_mut()) {
        for (_, data) in buffer.finish() {
            if data.trim() == "[DONE]" {
                continue;
            }
            for mut event in decoded.decode(&data).unwrap_or_default() {
                tool_names.restore_event(&mut event);
                accumulator.apply(&event);
                if let Ok(bytes) = encoder.encode(&event) {
                    let _ = sender.send(Ok(bytes::Bytes::from(bytes))).await;
                }
            }
        }
        for mut event in decoded.finish().unwrap_or_default() {
            tool_names.restore_event(&mut event);
            accumulator.apply(&event);
            if let Ok(bytes) = encoder.encode(&event) {
                let _ = sender.send(Ok(bytes::Bytes::from(bytes))).await;
            }
        }
        if let Ok(tail) = encoder.finish()
            && !tail.is_empty()
        {
            let _ = sender.send(Ok(bytes::Bytes::from(tail))).await;
        }
    }

    let usage = if passthrough {
        // A passthrough stream that never named a terminal event was cut. The
        // client already has the prefix, so the only honest answer is an
        // in-stream error after it.
        if !sniffer.saw_terminal() && !failed {
            failed = true;
        }
        sniffer.usage()
    } else {
        accumulator.usage
    };

    if failed {
        let frame = sse_frame(
            None,
            &serde_json::json!({
                "error": {
                    "type": "upstream_error",
                    "message": "the upstream stream ended before the reply was complete",
                }
            }),
        );
        let _ = sender.send(Ok(bytes::Bytes::from(frame))).await;
    }

    service.record(
        &prepared,
        &RequestOutcome {
            // A request only counts as a failover when a Key actually failed
            // before this one answered. Counting the size of the candidate pool
            // instead would label a first-try success as a failover on every
            // Provider that happens to hold more than one Key.
            kind: if failed {
                GatewayRequestOutcome::Failed
            } else if failed_attempts.is_empty() {
                GatewayRequestOutcome::Ok
            } else {
                GatewayRequestOutcome::Failover
            },
            credential: Some(key.id.clone()),
            status: Some(if failed { 502 } else { 200 }),
            profile_id: Some(profile_id.clone()),
            target: Some(target),
            skipped_cooling: prepared
                .member(&profile_id)
                .map(|member| member.skipped_cooling)
                .unwrap_or(0),
            skipped_over_limit: prepared
                .member(&profile_id)
                .map(|member| member.skipped_over_limit)
                .unwrap_or(0),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: None,
            failed_attempts,
            skipped_members,
            error_message: None,
            error_code: failed.then_some("model_gateway_upstream_failed".to_string()),
        },
    );
}

/// Reads a whole upstream reply for a client that asked for one body.
pub(crate) async fn gather(
    service: &ModelGatewayService,
    prepared: &PreparedCall,
    attempt: UpstreamAttempt,
    key: &KeyReference,
) -> GatewayResult<Value> {
    let tool_names = prepared.tool_names.clone();
    let UpstreamAttempt {
        body,
        prefetched,
        target,
        passthrough,
        profile_id,
        failed_attempts,
        skipped_members,
    } = attempt;
    let started = std::time::Instant::now();
    // A non-streaming client gets one body, so the whole reply is its "first
    // token": the deadline has already done its job by the time the body
    // arrives, and the graph still needs to stop drawing this request as
    // starting.
    service.note_first_token(&prepared.request_id, vibex_core::unix_timestamp_ms());
    let mut collected = Vec::new();
    let mut chunks = futures_util::stream::iter(
        prefetched
            .into_iter()
            .map(Ok::<bytes::Bytes, reqwest::Error>),
    )
    .chain(body);
    let mut body_failed = false;
    while let Some(chunk) = chunks.next().await {
        match chunk {
            Ok(chunk) => collected.extend_from_slice(&chunk),
            Err(_) => {
                body_failed = true;
                break;
            }
        }
    }
    let bytes = if body_failed {
        // The reply body never arrived. The ledger keeps the fact, so a
        // request that produced neither an answer nor a token count is
        // still visible as a failure rather than as a gap.
        service.record(
            prepared,
            &RequestOutcome {
                kind: GatewayRequestOutcome::Failed,
                profile_id: Some(profile_id.clone()),
                target: Some(target),
                skipped_cooling: 0,
                skipped_over_limit: 0,
                credential: Some(key.id.clone()),
                status: Some(502),
                usage: CanonicalUsage::default(),
                latency_ms: Some(started.elapsed().as_millis() as i64),
                served: None,
                failed_attempts,
                skipped_members,
                error_code: Some("model_gateway_upstream_failed".to_string()),
                error_message: None,
            },
        );
        return Err(GatewayError::Upstream {
            status: 502,
            message: "the upstream reply ended before it was complete".to_string(),
        });
    } else {
        bytes::Bytes::from(collected)
    };
    let (mut response, usage) = if passthrough {
        let parsed = parse_reply(target, &bytes).unwrap_or_default();
        let usage = parsed.usage.clone();
        (parsed, usage)
    } else {
        // A translated request asks the upstream for a stream, but an upstream
        // that only speaks whole bodies may ignore the flag. Accept both
        // shapes rather than turning a valid answer into an empty one.
        if bytes
            .iter()
            .copied()
            .find(|byte| !byte.is_ascii_whitespace())
            == Some(b'{')
        {
            let parsed = parse_reply(target, &bytes).unwrap_or_default();
            let usage = parsed.usage.clone();
            (parsed, usage)
        } else {
            let mut sniffer = UsageSniffer::new(target);
            let mut buffer = SseBuffer::default();
            let mut accumulator = ReplyAccumulator::default();
            let mut decoded = decoder(target);
            for (_, data) in buffer.push(&bytes) {
                if data.trim() == "[DONE]" {
                    continue;
                }
                for event in decoded.decode(&data).unwrap_or_default() {
                    accumulator.apply(&event);
                }
            }
            sniffer.observe(&buffer.finish());
            if sniffer.usage().total() > 0 {
                accumulator.apply(&CanonicalEvent::Usage(sniffer.usage()));
            }
            let parsed = accumulator.finish();
            let usage = parsed.usage.clone();
            (parsed, usage)
        }
    };
    // The reply names the tool the upstream was asked for; the client only
    // knows the name it declared, so the rewritten form is translated back
    // before the reply is rendered in the client's protocol.
    tool_names.restore_response(&mut response);
    let rendered = render_response(prepared.inbound, &response)?;
    if let Some(conversation) = prepared.conversation.as_deref() {
        service.pin_conversation(conversation, &profile_id, &key.id);
    }
    service.record(
        prepared,
        &RequestOutcome {
            kind: if failed_attempts.is_empty() {
                GatewayRequestOutcome::Ok
            } else {
                GatewayRequestOutcome::Failover
            },
            credential: Some(key.id.clone()),
            status: Some(200),
            profile_id: Some(profile_id.clone()),
            target: Some(target),
            skipped_cooling: prepared
                .member(&profile_id)
                .map(|member| member.skipped_cooling)
                .unwrap_or(0),
            skipped_over_limit: prepared
                .member(&profile_id)
                .map(|member| member.skipped_over_limit)
                .unwrap_or(0),
            usage,
            latency_ms: Some(started.elapsed().as_millis() as i64),
            served: (!response.model.is_empty()).then(|| response.model.clone()),
            failed_attempts,
            skipped_members,
            error_message: None,
            error_code: None,
        },
    );
    Ok(rendered)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn upstream(protocols: &[WireProtocol]) -> GatewayUpstream {
        GatewayUpstream {
            profile_id: ModelProviderProfileId::new(),
            display_name: "Upstream".to_string(),
            endpoints: protocols
                .iter()
                .map(|protocol| ModelProviderEndpoint {
                    id: format!("ep-{}", protocol.short_name()),
                    kind: ModelProviderEndpointKind::Api,
                    url: "https://example.invalid".to_string(),
                    wire_protocol_id: Some(protocol.wire_protocol_id().to_string()),
                })
                .collect(),
            models: vec!["m".to_string()],
            metadata: vibex_core::ModelProviderMetadata::default(),
            credentials: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// An upstream that publishes only an untyped address.
    fn untyped_upstream() -> GatewayUpstream {
        GatewayUpstream {
            profile_id: ModelProviderProfileId::new(),
            display_name: "Untyped".to_string(),
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://example.invalid".to_string(),
                wire_protocol_id: None,
            }],
            models: vec!["m".to_string()],
            metadata: vibex_core::ModelProviderMetadata::default(),
            credentials: Vec::new(),
            keys: Vec::new(),
        }
    }

    /// Three failures degrade one group, one success recovers it.
    ///
    /// The transition is the whole contract: a degraded group must come back
    /// without a restart, and a single good answer is the signal.
    #[test]
    fn one_group_degrades_after_three_failures_and_recovers_on_success() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-test.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-test-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 2));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure("group-a");
        }
        assert!(service.is_degraded());
        assert!(service.group_is_degraded("group-a"));
        assert_eq!(service.runtime_state().degraded_routes, 1);
        assert!(
            service
                .runtime_state()
                .notice
                .as_deref()
                .is_some_and(|notice| notice.starts_with("1 of 2 route groups")),
            "the notice must name the scope, got {:?}",
            service.runtime_state().notice
        );
        // The listener is still bound, so the UI can distinguish "stepped
        // aside" from "not running", and the other group keeps working.
        assert!(service.runtime_state().status.is_listening());
        assert!(service.runtime_state().base_url().is_some());
        assert!(!service.group_is_degraded("group-b"));
        assert!(!service.begin_attempt("group-b").unwrap());

        service.note_success("group-a");
        assert!(!service.is_degraded());
        assert_eq!(service.runtime_state().status, ModelGatewayStatus::Running);
        assert!(service.runtime_state().notice.is_none());
        assert_eq!(service.runtime_state().degraded_routes, 0);
    }

    /// A degraded group admits exactly one request at a time.
    ///
    /// This is what makes recovery possible at all: the single probe is the
    /// only request that can reach an upstream and produce the success
    /// `note_success` needs. A second caller is refused rather than queued, so
    /// it stays on its direct Provider instead of waiting behind a broken one.
    #[test]
    fn a_degraded_group_admits_one_probe_at_a_time() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-probe.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-probe-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure("group-a");
        }
        assert!(service.is_degraded());

        // While running, every request goes through and claims nothing.
        let running = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-running.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-running-db.db")),
            GatewayLimits::default(),
        );
        running.publish(ModelGatewayRuntimeState::running(3425, 1));
        assert!(!running.begin_attempt("group-a").unwrap());

        // The first caller through a degraded group holds the slot.
        assert!(service.begin_attempt("group-a").unwrap());
        // A second one is refused with the message the server must not count.
        let refused = service.begin_attempt("group-a").unwrap_err();
        assert!(
            matches!(&refused, GatewayError::NotConfigured(message)
                if message == ModelGatewayService::STEPPED_ASIDE),
            "the refusal must be recognisable as the gateway's own, got {refused:?}"
        );

        // Releasing lets the next request probe.
        service.finish_attempt("group-a", true);
        assert!(service.begin_attempt("group-a").unwrap());
        service.finish_attempt("group-a", true);
    }

    /// A probe that reaches an upstream and answers clears the state.
    #[test]
    fn a_successful_probe_leaves_the_degraded_state() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-heal.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-heal-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        for _ in 0..ModelGatewayService::DEGRADE_AFTER_FAILURES {
            service.note_failure("group-a");
        }
        assert!(service.is_degraded());

        assert!(service.begin_attempt("group-a").unwrap());
        service.note_success("group-a");

        assert!(!service.is_degraded());
        assert_eq!(service.runtime_state().status, ModelGatewayStatus::Running);
        // The next request is ordinary again, not a probe.
        assert!(!service.begin_attempt("group-a").unwrap());
    }

    /// A failure from an unrelated outage starts the count over.
    #[test]
    fn an_old_failure_does_not_accumulate() {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join("vibex-breaker-old.db"),
            ProviderConfigService::new(std::env::temp_dir().join("vibex-breaker-old-db.db")),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        service.note_failure("group-a");
        service.note_failure("group-a");
        if let Ok(mut breakers) = service.breakers.lock()
            && let Some(breaker) = breakers.get_mut("group-a")
        {
            breaker.last_failure_at_ms =
                vibex_core::unix_timestamp_ms() - ModelGatewayService::FAILURE_MEMORY_MS - 1;
        }
        service.note_failure("group-a");
        assert!(
            !service.is_degraded(),
            "a failure from an unrelated outage must not complete the count"
        );
    }

    #[test]
    fn endpoint_join_does_not_double_the_version_prefix() {
        assert_eq!(
            endpoint_url(
                "https://api.deepseek.com/v1",
                WireProtocol::ChatCompletions,
                "deepseek-chat",
                false
            )
            .unwrap(),
            "https://api.deepseek.com/v1/chat/completions"
        );
        assert_eq!(
            endpoint_url(
                "https://api.deepseek.com",
                WireProtocol::AnthropicMessages,
                "deepseek-chat",
                false
            )
            .unwrap(),
            "https://api.deepseek.com/v1/messages"
        );
        assert_eq!(
            endpoint_url(
                "https://generativelanguage.googleapis.com",
                WireProtocol::GeminiGenerateContent,
                "gemini-2.5-pro",
                false
            )
            .unwrap(),
            "https://generativelanguage.googleapis.com/v1beta/models/gemini-2.5-pro:generateContent"
        );
    }

    /// A declared protocol wins over passthrough: a client speaking another
    /// protocol is translated onto the one the endpoint states.
    #[test]
    fn passthrough_wins_over_translation() {
        let upstream = upstream(&[
            WireProtocol::ChatCompletions,
            WireProtocol::AnthropicMessages,
        ]);
        let policy = GatewayPolicy::default();
        assert_eq!(
            choose_target(WireProtocol::ChatCompletions, &upstream, None, &policy).unwrap(),
            TargetPlan {
                target: WireProtocol::ChatCompletions,
                passthrough: true,
                alternates: Vec::new(),
            }
        );
        // A protocol the upstream does not speak is translated.
        assert_eq!(
            choose_target(WireProtocol::Responses, &upstream, None, &policy).unwrap(),
            TargetPlan {
                target: WireProtocol::AnthropicMessages,
                passthrough: false,
                alternates: Vec::new(),
            }
        );
    }

    /// An endpoint that declares nothing is guessed once, with a fallback, and
    /// never claimed to support anything.
    ///
    /// This is the fix for the outage: the untyped address used to answer
    /// `endpoint_for` for every protocol, so a Responses body went to a Chat
    /// Completions endpoint unmodified and the vendor's 422 cooled the Key.
    #[test]
    fn an_untyped_endpoint_is_guessed_once_with_a_fallback() {
        let upstream = untyped_upstream();
        assert!(!upstream.supports(WireProtocol::Responses));
        assert!(upstream.endpoint_for("openai_responses").is_none());
        assert!(upstream.untyped_endpoint().is_some());

        let policy = GatewayPolicy::default();
        let plan = choose_target(WireProtocol::Responses, &upstream, None, &policy).unwrap();
        assert_eq!(plan.target, WireProtocol::Responses);
        assert!(plan.passthrough, "the first try is the cheap one");
        assert_eq!(
            plan.alternates,
            vec![
                (WireProtocol::AnthropicMessages, false),
                (WireProtocol::ChatCompletions, false),
                (WireProtocol::GeminiGenerateContent, false),
            ],
            "a refusal has to have somewhere to go, and every allowed protocol \
             is a candidate because the endpoint declared nothing"
        );
    }

    /// A remembered verdict replaces the guess: the second request does not pay
    /// for the same correction again.
    #[test]
    fn a_remembered_protocol_replaces_the_guess() {
        let upstream = untyped_upstream();
        let policy = GatewayPolicy::default();
        let plan = choose_target(
            WireProtocol::Responses,
            &upstream,
            Some(WireProtocol::ChatCompletions),
            &policy,
        )
        .unwrap();
        assert_eq!(
            plan,
            TargetPlan {
                target: WireProtocol::ChatCompletions,
                passthrough: false,
                alternates: Vec::new(),
            }
        );
        // The same verdict on an inbound that matches it is a passthrough.
        assert!(
            choose_target(
                WireProtocol::ChatCompletions,
                &upstream,
                Some(WireProtocol::ChatCompletions),
                &policy,
            )
            .unwrap()
            .passthrough
        );
    }

    #[test]
    fn a_disabled_translation_is_refused_instead_of_silently_used() {
        let upstream = upstream(&[WireProtocol::ChatCompletions]);
        let mut policy = GatewayPolicy::default();
        policy.translate.openai_chat_completions = false;
        let error = choose_target(WireProtocol::Responses, &upstream, None, &policy).unwrap_err();
        assert_eq!(error.code(), "model_gateway_translation_disabled");
    }

    /// A vendor answering "wrong shape" is not a Key failure.
    ///
    /// The old path classified it as `Other`, cooled the Key for ten seconds,
    /// and counted the request against the gateway — three guesses were enough
    /// to close the whole service.
    #[test]
    fn a_protocol_mismatch_blames_no_key() {
        let error = GatewayError::ProtocolMismatch {
            wire_protocol_id: "openai_responses".to_string(),
            status: 422,
            message: "unknown variant `function_call_output`".to_string(),
        };
        assert!(!error.blames_upstream());
        assert_eq!(error.code(), "model_gateway_protocol_mismatch");
        assert_eq!(member_failure_kind(&error), GatewayFailureKind::Other);
    }

    #[test]
    fn upstream_messages_are_extracted_and_truncated() {
        assert_eq!(
            upstream_message(r#"{"error":{"message":"bad key"}}"#),
            "bad key"
        );
        assert_eq!(upstream_message("plain failure"), "plain failure");
        let long = "x".repeat(600);
        let truncated = upstream_message(&long);
        assert!(truncated.ends_with('…'));
        assert_eq!(truncated.chars().count(), 513);
    }

    #[test]
    fn translation_policy_gates_only_the_protocols_it_names() {
        let mut policy = vibex_core::GatewayTranslatePolicy::default();
        assert!(policy.allows(WireProtocol::ChatCompletions.wire_protocol_id()));
        policy.openai_chat_completions = false;
        assert!(!policy.allows(WireProtocol::ChatCompletions.wire_protocol_id()));
        // A protocol the gateway cannot translate is not gated by this switch.
        assert!(policy.allows(vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE));
    }

    /// Every published model id names a group, and nothing else resolves.
    ///
    /// The gateway has no per-upstream catalogue any more, so a bare
    /// `alias/model` id is not a model it serves.
    #[test]
    fn model_catalog_lists_group_models_only() {
        let snapshot = GatewaySnapshot {
            settings: ModelGatewaySettings::default(),
            upstreams: Vec::new(),
            gateway_used_tokens: 0,
            groups: BTreeMap::from([(
                "g1".to_string(),
                GatewayRouteGroup {
                    group_id: "g1".to_string(),
                    display_name: "Team".to_string(),
                    strategy: vibex_core::RouteStrategy::Ordered,
                    key_strategy: GatewayRoutingStrategy::Ordered,
                    first_token_timeout_ms: None,
                    pin: GatewayPinScope::Off,
                    failover: vibex_core::GatewayFailover::Auto,
                    token_limit: None,
                    providers: Vec::new(),
                    models: vec![GatewayGroupModel {
                        model: "deepseek-chat".to_string(),
                        providers: Vec::new(),
                    }],
                    agent_model_id: Some("deepseek-chat".to_string()),
                    rules: Vec::new(),
                    classifier: None,
                    effort: None,
                    patience_ms: None,
                    sink: false,
                    unlists: false,
                    levels: Vec::new(),
                    pick_member_id: None,
                },
            )]),
        };
        let catalog = snapshot.model_catalog();
        assert_eq!(catalog.len(), 1);
        assert_eq!(catalog[0].id, "group/g1/deepseek-chat");
        assert!(snapshot.group_for_model(&catalog[0].id).is_some());
        assert!(
            snapshot.group_for_model("deepseek/deepseek-chat").is_none(),
            "a Provider-qualified id is not a gateway model"
        );
    }
}

#[cfg(test)]
mod inflight_tests {
    use super::*;
    use std::sync::Mutex as StdMutex;

    fn service() -> Arc<ModelGatewayService> {
        let service = ModelGatewayService::new(
            std::env::temp_dir().join(format!("vibex-inflight-{}.db", RequestId::new().as_str())),
            ProviderConfigService::new(std::env::temp_dir().join(format!(
                "vibex-inflight-cfg-{}.db",
                RequestId::new().as_str()
            ))),
            GatewayLimits::default(),
        );
        service.publish(ModelGatewayRuntimeState::running(3425, 1));
        service
    }

    fn inflight(request_id: &RequestId, attempt: u32) -> vibex_core::GatewayInflight {
        vibex_core::GatewayInflight {
            request_id: request_id.clone(),
            agent_id: Some("codex".to_string()),
            model_route_id: Some("model_route_test".to_string()),
            requested_model: "route/model_route_test".to_string(),
            model_provider_profile_id: ModelProviderProfileId::new(),
            attempt,
            credential_id: Some(RequestId::new()),
            wire_protocol_id: WireProtocol::ChatCompletions.wire_protocol_id().to_string(),
            started_at_ms: vibex_core::unix_timestamp_ms(),
            first_token_at_ms: None,
        }
    }

    /// A request is visible while it runs, not only once it has finished.
    #[test]
    fn an_inflight_request_is_listed_until_it_ends() {
        let service = service();
        let request_id = RequestId::new();
        assert!(service.inflight_requests().is_empty());

        service.note_inflight(inflight(&request_id, 1));
        let listed = service.inflight_requests();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].request_id, request_id);
        assert!(!listed[0].has_first_token());

        service.note_first_token(&request_id, vibex_core::unix_timestamp_ms());
        assert!(
            service.inflight_requests()[0].has_first_token(),
            "the first token ends the deadline and the starting state"
        );

        service.note_finished(&request_id, vibex_core::GatewayRequestOutcome::Ok);
        assert!(
            service.inflight_requests().is_empty(),
            "a finished request must not stay drawn"
        );
    }

    /// Moving to another member replaces the entry rather than adding one.
    #[test]
    fn a_member_retry_updates_the_same_request() {
        let service = service();
        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_inflight(inflight(&request_id, 2));

        let listed = service.inflight_requests();
        assert_eq!(listed.len(), 1, "one request, not one per attempt");
        assert_eq!(listed[0].attempt, 2, "the attempt number advances");
    }

    /// The table is bounded, so a long-running gateway cannot grow it forever.
    #[test]
    fn the_inflight_table_keeps_only_the_newest_requests() {
        let service = service();
        for _ in 0..ModelGatewayService::MAX_INFLIGHT + 3 {
            service.note_inflight(inflight(&RequestId::new(), 1));
        }
        assert_eq!(
            service.inflight_requests().len(),
            ModelGatewayService::MAX_INFLIGHT
        );
    }

    /// Every transition reaches the sink, in order.
    #[test]
    fn each_transition_is_published_to_the_sink() {
        let service = service();
        let seen: Arc<StdMutex<Vec<vibex_core::GatewayEvent>>> =
            Arc::new(StdMutex::new(Vec::new()));
        let sink = Arc::clone(&seen);
        service.set_event_sink(Arc::new(move |event| {
            sink.lock().unwrap().push(event);
        }));

        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_first_token(&request_id, 42);
        service.note_finished(&request_id, vibex_core::GatewayRequestOutcome::Failover);

        let events = seen.lock().unwrap();
        assert_eq!(events.len(), 3);
        assert!(matches!(events[0], vibex_core::GatewayEvent::Began(_)));
        assert!(matches!(
            events[1],
            vibex_core::GatewayEvent::FirstToken { at_ms: 42, .. }
        ));
        assert!(matches!(
            events[2],
            vibex_core::GatewayEvent::Ended {
                outcome: vibex_core::GatewayRequestOutcome::Failover,
                ..
            }
        ));
        // Every event names the same request, which is what lets a client
        // follow one request rather than guess.
        assert!(events.iter().all(|event| event.request_id() == &request_id));
    }

    #[test]
    fn a_prompt_names_the_model_without_its_routing_prefix() {
        // A gateway id is an address, so the prompt and the notice have to show
        // the model as the user knows it.
        assert_eq!(
            bare_model_name("group/route-1/deepseek-chat"),
            "deepseek-chat"
        );
        // Anything that is not a group id is already a name.
        assert_eq!(bare_model_name("deepseek-chat"), "deepseek-chat");
        assert_eq!(bare_model_name("group/only-a-route"), "group/only-a-route");
    }

    #[test]
    fn a_pinned_member_is_tried_before_the_strategy_order() {
        let a = ModelProviderProfileId::new();
        let b = ModelProviderProfileId::new();
        let c = ModelProviderProfileId::new();
        let mut plans = vec![a.clone(), b.clone(), c.clone()];
        // The strategy put `c` last; pinning it must still make it first, and
        // leave the rest in the order the strategy chose.
        hoist_pinned(&mut plans, Some(&c), |plan| plan);
        assert_eq!(plans, vec![c.clone(), a.clone(), b.clone()]);
        // Already first: nothing moves, so a second turn in the same
        // conversation does not reshuffle the fallbacks.
        hoist_pinned(&mut plans, Some(&c), |plan| plan);
        assert_eq!(plans, vec![c.clone(), a.clone(), b.clone()]);
        // A pin for a member that has since left the group is ignored rather
        // than dropping the request.
        let gone = ModelProviderProfileId::new();
        hoist_pinned(&mut plans, Some(&gone), |plan| plan);
        assert_eq!(plans, vec![c.clone(), a.clone(), b.clone()]);
        // No pin at all leaves the strategy's order untouched.
        hoist_pinned(&mut plans, None, |plan| plan);
        assert_eq!(plans, vec![c, a, b]);
    }

    #[test]
    fn an_answer_reaches_the_request_that_is_waiting() {
        let service = service();
        service.set_event_sink(Arc::new(|_| {}));
        let prompt = vibex_core::GatewayFailoverPrompt {
            id: RequestId::new(),
            model_route_id: "model_route_test".to_string(),
            route_name: "Team".to_string(),
            model: "route/model_route_test".to_string(),
            from_profile_id: ModelProviderProfileId::new(),
            from_name: "alpha".to_string(),
            to_profile_id: ModelProviderProfileId::new(),
            to_name: "beta".to_string(),
            expires_at_ms: vibex_core::unix_timestamp_ms() + 5_000,
            asked_at_ms: vibex_core::unix_timestamp_ms(),
        };
        let prompt_id = prompt.id.clone();
        let waiting = Arc::clone(&service);
        let task = std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .unwrap();
            runtime.block_on(waiting.ask_failover(prompt))
        });
        // The sender is registered before the event is published, so a client
        // answering the instant it draws the prompt cannot miss it.
        let mut answered = false;
        for _ in 0..200 {
            if service.answer_failover(&prompt_id, vibex_core::GatewayFailoverAnswer::Stay) {
                answered = true;
                break;
            }
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(answered, "the prompt never became answerable");
        assert_eq!(
            task.join().unwrap(),
            Some(vibex_core::GatewayFailoverAnswer::Stay)
        );
        // The entry is gone once settled: a second, stale click must not be
        // able to redirect a later request.
        assert!(!service.answer_failover(&prompt_id, vibex_core::GatewayFailoverAnswer::Stay));
    }

    /// A first token is only announced once.
    #[test]
    fn the_first_token_is_announced_once() {
        let service = service();
        let seen: Arc<StdMutex<usize>> = Arc::new(StdMutex::new(0));
        let sink = Arc::clone(&seen);
        service.set_event_sink(Arc::new(move |event| {
            if matches!(event, vibex_core::GatewayEvent::FirstToken { .. }) {
                *sink.lock().unwrap() += 1;
            }
        }));
        let request_id = RequestId::new();
        service.note_inflight(inflight(&request_id, 1));
        service.note_first_token(&request_id, 1);
        service.note_first_token(&request_id, 2);
        assert_eq!(*seen.lock().unwrap(), 1);
    }
}

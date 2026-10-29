//! Local model gateway contracts.
//!
//! The gateway is one loopback HTTP endpoint that Agents can be pointed at
//! instead of a vendor address. It owns request-level routing — which upstream
//! answers, which of its Keys is used, what happens when that Key is cooling
//! down — while the Provider Profile and Agent binding contracts keep owning
//! the durable configuration.
//!
//! Two rules shape everything in this module:
//!
//! - Routing state is request state. Nothing here may enter an Agent process
//!   fingerprint, so switching a Key never restarts an Agent process.
//! - Usage is counted in tokens only. Vendor billing cannot be read through an
//!   API key, so the contract deliberately carries no price, cost, or currency
//!   field. A local token ceiling is a user-set ceiling on observed tokens,
//!   not a claim about a vendor's quota.

use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use crate::ids::{ModelProviderProfileId, RequestId};
use crate::provider_projection::ModelProviderProfile;
use crate::{VibexError, VibexResult, unix_timestamp_ms};

/// How the gateway picks one of a Provider's Keys for the next request.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRoutingStrategy {
    /// Use the first ready Key until it cannot answer, then the next one.
    #[default]
    Ordered,
    /// Advance one Key per request so usage is spread across the pool.
    RoundRobin,
    /// Prefer the Key that has carried the fewest tokens in this window.
    LeastUsed,
}

impl GatewayRoutingStrategy {
    pub const ALL: [Self; 3] = [Self::Ordered, Self::RoundRobin, Self::LeastUsed];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::RoundRobin => "round_robin",
            Self::LeastUsed => "least_used",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ordered => "In order",
            Self::RoundRobin => "Rotate",
            Self::LeastUsed => "Least used",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|strategy| strategy.id() == value.trim())
    }
}

/// The gateway asking the user whether a conversation may leave its pinned
/// member.
///
/// The request that triggered this is already in flight, so the question cannot
/// wait forever: `expires_at_ms` is when an unanswered prompt accepts the switch
/// on its own. That is why the UI shows a countdown rather than a blocking
/// dialog — doing nothing has to be a valid answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayFailoverPrompt {
    pub id: RequestId,
    /// The route whose pinned member failed.
    pub model_route_id: String,
    /// The route's display name, so the prompt does not have to be joined to a
    /// route the reader may not have loaded.
    pub route_name: String,
    /// The model the conversation asked for, as the Agent names it.
    pub model: String,
    /// The member that failed, and the one that would take over.
    pub from_profile_id: ModelProviderProfileId,
    pub from_name: String,
    pub to_profile_id: ModelProviderProfileId,
    pub to_name: String,
    /// When an unanswered prompt switches on its own.
    pub expires_at_ms: i64,
    /// When the prompt appeared, so a client can compute the remaining time
    /// from its own clock instead of trusting a count it never saw tick.
    pub asked_at_ms: i64,
}

impl GatewayFailoverPrompt {
    /// How long the user has to answer before the switch is accepted.
    ///
    /// Five seconds is long enough to read a one-line prompt and short enough
    /// that an unattended session is not held up: the request is blocked while
    /// this runs, so the wait is a real cost paid by a turn the user may not
    /// even be watching.
    pub const ANSWER_WINDOW_MS: i64 = 5_000;

    /// What the user chose for a prompt.
    pub fn remaining_ms(&self, now_ms: i64) -> i64 {
        (self.expires_at_ms - now_ms).max(0)
    }
}

/// How the user answered a failover prompt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayFailoverAnswer {
    /// Switch this one time.
    Switch,
    /// Keep the conversation where it is; the request fails.
    Stay,
    /// Switch, and stop asking for this route from now on.
    AlwaysSwitch,
}

impl GatewayFailoverAnswer {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Switch => "switch",
            Self::Stay => "stay",
            Self::AlwaysSwitch => "always_switch",
        }
    }
}

/// How long a conversation stays on the Key it started on.
///
/// Vendors cache a prompt per account, so re-picking a Key on every turn
/// throws that cache away. Pinning trades a little load balance for a
/// materially cheaper turn.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayPinScope {
    /// Re-pick per request.
    Off,
    /// Keep a conversation on one Key until it is invalidated.
    #[default]
    Conversation,
}

/// What the gateway does when the member a conversation is pinned to fails.
///
/// Pinning a conversation to one Provider is what preserves a vendor's prompt
/// cache, but it must not turn a Provider outage into a stuck session. The
/// route decides how the escape hatch behaves, because the right answer differs
/// by what the group is for: a group of interchangeable clones should just move
/// on, while a group that deliberately mixes vendors should let the user choose.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayFailover {
    /// Move to the next member and say so afterwards.
    ///
    /// The notice is informational: it reports which member took over, and
    /// nothing waits on the user.
    #[default]
    Auto,
    /// Offer the switch and count down.
    ///
    /// The user can take the switch, refuse it, or enable it permanently for
    /// the group. Doing nothing accepts it, which is why the countdown exists:
    /// a request is already in flight and a prompt that waits forever would
    /// hang the turn.
    Ask,
}

impl GatewayFailover {
    pub const ALL: [Self; 2] = [Self::Auto, Self::Ask];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Auto => "auto",
            Self::Ask => "ask",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|failover| failover.id() == value.trim())
    }
}

impl GatewayPinScope {
    pub const ALL: [Self; 2] = [Self::Off, Self::Conversation];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Off => "off",
            Self::Conversation => "conversation",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|scope| scope.id() == value.trim())
    }
}

/// The window a local token ceiling is measured over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayTokenWindow {
    Day,
    Week,
    Month,
}

impl GatewayTokenWindow {
    pub const ALL: [Self; 3] = [Self::Day, Self::Week, Self::Month];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Week => "week",
            Self::Month => "month",
        }
    }

    /// The length of one window in milliseconds.
    pub const fn duration_ms(self) -> i64 {
        const DAY_MS: i64 = 24 * 60 * 60 * 1000;
        match self {
            Self::Day => DAY_MS,
            Self::Week => 7 * DAY_MS,
            Self::Month => 30 * DAY_MS,
        }
    }
}

/// A local ceiling on tokens the gateway has observed for one Provider.
///
/// This is a soft limit by construction: the gateway only sees requests it
/// served itself, and reaches the ceiling after a request has already been
/// spent. What it guarantees is that once the ceiling is crossed the Provider
/// stops being selected until the window rolls over.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTokenLimit {
    pub window: GatewayTokenWindow,
    pub tokens: u64,
}

impl GatewayTokenLimit {
    pub fn validate(&self) -> VibexResult<()> {
        if self.tokens == 0 {
            return Err(VibexError::validation(
                "model_gateway_token_limit_invalid",
                "a token limit must be greater than zero",
            ));
        }
        Ok(())
    }
}

/// Which protocol translations the gateway may perform.
///
/// A translation the user disables is refused with a 400 instead of being
/// performed silently: a request that cannot be expressed in the target
/// protocol should fail loudly rather than arrive mangled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTranslatePolicy {
    pub openai_chat_completions: bool,
    pub openai_responses: bool,
    pub anthropic_messages: bool,
    pub google_generative_ai: bool,
}

impl Default for GatewayTranslatePolicy {
    fn default() -> Self {
        Self {
            openai_chat_completions: true,
            openai_responses: true,
            anthropic_messages: true,
            google_generative_ai: true,
        }
    }
}

impl GatewayTranslatePolicy {
    pub fn allows(&self, wire_protocol_id: &str) -> bool {
        match wire_protocol_id {
            crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => self.openai_chat_completions,
            crate::WIRE_PROTOCOL_OPENAI_RESPONSES => self.openai_responses,
            crate::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => self.anthropic_messages,
            crate::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => self.google_generative_ai,
            // Protocols the gateway cannot translate are never gated here;
            // passthrough is the only route for them.
            _ => true,
        }
    }
}

/// Concurrency and rate limits for one upstream.
///
/// Vendors count requests per account, so these are per Key rather than global.
/// A Key that is already busy is not a failure: the gateway queues for it, and
/// only answers `429` when the queue itself is full or its wait ran out.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountLimits {
    /// Requests allowed in flight at once, across every model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_concurrency: Option<u32>,
    /// Requests allowed to start in any one minute.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_rpm: Option<u32>,
    /// How many requests may wait for a slot. `None` means unbounded.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_limit: Option<u32>,
    /// How long a request may wait for a slot before it is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_wait_ms: Option<u64>,
}

impl GatewayAccountLimits {
    /// The longest a request may wait before the gateway answers `429`.
    pub const DEFAULT_QUEUE_WAIT_MS: u64 = 30_000;

    pub fn is_configured(&self) -> bool {
        self.max_concurrency.is_some() || self.max_rpm.is_some()
    }

    pub fn validate(&self) -> VibexResult<()> {
        if self.max_concurrency == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_concurrency_invalid",
                "a concurrency limit must be at least one",
            ));
        }
        if self.max_rpm == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_rpm_invalid",
                "an RPM limit must be at least one",
            ));
        }
        if self.queue_limit == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_queue_limit_invalid",
                "a queue limit must be at least one",
            ));
        }
        if self.queue_wait_ms == Some(0) {
            return Err(VibexError::validation(
                "model_gateway_queue_wait_invalid",
                "a queue wait must be at least one millisecond",
            ));
        }
        Ok(())
    }

    pub fn queue_wait(&self) -> std::time::Duration {
        std::time::Duration::from_millis(self.queue_wait_ms.unwrap_or(Self::DEFAULT_QUEUE_WAIT_MS))
    }
}

/// Durable routing policy for the local gateway.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayPolicy {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<GatewayTokenLimit>,
    #[serde(default)]
    pub translate: GatewayTranslatePolicy,
    /// Concurrency and rate limits, keyed by credential id.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub account_limits: BTreeMap<RequestId, GatewayAccountLimits>,
    /// The Key the router tries first for each Provider, when the user asked
    /// for one. A Provider without an entry keeps its configured Key order.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub preferred_keys: BTreeMap<ModelProviderProfileId, RequestId>,
    /// Warn when the observed error rate exceeds this percentage.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate_warn_percent: Option<u16>,
    /// Warn when observed request latency exceeds this many milliseconds.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_warn_ms: Option<u64>,
    /// Default connection timeout for a gateway attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_connect_timeout_ms: Option<u64>,
    /// Default whole-request timeout for a gateway attempt.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_request_timeout_ms: Option<u64>,
    /// Default silence timeout before a stream is considered stalled.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_stream_idle_timeout_ms: Option<u64>,
    /// Per-failure-kind cooldowns, in milliseconds.
    ///
    /// The key is a [`GatewayFailureKind::id`]. Only the kinds that can cool a
    /// Key are accepted; auth is absent because a rejected credential does not
    /// heal with time. An empty map keeps the built-in durations.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cooldown_overrides_ms: BTreeMap<String, u64>,
}

impl Default for GatewayPolicy {
    fn default() -> Self {
        Self {
            enabled: true,
            token_limit: None,
            translate: GatewayTranslatePolicy::default(),
            account_limits: BTreeMap::new(),
            preferred_keys: BTreeMap::new(),
            error_rate_warn_percent: None,
            latency_warn_ms: None,
            default_connect_timeout_ms: None,
            default_request_timeout_ms: None,
            default_stream_idle_timeout_ms: None,
            cooldown_overrides_ms: BTreeMap::new(),
        }
    }
}

impl GatewayPolicy {
    pub fn validate(&self) -> VibexResult<()> {
        if let Some(limit) = self.token_limit.as_ref() {
            limit.validate()?;
        }
        for limits in self.account_limits.values() {
            limits.validate()?;
        }
        if self
            .error_rate_warn_percent
            .is_some_and(|value| value > 100)
        {
            return Err(VibexError::validation(
                "model_gateway_error_rate_threshold_invalid",
                "an error-rate threshold must be between zero and one hundred",
            ));
        }
        for value in [
            self.latency_warn_ms,
            self.default_connect_timeout_ms,
            self.default_request_timeout_ms,
            self.default_stream_idle_timeout_ms,
        ] {
            if value == Some(0) {
                return Err(VibexError::validation(
                    "model_gateway_timeout_invalid",
                    "a timeout or latency threshold must be at least one millisecond",
                ));
            }
        }
        for (kind, milliseconds) in &self.cooldown_overrides_ms {
            let parsed = GatewayFailureKind::from_id(kind).ok_or_else(|| {
                VibexError::validation(
                    "model_gateway_cooldown_kind_invalid",
                    "a cooldown override names an unknown failure kind",
                )
                .with_diagnostic("failureKind", kind)
            })?;
            if parsed == GatewayFailureKind::Auth {
                return Err(VibexError::validation(
                    "model_gateway_cooldown_kind_invalid",
                    "an authentication failure does not cool down with time",
                ));
            }
            if *milliseconds == 0 || *milliseconds > 24 * 60 * 60 * 1000 {
                return Err(VibexError::validation(
                    "model_gateway_cooldown_invalid",
                    "a cooldown override must be between one millisecond and one day",
                )
                .with_diagnostic("failureKind", kind)
                .with_diagnostic("milliseconds", milliseconds.to_string()));
            }
        }
        Ok(())
    }
}

/// The gateway's whole durable configuration.
///
/// It holds no upstream list. The gateway is a transport and a scheduler, not a
/// Provider collection: the Providers it may call are exactly the members of
/// the enabled route groups, read fresh on every snapshot. That is what makes
/// it independent of whether any route exists yet, and it is why switching it
/// off simply makes every route group unusable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewaySettings {
    pub display_name: String,
    /// The persisted listening port, or `0` before the first successful bind.
    pub port: u16,
    pub policy: GatewayPolicy,
    pub revision: i64,
    pub updated_at_ms: i64,
}

impl Default for ModelGatewaySettings {
    fn default() -> Self {
        Self {
            display_name: "Vibex Gateway".to_string(),
            port: 0,
            policy: GatewayPolicy::default(),
            revision: 1,
            updated_at_ms: 0,
        }
    }
}

impl ModelGatewaySettings {
    /// The largest display name the Config Center accepts, matching the
    /// Provider Profile limit so the gateway row is not the odd one out.
    pub const MAX_DISPLAY_NAME_LEN: usize = 160;

    pub fn validate(&self) -> VibexResult<()> {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_DISPLAY_NAME_LEN {
            return Err(VibexError::validation(
                "model_gateway_display_name_invalid",
                "gateway display name must be non-empty and within the supported length",
            ));
        }
        // A port below 1024 is not usable by an unprivileged loopback service,
        // and a port of 0 means "not assigned yet" rather than "random".
        if (1..1024).contains(&self.port) {
            return Err(VibexError::validation(
                "model_gateway_port_invalid",
                "the gateway port must be above 1023",
            ));
        }
        self.policy.validate()
    }
}

/// Lifecycle state of the listening service.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelGatewayStatus {
    /// The runtime has not started the service yet.
    Starting,
    Running,
    /// The service could not bind or was shut down. Route groups cannot be
    /// served while it is down; direct Providers are unaffected, because they
    /// never reach the gateway.
    Unavailable,
    /// The listener is up, but at least one route group has stepped out of the
    /// way after repeated upstream failures: that group refuses every request
    /// but one probe while a single attempt is allowed through.
    ///
    /// The scope is the route group, not the gateway. Other groups keep
    /// serving, and `base_url()` stays available, because a group whose
    /// members are all broken says nothing about an unrelated one.
    ///
    /// This is distinct from `Unavailable`: the port is still bound, and the
    /// state clears itself once an upstream answers again. Reporting it is the
    /// whole point — a silent refusal would leave the user unable to tell a
    /// broken group from an idle gateway.
    Degraded,
    /// The user turned the gateway off. Every route group becomes unusable and
    /// the surface stays visible.
    Disabled,
}

impl ModelGatewayStatus {
    /// Whether requests can be routed at all.
    ///
    /// A degraded gateway is still routing: the degraded set is a subset of
    /// the route groups, so a request naming a healthy group must not be
    /// turned away because a different one is stepping aside.
    pub const fn is_routable(self) -> bool {
        matches!(self, Self::Running | Self::Degraded)
    }

    /// The listener is bound even though routing may be bypassed.
    pub const fn is_listening(self) -> bool {
        matches!(self, Self::Running | Self::Degraded)
    }
}

/// A redacted, UI-facing snapshot of the running service.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewayRuntimeState {
    pub status: ModelGatewayStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub port: Option<u16>,
    /// How many enabled route groups the listener can serve right now.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_count: Option<usize>,
    /// How many of those groups are currently stepping aside.
    ///
    /// It is a count rather than a list because every surface only needs to
    /// say whether the gateway is fully serving or partly; which group is
    /// degraded is answered by the per-group routing state, not by this
    /// summary.
    #[serde(default, skip_serializing_if = "degraded_routes_is_zero")]
    pub degraded_routes: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notice: Option<String>,
    /// When the current listener became ready, for the uptime reading.
    ///
    /// It survives later status publishes so a degraded state still reports
    /// how long the gateway has been up. Absent on a state written before the
    /// field existed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub started_at_ms: Option<i64>,
    pub updated_at_ms: i64,
}

impl Default for ModelGatewayRuntimeState {
    fn default() -> Self {
        Self {
            status: ModelGatewayStatus::Starting,
            port: None,
            route_count: None,
            degraded_routes: 0,
            notice: None,
            started_at_ms: None,
            updated_at_ms: unix_timestamp_ms(),
        }
    }
}

impl ModelGatewayRuntimeState {
    pub fn running(port: u16, route_count: usize) -> Self {
        Self {
            status: ModelGatewayStatus::Running,
            port: Some(port),
            route_count: Some(route_count),
            degraded_routes: 0,
            notice: None,
            started_at_ms: Some(unix_timestamp_ms()),
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    pub fn unavailable(notice: impl Into<String>) -> Self {
        Self {
            status: ModelGatewayStatus::Unavailable,
            port: None,
            route_count: None,
            degraded_routes: 0,
            notice: Some(notice.into()),
            started_at_ms: None,
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    /// The address an Agent's base URL should point at, when the port is bound.
    ///
    /// A degraded gateway still answers on this address for every group that
    /// has not stepped aside, so withdrawing the URL here would take working
    /// groups away from the Agents using them.
    pub fn base_url(&self) -> Option<String> {
        self.port
            .filter(|_| self.status.is_listening())
            .map(|port| format!("http://127.0.0.1:{port}"))
    }
}

/// Keeps a fully-serving runtime state's JSON identical to what it was before
/// the per-group breaker existed.
pub const fn degraded_routes_is_zero(count: &usize) -> bool {
    *count == 0
}

/// Why a Key is not currently selectable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayFailureKind {
    /// The account is out of money.
    Credit,
    /// The plan's window is used up.
    Quota,
    /// Requests are arriving faster than allowed.
    Rate,
    /// The credential itself is rejected.
    Auth,
    /// The proxy in front of the request failed.
    Proxy,
    /// The upstream could not be reached or answered with an unusable error.
    Upstream,
    /// Something the gateway could not classify.
    Other,
}

impl GatewayFailureKind {
    /// How long a Key sits out after this kind of failure, in milliseconds.
    ///
    /// `auth` is absent on purpose: a rejected credential does not heal with
    /// time, so the Key becomes invalid instead of cooling down.
    pub const fn cooldown_ms(self) -> Option<i64> {
        match self {
            Self::Credit => Some(30 * 60 * 1000),
            Self::Quota => Some(15 * 60 * 1000),
            Self::Rate => Some(30 * 1000),
            Self::Auth => None,
            Self::Proxy => Some(60 * 1000),
            Self::Upstream | Self::Other => Some(10 * 1000),
        }
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Credit => "credit",
            Self::Quota => "quota",
            Self::Rate => "rate",
            Self::Auth => "auth",
            Self::Proxy => "proxy",
            Self::Upstream => "upstream",
            Self::Other => "other",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        [
            Self::Credit,
            Self::Quota,
            Self::Rate,
            Self::Auth,
            Self::Proxy,
            Self::Upstream,
            Self::Other,
        ]
        .into_iter()
        .find(|kind| kind.id() == value.trim())
    }

    /// Whether another Key should be tried for the same request.
    ///
    /// A rejected Key is marked invalid, but the request may still succeed on
    /// a different Key: that is the whole point of a pool. Only a reply that
    /// has already reached the client ends the attempt loop.
    pub const fn should_fail_over(self) -> bool {
        true
    }
}

/// Durable per-Key routing state. Key material never appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelProviderAccountState {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub enabled: bool,
    pub priority: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_until_ms: Option<i64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cooldown_reason: Option<GatewayFailureKind>,
    pub consecutive_failures: i64,
    pub updated_at_ms: i64,
}

impl ModelProviderAccountState {
    pub fn ready(
        model_provider_profile_id: ModelProviderProfileId,
        credential_id: RequestId,
    ) -> Self {
        Self {
            model_provider_profile_id,
            credential_id,
            enabled: true,
            priority: 0,
            cooldown_until_ms: None,
            cooldown_reason: None,
            consecutive_failures: 0,
            updated_at_ms: unix_timestamp_ms(),
        }
    }

    /// Whether this Key may be selected at `now_ms`.
    ///
    /// An authentication failure has no expiry: the credential is wrong until
    /// the user replaces it, so it never becomes ready again on its own.
    pub fn is_ready_at(&self, now_ms: i64) -> bool {
        self.enabled
            && self.cooldown_reason != Some(GatewayFailureKind::Auth)
            && self.cooldown_until_ms.is_none_or(|until| until <= now_ms)
    }

    /// The visible state of one Key, in the order the UI needs to explain it.
    pub fn visibility_at(&self, now_ms: i64) -> GatewayKeyState {
        if !self.enabled {
            return GatewayKeyState::Disabled;
        }
        match self.cooldown_reason {
            Some(GatewayFailureKind::Auth) => GatewayKeyState::Invalid,
            Some(kind) if self.cooldown_until_ms.is_some_and(|until| until > now_ms) => {
                GatewayKeyState::Cooling {
                    kind,
                    until_ms: self.cooldown_until_ms,
                }
            }
            _ => GatewayKeyState::Ready,
        }
    }
}

/// What the Config Center shows for one Key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayKeyState {
    Ready,
    Cooling {
        kind: GatewayFailureKind,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        until_ms: Option<i64>,
    },
    /// Authentication was rejected; the user has to replace the Key.
    Invalid,
    /// The user turned this Key off.
    Disabled,
}

impl GatewayKeyState {
    pub const fn is_ready(self) -> bool {
        matches!(self, Self::Ready)
    }

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ready => "ready",
            Self::Cooling { .. } => "cooling",
            Self::Invalid => "invalid",
            Self::Disabled => "disabled",
        }
    }
}

/// One row of the gateway's request ledger.
///
/// This record deliberately holds no prompt, no reply, no image bytes, and no
/// Key material: only the shape of the decision and the token counts the
/// upstream reported. It is pruned after
/// [`ModelGatewayRequestLog::RETENTION_MS`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelGatewayRequestLog {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    pub model_provider_profile_id: ModelProviderProfileId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<RequestId>,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_served: Option<String>,
    pub wire_in: String,
    pub wire_out: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status: Option<u16>,
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    #[serde(default)]
    pub cached_write_tokens: u64,
    #[serde(default)]
    pub reasoning_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    pub outcome: GatewayRequestOutcome,
    /// What the router did before a Key answered. Absent on a row written
    /// before the trace existed, and on a request that never got as far as a
    /// routing decision.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GatewayRouteTrace>,
    pub created_at_ms: i64,
}

impl ModelGatewayRequestLog {
    /// The ledger keeps a rolling week of metadata.
    pub const RETENTION_MS: i64 = 7 * 24 * 60 * 60 * 1000;

    pub fn total_tokens(&self) -> u64 {
        self.input_tokens
            .saturating_add(self.output_tokens)
            .saturating_add(self.cached_read_tokens)
            .saturating_add(self.cached_write_tokens)
            .saturating_add(self.reasoning_tokens)
    }
}

/// The payload version of [`GatewayRouteTrace`] as it is stored on a row.
///
/// The trace is the one ledger column whose shape may grow, so it carries its
/// own version. A reader that meets a version it does not know reports the row
/// as having no trace rather than failing the whole list: the ledger is a
/// diagnostic, and one unreadable annotation must not hide every other request.
pub const GATEWAY_ROUTE_TRACE_SCHEMA_VERSION: u32 = 1;

/// One Key the gateway tried and set aside before another one answered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteAttempt {
    pub credential_id: RequestId,
    /// Why this Key did not serve the reply.
    pub failure: GatewayFailureKind,
}

/// What the router did with one request, as the ledger records it.
///
/// Only ids appear here. The trace explains a failover — which Key was cooled
/// or over its ceiling, and which Key was tried and failed — without ever
/// naming Key material, a prompt, or a reply. Payload schema
/// [`GATEWAY_ROUTE_TRACE_SCHEMA_VERSION`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteTrace {
    pub schema_version: u32,
    /// Keys set aside before the first attempt because they were cooling.
    #[serde(default)]
    pub skipped_cooling: u32,
    /// Keys set aside because they had spent their local token ceiling.
    #[serde(default)]
    pub skipped_over_limit: u32,
    /// Keys tried, in order, that did not answer.
    #[serde(default)]
    pub failed_attempts: Vec<GatewayRouteAttempt>,
    /// Members this request walked past before one answered.
    ///
    /// A route may fail over across Providers as well as across Keys, and the
    /// two mean different things: a Key cooled down, while a whole member was
    /// unavailable. Keeping them in one list would make "which Provider did it
    /// try" unreadable.
    #[serde(default)]
    pub skipped_members: Vec<GatewaySkippedMember>,
    /// The gateway's own stable error code when the request never produced a
    /// reply.
    ///
    /// A failed row used to be count-only: the ledger said "failed" and the
    /// Agent's message said what the user read, and nothing joined the two. The
    /// code is the join — `model_gateway_translation_failed` points at the
    /// gateway, `model_gateway_protocol_mismatch` at the endpoint — so a later
    /// investigation starts from the ledger instead of a session transcript.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// What the upstream said when it refused the request.
    ///
    /// A `400` from a vendor is a description of the field it objected to
    /// ("Invalid schema for function 'read_file': ..."), and it is the only
    /// place that description exists: the Agent shows its own generic wrapper
    /// and the ledger used to keep the status alone. Redacted and bounded the
    /// same way every other diagnostic is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Why the member order was chosen, when a rule or classifier decided it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<GatewayRouteDecisionTrace>,
}

/// One route member that did not answer, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySkippedMember {
    pub model_provider_profile_id: ModelProviderProfileId,
    /// The protocol the member would have been called with.
    #[serde(default)]
    pub wire_protocol_id: String,
    pub failure: GatewayFailureKind,
}

/// Why the router chose the member order it did.
///
/// This is the part of the trace a reader can turn into a sentence: which rule
/// matched, what the classifier said, which effort was chosen, and how long the
/// route was willing to wait. It names ids and kinds only, never a prompt.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteDecisionTrace {
    /// The zero-based rule index that matched, for "rule 2" copy.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_index: Option<usize>,
    /// The rule id, which survives reordering better than an index does.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rule_id: Option<String>,
    /// A short machine-readable reason: `rule`, `classifier`, or `default`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier: Option<String>,
    /// The classifier request failed; the router fell back to the default order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier_error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub strategy: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patience_ms: Option<u64>,
    /// How many times the whole member walk was retried before it answered.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub retries: u32,
}

const fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}

impl GatewayRouteDecisionTrace {
    /// Whether there is anything to explain beyond the default order.
    pub fn is_unremarkable(&self) -> bool {
        self.rule_index.is_none()
            && self.rule_id.is_none()
            && self.reason.is_none()
            && self.classifier.is_none()
            && self.classifier_error.is_none()
            && self.intent.is_none()
            && self.effort.is_none()
            && self.strategy.is_none()
            && self.patience_ms.is_none()
            && self.retries == 0
    }
}

impl GatewayRouteTrace {
    /// A fresh trace at the current payload version.
    pub fn new(skipped_cooling: usize, skipped_over_limit: usize) -> Self {
        Self {
            schema_version: GATEWAY_ROUTE_TRACE_SCHEMA_VERSION,
            skipped_cooling: u32::try_from(skipped_cooling).unwrap_or(u32::MAX),
            skipped_over_limit: u32::try_from(skipped_over_limit).unwrap_or(u32::MAX),
            failed_attempts: Vec::new(),
            skipped_members: Vec::new(),
            error_code: None,
            error_message: None,
            decision: None,
        }
    }

    /// Whether the trace says anything beyond "the first Key answered".
    ///
    /// A failed request is always worth keeping, even when it walked past
    /// nothing: the error code is the whole reason the row exists.
    pub fn is_unremarkable(&self) -> bool {
        self.error_code.is_none()
            && self.error_message.is_none()
            && self.skipped_cooling == 0
            && self.skipped_over_limit == 0
            && self.failed_attempts.is_empty()
            && self.skipped_members.is_empty()
            && self
                .decision
                .as_ref()
                .is_none_or(GatewayRouteDecisionTrace::is_unremarkable)
    }

    /// Whether a reader can interpret this payload.
    pub fn is_current(&self) -> bool {
        self.schema_version == GATEWAY_ROUTE_TRACE_SCHEMA_VERSION
    }
}

/// Redacts and bounds a vendor message for the ledger.
///
/// An upstream refusal is the only description of which field it objected to,
/// so it is worth keeping — but it is vendor text that may quote a request
/// header or a path. Both are already the gateway's own diagnostics contract:
/// a bounded length and the shared sensitive-value check.
pub fn gateway_diagnostic_text(message: &str) -> String {
    const MAX: usize = 512;
    let trimmed = message.trim();
    if trimmed.is_empty() {
        return String::new();
    }
    let lower = trimmed.to_ascii_lowercase();
    let sensitive = lower.contains("api_key")
        || lower.contains("apikey")
        || lower.contains("authorization")
        || lower.contains("bearer ")
        || lower.contains("password")
        || lower.contains("secret")
        || lower.starts_with("sk-");
    if sensitive {
        return "[redacted-sensitive-output]".to_string();
    }
    if trimmed.chars().count() <= MAX {
        return trimmed.to_string();
    }
    let mut bounded = trimmed.chars().take(MAX).collect::<String>();
    bounded.push('…');
    bounded
}

/// How a gateway request ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GatewayRequestOutcome {
    Ok,
    /// A Key failed before the reply began and another one answered.
    Failover,
    /// The provider's Keys were all cooling down.
    Cooldown,
    /// The request was refused before reaching an upstream.
    Rejected,
    Failed,
}

impl GatewayRequestOutcome {
    pub const fn id(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Failover => "failover",
            Self::Cooldown => "cooldown",
            Self::Rejected => "rejected",
            Self::Failed => "failed",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        [
            Self::Ok,
            Self::Failover,
            Self::Cooldown,
            Self::Rejected,
            Self::Failed,
        ]
        .into_iter()
        .find(|outcome| outcome.id() == value.trim())
    }
}

/// Tokens observed for one Provider in one local ceiling window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayTokenWindowUsage {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub window: GatewayTokenWindow,
    pub window_start_ms: i64,
    pub window_end_ms: i64,
    pub used_tokens: u64,
}

impl GatewayTokenWindowUsage {
    pub const fn is_open_at(&self, now_ms: i64) -> bool {
        self.window_start_ms <= now_ms && now_ms < self.window_end_ms
    }

    pub const fn remaining(&self, limit_tokens: u64) -> u64 {
        limit_tokens.saturating_sub(self.used_tokens)
    }
}

/// One conversation's pinned Key.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayConversationPin {
    pub conversation_key: String,
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub chosen_at_ms: i64,
    pub expires_at_ms: i64,
}

impl GatewayConversationPin {
    /// A conversation with no traffic for this long is re-picked, because the
    /// vendor's prompt cache would have expired anyway.
    pub const TTL_MS: i64 = 30 * 60 * 1000;

    pub fn is_live_at(&self, now_ms: i64) -> bool {
        self.expires_at_ms > now_ms
    }
}

/// One Key as the gateway sees it, for the routing decision and the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayKeyCandidate {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub priority: i64,
    pub state: GatewayKeyState,
    pub used_tokens: u64,
}

impl GatewayKeyCandidate {
    pub const fn is_selectable(&self) -> bool {
        self.state.is_ready()
    }
}

/// Which upstream answered a request, and why.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayRouteDecision {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    pub pinned: bool,
    pub skipped_cooling: usize,
    pub failover_attempts: usize,
}

/// The model a request names, split into its optional Provider prefix and id.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GatewayModelRef<'a> {
    pub provider: Option<&'a str>,
    pub model: &'a str,
}

impl<'a> GatewayModelRef<'a> {
    /// Splits `provider/model` without treating a model id that merely
    /// contains a slash as qualified.
    pub fn parse(value: &'a str) -> Self {
        match value.split_once('/') {
            Some((provider, model)) if !provider.is_empty() && !model.is_empty() => Self {
                provider: Some(provider),
                model,
            },
            _ => Self {
                provider: None,
                model: value,
            },
        }
    }
}

/// One model the gateway serves, for the Config Center's read-only list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayModelSummary {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub provider_name: String,
    pub model_id: String,
    /// The id an Agent's model picker shows.
    pub display_id: String,
    pub wire_protocol_id: String,
    /// The route group this entry belongs to.
    ///
    /// Every model the gateway serves comes from a group, so this is present on
    /// every entry. It is an `Option` so a client written before groups is not
    /// forced to interpret it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route_id: Option<String>,
    /// How many Providers can answer the group.
    #[serde(default)]
    pub source_count: usize,
    /// How many models the group offers.
    #[serde(default)]
    pub model_count: usize,
}

/// The whole Config Center read for the gateway.
///
/// There is no Agent list here: an Agent reaches the gateway by binding a route
/// group, so the binding is the one switch and it lives with the route. There
/// is no upstream candidate list either: the gateway may call exactly the
/// members of the enabled route groups.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySnapshot {
    pub settings: ModelGatewaySettings,
    pub runtime: ModelGatewayRuntimeState,
    /// The Providers the enabled route groups name, with their Keys.
    ///
    /// This is a read of the groups, not a configuration: adding a Provider
    /// here would mean adding it to a group.
    pub upstreams: Vec<GatewayUpstreamSummary>,
    pub models: Vec<GatewayModelSummary>,
}

impl GatewaySnapshot {
    /// The number of Keys currently selectable across every upstream.
    pub fn ready_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| key.state.is_ready())
            .count()
    }

    /// The number of Keys sitting out a cooldown.
    pub fn cooling_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| matches!(key.state, GatewayKeyState::Cooling { .. }))
            .count()
    }

    /// The number of Keys the vendor rejected outright.
    pub fn invalid_key_count(&self) -> usize {
        self.upstreams
            .iter()
            .flat_map(|upstream| &upstream.keys)
            .filter(|key| matches!(key.state, GatewayKeyState::Invalid))
            .count()
    }

    /// Whether the surface can actually serve a request.
    ///
    /// A running listener with no route groups is routable in the sense that
    /// matters here: it is up and nothing is broken. It simply has no models to
    /// offer, which the model list already says. Treating "no routes yet" as
    /// "cannot route" made the first launch of a fresh installation look like a
    /// failure.
    pub fn is_routable(&self) -> bool {
        self.runtime.status.is_routable()
    }

    /// Whether at least one group currently offers a usable model.
    pub fn has_servable_models(&self) -> bool {
        !self.models.is_empty() && self.ready_key_count() > 0
    }
}

/// One upstream as the Config Center shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayUpstreamSummary {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub display_name: String,
    pub model_count: usize,
    pub keys: Vec<GatewayKeySummary>,
}

/// One Key's visible state. The secret itself is never part of this record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayKeySummary {
    pub credential_id: RequestId,
    pub display_name: String,
    /// The masked fingerprint the row shows instead of the credential id.
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub masked_hint: String,
    pub state: GatewayKeyState,
    #[serde(default)]
    pub used_tokens: u64,
    /// This Key's concurrency and rate limits, when it has any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limits: Option<GatewayAccountLimits>,
    /// Optional model ids this Key may serve. Empty means every model.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub models: Vec<String>,
    /// Optional per-Key proxy URL.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub proxy_url: Option<String>,
}

/// The retired per-Agent gateway switch, kept only so an older client that
/// still sends it gets a clear refusal instead of an unknown-variant decode
/// failure.
///
/// An Agent reaches the gateway by binding a route group. There is no second
/// switch, and adding one back would let an Agent believe it is routed while the
/// listener serves a different set of groups.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAgentUpdateRequest {
    pub agent_id: String,
    pub enabled: bool,
}

/// One request the gateway served, for the diagnostics list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRequestSummary {
    pub request_id: RequestId,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The Provider that answered, by id.
    ///
    /// The display name beside it is for reading; two Providers may share one,
    /// and a surface that filters the ledger by Provider must not conflate
    /// them.
    #[serde(default)]
    pub model_provider_profile_id: ModelProviderProfileId,
    pub provider_name: String,
    pub model_requested: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_served: Option<String>,
    pub wire_in: String,
    pub wire_out: String,
    pub status: Option<u16>,
    pub total_tokens: u64,
    /// The split behind `total_tokens`, so a detail view can name each part.
    #[serde(default)]
    pub input_tokens: u64,
    #[serde(default)]
    pub output_tokens: u64,
    #[serde(default)]
    pub cached_read_tokens: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_ms: Option<i64>,
    pub outcome: GatewayRequestOutcome,
    /// The Key that answered, under the name its Provider gives it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
    /// The route, once a Key has answered and the trace says something the
    /// caller could not already see.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub route: Option<GatewayRouteSummary>,
    pub created_at_ms: i64,
}

impl GatewayRequestSummary {
    /// Projects one stored row for a client.
    ///
    /// Key ids become the names their Provider gave them, because a client
    /// showing a route is explaining a decision, not exposing an id table. A
    /// Key that has since been removed keeps its row and loses only its name.
    pub fn from_log(mut log: ModelGatewayRequestLog, profiles: &[ModelProviderProfile]) -> Self {
        let profile = profiles
            .iter()
            .find(|profile| profile.id == log.model_provider_profile_id);
        let name_of = |credential: &RequestId| {
            profile.and_then(|profile| {
                profile
                    .credentials
                    .iter()
                    .find(|candidate| &candidate.id == credential)
                    .map(|candidate| candidate.display_label())
            })
        };
        let route = log
            .route
            .take()
            .filter(|trace| !trace.is_unremarkable())
            .map(|trace| GatewayRouteSummary {
                skipped_cooling: trace.skipped_cooling,
                skipped_over_limit: trace.skipped_over_limit,
                failed_attempts: trace
                    .failed_attempts
                    .into_iter()
                    .map(|attempt| GatewayRouteAttemptSummary {
                        credential_name: name_of(&attempt.credential_id),
                        credential_id: attempt.credential_id,
                        failure: attempt.failure,
                    })
                    .collect(),
                skipped_members: trace
                    .skipped_members
                    .into_iter()
                    .map(|member| GatewaySkippedMemberSummary {
                        provider_name: profiles
                            .iter()
                            .find(|candidate| candidate.id == member.model_provider_profile_id)
                            .map(|candidate| candidate.display_name.clone()),
                        model_provider_profile_id: member.model_provider_profile_id,
                        wire_protocol_id: member.wire_protocol_id,
                        failure: member.failure,
                    })
                    .collect(),
                error_code: trace.error_code,
                error_message: trace.error_message,
                decision: trace.decision,
            });
        Self {
            model_provider_profile_id: profile
                .map(|profile| profile.id.clone())
                .unwrap_or_else(|| log.model_provider_profile_id.clone()),
            total_tokens: log.total_tokens(),
            input_tokens: log.input_tokens,
            output_tokens: log.output_tokens,
            cached_read_tokens: log.cached_read_tokens,
            request_id: log.request_id,
            agent_id: log.agent_id,
            provider_name: profile
                .map(|profile| profile.display_name.clone())
                .unwrap_or_else(|| log.model_provider_profile_id.as_str().to_string()),
            model_requested: log.model_requested,
            model_served: log.model_served,
            wire_in: log.wire_in,
            wire_out: log.wire_out,
            status: log.status,
            latency_ms: log.latency_ms,
            outcome: log.outcome,
            credential_name: log.credential_id.as_ref().and_then(&name_of),
            route,
            created_at_ms: log.created_at_ms,
        }
    }
}

/// One request the gateway is serving right now.
///
/// This is memory state, never a ledger row: a request that is still running
/// has no outcome to record, and the graph needs to draw it *while* it runs.
/// The identifiers are the ones a client already sees; no prompt, reply, or Key
/// material appears here.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayInflight {
    pub request_id: RequestId,
    /// The Agent that sent it, when the request carried one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<String>,
    /// The route, when the request named one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_route_id: Option<String>,
    /// The model id the Agent asked for.
    pub requested_model: String,
    /// The member currently answering, and its place in the attempt order.
    pub model_provider_profile_id: ModelProviderProfileId,
    /// Which member of the route this is, counting from one: the graph numbers
    /// attempts so a failover reads as "the second source".
    pub attempt: u32,
    /// The Key serving this attempt, when one has been chosen.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_id: Option<RequestId>,
    /// The protocol the member is called with.
    pub wire_protocol_id: String,
    pub started_at_ms: i64,
    /// Set once the first token has arrived. A request with a deadline and no
    /// first token yet is the one that may be abandoned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_at_ms: Option<i64>,
}

impl GatewayInflight {
    /// Whether this request has produced any of its reply yet.
    pub fn has_first_token(&self) -> bool {
        self.first_token_at_ms.is_some()
    }
}

/// What the gateway tells a client about a request it is serving.
///
/// These are the transitions a graph needs, not every internal step: a request
/// begins, it starts answering, or it ends. Member-level retries are folded
/// into a new `Began` with a higher attempt number, because that is what the
/// graph draws.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", tag = "kind")]
pub enum GatewayEvent {
    /// A request started, or moved to another member.
    Began(GatewayInflight),
    /// The member produced its first token.
    FirstToken { request_id: RequestId, at_ms: i64 },
    /// The request finished, however it ended.
    Ended {
        request_id: RequestId,
        outcome: GatewayRequestOutcome,
    },
    /// A pinned member failed and the route asks before switching.
    ///
    /// The request is held while this is outstanding: the answer decides which
    /// member serves it, so nothing can proceed until either the user answers or
    /// the prompt expires.
    FailoverAsked(GatewayFailoverPrompt),
    /// The prompt is no longer outstanding, whatever settled it.
    FailoverSettled {
        prompt_id: RequestId,
        answer: GatewayFailoverAnswer,
    },
    /// A conversation moved from one member to another.
    ///
    /// Emitted for every switch, whether it was automatic or answered, because
    /// the point is the same in both cases: a reader has to learn that the
    /// answer came from a different Provider than the one the model name might
    /// suggest.
    FailoverSwitched {
        request_id: RequestId,
        model_route_id: Option<String>,
        model: String,
        from_name: String,
        to_name: String,
    },
}

impl GatewayEvent {
    /// The request this event is about.
    pub fn request_id(&self) -> &RequestId {
        match self {
            Self::Began(inflight) => &inflight.request_id,
            Self::FirstToken { request_id, .. } => request_id,
            Self::Ended { request_id, .. } => request_id,
            Self::FailoverAsked(prompt) => &prompt.id,
            Self::FailoverSettled { prompt_id, .. } => prompt_id,
            Self::FailoverSwitched { request_id, .. } => request_id,
        }
    }
}

/// The route, with each Key named the way its Provider labels it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteSummary {
    pub skipped_cooling: u32,
    pub skipped_over_limit: u32,
    pub failed_attempts: Vec<GatewayRouteAttemptSummary>,
    /// Members the request walked past, with their Providers named.
    #[serde(default)]
    pub skipped_members: Vec<GatewaySkippedMemberSummary>,
    /// The gateway's own error code, when the request never produced a reply.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    /// What the upstream said it refused, when it refused one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_message: Option<String>,
    /// Why the router chose this member order, when a rule or classifier did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<GatewayRouteDecisionTrace>,
}

/// One skipped route member, named for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySkippedMemberSummary {
    /// Absent once the Provider has been removed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_name: Option<String>,
    pub model_provider_profile_id: ModelProviderProfileId,
    pub wire_protocol_id: String,
    pub failure: GatewayFailureKind,
}

/// One failed attempt, named for display.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayRouteAttemptSummary {
    /// Absent once the Key has been removed from its Provider.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub credential_name: Option<String>,
    pub credential_id: RequestId,
    pub failure: GatewayFailureKind,
}

/// The settings mutation the Config Center submits.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewaySettingsUpdateRequest {
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<GatewayTokenLimit>,
    /// Concurrency and rate limits per credential. An omitted entry leaves that
    /// Key unlimited, and the whole map replaces the stored one.
    #[serde(default)]
    pub account_limits: BTreeMap<RequestId, GatewayAccountLimits>,
    /// Which protocol directions the gateway may translate.
    ///
    /// The write replaces the whole policy, like `account_limits`: an older
    /// client that omits the field sends the default, where every direction is
    /// allowed.
    #[serde(default)]
    pub translate: GatewayTranslatePolicy,
    /// Routing-state changes submitted with this write.
    ///
    /// These are commands, not a snapshot: only the Keys named here are
    /// touched, so turning one Key off does not rewrite every other Key's
    /// cooldown or limits.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub account_updates: Vec<GatewayAccountUpdate>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_rate_warn_percent: Option<u16>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latency_warn_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_connect_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_request_timeout_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub default_stream_idle_timeout_ms: Option<u64>,
    /// Per-failure-kind cooldown overrides; the write replaces the whole map.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub cooldown_overrides_ms: BTreeMap<String, u64>,
    pub expected_revision: i64,
}

/// One Key's routing-state change, as the Config Center submits it.
///
/// `enabled` and `preferred` are independent: a Key can be turned off without
/// changing which Key is preferred, and a Key can be preferred while it is
/// off (the preference takes effect again once it is enabled).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayAccountUpdate {
    pub model_provider_profile_id: ModelProviderProfileId,
    pub credential_id: RequestId,
    /// `Some(true)` re-enables the Key and clears any failure cooldown, so an
    /// account repaired outside Vibex can be tried again; `Some(false)` takes
    /// it out of rotation; `None` leaves it as it is.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub enabled: Option<bool>,
    /// `Some(true)` makes this the Key the router tries first for its
    /// Provider; `Some(false)` returns that Provider to its configured order.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preferred: Option<bool>,
}

impl GatewayAccountUpdate {
    pub fn validate(&self) -> VibexResult<()> {
        if self.enabled.is_none() && self.preferred.is_none() {
            return Err(VibexError::validation(
                "model_gateway_account_update_empty",
                "an account update must change `enabled` or `preferred`",
            ));
        }
        Ok(())
    }
}

/// The editor's question: what does the catalogue suggest for this model?
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCapabilitySuggestionRequest {
    /// The catalogue's provider id, such as `deepseek`.
    pub provider_id: String,
    pub model_id: String,
}

/// One model the caller wants the catalogue to identify.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCanonicalIdLookup {
    /// The catalogue's provider id, such as `deepseek`.
    pub provider_id: String,
    pub model_id: String,
}

/// The same-model question for several models at once.
///
/// A "same model" suggestion has to compare every configured model across
/// Providers, so one round trip per model would be the wrong shape. The answer
/// keeps the request's order, which is what lets the caller pair them up.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCanonicalIdsRequest {
    pub models: Vec<ModelCanonicalIdLookup>,
}

/// One model's catalogue identity, when the catalogue names one.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelCanonicalId {
    pub provider_id: String,
    pub model_id: String,
    /// The vendor-independent id. `None` means the model is not in the
    /// catalogue, which must read as "no suggestion", not as "unique".
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_model_id: Option<String>,
}

/// One suggested field, with how the match was made.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCapabilitySuggestion {
    /// `contextTokens`, `outputTokens`, `reasoning`, `imageInput`, `pdfInput`,
    /// or `temperature`.
    pub fields: Vec<GatewayCapabilityField>,
    /// The catalogue matched a longer id this one extends.
    pub approximate: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GatewayCapabilityField {
    pub field: String,
    /// The value as the editor's own field spelling, so it can be shown without
    /// a second mapping table.
    pub value: String,
}

impl GatewayCapabilitySuggestion {
    /// One suggested field's raw value.
    pub fn field(&self, name: &str) -> Option<&str> {
        self.fields
            .iter()
            .find(|entry| entry.field == name)
            .map(|entry| entry.value.as_str())
    }

    /// A suggested numeric field.
    pub fn number(&self, name: &str) -> Option<u32> {
        self.field(name).and_then(|value| value.parse().ok())
    }

    /// A suggested boolean field.
    pub fn flag(&self, name: &str) -> Option<bool> {
        self.field(name).map(|value| value == "true")
    }

    /// Writes one suggested field into a declaration and records it as having
    /// come from the catalogue.
    ///
    /// Adoption is a deliberate act on one field, so it is per field rather
    /// than "apply everything". A field the suggestion does not carry is left
    /// exactly as it was, and the return value says whether anything changed.
    pub fn adopt(&self, capabilities: &mut crate::ProviderModelCapabilities, field: &str) -> bool {
        let adopted = match field {
            "contextTokens" => self
                .number(field)
                .map(|value| capabilities.context_tokens = Some(value)),
            "outputTokens" => self
                .number(field)
                .map(|value| capabilities.output_tokens = Some(value)),
            "reasoning" => self
                .flag(field)
                .map(|value| capabilities.reasoning = Some(value)),
            "imageInput" => self
                .flag(field)
                .map(|value| capabilities.image_input = Some(value)),
            "pdfInput" => self
                .flag(field)
                .map(|value| capabilities.pdf_input = Some(value)),
            "temperature" => self
                .flag(field)
                .map(|value| capabilities.temperature = Some(value)),
            _ => None,
        };
        if adopted.is_some() {
            capabilities.set_source(field, crate::ModelCapabilitySource::Catalog);
            true
        } else {
            false
        }
    }
}

/// Where one Provider Profile field lands in a gateway-aware editor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProfileFieldVisibility {
    Editable,
    ReadOnly,
    Hidden,
}

impl ProfileFieldVisibility {
    pub const fn is_editable(self) -> bool {
        matches!(self, Self::Editable)
    }

    pub const fn is_visible(self) -> bool {
        !matches!(self, Self::Hidden)
    }
}

/// The visibility the Config Center applies to a Provider Profile's fields.
///
/// A regular Provider is edited exactly as it is. The built-in gateway appears
/// in the same list but only exposes what a user can meaningfully change: its
/// address, credentials, and aggregated model catalogue are computed by the
/// runtime, so editing them would only break every binding that points at it.
pub fn profile_field_visibility(is_gateway: bool, field: &str) -> ProfileFieldVisibility {
    if !is_gateway {
        return ProfileFieldVisibility::Editable;
    }
    match field {
        "displayName" | "status" | "routing" | "pin" | "tokenLimit" | "upstreams" => {
            ProfileFieldVisibility::Editable
        }
        "usedByAgents" | "runtimeStatus" | "protocolCapabilities" | "keyState" => {
            ProfileFieldVisibility::ReadOnly
        }
        _ => ProfileFieldVisibility::Hidden,
    }
}

/// The verdict for a batch of Keys after one failed request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GatewayKeyHealth {
    Ready,
    Cooling {
        kind: GatewayFailureKind,
        until_ms: i64,
    },
    Invalid,
}

impl GatewayKeyHealth {
    pub fn from_failure(kind: GatewayFailureKind, now_ms: i64) -> Self {
        match kind.cooldown_ms() {
            Some(duration) => Self::Cooling {
                kind,
                until_ms: now_ms.saturating_add(duration),
            },
            None => Self::Invalid,
        }
    }
}

impl fmt::Display for GatewayRoutingStrategy {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.id())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_failover_prompt_counts_down_to_its_deadline() {
        let prompt = GatewayFailoverPrompt {
            id: RequestId::new(),
            model_route_id: "route-1".to_string(),
            route_name: "Team".to_string(),
            model: "deepseek-chat".to_string(),
            from_profile_id: ModelProviderProfileId::new(),
            from_name: "alpha".to_string(),
            to_profile_id: ModelProviderProfileId::new(),
            to_name: "beta".to_string(),
            expires_at_ms: 10_500,
            asked_at_ms: 5_500,
        };
        // The window is the one the prompt was built with, and it never goes
        // negative: a client that asks after the deadline must not draw a
        // countdown that counts up.
        assert_eq!(prompt.remaining_ms(5_500), 5_000);
        assert_eq!(prompt.remaining_ms(8_000), 2_500);
        assert_eq!(prompt.remaining_ms(10_500), 0);
        assert_eq!(prompt.remaining_ms(99_999), 0);
    }

    #[test]
    fn a_failover_setting_round_trips_through_its_id() {
        assert_eq!(GatewayFailover::default(), GatewayFailover::Auto);
        for setting in GatewayFailover::ALL {
            assert_eq!(GatewayFailover::from_id(setting.id()), Some(setting));
        }
        assert_eq!(GatewayFailover::from_id("nonsense"), None);
        // The wire form is what the route table stores, so it has to be the id
        // rather than the Rust variant name.
        assert_eq!(
            serde_json::to_string(&GatewayFailover::Ask).unwrap(),
            "\"ask\""
        );
    }

    #[test]
    fn gateway_contracts_carry_no_money_fields() {
        let settings = ModelGatewaySettings {
            policy: GatewayPolicy {
                token_limit: Some(GatewayTokenLimit {
                    window: GatewayTokenWindow::Month,
                    tokens: 20_000_000,
                }),
                ..GatewayPolicy::default()
            },
            ..ModelGatewaySettings::default()
        };
        let encoded = serde_json::to_string(&settings).unwrap().to_lowercase();
        for forbidden in [
            "cost", "price", "currency", "amount", "usd", "budget", "micros",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "gateway settings must not carry `{forbidden}`: {encoded}"
            );
        }

        let log = ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: Some("codex".to_string()),
            model_provider_profile_id: ModelProviderProfileId::new(),
            credential_id: Some(RequestId::new()),
            model_requested: "deepseek/deepseek-chat".to_string(),
            model_served: Some("deepseek-chat".to_string()),
            wire_in: crate::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            wire_out: crate::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            status: Some(200),
            input_tokens: 1024,
            output_tokens: 180,
            cached_read_tokens: 800,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: Some(42),
            outcome: GatewayRequestOutcome::Ok,
            route: None,
            created_at_ms: unix_timestamp_ms(),
        };
        let encoded = serde_json::to_string(&log).unwrap().to_lowercase();
        for forbidden in [
            "cost", "price", "currency", "amount", "usd", "budget", "micros",
        ] {
            assert!(
                !encoded.contains(forbidden),
                "gateway request log must not carry `{forbidden}`: {encoded}"
            );
        }
    }

    /// An empty command is a client bug, not a no-op: it would bump the
    /// settings revision and rewrite settings for no visible reason.
    #[test]
    fn an_account_update_must_change_something() {
        let profile = ModelProviderProfileId::new();
        let update = GatewayAccountUpdate {
            model_provider_profile_id: profile.clone(),
            credential_id: RequestId::new(),
            enabled: None,
            preferred: None,
        };
        assert_eq!(
            update.validate().unwrap_err().code,
            "model_gateway_account_update_empty"
        );

        for (enabled, preferred) in [
            (Some(false), None),
            (None, Some(true)),
            (Some(true), Some(false)),
        ] {
            let update = GatewayAccountUpdate {
                model_provider_profile_id: profile.clone(),
                credential_id: RequestId::new(),
                enabled,
                preferred,
            };
            update.validate().unwrap();
        }
    }

    /// Preference is settings, so it round-trips with the policy and stays out
    /// of the Key's own state (which only holds health).
    #[test]
    fn a_preferred_key_round_trips_in_the_policy() {
        let profile = ModelProviderProfileId::new();
        let credential = RequestId::new();
        let policy = GatewayPolicy {
            preferred_keys: BTreeMap::from([(profile.clone(), credential.clone())]),
            ..GatewayPolicy::default()
        };
        let encoded = serde_json::to_string(&policy).unwrap();
        assert!(encoded.contains("preferredKeys"), "{encoded}");
        let decoded: GatewayPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded.preferred_keys.get(&profile), Some(&credential));

        // An older payload without the field still loads: preference is
        // additive, not a migration gate.
        let bare: GatewayPolicy =
            serde_json::from_str(r#"{"enabled":true,"routing":"ordered","pin":"conversation"}"#)
                .unwrap();
        assert!(bare.preferred_keys.is_empty());
    }

    #[test]
    fn account_state_separates_cooling_from_invalid() {
        let profile = ModelProviderProfileId::new();
        let credential = RequestId::new();
        let now = 1_000;

        let mut state = ModelProviderAccountState::ready(profile.clone(), credential.clone());
        assert_eq!(state.visibility_at(now), GatewayKeyState::Ready);
        assert!(state.is_ready_at(now));

        state.cooldown_reason = Some(GatewayFailureKind::Rate);
        state.cooldown_until_ms = Some(now + 30_000);
        assert_eq!(
            state.visibility_at(now),
            GatewayKeyState::Cooling {
                kind: GatewayFailureKind::Rate,
                until_ms: Some(now + 30_000)
            }
        );
        assert!(!state.is_ready_at(now));
        assert!(state.is_ready_at(now + 30_000));

        // An auth failure has no duration: the Key is invalid until replaced.
        state.cooldown_reason = Some(GatewayFailureKind::Auth);
        state.cooldown_until_ms = None;
        assert_eq!(state.visibility_at(now), GatewayKeyState::Invalid);
        assert!(!state.is_ready_at(now));

        state.cooldown_reason = None;
        state.enabled = false;
        assert_eq!(state.visibility_at(now), GatewayKeyState::Disabled);
    }

    /// A summary names the Keys a route used, and says nothing when there is
    /// nothing to explain.
    ///
    /// The two behaviours are one contract: the UI shows a route only when the
    /// trace has something a reader could not already see, so a first-try
    /// success must not produce one.
    #[test]
    fn a_request_summary_names_the_keys_its_route_used() {
        let profile_id = ModelProviderProfileId::new();
        let primary = RequestId::new();
        let backup = RequestId::new();
        let credential = |id: RequestId, name: &str| crate::ModelProviderCredentialReference {
            id,
            display_name: name.to_string(),
            masked_hint: String::new(),
            status: crate::AgentCredentialStatus::Ready,
            credential: crate::AgentCredential::Local { runtime: None },
            revision: 1,

            models: Vec::new(),
            proxy_url: None,
        };
        let profile = ModelProviderProfile {
            id: profile_id.clone(),
            display_name: "DeepSeek".to_string(),
            credentials: vec![
                credential(primary.clone(), "Primary"),
                credential(backup.clone(), "Backup"),
            ],
            ..ModelProviderProfile::default()
        };
        let log = |route: Option<GatewayRouteTrace>| ModelGatewayRequestLog {
            request_id: RequestId::new(),
            agent_id: None,
            model_provider_profile_id: profile_id.clone(),
            credential_id: Some(backup.clone()),
            model_requested: "deepseek/deepseek-chat".to_string(),
            model_served: None,
            wire_in: "openai_chat_completions".to_string(),
            wire_out: "openai_chat_completions".to_string(),
            status: Some(200),
            input_tokens: 0,
            output_tokens: 0,
            cached_read_tokens: 0,
            cached_write_tokens: 0,
            reasoning_tokens: 0,
            latency_ms: Some(0),
            outcome: GatewayRequestOutcome::Ok,
            route,
            created_at_ms: 0,
        };

        let unremarkable = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace::new(0, 0))),
            std::slice::from_ref(&profile),
        );
        assert_eq!(unremarkable.provider_name, "DeepSeek");
        assert_eq!(unremarkable.credential_name.as_deref(), Some("Backup"));
        assert!(unremarkable.route.is_none());

        let failover = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace {
                failed_attempts: vec![GatewayRouteAttempt {
                    credential_id: primary.clone(),
                    failure: GatewayFailureKind::Quota,
                }],
                ..GatewayRouteTrace::new(1, 0)
            })),
            std::slice::from_ref(&profile),
        );
        let route = failover.route.expect("a failover is worth reporting");
        assert_eq!(route.skipped_cooling, 1);
        assert_eq!(route.failed_attempts.len(), 1);
        assert_eq!(
            route.failed_attempts[0].credential_name.as_deref(),
            Some("Primary")
        );
        assert_eq!(route.failed_attempts[0].failure, GatewayFailureKind::Quota);

        // A Key removed since the request keeps its id and loses only its name,
        // and a row whose Provider is gone keeps its own provider id.
        let gone = GatewayRequestSummary::from_log(
            log(Some(GatewayRouteTrace {
                failed_attempts: vec![GatewayRouteAttempt {
                    credential_id: RequestId::new(),
                    failure: GatewayFailureKind::Rate,
                }],
                ..GatewayRouteTrace::new(0, 0)
            })),
            &[],
        );
        assert!(gone.provider_name.starts_with("model_provider_"));
        assert!(gone.credential_name.is_none());
        assert!(
            gone.route
                .expect("the route survives a removed Key")
                .failed_attempts[0]
                .credential_name
                .is_none()
        );
    }

    #[test]
    fn routing_strategies_round_trip_by_id() {
        for strategy in GatewayRoutingStrategy::ALL {
            assert_eq!(
                GatewayRoutingStrategy::from_id(strategy.id()),
                Some(strategy)
            );
        }
        assert_eq!(GatewayRoutingStrategy::from_id("smart"), None);
        assert_eq!(GatewayRoutingStrategy::from_id("pace"), None);
    }

    #[test]
    fn settings_reject_privileged_ports() {
        let mut settings = ModelGatewaySettings {
            port: 80,
            ..ModelGatewaySettings::default()
        };
        assert_eq!(
            settings.validate().unwrap_err().code,
            "model_gateway_port_invalid"
        );

        settings.port = 3425;
        settings.validate().unwrap();
    }

    /// The gateway holds no upstream list, so its settings cannot name a
    /// Provider. This is what makes it independent of whether any route exists.
    #[test]
    fn settings_carry_no_provider_list() {
        let encoded = serde_json::to_string(&ModelGatewaySettings::default()).unwrap();
        assert!(!encoded.contains("upstream"), "{encoded}");
    }

    #[test]
    fn token_limit_requires_a_positive_ceiling() {
        let limit = GatewayTokenLimit {
            window: GatewayTokenWindow::Day,
            tokens: 0,
        };
        assert_eq!(
            limit.validate().unwrap_err().code,
            "model_gateway_token_limit_invalid"
        );
        GatewayTokenLimit {
            window: GatewayTokenWindow::Day,
            tokens: 1,
        }
        .validate()
        .unwrap();
    }

    #[test]
    fn gateway_profile_hides_computed_fields() {
        assert_eq!(
            profile_field_visibility(true, "endpoints"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "credentials"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "configuredModels"),
            ProfileFieldVisibility::Hidden
        );
        assert_eq!(
            profile_field_visibility(true, "routing"),
            ProfileFieldVisibility::Editable
        );
        assert_eq!(
            profile_field_visibility(false, "endpoints"),
            ProfileFieldVisibility::Editable
        );
    }

    #[test]
    fn model_ref_only_splits_a_real_provider_prefix() {
        assert_eq!(
            GatewayModelRef::parse("deepseek/deepseek-chat"),
            GatewayModelRef {
                provider: Some("deepseek"),
                model: "deepseek-chat"
            }
        );
        assert_eq!(
            GatewayModelRef::parse("deepseek-chat"),
            GatewayModelRef {
                provider: None,
                model: "deepseek-chat"
            }
        );
        assert_eq!(
            GatewayModelRef::parse("open-r1/"),
            GatewayModelRef {
                provider: None,
                model: "open-r1/"
            }
        );
    }

    #[test]
    fn failure_kinds_classify_cooldowns() {
        assert_eq!(
            GatewayFailureKind::Credit.cooldown_ms(),
            Some(30 * 60 * 1000)
        );
        assert_eq!(GatewayFailureKind::Auth.cooldown_ms(), None);
        // An auth failure invalidates this Key, but the next one may work.
        assert!(GatewayFailureKind::Auth.should_fail_over());
        assert!(GatewayFailureKind::Rate.should_fail_over());
        assert_eq!(
            GatewayKeyHealth::from_failure(GatewayFailureKind::Auth, 5),
            GatewayKeyHealth::Invalid
        );
        assert_eq!(
            GatewayKeyHealth::from_failure(GatewayFailureKind::Proxy, 5),
            GatewayKeyHealth::Cooling {
                kind: GatewayFailureKind::Proxy,
                until_ms: 5 + 60_000
            }
        );
    }
}

#[cfg(test)]
mod gateway_defaults_tests {
    use super::*;

    #[test]
    fn alert_thresholds_and_default_timeouts_round_trip() {
        let policy = GatewayPolicy {
            error_rate_warn_percent: Some(25),
            latency_warn_ms: Some(2_500),
            default_connect_timeout_ms: Some(10_000),
            default_request_timeout_ms: Some(60_000),
            default_stream_idle_timeout_ms: Some(30_000),
            ..GatewayPolicy::default()
        };
        policy.validate().unwrap();
        let encoded = serde_json::to_string(&policy).unwrap();
        let decoded: GatewayPolicy = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, policy);
    }

    #[test]
    fn invalid_thresholds_are_rejected() {
        assert!(
            GatewayPolicy {
                error_rate_warn_percent: Some(101),
                ..GatewayPolicy::default()
            }
            .validate()
            .is_err()
        );
        assert!(
            GatewayPolicy {
                default_request_timeout_ms: Some(0),
                ..GatewayPolicy::default()
            }
            .validate()
            .is_err()
        );
    }

    #[test]
    fn cooldown_overrides_accept_only_cooling_kinds_and_sane_durations() {
        let mut policy = GatewayPolicy::default();
        policy
            .cooldown_overrides_ms
            .insert("upstream".to_string(), 2_000);
        policy.validate().unwrap();
        policy
            .cooldown_overrides_ms
            .insert("auth".to_string(), 2_000);
        assert_eq!(
            policy.validate().unwrap_err().code,
            "model_gateway_cooldown_kind_invalid"
        );
        policy.cooldown_overrides_ms.remove("auth");
        policy
            .cooldown_overrides_ms
            .insert("made-up".to_string(), 2_000);
        assert_eq!(
            policy.validate().unwrap_err().code,
            "model_gateway_cooldown_kind_invalid"
        );
        policy.cooldown_overrides_ms.remove("made-up");
        policy.cooldown_overrides_ms.insert("rate".to_string(), 0);
        assert_eq!(
            policy.validate().unwrap_err().code,
            "model_gateway_cooldown_invalid"
        );
    }

    #[test]
    fn a_running_gateway_records_when_it_became_ready() {
        let running = ModelGatewayRuntimeState::running(3425, 1);
        assert!(running.started_at_ms.is_some());
        assert_eq!(running.started_at_ms, Some(running.updated_at_ms));
        let unavailable = ModelGatewayRuntimeState::unavailable("not running");
        assert_eq!(unavailable.started_at_ms, None);
    }
}

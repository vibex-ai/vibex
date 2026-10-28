//! Reusable model routes.
//!
//! A route is the unit a session picks: the `ModelProviderProfile` and its
//! `AgentModelProviderBinding` say what a Provider *is* and which Agent reaches
//! it, while a route says *how* a request reaches a model — which sources are
//! tried, in what order, and under which policy.
//!
//! Two shapes exist:
//!
//! - [`RouteKind::ModelGroup`] — the route composes individual models, each a
//!   `(Provider, upstream model)` pair, behind one Agent-facing model id. The
//!   members may be different upstream models, which is what makes "try the
//!   same model at three vendors, then a cheaper fallback" expressible. A
//!   group is the gateway's only input: the gateway owns no Provider list of
//!   its own.
//! - [`RouteKind::Direct`] — the route names exactly one Provider and skips the
//!   gateway entirely. Its `base_url` is the Provider's, not the loopback one.
//! - An account (`RuntimeAuthSource::AgentAccount`) is *not* a route. Its
//!   destination is an `AgentAuthContext`, it is one per Agent, and it owns no
//!   members, so modelling it as a record would give `members` a meaning that
//!   changes by type. The route graph renders it as a node, not a row.
//!
//! Routes are global assets: they carry no `agent_id`. An Agent references one
//! through [`AgentModelRouteBinding`], the same way it references a shared
//! Provider through its binding. One route can therefore serve several Agents,
//! and editing its order reaches all of them at once.

use serde::{Deserialize, Serialize};

use crate::{
    AgentId, ModelProviderProfileId, ModelRouteId, ModelRouteMemberId, VibexError, VibexResult,
};

/// The most members one route may hold.
///
/// Sixteen is enough for "primary plus a failover per Provider" without turning
/// the route graph into a wall of curves.
pub const MAX_ROUTE_MEMBERS: usize = 16;

/// The most routes a user may keep.
///
/// This bounds the route list rather than the graph: a route is a saved
/// composition, and past this many the picker stops being a picker.
///
/// It counts the routes a user composed — model groups. A `Direct` route is
/// materialised from a Provider Profile by
/// `backfill_direct_routes`, one per direct Profile already on the installation,
/// so counting those would let a long Provider list exhaust the budget before a
/// single route was authored. See `ModelRouteRepository::count_user_authored`.
pub const MAX_ROUTES: usize = 32;

/// The prefix a group's model ids carry.
///
/// Every model the gateway serves is addressed through a group, so this is the
/// only prefix a request against the gateway may carry.
pub const ROUTE_GROUP_MODEL_PREFIX: &str = "group/";

/// How a route delivers a model to an Agent.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteKind {
    /// Served by the local gateway, composed from individual models.
    ///
    /// Each member is a `(Provider, upstream model)` pair and the route exposes
    /// one Agent-facing model id, [`ModelRoute::agent_model_id`]. The members
    /// may name different upstream models, so a route can express "the same
    /// model at two vendors, then a cheaper fallback" without requiring their
    /// catalogues to intersect.
    #[default]
    ModelGroup,
    /// One Provider, called directly. The gateway is not involved.
    Direct,
}

impl RouteKind {
    pub const ALL: [Self; 2] = [Self::ModelGroup, Self::Direct];

    pub const fn id(self) -> &'static str {
        match self {
            Self::ModelGroup => "model_group",
            Self::Direct => "direct",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        match value.trim() {
            // `provider_group` is the retired shape. A database written before
            // it was removed still reads as the group it now is; the migration
            // gives that row a model and an Agent-facing id.
            "provider_group" | "model_group" => Some(Self::ModelGroup),
            "direct" => Some(Self::Direct),
            _ => None,
        }
    }

    /// Whether the local gateway serves this kind.
    ///
    /// A direct route is the one shape that never reaches the listener.
    pub const fn via_gateway(self) -> bool {
        matches!(self, Self::ModelGroup)
    }

    /// Direct routes carry exactly one member; gateway routes may carry more.
    pub const fn member_limit(self) -> usize {
        match self {
            Self::ModelGroup => MAX_ROUTE_MEMBERS,
            Self::Direct => 1,
        }
    }
}

/// How the members of a gateway route are tried.
///
/// Every strategy is computable from data the gateway already holds: local
/// health and cooldowns, observed latency and token usage, configured weights,
/// and the attempt order. None of them needs a vendor quota, balance, or price,
/// which is why a quota-aware strategy is deliberately absent rather than
/// approximated.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStrategy {
    /// The first member takes every request until it cannot answer.
    #[default]
    Ordered,
    /// Each request moves to the next member.
    RoundRobin,
    /// The member with the fewest tokens in the window goes first.
    LeastUsed,
    /// Healthy members first, then recently faster ones, then the least busy.
    ///
    /// "Smart" is a statement about this ordering, not a promise of a better
    /// model: it never inspects the prompt, cost, or a vendor's allowance.
    Smart,
    /// The member with the lowest observed p50 first, then p95, then order.
    LowestLatency,
    /// Smooth weighted round-robin over the members' configured weights.
    Weighted,
    /// Only the member the user picked, with no rotation.
    Manual,
}

impl RouteStrategy {
    pub const ALL: [Self; 7] = [
        Self::Ordered,
        Self::RoundRobin,
        Self::LeastUsed,
        Self::Smart,
        Self::LowestLatency,
        Self::Weighted,
        Self::Manual,
    ];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Ordered => "ordered",
            Self::RoundRobin => "round_robin",
            Self::LeastUsed => "least_used",
            Self::Smart => "smart",
            Self::LowestLatency => "lowest_latency",
            Self::Weighted => "weighted",
            Self::Manual => "manual",
        }
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::Ordered => "In order",
            Self::RoundRobin => "Rotate",
            Self::LeastUsed => "Least used",
            Self::Smart => "Smart",
            Self::LowestLatency => "Lowest latency",
            Self::Weighted => "By weight",
            Self::Manual => "Manual",
        }
    }

    /// One sentence describing when the strategy is the right choice.
    ///
    /// The UI uses this so a strategy picker explains itself instead of offering
    /// "Smart" with no definition.
    pub const fn guidance(self) -> &'static str {
        match self {
            Self::Ordered => "Try members in the order you arranged them.",
            Self::RoundRobin => "Spread requests across members turn by turn.",
            Self::LeastUsed => "Prefer the member that has carried the fewest tokens.",
            Self::Smart => {
                "Prefer healthy members, then the recently fastest, then the least busy."
            }
            Self::LowestLatency => "Prefer the member with the lowest observed latency.",
            Self::Weighted => "Share requests by each member's weight.",
            Self::Manual => "Always use the one member you pick.",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|item| item.id() == value.trim())
    }
}

/// One member: a model at one Provider, in the order it is tried.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteMember {
    pub id: ModelRouteMemberId,
    pub model_provider_profile_id: ModelProviderProfileId,
    /// The model id the Provider serves.
    pub model_id: String,
    /// Whether this member may serve a request.
    ///
    /// Disabling keeps the member's place in the order, so trying a route
    /// without one of its sources does not mean removing and re-adding it.
    pub enabled: bool,
    /// Position in the attempt order. The route's members are always stored
    /// sorted by this value.
    pub order_index: i64,
    /// Reasoning effort to ask for, overriding what the Agent requested.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<String>,
    /// Relative share of requests under [`RouteStrategy::Weighted`].
    ///
    /// One means "the same as every other unweighted member". The value is kept
    /// under the other strategies so switching strategy does not lose it.
    #[serde(default = "default_member_weight")]
    pub weight: u32,
    /// An explicit context window for this member, overriding the route's.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// How long this member may take to produce its first token, overriding the
    /// route's value.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_timeout_ms: Option<u64>,
}

/// The weight a member carries unless the user changes it.
pub const DEFAULT_MEMBER_WEIGHT: u32 = 1;
/// The largest weight a member may carry.
pub const MAX_MEMBER_WEIGHT: u32 = 100;

const fn default_member_weight() -> u32 {
    DEFAULT_MEMBER_WEIGHT
}

impl ModelRouteMember {
    pub fn new(
        model_provider_profile_id: ModelProviderProfileId,
        model_id: impl Into<String>,
        order_index: i64,
    ) -> Self {
        Self {
            id: ModelRouteMemberId::new(),
            model_provider_profile_id,
            model_id: model_id.into(),
            enabled: true,
            order_index,
            reasoning_effort: None,
            weight: DEFAULT_MEMBER_WEIGHT,
            context_tokens: None,
            first_token_timeout_ms: None,
        }
    }

    /// One Provider as a member of a provider group.
    ///
    /// A group names Providers, not models: its model list is the intersection
    /// of their catalogues, so the member carries an empty model id and the
    /// provider is the whole address.
    pub fn provider(model_provider_profile_id: ModelProviderProfileId, order_index: i64) -> Self {
        Self::new(model_provider_profile_id, "", order_index)
    }

    /// Whether this member names a model rather than only a Provider.
    pub fn names_model(&self) -> bool {
        !self.model_id.trim().is_empty()
    }

    pub fn validate(&self) -> VibexResult<()> {
        if self.order_index < 0 {
            return Err(VibexError::validation(
                "route_member_order_invalid",
                "a route member order must not be negative",
            ));
        }
        if self.weight == 0 || self.weight > MAX_MEMBER_WEIGHT {
            return Err(VibexError::validation(
                "route_member_weight_invalid",
                "a route member weight must be between one and one hundred",
            ));
        }
        if self.context_tokens == Some(0) {
            return Err(VibexError::validation(
                "route_member_context_invalid",
                "a route member context window must be greater than zero",
            ));
        }
        if self.first_token_timeout_ms == Some(0) {
            return Err(VibexError::validation(
                "route_member_first_token_timeout_invalid",
                "a route member first-token timeout must be greater than zero",
            ));
        }
        if let Some(effort) = self.reasoning_effort.as_deref() {
            let effort = effort.trim();
            if effort.is_empty() || effort.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_member_effort_invalid",
                    "a route member reasoning effort must be non-empty and bounded",
                ));
            }
        }
        Ok(())
    }

    /// The model checks that only apply to a member that names one.
    fn validate_model(&self) -> VibexResult<()> {
        let model = self.model_id.trim();
        if model.is_empty() {
            return Err(VibexError::validation(
                "route_member_model_invalid",
                "a route member must name a model",
            ));
        }
        if model.chars().count() > 256 {
            return Err(VibexError::validation(
                "route_member_model_invalid",
                "a route member model id is longer than the supported length",
            ));
        }
        Ok(())
    }
}

/// The largest number of levels a route may declare.
///
/// A level is a named variant of the same route (for example `fast` and
/// `deep`); past a handful the picker is harder to read than the model list it
/// replaced.
pub const MAX_ROUTE_LEVELS: usize = 8;

/// The longest a route may hold a request open as it retries after every member
/// failed.
///
/// Four minutes is long enough to ride out a vendor's brief outage and short
/// enough that an Agent's turn does not appear hung forever. `None` disables the
/// behaviour, which is the default for existing routes.
pub const MAX_ROUTE_PATIENCE_MS: u64 = 240_000;

/// The patience a new model group starts with.
pub const DEFAULT_ROUTE_PATIENCE_MS: u64 = 60_000;

/// A window of the week during which a rule applies.
///
/// Minutes are counted from local midnight (`0..=1439`). `start_minute >
/// end_minute` is a window that crosses midnight, which is the normal shape for
/// "overnight". An absent `zone` means the machine's local time.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RouteTimeWindow {
    /// ISO weekdays, Monday (`1`) through Sunday (`7`).
    #[serde(default)]
    pub days: Vec<u8>,
    #[serde(default)]
    pub start_minute: u16,
    #[serde(default)]
    pub end_minute: u16,
    /// An IANA zone name. `None` means local time.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub zone: Option<String>,
}

impl RouteTimeWindow {
    /// Validates a window without consulting a clock or a timezone database.
    pub fn validate(&self) -> VibexResult<()> {
        if self.days.is_empty() {
            return Err(VibexError::validation(
                "route_rule_time_days_missing",
                "a time window must name at least one weekday",
            ));
        }
        let mut seen = Vec::new();
        for day in &self.days {
            if !(1..=7).contains(day) {
                return Err(VibexError::validation(
                    "route_rule_time_day_invalid",
                    "a weekday must be between Monday (1) and Sunday (7)",
                ));
            }
            if seen.contains(day) {
                return Err(VibexError::validation(
                    "route_rule_time_day_duplicate",
                    "a time window lists the same weekday twice",
                ));
            }
            seen.push(*day);
        }
        if self.start_minute >= 24 * 60 || self.end_minute >= 24 * 60 {
            return Err(VibexError::validation(
                "route_rule_time_minute_invalid",
                "a time window must stay within one day",
            ));
        }
        if self.start_minute == self.end_minute {
            return Err(VibexError::validation(
                "route_rule_time_empty",
                "a time window must start and end at different minutes",
            ));
        }
        if let Some(zone) = self.zone.as_deref() {
            let zone = zone.trim();
            if zone.is_empty() || zone.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_rule_time_zone_invalid",
                    "a timezone name must be non-empty and bounded",
                ));
            }
        }
        Ok(())
    }

    /// Whether `minute` (from midnight) falls inside the window on `day`.
    ///
    /// The caller supplies the local weekday and minute, so this stays a pure
    /// intersection test and the gateway owns the clock read.
    pub fn contains(&self, day: u8, minute: u16) -> bool {
        if !self.days.contains(&day) {
            return false;
        }
        if self.start_minute <= self.end_minute {
            minute >= self.start_minute && minute < self.end_minute
        } else {
            // The window runs past midnight: the tail belongs to the same
            // weekday the rule named, which is how "Friday 22:00–02:00" reads.
            minute >= self.start_minute || minute < self.end_minute
        }
    }
}

/// What a rule does with the members it names.
///
/// The order is deliberate: the starter is tried first, `more` afterwards in
/// their listed order, and any remaining enabled member follows the route's
/// strategy. Only a `Pause` rule stops routing instead of choosing.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteRule {
    /// Stable identity. Renaming Providers or reordering members never
    /// invalidates a rule.
    pub id: String,
    /// The member this rule tries first. Absent on a pause rule, which chooses
    /// nothing.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_member_id: Option<ModelRouteMemberId>,
    /// Members to try after the starter, in order.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub more_member_ids: Vec<ModelRouteMemberId>,
    /// An effort to ask for when this rule matches.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub use_effort: Option<String>,
    /// Match when the request's estimated context is at least this many tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens_min: Option<u64>,
    /// Match when the request does (`true`) or does not (`false`) carry images.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub images: Option<bool>,
    /// Match the effort the Agent asked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// Match these Agent ids. Empty means any Agent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub agents: Vec<String>,
    /// Match this classifier intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub intent: Option<String>,
    /// Match when the turn is (`true`) or is not (`false`) a compaction.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact: Option<bool>,
    /// Match this window of the week.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub time: Option<RouteTimeWindow>,
    /// Match when the workspace root contains this path fragment. Empty means
    /// any folder.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub folders: Vec<String>,
    /// Veto the rule when the workspace root contains one of these fragments.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub except_folders: Vec<String>,
    /// Stop routing for this turn instead of choosing a member.
    #[serde(default)]
    pub pause: bool,
}

impl ModelRouteRule {
    /// Whether the rule names at least one match condition.
    pub fn has_condition(&self) -> bool {
        self.tokens_min.is_some()
            || self.images.is_some()
            || self.effort.is_some()
            || !self.agents.is_empty()
            || self.intent.is_some()
            || self.compact.is_some()
            || self.time.is_some()
            || !self.folders.is_empty()
            || !self.except_folders.is_empty()
    }

    /// Whether the rule names an action that selects a member.
    pub fn selects_member(&self) -> bool {
        self.use_member_id.is_some()
    }

    fn validate(&self, member_ids: &[&str]) -> VibexResult<()> {
        let id = self.id.trim();
        if id.is_empty() || id.chars().count() > 128 {
            return Err(VibexError::validation(
                "route_rule_id_invalid",
                "a rule id must be non-empty and bounded",
            ));
        }
        if !self.has_condition() {
            return Err(VibexError::validation(
                "route_rule_condition_missing",
                "a rule must name at least one match condition",
            ));
        }
        if let Some(window) = &self.time {
            window.validate()?;
        }
        if let Some(member) = &self.use_member_id
            && !member_ids.contains(&member.as_str())
        {
            return Err(VibexError::validation(
                "route_rule_member_unknown",
                "a rule names a member that is not part of the route",
            ));
        }
        let mut seen = Vec::new();
        for member in &self.more_member_ids {
            if !member_ids.contains(&member.as_str()) {
                return Err(VibexError::validation(
                    "route_rule_member_unknown",
                    "a rule names a member that is not part of the route",
                ));
            }
            if self.use_member_id.as_ref() == Some(member) || seen.contains(&member.as_str()) {
                return Err(VibexError::validation(
                    "route_rule_member_duplicate",
                    "a rule lists the same member twice",
                ));
            }
            seen.push(member.as_str());
        }
        if let Some(effort) = self.use_effort.as_deref() {
            let effort = effort.trim();
            if effort.is_empty() || effort.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_rule_effort_invalid",
                    "a rule effort must be non-empty and bounded",
                ));
            }
        }
        if self.pause {
            // A pause rule exists to stop routing in a narrow circumstance.
            // Letting it also name a member made the two actions ambiguous.
            if self.selects_member()
                || !self.more_member_ids.is_empty()
                || self.use_effort.is_some()
            {
                return Err(VibexError::validation(
                    "route_rule_pause_action_invalid",
                    "a pause rule cannot choose a member or an effort",
                ));
            }
        } else {
            if !self.selects_member() {
                return Err(VibexError::validation(
                    "route_rule_member_missing",
                    "a rule must name the member it tries first",
                ));
            }
            if self.intent.is_some() && self.intent.as_deref().is_some_and(|i| i.trim().is_empty())
            {
                return Err(VibexError::validation(
                    "route_rule_intent_invalid",
                    "a rule intent must be non-empty when present",
                ));
            }
        }
        for agent in &self.agents {
            if agent.trim().is_empty() || agent.chars().count() > 128 {
                return Err(VibexError::validation(
                    "route_rule_agent_invalid",
                    "a rule Agent id must be non-empty and bounded",
                ));
            }
        }
        for folder in self.folders.iter().chain(&self.except_folders) {
            if folder.trim().is_empty() || folder.chars().count() > 512 {
                return Err(VibexError::validation(
                    "route_rule_folder_invalid",
                    "a rule folder must be non-empty and bounded",
                ));
            }
        }
        Ok(())
    }
}

/// Everything a rule may be matched against for one request.
///
/// The caller supplies the clock reading and the workspace root, because those
/// are facts about the request, not about the rule. Keeping them here rather
/// than reading a clock inside the matcher is what lets the simulator and the
/// gateway reach the same answer.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteRuleContext {
    pub agent_id: Option<String>,
    /// An estimate of the turn's context, in tokens.
    pub context_tokens: Option<u64>,
    pub has_images: bool,
    pub requested_effort: Option<String>,
    pub compact: bool,
    pub intent: Option<String>,
    /// The workspace root, when the caller has one.
    pub workspace_root: Option<String>,
    /// Local ISO weekday, `1` (Monday) through `7` (Sunday).
    pub local_weekday: Option<u8>,
    /// Local minute from midnight.
    pub local_minute: Option<u16>,
}

/// The first rule that matches a request, with its position.
///
/// Position zero is reported as `Some(0)`, so a caller cannot confuse "the
/// first rule matched" with "no rule matched".
pub fn first_matching_rule<'a>(
    rules: &'a [ModelRouteRule],
    context: &RouteRuleContext,
) -> Option<(usize, &'a ModelRouteRule)> {
    rules
        .iter()
        .enumerate()
        .find(|(_, rule)| rule_matches(rule, context))
}

/// Whether one rule matches.
///
/// Every condition present must hold; an absent condition is not a condition.
/// Folders compare as path fragments so a rule authored on one machine still
/// matches a project checked out elsewhere.
pub fn rule_matches(rule: &ModelRouteRule, context: &RouteRuleContext) -> bool {
    if let Some(agents) = (!rule.agents.is_empty()).then_some(&rule.agents) {
        let Some(agent_id) = context.agent_id.as_deref() else {
            return false;
        };
        if !agents.iter().any(|agent| agent.trim() == agent_id) {
            return false;
        }
    }
    if let Some(minimum) = rule.tokens_min
        && context.context_tokens.is_none_or(|tokens| tokens < minimum)
    {
        return false;
    }
    if let Some(images) = rule.images
        && images != context.has_images
    {
        return false;
    }
    if let Some(effort) = rule.effort.as_deref() {
        let Some(requested) = context.requested_effort.as_deref() else {
            return false;
        };
        if !requested.eq_ignore_ascii_case(effort.trim()) {
            return false;
        }
    }
    if let Some(compact) = rule.compact
        && compact != context.compact
    {
        return false;
    }
    if let Some(intent) = rule.intent.as_deref() {
        let Some(actual) = context.intent.as_deref() else {
            return false;
        };
        if !actual.eq_ignore_ascii_case(intent.trim()) {
            return false;
        }
    }
    if let Some(window) = &rule.time {
        let (Some(day), Some(minute)) = (context.local_weekday, context.local_minute) else {
            return false;
        };
        // A zone other than the machine's local zone is stored for a later
        // evaluator. Treating it as local would be a silent wrong answer, so
        // the rule simply does not match.
        if let Some(zone) = window.zone.as_deref()
            && !zone.trim().eq_ignore_ascii_case("local")
        {
            return false;
        }
        if !window.contains(day, minute) {
            return false;
        }
    }
    if !rule.folders.is_empty() {
        let Some(root) = context.workspace_root.as_deref() else {
            return false;
        };
        if !rule
            .folders
            .iter()
            .any(|fragment| root.contains(fragment.trim()))
        {
            return false;
        }
    }
    if rule.except_folders.iter().any(|fragment| {
        context
            .workspace_root
            .as_deref()
            .is_some_and(|root| root.contains(fragment.trim()))
    }) {
        return false;
    }
    true
}

/// What the simulator knows about one member.
///
/// The gateway supplies the same facts from its live state; the builder
/// supplies them from recent ledger rows. Keeping the shape here lets the
/// simulator and the router answer the same question the same way.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RouteMemberObservation {
    pub member_id: Option<ModelRouteMemberId>,
    pub used_tokens: u64,
    pub p50_ms: Option<u64>,
    pub p95_ms: Option<u64>,
    pub cooling: bool,
    pub inflight: u32,
}

/// The result of simulating one request against a route.
///
/// This is a decision, never a call: it reads the route, the request context,
/// and the observations supplied by the caller. Nothing here sends a request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RouteSimulation {
    /// The enabled members in the order the router would try them.
    pub order: Vec<ModelRouteMember>,
    /// The matching rule, when one applies.
    pub matched_rule_index: Option<usize>,
    pub matched_rule_id: Option<String>,
    /// A matching pause rule stops before any member.
    pub paused: bool,
    /// A p50 estimate taken from the first candidate's history, not a promise.
    pub estimated_latency_ms: Option<u64>,
    /// Members the simulation removed because the caller asked to fail them.
    pub failed_member_skipped: bool,
    /// Enabled members whose Keys are currently cooling.
    pub cooling_members: Vec<ModelRouteMemberId>,
}

impl RouteSimulation {
    pub fn is_empty(&self) -> bool {
        self.order.is_empty()
    }
}

/// Simulates the member order for one request without sending anything.
///
/// `rotation` is the same per-route counter the router uses for rotation and
/// weighted choice. `failed_member` lets a builder ask "what happens if this
/// one is down?" and see the fallback, which is the question a real outage
/// answers too late.
pub fn simulate_model_route(
    route: &ModelRoute,
    context: &RouteRuleContext,
    rotation: u64,
    observations: &[RouteMemberObservation],
    failed_member: Option<&ModelRouteMemberId>,
) -> RouteSimulation {
    let mut result = RouteSimulation {
        order: Vec::new(),
        matched_rule_index: None,
        matched_rule_id: None,
        paused: false,
        estimated_latency_ms: None,
        failed_member_skipped: false,
        cooling_members: Vec::new(),
    };
    if !route.is_model_group() {
        return result;
    }
    let observation = |member: &ModelRouteMember| {
        observations
            .iter()
            .find(|observation| {
                observation.member_id.as_ref() == Some(&member.id)
                    || observation.member_id.is_none()
            })
            .cloned()
            .unwrap_or_default()
    };
    let mut order = route
        .members
        .iter()
        .filter(|member| member.enabled)
        .cloned()
        .collect::<Vec<_>>();
    if let Some((index, rule)) = first_matching_rule(&route.rules, context) {
        result.matched_rule_index = Some(index);
        result.matched_rule_id = Some(rule.id.clone());
        if rule.pause {
            result.paused = true;
            return result;
        }
        let mut wanted = Vec::new();
        if let Some(member) = &rule.use_member_id {
            wanted.push(member.clone());
        }
        wanted.extend(rule.more_member_ids.iter().cloned());
        let mut reordered = Vec::with_capacity(order.len());
        for member_id in &wanted {
            if let Some(position) = order.iter().position(|member| &member.id == member_id)
                && let Some(member) = order.get(position).cloned()
                && !reordered.contains(&member)
            {
                reordered.push(member);
            }
        }
        for member in &order {
            if !reordered.contains(member) {
                reordered.push(member.clone());
            }
        }
        order = reordered;
    } else {
        order = strategy_order(route, order, rotation, &observation);
    }
    if let Some(failed) = failed_member
        && let Some(position) = order.iter().position(|member| &member.id == failed)
    {
        order.remove(position);
        result.failed_member_skipped = true;
    }
    result.cooling_members = order
        .iter()
        .filter(|member| observation(member).cooling)
        .map(|member| member.id.clone())
        .collect();
    result.estimated_latency_ms = order.first().and_then(|member| observation(member).p50_ms);
    result.order = order;
    result
}

fn strategy_order(
    route: &ModelRoute,
    mut order: Vec<ModelRouteMember>,
    rotation: u64,
    observation: &impl Fn(&ModelRouteMember) -> RouteMemberObservation,
) -> Vec<ModelRouteMember> {
    match route.strategy {
        RouteStrategy::Ordered => {}
        RouteStrategy::RoundRobin => {
            if !order.is_empty() {
                let offset = (rotation as usize) % order.len();
                order.rotate_left(offset);
            }
        }
        RouteStrategy::LeastUsed => {
            order.sort_by_key(|member| observation(member).used_tokens);
        }
        RouteStrategy::Smart => {
            order.sort_by_key(|member| {
                let observed = observation(member);
                (
                    observed.cooling,
                    observed.p50_ms.unwrap_or(u64::MAX),
                    observed.inflight,
                    observed.used_tokens,
                )
            });
        }
        RouteStrategy::LowestLatency => {
            order.sort_by_key(|member| {
                let observed = observation(member);
                (
                    observed.cooling,
                    observed.p50_ms.unwrap_or(u64::MAX),
                    observed.p95_ms.unwrap_or(u64::MAX),
                )
            });
        }
        RouteStrategy::Weighted => {
            order = simulate_weighted_order(&order, rotation);
        }
        RouteStrategy::Manual => {
            if let Some(picked) = &route.pick_member_id {
                order.retain(|member| &member.id == picked);
            } else {
                order.truncate(1);
            }
        }
    }
    order
}

fn simulate_weighted_order(members: &[ModelRouteMember], rotation: u64) -> Vec<ModelRouteMember> {
    let weights = members
        .iter()
        .map(|member| member.weight.max(DEFAULT_MEMBER_WEIGHT) as usize)
        .collect::<Vec<_>>();
    let total = weights.iter().sum::<usize>();
    if total == 0 {
        return members.to_vec();
    }
    let max_weight = weights.iter().copied().max().unwrap_or(1);
    let mut cycle = Vec::with_capacity(total);
    for round in 0..max_weight {
        for (index, member) in members.iter().enumerate() {
            if round < weights[index] {
                cycle.push(member.clone());
            }
        }
    }
    let start = (rotation as usize) % cycle.len().max(1);
    let mut ordered = Vec::with_capacity(members.len());
    for member in cycle
        .into_iter()
        .cycle()
        .skip(start)
        .take(members.len() * 2)
    {
        if !ordered.contains(&member) {
            ordered.push(member);
        }
        if ordered.len() == members.len() {
            break;
        }
    }
    ordered
}

/// A reusable composition of model sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRoute {
    pub id: ModelRouteId,
    pub display_name: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub description: Option<String>,
    pub kind: RouteKind,
    /// Members in attempt order.
    pub members: Vec<ModelRouteMember>,
    pub strategy: RouteStrategy,
    /// The model id Agents ask for, on a [`RouteKind::ModelGroup`].
    ///
    /// A model group exposes one virtual model; its members may be different
    /// upstream models at different Providers. Provider groups leave this empty
    /// because each shared model is its own entry.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_model_id: Option<String>,
    /// Ordered rules that may override the member order for one turn.
    ///
    /// The first match wins. An empty list means the strategy always decides.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub rules: Vec<ModelRouteRule>,
    /// A member or `provider/model` used to classify the turn's intent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub classifier: Option<String>,
    /// `Some("auto")` asks the classifier to choose an effort level.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effort: Option<String>,
    /// How long a fully failed attempt is retried before the turn gives up.
    ///
    /// `None` disables patience. The value is bounded so a request cannot be
    /// held indefinitely.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub patience_ms: Option<u64>,
    /// Keep this route out of model pickers and address it only explicitly.
    #[serde(default)]
    pub sink: bool,
    /// Hide this route's models from Agent and gateway model lists.
    #[serde(default)]
    pub unlists: bool,
    /// Named variants of this route offered by the picker.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub levels: Vec<String>,
    /// The only member [`RouteStrategy::Manual`] may use.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pick_member_id: Option<ModelRouteMemberId>,
    /// How long a member may take to produce its first token before the next
    /// one is asked instead.
    ///
    /// This exists because a vendor can accept a connection and then stall: the
    /// request never fails, so no error-based failover ever fires, and the turn
    /// hangs. `None` waits as long as it takes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_timeout_ms: Option<u64>,
    /// An explicit context window, overriding the default "smallest member"
    /// rule. Only meaningful for a gateway route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_tokens: Option<u32>,
    /// Whether one conversation stays on one Key.
    ///
    /// Route-scoped rather than gateway-scoped: a group with several Keys per
    /// Provider may want to hold a conversation on one of them, while another
    /// group over the same Providers prefers to re-pick each request.
    ///
    /// Conversation affinity is the default because it is what preserves a
    /// vendor's prompt cache, and it is what the gateway did for every request
    /// before pinning became a property of the composition. A group that wants
    /// to spread load instead turns it off.
    #[serde(default)]
    pub pin: crate::GatewayPinScope,
    /// What happens when the member a conversation is pinned to fails.
    ///
    /// Route-scoped like `pin`, because the two are halves of one decision: a
    /// route that pins a conversation has to say how it lets go. The default is
    /// to switch and report it, which keeps a Provider outage from stranding a
    /// session; a route that mixes vendors can ask instead.
    #[serde(default)]
    pub failover: crate::GatewayFailover,
    /// A ceiling on the tokens this route may spend in its window.
    ///
    /// `None` means no route-level ceiling; per-Key limits still apply. A route
    /// that names several members needs its own ceiling because the per-Key ones
    /// do not compose: three Keys with a limit each still allow three times the
    /// spend.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token_limit: Option<crate::GatewayTokenLimit>,
    pub status: RouteStatus,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RouteStatus {
    Enabled,
    Disabled,
}

impl RouteStatus {
    pub const ALL: [Self; 2] = [Self::Enabled, Self::Disabled];

    pub const fn id(self) -> &'static str {
        match self {
            Self::Enabled => "enabled",
            Self::Disabled => "disabled",
        }
    }

    pub fn from_id(value: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|status| status.id() == value.trim())
    }
}

impl Default for ModelRoute {
    /// An empty, disabled route.
    ///
    /// This exists so a caller — a fixture, or a page that has not loaded yet —
    /// can name only the fields it cares about. It is never a valid record to
    /// persist: `validate` rejects an empty name and an empty member list.
    fn default() -> Self {
        Self {
            id: ModelRouteId::new(),
            display_name: String::new(),
            description: None,
            kind: RouteKind::ModelGroup,
            members: Vec::new(),
            strategy: RouteStrategy::Ordered,
            agent_model_id: None,
            rules: Vec::new(),
            classifier: None,
            effort: None,
            patience_ms: None,
            sink: false,
            unlists: false,
            levels: Vec::new(),
            pick_member_id: None,
            first_token_timeout_ms: None,
            context_tokens: None,
            pin: crate::GatewayPinScope::Conversation,
            failover: crate::GatewayFailover::Auto,
            token_limit: None,
            status: RouteStatus::Disabled,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

impl ModelRoute {
    pub const MAX_DISPLAY_NAME_LEN: usize = 160;

    /// The members that may actually serve a request, in attempt order.
    pub fn enabled_members(&self) -> impl Iterator<Item = &ModelRouteMember> {
        self.members.iter().filter(|member| member.enabled)
    }

    /// The models this route offers to an Agent, deduplicated but order-stable.
    ///
    /// A member is a `(Provider, model)` pair, so one model configured at three
    /// Providers appears three times in `members` and once here.
    ///
    /// A model group exposes exactly its Agent-facing id, not the upstream
    /// names of its members: those are the route's business, and several
    /// members may name different upstream models. A direct route has no
    /// Agent-facing id, so its member's model is the one it names.
    pub fn distinct_models(&self) -> Vec<&str> {
        if self.is_model_group() {
            return self.agent_model().into_iter().collect();
        }
        let mut seen = Vec::new();
        for member in self.enabled_members().filter(|member| member.names_model()) {
            if !seen.contains(&member.model_id.as_str()) {
                seen.push(member.model_id.as_str());
            }
        }
        seen
    }

    /// Whether this route composes individual models behind one virtual name.
    pub fn is_model_group(&self) -> bool {
        self.kind == RouteKind::ModelGroup
    }

    /// The Agent-facing model id of a model group, trimmed.
    pub fn agent_model(&self) -> Option<&str> {
        self.agent_model_id
            .as_deref()
            .map(str::trim)
            .filter(|model| !model.is_empty())
    }

    /// Makes the Agent-facing model id the group's own name.
    ///
    /// A model group is one model to the Agent, so the name the reader typed is
    /// also the model a request names. Writing through this method keeps the
    /// two from becoming a second, conflicting identity that a reader would
    /// have to fill in by hand.
    pub fn sync_agent_model_id(&mut self) {
        self.agent_model_id = self
            .is_model_group()
            .then(|| self.display_name.trim().to_string());
    }

    /// How many sources back this route.
    pub fn source_count(&self) -> usize {
        self.enabled_members().count()
    }

    /// The prefix every model of a gateway group is addressed by.
    ///
    /// A model group exposes one virtual model, so its callable id is the
    /// prefix plus [`ModelRoute::agent_model`]. The route id lives in the
    /// middle so two groups may publish the same model name without colliding.
    ///
    /// A direct route never reaches the gateway, so it has no gateway id and
    /// this prefix describes nothing callable; it exists for the group shape
    /// every gateway route now has.
    pub fn model_id(&self) -> String {
        format!("{}{}/", ROUTE_GROUP_MODEL_PREFIX, self.id.as_str())
    }

    /// The id one model of a group is selected by.
    pub fn group_model_id(&self, model_id: &str) -> String {
        format!("{}{}", self.model_id(), model_id.trim().trim_matches('/'))
    }

    /// The single id a model group is called by, if it has one.
    pub fn gateway_model_id(&self) -> Option<String> {
        self.agent_model().map(|model| self.group_model_id(model))
    }

    /// Whether a request for this id names this route's model group.
    ///
    /// The id is still `group/<route>/<model>`; the model half has to equal the
    /// route's Agent-facing id so a request cannot address a member's upstream
    /// name through the group.
    pub fn serves_model_group_id(&self, requested: &str) -> bool {
        let Some(parsed) = route_group_model_ref(requested) else {
            return false;
        };
        parsed.group_id == self.id.as_str()
            && self.is_model_group()
            && self.agent_model() == Some(parsed.model_id)
    }

    /// Sorts members into attempt order. Callers mutate freely; `validate`
    /// requires the order to be dense and unique.
    pub fn sort_members(&mut self) {
        self.members.sort_by(|left, right| {
            left.order_index
                .cmp(&right.order_index)
                .then_with(|| left.id.as_str().cmp(right.id.as_str()))
        });
    }

    pub fn validate(&self) -> VibexResult<()> {
        let name = self.display_name.trim();
        if name.is_empty() || name.chars().count() > Self::MAX_DISPLAY_NAME_LEN {
            return Err(VibexError::validation(
                "route_display_name_invalid",
                "a route name must be non-empty and within the supported length",
            ));
        }
        if self.members.is_empty() {
            return Err(VibexError::validation(
                "route_member_missing",
                "a route must name at least one model",
            ));
        }
        if self.members.len() > MAX_ROUTE_MEMBERS {
            return Err(VibexError::validation(
                "route_member_limit_exceeded",
                "the route has more members than it supports",
            ));
        }
        if self.kind == RouteKind::Direct && self.members.len() != 1 {
            return Err(VibexError::validation(
                "route_direct_member_count_invalid",
                "a direct route must name exactly one model",
            ));
        }
        let mut seen_ids = Vec::new();
        let mut seen_orders = Vec::new();
        for member in &self.members {
            member.validate()?;
            // Both remaining shapes name an upstream model on every member: a
            // model group picks `(Provider, model)` pairs, and a direct route
            // is one such pair.
            member.validate_model()?;
            if seen_ids.contains(&member.id.as_str()) {
                return Err(VibexError::validation(
                    "route_member_duplicate",
                    "a route contains the same member twice",
                ));
            }
            seen_ids.push(member.id.as_str());
            let order = usize::try_from(member.order_index).unwrap_or(usize::MAX);
            if seen_orders.contains(&order) {
                return Err(VibexError::validation(
                    "route_member_order_duplicate",
                    "two route members claim the same place in the order",
                ));
            }
            seen_orders.push(order);
        }
        if self.is_model_group() {
            self.validate_model_group()?;
        } else if self.agent_model_id.is_some() {
            return Err(VibexError::validation(
                "route_agent_model_unexpected",
                "only a model group has an Agent-facing model id",
            ));
        }
        if self.first_token_timeout_ms == Some(0) {
            return Err(VibexError::validation(
                "route_first_token_timeout_invalid",
                "a first-token timeout must be greater than zero",
            ));
        }
        if self.context_tokens == Some(0) {
            return Err(VibexError::validation(
                "route_context_invalid",
                "a route context window must be greater than zero",
            ));
        }
        if let Some(patience) = self.patience_ms
            && (patience == 0 || patience > MAX_ROUTE_PATIENCE_MS)
        {
            return Err(VibexError::validation(
                "route_patience_invalid",
                "route patience must be between one millisecond and four minutes",
            ));
        }
        if let Some(effort) = self.effort.as_deref() {
            let effort = effort.trim();
            if effort.is_empty() || effort.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_effort_invalid",
                    "a route effort must be non-empty and bounded",
                ));
            }
            if effort != "auto" {
                return Err(VibexError::validation(
                    "route_effort_invalid",
                    "a route effort is either absent or auto",
                ));
            }
        }
        if let Some(classifier) = self.classifier.as_deref() {
            let classifier = classifier.trim();
            if classifier.is_empty() || classifier.chars().count() > 256 {
                return Err(VibexError::validation(
                    "route_classifier_invalid",
                    "a route classifier must be non-empty and bounded",
                ));
            }
            if Some(classifier) == self.agent_model() {
                return Err(VibexError::validation(
                    "route_classifier_cycle",
                    "a route cannot classify itself",
                ));
            }
        }
        if self.levels.len() > MAX_ROUTE_LEVELS {
            return Err(VibexError::validation(
                "route_level_limit_exceeded",
                "the route has more levels than it supports",
            ));
        }
        let mut seen_levels = Vec::new();
        for level in &self.levels {
            let level = level.trim();
            if level.is_empty() || level.chars().count() > 64 {
                return Err(VibexError::validation(
                    "route_level_invalid",
                    "a route level must be non-empty and bounded",
                ));
            }
            if seen_levels.contains(&level) {
                return Err(VibexError::validation(
                    "route_level_duplicate",
                    "the route lists the same level twice",
                ));
            }
            seen_levels.push(level);
        }
        let member_ids = self
            .members
            .iter()
            .map(|member| member.id.as_str())
            .collect::<Vec<_>>();
        if let Some(pick) = &self.pick_member_id
            && !member_ids.contains(&pick.as_str())
        {
            return Err(VibexError::validation(
                "route_manual_member_unknown",
                "the manually picked member is not part of the route",
            ));
        }
        if self.strategy == RouteStrategy::Manual && self.pick_member_id.is_none() {
            return Err(VibexError::validation(
                "route_manual_member_missing",
                "a manual route must name the member it uses",
            ));
        }
        let mut seen_rule_ids = Vec::new();
        for rule in &self.rules {
            rule.validate(&member_ids)?;
            let rule_id = rule.id.trim();
            if seen_rule_ids.contains(&rule_id) {
                return Err(VibexError::validation(
                    "route_rule_id_duplicate",
                    "the route lists the same rule id twice",
                ));
            }
            seen_rule_ids.push(rule_id);
        }
        Ok(())
    }

    /// The checks that only a model group has to satisfy.
    fn validate_model_group(&self) -> VibexResult<()> {
        let agent_model = self.agent_model().ok_or_else(|| {
            VibexError::validation(
                "route_agent_model_missing",
                "a model group must name the model Agents ask for",
            )
        })?;
        if agent_model.chars().count() > 256 {
            return Err(VibexError::validation(
                "route_agent_model_invalid",
                "a model group's Agent-facing id is longer than the supported length",
            ));
        }
        if agent_model.contains('/') {
            // The gateway splits `group/<route>/<model>` on the first slash.
            // A slash in the model half would make the id unresolvable.
            return Err(VibexError::validation(
                "route_agent_model_invalid",
                "a model group's Agent-facing id must not contain a slash",
            ));
        }
        if self.enabled_members().next().is_none() {
            return Err(VibexError::validation(
                "route_group_no_enabled_member",
                "a model group needs at least one enabled member",
            ));
        }
        let mut seen = Vec::new();
        for member in &self.members {
            let key = (
                member.model_provider_profile_id.as_str(),
                member.model_id.trim(),
            );
            if seen.contains(&key) {
                return Err(VibexError::validation(
                    "route_member_duplicate_model",
                    "a model group lists the same Provider and model twice",
                ));
            }
            seen.push(key);
        }
        Ok(())
    }
}

/// One Agent's reference to a route.
///
/// Several routes may be bound to one Agent; exactly one is its default. The
/// default is what a new session starts on, so the invariant only holds among
/// the live bindings of one Agent, which the write path enforces.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteBinding {
    pub id: crate::AgentModelRouteBindingId,
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
    pub is_default: bool,
    pub revision: i64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deleted_at_ms: Option<i64>,
}

impl AgentModelRouteBinding {
    pub fn new(agent_id: AgentId, model_route_id: ModelRouteId) -> Self {
        Self {
            id: crate::AgentModelRouteBindingId::new(),
            agent_id,
            model_route_id,
            is_default: false,
            revision: 1,
            created_at_ms: 0,
            updated_at_ms: 0,
            deleted_at_ms: None,
        }
    }
}

/// The capabilities a route may claim, given its members.
///
/// Numeric maxima take the smallest member: if one member cannot hold a
/// prompt, the route cannot either. Booleans take the intersection: a route
/// only claims what every member supports, because a request may land on any of
/// them. A capability a member does not declare counts as unsupported —
/// absent is not "yes".
pub fn intersect_route_capabilities(
    members: impl IntoIterator<Item = crate::ProviderModelCapabilities>,
) -> crate::ProviderModelCapabilities {
    let caps = members.into_iter().collect::<Vec<_>>();
    if caps.is_empty() {
        return crate::ProviderModelCapabilities::default();
    }
    let smallest = |pick: fn(&crate::ProviderModelCapabilities) -> Option<u32>| {
        caps.iter().filter_map(pick).min()
    };
    let every = |pick: fn(&crate::ProviderModelCapabilities) -> Option<bool>| {
        caps.iter()
            .all(|caps| pick(caps) == Some(true))
            .then_some(true)
    };
    crate::ProviderModelCapabilities {
        reasoning: every(|caps| caps.reasoning),
        image_input: every(|caps| caps.image_input),
        pdf_input: every(|caps| caps.pdf_input),
        temperature: every(|caps| caps.temperature),
        context_tokens: smallest(|caps| caps.context_tokens),
        output_tokens: smallest(|caps| caps.output_tokens),
        reasoning_efforts: intersect_reasoning_efforts(&caps),
        ..Default::default()
    }
}

fn intersect_reasoning_efforts(
    caps: &[crate::ProviderModelCapabilities],
) -> Option<crate::ProviderModelReasoningEfforts> {
    let declared = caps
        .iter()
        .filter_map(|caps| caps.reasoning_efforts.clone())
        .collect::<Vec<_>>();
    if declared.len() != caps.len() {
        // A member that declares no effort table supports none of them, so the
        // route cannot promise any either.
        return None;
    }
    let first = declared.first()?;
    let mut levels = std::collections::BTreeMap::new();
    for (level, wire) in &first.0 {
        if declared.iter().all(|efforts| efforts.0.contains_key(level)) {
            levels.insert(*level, wire.clone());
        }
    }
    // A member whose table declares no levels offers none; the route then
    // offers none, rather than falling back to the levels of the others.
    if levels.is_empty() {
        return None;
    }
    Some(crate::ProviderModelReasoningEfforts(levels))
}

/// One model several Providers serve under the same name, ready to become a
/// model group.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedModelGroup {
    /// The name the group would publish, and the model an Agent would ask for.
    ///
    /// It is the shortest spelling any serving Provider uses, without a vendor
    /// prefix: a reader sees the model's own name, not the first vendor's path.
    pub model_id: String,
    /// Providers that would answer, in the order they were configured.
    pub members: Vec<SuggestedModelGroupMember>,
}

/// One Provider's spelling of a suggested model.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SuggestedModelGroupMember {
    pub provider_profile_id: ModelProviderProfileId,
    /// The upstream id this Provider calls the model by.
    pub model_id: String,
}

/// Finds the models several enabled Providers serve under the same name.
///
/// A model group is the natural composition for "the same model at several
/// vendors", so the suggestions are exactly the candidate groups a reader would
/// otherwise build by hand. Two spellings count as one model when a vendor's
/// prefix, a version dot, a separator run, or a snapshot date differs (see
/// [`model_group_match_key`]); a variant suffix such as `-flash`, `:batch`, or
/// a four-digit release stays distinct.
///
/// A suggestion is dropped when a saved group already publishes its name, or
/// already contains every `(Provider, model)` pair it would add. The caller
/// creates a group from a suggestion; this function only reads configuration
/// and never writes.
pub fn suggested_model_groups(
    providers: &[crate::ModelProviderProfile],
    existing: &[ModelRoute],
) -> Vec<SuggestedModelGroup> {
    let existing: Vec<(String, std::collections::BTreeSet<(String, String)>)> = existing
        .iter()
        .filter(|route| route.deleted_at_ms.is_none() && route.is_model_group())
        .map(|route| {
            let name = route
                .agent_model()
                .unwrap_or(route.display_name.trim())
                .to_lowercase();
            let members = route
                .members
                .iter()
                .map(|member| {
                    (
                        member.model_provider_profile_id.as_str().to_string(),
                        member.model_id.trim().to_string(),
                    )
                })
                .collect();
            (name, members)
        })
        .collect();

    // One bucket per spelling key, in Provider order. A Provider that lists two
    // spellings of one model contributes the first only: a group member is one
    // source, not two entries of the same source.
    let mut order = Vec::<String>::new();
    let mut buckets = std::collections::BTreeMap::<String, Vec<SuggestedModelGroupMember>>::new();
    for profile in providers {
        if profile.status != crate::ModelProviderProfileStatus::Enabled {
            continue;
        }
        for entry in profile
            .configured_models
            .iter()
            .filter(|entry| entry.enabled)
        {
            let key = model_group_match_key(&entry.id);
            if key.is_empty() {
                continue;
            }
            let members = buckets.entry(key.clone()).or_default();
            if members
                .iter()
                .any(|member| member.provider_profile_id == profile.id)
            {
                continue;
            }
            if members.is_empty() {
                order.push(key);
            }
            members.push(SuggestedModelGroupMember {
                provider_profile_id: profile.id.clone(),
                model_id: entry.id.trim().to_string(),
            });
        }
    }

    let mut suggestions = Vec::new();
    let mut seen_names = std::collections::BTreeSet::new();
    for key in order {
        let members = buckets.remove(&key).unwrap_or_default();
        if members.len() < 2 || members.len() > MAX_ROUTE_MEMBERS {
            continue;
        }
        let Some(name) = suggested_model_group_name(&members) else {
            continue;
        };
        let lower = name.to_lowercase();
        if !seen_names.insert(lower.clone()) {
            continue;
        }
        if existing.iter().any(|(name, _)| name == &lower) {
            continue;
        }
        let pairs = members
            .iter()
            .map(|member| {
                (
                    member.provider_profile_id.as_str().to_string(),
                    member.model_id.clone(),
                )
            })
            .collect::<std::collections::BTreeSet<_>>();
        if existing
            .iter()
            .any(|(_, covered)| pairs.iter().all(|pair| covered.contains(pair)))
        {
            continue;
        }
        suggestions.push(SuggestedModelGroup {
            model_id: name,
            members,
        });
    }
    suggestions.sort_by(|left, right| {
        right
            .members
            .len()
            .cmp(&left.members.len())
            .then_with(|| left.model_id.cmp(&right.model_id))
    });
    suggestions
}

/// The group name a suggestion publishes: the shortest member spelling, with
/// any vendor path removed.
fn suggested_model_group_name(members: &[SuggestedModelGroupMember]) -> Option<String> {
    let mut best: Option<&str> = None;
    for member in members {
        let candidate = member
            .model_id
            .rsplit('/')
            .next()
            .unwrap_or(&member.model_id)
            .trim();
        if candidate.is_empty() {
            continue;
        }
        if best.is_none_or(|current| candidate.chars().count() < current.chars().count()) {
            best = Some(candidate);
        }
    }
    best.map(|name| {
        name.chars()
            .take(ModelRoute::MAX_DISPLAY_NAME_LEN)
            .collect()
    })
}

/// The key two vendors' spellings of one model share.
///
/// Lowercased, without a vendor path, with a version dot written as `-`, runs
/// of spaces, `_` and `-` collapsed, and a trailing snapshot date removed. A
/// variant after `:` (Fireworks' `:batch`), a `-flash`/`-thinking` suffix, and
/// a four-digit release (`-2507`) are part of the model's name and stay.
pub fn model_group_match_key(model_id: &str) -> String {
    let mut key = model_id
        .trim()
        .to_lowercase()
        .rsplit('/')
        .next()
        .unwrap_or_default()
        .to_string();
    key = key
        .chars()
        .map(|ch| {
            if ch == '_' || ch.is_whitespace() {
                '-'
            } else {
                ch
            }
        })
        .collect();
    // A dot between digits is a version separator: `claude-opus-5.5` and
    // Fireworks' `p` spelling become the same dashed form.
    let chars = key.chars().collect::<Vec<_>>();
    let mut normalized = String::with_capacity(key.len());
    for (index, ch) in chars.iter().enumerate() {
        let between_digits = index > 0
            && index + 1 < chars.len()
            && chars[index - 1].is_ascii_digit()
            && chars[index + 1].is_ascii_digit();
        if (*ch == '.' || *ch == 'p') && between_digits {
            normalized.push('-');
        } else {
            normalized.push(*ch);
        }
    }
    // Collapse repeated dashes so `v4.1-flash` and `v4-1_flash` agree.
    let mut collapsed = String::with_capacity(normalized.len());
    let mut previous_dash = false;
    for ch in normalized.chars() {
        if ch == '-' {
            if !previous_dash {
                collapsed.push('-');
            }
            previous_dash = true;
        } else {
            collapsed.push(ch);
            previous_dash = false;
        }
    }
    let mut key = collapsed.trim_matches('-').to_string();
    if let Some(index) = key.rfind(['-', '@']) {
        let suffix = &key[index + 1..];
        if snapshot_date(suffix) {
            key.truncate(index);
        }
    }
    key
}

/// Whether a suffix is the date a vendor dates a snapshot by: `YYYYMMDD` from
/// 2000, or a two-digit year (`23`..`39`) plus month and day.
fn snapshot_date(value: &str) -> bool {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return false;
    }
    match value.len() {
        8 => value.starts_with("20"),
        6 => {
            let year = value[0..2].parse::<u32>().unwrap_or(0);
            let month = value[2..4].parse::<u32>().unwrap_or(0);
            let day = value[4..6].parse::<u32>().unwrap_or(0);
            (23..=39).contains(&year) && (1..=12).contains(&month) && (1..=31).contains(&day)
        }
        _ => false,
    }
}

/// A model id that names a route group, split into the route and the model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RouteGroupModelRef<'a> {
    pub group_id: &'a str,
    pub model_id: &'a str,
}

/// Splits a `group/<route>/<model>` model id.
///
/// Returns `None` for anything that is not group-shaped, so an ordinary
/// Provider model keeps resolving the way it did.
pub fn route_group_model_ref(requested: &str) -> Option<RouteGroupModelRef<'_>> {
    let rest = requested.strip_prefix(ROUTE_GROUP_MODEL_PREFIX)?;
    let (group_id, model_id) = rest.split_once('/')?;
    let group_id = group_id.trim();
    let model_id = model_id.trim();
    (!group_id.is_empty() && !model_id.is_empty())
        .then_some(RouteGroupModelRef { group_id, model_id })
}

/// Replaces a route, including its member order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteUpdateRequest {
    pub route: ModelRoute,
    pub expected_revision: i64,
}

/// Removes a route and every binding that pointed at it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ModelRouteDeleteRequest {
    pub model_route_id: ModelRouteId,
}

/// Lets one Agent use a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteBindRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
    /// Make this the route a new session starts on.
    #[serde(default)]
    pub make_default: bool,
}

/// Stops one Agent from using a route.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteUnbindRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
}

/// Chooses which of an Agent's routes a new session starts on.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelRouteDefaultRequest {
    pub agent_id: AgentId,
    pub model_route_id: ModelRouteId,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn model_member(order_index: i64, model: &str) -> ModelRouteMember {
        ModelRouteMember::new(ModelProviderProfileId::new(), model, order_index)
    }

    fn route(kind: RouteKind, members: Vec<ModelRouteMember>) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            kind,
            agent_model_id: matches!(kind, RouteKind::ModelGroup).then(|| "Standard".to_string()),
            members,
            status: RouteStatus::Enabled,
            ..Default::default()
        }
    }

    #[test]
    fn a_direct_route_must_hold_exactly_one_member() {
        let one = route(RouteKind::Direct, vec![model_member(0, "a")]);
        assert!(one.validate().is_ok());

        let two = route(
            RouteKind::Direct,
            vec![model_member(0, "a"), model_member(1, "b")],
        );
        let error = two.validate().unwrap_err();
        assert_eq!(error.code, "route_direct_member_count_invalid");

        let none = route(RouteKind::Direct, vec![]);
        assert_eq!(none.validate().unwrap_err().code, "route_member_missing");
    }

    #[test]
    fn a_model_group_member_limit_is_sixteen() {
        let at_limit = route(
            RouteKind::ModelGroup,
            (0..16).map(|index| model_member(index, "m")).collect(),
        );
        assert!(at_limit.validate().is_ok());

        let over = route(
            RouteKind::ModelGroup,
            (0..17).map(|index| model_member(index, "m")).collect(),
        );
        assert_eq!(
            over.validate().unwrap_err().code,
            "route_member_limit_exceeded"
        );
    }

    #[test]
    fn a_route_rejects_duplicate_positions() {
        let route = route(
            RouteKind::ModelGroup,
            vec![model_member(0, "m"), model_member(0, "m")],
        );
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_order_duplicate"
        );
    }

    #[test]
    fn a_disabled_member_keeps_its_place_but_is_not_counted() {
        let mut route = route(
            RouteKind::ModelGroup,
            vec![model_member(0, "m"), model_member(1, "m")],
        );
        route.members[1].enabled = false;
        assert!(route.validate().is_ok());
        assert_eq!(route.source_count(), 1);
        assert_eq!(route.members.len(), 2, "the place is kept");
    }

    #[test]
    fn every_gateway_model_id_is_a_group_id() {
        let route = route(RouteKind::ModelGroup, vec![model_member(0, "m")]);
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
        assert_eq!(
            route.group_model_id("deepseek-chat"),
            format!("group/{}/deepseek-chat", route.id.as_str())
        );
    }

    #[test]
    fn a_direct_route_model_id_describes_no_gateway_entry() {
        // A direct route never reaches the gateway, so its id is only ever
        // compared against a gateway Profile's catalogue, never called.
        let route = route(RouteKind::Direct, vec![model_member(0, "deepseek-chat")]);
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
    }

    #[test]
    fn route_capabilities_take_the_smallest_window_and_the_shared_features() {
        let narrow = crate::ProviderModelCapabilities {
            context_tokens: Some(32_000),
            output_tokens: Some(4_000),
            reasoning: Some(true),
            image_input: Some(false),
            ..Default::default()
        };
        let wide = crate::ProviderModelCapabilities {
            context_tokens: Some(200_000),
            output_tokens: Some(8_000),
            reasoning: Some(true),
            image_input: Some(true),
            ..Default::default()
        };
        let merged = intersect_route_capabilities([narrow, wide]);
        assert_eq!(
            merged.context_tokens,
            Some(32_000),
            "the smallest window wins"
        );
        assert_eq!(merged.output_tokens, Some(4_000));
        assert_eq!(merged.reasoning, Some(true), "both reason");
        assert_eq!(merged.image_input, None, "one cannot take an image");
    }

    #[test]
    fn an_undeclared_capability_is_not_claimed() {
        let declared = crate::ProviderModelCapabilities {
            reasoning: Some(true),
            ..Default::default()
        };
        let silent = crate::ProviderModelCapabilities::default();
        let merged = intersect_route_capabilities([declared, silent]);
        assert_eq!(merged.reasoning, None, "absent is not yes");
    }

    #[test]
    fn a_group_model_id_names_the_route_and_the_model() {
        let route = route(
            RouteKind::ModelGroup,
            vec![model_member(0, "deepseek-chat")],
        );
        assert_eq!(route.model_id(), format!("group/{}/", route.id.as_str()));
        assert_eq!(
            route.group_model_id("deepseek-chat"),
            format!("group/{}/deepseek-chat", route.id.as_str())
        );
        let id = route.group_model_id("deepseek-chat");
        let parsed = route_group_model_ref(&id).expect("a group id parses");
        assert_eq!(parsed.group_id, route.id.as_str());
        assert_eq!(parsed.model_id, "deepseek-chat");
        assert!(route_group_model_ref("route/standard").is_none());
    }

    fn model_group(members: Vec<ModelRouteMember>) -> ModelRoute {
        ModelRoute {
            display_name: "Smart".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("deepseek-chat".to_string()),
            members,
            status: RouteStatus::Enabled,
            ..Default::default()
        }
    }

    #[test]
    fn a_model_group_lists_models_from_different_providers() {
        let mut left = model_member(0, "deepseek-chat");
        left.weight = 3;
        let mut right = model_member(1, "deepseek-v3");
        right.first_token_timeout_ms = Some(9_000);
        right.context_tokens = Some(64_000);
        let route = model_group(vec![left, right]);
        route
            .validate()
            .expect("two upstream models are a valid group");
        assert_eq!(route.agent_model(), Some("deepseek-chat"));
        assert_eq!(
            route.gateway_model_id().unwrap(),
            format!("group/{}/deepseek-chat", route.id.as_str())
        );
        assert!(route.serves_model_group_id(&route.gateway_model_id().unwrap()));
        assert!(!route.serves_model_group_id(&route.group_model_id("deepseek-v3")));
        assert_eq!(route.distinct_models(), vec!["deepseek-chat"]);
    }

    #[test]
    fn a_model_group_requires_an_agent_facing_id_and_an_enabled_member() {
        let mut route = model_group(vec![model_member(0, "deepseek-chat")]);
        route.agent_model_id = None;
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_agent_model_missing"
        );

        let mut slashed = model_group(vec![model_member(0, "deepseek-chat")]);
        slashed.agent_model_id = Some("vendor/model".to_string());
        assert_eq!(
            slashed.validate().unwrap_err().code,
            "route_agent_model_invalid"
        );

        let mut disabled = model_group(vec![model_member(0, "deepseek-chat")]);
        disabled.members[0].enabled = false;
        assert_eq!(
            disabled.validate().unwrap_err().code,
            "route_group_no_enabled_member"
        );
    }

    #[test]
    fn a_model_group_rejects_the_same_provider_and_model_twice() {
        let provider = ModelProviderProfileId::new();
        let first = ModelRouteMember::new(provider.clone(), "deepseek-chat", 0);
        let second = ModelRouteMember::new(provider, "deepseek-chat", 1);
        assert_eq!(
            model_group(vec![first, second])
                .validate()
                .unwrap_err()
                .code,
            "route_member_duplicate_model"
        );
    }

    #[test]
    fn a_model_group_requires_a_manual_member() {
        let mut route = model_group(vec![model_member(0, "deepseek-chat")]);
        route.strategy = RouteStrategy::Manual;
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_manual_member_missing"
        );
        route.pick_member_id = Some(route.members[0].id.clone());
        assert!(route.validate().is_ok());
    }

    #[test]
    fn member_weights_and_timeouts_are_bounded() {
        let mut route = model_group(vec![model_member(0, "deepseek-chat")]);
        route.members[0].weight = 0;
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_weight_invalid"
        );
        route.members[0].weight = MAX_MEMBER_WEIGHT + 1;
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_weight_invalid"
        );
        route.members[0].weight = 1;
        route.members[0].first_token_timeout_ms = Some(0);
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_member_first_token_timeout_invalid"
        );
    }

    #[test]
    fn route_patience_is_bounded_and_defaults_to_off() {
        let default = model_group(vec![model_member(0, "deepseek-chat")]);
        assert_eq!(default.patience_ms, None);
        assert!(default.validate().is_ok());

        let mut too_long = model_group(vec![model_member(0, "deepseek-chat")]);
        too_long.patience_ms = Some(MAX_ROUTE_PATIENCE_MS + 1);
        assert_eq!(
            too_long.validate().unwrap_err().code,
            "route_patience_invalid"
        );

        let mut off = model_group(vec![model_member(0, "deepseek-chat")]);
        off.patience_ms = Some(0);
        assert_eq!(off.validate().unwrap_err().code, "route_patience_invalid");
    }

    #[test]
    fn a_rule_matches_agents_tokens_images_and_time() {
        let member = model_member(0, "deepseek-chat");
        let member_id = member.id.clone();
        let mut route = model_group(vec![member]);
        route.rules = vec![ModelRouteRule {
            id: "rule-1".to_string(),
            use_member_id: Some(member_id.clone()),
            more_member_ids: vec![],
            use_effort: Some("high".to_string()),
            tokens_min: Some(100_000),
            images: Some(true),
            effort: Some("high".to_string()),
            agents: vec!["codex".to_string()],
            intent: None,
            compact: None,
            time: Some(RouteTimeWindow {
                days: vec![1, 2, 3, 4, 5],
                start_minute: 9 * 60,
                end_minute: 18 * 60,
                zone: None,
            }),
            folders: vec!["vibex".to_string()],
            except_folders: vec!["vendor".to_string()],
            pause: false,
        }];
        route.validate().expect("a fully specified rule validates");

        let mut context = RouteRuleContext {
            agent_id: Some("codex".to_string()),
            context_tokens: Some(200_000),
            has_images: true,
            requested_effort: Some("high".to_string()),
            intent: None,
            workspace_root: Some("/home/me/vibex".to_string()),
            local_weekday: Some(3),
            local_minute: Some(10 * 60),
            ..Default::default()
        };
        assert!(rule_matches(&route.rules[0], &context));
        assert_eq!(
            first_matching_rule(&route.rules, &context).map(|(index, _)| index),
            Some(0)
        );

        context.agent_id = Some("claude".to_string());
        assert!(!rule_matches(&route.rules[0], &context));
        context.agent_id = Some("codex".to_string());
        context.context_tokens = Some(10_000);
        assert!(!rule_matches(&route.rules[0], &context));
        context.context_tokens = Some(200_000);
        context.has_images = false;
        assert!(!rule_matches(&route.rules[0], &context));
        context.has_images = true;
        context.local_minute = Some(20 * 60);
        assert!(!rule_matches(&route.rules[0], &context));
        context.local_minute = Some(10 * 60);
        context.workspace_root = Some("/home/me/vibex/vendor".to_string());
        assert!(!rule_matches(&route.rules[0], &context));
    }

    #[test]
    fn a_pause_rule_cannot_also_choose_a_member() {
        let member = model_member(0, "deepseek-chat");
        let member_id = member.id.clone();
        let mut route = model_group(vec![member]);
        route.rules = vec![ModelRouteRule {
            id: "pause".to_string(),
            use_member_id: Some(member_id.clone()),
            pause: true,
            images: Some(false),
            ..Default::default()
        }];
        assert_eq!(
            route.validate().unwrap_err().code,
            "route_rule_pause_action_invalid"
        );
        route.rules[0].use_member_id = None;
        assert!(route.validate().is_ok());
    }

    #[test]
    fn a_time_window_may_cross_midnight() {
        let window = RouteTimeWindow {
            days: vec![5],
            start_minute: 22 * 60,
            end_minute: 2 * 60,
            zone: None,
        };
        window.validate().unwrap();
        assert!(window.contains(5, 23 * 60));
        assert!(window.contains(5, 60));
        assert!(!window.contains(5, 12 * 60));
        assert!(!window.contains(6, 60), "the tail belongs to Friday");
    }

    #[test]
    fn the_simulator_orders_by_strategy_and_reports_a_simulated_failure() {
        let mut first = model_member(0, "deepseek-chat");
        first.weight = 3;
        let second = model_member(1, "kimi-k2");
        let first_id = first.id.clone();
        let second_id = second.id.clone();
        let mut route = model_group(vec![first, second]);
        route.strategy = RouteStrategy::Weighted;
        let context = RouteRuleContext::default();
        let observations = vec![
            RouteMemberObservation {
                member_id: Some(first_id.clone()),
                p50_ms: Some(400),
                ..Default::default()
            },
            RouteMemberObservation {
                member_id: Some(second_id.clone()),
                p50_ms: Some(100),
                ..Default::default()
            },
        ];
        let start = simulate_model_route(&route, &context, 0, &observations, None);
        assert_eq!(start.order[0].id, first_id, "weight three starts first");
        assert_eq!(start.estimated_latency_ms, Some(400));

        let failure = simulate_model_route(&route, &context, 0, &observations, Some(&first_id));
        assert!(failure.failed_member_skipped);
        assert_eq!(failure.order[0].id, second_id);
        assert_eq!(failure.estimated_latency_ms, Some(100));

        route.strategy = RouteStrategy::LowestLatency;
        let latency = simulate_model_route(&route, &context, 0, &observations, None);
        assert_eq!(latency.order[0].id, second_id, "the faster member leads");
    }

    #[test]
    fn the_simulator_reports_a_matching_pause_rule() {
        let member = model_member(0, "deepseek-chat");
        let mut route = model_group(vec![member]);
        route.rules = vec![ModelRouteRule {
            id: "pause".to_string(),
            pause: true,
            time: Some(RouteTimeWindow {
                days: vec![1, 2, 3, 4, 5],
                start_minute: 9 * 60,
                end_minute: 10 * 60,
                zone: None,
            }),
            ..Default::default()
        }];
        let context = RouteRuleContext {
            local_weekday: Some(2),
            local_minute: Some(9 * 60 + 30),
            ..Default::default()
        };
        let simulation = simulate_model_route(&route, &context, 0, &[], None);
        assert!(simulation.paused);
        assert_eq!(simulation.matched_rule_id.as_deref(), Some("pause"));
        assert!(simulation.order.is_empty());
    }

    fn suggestion_provider(name: &str, models: &[&str]) -> crate::ModelProviderProfile {
        crate::ModelProviderProfile {
            display_name: name.to_string(),
            status: crate::ModelProviderProfileStatus::Enabled,
            configured_models: models
                .iter()
                .map(|id| crate::ModelProviderCatalogEntry {
                    id: (*id).to_string(),
                    display_name: None,
                    enabled: true,
                    metadata: Vec::new(),
                    capabilities: Default::default(),
                })
                .collect(),
            ..Default::default()
        }
    }

    #[test]
    fn a_group_name_is_the_model_an_agent_asks_for() {
        let mut route = route(RouteKind::ModelGroup, vec![model_member(0, "m")]);
        route.display_name = "Fast".to_string();
        route.agent_model_id = Some("old-name".to_string());
        route.sync_agent_model_id();
        assert_eq!(route.agent_model(), Some("Fast"));
        assert_eq!(
            route.gateway_model_id(),
            Some(format!("group/{}/Fast", route.id.as_str()))
        );

        let mut direct = route.clone();
        direct.kind = RouteKind::Direct;
        direct.sync_agent_model_id();
        assert_eq!(direct.agent_model_id, None);
    }

    #[test]
    fn suggestions_merge_vendor_spellings_of_one_model() {
        let left = suggestion_provider("Left", &["deepseek-v4.1-flash"]);
        let right = suggestion_provider("Right", &["vendor/deepseek-v4-1-flash"]);
        let third = suggestion_provider("Third", &["deepseek-v4-1-flash-260910"]);
        let suggestions =
            suggested_model_groups(&[left.clone(), right.clone(), third.clone()], &[]);
        assert_eq!(suggestions.len(), 1);
        assert_eq!(suggestions[0].members.len(), 3);
        assert_eq!(
            suggestions[0].members[0].provider_profile_id.as_str(),
            left.id.as_str()
        );
        assert_eq!(
            suggestions[0].members[1].model_id,
            "vendor/deepseek-v4-1-flash"
        );
        assert!(!suggestions[0].model_id.contains('/'));
    }

    #[test]
    fn suggestions_keep_variants_apart() {
        let left = suggestion_provider("Left", &["deepseek-v4.1-flash", "deepseek-v4.1-thinking"]);
        let right =
            suggestion_provider("Right", &["deepseek-v4-1-flash", "deepseek-v4-1-thinking"]);
        let suggestions = suggested_model_groups(&[left, right], &[]);
        assert_eq!(
            suggestions.len(),
            2,
            "flash and thinking are different models"
        );
    }

    #[test]
    fn suggestions_need_two_providers_and_skip_a_saved_group() {
        let only = suggestion_provider("Only", &["shared"]);
        assert!(suggested_model_groups(std::slice::from_ref(&only), &[]).is_empty());

        let left = suggestion_provider("Left", &["shared"]);
        let right = suggestion_provider("Right", &["shared"]);
        let suggestions = suggested_model_groups(&[left.clone(), right.clone()], &[]);
        assert_eq!(suggestions.len(), 1);

        let mut saved = model_group(vec![
            ModelRouteMember::new(left.id.clone(), "shared", 0),
            ModelRouteMember::new(right.id.clone(), "shared", 1),
        ]);
        saved.display_name = suggestions[0].model_id.clone();
        saved.agent_model_id = Some(suggestions[0].model_id.clone());
        assert!(
            suggested_model_groups(&[left, right], &[saved]).is_empty(),
            "a saved group covers the same providers and model"
        );
    }

    #[test]
    fn the_seven_strategies_round_trip_their_wire_ids() {
        assert_eq!(RouteStrategy::ALL.len(), 7);
        for strategy in RouteStrategy::ALL {
            assert_eq!(RouteStrategy::from_id(strategy.id()), Some(strategy));
            assert!(!strategy.guidance().is_empty());
        }
        assert_eq!(RouteStrategy::from_id("pace"), None);
        assert_eq!(RouteKind::ALL.len(), 2);
        assert!(RouteKind::ModelGroup.via_gateway());
        assert_eq!(
            RouteKind::from_id("provider_group"),
            Some(RouteKind::ModelGroup),
            "a retired row still reads as the group it now is"
        );
    }
}

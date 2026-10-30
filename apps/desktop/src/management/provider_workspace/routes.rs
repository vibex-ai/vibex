//! The gateway's Routing tab: the route map, the story of one request, and the
//! request records that pick it.
//!
//! The three read as one. While nothing is picked, the map plays the requests
//! running through the gateway and then rests on the last one, and the story
//! under it says in words what the map shows. Picking a record pins the map to
//! that request and replays its walk from the ledger: the member tried first,
//! each Key that refused and why, and who answered.
//!
//! A request this window watched while it ran is drawn from the gateway's own
//! announcements, kept here as a trail, so its dot moves when the gateway moved
//! it. The ledger adds what the announcements do not say — which Key refused
//! and why — once the request has been written down.

use std::collections::HashMap;

use super::route_stage::{
    FlightClock, HubState, LaneTone, RouteStage, StageAgent, StageFlight, StageHub, StageKey,
    StageLane, StageModel, StageRow, StageStep, StepVerdict, agent_hue,
};
use super::*;

/// How many requests the map remembers having watched.
const TRAIL_LIMIT: usize = 32;
/// How long a finished request's trail is kept after its end was heard.
const TRAIL_KEEP_MS: i64 = 60_000;
/// How long a finished request stays on the live map.
const LINGER_MS: i64 = 12_000;
/// The live map's room.
const STAGE_AGENTS: usize = 4;
const STAGE_LANES: usize = 10;
/// The height of one request record; the list is virtualised on it.
const ROUTE_ROW_H: f32 = 36.0;
/// The id a request without an Agent is drawn under.
const UNKNOWN_AGENT: &str = "agent";

// ----- what this window heard ----------------------------------------------------------

/// One member the gateway announced for a request.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GatewayTrailHop {
    pub profile_id: vibex_core::ModelProviderProfileId,
    /// The Key the member starts on.
    pub credential_id: Option<vibex_core::RequestId>,
    /// The member's place in the attempt order, from one.
    pub attempt: u32,
    /// When this window heard it, on the wall clock.
    pub heard_ms: i64,
}

/// A request the map watched while it ran: the members the gateway announced,
/// in order, each timed by when this window heard it.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct GatewayRouteTrail {
    pub request_id: vibex_core::RequestId,
    pub agent_id: Option<String>,
    pub model_route_id: Option<String>,
    pub requested_model: String,
    pub hops: Vec<GatewayTrailHop>,
    /// When its first token was heard.
    pub streaming_since: Option<i64>,
    /// When its end was heard, and how it ended.
    pub ended: Option<(i64, vibex_core::GatewayRequestOutcome)>,
}

impl GatewayRouteTrail {
    /// A request already running when the window started listening: all that
    /// is known is the member it is on now.
    fn from_inflight(inflight: &vibex_core::GatewayInflight) -> Self {
        Self {
            request_id: inflight.request_id.clone(),
            agent_id: inflight.agent_id.clone(),
            model_route_id: inflight.model_route_id.clone(),
            requested_model: inflight.requested_model.clone(),
            hops: vec![GatewayTrailHop {
                profile_id: inflight.model_provider_profile_id.clone(),
                credential_id: inflight.credential_id.clone(),
                attempt: inflight.attempt,
                heard_ms: inflight.started_at_ms,
            }],
            streaming_since: inflight.first_token_at_ms,
            ended: None,
        }
    }

    fn started_ms(&self) -> i64 {
        self.hops.first().map_or(0, |hop| hop.heard_ms)
    }
}

/// Folds one gateway event into the trails, and forgets the ones whose flight
/// is long over.
fn note_trail(trails: &mut Vec<GatewayRouteTrail>, event: &vibex_core::GatewayEvent, now: i64) {
    match event {
        vibex_core::GatewayEvent::Began(inflight) => {
            let index = match trails
                .iter()
                .position(|trail| trail.request_id == inflight.request_id)
            {
                Some(index) => index,
                None => {
                    trails.push(GatewayRouteTrail {
                        hops: Vec::new(),
                        streaming_since: None,
                        ..GatewayRouteTrail::from_inflight(inflight)
                    });
                    trails.len() - 1
                }
            };
            let trail = &mut trails[index];
            // A member announced again is not a new step of the walk.
            if trail
                .hops
                .last()
                .is_none_or(|hop| hop.attempt != inflight.attempt)
            {
                trail.hops.push(GatewayTrailHop {
                    profile_id: inflight.model_provider_profile_id.clone(),
                    credential_id: inflight.credential_id.clone(),
                    attempt: inflight.attempt,
                    heard_ms: now,
                });
                // A new member has said nothing yet.
                trail.streaming_since = None;
            }
        }
        vibex_core::GatewayEvent::FirstToken { request_id, .. } => {
            if let Some(trail) = trails
                .iter_mut()
                .find(|trail| &trail.request_id == request_id)
            {
                trail.streaming_since.get_or_insert(now);
            }
        }
        vibex_core::GatewayEvent::Ended {
            request_id,
            outcome,
        } => {
            if let Some(trail) = trails
                .iter_mut()
                .find(|trail| &trail.request_id == request_id)
            {
                trail.ended.get_or_insert((now, *outcome));
            }
        }
        _ => return,
    }
    trails.retain(|trail| trail.ended.is_none_or(|(at, _)| now - at < TRAIL_KEEP_MS));
    while trails.len() > TRAIL_LIMIT {
        let oldest = trails
            .iter()
            .position(|trail| trail.ended.is_some())
            .unwrap_or(0);
        trails.remove(oldest);
    }
}

impl ManagementCenter {
    /// Keeps the route map's memory of one gateway event.
    pub(crate) fn note_gateway_trail(&mut self, event: &vibex_core::GatewayEvent) {
        note_trail(
            &mut self.gateway_route_trails,
            event,
            vibex_core::unix_timestamp_ms(),
        );
    }
}

// ----- one request's walk -----------------------------------------------------------------

/// What became of one step of a walk.
#[derive(Clone, Copy, Debug, PartialEq)]
enum WalkVerdict {
    Pending,
    Answered,
    /// The Key or member refused, for this reason.
    Failed(vibex_core::GatewayFailureKind),
    /// The member was passed over without being asked, for this reason.
    Skipped(vibex_core::GatewayFailureKind),
    /// The gateway went on to the next member; why is not written down yet.
    MovedOn,
    /// The request ended here without an answer.
    Broke,
}

/// One member, or one Key of a member, a request went to.
#[derive(Clone, Debug, PartialEq)]
struct WalkStep {
    profile_id: vibex_core::ModelProviderProfileId,
    /// The Key by id, when the record names it.
    key_id: Option<String>,
    /// The Key by the name its Provider gave it, when only the name is known.
    key_name: Option<String>,
    verdict: WalkVerdict,
    /// When the gateway announced it, for a request seen live.
    at_ms: Option<i64>,
}

fn outcome_answered(outcome: vibex_core::GatewayRequestOutcome) -> bool {
    matches!(
        outcome,
        vibex_core::GatewayRequestOutcome::Ok | vibex_core::GatewayRequestOutcome::Failover
    )
}

/// The walk one recorded request took, member by member and Key by Key, in
/// the order the gateway tried them.
///
/// The ledger keeps two ordered lists: the Keys that refused and the members
/// that were left. A member's refusing Keys come before the member itself, so
/// each Key is given to the member it belongs to until a Key of another
/// Provider comes up; a member none of whose Keys refused was passed over.
/// Whatever Keys are left belong to the member that answered.
fn ledger_walk(
    row: &vibex_core::GatewayRequestSummary,
    owner_of: impl Fn(&vibex_core::RequestId) -> Option<vibex_core::ModelProviderProfileId>,
) -> Vec<WalkStep> {
    // Refused before any member was asked: nothing was walked.
    if row.outcome == vibex_core::GatewayRequestOutcome::Rejected {
        return Vec::new();
    }
    let route = row.route.as_ref();
    let attempts = route
        .map(|route| route.failed_attempts.as_slice())
        .unwrap_or_default();
    let key_step = |profile_id: vibex_core::ModelProviderProfileId,
                    attempt: &vibex_core::GatewayRouteAttemptSummary| WalkStep {
        profile_id,
        key_id: Some(attempt.credential_id.as_str().to_string()),
        key_name: attempt.credential_name.clone(),
        verdict: WalkVerdict::Failed(attempt.failure),
        at_ms: None,
    };
    let mut steps = Vec::new();
    let mut cursor = 0;
    for member in route
        .map(|route| route.skipped_members.as_slice())
        .unwrap_or_default()
    {
        let mut tried = false;
        while let Some(attempt) = attempts.get(cursor) {
            if owner_of(&attempt.credential_id)
                .is_some_and(|owner| owner != member.model_provider_profile_id)
            {
                break;
            }
            steps.push(key_step(member.model_provider_profile_id.clone(), attempt));
            cursor += 1;
            tried = true;
        }
        if !tried {
            steps.push(WalkStep {
                profile_id: member.model_provider_profile_id.clone(),
                key_id: None,
                key_name: None,
                verdict: WalkVerdict::Skipped(member.failure),
                at_ms: None,
            });
        }
    }
    let answering = row.model_provider_profile_id.clone();
    for attempt in &attempts[cursor..] {
        steps.push(key_step(
            owner_of(&attempt.credential_id).unwrap_or_else(|| answering.clone()),
            attempt,
        ));
    }
    if outcome_answered(row.outcome) {
        steps.push(WalkStep {
            profile_id: answering,
            key_id: None,
            key_name: row.credential_name.clone(),
            verdict: WalkVerdict::Answered,
            at_ms: None,
        });
    } else if row.credential_name.is_some() || steps.is_empty() {
        // A Key answered and the reply broke off, or the request failed on
        // its first member with nothing else written down.
        steps.push(WalkStep {
            profile_id: answering,
            key_id: None,
            key_name: row.credential_name.clone(),
            verdict: WalkVerdict::Broke,
            at_ms: None,
        });
    }
    steps
}

/// The walk of a request seen live: one step per member the gateway announced.
/// The ledger row, once written, says why each member before the last let it
/// go and which Key finally answered.
fn trail_walk(
    trail: &GatewayRouteTrail,
    row: Option<&vibex_core::GatewayRequestSummary>,
) -> Vec<WalkStep> {
    let skipped = row
        .and_then(|row| row.route.as_ref())
        .map(|route| route.skipped_members.as_slice())
        .unwrap_or_default();
    let mut cursor = 0;
    let mut steps = Vec::with_capacity(trail.hops.len());
    for (index, hop) in trail.hops.iter().enumerate() {
        let last = index + 1 == trail.hops.len();
        let verdict = if last {
            match trail.ended {
                None => WalkVerdict::Pending,
                Some((_, outcome)) if outcome_answered(outcome) => WalkVerdict::Answered,
                Some(_) => WalkVerdict::Broke,
            }
        } else {
            match skipped[cursor..]
                .iter()
                .position(|member| member.model_provider_profile_id == hop.profile_id)
            {
                Some(offset) => {
                    let member = &skipped[cursor + offset];
                    cursor += offset + 1;
                    WalkVerdict::Failed(member.failure)
                }
                None => WalkVerdict::MovedOn,
            }
        };
        let answered_key = last
            .then(|| row.and_then(|row| row.credential_name.clone()))
            .flatten();
        steps.push(WalkStep {
            profile_id: hop.profile_id.clone(),
            key_id: answered_key
                .is_none()
                .then(|| hop.credential_id.as_ref().map(|id| id.as_str().to_string()))
                .flatten(),
            key_name: answered_key,
            verdict,
            at_ms: Some(hop.heard_ms),
        });
    }
    steps
}

/// How many times a recorded request moved on before it ended.
fn walk_moves(walk: &[WalkStep]) -> usize {
    walk.iter()
        .filter(|step| {
            matches!(
                step.verdict,
                WalkVerdict::Failed(_) | WalkVerdict::Skipped(_) | WalkVerdict::MovedOn
            )
        })
        .count()
}

// ----- what the map shows -------------------------------------------------------------------

/// One request the map shows, and where its facts come from.
#[derive(Clone)]
struct Staged {
    /// What this window heard while it ran, when it watched it.
    trail: Option<GatewayRouteTrail>,
    /// Its ledger row, once written.
    row: Option<vibex_core::GatewayRequestSummary>,
    clock: FlightClock,
}

impl Staged {
    fn request_id(&self) -> Option<&vibex_core::RequestId> {
        self.trail
            .as_ref()
            .map(|trail| &trail.request_id)
            .or(self.row.as_ref().map(|row| &row.request_id))
    }

    fn agent_id(&self) -> &str {
        self.trail
            .as_ref()
            .and_then(|trail| trail.agent_id.as_deref())
            .or(self.row.as_ref().and_then(|row| row.agent_id.as_deref()))
            .unwrap_or(UNKNOWN_AGENT)
    }

    fn requested_model(&self) -> &str {
        self.trail
            .as_ref()
            .map(|trail| trail.requested_model.as_str())
            .or(self.row.as_ref().map(|row| row.model_requested.as_str()))
            .unwrap_or_default()
    }

    fn route_id(&self) -> Option<String> {
        self.trail
            .as_ref()
            .and_then(|trail| trail.model_route_id.clone())
            .or_else(|| {
                vibex_core::route_group_model_ref(self.requested_model())
                    .map(|parsed| parsed.group_id.to_string())
            })
    }

    fn running(&self) -> bool {
        self.trail
            .as_ref()
            .is_some_and(|trail| trail.ended.is_none())
    }
}

/// The model an Agent asked for, without the route group's address around it.
fn asked_model(requested: &str) -> &str {
    vibex_core::route_group_model_ref(requested)
        .map(|parsed| parsed.model_id)
        .unwrap_or(requested)
}

/// A Key as its Provider names it: the user's name, else its masked value.
fn key_label(credential_id: &str, display_name: &str, masked_hint: &str) -> String {
    let name = display_name.trim();
    if !name.is_empty() && name != format!("Key {credential_id}") {
        return name.to_string();
    }
    if !masked_hint.is_empty() {
        return masked_hint.to_string();
    }
    "Key".to_string()
}

/// The members' column as it is assembled: the lanes, the rows they sit in,
/// and which route member each lane draws.
#[derive(Default)]
struct LaneBook {
    lanes: Vec<StageLane>,
    rows: Vec<StageRow>,
    owners: Vec<(Option<String>, vibex_core::ModelProviderProfileId, String)>,
}

impl LaneBook {
    fn push(
        &mut self,
        route_id: Option<String>,
        profile_id: vibex_core::ModelProviderProfileId,
        lane: StageLane,
    ) -> usize {
        self.owners.push((route_id, profile_id, lane.model.clone()));
        self.lanes.push(lane);
        self.rows.push(StageRow::Lane(self.lanes.len() - 1));
        self.lanes.len() - 1
    }

    /// The lane a step lands on: the member of its route that names its
    /// Provider and model, else any lane of the Provider.
    fn find(
        &self,
        route_id: Option<&str>,
        profile_id: &vibex_core::ModelProviderProfileId,
        model: Option<&str>,
    ) -> Option<usize> {
        let owners = &self.owners;
        owners
            .iter()
            .position(|(route, profile, member_model)| {
                route.as_deref() == route_id
                    && profile == profile_id
                    && model.is_some_and(|model| model == member_model)
            })
            .or_else(|| {
                owners.iter().position(|(route, profile, _)| {
                    route.as_deref() == route_id && profile == profile_id
                })
            })
            .or_else(|| {
                owners
                    .iter()
                    .position(|(_, profile, _)| profile == profile_id)
            })
    }
}

impl ManagementCenter {
    fn workspace_route(&self, id: Option<&str>) -> Option<&vibex_core::ModelRoute> {
        let id = id?;
        self.model_routes
            .iter()
            .find(|route| route.id.as_str() == id)
    }

    fn workspace_provider_name(&self, id: &vibex_core::ModelProviderProfileId) -> String {
        self.providers
            .iter()
            .find(|profile| &profile.id == id)
            .map(|profile| profile.display_name.clone())
            .unwrap_or_else(|| {
                management_locale_text("a removed Provider", "已删除的供应商", "已刪除的供應商")
                    .to_string()
            })
    }

    fn workspace_credential_owner(
        &self,
        id: &vibex_core::RequestId,
    ) -> Option<vibex_core::ModelProviderProfileId> {
        self.providers
            .iter()
            .find(|profile| {
                profile
                    .credentials
                    .iter()
                    .any(|credential| &credential.id == id)
            })
            .map(|profile| profile.id.clone())
    }

    fn workspace_agent_name(&self, id: &str) -> String {
        if id == UNKNOWN_AGENT {
            management_locale_text("An Agent", "某个 Agent", "某個 Agent").to_string()
        } else {
            self.workspace_agent_label(id)
        }
    }

    /// The walk the map and the story tell for one staged request.
    fn staged_walk(&self, staged: &Staged) -> Vec<WalkStep> {
        match (&staged.trail, &staged.row) {
            (Some(trail), row) => trail_walk(trail, row.as_ref()),
            (None, Some(row)) => ledger_walk(row, |id| self.workspace_credential_owner(id)),
            (None, None) => Vec::new(),
        }
    }

    /// The requests the map shows now.
    ///
    /// A picked request alone. Otherwise every request running through the
    /// gateway and each Agent's latest one for a moment after it ends, for at
    /// most a few Agents; with nothing running, the newest recorded request.
    fn staged_requests(&self, now: i64) -> Vec<Staged> {
        let row_of = |id: &vibex_core::RequestId| {
            self.gateway_requests
                .iter()
                .find(|row| &row.request_id == id)
                .cloned()
        };
        let in_flight = |id: &vibex_core::RequestId| {
            self.gateway_inflight
                .iter()
                .any(|inflight| &inflight.request_id == id)
        };
        let on_stage = |trail: &GatewayRouteTrail| match trail.ended {
            None => in_flight(&trail.request_id),
            Some((at, _)) => now - at < LINGER_MS,
        };
        let mut trails = self
            .gateway_route_trails
            .iter()
            .filter(|trail| on_stage(trail))
            .cloned()
            .collect::<Vec<_>>();
        for inflight in &self.gateway_inflight {
            if !self
                .gateway_route_trails
                .iter()
                .any(|trail| trail.request_id == inflight.request_id)
            {
                trails.push(GatewayRouteTrail::from_inflight(inflight));
            }
        }

        if let Some(pinned) = self.gateway_route_pinned.as_ref() {
            if let Some(trail) = trails.iter().find(|trail| &trail.request_id == pinned) {
                return vec![Staged {
                    row: row_of(pinned),
                    trail: Some(trail.clone()),
                    clock: FlightClock::Live,
                }];
            }
            return row_of(pinned)
                .map(|row| Staged {
                    trail: None,
                    row: Some(row),
                    clock: FlightClock::Replay,
                })
                .into_iter()
                .collect();
        }

        trails.sort_by_key(GatewayRouteTrail::started_ms);
        // The Agents heard from most recently keep their place on the map.
        let mut agents = Vec::<Option<String>>::new();
        for trail in trails.iter().rev() {
            if !agents.contains(&trail.agent_id) && agents.len() < STAGE_AGENTS {
                agents.push(trail.agent_id.clone());
            }
        }
        // Of an Agent's finished requests, only its latest lingers.
        let mut latest_finished = HashMap::<Option<String>, vibex_core::RequestId>::new();
        for trail in trails.iter().filter(|trail| trail.ended.is_some()) {
            latest_finished.insert(trail.agent_id.clone(), trail.request_id.clone());
        }
        let staged = trails
            .into_iter()
            .filter(|trail| agents.contains(&trail.agent_id))
            .filter(|trail| {
                trail.ended.is_none()
                    || latest_finished.get(&trail.agent_id) == Some(&trail.request_id)
            })
            .map(|trail| Staged {
                row: row_of(&trail.request_id),
                trail: Some(trail),
                clock: FlightClock::Live,
            })
            .collect::<Vec<_>>();
        if !staged.is_empty() {
            return staged;
        }
        self.gateway_requests
            .iter()
            .max_by_key(|row| row.created_at_ms)
            .cloned()
            .map(|row| Staged {
                trail: None,
                row: Some(row),
                clock: FlightClock::Settled,
            })
            .into_iter()
            .collect()
    }

    /// One member's lane: its Provider, the model it is asked for, and how
    /// its Keys stand.
    fn workspace_stage_lane(
        &self,
        id: String,
        profile_id: &vibex_core::ModelProviderProfileId,
        model: &str,
        enabled: bool,
        now: i64,
    ) -> StageLane {
        let profile = self
            .providers
            .iter()
            .find(|profile| &profile.id == profile_id);
        let upstream = self
            .gateway
            .upstreams
            .iter()
            .find(|upstream| &upstream.model_provider_profile_id == profile_id);
        let keys = match upstream {
            Some(upstream) => upstream
                .keys
                .iter()
                .map(|key| StageKey {
                    id: key.credential_id.as_str().to_string(),
                    label: key_label(
                        key.credential_id.as_str(),
                        &key.display_name,
                        &key.masked_hint,
                    ),
                    tone: match key.state {
                        vibex_core::GatewayKeyState::Ready => LaneTone::Ready,
                        vibex_core::GatewayKeyState::Cooling { .. } => LaneTone::Cooling,
                        vibex_core::GatewayKeyState::Invalid => LaneTone::Down,
                        vibex_core::GatewayKeyState::Disabled => LaneTone::Off,
                    },
                })
                .collect::<Vec<_>>(),
            None => profile
                .map(|profile| {
                    profile
                        .credentials
                        .iter()
                        .map(|credential| StageKey {
                            id: credential.id.as_str().to_string(),
                            label: credential.display_label(),
                            tone: LaneTone::Ready,
                        })
                        .collect()
                })
                .unwrap_or_default(),
        };
        let ready = keys
            .iter()
            .filter(|key| key.tone == LaneTone::Ready)
            .count();
        let back_in = upstream
            .into_iter()
            .flat_map(|upstream| upstream.keys.iter())
            .filter_map(|key| match key.state {
                vibex_core::GatewayKeyState::Cooling {
                    until_ms: Some(until),
                    ..
                } if until > now => Some(until - now),
                _ => None,
            })
            .min();
        let (tone, note) = if !enabled {
            (
                LaneTone::Off,
                management_locale_text("turned off in the group", "已在组内停用", "已在組內停用")
                    .to_string(),
            )
        } else if keys.is_empty() {
            (
                LaneTone::Down,
                management_locale_text("no Key", "没有 Key", "沒有 Key").to_string(),
            )
        } else if ready > 0 {
            (
                LaneTone::Ready,
                if keys.len() == 1 {
                    management_locale_text("Key ready", "Key 可用", "Key 可用").to_string()
                } else {
                    format!(
                        "{ready}/{} {}",
                        keys.len(),
                        management_locale_text("Keys ready", "个 Key 可用", "個 Key 可用")
                    )
                },
            )
        } else if keys.iter().any(|key| key.tone == LaneTone::Cooling) {
            (
                LaneTone::Cooling,
                match back_in {
                    Some(ms) => format!(
                        "{} · {}",
                        management_locale_text("cooling", "冷却中", "冷卻中"),
                        said(
                            locale::current_locale(),
                            format!("back in {}", gateway_duration(ms)),
                            format!("{}后恢复", gateway_duration(ms)),
                            format!("{}後恢復", gateway_duration(ms)),
                        )
                    ),
                    None => management_locale_text("cooling", "冷却中", "冷卻中").to_string(),
                },
            )
        } else {
            (
                LaneTone::Down,
                management_locale_text("no usable Key", "没有可用的 Key", "沒有可用的 Key")
                    .to_string(),
            )
        };
        StageLane {
            id,
            marks: profile
                .map(|profile| {
                    [
                        profile.metadata.catalog_id.clone(),
                        profile.metadata.icon.clone(),
                        profile.vendor_hint.clone(),
                        Some(profile.display_name.clone()),
                    ]
                    .into_iter()
                    .flatten()
                    .collect()
                })
                .unwrap_or_default(),
            title: self.workspace_provider_name(profile_id),
            model: model.to_string(),
            keys,
            note,
            tone,
        }
    }

    /// Everything the route map draws for the staged requests.
    fn workspace_route_stage_model(&self, staged: &[Staged], now: i64) -> StageModel {
        let locale = locale::current_locale();
        let walks = staged
            .iter()
            .map(|staged| self.staged_walk(staged))
            .collect::<Vec<_>>();

        // The route groups on the map: those of the staged requests, or every
        // gateway group while there is nothing to show yet.
        let mut routes = Vec::<&vibex_core::ModelRoute>::new();
        for request in staged {
            if let Some(route) = self.workspace_route(request.route_id().as_deref())
                && !routes.iter().any(|known| known.id == route.id)
            {
                routes.push(route);
            }
        }
        if staged.is_empty() {
            routes = self
                .model_routes
                .iter()
                .filter(|route| route.kind.via_gateway())
                .take(3)
                .collect();
        }

        // Every member a step lands on is drawn; the rest fill the room left.
        let needed = staged
            .iter()
            .zip(&walks)
            .flat_map(|(request, walk)| {
                let route_id = request.route_id();
                walk.iter()
                    .map(move |step| (route_id.clone(), step.profile_id.clone()))
            })
            .collect::<Vec<_>>();
        let mut book = LaneBook::default();
        for route in &routes {
            if routes.len() > 1 {
                book.rows.push(StageRow::Group {
                    title: route.display_name.clone(),
                    detail: format!(
                        "{} · {} {}",
                        management_route_strategy_label(route.strategy),
                        route.members.len(),
                        management_locale_text("members", "个成员", "個成員")
                    ),
                });
            }
            let mut hidden = 0;
            for member in &route.members {
                let key = (
                    Some(route.id.as_str().to_string()),
                    member.model_provider_profile_id.clone(),
                );
                if needed.contains(&key) || book.lanes.len() < STAGE_LANES {
                    let lane = self.workspace_stage_lane(
                        member.id.as_str().to_string(),
                        &member.model_provider_profile_id,
                        &member.model_id,
                        member.enabled,
                        now,
                    );
                    book.push(key.0, key.1, lane);
                } else {
                    hidden += 1;
                }
            }
            if hidden > 0 {
                book.rows.push(StageRow::More(hidden));
            }
        }

        // The Agents, in the order they first appear.
        let mut agents = Vec::<StageAgent>::new();
        for request in staged {
            let id = request.agent_id().to_string();
            let detail = asked_model(request.requested_model()).to_string();
            match agents.iter_mut().find(|agent| agent.id == id) {
                Some(agent) => agent.detail = detail,
                None => agents.push(StageAgent {
                    label: self.workspace_agent_name(&id),
                    id,
                    detail,
                    idle: false,
                }),
            }
        }
        if staged.is_empty() {
            for binding in &self.agent_model_route_bindings {
                let bound = routes
                    .iter()
                    .any(|route| route.id == binding.model_route_id);
                let id = binding.agent_id.as_str().to_string();
                if bound
                    && !agents.iter().any(|agent| agent.id == id)
                    && agents.len() < STAGE_AGENTS
                {
                    agents.push(StageAgent {
                        label: self.workspace_agent_name(&id),
                        id,
                        detail: String::new(),
                        idle: true,
                    });
                }
            }
        }

        let mut flights = Vec::new();
        for (request, walk) in staged.iter().zip(&walks) {
            let Some(agent) = agents
                .iter()
                .position(|agent| agent.id == request.agent_id())
            else {
                continue;
            };
            let route_id = request.route_id();
            let served = request
                .row
                .as_ref()
                .and_then(|row| row.model_served.clone());
            let mut steps = Vec::with_capacity(walk.len());
            for step in walk {
                let model = (step.verdict == WalkVerdict::Answered)
                    .then_some(served.as_deref())
                    .flatten();
                let lane = match book.find(route_id.as_deref(), &step.profile_id, model) {
                    Some(lane) => lane,
                    None => {
                        let lane = self.workspace_stage_lane(
                            step.profile_id.as_str().to_string(),
                            &step.profile_id,
                            served.as_deref().unwrap_or_default(),
                            true,
                            now,
                        );
                        book.push(None, step.profile_id.clone(), lane)
                    }
                };
                // The Key by id when known; a name is matched against the
                // lane's Keys, since that is all the ledger keeps of it.
                let key = step.key_id.clone().or_else(|| {
                    let name = step.key_name.as_ref()?;
                    book.lanes[lane]
                        .keys
                        .iter()
                        .find(|key| &key.label == name)
                        .map(|key| key.id.clone())
                });
                steps.push(StageStep {
                    lane,
                    key,
                    verdict: stage_verdict(step.verdict, request.row.as_ref(), locale),
                    at_ms: step.at_ms,
                });
            }
            let ended = match (&request.trail, &request.row) {
                (Some(trail), _) => trail
                    .ended
                    .map(|(at, outcome)| (outcome_answered(outcome), Some(at))),
                (None, Some(row)) => Some((outcome_answered(row.outcome), None)),
                (None, None) => None,
            };
            flights.push(StageFlight {
                id: request
                    .request_id()
                    .map(|id| id.as_str().to_string())
                    .unwrap_or_default(),
                agent,
                steps,
                clock: request.clock,
                ended,
                streaming_since: request
                    .trail
                    .as_ref()
                    .and_then(|trail| trail.streaming_since),
            });
        }

        let running = staged.iter().filter(|request| request.running()).count();
        let status = &self.gateway.runtime.status;
        let hub = StageHub {
            title: gateway_display_name(&self.gateway.settings),
            detail: if running > 0 {
                said(
                    locale,
                    format!("{running} in flight"),
                    format!("{running} 个进行中"),
                    format!("{running} 個進行中"),
                )
            } else {
                management_locale_text("idle", "空闲", "閒置").to_string()
            },
            chip: match routes.as_slice() {
                [route] => Some(management_route_strategy_label(route.strategy)),
                [] => None,
                many => Some(said(
                    locale,
                    format!("{} groups", many.len()),
                    format!("{} 个路由组", many.len()),
                    format!("{} 個路由組", many.len()),
                )),
            },
            state: match status {
                vibex_core::ModelGatewayStatus::Running => HubState::Serving,
                vibex_core::ModelGatewayStatus::Degraded => HubState::Degraded,
                _ => HubState::Off,
            },
        };
        let note = book.rows.is_empty().then(|| {
            if routes.is_empty() && !staged.is_empty() {
                management_locale_text(
                    "The gateway refused it before any member was asked",
                    "网关在询问任何成员之前就拒绝了它",
                    "閘道在詢問任何成員之前就拒絕了它",
                )
                .to_string()
            } else {
                management_locale_text(
                    "No route group yet. Create one and give an Agent its model.",
                    "还没有路由组。新建一个并让 Agent 使用它的模型。",
                    "還沒有路由組。新增一個並讓 Agent 使用它的模型。",
                )
                .to_string()
            }
        });
        StageModel {
            agents,
            hub,
            lanes: book.lanes,
            rows: book.rows,
            flights,
            pinned: self
                .gateway_route_pinned
                .as_ref()
                .map(|id| id.as_str().to_string()),
            note,
        }
    }
}

/// One sentence written in each language, picked for the reader's.
fn said(locale: ResolvedLocale, en: String, zh_cn: String, zh_tw: String) -> String {
    match locale {
        ResolvedLocale::En => en,
        ResolvedLocale::ZhCn => zh_cn,
        ResolvedLocale::ZhTw => zh_tw,
    }
}

/// A walk step's verdict as the map tags it.
fn stage_verdict(
    verdict: WalkVerdict,
    row: Option<&vibex_core::GatewayRequestSummary>,
    locale: ResolvedLocale,
) -> StepVerdict {
    match verdict {
        WalkVerdict::Pending => StepVerdict::Pending,
        WalkVerdict::Answered => StepVerdict::Answered,
        WalkVerdict::Failed(kind) => StepVerdict::Failed(gateway_failure_text(kind, locale).into()),
        WalkVerdict::Skipped(kind) => {
            StepVerdict::Skipped(gateway_failure_text(kind, locale).into())
        }
        WalkVerdict::MovedOn => StepVerdict::Failed(
            management_locale_text_for(locale, "no answer", "未响应", "未回應").into(),
        ),
        WalkVerdict::Broke => StepVerdict::Failed(
            row.and_then(|row| row.status)
                .map(|status| format!("HTTP {status}"))
                .unwrap_or_else(|| {
                    management_locale_text_for(locale, "failed", "失败", "失敗").to_string()
                })
                .into(),
        ),
    }
}

// ----- the story -----------------------------------------------------------------------------

/// What a story line is about, which decides its mark.
#[derive(Clone, Copy, Debug, PartialEq)]
enum StoryKind {
    /// What was asked for, and of which group.
    Ask,
    /// Why the walk went the way it did.
    Why,
    /// A fact beside the walk.
    Aside,
    Answered,
    Refused,
    Waiting,
    /// The upstream's own words, verbatim.
    Said,
}

#[derive(Clone, Debug, PartialEq)]
struct StoryLine {
    kind: StoryKind,
    text: String,
}

impl StoryLine {
    fn new(kind: StoryKind, text: impl Into<String>) -> Self {
        Self {
            kind,
            text: text.into(),
        }
    }
}

/// The names a story needs, looked up once for the request it tells.
struct StoryNames {
    agent: String,
    model: String,
    /// The route group: its name, how it orders, how many members.
    route: Option<(String, String, usize)>,
    providers: HashMap<vibex_core::ModelProviderProfileId, String>,
    keys: HashMap<String, String>,
}

impl StoryNames {
    fn provider(&self, id: &vibex_core::ModelProviderProfileId, locale: ResolvedLocale) -> String {
        self.providers.get(id).cloned().unwrap_or_else(|| {
            management_locale_text_for(
                locale,
                "a removed Provider",
                "已删除的供应商",
                "已刪除的供應商",
            )
            .to_string()
        })
    }

    /// "Provider · Key", or the Provider alone when the Key is not known.
    fn place(&self, step: &WalkStep, locale: ResolvedLocale) -> String {
        let provider = self.provider(&step.profile_id, locale);
        let key = step.key_name.clone().or_else(|| {
            step.key_id
                .as_ref()
                .and_then(|id| self.keys.get(id).cloned())
        });
        match key {
            Some(key) => format!("{provider} · {key}"),
            None => provider,
        }
    }
}

/// The first line of every story: who asked for what, through which group.
fn story_ask(names: &StoryNames, locale: ResolvedLocale) -> StoryLine {
    let (agent, model) = (&names.agent, &names.model);
    let text = match &names.route {
        Some((route, strategy, members)) => said(
            locale,
            format!(
                "{agent} asked for {model} through the route group “{route}” ({strategy}, {members} members)"
            ),
            format!("{agent} 经路由组「{route}」请求 {model}（{strategy}，{members} 个成员）"),
            format!("{agent} 經路由組「{route}」請求 {model}（{strategy}，{members} 個成員）"),
        ),
        None => said(
            locale,
            format!("{agent} asked for {model}"),
            format!("{agent} 请求了 {model}"),
            format!("{agent} 請求了 {model}"),
        ),
    };
    StoryLine::new(StoryKind::Ask, text)
}

/// A wire protocol by its proper name.
fn wire_name(id: &str) -> &str {
    match id {
        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => "Chat Completions",
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "OpenAI Responses",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "Anthropic Messages",
        vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => "Google Gemini",
        other => other,
    }
}

/// A wire protocol as a list row's tag names it.
fn wire_short(id: &str) -> &str {
    match id {
        vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => "Chat",
        vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES => "Responses",
        vibex_core::WIRE_PROTOCOL_ANTHROPIC_MESSAGES => "Anthropic",
        vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI => "Gemini",
        other => other,
    }
}

/// The story of one recorded request, one line per decision.
fn recorded_story(
    row: &vibex_core::GatewayRequestSummary,
    walk: &[WalkStep],
    names: &StoryNames,
    locale: ResolvedLocale,
) -> Vec<StoryLine> {
    let mut lines = vec![story_ask(names, locale)];
    let route = row.route.as_ref();
    if let Some(decision) = route
        .and_then(|route| route.decision.as_ref())
        .filter(|decision| !decision.is_unremarkable())
    {
        let mut parts = Vec::new();
        if let Some(rule) = decision.rule_index {
            parts.push(said(
                locale,
                format!("rule {} matched", rule + 1),
                format!("命中规则 {}", rule + 1),
                format!("命中規則 {}", rule + 1),
            ));
        }
        if let Some(intent) = decision.intent.as_deref() {
            parts.push(said(
                locale,
                format!("intent {intent}"),
                format!("意图 {intent}"),
                format!("意圖 {intent}"),
            ));
        }
        if let Some(effort) = decision.effort.as_deref() {
            parts.push(said(
                locale,
                format!("effort {effort}"),
                format!("推理档位 {effort}"),
                format!("推理檔位 {effort}"),
            ));
        }
        if decision.retries > 0 {
            parts.push(said(
                locale,
                format!("the whole walk was retried {}×", decision.retries),
                format!("整轮重试了 {} 次", decision.retries),
                format!("整輪重試了 {} 次", decision.retries),
            ));
        }
        if let Some(error) = decision.classifier_error.as_deref() {
            parts.push(said(
                locale,
                format!("the classifier failed, so the default order was used ({error})"),
                format!("分类失败，按默认顺序（{error}）"),
                format!("分類失敗，按預設順序（{error}）"),
            ));
        }
        if !parts.is_empty() {
            lines.push(StoryLine::new(StoryKind::Why, parts.join(" · ")));
        }
    }
    if let Some(route) = route
        && (route.skipped_cooling > 0 || route.skipped_over_limit > 0)
    {
        let mut parts = Vec::new();
        if route.skipped_cooling > 0 {
            parts.push(said(
                locale,
                format!("{} cooling", route.skipped_cooling),
                format!("{} 个冷却中", route.skipped_cooling),
                format!("{} 個冷卻中", route.skipped_cooling),
            ));
        }
        if route.skipped_over_limit > 0 {
            parts.push(said(
                locale,
                format!("{} over their token ceiling", route.skipped_over_limit),
                format!("{} 个超出 Token 上限", route.skipped_over_limit),
                format!("{} 個超出 Token 上限", route.skipped_over_limit),
            ));
        }
        lines.push(StoryLine::new(
            StoryKind::Aside,
            said(
                locale,
                format!("Keys set aside before trying: {}", parts.join(", ")),
                format!("尝试前已跳过的 Key：{}", parts.join("，")),
                format!("嘗試前已跳過的 Key：{}", parts.join("，")),
            ),
        ));
    }
    if walk.is_empty() {
        let mut text = said(
            locale,
            "The gateway refused it before asking any member".to_string(),
            "网关在询问任何成员之前就拒绝了它".to_string(),
            "閘道在詢問任何成員之前就拒絕了它".to_string(),
        );
        if let Some(status) = row.status {
            text.push_str(&format!(" · HTTP {status}"));
        }
        if let Some(code) = gateway_request_error_code(row) {
            text.push_str(&format!(" · {code}"));
        }
        lines.push(StoryLine::new(StoryKind::Refused, text));
    }
    for (index, step) in walk.iter().enumerate() {
        let place = names.place(step, locale);
        let last = index + 1 == walk.len();
        match step.verdict {
            WalkVerdict::Failed(kind) => {
                let failure = gateway_failure_text(kind, locale);
                let rest = match kind.cooldown_ms() {
                    Some(ms) => said(
                        locale,
                        format!(", rests {}", gateway_duration(ms)),
                        format!("，冷却 {}", gateway_duration(ms)),
                        format!("，冷卻 {}", gateway_duration(ms)),
                    ),
                    None => management_locale_text_for(
                        locale,
                        ", marked invalid",
                        "，已标记为失效",
                        "，已標記為失效",
                    )
                    .to_string(),
                };
                let then = if last {
                    management_locale_text_for(locale, "", "", "")
                } else {
                    management_locale_text_for(
                        locale,
                        "; the gateway went on",
                        "；网关转到下一个",
                        "；閘道轉到下一個",
                    )
                };
                lines.push(StoryLine::new(
                    StoryKind::Refused,
                    format!("{place}: {failure}{rest}{then}"),
                ));
            }
            WalkVerdict::Skipped(kind) => lines.push(StoryLine::new(
                StoryKind::Refused,
                said(
                    locale,
                    format!(
                        "{place} was passed over: {}",
                        gateway_failure_text(kind, locale)
                    ),
                    format!("跳过了 {place}：{}", gateway_failure_text(kind, locale)),
                    format!("跳過了 {place}：{}", gateway_failure_text(kind, locale)),
                ),
            )),
            WalkVerdict::MovedOn => lines.push(StoryLine::new(
                StoryKind::Refused,
                said(
                    locale,
                    format!("{place} did not answer; the gateway went on"),
                    format!("{place} 未响应，网关转到下一个"),
                    format!("{place} 未回應，閘道轉到下一個"),
                ),
            )),
            WalkVerdict::Answered => {
                let mut parts = vec![match row.latency_ms {
                    Some(ms) => said(
                        locale,
                        format!("{place} answered in {}", gateway_duration(ms)),
                        format!("{place} 用时 {} 完成响应", gateway_duration(ms)),
                        format!("{place} 用時 {} 完成回應", gateway_duration(ms)),
                    ),
                    None => said(
                        locale,
                        format!("{place} answered"),
                        format!("{place} 完成响应"),
                        format!("{place} 完成回應"),
                    ),
                }];
                if let Some(served) = row.model_served.as_deref()
                    && served != asked_model(&row.model_requested)
                {
                    parts.push(said(
                        locale,
                        format!("as {served}"),
                        format!("实际模型 {served}"),
                        format!("實際模型 {served}"),
                    ));
                }
                if row.total_tokens > 0 {
                    parts.push(said(
                        locale,
                        format!(
                            "{} in / {} out",
                            gateway_token_count(row.input_tokens),
                            gateway_token_count(row.output_tokens)
                        ),
                        format!(
                            "输入 {} / 输出 {}",
                            gateway_token_count(row.input_tokens),
                            gateway_token_count(row.output_tokens)
                        ),
                        format!(
                            "輸入 {} / 輸出 {}",
                            gateway_token_count(row.input_tokens),
                            gateway_token_count(row.output_tokens)
                        ),
                    ));
                }
                if let Some(share) = cache_share(row) {
                    parts.push(said(
                        locale,
                        format!("{share}% from cache"),
                        format!("缓存命中 {share}%"),
                        format!("快取命中 {share}%"),
                    ));
                }
                if row.wire_in != row.wire_out {
                    parts.push(format!(
                        "{} → {}",
                        wire_name(&row.wire_in),
                        wire_name(&row.wire_out)
                    ));
                }
                lines.push(StoryLine::new(StoryKind::Answered, parts.join(" · ")));
            }
            WalkVerdict::Broke => {
                let mut text = said(
                    locale,
                    format!("{place} did not answer and the request failed"),
                    format!("{place} 未能完成响应，请求失败"),
                    format!("{place} 未能完成回應，請求失敗"),
                );
                if let Some(status) = row.status {
                    text.push_str(&format!(" · HTTP {status}"));
                }
                if let Some(code) = gateway_request_error_code(row) {
                    text.push_str(&format!(" · {code}"));
                }
                lines.push(StoryLine::new(StoryKind::Refused, text));
            }
            WalkVerdict::Pending => {}
        }
    }
    if !outcome_answered(row.outcome)
        && let Some(message) = route.and_then(|route| route.error_message.as_deref())
    {
        let message = message.chars().take(600).collect::<String>();
        lines.push(StoryLine::new(
            StoryKind::Said,
            said(
                locale,
                format!("It said: {message}"),
                format!("上游原话：{message}"),
                format!("上游原話：{message}"),
            ),
        ));
    }
    lines
}

/// The story of a request still running, as far as the gateway has told it.
fn live_story(
    trail: &GatewayRouteTrail,
    walk: &[WalkStep],
    names: &StoryNames,
    now: i64,
    locale: ResolvedLocale,
) -> Vec<StoryLine> {
    let mut lines = vec![story_ask(names, locale)];
    for step in walk {
        let place = names.place(step, locale);
        let line = match step.verdict {
            WalkVerdict::Failed(kind) => StoryLine::new(
                StoryKind::Refused,
                said(
                    locale,
                    format!(
                        "{place}: {}; the gateway went on",
                        gateway_failure_text(kind, locale)
                    ),
                    format!(
                        "{place}：{}，网关转到下一个",
                        gateway_failure_text(kind, locale)
                    ),
                    format!(
                        "{place}：{}，閘道轉到下一個",
                        gateway_failure_text(kind, locale)
                    ),
                ),
            ),
            WalkVerdict::MovedOn | WalkVerdict::Skipped(_) => StoryLine::new(
                StoryKind::Refused,
                said(
                    locale,
                    format!("{place} did not answer; the gateway went on"),
                    format!("{place} 未响应，网关转到下一个"),
                    format!("{place} 未回應，閘道轉到下一個"),
                ),
            ),
            WalkVerdict::Pending => {
                let since = step.at_ms.unwrap_or(now);
                let waited = gateway_duration((now - since).max(0));
                match trail.streaming_since {
                    Some(first) => StoryLine::new(
                        StoryKind::Waiting,
                        said(
                            locale,
                            format!(
                                "{place} is answering · first token after {} · {waited}",
                                gateway_duration((first - since).max(0))
                            ),
                            format!(
                                "{place} 正在输出 · 首 Token 用时 {} · 已 {waited}",
                                gateway_duration((first - since).max(0))
                            ),
                            format!(
                                "{place} 正在輸出 · 首 Token 用時 {} · 已 {waited}",
                                gateway_duration((first - since).max(0))
                            ),
                        ),
                    ),
                    None => StoryLine::new(
                        StoryKind::Waiting,
                        said(
                            locale,
                            format!("Waiting for {place} · {waited}"),
                            format!("等待 {place} 响应 · 已 {waited}"),
                            format!("等待 {place} 回應 · 已 {waited}"),
                        ),
                    ),
                }
            }
            WalkVerdict::Answered => StoryLine::new(
                StoryKind::Answered,
                said(
                    locale,
                    format!("{place} answered"),
                    format!("{place} 完成响应"),
                    format!("{place} 完成回應"),
                ),
            ),
            WalkVerdict::Broke => StoryLine::new(
                StoryKind::Refused,
                said(
                    locale,
                    format!("{place} did not answer and the request failed"),
                    format!("{place} 未能完成响应，请求失败"),
                    format!("{place} 未能完成回應，請求失敗"),
                ),
            ),
        };
        lines.push(line);
    }
    lines
}

/// The share of the prompt read from the vendor's cache, in whole percent.
fn cache_share(row: &vibex_core::GatewayRequestSummary) -> Option<u64> {
    let prompt = row.input_tokens.saturating_add(row.cached_read_tokens);
    (row.cached_read_tokens > 0 && prompt > 0)
        .then(|| (row.cached_read_tokens * 100 + prompt / 2) / prompt)
}

/// Output tokens a second, when the reply was long enough to say.
fn output_speed(row: &vibex_core::GatewayRequestSummary) -> Option<u64> {
    let ms = row.latency_ms.filter(|ms| *ms >= 250)?;
    (row.output_tokens >= 16).then(|| row.output_tokens * 1000 / ms as u64)
}

/// A token count short enough for a list row: 512, 12.3k, 1.2M.
fn compact_tokens(tokens: u64) -> String {
    match tokens {
        0..1_000 => tokens.to_string(),
        1_000..1_000_000 => format!("{:.1}k", tokens as f64 / 1_000.0),
        _ => format!("{:.1}M", tokens as f64 / 1_000_000.0),
    }
}

// ----- the tab -------------------------------------------------------------------------------

impl ManagementCenter {
    pub(super) fn render_workspace_gateway_routes(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let now = vibex_core::unix_timestamp_ms();
        // A pinned request nobody can see any more unpins: the map must not
        // describe a request that is in no list.
        if let Some(pinned) = self.gateway_route_pinned.clone()
            && !self
                .gateway_requests
                .iter()
                .any(|request| request.request_id == pinned)
            && !self
                .gateway_inflight
                .iter()
                .any(|request| request.request_id == pinned)
            && !self
                .gateway_route_trails
                .iter()
                .any(|trail| trail.request_id == pinned)
        {
            self.gateway_route_pinned = None;
        }
        let staged = self.staged_requests(now);
        let model = self.workspace_route_stage_model(&staged, now);
        let stage = self
            .gateway_route_stage
            .get_or_insert_with(|| cx.new(|_| RouteStage::new()))
            .clone();
        stage.update(cx, |stage, _| stage.set_model(model));

        let map = v_flex()
            .w_full()
            .min_w_0()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(cx.theme().border)
            .bg(theme::semantic_color("card", cx.theme().is_dark()).opacity(0.72))
            .child(self.render_workspace_route_head(&staged, cx))
            .child(div().w_full().px_3().child(stage))
            .child(self.render_workspace_route_story(&staged, now, cx));
        ws_stack(vec![
            map.into_any_element(),
            self.render_workspace_route_list(now, cx),
        ])
    }

    /// Picks a request on the map, or plays the one already picked again.
    fn pick_gateway_route(&mut self, id: vibex_core::RequestId, cx: &mut Context<Self>) {
        if self.gateway_route_pinned.as_ref() == Some(&id) {
            if let Some(stage) = self.gateway_route_stage.clone() {
                stage.update(cx, |stage, cx| stage.replay(cx));
            }
        } else {
            self.gateway_route_pinned = Some(id);
        }
        cx.notify();
    }

    /// The map's heading: live or pinned, and the ledger's counts.
    fn render_workspace_route_head(&self, staged: &[Staged], cx: &mut Context<Self>) -> AnyElement {
        let locale = locale::current_locale();
        let running = staged.iter().filter(|request| request.running()).count();
        let pinned = self.gateway_route_pinned.is_some();
        let state = if pinned {
            Tag::color(ColorName::Blue)
                .xsmall()
                .child(management_locale_text("Picked", "已选中", "已選取"))
                .into_any_element()
        } else {
            ws_state_fact(
                management_locale_text("Live", "实时", "即時"),
                if running > 0 {
                    cx.theme().success
                } else {
                    cx.theme().muted_foreground
                },
            )
        };
        let note = if pinned {
            if staged.iter().any(Staged::running) {
                management_locale_text(
                    "Following the request picked below",
                    "正在跟随下方选中的请求",
                    "正在跟隨下方選取的請求",
                )
                .to_string()
            } else {
                management_locale_text(
                    "Replaying the request picked below",
                    "正在回放下方选中的请求",
                    "正在回放下方選取的請求",
                )
                .to_string()
            }
        } else if running > 0 {
            said(
                locale,
                format!("{running} in flight, drawn as the gateway routes them"),
                format!("{running} 个请求进行中，按网关的实际路由实时绘制"),
                format!("{running} 個請求進行中，按閘道的實際路由即時繪製"),
            )
        } else if staged.is_empty() {
            management_locale_text(
                "Waiting for the first request through the gateway",
                "等待第一个经过网关的请求",
                "等待第一個經過閘道的請求",
            )
            .to_string()
        } else {
            management_locale_text(
                "Nothing in flight; showing the last request",
                "当前没有进行中的请求，显示最近一次",
                "目前沒有進行中的請求，顯示最近一次",
            )
            .to_string()
        };

        let owner_of = |id: &vibex_core::RequestId| self.workspace_credential_owner(id);
        let total = self.gateway_requests.len();
        let rerouted = self
            .gateway_requests
            .iter()
            .filter(|row| outcome_answered(row.outcome))
            .filter(|row| walk_moves(&ledger_walk(row, owner_of)) > 0)
            .count();
        let failed = self
            .gateway_requests
            .iter()
            .filter(|row| !outcome_answered(row.outcome))
            .count();
        let stat = |value: usize, label: &'static str, color: Hsla| {
            h_flex()
                .flex_none()
                .items_baseline()
                .gap_1()
                .child(
                    div()
                        .text_sm()
                        .font_semibold()
                        .text_color(color)
                        .child(value.to_string()),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(label),
                )
        };
        let stats = h_flex()
            .flex_none()
            .items_center()
            .gap_4()
            .child(stat(
                total,
                management_locale_text("requests", "请求", "請求"),
                cx.theme().foreground,
            ))
            .child(stat(
                rerouted,
                management_locale_text("rerouted", "转移", "轉移"),
                cx.theme().foreground,
            ))
            .child(if failed > 0 {
                Button::new("management-route-latest-failure")
                    .xsmall()
                    .ghost()
                    .label(said(
                        locale,
                        format!("{failed} failed"),
                        format!("{failed} 失败"),
                        format!("{failed} 失敗"),
                    ))
                    .tooltip(management_locale_text(
                        "Show the latest request that failed",
                        "查看最近一次失败的请求",
                        "查看最近一次失敗的請求",
                    ))
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(row) = this
                            .gateway_requests
                            .iter()
                            .filter(|row| !outcome_answered(row.outcome))
                            .max_by_key(|row| row.created_at_ms)
                        {
                            let id = row.request_id.clone();
                            this.pick_gateway_route(id, cx);
                        }
                    }))
                    .into_any_element()
            } else {
                stat(
                    0,
                    management_locale_text("failed", "失败", "失敗"),
                    cx.theme().foreground,
                )
                .into_any_element()
            });

        h_flex()
            .w_full()
            .min_w_0()
            .flex_wrap()
            .items_center()
            .justify_between()
            .gap_x_4()
            .gap_y_2()
            .px_4()
            .pt_3()
            .child(
                h_flex()
                    .min_w_0()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .text_sm()
                            .font_semibold()
                            .child(management_locale_text("Route map", "路由图", "路由圖")),
                    )
                    .child(div().flex_none().child(state))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(note),
                    ),
            )
            .child(stats)
            .into_any_element()
    }

    /// The request the story tells: the picked one, else the newest on the map.
    fn workspace_story_names(&self, staged: &Staged) -> StoryNames {
        let route = self.workspace_route(staged.route_id().as_deref());
        let mut providers = HashMap::new();
        for profile in &self.providers {
            providers.insert(profile.id.clone(), profile.display_name.clone());
        }
        let mut keys = HashMap::new();
        for credential in self
            .providers
            .iter()
            .flat_map(|profile| profile.credentials.iter())
        {
            keys.insert(
                credential.id.as_str().to_string(),
                credential.display_label(),
            );
        }
        StoryNames {
            agent: self.workspace_agent_name(staged.agent_id()),
            model: asked_model(staged.requested_model()).to_string(),
            route: route.map(|route| {
                (
                    route.display_name.clone(),
                    management_route_strategy_label(route.strategy),
                    route.members.iter().filter(|member| member.enabled).count(),
                )
            }),
            providers,
            keys,
        }
    }

    fn render_workspace_route_story(
        &self,
        staged: &[Staged],
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = locale::current_locale();
        // The newest running request leads; else the one on the map.
        let Some(focus) = staged
            .iter()
            .rev()
            .find(|request| request.running())
            .or(staged.last())
        else {
            return div()
                .w_full()
                .px_4()
                .py_3()
                .border_t_1()
                .border_color(cx.theme().border)
                .text_xs()
                .text_color(cx.theme().muted_foreground)
                .child(management_locale_text(
                    "Each request an Agent sends through the gateway shows up here: which member was tried first, every Key that refused and why, and who answered.",
                    "Agent 经过网关的每个请求都会显示在这里：先尝试了哪个成员、哪些 Key 拒绝了它以及原因、最终由谁响应。",
                    "Agent 經過閘道的每個請求都會顯示在這裡：先嘗試了哪個成員、哪些 Key 拒絕了它以及原因、最終由誰回應。",
                ))
                .into_any_element();
        };
        let names = self.workspace_story_names(focus);
        let walk = self.staged_walk(focus);
        let (lines, at_ms) = match (&focus.trail, &focus.row) {
            (_, Some(row)) if !focus.running() => (
                recorded_story(
                    row,
                    &ledger_walk(row, |id| self.workspace_credential_owner(id)),
                    &names,
                    locale,
                ),
                row.created_at_ms,
            ),
            (Some(trail), _) => (
                live_story(trail, &walk, &names, now, locale),
                trail.started_ms(),
            ),
            (None, _) => (Vec::new(), now),
        };
        let pinned = self.gateway_route_pinned.is_some();
        let title = if focus.running() {
            management_locale_text("Routing now", "正在路由", "正在路由")
        } else if pinned {
            management_locale_text(
                "How this request was routed",
                "这次请求的路由经过",
                "這次請求的路由經過",
            )
        } else {
            management_locale_text(
                "How the last request was routed",
                "最近一次请求的路由经过",
                "最近一次請求的路由經過",
            )
        };
        let replayable = focus.row.is_some() && !focus.running();
        let focus_id = focus.request_id().cloned();
        let actions = h_flex()
            .flex_none()
            .items_center()
            .gap_1()
            .when(replayable, |actions| {
                actions.child(
                    Button::new("management-route-replay")
                        .xsmall()
                        .ghost()
                        .icon(Icon::default().path("icons/vibex/rotate-ccw.svg"))
                        .label(management_locale_text("Replay", "重放", "重播"))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            if let Some(id) = focus_id.clone() {
                                if this.gateway_route_pinned.as_ref() == Some(&id) {
                                    this.pick_gateway_route(id, cx);
                                } else {
                                    this.gateway_route_pinned = Some(id);
                                    cx.notify();
                                }
                            }
                        })),
                )
            })
            .when(pinned, |actions| {
                actions.child(
                    Button::new("management-route-live")
                        .xsmall()
                        .outline()
                        .label(management_locale_text(
                            "Back to live",
                            "回到实时",
                            "回到即時",
                        ))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.gateway_route_pinned = None;
                            cx.notify();
                        })),
                )
            });

        let mut list = v_flex().w_full().min_w_0().gap_1p5();
        for line in lines {
            let mark = match line.kind {
                StoryKind::Ask => div().size(px(6.0)).rounded_full().bg(cx.theme().foreground),
                StoryKind::Why => div().size(px(6.0)).rounded_full().bg(cx.theme().primary),
                StoryKind::Aside | StoryKind::Said => div()
                    .size(px(6.0))
                    .rounded_full()
                    .border_1()
                    .border_color(cx.theme().muted_foreground),
                StoryKind::Answered => div().size(px(6.0)).rounded_full().bg(cx.theme().success),
                StoryKind::Refused => div().size(px(6.0)).rounded_full().bg(cx.theme().danger),
                StoryKind::Waiting => div().size(px(6.0)).rounded_full().bg(cx.theme().info),
            };
            let quiet = matches!(line.kind, StoryKind::Aside | StoryKind::Said);
            list = list.child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_start()
                    .gap_2()
                    .child(div().flex_none().pt(px(6.0)).child(mark))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_sm()
                            .text_color(if quiet {
                                cx.theme().muted_foreground
                            } else {
                                cx.theme().foreground
                            })
                            .when(line.kind == StoryKind::Said, |text| {
                                text.text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                            })
                            .child(line.text),
                    ),
            );
        }

        v_flex()
            .w_full()
            .min_w_0()
            .gap_2()
            .px_4()
            .py_3()
            .border_t_1()
            .border_color(cx.theme().border)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .items_center()
                    .justify_between()
                    .gap_3()
                    .child(
                        h_flex()
                            .min_w_0()
                            .items_center()
                            .gap_2()
                            .child(div().flex_none().text_xs().font_semibold().child(title))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_family(cx.theme().mono_font_family.clone())
                                    .text_color(cx.theme().muted_foreground)
                                    .child(gateway_clock(at_ms)),
                            ),
                    )
                    .child(actions),
            )
            .child(list)
            .into_any_element()
    }

    // ----- the records -------------------------------------------------------------------

    fn render_workspace_route_list(&mut self, now: i64, cx: &mut Context<Self>) -> AnyElement {
        let pending = self.mutation.is_some();
        let query = self
            .gateway_request_search
            .read(cx)
            .value()
            .trim()
            .to_lowercase();
        let active = self.gateway_request_filter;
        let center = cx.weak_entity();
        let filter = Button::new("management-gateway-request-filter")
            .small()
            .outline()
            .label(active.label())
            .dropdown_caret(true)
            .dropdown_menu(move |mut menu, _, _| {
                for filter in GatewayRequestFilter::ALL {
                    let center = center.clone();
                    menu = menu.item(
                        PopupMenuItem::new(filter.label())
                            .checked(filter == active)
                            .on_click(move |_, _, cx| {
                                center
                                    .update(cx, |this, cx| {
                                        this.gateway_request_filter = filter;
                                        cx.notify();
                                    })
                                    .ok();
                            }),
                    );
                }
                menu
            });
        let owner_of = |id: &vibex_core::RequestId| self.workspace_credential_owner(id);
        let matches_query = |agent: Option<&str>, model: &str, provider: &str| {
            query.is_empty()
                || model.to_lowercase().contains(&query)
                || provider.to_lowercase().contains(&query)
                || agent.is_some_and(|agent| {
                    agent.to_lowercase().contains(&query)
                        || self
                            .workspace_agent_label(agent)
                            .to_lowercase()
                            .contains(&query)
                })
        };
        let live = self
            .gateway_inflight
            .iter()
            .rev()
            .filter(|_| matches!(active, GatewayRequestFilter::All))
            .filter(|request| {
                matches_query(
                    request.agent_id.as_deref(),
                    &request.requested_model,
                    &self.workspace_provider_name(&request.model_provider_profile_id),
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        let mut done = self
            .gateway_requests
            .iter()
            .filter(|request| match active {
                GatewayRequestFilter::All => true,
                GatewayRequestFilter::Ok => {
                    outcome_answered(request.outcome)
                        && walk_moves(&ledger_walk(request, owner_of)) == 0
                }
                GatewayRequestFilter::Failover => {
                    outcome_answered(request.outcome)
                        && walk_moves(&ledger_walk(request, owner_of)) > 0
                }
                GatewayRequestFilter::Failed => !outcome_answered(request.outcome),
            })
            .filter(|request| {
                matches_query(
                    request.agent_id.as_deref(),
                    &request.model_requested,
                    &request.provider_name,
                )
            })
            .cloned()
            .collect::<Vec<_>>();
        done.sort_by_key(|request| std::cmp::Reverse(request.created_at_ms));

        let rows = live
            .into_iter()
            .map(RouteListRow::Live)
            .chain(
                done.into_iter()
                    .map(|done| RouteListRow::Done(Box::new(done))),
            )
            .collect::<Vec<_>>();
        let shown = rows.len();
        let content = if rows.is_empty() {
            ws_empty_line(
                if self.gateway_requests.is_empty() && self.gateway_inflight.is_empty() {
                    management_locale_text(
                        "No request yet. Once an Agent goes through the gateway, each request is listed here without its prompt or reply.",
                        "还没有请求。Agent 经过网关后，每个请求都会列在这里，但不含提示词和回复。",
                        "還沒有請求。Agent 經過閘道後，每個請求都會列在這裡，但不含提示詞和回覆。",
                    )
                } else {
                    management_locale_text(
                        "No request matches. Clear the search or the filter.",
                        "没有匹配的请求，请清除搜索或筛选。",
                        "沒有符合的請求，請清除搜尋或篩選。",
                    )
                },
                cx,
            )
        } else {
            let height = (rows.len() as f32 * ROUTE_ROW_H).clamp(ROUTE_ROW_H * 3.0, 480.0);
            let sizes = Rc::new(
                rows.iter()
                    .map(|_| gpui_size(px(0.0), px(ROUTE_ROW_H)))
                    .collect::<Vec<GpuiSize<gpui::Pixels>>>(),
            );
            let rows = Rc::new(rows);
            div()
                .w_full()
                .h(px(height))
                .min_h_0()
                .child(v_virtual_list(
                    cx.entity(),
                    "management-gateway-request-virtual-list",
                    sizes,
                    move |this, range: Range<usize>, _, cx| {
                        range
                            .filter_map(|index| {
                                let row = rows.get(index)?;
                                Some(this.render_workspace_routes_list_row(row, now, cx))
                            })
                            .collect()
                    },
                ))
                .into_any_element()
        };
        let foot = h_flex()
            .w_full()
            .min_w_0()
            .items_center()
            .justify_between()
            .gap_3()
            .pt_3()
            .child(
                div()
                    .min_w_0()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "The gateway keeps 7 days of metadata and lists the latest 50. It never contains the prompt, the reply, a Key, or image content.",
                        "网关保留最近 7 天的请求元数据，这里列出最新 50 条；不含提示词、回复、Key 和图片内容。",
                        "閘道保留最近 7 天的請求中繼資料，這裡列出最新 50 筆；不含提示詞、回覆、Key 和圖片內容。",
                    )),
            )
            .when(!self.gateway_requests.is_empty(), |foot| {
                foot.child(
                    Button::new("management-gateway-clear-requests")
                        .small()
                        .ghost()
                        .label(management_locale_text("Clear records…", "清除记录…", "清除記錄…"))
                        .disabled(pending)
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.confirm_clear_gateway_requests(window, cx)
                        })),
                )
            });
        let count = said(
            locale::current_locale(),
            format!("{shown} shown"),
            format!("显示 {shown} 条"),
            format!("顯示 {shown} 筆"),
        );
        ws_section(
            management_locale_text("Requests", "请求记录", "請求記錄"),
            Some(management_locale_text(
                "Pick a request to see its route on the map above; pick it again to replay it.",
                "选中一条请求即可在上方路由图查看它的路由，再次选中会重新回放。",
                "選取一筆請求即可在上方路由圖查看它的路由，再次選取會重新播放。",
            )),
            Some(
                h_flex()
                    .items_center()
                    .gap_2()
                    .child(
                        div()
                            .flex_none()
                            .text_xs()
                            .text_color(cx.theme().muted_foreground)
                            .child(count),
                    )
                    .child(div().w(rems(14.0)).child(management_search_input_compact(
                        &self.gateway_request_search,
                        cx,
                    )))
                    .child(filter)
                    .into_any_element(),
            ),
            v_flex()
                .w_full()
                .child(route_list_header(cx))
                .child(content)
                .child(foot)
                .into_any_element(),
            cx,
        )
    }

    fn render_workspace_routes_list_row(
        &self,
        row: &RouteListRow,
        now: i64,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let locale = locale::current_locale();
        let id = row.id().clone();
        let selected = self.gateway_route_pinned.as_ref() == Some(&id);
        let (at_ms, agent, requested) = match row {
            RouteListRow::Live(live) => (
                live.started_at_ms,
                live.agent_id.clone(),
                live.requested_model.clone(),
            ),
            RouteListRow::Done(done) => (
                done.created_at_ms,
                done.agent_id.clone(),
                done.model_requested.clone(),
            ),
        };
        let agent_key = agent.clone().unwrap_or_else(|| UNKNOWN_AGENT.to_string());
        let hue = agent_hue(&agent_key, cx);
        let route = vibex_core::route_group_model_ref(&requested)
            .and_then(|parsed| self.workspace_route(Some(parsed.group_id)))
            .map(|route| route.display_name.clone());

        // Who answered, how it ended, and what it cost.
        let failed = matches!(row, RouteListRow::Done(done) if !outcome_answered(done.outcome));
        let mut tags = Vec::<AnyElement>::new();
        let (dot, who, detail, metrics) = match row {
            RouteListRow::Live(live) => {
                let waited = (now - live.started_at_ms).max(0);
                let provider = self.workspace_provider_name(&live.model_provider_profile_id);
                let state = if live.first_token_at_ms.is_some() {
                    management_locale_text("streaming", "正在输出", "正在輸出")
                } else {
                    management_locale_text("waiting", "等待响应", "等待回應")
                };
                if live.attempt > 1 {
                    tags.push(
                        Tag::color(ColorName::Blue)
                            .xsmall()
                            .child(said(
                                locale,
                                format!("member {}", live.attempt),
                                format!("第 {} 个成员", live.attempt),
                                format!("第 {} 個成員", live.attempt),
                            ))
                            .into_any_element(),
                    );
                }
                let mut metrics = vec![gateway_duration(waited)];
                if let Some(first) = live.first_token_at_ms {
                    metrics.push(said(
                        locale,
                        format!(
                            "first token {}",
                            gateway_duration((first - live.started_at_ms).max(0))
                        ),
                        format!(
                            "首 Token {}",
                            gateway_duration((first - live.started_at_ms).max(0))
                        ),
                        format!(
                            "首 Token {}",
                            gateway_duration((first - live.started_at_ms).max(0))
                        ),
                    ));
                }
                (
                    (cx.theme().info, cx.theme().foreground),
                    provider,
                    state.to_string(),
                    metrics,
                )
            }
            RouteListRow::Done(done) => {
                let walk = ledger_walk(done, |id| self.workspace_credential_owner(id));
                let moves = walk_moves(&walk);
                let answered = outcome_answered(done.outcome);
                let dot = if !answered {
                    (cx.theme().danger, cx.theme().danger)
                } else if moves > 0 {
                    (cx.theme().primary, cx.theme().foreground)
                } else {
                    (cx.theme().success, cx.theme().foreground)
                };
                if answered && moves > 0 {
                    tags.push(
                        Tag::color(ColorName::Blue)
                            .xsmall()
                            .child(said(
                                locale,
                                format!("rerouted ×{moves}"),
                                format!("转移 ×{moves}"),
                                format!("轉移 ×{moves}"),
                            ))
                            .into_any_element(),
                    );
                }
                if done.wire_in != done.wire_out {
                    tags.push(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(cx.theme().muted_foreground)
                            .child(format!(
                                "{} → {}",
                                wire_short(&done.wire_in),
                                wire_short(&done.wire_out)
                            ))
                            .into_any_element(),
                    );
                }
                let who = if done.outcome == vibex_core::GatewayRequestOutcome::Rejected {
                    management_locale_text("gateway", "网关", "閘道").to_string()
                } else {
                    done.provider_name.clone()
                };
                let detail = if answered {
                    let key = gateway_credential_name(done.credential_name.as_deref(), locale);
                    match done.model_served.as_deref() {
                        Some(served) if served != asked_model(&done.model_requested) => {
                            format!("{key} · {served}")
                        }
                        _ => key.to_string(),
                    }
                } else {
                    let failure = walk.iter().rev().find_map(|step| match step.verdict {
                        WalkVerdict::Failed(kind) | WalkVerdict::Skipped(kind) => {
                            Some(gateway_failure_text(kind, locale).to_string())
                        }
                        _ => None,
                    });
                    [
                        done.status.map(|status| format!("HTTP {status}")),
                        failure.or_else(|| gateway_request_error_code(done)),
                    ]
                    .into_iter()
                    .flatten()
                    .collect::<Vec<_>>()
                    .join(" · ")
                };
                let mut metrics = Vec::new();
                if let Some(latency) = done.latency_ms {
                    metrics.push(gateway_duration(latency));
                }
                if done.total_tokens > 0 {
                    metrics.push(format!("{} tok", compact_tokens(done.total_tokens)));
                }
                if let Some(share) = cache_share(done) {
                    metrics.push(said(
                        locale,
                        format!("{share}% cached"),
                        format!("缓存 {share}%"),
                        format!("快取 {share}%"),
                    ));
                }
                if let Some(speed) = output_speed(done) {
                    metrics.push(format!("{speed} tok/s"));
                }
                (dot, who, detail, metrics)
            }
        };
        let ((dot_color, who_color), pick) = (dot, id.clone());
        let key_pick = id.clone();
        h_flex()
            .id(SharedString::from(format!(
                "management-gateway-request-row-{}",
                id.as_str()
            )))
            .w_full()
            .h(px(ROUTE_ROW_H))
            .min_w_0()
            .items_center()
            .gap_3()
            .px_2()
            .rounded(cx.theme().radius)
            .border_1()
            .border_color(if selected {
                cx.theme().list_active_border
            } else {
                cx.theme().transparent
            })
            .when(selected, |row| row.bg(cx.theme().list_active))
            .when(!selected, |row| {
                row.hover(|style| style.bg(cx.theme().list_hover))
            })
            .on_click(cx.listener(move |this, _, _, cx| this.pick_gateway_route(pick.clone(), cx)))
            .focusable()
            .tab_index(0)
            .on_key_down(cx.listener(move |this, event: &KeyDownEvent, _, cx| {
                if event.keystroke.key == "enter" || event.keystroke.key == "space" {
                    this.pick_gateway_route(key_pick.clone(), cx);
                    cx.stop_propagation();
                }
            }))
            .child(
                div()
                    .flex_none()
                    .w(rems(4.5))
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .font_family(cx.theme().mono_font_family.clone())
                    .child(gateway_clock(at_ms)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(hue))
                    .child(agent_brand_icon(&agent_key, px(14.0), None))
                    .child(
                        div()
                            .min_w(rems(3.0))
                            .truncate()
                            .text_xs()
                            .font_family(cx.theme().mono_font_family.clone())
                            .child(asked_model(&requested).to_string()),
                    )
                    .when_some(route, |line, route| {
                        line.child(
                            div()
                                .min_w_0()
                                .max_w(rems(9.0))
                                .truncate()
                                .px_1p5()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(cx.theme().border)
                                .text_size(px(10.5))
                                .text_color(cx.theme().muted_foreground)
                                .child(route),
                        )
                    }),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .items_center()
                    .gap_1p5()
                    .child(div().flex_none().size(px(6.0)).rounded_full().bg(dot_color))
                    .child(
                        div()
                            .min_w(rems(3.0))
                            .truncate()
                            .text_xs()
                            .font_medium()
                            .text_color(who_color)
                            .child(who),
                    )
                    .when(!detail.is_empty(), |line| {
                        line.child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_xs()
                                .text_color(if failed {
                                    cx.theme().danger
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(detail),
                        )
                    })
                    .children(tags.into_iter().map(|tag| div().flex_none().child(tag))),
            )
            .child(
                div()
                    .flex_none()
                    .w(rems(13.0))
                    .truncate()
                    .text_right()
                    .text_xs()
                    .font_family(cx.theme().mono_font_family.clone())
                    .text_color(cx.theme().muted_foreground)
                    .child(metrics.join(" · ")),
            )
            .into_any_element()
    }
}

#[derive(Clone)]
enum RouteListRow {
    Live(vibex_core::GatewayInflight),
    /// Boxed: a ledger row is several times the size of a live entry.
    Done(Box<vibex_core::GatewayRequestSummary>),
}

impl RouteListRow {
    fn id(&self) -> &vibex_core::RequestId {
        match self {
            Self::Live(live) => &live.request_id,
            Self::Done(done) => &done.request_id,
        }
    }
}

/// The list's column heads, on the same lanes as its rows.
fn route_list_header(cx: &App) -> AnyElement {
    h_flex()
        .w_full()
        .items_center()
        .gap_3()
        .px_2()
        .pb_1()
        .text_xs()
        .text_color(cx.theme().muted_foreground)
        .child(
            div()
                .flex_none()
                .w(rems(4.5))
                .child(management_locale_text("Time", "时间", "時間")),
        )
        .child(div().flex_1().min_w_0().child(management_locale_text(
            "Agent · model asked",
            "Agent · 请求模型",
            "Agent · 請求模型",
        )))
        .child(div().flex_1().min_w_0().child(management_locale_text(
            "Answered by",
            "响应方",
            "回應方",
        )))
        .child(
            div()
                .flex_none()
                .w(rems(13.0))
                .text_right()
                .child(management_locale_text(
                    "Time · tokens · cache · speed",
                    "用时 · Token · 缓存 · 速度",
                    "用時 · Token · 快取 · 速度",
                )),
        )
        .into_any_element()
}

/// A wall-clock time as the list shows it.
fn gateway_clock(ms: i64) -> String {
    use chrono::TimeZone as _;
    chrono::Local
        .timestamp_millis_opt(ms)
        .single()
        .map(|time| time.format("%H:%M:%S").to_string())
        .unwrap_or_else(|| "—".to_string())
}

/// A duration as a reader says it: milliseconds under a second, then seconds,
/// then minutes.
fn gateway_duration(ms: i64) -> String {
    if ms < 1_000 {
        format!("{ms} ms")
    } else if ms < 60_000 {
        format!("{:.1} s", ms as f64 / 1000.0)
    } else {
        format!("{} min", ms / 60_000)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn profile_id(name: &str) -> vibex_core::ModelProviderProfileId {
        vibex_core::ModelProviderProfileId::parse(format!("model_provider_{name}"))
            .expect("a valid profile id")
    }

    fn request_id(name: &str) -> vibex_core::RequestId {
        vibex_core::RequestId::parse(format!("request_{name}")).expect("a valid request id")
    }

    /// The short name a test gave a Provider.
    fn provider_name(id: &vibex_core::ModelProviderProfileId) -> String {
        id.as_str()
            .strip_prefix("model_provider_")
            .unwrap_or(id.as_str())
            .to_string()
    }

    fn row(
        outcome: vibex_core::GatewayRequestOutcome,
        answering: &str,
        credential_name: Option<&str>,
        route: Option<vibex_core::GatewayRouteSummary>,
    ) -> vibex_core::GatewayRequestSummary {
        vibex_core::GatewayRequestSummary {
            request_id: request_id("request-1"),
            agent_id: Some("codex".to_string()),
            model_provider_profile_id: profile_id(answering),
            provider_name: answering.to_string(),
            model_requested: "group/route-1/gpt-5".to_string(),
            model_served: None,
            wire_in: vibex_core::WIRE_PROTOCOL_OPENAI_RESPONSES.to_string(),
            wire_out: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            status: Some(200),
            total_tokens: 1_200,
            input_tokens: 200,
            output_tokens: 200,
            cached_read_tokens: 800,
            latency_ms: Some(2_000),
            outcome,
            credential_name: credential_name.map(str::to_string),
            route,
            created_at_ms: 10_000,
        }
    }

    fn trace(
        failed: Vec<(&str, vibex_core::GatewayFailureKind)>,
        skipped: Vec<(&str, vibex_core::GatewayFailureKind)>,
    ) -> vibex_core::GatewayRouteSummary {
        vibex_core::GatewayRouteSummary {
            skipped_cooling: 0,
            skipped_over_limit: 0,
            failed_attempts: failed
                .into_iter()
                .map(|(key, failure)| vibex_core::GatewayRouteAttemptSummary {
                    credential_name: Some(key.to_string()),
                    credential_id: request_id(key),
                    failure,
                })
                .collect(),
            skipped_members: skipped
                .into_iter()
                .map(
                    |(provider, failure)| vibex_core::GatewaySkippedMemberSummary {
                        provider_name: Some(provider.to_string()),
                        model_provider_profile_id: profile_id(provider),
                        wire_protocol_id: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
                            .to_string(),
                        failure,
                    },
                )
                .collect(),
            error_code: None,
            error_message: None,
            decision: None,
        }
    }

    /// Which Provider each test Key belongs to: `alpha-*` Keys are Alpha's.
    fn owner(id: &vibex_core::RequestId) -> Option<vibex_core::ModelProviderProfileId> {
        let key = id.as_str().strip_prefix("request_")?;
        Some(profile_id(key.split('-').next()?))
    }

    fn verdicts(walk: &[WalkStep]) -> Vec<(String, WalkVerdict)> {
        walk.iter()
            .map(|step| (provider_name(&step.profile_id), step.verdict))
            .collect()
    }

    /// A Key that refused before another Key of the same Provider answered is
    /// a step of its own, on the member that answered.
    #[test]
    fn a_refused_key_comes_before_the_key_that_answered() {
        use vibex_core::GatewayFailureKind::Rate;
        let walk = ledger_walk(
            &row(
                vibex_core::GatewayRequestOutcome::Failover,
                "alpha",
                Some("alpha-backup"),
                Some(trace(vec![("alpha-primary", Rate)], Vec::new())),
            ),
            owner,
        );
        assert_eq!(
            verdicts(&walk),
            vec![
                ("alpha".to_string(), WalkVerdict::Failed(Rate)),
                ("alpha".to_string(), WalkVerdict::Answered),
            ]
        );
        assert_eq!(walk[0].key_id.as_deref(), Some("request_alpha-primary"));
        assert_eq!(walk[1].key_name.as_deref(), Some("alpha-backup"));
        assert_eq!(walk_moves(&walk), 1);
    }

    /// A member whose Keys all refused is left after them; a member none of
    /// whose Keys was tried was passed over; the next member answers.
    #[test]
    fn members_left_behind_keep_their_order() {
        use vibex_core::GatewayFailureKind::{Quota, Rate};
        let walk = ledger_walk(
            &row(
                vibex_core::GatewayRequestOutcome::Failover,
                "gamma",
                Some("gamma-only"),
                Some(trace(
                    vec![("alpha-one", Quota), ("alpha-two", Rate)],
                    vec![("alpha", Rate), ("beta", Quota)],
                )),
            ),
            owner,
        );
        assert_eq!(
            verdicts(&walk),
            vec![
                ("alpha".to_string(), WalkVerdict::Failed(Quota)),
                ("alpha".to_string(), WalkVerdict::Failed(Rate)),
                ("beta".to_string(), WalkVerdict::Skipped(Quota)),
                ("gamma".to_string(), WalkVerdict::Answered),
            ]
        );
    }

    /// A refusal before any member, and a reply that broke off, read
    /// differently: the first walked nowhere, the second ended on its member.
    #[test]
    fn refusals_and_broken_replies_end_the_walk_differently() {
        let refused = ledger_walk(
            &row(
                vibex_core::GatewayRequestOutcome::Rejected,
                "unknown",
                None,
                None,
            ),
            owner,
        );
        assert!(refused.is_empty());

        let broke = ledger_walk(
            &row(
                vibex_core::GatewayRequestOutcome::Failed,
                "alpha",
                Some("alpha-primary"),
                None,
            ),
            owner,
        );
        assert_eq!(
            verdicts(&broke),
            vec![("alpha".to_string(), WalkVerdict::Broke)]
        );
    }

    fn began(request: &str, provider: &str, attempt: u32) -> vibex_core::GatewayEvent {
        vibex_core::GatewayEvent::Began(vibex_core::GatewayInflight {
            request_id: request_id(request),
            agent_id: Some("codex".to_string()),
            model_route_id: Some("route-1".to_string()),
            requested_model: "group/route-1/gpt-5".to_string(),
            model_provider_profile_id: profile_id(provider),
            attempt,
            credential_id: None,
            wire_protocol_id: vibex_core::WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
            started_at_ms: 0,
            first_token_at_ms: None,
        })
    }

    /// The trail keeps one step per member the gateway announced, timed by
    /// when each was heard, and its end.
    #[test]
    fn a_trail_follows_the_gateways_announcements() {
        let mut trails = Vec::new();
        note_trail(&mut trails, &began("request-1", "alpha", 1), 100);
        note_trail(&mut trails, &began("request-1", "alpha", 1), 150);
        note_trail(&mut trails, &began("request-1", "beta", 2), 400);
        note_trail(
            &mut trails,
            &vibex_core::GatewayEvent::FirstToken {
                request_id: request_id("request-1"),
                at_ms: 0,
            },
            900,
        );
        note_trail(
            &mut trails,
            &vibex_core::GatewayEvent::Ended {
                request_id: request_id("request-1"),
                outcome: vibex_core::GatewayRequestOutcome::Ok,
            },
            1_500,
        );
        assert_eq!(trails.len(), 1);
        let trail = &trails[0];
        assert_eq!(
            trail
                .hops
                .iter()
                .map(|hop| (provider_name(&hop.profile_id), hop.heard_ms))
                .collect::<Vec<_>>(),
            vec![("alpha".to_string(), 100), ("beta".to_string(), 400)]
        );
        assert_eq!(trail.streaming_since, Some(900));
        assert_eq!(
            trail.ended,
            Some((1_500, vibex_core::GatewayRequestOutcome::Ok))
        );
        let walk = trail_walk(trail, None);
        assert_eq!(
            verdicts(&walk),
            vec![
                ("alpha".to_string(), WalkVerdict::MovedOn),
                ("beta".to_string(), WalkVerdict::Answered),
            ]
        );

        // Once its flight is long over, the trail is forgotten.
        note_trail(
            &mut trails,
            &began("request-2", "alpha", 1),
            1_500 + TRAIL_KEEP_MS,
        );
        assert_eq!(trails.len(), 1);
        assert_eq!(trails[0].request_id, request_id("request-2"));
    }

    /// The ledger row, once written, says why a member the gateway left let
    /// the request go.
    #[test]
    fn the_ledger_names_why_a_live_request_moved_on() {
        use vibex_core::GatewayFailureKind::Quota;
        let mut trails = Vec::new();
        note_trail(&mut trails, &began("request-1", "alpha", 1), 100);
        note_trail(&mut trails, &began("request-1", "beta", 2), 400);
        let recorded = row(
            vibex_core::GatewayRequestOutcome::Failover,
            "beta",
            Some("beta-key"),
            Some(trace(Vec::new(), vec![("alpha", Quota)])),
        );
        let walk = trail_walk(&trails[0], Some(&recorded));
        assert_eq!(walk[0].verdict, WalkVerdict::Failed(Quota));
        assert_eq!(walk[1].verdict, WalkVerdict::Pending);
        assert_eq!(walk[1].key_name.as_deref(), Some("beta-key"));
    }

    fn names() -> StoryNames {
        StoryNames {
            agent: "Codex".to_string(),
            model: "gpt-5".to_string(),
            route: Some(("Fast".to_string(), "Smart".to_string(), 2)),
            providers: [
                (profile_id("alpha"), "Alpha".to_string()),
                (profile_id("beta"), "Beta".to_string()),
            ]
            .into_iter()
            .collect(),
            keys: HashMap::new(),
        }
    }

    /// The story names the Key that refused and why, then the one that
    /// answered with what it cost.
    #[test]
    fn the_story_names_the_key_it_stepped_over_and_the_one_that_answered() {
        use vibex_core::GatewayFailureKind::Rate;
        let recorded = row(
            vibex_core::GatewayRequestOutcome::Failover,
            "alpha",
            Some("Backup"),
            Some(trace(vec![("alpha-primary", Rate)], Vec::new())),
        );
        let mut walk = ledger_walk(&recorded, owner);
        walk[0].key_name = Some("Primary".to_string());
        let lines = recorded_story(&recorded, &walk, &names(), ResolvedLocale::En);
        let texts = lines
            .iter()
            .map(|line| line.text.as_str())
            .collect::<Vec<_>>();
        assert_eq!(
            texts[0],
            "Codex asked for gpt-5 through the route group “Fast” (Smart, 2 members)"
        );
        assert_eq!(
            texts[1],
            "Alpha · Primary: rate limited, rests 30.0 s; the gateway went on"
        );
        assert_eq!(
            texts[2],
            "Alpha · Backup answered in 2.0 s · 200 in / 200 out · 80% from cache · OpenAI Responses → Chat Completions"
        );
        assert_eq!(lines[1].kind, StoryKind::Refused);
        assert_eq!(lines[2].kind, StoryKind::Answered);

        let chinese = recorded_story(&recorded, &walk, &names(), ResolvedLocale::ZhCn);
        assert_eq!(
            chinese[0].text,
            "Codex 经路由组「Fast」请求 gpt-5（Smart，2 个成员）"
        );
    }

    /// A failed request says what the upstream said, word for word.
    #[test]
    fn a_failed_story_quotes_the_upstream() {
        let mut failed_trace = trace(Vec::new(), Vec::new());
        failed_trace.error_code = Some("model_gateway_upstream_failed".to_string());
        failed_trace.error_message = Some("Invalid schema for function 'read'".to_string());
        let mut recorded = row(
            vibex_core::GatewayRequestOutcome::Failed,
            "alpha",
            Some("Primary"),
            Some(failed_trace),
        );
        recorded.status = Some(400);
        let walk = ledger_walk(&recorded, owner);
        let lines = recorded_story(&recorded, &walk, &names(), ResolvedLocale::En);
        let last = lines.last().expect("a quoted line");
        assert_eq!(last.kind, StoryKind::Said);
        assert_eq!(last.text, "It said: Invalid schema for function 'read'");
        assert!(lines.iter().any(|line| {
            line.kind == StoryKind::Refused
                && line
                    .text
                    .contains("HTTP 400 · model_gateway_upstream_failed")
        }));
    }

    #[test]
    fn list_figures_read_at_a_glance() {
        assert_eq!(compact_tokens(512), "512");
        assert_eq!(compact_tokens(12_345), "12.3k");
        assert_eq!(compact_tokens(1_250_000), "1.2M");
        let recorded = row(vibex_core::GatewayRequestOutcome::Ok, "alpha", None, None);
        assert_eq!(cache_share(&recorded), Some(80));
        assert_eq!(output_speed(&recorded), Some(100));
        assert_eq!(asked_model("group/route-1/gpt-5"), "gpt-5");
        assert_eq!(asked_model("gpt-5"), "gpt-5");
    }
}

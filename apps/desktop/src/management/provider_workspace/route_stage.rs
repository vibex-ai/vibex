//! The gateway's route map: where each request went, drawn as it happens.
//!
//! Agents stand on the left, the gateway in the middle and the members of the
//! route groups on the right, joined by wires. A request is a dot in its
//! Agent's colour: it leaves the Agent, passes through the gateway and lands
//! on the member that was tried first, where it waits while that member
//! answers. A refusal shakes the member and names why; the dot returns to the
//! gateway and goes on to the next member. An answer flies home as a ring.
//!
//! The map decides nothing. Its rows and every verdict come from the gateway's
//! own events and ledger (`routes.rs` builds the [`StageModel`]); the map only
//! paces them so a reader can follow. A live request moves when the gateway
//! says it moved; a request picked from the list replays its record at an
//! even pace.
//!
//! The nodes are laid out by ordinary flex layout. Each one carries a canvas
//! that records its bounds during prepaint, and the wire and dot layers read
//! those bounds when they paint later in the same frame, so a wire always ends
//! where its node actually is. Frames are asked for only while a dot travels;
//! a request that is only waiting keeps a slower clock for its pulse.

use std::cell::RefCell;
use std::f32::consts::PI;
use std::rc::Rc;

use super::*;

// ----- the model ---------------------------------------------------------------

/// One Agent on the left edge.
#[derive(Clone, PartialEq)]
pub(crate) struct StageAgent {
    /// The Agent id, which picks its brand mark and its colour.
    pub id: String,
    pub label: String,
    /// The model it asked for last.
    pub detail: String,
    /// Bound to a route group, but sending nothing the map shows.
    pub idle: bool,
}

/// Whether the gateway can serve.
#[derive(Clone, Copy, PartialEq, Default)]
pub(crate) enum HubState {
    #[default]
    Serving,
    Degraded,
    Off,
}

/// The gateway in the middle.
#[derive(Clone, PartialEq, Default)]
pub(crate) struct StageHub {
    pub title: String,
    pub detail: String,
    /// How the route on the map orders its members.
    pub chip: Option<String>,
    pub state: HubState,
}

/// How a member, or one of its Keys, stands right now.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum LaneTone {
    Ready,
    Cooling,
    Down,
    Off,
}

/// One Key of a member, as a chip under its row.
#[derive(Clone, PartialEq)]
pub(crate) struct StageKey {
    pub id: String,
    pub label: String,
    pub tone: LaneTone,
}

/// One route member on the right edge: a Provider and the model it is asked
/// for there.
#[derive(Clone, PartialEq)]
pub(crate) struct StageLane {
    /// The route member, or the Provider when no member names it.
    pub id: String,
    /// What the Provider's logo is looked up by, most specific first.
    pub marks: Vec<String>,
    pub title: String,
    pub model: String,
    pub keys: Vec<StageKey>,
    /// What the row says while nothing happens on it.
    pub note: String,
    pub tone: LaneTone,
}

/// One row of the members' column.
#[derive(Clone, PartialEq)]
pub(crate) enum StageRow {
    /// A route group's heading, when the map shows more than one.
    Group { title: String, detail: String },
    /// A member, by its index in [`StageModel::lanes`].
    Lane(usize),
    /// Members the map had no room for.
    More(usize),
}

/// What became of one step of a request's walk.
#[derive(Clone, PartialEq, Debug)]
pub(crate) enum StepVerdict {
    /// The member is still being waited on.
    Pending,
    Answered,
    /// Tried and refused, with the reason its tag shows.
    Failed(SharedString),
    /// Passed over without being asked, with the reason.
    Skipped(SharedString),
}

/// One member, or one Key of a member, a request went to.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct StageStep {
    pub lane: usize,
    /// The Key, when the record names it.
    pub key: Option<String>,
    pub verdict: StepVerdict,
    /// When the gateway announced this step, for a request seen live.
    pub at_ms: Option<i64>,
}

/// How a flight is paced.
#[derive(Clone, Copy, PartialEq, Debug)]
pub(crate) enum FlightClock {
    /// Running, or just over: each step moves when the gateway said it did.
    Live,
    /// Picked from the list: its record replayed from the replay's start.
    Replay,
    /// Over and only shown: everything revealed, nothing moving.
    Settled,
}

/// One request on the map.
#[derive(Clone, PartialEq, Debug)]
pub(crate) struct StageFlight {
    pub id: String,
    pub agent: usize,
    pub steps: Vec<StageStep>,
    pub clock: FlightClock,
    /// Whether it was answered, and when the end was heard (live only).
    pub ended: Option<(bool, Option<i64>)>,
    /// When its first token was heard, for a request seen live.
    pub streaming_since: Option<i64>,
}

impl StageFlight {
    fn answered(&self) -> bool {
        match self.ended {
            Some((ok, _)) => ok,
            None => self
                .steps
                .last()
                .is_some_and(|step| step.verdict == StepVerdict::Answered),
        }
    }

    fn streaming(&self) -> bool {
        self.streaming_since.is_some() && self.ended.is_none()
    }
}

/// Everything the map draws.
#[derive(Clone, PartialEq, Default)]
pub(crate) struct StageModel {
    pub agents: Vec<StageAgent>,
    pub hub: StageHub,
    pub lanes: Vec<StageLane>,
    pub rows: Vec<StageRow>,
    pub flights: Vec<StageFlight>,
    /// The request the map is pinned to; picking another restarts the replay.
    pub pinned: Option<String>,
    /// Said in the members' column when it has no member to show.
    pub note: Option<String>,
}

pub(crate) struct RouteStage {
    model: StageModel,
    /// When the pinned request's replay began, on the wall clock.
    replay_from_ms: i64,
    /// A pending redraw for a pulse, while nothing travels.
    ambient: Option<Task<()>>,
}

impl RouteStage {
    pub(crate) fn new() -> Self {
        Self {
            model: StageModel::default(),
            replay_from_ms: 0,
            ambient: None,
        }
    }

    /// Takes the latest picture of the gateway.
    ///
    /// The owner calls this while it renders, and the map renders right after
    /// in the same frame, so nothing is notified here. A newly pinned request
    /// starts its replay from the top.
    pub(crate) fn set_model(&mut self, model: StageModel) {
        if model.pinned != self.model.pinned {
            self.replay_from_ms = vibex_core::unix_timestamp_ms();
        }
        self.model = model;
    }

    /// Plays the pinned request again from the top.
    pub(crate) fn replay(&mut self, cx: &mut Context<Self>) {
        self.replay_from_ms = vibex_core::unix_timestamp_ms();
        cx.notify();
    }

    /// Redraws a little later, for a pulse that needs no full frame rate.
    fn tick_soon(&mut self, cx: &mut Context<Self>) {
        if self.ambient.is_some() {
            return;
        }
        self.ambient = Some(cx.spawn(async move |this, cx| {
            cx.background_executor()
                .timer(Duration::from_millis(AMBIENT_TICK_MS))
                .await;
            let _ = this.update(cx, |this, cx| {
                this.ambient = None;
                cx.notify();
            });
        }));
    }
}

// ----- pacing --------------------------------------------------------------------

/// How long each part of a flight takes, in milliseconds. They pace a reader's
/// eye; they are not the request's own timings, which the story states.
const OUT_MS: f64 = 1_300.0;
const HOP_MS: f64 = 1_000.0;
const HOME_MS: f64 = 1_300.0;
const REFUSED_MS: f64 = 1_100.0;
/// A replayed member's wait for its answer.
const HOLD_MS: f64 = 850.0;
/// A replayed retry on another Key of the same member.
const RETRY_MS: f64 = 550.0;
/// A replayed member passed over without being asked.
const PASS_MS: f64 = 380.0;
/// The shortest wait a live member is seen to have, so a refusal that came
/// back at once still reads as the member answering.
const MIN_HOLD_MS: f64 = 320.0;
/// The pause before a replay's first dot leaves.
const LEAD_MS: f64 = 240.0;
const SHAKE_MS: f64 = 450.0;
const TAG_MS: f64 = 1_800.0;
/// The pulse of a dot waiting on a member, and of a busy gateway.
const PULSE_MS: f64 = 1_600.0;
/// One dash and its gap; the pattern travels one period per [`FLOW_MS`].
const DASH_PERIOD: f32 = 8.0;
const DASH_ON: f32 = 3.0;
const FLOW_MS: f64 = 700.0;
/// A waiting request redraws at this interval instead of every frame.
const AMBIENT_TICK_MS: u64 = 66;

#[derive(Clone, Copy, PartialEq, Debug)]
enum Leg {
    /// Agent → gateway → member.
    Out { lane: usize },
    /// Member → gateway → the next member.
    Hop { from: usize, to: usize },
    /// Member → gateway → Agent.
    Home { lane: usize, ok: bool },
    /// Agent → gateway → Agent: refused before any member was asked.
    Refused,
    /// Waiting on a member's answer.
    Hold { lane: usize },
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct TimedLeg {
    leg: Leg,
    start: f64,
    end: f64,
}

/// A flight laid out on the clock.
#[derive(Clone, PartialEq, Debug, Default)]
struct Schedule {
    legs: Vec<TimedLeg>,
    /// When each step's verdict shows; infinite until it is known.
    reveals: Vec<f64>,
    /// When the dot is home, once that is known.
    landed: Option<f64>,
}

impl Schedule {
    fn push(&mut self, leg: Leg, start: f64, length: f64) -> f64 {
        self.legs.push(TimedLeg {
            leg,
            start,
            end: start + length,
        });
        start + length
    }

    /// After a step's verdict at `at`: on to the next member, or home. A
    /// retry on another Key of the same member keeps the dot where it is.
    fn after_step(&mut self, step: &StageStep, next: Option<&StageStep>, ok: bool, at: f64) -> f64 {
        match next {
            Some(next) if next.lane == step.lane => at,
            Some(next) => self.push(
                Leg::Hop {
                    from: step.lane,
                    to: next.lane,
                },
                at,
                HOP_MS,
            ),
            None => {
                let home = self.push(
                    Leg::Home {
                        lane: step.lane,
                        ok,
                    },
                    at,
                    HOME_MS,
                );
                self.landed = Some(home);
                home
            }
        }
    }
}

fn schedule(flight: &StageFlight, replay_from: f64, still: bool) -> Schedule {
    let count = flight.steps.len();
    let mut out = Schedule {
        legs: Vec::new(),
        reveals: vec![f64::INFINITY; count],
        landed: None,
    };
    let clock = match flight.clock {
        FlightClock::Replay if still => FlightClock::Settled,
        clock => clock,
    };
    match clock {
        FlightClock::Settled => {
            out.reveals = vec![f64::NEG_INFINITY; count];
            out.landed = Some(f64::NEG_INFINITY);
        }
        FlightClock::Replay => {
            let start = replay_from + LEAD_MS;
            let Some(first) = flight.steps.first() else {
                out.landed = Some(out.push(Leg::Refused, start, REFUSED_MS));
                return out;
            };
            let mut at = out.push(Leg::Out { lane: first.lane }, start, OUT_MS);
            for (index, step) in flight.steps.iter().enumerate() {
                let next = flight.steps.get(index + 1);
                let hold = match (&step.verdict, next) {
                    (StepVerdict::Skipped(_), _) => PASS_MS,
                    (_, Some(next)) if next.lane == step.lane => RETRY_MS,
                    _ => HOLD_MS,
                };
                at = out.push(Leg::Hold { lane: step.lane }, at, hold);
                out.reveals[index] = at;
                at = out.after_step(step, next, flight.answered(), at);
            }
        }
        FlightClock::Live => {
            let Some(first) = flight.steps.first() else {
                return out;
            };
            let heard = |ms: Option<i64>| ms.map_or(f64::NEG_INFINITY, |ms| ms as f64);
            let mut at = heard(first.at_ms);
            if !still {
                at = out.push(Leg::Out { lane: first.lane }, at, OUT_MS);
            }
            for (index, step) in flight.steps.iter().enumerate() {
                let next = flight.steps.get(index + 1);
                // A step is decided once anything later is heard: the next
                // member being asked, or the request's end.
                let decided = match next {
                    Some(next) => Some(heard(next.at_ms)),
                    None => flight.ended.map(|(_, at_ms)| heard(at_ms)),
                };
                let Some(decided) = decided else {
                    out.legs.push(TimedLeg {
                        leg: Leg::Hold { lane: step.lane },
                        start: at,
                        end: f64::INFINITY,
                    });
                    break;
                };
                if still {
                    out.reveals[index] = decided;
                    if next.is_none() {
                        out.landed = Some(decided);
                    }
                    continue;
                }
                let decided = decided.max(at + MIN_HOLD_MS);
                out.legs.push(TimedLeg {
                    leg: Leg::Hold { lane: step.lane },
                    start: at,
                    end: decided,
                });
                out.reveals[index] = decided;
                at = out.after_step(step, next, flight.answered(), decided);
            }
        }
    }
    out
}

/// Where a flight's dot is at one moment.
#[derive(Clone, Copy, PartialEq, Debug)]
enum DotAt {
    /// Travelling a leg, `progress` of the way, eased.
    Moving { leg: Leg, progress: f32 },
    /// Waiting on a member.
    Waiting { lane: usize },
}

fn dot_at(schedule: &Schedule, now: f64) -> Option<DotAt> {
    for timed in &schedule.legs {
        if now < timed.start {
            return None;
        }
        if now < timed.end {
            return Some(match timed.leg {
                Leg::Hold { lane } => DotAt::Waiting { lane },
                leg => DotAt::Moving {
                    leg,
                    progress: ease(((now - timed.start) / (timed.end - timed.start)) as f32),
                },
            });
        }
    }
    None
}

/// A glide that starts and lands gently.
fn ease(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    (1.0 - (PI * t).cos()) / 2.0
}

/// How far a shaken row is pushed aside, `elapsed` ms after the refusal.
fn shake_offset(elapsed: f64) -> f32 {
    const KEYS: [(f64, f32); 6] = [
        (0.0, 0.0),
        (0.2, -5.0),
        (0.4, 4.0),
        (0.6, -2.0),
        (0.8, 1.0),
        (1.0, 0.0),
    ];
    if !(0.0..SHAKE_MS).contains(&elapsed) {
        return 0.0;
    }
    let t = elapsed / SHAKE_MS;
    KEYS.windows(2)
        .find(|pair| t <= pair[1].0)
        .map(|pair| {
            let span = (t - pair[0].0) / (pair[1].0 - pair[0].0);
            pair[0].1 + (pair[1].1 - pair[0].1) * span as f32
        })
        .unwrap_or(0.0)
}

/// A 0..1 phase that loops every `period` ms.
fn phase(now: f64, period: f64) -> f32 {
    (now.rem_euclid(period) / period) as f32
}

// ----- what each node shows now ------------------------------------------------------

/// How a wire is drawn this frame.
#[derive(Clone, Copy, PartialEq, Debug)]
enum WireLook {
    Idle,
    /// The way an answer took, or a dot is taking now.
    Lit(Hsla),
    /// A request is on it now; the dashes run towards the member, or back
    /// towards the Agent once the answer streams.
    Flow {
        hue: Hsla,
        inward: bool,
    },
    /// The member refused.
    Refused,
    /// The member is turned off.
    Off,
}

/// What a member's row says and how it looks this frame.
#[derive(Clone, Default)]
struct LaneLook {
    /// A request waits on it: the Agent's colour, whether it streams, the Key.
    waiting: Option<(Hsla, bool, Option<String>)>,
    /// It answered: the colour, the Agent, the Key.
    answered: Option<(Hsla, usize, Option<String>)>,
    /// It refused: why, when that showed, whether that ended the request, the Key.
    refused: Option<(SharedString, f64, bool, Option<String>)>,
    /// A dot travels its wire now.
    passing: Option<Hsla>,
}

/// The dot of one flight this frame.
#[derive(Clone, Copy)]
struct DotLook {
    agent: usize,
    hue: Hsla,
    at: DotAt,
}

/// Everything the map shows at one moment.
struct StageLooks {
    lanes: Vec<LaneLook>,
    agent_wires: Vec<WireLook>,
    lane_wires: Vec<WireLook>,
    /// The Agents with a request on the map, by colour.
    agents_active: Vec<Option<Hsla>>,
    dots: Vec<DotLook>,
    /// Any request still running through the gateway.
    busy: bool,
    /// A dot travels, a row shakes or a tag shows: frames are needed.
    moving: bool,
    /// Something pulses or flows: a slower clock is enough.
    ambient: bool,
}

fn stage_looks(
    model: &StageModel,
    hues: &[Hsla],
    now: f64,
    replay_from: f64,
    still: bool,
) -> StageLooks {
    let mut looks = StageLooks {
        lanes: vec![LaneLook::default(); model.lanes.len()],
        agent_wires: vec![WireLook::Idle; model.agents.len()],
        lane_wires: model
            .lanes
            .iter()
            .map(|lane| {
                if lane.tone == LaneTone::Off {
                    WireLook::Off
                } else {
                    WireLook::Idle
                }
            })
            .collect(),
        agents_active: vec![None; model.agents.len()],
        dots: Vec::new(),
        busy: false,
        moving: false,
        ambient: false,
    };
    for flight in &model.flights {
        let Some(hue) = hues.get(flight.agent).copied() else {
            continue;
        };
        let plan = schedule(flight, replay_from, still);
        let dot = dot_at(&plan, now);
        let landed = plan.landed.is_some_and(|landed| landed <= now);
        for (index, step) in flight.steps.iter().enumerate() {
            let revealed = plan.reveals[index];
            if revealed > now {
                continue;
            }
            let Some(lane) = looks.lanes.get_mut(step.lane) else {
                continue;
            };
            match &step.verdict {
                StepVerdict::Answered => {
                    lane.answered = Some((hue, flight.agent, step.key.clone()));
                    lane.refused = None;
                    looks.lane_wires[step.lane] = WireLook::Lit(hue);
                }
                StepVerdict::Failed(reason) | StepVerdict::Skipped(reason) => {
                    let last = index + 1 == flight.steps.len();
                    if lane.answered.is_none() {
                        lane.refused = Some((reason.clone(), revealed, last, step.key.clone()));
                    }
                    // A live refusal fades with its tag; a recorded one stays,
                    // because it is part of the route being read.
                    let lasting = flight.clock != FlightClock::Live || still;
                    if lane.answered.is_none() && (lasting || now - revealed < TAG_MS) {
                        looks.lane_wires[step.lane] = WireLook::Refused;
                    }
                    if !still && now - revealed < TAG_MS {
                        looks.moving = true;
                    }
                }
                StepVerdict::Pending => {}
            }
        }
        let in_flight = dot.is_some() || (still && !landed && flight.ended.is_none());
        if in_flight {
            looks.busy = true;
            looks.agents_active[flight.agent] = Some(hue);
            looks.agent_wires[flight.agent] = if still {
                WireLook::Lit(hue)
            } else {
                WireLook::Flow {
                    hue,
                    inward: flight.streaming(),
                }
            };
        } else if landed && flight.answered() && looks.agent_wires[flight.agent] == WireLook::Idle {
            looks.agent_wires[flight.agent] = WireLook::Lit(hue);
        }
        // The step being waited on: the first one not decided yet.
        let pending = plan
            .reveals
            .iter()
            .position(|revealed| *revealed > now)
            .and_then(|index| flight.steps.get(index));
        // Still, a live request shows where it waits without a dot.
        let waiting_lane = match dot {
            Some(DotAt::Waiting { lane }) => Some(lane),
            None if still && flight.clock == FlightClock::Live && flight.ended.is_none() => {
                pending.map(|step| step.lane)
            }
            _ => None,
        };
        if let Some(lane) = waiting_lane {
            let key = pending.and_then(|step| step.key.clone());
            if let Some(look) = looks.lanes.get_mut(lane) {
                look.waiting = Some((hue, flight.streaming(), key));
            }
            if let Some(wire) = looks.lane_wires.get_mut(lane) {
                *wire = if still {
                    WireLook::Lit(hue)
                } else {
                    WireLook::Flow {
                        hue,
                        inward: flight.streaming(),
                    }
                };
            }
            looks.ambient = true;
        }
        if let Some(DotAt::Moving { leg, .. }) = dot {
            looks.moving = true;
            let lit: &[usize] = match &leg {
                Leg::Out { lane } | Leg::Home { lane, .. } => std::slice::from_ref(lane),
                Leg::Hop { from, to } => &[*from, *to],
                Leg::Refused | Leg::Hold { .. } => &[],
            };
            for lane in lit {
                if let Some(look) = looks.lanes.get_mut(*lane) {
                    look.passing = Some(hue);
                }
                if let Some(wire) = looks.lane_wires.get_mut(*lane)
                    && !matches!(wire, WireLook::Flow { .. })
                {
                    *wire = WireLook::Lit(hue);
                }
            }
        }
        if let (Some(at), false) = (dot, still) {
            looks.dots.push(DotLook {
                agent: flight.agent,
                hue,
                at,
            });
        }
    }
    if looks.busy && !still {
        looks.ambient = true;
    }
    looks
}

// ----- geometry ------------------------------------------------------------------------

/// The resolved bounds of every node, recorded as the frame is laid out.
#[derive(Default)]
struct StageGeometry {
    agents: Vec<Option<Bounds<Pixels>>>,
    hub: Option<Bounds<Pixels>>,
    lanes: Vec<Option<Bounds<Pixels>>>,
}

type SharedGeometry = Rc<RefCell<StageGeometry>>;

#[derive(Clone, Copy)]
enum Slot {
    Agent(usize),
    Hub,
    Lane(usize),
}

/// An invisible canvas over a node that records where the node landed.
fn measure(geometry: &SharedGeometry, slot: Slot) -> impl IntoElement {
    let geometry = geometry.clone();
    canvas(
        move |bounds, _, _| {
            let mut geometry = geometry.borrow_mut();
            let entry = match slot {
                Slot::Agent(index) => geometry.agents.get_mut(index),
                Slot::Lane(index) => geometry.lanes.get_mut(index),
                Slot::Hub => Some(&mut geometry.hub),
            };
            if let Some(entry) = entry {
                *entry = Some(bounds);
            }
        },
        |_, _, _, _| {},
    )
    .absolute()
    .top_0()
    .left_0()
    .size_full()
}

#[derive(Clone, Copy, PartialEq, Debug)]
struct P {
    x: f32,
    y: f32,
}

impl P {
    fn at(self) -> gpui::Point<Pixels> {
        point(px(self.x), px(self.y))
    }
}

fn left_mid(bounds: Bounds<Pixels>) -> P {
    P {
        x: bounds.left().as_f32(),
        y: bounds.center().y.as_f32(),
    }
}

fn right_mid(bounds: Bounds<Pixels>) -> P {
    P {
        x: bounds.right().as_f32(),
        y: bounds.center().y.as_f32(),
    }
}

/// A run of points with the distance along it to each.
#[derive(Clone, Default, Debug)]
struct Polyline {
    points: Vec<P>,
    lengths: Vec<f32>,
}

impl Polyline {
    fn from_points(points: Vec<P>) -> Self {
        let mut lengths = Vec::with_capacity(points.len());
        let mut total = 0.0;
        for (index, point) in points.iter().enumerate() {
            if index > 0 {
                let previous = points[index - 1];
                total += (point.x - previous.x).hypot(point.y - previous.y);
            }
            lengths.push(total);
        }
        Self { points, lengths }
    }

    /// The S a wire draws between two nodes: it leaves and arrives level.
    fn wire(from: P, to: P) -> Self {
        const SEGMENTS: usize = 28;
        let mx = (from.x + to.x) / 2.0;
        let points = (0..=SEGMENTS)
            .map(|step| {
                let t = step as f32 / SEGMENTS as f32;
                let u = 1.0 - t;
                let (a, b, c, d) = (u * u * u, 3.0 * u * u * t, 3.0 * u * t * t, t * t * t);
                P {
                    x: from.x * a + mx * b + mx * c + to.x * d,
                    y: from.y * a + from.y * b + to.y * c + to.y * d,
                }
            })
            .collect();
        Self::from_points(points)
    }

    fn length(&self) -> f32 {
        self.lengths.last().copied().unwrap_or(0.0)
    }

    fn reversed(mut self) -> Self {
        self.points.reverse();
        Self::from_points(self.points)
    }

    fn then(mut self, next: Polyline) -> Self {
        let skip = usize::from(self.points.last() == next.points.first());
        self.points.extend(next.points.into_iter().skip(skip));
        Self::from_points(self.points)
    }

    /// The point `distance` along, clamped to the ends.
    fn at(&self, distance: f32) -> Option<P> {
        let first = *self.points.first()?;
        if self.points.len() == 1 || distance <= 0.0 {
            return Some(first);
        }
        let index = self.lengths.partition_point(|length| *length < distance);
        if index >= self.points.len() {
            return self.points.last().copied();
        }
        let (a, b) = (self.points[index - 1], self.points[index]);
        let (la, lb) = (self.lengths[index - 1], self.lengths[index]);
        let span = if lb > la {
            (distance - la) / (lb - la)
        } else {
            0.0
        };
        Some(P {
            x: a.x + (b.x - a.x) * span,
            y: a.y + (b.y - a.y) * span,
        })
    }

    /// The part between two distances, as its own points.
    fn slice(&self, from: f32, to: f32) -> Vec<P> {
        let (Some(start), Some(end)) = (self.at(from), self.at(to)) else {
            return Vec::new();
        };
        let mut points = vec![start];
        points.extend(
            self.points
                .iter()
                .zip(&self.lengths)
                .filter(|(_, length)| **length > from && **length < to)
                .map(|(point, _)| *point),
        );
        points.push(end);
        points
    }
}

/// The wires of the map, between measured nodes.
struct Wires {
    agents: Vec<Option<Polyline>>,
    lanes: Vec<Option<Polyline>>,
    hub: Option<Bounds<Pixels>>,
}

impl Wires {
    fn from(geometry: &StageGeometry) -> Self {
        let hub = geometry.hub;
        let agents = geometry
            .agents
            .iter()
            .map(|agent| Some(Polyline::wire(right_mid((*agent)?), left_mid(hub?))))
            .collect();
        let lanes = geometry
            .lanes
            .iter()
            .map(|lane| Some(Polyline::wire(right_mid(hub?), left_mid((*lane)?))))
            .collect();
        Self { agents, lanes, hub }
    }

    /// The straight way across the gateway, from where Agents arrive to where
    /// members leave.
    fn through(&self) -> Option<Polyline> {
        let hub = self.hub?;
        Some(Polyline::from_points(vec![left_mid(hub), right_mid(hub)]))
    }

    /// A short dip into the gateway and back out at the same edge: the
    /// gateway taking a request back before handing it on.
    fn dip(&self, right_edge: bool) -> Option<Polyline> {
        let hub = self.hub?;
        let (edge, depth) = if right_edge {
            (right_mid(hub), -18.0)
        } else {
            (left_mid(hub), 18.0)
        };
        let inside = P {
            x: edge.x + depth,
            y: edge.y,
        };
        Some(Polyline::from_points(vec![edge, inside, edge]))
    }

    /// The whole way one leg flies.
    fn leg(&self, agent: usize, leg: Leg) -> Option<Polyline> {
        let agent_wire = || self.agents.get(agent).cloned().flatten();
        let lane_wire = |lane: usize| self.lanes.get(lane).cloned().flatten();
        match leg {
            Leg::Out { lane } => Some(agent_wire()?.then(self.through()?).then(lane_wire(lane)?)),
            Leg::Hop { from, to } => Some(
                lane_wire(from)?
                    .reversed()
                    .then(self.dip(true)?)
                    .then(lane_wire(to)?),
            ),
            Leg::Home { lane, .. } => Some(
                lane_wire(lane)?
                    .reversed()
                    .then(self.through()?.reversed())
                    .then(agent_wire()?.reversed()),
            ),
            Leg::Refused => Some(
                agent_wire()?
                    .then(self.dip(false)?)
                    .then(agent_wire()?.reversed()),
            ),
            Leg::Hold { .. } => None,
        }
    }
}

/// How much of a dot shows at `point`: it fades as it passes through the
/// gateway's card, so a request reads as going through it, not over it.
fn through_hub(point: P, hub: Option<Bounds<Pixels>>) -> f32 {
    let Some(hub) = hub else {
        return 1.0;
    };
    let (left, right) = (hub.left().as_f32(), hub.right().as_f32());
    let (top, bottom) = (hub.top().as_f32(), hub.bottom().as_f32());
    if point.x <= left || point.x >= right || point.y <= top || point.y >= bottom {
        return 1.0;
    }
    let depth = (point.x - left).min(right - point.x);
    (1.0 - depth / 14.0).clamp(0.0, 1.0)
}

fn stroke(points: &[P], width: f32, window: &mut Window, color: Hsla) {
    if points.len() < 2 {
        return;
    }
    let mut builder = PathBuilder::stroke(px(width));
    builder.move_to(points[0].at());
    for point in &points[1..] {
        builder.line_to(point.at());
    }
    if let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

/// Dashes along `line`, shifted by `offset` along it, so moving the offset
/// makes them run.
fn dashes(line: &Polyline, width: f32, offset: f32, window: &mut Window, color: Hsla) {
    let length = line.length();
    if length <= 0.0 {
        return;
    }
    let mut builder = PathBuilder::stroke(px(width));
    let mut start = offset.rem_euclid(DASH_PERIOD) - DASH_PERIOD;
    let mut any = false;
    while start < length {
        let (from, to) = (start.max(0.0), (start + DASH_ON).min(length));
        if to > from {
            let points = line.slice(from, to);
            if let Some(first) = points.first() {
                builder.move_to(first.at());
                for point in &points[1..] {
                    builder.line_to(point.at());
                }
                any = true;
            }
        }
        start += DASH_PERIOD;
    }
    if any && let Ok(path) = builder.build() {
        window.paint_path(path, color);
    }
}

fn paint_dot(window: &mut Window, center: P, radius: f32, color: Hsla, ring: Option<Hsla>) {
    let bounds = Bounds::new(
        point(px(center.x - radius), px(center.y - radius)),
        gpui::size(px(radius * 2.0), px(radius * 2.0)),
    );
    let mut quad = gpui::fill(bounds, color).corner_radii(px(radius));
    if let Some(ring) = ring {
        quad = quad.border_widths(px(2.0)).border_color(ring);
    }
    window.paint_quad(quad);
}

/// The theme colours the canvases paint with, read before painting.
#[derive(Clone, Copy)]
struct Ink {
    border: Hsla,
    background: Hsla,
    danger: Hsla,
    primary: Hsla,
}

fn paint_wire(window: &mut Window, line: &Polyline, look: WireLook, ink: Ink, now: f64) {
    match look {
        WireLook::Idle => stroke(&line.points, 1.0, window, ink.border),
        WireLook::Off => dashes(line, 1.0, 0.0, window, ink.border.opacity(0.7)),
        WireLook::Lit(hue) => {
            stroke(&line.points, 1.0, window, ink.border);
            stroke(&line.points, 2.0, window, hue.opacity(0.85));
        }
        WireLook::Flow { hue, inward } => {
            stroke(&line.points, 1.0, window, ink.border);
            let travelled = phase(now, FLOW_MS) * DASH_PERIOD;
            let offset = if inward { -travelled } else { travelled };
            dashes(line, 2.0, offset, window, hue);
        }
        WireLook::Refused => dashes(line, 1.5, 0.0, window, ink.danger.opacity(0.6)),
    }
}

/// The colour an Agent's requests fly in: one of the theme's chart hues,
/// chosen by its id so it stays the same from one request to the next. None
/// is the danger colour, which on this map means a refusal.
pub(crate) fn agent_hue(agent_id: &str, cx: &App) -> Hsla {
    let theme = cx.theme();
    let palette = [
        theme.chart_1,
        theme.chart_2,
        theme.chart_3,
        theme.chart_4,
        theme.chart_5,
        theme.blue,
        theme.cyan,
        theme.magenta,
    ];
    let hash = agent_id.bytes().fold(0u32, |hash, byte| {
        hash.wrapping_mul(31).wrapping_add(byte as u32)
    });
    palette[hash as usize % palette.len()]
}

// ----- rendering ------------------------------------------------------------------------

impl Render for RouteStage {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let now = vibex_core::unix_timestamp_ms() as f64;
        let still = crate::motion::reduced_motion(cx)
            || crate::motion::pauses_while_inactive(!window.is_window_active());
        let hues = self
            .model
            .agents
            .iter()
            .map(|agent| agent_hue(&agent.id, cx))
            .collect::<Vec<_>>();
        let looks = stage_looks(&self.model, &hues, now, self.replay_from_ms as f64, still);
        if !still {
            if looks.moving {
                window.request_animation_frame();
            } else if looks.ambient {
                self.tick_soon(cx);
            }
        }

        let geometry: SharedGeometry = Rc::new(RefCell::new(StageGeometry {
            agents: vec![None; self.model.agents.len()],
            hub: None,
            lanes: vec![None; self.model.lanes.len()],
        }));
        let ink = Ink {
            border: cx.theme().border,
            background: cx.theme().background,
            danger: cx.theme().danger,
            primary: cx.theme().primary,
        };

        let wires = {
            let geometry = geometry.clone();
            let agent_looks = looks.agent_wires.clone();
            let lane_looks = looks.lane_wires.clone();
            let busy = looks.busy && !still;
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let wires = Wires::from(&geometry.borrow());
                    for (line, look) in wires.agents.iter().zip(&agent_looks) {
                        if let Some(line) = line {
                            paint_wire(window, line, *look, ink, now);
                        }
                    }
                    for (line, look) in wires.lanes.iter().zip(&lane_looks) {
                        if let Some(line) = line {
                            paint_wire(window, line, *look, ink, now);
                        }
                    }
                    // A gateway with a request in it breathes.
                    if busy && let Some(hub) = wires.hub {
                        let breath = phase(now, PULSE_MS);
                        let grow = px(3.0 + 7.0 * breath);
                        let ring = Bounds::new(
                            point(hub.left() - grow, hub.top() - grow),
                            gpui::size(hub.size.width + grow * 2.0, hub.size.height + grow * 2.0),
                        );
                        window.paint_quad(
                            gpui::outline(
                                ring,
                                ink.primary.opacity(0.32 * (1.0 - breath)),
                                gpui::BorderStyle::Solid,
                            )
                            .corner_radii(px(12.0) + grow),
                        );
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };

        let sky = {
            let geometry = geometry.clone();
            let dots = looks.dots.clone();
            canvas(
                |_, _, _| (),
                move |_, _, window, _| {
                    let geometry = geometry.borrow();
                    let wires = Wires::from(&geometry);
                    for dot in &dots {
                        match dot.at {
                            DotAt::Waiting { lane } => {
                                let Some(bounds) = geometry.lanes.get(lane).copied().flatten()
                                else {
                                    continue;
                                };
                                let edge = left_mid(bounds);
                                let center = P {
                                    x: edge.x - 7.0,
                                    y: edge.y,
                                };
                                let breath = phase(now, PULSE_MS);
                                paint_dot(
                                    window,
                                    center,
                                    6.0 + 7.0 * breath,
                                    dot.hue.opacity(0.28 * (1.0 - breath)),
                                    None,
                                );
                                paint_dot(window, center, 4.5, dot.hue, None);
                            }
                            DotAt::Moving { leg, progress } => {
                                let Some(line) = wires.leg(dot.agent, leg) else {
                                    continue;
                                };
                                let travelled = line.length() * progress;
                                let Some(center) = line.at(travelled) else {
                                    continue;
                                };
                                let home = matches!(leg, Leg::Home { .. });
                                let refused = matches!(leg, Leg::Home { ok: false, .. })
                                    || (matches!(leg, Leg::Refused) && progress > 0.5);
                                // A short wake behind it shows which way it goes.
                                for trail in 1..=4 {
                                    let Some(behind) = line.at(travelled - trail as f32 * 5.0)
                                    else {
                                        continue;
                                    };
                                    let fade = through_hub(behind, wires.hub);
                                    paint_dot(
                                        window,
                                        behind,
                                        3.4 - trail as f32 * 0.55,
                                        dot.hue.opacity((0.34 - trail as f32 * 0.07) * fade),
                                        None,
                                    );
                                }
                                let fade = through_hub(center, wires.hub);
                                if fade <= 0.02 {
                                    continue;
                                }
                                let halo = if refused { ink.danger } else { dot.hue };
                                paint_dot(window, center, 9.0, halo.opacity(0.18 * fade), None);
                                if refused {
                                    paint_dot(
                                        window,
                                        center,
                                        4.5,
                                        dot.hue.opacity(fade),
                                        Some(ink.danger.opacity(fade)),
                                    );
                                } else if home {
                                    paint_dot(
                                        window,
                                        center,
                                        4.5,
                                        ink.background.opacity(fade),
                                        Some(dot.hue.opacity(fade)),
                                    );
                                } else {
                                    paint_dot(window, center, 4.5, dot.hue.opacity(fade), None);
                                }
                            }
                        }
                    }
                },
            )
            .absolute()
            .top_0()
            .left_0()
            .size_full()
        };

        let agents = self.render_agents(&looks, &hues, &geometry, cx);
        let hub = self.render_hub(&looks, &geometry, cx);
        let lanes = self.render_lanes(&looks, now, still, &geometry, cx);

        div()
            .id("management-route-stage")
            .w_full()
            .overflow_x_scrollbar()
            .child(
                div()
                    .relative()
                    .w_full()
                    .min_w(rems(38.0))
                    .child(wires)
                    .child(
                        h_flex()
                            .w_full()
                            .items_center()
                            .gap(rems(3.5))
                            .px_1()
                            .py_5()
                            .child(agents)
                            .child(hub)
                            .child(lanes),
                    )
                    .child(sky),
            )
    }
}

impl RouteStage {
    fn render_agents(
        &self,
        looks: &StageLooks,
        hues: &[Hsla],
        geometry: &SharedGeometry,
        cx: &App,
    ) -> AnyElement {
        let mut column = v_flex().flex_none().w(rems(11.0)).gap_2();
        for (index, agent) in self.model.agents.iter().enumerate() {
            let hue = hues[index];
            let active = looks.agents_active[index];
            column = column.child(
                h_flex()
                    .id(SharedString::from(format!(
                        "management-route-stage-agent-{}",
                        agent.id
                    )))
                    .debug_selector(|| format!("route-stage-agent:{index}"))
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .px_2()
                    .py_1p5()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_color(match active {
                        Some(hue) => hue.opacity(0.85),
                        None => cx.theme().border,
                    })
                    .bg(cx.theme().background)
                    .when(agent.idle, |node| node.opacity(0.62))
                    .child(agent_brand_icon(&agent.id, px(18.0), None))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .text_xs()
                                    .font_semibold()
                                    .child(agent.label.clone()),
                            )
                            .when(!agent.detail.is_empty(), |text| {
                                text.child(
                                    div()
                                        .min_w_0()
                                        .truncate()
                                        .text_size(px(10.5))
                                        .font_family(cx.theme().mono_font_family.clone())
                                        .text_color(cx.theme().muted_foreground)
                                        .child(agent.detail.clone()),
                                )
                            }),
                    )
                    .child(div().flex_none().size(px(8.0)).rounded_full().bg(hue))
                    .child(measure(geometry, Slot::Agent(index))),
            );
        }
        if self.model.agents.is_empty() {
            column = column.child(
                div()
                    .w_full()
                    .px_2()
                    .py_2()
                    .rounded(cx.theme().radius_lg)
                    .border_1()
                    .border_dashed()
                    .border_color(cx.theme().border)
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(management_locale_text(
                        "No Agent uses a route group",
                        "还没有 Agent 使用路由组",
                        "還沒有 Agent 使用路由組",
                    )),
            );
        }
        column.into_any_element()
    }

    fn render_hub(&self, looks: &StageLooks, geometry: &SharedGeometry, cx: &App) -> AnyElement {
        let hub = &self.model.hub;
        let (state_color, border) = match hub.state {
            HubState::Serving if looks.busy => (cx.theme().success, cx.theme().primary),
            HubState::Serving => (cx.theme().success, cx.theme().border),
            HubState::Degraded => (cx.theme().warning, cx.theme().warning.opacity(0.7)),
            HubState::Off => (cx.theme().muted_foreground, cx.theme().border),
        };
        v_flex()
            .id("management-route-stage-hub")
            .debug_selector(|| "route-stage-hub".to_string())
            .flex_none()
            .w(rems(9.5))
            .items_center()
            .gap_1()
            .px_2()
            .py_3()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(border)
            .when(hub.state == HubState::Off, |node| node.border_dashed())
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .max_w_full()
                    .min_w_0()
                    .gap_1p5()
                    .child(
                        Icon::default()
                            .path("icons/vibex/server.svg")
                            .size(px(14.0))
                            .text_color(cx.theme().primary),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_semibold()
                            .child(hub.title.clone()),
                    ),
            )
            .child(
                h_flex()
                    .max_w_full()
                    .min_w_0()
                    .gap_1()
                    .child(
                        div()
                            .flex_none()
                            .size(px(6.0))
                            .rounded_full()
                            .bg(state_color),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_size(px(10.5))
                            .text_color(cx.theme().muted_foreground)
                            .child(hub.detail.clone()),
                    ),
            )
            .when_some(hub.chip.clone(), |node, chip| {
                node.child(
                    div()
                        .max_w_full()
                        .truncate()
                        .mt_0p5()
                        .px_2()
                        .rounded_full()
                        .bg(cx.theme().primary.opacity(0.12))
                        .text_size(px(10.5))
                        .font_medium()
                        .text_color(cx.theme().primary)
                        .child(chip),
                )
            })
            .child(measure(geometry, Slot::Hub))
            .into_any_element()
    }

    fn render_lanes(
        &self,
        looks: &StageLooks,
        now: f64,
        still: bool,
        geometry: &SharedGeometry,
        cx: &App,
    ) -> AnyElement {
        let mut column = v_flex().flex_1().min_w(rems(15.0)).gap_2();
        if self.model.rows.is_empty() {
            return column
                .child(
                    div()
                        .w_full()
                        .px_3()
                        .py_3()
                        .rounded(cx.theme().radius_lg)
                        .border_1()
                        .border_dashed()
                        .border_color(cx.theme().border)
                        .text_xs()
                        .text_color(cx.theme().muted_foreground)
                        .child(self.model.note.clone().unwrap_or_else(|| {
                            management_locale_text(
                                "No route group yet",
                                "还没有路由组",
                                "還沒有路由組",
                            )
                            .to_string()
                        })),
                )
                .into_any_element();
        }
        for row in &self.model.rows {
            column = column.child(match row {
                StageRow::Group { title, detail } => h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_1p5()
                    .pt_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        Icon::default()
                            .path("icons/vibex/git-branch.svg")
                            .size(px(12.0))
                            .text_color(cx.theme().muted_foreground),
                    )
                    .child(
                        div()
                            .flex_none()
                            .max_w(rems(12.0))
                            .truncate()
                            .font_medium()
                            .text_color(cx.theme().foreground)
                            .child(title.clone()),
                    )
                    .child(div().min_w_0().truncate().child(detail.clone()))
                    .into_any_element(),
                StageRow::More(count) => div()
                    .w_full()
                    .px_3()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(format!(
                        "+{count} {}",
                        management_locale_text("more members", "个成员未画出", "個成員未畫出")
                    ))
                    .into_any_element(),
                StageRow::Lane(index) => self.render_lane(*index, looks, now, still, geometry, cx),
            });
        }
        column.into_any_element()
    }

    fn render_lane(
        &self,
        index: usize,
        looks: &StageLooks,
        now: f64,
        still: bool,
        geometry: &SharedGeometry,
        cx: &App,
    ) -> AnyElement {
        let Some(lane) = self.model.lanes.get(index) else {
            return div().into_any_element();
        };
        let look = looks.lanes.get(index).cloned().unwrap_or_default();
        let agent_label = |agent: usize| {
            self.model
                .agents
                .get(agent)
                .map(|agent| agent.label.clone())
                .unwrap_or_default()
        };
        let many_agents = self.model.agents.iter().filter(|agent| !agent.idle).count() > 1;
        // What the row says, most current first.
        let (status, status_color) = if let Some((_, streaming, _)) = &look.waiting {
            (
                if *streaming {
                    management_locale_text("streaming…", "正在输出…", "正在輸出…")
                } else {
                    management_locale_text("waiting for the answer…", "等待响应…", "等待回應…")
                }
                .to_string(),
                cx.theme().foreground,
            )
        } else if let Some((_, agent, _)) = &look.answered {
            (
                if many_agents {
                    format!(
                        "{} {}",
                        management_locale_text("answered", "已响应", "已回應"),
                        agent_label(*agent)
                    )
                } else {
                    management_locale_text(
                        "answered this request",
                        "响应了这次请求",
                        "回應了這次請求",
                    )
                    .to_string()
                },
                cx.theme().foreground,
            )
        } else if let Some((reason, _, last, _)) = &look.refused {
            (
                if *last {
                    format!(
                        "{reason} · {}",
                        management_locale_text("the request failed", "请求失败", "請求失敗")
                    )
                } else {
                    format!(
                        "{reason} · {}",
                        management_locale_text(
                            "went on to the next",
                            "已转到下一个",
                            "已轉到下一個"
                        )
                    )
                },
                cx.theme().danger,
            )
        } else {
            (
                lane.note.clone(),
                match lane.tone {
                    LaneTone::Cooling => cx.theme().warning,
                    LaneTone::Down => cx.theme().danger,
                    LaneTone::Ready | LaneTone::Off => cx.theme().muted_foreground,
                },
            )
        };
        let lit = look
            .waiting
            .as_ref()
            .map(|(hue, _, _)| *hue)
            .or(look.answered.as_ref().map(|(hue, _, _)| *hue));
        let border = match (&look.waiting, &look.answered, &look.refused, look.passing) {
            (Some((hue, _, _)), _, _, _) => *hue,
            (_, Some((hue, _, _)), _, _) => hue.opacity(0.75),
            (_, _, Some(_), _) => cx.theme().danger.opacity(0.55),
            (_, _, _, Some(hue)) => hue.opacity(0.45),
            _ => cx.theme().border,
        };
        let refused_at = look.refused.as_ref().map(|(_, at, _, _)| *at);
        let shake = if still {
            0.0
        } else {
            refused_at.map_or(0.0, |at| shake_offset(now - at))
        };
        let tag = look
            .refused
            .as_ref()
            .filter(|(_, at, _, _)| !still && (0.0..TAG_MS).contains(&(now - at)))
            .map(|(reason, _, _, _)| reason.clone());
        let resting = look.waiting.is_none() && look.answered.is_none() && look.refused.is_none();
        let faded = lane.tone == LaneTone::Off
            || (resting && matches!(lane.tone, LaneTone::Down | LaneTone::Cooling));
        let mark = lane
            .marks
            .iter()
            .find_map(|candidate| model_brand_icon(candidate, px(15.0), None))
            .unwrap_or_else(|| {
                div()
                    .flex_none()
                    .size(px(15.0))
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(cx.theme().radius)
                    .bg(cx.theme().muted)
                    .text_size(px(9.0))
                    .font_semibold()
                    .text_color(cx.theme().muted_foreground)
                    .child(
                        lane.title
                            .trim()
                            .chars()
                            .next()
                            .map(|character| character.to_uppercase().to_string())
                            .unwrap_or_else(|| "?".to_string()),
                    )
                    .into_any_element()
            });
        // The Key the request is on, or that answered, and the one that
        // refused it lead the chips; the rest follow in the Provider's order.
        let busy_key = look
            .waiting
            .as_ref()
            .and_then(|(_, _, key)| key.clone())
            .or_else(|| look.answered.as_ref().and_then(|(_, _, key)| key.clone()));
        let refused_key = look.refused.as_ref().and_then(|(_, _, _, key)| key.clone());
        let mut keys = lane.keys.iter().collect::<Vec<_>>();
        keys.sort_by_key(|key| {
            if Some(&key.id) == busy_key.as_ref() {
                0
            } else if Some(&key.id) == refused_key.as_ref() {
                1
            } else {
                2
            }
        });
        const SHOWN_KEYS: usize = 4;
        let hidden_keys = keys.len().saturating_sub(SHOWN_KEYS);

        v_flex()
            .id(SharedString::from(format!(
                "management-route-stage-lane-{}",
                lane.id
            )))
            .debug_selector(|| format!("route-stage-lane:{index}"))
            .relative()
            .left(px(shake))
            .w_full()
            .min_w_0()
            .gap_1()
            .px_3()
            .py_2()
            .rounded(cx.theme().radius_lg)
            .border_1()
            .border_color(border)
            .when(resting && lane.tone != LaneTone::Ready, |row| {
                row.border_dashed()
            })
            .bg(cx.theme().background)
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_2()
                    .opacity(if faded { 0.55 } else { 1.0 })
                    .child(match lit {
                        Some(hue) => div()
                            .flex_none()
                            .size(px(8.0))
                            .rounded_full()
                            .bg(hue)
                            .into_any_element(),
                        None => div()
                            .flex_none()
                            .size(px(8.0))
                            .rounded_full()
                            .border_1()
                            .border_color(cx.theme().muted_foreground)
                            .into_any_element(),
                    })
                    .child(mark)
                    .child(
                        div()
                            .min_w(rems(3.0))
                            .truncate()
                            .text_sm()
                            .font_medium()
                            .child(lane.title.clone()),
                    )
                    .when(!lane.model.is_empty(), |line| {
                        line.child(
                            div()
                                .min_w(rems(2.5))
                                .max_w(rems(14.0))
                                .truncate()
                                .px_1p5()
                                .rounded(cx.theme().radius)
                                .bg(cx.theme().muted)
                                .text_size(px(11.0))
                                .font_family(cx.theme().mono_font_family.clone())
                                .text_color(cx.theme().muted_foreground)
                                .child(lane.model.clone()),
                        )
                    })
                    .child(div().flex_1().min_w_2())
                    .child(
                        div()
                            .min_w(rems(4.0))
                            .truncate()
                            .text_xs()
                            .text_color(status_color)
                            .child(status),
                    ),
            )
            .when(!keys.is_empty(), |row| {
                row.child(
                    h_flex()
                        .w_full()
                        .min_w_0()
                        .pl(rems(2.5))
                        .gap_1()
                        .opacity(if faded { 0.55 } else { 1.0 })
                        .children(keys.iter().take(SHOWN_KEYS).map(|key| {
                            let on = Some(&key.id) == busy_key.as_ref();
                            let refused = Some(&key.id) == refused_key.as_ref() && !on;
                            let tone = match key.tone {
                                LaneTone::Ready => cx.theme().success,
                                LaneTone::Cooling => cx.theme().warning,
                                LaneTone::Down => cx.theme().danger,
                                LaneTone::Off => cx.theme().muted_foreground,
                            };
                            h_flex()
                                .min_w_0()
                                .max_w(rems(8.0))
                                .gap_1()
                                .px_1p5()
                                .rounded(cx.theme().radius)
                                .border_1()
                                .border_color(match (on, lit) {
                                    (true, Some(hue)) => hue,
                                    _ if refused => cx.theme().danger.opacity(0.6),
                                    _ => cx.theme().border.opacity(0.8),
                                })
                                .text_size(px(10.5))
                                .text_color(if on {
                                    cx.theme().foreground
                                } else {
                                    cx.theme().muted_foreground
                                })
                                .child(div().flex_none().size(px(5.0)).rounded_full().bg(tone))
                                .child(div().min_w_0().truncate().child(key.label.clone()))
                        }))
                        .when(hidden_keys > 0, |chips| {
                            chips.child(
                                div()
                                    .flex_none()
                                    .text_size(px(10.5))
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("+{hidden_keys}")),
                            )
                        }),
                )
            })
            .when_some(tag, |row, tag| {
                row.child(
                    div()
                        .absolute()
                        .top(rems(-0.65))
                        .right(rems(0.75))
                        .child(Tag::color(ColorName::Red).xsmall().child(tag)),
                )
            })
            .child(measure(geometry, Slot::Lane(index)))
            .into_any_element()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn step(lane: usize, verdict: StepVerdict, at_ms: Option<i64>) -> StageStep {
        StageStep {
            lane,
            key: None,
            verdict,
            at_ms,
        }
    }

    fn flight(
        steps: Vec<StageStep>,
        clock: FlightClock,
        ended: Option<(bool, Option<i64>)>,
    ) -> StageFlight {
        StageFlight {
            id: "request".to_string(),
            agent: 0,
            steps,
            clock,
            ended,
            streaming_since: None,
        }
    }

    /// A replay flies out, waits, and only then names the refusal; the dot
    /// goes back through the gateway to the next member and home from there.
    #[test]
    fn a_replay_reveals_each_verdict_after_its_wait() {
        let walk = flight(
            vec![
                step(0, StepVerdict::Failed("rate limited".into()), None),
                step(1, StepVerdict::Answered, None),
            ],
            FlightClock::Replay,
            Some((true, None)),
        );
        let plan = schedule(&walk, 0.0, false);
        let legs = plan.legs.iter().map(|timed| timed.leg).collect::<Vec<_>>();
        assert_eq!(
            legs,
            vec![
                Leg::Out { lane: 0 },
                Leg::Hold { lane: 0 },
                Leg::Hop { from: 0, to: 1 },
                Leg::Hold { lane: 1 },
                Leg::Home { lane: 1, ok: true },
            ]
        );
        let first_reveal = LEAD_MS + OUT_MS + HOLD_MS;
        assert_eq!(plan.reveals[0], first_reveal);
        assert_eq!(plan.reveals[1], first_reveal + HOP_MS + HOLD_MS);
        assert_eq!(plan.landed, Some(plan.reveals[1] + HOME_MS));
        // Before the refusal shows, the dot is waiting on the first member.
        assert_eq!(
            dot_at(&plan, first_reveal - 1.0),
            Some(DotAt::Waiting { lane: 0 })
        );
        assert!(matches!(
            dot_at(&plan, first_reveal + 1.0),
            Some(DotAt::Moving {
                leg: Leg::Hop { from: 0, to: 1 },
                ..
            })
        ));
        assert_eq!(dot_at(&plan, plan.landed.unwrap() + 1.0), None);
    }

    /// Another Key of the same member is a retry in place: the dot does not
    /// leave the member between the two.
    #[test]
    fn a_key_retry_on_the_same_member_keeps_the_dot_there() {
        let walk = flight(
            vec![
                step(0, StepVerdict::Failed("rate limited".into()), None),
                step(0, StepVerdict::Answered, None),
            ],
            FlightClock::Replay,
            Some((true, None)),
        );
        let plan = schedule(&walk, 0.0, false);
        assert!(
            !plan
                .legs
                .iter()
                .any(|timed| matches!(timed.leg, Leg::Hop { .. }))
        );
        assert_eq!(plan.reveals[1] - plan.reveals[0], HOLD_MS);
    }

    /// A live request waits on its member for as long as the gateway has not
    /// said anything more, and a later announcement decides the earlier step.
    #[test]
    fn a_live_request_waits_until_the_gateway_says_more() {
        let waiting = flight(
            vec![step(0, StepVerdict::Pending, Some(1_000))],
            FlightClock::Live,
            None,
        );
        let plan = schedule(&waiting, 0.0, false);
        assert_eq!(
            dot_at(&plan, 1_000.0 + OUT_MS + 60_000.0),
            Some(DotAt::Waiting { lane: 0 })
        );
        assert_eq!(plan.reveals, vec![f64::INFINITY]);

        // The second member was asked 100 ms in: the first one's refusal is
        // shown once the dot has reached it and waited a moment.
        let moved = flight(
            vec![
                step(0, StepVerdict::Failed("moved on".into()), Some(1_000)),
                step(1, StepVerdict::Answered, Some(1_100)),
            ],
            FlightClock::Live,
            Some((true, Some(1_500))),
        );
        let plan = schedule(&moved, 0.0, false);
        let reached = 1_000.0 + OUT_MS;
        assert_eq!(plan.reveals[0], reached + MIN_HOLD_MS);
        assert_eq!(plan.landed, Some(plan.reveals[1] + HOME_MS));
    }

    /// With motion reduced, nothing travels: a replay shows its outcome at
    /// once and a live request shows where it waits.
    #[test]
    fn reduced_motion_shows_outcomes_without_flights() {
        let walk = flight(
            vec![step(0, StepVerdict::Answered, None)],
            FlightClock::Replay,
            Some((true, None)),
        );
        let plan = schedule(&walk, 0.0, true);
        assert!(plan.legs.is_empty());
        assert_eq!(plan.reveals, vec![f64::NEG_INFINITY]);

        let live = flight(
            vec![step(0, StepVerdict::Pending, Some(10))],
            FlightClock::Live,
            None,
        );
        let plan = schedule(&live, 0.0, true);
        assert_eq!(dot_at(&plan, 20.0), Some(DotAt::Waiting { lane: 0 }));
    }

    /// A request refused before any member flies to the gateway and back.
    #[test]
    fn a_refused_request_turns_back_at_the_gateway() {
        let refused = flight(Vec::new(), FlightClock::Replay, Some((false, None)));
        let plan = schedule(&refused, 0.0, false);
        assert_eq!(plan.legs.len(), 1);
        assert_eq!(plan.legs[0].leg, Leg::Refused);
        assert_eq!(plan.landed, Some(LEAD_MS + REFUSED_MS));
    }

    #[test]
    fn a_shake_settles_back_to_rest() {
        assert_eq!(shake_offset(-1.0), 0.0);
        assert!(shake_offset(SHAKE_MS * 0.2) < -4.0);
        assert!(shake_offset(SHAKE_MS * 0.4) > 3.0);
        assert_eq!(shake_offset(SHAKE_MS), 0.0);
    }

    #[test]
    fn a_polyline_is_walked_by_distance() {
        let line = Polyline::from_points(vec![
            P { x: 0.0, y: 0.0 },
            P { x: 10.0, y: 0.0 },
            P { x: 10.0, y: 10.0 },
        ]);
        assert_eq!(line.length(), 20.0);
        assert_eq!(line.at(5.0), Some(P { x: 5.0, y: 0.0 }));
        assert_eq!(line.at(15.0), Some(P { x: 10.0, y: 5.0 }));
        assert_eq!(line.at(99.0), Some(P { x: 10.0, y: 10.0 }));
        assert_eq!(
            line.slice(5.0, 15.0),
            vec![
                P { x: 5.0, y: 0.0 },
                P { x: 10.0, y: 0.0 },
                P { x: 10.0, y: 5.0 }
            ]
        );
        let joined = line.clone().then(line.reversed());
        assert_eq!(joined.length(), 40.0);
    }

    /// A wire leaves its node level and arrives level, ending exactly on both
    /// nodes' edges.
    #[test]
    fn a_wire_runs_from_edge_to_edge() {
        let wire = Polyline::wire(P { x: 0.0, y: 0.0 }, P { x: 100.0, y: 40.0 });
        assert_eq!(wire.points.first(), Some(&P { x: 0.0, y: 0.0 }));
        assert_eq!(wire.points.last(), Some(&P { x: 100.0, y: 40.0 }));
        let second = wire.points[1];
        assert!(second.y.abs() < 0.5, "it leaves level");
    }

    fn lane(id: &str, tone: LaneTone) -> StageLane {
        StageLane {
            id: id.to_string(),
            marks: vec![id.to_string()],
            title: id.to_string(),
            model: "gpt-5".to_string(),
            keys: vec![
                StageKey {
                    id: format!("{id}-primary"),
                    label: "Primary".to_string(),
                    tone,
                },
                StageKey {
                    id: format!("{id}-backup"),
                    label: "Backup".to_string(),
                    tone: LaneTone::Ready,
                },
            ],
            note: "2/2 Keys ready".to_string(),
            tone,
        }
    }

    /// Two Agents, two route groups: one request replayed through a refusal
    /// to the member that answered, one still waiting on its member.
    fn busy_model(now: i64) -> StageModel {
        let agent = |id: &str| StageAgent {
            id: id.to_string(),
            label: id.to_string(),
            detail: "gpt-5".to_string(),
            idle: false,
        };
        StageModel {
            agents: vec![agent("codex"), agent("claude")],
            hub: StageHub {
                title: "Vibex Gateway".to_string(),
                detail: "1 in flight".to_string(),
                chip: Some("2 groups".to_string()),
                state: HubState::Serving,
            },
            lanes: vec![
                lane("alpha", LaneTone::Ready),
                lane("beta", LaneTone::Ready),
                lane("gamma", LaneTone::Cooling),
            ],
            rows: vec![
                StageRow::Group {
                    title: "Fast".to_string(),
                    detail: "Smart · 2 members".to_string(),
                },
                StageRow::Lane(0),
                StageRow::Lane(1),
                StageRow::Group {
                    title: "Cheap".to_string(),
                    detail: "In order · 1 member".to_string(),
                },
                StageRow::Lane(2),
                StageRow::More(3),
            ],
            flights: vec![
                StageFlight {
                    id: "replayed".to_string(),
                    agent: 0,
                    steps: vec![
                        StageStep {
                            lane: 0,
                            key: Some("alpha-primary".to_string()),
                            verdict: StepVerdict::Failed("rate limited".into()),
                            at_ms: None,
                        },
                        StageStep {
                            lane: 1,
                            key: Some("beta-primary".to_string()),
                            verdict: StepVerdict::Answered,
                            at_ms: None,
                        },
                    ],
                    clock: FlightClock::Replay,
                    ended: Some((true, None)),
                    streaming_since: None,
                },
                StageFlight {
                    id: "waiting".to_string(),
                    agent: 1,
                    steps: vec![StageStep {
                        lane: 2,
                        key: Some("gamma-backup".to_string()),
                        verdict: StepVerdict::Pending,
                        at_ms: Some(now - 5_000),
                    }],
                    clock: FlightClock::Live,
                    ended: None,
                    streaming_since: Some(now - 1_000),
                },
            ],
            pinned: Some("replayed".to_string()),
            note: None,
        }
    }

    /// The map lays its three columns out in reading order and paints a walk
    /// at every moment of it: before the dot leaves, in flight, while a
    /// refusal shows, and once home — with motion and with motion reduced.
    #[gpui::test]
    fn the_map_lays_out_and_paints_every_moment_of_a_walk(cx: &mut gpui::TestAppContext) {
        cx.update(gpui_component::init);
        let now = vibex_core::unix_timestamp_ms();
        let stage = cx.new(|_| RouteStage::new());
        stage.update(cx, |stage, _| stage.set_model(busy_model(now)));
        let (_, cx) =
            cx.add_window_view(|window, cx| gpui_component::Root::new(stage.clone(), window, cx));
        let draw = |cx: &mut gpui::VisualTestContext| {
            cx.update(|window, cx| {
                let _ = window.draw(cx);
            });
        };
        draw(cx);
        let agent = cx
            .debug_bounds("route-stage-agent:0")
            .expect("an Agent node");
        let hub = cx
            .debug_bounds("route-stage-hub")
            .expect("the gateway node");
        let first = cx.debug_bounds("route-stage-lane:0").expect("a member row");
        let last = cx.debug_bounds("route-stage-lane:2").expect("a member row");
        assert!(
            agent.right() < hub.left(),
            "Agents stand left of the gateway"
        );
        assert!(
            hub.right() < first.left(),
            "members stand right of the gateway"
        );
        assert_eq!(first.left(), last.left(), "members share one column");
        assert!(first.bottom() < last.top());

        // Every moment of the replay: leaving, waiting, refused, hopping,
        // answered, home.
        let first_reveal = LEAD_MS + OUT_MS + HOLD_MS;
        for elapsed in [
            LEAD_MS + 200.0,
            LEAD_MS + OUT_MS + 100.0,
            first_reveal + 100.0,
            first_reveal + HOP_MS / 2.0,
            first_reveal + HOP_MS + HOLD_MS + 100.0,
            first_reveal + HOP_MS + HOLD_MS + HOME_MS + 100.0,
        ] {
            stage.update(cx, |stage, cx| {
                stage.replay_from_ms = vibex_core::unix_timestamp_ms() - elapsed as i64;
                cx.notify();
            });
            draw(cx);
            assert!(cx.debug_bounds("route-stage-lane:1").is_some());
        }

        cx.update(|_, cx| cx.set_reduce_motion(true));
        stage.update(cx, |stage, cx| stage.replay(cx));
        draw(cx);
        assert!(cx.debug_bounds("route-stage-hub").is_some());
    }
}

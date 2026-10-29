//! Each guardian's own rhythm along the composer companion's lane.
//!
//! The home page roams a closed route on one shared loop, which side-on is the
//! same slow sway for every guardian. A lane only shows travel, so here every
//! form keeps an itinerary of its own: where it stops, how long it rests, how it
//! covers the ground in between, and how long its lap takes. The spark hops in
//! bursts, the knot rolls with momentum and rocks to a stop, Pi strides and
//! looks around, the chest lumbers and peeks, the whale glides and breaches,
//! and Copilot darts between hovering spots.

use super::{
    geometry::{Vec2, smoothstep},
    guardian::{Boss, Guardian, beat, landing_squash},
    map::CENTER,
};
use std::f32::consts::{PI, TAU};

/// How far an itinerary's ends reach either side of the arena's centre, in
/// world units. A lane maps this reach onto its own width.
pub(super) const LANE_REACH: f32 = 72.0;
/// The arena row a lane pose stands on. A lane never shows depth; a duel opened
/// from the lane starts from here, in the upper half clear of the archer.
const LANE_ROW: f32 = -20.0;

/// One stretch of an itinerary: rest `hold` seconds where the previous stretch
/// ended, then travel to `to` — in lane units, -1 to 1 — over `travel` seconds.
/// The last stretch ends where the first begins, so a lap closes on itself.
#[derive(Clone, Copy, Debug)]
struct Leg {
    hold: f32,
    travel: f32,
    to: f32,
}
const fn leg(hold: f32, travel: f32, to: f32) -> Leg {
    Leg { hold, travel, to }
}

const CLAUDE_HOP: f32 = 0.85;
/// A burst of hops, a breath, a short burst on, then one long run back. The
/// hops there and back cancel, so the spin they roll closes over the lap.
const CLAUDE: [Leg; 3] = [
    leg(1.8, 4.0 * CLAUDE_HOP, 0.1),
    leg(0.8, 3.0 * CLAUDE_HOP, 0.92),
    leg(2.2, 7.0 * CLAUDE_HOP, -0.92),
];
/// Long rolls with a brake halfway home: the wheel stops without turning
/// around, then rolls on.
const CODEX: [Leg; 3] = [
    leg(1.8, 6.0, 0.92),
    leg(1.4, 3.0, 0.3),
    leg(1.0, 4.4, -0.92),
];
/// Whole turns the knot rolls over its lap.
const CODEX_TURNS: f32 = 3.0;
const PI_STEP: f32 = 1.2;
/// Steady strides with a pause mid-lane, then the whole way back in one walk.
const PI_WALK: [Leg; 3] = [
    leg(2.6, 5.0 * PI_STEP, -0.05),
    leg(1.8, 5.0 * PI_STEP, 0.9),
    leg(2.4, 10.0 * PI_STEP, -0.9),
];
const OPENCODE_HOP: f32 = 1.8;
/// Heavy hops with a long sit mid-lane, where the lid peeks open.
const OPENCODE: [Leg; 3] = [
    leg(2.0, 3.0 * OPENCODE_HOP, -0.05),
    leg(3.4, 3.0 * OPENCODE_HOP, 0.92),
    leg(1.6, 6.0 * OPENCODE_HOP, -0.92),
];
/// The whale never stops: each rest is a U-turn swum at the lane's end.
const DEEPSEEK: [Leg; 2] = [leg(2.0, 8.6, 0.88), leg(2.0, 8.6, -0.88)];
/// Where the outbound pass breaches, as a share of its swim. Only that pass
/// breaches: the window is cut for the breach the home page swims, heading
/// right, and the raised tail of a breach heading left would leave it.
const BREACH: f32 = 0.35;
/// Hover, dart, hover: uneven spots, uneven pauses, one barrel roll per lap.
const COPILOT: [Leg; 7] = [
    leg(2.2, 1.0, -0.2),
    leg(1.4, 0.7, 0.15),
    leg(2.4, 1.3, 0.9),
    leg(1.8, 0.9, 0.35),
    leg(1.0, 0.6, 0.6),
    leg(2.6, 1.2, -0.25),
    leg(1.4, 1.0, -0.9),
];
const COPILOT_ROLL: usize = 2;

fn itinerary(guardian: Guardian) -> &'static [Leg] {
    match guardian {
        Guardian::Claude => &CLAUDE,
        Guardian::Codex => &CODEX,
        Guardian::Pi => &PI_WALK,
        Guardian::OpenCode => &OPENCODE,
        Guardian::DeepSeek => &DEEPSEEK,
        Guardian::Copilot => &COPILOT,
    }
}
/// How long one guardian takes to walk its whole itinerary once.
pub(super) fn lane_lap(guardian: Guardian) -> f32 {
    lap(itinerary(guardian))
}
fn lap(legs: &[Leg]) -> f32 {
    legs.iter().map(|leg| leg.hold + leg.travel).sum()
}

/// Where an itinerary stands: the stretch under way and the seconds spent on it.
#[derive(Clone, Copy, Debug)]
struct Moment {
    index: usize,
    leg: Leg,
    from: f32,
    elapsed: f32,
}
impl Moment {
    fn at(legs: &'static [Leg], time: f32) -> Self {
        let mut elapsed = time.rem_euclid(lap(legs));
        for (index, leg) in legs.iter().enumerate() {
            let span = leg.hold + leg.travel;
            if elapsed < span || index + 1 == legs.len() {
                return Self {
                    index,
                    leg: *leg,
                    from: start(legs, index),
                    elapsed: elapsed.min(span),
                };
            }
            elapsed -= span;
        }
        unreachable!("an itinerary has at least one stretch")
    }
    /// Seconds into the rest, while the guardian still rests.
    fn resting(self) -> Option<f32> {
        (self.elapsed < self.leg.hold).then_some(self.elapsed)
    }
    /// Seconds into the travel, once the guardian has left.
    fn travelling(self) -> Option<f32> {
        (self.elapsed >= self.leg.hold).then_some(self.elapsed - self.leg.hold)
    }
    /// The share of the travel covered in time, 0 while resting.
    fn progress(self) -> f32 {
        self.travelling()
            .map_or(0.0, |elapsed| (elapsed / self.leg.travel).min(1.0))
    }
    fn heading(self) -> f32 {
        if self.leg.to >= self.from { 1.0 } else { -1.0 }
    }
}
fn previous(legs: &[Leg], index: usize) -> usize {
    (index + legs.len() - 1) % legs.len()
}
fn start(legs: &[Leg], index: usize) -> f32 {
    legs[previous(legs, index)].to
}
fn heading_of(legs: &[Leg], index: usize) -> f32 {
    if legs[index].to >= start(legs, index) {
        1.0
    } else {
        -1.0
    }
}
/// Whether the rest before stretch `index` turns the guardian around.
fn turns(legs: &[Leg], index: usize) -> bool {
    heading_of(legs, previous(legs, index)) != heading_of(legs, index)
}
/// Half-turns completed since the lap began, plus `progress` of the one under
/// way. Every turn goes the same way round, and a closed itinerary turns an
/// even number of times, so the facing closes over the lap.
fn half_turns(legs: &[Leg], moment: Moment, progress: f32) -> f32 {
    let done = (0..moment.index).filter(|&ix| turns(legs, ix)).count() as f32;
    let current = if moment.resting().is_some() {
        if turns(legs, moment.index) {
            smoothstep(progress)
        } else {
            0.0
        }
    } else {
        f32::from(turns(legs, moment.index))
    };
    done + current
}

/// The angle of the heading a lap starts with: 0 to the right, PI to the left.
fn heading_angle(legs: &[Leg]) -> f32 {
    if heading_of(legs, legs.len() - 1) > 0.0 {
        0.0
    } else {
        PI
    }
}
/// The lane units travelled before stretch `index`.
fn travelled(legs: &[Leg], index: usize) -> f32 {
    (0..index)
        .map(|ix| (legs[ix].to - start(legs, ix)).abs())
        .sum()
}

/// Distance covered by `t` of a stretch that speeds up over its first `ramp`,
/// cruises, and slows over its last `ramp`, as a share of the stretch.
fn cruise(t: f32, ramp: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    let top = 1.0 / (1.0 - ramp);
    if t < ramp {
        top * t * t / (2.0 * ramp)
    } else if t > 1.0 - ramp {
        1.0 - top * (1.0 - t).powi(2) / (2.0 * ramp)
    } else {
        top * (t - ramp * 0.5)
    }
}
fn smootherstep(t: f32) -> f32 {
    let t = t.clamp(0.0, 1.0);
    t * t * t * (t * (t * 6.0 - 15.0) + 10.0)
}

/// A run of equal hops: how far it has gone as a share of the run, the hop
/// count covered so far, the current arc from 0 to 1, and the body's squash.
struct Hops {
    share: f32,
    count: f32,
    arc: f32,
    squash: f32,
    /// How far into the current hop, from 0 to 1.
    beat: f32,
}
fn hopping(elapsed: f32, travel: f32, period: f32, air: f32) -> Hops {
    let total = (travel / period).round().max(1.0);
    let (hop, u) = if elapsed >= travel {
        (total - 1.0, 1.0)
    } else {
        beat(elapsed, period)
    };
    let flight = (u / air).min(1.0);
    // The run's last landing settles instead of crouching for a hop that does
    // not come; the rest that follows crouches again before take-off.
    let squash = if hop + 1.0 >= total && u > air + 0.25 {
        0.0
    } else {
        landing_squash(u, air)
    };
    let count = hop + smoothstep(flight);
    Hops {
        share: count / total,
        count,
        arc: if u < air { (flight * PI).sin() } else { 0.0 },
        squash,
        beat: u,
    }
}
/// The crouch that ends a rest, timed like the crouch that ends a hop.
fn crouch(elapsed: f32, hold: f32, period: f32, air: f32) -> f32 {
    let remaining = hold - elapsed;
    if remaining < 0.15 * period {
        landing_squash(1.0 - remaining / period, air)
    } else {
        0.0
    }
}
fn wrap(angle: f32) -> f32 {
    (angle + PI).rem_euclid(TAU) - PI
}

impl Boss {
    /// The pose a guardian holds `time` seconds into its lane, on its own
    /// itinerary and lap. The pose clock is monotonic: every itinerary closes
    /// on itself, so a long turn never cuts back to a first frame mid-lane.
    pub fn lane(guardian: Guardian, time: f32) -> Self {
        let mut boss = Self::new(guardian);
        let legs = itinerary(guardian);
        let lap = lane_lap(guardian);
        let time = time.rem_euclid(lap);
        let phase = time / lap;
        let moment = Moment::at(legs, time);
        let (from, to, heading) = (moment.from, moment.leg.to, moment.heading());
        let span = to - from;
        let place = match guardian {
            Guardian::Claude => {
                let signed =
                    |ix: usize| heading_of(legs, ix) * (legs[ix].travel / CLAUDE_HOP).round();
                let before: f32 = (0..moment.index).map(signed).sum();
                let (place, rolled) = match moment.travelling() {
                    Some(elapsed) => {
                        let hops = hopping(elapsed, moment.leg.travel, CLAUDE_HOP, 0.45);
                        boss.height = hops.arc * 9.0;
                        boss.squash = hops.squash;
                        (from + span * hops.share, heading * hops.count)
                    }
                    None => {
                        boss.squash = crouch(moment.elapsed, moment.leg.hold, CLAUDE_HOP, 0.45);
                        (from, 0.0)
                    }
                };
                // The spark rolls the way it hops, an eighth of a turn a hop.
                boss.spin = (before + rolled) * TAU / 8.0;
                boss.yaw = place * 0.3;
                place
            }
            Guardian::Codex => {
                let (place, rolled, turn) = match moment.resting() {
                    Some(elapsed) => {
                        // Momentum carries the wheel a touch past its stop; it
                        // rocks back, settles, and only then pivots.
                        let arrived = heading_of(legs, previous(legs, moment.index));
                        let settle = 1.0 - smoothstep(elapsed / 0.8);
                        let rock = (elapsed * TAU / 0.55).sin() * 0.035 * settle;
                        let turn = (elapsed - 0.6) / (moment.leg.hold - 0.6);
                        (from + arrived * rock, rock, turn)
                    }
                    None => {
                        let covered = span.abs() * cruise(moment.progress(), 0.3);
                        (from + heading * covered, covered, 0.0)
                    }
                };
                // Rolling without slipping: the wheel turns with the ground it
                // covers, a whole number of turns per lap so the lap closes.
                let roll = CODEX_TURNS * TAU / travelled(legs, legs.len());
                boss.spin = (travelled(legs, moment.index) + rolled) * roll;
                boss.height = ((boss.spin * 6.0).sin() * 0.35).max(0.0);
                boss.yaw = wrap(heading_angle(legs) + PI * half_turns(legs, moment, turn));
                place
            }
            Guardian::Pi => {
                let place = match moment.travelling() {
                    Some(_) => from + span * cruise(moment.progress(), 0.1),
                    None => from,
                };
                boss.yaw = place * 0.25;
                if let Some(elapsed) = moment.resting() {
                    // A look to either side before the next walk.
                    boss.yaw += (elapsed / moment.leg.hold * TAU).sin() * 0.15;
                }
                boss.position = lane_point(place);
                boss.rest_hands();
                if let Some(elapsed) = moment.travelling() {
                    // Strides fade in and out with the walk, so the stems stand
                    // together whenever the glyph stops.
                    let travel = moment.leg.travel;
                    let stride = smoothstep(elapsed.min(travel - elapsed) / 0.6);
                    let (step, u) = beat(elapsed.min(travel - 1e-3), PI_STEP);
                    let swing = step as usize % 2;
                    let forward = Vec2::new(heading, 0.0);
                    for ix in 0..2 {
                        let offset = if ix == swing {
                            -4.0 + smoothstep(u) * 8.0
                        } else {
                            4.0 - u * 8.0
                        };
                        boss.hands[ix] = boss.resting_hand(ix).plus(forward.scale(offset * stride));
                        boss.hand_heights[ix] = if ix == swing {
                            (u * PI).sin() * 4.5 * stride
                        } else {
                            0.0
                        };
                    }
                    boss.bank =
                        if swing == 0 { 1.0 } else { -1.0 } * (u * PI).sin() * 0.07 * stride;
                }
                place
            }
            Guardian::OpenCode => {
                let place = match moment.travelling() {
                    Some(elapsed) => {
                        let hops = hopping(elapsed, moment.leg.travel, OPENCODE_HOP, 0.4);
                        boss.height = hops.arc * 8.0;
                        boss.squash = hops.squash;
                        boss.lid = if hops.beat < 0.4 {
                            hops.arc * 0.55
                        } else if (0.55..0.8).contains(&hops.beat) {
                            ((hops.beat - 0.55) * PI * 12.0).sin().abs() * 0.3
                        } else {
                            0.0
                        };
                        from + span * hops.share
                    }
                    None => {
                        let (elapsed, hold) = (moment.elapsed, moment.leg.hold);
                        boss.squash = crouch(elapsed, hold, OPENCODE_HOP, 0.4);
                        if hold >= 3.0 {
                            // A long sit: the lid lifts to peek out, chatters,
                            // and drops shut before the next hop.
                            let peek = ((elapsed - 0.6) / (hold - 1.4)).clamp(0.0, 1.0);
                            let open = (peek * PI).sin();
                            boss.lid = open * 0.4 + (peek * PI * 8.0).sin().abs() * 0.08 * open;
                        }
                        from
                    }
                };
                boss.yaw = place * 0.35;
                place
            }
            Guardian::DeepSeek => {
                boss.height = 1.5 + (phase * TAU * 9.0).sin() * 0.6;
                boss.pitch = (phase * TAU * 5.0).sin() * 0.05;
                let (place, turn) = match moment.resting() {
                    Some(elapsed) => {
                        // A U-turn at the lane's end: out a little, round, and
                        // back along the lane the other way.
                        let turn = elapsed / moment.leg.hold;
                        let arrived = heading_of(legs, previous(legs, moment.index));
                        boss.bank = (turn * PI).sin() * 0.12;
                        (from + arrived * 0.05 * (turn * PI).sin().powi(2), turn)
                    }
                    None => {
                        // Tail kicks: the swim surges and glides three times a pass.
                        let t = moment.progress();
                        let kicks = 3.0;
                        let surge = t - 0.55 * (t * TAU * kicks).sin() / (TAU * kicks);
                        let breach = (t - BREACH) / 0.3;
                        if heading > 0.0 && (0.0..1.0).contains(&breach) {
                            let arc = (breach * PI).sin();
                            boss.height += arc * 10.4;
                            boss.pitch += (0.5 - breach) * arc;
                            boss.unravel = arc;
                        }
                        (from + span * smoothstep(surge), 0.0)
                    }
                };
                boss.yaw =
                    wrap(heading_angle(legs) - PI * 0.5 + PI * half_turns(legs, moment, turn));
                place
            }
            Guardian::Copilot => {
                boss.height = 8.0 + (phase * TAU * 3.0).sin();
                let (place, lean) = match moment.travelling() {
                    Some(_) => {
                        let t = moment.progress();
                        // Leaning with the dart's speed, fully over at 1.2 lane
                        // units a second.
                        let speed = span.abs() / moment.leg.travel * 30.0 * (t * (1.0 - t)).powi(2);
                        let lean = (speed / 1.2).min(1.0);
                        if moment.index == COPILOT_ROLL {
                            boss.bank = TAU * smoothstep(t);
                        }
                        (from + span * smootherstep(t), heading * lean)
                    }
                    None => (from, 0.0),
                };
                // A hover holds its spot but is never quite still: it looks
                // about before the next dart.
                let glance = moment.resting().map_or(0.0, |elapsed| {
                    (elapsed / moment.leg.hold * TAU).sin() * 0.15
                });
                boss.yaw = (lean * 0.45 + glance).clamp(-0.5, 0.5);
                boss.bank += -lean * 0.3;
                boss.pitch = lean.abs() * 0.12;
                place
            }
        };
        boss.position = lane_point(place);
        boss.direction = Vec2::from_angle(boss.yaw + PI * 0.5);
        boss.previous_position = boss.position;
        if guardian != Guardian::Pi {
            boss.rest_hands();
        }
        boss.update_core();
        boss.previous_core = boss.core;
        boss
    }
}
fn lane_point(place: f32) -> Vec2 {
    CENTER.plus(Vec2::new(place * LANE_REACH, LANE_ROW))
}

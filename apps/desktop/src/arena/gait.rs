//! Each guardian's own rhythm along the composer companion's lane.
//!
//! The home page roams a closed route on one shared loop, which side-on is the
//! same slow sway for every guardian. A lane only shows travel, so here every
//! form keeps an itinerary of its own: where it stops, how long it rests, how it
//! covers the ground in between, and how long its lap takes. The spark hops in
//! bursts, the knot rolls with momentum and rocks to a stop, Pi strides and
//! looks around, the chest lumbers and peeks, the whale swims steadily and breaches,
//! and Copilot darts between hovering spots.
//!
//! Every guardian also enters and leaves its own way: the spark falls in and
//! flares out upward, the knot rolls in and off an end, Pi climbs out of the
//! composer and steps back down into it, the chest pops out and jumps back in,
//! the whale surfaces and sounds, and Copilot swoops in and climbs away.

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
/// The whale swims at one steady speed and never stops: each rest is a
/// U-turn swum at the lane's end, at the same speed as the passes.
const DEEPSEEK: [Leg; 2] = [leg(2.0, 8.6, 0.85), leg(2.0, 8.6, -0.85)];
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
        Guardian::Cursor => &cursor::LEGS,
        Guardian::Cline => &cline::LEGS,
        Guardian::Grok => &grok::LEGS,
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
/// Half-turns completed since the lap began, plus `progress` — already eased
/// as the gait turns — of the one under way. Every turn goes the same way round, and a closed itinerary turns an
/// even number of times, so the facing closes over the lap.
fn half_turns(legs: &[Leg], moment: Moment, progress: f32) -> f32 {
    let done = (0..moment.index).filter(|&ix| turns(legs, ix)).count() as f32;
    let current = if moment.resting().is_some() {
        if turns(legs, moment.index) {
            progress.clamp(0.0, 1.0)
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
                boss.yaw =
                    wrap(heading_angle(legs) + PI * half_turns(legs, moment, smoothstep(turn)));
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
                        // A U-turn at the lane's end: half a circle seen side-on,
                        // entered and left at the speed of the passes, so the
                        // swim never slows or surges.
                        let turn = elapsed / moment.leg.hold;
                        let arrived = heading_of(legs, previous(legs, moment.index));
                        let speed = span.abs() / moment.leg.travel;
                        let radius = speed * moment.leg.hold / PI;
                        boss.bank = (turn * PI).sin() * 0.12;
                        (from + arrived * radius * (turn * PI).sin(), turn)
                    }
                    None => {
                        let t = moment.progress();
                        let breach = (t - BREACH) / 0.3;
                        if heading > 0.0 && (0.0..1.0).contains(&breach) {
                            let arc = (breach * PI).sin();
                            boss.height += arc * 10.4;
                            boss.pitch += (0.5 - breach) * arc;
                            boss.unravel = arc;
                        }
                        (from + span * t, 0.0)
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
            Guardian::Cursor => cursor::lane(&mut boss, legs, moment),
            Guardian::Cline => cline::lane(&mut boss, moment, phase),
            Guardian::Grok => grok::lane(&mut boss, legs, moment, phase),
        };
        boss.position = lane_point(place);
        if guardian != Guardian::Pi {
            boss.rest_hands();
        }
        settled(boss)
    }
}
fn lane_point(place: f32) -> Vec2 {
    CENTER.plus(Vec2::new(place * LANE_REACH, LANE_ROW))
}
/// Derives what follows from a pose once its fields are set.
fn settled(mut boss: Boss) -> Boss {
    boss.direction = Vec2::from_angle(boss.yaw + PI * 0.5);
    boss.previous_position = boss.position;
    boss.update_core();
    boss.previous_core = boss.core;
    boss
}

/// A companion as drawn: its pose, and how far it stands off its lane place
/// while it enters or leaves.
#[derive(Clone, Copy, Debug)]
pub(super) struct Shot {
    pub boss: Boss,
    /// Travel along the lane past the pose's own place, as a share of the way
    /// to being wholly out of sight past the lane's end: 1 is gone past the
    /// right end, -1 past the left one.
    pub slide: f32,
    /// Height off the lane, in lane heights: 1 is wholly sunk into the
    /// composer below, -1 wholly risen out of the lane's top.
    pub lift: f32,
    pub opacity: f32,
}
impl Shot {
    fn on_lane(boss: Boss) -> Self {
        Self {
            boss,
            slide: 0.0,
            lift: 0.0,
            opacity: 1.0,
        }
    }
}

/// How long a guardian takes to enter its lane when a turn starts.
pub(super) fn entrance(guardian: Guardian) -> f32 {
    match guardian {
        Guardian::Claude => 0.9,
        Guardian::Codex => 1.2,
        Guardian::Pi => 1.6,
        Guardian::OpenCode => 1.1,
        Guardian::DeepSeek => 1.5,
        Guardian::Copilot => 1.3,
        Guardian::Cursor => cursor::ENTRANCE,
        Guardian::Cline => cline::ENTRANCE,
        Guardian::Grok => grok::ENTRANCE,
    }
}
/// How long a guardian takes to leave its lane when the turn ends.
pub(super) fn exit(guardian: Guardian) -> f32 {
    match guardian {
        Guardian::Claude => 0.85,
        Guardian::Codex => 1.1,
        Guardian::Pi => 1.3,
        Guardian::OpenCode => 0.8,
        Guardian::DeepSeek => 1.25,
        Guardian::Copilot => 0.95,
        Guardian::Cursor => cursor::EXIT,
        Guardian::Cline => cline::EXIT,
        Guardian::Grok => grok::EXIT,
    }
}

/// The companion `time` seconds into its lane. Before 0 it is still entering:
/// each guardian arrives its own way and lands exactly on the lane's first
/// pose, so the entrance hands over to the itinerary without a cut.
pub(super) fn shot(guardian: Guardian, time: f32) -> Shot {
    let base = Boss::lane(guardian, time.max(0.0));
    if time >= 0.0 {
        return Shot::on_lane(base);
    }
    let a = ((time + entrance(guardian)) / entrance(guardian)).clamp(0.0, 1.0);
    let mut shot = Shot::on_lane(base);
    let boss = &mut shot.boss;
    match guardian {
        Guardian::Claude => {
            // The spark falls in from above the lane, whirling, lands flat,
            // and bounces once.
            if a < 0.5 {
                shot.lift = -1.15 * (1.0 - (a / 0.5).powi(2));
                boss.squash = -0.3;
            } else if a < 0.7 {
                boss.squash = 0.8 * (1.0 - smoothstep((a - 0.5) / 0.2));
            } else {
                let bounce = ((a - 0.7) / 0.3 * PI).sin();
                boss.height = bounce * 4.0;
                boss.squash = -0.2 * bounce;
            }
            boss.spin -= TAU * 0.75 * (1.0 - smoothstep(a));
        }
        Guardian::Codex => {
            // The knot rolls in from past the lane's left end and slows to a
            // stop, turning with the ground it covers.
            let left = (1.0 - a).powi(2);
            shot.slide = -left;
            boss.spin += left * TAU * 2.5;
            boss.height = ((boss.spin * 6.0).sin() * 0.35).max(0.0);
        }
        Guardian::Pi => {
            // The glyph climbs out of the composer one stem at a time.
            let (ix, from, to, u) = if a < 0.5 {
                (0, 1.05, 0.5, a / 0.5)
            } else {
                (1, 0.5, 0.0, (a - 0.5) / 0.5)
            };
            shot.lift = from + (to - from) * smoothstep(u);
            boss.hand_heights[ix] = (u * PI).sin() * 4.5;
            boss.bank = if ix == 0 { 1.0 } else { -1.0 } * (u * PI).sin() * 0.07;
        }
        Guardian::OpenCode => {
            // The chest pops out of the composer with its lid flung open,
            // lands hard, and chatters shut.
            if a < 0.65 {
                let p = a / 0.65;
                shot.lift = 1.0 - p - 0.75 * (p * PI).sin();
                boss.lid = 0.55;
                boss.squash = -0.3 * (p * PI).sin();
            } else {
                let p = (a - 0.65) / 0.35;
                boss.squash = 0.8 * (1.0 - smoothstep(p / 0.55));
                boss.lid = (p * PI * 3.0).sin().abs() * 0.3 * (1.0 - p);
            }
        }
        Guardian::DeepSeek => {
            // The whale surfaces nose first and bobs as it levels out.
            if a < 0.7 {
                let p = a / 0.7;
                shot.lift = 1.0 - smoothstep(p);
                boss.pitch += 0.3 * (p * PI).sin();
            } else {
                shot.lift = -0.06 * ((a - 0.7) / 0.3 * PI).sin();
            }
        }
        Guardian::Copilot => {
            // Copilot swoops in from above the left end, rolling once as it
            // brakes into a hover.
            let left = (1.0 - a).powi(2);
            shot.slide = -left;
            shot.lift = -0.9 * left;
            boss.bank += TAU * smoothstep(a);
            boss.yaw = (boss.yaw + 0.45 * (1.0 - a)).clamp(-0.5, 0.5);
            boss.pitch += 0.12 * (1.0 - a);
        }
        Guardian::Cursor => cursor::entering(&mut shot, a),
        Guardian::Cline => cline::entering(&mut shot, a),
        Guardian::Grok => grok::entering(&mut shot, a),
    }
    shot.boss = settled(shot.boss);
    shot
}

/// The companion leaving: `from` is where on its lane the turn ended, and
/// `time` runs on from there. Each guardian leaves its own way, starting from
/// exactly the pose it held, and is wholly out of sight by [`exit`].
pub(super) fn leaving(guardian: Guardian, from: f32, time: f32) -> Shot {
    let mut shot = shot(guardian, from);
    let a = ((time - from) / exit(guardian)).clamp(0.0, 1.0);
    let place = (shot.boss.position.x - CENTER.x) / LANE_REACH;
    // Whatever leaves along the lane takes the nearer end.
    let side = if place >= 0.0 { 1.0 } else { -1.0 };
    let boss = &mut shot.boss;
    let (slide, lift, opacity) = match guardian {
        Guardian::Claude => {
            // A crouch, then the spark flares up out of the lane, whirling.
            boss.squash = if a < 0.15 {
                boss.squash + (0.5 - boss.squash) * smoothstep(a / 0.15)
            } else {
                -0.3
            };
            let p = ((a - 0.15) / 0.85).max(0.0);
            boss.spin += TAU * 1.5 * p * p;
            (0.0, -1.2 * p * p, 1.0 - smoothstep((a - 0.7) / 0.3))
        }
        Guardian::Codex => {
            // The knot pivots toward the nearer end and rolls off it.
            let heading = if side > 0.0 { 0.0 } else { PI };
            boss.yaw = wrap(boss.yaw + wrap(heading - boss.yaw) * smoothstep(a / 0.3));
            let p = ((a - 0.3) / 0.7).max(0.0);
            boss.spin += TAU * 3.0 * p * p;
            boss.height = ((boss.spin * 6.0).sin() * 0.35).max(0.0);
            (side * p * p, 0.0, 1.0)
        }
        Guardian::Pi => {
            // The glyph gathers its stems and steps down into the composer.
            let gather = smoothstep(a / 0.2);
            for ix in 0..2 {
                let rest = boss.resting_hand(ix);
                boss.hands[ix] = boss.hands[ix].lerp(rest, gather);
                boss.hand_heights[ix] *= 1.0 - gather;
            }
            boss.bank *= 1.0 - gather;
            let (ix, from, to, u) = if a < 0.6 {
                (0, 0.0, 0.5, ((a - 0.2) / 0.4).max(0.0))
            } else {
                (1, 0.5, 1.1, (a - 0.6) / 0.4)
            };
            if u > 0.0 {
                boss.hand_heights[ix] = (u * PI).sin() * 4.5;
            }
            (0.0, from + (to - from) * smoothstep(u), 1.0)
        }
        Guardian::OpenCode => {
            // The chest gapes and jumps into the composer, lid first.
            boss.lid += (0.55 - boss.lid) * smoothstep(a / 0.3);
            boss.squash += (-0.3 - boss.squash) * smoothstep(a / 0.2);
            let lift = if a < 0.35 {
                -0.3 * (a / 0.7 * PI).sin()
            } else {
                -0.3 + 1.45 * ((a - 0.35) / 0.65).powi(2)
            };
            (0.0, lift, 1.0)
        }
        Guardian::DeepSeek => {
            // The whale noses down, raises its tail, and sounds.
            boss.pitch -= 0.45 * smoothstep(a / 0.4);
            boss.unravel = boss.unravel.max(smoothstep(a / 0.5));
            (0.0, 1.15 * smoothstep((a - 0.2) / 0.8), 1.0)
        }
        Guardian::Copilot => {
            // Copilot banks toward the nearer end and climbs away past it.
            let turn = smoothstep(a / 0.3);
            boss.yaw = (boss.yaw + side * 0.45 * turn).clamp(-0.5, 0.5);
            boss.bank -= side * 0.3 * turn;
            boss.pitch += (0.12 - boss.pitch) * turn;
            (side * a * a, -0.5 * a * a, 1.0)
        }
        Guardian::Cursor => cursor::leaving(boss, a),
        Guardian::Cline => cline::leaving(boss, a),
        Guardian::Grok => grok::leaving(boss, a, side),
    };
    // A turn that ends mid-entrance leaves from where the entrance had got to;
    // that offset hands over to the exit's own as it goes.
    let entering = 1.0 - a;
    shot.slide = shot.slide * entering + slide;
    shot.lift = shot.lift * entering + lift;
    shot.opacity = (shot.opacity + (1.0 - shot.opacity) * a) * opacity;
    shot.boss = settled(shot.boss);
    shot
}

#[path = "gait_cline.rs"]
mod cline;
#[path = "gait_cursor.rs"]
mod cursor;
#[path = "gait_grok.rs"]
mod grok;

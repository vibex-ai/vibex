//! Grok on the lane: it drifts still, spins its blade up, and slashes across
//! in one dash that skids to a stop.

use super::*;

/// Dashes of uneven length, with long waits between.
pub(super) const LEGS: [Leg; 5] = [
    leg(3.4, 1.3, -0.2),
    leg(3.0, 1.0, 0.3),
    leg(2.8, 1.1, 0.92),
    leg(2.4, 1.5, 0.0),
    leg(2.4, 1.5, -0.92),
];
pub(super) const ENTRANCE: f32 = 1.05;
pub(super) const EXIT: f32 = 0.9;
/// How long the blade spins up before a dash.
const SPIN_UP: f32 = 0.5;

pub(super) fn lane(boss: &mut Boss, legs: &[Leg], moment: Moment, phase: f32) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    let leg = moment.leg;
    boss.height = 1.5 + (phase * TAU * 6.0).sin() * 0.4;
    // One whole turn of the blade a dash, from its spin-up to its stop.
    let turning = (moment.elapsed - (leg.hold - SPIN_UP)) / (SPIN_UP + leg.travel);
    boss.spin = (moment.index as f32 + smoothstep(turning)) * TAU;
    match moment.travelling() {
        Some(_) => {
            let t = moment.progress();
            let rush = (t * PI).sin();
            let heading = moment.heading();
            boss.yaw = -heading * (0.25 + 0.35 * rush);
            boss.bank = heading * 0.18 * rush;
            from + span * dash(t)
        }
        None => {
            let arrived = heading_of(legs, previous(legs, moment.index));
            let turn = smoothstep(moment.elapsed / leg.hold);
            boss.yaw = -(arrived + (moment.heading() - arrived) * turn) * 0.25;
            from
        }
    }
}

/// A dash's share of its distance at `t`: hard off the mark, at its fastest
/// a third of the way, then a long skid to a stop.
fn dash(t: f32) -> f32 {
    1.0 - (1.0 - smoothstep(t)).powi(2)
}

/// Yanked in after its blade across the whole lane, from past the right end.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let carried = 1.0 - (1.0 - a).powi(3);
    shot.slide = 1.0 - carried;
    let boss = &mut shot.boss;
    boss.spin -= TAU * 2.0 * (1.0 - carried);
    boss.bank = -(a * PI).sin() * 0.18;
}

/// It spins its blade up on the spot, then slashes off past the far end.
pub(super) fn leaving(boss: &mut Boss, a: f32, side: f32) -> (f32, f32, f32) {
    boss.spin += TAU * 3.0 * a * a;
    let p = ((a - 0.35) / 0.65).max(0.0);
    let far = -side;
    boss.bank += far * 0.25 * smoothstep(p / 0.3);
    (far * p * p, 0.0, 1.0)
}

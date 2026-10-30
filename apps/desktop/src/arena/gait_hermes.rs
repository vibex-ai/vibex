//! Hermes on the lane: a messenger's swift swoops, dipping low mid-flight,
//! and long hovers between them, her hair stirring.

use super::*;

pub(super) const LEGS: [Leg; 5] = [
    leg(3.6, 2.2, -0.1),
    leg(4.0, 2.0, 0.9),
    leg(3.6, 2.6, 0.2),
    leg(3.8, 2.3, -0.4),
    leg(3.6, 2.0, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.3;
pub(super) const EXIT: f32 = 1.25;
/// Bobs a lap while she hovers.
const BOBS: f32 = 16.0;

pub(super) fn lane(boss: &mut Boss, moment: Moment, phase: f32) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    let bob = (phase * TAU * BOBS).sin() * 0.7;
    match moment.travelling() {
        Some(_) => {
            let t = moment.progress();
            let swoop = (t * PI).sin();
            boss.height = 4.0 + bob - 2.4 * swoop;
            boss.pitch = -0.15 * swoop;
            boss.yaw = -moment.heading() * 0.35 * swoop;
            from + span * smootherstep(t)
        }
        None => {
            boss.height = 4.0 + bob;
            from
        }
    }
}

/// She swoops up out of the composer from past the lane's right end.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let carried = 1.0 - (1.0 - a).powi(2);
    shot.slide = 1.0 - carried;
    shot.lift = 0.6 * (1.0 - a).powi(2) - 0.25 * (a * PI).sin();
    shot.boss.yaw += 0.4 * (1.0 - a);
}

/// She climbs away to the upper right, twirling once as she goes.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    boss.yaw += TAU * smoothstep(a);
    let p = a * a;
    (p, -1.1 * p, 1.0)
}

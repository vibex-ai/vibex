//! Antigravity on the lane: it drifts as if it weighed nothing, bobbing high
//! and low, and turns a slow somersault at the far end.

use super::*;

pub(super) const LEGS: [Leg; 4] = [
    leg(2.2, 4.4, -0.1),
    leg(2.6, 4.0, 0.9),
    leg(2.0, 4.6, 0.2),
    leg(2.4, 4.2, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.7;
pub(super) const EXIT: f32 = 1.4;
/// The stop where it somersaults: the far end.
const SOMERSAULT: usize = 2;

pub(super) fn lane(boss: &mut Boss, moment: Moment, phase: f32) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    boss.height = 5.0 + (phase * TAU * 6.0).sin() * 2.5;
    boss.yaw = (phase * TAU * 4.0).sin() * 0.3;
    boss.bank = (phase * TAU * 5.0).sin() * 0.12;
    match moment.travelling() {
        Some(_) => from + span * cruise(moment.progress(), 0.35),
        None => {
            if moment.index == SOMERSAULT {
                boss.pitch = TAU * smoothstep((moment.elapsed - 0.3) / 1.4);
            }
            from
        }
    }
}

/// It floats down from above upside down and turns itself the right way up.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let settle = 1.0 - (1.0 - a).powi(2);
    shot.lift = -1.1 * (1.0 - settle);
    shot.boss.pitch += PI * (1.0 - smoothstep(a));
}

/// It loses its weight altogether and floats up out of the lane, turning
/// over as it goes.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    boss.pitch += PI * smoothstep(a);
    (0.0, -1.15 * a * a, 1.0)
}

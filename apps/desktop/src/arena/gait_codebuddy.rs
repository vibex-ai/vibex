//! CodeBuddy on the lane: quick little scurries, sits where it grooms, and a
//! crouch, a wiggle and a pounce.

use super::*;

/// Scurry, pounce, scurry to the end; scurry back, pounce, scurry home.
pub(super) const LEGS: [Leg; 6] = [
    leg(1.8, 2.0, -0.3),
    leg(1.6, 0.8, 0.2),
    leg(1.2, 2.2, 0.9),
    leg(1.8, 2.6, 0.1),
    leg(1.4, 0.8, -0.5),
    leg(0.8, 1.4, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.0;
pub(super) const EXIT: f32 = 1.35;
const POUNCES: [usize; 2] = [1, 4];
/// One scurrying step, and the wiggle before a pounce.
const STEP: f32 = 0.25;
const WIGGLE: f32 = 0.8;

pub(super) fn lane(boss: &mut Boss, legs: &[Leg], moment: Moment) -> f32 {
    let (from, span, heading) = (moment.from, moment.leg.to - moment.from, moment.heading());
    let pouncing = POUNCES.contains(&moment.index);
    match moment.travelling() {
        Some(elapsed) => {
            let t = moment.progress();
            boss.yaw = -heading * 0.45;
            if pouncing {
                let arc = (t * PI).sin();
                boss.height = arc * 7.0;
                boss.squash = -0.3 * arc;
                from + span * smoothstep(t)
            } else {
                let (step, w) = beat(elapsed, STEP);
                let side = if (step as usize).is_multiple_of(2) {
                    1.0
                } else {
                    -1.0
                };
                boss.height = (w * PI).sin() * 1.2;
                boss.bank = side * 0.06 * (w * PI).sin();
                from + span * cruise(t, 0.15)
            }
        }
        None => {
            let elapsed = moment.elapsed;
            let arrived = heading_of(legs, previous(legs, moment.index));
            let turn = smoothstep(elapsed / moment.leg.hold);
            boss.yaw = -(arrived + (heading - arrived) * turn) * 0.45;
            let left = moment.leg.hold - elapsed;
            if POUNCES.contains(&previous(legs, moment.index)) {
                // Landing flat from the pounce.
                boss.squash = 0.6 * (1.0 - smoothstep(elapsed / 0.3));
            } else if pouncing && left < WIGGLE {
                // The crouch and the wiggle before it springs.
                let wiggle = smoothstep(1.0 - left / WIGGLE);
                boss.bank = (elapsed * 30.0).sin() * 0.08 * wiggle;
                boss.squash = 0.4 * wiggle;
            } else {
                // A groom: its head tips from side to side.
                boss.yaw += (elapsed * 2.6).sin() * 0.2;
            }
            from
        }
    }
}

/// A leap up from below the lane's left end, on a high arc, landing light.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    shot.slide = -(1.0 - a);
    shot.lift = 0.5 * (1.0 - a) - (a * PI).sin() * 0.6;
    let boss = &mut shot.boss;
    boss.squash = if a < 0.8 {
        -0.3 * (a / 0.8 * PI).sin()
    } else {
        0.5 * ((a - 0.8) / 0.2 * PI).sin()
    };
}

/// It crouches low and slinks off past the far end.
pub(super) fn leaving(boss: &mut Boss, a: f32, side: f32) -> (f32, f32, f32) {
    let low = smoothstep(a / 0.2);
    boss.squash += (0.35 - boss.squash) * low;
    boss.height *= 1.0 - low;
    boss.pitch += (-0.1 - boss.pitch) * low;
    let p = ((a - 0.15) / 0.85).max(0.0);
    (-side * p * p, 0.0, 1.0)
}

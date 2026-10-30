//! Devin on the lane: the flower stands up to show the logo and cartwheels
//! on its petals, a third of a turn a step.

use super::*;
use crate::arena::guardian::DEVIN_STANDING;

const STEP: f32 = 0.7;
/// Six steps to the middle, a halt, six to the end, twelve back. The steps
/// out and back cancel, so the turns close over the lap.
pub(super) const LEGS: [Leg; 3] = [
    leg(2.4, 6.0 * STEP, 0.0),
    leg(2.0, 6.0 * STEP, 0.9),
    leg(2.6, 12.0 * STEP, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.35;
pub(super) const EXIT: f32 = 1.15;
const THIRD: f32 = TAU / 3.0;

fn steps(leg: Leg) -> f32 {
    (leg.travel / STEP).round()
}

pub(super) fn lane(boss: &mut Boss, legs: &[Leg], moment: Moment) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    // A wheel turns against its travel.
    let turned: f32 = (0..moment.index)
        .map(|ix| -heading_of(legs, ix) * steps(legs[ix]))
        .sum();
    boss.pitch = -PI * 0.5;
    boss.height = DEVIN_STANDING;
    let (place, rolled) = match moment.travelling() {
        Some(elapsed) => {
            let total = steps(moment.leg);
            let (step, u) = if elapsed >= moment.leg.travel {
                (total - 1.0, 1.0)
            } else {
                beat(elapsed, STEP)
            };
            let roll = smoothstep((u / 0.6).min(1.0));
            boss.height += (roll * PI).sin() * 1.2;
            (
                from + span * (step + roll) / total,
                -moment.heading() * (step + roll),
            )
        }
        None => (from, 0.0),
    };
    boss.spin = (turned + rolled) * THIRD;
    place
}

/// It cartwheels in from past the lane's right end, two turns of a third.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let rolled = a * 2.0;
    let (step, u) = if a >= 1.0 {
        (1.0, 1.0)
    } else {
        (rolled.floor(), rolled.fract())
    };
    let roll = smoothstep((u / 0.6).min(1.0));
    let done = step + roll;
    shot.slide = 1.0 - done / 2.0;
    shot.boss.spin -= (2.0 - done) * THIRD;
    shot.boss.height += (roll * PI).sin() * 1.2;
}

/// It spins up on the spot like a rotor and lifts away out of the lane.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    boss.spin += TAU * 3.0 * a * a;
    let p = ((a - 0.3) / 0.7).max(0.0);
    (0.0, -1.2 * p * p, 1.0)
}

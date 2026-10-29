//! Cursor on the lane: one face at a time, a caret stepping along a line.
//!
//! The cube stands corner-on, the logo's hexagon, and rolls along a diagonal;
//! the lane shows only how far across that carries it. Its quarter-turns
//! accumulate as one rolling pitch with the facet held on its first face,
//! which is what rolling is, so the pose never snaps between faces.
//! Twenty-eight quarter-turns a lap bring the facet home.

use super::*;
use crate::arena::cube::{self, SIDE};

/// One quarter-turn and the beat it rests on its new face.
const STEP: f32 = 0.6;
/// The share of a step spent turning over.
const ROLL: f32 = 0.7;
/// Seven faces to the middle, a pause, seven to the end, fourteen back: a
/// diagonal quarter-turn carries the cube 0.13 of a lane unit across.
pub(super) const LEGS: [Leg; 3] = [
    leg(2.2, 7.0 * STEP, 0.0),
    leg(1.6, 7.0 * STEP, 0.9),
    leg(2.4, 14.0 * STEP, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.25;
pub(super) const EXIT: f32 = 1.15;

fn steps(leg: Leg) -> f32 {
    (leg.travel / STEP).round()
}
/// How far through one quarter-turn `u` of a step is: the turn's angle, the
/// center's lift, and its forward travel as a share of a side.
fn turn(u: f32) -> (f32, f32, f32) {
    let theta = smoothstep((u / ROLL).min(1.0)) * PI * 0.5;
    let (lift, advance) = cube::tumble(theta);
    (theta, lift, advance / SIDE)
}

pub(super) fn lane(boss: &mut Boss, legs: &[Leg], moment: Moment) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    let before: f32 = (0..moment.index).map(|ix| steps(legs[ix])).sum();
    let (place, rolled) = match moment.travelling() {
        Some(elapsed) => {
            let total = steps(moment.leg);
            let (step, u) = if elapsed >= moment.leg.travel {
                (total - 1.0, 1.0)
            } else {
                beat(elapsed, STEP)
            };
            let (theta, lift, advance) = turn(u);
            boss.height = lift;
            (
                from + span * (step + advance) / total,
                step * PI * 0.5 + theta,
            )
        }
        None => (from, 0.0),
    };
    boss.pitch = wrap(-(before * PI * 0.5 + rolled));
    // It rolls down-right going right and down-left going left, and pivots a
    // quarter-turn on the spot between them; being square it never needs
    // mirroring.
    let yaw = |heading: f32| cube::yaw_toward(Vec2::new(heading, 1.0));
    boss.yaw = match moment.resting() {
        Some(elapsed) => {
            let arrived = yaw(heading_of(legs, previous(legs, moment.index)));
            let leaving = yaw(moment.heading());
            let progress = smoothstep((elapsed - 0.3) / (moment.leg.hold - 0.6));
            arrived + (leaving - arrived) * progress
        }
        None => yaw(moment.heading()),
    };
    place
}

/// Two quarter-turns in from past the lane's left end. The lane's first pose
/// faces left, so the cube rolls in backward, which a cube can do.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let travelled = a * 2.0;
    let (step, u) = if a >= 1.0 {
        (1.0, 1.0)
    } else {
        (travelled.floor(), travelled.fract())
    };
    let (theta, lift, advance) = turn(u);
    let boss = &mut shot.boss;
    boss.pitch = wrap(boss.pitch - PI + step * PI * 0.5 + theta);
    boss.height = lift;
    shot.slide = -(1.0 - (step + advance) / 2.0);
}

/// A caret blinking out: it settles onto its face, blinks once, and is gone.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    // Settle onto the nearest face: the cube is the same on every one.
    let face = (boss.pitch / (PI * 0.5)).round() * PI * 0.5;
    let settle = smoothstep(a / 0.2);
    boss.pitch += (face - boss.pitch) * settle;
    boss.height *= 1.0 - settle;
    let fade = |from: f32, to: f32, start: f32| from + (to - from) * smoothstep((a - start) / 0.25);
    let opacity = if a < 0.3 {
        fade(1.0, 0.15, 0.0)
    } else if a < 0.6 {
        fade(0.15, 1.0, 0.3)
    } else {
        fade(1.0, 0.0, 0.6)
    };
    (0.0, 0.0, opacity)
}

//! Kimi on the lane: long, even slides, each followed by a sudden charge
//! that ends in a skid rocking the tile on its edge.

use super::*;

/// A glide out, a charge to the end, a glide back, a charge home.
pub(super) const LEGS: [Leg; 4] = [
    leg(2.4, 5.0, 0.1),
    leg(3.6, 1.1, 0.9),
    leg(2.0, 5.6, -0.1),
    leg(3.8, 1.1, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.15;
pub(super) const EXIT: f32 = 1.0;
/// How long the tile rocks after a charge, and crouches before one.
const SKID: f32 = 1.2;
const CROUCH: f32 = 0.55;

fn charging(index: usize) -> bool {
    index % 2 == 1
}

pub(super) fn lane(boss: &mut Boss, legs: &[Leg], moment: Moment) -> f32 {
    let (from, span, heading) = (moment.from, moment.leg.to - moment.from, moment.heading());
    boss.yaw = -heading * 0.3;
    match moment.travelling() {
        Some(_) if charging(moment.index) => {
            let t = moment.progress();
            boss.pitch = 0.18 * (1.0 - smoothstep(t / 0.2)) - 0.15 * (t * PI).sin();
            from + span * smootherstep(t)
        }
        Some(_) => from + span * cruise(moment.progress(), 0.15),
        None => {
            let elapsed = moment.elapsed;
            let arrived = heading_of(legs, previous(legs, moment.index));
            let turn = smoothstep(elapsed / moment.leg.hold);
            boss.yaw = -(arrived + (heading - arrived) * turn) * 0.3;
            if charging(previous(legs, moment.index)) && elapsed < SKID {
                // The skid out of a charge: a slide past the stop and a rock.
                let settle = 1.0 - elapsed / SKID;
                boss.bank = (elapsed * 14.0).sin() * 0.25 * settle;
                let slide = (elapsed / 0.5 * PI).sin().max(0.0) * 0.05 * settle;
                return from + arrived * slide;
            }
            if charging(moment.index) {
                // A crouch back on its heel before the charge.
                let left = moment.leg.hold - elapsed;
                boss.pitch = 0.18 * smoothstep(1.0 - left / CROUCH);
            }
            from
        }
    }
}

/// It is drawn up out of the composer edge first, like a card, and flips
/// upright as it clears it.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    shot.lift = 1.0 - smoothstep(a / 0.45);
    let up = smoothstep(a / 0.75);
    let wobble = if a > 0.75 {
        ((a - 0.75) / 0.25 * PI).sin() * 0.12
    } else {
        0.0
    };
    shot.boss.pitch = -PI * 0.5 * (1.0 - up) + wobble;
}

/// It falls flat on its back and slides away into the composer.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    let fall = (a / 0.45).min(1.0);
    boss.pitch += (PI * 0.5 - boss.pitch) * fall * fall;
    let slide = 1.1 * smoothstep((a - 0.4) / 0.6);
    (0.0, slide, 1.0)
}

//! ZCode on the lane: the whole Z marches, each step a stamp that rocks it
//! side to side, with the bars riding on its ends.

use super::*;

const STEP: f32 = 0.9;
/// Six stamps to the middle, a halt, six to the end, twelve back.
pub(super) const LEGS: [Leg; 3] = [
    leg(2.0, 6.0 * STEP, -0.1),
    leg(1.8, 6.0 * STEP, 0.9),
    leg(2.4, 12.0 * STEP, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.45;
pub(super) const EXIT: f32 = 1.05;

pub(super) fn lane(boss: &mut Boss, moment: Moment) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    boss.yaw = -moment.heading() * 0.15;
    let (place, rise) = match moment.travelling() {
        Some(elapsed) => {
            let total = (moment.leg.travel / STEP).round();
            let (step, u) = if elapsed >= moment.leg.travel {
                (total - 1.0, 1.0)
            } else {
                beat(elapsed, STEP)
            };
            let rise = (u * PI).sin();
            let side = if (step as usize).is_multiple_of(2) {
                1.0
            } else {
                -1.0
            };
            boss.height = rise * 3.0;
            boss.bank = side * 0.1 * rise;
            boss.pitch = -0.08 * rise;
            (from + span * (step + smoothstep(u)) / total, rise)
        }
        None => (from, 0.0),
    };
    // The bars ride the Z, lifting off its ends a little at each stamp.
    boss.position = lane_point(place);
    boss.seat_bars(rise * 0.6);
    place
}

/// Assembling: the slab rises out of the composer, then the top bar drops
/// onto it and the bottom bar slides in from the side.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    shot.lift = 1.0 - smoothstep(a / 0.55);
    let gather = 1.0 - smoothstep((a - 0.35) / 0.65);
    let boss = &mut shot.boss;
    boss.hand_heights[0] += gather * 9.0;
    boss.hands[1] = boss.hands[1].plus(Vec2::new(gather * 10.0, 0.0));
}

/// Coming apart: the bars fly off and the slab sinks into the composer.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    let scatter = smoothstep(a / 0.5);
    boss.hand_heights[0] += scatter * 9.0;
    boss.hands[1] = boss.hands[1].plus(Vec2::new(scatter * 10.0, 0.0));
    let sink = 1.1 * smoothstep((a - 0.25) / 0.75);
    (0.0, sink, 1.0 - smoothstep((a - 0.7) / 0.3))
}

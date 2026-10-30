//! Gemini on the lane: long arcing glides, a quarter turn at most stops,
//! and at two of them the true twin slips out of the white one and back.

use super::*;

pub(super) const LEGS: [Leg; 6] = [
    leg(1.4, 2.0, -0.4),
    leg(1.7, 1.8, 0.2),
    leg(1.3, 2.0, 0.9),
    leg(2.0, 2.5, 0.3),
    leg(1.5, 1.9, -0.3),
    leg(1.4, 2.6, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.2;
pub(super) const EXIT: f32 = 0.85;
/// The stops where the twins part; the star turns a quarter at the rest.
/// Four quarter turns a lap: the star's colours are not four-fold.
const PARTINGS: [usize; 2] = [2, 5];

pub(super) fn lane(boss: &mut Boss, moment: Moment, phase: f32) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    boss.height = 6.0 + (phase * TAU * 8.0).sin() * 0.8;
    let quarters_before = (0..moment.index)
        .filter(|ix| !PARTINGS.contains(ix))
        .count() as f32;
    let place = match moment.travelling() {
        Some(_) => {
            let t = moment.progress();
            boss.yaw = -moment.heading() * 0.25 * (t * PI).sin();
            from + span * smootherstep(t)
        }
        None => from,
    };
    let turning = !PARTINGS.contains(&moment.index);
    // The true twin is home in the white one unless they part.
    boss.hands[0] = lane_point(place);
    boss.hand_heights[0] = boss.height;
    let turn = match moment.resting() {
        Some(elapsed) if turning => smoothstep(elapsed / 0.5),
        Some(_) => 0.0,
        None => f32::from(turning),
    };
    boss.spin = (quarters_before + turn) * PI * 0.5;
    if let Some(elapsed) = moment.resting().filter(|_| !turning) {
        let apart = ((elapsed / moment.leg.hold).clamp(0.0, 1.0) * PI).sin();
        if apart > 0.0 {
            let position = lane_point(place);
            boss.detached = true;
            boss.hands[0] = position.plus(Vec2::new(8.0 * apart, 0.0));
            boss.hand_heights[0] = boss.height + 2.0 * apart;
        }
    }
    place
}

/// A shooting star from past the lane's upper right, turning as it comes.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    let carried = 1.0 - (1.0 - a).powi(2);
    shot.slide = 1.0 - carried;
    shot.lift = -(1.0 - carried);
    shot.boss.spin -= TAU * (1.0 - carried);
}

/// The twins part, the true one drifting off, and both fade away.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    let apart = smoothstep(a / 0.6);
    boss.detached = true;
    boss.hands[0] = boss.position.plus(Vec2::new(9.0 * apart, 0.0));
    boss.hand_heights[0] = boss.height + 2.0 * apart;
    (0.0, 0.0, 1.0 - smoothstep((a - 0.3) / 0.7))
}

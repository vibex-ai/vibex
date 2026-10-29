//! Cline on the lane: a head gliding between four stations, turned to the
//! viewer, rocking back at each as if it had fired.

use super::*;

/// Out to the far end station by station, and back the same way.
pub(super) const LEGS: [Leg; 6] = [
    leg(2.4, 1.85, -0.3),
    leg(2.4, 1.85, 0.3),
    leg(2.4, 1.85, 0.9),
    leg(2.4, 1.85, 0.3),
    leg(2.4, 1.85, -0.3),
    leg(2.4, 1.85, -0.9),
];
pub(super) const ENTRANCE: f32 = 1.4;
pub(super) const EXIT: f32 = 1.2;
/// Bobs a lap: one every second and a half.
const BOBS: f32 = 17.0;

pub(super) fn lane(boss: &mut Boss, moment: Moment, phase: f32) -> f32 {
    let (from, span) = (moment.from, moment.leg.to - moment.from);
    boss.height = 3.0 + (phase * TAU * BOBS).sin() * 0.6;
    match moment.travelling() {
        Some(_) => {
            let t = moment.progress();
            boss.yaw = -moment.heading() * 0.3 * (t * PI).sin();
            from + span * smootherstep(t)
        }
        None => {
            // The recoil of a volley at each stop.
            let held = moment.elapsed;
            if held < 0.4 {
                boss.pitch = (held / 0.4 * PI).sin() * 0.18;
            }
            from
        }
    }
}

/// Powering on: it flickers into being as it settles onto its hover.
pub(super) fn entering(shot: &mut Shot, a: f32) {
    shot.lift = -0.35 * (1.0 - smoothstep(a / 0.8));
    let ramp = |from: f32, to: f32, start: f32, length: f32| {
        from + (to - from) * smoothstep((a - start) / length)
    };
    shot.opacity = if a < 0.3 {
        ramp(0.0, 0.6, 0.0, 0.25)
    } else if a < 0.5 {
        ramp(0.6, 0.25, 0.3, 0.15)
    } else {
        ramp(0.25, 1.0, 0.5, 0.3)
    };
}

/// Powering off: the head nods forward, sinks into the composer and fades.
pub(super) fn leaving(boss: &mut Boss, a: f32) -> (f32, f32, f32) {
    let droop = smoothstep(a / 0.3);
    boss.pitch += (-0.4 - boss.pitch) * droop;
    // Its hover gives out as it nods.
    boss.height += (1.0 - boss.height) * droop;
    let sink = 1.1 * smoothstep((a - 0.25) / 0.75);
    (0.0, sink, 1.0 - smoothstep((a - 0.55) / 0.45))
}

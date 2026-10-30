use super::*;
use crate::arena::guardian::ARCH_HEART;

/// The logo's gradient, by where it falls on the A: blue at the feet, green
/// up the left leg, yellow over the top, red down the right.
const GREEN: [u8; 5] = [MOSS_DARK, MOSS_DARK, MOSS, MOSS_LIGHT, MOSS_LIGHT];
const YELLOW: [u8; 5] = [GOLD_DARK, GOLD_DARK, GOLD, GOLD, IVORY];
const RED: [u8; 5] = [CORE_DARK, CORE_DARK, CORE, CORE, CORE_LIGHT];
const BLUE: [u8; 5] = [WATER_DARK, WATER_DARK, WATER, WATER_LIGHT, WATER_LIGHT];

/// The logo's arch: an A whose two legs meet in a rounded top and taper to
/// curled feet, coloured along its length as the logo's gradient. A red
/// heart hangs under the apex between the legs; it is the weak point, and
/// only the arch's opening shows it.
pub(super) fn antigravity(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let spread = fall * 2.5;
    let path = [
        (-10.4 - spread, 0.0, 0.6),
        (-8.4, 4.6, 1.3),
        (-6.7, 9.6, 2.0),
        (-4.9, 14.0, 2.4),
        (-2.6, 17.3, 2.6),
        (0.0, 18.5, 2.6),
        (2.6, 17.3, 2.6),
        (4.9, 14.0, 2.4),
        (6.7, 9.6, 2.0),
        (8.4, 4.6, 1.3),
        (10.4 + spread, 0.0, 0.6),
    ];
    let ramps = [
        BLUE, GREEN, GREEN, GREEN, YELLOW, YELLOW, RED, RED, RED, BLUE,
    ];
    for (ix, pair) in path.windows(2).enumerate() {
        let (a, b) = (pair[0], pair[1]);
        mesh.tube(
            &[p3(a.0, 0.0, a.1), p3(b.0, 0.0, b.1)],
            &[a.2, b.2],
            1.0,
            ramps[ix],
        );
    }
    // The heart, hung from the apex on a thread of light.
    let open = boss.exposed > 0.0;
    let heart = p3(0.0, 0.0, ARCH_HEART - fall * 4.0);
    mesh.beam(heart.add(p3(0.0, 0.0, 2.0)), p3(0.0, 0.0, 16.4), 0.3, RUNE);
    let r = if open { 2.8 } else { 2.3 };
    mesh.sphere(heart, p3(r, r, r), 5, 10, |_| {
        if open { LUMINOUS } else { EMBER }
    });
    // Motes orbit it, weightless.
    for n in 0..4 {
        let a = time * 1.8 + n as f32 * TAU / 4.0;
        let at = heart.add(p3(a.cos() * 4.2, a.sin() * 4.2, (a * 2.0).sin() * 1.2));
        mesh.crystal(at, p3(0.6, 0.6, 0.6), [GREEN, YELLOW, RED, BLUE][n]);
    }
}

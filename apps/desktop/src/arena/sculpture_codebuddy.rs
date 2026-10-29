use super::*;

/// The logo's mascot as a chubby cat-bot: a big white head with two pointed
/// ears and a broad purple visor for a face, two slanted pill eyes on it, a
/// small round body, a teal gem on its chest — its weak point — and a tail
/// with a teal tip.
pub(super) fn codebuddy(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    mesh.sphere(p3(0.0, 0.0, 6.0), p3(6.4, 5.4, 5.2), 6, 12, |_| CLAY);
    for side in [-1.0, 1.0] {
        // Front paws.
        mesh.sphere(p3(side * 2.9, 4.2, 1.3), p3(1.6, 1.8, 1.3), 4, 8, |_| CLAY);
    }
    mesh.sphere(p3(0.0, 0.5, 14.2), p3(8.8, 7.2, 6.8), 8, 14, |_| CLAY);
    for side in [-1.0, 1.0] {
        let droop = fall * 1.5;
        mesh.tube(
            &[
                p3(side * 5.0, 0.0, 18.2),
                p3(side * 6.0, 0.0, 21.8 - droop),
                p3(side * 6.8, 0.0, 24.6 - droop * 1.5),
            ],
            &[2.8, 1.6, 0.2],
            0.8,
            CLAY,
        );
        mesh.tube(
            &[p3(side * 5.1, 0.9, 18.8), p3(side * 5.9, 0.9, 21.6 - droop)],
            &[1.4, 0.3],
            0.5,
            ARMOR,
        );
    }
    // The visor face and its slanted eyes.
    mesh.sphere(p3(0.6, 5.0, 13.4), p3(7.0, 3.0, 5.0), 6, 12, |_| GLASS);
    let blink = if fall > 0.3 {
        0.3
    } else if (time * 0.4).fract() < 0.04 {
        0.25
    } else {
        1.0
    };
    for x in [-2.4, 2.8] {
        mesh.box_rotated(
            p3(x, 7.7, 13.2),
            p3(1.3, 0.6, 2.9 * blink),
            p3(0.0, -0.5, 0.0),
            PORCELAIN,
        );
    }
    // The gem on its chest.
    let r = if open { 1.9 } else { 1.5 };
    mesh.crystal(
        p3(0.0, 5.4, 7.2),
        p3(r, 1.0, r * 1.2),
        if open { LUMINOUS } else { EMBER },
    );
    // The tail, curling up behind, with its teal tip.
    let flick = (time * 3.0).sin() * 0.8;
    let tail = [
        p3(0.0, -4.6, 4.0),
        p3(0.0, -7.6, 5.6),
        p3(1.4 + flick * 0.5, -9.2, 9.6),
        p3(2.4 + flick, -8.6, 13.0),
    ];
    mesh.tube(&tail, &[1.2, 1.1, 0.9, 0.8], 1.0, CLAY);
    mesh.sphere(tail[3], p3(1.3, 1.3, 1.3), 4, 8, |_| EMBER);
    if open {
        // Stars round a dazed head.
        for n in 0..3 {
            let a = time * 4.0 + n as f32 * TAU / 3.0;
            let star = p3(a.cos() * 7.0, a.sin() * 4.0, 24.0);
            mesh.crystal(star, p3(0.9, 0.9, 0.9), RUNE);
        }
    }
    let squash = boss.squash_scale();
    let bulge = 1.0 + 0.18 * boss.squash;
    mesh.deform(|p| p3(p.x * bulge, p.y * bulge, p.z * squash));
}

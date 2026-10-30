use super::*;
use crate::arena::{
    devin::{PETAL, PETAL_SIZE, SEGMENT_SIZE, petal_angle, segment_radius},
    guardian::DEVIN_HUB,
};

/// The logo's three linked hexagons as a flower lying flat: hexagon petals
/// round a dark hub with a mouth in it, links curving between them, and a
/// vine of smaller hexagons running out from each petal along the floor. The
/// heart sits down in the mouth.
pub(super) fn devin(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let open = boss.exposed > 0.0;
    let gaping = boss.attack == Attack::Inhale
        && matches!(boss.state, BossState::Windup | BossState::Recovery);
    let hub = p3(0.0, 0.0, DEVIN_HUB);
    hexagon(mesh, hub, 3.4, 2.0, boss.spin, DARK);
    let mouth = if gaping || open { 2.2 } else { 1.4 };
    mesh.sphere(
        hub.add(p3(0.0, 0.0, 0.9)),
        p3(mouth, mouth, 0.6),
        4,
        10,
        |_| {
            if open {
                LUMINOUS
            } else if gaping {
                ENERGY
            } else {
                EMBER
            }
        },
    );
    let lying = boss.pitch.abs() < 0.6;
    for arm in 0..3 {
        if boss.torn & (1 << arm) != 0 {
            continue;
        }
        let a = petal_angle(boss.spin, arm);
        let out = p3(a.cos(), a.sin(), 0.0);
        let wilt = fall * 1.5;
        let petal = hub.add(out.scale(PETAL)).add(p3(0.0, 0.0, -wilt));
        hexagon(mesh, petal, PETAL_SIZE, 2.4, a, CLAY);
        // The link curving round to the next petal, as in the logo.
        let next = petal_angle(boss.spin, arm + 1);
        let between = (a + next + if next < a { TAU } else { 0.0 }) * 0.5;
        let bend = hub.add(p3(between.cos(), between.sin(), 0.0).scale(PETAL * 0.55));
        let to = hub.add(p3(next.cos(), next.sin(), 0.0).scale(PETAL));
        mesh.tube(&[petal, bend, to], &[1.1, 0.8, 1.1], 0.7, ARMOR);
        if lying {
            // The vine along the floor.
            let mut root = petal;
            for k in 0..boss.vines[arm] as usize {
                let at = p3(0.0, 0.0, 1.0).add(out.scale(segment_radius(k)));
                mesh.beam(root, at, 0.6, ARMOR);
                hexagon(mesh, at, SEGMENT_SIZE, 1.6, a + k as f32 * 0.4, CLAY);
                root = at;
            }
        }
    }
}

/// A flat hexagonal prism of `radius` about `center`, `depth` thick along z,
/// turned `turn` in its plane.
fn hexagon(mesh: &mut Mesh, center: Point3, radius: f32, depth: f32, turn: f32, ramp: [u8; 5]) {
    let corner = |ix: usize, z: f32| {
        let a = turn + ix as f32 * TAU / 6.0;
        center.add(p3(a.cos() * radius, a.sin() * radius, z))
    };
    let half = depth * 0.5;
    for ix in 0..6 {
        let next = (ix + 1) % 6;
        // The top and bottom as fans of quads from the middle.
        let top = [
            center.add(p3(0.0, 0.0, half)),
            corner(ix, half),
            corner(next, half),
            corner(next, half),
        ];
        mesh.quad(facing(top, p3(0.0, 0.0, 1.0)), ramp);
        let bottom = [
            center.add(p3(0.0, 0.0, -half)),
            corner(ix, -half),
            corner(next, -half),
            corner(next, -half),
        ];
        mesh.quad(facing(bottom, p3(0.0, 0.0, -1.0)), ramp);
        let side = [
            corner(ix, half),
            corner(next, half),
            corner(next, -half),
            corner(ix, -half),
        ];
        let a = turn + (ix as f32 + 0.5) * TAU / 6.0;
        mesh.quad(facing(side, p3(a.cos(), a.sin(), 0.0)), ramp);
    }
}

use super::*;
use crate::arena::cube::{BACK, DOWN, EAST, HALF, SIDE, UP, WEST};

/// The logo's dark cube with the pointer cut into one face: a light facet
/// that turns to a charging glow before it fires, and stays bright while it is
/// bare. The facet rides whichever face the tumbling has turned it to.
pub(super) fn cursor(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let charging = boss.attack == Attack::Laser
        && matches!(boss.state, BossState::Windup | BossState::Striking);
    let open = boss.exposed > 0.0;
    mesh.bevel_box(
        p3(0.0, 0.0, HALF),
        p3(SIDE, SIDE, SIDE),
        1.1,
        Point3::default(),
        CLAY,
    );
    // The facet is authored on the front face and turned onto its own.
    let angles = match boss.facet {
        UP => p3(PI * 0.5, 0.0, 0.0),
        DOWN => p3(-PI * 0.5, 0.0, 0.0),
        BACK => p3(PI, 0.0, 0.0),
        EAST => p3(0.0, 0.0, -PI * 0.5),
        WEST => p3(0.0, 0.0, PI * 0.5),
        _ => Point3::default(),
    };
    let pulse = charging && (time * 14.0).sin() > 0.0;
    let facet = if fall > 0.4 {
        DARK
    } else if open && !pulse {
        LUMINOUS
    } else if charging {
        ENERGY
    } else {
        PORCELAIN
    };
    mesh.group(p3(0.0, 0.0, HALF), angles, |mesh| {
        let y = HALF + 0.02;
        mesh.cuboid(p3(0.0, y, 0.0), p3(SIDE - 3.2, 0.1, SIDE - 3.2), DARK);
        // The pointer: tip at the upper left, two wings opening down and to
        // the right, and the notch between them, as the cut through the logo.
        let plate = |x: f32, z: f32| p3(x, y + 0.12, z);
        let (tip, wing_a, notch, wing_b) = (
            plate(-4.6, 4.6),
            plate(-0.6, -4.8),
            plate(-0.6, 0.6),
            plate(4.8, 0.6),
        );
        for (a, b) in [(wing_a, notch), (notch, wing_b)] {
            mesh.convex_face([tip, a, b, b], Point3::default(), Point3::default(), facet);
        }
    });
}

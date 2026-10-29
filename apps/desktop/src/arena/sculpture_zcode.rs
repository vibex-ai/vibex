use super::*;
use crate::arena::{
    guardian::ZCODE_CORE,
    zed::{BAR_DEPTH, BARS, SLAB, SLAB_DEPTH},
};

/// The Z.ai logo's long diagonal as a standing slab of dark stone, with a
/// shutter over the core in the middle of its face. The two bars of the Z are
/// separate actors ([`zbar`]).
pub(super) fn zcode(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    mesh.prism(SLAB, Point3::default(), SLAB_DEPTH, CLAY);
    let front = SLAB_DEPTH * 0.5;
    // The core, behind its shutter in the middle of the face.
    let open = boss.exposed > 0.0;
    let opening = if open {
        1.0
    } else if boss.attack == Attack::Open && boss.state == BossState::Windup {
        boss.progress()
    } else {
        0.0
    };
    let sweeping = boss.attack == Attack::Sweep
        && matches!(boss.state, BossState::Windup | BossState::Striking);
    mesh.cuboid(p3(0.0, front - 0.1, ZCODE_CORE), p3(5.8, 0.2, 5.8), DARK);
    mesh.sphere(
        p3(0.0, front - 0.6, ZCODE_CORE),
        p3(2.3, 1.2, 2.3),
        5,
        10,
        |_| {
            if open {
                LUMINOUS
            } else if sweeping {
                ENERGY
            } else {
                EMBER
            }
        },
    );
    // The shutter: two leaves that slide apart along the slant to bare it.
    let slant = Point3::new(13.26, 0.0, 24.0).normalized();
    for side in [-1.0, 1.0] {
        let slide = side * (1.7 + opening * 3.6 + fall * 2.0);
        let leaf = p3(0.0, front + 0.3, ZCODE_CORE).add(slant.scale(slide));
        mesh.cuboid(
            leaf,
            p3(6.6, 0.6, 3.2),
            if sweeping { FRAME } else { ARMOR },
        );
    }
}

/// One bar of the Z, seated on the slab or flying as a fist. A fist holding
/// a switch down glows along its face.
pub(in crate::arena) fn zbar(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    let fall = smoothstep(fallen / 1.5);
    let mut mesh = Mesh::new();
    let ramp = if boss.pinned[ix] { FRAME } else { CLAY };
    mesh.prism(BARS[ix].0, Point3::default(), BAR_DEPTH, ramp);
    if boss.pinned[ix] {
        mesh.cuboid(
            p3(0.0, BAR_DEPTH * 0.5 + 0.05, 0.0),
            p3(5.0, 0.1, 0.6),
            ENERGY,
        );
    }
    let side = if ix == 0 { -1.0 } else { 1.0 };
    mesh.draw(
        frame,
        boss.hands[ix],
        p3(0.0, side * fall * 0.6, boss.yaw),
        (boss.hand_heights[ix] - fall * 6.0).max(1.8 * (1.0 - fall)),
        Point3::default(),
    );
}

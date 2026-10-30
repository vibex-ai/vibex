use super::*;
use crate::arena::{
    guardian::ZCODE_CORE,
    zed::{BAR_DEPTH, BARS, Breath, SLAB, SLAB_DEPTH, breath},
};

/// The Z.ai logo's long diagonal as a standing slab of dark stone, its mouth
/// in the middle of its face: the core behind two stone teeth, which break
/// away one by one. The two bars of the Z are separate actors ([`zbar`]).
pub(super) fn zcode(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    mesh.prism(SLAB, Point3::default(), SLAB_DEPTH, CLAY);
    let front = SLAB_DEPTH * 0.5;
    let open = boss.exposed > 0.0;
    let rearing =
        matches!(breath(boss.shield), Breath::Rise(_)) && boss.state != BossState::Dormant;
    mesh.cuboid(p3(0.0, front - 0.1, ZCODE_CORE), p3(5.8, 0.2, 5.8), DARK);
    mesh.sphere(
        p3(0.0, front - 0.6, ZCODE_CORE),
        p3(2.3, 1.2, 2.3),
        5,
        10,
        |_| {
            if open {
                LUMINOUS
            } else if rearing {
                ENERGY
            } else {
                EMBER
            }
        },
    );
    // The teeth across the mouth, along the slant; a broken one is gone.
    let slant = Point3::new(13.26, 0.0, 24.0).normalized();
    for (ix, side) in [-1.0, 1.0].into_iter().enumerate() {
        // An open core is only ever a toothless one.
        if boss.teeth as usize <= ix || open {
            continue;
        }
        let tooth = p3(0.0, front + 0.3, ZCODE_CORE).add(slant.scale(side * (1.7 + fall * 2.0)));
        mesh.cuboid(tooth, p3(6.6, 0.6, 3.2), ARMOR);
    }
}

/// One bar of the Z: seated on the slab, or an arm risen out of the floor,
/// glowing as it fires. A bar below the floor is not drawn.
pub(in crate::arena) fn zbar(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    if boss.hand_heights[ix] < -1.0 {
        return;
    }
    let fall = smoothstep(fallen / 1.5);
    let mut mesh = Mesh::new();
    let firing = boss.guard == ix
        && boss.attack == Attack::Laser
        && matches!(boss.state, BossState::Windup | BossState::Striking);
    mesh.prism(
        BARS[ix].0,
        Point3::default(),
        BAR_DEPTH,
        if firing { FRAME } else { CLAY },
    );
    if firing {
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

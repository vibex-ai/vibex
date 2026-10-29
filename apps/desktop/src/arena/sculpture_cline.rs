use super::*;
use crate::arena::guardian::CLINE_HEAD;

/// How far the shell reaches behind the frame.
const SHELL: f32 = 7.0;
/// Eyes that flare while a fan is coming.
const HOT_GLASS: [u8; 5] = [WATER, WATER_LIGHT, WHITE, WHITE, WHITE];

/// The logo's robot head: a rounded, hollow frame with a point at the middle
/// of each side, two upright pill eyes in a dark face, and the ball on its
/// antenna. The vent on the back of the head is its weak point.
pub(super) fn cline(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let firing =
        boss.attack == Attack::Fan && matches!(boss.state, BossState::Windup | BossState::Striking);
    let h = CLINE_HEAD;
    let depth = 6.0;
    // The dark face inside the frame, and the rounded shell behind it that
    // makes the outline a head from every side.
    mesh.bevel_box(
        p3(0.0, -0.6, h),
        p3(15.4, depth - 1.0, 12.4),
        1.2,
        Point3::default(),
        DARK,
    );
    mesh.bevel_box(
        p3(0.0, -SHELL * 0.5 - 1.0, h),
        p3(15.0, SHELL, 12.0),
        2.6,
        Point3::default(),
        ARMOR,
    );
    // The frame: bars, rounded corners and the pointed sides.
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            p3(0.0, 0.0, h + side * 6.6),
            p3(11.0, depth, 2.4),
            0.8,
            Point3::default(),
            CLAY,
        );
        mesh.bevel_box(
            p3(side * 8.4, 0.0, h),
            p3(2.4, depth, 8.6),
            0.8,
            Point3::default(),
            CLAY,
        );
        for up in [-1.0, 1.0] {
            mesh.bevel_box(
                p3(side * 6.9, 0.0, h + up * 5.2),
                p3(4.6, depth, 2.4),
                0.8,
                p3(0.0, side * up * -PI * 0.25, 0.0),
                CLAY,
            );
        }
        mesh.crystal(p3(side * 9.7, 0.0, h), p3(1.6, depth * 0.48, 2.6), CLAY);
        // An upright pill eye.
        mesh.bevel_box(
            p3(side * 2.9, depth * 0.5 - 0.6, h - 0.4),
            p3(2.2, 1.0, 5.6),
            0.95,
            Point3::default(),
            if firing { HOT_GLASS } else { GLASS },
        );
    }
    // The antenna ball, as large as the logo's, resting on a short stalk and
    // nodding as the head bobs.
    let sway = (time * 2.4).sin() * 0.35 * (1.0 - fall);
    let ball = p3(sway, 0.0, h + 10.4);
    mesh.beam(p3(0.0, 0.0, h + 7.4), ball, 0.6, ARMOR);
    mesh.sphere(ball, p3(2.9, 2.9, 2.9), 6, 12, |_| CLAY);
    // The vent on the back of the shell.
    let open = boss.exposed > 0.0;
    let back = -SHELL - 1.2;
    mesh.hoop(p3(0.0, back, h), p3(3.0, 0.4, 3.0), 0.7, METAL);
    mesh.sphere(p3(0.0, back - 0.2, h), p3(2.2, 0.8, 2.2), 4, 8, |_| {
        if open { LUMINOUS } else { EMBER }
    });
}

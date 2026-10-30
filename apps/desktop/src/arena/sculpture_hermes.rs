use super::*;
use crate::arena::guardian::{HERMES_HEAD, RelicState};

/// The logo's portrait as a floating bust: long black hair cut straight
/// across the brow and flaring out at the shoulders, a white band over it
/// with a messenger's wings at its ends, a pale face with large dark eyes,
/// and the white "N" badge on her choker — her weak point. Her caduceus
/// floats at her side while she keeps it ([`caduceus`]).
pub(super) fn hermes(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    let h = HERMES_HEAD;
    let sway = (time * 1.6).sin() * 0.4 * (1.0 - fall);
    // Shoulders in a white blouse, the black choker above them.
    mesh.rounded(p3(0.0, 0.0, 3.0), p3(6.8, 3.6, 2.6), PORCELAIN);
    mesh.beam(p3(0.0, 0.0, 5.0), p3(0.0, 0.5, h - 4.0), 1.3, PORCELAIN);
    mesh.group(p3(0.0, 0.3, 6.6), p3(PI * 0.5, 0.0, 0.0), |mesh| {
        mesh.hoop(Point3::default(), p3(1.6, 0.4, 1.4), 0.5, DARK);
    });
    // The badge on the choker: a white square with an N, glowing when bare.
    let badge = p3(0.0, 1.9, 6.4);
    mesh.cuboid(
        badge,
        p3(2.0, 0.4, 2.0),
        if open { LUMINOUS } else { PORCELAIN },
    );
    if !open {
        let n = |x: f32, z: f32| badge.add(p3(x, 0.25, z));
        mesh.beam(n(-0.55, -0.6), n(-0.55, 0.6), 0.16, DARK);
        mesh.beam(n(0.55, -0.6), n(0.55, 0.6), 0.16, DARK);
        mesh.beam(n(-0.55, 0.6), n(0.55, -0.6), 0.16, DARK);
    }
    // The face.
    mesh.sphere(p3(0.0, 1.4, h - 0.4), p3(3.9, 3.5, 4.6), 7, 12, |_| {
        PORCELAIN
    });
    for side in [-1.0, 1.0] {
        let eye = p3(side * 1.55, 4.6, h - 0.2);
        mesh.sphere(eye, p3(0.95, 0.35, 1.2), 3, 8, |_| DARK);
        mesh.cuboid(
            eye.add(p3(side * -0.25, 0.3, 0.4)),
            p3(0.35, 0.1, 0.35),
            PORCELAIN,
        );
    }
    // The hair: a full black crown, the fringe cut straight across, and
    // long sides that flare out at the shoulders.
    mesh.sphere(p3(0.0, -0.8, h + 1.2), p3(5.9, 5.2, 6.0), 9, 16, |_| CLAY);
    mesh.rounded(p3(0.0, 3.4, h + 2.6), p3(4.5, 1.3, 1.7), CLAY);
    for side in [-1.0, 1.0] {
        mesh.rounded(p3(side * 4.4, 0.0, h - 3.6), p3(2.1, 3.4, 5.0), CLAY);
        mesh.rounded(
            p3(side * (6.4 + sway * 0.5), 0.4, h - 9.0 + fall * 1.5),
            p3(2.4, 2.8, 1.5),
            CLAY,
        );
    }
    mesh.rounded(p3(0.0, -3.2, h - 4.6), p3(5.0, 2.0, 5.0), CLAY);
    // The white band over the crown, a gold wing at each end.
    let band: Vec<Point3> = (0..=8)
        .map(|n| {
            let a = n as f32 / 8.0 * PI;
            p3(-a.cos() * 6.3, 0.4, h + 1.8 + a.sin() * 5.6)
        })
        .collect();
    mesh.tube(&band, &[0.55; 9], 1.0, PORCELAIN);
    for side in [-1.0, 1.0] {
        let root = p3(side * 6.3, 0.4, h + 1.8);
        for (ix, lift) in [(0, 0.0), (1, 1.4)] {
            mesh.box_rotated(
                root.add(p3(side * (1.4 + ix as f32 * 0.3), -0.6, lift)),
                p3(2.6, 0.3, 0.9),
                p3(0.0, side * (0.5 + ix as f32 * 0.3), 0.0),
                RUNE,
            );
        }
    }
    if boss.relic.state == RelicState::Held {
        // The caduceus floats at her side while she keeps it.
        mesh.group(p3(9.0, 0.0, 3.0 + sway), p3(0.0, 0.0, time * 0.8), |mesh| {
            caduceus_mesh(mesh);
        });
    }
}

/// The caduceus in flight or fallen, spinning as it goes.
pub(in crate::arena) fn caduceus(frame: &mut Raster, boss: &Boss, time: f32) {
    let mut mesh = Mesh::new();
    let flying = boss.relic.state == RelicState::Flying;
    // Thrown, it whirls flat end over end like a boomerang; dropped, it lies
    // on the floor. Either way it is laid down, centred on where it is.
    mesh.group(Point3::default(), p3(PI * 0.5, 0.0, 0.0), |mesh| {
        mesh.group(p3(0.0, 0.0, -6.3), Point3::default(), caduceus_mesh);
    });
    let turn = if flying { time * 9.0 } else { 0.4 };
    mesh.draw(
        frame,
        boss.relic.position,
        p3(0.0, 0.0, turn),
        if flying { 3.0 } else { 0.8 },
        Point3::default(),
    );
}

/// A gold staff topped with a ball and a pair of white wings, two teal
/// serpents winding up it.
fn caduceus_mesh(mesh: &mut Mesh) {
    mesh.beam(p3(0.0, 0.0, 0.0), p3(0.0, 0.0, 12.0), 0.45, METAL);
    mesh.sphere(p3(0.0, 0.0, 12.6), p3(1.0, 1.0, 1.0), 4, 8, |_| METAL);
    for side in [-1.0, 1.0] {
        mesh.box_rotated(
            p3(side * 2.0, 0.0, 11.6),
            p3(3.2, 0.3, 1.2),
            p3(0.0, side * 0.45, 0.0),
            PORCELAIN,
        );
        let coil: Vec<Point3> = (0..=10)
            .map(|n| {
                let a = n as f32 * 0.9 + if side > 0.0 { PI } else { 0.0 };
                p3(a.cos() * 1.3, a.sin() * 1.3, 1.0 + n as f32 * 0.95)
            })
            .collect();
        mesh.tube(&coil, &[0.4; 11], 1.0, GLASS);
    }
}

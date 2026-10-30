use super::*;
use crate::arena::guardian::{HERMES_HEAD, RelicState};

/// Her black hair, lit: the logo's sheen is a cold blue where the crown
/// catches the light, deep violet-black in the mass.
const HAIR_RAMP: [u8; 5] = [OUTLINE, BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT];
/// A lock in shadow, and one turned to the light.
const LOCK: [u8; 5] = [OUTLINE, OUTLINE, BODY_SHADOW, BODY_DARK, BODY];
const LOCK_LIT: [u8; 5] = [BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT, BODY_GLEAM];
/// Pale skin, a shade warmer than porcelain, and its shadow side.
const FACE: [u8; 5] = [DUST, DUST, IVORY, IVORY, WHITE];
const BLUSH: [u8; 5] = [SKIN_DARK, SKIN_DARK, SKIN_DARK, SKIN_DARK, SKIN_DARK];
/// The dark of an eye, and the iris ring around its pupil.
const EYE: [u8; 5] = [OUTLINE, OUTLINE, OUTLINE, HAIR, HAIR_LIGHT];
const IRIS: [u8; 5] = [OUTLINE, HAIR, HAIR, HAIR_LIGHT, BODY_LIGHT];
/// The gold of the circlet's wings.
const WING: [u8; 5] = [GOLD_DARK, GOLD_DARK, GOLD, IVORY, WHITE];
/// The blouse's white, and the shadow a fold leaves in it.
const BIB: [u8; 5] = [DUST, DUST, IVORY, WHITE, WHITE];
/// The dark grip bound round the shaft: brass in shadow, not bare gold.
const WOOD: [u8; 5] = [OUTLINE, GOLD_DARK, GOLD_DARK, GOLD, IVORY];
/// One flat ink, for a catchlight or a lash.
const SPARK: [u8; 5] = [WHITE; 5];
const LASH: [u8; 5] = [OUTLINE; 5];

/// The logo's portrait as a floating bust: a full crown of black hair combed
/// to a shine, the fringe cut straight across the brow, long side locks that
/// flare past the jaw and turn out at the shoulders; a white band laid over
/// the crown with a gold messenger's wing at each end; a pale face with large
/// dark eyes, a small nose and a soft mouth; a white blouse with a collar, and
/// the black choker with its white "N" badge — her weak point. Her caduceus
/// floats at her side while she keeps it ([`caduceus`]).
pub(super) fn hermes(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    let h = HERMES_HEAD;
    let sway = (time * 1.6).sin() * 0.4 * (1.0 - fall);
    let lift = (time * 1.1).sin() * 0.35 * (1.0 - fall);
    // Her floating height moves the whole bust; the head stays on `h` above it.
    let up = lift;
    // The blouse: a waist gathered in, then the shoulders.
    mesh.rounded_tapered(
        p3(0.0, 0.0, 2.6 + up),
        p3(7.2, 4.2, 3.0),
        0.76,
        1.2,
        PORCELAIN,
    );
    for side in [-1.0, 1.0] {
        mesh.rounded(p3(side * 4.7, 0.3, 3.9 + up), p3(3.2, 3.4, 2.5), PORCELAIN);
    }
    // The collar's V, and the shadow it lays on the chest.
    for side in [-1.0, 1.0] {
        mesh.taper_box(
            p3(side * 2.2, 2.6, 6.4 + up),
            (2.6, 1.0),
            (0.6, 0.5),
            2.8,
            p3(0.0, 0.0, side * 0.55),
            BIB,
        );
    }
    mesh.bevel_box(
        p3(0.0, 2.9, 6.0 + up),
        p3(3.2, 0.8, 1.3),
        0.4,
        Point3::default(),
        BIB,
    );
    // The neck, then the choker on it.
    mesh.beam(p3(0.0, 0.3, 5.6 + up), p3(0.0, 0.5, 11.0 + up), 1.4, FACE);
    mesh.group(p3(0.0, 0.6, 7.4 + up), p3(PI * 0.5, 0.0, 0.0), |mesh| {
        mesh.hoop(Point3::default(), p3(1.8, 0.45, 1.55), 0.6, DARK);
    });
    // The badge on the choker: white, with an N cut into it.
    let badge_center = p3(0.0, 2.2, 7.2 + up);
    mesh.bevel_box(
        badge_center,
        p3(2.2, 0.55, 2.2),
        0.28,
        Point3::default(),
        if open { LUMINOUS } else { PORCELAIN },
    );
    if !open {
        let n = |x: f32, z: f32| badge_center.add(p3(x, 0.34, z));
        mesh.beam(n(-0.62, -0.68), n(-0.62, 0.68), 0.18, DARK);
        mesh.beam(n(0.62, -0.68), n(0.62, 0.68), 0.18, DARK);
        mesh.beam(n(-0.62, 0.68), n(0.62, -0.68), 0.18, DARK);
    }
    // The head, set forward of the hair and high enough that the fringe can
    // sit on the brow without covering the eyes.
    let face = p3(0.0, 0.9, h - 0.9 + up);
    mesh.group(face, Point3::default(), |mesh| {
        // A shallow head, so the features can sit on the front of it.
        mesh.rounded_tapered(Point3::default(), p3(4.4, 3.0, 4.5), 1.0, 0.7, FACE);
        // The eyes, wide and large, with a tall iris and a catchlight. They
        // sit proud of the face, in the plane the light reaches.
        for side in [-1.0, 1.0] {
            let eye = p3(side * 1.7, 2.9, 0.4);
            mesh.bevel_box(eye, p3(1.8, 0.55, 2.1), 0.3, Point3::default(), EYE);
            mesh.bevel_box(
                eye.add(p3(0.0, 0.3, -0.05)),
                p3(1.3, 0.3, 1.6),
                0.26,
                Point3::default(),
                IRIS,
            );
            mesh.cuboid(
                eye.add(p3(side * 0.08, 0.45, -0.1)),
                p3(0.7, 0.2, 0.9),
                DARK,
            );
            mesh.sphere(
                eye.add(p3(side * -0.48, 0.56, 0.35)),
                p3(0.36, 0.14, 0.36),
                3,
                8,
                |_| SPARK,
            );
            // The upper lash, heaviest at the outer corner.
            mesh.beam(
                eye.add(p3(side * -0.92, 0.4, 0.7)),
                eye.add(p3(side * 0.92, 0.5, 0.62)),
                0.22,
                LASH,
            );
        }
        // A small nose, a soft closed mouth, and a blush low on the cheek.
        mesh.taper_box(
            p3(0.0, 1.8, 2.4),
            (0.85, 0.8),
            (0.4, 0.3),
            0.8,
            p3(0.0, 0.0, PI * 0.5),
            BLUSH,
        );
        mesh.beam(p3(-0.85, 0.95, 2.5), p3(0.0, 0.8, 2.6), 0.2, BLUSH);
        mesh.beam(p3(0.0, 0.8, 2.6), p3(0.85, 0.95, 2.5), 0.2, BLUSH);
        for side in [-1.0, 1.0] {
            mesh.sphere(p3(side * 3.0, 1.4, 1.6), p3(0.85, 0.22, 0.6), 3, 8, |_| {
                BLUSH
            });
        }
    });
    // The hair: a crown that caps the skull and stops at the brow, a fringe of
    // five graduated locks under it, then the long side locks.
    let crown = p3(0.0, -1.9, h + 1.2 + up);
    mesh.group(crown, Point3::default(), |mesh| {
        mesh.rounded_tapered(Point3::default(), p3(6.3, 5.3, 4.9), 0.92, 1.0, HAIR_RAMP);
        // The fringe: cut straight across the brow, raked to her left.
        for (n, (x, drop, width)) in [
            (-4.3, 0.0, 2.5),
            (-2.1, 0.4, 2.6),
            (0.2, 0.55, 2.7),
            (2.5, 0.35, 2.5),
            (4.4, 0.0, 2.2),
        ]
        .into_iter()
        .enumerate()
        {
            mesh.taper_box(
                p3(x + sway * 0.1, 4.2, 2.1 - drop * 0.4),
                (width, 2.0),
                (width * 0.9, 1.0),
                1.7 + drop * 0.6,
                p3(0.0, 0.0, -0.12 - n as f32 * 0.05),
                if n % 2 == 0 { LOCK } else { LOCK_LIT },
            );
        }
        // A lit lock and a lit streak on the crown's near cheek.
        mesh.taper_box(
            p3(-3.4, 4.4, 0.2),
            (3.2, 0.9),
            (1.2, 0.5),
            3.4,
            p3(0.0, 0.0, 0.3),
            LOCK_LIT,
        );
        mesh.taper_box(
            p3(-5.6, 2.4, -1.2),
            (2.4, 1.2),
            (0.9, 0.7),
            4.0,
            p3(0.0, 0.0, 0.12),
            LOCK_LIT,
        );
    });
    // The long locks, from the temples down past the jaw to the shoulders.
    for side in [-1.0, 1.0] {
        mesh.taper_box(
            p3(side * 5.5, 0.6, h - 3.4 + up),
            (3.2, 4.6),
            (2.4, 2.8),
            7.8,
            p3(0.0, 0.0, side * 0.09),
            LOCK,
        );
        // The flare at the shoulder, turned outward.
        mesh.taper_box(
            p3(side * (7.4 + sway * 0.4), 1.0, h - 9.2 + up + fall * 1.5),
            (2.6, 3.2),
            (1.4, 1.6),
            4.6,
            p3(0.0, 0.0, side * 0.44),
            if side < 0.0 { LOCK_LIT } else { LOCK },
        );
        mesh.group(
            p3(side * (8.7 + sway * 0.5), 1.4, h - 10.9 + up + fall * 1.8),
            p3(0.0, 0.0, side * 0.95),
            |mesh| mesh.rounded(Point3::default(), p3(1.5, 2.3, 2.6), LOCK),
        );
        // The mass behind the shoulders, so the bust has hair at its back.
        mesh.rounded(p3(side * 3.0, -3.6, h - 6.0 + up), p3(4.0, 2.4, 4.2), LOCK);
    }
    // The white band over the crown, a gold wing at each end.
    let band: Vec<Point3> = (0..=10)
        .map(|n| {
            let a = n as f32 / 10.0 * PI;
            p3(-a.cos() * 6.5, -0.6 + up, h + 1.0 + a.sin() * 5.4)
        })
        .collect();
    mesh.tube(&band, &[0.66; 11], 1.0, PORCELAIN);
    for side in [-1.0, 1.0] {
        let root = p3(side * 6.5, -0.6 + up, h + 1.0);
        // Three feathers per wing, each shorter and higher than the last.
        for (ix, (length, rise, rake)) in [
            (3.4f32, 0.0f32, 0.30f32),
            (2.7, 0.85, 0.52),
            (2.0, 1.6, 0.76),
        ]
        .into_iter()
        .enumerate()
        {
            mesh.group(
                root.add(p3(side * (0.9 + ix as f32 * 0.25), -0.4, rise)),
                p3(0.0, 0.0, side * rake),
                |mesh| {
                    mesh.taper_box(
                        Point3::default(),
                        (1.0, 1.0),
                        (0.55, 0.4),
                        length,
                        p3(0.0, 0.0, PI * 0.5),
                        if ix == 0 { WING } else { METAL },
                    );
                },
            );
        }
    }
    if boss.relic.state == RelicState::Held {
        // The caduceus floats at her side while she keeps it.
        mesh.group(
            p3(9.8, 0.0, 3.2 + up + sway),
            p3(0.0, 0.0, time * 0.8),
            |mesh| {
                caduceus_mesh(mesh);
            },
        );
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
/// serpents winding up it, their heads meeting under the ball.
fn caduceus_mesh(mesh: &mut Mesh) {
    mesh.beam(p3(0.0, 0.0, 0.0), p3(0.0, 0.0, 12.0), 0.45, METAL);
    // A grip bound at the middle of the shaft.
    mesh.bevel_box(
        p3(0.0, 0.0, 5.4),
        p3(1.1, 1.1, 1.6),
        0.25,
        Point3::default(),
        WOOD,
    );
    mesh.sphere(p3(0.0, 0.0, 12.6), p3(1.0, 1.0, 1.0), 4, 8, |_| METAL);
    for side in [-1.0, 1.0] {
        // Wings of three feathers, swept up and back from the ball.
        for (ix, (reach, rise)) in [(3.4f32, 0.0f32), (2.9, 0.9), (2.2, 1.7)]
            .into_iter()
            .enumerate()
        {
            mesh.box_rotated(
                p3(side * (1.2 + reach * 0.45), 0.0, 11.7 + rise),
                p3(reach, 0.3, 0.85),
                p3(0.0, side * (0.28 + ix as f32 * 0.14), 0.0),
                if ix == 0 { PORCELAIN } else { ARMOR },
            );
        }
        // The serpent coiling up the shaft, its head lifted to the ball.
        let coil: Vec<Point3> = (0..=12)
            .map(|n| {
                let a = n as f32 * 0.9 + if side > 0.0 { PI } else { 0.0 };
                let taper = 1.0 - n as f32 * 0.02;
                p3(
                    a.cos() * 1.35 * taper,
                    a.sin() * 1.35 * taper,
                    1.0 + n as f32 * 0.87,
                )
            })
            .collect();
        mesh.tube(&coil, &[0.42; 13], 1.0, GLASS);
        let head = coil[12];
        mesh.sphere(
            head.add(p3(-side * 0.35, 0.0, 0.5)),
            p3(0.55, 0.4, 0.7),
            3,
            8,
            |_| GLASS,
        );
        for eye in [-1.0, 1.0] {
            mesh.cuboid(
                head.add(p3(-side * 0.35 + eye * 0.28, 0.0, 0.75)),
                p3(0.16, 0.22, 0.2),
                DARK,
            );
        }
    }
}

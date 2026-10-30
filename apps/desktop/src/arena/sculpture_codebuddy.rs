use super::*;

/// The violet screen of the face, the logo's gradient: its lit faces stay
/// in the blue, and a teal glint sits low on it as the logo's glow.
const SCREEN: [u8; 5] = [WATER_DARK, WATER_DARK, WATER_DARK, WATER, WATER];

/// The logo's mascot as a chibi cat-bot. A broad, rounded head carries two
/// big pointed ears and, on its front, a rounded violet screen for a face
/// with two slanted white pill eyes; a small rounded body sits under it with
/// a teal gem at its collar — the weak point — and a thin tail curls up
/// behind.
pub(super) fn codebuddy(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    let droop = fall * 1.6;
    // The body and its paws.
    mesh.bevel_box(
        p3(0.0, 0.0, 3.6),
        p3(10.0, 8.0, 6.6),
        2.8,
        Point3::default(),
        CLAY,
    );
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            p3(side * 2.8, 3.8, 0.9),
            p3(3.0, 3.0, 1.8),
            0.8,
            Point3::default(),
            CLAY,
        );
    }
    // The head: wide and soft-cornered, as the logo's silhouette.
    let head = p3(0.0, 0.6, 13.2 - droop * 0.5);
    rounded(mesh, head, p3(9.0, 6.0, 6.2), CLAY);
    for side in [-1.0f32, 1.0] {
        // A big pointed ear, leaning out, with its lilac inside.
        let ear = |inset: f32| -> [(f32, f32); 4] {
            [
                (side * (2.6 + inset), 0.0),
                (side * (8.6 - inset * 0.6), 0.0),
                (side * (7.6 - inset * 0.2), 7.6 - inset * 1.6 - droop),
                (side * (7.6 - inset * 0.2), 7.6 - inset * 1.6 - droop),
            ]
        };
        let base = head.add(p3(0.0, 0.0, 5.0));
        mesh.prism(ear(0.0), base, 4.2, CLAY);
        mesh.prism(ear(1.6), base.add(p3(0.0, 2.2, 0.4)), 0.4, ARMOR);
    }
    // The screen, set into the front of the head.
    let screen = head.add(p3(0.4, 5.4, -0.8));
    rounded(mesh, screen, p3(7.0, 0.9, 4.4), SCREEN);
    // The eyes: white pills slanted down to the right, the right one higher.
    let blink = if fall > 0.3 {
        0.35
    } else if (time * 0.4).rem_euclid(1.0) < 0.04 {
        0.3
    } else {
        1.0
    };
    for (x, z) in [(-3.6, -0.6), (3.0, 1.2)] {
        let eye = screen.add(p3(x, 1.0, z));
        let reach = p3(0.75, 0.0, -1.35).scale(blink);
        mesh.tube(
            &[eye.sub(reach), eye.add(reach)],
            &[0.85, 0.85],
            0.6,
            PORCELAIN,
        );
    }
    // The gem at its collar.
    let r = if open { 2.2 } else { 1.7 };
    mesh.crystal(
        p3(0.0, 4.3, 6.6),
        p3(r, 0.9, r * 1.1),
        if open { LUMINOUS } else { EMBER },
    );
    // The tail curls up behind, its tip teal.
    let flick = (time * 3.0).sin() * 0.7;
    let tail = [
        p3(0.0, -3.8, 2.6),
        p3(0.0, -6.6, 3.6),
        p3(1.0 + flick * 0.5, -8.2, 7.0),
        p3(2.0 + flick, -7.4, 10.4),
    ];
    mesh.tube(&tail, &[0.9, 0.8, 0.7, 0.6], 1.0, CLAY);
    mesh.sphere(tail[3], p3(1.1, 1.1, 1.1), 4, 8, |_| EMBER);
    if open {
        // Stars round a dazed head.
        for n in 0..3 {
            let a = time * 4.0 + n as f32 * TAU / 3.0;
            let star = p3(a.cos() * 8.0, a.sin() * 4.5, 24.0);
            mesh.crystal(star, p3(0.9, 0.9, 0.9), RUNE);
        }
    }
    let squash = boss.squash_scale();
    let bulge = 1.0 + 0.18 * boss.squash;
    mesh.deform(|p| p3(p.x * bulge, p.y * bulge, p.z * squash));
}

/// A soft-cornered block: a sphere pushed out toward its bounding box, so it
/// reads as one smooth rounded shape rather than a cube or a ball.
fn rounded(mesh: &mut Mesh, center: Point3, half: Point3, ramp: [u8; 5]) {
    let first = mesh.faces.len();
    mesh.sphere(Point3::default(), p3(1.0, 1.0, 1.0), 10, 20, |_| ramp);
    let square = |v: f32| v.signum() * v.abs().powf(0.55);
    for face in &mut mesh.faces[first..] {
        for point in &mut face.points {
            *point = center.add(p3(
                square(point.x) * half.x,
                square(point.y) * half.y,
                square(point.z) * half.z,
            ));
        }
    }
}

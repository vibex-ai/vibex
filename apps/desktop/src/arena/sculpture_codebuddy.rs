use super::*;

/// The violet screen of the face, the logo's gradient: its lit faces stay
/// in the blue, and a teal glint sits low on it as the logo's glow.
const SCREEN: [u8; 5] = [WATER_DARK, WATER_DARK, WATER_DARK, WATER, WATER];
/// The screen's sunken well, one step under the glass at every light.
const WELL: [u8; 5] = [OUTLINE, OUTLINE, OUTLINE, WATER_DARK, WATER_DARK];
/// The lilac inside an ear, and the softer lilac of its root.
const LINING: [u8; 5] = [BODY_SHADOW, BODY_DARK, BODY_DARK, BODY, BODY_LIGHT];
/// A cool gleam on the glass, one tone over its lit face.
const GLINT: [u8; 5] = [WATER_LIGHT; 5];
/// Warm porcelain for the bib and the paw pads, a touch off the body white.
const BIB: [u8; 5] = [DUST, IVORY, WHITE, WHITE, WHITE];

/// The logo's mascot as a chibi cat-bot: a broad head that narrows to the
/// crown, two tall pointed ears with lilac linings, a violet visor set in a
/// pale bezel with two slanted pill eyes and a highlight across the glass, a
/// small pear of a body with a bib, four paws, and a tail curling up behind.
/// The teal gem at its collar is the weak point.
pub(super) fn codebuddy(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    let droop = fall * 1.6;
    let swell = 1.0 + 0.18 * boss.squash;
    let nod = (time * 1.7).sin() * 0.2 * (1.0 - fall);
    // Body: a pear standing on four short legs, its chest turned forward.
    mesh.rounded_tapered(p3(0.0, 0.0, 4.4), p3(6.0, 4.2, 4.0), 0.82, 1.02, CLAY);
    mesh.rounded(p3(0.0, 3.1, 5.1), p3(3.6, 1.1, 2.6), BIB);
    // Shoulder caps, so the arms read as limbs and not as more of the body.
    for side in [-1.0, 1.0] {
        mesh.rounded(p3(side * 4.8, 1.1, 5.2), p3(1.6, 1.8, 2.2), CLAY);
        // Forelegs, then the hind pair behind them.
        for (x, y, z, tall) in [(side * 3.1, 2.4, 1.9, 1.9), (side * 4.2, -1.9, 2.1, 2.1)] {
            mesh.rounded(p3(x, y, z), p3(1.35, 1.5, tall), CLAY);
            // The paw, flattened where it meets the floor.
            mesh.rounded(p3(x, y + 0.55, 0.75 - fall * 0.1), p3(1.5, 1.9, 0.85), CLAY);
            // Two toe cuts on the front of each paw.
            for toe in [-0.5, 0.5] {
                mesh.cuboid(p3(x + toe, y + 2.1, 0.5), p3(0.16, 0.5, 0.7), DARK);
            }
        }
    }
    // The collar, and the gem hung from it: the weak point.
    mesh.hoop(p3(0.0, 0.6, 7.3), p3(4.5, 0.7, 1.9), 0.7, ARMOR);
    let r = if open { 2.2 } else { 1.7 };
    mesh.crystal(
        p3(0.0, 4.6, 6.9),
        p3(r, 0.9, r * 1.1),
        if open { LUMINOUS } else { EMBER },
    );
    if !open {
        // A polished facet on the gem, so it reads as cut stone.
        mesh.crystal(p3(0.0, 5.3, 6.6), p3(0.7, 0.3, 0.8), PORCELAIN);
    }
    // The head: broad at the cheeks, gathered toward the crown.
    let head = p3(0.0, 0.4, 12.9 - droop * 0.5);
    mesh.group(head, p3(nod, 0.0, 0.0), |mesh| {
        mesh.rounded_tapered(Point3::default(), p3(8.8, 5.6, 4.8), 0.86, 1.0, CLAY);
        // Cheek tufts, one either side, breaking the egg silhouette.
        for side in [-1.0, 1.0] {
            mesh.taper_box(
                p3(side * 8.1, 1.6, -1.2),
                (1.6, 3.2),
                (0.5, 0.8),
                3.6,
                p3(0.0, 0.0, side * 0.9),
                CLAY,
            );
            // The ear: a long wedge, leaning off the crown, with its lining
            // sunk into the front of it.
            mesh.taper_box(
                p3(side * 4.7, 0.6, 5.6),
                (5.8, 3.6),
                (0.55, 0.9),
                9.2,
                p3(0.0, 0.0, side * 0.30),
                CLAY,
            );
            mesh.taper_box(
                p3(side * 4.9, 2.3, 5.4),
                (3.2, 0.6),
                (0.3, 0.2),
                6.5,
                p3(0.0, 0.0, side * 0.30),
                LINING,
            );
        }
        // The visor's bezel, then the dark well, then the glass inside it, so
        // the screen sits in the face instead of on it.
        let bezel = p3(0.0, 4.9, -0.4);
        let lopsided = if open { 0.06 } else { 0.0 };
        mesh.group(bezel, p3(0.0, lopsided, 0.0), |mesh| {
            mesh.rounded(Point3::default(), p3(7.0, 1.4, 3.9), ARMOR);
            mesh.rounded(p3(0.0, 0.7, 0.0), p3(6.0, 0.85, 3.2), WELL);
            // The glass sits proud of its well, so the visor's rim is a real
            // step of depth and not a change of colour.
            mesh.rounded(p3(0.0, 1.05, 0.0), p3(5.6, 0.9, 2.9), SCREEN);
            // A thin cool gleam along the glass's upper edge, not a shape
            // of its own: the visor must never read as a third eye.
            mesh.rounded(p3(-2.1, 2.05, 2.0), p3(1.35, 0.08, 0.18), GLINT);
            // The eyes: two white pills, canted down to the right, the far one
            // set a little higher, as in the logo.
            let blink = if fall > 0.3 {
                0.32
            } else if (time * 0.4).rem_euclid(1.0) < 0.04 {
                0.28
            } else {
                1.0
            };
            // Big pill eyes: long, slanted down to the right, the far one
            // lifted, as the logo draws them.
            for (x, z) in [(-2.4, 0.35), (2.4, 0.85)] {
                let centre = p3(x, 2.25, z);
                let half = p3(0.66, 0.0, 1.15).scale(blink);
                mesh.tube(
                    &[centre.sub(half), centre, centre.add(half)],
                    &[0.38 * blink.max(0.5), 0.7, 0.38 * blink.max(0.5)],
                    1.0,
                    PORCELAIN,
                );
            }
        });
        // A soft blush at the cheeks, under the visor.
        for side in [-1.0, 1.0] {
            mesh.sphere(p3(side * 6.2, 3.9, -2.2), p3(0.9, 0.5, 0.8), 3, 8, |_| {
                LINING
            });
        }
    });
    // The tail: a curl off the back, thickening at its root, tip teal.
    let flick = (time * 3.0).sin() * 0.7;
    let tail = [
        p3(0.0, -3.6, 3.2),
        p3(0.0, -6.4, 3.4),
        p3(0.8 + flick * 0.3, -8.2, 6.4),
        p3(1.8 + flick, -7.8, 9.6),
        p3(2.6 + flick * 1.2, -6.2, 11.2),
    ];
    mesh.tube(&tail, &[1.15, 0.95, 0.8, 0.68, 0.55], 1.0, CLAY);
    mesh.sphere(tail[4], p3(1.05, 1.05, 1.05), 4, 8, |_| EMBER);
    if open {
        // Stars round a dazed head.
        for n in 0..3 {
            let a = time * 4.0 + n as f32 * TAU / 3.0;
            let star = p3(a.cos() * 9.0, a.sin() * 5.0, 24.0 + (a * 2.0).sin());
            mesh.crystal(star, p3(0.9, 0.9, 0.9), RUNE);
        }
    }
    let squash = boss.squash_scale();
    mesh.deform(|p| p3(p.x * swell, p.y * swell, p.z * squash));
}

use super::*;
use crate::arena::guardian::KIMI_TILE;

/// The logo's K in its 100-unit box, as convex pieces: the stem, the upper
/// arm, the lower leg, and the wedge where they meet.
const K: [[(f32, f32); 4]; 4] = [
    [(22.0, 75.0), (35.0, 75.0), (35.0, 25.0), (22.0, 25.0)],
    [(35.0, 47.0), (49.0, 50.0), (76.0, 25.0), (58.0, 25.0)],
    [(35.0, 54.0), (60.0, 75.0), (78.0, 75.0), (49.0, 50.0)],
    [(35.0, 54.0), (49.0, 50.0), (35.0, 47.0), (35.0, 47.0)],
];

/// The tile's face: graphite, its lit cheek catching a cold sheen. The rim is
/// drawn darker, so the slab has a top and a side of its own.
const TILE: [u8; 5] = [OUTLINE, BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT];
/// The blue square at the corner: the logo's own blue, its glass a step up.
const BLUE: [u8; 5] = [WATER_DARK, WATER_DARK, WATER_DARK, WATER, WATER];
/// One flat ink each: the routed groove, and the catchlight on the glass.
const CUT: [u8; 5] = [OUTLINE; 5];
const SHINE: [u8; 5] = [WATER_LIGHT; 5];

/// The rounded-square outline of the app icon, `half` to a side, its corners
/// cut at the radius the logo uses. The flat sides are kept long, so the
/// bevel still reads as a chamfer on the icon's own edges.
fn outline(half: f32) -> Vec<(f32, f32)> {
    const STEPS: usize = 4;
    let radius = half * 0.46;
    let inner = half - radius;
    let mut points = Vec::with_capacity(4 * (STEPS + 1));
    for (cx, cz, start) in [
        (inner, inner, 0.0),
        (-inner, inner, PI * 0.5),
        (-inner, -inner, PI),
        (inner, -inner, PI * 1.5),
    ] {
        for step in 0..=STEPS {
            let a = start + step as f32 / STEPS as f32 * PI * 0.5;
            points.push((cx + a.cos() * radius, cz + a.sin() * radius));
        }
    }
    points
}

/// The logo's tile: a graphite app-icon slab standing on edge, its rounded
/// corners and bevelled rims given real thickness, a routed groove round the
/// margin of its face, the white K carved into the middle of it with a chamfer
/// along each arm, and the blue square raised at the upper right corner — the
/// weak point.
pub(super) fn kimi(mesh: &mut Mesh, boss: &Boss) {
    let half = KIMI_TILE * 0.5;
    let depth = 5.8;
    let front = depth * 0.5;
    mesh.slab(
        &outline(half),
        p3(0.0, 0.0, half),
        Point3::default(),
        depth,
        1.1,
        TILE,
    );
    // A smaller plate on the back, so the silhouette has a shaded rear cheek
    // rather than one flat wall.
    mesh.slab(
        &outline(half - 1.2),
        p3(0.0, -front + 0.45, half),
        Point3::default(),
        1.0,
        0.5,
        [OUTLINE, OUTLINE, BODY_SHADOW, BODY_DARK, BODY],
    );
    // The routed groove just inside the rim: the tile's own frame.
    let groove = half - 2.0;
    let run = 2.0 * groove;
    for (x, z) in [(0.0, -groove), (0.0, groove), (-groove, 0.0), (groove, 0.0)] {
        let size = if z.abs() > 0.0 {
            p3(run, 0.16, 0.26)
        } else {
            p3(0.26, 0.16, run)
        };
        mesh.bevel_box(
            p3(x, front - 0.02, half + z),
            size,
            0.07,
            Point3::default(),
            CUT,
        );
    }
    // The logo's box maps onto the tile's face, y down to z up.
    let scale = KIMI_TILE / 100.0;
    let face = |(x, y): (f32, f32)| ((x - 50.0) * scale, half - (y - 50.0) * scale);
    // The K, raised proud of the face with a chamfer of its own.
    for piece in K {
        mesh.bevel_prism(
            piece.map(face),
            p3(0.0, front - 0.1, 0.0),
            0.5,
            0.42,
            PORCELAIN,
            ARMOR,
        );
    }
    // The blue square: a raised block with its own chamfer and a catchlight.
    let open = boss.exposed > 0.0;
    let (x, z) = face((79.0, 24.0));
    let grow = if open { 0.5 } else { 0.0 };
    let size = 3.0 + grow;
    mesh.bevel_box(
        p3(x, front + 0.72, z),
        p3(size, 1.7, size),
        0.5,
        Point3::default(),
        if open { LUMINOUS } else { BLUE },
    );
    if !open {
        mesh.cuboid(
            p3(x - 0.55, front + 1.62, z + 0.55),
            p3(1.1, 0.12, 0.5),
            SHINE,
        );
    }
}

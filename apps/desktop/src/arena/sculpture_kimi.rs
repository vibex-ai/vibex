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

/// The logo's tile: a black rounded square standing on edge, the white K
/// raised on its face, and the blue square at its upper right corner, which
/// is its weak point.
pub(super) fn kimi(mesh: &mut Mesh, boss: &Boss) {
    let half = KIMI_TILE * 0.5;
    let depth = 5.0;
    mesh.bevel_box(
        p3(0.0, 0.0, half),
        p3(KIMI_TILE, depth, KIMI_TILE),
        2.4,
        Point3::default(),
        CLAY,
    );
    // The logo's box maps onto the tile's face, y down to z up.
    let scale = KIMI_TILE / 100.0;
    let face = |(x, y): (f32, f32)| ((x - 50.0) * scale, (100.0 - y) * scale);
    for piece in K {
        mesh.prism(
            piece.map(face),
            p3(0.0, depth * 0.5 + 0.2, 0.0),
            0.5,
            PORCELAIN,
        );
    }
    let open = boss.exposed > 0.0;
    let (x, z) = face((79.0, 24.0));
    let grow = if open { 0.5 } else { 0.0 };
    mesh.bevel_box(
        p3(x, depth * 0.5 + 0.6, z),
        p3(2.6 + grow, 1.6, 2.6 + grow),
        0.5,
        Point3::default(),
        if open { LUMINOUS } else { EMBER },
    );
}

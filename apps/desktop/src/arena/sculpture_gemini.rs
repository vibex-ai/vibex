use super::*;
use crate::arena::guardian::GEMINI_STAR;

const BLUE: [u8; 5] = [WATER_DARK, WATER_DARK, WATER, WATER_LIGHT, WATER_LIGHT];
const GREEN: [u8; 5] = [MOSS_DARK, MOSS_DARK, MOSS, MOSS_LIGHT, MOSS_LIGHT];
const YELLOW: [u8; 5] = [GOLD_DARK, GOLD_DARK, GOLD, GOLD, IVORY];
const PALE: [u8; 5] = [DUST, DUST, IVORY, WHITE, WHITE];
/// The true twin's half width, and the white twin's.
const TRUE: f32 = 8.5;
const WHITE_TWIN: f32 = 10.5;

/// Gemini as two stars: the pale white twin, larger, and the true twin in
/// the logo's colours in front of it — or, while it is apart, only the white
/// one here and the true twin drawn where it went ([`twin`]).
pub(super) fn gemini(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let center = p3(0.0, -1.6, GEMINI_STAR - fall * 3.0);
    star(mesh, center, WHITE_TWIN, 1.4, boss.spin, |_| PALE);
    if !boss.detached {
        true_star(mesh, p3(0.0, 1.2, GEMINI_STAR - fall * 3.0), boss);
    }
}

/// The true twin, away from the white one.
pub(in crate::arena) fn twin(frame: &mut Raster, boss: &Boss) {
    let mut mesh = Mesh::new();
    true_star(&mut mesh, p3(0.0, 0.0, GEMINI_STAR), boss);
    mesh.draw(
        frame,
        boss.hands[0],
        p3(0.0, 0.0, boss.yaw),
        boss.hand_heights[0],
        p3(0.0, 0.0, GEMINI_STAR),
    );
}

fn true_star(mesh: &mut Mesh, center: Point3, boss: &Boss) {
    // Coloured as the logo: blue above, green below left, yellow below right.
    star(mesh, center, TRUE, 2.4, boss.spin, |m: f32| {
        if m.sin() > 0.0 {
            BLUE
        } else if m.cos() < 0.0 {
            GREEN
        } else {
            YELLOW
        }
    });
    let open = boss.exposed > 0.0;
    let r = if open { 2.2 } else { 1.4 };
    mesh.sphere(center.add(p3(0.0, 1.4, 0.0)), p3(r, 0.8, r), 4, 10, |_| {
        if open { LUMINOUS } else { PALE }
    });
}

/// The logo's four-pointed star with concave sides, `radius` to a point,
/// `depth` thick, upright in the (x, z) plane and turned `spin` in it.
/// `paint` colours each sliver by its angle on the unturned star.
fn star(
    mesh: &mut Mesh,
    center: Point3,
    radius: f32,
    depth: f32,
    spin: f32,
    paint: impl Fn(f32) -> [u8; 5],
) {
    const SLIVERS: usize = 32;
    let edge = |k: usize| {
        let a = k as f32 * TAU / SLIVERS as f32;
        let (s, c) = a.sin_cos();
        // A concave four-point outline between an astroid and a diamond.
        let x = radius * c.signum() * c.abs().powf(2.8);
        let z = radius * s.signum() * s.abs().powf(2.8);
        let (ts, tc) = spin.sin_cos();
        (x * tc - z * ts, x * ts + z * tc)
    };
    let half = depth * 0.5;
    for k in 0..SLIVERS {
        let (a, b) = (edge(k), edge(k + 1));
        let ramp = paint((k as f32 + 0.5) * TAU / SLIVERS as f32);
        let at = |(x, z): (f32, f32), y: f32| center.add(p3(x, y, z));
        // The faces bulge toward the middle.
        let front = [
            center.add(p3(0.0, half + 0.6, 0.0)),
            at(a, half),
            at(b, half),
            at(b, half),
        ];
        mesh.quad(facing(front, p3(0.0, 1.0, 0.0)), ramp);
        let back = [
            center.add(p3(0.0, -half - 0.6, 0.0)),
            at(a, -half),
            at(b, -half),
            at(b, -half),
        ];
        mesh.quad(facing(back, p3(0.0, -1.0, 0.0)), ramp);
        let out = p3((a.0 + b.0) * 0.5, 0.0, (a.1 + b.1) * 0.5);
        mesh.quad(
            facing([at(a, half), at(b, half), at(b, -half), at(a, -half)], out),
            ramp,
        );
    }
}

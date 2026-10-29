use super::*;
use crate::arena::guardian::GROK_RING;

/// The ring's outer radius, its band, and half its depth.
const RADIUS: f32 = 7.6;
const BAND: f32 = 2.8;
const DEPTH: f32 = 1.5;
/// The slash's half length and half width.
const REACH: f32 = 12.5;
const EDGE: f32 = 1.3;
/// Where the slash crosses the ring, as in the logo: the ring is broken there.
const CROSSING: f32 = PI * 0.25;

/// The logo's ring, broken where the slash cuts through it from lower left
/// to upper right. The slash is a blade that spins in the ring and can be
/// hurled; a red core is set in the back of the disc.
pub(super) fn grok(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let center = p3(0.0, 0.0, GROK_RING);
    let segments = 28;
    for ix in 0..segments {
        let (a, b) = (
            ix as f32 * TAU / segments as f32,
            (ix + 1) as f32 * TAU / segments as f32,
        );
        let middle = (a + b) * 0.5;
        let gap = |cut: f32| ((middle - cut + PI).rem_euclid(TAU) - PI).abs() < 0.3;
        if gap(CROSSING) || gap(CROSSING + PI) {
            continue;
        }
        // A beaten ring sags open along its cut.
        let sag = fall * 1.5 * middle.sin().max(0.0);
        let point = |angle: f32, radius: f32, y: f32| {
            center.add(p3(angle.cos() * radius, y, angle.sin() * radius - sag))
        };
        let inner = RADIUS - BAND;
        let radial = p3(middle.cos(), 0.0, middle.sin());
        let band = |y: f32, r0: f32, r1: f32| {
            [
                point(a, r0, y),
                point(b, r0, y),
                point(b, r1, y),
                point(a, r1, y),
            ]
        };
        let rim = |r: f32| {
            [
                point(a, r, DEPTH),
                point(b, r, DEPTH),
                point(b, r, -DEPTH),
                point(a, r, -DEPTH),
            ]
        };
        for (quad, normal) in [
            (band(DEPTH, inner, RADIUS), p3(0.0, 1.0, 0.0)),
            (band(-DEPTH, inner, RADIUS), p3(0.0, -1.0, 0.0)),
            (rim(RADIUS), radial),
            (rim(inner), radial.scale(-1.0)),
        ] {
            mesh.quad(facing(quad, normal), CLAY);
        }
        // The ring's open ends are capped where the slash parts it.
        for (end, toward) in [(a, -1.0), (b, 1.0)] {
            if gap(end + toward * 0.3) {
                let cap = [
                    point(end, inner, DEPTH),
                    point(end, RADIUS, DEPTH),
                    point(end, RADIUS, -DEPTH),
                    point(end, inner, -DEPTH),
                ];
                let along = p3(-end.sin(), 0.0, end.cos()).scale(toward);
                mesh.quad(facing(cap, along), CLAY);
            }
        }
    }
    // A dark membrane fills the ring, the core set in its back.
    let membrane = RADIUS - BAND + 0.2;
    mesh.sphere(center, p3(membrane, 0.4, membrane), 4, 16, |_| DARK);
    let open = boss.exposed > 0.0;
    let r = if open { 2.4 } else { 1.9 };
    mesh.sphere(center.add(p3(0.0, -0.9, 0.0)), p3(r, 1.2, r), 5, 10, |_| {
        if open { LUMINOUS } else { EMBER }
    });
    if !boss.detached {
        slash(
            mesh,
            center.add(p3(0.0, DEPTH + 0.4, 0.0)),
            CROSSING + boss.spin,
        );
    }
}

/// The blade: a long, flat, double-edged slash lying across `angle` in the
/// ring's plane.
fn slash(mesh: &mut Mesh, center: Point3, angle: f32) {
    let (s, c) = angle.sin_cos();
    let at = |u: f32, v: f32, y: f32| center.add(p3(u * c - v * s, y, u * s + v * c));
    let t = 0.45;
    let rim = [(REACH, 0.0), (0.0, EDGE), (-REACH, 0.0), (0.0, -EDGE)];
    let face = |y: f32| rim.map(|(u, v)| at(u, v, y));
    let (front, back) = (face(t), face(-t));
    mesh.quad(facing(front, p3(0.0, 1.0, 0.0)), FRAME);
    mesh.quad(facing(back, p3(0.0, -1.0, 0.0)), FRAME);
    for ix in 0..4 {
        let next = (ix + 1) % 4;
        let out = front[ix].add(front[next]).scale(0.5).sub(center);
        let out = p3(out.x, 0.0, out.z);
        mesh.quad(
            facing([front[next], front[ix], back[ix], back[next]], out),
            ARMOR,
        );
    }
}
/// `quad` wound so the drawing faces it along `normal`.
fn facing(mut quad: [Point3; 4], normal: Point3) -> [Point3; 4] {
    let n = quad[2].sub(quad[0]).cross(quad[3].sub(quad[1]));
    if n.dot(normal) < 0.0 {
        quad.reverse();
    }
    quad
}

/// The hurled blade, drawn where it flies or lies, turning as it goes.
pub(in crate::arena) fn blade(frame: &mut Raster, boss: &Boss) {
    let mut mesh = Mesh::new();
    // Thrown, the slash lies flat and spins like a disc.
    mesh.group(Point3::default(), p3(PI * 0.5, 0.0, 0.0), |mesh| {
        slash(mesh, Point3::default(), boss.spin);
    });
    mesh.draw(
        frame,
        boss.hands[0],
        p3(0.0, 0.0, 0.0),
        boss.hand_heights[0] + 1.0,
        Point3::default(),
    );
}

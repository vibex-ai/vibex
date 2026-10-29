//! Authored spatial pixel sculptures. Geometry, articulated joints and material
//! ramps are shared by the home preview and the live encounter.

use super::{
    geometry::{Vec2, smoothstep},
    guardian::{
        Attack, Boss, BossState, Guardian, IDLE_LOOP, STEMS, Shard, spark_height, tail_lift,
    },
    raster::{DepthBuffer, Raster, ink::*},
};
use std::f32::consts::{PI, TAU};

#[derive(Clone, Copy, Debug, Default)]
struct Point3 {
    x: f32,
    y: f32,
    z: f32,
}

impl Point3 {
    fn new(x: f32, y: f32, z: f32) -> Self {
        Self { x, y, z }
    }
    fn add(self, p: Self) -> Self {
        Self::new(self.x + p.x, self.y + p.y, self.z + p.z)
    }
    fn sub(self, p: Self) -> Self {
        Self::new(self.x - p.x, self.y - p.y, self.z - p.z)
    }
    fn scale(self, s: f32) -> Self {
        Self::new(self.x * s, self.y * s, self.z * s)
    }
    fn dot(self, p: Self) -> f32 {
        self.x * p.x + self.y * p.y + self.z * p.z
    }
    fn cross(self, p: Self) -> Self {
        Self::new(
            self.y * p.z - self.z * p.y,
            self.z * p.x - self.x * p.z,
            self.x * p.y - self.y * p.x,
        )
    }
    fn length(self) -> f32 {
        self.dot(self).sqrt()
    }
    fn normalized(self) -> Self {
        self.scale(1.0 / self.length().max(0.0001))
    }
    fn rotate(self, yaw: f32, pitch: f32, roll: f32) -> Self {
        let (s, c) = pitch.sin_cos();
        let p = Self::new(self.x, self.y * c - self.z * s, self.y * s + self.z * c);
        let (s, c) = roll.sin_cos();
        let p = Self::new(p.x * c + p.z * s, p.y, -p.x * s + p.z * c);
        let (s, c) = yaw.sin_cos();
        Self::new(p.x * c - p.y * s, p.x * s + p.y * c, p.z)
    }
}

fn p3(x: f32, y: f32, z: f32) -> Point3 {
    Point3::new(x, y, z)
}

const CLAY: [u8; 5] = [BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT, BODY_GLEAM];
const ARMOR: [u8; 5] = [BODY_SHADOW, BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT];
/// Front faces keep the light body tone, like the flat logo surface.
const FRAME: [u8; 5] = [BODY_DARK, BODY, BODY_LIGHT, BODY_GLEAM, BODY_GLEAM];
/// Saturated skin whose front faces keep the exact logo tone.
const SKIN: [u8; 5] = [BODY_SHADOW, BODY_DARK, BODY, BODY, BODY_LIGHT];
/// Deep orca back: the logo blue appears only on lit faces.
const NAVY: [u8; 5] = [OUTLINE, BODY_SHADOW, BODY_SHADOW, BODY_DARK, BODY];
const METAL: [u8; 5] = [BODY_SHADOW, GOLD_DARK, GOLD, IVORY, WHITE];
const DARK: [u8; 5] = [OUTLINE, OUTLINE, SHADOW, BODY_SHADOW, BODY_DARK];
const GLASS: [u8; 5] = [WATER_DARK, WATER_DARK, WATER, WATER_LIGHT, WHITE];
const PORCELAIN: [u8; 5] = [DUST, IVORY, WHITE, WHITE, WHITE];
const ENERGY: [u8; 5] = [CORE_DARK, CORE, CORE_LIGHT, WHITE, WHITE];
const LUMINOUS: [u8; 5] = [CORE, CORE_LIGHT, WHITE, WHITE, WHITE];
const EMBER: [u8; 5] = [CORE_DARK, CORE_DARK, CORE, CORE, CORE_LIGHT];
const RUNE: [u8; 5] = [GOLD_DARK, GOLD_DARK, GOLD, GOLD, IVORY];

struct Face {
    points: [Point3; 4],
    ramp: [u8; 5],
}

struct Mesh {
    faces: Vec<Face>,
}

impl Mesh {
    fn new() -> Self {
        Self {
            faces: Vec::with_capacity(1024),
        }
    }
    fn quad(&mut self, points: [Point3; 4], ramp: [u8; 5]) {
        self.faces.push(Face { points, ramp });
    }
    fn convex_face(
        &mut self,
        mut points: [Point3; 4],
        center: Point3,
        angles: Point3,
        ramp: [u8; 5],
    ) {
        let normal = points[1].sub(points[0]).cross(points[2].sub(points[0]));
        let outward = points.iter().fold(Point3::default(), |sum, p| sum.add(*p));
        if normal.dot(outward) < 0.0 {
            if points[2].sub(points[3]).length() < 0.00001 {
                points.swap(0, 1);
            } else {
                points.reverse();
            }
        }
        self.quad(
            points.map(|p| p.rotate(angles.z, angles.x, angles.y).add(center)),
            ramp,
        );
    }
    fn cuboid(&mut self, center: Point3, size: Point3, ramp: [u8; 5]) {
        self.box_rotated(center, size, Point3::default(), ramp);
    }
    fn box_rotated(&mut self, center: Point3, size: Point3, angles: Point3, ramp: [u8; 5]) {
        let corners = [
            (-1.0, -1.0, -1.0),
            (1.0, -1.0, -1.0),
            (1.0, 1.0, -1.0),
            (-1.0, 1.0, -1.0),
            (-1.0, -1.0, 1.0),
            (1.0, -1.0, 1.0),
            (1.0, 1.0, 1.0),
            (-1.0, 1.0, 1.0),
        ]
        .map(|(x, y, z)| {
            Point3::new(x * size.x * 0.5, y * size.y * 0.5, z * size.z * 0.5)
                .rotate(angles.z, angles.x, angles.y)
                .add(center)
        });
        for face in [
            [4, 5, 6, 7],
            [2, 3, 7, 6],
            [0, 1, 5, 4],
            [1, 2, 6, 5],
            [3, 0, 4, 7],
            [0, 3, 2, 1],
        ] {
            self.quad(face.map(|ix| corners[ix]), ramp);
        }
    }
    fn bevel_box(
        &mut self,
        center: Point3,
        size: Point3,
        bevel: f32,
        angles: Point3,
        ramp: [u8; 5],
    ) {
        let half = [size.x * 0.5, size.y * 0.5, size.z * 0.5];
        let bevel = bevel.min(half.into_iter().fold(f32::INFINITY, f32::min) * 0.8);
        let inner = half.map(|value| value - bevel);
        let point = |p: [f32; 3]| Point3::new(p[0], p[1], p[2]);
        for axis in 0..3 {
            for sign in [-1.0, 1.0] {
                let points = [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)].map(|(a, b)| {
                    let mut p = [0.0; 3];
                    p[axis] = sign * half[axis];
                    p[(axis + 1) % 3] = a * inner[(axis + 1) % 3];
                    p[(axis + 2) % 3] = b * inner[(axis + 2) % 3];
                    point(p)
                });
                self.convex_face(points, center, angles, ramp);
            }
        }
        for a in 0..3 {
            for b in a + 1..3 {
                let c = 3 - a - b;
                for sa in [-1.0, 1.0] {
                    for sb in [-1.0, 1.0] {
                        let points = [(false, -1.0), (true, -1.0), (true, 1.0), (false, 1.0)].map(
                            |(edge, sc)| {
                                let mut p = [0.0; 3];
                                p[a] = sa * if edge { inner[a] } else { half[a] };
                                p[b] = sb * if edge { half[b] } else { inner[b] };
                                p[c] = sc * inner[c];
                                point(p)
                            },
                        );
                        self.convex_face(points, center, angles, ramp);
                    }
                }
            }
        }
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    let signs = [sx, sy, sz];
                    let points = [0, 1, 2, 2].map(|corner| {
                        point(std::array::from_fn(|axis| {
                            signs[axis]
                                * if axis == corner {
                                    half[axis]
                                } else {
                                    inner[axis]
                                }
                        }))
                    });
                    self.convex_face(points, center, angles, ramp);
                }
            }
        }
    }
    /// A tessellated ellipsoid whose material may vary with the unit normal.
    fn sphere(
        &mut self,
        center: Point3,
        size: Point3,
        rings: usize,
        segments: usize,
        paint: impl Fn(Point3) -> [u8; 5],
    ) {
        let unit = |lat: usize, lon: usize| {
            let a = lat as f32 / rings as f32 * PI - PI * 0.5;
            let b = lon as f32 / segments as f32 * TAU;
            Point3::new(a.cos() * b.cos(), a.cos() * b.sin(), a.sin())
        };
        for lat in 0..rings {
            for lon in 0..segments {
                let points = [
                    unit(lat, lon),
                    unit(lat, lon + 1),
                    unit(lat + 1, lon + 1),
                    unit(lat + 1, lon),
                ];
                let middle = points
                    .iter()
                    .fold(Point3::default(), |sum, p| sum.add(*p))
                    .scale(0.25);
                self.quad(
                    points
                        .map(|p| center.add(Point3::new(p.x * size.x, p.y * size.y, p.z * size.z))),
                    paint(middle),
                );
            }
        }
    }
    fn crystal(&mut self, center: Point3, size: Point3, ramp: [u8; 5]) {
        for sx in [-1.0, 1.0] {
            for sy in [-1.0, 1.0] {
                for sz in [-1.0, 1.0] {
                    self.convex_face(
                        [
                            Point3::new(sx * size.x, 0.0, 0.0),
                            Point3::new(0.0, sy * size.y, 0.0),
                            Point3::new(0.0, 0.0, sz * size.z),
                            Point3::new(0.0, 0.0, sz * size.z),
                        ],
                        center,
                        Point3::default(),
                        ramp,
                    );
                }
            }
        }
    }
    fn tube(&mut self, path: &[Point3], radii: &[f32], depth: f32, ramp: [u8; 5]) {
        let mut rings = Vec::with_capacity(path.len());
        let mut right = Point3::new(1.0, 0.0, 0.0);
        for (ix, p) in path.iter().enumerate() {
            let tangent = path[(ix + 1).min(path.len() - 1)]
                .sub(path[ix.saturating_sub(1)])
                .normalized();
            right = right.sub(tangent.scale(right.dot(tangent)));
            if right.length() < 0.01 {
                right = tangent.cross(Point3::new(0.0, 1.0, 0.0));
            }
            if right.length() < 0.01 {
                right = tangent.cross(Point3::new(0.0, 0.0, 1.0));
            }
            right = right.normalized();
            let up = tangent.cross(right);
            rings.push(
                [
                    (1.0, 0.62),
                    (0.62, 1.0),
                    (-0.62, 1.0),
                    (-1.0, 0.62),
                    (-1.0, -0.62),
                    (-0.62, -1.0),
                    (0.62, -1.0),
                    (1.0, -0.62),
                ]
                .map(|(a, b)| {
                    p.add(right.scale(a * radii[ix]))
                        .add(up.scale(b * radii[ix] * depth))
                }),
            );
        }
        for pair in rings.windows(2) {
            for ix in 0..8 {
                let next = (ix + 1) % 8;
                self.quad(
                    [pair[0][ix], pair[0][next], pair[1][next], pair[1][ix]],
                    ramp,
                );
            }
        }
        for ix in 0..8 {
            let next = (ix + 1) % 8;
            self.quad([path[0], rings[0][next], rings[0][ix], rings[0][ix]], ramp);
            let end = path.len() - 1;
            self.quad(
                [
                    path[end],
                    rings[end][ix],
                    rings[end][next],
                    rings[end][next],
                ],
                ramp,
            );
        }
    }
    fn beam(&mut self, a: Point3, b: Point3, radius: f32, ramp: [u8; 5]) {
        self.tube(&[a, b], &[radius, radius], 1.0, ramp);
    }
    fn hoop(&mut self, center: Point3, size: Point3, border: f32, ramp: [u8; 5]) {
        let point = |ix: usize, inner: bool, front: bool| {
            let a = ix as f32 * TAU / 12.0;
            center.add(Point3::new(
                a.cos() * (size.x - if inner { border } else { 0.0 }),
                if front { size.y } else { -size.y },
                a.sin() * (size.z - if inner { border } else { 0.0 }),
            ))
        };
        for ix in 0..12 {
            let next = ix + 1;
            let (a, b, c, d) = (
                point(ix, false, true),
                point(next, false, true),
                point(next, true, true),
                point(ix, true, true),
            );
            let (e, f, g, h) = (
                point(ix, false, false),
                point(next, false, false),
                point(next, true, false),
                point(ix, true, false),
            );
            self.quad([a, d, c, b], ramp);
            self.quad([e, f, g, h], ramp);
            self.quad([a, b, f, e], ramp);
            self.quad([d, h, g, c], ramp);
        }
    }
    fn group(&mut self, origin: Point3, angles: Point3, build: impl FnOnce(&mut Self)) {
        let first = self.faces.len();
        build(self);
        for face in &mut self.faces[first..] {
            for point in &mut face.points {
                *point = point.rotate(angles.z, angles.x, angles.y).add(origin);
            }
        }
    }
    /// Reshapes every face, e.g. a landing squash about the ground contact.
    fn deform(&mut self, change: impl Fn(Point3) -> Point3) {
        for face in &mut self.faces {
            for point in &mut face.points {
                *point = change(*point);
            }
        }
    }
    fn draw(self, frame: &mut Raster, anchor: Vec2, angles: Point3, height: f32, pivot: Point3) {
        let mut faces = Vec::with_capacity(self.faces.len());
        let (mut left, mut top, mut right, mut bottom) = (
            f32::INFINITY,
            f32::INFINITY,
            f32::NEG_INFINITY,
            f32::NEG_INFINITY,
        );
        for face in self.faces {
            // Articulation pivots around the visible core, whose aim-plane
            // coordinate remains the collision owner's authoritative point.
            let points = face.points.map(|p| {
                p.sub(pivot)
                    .rotate(0.0, angles.x, angles.y)
                    .add(pivot)
                    .rotate(angles.z, 0.0, 0.0)
                    .add(Point3::new(0.0, 0.0, height))
            });
            // Diagonals keep the winding of quads that collapse to triangles.
            let normal = points[2].sub(points[0]).cross(points[3].sub(points[1]));
            if normal.y + normal.z * 0.62 <= 0.00001 {
                continue;
            }
            let light = normal.dot(Point3::new(-0.50, 0.30, 0.81)) / normal.length();
            let shade = [-0.45, -0.12, 0.40, 0.72]
                .into_iter()
                .filter(|threshold| light > *threshold)
                .count();
            let points = points.map(|p| {
                let x = (anchor.x + p.x) * 4.0;
                let y = (anchor.y + p.y * 0.62 - p.z) * 4.0;
                left = left.min(x);
                right = right.max(x);
                top = top.min(y);
                bottom = bottom.max(y);
                (x, y, p.y + p.z * 0.62)
            });
            faces.push((points, face.ramp[shade]));
        }
        if faces.is_empty() {
            return;
        }
        let mut depth = DepthBuffer::new(
            left.floor() as i32,
            top.floor() as i32,
            right.ceil() as i32 + 1,
            bottom.ceil() as i32 + 1,
        );
        for (points, color) in faces {
            frame.polygon_depth(&points, color, &mut depth);
        }
        frame.outline(&depth, OUTLINE);
    }
}

pub(super) fn draw(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let time = if reduced {
        0.0
    } else {
        time.rem_euclid(IDLE_LOOP)
    };
    let fall = smoothstep(fallen / 1.8);
    let mut mesh = Mesh::new();
    match boss.guardian {
        Guardian::Claude => spark(
            &mut mesh,
            boss.scale(),
            boss.squash,
            boss.spin,
            (fall <= 0.0).then_some(boss.generation >= 4),
            time,
            fall,
        ),
        Guardian::Codex => knot(&mut mesh, boss, time, fall),
        Guardian::Pi => glyph(&mut mesh, boss, time, fall),
        Guardian::OpenCode => chest(&mut mesh, boss, fall),
        Guardian::DeepSeek => whale(&mut mesh, boss, time, fall),
        Guardian::Copilot => copilot(&mut mesh, boss, time, fall),
        Guardian::Cursor => cursor::cursor(&mut mesh, boss, time, fall),
        Guardian::Cline => cline::cline(&mut mesh, boss, time, fall),
        Guardian::Grok => grok::grok(&mut mesh, boss, fall),
    }
    let (x, y, z) = boss.pivot();
    let (pitch, bank) = boss.tilt();
    let (pitch, bank, height, yaw) = match boss.guardian {
        // The flat spark keeps its face toward the viewer.
        Guardian::Claude => (
            pitch,
            bank,
            boss.height - fall * 4.0,
            boss.yaw.clamp(-0.45, 0.45),
        ),
        Guardian::DeepSeek => (
            pitch - fall * 0.4,
            bank,
            boss.height - fall * 15.0,
            boss.yaw,
        ),
        Guardian::Copilot => (
            pitch + fall * 0.45,
            bank + fall * 0.25,
            boss.height * (1.0 - fall),
            boss.yaw,
        ),
        Guardian::OpenCode => (pitch, bank + fall * 0.18, boss.height, boss.yaw),
        // A beaten cube settles askew on one edge.
        Guardian::Cursor => (
            pitch,
            bank + fall * 0.35,
            boss.height - fall * 2.0,
            boss.yaw,
        ),
        // A beaten head powers down and drops, nodding forward.
        Guardian::Cline => (
            pitch - fall * 0.45,
            bank,
            boss.height - fall * 3.0,
            boss.yaw,
        ),
        // A beaten ring sinks and tips back.
        Guardian::Grok => (pitch + fall * 0.5, bank, boss.height - fall * 3.0, boss.yaw),
        _ => (pitch, bank, boss.height, boss.yaw),
    };
    mesh.draw(
        frame,
        boss.position,
        Point3::new(pitch, bank, yaw),
        height,
        Point3::new(x, y, z),
    );
}

/// Twelve uneven, club-ended rays around a nucleus, after the Claude spark:
/// angular jitter and relative length.
const RAYS: [(f32, f32); 12] = [
    (0.00, 1.00),
    (0.04, 0.74),
    (-0.03, 0.92),
    (0.05, 0.68),
    (-0.03, 1.00),
    (0.02, 0.80),
    (0.00, 0.95),
    (-0.04, 0.70),
    (0.03, 0.98),
    (-0.02, 0.78),
    (0.04, 0.90),
    (-0.03, 0.72),
];

/// `heart` is `Some(bare)` for the carrier; split sparks have none.
fn spark(
    mesh: &mut Mesh,
    size: f32,
    squash: f32,
    spin: f32,
    heart: Option<bool>,
    time: f32,
    fall: f32,
) {
    let center = p3(0.0, 0.0, spark_height(size, squash));
    let (wide, tall) = (1.0 + 0.2 * squash, 1.0 - 0.25 * squash);
    let reach = 8.4 * size * (1.0 - fall * 0.65);
    for (ix, (jitter, length)) in RAYS.into_iter().enumerate() {
        let angle = ix as f32 * TAU / 12.0 + jitter + spin;
        let direction = p3(angle.cos() * wide, 0.0, angle.sin() * tall);
        let depth = if ix % 2 == 0 { 0.45 } else { -0.45 } * size;
        let at = |r: f32| center.add(direction.scale(r)).add(p3(0.0, depth, 0.0));
        let l = reach * length;
        let w = size * (0.75 + 0.25 * length);
        mesh.tube(
            &[
                at(0.9 * size),
                at(l * 0.35),
                at(l * 0.68),
                at(l * 0.9),
                at(l),
            ],
            &[0.35 * w, 0.7 * w, 0.85 * w, 0.65 * w, 0.15 * w],
            0.6,
            CLAY,
        );
    }
    mesh.sphere(
        center,
        p3(2.5 * size * wide, 1.8 * size, 2.5 * size * tall),
        6,
        10,
        |_| ARMOR,
    );
    if let Some(bare) = heart {
        let beat = 1.0 + (time * TAU * 1.4).sin().max(0.0) * 0.18;
        let r = if bare { 1.7 } else { (1.3 * size).max(0.8) } * beat;
        mesh.sphere(
            center.add(p3(0.0, 1.6 * size + 0.2, 0.0)),
            p3(r, r * 0.75, r * 1.05),
            4,
            8,
            |_| if bare { LUMINOUS } else { ENERGY },
        );
    }
}

pub(super) fn shard(frame: &mut Raster, shard: &Shard, time: f32) {
    let mut mesh = Mesh::new();
    spark(&mut mesh, shard.size, 0.0, shard.spin, None, time, 0.0);
    mesh.draw(
        frame,
        shard.position,
        Point3::default(),
        shard.height,
        p3(0.0, 0.0, spark_height(shard.size, 0.0)),
    );
}

fn knot(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.unravel;
    let split = open * 6.5 + fall * 8.0;
    let orbit = open * time * 0.9;
    for strand in 0..6 {
        let angle = strand as f32 * TAU / 6.0 + orbit;
        let origin = p3(angle.cos() * split, 0.0, 11.0 + angle.sin() * split);
        mesh.group(origin, p3(open * 0.35, -angle, 0.0), |mesh| {
            let corners = [
                p3(1.7, -2.1, 2.95),
                p3(5.3, -2.1, 5.0),
                p3(9.0, -0.6, 2.85),
                p3(9.0, 2.1, -2.85),
                p3(5.3, 2.1, -5.0),
                p3(1.7, 2.1, -2.95),
            ];
            let mut path = Vec::with_capacity(10);
            path.push(corners[0]);
            for ix in 1..corners.len() - 1 {
                path.push(
                    corners[ix].sub(corners[ix].sub(corners[ix - 1]).normalized().scale(0.65)),
                );
                path.push(
                    corners[ix].add(corners[ix + 1].sub(corners[ix]).normalized().scale(0.65)),
                );
            }
            path.push(corners[corners.len() - 1]);
            mesh.tube(&path, &[1.24; 10], 1.15, CLAY);
            for (x, y, z, tall) in [
                (7.7, 3.45, -3.58, 0.8),
                (8.95, 3.45, -1.4, 1.6),
                (4.3, 3.43, -4.45, 0.65),
            ] {
                mesh.cuboid(p3(x, y, z), p3(0.22, 0.13, tall), METAL);
                mesh.cuboid(p3(x + 0.37, y, z - tall * 0.3), p3(0.6, 0.13, 0.16), METAL);
            }
        });
    }
    let core = p3(0.0, 0.0, 11.0);
    let bare = boss.exposed > 0.0;
    let shielded = boss.cube_shielded();
    let size = if bare { 3.3 } else { 2.6 };
    let turn = if bare { time * 1.7 } else { 0.0 };
    mesh.bevel_box(
        core,
        p3(size, size, size),
        0.22,
        p3(turn * 0.6, 0.0, PI * 0.25 + turn),
        if shielded { ENERGY } else { LUMINOUS },
    );
    if shielded {
        // A wire cage marks the closed beat of the shield.
        let h = 2.9;
        for axis in 0..3 {
            for (a, b) in [(-1.0, -1.0), (-1.0, 1.0), (1.0, -1.0), (1.0, 1.0)] {
                let corner = |end: f32| {
                    let mut p = [0.0; 3];
                    p[axis] = end * h;
                    p[(axis + 1) % 3] = a * h;
                    p[(axis + 2) % 3] = b * h;
                    core.add(p3(p[0], p[1], p[2]))
                };
                mesh.beam(corner(-1.0), corner(1.0), 0.2, ENERGY);
            }
        }
    }
    let squash = boss.squash_scale();
    let bulge = 1.0 + 0.15 * boss.squash;
    mesh.deform(|p| p3(p.x * bulge, p.y, p.z * squash));
}

fn glyph(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    // The block letter at 1.6 logo units: the slab, the column right of the
    // aperture and the step beneath it. Both stems are separate actors.
    for (ix, (x0, x1, z0, z1)) in [
        (-11.2, 6.03, 16.8, 22.4),
        (0.29, 6.03, 11.2, 16.8),
        (-5.46, 0.29, 5.6, 11.2),
    ]
    .into_iter()
    .enumerate()
    {
        let spread = ix as f32 - 1.0;
        let center = p3(
            (x0 + x1) * 0.5 + fall * spread * 2.5,
            0.0,
            (z0 + z1) * 0.5 - fall * (4.0 + ix as f32 * 3.0),
        );
        let size = p3(x1 - x0, 6.0, z1 - z0);
        mesh.group(
            center,
            p3(fall * 0.15 * (ix as f32 + 1.0), -fall * 0.2 * spread, 0.0),
            |mesh| {
                mesh.bevel_box(Point3::default(), size, 0.4, Point3::default(), CLAY);
                // Masonry joints on the carved front.
                let joints = (size.x / 5.6).round().max(1.0) as usize;
                for n in 1..joints {
                    let x = -size.x * 0.5 + size.x * n as f32 / joints as f32;
                    mesh.cuboid(p3(x, 3.02, 0.0), p3(0.14, 0.1, size.z - 0.9), DARK);
                }
                if ix == 0 {
                    mesh.cuboid(p3(-0.4, 3.04, 0.6), p3(size.x - 2.6, 0.1, 0.32), RUNE);
                }
            },
        );
    }
    // The aperture holds the eye.
    let open = boss.exposed > 0.0;
    mesh.cuboid(p3(-2.59, -2.2, 14.0), p3(5.6, 0.3, 5.5), DARK);
    mesh.cuboid(p3(-2.59, 0.0, 11.28), p3(5.7, 6.0, 0.12), DARK);
    let r = if open { 2.4 } else { 2.0 };
    mesh.sphere(p3(-2.6, 0.5, 14.0), p3(r, r * 0.8, r), 5, 10, |_| {
        if open { LUMINOUS } else { ENERGY }
    });
    mesh.sphere(
        p3(-2.6, 0.5 + r * 0.8, 14.0),
        p3(0.8, 0.35, 0.95),
        3,
        8,
        |_| DARK,
    );
    for ix in 0..4 {
        let a = time * TAU / 9.0 + ix as f32 * TAU / 4.0;
        let center = p3(
            a.cos() * (14.5 + fall * 4.0),
            a.sin() * 4.0,
            20.0 + (time * 1.3 + ix as f32).sin() * 1.2 - fall * 12.0,
        );
        mesh.bevel_box(center, p3(1.6, 1.6, 1.9), 0.25, p3(a * 0.5, 0.3, a), CLAY);
    }
}

pub(super) fn stem(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    let fall = smoothstep(fallen / 1.5);
    let (width, length) = STEMS[ix];
    let lift = boss.hand_heights[ix] * (1.0 - fall);
    let mut mesh = Mesh::new();
    mesh.bevel_box(
        p3(0.0, 0.0, length * 0.5),
        p3(width, 6.0, length),
        0.4,
        Point3::default(),
        CLAY,
    );
    let joints = (length / 5.6).round() as usize;
    for n in 1..joints {
        let z = length * n as f32 / joints as f32;
        mesh.cuboid(p3(0.0, 3.02, z), p3(width - 0.9, 0.1, 0.14), DARK);
    }
    // A carved rune that kindles while the stem is raised to strike.
    let glow = if lift > 3.0 { ENERGY } else { RUNE };
    mesh.cuboid(p3(0.0, 3.05, length * 0.25), p3(0.35, 0.1, 2.2), glow);
    mesh.cuboid(p3(0.0, 3.05, length * 0.25), p3(1.6, 0.1, 0.35), glow);
    let side = if ix == 0 { -1.0 } else { 1.0 };
    mesh.draw(
        frame,
        boss.hands[ix],
        p3(0.0, side * fall * 0.5, boss.yaw),
        lift - fall * 3.0,
        p3(0.0, 0.0, length * 0.5),
    );
}

fn chest(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let open = boss.exposed > 0.0;
    let lid = (boss.lid + fall * 0.5).clamp(0.0, 1.4);
    // Lower jaw: the logo's light frame around its filled lower window.
    mesh.bevel_box(
        p3(0.0, 0.0, 4.0),
        p3(12.0, 9.0, 8.0),
        0.6,
        Point3::default(),
        FRAME,
    );
    mesh.bevel_box(
        p3(0.0, 4.4, 5.45),
        p3(6.0, 0.5, 4.9),
        0.15,
        Point3::default(),
        ARMOR,
    );
    mesh.cuboid(p3(0.0, -0.2, 8.04), p3(10.8, 8.0, 0.1), DARK);
    for n in 0..4 {
        mesh.crystal(
            p3(-2.7 + n as f32 * 1.8, 3.6, 8.1),
            p3(0.45, 0.4, 0.9),
            PORCELAIN,
        );
    }
    for side in [-1.0, 1.0] {
        for z in [2.0, 6.0] {
            mesh.cuboid(p3(side * 6.05, 2.5, z), p3(0.2, 0.8, 0.8), DARK);
        }
    }
    // The gem is both the eye glinting through the slot and the heart.
    let gem = if open { 1.6 } else { 1.15 };
    mesh.sphere(p3(0.0, 1.5, 9.0), p3(gem, gem * 0.8, gem), 4, 8, |_| {
        if open { LUMINOUS } else { EMBER }
    });
    // Upper jaw hinged on the back edge; its window slot is the mouth.
    mesh.group(p3(0.0, -4.5, 8.0), p3(lid * 1.15, 0.0, 0.0), |mesh| {
        for side in [-1.0, 1.0] {
            mesh.bevel_box(
                p3(side * 4.5, 4.5, 3.0),
                p3(3.0, 9.0, 6.0),
                0.5,
                Point3::default(),
                FRAME,
            );
        }
        mesh.bevel_box(
            p3(0.0, 4.5, 4.5),
            p3(6.2, 9.0, 3.0),
            0.4,
            Point3::default(),
            FRAME,
        );
        mesh.cuboid(p3(0.0, 2.0, 1.5), p3(6.2, 4.0, 3.0), DARK);
        mesh.cuboid(p3(0.0, 4.5, -0.05), p3(11.6, 8.8, 0.1), DARK);
        for n in 0..3 {
            mesh.crystal(
                p3(-1.8 + n as f32 * 1.8, 8.2, -0.3),
                p3(0.45, 0.4, 0.8),
                PORCELAIN,
            );
        }
        mesh.beam(p3(-5.5, 0.2, 0.2), p3(5.5, 0.2, 0.2), 0.5, METAL);
    });
    let squash = boss.squash_scale();
    let bulge = 1.0 + 0.2 * boss.squash;
    mesh.deform(|p| p3(p.x * bulge, p.y, p.z * squash));
}

/// Body stations from nose to tail stock: (y, center z, half width, half height).
const ORCA: [(f32, f32, f32, f32); 11] = [
    (13.2, 7.0, 0.5, 0.5),
    (12.2, 7.1, 2.6, 2.4),
    (10.0, 7.4, 4.6, 4.2),
    (7.0, 7.6, 6.0, 5.4),
    (3.0, 7.8, 6.6, 6.0),
    (-1.0, 7.9, 6.2, 5.6),
    (-5.0, 8.2, 4.9, 4.5),
    (-8.0, 8.6, 3.6, 3.4),
    (-10.6, 9.2, 2.6, 2.6),
    (-12.8, 9.8, 1.8, 1.9),
    (-14.6, 10.4, 1.3, 1.3),
];

/// Orca markings on a deep logo-blue body: white chin, belly and flank
/// sweep, and a lighter saddle behind the dorsal fin.
fn orca_skin(y: f32, up: f32) -> [u8; 5] {
    let belly = up < -0.3 && y > -10.0;
    let flank = (-9.0..-3.5).contains(&y) && up < 0.05;
    let chin = y > 9.0 && up < 0.0;
    if belly || flank || chin {
        PORCELAIN
    } else if (-7.5..-3.5).contains(&y) && up > 0.75 {
        SKIN
    } else {
        NAVY
    }
}

/// A thin fin triangle with real thickness, so both faces render.
fn fin(mesh: &mut Mesh, front: Point3, back: Point3, tip: Point3, thickness: f32, ramp: [u8; 5]) {
    let center = front.add(back).add(tip).scale(1.0 / 3.0);
    let side = |p: Point3, x: f32| p.add(p3(x, 0.0, 0.0)).sub(center);
    let (l, r) = (-thickness, thickness);
    for points in [
        [side(front, l), side(back, l), side(tip, l), side(tip, l)],
        [side(front, r), side(back, r), side(tip, r), side(tip, r)],
        [side(front, l), side(front, r), side(tip, r), side(tip, l)],
        [side(back, l), side(back, r), side(tip, r), side(tip, l)],
    ] {
        mesh.convex_face(points, center, Point3::default(), ramp);
    }
}

fn whale(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let curl = boss.unravel;
    // Vertical fluke beats while swimming; a raised tail holds still.
    let beat = (time * TAU / 1.2).sin() * (1.0 - curl) * if boss.exposed > 0.0 { 0.0 } else { 1.0 };
    let lift = |y: f32| tail_lift(curl, y) + beat * ((-2.0 - y) / 12.5).max(0.0).powi(2) * 1.6;
    let segments = 20;
    let ring = |(y, z, w, h): (f32, f32, f32, f32)| -> Vec<Point3> {
        (0..=segments)
            .map(|ix| {
                let a = ix as f32 * TAU / segments as f32;
                p3(a.cos() * w, y, z + lift(y) + a.sin() * h)
            })
            .collect()
    };
    let rings: Vec<Vec<Point3>> = ORCA.iter().map(|station| ring(*station)).collect();
    for (ix, pair) in rings.windows(2).enumerate() {
        let (y0, z0, ..) = ORCA[ix];
        let (y1, z1, ..) = ORCA[ix + 1];
        let axis = p3(
            0.0,
            (y0 + y1) * 0.5,
            (z0 + z1) * 0.5 + lift((y0 + y1) * 0.5),
        );
        for j in 0..segments {
            let mut points = [pair[0][j], pair[0][j + 1], pair[1][j + 1], pair[1][j]];
            let middle = points
                .iter()
                .fold(Point3::default(), |sum, p| sum.add(*p))
                .scale(0.25);
            let normal = points[2].sub(points[0]).cross(points[3].sub(points[1]));
            if normal.dot(middle.sub(axis)) < 0.0 {
                points.reverse();
            }
            let a = (j as f32 + 0.5) * TAU / segments as f32;
            mesh.quad(points, orca_skin(middle.y, a.sin()));
        }
    }
    // Rounded nose cap.
    let nose = p3(0.0, 13.6, 7.0 + lift(13.6));
    for j in 0..segments {
        mesh.quad([nose, rings[0][j + 1], rings[0][j], rings[0][j]], NAVY);
    }
    // The tall dorsal fin, raked back.
    fin(
        mesh,
        p3(0.0, 2.2, 13.2),
        p3(0.0, -4.6, 13.4 + lift(-4.6)),
        p3(0.0, -3.6, 21.2 - fall * 4.0),
        0.35,
        NAVY,
    );
    for side in [-1.0, 1.0] {
        // Paddle flippers.
        mesh.tube(
            &[
                p3(side * 5.2, 6.0, 4.6),
                p3(side * 7.6, 4.8, 3.2 + beat * 0.3 * side),
                p3(side * 9.6, 3.0, 2.4 + beat * 0.4 * side),
                p3(side * 10.6, 1.8, 2.2 - fall),
            ],
            &[1.4, 1.9, 1.5, 0.3],
            0.35,
            NAVY,
        );
        // Oval white patch behind the eye.
        mesh.group(
            p3(side * 5.75, 5.6, 9.0),
            p3(0.0, 0.0, side * 0.12),
            |mesh| {
                mesh.sphere(Point3::default(), p3(0.45, 2.1, 0.95), 4, 10, |_| PORCELAIN);
            },
        );
        // Small dark eye just ahead of the patch, and the mouth line.
        mesh.sphere(
            p3(side * 5.25, 8.5, 8.0),
            p3(0.62, 0.62, 0.62),
            3,
            6,
            |_| DARK,
        );
        mesh.tube(
            &[
                p3(side * 1.6, 12.7, 6.4),
                p3(side * 3.6, 11.0, 5.9),
                p3(side * 5.0, 8.8, 6.3),
            ],
            &[0.16, 0.18, 0.12],
            1.0,
            DARK,
        );
        // Horizontal flukes swept back from the tail stock.
        let root = p3(0.0, -14.6, 10.4 + lift(-14.6));
        let flex = lift(-17.0) - lift(-14.6);
        mesh.tube(
            &[
                root,
                p3(side * 2.6, -15.6, root.z + flex * 0.4),
                p3(side * 5.2, -17.0, root.z + flex * 0.8 + 0.5),
                p3(side * 6.4, -18.2, root.z + flex + 0.9),
            ],
            &[1.3, 1.8, 1.2, 0.15],
            0.35,
            NAVY,
        );
    }
    let open = boss.exposed > 0.0;
    let r = if open { 1.9 } else { 1.3 };
    mesh.sphere(
        p3(0.0, -15.2, 11.4 + tail_lift(curl, -15.2)),
        p3(r, r, r),
        4,
        8,
        |_| if open { LUMINOUS } else { EMBER },
    );
}

fn copilot(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = boss.exposed > 0.0;
    let throwing = boss.attack == Attack::Volley
        && matches!(boss.state, BossState::Windup | BossState::Striking);
    mesh.sphere(p3(0.0, 0.0, 11.0), p3(9.4, 7.6, 7.8), 8, 14, |_| CLAY);
    // Face plate with the two vertical eye slots.
    mesh.bevel_box(
        p3(0.0, 5.6, 8.6),
        p3(12.4, 3.6, 7.2),
        1.6,
        Point3::default(),
        DARK,
    );
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            p3(side * 2.4, 7.35, 8.6),
            p3(1.6, 0.4, 3.8),
            0.5,
            Point3::default(),
            GLASS,
        );
        // Goggles pushed up on the brow.
        let lens = p3(side * 4.3, 5.4, 14.6);
        mesh.hoop(lens, p3(3.3, 0.7, 2.6), 0.75, ARMOR);
        mesh.sphere(
            lens.add(p3(0.0, 0.5, 0.0)),
            p3(2.6, 0.7, 2.0),
            4,
            10,
            |_| GLASS,
        );
        mesh.sphere(p3(side * 9.6, 0.0, 10.6), p3(1.8, 2.8, 3.2), 4, 8, |_| {
            ARMOR
        });
        mesh.sphere(p3(side * 10.6, 0.4, 10.6), p3(1.0, 1.7, 2.0), 3, 8, |_| {
            GLASS
        });
        // Small hovering hands; the throwing hand rises.
        let raise = if throwing && side > 0.0 { 5.0 } else { 0.0 };
        let bob = (time * 3.0 + side).sin() * 0.5;
        mesh.sphere(
            p3(side * 8.0, 3.0, 4.0 + bob + raise - fall * 2.0),
            p3(1.7, 1.7, 1.6),
            4,
            8,
            |_| CLAY,
        );
    }
    mesh.beam(p3(-1.2, 6.0, 14.8), p3(1.2, 6.0, 14.8), 0.5, ARMOR);
    // Rear jet: the weak point glows hot while the guardian is stunned.
    mesh.tube(
        &[
            p3(0.0, -6.0, 10.2),
            p3(0.0, -8.0, 10.0),
            p3(0.0, -9.2, 10.0),
        ],
        &[2.6, 2.3, 2.0],
        1.0,
        ARMOR,
    );
    mesh.hoop(p3(0.0, -9.2, 10.0), p3(2.1, 0.4, 2.1), 0.6, METAL);
    let r = if open { 1.9 } else { 1.4 };
    mesh.sphere(p3(0.0, -8.6, 10.0), p3(r, r, r), 4, 8, |_| {
        if open { LUMINOUS } else { EMBER }
    });
    if !open && fall <= 0.0 {
        let flame = 2.4 + (time * TAU / 0.3).sin() * 0.6;
        mesh.tube(
            &[p3(0.0, -9.6, 10.0), p3(0.0, -9.6 - flame, 10.0)],
            &[1.1, 0.1],
            1.0,
            ENERGY,
        );
    }
}

#[path = "sculpture_cline.rs"]
mod cline;
#[path = "sculpture_cursor.rs"]
mod cursor;
#[path = "sculpture_grok.rs"]
mod grok;
pub(super) use grok::blade;

#[cfg(test)]
#[path = "sculpture_tests.rs"]
mod tests;

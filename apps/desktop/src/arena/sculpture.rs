//! Authored spatial pixel sculptures. Geometry, articulated joints and material
//! ramps are shared by the home preview and the live encounter.

use super::{
    geometry::{Vec2, smoothstep},
    guardian::{Attack, Boss, BossState, Guardian},
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
    fn lerp(self, p: Self, t: f32) -> Self {
        self.add(p.sub(self).scale(t))
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

const CLAY: [u8; 5] = [BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT, BODY_GLEAM];
const ARMOR: [u8; 5] = [BODY_SHADOW, BODY_SHADOW, BODY_DARK, BODY, BODY_LIGHT];
const METAL: [u8; 5] = [BODY_SHADOW, GOLD_DARK, GOLD, IVORY, WHITE];
const DARK: [u8; 5] = [OUTLINE, OUTLINE, SHADOW, BODY_SHADOW, BODY_DARK];
const GLASS: [u8; 5] = [WATER_DARK, WATER_DARK, WATER, WATER_LIGHT, CORE_LIGHT];
const BONE: [u8; 5] = [GOLD_DARK, DUST, IVORY, WHITE, WHITE];
const PORCELAIN: [u8; 5] = [DUST, IVORY, WHITE, WHITE, WHITE];
const ENERGY: [u8; 5] = [CORE_DARK, CORE, CORE_LIGHT, WHITE, WHITE];
const LUMINOUS: [u8; 5] = [CORE, CORE_LIGHT, WHITE, WHITE, WHITE];

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
    fn ellipsoid(&mut self, center: Point3, size: Point3, ramp: [u8; 5]) {
        for lat in 0..8 {
            for lon in 0..12 {
                let point = |lat: usize, lon: usize| {
                    let a = lat as f32 / 8.0 * PI - PI * 0.5;
                    let b = lon as f32 / 12.0 * TAU;
                    center.add(Point3::new(
                        a.cos() * b.cos() * size.x,
                        a.cos() * b.sin() * size.y,
                        a.sin() * size.z,
                    ))
                };
                self.quad(
                    [
                        point(lat, lon),
                        point(lat, lon + 1),
                        point(lat + 1, lon + 1),
                        point(lat + 1, lon),
                    ],
                    ramp,
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
    fn frame(&mut self, center: Point3, size: Point3, border: f32, ramp: [u8; 5]) {
        for side in [-1.0, 1.0] {
            self.cuboid(
                center.add(Point3::new(side * (size.x - border) * 0.5, 0.0, 0.0)),
                Point3::new(border, size.y, size.z),
                ramp,
            );
            self.cuboid(
                center.add(Point3::new(0.0, 0.0, side * (size.z - border) * 0.5)),
                Point3::new(size.x - border * 2.0, size.y, border),
                ramp,
            );
        }
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
            let normal = points[1].sub(points[0]).cross(points[2].sub(points[0]));
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
    }
}

pub(super) fn draw(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let time = if reduced {
        0.0
    } else {
        time.rem_euclid(super::art::IDLE_PERIOD)
    };
    let fall = smoothstep(fallen / 1.8);
    let mut mesh = Mesh::new();
    let pivot = match boss.guardian {
        Guardian::Claude => {
            claude(&mut mesh, boss, fall);
            Point3::new(0.0, 3.7, 6.3)
        }
        Guardian::Codex => {
            codex(&mut mesh, boss, time, fall);
            Point3::new(0.0, 0.0, 11.0)
        }
        Guardian::Pi => {
            pi(&mut mesh, boss, time, fall);
            Point3::new(-2.6, -0.6, 17.0)
        }
        Guardian::OpenCode => {
            terminal(&mut mesh, boss, time, fall);
            Point3::new(0.0, 5.7, 9.0)
        }
        Guardian::DeepSeek => {
            whale(&mut mesh, boss, time, fall);
            Point3::new(0.0, -15.5, 15.0)
        }
        Guardian::Copilot => {
            copilot(&mut mesh, boss, time, fall);
            Point3::new(0.0, 4.2, 3.8)
        }
    };
    let (pitch, bank, height) = match boss.guardian {
        Guardian::Claude => (
            boss.pitch + fall * 0.38,
            boss.bank,
            boss.height - fall * 4.0,
        ),
        Guardian::Codex => (boss.spin, boss.bank, boss.height),
        Guardian::Pi => (boss.pitch, boss.bank, boss.height),
        Guardian::OpenCode => (boss.pitch, boss.bank + fall * 0.18, boss.height),
        Guardian::DeepSeek => (
            boss.pitch - fall * 0.4,
            boss.bank,
            boss.height - fall * 15.0,
        ),
        Guardian::Copilot => (
            boss.pitch + fall * 0.45,
            boss.bank + fall * 0.25,
            boss.height * (1.0 - fall),
        ),
    };
    mesh.draw(
        frame,
        boss.position,
        Point3::new(pitch, bank, boss.yaw),
        height,
        pivot,
    );
}

fn claude(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let open = if boss.exposed > 0.0 { 1.0 } else { 0.0 };
    let split = open * 2.5 + fall * 3.5;
    mesh.bevel_box(
        Point3::new(0.0, 0.0, 15.3),
        Point3::new(19.0, 10.0, 10.5),
        0.65,
        Point3::default(),
        CLAY,
    );
    mesh.cuboid(
        Point3::new(-0.25, -0.3, 20.57),
        Point3::new(16.9, 8.0, 0.22),
        CLAY,
    );
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            Point3::new(side * 4.3, 5.04, 15.6),
            Point3::new(2.15, 0.42, 5.0),
            0.18,
            Point3::default(),
            ARMOR,
        );
        mesh.cuboid(
            Point3::new(side * 4.3, 5.30, 15.6),
            Point3::new(1.5, 0.16, 4.2),
            PORCELAIN,
        );
        for x in [2.6, 7.6] {
            mesh.bevel_box(
                Point3::new(side * x, 3.5, 9.5),
                Point3::new(2.0, 2.9, 2.6),
                0.25,
                Point3::default(),
                CLAY,
            );
        }
    }
    for ix in 0..8 {
        let a = ix as f32 * TAU / 8.0;
        let b = (ix + 1) as f32 * TAU / 8.0;
        let shift = Point3::new((a + PI / 8.0).cos().signum() * split, 0.0, -fall * 2.0);
        let top = |angle: f32| Point3::new(angle.cos() * 5.8, angle.sin() * 4.2, 10.1).add(shift);
        let tip = Point3::new(shift.x * 0.65, 0.5, 1.4 - fall * 1.5);
        let rim =
            Point3::new((a + PI / 8.0).cos() * 3.6, (a + PI / 8.0).sin() * 3.0, 6.0).add(shift);
        mesh.quad([top(b), top(a), rim, rim], CLAY);
        mesh.quad([rim, top(a), tip, tip], ARMOR);
        mesh.quad([top(b), rim, tip, tip], ARMOR);
    }
    mesh.crystal(
        Point3::new(0.0, 3.7, 6.3),
        Point3::new(0.28 + open * 1.2, 0.6, 2.7 + open * 0.5),
        LUMINOUS,
    );
}

fn ground_point(boss: &Boss, ground: Vec2, height: f32) -> Point3 {
    let p = ground.minus(boss.position);
    Point3::new(p.x, p.y / 0.62, height)
}

pub(super) fn tentacle(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    let fall = smoothstep(fallen / 1.5);
    let (ground, root_height) = boss.tentacle_root(ix);
    let root = ground_point(boss, ground, root_height - fall * 4.0);
    let tip = ground_point(
        boss,
        boss.tentacles[ix],
        boss.tentacle_heights[ix] * (1.0 - fall),
    );
    let side = if ix.is_multiple_of(2) { -1.0 } else { 1.0 };
    let curl =
        Point3::new(side * 3.4, if ix < 2 { -1.4 } else { 1.2 }, 0.0).rotate(boss.yaw, 0.0, 0.0);
    let joint = root.lerp(tip, 0.72);
    let path = [
        root,
        root.lerp(tip, 0.18)
            .add(curl.scale(0.55))
            .add(Point3::new(0.0, 0.0, 0.9)),
        root.lerp(tip, 0.38)
            .add(curl)
            .add(Point3::new(0.0, 0.0, -0.1)),
        root.lerp(tip, 0.57)
            .add(curl.scale(0.85))
            .add(Point3::new(0.0, 0.0, 0.35)),
        joint,
        root.lerp(tip, 0.89)
            .sub(curl.scale(0.35))
            .add(Point3::new(0.0, 0.0, -1.0)),
        tip,
    ];
    let cut = boss.severed & (1 << ix) != 0;
    let mut mesh = Mesh::new();
    let length = if cut { 5 } else { 7 };
    mesh.tube(
        &path[..length],
        &[2.3, 2.3, 1.95, 1.55, 1.08, 0.62, 0.16][..length],
        0.86,
        CLAY,
    );
    let tangent = path[4].sub(path[3]).normalized();
    mesh.beam(
        joint.sub(tangent.scale(0.24)),
        joint.add(tangent.scale(0.24)),
        1.28,
        if cut || boss.tentacle_vulnerable(ix) {
            ENERGY
        } else {
            ARMOR
        },
    );
    if cut {
        mesh.crystal(joint, Point3::new(0.62, 0.56, 0.58), LUMINOUS);
    }
    mesh.draw(
        frame,
        boss.position,
        Point3::default(),
        0.0,
        Point3::default(),
    );
}

pub(super) fn hand(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    let side = if ix == 0 { -1.0 } else { 1.0 };
    let fall = smoothstep(fallen / 1.5);
    let height = boss.hand_heights[ix] * (1.0 - fall);
    let shoulder = Point3::new(side * 11.0, -0.8, 12.0)
        .rotate(boss.yaw, 0.0, 0.0)
        .add(Point3::new(0.0, 0.0, boss.height));
    let wrist = ground_point(boss, boss.hands[ix], height + 3.0);
    let mut mesh = Mesh::new();
    for (start, end, radius) in [(0.08, 0.31, 1.8), (0.44, 0.72, 2.25)] {
        mesh.beam(
            shoulder.lerp(wrist, start),
            shoulder.lerp(wrist, end),
            radius,
            CLAY,
        );
    }
    mesh.beam(
        shoulder.lerp(wrist, 0.30),
        shoulder.lerp(wrist, 0.45),
        0.30,
        PORCELAIN,
    );
    mesh.group(
        wrist,
        Point3::new(0.06 + height * 0.012, side * fall * 0.6, boss.yaw * 0.55),
        |mesh| {
            mesh.bevel_box(
                Point3::new(0.0, -0.6, 0.0),
                Point3::new(6.4, 4.7, 4.9),
                0.5,
                Point3::default(),
                CLAY,
            );
            for finger in 0..4 {
                let x = -2.3 + finger as f32 * 1.5;
                mesh.bevel_box(
                    Point3::new(x, 2.0, -0.8),
                    Point3::new(1.30, 2.9, 3.1),
                    0.27,
                    Point3::default(),
                    CLAY,
                );
                mesh.cuboid(
                    Point3::new(x, 2.0, 0.83),
                    Point3::new(0.8, 1.7, 0.13),
                    ARMOR,
                );
            }
            mesh.bevel_box(
                Point3::new(-side * 3.45, 0.9, -0.8),
                Point3::new(1.9, 2.8, 2.1),
                0.3,
                Point3::new(0.0, -side * 0.3, 0.0),
                CLAY,
            );
            mesh.frame(
                Point3::new(0.0, 1.86, 0.15),
                Point3::new(2.4, 0.2, 2.0),
                0.35,
                PORCELAIN,
            );
            if boss.seals & (1 << ix) != 0 {
                mesh.beam(
                    Point3::new(-3.6, 0.0, -2.7),
                    Point3::new(3.6, 0.0, -2.7),
                    0.20,
                    LUMINOUS,
                );
            }
        },
    );
    mesh.draw(
        frame,
        boss.position,
        Point3::default(),
        0.0,
        Point3::default(),
    );
}

fn codex(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let opened = if boss.exposed > 0.0 {
        1.0
    } else {
        boss.strain * 0.7
    };
    let split = opened * 3.8 + fall * 8.0;
    let rotation = if boss.attack == Attack::Barrage && boss.state == BossState::Striking {
        boss.progress() * TAU
    } else {
        0.0
    };
    for strand in 0..6 {
        let angle = strand as f32 * TAU / 6.0 + rotation;
        let origin = Point3::new(angle.cos() * split, 0.0, 11.0 + angle.sin() * split);
        mesh.group(origin, Point3::new(0.0, -angle, 0.0), |mesh| {
            let corners = [
                Point3::new(1.7, -2.1, 2.95),
                Point3::new(5.3, -2.1, 5.0),
                Point3::new(9.0, -0.6, 2.85),
                Point3::new(9.0, 2.1, -2.85),
                Point3::new(5.3, 2.1, -5.0),
                Point3::new(1.7, 2.1, -2.95),
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
                mesh.cuboid(Point3::new(x, y, z), Point3::new(0.22, 0.13, tall), METAL);
                mesh.cuboid(
                    Point3::new(x + 0.37, y, z - tall * 0.3),
                    Point3::new(0.6, 0.13, 0.16),
                    METAL,
                );
            }
        });
    }
    let core = Point3::new(0.0, 0.0, 11.0);
    mesh.bevel_box(
        core,
        Point3::new(2.7, 2.7, 2.7),
        0.20,
        Point3::new(0.0, 0.0, PI * 0.25),
        LUMINOUS,
    );
    for ix in 0..6 {
        let a = ix as f32 * TAU / 6.0 + time * TAU / 12.0;
        let radius = 13.4 + fall * 6.0;
        let center = Point3::new(a.cos() * radius, a.sin() * 4.8, 11.0 + a.sin() * 11.0);
        mesh.bevel_box(
            center,
            Point3::new(0.9, 1.1, 2.4 + (ix % 2) as f32),
            0.18,
            Point3::new(0.0, a * 0.25, 0.0),
            CLAY,
        );
        mesh.cuboid(
            center.add(Point3::new(0.0, 0.05, -2.6)),
            Point3::new(0.18, 0.23, 1.8),
            ENERGY,
        );
    }
}

fn pi(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    // Four rows keep the two separated stems and the square through-aperture
    // legible at every size; the masonry has actual side and rear planes.
    for (row, mask) in [0b1110u8, 0b1010, 0b1101, 0b1001].into_iter().enumerate() {
        for col in 0..4 {
            if mask & (1 << (3 - col)) == 0 {
                continue;
            }
            let drift = Point3::new(
                (col as f32 - 1.5) * fall * 4.0,
                (col as f32 - row as f32) * fall,
                -fall * (3.0 + row as f32 * 2.0),
            );
            let center =
                Point3::new(-7.8 + col as f32 * 5.2, 0.0, 22.2 - row as f32 * 5.2).add(drift);
            mesh.group(
                center,
                Point3::new(fall * 0.20, (col as f32 - 1.5) * fall * 0.16, 0.0),
                |mesh| {
                    mesh.bevel_box(
                        Point3::default(),
                        Point3::new(5.16, 6.4, 5.16),
                        0.24,
                        Point3::default(),
                        CLAY,
                    );
                    mesh.cuboid(
                        Point3::new(0.0, 3.23, 0.10),
                        Point3::new(4.85, 0.10, 0.15),
                        DARK,
                    );
                    for (x, z) in [(-0.80, 1.3), (0.95, -1.25)] {
                        mesh.cuboid(Point3::new(x, 3.23, z), Point3::new(0.14, 0.11, 2.05), DARK);
                    }
                    mesh.cuboid(
                        Point3::new(2.61, 0.0, -0.12),
                        Point3::new(0.1, 5.8, 0.16),
                        DARK,
                    );
                    mesh.cuboid(
                        Point3::new(-2.61, 0.0, 0.10),
                        Point3::new(0.1, 5.8, 0.16),
                        DARK,
                    );
                },
            );
        }
    }
    let core = Point3::new(-2.6, -0.6, 17.0);
    mesh.bevel_box(
        core,
        Point3::new(if boss.exposed > 0.0 { 3.5 } else { 2.9 }, 0.8, 3.5),
        0.10,
        Point3::default(),
        PORCELAIN,
    );
    for (ix, side) in [-1.0, 1.0].into_iter().enumerate() {
        if boss.seals & (1 << ix) == 0 {
            mesh.box_rotated(
                core.add(Point3::new(side * 0.86, 2.4, 0.0)),
                Point3::new(0.65, 0.85, 4.1),
                Point3::new(0.0, side * 0.30, 0.0),
                CLAY,
            );
        }
    }
    for (ix, (x, y, z, size)) in [
        (-12.8, -3.0, 23.0, 1.8),
        (13.3, -1.0, 22.0, 1.9),
        (-13.2, 2.0, 7.0, 1.2),
        (12.5, -4.0, 14.0, 1.4),
        (-4.5, 0.0, 0.6, 2.1),
        (5.5, 1.0, 1.3, 1.7),
        (-9.5, -3.5, 28.0, 1.25),
        (10.6, -2.0, 3.8, 1.2),
    ]
    .into_iter()
    .enumerate()
    {
        let phase = time * TAU / 6.0 + ix as f32 * PI / 4.0;
        let center = Point3::new(
            x * (1.0 + fall * 0.3),
            y,
            z + phase.sin() * 0.45 - fall * 5.0,
        );
        mesh.bevel_box(
            center,
            Point3::new(size, size * 1.15, size * 1.3),
            0.18,
            Point3::new(phase.sin() * 0.12, ix as f32 * 0.13 + fall, 0.0),
            CLAY,
        );
        if ix == 4 || ix == 5 {
            mesh.cuboid(
                center.add(Point3::new(0.0, 0.0, -size * 0.9)),
                Point3::new(0.18, 0.25, 0.9),
                PORCELAIN,
            );
        }
    }
}

fn terminal(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let open = if boss.exposed > 0.0 {
        1.0
    } else if boss.is_inhaling() {
        0.55
    } else {
        0.0
    };
    let peel = boss.tether.max(open) + fall;
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            Point3::new(side * (6.55 + fall * 4.0), 0.0, 9.0),
            Point3::new(3.9, 11.0, 18.0),
            0.35,
            Point3::new(0.0, side * fall * 0.25, 0.0),
            CLAY,
        );
        mesh.bevel_box(
            Point3::new(0.0, 0.0, 9.0 + side * (7.1 + fall * 3.5)),
            Point3::new(9.25, 11.0, 3.8),
            0.28,
            Point3::new(fall * 0.18, 0.0, 0.0),
            CLAY,
        );
        mesh.cuboid(
            Point3::new(side * (6.85 + fall * 4.0), 5.66, 9.0),
            Point3::new(2.9, 0.82, 17.1),
            PORCELAIN,
        );
        mesh.cuboid(
            Point3::new(0.0, 5.66, 9.0 + side * (7.05 + fall * 3.5)),
            Point3::new(10.8, 0.82, 3.0),
            PORCELAIN,
        );
        mesh.box_rotated(
            Point3::new(side * (2.0 + peel * 3.7), 2.8 - peel * 1.1, 9.0),
            Point3::new(3.9, 1.5, 10.4),
            Point3::new(0.0, 0.0, -side * peel * 0.70),
            ARMOR,
        );
        for z in [4.0, 9.0, 14.0] {
            mesh.cuboid(
                Point3::new(side * (8.54 + fall * 4.0), 0.0, z),
                Point3::new(0.15, 9.3, 0.18),
                DARK,
            );
        }
    }
    mesh.cuboid(
        Point3::new(0.0, -5.4, 9.0),
        Point3::new(9.5, 0.5, 11.8),
        DARK,
    );
    mesh.frame(
        Point3::new(0.0, 3.2, 9.0),
        Point3::new(9.5, 0.3, 11.9),
        0.40,
        ARMOR,
    );
    if open > 0.0 || boss.tether > 0.0 {
        mesh.bevel_box(
            Point3::new(0.0, 5.7, 9.0),
            Point3::new(1.0 + open * 1.8, 0.5, 1.0 + open * 1.8),
            0.18,
            Point3::default(),
            LUMINOUS,
        );
    }
    for (ix, (x, y, z)) in [
        (-12.0, -2.0, 12.0),
        (12.0, -1.0, 10.0),
        (-10.5, 3.0, 3.0),
        (10.5, 2.0, 20.0),
        (1.0, -4.0, 22.0),
        (-1.0, 1.0, -2.5),
    ]
    .into_iter()
    .enumerate()
    {
        let phase = time * TAU / 6.0 + ix as f32 * PI / 3.0;
        let size = if ix < 4 {
            Point3::new(2.8, 1.4, 5.4)
        } else {
            Point3::new(5.0, 2.3, 1.8)
        };
        mesh.group(
            Point3::new(
                x * (1.0 + fall * 0.45),
                y + phase.cos() * 0.6,
                z + phase.sin() * 0.45,
            ),
            Point3::new(0.0, phase.sin() * 0.13 + fall * 0.25, 0.12),
            |mesh| {
                mesh.frame(Point3::default(), size, 0.58, CLAY);
                mesh.cuboid(
                    Point3::new(-size.x * 0.5 + 0.22, size.y * 0.5 + 0.1, 0.0),
                    Point3::new(0.32, 0.16, size.z - 0.5),
                    PORCELAIN,
                );
            },
        );
    }
}

fn whale(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let center = Point3::new(0.0, 1.2, 8.2);
    let size = Point3::new(10.2, 10.8, 8.2);
    mesh.ellipsoid(center, size.scale(0.985), ARMOR);
    for lat in 0..8 {
        for lon in 0..16 {
            let point = |lat: usize, lon: usize| {
                let a = lat as f32 / 8.0 * PI - PI * 0.5;
                let b = (lon as f32 + if lat.is_multiple_of(2) { 0.0 } else { 0.35 }) * TAU / 16.0;
                center.add(Point3::new(
                    a.cos() * b.cos() * size.x,
                    a.cos() * b.sin() * size.y,
                    a.sin() * size.z,
                ))
            };
            let points = [
                point(lat, lon),
                point(lat, lon + 1),
                point(lat + 1, lon + 1),
                point(lat + 1, lon),
            ];
            let middle = points
                .iter()
                .fold(Point3::default(), |sum, p| sum.add(*p))
                .scale(0.25);
            let ramp = if middle.z < 7.7 && middle.y > -5.5 {
                BONE
            } else {
                CLAY
            };
            mesh.quad(points.map(|p| middle.lerp(p, 0.963)), ramp);
        }
    }
    let swim = (time * TAU / 3.0).sin() * 0.45;
    mesh.tube(
        &[
            Point3::new(0.0, -6.0, 7.4),
            Point3::new(0.0, -10.0, 7.8),
            Point3::new(0.0, -14.0, 10.8),
            Point3::new(0.0, -15.5, 15.0),
        ],
        &[4.6, 3.3, 2.2, 1.45],
        1.0,
        CLAY,
    );
    for side in [-1.0, 1.0] {
        mesh.tube(
            &[
                Point3::new(0.0, -15.5, 15.0),
                Point3::new(side * 3.6, -16.8, 16.9 + swim * side),
                Point3::new(side * 6.4, -17.0, 20.0 + swim * side),
                Point3::new(side * 6.8, -16.1, 22.5 + swim * side),
            ],
            &[1.25, 1.75, 1.3, 0.10],
            0.48,
            CLAY,
        );
        mesh.tube(
            &[
                Point3::new(side * 8.1, 3.5, 5.8),
                Point3::new(side * 10.4, 1.5, 4.5 + swim * side),
                Point3::new(side * 12.5, -1.8, 4.3 + swim * side + fall),
            ],
            &[1.65, 1.40, 0.10],
            0.40,
            CLAY,
        );
        let eye = Point3::new(side * 8.15, 6.9, 10.6);
        mesh.ellipsoid(eye, Point3::new(0.9, 0.7, 0.85), DARK);
        mesh.ellipsoid(
            eye.add(Point3::new(0.0, 0.4, 0.12)),
            Point3::new(0.48, 0.40, 0.48),
            LUMINOUS,
        );
        mesh.beam(
            Point3::new(side * 8.5, 5.9, 11.4),
            Point3::new(side * 9.0, 3.9, 11.8),
            0.20,
            ENERGY,
        );
        mesh.beam(
            Point3::new(side * 9.0, 3.9, 11.8),
            Point3::new(side * 9.6, 1.7, 10.9),
            0.22,
            ENERGY,
        );
    }
    mesh.tube(
        &[
            Point3::new(0.0, -1.0, 15.2),
            Point3::new(0.0, -4.0, 18.1),
            Point3::new(0.0, -6.0, 14.8),
        ],
        &[1.1, 0.75, 0.05],
        0.75,
        CLAY,
    );
    let opened = boss.exposed > 0.0;
    mesh.hoop(
        Point3::new(0.0, -15.5, 15.0),
        Point3::new(1.72, 0.7, 1.72),
        0.55,
        if opened { BONE } else { CLAY },
    );
    if opened {
        mesh.crystal(
            Point3::new(0.0, -15.5, 15.0),
            Point3::new(1.05, 0.8, 1.2),
            LUMINOUS,
        );
    }
}

fn copilot(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let charged = boss.attack == Attack::VisorBeam
        && matches!(boss.state, BossState::Windup | BossState::Striking);
    let opened = boss.exposed > 0.0;
    mesh.ellipsoid(
        Point3::new(0.0, 0.0, 12.2),
        Point3::new(9.3, 6.7, 7.5),
        CLAY,
    );
    mesh.bevel_box(
        Point3::new(0.0, 5.75, 9.0),
        Point3::new(14.3, 2.0, 6.7),
        1.0,
        Point3::default(),
        ARMOR,
    );
    mesh.bevel_box(
        Point3::new(0.0, 6.22, 9.15),
        Point3::new(13.4, 1.40, 5.75),
        0.7,
        Point3::default(),
        BONE,
    );
    for side in [-1.0, 1.0] {
        mesh.bevel_box(
            Point3::new(side * 2.20, 7.0, 9.1),
            Point3::new(1.25, 0.42, 2.5),
            0.22,
            Point3::default(),
            DARK,
        );
        mesh.cuboid(
            Point3::new(side * 2.2, 7.24, 9.35),
            Point3::new(0.70, 0.14, 0.42),
            GLASS,
        );
        let lens = Point3::new(side * 4.4, 5.9, 14.2);
        mesh.hoop(lens, Point3::new(3.95, 0.90, 3.65), 0.62, DARK);
        mesh.hoop(
            lens.add(Point3::new(0.0, 0.90, 0.0)),
            Point3::new(3.60, 0.36, 3.32),
            0.38,
            BONE,
        );
        mesh.ellipsoid(
            lens.add(Point3::new(0.0, 0.92, 0.0)),
            Point3::new(3.12, 0.78, 2.86),
            GLASS,
        );
        mesh.box_rotated(
            lens.add(Point3::new(-0.92, 1.68, 1.42)),
            Point3::new(1.25, 0.11, 0.42),
            Point3::new(0.0, -0.40, 0.0),
            PORCELAIN,
        );
        if charged {
            mesh.crystal(
                lens.add(Point3::new(0.0, 1.75, 0.0)),
                Point3::new(0.65, 0.32, 0.65),
                LUMINOUS,
            );
        }
        mesh.ellipsoid(
            Point3::new(side * 10.0, 0.0, 10.9),
            Point3::new(1.6, 2.6, 3.0),
            ARMOR,
        );
        mesh.ellipsoid(
            Point3::new(side * 10.5, 0.7, 10.9),
            Point3::new(1.3, 2.25, 2.5),
            CLAY,
        );
        mesh.ellipsoid(
            Point3::new(side * 10.85, 1.2, 10.9),
            Point3::new(0.6, 1.0, 1.1),
            GLASS,
        );
        let hand = Point3::new(
            side * (6.1 + if charged { 0.6 } else { 0.0 }),
            1.5,
            3.45 - fall,
        );
        mesh.beam(Point3::new(side * 3.8, 0.0, 4.7), hand, 0.52, ARMOR);
        mesh.bevel_box(
            hand,
            Point3::new(2.4, 2.6, 2.5),
            0.5,
            Point3::new(0.0, side * 0.25, 0.0),
            CLAY,
        );
        mesh.ellipsoid(
            hand.add(Point3::new(0.0, 1.35, 0.0)),
            Point3::new(0.7, 0.35, 0.7),
            GLASS,
        );
        mesh.bevel_box(
            Point3::new(side * (2.0 + if opened { 1.9 } else { 0.0 }), 2.2, 3.8),
            Point3::new(1.3, 3.5, 3.8),
            0.3,
            Point3::new(0.0, side * if opened { 0.45 } else { 0.0 }, 0.0),
            CLAY,
        );
        mesh.bevel_box(
            Point3::new(side * 1.55, 0.4, 0.95),
            Point3::new(2.1, 2.4, 1.6),
            0.20,
            Point3::default(),
            ARMOR,
        );
        let exhaust = (2.0 + (time * TAU / 0.75).sin() * 0.35) * (1.0 - fall);
        mesh.tube(
            &[
                Point3::new(side * 1.55, 0.4, 0.4),
                Point3::new(side * 1.55, 0.4, -0.7),
                Point3::new(side * 1.55, 0.4, -exhaust - 0.9),
            ],
            &[0.65, 0.5, 0.04],
            0.8,
            ENERGY,
        );
    }
    mesh.bevel_box(
        Point3::new(0.0, 6.8, 14.2),
        Point3::new(1.5, 0.75, 1.1),
        0.18,
        Point3::default(),
        ARMOR,
    );
    mesh.bevel_box(
        Point3::new(0.0, 1.6, 3.8),
        Point3::new(4.3, 3.5, 3.9),
        0.45,
        Point3::default(),
        ARMOR,
    );
    mesh.hoop(
        Point3::new(0.0, 3.65, 3.8),
        Point3::new(1.65, 0.45, 1.65),
        0.42,
        GLASS,
    );
    mesh.crystal(
        Point3::new(0.0, 4.2, 3.8),
        Point3::new(
            if opened { 1.32 } else { 0.87 },
            0.6,
            if opened { 1.32 } else { 0.87 },
        ),
        LUMINOUS,
    );
    mesh.ellipsoid(
        Point3::new(0.0, -6.55, 11.9),
        Point3::new(2.8, 0.9, 2.8),
        ARMOR,
    );
    mesh.ellipsoid(
        Point3::new(0.0, -7.2, 11.9),
        Point3::new(1.9, 0.45, 1.9),
        GLASS,
    );
    if charged && !opened {
        for ix in 0..4 {
            let a = boss.shield_angle + ix as f32 * PI * 0.5;
            mesh.bevel_box(
                Point3::new(a.cos() * 12.6, a.sin() * 5.0, 12.0 + a.sin() * 8.4),
                Point3::new(3.7, 0.7, 1.4),
                0.24,
                Point3::new(0.0, -a - PI * 0.5, 0.0),
                GLASS,
            );
        }
    }
}

#[cfg(test)]
#[path = "sculpture_tests.rs"]
mod tests;

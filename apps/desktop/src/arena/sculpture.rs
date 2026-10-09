//! Small orthographic meshes rasterized into the same integer pixel grid as the
//! world. Real face rotation supplies foreshortening, occlusion and side planes.

use super::{
    geometry::{Vec2, smoothstep},
    guardian::{Attack, Boss, BossState, Guardian},
    raster::{Raster, ink::*},
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
    fn cross(self, p: Self) -> Self {
        Self::new(
            self.y * p.z - self.z * p.y,
            self.z * p.x - self.x * p.z,
            self.x * p.y - self.y * p.x,
        )
    }
    fn length(self) -> f32 {
        (self.x * self.x + self.y * self.y + self.z * self.z).sqrt()
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
const METAL: [u8; 5] = [GOLD_DARK, GOLD_DARK, GOLD, IVORY, WHITE];
const DARK: [u8; 5] = [OUTLINE, OUTLINE, DEPTH, BODY_SHADOW, BODY_DARK];
const GLASS: [u8; 5] = [WATER_DARK, WATER_DARK, WATER, WATER_LIGHT, IVORY];
const BONE: [u8; 5] = [BODY_DARK, BODY, BODY_LIGHT, IVORY, WHITE];

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
            faces: Vec::with_capacity(512),
        }
    }
    fn quad(&mut self, points: [Point3; 4], ramp: [u8; 5]) {
        self.faces.push(Face { points, ramp });
    }
    fn sheet(&mut self, points: [Point3; 4], ramp: [u8; 5]) {
        // Fins and feather plates have two visible sides. Their winding must
        // not make a mirrored limb disappear when the guardian turns.
        self.quad(points, ramp);
        self.quad([points[3], points[2], points[1], points[0]], ramp);
    }
    fn cuboid(&mut self, center: Point3, size: Point3, ramp: [u8; 5]) {
        self.box_rotated(center, size, Point3::default(), ramp);
    }
    fn box_rotated(&mut self, center: Point3, size: Point3, angles: Point3, ramp: [u8; 5]) {
        let corners = [
            (-1., -1., -1.),
            (1., -1., -1.),
            (1., 1., -1.),
            (-1., 1., -1.),
            (-1., -1., 1.),
            (1., -1., 1.),
            (1., 1., 1.),
            (-1., 1., 1.),
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
    fn ellipsoid(&mut self, center: Point3, size: Point3, ramp: [u8; 5]) {
        for lat in 0..8 {
            for lon in 0..14 {
                let point = |lat: i32, lon: i32| {
                    let a = lat as f32 / 8.0 * PI - PI * 0.5;
                    let b = lon as f32 / 14.0 * TAU;
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
    fn beam(&mut self, a: Point3, b: Point3, radius: f32, ramp: [u8; 5]) {
        let d = b.sub(a);
        let length = d.length().max(0.001);
        let d = d.scale(1.0 / length);
        let side = if d.z.abs() < 0.9 {
            d.cross(Point3::new(0.0, 0.0, 1.0))
        } else {
            d.cross(Point3::new(0.0, 1.0, 0.0))
        };
        let side = side.scale(radius / side.length());
        let up = d.cross(side);
        for ix in 0..6 {
            let p = |origin: Point3, ix: usize| {
                let a = ix as f32 * TAU / 6.0;
                origin.add(side.scale(a.cos())).add(up.scale(a.sin()))
            };
            self.quad([p(a, ix), p(a, ix + 1), p(b, ix + 1), p(b, ix)], ramp);
        }
    }
    fn draw(self, frame: &mut Raster, anchor: Vec2, yaw: f32, pitch: f32, roll: f32, height: f32) {
        let mut faces = Vec::with_capacity(self.faces.len());
        for face in self.faces {
            let points = face.points.map(|p| {
                p.rotate(yaw, pitch, roll)
                    .add(Point3::new(0.0, 0.0, height))
            });
            let normal = points[1].sub(points[0]).cross(points[2].sub(points[0]));
            // Orthographic camera looks from the south and above the ground.
            if normal.y + normal.z * 0.62 <= 0.0 {
                continue;
            }
            let light =
                (-normal.x * 0.42 - normal.y * 0.18 + normal.z * 0.88) / normal.length().max(0.001);
            let color = face.ramp[((light + 0.62) * 2.6).round().clamp(0.0, 4.0) as usize];
            let depth = points.iter().map(|p| p.y + p.z * 0.62).sum::<f32>() * 0.25;
            faces.push((depth, points, color));
        }
        faces.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        for (_, points, color) in faces {
            let points = points.map(|p| {
                (
                    ((anchor.x + p.x) * 4.0).round() as i32,
                    ((anchor.y + p.y * 0.62 - p.z) * 4.0).round() as i32,
                )
            });
            frame.polygon(&points, color);
        }
    }
}

pub(super) fn draw(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let time = if reduced {
        0.0
    } else {
        time.rem_euclid(super::art::IDLE_PERIOD)
    };
    let mut mesh = Mesh::new();
    let fall = smoothstep(fallen / 1.5);
    let breath = (time * TAU / 3.0).sin() * 0.14;
    match boss.guardian {
        Guardian::Claude => claude(&mut mesh, boss, breath, fall),
        Guardian::Codex => codex(&mut mesh, boss, fallen),
        Guardian::Pi => pi(&mut mesh, boss, time, fallen),
        Guardian::OpenCode => terminal(&mut mesh, boss, fall),
        Guardian::DeepSeek => whale(&mut mesh, boss, time, fall),
        Guardian::Copilot => copilot(&mut mesh, boss, time, fall),
    }
    if boss.guardian == Guardian::OpenCode {
        for face in &mut mesh.faces {
            for point in &mut face.points {
                point.z -= 7.5;
            }
        }
    }
    let yaw = boss.yaw;
    let (pitch, bank, height) = match boss.guardian {
        Guardian::Codex => (boss.spin, boss.bank, boss.height + 7.0),
        Guardian::DeepSeek => (
            boss.pitch - fall * 0.8,
            boss.bank,
            boss.height - fall * 10.0,
        ),
        Guardian::OpenCode => (boss.pitch, boss.bank + fall * 0.18, boss.height + 7.5),
        Guardian::Pi => (0.0, 0.0, boss.height + fall * 6.0),
        Guardian::Copilot => (
            boss.pitch + fall * 0.45,
            boss.bank + fall * 0.3,
            boss.height * (1.0 - fall),
        ),
        Guardian::Claude => (boss.pitch, boss.bank, boss.height - fall * 1.8),
    };
    mesh.draw(frame, boss.position, yaw, pitch, bank, height);
}

fn claude(mesh: &mut Mesh, boss: &Boss, breath: f32, fall: f32) {
    // Wide square torso, two square eyes and four short planted legs.
    for (ix, x) in [-5.7, -2.1, 2.1, 5.7].into_iter().enumerate() {
        let stride = (boss.stride + ix as f32 * PI).sin()
            * if boss.state == BossState::Watching || boss.state == BossState::Dormant {
                0.75
            } else {
                0.12
            };
        mesh.cuboid(
            Point3::new(x, 0.0, 2.0 + stride),
            Point3::new(2.1, 4.0, 4.0 - fall),
            CLAY,
        );
        mesh.cuboid(Point3::new(x, 1.8, 0.8), Point3::new(2.7, 3.8, 1.4), CLAY);
    }
    mesh.cuboid(
        Point3::new(0.0, 0.0, 9.1 + breath - fall),
        Point3::new(16.5, 8.0, 10.0),
        CLAY,
    );
    mesh.cuboid(
        Point3::new(0.0, -0.2, 14.1 + breath - fall),
        Point3::new(15.6, 7.2, 0.65),
        CLAY,
    );
    for x in [-4.3, 4.3] {
        mesh.cuboid(
            Point3::new(x, 4.06, 10.9 + breath - fall),
            Point3::new(
                1.65,
                0.20,
                if boss.state == BossState::Dormant {
                    1.0
                } else {
                    2.0
                },
            ),
            DARK,
        );
        if boss.state == BossState::Windup {
            mesh.cuboid(
                Point3::new(x - 0.3, 4.2, 11.2 - fall),
                Point3::new(0.55, 0.2, 0.65),
                METAL,
            );
        }
    }
    mesh.cuboid(
        Point3::new(0.0, 4.03, 6.5 - fall),
        Point3::new(
            if boss.exposed > 0.0 { 4.2 } else { 0.7 },
            0.25,
            if boss.exposed > 0.0 { 3.5 } else { 0.7 },
        ),
        DARK,
    );
    for side in [-1.0, 1.0] {
        mesh.cuboid(
            Point3::new(side * 8.7, 0.0, 7.5),
            Point3::new(2.5, 4.2, 3.8),
            CLAY,
        );
        for ix in 0..3 {
            mesh.cuboid(
                Point3::new(side * (7.3 - ix as f32 * 1.1), 4.1, 5.1),
                Point3::new(0.8, 0.15, 0.3),
                [BODY_DARK; 5],
            );
        }
    }
}

pub(super) fn hand(frame: &mut Raster, boss: &Boss, ix: usize, fallen: f32) {
    let side = if ix == 0 { -1.0 } else { 1.0 };
    let anchor = boss.hands[ix];
    let height = boss.hand_heights[ix] * (1.0 - smoothstep(fallen));
    let shoulder = boss.local(side * 8.0, 0.0, 7.0);
    let end = anchor.plus(Vec2::new(0.0, -height));
    let a = ((shoulder.x * 4.0) as i32, (shoulder.y * 4.0) as i32);
    let b = ((end.x * 4.0) as i32, (end.y * 4.0) as i32);
    for n in 0..8 {
        let t = n as f32 / 7.0;
        let p = (
            (a.0 as f32 + (b.0 - a.0) as f32 * t) as i32,
            (a.1 as f32 + (b.1 - a.1) as f32 * t) as i32,
        );
        frame.ellipse((p.0 + 1, p.1 + 1), (4, 4), BODY_SHADOW);
        frame.ellipse(p, (3, 3), BODY_DARK);
    }
    let mut mesh = Mesh::new();
    mesh.cuboid(Point3::new(0.0, 0.0, 2.0), Point3::new(6.0, 5.0, 4.0), CLAY);
    for n in 0..4 {
        mesh.cuboid(
            Point3::new(-2.2 + n as f32 * 1.45, 2.0, 1.3),
            Point3::new(1.2, 2.4, 2.2),
            CLAY,
        );
    }
    mesh.cuboid(
        Point3::new(0.0, 0.0, 4.1),
        Point3::new(2.8, 2.5, 0.3),
        METAL,
    );
    mesh.draw(
        frame,
        anchor,
        boss.yaw * 0.3,
        0.0,
        side * (height * 0.012),
        height,
    );
}

fn codex(mesh: &mut Mesh, _: &Boss, fallen: f32) {
    // Six genuinely interwoven strands around a hollow center, each turning
    // through depth rather than rotating a logo bitmap on its front face.
    let unravel = smoothstep((fallen - 0.25) / 1.7);
    for strand in 0..6 {
        let offset = strand as f32 * TAU / 6.0;
        for part in 0..9 {
            let point = |part: i32| {
                let a = offset + part as f32 / 9.0 * TAU * 0.53;
                let r = 4.8 + (a * 3.0 - offset * 3.0).sin() * 1.2 + unravel * strand as f32 * 0.8;
                Point3::new(
                    a.cos() * r,
                    (a * 3.0 - offset * 2.0).cos() * 2.4,
                    a.sin() * r,
                )
            };
            mesh.beam(point(part), point(part + 1), 1.45, CLAY);
        }
    }
}

fn pi(mesh: &mut Mesh, boss: &Boss, time: f32, fallen: f32) {
    let rise = smoothstep(fallen / 2.0) * 4.0;
    // A hollow P mantle and an i-shaped staff bearer read as one small mage.
    mesh.cuboid(
        Point3::new(-3.6, 0.0, 8.4 + rise),
        Point3::new(2.6, 3.2, 15.4),
        BONE,
    );
    mesh.cuboid(
        Point3::new(0.2, 0.0, 15.0 + rise),
        Point3::new(6.0, 3.5, 2.5),
        BONE,
    );
    mesh.cuboid(
        Point3::new(2.7, 0.0, 11.6 + rise),
        Point3::new(2.4, 3.5, 5.0),
        BONE,
    );
    mesh.cuboid(
        Point3::new(0.0, 0.0, 8.8 + rise),
        Point3::new(5.5, 3.5, 2.3),
        BONE,
    );
    for n in 0..6 {
        let sway = (time * TAU / 3.0 + n as f32 * 0.8).sin() * 0.45;
        mesh.box_rotated(
            Point3::new(-3.0 + n as f32 * 1.0, -0.5, 2.9 + rise),
            Point3::new(1.3, 3.9, 6.0),
            Point3::new(0.0, sway * 0.1, 0.0),
            CLAY,
        );
    }
    mesh.cuboid(Point3::new(5.6, 0.1, 5.2), Point3::new(2.7, 3.0, 8.0), BONE);
    mesh.ellipsoid(
        Point3::new(5.6, 0.1, 11.6),
        Point3::new(1.8, 1.7, 1.8),
        BONE,
    );
    mesh.cuboid(
        Point3::new(5.5, 1.75, 11.4),
        Point3::new(1.8, 0.1, 0.65),
        DARK,
    );
    let cast = if boss.state == BossState::Windup {
        smoothstep(boss.progress())
    } else {
        0.0
    };
    let tip = Point3::new(9.5 - cast * 2.4, 0.2, 17.5 + cast * 3.0);
    mesh.beam(Point3::new(10.0, 0.5, 0.7), tip, 0.45, METAL);
    mesh.beam(
        Point3::new(5.6, 0.0, 7.0),
        Point3::new(9.5, 0.4, 8.0),
        0.75,
        BONE,
    );
    mesh.ellipsoid(
        tip,
        Point3::new(1.4, 1.1, 1.7),
        if cast > 0.5 { METAL } else { GLASS },
    );
    for n in 0..4 {
        let a = time * TAU / 6.0 + n as f32 * TAU / 4.0;
        let p = Point3::new(
            a.cos() * 8.5,
            -3.0 + a.sin() * 4.0,
            8.0 + a.sin() * 2.0 + rise,
        );
        mesh.box_rotated(
            p,
            Point3::new(0.6, 0.4, 2.0),
            Point3::new(0.0, 0.0, a),
            METAL,
        );
    }
}

fn terminal(mesh: &mut Mesh, boss: &Boss, fall: f32) {
    let split = fall * 3.5;
    mesh.cuboid(
        Point3::new(-5.2 - split, 0.0, 7.5),
        Point3::new(2.8, 9.0, 15.0),
        CLAY,
    );
    mesh.cuboid(
        Point3::new(5.2 + split, 0.0, 7.5),
        Point3::new(2.8, 9.0, 15.0),
        CLAY,
    );
    mesh.cuboid(
        Point3::new(0.0, -3.8, 7.5),
        Point3::new(8.0, 1.3, 13.0),
        DARK,
    );
    mesh.cuboid(
        Point3::new(0.0, 0.0, 14.0 + split),
        Point3::new(8.0, 9.0, 2.0),
        CLAY,
    );
    mesh.cuboid(
        Point3::new(0.0, 0.0, 1.0),
        Point3::new(10.0, 9.0, 2.0),
        CLAY,
    );
    let aperture = if boss.exposed > 0.0
        || boss.attack == Attack::Sweep
            && matches!(boss.state, BossState::Windup | BossState::Striking)
    {
        1.0
    } else {
        0.0
    };
    for side in [-1.0, 1.0] {
        mesh.cuboid(
            Point3::new(side * (1.9 + aperture * 1.5), 4.4, 7.0),
            Point3::new(3.7 - aperture * 2.7, 0.7, 10.0),
            CLAY,
        );
    }
    for n in 0..3 {
        mesh.cuboid(
            Point3::new(-3.0 + n as f32 * 3.0, 4.6, 2.0),
            Point3::new(1.0, 0.2, 0.3),
            DARK,
        );
    }
    mesh.cuboid(
        Point3::new(0.0, 4.2, 7.0),
        Point3::new(3.8, 0.2, 1.3),
        GLASS,
    );
    for n in 0..4 {
        mesh.cuboid(
            Point3::new(-6.7, -2.5 + n as f32 * 1.6, 7.0),
            Point3::new(0.2, 0.55, 4.4),
            DARK,
        );
    }
}

fn whale(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let swim = (time * TAU / 1.5).sin();
    mesh.ellipsoid(Point3::new(0.0, 2.0, 5.0), Point3::new(6.7, 9.2, 5.2), CLAY);
    mesh.ellipsoid(Point3::new(0.0, 5.5, 3.2), Point3::new(5.5, 5.9, 2.9), BONE);
    mesh.ellipsoid(
        Point3::new(0.0, -5.5, 5.1),
        Point3::new(4.9, 5.6, 3.8),
        CLAY,
    );
    mesh.ellipsoid(
        Point3::new(swim * 0.8, -10.0, 5.2 + swim * 0.5),
        Point3::new(2.6, 4.0, 2.1),
        CLAY,
    );
    let tail = Point3::new(swim * 1.1, -13.0, 5.6 + swim * 0.9);
    for side in [-1.0, 1.0] {
        let wing = Point3::new(side * 7.6, 0.0, 1.8 + (time * TAU / 2.0 + side).sin() * 0.4);
        mesh.sheet(
            [
                Point3::new(side * 4.5, 2.2, 4.0),
                Point3::new(side * 11.0, -3.8, 1.2 + fall * 3.0),
                Point3::new(side * 8.4, -5.5, 0.7),
                wing,
            ],
            CLAY,
        );
        mesh.sheet(
            [
                tail,
                tail.add(Point3::new(side * 6.5, -3.0, 0.9)),
                tail.add(Point3::new(side * 4.4, -5.0, 0.0)),
                tail.add(Point3::new(side * 0.6, -2.3, -0.6)),
            ],
            CLAY,
        );
        mesh.ellipsoid(
            Point3::new(side * 5.7, 6.2, 5.3),
            Point3::new(0.75, 0.75, 0.7),
            DARK,
        );
        mesh.ellipsoid(
            Point3::new(side * 5.9, 6.5, 5.6),
            Point3::new(0.25, 0.25, 0.25),
            BONE,
        );
    }
    mesh.sheet(
        [
            Point3::new(-0.9, -2.0, 8.0),
            Point3::new(0.0, -6.0, 12.5),
            Point3::new(1.0, -7.0, 7.3),
            Point3::new(0.7, -2.0, 8.0),
        ],
        CLAY,
    );
    if boss.exposed > 0.0 {
        mesh.cuboid(
            Point3::new(0.0, 10.2, 3.8),
            Point3::new(4.0, 0.5, 3.3),
            DARK,
        );
    }
}

fn copilot(mesh: &mut Mesh, boss: &Boss, time: f32, fall: f32) {
    let grounded = boss.state == BossState::Recovery;
    let flap = if grounded {
        0.15
    } else {
        (time * TAU / 0.9).sin() * 0.38
    };
    for side in [-1.0, 1.0] {
        let elbow = Point3::new(side * 8.5, -1.2, 7.0 + flap * 4.0 - fall * 4.0);
        mesh.beam(Point3::new(side * 4.0, 0.0, 6.5), elbow, 1.2, CLAY);
        for n in 0..5 {
            let base = elbow.add(Point3::new(side * n as f32 * 1.1, -n as f32 * 0.5, 0.0));
            let tip = base.add(Point3::new(
                side * (7.0 - n as f32 * 0.6),
                -4.0 - n as f32 * 0.5,
                flap * 7.0 - fall * (3.0 + n as f32),
            ));
            mesh.beam(base, tip, 0.8, if n % 2 == 0 { CLAY } else { BONE });
            mesh.sheet(
                [
                    base.add(Point3::new(0.0, 0.0, 0.5)),
                    tip,
                    tip.add(Point3::new(0.0, -1.2, -0.4)),
                    base.add(Point3::new(0.0, -1.1, -0.4)),
                ],
                BONE,
            );
        }
        mesh.beam(
            Point3::new(side * 2.5, 0.0, 2.8),
            Point3::new(side * 2.7, 1.0, 0.6),
            0.8,
            CLAY,
        );
    }
    mesh.ellipsoid(Point3::new(0.0, 0.0, 6.4), Point3::new(5.5, 4.5, 6.2), CLAY);
    mesh.cuboid(
        Point3::new(0.0, -0.5, 12.2),
        Point3::new(3.0, 5.5, 1.0),
        METAL,
    );
    for side in [-1.0, 1.0] {
        mesh.ellipsoid(
            Point3::new(side * 2.7, 3.5, 9.1),
            Point3::new(2.6, 1.2, 2.5),
            DARK,
        );
        mesh.ellipsoid(
            Point3::new(side * 2.7, 4.2, 9.1),
            Point3::new(2.0, 0.7, 1.7),
            GLASS,
        );
        mesh.cuboid(
            Point3::new(side * 2.7 - 0.5, 4.8, 9.8),
            Point3::new(1.4, 0.2, 0.35),
            BONE,
        );
    }
    mesh.beam(
        Point3::new(-0.8, 4.1, 9.5),
        Point3::new(0.8, 4.1, 9.5),
        0.4,
        METAL,
    );
    mesh.cuboid(Point3::new(0.0, 4.2, 5.0), Point3::new(2.0, 1.5, 2.4), DARK);
}

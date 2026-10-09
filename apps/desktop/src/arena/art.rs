//! Layered pixel sculptures, a tiled ruin, and the archer's animation frames.
//! Bodies use raised front faces, visible side planes and ground shadows.

use std::{
    f32::consts::{PI, TAU},
    sync::OnceLock,
};

use super::{
    combat::{
        Arena, ArrowState, Attack, Boss, BossState, EffectKind, Guardian, INTRO_DURATION,
        MIN_CHARGE, Phase, Vec2, smoothstep,
    },
    raster::{Raster, dither, hash, ink::*},
};

pub(super) const SCALE: f32 = 4.0;
pub(super) const WIDTH: usize = 384;
pub(super) const HEIGHT: usize = 224;
pub(super) const IDLE_PERIOD: f32 = 6.0;

fn pixel(point: Vec2) -> (i32, i32) {
    (
        (point.x * SCALE).round() as i32,
        (point.y * SCALE).round() as i32,
    )
}

pub(super) fn prepare() {
    floor();
    logos();
    hero_sprite();
}

pub(super) fn floor() -> &'static Raster {
    static FLOOR_ART: OnceLock<Raster> = OnceLock::new();
    FLOOR_ART.get_or_init(|| {
        let mut frame = Raster::new(WIDTH, HEIGHT);
        frame.rect(0, 0, WIDTH as i32, HEIGHT as i32, MOSS_DARK);
        frame.rect(10, 11, 365, 206, DEPTH);
        frame.rect(12, 12, 361, 198, FLOOR_SHADE);
        for row in 0..9 {
            for col in 0..16 {
                let x = 13 + col * 24 - (row % 2) * 12;
                let y = 12 + row * 24;
                let seed = hash(col, row);
                frame.rect(
                    x,
                    y,
                    23,
                    23,
                    if seed.is_multiple_of(4) {
                        FLOOR_LIGHT
                    } else {
                        FLOOR
                    },
                );
                frame.line((x + 1, y + 1), (x + 21, y + 1), FLOOR_LIGHT);
                frame.line((x + 22, y + 2), (x + 22, y + 21), SEAM);
                frame.line((x + 2, y + 22), (x + 21, y + 22), SEAM);
                if seed.is_multiple_of(3) {
                    let cut = 5 + (seed % 12) as i32;
                    frame.line((x + cut, y), (x + cut - 3, y + 6), SEAM);
                    frame.line((x + cut - 3, y + 6), (x + cut + 1, y + 10), SEAM);
                    frame.put(x + cut - 2, y + 6, FLOOR_LIGHT);
                }
                if seed.is_multiple_of(5) {
                    for i in 0..4 {
                        let s = hash(col + i, row * 7);
                        frame.rect(
                            x + (s % 17) as i32 + 2,
                            y + ((s >> 8) % 17) as i32 + 2,
                            2,
                            1,
                            FLOOR_SHADE,
                        );
                    }
                }
            }
        }
        // The carved central seal stays quiet enough to read silhouettes over it.
        for radius in [49, 52, 67, 70] {
            frame.ring((192, 113), (radius, radius * 3 / 4), SEAM, false);
        }
        for ix in 0..12 {
            let a = TAU * ix as f32 / 12.0;
            let x = 192 + (a.cos() * 60.0).round() as i32;
            let y = 113 + (a.sin() * 45.0).round() as i32;
            frame.line((x - 2, y - 2), (x + 2, y + 2), SEAM);
            frame.line((x + 2, y - 2), (x - 2, y + 2), FLOOR_LIGHT);
        }
        // Raised perimeter stones, with their vertical face below the walkable plane.
        for col in 0..24 {
            let x = col * 16;
            frame.rect(x, 0, 15, 9, WALL);
            frame.rect(x + 1, 0, 13, 2, WALL_LIGHT);
            frame.rect(x, 9, 15, 3, DEPTH);
            frame.rect(x, 211, 15, 8, WALL);
            frame.rect(x, 210, 15, 2, FLOOR_LIGHT);
            frame.rect(x + 3, 214, 9, 2, DEPTH);
        }
        for y in (12..211).step_by(16) {
            frame.rect(0, y, 12, 15, WALL);
            frame.rect(2, y, 3, 14, WALL_LIGHT);
            frame.rect(373, y, 11, 15, WALL);
            frame.rect(374, y, 2, 14, WALL_LIGHT);
        }
        // Vegetation grows in clustered silhouettes at the perimeter, not noise
        // over the fighting area. Its shadows share the stone lighting direction.
        for ix in 0..140 {
            let seed = hash(ix, 71);
            let side = ix % 4;
            let distance = (seed % 19) as i32;
            let (x, y) = match side {
                0 => (distance, (seed / 23 % 224) as i32),
                1 => (383 - distance, (seed / 23 % 224) as i32),
                2 => ((seed / 23 % 384) as i32, distance / 2),
                _ => ((seed / 23 % 384) as i32, 224 - distance / 2),
            };
            frame.ellipse((x + 2, y + 2), (7, 3), MOSS_DARK);
            frame.ellipse((x, y), (6, 3), MOSS);
            frame.line((x - 2, y), (x - 4, y - 5), MOSS_LIGHT);
            frame.line((x, y), (x + 1, y - 7), GRASS);
            frame.line((x + 2, y), (x + 4, y - 4), MOSS_LIGHT);
        }
        for ix in 0..14 {
            let seed = hash(ix, 211);
            let x = 25 + (seed % 330) as i32;
            let y = 18 + (seed / 331 % 185) as i32;
            frame.rect(x, y, 3, 1, LEAF);
            frame.put(x + 1, y - 1, GOLD_DARK);
        }
        for (x, y) in [(20, 24), (356, 24), (20, 203), (356, 203)] {
            frame.ellipse((x + 4, y + 2), (10, 4), SHADOW);
            block(&mut frame, (x - 6, y - 16), (12, 18), 4);
            frame.rect(x - 4, y - 12, 2, 10, BODY_SHADOW);
            frame.rect(x + 2, y - 12, 2, 10, BODY_LIGHT);
            frame.rect(x - 8, y - 18, 16, 3, WALL_LIGHT);
            frame.rect(x - 7, y, 15, 3, WALL);
        }
        for row in 0..4 {
            frame.rect(167 - row * 3, 211 + row * 3, 50 + row * 6, 2, FLOOR_LIGHT);
            frame.rect(167 - row * 3, 213 + row * 3, 50 + row * 6, 1, SEAM);
        }
        frame
    })
}

fn logos() -> &'static [Raster; 6] {
    static LOGOS: OnceLock<[Raster; 6]> = OnceLock::new();
    LOGOS.get_or_init(|| Guardian::ALL.map(logo))
}

fn logo(guardian: Guardian) -> Raster {
    let tree = resvg::usvg::Tree::from_data(guardian.logo(), &resvg::usvg::Options::default())
        .expect("bundled guardian mark must be valid SVG");
    let mut pixels = resvg::tiny_skia::Pixmap::new(72, 72).expect("bounded sprite allocation");
    let size = tree.size();
    let scale = 64.0 / size.width().max(size.height());
    let transform = resvg::tiny_skia::Transform::from_row(
        scale,
        0.0,
        0.0,
        scale,
        (72.0 - size.width() * scale) * 0.5,
        (72.0 - size.height() * scale) * 0.5,
    );
    resvg::render(&tree, transform, &mut pixels.as_mut());
    let mut mask = Raster::new(72, 72);
    for y in 0..72 {
        for x in 0..72 {
            if pixels.data()[((y * 72 + x) * 4 + 3) as usize] >= 160 {
                mask.put(x, y, BODY);
            }
        }
    }
    let mut sprite = Raster::new(72, 72);
    for y in 0..72 {
        for x in 0..72 {
            if mask.get(x, y) == CLEAR {
                continue;
            }
            let color = if mask.get(x - 1, y) == CLEAR || mask.get(x, y - 1) == CLEAR {
                BODY_GLEAM
            } else if mask.get(x + 2, y + 2) == CLEAR {
                BODY_DARK
            } else if mask.get(x - 2, y - 2) == CLEAR {
                BODY_LIGHT
            } else if hash(x / 2, y / 2).is_multiple_of(37) {
                BODY_DARK
            } else {
                BODY
            };
            sprite.put(x, y, color);
        }
    }
    sprite
}

fn block(frame: &mut Raster, origin: (i32, i32), size: (i32, i32), depth: i32) {
    let (x, y) = origin;
    let (w, h) = size;
    frame.polygon(
        &[
            (x, y),
            (x + depth, y - depth),
            (x + w + depth, y - depth),
            (x + w, y),
        ],
        BODY_LIGHT,
    );
    frame.polygon(
        &[
            (x + w, y),
            (x + w + depth, y - depth),
            (x + w + depth, y + h - depth),
            (x + w, y + h),
        ],
        BODY_DARK,
    );
    frame.rect(x, y, w, h, BODY);
    frame.line((x, y), (x + w - 1, y), BODY_GLEAM);
    frame.line((x, y), (x, y + h - 1), BODY_LIGHT);
    frame.line((x + 1, y + h - 1), (x + w, y + h - 1), BODY_SHADOW);
}

fn stone_hand(frame: &mut Raster, center: (i32, i32), raised: f32) {
    let (x, ground) = center;
    let y = ground - raised.round() as i32;
    frame.ellipse((x + 3, ground + 7), (13, 4), SHADOW);
    block(frame, (x - 11, y - 11), (23, 20), 5);
    for finger in 0..4 {
        let fx = x - 9 + finger * 5;
        frame.rect(fx, y - 5, 4, 9, BODY_LIGHT);
        frame.rect(fx, y + 4, 4, 3, BODY_DARK);
        frame.line((fx, y - 6), (fx + 3, y - 6), BODY_GLEAM);
    }
    frame.rect(x - 4, y - 10, 7, 3, GOLD_DARK);
    frame.rect(x - 3, y - 10, 5, 1, GOLD);
}

fn joint(frame: &mut Raster, from: (i32, i32), to: (i32, i32), color: u8) {
    for ix in 0..7 {
        let t = ix as f32 / 6.0;
        let x = (from.0 as f32 + (to.0 - from.0) as f32 * t).round() as i32;
        let y = (from.1 as f32 + (to.1 - from.1) as f32 * t).round() as i32;
        frame.ellipse((x, y + 2), (4, 4), BODY_SHADOW);
        frame.ellipse((x, y), (3, 3), color);
        frame.put(x - 1, y - 2, BODY_LIGHT);
    }
}

fn volume(
    frame: &mut Raster,
    sprite: &Raster,
    center: (i32, i32),
    angle: f32,
    visibility: f32,
    yaw: f32,
) {
    // Extruded masks retain side planes as a sculpture turns. All resampling is
    // nearest-neighbor on the logical raster, including a rotating knot.
    let mut side = sprite.clone();
    for pixel in &mut side.pixels {
        if *pixel != CLEAR {
            *pixel = BODY_SHADOW;
        }
    }
    for depth in (1..=6).rev() {
        let dx = (depth as f32 * (0.6 + yaw * 0.4)).round() as i32;
        frame.transformed(&side, (center.0 + dx, center.1 + depth), angle, visibility);
    }
    frame.transformed(sprite, center, angle, visibility);
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Pose {
    pub lift: f32,
    pub sway: f32,
    pub wake: f32,
}

pub(super) fn pose(time: f32, wake: f32, reduced: bool) -> Pose {
    let time = time.rem_euclid(IDLE_PERIOD);
    let breathing = if reduced {
        0.0
    } else {
        (time * TAU / (IDLE_PERIOD * 0.5)).sin()
    };
    Pose {
        lift: breathing * 1.4 + smoothstep(wake) * 3.0,
        sway: if reduced {
            0.0
        } else {
            (time * TAU / IDLE_PERIOD).sin()
        },
        wake: smoothstep(wake),
    }
}

fn boss(
    frame: &mut Raster,
    boss: &Boss,
    time: f32,
    wake: f32,
    fallen: f32,
    reduced: bool,
) -> Option<(i32, i32)> {
    let pose = pose(time, wake, reduced);
    let ambient_time = if reduced { 0.0 } else { time };
    let ground = pixel(boss.position);
    let leap = if boss.airborne() {
        (boss.progress() * PI).sin() * 40.0
    } else {
        0.0
    };
    let collapse = smoothstep(fallen) * 24.0;
    let center = (
        ground.0,
        ground.1 - 23 - pose.lift.round() as i32 - leap.round() as i32 + collapse.round() as i32,
    );
    let visibility = 1.0 - smoothstep((fallen - 0.3) / 0.7);
    frame.ellipse(
        (ground.0 + 4, ground.1 + 7),
        (32 - (leap * 0.25) as i32, 9 - (leap * 0.07) as i32),
        SHADOW,
    );
    if visibility <= 0.0 {
        return None;
    }
    let mut sculpture = Raster::new(WIDTH, HEIGHT);
    match boss.guardian {
        Guardian::Claude => claude(&mut sculpture, boss, center, pose),
        Guardian::Codex => codex(&mut sculpture, boss, center, pose),
        Guardian::Pi => pi(&mut sculpture, boss, center, ambient_time, pose),
        Guardian::OpenCode => opencode(&mut sculpture, boss, center, pose),
        Guardian::DeepSeek => deepseek(&mut sculpture, boss, center, ambient_time, pose),
        Guardian::Copilot => copilot(&mut sculpture, boss, center, pose),
    }
    if visibility >= 1.0 {
        frame.blit(&sculpture, 0, 0);
    } else {
        for y in 0..HEIGHT as i32 {
            for x in 0..WIDTH as i32 {
                let color = sculpture.get(x, y);
                if color != CLEAR && dither(x, y, visibility) {
                    frame.put(x, y, color);
                }
            }
        }
    }
    if boss.exposed > 0.0 && fallen <= 0.0 {
        let core = pixel(boss.core);
        frame.ellipse(core, (9, 7), CORE_DARK);
        frame.ellipse(core, (6, 5), CORE);
        frame.polygon(
            &[
                (core.0, core.1 - 5),
                (core.0 + 4, core.1),
                (core.0, core.1 + 5),
                (core.0 - 4, core.1),
            ],
            CORE_LIGHT,
        );
        frame.rect(core.0 - 1, core.1 - 3, 2, 4, WHITE);
        frame.ring(core, (11, 9), CORE, true);
    }
    Some(center)
}

fn hand_pose(boss: &Boss, side: f32, center: (i32, i32)) -> ((i32, i32), f32) {
    let rest = Vec2::new(center.0 as f32 + side * 47.0, center.1 as f32 + 18.0);
    let active = match boss.attack {
        Attack::LeftFist => side < 0.0,
        Attack::RightFist => side > 0.0,
        Attack::Clap | Attack::Stomp => true,
        _ => false,
    };
    if !active {
        return ((rest.x.round() as i32, rest.y.round() as i32), 0.0);
    }
    let target = Vec2::new(
        boss.target.x * SCALE
            + if matches!(boss.attack, Attack::Clap | Attack::Stomp) {
                side * 11.0
            } else {
                0.0
            },
        boss.target.y * SCALE,
    );
    let (position, height) = match boss.state {
        BossState::Windup => {
            let t = smoothstep(boss.progress());
            (rest.lerp(target, t), t * 47.0)
        }
        BossState::Striking => (target, (1.0 - boss.progress().powi(3)) * 47.0),
        BossState::Recovery => {
            let t = smoothstep((boss.time - 0.13) / 0.55);
            (target.lerp(rest, t), (t * PI).sin() * 12.0)
        }
        _ => (rest, 0.0),
    };
    (
        (position.x.round() as i32, position.y.round() as i32),
        height,
    )
}

fn claude(frame: &mut Raster, boss: &Boss, center: (i32, i32), pose: Pose) {
    let (x, y) = center;
    // The four legs, square eyes and wide little arms are the mascot's silhouette.
    for (ix, offset) in [-28, -12, 12, 28].into_iter().enumerate() {
        let step = (pose.sway * if ix % 2 == 0 { 1.0 } else { -1.0 }).round() as i32;
        block(frame, (x + offset - 4, y + 13 + step), (9, 18), 5);
    }
    block(frame, (x - 32, y - 30), (64, 47), 7);
    frame.rect(x - 31, y - 29, 61, 3, BODY_LIGHT);
    frame.rect(x - 25, y + 12, 49, 3, BODY_DARK);
    for eye in [-18, 18] {
        let height = 3 + (pose.wake * 4.0).round() as i32;
        frame.rect(x + eye - 3, y - 20, 7, height, OUTLINE);
        if boss.state == BossState::Windup {
            frame.rect(x + eye - 2, y - 19, 2, 2, CORE_LIGHT);
        }
    }
    frame.rect(x - 3, y + 3, 7, 2, BODY_DARK);
    for side in [-1.0, 1.0] {
        let (hand, raised) = hand_pose(boss, side, center);
        let shoulder = (x + (side * 34.0) as i32, y);
        joint(frame, shoulder, (hand.0, hand.1 - raised as i32), BODY_DARK);
        stone_hand(frame, hand, raised);
    }
    for (dx, dy) in [(-28, -23), (24, 11), (-22, 9)] {
        frame.rect(x + dx, y + dy, 3, 2, BODY_DARK);
        frame.put(x + dx, y + dy - 1, BODY_GLEAM);
    }
}

fn codex(frame: &mut Raster, boss: &Boss, center: (i32, i32), pose: Pose) {
    for side in [-1, 1] {
        block(
            frame,
            (center.0 + side * 23 - 7, center.1 + 24),
            (15, 14),
            5,
        );
        joint(
            frame,
            (center.0 + side * 26, center.1 + 10),
            (center.0 + side * 42, center.1 + 27),
            BODY_DARK,
        );
        stone_hand(frame, (center.0 + side * 43, center.1 + 30), 0.0);
    }
    let angle = boss.spin + pose.sway * 0.025;
    volume(
        frame,
        &logos()[Guardian::Codex as usize],
        center,
        angle,
        1.0,
        boss.direction.x,
    );
    frame.ellipse(center, (7, 6), BODY_SHADOW);
    frame.rect(
        center.0 - 4,
        center.1 - 1,
        8,
        1 + (pose.wake * 2.0).round() as i32,
        GOLD,
    );
    for ix in 0..4 {
        let x = center.0 - 24 + ix * 14;
        frame.rect(x, center.1 - 24, 3, 4, MOSS);
        frame.line((x, center.1 - 23), (x - 2, center.1 - 15 + ix), MOSS_LIGHT);
    }
}

fn pi(frame: &mut Raster, boss: &Boss, center: (i32, i32), time: f32, pose: Pose) {
    let (x, y) = center;
    // P is a hollow mantle; the short i is the staff-bearing figure beside it.
    frame.polygon(
        &[
            (x - 27, y + 6),
            (x - 1, y),
            (x + 5, y + 34),
            (x - 32, y + 36),
        ],
        BODY_SHADOW,
    );
    for i in 0..4 {
        frame.line((x - 24 + i * 7, y + 5), (x - 28 + i * 9, y + 33), BODY_DARK);
    }
    volume(
        frame,
        &logos()[Guardian::Pi as usize],
        (x - 7, y - 1),
        pose.sway * 0.015,
        1.0,
        boss.direction.x,
    );
    block(frame, (x + 17, y - 10), (13, 12), 4);
    frame.rect(x + 19, y - 5, 3, 2, OUTLINE);
    frame.rect(x + 26, y - 5, 2, 2, OUTLINE);
    let cast = if boss.state == BossState::Windup {
        smoothstep(boss.progress())
    } else if boss.state == BossState::Striking {
        1.0 - boss.progress()
    } else {
        0.0
    };
    let tip = (x + 42 - (cast * 13.0) as i32, y - 31 - (cast * 14.0) as i32);
    let grip = (x + 36, y + 14);
    joint(frame, (x + 25, y + 8), grip, BODY_LIGHT);
    frame.line((grip.0 + 1, grip.1 + 23), (tip.0 + 1, tip.1), BODY_SHADOW);
    frame.line((grip.0, grip.1 + 23), tip, GOLD_DARK);
    frame.line((grip.0 - 1, grip.1 + 22), (tip.0 - 1, tip.1), GOLD);
    frame.ring(tip, (7, 9), GOLD, false);
    frame.ellipse(tip, (4, 6), if cast > 0.3 { CORE_LIGHT } else { CORE_DARK });
    frame.rect(
        tip.0 - 1,
        tip.1 - 3,
        2,
        3,
        if pose.wake > 0.5 { WHITE } else { GOLD },
    );
    if cast > 0.0 {
        let radius = 10 + (cast * 8.0) as i32;
        frame.ring(tip, (radius, radius), CORE, true);
        for ix in 0..3 {
            let a = time * 3.0 + ix as f32 * TAU / 3.0;
            frame.rect(
                tip.0 + (a.cos() * radius as f32) as i32,
                tip.1 + (a.sin() * radius as f32) as i32,
                2,
                2,
                WHITE,
            );
        }
    }
}

fn opencode(frame: &mut Raster, boss: &Boss, center: (i32, i32), pose: Pose) {
    let (x, y) = center;
    for side in [-1.0, 1.0] {
        block(frame, (x + (side * 19.0) as i32 - 6, y + 25), (14, 15), 5);
        let (hand, raised) = hand_pose(boss, side, center);
        joint(
            frame,
            (x + (side * 27.0) as i32, y + 5),
            (hand.0, hand.1 - raised as i32),
            BODY_DARK,
        );
        stone_hand(frame, hand, raised);
    }
    block(frame, (x - 29, y - 33), (58, 65), 7);
    frame.rect(x - 16, y - 20, 33, 42, OUTLINE);
    frame.rect(x - 14, y + 1, 29, 19, BODY_DARK);
    frame.rect(x - 14, y + 1, 29, 3, BODY_LIGHT);
    let eye = 1 + (pose.wake * 3.0).round() as i32;
    frame.rect(x - 10, y - 11, 5, eye, WATER_LIGHT);
    frame.rect(x + 6, y - 11, 5, eye, WATER_LIGHT);
    frame.rect(x - 8, y + 11, 16, 2, BODY_SHADOW);
    for ix in 0..4 {
        frame.rect(x - 22 + ix * 14, y + 27, 3, 1, BODY_GLEAM);
    }
}

fn deepseek(frame: &mut Raster, boss: &Boss, center: (i32, i32), time: f32, pose: Pose) {
    // Normalize before adding stagger offsets so repeat boundaries rasterize
    // identically instead of losing a pixel to floating-point truncation.
    let time = time.rem_euclid(IDLE_PERIOD);
    let ground = pixel(boss.position);
    frame.ellipse((ground.0, ground.1 + 4), (37, 10), WATER_DARK);
    for ix in 0..3 {
        let spread = ((time / (IDLE_PERIOD / 3.0) + ix as f32 / 3.0).fract() * 21.0) as i32;
        frame.ring(
            (ground.0, ground.1 + 4),
            (22 + spread, 4 + spread / 4),
            WATER,
            true,
        );
    }
    let angle = pose.sway * 0.045
        + if boss.state == BossState::Rushing {
            boss.direction.y * 0.18
        } else {
            0.0
        };
    volume(
        frame,
        &logos()[Guardian::DeepSeek as usize],
        center,
        angle,
        1.0,
        boss.direction.x,
    );
    let fin = (center.0 - 10, center.1 + 17);
    frame.polygon(
        &[
            fin,
            (fin.0 + 19, fin.1 - 2),
            (fin.0 + 4, fin.1 + 18),
            (fin.0 - 3, fin.1 + 11),
        ],
        BODY_SHADOW,
    );
    frame.polygon(
        &[fin, (fin.0 + 15, fin.1 - 1), (fin.0 + 3, fin.1 + 13)],
        BODY_LIGHT,
    );
    frame.rect(
        center.0 - 19,
        center.1 + 1,
        4,
        1 + (pose.wake * 3.0).round() as i32,
        OUTLINE,
    );
    frame.put(center.0 - 19, center.1 + 1, WHITE);
    frame.line(
        (center.0 - 29, center.1 + 12),
        (center.0 - 17, center.1 + 15),
        BODY_GLEAM,
    );
    for ix in 0..3 {
        let rise = ((time / (IDLE_PERIOD * 0.5) + ix as f32 / 3.0).fract() * 27.0) as i32;
        frame.ring(
            (center.0 - 10 + ix * 4, center.1 - 17 - rise),
            (2, 2),
            WATER_LIGHT,
            false,
        );
    }
}

fn copilot(frame: &mut Raster, boss: &Boss, center: (i32, i32), pose: Pose) {
    let (x, y) = center;
    let spread = if boss.state == BossState::Windup {
        smoothstep(boss.progress()) * 17.0
    } else {
        pose.sway * 3.0
    };
    for side in [-1, 1] {
        for feather in (0..4).rev() {
            let base = x + side * (24 + feather * 5);
            let tip = x + side * (49 + feather * 3);
            let lift = spread as i32 - feather * 3;
            frame.polygon(
                &[
                    (base, y + 2),
                    (tip, y - 11 - lift),
                    (tip + side * 3, y + 1 - lift),
                    (base, y + 22),
                ],
                BODY_SHADOW,
            );
            frame.polygon(
                &[
                    (base, y),
                    (tip, y - 13 - lift),
                    (tip, y - 6 - lift),
                    (base, y + 16),
                ],
                BODY_LIGHT,
            );
            frame.line((base, y), (tip, y - 13 - lift), BODY_GLEAM);
        }
        block(frame, (x + side * 14 - 5, y + 26), (10, 11), 4);
    }
    frame.ellipse((x, y + 3), (27, 24), BODY_SHADOW);
    volume(
        frame,
        &logos()[Guardian::Copilot as usize],
        center,
        pose.sway * 0.02,
        1.0,
        boss.direction.x,
    );
    for side in [-1, 1] {
        frame.ellipse((x + side * 14, y - 12), (9, 7), BODY_DARK);
        frame.line(
            (x + side * 14 - 5, y - 16),
            (x + side * 14 + 3, y - 16),
            BODY_LIGHT,
        );
        frame.rect(
            x + side * 8 - 2,
            y + 8,
            4,
            2 + (pose.wake * 6.0).round() as i32,
            WATER_LIGHT,
        );
    }
}

fn hero_sprite() -> &'static Raster {
    static HERO: OnceLock<Raster> = OnceLock::new();
    HERO.get_or_init(|| {
        const ROWS: &[&str] = &[
            "    hhhhh    ",
            "   hHHHhhh   ",
            "  hHHHHhhhh  ",
            "  hHhSSSShh  ",
            "   hsSoSoh   ",
            "   hhSSSSh   ",
            "    rSSSh    ",
            "   rrCClrr   ",
            "  rrCCllCrr  ",
            "  SrCClCCrS  ",
            "  SrrCCCrrS  ",
            "   rrbbbrr   ",
            "   rClllCr   ",
            "    CC CC    ",
            "    bb bb    ",
            "   hhb bhh   ",
        ];
        let mut sprite = Raster::new(13, 16);
        for (y, row) in ROWS.iter().enumerate() {
            for (x, ch) in row.bytes().enumerate() {
                let color = match ch {
                    b'h' => HAIR,
                    b'H' => HAIR_LIGHT,
                    b'S' => SKIN,
                    b's' => SKIN_DARK,
                    b'o' => OUTLINE,
                    b'r' => CAPE,
                    b'C' => CLOTH,
                    b'l' => CLOTH_LIGHT,
                    b'b' => CLOTH_DARK,
                    _ => CLEAR,
                };
                sprite.put(x as i32, y as i32, color);
            }
        }
        sprite
    })
}

fn hero(frame: &mut Raster, arena: &Arena, reduced: bool) {
    let player = &arena.player;
    let (x, y) = pixel(player.position);
    let spawn = if arena.phase == Phase::Awakening {
        smoothstep((arena.phase_time - 0.18) / 0.65)
    } else {
        1.0
    };
    if spawn <= 0.0 {
        return;
    }
    frame.ellipse((x + 1, y + 2), (7, 3), SHADOW);
    let dying = arena.phase == Phase::Defeat;
    let angle = if dying {
        smoothstep(arena.phase_time / 0.4) * PI * 0.5
    } else if player.roll_remaining > 0.0 {
        (1.0 - player.roll_remaining / 0.26) * TAU
    } else {
        0.0
    };
    let stride = if player.moving && !reduced {
        (player.stride * PI).sin().round() as i32
    } else {
        0
    };
    let mut sprite = hero_sprite().clone();
    if player.facing.y < -0.35 && !dying {
        sprite.rect(4, 3, 6, 3, HAIR_LIGHT);
        sprite.rect(4, 7, 5, 6, CAPE);
        sprite.rect(5, 8, 2, 4, CAPE_LIGHT);
    }
    frame.transformed(&sprite, (x, y - 7 + stride), angle, spawn);
    if !dying && player.roll_remaining <= 0.0 {
        let aim = player.facing;
        let side = Vec2::new(-aim.y, aim.x);
        let hand = player
            .position
            .scale(SCALE)
            .plus(aim.scale(7.0))
            .plus(Vec2::new(0.0, -6.0));
        let start = hand.plus(side.scale(5.0));
        let middle = hand.plus(aim.scale(3.0));
        let end = hand.minus(side.scale(5.0));
        frame.line(
            (start.x as i32, start.y as i32),
            (middle.x as i32, middle.y as i32),
            GOLD,
        );
        frame.line(
            (middle.x as i32, middle.y as i32),
            (end.x as i32, end.y as i32),
            GOLD_DARK,
        );
        frame.line(
            (start.x as i32, start.y as i32),
            (end.x as i32, end.y as i32),
            IVORY,
        );
        if player.charge > 0.0 {
            let tip = hand.plus(aim.scale(8.0));
            frame.line(
                (hand.x as i32, hand.y as i32),
                (tip.x as i32, tip.y as i32),
                WHITE,
            );
            let ready = player.charge >= MIN_CHARGE;
            frame.ring(
                (x, y - 7),
                (10, 10),
                if ready { IVORY } else { GOLD_DARK },
                true,
            );
            if ready {
                frame.rect(tip.x as i32 - 1, tip.y as i32 - 1, 3, 3, WHITE);
            }
        }
    }
}

fn telegraph(frame: &mut Raster, boss: &Boss) {
    if boss.state != BossState::Windup {
        return;
    }
    let locked = boss.progress() >= 0.60;
    let color = if locked { WARN } else { WARN_DARK };
    let target = pixel(boss.target);
    let origin = pixel(boss.position);
    match boss.attack {
        Attack::Cross => {
            for offset in [-6, 6] {
                frame.line((12, target.1 + offset), (371, target.1 + offset), color);
                frame.line((target.0 + offset, 12), (target.0 + offset, 211), color);
            }
        }
        Attack::Sweep | Attack::Rush | Attack::Dash => {
            let origin = boss.attack_origin();
            let side = Vec2::new(-boss.direction.y, boss.direction.x).scale(
                if boss.attack == Attack::Sweep {
                    2.0
                } else {
                    boss.radius()
                },
            );
            for side in [side, side.scale(-1.0)] {
                frame.line(
                    pixel(origin.plus(side)),
                    pixel(origin.plus(boss.direction.scale(100.0)).plus(side)),
                    color,
                );
            }
        }
        Attack::Volley => {
            frame.ring(target, (12, 12), color, !locked);
            frame.line((target.0 - 17, target.1), (target.0 + 17, target.1), color);
            frame.line((target.0, target.1 - 17), (target.0, target.1 + 17), color);
        }
        Attack::Pulse => frame.ring(origin, (28, 28), color, !locked),
        _ => {
            let radius = (boss.attack.impact_radius() * SCALE) as i32;
            frame.ring(target, (radius, radius), color, !locked);
            let closing = radius + ((1.0 - boss.progress()) * 13.0) as i32;
            frame.ring(target, (closing, closing), WARN_DARK, true);
            if locked {
                frame.rect(target.0 - 1, target.1 - 1, 3, 3, WARN);
            }
        }
    }
}

fn effects(frame: &mut Raster, arena: &Arena, reduced: bool) {
    for effect in &arena.effects {
        let t = 1.0 - effect.remaining / effect.duration;
        let center = pixel(effect.position);
        let radius = effect.radius * SCALE;
        match effect.kind {
            EffectKind::Cross => {
                let w = ((1.0 - t) * 6.0).ceil() as i32;
                frame.rect(12, center.1 - w, 360, w * 2, CORE);
                frame.rect(center.0 - w, 12, w * 2, 200, CORE);
                frame.rect(12, center.1 - 1, 360, 2, WHITE);
                frame.rect(center.0 - 1, 12, 2, 200, WHITE);
            }
            EffectKind::Sweep => {
                let side = Vec2::new(-effect.direction.y, effect.direction.x);
                let width = ((1.0 - t) * radius).ceil() as i32;
                for offset in -width..=width {
                    let origin = effect.position.plus(side.scale(offset as f32 / SCALE));
                    frame.line(
                        pixel(origin),
                        pixel(origin.plus(effect.direction.scale(120.0))),
                        if offset.abs() <= width / 2 {
                            WHITE
                        } else {
                            FIRE
                        },
                    );
                }
            }
            EffectKind::Impact => {
                let r = (radius * (0.5 + t * 0.6)) as i32;
                frame.ring(
                    center,
                    (r, r / 2),
                    if t < 0.25 { IVORY } else { FLOOR_SHADE },
                    true,
                );
                for ix in 0..10 {
                    let a = ix as f32 * TAU / 10.0 + 0.4;
                    let p = (
                        center.0 + (a.cos() * radius * 0.65) as i32,
                        center.1 + (a.sin() * radius * 0.4) as i32,
                    );
                    frame.line(center, p, FLOOR_SHADE);
                }
            }
            EffectKind::Roll if !reduced => {
                for ix in 0..4 {
                    frame.rect(
                        center.0 - 5 + ix * 3,
                        center.1 - (t * 4.0) as i32,
                        2,
                        2,
                        DUST,
                    );
                }
            }
            _ => {}
        }
        if reduced
            || matches!(
                effect.kind,
                EffectKind::Cross | EffectKind::Sweep | EffectKind::Roll
            )
        {
            continue;
        }
        let count = if effect.kind == EffectKind::Victory {
            24
        } else {
            10
        };
        for ix in 0..count {
            let a = TAU * ix as f32 / count as f32 + 0.21;
            let travel = radius * (0.25 + t * 1.5);
            let z = (t * PI).sin()
                * if effect.kind == EffectKind::Victory {
                    35.0
                } else {
                    12.0
                };
            let x = center.0 + (a.cos() * travel) as i32;
            let y = center.1 + (a.sin() * travel * 0.65 - z) as i32;
            let color = match effect.kind {
                EffectKind::Victory | EffectKind::Catch => {
                    if ix % 2 == 0 {
                        WHITE
                    } else {
                        CORE_LIGHT
                    }
                }
                EffectKind::Defeat => CAPE,
                EffectKind::Armor | EffectKind::Shot => GOLD,
                _ => {
                    if ix % 3 == 0 {
                        FLOOR_LIGHT
                    } else {
                        DUST
                    }
                }
            };
            frame.rect(x, y, if t < 0.4 { 3 } else { 2 }, 2, color);
        }
    }
}

pub(super) fn preview(guardian: Guardian, time: f32, reduced: bool) -> Raster {
    let mut frame = Raster::new(WIDTH, HEIGHT);
    let _ = boss(&mut frame, &Boss::new(guardian), time, 0.0, 0.0, reduced);
    frame
}

pub(super) fn battle(arena: &Arena, reduced: bool) -> Raster {
    // Ground is composited separately so entering from the vignette reveals the
    // arena without replacing the guardian's already-visible idle silhouette.
    let mut frame = Raster::new(WIDTH, HEIGHT);
    telegraph(&mut frame, &arena.boss);
    for wave in &arena.waves {
        let center = pixel(wave.position);
        let radius = (wave.radius * SCALE) as i32;
        frame.ring(center, (radius, radius), WARN_DARK, false);
        frame.ring(center, (radius - 1, radius - 1), WARN, false);
        frame.ring(center, (radius - 3, radius - 3), IVORY, true);
    }
    let wake = if arena.phase == Phase::Awakening {
        arena.phase_time / INTRO_DURATION
    } else {
        1.0
    };
    let fallen = if arena.phase == Phase::Victory {
        arena.phase_time / 1.25
    } else {
        0.0
    };
    // Ground-plane depth order keeps the archer behind a body when walking north
    // of it. Raised hands and flying debris are painted on their own depth layer.
    if arena.player.position.y < arena.boss.position.y {
        hero(&mut frame, arena, reduced);
    }
    let center = boss(
        &mut frame,
        &arena.boss,
        arena.visual_time,
        wake,
        fallen,
        reduced,
    );
    if arena.player.position.y >= arena.boss.position.y {
        hero(&mut frame, arena, reduced);
        if let Some(center) = center
            && fallen <= 0.0
            && matches!(arena.boss.guardian, Guardian::Claude | Guardian::OpenCode)
        {
            // A reaching fist can be in front of the archer even while its body
            // is behind them. Sort that separate ground anchor after the hero.
            for side in [-1.0, 1.0] {
                let (hand, raised) = hand_pose(&arena.boss, side, center);
                if hand.1 as f32 > arena.player.position.y * SCALE {
                    stone_hand(&mut frame, hand, raised);
                }
            }
        }
    }
    for projectile in &arena.projectiles {
        let (x, y) = pixel(projectile.position);
        let tail = pixel(
            projectile
                .position
                .minus(projectile.velocity.normalized().scale(2.5)),
        );
        frame.line(tail, (x, y), CORE_DARK);
        frame.ellipse((x, y), (3, 2), FIRE);
        frame.rect(x - 1, y - 1, 2, 2, WHITE);
    }
    if arena.arrow.state != ArrowState::Ready {
        let point = pixel(arena.arrow.position);
        let direction = arena.arrow.velocity.normalized();
        let tail = pixel(arena.arrow.position.minus(direction.scale(2.2)));
        if arena.arrow.state == ArrowState::Returning {
            frame.line(pixel(arena.player.position), point, WATER);
        }
        frame.line(tail, point, IVORY);
        frame.rect(point.0 - 1, point.1 - 1, 3, 3, WHITE);
        let side = Vec2::new(-direction.y, direction.x);
        frame.line(
            pixel(
                arena
                    .arrow
                    .position
                    .minus(direction.scale(1.0))
                    .plus(side.scale(0.7)),
            ),
            point,
            GOLD,
        );
        frame.line(
            pixel(
                arena
                    .arrow
                    .position
                    .minus(direction.scale(1.0))
                    .minus(side.scale(0.7)),
            ),
            point,
            GOLD,
        );
        if arena.arrow.state == ArrowState::Lodged {
            frame.ring(point, (6, 4), GOLD, true);
        }
    }
    effects(&mut frame, arena, reduced);
    frame
}

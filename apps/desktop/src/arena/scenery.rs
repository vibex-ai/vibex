//! Six authored sanctuaries. Terrain silhouettes and column anchors come from
//! the collision map; props are painted in the actors' ground-depth order.

use super::{
    art::{HEIGHT, SCALE, TOP_PAD, WIDTH, pixel},
    geometry::Vec2,
    guardian::Guardian,
    map::{
        BLOCK_RADIUS, BRAZIER, CENTER, GATE, GATE_RADIUS, ISLANDS, Map, PLATES, Pillar, REACH,
        STATIONS, ScarKind, THRONE,
    },
    raster::{Raster, hash, ink::*},
};
use std::{
    f32::consts::{PI, TAU},
    sync::OnceLock,
};

pub(super) fn floor(guardian: Guardian) -> &'static Raster {
    static FLOORS: OnceLock<[Raster; Guardian::ALL.len()]> = OnceLock::new();
    &FLOORS.get_or_init(|| Guardian::ALL.map(build))[guardian as usize]
}

/// Mutable terrain is painted over the cached architecture. The collision map
/// owns destruction, so a submerged platform cannot keep looking walkable.
pub(super) fn damage(frame: &mut Raster, map: &Map) {
    if map.guardian == Guardian::DeepSeek {
        for (ix, (position, radius)) in ISLANDS.into_iter().enumerate() {
            if map.sunken & (1 << ix) == 0 {
                continue;
            }
            let c = pixel(position);
            let r = (radius * SCALE) as i32;
            frame.ellipse((c.0 + 1, c.1), (r + 9, r * 2 / 3 + 10), WATER_DARK);
            frame.ring(c, (r + 3, r * 2 / 3 + 3), WATER, true);
            for n in 0..7 {
                let a = n as f32 * TAU / 7.0;
                let x = c.0 + (a.cos() * r as f32 * 0.75) as i32;
                let y = c.1 + (a.sin() * r as f32 * 0.5) as i32;
                frame.polygon(&[(x - 2, y), (x + 1, y - 2), (x + 4, y), (x, y + 2)], WATER);
                frame.line((x - 3, y + 3), (x + 5, y + 3), WATER_LIGHT);
            }
        }
    }
    for scar in &map.scars {
        let c = pixel(scar.position);
        let radius = scar.radius * SCALE;
        if scar.kind == ScarKind::Scorch {
            frame.ellipse(
                c,
                ((radius * 0.85) as i32, (radius * 0.52) as i32),
                FLOOR_SHADE,
            );
            frame.ring(
                c,
                ((radius * 0.8) as i32, (radius * 0.48) as i32),
                SEAM,
                true,
            );
        }
        for n in 0..7 {
            let seed = hash(n, c.0 + c.1 * 7);
            let a = n as f32 * TAU / 7.0 + (seed % 7) as f32 * 0.04;
            let length = radius * (0.55 + (seed % 13) as f32 * 0.035);
            let mid = (
                c.0 + (a.cos() * length * 0.4) as i32 + 2,
                c.1 + (a.sin() * length * 0.26) as i32,
            );
            let end = (
                c.0 + (a.cos() * length) as i32,
                c.1 + (a.sin() * length * 0.62) as i32,
            );
            frame.line(c, mid, SEAM);
            frame.line(mid, end, SEAM);
            if scar.kind == ScarKind::Crack {
                frame.line((mid.0, mid.1 + 1), (end.0, end.1 + 1), FLOOR_LIGHT);
                frame.line(mid, (mid.0 - 3, mid.1 + 4), FLOOR_SHADE);
                frame.rect(end.0 + 2, end.1, 3, 2, WALL);
                frame.put(end.0 + 2, end.1, FLOOR_LIGHT);
            }
        }
    }
}

fn build(guardian: Guardian) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    frame.rect(0, -TOP_PAD, WIDTH as i32, HEIGHT as i32, FLOOR);
    let map = Map::new(guardian);
    match guardian {
        Guardian::Claude => garden(&mut frame),
        Guardian::Codex => court(&mut frame),
        Guardian::Pi => observatory(&mut frame),
        Guardian::OpenCode => foundry(&mut frame),
        Guardian::DeepSeek => lagoon(&mut frame),
        Guardian::Copilot => terrace(&mut frame),
        Guardian::Cursor => grid_hall(&mut frame),
        Guardian::Cline => server_room(&mut frame),
        Guardian::Grok => observatory_floor(&mut frame),
        Guardian::ZCode => gate_hall(&mut frame),
        Guardian::Kimi => page(&mut frame),
        Guardian::CodeBuddy => playroom(&mut frame),
        Guardian::Antigravity => plaza(&mut frame),
        Guardian::Devin => greenhouse(&mut frame),
        Guardian::Gemini => constellations(&mut frame),
    }
    // Keep the visible rim identical to the movement boundary. The front edge
    // is extruded downward and the camera can reveal its actual vertical face.
    for y in -TOP_PAD..HEIGHT as i32 - TOP_PAD {
        for x in 0..WIDTH as i32 {
            let p = Vec2::new(x as f32 / SCALE, y as f32 / SCALE);
            if !map.inside(p, 0.0) {
                let above = Vec2::new(p.x, p.y - 3.5);
                let color = if map.inside(above, 0.0) {
                    if y % 7 == 0 || (x + (y / 7 % 2) * 12) % 29 == 0 {
                        SEAM
                    } else {
                        WALL
                    }
                } else {
                    DEPTH
                };
                frame.put(x, y, color);
            } else if !map.inside(p, 0.65) {
                frame.put(x, y, WALL_LIGHT);
            } else if !map.inside(p, 1.25) {
                frame.put(x, y, SEAM);
            }
        }
    }
    // Bounded clusters keep texture intentional instead of per-pixel noise.
    for n in 0..150 {
        let h = hash(n, guardian as i32 + 211);
        let p = Vec2::new(
            8.0 + (h % 640) as f32 / 4.0,
            7.0 + ((h >> 12) % 408) as f32 / 4.0,
        );
        if !map.inside(p, 2.0) || map.inside(p, 9.0) {
            continue;
        }
        let (x, y) = pixel(p);
        if matches!(guardian, Guardian::Claude | Guardian::Codex) {
            tuft(&mut frame, x, y, (h % 5 + 2) as i32);
        } else if guardian == Guardian::Copilot {
            frame.rect(x, y, 5, 2, FLOOR_SHADE);
            frame.line((x, y), (x + 3, y - 2), FLOOR_LIGHT);
        }
    }
    frame
}

/// The constellation of the twins, charted on Gemini's terrace: two
/// figures side by side, each star an offset from the middle.
const GEMINI_STARS: [(f32, f32); 12] = [
    (-30.0, -28.0),
    (-34.0, -10.0),
    (-38.0, 8.0),
    (-44.0, 26.0),
    (-22.0, 10.0),
    (-26.0, 28.0),
    (10.0, -30.0),
    (14.0, -12.0),
    (18.0, 6.0),
    (12.0, 26.0),
    (34.0, 8.0),
    (40.0, 26.0),
];
/// The lines that join the twins' stars.
const GEMINI_LINES: [(usize, usize); 11] = [
    (0, 1),
    (1, 2),
    (2, 3),
    (1, 4),
    (4, 5),
    (0, 6),
    (6, 7),
    (7, 8),
    (8, 9),
    (7, 10),
    (10, 11),
];
/// Gemini's terrace: dusk-blue flags with the constellation of the twins
/// charted on them in faint lines and bright stars.
fn constellations(frame: &mut Raster) {
    tiles(frame, 44, 30, true);
    for (a, b) in GEMINI_LINES {
        let (from, to) = (GEMINI_STARS[a], GEMINI_STARS[b]);
        frame.line(
            pixel(CENTER.plus(Vec2::new(from.0, from.1))),
            pixel(CENTER.plus(Vec2::new(to.0, to.1))),
            WALL_LIGHT,
        );
    }
    for (x, y) in GEMINI_STARS {
        let c = pixel(CENTER.plus(Vec2::new(x, y)));
        frame.ellipse(c, (3, 3), WALL_LIGHT);
        frame.rect(c.0 - 1, c.1 - 1, 2, 2, WHITE);
    }
}
/// Devin's greenhouse: slate flags with moss in the joints, and a round bed
/// of dark soil in the middle where the flower is rooted, edged in stone.
fn greenhouse(frame: &mut Raster) {
    tiles(frame, 40, 28, true);
    let c = pixel(CENTER);
    frame.ellipse(c, (120, 74), WALL);
    frame.ellipse(c, (114, 70), DEPTH);
    for n in 0..90 {
        let h = hash(n, 109);
        let a = (h % 628) as f32 * 0.01;
        let r = (h >> 10) % 100;
        let p = (
            c.0 + (a.cos() * r as f32) as i32,
            c.1 + (a.sin() * r as f32 * 0.6) as i32,
        );
        frame.rect(p.0, p.1, 2, 1, FLOOR_SHADE);
    }
    for n in 0..40 {
        let h = hash(n, 127);
        let x = (h % WIDTH as u32) as i32;
        let y = ((h >> 10) % 460) as i32 - TOP_PAD / 2;
        frame.polygon(&[(x, y), (x + 5, y - 3), (x + 9, y)], MOSS);
        frame.line((x, y), (x + 9, y), MOSS_DARK);
    }
}
/// Antigravity's plaza: broad pale slabs, and a ring inlaid round the
/// middle in the logo's four colours.
fn plaza(frame: &mut Raster) {
    tiles(frame, 56, 34, false);
    let c = pixel(CENTER);
    for (ix, color) in [MOSS, GOLD, CORE, WATER].into_iter().enumerate() {
        let start = ix as f32 * TAU / 4.0 - PI * 0.75;
        for n in 0..40 {
            let a = start + n as f32 * TAU / 160.0;
            for r in [150.0f32, 154.0, 158.0] {
                let p = (
                    c.0 + (a.cos() * r) as i32,
                    c.1 + (a.sin() * r * 0.62) as i32,
                );
                frame.rect(p.0, p.1, 2, 2, color);
            }
        }
    }
    frame.ring(c, (146, 90), SEAM, false);
    frame.ring(c, (162, 100), SEAM, false);
}
/// CodeBuddy's playroom: a round violet rug in rings, paw prints wandering
/// across it, and a ball of yarn's trail looping round the middle.
fn playroom(frame: &mut Raster) {
    let c = pixel(CENTER);
    for (n, r) in [250, 205, 160, 115, 70].into_iter().enumerate() {
        let color = if n % 2 == 0 { FLOOR_LIGHT } else { FLOOR };
        frame.ellipse(c, (r, r * 2 / 3), color);
        frame.ring(c, (r, r * 2 / 3), SEAM, true);
    }
    for n in 0..26 {
        let h = hash(n, 101);
        let a = n as f32 * 0.55 + (h % 7) as f32 * 0.05;
        let r = 60.0 + (n % 9) as f32 * 18.0;
        let (x, y) = (
            c.0 + (a.cos() * r) as i32,
            c.1 + (a.sin() * r * 0.66) as i32,
        );
        frame.ellipse((x, y), (3, 2), FLOOR_SHADE);
        for (dx, dy) in [(-3, -4), (0, -5), (3, -4)] {
            frame.rect(x + dx, y + dy, 2, 2, FLOOR_SHADE);
        }
    }
    for n in 0..60 {
        let a = n as f32 * TAU / 60.0;
        let wobble = (a * 5.0).sin() * 14.0;
        let p = (
            c.0 + (a.cos() * (92.0 + wobble)) as i32,
            c.1 + (a.sin() * (58.0 + wobble * 0.6)) as i32,
        );
        frame.rect(p.0, p.1, 2, 1, WALL_LIGHT);
    }
}
/// Kimi's page: ruled lines down a pale floor, a margin rule, and faint
/// token squares scattered along the lines like words.
fn page(frame: &mut Raster) {
    for y in (-TOP_PAD..HEIGHT as i32).step_by(26) {
        frame.line((0, y), (WIDTH as i32, y), FLOOR_SHADE);
    }
    let margin = pixel(CENTER).0 - 250;
    frame.line((margin, -TOP_PAD), (margin, HEIGHT as i32), WALL);
    frame.line((margin + 3, -TOP_PAD), (margin + 3, HEIGHT as i32), WALL);
    for n in 0..140 {
        let h = hash(n, 83);
        let row = (h % 18) as i32;
        let x = margin + 12 + ((h >> 8) % 560) as i32 / 10 * 10;
        let y = -TOP_PAD + 26 * row + 13;
        let w = 4 + (h >> 20) as i32 % 12;
        frame.rect(x, y - 3, w, 6, FLOOR_LIGHT);
        frame.rect(x, y + 3, w, 1, FLOOR_SHADE);
    }
}
/// ZCode's gate hall: broad sandstone flags, a stepped dais at the north
/// where the slab stands, and a Z cut into the floor down the middle.
fn gate_hall(frame: &mut Raster) {
    tiles(frame, 48, 30, true);
    let gate = pixel(GATE);
    for (grow, color) in [(14, WALL), (8, WALL_LIGHT), (0, FLOOR_LIGHT)] {
        frame.ellipse(
            gate,
            (
                GATE_RADIUS as i32 * 4 + grow,
                (GATE_RADIUS * 2.6) as i32 + grow / 2,
            ),
            color,
        );
    }
    frame.ring(
        gate,
        (GATE_RADIUS as i32 * 4 - 4, (GATE_RADIUS * 2.6) as i32 - 3),
        SEAM,
        false,
    );
    let c = pixel(CENTER.plus(Vec2::new(0.0, 14.0)));
    let (w, h) = (70, 40);
    for offset in -1..=1 {
        frame.line(
            (c.0 - w, c.1 - h + offset),
            (c.0 + w, c.1 - h + offset),
            SEAM,
        );
        frame.line(
            (c.0 + w + offset, c.1 - h),
            (c.0 - w + offset, c.1 + h),
            SEAM,
        );
        frame.line(
            (c.0 - w, c.1 + h + offset),
            (c.0 + w, c.1 + h + offset),
            SEAM,
        );
    }
}
/// Grok's observatory: polished black stone strewn with stars, orbits
/// engraved round the middle, and one long slash cut across it from lower
/// left to upper right.
fn observatory_floor(frame: &mut Raster) {
    for n in 0..260 {
        let h = hash(n, 71);
        let (x, y) = (
            (h % WIDTH as u32) as i32,
            ((h >> 11) % HEIGHT as u32) as i32 - TOP_PAD,
        );
        let color = match h % 7 {
            0 => WALL_LIGHT,
            1 | 2 => WALL,
            _ => FLOOR_LIGHT,
        };
        frame.put(x, y, color);
        if h.is_multiple_of(23) {
            frame.put(x + 1, y, color);
            frame.put(x, y + 1, color);
        }
    }
    let c = pixel(CENTER);
    for (r, color) in [
        (70, FLOOR_LIGHT),
        (132, SEAM),
        (136, FLOOR_LIGHT),
        (230, SEAM),
    ] {
        frame.ring(c, (r, r), color, r == 230);
    }
    for n in 0..12 {
        let a = n as f32 * TAU / 12.0;
        let tick = |r: f32| (c.0 + (a.cos() * r) as i32, c.1 + (a.sin() * r) as i32);
        frame.line(tick(126.0), tick(142.0), FLOOR_LIGHT);
    }
    // The slash, as in the logo.
    let (from, to) = ((c.0 - 210, c.1 + 150), (c.0 + 210, c.1 - 150));
    for offset in -2..=2 {
        frame.line(
            (from.0 + offset, from.1),
            (to.0 + offset, to.1),
            if offset == 0 { WALL_LIGHT } else { WALL },
        );
    }
}
/// Cline's server room: a raised floor of square panels, some vented, with
/// cable trays down both sides and a pad at each of the four stations it
/// glides between.
fn server_room(frame: &mut Raster) {
    const PANEL: i32 = 36;
    for row in -4..(HEIGHT as i32 / PANEL + 1) {
        for col in 0..(WIDTH as i32 / PANEL + 1) {
            let (x, y) = (col * PANEL, row * PANEL);
            let seed = hash(col, row + 7);
            frame.line((x, y), (x + PANEL - 1, y), SEAM);
            frame.line((x, y), (x, y + PANEL - 1), SEAM);
            frame.line((x + 1, y + 1), (x + PANEL - 2, y + 1), FLOOR_LIGHT);
            for (dx, dy) in [
                (3, 3),
                (PANEL - 4, 3),
                (3, PANEL - 4),
                (PANEL - 4, PANEL - 4),
            ] {
                frame.put(x + dx, y + dy, FLOOR_LIGHT);
            }
            if seed.is_multiple_of(3) {
                // A vented panel.
                for vy in (8..PANEL - 6).step_by(4) {
                    for vx in (8..PANEL - 6).step_by(4) {
                        frame.rect(x + vx, y + vy, 2, 2, FLOOR_SHADE);
                    }
                }
            }
        }
    }
    let c = pixel(CENTER);
    for side in [-1, 1] {
        let x = c.0 + side * 262;
        frame.rect(x - 9, -TOP_PAD, 18, HEIGHT as i32, DEPTH);
        for (n, color) in [WATER, CORE_DARK, WATER_DARK].into_iter().enumerate() {
            let cx = x - 5 + n as i32 * 5;
            frame.line((cx, -TOP_PAD), (cx, HEIGHT as i32), color);
        }
    }
    for station in STATIONS {
        let p = pixel(station);
        frame.ring(p, (30, 18), FLOOR_SHADE, false);
        frame.ring(p, (26, 15), SEAM, true);
        frame.rect(p.0 - 2, p.1 - 2, 5, 5, WALL);
    }
}
/// Cursor's hall: a pale grid laid on the diagonals the cube rolls along,
/// one cube side between lines, with major lines every fourth, pointer glyphs
/// etched where major lines cross, and a dashed selection round the middle.
fn grid_hall(frame: &mut Raster) {
    // One cube side between parallel lines, measured across them.
    let spacing = crate::arena::cube::SIDE * std::f32::consts::SQRT_2 * SCALE;
    let c = pixel(CENTER);
    for y in -TOP_PAD..HEIGHT as i32 - TOP_PAD {
        for x in 0..WIDTH as i32 {
            let u = ((x - c.0) + (y - c.1)) as f32 / spacing;
            let v = ((x - c.0) - (y - c.1)) as f32 / spacing;
            let (cu, cv) = (u.floor() as i32, v.floor() as i32);
            let on = |t: f32| t - t.floor() < 1.4 / spacing;
            let major = |n: i32| n.rem_euclid(4) == 0;
            if on(u) {
                frame.put(x, y, if major(cu) { FLOOR_SHADE } else { SEAM });
            } else if on(v) {
                frame.put(x, y, if major(cv) { FLOOR_SHADE } else { SEAM });
            } else if hash(cu, cv + 40).is_multiple_of(9) {
                frame.put(x, y, FLOOR_LIGHT);
            }
        }
    }
    for a in -6..=6 {
        for b in -6..=6 {
            if a % 4 != 0 || b % 4 != 0 {
                continue;
            }
            // Where two major lines cross, a small pointer.
            let (u, v) = (a as f32 * spacing, b as f32 * spacing);
            let tip = (
                c.0 + ((u + v) * 0.5) as i32 + 4,
                c.1 + ((u - v) * 0.5) as i32 + 4,
            );
            frame.polygon(
                &[tip, (tip.0, tip.1 + 11), (tip.0 + 8, tip.1 + 8)],
                FLOOR_SHADE,
            );
            frame.polygon(
                &[
                    (tip.0 + 1, tip.1 + 2),
                    (tip.0 + 1, tip.1 + 8),
                    (tip.0 + 5, tip.1 + 7),
                ],
                FLOOR_LIGHT,
            );
        }
    }
    let (w, h) = (150, 90);
    for (from, to) in [
        ((c.0 - w, c.1 - h), (c.0 + w, c.1 - h)),
        ((c.0 - w, c.1 + h), (c.0 + w, c.1 + h)),
        ((c.0 - w, c.1 - h), (c.0 - w, c.1 + h)),
        ((c.0 + w, c.1 - h), (c.0 + w, c.1 + h)),
    ] {
        let length = (to.0 - from.0).abs().max((to.1 - from.1).abs());
        for n in (0..length).step_by(12) {
            let at = |n: i32| {
                let t = n.min(length) as f32 / length as f32;
                (
                    from.0 + ((to.0 - from.0) as f32 * t) as i32,
                    from.1 + ((to.1 - from.1) as f32 * t) as i32,
                )
            };
            frame.line(at(n), at(n + 6), WALL);
        }
    }
    for (x, y) in [(-w, -h), (w, -h), (-w, h), (w, h)] {
        frame.rect(c.0 + x - 3, c.1 + y - 3, 7, 7, WALL);
        frame.rect(c.0 + x - 2, c.1 + y - 2, 5, 5, WALL_LIGHT);
    }
}
fn tiles(frame: &mut Raster, width: i32, height: i32, offset: bool) {
    for row in 0..(HEIGHT as i32 / height + 1) {
        for col in 0..(WIDTH as i32 / width + 2) {
            let x = col * width - if offset { row % 2 * width / 2 } else { 0 };
            let y = row * height;
            let seed = hash(col, row);
            frame.rect(
                x + 1,
                y + 1,
                width - 1,
                height - 1,
                if seed.is_multiple_of(7) {
                    FLOOR_LIGHT
                } else {
                    FLOOR
                },
            );
            frame.line((x, y), (x + width - 1, y), SEAM);
            frame.line((x, y), (x, y + height - 1), FLOOR_SHADE);
            if seed.is_multiple_of(4) {
                let cut = x + 6 + (seed % ((width - 9) as u32)) as i32;
                frame.line((cut, y + 1), (cut - 3, y + 6), FLOOR_SHADE);
                frame.line((cut - 3, y + 6), (cut + 2, y + 10), FLOOR_SHADE);
            }
        }
    }
}
fn garden(frame: &mut Raster) {
    tiles(frame, 42, 26, true);
    for n in 0..70 {
        let h = hash(n, 17);
        let x = 60 + (h % 580) as i32;
        let y = 55 + ((h >> 10) % 345) as i32;
        frame.ellipse((x, y), (5, 2), FLOOR_SHADE);
        frame.rect(x - 1, y - 2, 3, 1, LEAF);
    }
    let c = pixel(CENTER);
    for r in [73, 76, 113, 116] {
        frame.ring(c, (r, r * 3 / 4), SEAM, false);
    }
    for n in 0..8 {
        let a = n as f32 * TAU / 8.0;
        let p = (c.0 + (a.cos() * 94.0) as i32, c.1 + (a.sin() * 70.0) as i32);
        frame.polygon(
            &[
                (p.0 - 4, p.1),
                (p.0, p.1 - 5),
                (p.0 + 4, p.1),
                (p.0, p.1 + 5),
            ],
            FLOOR_SHADE,
        );
    }
    for side in [-1, 1] {
        for n in 0..7 {
            let x = c.0 + side * 248;
            let y = 105 + n * 34;
            frame.rect(x, y, 22, 23, MOSS_DARK);
            frame.rect(x + 2, y + 2, 18, 15, MOSS);
        }
    }
}
fn court(frame: &mut Raster) {
    tiles(frame, 33, 22, true);
    let c = pixel(CENTER);
    for r in [76, 81, 143, 149, 221, 225, 252, 255] {
        frame.ring(
            c,
            (r, r * 7 / 10),
            if r % 2 == 0 { FLOOR_LIGHT } else { SEAM },
            false,
        );
    }
    for n in 0..16 {
        let a = n as f32 * TAU / 16.0;
        let r1 = 150.0;
        let r2 = 220.0;
        frame.line(
            (
                c.0 + (a.cos() * r1) as i32,
                c.1 + (a.sin() * r1 * 0.7) as i32,
            ),
            (
                c.0 + (a.cos() * r2) as i32,
                c.1 + (a.sin() * r2 * 0.7) as i32,
            ),
            FLOOR_SHADE,
        );
    }
    for p in Map::new(Guardian::Codex).pillars() {
        let c = pixel(p.position);
        frame.ring(c, (29, 19), SEAM, false);
        frame.ring(c, (32, 21), FLOOR_LIGHT, false);
    }
    // Forge channels lead from every plate to the central brazier.
    for plate in PLATES {
        let from = pixel(plate);
        let to = pixel(BRAZIER);
        frame.line((from.0, from.1 + 1), (to.0, to.1 + 1), FLOOR_SHADE);
        frame.line(from, to, SEAM);
        let c = pixel(plate);
        frame.ellipse((c.0, c.1 + 2), (29, 28), FLOOR_SHADE);
        frame.ellipse(c, (26, 26), WALL);
        frame.ring(c, (26, 26), WALL_LIGHT, false);
    }
}
fn observatory(frame: &mut Raster) {
    tiles(frame, 36, 36, false);
    let c = pixel(CENTER);
    for r in [87, 90, 159, 163, 209, 213] {
        frame.ring(c, (r, r * 3 / 4), SEAM, false);
    }
    for n in 0..12 {
        let a = n as f32 * TAU / 12.0;
        let a2 = a + TAU * 5.0 / 12.0;
        frame.line(
            (
                c.0 + (a.cos() * 159.0) as i32,
                c.1 + (a.sin() * 119.0) as i32,
            ),
            (
                c.0 + (a2.cos() * 159.0) as i32,
                c.1 + (a2.sin() * 119.0) as i32,
            ),
            FLOOR_SHADE,
        );
    }
    // The glyph's raised dais and the arc its stems can reach.
    let t = pixel(THRONE);
    frame.ellipse((t.0, t.1 + 8), (62, 30), FLOOR_SHADE);
    frame.ellipse((t.0, t.1 + 4), (58, 27), WALL);
    frame.ellipse(t, (56, 25), FLOOR_LIGHT);
    frame.ring(t, (50, 22), GOLD_DARK, false);
    for n in 0..12 {
        let a = n as f32 * TAU / 12.0;
        let q = (t.0 + (a.cos() * 44.0) as i32, t.1 + (a.sin() * 18.0) as i32);
        rune(frame, q, n, GOLD_DARK);
    }
    let reach = (REACH * SCALE) as i32;
    for n in 0..48 {
        let a = PI * (0.06 + 0.88 * n as f32 / 47.0);
        let q = (
            t.0 + (a.cos() * reach as f32) as i32,
            t.1 + (a.sin() * reach as f32) as i32,
        );
        if n % 2 == 0 {
            frame.rect(q.0 - 2, q.1 - 1, 4, 2, GOLD_DARK);
        }
    }
    // The south gate: a sill beneath the arch beyond the stems' reach.
    let g = pixel(Vec2::new(CENTER.x, 101.0));
    frame.rect(g.0 - 34, g.1 - 6, 68, 10, WALL);
    frame.rect(g.0 - 30, g.1 - 4, 60, 6, FLOOR_LIGHT);
    for x in [-34, 26] {
        frame.rect(g.0 + x, g.1 - 16, 8, 20, WALL_LIGHT);
        frame.rect(g.0 + x + 2, g.1 - 16, 2, 20, FLOOR_LIGHT);
    }
}
fn foundry(frame: &mut Raster) {
    for row in 0..12 {
        for col in 0..18 {
            let x = col * 40;
            let y = row * 40;
            frame.rect(
                x,
                y,
                39,
                39,
                if (row + col) % 2 == 0 {
                    FLOOR
                } else {
                    FLOOR_SHADE
                },
            );
            frame.rect(x + 2, y + 2, 35, 1, FLOOR_LIGHT);
            frame.rect(x + 2, y + 2, 1, 35, FLOOR_LIGHT);
            frame.rect(x + 37, y + 2, 1, 35, SEAM);
        }
    }
    for x in [146, 155, 548, 557] {
        frame.rect(x, 42, 2, 375, SEAM);
    }
    for y in [110, 120, 342, 352] {
        frame.rect(60, y, 584, 2, SEAM);
    }
    for side in [-1, 1] {
        for n in 0..15 {
            let x = 352 + side * 267;
            let y = 59 + n * 23;
            frame.rect(x, y, 16, 12, WALL);
            frame.rect(x + 2, y + 3, 12, 1, OUTLINE);
            frame.rect(x + 2, y + 7, 12, 1, OUTLINE);
        }
    }
}
fn lagoon(frame: &mut Raster) {
    frame.rect(0, 0, WIDTH as i32, HEIGHT as i32, WATER_DARK);
    let c = pixel(CENTER);
    frame.ellipse(c, (287, 181), WATER);
    frame.ellipse((c.0 + 12, c.1 + 8), (254, 151), WATER_DARK);
    for n in 0..170 {
        let h = hash(n, 811);
        let x = (h % 704) as i32;
        let y = ((h >> 10) % 464) as i32;
        frame.line((x, y), (x + 7 + (h % 14) as i32, y), WATER);
    }
    for (point, radius) in ISLANDS {
        let (x, y) = pixel(point);
        let r = (radius * 4.0) as i32;
        // Ice floes: a submerged shelf, a cut edge and a pale crust.
        frame.ellipse((x + 3, y + 4), (r + 7, r * 2 / 3 + 5), WATER);
        frame.ellipse((x, y), (r, r * 2 / 3), WALL);
        frame.ellipse((x, y - 4), (r - 2, r * 2 / 3 - 2), FLOOR);
        frame.ring((x, y - 4), (r - 3, r * 2 / 3 - 3), FLOOR_LIGHT, false);
        for n in 0..5 {
            let h = hash(n, x);
            let a = n as f32 * TAU / 5.0 + (h % 9) as f32 * 0.1;
            let length = r as f32 * (0.35 + (h % 5) as f32 * 0.08);
            let from = (x + (a.cos() * 6.0) as i32, y - 4 + (a.sin() * 4.0) as i32);
            let to = (
                x + (a.cos() * length) as i32,
                y - 4 + (a.sin() * length * 0.6) as i32,
            );
            frame.line(from, to, SEAM);
        }
        for n in 0..6 {
            let h = hash(n, y);
            frame.rect(
                x - r / 2 + (h % (r as u32)) as i32,
                y - r / 3 + ((h >> 8) % (r as u32 / 2)) as i32,
                2,
                1,
                FLOOR_LIGHT,
            );
        }
    }
}
fn terrace(frame: &mut Raster) {
    tiles(frame, 52, 24, true);
    let c = pixel(CENTER);
    for side in [-1, 1] {
        for n in 0..5 {
            let x = c.0 + side * (26 + n * 17);
            let y = c.1 - 45;
            frame.line((x, y), (x + side * 35, y + 45), SEAM);
            frame.line((x + side * 35, y + 45), (x, y + 90), SEAM);
        }
    }
    for y in [65, 72, 380, 387] {
        frame.rect(90, y, 524, 2, FLOOR_LIGHT);
    }
    for n in 0..13 {
        let x = 96 + n * 42;
        for y in [78, 362] {
            frame.polygon(
                &[(x, y), (x + 6, y + 5), (x, y + 10), (x - 6, y + 5)],
                FLOOR_SHADE,
            );
        }
    }
    // Icicles fringe the eaves above the north rim.
    for n in 0..40 {
        let h = hash(n, 71);
        let x = 90 + n * 13 + (h % 5) as i32;
        let length = 6 + (h % 9) as i32;
        frame.polygon(&[(x - 3, 54), (x + 3, 54), (x, 54 + length)], WALL_LIGHT);
        frame.line((x - 1, 54), (x, 52 + length), FLOOR_LIGHT);
    }
}
fn tuft(frame: &mut Raster, x: i32, y: i32, size: i32) {
    frame.ellipse((x + 2, y + 1), (size + 3, 2), MOSS_DARK);
    frame.ellipse((x, y), (size + 1, 2), MOSS);
    frame.line((x, y), (x - 3, y - size - 1), MOSS_LIGHT);
    frame.line((x + 2, y), (x + 4, y - size - 3), GRASS);
}
pub(super) fn rune(frame: &mut Raster, p: (i32, i32), ix: i32, color: u8) {
    let (x, y) = p;
    frame.line((x - 2, y - 3), (x - 2, y + 3), color);
    frame.line((x - 2, y - 3), (x + 2, y - 1), color);
    frame.line((x + 2, y - 1), (x - 2, y + 1), color);
    if ix % 2 == 0 {
        frame.line((x - 1, y + 1), (x + 3, y + 3), color);
    }
}

pub(super) fn pillar(frame: &mut Raster, pillar: Pillar, broken: bool) {
    let (x, y) = pixel(pillar.position);
    let r = (pillar.radius * 4.0) as i32;
    let h = (pillar.height * 4.0) as i32;
    if broken {
        frame.ellipse((x + 2, y + 1), (r + 6, r / 2 + 4), FLOOR_SHADE);
        for n in 0..7 {
            let a = n as f32 * TAU / 7.0;
            let q = (
                x + (a.cos() * r as f32) as i32,
                y + (a.sin() * r as f32 * 0.6) as i32,
            );
            let width = 4 + n % 3;
            frame.polygon(
                &[
                    (q.0 - width, q.1),
                    (q.0 - width, q.1 - 4),
                    (q.0 + 1, q.1 - 7),
                    (q.0 + width, q.1 - 3),
                    (q.0 + width, q.1 + 2),
                ],
                WALL,
            );
            frame.polygon(
                &[
                    (q.0 - width, q.1 - 4),
                    (q.0 + 1, q.1 - 7),
                    (q.0 + width, q.1 - 3),
                    (q.0, q.1 - 1),
                ],
                WALL_LIGHT,
            );
            frame.line((q.0, q.1 - 1), (q.0, q.1 + 2), SEAM);
        }
        return;
    }
    frame.polygon(
        &[
            (x - r, y),
            (x + r, y),
            (x + r + h / 2, y + h / 4),
            (x + h / 2, y + h / 4 + 3),
        ],
        SHADOW,
    );
    frame.rect(x - r - 3, y - 5, r * 2 + 7, 7, WALL);
    frame.ellipse((x, y - 5), (r + 3, r / 2 + 1), FLOOR_LIGHT);
    frame.rect(x - r, y - h, r * 2, h, WALL);
    frame.rect(x - r + 1, y - h, 4, h, WALL_LIGHT);
    frame.rect(x - r + 5, y - h, 2, h, FLOOR_LIGHT);
    frame.rect(x + r - 5, y - h, 5, h, DEPTH);
    for dy in (8..h).step_by(11) {
        frame.line((x - r + 2, y - dy), (x + r - 1, y - dy), SEAM);
    }
    frame.ellipse((x, y - h), (r, r / 2), WALL_LIGHT);
    frame.ellipse((x, y - h - 1), (r - 3, r / 2 - 2), FLOOR_LIGHT);
    frame.line((x - r + 2, y - h + 2), (x, y - h + r / 2), DUST);
    frame.line((x + 1, y - h + 7), (x - 3, y - h + 17), DEPTH);
    frame.line((x - 3, y - h + 17), (x + 1, y - h + 26), DEPTH);
}

pub(super) fn ambient(frame: &mut Raster, map: &Map, time: f32, reduced: bool) {
    if reduced {
        return;
    }
    match map.guardian {
        Guardian::DeepSeek => {
            for (ix, (p, r)) in ISLANDS.iter().enumerate() {
                if map.sunken & (1 << ix) != 0 {
                    continue;
                }
                let c = pixel(*p);
                let spread = ((time * 0.35 + ix as f32 * 0.2).fract() * 8.0) as i32;
                frame.ring(
                    c,
                    ((r * 4.0) as i32 + spread, (r * 2.7) as i32 + spread / 2),
                    WATER_LIGHT,
                    true,
                );
            }
        }
        Guardian::Pi => {
            let c = pixel(THRONE);
            for n in 0..6 {
                let t = time * 0.4 + n as f32 * TAU / 6.0;
                frame.rect(
                    c.0 + (t.cos() * 50.0) as i32,
                    c.1 + (t.sin() * 22.0) as i32,
                    2,
                    1,
                    GOLD,
                );
            }
        }
        Guardian::Claude | Guardian::Codex => {
            for n in 0..8 {
                let h = hash(n, 63);
                let t = (time * 0.04 + n as f32 * 0.13).fract();
                let x = (h % 600) as f32 + 30.0 + t * 32.0;
                let y = (h / 601 % 400) as f32 + 10.0 + t * 20.0;
                frame.rect(x as i32, y as i32, 3, 1, LEAF);
            }
        }
        Guardian::Copilot => {
            for n in 0..8 {
                let h = hash(n, 91);
                let t = (time * 0.09 + n as f32 * 0.13).fract();
                let x = (t * WIDTH as f32) as i32;
                let y = (h % 400 + 30) as i32;
                frame.line((x, y), (x + 12, y - 3), FLOOR_LIGHT);
            }
        }
        Guardian::Cursor => {
            // Carets blink on the grid, each on its own beat.
            for n in 0..6 {
                let h = hash(n, 47);
                if (time * 1.6 + n as f32 * 0.37).fract() < 0.5 {
                    let x = 120 + (h % 460) as i32 / 32 * 32 + 12;
                    let y = 60 + ((h >> 9) % 320) as i32 / 32 * 32 + 8;
                    frame.rect(x, y, 2, 14, WALL);
                }
            }
        }
        Guardian::Cline => {
            // Status lights wink on the vented panels.
            for n in 0..10 {
                let h = hash(n, 29);
                if (time * 0.9 + (h % 13) as f32 * 0.17).fract() < 0.3 {
                    let x = 40 + (h % 620) as i32 / 36 * 36 + 18;
                    let y = 10 + ((h >> 9) % 400) as i32 / 36 * 36 + 18;
                    let color = if n % 3 == 0 { CORE } else { WATER_LIGHT };
                    frame.rect(x - 1, y - 1, 3, 2, color);
                }
            }
        }
        Guardian::Grok => {
            // Stars twinkle in the polished black.
            for n in 0..14 {
                let h = hash(n, 53);
                let beat = (time * 0.7 + (h % 17) as f32 * 0.11).fract();
                if beat < 0.25 {
                    let x = (h % WIDTH as u32) as i32;
                    let y = ((h >> 10) % 460) as i32;
                    frame.line((x - 2, y), (x + 2, y), WALL_LIGHT);
                    frame.line((x, y - 2), (x, y + 2), WALL_LIGHT);
                    frame.put(x, y, WHITE);
                }
            }
        }
        Guardian::ZCode => {
            // Sand drifts down the hall in the light from the gate.
            for n in 0..10 {
                let h = hash(n, 61);
                let t = (time * 0.07 + n as f32 * 0.1).fract();
                let x = (h % 600) as i32 + 50 + (t * 20.0) as i32;
                let y = (t * 420.0) as i32 + 10;
                frame.rect(x, y, 2, 1, WALL_LIGHT);
            }
        }
        Guardian::Kimi => {
            // Context being written: tokens light up along the lines.
            let margin = pixel(CENTER).0 - 250;
            for n in 0..6 {
                let h = hash(n, 89);
                let beat = (time * 0.8 + (h % 11) as f32 * 0.13).fract();
                let row = (h % 18) as i32;
                let x = margin + 12 + ((h >> 8) % 560) as i32 / 10 * 10;
                let y = -TOP_PAD + 26 * row + 10;
                let on_page = map.inside(Vec2::new(x as f32 / SCALE, y as f32 / SCALE), 2.0);
                if beat < 0.35 && on_page {
                    frame.rect(x, y, 8, 6, CORE);
                }
            }
        }
        Guardian::CodeBuddy => {
            // Teal motes drift up from the rug.
            for n in 0..9 {
                let h = hash(n, 97);
                let t = (time * 0.12 + n as f32 * 0.11).fract();
                let p = Vec2::new(30.0 + (h % 116) as f32, 100.0 - t * 80.0);
                if map.inside(p, 3.0) {
                    let (x, y) = pixel(p);
                    frame.rect(x, y, 2, 2, if n % 3 == 0 { CORE_LIGHT } else { CORE });
                }
            }
        }
        Guardian::Antigravity => {
            // Grit floats up off the plaza, weightless.
            for n in 0..12 {
                let h = hash(n, 107);
                let t = (time * 0.15 + n as f32 * 0.083).fract();
                let p = Vec2::new(24.0 + (h % 128) as f32, 104.0 - t * 96.0);
                if map.inside(p, 2.0) {
                    let (x, y) = pixel(p);
                    let color = [MOSS, GOLD, CORE, WATER][n as usize % 4];
                    frame.rect(x, y, 2, 2, color);
                }
            }
        }
        Guardian::Devin => {
            // Pollen drifts across the greenhouse.
            for n in 0..10 {
                let h = hash(n, 113);
                let t = (time * 0.05 + n as f32 * 0.1).fract();
                let p = Vec2::new(
                    20.0 + t * 136.0,
                    18.0 + (h % 80) as f32 + (t * 9.0).sin() * 3.0,
                );
                if map.inside(p, 2.0) {
                    let (x, y) = pixel(p);
                    frame.rect(x, y, 2, 1, CORE);
                }
            }
        }
        Guardian::Gemini => {
            // The charted stars twinkle by turns.
            for (ix, (x, y)) in GEMINI_STARS.iter().enumerate() {
                if (time * 0.6 + ix as f32 * 0.29).fract() < 0.2 {
                    let c = pixel(CENTER.plus(Vec2::new(*x, *y)));
                    frame.line((c.0 - 4, c.1), (c.0 + 4, c.1), WHITE);
                    frame.line((c.0, c.1 - 4), (c.0, c.1 + 4), WHITE);
                }
            }
        }
        Guardian::OpenCode => {}
    }
}

/// A spat code block: it stops the archer until a landing crushes it.
pub(super) fn block(frame: &mut Raster, position: Vec2) {
    let (x, y) = pixel(position);
    let r = (BLOCK_RADIUS * SCALE) as i32;
    frame.ellipse((x + 2, y + 2), (r + 2, r / 2 + 1), SHADOW);
    frame.rect(x - r - 1, y - r * 2 - 1, r * 2 + 2, r * 2 + 3, OUTLINE);
    frame.rect(x - r, y - r * 2, r * 2, r, BODY_LIGHT);
    frame.rect(x - r, y - r, r * 2, r + 1, BODY);
    frame.rect(x + r - 2, y - r, 2, r + 1, BODY_DARK);
    frame.line((x - 2, y - r * 2 + 2), (x - 4, y - r * 2 + 4), BODY_SHADOW);
    frame.line((x - 4, y - r * 2 + 4), (x - 2, y - r * 2 + 6), BODY_SHADOW);
    frame.line((x + 2, y - r * 2 + 2), (x + 4, y - r * 2 + 4), BODY_SHADOW);
    frame.line((x + 4, y - r * 2 + 4), (x + 2, y - r * 2 + 6), BODY_SHADOW);
}

/// The court's brazier bowl; it burns while the knot holds a plate down.
pub(super) fn brazier(frame: &mut Raster, lit: bool, time: f32, reduced: bool) {
    let (x, y) = pixel(BRAZIER);
    frame.ellipse((x + 2, y + 3), (17, 6), SHADOW);
    frame.ellipse((x, y), (15, 6), WALL);
    frame.rect(x - 15, y - 6, 30, 6, WALL);
    frame.ellipse((x, y - 6), (15, 5), WALL_LIGHT);
    frame.ellipse((x, y - 6), (11, 3), if lit { FIRE } else { DEPTH });
    for n in 0..5 {
        frame.rect(x - 13 + n * 6, y - 4, 2, 3, SEAM);
    }
    if !lit {
        return;
    }
    let tick = if reduced { 0 } else { (time * 18.0) as i32 };
    for n in 0..7 {
        let seed = hash(n, tick);
        let fx = x - 9 + n * 3;
        let height = 10 + (seed % 9) as i32 + if (2..5).contains(&n) { 8 } else { 0 };
        frame.polygon(
            &[(fx - 3, y - 6), (fx, y - 6 - height), (fx + 3, y - 6)],
            if n % 2 == 0 { FIRE } else { WARN },
        );
    }
    frame.polygon(&[(x - 4, y - 6), (x, y - 20), (x + 4, y - 6)], WHITE);
}

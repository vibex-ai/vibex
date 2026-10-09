//! Integer-pixel scenery, articulated guardians, and readable combat effects.

use super::{
    combat::{
        Arena, ArrowState, Attack, Boss, BossState, EffectKind, Guardian, HazardKind, MIN_CHARGE,
        Phase, ProjectileKind, Vec2, WaveKind, smoothstep,
    },
    raster::{Raster, dither, ink::*},
    scenery, sculpture,
};
use std::{
    f32::consts::{PI, TAU},
    sync::OnceLock,
};
pub(super) const SCALE: f32 = 4.0;
pub(super) const WIDTH: usize = 704;
pub(super) const TOP_PAD: i32 = 128;
pub(super) const HEIGHT: usize = 464 + TOP_PAD as usize;
pub(super) const IDLE_PERIOD: f32 = 36.0;
pub(super) fn pixel(p: Vec2) -> (i32, i32) {
    ((p.x * SCALE).round() as i32, (p.y * SCALE).round() as i32)
}
pub(super) fn prepare() {
    for guardian in Guardian::ALL {
        scenery::floor(guardian);
    }
    hero_sprite();
}
pub(super) fn floor(guardian: Guardian) -> &'static Raster {
    scenery::floor(guardian)
}

fn shadow(frame: &mut Raster, boss: &Boss) {
    let (x, y) = pixel(boss.position);
    let height = boss.height.max(0.0);
    let width = match boss.guardian {
        Guardian::DeepSeek => 38,
        Guardian::Copilot => 34,
        _ => 28,
    };
    let shrink = (height * 0.42).round() as i32;
    frame.ellipse((x + 3, y + 4), (width - shrink, 11 - shrink / 4), SHADOW);
    if boss.guardian == Guardian::Claude {
        for ix in 0..2 {
            let p = pixel(boss.hands[ix]);
            let r = 13 - (boss.hand_heights[ix] * 0.15) as i32;
            frame.ellipse((p.0 + 2, p.1 + 2), (r, 5), SHADOW);
        }
    }
}
fn guardian(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let visibility = 1.0 - smoothstep((fallen - 1.3) / 1.15);
    if visibility <= 0.0 {
        return;
    }
    let mut body = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    sculpture::draw(&mut body, boss, time, fallen, reduced);
    let submerged = boss.state == BossState::Submerged;
    if submerged {
        for color in &mut body.pixels {
            if *color != CLEAR {
                *color = WATER_DARK;
            }
        }
    }
    let opacity = visibility
        * if submerged {
            0.48
        } else if boss.state == BossState::Relocating {
            0.6
        } else {
            1.0
        };
    for (ix, color) in body.pixels.iter().enumerate() {
        if *color != CLEAR && dither((ix % WIDTH) as i32, (ix / WIDTH) as i32, opacity) {
            frame.pixels[ix] = *color;
        }
    }
    if boss.exposed > 0.0 && fallen <= 0.0 {
        weak_point(frame, boss, time, reduced);
    }
}

fn weak_point(frame: &mut Raster, boss: &Boss, time: f32, reduced: bool) {
    let (x, y) = pixel(boss.core);
    let beat = if reduced {
        0
    } else {
        (time * 8.0).sin().round() as i32
    };
    match boss.guardian {
        Guardian::Claude => {
            frame.rect(x - 7, y - 6, 15, 13, BODY_SHADOW);
            for side in [-1, 1] {
                frame.line((x, y), (x + side * 11, y - 4), CORE_DARK);
                frame.line((x, y), (x + side * 9, y + 7), CORE);
            }
            frame.polygon(
                &[(x, y - 6), (x + 5, y), (x, y + 6), (x - 5, y)],
                CORE_LIGHT,
            );
            frame.rect(x - 1, y - 2, 3, 4, WHITE);
        }
        Guardian::Codex => {
            for strand in 0..6 {
                let a = strand as f32 * TAU / 6.0;
                let p = (x + (a.cos() * 10.0) as i32, y + (a.sin() * 8.0) as i32);
                frame.line(p, (x, y), CORE_DARK);
                frame.ellipse(p, (3, 2), IVORY);
            }
            frame.ellipse((x, y), (5 + beat, 5 + beat), CORE_LIGHT);
            frame.rect(x - 1, y - 2, 2, 3, WHITE);
        }
        Guardian::Pi => {
            frame.ring((x, y), (10, 10), GOLD, true);
            scenery::rune(frame, (x, y), 2, CORE_LIGHT);
            for side in [-1, 1] {
                frame.line((x + side * 9, y), (x + side * 3, y), CORE_LIGHT);
                frame.line((x + side * 3, y), (x + side * 6, y - 3), WHITE);
            }
        }
        Guardian::OpenCode => {
            frame.rect(x - 8, y - 6, 17, 13, OUTLINE);
            frame.rect(x - 5, y - 4, 11, 9, FIRE);
            frame.rect(x - 3, y - 1, 7, 3, IVORY);
            for side in [-1, 1] {
                frame.rect(x + side * 10, y - 7, 2, 15, BODY_LIGHT);
            }
            frame.rect(x - 2, y - 1, 5, 2, WHITE);
        }
        Guardian::DeepSeek => {
            frame.ellipse((x, y), (11, 7), OUTLINE);
            frame.ellipse((x, y + 1), (7, 4 + beat), CORE);
            for tooth in [-6, -2, 2, 6] {
                frame.line((x + tooth, y - 5), (x + tooth, y - 2), IVORY);
            }
            frame.ellipse((x, y + 2), (3, 2), CORE_LIGHT);
        }
        Guardian::Copilot => {
            frame.ellipse((x, y), (10, 8), OUTLINE);
            frame.ellipse((x, y), (8, 6), WATER_LIGHT);
            frame.line((x - 6, y - 4), (x + 1, y + 1), WHITE);
            frame.line((x + 1, y + 1), (x - 2, y + 5), WHITE);
            frame.line((x + 1, y + 1), (x + 7, y + 3), WHITE);
            frame.rect(x - 2, y - 2, 5, 5, CORE_LIGHT);
        }
    }
}

#[cfg(test)]
pub(super) fn preview(guardian: Guardian, time: f32, reduced: bool) -> Raster {
    idle(&Boss::idle(guardian, time, reduced), time, reduced)
}
pub(super) fn idle(boss: &Boss, time: f32, reduced: bool) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    shadow(&mut frame, boss);
    if boss.guardian == Guardian::Claude {
        let mut order = [
            (boss.position.y, 0),
            (boss.hands[0].y, 1),
            (boss.hands[1].y, 2),
        ];
        order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
        for (_, part) in order {
            if part == 0 {
                guardian(&mut frame, boss, time, 0.0, reduced);
            } else {
                sculpture::hand(&mut frame, boss, part - 1, 0.0);
            }
        }
    } else {
        guardian(&mut frame, boss, time, 0.0, reduced);
    }
    frame
}

pub(super) fn battle(arena: &Arena, reduced: bool) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    let reveal = arena.ground_visibility();
    if reveal > 0.0 {
        scenery::ambient(&mut frame, &arena.map, arena.visual_time, reduced);
    }
    telegraph(&mut frame, &arena.boss);
    hazards(&mut frame, arena);
    for wave in &arena.waves {
        let c = pixel(wave.position);
        let radius = (wave.radius * SCALE) as i32;
        let (dark, light) = match wave.kind {
            WaveKind::Stone => (FLOOR_SHADE, DUST),
            WaveKind::Water => (WATER, WATER_LIGHT),
            WaveKind::Magic => (CORE_DARK, CORE_LIGHT),
        };
        frame.ring(c, (radius, radius), dark, false);
        frame.ring(c, (radius - 2, radius - 2), light, true);
        if wave.kind == WaveKind::Magic {
            for n in 0..12 {
                let a = n as f32 * TAU / 12.0;
                scenery::rune(
                    &mut frame,
                    (
                        c.0 + (a.cos() * radius as f32) as i32,
                        c.1 + (a.sin() * radius as f32) as i32,
                    ),
                    n,
                    light,
                );
            }
        }
    }
    shadow(&mut frame, &arena.boss);
    let fallen = if arena.phase == Phase::Victory {
        arena.phase_time
    } else {
        0.0
    };
    let mut order = vec![
        (arena.boss.position.y, 0usize),
        (arena.player.position.y, 1),
    ];
    if arena.boss.guardian == Guardian::Claude {
        for ix in 0..2 {
            order.push((arena.boss.hands[ix].y, 2 + ix));
        }
    }
    if reveal > 0.0 {
        for (ix, p) in arena.map.pillars().iter().enumerate() {
            order.push((p.position.y, 4 + ix));
        }
    }
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for (_, actor) in order {
        match actor {
            0 => guardian(&mut frame, &arena.boss, arena.visual_time, fallen, reduced),
            1 => hero(&mut frame, arena, reduced),
            2 | 3 => {
                if fallen < 2.1 {
                    sculpture::hand(&mut frame, &arena.boss, actor - 2, fallen);
                }
            }
            _ => scenery::pillar(
                &mut frame,
                arena.map.pillars()[actor - 4],
                !arena.map.intact(actor - 4),
            ),
        }
    }
    for p in &arena.projectiles {
        let c = pixel(p.position);
        let tail = pixel(p.position.minus(p.velocity.normalized().scale(3.0)));
        frame.line(
            tail,
            c,
            if p.kind == ProjectileKind::Feather {
                GOLD_DARK
            } else {
                CORE_DARK
            },
        );
        if p.kind == ProjectileKind::Feather {
            let side = p.velocity.normalized().perpendicular().scale(0.7);
            frame.polygon(
                &[
                    c,
                    pixel(
                        p.position
                            .minus(p.velocity.normalized().scale(2.6))
                            .plus(side),
                    ),
                    tail,
                    pixel(p.position.minus(side)),
                ],
                IVORY,
            );
        } else {
            frame.ring(c, (4, 4), CORE, false);
            frame.rect(c.0 - 1, c.1 - 1, 2, 2, WHITE);
        }
    }
    arrow(&mut frame, arena);
    effects(&mut frame, arena, reduced);
    frame
}

fn arrow(frame: &mut Raster, arena: &Arena) {
    if arena.arrow.state == ArrowState::Ready {
        return;
    }
    let p = pixel(arena.arrow.position);
    let d = arena.arrow.velocity.normalized();
    let tail = pixel(arena.arrow.position.minus(d.scale(2.6)));
    if arena.arrow.state == ArrowState::Returning {
        frame.line(pixel(arena.player.position), p, WATER);
    }
    frame.line((tail.0 + 1, tail.1 + 1), (p.0 + 1, p.1 + 1), OUTLINE);
    frame.line(tail, p, IVORY);
    frame.rect(p.0 - 1, p.1 - 1, 3, 3, WHITE);
    for side in [-1.0, 1.0] {
        frame.line(
            pixel(
                arena
                    .arrow
                    .position
                    .minus(d.scale(1.0))
                    .plus(d.perpendicular().scale(side * 0.75)),
            ),
            p,
            GOLD,
        );
    }
    if arena.arrow.state == ArrowState::Lodged {
        frame.ring(p, (6, 4), GOLD, true);
    }
}
fn telegraph(frame: &mut Raster, boss: &Boss) {
    if !matches!(boss.state, BossState::Windup | BossState::Striking) {
        return;
    }
    let locked = boss.state == BossState::Striking || boss.progress() >= 0.6;
    let color = if locked { WARN } else { WARN_DARK };
    let c = pixel(boss.target);
    let radius = (boss.impact_radius() * SCALE).ceil() as i32;
    match boss.guardian {
        Guardian::Claude => {
            for ix in 0..2 {
                if boss.attack == Attack::Clap || (ix == 0) == (boss.attack == Attack::LeftFist) {
                    let p = pixel(boss.hands[ix]);
                    frame.ring(p, (radius, radius), color, !locked);
                }
            }
        }
        Guardian::Codex => {
            if boss.attack == Attack::Leap {
                frame.ring(c, (radius, radius), color, !locked);
            } else {
                for n in 1..8 {
                    let p = boss
                        .position
                        .plus(boss.direction.scale(8.0 + n as f32 * 4.0));
                    let side = boss.direction.perpendicular().scale(1.1);
                    frame.line(pixel(p.minus(boss.direction).plus(side)), pixel(p), color);
                    frame.line(pixel(p.minus(boss.direction).minus(side)), pixel(p), color);
                }
            }
        }
        Guardian::Pi => {
            if boss.attack == Attack::Cross {
                for n in 0..8 {
                    let a = n as f32 * TAU / 8.0;
                    scenery::rune(
                        frame,
                        (c.0 + (a.cos() * 30.0) as i32, c.1 + (a.sin() * 30.0) as i32),
                        n,
                        color,
                    );
                }
            } else {
                let c = pixel(boss.position);
                frame.ring(c, (27, 19), color, true);
            }
        }
        Guardian::OpenCode => {
            if boss.attack == Attack::Sweep {
                frame.line(pixel(boss.attack_origin()), pixel(boss.beam_end), color);
            } else {
                frame.line(
                    (c.0 - radius, c.1 - radius),
                    (c.0 + radius, c.1 - radius),
                    color,
                );
                frame.line(
                    (c.0 + radius, c.1 - radius),
                    (c.0 + radius, c.1 + radius),
                    color,
                );
                frame.line(
                    (c.0 + radius, c.1 + radius),
                    (c.0 - radius, c.1 + radius),
                    color,
                );
                frame.line(
                    (c.0 - radius, c.1 + radius),
                    (c.0 - radius, c.1 - radius),
                    color,
                );
            }
        }
        Guardian::DeepSeek => {
            if boss.attack == Attack::Leap {
                frame.ring(c, (radius, radius), WATER_LIGHT, !locked);
            }
            let nose = pixel(boss.position.plus(boss.direction.scale(9.0)));
            frame.ring(nose, (17, 8), WATER_LIGHT, true);
        }
        Guardian::Copilot => {
            if boss.attack == Attack::Dive {
                frame.ring(c, (radius, radius), color, !locked);
                frame.line((c.0 - 4, c.1), (c.0 + 4, c.1), color);
            }
        }
    }
}
fn hazards(frame: &mut Raster, arena: &Arena) {
    for h in &arena.hazards {
        let active = h.active();
        let c = pixel(h.position);
        let end = pixel(h.end);
        let r = (h.radius * SCALE) as i32;
        match h.kind {
            HazardKind::Beam => {
                if active {
                    let side = h.end.minus(h.position).normalized().perpendicular();
                    for offset in -r..=r {
                        let delta = side.scale(offset as f32 / SCALE);
                        frame.line(
                            pixel(h.position.plus(delta)),
                            pixel(h.end.plus(delta)),
                            if offset.abs() < r / 2 {
                                WHITE
                            } else {
                                WATER_LIGHT
                            },
                        );
                    }
                    frame.ellipse(end, (7, 5), IVORY);
                }
            }
            HazardKind::Rune => {
                let direction = h.end.minus(h.position);
                for n in 0..12 {
                    let p = pixel(h.position.plus(direction.scale(n as f32 / 11.0)));
                    scenery::rune(frame, p, n, if active { CORE_LIGHT } else { CORE_DARK });
                }
                if active {
                    frame.line(c, end, WHITE);
                    for d in [-2, 2] {
                        frame.line((c.0 + d, c.1), (end.0 + d, end.1), CORE);
                    }
                }
            }
            HazardKind::Fissure => {
                for n in 0..18 {
                    let t = n as f32 / 18.0;
                    let p = h.position.lerp(h.end, t);
                    let q = h.position.lerp(h.end, (n + 1) as f32 / 18.0);
                    let d = h
                        .end
                        .minus(h.position)
                        .normalized()
                        .perpendicular()
                        .scale(if n % 2 == 0 { 0.7 } else { -0.7 });
                    frame.line(
                        pixel(p.plus(d)),
                        pixel(q.minus(d)),
                        if active { WARN } else { SEAM },
                    );
                    if active {
                        let p = pixel(p);
                        frame.line(p, (p.0 + 2, p.1 - 5), FLOOR_LIGHT);
                    }
                }
            }
            HazardKind::Geyser => {
                frame.ring(c, (r, r), WATER_LIGHT, !active);
                if active {
                    let t = (h.time - h.delay) / h.duration;
                    let height = ((t * PI).sin() * 49.0) as i32;
                    frame.ellipse((c.0, c.1 - height / 2), (r / 2, height / 2 + 1), WATER);
                    for n in 0..7 {
                        frame.line(
                            (c.0 - 10 + n * 3, c.1),
                            (c.0 - 13 + n * 4, c.1 - height),
                            WATER_LIGHT,
                        );
                    }
                }
            }
        }
    }
}
fn effects(frame: &mut Raster, arena: &Arena, reduced: bool) {
    for effect in &arena.effects {
        let t = 1.0 - effect.remaining / effect.duration;
        let c = pixel(effect.position);
        let radius = effect.radius * SCALE;
        match effect.kind {
            EffectKind::Wake => {
                for n in 0..3 {
                    let r = (radius * (0.3 + t * 0.8) + n as f32 * 4.0) as i32;
                    frame.ring(
                        c,
                        (r, r / 3),
                        if n == 0 { WATER_LIGHT } else { WATER },
                        true,
                    );
                }
            }
            EffectKind::Impact | EffectKind::Rubble => {
                let r = (radius * (0.4 + t)) as i32;
                frame.ring(c, (r, r / 2), if t < 0.2 { IVORY } else { DUST }, true);
                for n in 0..8 {
                    let a = n as f32 * TAU / 8.0;
                    let p = (
                        c.0 + (a.cos() * radius * 0.7) as i32,
                        c.1 + (a.sin() * radius * 0.35) as i32,
                    );
                    frame.line(c, p, SEAM);
                }
            }
            EffectKind::Teleport => {
                let r = (radius * (1.0 - t)) as i32;
                frame.ring(c, (r, r / 2), GOLD, true);
            }
            EffectKind::Steam => {
                for n in 0..5 {
                    let rise = ((t + n as f32 * 0.15).fract() * 33.0) as i32;
                    frame.ellipse((c.0 - 8 + n * 4, c.1 - rise), (2 + rise / 10, 2), DUST);
                }
            }
            _ => {}
        }
        if reduced {
            continue;
        }
        let count = if effect.kind == EffectKind::Victory {
            28
        } else if effect.kind == EffectKind::Roll {
            4
        } else {
            10
        };
        for n in 0..count {
            let a = n as f32 * TAU / count as f32 + 0.21;
            let travel = radius * (0.1 + t * 1.4);
            let z = if effect.kind == EffectKind::Victory {
                t * 60.0
            } else {
                (t * PI).sin()
                    * if effect.kind == EffectKind::Rubble {
                        29.0
                    } else {
                        12.0
                    }
            };
            let x = c.0 + (a.cos() * travel) as i32;
            let ground = c.1 + (a.sin() * travel * 0.62) as i32;
            let y = ground - z as i32;
            let color = match effect.kind {
                EffectKind::Victory | EffectKind::Catch => {
                    if n % 2 == 0 {
                        WHITE
                    } else {
                        CORE_LIGHT
                    }
                }
                EffectKind::Defeat => CAPE,
                EffectKind::Armor | EffectKind::Shot | EffectKind::Teleport => GOLD,
                EffectKind::Wake => WATER_LIGHT,
                EffectKind::Feather => IVORY,
                EffectKind::Cross => CORE,
                EffectKind::Steam => DUST,
                _ => {
                    if n % 3 == 0 {
                        FLOOR_LIGHT
                    } else {
                        DUST
                    }
                }
            };
            if matches!(effect.kind, EffectKind::Rubble | EffectKind::Impact) {
                frame.ellipse((x + 2, ground), (3, 1), SHADOW);
                frame.rect(x, y, 3, 3, WALL);
                frame.rect(x, y, 3, 1, color);
            } else if effect.kind == EffectKind::Feather {
                frame.line((x, y), (x + 3, y - 5), color);
            } else {
                frame.rect(x, y, if t < 0.45 { 3 } else { 2 }, 2, color);
            }
        }
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
        smoothstep((arena.phase_time - 0.35) / 0.7)
    } else if arena.phase == Phase::Rebirth {
        smoothstep(arena.phase_time / 0.6)
    } else if arena.phase == Phase::Defeat {
        1.0 - smoothstep((arena.phase_time - 0.35) / 0.65)
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
        (1.0 - player.roll_remaining / 0.28) * TAU
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

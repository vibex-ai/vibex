//! Integer-pixel scenery, articulated guardians, and readable combat effects.

use super::{
    combat::{
        Arena, ArrowState, Boss, BossState, Guardian, MIN_CHARGE, Phase, ROLL_DURATION, Vec2,
        smoothstep,
    },
    raster::{Raster, dither, ink::*},
    scenery, sculpture,
};
use std::{
    f32::consts::{PI, TAU},
    sync::OnceLock,
};
#[path = "effects.rs"]
mod effects;

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

#[derive(Clone, Copy)]
enum Actor {
    Body,
    Tentacle(usize),
    Hand(usize),
    Hero,
    Pillar(usize),
}

fn guardian_parts(boss: &Boss) -> Vec<(f32, Actor)> {
    let mut parts = Vec::with_capacity(12);
    parts.push((boss.position.y, Actor::Body));
    match boss.guardian {
        Guardian::Claude => {
            for (ix, position) in boss.tentacles.iter().enumerate() {
                parts.push((position.y, Actor::Tentacle(ix)));
            }
        }
        Guardian::Pi => {
            for (ix, position) in boss.hands.iter().enumerate() {
                parts.push((position.y, Actor::Hand(ix)));
            }
        }
        _ => {}
    }
    parts
}

fn draw_part(frame: &mut Raster, boss: &Boss, part: Actor, time: f32, fallen: f32, reduced: bool) {
    match part {
        Actor::Body => guardian(frame, boss, time, fallen, reduced),
        Actor::Tentacle(ix) if fallen < 2.1 => sculpture::tentacle(frame, boss, ix, fallen),
        Actor::Hand(ix) if fallen < 2.1 => sculpture::hand(frame, boss, ix, fallen),
        _ => {}
    }
}

fn shadow(frame: &mut Raster, boss: &Boss) {
    let (x, y) = pixel(boss.position);
    let height = boss.height.max(0.0);
    let width = match boss.guardian {
        Guardian::DeepSeek => 45,
        Guardian::Claude => 40,
        Guardian::Pi => 43,
        _ => 37,
    };
    let shrink = (height * 0.42).round() as i32;
    frame.ellipse((x + 3, y + 4), (width - shrink, 11 - shrink / 4), SHADOW);
    if boss.guardian == Guardian::Pi {
        for ix in 0..2 {
            let p = pixel(boss.hands[ix]);
            let r = 13 - (boss.hand_heights[ix] * 0.15) as i32;
            frame.ellipse((p.0 + 2, p.1 + 2), (r, 5), SHADOW);
        }
    }
    if boss.guardian == Guardian::Claude {
        for (ix, position) in boss.tentacles.iter().enumerate() {
            if boss.severed & (1 << ix) == 0 {
                let p = pixel(*position);
                let r = (8.0 - boss.tentacle_heights[ix] * 0.16).max(2.0) as i32;
                frame.ellipse((p.0 + 2, p.1 + 2), (r, 3), SHADOW);
            }
        }
    }
}
fn guardian(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let visibility = 1.0 - smoothstep((fallen - 1.3) / 1.15);
    if visibility <= 0.0 {
        return;
    }
    let submerged = boss.state == BossState::Submerged;
    let opacity = visibility * if submerged { 0.48 } else { 1.0 };
    if opacity >= 1.0 {
        sculpture::draw(frame, boss, time, fallen, reduced);
    } else {
        let mut body = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
        sculpture::draw(&mut body, boss, time, fallen, reduced);
        for (ix, color) in body.pixels.iter().enumerate() {
            if *color != CLEAR && dither((ix % WIDTH) as i32, (ix / WIDTH) as i32, opacity) {
                frame.pixels[ix] = if submerged { WATER_DARK } else { *color };
            }
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
            frame.rect(x - 7, y - 7, 15, 15, CORE_DARK);
            frame.rect(x - 5, y - 5, 11, 11, CORE);
            frame.rect(x - 3, y - 3, 7, 7, CORE_LIGHT);
            frame.line((x, y - 10), (x, y + 10), IVORY);
            frame.line((x - 10, y), (x + 10, y), IVORY);
            frame.rect(x - 1, y - 1, 3, 3, WHITE);
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
            frame.polygon(
                &[(x - 8, y), (x, y - 10), (x + 8, y), (x, y + 7)],
                BODY_SHADOW,
            );
            frame.polygon(
                &[(x - 5, y), (x, y - 7), (x + 5, y), (x, y + 5)],
                CORE_LIGHT,
            );
            frame.line((x - 9, y + 2), (x + 9, y - 2), CORE);
            frame.line((x, y - 7), (x, y + 5), WHITE);
            frame.ring((x, y), (10 + beat, 10 + beat), WATER_LIGHT, true);
        }
        Guardian::Copilot => {
            frame.ellipse((x, y), (9, 8), OUTLINE);
            frame.ring((x, y), (9, 8), IVORY, true);
            frame.ellipse((x, y), (6 + beat, 5 + beat), CORE);
            frame.ellipse((x - 1, y - 1), (4, 4), CORE_LIGHT);
            frame.line((x, y - 10), (x, y + 10), WATER_LIGHT);
            frame.line((x - 11, y), (x + 11, y), WATER_LIGHT);
            frame.rect(x - 1, y - 1, 3, 3, WHITE);
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
    let mut order = guardian_parts(boss);
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for (_, part) in order {
        draw_part(&mut frame, boss, part, time, 0.0, reduced);
    }
    frame
}

pub(super) fn battle(arena: &Arena, reduced: bool) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    let reveal = arena.ground_visibility();
    if reveal > 0.0 {
        scenery::damage(&mut frame, &arena.map);
        scenery::ambient(&mut frame, &arena.map, arena.visual_time, reduced);
    }
    effects::ground(&mut frame, arena, reduced);
    shadow(&mut frame, &arena.boss);
    let fallen = if arena.phase == Phase::Victory {
        arena.phase_time
    } else {
        0.0
    };
    let mut order = guardian_parts(&arena.boss);
    order.push((arena.player.position.y, Actor::Hero));
    if reveal > 0.0 {
        for (ix, p) in arena.map.pillars().iter().enumerate() {
            order.push((p.position.y, Actor::Pillar(ix)));
        }
    }
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for (_, actor) in order {
        match actor {
            Actor::Hero => hero(&mut frame, arena, reduced),
            Actor::Pillar(ix) => {
                scenery::pillar(&mut frame, arena.map.pillars()[ix], !arena.map.intact(ix))
            }
            part => draw_part(
                &mut frame,
                &arena.boss,
                part,
                arena.visual_time,
                fallen,
                reduced,
            ),
        }
    }
    effects::attacks(&mut frame, arena, reduced);
    arrow(&mut frame, arena);
    effects::particles(&mut frame, arena, reduced);
    frame
}

fn arrow(frame: &mut Raster, arena: &Arena) {
    if arena.arrow.state == ArrowState::Ready {
        return;
    }
    let p = pixel(arena.arrow.position);
    let d = arena.arrow.velocity.normalized();
    let tail = pixel(arena.arrow.position.minus(d.scale(2.6)));
    if matches!(
        arena.arrow.state,
        ArrowState::Returning | ArrowState::Tethered
    ) {
        let from = pixel(arena.player.position);
        frame.line(from, p, WATER_DARK);
        let direction = arena.arrow.position.minus(arena.player.position);
        for n in 0..12 {
            let t = n as f32 / 12.0;
            let a = arena.player.position.plus(direction.scale(t));
            let b = arena.player.position.plus(direction.scale(t + 0.025));
            frame.line(pixel(a), pixel(b), WATER_LIGHT);
        }
        if arena.arrow.state == ArrowState::Tethered {
            let strain = arena.boss.tether.clamp(0.0, 1.0);
            frame.ring(p, (7 + (strain * 5.0) as i32, 7), IVORY, true);
            frame.line((p.0, p.1 - 10), (p.0, p.1 + 10), CORE_LIGHT);
        }
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
        (1.0 - player.roll_remaining / ROLL_DURATION) * TAU
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
    if player.swimming && !dying {
        sprite.rect(0, 11, 13, 5, CLEAR);
        frame.ring((x, y - 1), (10, 4), WATER_LIGHT, true);
        frame.ring((x + 1, y + 1), (15, 5), WATER, true);
    } else if player.moving && !dying && !reduced {
        let tail = player.position.minus(player.facing.scale(2.0));
        let p = pixel(tail);
        frame.polygon(
            &[
                (x - 3, y - 9),
                (x + 3, y - 9),
                (p.0 + 3, p.1 - 4 + stride),
                (p.0 - 3, p.1 - 5),
            ],
            CAPE,
        );
        frame.line((x, y - 8), (p.0, p.1 - 4 + stride), CAPE_LIGHT);
    }
    if player.roll_remaining > 0.0 && !reduced {
        let trail = player.position.minus(player.facing.scale(2.8));
        let p = pixel(trail);
        frame.ellipse((p.0, p.1 - 3), (4, 2), CLOTH_DARK);
        frame.line((p.0 - 2, p.1 - 4), (p.0 + 2, p.1 - 4), CAPE);
    }
    frame.transformed(&sprite, (x, y - 7 + stride), angle, spawn);
    if !dying && !player.swimming && player.roll_remaining <= 0.0 {
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
            let progress = (player.charge / MIN_CHARGE).clamp(0.0, 1.0);
            frame.ring((x, y - 7), (10, 10), GOLD_DARK, true);
            for n in 0..(progress * 16.0) as i32 {
                let angle = -PI * 0.5 + n as f32 * TAU / 16.0;
                frame.rect(
                    x + (angle.cos() * 10.0) as i32,
                    y - 7 + (angle.sin() * 10.0) as i32,
                    2,
                    2,
                    if ready { IVORY } else { GOLD },
                );
            }
            if ready {
                frame.rect(tip.x as i32 - 1, tip.y as i32 - 1, 3, 3, WHITE);
            }
        }
    }
}

#[cfg(test)]
#[path = "art_tests.rs"]
mod tests;

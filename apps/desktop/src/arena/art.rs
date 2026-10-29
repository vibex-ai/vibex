//! Integer-pixel scenery, articulated guardians, and readable combat effects.

use super::{
    combat::{
        Arena, ArrowState, Attack, Boss, BossState, Guardian, MIN_CHARGE, Phase, ROLL_DURATION,
        Vec2, smoothstep,
    },
    guardian::IDLE_LOOP,
    map::BRAZIER,
    raster::{Raster, dither, hash, ink::*},
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
pub(super) const IDLE_PERIOD: f32 = IDLE_LOOP;
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
    Stem(usize),
    Blade,
    Bar(usize),
    Shard(usize),
    Block(usize),
    Brazier,
    Hero,
    Pillar(usize),
}

fn guardian_parts(boss: &Boss) -> Vec<(f32, Actor)> {
    let mut parts = Vec::with_capacity(8);
    parts.push((boss.position.y, Actor::Body));
    match boss.guardian {
        Guardian::Pi => {
            for (ix, position) in boss.hands.iter().enumerate() {
                parts.push((position.y, Actor::Stem(ix)));
            }
        }
        Guardian::ZCode => {
            for (ix, position) in boss.hands.iter().enumerate() {
                parts.push((position.y, Actor::Bar(ix)));
            }
        }
        Guardian::Grok if boss.detached => {
            parts.push((boss.hands[0].y, Actor::Blade));
        }
        Guardian::Claude => {
            for (ix, shard) in boss.shards.iter().enumerate() {
                if shard.alive {
                    parts.push((shard.position.y, Actor::Shard(ix)));
                }
            }
        }
        _ => {}
    }
    parts
}

fn draw_part(frame: &mut Raster, boss: &Boss, part: Actor, time: f32, fallen: f32, reduced: bool) {
    match part {
        Actor::Body => guardian(frame, boss, time, fallen, reduced),
        Actor::Stem(ix) if fallen < 2.1 => sculpture::stem(frame, boss, ix, fallen),
        Actor::Blade => sculpture::blade(frame, boss),
        Actor::Bar(ix) => sculpture::zbar(frame, boss, ix, fallen),
        Actor::Shard(ix) => sculpture::shard(frame, &boss.shards[ix], time),
        _ => {}
    }
}

fn shadow(frame: &mut Raster, boss: &Boss) {
    let hidden = boss.fade < 0.5
        || boss.guardian == Guardian::DeepSeek
            && (boss.state == BossState::Submerged || boss.height < -0.5);
    if !hidden {
        let (x, y) = pixel(boss.position);
        let width = match boss.guardian {
            Guardian::Claude => (34.0 * boss.scale()) as i32,
            Guardian::Codex => 34,
            Guardian::Pi => 40,
            Guardian::DeepSeek => 50,
            Guardian::OpenCode | Guardian::Copilot => 30,
            Guardian::Cursor => 38,
            Guardian::Cline => 32,
            Guardian::Grok => 30,
            Guardian::ZCode => 44,
        };
        let shrink = (boss.height.max(0.0) * 0.42).round() as i32;
        frame.ellipse(
            (x + 2, y + 3),
            ((width - shrink).max(5), (10 - shrink / 4).max(2)),
            SHADOW,
        );
    }
    if boss.guardian == Guardian::ZCode {
        for ix in 0..2 {
            let p = pixel(boss.hands[ix]);
            let r = (16.0 - boss.hand_heights[ix] * 0.3).max(6.0) as i32;
            frame.ellipse((p.0 + 2, p.1 + 2), (r, (r / 3).max(2)), SHADOW);
        }
    }
    if boss.guardian == Guardian::Grok && boss.detached {
        let p = pixel(boss.hands[0]);
        frame.ellipse((p.0 + 2, p.1 + 2), (14, 3), SHADOW);
    }
    if boss.guardian == Guardian::Pi {
        for ix in 0..2 {
            let p = pixel(boss.hands[ix]);
            let r = (13.0 - boss.hand_heights[ix] * 0.3).max(5.0) as i32;
            frame.ellipse((p.0 + 2, p.1 + 2), (r, (r / 2).max(2)), SHADOW);
        }
    }
    for shard in boss.shards.iter().filter(|s| s.alive) {
        let p = pixel(shard.position);
        let r = (30.0 * shard.size - shard.height * 0.4).max(4.0) as i32;
        frame.ellipse((p.0 + 2, p.1 + 2), (r, (r / 3).max(2)), SHADOW);
    }
}

fn guardian(frame: &mut Raster, boss: &Boss, time: f32, fallen: f32, reduced: bool) {
    let visibility = (1.0 - smoothstep((fallen - 1.3) / 1.15)) * boss.fade;
    if visibility <= 0.0 {
        return;
    }
    let submerged = boss.guardian == Guardian::DeepSeek
        && (boss.state == BossState::Submerged
            || boss.attack == Attack::Discharge && boss.state == BossState::Striking);
    let opacity = visibility * if submerged { 0.5 } else { 1.0 };
    let flash = if reduced {
        0.0
    } else {
        (boss.flash / 0.12).clamp(0.0, 1.0)
    };
    if opacity >= 1.0 && flash <= 0.0 {
        sculpture::draw(frame, boss, time, fallen, reduced);
    } else {
        let mut body = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
        sculpture::draw(&mut body, boss, time, fallen, reduced);
        for (ix, color) in body.pixels.iter().enumerate() {
            let (x, y) = ((ix % WIDTH) as i32, (ix / WIDTH) as i32);
            if *color != CLEAR && dither(x, y, opacity) {
                frame.pixels[ix] = if submerged {
                    WATER_DARK
                } else if dither(x + 1, y, flash) {
                    WHITE
                } else {
                    *color
                };
            }
        }
    }
    if boss.exposed > 0.0 && fallen <= 0.0 && !submerged {
        weak_point(frame, boss, time, reduced);
    }
}

/// A readable marker over the weak point while it accepts the arrow.
fn weak_point(frame: &mut Raster, boss: &Boss, time: f32, reduced: bool) {
    let (x, y) = pixel(boss.core);
    let beat = if reduced {
        0
    } else {
        ((time * 9.0).sin() * 1.5).round() as i32
    };
    match boss.guardian {
        Guardian::Claude => {
            // A bare heart.
            for (grow, color) in [(1, OUTLINE), (0, CORE)] {
                frame.ellipse((x - 2, y - 2), (2 + grow, 2 + grow), color);
                frame.ellipse((x + 2, y - 2), (2 + grow, 2 + grow), color);
                frame.polygon(
                    &[
                        (x - 4 - grow, y - 1),
                        (x + 4 + grow, y - 1),
                        (x, y + 4 + grow),
                    ],
                    color,
                );
            }
            frame.rect(x - 3, y - 3, 2, 1, CORE_LIGHT);
            frame.ring((x, y), (10 + beat, 10 + beat), CORE_LIGHT, true);
        }
        Guardian::Codex => {
            let shielded = boss.cube_shielded();
            let color = if shielded { CORE_DARK } else { CORE_LIGHT };
            frame.ring((x, y), (11 + beat, 11 + beat), color, shielded);
            if !shielded {
                gleam(frame, (x, y), 7, WHITE);
            }
        }
        Guardian::Pi => {
            frame.ring((x, y), (12 + beat, 12 + beat), CORE_LIGHT, false);
            frame.ring((x, y), (15 + beat, 15 + beat), CORE, true);
            gleam(frame, (x, y), 5, WHITE);
        }
        Guardian::OpenCode
        | Guardian::DeepSeek
        | Guardian::Copilot
        | Guardian::Cursor
        | Guardian::Cline
        | Guardian::Grok
        | Guardian::ZCode => {
            frame.ring((x, y), (9 + beat, 9 + beat), CORE_LIGHT, false);
            frame.ring((x, y), (13 + beat, 13 + beat), CORE, true);
            gleam(frame, (x, y), 4, WHITE);
        }
    }
}

fn gleam(frame: &mut Raster, c: (i32, i32), radius: i32, color: u8) {
    frame.line((c.0 - radius, c.1), (c.0 + radius, c.1), color);
    frame.line((c.0, c.1 - radius), (c.0, c.1 + radius), color);
    frame.rect(c.0 - 1, c.1 - 1, 3, 3, WHITE);
}

#[cfg(test)]
pub(super) fn preview(guardian: Guardian, time: f32, reduced: bool) -> Raster {
    idle(&Boss::idle(guardian, time, reduced), time, reduced)
}

/// The raster window a companion crops around a guardian's ground anchor, in
/// raster pixels relative to that anchor: `(left, top, right, bottom)`.
///
/// A companion is one small model riding a lane, so it draws the idle pose into
/// a window instead of the whole arena-sized frame. The window has to hold
/// every pose the idle loop reaches — the whale's breach, the knot's roll,
/// Pi's detached stems — because the companion fits the window, not the pose,
/// into its lane; a window too small would slice the model at the extremes.
pub(super) const COMPANION_BOXES: [(i32, i32, i32, i32); Guardian::ALL.len()] = [
    (-43, -124, 43, -2),
    (-48, -102, 48, 13),
    (-67, -111, 67, 33),
    (-36, -122, 36, 19),
    (-88, -206, 84, 22),
    (-55, -133, 56, 6),
    (-43, -85, 42, 21),
    (-49, -122, 48, -5),
    (-54, -100, 53, 15),
    (-92, -161, 91, 7),
];
pub(super) fn companion_box(guardian: Guardian) -> (i32, i32, i32, i32) {
    COMPANION_BOXES[guardian as usize]
}
/// The height, in raster pixels, of each guardian's ordinary idle pose: the
/// median over its loop. The window above also has to hold the loop's
/// extremes, so it is a poor measure of how large the model reads; a companion
/// sizes the model by this instead.
pub(super) const COMPANION_HEIGHTS: [i32; Guardian::ALL.len()] =
    [65, 96, 110, 85, 106, 82, 92, 95, 79, 117];
pub(super) fn companion_height(guardian: Guardian) -> i32 {
    COMPANION_HEIGHTS[guardian as usize]
}

/// One idle frame, cropped to [`companion_box`] around the anchor the model
/// currently stands on.
///
/// The crop keeps the articulated parts the arena draws — the stems and shards
/// that hang off the body — and drops the arena ground, which a companion
/// never stands on.
pub(super) fn companion(boss: &Boss, time: f32, reduced: bool) -> Raster {
    let (left, top, right, bottom) = companion_box(boss.guardian);
    let (x, y) = pixel(boss.position);
    let mut frame = Raster::with_offset(
        (right - left) as usize,
        (bottom - top) as usize,
        -(x + left),
        -(y + top),
    );
    let mut order = guardian_parts(boss);
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for (_, part) in order {
        draw_part(&mut frame, boss, part, time, 0.0, reduced);
    }
    frame
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
        for (ix, block) in arena.map.blocks.iter().enumerate() {
            order.push((block.y, Actor::Block(ix)));
        }
        if arena.boss.guardian == Guardian::Codex {
            order.push((BRAZIER.y, Actor::Brazier));
        }
    }
    order.sort_unstable_by(|a, b| a.0.total_cmp(&b.0));
    for (_, actor) in order {
        match actor {
            Actor::Hero => hero(&mut frame, arena, reduced),
            Actor::Pillar(ix) => {
                scenery::pillar(&mut frame, arena.map.pillars()[ix], !arena.map.intact(ix))
            }
            Actor::Block(ix) => scenery::block(&mut frame, arena.map.blocks[ix]),
            Actor::Brazier => {
                scenery::brazier(&mut frame, arena.brazier_lit(), arena.visual_time, reduced)
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
    arrow(&mut frame, arena, reduced);
    effects::particles(&mut frame, arena, reduced);
    frame
}

fn arrow(frame: &mut Raster, arena: &Arena, reduced: bool) {
    if arena.arrow.state == ArrowState::Ready {
        return;
    }
    let p = pixel(arena.arrow.position);
    let d = arena.arrow.velocity.normalized();
    let tail = pixel(arena.arrow.position.minus(d.scale(2.6)));
    if arena.arrow.state == ArrowState::Returning {
        let from = pixel(arena.player.position);
        frame.line(from, p, WATER_DARK);
        let direction = arena.arrow.position.minus(arena.player.position);
        for n in 0..12 {
            let t = n as f32 / 12.0;
            let a = arena.player.position.plus(direction.scale(t));
            let b = arena.player.position.plus(direction.scale(t + 0.025));
            frame.line(pixel(a), pixel(b), WATER_LIGHT);
        }
    }
    if arena.arrow.state == ArrowState::Flying && !reduced {
        // A short speed streak behind a loosed arrow.
        let streak = pixel(arena.arrow.position.minus(d.scale(6.5)));
        frame.line(streak, tail, DUST);
    }
    if arena.arrow.burning > 0.0 && !reduced {
        let tick = (arena.visual_time * 30.0) as i32;
        for n in 0..5 {
            let flicker = (hash(n, tick) % 3) as i32;
            let q = pixel(arena.arrow.position.minus(d.scale(0.6 + n as f32 * 0.7)));
            frame.rect(
                q.0 - 1,
                q.1 - 2 - flicker,
                2,
                2 + flicker,
                if n % 2 == 0 { FIRE } else { WARN },
            );
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
    let rolling = player.roll_remaining > 0.0;
    let angle = if dying {
        smoothstep(arena.phase_time / 0.4) * PI * 0.5
    } else if rolling {
        (1.0 - player.roll_remaining / ROLL_DURATION) * TAU
    } else if player.knocked() {
        player.knockback.x.signum() * 0.55
    } else if player.sprinting && !reduced {
        player.facing.x * 0.18
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
    if rolling && !reduced {
        // Two fading afterimages trace the dodge.
        let progress = 1.0 - player.roll_remaining / ROLL_DURATION;
        for (n, visibility) in [(1.0, 0.45), (2.0, 0.22)] {
            let p = pixel(player.position.minus(player.facing.scale(n * 2.4)));
            frame.transformed(
                &sprite,
                (p.0, p.1 - 7),
                angle - n * 0.9,
                visibility * (1.0 - progress * 0.4) * spawn,
            );
        }
    }
    if player.swimming && !dying {
        sprite.rect(0, 11, 13, 5, CLEAR);
        frame.ring((x, y - 1), (10, 4), WATER_LIGHT, true);
        frame.ring((x + 1, y + 1), (15, 5), WATER, true);
    } else if player.moving && !dying && !reduced {
        let tail =
            player.position.minus(
                player
                    .facing
                    .scale(if player.sprinting { 3.0 } else { 2.0 }),
            );
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
    frame.transformed(&sprite, (x, y - 7 + stride), angle, spawn);
    if player.slowed && !dying && !reduced {
        for n in 0..3 {
            let drip = ((arena.visual_time * 3.0 + n as f32 * 0.33).fract() * 5.0) as i32;
            frame.rect(x - 4 + n * 4, y - 2 + drip, 2, 2, CORE);
        }
    }
    if !dying && !player.swimming && !rolling && !player.knocked() {
        let aim = player.facing;
        let side = Vec2::new(-aim.y, aim.x);
        let hand = player
            .position
            .scale(SCALE)
            .plus(aim.scale(7.0))
            .plus(Vec2::new(0.0, -6.0));
        let start = hand.plus(side.scale(5.0));
        let middle = hand.plus(aim.scale(3.0 - player.charge * 2.5));
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
#[path = "sheet_tests.rs"]
mod sheets;
#[cfg(test)]
#[path = "art_tests.rs"]
mod tests;

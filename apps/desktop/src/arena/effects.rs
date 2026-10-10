//! Combat cues use simulation positions and radii. Decorative motion never
//! changes the footprint of a landing, a beam or an exposed joint.

use super::{SCALE, pixel};
use crate::arena::{
    combat::{
        Arena, Attack, Boss, BossState, EffectKind, Guardian, HazardKind, Phase, ProjectileKind,
        Vec2, WaveKind, smoothstep,
    },
    map::{SEAL_RADIUS, SEALS},
    raster::{Raster, hash, ink::*},
};
use std::f32::consts::{PI, TAU};

fn outline(frame: &mut Raster, c: (i32, i32), r: i32, color: u8) {
    for (a, b) in [
        ((c.0 - r, c.1 - r), (c.0 + r, c.1 - r)),
        ((c.0 + r, c.1 - r), (c.0 + r, c.1 + r)),
        ((c.0 + r, c.1 + r), (c.0 - r, c.1 + r)),
        ((c.0 - r, c.1 + r), (c.0 - r, c.1 - r)),
    ] {
        frame.line(a, b, color);
    }
}

fn gleam(frame: &mut Raster, c: (i32, i32), radius: i32, color: u8) {
    frame.line((c.0 - radius, c.1), (c.0 + radius, c.1), color);
    frame.line((c.0, c.1 - radius), (c.0, c.1 + radius), color);
    frame.rect(c.0 - 1, c.1 - 1, 3, 3, WHITE);
}

fn route(frame: &mut Raster, from: Vec2, to: Vec2, radius: f32, color: u8) {
    let direction = to.minus(from);
    let side = direction.normalized().perpendicular().scale(radius);
    for offset in [-1.0, 1.0] {
        for part in 0..10 {
            let a = from.lerp(to, part as f32 / 10.0).plus(side.scale(offset));
            let b = from
                .lerp(to, (part as f32 + 0.5) / 10.0)
                .plus(side.scale(offset));
            frame.line(pixel(a), pixel(b), color);
        }
    }
    let heading = direction.normalized();
    for part in 1..6 {
        let p = from.lerp(to, part as f32 / 6.0);
        let back = p.minus(heading.scale(1.6));
        let wing = heading.perpendicular().scale(1.3);
        frame.line(pixel(back.plus(wing)), pixel(p), color);
        frame.line(pixel(back.minus(wing)), pixel(p), color);
    }
}

pub(super) fn ground(frame: &mut Raster, arena: &Arena, reduced: bool) {
    if arena.ground_visibility() <= 0.0 {
        return;
    }
    if arena.boss.guardian == Guardian::Pi {
        seals(frame, &arena.boss, reduced);
    }
    if arena.phase == Phase::Battle {
        telegraph(frame, arena);
    }
    for wave in &arena.waves {
        let c = pixel(wave.position);
        let r = (wave.radius * SCALE).round() as i32;
        let (dark, light) = match wave.kind {
            WaveKind::Stone if arena.boss.guardian == Guardian::Codex => (GOLD_DARK, IVORY),
            WaveKind::Stone => (FLOOR_SHADE, DUST),
            WaveKind::Water => (WATER, WATER_LIGHT),
        };
        // A wave's ground radius is circular, just like its collision field.
        frame.ring(c, (r + 2, r + 2), dark, false);
        frame.ring(c, (r, r), light, false);
        frame.ring(c, (r - 2, r - 2), dark, true);
        for n in 0..16 {
            let a = n as f32 * TAU / 16.0;
            let p = (
                c.0 + (a.cos() * r as f32) as i32,
                c.1 + (a.sin() * r as f32) as i32,
            );
            if wave.kind == WaveKind::Stone {
                frame.rect(p.0, p.1 - 3, 3, 3, WALL);
                frame.put(p.0, p.1 - 3, DUST);
            } else {
                frame.line(p, (p.0 + 3, p.1 - 4), light);
            }
        }
    }
    for h in &arena.hazards {
        let c = pixel(h.position);
        let r = (h.radius * SCALE).ceil() as i32;
        let active = h.active();
        match h.kind {
            HazardKind::Gravity => {
                frame.ring(c, (r, r), CORE_DARK, true);
                frame.ring(c, (r - 2, r - 2), BODY_LIGHT, true);
                for n in 0..12 {
                    let a = n as f32 * TAU / 12.0;
                    let flow = if reduced {
                        0.5
                    } else {
                        (h.time * 0.65 + n as f32 * 0.17).fract()
                    };
                    let p = h
                        .position
                        .plus(Vec2::from_angle(a).scale(h.radius * (1.0 - flow)));
                    let q = p.lerp(h.position, 0.1);
                    frame.line(pixel(p), pixel(q), IVORY);
                }
            }
            HazardKind::Geyser => {
                frame.ring(c, (r, r), if active { WATER_LIGHT } else { WATER }, !active);
                if !active {
                    let progress = (h.time / h.delay.max(0.001)).clamp(0.0, 1.0);
                    frame.ring(
                        c,
                        ((r as f32 * (1.35 - progress * 0.35)) as i32, r),
                        WATER_LIGHT,
                        true,
                    );
                    for n in [-1, 1] {
                        frame.ellipse((c.0 + n * r / 3, c.1 - n * 2), (2, 1), WATER_LIGHT);
                    }
                }
            }
            HazardKind::Beam => {
                if !active {
                    route(frame, h.position, h.end, h.radius, WARN_DARK);
                }
            }
            HazardKind::Scorch => {
                let direction = h.end.minus(h.position).normalized();
                let count = (h.end.minus(h.position).length() * 2.0)
                    .ceil()
                    .clamp(3.0, 32.0) as i32;
                for n in 0..count {
                    let p = h.position.lerp(h.end, n as f32 / (count - 1) as f32);
                    let q = h.position.lerp(h.end, (n + 1) as f32 / (count - 1) as f32);
                    let d = direction.perpendicular().scale(if n % 2 == 0 {
                        h.radius * 0.65
                    } else {
                        -h.radius * 0.65
                    });
                    frame.line(
                        pixel(p.plus(d)),
                        pixel(q.minus(d)),
                        if active { WARN } else { SEAM },
                    );
                    if active {
                        let c = pixel(p);
                        let flicker = if reduced {
                            2
                        } else {
                            ((h.time * 15.0 + n as f32).sin() * 3.0) as i32
                        };
                        let height = (r + 4 + flicker).max(3);
                        frame.ellipse(c, (r.max(2), 2), CORE_DARK);
                        frame.polygon(
                            &[
                                (c.0 - 3, c.1),
                                (c.0 - 1, c.1 - height),
                                (c.0 + 3, c.1 - 3),
                                (c.0 + 2, c.1),
                            ],
                            FIRE,
                        );
                        frame.line(c, (c.0, c.1 - height / 2), WARN);
                    }
                }
            }
        }
    }
}

fn seals(frame: &mut Raster, boss: &Boss, reduced: bool) {
    for (ix, position) in SEALS.into_iter().enumerate() {
        let c = pixel(position);
        let r = (SEAL_RADIUS * SCALE) as i32;
        let locked = boss.seals & (1 << ix) != 0;
        let color = if locked { CORE_LIGHT } else { GOLD_DARK };
        frame.ellipse((c.0, c.1 + 2), (r + 4, r + 2), FLOOR_SHADE);
        frame.ring(c, (r, r), color, false);
        frame.ring(c, (r - 4, r - 4), if locked { CORE } else { SEAM }, false);
        outline(frame, c, 7, color);
        frame.rect(
            c.0 - 2,
            c.1 - 2,
            5,
            5,
            if locked { IVORY } else { FLOOR_LIGHT },
        );
        for side in [-1, 1] {
            frame.line((c.0 + side * 10, c.1), (c.0 + side * (r - 5), c.1), color);
        }
        if locked {
            for n in 0..6 {
                let a = n as f32 * TAU / 6.0;
                let t = if reduced {
                    0.5
                } else {
                    (boss.time * 0.6 + n as f32 / 6.0).fract()
                };
                let p = (
                    c.0 + (a.cos() * r as f32) as i32,
                    c.1 + (a.sin() * r as f32) as i32,
                );
                frame.line(p, (p.0, p.1 - (t * 15.0) as i32), color);
            }
        }
    }
}

fn telegraph(frame: &mut Raster, arena: &Arena) {
    let boss = &arena.boss;
    if !matches!(
        boss.state,
        BossState::Windup | BossState::Striking | BossState::Rushing
    ) {
        return;
    }
    let locked = boss.target_locked();
    let color = if locked { WARN } else { WARN_DARK };
    let c = pixel(boss.target);
    let r = (boss.impact_radius() * SCALE).ceil() as i32;
    match boss.attack {
        Attack::LeftFist | Attack::RightFist => {
            if boss.state == BossState::Windup || boss.state == BossState::Striking {
                frame.ring(c, (r, r), color, !locked);
                frame.line((c.0 - 4, c.1), (c.0 + 4, c.1), color);
                frame.line((c.0, c.1 - 4), (c.0, c.1 + 4), color);
            }
        }
        Attack::TendrilSweep => {
            for ix in 0..6 {
                if ix / 2 != boss.ward as usize || boss.severed & (1 << ix) != 0 {
                    continue;
                }
                let p = pixel(boss.tentacle_strike(ix, 1.0));
                frame.ring(p, (r, r), color, !locked);
            }
        }
        Attack::Rush => {
            if boss.state == BossState::Windup {
                let end = boss.position.plus(boss.direction.scale(35.0));
                route(frame, boss.position, end, boss.radius(), color);
            }
        }
        Attack::Dash => {
            route(frame, boss.from, boss.target, boss.radius(), color);
            frame.ring(c, (r, r), color, !locked);
            frame.ring(c, (r + 3, r + 3), IVORY, true);
            frame.line((c.0 - 4, c.1), (c.0 + 4, c.1), color);
            frame.line((c.0, c.1 - 4), (c.0, c.1 + 4), color);
        }
        Attack::Leap | Attack::Stomp => {
            if boss.attack == Attack::Stomp {
                outline(frame, c, r, color);
                outline(frame, c, r - 3, WARN_DARK);
            } else {
                frame.ring(c, (r, r), color, !locked);
            }
            let ring = if boss.state == BossState::Windup {
                r + ((1.0 - boss.progress()) * 16.0) as i32
            } else {
                r
            };
            frame.ring(
                c,
                (ring, ring),
                if locked { IVORY } else { WARN_DARK },
                true,
            );
            for side in [-1, 1] {
                frame.line(
                    (c.0 + side * (r + 4), c.1),
                    (c.0 + side * (r + 9), c.1),
                    color,
                );
            }
        }
        Attack::Sweep | Attack::VisorBeam => {
            if boss.state == BossState::Windup {
                route(
                    frame,
                    boss.attack_origin(),
                    boss.beam_end,
                    boss.beam_radius(),
                    color,
                );
                let origin = pixel(boss.attack_origin());
                let r = 3 + (boss.progress() * 6.0) as i32;
                frame.ring(origin, (r, r), color, false);
            }
        }
        Attack::Pulse => {
            let c = pixel(boss.pulse_origin());
            frame.ring(c, (29, 29), color, true);
            frame.ring(c, (36, 36), WARN_DARK, true);
            for n in 0..8 {
                let a = n as f32 * TAU / 8.0;
                let p = (c.0 + (a.cos() * 37.0) as i32, c.1 + (a.sin() * 37.0) as i32);
                frame.rect(p.0 - 1, p.1 - 1, 3, 3, color);
            }
        }
        Attack::Spokes => {
            if boss.state == BossState::Windup {
                for (from, end) in arena.spokes() {
                    route(frame, from, end, boss.beam_radius(), color);
                }
            }
        }
        Attack::Inhale => {
            let c = pixel(boss.attack_origin());
            frame.ring(c, (12, 12), IVORY, true);
            for n in 0..8 {
                let a = n as f32 * TAU / 8.0;
                let distance = 12.0 + (1.0 - boss.progress()) * 16.0;
                let p = (
                    c.0 + (a.cos() * distance) as i32,
                    c.1 + (a.sin() * distance) as i32,
                );
                frame.line(
                    p,
                    (c.0 + (a.cos() * 10.0) as i32, c.1 + (a.sin() * 10.0) as i32),
                    BODY_LIGHT,
                );
            }
        }
        Attack::Volley | Attack::Barrage => {
            let c = pixel(boss.attack_origin());
            for side in [-1, 1] {
                outline(frame, (c.0 + side * 12, c.1 - 4), 3, color);
            }
        }
    }
}

pub(super) fn attacks(frame: &mut Raster, arena: &Arena, reduced: bool) {
    if arena.boss.guardian == Guardian::Claude && arena.phase == Phase::Battle {
        for ix in 0..6 {
            if arena.boss.tentacle_vulnerable(ix) {
                let c = pixel(arena.boss.tentacle_joint(ix));
                frame.ring(c, (7, 7), CORE_DARK, false);
                frame.ring(c, (5, 5), CORE_LIGHT, true);
                gleam(frame, c, 3, CORE);
            }
        }
    }
    for h in &arena.hazards {
        if !h.active() {
            continue;
        }
        let c = pixel(h.position);
        let r = (h.radius * SCALE).ceil() as i32;
        if h.kind == HazardKind::Beam {
            let side = h.end.minus(h.position).normalized().perpendicular();
            for offset in -r..=r {
                let delta = side.scale(offset as f32 / SCALE);
                frame.line(
                    pixel(h.position.plus(delta)),
                    pixel(h.end.plus(delta)),
                    if offset.abs() < r / 3 {
                        WHITE
                    } else if offset.abs() < r - 1 {
                        CORE_LIGHT
                    } else {
                        CORE
                    },
                );
            }
            gleam(frame, c, r + 5, CORE_LIGHT);
            let end = pixel(h.end);
            frame.ellipse(end, (r + 3, r + 1), CORE);
            gleam(frame, end, r + 4, IVORY);
            if !reduced {
                for n in 0..5 {
                    let a = n as f32 * TAU / 5.0 + h.time;
                    let d = 5.0 + (h.time * 30.0 + n as f32 * 2.0).rem_euclid(12.0);
                    frame.line(
                        end,
                        (end.0 + (a.cos() * d) as i32, end.1 + (a.sin() * d) as i32),
                        CORE_LIGHT,
                    );
                }
            }
        } else if h.kind == HazardKind::Geyser {
            let t = ((h.time - h.delay) / h.duration).clamp(0.0, 1.0);
            let height = ((t * PI).sin() * 58.0).round() as i32;
            let width = (r / 2).max(3);
            frame.polygon(
                &[
                    (c.0 - width, c.1),
                    (c.0 - width / 2, c.1 - height),
                    (c.0 + width / 2, c.1 - height),
                    (c.0 + width, c.1),
                ],
                WATER,
            );
            frame.ellipse((c.0, c.1 - height), (width / 2 + 2, 3), WATER_LIGHT);
            for n in -2..=2 {
                let x = c.0 + n * width / 3;
                frame.line(
                    (x, c.1),
                    (x + n, c.1 - height + n.abs() * 3),
                    if n == 0 { IVORY } else { WATER_LIGHT },
                );
            }
            frame.ring(c, (r, r), WATER_LIGHT, false);
        }
    }
    for p in &arena.projectiles {
        let c = pixel(p.position);
        let direction = p.velocity.normalized();
        let tail = pixel(p.position.minus(direction.scale(3.2)));
        let color = match p.kind {
            ProjectileKind::Orb | ProjectileKind::ChargedOrb => WATER_LIGHT,
            ProjectileKind::Block => BODY_LIGHT,
            ProjectileKind::Segment => GOLD,
        };
        frame.line(
            tail,
            c,
            if p.kind == ProjectileKind::Block {
                BODY_DARK
            } else {
                CORE_DARK
            },
        );
        match p.kind {
            ProjectileKind::Orb | ProjectileKind::ChargedOrb => {
                let charged = p.kind == ProjectileKind::ChargedOrb;
                let r = if charged { 8 } else { 4 };
                frame.ellipse(c, (r, r), WATER_DARK);
                frame.ring(c, (r, r), color, false);
                frame.ellipse(c, (r - 2, r - 2), WATER);
                frame.ellipse((c.0 - 1, c.1 - 1), ((r - 3).max(1), (r - 3).max(1)), IVORY);
                if charged {
                    // Four open brackets make the interceptable charge distinct
                    // from ordinary round shots without relying on color alone.
                    for side in [-1, 1] {
                        frame.line(
                            (c.0 + side * 11, c.1 - 4),
                            (c.0 + side * 11, c.1 + 4),
                            CORE_LIGHT,
                        );
                        frame.line(
                            (c.0 - 4, c.1 + side * 11),
                            (c.0 + 4, c.1 + side * 11),
                            CORE_LIGHT,
                        );
                    }
                }
            }
            ProjectileKind::Block => {
                frame.ellipse((c.0 + 2, c.1 + 6), (5, 2), SHADOW);
                frame.polygon(
                    &[
                        (c.0 - 4, c.1 - 3),
                        (c.0 + 1, c.1 - 6),
                        (c.0 + 5, c.1 - 3),
                        (c.0, c.1),
                    ],
                    BODY_LIGHT,
                );
                frame.polygon(
                    &[
                        (c.0 - 4, c.1 - 3),
                        (c.0, c.1),
                        (c.0, c.1 + 6),
                        (c.0 - 4, c.1 + 3),
                    ],
                    BODY,
                );
                frame.polygon(
                    &[
                        (c.0, c.1),
                        (c.0 + 5, c.1 - 3),
                        (c.0 + 5, c.1 + 3),
                        (c.0, c.1 + 6),
                    ],
                    BODY_SHADOW,
                );
                frame.line((c.0 - 2, c.1), (c.0 - 2, c.1 + 3), IVORY);
            }
            ProjectileKind::Segment => {
                let side = direction.perpendicular().scale(0.9);
                frame.polygon(
                    &[
                        c,
                        pixel(p.position.minus(direction.scale(3.8)).plus(side)),
                        tail,
                        pixel(p.position.minus(side)),
                    ],
                    color,
                );
                frame.line(tail, c, IVORY);
            }
        }
    }
}

pub(super) fn particles(frame: &mut Raster, arena: &Arena, reduced: bool) {
    for effect in &arena.effects {
        let t = (1.0 - effect.remaining / effect.duration).clamp(0.0, 1.0);
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
                let r = (radius * (0.35 + t * 1.1)) as i32;
                frame.ring(c, (r, r / 2), if t < 0.12 { IVORY } else { DUST }, true);
                if !reduced && t < 0.14 {
                    for n in 0..8 {
                        let a = n as f32 * TAU / 8.0;
                        let tip = (
                            c.0 + (a.cos() * radius * 0.8) as i32,
                            c.1 + (a.sin() * radius * 0.5) as i32,
                        );
                        frame.line(c, tip, IVORY);
                    }
                }
            }
            EffectKind::Sever => {
                frame.ring(
                    c,
                    ((7.0 + t * 15.0) as i32, (4.0 + t * 7.0) as i32),
                    CORE,
                    true,
                );
                if t < 0.3 {
                    frame.line((c.0 - 12, c.1 + 7), (c.0 + 12, c.1 - 7), IVORY);
                }
            }
            EffectKind::Overload => {
                let r = (radius * smoothstep(t * 2.0)) as i32;
                frame.ring(c, (r, r), WATER_LIGHT, true);
                for n in 0..6 {
                    let a = n as f32 * TAU / 6.0;
                    let p = (
                        c.0 + (a.cos() * r as f32) as i32,
                        c.1 + (a.sin() * r as f32) as i32,
                    );
                    let q = (
                        c.0 + (a.cos() * r as f32 * 0.6) as i32 + 3,
                        c.1 + (a.sin() * r as f32 * 0.6) as i32 - 4,
                    );
                    frame.line(c, q, CORE);
                    frame.line(q, p, CORE_LIGHT);
                }
            }
            EffectKind::Seal => {
                let r = (radius * (1.0 - t)) as i32;
                frame.ring(c, (r, r / 2), CORE_LIGHT, true);
            }
            EffectKind::Steam => {
                if !reduced {
                    for n in 0..5 {
                        let rise = ((t + n as f32 * 0.15).fract() * 33.0) as i32;
                        frame.ring(
                            (c.0 - 8 + n * 4, c.1 - rise),
                            (2 + rise / 10, 2),
                            DUST,
                            true,
                        );
                    }
                }
            }
            EffectKind::Catch => {
                if t < 0.45 {
                    gleam(frame, (c.0, c.1 - 6), 5, IVORY);
                }
            }
            EffectKind::Shot => {
                if t < 0.5 {
                    gleam(frame, c, 4, IVORY);
                }
            }
            EffectKind::Armor => {
                frame.line(
                    (c.0 - 5, c.1 + 3),
                    (c.0 + 5, c.1 - 3),
                    if t < 0.4 { WHITE } else { GOLD },
                );
            }
            EffectKind::Victory => {
                if !reduced {
                    let r = (radius * (0.2 + t * 1.4)) as i32;
                    frame.ring(c, (r, r / 2), CORE_LIGHT, true);
                    let spirit = (c.0, c.1 - (smoothstep(t) * 76.0) as i32);
                    for n in 0..6 {
                        let a = n as f32 * TAU / 6.0 + t * TAU;
                        let length = radius * (1.0 - t) * 0.5;
                        let p = (
                            spirit.0 + (a.cos() * length) as i32,
                            spirit.1 + (a.sin() * length * 0.6) as i32,
                        );
                        frame.line(p, spirit, CORE_LIGHT);
                        frame.ellipse(p, (2, 3), IVORY);
                    }
                    gleam(frame, spirit, 7, CORE_LIGHT);
                }
            }
            EffectKind::Defeat => {
                if !reduced {
                    let spirit = (c.0, c.1 - 8 - (t * 22.0) as i32);
                    frame.ring(spirit, (3, 4), IVORY, false);
                }
            }
            EffectKind::Roll | EffectKind::Cross | EffectKind::Sweep => {}
        }
        if reduced {
            continue;
        }
        let count = match effect.kind {
            EffectKind::Victory => 24,
            EffectKind::Rubble | EffectKind::Sever => 18,
            EffectKind::Impact | EffectKind::Overload => 14,
            EffectKind::Roll | EffectKind::Shot | EffectKind::Catch => 4,
            _ => 8,
        };
        for n in 0..count {
            let seed = hash(n, c.0.wrapping_add(c.1 * 11));
            let a = n as f32 * TAU / count as f32 + (seed % 13) as f32 * 0.025;
            let travel =
                radius * (0.1 + (1.0 - (1.0 - t).powi(2)) * (0.7 + (seed % 9) as f32 * 0.06));
            let debris = matches!(
                effect.kind,
                EffectKind::Rubble | EffectKind::Impact | EffectKind::Sever
            );
            let z = if effect.kind == EffectKind::Victory {
                t * 58.0
            } else {
                (t * PI).sin()
                    * if debris {
                        20.0 + (seed % 18) as f32
                    } else {
                        12.0
                    }
            };
            let x = c.0 + (a.cos() * travel) as i32;
            let ground = c.1 + (a.sin() * travel * 0.62) as i32;
            let y = ground - z as i32;
            let color = match effect.kind {
                EffectKind::Victory | EffectKind::Catch | EffectKind::Seal => {
                    if n % 2 == 0 {
                        WHITE
                    } else {
                        CORE_LIGHT
                    }
                }
                EffectKind::Defeat => CAPE,
                EffectKind::Armor | EffectKind::Shot => GOLD,
                EffectKind::Wake | EffectKind::Overload => WATER_LIGHT,
                EffectKind::Cross => CORE,
                EffectKind::Sever => BODY_LIGHT,
                _ => {
                    if n % 3 == 0 {
                        FLOOR_LIGHT
                    } else {
                        DUST
                    }
                }
            };
            if debris {
                let size = 2 + (seed % 3) as i32;
                frame.ellipse((x + 2, ground), (size + 1, 1), SHADOW);
                frame.rect(
                    x,
                    y,
                    size + 1,
                    size,
                    if effect.kind == EffectKind::Sever {
                        BODY_DARK
                    } else {
                        WALL
                    },
                );
                frame.rect(x, y, size + 1, 1, color);
                frame.put(x, y + size, OUTLINE);
            } else if effect.kind == EffectKind::Wake {
                frame.line((x, y), (x + 1, y + 4), color);
            } else {
                frame.rect(x, y, if t < 0.45 { 2 } else { 1 }, 2, color);
            }
        }
    }
}

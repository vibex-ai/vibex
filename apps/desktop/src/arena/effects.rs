//! Combat cues use simulation positions and radii. Decorative motion never
//! changes the footprint of a landing, a dash or an exposed weak point.

use super::{SCALE, pixel};
use crate::arena::{
    combat::{
        Arena, Attack, BossState, EffectKind, Guardian, HazardKind, Phase, ProjectileKind, Vec2,
        WaveKind, smoothstep,
    },
    map::{CENTER, ISLANDS, PLATE_RADIUS, PLATES, SWITCH_RADIUS, SWITCHES},
    raster::{Raster, hash, ink::*},
};
use std::f32::consts::{PI, TAU};

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

/// A committed landing: the solid ring is the exact impact radius and the
/// dashed ring closes onto it as the strike approaches.
fn landing(frame: &mut Raster, c: (i32, i32), r: i32, color: u8, progress: f32) {
    frame.ring(c, (r, r), color, false);
    frame.ring(c, (r - 2, r - 2), WARN_DARK, true);
    let closing = r + ((1.0 - progress.clamp(0.0, 1.0)) * 18.0) as i32;
    frame.ring(c, (closing, closing), IVORY, true);
    for side in [-1, 1] {
        frame.line(
            (c.0 + side * (r + 3), c.1),
            (c.0 + side * (r + 8), c.1),
            color,
        );
        frame.line(
            (c.0, c.1 + side * (r + 3)),
            (c.0, c.1 + side * (r + 8)),
            color,
        );
    }
}

pub(super) fn ground(frame: &mut Raster, arena: &Arena, reduced: bool) {
    if arena.ground_visibility() <= 0.0 {
        return;
    }
    if arena.boss.guardian == Guardian::Codex {
        plates(frame, arena, reduced);
    }
    if arena.boss.guardian == Guardian::ZCode {
        switches(frame, arena);
    }
    for h in &arena.hazards {
        let c = pixel(h.position);
        let r = (h.radius * SCALE).round() as i32;
        match h.kind {
            HazardKind::Goo => {
                let left = h.delay + h.duration - h.time;
                let r = (r as f32 * (0.55 + 0.45 * (left / 0.8).min(1.0))) as i32;
                frame.ellipse(c, (r, r), CORE_DARK);
                for n in 0..5 {
                    let seed = hash(n, c.0 + c.1 * 5);
                    let a = n as f32 * TAU / 5.0 + (seed % 7) as f32 * 0.1;
                    let p = (
                        c.0 + (a.cos() * r as f32 * 0.55) as i32,
                        c.1 + (a.sin() * r as f32 * 0.55) as i32,
                    );
                    frame.ellipse(p, (r / 3 + 1, r / 4 + 1), CORE);
                }
                frame.ring(c, (r, r), CORE, false);
                if !reduced {
                    let bubble = (h.time * 1.7 + (c.0 % 7) as f32 * 0.13).fract();
                    let p = (c.0 + r / 3, c.1 - r / 4);
                    frame.ring(
                        p,
                        ((bubble * 4.0) as i32 + 1, (bubble * 3.0) as i32 + 1),
                        CORE_LIGHT,
                        false,
                    );
                }
                frame.rect(c.0 - r / 2, c.1 - r / 2, 2, 1, CORE_LIGHT);
            }
            HazardKind::Trail => {
                // A block of context: a lit square that blinks as it fades.
                let left = h.delay + h.duration - h.time;
                if left < 1.2 && !reduced && (left * 8.0).fract() < 0.4 {
                    continue;
                }
                let s = r - 2;
                frame.rect(c.0 - s - 1, c.1 - s - 1, s * 2 + 3, s * 2 + 3, CORE_DARK);
                frame.rect(c.0 - s, c.1 - s, s * 2 + 1, s * 2 + 1, CORE);
                frame.rect(c.0 - s + 2, c.1 - s + 2, s, 2, CORE_LIGHT);
            }
            HazardKind::Yarn => {
                // A landed hairball; it twitches before it rolls home.
                let left = h.delay + h.duration - h.time;
                let shake = if left < 0.5 && !reduced {
                    ((h.time * 40.0).sin() * 1.5) as i32
                } else {
                    0
                };
                yarn(frame, (c.0 + shake, c.1 - 4), 0.0);
            }
            HazardKind::Bug => {
                if h.time < h.delay {
                    // The bug, and the ring of its burst closing as it ticks.
                    let progress = (h.time / h.delay.max(0.001)).clamp(0.0, 1.0);
                    let urgent = progress > 0.65;
                    frame.ring(c, (r, r), if urgent { WARN } else { WARN_DARK }, !urgent);
                    let closing = r + ((1.0 - progress) * 16.0) as i32;
                    frame.ring(c, (closing, closing), IVORY, true);
                    bug(
                        frame,
                        c,
                        !reduced && (h.time * (4.0 + progress * 10.0)).fract() < 0.5,
                    );
                }
            }
            HazardKind::Drop | HazardKind::Icicle | HazardKind::Hairball if h.time < h.delay => {
                // The shadow grows under the falling body.
                let progress = (h.time / h.delay.max(0.001)).clamp(0.0, 1.0);
                let shade = (r as f32 * (0.35 + 0.65 * progress)) as i32;
                frame.ellipse((c.0 + 1, c.1 + 1), (shade, (shade * 2 / 3).max(1)), SHADOW);
                let urgent = progress > 0.6;
                frame.ring(c, (r, r), if urgent { WARN } else { WARN_DARK }, !urgent);
            }
            _ => {}
        }
    }
    for beam in &arena.beams {
        if beam.time >= beam.delay {
            continue;
        }
        // The line it will fire down: rails at its exact width, closing in
        // and turning solid as the shot nears.
        let progress = (beam.time / beam.delay.max(0.001)).clamp(0.0, 1.0);
        let urgent = progress > 0.6;
        let color = if urgent { WARN } else { WARN_DARK };
        let side = beam
            .to
            .minus(beam.from)
            .normalized()
            .perpendicular()
            .scale(beam.width + (1.0 - progress) * 3.0);
        for offset in [-1.0, 1.0] {
            let a = beam.from.plus(side.scale(offset));
            let b = beam.to.plus(side.scale(offset));
            if urgent {
                frame.line(pixel(a), pixel(b), color);
            } else {
                let length = b.minus(a).length();
                let dashes = (length / 3.0).ceil().max(1.0) as usize;
                for n in (0..dashes).step_by(2) {
                    frame.line(
                        pixel(a.lerp(b, n as f32 / dashes as f32)),
                        pixel(a.lerp(b, (n as f32 + 1.0) / dashes as f32)),
                        color,
                    );
                }
            }
        }
    }
    if arena.phase == Phase::Battle {
        telegraph(frame, arena);
    }
    for wave in &arena.waves {
        let c = pixel(wave.position);
        let r = (wave.radius * SCALE).round() as i32;
        let (dark, light) = match wave.kind {
            WaveKind::Stone => (FLOOR_SHADE, DUST),
            WaveKind::Water => (WATER, WATER_LIGHT),
        };
        // A wave's ground radius is circular, just like its collision field.
        frame.ring(c, (r + 2, r + 2), dark, false);
        frame.ring(c, (r, r), light, false);
        frame.ring(c, (r - 2, r - 2), dark, true);
        let count = if wave.kind == WaveKind::Water { 28 } else { 16 };
        for n in 0..count {
            let a = n as f32 * TAU / count as f32;
            let p = (
                c.0 + (a.cos() * r as f32) as i32,
                c.1 + (a.sin() * r as f32) as i32,
            );
            if wave.kind == WaveKind::Stone {
                frame.rect(p.0, p.1 - 3, 3, 3, WALL);
                frame.put(p.0, p.1 - 3, DUST);
            } else {
                // Electrified water: crackling arcs ride the ring.
                let jag = if reduced {
                    0
                } else {
                    (hash(n, r) % 5) as i32 - 2
                };
                frame.line(p, (p.0 + 3 + jag, p.1 - 5), WHITE);
                frame.line((p.0 + 3 + jag, p.1 - 5), (p.0 + 1, p.1 - 9), CORE_LIGHT);
            }
        }
    }
}

fn plates(frame: &mut Raster, arena: &Arena, reduced: bool) {
    let lit = arena.boss.plate;
    for (ix, plate) in PLATES.iter().enumerate() {
        let c = pixel(*plate);
        let r = (PLATE_RADIUS * SCALE) as i32;
        let pressed = lit == Some(ix);
        frame.ring(c, (r, r), if pressed { FIRE } else { GOLD_DARK }, false);
        frame.ring(c, (r - 4, r - 4), if pressed { WARN } else { SEAM }, false);
        // A flame glyph marks every plate.
        let color = if pressed { WARN } else { GOLD_DARK };
        frame.polygon(
            &[(c.0 - 5, c.1 + 4), (c.0, c.1 - 7), (c.0 + 5, c.1 + 4)],
            color,
        );
        frame.polygon(
            &[(c.0 - 2, c.1 + 4), (c.0, c.1 - 1), (c.0 + 2, c.1 + 4)],
            if pressed { WHITE } else { SEAM },
        );
        if pressed {
            // Embers run along the channel toward the brazier.
            let flow = if reduced {
                0.5
            } else {
                (arena.visual_time * 1.6).fract()
            };
            for n in 0..6 {
                let t = (n as f32 / 6.0 + flow / 6.0).min(1.0);
                let p = pixel(plate.lerp(CENTER, t));
                frame.rect(p.0 - 1, p.1 - 1, 3, 2, if n % 2 == 0 { FIRE } else { WARN });
            }
        }
    }
}

/// ZCode's two floor switches, lit while a fist holds each down.
fn switches(frame: &mut Raster, arena: &Arena) {
    let boss = &arena.boss;
    for switch in SWITCHES {
        let held = (0..2).any(|ix| boss.pinned[ix] && boss.hands[ix].minus(switch).length() < 1.0);
        let c = pixel(switch);
        let r = (SWITCH_RADIUS * SCALE) as i32;
        frame.ring(c, (r, r), if held { CORE_LIGHT } else { WALL }, false);
        frame.ring(c, (r - 4, r - 4), if held { CORE } else { SEAM }, false);
        // A Z on every switch.
        let color = if held { CORE_LIGHT } else { WALL };
        frame.line((c.0 - 8, c.1 - 7), (c.0 + 8, c.1 - 7), color);
        frame.line((c.0 + 8, c.1 - 7), (c.0 - 8, c.1 + 7), color);
        frame.line((c.0 - 8, c.1 + 7), (c.0 + 8, c.1 + 7), color);
    }
}

fn telegraph(frame: &mut Raster, arena: &Arena) {
    let boss = &arena.boss;
    for shard in boss.shards.iter().filter(|s| s.alive && s.airborne) {
        let r = ((2.0 + 4.0 * shard.size) * SCALE) as i32;
        let progress = shard.time / shard.duration;
        landing(frame, pixel(shard.target), r, WARN_DARK, progress);
    }
    if !matches!(
        boss.state,
        BossState::Windup | BossState::Striking | BossState::Rushing | BossState::Submerged
    ) {
        return;
    }
    let locked = boss.target_locked();
    let color = if locked { WARN } else { WARN_DARK };
    let c = pixel(boss.target);
    let r = (boss.impact_radius() * SCALE).ceil() as i32;
    let progress = boss.progress();
    let aerial = matches!(boss.state, BossState::Windup | BossState::Striking);
    match (boss.guardian, boss.attack) {
        (_, Attack::Hop | Attack::Leap) if aerial => {
            let closing = if boss.state == BossState::Windup {
                progress * 0.3
            } else {
                0.3 + progress * 0.7
            };
            landing(frame, c, r, color, closing);
        }
        (Guardian::Pi | Guardian::ZCode, Attack::LeftFist | Attack::RightFist) if aerial => {
            landing(frame, c, r, color, progress);
            frame.line((c.0 - 4, c.1), (c.0 + 4, c.1), color);
            frame.line((c.0, c.1 - 4), (c.0, c.1 + 4), color);
        }
        (Guardian::Codex, Attack::Rush) if boss.state == BossState::Windup => {
            let end = arena.map.beam_end(boss.position, boss.direction);
            route(frame, boss.position, end, boss.radius(), color);
        }
        (Guardian::Copilot, Attack::Dash) if boss.state != BossState::Striking => {
            let from = if boss.state == BossState::Rushing {
                boss.position
            } else {
                boss.from
            };
            route(frame, from, boss.target, boss.radius(), color);
            landing(frame, c, r, color, progress);
        }
        (Guardian::OpenCode, Attack::Ambush) if boss.state == BossState::Submerged => {
            // The vanished chest still casts the shadow of its return.
            let shade = (r as f32 * (0.5 + 0.5 * progress)) as i32;
            frame.ellipse((c.0 + 2, c.1 + 3), (shade, (shade / 3).max(2)), SHADOW);
            landing(frame, c, r, color, progress);
        }
        (Guardian::OpenCode, Attack::Bite) if aerial => {
            let bite = pixel(boss.target.plus(boss.direction.scale(3.0)));
            landing(frame, bite, r, color, progress);
        }
        (Guardian::OpenCode, Attack::Gape) if boss.state == BossState::Windup => {
            let origin = boss.attack_origin();
            let aim = arena.player.position.minus(origin).angle();
            for n in 0..5 {
                let a = aim + (n as f32 - 2.0) * 0.25;
                let tip = origin.plus(Vec2::from_angle(a).scale(14.0 + progress * 10.0));
                frame.line(
                    pixel(origin.plus(Vec2::from_angle(a).scale(6.0))),
                    pixel(tip),
                    color,
                );
            }
        }
        (Guardian::DeepSeek, Attack::Breach) => {
            if boss.state == BossState::Submerged && !locked {
                // Tracking: the eruption follows the archer until it locks.
                frame.ring(pixel(arena.player.position), (r, r), WARN_DARK, true);
            } else if boss.state == BossState::Submerged {
                landing(frame, c, r, WARN, progress);
                // Where it will burst from the water.
                frame.ring(pixel(boss.position), (20, 20), WATER_LIGHT, true);
            } else if boss.state == BossState::Striking {
                landing(frame, c, r, WARN, progress);
            }
        }
        (Guardian::DeepSeek, Attack::Discharge) => {
            // Dry floes are the only shelter from the coming discharge.
            for (ix, (position, radius)) in ISLANDS.iter().enumerate() {
                if arena.map.sunken & (1 << ix) == 0 {
                    let p = pixel(*position);
                    let rr = (*radius * SCALE) as i32 + 3;
                    frame.ring(p, (rr, rr * 2 / 3), IVORY, false);
                }
            }
            if boss.state == BossState::Striking && boss.time < 1.0 {
                let r = (8.0 + (1.0 - boss.time) * 30.0) as i32;
                frame.ring(pixel(CENTER), (r, r), WATER_LIGHT, true);
            }
        }
        (Guardian::Grok, Attack::Throw) => {
            // The swing the blade will fly, closing on where it lodges.
            let arc: Vec<Vec2> = (0..=12).map(|n| arena.blade_at(n as f32 / 12.0)).collect();
            for (n, pair) in arc.windows(2).enumerate() {
                if boss.state == BossState::Striking || n % 2 == 0 {
                    frame.line(pixel(pair[0]), pixel(pair[1]), color);
                }
            }
            let r = (crate::arena::combat::GROK_BLADE_RADIUS * SCALE) as i32 + 4;
            landing(frame, c, r, color, progress);
        }
        (Guardian::CodeBuddy, Attack::Bomb) if boss.state == BossState::Windup => {
            let r = (crate::arena::combat::CODEBUDDY_BUG_RADIUS * SCALE) as i32;
            landing(frame, c, r, color, progress);
        }
        (Guardian::Devin, Attack::Inhale) if boss.state == BossState::Windup => {
            // Air drawn in to the open mouth.
            let mouth = pixel(boss.body_center());
            for n in 0..8 {
                let a = n as f32 * TAU / 8.0 + progress * 3.0;
                let r = 30.0 * (1.0 - (progress * 4.0 + n as f32 * 0.25).fract());
                let p = (
                    mouth.0 + (a.cos() * r) as i32,
                    mouth.1 + (a.sin() * r * 0.6) as i32,
                );
                frame.rect(p.0, p.1, 2, 2, CORE_LIGHT);
            }
            frame.ring(mouth, (12, 8), CORE, true);
        }
        (Guardian::Gemini, Attack::Volley) if boss.state == BossState::Windup => {
            let p = pixel(boss.attack_origin());
            let r = 3 + (progress * 7.0) as i32;
            frame.ring(p, (r, r), color, false);
            frame.line((p.0 - r - 3, p.1), (p.0 + r + 3, p.1), color);
            frame.line((p.0, p.1 - r - 3), (p.0, p.1 + r + 3), color);
        }
        (Guardian::Devin, Attack::Volley) if boss.state == BossState::Windup => {
            let mouth = pixel(boss.body_center());
            let r = 4 + (progress * 10.0) as i32;
            frame.ring(mouth, (r, r), color, false);
        }
        (Guardian::Kimi, Attack::Charge) if boss.state == BossState::Windup => {
            route(frame, boss.position, boss.target, boss.radius(), color);
        }
        (Guardian::ZCode, Attack::Sweep) if boss.state == BossState::Windup => {
            // The half circle the eye will sweep, west to east through the
            // south, drawn ahead of it as rays that grow as it charges.
            let eye = arena.zcode_eye();
            for n in 0..=8 {
                let a = PI - PI * n as f32 / 8.0;
                let ray = Vec2::from_angle(a);
                frame.line(
                    pixel(eye.plus(ray.scale(10.0))),
                    pixel(eye.plus(ray.scale(12.0 + progress * 14.0))),
                    if n == 0 { WARN } else { color },
                );
            }
        }
        (Guardian::Grok, Attack::Pull) => {
            route(frame, boss.position, boss.target, boss.radius(), WARN);
        }
        (Guardian::Grok, Attack::Frenzy) => {
            let r = (crate::arena::combat::GROK_FRENZY_RADIUS * SCALE) as i32;
            frame.ring(pixel(boss.position), (r, r), WARN, false);
            frame.ring(pixel(boss.position), (r + 3, r + 3), WARN_DARK, true);
        }
        (Guardian::Cline, Attack::Glide) => {
            route(frame, boss.from, boss.target, boss.radius(), WARN_DARK);
        }
        (Guardian::Cline, Attack::Fan) if boss.state == BossState::Windup => {
            // Where the fan will fly: short rays that grow as the eyes charge.
            let origin = boss.attack_origin();
            let aim = boss.target.minus(origin).angle();
            for offset in boss.fan() {
                let ray = Vec2::from_angle(aim + offset);
                frame.line(
                    pixel(origin.plus(ray.scale(8.0))),
                    pixel(origin.plus(ray.scale(10.0 + progress * 14.0))),
                    color,
                );
            }
        }
        (Guardian::Cursor, Attack::Tumble) => {
            route(frame, boss.from, boss.target, boss.radius(), color);
        }
        (Guardian::Cursor, Attack::Laser)
            if boss.state == BossState::Windup && boss.facet == crate::arena::cube::DOWN =>
        {
            // Light leaks out under the cube before the floor blast.
            landing(
                frame,
                pixel(boss.position),
                (7.0 * SCALE) as i32,
                WARN,
                progress,
            );
        }
        (Guardian::Copilot, Attack::Volley) if boss.state == BossState::Windup => {
            let p = pixel(boss.attack_origin());
            let r = 3 + (progress * 6.0) as i32;
            frame.ring(p, (r, r), color, false);
        }
        _ => {}
    }
}

pub(super) fn attacks(frame: &mut Raster, arena: &Arena, reduced: bool) {
    for h in &arena.hazards {
        if h.time >= h.delay
            || !matches!(
                h.kind,
                HazardKind::Drop | HazardKind::Icicle | HazardKind::Hairball
            )
        {
            continue;
        }
        let progress = (h.time / h.delay.max(0.001)).clamp(0.0, 1.0);
        let fall = ((1.0 - progress * progress) * 70.0) as i32;
        let c = pixel(h.position);
        let (x, y) = (c.0, c.1 - fall);
        if h.kind == HazardKind::Drop {
            code_block(frame, (x, y - 4));
        } else if h.kind == HazardKind::Hairball {
            yarn(frame, (x, y - 4), h.time * 8.0);
        } else {
            frame.polygon(&[(x - 4, y - 12), (x + 4, y - 12), (x, y + 2)], WATER_LIGHT);
            frame.polygon(&[(x - 4, y - 12), (x, y - 12), (x, y + 2)], WHITE);
            frame.line((x - 4, y - 12), (x + 4, y - 12), WATER);
        }
    }
    for p in &arena.projectiles {
        let c = pixel(p.position);
        let direction = p.velocity.normalized();
        match p.kind {
            ProjectileKind::Orb => {
                if !reduced {
                    for n in 1..4 {
                        let trail = pixel(p.position.minus(direction.scale(n as f32 * 1.5)));
                        frame.ellipse(
                            trail,
                            (4 - n, 4 - n),
                            if n == 1 { WATER } else { WATER_DARK },
                        );
                    }
                }
                let pulse = if reduced {
                    0
                } else {
                    ((p.age * 18.0).sin() * 1.2) as i32
                };
                frame.ellipse(c, (7 + pulse, 7 + pulse), WATER_DARK);
                frame.ellipse(c, (5 + pulse, 5 + pulse), WATER);
                frame.ellipse((c.0 - 1, c.1 - 1), (3, 3), WATER_LIGHT);
                frame.rect(c.0 - 2, c.1 - 3, 2, 2, WHITE);
                frame.ring(c, (9 + pulse, 9 + pulse), WATER_LIGHT, true);
            }
            ProjectileKind::Bolt => {
                // A streak of eye-light, brightest at its head.
                let tail = p.position.minus(direction.scale(3.4));
                let side = direction.perpendicular().scale(0.3);
                for (offset, color) in [(-1.0, WATER_DARK), (1.0, WATER_DARK), (0.0, WATER_LIGHT)] {
                    frame.line(
                        pixel(tail.plus(side.scale(offset))),
                        pixel(p.position.plus(side.scale(offset))),
                        color,
                    );
                }
                frame.ellipse(c, (3, 3), WATER_LIGHT);
                frame.rect(c.0 - 1, c.1 - 1, 2, 2, WHITE);
            }
            ProjectileKind::Spore => {
                // A puff of spores, swelling and shrinking as it drifts.
                let swell = if reduced {
                    0
                } else {
                    ((p.age * 6.0).sin() * 1.5) as i32
                };
                frame.ellipse((c.0 + 2, c.1 + 4), (6, 2), SHADOW);
                frame.ellipse((c.0, c.1 - 4), (7 + swell, 6 + swell), CORE_DARK);
                frame.ellipse((c.0 - 2, c.1 - 6), (4 + swell / 2, 3), CORE);
                frame.ellipse((c.0 + 3, c.1 - 3), (3, 3), CORE);
                frame.rect(c.0 - 2, c.1 - 8, 2, 2, CORE_LIGHT);
            }
            ProjectileKind::Mote => {
                // A weightless mote in one of the logo's colours.
                let quarter = ((p.velocity.angle() + PI) / (PI * 0.5)) as usize % 4;
                let color = [MOSS, GOLD, CORE, WATER][quarter];
                let bob = if reduced {
                    0
                } else {
                    ((p.age * 9.0).sin() * 1.5) as i32
                };
                frame.ellipse((c.0 + 2, c.1 + 4), (4, 1), SHADOW);
                frame.ellipse((c.0, c.1 - 5 + bob), (4, 4), OUTLINE);
                frame.ellipse((c.0, c.1 - 5 + bob), (3, 3), color);
                frame.rect(c.0 - 1, c.1 - 7 + bob, 2, 2, WHITE);
            }
            ProjectileKind::Yarn => {
                frame.ellipse((c.0 + 2, c.1 + 5), (6, 2), SHADOW);
                yarn(frame, (c.0, c.1 - 2), p.age * 14.0);
            }
            ProjectileKind::Block => {
                let tail = pixel(p.position.minus(direction.scale(3.2)));
                frame.line(tail, c, BODY_DARK);
                frame.ellipse((c.0 + 2, c.1 + 6), (5, 2), SHADOW);
                code_block(frame, c);
            }
        }
    }
    for beam in arena.beams.iter().filter(|b| b.active()) {
        let fade = 1.0 - (beam.time - beam.delay) / beam.duration.max(0.001);
        let side = beam.to.minus(beam.from).normalized().perpendicular();
        let rays = (beam.width * SCALE * fade.max(0.35)) as i32;
        for n in -rays..=rays {
            let offset = side.scale(n as f32 / SCALE);
            let edge = n.abs() as f32 / rays.max(1) as f32;
            let color = if edge < 0.35 {
                WHITE
            } else if edge < 0.75 {
                CORE_LIGHT
            } else {
                CORE
            };
            frame.line(
                pixel(beam.from.plus(offset)),
                pixel(beam.to.plus(offset)),
                color,
            );
        }
        if !reduced {
            let tick = (arena.visual_time * 30.0) as i32;
            for n in 0..6 {
                let seed = hash(n, tick);
                let t = (seed % 97) as f32 / 97.0;
                let p = pixel(beam.from.lerp(beam.to, t));
                frame.rect(p.0 - 1, p.1 - 6 - (seed % 5) as i32, 3, 2, CORE_LIGHT);
            }
        }
        let end = pixel(beam.to);
        frame.ellipse(end, (6, 4), CORE_LIGHT);
        frame.ellipse(end, (3, 2), WHITE);
    }
    if arena.boss.guardian == Guardian::DeepSeek
        && arena.boss.attack == Attack::Discharge
        && arena.boss.state == BossState::Striking
        && !reduced
    {
        // Arcs leap off the submerged body while the charge builds.
        let c = pixel(arena.boss.position);
        let tick = (arena.visual_time * 20.0) as i32;
        for n in 0..6 {
            let seed = hash(n, tick);
            let a = n as f32 * TAU / 6.0 + (seed % 11) as f32 * 0.1;
            let length = 16.0 + (seed % 17) as f32;
            let mid = (
                c.0 + (a.cos() * length * 0.5) as i32 + (seed % 5) as i32 - 2,
                c.1 + (a.sin() * length * 0.3) as i32 - 6,
            );
            let end = (
                c.0 + (a.cos() * length) as i32,
                c.1 + (a.sin() * length * 0.6) as i32 - 2,
            );
            frame.line((c.0, c.1 - 8), mid, WHITE);
            frame.line(mid, end, WATER_LIGHT);
        }
    }
}

/// A hairball: a fuzzy round wad with a strand wound round it.
fn yarn(frame: &mut Raster, c: (i32, i32), turn: f32) {
    frame.ellipse(c, (7, 6), OUTLINE);
    frame.ellipse(c, (6, 5), BODY_LIGHT);
    frame.ellipse((c.0 - 1, c.1 - 1), (3, 2), BODY_GLEAM);
    for n in 0..3 {
        let a = turn + n as f32 * TAU / 3.0;
        let (s, k) = a.sin_cos();
        frame.line(
            (c.0 + (k * 5.0) as i32, c.1 + (s * 4.0) as i32),
            (c.0 - (s * 4.0) as i32, c.1 + (k * 3.0) as i32),
            BODY_DARK,
        );
    }
}

/// CodeBuddy's bug bomb: a round shell with spots, its fuse light blinking
/// faster as it ticks down.
fn bug(frame: &mut Raster, c: (i32, i32), lit: bool) {
    frame.ellipse((c.0 + 2, c.1 + 3), (8, 3), SHADOW);
    frame.ellipse((c.0, c.1 - 4), (8, 6), OUTLINE);
    frame.ellipse((c.0, c.1 - 4), (7, 5), WARN);
    frame.line((c.0, c.1 - 9), (c.0, c.1 + 1), OUTLINE);
    for (dx, dy) in [(-4, -6), (3, -3), (-3, -1), (4, -7)] {
        frame.rect(c.0 + dx, c.1 + dy, 2, 2, OUTLINE);
    }
    frame.ellipse((c.0, c.1 - 10), (3, 2), OUTLINE);
    frame.rect(
        c.0 - 1,
        c.1 - 13,
        2,
        2,
        if lit { CORE_LIGHT } else { CORE_DARK },
    );
}

/// A small code block: a lit cube face marked with angle brackets.
fn code_block(frame: &mut Raster, c: (i32, i32)) {
    frame.rect(c.0 - 5, c.1 - 5, 11, 11, OUTLINE);
    frame.rect(c.0 - 4, c.1 - 4, 9, 4, BODY_LIGHT);
    frame.rect(c.0 - 4, c.1, 9, 4, BODY);
    frame.rect(c.0 - 4, c.1 + 4, 9, 1, BODY_DARK);
    frame.line((c.0 - 1, c.1 - 3), (c.0 - 3, c.1 - 1), BODY_SHADOW);
    frame.line((c.0 - 3, c.1 - 1), (c.0 - 1, c.1 + 1), BODY_SHADOW);
    frame.line((c.0 + 1, c.1 - 3), (c.0 + 3, c.1 - 1), BODY_SHADOW);
    frame.line((c.0 + 3, c.1 - 1), (c.0 + 1, c.1 + 1), BODY_SHADOW);
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
                if t < 0.1 {
                    frame.ellipse(c, ((radius * 0.5) as i32, (radius * 0.25) as i32), IVORY);
                }
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
            EffectKind::Burst => {
                let r = (radius * smoothstep(t * 1.6)) as i32;
                frame.ring(c, (r, r), CORE_LIGHT, t > 0.4);
                if t < 0.35 {
                    for n in 0..10 {
                        let a = n as f32 * TAU / 10.0 + 0.2;
                        let inner = radius * 0.2;
                        let outer = radius * (0.5 + t * 1.4);
                        frame.line(
                            (
                                c.0 + (a.cos() * inner) as i32,
                                c.1 + (a.sin() * inner) as i32,
                            ),
                            (
                                c.0 + (a.cos() * outer) as i32,
                                c.1 + (a.sin() * outer) as i32,
                            ),
                            if n % 2 == 0 { WHITE } else { CORE_LIGHT },
                        );
                    }
                }
            }
            EffectKind::Spark => {
                if !reduced {
                    let tick = (arena.visual_time * 24.0) as i32;
                    for n in 0..4 {
                        let seed = hash(n, tick + c.0);
                        let a = n as f32 * TAU / 4.0 + (seed % 13) as f32 * 0.12;
                        let length = radius * (0.5 + (seed % 7) as f32 * 0.07) * (1.0 - t * 0.5);
                        let mid = (
                            c.0 + (a.cos() * length * 0.5) as i32 + (seed % 5) as i32 - 2,
                            c.1 + (a.sin() * length * 0.5) as i32 - (seed % 3) as i32,
                        );
                        let end = (
                            c.0 + (a.cos() * length) as i32,
                            c.1 + (a.sin() * length) as i32,
                        );
                        frame.line(c, mid, WHITE);
                        frame.line(mid, end, WATER_LIGHT);
                    }
                }
            }
            EffectKind::Dust => {
                if !reduced {
                    for n in 0..3 {
                        let seed = hash(n, c.0 * 3 + c.1);
                        let drift = (n as f32 - 1.0) * radius * 0.5 * t;
                        let rise = (t * 6.0) as i32;
                        let size = (2.0 + radius * 0.25 * (1.0 - t)) as i32;
                        frame.ellipse(
                            (c.0 + drift as i32 + (seed % 3) as i32 - 1, c.1 - rise),
                            (size, (size * 2 / 3).max(1)),
                            if t < 0.5 { FLOOR_LIGHT } else { DUST },
                        );
                    }
                }
            }
            EffectKind::Ignite => {
                let tick = (arena.visual_time * 30.0) as i32;
                for n in 0..7 {
                    let seed = hash(n, tick + c.1);
                    let x = c.0 + ((n as f32 - 3.0) * radius * 0.18) as i32;
                    let height = ((radius * 0.9 + (seed % 6) as f32) * (1.0 - t)) as i32;
                    frame.polygon(
                        &[(x - 2, c.1), (x, c.1 - height), (x + 2, c.1)],
                        if n % 2 == 0 { FIRE } else { WARN },
                    );
                }
                frame.rect(c.0 - 1, c.1 - (radius * 0.5) as i32, 2, 3, WHITE);
            }
            EffectKind::Flash => {
                let r = (radius * (0.2 + t * 1.3)) as i32;
                frame.ring(c, (r, r), WHITE, false);
                frame.ring(c, (r + 3, r + 3), CORE_LIGHT, t > 0.3);
                if t < 0.5 {
                    for n in 0..12 {
                        let a = n as f32 * TAU / 12.0;
                        let outer = radius * (0.6 + t * 1.6);
                        frame.line(
                            c,
                            (
                                c.0 + (a.cos() * outer) as i32,
                                c.1 + (a.sin() * outer) as i32,
                            ),
                            WHITE,
                        );
                    }
                }
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
            EffectKind::Roll => {}
        }
        if reduced {
            continue;
        }
        let count = match effect.kind {
            EffectKind::Victory => 24,
            EffectKind::Rubble | EffectKind::Burst => 18,
            EffectKind::Impact | EffectKind::Flash => 14,
            EffectKind::Ignite | EffectKind::Spark => 8,
            EffectKind::Roll | EffectKind::Shot | EffectKind::Catch => 4,
            EffectKind::Dust | EffectKind::Wake => 0,
            _ => 8,
        };
        for n in 0..count {
            let seed = hash(n, c.0.wrapping_add(c.1 * 11));
            let a = n as f32 * TAU / count as f32 + (seed % 13) as f32 * 0.025;
            let travel =
                radius * (0.1 + (1.0 - (1.0 - t).powi(2)) * (0.7 + (seed % 9) as f32 * 0.06));
            let debris = matches!(effect.kind, EffectKind::Rubble | EffectKind::Impact);
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
                EffectKind::Victory | EffectKind::Catch | EffectKind::Flash => {
                    if n % 2 == 0 {
                        WHITE
                    } else {
                        CORE_LIGHT
                    }
                }
                EffectKind::Defeat => CAPE,
                EffectKind::Armor | EffectKind::Shot => GOLD,
                EffectKind::Spark => WATER_LIGHT,
                EffectKind::Burst => {
                    if n % 3 == 0 {
                        WHITE
                    } else {
                        CORE
                    }
                }
                EffectKind::Ignite => {
                    if n % 2 == 0 {
                        FIRE
                    } else {
                        WARN
                    }
                }
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
                frame.rect(x, y, size + 1, size, WALL);
                frame.rect(x, y, size + 1, 1, color);
                frame.put(x, y + size, OUTLINE);
            } else {
                frame.rect(x, y, if t < 0.45 { 2 } else { 1 }, 2, color);
            }
        }
    }
}

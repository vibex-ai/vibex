use super::*;
use crate::arena::map::{ISLANDS, SEALS};

#[derive(Default)]
struct Pilot {
    flying_frames: usize,
}

impl Pilot {
    fn tick(&mut self, arena: &mut Arena) {
        if arena.phase != Phase::Battle {
            arena.tick(Controls::default());
            return;
        }
        let boss = arena.boss;
        let player = arena.player.position;
        let outward = player.minus(boss.position).normalized();
        let mut goal = boss.position.plus(outward.scale(28.0));
        let mut target = (boss.exposed > 0.6).then_some(boss.core);
        match boss.guardian {
            Guardian::Claude if target.is_none() => {
                target = (0..6)
                    .filter(|ix| boss.tentacle_vulnerable(*ix))
                    .map(|ix| boss.tentacle_joint(ix))
                    .min_by(|a, b| {
                        a.minus(player)
                            .length()
                            .total_cmp(&b.minus(player).length())
                    });
            }
            Guardian::Pi => {
                if target.is_some() {
                    goal = CENTER.plus(Vec2::new(0.0, 18.0));
                } else {
                    let ix = usize::from(boss.seals & 1 != 0);
                    goal = SEALS[ix];
                    let correct = (ix == 0 && boss.attack == Attack::LeftFist)
                        || (ix == 1 && boss.attack == Attack::RightFist);
                    if correct
                        && boss.state == BossState::Windup
                        && boss.progress() > 0.6
                        && boss.target.minus(SEALS[ix]).length() < 5.0
                    {
                        goal = if boss.seals == 0 {
                            SEALS[1]
                        } else {
                            CENTER.plus(Vec2::new(0.0, 18.0))
                        };
                    }
                }
            }
            Guardian::OpenCode => {
                if boss.is_inhaling() || target.is_some() {
                    goal = boss.core.plus(boss.direction.scale(27.0));
                    if player.minus(boss.core).dot(boss.direction) > 8.0 && boss.is_inhaling() {
                        target = Some(boss.core);
                    }
                }
            }
            Guardian::DeepSeek => {
                let island = ISLANDS
                    .iter()
                    .enumerate()
                    .filter(|(ix, _)| {
                        arena.map.sunken & (1 << ix) == 0 || arena.map.sunken == 0b1111
                    })
                    .min_by(|(_, a), (_, b)| {
                        a.0.minus(player)
                            .length()
                            .total_cmp(&b.0.minus(player).length())
                    })
                    .map(|(_, island)| *island)
                    .unwrap();
                let returning_to_land = boss.attack == Attack::Pulse
                    || boss.state == BossState::Recovery
                    || boss.state == BossState::Striking
                    || boss.state == BossState::Windup && boss.progress() >= 0.55;
                goal = if returning_to_land {
                    island.0
                } else {
                    island
                        .0
                        .plus(CENTER.minus(island.0).normalized().scale(island.1 + 13.0))
                };
                if !arena.map.on_island(player) {
                    target = None;
                }
            }
            Guardian::Copilot if target.is_none() => {
                target = arena
                    .projectiles
                    .iter()
                    .find(|p| p.kind == ProjectileKind::ChargedOrb)
                    .map(|p| p.position);
            }
            _ => {}
        }
        let mut movement = steer(arena, goal);
        let mut controls = Controls::default();
        let dodge = danger_direction(arena, goal);
        if let Some(direction) = dodge {
            movement = direction;
            if arena.player.roll_cooldown <= 0.0 {
                arena.roll(direction);
            }
        }
        if arena.arrow.state == ArrowState::Ready {
            self.flying_frames = 0;
            if let Some(target) = target {
                let distance = target.minus(player).length();
                let near = target.minus(target.minus(player).normalized().scale(CORE_RADIUS + 0.1));
                if dodge.is_none()
                    && distance < 65.0
                    && distance > 3.0
                    && player.minus(boss.position).length() > boss.radius() + 3.0
                    && !arena.map.cover_hit(player, near)
                    && arena.player.roll_remaining <= 0.0
                {
                    controls.aim = Some(target);
                    controls.shoot = true;
                    movement = Vec2::default();
                    if arena.player.charge >= 0.34 {
                        arena.release_shot(Some(target));
                        controls.shoot = false;
                    }
                }
            }
        } else if arena.arrow.state == ArrowState::Flying {
            self.flying_frames += 1;
            controls.recall = self.flying_frames > 65;
        } else {
            controls.recall = dodge.is_none();
        }
        controls.movement = movement;
        arena.tick(controls);
    }
}

fn steer(arena: &Arena, goal: Vec2) -> Vec2 {
    let player = arena.player.position;
    if goal.minus(player).length() < 1.0 {
        return Vec2::default();
    }
    let desired = goal.minus(player).angle();
    (0..16)
        .map(|ix| {
            let direction = Vec2::from_angle(desired + ix as f32 * TAU / 16.0);
            let target = player.plus(direction.scale(4.0));
            let projected = arena.map.move_body(player, target, PLAYER_RADIUS);
            let blocked = target.minus(projected).length() * 8.0;
            let separation = projected.minus(arena.boss.position).length();
            let contact = if arena.boss.airborne() {
                0.0
            } else {
                (arena.boss.radius() + 5.0 - separation).max(0.0) * 4.0
            };
            (
                direction,
                projected.minus(goal).length() + blocked + contact,
            )
        })
        .min_by(|a, b| a.1.total_cmp(&b.1))
        .unwrap()
        .0
}

fn danger_direction(arena: &Arena, goal: Vec2) -> Option<Vec2> {
    let player = arena.player.position;
    for wave in &arena.waves {
        if wave.kind == WaveKind::Water && arena.map.on_island(player) {
            continue;
        }
        let gap = player.minus(wave.position).length() - wave.radius;
        if (-2.0..5.0).contains(&gap) {
            return Some(wave.position.minus(player).normalized());
        }
    }
    for hazard in &arena.hazards {
        if hazard.kind == HazardKind::Gravity || hazard.time < hazard.delay - 0.2 {
            continue;
        }
        if segment_distance(player, hazard.position, hazard.end) < hazard.radius + 3.0 {
            let direction = hazard
                .end
                .minus(hazard.position)
                .normalized()
                .perpendicular();
            let side = if direction.dot(goal.minus(player)) < 0.0 {
                -1.0
            } else {
                1.0
            };
            return Some(direction.scale(side));
        }
    }
    for p in &arena.projectiles {
        if p.kind == ProjectileKind::ChargedOrb && p.age < 1.0 {
            continue;
        }
        if segment_distance(player, p.position, p.position.plus(p.velocity.scale(0.24))) < 2.6 {
            let direction = p.velocity.normalized().perpendicular();
            let side = if direction.dot(goal.minus(player)) < 0.0 {
                -1.0
            } else {
                1.0
            };
            return Some(direction.scale(side));
        }
    }
    let b = arena.boss;
    if b.state == BossState::Windup
        && b.progress() > 0.56
        && matches!(b.attack, Attack::Sweep | Attack::VisorBeam)
        && segment_distance(player, b.attack_origin(), b.beam_end) < 4.0
    {
        return Some(b.direction.perpendicular());
    }
    if b.state == BossState::Rushing && player.minus(b.position).length() < 18.0 {
        return Some(b.direction.perpendicular());
    }
    let tells =
        (b.state == BossState::Windup && b.progress() > 0.67) || b.state == BossState::Striking;
    if tells
        && matches!(
            b.attack,
            Attack::LeftFist | Attack::RightFist | Attack::Leap | Attack::Stomp | Attack::Dash
        )
        && player.minus(b.target).length() < b.impact_radius() + 7.0
    {
        let away = player.minus(b.target);
        return Some(if away.length() < 1.0 {
            goal.minus(player).normalized()
        } else {
            away.normalized()
        });
    }
    if tells && b.attack == Attack::TendrilSweep {
        for ix in b.ward as usize * 2..b.ward as usize * 2 + 2 {
            if b.severed & (1 << ix) == 0
                && player.minus(b.tentacles[ix]).length() < b.impact_radius() + 4.0
            {
                return Some(player.minus(b.tentacles[ix]).normalized());
            }
        }
    }
    None
}

/// The pilot reads visible tells and sends ordinary input only. It never
/// changes positions, timers, armor, health, phase, damage or the random seed.
#[test]
fn every_guardian_is_winnable_through_ordinary_inputs_across_spawn_seeds() {
    for guardian in Guardian::ALL {
        for seed in [1, 17, 127] {
            let mut arena = Arena::from_preview(Boss::new(guardian), 0.0, seed);
            let mut pilot = Pilot::default();
            for _ in 0..24_000 {
                if arena.phase == Phase::Victory {
                    break;
                }
                pilot.tick(&mut arena);
            }
            assert_eq!(
                arena.phase,
                Phase::Victory,
                "{guardian:?} seed {seed}: deaths={} player={:?} boss={:?} arrow={:?}",
                arena.deaths,
                arena.player.position,
                arena.boss,
                arena.arrow
            );
        }
    }
}

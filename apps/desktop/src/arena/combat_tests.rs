use super::*;
use crate::arena::map::{CENTER, ISLANDS};

#[test]
fn flight_entry_and_takeoff_keep_height_continuous_and_terminal_turns_finish_upright() {
    for guardian in [Guardian::Copilot, Guardian::OpenCode] {
        let mut arena = Arena::from_preview(Boss::idle(guardian, 3.7, false), 3.7, 17);
        arena.player.invulnerable = 120.0;
        let mut landings = 0;
        for _ in 0..2400 {
            let before = arena.boss;
            let phase = arena.phase;
            arena.tick(Controls::default());
            if guardian == Guardian::Copilot && phase == Phase::Awakening {
                assert!((arena.boss.height - before.height).abs() < 0.25);
            }
            if before.state != arena.boss.state {
                assert!(
                    (arena.boss.height - before.height).abs() < 2.0,
                    "{guardian:?}: {:?} -> {:?}",
                    before.state,
                    arena.boss.state
                );
                if guardian == Guardian::OpenCode && before.state == BossState::Striking {
                    let angular_step = (arena.boss.pitch - before.pitch).sin().abs();
                    assert!(angular_step < 0.25);
                    assert!((arena.boss.pitch - before.pitch).cos() > 0.9);
                    landings += 1;
                }
            }
        }
        if guardian == Guardian::OpenCode {
            assert!(landings >= 4);
        }
    }
}
#[test]
fn entry_spawn_avoids_where_the_guardian_will_settle() {
    let mut boss = Boss::new(Guardian::Pi);
    boss.position = Map::new(Guardian::Pi).pillars()[1].position;
    boss.previous_position = boss.position;
    boss.update_core();
    for seed in 1..128 {
        let mut arena = Arena::from_preview(boss, 0.0, seed);
        wait_out_intro(&mut arena);
        assert!(arena.player.position.minus(arena.boss.position).length() > 29.0);
    }
}

fn ticks(arena: &mut Arena, count: usize, controls: Controls) {
    for _ in 0..count {
        arena.tick(controls);
    }
}
fn battle(guardian: Guardian) -> Arena {
    let mut arena = Arena::new(guardian, 0.0);
    ticks(&mut arena, 86, Controls::default());
    assert_eq!(arena.phase, Phase::Battle);
    arena
}
fn wait_out_intro(arena: &mut Arena) {
    for _ in 0..180 {
        if arena.phase == Phase::Battle {
            return;
        }
        arena.tick(Controls::default());
    }
    panic!("intro did not end");
}

#[test]
fn agent_identity_selects_only_the_six_supported_guardians() {
    for (id, g) in [
        ("claude", Guardian::Claude),
        ("codex", Guardian::Codex),
        ("pi", Guardian::Pi),
        ("opencode", Guardian::OpenCode),
        ("deepseek-harness", Guardian::DeepSeek),
        ("copilot", Guardian::Copilot),
    ] {
        assert_eq!(Guardian::from_agent(id), Some(g));
    }
    for id in ["gemini", "qwen-code", "custom-agent", ""] {
        assert_eq!(Guardian::from_agent(id), None);
    }
}
#[test]
fn entry_keeps_the_measured_roaming_pose_and_rejects_gameplay_until_awake() {
    for g in Guardian::ALL {
        let boss = Boss::idle(g, 8.4, false);
        let mut arena = Arena::from_preview(boss, 8.4, 123);
        assert_eq!(arena.boss.position, boss.position);
        assert_eq!(arena.boss.yaw, boss.yaw);
        assert_eq!(arena.boss.height, boss.height);
        assert_eq!(arena.visual_time, 8.4);
        let start = arena.player.position;
        for _ in 0..45 {
            arena.roll(Vec2::new(1.0, 0.0));
            arena.tick(Controls {
                movement: Vec2::new(1.0, 0.0),
                shoot: true,
                recall: true,
                aim: None,
            });
            arena.release_shot(None);
        }
        assert_eq!(arena.player.position, start);
        assert_eq!(arena.arrow.state, ArrowState::Ready);
        assert_eq!(arena.player.charge, 0.0);
        assert!(arena.hazards.is_empty());
        wait_out_intro(&mut arena);
        assert!(arena.map.is_clear(arena.boss.position, arena.boss.radius()));
    }
}
#[test]
fn one_arrow_needs_a_full_draw_and_can_always_be_recalled() {
    let mut arena = battle(Guardian::Pi);
    arena.boss.enter(BossState::Watching, 10.0);
    ticks(
        &mut arena,
        8,
        Controls {
            shoot: true,
            ..Controls::default()
        },
    );
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    ticks(
        &mut arena,
        22,
        Controls {
            shoot: true,
            ..Controls::default()
        },
    );
    arena.release_shot(Some(Vec2::new(170.0, 110.0)));
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    let arrow = arena.arrow.position;
    arena.release_shot(None);
    assert_eq!(arena.arrow.position, arrow);
    ticks(
        &mut arena,
        90,
        Controls {
            recall: true,
            ..Controls::default()
        },
    );
    assert_eq!(arena.arrow.state, ArrowState::Ready);
}
#[test]
fn drawing_and_recalling_hold_position_and_roll_cancels_the_draw() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.enter(BossState::Watching, 10.0);
    let start = arena.player.position;
    ticks(
        &mut arena,
        24,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            shoot: true,
            ..Controls::default()
        },
    );
    assert_eq!(arena.player.position, start);
    arena.roll(Vec2::new(1.0, 0.0));
    assert_eq!(arena.player.charge, 0.0);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    ticks(&mut arena, 30, Controls::default());
    arena.arrow.state = ArrowState::Lodged;
    arena.arrow.position = Vec2::new(165.0, 104.0);
    let start = arena.player.position;
    ticks(
        &mut arena,
        4,
        Controls {
            recall: true,
            movement: Vec2::new(0.0, 1.0),
            ..Controls::default()
        },
    );
    assert_eq!(arena.player.position, start);
    arena.cancel_input();
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
}
#[test]
fn movement_slides_along_terrain_without_crossing_columns_or_the_rim() {
    for guardian in Guardian::ALL {
        let map = Map::new(guardian);
        let mut p = CENTER;
        for _ in 0..300 {
            p = map.move_body(p, p.plus(Vec2::new(1.0, 0.7)), 0.9);
            assert!(map.is_clear(p, 0.9));
        }
        for pillar in map.pillars() {
            let from = pillar.position.minus(Vec2::new(12.0, 0.0));
            let to = pillar.position.plus(Vec2::new(12.0, 0.0));
            let result = map.move_body(from, to, 0.9);
            assert!(result.x < pillar.position.x);
        }
    }
}
#[test]
fn every_attack_commits_its_target_before_the_strike() {
    for g in Guardian::ALL {
        let mut arena = battle(g);
        arena.player.position = Vec2::new(94.0, 83.0);
        arena.boss.prepare(arena.player.position);
        arena.boss.time = arena.boss.duration * 0.73;
        let target = arena.boss.target;
        arena.player.position = Vec2::new(42.0, 68.0);
        arena.tick_boss();
        assert_eq!(arena.boss.target, target, "{g:?}");
    }
}
#[test]
fn fists_have_separate_ground_anchors_and_the_double_slam_opens_the_chest() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.prepare(Vec2::new(94.0, 70.0));
    arena.boss.time = 0.6;
    arena.tick_boss();
    assert!(arena.boss.hand_heights[0] > arena.boss.hand_heights[1] + 5.0);
    assert_ne!(arena.boss.hands[0], arena.boss.hands[1]);
    assert_eq!(arena.boss.exposed, 0.0);
    arena.boss.attacks = 2;
    arena.boss.prepare(Vec2::new(94.0, 70.0));
    arena.boss.time = arena.boss.duration;
    arena.tick_boss();
    arena.boss.time = arena.boss.duration;
    arena.player.position = Vec2::new(45.0, 50.0);
    arena.tick_boss();
    assert!(arena.boss.exposed > 2.0);
    assert_eq!(arena.hazards.len(), 2);
    assert!(
        arena
            .hazards
            .iter()
            .all(|h| h.kind == HazardKind::Fissure && !h.active())
    );
}
#[test]
fn the_rolling_knot_breaks_cover_and_perimeter_impacts_keep_it_beatable() {
    let mut arena = battle(Guardian::Codex);
    let pillar = arena.map.pillars()[0];
    arena.boss.position = pillar.position.minus(Vec2::new(10.4, 0.0));
    arena.boss.direction = Vec2::new(1.0, 0.0);
    arena.boss.attack = Attack::Rush;
    arena.boss.enter(BossState::Rushing, 3.0);
    arena.tick_boss();
    assert!(!arena.map.intact(0));
    assert!(arena.boss.exposed > 2.5);
    assert!(
        arena
            .boss
            .core
            .minus(arena.boss.position)
            .dot(arena.boss.direction)
            < 0.0
    );
    arena.map.broken = 0xff;
    arena.boss.position = Vec2::new(146.0, 58.0);
    arena.boss.exposed = 0.0;
    arena.boss.enter(BossState::Rushing, 3.0);
    for _ in 0..60 {
        arena.tick_boss();
        if arena.boss.exposed > 0.0 {
            break;
        }
    }
    assert!(arena.boss.exposed > 0.0);
}
#[test]
fn the_mages_seal_requires_a_returning_arrow() {
    let mut arena = battle(Guardian::Pi);
    arena.boss.height = 0.0;
    arena.boss.opening(2.0);
    arena.boss.update_core();
    let core = arena.boss.core;
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: core.minus(Vec2::new(0.0, 2.0)),
        velocity: Vec2::new(0.0, 180.0),
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Battle);
    arena.arrow = Arrow {
        state: ArrowState::Returning,
        position: core.minus(Vec2::new(0.0, 1.0)),
        velocity: Vec2::new(0.0, 120.0),
    };
    arena.player.position = core.plus(Vec2::new(0.0, 24.0));
    arena.tick_arrow(true);
    assert_eq!(arena.phase, Phase::Victory);
}
#[test]
fn open_armor_still_requires_hitting_the_specific_core() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.opening(2.0);
    let core = arena.boss.core;
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: core.plus(Vec2::new(3.5, -1.0)),
        velocity: Vec2::new(0.0, 120.0),
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Battle);
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: core.plus(Vec2::new(0.0, -1.0)),
        velocity: Vec2::new(0.0, 120.0),
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}
#[test]
fn shutter_beams_stop_at_physical_cover_and_the_opening_is_directional() {
    let mut arena = battle(Guardian::OpenCode);
    let origin = Vec2::new(51.0, 20.0);
    let end = arena.map.beam_end(origin, Vec2::new(0.0, 1.0));
    assert!(end.y < 34.0);
    arena.player.invulnerable = 0.0;
    arena.player.position = Vec2::new(51.0, 55.0);
    arena.hazard(HazardKind::Beam, origin, end, 0.0, 1.0, 1.4);
    arena.tick_threats(arena.player.position);
    assert_eq!(arena.phase, Phase::Battle);
    arena.player.position = Vec2::new(51.0, 24.0);
    arena.tick_threats(arena.player.position);
    assert_eq!(arena.phase, Phase::Defeat);
    let mut arena = battle(Guardian::OpenCode);
    arena.boss.opening(2.0);
    arena.boss.direction = Vec2::new(0.0, 1.0);
    assert!(arena.accepts_core_hit(arena.boss.core.plus(Vec2::new(0.0, 8.0)), false));
    assert!(!arena.accepts_core_hit(arena.boss.core.minus(Vec2::new(0.0, 8.0)), false));
}
#[test]
fn water_islands_are_cover_from_waves_but_not_from_a_breach() {
    let mut arena = battle(Guardian::DeepSeek);
    let island = ISLANDS[0].0;
    arena.player.position = island;
    arena.player.invulnerable = 0.0;
    arena.wave(
        island.minus(Vec2::new(10.0, 0.0)),
        10.0,
        35.0,
        WaveKind::Water,
    );
    arena.tick_threats(island);
    assert_eq!(arena.phase, Phase::Battle);
    arena.impact(island, 7.0);
    assert_eq!(arena.phase, Phase::Defeat);
}
#[test]
fn fast_feathers_use_swept_collision_and_telegraphs_are_safe() {
    let mut arena = battle(Guardian::Copilot);
    arena.player.position = CENTER;
    arena.player.invulnerable = 0.0;
    arena.hazard(HazardKind::Geyser, CENTER, CENTER, 0.5, 0.2, 6.0);
    arena.tick_threats(CENTER);
    assert_eq!(arena.phase, Phase::Battle);
    arena.projectile(
        CENTER.minus(Vec2::new(12.0, 0.0)),
        Vec2::new(1500.0, 0.0),
        ProjectileKind::Feather,
    );
    arena.tick_threats(CENTER);
    assert_eq!(arena.phase, Phase::Defeat);
    assert!(arena.hazards.is_empty());
    arena.wave(CENTER, 1.0, 30.0, WaveKind::Stone);
    assert!(arena.waves.is_empty());
}
#[test]
fn death_automatically_returns_at_random_clear_positions_without_restarting_entry() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        let mut positions = Vec::new();
        for _ in 0..12 {
            arena.player.invulnerable = 0.0;
            arena.hurt();
            assert_eq!(arena.phase, Phase::Defeat);
            ticks(&mut arena, 62, Controls::default());
            assert_eq!(arena.phase, Phase::Rebirth);
            assert!(arena.map.is_clear(arena.player.position, 3.0));
            assert!(arena.player.position.minus(arena.boss.position).length() > 29.0);
            assert_eq!(arena.arrow.state, ArrowState::Ready);
            positions.push(arena.player.position);
            ticks(&mut arena, 44, Controls::default());
            assert_eq!(arena.phase, Phase::Battle);
            assert!(arena.player.invulnerable > 0.0);
        }
        assert!(positions.windows(2).all(|p| p[0] != p[1]));
        assert_eq!(arena.deaths, 12);
    }
}
#[test]
fn victory_has_a_finite_departure_and_clears_all_live_threats() {
    let mut arena = battle(Guardian::Pi);
    arena.projectile(CENTER, Vec2::new(1.0, 0.0), ProjectileKind::Rune);
    arena.wave(CENTER, 1.0, 50.0, WaveKind::Magic);
    arena.win();
    assert!(arena.projectiles.is_empty() && arena.waves.is_empty() && arena.hazards.is_empty());
    ticks(&mut arena, 140, Controls::default());
    assert!(!arena.outcome_ready());
    ticks(&mut arena, 70, Controls::default());
    assert!(arena.outcome_ready());
    assert!(!arena.needs_tick());
    assert_eq!(arena.home_opacity(), 1.0);
    let time = arena.visual_time;
    ticks(&mut arena, 120, Controls::default());
    assert_eq!(arena.visual_time, time);
}
#[test]
fn camera_follows_continuously_and_roaming_is_not_a_stationary_breathing_loop() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.enter(BossState::Watching, 50.0);
    arena.player.position = CENTER;
    arena.camera = CENTER;
    let before = arena.camera;
    arena.tick(Controls {
        movement: Vec2::new(1.0, 0.0),
        ..Controls::default()
    });
    assert!(arena.camera.minus(before).length() < 0.5);
    let start = arena.camera;
    ticks(
        &mut arena,
        75,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            ..Controls::default()
        },
    );
    assert!(arena.camera.x > start.x + 10.0);
    for g in Guardian::ALL {
        let a = Boss::idle(g, 2.0, false);
        let b = Boss::idle(g, 15.0, false);
        assert!(a.position.minus(b.position).length() > 15.0, "{g:?}");
    }
}

/// This pilot uses the public movement/draw/recall/roll inputs. It gets no
/// invulnerability, position writes or forced phase changes, including retries.
#[test]
fn every_guardian_is_winnable_through_ordinary_inputs_and_automatic_rebirth() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 0.0);
        for frame in 0..48_000 {
            if arena.phase == Phase::Victory {
                break;
            }
            if arena.phase != Phase::Battle {
                arena.tick(Controls::default());
                continue;
            }
            let to_boss = arena.boss.position.minus(arena.player.position);
            let distance = to_boss.length();
            let open = arena.boss.exposed > 0.55;
            let shoot_side = match guardian {
                Guardian::Codex => arena.boss.direction.scale(-1.0),
                Guardian::OpenCode => arena.boss.direction,
                _ => to_boss.normalized().scale(-1.0),
            };
            let desired = arena.boss.core.plus(shoot_side.scale(23.0));
            let to_desired = desired.minus(arena.player.position);
            let mut movement = if open {
                to_desired.normalized()
            } else {
                to_boss
                    .normalized()
                    .perpendicular()
                    .plus(
                        to_boss
                            .normalized()
                            .scale(((distance - 26.0) / 10.0).clamp(-1.0, 1.0)),
                    )
                    .normalized()
            };
            let aligned = match guardian {
                Guardian::Codex => {
                    arena
                        .player
                        .position
                        .minus(arena.boss.position)
                        .dot(arena.boss.direction)
                        < -8.0
                }
                Guardian::OpenCode => {
                    arena
                        .player
                        .position
                        .minus(arena.boss.core)
                        .dot(arena.boss.direction)
                        > 8.0
                }
                _ => true,
            };
            let wave_near = arena.waves.iter().any(|wave| {
                let gap = arena.player.position.minus(wave.position).length() - wave.radius;
                gap > -2.0 && gap < 8.0
            });
            let attack = open && aligned && distance > 11.0 && distance < 51.0 && !wave_near;
            let mut controls = Controls::default();
            if arena.arrow.state == ArrowState::Ready
                && attack
                && arena.player.roll_remaining <= 0.0
            {
                controls.shoot = true;
                movement = Vec2::default();
                if arena.player.charge >= 0.37 {
                    arena.release_shot(None);
                    controls.shoot = false;
                }
            } else if arena.arrow.state != ArrowState::Ready {
                if arena.arrow.state == ArrowState::Flying {
                    let beyond = arena
                        .arrow
                        .position
                        .minus(arena.boss.core)
                        .dot(arena.arrow.velocity.normalized());
                    if beyond > 5.0 {
                        controls.recall = true;
                    }
                    if guardian == Guardian::Pi {
                        movement = Vec2::default();
                    }
                } else {
                    controls.recall = true;
                }
            }
            if wave_near && arena.player.roll_cooldown <= 0.0 {
                let wave = arena.waves.first().unwrap();
                movement = wave.position.minus(arena.player.position).normalized();
                controls.shoot = false;
                controls.recall = false;
                arena.roll(movement);
            } else if !controls.shoot && !controls.recall && frame % 33 == 0 {
                arena.roll(movement);
            }
            controls.movement = movement;
            arena.tick(controls);
        }
        assert_eq!(
            arena.phase,
            Phase::Victory,
            "{guardian:?} was not defeated; deaths={}, position={:?}, boss={:?}, arrow={:?}",
            arena.deaths,
            arena.player.position,
            arena.boss,
            arena.arrow
        );
    }
}
#[test]
fn long_running_encounters_bound_effects_and_keep_all_coordinates_finite() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 0.0);
        for frame in 0..9000 {
            let direction = Vec2::from_angle(frame as f32 * 0.025);
            if frame % 37 == 0 {
                arena.roll(direction);
            }
            arena.tick(Controls {
                movement: direction,
                ..Controls::default()
            });
            assert!(
                arena.projectiles.len() <= MAX_PROJECTILES
                    && arena.waves.len() <= MAX_WAVES
                    && arena.effects.len() <= MAX_EFFECTS
                    && arena.hazards.len() <= MAX_HAZARDS
            );
            assert!(
                arena.player.position.x.is_finite()
                    && arena.player.position.y.is_finite()
                    && arena.boss.height.is_finite()
            );
            assert!(arena.map.is_clear(arena.player.position, 0.85));
            assert!(
                arena.map.inside(arena.boss.position, 0.0),
                "{guardian:?}: {:?}",
                arena.boss.position
            );
        }
        assert!(arena.elapsed > 5.0, "{guardian:?}");
    }
}

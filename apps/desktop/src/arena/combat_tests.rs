use super::*;
use crate::arena::map::{CENTER, ISLANDS, THRONE};

pub(super) fn ticks(arena: &mut Arena, count: usize, controls: Controls) {
    for _ in 0..count {
        arena.tick(controls);
    }
}
/// Advances by simulated seconds at the fixed step.
pub(super) fn seconds(arena: &mut Arena, duration: f32, controls: Controls) {
    ticks(arena, (duration / STEP).round() as usize, controls);
}
pub(super) fn battle(guardian: Guardian) -> Arena {
    let mut arena = Arena::new(guardian, 0.0);
    wait_out_intro(&mut arena);
    arena
}
fn wait_out_intro(arena: &mut Arena) {
    for _ in 0..(3.0 / STEP) as usize {
        if arena.phase == Phase::Battle {
            return;
        }
        arena.tick(Controls::default());
    }
    panic!("intro did not end");
}
fn moving(direction: Vec2) -> Controls {
    Controls {
        movement: direction,
        ..Controls::default()
    }
}

#[test]
fn entry_and_state_changes_keep_height_continuous_and_spins_finish_upright() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::from_preview(Boss::idle(guardian, 3.7, false), 3.7, 17);
        arena.player.invulnerable = f32::INFINITY;
        for frame in 0..(24.0 / STEP) as usize {
            let before = arena.boss;
            let phase = arena.phase;
            arena.tick(moving(Vec2::from_angle(frame as f32 * 0.004)));
            let jump = (arena.boss.height - before.height).abs();
            if phase == Phase::Awakening {
                assert!(jump < 0.25, "{guardian:?} entry");
            }
            if phase == Phase::Battle && before.state != arena.boss.state {
                assert!(
                    jump < 2.0,
                    "{guardian:?}: {:?} -> {:?}",
                    before.state,
                    arena.boss.state
                );
            }
            if arena.boss.state == BossState::Watching && arena.boss.time > 0.4 {
                assert!(
                    arena.boss.bank.abs() < 0.5 && arena.boss.pitch.abs() < 0.5,
                    "{guardian:?} is not upright while watching"
                );
            }
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
        assert!(arena.map.standable(arena.player.position, 0.9));
    }
}

#[test]
fn agent_identity_selects_only_the_supported_guardians() {
    for (id, g) in [
        ("claude", Guardian::Claude),
        ("codex", Guardian::Codex),
        ("pi", Guardian::Pi),
        ("opencode", Guardian::OpenCode),
        ("deepseek-harness", Guardian::DeepSeek),
        ("copilot", Guardian::Copilot),
        ("cursor", Guardian::Cursor),
        ("cursor-agent", Guardian::Cursor),
        ("cline", Guardian::Cline),
        ("grok", Guardian::Grok),
        ("zcode", Guardian::ZCode),
        ("kimi", Guardian::Kimi),
        ("codebuddy-code", Guardian::CodeBuddy),
        ("antigravity", Guardian::Antigravity),
        ("devin", Guardian::Devin),
        ("gemini", Guardian::Gemini),
        ("hermes", Guardian::Hermes),
    ] {
        assert_eq!(Guardian::from_agent(id), Some(g));
    }
    for id in ["qwen-code", "custom-agent", ""] {
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
        for _ in 0..90 {
            arena.roll(Vec2::new(1.0, 0.0));
            arena.tick(Controls {
                movement: Vec2::new(1.0, 0.0),
                shoot: true,
                recall: true,
                sprint: true,
                ..Controls::default()
            });
            arena.release_shot(None);
        }
        assert_eq!(arena.player.position, start);
        assert_eq!(arena.arrow.state, ArrowState::Ready);
        assert_eq!(arena.player.charge, 0.0);
        assert!(arena.hazards.is_empty());
        wait_out_intro(&mut arena);
        assert!(arena.map.is_clear(arena.boss.position, arena.boss.radius()));
        assert_eq!(arena.boss.squash, 0.0);
        assert_eq!(arena.boss.lid, 0.0);
    }
}
#[test]
fn one_arrow_needs_a_full_draw_and_can_always_be_recalled() {
    let mut arena = battle(Guardian::Codex);
    arena.player.invulnerable = f32::INFINITY;
    arena.boss.enter(BossState::Watching, 30.0);
    let draw = Controls {
        shoot: true,
        ..Controls::default()
    };
    seconds(&mut arena, MIN_CHARGE * 0.5, draw);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    seconds(&mut arena, MIN_CHARGE + 0.02, draw);
    arena.release_shot(Some(Vec2::new(170.0, 110.0)));
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    let arrow = arena.arrow.position;
    arena.release_shot(None);
    assert_eq!(arena.arrow.position, arrow);
    seconds(
        &mut arena,
        2.0,
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
    arena.boss.enter(BossState::Watching, 30.0);
    let start = arena.player.position;
    seconds(
        &mut arena,
        0.2,
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
    seconds(&mut arena, 0.25, Controls::default());
    arena.arrow.state = ArrowState::Lodged;
    arena.arrow.position = Vec2::new(150.0, 100.0);
    let start = arena.player.position;
    ticks(
        &mut arena,
        8,
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
fn walking_is_brisk_and_holding_the_roll_key_sprints() {
    let mut arena = battle(Guardian::Copilot);
    arena.player.invulnerable = f32::INFINITY;
    arena.boss.enter(BossState::Watching, 30.0);
    arena.player.position = CENTER.minus(Vec2::new(30.0, 20.0));
    let start = arena.player.position;
    seconds(&mut arena, 0.5, moving(Vec2::new(1.0, 0.0)));
    let walked = arena.player.position.x - start.x;
    assert!((walked - 15.0).abs() < 0.5, "walked {walked}");
    let start = arena.player.position;
    seconds(
        &mut arena,
        0.5,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            sprint: true,
            ..Controls::default()
        },
    );
    assert!(arena.player.position.x - start.x > walked * 1.4);
    assert!(arena.player.sprinting);
}
#[test]
fn movement_slides_along_terrain_without_crossing_columns_the_rim_or_props() {
    for guardian in Guardian::ALL {
        let map = Map::new(guardian);
        let mut p = CENTER.plus(Vec2::new(0.0, 20.0));
        for _ in 0..300 {
            p = map.walk(p, p.plus(Vec2::new(1.0, 0.7)), 0.9);
            assert!(map.standable(p, 0.9));
        }
        for pillar in map.pillars() {
            let from = pillar.position.minus(Vec2::new(12.0, 0.0));
            let to = pillar.position.plus(Vec2::new(12.0, 0.0));
            let result = map.move_body(from, to, 0.9);
            assert!(result.x < pillar.position.x);
        }
    }
    let mut map = Map::new(Guardian::OpenCode);
    let block = CENTER.plus(Vec2::new(0.0, 20.0));
    map.drop_block(block);
    let stopped = map.walk(
        block.minus(Vec2::new(8.0, 0.0)),
        block.plus(Vec2::new(8.0, 0.0)),
        0.9,
    );
    assert!(stopped.x < block.x - 3.0);
    // A block that lands on the archer never traps them.
    let free = map.walk(block, block.plus(Vec2::new(6.0, 0.0)), 0.9);
    assert!(free.x > block.x + 5.0);
    let map = Map::new(Guardian::Pi);
    let below = THRONE.plus(Vec2::new(0.0, 20.0));
    assert!(map.walk(below, THRONE, 0.9).y > THRONE.y + 10.0);
}
#[test]
fn every_attack_commits_its_target_before_the_strike() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        arena.player.invulnerable = f32::INFINITY;
        let mut locked: Option<(u32, BossState, Attack, Vec2)> = None;
        let mut checked = 0;
        for frame in 0..(30.0 / STEP) as usize {
            arena.tick(moving(Vec2::from_angle(frame as f32 * 0.01)));
            let boss = arena.boss;
            let tracking = matches!(boss.state, BossState::Windup | BossState::Submerged);
            if tracking && boss.target_locked() {
                if let Some((attacks, state, attack, target)) = locked
                    && (attacks, state, attack) == (boss.attacks, boss.state, boss.attack)
                {
                    assert_eq!(boss.target, target, "{guardian:?} {attack:?}");
                    checked += 1;
                }
                locked = Some((boss.attacks, boss.state, boss.attack, boss.target));
            } else {
                locked = None;
            }
        }
        assert!(checked > 20, "{guardian:?} committed only {checked} frames");
    }
}
#[test]
fn landing_strikes_hit_exactly_their_telegraphed_radius() {
    let mut arena = battle(Guardian::Codex);
    arena.player.invulnerable = 0.0;
    arena.boss.attack = Attack::Leap;
    let landing = CENTER.plus(Vec2::new(-30.0, 20.0));
    arena.boss.target = landing;
    let radius = arena.boss.impact_radius();
    arena.player.position = landing.plus(Vec2::new(radius + PLAYER_RADIUS + 0.2, 0.0));
    arena.impact(landing, radius);
    assert_eq!(arena.phase, Phase::Battle);
    arena.player.position = landing.plus(Vec2::new(radius + PLAYER_RADIUS - 0.2, 0.0));
    arena.impact(landing, radius);
    assert_eq!(arena.phase, Phase::Defeat);
}
#[test]
fn a_closed_weak_point_glances_the_arrow_off_without_consuming_it() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        arena.boss.enter(BossState::Watching, 30.0);
        arena.boss.exposed = 0.0;
        arena.boss.update_core();
        let core = arena.boss.core;
        arena.arrow = Arrow {
            state: ArrowState::Flying,
            position: core.plus(Vec2::new(0.0, 5.0)),
            velocity: Vec2::new(0.0, -1200.0),
            burning: 0.0,
        };
        arena.tick_arrow(false);
        assert_eq!(arena.phase, Phase::Battle, "{guardian:?}");
        assert_eq!(arena.arrow.state, ArrowState::Lodged, "{guardian:?}");
    }
}
#[test]
fn a_bare_weak_point_still_requires_hitting_it_precisely() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.generation = 4;
    arena.boss.exposed = 1.0;
    arena.boss.update_core();
    arena.boss.previous_core = arena.boss.core;
    let core = arena.boss.core;
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: core.plus(Vec2::new(3.5, -1.0)),
        velocity: Vec2::new(0.0, 240.0),
        burning: 0.0,
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Battle);
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: core.plus(Vec2::new(0.0, -1.0)),
        velocity: Vec2::new(0.0, 240.0),
        burning: 0.0,
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}
#[test]
fn the_gaping_mouth_opens_to_the_front_and_the_jet_to_the_rear() {
    let mut arena = battle(Guardian::OpenCode);
    arena.boss.exposed = 1.0;
    arena.boss.direction = Vec2::new(0.0, 1.0);
    assert!(arena.accepts_core_hit(arena.boss.core.plus(Vec2::new(0.0, 8.0))));
    assert!(!arena.accepts_core_hit(arena.boss.core.minus(Vec2::new(0.0, 8.0))));
    let mut arena = battle(Guardian::Copilot);
    arena.boss.exposed = 1.0;
    arena.boss.direction = Vec2::new(0.0, 1.0);
    assert!(!arena.accepts_core_hit(arena.boss.core.plus(Vec2::new(0.0, 8.0))));
    assert!(arena.accepts_core_hit(arena.boss.core.minus(Vec2::new(0.0, 8.0))));
}
#[test]
fn water_floes_are_cover_from_the_discharge_but_not_from_a_breach() {
    let mut arena = battle(Guardian::DeepSeek);
    let floe = ISLANDS[0].0;
    arena.player.position = floe;
    arena.player.invulnerable = 0.0;
    arena.wave(
        floe.minus(Vec2::new(10.0, 0.0)),
        10.0,
        35.0,
        WaveKind::Water,
    );
    arena.tick_threats(floe);
    assert_eq!(arena.phase, Phase::Battle);
    arena.impact(floe, 7.0);
    assert_eq!(arena.phase, Phase::Defeat);
}
#[test]
fn fast_blocks_use_swept_collision_and_falling_tells_are_safe() {
    let mut arena = battle(Guardian::OpenCode);
    arena.player.position = CENTER.plus(Vec2::new(0.0, 25.0));
    let player = arena.player.position;
    arena.player.invulnerable = 0.0;
    arena.hazard(HazardKind::Drop, player, 0.5, 0.2, 6.0);
    arena.tick_threats(player);
    assert_eq!(arena.phase, Phase::Battle);
    arena.projectile(
        player.minus(Vec2::new(12.0, 0.0)),
        Vec2::new(3000.0, 0.0),
        ProjectileKind::Block,
    );
    arena.tick_threats(player);
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
            seconds(&mut arena, 1.02, Controls::default());
            assert_eq!(arena.phase, Phase::Rebirth);
            assert!(arena.map.standable(arena.player.position, 3.0));
            assert!(arena.player.position.minus(arena.boss.position).length() > 29.0);
            assert_eq!(arena.arrow.state, ArrowState::Ready);
            assert!(arena.map.blocks.is_empty());
            positions.push(arena.player.position);
            seconds(&mut arena, 0.75, Controls::default());
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
    arena.projectile(CENTER, Vec2::new(1.0, 0.0), ProjectileKind::Block);
    arena.wave(CENTER, 1.0, 50.0, WaveKind::Stone);
    arena.win();
    assert!(arena.projectiles.is_empty() && arena.waves.is_empty() && arena.hazards.is_empty());
    assert!(arena.effects.iter().any(|e| e.kind == EffectKind::Flash));
    seconds(&mut arena, 2.4, Controls::default());
    assert!(!arena.outcome_ready());
    seconds(&mut arena, 1.0, Controls::default());
    assert!(arena.outcome_ready());
    assert!(!arena.needs_tick());
    assert_eq!(arena.home_opacity(), 1.0);
    let time = arena.visual_time;
    seconds(&mut arena, 1.0, Controls::default());
    assert_eq!(arena.visual_time, time);
}
#[test]
fn camera_follows_continuously_and_roaming_is_not_a_stationary_breathing_loop() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.enter(BossState::Watching, 50.0);
    arena.player.position = CENTER;
    arena.camera = CENTER;
    let before = arena.camera;
    arena.tick(moving(Vec2::new(1.0, 0.0)));
    assert!(arena.camera.minus(before).length() < 0.5);
    let start = arena.camera;
    seconds(&mut arena, 1.25, moving(Vec2::new(1.0, 0.0)));
    assert!(arena.camera.x > start.x + 10.0);
    for g in Guardian::ALL {
        let a = Boss::idle(g, 2.0, false);
        let b = Boss::idle(g, 15.0, false);
        assert!(a.position.minus(b.position).length() > 15.0, "{g:?}");
    }
}

#[test]
fn long_running_encounters_bound_effects_and_keep_all_coordinates_finite() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 0.0);
        for frame in 0..(150.0 / STEP) as usize {
            let direction = Vec2::from_angle(frame as f32 * 0.0125);
            if frame % 74 == 0 {
                arena.roll(direction);
            }
            arena.tick(moving(direction));
            assert!(
                arena.projectiles.len() <= MAX_PROJECTILES
                    && arena.waves.len() <= MAX_WAVES
                    && arena.effects.len() <= MAX_EFFECTS
                    && arena.hazards.len() <= MAX_HAZARDS
                    && arena.map.blocks.len() <= crate::arena::map::MAX_BLOCKS
            );
            assert!(
                arena.player.position.x.is_finite()
                    && arena.player.position.y.is_finite()
                    && arena.player.knockback.length().is_finite()
                    && arena.boss.height.is_finite()
                    && arena.boss.core.x.is_finite()
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

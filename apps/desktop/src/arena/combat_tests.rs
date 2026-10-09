use super::*;

fn ticks(arena: &mut Arena, count: usize, controls: Controls) {
    for _ in 0..count {
        arena.tick(controls);
    }
}

fn battle(guardian: Guardian) -> Arena {
    let mut arena = Arena::new(guardian, 0.0);
    for _ in 0..100 {
        if arena.phase == Phase::Battle {
            break;
        }
        arena.tick(Controls::default());
    }
    assert_eq!(arena.phase, Phase::Battle);
    arena
}

fn arrow_through_core(arena: &mut Arena, returning: bool) {
    let core = arena.boss.core;
    arena.arrow = Arrow {
        state: if returning {
            ArrowState::Returning
        } else {
            ArrowState::Flying
        },
        position: core.plus(Vec2::new(0.0, -2.0)),
        velocity: Vec2::new(0.0, 78.0),
    };
    arena.player.position = core.plus(Vec2::new(0.0, 20.0));
    arena.tick_arrow(returning);
}

#[test]
fn awakening_preserves_idle_time_and_does_not_accept_attacks() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 2.375);
        let position = arena.player.position;
        ticks(
            &mut arena,
            30,
            Controls {
                shoot: true,
                movement: Vec2::new(1.0, 0.0),
                ..Default::default()
            },
        );
        arena.release_shot(None);
        arena.roll(Vec2::new(1.0, 0.0));
        assert_eq!(arena.phase, Phase::Awakening);
        assert_eq!(arena.player.position, position);
        assert_eq!(arena.player.charge, 0.0);
        assert_eq!(arena.arrow.state, ArrowState::Ready);
        assert_eq!(arena.boss.attacks, 0);
        assert!((arena.visual_time - 2.875).abs() < 0.001);
        ticks(&mut arena, 50, Controls::default());
        assert_eq!(arena.phase, Phase::Battle);
        assert_eq!(
            arena.boss.attacks, 0,
            "first attack gives the player time to orient"
        );
    }
}

#[test]
fn one_arrow_requires_a_draw_and_retrieval() {
    let mut arena = battle(Guardian::Claude);
    let shoot = Controls {
        shoot: true,
        ..Default::default()
    };
    ticks(&mut arena, 5, shoot);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    ticks(&mut arena, 22, shoot);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    let original = arena.arrow.position;
    arena.player.charge = 1.0;
    arena.release_shot(None);
    assert_eq!(arena.arrow.position, original);
    ticks(
        &mut arena,
        20,
        Controls {
            recall: true,
            ..Default::default()
        },
    );
    assert_eq!(arena.arrow.state, ArrowState::Ready);
}

#[test]
fn drawing_and_recalling_commit_the_archer_to_standing_still() {
    let mut arena = battle(Guardian::Claude);
    let start = arena.player.position;
    ticks(
        &mut arena,
        22,
        Controls {
            shoot: true,
            movement: Vec2::new(1.0, 0.0),
            ..Default::default()
        },
    );
    assert_eq!(arena.player.position, start);
    arena.release_shot(None);
    ticks(&mut arena, 10, Controls::default());
    ticks(
        &mut arena,
        4,
        Controls {
            recall: true,
            movement: Vec2::new(1.0, 0.0),
            ..Default::default()
        },
    );
    assert_eq!(arena.player.position, start);
}

#[test]
fn armor_and_the_return_seal_require_the_correct_opening() {
    for guardian in Guardian::ALL {
        for returning in [false, true] {
            let mut arena = battle(guardian);
            arrow_through_core(&mut arena, returning);
            assert_eq!(arena.phase, Phase::Battle);
        }
        let mut arena = battle(guardian);
        arena.boss.exposed = 1.0;
        arrow_through_core(&mut arena, false);
        if guardian == Guardian::Pi {
            assert_eq!(arena.phase, Phase::Battle);
            assert_eq!(arena.cue, Some(Cue::ReturnArrow));
            arrow_through_core(&mut arena, true);
        }
        assert_eq!(arena.phase, Phase::Victory);
        assert!(arena.projectiles.is_empty() && arena.waves.is_empty());
    }
}

#[test]
fn an_open_body_is_not_a_hit_on_the_weak_point() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.exposed = 2.0;
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: arena.boss.core.plus(Vec2::new(4.0, -3.0)),
        velocity: Vec2::new(0.0, 90.0),
    };
    for _ in 0..5 {
        arena.tick_arrow(false);
    }
    assert_eq!(arena.phase, Phase::Battle);
}

#[test]
fn roll_cancels_the_draw_and_one_unprotected_hit_ends_the_attempt() {
    let mut arena = battle(Guardian::Claude);
    arena.player.charge = 0.7;
    arena.roll(Vec2::new(1.0, 0.0));
    assert_eq!(arena.player.charge, 0.0);
    arena.hurt();
    assert_eq!(arena.phase, Phase::Battle);
    let cooldown = arena.player.roll_cooldown;
    arena.roll(Vec2::new(-1.0, 0.0));
    assert_eq!(arena.player.roll_cooldown, cooldown);
    ticks(&mut arena, 25, Controls::default());
    arena.hurt();
    assert_eq!(arena.phase, Phase::Defeat);
    assert!(!arena.outcome_ready());
    ticks(&mut arena, 80, Controls::default());
    assert!(arena.outcome_ready());
    assert!(!arena.needs_tick());
}

#[test]
fn every_telegraph_locks_before_impact_and_does_not_retarget_the_dodge() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        arena.boss.prepare(arena.player.position);
        while arena.boss.progress() < 0.61 {
            arena.tick_boss();
        }
        let locked = arena.boss.target;
        arena.player.position = Vec2::new(83.0, 38.0);
        while arena.boss.state == BossState::Windup {
            arena.tick_boss();
        }
        assert_eq!(
            arena.boss.target, locked,
            "{guardian:?} retargeted after its tell"
        );
        assert_eq!(arena.phase, Phase::Battle, "a telegraph cannot deal damage");
    }
}

#[test]
fn claude_opens_after_the_double_slam_and_deepseek_after_three_surges() {
    let mut claude = battle(Guardian::Claude);
    for attack in [Attack::LeftFist, Attack::RightFist, Attack::Clap] {
        claude.boss.prepare(Vec2::new(20.0, 40.0));
        assert_eq!(claude.boss.attack, attack);
        claude.boss.target = Vec2::new(20.0, 40.0);
        claude.impact();
        assert_eq!(claude.boss.exposed > 0.0, attack == Attack::Clap);
    }
    let mut whale = battle(Guardian::DeepSeek);
    for ix in 0..3 {
        whale.boss.prepare(Vec2::new(20.0, 40.0));
        whale
            .boss
            .enter(BossState::Rushing, Attack::Dash.strike_duration());
        while whale.boss.state == BossState::Rushing {
            whale.tick_boss();
        }
        assert_eq!(whale.boss.exposed > 0.0, ix == 2);
    }
}

#[test]
fn codex_exposes_its_rear_after_a_wall_impact() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.prepare(Vec2::new(48.0, 48.0));
    arena.boss.enter(BossState::Rushing, 3.0);
    assert_eq!(arena.boss.exposed, 0.0);
    for _ in 0..60 {
        arena.tick_boss();
    }
    assert_eq!(arena.boss.state, BossState::Recovery);
    assert!(arena.boss.exposed > 1.0);
    assert!(arena.boss.core.y < arena.boss.position.y);
}

#[test]
fn the_beam_aims_and_hits_from_the_open_shutter() {
    let mut arena = battle(Guardian::OpenCode);
    arena.boss.attacks = 1;
    let target = arena.boss.core.plus(Vec2::new(30.0, 0.0));
    arena.player.position = target;
    arena.boss.prepare(target);
    assert_eq!(arena.boss.attack, Attack::Sweep);
    assert_eq!(arena.boss.direction, Vec2::new(1.0, 0.0));
    while arena.boss.state == BossState::Windup {
        arena.tick_boss();
    }
    let mut dodged = arena.clone();
    dodged.player.position.y += 4.0;
    arena.impact();
    dodged.impact();
    assert_eq!(arena.phase, Phase::Defeat);
    assert_eq!(dodged.phase, Phase::Battle);
    let beam = arena
        .effects
        .iter()
        .find(|effect| effect.kind == EffectKind::Sweep)
        .unwrap();
    assert_eq!(beam.position, arena.boss.core);
    assert_eq!(beam.direction, Vec2::new(1.0, 0.0));
}

#[test]
fn a_lethal_landing_cannot_create_an_after_death_shockwave() {
    for attack in [Attack::Clap, Attack::Leap, Attack::Dive] {
        let mut arena = battle(Guardian::Claude);
        arena.boss.attack = attack;
        arena.boss.target = arena.player.position;
        arena.impact();
        assert_eq!(arena.phase, Phase::Defeat);
        assert!(arena.waves.is_empty());
        assert!(arena.projectiles.is_empty());
    }
}

#[test]
fn a_charge_cannot_tunnel_through_the_archer() {
    let mut arena = battle(Guardian::DeepSeek);
    arena.player.position = Vec2::new(48.0, 35.0);
    arena.boss.position = Vec2::new(48.0, 28.0);
    arena.boss.direction = Vec2::new(0.0, 1.0);
    arena.boss.attack = Attack::Dash;
    arena.boss.enter(BossState::Rushing, 0.36);
    ticks(&mut arena, 3, Controls::default());
    assert_eq!(arena.phase, Phase::Defeat);
}

#[test]
fn clearing_input_neither_fires_nor_keeps_recalling() {
    let mut arena = battle(Guardian::Claude);
    arena.player.charge = 1.0;
    arena.cancel_input();
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    assert_eq!(arena.player.charge, 0.0);
    arena.arrow.state = ArrowState::Returning;
    arena.cancel_input();
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
}

#[test]
fn diagonal_movement_is_normalized_and_boundary_clamps_are_stable() {
    let mut straight = battle(Guardian::Claude);
    let mut diagonal = straight.clone();
    let start = straight.player.position;
    ticks(
        &mut straight,
        10,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            ..Default::default()
        },
    );
    ticks(
        &mut diagonal,
        10,
        Controls {
            movement: Vec2::new(1.0, 1.0),
            ..Default::default()
        },
    );
    assert!(
        (straight.player.position.minus(start).length()
            - diagonal.player.position.minus(start).length())
        .abs()
            < 0.001
    );
    for _ in 0..600 {
        diagonal.player.invulnerable = 1.0;
        diagonal.tick(Controls {
            movement: Vec2::new(1.0, 1.0),
            ..Default::default()
        });
    }
    assert!(diagonal.player.position.x <= WIDTH - 3.5);
    assert!(diagonal.player.position.y <= HEIGHT - 3.5);
}

#[test]
fn long_battles_bound_threats_and_finished_outros_become_inert() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        for _ in 0..36_000 {
            arena.player.invulnerable = 1.0;
            arena.tick(Controls::default());
            assert!(arena.projectiles.len() <= MAX_PROJECTILES);
            assert!(arena.effects.len() <= MAX_EFFECTS);
            assert!(arena.waves.len() <= MAX_WAVES);
        }
        arena.phase = Phase::Victory;
        arena.phase_time = 0.0;
        ticks(&mut arena, 100, Controls::default());
        let time = arena.visual_time;
        ticks(
            &mut arena,
            100,
            Controls {
                shoot: true,
                ..Default::default()
            },
        );
        arena.roll(Vec2::new(1.0, 0.0));
        assert_eq!(arena.visual_time, time);
        assert_eq!(arena.player.charge, 0.0);
    }
}

fn evade(arena: &Arena) -> Vec2 {
    let boss = &arena.boss;
    match boss.attack {
        Attack::Cross => Vec2::new(
            if boss.target.x < 48.0 { 1.0 } else { -1.0 },
            if boss.target.y < 28.0 { 1.0 } else { -1.0 },
        ),
        Attack::Rush | Attack::Dash | Attack::Sweep | Attack::Volley => {
            let side = Vec2::new(boss.direction.y, -boss.direction.x);
            let next = arena.player.position.plus(side.scale(12.0));
            if next == next.clamped(6.0) {
                side
            } else {
                side.scale(-1.0)
            }
        }
        _ => Vec2::new(if boss.target.x < 48.0 { 1.0 } else { -1.0 }, 0.0),
    }
}

#[test]
fn every_guardian_can_be_defeated_with_normal_inputs_and_no_damage_override() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 0.0);
        for _ in 0..2400 {
            if matches!(arena.phase, Phase::Victory | Phase::Defeat) {
                break;
            }
            let mut controls = Controls::default();
            if (arena.boss.state == BossState::Windup && arena.boss.progress() >= 0.60)
                || arena.boss.state == BossState::Striking
                || arena.boss.state == BossState::Rushing
            {
                controls.movement = evade(&arena);
            }
            let close_wave = arena
                .waves
                .iter()
                .find(|wave| {
                    (arena.player.position.minus(wave.position).length() - wave.radius).abs() < 4.0
                })
                .map(|wave| arena.player.position.minus(wave.position).normalized());
            let close_bolt = arena
                .projectiles
                .iter()
                .any(|bolt| bolt.position.minus(arena.player.position).length() < 5.0);
            if let Some(direction) = close_wave {
                arena.roll(direction);
            } else if close_bolt
                || (arena.boss.state == BossState::Rushing
                    && arena.player.position.minus(arena.boss.position).length() < 17.0)
            {
                arena.roll(evade(&arena));
            }
            if arena.boss.exposed > 0.0 && arena.player.roll_remaining <= 0.0 {
                match arena.arrow.state {
                    ArrowState::Ready => {
                        if arena.player.charge >= 0.35 {
                            arena.release_shot(None);
                        } else {
                            controls.shoot = true;
                        }
                    }
                    ArrowState::Flying => {
                        let behind = arena
                            .arrow
                            .position
                            .minus(arena.boss.core)
                            .dot(arena.boss.core.minus(arena.player.position))
                            > 0.0;
                        controls.recall = guardian == Guardian::Pi && behind;
                    }
                    ArrowState::Lodged | ArrowState::Returning => controls.recall = true,
                }
            } else if arena.arrow.state != ArrowState::Ready {
                controls.recall = true;
            }
            arena.tick(controls);
        }
        assert_eq!(
            arena.phase,
            Phase::Victory,
            "{guardian:?}: {:?}, {:?}, player {:?}, boss {:?}",
            arena.boss.state,
            arena.boss.attack,
            arena.player.position,
            arena.boss.position
        );
    }
}

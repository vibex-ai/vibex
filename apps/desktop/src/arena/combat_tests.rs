use super::*;

fn ticks(arena: &mut Arena, count: usize, controls: Controls) {
    for _ in 0..count {
        arena.tick(controls);
    }
}

fn arrow_through_core(arena: &mut Arena, returning: bool) {
    let core = arena.boss.core;
    arena.arrow = Arrow {
        state: if returning {
            ArrowState::Returning
        } else {
            ArrowState::Flying
        },
        position: core.plus(Vec2::new(0.0, -3.0)),
        velocity: Vec2::new(0.0, 78.0),
    };
    arena.player.position = core.plus(Vec2::new(0.0, 20.0));
    arena.tick_arrow(returning);
}

#[test]
fn one_arrow_requires_a_real_draw_and_retrieval() {
    let mut arena = Arena::new(Guardian::Ember);
    let shoot = Controls {
        shoot: true,
        ..Default::default()
    };
    ticks(&mut arena, 5, shoot);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    ticks(&mut arena, 20, shoot);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    let original = arena.arrow.position;
    arena.player.charge = 1.0;
    arena.release_shot(None);
    assert_eq!(
        arena.arrow.position, original,
        "a second shot must not replace the arrow"
    );
    ticks(
        &mut arena,
        90,
        Controls {
            recall: true,
            ..Default::default()
        },
    );
    assert_eq!(arena.arrow.state, ArrowState::Ready);
}

#[test]
fn drawing_and_recalling_commit_the_player_to_standing_still() {
    let mut arena = Arena::new(Guardian::Ember);
    let start = arena.player.position;
    ticks(
        &mut arena,
        20,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            shoot: true,
            ..Default::default()
        },
    );
    assert_eq!(arena.player.position, start);
    arena.release_shot(None);
    ticks(&mut arena, 15, Controls::default());
    ticks(
        &mut arena,
        4,
        Controls {
            movement: Vec2::new(1.0, 0.0),
            recall: true,
            ..Default::default()
        },
    );
    assert_eq!(arena.player.position, start);
}

#[test]
fn a_sealed_guardian_cannot_be_defeated_by_shooting_or_recalling() {
    for guardian in Guardian::ALL {
        for returning in [false, true] {
            let mut arena = Arena::new(guardian);
            arrow_through_core(&mut arena, returning);
            assert_eq!(arena.phase, Phase::Battle);
        }
    }
}

#[test]
fn an_exposed_core_takes_one_precise_hit_and_prism_requires_the_return_path() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian);
        arena.boss.exposed = 1.0;
        arrow_through_core(&mut arena, false);
        if guardian == Guardian::Prism {
            assert_eq!(arena.phase, Phase::Battle);
            assert_eq!(arena.cue, Some(Cue::ReturnArrow));
            arrow_through_core(&mut arena, true);
        }
        assert_eq!(arena.phase, Phase::Victory);
        assert!(arena.projectiles.is_empty());
    }
}

#[test]
fn the_body_is_not_the_weak_point_even_when_the_core_is_open() {
    let mut arena = Arena::new(Guardian::Ember);
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
fn rolling_cancels_a_draw_and_protects_against_damage() {
    let mut arena = Arena::new(Guardian::Ember);
    arena.player.charge = 0.7;
    arena.roll(Vec2::new(1.0, 0.0));
    assert_eq!(arena.player.charge, 0.0);
    arena.hurt();
    assert_eq!(arena.player.health, 3);
    let cooldown = arena.player.roll_cooldown;
    arena.roll(Vec2::new(-1.0, 0.0));
    assert_eq!(arena.player.roll_cooldown, cooldown);
    ticks(&mut arena, 25, Controls::default());
    arena.hurt();
    assert_eq!(arena.player.health, 2);
}

#[test]
fn knot_only_exposes_its_rear_core_after_hitting_a_wall() {
    let mut arena = Arena::new(Guardian::Knot);
    let origin = arena.boss.position;
    arena.launch_attack(Windup {
        attack: Attack::Charge,
        origin,
        target: Vec2::new(48.0, 45.0),
        remaining: 0.0,
        duration: 1.0,
    });
    assert_eq!(arena.boss.exposed, 0.0);
    for _ in 0..60 {
        arena.tick_boss(STEP);
    }
    assert!(!arena.boss.charging);
    assert!(arena.boss.exposed > 1.0);
    assert!(arena.boss.core.y < arena.boss.position.y);
}

#[test]
fn cross_attack_uses_the_telegraphed_location_and_swaps_the_real_prism() {
    let mut arena = Arena::new(Guardian::Prism);
    let origin = arena.boss.position;
    let target = arena.player.position;
    arena.player.position = target.plus(Vec2::new(6.0, 6.0));
    arena.launch_attack(Windup {
        attack: Attack::Cross,
        origin,
        target,
        remaining: 0.0,
        duration: 1.0,
    });
    assert_eq!(arena.player.health, 3);
    assert_eq!(arena.boss.position.x, WIDTH - origin.x);
    assert_eq!(arena.boss.core, arena.boss.position);
    assert!(arena.boss.exposed > 0.0);
}

#[test]
fn focus_spends_energy_slows_threats_and_cannot_open_a_core() {
    let mut arena = Arena::new(Guardian::Ember);
    let mut ordinary = arena.clone();
    arena.focus();
    assert_eq!(arena.player.focus, 40.0);
    let spent = arena.player.focus;
    arena.focus();
    assert_eq!(
        arena.player.focus, spent,
        "an active field cannot spend focus again"
    );
    ticks(&mut arena, 30, Controls::default());
    ticks(&mut ordinary, 30, Controls::default());
    assert!(arena.boss.rest > ordinary.boss.rest);
    assert_eq!(arena.boss.exposed, 0.0);
    arena.player.focus = 20.0;
    arena.catch_arrow();
    assert_eq!(arena.player.focus, 36.0);
    arena.focus_remaining = 0.0;
    arena.focus();
    assert_eq!(arena.cue, Some(Cue::FocusEmpty));
    assert_eq!(arena.player.focus, 36.0);
}

#[test]
fn clearing_input_releases_a_recall_without_firing_a_charged_arrow() {
    let mut arena = Arena::new(Guardian::Ember);
    arena.player.charge = 1.0;
    arena.cancel_input();
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    assert_eq!(arena.player.charge, 0.0);
    arena.arrow.state = ArrowState::Returning;
    arena.cancel_input();
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
}

#[test]
fn diagonal_movement_is_normalized_and_cannot_leave_the_arena() {
    let mut straight = Arena::new(Guardian::Ember);
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
    ticks(
        &mut diagonal,
        600,
        Controls {
            movement: Vec2::new(1.0, 1.0),
            ..Default::default()
        },
    );
    assert!(diagonal.player.position.x <= WIDTH - 3.0);
    assert!(diagonal.player.position.y <= HEIGHT - 3.0);
}

#[test]
fn finished_battles_are_inert_and_long_battles_keep_bounded_effects() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian);
        for _ in 0..36_000 {
            arena.player.invulnerable = 1.0;
            arena.tick(Controls::default());
            assert!(arena.projectiles.len() <= MAX_PROJECTILES);
            assert!(arena.effects.len() <= 24);
            assert!((0.0..=100.0).contains(&arena.player.focus));
        }
        arena.phase = Phase::Victory;
        let elapsed = arena.elapsed;
        arena.tick(Controls {
            shoot: true,
            ..Default::default()
        });
        arena.roll(Vec2::new(1.0, 0.0));
        arena.focus();
        assert_eq!(arena.elapsed, elapsed);
        assert_eq!(arena.player.charge, 0.0);
        assert_eq!(arena.player.roll_remaining, 0.0);
    }
}

#[test]
fn every_guardian_can_be_defeated_through_normal_player_inputs() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian);
        for _ in 0..1_200 {
            if arena.phase != Phase::Battle {
                break;
            }
            let mut controls = Controls::default();
            if arena.boss.charging && arena.player.roll_cooldown <= 0.0 {
                arena.roll(Vec2::new(1.0, 0.0));
            }
            if arena
                .boss
                .windup
                .is_some_and(|windup| windup.attack == Attack::Cross)
            {
                controls.movement = Vec2::new(1.0, -1.0);
            }
            if arena.boss.exposed > 0.0 {
                match arena.arrow.state {
                    ArrowState::Ready => {
                        if arena.player.charge >= 0.35 {
                            arena.release_shot(None);
                        } else {
                            controls.shoot = true;
                        }
                    }
                    ArrowState::Flying => {
                        let behind_core = arena
                            .arrow
                            .position
                            .minus(arena.boss.core)
                            .dot(arena.boss.core.minus(arena.player.position))
                            > 0.0;
                        controls.recall = guardian == Guardian::Prism && behind_core;
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
            "{guardian:?} must have a reachable opening"
        );
    }
}

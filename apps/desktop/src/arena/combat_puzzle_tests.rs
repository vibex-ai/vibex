use super::tests::{battle, ticks};
use super::*;
use crate::arena::map::{ISLANDS, MAX_SCARS};

fn arrow_through(arena: &mut Arena, point: Vec2, direction: Vec2) {
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: point.minus(direction.scale(8.0)),
        velocity: direction.scale(720.0),
    };
    arena.tick_arrow(false);
}

#[test]
fn two_distinct_tendon_cuts_open_the_underside_and_a_missed_opening_regrows() {
    let mut arena = battle(Guardian::Claude);
    arena.boss.position = CENTER;
    arena.boss.yaw = 0.0;
    arena.boss.height = 2.5;
    arena.boss.rest_tentacles();
    arena.boss.update_core();
    arena.boss.attack = Attack::TendrilSweep;
    arena.boss.ward = 0;
    arena.boss.enter(BossState::Recovery, 2.0);
    let left = arena.boss.tentacle_joint(0);
    arrow_through(&mut arena, left, Vec2::new(1.0, 0.0));
    assert_eq!(arena.boss.severed, 1);
    assert_eq!(arena.boss.exposed, 0.0);
    arrow_through(&mut arena, left, Vec2::new(1.0, 0.0));
    assert_eq!(arena.boss.severed, 1);
    assert_eq!(arena.boss.exposed, 0.0);
    let right = arena.boss.tentacle_joint(1);
    arrow_through(&mut arena, right, Vec2::new(-1.0, 0.0));
    assert_eq!(arena.boss.severed, 3);
    assert!(arena.boss.exposed > 4.0);
    assert_eq!(arena.phase, Phase::Battle);
    for _ in 0..290 {
        arena.tick_boss();
    }
    assert_eq!(arena.boss.severed, 0);
    assert_eq!(arena.boss.exposed, 0.0);
}

#[test]
fn idle_attack_loops_never_solve_the_player_created_puzzles() {
    for guardian in [
        Guardian::Claude,
        Guardian::Pi,
        Guardian::OpenCode,
        Guardian::Copilot,
    ] {
        let mut arena = battle(guardian);
        for _ in 0..3600 {
            arena.tick_boss();
            assert_eq!(arena.boss.exposed, 0.0, "{guardian:?}");
        }
    }
}

#[test]
fn every_closed_carapace_rejects_a_shot_without_consuming_the_arrow() {
    for guardian in Guardian::ALL {
        let mut arena = battle(guardian);
        arena.boss.position = CENTER;
        arena.boss.height = 0.0;
        arena.boss.yaw = 0.0;
        arena.boss.update_core();
        let core = arena.boss.core;
        arrow_through(&mut arena, core, Vec2::new(0.0, -1.0));
        assert_eq!(arena.phase, Phase::Battle, "{guardian:?}");
        assert_eq!(arena.arrow.state, ArrowState::Lodged, "{guardian:?}");
        for _ in 0..150 {
            arena.tick_arrow(true);
            if arena.arrow.state == ArrowState::Ready {
                break;
            }
        }
        assert_eq!(arena.arrow.state, ArrowState::Ready, "{guardian:?}");
    }
}

#[test]
fn a_fast_arrow_hits_cover_before_a_core_and_destroyed_cover_stays_destroyed() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.position = Vec2::new(59.0, 31.0);
    arena.boss.opening(3.0);
    let shot = Arrow {
        state: ArrowState::Flying,
        position: Vec2::new(59.0, 52.0),
        velocity: Vec2::new(0.0, -3000.0),
    };
    arena.arrow = shot;
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Battle);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    assert!(arena.arrow.position.y > arena.boss.core.y + CORE_RADIUS);
    arena.map.broken |= 1;
    arena.map.scar(Vec2::new(59.0, 40.0), 8.0, ScarKind::Crack);
    arena.respawn();
    assert!(!arena.map.intact(0));
    assert_eq!(arena.map.scars.len(), 1);
    ticks(&mut arena, 44, Controls::default());
    arena.boss.opening(3.0);
    arena.arrow = shot;
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn a_swallowed_arrow_needs_a_sustained_pull_and_is_ejected_if_the_pull_is_missed() {
    let mut arena = battle(Guardian::OpenCode);
    arena.boss.position = CENTER;
    arena.boss.yaw = 0.0;
    arena.boss.direction = Vec2::new(0.0, 1.0);
    arena.boss.attack = Attack::Inhale;
    arena.boss.enter(BossState::Striking, 3.0);
    arena.boss.update_core();
    let core = arena.boss.core;
    arrow_through(&mut arena, core, Vec2::new(0.0, -1.0));
    assert_eq!(arena.arrow.state, ArrowState::Tethered);
    assert_eq!(arena.boss.exposed, 0.0);
    for _ in 0..12 {
        arena.tick_arrow(true);
    }
    assert_eq!(arena.boss.exposed, 0.0);
    assert!(arena.boss.tether > 0.2);
    arena.boss.time = arena.boss.duration;
    arena.tick_boss();
    arena.tick_arrow(false);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
    assert_eq!(arena.boss.tether, 0.0);
    arena.boss.attack = Attack::Inhale;
    arena.boss.enter(BossState::Striking, 3.0);
    let core = arena.boss.core;
    arrow_through(&mut arena, core, Vec2::new(0.0, -1.0));
    for _ in 0..39 {
        arena.tick_arrow(true);
    }
    assert!(arena.boss.exposed > 4.5);
    assert_eq!(arena.arrow.state, ArrowState::Returning);
    assert_eq!(arena.boss.tether, 1.0);
}

#[test]
fn only_intercepting_the_charged_drone_overloads_the_shield() {
    let mut arena = battle(Guardian::Copilot);
    arena.boss.position = CENTER;
    arena.boss.update_core();
    let point = arena.boss.core.plus(Vec2::new(15.0, 0.0));
    arena.projectile(point, Vec2::new(0.0, 10.0), ProjectileKind::Orb);
    arrow_through(&mut arena, point, Vec2::new(0.0, -1.0));
    assert_eq!(arena.boss.exposed, 0.0);
    arena.projectile(point, Vec2::new(0.0, 10.0), ProjectileKind::ChargedOrb);
    arrow_through(&mut arena, point, Vec2::new(0.0, -1.0));
    assert!(arena.boss.exposed > 4.0);
    assert!(arena.projectiles.is_empty());
    assert_eq!(arena.phase, Phase::Battle);
    assert_eq!(arena.arrow.state, ArrowState::Lodged);
}

#[test]
fn swimming_prevents_drawing_but_not_recall_and_the_pulse_restores_all_platforms() {
    let mut arena = battle(Guardian::DeepSeek);
    arena.boss.enter(BossState::Watching, 10.0);
    arena.player.position = ISLANDS[0].0;
    ticks(
        &mut arena,
        20,
        Controls {
            shoot: true,
            ..Controls::default()
        },
    );
    assert!(arena.player.charge >= MIN_CHARGE);
    arena.map.break_islands(arena.player.position, 9.0);
    arena.tick(Controls {
        shoot: true,
        ..Controls::default()
    });
    assert!(arena.player.swimming);
    assert_eq!(arena.player.charge, 0.0);
    arena.release_shot(None);
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    arena.arrow.state = ArrowState::Lodged;
    arena.arrow.position = arena.player.position.plus(Vec2::new(12.0, 0.0));
    ticks(
        &mut arena,
        60,
        Controls {
            recall: true,
            ..Controls::default()
        },
    );
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    arena.map.sunken = 0b1111;
    arena.boss.attack = Attack::Pulse;
    arena.boss.enter(BossState::Striking, 6.0);
    arena.tick_boss();
    assert_eq!(arena.map.sunken, 0);
    assert!(arena.map.on_island(arena.player.position));
    assert!(arena.waves.iter().any(|wave| wave.kind == WaveKind::Water));
}

#[test]
fn input_buffers_bridge_short_recovery_and_cancel_with_focus_input() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.enter(BossState::Watching, 10.0);
    arena.roll(Vec2::new(1.0, 0.0));
    ticks(&mut arena, 24, Controls::default());
    arena.roll(Vec2::new(0.0, 1.0));
    ticks(&mut arena, 8, Controls::default());
    assert!(arena.player.roll_remaining > 0.2);
    assert_eq!(arena.player.facing, Vec2::new(0.0, 1.0));
    ticks(&mut arena, 20, Controls::default());
    arena.player.charge = MIN_CHARGE;
    arena.hit_pause = 0.05;
    arena.release_shot(Some(Vec2::new(30.0, 80.0)));
    assert_eq!(arena.arrow.state, ArrowState::Ready);
    ticks(&mut arena, 5, Controls::default());
    assert_eq!(arena.arrow.state, ArrowState::Flying);
    arena.arrow = Arrow::default();
    arena.player.charge = MIN_CHARGE;
    arena.hit_pause = 0.05;
    arena.release_shot(None);
    arena.cancel_input();
    ticks(&mut arena, 5, Controls::default());
    assert_eq!(arena.arrow.state, ArrowState::Ready);
}

#[test]
fn terrain_scars_are_bounded_and_retain_the_most_recent_impacts() {
    let mut map = Map::new(Guardian::Claude);
    for ix in 0..100 {
        map.scar(
            Vec2::new(
                (ix % 20) as f32 * 6.0 + 20.0,
                (ix / 20) as f32 * 10.0 + 30.0,
            ),
            4.0,
            ScarKind::Crack,
        );
    }
    assert_eq!(map.scars.len(), MAX_SCARS);
    assert_eq!(map.scars.last().unwrap().position, Vec2::new(134.0, 70.0));
}

#[test]
fn a_missed_second_seal_releases_the_first_hand_for_another_attempt() {
    let mut arena = battle(Guardian::Pi);
    arena.boss.seals = 1;
    arena.boss.seal_time[0] = 0.02;
    arena.boss.enter(BossState::Watching, 1.0);
    arena.tick_boss();
    assert_eq!(arena.boss.seals, 1);
    arena.tick_boss();
    assert_eq!(arena.boss.seals, 0);
    assert_eq!(arena.boss.exposed, 0.0);
    arena.boss.prepare(arena.player.position);
    assert_eq!(arena.boss.attack, Attack::LeftFist);
}

#[test]
fn a_moving_core_uses_relative_sweep_instead_of_only_its_final_position() {
    let mut arena = battle(Guardian::Codex);
    arena.boss.position = CENTER;
    arena.boss.opening(2.0);
    let center = arena.boss.core;
    arena.boss.previous_core = center.minus(Vec2::new(8.0, 0.0));
    arena.boss.core = center.plus(Vec2::new(8.0, 0.0));
    arena.arrow = Arrow {
        state: ArrowState::Flying,
        position: center,
        velocity: Vec2::new(0.0, 1.0),
    };
    arena.tick_arrow(false);
    assert_eq!(arena.phase, Phase::Victory);
}

#[test]
fn fast_player_motion_cannot_tunnel_across_a_live_beam() {
    let mut arena = battle(Guardian::OpenCode);
    arena.player.invulnerable = 0.0;
    arena.player.position = Vec2::new(84.0, 58.0);
    arena.hazard(
        HazardKind::Beam,
        Vec2::new(80.0, 40.0),
        Vec2::new(80.0, 70.0),
        0.0,
        0.5,
        1.0,
    );
    arena.tick_threats(Vec2::new(76.0, 58.0));
    assert_eq!(arena.phase, Phase::Defeat);
}

#[test]
fn locked_radial_tells_keep_their_origins_directions_and_cover_at_release() {
    for yaw in [0.0, 0.73, 2.1] {
        let mut arena = battle(Guardian::Codex);
        arena.boss.position = CENTER;
        arena.boss.yaw = yaw;
        arena.boss.update_core();
        arena.boss.attack = Attack::Spokes;
        arena.boss.enter(BossState::Windup, 1.0);
        arena.boss.time = 0.65;
        let tells = arena.spokes();
        while arena.hazards.is_empty() {
            // Movement after the lock must not rotate or translate the tell.
            arena.player.position = arena.player.position.plus(Vec2::new(0.2, -0.1));
            arena.tick_boss();
        }
        assert_eq!(arena.hazards.len(), tells.len());
        for (hazard, (from, end)) in arena.hazards.iter().zip(tells) {
            assert_eq!(hazard.position, from);
            assert_eq!(hazard.end, end);
            assert_eq!(hazard.radius, 1.0);
            assert!(!arena.map.cover_hit(from, end));
        }
    }
}

use super::*;
use crate::arena::{
    combat::{Attack, Controls},
    map::{CENTER, ISLANDS, Map},
};

fn windup(guardian: Guardian, attack: Attack) -> Arena {
    let mut arena = Arena::new(guardian, 0.0);
    arena.phase = Phase::Battle;
    arena.boss.attack = attack;
    arena.boss.enter(BossState::Windup, 1.0);
    arena.boss.time = 0.99;
    arena
}

fn cues(arena: &Arena, reduced: bool) -> Raster {
    let mut frame = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    effects::ground(&mut frame, arena, reduced);
    frame
}

#[test]
fn a_jet_dash_warns_its_full_path_and_landing_through_flight() {
    let mut arena = windup(Guardian::Copilot, Attack::Dash);
    arena.boss.from = Vec2::new(40.0, 45.0);
    arena.boss.position = arena.boss.from;
    arena.boss.target = Vec2::new(125.0, 78.0);
    arena.boss.direction = arena.boss.target.minus(arena.boss.from).normalized();
    let target = pixel(arena.boss.target);
    let radius = (arena.boss.impact_radius() * SCALE).ceil() as i32;
    for state in [BossState::Windup, BossState::Rushing] {
        arena.boss.state = state;
        for reduced in [false, true] {
            let frame = cues(&arena, reduced);
            assert_eq!(frame.get(target.0 + radius, target.1), WARN);
            let halfway = pixel(arena.boss.from.lerp(arena.boss.target, 0.5));
            assert_eq!(frame.get(halfway.0, halfway.1), WARN);
        }
        arena.boss.position = arena.boss.from.lerp(arena.boss.target, 0.8);
    }
}

#[test]
fn a_water_pulse_warns_the_actual_wave_origin_away_from_the_whale() {
    let mut arena = windup(Guardian::DeepSeek, Attack::Pulse);
    arena.boss.position = Vec2::new(44.0, 82.0);
    let frame = cues(&arena, true);
    for _ in 0..3 {
        arena.tick(Controls::default());
    }
    let wave = arena.waves.first().expect("the pulse creates a water wave");
    assert_eq!(wave.position, CENTER);
    assert_ne!(wave.position, arena.boss.position);
    let origin = pixel(wave.position);
    assert_eq!(frame.get(origin.0 + 29, origin.1), WARN);
}

#[test]
fn radial_warning_marks_follow_the_same_rays_as_the_clipped_beams() {
    for yaw in [0.0, 0.73, 2.1] {
        let mut arena = windup(Guardian::Codex, Attack::Spokes);
        arena.boss.position = CENTER;
        arena.boss.yaw = yaw;
        arena.boss.update_core();
        for reduced in [false, true] {
            let frame = cues(&arena, reduced);
            for (from, end) in arena.spokes() {
                let midpoint = pixel(from.lerp(end, 0.5));
                assert_eq!(frame.get(midpoint.0, midpoint.1), WARN, "yaw {yaw}");
            }
        }
    }
}

#[test]
fn tentacle_warnings_mark_the_final_impact_before_the_limbs_arrive() {
    let mut arena = windup(Guardian::Claude, Attack::TendrilSweep);
    arena.boss.target = Vec2::new(88.0, 84.0);
    arena.boss.direction = Vec2::new(0.0, 1.0);
    let radius = (arena.boss.impact_radius() * SCALE).ceil() as i32;
    for pair in 0..3 {
        arena.boss.ward = pair;
        let frame = cues(&arena, true);
        for ix in pair as usize * 2..pair as usize * 2 + 2 {
            let landing = arena.boss.tentacle_strike(ix, 1.0);
            assert!(landing.minus(arena.boss.tentacles[ix]).length() > 5.0);
            let center = pixel(landing);
            assert_eq!(frame.get(center.0 + radius, center.1), WARN);
        }
    }
}

#[test]
fn submerged_islands_erase_walkable_art_without_changing_the_cached_floor() {
    let intact = Map::new(Guardian::DeepSeek);
    let mut map = intact.clone();
    map.sunken = 0b1111;
    let cached = floor(Guardian::DeepSeek);
    let mut overlay = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    scenery::damage(&mut overlay, &map);
    let mut submerged = cached.clone();
    submerged.blit(&overlay, 0, 0);
    for (position, radius) in ISLANDS {
        let center = pixel(position);
        let r = (radius * SCALE).ceil() as i32;
        for y in center.1 - r..=center.1 + r {
            for x in center.0 - r..=center.0 + r {
                let ground = Vec2::new(x as f32 / SCALE, y as f32 / SCALE);
                if intact.on_island(ground) {
                    assert!(!map.on_island(ground));
                    assert!(
                        matches!(submerged.get(x, y), WATER_DARK | WATER | WATER_LIGHT),
                        "walkable art remains at {x}, {y}"
                    );
                }
            }
        }
    }
    map.sunken = 0;
    let mut restored = Raster::with_offset(WIDTH, HEIGHT, 0, TOP_PAD);
    scenery::damage(&mut restored, &map);
    assert!(restored.pixels.iter().all(|ink| *ink == CLEAR));
}

use super::*;

fn front(guardian: Guardian) -> Boss {
    let mut boss = Boss::new(guardian);
    boss.position = Vec2::new(64.0, 72.0);
    boss.yaw = 0.0;
    boss.height = 0.0;
    boss.rest_hands();
    boss.update_core();
    boss
}

fn render(boss: &Boss) -> Raster {
    let mut frame = Raster::with_offset(704, 592, 0, 128);
    draw(&mut frame, boss, 0.0, 0.0, true);
    frame
}

fn sample(frame: &Raster, boss: &Boss, x: f32, y: f32, z: f32) -> u8 {
    let p = boss.local(x, y, z);
    frame.get((p.x * 4.0).floor() as i32, (p.y * 4.0).floor() as i32)
}

fn at(frame: &Raster, p: Vec2) -> u8 {
    frame.get((p.x * 4.0).floor() as i32, (p.y * 4.0).floor() as i32)
}

fn body(color: u8) -> bool {
    matches!(
        color,
        BODY_SHADOW | BODY_DARK | BODY | BODY_LIGHT | BODY_GLEAM
    )
}

fn lit(color: u8) -> bool {
    matches!(color, BODY | BODY_LIGHT | BODY_GLEAM)
}

#[test]
fn the_spark_radiates_uneven_rays_around_a_glowing_heart() {
    let boss = front(Guardian::Claude);
    let frame = render(&boss);
    let center = boss.body_center();
    let heart = at(&frame, center);
    assert!(matches!(heart, CORE | CORE_LIGHT | WHITE), "{heart}");
    // Rays reach well beyond the nucleus, with gaps between neighbours.
    let mut lengths = Vec::new();
    let angle = |ray: usize| ray as f32 * TAU / 12.0 + super::RAYS[ray % 12].0;
    for ray in 0..12 {
        let a = angle(ray);
        let direction = Vec2::new(a.cos(), -a.sin());
        let length = (8..48)
            .map(|n| n as f32 * 0.25)
            .take_while(|r| at(&frame, center.plus(direction.scale(*r))) != CLEAR)
            .last()
            .unwrap_or(0.0);
        lengths.push(length);
        let middle = (a + angle(ray + 1) + if ray == 11 { TAU } else { 0.0 }) * 0.5;
        let between = Vec2::new(middle.cos(), -middle.sin()).scale(6.0);
        assert!(
            !body(at(&frame, center.plus(between))),
            "gap after ray {ray}"
        );
    }
    let longest = lengths.iter().cloned().fold(0.0, f32::max);
    let shortest = lengths.iter().cloned().fold(f32::INFINITY, f32::min);
    assert!(shortest > 4.0, "{lengths:?}");
    assert!(longest - shortest > 1.2, "rays must be uneven: {lengths:?}");
}

#[test]
fn split_sparks_shrink_and_have_no_heart() {
    let mut boss = front(Guardian::Claude);
    boss.generation = 2;
    let frame = render(&boss);
    let width = (0..80)
        .filter(|x| {
            frame.get(
                boss.position.x as i32 * 4 - 40 + x,
                (boss.body_center().y * 4.0) as i32,
            ) != CLEAR
        })
        .count();
    let large = render(&front(Guardian::Claude));
    let large_width = (0..80)
        .filter(|x| {
            large.get(
                boss.position.x as i32 * 4 - 40 + x,
                (front(Guardian::Claude).body_center().y * 4.0) as i32,
            ) != CLEAR
        })
        .count();
    assert!(width < large_width * 3 / 4, "{width} vs {large_width}");
    let shard = Shard {
        alive: true,
        position: Vec2::new(64.0, 72.0),
        size: 0.8,
        ..Shard::default()
    };
    let mut frame = Raster::with_offset(704, 592, 0, 128);
    super::shard(&mut frame, &shard, 0.0);
    let center = shard.center();
    assert!(!matches!(at(&frame, center), CORE | CORE_LIGHT | WHITE));
}

#[test]
fn pi_preserves_the_square_aperture_and_its_two_separate_stems() {
    let boss = front(Guardian::Pi);
    let mut frame = Raster::with_offset(704, 592, 0, 128);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    for ix in 0..2 {
        stem(&mut frame, &boss, ix, 0.0);
    }
    let front_y = 3.1;
    // The slab, the column right of the aperture and the step below it.
    assert!(body(sample(&frame, &boss, -8.0, front_y, 20.0)));
    assert!(body(sample(&frame, &boss, 3.0, front_y, 14.0)));
    assert!(body(sample(&frame, &boss, -2.6, front_y, 8.0)));
    // The aperture shows a dark hole and the eye, never lit masonry.
    for (x, z) in [(-4.6, 12.0), (-0.6, 16.0), (-4.6, 16.0)] {
        assert!(!lit(sample(&frame, &boss, x, front_y, z)), "{x} {z}");
    }
    // Left leg and the separate right stem, with an open gap between the
    // step and the right stem.
    assert!(body(sample(&frame, &boss, -8.3, front_y, 6.0)));
    assert!(body(sample(&frame, &boss, 8.6, front_y, 5.0)));
    assert_eq!(sample(&frame, &boss, 3.2, front_y, 2.0), CLEAR);
}

#[test]
fn copilot_has_two_dark_face_slots_below_two_goggle_lenses() {
    let boss = front(Guardian::Copilot);
    let frame = render(&boss);
    for side in [-1.0, 1.0] {
        let slot = sample(&frame, &boss, side * 2.4, 7.6, 8.6);
        assert!(
            matches!(slot, WATER_DARK | WATER | WATER_LIGHT | WHITE),
            "slot {slot}"
        );
        let lens = sample(&frame, &boss, side * 4.3, 6.2, 14.6);
        assert!(
            matches!(lens, WATER_DARK | WATER | WATER_LIGHT | WHITE),
            "lens {lens}"
        );
    }
    let plate = sample(&frame, &boss, 0.0, 7.4, 8.6);
    assert!(
        matches!(plate, OUTLINE | SHADOW | BODY_SHADOW | BODY_DARK),
        "plate {plate}"
    );
}

#[test]
fn the_chest_keeps_a_light_frame_around_a_dark_mouth_that_opens() {
    let mut boss = front(Guardian::OpenCode);
    let closed = render(&boss);
    let frame = sample(&closed, &boss, -5.0, 4.5, 4.0);
    assert!(matches!(frame, BODY | BODY_LIGHT | BODY_GLEAM), "{frame}");
    boss.lid = 1.0;
    boss.exposed = 1.0;
    let open = render(&boss);
    let core = at(&open, boss.core);
    assert!(matches!(core, CORE | CORE_LIGHT | WHITE), "{core}");
    let changed = closed
        .pixels
        .iter()
        .zip(&open.pixels)
        .filter(|(a, b)| a != b)
        .count();
    assert!(changed > 200);
}

#[test]
fn melting_the_knot_reveals_the_luminous_cube() {
    let mut boss = front(Guardian::Codex);
    let closed = render(&boss);
    boss.exposed = 3.0;
    boss.unravel = 1.0;
    boss.shield = 1.0;
    let open = render(&boss);
    let center = boss.core;
    let ring = |frame: &Raster| {
        (0..24)
            .filter(|ix| {
                let a = *ix as f32 * TAU / 24.0;
                body(at(
                    frame,
                    center.plus(Vec2::new(a.cos(), a.sin()).scale(7.0)),
                ))
            })
            .count()
    };
    assert!(ring(&open) < ring(&closed));
    assert!(matches!(at(&open, center), CORE_LIGHT | WHITE));
}

#[test]
fn the_whale_reads_as_an_orca_with_dorsal_fin_eye_patch_and_white_belly() {
    let mut boss = front(Guardian::DeepSeek);
    boss.yaw = -PI * 0.5;
    let frame = render(&boss);
    let white = |c: u8| matches!(c, DUST | IVORY | WHITE);
    // A dark eye beside a white patch on the near side.
    let eye = boss.local(-5.25, 8.5, 8.0);
    let (ex, ey) = ((eye.x * 4.0) as i32, (eye.y * 4.0) as i32);
    let mut patch = 0;
    let mut dark = 0;
    for y in ey - 10..=ey + 10 {
        for x in ex - 12..=ex + 4 {
            match frame.get(x, y) {
                c if white(c) => patch += 1,
                OUTLINE | SHADOW => dark += 1,
                _ => {}
            }
        }
    }
    assert!(patch >= 8 && dark >= 2, "eye patch {patch} dark {dark}");
    let belly = sample(&frame, &boss, -5.0, 0.0, 4.5);
    assert!(white(belly), "belly {belly}");
    let flank = sample(&frame, &boss, -7.0, 2.0, 8.0);
    assert!(body(flank), "flank {flank}");
    // A tall dorsal fin rises high above the back; just ahead of it is open air.
    assert!(body(sample(&frame, &boss, 0.0, -2.0, 18.0)));
    assert_eq!(sample(&frame, &boss, 0.0, 2.0, 18.5), CLEAR);
}

#[test]
fn the_exposed_whale_curls_its_tail_over_its_back_like_the_logo() {
    let mut boss = front(Guardian::DeepSeek);
    boss.update_core();
    let swimming = boss.core;
    boss.unravel = 1.0;
    boss.exposed = 1.0;
    boss.update_core();
    assert!(boss.core.y < swimming.y - 8.0);
    // Seen head-on, the raised flukes clear the back and fin.
    let frame = render(&boss);
    assert!(matches!(at(&frame, boss.core), CORE | CORE_LIGHT | WHITE));
}

#[test]
fn every_weak_point_is_drawn_where_collision_places_it() {
    for guardian in Guardian::ALL {
        let mut boss = front(guardian);
        boss.exposed = 1.0;
        boss.generation = 4;
        boss.lid = 1.0;
        boss.unravel = 1.0;
        boss.shield = 1.0;
        boss.yaw = if matches!(guardian, Guardian::DeepSeek | Guardian::Copilot) {
            PI
        } else {
            0.0
        };
        boss.update_core();
        let frame = render(&boss);
        let color = at(&frame, boss.core);
        assert!(
            matches!(color, CORE | CORE_LIGHT | WHITE),
            "{guardian:?}: {color}"
        );
    }
}

#[test]
fn authored_mesh_animation_freezes_under_reduced_motion() {
    for guardian in Guardian::ALL {
        let boss = front(guardian);
        let mut first = Raster::new(512, 384);
        let mut later = Raster::new(512, 384);
        draw(&mut first, &boss, 0.0, 0.0, true);
        draw(&mut later, &boss, 15.75, 0.0, true);
        assert_eq!(first.pixels, later.pixels, "{guardian:?}");
    }
}

#[test]
fn bevel_corners_keep_outward_winding_without_degenerate_triangles() {
    let mut mesh = Mesh::new();
    mesh.bevel_box(
        Point3::default(),
        Point3::new(4.0, 6.0, 8.0),
        0.4,
        Point3::default(),
        CLAY,
    );
    for face in &mesh.faces {
        let normal = face.points[1]
            .sub(face.points[0])
            .cross(face.points[2].sub(face.points[0]));
        let middle = face
            .points
            .iter()
            .fold(Point3::default(), |sum, p| sum.add(*p))
            .scale(0.25);
        assert!(normal.length() > 0.0001);
        assert!(normal.dot(middle) > 0.0);
    }
}

#[test]
fn every_model_carries_a_dark_silhouette() {
    for guardian in Guardian::ALL {
        let boss = front(guardian);
        let frame = render(&boss);
        let outline = frame.pixels.iter().filter(|c| **c == OUTLINE).count();
        assert!(outline > 60, "{guardian:?}: {outline}");
    }
}

#[test]
fn the_cube_carries_its_pointer_on_whichever_face_it_has_turned_to() {
    use crate::arena::cube::{FRONT, UP, WEST};
    for facet in [FRONT, WEST, UP] {
        let mut boss = front(Guardian::Cursor);
        boss.yaw = -PI * 0.25;
        boss.facet = facet;
        boss.exposed = 1.0;
        boss.update_core();
        let frame = render(&boss);
        let color = at(&frame, boss.core);
        assert!(
            matches!(color, CORE | CORE_LIGHT | WHITE),
            "{facet}: {color}"
        );
        // Closed, the pointer is the logo's light cut, never the dark body.
        boss.exposed = 0.0;
        let frame = render(&boss);
        let color = at(&frame, boss.core);
        assert!(matches!(color, DUST | IVORY | WHITE), "{facet}: {color}");
    }
    // Corner-on, the cube shows the logo's hexagon: two side faces and the top.
    let mut boss = front(Guardian::Cursor);
    boss.yaw = -PI * 0.25;
    boss.facet = crate::arena::cube::DOWN;
    let frame = render(&boss);
    let center = boss.body_center();
    let left = at(&frame, center.plus(Vec2::new(-5.0, 2.0)));
    let right = at(&frame, center.plus(Vec2::new(5.0, 2.0)));
    let top = at(&frame, center.plus(Vec2::new(0.0, -6.0)));
    assert!(body(left) && body(right) && body(top));
    assert!(left != right || right != top, "faces share one shade");
}

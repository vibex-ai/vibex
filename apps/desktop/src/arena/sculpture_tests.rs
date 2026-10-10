use super::*;

fn front(guardian: Guardian) -> Boss {
    let mut boss = Boss::new(guardian);
    boss.position = Vec2::new(64.0, 72.0);
    boss.yaw = 0.0;
    boss.height = 0.0;
    boss.rest_hands();
    boss.rest_tentacles();
    boss.update_core();
    boss
}

fn sample(frame: &Raster, boss: &Boss, x: f32, y: f32, z: f32) -> u8 {
    let p = boss.local(x, y, z);
    frame.get((p.x * 4.0).floor() as i32, (p.y * 4.0).floor() as i32)
}

#[test]
fn pi_preserves_the_square_aperture_and_separate_right_stem() {
    let boss = front(Guardian::Pi);
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    for (x, z) in [
        (-7.8, 22.2),
        (-2.6, 22.2),
        (2.6, 22.2),
        (-7.8, 17.0),
        (2.6, 17.0),
        (-7.8, 11.8),
        (-2.6, 11.8),
        (7.8, 11.8),
        (-7.8, 6.6),
        (7.8, 6.6),
    ] {
        assert_ne!(sample(&frame, &boss, x, 3.4, z), CLEAR, "{x}, {z}");
    }
    for (x, z) in [
        (7.8, 22.2),
        (7.8, 18.7),
        (2.6, 11.8),
        (-2.6, 6.6),
        (2.6, 6.6),
    ] {
        assert_eq!(sample(&frame, &boss, x, 3.4, z), CLEAR, "{x}, {z}");
    }
    let aperture = sample(&frame, &boss, -2.6, -0.6, 17.0);
    assert!(matches!(aperture, IVORY | WHITE), "aperture ink {aperture}");
}

#[test]
fn copilot_has_two_dark_face_slots_below_two_blue_goggles() {
    let boss = front(Guardian::Copilot);
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    for side in [-1.0, 1.0] {
        let eye = sample(&frame, &boss, side * 2.20, 7.24, 8.55);
        assert!(
            matches!(eye, OUTLINE | SHADOW | BODY_SHADOW | BODY_DARK),
            "eye ink {eye}"
        );
        let lens = sample(&frame, &boss, side * 4.4, 7.60, 14.2);
        assert!(
            matches!(lens, WATER_DARK | WATER | WATER_LIGHT | CORE_LIGHT),
            "lens ink {lens}"
        );
    }
    let face = sample(&frame, &boss, 0.0, 7.0, 8.55);
    assert!(matches!(face, DUST | IVORY | WHITE), "face ink {face}");
}

#[test]
fn claude_has_tall_pale_eyes_and_a_pointed_underside() {
    let boss = front(Guardian::Claude);
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    for side in [-1.0, 1.0] {
        for z in [14.0, 15.0, 16.0, 17.0] {
            assert!(matches!(
                sample(&frame, &boss, side * 4.3, 5.38, z),
                IVORY | WHITE
            ));
        }
    }
    assert_ne!(sample(&frame, &boss, 0.0, 0.6, 2.0), CLEAR);
    assert_eq!(sample(&frame, &boss, 4.5, 0.6, 2.0), CLEAR);
}

#[test]
fn the_terminal_keeps_a_white_rectangular_rim_around_a_dark_recess() {
    let boss = front(Guardian::OpenCode);
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    for x in [-6.85, 6.85] {
        assert!(matches!(sample(&frame, &boss, x, 6.1, 9.0), IVORY | WHITE));
    }
    for z in [2.0, 16.0] {
        assert!(matches!(sample(&frame, &boss, 0.0, 6.1, z), IVORY | WHITE));
    }
    assert!(matches!(
        sample(&frame, &boss, 0.0, 5.7, 9.0),
        OUTLINE | SHADOW | BODY_SHADOW | BODY_DARK | BODY
    ));
}

#[test]
fn separating_the_knot_reveals_the_luminous_cube() {
    let mut boss = front(Guardian::Codex);
    boss.exposed = 2.0;
    boss.strain = 1.0;
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    assert!(matches!(
        sample(&frame, &boss, 0.0, 1.3, 11.0),
        CORE | CORE_LIGHT | WHITE
    ));
    let core = boss.core;
    for x in [-3.2, 3.2] {
        assert_eq!(
            frame.get(((core.x + x) * 4.0) as i32, (core.y * 4.0) as i32),
            CLEAR
        );
    }
}

#[test]
fn the_whale_has_an_ivory_jaw_and_two_raised_tail_lobes() {
    let boss = front(Guardian::DeepSeek);
    let mut frame = Raster::new(512, 384);
    draw(&mut frame, &boss, 0.0, 0.0, true);
    assert!(matches!(
        sample(&frame, &boss, 0.0, 11.1, 4.0),
        DUST | IVORY | WHITE
    ));
    for side in [-1.0, 1.0] {
        let tail = sample(&frame, &boss, side * 6.4, -17.0, 20.0);
        assert!(
            (BODY_SHADOW..=BODY_GLEAM).contains(&tail),
            "tail ink {tail}"
        );
    }
}

#[test]
fn the_visible_cut_joint_uses_the_encounter_anchor() {
    let mut boss = front(Guardian::Claude);
    boss.state = BossState::Recovery;
    boss.attack = Attack::TendrilSweep;
    boss.ward = 0;
    boss.tentacles[0] = boss.position.plus(Vec2::new(-25.0, 8.0));
    boss.tentacle_heights[0] = 0.4;
    let mut frame = Raster::new(512, 384);
    tentacle(&mut frame, &boss, 0, 0.0);
    let joint = boss.tentacle_joint(0);
    let (x, y) = (
        (joint.x * 4.0).round() as i32,
        (joint.y * 4.0).round() as i32,
    );
    let mut visible = 0;
    for dy in -3..=3 {
        for dx in -3..=3 {
            visible += usize::from(matches!(
                frame.get(x + dx, y + dy),
                CORE | CORE_LIGHT | WHITE
            ));
        }
    }
    assert!(
        visible >= 3,
        "the cut marker must overlap its collision joint"
    );
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

use super::*;
use crate::arena::{combat::Controls, raster::ink};

#[test]
fn pointer_mapping_uses_the_following_camera_at_all_viewport_shapes() {
    for (w, h, rem) in [
        (360.0, 620.0, 14.0),
        (900.0, 700.0, 16.0),
        (1200.0, 560.0, 24.0),
    ] {
        let bounds = Bounds::new(point(px(31.0), px(17.0)), size(px(w), px(h)));
        for camera in [Vec2::new(42.0, 37.0), CENTER, Vec2::new(133.0, 88.0)] {
            let g = Geometry::battle(bounds, camera, px(rem));
            let target = Vec2::new(95.0, 67.0);
            let actual = g.world(g.screen(target)).unwrap();
            assert!((actual.x - target.x).abs() < 0.001 && (actual.y - target.y).abs() < 0.001);
            assert!(g.unit * WIDTH >= bounds.size.width && g.unit * HEIGHT >= bounds.size.height);
        }
    }
    assert!(
        Geometry {
            origin: point(px(0.0), px(0.0)),
            unit: px(0.0)
        }
        .world(point(px(0.0), px(0.0)))
        .is_none()
    );
}
#[test]
fn the_camera_moves_with_the_archer_instead_of_fitting_the_entire_map() {
    let bounds = Bounds::new(point(px(0.0), px(0.0)), size(px(900.0), px(640.0)));
    let a = Geometry::battle(bounds, Vec2::new(61.0, 46.0), px(16.0));
    let b = Geometry::battle(bounds, Vec2::new(116.0, 78.0), px(16.0));
    assert!(b.origin.x < a.origin.x - px(100.0));
    assert!(b.origin.y < a.origin.y);
    assert_eq!(a.unit, b.unit);
}
#[test]
fn all_six_sanctuaries_have_distinct_geometry_and_art() {
    let floors = Guardian::ALL.map(art::floor);
    for (ix, a) in floors.iter().enumerate() {
        assert!(ground_rectangles(Guardian::ALL[ix]).len() < 40_000);
        for b in &floors[ix + 1..] {
            assert_ne!(a.pixels, b.pixels);
        }
    }
}
#[test]
fn merged_quads_reconstruct_the_world_and_raised_geometry_exactly() {
    for guardian in Guardian::ALL {
        let mut arena = Arena::new(guardian, 0.0);
        for _ in 0..120 {
            arena.tick(Controls::default());
        }
        let actors = art::battle(&arena, false);
        let mut expected = art::floor(guardian).clone();
        expected.blit(&actors, 0, 0);
        let mut restored = Raster::new(art::WIDTH, art::HEIGHT);
        let mut rectangles = ground_rectangles(guardian).to_vec();
        rectangles.extend(actors.rectangles());
        assert!(
            rectangles.len() < 48_000,
            "{guardian:?}: {}",
            rectangles.len()
        );
        for r in rectangles {
            restored.rect(
                r.x as i32,
                r.y as i32,
                r.width as i32,
                r.height as i32,
                r.ink,
            );
        }
        assert!(restored.pixels == expected.pixels, "{guardian:?}");
    }
}
#[test]
fn a_roaming_guardian_is_the_same_model_and_pose_in_the_first_battle_frame() {
    for guardian in Guardian::ALL {
        for (time, reduced) in [(0.0, true), (3.7, false), (17.2, false)] {
            let sample = PreviewSample::at(guardian, time, reduced);
            let preview = art::idle(&sample.boss, time, reduced);
            assert!(
                !preview
                    .pixels
                    .iter()
                    .any(|i| matches!(*i, ink::SKIN | ink::CAPE | ink::CLOTH))
            );
            let arena = Arena::from_preview(sample.boss, time, 17);
            assert!(
                preview.pixels == art::battle(&arena, reduced).pixels,
                "{guardian:?} at {time}"
            );
        }
    }
}
#[test]
fn reduced_motion_freezes_every_part_of_the_home_model() {
    for guardian in Guardian::ALL {
        let expected = art::preview(guardian, 0.0, true);
        for time in [1.2, 8.3, 24.5] {
            assert!(
                expected.pixels == art::preview(guardian, time, true).pixels,
                "{guardian:?}"
            );
        }
        assert!(
            art::preview(guardian, 0.0, false).pixels
                == art::preview(guardian, art::IDLE_PERIOD, false).pixels,
            "{guardian:?} loop"
        );
    }
}
#[test]
fn the_home_button_follows_the_visible_model_and_fits_the_page() {
    for (w, h, rem) in [
        (360.0, 480.0, 14.0),
        (900.0, 640.0, 16.0),
        (1200.0, 700.0, 24.0),
    ] {
        let bounds = Bounds::new(point(px(31.0), px(19.0)), size(px(w), px(h)));
        let g = Geometry::preview(bounds, px(rem));
        for guardian in Guardian::ALL {
            for tick in 0..72 {
                let t = tick as f32 * 0.5;
                let frame = art::preview(guardian, t, false);
                let (l, top, r, b) = model_bounds(&frame);
                assert!(r > l && b > top);
                let left = g.origin.x + g.unit * l as f32 / art::SCALE;
                let right = g.origin.x + g.unit * r as f32 / art::SCALE;
                let top = g.origin.y + g.unit * (top as f32 - art::TOP_PAD as f32) / art::SCALE;
                let bottom = g.origin.y + g.unit * (b as f32 - art::TOP_PAD as f32) / art::SCALE;
                assert!(
                    left >= bounds.left() - px(2.0)
                        && right <= bounds.right() + px(2.0)
                        && top >= bounds.top() - px(2.0)
                        && bottom <= bounds.bottom() + px(2.0),
                    "{guardian:?} at {t}: {left:?} {top:?} {right:?} {bottom:?} in {bounds:?}"
                );
            }
        }
    }
}
#[test]
fn spatial_rotation_changes_visible_faces_and_flight_keeps_the_upper_model() {
    for guardian in Guardian::ALL {
        let mut boss = Boss::new(guardian);
        let rest = art::idle(&boss, 0.0, true);
        boss.yaw = 1.3;
        boss.pitch = 0.6;
        boss.bank = 0.15;
        let turned = art::idle(&boss, 0.0, true);
        assert_ne!(rest.pixels, turned.pixels, "{guardian:?}");
        let material_pixels = rest
            .pixels
            .iter()
            .filter(|i| **i != ink::CLEAR && **i != ink::SHADOW)
            .count();
        assert!(material_pixels > 350, "{guardian:?}: {material_pixels}");
    }
    let mut boss = Boss::new(Guardian::Copilot);
    boss.position = Vec2::new(88.0, 16.0);
    boss.height = 23.0;
    let frame = art::idle(&boss, 0.0, true);
    let (_, top, _, _) = model_bounds(&frame);
    assert!(top > 0 && top < art::TOP_PAD as usize);
}
#[test]
fn the_whales_thin_fins_remain_visible_on_both_sides() {
    let boss = Boss::new(Guardian::DeepSeek);
    let frame = art::idle(&boss, 0.0, true);
    let (left, _, right, _) = model_bounds(&frame);
    assert!(left as f32 / art::SCALE < boss.position.x - 9.0);
    assert!(right as f32 / art::SCALE > boss.position.x + 9.0);
}
#[test]
fn the_measured_entry_camera_interpolates_without_a_projection_jump() {
    let bounds = Bounds::new(point(px(30.0), px(20.0)), size(px(920.0), px(640.0)));
    let home = Geometry::preview(bounds, px(16.0));
    let field = Geometry::battle(bounds, Vec2::new(120.0, 73.0), px(16.0));
    assert_eq!(home.interpolate(field, 0.0), home);
    assert_eq!(home.interpolate(field, 1.0), field);
    let midpoint = home.interpolate(field, 0.5);
    assert!(midpoint.unit > home.unit && midpoint.unit < field.unit);
}
#[gpui::test]
fn authored_guardian_colors_are_identical_on_light_and_dark_home_surfaces(
    cx: &mut gpui::TestAppContext,
) {
    cx.update(|cx| {
        gpui_component::init(cx);
        let expected = Guardian::ALL.map(palette::material_colors);
        for mode in [
            gpui_component::ThemeMode::Light,
            gpui_component::ThemeMode::Dark,
        ] {
            gpui_component::Theme::change(mode, None, cx);
            for (guardian, expected) in Guardian::ALL.into_iter().zip(expected) {
                assert_eq!(palette::material_colors(guardian), expected);
            }
        }
    });
}

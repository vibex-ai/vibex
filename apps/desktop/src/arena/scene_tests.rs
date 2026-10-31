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
/// The guardian is the page's entry, so its pointer target has to be forgiving
/// rather than exactly the silhouette of a moving raster. A margin around the
/// fitted art is what a reader aims at; empty home space beyond it must stay
/// empty, and the target may never leave the scene it is drawn in.
#[test]
fn the_home_entry_target_grows_past_the_art_but_not_past_the_scene() {
    let scene = Bounds::new(point(px(10.0), px(20.0)), size(px(800.0), px(600.0)));
    // An ordinary figure away from the scene's edges gains the margin evenly.
    let art = Bounds::new(point(px(300.0), px(200.0)), size(px(90.0), px(80.0)));
    let target = entry_target(scene, art, px(ENTRY_SLOP));
    assert_eq!(target.left(), art.left() - px(ENTRY_SLOP));
    assert_eq!(target.top(), art.top() - px(ENTRY_SLOP));
    assert_eq!(target.right(), art.right() + px(ENTRY_SLOP));
    assert_eq!(target.bottom(), art.bottom() + px(ENTRY_SLOP));
    assert!(target.contains(&art.center()));

    // A figure against an edge keeps its margin on the inside and is clipped on
    // the outside, so the target never reaches outside the scene.
    let edge = Bounds::new(point(scene.left(), scene.top()), size(px(40.0), px(30.0)));
    let target = entry_target(scene, edge, px(ENTRY_SLOP));
    assert_eq!(target.left(), scene.left());
    assert_eq!(target.top(), scene.top());
    assert_eq!(target.right(), edge.right() + px(ENTRY_SLOP));
    assert_eq!(target.bottom(), edge.bottom() + px(ENTRY_SLOP));
    assert!(target.left() >= scene.left() && target.top() >= scene.top());

    // A scene too small to hold the margin collapses onto it rather than
    // inverting the rectangle.
    let cramped = Bounds::new(scene.origin, size(px(8.0), px(8.0)));
    let target = entry_target(scene, cramped, px(ENTRY_SLOP));
    assert!(target.size.width >= px(0.0) && target.size.height >= px(0.0));
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
#[test]
fn the_baked_ground_reproduces_the_floor_palette_behind_a_clamped_border() {
    for guardian in Guardian::ALL {
        let raster = art::floor(guardian);
        let colors = palette::material_colors(guardian).map(Rgba::from);
        let image = bake_ground(guardian);
        let frame = image
            .as_bytes(0)
            .expect("a baked ground image holds one frame");
        let (width, height) = (
            (raster.width + 2 * GROUND_BORDER) * GROUND_SUPERSAMPLE,
            (raster.height + 2 * GROUND_BORDER) * GROUND_SUPERSAMPLE,
        );
        assert_eq!(frame.len(), width * height * 4, "{guardian:?}");
        // The screen-space mapping `paint_ground` builds treats the border as
        // `GROUND_BORDER` raster pixels, so every interior texel must resolve
        // back to the ink at that raster pixel.
        for (x, y) in [
            (GROUND_SUPERSAMPLE, GROUND_SUPERSAMPLE),
            (width / 2, height / 2),
            (
                width - GROUND_SUPERSAMPLE - 1,
                height - GROUND_SUPERSAMPLE - 1,
            ),
        ] {
            let sx = x / GROUND_SUPERSAMPLE - GROUND_BORDER;
            let sy = y / GROUND_SUPERSAMPLE - GROUND_BORDER;
            let color = colors[raster.pixels[sy * raster.width + sx] as usize];
            let pixel = &frame[(y * width + x) * 4..(y * width + x) * 4 + 4];
            assert_eq!(
                [
                    (color.r * 255.0).round() as u8,
                    (color.g * 255.0).round() as u8,
                    (color.b * 255.0).round() as u8,
                ],
                [pixel[2], pixel[1], pixel[0]],
                "{guardian:?} at ({x}, {y})"
            );
            assert_eq!(pixel[3], 255, "{guardian:?} at ({x}, {y})");
        }
        // The border repeats the edge so the linear sampler cannot bleed a
        // neighbouring atlas tile in at the seam.
        let edge = &frame[..4];
        let just_inside = &frame[(GROUND_SUPERSAMPLE * 4)..(GROUND_SUPERSAMPLE * 4 + 4)];
        assert_eq!(edge, just_inside, "{guardian:?} top edge");
    }
}

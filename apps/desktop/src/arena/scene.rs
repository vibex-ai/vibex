//! One pixel canvas shared by the idle preview and the playable arena.

use std::{cell::Cell, rc::Rc, sync::OnceLock};

use gpui::{
    AnyElement, App, Background, Bounds, Hsla, IntoElement, Pixels, Point, Styled as _, Window,
    canvas, fill, linear_color_stop, linear_gradient, point, px, size,
};
use gpui_component::ActiveTheme as _;

use super::{
    art,
    combat::{Arena, Guardian, HEIGHT, INTRO_DURATION, Phase, Vec2, WIDTH, smoothstep},
    palette,
    raster::{PixelRect, ink},
};

#[derive(Clone, Copy, Debug)]
pub(super) struct PreviewSample {
    pub guardian: Guardian,
    pub time: f32,
    pub viewport: Option<PreviewViewport>,
}

impl Default for PreviewSample {
    fn default() -> Self {
        Self {
            guardian: Guardian::Claude,
            time: 0.0,
            viewport: None,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct PreviewViewport {
    pub bounds: Bounds<Pixels>,
    pub geometry: Geometry,
}

#[derive(Clone, Copy, Debug, PartialEq)]
pub(super) struct Geometry {
    pub origin: Point<Pixels>,
    pub unit: Pixels,
}

impl Geometry {
    pub fn battle(bounds: Bounds<Pixels>) -> Self {
        let unit = (bounds.size.width / WIDTH).min(bounds.size.height / HEIGHT);
        Self {
            origin: bounds.origin
                + point(
                    (bounds.size.width - unit * WIDTH) * 0.5,
                    (bounds.size.height - unit * HEIGHT) * 0.5,
                ),
            unit,
        }
    }

    fn preview(bounds: Bounds<Pixels>, rem: Pixels) -> Self {
        let unit = (rem * 0.5).min(bounds.size.width / 34.0);
        Self {
            origin: bounds.origin
                + point(
                    bounds.size.width * 0.60 - unit * 48.0,
                    bounds.size.height * 0.78 - unit * 22.0,
                ),
            unit,
        }
    }

    fn interpolate(self, target: Self, progress: f32) -> Self {
        Self {
            origin: self.origin + (target.origin - self.origin) * progress,
            unit: self.unit + (target.unit - self.unit) * progress,
        }
    }

    pub fn world(self, position: Point<Pixels>) -> Option<Vec2> {
        if self.unit <= px(0.0) {
            return None;
        }
        let x = (position.x - self.origin.x) / self.unit;
        let y = (position.y - self.origin.y) / self.unit;
        ((0.0..WIDTH).contains(&x) && (0.0..HEIGHT).contains(&y)).then_some(Vec2::new(x, y))
    }
}

struct Layout {
    rectangles: Vec<PixelRect>,
    geometry: Geometry,
    colors: [Hsla; ink::COUNT],
    ground: Option<[Hsla; ink::COUNT]>,
    fade: Option<(Bounds<Pixels>, Background)>,
}

fn paint(_: Bounds<Pixels>, layout: Layout, window: &mut Window, _: &mut App) {
    if layout.geometry.unit <= px(0.0) {
        return;
    }
    if let Some(colors) = layout.ground {
        paint_rectangles(ground_rectangles(), layout.geometry, &colors, window);
    }
    paint_rectangles(&layout.rectangles, layout.geometry, &layout.colors, window);
    if let Some((bounds, fade)) = layout.fade {
        window.paint_quad(fill(bounds, fade));
    }
}

fn ground_rectangles() -> &'static [PixelRect] {
    static GROUND: OnceLock<Vec<PixelRect>> = OnceLock::new();
    GROUND.get_or_init(|| art::floor().rectangles())
}

fn paint_rectangles(
    rectangles: &[PixelRect],
    geometry: Geometry,
    colors: &[Hsla; ink::COUNT],
    window: &mut Window,
) {
    let pixel = geometry.unit / art::SCALE;
    let scale = window.scale_factor();
    // Snap shared raster edges to device pixels. Fractional viewport fits stay
    // crisp without filtering, seams, per-pixel elements, or retained textures.
    let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
    for rect in rectangles {
        let left = snap(geometry.origin.x + pixel * f32::from(rect.x));
        let top = snap(geometry.origin.y + pixel * f32::from(rect.y));
        let right = snap(geometry.origin.x + pixel * f32::from(rect.x + rect.width));
        let bottom = snap(geometry.origin.y + pixel * f32::from(rect.y + rect.height));
        if right > left && bottom > top {
            window.paint_quad(fill(
                Bounds::new(point(left, top), size(right - left, bottom - top)),
                colors[rect.ink as usize],
            ));
        }
    }
}

pub(super) fn battle(
    arena: &Arena,
    entry: Option<PreviewViewport>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    projection: Rc<Cell<Option<Geometry>>>,
    reduced_motion: bool,
) -> AnyElement {
    let frame = art::battle(arena, reduced_motion);
    let guardian = arena.boss.guardian;
    let reveal = if arena.phase == Phase::Awakening {
        smoothstep(arena.phase_time / INTRO_DURATION)
    } else {
        1.0
    };
    let travel = if reduced_motion {
        1.0
    } else {
        smoothstep(arena.phase_time / (INTRO_DURATION * 0.65))
    };
    let entry = entry.filter(|_| arena.phase == Phase::Awakening);
    let shake = if reduced_motion {
        Vec2::default()
    } else {
        arena.shake()
    };
    canvas(
        move |layout_bounds, _, cx| {
            bounds.set(Some(layout_bounds));
            let mut geometry = Geometry::battle(layout_bounds);
            if let Some(entry) = entry {
                geometry = entry.geometry.interpolate(geometry, travel);
            }
            geometry.origin += point(geometry.unit * shake.x, geometry.unit * shake.y);
            projection.set(Some(geometry));
            Layout {
                rectangles: frame.rectangles(),
                geometry,
                colors: palette::colors(guardian, false, reveal, cx),
                ground: (reveal > 0.0).then(|| palette::ground_colors(guardian, reveal, cx)),
                fade: entry
                    .filter(|_| reveal < 1.0 && !reduced_motion)
                    .map(|entry| {
                        (
                            entry.bounds,
                            linear_gradient(
                                180.0,
                                linear_color_stop(cx.theme().background.opacity(0.0), 0.45),
                                linear_color_stop(cx.theme().background.opacity(1.0 - reveal), 1.0),
                            ),
                        )
                    }),
            }
        },
        paint,
    )
    .size_full()
    .into_any_element()
}

pub(super) fn preview(
    sample: PreviewSample,
    measured: Rc<Cell<PreviewSample>>,
    reduced_motion: bool,
) -> AnyElement {
    canvas(
        move |bounds, window, cx| {
            // This is the last *displayed* pose, not an independent battle clock.
            let geometry = Geometry::preview(bounds, window.rem_size());
            measured.set(PreviewSample {
                viewport: Some(PreviewViewport { bounds, geometry }),
                ..sample
            });
            let frame = art::preview(sample.guardian, sample.time, reduced_motion);
            Layout {
                rectangles: frame.rectangles(),
                geometry,
                colors: palette::colors(sample.guardian, true, 1.0, cx),
                ground: None,
                fade: Some((
                    bounds,
                    linear_gradient(
                        180.0,
                        linear_color_stop(cx.theme().background.opacity(0.0), 0.45),
                        linear_color_stop(cx.theme().background, 1.0),
                    ),
                )),
            }
        },
        paint,
    )
    .size_full()
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::arena::raster::Raster;

    fn assert_same_pixels(expected: &Raster, actual: &Raster, context: &str) {
        assert_eq!(
            (expected.width, expected.height),
            (actual.width, actual.height)
        );
        let difference = expected
            .pixels
            .iter()
            .zip(&actual.pixels)
            .enumerate()
            .find(|(_, (a, b))| a != b);
        assert!(
            difference.is_none(),
            "{context}: first changed pixel {difference:?}"
        );
    }

    #[test]
    fn pointer_coordinates_follow_the_letterboxed_arena() {
        for (width, height) in [(960.0, 560.0), (320.0, 420.0), (1400.0, 400.0)] {
            let bounds = Bounds::new(point(px(17.0), px(31.0)), size(px(width), px(height)));
            let geometry = Geometry::battle(bounds);
            let center = geometry.origin + point(geometry.unit * 48.0, geometry.unit * 28.0);
            let world = geometry.world(center).unwrap();
            assert!((world.x - 48.0).abs() < 0.001);
            assert!((world.y - 28.0).abs() < 0.001);
            assert!(
                geometry
                    .world(geometry.origin - point(px(1.0), px(1.0)))
                    .is_none()
            );
        }
    }

    #[test]
    fn merged_rectangles_reconstruct_the_exact_raster() {
        for guardian in Guardian::ALL {
            let actors = art::battle(&Arena::new(guardian, 0.0), false);
            let mut frame = art::floor().clone();
            frame.blit(&actors, 0, 0);
            let mut rectangles = ground_rectangles().to_vec();
            rectangles.extend(actors.rectangles());
            assert!(
                rectangles.len() < 12_000,
                "{guardian:?}: {} quads",
                rectangles.len()
            );
            let mut restored = Raster::new(art::WIDTH, art::HEIGHT);
            for rect in rectangles {
                assert!((rect.ink as usize) < ink::COUNT);
                restored.rect(
                    i32::from(rect.x),
                    i32::from(rect.y),
                    i32::from(rect.width),
                    i32::from(rect.height),
                    rect.ink,
                );
            }
            assert_same_pixels(&frame, &restored, &format!("{guardian:?} rectangles"));
        }
    }

    #[test]
    fn idle_preview_has_no_archer_and_entry_keeps_the_displayed_pose() {
        for guardian in Guardian::ALL {
            for time in [0.0, 0.37, 2.75, 5.99] {
                let preview = art::preview(guardian, time, false);
                assert!(
                    !preview
                        .pixels
                        .iter()
                        .any(|color| matches!(*color, ink::SKIN | ink::CAPE | ink::CLOTH))
                );
                let arena = Arena::new(guardian, time);
                assert_same_pixels(
                    &preview,
                    &art::battle(&arena, false),
                    &format!("{guardian:?} entry"),
                );
                assert_eq!(
                    art::pose(time, 0.0, false),
                    art::pose(arena.visual_time, arena.phase_time / INTRO_DURATION, false)
                );
            }
        }
    }

    #[test]
    fn reduced_motion_freezes_every_idle_guardian_and_preview_loops_join() {
        for guardian in Guardian::ALL {
            let still = art::preview(guardian, 0.0, true);
            for time in [0.37, 2.75, 5.99] {
                assert_same_pixels(
                    &still,
                    &art::preview(guardian, time, true),
                    &format!("{guardian:?} reduced motion"),
                );
            }
            assert_same_pixels(
                &art::preview(guardian, 0.0, false),
                &art::preview(guardian, art::IDLE_PERIOD, false),
                &format!("{guardian:?} idle loop"),
            );
        }
    }

    #[test]
    fn the_entry_camera_starts_at_the_preview_and_settles_in_the_field() {
        let preview = Geometry::preview(
            Bounds::new(point(px(80.0), px(32.0)), size(px(960.0), px(192.0))),
            px(16.0),
        );
        let field = Geometry::battle(Bounds::new(
            point(px(80.0), px(140.0)),
            size(px(960.0), px(500.0)),
        ));
        assert_eq!(preview.interpolate(field, 0.0), preview);
        assert_eq!(preview.interpolate(field, 1.0), field);
        let middle = preview.interpolate(field, 0.5);
        let center = middle.origin + point(middle.unit * 48.0, middle.unit * 22.0);
        let world = middle.world(center).unwrap();
        assert!((world.x - 48.0).abs() < 0.001);
        assert!((world.y - 22.0).abs() < 0.001);
    }

    #[gpui::test]
    fn entry_colors_join_the_home_theme_before_revealing_the_materials(
        cx: &mut gpui::TestAppContext,
    ) {
        cx.update(|cx| {
            gpui_component::init(cx);
            for mode in [
                gpui_component::ThemeMode::Light,
                gpui_component::ThemeMode::Dark,
            ] {
                gpui_component::Theme::change(mode, None, cx);
                for guardian in Guardian::ALL {
                    assert_eq!(
                        palette::colors(guardian, false, 0.0, cx),
                        palette::colors(guardian, true, 1.0, cx)
                    );
                    assert_eq!(
                        palette::colors(guardian, false, 1.0, cx),
                        palette::material_colors(guardian)
                    );
                    assert!(
                        palette::ground_colors(guardian, 0.0, cx)
                            .iter()
                            .all(|color| *color == cx.theme().background)
                    );
                }
            }
        });
    }
}

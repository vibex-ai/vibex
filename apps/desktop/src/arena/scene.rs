//! Camera, native entry hit target and one clipped pixel canvas. Aim conversion
//! always uses the last displayed camera, including its transient shake.

use super::{
    HomeArena, art,
    combat::{Arena, Boss, Guardian, HEIGHT, INTRO_DURATION, Phase, Vec2, WIDTH, smoothstep},
    map::CENTER,
    palette,
    raster::{PixelRect, Raster, ink},
};
use gpui::{
    AnyElement, App, AvailableSpace, Bounds, Context, Element, ElementId, GlobalElementId, Hsla,
    InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, Pixels, Point, Style,
    Styled as _, WeakEntity, Window, canvas, fill, point, px, relative, size,
};
use gpui_component::button::{Button, ButtonVariants as _};
use std::{cell::Cell, rc::Rc, sync::OnceLock};

#[derive(Clone, Copy, Debug)]
pub(super) struct PreviewSample {
    pub guardian: Guardian,
    pub time: f32,
    pub boss: Boss,
    pub viewport: Option<PreviewViewport>,
}
impl PreviewSample {
    pub fn at(guardian: Guardian, time: f32, reduced: bool) -> Self {
        Self {
            guardian,
            time,
            boss: Boss::idle(guardian, time, reduced),
            viewport: None,
        }
    }
}
impl Default for PreviewSample {
    fn default() -> Self {
        Self::at(Guardian::Claude, 0.0, true)
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
    pub fn battle(bounds: Bounds<Pixels>, camera: Vec2, rem: Pixels) -> Self {
        let unit = (rem * 0.52)
            .max(bounds.size.width / (WIDTH - 6.0))
            .max(bounds.size.height / (HEIGHT - 6.0));
        let half = Vec2::new(
            bounds.size.width / unit * 0.5,
            bounds.size.height / unit * 0.5,
        );
        let camera = Vec2::new(
            camera.x.clamp(half.x, WIDTH - half.x),
            camera.y.clamp(half.y - 20.0, HEIGHT + 8.0 - half.y),
        );
        Self {
            origin: bounds.center() - point(unit * camera.x, unit * camera.y),
            unit,
        }
    }
    pub fn preview(bounds: Bounds<Pixels>, rem: Pixels) -> Self {
        let unit = (rem * 0.52)
            .min(bounds.size.width / 152.0)
            .min(bounds.size.height / 128.0);
        Self {
            // Reserve room for the winged guardian above its ground anchor.
            origin: bounds.center() - point(unit * CENTER.x, unit * (CENTER.y - 5.0)),
            unit,
        }
    }
    fn interpolate(self, target: Self, t: f32) -> Self {
        Self {
            origin: self.origin + (target.origin - self.origin) * t,
            unit: self.unit + (target.unit - self.unit) * t,
        }
    }
    #[cfg(test)]
    pub fn screen(self, p: Vec2) -> Point<Pixels> {
        self.origin + point(self.unit * p.x, self.unit * p.y)
    }
    pub fn world(self, p: Point<Pixels>) -> Option<Vec2> {
        if self.unit <= px(0.0) {
            return None;
        }
        let p = Vec2::new(
            (p.x - self.origin.x) / self.unit,
            (p.y - self.origin.y) / self.unit,
        );
        ((0.0..WIDTH).contains(&p.x) && (0.0..HEIGHT).contains(&p.y)).then_some(p)
    }
}
pub(super) struct Layout {
    rectangles: Vec<PixelRect>,
    geometry: Geometry,
    colors: [Hsla; ink::COUNT],
    ground: Option<(Guardian, [Hsla; ink::COUNT])>,
    background: Option<Hsla>,
}
fn paint(bounds: Bounds<Pixels>, layout: Layout, window: &mut Window, _: &mut App) {
    if layout.geometry.unit <= px(0.0) {
        return;
    }
    if let Some(color) = layout.background {
        window.paint_quad(fill(bounds, color));
    }
    if let Some((guardian, colors)) = layout.ground {
        paint_rectangles(
            ground_rectangles(guardian),
            layout.geometry,
            &colors,
            window,
        );
    }
    paint_rectangles(&layout.rectangles, layout.geometry, &layout.colors, window);
}
fn ground_rectangles(guardian: Guardian) -> &'static [PixelRect] {
    static GROUND: OnceLock<[Vec<PixelRect>; 6]> = OnceLock::new();
    &GROUND.get_or_init(|| Guardian::ALL.map(|g| art::floor(g).rectangles()))[guardian as usize]
}
fn paint_rectangles(
    rectangles: &[PixelRect],
    geometry: Geometry,
    colors: &[Hsla; ink::COUNT],
    window: &mut Window,
) {
    let pixel = geometry.unit / art::SCALE;
    let scale = window.scale_factor();
    let clip = window.content_mask().bounds;
    let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
    for rect in rectangles {
        let left = snap(geometry.origin.x + pixel * f32::from(rect.x));
        let top = snap(geometry.origin.y + pixel * (f32::from(rect.y) - art::TOP_PAD as f32));
        let right = snap(geometry.origin.x + pixel * f32::from(rect.x + rect.width));
        let bottom = snap(
            geometry.origin.y + pixel * (f32::from(rect.y + rect.height) - art::TOP_PAD as f32),
        );
        if right > left
            && bottom > top
            && right >= clip.left()
            && left <= clip.right()
            && bottom >= clip.top()
            && top <= clip.bottom()
        {
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
    reduced: bool,
    paused: bool,
) -> AnyElement {
    let mut frame = art::battle(arena, reduced);
    if paused && arena.phase == Phase::Battle {
        let (x, y) = art::pixel(arena.player.position);
        frame.rect(x - 5, y - 27, 3, 8, ink::IVORY);
        frame.rect(x + 2, y - 27, 3, 8, ink::IVORY);
    }
    let guardian = arena.boss.guardian;
    let reveal = arena.ground_visibility();
    let opacity = if arena.phase == Phase::Victory {
        reveal
    } else {
        1.0
    };
    let camera = arena.camera;
    let travel = if reduced {
        1.0
    } else {
        smoothstep(arena.phase_time / (INTRO_DURATION * 0.95))
    };
    let entry = entry.filter(|_| arena.phase == Phase::Awakening);
    let shake = if reduced {
        Vec2::default()
    } else {
        arena.shake()
    };
    canvas(
        move |layout_bounds, window, _| {
            bounds.set(Some(layout_bounds));
            let mut geometry = Geometry::battle(layout_bounds, camera, window.rem_size());
            if let Some(entry) = entry {
                geometry = entry.geometry.interpolate(geometry, travel);
            }
            geometry.origin += point(geometry.unit * shake.x, geometry.unit * shake.y);
            projection.set(Some(geometry));
            let material = palette::material_colors(guardian);
            Layout {
                rectangles: frame.rectangles(),
                geometry,
                colors: material.map(|c| c.opacity(opacity)),
                ground: (reveal > 0.0).then(|| (guardian, material.map(|c| c.opacity(reveal)))),
                background: (reveal > 0.0).then(|| material[ink::DEPTH as usize].opacity(reveal)),
            }
        },
        paint,
    )
    .size_full()
    .into_any_element()
}

/// The custom element only measures a moving raster. A normal Button still
/// owns keyboard activation, focus, accessibility and pointer dispatch.
pub(super) struct Entry {
    sample: PreviewSample,
    measured: Rc<Cell<PreviewSample>>,
    reduced: bool,
    button: AnyElement,
}
impl Entry {
    pub fn new(
        sample: PreviewSample,
        measured: Rc<Cell<PreviewSample>>,
        reduced: bool,
        state: WeakEntity<HomeArena>,
    ) -> Self {
        let button = Button::new("open-unbound")
            .debug_selector(|| "unbound-boss-entry".into())
            .ghost()
            .w_full()
            .h_full()
            .p_0()
            .accessibility_label(super::copy::entry_label(sample.guardian))
            .tooltip(super::copy::entry_help())
            .on_click(move |_, window, cx| {
                let _ = state.update(cx, |state, cx: &mut Context<HomeArena>| {
                    state.open(window, cx)
                });
            })
            .into_any_element();
        Self {
            sample,
            measured,
            reduced,
            button,
        }
    }
}
impl IntoElement for Entry {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Entry {
    type RequestLayoutState = ();
    type PrepaintState = Layout;
    fn id(&self) -> Option<ElementId> {
        Some("unbound-roaming-entry".into())
    }
    fn source_location(&self) -> Option<&'static std::panic::Location<'static>> {
        None
    }
    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let mut style = Style::default();
        style.size.width = relative(1.0).into();
        style.size.height = relative(1.0).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) -> Layout {
        let geometry = Geometry::preview(bounds, window.rem_size());
        self.measured.set(PreviewSample {
            viewport: Some(PreviewViewport { bounds, geometry }),
            ..self.sample
        });
        let frame = art::idle(&self.sample.boss, self.sample.time, self.reduced);
        let (left, top, right, bottom) = model_bounds(&frame);
        let pixel = geometry.unit / art::SCALE;
        let hit_origin = geometry.origin
            + point(
                pixel * left as f32,
                pixel * (top as f32 - art::TOP_PAD as f32),
            );
        self.button.layout_as_root(
            size(
                AvailableSpace::Definite(pixel * (right - left) as f32),
                AvailableSpace::Definite(pixel * (bottom - top) as f32),
            ),
            window,
            cx,
        );
        self.button.prepaint_at(hit_origin, window, cx);
        Layout {
            rectangles: frame.rectangles(),
            geometry,
            colors: palette::material_colors(self.sample.guardian),
            ground: None,
            background: None,
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        layout: &mut Layout,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.button.paint(window, cx);
        paint_rectangles(&layout.rectangles, layout.geometry, &layout.colors, window);
        let _ = bounds;
    }
}
fn model_bounds(frame: &Raster) -> (usize, usize, usize, usize) {
    let mut left = frame.width;
    let mut top = frame.height;
    let mut right = 0;
    let mut bottom = 0;
    for (ix, color) in frame.pixels.iter().enumerate() {
        if *color != ink::CLEAR && *color != ink::SHADOW {
            let x = ix % frame.width;
            let y = ix / frame.width;
            left = left.min(x);
            right = right.max(x + 1);
            top = top.min(y);
            bottom = bottom.max(y + 1);
        }
    }
    (left.min(right), top.min(bottom), right, bottom)
}

#[cfg(test)]
#[path = "scene_tests.rs"]
mod tests;

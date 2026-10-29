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
    AnyElement, App, AvailableSpace, Bounds, Context, Corners, Element, ElementId, GlobalElementId,
    Hsla, InspectorElementId, InteractiveElement as _, IntoElement, LayoutId, ParentElement as _,
    Pixels, Point, RenderImage, Rgba, Style, Styled as _, WeakEntity, Window, canvas, div, fill,
    point, px, relative, size,
};
use gpui_component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use image::{Frame, RgbaImage};
use std::{
    cell::Cell,
    rc::Rc,
    sync::{Arc, OnceLock},
};

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
            .min(bounds.size.width / 176.0)
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
/// The static sanctuary ground, painted as one image instead of the thousands
/// of merged rectangles it decomposes into. The baked texture already carries
/// the palette, so a frame only pays for a single textured quad.
struct GroundLayout {
    geometry: Geometry,
    image: Arc<RenderImage>,
    background: Hsla,
}
/// The moving parts of a battle frame: the actor raster's merged rectangles,
/// resolved against the same camera geometry the baked ground uses.
pub(super) struct ActorsLayout {
    rectangles: Vec<PixelRect>,
    geometry: Geometry,
    colors: [Hsla; ink::COUNT],
}

fn paint_ground(bounds: Bounds<Pixels>, layout: GroundLayout, window: &mut Window, _: &mut App) {
    if layout.geometry.unit <= px(0.0) {
        return;
    }
    window.paint_quad(fill(bounds, layout.background));
    let pixel = layout.geometry.unit / art::SCALE;
    let origin = layout.geometry.origin;
    // One raster pixel maps to `pixel` logical pixels. The baked texture stores
    // `GROUND_SUPERSAMPLE` texels per raster pixel but covers the same screen
    // area, so only the clamp border shifts the bounds: `image_bounds` reaches
    // `GROUND_BORDER` raster pixels past the art on every side while `content`
    // shows only the art, and the atlas's linear sampler can never pull a
    // neighbouring tile's texels in.
    let border = GROUND_BORDER as f32;
    let top = origin.y - pixel * (art::TOP_PAD as f32 + border);
    let image = Bounds::new(
        point(origin.x - pixel * border, top),
        size(
            pixel * (art::WIDTH as f32 + 2.0 * border),
            pixel * (art::HEIGHT as f32 + 2.0 * border),
        ),
    );
    let content = Bounds::new(
        point(origin.x, origin.y - pixel * art::TOP_PAD as f32),
        size(pixel * art::WIDTH as f32, pixel * art::HEIGHT as f32),
    );
    let _ = window.paint_image(content, image, Corners::default(), layout.image, 0, false);
}
fn paint_actors(_: Bounds<Pixels>, layout: ActorsLayout, window: &mut Window, _: &mut App) {
    if layout.geometry.unit <= px(0.0) {
        return;
    }
    paint_rectangles(&layout.rectangles, layout.geometry, &layout.colors, window);
}
/// The ground raster for `guardian`, converted once into a GPU texture and kept
/// for the life of the process. Every floor is fully opaque, so it needs no
/// blending beyond the quad's own element opacity.
fn ground_image(guardian: Guardian) -> Arc<RenderImage> {
    static IMAGES: OnceLock<[OnceLock<Arc<RenderImage>>; Guardian::ALL.len()]> = OnceLock::new();
    let images = IMAGES.get_or_init(|| std::array::from_fn(|_| OnceLock::new()));
    images[guardian as usize]
        .get_or_init(|| Arc::new(bake_ground(guardian)))
        .clone()
}
/// A one-raster-pixel clamp border repeats the raster's edge so the atlas's
/// linear sampler blends each edge pixel with a copy of itself, never with a
/// neighbouring tile or a transparent gap.
const GROUND_BORDER: usize = 1;
/// The atlas's only sampler filters linearly (this GPUI has no nearest filter),
/// so a texture at the raster's own resolution would soften every one-pixel
/// seam and ring. Each raster pixel is stored as a 2x2 block instead, which
/// halves the edge ramp: about one device pixel on a 1x display and about one
/// logical pixel on a 2x display. A larger factor would minify on 1x displays,
/// where the atlas keeps no mipmaps and would shimmer while the camera pans.
const GROUND_SUPERSAMPLE: usize = 2;
fn bake_ground(guardian: Guardian) -> RenderImage {
    let raster = art::floor(guardian);
    let colors = palette::material_colors(guardian).map(Rgba::from);
    let (width, height) = (raster.width, raster.height);
    let (baked_width, baked_height) = (
        (width + 2 * GROUND_BORDER) * GROUND_SUPERSAMPLE,
        (height + 2 * GROUND_BORDER) * GROUND_SUPERSAMPLE,
    );
    let mut bytes = vec![0u8; baked_width * baked_height * 4];
    for y in 0..baked_height {
        for x in 0..baked_width {
            // Fold the supersample back into a raster pixel, clamping through
            // the border so the edge repeats outwards.
            let sx = (x / GROUND_SUPERSAMPLE) as i32 - GROUND_BORDER as i32;
            let sy = (y / GROUND_SUPERSAMPLE) as i32 - GROUND_BORDER as i32;
            let sx = sx.clamp(0, width as i32 - 1) as usize;
            let sy = sy.clamp(0, height as i32 - 1) as usize;
            let color = colors[raster.pixels[sy * width + sx] as usize];
            let pixel = &mut bytes[(y * baked_width + x) * 4..(y * baked_width + x) * 4 + 4];
            // The atlas uploads straight-alpha BGRA.
            pixel[0] = (color.b * 255.0).round().clamp(0.0, 255.0) as u8;
            pixel[1] = (color.g * 255.0).round().clamp(0.0, 255.0) as u8;
            pixel[2] = (color.r * 255.0).round().clamp(0.0, 255.0) as u8;
            pixel[3] = (color.a * 255.0).round().clamp(0.0, 255.0) as u8;
        }
    }
    let image = RgbaImage::from_raw(baked_width as u32, baked_height as u32, bytes)
        .expect("a baked ground image matches its raster dimensions");
    RenderImage::new(vec![Frame::new(image)])
}
#[cfg(test)]
fn ground_rectangles(guardian: Guardian) -> &'static [PixelRect] {
    static GROUND: OnceLock<[Vec<PixelRect>; Guardian::ALL.len()]> = OnceLock::new();
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
    let material = palette::material_colors(guardian);
    let actor_colors = material.map(|color| color.opacity(opacity));
    let rectangles = frame.rectangles();
    // The sanctuary ground is one baked texture: the camera geometry resolves at
    // prepaint, then a single textured quad replaces the thousands of merged
    // rectangles the floor decomposes into. The element opacity carries the
    // ground's own reveal, which the actors' colors do not share.
    let ground = canvas(
        {
            let bounds = bounds.clone();
            let projection = projection.clone();
            let image = ground_image(guardian);
            move |layout_bounds, window, _| {
                bounds.set(Some(layout_bounds));
                let mut geometry = Geometry::battle(layout_bounds, camera, window.rem_size());
                if let Some(entry) = entry {
                    geometry = entry.geometry.interpolate(geometry, travel);
                }
                geometry.origin += point(geometry.unit * shake.x, geometry.unit * shake.y);
                projection.set(Some(geometry));
                GroundLayout {
                    geometry,
                    image: image.clone(),
                    background: material[ink::DEPTH as usize],
                }
            }
        },
        paint_ground,
    )
    .size_full();
    let actors = canvas(
        move |layout_bounds, window, _| {
            let mut geometry = Geometry::battle(layout_bounds, camera, window.rem_size());
            if let Some(entry) = entry {
                geometry = entry.geometry.interpolate(geometry, travel);
            }
            geometry.origin += point(geometry.unit * shake.x, geometry.unit * shake.y);
            ActorsLayout {
                rectangles,
                geometry,
                colors: actor_colors,
            }
        },
        paint_actors,
    )
    .size_full();
    div()
        .relative()
        .size_full()
        .child(
            div()
                .absolute()
                .inset_0()
                .size_full()
                .opacity(reveal)
                .child(ground),
        )
        .child(div().absolute().inset_0().size_full().child(actors))
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
        chrome: ButtonCustomVariant,
    ) -> Self {
        // The guardian art is the whole control, so the button paints nothing
        // in any state: a variant that keeps hover and active transparent means
        // the model never gets a rectangle washed over it, and the pointing
        // hand stays the only affordance.
        let button = Button::new("open-unbound")
            .debug_selector(|| "unbound-boss-entry".into())
            .custom(chrome)
            .cursor_pointer()
            .w_full()
            .h_full()
            .p_0()
            .accessibility_label(super::copy::entry_label(sample.guardian))
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
    type PrepaintState = ActorsLayout;
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
    ) -> ActorsLayout {
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
        ActorsLayout {
            rectangles: frame.rectangles(),
            geometry,
            colors: palette::material_colors(self.sample.guardian),
        }
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        layout: &mut ActorsLayout,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.button.paint(window, cx);
        paint_rectangles(&layout.rectangles, layout.geometry, &layout.colors, window);
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

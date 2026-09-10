//! Frosted-glass surfaces: `frosted` wraps a floating card (popover or
//! dialog) so its entire subtree paints inside one scene layer with a
//! backdrop blur painted first, and `glass_tint` provides the translucent
//! card tint painted over that blur.
//!
//! The single layer order is load-bearing: with per-primitive draw ordering,
//! a hover repaint elsewhere could reassign the card's quads BELOW the blur,
//! letting washes and borders get snapshotted and blurred away. Inside one
//! layer the blur/content relationship is structural: blur first, then tint,
//! border, and content.
//!
//! Whether a card is actually frosted is decided by the global
//! [`GlassSettings`], applied by the desktop appearance layer. On renderers
//! that cannot snapshot the framebuffer, `paint_backdrop_blur` degrades to
//! nothing and the caller's translucent tint stays readable, so surfaces can
//! opt in unconditionally.

use gpui::{
    AnyElement, App, Bounds, Corners, Element, Global, GlobalElementId, InspectorElementId,
    IntoElement, LayoutId, Pixels, Window, px,
};

/// Default blur sigma for floating cards. Roughly CSS `blur(24px)`: enough
/// to dissolve text behind a card without making the backdrop unreadable.
pub const DEFAULT_GLASS_BLUR_RADIUS: f32 = 24.0;

/// Fraction of the surface tint kept over the blurred backdrop. The rest of
/// the theme color's alpha yields to what the blur resolved behind the card.
pub const DEFAULT_GLASS_TINT_OPACITY: f32 = 0.82;

/// Whether the platform's GPUI renderer implements backdrop blur.
///
/// Windows renders through the vendored DirectX pipeline, which never reads
/// `BackdropBlur` primitives, so frosted cards would silently degrade to
/// translucent tints over an opaque window there. Linux, macOS, and the
/// mobile/web renderers (wgpu / Metal) snapshot and blur the framebuffer. The
/// appearance layer uses this to keep the preference off and hide its
/// settings entries instead of exposing a control with no effect.
pub fn platform_supports_backdrop_blur() -> bool {
    #[cfg(target_os = "windows")]
    {
        false
    }
    #[cfg(not(target_os = "windows"))]
    {
        true
    }
}

/// Global frosted-glass preference.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct GlassSettings {
    /// Paint floating cards over a blurred snapshot of what is behind them.
    pub enabled: bool,
    /// Blur sigma in logical pixels for frosted cards.
    pub blur_radius: f32,
    /// Opacity multiplier applied to card tints under glass so content stays
    /// readable over the blurred backdrop.
    pub tint_opacity: f32,
}

impl Global for GlassSettings {}

impl Default for GlassSettings {
    fn default() -> Self {
        Self {
            enabled: false,
            blur_radius: DEFAULT_GLASS_BLUR_RADIUS,
            tint_opacity: DEFAULT_GLASS_TINT_OPACITY,
        }
    }
}

impl GlassSettings {
    /// Whether the current settings would actually frost a card.
    pub fn frosted(&self) -> bool {
        self.enabled && self.blur_radius > 0.0
    }

    /// Tint for a card surface: `color` thinned by the tint opacity so the
    /// blurred backdrop shows through without washing out content.
    pub fn tint(&self, color: gpui::Hsla) -> gpui::Hsla {
        if self.frosted() {
            color.opacity(color.a * self.tint_opacity.clamp(0.0, 1.0))
        } else {
            color
        }
    }
}

/// Read the active glass settings. Defaults to solid (glass off) until the
/// appearance layer applies the user preference.
pub fn glass_settings(cx: &App) -> GlassSettings {
    cx.try_global::<GlassSettings>()
        .copied()
        .unwrap_or_default()
}

/// Store the active glass settings (called by the desktop appearance layer).
pub fn apply_glass_settings(settings: GlassSettings, cx: &mut App) {
    cx.set_global(settings);
}

/// Convenience: tint a theme surface color for a glass card — thinned under
/// glass, unchanged otherwise.
pub fn glass_tint(color: gpui::Hsla, cx: &App) -> gpui::Hsla {
    glass_settings(cx).tint(color)
}

/// Frost `child` (a floating card): backdrop-blurred under glass, pass-through
/// otherwise. `corner_radius` must match the card's own rounding.
pub fn frosted(corner_radius: f32, child: impl IntoElement) -> Frosted {
    Frosted {
        corner_radius,
        child: child.into_any_element(),
    }
}

pub struct Frosted {
    corner_radius: f32,
    child: AnyElement,
}

impl Element for Frosted {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        let settings = glass_settings(cx);
        if settings.frosted() {
            window.paint_layer(bounds, |window| {
                window.paint_backdrop_blur(
                    bounds,
                    Corners::all(px(self.corner_radius)),
                    px(settings.blur_radius),
                );
                self.child.paint(window, cx);
            });
        } else {
            self.child.paint(window, cx);
        }
    }
}

impl IntoElement for Frosted {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

/// Paint `child` in its own scene layer, giving it a fresh draw order above
/// everything painted so far in the enclosing layer. Needed for overlays
/// INSIDE a frosted card: the card's single layer means every primitive
/// shares one draw order, and equal orders render grouped by primitive kind —
/// so a close button painted "after" a thumbnail still shows up UNDER the
/// image. A nested layer restores the intended stacking.
pub fn layered(child: impl IntoElement) -> Layered {
    Layered {
        child: child.into_any_element(),
    }
}

pub struct Layered {
    child: AnyElement,
}

impl Element for Layered {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<gpui::ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        (self.child.request_layout(window, cx), ())
    }

    fn prepaint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        _bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        window: &mut Window,
        cx: &mut App,
    ) {
        self.child.prepaint(window, cx);
    }

    fn paint(
        &mut self,
        _id: Option<&GlobalElementId>,
        _inspector_id: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _request_layout: &mut Self::RequestLayoutState,
        _prepaint: &mut Self::PrepaintState,
        window: &mut Window,
        cx: &mut App,
    ) {
        window.paint_layer(bounds, |window| self.child.paint(window, cx));
    }
}

impl IntoElement for Layered {
    type Element = Self;

    fn into_element(self) -> Self::Element {
        self
    }
}

//! A loading spinner that renders at a bounded frame rate.
//!
//! gpui-component's [`Spinner`](gpui_component::spinner::Spinner) repeats a
//! `with_animation` rotation, and a repeating animation asks the window for a
//! frame on every display refresh for as long as it is mounted. A spinner in
//! the sidebar therefore repainted the whole workbench at the panel rate even
//! when nothing else was moving.
//!
//! This is the same spinner — same icon, size, easing, and color — with
//! [`Animation::with_max_fps`] set so the rotation is sampled a few dozen times
//! a second instead of every refresh. The sweep is far slower than the sample
//! rate, so the throttle is invisible at the sizes the app draws.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Hsla, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Transformation, Window, div, ease_in_out, percentage,
    prelude::FluentBuilder as _,
};
use gpui_component::{Icon, IconName, Sizable, Size};

/// Frames per second the spinner rotation is sampled at.
///
/// The loader completes one turn every 0.8s; at 30fps that is a 12° step, well
/// below what the eye can resolve on an icon this small, while the workbench
/// repaints at less than half the frames a 75Hz panel would otherwise demand.
const SPINNER_MAX_FPS: f32 = 30.0;

/// A cycling loading spinner.
#[derive(IntoElement)]
pub struct Spinner {
    size: Size,
    icon: Icon,
    speed: Duration,
    easing: Box<dyn Fn(f32) -> f32>,
    color: Option<Hsla>,
}

impl Spinner {
    /// Create a new loading spinner.
    pub fn new() -> Self {
        Self {
            size: Size::Medium,
            speed: Duration::from_secs_f64(0.8),
            easing: Box::new(ease_in_out),
            icon: Icon::new(IconName::Loader),
            color: None,
        }
    }

    /// Set specified icon for the spinner.
    ///
    /// Default is [`IconName::Loader`].
    ///
    /// Please ensure the icon used is suitable for a loading spinner.
    pub fn icon(mut self, icon: impl Into<Icon>) -> Self {
        self.icon = icon.into();
        self
    }

    /// Set the icon color.
    pub fn color(mut self, color: Hsla) -> Self {
        self.color = Some(color);
        self
    }

    /// Set the easing function.
    pub fn ease(mut self, easing: impl Fn(f32) -> f32 + 'static) -> Self {
        self.easing = Box::new(easing);
        self
    }
}

impl Default for Spinner {
    fn default() -> Self {
        Self::new()
    }
}

impl Sizable for Spinner {
    fn with_size(mut self, size: impl Into<Size>) -> Self {
        self.size = size.into();
        self
    }
}

impl RenderOnce for Spinner {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        div()
            .child(
                self.icon
                    .with_size(self.size)
                    .when_some(self.color, |this, color| this.text_color(color))
                    .with_animation(
                        "circle",
                        Animation::new(self.speed)
                            .repeat()
                            .with_easing(self.easing)
                            .with_max_fps(SPINNER_MAX_FPS),
                        |this, delta| this.transform(Transformation::rotate(percentage(delta))),
                    ),
            )
            .into_element()
    }
}

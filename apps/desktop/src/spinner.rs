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
//!
//! A spinner in a window nobody is looking at is not animated at all: the
//! icon is drawn from the same rest angle, repeating animation and all. An
//! inactive window still gets its frames throttled by the platform, but a
//! parked workbench with a running session would keep asking for them forever;
//! drawing still is what actually stops the clock. Window activation itself
//! repaints the window (gpui refreshes on every active-status change), so the
//! rotation resumes the moment the window comes back.

use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, Hsla, IntoElement, ParentElement as _, RenderOnce,
    Styled as _, Transformation, Window, div, ease_in_out, percentage, prelude::FluentBuilder as _,
};
use gpui_component::{Icon, IconName, Sizable, Size};

/// Frames per second a sidebar status spinner is sampled at.
///
/// The loader completes one turn every 0.8s; at 30fps that is a 12° step, well
/// below what the eye can resolve on an icon this small, while the workbench
/// repaints at less than half the frames a 75Hz panel would otherwise demand.
const SPINNER_MAX_FPS: f32 = 30.0;

/// Sample rate for the small status indicators embedded in the sidebar and
/// other always-mounted chrome.
///
/// These are 12px marks the eye reads as "still running", not as motion. At
/// 10fps the loader steps 45° per frame, which reads as a pulse rather than a
/// smooth turn — and it asks the window for a third of the frames. Since such
/// an indicator is mounted for as long as its session runs, that difference is
/// the difference between a mostly idle main thread and a busy one.
pub const STATUS_INDICATOR_MAX_FPS: f32 = 10.0;

/// A cycling loading spinner.
#[derive(IntoElement)]
pub struct Spinner {
    size: Size,
    icon: Icon,
    speed: Duration,
    easing: Box<dyn Fn(f32) -> f32>,
    color: Option<Hsla>,
    max_fps: f32,
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
            max_fps: SPINNER_MAX_FPS,
        }
    }

    /// A spinner for always-mounted status chrome — see
    /// [`STATUS_INDICATOR_MAX_FPS`].
    pub fn status_indicator() -> Self {
        Self::new().fps(STATUS_INDICATOR_MAX_FPS)
    }

    /// Sample the rotation at `fps` frames per second instead of the default.
    pub fn fps(mut self, fps: f32) -> Self {
        self.max_fps = fps.clamp(1.0, 240.0);
        self
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
    fn render(self, window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let Self {
            size,
            icon,
            speed,
            easing,
            color,
            max_fps,
        } = self;
        let icon = icon
            .with_size(size)
            .when_some(color, |this, color| this.text_color(color));
        if !window.is_window_active() {
            return div().child(icon).into_any_element();
        }
        div()
            .child(
                icon.with_animation(
                    "circle",
                    Animation::new(speed)
                        .repeat()
                        .with_easing(easing)
                        .with_max_fps(max_fps),
                    |this, delta| this.transform(Transformation::rotate(percentage(delta))),
                ),
            )
            .into_any_element()
    }
}

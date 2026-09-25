//! Motion kit — the shared easing curves, entrance specs, and animated hover
//! washes used across the desktop workbench.
//!
//! GPUI's `.hover()` styles snap by construction — the wash applies the frame
//! the pointer enters. The app-level convention is Tailwind `transition-colors`
//! (150ms, cubic-bezier(0.4, 0, 0.2, 1)) on every interactive wash, so hover
//! states fade. The [`HoverFades`] store below drives that manually: a
//! per-element-key hover progress, advanced from wall time on each render, with
//! [`schedule_hover_frames`] keeping frames coming while any fade is mid-flight.
//!
//! A fade is owned by the view whose render reads it, and a flip notifies that
//! owner — not the window. `Window::refresh` marks the whole window dirty and
//! sets `refreshing`, which defeats every `.cached()` subtree in the path; a
//! hover over one row would then rebuild the code workbench, the terminal and
//! the management centre as well.
//!
//! Never use `with_animation` for hover blends: its element-id-keyed clock
//! replays from 0 on remount, and a remount mid-hover is a full-opacity flash.
//!
//! Reduced motion: gpui's `App::reduce_motion` flag is honored automatically by
//! every `with_animation` element — oneshots snap to their end state, repeating
//! ones to their start state, and no frames are scheduled. The hover fades read
//! the same flag in [`hover_listener`] and snap instead of tweening. The
//! effective flag is the OR of the user's reduced-motion preference and — only
//! while the "pause animation when inactive" setting is on — the window not
//! being active.
//!
//! translateY is implemented as a `top` inset: taffy applies relative insets
//! after layout, so — like a CSS transform — siblings never move. Entrances
//! never set `position` themselves; an element's own positioning (relative is
//! the gpui default) must survive the animation untouched.

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationElement, App, ElementId, EntityId, Hsla, IntoElement, Rgba, SharedString,
    Styled, Window, px,
};

pub use gpui::AnimationExt;

// ---------------------------------------------------------------------------
// Cubic bezier
// ---------------------------------------------------------------------------

/// A CSS `cubic-bezier(x1, y1, x2, y2)` timing function (endpoints fixed at
/// (0,0) and (1,1)). Evaluation solves x(t) = input by Newton iteration with a
/// bisection fallback — the standard UnitBezier approach.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CubicBezier {
    pub x1: f32,
    pub y1: f32,
    pub x2: f32,
    pub y2: f32,
}

impl CubicBezier {
    pub const fn new(x1: f32, y1: f32, x2: f32, y2: f32) -> Self {
        Self { x1, y1, x2, y2 }
    }

    fn coefficients(a: f32, b: f32) -> (f32, f32) {
        let c = 3.0 * a;
        let bb = 3.0 * (b - a) - c;
        let aa = 1.0 - c - bb;
        (aa, bb)
    }

    fn sample_x(&self, t: f32) -> f32 {
        let (a, b) = Self::coefficients(self.x1, self.x2);
        ((a * t + b) * t + 3.0 * self.x1) * t
    }

    fn sample_y(&self, t: f32) -> f32 {
        let (a, b) = Self::coefficients(self.y1, self.y2);
        ((a * t + b) * t + 3.0 * self.y1) * t
    }

    fn sample_x_derivative(&self, t: f32) -> f32 {
        let (a, b) = Self::coefficients(self.x1, self.x2);
        (3.0 * a * t + 2.0 * b) * t + 3.0 * self.x1
    }

    /// Curve parameter `t` for a given progress `x` (both 0..1).
    fn solve_t_for_x(&self, x: f32) -> f32 {
        // Newton–Raphson.
        let mut t = x;
        for _ in 0..8 {
            let err = self.sample_x(t) - x;
            if err.abs() < 1e-6 {
                return t;
            }
            let d = self.sample_x_derivative(t);
            if d.abs() < 1e-6 {
                break;
            }
            t -= err / d;
        }
        // Bisection fallback (x(t) is monotonic for valid CSS beziers).
        let (mut lo, mut hi) = (0.0_f32, 1.0_f32);
        for _ in 0..32 {
            let mid = (lo + hi) / 2.0;
            if self.sample_x(mid) < x {
                lo = mid
            } else {
                hi = mid
            }
        }
        (lo + hi) / 2.0
    }

    /// Eased output for input progress `x ∈ [0,1]` (clamped).
    pub fn eval(&self, x: f32) -> f32 {
        if x <= 0.0 {
            return 0.0;
        }
        if x >= 1.0 {
            return 1.0;
        }
        // f32 rounding can push sample_y a hair past 1.0; gpui's animation
        // element asserts `delta ∈ [0,1]` and aborts, so clamp the output hard.
        self.sample_y(self.solve_t_for_x(x)).clamp(0.0, 1.0)
    }

    /// This curve as a gpui easing closure.
    pub fn easing(self) -> impl Fn(f32) -> f32 + 'static {
        move |x| self.eval(x)
    }
}

/// Signature entrance curve — CSS `cubic-bezier(0.16, 1, 0.3, 1)`.
pub const EASE_OUT_EXPO: CubicBezier = CubicBezier::new(0.16, 1.0, 0.3, 1.0);
/// Sidebar resort glide — CSS `cubic-bezier(0.22, 1, 0.36, 1)`.
pub const EASE_RESORT: CubicBezier = CubicBezier::new(0.22, 1.0, 0.36, 1.0);
/// CSS `ease-out` — width/height transitions.
pub const EASE_OUT: CubicBezier = CubicBezier::new(0.0, 0.0, 0.58, 1.0);
/// CSS `ease` — quick fades, menu/dialog pops.
pub const EASE: CubicBezier = CubicBezier::new(0.25, 0.1, 0.25, 1.0);
/// CSS `ease-in-out` — scroll glides.
pub const EASE_IN_OUT: CubicBezier = CubicBezier::new(0.42, 0.0, 0.58, 1.0);
/// Tailwind's default transition curve — CSS `cubic-bezier(0.4, 0, 0.2, 1)`.
/// The temporal blend every interactive hover wash rides.
pub const EASE_TAILWIND: CubicBezier = CubicBezier::new(0.4, 0.0, 0.2, 1.0);

// ---------------------------------------------------------------------------
// Motion specs (the catalog)
// ---------------------------------------------------------------------------

/// One catalog entry: duration + optional delay + curve. The delay is folded
/// into the gpui animation timeline (gpui `Animation` has no native delay): the
/// animation runs for `delay + duration` and [`MotionSpec::progress`] holds 0
/// until the delay has elapsed.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct MotionSpec {
    pub duration_ms: u64,
    pub delay_ms: u64,
    pub curve: CubicBezier,
}

impl MotionSpec {
    pub const fn new(duration_ms: u64, curve: CubicBezier) -> Self {
        Self {
            duration_ms,
            delay_ms: 0,
            curve,
        }
    }

    pub const fn with_delay(mut self, delay_ms: u64) -> Self {
        self.delay_ms = delay_ms;
        self
    }

    /// Wall-clock span of the whole timeline (delay + duration).
    pub fn total(&self) -> Duration {
        Duration::from_millis(self.delay_ms + self.duration_ms)
    }

    /// Eased progress (0..1) for a raw timeline delta (0..1 across [`Self::total`]).
    pub fn progress(&self, raw_delta: f32) -> f32 {
        let total = (self.delay_ms + self.duration_ms) as f32;
        if total <= 0.0 || self.duration_ms == 0 {
            return 1.0;
        }
        let t =
            (raw_delta.clamp(0.0, 1.0) * total - self.delay_ms as f32) / self.duration_ms as f32;
        self.curve.eval(t.clamp(0.0, 1.0))
    }

    /// A oneshot gpui [`Animation`] for this spec (delay folded in).
    /// Wall-clock span honors [`speed_scale`] (measurement knob).
    pub fn animation(&self) -> Animation {
        let spec = *self;
        Animation::new(spec.total().mul_f32(speed_scale())).with_easing(move |d| spec.progress(d))
    }
}

/// Entrances: 0.5s expo-out fade + 4px rise.
pub const FADE_IN: MotionSpec = MotionSpec::new(500, EASE_OUT_EXPO);
/// Quick fade: 0.15s.
pub const FADE_QUICK: MotionSpec = MotionSpec::new(150, EASE);
/// Overlay/panel entrance: 0.2s expo-out.
pub const OVERLAY_FADE: MotionSpec = MotionSpec::new(200, EASE_OUT_EXPO);
/// Popover-in: 0.14s, fade + translateY −2.
pub const MENU_IN: MotionSpec = MotionSpec::new(140, EASE);
/// Popover-out: 0.1s — quicker than the entrance (exits should get out of the
/// way; the Radix convention of a shorter close than open).
pub const MENU_OUT: MotionSpec = MotionSpec::new(100, EASE);
/// Dialog-in: 0.18s, fade + 2px rise.
pub const DIALOG_IN: MotionSpec = MotionSpec::new(180, EASE);
/// Sidebar / pane width+height transitions: 200ms ease-out.
pub const RESIZE: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// Segmented-control thumb travel: 200ms ease-out, the same movement class a
/// region uses when it changes place.
pub const SEGMENT_SLIDE: MotionSpec = MotionSpec::new(200, EASE_OUT);
/// CSS `transition-colors` default: 150ms over [`EASE_TAILWIND`] — the temporal
/// blend every interactive hover wash rides.
pub const HOVER_FADE: MotionSpec = MotionSpec::new(150, EASE_TAILWIND);

// ---------------------------------------------------------------------------
// Element helpers (entrances)
// ---------------------------------------------------------------------------

/// Standard entrance: opacity 0→1 + translateY 4→0 over [`FADE_IN`].
pub fn fade_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(4.0 * (1.0 - t)))
    })
}

/// Quick opacity-only fade over [`FADE_QUICK`].
pub fn fade_quick<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, FADE_QUICK.animation(), |el, t| el.opacity(t))
}

/// Overlay entrance: opacity 0→1 + translateY 6→0 over [`OVERLAY_FADE`].
///
/// Unlike the in-flow entrances above, this must not force `relative` onto the
/// element: callers pass `absolute inset_0` overlays, and overriding their
/// position would drop the overlay back into normal flow, stacking it below
/// the shell instead of covering it. `top` still animates the rise because
/// absolute elements honor the top inset directly.
pub fn overlay_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, OVERLAY_FADE.animation(), |el, t| {
        el.opacity(t).top(px(6.0 * (1.0 - t)))
    })
}

/// Popover entrance: fade + translateY −2→0 over [`MENU_IN`].
pub fn menu_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_IN.animation(), |el, t| {
        el.relative()
            .opacity(0.3 + 0.7 * t)
            .top(px(-2.0 * (1.0 - t)))
    })
}

/// Dialog entrance over [`DIALOG_IN`] (fade + 2px rise).
pub fn dialog_in<E>(id: impl Into<ElementId>, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, DIALOG_IN.animation(), |el, t| {
        el.relative().opacity(t).top(px(2.0 * (1.0 - t)))
    })
}

/// Eased exit progress (0..=1) for a popover whose close began at `since`,
/// computed from the wall clock at render time.
///
/// The exit runs off an [`Instant`] rather than a `with_animation` clock: a
/// closing panel is re-rendered by its owner on every frame, and an
/// element-id-keyed animation would replay from 0 if the panel remounted
/// mid-exit. Deriving the progress from the timestamp is monotonic by
/// construction.
pub fn exit_progress(since: Instant) -> f32 {
    let total = MENU_OUT.total().mul_f32(speed_scale()).as_secs_f32();
    let raw = if total <= 0.0 {
        1.0
    } else {
        (since.elapsed().as_secs_f32() / total).clamp(0.0, 1.0)
    };
    MENU_OUT.progress(raw)
}

/// Popover exit: the reverse of [`menu_in`] — fade to 0 + translateY 0→−2 over
/// [`MENU_OUT`]. `t` is the eased progress from [`exit_progress`]; the element
/// must stay mounted for the whole timeline, which is the owner's job (the
/// state is held alive until [`MENU_OUT::total`](MotionSpec::total) elapses).
pub fn menu_out<E>(id: impl Into<ElementId>, t: f32, element: E) -> AnimationElement<E>
where
    E: Styled + IntoElement + 'static,
{
    element.with_animation(id, MENU_OUT.animation(), move |el, _| {
        el.relative().opacity(1.0 - t).top(px(-2.0 * t))
    })
}

// ---------------------------------------------------------------------------
// Hover color fades (CSS `transition-colors` parity)
// ---------------------------------------------------------------------------

/// How long a hover-fade entry stays on the clock after its last read. One
/// lease outlives a few missed frames; an unmounted element stops renewing and
/// the entry drops, letting a remount start clean.
const HOVER_LEASE: Duration = Duration::from_millis(400);

/// One element's hover fade: progress runs `origin → target` over
/// [`HOVER_FADE`], re-anchored at `origin` whenever the pointer flips
/// direction mid-flight so the blend is continuous.
#[derive(Debug, Clone, Copy)]
struct FadeEntry {
    origin: f32,
    target: f32,
    started: Instant,
    /// Wall clock at the last read (liveness stamp — see [`HoverFades`]).
    seen: Instant,
    /// The view whose render reads this fade. A flip notifies it, and it is the
    /// view that gets the follow-up frame while the fade is mid-flight.
    owner: EntityId,
}

impl FadeEntry {
    fn value(&self, now: Instant, duration: Duration) -> f32 {
        let elapsed = now.saturating_duration_since(self.started);
        if duration.is_zero() || elapsed >= duration {
            return self.target;
        }
        let raw = elapsed.as_secs_f32() / duration.as_secs_f32();
        lerp(self.origin, self.target, HOVER_FADE.curve.eval(raw))
    }

    fn settled(&self, now: Instant, duration: Duration) -> bool {
        self.origin == self.target || now.saturating_duration_since(self.started) >= duration
    }
}

/// Per-key hover progress store.
///
/// A main-thread `thread_local` rather than a gpui Global so the free-function
/// element builders can blend colors without threading `cx` through every
/// signature. All access happens on the UI thread (element builders, mouse
/// listeners, the render tail).
///
/// Staleness: an element that unmounts mid-hover never gets its leave event, so
/// entries are stamped with a wall-clock read time and pruned by
/// [`hover_fades_active`] when a full lease passes without a read — a reopened
/// list never inherits a dead entry's wash. Time-based (not frame-counted) so
/// multi-window roots cannot over-advance the counter.
#[derive(Default)]
pub struct HoverFades {
    entries: HashMap<String, FadeEntry>,
}

impl HoverFades {
    fn duration() -> Duration {
        HOVER_FADE.total().mul_f32(speed_scale())
    }

    /// Pointer entered (`hovered`) or left the element behind `key`. Reduced
    /// motion snaps straight to the endpoint.
    pub fn set_at(
        &mut self,
        key: &str,
        hovered: bool,
        reduced: bool,
        owner: EntityId,
        now: Instant,
    ) {
        let target = if hovered { 1.0 } else { 0.0 };
        let duration = Self::duration();
        let current = self
            .entries
            .get(key)
            .map(|e| e.value(now, duration))
            .unwrap_or(0.0);
        if target == 0.0 && !self.entries.contains_key(key) {
            return; // never-hovered element reporting a leave — nothing to do
        }
        let origin = if reduced { target } else { current };
        self.entries.insert(
            key.to_string(),
            FadeEntry {
                origin,
                target,
                started: now,
                seen: now,
                owner,
            },
        );
    }

    /// Hover progress (0..1) for `key` at `now`; stamps liveness.
    pub fn value_at(&mut self, key: &str, now: Instant) -> f32 {
        match self.entries.get_mut(key) {
            Some(entry) => {
                entry.seen = now;
                entry.value(now, Self::duration())
            }
            None => 0.0,
        }
    }

    /// Once-per-frame bookkeeping (call exactly once per frame from the window
    /// root): prune entries that settled back to rest or went a full lease
    /// unread (unmounted), and hand every owner whose fade is still mid-flight
    /// to `schedule` so it can request its next frame.
    pub fn tick_at(&mut self, now: Instant, mut schedule: impl FnMut(EntityId)) {
        let duration = Self::duration();
        let mut active: Vec<EntityId> = Vec::new();
        self.entries.retain(|_, entry| {
            if now.saturating_duration_since(entry.seen) > HOVER_LEASE {
                return false;
            }
            let settled = entry.settled(now, duration);
            if !settled && !active.contains(&entry.owner) {
                active.push(entry.owner);
            }
            // Settled at rest — steady state, indistinguishable from absent.
            !(settled && entry.target == 0.0)
        });
        for owner in active {
            schedule(owner);
        }
    }
}

thread_local! {
    static HOVER_FADES: RefCell<HoverFades> = RefCell::new(HoverFades::default());
}

/// Stable store key for an element's hover fade. Namespaced by caller so the
/// global store cannot collide across surfaces.
pub fn hover_key(namespace: &str, id: impl std::fmt::Display) -> String {
    format!("{namespace}:{id}")
}

/// Hover progress (0..1) for `key` this frame.
pub fn hover_t(key: &str) -> f32 {
    HOVER_FADES.with(|fades| fades.borrow_mut().value_at(key, Instant::now()))
}

/// Record a hover flip for `key` (reduced motion snaps). `owner` is the view
/// whose render reads the fade; it is the one notified when the blend must
/// advance.
pub fn set_hover(key: &str, hovered: bool, reduced: bool, owner: EntityId) {
    HOVER_FADES.with(|fades| {
        fades
            .borrow_mut()
            .set_at(key, hovered, reduced, owner, Instant::now())
    });
}

/// An `.on_hover` listener driving the fade for `key` — pair with
/// [`hover_t`]/[`hover_blend`] reads of the same key in the same element.
///
/// `owner` is the view being rendered when the listener is built (normally
/// `cx.entity_id()`): the flip notifies that view instead of calling
/// `Window::refresh`, so cached sibling subtrees survive a hover.
pub fn hover_listener(
    owner: EntityId,
    key: impl Into<SharedString>,
) -> impl Fn(&bool, &mut Window, &mut App) + 'static {
    let key = key.into();
    move |hovered, _window, cx| {
        set_hover(&key, *hovered, cx.reduce_motion(), owner);
        // Event-dispatch context: `request_animation_frame` is draw-phase-only,
        // and `Window::refresh` would defeat every `.cached()` subtree. Notify
        // the owning view; [`schedule_hover_frames`] keeps the fade advancing.
        cx.notify(owner);
    }
}

/// Frame-drive hook: call ONCE per frame (the window root render). Schedules a
/// follow-up frame for every view that still has a hover fade mid-flight, so
/// each blend advances inside its own (possibly cached) view instead of
/// re-rendering the whole window.
pub fn schedule_hover_frames(window: &mut Window) {
    HOVER_FADES.with(|fades| {
        fades.borrow_mut().tick_at(Instant::now(), |owner| {
            window.on_next_frame(move |_, cx| cx.notify(owner));
        });
    });
}

/// Linear interpolation (layout tweens, color channels).
pub fn lerp(from: f32, to: f32, t: f32) -> f32 {
    from + (to - from) * t
}

/// Blend two colors by `t` the way the browser transitions them: component
/// interpolation in sRGB with premultiplied alpha — a wash fading in from
/// transparent brightens without passing through grey.
pub fn mix(from: Hsla, to: Hsla, t: f32) -> Hsla {
    let t = t.clamp(0.0, 1.0);
    if t <= 0.0 {
        return from;
    }
    if t >= 1.0 {
        return to;
    }
    let (f, g) = (Rgba::from(from), Rgba::from(to));
    let a = lerp(f.a, g.a, t);
    if a <= f32::EPSILON {
        // Both endpoints (effectively) transparent — carry the target's hue.
        return Hsla::from(Rgba { a: 0.0, ..g });
    }
    Hsla::from(Rgba {
        r: lerp(f.r * f.a, g.r * g.a, t) / a,
        g: lerp(f.g * f.a, g.g * g.a, t) / a,
        b: lerp(f.b * f.a, g.b * g.a, t) / a,
        a,
    })
}

/// The standard hover blend: rest → hover color at `key`'s current progress.
pub fn hover_blend(key: &str, rest: Hsla, hover: Hsla) -> Hsla {
    mix(rest, hover, hover_t(key))
}

// ---------------------------------------------------------------------------
// Reduced motion + measurement knob
// ---------------------------------------------------------------------------

/// Dev/measurement knob (`VIBEX_MOTION_SCALE`, default 1): stretches every
/// catalog timeline by this factor — e.g. `VIBEX_MOTION_SCALE=10` slows the
/// 150ms hover fades to 1.5s so the blend can be inspected frame by frame.
/// Read once; never set in production.
pub fn speed_scale() -> f32 {
    static SCALE: std::sync::OnceLock<f32> = std::sync::OnceLock::new();
    *SCALE.get_or_init(|| {
        std::env::var("VIBEX_MOTION_SCALE")
            .ok()
            .and_then(|v| v.parse::<f32>().ok())
            .filter(|s| s.is_finite())
            .map(|s| s.clamp(0.01, 100.0))
            .unwrap_or(1.0)
    })
}

/// Global reduced-motion flag. gpui snaps every `with_animation` element when
/// set (end state for oneshots, rest state for loops) and schedules no frames.
pub fn reduced_motion(cx: &App) -> bool {
    cx.reduce_motion()
}

/// The user's reduced-motion preference, kept apart from the effective
/// `App::reduce_motion` flag so the workbench can pause animations for a
/// second reason — the window not being active — without forgetting the
/// setting.
static USER_REDUCED_MOTION: AtomicBool = AtomicBool::new(false);

/// Whether the workbench window is currently inactive (unfocused, minimized,
/// or hidden). A window nobody is looking at gains nothing from an animation
/// running, and a repeating one keeps the compositor busy for as long as it is
/// mounted.
static WINDOW_INACTIVE: AtomicBool = AtomicBool::new(false);

/// Whether an inactive window should also stop animating.
///
/// Off by default: the pause saves power, but it freezes a backgrounded
/// workbench mid-animation, so it is opt-in through the appearance settings.
static PAUSE_INACTIVE_ANIMATION: AtomicBool = AtomicBool::new(false);

/// Record the user's reduced-motion preference and re-derive the effective
/// flag. Called whenever the appearance settings are applied.
pub fn set_user_reduced_motion(reduced: bool, cx: &mut App) {
    USER_REDUCED_MOTION.store(reduced, Ordering::Relaxed);
    sync_reduce_motion(cx);
}

/// Record whether an inactive window should pause its animations, and
/// re-derive the effective flag. Called whenever the appearance settings are
/// applied.
pub fn set_pause_inactive_animation(enabled: bool, cx: &mut App) {
    PAUSE_INACTIVE_ANIMATION.store(enabled, Ordering::Relaxed);
    sync_reduce_motion(cx);
}

/// Record whether the workbench window is active. While it is not — and the
/// setting asks for it — every `with_animation` element snaps to its rest state
/// and schedules no frames; gpui already implements that behind
/// `App::reduce_motion`, so the gate reuses it rather than tracking each
/// animation individually.
pub fn set_window_active(active: bool, cx: &mut App) {
    WINDOW_INACTIVE.store(!active, Ordering::Relaxed);
    sync_reduce_motion(cx);
}

fn sync_reduce_motion(cx: &mut App) {
    let reduced = effective_reduced_motion(
        USER_REDUCED_MOTION.load(Ordering::Relaxed),
        PAUSE_INACTIVE_ANIMATION.load(Ordering::Relaxed),
        WINDOW_INACTIVE.load(Ordering::Relaxed),
    );
    cx.set_reduce_motion(reduced);
}

/// Whether the workbench window is currently inactive (unfocused, minimized,
/// or hidden). Background timers use this to skip repaints nobody can see.
pub fn window_is_inactive() -> bool {
    WINDOW_INACTIVE.load(Ordering::Relaxed)
}

/// The three inputs to [`sync_reduce_motion`], as a pure function so the truth
/// table — in particular that an inactive window only pauses when the setting
/// asks for it — is testable without an `App`.
fn effective_reduced_motion(
    user_reduced_motion: bool,
    pause_inactive_animation: bool,
    window_inactive: bool,
) -> bool {
    user_reduced_motion || (pause_inactive_animation && window_inactive)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stand-in owner for the fade store; the tests only care that a fade
    /// remembers which view to drive.
    /// The stand-in owner is built at runtime: `EntityId` has no const
    /// constructor in this gpui version, and the tests only compare ids.
    fn owner() -> EntityId {
        EntityId::from(7)
    }

    /// Collect the owners a tick wants to keep drawing.
    fn tick_owners(fades: &mut HoverFades, now: Instant) -> Vec<EntityId> {
        let mut owners = Vec::new();
        fades.tick_at(now, |owner| owners.push(owner));
        owners
    }

    fn assert_close(actual: f32, expected: f32, tol: f32, ctx: &str) {
        assert!(
            (actual - expected).abs() <= tol,
            "{ctx}: got {actual}, expected {expected} ±{tol}"
        );
    }

    #[test]
    fn eval_never_escapes_unit_interval_dense_sweep() {
        // f32 rounding produced 1.000000119 near the tail of EASE_OUT_EXPO,
        // tripping gpui's `delta ∈ [0,1]` assert. Sweep densely,
        // including the values right below 1.0 where Newton lands closest to
        // the endpoint.
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_IN_OUT, EASE_TAILWIND] {
            for i in 0..=100_000u32 {
                let x = i as f32 / 100_000.0;
                let y = curve.eval(x);
                assert!((0.0..=1.0).contains(&y), "eval({x}) = {y} escaped [0,1]");
            }
            for x in [0.999_999f32, 0.999_999_9, 1.0 - f32::EPSILON] {
                let y = curve.eval(x);
                assert!((0.0..=1.0).contains(&y), "eval({x}) = {y} escaped [0,1]");
            }
        }
    }

    #[test]
    fn exit_is_quicker_than_entry() {
        // Exits get out of the way: a close must never outlast its open.
        let pair = [(MENU_IN.duration_ms, MENU_OUT.duration_ms)];
        for (open, close) in pair {
            assert!(close < open, "close {close}ms must beat open {open}ms");
        }
        assert_eq!(MENU_OUT.delay_ms, MENU_IN.delay_ms);
    }

    #[test]
    fn exit_progress_is_monotonic_and_bounded() {
        let since = Instant::now();
        let first = exit_progress(since);
        assert!((0.0..=1.0).contains(&first), "progress escaped [0,1]");
        // Wall-clock derived, so a fresh instant reads near the start and an
        // elapsed timeline reads exactly 1 — never a replay from 0.
        assert!(first < 0.5);
        assert_eq!(exit_progress(since - MENU_OUT.total() * 2), 1.0);
    }

    #[test]
    fn bezier_linear_is_identity() {
        let linear = CubicBezier::new(0.0, 0.0, 1.0, 1.0);
        for x in [0.0, 0.1, 0.25, 0.5, 0.75, 0.9, 1.0] {
            assert_close(linear.eval(x), x, 1e-4, "linear");
        }
    }

    #[test]
    fn bezier_known_values() {
        // References computed independently with 80-step bisection.
        let cases: [(&str, CubicBezier, [f32; 5]); 3] = [
            (
                "expo",
                EASE_OUT_EXPO,
                [0.494391, 0.825622, 0.971779, 0.997677, 0.999878],
            ),
            (
                "ease-out",
                EASE_OUT,
                [0.160572, 0.378138, 0.684643, 0.906535, 0.982973],
            ),
            (
                "ease",
                EASE,
                [0.094796, 0.408511, 0.802403, 0.960459, 0.994316],
            ),
        ];
        for (name, curve, expected) in cases {
            for (x, want) in [0.1, 0.25, 0.5, 0.75, 0.9].into_iter().zip(expected) {
                assert_close(curve.eval(x), want, 1e-3, name);
            }
        }
    }

    #[test]
    fn bezier_endpoints_and_clamping() {
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_IN_OUT, EASE_TAILWIND] {
            assert_eq!(curve.eval(0.0), 0.0);
            assert_eq!(curve.eval(1.0), 1.0);
            assert_eq!(curve.eval(-0.5), 0.0);
            assert_eq!(curve.eval(1.5), 1.0);
        }
    }

    #[test]
    fn bezier_is_monotonic_for_catalog_curves() {
        for curve in [EASE_OUT_EXPO, EASE_OUT, EASE, EASE_IN_OUT, EASE_TAILWIND] {
            let mut last = 0.0;
            for i in 0..=100 {
                let y = curve.eval(i as f32 / 100.0);
                assert!(y >= last - 1e-4, "monotonicity violated at {i}");
                last = y;
            }
        }
    }

    #[test]
    fn catalog_timings() {
        assert_eq!(FADE_IN.duration_ms, 500);
        assert_eq!(FADE_QUICK.duration_ms, 150);
        assert_eq!(OVERLAY_FADE.duration_ms, 200);
        assert_eq!(MENU_IN.duration_ms, 140);
        assert_eq!(DIALOG_IN.duration_ms, 180);
        assert_eq!(RESIZE.duration_ms, 200);
        assert_eq!(HOVER_FADE.duration_ms, 150);
        assert_eq!(EASE_OUT_EXPO, CubicBezier::new(0.16, 1.0, 0.3, 1.0));
        assert_eq!(EASE_TAILWIND, CubicBezier::new(0.4, 0.0, 0.2, 1.0));
    }

    #[test]
    fn spec_delay_holds_then_runs() {
        let delayed = MotionSpec::new(500, EASE).with_delay(150);
        assert_eq!(delayed.total(), Duration::from_millis(650));
        assert_eq!(delayed.progress(0.0), 0.0);
        // Still inside the delay window at raw 0.2 (130ms < 150ms).
        assert_eq!(delayed.progress(0.2), 0.0);
        // Fully done at the end; clamped beyond.
        assert_eq!(delayed.progress(1.0), 1.0);
        assert_eq!(delayed.progress(2.0), 1.0);
        // No-delay specs pass straight through the curve.
        assert_close(
            FADE_IN.progress(0.5),
            EASE_OUT_EXPO.eval(0.5),
            1e-6,
            "no-delay",
        );
    }

    #[test]
    fn hover_fade_ramps_and_reverses_continuously() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        // Enter: 0 at the flip, mid-flight strictly between, 1 at 150ms.
        fades.set_at("pill", true, false, owner(), t0);
        assert_eq!(fades.value_at("pill", t0), 0.0);
        let mid = fades.value_at("pill", ms(75));
        assert!(mid > 0.0 && mid < 1.0, "mid-flight enter: {mid}");
        assert_eq!(fades.value_at("pill", ms(150)), 1.0);
        assert_eq!(fades.value_at("pill", ms(400)), 1.0, "clamps past the end");

        // Leave mid-flight re-anchors at the current value — no jump.
        fades.set_at("pill", true, false, owner(), t0);
        let at_flip = fades.value_at("pill", ms(75));
        fades.set_at("pill", false, false, owner(), ms(75));
        let after_flip = fades.value_at("pill", ms(75));
        assert!(
            (after_flip - at_flip).abs() < 1e-4,
            "continuity: {at_flip} vs {after_flip}"
        );
        let falling = fades.value_at("pill", ms(140));
        assert!(falling < after_flip, "fades back down");
        assert_eq!(fades.value_at("pill", ms(225)), 0.0, "lands at rest");
    }

    #[test]
    fn hover_fade_reduced_motion_snaps() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("row", true, true, owner(), t0);
        assert_eq!(fades.value_at("row", t0), 1.0, "enter snaps to 1");
        fades.set_at("row", false, true, owner(), t0);
        assert_eq!(fades.value_at("row", t0), 0.0, "leave snaps to 0");
    }

    /// An inactive window only stops animating when the setting asks for it.
    ///
    /// The default must leave a backgrounded workbench animating: the pause is
    /// a power optimization, and it froze the window in a way the user could
    /// see, so it is now opt-in.
    #[test]
    fn inactive_window_pauses_only_when_the_setting_is_on() {
        assert!(
            !effective_reduced_motion(false, false, true),
            "an inactive window keeps animating by default"
        );
        assert!(!effective_reduced_motion(false, false, false));
        assert!(
            effective_reduced_motion(false, true, true),
            "the setting pauses an inactive window"
        );
        assert!(
            !effective_reduced_motion(false, true, false),
            "the setting says nothing about an active window"
        );
        assert!(
            effective_reduced_motion(true, false, false),
            "the user's own preference still snaps animations"
        );
        assert!(effective_reduced_motion(true, true, true));
    }

    #[test]
    fn hover_fade_leave_without_enter_is_inert() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        fades.set_at("ghost", false, false, owner(), t0);
        assert!(fades.entries.is_empty(), "no entry for a leave-only key");
        assert_eq!(fades.value_at("ghost", t0), 0.0);
    }

    #[test]
    fn hover_tick_reports_flight_and_prunes() {
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        fades.set_at("a", true, false, owner(), t0);
        // Mid-flight: the owner is asked for a frame (read each frame).
        assert_eq!(tick_owners(&mut fades, ms(50)), vec![owner()]);
        fades.value_at("a", ms(50));
        assert_eq!(tick_owners(&mut fades, ms(100)), vec![owner()]);
        fades.value_at("a", ms(100));
        // Settled hovered (still read): no more frames needed, entry kept.
        assert!(tick_owners(&mut fades, ms(200)).is_empty());
        fades.value_at("a", ms(200));
        assert_eq!(fades.value_at("a", ms(250)), 1.0);

        // Leave → fades → settles at rest → entry evicted.
        fades.set_at("a", false, false, owner(), ms(250));
        assert_eq!(tick_owners(&mut fades, ms(300)), vec![owner()]);
        fades.value_at("a", ms(300));
        assert!(
            tick_owners(&mut fades, ms(500)).is_empty(),
            "settled at rest"
        );
        assert!(fades.entries.is_empty(), "rest entries are pruned");
    }

    #[test]
    fn hover_tick_evicts_unread_entries() {
        // An element that unmounts mid-hover never sends its leave — a full
        // lease without a read drops the entry so a remount starts clean.
        let mut fades = HoverFades::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        fades.set_at("menu-row", true, false, owner(), t0);
        tick_owners(&mut fades, ms(16));
        fades.value_at("menu-row", ms(16)); // mounted, read
        // Still fresh and mid-flight without further reads: keep frames coming.
        assert_eq!(tick_owners(&mut fades, ms(100)), vec![owner()]);
        // A full lease without any read evicts the entry.
        tick_owners(&mut fades, ms(600));
        assert!(fades.entries.is_empty(), "unread entry evicted");
        assert_eq!(fades.value_at("menu-row", ms(600)), 0.0);
    }

    #[test]
    fn mix_endpoints_and_transparent_blend() {
        let rest = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.235,
            a: 1.0,
        };
        let hover = Hsla {
            h: 0.0,
            s: 0.0,
            l: 0.29,
            a: 1.0,
        };
        assert_eq!(mix(rest, hover, 0.0), rest);
        assert_eq!(mix(rest, hover, 1.0), hover);
        assert_eq!(mix(rest, hover, -1.0), rest, "t clamps low");
        assert_eq!(mix(rest, hover, 2.0), hover, "t clamps high");

        // Opaque blend: lightness moves monotonically between the endpoints.
        let mid = mix(rest, hover, 0.5);
        assert!(mid.l > rest.l && mid.l < hover.l, "mid lightness {}", mid.l);

        // Transparent → wash: alpha ramps, hue stays the wash's (premultiplied
        // — never a darkened grey mid-fade).
        let wash = Hsla {
            h: 0.0,
            s: 0.0,
            l: 1.0,
            a: 0.06,
        };
        let half = mix(
            Hsla {
                h: 0.0,
                s: 0.0,
                l: 0.0,
                a: 0.0,
            },
            wash,
            0.5,
        );
        assert!((half.a - 0.03).abs() < 1e-4, "alpha midpoint {}", half.a);
        let half_rgba = Rgba::from(half);
        assert!(
            half_rgba.r > 0.99 && half_rgba.g > 0.99 && half_rgba.b > 0.99,
            "white wash keeps its hue: {half_rgba:?}"
        );
    }
}

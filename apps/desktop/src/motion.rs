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
//! That flag only reaches elements built on `with_animation`. Animation-driven
//! frames the app schedules itself (status spinners, the terminal cursor blink,
//! the turn timer) are not covered by it, so each one asks
//! [`pauses_while_inactive`] before standing still — they save power in a
//! backgrounded window only when the user asked for that trade.
//!
//! translateY is implemented as a `top` inset: taffy applies relative insets
//! after layout, so — like a CSS transform — siblings never move. Entrances
//! never set `position` themselves; an element's own positioning (relative is
//! the gpui default) must survive the animation untouched.

use std::cell::{Cell, RefCell};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationElement, AnyElement, App, AppContext as _, Context, ElementId, Empty,
    Entity, EntityId, Hsla, InteractiveElement as _, IntoElement, ParentElement as _, Render, Rgba,
    SharedString, StyleRefinement, Styled, Window, div, px,
};
use gpui_component::StyledExt as _;

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
/// The composer's height morph between its one-line bar and its multi-row
/// box: 180ms ease-out. Short enough that typing never waits on it, long
/// enough that the timeline's bottom edge glides instead of jumping.
pub const COMPOSER_MORPH: MotionSpec = MotionSpec::new(180, EASE_OUT);
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

thread_local! {
    /// The view a render pass is building on behalf of.
    ///
    /// The workbench renders its sidebar and timeline through child host views
    /// (`SidebarView`, `TimelineView`) so those subtrees keep their own render
    /// boundary. A host pushes its entity id here while it renders the
    /// workbench's state, so every hover fade created inside that subtree is
    /// owned by the host — a sidebar hover notifies the sidebar alone instead of
    /// rebuilding the whole workbench. Nested scopes restore the previous owner.
    static HOVER_OWNER_SCOPE: Cell<Option<EntityId>> = const { Cell::new(None) };
}

/// Runs `build` with `owner` as the hover-fade owner for every
/// [`hover_listener`] built inside it. Used by the host views that render the
/// workbench's state on the workbench's behalf.
pub fn with_hover_owner<R>(owner: EntityId, build: impl FnOnce() -> R) -> R {
    let previous = HOVER_OWNER_SCOPE.with(|scope| scope.replace(Some(owner)));
    let result = build();
    HOVER_OWNER_SCOPE.with(|scope| scope.set(previous));
    result
}

/// The host view that currently owns the render pass, if any.
fn scoped_hover_owner() -> Option<EntityId> {
    HOVER_OWNER_SCOPE.with(Cell::get)
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
    // Resolve the owner now, not in the listener: a host view sets the scope
    // only for the duration of its render, and the flip arrives later during
    // event dispatch, when no scope is active.
    let owner = scoped_hover_owner().unwrap_or(owner);
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

// ---------------------------------------------------------------------------
// Height morphs (the composer's density change)
// ---------------------------------------------------------------------------

/// One in-flight height morph: an eased glide from the height the surface had
/// when its target changed to the live target.
///
/// The morph animates the surface's COMMITTED height: the density decision
/// commits its final layout immediately (the input entity never remounts —
/// the caret survives) while the surface's height glides toward it. The
/// surface sits at the BOTTOM of the session column, so its bottom edge is
/// stationary on screen and only the top edge sweeps; the control row rides
/// the stationary bottom while the text glides with the top.
///
/// Like the hover fades, this is driven manually from wall time, never
/// `with_animation`: the composer's tree is rebuilt on every timeline frame,
/// and an element-id-keyed clock would replay the morph from 0 on any remount
/// mid-flight. The target is re-read every frame, so a draft that keeps
/// growing mid-morph moves the endpoint instead of finishing on a stale
/// height, and a reversal re-anchors at the CURRENT animated height.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct HeightMorph {
    /// Rendered height when the target changed — the glide's start point.
    pub from: f32,
    pub started: Instant,
}

impl HeightMorph {
    fn duration() -> Duration {
        COMPOSER_MORPH.total().mul_f32(speed_scale())
    }

    /// Eased progress (0..1) at `now`.
    pub fn progress(&self, now: Instant) -> f32 {
        let duration = Self::duration();
        if duration.is_zero() {
            return 1.0;
        }
        let raw =
            now.saturating_duration_since(self.started).as_secs_f32() / duration.as_secs_f32();
        COMPOSER_MORPH.curve.eval(raw.clamp(0.0, 1.0))
    }

    pub fn settled(&self, now: Instant) -> bool {
        now.saturating_duration_since(self.started) >= Self::duration()
    }

    /// The height to paint this frame: eased lerp from the change-time height
    /// to the LIVE target.
    pub fn height(&self, target: f32, now: Instant) -> f32 {
        lerp(self.from, target, self.progress(now))
    }
}

/// One surface's height-morph state: the last target it committed, the last
/// height it painted, and the morph in flight (if any).
#[derive(Debug, Clone, Copy)]
struct HeightEntry {
    target: f32,
    last_height: f32,
    morph: Option<HeightMorph>,
    /// Wall clock at the last read (liveness stamp — see [`HeightMorphs`]).
    seen: Instant,
    /// The view whose render reads this morph; it gets the follow-up frames.
    owner: EntityId,
}

/// Per-key height-morph store, the layout twin of [`HoverFades`].
///
/// Same threading and staleness rules: a main-thread `thread_local`, entries
/// stamped on read, pruned by [`HeightMorphs::tick_at`] when a full lease
/// passes unread so a remounted surface starts clean instead of inheriting a
/// dead entry's height.
#[derive(Default)]
pub struct HeightMorphs {
    entries: HashMap<String, HeightEntry>,
}

impl HeightMorphs {
    /// Resolve the height one surface paints this frame.
    ///
    /// `target` is the height the committed layout asks for. A changed target
    /// arms one morph from the last painted height (mid-flight that is the
    /// CURRENT animated height, so a reversal hands off seamlessly); an
    /// unchanged target never restarts the animation. `snap` (reduced motion,
    /// a session switch, or a first paint with no measured height) commits the
    /// target immediately and kills anything in flight.
    pub fn resolve_at(
        &mut self,
        key: &str,
        target: f32,
        snap: bool,
        owner: EntityId,
        now: Instant,
    ) -> f32 {
        let previous = self.entries.get(key).copied();
        let mut entry = match previous {
            Some(entry) => entry,
            None => HeightEntry {
                target,
                last_height: target,
                morph: None,
                seen: now,
                owner,
            },
        };
        entry.seen = now;
        entry.owner = owner;
        let changed = (entry.target - target).abs() > 0.5;
        let in_flight = entry.morph.is_some_and(|morph| !morph.settled(now));
        if snap || previous.is_none() {
            entry.morph = None;
        } else if changed && !in_flight {
            // A settled surface arms a fresh glide from its resting height.
            // A target that moves MID-flight is tracked by the running morph
            // — `height` lerps toward the live target — never restarted:
            // re-anchoring per keystroke would stretch the glide forever,
            // and a reversal at the far end of an ease-out has almost no
            // distance left to re-anchor over anyway.
            entry.morph = Some(HeightMorph {
                from: entry.last_height,
                started: now,
            });
        }
        entry.target = target;
        let height = match entry.morph {
            Some(morph) if !morph.settled(now) => morph.height(target, now),
            _ => {
                entry.morph = None;
                target
            }
        };
        entry.last_height = height;
        self.entries.insert(key.to_string(), entry);
        height
    }

    /// Once-per-frame bookkeeping (call exactly once per frame from the window
    /// root, beside [`HoverFades::tick_at`]): prune entries a full lease went
    /// unread, and hand every owner whose morph is still mid-flight to
    /// `schedule` so it can request its next frame.
    pub fn tick_at(&mut self, now: Instant, mut schedule: impl FnMut(EntityId)) {
        let mut active: Vec<EntityId> = Vec::new();
        self.entries.retain(|_, entry| {
            if now.saturating_duration_since(entry.seen) > HOVER_LEASE {
                return false;
            }
            let in_flight = entry.morph.is_some_and(|morph| !morph.settled(now));
            if in_flight && !active.contains(&entry.owner) {
                active.push(entry.owner);
            }
            true
        });
        for owner in active {
            schedule(owner);
        }
    }
}

thread_local! {
    static HEIGHT_MORPHS: RefCell<HeightMorphs> = RefCell::new(HeightMorphs::default());
}

thread_local! {
    /// The view a height morph should ask for its next frame while one is being
    /// built inside it.
    ///
    /// Same idea as the hover fades' owner scope, and it exists for the same
    /// reason times a hundred: the morph runs for a dozen frames, and the
    /// composer is hosted by a view of its own precisely so that a frame of its
    /// own does not rebuild the sidebar, every pane and the transcript beside
    /// it. Handing the morph the workbench's id made every one of those frames a
    /// whole-window rebuild, which is what a coarse, stepping glide looks like.
    static HEIGHT_OWNER_SCOPE: Cell<Option<EntityId>> = const { Cell::new(None) };
}

/// Runs `build` with `owner` as the height-morph owner for every
/// [`resolve_height`] built inside it.
///
/// The host view that embeds a morphing surface pushes its own id here, so the
/// frames the morph asks for repaint that view instead of whatever view happens
/// to be rendering the surface.
pub fn with_height_owner<R>(owner: EntityId, build: impl FnOnce() -> R) -> R {
    let previous = HEIGHT_OWNER_SCOPE.with(|scope| scope.replace(Some(owner)));
    let result = build();
    HEIGHT_OWNER_SCOPE.with(|scope| scope.set(previous));
    result
}

/// The host view that currently owns the height morph, if any.
fn scoped_height_owner() -> Option<EntityId> {
    HEIGHT_OWNER_SCOPE.with(Cell::get)
}

/// Resolve the height for `key` this frame (see [`HeightMorphs::resolve_at`]).
pub fn resolve_height(key: &str, target: f32, snap: bool, owner: EntityId) -> f32 {
    let owner = scoped_height_owner().unwrap_or(owner);
    HEIGHT_MORPHS.with(|morphs| {
        morphs
            .borrow_mut()
            .resolve_at(key, target, snap, owner, Instant::now())
    })
}

/// Whether `key`'s surface is mid-glide this frame.
///
/// A surface that is animating its own height is, by definition, about to be
/// repainted by the morph's own frame drive. A caller that measures it — the
/// composer parks its bounds for the menus it anchors — can ask here before
/// asking for a repaint of its own, so one glide does not wake every view that
/// watches the measurement.
pub fn height_morph_in_flight(key: &str) -> bool {
    HEIGHT_MORPHS.with(|morphs| {
        let morphs = morphs.borrow();
        let now = Instant::now();
        morphs
            .entries
            .get(key)
            .is_some_and(|entry| entry.morph.is_some_and(|morph| !morph.settled(now)))
    })
}

/// Frame-drive hook for height morphs: call ONCE per frame beside
/// [`schedule_hover_frames`].
pub fn schedule_height_morph_frames(window: &mut Window) {
    HEIGHT_MORPHS.with(|morphs| {
        morphs.borrow_mut().tick_at(Instant::now(), |owner| {
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

/// Whether a window that is not active should stand rendering still right now.
///
/// [`sync_reduce_motion`] already folds the preference into gpui's global
/// `App::reduce_motion` flag, which covers every `with_animation` element.
/// Callers that drive their own frames — the status spinners, the terminal
/// cursor blink, the turn timer — are not covered by it and ask through here
/// instead, with their own reading of the window state. Skipping that work
/// whenever the window is inactive freezes a backgrounded window the user
/// asked to keep animating, which is why the preference has to be part of the
/// decision.
pub fn pauses_while_inactive(window_inactive: bool) -> bool {
    inactive_window_pause(
        PAUSE_INACTIVE_ANIMATION.load(Ordering::Relaxed),
        window_inactive,
    )
}

/// The decision [`pauses_while_inactive`] makes, as a pure function so the
/// truth table is testable without an `App`.
fn inactive_window_pause(pause_inactive_animation: bool, window_inactive: bool) -> bool {
    pause_inactive_animation && window_inactive
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
    user_reduced_motion || inactive_window_pause(pause_inactive_animation, window_inactive)
}

// ---------------------------------------------------------------------------
// Animated hosts
// ---------------------------------------------------------------------------

/// The element an animation wraps, rebuilt with the phase it is drawn at.
type AnimatedBuild = Rc<dyn Fn(f32) -> AnyElement>;

/// One decorative animation, mounted in a view of its own.
///
/// `with_animation` asks the view it was built in for its next frame — its
/// scheduled re-render notifies [`Window::current_view`](gpui::Window) — and a
/// view that was notified rebuilds itself, however deep inside it the animation
/// sat. Built inline in the workbench's render, every animating mark (a live
/// label's sweep, a running row's activity dot, the composer companion's lane)
/// therefore asked for a workbench frame thirty times a second, and each of
/// those frames rebuilt the whole page: `prepaint` alone measured 8ms on a quiet
/// session and 50ms on a long one. A mark that costs its page that much does not
/// move at 30fps; it moves in whatever gaps the page could afford, which is
/// what "the animation stutters at high fps" looks like from the outside.
///
/// gpui draws a nested view again from last frame instead of rebuilding the view
/// around it: notifying a child splices rather than walks. So the mark is
/// mounted here, and the frames its animation asks for land on a subtree the
/// size of the mark.
///
/// The wrapper the caller drew — `style` and `debug_selector` — moves onto the
/// host's own root, so the swap moves nothing on screen.
pub(crate) struct AnimatedHost {
    style: StyleRefinement,
    debug_selector: Option<SharedString>,
    id: ElementId,
    animation: Animation,
    build: AnimatedBuild,
}

impl AnimatedHost {
    pub(crate) fn new(
        style: StyleRefinement,
        debug_selector: Option<SharedString>,
        id: ElementId,
        animation: Animation,
        build: impl Fn(f32) -> AnyElement + 'static,
    ) -> Self {
        Self {
            style,
            debug_selector,
            id,
            animation,
            build: Rc::new(build),
        }
    }
}

impl Default for AnimatedHost {
    fn default() -> Self {
        Self {
            style: StyleRefinement::default(),
            debug_selector: None,
            id: ElementId::Name("animated-host".into()),
            animation: Animation::new(Duration::from_millis(1))
                .repeat()
                .with_max_fps(1.0),
            build: Rc::new(|_| Empty.into_any_element()),
        }
    }
}

impl Render for AnimatedHost {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        let mut slot = div().refine_style(&self.style);
        if let Some(selector) = self.debug_selector.clone() {
            slot = slot.debug_selector(move || selector.to_string());
        }
        let build = self.build.clone();
        slot.with_animation(
            self.id.clone(),
            self.animation.clone(),
            move |this, phase| this.child(build(phase)),
        )
    }
}

/// Mounts `host` as the view that owns its animation, under `key`.
///
/// The key is what keeps one entity across the parent's rebuilds — the same
/// mark keeps the same host, and its phase with it. Along with the element, the
/// host goes away, so a row that scrolls out of view leaves nothing behind.
///
/// This deliberately does not go through `Window::use_keyed_state`: that
/// observes the state it hands out and notifies the view that created it
/// whenever it changes, so every frame the host drew would notify the page and
/// put the work this host exists to absorb straight back on it. Kept as element
/// state instead, the host has one listener — its own animation — and the page
/// hears nothing.
pub(crate) fn animated_host(
    window: &mut Window,
    cx: &mut App,
    key: impl Into<ElementId>,
    host: AnimatedHost,
) -> AnyElement {
    let pending = Rc::new(RefCell::new(Some(host)));
    let entity = window.with_global_id(key.into(), |global_id, window| {
        window.with_element_state::<Entity<AnimatedHost>, _>(global_id, |state, _| {
            let entity = match state {
                Some(entity) => entity,
                None => {
                    let host = pending.borrow_mut().take().unwrap_or_default();
                    cx.new(|_| host)
                }
            };
            (entity.clone(), entity)
        })
    });
    if let Some(host) = pending.borrow_mut().take() {
        entity.update(cx, |view, _| *view = host);
    }
    entity.into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::TestAppContext;

    /// A mark's frames repaint the mark, not the page that holds it.
    ///
    /// This is the whole point of [`AnimatedHost`]: `with_animation` asks the
    /// view it was built in for its next frame, so a mark built inline in the
    /// workbench's render asked the workbench for thirty frames a second and got
    /// a whole-page rebuild for each one — the cost that turns a steady swim
    /// into a stall and a jump. Counting renders is the assertion: the page
    /// keeps the one render it had, the host takes the mark's frame.
    #[gpui::test]
    fn an_animated_host_draws_its_frames_without_rebuilding_the_page(cx: &mut TestAppContext) {
        struct Page {
            renders: Rc<Cell<usize>>,
            host: Entity<AnimatedHost>,
        }
        impl Render for Page {
            fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
                self.renders.set(self.renders.get() + 1);
                div().size_full().child(self.host.clone())
            }
        }

        let page_renders = Rc::new(Cell::new(0));
        let mark_renders = Rc::new(Cell::new(0));
        let marked = mark_renders.clone();
        let host = cx.new(|_| {
            AnimatedHost::new(
                StyleRefinement::default().w(px(16.0)).h(px(16.0)),
                None,
                ElementId::Name("animated-mark".into()),
                Animation::new(Duration::from_secs(1))
                    .repeat()
                    .with_max_fps(30.0),
                move |_| {
                    marked.set(marked.get() + 1);
                    Empty.into_any_element()
                },
            )
        });
        let (_, cx) = cx.add_window_view({
            let page_renders = page_renders.clone();
            let host = host.clone();
            move |_, _| Page {
                renders: page_renders,
                host,
            }
        });
        cx.run_until_parked();
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });
        let (page_before, mark_before) = (page_renders.get(), mark_renders.get());
        assert!(mark_before > 0, "the mark draws at least once");

        // The frame a mark's animation asks for, thirty times a second.
        cx.update(|_, cx| host.update(cx, |_, cx| cx.notify()));
        cx.update(|window, cx| {
            let _ = window.draw(cx);
        });

        assert_eq!(
            page_renders.get(),
            page_before,
            "the page is drawn again from last frame, not rebuilt"
        );
        assert!(
            mark_renders.get() > mark_before,
            "the mark's own frame is drawn"
        );
    }

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

    /// Frame drivers the motion gate cannot reach — the status spinners, the
    /// terminal cursor blink, the turn timer — go through
    /// [`pauses_while_inactive`], and it has to answer the same way: an
    /// inactive window only holds those frames when the setting asks for it.
    ///
    /// The regression this pins is a driver that skipped its work because the
    /// window was inactive while the preference was off, which froze a
    /// backgrounded workbench the user had asked to keep animating.
    #[test]
    fn self_driven_frames_pause_only_when_the_setting_asks() {
        assert!(
            !inactive_window_pause(false, true),
            "the preference off never pauses, however inactive the window is"
        );
        assert!(!inactive_window_pause(false, false));
        assert!(
            inactive_window_pause(true, true),
            "the preference pauses an inactive window"
        );
        assert!(
            !inactive_window_pause(true, false),
            "the preference says nothing about an active window"
        );
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
    fn height_morph_glides_to_the_live_target_and_reverses_continuously() {
        let mut morphs = HeightMorphs::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);

        // First paint commits the target without a morph.
        assert_eq!(
            morphs.resolve_at("composer", 64.0, false, owner(), t0),
            64.0
        );
        // A changed target starts the glide at the last painted height.
        assert_eq!(
            morphs.resolve_at("composer", 136.0, false, owner(), t0),
            64.0
        );
        let mid = morphs.resolve_at("composer", 136.0, false, owner(), ms(90));
        assert!(mid > 64.0 && mid < 136.0, "mid-flight: {mid}");
        assert_eq!(
            morphs.resolve_at("composer", 136.0, false, owner(), ms(180)),
            136.0
        );

        // A reversal mid-flight re-anchors at the CURRENT animated height:
        // the frame after the flip continues from where the glide was —
        // never a pop back to an endpoint. Ease-out covers most of the
        // distance in the first frames, so the reads sit right at the start.
        morphs.resolve_at("composer", 64.0, false, owner(), ms(200));
        let growing = morphs.resolve_at("composer", 136.0, false, owner(), ms(201));
        assert!(growing > 64.0 && growing <= 136.0, "mid-flight: {growing}");
        let at_flip = morphs.resolve_at("composer", 64.0, false, owner(), ms(202));
        assert!(
            at_flip <= growing && growing - at_flip < 16.0,
            "continuity: {growing} vs {at_flip}"
        );
        let falling = morphs.resolve_at("composer", 64.0, false, owner(), ms(280));
        assert!(falling < at_flip, "glides back down");
        assert_eq!(
            morphs.resolve_at("composer", 64.0, false, owner(), ms(390)),
            64.0
        );
    }

    #[test]
    fn height_morph_tracks_a_target_that_moves_mid_flight() {
        let mut morphs = HeightMorphs::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        morphs.resolve_at("composer", 64.0, false, owner(), t0);
        morphs.resolve_at("composer", 136.0, false, owner(), t0);
        // The draft keeps growing: the morph aims at the new endpoint instead
        // of finishing on the stale one.
        let mid = morphs.resolve_at("composer", 180.0, false, owner(), ms(90));
        assert!(mid > 64.0 && mid < 180.0, "mid-flight: {mid}");
        assert_eq!(
            morphs.resolve_at("composer", 180.0, false, owner(), ms(180)),
            180.0
        );
    }

    #[test]
    fn height_morph_snap_kills_the_flight_and_never_arms_one() {
        let mut morphs = HeightMorphs::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        morphs.resolve_at("composer", 64.0, false, owner(), t0);
        morphs.resolve_at("composer", 136.0, false, owner(), t0);
        // A session switch (snap) commits immediately, mid-flight included.
        assert_eq!(
            morphs.resolve_at("composer", 200.0, true, owner(), ms(50)),
            200.0
        );
        assert_eq!(
            morphs.resolve_at("composer", 64.0, true, owner(), ms(60)),
            64.0
        );
    }

    /// The morph's frames go to the view that hosts the surface.
    ///
    /// A glide is a dozen frames, and the composer that arms one is a view of
    /// its own: notified as the workbench, each step rewrote the sidebar, every
    /// pane and the transcript as well, which is the cost that turns a short
    /// ease-out into a stepping glide.
    #[test]
    fn height_morph_frames_go_to_the_scoped_host_view() {
        let workbench = EntityId::from(3);
        let host = EntityId::from(4);
        let key = "morph_owner_scope";
        resolve_height(key, 64.0, false, workbench);
        resolve_height(key, 136.0, false, workbench);
        assert!(height_morph_in_flight(key));
        // A read inside the host's render hands the morph to the host.
        with_height_owner(host, || {
            let _ = resolve_height(key, 136.0, false, workbench);
        });
        let owner =
            HEIGHT_MORPHS.with(|morphs| morphs.borrow().entries.get(key).map(|entry| entry.owner));
        assert_eq!(owner, Some(host), "the host view drives the glide");
        // Outside the scope the workbench's own id is back in charge.
        let _ = resolve_height(key, 136.0, false, workbench);
        let owner =
            HEIGHT_MORPHS.with(|morphs| morphs.borrow().entries.get(key).map(|entry| entry.owner));
        assert_eq!(owner, Some(workbench), "the scope is restored on exit");
    }

    /// A settled glide is not in flight, so measurement wake-ups resume.
    #[test]
    fn height_morph_reports_flight_only_while_it_travels() {
        let key = "morph_flight_window";
        let owner = owner();
        // First paint commits without a glide.
        resolve_height(key, 64.0, false, owner);
        assert!(!height_morph_in_flight(key));
        resolve_height(key, 136.0, false, owner);
        assert!(height_morph_in_flight(key));
        // A snap (reduced motion, a session switch) lands at once.
        resolve_height(key, 200.0, true, owner);
        assert!(!height_morph_in_flight(key));
    }

    #[test]
    fn height_tick_reports_flight_and_prunes_unread_entries() {
        let mut morphs = HeightMorphs::default();
        let t0 = Instant::now();
        let ms = |m: u64| t0 + Duration::from_millis(m);
        let mut owners = Vec::new();
        let mut tick = |morphs: &mut HeightMorphs, now: Instant| {
            owners.clear();
            morphs.tick_at(now, |owner| owners.push(owner));
            owners.clone()
        };

        morphs.resolve_at("composer", 64.0, false, owner(), t0);
        morphs.resolve_at("composer", 136.0, false, owner(), t0);
        assert_eq!(tick(&mut morphs, ms(50)), vec![owner()]);
        morphs.resolve_at("composer", 136.0, false, owner(), ms(50));
        // Settled: no more frames needed, entry kept for the next change.
        assert!(tick(&mut morphs, ms(200)).is_empty());
        morphs.resolve_at("composer", 136.0, false, owner(), ms(200));
        assert!(morphs.entries.contains_key("composer"));
        // A full lease without a read evicts the entry.
        tick(&mut morphs, ms(700));
        assert!(morphs.entries.is_empty(), "unread entry evicted");
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

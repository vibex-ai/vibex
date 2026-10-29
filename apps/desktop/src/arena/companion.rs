//! The reward a won encounter unlocks: that Agent's guardian rides a lane the
//! session page floats over the bottom of its timeline while the turn runs. It
//! enters its own way when a turn starts and leaves its own way when the turn
//! ends (see `gait`). Floating it — instead of giving it a row of the composer
//! — keeps the timeline at its full height and lets the lane share the
//! timeline's bottom with the follow-the-agent button, painted over it rather
//! than beside it. Double-clicking it opens the same duel the home page offers,
//! over the session page instead of the new-session page.

use super::{
    ArenaView, Awakened, art,
    combat::{Boss, Guardian},
    copy,
    gait::{self, LANE_REACH, Shot},
    map::CENTER,
    motion, palette,
    raster::{PixelRect, ink},
    scene::{Geometry, PreviewSample, PreviewViewport},
};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, DismissEvent, Element,
    ElementId, Entity, EventEmitter, FocusHandle, GlobalElementId, Hsla, InspectorElementId,
    IntoElement, LayoutId, Pixels, Style, Styled as _, Subscription, Window, div, fill, point,
    prelude::*, px, size,
};
use gpui_component::{ElementExt as _, InteractiveElementExt as _, WindowExt as _, v_flex};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    time::{Duration, Instant},
};

/// The height a guardian's ordinary standing pose is drawn at, in screen
/// pixels. Every guardian reads at about this size, whatever its proportions.
///
/// This is a lane the session page floats over the timeline's bottom, not a
/// stage: the model is small enough to read as a passenger of the composer it
/// dives into rather than as the home page's guardian, yet large enough that
/// its pixels still resolve into a figure.
const MODEL_HEIGHT: f32 = 40.0;
/// The tallest lane a companion may reserve. A guardian whose loop reaches far
/// above its standing pose — the whale's breach — is drawn smaller instead.
const MAX_LANE: f32 = 72.0;
/// Idle poses are rebuilt at this rate. The pixel art does not read faster, and
/// every rebuild rasterizes the whole sculpture.
const FRAME_RATE: f32 = 12.0;
/// The clear margin kept at both ends of the travel.
const INSET: f32 = 12.0;

/// What the companion is doing, which is exactly what decides how it is drawn.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Stage {
    /// No companion is mounted: no reward, no running turn, or a duel is up.
    Idle,
    /// The session is running; the companion enters, then travels its lane.
    /// `clock` is the pose's own monotonic clock: the instant its itinerary
    /// starts, still ahead while the guardian makes its entrance.
    Roaming { clock: Instant },
    /// The turn ended; the companion leaves its own way from the pose it held
    /// `from` seconds into its lane, on the same pose clock, so the exit
    /// continues the pass instead of restarting it.
    Leaving { clock: Instant, from: f32 },
}
/// Signed seconds from `clock` to `now`: negative while `clock` is ahead.
fn seconds(now: Instant, clock: Instant) -> f32 {
    match now.checked_duration_since(clock) {
        Some(elapsed) => elapsed.as_secs_f32(),
        None => -(clock - now).as_secs_f32(),
    }
}

/// Which of the companion's motions a frame is sampled from.
#[derive(Clone, Copy, Debug, PartialEq)]
enum Motion {
    /// The entrance and then the lane itinerary.
    Lane,
    /// The exit, from the lane time the turn ended at.
    Leaving { from: f32 },
}
impl Motion {
    fn shot(self, guardian: Guardian, time: f32) -> Shot {
        match self {
            Self::Lane => gait::shot(guardian, time),
            Self::Leaving { from } => gait::leaving(guardian, from, time),
        }
    }
}

/// The unlocked guardian that rides above one Agent session's composer.
pub(crate) struct Companion {
    guardian: Option<Guardian>,
    stage: Stage,
    /// Bumped whenever a stage replaces the one a task was scheduled for, so a
    /// retire task cannot retire a companion that has since started over.
    generation: u64,
    frames: Rc<RefCell<FrameCache>>,
    /// The lane's width, measured by its last prepaint. The travel is resolved
    /// while the tree is laid out, before this frame's bounds exist.
    lane_width: Rc<Cell<Pixels>>,
    /// The pose the companion last painted, handed to the duel it opens as the
    /// entry camera, so the model grows out of the lane it came from.
    sample: Rc<Cell<Option<PreviewSample>>>,
    /// Where the keyboard returns when a duel ends.
    composer_focus: Option<FocusHandle>,
    /// The pace to beat for the guardian whose duel is about to open. The
    /// session page reads persisted state and hands it down with the lane.
    best_ms: Option<u64>,
    battle: Option<Entity<ArenaView>>,
    dismissal: Option<Subscription>,
}
impl EventEmitter<Awakened> for Companion {}
impl Companion {
    pub(crate) fn new() -> Self {
        Self {
            guardian: None,
            stage: Stage::Idle,
            generation: 0,
            frames: Rc::new(RefCell::new(FrameCache::default())),
            lane_width: Rc::new(Cell::new(px(0.0))),
            sample: Rc::new(Cell::new(None)),
            composer_focus: None,
            best_ms: None,
            battle: None,
            dismissal: None,
        }
    }
    /// Follows the composer being rendered: which Agent it belongs to, whether
    /// its turn is running, and where the keyboard sits.
    ///
    /// This is called from the render path, so it never notifies. Everything it
    /// changes is readable by the same frame that asked for it.
    fn track(
        &mut self,
        reward: Option<&str>,
        running: bool,
        focus: Option<FocusHandle>,
        cx: &mut Context<Self>,
    ) {
        self.composer_focus = focus;
        let guardian = reward.and_then(Guardian::from_agent);
        if guardian != self.guardian {
            // A different Agent owns the composer now. The previous companion
            // leaves with the surface it belonged to: an exit would be a lie
            // about a session that is still running, and an entrance a lie
            // about a turn that did not just start, so the lane hands over at
            // once. A duel it opened belongs to that session too, so it ends
            // here rather than following the reader to a session it was never
            // about.
            self.guardian = guardian;
            self.generation += 1;
            self.frames.borrow_mut().clear();
            self.sample.set(None);
            self.dismiss(cx);
            self.stage = match guardian {
                Some(_) if running => Stage::Roaming {
                    clock: cx.background_executor().now(),
                },
                _ => Stage::Idle,
            };
            return;
        }
        match (running, self.stage) {
            (true, Stage::Roaming { .. }) | (false, Stage::Idle) => {}
            (false, Stage::Leaving { .. }) => {}
            (true, _) => {
                // A turn started: the guardian makes its entrance first.
                let entrance = self.guardian.map_or(0.0, gait::entrance);
                self.generation += 1;
                self.stage = Stage::Roaming {
                    clock: cx.background_executor().now() + Duration::from_secs_f32(entrance),
                };
            }
            (false, Stage::Roaming { .. }) => self.leave(cx),
        }
    }
    /// Ends a duel without a result: the session it was opened over is gone.
    fn dismiss(&mut self, cx: &mut Context<Self>) {
        let Some(battle) = self.battle.take() else {
            return;
        };
        self.dismissal = None;
        battle.update(cx, |view, cx| view.close(cx));
        cx.notify();
    }
    /// The session stopped: the companion leaves its lane its own way and is
    /// gone by the time the exit ends.
    fn leave(&mut self, cx: &mut Context<Self>) {
        let (Stage::Roaming { clock }, Some(guardian)) = (self.stage, self.guardian) else {
            self.stage = Stage::Idle;
            return;
        };
        let executor = cx.background_executor().clone();
        self.stage = Stage::Leaving {
            clock,
            from: seconds(executor.now(), clock),
        };
        self.generation += 1;
        let generation = self.generation;
        cx.spawn(async move |this, cx| {
            executor
                .timer(Duration::from_secs_f32(gait::exit(guardian)) + Duration::from_millis(40))
                .await;
            this.update(cx, |companion, cx| {
                if companion.generation == generation {
                    companion.stage = Stage::Idle;
                    companion.sample.set(None);
                    cx.notify();
                }
            })
            .ok();
        })
        .detach();
    }
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.battle.is_some() || window.has_active_dialog(cx) {
            return;
        }
        let Some(sample) = self.sample.get() else {
            return;
        };
        let best_ms = self.best_ms;
        let battle = cx.new(|cx| ArenaView::new(sample, best_ms, window, cx));
        let focus = self.composer_focus.clone();
        self.dismissal = Some(cx.subscribe_in(
            &battle,
            window,
            move |this, battle, _: &DismissEvent, window, cx| {
                if let Some(clear_ms) = battle.read(cx).arena.clear_millis()
                    && let Some(guardian) = this.guardian
                {
                    // The duel was only reachable because this reward was
                    // already unlocked, so a replayed victory only ever
                    // re-reports the same unlock — but its clear time can still
                    // be a new record.
                    this.best_ms = Some(this.best_ms.map_or(clear_ms, |best| best.min(clear_ms)));
                    cx.emit(Awakened {
                        key: guardian.key(),
                        clear_ms,
                    });
                }
                this.battle = None;
                this.dismissal = None;
                this.sample.set(None);
                if let Some(focus) = focus.as_ref() {
                    focus.focus(window, cx);
                }
                cx.notify();
            },
        ));
        let initial_focus = battle.downgrade();
        self.battle = Some(battle);
        window.on_next_frame(move |window, cx| {
            let _ = initial_focus.update(cx, |view, cx| view.resume(window, cx));
        });
        cx.notify();
    }
    fn battle(&self) -> Option<Entity<ArenaView>> {
        self.battle.clone()
    }
}

/// Where a guardian's crop is drawn in the lane.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Fit {
    /// The crop resampled to whole screen pixels, one ink each.
    columns: usize,
    rows: usize,
    /// Screen pixels per raster pixel.
    pixel: f32,
}
impl Fit {
    fn width(self) -> f32 {
        self.columns as f32
    }
    /// The lane is exactly as tall as the crop: the crop already holds every
    /// pose of the loop, so nothing is sliced and nothing is wasted.
    fn lane(self) -> f32 {
        self.rows as f32
    }
}
fn fit(guardian: Guardian) -> Fit {
    let (left, top, right, bottom) = art::companion_box(guardian);
    let (box_width, box_height) = ((right - left) as f32, (bottom - top) as f32);
    let standing = art::companion_height(guardian) as f32;
    let scale = (MODEL_HEIGHT / standing).min(MAX_LANE / box_height);
    let rows = (box_height * scale).round().max(1.0);
    Fit {
        columns: (box_width * scale).round().max(1.0) as usize,
        rows: rows as usize,
        pixel: rows / box_height,
    }
}
/// How far across the lane a lane pose stands, from 0 to 1.
///
/// The pose comes from the guardian's own itinerary ([`Boss::lane`]): where it
/// stops, how long it rests, and how it hops, rolls, strides, swims, or darts
/// between, each on a lap of its own length.
fn across(boss: &Boss) -> f32 {
    ((boss.position.x - CENTER.x) / LANE_REACH * 0.5 + 0.5).clamp(0.0, 1.0)
}
/// The model's left edge for a place `across` the lane, in whole screen pixels
/// from the lane's left edge, so every cell lands on a pixel of its own.
fn lane_x(lane_width: f32, badge_width: f32, across: f32) -> f32 {
    let span = (lane_width - badge_width - INSET * 2.0).max(0.0);
    (INSET + across.clamp(0.0, 1.0) * span).round()
}
/// The model's top-left corner in the lane for a shot, in whole screen pixels:
/// its lane place, carried toward or past either end by the shot's slide, and
/// raised out of or sunk below the lane by its lift.
fn placement(shot: &Shot, lane_width: f32, badge_width: f32, lane_height: f32) -> (f32, f32) {
    let x = lane_x(lane_width, badge_width, across(&shot.boss));
    let x = if shot.slide >= 0.0 {
        x + (lane_width - x) * shot.slide
    } else {
        x + (x + badge_width) * shot.slide
    };
    (x.round(), (shot.lift * lane_height).round())
}

/// The lane the session page floats over the timeline's bottom, or nothing when
/// this session has no reward to show, when its turn is not running, or when
/// motion is reduced.
///
/// The lane is built here as the strip it always was; where it is mounted — in
/// flow, or absolutely above the composer's own edge — is the page's decision.
/// `lane`, with the persisted best clear time for the reward's guardian.
///
/// The session page owns UI state, so it passes the record down rather than
/// letting the arena read it: a duel opened over a session must show the same
/// pace to beat as one opened from the home page.
pub(crate) fn lane_with_best(
    state: &Entity<Companion>,
    reward: Option<&str>,
    best_ms: Option<u64>,
    running: bool,
    focus: Option<FocusHandle>,
    reduced: bool,
    cx: &mut App,
) -> Option<AnyElement> {
    // Reduced motion drops the effect entirely — the setting asks for a still
    // workbench, and a roaming model is exactly the motion it turns off. The
    // companion is still told the turn ended, so nothing is left half-gone.
    state.update(cx, |companion, cx| {
        companion.best_ms = best_ms;
        companion.track(reward, running && !reduced, focus, cx)
    });
    let (guardian, stage, frames, sample, lane_width) = {
        let companion = state.read(cx);
        if reduced || companion.battle().is_some() {
            return None;
        }
        (
            companion.guardian?,
            companion.stage,
            companion.frames.clone(),
            companion.sample.clone(),
            companion.lane_width.clone(),
        )
    };
    let (motion, clock, id) = match stage {
        Stage::Idle => return None,
        Stage::Roaming { clock } => (Motion::Lane, clock, "composer-companion-roam"),
        Stage::Leaving { clock, from } => {
            (Motion::Leaving { from }, clock, "composer-companion-leave")
        }
    };
    let executor = cx.background_executor().clone();
    let measured = lane_width.clone();
    let state = state.clone();
    let geometry = fit(guardian);
    let badge_width = geometry.width();
    let badge = move |time: f32, opacity: f32, x: f32, y: f32| {
        div()
            .id("composer-companion")
            .debug_selector(|| "composer-companion".into())
            .absolute()
            .left(px(x))
            .top(px(y))
            .cursor_pointer()
            .aria_label(copy::companion_label(guardian))
            .on_double_click({
                let state = state.clone();
                move |_, window, cx| {
                    state.update(cx, |companion, cx| companion.open(window, cx));
                }
            })
            .child(Badge {
                guardian,
                motion,
                time,
                opacity,
                fit: geometry,
                window_box: art::companion_box(guardian),
                frames: frames.clone(),
                sample: sample.clone(),
            })
    };
    let lane = div()
        .id("composer-companion-lane")
        .debug_selector(|| "composer-companion-lane".into())
        .relative()
        .w_full()
        .min_w_0()
        .h(px(geometry.lane()))
        .flex_none()
        .overflow_hidden()
        .on_prepaint(move |bounds, _, _| measured.set(bounds.size.width));
    Some(
        lane.with_animation(
            id,
            // The animation is only the frame ticker: the pose and the place
            // both come from the pose's own monotonic clock, so the authored
            // loop closes on itself and never cuts back to its first frame in
            // the middle of the lane.
            Animation::new(Duration::from_secs(60))
                .repeat()
                .with_max_fps(30.0),
            move |this, _| {
                let time = seconds(executor.now(), clock);
                let shot = motion.shot(guardian, time);
                let (x, y) = placement(
                    &shot,
                    f32::from(lane_width.get()),
                    badge_width,
                    geometry.lane(),
                );
                this.child(badge(time, shot.opacity, x, y))
            },
        )
        .into_any_element(),
    )
}

/// Wraps a session page so an open duel covers it, exactly as it does on the
/// home page: the page fades away on entry and returns when the duel ends.
/// Without a duel the page is handed straight back, so the ordinary session
/// surface keeps the layout it always had.
pub(crate) fn session_surface(
    state: &Entity<Companion>,
    content: impl IntoElement,
    cx: &mut App,
) -> AnyElement {
    let Some(battle) = state.read(cx).battle() else {
        return content.into_any_element();
    };
    let opacity = battle.read(cx).home_opacity.clone();
    let page = div().w_full().min_w_0().min_h_0().flex_1().child(content);
    let content = if motion::reduced_motion(cx) {
        page.invisible().into_any_element()
    } else {
        page.with_animation(
            "composer-companion-veil",
            Animation::new(Duration::from_secs(60))
                .repeat()
                .with_max_fps(30.0),
            move |this, _| {
                let opacity = opacity.get();
                this.opacity(opacity)
                    .when(opacity <= 0.0, |this| this.invisible())
            },
        )
        .into_any_element()
    };
    v_flex()
        .id("composer-companion-surface")
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .child(content)
        .child(
            div()
                .id("composer-companion-duel")
                .debug_selector(|| "composer-companion-duel".into())
                .absolute()
                .top_0()
                .left_0()
                .size_full()
                .child(battle),
        )
        .into_any_element()
}

/// Merged rectangles of the pose last drawn, so a repaint that lands on the
/// same pose costs nothing beyond the quads it paints.
#[derive(Default)]
struct FrameCache {
    key: Option<(Guardian, Motion, i64, usize, usize)>,
    rectangles: Vec<PixelRect>,
}
impl FrameCache {
    fn clear(&mut self) {
        self.key = None;
        self.rectangles.clear();
    }
    fn rectangles(
        &mut self,
        guardian: Guardian,
        motion: Motion,
        time: f32,
        fit: Fit,
    ) -> Vec<PixelRect> {
        let tick = (time * FRAME_RATE).floor() as i64;
        let key = (guardian, motion, tick, fit.columns, fit.rows);
        if self.key != Some(key) {
            self.key = Some(key);
            let time = tick as f32 / FRAME_RATE;
            let boss = motion.shot(guardian, time).boss;
            self.rectangles = art::companion(&boss, time, false)
                .resample(fit.columns, fit.rows)
                .rectangles();
        }
        self.rectangles.clone()
    }
}

/// One cropped idle frame, resampled to whole screen pixels and painted as
/// merged quads.
///
/// The element only carries the model's own rectangles; the box it was cropped
/// from is fixed for the whole loop, so the model never shifts inside it. The
/// model turns to face where its itinerary heads; it is never mirrored.
struct Badge {
    guardian: Guardian,
    motion: Motion,
    time: f32,
    opacity: f32,
    fit: Fit,
    /// The crop window in raster pixels: `(left, top, right, bottom)`.
    window_box: (i32, i32, i32, i32),
    frames: Rc<RefCell<FrameCache>>,
    sample: Rc<Cell<Option<PreviewSample>>>,
}
impl IntoElement for Badge {
    type Element = Self;
    fn into_element(self) -> Self {
        self
    }
}
impl Element for Badge {
    type RequestLayoutState = ();
    type PrepaintState = (Vec<PixelRect>, [Hsla; ink::COUNT]);
    fn id(&self) -> Option<ElementId> {
        Some("composer-companion-badge".into())
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
        style.size.width = px(self.fit.columns as f32).into();
        style.size.height = px(self.fit.rows as f32).into();
        (window.request_layout(style, [], cx), ())
    }
    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
        let rectangles =
            self.frames
                .borrow_mut()
                .rectangles(self.guardian, self.motion, self.time, self.fit);
        let (left, top, _, _) = self.window_box;
        let pixel = px(self.fit.pixel);
        let boss = self.motion.shot(self.guardian, self.time).boss;
        let (x, y) = art::pixel(boss.position);
        // The duel starts from this frame: the same model, in the same pose, at
        // the same spot, so it grows out of the lane it was standing in.
        self.sample.set(Some(PreviewSample {
            guardian: self.guardian,
            time: self.time,
            boss,
            viewport: Some(PreviewViewport {
                bounds,
                geometry: Geometry {
                    origin: bounds.origin
                        - point(pixel * (x + left) as f32, pixel * (y + top) as f32),
                    unit: pixel * art::SCALE,
                },
            }),
        }));
        (rectangles, palette::material_colors(self.guardian))
    }
    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        bounds: Bounds<Pixels>,
        _: &mut (),
        (rectangles, colors): &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        // One cell is one screen pixel; snapping keeps every cell on the device
        // grid, so the model stays crisp at fractional scale factors too.
        let scale = window.scale_factor();
        let snap = |value: Pixels| px((f32::from(value) * scale).round() / scale);
        let clip = window.content_mask().bounds;
        for rect in rectangles.iter() {
            let left = bounds.origin.x + px(f32::from(rect.x));
            let top = bounds.origin.y + px(f32::from(rect.y));
            let right = left + px(f32::from(rect.width));
            let bottom = top + px(f32::from(rect.height));
            let (left, top, right, bottom) = (snap(left), snap(top), snap(right), snap(bottom));
            if right > left
                && bottom > top
                && right >= clip.left()
                && left <= clip.right()
                && bottom >= clip.top()
                && top <= clip.bottom()
            {
                window.paint_quad(fill(
                    Bounds::new(point(left, top), size(right - left, bottom - top)),
                    colors[rect.ink as usize].opacity(self.opacity),
                ));
            }
        }
    }
}

#[cfg(test)]
#[path = "companion_tests.rs"]
mod tests;

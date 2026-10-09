//! A roaming guardian on the new-session home, becoming a local boss encounter
//! in that exact surface. Session drafts and Agent runtime state stay untouched.

mod art;
mod combat;
mod copy;
mod geometry;
mod guardian;
mod map;
mod palette;
mod raster;
mod scene;
mod scenery;
mod sculpture;

use crate::{locale::text, motion};
use combat::{Arena, Controls, Guardian, Phase, STEP, Vec2};
use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Global, KeyBinding, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Subscription, Task, Window,
    actions, div, prelude::*,
};
use gpui_component::{ActiveTheme as _, WindowExt as _, scroll::ScrollableElement as _, v_flex};
use std::{
    cell::Cell,
    collections::BTreeSet,
    rc::Rc,
    time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

pub(crate) fn setting_title() -> &'static str {
    text("Home mini-game", "首页小游戏", "首頁小遊戲")
}
pub(crate) fn setting_description() -> &'static str {
    text(
        "Let a pixel guardian roam the new-session home. Select its Agent and click the guardian to play.",
        "让像素守卫在新建会话首页游走，切换 Agent 后点击对应守卫即可游玩。",
        "讓像素守衛在新增工作階段首頁遊走，切換 Agent 後點擊對應守衛即可遊玩。",
    )
}
actions!(unbound, [Roll, TogglePause, Retry, Advance, Exit]);
struct Bindings;
impl Global for Bindings {}
fn init(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    cx.set_global(Bindings);
    cx.bind_keys([
        KeyBinding::new("space", Roll, Some("Unbound")),
        KeyBinding::new("p", TogglePause, Some("Unbound")),
        KeyBinding::new("r", Retry, Some("Unbound")),
        KeyBinding::new("enter", Advance, Some("Unbound")),
        KeyBinding::new("escape", Exit, Some("Unbound")),
    ]);
}

pub(crate) struct HomeArena {
    battle: Option<Entity<ArenaView>>,
    composer_focus: FocusHandle,
    dismissal: Option<Subscription>,
    preview: Rc<Cell<scene::PreviewSample>>,
    preview_seed: f32,
    selected: Option<Guardian>,
    agent_id: Option<String>,
    completed: bool,
}
impl HomeArena {
    pub(crate) fn new(composer_focus: FocusHandle) -> Self {
        art::prepare();
        Self {
            battle: None,
            composer_focus,
            dismissal: None,
            preview: Rc::new(Cell::new(scene::PreviewSample::default())),
            preview_seed: 0.0,
            selected: Some(Guardian::Claude),
            agent_id: None,
            completed: false,
        }
    }
    /// Called from draft selection events, never as a side effect of rendering.
    pub(crate) fn select_agent(&mut self, id: Option<&str>, cx: &mut Context<Self>) {
        if self.agent_id.as_deref() == id {
            return;
        }
        self.close(cx);
        self.agent_id = id.map(str::to_owned);
        self.selected = id.and_then(Guardian::from_agent);
        self.completed = false;
        self.preview_seed = 0.0;
        if let Some(guardian) = self.selected {
            self.preview.set(scene::PreviewSample::at(
                guardian,
                0.0,
                motion::reduced_motion(cx),
            ));
        }
        cx.notify();
    }
    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.battle.is_some()
            || self.completed
            || self.selected.is_none()
            || window.has_active_dialog(cx)
        {
            return;
        }
        init(cx);
        let sample = self.preview.get();
        let battle = cx.new(|cx| ArenaView::new(sample, window, cx));
        self.dismissal = Some(cx.subscribe_in(
            &battle,
            window,
            |this, battle, _: &DismissEvent, window, cx| {
                this.completed = battle.read(cx).arena.phase == Phase::Victory;
                this.close(cx);
                this.composer_focus.focus(window, cx);
            },
        ));
        let initial_focus = battle.downgrade();
        self.battle = Some(battle);
        window.on_next_frame(move |window, cx| {
            let _ = initial_focus.update(cx, |view, cx| view.resume(window, cx));
        });
        cx.notify();
    }
    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(battle) = self.battle.take() {
            self.preview_seed = battle.read(cx).arena.visual_time;
            self.preview.set(scene::PreviewSample::at(
                battle.read(cx).arena.boss.guardian,
                self.preview_seed,
                motion::reduced_motion(cx),
            ));
            battle.update(cx, |view, cx| view.close(cx));
            self.dismissal = None;
            cx.notify();
        }
    }
}

pub(crate) fn home_surface(
    state: &Entity<HomeArena>,
    enabled: bool,
    content: impl IntoElement,
    window: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let battle = enabled.then(|| state.read(cx).battle.clone()).flatten();
    let mut surface = v_flex()
        .id("new-session-home")
        .debug_selector(|| "new-session-home".into())
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .overflow_hidden()
        .bg(cx.theme().background);
    if enabled && battle.is_none() && !state.read(cx).completed && state.read(cx).selected.is_some()
    {
        surface = surface.child(banner(state, window, cx));
    }
    let content = v_flex()
        .id("home-session-content")
        .relative()
        .size_full()
        .min_h_0()
        .overflow_y_scrollbar()
        .child(
            v_flex()
                .relative()
                .w_full()
                .flex_1()
                .items_center()
                .px_6()
                .pt_6()
                .pb_4()
                .child(content),
        );
    let content = if let Some(battle) = &battle {
        if motion::reduced_motion(cx) {
            content.invisible().into_any_element()
        } else {
            let opacity = battle.read(cx).home_opacity.clone();
            content
                .with_animation(
                    "unbound-home-content",
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
        }
    } else {
        content.into_any_element()
    };
    surface = surface.child(content);
    if let Some(battle) = battle {
        surface = surface.child(div().absolute().inset_0().size_full().child(battle));
    }
    surface.into_any_element()
}

fn banner(state: &Entity<HomeArena>, window: &mut Window, cx: &mut App) -> AnyElement {
    let measured = state.read(cx).preview.clone();
    let sample = measured.get();
    let seed = state.read(cx).preview_seed;
    let guardian = state.read(cx).selected.unwrap_or(sample.guardian);
    let state = state.downgrade();
    let art = div()
        .id("new-session-arena")
        .debug_selector(|| "unbound-banner".into())
        .absolute()
        .inset_0()
        .size_full()
        .overflow_hidden();
    let reduced = motion::reduced_motion(cx);
    if window.has_active_dialog(cx)
        || reduced
        || motion::pauses_while_inactive(!window.is_window_active())
    {
        let sample = if reduced {
            scene::PreviewSample::at(guardian, seed, true)
        } else {
            sample
        };
        art.child(scene::Entry::new(sample, measured, reduced, state))
            .into_any_element()
    } else {
        art.with_animation(
            "unbound-vignette",
            Animation::new(Duration::from_secs_f32(art::IDLE_PERIOD))
                .repeat()
                .with_max_fps(24.0),
            move |this, phase| {
                this.child(scene::Entry::new(
                    scene::PreviewSample::at(guardian, seed + phase * art::IDLE_PERIOD, false),
                    measured.clone(),
                    false,
                    state.clone(),
                ))
            },
        )
        .into_any_element()
    }
}

struct ArenaView {
    arena: Arena,
    entry: Option<scene::PreviewViewport>,
    focus: FocusHandle,
    keys: BTreeSet<String>,
    mouse_shoot: bool,
    mouse_recall: bool,
    aim: Option<Vec2>,
    pointer: Option<Point<Pixels>>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    projection: Rc<Cell<Option<scene::Geometry>>>,
    home_opacity: Rc<Cell<f32>>,
    paused: bool,
    started: bool,
    closed: bool,
    clock: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}
impl EventEmitter<DismissEvent> for ArenaView {}
impl ArenaView {
    fn new(sample: scene::PreviewSample, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |view, _, cx| view.pause(cx));
        let activation = cx.observe_window_activation(window, |view, window, cx| {
            if !window.is_window_active() {
                view.pause(cx);
            }
        });
        let seed = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.subsec_nanos())
            .unwrap_or(0x6d2b79f5);
        Self {
            arena: Arena::from_preview(sample.boss, sample.time, seed),
            entry: sample.viewport,
            focus,
            keys: BTreeSet::new(),
            mouse_shoot: false,
            mouse_recall: false,
            aim: None,
            pointer: None,
            bounds: Rc::new(Cell::new(None)),
            projection: Rc::new(Cell::new(None)),
            home_opacity: Rc::new(Cell::new(1.0)),
            paused: true,
            started: false,
            closed: false,
            clock: None,
            _subscriptions: vec![blur, activation],
        }
    }
    fn controls(&self) -> Controls {
        let held =
            |letter: &str, arrow: &str| self.keys.contains(letter) || self.keys.contains(arrow);
        Controls {
            movement: Vec2::new(
                i32::from(held("d", "right")) as f32 - i32::from(held("a", "left")) as f32,
                i32::from(held("s", "down")) as f32 - i32::from(held("w", "up")) as f32,
            ),
            shoot: self.mouse_shoot || self.keys.contains("j"),
            recall: self.mouse_recall || self.keys.contains("k"),
            aim: self
                .pointer
                .and_then(|p| self.world_position(p))
                .or(self.aim),
        }
    }
    fn clear_input(&mut self) {
        self.keys.clear();
        self.mouse_shoot = false;
        self.mouse_recall = false;
        self.aim = None;
        self.pointer = None;
        self.arena.cancel_input();
    }
    fn pause(&mut self, cx: &mut Context<Self>) {
        self.paused = true;
        self.clock = None;
        self.clear_input();
        cx.notify();
    }
    fn close(&mut self, cx: &mut Context<Self>) {
        self.closed = true;
        self.pause(cx);
    }
    fn exit(&mut self, _: &Exit, _: &mut Window, cx: &mut Context<Self>) {
        self.close(cx);
        cx.emit(DismissEvent);
        cx.stop_propagation();
    }
    fn resume(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.closed {
            return;
        }
        self.clear_input();
        self.paused = false;
        self.started = true;
        self.focus.focus(window, cx);
        self.start_clock(window, cx);
        cx.notify();
    }
    fn step(&mut self, controls: Controls, cx: &mut Context<Self>) {
        self.arena.tick(controls);
        self.home_opacity.set(self.arena.home_opacity());
        if matches!(
            self.arena.phase,
            Phase::Victory | Phase::Defeat | Phase::Rebirth
        ) {
            self.clear_input();
        }
        if self.arena.outcome_ready() && !self.closed {
            self.close(cx);
            cx.emit(DismissEvent);
        }
    }
    fn start_clock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clock = None;
        if !self.arena.needs_tick() {
            return;
        }
        let executor = cx.background_executor().clone();
        self.clock = Some(cx.spawn_in(window, async move |entity, cx| {
            let mut last = Instant::now();
            let mut accumulated = 0.0;
            loop {
                executor.timer(Duration::from_millis(16)).await;
                let now = Instant::now();
                accumulated += now.duration_since(last).as_secs_f32().min(0.1);
                last = now;
                let keep_running = entity
                    .update_in(cx, |view, window, cx| {
                        if view.closed || view.paused {
                            return false;
                        }
                        if !view.focus.is_focused(window) || !window.is_window_active() {
                            view.pause(cx);
                            return false;
                        }
                        while accumulated >= STEP && !view.closed {
                            view.step(view.controls(), cx);
                            accumulated -= STEP;
                        }
                        cx.notify();
                        !view.closed && view.arena.needs_tick()
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }
    fn toggle_pause(&mut self, _: &TogglePause, window: &mut Window, cx: &mut Context<Self>) {
        if self.paused {
            self.resume(window, cx);
        } else {
            self.pause(cx);
        }
        cx.stop_propagation();
    }
    fn retry(&mut self, _: &Retry, window: &mut Window, cx: &mut Context<Self>) {
        if self.arena.phase != Phase::Victory {
            self.arena.respawn();
            self.entry = None;
            self.home_opacity.set(0.0);
            self.resume(window, cx);
        }
        cx.stop_propagation();
    }
    fn advance(&mut self, _: &Advance, window: &mut Window, cx: &mut Context<Self>) {
        if self.paused {
            self.resume(window, cx);
        }
        cx.stop_propagation();
    }
    fn roll(&mut self, _: &Roll, _: &mut Window, cx: &mut Context<Self>) {
        if !self.paused {
            self.arena.roll(self.controls().movement);
            cx.notify();
        }
        cx.stop_propagation();
    }
    fn key_down(&mut self, event: &KeyDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        let modifiers = event.keystroke.modifiers;
        if self.paused
            || !self.focus.is_focused(window)
            || modifiers.control
            || modifiers.platform
            || modifiers.alt
        {
            return;
        }
        if matches!(
            key,
            "w" | "a" | "s" | "d" | "up" | "down" | "left" | "right" | "j" | "k"
        ) {
            self.keys.insert(key.to_owned());
            if matches!(key, "j" | "k") {
                self.aim = None;
                self.pointer = None;
            }
            cx.stop_propagation();
        }
    }
    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.keys.remove(key) {
            if key == "j" && !self.mouse_shoot && !self.paused {
                self.arena.release_shot(self.controls().aim);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }
    fn world_position(&self, position: Point<Pixels>) -> Option<Vec2> {
        self.projection.get().and_then(|g| g.world(position))
    }
    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.paused {
            self.resume(window, cx);
            cx.stop_propagation();
            return;
        }
        if self.arena.phase != Phase::Battle {
            return;
        }
        if self.world_position(event.position).is_none() {
            return;
        }
        self.pointer = Some(event.position);
        self.aim = None;
        match event.button {
            MouseButton::Left => self.mouse_shoot = true,
            MouseButton::Right => self.mouse_recall = true,
            _ => {}
        }
        self.focus.focus(window, cx);
        cx.stop_propagation();
    }
    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.mouse_shoot {
            self.mouse_shoot = false;
            if !self.keys.contains("j") && !self.paused {
                self.arena.release_shot(self.controls().aim);
            }
            cx.notify();
        } else if event.button == MouseButton::Right {
            self.mouse_recall = false;
        }
    }
    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if self.world_position(event.position).is_some() {
            self.pointer = Some(event.position);
            self.aim = None;
        }
    }
}
impl Render for ArenaView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let board = scene::battle(
            &self.arena,
            self.entry,
            self.bounds.clone(),
            self.projection.clone(),
            motion::reduced_motion(cx),
            self.paused && self.started,
        );
        div()
            .id("unbound-game")
            .debug_selector(|| "unbound-game".into())
            .key_context("Unbound")
            .track_focus(&self.focus)
            .relative()
            .occlude()
            .overflow_hidden()
            .size_full()
            .min_h_0()
            .min_w_0()
            .aria_label(copy::battle_label(self.arena.boss.guardian, self.paused))
            .on_action(cx.listener(Self::roll))
            .on_action(cx.listener(Self::toggle_pause))
            .on_action(cx.listener(Self::retry))
            .on_action(cx.listener(Self::advance))
            .on_action(cx.listener(Self::exit))
            .on_key_down(cx.listener(Self::key_down))
            .on_key_up(cx.listener(Self::key_up))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up(MouseButton::Right, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Left, cx.listener(Self::mouse_up))
            .on_mouse_up_out(MouseButton::Right, cx.listener(Self::mouse_up))
            .child(
                div()
                    .id("unbound-battlefield")
                    .debug_selector(|| "unbound-battlefield".into())
                    .size_full()
                    .relative()
                    .overflow_hidden()
                    .when(!self.paused && self.arena.phase == Phase::Battle, |this| {
                        this.cursor_crosshair()
                    })
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .child(board),
            )
    }
}
#[cfg(test)]
mod tests;

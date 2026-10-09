//! A quiet home vignette and a local, immediately playable boss arena.
//!
//! The preview owns no simulation or timer. The home surface retains each
//! battle, its input focus and its bounded clock until play ends or navigation
//! leaves the home.

mod combat;
mod copy;
mod scene;

use std::{
    cell::Cell,
    collections::BTreeSet,
    rc::Rc,
    time::{Duration, Instant},
};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, Bounds, Context, DismissEvent, Entity,
    EventEmitter, FocusHandle, Global, KeyBinding, KeyDownEvent, KeyUpEvent, MouseButton,
    MouseDownEvent, MouseMoveEvent, MouseUpEvent, Pixels, Point, Subscription, Task, Window,
    actions, div, prelude::*, relative, rems,
};
use gpui_component::{
    ActiveTheme as _, Sizable as _, StyledExt as _, WindowExt as _,
    button::{Button, ButtonVariants as _},
    h_flex,
    scroll::ScrollableElement as _,
    v_flex,
};

use crate::{locale::text, motion};
use combat::{Arena, Controls, FOCUS_COST, Guardian, Phase, STEP, Vec2};

const BANNER_HEIGHT_REM: f32 = 12.0;

pub(crate) fn setting_title() -> &'static str {
    text("Home mini-game", "首页小游戏", "首頁小遊戲")
}

pub(crate) fn setting_description() -> &'static str {
    text(
        "Show the ASCII arena on the new-session home. Open it to challenge a guardian.",
        "在新建会话首页显示字符画战场，点击即可挑战守卫。",
        "在新增工作階段首頁顯示字元畫戰場，點擊即可挑戰守衛。",
    )
}

actions!(
    unbound,
    [Roll, Stillness, TogglePause, Retry, Advance, Exit]
);

struct Bindings;
impl Global for Bindings {}

fn init(cx: &mut App) {
    if cx.has_global::<Bindings>() {
        return;
    }
    cx.set_global(Bindings);
    cx.bind_keys([
        KeyBinding::new("space", Roll, Some("Unbound")),
        KeyBinding::new("e", Stillness, Some("Unbound")),
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
}

impl HomeArena {
    pub(crate) fn new(composer_focus: FocusHandle) -> Self {
        Self {
            battle: None,
            composer_focus,
            dismissal: None,
        }
    }

    fn open(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.battle.is_some() || window.has_active_dialog(cx) {
            return;
        }
        init(cx);
        let battle = cx.new(|cx| ArenaView::new(window, cx));
        self.dismissal =
            Some(
                cx.subscribe_in(&battle, window, |this, _, _: &DismissEvent, window, cx| {
                    this.close(cx);
                    this.composer_focus.focus(window, cx);
                }),
            );
        let initial_focus = battle.downgrade();
        self.battle = Some(battle);
        window.on_next_frame(move |window, cx| {
            let _ = initial_focus.update(cx, |view, cx| view.resume(window, cx));
        });
        cx.notify();
    }

    pub(crate) fn close(&mut self, cx: &mut Context<Self>) {
        if let Some(battle) = self.battle.take() {
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
    let surface = v_flex()
        .id("new-session-home")
        .debug_selector(|| "new-session-home".into())
        .relative()
        .size_full()
        .min_w_0()
        .min_h_0()
        .bg(cx.theme().background);
    if enabled && let Some(battle) = &state.read(cx).battle {
        return surface.child(battle.clone()).into_any_element();
    }
    surface
        .overflow_y_scrollbar()
        .when(enabled, |this| this.child(banner(state, window, cx)))
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
        )
        .into_any_element()
}

fn banner(state: &Entity<HomeArena>, window: &mut Window, cx: &mut App) -> AnyElement {
    let state = state.downgrade();
    let art = div().size_full().overflow_hidden();
    let dialog_open = window.has_active_dialog(cx);
    let art = if dialog_open
        || motion::reduced_motion(cx)
        || motion::pauses_while_inactive(!window.is_window_active())
    {
        art.child(scene::preview(0.15)).into_any_element()
    } else {
        art.with_animation(
            "unbound-vignette",
            Animation::new(Duration::from_secs(9))
                .repeat_synced()
                .with_max_fps(12.0),
            |this, phase| this.child(scene::preview(phase)),
        )
        .into_any_element()
    };
    let label = text("Play Unbound", "进入断链战场", "進入斷鏈戰場");
    div()
        .id("new-session-arena")
        .debug_selector(|| "unbound-banner".into())
        .absolute()
        .top_0()
        .left_0()
        .w_full()
        .h(rems(BANNER_HEIGHT_REM))
        .flex_none()
        .child(
            Button::new("open-unbound")
                .ghost()
                .w_full()
                .h_full()
                .p_0()
                .accessibility_label(label)
                .tooltip(label)
                .child(
                    div()
                        .relative()
                        .size_full()
                        .overflow_hidden()
                        .child(art)
                        .child(
                            h_flex()
                                .absolute()
                                .top_3()
                                .right_6()
                                .gap_2()
                                .text_xs()
                                .text_color(cx.theme().muted_foreground)
                                .child(copy::title())
                                .child(text("Play", "开始", "開始")),
                        ),
                )
                .on_click(move |_, window, cx| {
                    let _ = state.update(cx, |state, cx| state.open(window, cx));
                }),
        )
        .into_any_element()
}

struct ArenaView {
    arena: Arena,
    focus: FocusHandle,
    keys: BTreeSet<String>,
    mouse_shoot: bool,
    mouse_recall: bool,
    aim: Option<Vec2>,
    bounds: Rc<Cell<Option<Bounds<Pixels>>>>,
    paused: bool,
    closed: bool,
    attempt: u32,
    defeated: BTreeSet<&'static str>,
    clock: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<DismissEvent> for ArenaView {}

impl ArenaView {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |view, _, cx| view.pause(cx));
        let activation = cx.observe_window_activation(window, |view, window, cx| {
            // Combat requires input: losing the window must not cost a life.
            // This is independent of the preference for ambient UI animation.
            if !window.is_window_active() {
                view.pause(cx);
            }
        });
        Self {
            arena: Arena::new(Guardian::Ember),
            focus,
            keys: BTreeSet::new(),
            mouse_shoot: false,
            mouse_recall: false,
            aim: None,
            bounds: Rc::new(Cell::new(None)),
            paused: true,
            closed: false,
            attempt: 1,
            defeated: BTreeSet::new(),
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
            aim: self.aim,
        }
    }

    fn clear_input(&mut self) {
        self.keys.clear();
        self.mouse_shoot = false;
        self.mouse_recall = false;
        self.aim = None;
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
        self.focus.focus(window, cx);
        self.start_clock(window, cx);
        cx.notify();
    }

    fn start_clock(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.clock = None;
        if self.arena.phase != Phase::Battle {
            return;
        }
        let executor = cx.background_executor().clone();
        self.clock = Some(cx.spawn_in(window, async move |entity, cx| {
            let mut last = Instant::now();
            let mut accumulated = 0.0;
            loop {
                executor.timer(Duration::from_millis(33)).await;
                let now = Instant::now();
                // A suspended or overloaded UI never advances a lethal backlog.
                accumulated += now.duration_since(last).as_secs_f32().min(0.1);
                last = now;
                let keep_running = entity
                    .update_in(cx, |view, window, cx| {
                        if view.closed || view.paused || view.arena.phase != Phase::Battle {
                            return false;
                        }
                        if !view.focus.is_focused(window) || !window.is_window_active() {
                            view.pause(cx);
                            return false;
                        }
                        let controls = view.controls();
                        while accumulated >= STEP {
                            view.arena.tick(controls);
                            accumulated -= STEP;
                        }
                        if view.arena.phase == Phase::Victory {
                            view.defeated.insert(view.arena.boss.guardian.agent());
                            view.clear_input();
                        } else if view.arena.phase == Phase::Defeat {
                            view.clear_input();
                        }
                        cx.notify();
                        view.arena.phase == Phase::Battle
                    })
                    .unwrap_or(false);
                if !keep_running {
                    break;
                }
            }
        }));
    }

    fn toggle_pause(&mut self, _: &TogglePause, window: &mut Window, cx: &mut Context<Self>) {
        if self.arena.phase == Phase::Battle {
            if self.paused {
                self.resume(window, cx);
            } else {
                self.pause(cx);
            }
        }
        cx.stop_propagation();
    }

    fn retry(&mut self, _: &Retry, window: &mut Window, cx: &mut Context<Self>) {
        self.arena = Arena::new(self.arena.boss.guardian);
        self.attempt = self.attempt.saturating_add(1);
        self.resume(window, cx);
        cx.stop_propagation();
    }

    fn advance(&mut self, _: &Advance, window: &mut Window, cx: &mut Context<Self>) {
        if self.arena.phase == Phase::Victory {
            self.arena = Arena::new(self.arena.boss.guardian.next());
            self.attempt = 1;
            self.resume(window, cx);
        } else if self.arena.phase == Phase::Defeat {
            self.retry(&Retry, window, cx);
        } else if self.paused {
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

    fn stillness(&mut self, _: &Stillness, _: &mut Window, cx: &mut Context<Self>) {
        if !self.paused {
            self.arena.focus();
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
            // A keyboard attack deliberately restores assisted aim after mouse use.
            if matches!(key, "j" | "k") {
                self.aim = None;
            }
            cx.stop_propagation();
        }
    }

    fn key_up(&mut self, event: &KeyUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        let key = event.keystroke.key.as_str();
        if self.keys.remove(key) {
            if key == "j" && !self.mouse_shoot && !self.paused {
                self.arena.release_shot(self.aim);
            }
            cx.stop_propagation();
            cx.notify();
        }
    }

    fn world_position(&self, position: Point<Pixels>) -> Option<Vec2> {
        self.bounds
            .get()
            .and_then(|bounds| scene::Geometry::battle(bounds).world(position))
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.arena.phase != Phase::Battle {
            return;
        }
        let Some(aim) = self.world_position(event.position) else {
            return;
        };
        if self.paused {
            self.resume(window, cx);
        } else {
            self.aim = Some(aim);
            match event.button {
                MouseButton::Left => self.mouse_shoot = true,
                MouseButton::Right => self.mouse_recall = true,
                _ => {}
            }
            self.focus.focus(window, cx);
        }
        cx.stop_propagation();
    }

    fn mouse_up(&mut self, event: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        if event.button == MouseButton::Left && self.mouse_shoot {
            self.mouse_shoot = false;
            if !self.keys.contains("j") && !self.paused {
                self.arena.release_shot(self.aim);
            }
            cx.notify();
        } else if event.button == MouseButton::Right {
            self.mouse_recall = false;
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, _: &mut Context<Self>) {
        if let Some(aim) = self.world_position(event.position) {
            self.aim = Some(aim);
        }
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let guardian = self.arena.boss.guardian;
        let paused = self.paused;
        v_flex()
            .debug_selector(|| "unbound-header".into())
            .flex_none()
            .gap_3()
            .px_4()
            .pt_4()
            .pb_3()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .gap_3()
                    .items_start()
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .gap_1()
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(format!("Vibex / {}", copy::title())),
                            )
                            .child(div().text_base().font_semibold().child(format!(
                                "{} · {}",
                                guardian.agent(),
                                copy::guardian_name(guardian)
                            ))),
                    )
                    .child(
                        h_flex()
                            .flex_none()
                            .gap_1()
                            .child(
                                Button::new("unbound-pause")
                                    .debug_selector(|| "unbound-pause".into())
                                    .small()
                                    .ghost()
                                    .label(if self.paused {
                                        text("Resume", "继续", "繼續")
                                    } else {
                                        text("Pause", "暂停", "暫停")
                                    })
                                    .tooltip(text(
                                        "Pause / resume (P)",
                                        "暂停 / 继续（P）",
                                        "暫停 / 繼續（P）",
                                    ))
                                    .on_click(cx.listener(move |this, _, window, cx| {
                                        // A button may take focus before its click is delivered.
                                        // Honor the command shown before that automatic pause.
                                        if paused {
                                            this.resume(window, cx);
                                        } else {
                                            this.pause(cx);
                                        }
                                    })),
                            )
                            .child(
                                Button::new("unbound-close")
                                    .debug_selector(|| "unbound-close".into())
                                    .small()
                                    .ghost()
                                    .label(text("Back", "返回", "返回"))
                                    .tooltip(text(
                                        "Back to new session (Esc)",
                                        "返回新建会话（Esc）",
                                        "返回新增工作階段（Esc）",
                                    ))
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.exit(&Exit, window, cx);
                                    })),
                            ),
                    ),
            )
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_2()
                    .text_xs()
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(text("Life", "生命", "生命")),
                            )
                            .child(
                                div()
                                    .font_family("Lilex")
                                    .text_color(cx.theme().danger)
                                    .child(format!(
                                        "{}{}",
                                        "◆".repeat(usize::from(self.arena.player.health)),
                                        "◇".repeat(usize::from(3 - self.arena.player.health))
                                    )),
                            ),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                div()
                                    .text_color(cx.theme().muted_foreground)
                                    .child(text("Focus", "专注", "專注")),
                            )
                            .child(
                                div().w_16().h_1().bg(cx.theme().muted).child(
                                    div()
                                        .h_full()
                                        .w(relative(self.arena.player.focus / 100.0))
                                        .bg(cx.theme().info),
                                ),
                            )
                            .child(format!("{:.0}", self.arena.player.focus)),
                    )
                    .child(
                        div()
                            .text_color(if self.arena.boss.exposed > 0.0 {
                                cx.theme().warning
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(copy::boss_state(&self.arena)),
                    )
                    .child(div().text_color(cx.theme().muted_foreground).child(format!(
                        "{} {}/{}",
                        text("Guardians", "守卫", "守衛"),
                        self.defeated.len(),
                        Guardian::ALL.len()
                    ))),
            )
            .into_any_element()
    }

    fn outcome(&self, cx: &mut Context<Self>) -> AnyElement {
        let (heading, description, label) = match self.arena.phase {
            Phase::Victory => (
                text("Core shattered", "核心已击破", "核心已擊破"),
                text(
                    "One arrow. One opening.",
                    "一支箭，一瞬破绽。",
                    "一支箭，一瞬破綻。",
                ),
                text("Next guardian", "下一位守卫", "下一位守衛"),
            ),
            Phase::Defeat => (
                text(
                    "Try another angle",
                    "换个角度，再来一次",
                    "換個角度，再來一次",
                ),
                copy::tactic(self.arena.boss.guardian),
                text("Try again", "再战一次", "再戰一次"),
            ),
            Phase::Battle => (
                text("Paused", "已暂停", "已暫停"),
                text(
                    "Your battle will wait for you.",
                    "战场会等你回来。",
                    "戰場會等你回來。",
                ),
                text("Resume", "继续战斗", "繼續戰鬥"),
            ),
        };
        div()
            .absolute()
            .inset_0()
            .flex()
            .items_center()
            .justify_center()
            .p_4()
            .child(
                v_flex()
                    .w_full()
                    .max_w(rems(28.0))
                    .gap_3()
                    .p_5()
                    .bg(cx.theme().background.opacity(0.97))
                    .border_1()
                    .border_color(cx.theme().border)
                    .rounded(cx.theme().radius)
                    .child(div().text_lg().font_semibold().child(heading))
                    .child(
                        div()
                            .text_sm()
                            .text_color(cx.theme().muted_foreground)
                            .child(description),
                    )
                    .child(
                        h_flex()
                            .gap_2()
                            .child(
                                Button::new("unbound-continue")
                                    .primary()
                                    .label(label)
                                    .debug_selector(|| "unbound-continue".into())
                                    .on_click(cx.listener(|this, _, window, cx| {
                                        this.advance(&Advance, window, cx)
                                    })),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(cx.theme().muted_foreground)
                                    .child("Enter"),
                            ),
                    ),
            )
            .into_any_element()
    }

    fn footer(&self, cx: &mut Context<Self>) -> AnyElement {
        let focused = self.arena.focus_remaining > 0.0;
        v_flex()
            .debug_selector(|| "unbound-footer".into())
            .flex_none()
            .gap_2()
            .px_4()
            .pb_4()
            .pt_3()
            .child(
                h_flex()
                    .w_full()
                    .min_w_0()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_2()
                    .text_xs()
                    .child(
                        div()
                            .text_color(cx.theme().foreground)
                            .child(copy::arrow_state(&self.arena)),
                    )
                    .child(
                        div()
                            .text_color(if focused {
                                cx.theme().info
                            } else {
                                cx.theme().muted_foreground
                            })
                            .child(if focused {
                                text("Stillness active", "静域生效中", "靜域生效中")
                            } else if self.arena.player.focus >= FOCUS_COST {
                                text("E · Stillness ready", "E · 静域就绪", "E · 靜域就緒")
                            } else {
                                text("E · Recovering focus", "E · 专注恢复中", "E · 專注恢復中")
                            }),
                    )
                    .child(div().text_color(cx.theme().muted_foreground).child(format!(
                        "{} {}",
                        text("Attempt", "尝试", "嘗試"),
                        self.attempt
                    )))
                    .child(
                        Button::new("unbound-retry")
                            .xsmall()
                            .ghost()
                            .label(text("Retry", "重试", "重試"))
                            .tooltip(text(
                                "Retry this guardian (R)",
                                "重试当前守卫（R）",
                                "重試目前守衛（R）",
                            ))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.retry(&Retry, window, cx)),
                            ),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .text_sm()
                    .text_color(cx.theme().muted_foreground)
                    .child(copy::cue(&self.arena)),
            )
            .child(
                h_flex()
                    .flex_wrap()
                    .gap_x_4()
                    .gap_y_1()
                    .text_xs()
                    .text_color(cx.theme().muted_foreground)
                    .child(text(
                        "WASD / ↑↓←→  Move",
                        "WASD / 方向键  移动",
                        "WASD / 方向鍵  移動",
                    ))
                    .child(text(
                        "Hold J / left mouse → release to shoot",
                        "按住 J / 左键 → 松开射箭",
                        "按住 J / 左鍵 → 鬆開射箭",
                    ))
                    .child(text(
                        "Hold K / right mouse  Recall",
                        "按住 K / 右键  召回",
                        "按住 K / 右鍵  召回",
                    ))
                    .child(text("Space  Roll", "空格  翻滚", "空格  翻滾"))
                    .child(text(
                        "E  Slow threats · 60 focus",
                        "E  静域减速 · 60 专注",
                        "E  靜域減速 · 60 專注",
                    )),
            )
            .into_any_element()
    }
}

impl Render for ArenaView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let board = scene::battle(&self.arena, self.bounds.clone(), motion::reduced_motion(cx));
        v_flex()
            .id("unbound-game")
            .debug_selector(|| "unbound-game".into())
            .key_context("Unbound")
            .track_focus(&self.focus)
            .size_full()
            .min_h_0()
            .min_w_0()
            .bg(cx.theme().background)
            .border_1()
            .border_color(cx.theme().transparent)
            .focus_visible(|style| style.border_color(cx.theme().ring))
            .on_action(cx.listener(Self::roll))
            .on_action(cx.listener(Self::stillness))
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
            .child(self.header(cx))
            .child(
                div()
                    .id("unbound-battlefield")
                    .debug_selector(|| "unbound-battlefield".into())
                    .relative()
                    .w_full()
                    .flex_1()
                    .min_h_0()
                    .min_w_0()
                    .overflow_hidden()
                    .border_y_1()
                    .border_color(cx.theme().border.opacity(0.55))
                    .when(!self.paused && self.arena.phase == Phase::Battle, |this| {
                        this.cursor_crosshair()
                    })
                    .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
                    .on_mouse_down(MouseButton::Right, cx.listener(Self::mouse_down))
                    .on_mouse_move(cx.listener(Self::mouse_move))
                    .child(board)
                    .when(self.paused || self.arena.phase != Phase::Battle, |this| {
                        this.child(self.outcome(cx))
                    }),
            )
            .child(self.footer(cx))
    }
}

#[cfg(test)]
mod tests;

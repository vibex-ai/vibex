use super::*;
use combat::{ArrowState, VICTORY_DURATION};
use gpui::{
    Focusable as _, Keystroke, Modifiers, TestAppContext, VisualTestContext, point, px, rems, size,
};
use gpui_component::{
    Root, Theme, ThemeMode,
    button::Button,
    h_flex,
    input::{Input, InputState},
};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
}
fn key(cx: &mut VisualTestContext, name: &str, down: bool) {
    let keystroke = Keystroke::parse(name).unwrap();
    if down {
        cx.simulate_event(KeyDownEvent {
            keystroke,
            is_held: false,
            prefer_character_input: false,
        });
    } else {
        cx.simulate_event(KeyUpEvent { keystroke });
    }
    draw(cx);
}
fn press(cx: &mut VisualTestContext, name: &str) {
    key(cx, name, true);
    key(cx, name, false);
}
fn setup(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        init(cx);
        cx.set_reduce_motion(true);
    });
}
fn steps(view: &Entity<ArenaView>, count: usize, cx: &mut VisualTestContext) {
    view.update(cx, |view, cx| {
        view.clock = None;
        for _ in 0..count {
            view.step(view.controls(), cx);
            if view.closed {
                break;
            }
        }
        cx.notify();
    });
    draw(cx);
}
fn awaken(view: &Entity<ArenaView>, cx: &mut VisualTestContext) {
    steps(view, 86, cx);
    assert_eq!(view.read_with(cx, |v, _| v.arena.phase), Phase::Battle);
}
fn click(cx: &mut VisualTestContext, id: &'static str) {
    let b = cx
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("missing {id}"));
    cx.simulate_click(b.center(), Modifiers::none());
    draw(cx);
    draw(cx);
}

struct Home {
    input: Entity<InputState>,
    arena: Entity<HomeArena>,
    show_arena: bool,
    left_home: bool,
    _subscription: Subscription,
}
impl Home {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let arena = cx.new(|cx| HomeArena::new(input.read(cx).focus_handle(cx)));
        let subscription = cx.observe(&arena, |_, _, cx| cx.notify());
        Self {
            input,
            arena,
            show_arena: true,
            left_home: false,
            _subscription: subscription,
        }
    }
}
impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let content = v_flex()
            .w_full()
            .max_w(rems(48.0))
            .flex_1()
            .justify_center()
            .child(
                v_flex()
                    .id("home-content")
                    .debug_selector(|| "home-content".into())
                    .occlude()
                    .h(rems(14.0))
                    .gap_4()
                    .child("Start a new session")
                    .child(
                        div()
                            .debug_selector(|| "home-composer".into())
                            .child(Input::new(&self.input)),
                    ),
            );
        v_flex()
            .size_full()
            .child(
                h_flex()
                    .h_8()
                    .flex_none()
                    .child(
                        Button::new("home-navigation")
                            .debug_selector(|| "home-navigation".into())
                            .label("Sessions")
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.arena.update(cx, |arena, cx| arena.close(cx));
                                this.left_home = true;
                                cx.notify();
                            })),
                    )
                    .children(
                        [
                            "claude",
                            "codex",
                            "pi",
                            "opencode",
                            "deepseek-harness",
                            "copilot",
                        ]
                        .into_iter()
                        .map(|id| {
                            Button::new(format!("home-agent-{id}"))
                                .debug_selector(move || format!("home-agent-{id}"))
                                .label(id)
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.arena
                                        .update(cx, |arena, cx| arena.select_agent(Some(id), cx))
                                }))
                        }),
                    ),
            )
            .child(div().flex_1().min_h_0().when(!self.left_home, |this| {
                this.child(home_surface(
                    &self.arena,
                    self.show_arena,
                    content,
                    window,
                    cx,
                ))
            }))
    }
}
fn open_home(cx: &mut TestAppContext) -> (Entity<Home>, &mut VisualTestContext) {
    setup(cx);
    let mut home = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Home::new(window, cx));
        home = Some(view.clone());
        Root::new(view, window, cx)
    });
    cx.update(|window, _| window.activate_window());
    draw(cx);
    (home.unwrap(), cx)
}
fn battle_view(home: &Entity<Home>, cx: &VisualTestContext) -> Entity<ArenaView> {
    home.read_with(cx, |home, cx| home.arena.read(cx).battle.clone().unwrap())
}

#[gpui::test]
fn only_the_guardian_is_a_pointer_entry_and_escape_restores_the_draft(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    cx.update(|window, cx| {
        home.read(cx)
            .input
            .clone()
            .update(cx, |input, cx| input.focus(window, cx))
    });
    cx.simulate_input("draft ");
    let page = cx.debug_bounds("new-session-home").unwrap();
    cx.simulate_click(
        point(page.right() - px(10.0), page.top() + px(10.0)),
        Modifiers::none(),
    );
    draw(cx);
    assert!(home.read_with(cx, |home, cx| home.arena.read(cx).battle.is_none()));
    click(cx, "unbound-boss-entry");
    let view = battle_view(&home, cx);
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert_eq!(
        cx.debug_bounds("unbound-game"),
        cx.debug_bounds("new-session-home")
    );
    assert_eq!(
        cx.debug_bounds("unbound-battlefield"),
        cx.debug_bounds("new-session-home")
    );
    for old in [
        "unbound-header",
        "unbound-footer",
        "unbound-continue",
        "unbound-close",
        "unbound-guardian-Claude",
    ] {
        assert!(cx.debug_bounds(old).is_none());
    }
    assert!(cx.update(|window, cx| view.read(cx).focus.is_focused(window)));
    key(cx, "j", true);
    press(cx, "escape");
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.closed
        && v.clock.is_none()
        && v.keys.is_empty()));
    cx.simulate_input("jwasd");
    assert_eq!(
        home.read_with(cx, |h, cx| h.input.read(cx).value().to_string()),
        "draft jwasd"
    );
    press(cx, "shift-tab");
    press(cx, "enter");
    draw(cx);
    assert!(cx.debug_bounds("unbound-game").is_some());
    press(cx, "escape");
    home.update(cx, |home, cx| {
        home.show_arena = false;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("unbound-boss-entry").is_none());
}
#[gpui::test]
fn roaming_and_opt_out_preserve_home_layout_and_composer_hit_testing(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    for (width, height, rem, mode) in [
        (600.0, 400.0, 14.0, ThemeMode::Light),
        (900.0, 640.0, 16.0, ThemeMode::Dark),
        (1200.0, 760.0, 24.0, ThemeMode::Light),
    ] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| {
            Theme::change(mode, Some(window), cx);
            Theme::global_mut(cx).font_size = px(rem);
            Theme::sync_base(cx);
        });
        home.update(cx, |h, cx| {
            h.show_arena = false;
            cx.notify();
        });
        draw(cx);
        let before = cx.debug_bounds("home-content").unwrap();
        home.update(cx, |h, cx| {
            h.show_arena = true;
            cx.notify();
        });
        draw(cx);
        assert_eq!(cx.debug_bounds("home-content"), Some(before));
        assert_eq!(
            cx.debug_bounds("unbound-banner"),
            cx.debug_bounds("new-session-home")
        );
        click(cx, "home-composer");
        cx.simulate_input("a");
        assert!(home.read_with(cx, |h, cx| h.arena.read(cx).battle.is_none()));
    }
    assert_eq!(
        home.read_with(cx, |h, cx| h.input.read(cx).value().to_string()),
        "aaa"
    );
}
#[gpui::test]
fn navigation_cancels_both_pending_entry_focus_and_live_combat(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    let state = home.read_with(cx, |h, _| h.arena.clone());
    let early = cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.open(window, cx);
            let battle = state.battle.clone().unwrap();
            state.close(cx);
            battle
        })
    });
    draw(cx);
    draw(cx);
    assert!(early.read_with(cx, |v, _| v.closed && v.clock.is_none()));
    click(cx, "unbound-boss-entry");
    let view = battle_view(&home, cx);
    key(cx, "w", true);
    click(cx, "home-navigation");
    assert!(view.read_with(cx, |v, _| v.closed
        && v.clock.is_none()
        && v.keys.is_empty()));
    assert!(cx.debug_bounds("unbound-game").is_none());
}
#[gpui::test]
fn agent_switches_select_the_model_and_victory_hides_it_until_the_next_switch(
    cx: &mut TestAppContext,
) {
    let (home, cx) = open_home(cx);
    cx.update(|window, cx| {
        home.read(cx)
            .input
            .clone()
            .update(cx, |input, cx| input.focus(window, cx))
    });
    cx.simulate_input("keep me");
    for reduced in [true, false] {
        cx.update(|_, cx| cx.set_reduce_motion(reduced));
        draw(cx);
        for (id, guardian) in [
            ("home-agent-claude", Guardian::Claude),
            ("home-agent-codex", Guardian::Codex),
            ("home-agent-pi", Guardian::Pi),
            ("home-agent-opencode", Guardian::OpenCode),
            ("home-agent-deepseek-harness", Guardian::DeepSeek),
            ("home-agent-copilot", Guardian::Copilot),
        ] {
            click(cx, id);
            click(cx, "unbound-boss-entry");
            assert!(
                home.read_with(cx, |h, cx| h.arena.read(cx).battle.is_some()),
                "{guardian:?} entry, reduced motion: {reduced}"
            );
            let view = battle_view(&home, cx);
            assert_eq!(view.read_with(cx, |v, _| v.arena.boss.guardian), guardian);
            view.update(cx, |v, cx| {
                v.arena.phase = Phase::Victory;
                v.arena.phase_time = VICTORY_DURATION - 0.05;
                cx.notify();
            });
            steps(&view, 8, cx);
            draw(cx);
            assert!(view.read_with(cx, |v, _| v.closed && v.clock.is_none()));
            assert!(cx.debug_bounds("unbound-game").is_none());
            assert!(cx.debug_bounds("unbound-boss-entry").is_none());
            click(cx, id);
            assert!(cx.debug_bounds("unbound-boss-entry").is_none());
        }
        click(cx, "home-agent-claude");
    }
    click(cx, "home-agent-claude");
    assert!(cx.debug_bounds("unbound-boss-entry").is_some());
    assert_eq!(
        home.read_with(cx, |h, cx| h.input.read(cx).value().to_string()),
        "keep me"
    );
}
#[gpui::test]
fn the_first_frame_uses_the_measured_pose_and_full_home_projection(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    cx.update(|_, cx| cx.set_reduce_motion(false));
    draw(cx);
    let state = home.read_with(cx, |h, _| h.arena.clone());
    click(cx, "unbound-boss-entry");
    // Pointer down/hover can draw another idle frame before pointer up. The
    // retained sample is the one activation actually consumed.
    let sample = state.read_with(cx, |s, _| s.preview.get());
    let view = battle_view(&home, cx);
    view.update(cx, |view, cx| {
        view.clock = None;
        view.arena.phase_time = 0.0;
        cx.notify();
    });
    draw(cx);
    assert_eq!(
        view.read_with(cx, |v, _| v.projection.get()),
        sample.viewport.map(|v| v.geometry)
    );
    assert!(view.read_with(
        cx,
        |v, _| v.arena.boss.position.minus(sample.boss.position).length() < 0.05
    ));
    steps(&view, 35, cx);
    assert!(view.read_with(cx, |v, _| v.home_opacity.get() < 0.01));
    press(cx, "escape");
    assert!(cx.debug_bounds("home-composer").is_some());
}
#[gpui::test]
fn death_rebirth_clears_input_and_never_opens_a_result_surface(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    click(cx, "unbound-boss-entry");
    let view = battle_view(&home, cx);
    awaken(&view, cx);
    let before = view.read_with(cx, |v, _| v.arena.player.position);
    key(cx, "j", true);
    view.update(cx, |v, cx| {
        v.arena.phase = Phase::Defeat;
        v.arena.phase_time = 0.0;
        cx.notify();
    });
    steps(&view, 106, cx);
    view.read_with(cx, |v, _| {
        assert_eq!(v.arena.phase, Phase::Battle);
        assert_ne!(v.arena.player.position, before);
        assert!(v.keys.is_empty());
        assert_eq!(v.arena.arrow.state, ArrowState::Ready);
        assert!(!v.closed);
    });
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.debug_bounds("unbound-continue").is_none());
    assert!(cx.debug_bounds("unbound-game").is_some());
}
#[gpui::test]
fn keyboard_pointer_focus_and_pause_use_the_live_battle_without_chrome(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    click(cx, "unbound-boss-entry");
    let view = battle_view(&home, cx);
    awaken(&view, cx);
    key(cx, "j", true);
    steps(&view, 22, cx);
    key(cx, "j", false);
    assert_eq!(
        view.read_with(cx, |v, _| v.arena.arrow.state),
        ArrowState::Flying
    );
    press(cx, "space");
    assert!(view.read_with(cx, |v, _| v.arena.player.roll_remaining > 0.0));
    key(cx, "w", true);
    press(cx, "p");
    assert!(view.read_with(cx, |v, _| v.paused
        && v.clock.is_none()
        && v.keys.is_empty()));
    click(cx, "unbound-battlefield");
    assert!(view.read_with(cx, |v, _| !v.paused && v.clock.is_some() && !v.mouse_shoot));
    press(cx, "r");
    steps(&view, 43, cx);
    assert_eq!(view.read_with(cx, |v, _| v.arena.phase), Phase::Battle);
    let field = cx.debug_bounds("unbound-battlefield").unwrap();
    let target = field.center() + point(px(50.0), px(-30.0));
    cx.simulate_mouse_down(target, MouseButton::Left, Modifiers::none());
    steps(&view, 22, cx);
    cx.simulate_mouse_up(
        point(px(-1.0), px(-1.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    assert!(view.read_with(cx, |v, _| !v.mouse_shoot));
    assert_eq!(
        view.read_with(cx, |v, _| v.arena.arrow.state),
        ArrowState::Flying
    );
    view.update(cx, |v, cx| {
        v.arena.camera = map::CENTER;
        cx.notify();
    });
    draw(cx);
    let before = view.read_with(cx, |v, _| v.controls().aim.unwrap());
    view.update(cx, |v, cx| {
        v.arena.camera.x += 20.0;
        cx.notify();
    });
    draw(cx);
    let after = view.read_with(cx, |v, _| v.controls().aim.unwrap());
    assert!((before.x - after.x).abs() > 0.1);
    key(cx, "k", true);
    assert!(view.read_with(cx, |v, _| v.controls().aim.is_none()));
    cx.update(|window, cx| window.blur(cx));
    draw(cx);
    assert!(view.read_with(cx, |v, _| v.paused
        && v.clock.is_none()
        && v.keys.is_empty()));
}
#[gpui::test]
fn the_battle_fills_the_home_at_narrow_widths_and_zoom_levels(cx: &mut TestAppContext) {
    let (home, cx) = open_home(cx);
    click(cx, "unbound-boss-entry");
    let view = battle_view(&home, cx);
    for (w, h, rem, mode) in [
        (360.0, 620.0, 14.0, ThemeMode::Light),
        (900.0, 720.0, 16.0, ThemeMode::Dark),
        (1200.0, 860.0, 24.0, ThemeMode::Light),
    ] {
        cx.simulate_resize(size(px(w), px(h)));
        cx.update(|window, cx| {
            Theme::change(mode, Some(window), cx);
            Theme::global_mut(cx).font_size = px(rem);
            Theme::sync_base(cx);
        });
        draw(cx);
        assert_eq!(
            cx.debug_bounds("unbound-battlefield"),
            cx.debug_bounds("new-session-home")
        );
        assert_eq!(
            cx.debug_bounds("unbound-game"),
            cx.debug_bounds("new-session-home")
        );
        let projection = view.read_with(cx, |v, _| v.projection.get().unwrap());
        assert!(projection.unit > px(0.0));
    }
}

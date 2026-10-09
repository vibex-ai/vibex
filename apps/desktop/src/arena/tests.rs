use super::*;
use combat::ArrowState;
use gpui::{
    Entity, Focusable as _, Keystroke, Modifiers, TestAppContext, VisualTestContext, point, px,
    size,
};
use gpui_component::{
    Root, Theme, ThemeMode,
    input::{Input, InputState},
};

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
}

fn key(cx: &mut VisualTestContext, key: &str, down: bool) {
    let keystroke = Keystroke::parse(key).unwrap();
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
                h_flex().h_8().flex_none().child(
                    Button::new("home-navigation")
                        .debug_selector(|| "home-navigation".into())
                        .label("Sessions")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.arena.update(cx, |arena, cx| arena.close(cx));
                            this.left_home = true;
                            cx.notify();
                        })),
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

#[gpui::test]
fn banner_expands_in_the_home_and_escape_preserves_the_composer(cx: &mut TestAppContext) {
    setup(cx);
    let mut home = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Home::new(window, cx));
        home = Some(view.clone());
        Root::new(view, window, cx)
    });
    let home = home.unwrap();
    cx.update(|window, _| window.activate_window());
    draw(cx);
    cx.update(|window, cx| {
        home.read(cx).input.clone().update(cx, |input, cx| {
            input.focus(window, cx);
        });
    });
    cx.simulate_input("draft ");
    let banner_bounds = cx.debug_bounds("unbound-banner").unwrap();
    cx.simulate_click(
        point(
            banner_bounds.right() - px(24.0),
            banner_bounds.top() + px(16.0),
        ),
        Modifiers::none(),
    );
    draw(cx);
    draw(cx);
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.debug_bounds("unbound-battlefield").is_some());
    assert_eq!(
        cx.debug_bounds("unbound-game"),
        cx.debug_bounds("new-session-home")
    );
    assert!(cx.debug_bounds("home-composer").is_none());
    let opened = home.read_with(cx, |home, cx| home.arena.read(cx).battle.clone().unwrap());
    assert!(cx.update(|window, cx| opened.read(cx).focus.is_focused(window)));
    key(cx, "j", true);
    assert!(opened.read_with(cx, |view, _| view.controls().shoot));
    press(cx, "escape");
    draw(cx);
    assert!(opened.read_with(cx, |view, _| view.closed
        && view.clock.is_none()
        && view.keys.is_empty()));
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));
    cx.simulate_input("jwasd");
    assert_eq!(
        home.read_with(cx, |home, cx| home.input.read(cx).value().to_string()),
        "draft jwasd"
    );

    // The entry is also reachable from the composer by keyboard.
    press(cx, "shift-tab");
    press(cx, "enter");
    draw(cx);
    assert!(cx.debug_bounds("unbound-game").is_some());
    let back = cx.debug_bounds("unbound-close").unwrap();
    cx.simulate_click(back.center(), Modifiers::none());
    draw(cx);
    assert!(cx.debug_bounds("home-composer").is_some());
    cx.simulate_input("k");
    assert_eq!(
        home.read_with(cx, |home, cx| home.input.read(cx).value().to_string()),
        "draft jwasdk"
    );

    home.update(cx, |home, cx| {
        home.show_arena = false;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("unbound-banner").is_none());
    assert!(cx.debug_bounds("unbound-game").is_none());
}

#[gpui::test]
fn the_preview_does_not_move_or_intercept_home_content(cx: &mut TestAppContext) {
    setup(cx);
    let mut home = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Home::new(window, cx));
        home = Some(view.clone());
        Root::new(view, window, cx)
    });
    let home = home.unwrap();
    for (width, height, rem, mode) in [
        (600.0, 360.0, 14.0, ThemeMode::Light),
        (900.0, 500.0, 16.0, ThemeMode::Dark),
        (1200.0, 640.0, 24.0, ThemeMode::Light),
    ] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| {
            Theme::change(mode, Some(window), cx);
            Theme::global_mut(cx).font_size = px(rem);
            Theme::sync_base(cx);
        });
        home.update(cx, |home, cx| {
            home.show_arena = false;
            cx.notify();
        });
        draw(cx);
        let without_preview = cx.debug_bounds("home-content").unwrap();
        home.update(cx, |home, cx| {
            home.show_arena = true;
            cx.notify();
        });
        draw(cx);
        let content = cx.debug_bounds("home-content").unwrap();
        let preview = cx.debug_bounds("unbound-banner").unwrap();
        assert_eq!(content, without_preview);
        assert!(content.top() < preview.bottom());
        let composer = cx.debug_bounds("home-composer").unwrap();
        assert!(composer.center().y < preview.bottom());
        cx.simulate_click(composer.center(), Modifiers::none());
        cx.simulate_input("a");
        draw(cx);
        assert!(home.read_with(cx, |home, cx| home.arena.read(cx).battle.is_none()));
        assert!(cx.debug_bounds("unbound-game").is_none());
    }
    assert_eq!(
        home.read_with(cx, |home, cx| home.input.read(cx).value().to_string()),
        "aaa"
    );
}

#[gpui::test]
fn leaving_home_cancels_pending_focus_and_running_combat(cx: &mut TestAppContext) {
    setup(cx);
    let mut home = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Home::new(window, cx));
        home = Some(view.clone());
        Root::new(view, window, cx)
    });
    let home = home.unwrap();
    cx.update(|window, _| window.activate_window());
    draw(cx);
    let state = home.read_with(cx, |home, _| home.arena.clone());
    let closed_before_focus = cx.update(|window, cx| {
        state.update(cx, |state, cx| {
            state.open(window, cx);
            let battle = state.battle.clone().unwrap();
            state.close(cx);
            battle
        })
    });
    draw(cx);
    draw(cx);
    assert!(closed_before_focus.read_with(cx, |view, _| view.closed && view.clock.is_none()));

    cx.update(|window, cx| state.update(cx, |state, cx| state.open(window, cx)));
    draw(cx);
    draw(cx);
    let battle = state.read_with(cx, |state, _| state.battle.clone().unwrap());
    key(cx, "w", true);
    assert!(battle.read_with(cx, |view, _| view.clock.is_some()));
    let navigation = cx.debug_bounds("home-navigation").unwrap();
    cx.simulate_click(navigation.center(), Modifiers::none());
    draw(cx);
    assert!(home.read_with(cx, |home, _| home.left_home));
    assert!(state.read_with(cx, |state, _| state.battle.is_none()));
    assert!(battle.read_with(cx, |view, _| view.closed
        && view.clock.is_none()
        && view.keys.is_empty()));
    assert!(cx.debug_bounds("unbound-game").is_none());
}

#[gpui::test]
fn keyboard_combat_pause_pointer_resume_and_retry_use_the_live_view(cx: &mut TestAppContext) {
    setup(cx);
    let mut arena = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| ArenaView::new(window, cx));
        arena = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = arena.unwrap();
    cx.update(|window, _| window.activate_window());
    draw(cx);
    cx.update(|window, cx| view.update(cx, |view, cx| view.resume(window, cx)));
    draw(cx);
    key(cx, "d", true);
    view.update(cx, |view, _| {
        let controls = view.controls();
        for _ in 0..12 {
            view.arena.tick(controls);
        }
    });
    key(cx, "d", false);
    assert!(view.read_with(cx, |view, _| view.arena.player.position.x > 50.0));
    assert_eq!(
        view.read_with(cx, |view, _| view.controls().movement),
        Vec2::default()
    );

    key(cx, "j", true);
    view.update(cx, |view, _| {
        let controls = view.controls();
        for _ in 0..20 {
            view.arena.tick(controls);
        }
    });
    key(cx, "j", false);
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.arrow.state),
        ArrowState::Flying
    );
    press(cx, "e");
    assert!(view.read_with(cx, |view, _| view.arena.focus_remaining > 0.0));
    press(cx, "space");
    assert!(view.read_with(cx, |view, _| view.arena.player.roll_remaining > 0.0));

    key(cx, "w", true);
    press(cx, "p");
    assert!(view.read_with(cx, |view, _| view.paused
        && view.keys.is_empty()
        && view.clock.is_none()));
    let resume = cx.debug_bounds("unbound-continue").unwrap();
    cx.simulate_click(resume.center(), Modifiers::none());
    draw(cx);
    assert!(view.read_with(cx, |view, _| !view.paused && view.clock.is_some()));
    let pause = cx.debug_bounds("unbound-pause").unwrap();
    cx.simulate_click(pause.center(), Modifiers::none());
    draw(cx);
    assert!(view.read_with(cx, |view, _| view.paused && view.clock.is_none()));
    press(cx, "p");
    press(cx, "r");
    assert_eq!(view.read_with(cx, |view, _| view.arena.player.health), 3);
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.arrow.state),
        ArrowState::Ready
    );
    let bounds = view.read_with(cx, |view, _| view.bounds.get().unwrap());
    let geometry = scene::Geometry::battle(bounds);
    let target = geometry.origin + point(geometry.unit * 74.0, geometry.unit * 10.0);
    cx.simulate_mouse_down(target, MouseButton::Left, Modifiers::none());
    view.update(cx, |view, _| {
        let controls = view.controls();
        for _ in 0..20 {
            view.arena.tick(controls);
        }
    });
    cx.simulate_mouse_up(
        point(px(-1.0), px(-1.0)),
        MouseButton::Left,
        Modifiers::none(),
    );
    draw(cx);
    assert!(view.read_with(cx, |view, _| !view.mouse_shoot
        && view.arena.arrow.velocity.x > 0.0));
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.arrow.state),
        ArrowState::Flying
    );
    view.update(cx, |view, _| {
        for _ in 0..15 {
            view.arena.tick(Controls::default());
        }
    });
    cx.simulate_mouse_down(target, MouseButton::Right, Modifiers::none());
    view.update(cx, |view, _| {
        view.arena.tick(view.controls());
    });
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.arrow.state),
        ArrowState::Returning
    );
    cx.simulate_mouse_up(target, MouseButton::Right, Modifiers::none());
    assert!(!view.read_with(cx, |view, _| view.mouse_recall));
    view.update(cx, |view, cx| {
        view.arena.phase = Phase::Victory;
        cx.notify();
    });
    draw(cx);
    press(cx, "enter");
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.boss.guardian),
        Guardian::Knot
    );
    assert_eq!(
        view.read_with(cx, |view, _| view.arena.phase),
        Phase::Battle
    );

    cx.update(|window, cx| window.blur(cx));
    draw(cx);
    assert!(view.read_with(cx, |view, _| view.paused && view.clock.is_none()));
}

#[gpui::test]
fn battlefield_and_controls_fit_resizes_zoom_and_both_themes(cx: &mut TestAppContext) {
    setup(cx);
    let mut arena = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| ArenaView::new(window, cx));
        arena = Some(view.clone());
        Root::new(view, window, cx)
    });
    let view = arena.unwrap();
    for (width, height, rem, mode) in [
        (360.0, 620.0, 14.0, ThemeMode::Light),
        (900.0, 720.0, 16.0, ThemeMode::Dark),
        (1200.0, 900.0, 24.0, ThemeMode::Light),
    ] {
        cx.simulate_resize(size(px(width), px(height)));
        cx.update(|window, cx| {
            Theme::change(mode, Some(window), cx);
            Theme::global_mut(cx).font_size = px(rem);
            Theme::sync_base(cx);
            view.update(cx, |view, cx| {
                view.arena = Arena::new(Guardian::Prism);
                cx.notify();
            });
        });
        draw(cx);
        let root = cx.debug_bounds("unbound-game").unwrap();
        let header = cx.debug_bounds("unbound-header").unwrap();
        let field = cx.debug_bounds("unbound-battlefield").unwrap();
        let footer = cx.debug_bounds("unbound-footer").unwrap();
        assert!(
            field.size.height > px(100.0),
            "the fight needs a usable viewport at {width} / {rem}"
        );
        assert!(header.bottom() <= field.top() + px(1.0));
        assert!(field.bottom() <= footer.top() + px(1.0));
        assert!(footer.bottom() <= root.bottom() + px(1.0));
        assert!(root.right() <= px(width) + px(1.0));
        assert_eq!(header.left(), footer.left());
        assert_eq!(header.right(), footer.right());
    }
}

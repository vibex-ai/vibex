use super::*;
use combat::ArrowState;
use gpui::{Entity, Keystroke, Modifiers, TestAppContext, VisualTestContext, point, px, size};
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
    show_arena: bool,
}

impl Render for Home {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .when(self.show_arena, |this| this.child(banner(window, cx)))
            .child(Input::new(&self.input))
    }
}

#[gpui::test]
fn banner_opens_a_real_dialog_and_escape_returns_to_the_composer(cx: &mut TestAppContext) {
    setup(cx);
    let mut home = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let input = cx.new(|cx| InputState::new(window, cx));
        let view = cx.new(|_| Home {
            input,
            show_arena: true,
        });
        home = Some(view.clone());
        Root::new(view, window, cx)
    });
    let home = home.unwrap();
    cx.update(|window, _| window.activate_window());
    draw(cx);
    let banner_bounds = cx.debug_bounds("unbound-banner").unwrap();
    cx.simulate_click(banner_bounds.center(), Modifiers::none());
    draw(cx);
    draw(cx);
    assert!(cx.update(|window, cx| window.has_active_dialog(cx)));
    assert!(cx.debug_bounds("unbound-battlefield").is_some());
    press(cx, "escape");
    draw(cx);
    assert!(!cx.update(|window, cx| window.has_active_dialog(cx)));

    // Programmatic opening must preserve the previously focused editor too.
    let opened = cx.update(|window, cx| {
        let input = home.read(cx).input.clone();
        input.update(cx, |input, cx| input.focus(window, cx));
        open(window, cx).unwrap()
    });
    draw(cx);
    draw(cx);
    assert!(cx.update(|window, cx| opened.read(cx).focus.is_focused(window)));
    key(cx, "j", true);
    assert!(opened.read_with(cx, |view, _| view.controls().shoot));
    key(cx, "j", false);
    press(cx, "escape");
    draw(cx);
    assert!(opened.read_with(cx, |view, _| view.closed && view.clock.is_none()));
    cx.simulate_input("jwasd");
    assert_eq!(
        home.read_with(cx, |home, cx| home.input.read(cx).value().to_string()),
        "jwasd"
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

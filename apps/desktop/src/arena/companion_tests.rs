//! The companion's own contract: when the lane exists, that it stays inside the
//! composer column on top of everything the composer stacks there, that a finished turn dives it away, and that the duel it
//! opens covers the session page exactly as the home page covers its own.

use super::*;
use crate::arena::combat::Phase;
use crate::arena::gait::{self, lane_lap};
use crate::arena::init as arena_init;
use gpui::{
    Focusable as _, KeyDownEvent, KeyUpEvent, Keystroke, Modifiers, MouseButton, MouseUpEvent,
    TestAppContext, VisualTestContext, px, size,
};
use gpui_component::{
    Root,
    input::{Input, InputState},
};
use std::f32::consts::{PI, TAU};
use std::time::Duration;

fn draw(cx: &mut VisualTestContext) {
    cx.run_until_parked();
    cx.update(|window, cx| {
        window.draw(cx).clear(cx);
        window.simulate_next_frame(cx);
    });
}
fn setup(cx: &mut TestAppContext) {
    cx.update(|cx| {
        gpui_component::init(cx);
        arena_init(cx);
    });
}
fn double_click(cx: &mut VisualTestContext, id: &'static str) {
    let bounds = cx
        .debug_bounds(id)
        .unwrap_or_else(|| panic!("missing {id}"));
    let position = bounds.center();
    cx.simulate_mouse_down(position, MouseButton::Left, Modifiers::none());
    cx.simulate_event(MouseUpEvent {
        position,
        modifiers: Modifiers::none(),
        button: MouseButton::Left,
        click_count: 2,
    });
    draw(cx);
}
fn press(cx: &mut VisualTestContext, name: &str) {
    let keystroke = Keystroke::parse(name).unwrap();
    cx.simulate_event(KeyDownEvent {
        keystroke: keystroke.clone(),
        is_held: false,
        prefer_character_input: false,
    });
    cx.simulate_event(KeyUpEvent { keystroke });
    draw(cx);
}

/// A session page with the companion lane mounted on top of its composer, above
/// an extension row the way the app stacks its status line and queue, which is
/// all the app's own composer column has to provide.
struct Harness {
    companion: Entity<Companion>,
    input: Entity<InputState>,
    reward: Option<&'static str>,
    best_ms: Option<u64>,
    running: bool,
    reduced: bool,
    awakened: Vec<(&'static str, u64)>,
    _observed: Subscription,
    _awakened: Subscription,
}
impl Harness {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let input = cx.new(|cx| InputState::new(window, cx));
        let companion = cx.new(|_| Companion::new());
        let observed = cx.observe(&companion, |_, _, cx| cx.notify());
        let awakened = cx.subscribe(&companion, |this, _, event: &Awakened, cx| {
            this.awakened.push((event.key, event.clear_ms));
            cx.notify();
        });
        Self {
            companion,
            input,
            reward: Some("claude"),
            best_ms: None,
            running: true,
            reduced: false,
            awakened: Vec::new(),
            _observed: observed,
            _awakened: awakened,
        }
    }
    fn guardian(&self, cx: &App) -> Option<Guardian> {
        self.companion.read(cx).guardian
    }
    fn stage(&self, cx: &App) -> Stage {
        self.companion.read(cx).stage
    }
}
impl Render for Harness {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let lane = lane_with_best(
            &self.companion,
            self.reward,
            self.best_ms,
            self.running,
            Some(self.input.read(cx).focus_handle(cx)),
            self.reduced,
            cx,
        );
        let page = v_flex()
            .id("session-page")
            .debug_selector(|| "session-page".into())
            .size_full()
            .child(
                div()
                    .debug_selector(|| "session-timeline".into())
                    .flex_1()
                    .min_h_0()
                    .child("timeline"),
            )
            .children(lane)
            .child(
                div()
                    .debug_selector(|| "session-extension".into())
                    .h(px(28.0))
                    .flex_none()
                    .child("queue"),
            )
            .child(
                div()
                    .debug_selector(|| "session-composer".into())
                    .h(px(112.0))
                    .flex_none()
                    .child(Input::new(&self.input)),
            );
        session_surface(&self.companion, page, cx)
    }
}
fn open(cx: &mut TestAppContext) -> (Entity<Harness>, &mut VisualTestContext) {
    setup(cx);
    let mut harness = None;
    let (_, cx) = cx.add_window_view(|window, cx| {
        let view = cx.new(|cx| Harness::new(window, cx));
        harness = Some(view.clone());
        Root::new(view, window, cx)
    });
    cx.update(|window, _| window.activate_window());
    cx.update(|_, cx| cx.set_reduce_motion(true));
    cx.simulate_resize(size(px(720.0), px(560.0)));
    draw(cx);
    (harness.unwrap(), cx)
}
fn drive(
    harness: &Entity<Harness>,
    reward: Option<&'static str>,
    running: bool,
    cx: &mut VisualTestContext,
) {
    harness.update(cx, |harness, cx| {
        harness.reward = reward;
        harness.running = running;
        cx.notify();
    });
    draw(cx);
}

/// The duel the lane opens shows the persisted pace to beat, not a blank clock:
/// the session page has UI state, so it hands the record down with the lane.
#[gpui::test]
fn the_duel_shows_the_best_time_the_session_page_hands_down(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    harness.update(cx, |harness, cx| {
        harness.best_ms = Some(64_200);
        cx.notify();
    });
    draw(cx);
    double_click(cx, "composer-companion");
    let battle = harness.read_with(cx, |harness, cx| {
        harness.companion.read(cx).battle.clone().unwrap()
    });
    assert_eq!(battle.read_with(cx, |view, _| view.best_ms), Some(64_200));
}

/// The model is fitted to the lane, and the lane is the composer's strip: a
/// guardian as tall as the home page's would cover the conversation, and one
/// drawn at a fraction of a screen pixel per raster pixel dissolves into noise.
#[test]
fn every_guardian_fits_inside_the_lane_it_rides_at_a_legible_size() {
    for guardian in Guardian::ALL {
        let fitted = fit(guardian);
        let (_, top, _, bottom) = art::companion_box(guardian);
        assert!(
            fitted.lane() <= MAX_LANE,
            "{guardian:?} reserves more than the tallest lane"
        );
        assert!(
            (fitted.rows as f32 - fitted.pixel * (bottom - top) as f32).abs() < 0.001,
            "{guardian:?}'s whole window fits its lane"
        );
        let standing = fitted.pixel * art::companion_height(guardian) as f32;
        assert!(
            (32.0..=MODEL_HEIGHT + 0.5).contains(&standing),
            "{guardian:?} stands {standing}px tall"
        );
        assert!(
            fitted.width() < 280.0,
            "{guardian:?} is wider than the narrowest composer column it rides"
        );
    }
}

/// Every guardian travels the lane on a rhythm of its own rather than one
/// shared sway: its lap differs from every other guardian's, it reaches both
/// ends of the lane, it pauses or turns at its own spots instead of swaying
/// evenly — all but the whale, which swims at one steady speed — and on average
/// it never outpaces a walk.
#[test]
fn every_guardian_crosses_the_lane_on_a_rhythm_of_its_own() {
    const STEP: f32 = 1.0 / 60.0;
    let mut laps = Vec::new();
    for guardian in Guardian::ALL {
        let lap = lane_lap(guardian);
        laps.push((lap, guardian));
        let (mut lowest, mut highest, mut travelled) = (1.0f32, 0.0f32, 0.0f32);
        let mut still = 0.0f32;
        let mut previous = across(&Boss::lane(guardian, 0.0));
        let mut time = STEP;
        while time <= lap {
            let place = across(&Boss::lane(guardian, time));
            let speed = (place - previous).abs() / STEP;
            lowest = lowest.min(place);
            highest = highest.max(place);
            travelled += (place - previous).abs();
            if speed < 0.05 {
                still += STEP;
            }
            previous = place;
            time += STEP;
        }
        assert!(
            lowest < 0.12 && highest > 0.88,
            "{guardian:?} covers {lowest}..{highest} of the lane"
        );
        // On a 720px column this is well under a hundred pixels a second.
        let pace = travelled / lap;
        assert!(pace < 0.12, "{guardian:?} crosses {pace} lanes a second");
        // The whale is the one swimmer: it never stops and keeps one speed,
        // and its rhythm is its own lap and breach.
        assert!(
            guardian == Guardian::DeepSeek || still / lap > 0.15,
            "{guardian:?} spends only {still}s of its {lap}s lap at rest or turning"
        );
    }
    laps.sort_by(|a, b| a.0.total_cmp(&b.0));
    for pair in laps.windows(2) {
        assert!(
            pair[1].0 - pair[0].0 > 0.5,
            "{:?} and {:?} share a lap of about {}s",
            pair[0].1,
            pair[1].1,
            pair[0].0
        );
    }
}

/// The pose clock is monotonic, so every itinerary has to close on itself and
/// move without a jump anywhere, the lap's seam included: the lane never cuts
/// back to a first frame, and nothing leaps from one frame to the next.
#[test]
fn every_lane_itinerary_is_continuous_and_closes_its_lap() {
    const STEP: f32 = 1.0 / 60.0;
    let turn = |a: f32, b: f32| ((a - b + PI).rem_euclid(TAU) - PI).abs();
    for guardian in Guardian::ALL {
        let lap = lane_lap(guardian);
        let mut previous = Boss::lane(guardian, -STEP);
        let mut time = 0.0;
        while time <= lap + STEP {
            let boss = Boss::lane(guardian, time);
            let at = format!("{guardian:?} at {time}s");
            assert!(
                (across(&boss) - across(&previous)).abs() < 0.03,
                "{at} jumps along the lane"
            );
            assert!((boss.height - previous.height).abs() < 2.0, "{at} jumps up");
            // A crouch springs into the air at take-off and a landing lands
            // flat at once, exactly as the home page's hops do.
            assert!((boss.squash - previous.squash).abs() < 0.9, "{at} squashes");
            assert!((boss.lid - previous.lid).abs() < 0.2, "{at} slams its lid");
            assert!(
                (boss.unravel - previous.unravel).abs() < 0.2,
                "{at} unravels"
            );
            for (angle, now, then) in [
                ("yaw", boss.yaw, previous.yaw),
                ("spin", boss.spin, previous.spin),
                ("pitch", boss.pitch, previous.pitch),
                ("bank", boss.bank, previous.bank),
            ] {
                assert!(turn(now, then) < 0.3, "{at} snaps its {angle}");
            }
            for ix in 0..2 {
                assert!(
                    boss.hands[ix].minus(previous.hands[ix]).length() < 2.0,
                    "{at} snaps stem {ix}"
                );
            }
            previous = boss;
            time += STEP;
        }
    }
}

/// Each guardian enters and leaves its own way: an entrance starts wholly out
/// of sight and lands exactly on the lane's first pose, an exit starts from
/// exactly the pose the turn ended on and ends wholly out of sight, and neither
/// jumps from one frame to the next.
#[test]
fn every_guardian_enters_and_leaves_its_own_way_without_a_cut() {
    const STEP: f32 = 1.0 / 60.0;
    let hidden = |shot: &Shot| {
        shot.slide.abs() >= 0.999 || shot.lift.abs() >= 0.999 || shot.opacity <= 0.001
    };
    let jump = |a: &Shot, b: &Shot| {
        (a.slide - b.slide).abs() < 0.1
            && (a.lift - b.lift).abs() < 0.12
            && (a.opacity - b.opacity).abs() < 0.1
            && (across(&a.boss) - across(&b.boss)).abs() < 0.03
            && (a.boss.height - b.boss.height).abs() < 2.0
    };
    let mut ways = Vec::new();
    for guardian in Guardian::ALL {
        let entrance = gait::entrance(guardian);
        let first = gait::shot(guardian, -entrance);
        assert!(
            hidden(&first),
            "{guardian:?} starts its entrance in sight: {first:?}"
        );
        let mut previous = first;
        let mut time = -entrance;
        while time < 0.5 {
            let shot = gait::shot(guardian, time);
            assert!(
                jump(&previous, &shot),
                "{guardian:?} jumps entering at {time}s"
            );
            previous = shot;
            time += STEP;
        }
        let landed = gait::shot(guardian, -1e-4);
        assert!(
            landed.slide.abs() < 0.01 && landed.lift.abs() < 0.01 && landed.opacity > 0.99,
            "{guardian:?} lands off its lane: {landed:?}"
        );
        for from in [-entrance * 0.5, 0.0, 3.3, lane_lap(guardian) * 0.7] {
            let held = gait::shot(guardian, from);
            let mut previous = gait::leaving(guardian, from, from);
            assert!(
                jump(&held, &previous)
                    && (held.boss.squash - previous.boss.squash).abs() < 0.1
                    && (held.boss.lid - previous.boss.lid).abs() < 0.1
                    && (held.boss.pitch - previous.boss.pitch).abs() < 0.1,
                "{guardian:?} cuts away from its pose leaving at {from}s"
            );
            let mut time = from;
            while time <= from + gait::exit(guardian) {
                let shot = gait::leaving(guardian, from, time);
                assert!(
                    jump(&previous, &shot),
                    "{guardian:?} jumps leaving at {time}s"
                );
                previous = shot;
                time += STEP;
            }
            let gone = gait::leaving(guardian, from, from + gait::exit(guardian));
            assert!(
                hidden(&gone),
                "{guardian:?} is still in sight after leaving: {gone:?}"
            );
        }
        ways.push((
            (gait::entrance(guardian) * 100.0) as i32,
            (gait::exit(guardian) * 100.0) as i32,
        ));
    }
    ways.sort_unstable();
    ways.dedup();
    assert_eq!(
        ways.len(),
        Guardian::ALL.len(),
        "two guardians share an entrance and exit"
    );
}

/// The shot's offsets carry the model wholly out of the lane at either end,
/// above it, or below it, on whole pixels.
#[test]
fn a_shot_leaves_the_lane_wholly_at_its_ends() {
    let boss = Boss::lane(Guardian::Codex, 0.0);
    let shot = |slide: f32, lift: f32| Shot {
        boss,
        slide,
        lift,
        opacity: 1.0,
    };
    let (lane, badge, height) = (720.0, 90.0, 60.0);
    let (x, y) = placement(&shot(0.0, 0.0), lane, badge, height);
    assert_eq!((x, y), (lane_x(lane, badge, across(&boss)), 0.0));
    assert_eq!(placement(&shot(1.0, 0.0), lane, badge, height).0, lane);
    assert_eq!(placement(&shot(-1.0, 0.0), lane, badge, height).0, -badge);
    assert_eq!(placement(&shot(0.0, 1.0), lane, badge, height).1, height);
    assert_eq!(placement(&shot(0.0, -1.0), lane, badge, height).1, -height);
    assert_eq!(
        placement(&shot(0.37, 0.41), lane, badge, height).0.fract(),
        0.0
    );
}

/// A turn that starts brings the guardian in from out of sight, and it lands
/// on its lane once its entrance is over; a session switched to mid-turn hands
/// the lane over at once instead.
#[gpui::test]
fn a_starting_turn_brings_the_companion_in_its_own_way(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    drive(&harness, Some("claude"), false, cx);
    cx.background_executor.advance_clock(
        Duration::from_secs_f32(gait::exit(Guardian::Claude)) + Duration::from_millis(60),
    );
    draw(cx);
    draw(cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_none());

    drive(&harness, Some("claude"), true, cx);
    let lane = cx.debug_bounds("composer-companion-lane").unwrap();
    let badge = cx.debug_bounds("composer-companion").unwrap();
    assert!(
        badge.bottom() <= lane.top(),
        "the spark starts above the lane it falls into"
    );
    cx.background_executor
        .advance_clock(Duration::from_secs_f32(gait::entrance(Guardian::Claude)));
    draw(cx);
    let lane = cx.debug_bounds("composer-companion-lane").unwrap();
    let badge = cx.debug_bounds("composer-companion").unwrap();
    assert!(badge.top() >= lane.top() && badge.bottom() <= lane.bottom());

    drive(&harness, Some("codex"), true, cx);
    let lane = cx.debug_bounds("composer-companion-lane").unwrap();
    let badge = cx.debug_bounds("composer-companion").unwrap();
    assert!(
        badge.left() >= lane.left() && badge.right() <= lane.right(),
        "a hand-over is not an entrance"
    );
}

/// The travel never leaves the lane at any width the composer can take, and
/// every place lands on a whole screen pixel.
#[test]
fn the_companion_stays_inside_the_lane_on_whole_pixels() {
    for lane_width in [280.0f32, 720.0, 1600.0] {
        let badge_width = fit(Guardian::Pi).width();
        let span = lane_width - badge_width - INSET * 2.0;
        assert_eq!(lane_x(lane_width, badge_width, 0.0), INSET);
        assert_eq!(lane_x(lane_width, badge_width, 1.0), (INSET + span).round());
        assert_eq!(lane_x(lane_width, badge_width, 0.37).fract(), 0.0);
    }
    // A lane too narrow to travel still keeps the model at its left margin
    // instead of pushing it out of the composer.
    assert_eq!(lane_x(30.0, 40.0, 0.5), INSET);
}

/// Resampling keeps one ink per screen pixel and the figure's silhouette: the
/// coarse frame is neither empty nor bloated past the drawing it came from.
#[test]
fn the_resampled_frame_keeps_the_figure() {
    for guardian in Guardian::ALL {
        let fitted = fit(guardian);
        let boss = Boss::lane(guardian, 3.5);
        let fine = art::companion(&boss, 3.5, false);
        let coarse = fine.resample(fitted.columns, fitted.rows);
        assert_eq!((coarse.width, coarse.height), (fitted.columns, fitted.rows));
        let share = |frame: &crate::arena::raster::Raster| {
            frame.pixels.iter().filter(|ink| **ink != 0).count() as f32 / frame.pixels.len() as f32
        };
        let (fine, coarse) = (share(&fine), share(&coarse));
        assert!(
            coarse > fine * 0.6 && coarse < fine * 1.4,
            "{guardian:?} covers {coarse} of its cells, against {fine} of its drawing"
        );
    }
}

#[gpui::test]
fn the_lane_belongs_to_an_unlocked_guardian_of_a_running_session(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    assert!(cx.debug_bounds("composer-companion").is_some());
    assert_eq!(
        harness.read_with(cx, |harness, cx| harness.guardian(cx)),
        Some(Guardian::Claude)
    );

    // A session that is not working never mounts one: the companion is a sign
    // of work in flight, not a decoration of the input box.
    drive(&harness, Some("codex"), false, cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_none());

    // Neither does an Agent whose guardian has never been beaten.
    drive(&harness, None, true, cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_none());

    drive(&harness, Some("deepseek-harness"), true, cx);
    assert_eq!(
        harness.read_with(cx, |harness, cx| harness.guardian(cx)),
        Some(Guardian::DeepSeek),
        "an Agent alias resolves to the same guardian"
    );
    assert!(cx.debug_bounds("composer-companion").is_some());
}

/// Reduced motion turns the effect off entirely.
#[gpui::test]
fn reduced_motion_never_mounts_the_companion(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_some());
    harness.update(cx, |harness, cx| {
        harness.reduced = true;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("composer-companion").is_none());
    cx.background_executor.advance_clock(
        Duration::from_secs_f32(gait::exit(Guardian::Claude)) + Duration::from_millis(60),
    );
    draw(cx);
    draw(cx);
    assert_eq!(
        harness.read_with(cx, |harness, cx| harness.stage(cx)),
        Stage::Idle,
        "and leaves nothing roaming behind the setting"
    );
    harness.update(cx, |harness, cx| {
        harness.reduced = false;
        cx.notify();
    });
    draw(cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_some());
}

#[gpui::test]
fn the_companion_rides_on_top_of_every_composer_row(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    let lane = cx.debug_bounds("composer-companion-lane").unwrap();
    let page = cx.debug_bounds("session-page").unwrap();
    let badge = cx.debug_bounds("composer-companion").unwrap();
    let extension = cx.debug_bounds("session-extension").unwrap();
    let composer = cx.debug_bounds("session-composer").unwrap();
    assert_eq!(lane.size.height, px(fit(Guardian::Claude).lane()));
    assert!(badge.top() >= lane.top() && badge.bottom() <= lane.bottom());
    assert!(badge.left() >= lane.left() && badge.right() <= lane.right());
    assert_eq!(
        lane.bottom(),
        extension.top(),
        "the lane sits above the rows stacked on the input box, not under them"
    );
    assert_eq!(extension.bottom(), composer.top());
    assert_eq!(composer.bottom(), page.bottom());
    assert!(
        badge.size.height < composer.size.height,
        "the companion is smaller than the input box it dives into"
    );
    let _ = harness;
}

#[gpui::test]
fn a_finished_turn_dives_the_companion_into_the_composer_and_retires_it(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    let roaming = cx.debug_bounds("composer-companion-lane").unwrap();
    drive(&harness, Some("claude"), false, cx);
    // The dive is a transition, not a cut: the lane is still there while the
    // model falls out of it.
    assert!(cx.debug_bounds("composer-companion-lane").is_some());
    let stage = harness.read_with(cx, |harness, cx| harness.stage(cx));
    assert!(
        matches!(stage, Stage::Leaving { .. }),
        "the companion is diving, not gone: {stage:?}"
    );
    cx.background_executor.advance_clock(
        Duration::from_secs_f32(gait::exit(Guardian::Claude)) + Duration::from_millis(60),
    );
    draw(cx);
    draw(cx);
    assert!(cx.debug_bounds("composer-companion-lane").is_none());
    assert_eq!(
        cx.debug_bounds("session-extension").unwrap().top(),
        roaming.bottom()
    );
}

#[gpui::test]
fn double_clicking_the_companion_opens_its_duel_over_the_session(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    let page = cx.debug_bounds("session-page").unwrap();
    double_click(cx, "composer-companion");
    let duel = cx.debug_bounds("composer-companion-duel").unwrap();
    assert_eq!(duel, page, "the duel covers the session page");
    assert_eq!(
        cx.debug_bounds("unbound-game"),
        cx.debug_bounds("composer-companion-duel")
    );
    assert!(
        cx.debug_bounds("session-timeline").is_none(),
        "the session's own elements are gone while the duel is up"
    );
    let battle = harness.read_with(cx, |harness, cx| {
        harness.companion.read(cx).battle.clone().unwrap()
    });
    assert_eq!(
        battle.read_with(cx, |view, _| view.arena.boss.guardian),
        Guardian::Claude
    );
    press(cx, "escape");
    assert!(cx.debug_bounds("composer-companion-duel").is_none());
    assert!(cx.debug_bounds("session-timeline").is_some());
}

/// A replay ends the same way the first win did: the unlock is reported again,
/// which is what keeps the reward permanent when the state is re-persisted.
#[gpui::test]
fn a_won_duel_reports_the_unlock_it_was_reached_through(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    double_click(cx, "composer-companion");
    let battle = harness.read_with(cx, |harness, cx| {
        harness.companion.read(cx).battle.clone().unwrap()
    });
    battle.update(cx, |view, cx| {
        view.clock = None;
        view.arena.run_time = 42.5;
        view.arena.phase = Phase::Victory;
        view.arena.phase_time = 3.2;
        cx.notify();
    });
    for _ in 0..24 {
        battle.update(cx, |view, cx| view.step(view.controls(), cx));
        draw(cx);
    }
    assert!(battle.read_with(cx, |view, _| view.closed));
    assert_eq!(
        harness.read_with(cx, |harness, _| harness.awakened.clone()),
        vec![("claude", 42_500)]
    );
    assert!(cx.debug_bounds("composer-companion-duel").is_none());
}

/// The companion belongs to the session whose composer it rides. Switching
/// sessions hands the lane to that session's own guardian, and a duel opened
/// over the previous one ends with it instead of following the reader.
#[gpui::test]
fn a_duel_ends_when_the_composer_moves_to_another_agent(cx: &mut TestAppContext) {
    let (harness, cx) = open(cx);
    double_click(cx, "composer-companion");
    assert!(cx.debug_bounds("composer-companion-duel").is_some());
    drive(&harness, Some("codex"), true, cx);
    draw(cx);
    assert!(
        harness.read_with(cx, |harness, cx| harness
            .companion
            .read(cx)
            .battle
            .is_none()),
        "the duel stays with the session it was fought over"
    );
    assert!(cx.debug_bounds("composer-companion-duel").is_none());
    assert_eq!(
        harness.read_with(cx, |harness, cx| harness.guardian(cx)),
        Some(Guardian::Codex)
    );
    assert!(cx.debug_bounds("composer-companion").is_some());
    assert!(cx.debug_bounds("session-timeline").is_some());
}

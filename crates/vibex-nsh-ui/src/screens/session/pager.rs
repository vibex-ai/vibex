//! The three pages of the session workspace, and the gesture between them.
//!
//! The conversation is the page in the middle. The sessions page comes in
//! from the left and the workbench from the right, each as wide as the screen:
//! the tree on the left is three levels deep, and a row needs the whole width
//! to show its level and still have room for a title. The conversation slides
//! a little with whichever page is coming and dims as far as that page has
//! come, so the three read as one strip at two depths.
//!
//! A horizontal drag anywhere on the workspace moves the pages natively: the
//! platform writes the finger's travel into one animated value and the pages'
//! transforms are derived from it, so following the finger costs the app
//! nothing per frame. The app only hears where the drag started and where it
//! was let go, and decides which page the release settles on.

use std::{cell::Cell, rc::Rc};

use nsh_core::{element::Timing, prelude::*};

use crate::{motion, theme};

/// The page the workspace rests on.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub enum ShellPage {
    /// The conversation.
    #[default]
    Home,
    /// The sessions tree, on the left.
    Sessions,
    /// Files, changes and the terminal, on the right.
    Workbench,
}

impl ShellPage {
    /// Where the drag value stands, in pages, with this page fully in.
    const fn open_offset(self) -> f32 {
        match self {
            ShellPage::Home => 0.0,
            ShellPage::Sessions => 1.0,
            ShellPage::Workbench => -1.0,
        }
    }
}

/// The workspace's pages: which one it rests on, and the values that move
/// them.
#[derive(Clone, Copy)]
pub struct Pager {
    /// The finger's travel in points: one screen width with the sessions page
    /// fully in, minus one with the workbench.
    drag: AnimatedValue,
    /// The screen's width, as an animated value the transforms scale by.
    width: AnimatedValue,
    /// Which side page a gesture may reveal: a drag that started toward one
    /// never pulls the other out when it crosses back over the middle.
    gate_sessions: AnimatedValue,
    gate_workbench: AnimatedValue,
    /// The page the workspace rests on, or is settling on.
    page: Signal<ShellPage>,
    /// Whether each side page is laid out: while it is open, dragged or
    /// settling.
    sessions_shown: Signal<bool>,
    workbench_shown: Signal<bool>,
    /// Whether the workbench has been opened at all: its surfaces are made on
    /// the first visit.
    workbench_made: Signal<bool>,
    /// Counts settles, so a finished animation only tidies up after itself.
    run: Stored<u64>,
    drag_page: Stored<Option<ShellPage>>,
    on_pre_back: Stored<Option<Rc<dyn Fn() -> bool>>>,
    on_back: Stored<Option<Rc<dyn Fn() -> bool>>>,
    on_sessions_back: Stored<Option<Rc<dyn Fn() -> bool>>>,
}

thread_local! {
    /// The mounted workspace's pager, for the platform's back key and gesture.
    static CURRENT: Cell<Option<Pager>> = const { Cell::new(None) };
}

/// The pager of the workspace on screen, if one is mounted.
pub fn current() -> Option<Pager> {
    CURRENT.with(Cell::get)
}

impl Pager {
    /// Makes the pager under the current owner and registers it as the one on
    /// screen.
    pub fn new() -> Self {
        let width = untracked(|| window().width.0).max(1.0);
        let pager = Self {
            drag: animated_value(0.0),
            width: animated_value(width),
            gate_sessions: animated_value(0.0),
            gate_workbench: animated_value(0.0),
            page: signal(ShellPage::Home),
            sessions_shown: signal(false),
            workbench_shown: signal(false),
            workbench_made: signal(false),
            run: stored(0),
            drag_page: stored(None),
            on_pre_back: stored(None),
            on_back: stored(None),
            on_sessions_back: stored(None),
        };
        CURRENT.with(|cell| cell.set(Some(pager)));
        cleanup(|| CURRENT.with(|cell| cell.set(None)));
        // A rotation or a window change moves a resting side page with the
        // screen it fills.
        effect(move || {
            let width = window().width.0.max(1.0);
            pager.width.set(width);
            let page = pager.page.get_untracked();
            if page != ShellPage::Home {
                pager.drag.set(pager.rest(page));
            }
        });
        pager
    }

    /// The page the workspace rests on (reactive).
    pub fn page(self) -> ShellPage {
        self.page.get()
    }

    /// The same, without subscribing.
    pub fn page_untracked(self) -> ShellPage {
        self.page.get_untracked()
    }

    pub(super) fn show_initial(self, page: ShellPage) {
        self.arm(page);
        self.page.set(page);
        self.drag.set(self.rest(page));
    }

    /// Slides `page` in, or back to the conversation for [`ShellPage::Home`].
    pub fn open(self, page: ShellPage) {
        self.settle(page);
    }

    /// Returns to the conversation.
    pub fn close(self) {
        self.settle(ShellPage::Home);
    }

    pub(super) fn on_back(self, on_back: impl Fn() -> bool + 'static) {
        self.on_back.replace(Some(Rc::new(on_back)));
    }

    pub(super) fn on_pre_back(self, on_back: impl Fn() -> bool + 'static) {
        self.on_pre_back.replace(Some(Rc::new(on_back)));
    }

    pub(super) fn on_sessions_back(self, on_back: impl Fn() -> bool + 'static) {
        self.on_sessions_back.replace(Some(Rc::new(on_back)));
    }

    pub(super) fn back(self) -> bool {
        if self.on_pre_back.get().is_some_and(|on_back| on_back()) {
            return true;
        }
        let handler = if self.page_untracked() == ShellPage::Sessions {
            self.on_sessions_back
        } else {
            self.on_back
        };
        if !handler.get().is_some_and(|on_back| on_back())
            && self.page_untracked() != ShellPage::Home
        {
            self.close();
        }
        true
    }

    fn width_now(self) -> f32 {
        untracked(|| window().width.0).max(1.0)
    }

    /// Where the drag value stands with `page` at rest.
    fn rest(self, page: ShellPage) -> f32 {
        page.open_offset() * self.width_now()
    }

    /// Lets one side page answer the drag value, and lays it out.
    fn arm(self, page: ShellPage) {
        match page {
            ShellPage::Sessions => {
                self.gate_sessions.set(1.0);
                self.gate_workbench.set(0.0);
                self.sessions_shown.set(true);
                self.workbench_shown.set(false);
            }
            ShellPage::Workbench => {
                self.gate_sessions.set(0.0);
                self.gate_workbench.set(1.0);
                self.workbench_made.set(true);
                self.workbench_shown.set(true);
                self.sessions_shown.set(false);
            }
            ShellPage::Home => {}
        }
    }

    /// Animates to `target` and tidies up when the page lands.
    fn settle(self, target: ShellPage) {
        let run = self.run.get().wrapping_add(1);
        self.run.replace(run);
        self.drag_page.replace(None);
        dismiss_keyboard();
        if target != ShellPage::Home {
            self.arm(target);
        }
        if self.page.get_untracked() != target {
            self.page.set(target);
        }
        let to = self.rest(target);
        let opening = target != ShellPage::Home;
        let landed = move || {
            if self.run.get() != run {
                return;
            }
            // Only the page the workspace rests on stays laid out.
            if target != ShellPage::Sessions {
                self.sessions_shown.set(false);
            }
            if target != ShellPage::Workbench {
                self.workbench_shown.set(false);
            }
        };
        if motion::reduce_motion() {
            self.drag.set(to);
            landed();
            return;
        }
        // Opening decelerates onto the page; closing is quicker, because the
        // reader has already decided.
        let timing = if opening {
            Timing::new(
                theme::DRAWER_OPEN_ANIMATION_MS,
                Easing::Curve(Curve::Poly(5.0), EaseMode::Out),
            )
        } else {
            Timing::new(theme::DRAWER_CLOSE_ANIMATION_MS, Easing::EaseInOut)
        };
        let end = self.drag.timing_with(to, timing);
        spawn(async move {
            if end.await {
                landed();
            }
        });
    }

    /// A drag began: decide which side page it may reveal.
    fn pan_started(self, event: &PanEvent) {
        // The finger takes the pages from any animation that was moving them.
        self.run.replace(self.run.get().wrapping_add(1));
        dismiss_keyboard();
        let from = self.page_untracked();
        let delta = if event.translation.x.0 != 0.0 {
            event.translation.x.0
        } else {
            event.velocity_x
        };
        let page = match from {
            ShellPage::Home if delta > 0.0 => Some(ShellPage::Sessions),
            ShellPage::Home if delta < 0.0 => Some(ShellPage::Workbench),
            ShellPage::Home => None,
            page => Some(page),
        };
        self.drag_page.replace(page);
        if let Some(page) = page {
            self.arm(page);
        } else {
            self.gate_sessions.set(1.0);
            self.gate_workbench.set(1.0);
            self.sessions_shown.set(true);
            self.workbench_made.set(true);
            self.workbench_shown.set(true);
        }
    }

    /// The drag was let go: settle on the page the release asks for.
    fn pan_ended(self, event: &PanEvent) {
        let from = self.page.get_untracked();
        let value = event
            .value_x
            .unwrap_or(self.rest(from) + event.translation.x.0);
        let moved = self.drag_page.replace(None).unwrap_or({
            if value >= 0.0 {
                ShellPage::Sessions
            } else {
                ShellPage::Workbench
            }
        });
        let direction = moved.open_offset();
        let reveal = (value / self.width_now() * direction).clamp(0.0, 1.0);
        let target = if event.cancelled {
            from
        } else if release_opens(
            reveal,
            event.velocity_x * direction,
            event.velocity_y,
            from == moved,
        ) {
            moved
        } else {
            ShellPage::Home
        };
        if target != from && target != ShellPage::Home {
            motion::tick();
        }
        self.settle(target);
    }
}

impl Default for Pager {
    fn default() -> Self {
        Self::new()
    }
}

/// Whether a release leaves the dragged side page open.
///
/// `reveal` is how much of the page shows, `toward` the release speed toward
/// the page (negative: away from it), `across` the speed along the other
/// axis, and `was_open` whether the drag started with the page fully in.
///
/// A flick decides on its own; a slow release decides by travel, with a
/// forgiving threshold on the side the drag started from so a short pull is
/// enough to commit and a wobble at the end is not enough to undo it.
fn release_opens(reveal: f32, toward: f32, across: f32, was_open: bool) -> bool {
    // A frame of travel at the release, as the platform's velocity reports it.
    const COMMIT_SPEED: f32 = 120.0;
    const REVERSE_SPEED: f32 = 1680.0;
    let flick = toward.abs() >= theme::DRAWER_SWIPE_VELOCITY
        && toward.abs() > across.abs() * theme::DRAWER_SWIPE_STRAIGHTNESS;
    let flick_toward = flick && toward > 0.0;
    let flick_away = flick && toward < 0.0;
    if was_open {
        if flick_away || toward < -COMMIT_SPEED {
            false
        } else if toward > REVERSE_SPEED {
            true
        } else {
            reveal > 1.0 - theme::DRAWER_SNAP_TRAVEL_RATIO
        }
    } else if flick_toward || toward > COMMIT_SPEED {
        true
    } else if flick_away || toward < -REVERSE_SPEED {
        false
    } else {
        reveal >= theme::DRAWER_SNAP_TRAVEL_RATIO
    }
}

/// The workspace: `home` in the middle, `sessions` and `workbench` beside it.
///
/// The side pages are built by the caller and stay mounted once made, so a
/// page keeps its scroll position and its fields between visits; a page that
/// is not on screen takes no layout and no touches.
pub fn pager(
    pager: Pager,
    home: impl IntoElement,
    sessions: impl IntoElement,
    workbench: impl Fn() -> AnyElement + 'static,
) -> impl IntoElement {
    let unit = pager.drag.div(&pager.width);
    let reveal_sessions = unit
        .interpolate(&[0.0, 1.0], &[0.0, 1.0])
        .mul(&pager.gate_sessions);
    let reveal_workbench = unit
        .interpolate(&[-1.0, 0.0], &[1.0, 0.0])
        .mul(&pager.gate_workbench);
    let sessions_x = reveal_sessions.sub(1.0).mul(&pager.width);
    let workbench_x = pager.width.sub(&reveal_workbench.mul(&pager.width));
    // The conversation gives way to the page that is coming, a part of the
    // distance that page covers.
    let home_x = reveal_sessions
        .sub(&reveal_workbench)
        .mul(&pager.width)
        .mul(theme::PAGE_PARALLAX);
    let dim = reveal_sessions
        .add(&reveal_workbench)
        .mul(theme::DRAWER_BACKDROP_OPACITY);

    view()
        .flex_1()
        .relative()
        .overflow_hidden()
        .bg(theme::block_page)
        .pan_values(Some(&pager.drag), None)
        .pan_enabled(true)
        .on_pan_start(move |event: &PanEvent| pager.pan_started(event))
        .on_pan_end(move |event: &PanEvent| pager.pan_ended(event))
        .child(
            view()
                .flex_1()
                .min_h(px(0.0))
                .accessibility_elements_hidden(move || pager.page() != ShellPage::Home)
                .bind_translate_x(&home_x)
                .child(home),
        )
        .child(
            view()
                .absolute()
                .inset(px(0.0))
                .bg(nsh_core::color::black())
                .bind_opacity(&dim)
                .pointer_events(PointerEvents::None),
        )
        .child(
            side_page(move || pager.sessions_shown.get())
                .bg(theme::block_page)
                .bind_translate_x(&sessions_x)
                .child(sessions),
        )
        .child(
            side_page(move || pager.workbench_shown.get())
                .bg(theme::block_page)
                .bind_translate_x(&workbench_x)
                .child(when(move || pager.workbench_made.get(), workbench)),
        )
}

/// The frame of a side page: as large as the workspace, and out of the layout
/// while it is not shown.
fn side_page(shown: impl Fn() -> bool + 'static) -> View {
    view()
        .absolute()
        .top(px(0.0))
        .bottom(px(0.0))
        .left(px(0.0))
        .w_full()
        .flex_col()
        // The page takes the touches on it: a press on an empty stretch must
        // not reach the conversation underneath.
        .on_press(|| {})
        .style_when(move || !shown(), |style| style.hidden())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_slow_release_settles_by_travel() {
        assert!(!release_opens(0.10, 0.0, 0.0, false));
        assert!(release_opens(0.15, 0.0, 0.0, false));
        assert!(release_opens(0.90, 0.0, 0.0, true));
        assert!(!release_opens(0.85, 0.0, 0.0, true));
    }

    #[test]
    fn a_flick_commits_from_a_short_drag() {
        assert!(release_opens(0.03, 900.0, 0.0, false));
        assert!(!release_opens(0.97, -900.0, 0.0, true));
    }

    #[test]
    fn release_jitter_does_not_undo_a_drag() {
        // Still moving toward the page: it opens, however short the pull.
        assert!(release_opens(0.05, 200.0, 0.0, false));
        // A small backward wobble after a long pull keeps the page.
        assert!(release_opens(0.60, -300.0, 0.0, false));
        // A decisive reversal gives it back.
        assert!(!release_opens(0.60, -2000.0, 0.0, false));
    }

    #[test]
    fn a_diagonal_fling_is_not_a_swipe() {
        // Fast, but mostly along the other axis: travel decides.
        assert!(!release_opens(0.05, 100.0, 900.0, false));
    }
}

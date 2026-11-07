//! The sheets the phone pulls up from the foot of the screen.
//!
//! Every bottom sheet on the native client is the same object seen from a
//! different page: a scrim over what the reader was doing, a panel rising from
//! the bottom edge with a grabber on top, and a finger that moves it. This
//! module owns that object once — the rise, the fall, the scrim's own fade, and
//! the two pulls a finger can make — so a sheet's own code is only what it has
//! to say.
//!
//! A sheet has two heights and two gestures. It rests at the height its page
//! allows a sheet, which is a share of the screen rather than the whole of it;
//! a finger that drags up anywhere on the panel pulls it open to the taller
//! height it is allowed — nearer the top of the screen, and still short of it,
//! because the page has to stay recognisable behind the scrim. A finger that
//! drags down gives that height back first and only then moves the panel off
//! the screen, so a sheet is never dismissed by a reader who was closing it.
//! Both heights are bounded by the body inside the panel: a list that outgrows
//! the sheet scrolls once the sheet has no more height to give.
//!
//! Only a gesture that is actually vertical is either. A panel can hold a list
//! that scrolls sideways — the runtime sheet's Agent strip is one — and a
//! finger moving along it drifts down a little on the way; a sheet that read
//! that drift as a pull would leave from under a reader who was only moving a
//! logo. So the two translations a pan carries are weighed against each other,
//! and the ones that go across stay with the list that owns them.
//!
//! A page keeps its sheet's own flag as the source of truth for what the sheet
//! shows. Closing a sheet does not clear that flag: the flag stays set for as
//! long as the panel is on screen, the fall included, and the page clears it
//! when [`BottomSheet::land`] says the fall has landed. That is what lets a
//! sheet travel away with its own content still in it, instead of emptying out
//! and leaving a blank card to slide down.
//!
//! Each sheet carries its own motion, so a page can stack one sheet over
//! another — the drawer's row menu under its folder's archive window — without
//! the two fighting over one animation.

use std::time::{Duration, Instant};

use gpui::{
    Animation, AnimationExt as _, AnyElement, App, ElementId, IntoElement, MouseButton,
    MouseUpEvent, Pixels, ScrollDelta, ScrollEvent, ScrollHandle, Styled as _, TouchPhase, Window,
    div, prelude::*, px,
};

use crate::platform::WindowInsetsExt as _;
use crate::theme;

/// Which sheet a rise or fall belongs to.
///
/// The key is what tells one panel from another: a page may have more than one
/// sheet on screen, and each of them owns its own motion.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetKey {
    /// The pairing fallbacks: finding a runtime, or typing a code.
    PairingFallback,
    /// The sidebar row's own commands.
    SidebarRowMenu,
    /// The sidebar's own commands, which belong to no row.
    SidebarRootMenu,
    /// The archive window a folder files its sessions on.
    FolderArchiveMenu,
    /// When a waiting send should leave.
    SidebarRescheduleMenu,
    /// The guard in front of deleting the selected sessions.
    BatchDelete,
    /// The authority's directories, so a worktree path can be chosen.
    WorktreePaths,
    /// The members of a session group.
    GroupMembers,
    /// The guard in front of removing a project.
    ProjectRemove,
    /// The objective a goal control will issue.
    GoalObjective,
    /// One runtime's own actions.
    HostActions,
    /// The prompts already sent in this session.
    ComposerHistory,
    /// What an attach control is about to attach.
    ComposerAttach,
    /// A choice list the new-session page opens: the project the session
    /// belongs to, the place it is worked, or the branch a worktree is cut from.
    NewSessionPicker,
    /// The Agent, provider and model a session runs on.
    RuntimeOptions,
    /// One group of the Agent's own options — reasoning, mode, or the model's
    /// session options — in the composer's own drawer.
    RuntimeOption,
    /// The palette picker.
    SettingsPalette,
    /// Go to a line in the file on screen.
    FileGoto,
    /// The branch a history filter is scoped to.
    GitBranches,
    /// The author or date a history filter is scoped to.
    GitFilter,
    /// The guard in front of merging or discarding a worktree.
    WorktreeConfirmation,
    /// The guard in front of rolling changes back.
    GitRevert,
    /// The patch for one file, read over the tree it belongs to.
    GitDiff,
    /// The question an Agent parked on: an approval, or a form it asked the
    /// reader to fill in. The panel covers the composer while the reader
    /// answers, and one asks for one request at a time.
    AgentQuestion,
}

/// How tall a sheet may be: where it rests, and how far it may be pulled open.
#[derive(Clone, Copy, PartialEq, Debug)]
pub struct SheetSize {
    /// The height a sheet keeps when the reader has not pulled it open. A sheet
    /// never takes the whole page: whatever it explains stays in sight behind
    /// the scrim, so it reads as a thing laid over the page.
    pub resting: f32,
    /// The height an upward pull may take it to, short of the whole screen.
    pub expanded: f32,
}

impl SheetSize {
    /// How much taller an upward pull may make the sheet.
    fn extra(&self) -> f32 {
        (self.expanded - self.resting).max(0.0)
    }
}

/// The two heights a sheet works between, read from the window.
///
/// The resting height is the viewport's, because a sheet that fits the screen
/// is what the reader sees. The expanded height is the page's — the viewport
/// without the platform's own bars — so a sheet pulled wide open still leaves
/// the page's edges in sight on a phone with tall insets.
pub fn sheet_size(window: &Window) -> SheetSize {
    let viewport = f32::from(window.viewport_size().height);
    let insets = window.safe_insets().effective();
    let page = (viewport - f32::from(insets.top) - f32::from(insets.bottom)).max(1.0);
    let resting = viewport * theme::SHEET_HEIGHT_RATIO;
    let expanded = page * theme::SHEET_EXPANDED_HEIGHT_RATIO;
    SheetSize {
        resting,
        expanded: expanded.max(resting),
    }
}

/// How tall a sheet rests before its body scrolls instead.
pub fn max_height(window: &Window) -> Pixels {
    px(sheet_size(window).resting)
}

/// One page's bottom sheets: every panel on screen, its rise or fall, and
/// whatever the finger is doing to it.
#[derive(Default)]
pub struct BottomSheet {
    panels: Vec<Panel>,
}

/// One sheet on screen.
struct Panel {
    key: SheetKey,
    /// The rise or fall carrying the panel, once one has started. A landed
    /// motion stays as the record of where the panel rests.
    motion: Option<Motion>,
    /// Whether the page has asked the sheet to close.
    closing: bool,
    /// How tall this sheet may be, read from the window on the last frame.
    size: SheetSize,
    /// The handle the sheet's own body scrolls with, once it has one. The pan
    /// reads it: a finger that drags down inside a body which still has
    /// somewhere to scroll belongs to that body, not to the sheet.
    body_scroll: Option<ScrollHandle>,
    /// The finger pulling the panel down, while it is pulling.
    pull: Option<Pull>,
    /// When the last pull let go, so the lift that ended it is not read as a
    /// tap on the scrim it landed on.
    released_at: Option<Instant>,
    /// The extra height the panel was resting at when the last pull let go, and
    /// the height it settled on, so a release that lands where it started is
    /// not read as a move.
    released_height: Option<(f32, f32)>,
    /// The identity the next motion takes, so a new one restarts the animation.
    next_id: u64,
}

impl Panel {
    /// A panel that has just been opened: it comes up from the foot of the
    /// screen at its resting height, wherever the last sheet left it.
    fn opening(key: SheetKey, size: SheetSize) -> Self {
        let mut panel = Self {
            key,
            motion: None,
            closing: false,
            size,
            body_scroll: None,
            pull: None,
            released_at: None,
            released_height: None,
            next_id: 0,
        };
        panel.start(
            Placement {
                offset: size.resting,
                ..Placement::REST
            },
            Placement::REST,
            theme::SHEET_RISE_MS,
            Easing::Rise,
            false,
        );
        panel
    }

    /// Where the panel is now: under a finger, that is where the finger put it;
    /// otherwise it is wherever the motion carrying it has got to.
    fn placement_now(&self) -> Placement {
        match self.claimed_pull() {
            Some(pull) => Placement {
                offset: pull.offset,
                extent: pull.opened,
            },
            None => self.motion.map_or(Placement::REST, |motion| {
                motion.placement_at(motion.elapsed())
            }),
        }
    }

    /// How tall the panel may be now: its resting height plus whatever the
    /// finger or a motion has pulled it open by.
    fn travel_now(&self) -> f32 {
        self.size.resting + self.placement_now().extent
    }

    /// The height the panel would have if the reader pulled it open this far.
    fn travel_at(&self, extent: f32) -> f32 {
        self.size.resting + extent
    }

    /// The finger's pull, if it has claimed the panel.
    fn claimed_pull(&self) -> Option<&Pull> {
        self.pull.as_ref().filter(|pull| pull.claimed)
    }

    /// How much of the motion on screen is left to run.
    fn remaining(&self) -> Duration {
        self.motion
            .map_or(Duration::ZERO, |motion| motion.remaining())
    }

    /// Starts a motion from wherever the panel is now.
    fn start(
        &mut self,
        from: Placement,
        to: Placement,
        base_ms: u64,
        easing: Easing,
        falling: bool,
    ) {
        self.next_id = self.next_id.wrapping_add(1);
        self.motion = Some(Motion {
            id: self.next_id,
            from,
            to,
            duration: flight(base_ms),
            easing,
            started: Instant::now(),
            falling,
        });
    }

    fn snapshot(&self) -> SheetPresentation {
        let placement = self.placement_now();
        let offset = placement.offset;
        SheetPresentation {
            offset,
            // A panel under a finger is placed by the finger, so the motion is
            // held at that offset rather than left to run. It is held rather
            // than dropped because the animation element carries the identity
            // of everything inside the panel: unmounting it mid-drag would
            // reset whatever the panel happens to be holding — a field's
            // cursor, a list's place.
            motion: match self.claimed_pull() {
                Some(_) => self.motion.map(|motion| motion.held_at(placement)),
                None => self.motion,
            },
            travel: self.travel_now(),
            resting: self.size.resting,
        }
    }
}

/// Where a panel is.
///
/// A sheet has two things a finger can do to it, and they are separate axes: a
/// pull downward moves the panel off the screen, and a pull upward makes it
/// taller. Both are held in one placement so a motion carries them together.
#[derive(Clone, Copy, Debug, PartialEq)]
struct Placement {
    /// Pixels the panel sits below its resting place.
    offset: f32,
    /// Pixels of extra height the panel has been pulled open by.
    extent: f32,
}

impl Placement {
    /// A panel at its resting height, in its resting place.
    const REST: Self = Self {
        offset: 0.0,
        extent: 0.0,
    };

    /// Part way from one placement to another, along the animation's curve.
    fn between(from: Self, to: Self, delta: f32) -> Self {
        Self {
            offset: from.offset + (to.offset - from.offset) * delta,
            extent: from.extent + (to.extent - from.extent) * delta,
        }
    }
}

/// What the shell draws for one frame of a sheet.
#[derive(Clone, Copy)]
pub struct SheetPresentation {
    /// Where the panel is below its resting place: zero at rest, the whole
    /// travel once it has cleared the screen.
    offset: f32,
    /// The motion carrying the panel, when one is running.
    motion: Option<Motion>,
    /// How tall the panel may be this frame: where it rests, plus however much
    /// of its extra height it has been pulled open by.
    travel: f32,
    /// The height it rests at, which is what an open panel's extra height is
    /// measured from while a motion carries both.
    resting: f32,
}

impl SheetPresentation {
    /// The scrim's own strength: it fades with the panel it is behind, so a
    /// pull that is half way down is half way out of the page as well.
    fn scrim_opacity(&self) -> f32 {
        theme::SHEET_BACKDROP_OPACITY * reveal_of(self.offset, self.travel)
    }
}

/// The scrim's strength along a motion's own curve, as a value the animation
/// element can carry.
fn scrim_curve(motion: Option<Motion>, offset: f32, travel: f32) -> impl Fn(f32) -> f32 {
    move |delta| {
        let offset = match motion {
            Some(motion) => motion.placement_at_delta(delta).offset,
            None => offset,
        };
        theme::SHEET_BACKDROP_OPACITY * reveal_of(offset, travel)
    }
}

/// What a page tells the sheet about the pan the platform reported.
///
/// A pan is reported in both axes, not only the one the sheet works in: a
/// finger dragging a horizontal list inside the panel is making the same kind
/// of event, and only the translation it has accumulated in each direction
/// says which of the two the gesture belongs to.
#[derive(Clone, Copy, PartialEq, Debug)]
pub enum SheetPan {
    /// A finger arrived, having already moved this far.
    Started { dx: f32, dy: f32 },
    /// A finger moved by this many pixels since the last report.
    Moved { dx: f32, dy: f32 },
    /// A finger let go.
    Ended,
    /// The platform took the pan away — a call, a system gesture.
    Cancelled,
}

/// What the sheet did with a pan, which is what the page has to act on.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum SheetPull {
    /// The pan is not the sheet's: the lists under the finger keep it.
    Ignored,
    /// The panel is following the finger, so the pan must not reach anything
    /// else — the list inside the sheet would otherwise scroll with it.
    Pulling,
    /// The finger let go short of a dismissal: the panel is on its way back up.
    Restored,
    /// The finger let go far enough, or fast enough, that the sheet is leaving.
    Dismissed(SheetKey),
}

/// Reads one platform pan event.
///
/// Mobile platforms report a finger drag as a scroll wheel carrying a touch
/// phase; a mouse wheel carries none, and never pulls a sheet.
pub fn sheet_pan(event: &ScrollEvent) -> Option<SheetPan> {
    let movement = |delta: ScrollDelta| match delta {
        ScrollDelta::Pixels(delta) => Some((f32::from(delta.x), f32::from(delta.y))),
        ScrollDelta::Lines(_) => None,
    };
    match event.touch_phase {
        TouchPhase::Started => {
            let (dx, dy) = movement(event.delta)?;
            Some(SheetPan::Started { dx, dy })
        }
        TouchPhase::Moved => {
            let (dx, dy) = movement(event.delta)?;
            Some(SheetPan::Moved { dx, dy })
        }
        TouchPhase::Ended => Some(SheetPan::Ended),
        TouchPhase::Cancelled => Some(SheetPan::Cancelled),
    }
}

impl BottomSheet {
    fn index_of(&self, key: SheetKey) -> Option<usize> {
        self.panels.iter().position(|panel| panel.key == key)
    }

    fn panel(&self, key: SheetKey) -> Option<&Panel> {
        self.panels.iter().find(|panel| panel.key == key)
    }

    fn panel_mut(&mut self, key: SheetKey) -> Option<&mut Panel> {
        self.panels.iter_mut().find(|panel| panel.key == key)
    }

    /// Whether this sheet is on its way out: the page has closed it, and the
    /// panel is still clearing the screen.
    pub fn is_leaving(&self, key: SheetKey) -> bool {
        self.panel(key).is_some_and(|panel| panel.closing)
    }

    /// Whether the pull that just ended changed the sheet's height.
    ///
    /// Asked right after [`BottomSheet::pull`] reported `Restored`. The height
    /// settles at one of two ends, and only a release that lands on the end it
    /// was not already at is a move: giving back a height, or taking one that
    /// was already taken, is the sheet going where it already was.
    pub fn settled_at_a_new_height(&self, key: SheetKey) -> bool {
        self.panel(key)
            .and_then(|panel| panel.released_height)
            .is_some_and(|(from, to)| (from - to).abs() >= 0.5)
    }

    /// Whether any panel is on screen for this page.
    ///
    /// A sheet is modal: its scrim covers the page, and the page's own gestures
    /// are watched from ancestors the scrim cannot occlude, so a page that
    /// would move under it has to ask. A panel on its way out still counts —
    /// until it has cleared the screen it is the thing the reader is looking
    /// at, and the page behind it must not answer the same finger.
    pub fn is_up(&self) -> bool {
        !self.panels.is_empty()
    }

    /// Drops every panel at once, without a fall.
    ///
    /// A page that clears its own flag when a panel lands cannot land a panel
    /// whose page has already been left — a pairing fallback that was still
    /// open when the workspace replaced the pairing screen, for instance. That
    /// panel would keep [`Self::is_up`] true forever, and every page swipe on
    /// the new screen would stand down for a sheet that is not drawn anywhere.
    /// A page crossing a boundary that retires its sheets calls this instead,
    /// so the next first frame has nothing modal in front of it.
    pub fn clear(&mut self) {
        self.panels.clear();
    }

    /// Whether a pull of this sheet let go recently enough that the lift it
    /// ended must not be read as a tap on the scrim it landed on.
    pub fn pull_just_released(&self, key: SheetKey) -> bool {
        self.panel(key).is_some_and(|panel| {
            panel.released_at.is_some_and(|at| {
                at.elapsed() < Duration::from_millis(theme::SHEET_PULL_TAP_GUARD_MS)
            })
        })
    }

    /// Announces that the sheet is closing, and reports how long the panel
    /// takes to clear the screen.
    ///
    /// The page keeps its own flag set until [`BottomSheet::land`] says the
    /// fall has landed: that is what keeps the sheet's content in the panel
    /// while it slides away.
    pub fn lower(&mut self, key: SheetKey) -> Duration {
        let Some(panel) = self.panel_mut(key) else {
            // Nothing of this sheet is on screen, so there is nothing to
            // animate away and the page can clear its flag at once.
            return Duration::ZERO;
        };
        if panel.closing {
            // Already on its way out: a second request is answered with the
            // rest of the fall it is in.
            return panel.remaining();
        }
        panel.closing = true;
        panel.pull = None;
        let placement = panel.placement_now();
        panel.start(
            placement,
            Placement {
                offset: panel.travel_at(placement.extent),
                extent: placement.extent,
            },
            theme::SHEET_FALL_MS,
            Easing::Fall,
            true,
        );
        panel.remaining()
    }

    /// Clears the sheet once its fall has landed, and reports whether it did.
    ///
    /// The second landing — a page that closed the same sheet twice, or a
    /// timer that fires after the flag was already dropped — has nothing to
    /// clear and answers `false`, so the page's own state is touched once.
    pub fn land(&mut self, key: SheetKey) -> bool {
        let Some(index) = self.index_of(key) else {
            return false;
        };
        if !self.panels[index].closing {
            return false;
        }
        self.panels.remove(index);
        true
    }

    /// What the shell should draw for `key` this frame, or nothing at all when
    /// the sheet is not on screen.
    ///
    /// `up` is the page's own flag. A flag that is cleared without a
    /// [`BottomSheet::lower`] — a page reset that drops everything at once —
    /// has no fall to draw, so the panel leaves on this frame.
    pub fn presentation(
        &mut self,
        key: SheetKey,
        up: bool,
        size: SheetSize,
    ) -> Option<SheetPresentation> {
        let Some(index) = self.index_of(key) else {
            if !up {
                return None;
            }
            // The first frame of a sheet the page has opened: it comes up
            // from the foot of the screen.
            self.panels.push(Panel::opening(key, size));
            return self.panels.last().map(Panel::snapshot);
        };
        let dropped = {
            let panel = &mut self.panels[index];
            panel.size = size;
            // A page that drops its flag without closing the sheet has no fall
            // to draw, so the panel leaves on this frame.
            !up && !panel.closing
        };
        if dropped {
            self.panels.remove(index);
            return None;
        }
        Some(self.panels[index].snapshot())
    }

    /// The handle a sheet's own body scrolls with.
    ///
    /// The sheet tracks its body with it, and the pan reads it: a finger that
    /// drags down inside a body which still has somewhere to scroll belongs to
    /// that body, and only a body with nowhere left to go hands the pan to the
    /// sheet. A sheet whose body cannot scroll never asks for one, and one that
    /// is not on screen gets a handle nothing reads.
    pub fn body_scroll(&mut self, key: SheetKey) -> ScrollHandle {
        match self.panel_mut(key) {
            Some(panel) => panel
                .body_scroll
                .get_or_insert_with(ScrollHandle::new)
                .clone(),
            None => ScrollHandle::new(),
        }
    }

    /// Feeds one sheet one platform pan.
    pub fn pull(&mut self, key: SheetKey, pan: SheetPan, size: SheetSize) -> SheetPull {
        // Every sheet on screen watches the same page, so only the one on top
        // answers: a pan that reached two of them would move two panels at once.
        if self.panels.last().is_some_and(|panel| panel.key != key) {
            return SheetPull::Ignored;
        }
        let Some(index) = self.index_of(key) else {
            return SheetPull::Ignored;
        };
        let panel = &mut self.panels[index];
        panel.size = size;
        if panel.closing {
            // A sheet that is already leaving is not caught again.
            return SheetPull::Ignored;
        }
        match pan {
            SheetPan::Started { dx, dy } => {
                let opened = panel.placement_now().extent;
                panel.pull = Some(Pull::started(Instant::now(), opened));
                panel.advance(dx, dy)
            }
            SheetPan::Moved { dx, dy } => panel.advance(dx, dy),
            SheetPan::Ended => panel.release(),
            SheetPan::Cancelled => panel.cancelled(),
        }
    }
}

#[cfg(test)]
impl Panel {
    /// Runs the panel's motion to its end, the way a frame some milliseconds
    /// later would have seen it.
    fn settle(&mut self) {
        if let Some(motion) = &mut self.motion {
            motion.from = motion.to;
            motion.started = Instant::now();
        }
    }
}

#[cfg(test)]
impl BottomSheet {
    /// Lets one sheet's motion finish before the test reads it.
    fn settle(&mut self, key: SheetKey) {
        if let Some(panel) = self.panel_mut(key) {
            panel.settle();
        }
    }

    /// Puts every panel at the end of its flight.
    ///
    /// A test's clock is not the clock the animation element reads, so a test
    /// that needs to see a settled panel says so instead of waiting.
    pub fn settle_flights(&mut self) {
        for panel in &mut self.panels {
            panel.settle();
        }
    }
}

impl Panel {
    /// Whether the body under the finger has nowhere left to scroll up, which
    /// is what lets the pan become the sheet's.
    fn body_at_top(&self) -> bool {
        self.body_scroll
            .as_ref()
            .is_none_or(|handle| handle.offset().y >= px(0.0))
    }

    /// Advances the pull by one translation, and reports who owns the pan now.
    ///
    /// A finger on its way down belongs to the body under it while that body
    /// still has somewhere to scroll, and to the sheet once it does not. A
    /// finger on its way up is the reader pulling the sheet open — from
    /// anywhere on the panel, which is what makes the gesture worth having —
    /// and only becomes the body's once there is no height left to take.
    ///
    /// Distance in the sheet's own axis is only half the question. The panel
    /// holds lists that scroll the other way — the Agent strip above the model
    /// list is one — and a finger dragging one of those makes the same pan: it
    /// travels a little way down as it goes across, and a pull that took that
    /// drift for a dismissal would drop the sheet out from under a reader who
    /// was only moving a logo. So the two translations are compared as they
    /// accumulate, and a gesture that has come further across than down stays
    /// with whatever the finger is actually on.
    fn advance(&mut self, dx: f32, dy: f32) -> SheetPull {
        let at_top = self.body_at_top();
        let extra = self.size.extra();
        let Some(pull) = self.pull.as_mut() else {
            return SheetPull::Ignored;
        };
        pull.record(dx, dy);
        if !pull.claimed {
            let opening = pull.travel < 0.0;
            let moves_the_panel = if opening {
                extra > 0.0 && pull.extent < extra - 0.5
            } else {
                at_top
            };
            let vertical =
                pull.travel.abs() >= pull.across.abs() * theme::SHEET_PULL_VERTICAL_RATIO;
            if !moves_the_panel || !vertical || pull.travel.abs() < theme::SHEET_PULL_THRESHOLD {
                return SheetPull::Ignored;
            }
            pull.claimed = true;
        }
        self.spend_pull();
        SheetPull::Pulling
    }

    /// Spends the gesture's translation on the sheet's two axes, in the order
    /// the reader feels them: a finger on its way down gives the extra height
    /// back before it starts moving the panel off the screen, and one on its
    /// way up takes the height before anything else.
    fn spend_pull(&mut self) {
        let extra = self.size.extra();
        let Some(pull) = self.pull.as_ref() else {
            return;
        };
        let mut remaining = pull.travel;
        let mut opened = pull.extent;
        let mut offset = 0.0;
        if remaining > 0.0 {
            let given_back = remaining.min(opened);
            opened -= given_back;
            remaining -= given_back;
            // What is left moves the panel, and the panel travels its own
            // height: a sheet pulled open has further to go before it is gone.
            offset = remaining.min(self.travel_at(opened));
        } else if remaining < 0.0 {
            opened = (opened - remaining).min(extra);
        }
        if let Some(pull) = self.pull.as_mut() {
            pull.opened = opened;
            pull.offset = offset;
        }
    }

    /// Lets the panel go where the finger left it.
    ///
    /// The height settles at one of its two ends — a reader who pulled the
    /// sheet more than half way open gets the whole of it, and one who gave
    /// back more than half of it gets its resting height — while the panel
    /// itself either leaves or comes back up.
    fn release(&mut self) -> SheetPull {
        self.released_at = Some(Instant::now());
        let Some(pull) = self.pull.take() else {
            return SheetPull::Ignored;
        };
        if !pull.claimed {
            return SheetPull::Ignored;
        }
        let extra = self.size.extra();
        let open = pull_opens(pull.opened, extra, pull.velocity);
        let from = Placement {
            offset: pull.offset,
            extent: pull.opened,
        };
        if pull_dismisses(pull.offset, pull.velocity) {
            self.closing = true;
            self.start(
                from,
                Placement {
                    offset: self.travel_at(pull.opened),
                    extent: pull.opened,
                },
                theme::SHEET_FALL_MS,
                Easing::Fall,
                true,
            );
            SheetPull::Dismissed(self.key)
        } else {
            self.start(
                from,
                Placement {
                    offset: 0.0,
                    extent: if open { extra } else { 0.0 },
                },
                theme::SHEET_RISE_MS,
                Easing::Rise,
                false,
            );
            // Where it came from and where it landed, recorded for the caller
            // that only wants to announce a real move.
            self.released_height = Some((pull.extent, if open { extra } else { 0.0 }));
            SheetPull::Restored
        }
    }

    /// Puts the panel back when the platform takes the gesture away.
    ///
    /// The reader did not finish the pull, so the sheet neither leaves on it
    /// nor keeps the height it had taken: it goes back to where the gesture
    /// found it.
    fn cancelled(&mut self) -> SheetPull {
        self.released_at = Some(Instant::now());
        self.released_height = None;
        let Some(pull) = self.pull.take() else {
            return SheetPull::Ignored;
        };
        if !pull.claimed {
            return SheetPull::Ignored;
        }
        self.start(
            Placement {
                offset: pull.offset,
                extent: pull.opened,
            },
            Placement {
                offset: 0.0,
                extent: pull.extent,
            },
            theme::SHEET_RISE_MS,
            Easing::Rise,
            false,
        );
        SheetPull::Restored
    }
}

/// Whether a released pull takes the sheet with it.
///
/// A pull that came far enough, or that was still moving fast enough when it
/// let go, dismisses the sheet; anything else goes back up. The distance is a
/// fixed one rather than a share of the sheet, because what the reader judges
/// it against is the rows that slid up, not the panel as a whole.
fn pull_dismisses(offset: f32, velocity: f32) -> bool {
    offset >= theme::SHEET_PULL_DISMISS_TRAVEL
        || (offset > 0.0 && velocity >= theme::SHEET_PULL_DISMISS_VELOCITY)
}

/// Whether a released pull leaves the sheet at its full height.
///
/// A pull that came past half way of the height it could take keeps it, and so
/// does one that was still moving upward when it let go; anything else goes back
/// to the height the sheet rests at, which is what keeps a nudge from leaving a
/// half-open panel behind.
fn pull_opens(opened: f32, extra: f32, velocity: f32) -> bool {
    if velocity <= -theme::SHEET_EXPAND_VELOCITY {
        return true;
    }
    if velocity >= theme::SHEET_EXPAND_VELOCITY {
        return false;
    }
    extra > 0.0 && opened * 2.0 >= extra
}

/// How long a held motion's animation is given: long enough that the element
/// never divides by zero, short enough that no curve is ever read from it.
const HELD_FLIGHT: Duration = Duration::from_millis(1);

/// The shortest gap between two pan reports that can say anything about how
/// fast the finger is moving.
const VELOCITY_SAMPLE_FLOOR: Duration = Duration::from_millis(4);

/// How long a sheet's motion runs, never zero: the animation element divides
/// by the duration, so a preference that asks for no motion is served by a
/// flight short enough to read as one frame.
fn flight(base_ms: u64) -> Duration {
    theme::motion_ms(base_ms).max(Duration::from_millis(1))
}

/// The share of a panel that is up, from its own offset.
fn reveal_of(offset: f32, travel: f32) -> f32 {
    if travel <= 0.0 {
        return 1.0;
    }
    (1.0 - offset / travel).clamp(0.0, 1.0)
}

/// A rise or a fall, and where it is.
#[derive(Clone, Copy, Debug)]
struct Motion {
    /// The identity the animation element keys on: a fresh one restarts it.
    id: u64,
    from: Placement,
    to: Placement,
    duration: Duration,
    easing: Easing,
    started: Instant,
    /// Whether this is the panel leaving.
    falling: bool,
}

impl Motion {
    fn elapsed(&self) -> Duration {
        self.started.elapsed()
    }

    /// How much of the flight is left, with a frame of slack so the last frame
    /// is painted before the page clears the sheet.
    fn remaining(&self) -> Duration {
        self.duration
            .saturating_sub(self.elapsed())
            .saturating_add(Duration::from_millis(16))
    }

    /// Where the panel sits when the animation element reports this much of the
    /// curve. The element hands over an eased value, so the interpolation here
    /// is the straight line between the two ends.
    fn placement_at_delta(&self, delta: f32) -> Placement {
        Placement::between(self.from, self.to, delta)
    }

    /// Where the panel sits now, read from the clock rather than from the
    /// element: a motion that is interrupted has to know where it got to.
    fn placement_at(&self, elapsed: Duration) -> Placement {
        if self.duration.is_zero() {
            return self.to;
        }
        let delta = (elapsed.as_secs_f32() / self.duration.as_secs_f32()).clamp(0.0, 1.0);
        self.placement_at_delta((self.easing.curve())(delta))
    }

    /// The same motion, held where it is: it renders that placement and no
    /// other.
    fn held_at(mut self, placement: Placement) -> Self {
        self.from = placement;
        self.to = placement;
        self.duration = HELD_FLIGHT;
        self.started = Instant::now();
        self
    }

    fn animation(&self) -> Animation {
        Animation::new(self.duration).with_easing(self.easing.curve())
    }

    fn element_id(&self, stem: &str) -> ElementId {
        ElementId::NamedInteger(stem.into(), self.id)
    }
}

/// How a sheet travels.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Easing {
    /// A panel coming up: it covers most of its travel at once and settles, so
    /// the rise reads as the sheet arriving rather than sliding.
    Rise,
    /// A panel going away: it leaves slowly and accelerates off the screen.
    Fall,
}

impl Easing {
    /// The curve, as a plain function: the shell hands it to the animation
    /// element, and the sheet reads the same curve to know where an interrupted
    /// panel got to. Both halves have to be the same shape for the two to
    /// agree — hence `ease_out_quint` written out, since it is needed as a
    /// value here and only as a closure in the toolkit.
    fn curve(self) -> fn(f32) -> f32 {
        match self {
            Easing::Rise => |delta| 1.0 - (1.0 - delta).powi(5),
            Easing::Fall => |delta| delta * delta * delta,
        }
    }
}

/// The finger's own pull, from the moment it starts to the moment it lets go.
#[derive(Clone, Copy, Debug)]
struct Pull {
    /// The gesture's own translation so far, positive downward.
    travel: f32,
    /// The gesture's translation so far across the panel, positive rightward.
    ///
    /// The sheet never spends it — it is what tells a pull on the panel from a
    /// drag along a list inside it.
    across: f32,
    /// How far open the panel was when the gesture began.
    extent: f32,
    /// Where the gesture has put the panel below its resting place.
    offset: f32,
    /// How far open the gesture has pulled the panel.
    opened: f32,
    /// The finger's speed, in pixels per second, as a moving average.
    velocity: f32,
    /// How many samples the average is built from.
    samples: u32,
    /// When the last sample arrived.
    sampled_at: Instant,
    /// Whether the panel is following this finger. Until it is, the pan belongs
    /// to whatever the finger is actually on.
    claimed: bool,
}

impl Pull {
    fn started(now: Instant, extent: f32) -> Self {
        Self {
            travel: 0.0,
            across: 0.0,
            extent,
            offset: 0.0,
            opened: extent,
            velocity: 0.0,
            samples: 0,
            sampled_at: now,
            claimed: false,
        }
    }

    /// Blends one more translation into the pull.
    ///
    /// Each platform report carries the translation since the last one, so the
    /// speed is folded in with a fixed blend rather than accumulated from raw
    /// distances: a fast flick right before the finger lifts still dominates
    /// the estimate, which is what tells a flick from a drag.
    fn record(&mut self, dx: f32, dy: f32) {
        let now = Instant::now();
        let elapsed = now.duration_since(self.sampled_at);
        self.sampled_at = now;
        self.travel += dy;
        self.across += dx;
        if elapsed < VELOCITY_SAMPLE_FLOOR {
            // Reports that arrive back to back describe coalesced input: the
            // gap between them is the platform's own jitter, and dividing by it
            // would report a speed no finger has.
            return;
        }
        let seconds = elapsed.as_secs_f32();
        let instant = dy / seconds;
        let blend = if self.samples == 0 { 1.0 } else { 0.55 };
        self.velocity += (instant - self.velocity) * blend;
        self.samples = self.samples.saturating_add(1);
    }
}

/// What a tap on a sheet's scrim does.
type DismissListener = Box<dyn Fn(&MouseUpEvent, &mut Window, &mut App)>;

/// What the platform's pan stream does while a finger is on a sheet's panel.
type PanListener = Box<dyn Fn(&ScrollEvent, &mut Window, &mut App)>;

/// The shell every bottom sheet is drawn in: the scrim, the panel, and the
/// finger that pulls it.
pub struct BottomSheetShell {
    /// The element id stem, and the name a test finds the panel by.
    id: String,
    presentation: SheetPresentation,
    /// What a tap on the scrim does.
    dismiss: Option<DismissListener>,
    /// What the platform's pan stream does while the finger is on the panel.
    pan: Option<PanListener>,
    /// The sheet's own content, under the grabber.
    content: Option<AnyElement>,
}

/// Builds the shell for one frame of a sheet.
pub fn bottom_sheet_shell(
    id: impl Into<String>,
    presentation: SheetPresentation,
) -> BottomSheetShell {
    BottomSheetShell {
        id: id.into(),
        presentation,
        dismiss: None,
        pan: None,
        content: None,
    }
}

impl BottomSheetShell {
    /// What a tap outside the panel does.
    pub fn on_dismiss(
        mut self,
        dismiss: impl Fn(&MouseUpEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.dismiss = Some(Box::new(dismiss));
        self
    }

    /// What the platform's pan stream does while the finger is on the panel.
    ///
    /// The listener is installed in the capture phase, ahead of the lists
    /// inside the sheet: a pull the sheet claims has to reach it before the
    /// body under the finger has scrolled with the same gesture.
    pub fn on_pan(mut self, pan: impl Fn(&ScrollEvent, &mut Window, &mut App) + 'static) -> Self {
        self.pan = Some(Box::new(pan));
        self
    }

    /// The sheet's own content: its header and its body, under the grabber.
    pub fn content(mut self, content: impl IntoElement) -> Self {
        self.content = Some(content.into_any_element());
        self
    }

    /// Puts the shell together.
    fn compose(mut self) -> AnyElement {
        let presentation = self.presentation;
        let travel = presentation.travel;
        let offset = presentation.offset;
        let motion = presentation.motion;
        let content = self.content.take();

        // The panel's own name is the sheet's, so the bounds a test measures
        // are the whole surface the reader sees — grabber, header and all.
        let panel = div()
            .debug_selector({
                let selector = self.id.clone();
                move || selector.clone()
            })
            .w_full()
            .max_h(px(travel))
            .flex()
            .flex_col()
            .rounded_t(px(theme::RADIUS_SHEET))
            .border_t_1()
            .border_color(theme::border_default())
            .bg(theme::bg_card())
            .overflow_hidden()
            // The panel owns its own taps: only the scrim around it dismisses
            // what it is showing.
            .on_mouse_up(MouseButton::Left, |_, _, cx| cx.stop_propagation())
            .child(sheet_grabber())
            // The content takes what is left of the panel and no more, so a
            // long list inside it scrolls instead of growing the panel past the
            // height a sheet is allowed.
            .child(div().flex_1().min_h_0().flex().flex_col().children(content));

        // The panel's own height is animated apart from its place, because a
        // panel that is being pulled open grows while it stays where it is.
        let panel: AnyElement = match motion {
            Some(motion) => {
                let resting = presentation.resting;
                panel
                    .with_animation(
                        motion.element_id(&format!("{}-size", self.id)),
                        motion.animation(),
                        move |element, delta| {
                            element.max_h(px(resting + motion.placement_at_delta(delta).extent))
                        },
                    )
                    .into_any_element()
            }
            None => panel.into_any_element(),
        };

        let positioner = div()
            .absolute()
            .left(px(0.0))
            .right(px(0.0))
            .bottom(px(-offset));
        let positioner: AnyElement = match motion {
            Some(motion) => positioner
                .with_animation(
                    motion.element_id(&format!("{}-panel", self.id)),
                    motion.animation(),
                    move |element, delta| {
                        element.bottom(px(-motion.placement_at_delta(delta).offset))
                    },
                )
                .child(panel)
                .into_any_element(),
            None => positioner
                .bottom(px(-offset))
                .child(panel)
                .into_any_element(),
        };
        let mut scrim = div()
            .id(ElementId::Name(format!("{}-scrim", self.id).into()))
            .absolute()
            .inset_0()
            .occlude()
            .flex()
            .flex_col()
            .justify_end();
        if let Some(dismiss) = self.dismiss.take() {
            scrim = scrim.on_mouse_up(MouseButton::Left, dismiss);
        }
        let shell: AnyElement = match motion {
            Some(motion) => {
                let opacity = scrim_curve(Some(motion), offset, travel);
                scrim
                    .with_animation(
                        motion.element_id(&format!("{}-scrim", self.id)),
                        motion.animation(),
                        move |element, delta| element.bg(theme::backdrop(opacity(delta))),
                    )
                    .child(positioner)
                    .into_any_element()
            }
            None => scrim
                .bg(theme::backdrop(presentation.scrim_opacity()))
                .child(positioner)
                .into_any_element(),
        };

        // The pull is watched from the scrim rather than from the panel. The
        // panel is the thing the finger is dragging away from its own contact
        // point, so a listener that travelled with it slid out from under the
        // finger after a few pixels — the higher up the panel the finger
        // landed, the sooner the drag died mid-gesture. The scrim covers the
        // page and never moves.
        match self.pan.take() {
            Some(pan) => crate::scroll_capture::capture_scroll_wheel(
                ElementId::Name(format!("{}-pull", self.id).into()),
                shell,
                pan,
            )
            .into_any_element(),
            None => shell,
        }
    }
}

impl IntoElement for BottomSheetShell {
    type Element = AnyElement;

    fn into_element(self) -> Self::Element {
        self.compose()
    }
}

/// The bar at the top of every sheet.
///
/// It is a control, not a border: it is the one mark that says the panel can be
/// pulled back down, so it is drawn in the muted foreground rather than the
/// panel's own edge, which is a tenth of a black and disappears against the
/// surface it is drawn on.
fn sheet_grabber() -> gpui::Div {
    div()
        .flex_shrink_0()
        .w_full()
        .pt(px(theme::SPACING_SM))
        .pb(px(theme::SPACING_XS))
        .flex()
        .justify_center()
        .child(
            div()
                .w(px(theme::SHEET_GRABBER_WIDTH))
                .h(px(theme::SHEET_GRABBER_HEIGHT))
                .rounded_full()
                .bg(theme::sheet_grabber()),
        )
}

#[cfg(test)]
mod tests {
    use super::*;
    use gpui::point;

    /// The shell is the one object every bottom sheet wears, so what it owes a
    /// sheet is checked here rather than once per page: the surface, the scrim
    /// under it, the grabber on top, and the two gestures.
    #[test]
    fn the_shell_keeps_the_surface_every_sheet_wears() {
        let source = include_str!("bottom_sheet.rs");
        let (_, shell) = source
            .split_once("    fn compose(mut self) -> AnyElement {")
            .expect("the shell's own composition should remain inspectable");
        for shared in [
            ".absolute()",
            ".occlude()",
            ".justify_end()",
            "max_h(px(travel))",
            "RADIUS_SHEET",
            "sheet_grabber()",
            "capture_scroll_wheel(",
            "on_mouse_up(MouseButton::Left, dismiss)",
        ] {
            assert!(shell.contains(shared), "the shell should keep {shared}");
        }
    }

    /// The height a sheet rests at in these tests, and the height it may be
    /// pulled open to: the sheet the phone shows, without a window.
    const TRAVEL: f32 = 600.0;
    const EXPANDED: f32 = 850.0;
    const SIZE: SheetSize = SheetSize {
        resting: TRAVEL,
        expanded: EXPANDED,
    };
    /// How much taller the sheet may be pulled.
    const EXTRA: f32 = EXPANDED - TRAVEL;

    fn up(sheet: &mut BottomSheet, key: SheetKey) -> SheetPresentation {
        sheet
            .presentation(key, true, SIZE)
            .expect("the sheet should be on screen")
    }

    /// Two offsets are the same place within a pixel.
    fn assert_place(actual: f32, expected: f32, context: &str) {
        assert!(
            (actual - expected).abs() < 1.0,
            "{context}: {actual} is not {expected}"
        );
    }

    /// Where a panel's flight starts and where it ends.
    ///
    /// A test that reads the live offset is reading how much wall-clock time
    /// passed since the motion began, which is how busy the machine is; the
    /// two ends of the curve are what the flight actually promises.
    fn flight_ends(presentation: &SheetPresentation) -> (f32, f32) {
        match presentation.motion {
            Some(motion) => (
                motion.placement_at(Duration::ZERO).offset,
                motion.placement_at(motion.duration).offset,
            ),
            None => (presentation.offset, presentation.offset),
        }
    }

    /// Where a panel's flight leaves its height, and where it leaves the panel.
    fn flight_heights(presentation: &SheetPresentation) -> (f32, f32) {
        match presentation.motion {
            Some(motion) => (
                presentation.resting + motion.placement_at(Duration::ZERO).extent,
                presentation.resting + motion.placement_at(motion.duration).extent,
            ),
            None => (presentation.travel, presentation.travel),
        }
    }

    /// Pulls a settled sheet open, the way a reader does.
    fn open(sheet: &mut BottomSheet, key: SheetKey) {
        assert_eq!(
            sheet.pull(
                key,
                SheetPan::Started {
                    dx: 0.0,
                    dy: -EXTRA
                },
                SIZE
            ),
            SheetPull::Pulling
        );
        sheet.pull(key, SheetPan::Ended, SIZE);
        sheet.settle(key);
        let opened = up(sheet, key);
        assert_place(opened.travel, EXPANDED, "the sheet should be open");
    }

    /// A sheet that is up, with its rise already run: where the reader sees it
    /// once the panel has arrived.
    fn settled(sheet: &mut BottomSheet, key: SheetKey) -> SheetPresentation {
        // Opening it first is what puts the panel on screen; settling is what
        // lets its rise finish.
        up(sheet, key);
        sheet.settle(key);
        up(sheet, key)
    }

    /// A sheet is only drawn while its page says it is up, and a page that
    /// clears the flag without closing the sheet takes it off screen at once.
    #[test]
    fn a_sheet_is_only_drawn_while_its_page_says_it_is_up() {
        let mut sheet = BottomSheet::default();
        assert!(
            sheet
                .presentation(SheetKey::PairingFallback, false, SIZE)
                .is_none(),
            "a sheet nobody opened is not drawn"
        );

        let (start, end) = flight_ends(&up(&mut sheet, SheetKey::PairingFallback));
        assert_place(start, TRAVEL, "a sheet that is opened rises from the foot");
        assert_place(end, 0.0, "and settles where it rests");
        assert_place(
            settled(&mut sheet, SheetKey::PairingFallback).offset,
            0.0,
            "the sheet settles where it rests",
        );
        assert!(
            sheet
                .presentation(SheetKey::GitBranches, false, SIZE)
                .is_none(),
            "another page's sheet is not this one's to draw"
        );
        assert!(
            sheet
                .presentation(SheetKey::PairingFallback, false, SIZE)
                .is_none(),
            "a reset that drops the flag leaves no fall behind it"
        );
    }

    /// A closing sheet keeps its flag set through the fall, and lands where the
    /// page can clear it.
    #[test]
    fn a_closing_sheet_lands_only_when_its_fall_is_done() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::SidebarRowMenu);
        let flight = sheet.lower(SheetKey::SidebarRowMenu);
        assert!(flight > Duration::ZERO, "the fall has to take some time");

        // Still drawn while it falls, even though the page is closing it.
        let falling = up(&mut sheet, SheetKey::SidebarRowMenu);
        assert!(falling.offset > 0.0, "the panel is on its way down");
        assert!(sheet.is_leaving(SheetKey::SidebarRowMenu));
        assert!(!sheet.land(SheetKey::PairingFallback));

        assert!(sheet.land(SheetKey::SidebarRowMenu));
        assert!(
            sheet
                .presentation(SheetKey::SidebarRowMenu, false, SIZE)
                .is_none(),
            "a landed sheet leaves the screen"
        );
    }

    /// Two sheets of one page carry their own motion, so the one underneath
    /// keeps its place while the one on top rises over it.
    #[test]
    fn a_stacked_sheet_rises_without_moving_the_one_under_it() {
        let mut sheet = BottomSheet::default();
        let resting = settled(&mut sheet, SheetKey::SidebarRowMenu);
        assert_place(resting.offset, 0.0, "the first sheet has settled");

        let (start, _) = flight_ends(&up(&mut sheet, SheetKey::FolderArchiveMenu));
        assert_place(start, TRAVEL, "the sheet on top rises from the foot");
        assert_place(
            settled(&mut sheet, SheetKey::SidebarRowMenu).offset,
            0.0,
            "the sheet underneath is where it was",
        );

        // Closing the one on top leaves the one underneath alone.
        sheet.lower(SheetKey::FolderArchiveMenu);
        sheet.land(SheetKey::FolderArchiveMenu);
        assert_place(
            settled(&mut sheet, SheetKey::SidebarRowMenu).offset,
            0.0,
            "the sheet underneath never moved",
        );
    }

    /// The scrim follows the panel: a pull that is half way down has taken half
    /// the dimming with it.
    #[test]
    fn the_scrim_fades_with_the_panel_it_is_behind() {
        let mut sheet = BottomSheet::default();
        let opened = settled(&mut sheet, SheetKey::GitFilter);
        assert!(
            (opened.scrim_opacity() - theme::SHEET_BACKDROP_OPACITY).abs() < 0.01,
            "a sheet at rest dims the page by its own scrim"
        );

        let half = SheetPresentation {
            offset: TRAVEL / 2.0,
            motion: None,
            travel: TRAVEL,
            resting: TRAVEL,
        };
        assert!(half.scrim_opacity() > 0.0);
        assert!(half.scrim_opacity() < theme::SHEET_BACKDROP_OPACITY);
        assert_eq!(
            reveal_of(TRAVEL, TRAVEL),
            0.0,
            "a panel that has cleared the screen leaves no scrim"
        );
    }

    /// A drag that has not come far enough is still a tap, and a drag inside a
    /// body that can scroll belongs to that body.
    #[test]
    fn a_pull_is_only_claimed_once_the_finger_means_it() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::GitBranches);
        let body = sheet.body_scroll(SheetKey::GitBranches);

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dx: 0.0, dy: 2.0 },
                SIZE
            ),
            SheetPull::Ignored,
            "a tap that wobbles is a tap"
        );

        // A body with room left to scroll keeps the pan for itself.
        body.set_offset(point(px(0.0), px(-240.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started {
                    dx: 0.0,
                    dy: theme::SHEET_PULL_THRESHOLD
                },
                SIZE
            ),
            SheetPull::Ignored,
            "a body that can still scroll up owns the pan"
        );

        // The same finger on a body that is already at its top pulls the sheet.
        body.set_offset(point(px(0.0), px(0.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved {
                    dx: 0.0,
                    dy: theme::SHEET_PULL_THRESHOLD
                },
                SIZE
            ),
            SheetPull::Pulling
        );
        // Once the panel is following the finger it keeps it, even if the body
        // would have had somewhere to scroll.
        body.set_offset(point(px(0.0), px(-240.0)));
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved { dx: 0.0, dy: 10.0 },
                SIZE
            ),
            SheetPull::Pulling
        );
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, SIZE),
            SheetPull::Restored
        );
    }

    /// A gesture that is going across the panel belongs to whatever is under
    /// the finger, even when it drifts a long way down on the way.
    ///
    /// This is the Agent strip in the runtime sheet: a finger swiping it back
    /// to the first Agent also travels down, and a sheet that read that drift
    /// as a pull left the reader without their settings.
    #[test]
    fn a_sideways_gesture_stays_with_the_list_under_the_finger() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::RuntimeOptions);
        let body = sheet.body_scroll(SheetKey::RuntimeOptions);
        body.set_offset(point(px(0.0), px(0.0)));

        // The finger crosses further than it descends, so the pan is the
        // strip's — even once the drift is past the distance that would pull
        // the panel on its own.
        assert_eq!(
            sheet.pull(
                SheetKey::RuntimeOptions,
                SheetPan::Started { dx: 6.0, dy: 2.0 },
                SIZE
            ),
            SheetPull::Ignored,
            "the strip keeps a pan that is only starting to cross"
        );
        assert_eq!(
            sheet.pull(
                SheetKey::RuntimeOptions,
                SheetPan::Moved {
                    dx: 120.0,
                    dy: theme::SHEET_PULL_DISMISS_TRAVEL / 2.0
                },
                SIZE
            ),
            SheetPull::Ignored,
            "a swipe along the strip is not a pull, however far it has drifted"
        );
        assert!(
            (up(&mut sheet, SheetKey::RuntimeOptions).offset).abs() < 1.0,
            "the panel never followed the sideways gesture"
        );
        assert_eq!(
            sheet.pull(SheetKey::RuntimeOptions, SheetPan::Ended, SIZE),
            SheetPull::Ignored,
            "and letting go leaves the sheet where it was"
        );
        assert!(!sheet.is_leaving(SheetKey::RuntimeOptions));

        // The same panel still answers a finger that pulls it straight down.
        assert_eq!(
            sheet.pull(
                SheetKey::RuntimeOptions,
                SheetPan::Started {
                    dx: 0.0,
                    dy: theme::SHEET_PULL_THRESHOLD
                },
                SIZE
            ),
            SheetPull::Pulling
        );
    }

    /// A sheet whose body cannot scroll — a menu of three rows — never keeps a
    /// downward pull for itself.
    #[test]
    fn a_body_that_cannot_scroll_hands_every_pull_to_the_sheet() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::ComposerAttach);
        assert_eq!(
            sheet.pull(
                SheetKey::ComposerAttach,
                SheetPan::Started { dx: 0.0, dy: 40.0 },
                SIZE
            ),
            SheetPull::Pulling
        );
    }

    /// A release that ends where the pull started is not a move, and only a
    /// move is worth announcing: the sheet's tick belongs to a height the
    /// reader actually changed, not to the drag on its way to nowhere.
    #[test]
    fn a_release_that_changes_no_height_is_not_a_move() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::AgentQuestion);

        // A pull that never claims the panel: the body under the finger keeps
        // it, and the sheet has nothing to report.
        let _ = sheet.pull(
            SheetKey::AgentQuestion,
            SheetPan::Started {
                dx: 0.0,
                dy: theme::SHEET_PULL_THRESHOLD / 2.0,
            },
            SIZE,
        );
        let _ = sheet.pull(SheetKey::AgentQuestion, SheetPan::Ended, SIZE);
        assert!(
            !sheet.settled_at_a_new_height(SheetKey::AgentQuestion),
            "a pan the body kept is not a height change"
        );

        // A downward pull that is let go short of a dismissal goes back to the
        // resting height, which is where it already was.
        let _ = sheet.pull(
            SheetKey::AgentQuestion,
            SheetPan::Started { dx: 0.0, dy: 60.0 },
            SIZE,
        );
        let _ = sheet.pull(SheetKey::AgentQuestion, SheetPan::Ended, SIZE);
        assert!(
            !sheet.settled_at_a_new_height(SheetKey::AgentQuestion),
            "giving back a height the sheet did not have is not a move"
        );

        // Pulling it open is: the finger took the panel past half of what it
        // can hold, and the sheet stays there.
        let _ = sheet.pull(
            SheetKey::AgentQuestion,
            SheetPan::Started {
                dx: 0.0,
                dy: -(EXTRA / 2.0 + 20.0),
            },
            SIZE,
        );
        let _ = sheet.pull(SheetKey::AgentQuestion, SheetPan::Ended, SIZE);
        assert!(
            sheet.settled_at_a_new_height(SheetKey::AgentQuestion),
            "the sheet crossed a height, so the move is worth its tick"
        );
        // The pull above is a stray an assertion helper reads as one more test
        // gesture; settle the motion it started before asking again.
        sheet.settle(SheetKey::AgentQuestion);
    }

    /// A gesture the platform takes away announces nothing: the reader did not
    /// finish it, so there is no move to report.
    #[test]
    fn a_cancelled_pull_is_never_a_move() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::AgentQuestion);
        let _ = sheet.pull(
            SheetKey::AgentQuestion,
            SheetPan::Started {
                dx: 0.0,
                dy: -(EXTRA / 2.0 + 20.0),
            },
            SIZE,
        );
        let _ = sheet.pull(SheetKey::AgentQuestion, SheetPan::Cancelled, SIZE);
        assert!(!sheet.settled_at_a_new_height(SheetKey::AgentQuestion));
    }

    /// A panel that a finger owns is where the finger put it: the shell is
    /// handed the pull's own offset, and the motion is held there so the
    /// animation element cannot paint its last frame over the finger.
    #[test]
    fn a_pulled_panel_follows_the_finger() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dx: 0.0, dy: 40.0 },
                SIZE
            ),
            SheetPull::Pulling
        );
        let pulled = sheet
            .presentation(SheetKey::GitBranches, true, SIZE)
            .expect("a pulled sheet is on screen");
        assert!(
            (pulled.offset - 40.0).abs() < 1.0,
            "the panel should be where the finger is: {}",
            pulled.offset
        );
        let held = pulled.motion.expect("a held panel keeps its motion");
        assert_eq!(held.from, held.to, "nothing should be animating it");
        assert!(pulled.scrim_opacity() < theme::SHEET_BACKDROP_OPACITY);

        // And it keeps following the finger, rather than only the first sample.
        sheet.pull(
            SheetKey::GitBranches,
            SheetPan::Moved { dx: 0.0, dy: 20.0 },
            SIZE,
        );
        let further = sheet
            .presentation(SheetKey::GitBranches, true, SIZE)
            .expect("a pulled sheet is on screen");
        assert!(
            (further.offset - 60.0).abs() < 1.0,
            "the panel should still be under the finger: {}",
            further.offset
        );

        // Letting go hands the panel back to a curve, from where the finger
        // left it.
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, SIZE),
            SheetPull::Restored
        );
        let released = sheet
            .presentation(SheetKey::GitBranches, true, SIZE)
            .expect("a released sheet is on screen");
        let motion = released.motion.expect("a released panel carries a motion");
        assert!(
            motion.from.offset > motion.to.offset,
            "the panel is going back up"
        );
        assert!(
            (motion.from.offset - 60.0).abs() < 1.0,
            "the release starts where the finger left the panel: {}",
            motion.from.offset
        );
    }

    /// A finger that drags up anywhere on a sheet pulls it open, and the panel
    /// is taller for it — the whole point of the gesture, since the reader is
    /// usually looking at a list that is cut off.
    #[test]
    fn an_upward_pull_opens_the_sheet_to_its_full_height() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);
        let resting = up(&mut sheet, SheetKey::GitBranches);
        assert_place(
            resting.travel,
            TRAVEL,
            "a sheet rests at its resting height",
        );

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dx: 0.0, dy: -40.0 },
                SIZE
            ),
            SheetPull::Pulling,
            "an upward pull belongs to the sheet wherever the finger landed"
        );
        let pulled = up(&mut sheet, SheetKey::GitBranches);
        assert_place(
            pulled.travel,
            TRAVEL + 40.0,
            "the sheet should be as much taller as the finger has moved",
        );
        assert_place(pulled.offset, 0.0, "and it should not have moved down");

        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved {
                    dx: 0.0,
                    dy: -400.0
                },
                SIZE
            ),
            SheetPull::Pulling
        );
        let wide = up(&mut sheet, SheetKey::GitBranches);
        assert_place(
            wide.travel,
            EXPANDED,
            "the sheet stops at the height it is allowed",
        );

        // Letting go past half way keeps the height it took.
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, SIZE),
            SheetPull::Restored
        );
        let (_, settled_height) = flight_heights(
            &sheet
                .presentation(SheetKey::GitBranches, true, SIZE)
                .expect("the sheet is on screen"),
        );
        assert_place(settled_height, EXPANDED, "the sheet stays open");
    }

    /// A short pull that is let go springs back to the height the sheet rests
    /// at, so a reader who only nudged it is not left with a half-open panel.
    #[test]
    fn a_short_upward_pull_springs_back_to_the_resting_height() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dx: 0.0, dy: -20.0 },
                SIZE
            ),
            SheetPull::Pulling
        );
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, SIZE),
            SheetPull::Restored
        );
        let (_, settled_height) = flight_heights(
            &sheet
                .presentation(SheetKey::GitBranches, true, SIZE)
                .expect("the sheet is on screen"),
        );
        assert_place(settled_height, TRAVEL, "the sheet goes back to its rest");
    }

    /// A finger on its way down gives the height back before it moves the
    /// panel: an open sheet shrinks to its resting height before it starts
    /// leaving, and only a pull that goes on past that dismisses it.
    #[test]
    fn a_downward_pull_gives_the_height_back_first() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);
        open(&mut sheet, SheetKey::GitBranches);

        // Half of the extra height comes back with this much of the pull.
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started {
                    dx: 0.0,
                    dy: EXTRA / 2.0
                },
                SIZE
            ),
            SheetPull::Pulling
        );
        let shrinking = up(&mut sheet, SheetKey::GitBranches);
        assert_place(
            shrinking.travel,
            EXPANDED - EXTRA / 2.0,
            "the sheet gives its height back first",
        );
        assert_place(shrinking.offset, 0.0, "and has not moved down yet");

        // The rest of the height, and then the dismissal distance, take the
        // finger further than the sheet was tall; past that the panel starts to
        // move, and a release leaves.
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Moved {
                    dx: 0.0,
                    dy: EXTRA / 2.0 + theme::SHEET_PULL_DISMISS_TRAVEL
                },
                SIZE
            ),
            SheetPull::Pulling
        );
        let moving = up(&mut sheet, SheetKey::GitBranches);
        assert_place(moving.travel, TRAVEL, "the height is back to its rest");
        assert!(
            moving.offset > 0.0,
            "the panel is on its way down: {}",
            moving.offset
        );
        assert_eq!(
            sheet.pull(SheetKey::GitBranches, SheetPan::Ended, SIZE),
            SheetPull::Dismissed(SheetKey::GitBranches)
        );
    }

    /// A sheet that is already open has no height left to take, so an upward
    /// pull belongs to the list inside it.
    #[test]
    fn an_open_sheet_leaves_an_upward_pull_to_its_body() {
        let mut sheet = BottomSheet::default();
        settled(&mut sheet, SheetKey::GitBranches);
        open(&mut sheet, SheetKey::GitBranches);
        assert_eq!(
            sheet.pull(
                SheetKey::GitBranches,
                SheetPan::Started { dx: 0.0, dy: -40.0 },
                SIZE
            ),
            SheetPull::Ignored,
            "nothing left to open"
        );
    }

    /// Half way is what decides whether a released pull leaves the sheet open,
    /// and a flick decides the same way whatever the distance was.
    #[test]
    fn a_released_pull_settles_the_height_it_was_going() {
        assert!(!pull_opens(EXTRA / 2.0 - 1.0, EXTRA, 0.0));
        assert!(pull_opens(EXTRA / 2.0, EXTRA, 0.0));
        assert!(
            pull_opens(EXTRA / 4.0, EXTRA, -theme::SHEET_EXPAND_VELOCITY),
            "an upward flick opens a sheet that a slow pull would not"
        );
        assert!(
            !pull_opens(EXTRA - 1.0, EXTRA, theme::SHEET_EXPAND_VELOCITY),
            "and a downward flick gives back a height a slow pull would keep"
        );
        assert!(
            !pull_opens(0.0, 0.0, 0.0),
            "a sheet with no extra height is never open"
        );
    }

    /// A pull that comes far enough takes the sheet with it, and the page is
    /// told which sheet is leaving.
    #[test]
    fn a_long_pull_dismisses_the_sheet() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::NewSessionPicker);
        assert_eq!(
            sheet.pull(
                SheetKey::NewSessionPicker,
                SheetPan::Started { dx: 0.0, dy: 120.0 },
                SIZE
            ),
            SheetPull::Pulling
        );
        assert_eq!(
            sheet.pull(SheetKey::NewSessionPicker, SheetPan::Ended, SIZE),
            SheetPull::Dismissed(SheetKey::NewSessionPicker)
        );
        assert!(sheet.is_leaving(SheetKey::NewSessionPicker));
    }

    /// A pan that belongs to no sheet on screen is left to the page.
    #[test]
    fn a_pan_on_a_page_with_no_sheet_is_ignored() {
        let mut sheet = BottomSheet::default();
        assert_eq!(
            sheet.pull(
                SheetKey::FileGoto,
                SheetPan::Started { dx: 0.0, dy: 40.0 },
                SIZE
            ),
            SheetPull::Ignored
        );
        assert_eq!(
            sheet.pull(SheetKey::FileGoto, SheetPan::Ended, SIZE),
            SheetPull::Ignored
        );
    }

    /// A gesture the platform takes away puts the panel back: the reader never
    /// finished the pull, so the sheet does not leave on it.
    #[test]
    fn a_cancelled_pull_goes_back_up() {
        let mut sheet = BottomSheet::default();
        up(&mut sheet, SheetKey::WorktreePaths);
        sheet.pull(
            SheetKey::WorktreePaths,
            SheetPan::Started { dx: 0.0, dy: 120.0 },
            SIZE,
        );
        assert_eq!(
            sheet.pull(SheetKey::WorktreePaths, SheetPan::Cancelled, SIZE),
            SheetPull::Restored
        );
        assert!(!sheet.is_leaving(SheetKey::WorktreePaths));
    }

    /// What the reader judges a dismissal against is how far the panel came,
    /// not how fast the platform happened to report it.
    #[test]
    fn a_dismissal_is_a_distance_or_a_flick() {
        assert!(!pull_dismisses(0.0, 0.0));
        assert!(!pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL - 1.0, 0.0));
        assert!(pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL, 0.0));
        assert!(pull_dismisses(
            theme::SHEET_PULL_DISMISS_TRAVEL / 4.0,
            theme::SHEET_PULL_DISMISS_VELOCITY
        ));
        assert!(
            !pull_dismisses(theme::SHEET_PULL_DISMISS_TRAVEL / 4.0, 0.0),
            "a slow short pull goes back up"
        );
    }

    /// A panel that has just opened is at the foot of the screen, and a pull
    /// that is let go short of a dismissal brings it back to its rest.
    #[test]
    fn a_panel_rises_from_the_foot_and_settles_at_its_rest() {
        let mut sheet = BottomSheet::default();
        let rising = up(&mut sheet, SheetKey::SettingsPalette);
        let (start, end) = flight_ends(&rising);
        assert_place(start, TRAVEL, "a fresh panel starts below the edge");
        assert_place(end, 0.0, "and comes to rest at the edge");
        assert!(rising.motion.is_some_and(|motion| !motion.falling));

        // A motion that is given time reaches its end.
        let motion = rising.motion.expect("a rising panel carries a motion");
        assert_eq!(motion.placement_at(motion.duration).offset, 0.0);
        assert_eq!(motion.placement_at(Duration::ZERO).offset, TRAVEL);
    }
}

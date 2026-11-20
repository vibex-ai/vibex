//! What the app adds to the touch selection the kit draws.
//!
//! A long press that lands on text is the kit's gesture: it takes the word
//! under the finger, paints a handle at each end of it, and opens its own
//! Copy / Select All menu over it. Two things a phone expects of that toolbar
//! are missing from it, and both are the app's to add.
//!
//! **A tick.** Taking a selection is the only confirmation the gesture gets
//! before the finger lifts, and every native selection gives it. It is asked
//! for when the touch selection *appears*: a handle drag and a scroll both end
//! with the menu reopening over a selection that never went away, and ticking
//! there too would make the phone buzz at the end of every gesture.
//!
//! **A way out.** The kit's Copy closes the menu and keeps the selection —
//! highlight, handles and all — which on a phone reads as "the button did
//! nothing", because the screen still shows everything it showed before. The
//! press itself cannot be hooked: the menu belongs to the kit, and its items
//! call into the kit's own state. It is read off the mouse instead, on the
//! release: a left button going up over a live touch selection with its menu
//! shut and no handle in hand is a release the menu consumed. The read happens
//! on the capture pass and the release is deferred to the end of the effect
//! cycle — the menu is an occluding popup that stops the event before the
//! bubble reaches a layer mounted this early, and the item's own action has to
//! run before the selection is let go. Nothing else reaches that state — a
//! press on the text clears the selection, a press on a handle leaves it
//! dragging until the menu reopens, a tap on the selection reopens the menu
//! where it stands, Select All reopens it over the whole message, and a scroll
//! never releases a button at all.
//!
//! Both selections the app shows are covered: the window selection over
//! message text, and the composer's own, which the kit keeps inside the input.

use std::cell::Cell;
use std::rc::Rc;

use gpui::{
    App, Bounds, DispatchPhase, Element, ElementId, Entity, GlobalElementId, InspectorElementId,
    LayoutId, MouseButton, MouseUpEvent, Pixels, Style, Subscription, Window,
};
use gpui_base::{TextSelection, TouchSelectionSnapshot};
use gpui_component::input::TextareaState;

use crate::platform;

/// The kit's touch toolbar, watched for the moment it appears.
///
/// Held by whoever installs it: the subscriptions are the whole thing.
pub struct TouchToolbar {
    _window_selection: Subscription,
    _composer: Subscription,
}

impl TouchToolbar {
    /// Watches both touch selections this window can show.
    pub fn install(composer: &Entity<TextareaState>, window: &Window, cx: &mut App) -> Self {
        let window_handle = window.window_handle();
        let window_toolbar = Appearance::default();
        let window_selection = TextSelection::observe_touch_selection(window, cx, move |cx| {
            let _ = window_handle.update(cx, |_, window, cx| {
                let shown = holding_selection(TextSelection::touch_selection(window, cx));
                if window_toolbar.note(shown) {
                    platform::long_press_feedback();
                }
            });
        });
        let composer_toolbar = Appearance::default();
        let composer_subscription = cx.observe(composer, move |input, cx| {
            let shown = holding_selection(input.read(cx).touch_selection());
            if composer_toolbar.note(shown) {
                platform::long_press_feedback();
            }
        });
        Self {
            _window_selection: window_selection,
            _composer: composer_subscription,
        }
    }
}

/// Whether a touch selection is up at all: the handles are on screen, and the
/// menu is either over them or was closed for a drag or a scroll.
fn holding_selection(snapshot: Option<TouchSelectionSnapshot>) -> bool {
    snapshot.is_some()
}

/// Whether a touch selection was up before, so the tick follows the gesture
/// that made one rather than every change to it.
#[derive(Default)]
struct Appearance(Rc<Cell<bool>>);

impl Appearance {
    /// Records what the selection looks like now, and answers whether that is
    /// one arriving.
    fn note(&self, shown: bool) -> bool {
        // Record on every look, including "gone": short-circuiting the write
        // would leave the last "up" behind after the selection was let go, and
        // the next arrival would read as a continuation and stay silent.
        let was = self.0.replace(shown);
        shown && !was
    }
}

/// The layer that catches the release a copy menu leaves behind.
///
/// Mount one anywhere in the window the selection can appear in — the layer
/// draws nothing. It listens on the capture pass, which runs before the menu
/// stops the event, and defers the release to the end of the effect cycle so
/// the item's own action has already run.
pub fn watch(composer: Entity<TextareaState>) -> SelectionWatch {
    SelectionWatch { composer }
}

/// See [`watch`].
pub struct SelectionWatch {
    composer: Entity<TextareaState>,
}

impl Element for SelectionWatch {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        Some("mobile-selection-watch".into())
    }

    fn source_location(&self) -> Option<&'static core::panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, Self::RequestLayoutState) {
        // Nothing is drawn and nothing is laid out: the layer is here for the
        // listener its paint pass registers.
        (window.request_layout(Style::default(), [], cx), ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Window,
        _: &mut App,
    ) -> Self::PrepaintState {
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut Self::RequestLayoutState,
        _: &mut Self::PrepaintState,
        window: &mut Window,
        _: &mut App,
    ) {
        // The listener is re-registered every frame this layer paints; a frame
        // that reuses the layer keeps the one from the frame it painted in.
        //
        // It reads the release on the *capture* pass, then runs after the whole
        // dispatch. The menu is an occluding popup: it stops the event before
        // the bubble reaches a listener registered this early, so a bubble
        // listener never fires at all. Deferring also puts the release after the
        // item's own action has run, which is the order the two need — Copy has
        // to take the text before the selection is let go.
        let composer = self.composer.clone();
        window.on_mouse_event(move |event: &MouseUpEvent, phase, window, cx| {
            if phase != DispatchPhase::Capture || event.button != MouseButton::Left {
                return;
            }
            let composer = composer.clone();
            window.defer(cx, move |window, cx| release(window, &composer, cx));
        });
    }
}

/// Lets go of the selection a toolbar has finished with.
///
/// The menu's own actions are the only way to reach the state this looks for;
/// see the module comment for why no other gesture can be taken for one.
fn release(window: &mut Window, composer: &Entity<TextareaState>, cx: &mut App) {
    if let Some(snapshot) = TextSelection::touch_selection(window, cx)
        && !snapshot.is_menu_open()
        && snapshot.dragging().is_none()
    {
        TextSelection::clear(window, cx);
    }
    let left_holding = composer
        .read(cx)
        .touch_selection()
        .is_some_and(|snapshot| !snapshot.is_menu_open() && snapshot.dragging().is_none());
    if left_holding {
        // The input's touch selection lets go as soon as the selection is
        // something else, so a caret where the selection was is all it takes.
        let caret = composer.read(cx).selected_range().end;
        composer.update(cx, |input, cx| {
            input.set_selected_range(caret..caret, cx);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The tick follows the touch selection arriving. A menu that closes for a
    /// scroll or a handle drag comes back over a selection that never left, and
    /// that is not a new gesture asking for feedback — the handles are what a
    /// finger is answered with, and they were already there.
    #[test]
    fn the_tick_belongs_to_the_selection_arriving() {
        let appearance = Appearance::default();
        assert!(!appearance.note(false));
        assert!(appearance.note(true), "the selection arriving is the tick");
        assert!(
            !appearance.note(true),
            "it does not tick again while it stays"
        );
        assert!(!appearance.note(false), "letting go is not a tick either");
        assert!(appearance.note(true), "and it ticks again for the next one");
    }
}

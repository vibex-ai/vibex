//! The phone's lightweight hints.
//!
//! Every notice the mobile client shows is a toast: it appears over whatever
//! screen is up, holds for a few seconds, and leaves on its own or under a tap.
//! Nothing here takes a place in a page's layout. A phone has one narrow column
//! of content, and a status line pinned under a list or above the composer both
//! pushes that content around and stays there until some later state change
//! happens to clear it — which is how a stale transport message ends up sitting
//! on the pairing screen long after the connection it describes is gone.
//!
//! The surface is the kit's `Notification` on the window's notification layer,
//! the same one the desktop workbench delivers its hints through, so tone,
//! autohide and replace-by-id behave identically on both clients. `theme.rs`
//! moves the layer to the phone's own corner and width.

use std::rc::Rc;

use gpui::{
    App, InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Styled as _,
    Window, div,
};
use gpui_component::WindowExt as _;
use gpui_component::button::Button;
use gpui_component::notification::{Notification, NotificationType};

use crate::theme;

/// The weight a hint carries.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tone {
    /// A completed step worth naming — a rename, an added project.
    Success,
    /// The app could not finish what was asked.
    Error,
    /// The connection changed under the user.
    Warning,
    /// Anything else worth saying out loud.
    Info,
}

impl Tone {
    fn notification_type(self) -> NotificationType {
        match self {
            Self::Success => NotificationType::Success,
            Self::Error => NotificationType::Error,
            Self::Warning => NotificationType::Warning,
            Self::Info => NotificationType::Info,
        }
    }

    /// The debug selector for a hint of this weight.
    ///
    /// The key is the tone rather than the message because a laid-out element's
    /// selector has to be a `&'static str`: a message is not known until the
    /// hint is built, a tone is known at compile time. A test can therefore
    /// assert the weight of what is on screen, and how many hints there are,
    /// which together say whether an announcement happened at all.
    pub fn selector(self) -> &'static str {
        match self {
            Self::Success => "mobile-hint-success",
            Self::Error => "mobile-hint-error",
            Self::Warning => "mobile-hint-warning",
            Self::Info => "mobile-hint-info",
        }
    }
}

/// What a hint's button does.
///
/// Archiving takes a session out of the list, and the card that reports it is
/// where a tap puts it back before the card ages out. The variant names the work
/// rather than carrying a closure, so a hint stays comparable and the app keeps
/// the dispatch.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum HintAction {
    /// Put back the sessions an archive just removed.
    UndoArchive(Vec<vibex_core::VibexSessionId>),
}

impl HintAction {
    pub fn label(&self) -> &'static str {
        match self {
            Self::UndoArchive(_) => crate::locale::common("Undo"),
        }
    }
}

/// One hint waiting for the next frame, or already on screen.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Hint {
    pub tone: Tone,
    pub message: String,
    /// A control on the card, when the hint reports something the reader can
    /// still take back.
    pub action: Option<HintAction>,
}

impl Hint {
    pub fn new(tone: Tone, message: impl Into<String>) -> Self {
        Self {
            tone,
            message: message.into(),
            action: None,
        }
    }

    /// A hint that offers to take back what it just reported.
    pub fn with_action(tone: Tone, message: impl Into<String>, action: HintAction) -> Self {
        Self {
            tone,
            message: message.into(),
            action: Some(action),
        }
    }
}

/// What a hint's button runs when it is tapped.
///
/// A boxed closure rather than a generic: the hint layer is handed one by the
/// app, which is the only place that knows what the action means.
pub type HintHandler = Rc<dyn Fn(&mut Window, &mut App)>;

/// Distinct marker so the kit keys mobile hints apart from any other
/// notification family that ever lands on this window.
struct MobileHint;

/// Puts one hint on the window's notification layer.
///
/// The hint's text is its identity: asking for the same thing twice replaces
/// the hint on screen instead of stacking a second copy of it, which is what
/// keeps a repeating event — a connection that keeps dropping — from filling
/// the corner with identical cards. Different messages stack, newest at the
/// bottom, and the layer caps how many are visible.
pub fn push(window: &mut Window, cx: &mut App, hint: &Hint, on_action: Option<HintHandler>) {
    // A window that never got the kit's root — a bare test window — has no
    // notification layer to push to, and `WindowExt::push_notification` panics
    // on the missing window state instead of doing nothing. Every window the
    // application opens is built by `open_root_window`, so this only decides
    // what a test window without a layer does with a hint: drop it.
    if window.root::<gpui_component::Root>().flatten().is_none() {
        return;
    }
    let message: SharedString = hint.message.clone().into();
    let key = hint.tone.selector();
    // The kit's own close button only appears under a pointer, and a phone has
    // none, so the card itself is the dismiss target: the kit takes the hint
    // down before it runs this handler, which therefore has nothing left to do.
    let action = match (hint.action.as_ref(), on_action) {
        (Some(action), Some(run)) => Some((action.label(), run)),
        _ => None,
    };
    let mut notification = Notification::new()
        .with_type(hint.tone.notification_type())
        .id1::<MobileHint>(hint.message.clone())
        .on_click(|_, _, _| {});
    if let Some((label, run)) = action {
        // The kit switches autohide off the moment an action is attached, which
        // made the archive card the one hint on the phone that never left: it
        // sat in the corner until it was tapped, long after the screen it
        // described was gone. The flag is turned back on after the action, so a
        // card with a button ages out like every other hint and the button is a
        // shortcut inside that window rather than the only way to dismiss it.
        // The undo is not lost with the card: the sessions it would have put
        // back stay archived on the desktop, where the archived list restores
        // them.
        notification = notification
            .action(move |_, _, _| {
                let run = run.clone();
                Button::new("mobile-hint-action")
                    .label(label)
                    .on_click(move |_, window, cx| run(window, cx))
            })
            .autohide(true);
    }
    let notification = notification.content(move |_, _, _| {
        div()
            .w_full()
            .min_w_0()
            .text_size(theme::font_body())
            .debug_selector({
                let key = key.to_string();
                move || key.clone()
            })
            .child(message.clone())
            .into_any_element()
    });
    window.push_notification(notification, cx);
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use gpui::{AppContext as _, Context, Render, TestAppContext, Window, div};

    use super::*;

    /// An empty page: the hint layer belongs to the kit's `Root`, not to the
    /// screen under it.
    struct Bare;

    impl Render for Bare {
        fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
            div()
        }
    }

    fn rooted_window(cx: &mut TestAppContext) -> &mut gpui::VisualTestContext {
        cx.update(gpui_component::init);
        let (_, cx) = cx.add_window_view(|window, cx| {
            let bare = cx.new(|_| Bare);
            gpui_component::Root::new(bare, window, cx)
        });
        cx
    }

    fn mounted_hints(cx: &mut gpui::VisualTestContext) -> usize {
        cx.update(|window, cx| window.notifications(cx).len())
    }

    /// The same message from several producers is one card.
    ///
    /// A single failure often has more than one operation behind it — the
    /// terminal list, the create and the fit can each report the same offline
    /// backend — and each of those paths calls `fail` independently. The hint's
    /// text is its identity, so the later pushes have to replace the card
    /// already on screen rather than stack copies of it.
    #[gpui::test]
    fn repeated_identical_hints_replace_instead_of_stacking(cx: &mut TestAppContext) {
        let cx = rooted_window(cx);
        cx.update(|window, cx| {
            for _ in 0..3 {
                push(
                    window,
                    cx,
                    &Hint::new(Tone::Error, "the authoritative terminal backend is offline"),
                    None,
                );
            }
        });
        assert_eq!(
            mounted_hints(cx),
            1,
            "three reports of one failure must read as one hint"
        );
    }

    /// A hint that carries an undo still leaves on its own.
    ///
    /// The kit clears a notification's autohide as soon as an action is attached,
    /// which made the card that reports an archive the one hint on the phone that
    /// outlived the screen it described: it sat in the corner until it was
    /// tapped. It has to age out on the kit's own clock, exactly like the hint
    /// without a button beside it.
    #[gpui::test]
    fn an_action_hint_leaves_with_the_hints_that_have_none(cx: &mut TestAppContext) {
        let cx = rooted_window(cx);
        let run: HintHandler = Rc::new(|_, _| {});
        cx.update(|window, cx| {
            push(window, cx, &Hint::new(Tone::Success, "plain"), None);
            push(
                window,
                cx,
                &Hint::with_action(
                    Tone::Success,
                    "archived",
                    HintAction::UndoArchive(Vec::new()),
                ),
                Some(run),
            );
        });
        assert_eq!(mounted_hints(cx), 2, "both hints reach the layer");

        // The enter transition, the kit's five-second hold, and the exit.
        cx.executor().advance_clock(
            Duration::from_millis(400) + Duration::from_secs(5) + Duration::from_millis(250),
        );
        cx.run_until_parked();

        assert_eq!(
            mounted_hints(cx),
            0,
            "a hint with an action ages out with the rest"
        );
    }
}

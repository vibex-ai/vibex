use std::{
    cell::RefCell,
    rc::Rc,
    sync::atomic::{AtomicU64, Ordering},
};

use gpui::{
    App, BorderStyle, ElementId, FocusHandle, InteractiveElement, Interactivity, IntoElement,
    MouseButton, ParentElement as _, RenderOnce, SharedString, StatefulInteractiveElement,
    Styled as _, Window, WindowAppearance, div, px,
};
use gpui_base::{SelectableText, TextSelectionHandle};
use gpui_component::{
    Icon, Sizable as _,
    button::{Button, ButtonVariants as _},
    empty::Empty,
    notification::{Notification, NotificationType},
};
use vibex_desktop_runtime::validate_external_open_url;

use crate::platform::open_external_url;

#[derive(IntoElement)]
struct AccessibleButton(Button);

impl InteractiveElement for AccessibleButton {
    fn interactivity(&mut self) -> &mut Interactivity {
        self.0.interactivity()
    }
}

impl StatefulInteractiveElement for AccessibleButton {}

impl RenderOnce for AccessibleButton {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        self.0
    }
}

pub fn button_with_aria_label(button: Button, label: impl Into<SharedString>) -> impl IntoElement {
    AccessibleButton(button).aria_label(label)
}

pub fn is_dark_system_appearance(cx: &App) -> bool {
    matches!(
        cx.window_appearance(),
        WindowAppearance::Dark | WindowAppearance::VibrantDark
    )
}

/// [`Empty`] paints a dashed border as soon as a border width is set. Vibex's
/// empty-state cards have always used a solid hairline, so keep that look while
/// the component owns the rest of the layout.
pub fn solid_empty_border(mut empty: Empty) -> Empty {
    empty.style().border_style = Some(BorderStyle::Solid);
    empty
}

/// Documentation pages a surface inside the product links to.
pub const DOCS_SELF_HOSTED_SERVER_URL: &str = "https://vibex.peatboy.com/docs/self-hosted-server";
pub const DOCS_REMOTE_MOBILE_URL: &str = "https://vibex.peatboy.com/docs/remote-mobile";

/// The help affordance every surface header shares: one glyph, one hit target.
/// The glyph is a child rather than `Button::icon`, so the frame owns the size
/// and the mark keeps the exact 14px the panel's other header controls use.
const DOCS_HELP_ICON: &str = "icons/vibex/circle-question-mark.svg";
const DOCS_HELP_BUTTON_SIZE: f32 = 24.0;
const DOCS_HELP_GLYPH_SIZE: f32 = 14.0;

/// Element id prefix for one hint's selectable message.
const HINT_TEXT_ID: &str = "selectable-hint-text";
/// Element id prefix for the hit area that owns a hint message's drag release.
const HINT_TEXT_GUARD_ID: &str = "selectable-hint-text-guard";
/// Test hook for the laid-out message bounds.
const HINT_TEXT_DEBUG_KEY: &str = "hint-notification-text";

/// Serial that keeps each hint's message a distinct selection participant.
///
/// A notification is rebuilt on every frame, so the identity cannot come from
/// the element tree; it is minted once per pushed hint and carried into the
/// content builder.
static NEXT_HINT_TEXT_SERIAL: AtomicU64 = AtomicU64::new(1);

/// A light hint or error result on the notification layer whose text the
/// pointer can drag-select and the copy shortcut can copy.
///
/// The kit paints a notification's `message` as plain text that no selection
/// layer can reach, so the message travels as custom content instead. The
/// surface stays the kit's `Notification`: placement, tone, autohide, and the
/// replace-by-id contract are unchanged.
///
/// The message is deliberately not also set through `Notification::message`,
/// which would paint it a second time under the content. Every hint the
/// workbench pushes is delivered in-app, so there is no system notification
/// body that needs the plain string.
///
/// This helper owns `on_close` to hand back the focus a selection took, so a
/// hint that needs its own close handling has to compose it here.
pub fn hint_notification(
    tone: NotificationType,
    message: impl Into<SharedString>,
    cx: &mut App,
) -> Notification {
    let text = SelectableHintText::new(message, cx);
    let focus = text.focus.clone();
    let restore_focus = text.restore_focus.clone();
    Notification::new()
        .with_type(tone)
        .content(move |_, _, _| {
            div()
                .text_sm()
                .w_full()
                .min_w_0()
                .child(text.clone())
                .into_any_element()
        })
        .on_close(move |window, cx| {
            // A selection took focus so the copy shortcut could reach it. The
            // hint is about to unmount, and a window still focused on it would
            // be left with no caret at all, so the focus it borrowed goes back
            // to whatever the user was working in. Only a hint that still holds
            // focus gives it back: anything the user focused in the meantime
            // outranks the element the drag started from.
            let previous = restore_focus.borrow_mut().take();
            if focus.is_focused(window)
                && let Some(previous) = previous
            {
                window.focus(&previous, cx);
            }
        })
}

/// One hint's message as a run of the window text selection.
///
/// [`SelectableText`] alone would hand the release that ends a drag to the
/// card's click-to-dismiss, so the hint would vanish exactly when its text was
/// selected. The wrapper keeps that release when it left a selection behind,
/// while a plain click still dismisses the hint.
#[derive(Clone, IntoElement)]
struct SelectableHintText {
    id: ElementId,
    guard_id: ElementId,
    text: SharedString,
    selection: TextSelectionHandle,
    focus: FocusHandle,
    /// What held focus before a selection took it, for [`hint_notification`] to
    /// hand back when the hint closes.
    restore_focus: Rc<RefCell<Option<FocusHandle>>>,
}

impl SelectableHintText {
    fn new(message: impl Into<SharedString>, cx: &mut App) -> Self {
        let serial = NEXT_HINT_TEXT_SERIAL.fetch_add(1, Ordering::Relaxed) as usize;
        let text = message.into();
        Self {
            id: ElementId::named_usize(HINT_TEXT_ID, serial),
            guard_id: ElementId::named_usize(HINT_TEXT_GUARD_ID, serial),
            selection: TextSelectionHandle::new(text.clone(), cx),
            focus: cx.focus_handle(),
            restore_focus: Rc::new(RefCell::new(None)),
            text,
        }
    }
}

impl RenderOnce for SelectableHintText {
    fn render(self, _window: &mut Window, _cx: &mut App) -> impl IntoElement {
        let selection = self.selection.clone();
        let focus = self.focus.clone();
        let restore_focus = self.restore_focus.clone();
        div()
            .id(self.guard_id)
            .track_focus(&self.focus)
            .w_full()
            .min_w_0()
            .debug_selector(|| HINT_TEXT_DEBUG_KEY.to_string())
            // A tracked focus handle takes focus on mouse down, which would pull
            // the caret out of whatever the user is typing at every time a hint
            // is clicked away. The press is kept from doing that; a release that
            // actually resolved a selection takes focus below.
            .on_mouse_down(MouseButton::Left, |_, window, _| window.prevent_default())
            .on_mouse_up(MouseButton::Left, move |_, window, cx| {
                // A release that resolved a selection over this run is a copy
                // gesture. Letting it through would dismiss the hint and take
                // the selected text with it.
                if selection.snapshot(cx).is_some() {
                    // The copy shortcut is dispatched from the focused node, so
                    // the selection has to hold focus for the hint to be
                    // copyable.
                    *restore_focus.borrow_mut() = window.focused(cx);
                    window.focus(&focus, cx);
                    cx.stop_propagation();
                }
            })
            .child(SelectableText::with_handle(
                self.id,
                self.selection,
                self.text,
            ))
    }
}

/// A quiet, icon-only help button that opens one documentation page.
///
/// `label` names the page for both the tooltip and AccessKit, because the icon
/// alone cannot say what pressing the button opens. The page is a compile-time
/// constant, so the external-open boundary can only reject it by programming
/// error; a header has no error lane, and the button stays available to retry.
pub fn docs_help_button(
    id: impl Into<SharedString>,
    label: impl Into<SharedString>,
    url: &'static str,
) -> Button {
    let label = label.into();
    let id: SharedString = id.into();
    Button::new(id)
        .small()
        .ghost()
        .compact()
        .flex_none()
        .size(px(DOCS_HELP_BUTTON_SIZE))
        .px_0()
        .tooltip(label.clone())
        .accessibility_label(label)
        .child(
            Icon::default()
                .path(DOCS_HELP_ICON)
                .size(px(DOCS_HELP_GLYPH_SIZE)),
        )
        .on_click(move |_, _, _| {
            let _ = validate_external_open_url(url)
                .and_then(|validated| open_external_url(&validated.url));
        })
}

#[cfg(test)]
mod tests {
    use gpui::AssetSource as _;

    use super::*;

    /// An icon path that is not in the asset bundle resolves to nothing, with
    /// no error and no placeholder, so a missing registration would leave the
    /// help button invisible rather than broken.
    #[test]
    fn the_docs_help_icon_is_bundled() {
        let assets = crate::assets::VibexAssets;
        assert!(
            assets
                .load(DOCS_HELP_ICON)
                .expect("asset lookup should not fail")
                .is_some(),
            "the docs help button asks for {DOCS_HELP_ICON}, which is not in the asset bundle"
        );
    }

    /// The pages are constants, but they still cross the external-open
    /// boundary on click: a URL the boundary rejects is a button that silently
    /// does nothing.
    #[test]
    fn the_docs_help_urls_pass_the_external_open_boundary() {
        for url in [DOCS_SELF_HOSTED_SERVER_URL, DOCS_REMOTE_MOBILE_URL] {
            assert_eq!(
                validate_external_open_url(url)
                    .expect("a docs page should validate")
                    .url,
                url
            );
        }
    }
}

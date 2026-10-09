//! The window's hint layer.
//!
//! GPUI paints a `deferred` draw after every inline element of a frame, so the
//! priority a layer is deferred at — not its place in the element tree — decides
//! whether a hint stays readable over an open dialog. The kit renders dialogs
//! through `gpui_base::Dialog` as deferred draws at priority `10 + layer`, popups
//! (menus, selects, popovers) at `gpui_base::POPUP_PRIORITY` (100), and tooltips
//! at 200.
//!
//! Up to GPUI Kit 0.6.4 the workbench rendered the kit's notification list itself
//! and deferred it at [`HINT_LAYER_PRIORITY`], in the gap between the dialog band
//! and the popup band, so a result pushed while a dialog was still open — a
//! settings operation, a rejected shortcut chord — stayed legible above the
//! dialog's backdrop instead of being dimmed by it. 0.7.0 moved the notification
//! layer inside `gpui_base::Root` and no longer exposes the list, which would put
//! every hint below that backdrop.
//!
//! The workbench therefore owns the list again. [`HintLayer`] is a root plugin
//! that mounts the same kit `NotificationList` in the same deferred layer, and
//! [`push`] is the one entry point that fills it. `Root` builds one plugin
//! instance per window, so a hint still lands on the window it was pushed from.

use gpui::{
    App, AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Window, deferred, div,
};
use gpui_base::{Root, RootPlugin};
use gpui_component::notification::{Notification, NotificationList};

/// Deferred paint priority for the hint layer.
///
/// The layer takes the gap between the dialog band and the popup band: above
/// every dialog backdrop, below the menus and tooltips the user is actively
/// pointing at.
const HINT_LAYER_PRIORITY: usize = 99;

/// Registers the hint layer for every window root created afterwards.
///
/// Must run during application initialization, before the first window opens.
pub fn init(cx: &mut App) {
    Root::register_plugin::<HintLayer>(cx, HintLayer::new);
}

/// Pushes one hint onto the layer of the window it is delivered to.
///
/// The `id` a notification carries still decides replace-by-id, so a newer hint
/// of the same kind replaces the older one instead of stacking on it.
///
/// `window` and `cx` are taken in the order a window callback hands them over,
/// so the pair passes straight through from the callback that produced the
/// hint.
pub fn push(window: &mut Window, notification: impl Into<Notification>, cx: &mut App) {
    let layer = Root::read(window, cx).plugin::<HintLayer>().expect(
        "the hint layer is not registered on this window; call hint_layer::init before opening it",
    );
    layer.update(cx, |layer, cx| {
        layer
            .list
            .update(cx, |list, cx| list.push(notification, window, cx));
    });
}

/// The hint list mounted on `window`'s layer, if that window has one.
///
/// Deliver hints through [`push`]; this is for reading the mounted set — how
/// many hints are on screen, or clearing them wholesale.
pub fn list(window: &Window, cx: &App) -> Option<Entity<NotificationList>> {
    Root::read(window, cx)
        .plugin::<HintLayer>()
        .map(|layer| layer.read(cx).list.clone())
}

/// The per-window host of the workbench's hints.
struct HintLayer {
    list: Entity<NotificationList>,
}

impl HintLayer {
    fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        Self {
            list: cx.new(|cx| NotificationList::new(window, cx)),
        }
    }
}

impl Render for HintLayer {
    fn render(&mut self, _window: &mut Window, _cx: &mut Context<Self>) -> impl IntoElement {
        deferred(
            div()
                .absolute()
                .top_0()
                .left_0()
                .right_0()
                .flex()
                .justify_center()
                .child(self.list.clone()),
        )
        .with_priority(HINT_LAYER_PRIORITY)
    }
}

impl RootPlugin for HintLayer {}

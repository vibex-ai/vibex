//! Android reports edge distances; only the GPUI thread converts them to a
//! content rect. A rect calculated by Java can belong to a newer surface size
//! than the render thread, turning a rotation into enormous bottom/right padding.

#[cfg(target_os = "android")]
use std::sync::OnceLock;

use gpui::{DevicePixels, Size};
use tokio::sync::watch;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct SafeAreaInsets {
    left: i32,
    top: i32,
    right: i32,
    bottom: i32,
}

impl SafeAreaInsets {
    fn new(left: i32, top: i32, right: i32, bottom: i32) -> Self {
        Self {
            left: left.max(0),
            top: top.max(0),
            right: right.max(0),
            bottom: bottom.max(0),
        }
    }

    fn content_rect(self, size: Size<DevicePixels>) -> [i32; 4] {
        [
            self.left,
            self.top,
            size.width.0 - self.right,
            size.height.0 - self.bottom,
        ]
    }
}

fn publish(sender: &watch::Sender<SafeAreaInsets>, insets: SafeAreaInsets) {
    sender.send_if_modified(|current| {
        if *current == insets {
            return false;
        }
        *current = insets;
        true
    });
}

#[cfg(target_os = "android")]
fn updates() -> &'static watch::Sender<SafeAreaInsets> {
    static UPDATES: OnceLock<watch::Sender<SafeAreaInsets>> = OnceLock::new();
    UPDATES.get_or_init(|| watch::channel(SafeAreaInsets::default()).0)
}

/// Called by the Java UI thread, including before the first GPUI window exists.
#[cfg(target_os = "android")]
pub(crate) fn report(left: i32, top: i32, right: i32, bottom: i32) {
    publish(updates(), SafeAreaInsets::new(left, top, right, bottom));
}

/// Applies the latest host insets on the process-lived GPUI thread. The watch
/// channel retains early reports and coalesces rotation/recreation bursts.
#[cfg(target_os = "android")]
pub(crate) fn observe(window: gpui::AnyWindowHandle, cx: &gpui::App) {
    let Some(native_window) =
        gpui_mobile::android::jni::platform().and_then(|platform| platform.primary_window())
    else {
        return;
    };
    let mut receiver = updates().subscribe();
    apply(&native_window, *receiver.borrow_and_update());
    cx.spawn(async move |cx| {
        while receiver.changed().await.is_ok() {
            let insets = *receiver.borrow_and_update();
            if window
                .update(cx, |_, window, _| {
                    apply(&native_window, insets);
                    // Insets can change without a resize, e.g. system bars.
                    window.refresh();
                })
                .is_err()
            {
                break;
            }
        }
    })
    .detach();
}

#[cfg(target_os = "android")]
fn apply(window: &gpui_mobile::android::AndroidWindow, insets: SafeAreaInsets) {
    // Both reads and writes use the same render-thread-owned dimensions.
    // Later surface resizes leave the stored edge distances unchanged.
    let native_size = window.physical_size();
    let size = gpui::size(
        DevicePixels(native_size.width.0),
        DevicePixels(native_size.height.0),
    );
    let [left, top, right, bottom] = insets.content_rect(size);
    window.update_safe_area_from_content_rect(left, top, right, bottom);
}

#[cfg(test)]
mod tests {
    use super::{SafeAreaInsets, publish};
    use gpui::{DevicePixels, size};
    use tokio::sync::watch;

    #[test]
    fn rotation_never_converts_a_surface_size_delta_into_an_inset() {
        let portrait = size(DevicePixels(1440), DevicePixels(3168));
        let landscape = size(DevicePixels(3008), DevicePixels(1440));
        let portrait_insets = SafeAreaInsets::new(0, 160, 0, 64);
        let landscape_insets = SafeAreaInsets::new(160, 96, 64, 0);

        // Host reports can precede or follow the queued surface resize. In
        // either order, projecting through the platform's content-rect API
        // must retain the actual edge distances, including on return to portrait.
        for (insets, surface) in [
            (portrait_insets, portrait),
            (landscape_insets, portrait),
            (landscape_insets, landscape),
            (portrait_insets, landscape),
            (portrait_insets, portrait),
        ] {
            let [left, top, right, bottom] = insets.content_rect(surface);
            let applied = SafeAreaInsets::new(
                left,
                top,
                surface.width.0 - right,
                surface.height.0 - bottom,
            );
            assert_eq!(applied, insets, "surface: {surface:?}");
        }

        // The old Java rect, read against the previous orientation, caused
        // exactly this 1568px right inset on the affected phone.
        let [_, _, old_right, _] = portrait_insets.content_rect(portrait);
        assert_eq!(landscape.width.0 - old_right, 1568);
    }

    #[test]
    fn early_reports_survive_and_rotation_bursts_deliver_the_latest_insets() {
        let (sender, receiver) = watch::channel(SafeAreaInsets::default());
        drop(receiver);
        let portrait = SafeAreaInsets::new(0, 160, 0, 64);
        let landscape = SafeAreaInsets::new(160, 96, 64, 0);

        // Android can report insets before the GPUI window subscribes.
        publish(&sender, portrait);
        let mut receiver = sender.subscribe();
        assert_eq!(*receiver.borrow_and_update(), portrait);

        publish(&sender, landscape);
        publish(&sender, portrait);
        assert!(receiver.has_changed().unwrap());
        assert_eq!(*receiver.borrow_and_update(), portrait);
        assert!(!receiver.has_changed().unwrap());

        // Repeated layout notifications do not keep an idle window repainting.
        publish(&sender, portrait);
        assert!(!receiver.has_changed().unwrap());

        // A recreated Activity observes current geometry without rebuilding GPUI.
        let replacement = sender.subscribe();
        assert_eq!(*replacement.borrow(), portrait);
    }
}

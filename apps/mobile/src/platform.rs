//! The mobile platform layer.
//!
//! Both native clients run on `gpui-pre-mobile`, whose platform types are
//! supplied per target (`gpui_mobile::android` / `::ios`) rather than by
//! `gpui_platform`. The hosts own building the platform — the Android host
//! entry does it on its process-lived render thread, the iOS one when UIKit
//! launches — so this module only normalizes the window-level APIs the app
//! calls.
//!
//! Three things differ from the desktop platform surface and are normalized
//! here instead of at every call site:
//!
//! - **Safe-area and IME insets.** `gpui-pre` has `WindowInsets`, but the
//!   mobile platform never feeds it, so `Window::fully_visible_bounds()` would
//!   report the whole window. The crate exposes the real values per platform
//!   (`safe_area_insets` / `AndroidWindow::safe_area_insets_logical` /
//!   `keyboard_height`); [`insets`] composes them into gpui's own type.
//! - **Software keyboard.** The crate drives the keyboard off focus changes and
//!   does not implement `PlatformWindow::show_soft_keyboard`, so
//!   `Window::request_virtual_keyboard()` is a no-op. [`show_keyboard`] and
//!   [`hide_keyboard`] call the crate's free functions instead.
//! - **Application lifecycle.** Nothing implements `Platform::on_app_lifecycle`
//!   on this platform; the host forwards iOS phases over FFI and Android phases
//!   over JNI, both landing in [`notify_lifecycle`].

use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

use gpui::{Edges, Pixels, Window, px};

/// The insets obscuring a window: system UI plus the software keyboard.
///
/// Mirrors the shape of `gpui::WindowInsets` for the call sites that lay the
/// root view out.
#[derive(Debug, Clone, Copy, PartialEq, Default)]
pub struct WindowInsets {
    pub top: f32,
    pub right: f32,
    pub bottom: f32,
    pub left: f32,
}

impl WindowInsets {
    /// The inset content should avoid on each edge: the system safe area and
    /// the keyboard overlap instead of stacking.
    pub fn effective(&self) -> Edges<Pixels> {
        Edges {
            top: px(self.top),
            right: px(self.right),
            bottom: px(self.bottom),
            left: px(self.left),
        }
    }
}

/// The window's obscuring insets, in logical pixels.
///
/// The keyboard's height is reported on its own rather than through the safe
/// area, so it is folded into the bottom edge — which is where it always sits.
pub fn insets(window: &Window) -> WindowInsets {
    #[cfg(target_os = "ios")]
    {
        let _ = window;
        let (top, bottom, left, right) = gpui_mobile::safe_area_insets();
        WindowInsets {
            top,
            right,
            bottom: bottom.max(gpui_mobile::keyboard_height()),
            left,
        }
    }
    #[cfg(target_os = "android")]
    {
        let _ = window;
        let Some(platform) = gpui_mobile::android::jni::platform() else {
            return WindowInsets::default();
        };
        let Some(active) = platform.primary_window() else {
            return WindowInsets::default();
        };
        let safe_area = active.safe_area_insets_logical();
        WindowInsets {
            top: safe_area.top,
            right: safe_area.right,
            bottom: safe_area.bottom.max(gpui_mobile::keyboard_height()),
            left: safe_area.left,
        }
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        // Host builds never open a window; the visible bounds are the closest
        // stand-in so tests that lay out a root view still get sane numbers.
        let visible = window.fully_visible_bounds();
        let viewport = window.viewport_size();
        WindowInsets {
            top: f32::from(visible.top()),
            right: f32::from(viewport.width - visible.right()),
            bottom: f32::from(viewport.height - visible.bottom()),
            left: f32::from(visible.left()),
        }
    }
}

/// Requests the software keyboard for the focused input.
pub fn show_keyboard() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        gpui_mobile::show_keyboard();
    }
}

/// Dismisses the software keyboard without changing GPUI focus.
pub fn hide_keyboard() {
    #[cfg(any(target_os = "android", target_os = "ios"))]
    {
        gpui_mobile::hide_keyboard();
    }
}

/// The keyboard's visibility as last reported by the platform.
///
/// Android's host activity reports it from the layout change the IME causes;
/// iOS derives it from the keyboard height `gpui-pre-mobile` tracks itself.
static KEYBOARD_VISIBLE: AtomicBool = AtomicBool::new(false);

/// Records the software keyboard's visibility (Android host callback).
pub fn set_keyboard_visible(visible: bool) {
    KEYBOARD_VISIBLE.store(visible, Ordering::Release);
}

/// Whether the software keyboard is on screen.
pub fn keyboard_visible() -> bool {
    #[cfg(target_os = "ios")]
    {
        gpui_mobile::keyboard_height() > 0.0
    }
    #[cfg(not(target_os = "ios"))]
    {
        KEYBOARD_VISIBLE.load(Ordering::Acquire)
    }
}

/// Re-requests the software keyboard for a tap on a text field.
///
/// The platform shows and hides the IME from focus changes alone, so an input
/// that keeps GPUI focus while the user dismisses the keyboard never asks for
/// it again — the tap has to, or the field stays focused with no way back to
/// typing but tapping elsewhere first.
pub fn resume_keyboard() {
    if !keyboard_visible() {
        show_keyboard();
    }
}

/// Tells the IME's editor what the focused field actually holds.
///
/// Android's keyboard decides whether a held Backspace can keep deleting from
/// the editor it is bound to, not from the app. The host activity's editor is a
/// 1×1 scratch widget that is cleared after every commit, so left alone it
/// reports an empty document with the caret at position 0 and the keyboard
/// stops repeating after the first character. Reporting the real text and
/// caret keeps the repeat alive for as long as the field has anything to
/// delete, which is how a held Backspace behaves in any other app.
///
/// `selection` is a byte range into `text`, the unit GPUI's inputs report;
/// Android counts cursor positions in UTF-16 units, so it is converted here.
/// `text` is written whole, so the caller passes the field's current value.
pub fn sync_editor_document(text: &str, selection: Range<usize>) {
    let utf16 = utf16_range(text, selection);
    #[cfg(test)]
    EDITOR_DOCUMENTS.with(|documents| {
        documents
            .borrow_mut()
            .push((text.to_owned(), utf16.clone()));
    });
    #[cfg(target_os = "android")]
    {
        android::sync_editor_document(text, utf16);
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = utf16;
    }
}

/// A byte range in `text` as the UTF-16 range Android addresses the string by.
///
/// A byte inside a character is clamped back to that character's start, so an
/// offset the platform hands over can never point between the halves of a
/// surrogate pair.
fn utf16_range(text: &str, selection: Range<usize>) -> Range<usize> {
    fn offset(text: &str, byte: usize) -> usize {
        let byte = byte.min(text.len());
        text.char_indices()
            .take_while(|(index, ch)| index + ch.len_utf8() <= byte)
            .map(|(_, ch)| ch.len_utf16())
            .sum()
    }
    offset(text, selection.start)..offset(text, selection.end)
}

// Every document published this thread's tests are about. A device call has no
// visible effect to assert on, so the recorder stands in for the JNI bridge.
#[cfg(test)]
thread_local! {
    static EDITOR_DOCUMENTS: std::cell::RefCell<Vec<(String, Range<usize>)>> =
        const { std::cell::RefCell::new(Vec::new()) };
}

/// The documents [`sync_editor_document`] has published on this thread.
#[cfg(test)]
pub fn editor_documents() -> Vec<(String, Range<usize>)> {
    EDITOR_DOCUMENTS.with(|documents| documents.borrow().clone())
}

/// Clears the recorded documents, so one test reads only its own.
#[cfg(test)]
pub fn clear_editor_documents() {
    EDITOR_DOCUMENTS.with(|documents| documents.borrow_mut().clear());
}

/// One of the device's own feedback patterns.
///
/// The names are semantics, not durations: a phone decides how each one feels,
/// and the system's touch-feedback setting decides whether any of them plays at
/// all. `Selection` is the tick a picker makes as it crosses a step, which is
/// the only one used for something that is not a finished action.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Haptic {
    /// A chip going on or off, a drawer settling on a page.
    Selection,
    /// A small tap whose work is done.
    Light,
    /// A deliberate action with weight to it: a stop, a dismissal.
    Medium,
    /// A long press — the gesture the system itself plays this way.
    Heavy,
    /// Something that succeeded.
    Success,
    /// Something the reader should look at.
    Warning,
    /// The app could not finish what was asked.
    Error,
}

/// Whether the reader lets the app play feedback.
///
/// Process-wide because the setting is, and because most call sites have no
/// window to thread it through. On by default: the switch exists so a reader
/// who does not want it can say so, not so the app has to ask first.
static HAPTICS_ENABLED: AtomicBool = AtomicBool::new(true);

/// Records the reader's feedback preference.
pub fn set_haptics_enabled(enabled: bool) {
    HAPTICS_ENABLED.store(enabled, Ordering::Release);
}

/// Whether the app is allowed to play feedback.
pub fn haptics_enabled() -> bool {
    HAPTICS_ENABLED.load(Ordering::Acquire)
}

/// The tick a long press gets when it has something to show for itself.
///
/// The platform's own long-press feedback, not a raw vibration: it is the
/// gesture the system already uses for this, it costs no permission, and a
/// phone whose owner turned touch feedback off stays quiet, which a
/// `Vibrator.vibrate` call would not honor.
pub fn long_press_feedback() {
    haptic(Haptic::Heavy);
}

/// Asks the device for one feedback pattern.
///
/// Every call site goes through here, so the preference, the lifecycle and the
/// one-tick-per-event rule live in one place. Feedback while the app is in the
/// background is dropped: a phone buzzing in a pocket for a page its owner
/// cannot see is noise, and the system's own notification is the signal that
/// belongs there.
pub fn haptic(kind: Haptic) {
    // A burst of the same pattern is one event seen several times — a hint
    // pushed from a batch of events, a state that settles over two frames.
    // Distinct patterns still play back to back, which is what lets one action
    // be followed by its notice.
    //
    // Both refusals are one condition rather than two early returns: on a host
    // build every line below is compiled out, so a trailing `return` would be
    // the whole body the lint sees, and the second guard must still not run
    // when the first one refuses — short-circuiting keeps that.
    if plays(haptics_enabled(), crate::lifecycle::is_backgrounded()) && !repeats_within_window(kind)
    {
        #[cfg(test)]
        FEEDBACKS.with(|count| count.set(count.get() + 1));
        #[cfg(any(target_os = "android", target_os = "ios"))]
        {
            let _ = gpui_mobile::packages::vibration::haptic_feedback(platform_pattern(kind));
        }
    }
}

/// Whether a tick is wanted at all, before any pattern is chosen.
///
/// Kept as a free function so the two reasons it can be refused — the reader
/// turned feedback off, and the app is not on screen — are asserted rather than
/// inferred from a device that no test can observe.
fn plays(enabled: bool, backgrounded: bool) -> bool {
    enabled && !backgrounded
}

/// How soon the same outcome counts as the same outcome.
const HAPTIC_REPEAT_WINDOW: Duration = Duration::from_millis(80);

// The last outcome this thread announced, so a repeat can be recognized. The
// interface runs on one thread, and a test harness gives every test its own,
// which is also what keeps one test's ticks out of another's count.
thread_local! {
    static LAST_HAPTIC: std::cell::Cell<Option<(Haptic, Instant)>> =
        const { std::cell::Cell::new(None) };
}

/// Whether `kind` repeats the outcome this thread just announced.
///
/// Only the patterns that report an outcome are folded. A gesture's tick is
/// issued once per gesture by the code that recognizes it, and two of those are
/// never closer together than the gesture itself, so folding them could only
/// swallow one; a hint or a notification, by contrast, can be pushed several
/// times for one event — once per event in a coalesced batch, or once per
/// surface that noticed.
fn repeats_within_window(kind: Haptic) -> bool {
    if !matches!(kind, Haptic::Success | Haptic::Warning | Haptic::Error) {
        return false;
    }
    LAST_HAPTIC.with(|last| {
        let now = Instant::now();
        let repeat = last.get().is_some_and(|(previous, at)| {
            previous == kind && now.saturating_duration_since(at) < HAPTIC_REPEAT_WINDOW
        });
        if !repeat {
            last.set(Some((kind, now)));
        }
        repeat
    })
}

/// The platform crate's own name for a pattern.
#[cfg(any(target_os = "android", target_os = "ios"))]
fn platform_pattern(kind: Haptic) -> gpui_mobile::packages::vibration::HapticFeedback {
    use gpui_mobile::packages::vibration::HapticFeedback;
    match kind {
        Haptic::Selection => HapticFeedback::Selection,
        Haptic::Light => HapticFeedback::Light,
        Haptic::Medium => HapticFeedback::Medium,
        Haptic::Heavy => HapticFeedback::Heavy,
        Haptic::Success => HapticFeedback::Success,
        Haptic::Warning => HapticFeedback::Warning,
        Haptic::Error => HapticFeedback::Error,
    }
}

// How many times this thread has asked for a tick. A test harness gives every
// test its own thread, so a test can count the ticks its own gestures cause;
// on a device the call is the only visible effect, which is exactly what a
// test cannot see.
#[cfg(test)]
thread_local! {
    static FEEDBACKS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

/// The ticks [`haptic`] has been asked for on this thread.
#[cfg(test)]
pub fn feedbacks() -> usize {
    FEEDBACKS.with(std::cell::Cell::get)
}

/// The window-inset API the rest of the app uses.
///
/// Implemented for `Window` so call sites read like the desktop ones
/// (`window.safe_insets().effective()`), keeping the platform difference in
/// one place.
///
/// The method is deliberately not called `insets`: gpui 0.3.10 added an
/// inherent `Window::insets`, and an inherent method always wins over a trait
/// method of the same name. `gpui-pre-mobile` leaves `PlatformWindow::insets`
/// at its default of zero, so a call that resolved to gpui's method would lay
/// the phone out under the status bar and the keyboard without any error.
pub trait WindowInsetsExt {
    fn safe_insets(&self) -> WindowInsets;
}

impl WindowInsetsExt for Window {
    fn safe_insets(&self) -> WindowInsets {
        insets(self)
    }
}

/// Forward an application lifecycle phase to the subscribers.
///
/// `gpui-pre-mobile` never calls `Platform::on_app_lifecycle`, so the hosts
/// report phases directly: iOS from its app-delegate FFI shims, Android from
/// the Activity's `onResume`/`onPause`.
pub fn notify_lifecycle(phase: gpui::AppLifecyclePhase) {
    crate::lifecycle::notify(phase);
}

/// Forward one phase of the system's back gesture to the subscribers.
///
/// The Android activity reports the phases its `OnBackInvokedCallback` /
/// `OnBackAnimationCallback` receives; the edge and progress it carries are
/// what let a swipe from a side strip pull the matching page out (see
/// [`crate::back_gesture`]).
pub fn notify_back_gesture(phase: i32, edge: i32, progress: f32) {
    crate::back_gesture::notify_from_host(phase, edge, progress);
}

/// Installs the platform's logging and panic handling.
///
/// Routing `log` to logcat has to happen before the first frame, or everything
/// the platform layer reports is dropped.
///
/// The Activity recreation path calls this again for every new instance, so the
/// hooks are installed exactly once: a panic hook that chained itself on each
/// start would print one backtrace per recreation.
pub fn install_diagnostics() {
    #[cfg(target_os = "android")]
    {
        static HOOKS: std::sync::OnceLock<()> = std::sync::OnceLock::new();
        HOOKS.get_or_init(|| {
            gpui_mobile::android::init_logger();
            gpui_mobile::android::jni::install_panic_hook();
            install_panic_backtrace();
        });
    }
}

/// Adds a captured backtrace after whatever the platform's hook reports.
///
/// A panic in main-thread work is fatal on Android: it unwinds into the looper
/// callback that work runs in, which cannot unwind, so the process aborts. The
/// crash dump written afterwards carries native frames only — the Rust frames
/// are popped while the first panic unwinds — which is what leaves a message
/// like "there is no reactor running" without the call site that caused it.
/// Capturing the backtrace while the panic is still on the stack keeps those
/// frames in logcat. A release build keeps no symbols, so its addresses are
/// read against the unstripped `.so` the same source builds.
#[cfg(target_os = "android")]
fn install_panic_backtrace() {
    let platform_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        platform_hook(info);
        // The platform logger is what turns this into a logcat line.
        log::error!(
            "panic backtrace:\n{}",
            std::backtrace::Backtrace::force_capture()
        );
    }));
}

/// Opens this app's page in the OS settings.
///
/// A denied local-network permission can only be re-granted there, and the
/// nearby-pairing panel that reports the denial has no other way to send the
/// user to it. Returns `false` when the platform refused to open it.
pub fn open_app_settings() -> bool {
    #[cfg(target_os = "android")]
    {
        android::open_app_settings()
    }
    #[cfg(target_os = "ios")]
    {
        unsafe { vibex_ios_open_app_settings() };
        true
    }
    #[cfg(not(any(target_os = "android", target_os = "ios")))]
    {
        false
    }
}

#[cfg(target_os = "ios")]
unsafe extern "C" {
    fn vibex_ios_open_app_settings();
}

/// Hands a link to whichever app can open it.
///
/// Only the two web schemes are accepted: the terminal prints hyperlinks an
/// application wrote, and `file:` or `intent:` would let one of those reach a
/// part of the system the reader never asked for. Anything else is dropped
/// without a word, which is what the desktop does with an unknown scheme too.
pub fn open_external_url(url: &str) -> bool {
    let url = url.trim();
    if !(url.starts_with("https://") || url.starts_with("http://")) {
        return false;
    }
    #[cfg(target_os = "android")]
    {
        android::open_external_url(url)
    }
    #[cfg(not(target_os = "android"))]
    {
        let _ = url;
        false
    }
}

/// The Android half of [`open_app_settings`] and [`open_external_url`].
///
/// The host activity is the only object that can start an intent, so the calls
/// go through [`crate::android_bridge`], which always addresses the Activity
/// the platform layer recorded last.
#[cfg(target_os = "android")]
mod android {
    /// Hands the host activity's IME editor the focused field's text and caret.
    ///
    /// The activity keeps a copy of this report and answers the keyboard's
    /// reads from it; see [`super::sync_editor_document`] for why the keyboard
    /// needs it.
    pub fn sync_editor_document(text: &str, selection: std::ops::Range<usize>) {
        crate::android_bridge::with_activity(|env, activity| {
            let text = env.new_string(text)?;
            env.call_method(
                activity,
                jni::jni_str!("gpuiSetDocument"),
                jni::jni_sig!((text: JString, start: jint, end: jint) -> ()),
                &[
                    (&text).into(),
                    jni::JValue::Int(selection.start as i32),
                    jni::JValue::Int(selection.end as i32),
                ],
            )?;
            Ok(())
        });
    }

    /// Opens one web link in the system browser.
    pub fn open_external_url(url: &str) -> bool {
        crate::android_bridge::with_activity(|env, activity| {
            let url = env.new_string(url)?;
            env.call_method(
                activity,
                jni::jni_str!("openExternalUrl"),
                jni::jni_sig!((url: JString) -> ()),
                &[(&url).into()],
            )?;
            Ok(())
        })
        .is_some()
    }

    pub fn open_app_settings() -> bool {
        crate::android_bridge::with_activity(|env, activity| {
            let result = env.call_method(
                activity,
                jni::jni_str!("openAppSettings"),
                jni::jni_sig!(() -> ()),
                &[],
            );
            if result.is_err() {
                let _ = env.exception_clear();
            }
            Ok(result.is_ok())
        })
        .unwrap_or(false)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        HAPTIC_REPEAT_WINDOW, Haptic, clear_editor_documents, editor_documents, feedbacks, haptic,
        set_haptics_enabled, sync_editor_document, utf16_range,
    };

    /// The preference is process-wide and the tests share it, so each test that
    /// changes it puts it back rather than leaving the next one muted.
    #[test]
    fn the_preference_gates_every_pattern() {
        let before = feedbacks();
        set_haptics_enabled(false);
        haptic(Haptic::Selection);
        haptic(Haptic::Warning);
        assert_eq!(
            feedbacks(),
            before,
            "a reader who turned feedback off must not feel any of it"
        );

        set_haptics_enabled(true);
        haptic(Haptic::Selection);
        assert_eq!(feedbacks(), before + 1);
    }

    /// One outcome seen several times is one tick: a hint pushed from a batch,
    /// or a state that settles over two frames, must not play twice.
    #[test]
    fn the_same_outcome_within_the_window_plays_once() {
        let before = feedbacks();
        haptic(Haptic::Success);
        haptic(Haptic::Success);
        assert_eq!(
            feedbacks(),
            before + 1,
            "the second tick is the same event as the first"
        );
    }

    /// A gesture is its own event, so two of them are always two ticks — even
    /// back to back, which is exactly how a test can produce them.
    #[test]
    fn a_gesture_is_never_folded_into_the_one_before_it() {
        let before = feedbacks();
        haptic(Haptic::Heavy);
        haptic(Haptic::Heavy);
        haptic(Haptic::Selection);
        haptic(Haptic::Selection);
        assert_eq!(feedbacks(), before + 4);
    }

    /// Distinct outcomes are distinct events, and the window is short enough
    /// that a genuinely new one is never swallowed by the one before it.
    #[test]
    fn a_different_outcome_plays_again() {
        assert!(
            HAPTIC_REPEAT_WINDOW <= std::time::Duration::from_millis(100),
            "an 80ms fold is what makes a repeat invisible to the finger"
        );
        let before = feedbacks();
        haptic(Haptic::Success);
        haptic(Haptic::Error);
        assert_eq!(feedbacks(), before + 2);
    }

    #[test]
    fn utf16_offsets_count_the_units_android_addresses_text_by() {
        // "你" is one UTF-16 unit but three UTF-8 bytes, and an emoji is two
        // units, so a byte caret cannot be handed over unchanged.
        assert_eq!(utf16_range("你😀好", 0..0), 0..0);
        assert_eq!(utf16_range("你😀好", 3..3), 1..1);
        assert_eq!(utf16_range("你😀好", 7..7), 3..3);
        assert_eq!(utf16_range("你😀好", 10..10), 4..4);
        assert_eq!(utf16_range("你😀好", 0..10), 0..4);
        assert_eq!(utf16_range("你😀好", 3..7), 1..3);
    }

    #[test]
    fn a_byte_inside_a_character_clamps_to_its_start() {
        // An offset past the end, or landing inside a multi-byte character,
        // must not split a surrogate pair when Android turns it into a cursor.
        assert_eq!(utf16_range("你😀", 1..2), 0..0);
        assert_eq!(utf16_range("你😀", 4..6), 1..1);
        assert_eq!(utf16_range("你好", 99..99), 2..2);
    }

    #[test]
    fn a_published_document_carries_the_caret_as_utf16() {
        clear_editor_documents();
        sync_editor_document("你😀好", 3..7);
        assert_eq!(
            editor_documents(),
            vec![("你😀好".to_owned(), 1..3)],
            "the keyboard is told the caret in the units it counts in"
        );
    }
}

//! The app's motion preference.
//!
//! `nsh` reports the platform's reduce-motion setting for its own transitions;
//! the phone also keeps an app-level choice, as the GPUI client does, so the
//! app's own indicators can stop while the platform keeps its own behavior.
//! The value is on the drawing thread, like the language and type scale.

use std::cell::Cell;

thread_local! {
    static REDUCE_MOTION: Cell<bool> = const { Cell::new(false) };
}

/// Records the reader's choice.
pub fn set_reduce_motion(reduce: bool) {
    REDUCE_MOTION.with(|cell| cell.set(reduce));
}

/// Whether the app should skip its own motion.
pub fn reduce_motion() -> bool {
    REDUCE_MOTION.with(Cell::get)
}

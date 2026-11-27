//! The platform hosts that embed the app.
//!
//! On the phones the app runs inside the existing Kotlin and Swift shells,
//! which own the notifications, the foreground service, the camera and the
//! discovery bridge. These modules are the entry points those shells call:
//! they mount the Rust root into a platform view and hand back what stops it.
//! The shells themselves are documented in the app's README.

#[cfg(target_os = "android")]
mod android;
#[cfg(target_os = "ios")]
mod ios;

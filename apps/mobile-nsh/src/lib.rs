//! The Vibex mobile client, written as a Native Shell (`nsh`) Rust app.
//!
//! This crate is the entry point `nsh.json` names: it registers the root
//! component, builds the pairing service and hands the page its bindings. The
//! screens live in `crates/vibex-nsh-ui`, which the desktop can reuse when it
//! moves to `nsh`.
//!
//! Why a rewrite and not a port: `nsh` mounts each platform's own views
//! (UIKit on iOS, Android views on Android), while the GPUI client drew the
//! whole surface itself. Text input, selection, scrolling, navigation and the
//! keyboard therefore become the platform's, which is the point of the move.
//! The information architecture, copy, palette and spacing stay the phone's,
//! so the two clients look like one product while they run side by side.
//!
//! What is wired today: the pairing page with its saved runtimes, manual entry
//! and nearby sheet, backed by `vibex-mobile-pairing` — entries are claimed on
//! a background task and the credential is stored in the app's data
//! directory. The camera and local-network discovery are platform seams that
//! the host integration fills in next.

mod pairing;
pub mod theme_gen;

use nsh_core::prelude::*;
use pairing::PairingService;

/// The app's UI set (`nsh.json` `"rust"`).
#[native_module("vibex/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The app's root: the whole surface, on the theme's background.
#[component]
pub fn App() -> impl IntoElement {
    // The app's own data directory. A host that keeps no files for the app
    // (the web) gets a temporary one, so the page still works there.
    let data = data_dir().unwrap_or_else(|| std::env::temp_dir().join("vibex-mobile-nsh"));
    let service = PairingService::new(data);
    view()
        .flex_1()
        .bg(move || nsh_base::theme().background)
        .child(vibex_nsh_ui::screens::pairing::pairing(service.host()))
}

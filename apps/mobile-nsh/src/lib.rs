//! The Vibex mobile client, written as a Native Shell (`nsh`) Rust app.
//!
//! This crate is the entry point `nsh.json` names: it registers the root
//! component and builds the host seams the UI layer needs. The screens live in
//! `crates/vibex-nsh-ui`, which the desktop can reuse when it moves to `nsh`.
//!
//! Why a rewrite and not a port: `nsh` mounts each platform's own views
//! (UIKit on iOS, Android views on Android), while the GPUI client drew the
//! whole surface itself. Text input, selection, scrolling, navigation and the
//! keyboard therefore become the platform's, which is the point of the move.
//! The information architecture, copy, palette and spacing stay the phone's,
//! so the two clients look like one product while they run side by side.
//!
//! What is wired today: the pairing page, with its three entries and the
//! scanner/link/nearby sheets. The callbacks below are the seams the transport
//! layer fills in next (camera, clipboard, discovery, `vibex-remote-client`);
//! they log until then, so the page is inspectable on a device without a
//! desktop runtime.

use std::rc::Rc;

use nsh_core::prelude::*;
use vibex_nsh_ui::screens::pairing::{self, PairingHost};

pub mod theme_gen;

/// The app's UI set (`nsh.json` `"rust"`).
#[native_module("vibex/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The app's root: the whole surface, on the theme's background.
#[component]
pub fn App() -> impl IntoElement {
    view()
        .flex_1()
        .bg(move || nsh_base::theme().background)
        .child(pairing::pairing(host()))
}

/// The pairing page's host seams.
///
/// Every action is a no-op that logs for now; the transport migration replaces
/// them one at a time. Keeping them in one function makes the wiring visible
/// and lets a test call the screen with a fixture host instead.
fn host() -> PairingHost {
    PairingHost {
        saved: Vec::new(),
        nearby: Vec::new(),
        on_scan: Rc::new(|| log::info!("pairing: scan the desktop's QR code")),
        on_connect: Rc::new(|entry| log::info!("pairing: connect with {entry}")),
        on_paste: Rc::new(|| None),
        on_find: Rc::new(|| log::info!("pairing: start nearby discovery")),
        on_pick_saved: Rc::new(|id| log::info!("pairing: switch to saved runtime {id}")),
        on_pick_nearby: Rc::new(|key| log::info!("pairing: pair with nearby runtime {key}")),
    }
}

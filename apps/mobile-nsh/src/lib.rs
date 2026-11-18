//! The Vibex mobile client, written as a Native Shell (`nsh`) Rust app.
//!
//! This crate is the entry point `nsh.json` names: it registers the root
//! component, builds the pairing service and the session shell, and switches
//! between them when a runtime connects. The screens live in
//! `crates/vibex-nsh-ui`, which the desktop can reuse when it moves to `nsh`.
//!
//! Why a rewrite and not a port: `nsh` mounts each platform's own views
//! (UIKit on iOS, Android views on Android), while the GPUI client drew the
//! whole surface itself. Text input, selection, scrolling, navigation and the
//! keyboard therefore become the platform's, which is the point of the move.
//! The information architecture, copy, palette and spacing stay the phone's,
//! so the two clients look like one product while they run side by side.
//!
//! What is wired today: pairing (claims and a credential store through
//! `vibex-mobile-pairing`) and the session shell (the runtime's sessions, a
//! transcript projected by `vibex-desktop-model`, and the composer). The
//! camera and local-network discovery are platform seams the host fills in
//! next.

mod pairing;
mod session;
pub mod theme_gen;

use std::rc::Rc;

use nsh_core::prelude::*;
use pairing::PairingService;
use session::SessionService;
use vibex_nsh_ui::screens::{pairing as pairing_screen, session as session_screen};

/// The app's UI set (`nsh.json` `"rust"`).
#[native_module("vibex/native")]
fn native(registry: &mut Registry) {
    registry.register::<App>();
}

/// The app's root: pairing while no runtime is connected, the session shell
/// once one is.
#[component]
pub fn App() -> impl IntoElement {
    // The app's own data directory. A host that keeps no files for the app
    // (the web) gets a temporary one, so the page still works there.
    let data = data_dir().unwrap_or_else(|| std::env::temp_dir().join("vibex-mobile-nsh"));
    let service = PairingService::new(data);
    let connected = service.connected();
    let pairing_service = service.clone();
    let shell_service = service;

    view()
        .flex_1()
        .bg(move || nsh_base::theme().background)
        .child(when(
            move || connected.get().is_none(),
            move || {
                view()
                    .flex_1()
                    .child(pairing_screen::pairing(pairing_service.host()))
                    .into_element()
            },
        ))
        .child(when(
            move || connected.get().is_some(),
            move || {
                let runtime = connected.get_untracked().expect("checked by the condition");
                let disconnect: Rc<dyn Fn()> = {
                    let service = shell_service.clone();
                    Rc::new(move || service.disconnect())
                };
                match SessionService::new(&runtime.bundle, disconnect) {
                    Ok(sessions) => view()
                        .flex_1()
                        .child(session_screen::session(sessions.host()))
                        .into_element(),
                    Err(error) => disconnected_error(&error).into_element(),
                }
            },
        ))
}

/// What the phone shows when a connected runtime cannot build its backend.
fn disconnected_error(error: &vibex_backend::BackendError) -> impl IntoElement {
    view()
        .flex_1()
        .items_center()
        .justify_center()
        .p(px(24.0))
        .child(
            text(pairing::error_text(error))
                .text_size(px(15.0))
                .text_color(move || nsh_base::theme().destructive),
        )
}

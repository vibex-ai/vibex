//! The pairing page's flows, rendered headlessly through the real engine.
//!
//! These mount [`vibex_mobile_nsh::App`] with `nsh`'s headless driver, so the
//! screen is built, laid out and queried the way a phone would run it, without
//! a simulator. They are the start of the parity suite: every screen the
//! migration moves over gets the flows its GPUI pair has.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use tempfile::TempDir;

/// Mounts the real root with its own data directory, so saved runtimes never
/// cross between tests (or onto this machine's real store).
fn start() -> (Headless, TempDir) {
    let directory = TempDir::new().unwrap();
    let mut config = EngineConfig::new(Root::of::<vibex_mobile_nsh::App>());
    config.data_dir = Some(directory.path().to_path_buf());
    let mut app = Headless::start(config).unwrap();
    app.wait_for("Scan").unwrap();
    (app, directory)
}

#[test]
fn the_pairing_page_shows_the_product_and_three_ways_in() {
    let (app, _directory) = start();
    assert!(app.screen().shows("ibex"), "the wordmark is on the page");
    for entry in ["Scan", "Find", "Link"] {
        assert!(app.screen().shows(entry), "`{entry}` is offered");
    }
}

#[test]
fn link_opens_the_manual_entry_sheet() {
    let (mut app, _directory) = start();
    app.press("role:button[Link]").unwrap();
    app.wait_for("Link a runtime").unwrap();
    app.wait_for("Paste a code or link").unwrap();
    // The connect action stays disabled until there is something to claim.
    assert!(!app.screen().enabled("role:button[Connect]").unwrap());
    app.type_text("text:vibex://…", "vibex://example").unwrap();
    app.wait_for("Connect").unwrap();
    assert!(app.screen().enabled("role:button[Connect]").unwrap());
}

#[test]
fn find_opens_the_nearby_sheet() {
    let (mut app, _directory) = start();
    app.press("role:button[Find]").unwrap();
    app.wait_for("Nearby").unwrap();
    app.wait_for("Searching...").unwrap();
}

#[test]
fn a_bare_code_without_an_address_says_what_is_missing() {
    let (mut app, _directory) = start();
    app.press("role:button[Link]").unwrap();
    app.wait_for("Link a runtime").unwrap();
    app.type_text("text:vibex://…", "123456").unwrap();
    app.press("role:button[Connect]").unwrap();
    // The claim is refused before any network call, and the page says which
    // piece of the entry is missing.
    app.wait_for("Enter the pairing code and the server address.")
        .unwrap();
}

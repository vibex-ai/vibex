//! The pairing page's flows, rendered headlessly through the real engine.
//!
//! These mount [`vibex_mobile_nsh::App`] with `nsh`'s headless driver, so the
//! screen is built, laid out and queried the way a phone would run it, without
//! a simulator. They are the start of the parity suite: every screen the
//! migration moves over gets the flows its GPUI pair has.

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;

fn start() -> Headless {
    let mut app = Headless::start(EngineConfig::new(Root::of::<vibex_mobile_nsh::App>())).unwrap();
    app.wait_for("Scan").unwrap();
    app
}

#[test]
fn the_pairing_page_shows_the_product_and_three_ways_in() {
    let app = start();
    assert!(app.screen().shows("ibex"), "the wordmark is on the page");
    for entry in ["Scan", "Find", "Link"] {
        assert!(app.screen().shows(entry), "`{entry}` is offered");
    }
}

#[test]
fn link_opens_the_manual_entry_sheet() {
    let mut app = start();
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
    let mut app = start();
    app.press("role:button[Find]").unwrap();
    app.wait_for("Nearby").unwrap();
    app.wait_for("Searching...").unwrap();
}

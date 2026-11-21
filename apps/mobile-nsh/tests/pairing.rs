//! The pairing page's flows, rendered headlessly through the real engine.
//!
//! These mount [`vibex_mobile_nsh::App`] with `nsh`'s headless driver, so the
//! screen is built, laid out and queried the way a phone would run it, without
//! a simulator. They are the start of the parity suite: every screen the
//! migration moves over gets the flows its GPUI pair has.

use std::path::Path;

use nsh_engine::{EngineConfig, Root};
use nsh_testing::Headless;
use tempfile::TempDir;

/// Mounts the real root with its own data directory, so saved runtimes never
/// cross between tests (or onto this machine's real store).
fn start() -> (Headless, TempDir) {
    let directory = TempDir::new().unwrap();
    let app = start_at(directory.path());
    (app, directory)
}

fn start_at(directory: &Path) -> Headless {
    let mut config = EngineConfig::new(Root::of::<vibex_mobile_nsh::App>());
    config.data_dir = Some(directory.to_path_buf());
    let mut app = Headless::start(config).unwrap();
    app.wait_for("ibex").unwrap();
    app
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
    app.wait_for("Paste a code or link").unwrap();
    app.type_text("text:Pairing code or link", "vibex://example")
        .unwrap();
    app.wait_for("Connect").unwrap();
    assert!(app.screen().enabled("role:button[Connect]").unwrap());
}

#[test]
fn find_opens_the_nearby_sheet() {
    let (mut app, _directory) = start();
    app.press("role:button[Find]").unwrap();
    app.wait_for("Find on this network").unwrap();
    app.wait_for("Searching...").unwrap();
}

#[test]
fn an_empty_entry_is_refused_with_the_copy_that_names_what_to_paste() {
    let (mut app, _directory) = start();
    app.press("role:button[Link]").unwrap();
    app.wait_for("Paste a code or link").unwrap();
    app.press("role:button[Connect]").unwrap();
    app.wait_for("Paste the pairing code or link from the computer or the cloud runtime.")
        .unwrap();
}

#[test]
fn a_bare_code_without_an_address_says_what_is_missing() {
    let (mut app, _directory) = start();
    app.press("role:button[Link]").unwrap();
    app.wait_for("Paste a code or link").unwrap();
    app.type_text("text:Pairing code or link", "123456")
        .unwrap();
    app.press("role:button[Connect]").unwrap();
    // The claim is refused before any network call, and the sheet says which
    // piece of the entry is missing.
    app.wait_for("Enter the server address as well as the pairing code.")
        .unwrap();
}

#[test]
fn settings_changes_the_language_and_survives_the_sheet() {
    let (mut app, _directory) = start();
    app.press("role:button[Settings]").unwrap();
    app.wait_for("Appearance").unwrap();
    app.press("role:button[Language]").unwrap();
    app.wait_for("简体中文").unwrap();
    app.press("role:button[简体中文]").unwrap();
    // The whole surface is rebuilt in the chosen language, not just the row.
    app.wait_for("语言").unwrap();
    app.press("role:button[返回]").unwrap();
    app.wait_for("设置").unwrap();
    assert!(app.screen().shows("外观"));
    app.press("role:button[返回]").unwrap();
    app.wait_for_gone("外观", std::time::Duration::from_secs(5))
        .unwrap();
}

#[test]
fn the_chosen_language_survives_a_restart() {
    let directory = TempDir::new().unwrap();
    {
        let mut app = start_at(directory.path());
        app.press("role:button[Settings]").unwrap();
        app.wait_for("Appearance").unwrap();
        app.press("role:button[Language]").unwrap();
        app.wait_for("简体中文").unwrap();
        app.press("role:button[简体中文]").unwrap();
        app.wait_for("语言").unwrap();
    }
    // The settings store is written on the engine's own thread; give it the
    // moment the next launch would have spent before it reads the page.
    std::thread::sleep(std::time::Duration::from_millis(300));
    let mut app = start_at(directory.path());
    app.press("role:button[设置]").unwrap();
    app.wait_for("外观").unwrap();
}

#[test]
fn settings_steps_the_text_size() {
    let (mut app, _directory) = start();
    app.press("role:button[Settings]").unwrap();
    app.wait_for("Appearance").unwrap();
    app.press("role:button[Appearance]").unwrap();
    app.wait_for("Text size").unwrap();
    app.press("role:button[Increase text size]").unwrap();
    app.wait_for("Large").unwrap();
    app.press("role:button[Decrease text size]").unwrap();
    app.wait_for("Standard").unwrap();
}

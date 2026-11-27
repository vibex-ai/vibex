//! The pairing page's flows, rendered headlessly through the real engine.
//!
//! These mount [`vibex_mobile::App`] with `nsh`'s headless driver, so the
//! screen is built, laid out and queried the way a phone would run it, without
//! a simulator. They are the start of the parity suite: every screen the
//! migration moves over gets the flows its GPUI pair has.

use std::path::Path;

use nsh_core::prelude::*;
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
    let mut config = EngineConfig::new(Root::of::<vibex_mobile::App>());
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
fn settings_offers_every_language_the_clients_ship() {
    let (mut app, _directory) = start();
    app.press("role:button[Settings]").unwrap();
    app.wait_for("Appearance").unwrap();
    app.press("role:button[Language]").unwrap();
    app.wait_for("日本語").unwrap();
    for language in [
        "한국어",
        "Deutsch",
        "Français",
        "Español",
        "Português (BR)",
        "Русский",
    ] {
        assert!(app.screen().shows(language), "`{language}` is offered");
    }
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

fn seed_multi_route_runtime(directory: &Path, display_name: Option<&str>, last_connected: bool) {
    let identity =
        vibex_remote_client::ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).unwrap();
    let bundle = vibex_mobile_pairing::MobileCredentialBundle {
        schema_version: vibex_mobile_pairing::MOBILE_CREDENTIAL_SCHEMA_VERSION.to_owned(),
        record: vibex_remote_client::RemoteCredentialRecord {
            server_url: "https://studio.tailnet.ts.net".to_owned(),
            auth: vibex_core::RemoteAuthProof {
                device_id: identity.device_id().clone(),
                auth_token: "grant-token".to_owned(),
            },
            device_identity_public_key: identity.public_key_base64(),
            server_identity_public_key: Some("server-public-key".to_owned()),
        },
        identity_private_key: identity.private_key_base64(),
        expected_server_id: "machine-1".to_owned(),
        client_type: vibex_core::RemoteClientType::Mobile,
        allow_insecure_local_dev: false,
        display_name: display_name.map(str::to_owned),
        route: Some(vibex_mobile_pairing::MobileRemoteRouteBundle {
            local_network: None,
            direct_candidates: vec![
                "https://studio.tailnet.ts.net".to_owned(),
                "https://direct.example.com".to_owned(),
            ],
            relay: Some(vibex_mobile_pairing::MobileRelayCandidate {
                url: "https://relay.example.com".to_owned(),
                room_id: vibex_core::RelayRoomId::new(),
                local_peer_id: vibex_core::RelayPeerId::new(),
                pc_peer_id: vibex_core::RelayPeerId::new(),
                pc_public_key: "relay-pub".to_owned(),
            }),
        }),
    };
    vibex_mobile_pairing::PairingStore::new(directory.join("pairing.json"))
        .upsert(bundle)
        .unwrap();
    if last_connected {
        std::fs::write(directory.join("last-runtime.txt"), "machine-1").unwrap();
    }
}

#[test]
fn actions_sheet_shows_connection_methods_for_multi_route_runtime() {
    let directory = TempDir::new().unwrap();
    seed_multi_route_runtime(directory.path(), Some("Studio Mac"), false);
    let mut app = start_at(directory.path());
    assert!(app.screen().shows("Studio Mac"));
    assert!(!app.screen().shows("studio.tailnet.ts.net"));

    app.press("role:button[Runtime actions]").unwrap();
    app.wait_for("Connection method").unwrap();
    for expected in [
        "Auto (Direct first)",
        "Tailscale",
        "Direct",
        "Self-hosted relay",
    ] {
        assert!(app.screen().shows(expected), "`{expected}` is offered");
    }
}

#[test]
fn removing_the_only_saved_runtime_in_switcher_returns_to_initial_pairing_page() {
    let directory = TempDir::new().unwrap();
    seed_multi_route_runtime(directory.path(), Some("Studio Mac"), true);
    let mut config = EngineConfig::new(Root::of::<vibex_mobile::App>());
    config.data_dir = Some(directory.path().to_path_buf());
    let mut app = Headless::start(config).unwrap();

    // Open switcher from the connecting screen's Switch runtime button.
    app.wait_for("role:button[Switch runtime]").unwrap();
    app.press("role:button[Switch runtime]").unwrap();
    app.wait_for("role:button[Back]").unwrap();
    app.wait_for("Studio Mac").unwrap();

    // Remove the only paired runtime.
    app.press("role:button[Runtime actions]").unwrap();
    app.wait_for("role:button[Remove]").unwrap();
    app.press("role:button[Remove]").unwrap();
    app.wait_for("Remove \"Studio Mac\"?").unwrap();
    app.press("role:button[Remove]").unwrap();

    // Should leave switcher mode automatically and land on the initial pairing page.
    app.wait_for("ibex").unwrap();
    assert!(app.screen().shows("Scan"));
    assert!(app.screen().shows("Find"));
    assert!(app.screen().shows("Link"));
    assert!(
        app.screen().layout("role:button[Back]").unwrap().is_none(),
        "initial pairing page must not show a switcher Back button"
    );
}

#[component]
fn PairingWithUpdate() -> impl IntoElement {
    let installed = signal(None::<SharedString>);
    vibex_mobile_ui::update::status().set(vibex_mobile_ui::update::Availability::Available {
        version: "9.9.9".to_owned(),
        download: Some("https://example.com/vibex-9.9.9.apk".to_owned()),
    });
    spawn(async move {
        loop {
            timer(std::time::Duration::from_millis(20)).await;
            if let Some(url) = vibex_mobile_ui::update::take_install() {
                installed.set(Some(url.into()));
                break;
            }
        }
    });
    type PairFn = std::rc::Rc<dyn Fn(&str, &str)>;
    let noop: std::rc::Rc<dyn Fn()> = std::rc::Rc::new(|| {});
    let noop_pick: std::rc::Rc<dyn Fn(&str)> = std::rc::Rc::new(|_| {});
    let noop_pair: PairFn = std::rc::Rc::new(|_, _| {});
    let host = vibex_mobile_ui::screens::pairing::PairingHost {
        saved: signal(Vec::new()),
        nearby: signal(Vec::new()),
        nearby_phase: signal(vibex_mobile_ui::screens::pairing::NearbyPhase::Searching),
        on_stop_nearby: noop.clone(),
        on_retry_nearby: noop.clone(),
        busy: signal(false),
        error: signal(None),
        on_scan: noop.clone(),
        on_connect: noop_pair.clone(),
        on_paste: std::rc::Rc::new(|_| {}),
        on_find: noop.clone(),
        on_pick_saved: noop_pick.clone(),
        on_pick_route: noop_pair.clone(),
        actions_id: signal(None),
        dialog_id: signal(None),
        rename_open: signal(false),
        remove_open: signal(false),
        on_show_actions: noop_pick.clone(),
        on_close_actions: noop.clone(),
        on_open_rename: noop_pick.clone(),
        on_close_rename: noop.clone(),
        on_open_remove: noop_pick.clone(),
        on_close_remove: noop.clone(),
        on_show_details: noop_pick.clone(),
        on_remove_saved: noop_pick.clone(),
        on_rename_saved: noop_pair,
        rename_draft: signal(String::new()),
        switcher: signal(false),
        on_close_switcher: noop.clone(),
        connected_label: signal(None),
        active_id: signal(None),
        on_disconnect: noop.clone(),
        on_clear_runtimes: noop.clone(),
        details: signal(None),
        on_close_details: noop.clone(),
        on_pick_nearby: noop_pick,
        on_open_settings: noop.clone(),
        on_open_platform_settings: noop,
    };
    view()
        .size_full()
        .child(vibex_mobile_ui::screens::pairing::pairing(host))
        .child(when(
            move || installed.get().is_some(),
            move || text(move || installed.get().unwrap_or_default()),
        ))
}

#[test]
fn update_button_sits_left_of_settings_on_pairing_page_and_queues_install() {
    let config = EngineConfig::new(Root::of::<PairingWithUpdate>());
    let mut app = Headless::start(config).unwrap();
    app.wait_for("ibex").unwrap();
    app.wait_for("role:button[Install update]").unwrap();

    let update_bounds = app
        .screen()
        .layout("role:button[Install update]")
        .unwrap()
        .expect("update button on pairing page")
        .bounds;
    let settings_bounds = app
        .screen()
        .layout("role:button[Settings]")
        .unwrap()
        .expect("settings button on pairing page")
        .bounds;
    assert!(
        update_bounds.origin.x.0 + update_bounds.size.width.0 <= settings_bounds.origin.x.0,
        "update button must sit to the left of Settings: update={update_bounds:?}, settings={settings_bounds:?}"
    );
    assert!(
        (update_bounds.origin.y.0 - settings_bounds.origin.y.0).abs() < 1.0,
        "update button must align vertically with Settings: update={update_bounds:?}, settings={settings_bounds:?}"
    );

    app.press("role:button[Install update]").unwrap();
    app.wait_for("text:https://example.com/vibex-9.9.9.apk")
        .unwrap();
}

//! The iOS host's contract, checked without an Xcode.
//!
//! The Swift shell and the Rust library meet at one C ABI. Nothing on this
//! machine compiles either side for iOS, so the only way to catch a typo in a
//! symbol name is to compare the three files that have to agree:
//!
//! - `ios/Headers/VibexMobile.h` declares what Swift may call;
//! - `src/platform/ios.rs` defines what the library exports;
//! - `ios/Vibex/*.swift` and `ios/VibexShare/*.swift` call them.
//!
//! A function in the header that Rust does not define is a link error on a
//! Mac that would otherwise only show up there; a call the header does not
//! declare is a Swift compile error. Both are cheap to check here.

use std::{collections::BTreeSet, fs, path::Path};

/// The header's `vibex_mobile_*` function names.
fn declared() -> BTreeSet<String> {
    let header = fs::read_to_string(header_path()).expect("the iOS host header");
    let mut names = BTreeSet::new();
    for line in header.lines() {
        let line = line.trim();
        // Declarations are `RET name(ARGS);` at the file's top level, one per
        // line, and comments were stripped by the `///` check.
        if line.starts_with("///") || line.starts_with("//") || line.starts_with('#') {
            continue;
        }
        let Some(semicolon) = line.find('(') else {
            continue;
        };
        let before = &line[..semicolon];
        let Some(name) = before.split_whitespace().last() else {
            continue;
        };
        // `void *vibex_mobile_attach(void *host)`: the return type's `*`
        // rides on the declaration's last token.
        let name = name.trim_start_matches('*');
        if name.starts_with("vibex_mobile_") {
            names.insert(name.to_owned());
        }
    }
    names
}

/// The names `src/platform/ios.rs` exports with `#[unsafe(no_mangle)]`.
fn exported() -> BTreeSet<String> {
    let source =
        fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("src/platform/ios.rs"))
            .expect("the iOS platform module");
    let mut names = BTreeSet::new();
    let mut lines = source.lines().peekable();
    while let Some(line) = lines.next() {
        if !line.contains("#[unsafe(no_mangle)]") {
            continue;
        }
        // The signature is on the next non-attribute line:
        // `pub extern "C" fn vibex_mobile_attach(…)`.
        for next in lines.by_ref() {
            let next = next.trim();
            if next.is_empty() || next.starts_with("#[") {
                continue;
            }
            if let Some(rest) = next.split("fn ").nth(1)
                && let Some(name) = rest.split(['(', '<']).next()
            {
                names.insert(name.trim().to_owned());
            }
            break;
        }
    }
    names
}

/// The symbols the Swift host calls.
fn called() -> BTreeSet<String> {
    let mut names = BTreeSet::new();
    let ios = Path::new(env!("CARGO_MANIFEST_DIR")).join("ios");
    for directory in ["Vibex", "VibexShare"] {
        let Ok(entries) = fs::read_dir(ios.join(directory)) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().and_then(|ext| ext.to_str()) != Some("swift") {
                continue;
            }
            let source = fs::read_to_string(&path).expect("a Swift source");
            for (index, _) in source.match_indices("vibex_mobile_") {
                let rest = &source[index..];
                let end = rest
                    .find(|c: char| !(c.is_ascii_alphanumeric() || c == '_'))
                    .unwrap_or(rest.len());
                // A declaration in the bridging header is not a call, and a
                // doc comment naming a function is not one either: the
                // caller is inside Swift code, which names the function then
                // a `(`.
                let after = &rest[end..];
                if after.starts_with('(') {
                    names.insert(rest[..end].to_owned());
                }
            }
        }
    }
    names
}

fn header_path() -> std::path::PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/Headers/VibexMobile.h")
}

#[test]
fn every_declared_symbol_is_exported_by_the_library() {
    let declared = declared();
    assert!(
        declared.len() > 10,
        "the header should declare the host ABI, found {declared:?}"
    );
    let exported = exported();
    let missing = declared.difference(&exported).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "declared in VibexMobile.h but not exported by src/platform/ios.rs: {missing:?}"
    );
    let extra = exported.difference(&declared).cloned().collect::<Vec<_>>();
    assert!(
        extra.is_empty(),
        "exported by src/platform/ios.rs but missing from VibexMobile.h: {extra:?}"
    );
}

#[test]
fn the_swift_host_only_calls_declared_symbols() {
    let declared = declared();
    let called = called();
    assert!(
        called.len() > 5,
        "the Swift host should use the ABI, found {called:?}"
    );
    let missing = called.difference(&declared).cloned().collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "called from Swift but not declared in VibexMobile.h: {missing:?}"
    );
}

#[test]
fn the_host_answers_every_request_the_app_makes() {
    // The request codes are the app's (`src/host.rs`), and the shell switches
    // on all of them. A code with no case would silently do nothing.
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/Vibex/HostBridge.swift"),
    )
    .expect("the host bridge");
    for (code, name) in [
        (1, "scan"),
        (2, "discoveryStart"),
        (3, "discoveryStop"),
        (4, "camera"),
        (5, "appSettings"),
    ] {
        let arm = format!("case .{name}:");
        assert!(
            source.contains(&arm),
            "request code {code} ({name}) has no branch in HostBridge.poll"
        );
    }
}

#[test]
fn the_share_extension_and_the_app_agree_on_their_handoff() {
    // The handoff is a URL, not an App Group: a free Apple ID — the account
    // the unsigned install path uses — cannot sign the App Groups
    // entitlement, and the GPUI client this build replaces had no
    // entitlements either. Both sides have to name the same scheme host and
    // query keys for a share to arrive.
    let app = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/Vibex/ShareIntake.swift"),
    )
    .expect("the app's share intake");
    let extension = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/VibexShare/ShareViewController.swift"),
    )
    .expect("the share extension");
    for (key, value) in [
        ("scheme", "vibex"),
        ("host", "share"),
        ("text", "text"),
        ("pasteboard", "pasteboard"),
    ] {
        for (side, source) in [("app", &app), ("extension", &extension)] {
            assert!(
                source.contains(&format!("\"{value}\"")),
                "the {side} side of a share must name {key} as {value}"
            );
        }
    }
    // Asking for an App Group would make the bundle un-signable with a free
    // Apple ID, and the install path depends on that.
    for entitlements in ["ios/Vibex.entitlements", "ios/VibexShare.entitlements"] {
        assert!(
            !Path::new(env!("CARGO_MANIFEST_DIR"))
                .join(entitlements)
                .exists(),
            "{entitlements} must stay deleted: a managed entitlement breaks free re-signing"
        );
    }
    let project = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/project.yml"))
        .expect("the iOS project");
    assert!(
        !project.contains("CODE_SIGN_ENTITLEMENTS"),
        "the iOS project must not ask for entitlements"
    );
}

#[test]
fn the_app_keeps_the_gpui_bundle_id_so_an_update_replaces_it() {
    // iOS installs a different bundle id side by side, so the upgrade in place
    // — and the data the old build left in Application Support/Vibex — depends
    // on this id staying the GPUI client's.
    let project = fs::read_to_string(Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/project.yml"))
        .expect("the iOS project");
    assert!(
        project.contains("PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile\n"),
        "the app bundle id must stay ai.vibex.mobile"
    );
    assert!(
        project.contains("PRODUCT_BUNDLE_IDENTIFIER: ai.vibex.mobile.share\n"),
        "the share extension must stay under the app's bundle id"
    );
    assert!(
        !project.contains("ai.vibex.mobile.nsh"),
        "no target may keep the old nsh-suffixed bundle id"
    );
}

#[test]
fn notifications_are_installed_before_the_app_is_attached() {
    // A press that starts the process is delivered to the delegate the
    // process has when it finishes launching; `attach` runs the app, which
    // listens later. Installing after the mount would drop that first press.
    let source = fs::read_to_string(
        Path::new(env!("CARGO_MANIFEST_DIR")).join("ios/Vibex/AppDelegate.swift"),
    )
    .expect("the app delegate");
    let install = source
        .find("vibex_mobile_install_notifications()")
        .expect("the delegate installs the notification center");
    let attach = source
        .find("vibex_mobile_attach(")
        .expect("the delegate mounts the app");
    assert!(
        install < attach,
        "install the notification center before mounting the app"
    );
}

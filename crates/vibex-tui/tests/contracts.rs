//! Contract tests: the architectural rules, pinned so a later change cannot
//! quietly break them.
//!
//! These are deliberately written against files and manifests rather than
//! against behaviour, because the rules they protect are about *shape* — which
//! crates may be reached, which keys must resolve, which words may never appear
//! on screen.

use std::path::{Path, PathBuf};

fn workspace_root() -> PathBuf {
    // `CARGO_MANIFEST_DIR` is `crates/vibex-tui`.
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("workspace root")
        .to_path_buf()
}

fn read(path: &Path) -> String {
    std::fs::read_to_string(path).unwrap_or_else(|error| panic!("{}: {error}", path.display()))
}

fn source_files(directory: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![directory.to_path_buf()];
    while let Some(directory) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&directory) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|extension| extension == "rs") {
                files.push(path);
            }
        }
    }
    files
}

/// Every `use` and path reference in the crate's sources.
fn crate_sources() -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    source_files(&root)
        .iter()
        .map(|path| read(path))
        .collect::<Vec<_>>()
        .join("\n")
}

#[test]
fn the_client_never_reaches_past_the_backend_traits() {
    // The TUI is a client. Reaching for the runtime, the database, the browser
    // runtime or the PDF/Office stack would make it a second state owner and
    // would drag a window server into a character-grid binary.
    let sources = crate_sources();
    for forbidden in [
        "vibex_desktop_runtime",
        "vibex_db",
        "vibex_agent_acp",
        "vibex_browser",
        "vibex_content",
        "gpui::",
        "gpui_component",
    ] {
        assert!(
            !sources.contains(forbidden),
            "crates/vibex-tui must not reference `{forbidden}`"
        );
    }
}

#[test]
fn the_manifest_declares_no_gui_or_runtime_dependency() {
    let manifest = read(&Path::new(env!("CARGO_MANIFEST_DIR")).join("Cargo.toml"));
    let dependencies = manifest
        .split("[dependencies]")
        .nth(1)
        .expect("a dependencies section")
        .split('[')
        .next()
        .expect("dependency block");
    for forbidden in [
        "gpui",
        "vibex-desktop-runtime",
        "vibex-db",
        "vibex-browser",
        "vibex-content",
    ] {
        assert!(
            !dependencies.contains(forbidden),
            "crates/vibex-tui declares `{forbidden}`"
        );
    }
    // The client talks to the runtime only through the domain traits.
    assert!(dependencies.contains("vibex-backend"));
}

#[test]
fn every_advertised_binding_resolves_to_an_intent() {
    // The key bar and the help panel render `label.is_some()` bindings, so a
    // label on a key that resolves to nothing would advertise a dead key.
    let keymap = vibex_tui::keymap::Keymap::built_in();
    for binding in keymap.bindings() {
        let resolved = keymap.resolve(&[binding.scope], binding.chord);
        assert_eq!(
            resolved,
            Some(binding.intent),
            "{} {} does not resolve to its own intent",
            binding.scope.id(),
            binding.chord
        );
    }
}

#[test]
fn every_intent_is_reachable_from_some_scope() {
    let keymap = vibex_tui::keymap::Keymap::built_in();
    for intent in vibex_tui::action::Intent::ALL {
        assert!(
            keymap.chord_for(*intent).is_some(),
            "{} has no reachable key",
            intent.id()
        );
    }
}

#[test]
fn advertised_keys_are_unique_within_a_scope() {
    let keymap = vibex_tui::keymap::Keymap::built_in();
    let mut seen = std::collections::HashMap::new();
    for binding in keymap.bindings() {
        if binding.label.is_none() {
            continue;
        }
        if let Some(previous) =
            seen.insert((binding.scope, binding.chord.display()), binding.intent)
        {
            assert_eq!(
                previous,
                binding.intent,
                "{} {} is advertised for two intents",
                binding.scope.id(),
                binding.chord
            );
        }
    }
}

#[test]
fn escape_never_cancels_a_running_turn() {
    // `Esc` is overloaded by most terminals already; making it also interrupt
    // would mean "close this overlay" and "stop the Agent" are the same key.
    let keymap = vibex_tui::keymap::Keymap::built_in();
    for scope in [
        vibex_tui::keymap::Scope::Agent,
        vibex_tui::keymap::Scope::Composer,
        vibex_tui::keymap::Scope::Global,
    ] {
        let intent = keymap.resolve(
            &[scope],
            vibex_tui::keymap::Chord::plain(crossterm::event::KeyCode::Esc),
        );
        assert_ne!(
            intent,
            Some(vibex_tui::action::Intent::ContextualCancel),
            "Esc must not interrupt in {}",
            scope.id()
        );
    }
    assert_eq!(
        keymap.resolve(
            &[vibex_tui::keymap::Scope::Global],
            vibex_tui::keymap::Chord::ctrl('c')
        ),
        Some(vibex_tui::action::Intent::ContextualCancel)
    );
}

#[test]
fn the_session_view_keeps_the_global_escape_hatches() {
    // The composer owns the keyboard inside a session, and a composer-scope
    // binding wins over the global one. Two chords must never be taken by it,
    // because they are the ways *out*: the command palette and quitting. A
    // reader who could type but not leave reported both as dead keys.
    let keymap = vibex_tui::keymap::Keymap::built_in();
    use vibex_tui::keymap::Scope;
    let scopes = [Scope::Composer, Scope::Agent, Scope::Global];
    assert_eq!(
        keymap.resolve(&scopes, vibex_tui::keymap::Chord::ctrl('p')),
        Some(vibex_tui::action::Intent::OpenCommandPalette),
        "Ctrl+P no longer opens the command palette inside a session"
    );
    assert_eq!(
        keymap.resolve(&scopes, vibex_tui::keymap::Chord::ctrl('q')),
        Some(vibex_tui::action::Intent::RequestQuit),
        "Ctrl+Q no longer asks to quit inside a session"
    );
    assert_eq!(
        keymap.resolve(
            &scopes,
            vibex_tui::keymap::Chord::plain(crossterm::event::KeyCode::Esc)
        ),
        Some(vibex_tui::action::Intent::Back),
        "Esc no longer walks back out of a session"
    );
    // The runtime switcher is the Agent and model entry point, so it has to be
    // reachable from the composer too.
    assert_eq!(
        keymap.resolve(&scopes, vibex_tui::keymap::Chord::ctrl('g')),
        Some(vibex_tui::action::Intent::SwitchAgentRuntime)
    );
    // Completion navigation lives on the Alt layer for exactly that reason.
    assert_eq!(
        keymap.resolve(
            &scopes,
            vibex_tui::keymap::Chord::new(
                crossterm::event::KeyCode::Up,
                crossterm::event::KeyModifiers::ALT
            )
        ),
        Some(vibex_tui::action::Intent::CompletionPrevious)
    );
    assert_eq!(
        keymap.resolve(
            &scopes,
            vibex_tui::keymap::Chord::new(
                crossterm::event::KeyCode::Down,
                crossterm::event::KeyModifiers::ALT
            )
        ),
        Some(vibex_tui::action::Intent::CompletionNext)
    );
}

#[test]
fn the_user_key_file_uses_the_documented_path() {
    // The path is part of the product contract in `settings_keys_hint`.
    let strings = vibex_tui::Strings::for_locale(vibex_tui::Locale::En);
    assert!(strings.settings_keys_hint().contains("tui-keys.toml"));
}

#[test]
fn every_locale_key_is_translated_in_all_three_languages() {
    use vibex_tui::locale::Locale;
    let en = vibex_tui::Strings::for_locale(Locale::En);
    let cn = vibex_tui::Strings::for_locale(Locale::ZhCn);
    let tw = vibex_tui::Strings::for_locale(Locale::ZhTw);
    // Spot-check a representative slice from every area of the table. Because
    // the accessors are macro-generated from one declaration, a missing
    // translation is a compile error; this asserts the copy is real.
    type Copy = (fn(&vibex_tui::Strings) -> &'static str, &'static str);
    let pairs: [Copy; 6] = [
        (|s| s.nav_sessions(), "sessions"),
        (|s| s.approval_title(), "approval"),
        (|s| s.elicitation_title(), "elicitation"),
        (|s| s.devices_title(), "devices"),
        (|s| s.usage_title(), "usage"),
        (|s| s.recovery_title(), "recovery"),
    ];
    for (accessor, label) in pairs {
        assert!(!accessor(&en).is_empty(), "{label} missing in en");
        assert!(!accessor(&cn).is_empty(), "{label} missing in zh-CN");
        assert!(!accessor(&tw).is_empty(), "{label} missing in zh-TW");
        assert_ne!(accessor(&en), accessor(&cn), "{label} is untranslated");
    }
}

#[test]
fn no_rendered_string_contains_a_secret_shaped_token() {
    // The copy must never promise to show a stored credential.
    for locale in [
        vibex_tui::Locale::En,
        vibex_tui::Locale::ZhCn,
        vibex_tui::Locale::ZhTw,
    ] {
        let strings = vibex_tui::Strings::for_locale(locale);
        let text = [
            strings.management_secret_write_only(),
            strings.management_provider_secret_mutate_hint(),
            strings.devices_code_hint(),
        ]
        .join(" ");
        for forbidden in ["api_key", "api key", "password is", "sk-"] {
            assert!(
                !text.to_lowercase().contains(forbidden),
                "{locale:?} copy mentions {forbidden}: {text}"
            );
        }
    }
}

#[test]
fn effect_keys_are_stable_identifiers() {
    use vibex_tui::app::Effect;
    for effect in [
        Effect::ListSessions {
            include_archived: false,
        },
        Effect::ListDevices,
        Effect::LoadUsage,
        Effect::RefreshTimeline,
    ] {
        let key = effect.key();
        assert!(!key.is_empty());
        assert!(
            key.chars()
                .all(|character| character.is_ascii_lowercase() || character == '_'),
            "{key} is not a stable identifier"
        );
    }
}

#[test]
fn the_documentation_exists_for_every_page() {
    // A shipped page must have a user-facing document, so a feature cannot
    // arrive without something that explains it.
    let root = workspace_root();
    let docs = root.join("docs").join("tui");
    assert!(
        docs.join("README.md").exists(),
        "docs/tui/README.md must document the client"
    );
    let text = read(&docs.join("README.md"));
    for page in [
        "Sessions",
        "Agent",
        "Files",
        "Changes",
        "Management",
        "Devices",
        "Usage",
        "Recovery",
        "Settings",
        "Help",
    ] {
        assert!(text.contains(page), "docs/tui/README.md omits {page}");
    }
}

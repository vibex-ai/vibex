//! The checked-in `theme.json` must match the shared design token source.
//!
//! `nsh` reads the theme at build and packaging time, so a token change on the
//! desktop side has to regenerate this file; this test is the tripwire, and its
//! message names the command that fixes it.

use std::{fs, path::Path};

use serde_json::Value;
use vibex_mobile::theme_gen::{self, THEME_FILE, TOKEN_SOURCE};

fn manifest() -> &'static Path {
    Path::new(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn theme_file_matches_the_token_source() {
    let tokens = fs::read_to_string(manifest().join(TOKEN_SOURCE)).expect("read the token source");
    let generated = theme_gen::generate(&tokens).expect("generate the theme");
    let checked_in =
        fs::read_to_string(manifest().join(THEME_FILE)).expect("read the checked-in theme");
    assert_eq!(
        generated, checked_in,
        "theme.json is stale; run `cargo run -p vibex-mobile --bin gen-theme`"
    );
}

#[test]
fn both_appearances_carry_every_token() {
    let theme: Value =
        serde_json::from_str(&fs::read_to_string(manifest().join(THEME_FILE)).unwrap()).unwrap();
    let css_vars = &theme["cssVars"];
    let light = css_vars["light"].as_object().expect("a light section");
    let dark = css_vars["dark"].as_object().expect("a dark section");
    assert_eq!(light.len(), dark.len());
    assert!(light.len() > 40, "the token source loses names");
    for name in light.keys() {
        assert!(
            dark.contains_key(name),
            "`{name}` is only in the light theme"
        );
    }
    assert_eq!(css_vars["theme"]["radius"], "8px");
    assert_ne!(light["background"], dark["background"]);
}

#[test]
fn nsh_parses_the_generated_theme() {
    // The real parser, not a shape check: a value `nsh` cannot read would ship
    // as a warning and a missing token, and the page would draw wrong.
    let json = fs::read_to_string(manifest().join(THEME_FILE)).unwrap();
    let tokens = nsh_base::tokens_of(Some(&json)).expect("nsh reads the generated theme");
    let background = tokens.color("background").expect("`background` parses");
    assert_ne!(
        background.light, background.dark,
        "both appearances resolve"
    );
    assert!(
        tokens.color("border").is_some(),
        "a translucent token parses"
    );
    assert!(
        tokens.color("sidebar-border").is_some(),
        "a custom token parses"
    );
    assert_eq!(tokens.number("radius"), Some(8.0));
}

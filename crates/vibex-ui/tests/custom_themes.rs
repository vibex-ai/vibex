//! End-to-end coverage for user theme files.
//!
//! This lives in its own test binary on purpose: installing user themes is a
//! once-per-process operation, so exercising it inside the library's unit
//! tests would leak the installed themes into every other test in that binary.

use vibex_desktop_model::ThemeSelection;
use vibex_ui::{
    CustomThemeInstallError, GpuiThemeMode, default_theme, install_custom_themes, load_str,
    resolve_selection, resolve_theme, semantic_token, theme_at, theme_for, theme_index, themes_for,
};

// A stronger raw-string delimiter is required: the document's hex colors
// contain the `"#` sequence that would close an `r#"…"#` literal.
const THEME_FILE: &str = r##"{
  "schemaVersion": "vibex-theme-file.v1",
  "name": "Test themes",
  "themes": [
    {
      "id": "test-midnight",
      "name": "Test Midnight",
      "mode": "dark",
      "semanticColors": { "background": "#0b0d16", "foreground": "#e4e7f2" }
    },
    {
      "id": "vibex-light",
      "name": "Shadowing Light",
      "mode": "light",
      "semanticColors": { "background": "#fffdf7" }
    },
    {
      "id": "broken",
      "mode": "sepia"
    }
  ]
}"##;

#[test]
fn user_themes_join_the_catalog_and_can_shadow_a_built_in() {
    let (themes, errors) = load_str(THEME_FILE).expect("the file itself must parse");
    // `broken` is reported without hiding its valid siblings.
    assert_eq!(themes.len(), 2);
    assert_eq!(errors.len(), 1);

    let installed = themes.len();
    install_custom_themes(themes).expect("first install wins");
    assert_eq!(
        install_custom_themes(Vec::new()),
        Err(CustomThemeInstallError::AlreadyInstalled),
        "installing twice would hand out references that never take effect"
    );

    // A new id is visible to every lookup the renderers use.
    let midnight = theme_for("test-midnight", GpuiThemeMode::Dark).expect("custom dark theme");
    assert_eq!(midnight.name, "Test Midnight");
    assert_eq!(
        midnight.tokens.len(),
        default_theme(GpuiThemeMode::Dark).tokens.len()
    );
    assert_eq!(
        semantic_token(midnight, "background").unwrap().hex,
        "#0b0d16"
    );
    assert!(
        theme_index("test-midnight", GpuiThemeMode::Dark).is_some(),
        "a cached catalog position must resolve"
    );

    // The catalog grew by the genuinely new theme only: the shadowing light
    // entry replaces its built-in rather than appearing beside it.
    let dark_ids: Vec<&str> = themes_for(GpuiThemeMode::Dark)
        .map(|theme| theme.id)
        .collect();
    assert!(dark_ids.contains(&"test-midnight"));
    assert_eq!(
        dark_ids.iter().filter(|id| **id == "test-midnight").count(),
        1
    );

    let light_ids: Vec<&str> = themes_for(GpuiThemeMode::Light)
        .map(|theme| theme.id)
        .collect();
    assert_eq!(
        light_ids.iter().filter(|id| **id == "vibex-light").count(),
        1,
        "a shadowed built-in must not also be listed"
    );

    // Catalog positions address the same theme they were taken from. The
    // position is global to the catalog, not per-appearance, which is what
    // lets the renderer cache one number per appearance slot.
    for theme in themes_for(GpuiThemeMode::Light) {
        let index =
            theme_index(theme.id, GpuiThemeMode::Light).expect("a listed theme has a position");
        assert_eq!(
            theme_at(index).map(|found| found.id),
            Some(theme.id),
            "position {index} must round-trip"
        );
    }

    // Installing user themes must not repoint a position the renderer may
    // already have cached: built-ins keep their slots, a shadowing theme takes
    // the slot of the theme it replaces, and new themes append.
    assert_eq!(
        theme_index("vibex-dark", GpuiThemeMode::Dark),
        Some(5),
        "built-ins keep their original positions"
    );
    assert_eq!(
        theme_index("vibex-light", GpuiThemeMode::Light),
        Some(0),
        "a shadowing theme takes the slot of the one it replaces"
    );
    let midnight_index = theme_index("test-midnight", GpuiThemeMode::Dark).unwrap();
    assert!(
        midnight_index >= 10,
        "a genuinely new theme appends after the built-ins, got {midnight_index}"
    );

    // A custom theme does not appear under the opposite appearance...
    assert!(theme_for("test-midnight", GpuiThemeMode::Light).is_none());
    // ...and a light id that shadows a built-in replaces it.
    assert_eq!(
        theme_for("vibex-light", GpuiThemeMode::Light).unwrap().name,
        "Shadowing Light"
    );

    // Selection resolves through the custom entry, and the untouched slot still
    // lands on the built-in default.
    let mut selection = ThemeSelection::default();
    selection.select_dark("test-midnight");
    assert_eq!(
        resolve_selection(&selection, GpuiThemeMode::Dark).id,
        "test-midnight"
    );
    assert_eq!(
        resolve_selection(&selection, GpuiThemeMode::Light).id,
        "vibex-light"
    );

    // An unknown id still falls back rather than failing the frame.
    assert_eq!(
        resolve_theme(Some("never-installed"), GpuiThemeMode::Dark).id,
        "vibex-dark"
    );

    assert!(installed >= 2);
}

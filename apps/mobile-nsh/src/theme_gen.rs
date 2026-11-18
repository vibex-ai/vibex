//! Generates the app's `theme.json` from the product's single token source.
//!
//! `crates/vibex-ui/theme/tokens.json` is the design token source the desktop
//! and the GPUI mobile client already draw with (`generated_tokens.rs`). `nsh`
//! reads a theme file in shadcn's `cssVars` shape and resolves it per
//! appearance, so this module maps the source's two default themes
//! (`vibex-light` and `vibex-dark`) into that shape. The result is checked in
//! as `theme.json` and `nsh.json` points at it; a test regenerates it and fails
//! when the token source moved without the file being refreshed.
//!
//! Only the palette comes from here. The phone's spacing, radius and type
//! ladder live in `vibex_nsh_ui::theme`, because the token source's control
//! radius (6) is the desktop's while the phone draws controls at 8.
//!
//! Regenerate with:
//!
//! ```sh
//! cargo run -p vibex-mobile-nsh --bin gen-theme
//! ```

use serde_json::{Map, Value, json};

/// The token source, relative to this crate's manifest directory.
pub const TOKEN_SOURCE: &str = "../../crates/vibex-ui/theme/tokens.json";

/// The generated theme file, relative to this crate's manifest directory.
pub const THEME_FILE: &str = "theme.json";

/// The phone's control radius, as a CSS length for the theme file.
///
/// The shared source has the desktop's 6-point control radius; the phone's
/// GPUI client draws its controls at 8 (`RADIUS_CONTROL`), and the migrated
/// screens keep that value until the two clients are reconciled.
const RADIUS: &str = "8px";

/// The name `nsh` shows for the theme.
const NAME: &str = "Vibex";

/// Reads the token source's default light and dark themes and writes the
/// `cssVars` theme file `nsh.json`'s `"theme"` names.
pub fn generate(tokens_json: &str) -> Result<String, String> {
    let tokens: Value =
        serde_json::from_str(tokens_json).map_err(|error| format!("token source: {error}"))?;
    let default = tokens
        .get("defaultTheme")
        .and_then(Value::as_object)
        .ok_or("token source: no `defaultTheme`")?;
    let light_id = default
        .get("light")
        .and_then(Value::as_str)
        .ok_or("token source: no `defaultTheme.light`")?;
    let dark_id = default
        .get("dark")
        .and_then(Value::as_str)
        .ok_or("token source: no `defaultTheme.dark`")?;
    let themes = tokens
        .get("themes")
        .and_then(Value::as_object)
        .ok_or("token source: no `themes`")?;
    let light = themes
        .get(light_id)
        .ok_or_else(|| format!("token source: theme `{light_id}` is missing"))?;
    let dark = themes
        .get(dark_id)
        .ok_or_else(|| format!("token source: theme `{dark_id}` is missing"))?;

    let mut css_vars = Map::new();
    css_vars.insert("theme".to_owned(), json!({ "radius": RADIUS }));
    css_vars.insert("light".to_owned(), Value::Object(colors(light)?));
    css_vars.insert("dark".to_owned(), Value::Object(colors(dark)?));

    let theme = json!({
        "name": NAME,
        "cssVars": Value::Object(css_vars),
    });
    let mut output =
        serde_json::to_string_pretty(&theme).map_err(|error| format!("theme file: {error}"))?;
    output.push('\n');
    Ok(output)
}

/// One theme's semantic colors as CSS values.
fn colors(theme: &Value) -> Result<Map<String, Value>, String> {
    let theme_name = theme.get("name").and_then(Value::as_str).unwrap_or("theme");
    let colors = theme
        .get("semanticColors")
        .and_then(Value::as_object)
        .ok_or_else(|| format!("theme `{theme_name}`: no `semanticColors`"))?;
    let mut vars = Map::new();
    for (name, value) in colors {
        let value = value
            .as_str()
            .ok_or_else(|| format!("theme `{theme_name}`: `{name}` is not a string"))?;
        vars.insert(name.clone(), Value::String(css_color(value)));
    }
    Ok(vars)
}

/// A token's stored oklch components as a CSS color.
///
/// The source stores what shadcn's tokens have: `1 0 0`, or `0 0 0 / 10%` for
/// a translucent one. `nsh` parses CSS colors, so the components are wrapped
/// in `oklch()` (and left alone when a future source stores a full color).
fn css_color(value: &str) -> String {
    let value = value.trim();
    if value.starts_with("oklch(") || value.starts_with('#') || value.starts_with("rgb") {
        value.to_owned()
    } else {
        format!("oklch({value})")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wraps_oklch_components() {
        assert_eq!(css_color("0.5 0.01 285.9"), "oklch(0.5 0.01 285.9)");
        assert_eq!(css_color("0 0 0 / 10%"), "oklch(0 0 0 / 10%)");
        assert_eq!(css_color("#ffffff"), "#ffffff");
        assert_eq!(css_color("oklch(0.2 0 0)"), "oklch(0.2 0 0)");
    }

    #[test]
    fn rejects_a_source_without_themes() {
        assert!(generate("{}").is_err());
    }
}

//! User theme files.
//!
//! A theme file is a small JSON document a user can drop into the app's theme
//! directory to add palettes to the pickers. It compiles into the same
//! [`GpuiThemeDefinition`] the built-in catalog uses, so nothing downstream —
//! renderers, pickers, persistence — needs to know where a theme came from.
//!
//! # Partial palettes
//!
//! Authoring all 67 semantic roles by hand would make the format useless, so a
//! file only names the roles it wants to change. Compilation starts from the
//! built-in default for the entry's appearance and layers the authored roles on
//! top; every `*-foreground` role the author left out is then re-derived
//! against the surface it is painted on. A file that names nothing but
//! `background` and `foreground` still yields a complete, contrast-checked
//! palette.
//!
//! # Failure policy
//!
//! Nothing here panics on user input. A file that does not compile is reported
//! as a [`ThemeFileError`] and skipped; the rest of the file's entries and
//! every other file still load.

use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{GpuiColorToken, GpuiThemeDefinition, GpuiThemeMode, default_theme};

/// Schema marker for the theme file format.
pub const THEME_FILE_SCHEMA_VERSION: &str = "vibex-theme-file.v1";

/// Directory, relative to the app home, that theme files are read from.
pub const THEME_DIRECTORY: &str = "themes";

/// File extension theme files are read from.
pub const THEME_FILE_EXTENSION: &str = "json";

/// The theme directory beneath `home`.
pub fn theme_directory(home: &Path) -> PathBuf {
    home.join(THEME_DIRECTORY)
}

/// A theme file on disk.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeFile {
    pub schema_version: String,
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub author: Option<String>,
    pub themes: Vec<ThemeFileEntry>,
}

/// One palette inside a theme file.
///
/// Every field defaults so that a malformed entry is reported as its own
/// [`ThemeFileError`] instead of failing deserialization for the whole
/// document — one bad entry must not hide its valid siblings.
#[derive(Debug, Clone, Deserialize, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ThemeFileEntry {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub mode: String,
    /// Authored roles. Names are the same semantic roles the built-in token
    /// source uses; values are CSS hex (`#rrggbb`, `#rrggbbaa`), hex with an
    /// alpha suffix (`#rrggbb / 12%`), or OKLCH (`0.62 0.205 255`, optionally
    /// ` / 12%`).
    #[serde(default)]
    pub semantic_colors: BTreeMap<String, String>,
    /// Optional syntax highlight block, in the same shape the built-in token
    /// source uses. Omitted, the appearance's built-in highlight block is kept.
    #[serde(default)]
    pub syntax_highlight: Option<serde_json::Value>,
}

/// Why a theme file or one of its entries was rejected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ThemeFileError {
    /// The document is not valid JSON.
    Json(String),
    /// The document's schema marker is missing or unknown.
    SchemaVersion { found: String },
    /// The document declares no themes.
    Empty,
    /// An entry id is empty or not a kebab-case slug.
    InvalidId(String),
    /// An entry name is empty.
    InvalidName(String),
    /// An entry mode is neither `light` nor `dark`.
    InvalidMode(String),
    /// A color value could not be parsed.
    InvalidColor { role: String, value: String },
    /// An authored role is not part of the semantic contract.
    UnknownRole(String),
    /// The highlight block is not usable.
    InvalidHighlight(String),
    /// Two entries in one document share an id.
    DuplicateId(String),
    /// The file could not be read.
    Io(String),
}

impl ThemeFileError {
    /// Stable identifier for logs and diagnostics.
    pub fn stable_code(&self) -> &'static str {
        match self {
            Self::Json(_) => "theme_file/json_invalid",
            Self::SchemaVersion { .. } => "theme_file/schema_unsupported",
            Self::Empty => "theme_file/no_themes",
            Self::InvalidId(_) => "theme_file/id_invalid",
            Self::InvalidName(_) => "theme_file/name_invalid",
            Self::InvalidMode(_) => "theme_file/mode_invalid",
            Self::InvalidColor { .. } => "theme_file/color_invalid",
            Self::UnknownRole(_) => "theme_file/role_unknown",
            Self::InvalidHighlight(_) => "theme_file/highlight_invalid",
            Self::DuplicateId(_) => "theme_file/id_duplicate",
            Self::Io(_) => "theme_file/io",
        }
    }
}

impl fmt::Display for ThemeFileError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(message) => write!(formatter, "theme file is not valid JSON: {message}"),
            Self::SchemaVersion { found } => write!(
                formatter,
                "theme file schema must be {THEME_FILE_SCHEMA_VERSION}, found {found:?}"
            ),
            Self::Empty => write!(formatter, "theme file declares no themes"),
            Self::InvalidId(id) => write!(formatter, "theme id {id:?} must be a kebab-case slug"),
            Self::InvalidName(name) => write!(formatter, "theme name {name:?} must not be empty"),
            Self::InvalidMode(mode) => {
                write!(formatter, "theme mode {mode:?} must be light or dark")
            }
            Self::InvalidColor { role, value } => {
                write!(
                    formatter,
                    "role {role:?} has an unsupported color {value:?}"
                )
            }
            Self::UnknownRole(role) => write!(formatter, "role {role:?} is not a semantic role"),
            Self::InvalidHighlight(message) => write!(formatter, "syntax highlight: {message}"),
            Self::DuplicateId(id) => write!(formatter, "theme id {id:?} is declared twice"),
            Self::Io(message) => write!(formatter, "theme file could not be read: {message}"),
        }
    }
}

impl std::error::Error for ThemeFileError {}

/// A straight sRGB color with an alpha channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rgba {
    r: u8,
    g: u8,
    b: u8,
    a: u8,
}

impl Rgba {
    const WHITE: Self = Self::rgb(255, 255, 255);
    const BLACK: Self = Self::rgb(0, 0, 0);

    const fn rgb(r: u8, g: u8, b: u8) -> Self {
        Self { r, g, b, a: 255 }
    }

    fn to_hex(self) -> String {
        if self.a == 255 {
            format!("#{:02x}{:02x}{:02x}", self.r, self.g, self.b)
        } else {
            format!("#{:02x}{:02x}{:02x}{:02x}", self.r, self.g, self.b, self.a)
        }
    }

    fn packed(self) -> u32 {
        (u32::from(self.r) << 16) | (u32::from(self.g) << 8) | u32::from(self.b)
    }

    fn alpha(self) -> f32 {
        f32::from(self.a) / 255.0
    }

    fn linear(self, channel: u8) -> f32 {
        let value = f32::from(channel) / 255.0;
        if value <= 0.04045 {
            value / 12.92
        } else {
            ((value + 0.055) / 1.055).powf(2.4)
        }
    }

    /// WCAG relative luminance of the opaque part.
    fn luminance(self) -> f32 {
        0.2126 * self.linear(self.r) + 0.7152 * self.linear(self.g) + 0.0722 * self.linear(self.b)
    }

    fn opaque(self) -> Self {
        Self { a: 255, ..self }
    }

    fn with_alpha(self, alpha: u8) -> Self {
        Self { a: alpha, ..self }
    }

    fn mix(self, other: Self, amount: f32) -> Self {
        let amount = amount.clamp(0.0, 1.0);
        let blend = |first: u8, second: u8| {
            (f32::from(first) + (f32::from(second) - f32::from(first)) * amount).round() as u8
        };
        Self {
            r: blend(self.r, other.r),
            g: blend(self.g, other.g),
            b: blend(self.b, other.b),
            a: blend(self.a, other.a),
        }
    }
}

fn contrast(first: Rgba, second: Rgba) -> f32 {
    let (a, b) = (first.luminance(), second.luminance());
    (a.max(b) + 0.05) / (a.min(b) + 0.05)
}

fn best_on(background: Rgba) -> Rgba {
    if contrast(Rgba::WHITE, background) >= contrast(Rgba::BLACK, background) {
        Rgba::WHITE
    } else {
        Rgba::BLACK
    }
}

/// Move `color` toward whichever pole reaches `minimum` contrast on `background`.
fn ensure_contrast(color: Rgba, background: Rgba, minimum: f32) -> Rgba {
    let background = background.opaque();
    let color = color.opaque();
    if contrast(color, background) >= minimum {
        return color;
    }
    let target = best_on(background);
    for step in 1..=20 {
        let candidate = color.mix(target, step as f32 / 20.0);
        if contrast(candidate, background) >= minimum {
            return candidate;
        }
    }
    target
}

fn nibble(byte: u8) -> Option<u8> {
    match byte {
        b'0'..=b'9' => Some(byte - b'0'),
        b'a'..=b'f' => Some(byte - b'a' + 10),
        b'A'..=b'F' => Some(byte - b'A' + 10),
        _ => None,
    }
}

fn parse_hex(value: &str) -> Option<Rgba> {
    let digits = value.strip_prefix('#')?;
    let bytes = digits.as_bytes();
    let expand = |byte: u8| nibble(byte).map(|value| (value << 4) | value);
    let pair = |high: u8, low: u8| Some((nibble(high)? << 4) | nibble(low)?);
    match bytes.len() {
        3 => Some(Rgba::rgb(
            expand(bytes[0])?,
            expand(bytes[1])?,
            expand(bytes[2])?,
        )),
        4 => Some(Rgba {
            r: expand(bytes[0])?,
            g: expand(bytes[1])?,
            b: expand(bytes[2])?,
            a: expand(bytes[3])?,
        }),
        6 => Some(Rgba::rgb(
            pair(bytes[0], bytes[1])?,
            pair(bytes[2], bytes[3])?,
            pair(bytes[4], bytes[5])?,
        )),
        8 => Some(Rgba {
            r: pair(bytes[0], bytes[1])?,
            g: pair(bytes[2], bytes[3])?,
            b: pair(bytes[4], bytes[5])?,
            a: pair(bytes[6], bytes[7])?,
        }),
        _ => None,
    }
}

/// Convert OKLCH to sRGB. Mirrors the token generator so a value authored in
/// either place resolves to the same color.
///
/// The transfer constants are f64 on purpose: they carry more significant
/// digits than f32 can hold, and truncating them before the cube would drift
/// the result away from the generator's.
fn parse_oklch(value: &str) -> Option<Rgba> {
    let (body, alpha) = match value.split_once('/') {
        Some((body, alpha)) => {
            let alpha = alpha.trim().strip_suffix('%')?;
            (
                body,
                (alpha.trim().parse::<f64>().ok()? / 100.0).clamp(0.0, 1.0),
            )
        }
        None => (value, 1.0),
    };
    let parts: Vec<f64> = body
        .split_whitespace()
        .map(str::parse::<f64>)
        .collect::<Result<_, _>>()
        .ok()?;
    let [lightness, chroma, hue] = parts[..] else {
        return None;
    };
    if !(0.0..=1.0).contains(&lightness) || chroma < 0.0 {
        return None;
    }
    let radians = hue.to_radians();
    let a = chroma * radians.cos();
    let b = chroma * radians.sin();
    let l_prime = lightness + 0.396_337_777_4 * a + 0.215_803_757_3 * b;
    let m_prime = lightness - 0.105_561_345_8 * a - 0.063_854_172_8 * b;
    let s_prime = lightness - 0.089_484_177_5 * a - 1.291_485_548_0 * b;
    let (l, m, s) = (l_prime.powi(3), m_prime.powi(3), s_prime.powi(3));
    let channels = [
        4.076_741_662_1 * l - 3.307_711_591_3 * m + 0.230_969_929_2 * s,
        -1.268_438_004_6 * l + 2.609_757_401_1 * m - 0.341_319_396_5 * s,
        -0.004_196_086_3 * l - 0.703_418_614_7 * m + 1.707_614_701_0 * s,
    ];
    let encode = |channel: f64| {
        let channel = channel.clamp(0.0, 1.0);
        let encoded = if channel <= 0.0031308 {
            12.92 * channel
        } else {
            1.055 * channel.powf(1.0 / 2.4) - 0.055
        };
        (encoded * 255.0).round() as u8
    };
    Some(Rgba {
        r: encode(channels[0]),
        g: encode(channels[1]),
        b: encode(channels[2]),
        a: (alpha * 255.0).round() as u8,
    })
}

fn parse_color(value: &str) -> Option<Rgba> {
    let value = value.trim();
    // `#rrggbb / 12%` is accepted alongside the 8-digit form so a hex palette
    // can express translucent surfaces the same way the OKLCH spelling does.
    if let Some((body, alpha)) = value.split_once('/') {
        let alpha = alpha.trim().strip_suffix('%')?;
        let alpha = (alpha.trim().parse::<f32>().ok()? / 100.0).clamp(0.0, 1.0);
        let color = parse_color(body.trim())?;
        return Some(color.opaque().with_alpha((alpha * 255.0).round() as u8));
    }
    if value.starts_with('#') {
        parse_hex(value)
    } else {
        parse_oklch(value)
    }
}

/// The surface each `*-foreground` role is painted on, used to re-derive the
/// roles an author left out.
fn foreground_surface(role: &str) -> Option<&'static str> {
    Some(match role {
        "foreground" => "background",
        "card-foreground" => "card",
        "popover-foreground" => "popover",
        // Secondary copy is read against the page, not against the chip it sits
        // beside, so it is checked against the background.
        "muted-foreground" => "background",
        "secondary-foreground" => "secondary",
        "accent-foreground" => "accent",
        "primary-foreground" => "primary",
        "warning-foreground" => "warning",
        "sidebar-foreground" => "sidebar",
        "sidebar-primary-foreground" => "sidebar-primary",
        "sidebar-accent-foreground" => "sidebar-accent",
        _ => return None,
    })
}

fn to_token(name: &str, color: Rgba, authored: Option<&str>) -> GpuiColorToken {
    // The authored spelling is kept when it was OKLCH so a round-trip through
    // the picker shows what the file said; hex input is normalized.
    let oklch = authored
        .filter(|value| !value.trim().starts_with('#'))
        .map(str::to_owned)
        .unwrap_or_else(|| color.to_hex());
    GpuiColorToken {
        name: Box::leak(name.to_owned().into_boxed_str()),
        oklch: Box::leak(oklch.into_boxed_str()),
        hex: Box::leak(color.to_hex().into_boxed_str()),
        rgb: color.packed(),
        alpha: color.alpha(),
    }
}

fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.split('-').all(|segment| {
            !segment.is_empty()
                && segment
                    .bytes()
                    .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit())
        })
}

/// Compile one authored entry into a complete theme.
///
/// The result borrows `'static` data because that is the shape the catalog
/// hands to renderers. Compiled strings and token slices are therefore boxed
/// and leaked: installation happens once, the values live for the rest of the
/// process by design, and leaking is what makes that lifetime honest rather
/// than asserted.
pub fn compile_entry(entry: &ThemeFileEntry) -> Result<GpuiThemeDefinition, ThemeFileError> {
    if !valid_id(&entry.id) {
        return Err(ThemeFileError::InvalidId(entry.id.clone()));
    }
    if entry.name.trim().is_empty() {
        return Err(ThemeFileError::InvalidName(entry.name.clone()));
    }
    let mode = match entry.mode.as_str() {
        "light" => GpuiThemeMode::Light,
        "dark" => GpuiThemeMode::Dark,
        other => return Err(ThemeFileError::InvalidMode(other.to_owned())),
    };

    let base = default_theme(mode);
    let mut colors: BTreeMap<&str, Rgba> = BTreeMap::new();
    for token in base.tokens {
        colors.insert(token.name, token_from(token));
    }
    let mut authored: BTreeMap<&str, &str> = BTreeMap::new();

    for (role, value) in &entry.semantic_colors {
        let color = parse_color(value).ok_or_else(|| ThemeFileError::InvalidColor {
            role: role.clone(),
            value: value.clone(),
        })?;
        let known = colors
            .get_mut(role.as_str())
            .ok_or_else(|| ThemeFileError::UnknownRole(role.clone()))?;
        *known = color;
        authored.insert(role.as_str(), value.as_str());
    }

    // Re-derive every foreground the author did not pin, against the surface it
    // is actually painted on. An authored palette therefore cannot ship
    // unreadable text just because it changed a surface.
    for role in base.tokens.iter().map(|token| token.name) {
        if authored.contains_key(role) {
            continue;
        }
        let Some(surface) = foreground_surface(role) else {
            continue;
        };
        let (Some(foreground), Some(background)) =
            (colors.get(role).copied(), colors.get(surface).copied())
        else {
            continue;
        };
        let repaired = ensure_contrast(foreground, background, 4.5);
        colors.insert(role, repaired);
    }

    let tokens: Vec<GpuiColorToken> = base
        .tokens
        .iter()
        .map(|token| {
            let color = colors
                .get(token.name)
                .copied()
                .unwrap_or_else(|| token_from(token));
            to_token(token.name, color, authored.get(token.name).copied())
        })
        .collect();

    let highlight_json = match &entry.syntax_highlight {
        Some(value) => {
            validate_highlight(value)?;
            value.to_string()
        }
        None => base.highlight_json.to_owned(),
    };

    Ok(GpuiThemeDefinition {
        id: Box::leak(entry.id.clone().into_boxed_str()),
        name: Box::leak(entry.name.trim().to_owned().into_boxed_str()),
        mode,
        tokens: Box::leak(tokens.into_boxed_slice()),
        highlight_json: Box::leak(highlight_json.into_boxed_str()),
    })
}

fn token_from(token: &GpuiColorToken) -> Rgba {
    Rgba {
        r: ((token.rgb >> 16) & 0xff) as u8,
        g: ((token.rgb >> 8) & 0xff) as u8,
        b: (token.rgb & 0xff) as u8,
        a: (token.alpha.clamp(0.0, 1.0) * 255.0).round() as u8,
    }
}

/// A highlight block is accepted when it parses as an object carrying the six
/// editor colors the renderer requires as valid colors. Everything else is the
/// highlighter's business; an unknown key is ignored rather than rejected so a
/// file can carry styles a newer build understands.
fn validate_highlight(value: &serde_json::Value) -> Result<(), ThemeFileError> {
    let object = value
        .as_object()
        .ok_or_else(|| ThemeFileError::InvalidHighlight("must be an object".to_owned()))?;
    for key in [
        "editor.foreground",
        "editor.background",
        "editor.active_line.background",
        "editor.line_number",
        "editor.active_line_number",
        "editor.invisible",
    ] {
        let color = object
            .get(key)
            .and_then(serde_json::Value::as_str)
            .ok_or_else(|| ThemeFileError::InvalidHighlight(format!("{key} is missing")))?;
        if parse_color(color).is_none() {
            return Err(ThemeFileError::InvalidHighlight(format!(
                "{key} is not a color"
            )));
        }
    }
    match object.get("syntax") {
        Some(serde_json::Value::Object(syntax)) if !syntax.is_empty() => Ok(()),
        Some(_) => Err(ThemeFileError::InvalidHighlight(
            "syntax must be a non-empty object".to_owned(),
        )),
        None => Err(ThemeFileError::InvalidHighlight(
            "syntax is missing".to_owned(),
        )),
    }
}

/// Compile every entry in a parsed document.
///
/// Entries compile independently: one broken entry is reported without hiding
/// its valid siblings.
pub fn compile_file(file: &ThemeFile) -> (Vec<GpuiThemeDefinition>, Vec<ThemeFileError>) {
    let mut compiled = Vec::new();
    let mut errors = Vec::new();
    let mut seen: Vec<&str> = Vec::new();
    for entry in &file.themes {
        if seen.contains(&entry.id.as_str()) {
            errors.push(ThemeFileError::DuplicateId(entry.id.clone()));
            continue;
        }
        seen.push(&entry.id);
        match compile_entry(entry) {
            Ok(theme) => compiled.push(theme),
            Err(error) => errors.push(error),
        }
    }
    (compiled, errors)
}

/// Parse and compile a theme file's contents.
pub fn load_str(
    contents: &str,
) -> Result<(Vec<GpuiThemeDefinition>, Vec<ThemeFileError>), ThemeFileError> {
    let file: ThemeFile =
        serde_json::from_str(contents).map_err(|error| ThemeFileError::Json(error.to_string()))?;
    if file.schema_version != THEME_FILE_SCHEMA_VERSION {
        return Err(ThemeFileError::SchemaVersion {
            found: file.schema_version,
        });
    }
    if file.themes.is_empty() {
        return Err(ThemeFileError::Empty);
    }
    Ok(compile_file(&file))
}

/// Read and compile one theme file.
pub fn load_path(
    path: &Path,
) -> Result<(Vec<GpuiThemeDefinition>, Vec<ThemeFileError>), ThemeFileError> {
    let contents =
        std::fs::read_to_string(path).map_err(|error| ThemeFileError::Io(error.to_string()))?;
    load_str(&contents)
}

/// Every `*.json` file in `directory`, in stable path order.
///
/// A missing directory is not an error: it is the state before a user has
/// added any themes.
pub fn discover(directory: &Path) -> Vec<PathBuf> {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return Vec::new();
    };
    let mut paths: Vec<PathBuf> = entries
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.is_file()
                && path
                    .extension()
                    .is_some_and(|extension| extension == THEME_FILE_EXTENSION)
        })
        .collect();
    paths.sort();
    paths
}

/// Load every theme file in `directory`, collecting themes and per-file errors.
pub fn load_directory(
    directory: &Path,
) -> (Vec<GpuiThemeDefinition>, Vec<(PathBuf, ThemeFileError)>) {
    let mut themes = Vec::new();
    let mut errors = Vec::new();
    for path in discover(directory) {
        match load_path(&path) {
            Ok((mut compiled, entry_errors)) => {
                themes.append(&mut compiled);
                for error in entry_errors {
                    errors.push((path.clone(), error));
                }
            }
            Err(error) => errors.push((path, error)),
        }
    }
    (themes, errors)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::semantic_token;

    fn entry(id: &str, mode: &str, colors: &[(&str, &str)]) -> ThemeFileEntry {
        ThemeFileEntry {
            id: id.to_owned(),
            name: id.to_owned(),
            mode: mode.to_owned(),
            semantic_colors: colors
                .iter()
                .map(|(role, value)| ((*role).to_owned(), (*value).to_owned()))
                .collect(),
            syntax_highlight: None,
        }
    }

    #[test]
    fn a_minimal_entry_compiles_into_the_full_contract() {
        let compiled = compile_entry(&entry(
            "my-dark",
            "dark",
            &[("background", "#101014"), ("foreground", "#e8e8ea")],
        ))
        .unwrap();

        let base = default_theme(GpuiThemeMode::Dark);
        assert_eq!(compiled.id, "my-dark");
        assert_eq!(compiled.mode, GpuiThemeMode::Dark);
        assert_eq!(compiled.tokens.len(), base.tokens.len());
        assert_eq!(
            semantic_token(&compiled, "background").unwrap().hex,
            "#101014"
        );
        // Unnamed roles keep the appearance's default, so a partial file is
        // still a complete palette.
        assert_eq!(
            semantic_token(&compiled, "warning").unwrap().hex,
            semantic_token(base, "warning").unwrap().hex
        );
        assert_eq!(compiled.highlight_json, base.highlight_json);
    }

    #[test]
    fn authored_colors_accept_hex_and_oklch() {
        let compiled = compile_entry(&entry(
            "mixed",
            "light",
            &[
                ("background", "#ffffff"),
                ("foreground", "0.25 0 0"),
                ("card", "#f4f4f5"),
                ("border", "0 0 0 / 10%"),
            ],
        ))
        .unwrap();

        assert_eq!(semantic_token(&compiled, "card").unwrap().hex, "#f4f4f5");
        assert_eq!(
            semantic_token(&compiled, "foreground").unwrap().hex,
            "#222222"
        );
        assert_eq!(
            semantic_token(&compiled, "foreground").unwrap().oklch,
            "0.25 0 0"
        );
        assert!((semantic_token(&compiled, "border").unwrap().alpha - 0.1).abs() < 0.01);
    }

    #[test]
    fn foregrounds_are_repaired_against_the_surface_they_sit_on() {
        // A dark background with the light default's dark foreground would be
        // unreadable; the repair must not ship it.
        let compiled = compile_entry(&entry(
            "midnight",
            "light",
            &[("background", "#101014"), ("card", "#101014")],
        ))
        .unwrap();

        let background = semantic_token(&compiled, "background").unwrap();
        let foreground = semantic_token(&compiled, "foreground").unwrap();
        let luminance = |token: &GpuiColorToken| {
            let color = token_from(token);
            color.luminance()
        };
        let ratio = {
            let (a, b) = (luminance(&foreground), luminance(&background));
            (a.max(b) + 0.05) / (a.min(b) + 0.05)
        };
        assert!(ratio >= 4.5, "repaired foreground is only {ratio:.2}:1");
    }

    #[test]
    fn an_authored_foreground_is_never_overridden() {
        let compiled = compile_entry(&entry(
            "pinned",
            "dark",
            &[("background", "#101014"), ("foreground", "#ff0000")],
        ))
        .unwrap();
        assert_eq!(
            semantic_token(&compiled, "foreground").unwrap().hex,
            "#ff0000"
        );
    }

    /// Translucent surfaces can be spelled either way; both must agree.
    #[test]
    fn hex_alpha_matches_the_eight_digit_form() {
        let spaced =
            compile_entry(&entry("spaced", "dark", &[("border", "#ffffff / 14%")])).unwrap();
        let packed = compile_entry(&entry("packed", "dark", &[("border", "#ffffff24")])).unwrap();
        let spaced = semantic_token(&spaced, "border").unwrap();
        let packed = semantic_token(&packed, "border").unwrap();
        assert_eq!(spaced.rgb, packed.rgb);
        assert!((spaced.alpha - packed.alpha).abs() < 0.01);
        assert!((spaced.alpha - 0.14).abs() < 0.01);
    }

    #[test]
    fn an_unknown_role_is_rejected() {
        let error = compile_entry(&entry("bad", "dark", &[("not-a-role", "#ffffff")])).unwrap_err();
        assert_eq!(error, ThemeFileError::UnknownRole("not-a-role".to_owned()));
        assert_eq!(error.stable_code(), "theme_file/role_unknown");
    }

    #[test]
    fn malformed_ids_names_modes_and_colors_are_rejected() {
        for (entry, expected) in [
            (
                entry("Not A Slug", "dark", &[]),
                ThemeFileError::InvalidId("Not A Slug".to_owned()),
            ),
            (
                ThemeFileEntry {
                    name: "  ".to_owned(),
                    ..entry("ok", "dark", &[])
                },
                ThemeFileError::InvalidName("  ".to_owned()),
            ),
            (
                entry("ok", "sepia", &[]),
                ThemeFileError::InvalidMode("sepia".to_owned()),
            ),
            (
                entry("ok", "dark", &[("background", "not-a-color")]),
                ThemeFileError::InvalidColor {
                    role: "background".to_owned(),
                    value: "not-a-color".to_owned(),
                },
            ),
        ] {
            assert_eq!(compile_entry(&entry).unwrap_err(), expected);
        }
    }

    #[test]
    fn a_file_compiles_its_entries_independently() {
        let file = ThemeFile {
            schema_version: THEME_FILE_SCHEMA_VERSION.to_owned(),
            name: None,
            author: None,
            themes: vec![
                entry("good-one", "light", &[("background", "#fdfdfd")]),
                entry("broken", "sepia", &[]),
                entry("good-two", "dark", &[("background", "#0b0b0d")]),
            ],
        };
        let (compiled, errors) = compile_file(&file);
        assert_eq!(
            compiled.iter().map(|theme| theme.id).collect::<Vec<_>>(),
            vec!["good-one", "good-two"]
        );
        assert_eq!(errors.len(), 1);
        assert!(matches!(errors[0], ThemeFileError::InvalidMode(_)));
    }

    /// A malformed entry is reported as its own error instead of failing
    /// deserialization for the whole document.
    #[test]
    fn entries_missing_required_fields_do_not_fail_the_document() {
        let contents = r##"{
          "schemaVersion": "vibex-theme-file.v1",
          "themes": [
            { "id": "no-name", "mode": "dark" },
            { "name": "No Id", "mode": "dark" },
            { "id": "fine", "name": "Fine", "mode": "light" }
          ]
        }"##;
        let (compiled, errors) = load_str(contents).expect("the document itself must parse");
        assert_eq!(
            compiled.iter().map(|theme| theme.id).collect::<Vec<_>>(),
            vec!["fine"]
        );
        assert_eq!(
            errors,
            vec![
                ThemeFileError::InvalidName(String::new()),
                ThemeFileError::InvalidId(String::new()),
            ]
        );
    }

    #[test]
    fn duplicate_ids_inside_one_file_are_rejected() {
        let file = ThemeFile {
            schema_version: THEME_FILE_SCHEMA_VERSION.to_owned(),
            name: None,
            author: None,
            themes: vec![entry("same", "light", &[]), entry("same", "dark", &[])],
        };
        let (compiled, errors) = compile_file(&file);
        assert_eq!(compiled.len(), 1);
        assert_eq!(errors, vec![ThemeFileError::DuplicateId("same".to_owned())]);
    }

    #[test]
    fn the_schema_marker_is_enforced() {
        let contents = r#"{"schemaVersion":"vibex-theme-file.v0","themes":[]}"#;
        assert_eq!(
            load_str(contents).unwrap_err(),
            ThemeFileError::SchemaVersion {
                found: "vibex-theme-file.v0".to_owned()
            }
        );
        assert!(matches!(
            load_str("not json").unwrap_err(),
            ThemeFileError::Json(_)
        ));
        assert_eq!(
            load_str(r#"{"schemaVersion":"vibex-theme-file.v1","themes":[]}"#).unwrap_err(),
            ThemeFileError::Empty
        );
    }

    #[test]
    fn a_custom_highlight_block_is_validated_before_it_is_accepted() {
        let mut with_highlight = entry("highlighted", "dark", &[]);
        with_highlight.syntax_highlight = Some(serde_json::json!({"syntax": {}}));
        assert!(matches!(
            compile_entry(&with_highlight).unwrap_err(),
            ThemeFileError::InvalidHighlight(_)
        ));

        let mut complete = entry("highlighted", "dark", &[]);
        complete.syntax_highlight = Some(serde_json::json!({
            "editor.foreground": "#ffffff",
            "editor.background": "#000000",
            "editor.active_line.background": "#111111",
            "editor.line_number": "#888888",
            "editor.active_line_number": "#dddddd",
            "editor.invisible": "#77777766",
            "syntax": { "keyword": { "color": "#88aaff" } }
        }));
        let compiled = compile_entry(&complete).unwrap();
        assert!(compiled.highlight_json.contains("keyword"));
    }

    /// The shipped example is what users copy, so it is kept compiling.
    #[test]
    fn the_shipped_example_file_compiles_cleanly() {
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("theme")
            .join("example-theme-file.json");
        let (themes, errors) = load_path(&path).expect("example theme file must parse");
        assert!(errors.is_empty(), "{errors:#?}");
        assert_eq!(themes.len(), 2);

        let base_dark = default_theme(GpuiThemeMode::Dark);
        let midnight = themes
            .iter()
            .find(|theme| theme.id == "example-midnight")
            .expect("example dark theme");
        assert_eq!(midnight.mode, GpuiThemeMode::Dark);
        assert_eq!(midnight.tokens.len(), base_dark.tokens.len());
        assert_eq!(
            semantic_token(midnight, "background").unwrap().hex,
            "#0b0d16"
        );
        // Roles the example does not name still resolve, from the default.
        assert!(semantic_token(midnight, "right-rail-file-icon-code").is_some());
        assert!(!midnight.highlight_json.is_empty());
    }

    /// A theme directory is read in stable order and ignores everything that is
    /// not a theme file, so a user's notes next to their themes are harmless.
    #[test]
    fn discovery_reads_json_files_in_stable_order() {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(directory.path().join("b.json"), "{}").unwrap();
        std::fs::write(directory.path().join("a.json"), "{}").unwrap();
        std::fs::write(directory.path().join("notes.txt"), "hello").unwrap();
        std::fs::create_dir(directory.path().join("nested.json")).unwrap();

        let names: Vec<String> = discover(directory.path())
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names, vec!["a.json", "b.json"]);

        // A directory that does not exist yet is not an error.
        assert!(discover(&directory.path().join("missing")).is_empty());
    }
}

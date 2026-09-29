//! Semantic design tokens projected onto a terminal colour model.
//!
//! The desktop remains the visual source of truth. This module reads the
//! generated `GpuiColorToken` table (which already carries `rgb`/`alpha`, so no
//! Oklch conversion happens at runtime for built-in themes) and degrades it to
//! whatever the current terminal can actually display.
//!
//! Degradation order, highest fidelity first:
//!
//! ```text
//! truecolor (16.7M) → ansi256 (xterm cube + grey ramp) → ansi16 → none
//! ```
//!
//! `NO_COLOR` wins over everything, then an explicit `VIBEX_TUI_COLOR`, then
//! capability detection, then the truecolor default.

use std::env;

use ratatui::style::{Color, Modifier, Style};
use vibex_ui::{GpuiThemeDefinition, GpuiThemeMode, theme_catalog};

/// How many colours the terminal is willing to accept.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Ansi256,
    Ansi16,
    None,
}

impl ColorMode {
    /// Resolve the colour mode from the environment.
    ///
    /// Precedence is `NO_COLOR` > `VIBEX_TUI_COLOR` > detection > truecolor,
    /// matching the documented contract in the TUI design report.
    pub fn detect() -> Self {
        Self::detect_from(|key| env::var(key).ok())
    }

    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        if lookup("NO_COLOR").is_some_and(|value| !value.is_empty()) {
            return Self::None;
        }
        if let Some(explicit) = lookup("VIBEX_TUI_COLOR") {
            match explicit.trim().to_ascii_lowercase().as_str() {
                "truecolor" | "24bit" | "true" => return Self::TrueColor,
                "ansi256" | "256" => return Self::Ansi256,
                "16" | "ansi16" | "basic" => return Self::Ansi16,
                "none" | "off" | "no" => return Self::None,
                _ => {}
            }
        }
        let colorterm = lookup("COLORTERM").unwrap_or_default().to_ascii_lowercase();
        if colorterm.contains("truecolor") || colorterm.contains("24bit") {
            return Self::TrueColor;
        }
        let term = lookup("TERM").unwrap_or_default().to_ascii_lowercase();
        if term.is_empty() {
            // A dumb or missing TERM cannot be assumed to render colour.
            return Self::Ansi16;
        }
        if term == "dumb" {
            return Self::None;
        }
        if term.contains("256color") {
            return Self::Ansi256;
        }
        Self::Ansi16
    }

    pub const fn label(self) -> &'static str {
        match self {
            Self::TrueColor => "truecolor",
            Self::Ansi256 => "ansi256",
            Self::Ansi16 => "16",
            Self::None => "none",
        }
    }

    /// Resolve a colour, or `None` when the mode forbids colour entirely.
    pub fn color(self, rgb: u32) -> Option<Color> {
        match self {
            Self::None => None,
            Self::TrueColor => Some(Color::Rgb(
                ((rgb >> 16) & 0xff) as u8,
                ((rgb >> 8) & 0xff) as u8,
                (rgb & 0xff) as u8,
            )),
            Self::Ansi256 => Some(Color::Indexed(rgb_to_ansi256(rgb))),
            Self::Ansi16 => Some(rgb_to_ansi16(rgb)),
        }
    }
}

/// The 16 ANSI colours as sRGB triplets, used as the nearest-neighbour target
/// for the lowest colour tier. Values are the conventional xterm palette.
const ANSI16_RGB: [u32; 16] = [
    0x00_00_00, // black
    0x80_00_00, // red
    0x00_80_00, // green
    0x80_80_00, // yellow
    0x00_00_80, // blue
    0x80_00_80, // magenta
    0x00_80_80, // cyan
    0xc0_c0_c0, // white
    0x80_80_80, // bright black
    0xff_00_00, // bright red
    0x00_ff_00, // bright green
    0xff_ff_00, // bright yellow
    0x00_00_ff, // bright blue
    0xff_00_ff, // bright magenta
    0x00_ff_ff, // bright cyan
    0xff_ff_ff, // bright white
];

const ANSI16_COLORS: [Color; 16] = [
    Color::Black,
    Color::Red,
    Color::Green,
    Color::Yellow,
    Color::Blue,
    Color::Magenta,
    Color::Cyan,
    Color::Gray,
    Color::DarkGray,
    Color::LightRed,
    Color::LightGreen,
    Color::LightYellow,
    Color::LightBlue,
    Color::LightMagenta,
    Color::LightCyan,
    Color::White,
];

/// Map an sRGB triplet to the nearest xterm-256 index.
///
/// The 6×6×6 colour cube and the 24-step grey ramp are both considered, because
/// grayscale text (which is most of a terminal UI) lands far more accurately on
/// the ramp than on the cube.
pub fn rgb_to_ansi256(rgb: u32) -> u8 {
    let (r, g, b) = (
        ((rgb >> 16) & 0xff) as i32,
        ((rgb >> 8) & 0xff) as i32,
        (rgb & 0xff) as i32,
    );

    // Index 16 + 36r + 6g + b uses the levels {0,95,135,175,215,255}.
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let nearest_level = |value: i32| -> usize {
        let mut best = 0usize;
        let mut best_distance = i32::MAX;
        for (index, level) in LEVELS.iter().enumerate() {
            let distance = (value - level).abs();
            if distance < best_distance {
                best_distance = distance;
                best = index;
            }
        }
        best
    };
    let (ri, gi, bi) = (nearest_level(r), nearest_level(g), nearest_level(b));
    let cube_index = 16 + 36 * ri + 6 * gi + bi;
    let cube_distance = squared_distance((r, g, b), (LEVELS[ri], LEVELS[gi], LEVELS[bi]));

    // The grey ramp runs 8, 18, 28, ... 238 (index 232..=255).
    let grey_average = (r + g + b) / 3;
    let grey_step = ((grey_average - 8).clamp(0, 238 - 8) + 5) / 10;
    let grey_value = 8 + grey_step * 10;
    let grey_index = 232 + grey_step;
    let grey_distance = squared_distance((r, g, b), (grey_value, grey_value, grey_value));

    // A pure grey is always better served by the ramp.
    let spread = r.max(g).max(b) - r.min(g).min(b);
    if spread <= 8 || grey_distance < cube_distance {
        grey_index as u8
    } else {
        cube_index as u8
    }
}

fn squared_distance(left: (i32, i32, i32), right: (i32, i32, i32)) -> i32 {
    let (dr, dg, db) = (left.0 - right.0, left.1 - right.1, left.2 - right.2);
    dr * dr + dg * dg + db * db
}

/// Map an sRGB triplet to the nearest of the 16 ANSI colours.
pub fn rgb_to_ansi16(rgb: u32) -> Color {
    let mut best = 0usize;
    let mut best_distance = i32::MAX;
    for (index, candidate) in ANSI16_RGB.iter().enumerate() {
        let distance = squared_distance(
            (
                ((rgb >> 16) & 0xff) as i32,
                ((rgb >> 8) & 0xff) as i32,
                (rgb & 0xff) as i32,
            ),
            (
                ((candidate >> 16) & 0xff) as i32,
                ((candidate >> 8) & 0xff) as i32,
                (candidate & 0xff) as i32,
            ),
        );
        if distance < best_distance {
            best_distance = distance;
            best = index;
        }
    }
    ANSI16_COLORS[best]
}

/// Whether the environment can render the box-drawing and geometric glyph set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GlyphMode {
    /// Box drawing, bullets and arrows are available.
    Unicode,
    /// Fall back to `+-|` borders and ASCII markers.
    Ascii,
}

impl GlyphMode {
    pub fn detect() -> Self {
        Self::detect_from(|key| env::var(key).ok())
    }

    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        if let Some(explicit) = lookup("VIBEX_TUI_ICONS") {
            match explicit.trim().to_ascii_lowercase().as_str() {
                "ascii" | "text" => return Self::Ascii,
                "emoji" | "unicode" | "auto" => return Self::Unicode,
                _ => {}
            }
        }
        let locale = lookup("LC_ALL")
            .or_else(|| lookup("LC_CTYPE"))
            .or_else(|| lookup("LANG"));
        // A missing locale does not flip the default: terminals that report no
        // locale at all are overwhelmingly UTF-8 these days.
        match locale {
            Some(value) if !value.to_ascii_lowercase().contains("utf") => Self::Ascii,
            _ => Self::Unicode,
        }
    }
}

/// Colour-capability snapshot shared by every renderer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ColorCapability {
    pub mode: ColorMode,
    pub glyphs: GlyphMode,
}

impl ColorCapability {
    pub fn detect() -> Self {
        Self {
            mode: ColorMode::detect(),
            glyphs: GlyphMode::detect(),
        }
    }

    pub fn color(&self, rgb: u32) -> Option<Color> {
        self.mode.color(rgb)
    }
}

impl Default for ColorCapability {
    fn default() -> Self {
        Self::detect()
    }
}

/// The semantic roles a terminal interface actually needs.
///
/// The full 67-token catalogue exists for pixel rendering; a character grid
/// collapses it to these roles. Anything not listed here is read on demand
/// through [`TuiTheme::token`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeRole {
    pub background: Color,
    pub foreground: Color,
    pub muted_foreground: Color,
    pub border: Color,
    pub focus: Color,
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub surface: Color,
    pub surface_raised: Color,
    pub code_background: Color,
}

impl ThemeRole {
    /// Highest-contrast colour pair available for text on `surface`.
    pub const fn contrast_foreground(self) -> Color {
        self.foreground
    }
}

/// A resolved theme plus the renderer conveniences built on top of it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TuiTheme {
    pub id: &'static str,
    pub name: &'static str,
    pub mode: GpuiThemeMode,
    pub capability: ColorCapability,
    pub roles: ThemeRole,
}

impl TuiTheme {
    /// Resolve a theme id for a mode, degrading to the appearance default for
    /// unknown ids exactly like the desktop catalogue does.
    pub fn resolve(id: Option<&str>, mode: GpuiThemeMode, capability: ColorCapability) -> Self {
        let definition = theme_catalog::resolve_theme(id, mode);
        Self::from_definition(definition, capability)
    }

    pub fn from_definition(
        definition: &'static GpuiThemeDefinition,
        capability: ColorCapability,
    ) -> Self {
        let token = |name: &str, fallback: u32| -> u32 {
            definition
                .tokens
                .iter()
                .find(|token| token.name == name)
                .map(|token| token.rgb)
                .unwrap_or(fallback)
        };
        let dark = definition.mode == GpuiThemeMode::Dark;
        let (default_bg, default_fg) = if dark {
            (0x0b0b0d, 0xebebeb)
        } else {
            (0xffffff, 0x1a1a1a)
        };
        let background_rgb = token("background", default_bg);
        let foreground_rgb = token("foreground", default_fg);
        let color = |rgb: u32| {
            capability
                .color(rgb)
                // With colour disabled the terminal's own default is the only
                // honest answer; `Color::Reset` keeps reverse-video and bold as
                // the remaining carriers of meaning.
                .unwrap_or(Color::Reset)
        };
        let roles = ThemeRole {
            background: color(background_rgb),
            foreground: color(foreground_rgb),
            muted_foreground: color(token("muted-foreground", foreground_rgb)),
            border: color(token("border", foreground_rgb)),
            focus: color(token("ring", foreground_rgb)),
            accent: color(token("primary", foreground_rgb)),
            success: color(token("chart-2", 0x2e9e5b)),
            warning: color(token("warning", 0xd8a123)),
            danger: color(token("destructive", 0xd6453f)),
            surface: color(token("card", background_rgb)),
            surface_raised: color(token("popover", background_rgb)),
            code_background: color(token("muted", background_rgb)),
        };
        Self {
            id: definition.id,
            name: definition.name,
            mode: definition.mode,
            capability,
            roles,
        }
    }

    /// Raw token lookup for callers that need a colour outside the role set.
    pub fn token(&self, name: &str) -> Option<Color> {
        if self.capability.mode == ColorMode::None {
            return None;
        }
        let definition = theme_catalog::theme(self.id)?;
        self.capability
            .color(theme_catalog::semantic_token(definition, name)?.rgb)
    }

    pub fn base(&self) -> Style {
        Style::default()
            .fg(self.roles.foreground)
            .bg(self.roles.background)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.roles.muted_foreground)
    }

    pub fn accent(&self) -> Style {
        Style::default().fg(self.roles.accent)
    }

    pub fn strong(&self) -> Style {
        Style::default()
            .fg(self.roles.foreground)
            .add_modifier(Modifier::BOLD)
    }

    pub fn danger(&self) -> Style {
        Style::default().fg(self.roles.danger)
    }

    pub fn warning(&self) -> Style {
        Style::default().fg(self.roles.warning)
    }

    pub fn success(&self) -> Style {
        Style::default().fg(self.roles.success)
    }

    pub fn border_style(&self) -> Style {
        Style::default().fg(self.roles.border)
    }

    pub fn focus_style(&self) -> Style {
        Style::default()
            .fg(self.roles.focus)
            .add_modifier(Modifier::BOLD)
    }

    pub fn selected(&self) -> Style {
        // Selection survives a colour-less terminal through reverse video.
        Style::default().add_modifier(Modifier::REVERSED)
    }

    pub fn code(&self) -> Style {
        Style::default().bg(self.roles.code_background)
    }

    /// Whether colour is available at all. Renderers must not rely on colour as
    /// the only carrier of meaning regardless of this value.
    pub const fn has_color(&self) -> bool {
        !matches!(self.capability.mode, ColorMode::None)
    }

    pub const fn glyphs(&self) -> GlyphMode {
        self.capability.glyphs
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env_map(pairs: &[(&str, &str)]) -> impl FnMut(&str) -> Option<String> + use<> {
        let owned = pairs
            .iter()
            .map(|(key, value)| (key.to_string(), value.to_string()))
            .collect::<Vec<_>>();
        move |key: &str| {
            owned
                .iter()
                .find(|(candidate, _)| candidate == key)
                .map(|(_, value)| value.clone())
        }
    }

    #[test]
    fn no_color_wins_over_every_hint() {
        let mode = ColorMode::detect_from(env_map(&[
            ("NO_COLOR", "1"),
            ("VIBEX_TUI_COLOR", "truecolor"),
            ("COLORTERM", "truecolor"),
        ]));
        assert_eq!(mode, ColorMode::None);
    }

    #[test]
    fn explicit_override_beats_detection() {
        let mode = ColorMode::detect_from(env_map(&[
            ("VIBEX_TUI_COLOR", "16"),
            ("COLORTERM", "truecolor"),
        ]));
        assert_eq!(mode, ColorMode::Ansi16);
    }

    #[test]
    fn detects_colorterm_and_256_color_terms() {
        assert_eq!(
            ColorMode::detect_from(env_map(&[("COLORTERM", "24bit")])),
            ColorMode::TrueColor
        );
        assert_eq!(
            ColorMode::detect_from(env_map(&[("TERM", "xterm-256color")])),
            ColorMode::Ansi256
        );
        assert_eq!(
            ColorMode::detect_from(env_map(&[("TERM", "dumb")])),
            ColorMode::None
        );
    }

    #[test]
    fn ansi256_maps_grays_onto_the_grey_ramp() {
        assert_eq!(rgb_to_ansi256(0x0008_0808), 232);
        assert_eq!(rgb_to_ansi256(0xee_ee_ee), 255);
        // A saturated red must land in the cube, not the ramp.
        assert!(rgb_to_ansi256(0xff_00_00) < 232);
    }

    #[test]
    fn ansi16_keeps_bright_colors_recognisable() {
        assert_eq!(rgb_to_ansi16(0xff_00_00), Color::LightRed);
        assert_eq!(rgb_to_ansi16(0x00_00_00), Color::Black);
    }

    #[test]
    fn built_in_themes_resolve_to_finite_colors_in_every_mode() {
        for mode in [
            ColorMode::TrueColor,
            ColorMode::Ansi256,
            ColorMode::Ansi16,
            ColorMode::None,
        ] {
            let capability = ColorCapability {
                mode,
                glyphs: GlyphMode::Unicode,
            };
            let theme = TuiTheme::resolve(Some("vibex-dark"), GpuiThemeMode::Dark, capability);
            assert_eq!(theme.id, "vibex-dark");
            if mode == ColorMode::None {
                assert!(!theme.has_color());
                assert_eq!(theme.roles.foreground, Color::Reset);
            } else {
                assert!(theme.has_color());
            }
        }
    }

    #[test]
    fn unknown_theme_falls_back_without_failing() {
        let theme = TuiTheme::resolve(
            Some("does-not-exist"),
            GpuiThemeMode::Light,
            ColorCapability {
                mode: ColorMode::TrueColor,
                glyphs: GlyphMode::Unicode,
            },
        );
        assert_eq!(
            theme.id,
            theme_catalog::default_theme_id(GpuiThemeMode::Light)
        );
    }

    #[test]
    fn every_shipped_theme_degrades_its_key_roles_distinctly() {
        // Mirrors the "ansi256 pin" idea: background, foreground and danger must
        // not collapse onto the same index, which would make approval risk
        // colours invisible.
        let capability = ColorCapability {
            mode: ColorMode::Ansi256,
            glyphs: GlyphMode::Unicode,
        };
        for mode in GpuiThemeMode::ALL {
            for definition in theme_catalog::themes_for(mode) {
                let theme = TuiTheme::from_definition(definition, capability);
                assert_ne!(
                    theme.roles.background, theme.roles.foreground,
                    "{} lost its text contrast",
                    definition.id
                );
                assert_ne!(
                    theme.roles.foreground, theme.roles.danger,
                    "{} lost its danger colour",
                    definition.id
                );
            }
        }
    }

    #[test]
    fn non_utf8_locale_switches_the_glyph_set() {
        assert_eq!(
            GlyphMode::detect_from(env_map(&[("LANG", "C")])),
            GlyphMode::Ascii
        );
        assert_eq!(
            GlyphMode::detect_from(env_map(&[("LANG", "en_US.UTF-8")])),
            GlyphMode::Unicode
        );
        // A missing locale must not flip the default.
        assert_eq!(GlyphMode::detect_from(env_map(&[])), GlyphMode::Unicode);
    }
}

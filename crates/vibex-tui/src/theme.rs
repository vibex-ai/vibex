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

/// Blend two packed sRGB colours. `t` is how far to move from `from` to `to`.
///
/// Used to derive the grey steps and to fade chrome toward the background when
/// a pane loses focus. Fading rather than recolouring is what keeps a blurred
/// pane recognisable: it stays the same hue, only quieter.
pub fn mix_rgb(from: u32, to: u32, t: f32) -> u32 {
    let t = t.clamp(0.0, 1.0);
    let channel = |shift: u32| -> u32 {
        let a = ((from >> shift) & 0xff) as f32;
        let b = ((to >> shift) & 0xff) as f32;
        ((a + (b - a) * t).round() as u32) & 0xff
    };
    (channel(16) << 16) | (channel(8) << 8) | channel(0)
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
/// The pixel catalogue has 67 entries because a window can afford that many
/// distinctions. A character grid needs fewer *hues* but just as much
/// *hierarchy*, and hierarchy is what a flat palette loses: with one muted grey
/// and one accent, every secondary element competes with every other, and the
/// eye has nowhere to rest.
///
/// The roles below are grouped the way the interface uses them:
///
/// * **surfaces** — layered backgrounds, so a composer or a raised row reads as
///   a distinct plane rather than as text on the same canvas;
/// * **rails** — the accent colour for each kind of transcript block, which is
///   how a reader tells a tool call from a thought without reading the label;
/// * **grey** — three steps, because "dim punctuation", "muted body" and
///   "secondary label" are three different jobs;
/// * **semantic** — a command is not an error is not a path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeRole {
    // ---- surfaces -------------------------------------------------------
    pub background: Color,
    /// Slightly lifted from `background`; used for inline panels.
    pub surface: Color,
    /// Raised above `surface`; overlays and the composer sit here.
    pub surface_raised: Color,
    /// Selected or hovered row inside a list.
    pub surface_highlight: Color,
    /// Code and preformatted blocks.
    pub code_background: Color,

    // ---- text -----------------------------------------------------------
    pub foreground: Color,
    /// Body copy one step down from `foreground`.
    pub text_secondary: Color,
    /// Brightest grey: secondary labels that still need to be read.
    pub gray_bright: Color,
    /// The default grey: muted body, collapsed summaries.
    pub gray: Color,
    /// Dimmest: punctuation, separators, chrome.
    pub gray_dim: Color,

    // ---- structure ------------------------------------------------------
    pub border: Color,
    pub border_focused: Color,
    pub focus: Color,

    // ---- rails: one per transcript block kind ---------------------------
    pub accent_user: Color,
    pub accent_agent: Color,
    pub accent_thinking: Color,
    pub accent_tool: Color,
    pub accent_system: Color,
    pub accent_error: Color,
    pub accent_success: Color,
    /// The Agent is working: the running rail animates.
    pub accent_running: Color,
    /// Approval and elicitation surfaces.
    pub accent_attention: Color,

    // ---- semantic -------------------------------------------------------
    pub accent: Color,
    pub success: Color,
    pub warning: Color,
    pub danger: Color,
    pub command: Color,
    pub path: Color,

    // ---- diff -----------------------------------------------------------
    pub diff_insert: Color,
    pub diff_delete: Color,
}

impl ThemeRole {
    /// Highest-contrast colour pair available for text on `surface`.
    pub const fn contrast_foreground(self) -> Color {
        self.foreground
    }
}

/// The transcript rail a block wears.
///
/// Named by intent rather than by colour so a theme change cannot make a
/// thinking block look like an error.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rail {
    User,
    Agent,
    Thinking,
    Tool,
    System,
    Error,
    Success,
    Running,
    Attention,
}

/// Extract the packed sRGB value of a resolved colour.
pub fn color_rgb(color: Color) -> Option<u32> {
    match color {
        Color::Rgb(r, g, b) => Some((u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)),
        _ => None,
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
            // Surfaces step away from the canvas so a plane change is visible
            // without a border. The light themes step *toward* grey instead,
            // which is why these are tokens rather than arithmetic.
            background: color(background_rgb),
            surface: color(token("card", background_rgb)),
            surface_raised: color(token("popover", background_rgb)),
            surface_highlight: color(token("accent", token("secondary", background_rgb))),
            code_background: color(token("muted", background_rgb)),

            foreground: color(foreground_rgb),
            text_secondary: color(token("secondary-foreground", foreground_rgb)),
            // Three greys derived from the one the catalogue ships: the bright
            // step leans toward the foreground, the dim step away from it. A
            // single muted token cannot carry three jobs.
            gray_bright: color(mix_rgb(
                token("muted-foreground", foreground_rgb),
                foreground_rgb,
                0.35,
            )),
            gray: color(token("muted-foreground", foreground_rgb)),
            gray_dim: color(mix_rgb(
                token("muted-foreground", foreground_rgb),
                background_rgb,
                0.45,
            )),

            border: color(token("border", foreground_rgb)),
            // A focused border has to be visible against the surface it wraps,
            // so it uses the ring token rather than a brighter grey.
            border_focused: color(token("ring", foreground_rgb)),
            focus: color(token("ring", foreground_rgb)),

            // Rails. The catalogue has no per-role hues, so these are derived
            // from the chart series, which exist precisely to give a fixed set
            // of distinguishable colours inside one theme.
            accent_user: color(token("primary", foreground_rgb)),
            accent_agent: color(token("foreground", foreground_rgb)),
            accent_thinking: color(token("chart-category-9", 0x8f7fd4)),
            accent_tool: color(token("chart-category-2", 0x5aa6d8)),
            accent_system: color(token("muted-foreground", foreground_rgb)),
            accent_error: color(token("destructive", 0xd6453f)),
            accent_success: color(token("chart-2", 0x2e9e5b)),
            accent_running: color(token("chart-category-1", 0x5b8dee)),
            accent_attention: color(token("warning", 0xd8a123)),

            accent: color(token("primary", foreground_rgb)),
            success: color(token("chart-2", 0x2e9e5b)),
            warning: color(token("warning", 0xd8a123)),
            danger: color(token("destructive", 0xd6453f)),
            command: color(token("chart-category-4", 0xd8a94a)),
            path: color(token("chart-category-6", 0xd88a5a)),

            diff_insert: color(token("right-rail-status-added", 0x3fae6a)),
            diff_delete: color(token("destructive", 0xd6453f)),
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
        Style::default().fg(self.roles.gray)
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

    /// The rail colour for a block kind. One place decides the mapping, so a
    /// new block kind cannot silently inherit the wrong hue.
    pub fn rail(&self, rail: Rail) -> Color {
        match rail {
            Rail::User => self.roles.accent_user,
            Rail::Agent => self.roles.accent_agent,
            Rail::Thinking => self.roles.accent_thinking,
            Rail::Tool => self.roles.accent_tool,
            Rail::System => self.roles.accent_system,
            Rail::Error => self.roles.accent_error,
            Rail::Success => self.roles.accent_success,
            Rail::Running => self.roles.accent_running,
            Rail::Attention => self.roles.accent_attention,
        }
    }

    /// Fade a colour toward the canvas. `weight` is 1.0 for full strength and
    /// 0.0 for invisible.
    ///
    /// Returns the plain resolved colour when the terminal cannot blend
    /// (indexed palettes have no intermediate steps), so callers never have to
    /// branch on the colour mode themselves.
    pub fn fade(&self, color: Color, weight: f32) -> Color {
        if weight >= 1.0 {
            return color;
        }
        if !matches!(self.capability.mode, ColorMode::TrueColor) || !matches!(color, Color::Rgb(..))
        {
            return color;
        }
        let (Some(from), Some(background)) = (color_rgb(color), color_rgb(self.roles.background))
        else {
            return color;
        };
        self.capability
            .color(mix_rgb(from, background, 1.0 - weight))
            .unwrap_or(color)
    }

    /// The style for chrome that belongs to an unfocused pane.
    pub fn dimmed(&self, color: Color) -> Style {
        Style::default().fg(self.fade(color, 0.55))
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

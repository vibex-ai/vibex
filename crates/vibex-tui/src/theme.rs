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
        if term_draws_truecolor(&term) {
            return Self::TrueColor;
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

    /// Whether a blended ramp survives this palette.
    ///
    /// True colour is the only mode whose resolved colours carry their own
    /// channels: an indexed palette hands back an index, and this module has no
    /// table to read one back with. Sixteen colours would also collapse the
    /// ramp into two or three steps, which reads as a flicker rather than as a
    /// fade. Callers that need a gradient in either mode are expected to step
    /// down the theme's own greys and to reach for weight — `BOLD` and `DIM` —
    /// instead of for a blend.
    pub const fn blends(self) -> bool {
        matches!(self, Self::TrueColor)
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

/// Terminal families whose entries draw 24-bit colour by construction.
///
/// `COLORTERM` is the usual hint, but it is an environment variable, and the
/// environments that matter do not all carry it: an SSH session inherits only
/// the names `sshd` is configured to accept, so a client that reports
/// `COLORTERM=truecolor` in a local shell arrives as `TERM` alone. A `TERM`
/// name is a statement about the terminal itself rather than about the shell
/// around it, so it survives the hop — and without it the same client drops to
/// the sixteen-colour tier, where the page loses its own background (see
/// [`ThemeRole::canvas`]).
const TRUECOLOR_TERMINALS: [&str; 8] = [
    "kitty",
    "ghostty",
    "wezterm",
    "foot",
    "contour",
    "alacritty",
    "rio",
    "hyper",
];

/// Whether a terminfo name promises 24-bit colour.
///
/// Matched as a family, so `xterm-kitty`, `foot-extra` and `tmux-ghostty` all
/// answer yes, while an unrelated name that merely contains the letters does
/// not.
fn term_draws_truecolor(term: &str) -> bool {
    // `xterm-direct` and `tmux-direct` are terminfo's own names for 24-bit
    // colour; a name that spells the capability out is taken at its word.
    if term.contains("direct") || term.contains("truecolor") || term.contains("24bit") {
        return true;
    }
    TRUECOLOR_TERMINALS.iter().any(|family| {
        let family = *family;
        term == family
            || term
                .strip_prefix(family)
                .is_some_and(|rest| rest.starts_with('-'))
            || term
                .strip_suffix(family)
                .is_some_and(|rest| rest.ends_with('-'))
    })
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

    /// The icon set the environment demands, when it names one this build
    /// knows.
    ///
    /// Split out from [`GlyphMode::detect_from`] because an explicit choice has
    /// to outrank what the interface remembered, while the locale fallback has
    /// to lose to it: `VIBEX_TUI_ICONS` is this invocation's answer, and the
    /// file is the reader's standing one.
    pub fn explicit_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Option<Self> {
        let explicit = lookup("VIBEX_TUI_ICONS")?;
        match explicit.trim().to_ascii_lowercase().as_str() {
            "ascii" | "text" => Some(Self::Ascii),
            "emoji" | "unicode" | "auto" => Some(Self::Unicode),
            _ => None,
        }
    }

    pub fn detect_from(mut lookup: impl FnMut(&str) -> Option<String>) -> Self {
        if let Some(explicit) = Self::explicit_from(&mut lookup) {
            return explicit;
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
/// * **semantic** — a command is not an error. `path` is the exception that
///   proves the rule: a location is where the reader already is, so it takes the
///   secondary grey rather than a hue, and colour on a status row keeps meaning
///   "something happened".
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThemeRole {
    // ---- surfaces -------------------------------------------------------
    /// The page the whole frame is painted on.
    ///
    /// A role of its own rather than an alias of `background`, because the
    /// sixteen-colour tier cannot paint a canvas the theme chose. `black` is a
    /// palette slot, not a promise of darkness: Catppuccin draws it as a mid
    /// blue-grey and Solarized and Nord as a lifted one, so a page filled with
    /// it reads as a washed-out sheet — and every surface that degrades to the
    /// same slot disappears into it. The canvas therefore falls back to the
    /// terminal's own background, which is by definition the colour its reader
    /// chose to read on. `background` keeps its palette colour, because the
    /// cells that use it as *ink* — text on an accent chip, a selection — sit on
    /// a fill the palette owns.
    pub canvas: Color,
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
    /// Where the reader is. Deliberately not a hue: a path is chrome, and on the
    /// status row colour has to keep meaning a state rather than a location.
    pub path: Color,
    /// A hyperlink's label. Its own colour rather than `accent`, which several
    /// themes resolve to the foreground and would leave a link looking like
    /// ordinary prose with an underline.
    pub link: Color,

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

/// The colours a markdown document is drawn in.
///
/// A document is not a rail: it needs a hue *ladder* for structure (a heading
/// level is a depth, and depth has to be visible at a glance), one colour for
/// literals, and one muted step for the marks that are punctuation rather than
/// content — bullets, rules, quote bars, table borders. Emphasis deliberately
/// has no colour of its own: weight and slant carry it, so colour can keep
/// meaning "this is a different kind of thing".
///
/// The hues come from the catalogue's chart series, which exist precisely to be
/// a fixed set of distinguishable colours inside one theme, so every shipped
/// theme gets the same relationships with its own hues.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MarkdownPalette {
    /// Heading levels 1..=6, brightest first. The last three recede into grey:
    /// a document rarely goes that deep, and when it does the deepest levels
    /// are structure, not emphasis.
    pub heading: [Color; 6],
    /// Inline code and the body of a fenced block: a literal that is a name or
    /// an expression.
    pub code: Color,
    /// A literal that is a count, a version or a measurement.
    pub code_number: Color,
    /// A literal that is a path, a file name or a glob.
    pub code_path: Color,
    /// A token that is neither prose nor program text: a key to press, a
    /// formula. It gets the hue a type name gets, because that is what it is —
    /// a name that belongs to the system rather than to the sentence.
    pub special: Color,
    /// The language label on a fence, which names the literal rather than being
    /// one.
    pub code_language: Color,
    /// A link's label.
    pub link: Color,
    /// The target printed beside a link whose label does not name it.
    pub link_target: Color,
    /// Bullets, ordered markers, footnote references.
    pub marker: Color,
    /// A checked task box.
    pub task_done: Color,
    /// An unchecked task box.
    pub task_todo: Color,
    /// The bar beside a quotation.
    pub quote: Color,
    /// A thematic break.
    pub rule: Color,
    /// A table's borders.
    pub table_border: Color,
}

/// The syntax colours a fenced code block is highlighted with.
///
/// Read once per theme rather than per block: the catalogue ships the palette
/// as JSON, and parsing it inside every render would put a JSON parse on the
/// streaming path.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SyntaxPalette {
    pub keyword: Option<Color>,
    pub string: Option<Color>,
    pub comment: Option<Color>,
    pub number: Option<Color>,
    pub function: Option<Color>,
    pub type_name: Option<Color>,
    /// `text.code.span`: the colour of a literal that is not being highlighted.
    pub code: Option<Color>,
    /// `attribute`: a name that belongs to a system rather than to the program —
    /// a path, a file, a field.
    pub attribute: Option<Color>,
    /// `title`: a symbol definition or a heading inside code.
    pub title: Option<Color>,
}

impl SyntaxPalette {
    /// Read the catalogue's syntax palette out of the theme definition.
    ///
    /// `resolve` maps a packed sRGB value to a colour the terminal can show, so
    /// a palette that cannot be represented degrades with everything else
    /// instead of leaking `Rgb` into a 16-colour terminal.
    pub fn from_json(json: &str, resolve: &impl Fn(u32) -> Color) -> Self {
        let Ok(document) = serde_json::from_str::<serde_json::Value>(json) else {
            return Self::default();
        };
        let Some(syntax) = document.get("syntax") else {
            return Self::default();
        };
        let lookup = |key: &str| -> Option<Color> {
            let value = syntax.get(key)?.get("color")?.as_str()?;
            parse_hex_color(value).map(resolve)
        };
        Self {
            keyword: lookup("keyword"),
            string: lookup("string"),
            comment: lookup("comment"),
            number: lookup("number"),
            function: lookup("function"),
            type_name: lookup("type"),
            code: lookup("text.code.span"),
            attribute: lookup("attribute"),
            title: lookup("title"),
        }
    }
}

/// Parse a `#rrggbb` colour from the token catalogue.
fn parse_hex_color(value: &str) -> Option<u32> {
    let value = value.trim().trim_start_matches('#');
    (value.len() == 6).then(|| u32::from_str_radix(value, 16).ok())?
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
    /// The colours markdown is drawn in.
    pub markdown: MarkdownPalette,
    /// The syntax colours a fenced code block is highlighted with.
    pub syntax: SyntaxPalette,
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
        // The bright step of the grey scale, named once: the status band's
        // location is a secondary label rather than a status of its own, so it
        // wears the same grey.
        let bright_rgb = mix_rgb(
            token("muted-foreground", foreground_rgb),
            foreground_rgb,
            0.35,
        );
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
            //
            // Sixteen colours are the one tier that cannot paint the plane the
            // theme asked for: `black` is whatever the reader's palette says it
            // is, so the page is left to the terminal — the only colour on
            // screen that is guaranteed to be a background — while the palette
            // goes on supplying every colour used as ink.
            canvas: if matches!(capability.mode, ColorMode::Ansi16) {
                Color::Reset
            } else {
                color(background_rgb)
            },
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
            gray_bright: color(bright_rgb),
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
            // Where the reader already is, not a status to notice: the chart
            // series' orange made the location read as a warning — the same
            // family of hue as `danger` — so it wears the bright grey a
            // secondary label gets and lets colour mean "something happened".
            path: color(bright_rgb),
            link: color(token("chart-category-2", 0x5aa6d8)),

            diff_insert: color(token("right-rail-status-added", 0x3fae6a)),
            diff_delete: color(token("destructive", 0xd6453f)),
        };
        // A colour-less terminal gets no syntax palette at all: the highlighter
        // checks for an empty one and skips the work, and `Reset` would
        // otherwise make every token look "coloured".
        let syntax = if matches!(capability.mode, ColorMode::None) {
            SyntaxPalette::default()
        } else {
            SyntaxPalette::from_json(definition.highlight_json, &color)
        };
        // Body copy, so a markdown-only colour never has to reach for `roles`.
        let body_rgb = token("muted-foreground", foreground_rgb);
        let markdown = MarkdownPalette {
            heading: [
                // Three hues, then the greys: a document that nests deeper than
                // three levels is outlining, not emphasising.
                color(token("chart-category-1", 0x5b8dee)),
                color(token("chart-category-3", 0x4bbf9a)),
                color(token("chart-category-9", 0x8f7fd4)),
                color(mix_rgb(body_rgb, foreground_rgb, 0.45)),
                color(body_rgb),
                color(mix_rgb(body_rgb, background_rgb, 0.45)),
            ],
            code: syntax
                .code
                .unwrap_or_else(|| color(token("chart-category-3", 0x4bbf9a))),
            // Numbers and paths are the two other things a sentence is full of
            // once it mentions code, and one colour for all three is what makes
            // a technical paragraph read as one grey block.
            code_number: syntax
                .number
                .unwrap_or_else(|| color(token("chart-category-4", 0xd8a94a))),
            code_path: syntax
                .attribute
                .unwrap_or_else(|| color(token("chart-category-2", 0x5aa6d8))),
            // A theme may paint a type name and a literal the same colour; the
            // palette cannot, or a keycap stops being findable in a sentence.
            special: syntax
                .type_name
                .filter(|colour| Some(*colour) != syntax.code)
                .unwrap_or_else(|| color(token("chart-category-9", 0x99a6f0))),
            code_language: color(mix_rgb(body_rgb, background_rgb, 0.3)),
            link: color(token("chart-category-2", 0x5aa6d8)),
            link_target: color(mix_rgb(body_rgb, background_rgb, 0.3)),
            marker: color(body_rgb),
            task_done: color(token("chart-2", 0x2e9e5b)),
            task_todo: color(body_rgb),
            quote: color(body_rgb),
            rule: color(mix_rgb(body_rgb, background_rgb, 0.3)),
            table_border: color(mix_rgb(body_rgb, background_rgb, 0.2)),
        };
        Self {
            id: definition.id,
            name: definition.name,
            mode: definition.mode,
            capability,
            roles,
            markdown,
            syntax,
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
            .bg(self.roles.canvas)
    }

    pub fn muted(&self) -> Style {
        Style::default().fg(self.roles.gray)
    }

    /// Running prose: one step below `foreground`, so headings, emphasis and
    /// code read as a different kind of thing rather than as more of the same
    /// white. The catalogue's `secondary-foreground` is the foreground in
    /// several themes, which is why this is the bright grey rather than that
    /// token.
    pub fn prose(&self) -> Style {
        Style::default().fg(self.roles.gray_bright)
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
        let (Some(from), Some(background)) = (color_rgb(color), color_rgb(self.roles.canvas))
        else {
            return color;
        };
        self.capability
            .color(mix_rgb(from, background, 1.0 - weight))
            .unwrap_or(color)
    }

    /// Blend one colour toward another. `weight` is 1.0 for `to`, 0.0 for `from`.
    ///
    /// [`Self::fade`] walks a colour toward the canvas, which is what *dimming*
    /// means. A light does the opposite: it has to put something *onto* the
    /// surface it crosses, and the something is a hue rather than an absence of
    /// one, so it needs a second colour to walk toward.
    ///
    /// The guard is [`ColorMode::blends`], the same one `fade` applies, and for
    /// the same reason: a colour that cannot be taken apart cannot be mixed.
    /// Callers that need this to degrade well should give the shallow modes
    /// their own path rather than leaning on the fallback, which is simply the
    /// colour they passed in.
    pub fn blend(&self, from: Color, to: Color, weight: f32) -> Color {
        if weight >= 1.0 {
            return to;
        }
        if weight <= 0.0 || !self.capability.mode.blends() {
            return from;
        }
        let (Some(from_rgb), Some(to_rgb)) = (color_rgb(from), color_rgb(to)) else {
            return from;
        };
        self.capability
            .color(mix_rgb(from_rgb, to_rgb, weight))
            .unwrap_or(from)
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
    fn a_modern_terminal_keeps_its_colour_without_colorterm() {
        // The environment that matters is the one reached over a link:
        // `COLORTERM` is not forwarded unless `sshd` is configured to accept
        // it, so a client that draws 24-bit colour locally arrives as `TERM`
        // alone. Degrading it to sixteen colours there is what paints the page
        // in the palette's `black` — a mid grey on Catppuccin, Solarized and
        // Nord — and turns the whole interface into a washed-out sheet.
        for term in [
            "xterm-kitty",
            "xterm-ghostty",
            "wezterm",
            "foot",
            "foot-extra",
            "contour",
            "alacritty",
            "rio",
            "xterm-direct",
            "tmux-direct",
            "xterm-truecolor",
        ] {
            assert_eq!(
                ColorMode::detect_from(env_map(&[("TERM", term)])),
                ColorMode::TrueColor,
                "{term} was degraded despite drawing 24-bit colour"
            );
        }
    }

    #[test]
    fn a_terminal_that_names_a_limited_palette_is_taken_at_its_word() {
        // The other half of the contract: a name that says eight or sixteen
        // colours must not be promoted by a hint nobody sent.
        for term in [
            "xterm", "screen", "tmux", "linux", "vt100", "ansi", "cons25",
        ] {
            assert_eq!(
                ColorMode::detect_from(env_map(&[("TERM", term)])),
                ColorMode::Ansi16,
                "{term} was promoted past its palette"
            );
        }
        // A family match is a family match, not a substring: a name that only
        // contains the letters is still just a name.
        assert_eq!(
            ColorMode::detect_from(env_map(&[("TERM", "kittyfoot")])),
            ColorMode::Ansi16
        );
    }

    #[test]
    fn the_sixteen_colour_tier_leaves_the_page_to_the_terminal() {
        // Sixteen colours is the tier that cannot paint the canvas the theme
        // chose: `black` is a palette slot, and on the schemes named above it
        // is a mid grey, so the page and every surface collapse onto one
        // washed-out colour. The page therefore belongs to the terminal, while
        // the roles used as ink keep a palette colour of their own — including
        // `background`, which is the ink on an accent chip and would make a
        // selection invisible if it went back to the terminal's default.
        let capability = ColorCapability {
            mode: ColorMode::Ansi16,
            glyphs: GlyphMode::Unicode,
        };
        for mode in GpuiThemeMode::ALL {
            for definition in theme_catalog::themes_for(mode) {
                let theme = TuiTheme::from_definition(definition, capability);
                assert_eq!(
                    theme.roles.canvas,
                    Color::Reset,
                    "{} painted its own page in sixteen colours",
                    definition.id
                );
                assert_eq!(theme.base().bg, Some(Color::Reset));
                assert_ne!(
                    theme.roles.background,
                    Color::Reset,
                    "{} left the ink on its accent chips to the terminal",
                    definition.id
                );
                assert_ne!(
                    theme.roles.foreground, theme.roles.background,
                    "{} lost the contrast of an inverted chip",
                    definition.id
                );
            }
        }
    }

    #[test]
    fn a_faithful_palette_still_paints_the_theme_canvas() {
        // The fallback is scoped to the tier that needs it: with a channel per
        // colour the canvas is the theme's own background again.
        for mode in [ColorMode::TrueColor, ColorMode::Ansi256] {
            let capability = ColorCapability {
                mode,
                glyphs: GlyphMode::Unicode,
            };
            for theme_mode in GpuiThemeMode::ALL {
                for definition in theme_catalog::themes_for(theme_mode) {
                    let theme = TuiTheme::from_definition(definition, capability);
                    assert_eq!(
                        theme.roles.canvas, theme.roles.background,
                        "{} lost its canvas in {mode:?}",
                        definition.id
                    );
                    assert_eq!(theme.base().bg, Some(theme.roles.background));
                }
            }
        }
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
    fn every_shipped_theme_keeps_a_markdown_hue_ladder() {
        // Markdown carries structure with colour: a heading level, a literal,
        // and a link have to stay three different things in every theme, and
        // the top three heading levels have to stay three different *depths*.
        let capability = ColorCapability {
            mode: ColorMode::TrueColor,
            glyphs: GlyphMode::Unicode,
        };
        for mode in GpuiThemeMode::ALL {
            for definition in theme_catalog::themes_for(mode) {
                let theme = TuiTheme::from_definition(definition, capability);
                let markdown = theme.markdown;
                let ladder = markdown.heading[..3]
                    .iter()
                    .map(|colour| format!("{colour:?}"))
                    .collect::<std::collections::BTreeSet<_>>();
                assert_eq!(
                    ladder.len(),
                    3,
                    "{} collapses its heading levels onto fewer colours",
                    definition.id
                );
                for (left, right) in [
                    (markdown.heading[0], markdown.code),
                    (markdown.heading[0], markdown.link),
                    (markdown.code, markdown.link),
                    // A literal is one of three kinds, and the three kinds are
                    // what give a technical paragraph its relief.
                    (markdown.code, markdown.code_number),
                    (markdown.code, markdown.code_path),
                    (markdown.code_number, markdown.code_path),
                    (markdown.code_number, markdown.link),
                    (markdown.special, markdown.link),
                    (markdown.special, markdown.code),
                    (markdown.table_border, markdown.rule),
                ] {
                    assert_ne!(
                        left, right,
                        "{} draws two markdown roles in one colour",
                        definition.id
                    );
                }
            }
        }
    }

    #[test]
    fn a_colour_less_theme_has_no_syntax_palette() {
        // `Reset` is a colour as far as an `Option<Color>` is concerned; the
        // highlighter checks for the empty palette to skip its work entirely.
        let capability = ColorCapability {
            mode: ColorMode::None,
            glyphs: GlyphMode::Ascii,
        };
        let definition = theme_catalog::themes_for(GpuiThemeMode::Dark)
            .next()
            .expect("a shipped dark theme");
        let theme = TuiTheme::from_definition(definition, capability);
        assert_eq!(theme.syntax, SyntaxPalette::default());
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

    #[test]
    fn only_a_named_icon_set_counts_as_explicit() {
        // The explicit choice is the one the environment states, not the one
        // detection inferred from the locale: a remembered choice has to lose
        // to the first and win over the second.
        assert_eq!(
            GlyphMode::explicit_from(env_map(&[("VIBEX_TUI_ICONS", "ascii")])),
            Some(GlyphMode::Ascii)
        );
        assert_eq!(
            GlyphMode::explicit_from(env_map(&[("VIBEX_TUI_ICONS", "emoji")])),
            Some(GlyphMode::Unicode)
        );
        assert_eq!(
            GlyphMode::explicit_from(env_map(&[("LANG", "C")])),
            None,
            "a locale is not an explicit icon set"
        );
        assert_eq!(
            GlyphMode::explicit_from(env_map(&[("VIBEX_TUI_ICONS", "fancy")])),
            None,
            "a value this build does not know is not a choice"
        );
    }
}

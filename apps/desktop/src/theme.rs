use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicU8, AtomicUsize, Ordering};

use crate::typography::apply_code_font_weight;
use gpui::{App, Hsla, Window, WindowAppearance, px};
use gpui_component::{
    Theme, ThemeMode as ComponentThemeMode,
    highlighter::{HighlightTheme, HighlightThemeStyle},
};
use vibex_desktop_model::{AppearanceUiState, ThemeMode, ThemeSelection};
use vibex_ui::{
    CODE_TYPOGRAPHY, GpuiColorToken, GpuiThemeDefinition, GpuiThemeMode, INTERFACE_TYPOGRAPHY,
    RADII, SHADOWS_ENABLED, default_theme, semantic_token as catalog_token, theme_index,
};

use crate::motion::{mix, set_pause_inactive_animation, set_user_reduced_motion};

pub use vibex_ui::{
    TOKEN_PRODUCT_VISUAL_SOURCE, TOKEN_SCHEMA_VERSION, TOKEN_SOURCE_PATH, TOKEN_SOURCE_SHA256,
};

/// What is painted behind the workbench's surfaces.
///
/// The appearance pass maps the same palette either way; the backdrop only
/// decides whether the theme's containers let an image through.
///
/// The theme is a process-wide value that every window paints from, so a window
/// that publishes an appearance has to pass the backdrop the workbench
/// resolved: one that decided differently would repaint every other window with
/// its own choice. A window that never publishes one — the computer panel opens
/// without an appearance pass — simply paints the palette it finds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Backdrop {
    /// The theme's own page color, opaque.
    #[default]
    Theme,
    /// A user image behind the surfaces, so the containers fade to let it show.
    Wallpaper,
}

impl Backdrop {
    /// Whether a user image is painted behind the surfaces.
    pub fn is_wallpaper(self) -> bool {
        matches!(self, Self::Wallpaper)
    }

    /// The backdrop that goes with an appearance whose wallpaper resolves.
    pub fn for_wallpaper(wallpaper: bool) -> Self {
        if wallpaper {
            Self::Wallpaper
        } else {
            Self::Theme
        }
    }
}

/// The palette the renderer is currently painting with, per appearance.
///
/// Colors are read imperatively at paint time through [`semantic_color`],
/// which has no `App` handle to consult, so the active catalog position is
/// mirrored here whenever the selection changes. `UNSET` resolves to the
/// catalog default, which is what the very first frame and any unit test that
/// never installs a theme see.
const UNSET: usize = usize::MAX;

static ACTIVE_LIGHT: AtomicUsize = AtomicUsize::new(UNSET);
static ACTIVE_DARK: AtomicUsize = AtomicUsize::new(UNSET);

/// Whether a wallpaper is painted behind the surfaces.
///
/// Mirrored alongside the catalog position, for the same reason: a container
/// color is read at paint time through [`semantic_color`], which has no `App`
/// handle to consult. The surfaces the workbench paints come from both places —
/// the framework's theme and the shared catalog — and only the catalog path can
/// be resolved here, so this is what keeps a card from staying opaque while the
/// panel behind it lets the image through.
static WALLPAPER: AtomicBool = AtomicBool::new(false);

/// Point the paint-time palette at the backdrop it is drawing over.
///
/// Called by [`apply_appearance`] before it reads any token, next to
/// [`set_active_selection`] and for the same reason.
fn set_wallpaper_backdrop(backdrop: Backdrop) {
    WALLPAPER.store(backdrop.is_wallpaper(), Ordering::Relaxed);
}

fn wallpaper_backdrop() -> bool {
    WALLPAPER.load(Ordering::Relaxed)
}

fn mode_slot(mode: GpuiThemeMode) -> &'static AtomicUsize {
    match mode {
        GpuiThemeMode::Light => &ACTIVE_LIGHT,
        GpuiThemeMode::Dark => &ACTIVE_DARK,
    }
}

fn model_mode(dark: bool) -> GpuiThemeMode {
    if dark {
        GpuiThemeMode::Dark
    } else {
        GpuiThemeMode::Light
    }
}

/// The theme variant the renderer is painting with for `mode`.
pub fn active_theme(mode: GpuiThemeMode) -> &'static GpuiThemeDefinition {
    let index = mode_slot(mode).load(Ordering::Relaxed);
    if index == UNSET {
        return default_theme(mode);
    }
    vibex_ui::theme_at(index).unwrap_or_else(|| default_theme(mode))
}

/// The catalog position a selection resolves to for `mode`, or [`UNSET`].
///
/// Pure, so the mapping is testable without mutating the process-wide slots
/// that other tests read.
fn selection_index(selection: &ThemeSelection, mode: GpuiThemeMode) -> usize {
    let requested = match mode {
        GpuiThemeMode::Light => selection.light(),
        GpuiThemeMode::Dark => selection.dark(),
    };
    // A stale or cross-appearance id resolves to the default here rather than
    // being stored, so `active_theme` never has to correct it.
    requested
        .and_then(|id| theme_index(id, mode))
        .unwrap_or(UNSET)
}

/// Point the paint-time palette at `selection`.
///
/// Called by [`apply_appearance`] before it reads any token, so the whole
/// palette — including the colors the gpui-component theme is built from — is
/// resolved against the same variant.
pub fn set_active_selection(selection: &ThemeSelection) {
    for mode in GpuiThemeMode::ALL {
        mode_slot(mode).store(selection_index(selection, mode), Ordering::Relaxed);
    }
}

fn shared_code_font_family() -> &'static str {
    match CODE_TYPOGRAPHY.family {
        "platform_monospace" => crate::platform::default_code_font_family(),
        unsupported => panic!("unsupported shared GPUI code-font policy: {unsupported}"),
    }
}

/// Load user theme files from `home` into the shared catalog.
///
/// Called once at startup, before the persisted selection is applied, so a
/// saved custom theme resolves on the first frame. A file that does not
/// compile is reported and skipped — it never keeps the app from starting, and
/// never hides the themes in its valid siblings.
pub fn install_user_themes(home: &std::path::Path) {
    let directory = vibex_ui::theme_directory(home);
    let (themes, errors) = vibex_ui::load_directory(&directory);
    for (path, error) in &errors {
        tracing::warn!(
            code = error.stable_code(),
            path = %path.display(),
            "user theme skipped: {error}"
        );
    }
    if themes.is_empty() {
        return;
    }
    let count = themes.len();
    match vibex_ui::install_custom_themes(themes) {
        Ok(()) => tracing::info!(
            count,
            directory = %directory.display(),
            "user themes installed"
        ),
        Err(error) => tracing::warn!("user themes were not installed: {error}"),
    }
}

fn shared_highlight_theme(mode: GpuiThemeMode) -> Arc<HighlightTheme> {
    let definition = active_theme(mode);
    // A user theme's highlight block is validated structurally when its file
    // loads, but only the highlighter can reject a style it cannot use. Fall
    // back to the appearance's built-in block rather than failing the frame.
    let style = serde_json::from_str::<HighlightThemeStyle>(definition.highlight_json)
        .or_else(|error| {
            tracing::warn!(
                theme = definition.id,
                "syntax highlight block rejected, using the built-in one: {error}"
            );
            serde_json::from_str::<HighlightThemeStyle>(default_theme(mode).highlight_json)
        })
        .expect("built-in syntax highlight tokens must be valid");
    Arc::new(HighlightTheme {
        name: definition.name.to_string(),
        appearance: match mode {
            GpuiThemeMode::Light => ComponentThemeMode::Light,
            GpuiThemeMode::Dark => ComponentThemeMode::Dark,
        },
        style,
    })
}

fn apply_semantic_popover_colors(theme: &mut Theme, is_dark: bool) {
    let popover = semantic_color("popover", is_dark);
    let popover_foreground = semantic_color("popover-foreground", is_dark);
    theme.popover = popover;
    theme.tokens.popover = popover.into();
    theme.popover_foreground = popover_foreground;
    theme.tokens.popover_foreground = popover_foreground.into();
}

fn apply_semantic_highlight_colors(theme: &mut Theme, is_dark: bool) {
    let accent = semantic_color("accent", is_dark);
    let accent_foreground = semantic_color("accent-foreground", is_dark);
    theme.accent = accent;
    theme.tokens.accent = accent.into();
    theme.accent_foreground = accent_foreground;
    theme.tokens.accent_foreground = accent_foreground.into();

    let sidebar_accent = semantic_color("sidebar-accent", is_dark);
    let sidebar_accent_foreground = semantic_color("sidebar-accent-foreground", is_dark);
    theme.sidebar_accent = sidebar_accent;
    theme.tokens.sidebar_accent = sidebar_accent.into();
    theme.sidebar_accent_foreground = sidebar_accent_foreground;
    theme.tokens.sidebar_accent_foreground = sidebar_accent_foreground.into();
}

/// State-wash tone shared by hover, active, and selected fills. Quoted in
/// dark-mode terms: dark paints a soft-white wash, light the tone-flipped
/// soft-black — the same alpha family the dark tuning established.
///
/// The wash is a scrim, not a palette color: its *lightness* is fixed at the
/// pole that reads as "raised" for the appearance, while hue and a damped
/// chroma follow the active theme's background so a warm or tinted palette
/// gets a wash in its own family instead of a neutral grey one.
fn state_wash(is_dark: bool, alpha: f32) -> Hsla {
    let background = semantic_color("background", is_dark);
    let pole = if is_dark { 0.92 } else { 0.10 };
    gpui::hsla(background.h, background.s * WASH_CHROMA_SCALE, pole, alpha)
}

/// How much of the background's chroma survives into a wash. Enough to keep
/// the wash in the theme's family, low enough that it never reads as a color.
const WASH_CHROMA_SCALE: f32 = 0.35;

/// Hover wash for interactive rows, buttons, and tabs (theme-independent —
/// callers blending it per-frame through `motion::hover_blend` need a value,
/// not a borrow of the global theme).
pub fn hover_wash(is_dark: bool) -> Hsla {
    state_wash(is_dark, 0.11)
}

/// Active/pressed and selected wash.
pub fn active_wash(is_dark: bool) -> Hsla {
    state_wash(is_dark, 0.16)
}

/// How far the tab-strip plate leans from the page toward `muted`.
///
/// Enough that the selected folder tab — filled with the page color — lifts off
/// the strip, and that the unselected tabs' muted ink still reads on it.
const TAB_STRIP_PLATE_MIX: f32 = 0.60;

/// The plate a tab strip paints behind its tabs.
///
/// A folder tab fills its selected shape with the page color and leaves its
/// unselected siblings transparent, so the strip under them is what the tabs
/// are read against: it has to differ from the page, or the selected tab and
/// the strip become one surface.
///
/// `muted` is opaque and one step above the page in every bundled theme, but at
/// full strength it is the button plate. Mixed onto the page instead, the strip
/// lands between the two, and it is mixed here rather than painted as a
/// translucent wash so the tone does not depend on what happens to sit behind
/// the strip (the page, a card, or a wallpaper). Every strip takes this one
/// value, so the editor, session-group, Composer and terminal strips match.
pub fn tab_strip_plate(theme: &Theme) -> Hsla {
    theme
        .background
        .blend(theme.muted.opacity(TAB_STRIP_PLATE_MIX))
}

/// The fill a module panel paints behind its rows.
///
/// A module is a titled block of rows — a settings group, a Provider section,
/// a record card — and every one of them takes this same fill so the two
/// places that show modules, Settings and the configuration center, read as
/// one surface rather than two near-misses.
///
/// The fill is solid rather than a translucent wash. Modules nest (a traffic
/// section holds stat tiles), and two washes of one color compound into a
/// darker patch wherever they overlap, so a card read a different tone
/// depending on what happened to sit behind it. Solid, the tone is one value
/// however deep it stacks, and in a light appearance it is the neutral plate
/// itself rather than a grayed page — gray over white muddies toward a dirty
/// warm beige once it meets the ink washes. A dark appearance flattens the same
/// wash onto the page up front, so its plate is opaque too.
///
/// Modules are not outlined. The fill separates them from the page, and a
/// hairline around every card boxed the content in instead of letting the
/// tone do that work.
pub fn module_surface(is_dark: bool) -> Hsla {
    if is_dark {
        semantic_color("background", is_dark).blend(semantic_color("card", is_dark).opacity(0.35))
    } else {
        semantic_color("muted", is_dark)
    }
}

/// The fill a tile paints when it sits *on* a module.
///
/// A stat tile inside a section cannot take [`module_surface`] again: solid,
/// the two would land on one color and the tile would dissolve into the card.
/// The tile therefore climbs to the raised card, which is one step above the
/// module in every appearance — pure white on the light module, the lighter
/// card on the dark one — so the module reads as the ground and the tiles as
/// the cards laid on it.
pub fn module_tile_surface(is_dark: bool) -> Hsla {
    semantic_color("card", is_dark)
}

/// The native appearance a theme asks the platform's window chrome to adopt.
///
/// macOS draws its inactive traffic lights — and the window border — from the
/// application's effective `NSAppearance`, not from the pixels behind them. A
/// light theme left on a dark Mac therefore gets near-white inactive buttons
/// over a light title bar: they read as gone until the window is focused or
/// hovered, while every other app shows a dimmed trio. Pinning the native
/// appearance to the theme keeps the chrome and the surfaces on one side.
///
/// `None` clears the override so AppKit follows the system setting again —
/// which is exactly what the `System` theme wants.
fn pinned_native_appearance(theme: ThemeMode) -> Option<WindowAppearance> {
    match theme {
        ThemeMode::Light => Some(WindowAppearance::Light),
        ThemeMode::Dark => Some(WindowAppearance::Dark),
        ThemeMode::System => None,
    }
}

/// What [`pin_native_appearance`] last published, in the form the atomic can
/// hold. `UNSET` only exists for the first pass and for tests.
const NATIVE_APPEARANCE_UNSET: u8 = 0;
const NATIVE_APPEARANCE_SYSTEM: u8 = 1;
const NATIVE_APPEARANCE_LIGHT: u8 = 2;
const NATIVE_APPEARANCE_DARK: u8 = 3;

/// The tag [`pin_native_appearance`] stores for a requested theme.
const fn native_appearance_tag(theme: ThemeMode) -> u8 {
    match theme {
        ThemeMode::Light => NATIVE_APPEARANCE_LIGHT,
        ThemeMode::Dark => NATIVE_APPEARANCE_DARK,
        ThemeMode::System => NATIVE_APPEARANCE_SYSTEM,
    }
}

/// The process-wide tag of the native appearance the current pass published.
///
/// The appearance pass runs on every theme edit, every window bind, and — in
/// the `System` theme — from the appearance observer itself. Publishing
/// `NSApplication.appearance` each time posts AppKit notifications that bring
/// the observer back around, so a pass that would republish the value it
/// already published has to be a no-op.
static PINNED_NATIVE_APPEARANCE: AtomicU8 = AtomicU8::new(NATIVE_APPEARANCE_UNSET);

/// Publishes the theme's appearance to the platform's window chrome.
///
/// Called first in [`apply_appearance`], before the theme mode is resolved:
/// the `System` branch reads the window appearance, so an override left over
/// from the previous theme would otherwise be read back as the system's own.
fn pin_native_appearance(theme: ThemeMode, cx: &App) {
    let wanted = native_appearance_tag(theme);
    if PINNED_NATIVE_APPEARANCE.swap(wanted, Ordering::Relaxed) == wanted {
        return;
    }
    cx.set_window_appearance(pinned_native_appearance(theme));
}

pub fn apply_appearance(
    appearance: &AppearanceUiState,
    backdrop: Backdrop,
    window: Option<&mut Window>,
    cx: &mut App,
) {
    // Pin the native chrome to the theme before anything reads the appearance.
    pin_native_appearance(appearance.theme, cx);
    match appearance.theme {
        ThemeMode::Light => Theme::change(ComponentThemeMode::Light, window, cx),
        ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, window, cx),
        ThemeMode::System => Theme::sync_system_appearance(window, cx),
    }
    // Install the selection before any token is read: everything below — the
    // highlight theme, the component colors, the washes — resolves against the
    // variant this points at, and so does every paint after it. The backdrop
    // goes with it, because whether a container lets an image through is part
    // of the color it resolves to.
    set_active_selection(&appearance.theme_selection);
    set_wallpaper_backdrop(backdrop);
    // Keep gpui's global animation flag in step with the user preference: the
    // vendored gpui snaps every `with_animation` element (modal slides, menu
    // fades, entrance lifts) to its end state and schedules no frames while it
    // is set — the app-side `Transition` checks stay as a second guard. The
    // workbench window ORs in its own "not active" pause through the same
    // helper, but only when the user asked for that pause, so a background
    // window keeps animating by default.
    set_user_reduced_motion(appearance.reduced_motion, cx);
    set_pause_inactive_animation(appearance.pause_inactive_animation, cx);
    Theme::update(cx, |theme| {
        theme.font_family = appearance
            .interface_font
            .family
            .as_deref()
            .unwrap_or(INTERFACE_TYPOGRAPHY.family)
            .to_string()
            .into();
        theme.font_size = scaled_font_size(
            appearance.interface_font.size,
            appearance.window_scale_percent,
        );
        theme.mono_font_family = appearance
            .code_font
            .family
            .as_deref()
            .unwrap_or(shared_code_font_family())
            .to_string()
            .into();
        theme.mono_font_size =
            scaled_font_size(appearance.code_font.size, appearance.window_scale_percent);
        theme.radius = px(RADII.control_px);
        theme.radius_lg = px(RADII.large_px);
        theme.shadow = SHADOWS_ENABLED;
        let is_dark = theme.is_dark();
        theme.highlight_theme = shared_highlight_theme(model_mode(is_dark));
        apply_semantic_popover_colors(theme, is_dark);
        apply_semantic_highlight_colors(theme, is_dark);
        // Full semantic mapping — every interactive surface resolves from the
        // shared token source so the two appearances stay in one tuned
        // relationship (dark is the authored one; light flips tone, not layout).
        let background = semantic_color("background", is_dark);
        let foreground = semantic_color("foreground", is_dark);
        let secondary = semantic_color("secondary", is_dark);
        let secondary_foreground = semantic_color("secondary-foreground", is_dark);
        let muted = semantic_color("muted", is_dark);
        let muted_foreground = semantic_color("muted-foreground", is_dark);
        let accent = semantic_color("accent", is_dark);
        let border = semantic_color("border", is_dark);
        let input = semantic_color("input", is_dark);
        let ring = semantic_color("ring", is_dark);
        let sidebar = semantic_color("sidebar", is_dark);
        let sidebar_foreground = semantic_color("sidebar-foreground", is_dark);
        let hover = hover_wash(is_dark);
        let active = active_wash(is_dark);

        theme.background = background;
        theme.tokens.background = background.into();
        theme.foreground = foreground;
        theme.tokens.foreground = foreground.into();
        theme.secondary = secondary;
        theme.tokens.secondary = secondary.into();
        theme.secondary_foreground = secondary_foreground;
        theme.tokens.secondary_foreground = secondary_foreground.into();
        theme.muted = muted;
        theme.tokens.muted = muted.into();
        theme.muted_foreground = muted_foreground;
        theme.tokens.muted_foreground = muted_foreground.into();
        theme.primary = semantic_color("primary", is_dark);
        theme.tokens.primary = theme.primary.into();
        theme.primary_foreground = semantic_color("primary-foreground", is_dark);
        theme.tokens.primary_foreground = theme.primary_foreground.into();
        theme.border = border;
        theme.tokens.border = border.into();
        theme.input = input;
        theme.tokens.input = input.into();
        theme.ring = ring;
        theme.tokens.ring = ring.into();
        theme.sidebar = sidebar;
        theme.tokens.sidebar = sidebar.into();
        theme.sidebar_foreground = sidebar_foreground;
        theme.tokens.sidebar_foreground = sidebar_foreground.into();
        theme.sidebar_primary = semantic_color("sidebar-primary", is_dark);
        theme.tokens.sidebar_primary = theme.sidebar_primary.into();
        theme.sidebar_primary_foreground = semantic_color("sidebar-primary-foreground", is_dark);
        theme.tokens.sidebar_primary_foreground = theme.sidebar_primary_foreground.into();
        theme.sidebar_border = semantic_color("sidebar-border", is_dark);
        theme.tokens.sidebar_border = theme.sidebar_border.into();
        // Hover/active washes and their derived component plates. Component hover
        // plates stay opaque-ready (raised pills never swap to translucent washes),
        // so they compose the wash over the surface they sit on.
        theme.accent = accent;
        theme.accent_foreground = semantic_color("accent-foreground", is_dark);
        theme.list_hover = hover;
        theme.tokens.list_hover = hover.into();
        theme.table_hover = hover;
        theme.tokens.table_hover = hover.into();
        theme.secondary_hover = mix(secondary, hover, 0.5);
        theme.tokens.secondary_hover = theme.secondary_hover.into();
        theme.button_hover = mix(secondary, hover, 0.5);
        theme.tokens.button_hover = theme.button_hover.into();
        theme.button_active = mix(secondary, active, 0.5);
        theme.tokens.button_active = theme.button_active.into();
        theme.primary_hover = mix(theme.primary, foreground, 0.08);
        theme.tokens.primary_hover = theme.primary_hover.into();
        theme.primary_active = mix(theme.primary, foreground, 0.16);
        theme.tokens.primary_active = theme.primary_active.into();
        // Modal scrim: darken what is behind it. A light-mode scrim of the dark
        // strength reads as a blackout on a bright field, so light runs ~half.
        theme.overlay = gpui::black().opacity(if is_dark { 0.60 } else { 0.32 });
        theme.title_bar = sidebar;
        theme.tokens.title_bar = sidebar.into();
        theme.title_bar_border = semantic_color("sidebar-border", is_dark);
        theme.tokens.title_bar_border = theme.title_bar_border.into();
        if appearance.high_contrast {
            let foreground = theme.foreground;
            theme.border = foreground.alpha(if theme.is_dark() { 0.42 } else { 0.30 });
            theme.ring = foreground.alpha(0.72);
            theme.sidebar_border = theme.border;
            theme.title_bar_border = theme.border;
        }
        // Everything above maps the product's own roles. The rest of the framework
        // palette — switches, segmented tabs, outline buttons, scrollbars,
        // skeletons, selections — would otherwise keep the stock neutral colors
        // `Theme::change` loaded, which is why a themed window still showed grey
        // chrome. Complete it from the same variant. `Theme::update` then publishes
        // the result to the base layer that owns scrollbars, resize handles, and
        // text view defaults, and refreshes every window.
        vibex_ui::apply_component_palette(theme, active_theme(model_mode(is_dark)));
        // A wallpaper is painted behind the workbench, so the container roles
        // have to let it through — after the palette bridge, which derives its
        // own container roles from the ones mapped above. The roles the catalog
        // owns rather than the framework have already faded, on the way through
        // `semantic_color`; setting the same alpha again lands on one color.
        if backdrop.is_wallpaper() {
            vibex_ui::let_wallpaper_through(theme, is_dark);
        }
    });
    if backdrop.is_wallpaper() {
        restore_window_plate(cx);
    }
    apply_code_font_weight(appearance.code_font.weight, cx);
}

/// Puts the window's own base plate back to the authored page color.
///
/// The framework paints each window's background from the legacy
/// `background` token, and it realigns that token to its color on every edit
/// that changes the color — so the fade above reaches the plate too. The plate
/// is restored after the pass has closed, because inside one the realignment
/// would simply run again: a translucent base plate would let whatever sits
/// behind the window show through it, and the surfaces painted over it would
/// no longer compose the color they were authored in.
fn restore_window_plate(cx: &mut App) {
    let authored = page_plate(Theme::global(cx).is_dark());
    Theme::global_mut(cx).tokens.background = authored.into();
}

pub fn scaled_font_size(size: u16, window_scale_percent: u16) -> gpui::Pixels {
    px(f32::from(size) * f32::from(window_scale_percent) / 100.0)
}

/// Read a semantic token from the palette currently active for `dark`.
///
/// Most containers in the workbench are painted from the catalog directly rather
/// than from the framework's theme, so this is where a wallpaper reaches them:
/// a container role comes back with the alpha its tier takes, and every other
/// role — ink, lines, status colors, selection plates — is exactly what the
/// palette authored.
pub(crate) fn semantic_color(name: &str, dark: bool) -> Hsla {
    let color = semantic_color_for(active_theme(model_mode(dark)), name);
    if wallpaper_backdrop() {
        return vibex_ui::wallpaper_surface(name, color, dark);
    }
    color
}

/// Read a semantic token from one specific theme, as that theme authored it.
///
/// Used by previews that must paint a palette other than the active one —
/// theme pickers above all, which show every candidate at once — and by the
/// paint-time path that applies the wallpaper policy itself.
pub fn semantic_color_for(theme: &GpuiThemeDefinition, name: &str) -> Hsla {
    let token = catalog_token(theme, name)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    Hsla {
        a: token.alpha,
        ..gpui::rgb(token.rgb).into()
    }
}

/// Read a semantic token from the palette currently active for `dark`.
pub fn semantic_token(name: &str, dark: bool) -> Option<GpuiColorToken> {
    catalog_token(active_theme(model_mode(dark)), name)
}

/// The authored page color at full opacity.
///
/// A wallpaper fades the theme's containers so the image shows through, which
/// leaves the global `background` translucent — and a window's own base plate
/// with it. This is the same color with its opacity back: the workbench paints
/// a wallpaper over it, so a `Contain` fit's letterbox lands on the page rather
/// than on nothing, and the veil [`wallpaper_veil`] dims an image with is mixed
/// from it.
pub fn page_plate(is_dark: bool) -> Hsla {
    semantic_color_for(active_theme(model_mode(is_dark)), "background").alpha(1.0)
}

/// The veil painted over a wallpaper to keep the text above it legible.
///
/// Mixed from the page color rather than from black: in a light appearance the
/// veil is the theme's own light page color, so the ink that reads over it
/// keeps the contrast the palette was tuned for, and in a dark appearance it
/// darkens the image the same way.
pub fn wallpaper_veil(is_dark: bool, dim_percent: u16) -> Hsla {
    let alpha = f32::from(dim_percent.min(100)) / 100.0;
    page_plate(is_dark).alpha(alpha)
}

#[cfg(test)]
mod tests {
    use std::sync::{Mutex, MutexGuard};

    use super::*;

    /// Serializes the tests that move the process-wide appearance slots.
    ///
    /// `set_active_selection` writes process-wide atomics, so a test that
    /// points them at a named variant must not overlap the one that asserts an
    /// unset slot still resolves to the catalog default.
    static SLOT_LOCK: Mutex<()> = Mutex::new(());

    fn slot_guard() -> MutexGuard<'static, ()> {
        SLOT_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }

    /// Puts the process-wide appearance slots back when a test moves them.
    ///
    /// Restoring on drop keeps the moved state from leaking even if an
    /// assertion fails partway through.
    struct SlotRestore {
        light: usize,
        dark: usize,
        wallpaper: bool,
    }

    impl SlotRestore {
        fn capture() -> Self {
            Self {
                light: mode_slot(GpuiThemeMode::Light).load(Ordering::Relaxed),
                dark: mode_slot(GpuiThemeMode::Dark).load(Ordering::Relaxed),
                wallpaper: wallpaper_backdrop(),
            }
        }
    }

    impl Drop for SlotRestore {
        fn drop(&mut self) {
            mode_slot(GpuiThemeMode::Light).store(self.light, Ordering::Relaxed);
            mode_slot(GpuiThemeMode::Dark).store(self.dark, Ordering::Relaxed);
            WALLPAPER.store(self.wallpaper, Ordering::Relaxed);
        }
    }

    /// Puts the native-appearance tag back when a test moves it.
    struct NativeAppearanceRestore(u8);

    impl NativeAppearanceRestore {
        fn capture() -> Self {
            Self(PINNED_NATIVE_APPEARANCE.load(Ordering::Relaxed))
        }
    }

    impl Drop for NativeAppearanceRestore {
        fn drop(&mut self) {
            PINNED_NATIVE_APPEARANCE.store(self.0, Ordering::Relaxed);
        }
    }

    /// AppKit paints the inactive traffic lights from the application's
    /// effective appearance, not from the pixels behind them: a light theme
    /// left on a dark Mac gets near-white inactive buttons over a light title
    /// bar, which reads as the buttons disappearing. The theme therefore has
    /// to state its own side, and only `System` may leave the choice to the
    /// platform.
    #[test]
    fn the_native_appearance_follows_the_theme_and_system_defers() {
        assert_eq!(
            pinned_native_appearance(ThemeMode::Light),
            Some(WindowAppearance::Light)
        );
        assert_eq!(
            pinned_native_appearance(ThemeMode::Dark),
            Some(WindowAppearance::Dark)
        );
        assert_eq!(
            pinned_native_appearance(ThemeMode::System),
            None,
            "System hands the window chrome back to the platform"
        );
    }

    /// Republishing the appearance it already published would post AppKit
    /// notifications that bring the appearance observer straight back around,
    /// so a frozen theme has to leave the tag alone — while `System` still
    /// reaches the platform once, because clearing an override is a change.
    #[gpui::test]
    fn pinning_the_native_appearance_is_idempotent(cx: &mut gpui::TestAppContext) {
        let _guard = slot_guard();
        let _restore = NativeAppearanceRestore::capture();
        PINNED_NATIVE_APPEARANCE.store(NATIVE_APPEARANCE_UNSET, Ordering::Relaxed);

        cx.update(|cx| pin_native_appearance(ThemeMode::Dark, cx));
        assert_eq!(
            PINNED_NATIVE_APPEARANCE.load(Ordering::Relaxed),
            native_appearance_tag(ThemeMode::Dark)
        );
        cx.update(|cx| pin_native_appearance(ThemeMode::Dark, cx));
        assert_eq!(
            PINNED_NATIVE_APPEARANCE.load(Ordering::Relaxed),
            native_appearance_tag(ThemeMode::Dark),
            "a repeated pass keeps the tag it already published"
        );

        cx.update(|cx| pin_native_appearance(ThemeMode::System, cx));
        assert_eq!(
            PINNED_NATIVE_APPEARANCE.load(Ordering::Relaxed),
            NATIVE_APPEARANCE_SYSTEM,
            "System still publishes the cleared override"
        );
    }

    /// The pin has to land before the mode is resolved: `sync_system_appearance`
    /// reads the window appearance, so an override left over from the previous
    /// theme would be read back as the system's own choice.
    #[test]
    fn the_native_appearance_is_pinned_before_the_mode_is_resolved() {
        let source = include_str!("theme.rs");
        let pass = source
            .split_once("pub fn apply_appearance(")
            .and_then(|(_, tail)| tail.split_once("\nfn restore_window_plate"))
            .map(|(body, _)| body)
            .expect("the appearance pass should remain inspectable");
        let pin = pass
            .find("pin_native_appearance(appearance.theme, cx);")
            .expect("the pass has to pin the native appearance");
        let system = pass
            .find("Theme::sync_system_appearance(window, cx)")
            .expect("the pass still resolves the system mode");
        assert!(
            pin < system,
            "the native appearance must be pinned before the mode is read"
        );
    }

    /// A wallpaper is painted behind the workbench's surfaces, so the pass has
    /// to fade the theme's containers without touching the ink that reads over
    /// them — and the window's own plate stays the authored page color, because
    /// a translucent base plate would let the desktop behind the window show
    /// through it.
    ///
    /// Both appearances are covered because the alphas differ between them, and
    /// both paint paths are: the framework's theme roles and the catalog roles
    /// the workbench paints its cards and panels from.
    #[gpui::test]
    fn a_wallpaper_backdrop_fades_containers_and_keeps_the_window_plate_opaque(
        cx: &mut gpui::TestAppContext,
    ) {
        let _guard = slot_guard();
        let _slots = SlotRestore::capture();
        cx.update(gpui_component::init);

        for (mode, dark) in [(ThemeMode::Dark, true), (ThemeMode::Light, false)] {
            let appearance = AppearanceUiState {
                theme: mode,
                ..Default::default()
            };
            let surfaces = vibex_ui::WallpaperSurfaces::for_appearance(dark);
            let authored = semantic_color_for(active_theme(model_mode(dark)), "background");
            let catalog = active_theme(model_mode(dark));

            cx.update(|cx| apply_appearance(&appearance, Backdrop::Theme, None, cx));
            cx.update(|cx| {
                let theme = Theme::global(cx);
                assert_eq!(
                    theme.background.a, authored.a,
                    "{mode:?}: without a wallpaper the page keeps the authored color"
                );
                assert_eq!(
                    semantic_color("card", dark).a,
                    semantic_color_for(catalog, "card").a,
                    "{mode:?}: without a wallpaper a card keeps the authored color"
                );
            });

            cx.update(|cx| apply_appearance(&appearance, Backdrop::Wallpaper, None, cx));
            cx.update(|cx| {
                let theme = Theme::global(cx);
                assert_eq!(
                    theme.background.a, surfaces.base,
                    "{mode:?}: the page has to let the image through"
                );
                assert_eq!(theme.sidebar.a, surfaces.base);
                assert_eq!(theme.group_box.a, surfaces.raised);
                assert_eq!(
                    theme.scrollbar.a, surfaces.raised,
                    "{mode:?}: a scrollbar track is a plate, not an edge"
                );
                assert_eq!(
                    theme.foreground,
                    semantic_color_for(catalog, "foreground"),
                    "{mode:?}: text keeps the authored ink"
                );
                assert_eq!(
                    theme.border,
                    semantic_color_for(catalog, "border"),
                    "{mode:?}: borders keep the authored hairline"
                );

                // The catalog is the other paint path: a card, the composer,
                // and the right rail are painted from it, so they fade by the
                // same rule the framework roles do.
                let authored_alpha = |role: &str| semantic_color_for(catalog, role).a;
                assert_eq!(
                    semantic_color("right-rail-surface", dark).a,
                    surfaces.base,
                    "{mode:?}: the right rail is chrome"
                );
                assert_eq!(
                    semantic_color("muted-foreground", dark).a,
                    authored_alpha("muted-foreground"),
                    "{mode:?}: ink is never a container"
                );
                if dark {
                    // A dark appearance separates its page from its plates by
                    // tone already, so the plates keep the authored color.
                    assert_eq!(semantic_color("card", dark).a, authored_alpha("card"));
                    assert_eq!(
                        semantic_color("composer-surface", dark).a,
                        authored_alpha("composer-surface")
                    );
                } else {
                    assert_eq!(
                        semantic_color("card", dark).a,
                        surfaces.raised,
                        "{mode:?}: a light plate the color of the page has to fade"
                    );
                    assert_eq!(semantic_color("composer-surface", dark).a, surfaces.raised);
                }

                assert_eq!(
                    page_plate(dark),
                    authored,
                    "{mode:?}: the plate the image sits on is the authored page color"
                );
                assert_eq!(wallpaper_veil(dark, 40).a, 0.4);
                assert_eq!(wallpaper_veil(dark, 400).a, 1.0);
                assert_eq!(
                    theme.tokens.background.color.a, authored.a,
                    "{mode:?}: the legacy plate the framework paints a window from must stay opaque"
                );
            });
        }
    }

    #[gpui::test]
    fn appearance_pass_completes_the_framework_palette(cx: &mut gpui::TestAppContext) {
        let _guard = slot_guard();
        let _slots = SlotRestore::capture();
        cx.update(gpui_component::init);
        for (mode, id) in [
            (ThemeMode::Light, "gruvbox-light"),
            (ThemeMode::Dark, "tokyo-night"),
        ] {
            let mut appearance = AppearanceUiState {
                theme: mode,
                ..Default::default()
            };
            appearance.theme_selection.select_light("gruvbox-light");
            appearance.theme_selection.select_dark("tokyo-night");
            cx.update(|cx| apply_appearance(&appearance, Backdrop::Theme, None, cx));

            // gpui-component resets its palette to the stock neutrals on every
            // `Theme::change`; the appearance pass has to fill the component
            // roles back in from the selected variant, or switches, segmented
            // tabs, and outline buttons paint the framework's greys. Read the
            // expectation from the named variant rather than the process-wide
            // selection slot, which parallel tests also move.
            let definition = vibex_ui::theme(id).expect("built-in theme");
            cx.update(|cx| {
                let theme = Theme::global(cx);
                for (name, expected, actual) in [
                    (
                        // The unchecked track is derived, not a role: the light
                        // module panel is `muted` itself, so a `muted` track
                        // would vanish into it.
                        "switch",
                        vibex_ui::switch_track_off(
                            semantic_color_for(definition, "muted"),
                            semantic_color_for(definition, "muted-foreground"),
                        ),
                        theme.tokens.switch.color,
                    ),
                    (
                        "tab_bar_segmented",
                        semantic_color_for(definition, "secondary"),
                        theme.tokens.tab_bar_segmented.color,
                    ),
                    (
                        "button",
                        semantic_color_for(definition, "secondary"),
                        theme.tokens.button.color,
                    ),
                    (
                        "danger",
                        semantic_color_for(definition, "destructive"),
                        theme.tokens.danger.color,
                    ),
                    (
                        "caret",
                        semantic_color_for(definition, "primary"),
                        theme.caret,
                    ),
                ] {
                    assert_eq!(actual, expected, "{mode:?}: {name} is not themed");
                }
            });
        }
    }

    #[test]
    fn window_scale_applies_to_interface_and_code_metrics() {
        assert_eq!(scaled_font_size(14, 100), px(14.0));
        assert_eq!(scaled_font_size(14, 150), px(21.0));
        assert_eq!(scaled_font_size(13, 75), px(9.75));
    }

    #[test]
    fn generated_tokens_keep_shared_source_identity_and_core_semantics() {
        assert_eq!(TOKEN_SOURCE_SHA256.len(), 64);
        assert_eq!(TOKEN_SCHEMA_VERSION, "vibex-design-tokens.v2");
        assert_eq!(TOKEN_PRODUCT_VISUAL_SOURCE, "apps/desktop");
        assert_eq!(TOKEN_SOURCE_PATH, "crates/vibex-ui/theme/tokens.json");

        // Assert against the catalog defaults directly rather than through the
        // process-wide active slot, so the expectations cannot depend on test
        // ordering.
        let light = default_theme(GpuiThemeMode::Light);
        let dark = default_theme(GpuiThemeMode::Dark);
        assert_eq!(catalog_token(light, "background").unwrap().hex, "#ffffff");
        assert_eq!(catalog_token(dark, "foreground").unwrap().hex, "#e5e5e5");
        assert_eq!(catalog_token(dark, "border").unwrap().alpha, 0.1);
        for theme in [light, dark] {
            assert!(catalog_token(theme, "warning-foreground").is_some());
            assert!(theme.tokens.len() >= 40);
        }
    }

    #[test]
    fn an_unset_slot_paints_the_catalog_default() {
        let _guard = slot_guard();
        for mode in GpuiThemeMode::ALL {
            assert_eq!(active_theme(mode).id, default_theme(mode).id);
        }
    }

    #[test]
    fn selection_resolves_each_appearance_slot_independently() {
        let mut selection = ThemeSelection::default();
        assert_eq!(selection_index(&selection, GpuiThemeMode::Light), UNSET);
        assert_eq!(selection_index(&selection, GpuiThemeMode::Dark), UNSET);

        selection.select_light("gruvbox-light");
        selection.select_dark("nord");

        // Resolve through the catalog so the assertion does not depend on
        // catalog ordering.
        let light_index = vibex_ui::theme_index("gruvbox-light", GpuiThemeMode::Light);
        let dark_index = vibex_ui::theme_index("nord", GpuiThemeMode::Dark);
        assert_eq!(
            selection_index(&selection, GpuiThemeMode::Light),
            light_index.unwrap()
        );
        assert_eq!(
            selection_index(&selection, GpuiThemeMode::Dark),
            dark_index.unwrap()
        );

        // Changing one slot leaves the other alone.
        selection.select_light("solarized-light");
        assert_eq!(
            selection_index(&selection, GpuiThemeMode::Dark),
            dark_index.unwrap()
        );

        // A stale id, or one authored for the other appearance, falls back to
        // the default instead of adopting a mismatched palette.
        selection.select_light("removed-theme");
        selection.select_dark("vibex-light");
        assert_eq!(selection_index(&selection, GpuiThemeMode::Light), UNSET);
        assert_eq!(selection_index(&selection, GpuiThemeMode::Dark), UNSET);
    }

    #[test]
    fn state_washes_flip_tone_between_appearances() {
        for (is_dark, wash_channel) in [(true, 0.92_f32), (false, 0.10)] {
            let hover = hover_wash(is_dark);
            assert_eq!(hover.l, wash_channel);
            assert!((hover.a - 0.11).abs() < 1e-6);
            let active = active_wash(is_dark);
            assert_eq!(active.l, wash_channel);
            assert!((active.a - 0.16).abs() < 1e-6);
        }
    }

    /// A folder tab is filled with the page color, so a strip plate that lands
    /// on the page would leave the selected tab indistinguishable from the
    /// strip it sits on. Every bundled theme has to separate the two, and the
    /// plate has to stay opaque so it reads the same over a card or over a
    /// wallpaper — which is why it is mixed from opaque roles rather than
    /// painted as a translucent wash.
    #[test]
    fn every_theme_separates_the_tab_strip_plate_from_the_page() {
        for theme in vibex_ui::themes_for(GpuiThemeMode::Light)
            .chain(vibex_ui::themes_for(GpuiThemeMode::Dark))
        {
            let page = semantic_color_for(theme, "background");
            let muted = semantic_color_for(theme, "muted");
            let plate = page.blend(muted.opacity(TAB_STRIP_PLATE_MIX));
            assert!(
                plate.a >= 1.0 - 1e-6,
                "{}: the tab strip plate is translucent",
                theme.name
            );
            assert!(
                (plate.l - page.l).abs() > 0.02,
                "{}: the tab strip plate sits on the page ({} vs {})",
                theme.name,
                plate.l,
                page.l
            );
        }
    }

    /// A module is an opaque plate, and the tile that lands on it is one step
    /// apart from it.
    ///
    /// Modules nest, so a translucent fill compounds wherever a tile overlaps
    /// its section and the same card reads as a different tone. Solid, the
    /// tone is one value at any depth — and because it is solid, the nested
    /// tile has to take a different role rather than the same one twice.
    #[test]
    fn module_fills_stay_opaque_and_nested_tiles_step_off_them() {
        let _guard = slot_guard();
        let _slots = SlotRestore::capture();
        // Both slots at the product default, so the helpers read the palette
        // this ships with rather than whatever a sibling test left behind.
        set_active_selection(&ThemeSelection::default());
        for mode in GpuiThemeMode::ALL {
            let is_dark = mode.is_dark();
            let module = module_surface(is_dark);
            let tile = module_tile_surface(is_dark);
            assert_eq!(module.a, 1.0, "{mode:?}: a module plate is a solid fill");
            assert_eq!(tile.a, 1.0, "{mode:?}: a tile on a module is a solid fill");
            assert!(
                (tile.l - module.l).abs() > 0.01,
                "{mode:?}: the tile ({} ) and its module ({}) are one color",
                tile.l,
                module.l
            );
            // The tile is the raised one: it lifts off the module in both
            // appearances, toward the page in light and toward the ink in dark.
            assert!(
                tile.l > module.l,
                "{mode:?}: the tile must sit above its module"
            );
        }
    }

    #[test]
    fn shared_highlight_tokens_preserve_the_locked_component_defaults() {
        // This reads the same process-wide slots as
        // `an_unset_slot_paints_the_catalog_default`, so it needs the same
        // guard: a test that points them at a named variant would otherwise
        // make the locked defaults look like that variant's block.
        let _guard = slot_guard();
        assert_eq!(
            shared_highlight_theme(GpuiThemeMode::Light).style,
            HighlightTheme::default_light().style
        );
        assert_eq!(
            shared_highlight_theme(GpuiThemeMode::Dark).style,
            HighlightTheme::default_dark().style
        );
    }

    #[test]
    fn semantic_popover_colors_cover_custom_and_component_menu_paths() {
        for is_dark in [false, true] {
            let mut theme = Theme::default();
            apply_semantic_popover_colors(&mut theme, is_dark);

            let expected_background = semantic_color("popover", is_dark);
            assert_eq!(theme.popover, expected_background);
            assert_eq!(theme.tokens.popover.color, expected_background);
            let expected_foreground = semantic_color("popover-foreground", is_dark);
            assert_eq!(theme.popover_foreground, expected_foreground);
            assert_eq!(theme.tokens.popover_foreground.color, expected_foreground);
        }
    }

    #[test]
    fn semantic_highlight_colors_cover_component_and_custom_paths() {
        for is_dark in [false, true] {
            let mut theme = Theme::default();
            apply_semantic_highlight_colors(&mut theme, is_dark);

            let expected_accent = semantic_color("accent", is_dark);
            assert_eq!(theme.accent, expected_accent);
            assert_eq!(theme.tokens.accent.color, expected_accent);
            let expected_accent_foreground = semantic_color("accent-foreground", is_dark);
            assert_eq!(theme.accent_foreground, expected_accent_foreground);
            assert_eq!(
                theme.tokens.accent_foreground.color,
                expected_accent_foreground
            );

            let expected_sidebar_accent = semantic_color("sidebar-accent", is_dark);
            assert_eq!(theme.sidebar_accent, expected_sidebar_accent);
            assert_eq!(theme.tokens.sidebar_accent.color, expected_sidebar_accent);
            let expected_sidebar_foreground = semantic_color("sidebar-accent-foreground", is_dark);
            assert_eq!(theme.sidebar_accent_foreground, expected_sidebar_foreground);
            assert_eq!(
                theme.tokens.sidebar_accent_foreground.color,
                expected_sidebar_foreground
            );
        }
    }
}

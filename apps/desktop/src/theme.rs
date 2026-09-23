use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use gpui::{App, Hsla, Window, px};
use gpui_component::{
    Theme, ThemeMode as ComponentThemeMode,
    highlighter::{HighlightTheme, HighlightThemeStyle},
};
use vibex_desktop_model::{AppearanceUiState, ThemeMode, ThemeSelection};
use vibex_markdown::apply_code_font_weight;
use vibex_ui::{
    CODE_TYPOGRAPHY, GpuiColorToken, GpuiThemeDefinition, GpuiThemeMode, INTERFACE_TYPOGRAPHY,
    RADII, SHADOWS_ENABLED, default_theme, semantic_token as catalog_token, theme_index,
};

use crate::motion::{mix, set_pause_inactive_animation, set_user_reduced_motion};

pub use vibex_ui::{
    TOKEN_PRODUCT_VISUAL_SOURCE, TOKEN_SCHEMA_VERSION, TOKEN_SOURCE_PATH, TOKEN_SOURCE_SHA256,
};

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

pub fn apply_appearance(appearance: &AppearanceUiState, window: Option<&mut Window>, cx: &mut App) {
    match appearance.theme {
        ThemeMode::Light => Theme::change(ComponentThemeMode::Light, window, cx),
        ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, window, cx),
        ThemeMode::System => Theme::sync_system_appearance(window, cx),
    }
    // Install the selection before any token is read: everything below — the
    // highlight theme, the component colors, the washes — resolves against the
    // variant this points at, and so does every paint after it.
    set_active_selection(&appearance.theme_selection);
    // Keep gpui's global animation flag in step with the user preference: the
    // vendored gpui snaps every `with_animation` element (modal slides, menu
    // fades, entrance lifts) to its end state and schedules no frames while it
    // is set — the app-side `Transition` checks stay as a second guard. The
    // workbench window ORs in its own "not active" pause through the same
    // helper, but only when the user asked for that pause, so a background
    // window keeps animating by default.
    set_user_reduced_motion(appearance.reduced_motion, cx);
    set_pause_inactive_animation(appearance.pause_inactive_animation, cx);
    let theme = Theme::global_mut(cx);
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
    // chrome. Complete it from the same variant, then publish the result to the
    // base layer that owns scrollbars, resize handles, and text view defaults.
    vibex_ui::apply_component_palette(theme, active_theme(model_mode(is_dark)));
    Theme::sync_base(cx);
    apply_code_font_weight(appearance.code_font.weight, cx);
}

pub fn scaled_font_size(size: u16, window_scale_percent: u16) -> gpui::Pixels {
    px(f32::from(size) * f32::from(window_scale_percent) / 100.0)
}

pub(crate) fn semantic_color(name: &str, dark: bool) -> Hsla {
    semantic_color_for(active_theme(model_mode(dark)), name)
}

/// Read a semantic token from one specific theme.
///
/// Used by previews that must paint a palette other than the active one —
/// theme pickers above all, which show every candidate at once.
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
    }

    impl SlotRestore {
        fn capture() -> Self {
            Self {
                light: mode_slot(GpuiThemeMode::Light).load(Ordering::Relaxed),
                dark: mode_slot(GpuiThemeMode::Dark).load(Ordering::Relaxed),
            }
        }
    }

    impl Drop for SlotRestore {
        fn drop(&mut self) {
            mode_slot(GpuiThemeMode::Light).store(self.light, Ordering::Relaxed);
            mode_slot(GpuiThemeMode::Dark).store(self.dark, Ordering::Relaxed);
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
            cx.update(|cx| apply_appearance(&appearance, None, cx));

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
                        "switch",
                        semantic_color_for(definition, "muted"),
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

    #[test]
    fn shared_highlight_tokens_preserve_the_locked_component_defaults() {
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

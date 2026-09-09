use std::sync::Arc;

use gpui::{App, Hsla, Window, px};
use gpui_component::{
    Theme, ThemeMode as ComponentThemeMode,
    highlighter::{HighlightTheme, HighlightThemeStyle},
};
use vibex_desktop_model::{AppearanceUiState, ThemeMode};
use vibex_markdown::apply_code_font_weight;
use vibex_ui::{
    CODE_TYPOGRAPHY, DARK_HIGHLIGHT_THEME_JSON, DARK_TOKENS, GpuiColorToken, INTERFACE_TYPOGRAPHY,
    LIGHT_HIGHLIGHT_THEME_JSON, LIGHT_TOKENS, RADII, SHADOWS_ENABLED,
};

use crate::glass;
use crate::motion::mix;

pub use vibex_ui::{
    GPUI_COMPONENT_REVISION, GPUI_REVISION, TOKEN_PRODUCT_VISUAL_SOURCE, TOKEN_SCHEMA_VERSION,
    TOKEN_SOURCE_PATH, TOKEN_SOURCE_SHA256,
};

fn shared_code_font_family() -> &'static str {
    match CODE_TYPOGRAPHY.family {
        "platform_monospace" => crate::platform::default_code_font_family(),
        unsupported => panic!("unsupported shared GPUI code-font policy: {unsupported}"),
    }
}

fn shared_highlight_theme(is_dark: bool) -> Arc<HighlightTheme> {
    let (name, appearance, source) = if is_dark {
        (
            "Vibex Dark",
            ComponentThemeMode::Dark,
            DARK_HIGHLIGHT_THEME_JSON,
        )
    } else {
        (
            "Vibex Light",
            ComponentThemeMode::Light,
            LIGHT_HIGHLIGHT_THEME_JSON,
        )
    };
    let style = serde_json::from_str::<HighlightThemeStyle>(source)
        .expect("shared GPUI syntax highlight tokens must be valid");
    Arc::new(HighlightTheme {
        name: name.to_string(),
        appearance,
        style,
    })
}

/// Propagate the frosted-glass preference to the shared glass layer. Card
/// surfaces read this through `glass::glass_settings` when they paint.
fn apply_glass_surfaces(appearance: &AppearanceUiState, cx: &mut App) {
    glass::apply_glass_settings(
        glass::GlassSettings {
            enabled: appearance.glass_surfaces,
            blur_radius: f32::from(appearance.glass_blur_radius.clamp(4, 64)),
            tint_opacity: glass::DEFAULT_GLASS_TINT_OPACITY,
        },
        cx,
    );
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
fn state_wash(is_dark: bool, alpha: f32) -> Hsla {
    if is_dark {
        gpui::hsla(0.0, 0.0, 0.92, alpha)
    } else {
        gpui::hsla(0.0, 0.0, 0.10, alpha)
    }
}

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
    apply_glass_surfaces(appearance, cx);
    // Keep gpui's global animation flag in step with the user preference: the
    // vendored gpui snaps every `with_animation` element (modal slides, menu
    // fades, entrance lifts) to its end state and schedules no frames while it
    // is set — the app-side `Transition` checks stay as a second guard.
    cx.set_reduce_motion(appearance.reduced_motion);
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
    theme.highlight_theme = shared_highlight_theme(is_dark);
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
    apply_code_font_weight(appearance.code_font.weight, cx);
}

pub fn scaled_font_size(size: u16, window_scale_percent: u16) -> gpui::Pixels {
    px(f32::from(size) * f32::from(window_scale_percent) / 100.0)
}

pub(crate) fn semantic_color(name: &str, dark: bool) -> Hsla {
    let token = semantic_token(name, dark)
        .unwrap_or_else(|| panic!("missing generated GPUI semantic token: {name}"));
    Hsla {
        a: token.alpha,
        ..gpui::rgb(token.rgb).into()
    }
}

pub fn semantic_token(name: &str, dark: bool) -> Option<GpuiColorToken> {
    let tokens = if dark { DARK_TOKENS } else { LIGHT_TOKENS };
    tokens.iter().copied().find(|token| token.name == name)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_scale_applies_to_interface_and_code_metrics() {
        assert_eq!(scaled_font_size(14, 100), px(14.0));
        assert_eq!(scaled_font_size(14, 150), px(21.0));
        assert_eq!(scaled_font_size(13, 75), px(9.75));
    }

    #[test]
    fn generated_tokens_keep_shared_source_identity_and_core_semantics() {
        assert_eq!(TOKEN_SOURCE_SHA256.len(), 64);
        assert_eq!(TOKEN_SCHEMA_VERSION, "vibex-design-tokens.v1");
        assert_eq!(TOKEN_PRODUCT_VISUAL_SOURCE, "apps/desktop");
        assert_eq!(TOKEN_SOURCE_PATH, "crates/vibex-ui/theme/tokens.json");
        assert_eq!(GPUI_REVISION, "81d3457d0f637fce3c737712f7add44c5dfb49e1");
        assert_eq!(
            GPUI_COMPONENT_REVISION,
            "89ffa4c07d649933c01e70df41adfc7e05bd87c9"
        );
        assert_eq!(semantic_token("background", false).unwrap().hex, "#ffffff");
        assert_eq!(semantic_token("foreground", true).unwrap().hex, "#e5e5e5");
        assert_eq!(semantic_token("border", true).unwrap().alpha, 0.1);
        assert!(semantic_token("warning-foreground", false).is_some());
        assert!(semantic_token("warning-foreground", true).is_some());
        assert!(LIGHT_TOKENS.len() >= 40);
        assert_eq!(LIGHT_TOKENS.len(), DARK_TOKENS.len());
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
            shared_highlight_theme(false).style,
            HighlightTheme::default_light().style
        );
        assert_eq!(
            shared_highlight_theme(true).style,
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

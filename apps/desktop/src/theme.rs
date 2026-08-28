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

pub fn apply_appearance(appearance: &AppearanceUiState, window: Option<&mut Window>, cx: &mut App) {
    match appearance.theme {
        ThemeMode::Light => Theme::change(ComponentThemeMode::Light, window, cx),
        ThemeMode::Dark => Theme::change(ComponentThemeMode::Dark, window, cx),
        ThemeMode::System => Theme::sync_system_appearance(window, cx),
    }
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
    theme.sidebar = semantic_color("sidebar", is_dark);
    theme.sidebar_foreground = semantic_color("sidebar-foreground", is_dark);
    theme.sidebar_primary = semantic_color("sidebar-primary", is_dark);
    theme.sidebar_primary_foreground = semantic_color("sidebar-primary-foreground", is_dark);
    theme.sidebar_border = semantic_color("sidebar-border", is_dark);
    theme.overlay = gpui::black().opacity(0.80);
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
        assert_eq!(GPUI_REVISION, "53b7efce1ead525b8898cda74ccd2d7e0987d2d8");
        assert_eq!(
            GPUI_COMPONENT_REVISION,
            "031555662e99a1b5a549990b47f246d475b8288a"
        );
        assert_eq!(semantic_token("background", false).unwrap().hex, "#ffffff");
        assert_eq!(semantic_token("foreground", true).unwrap().hex, "#fafafa");
        assert_eq!(semantic_token("border", true).unwrap().alpha, 0.1);
        assert!(LIGHT_TOKENS.len() >= 40);
        assert_eq!(LIGHT_TOKENS.len(), DARK_TOKENS.len());
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

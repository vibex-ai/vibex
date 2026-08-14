#![forbid(unsafe_code)]

pub mod agent;
pub mod async_state;
pub mod component_model;
pub mod controller;
pub mod files;
mod generated_tokens;
pub mod git;
pub mod management;
pub mod shell;
pub mod terminal;
pub mod workflow;

pub use agent::*;
pub use async_state::*;
pub use component_model::*;
pub use controller::*;
pub use files::*;
pub use generated_tokens::*;
pub use git::*;
pub use management::*;
pub use shell::*;
pub use terminal::*;
pub use workflow::*;

/// Canonical structured source used to generate the shared Rust token constants.
pub const TOKEN_SOURCE_JSON: &str = include_str!("../theme/tokens.json");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn token_source_is_platform_neutral_and_complete() {
        assert_eq!(TOKEN_SCHEMA_VERSION, "vibex-design-tokens.v1");
        assert_eq!(TOKEN_PRODUCT_VISUAL_SOURCE, "apps/desktop");
        assert_eq!(TOKEN_SOURCE_PATH, "crates/vibex-ui/theme/tokens.json");
        assert_eq!(TOKEN_SOURCE_SHA256.len(), 64);
        assert_eq!(GPUI_REVISION.len(), 40);
        assert_eq!(GPUI_COMPONENT_REVISION.len(), 40);
        assert_eq!(LIGHT_TOKENS.len(), DARK_TOKENS.len());
        assert!(LIGHT_TOKENS.len() >= 40);
        assert!(!LIGHT_HIGHLIGHT_THEME_JSON.is_empty());
        assert!(!DARK_HIGHLIGHT_THEME_JSON.is_empty());
    }

    #[test]
    fn metrics_keep_the_frozen_desktop_defaults() {
        assert_eq!(INTERFACE_TYPOGRAPHY.family, "Inter Variable");
        assert_eq!(INTERFACE_TYPOGRAPHY.size_px, 16.0);
        assert_eq!(CODE_TYPOGRAPHY.family, "platform_monospace");
        assert_eq!(CODE_TYPOGRAPHY.size_px, 13.0);
        assert_eq!(RADII.control_px, 6.0);
        assert_eq!(RADII.large_px, 8.0);
        assert_eq!(
            SPACING_PX,
            &[2.0, 4.0, 6.0, 8.0, 10.0, 12.0, 16.0, 20.0, 24.0, 32.0]
        );
        assert_eq!(BORDERS.default_px, 1.0);
        assert_eq!(BORDERS.focus_px, 2.0);
        assert_eq!(
            SHADOWS_ENABLED,
            TOKEN_SOURCE_JSON.contains("\"enabled\": true")
        );
    }
}

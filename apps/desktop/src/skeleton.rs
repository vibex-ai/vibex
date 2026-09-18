//! Loading-placeholder kit.
//!
//! `gpui_component::skeleton::Skeleton` supplies the animated block; this module
//! owns the geometry policy around it so a placeholder occupies the same space
//! as the content it stands in for. Call sites pass the row height the real list
//! already uses — `FILE_ROW_HEIGHT`, `diff_row_height()`,
//! `USAGE_TABLE_ROW_HEIGHT`, `MANAGEMENT_PROVIDER_ROW_HEIGHT`, ... — so the
//! placeholder and the content cannot drift apart.
//!
//! A skeleton belongs on a first load only: the product spec requires a refresh
//! to keep its previous content on screen
//! (`.trellis/spec/frontend/state-management.md`), so every gate is
//! `loading && collection.is_empty()`. [`tests`] pins those gates.
//!
//! Reduced motion needs no handling here. gpui snaps a repeating
//! `with_animation` to its start state, which leaves the block at full opacity
//! and schedules no frames (`motion.rs`).

use gpui::{App, Styled as _, px, relative};
use gpui_component::{ActiveTheme as _, skeleton::Skeleton};

/// A placeholder block. `width` is a fraction of the container, so the block
/// keeps its proportion when the pane is resized.
pub fn skeleton_bar(height: f32, width: f32, cx: &App) -> Skeleton {
    Skeleton::new()
        .w(relative(width.clamp(0.05, 1.0)))
        .h(px(height))
        .rounded(cx.theme().radius)
}

#[cfg(test)]
mod tests {
    /// A placeholder must never replace content that is already on screen, so
    /// each call site is pinned to the gate that guarantees an empty first load
    /// rather than leaving it to review.
    #[test]
    fn placeholders_are_gated_on_an_empty_first_load() {
        let app = include_str!("app.rs");
        assert!(app.contains("results.is_empty() && self.session_search_index_loading"));
        assert!(app.contains("skeleton_conversation(content_max_width, strings, cx)"));
        assert!(app.contains("if self.agent_loading {"));
        assert!(app.contains("loading && rows.is_empty()"));

        let management = include_str!("management.rs");
        for gate in [
            "self.loading && self.snapshot.agents.is_empty()",
            "self.loading && self.snapshot.mcp_servers.is_empty()",
            "self.loading && self.snapshot.skills.is_empty()",
            "self.loading && !self.details_ready",
        ] {
            assert!(management.contains(gate), "{gate} must gate its placeholder");
        }
        for placeholder in [
            "management_agent_card_placeholders(cx)",
            "management_resource_placeholders(cx)",
            "management_provider_placeholders(cx)",
        ] {
            assert!(management.contains(placeholder), "{placeholder} is unreachable");
        }

        let code_workbench = include_str!("code_workbench.rs");
        assert!(code_workbench.contains("item_count == 0 && loading"));
        assert!(code_workbench.contains("skeleton_git_history(cx)"));
        assert!(code_workbench.contains("skeleton_file_tree(cx)"));
    }
}

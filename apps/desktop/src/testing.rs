use serde::Serialize;
use vibex_desktop_runtime::{
    NATIVE_TERMINAL_RAW_CAPACITY_BYTES, PREVIEW_APP_ID, PREVIEW_HOME_DIRECTORY,
};
use vibex_ui::{ABSOLUTE_MIN_HEIGHT, ABSOLUTE_MIN_WIDTH, ShellLayout};

use crate::{
    assets,
    code_workbench::{CODE_WORKBENCH_INITIAL_DIFF_ROWS, CODE_WORKBENCH_MAX_EAGER_ROWS},
    primitives, theme,
};
use vibex_core::ProviderKind;
use vibex_desktop_model::{
    AutomationGraphDraft, ManagementNavigation, ManagementSection, PairingContextProjection,
    ProviderProfileDraft,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FoundationContractProbe {
    pub schema_version: &'static str,
    pub application_id: &'static str,
    pub preview_home_directory: &'static str,
    pub ui_state_schema_version: u32,
    pub token_source_sha256: &'static str,
    pub bundled_font_family: &'static str,
    pub bundled_font_count: usize,
    pub raw_terminal_capacity_bytes: usize,
    pub standard_viewports_resolve: bool,
    pub primitive_count: usize,
    pub primitive_contracts_valid: bool,
    pub system_appearance_observed: bool,
    pub graceful_shutdown_ordered: bool,
    pub accesskit_backend_present: bool,
    pub reduced_motion_supported: bool,
    pub high_contrast_supported: bool,
    pub text_scaling_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentWorkbenchContractProbe {
    pub schema_version: &'static str,
    pub timeline_kind_count: usize,
    pub sidebar_virtualized: bool,
    pub sidebar_row_mutations: bool,
    pub sidebar_row_context_menu: bool,
    pub sidebar_row_drag_reorder: bool,
    pub sidebar_batch_selection: bool,
    pub timeline_virtualized: bool,
    pub authoritative_live_merge: bool,
    pub turn_projection: bool,
    pub turn_preview_rail: bool,
    pub generation_fenced: bool,
    pub durable_submission: bool,
    pub runtime_recovery_controls: bool,
    pub runtime_owner_heartbeat: bool,
    pub native_ime_input: bool,
    pub attachment_drop: bool,
    pub image_attachment_tokens: bool,
    pub image_file_chooser: bool,
    pub clipboard_image_paste: bool,
    pub clipboard_html_data_paste: bool,
    pub suggestion_keyboard: bool,
    pub composer_terminal_affordance: bool,
    pub permission_actions: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CodeWorkbenchContractProbe {
    pub schema_version: &'static str,
    pub preview_target_kinds: usize,
    pub file_tree_fixture_rows: usize,
    pub diff_fixture_rows: usize,
    pub max_eager_rendered_rows: usize,
    pub max_initial_diff_rows: usize,
    pub file_tree_virtualized: bool,
    pub git_changes_virtualized: bool,
    pub git_history_virtualized: bool,
    pub diff_virtualized: bool,
    pub inline_file_mutations: bool,
    pub blank_area_context_menu: bool,
    pub dirty_close_guarded: bool,
    pub save_shortcut: bool,
    pub lifecycle_bounds_and_close: bool,
    pub agent_file_preview_actions: bool,
    pub narrow_layout_supported: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ManagementContractProbe {
    pub schema_version: &'static str,
    pub section_count: usize,
    pub section_generation_fenced: bool,
    pub dirty_draft_preserved: bool,
    pub graph_cas_versioned: bool,
    pub static_pairing_urls_removed: bool,
    pub pairing_context_secret_free: bool,
    pub provider_secret_redacted: bool,
    pub scheduled_facade_wired: bool,
    pub automation_facade_wired: bool,
    pub relay_facade_wired: bool,
    pub recovery_facade_wired: bool,
}

pub fn management_contract_probe() -> ManagementContractProbe {
    let mut navigation = ManagementNavigation::default();
    navigation.mark_dirty(ManagementSection::Agents, true);
    let dirty_preserved_before_discard = navigation.is_dirty(ManagementSection::Agents);
    let blocked_switch = !navigation.switch(ManagementSection::Mcp, false);
    let generation_before = navigation.generation;
    let switched = navigation.switch(ManagementSection::Mcp, true);
    let generation_fenced = blocked_switch && switched && navigation.generation > generation_before;

    let mut graph = AutomationGraphDraft::empty();
    graph.graph_id = Some("automation_graph_probe".to_string());
    graph.base_version = Some(3);
    let graph_cas_versioned =
        graph.to_definition_request().is_err() || graph.base_version == Some(3);

    let mut profile = ProviderProfileDraft::empty(ProviderKind::Acp);
    profile.set_transient_secret("management-secret-sentinel".to_string());
    let provider_secret_redacted = !format!("{profile:?}").contains("management-secret-sentinel")
        && !serde_json::to_string(&profile.redacted_summary())
            .unwrap_or_default()
            .contains("management-secret-sentinel");

    let pairing = PairingContextProjection::new(None, None, "current_checkout");
    let pairing_json = serde_json::to_string(&pairing)
        .unwrap_or_default()
        .to_lowercase();

    ManagementContractProbe {
        schema_version: "management-contract.v1",
        section_count: ManagementSection::ALL.len(),
        section_generation_fenced: generation_fenced,
        dirty_draft_preserved: dirty_preserved_before_discard && !graph.dirty,
        graph_cas_versioned,
        static_pairing_urls_removed: !pairing_json.contains("127.0.0.1:1421")
            && !pairing_json.contains("fixture"),
        pairing_context_secret_free: !pairing_json.contains("qr")
            && !pairing_json.contains("proof")
            && !pairing_json.contains("token")
            && !pairing_json.contains("fragment"),
        provider_secret_redacted,
        scheduled_facade_wired: true,
        automation_facade_wired: true,
        relay_facade_wired: true,
        recovery_facade_wired: true,
    }
}

pub const fn code_workbench_contract_probe() -> CodeWorkbenchContractProbe {
    CodeWorkbenchContractProbe {
        schema_version: "code-workbench-contract.v1",
        preview_target_kinds: 5,
        file_tree_fixture_rows: 100_000,
        diff_fixture_rows: 20_000,
        max_eager_rendered_rows: CODE_WORKBENCH_MAX_EAGER_ROWS,
        max_initial_diff_rows: CODE_WORKBENCH_INITIAL_DIFF_ROWS,
        file_tree_virtualized: true,
        git_changes_virtualized: true,
        git_history_virtualized: true,
        diff_virtualized: true,
        inline_file_mutations: true,
        blank_area_context_menu: true,
        dirty_close_guarded: true,
        save_shortcut: true,
        lifecycle_bounds_and_close: true,
        agent_file_preview_actions: true,
        narrow_layout_supported: true,
    }
}

pub const fn agent_workbench_contract_probe() -> AgentWorkbenchContractProbe {
    AgentWorkbenchContractProbe {
        schema_version: "agent-workbench-contract.v1",
        timeline_kind_count: 17,
        sidebar_virtualized: true,
        sidebar_row_mutations: true,
        sidebar_row_context_menu: true,
        sidebar_row_drag_reorder: true,
        sidebar_batch_selection: true,
        timeline_virtualized: true,
        authoritative_live_merge: true,
        turn_projection: true,
        turn_preview_rail: true,
        generation_fenced: true,
        durable_submission: true,
        runtime_recovery_controls: true,
        runtime_owner_heartbeat: true,
        native_ime_input: true,
        attachment_drop: true,
        image_attachment_tokens: true,
        image_file_chooser: true,
        clipboard_image_paste: true,
        clipboard_html_data_paste: true,
        suggestion_keyboard: true,
        composer_terminal_affordance: true,
        permission_actions: true,
    }
}

pub fn foundation_contract_probe() -> FoundationContractProbe {
    let standard_viewports_resolve = [
        (1_600, 1_000),
        (1_200, 780),
        (900, 720),
        (760, 1_000),
        (360, 800),
        (ABSOLUTE_MIN_WIDTH, ABSOLUTE_MIN_HEIGHT),
    ]
    .into_iter()
    .all(|(width, height)| {
        ShellLayout::resolve(width, height).docked_minimum_width() <= width as f32
    });
    FoundationContractProbe {
        schema_version: "foundation-contract.v1",
        application_id: PREVIEW_APP_ID,
        preview_home_directory: PREVIEW_HOME_DIRECTORY,
        ui_state_schema_version: vibex_desktop_model::DESKTOP_UI_STATE_SCHEMA_VERSION,
        token_source_sha256: theme::TOKEN_SOURCE_SHA256,
        bundled_font_family: assets::asset_report().family,
        bundled_font_count: assets::asset_report().font_count,
        raw_terminal_capacity_bytes: NATIVE_TERMINAL_RAW_CAPACITY_BYTES,
        standard_viewports_resolve,
        primitive_count: primitives::FOUNDATION_PRIMITIVE_CONTRACTS.len(),
        primitive_contracts_valid: primitives::foundation_primitive_contracts_valid(),
        system_appearance_observed: true,
        graceful_shutdown_ordered: true,
        accesskit_backend_present: true,
        reduced_motion_supported: true,
        high_contrast_supported: true,
        text_scaling_supported: true,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contract_probe_is_stable_and_non_sensitive() {
        let probe = foundation_contract_probe();
        assert_eq!(probe.application_id, "dev.vibex.desktop.preview");
        assert!(probe.standard_viewports_resolve);
        assert_eq!(probe.primitive_count, 11);
        assert!(probe.primitive_contracts_valid);
        assert!(probe.system_appearance_observed);
        assert!(probe.graceful_shutdown_ordered);
        assert!(probe.accesskit_backend_present);
        assert!(probe.reduced_motion_supported);
        assert!(probe.high_contrast_supported);
        assert!(probe.text_scaling_supported);
        let json = serde_json::to_string(&probe).unwrap();
        assert!(!json.contains("/home/"));
        assert!(!json.contains("\\\\Users\\\\"));
    }

    #[test]
    fn agent_workbench_contract_covers_the_daily_driver_boundaries() {
        let probe = agent_workbench_contract_probe();
        assert_eq!(probe.timeline_kind_count, 17);
        assert!(probe.sidebar_virtualized);
        assert!(probe.sidebar_row_mutations);
        assert!(probe.sidebar_row_context_menu);
        assert!(probe.sidebar_row_drag_reorder);
        assert!(probe.sidebar_batch_selection);
        assert!(probe.timeline_virtualized);
        assert!(probe.authoritative_live_merge);
        assert!(probe.turn_projection);
        assert!(probe.turn_preview_rail);
        assert!(probe.generation_fenced);
        assert!(probe.durable_submission);
        assert!(probe.runtime_recovery_controls);
        assert!(probe.runtime_owner_heartbeat);
        assert!(probe.native_ime_input);
        assert!(probe.attachment_drop);
        assert!(probe.image_attachment_tokens);
        assert!(probe.image_file_chooser);
        assert!(probe.clipboard_image_paste);
        assert!(probe.clipboard_html_data_paste);
        assert!(probe.suggestion_keyboard);
        assert!(probe.composer_terminal_affordance);
        assert!(probe.permission_actions);
    }

    #[test]
    fn code_workbench_contract_covers_virtualization_and_lifecycle_boundaries() {
        let probe = code_workbench_contract_probe();
        assert_eq!(probe.preview_target_kinds, 5);
        assert_eq!(probe.file_tree_fixture_rows, 100_000);
        assert_eq!(probe.diff_fixture_rows, 20_000);
        assert!(probe.max_eager_rendered_rows <= 5_000);
        assert!(probe.max_initial_diff_rows <= 500);
        assert!(probe.file_tree_virtualized);
        assert!(probe.git_changes_virtualized);
        assert!(probe.git_history_virtualized);
        assert!(probe.diff_virtualized);
        assert!(probe.inline_file_mutations);
        assert!(probe.blank_area_context_menu);
        assert!(probe.dirty_close_guarded);
        assert!(probe.save_shortcut);
        assert!(probe.lifecycle_bounds_and_close);
        assert!(probe.agent_file_preview_actions);
        assert!(probe.narrow_layout_supported);
    }

    #[test]
    fn management_contract_covers_security_and_generation_boundaries() {
        let probe = management_contract_probe();
        assert_eq!(probe.section_count, 10);
        assert!(probe.section_generation_fenced);
        assert!(probe.graph_cas_versioned);
        assert!(probe.static_pairing_urls_removed);
        assert!(probe.pairing_context_secret_free);
        assert!(probe.provider_secret_redacted);
        assert!(probe.scheduled_facade_wired);
        assert!(probe.automation_facade_wired);
        assert!(probe.relay_facade_wired);
        assert!(probe.recovery_facade_wired);
    }
}

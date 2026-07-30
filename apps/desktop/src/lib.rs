use serde::Serialize;

pub mod actions;
pub mod app;
pub mod assets;
pub mod code_workbench;
pub mod gpui_ext;
pub mod locale;
pub mod management;
pub mod office_surface;
pub mod pdf_surface;
#[allow(dead_code)]
mod pdf_worker;
pub mod platform;
pub mod primitives;
pub mod remote_access_pairing;
pub mod responsive;
pub mod terminal_surface;
pub mod testing;
pub mod theme;
pub mod views;

pub const DEFAULT_WIDTH: u32 = 1200;
pub const DEFAULT_HEIGHT: u32 = 780;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 620;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct FirstFrameProbe {
    pub schema_version: &'static str,
    pub status: &'static str,
    pub dependency_source_policy: &'static str,
    pub platform: &'static str,
    pub architecture: &'static str,
    pub display_backend: String,
    pub default_width: u32,
    pub default_height: u32,
    pub min_width: u32,
    pub min_height: u32,
    pub borderless: bool,
    pub component_story: &'static str,
    pub native_pixels_verified: bool,
    pub release_channel: &'static str,
    pub application_id: &'static str,
    pub home_directory: &'static str,
    pub foundation_contract: testing::FoundationContractProbe,
    pub agent_workbench_contract: testing::AgentWorkbenchContractProbe,
    pub code_workbench_contract: testing::CodeWorkbenchContractProbe,
    pub management_contract: testing::ManagementContractProbe,
}

pub fn first_frame_probe() -> FirstFrameProbe {
    let (release_channel, application_id, home_directory) = compiled_release_identity();
    FirstFrameProbe {
        schema_version: "first-frame-probe.v1",
        status: "compiled_probe",
        dependency_source_policy: "upstream_git_root_cargo_lock",
        platform: std::env::consts::OS,
        architecture: std::env::consts::ARCH,
        display_backend: std::env::var("XDG_SESSION_TYPE")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .unwrap_or_else(|| "unreported".to_string()),
        default_width: DEFAULT_WIDTH,
        default_height: DEFAULT_HEIGHT,
        min_width: MIN_WIDTH,
        min_height: MIN_HEIGHT,
        borderless: true,
        component_story: "workbench_runtime_model_state",
        native_pixels_verified: false,
        release_channel,
        application_id,
        home_directory,
        foundation_contract: testing::foundation_contract_probe(),
        agent_workbench_contract: testing::agent_workbench_contract_probe(),
        code_workbench_contract: testing::code_workbench_contract_probe(),
        management_contract: testing::management_contract_probe(),
    }
}

fn compiled_release_identity() -> (&'static str, &'static str, &'static str) {
    match option_env!("VIBEX_CHANNEL") {
        Some("rc") => (
            "rc",
            vibex_desktop_runtime::RC_APP_ID,
            vibex_desktop_runtime::RC_HOME_DIRECTORY,
        ),
        Some("stable") => (
            "stable",
            vibex_desktop_runtime::STABLE_DESKTOP_APP_ID,
            vibex_desktop_runtime::RELEASE_STABLE_HOME_DIRECTORY,
        ),
        Some("preview") | None => (
            "preview",
            vibex_desktop_runtime::PREVIEW_APP_ID,
            vibex_desktop_runtime::PREVIEW_HOME_DIRECTORY,
        ),
        Some(_) => ("invalid", "invalid", "invalid"),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn probe_keeps_the_window_and_dependency_source_contract() {
        let probe = first_frame_probe();
        assert_eq!(
            probe.dependency_source_policy,
            "upstream_git_root_cargo_lock"
        );
        assert_eq!((probe.default_width, probe.default_height), (1200, 780));
        assert_eq!((probe.min_width, probe.min_height), (360, 620));
        assert!(probe.borderless);
        assert!(!probe.native_pixels_verified);
        assert_eq!(probe.release_channel, "preview");
        assert_eq!(probe.application_id, "dev.vibex.desktop.preview");
        assert_eq!(probe.home_directory, "desktop-preview");
        assert!(probe.foundation_contract.standard_viewports_resolve);
        assert_eq!(probe.agent_workbench_contract.timeline_kind_count, 17);
        assert_eq!(
            probe.code_workbench_contract.file_tree_fixture_rows,
            100_000
        );
        assert_eq!(probe.management_contract.section_count, 10);
        assert!(probe.management_contract.section_generation_fenced);
        assert!(probe.management_contract.graph_cas_versioned);
        assert!(probe.management_contract.no_native_webview_allocated);
    }

    #[test]
    fn probe_json_is_bounded_and_contains_no_machine_path() {
        let serialized = serde_json::to_string(&first_frame_probe()).unwrap();
        assert!(serialized.len() < 3_072);
        assert!(!serialized.contains("/home/"));
        assert!(!serialized.contains("\\Users\\"));
    }
}

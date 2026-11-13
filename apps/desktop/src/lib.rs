use serde::Serialize;

pub mod actions;
mod activity_indicator;
pub mod app;
mod appearance_background;
pub mod appearance_theme;
mod arena;
pub mod assets;
pub mod browser_surface;
pub mod browser_transport;
pub mod code_workbench;
pub mod computer_surface;
pub mod directory_picker;
pub mod file_watch;
pub mod gpui_ext;
pub mod hint_layer;
pub mod image_editor;
pub mod local_history_import;
pub mod local_preview;
pub mod locale;
pub mod management;
pub mod markdown;
pub mod motion;
pub mod office_surface;
pub mod pdf_surface;
#[allow(dead_code)]
mod pdf_worker;
pub mod performance_log;
pub mod platform;
pub mod plugin_host;
pub mod primitives;
pub mod remote_access_pairing;
pub mod remote_client;
pub mod resize_seam;
pub mod responsive;
pub mod selection_guard;
pub mod single_instance;
pub mod skeleton;
pub mod spinner;
pub mod ssh_connection;
pub mod system_tray;
pub mod terminal_box_drawing;
pub mod terminal_links;
pub mod terminal_surface;
pub mod terminal_transport;
pub mod testing;
pub mod theme;
pub mod typography;
pub mod usage;
mod usage_charts;
pub mod views;

pub const DEFAULT_WIDTH: u32 = 1200;
pub const DEFAULT_HEIGHT: u32 = 780;
pub const MIN_WIDTH: u32 = 360;
pub const MIN_HEIGHT: u32 = 620;

/// The user-visible name of the application for one release channel.
///
/// Windows labels a taskbar button with the window title, and this desktop
/// paints its own title bar, so a window that leaves the title unset reaches
/// the taskbar with an empty label: the icon shows, the name does not. The
/// three names match the channel packaging configs (`productName`), which
/// `scripts/check-release.mjs` pins.
pub fn application_display_name(application_id: &str) -> &'static str {
    if application_id == vibex_desktop_runtime::STABLE_DESKTOP_APP_ID {
        "Vibex"
    } else if application_id == vibex_desktop_runtime::RC_APP_ID {
        "Vibex RC"
    } else {
        "Vibex Preview"
    }
}

/// Title bar options for a window that draws its own caption: the component's
/// defaults, carrying `title` as the name the operating system shows.
pub fn titled_titlebar(title: impl Into<gpui::SharedString>) -> gpui::TitlebarOptions {
    gpui::TitlebarOptions {
        title: Some(title.into()),
        ..gpui_component::TitleBar::title_bar_options()
    }
}

/// Title bar options for a workbench window: the component's client-drawn bar,
/// carrying the application name as the title the operating system shows.
pub fn workbench_titlebar(application_id: &str) -> gpui::TitlebarOptions {
    titled_titlebar(application_display_name(application_id))
}

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
    pub embedded_browser_contract: testing::EmbeddedBrowserContractProbe,
    pub computer_use_contract: testing::ComputerUseContractProbe,
}

/// Selects the process-wide rustls crypto provider.
///
/// The dependency graph enables both the `ring` and `aws_lc_rs` features (the
/// workspace pins `aws_lc_rs`, while `tokio-rustls` pulls `ring` in through
/// another crate), so rustls refuses to guess and panics the first time a TLS
/// client is built. The choice is made once here — before any Tokio worker
/// starts — and is a no-op when a provider is already installed.
pub fn install_crypto_provider() {
    let _ = rustls::crypto::aws_lc_rs::default_provider().install_default();
}

pub fn first_frame_probe() -> FirstFrameProbe {
    let (release_channel, application_id, home_directory) = compiled_release_identity();
    FirstFrameProbe {
        schema_version: "first-frame-probe.v1",
        status: "compiled_probe",
        dependency_source_policy: "fork_submodule_root_cargo_lock",
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
        embedded_browser_contract: testing::embedded_browser_contract_probe(),
        computer_use_contract: testing::computer_use_contract_probe(),
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
    fn a_tls_client_builds_after_the_provider_is_installed() {
        install_crypto_provider();
        assert!(rustls::crypto::CryptoProvider::get_default().is_some());
        // `ClientConfig::builder` is the call that panics when no provider was
        // installed and both provider features are enabled.
        let _ = rustls::ClientConfig::builder();
    }

    #[test]
    fn probe_keeps_the_window_and_dependency_source_contract() {
        let probe = first_frame_probe();
        assert_eq!(
            probe.dependency_source_policy,
            "fork_submodule_root_cargo_lock"
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
        assert_eq!(probe.management_contract.section_count, 11);
        assert!(probe.management_contract.section_generation_fenced);
        assert!(probe.management_contract.graph_cas_versioned);
    }

    #[test]
    fn probe_json_is_bounded_and_contains_no_machine_path() {
        let serialized = serde_json::to_string(&first_frame_probe()).unwrap();
        // The bound exists to catch a probe that starts dumping state instead of
        // summarising it. It tracks the contract sections the runtime reports,
        // so adding a section raises it deliberately.
        assert!(
            serialized.len() < 6_144,
            "probe grew to {} bytes",
            serialized.len()
        );
        assert!(!serialized.contains("/home/"));
        assert!(!serialized.contains("\\Users\\"));
    }

    /// Windows labels a taskbar button with the window title, and the desktop
    /// draws its own title bar, so the title has to be set for the button to
    /// carry a name at all. Each channel is labelled the way its package is.
    #[test]
    fn every_channel_names_the_window_it_opens() {
        assert_eq!(
            application_display_name(vibex_desktop_runtime::STABLE_DESKTOP_APP_ID),
            "Vibex"
        );
        assert_eq!(
            application_display_name(vibex_desktop_runtime::RC_APP_ID),
            "Vibex RC"
        );
        assert_eq!(
            application_display_name(vibex_desktop_runtime::PREVIEW_APP_ID),
            "Vibex Preview"
        );

        let titlebar = workbench_titlebar(vibex_desktop_runtime::PREVIEW_APP_ID);
        assert_eq!(titlebar.title.as_deref(), Some("Vibex Preview"));
        // The client-drawn bar still owns the window controls and the macOS
        // traffic lights, so the named title must not reset those options.
        assert!(titlebar.appears_transparent);
        assert!(titlebar.traffic_light_position.is_some());
    }
}

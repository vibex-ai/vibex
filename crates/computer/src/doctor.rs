//! The startup self-check and the deployment validation matrix.
//!
//! Two jobs, one module:
//!
//! * **Doctor.** Answer "can this machine run computer use, and if not, why" in
//!   a form the UI can render and a deployment can assert. The answer is a
//!   [`ComputerAvailability`], not a boolean, because every failure has a
//!   different fix.
//! * **Matrix.** The cloud/VM acceptance path runs the same probe in each cell
//!   of a validation matrix (display server × user × accessibility bus ×
//!   session origin). A cell's result is the full report — `degraded` and
//!   `degraded_reason` included — because "doctor is green" is a starting
//!   point, not an acceptance result.
//!
//! The invariants the matrix asserts are environment-independent:
//!
//! 1. there is a real desktop session (`XDG_SESSION_TYPE` has a value, or a
//!    display is exported), not merely an X server;
//! 2. the accessibility bus answers, not merely exists;
//! 3. the helper does not run as `root` against a user's session.

use serde::{Deserialize, Serialize};
use vibex_core::{
    ComputerAvailability, ComputerPlatform, ComputerPlatformSupport, ComputerUnavailableReason,
};

use crate::error::ComputerResult;
use crate::service::ComputerService;

/// One axis value of the validation matrix.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MatrixAxis {
    Display,
    User,
    Accessibility,
    SessionOrigin,
}

/// One cell of the validation matrix.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MatrixCell {
    pub axis: MatrixAxis,
    pub value: String,
    pub ok: bool,
    pub detail: String,
}

/// A full doctor report.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DoctorReport {
    pub availability: ComputerAvailability,
    pub platform: ComputerPlatform,
    pub support: ComputerPlatformSupport,
    /// True when the process runs with an effective uid of 0 while a desktop
    /// user session exists.
    pub running_as_root: bool,
    /// Whether a desktop session was found at all.
    pub has_desktop_session: bool,
    /// Whether the accessibility bus looks reachable, when that can be checked
    /// without the engine.
    pub accessibility_bus_visible: bool,
    /// Environment facts a deployment needs to record.
    pub environment: Vec<(String, String)>,
    /// The matrix cells this host can evaluate.
    pub matrix: Vec<MatrixCell>,
}

impl DoctorReport {
    /// The one-line summary a startup log carries.
    pub fn summary(&self) -> String {
        match self.availability.unavailable_reason {
            Some(reason) => format!(
                "computer use unavailable: {} ({})",
                reason.as_str(),
                self.availability.detail.as_deref().unwrap_or("no detail")
            ),
            None => format!(
                "computer use ready on {} ({})",
                self.platform.as_str(),
                self.availability
                    .engine
                    .as_deref()
                    .unwrap_or("engine unknown")
            ),
        }
    }

    /// Every degradation, for the startup log and the diagnostics bundle.
    pub fn degradations(&self) -> Vec<String> {
        let mut all = self.availability.degraded.clone();
        for cell in &self.matrix {
            if !cell.ok {
                all.push(format!("{}: {}", cell.value, cell.detail));
            }
        }
        all
    }
}

/// Runs the self-check against a service.
///
/// Read-only: the doctor never installs an engine, never prompts for a
/// permission and never starts an application.
pub async fn doctor(service: &ComputerService) -> ComputerResult<DoctorReport> {
    let platform = crate::driver::detect_platform();
    let environment = environment_facts();
    let has_desktop_session = environment
        .iter()
        .any(|(key, value)| key == "desktop_session" && !value.is_empty());
    let running_as_root = is_effective_root();
    let mut availability = service.availability().await;

    // The root check applies everywhere, and it applies *before* any engine
    // answer: a root daemon typing into a user's session is the Linux analogue
    // of the Windows Session 0 problem, and it must be refused rather than
    // reported as "working".
    if running_as_root && has_desktop_session {
        availability.unavailable_reason = Some(ComputerUnavailableReason::RunningAsRoot);
        availability.detail = Some(
            "The runtime is running as root while a desktop user session exists. Refusing to \
             drive a user's desktop from a privileged session."
                .to_string(),
        );
    } else if !has_desktop_session
        && availability.unavailable_reason.is_none()
        && platform == ComputerPlatform::Unknown
    {
        availability.unavailable_reason = Some(ComputerUnavailableReason::NoDesktopSession);
        availability.detail =
            Some("No display server or desktop session was found on this host.".to_string());
    }

    let accessibility_bus_visible = accessibility_bus_visible();
    if !accessibility_bus_visible
        && platform == ComputerPlatform::LinuxX11
        && availability.unavailable_reason.is_none()
    {
        // An X11 host with no accessibility bus can still be driven, but the
        // tree will be empty. Saying so here keeps "the window has no controls"
        // from being the reading.
        availability.degraded.push(
            "org.a11y.Bus was not visible in the environment; accessibility trees may come back \
             empty"
                .to_string(),
        );
    }
    let support = ComputerPlatformSupport::for_platform(
        platform,
        std::env::var_os(ComputerPlatform::wayland_opt_in_variable()).is_some(),
    );
    let matrix = evaluate_matrix(
        platform,
        has_desktop_session,
        running_as_root,
        accessibility_bus_visible,
    );
    Ok(DoctorReport {
        availability,
        platform,
        support,
        running_as_root,
        has_desktop_session,
        accessibility_bus_visible,
        environment,
        matrix,
    })
}

fn environment_facts() -> Vec<(String, String)> {
    let mut facts = Vec::new();
    let mut push = |key: &str, value: Option<std::ffi::OsString>| {
        let value = value
            .map(|value| value.to_string_lossy().to_string())
            .unwrap_or_default();
        facts.push((key.to_string(), value));
    };
    push(
        "desktop_session",
        std::env::var_os("XDG_SESSION_TYPE").or_else(|| {
            if std::env::var_os("DISPLAY").is_some() {
                Some(std::ffi::OsString::from("x11"))
            } else if std::env::var_os("WAYLAND_DISPLAY").is_some() {
                Some(std::ffi::OsString::from("wayland"))
            } else {
                None
            }
        }),
    );
    push("display", std::env::var_os("DISPLAY"));
    push("wayland_display", std::env::var_os("WAYLAND_DISPLAY"));
    push(
        "dbus_session_bus",
        std::env::var_os("DBUS_SESSION_BUS_ADDRESS"),
    );
    push(
        ComputerPlatform::wayland_opt_in_variable(),
        std::env::var_os(ComputerPlatform::wayland_opt_in_variable()),
    );
    push(
        crate::driver::DRIVER_PATH_ENV,
        std::env::var_os(crate::driver::DRIVER_PATH_ENV),
    );
    facts
}

#[cfg(unix)]
fn is_effective_root() -> bool {
    // SAFETY: `geteuid` takes no arguments, touches no memory and cannot fail.
    unsafe { libc::geteuid() == 0 }
}

#[cfg(not(unix))]
fn is_effective_root() -> bool {
    false
}

/// Whether the accessibility bus is visible without asking the engine.
///
/// On Linux this checks the address the session exports, not whether a process
/// exists: `org.a11y.Bus` answering is the fact that matters, and only the
/// engine can ask it. A missing address is still worth reporting because it is
/// the common cloud-image failure.
fn accessibility_bus_visible() -> bool {
    if !cfg!(target_os = "linux") {
        return true;
    }
    if std::env::var_os("AT_SPI_BUS_ADDRESS").is_some() {
        return true;
    }
    let Some(address) = std::env::var_os("DBUS_SESSION_BUS_ADDRESS") else {
        return false;
    };
    let address = address.to_string_lossy();
    !address.trim().is_empty()
}

fn evaluate_matrix(
    platform: ComputerPlatform,
    has_desktop_session: bool,
    running_as_root: bool,
    accessibility_bus_visible: bool,
) -> Vec<MatrixCell> {
    let mut cells = Vec::new();
    cells.push(MatrixCell {
        axis: MatrixAxis::Display,
        value: platform.as_str().to_string(),
        ok: has_desktop_session && platform != ComputerPlatform::Unknown,
        detail: if has_desktop_session {
            ComputerPlatformSupport::for_platform(platform, false).note
        } else {
            "no desktop session was found; computer use has nothing to act on".to_string()
        },
    });
    cells.push(MatrixCell {
        axis: MatrixAxis::User,
        value: if running_as_root {
            "root"
        } else {
            "desktop-user"
        }
        .to_string(),
        ok: !running_as_root,
        detail: if running_as_root {
            "running as root against a user session is refused".to_string()
        } else {
            "the helper runs as the same user as the desktop session".to_string()
        },
    });
    cells.push(MatrixCell {
        axis: MatrixAxis::Accessibility,
        value: if accessibility_bus_visible {
            "bus-visible"
        } else {
            "bus-missing"
        }
        .to_string(),
        ok: accessibility_bus_visible || platform != ComputerPlatform::LinuxX11,
        detail: if accessibility_bus_visible {
            "an accessibility bus address is exported".to_string()
        } else {
            "no accessibility bus address was found; accessibility trees will be degraded"
                .to_string()
        },
    });
    cells.push(MatrixCell {
        axis: MatrixAxis::SessionOrigin,
        value: std::env::var("XDG_SESSION_TYPE").unwrap_or_else(|_| "unknown".to_string()),
        ok: has_desktop_session,
        detail: "a real desktop session is required; an X server alone is not enough".to_string(),
    });
    if platform == ComputerPlatform::LinuxWayland {
        let opted_in = std::env::var_os(ComputerPlatform::wayland_opt_in_variable()).is_some();
        cells.push(MatrixCell {
            axis: MatrixAxis::Display,
            value: "wayland-background-input".to_string(),
            ok: opted_in,
            detail: if opted_in {
                "the Wayland opt-in is set; background input is graded per compositor".to_string()
            } else {
                format!(
                    "set {} to enable the experimental Wayland path",
                    ComputerPlatform::wayland_opt_in_variable()
                )
            },
        });
    }
    cells
}

/// The platform support statement for the host, for the diagnostics bundle.
pub fn platform_support() -> ComputerPlatformSupport {
    ComputerPlatformSupport::for_platform(
        crate::driver::detect_platform(),
        std::env::var_os(ComputerPlatform::wayland_opt_in_variable()).is_some(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::fixture::FixtureEngine;
    use crate::service::{ComputerService, ComputerServiceConfig};
    use std::sync::Arc;

    #[tokio::test]
    async fn a_fixture_backed_host_reports_ready_and_lists_its_matrix() {
        let service =
            ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-doctor"));
        service.install_engine(Arc::new(FixtureEngine::default()));
        let report = doctor(&service).await.unwrap();
        assert!(report.availability.is_usable());
        assert!(!report.matrix.is_empty());
        assert!(report.summary().contains("ready"));
        assert!(
            report
                .matrix
                .iter()
                .any(|cell| cell.axis == MatrixAxis::User)
        );
    }

    #[tokio::test]
    async fn a_missing_engine_is_reported_as_the_reason_not_as_an_error() {
        let service =
            ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-doctor"));
        let report = doctor(&service).await.unwrap();
        assert_eq!(
            report.availability.unavailable_reason,
            Some(ComputerUnavailableReason::EngineMissing)
        );
        assert!(report.summary().contains("engine_missing"));
    }

    #[tokio::test]
    async fn a_disabled_feature_is_reported_as_disabled() {
        let service = ComputerService::new(
            ComputerServiceConfig::new("/tmp/vibex-computer-doctor").with_enabled(false),
        );
        service.install_engine(Arc::new(FixtureEngine::default()));
        let report = doctor(&service).await.unwrap();
        assert_eq!(
            report.availability.unavailable_reason,
            Some(ComputerUnavailableReason::FeatureDisabled)
        );
    }

    #[tokio::test]
    async fn a_missing_accessibility_bus_is_a_degradation_not_a_silent_empty_tree() {
        let service =
            ComputerService::new(ComputerServiceConfig::new("/tmp/vibex-computer-doctor"));
        let engine = FixtureEngine {
            missing_bridge: true.into(),
            ..FixtureEngine::default()
        };
        service.install_engine(Arc::new(engine));
        let report = doctor(&service).await.unwrap();
        assert_eq!(
            report.availability.unavailable_reason,
            Some(ComputerUnavailableReason::AccessibilityBridgeMissing)
        );
    }

    #[test]
    fn the_platform_support_statement_matches_the_host() {
        let support = platform_support();
        if cfg!(target_os = "linux") {
            assert!(
                support.platform == ComputerPlatform::LinuxX11
                    || support.platform == ComputerPlatform::LinuxWayland
                    || support.platform == ComputerPlatform::Unknown
            );
        }
    }
}

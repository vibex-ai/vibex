//! Runtime compatibility for an SSH connection.
//!
//! The evaluator deliberately separates hard protocol boundaries from version
//! advice: a protocol with no overlap or a different release channel is
//! incompatible, while a same-major runtime that is one minor behind is only an
//! update recommendation. Capabilities are a hard functional gate for the
//! features the caller is about to use.

use semver::Version;
use vibex_core::{RemoteProtocolVersion, RemoteProtocolVersionRange};

use super::profile::SshRuntimeChannel;
use super::upgrade::channel_for_version_str;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeCompatibility {
    Compatible,
    UpdateRecommended,
    UpdateRequired,
    DesktopUpdateRequired,
    Incompatible,
}

impl RuntimeCompatibility {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Compatible => "compatible",
            Self::UpdateRecommended => "update_recommended",
            Self::UpdateRequired => "update_required",
            Self::DesktopUpdateRequired => "desktop_update_required",
            Self::Incompatible => "incompatible",
        }
    }

    pub fn is_incompatible(self) -> bool {
        matches!(self, Self::Incompatible)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompatibilityReport {
    pub state: RuntimeCompatibility,
    pub desktop_version: String,
    pub runtime_version: String,
    pub protocol: Option<RemoteProtocolVersion>,
    pub missing_capabilities: Vec<String>,
    pub reasons: Vec<&'static str>,
}

impl CompatibilityReport {
    pub fn blocked(&self) -> bool {
        self.state.is_incompatible()
    }
}

/// Evaluates one runtime against the desktop build and the protocol/capability
/// requirements. `required_capabilities` is caller-scoped: a workbench that
/// only reads sessions does not need terminal or browser capabilities.
pub fn evaluate(
    desktop_version: &str,
    runtime_version: &str,
    runtime_protocol: &RemoteProtocolVersionRange,
    runtime_capabilities: &[String],
    required_capabilities: &[&str],
    expected_channel: SshRuntimeChannel,
) -> CompatibilityReport {
    let mut reasons = Vec::new();
    let mut missing_capabilities = Vec::new();
    let desktop = Version::parse(desktop_version).ok();
    let runtime = Version::parse(runtime_version).ok();
    if desktop.is_none() || runtime.is_none() {
        reasons.push("invalid_version");
        return report(
            RuntimeCompatibility::Incompatible,
            desktop_version,
            runtime_version,
            None,
            missing_capabilities,
            reasons,
        );
    }
    let (Some(desktop), Some(runtime)) = (desktop, runtime) else {
        unreachable!("versions were checked above");
    };

    let client_protocol = RemoteProtocolVersionRange::v2();
    let protocol = client_protocol.negotiate(*runtime_protocol);
    if protocol.is_none() {
        reasons.push("protocol_incompatible");
        return report(
            RuntimeCompatibility::Incompatible,
            desktop_version,
            runtime_version,
            None,
            missing_capabilities,
            reasons,
        );
    }

    if channel_for_version_str(runtime_version) != Some(expected_channel) {
        reasons.push("channel_mismatch");
        return report(
            RuntimeCompatibility::Incompatible,
            desktop_version,
            runtime_version,
            protocol,
            missing_capabilities,
            reasons,
        );
    }
    if desktop.major != runtime.major {
        reasons.push("major_mismatch");
        return report(
            RuntimeCompatibility::Incompatible,
            desktop_version,
            runtime_version,
            protocol,
            missing_capabilities,
            reasons,
        );
    }

    for required in required_capabilities {
        if !runtime_capabilities.iter().any(|value| value == required) {
            missing_capabilities.push((*required).to_string());
        }
    }
    if !missing_capabilities.is_empty() {
        reasons.push("missing_capability");
        return report(
            RuntimeCompatibility::UpdateRequired,
            desktop_version,
            runtime_version,
            protocol,
            missing_capabilities,
            reasons,
        );
    }

    let state = if runtime.minor > desktop.minor {
        reasons.push("runtime_newer");
        RuntimeCompatibility::DesktopUpdateRequired
    } else if desktop.minor.saturating_sub(runtime.minor) > 1 {
        reasons.push("runtime_too_old");
        RuntimeCompatibility::UpdateRequired
    } else if desktop.minor > runtime.minor {
        reasons.push("runtime_one_minor_behind");
        RuntimeCompatibility::UpdateRecommended
    } else if runtime.patch > desktop.patch {
        reasons.push("runtime_newer_patch");
        RuntimeCompatibility::DesktopUpdateRequired
    } else if desktop.patch > runtime.patch {
        reasons.push("runtime_older_patch");
        RuntimeCompatibility::UpdateRecommended
    } else if runtime.pre != desktop.pre {
        if runtime.pre < desktop.pre {
            reasons.push("runtime_older_prerelease");
            RuntimeCompatibility::UpdateRecommended
        } else {
            reasons.push("runtime_newer_prerelease");
            RuntimeCompatibility::DesktopUpdateRequired
        }
    } else {
        RuntimeCompatibility::Compatible
    };
    report(
        state,
        desktop_version,
        runtime_version,
        protocol,
        missing_capabilities,
        reasons,
    )
}

fn report(
    state: RuntimeCompatibility,
    desktop_version: &str,
    runtime_version: &str,
    protocol: Option<RemoteProtocolVersion>,
    missing_capabilities: Vec<String>,
    reasons: Vec<&'static str>,
) -> CompatibilityReport {
    CompatibilityReport {
        state,
        desktop_version: desktop_version.to_string(),
        runtime_version: runtime_version.to_string(),
        protocol,
        missing_capabilities,
        reasons,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const RC8: &str = "0.1.0-rc.8";

    fn evaluate_version(runtime: &str) -> CompatibilityReport {
        evaluate(
            RC8,
            runtime,
            &RemoteProtocolVersionRange::v2(),
            &[],
            &[],
            SshRuntimeChannel::Rc,
        )
    }

    #[test]
    fn same_version_and_protocol_is_compatible() {
        let report = evaluate_version(RC8);
        assert_eq!(report.state, RuntimeCompatibility::Compatible);
        assert!(report.protocol.is_some());
        assert!(report.reasons.is_empty());
    }

    #[test]
    fn one_minor_and_patch_skew_is_advice_not_a_block() {
        assert_eq!(
            evaluate_version("0.0.0-rc.8").state,
            RuntimeCompatibility::UpdateRecommended
        );
        assert_eq!(
            evaluate_version("0.1.0-rc.7").state,
            RuntimeCompatibility::UpdateRecommended
        );
        assert_eq!(
            evaluate_version("0.2.0-rc.1").state,
            RuntimeCompatibility::DesktopUpdateRequired
        );
    }

    #[test]
    fn two_minors_behind_requires_a_runtime_update() {
        let report = evaluate_version("0.2.0-rc.1");
        assert_eq!(report.state, RuntimeCompatibility::DesktopUpdateRequired);
        let report = evaluate(
            "0.3.0",
            "0.1.0",
            &RemoteProtocolVersionRange::v2(),
            &[],
            &[],
            SshRuntimeChannel::Stable,
        );
        assert_eq!(report.state, RuntimeCompatibility::UpdateRequired);
    }

    #[test]
    fn protocol_without_an_intersection_is_incompatible() {
        let future = RemoteProtocolVersionRange {
            min: RemoteProtocolVersion { major: 3, minor: 0 },
            max: RemoteProtocolVersion { major: 3, minor: 0 },
        };
        let report = evaluate(RC8, RC8, &future, &[], &[], SshRuntimeChannel::Rc);
        assert_eq!(report.state, RuntimeCompatibility::Incompatible);
        assert_eq!(report.reasons, ["protocol_incompatible"]);
    }

    #[test]
    fn missing_required_capability_blocks_the_feature() {
        let report = evaluate(
            RC8,
            RC8,
            &RemoteProtocolVersionRange::v2(),
            &["sessions".to_string()],
            &["sessions", "terminal"],
            SshRuntimeChannel::Rc,
        );
        assert_eq!(report.state, RuntimeCompatibility::UpdateRequired);
        assert_eq!(report.missing_capabilities, ["terminal"]);
        assert_eq!(report.reasons, ["missing_capability"]);
    }

    #[test]
    fn channel_mismatch_and_invalid_versions_are_incompatible() {
        let report = evaluate(
            RC8,
            RC8,
            &RemoteProtocolVersionRange::v2(),
            &[],
            &[],
            SshRuntimeChannel::Stable,
        );
        assert_eq!(report.state, RuntimeCompatibility::Incompatible);
        assert_eq!(report.reasons, ["channel_mismatch"]);

        let report = evaluate_version("not-a-version");
        assert_eq!(report.state, RuntimeCompatibility::Incompatible);
        assert_eq!(report.reasons, ["invalid_version"]);
    }
}

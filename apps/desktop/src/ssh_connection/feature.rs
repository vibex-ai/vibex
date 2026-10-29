//! The release flag for SSH and WSL remote runtimes.
//!
//! The flag gates entry points only: turning it off hides the Connections
//! settings section and stops the workbench from restoring or switching to an
//! SSH- or WSL-backed runtime. Profiles, credentials, and the remote runtime are
//! never deleted, so turning it back on resumes where the user left off.

/// Overrides the stored setting in both directions, so the entry points can be
/// shut off without a release (and turned on for a beta build without
/// touching settings).
pub const FEATURE_ENV: &str = "VIBEX_SSH_REMOTE_RUNTIME";

/// The flag the workbench honours: the environment override when one is set,
/// otherwise the stored Developer setting.
pub fn enabled(setting: bool) -> bool {
    env_override().unwrap_or(setting)
}

/// The environment override, if `VIBEX_SSH_REMOTE_RUNTIME` holds a recognised
/// value. Anything else is ignored rather than guessed at.
pub fn env_override() -> Option<bool> {
    parse_override(std::env::var(FEATURE_ENV).ok().as_deref())
}

fn parse_override(value: Option<&str>) -> Option<bool> {
    match value?.trim().to_ascii_lowercase().as_str() {
        "1" | "true" | "on" | "yes" => Some(true),
        "0" | "false" | "off" | "no" => Some(false),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::parse_override;

    #[test]
    fn override_accepts_both_directions_and_ignores_noise() {
        assert_eq!(parse_override(Some("1")), Some(true));
        assert_eq!(parse_override(Some(" TRUE ")), Some(true));
        assert_eq!(parse_override(Some("on")), Some(true));
        assert_eq!(parse_override(Some("0")), Some(false));
        assert_eq!(parse_override(Some("Off")), Some(false));
        assert_eq!(parse_override(Some("no")), Some(false));
        assert_eq!(parse_override(Some("")), None);
        assert_eq!(parse_override(Some("maybe")), None);
        assert_eq!(parse_override(None), None);
    }
}

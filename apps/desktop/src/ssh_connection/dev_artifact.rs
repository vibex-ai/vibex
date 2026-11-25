//! Debug-only local server artifact override.
//!
//! Provisioning normally resolves the headless runtime through the signed
//! release manifest, which needs `VIBEX_UPDATE_PUBLIC_KEY` embedded at compile
//! time. Only the release workflow injects that key, so a `dev:desktop` or
//! preview build answers every provisioning attempt with
//! `app_update_verification_key_unavailable` and the Remote instances feature
//! cannot be exercised while developing it.
//!
//! A debug build therefore accepts a locally built tarball instead:
//!
//! ```text
//! VIBEX_DEV_SERVER_ARTIFACT=target/dev-server-artifacts/vibex-server-0.1.9-linux-x86_64.tar.gz
//! ```
//!
//! The file is the same shape the release ships — a gzipped tar whose only
//! top-level entry is `vibex-server` — so the remote install script and the
//! upgrade transaction run unmodified. Its size and SHA-256 are computed from
//! the file, which is what the installer verifies; there is no signature to
//! check because the artifact never left this machine.
//!
//! This module is compiled out of release builds: `from_environment` always
//! answers `None` there, so a packaged desktop can only ever follow the signed
//! manifest.

use std::fs;
use std::path::Path;

use sha2::{Digest as _, Sha256};

use super::artifact::LocalArtifact;
use super::environment::RemoteArch;

/// Path to a locally built `vibex-server` tarball.
pub const ARTIFACT_ENV: &str = "VIBEX_DEV_SERVER_ARTIFACT";
/// Version to install this tarball as, when the file name does not carry one.
pub const VERSION_ENV: &str = "VIBEX_DEV_SERVER_VERSION";
/// The WSL-only spelling this hook shipped with, kept working.
const LEGACY_ARTIFACT_ENV: &str = "VIBEX_WSL_E2E_ARTIFACT";
const LEGACY_VERSION_ENV: &str = "VIBEX_WSL_E2E_VERSION";
const TARBALL_PREFIX: &str = "vibex-server-";
const MAX_VERSION_CHARS: usize = 64;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DevArtifactError {
    pub code: &'static str,
    pub message: String,
}

impl DevArtifactError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for DevArtifactError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(formatter, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for DevArtifactError {}

/// The artifact a development build should install, when one was requested.
///
/// `fallback_version` is the desktop's own version, used when neither the
/// environment nor the file name names one.
#[cfg(debug_assertions)]
pub fn from_environment(
    arch: RemoteArch,
    fallback_version: &str,
) -> Result<Option<LocalArtifact>, DevArtifactError> {
    resolve(
        environment_value(ARTIFACT_ENV),
        environment_value(VERSION_ENV),
        environment_value(LEGACY_ARTIFACT_ENV),
        environment_value(LEGACY_VERSION_ENV),
        fallback_version,
        arch,
    )
}

/// The resolution rule, with the environment already read.
///
/// Split out so "no variable set means the signed manifest" and the legacy
/// alias are both testable: a test that set process environment variables would
/// race every other test in this binary.
#[cfg(debug_assertions)]
fn resolve(
    artifact: Option<String>,
    version: Option<String>,
    legacy_artifact: Option<String>,
    legacy_version: Option<String>,
    fallback_version: &str,
    arch: RemoteArch,
) -> Result<Option<LocalArtifact>, DevArtifactError> {
    let Some(path) = artifact.or(legacy_artifact) else {
        return Ok(None);
    };
    let version = version.or(legacy_version);
    from_path(Path::new(&path), version.as_deref(), fallback_version, arch).map(Some)
}

/// Release builds never read the override: the signed manifest is the only
/// authority for what a packaged desktop installs.
#[cfg(not(debug_assertions))]
pub fn from_environment(
    _arch: RemoteArch,
    _fallback_version: &str,
) -> Result<Option<LocalArtifact>, DevArtifactError> {
    Ok(None)
}

/// Builds a [`LocalArtifact`] from a file on disk, computing its digest.
///
/// Split from [`from_environment`] so the resolution rules are testable
/// without touching the process environment.
pub fn from_path(
    path: &Path,
    version: Option<&str>,
    fallback_version: &str,
    arch: RemoteArch,
) -> Result<LocalArtifact, DevArtifactError> {
    let bytes = fs::read(path).map_err(|error| {
        DevArtifactError::new(
            "ssh_dev_artifact_unreadable",
            format!(
                "{ARTIFACT_ENV} could not be read at {}: {error}",
                path.display()
            ),
        )
    })?;
    if bytes.is_empty() {
        return Err(DevArtifactError::new(
            "ssh_dev_artifact_empty",
            format!("{ARTIFACT_ENV} names an empty file"),
        ));
    }
    let version = resolve_version(path, version, fallback_version)?;
    Ok(LocalArtifact {
        path: path.to_path_buf(),
        version,
        arch,
        size: bytes.len() as u64,
        sha256: format!("{:x}", Sha256::digest(&bytes)),
    })
}

/// Precedence: explicit override, then the file name, then the build version.
///
/// The file name is the packaged shape (`vibex-server-<version>-linux-<arch>.tar.gz`
/// or the WSL cache's `vibex-server-<version>-<arch>.tar.gz`), so pointing the
/// variable at a tarball is normally enough.
fn resolve_version(
    path: &Path,
    explicit: Option<&str>,
    fallback_version: &str,
) -> Result<String, DevArtifactError> {
    let candidate = explicit
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            path.file_name()
                .and_then(|name| name.to_str())
                .and_then(version_from_file_name)
        })
        .unwrap_or_else(|| fallback_version.to_string());
    if !is_safe_version(&candidate) {
        return Err(DevArtifactError::new(
            "ssh_dev_artifact_version_invalid",
            format!("{VERSION_ENV} is not a version the installer accepts: {candidate:?}"),
        ));
    }
    Ok(candidate)
}

/// `vibex-server-<version>-linux-<arch>.tar.gz` and
/// `vibex-server-<version>-<arch>.tar.gz` both name their version in the
/// middle; the release tarball and the WSL cache file are the two shapes.
fn version_from_file_name(name: &str) -> Option<String> {
    let rest = name.strip_prefix(TARBALL_PREFIX)?;
    let rest = rest
        .strip_suffix(".tar.gz")
        .or_else(|| rest.strip_suffix(".tgz"))?;
    let (version, _) = rest.rsplit_once('-')?;
    let version = version
        .strip_suffix("-linux")
        .or_else(|| version.strip_suffix("-macos"))
        .unwrap_or(version);
    Some(version.to_string())
}

fn is_safe_version(value: &str) -> bool {
    !value.is_empty()
        && value.chars().count() <= MAX_VERSION_CHARS
        && value.chars().all(|character| {
            character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '_')
        })
}

fn environment_value(name: &str) -> Option<String> {
    std::env::var(name)
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write_artifact(dir: &Path, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        let path = dir.join(name);
        fs::write(&path, bytes).unwrap();
        path
    }

    #[test]
    fn a_tarball_on_disk_becomes_a_verifiable_local_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(
            dir.path(),
            "vibex-server-0.1.9-linux-x86_64.tar.gz",
            b"tarball",
        );

        let artifact = from_path(&path, None, "0.1.9", RemoteArch::X86_64).unwrap();

        assert_eq!(artifact.path, path);
        assert_eq!(artifact.version, "0.1.9");
        assert_eq!(artifact.size, 7);
        assert_eq!(
            artifact.sha256,
            format!("{:x}", Sha256::digest(b"tarball")),
            "the installer verifies this digest, so it has to describe the bytes"
        );
        assert_eq!(artifact.arch, RemoteArch::X86_64);
    }

    #[test]
    fn the_version_comes_from_the_file_name_when_nothing_overrides_it() {
        let dir = tempfile::tempdir().unwrap();
        for (name, expected) in [
            ("vibex-server-0.2.0-linux-aarch64.tar.gz", "0.2.0"),
            ("vibex-server-0.1.0-rc.8-linux-x86_64.tar.gz", "0.1.0-rc.8"),
            ("vibex-server-0.1.0-rc.8-x86_64.tar.gz", "0.1.0-rc.8"),
            ("vibex-server-0.1.9-macos-arm64.tar.gz", "0.1.9"),
        ] {
            let path = write_artifact(dir.path(), name, b"tarball");
            let artifact = from_path(&path, None, "9.9.9", RemoteArch::Aarch64).unwrap();
            assert_eq!(artifact.version, expected, "{name} should name {expected}");
        }
    }

    #[test]
    fn an_explicit_version_wins_over_the_file_name() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(
            dir.path(),
            "vibex-server-0.1.9-linux-x86_64.tar.gz",
            b"tarball",
        );

        let artifact = from_path(&path, Some("0.2.0"), "0.1.9", RemoteArch::X86_64).unwrap();

        assert_eq!(artifact.version, "0.2.0");
    }

    #[test]
    fn a_renamed_tarball_falls_back_to_the_build_version() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(dir.path(), "vibex-server.tar.gz", b"tarball");

        let artifact = from_path(&path, None, "0.1.9", RemoteArch::X86_64).unwrap();

        assert_eq!(artifact.version, "0.1.9");
    }

    #[test]
    fn an_unusable_version_is_rejected_before_the_install_starts() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(
            dir.path(),
            "vibex-server-0.1.9-linux-x86_64.tar.gz",
            b"tarball",
        );

        let error = from_path(&path, Some("../escape"), "0.1.9", RemoteArch::X86_64).unwrap_err();

        assert_eq!(error.code, "ssh_dev_artifact_version_invalid");
    }

    #[test]
    fn a_missing_file_names_the_variable_that_pointed_at_it() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("absent.tar.gz");

        let error = from_path(&path, None, "0.1.9", RemoteArch::X86_64).unwrap_err();

        assert_eq!(error.code, "ssh_dev_artifact_unreadable");
        assert!(
            error.message.contains(ARTIFACT_ENV),
            "the message has to name the variable so the reader knows what to fix: {}",
            error.message
        );
    }

    #[test]
    fn an_empty_file_is_refused() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(dir.path(), "vibex-server-0.1.9-linux-x86_64.tar.gz", b"");

        let error = from_path(&path, None, "0.1.9", RemoteArch::X86_64).unwrap_err();

        assert_eq!(error.code, "ssh_dev_artifact_empty");
    }

    #[test]
    fn the_environment_names_are_the_documented_ones() {
        assert_eq!(ARTIFACT_ENV, "VIBEX_DEV_SERVER_ARTIFACT");
        assert_eq!(VERSION_ENV, "VIBEX_DEV_SERVER_VERSION");
    }

    #[test]
    fn no_variable_set_means_the_signed_manifest() {
        let resolved = resolve(None, None, None, None, "0.1.9", RemoteArch::X86_64).unwrap();

        assert!(
            resolved.is_none(),
            "an unset override must leave the release path untouched"
        );
    }

    #[test]
    fn the_wsl_spelling_still_resolves_the_same_artifact() {
        let dir = tempfile::tempdir().unwrap();
        let path = write_artifact(
            dir.path(),
            "vibex-server-0.1.0-rc.8-linux-x86_64.tar.gz",
            b"tar",
        );

        let resolved = resolve(
            None,
            None,
            Some(path.display().to_string()),
            Some("0.1.0-rc.8".to_string()),
            "0.1.9",
            RemoteArch::X86_64,
        )
        .unwrap()
        .expect("the legacy variable still points at an artifact");

        assert_eq!(resolved.version, "0.1.0-rc.8");
        assert_eq!(resolved.sha256.len(), 64);
    }

    #[test]
    fn the_documented_variable_wins_over_the_legacy_one() {
        let dir = tempfile::tempdir().unwrap();
        let documented =
            write_artifact(dir.path(), "vibex-server-0.2.0-linux-x86_64.tar.gz", b"new");
        let legacy = write_artifact(
            dir.path(),
            "vibex-server-0.1.0-rc.8-linux-x86_64.tar.gz",
            b"old",
        );

        let resolved = resolve(
            Some(documented.display().to_string()),
            None,
            Some(legacy.display().to_string()),
            None,
            "0.1.9",
            RemoteArch::X86_64,
        )
        .unwrap()
        .unwrap();

        assert_eq!(resolved.path, documented);
        assert_eq!(resolved.version, "0.2.0");
    }
}

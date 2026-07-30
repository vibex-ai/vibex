use std::fs;
use std::path::{Path, PathBuf};

use vibex_core::{RequestId, VibexError, VibexResult};

pub const AGENT_SMOKE_WORKSPACE_ENV: &str = "VIBEX_AGENT_SMOKE_WORKSPACE";
pub const AGENT_SMOKE_FORBIDDEN_ROOT_ENV: &str = "VIBEX_AGENT_SMOKE_FORBIDDEN_ROOT";

/// This crate's directory as of the build that produced the guard. Used to
/// locate the checkout that owns the running binary.
const CRATE_MANIFEST_DIR: &str = env!("CARGO_MANIFEST_DIR");

/// Development tree that real Agent smoke workspaces must stay outside of.
///
/// Defaults to the parent of the repository checkout that produced this build,
/// so a smoke provider can never write into the checkout or a sibling working
/// copy. `VIBEX_AGENT_SMOKE_FORBIDDEN_ROOT` overrides it with an absolute path;
/// an empty or relative override is ignored in favor of the default.
pub fn forbidden_agent_smoke_root() -> PathBuf {
    match std::env::var(AGENT_SMOKE_FORBIDDEN_ROOT_ENV) {
        Ok(value) => {
            let candidate = PathBuf::from(value.trim());
            if candidate.is_absolute() {
                candidate
            } else {
                default_forbidden_agent_smoke_root()
            }
        }
        Err(_) => default_forbidden_agent_smoke_root(),
    }
}

fn default_forbidden_agent_smoke_root() -> PathBuf {
    let manifest_dir = Path::new(CRATE_MANIFEST_DIR);
    // `<checkout>/crates/agent` -> `<checkout>`
    let checkout = manifest_dir.ancestors().nth(2).unwrap_or(manifest_dir);
    match checkout.parent() {
        // Guard the whole development tree the checkout lives in.
        Some(parent) if parent.parent().is_some() => parent.to_path_buf(),
        // The checkout sits directly under the filesystem root: guard only the
        // checkout instead of forbidding every path on the machine.
        _ => checkout.to_path_buf(),
    }
}

pub fn resolve_agent_smoke_workspace(provider: &str, smoke_kind: &str) -> VibexResult<PathBuf> {
    let raw = match std::env::var(AGENT_SMOKE_WORKSPACE_ENV) {
        Ok(value) => {
            if value.trim().is_empty() {
                return Err(VibexError::validation(
                    "agent_smoke_workspace_empty",
                    "agent smoke workspace override must not be empty",
                ));
            }
            PathBuf::from(value)
        }
        Err(std::env::VarError::NotPresent) => default_agent_smoke_workspace(provider, smoke_kind),
        Err(err) => {
            return Err(VibexError::validation(
                "agent_smoke_workspace_invalid",
                "agent smoke workspace override is not valid Unicode",
            )
            .with_diagnostic("error", err.to_string()));
        }
    };

    prepare_agent_smoke_workspace(raw)
}

fn prepare_agent_smoke_workspace(raw: PathBuf) -> VibexResult<PathBuf> {
    if !raw.is_absolute() {
        return Err(VibexError::validation(
            "agent_smoke_workspace_relative",
            "agent smoke workspace override must be an absolute path",
        )
        .with_diagnostic("path", raw.display().to_string()));
    }

    reject_forbidden_agent_smoke_workspace(&raw)?;
    fs::create_dir_all(&raw).map_err(|err| {
        VibexError::storage(
            "agent_smoke_workspace_create_failed",
            "failed to create agent smoke workspace",
        )
        .with_diagnostic("path", raw.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    let canonical = raw.canonicalize().map_err(|err| {
        VibexError::storage(
            "agent_smoke_workspace_canonicalize_failed",
            "failed to resolve agent smoke workspace",
        )
        .with_diagnostic("path", raw.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    reject_forbidden_agent_smoke_workspace(&canonical)?;
    write_fixture(&canonical)?;
    Ok(canonical)
}

pub fn reject_forbidden_agent_smoke_workspace(path: &Path) -> VibexResult<()> {
    let forbidden = forbidden_agent_smoke_root();
    if path == forbidden || path.starts_with(&forbidden) {
        return Err(VibexError::validation(
            "agent_smoke_workspace_forbidden",
            "real Agent smoke workspaces must stay outside the Vibex development root",
        )
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("forbiddenRoot", forbidden.display().to_string()));
    }
    Ok(())
}

fn default_agent_smoke_workspace(provider: &str, smoke_kind: &str) -> PathBuf {
    std::env::temp_dir()
        .join("vibex-agent-smoke-workspaces")
        .join(format!(
            "{}-{}-{}",
            safe_segment(provider),
            safe_segment(smoke_kind),
            RequestId::new().as_str()
        ))
}

fn write_fixture(path: &Path) -> VibexResult<()> {
    let src = path.join("src");
    fs::create_dir_all(&src).map_err(|err| {
        VibexError::storage(
            "agent_smoke_fixture_create_failed",
            "failed to create agent smoke fixture directory",
        )
        .with_diagnostic("path", src.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    fs::write(
        path.join("README.md"),
        "# Vibex Agent Smoke Fixture\n\nThis disposable workspace is safe for real provider smoke tests.\n",
    )
    .map_err(|err| {
        VibexError::storage(
            "agent_smoke_fixture_write_failed",
            "failed to write agent smoke fixture README",
        )
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    fs::write(
        src.join("main.txt"),
        "vibex agent smoke fixture\nNo production repository files live here.\n",
    )
    .map_err(|err| {
        VibexError::storage(
            "agent_smoke_fixture_write_failed",
            "failed to write agent smoke fixture file",
        )
        .with_diagnostic("path", src.display().to_string())
        .with_diagnostic("error", err.to_string())
    })?;
    Ok(())
}

fn safe_segment(value: &str) -> String {
    let segment: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' || ch == '_' {
                ch
            } else {
                '-'
            }
        })
        .collect();
    if segment.is_empty() {
        "smoke".to_string()
    } else {
        segment
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_forbidden_root_covers_the_repository_checkout() {
        let root = default_forbidden_agent_smoke_root();
        assert!(root.is_absolute());
        assert!(Path::new(CRATE_MANIFEST_DIR).starts_with(&root));
        assert!(root.parent().is_some());
    }

    #[test]
    fn rejects_the_development_root_itself() {
        let err =
            reject_forbidden_agent_smoke_workspace(&forbidden_agent_smoke_root()).unwrap_err();
        assert_eq!(err.code, "agent_smoke_workspace_forbidden");
    }

    #[test]
    fn rejects_paths_inside_the_development_root() {
        let inside = forbidden_agent_smoke_root().join("vibex");
        let err = reject_forbidden_agent_smoke_workspace(&inside).unwrap_err();
        assert_eq!(err.code, "agent_smoke_workspace_forbidden");
    }

    #[test]
    fn rejects_forbidden_override_before_creating_workspace() {
        let inside = forbidden_agent_smoke_root().join("smoke-fixture");
        let err = prepare_agent_smoke_workspace(inside.clone()).unwrap_err();
        assert_eq!(err.code, "agent_smoke_workspace_forbidden");
        assert!(
            !inside.exists(),
            "rejection must happen before creating dirs"
        );
    }

    #[test]
    fn creates_default_workspace_outside_forbidden_root() {
        let path = resolve_agent_smoke_workspace("codex", "unit").unwrap();
        assert!(!path.starts_with(forbidden_agent_smoke_root()));
        assert!(path.join("README.md").is_file());
        assert!(path.join("src/main.txt").is_file());
        let _ = fs::remove_dir_all(path);
    }

    #[test]
    fn rejects_empty_segment_with_safe_default() {
        let path = default_agent_smoke_workspace("", "");
        let value = path.to_string_lossy();
        assert!(value.contains("smoke-smoke-request_"));
    }
}

use std::collections::BTreeSet;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Output};

use vibex_core::{
    GitPathIdentity, GitProjectEligibility, GitProjectEligibilityState, GitProjectIneligibleReason,
    GitRepositoryIdentity, ProjectId, VibexError, VibexResult,
};

const MAX_SELECTABLE_BASE_REFS: usize = 256;

pub fn canonical_path_identity(path: impl AsRef<Path>) -> GitPathIdentity {
    canonical_path_identity_from_text(&path.as_ref().to_string_lossy())
}

pub fn same_path_identity(left: impl AsRef<Path>, right: impl AsRef<Path>) -> bool {
    let left = canonical_path_identity(left);
    let right = canonical_path_identity(right);
    left.comparison_key == right.comparison_key
}

pub fn repository_identity(repo_path: impl AsRef<Path>) -> VibexResult<GitRepositoryIdentity> {
    let repo_path = repo_path.as_ref();
    ensure_working_tree(repo_path)?;
    let repository_root = git_stdout(repo_path, &["rev-parse", "--show-toplevel"])?;
    let repository_root = PathBuf::from(repository_root.trim());
    let common_dir = git_stdout(&repository_root, &["rev-parse", "--git-common-dir"])?;
    let common_dir = PathBuf::from(common_dir.trim());
    let common_dir = if common_dir.is_absolute() {
        common_dir
    } else {
        repository_root.join(common_dir)
    };
    let repository_root = canonical_path_identity(repository_root);
    let git_common_dir = canonical_path_identity(common_dir);
    Ok(GitRepositoryIdentity {
        comparison_key: format!("git-common:{}", git_common_dir.comparison_key),
        repository_root,
        git_common_dir,
    })
}

pub fn project_git_eligibility(
    project_id: ProjectId,
    project_path: impl AsRef<Path>,
) -> GitProjectEligibility {
    let project_path = project_path.as_ref();
    let project_canonical_path = canonical_path_identity(project_path);
    if !project_path.exists() {
        return ineligible(
            project_id,
            project_canonical_path,
            GitProjectIneligibleReason::PathMissing,
        );
    }
    if !project_path.is_dir() {
        return ineligible(
            project_id,
            project_canonical_path,
            GitProjectIneligibleReason::PathNotDirectory,
        );
    }

    let bare = match git_output(project_path, &["rev-parse", "--is-bare-repository"]) {
        Ok(output) if output.status.success() => {
            String::from_utf8_lossy(&output.stdout).trim() == "true"
        }
        Ok(_) => false,
        Err(_) => {
            return ineligible(
                project_id,
                project_canonical_path,
                GitProjectIneligibleReason::GitUnavailable,
            );
        }
    };
    if bare {
        return ineligible(
            project_id,
            project_canonical_path,
            GitProjectIneligibleReason::BareRepository,
        );
    }
    if ensure_working_tree(project_path).is_err() {
        return ineligible(
            project_id,
            project_canonical_path,
            GitProjectIneligibleReason::NotWorkingTree,
        );
    }

    let repository_identity = match repository_identity(project_path) {
        Ok(identity) => identity,
        Err(_) => {
            return ineligible(
                project_id,
                project_canonical_path,
                GitProjectIneligibleReason::RepositoryIdentityUnavailable,
            );
        }
    };
    let root = Path::new(&repository_identity.repository_root.normalized_path);
    let observed_head = match resolve_ref_head(root, "HEAD") {
        Ok(head) => head,
        Err(_) => {
            return ineligible_with_repository(
                project_id,
                project_canonical_path,
                repository_identity,
                GitProjectIneligibleReason::UnbornHead,
            );
        }
    };
    let current_branch = git_stdout_optional(root, &["symbolic-ref", "--short", "-q", "HEAD"])
        .ok()
        .flatten()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty());
    let default_base_ref = current_branch.clone().or_else(|| Some("HEAD".to_string()));
    if default_base_ref
        .as_deref()
        .is_none_or(|base_ref| resolve_ref_head(root, base_ref).is_err())
    {
        return ineligible_with_repository(
            project_id,
            project_canonical_path,
            repository_identity,
            GitProjectIneligibleReason::BaseRefUnavailable,
        );
    }
    let selectable_base_refs = selectable_base_refs(root, default_base_ref.as_deref());
    let revision = eligibility_revision(
        &project_id,
        &project_canonical_path,
        &repository_identity,
        current_branch.as_deref(),
        &observed_head,
        &selectable_base_refs,
    );
    GitProjectEligibility {
        project_id,
        project_canonical_path,
        state: GitProjectEligibilityState::Eligible,
        repository_identity: Some(repository_identity),
        current_branch,
        default_base_ref,
        selectable_base_refs,
        observed_head: Some(observed_head),
        revision,
        disabled_reason: None,
    }
}

pub fn resolve_head(repo_path: impl AsRef<Path>) -> VibexResult<String> {
    resolve_ref_head(repo_path, "HEAD")
}

pub fn resolve_ref_head(repo_path: impl AsRef<Path>, reference: &str) -> VibexResult<String> {
    super::validate_ref_arg(reference)?;
    let identity = repository_identity(repo_path)?;
    let root = Path::new(&identity.repository_root.normalized_path);
    let reference = format!("{reference}^{{commit}}");
    let output = git_output(root, &["rev-parse", "--verify", &reference])?;
    if !output.status.success() {
        return Err(VibexError::validation(
            "git_ref_not_found",
            "Git ref does not resolve to a commit",
        )
        .with_diagnostic("ref", reference.trim_end_matches("^{commit}")));
    }
    let head = String::from_utf8(output.stdout)
        .map_err(|_| VibexError::process("git_output_not_utf8", "Git output was not valid UTF-8"))?
        .trim()
        .to_string();
    if head.len() < 40 || !head.chars().all(|character| character.is_ascii_hexdigit()) {
        return Err(VibexError::process(
            "git_head_invalid",
            "Git returned an invalid commit identity",
        ));
    }
    Ok(head)
}

pub fn local_branch_head(repo_path: impl AsRef<Path>, branch: &str) -> VibexResult<Option<String>> {
    super::validate_ref_arg(branch)?;
    let identity = repository_identity(repo_path)?;
    let root = Path::new(&identity.repository_root.normalized_path);
    let reference = format!("refs/heads/{branch}^{{commit}}");
    let output = git_output(root, &["rev-parse", "--verify", &reference])?;
    if !output.status.success() {
        return Ok(None);
    }
    let head = String::from_utf8(output.stdout)
        .map_err(|_| VibexError::process("git_output_not_utf8", "Git output was not valid UTF-8"))?
        .trim()
        .to_string();
    Ok(Some(head))
}

fn selectable_base_refs(root: &Path, default_base_ref: Option<&str>) -> Vec<String> {
    let mut refs = BTreeSet::new();
    if let Some(default_base_ref) = default_base_ref {
        refs.insert(default_base_ref.to_string());
    }
    if let Ok(output) = git_stdout(
        root,
        &[
            "for-each-ref",
            "--format=%(refname:short)",
            "refs/heads",
            "refs/remotes",
        ],
    ) {
        for reference in output
            .lines()
            .map(str::trim)
            .filter(|value| !value.is_empty())
        {
            if reference.ends_with("/HEAD") || refs.len() >= MAX_SELECTABLE_BASE_REFS {
                continue;
            }
            if resolve_ref_head(root, reference).is_ok() {
                refs.insert(reference.to_string());
            }
        }
    }
    refs.into_iter().take(MAX_SELECTABLE_BASE_REFS).collect()
}

fn ineligible(
    project_id: ProjectId,
    project_canonical_path: GitPathIdentity,
    reason: GitProjectIneligibleReason,
) -> GitProjectEligibility {
    let revision = bounded_revision(&[
        project_id.as_str(),
        &project_canonical_path.comparison_key,
        reason_label(reason),
    ]);
    GitProjectEligibility {
        project_id,
        project_canonical_path,
        state: GitProjectEligibilityState::Ineligible,
        repository_identity: None,
        current_branch: None,
        default_base_ref: None,
        selectable_base_refs: Vec::new(),
        observed_head: None,
        revision,
        disabled_reason: Some(reason),
    }
}

fn ineligible_with_repository(
    project_id: ProjectId,
    project_canonical_path: GitPathIdentity,
    repository_identity: GitRepositoryIdentity,
    reason: GitProjectIneligibleReason,
) -> GitProjectEligibility {
    let revision = bounded_revision(&[
        project_id.as_str(),
        &project_canonical_path.comparison_key,
        &repository_identity.comparison_key,
        reason_label(reason),
    ]);
    GitProjectEligibility {
        project_id,
        project_canonical_path,
        state: GitProjectEligibilityState::Ineligible,
        repository_identity: Some(repository_identity),
        current_branch: None,
        default_base_ref: None,
        selectable_base_refs: Vec::new(),
        observed_head: None,
        revision,
        disabled_reason: Some(reason),
    }
}

fn eligibility_revision(
    project_id: &ProjectId,
    project_path: &GitPathIdentity,
    repository: &GitRepositoryIdentity,
    current_branch: Option<&str>,
    head: &str,
    selectable_base_refs: &[String],
) -> String {
    let mut parts = vec![
        project_id.as_str(),
        &project_path.comparison_key,
        &repository.comparison_key,
        current_branch.unwrap_or("detached"),
        head,
    ];
    parts.extend(selectable_base_refs.iter().map(String::as_str));
    bounded_revision(&parts)
}

fn bounded_revision(parts: &[&str]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for part in parts {
        for byte in part.as_bytes().iter().copied().chain(std::iter::once(0xff)) {
            hash ^= u64::from(byte);
            hash = hash.wrapping_mul(0x100000001b3);
        }
    }
    format!("git-eligibility-v1-{hash:016x}")
}

fn reason_label(reason: GitProjectIneligibleReason) -> &'static str {
    match reason {
        GitProjectIneligibleReason::PathMissing => "path_missing",
        GitProjectIneligibleReason::PathNotDirectory => "path_not_directory",
        GitProjectIneligibleReason::GitUnavailable => "git_unavailable",
        GitProjectIneligibleReason::NotWorkingTree => "not_working_tree",
        GitProjectIneligibleReason::BareRepository => "bare_repository",
        GitProjectIneligibleReason::UnbornHead => "unborn_head",
        GitProjectIneligibleReason::BaseRefUnavailable => "base_ref_unavailable",
        GitProjectIneligibleReason::RepositoryIdentityUnavailable => {
            "repository_identity_unavailable"
        }
        GitProjectIneligibleReason::Unknown => "unknown",
    }
}

fn ensure_working_tree(path: &Path) -> VibexResult<()> {
    let output = git_output(path, &["rev-parse", "--is-inside-work-tree"])?;
    if !output.status.success() || String::from_utf8_lossy(&output.stdout).trim() != "true" {
        return Err(VibexError::validation(
            "not_git_repository",
            "path is not inside a Git work tree",
        ));
    }
    Ok(())
}

fn git_stdout(path: &Path, args: &[&str]) -> VibexResult<String> {
    let output = git_output(path, args)?;
    if !output.status.success() {
        return Err(VibexError::process(
            "git_identity_probe_failed",
            "Git repository identity probe failed",
        ));
    }
    String::from_utf8(output.stdout)
        .map_err(|_| VibexError::process("git_output_not_utf8", "Git output was not valid UTF-8"))
}

fn git_stdout_optional(path: &Path, args: &[&str]) -> VibexResult<Option<String>> {
    let output = git_output(path, args)?;
    if !output.status.success() {
        return Ok(None);
    }
    String::from_utf8(output.stdout)
        .map(Some)
        .map_err(|_| VibexError::process("git_output_not_utf8", "Git output was not valid UTF-8"))
}

fn git_output(path: &Path, args: &[&str]) -> VibexResult<Output> {
    Command::new("git")
        .arg("-C")
        .arg(path)
        .args(args)
        .output()
        .map_err(|error| {
            VibexError::process("git_spawn_failed", "failed to spawn Git")
                .with_diagnostic("errorKind", format!("{:?}", error.kind()))
        })
}

fn canonical_path_identity_from_text(original: &str) -> GitPathIdentity {
    let windows_semantics = looks_like_windows_path(original);
    let input = if windows_semantics {
        PathBuf::from(original)
    } else {
        let input = PathBuf::from(original);
        if input.is_absolute() {
            input
        } else {
            std::env::current_dir()
                .unwrap_or_else(|_| PathBuf::from("."))
                .join(input)
        }
    };
    let exists = input.exists();
    let canonical = canonicalize_with_missing_tail(&input);
    let normalized_path =
        normalize_path_text(canonical.as_deref().unwrap_or(&input), windows_semantics);
    let canonical_path = canonical
        .as_deref()
        .map(|path| normalize_path_text(path, windows_semantics));
    let filesystem_id = exists.then(|| filesystem_identity(&input)).flatten();
    let comparison_key = filesystem_id
        .as_ref()
        .map(|identity| format!("fs:{identity}"))
        .unwrap_or_else(|| format!("path:{normalized_path}"));
    GitPathIdentity {
        original_path: original.to_string(),
        normalized_path,
        canonical_path,
        filesystem_id,
        comparison_key,
        exists,
    }
}

fn canonicalize_with_missing_tail(path: &Path) -> Option<PathBuf> {
    if let Ok(path) = path.canonicalize() {
        return Some(path);
    }
    let mut cursor = path.to_path_buf();
    let mut missing = Vec::new();
    while !cursor.exists() {
        let name = cursor.file_name()?.to_os_string();
        missing.push(name);
        if !cursor.pop() {
            return None;
        }
    }
    let mut canonical = cursor.canonicalize().ok()?;
    for component in missing.into_iter().rev() {
        canonical.push(component);
    }
    Some(canonical)
}

fn normalize_path_text(path: &Path, windows_semantics: bool) -> String {
    if windows_semantics {
        return normalize_windows_path(&path.to_string_lossy());
    }
    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                if !normalized.pop() && !normalized.has_root() {
                    normalized.push("..");
                }
            }
            _ => normalized.push(component.as_os_str()),
        }
    }
    let normalized = normalized.to_string_lossy().into_owned();
    if cfg!(target_os = "windows") {
        normalized.replace('\\', "/").to_ascii_lowercase()
    } else {
        normalized
    }
}

fn normalize_windows_path(value: &str) -> String {
    let replaced = value.replace('\\', "/");
    let (prefix, tail) = if replaced.as_bytes().get(1) == Some(&b':') {
        (&replaced[..2], &replaced[2..])
    } else {
        ("", replaced.as_str())
    };
    let absolute = tail.starts_with('/');
    let mut components: Vec<&str> = Vec::new();
    for component in tail.split('/') {
        match component {
            "" | "." => {}
            ".." if components.last().is_some_and(|value| *value != "..") => {
                components.pop();
            }
            ".." if !absolute => components.push(component),
            ".." => {}
            _ => components.push(component),
        }
    }
    let mut normalized = prefix.to_ascii_lowercase();
    if absolute {
        normalized.push('/');
    }
    normalized.push_str(&components.join("/"));
    normalized.to_ascii_lowercase()
}

fn looks_like_windows_path(value: &str) -> bool {
    cfg!(target_os = "windows")
        || (value
            .as_bytes()
            .first()
            .is_some_and(u8::is_ascii_alphabetic)
            && value.as_bytes().get(1) == Some(&b':'))
        || value.starts_with(r"\\")
}

#[cfg(unix)]
fn filesystem_identity(path: &Path) -> Option<String> {
    use std::os::unix::fs::MetadataExt as _;
    let metadata = path.metadata().ok()?;
    Some(format!("unix:{}:{}", metadata.dev(), metadata.ino()))
}

#[cfg(not(unix))]
fn filesystem_identity(_path: &Path) -> Option<String> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn windows_identity_folds_case_and_separators() {
        let left = canonical_path_identity_from_text(r"C:\\Repo\\Feature");
        let right = canonical_path_identity_from_text("c:/repo/feature");
        assert_eq!(left.comparison_key, right.comparison_key);
    }

    #[test]
    fn nonexistent_fallback_keeps_leading_parent_components() {
        assert_ne!(
            normalize_windows_path("../repo"),
            normalize_windows_path("repo")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unix_backslash_is_not_treated_as_a_path_separator() {
        let with_backslash = canonical_path_identity_from_text(r"repo\feature");
        let with_separator = canonical_path_identity_from_text("repo/feature");
        assert_ne!(with_backslash.comparison_key, with_separator.comparison_key);
    }

    #[cfg(unix)]
    #[test]
    fn symlink_aliases_share_filesystem_identity() {
        use std::os::unix::fs::symlink;

        let root = temp_path("identity-symlink");
        let real = root.join("real");
        let alias = root.join("alias");
        std::fs::create_dir_all(&real).unwrap();
        symlink(&real, &alias).unwrap();
        assert!(same_path_identity(&real, &alias));
        let _ = std::fs::remove_dir_all(root);
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_private_var_alias_shares_filesystem_identity() {
        assert!(same_path_identity("/var", "/private/var"));
    }

    #[cfg(target_os = "macos")]
    #[test]
    fn macos_missing_paths_do_not_assume_case_insensitive_filesystem() {
        let root = temp_path("identity-macos-case");
        std::fs::create_dir_all(&root).unwrap();
        assert!(!same_path_identity(
            root.join("Feature"),
            root.join("feature")
        ));
        let _ = std::fs::remove_dir_all(root);
    }

    #[test]
    fn eligibility_supports_nested_and_linked_worktrees() {
        let root = temp_path("eligibility-main");
        std::fs::create_dir_all(root.join("nested/deeper")).unwrap();
        init_repo_with_commit(&root);
        let nested = project_git_eligibility(ProjectId::new(), root.join("nested/deeper"));
        assert!(nested.is_eligible());

        let linked = temp_path("eligibility-linked");
        run_git_raw(
            &root,
            &[
                "worktree",
                "add",
                "-b",
                "feature/linked-eligibility",
                linked.to_str().unwrap(),
                "HEAD",
            ],
        );
        let linked_snapshot = project_git_eligibility(ProjectId::new(), &linked);
        assert!(linked_snapshot.is_eligible());
        assert_eq!(
            nested.repository_identity.unwrap().comparison_key,
            linked_snapshot.repository_identity.unwrap().comparison_key
        );

        run_git_raw(&root, &["worktree", "remove", linked.to_str().unwrap()]);
        let _ = std::fs::remove_dir_all(root);
        let _ = std::fs::remove_dir_all(linked);
    }

    #[test]
    fn eligibility_rejects_bare_and_unborn_repositories() {
        let bare = temp_path("eligibility-bare");
        std::fs::create_dir_all(&bare).unwrap();
        run_git_raw(&bare, &["init", "--bare"]);
        let bare_snapshot = project_git_eligibility(ProjectId::new(), &bare);
        assert_eq!(
            bare_snapshot.disabled_reason,
            Some(GitProjectIneligibleReason::BareRepository)
        );

        let unborn = temp_path("eligibility-unborn");
        std::fs::create_dir_all(&unborn).unwrap();
        run_git_raw(&unborn, &["init"]);
        let unborn_snapshot = project_git_eligibility(ProjectId::new(), &unborn);
        assert_eq!(
            unborn_snapshot.disabled_reason,
            Some(GitProjectIneligibleReason::UnbornHead)
        );
        let _ = std::fs::remove_dir_all(bare);
        let _ = std::fs::remove_dir_all(unborn);
    }

    fn temp_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-git-{label}-{}",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn init_repo_with_commit(root: &Path) {
        run_git_raw(root, &["init"]);
        run_git_raw(root, &["config", "user.email", "vibex@example.invalid"]);
        run_git_raw(root, &["config", "user.name", "Vibex Test"]);
        std::fs::write(root.join("README.md"), "hello\n").unwrap();
        run_git_raw(root, &["add", "README.md"]);
        run_git_raw(root, &["commit", "-m", "initial"]);
    }

    fn run_git_raw(root: &Path, args: &[&str]) {
        let output = Command::new("git")
            .arg("-C")
            .arg(root)
            .args(args)
            .output()
            .unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

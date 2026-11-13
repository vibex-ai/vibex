//! What a plugin's declared scope actually admits.
//!
//! A permission says *that* a plugin may reach something; its scope says *how
//! far*. The manifest's `network.hosts`, `fs.read`, `fs.write` and
//! `process.executables` are those scopes, and this module is the one place
//! that answers "does this grant cover that address, path or program?".
//!
//! The rule throughout is to refuse when unsure. A pattern this code does not
//! understand is not a wildcard; a path that cannot be resolved is not inside a
//! root; a program name that is not a plain file name is not an executable.

use std::path::{Component, Path, PathBuf};

/// Whether a URL host is covered by the patterns a manifest declared.
///
/// A pattern is `*` (every host), `*.example.com` (that domain and its
/// subdomains), or an exact host. Matching is case-insensitive and anchored:
/// `*.example.com` does not match `evil-example.com`, and an exact host does
/// not match a subdomain.
pub fn host_allowed(patterns: &[String], host: &str) -> bool {
    let host = host.trim().trim_end_matches('.').to_ascii_lowercase();
    if host.is_empty() {
        return false;
    }
    patterns.iter().any(|pattern| {
        let pattern = pattern.trim().to_ascii_lowercase();
        if pattern == "*" {
            return true;
        }
        if let Some(suffix) = pattern.strip_prefix("*.") {
            // The apex counts: `*.example.com` admits `example.com` itself,
            // which is what an author writing it means.
            return host == suffix || host.ends_with(&format!(".{suffix}"));
        }
        host == pattern
    })
}

/// Whether a path is inside one of the roots a manifest declared.
///
/// `${workspace}` and `${home}` are the only variables: a manifest cannot name an
/// arbitrary environment variable, because the consent sheet could not show
/// what it resolved to. A root that does not resolve is skipped rather than
/// treated as "everything".
pub fn path_allowed(
    roots: &[String],
    path: &Path,
    workspace: Option<&Path>,
    home: Option<&Path>,
) -> bool {
    let Some(candidate) = normalize(path) else {
        return false;
    };
    roots.iter().any(|root| {
        let Some(root) = resolve_root(root, workspace, home) else {
            return false;
        };
        let Some(root) = normalize(&root) else {
            return false;
        };
        candidate.starts_with(&root)
    })
}

/// Expands the two variables a root may name.
fn resolve_root(root: &str, workspace: Option<&Path>, home: Option<&Path>) -> Option<PathBuf> {
    let root = root.trim();
    if root.is_empty() {
        return None;
    }
    if let Some(rest) = root.strip_prefix("${workspace}") {
        let base = workspace?;
        return Some(join_relative(base, rest));
    }
    if let Some(rest) = root.strip_prefix("${home}") {
        let base = home?;
        return Some(join_relative(base, rest));
    }
    // An absolute path is allowed, but a relative one is not: it would resolve
    // against whatever the process happened to be running in.
    let path = Path::new(root);
    path.is_absolute().then(|| path.to_path_buf())
}

fn join_relative(base: &Path, rest: &str) -> PathBuf {
    let rest = rest.trim_start_matches(['/', '\\']);
    if rest.is_empty() {
        base.to_path_buf()
    } else {
        base.join(rest)
    }
}

/// Lexically normalizes a path without touching the filesystem.
///
/// `..` is resolved rather than refused: `/a/b/../c` is `/a/c`, and a plugin
/// that names it should get the path it meant. What is refused is escaping the
/// root, which `starts_with` on the normalized result catches.
fn normalize(path: &Path) -> Option<PathBuf> {
    let mut out = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => out.push(prefix.as_os_str()),
            Component::RootDir => out.push(std::path::MAIN_SEPARATOR.to_string()),
            Component::CurDir => {}
            Component::ParentDir => {
                if !out.pop() {
                    // `..` at the root: there is nowhere above it, so the path
                    // is not one this module can vouch for.
                    return None;
                }
            }
            Component::Normal(part) => out.push(part),
        }
    }
    Some(out)
}

/// Whether an executable name is one a manifest declared.
///
/// The name is compared as a plain file name with no directory part: a plugin
/// granted `git` cannot run `./git`, `/usr/bin/git` or `../git`, because those
/// are different programs that happen to share a name.
pub fn executable_allowed(allowed: &[String], program: &str) -> bool {
    let program = program.trim();
    if program.is_empty() || program.contains('/') || program.contains('\\') {
        return false;
    }
    if program == "." || program == ".." {
        return false;
    }
    allowed.iter().any(|candidate| {
        let candidate = candidate.trim();
        // A declared name may carry its platform suffix; compare the stem too
        // so `node` admits `node.exe` on Windows without widening the grant.
        candidate.eq_ignore_ascii_case(program)
            || candidate
                .strip_suffix(".exe")
                .is_some_and(|stem| stem.eq_ignore_ascii_case(program))
            || program
                .strip_suffix(".exe")
                .is_some_and(|stem| candidate.eq_ignore_ascii_case(stem))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roots(values: &[&str]) -> Vec<String> {
        values.iter().map(|value| value.to_string()).collect()
    }

    #[test]
    fn a_host_pattern_is_anchored_and_admits_the_apex() {
        let patterns = roots(&["*.example.com"]);
        assert!(host_allowed(&patterns, "example.com"));
        assert!(host_allowed(&patterns, "api.example.com"));
        assert!(host_allowed(&patterns, "a.b.example.com"));
        // Not a suffix match: the dot is what anchors it.
        assert!(!host_allowed(&patterns, "evil-example.com"));
        assert!(!host_allowed(&patterns, "example.com.evil.net"));

        let exact = roots(&["api.example.com"]);
        assert!(host_allowed(&exact, "api.example.com"));
        assert!(host_allowed(&exact, "API.EXAMPLE.COM"));
        assert!(!host_allowed(&exact, "other.example.com"));
        // An exact host does not silently become a subdomain rule.
        assert!(!host_allowed(&exact, "x.api.example.com"));
    }

    #[test]
    fn a_wildcard_host_admits_everything_and_nothing_else_does() {
        assert!(host_allowed(&roots(&["*"]), "anything.example"));
        assert!(!host_allowed(&[], "anything.example"));
        // A pattern this code does not understand is not a wildcard.
        assert!(!host_allowed(&roots(&["example.*"]), "example.com"));
        assert!(!host_allowed(&roots(&[""]), "example.com"));
    }

    #[test]
    fn a_path_is_inside_a_root_or_it_is_not() {
        let workspace = Path::new("/home/me/project");
        let home = Path::new("/home/me");
        let allowed = roots(&["${workspace}"]);

        assert!(path_allowed(
            &allowed,
            Path::new("/home/me/project/src/main.rs"),
            Some(workspace),
            Some(home)
        ));
        assert!(path_allowed(
            &allowed,
            Path::new("/home/me/project"),
            Some(workspace),
            Some(home)
        ));
        // A sibling with a shared prefix is not inside.
        assert!(!path_allowed(
            &allowed,
            Path::new("/home/me/project-other/x"),
            Some(workspace),
            Some(home)
        ));
        // Nor is a parent reached through `..`.
        assert!(!path_allowed(
            &allowed,
            Path::new("/home/me/project/../secret"),
            Some(workspace),
            Some(home)
        ));
    }

    #[test]
    fn a_path_inside_the_root_is_normalized_before_comparing() {
        let workspace = Path::new("/home/me/project");
        let allowed = roots(&["${workspace}"]);
        assert!(path_allowed(
            &allowed,
            Path::new("/home/me/project/src/../Cargo.toml"),
            Some(workspace),
            None,
        ));
    }

    #[test]
    fn an_unresolvable_root_admits_nothing() {
        // `${workspace}` with no workspace is not "every path".
        let allowed = roots(&["${workspace}"]);
        assert!(!path_allowed(
            &allowed,
            Path::new("/etc/passwd"),
            None,
            None
        ));
        // A relative root is refused for the same reason.
        let relative = roots(&["src"]);
        assert!(!path_allowed(
            &relative,
            Path::new("/home/me/project/src/a.rs"),
            Some(Path::new("/home/me/project")),
            None
        ));
    }

    #[test]
    fn a_sub_root_is_admitted() {
        let workspace = Path::new("/home/me/project");
        let allowed = roots(&["${workspace}/docs"]);
        assert!(path_allowed(
            &allowed,
            Path::new("/home/me/project/docs/a.md"),
            Some(workspace),
            None
        ));
        assert!(!path_allowed(
            &allowed,
            Path::new("/home/me/project/src/a.rs"),
            Some(workspace),
            None
        ));
    }

    #[test]
    fn an_executable_is_a_plain_name_that_was_declared() {
        let allowed = roots(&["git", "node"]);
        assert!(executable_allowed(&allowed, "git"));
        assert!(executable_allowed(&allowed, "GIT"));
        assert!(executable_allowed(&allowed, "node.exe"));
        // A path is a different program that shares a name.
        assert!(!executable_allowed(&allowed, "/usr/bin/git"));
        assert!(!executable_allowed(&allowed, "./git"));
        assert!(!executable_allowed(&allowed, "../git"));
        assert!(!executable_allowed(&allowed, "bash"));
        assert!(!executable_allowed(&[], "git"));
        assert!(!executable_allowed(&allowed, ""));
    }
}

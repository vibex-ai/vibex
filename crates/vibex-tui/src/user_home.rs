//! Where a client's own files live, on every platform Vibex ships.
//!
//! The TUI reads three environment variables that name a home outright, and
//! falls back to the user's own home directory when none of them is set. That
//! fallback is the only part that is platform-specific: `HOME` is a POSIX
//! convention, and Windows sets `USERPROFILE` instead. Reading `HOME` alone
//! therefore leaves the client unable to resolve a home at all on a stock
//! Windows shell, where the desktop app — which resolves through the same
//! library this module uses — finds one without trouble.
//!
//! Every reader that needs "the user's home directory" goes through
//! [`user_home`], so the two clients agree about where that is.

use std::path::PathBuf;

/// The current user's home directory.
///
/// `dirs` resolves the platform's own answer — `HOME` on Unix, the known
/// folder API (backed by `USERPROFILE`) on Windows — instead of assuming a
/// POSIX variable is exported. It is already in the workspace graph through
/// the database crate, so this adds no package.
pub fn user_home() -> Option<PathBuf> {
    dirs::home_dir().filter(|path| !path.as_os_str().is_empty())
}

/// The `.vibex` directory under the user's home, which every client treats as
/// the default place its own files live.
pub fn default_vibex_home() -> Option<PathBuf> {
    user_home().map(|home| home.join(".vibex"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_user_home_is_absolute_and_not_empty() {
        let Some(home) = user_home() else {
            // A container can run with no home at all; the resolver reports
            // `None` there rather than inventing a directory.
            return;
        };
        assert!(home.is_absolute(), "user home must be absolute: {home:?}");
        assert!(!home.as_os_str().is_empty());
    }

    #[test]
    fn the_default_vibex_home_sits_under_the_user_home() {
        let Some(home) = user_home() else {
            return;
        };
        let Some(vibex) = default_vibex_home() else {
            panic!("a resolved user home must yield a vibex home");
        };
        assert_eq!(vibex, home.join(".vibex"));
    }
}

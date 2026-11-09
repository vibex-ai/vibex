//! Environment hygiene for child processes spawned by a packaged desktop app.
//!
//! A Linux AppImage is a mount: its launcher exports `APPDIR`, `APPIMAGE` and a
//! `PATH`/`LD_LIBRARY_PATH` that point inside `/tmp/.mount_*`, and every child
//! process inherits them. That is correct for the application's own libraries
//! and wrong for everything it launches: a host binary that loads the bundle's
//! `libglib`/`libdbus` gets a mixed GLib stack and fails with symbol errors
//! before it ever reaches the session bus. The accessibility bridge of the
//! desktop driver fails exactly this way.
//!
//! The policy therefore lives here, once, rather than in each spawner: strip
//! mount-local entries from path lists, drop the launcher markers, and leave
//! every host entry in its original order. Callers apply it to whichever
//! command type they build.

#[cfg(target_os = "linux")]
use std::ffi::{OsStr, OsString};
#[cfg(target_os = "linux")]
use std::path::Path;
use std::process::Command;

/// Path-list variables an AppImage launcher rewrites for its own bundle.
#[cfg(target_os = "linux")]
pub const APPIMAGE_PATH_LIST_KEYS: &[&str] = &[
    "PATH",
    "LD_LIBRARY_PATH",
    "GSETTINGS_SCHEMA_DIR",
    "GST_PLUGIN_SYSTEM_PATH",
    "GST_PLUGIN_SYSTEM_PATH_1_0",
    "PERLLIB",
    "PYTHONHOME",
    "PYTHONPATH",
    "QT_PLUGIN_PATH",
    "XDG_DATA_DIRS",
];

/// Variables only the launcher itself understands.
#[cfg(target_os = "linux")]
pub const APPIMAGE_LAUNCHER_KEYS: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
    "PYTHONDONTWRITEBYTECODE",
];

/// Removes every entry that lives inside `app_dir` from one path-list value.
///
/// `None` means nothing was mount-local and the inherited value must stay
/// untouched. `Some("")` means every entry was mount-local, which callers
/// translate into removing the variable rather than exporting an empty path
/// list.
#[cfg(target_os = "linux")]
pub fn retain_host_paths(value: &OsStr, app_dir: &Path) -> Option<OsString> {
    let paths = std::env::split_paths(value).collect::<Vec<_>>();
    let retained = paths
        .iter()
        .filter(|path| !path.starts_with(app_dir))
        .cloned()
        .collect::<Vec<_>>();
    if retained.len() == paths.len() {
        return None;
    }
    match std::env::join_paths(retained) {
        Ok(value) if !value.is_empty() => Some(value),
        _ => Some(OsString::new()),
    }
}

/// Applies the AppImage policy to one command.
///
/// The AppImage mount is read from the launched process's `APPDIR`, so this is
/// the entry point for every spawn site that has no reason to see the policy
/// itself. It is a no-op outside Linux and outside an AppImage launch.
pub fn sanitize_inherited_appimage_environment(command: &mut Command) {
    #[cfg(target_os = "linux")]
    {
        use std::path::PathBuf;

        let Some(app_dir) = std::env::var_os("APPDIR").map(PathBuf::from) else {
            return;
        };
        if !app_dir.is_absolute() {
            return;
        }
        sanitize_appimage_environment(command, &app_dir, |key| std::env::var_os(key));
    }

    #[cfg(not(target_os = "linux"))]
    let _ = command;
}

/// The policy with the mount and the inherited environment made explicit.
///
/// Taking both as parameters is what lets the tests describe a mount that does
/// not exist on the machine running them, and what keeps the process
/// environment out of the decision.
#[cfg(target_os = "linux")]
pub fn sanitize_appimage_environment(
    command: &mut Command,
    app_dir: &Path,
    inherited_value: impl Fn(&str) -> Option<OsString>,
) {
    for key in APPIMAGE_PATH_LIST_KEYS {
        let Some(value) = inherited_value(key) else {
            continue;
        };
        let Some(retained) = retain_host_paths(&value, app_dir) else {
            continue;
        };
        if retained.is_empty() {
            command.env_remove(key);
        } else {
            command.env(key, retained);
        }
    }

    for key in APPIMAGE_LAUNCHER_KEYS {
        command.env_remove(key);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::BTreeMap;

    use super::*;

    fn overrides(command: &Command) -> BTreeMap<OsString, Option<OsString>> {
        command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(std::ffi::OsStr::to_os_string)))
            .collect()
    }

    #[test]
    fn mount_paths_are_removed_and_host_paths_keep_their_order() {
        let app_dir = Path::new("/tmp/.mount_vibex-test");
        let inherited = BTreeMap::from([
            (
                "PATH",
                OsString::from("/tmp/.mount_vibex-test/usr/bin:/home/test/.local/bin:/usr/bin"),
            ),
            (
                "LD_LIBRARY_PATH",
                OsString::from("/tmp/.mount_vibex-test/usr/lib:/opt/host/lib"),
            ),
            ("PYTHONHOME", OsString::from("/tmp/.mount_vibex-test/usr")),
            (
                "PYTHONPATH",
                OsString::from("/tmp/.mount_vibex-test/usr/share/pyshared"),
            ),
            (
                "XDG_DATA_DIRS",
                OsString::from("/tmp/.mount_vibex-test/usr/share:/usr/local/share:/usr/share"),
            ),
            ("VIBEX_TEST_SENTINEL", OsString::from("preserved")),
        ]);
        let mut command = Command::new("/bin/sh");

        sanitize_appimage_environment(&mut command, app_dir, |key| inherited.get(key).cloned());

        let overrides = overrides(&command);
        assert_eq!(
            overrides.get(OsStr::new("PATH")),
            Some(&Some(OsString::from("/home/test/.local/bin:/usr/bin")))
        );
        assert_eq!(
            overrides.get(OsStr::new("LD_LIBRARY_PATH")),
            Some(&Some(OsString::from("/opt/host/lib")))
        );
        assert_eq!(
            overrides.get(OsStr::new("XDG_DATA_DIRS")),
            Some(&Some(OsString::from("/usr/local/share:/usr/share")))
        );
        for key in [
            "APPDIR",
            "APPIMAGE",
            "ARGV0",
            "OWD",
            "PYTHONDONTWRITEBYTECODE",
            "PYTHONHOME",
            "PYTHONPATH",
        ] {
            assert_eq!(overrides.get(OsStr::new(key)), Some(&None), "{key}");
        }
        assert!(!overrides.contains_key(OsStr::new("VIBEX_TEST_SENTINEL")));
    }

    /// An AppImage mount that covers a whole path list removes the variable
    /// instead of exporting an empty list, which the loader reads as the
    /// current directory.
    #[test]
    fn a_fully_mount_local_path_list_is_removed() {
        let app_dir = Path::new("/tmp/.mount_vibex-test");
        let inherited = BTreeMap::from([(
            "LD_LIBRARY_PATH",
            OsString::from("/tmp/.mount_vibex-test/usr/lib:/tmp/.mount_vibex-test/lib64"),
        )]);
        let mut command = Command::new("/bin/sh");

        sanitize_appimage_environment(&mut command, app_dir, |key| inherited.get(key).cloned());

        assert_eq!(
            overrides(&command).get(OsStr::new("LD_LIBRARY_PATH")),
            Some(&None)
        );
    }

    /// A host value with no mount entry is not rewritten: an untouched
    /// environment must stay untouched.
    #[test]
    fn a_host_only_path_list_is_not_rewritten() {
        let app_dir = Path::new("/tmp/.mount_vibex-test");
        let inherited = BTreeMap::from([(
            "LD_LIBRARY_PATH",
            OsString::from("/opt/host/lib:/usr/local/lib"),
        )]);
        let mut command = Command::new("/bin/sh");

        sanitize_appimage_environment(&mut command, app_dir, |key| inherited.get(key).cloned());

        assert!(!overrides(&command).contains_key(OsStr::new("LD_LIBRARY_PATH")));
    }

    /// The sanitized command can still start a host interpreter — the case the
    /// ACP child hit when the bundle's `PYTHONHOME` reached it.
    #[test]
    fn sanitized_command_imports_host_python_encodings() {
        let app_dir = Path::new("/tmp/.mount_vibex-test");
        let inherited = BTreeMap::from([
            (
                "PATH",
                OsString::from("/tmp/.mount_vibex-test/usr/bin:/usr/bin:/bin"),
            ),
            ("PYTHONHOME", OsString::from("/tmp/.mount_vibex-test/usr")),
            (
                "PYTHONPATH",
                OsString::from("/tmp/.mount_vibex-test/usr/share/pyshared"),
            ),
        ]);
        let mut command = Command::new("python3");
        command.args(["-c", "import encodings"]);
        command.env("PATH", inherited.get("PATH").unwrap());
        command.env("PYTHONHOME", inherited.get("PYTHONHOME").unwrap());
        command.env("PYTHONPATH", inherited.get("PYTHONPATH").unwrap());

        sanitize_appimage_environment(&mut command, app_dir, |key| inherited.get(key).cloned());

        let output = command.output().expect("host python should start");
        assert!(
            output.status.success(),
            "host Python failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

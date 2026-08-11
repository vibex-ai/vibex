use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

/// Remove the ACP child from the desktop's controlling terminal before the
/// process-group hook moves it into the background. Some CLIs open `/dev/tty`
/// for interactive services even in ACP mode; a background process group that
/// does so is stopped by `SIGTTIN` and never answers the ACP handshake.
#[cfg(unix)]
pub fn detach_from_controlling_terminal(command: &mut Command) {
    use std::os::unix::process::CommandExt;

    // SAFETY: the hook only performs async-signal-safe syscalls between fork
    // and exec. It does not allocate, lock, or access shared Rust state.
    unsafe {
        command.pre_exec(|| {
            const DEV_TTY: &[u8] = b"/dev/tty\0";
            // The child may not have a controlling terminal (for example in a
            // service or test process), so failure to open it is expected.
            let fd = libc::open(DEV_TTY.as_ptr().cast(), libc::O_RDWR | libc::O_CLOEXEC);
            if fd >= 0 {
                libc::ioctl(fd, libc::TIOCNOTTY);
                libc::close(fd);
            }
            Ok(())
        });
    }
}

#[cfg(not(unix))]
pub fn detach_from_controlling_terminal(_command: &mut Command) {}

#[cfg(windows)]
pub(crate) const WINDOWS_CREATE_NO_WINDOW: u32 = 0x0800_0000;

#[cfg(target_os = "linux")]
const APPIMAGE_PATH_LIST_KEYS: &[&str] = &[
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

#[cfg(target_os = "linux")]
const APPIMAGE_LAUNCHER_KEYS: &[&str] = &[
    "APPDIR",
    "APPIMAGE",
    "ARGV0",
    "OWD",
    "PYTHONDONTWRITEBYTECODE",
];

/// Configure ACP child processes without leaking AppImage paths or opening
/// standalone Windows console windows.
pub fn sanitize_inherited_appimage_environment(command: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;

        command.creation_flags(WINDOWS_CREATE_NO_WINDOW);
    }

    #[cfg(target_os = "linux")]
    {
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

#[cfg(target_os = "linux")]
fn sanitize_appimage_environment(
    command: &mut Command,
    app_dir: &Path,
    inherited_value: impl Fn(&str) -> Option<OsString>,
) {
    for key in APPIMAGE_PATH_LIST_KEYS {
        let Some(value) = inherited_value(key) else {
            continue;
        };
        let paths = std::env::split_paths(&value).collect::<Vec<_>>();
        let retained = paths
            .iter()
            .filter(|path| !path.starts_with(app_dir))
            .cloned()
            .collect::<Vec<_>>();
        if retained.len() == paths.len() {
            continue;
        }
        match std::env::join_paths(retained) {
            Ok(value) if !value.is_empty() => command.env(key, value),
            _ => command.env_remove(key),
        };
    }

    for key in APPIMAGE_LAUNCHER_KEYS {
        command.env_remove(key);
    }
}

#[cfg(all(test, target_os = "linux"))]
mod tests {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;

    use super::*;

    #[test]
    fn acp_child_does_not_inherit_the_controlling_terminal() {
        let mut command = Command::new("/bin/sh");
        command.arg("-c").arg(
            "if dd if=/dev/tty of=/dev/null count=0 2>/dev/null; then exit 42; else exit 0; fi",
        );
        detach_from_controlling_terminal(&mut command);

        let status = command.status().unwrap();

        assert!(status.success());
    }

    #[test]
    fn appimage_environment_keeps_only_host_paths() {
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

        let overrides = command
            .get_envs()
            .map(|(key, value)| (key.to_os_string(), value.map(OsStr::to_os_string)))
            .collect::<BTreeMap<_, _>>();
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

        sanitize_appimage_environment(&mut command, app_dir, |key| inherited.get(key).cloned());

        let output = command.output().expect("host python should start");
        assert!(
            output.status.success(),
            "host Python failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

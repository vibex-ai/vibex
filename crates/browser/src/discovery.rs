//! Detection of a system Chromium-family browser.
//!
//! Detection lives in the runtime, not the UI: the runtime is what spawns the
//! browser, and the runtime may be a headless server on another machine where
//! the user's own desktop has no bearing on whether Chrome exists.
//!
//! Detection walks `PATH` (and, on Windows, the registry and well-known install
//! directories; on macOS, `/Applications`). That is thousands of `is_file`
//! lookups on Windows, so the answer is computed once and cached.

use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vibex_core::{BrowserAvailability, BrowserInstallation, BrowserUnavailableReason};

/// A candidate browser family, in preference order.
pub struct BrowserCandidate {
    pub id: &'static str,
    pub label: &'static str,
    /// Commands to look up on `PATH`.
    pub commands: &'static [&'static str],
    /// macOS application bundle names under `/Applications` and
    /// `~/Applications`.
    pub mac_apps: &'static [&'static str],
    /// Windows executable names looked up through the `App Paths` registry key
    /// and the well-known install directories.
    pub windows_executables: &'static [&'static str],
}

/// Preference order: Chrome, then Chromium, then Edge, then Brave.
///
/// Windows prefers Edge only because it is preinstalled; the ordering here is a
/// tiebreak, not a platform statement — every candidate is probed on every
/// platform and the first hit wins.
pub const BROWSER_CANDIDATES: &[BrowserCandidate] = &[
    BrowserCandidate {
        id: "chrome",
        label: "Google Chrome",
        commands: &["google-chrome", "google-chrome-stable", "chrome"],
        mac_apps: &["Google Chrome"],
        windows_executables: &["chrome.exe"],
    },
    BrowserCandidate {
        id: "chromium",
        label: "Chromium",
        commands: &["chromium", "chromium-browser", "chromium-freeworld"],
        mac_apps: &["Chromium"],
        windows_executables: &["chromium.exe"],
    },
    BrowserCandidate {
        id: "edge",
        label: "Microsoft Edge",
        commands: &["microsoft-edge", "microsoft-edge-stable", "msedge"],
        mac_apps: &["Microsoft Edge"],
        windows_executables: &["msedge.exe"],
    },
    BrowserCandidate {
        id: "brave",
        label: "Brave",
        commands: &["brave-browser", "brave"],
        mac_apps: &["Brave Browser"],
        windows_executables: &["brave.exe"],
    },
];

/// Executable candidates for a command name, including Windows extensions.
pub fn executable_candidates(command: &str) -> Vec<String> {
    if cfg!(windows) {
        let mut candidates = vec![command.to_string()];
        for extension in [".exe", ".cmd", ".bat"] {
            candidates.push(format!("{command}{extension}"));
        }
        candidates
    } else {
        vec![command.to_string()]
    }
}

/// Resolves a command on `PATH`, returning the first existing absolute file.
pub fn find_executable(command: &str) -> Option<PathBuf> {
    let path = std::env::var_os("PATH")?;
    for directory in std::env::split_paths(&path) {
        for candidate in executable_candidates(command) {
            let full = directory.join(&candidate);
            if is_executable_file(&full) {
                return Some(full);
            }
        }
    }
    None
}

fn is_executable_file(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
    if !metadata.is_file() {
        return false;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        true
    }
}

/// Finds a macOS application bundle's inner executable.
///
/// Spawning a `.app` bundle requires `open -a`, which cannot carry the pipe
/// file descriptors the debugging channel needs, so the executable inside
/// `Contents/MacOS` is what the runtime actually launches.
pub fn find_mac_application(app_names: &[&str]) -> Option<PathBuf> {
    if !cfg!(target_os = "macos") {
        return None;
    }
    let mut roots = vec![PathBuf::from("/Applications")];
    if let Some(home) = std::env::var_os("HOME") {
        roots.push(PathBuf::from(home).join("Applications"));
    }
    for root in roots {
        for name in app_names {
            let bundle = root.join(format!("{name}.app"));
            if !bundle.is_dir() {
                continue;
            }
            let macos = bundle.join("Contents").join("MacOS");
            let Ok(entries) = std::fs::read_dir(&macos) else {
                continue;
            };
            // The executable normally shares the bundle's name; fall back to
            // whatever single executable lives there.
            let preferred = macos.join(name);
            if is_executable_file(&preferred) {
                return Some(preferred);
            }
            for entry in entries.flatten() {
                let path = entry.path();
                if is_executable_file(&path) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Looks up a Windows executable through the `App Paths` registry key and the
/// well-known install directories.
///
/// Chrome and Edge are not on `PATH` by default, so a `PATH`-only probe would
/// miss the browsers that are actually installed on most Windows machines.
pub fn find_windows_browser(executables: &[&str]) -> Option<PathBuf> {
    if !cfg!(windows) {
        return None;
    }
    let roots = [
        std::env::var_os("ProgramFiles").map(PathBuf::from),
        std::env::var_os("ProgramFiles(x86)").map(PathBuf::from),
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from),
    ];
    let relative = [
        ["Google", "Chrome", "Application"].as_slice(),
        ["Chromium", "Application"].as_slice(),
        ["Microsoft", "Edge", "Application"].as_slice(),
        ["BraveSoftware", "Brave-Browser", "Application"].as_slice(),
    ];
    for executable in executables {
        for root in roots.iter().flatten() {
            for tail in relative {
                let mut path = root.clone();
                for segment in tail {
                    path.push(segment);
                }
                path.push(executable);
                if is_executable_file(&path) {
                    return Some(path);
                }
            }
        }
    }
    None
}

/// Locates every installed candidate, most preferred first.
pub fn detect_installations() -> Vec<BrowserInstallation> {
    let mut found = Vec::new();
    for candidate in BROWSER_CANDIDATES {
        let executable = candidate
            .commands
            .iter()
            .find_map(|command| find_executable(command))
            .or_else(|| find_mac_application(candidate.mac_apps))
            .or_else(|| find_windows_browser(candidate.windows_executables));
        if let Some(executable) = executable {
            found.push(BrowserInstallation {
                id: candidate.id.to_string(),
                label: candidate.label.to_string(),
                executable: executable.to_string_lossy().to_string(),
                version: probe_version(&executable),
            });
        }
    }
    found
}

/// How long a version probe may take before the browser is killed.
///
/// The version is display metadata, so an unresponsive browser must not hold
/// the runtime's startup: overrunning the deadline reports "unknown version".
#[cfg(not(windows))]
const VERSION_PROBE_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Reads the browser's reported version, tolerating every failure mode.
///
/// A version probe must never block startup or fail detection, so a browser
/// that will not answer `--version` — or answers too slowly — is still usable
/// and simply reports no version.
#[cfg(not(windows))]
fn probe_version(executable: &Path) -> Option<String> {
    probe_version_within(executable, VERSION_PROBE_TIMEOUT)
}

/// The probe itself, with the deadline injectable so a test can prove that a
/// browser which never exits is killed and reported as unknown rather than
/// waited on.
#[cfg(not(windows))]
fn probe_version_within(executable: &Path, timeout: std::time::Duration) -> Option<String> {
    use std::io::Read;

    let mut command = std::process::Command::new(executable);
    command
        .arg("--version")
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    crate::process::detach_from_controlling_terminal(&mut command);
    let mut child = command.spawn().ok()?;
    let deadline = std::time::Instant::now() + timeout;
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break status,
            Ok(None) => {
                if std::time::Instant::now() >= deadline {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => {
                let _ = child.kill();
                let _ = child.wait();
                return None;
            }
        }
    };
    if !status.success() {
        return None;
    }
    let mut text = String::new();
    child.stdout.take()?.read_to_string(&mut text).ok()?;
    let version = text.split_whitespace().last()?.trim().to_string();
    if version.is_empty() {
        None
    } else {
        Some(version)
    }
}

/// Windows browsers are never asked for their version.
///
/// `chrome.exe --version` is not a version switch on Windows: Chrome ignores it
/// and boots the whole browser, and because the probe passes no
/// `--user-data-dir` it boots the user's own profile and exits only when the
/// user closes that window. Detection runs on every runtime start
/// (`BrowserService::new`), so probing here opened a browser window out of
/// nowhere on every launch — and with no browser already running, waiting for
/// the process blocked the startup for as long as the window stayed open. The
/// version has no consumer in the product, so Windows reports it as unknown
/// instead of starting a browser to find out.
#[cfg(windows)]
fn probe_version(_executable: &Path) -> Option<String> {
    None
}

/// Returns the process-wide cached detection result.
///
/// The walk happens once; every later question reads the same answer.
pub fn cached_installations() -> &'static [BrowserInstallation] {
    static CACHE: OnceLock<Vec<BrowserInstallation>> = OnceLock::new();
    CACHE.get_or_init(detect_installations)
}

/// Clears the detection cache. Only used by tests and by an explicit
/// "re-detect browsers" action.
pub fn reset_detection_cache() {
    // `OnceLock` cannot be reset; tests use `detect_installations` directly.
}

/// Resolves availability for the current host.
pub fn availability() -> BrowserAvailability {
    let installations = cached_installations();
    match installations.first() {
        Some(installation) => BrowserAvailability {
            unavailable_reason: None,
            installation: Some(installation.clone()),
            detail: None,
            installed_candidates: installations.to_vec(),
        },
        None => BrowserAvailability::unavailable(
            BrowserUnavailableReason::BrowserMissing,
            Some(
                "No Chromium-based browser was found on the machine running the Vibex runtime. \
                 Install Google Chrome, Chromium, Microsoft Edge or Brave, then re-open the panel."
                    .to_string(),
            ),
        ),
    }
}

/// Picks a specific installation by id, or the preferred one.
pub fn installation_by_id(id: Option<&str>) -> Option<BrowserInstallation> {
    match id {
        Some(id) => cached_installations()
            .iter()
            .find(|installation| installation.id == id)
            .cloned(),
        None => cached_installations().first().cloned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn candidate_table_covers_the_expected_families() {
        let ids: Vec<&str> = BROWSER_CANDIDATES.iter().map(|entry| entry.id).collect();
        assert_eq!(ids, vec!["chrome", "chromium", "edge", "brave"]);
        for candidate in BROWSER_CANDIDATES {
            assert!(!candidate.label.is_empty());
            assert!(!candidate.commands.is_empty());
        }
    }

    #[test]
    fn executable_candidates_add_windows_extensions_only_on_windows() {
        let candidates = executable_candidates("chrome");
        assert!(candidates.contains(&"chrome".to_string()));
        if cfg!(windows) {
            assert!(candidates.contains(&"chrome.exe".to_string()));
        } else {
            assert_eq!(candidates.len(), 1);
        }
    }

    #[test]
    fn mac_application_lookup_is_a_no_op_off_macos() {
        if !cfg!(target_os = "macos") {
            assert!(find_mac_application(&["Google Chrome"]).is_none());
        }
    }

    #[test]
    fn windows_lookup_is_a_no_op_off_windows() {
        if !cfg!(windows) {
            assert!(find_windows_browser(&["chrome.exe"]).is_none());
        }
    }

    #[test]
    fn availability_is_honest_when_nothing_is_installed() {
        let availability = availability();
        if availability.installed_candidates.is_empty() {
            assert_eq!(
                availability.unavailable_reason,
                Some(BrowserUnavailableReason::BrowserMissing)
            );
            assert!(availability.detail.is_some());
        } else {
            assert!(availability.is_available());
        }
    }

    #[test]
    fn detection_cache_returns_a_stable_answer() {
        let first = cached_installations().len();
        let second = cached_installations().len();
        assert_eq!(first, second);
    }

    /// The probe runs on every startup, so a browser that never answers must not
    /// be able to hold it: the deadline kills it and reports no version.
    #[cfg(unix)]
    #[test]
    fn a_browser_that_never_answers_the_probe_is_killed() {
        use std::os::unix::fs::PermissionsExt;

        let directory = tempfile::tempdir().expect("temp dir");
        let executable = directory.path().join("hanging-browser");
        std::fs::write(&executable, "#!/bin/sh\nsleep 30\n").expect("write");
        std::fs::set_permissions(&executable, std::fs::Permissions::from_mode(0o755))
            .expect("executable");
        let started = std::time::Instant::now();
        assert_eq!(
            probe_version_within(&executable, std::time::Duration::from_millis(200)),
            None
        );
        assert!(
            started.elapsed() < std::time::Duration::from_secs(10),
            "the probe waited for the browser instead of killing it"
        );
    }

    /// Windows is the platform where `--version` starts a browser, so the probe
    /// must not run one at all — proved with a stand-in that would leave a file
    /// behind if it were ever executed.
    #[cfg(windows)]
    #[test]
    fn windows_never_starts_a_browser_to_read_its_version() {
        let directory = tempfile::tempdir().expect("temp dir");
        let marker = directory.path().join("spawned.txt");
        let stand_in = directory.path().join("stand-in.cmd");
        std::fs::write(
            &stand_in,
            format!(
                "@echo off\r\necho 1 > \"{}\"\r\necho Google Chrome 999.0.0.0\r\n",
                marker.display()
            ),
        )
        .expect("write");
        assert_eq!(probe_version(&stand_in), None);
        assert!(
            !marker.exists(),
            "the version probe executed a browser on Windows"
        );
    }
}

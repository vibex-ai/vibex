//! Existing Vibex data directories on a remote host.
//!
//! Provisioning owns `{install_root}/home` by construction: that is the
//! directory the launcher hands the remote runtime as `VIBEX_HOME`, and it is
//! rebuilt from the release artifact whenever a profile is re-paired. A host
//! that already runs Vibex keeps its data somewhere else — a desktop build
//! under `~/.vibex/desktop-<channel>`, or an older pre-channel `~/.vibex` that
//! holds the database itself.
//!
//! Adopting one is a per-connection choice, never automatic: the runtime
//! identity lives in that database, so a runtime pointed at another home comes
//! up as a different server and every client has to pair with it again. This
//! module only answers "which directories exist, and can each be opened right
//! now" over the fixed scan script below; the decision stays with the reader.

use std::fmt;
use std::time::Duration;

use super::command::SshCommand;
use super::error::SshFailure;

/// The scan is one bounded walk over known directories.
const SCAN_TIMEOUT: Duration = Duration::from_secs(20);
/// Detection is a convenience, so it reports a short list rather than a survey.
const MAX_HOMES: usize = 8;

/// Where a candidate directory came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RuntimeHomeKind {
    /// `{install_root}/home`: the directory provisioning creates and owns.
    Managed,
    /// A desktop build's stable channel home.
    DesktopStable,
    /// A desktop build's release-candidate home.
    DesktopRc,
    /// A desktop build's preview home.
    DesktopPreview,
    /// A `~/.vibex` that holds the database directly, from before the
    /// per-channel homes existed.
    DesktopLegacy,
}

impl RuntimeHomeKind {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Managed => "managed",
            Self::DesktopStable => "desktop_stable",
            Self::DesktopRc => "desktop_rc",
            Self::DesktopPreview => "desktop_preview",
            Self::DesktopLegacy => "desktop_legacy",
        }
    }

    pub fn from_token(token: &str) -> Option<Self> {
        match token {
            "managed" => Some(Self::Managed),
            "desktop_stable" => Some(Self::DesktopStable),
            "desktop_rc" => Some(Self::DesktopRc),
            "desktop_preview" => Some(Self::DesktopPreview),
            "desktop_legacy" => Some(Self::DesktopLegacy),
            _ => None,
        }
    }

    /// A desktop home holds the reader's own sessions, so adopting it is the
    /// interesting case; the managed home is the default and needs no fanfare.
    pub const fn is_desktop(self) -> bool {
        !matches!(self, Self::Managed)
    }
}

/// Whether the directory's runtime home lock is held.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HomeLockState {
    Free,
    /// Some process holds the lock, so a runtime is running against it.
    Busy,
    /// The host has neither the lock file nor a way to test it.
    Unknown,
}

impl HomeLockState {
    pub const fn token(self) -> &'static str {
        match self {
            Self::Free => "free",
            Self::Busy => "busy",
            Self::Unknown => "unknown",
        }
    }

    fn from_token(token: &str) -> Self {
        match token {
            "free" => Self::Free,
            "busy" => Self::Busy,
            _ => Self::Unknown,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteRuntimeHome {
    pub path: String,
    pub kind: RuntimeHomeKind,
    /// Whether a `vibex.db` sits in the directory. A directory without one has
    /// no sessions to adopt, and the picker greys it out.
    pub has_database: bool,
    pub lock: HomeLockState,
    /// Modification time of the database, in milliseconds. Informational: the
    /// picker sorts on it so the reader's most recent data comes first.
    pub last_used_ms: Option<i64>,
}

impl RemoteRuntimeHome {
    /// Whether a runtime may be pointed at this directory right now.
    pub const fn adoptable(&self) -> bool {
        self.has_database && !matches!(self.lock, HomeLockState::Busy)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum HomeScanError {
    Script(String),
    Ssh(SshFailure),
    MalformedOutput(String),
}

impl fmt::Display for HomeScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Script(detail) => write!(formatter, "the home scan script is invalid: {detail}"),
            Self::Ssh(failure) => write!(formatter, "{}", failure.message),
            Self::MalformedOutput(detail) => {
                write!(formatter, "the home scan output is malformed: {detail}")
            }
        }
    }
}

impl std::error::Error for HomeScanError {}

/// The fixed scan script. `$1` is the install root, so no path is interpolated
/// into the script text.
///
/// Every candidate is reported with the same four facts. `flock -n` is the only
/// reliable probe for "is a runtime using this home": the lock file can outlive
/// its owner, so its mere presence is not an answer, and a host without
/// `flock` reports `unknown` rather than guessing.
pub const HOME_SCAN_SCRIPT: &str = r#"set -eu
root=$1
lock_state() {
  lock="$1/.vibex-runtime.lock"
  if [ ! -f "$lock" ]; then
    printf 'free'
  elif command -v flock >/dev/null 2>&1; then
    if flock -n "$lock" true 2>/dev/null; then printf 'free'; else printf 'busy'; fi
  else
    printf 'unknown'
  fi
}
database_stamp() {
  db="$1/vibex.db"
  if [ ! -f "$db" ]; then
    printf '0'
  elif stat -c %Y "$db" >/dev/null 2>&1; then
    stat -c %Y "$db" 2>/dev/null || printf '0'
  elif stat -f %m "$db" >/dev/null 2>&1; then
    stat -f %m "$db" 2>/dev/null || printf '0'
  else
    printf '0'
  fi
}
report() {
  path=$1
  if [ ! -d "$path" ]; then
    return 0
  fi
  if [ -f "$path/vibex.db" ]; then
    db=yes
  else
    db=no
  fi
  printf 'home=%s\tkind=%s\tdb=%s\tlock=%s\tmtime=%s\n' \
    "$path" "$2" "$db" "$(lock_state "$path")" "$(database_stamp "$path")"
}
report "$root/home" managed
report "$HOME/.vibex/desktop-stable" desktop_stable
report "$HOME/.vibex/desktop-rc" desktop_rc
report "$HOME/.vibex/desktop-preview" desktop_preview
report "$HOME/.vibex" desktop_legacy
echo scan=ok
"#;

pub fn scan_script() -> &'static str {
    HOME_SCAN_SCRIPT
}

/// Runs the scan against `command`, which must already point at the host.
pub fn scan(
    command: &SshCommand,
    install_root: &str,
) -> Result<Vec<RemoteRuntimeHome>, HomeScanError> {
    let mut command = command.clone();
    command
        .remote_script(HOME_SCAN_SCRIPT)
        .map_err(|error| HomeScanError::Script(error.to_string()))?;
    command.args.push(install_root.to_string().into());
    let output = command
        .run_with_timeout(SCAN_TIMEOUT)
        .map_err(HomeScanError::Ssh)?;
    if !output.status.success() {
        return Err(HomeScanError::Ssh(SshFailure::from_parts(
            output.status.code(),
            &output.stderr_lossy(),
        )));
    }
    Ok(parse_scan_output(&output.stdout_lossy()))
}

/// Parses the scan document. Unknown keys and unreadable lines are dropped: a
/// host that grows a new field must not break an older desktop.
pub fn parse_scan_output(stdout: &str) -> Vec<RemoteRuntimeHome> {
    let mut homes: Vec<RemoteRuntimeHome> = Vec::new();
    for line in stdout.lines() {
        let line = line.trim();
        if line.is_empty() || line == "scan=ok" {
            continue;
        }
        let mut path: Option<String> = None;
        let mut kind: Option<RuntimeHomeKind> = None;
        let mut has_database = false;
        let mut lock = HomeLockState::Unknown;
        let mut last_used_ms: Option<i64> = None;
        for field in line.split('\t') {
            let Some((key, value)) = field.split_once('=') else {
                continue;
            };
            let value = value.trim();
            match key {
                "home" => {
                    if value.starts_with('/') {
                        path = Some(value.to_string());
                    }
                }
                "kind" => kind = RuntimeHomeKind::from_token(value),
                "db" => has_database = value == "yes",
                "lock" => lock = HomeLockState::from_token(value),
                "mtime" => {
                    last_used_ms = value
                        .parse::<i64>()
                        .ok()
                        .filter(|seconds| *seconds > 0)
                        .map(|seconds| seconds.saturating_mul(1000));
                }
                _ => {}
            }
        }
        let (Some(path), Some(kind)) = (path, kind) else {
            continue;
        };
        // The legacy home is `~/.vibex` itself, which is also the parent of the
        // per-channel homes. Only report it when it holds a database of its
        // own, so adopting it is never a way to nest homes inside a home.
        if kind == RuntimeHomeKind::DesktopLegacy && !has_database {
            continue;
        }
        if homes.iter().any(|existing| existing.path == path) {
            continue;
        }
        homes.push(RemoteRuntimeHome {
            path,
            kind,
            has_database,
            lock,
            last_used_ms,
        });
        if homes.len() >= MAX_HOMES {
            break;
        }
    }
    // Most recently used first; the managed home sinks below real data.
    homes.sort_by(|left, right| {
        right
            .last_used_ms
            .unwrap_or_default()
            .cmp(&left.last_used_ms.unwrap_or_default())
    });
    homes
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "home=/home/dev/.local/share/vibex/server/home\tkind=managed\tdb=yes\tlock=free\tmtime=1700000000\n\
home=/home/dev/.vibex/desktop-stable\tkind=desktop_stable\tdb=yes\tlock=busy\tmtime=1800000000\n\
scan=ok\n";

    #[test]
    fn parses_every_candidate_most_recent_first() {
        let homes = parse_scan_output(CANONICAL);
        assert_eq!(homes.len(), 2);
        assert_eq!(homes[0].path, "/home/dev/.vibex/desktop-stable");
        assert_eq!(homes[0].kind, RuntimeHomeKind::DesktopStable);
        assert_eq!(homes[0].lock, HomeLockState::Busy);
        assert_eq!(homes[0].last_used_ms, Some(1_800_000_000_000));
        assert!(!homes[0].adoptable());
        assert_eq!(homes[1].kind, RuntimeHomeKind::Managed);
        assert!(homes[1].adoptable());
    }

    #[test]
    fn a_directory_without_a_database_is_not_adoptable() {
        let homes = parse_scan_output(
            "home=/home/dev/.vibex/desktop-rc\tkind=desktop_rc\tdb=no\tlock=free\tmtime=0\n",
        );
        assert_eq!(homes.len(), 1);
        assert!(!homes[0].adoptable());
        assert_eq!(homes[0].last_used_ms, None);
    }

    #[test]
    fn an_unreadable_lock_state_stays_adoptable_but_reported() {
        let homes = parse_scan_output(
            "home=/home/dev/.vibex/desktop-preview\tkind=desktop_preview\tdb=yes\tlock=unknown\tmtime=0\n",
        );
        assert_eq!(homes[0].lock, HomeLockState::Unknown);
        assert!(homes[0].adoptable());
    }

    #[test]
    fn the_legacy_home_is_only_reported_when_it_holds_a_database() {
        let homes = parse_scan_output(
            "home=/home/dev/.vibex\tkind=desktop_legacy\tdb=no\tlock=free\tmtime=0\n\
home=/home/dev/.vibex\tkind=desktop_legacy\tdb=yes\tlock=free\tmtime=0\n",
        );
        assert_eq!(homes.len(), 1);
        assert_eq!(homes[0].kind, RuntimeHomeKind::DesktopLegacy);
    }

    #[test]
    fn malformed_and_unknown_lines_are_dropped() {
        let homes = parse_scan_output(
            "garbage\n\
home=relative/path\tkind=desktop_stable\tdb=yes\tlock=free\tmtime=0\n\
home=/home/dev/.vibex/desktop-stable\tkind=nonsense\tdb=yes\tlock=free\tmtime=0\n\
home=/home/dev/.vibex/desktop-stable\tkind=desktop_stable\tdb=yes\tlock=free\tmtime=0\n",
        );
        assert_eq!(homes.len(), 1);
        assert_eq!(homes[0].path, "/home/dev/.vibex/desktop-stable");
    }

    #[test]
    fn the_scan_script_covers_every_kind_and_reports_a_completion_marker() {
        let script = scan_script();
        for kind in [
            "managed",
            "desktop_stable",
            "desktop_rc",
            "desktop_preview",
            "desktop_legacy",
        ] {
            assert!(script.contains(kind), "the scan should report {kind}");
        }
        assert!(script.contains("echo scan=ok"));
        assert!(script.contains("flock -n"));
        // The install root arrives as an argument, never interpolated.
        assert!(script.contains("root=$1"));
    }

    #[test]
    fn kind_tokens_round_trip() {
        for kind in [
            RuntimeHomeKind::Managed,
            RuntimeHomeKind::DesktopStable,
            RuntimeHomeKind::DesktopRc,
            RuntimeHomeKind::DesktopPreview,
            RuntimeHomeKind::DesktopLegacy,
        ] {
            assert_eq!(RuntimeHomeKind::from_token(kind.token()), Some(kind));
        }
        assert!(RuntimeHomeKind::DesktopStable.is_desktop());
        assert!(!RuntimeHomeKind::Managed.is_desktop());
    }
}

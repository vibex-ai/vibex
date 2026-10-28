//! Remote Linux environment probe used before installing a runtime.
//!
//! The probe is a fixed POSIX `sh` script: it has no user input and reports a
//! flat `key=value` document. Parsing and validation are separate so fixtures
//! can exercise old glibc, low disk, busy ports and unknown shells without a
//! live host.

use std::fmt;

use super::command::SshCommand;
use super::error::SshFailure;

/// The Linux glibc floor promised by the server artifact contract.
pub const MIN_GLIBC: GlibcVersion = GlibcVersion {
    major: 2,
    minor: 35,
};

/// Leave room for the runtime, its SQLite database and a few sessions.
pub const MIN_FREE_KB: u64 = 512 * 1024;

const RUNTIME_CURRENT_PATH: &str = ".local/share/vibex/server/current/vibex-server";

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub struct GlibcVersion {
    pub major: u16,
    pub minor: u16,
}

impl GlibcVersion {
    pub fn parse(value: &str) -> Option<Self> {
        let (major, minor) = value.trim().split_once('.')?;
        Some(Self {
            major: major.parse().ok()?,
            minor: minor.parse().ok()?,
        })
    }
}

impl fmt::Display for GlibcVersion {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "{}.{}", self.major, self.minor)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RemoteArch {
    X86_64,
    Aarch64,
}

impl RemoteArch {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::X86_64 => "x86_64",
            Self::Aarch64 => "aarch64",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PortState {
    Free,
    Busy,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EnvironmentWarning {
    GitMissing,
    ShellUnknown,
    PortUnknown,
}

impl EnvironmentWarning {
    pub const fn code(self) -> &'static str {
        match self {
            Self::GitMissing => "ssh_environment_git_missing",
            Self::ShellUnknown => "ssh_environment_shell_unknown",
            Self::PortUnknown => "ssh_environment_port_unknown",
        }
    }

    pub const fn message(self) -> &'static str {
        match self {
            Self::GitMissing => {
                "git is not available on the host; workspace history will be limited"
            }
            Self::ShellUnknown => "SHELL is not set; the runtime will use /bin/sh",
            Self::PortUnknown => {
                "the target port state could not be checked; startup will have to probe it"
            }
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RemoteEnvironment {
    pub os: String,
    pub arch: RemoteArch,
    pub home: String,
    pub shell: String,
    pub glibc: GlibcVersion,
    pub free_kb: u64,
    pub git_available: bool,
    pub sha256sum_available: bool,
    pub home_writable: bool,
    pub port_state: PortState,
    pub runtime_present: bool,
    pub warnings: Vec<EnvironmentWarning>,
}

impl RemoteEnvironment {
    /// Blocking checks. Git and an unknown port are warnings; a non-writable
    /// home, missing sha256sum, old glibc, low disk and a busy port are errors.
    pub fn validate(&self, min_free_kb: u64) -> Result<(), EnvironmentError> {
        if self.glibc < MIN_GLIBC {
            return Err(EnvironmentError::GlibcTooOld { found: self.glibc });
        }
        if self.free_kb < min_free_kb {
            return Err(EnvironmentError::InsufficientDisk {
                free_kb: self.free_kb,
                required_kb: min_free_kb,
            });
        }
        if !self.home_writable {
            return Err(EnvironmentError::HomeNotWritable);
        }
        if !self.sha256sum_available {
            return Err(EnvironmentError::MissingSha256sum);
        }
        if self.port_state == PortState::Busy {
            return Err(EnvironmentError::PortBusy);
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnvironmentError {
    UnsupportedOs { found: String },
    UnsupportedArch { found: String },
    MissingGlibc,
    GlibcTooOld { found: GlibcVersion },
    InsufficientDisk { free_kb: u64, required_kb: u64 },
    HomeNotWritable,
    MissingSha256sum,
    PortBusy,
    ProbeIncomplete,
    MalformedOutput { detail: String },
    Script(String),
    Ssh(SshFailure),
}

impl EnvironmentError {
    pub fn code(&self) -> &'static str {
        match self {
            Self::UnsupportedOs { .. } => "ssh_environment_unsupported_os",
            Self::UnsupportedArch { .. } => "ssh_environment_unsupported_arch",
            Self::MissingGlibc => "ssh_environment_missing_glibc",
            Self::GlibcTooOld { .. } => "ssh_environment_glibc_too_old",
            Self::InsufficientDisk { .. } => "ssh_environment_disk_low",
            Self::HomeNotWritable => "ssh_environment_home_not_writable",
            Self::MissingSha256sum => "ssh_environment_missing_sha256sum",
            Self::PortBusy => "ssh_environment_port_busy",
            Self::ProbeIncomplete => "ssh_environment_probe_incomplete",
            Self::MalformedOutput { .. } => "ssh_environment_malformed",
            Self::Script(_) => "ssh_environment_script_invalid",
            Self::Ssh(failure) => failure.code(),
        }
    }

    pub fn message(&self) -> String {
        match self {
            Self::UnsupportedOs { found } => {
                format!("the SSH host must run Linux; found {found}")
            }
            Self::UnsupportedArch { found } => {
                format!("Linux architecture {found} is not supported; expected x86_64 or aarch64")
            }
            Self::MissingGlibc => {
                "the host does not expose a glibc version; musl-only hosts are not supported"
                    .to_string()
            }
            Self::GlibcTooOld { found } => {
                format!("glibc {found} is older than the required {MIN_GLIBC}")
            }
            Self::InsufficientDisk {
                free_kb,
                required_kb,
            } => format!(
                "the host has {} MiB free; at least {} MiB is required",
                free_kb / 1024,
                required_kb / 1024
            ),
            Self::HomeNotWritable => "the remote home directory is not writable".to_string(),
            Self::MissingSha256sum => {
                "sha256sum is not available on the host; the runtime artifact cannot be verified"
                    .to_string()
            }
            Self::PortBusy => "the remote runtime port is already in use".to_string(),
            Self::ProbeIncomplete => "the remote environment probe did not complete".to_string(),
            Self::MalformedOutput { detail } => {
                format!("the remote environment probe returned an invalid value: {detail}")
            }
            Self::Script(message) => message.clone(),
            Self::Ssh(failure) => failure.message.clone(),
        }
    }

    pub fn retryable(&self) -> bool {
        match self {
            Self::PortBusy | Self::ProbeIncomplete => true,
            Self::Ssh(failure) => failure.is_retryable(),
            Self::UnsupportedOs { .. }
            | Self::UnsupportedArch { .. }
            | Self::MissingGlibc
            | Self::GlibcTooOld { .. }
            | Self::InsufficientDisk { .. }
            | Self::HomeNotWritable
            | Self::MissingSha256sum
            | Self::MalformedOutput { .. }
            | Self::Script(_) => false,
        }
    }
}

impl fmt::Display for EnvironmentError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message())
    }
}

impl std::error::Error for EnvironmentError {}

impl From<SshFailure> for EnvironmentError {
    fn from(failure: SshFailure) -> Self {
        Self::Ssh(failure)
    }
}

/// Fixed probe script. `remote_port` is a validated `u16`, so embedding it as a
/// decimal literal is safe; no SSH/profile text enters the script.
pub fn probe_script(remote_port: u16) -> String {
    format!(
        r#"set -eu
printf 'os=%s\n' "$(uname -s)"
printf 'arch=%s\n' "$(uname -m)"
printf 'home=%s\n' "$HOME"
printf 'shell=%s\n' "${{SHELL:-}}"
if command -v git >/dev/null 2>&1; then echo git=yes; else echo git=no; fi
if command -v sha256sum >/dev/null 2>&1; then echo sha256sum=yes; else echo sha256sum=no; fi
df -Pk "$HOME" 2>/dev/null | awk 'NR==2 {{print "free_kb=" $4}}' || true
glibc="$(getconf GNU_LIBC_VERSION 2>/dev/null || true)"
glibc="${{glibc#glibc }}"
printf 'glibc=%s\n' "$glibc"
if [ -w "$HOME" ]; then echo home_writable=yes; else echo home_writable=no; fi
port=unknown
if command -v ss >/dev/null 2>&1; then
  if ss -ltn "sport = :{remote_port}" >/dev/null 2>&1; then
    if [ "$(ss -ltn "sport = :{remote_port}" 2>/dev/null | wc -l)" -gt 1 ]; then
      port=busy
    else
      port=free
    fi
  fi
fi
printf 'port=%s\n' "$port"
if [ -x "$HOME/{RUNTIME_CURRENT_PATH}" ]; then echo runtime=present; else echo runtime=absent; fi
printf 'probe=ok\n'
"#
    )
}

/// Parses the probe's `key=value` output. Unknown lines (login banners, MOTD)
/// are ignored; a known key with an invalid value is a hard error.
pub fn parse_probe_output(stdout: &str) -> Result<RemoteEnvironment, EnvironmentError> {
    let mut probe_ok = false;
    let mut os = None;
    let mut arch = None;
    let mut home = None;
    let mut shell = String::new();
    let mut glibc = None;
    let mut free_kb = None;
    let mut git_available = false;
    let mut sha256sum_available = false;
    let mut home_writable = false;
    let mut port_state = PortState::Unknown;
    let mut runtime_present = false;

    for line in stdout.lines() {
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "probe" => {
                if value != "ok" {
                    return Err(EnvironmentError::ProbeIncomplete);
                }
                probe_ok = true;
            }
            "os" => os = Some(value.to_string()),
            "arch" => {
                arch = Some(match value {
                    "x86_64" => RemoteArch::X86_64,
                    "aarch64" => RemoteArch::Aarch64,
                    other => {
                        return Err(EnvironmentError::UnsupportedArch {
                            found: other.to_string(),
                        });
                    }
                })
            }
            "home" => {
                if value.is_empty() {
                    return Err(EnvironmentError::MalformedOutput {
                        detail: "home".to_string(),
                    });
                }
                home = Some(value.to_string());
            }
            "shell" => shell = value.to_string(),
            "glibc" => {
                if !value.is_empty() {
                    glibc = Some(GlibcVersion::parse(value).ok_or_else(|| {
                        EnvironmentError::MalformedOutput {
                            detail: "glibc".to_string(),
                        }
                    })?);
                }
            }
            "free_kb" => {
                free_kb =
                    Some(
                        value
                            .parse::<u64>()
                            .map_err(|_| EnvironmentError::MalformedOutput {
                                detail: "free_kb".to_string(),
                            })?,
                    )
            }
            "git" => git_available = parse_yes_no(value, "git")?,
            "sha256sum" => sha256sum_available = parse_yes_no(value, "sha256sum")?,
            "home_writable" => home_writable = parse_yes_no(value, "home_writable")?,
            "port" => {
                port_state = match value {
                    "free" => PortState::Free,
                    "busy" => PortState::Busy,
                    "unknown" => PortState::Unknown,
                    _ => {
                        return Err(EnvironmentError::MalformedOutput {
                            detail: "port".to_string(),
                        });
                    }
                }
            }
            "runtime" => {
                runtime_present = match value {
                    "present" => true,
                    "absent" => false,
                    _ => {
                        return Err(EnvironmentError::MalformedOutput {
                            detail: "runtime".to_string(),
                        });
                    }
                }
            }
            _ => {}
        }
    }

    if !probe_ok {
        return Err(EnvironmentError::ProbeIncomplete);
    }
    let os = os.ok_or(EnvironmentError::ProbeIncomplete)?;
    if os != "Linux" {
        return Err(EnvironmentError::UnsupportedOs { found: os });
    }
    let arch = arch.ok_or(EnvironmentError::ProbeIncomplete)?;
    let home = home.ok_or(EnvironmentError::ProbeIncomplete)?;
    let glibc = glibc.ok_or(EnvironmentError::MissingGlibc)?;
    let free_kb = free_kb.ok_or(EnvironmentError::ProbeIncomplete)?;

    let mut warnings = Vec::new();
    if !git_available {
        warnings.push(EnvironmentWarning::GitMissing);
    }
    if shell.is_empty() {
        warnings.push(EnvironmentWarning::ShellUnknown);
    }
    if port_state == PortState::Unknown {
        warnings.push(EnvironmentWarning::PortUnknown);
    }

    Ok(RemoteEnvironment {
        os,
        arch,
        home,
        shell,
        glibc,
        free_kb,
        git_available,
        sha256sum_available,
        home_writable,
        port_state,
        runtime_present,
        warnings,
    })
}

/// Runs the probe and validates the result. `min_free_kb` is injectable so
/// tests and the install policy can choose their own floor.
pub fn probe(
    command: &SshCommand,
    remote_port: u16,
    min_free_kb: u64,
) -> Result<RemoteEnvironment, EnvironmentError> {
    let mut command = command.clone();
    command
        .remote_script(&probe_script(remote_port))
        .map_err(|error| EnvironmentError::Script(error.to_string()))?;
    let output = command.run().map_err(EnvironmentError::Ssh)?;
    if !output.status.success() {
        return Err(EnvironmentError::Ssh(SshFailure::from_parts(
            output.status.code(),
            &output.stderr_lossy(),
        )));
    }
    let environment = parse_probe_output(&output.stdout_lossy())?;
    environment.validate(min_free_kb)?;
    Ok(environment)
}

fn parse_yes_no(value: &str, key: &str) -> Result<bool, EnvironmentError> {
    match value {
        "yes" => Ok(true),
        "no" => Ok(false),
        _ => Err(EnvironmentError::MalformedOutput {
            detail: key.to_string(),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const CANONICAL: &str = "probe=ok\n\
os=Linux\n\
arch=aarch64\n\
home=/home/dev\n\
shell=/bin/bash\n\
git=yes\n\
sha256sum=yes\n\
free_kb=1048576\n\
glibc=2.36\n\
home_writable=yes\n\
port=free\n\
runtime=present\n";

    #[test]
    fn parses_a_complete_linux_environment() {
        let environment = parse_probe_output(CANONICAL).unwrap();
        assert_eq!(environment.os, "Linux");
        assert_eq!(environment.arch, RemoteArch::Aarch64);
        assert_eq!(environment.home, "/home/dev");
        assert_eq!(environment.shell, "/bin/bash");
        assert_eq!(
            environment.glibc,
            GlibcVersion {
                major: 2,
                minor: 36
            }
        );
        assert_eq!(environment.free_kb, 1_048_576);
        assert!(environment.git_available);
        assert!(environment.sha256sum_available);
        assert!(environment.home_writable);
        assert_eq!(environment.port_state, PortState::Free);
        assert!(environment.runtime_present);
        assert!(environment.warnings.is_empty());
        assert_eq!(environment.validate(MIN_FREE_KB), Ok(()));
    }

    #[test]
    fn rejects_a_non_linux_host() {
        let output = CANONICAL.replace("os=Linux", "os=Darwin");
        assert_eq!(
            parse_probe_output(&output),
            Err(EnvironmentError::UnsupportedOs {
                found: "Darwin".to_string()
            })
        );
    }

    #[test]
    fn rejects_an_unsupported_architecture() {
        let output = CANONICAL.replace("arch=aarch64", "arch=armv7l");
        assert_eq!(
            parse_probe_output(&output),
            Err(EnvironmentError::UnsupportedArch {
                found: "armv7l".to_string()
            })
        );
    }

    #[test]
    fn missing_glibc_is_blocking_and_old_glibc_fails_validation() {
        let missing = CANONICAL.replace("glibc=2.36\n", "");
        assert_eq!(
            parse_probe_output(&missing),
            Err(EnvironmentError::MissingGlibc)
        );

        let old = CANONICAL.replace("glibc=2.36", "glibc=2.34");
        let environment = parse_probe_output(&old).unwrap();
        assert_eq!(
            environment.validate(MIN_FREE_KB),
            Err(EnvironmentError::GlibcTooOld {
                found: GlibcVersion {
                    major: 2,
                    minor: 34
                }
            })
        );
    }

    #[test]
    fn disk_home_and_checksum_failures_are_blocking() {
        let low_disk =
            parse_probe_output(&CANONICAL.replace("free_kb=1048576", "free_kb=1024")).unwrap();
        assert_eq!(
            low_disk.validate(MIN_FREE_KB),
            Err(EnvironmentError::InsufficientDisk {
                free_kb: 1024,
                required_kb: MIN_FREE_KB
            })
        );

        let read_only =
            parse_probe_output(&CANONICAL.replace("home_writable=yes", "home_writable=no"))
                .unwrap();
        assert_eq!(
            read_only.validate(MIN_FREE_KB),
            Err(EnvironmentError::HomeNotWritable)
        );

        let no_sha =
            parse_probe_output(&CANONICAL.replace("sha256sum=yes", "sha256sum=no")).unwrap();
        assert_eq!(
            no_sha.validate(MIN_FREE_KB),
            Err(EnvironmentError::MissingSha256sum)
        );
    }

    #[test]
    fn a_busy_port_is_blocking_but_an_unknown_port_only_warns() {
        let busy = parse_probe_output(&CANONICAL.replace("port=free", "port=busy")).unwrap();
        assert_eq!(busy.validate(MIN_FREE_KB), Err(EnvironmentError::PortBusy));

        let unknown = parse_probe_output(&CANONICAL.replace("port=free", "port=unknown")).unwrap();
        assert_eq!(unknown.validate(MIN_FREE_KB), Ok(()));
        assert_eq!(unknown.warnings, vec![EnvironmentWarning::PortUnknown]);
    }

    #[test]
    fn optional_git_and_shell_produce_warnings() {
        let output = CANONICAL
            .replace("git=yes", "git=no")
            .replace("shell=/bin/bash", "shell=");
        let environment = parse_probe_output(&output).unwrap();
        assert!(!environment.git_available);
        assert_eq!(
            environment.warnings,
            vec![
                EnvironmentWarning::GitMissing,
                EnvironmentWarning::ShellUnknown
            ]
        );
    }

    #[test]
    fn unknown_lines_are_ignored_and_equals_signs_survive_in_paths() {
        let output = format!("Welcome to the host\n{CANONICAL}home=/home/a=b\n");
        let environment = parse_probe_output(&output).unwrap();
        assert_eq!(environment.home, "/home/a=b");
    }

    #[test]
    fn malformed_known_values_fail() {
        for (key, value) in [
            ("free_kb", "many"),
            ("git", "maybe"),
            ("sha256sum", "perhaps"),
            ("home_writable", "sometimes"),
            ("port", "closed"),
            ("runtime", "maybe"),
            ("glibc", "2"),
        ] {
            let line = format!("{key}={value}");
            let output = CANONICAL
                .lines()
                .filter(|line| !line.starts_with(&format!("{key}=")))
                .collect::<Vec<_>>()
                .join("\n");
            let output = format!("{output}\n{line}\n");
            assert!(
                matches!(
                    parse_probe_output(&output),
                    Err(EnvironmentError::MalformedOutput { .. })
                ),
                "{key}={value} should be malformed"
            );
        }
    }

    #[test]
    fn a_probe_without_its_completion_marker_fails() {
        assert_eq!(
            parse_probe_output(&CANONICAL.replace("probe=ok\n", "")),
            Err(EnvironmentError::ProbeIncomplete)
        );
        assert_eq!(
            parse_probe_output(&CANONICAL.replace("probe=ok", "probe=interrupted")),
            Err(EnvironmentError::ProbeIncomplete)
        );
    }

    #[test]
    fn the_script_embeds_only_a_numeric_port_and_keeps_its_invariants() {
        let script = probe_script(8765);
        assert!(script.contains("sport = :8765"));
        assert!(script.contains("command -v sha256sum"));
        assert!(script.contains("probe=ok"));
        assert!(script.contains(".local/share/vibex/server/current/vibex-server"));
        assert!(!script.contains("sh -c"));
    }

    #[cfg(all(target_os = "linux", target_env = "gnu"))]
    #[test]
    fn the_probe_script_executes_and_parses_on_linux() {
        use std::io::Write as _;
        use std::process::{Command, Stdio};

        let _process_guard = crate::ssh_connection::test_support::process_guard();

        let mut child = Command::new("sh")
            .arg("-s")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(probe_script(8765).as_bytes())
            .unwrap();
        let output = child.wait_with_output().unwrap();
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let environment = parse_probe_output(&String::from_utf8_lossy(&output.stdout)).unwrap();
        assert_eq!(environment.os, "Linux");
        assert!(environment.glibc >= MIN_GLIBC);
        assert!(environment.free_kb > 0);
        // Port 8765 may legitimately be in use on the test host; the script's
        // detection is covered by the parser fixture instead.
    }

    #[cfg(unix)]
    #[test]
    fn probe_runs_the_fixed_script_through_the_ssh_command() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = crate::ssh_connection::test_support::unique_path(dir.path(), "fake-ssh");
        crate::ssh_connection::test_support::write_executable(
            &ssh,
            "#!/bin/sh\ncat >/dev/null\ncat <<'OUT'\nprobe=ok\nos=Linux\narch=x86_64\nhome=/home/dev\nshell=/bin/bash\ngit=yes\nsha256sum=yes\nfree_kb=1048576\nglibc=2.36\nhome_writable=yes\nport=free\nruntime=absent\nOUT\n",
        );

        let command = SshCommand {
            program: ssh,
            args: Vec::new(),
            stdin: None,
        };
        let environment = probe(&command, 8765, MIN_FREE_KB).unwrap();
        assert_eq!(environment.arch, RemoteArch::X86_64);
        assert_eq!(environment.port_state, PortState::Free);
        assert!(!environment.runtime_present);
    }
}

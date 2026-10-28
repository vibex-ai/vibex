//! Host-key trust checks.
//!
//! Unknown hosts are not accepted silently: the caller checks known_hosts
//! first, can show a scanned fingerprint, and only then writes the host key.
//! A changed key is a hard failure surfaced by `SshFailureKind::HostKeyChanged`.

use std::env;
use std::path::{Path, PathBuf};
use std::process::Command;

use super::error::SshFailure;

/// `~/.ssh/known_hosts` on Unix and `%USERPROFILE%\.ssh\known_hosts` on Windows.
pub fn default_known_hosts_path() -> Option<PathBuf> {
    #[cfg(windows)]
    let home = env::var_os("USERPROFILE").or_else(|| env::var_os("HOME"));
    #[cfg(not(windows))]
    let home = env::var_os("HOME");
    home.filter(|value| !value.to_string_lossy().is_empty())
        .map(PathBuf::from)
        .map(|home| home.join(".ssh").join("known_hosts"))
}

/// Returns whether `known_hosts` already contains an entry for `host`.
///
/// `ssh-keygen -F` exits 0 when it finds the host and 1 when it does not; any
/// other status is a real helper failure.
pub fn known_hosts_contains(
    ssh_keygen: &Path,
    known_hosts: &Path,
    host: &str,
) -> Result<bool, SshFailure> {
    validate_host(host)?;
    let output = Command::new(ssh_keygen)
        .args(["-F", host, "-f"])
        .arg(known_hosts)
        .env("LC_ALL", "C")
        .output()
        .map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                SshFailure::client_missing(&error.to_string())
            } else {
                SshFailure::spawn(&error.to_string())
            }
        })?;
    match output.status.code() {
        Some(0) => Ok(!output.stdout.is_empty()),
        Some(1) => Ok(false),
        _ => Err(SshFailure::from_parts(
            output.status.code(),
            &String::from_utf8_lossy(&output.stderr),
        )),
    }
}

fn validate_host(host: &str) -> Result<(), SshFailure> {
    if !super::command::host_is_valid(host) {
        return Err(SshFailure::invalid_request("invalid SSH host"));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::path::PathBuf;

    use super::*;
    use crate::ssh_connection::error::SshFailureKind;

    #[cfg(unix)]
    fn fake_keygen(dir: &Path, body: &str) -> PathBuf {
        use std::os::unix::fs::PermissionsExt as _;

        let path = dir.join("fake-keygen");
        fs::write(&path, format!("#!/bin/sh\n{body}\n")).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o755)).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn reports_known_and_unknown_hosts() {
        let dir = tempfile::tempdir().unwrap();
        let known = fake_keygen(dir.path(), "printf 'host ssh-ed25519 AAAA\\n'");
        assert!(known_hosts_contains(&known, &dir.path().join("known_hosts"), "host").unwrap());

        let unknown = fake_keygen(dir.path(), "exit 1");
        assert!(!known_hosts_contains(&unknown, &dir.path().join("known_hosts"), "host").unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn reports_a_helper_failure() {
        let dir = tempfile::tempdir().unwrap();
        let broken = fake_keygen(dir.path(), "printf 'boom\\n' >&2; exit 2");
        let failure =
            known_hosts_contains(&broken, &dir.path().join("known_hosts"), "host").unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::RemoteCommand);
    }

    #[test]
    fn rejects_an_injection_shaped_host_before_spawning() {
        let failure = known_hosts_contains(
            Path::new("ssh-keygen"),
            Path::new("/tmp/known_hosts"),
            "-oProxyCommand=id",
        )
        .unwrap_err();
        assert_eq!(failure.kind, SshFailureKind::InvalidRequest);
    }
}

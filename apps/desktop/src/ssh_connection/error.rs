//! Typed SSH failures.
//!
//! Classification is intentionally based on OpenSSH's stable wording plus the
//! exit code, not on a single generic "connection failed" error. The UI needs
//! to tell authentication, host-key, DNS, refusal, timeout, and remote-command
//! failures apart because each has a different recovery path.

use std::fmt;

const MAX_STDERR_TAIL_CHARS: usize = 4096;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshFailureKind {
    Authentication,
    HostKeyUnknown,
    HostKeyChanged,
    ConnectionRefused,
    Timeout,
    Dns,
    PermissionDenied,
    RemoteCommand,
    ClientMissing,
    Spawn,
    InvalidRequest,
    Other,
}

impl SshFailureKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::Authentication => "ssh_auth_failed",
            Self::HostKeyUnknown => "ssh_host_key_unknown",
            Self::HostKeyChanged => "ssh_host_key_changed",
            Self::ConnectionRefused => "ssh_connection_refused",
            Self::Timeout => "ssh_timeout",
            Self::Dns => "ssh_dns_failed",
            Self::PermissionDenied => "ssh_permission_denied",
            Self::RemoteCommand => "ssh_remote_command_failed",
            Self::ClientMissing => "ssh_client_missing",
            Self::Spawn => "ssh_spawn_failed",
            Self::InvalidRequest => "ssh_invalid_request",
            Self::Other => "ssh_failed",
        }
    }

    const fn message(self) -> &'static str {
        match self {
            Self::Authentication => "SSH authentication failed",
            Self::HostKeyUnknown => "the host key is not trusted yet",
            Self::HostKeyChanged => "the host key changed since the last connection",
            Self::ConnectionRefused => "the host refused the SSH connection",
            Self::Timeout => "the SSH connection timed out",
            Self::Dns => "the host name could not be resolved",
            Self::PermissionDenied => "the SSH operation was denied",
            Self::RemoteCommand => "the remote command failed",
            Self::ClientMissing => "the SSH client or helper could not be found",
            Self::Spawn => "the SSH client could not be started",
            Self::InvalidRequest => "the SSH request is invalid",
            Self::Other => "the SSH command failed",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshFailure {
    pub kind: SshFailureKind,
    pub exit_code: Option<i32>,
    pub message: String,
    pub stderr_tail: String,
}

impl SshFailure {
    pub fn from_parts(exit_code: Option<i32>, stderr: &str) -> Self {
        let kind = classify(exit_code, stderr);
        Self {
            kind,
            exit_code,
            message: kind.message().to_string(),
            stderr_tail: tail(stderr, MAX_STDERR_TAIL_CHARS),
        }
    }

    pub fn timeout(stderr: &str) -> Self {
        Self {
            kind: SshFailureKind::Timeout,
            exit_code: None,
            message: SshFailureKind::Timeout.message().to_string(),
            stderr_tail: tail(stderr, MAX_STDERR_TAIL_CHARS),
        }
    }

    pub fn client_missing(detail: &str) -> Self {
        Self {
            kind: SshFailureKind::ClientMissing,
            exit_code: None,
            message: SshFailureKind::ClientMissing.message().to_string(),
            stderr_tail: tail(detail, MAX_STDERR_TAIL_CHARS),
        }
    }

    pub fn invalid_request(detail: &str) -> Self {
        Self {
            kind: SshFailureKind::InvalidRequest,
            exit_code: None,
            message: SshFailureKind::InvalidRequest.message().to_string(),
            stderr_tail: tail(detail, MAX_STDERR_TAIL_CHARS),
        }
    }

    pub fn spawn(detail: &str) -> Self {
        Self {
            kind: SshFailureKind::Spawn,
            exit_code: None,
            message: SshFailureKind::Spawn.message().to_string(),
            stderr_tail: tail(detail, MAX_STDERR_TAIL_CHARS),
        }
    }

    pub fn code(&self) -> &'static str {
        self.kind.code()
    }

    pub fn is_retryable(&self) -> bool {
        matches!(
            self.kind,
            SshFailureKind::ConnectionRefused | SshFailureKind::Timeout | SshFailureKind::Spawn
        )
    }
}

impl fmt::Display for SshFailure {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for SshFailure {}

/// OpenSSH writes host-key problems to stderr on exit 255. The changed-host
/// warning contains "Host key verification failed" too, so it must be checked
/// before the unknown-host case.
fn classify(exit_code: Option<i32>, stderr: &str) -> SshFailureKind {
    let lower = stderr.to_ascii_lowercase();
    if lower.contains("remote host identification has changed")
        || (lower.contains("host key for") && lower.contains("has changed"))
        || (lower.contains("host key verification failed") && lower.contains("changed"))
    {
        return SshFailureKind::HostKeyChanged;
    }
    if (lower.contains("no ") && lower.contains("host key is known"))
        || lower.contains("authenticity of host")
        || lower.contains("host key verification failed")
    {
        return SshFailureKind::HostKeyUnknown;
    }
    if lower.contains("permission denied (publickey")
        || lower.contains("authentication failed")
        || lower.contains("too many authentication failures")
        || lower.contains("no supported authentication methods")
        || (exit_code == Some(255) && lower.contains("permission denied"))
    {
        return SshFailureKind::Authentication;
    }
    if lower.contains("connection refused") {
        return SshFailureKind::ConnectionRefused;
    }
    if lower.contains("connection timed out") || lower.contains("operation timed out") {
        return SshFailureKind::Timeout;
    }
    if lower.contains("could not resolve hostname")
        || lower.contains("name or service not known")
        || lower.contains("nodename nor servname provided")
    {
        return SshFailureKind::Dns;
    }
    if exit_code == Some(255) {
        return SshFailureKind::Other;
    }
    if exit_code.is_some_and(|code| code != 0) {
        return SshFailureKind::RemoteCommand;
    }
    SshFailureKind::Other
}

fn tail(value: &str, max_chars: usize) -> String {
    let mut characters = value.chars().rev().take(max_chars).collect::<Vec<_>>();
    characters.reverse();
    characters.into_iter().collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn classifies_changed_before_unknown() {
        let failure = SshFailure::from_parts(
            Some(255),
            "@@@@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED! @@@@@@\n\
             Host key for 10.0.0.5 has changed and you have requested strict checking.\n\
             Host key verification failed.\n",
        );
        assert_eq!(failure.kind, SshFailureKind::HostKeyChanged);
        assert_eq!(failure.code(), "ssh_host_key_changed");
    }

    #[test]
    fn classifies_unknown_and_authentication() {
        let unknown = SshFailure::from_parts(
            Some(255),
            "No ED25519 host key is known for 10.0.0.5 and you have requested strict checking.\n\
             Host key verification failed.\n",
        );
        assert_eq!(unknown.kind, SshFailureKind::HostKeyUnknown);

        let auth =
            SshFailure::from_parts(Some(255), "dev@10.0.0.5: Permission denied (publickey).\n");
        assert_eq!(auth.kind, SshFailureKind::Authentication);
        assert_eq!(auth.code(), "ssh_auth_failed");
    }

    #[test]
    fn classifies_transport_failures() {
        assert_eq!(
            SshFailure::from_parts(
                Some(255),
                "connect to host 10.0.0.5 port 22: Connection refused\n"
            )
            .kind,
            SshFailureKind::ConnectionRefused
        );
        assert_eq!(
            SshFailure::from_parts(
                Some(255),
                "ssh: connect to host 10.0.0.5 port 22: Connection timed out\n"
            )
            .kind,
            SshFailureKind::Timeout
        );
        assert_eq!(
            SshFailure::from_parts(
                Some(255),
                "ssh: Could not resolve hostname nope: Name or service not known\n"
            )
            .kind,
            SshFailureKind::Dns
        );
    }

    #[test]
    fn classifies_remote_command_failure() {
        let failure = SshFailure::from_parts(Some(1), "bash: nope: command not found\n");
        assert_eq!(failure.kind, SshFailureKind::RemoteCommand);
        assert_eq!(failure.code(), "ssh_remote_command_failed");
        assert!(failure.stderr_tail.contains("command not found"));
    }

    #[test]
    fn timeout_and_spawn_have_distinct_codes() {
        assert_eq!(SshFailure::timeout("slow").code(), "ssh_timeout");
        assert_eq!(
            SshFailure::client_missing("ssh").code(),
            "ssh_client_missing"
        );
        assert!(SshFailure::timeout("slow").is_retryable());
    }
}

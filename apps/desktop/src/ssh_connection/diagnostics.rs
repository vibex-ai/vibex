//! Structured SSH failure cards.
//!
//! OpenSSH reports failures as text, and provisioning wraps that text in
//! several layers. This module maps the stable error codes to one actionable
//! card shape so the UI can offer the right next step instead of a bare
//! "connection failed". It never executes a recovery action itself.

use super::error::SshFailure;
use crate::locale;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshDiagnosticKind {
    MissingClient,
    MissingSshd,
    RemoteLoginDisabled,
    Authentication,
    HostKeyUnknown,
    HostKeyChanged,
    ConnectionRefused,
    Timeout,
    Dns,
    Permission,
    Disk,
    Glibc,
    Port,
    Start,
    Version,
    Tunnel,
    Pairing,
    Upgrade,
    Unknown,
}

impl SshDiagnosticKind {
    pub const fn code(self) -> &'static str {
        match self {
            Self::MissingClient => "missing_client",
            Self::MissingSshd => "missing_sshd",
            Self::RemoteLoginDisabled => "remote_login_disabled",
            Self::Authentication => "authentication",
            Self::HostKeyUnknown => "host_key_unknown",
            Self::HostKeyChanged => "host_key_changed",
            Self::ConnectionRefused => "connection_refused",
            Self::Timeout => "timeout",
            Self::Dns => "dns",
            Self::Permission => "permission",
            Self::Disk => "disk",
            Self::Glibc => "glibc",
            Self::Port => "port",
            Self::Start => "start",
            Self::Version => "version",
            Self::Tunnel => "tunnel",
            Self::Pairing => "pairing",
            Self::Upgrade => "upgrade",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SshDiagnosticAction {
    Retry,
    InstallClient,
    BootstrapSshd,
    EnableRemoteLogin,
    ReviewHostKey,
    CheckPermissions,
    CheckDisk,
    UpdateRuntime,
    UpdateVibex,
    CopyDiagnostics,
    CopyLogCommand,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SshDiagnostic {
    pub kind: SshDiagnosticKind,
    pub code: String,
    pub message: String,
    pub actions: Vec<SshDiagnosticAction>,
    pub host_key_fingerprints: Vec<String>,
    /// Short recovery instructions specific to the classified error.
    pub guidance: Option<String>,
    pub diagnostics: String,
    pub log_command: Option<String>,
}

/// Maps a stable code and its remote stderr tail to one card. `host_alias` and
/// `log_path` are optional because some failures happen before a profile is
/// resolved; when present they become part of the copyable diagnostics.
pub fn diagnose(
    code: &str,
    message: &str,
    stderr_tail: &str,
    host_alias: Option<&str>,
    log_path: Option<&str>,
) -> SshDiagnostic {
    diagnose_with_platform(code, message, stderr_tail, host_alias, log_path, None)
}

/// Like [`diagnose`], but uses a known remote OS to disambiguate a refused
/// connection. On macOS a closed port usually means Remote Login is off, while
/// on Linux it usually means sshd is not installed/running.
pub fn diagnose_with_platform(
    code: &str,
    message: &str,
    stderr_tail: &str,
    host_alias: Option<&str>,
    log_path: Option<&str>,
    remote_os: Option<&str>,
) -> SshDiagnostic {
    let kind = classify_with_platform(code, remote_os);
    let host_key_fingerprints = fingerprints(stderr_tail);
    let mut actions = match kind {
        SshDiagnosticKind::MissingClient => vec![
            SshDiagnosticAction::InstallClient,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::MissingSshd | SshDiagnosticKind::ConnectionRefused => vec![
            SshDiagnosticAction::BootstrapSshd,
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::RemoteLoginDisabled => vec![
            SshDiagnosticAction::EnableRemoteLogin,
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Authentication => vec![
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::HostKeyUnknown | SshDiagnosticKind::HostKeyChanged => vec![
            SshDiagnosticAction::ReviewHostKey,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Permission => vec![
            SshDiagnosticAction::CheckPermissions,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Disk => vec![
            SshDiagnosticAction::CheckDisk,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Glibc => vec![
            SshDiagnosticAction::UpdateRuntime,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Port => vec![
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Start
        | SshDiagnosticKind::Tunnel
        | SshDiagnosticKind::Pairing
        | SshDiagnosticKind::Unknown => vec![
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
            SshDiagnosticAction::CopyLogCommand,
        ],
        SshDiagnosticKind::Version => vec![
            SshDiagnosticAction::UpdateRuntime,
            SshDiagnosticAction::UpdateVibex,
            SshDiagnosticAction::CopyDiagnostics,
        ],
        SshDiagnosticKind::Upgrade => vec![
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
            SshDiagnosticAction::CopyLogCommand,
        ],
        SshDiagnosticKind::Timeout | SshDiagnosticKind::Dns => vec![
            SshDiagnosticAction::Retry,
            SshDiagnosticAction::CopyDiagnostics,
        ],
    };
    actions.dedup();

    // A refused connection to a host whose OS is not known yet (the first
    // connection) is as likely a Mac with Remote Login off as a Linux host
    // without sshd, so the card offers both ways forward.
    let os_unknown_refusal = code == "ssh_connection_refused" && remote_os.is_none();
    if os_unknown_refusal {
        actions.insert(1, SshDiagnosticAction::EnableRemoteLogin);
    }
    let guidance = match kind {
        _ if os_unknown_refusal => Some(locale::text(
            "Nothing accepted the SSH connection. On Linux, install and start an SSH server, or bootstrap it from the provider console. On a Mac, turn on System Settings → General → Sharing → Remote Login, or run `sudo systemsetup -setremotelogin on`.",
            "SSH 连接被拒绝。Linux 主机请安装并启动 SSH 服务，或通过云厂商控制台初始化；Mac 请开启 系统设置 → 通用 → 共享 → 远程登录，或运行 `sudo systemsetup -setremotelogin on`。",
            "SSH 連線被拒絕。Linux 主機請安裝並啟動 SSH 服務，或透過雲端供應商主控台初始化；Mac 請開啟 系統設定 → 一般 → 共享 → 遠端登入，或執行 `sudo systemsetup -setremotelogin on`。",
        )),
        SshDiagnosticKind::RemoteLoginDisabled => Some(locale::text(
            "On the Mac, open System Settings → General → Sharing → Remote Login, or run `sudo systemsetup -setremotelogin on`.",
            "在 Mac 上打开 系统设置 → 通用 → 共享 → 远程登录，或运行 `sudo systemsetup -setremotelogin on`。",
            "在 Mac 上開啟 系統設定 → 一般 → 共享 → 遠端登入，或執行 `sudo systemsetup -setremotelogin on`。",
        )),
        SshDiagnosticKind::MissingClient => Some(locale::text(
            "Install OpenSSH: `sudo apt install openssh-client` on Debian/Ubuntu, or `brew install openssh` on macOS.",
            "安装 OpenSSH：Debian/Ubuntu 运行 `sudo apt install openssh-client`，macOS 运行 `brew install openssh`。",
            "安裝 OpenSSH：Debian/Ubuntu 執行 `sudo apt install openssh-client`，macOS 執行 `brew install openssh`。",
        )),
        SshDiagnosticKind::MissingSshd | SshDiagnosticKind::ConnectionRefused => Some(locale::text(
            "Install and start an SSH server, or use the provider console to bootstrap it.",
            "安装并启动 SSH 服务，或通过云厂商控制台初始化。",
            "安裝並啟動 SSH 服務，或透過雲端供應商主控台初始化。",
        )),
        SshDiagnosticKind::Disk => Some(locale::text(
            "Free space under the remote home or install root, then retry.",
            "清理远端主目录或安装目录所在磁盘的空间，然后重试。",
            "清理遠端主目錄或安裝目錄所在磁碟的空間，然後重試。",
        )),
        SshDiagnosticKind::Permission => Some(locale::text(
            "Check ownership and write permission on the remote home and install root.",
            "检查远端主目录和安装目录的所有者与写权限。",
            "檢查遠端主目錄與安裝目錄的擁有者與寫入權限。",
        )),
        SshDiagnosticKind::Port => Some(locale::text(
            "Free the runtime port or retry so the next free port is selected.",
            "释放运行时端口，或重试以自动选择下一个空闲端口。",
            "釋放執行階段連接埠，或重試以自動選擇下一個空閒連接埠。",
        )),
        _ => None,
    }
    .map(str::to_string);
    let log_command = log_path
        .zip(host_alias)
        .map(|(path, alias)| format!("ssh {alias} 'tail -n 200 {path}'"));
    let mut diagnostics = format!("code: {code}\nmessage: {message}\n");
    if let Some(alias) = host_alias {
        diagnostics.push_str(&format!("host: {alias}\n"));
    }
    if let Some(path) = log_path {
        diagnostics.push_str(&format!("log: {path}\n"));
    }
    if !host_key_fingerprints.is_empty() {
        diagnostics.push_str(&format!(
            "host-key-fingerprints: {}\n",
            host_key_fingerprints.join(", ")
        ));
    }
    if !stderr_tail.is_empty() {
        diagnostics.push_str("stderr:\n");
        diagnostics.push_str(stderr_tail);
        if !stderr_tail.ends_with('\n') {
            diagnostics.push('\n');
        }
    }
    if let Some(command) = &log_command {
        diagnostics.push_str(&format!("log-command: {command}\n"));
    }

    SshDiagnostic {
        kind,
        code: code.to_string(),
        message: message.to_string(),
        actions,
        host_key_fingerprints,
        guidance,
        diagnostics,
        log_command,
    }
}

pub fn classify_with_platform(code: &str, remote_os: Option<&str>) -> SshDiagnosticKind {
    if code == "ssh_connection_refused" && remote_os.is_some_and(is_macos) {
        return SshDiagnosticKind::RemoteLoginDisabled;
    }
    classify(code)
}

fn is_macos(value: &str) -> bool {
    matches!(
        value.trim().to_ascii_lowercase().as_str(),
        "macos" | "mac" | "osx" | "darwin"
    )
}

pub fn classify(code: &str) -> SshDiagnosticKind {
    match code {
        "ssh_client_missing" | "ssh_spawn_failed" => SshDiagnosticKind::MissingClient,
        "ssh_auth_failed" => SshDiagnosticKind::Authentication,
        "ssh_host_key_unknown" => SshDiagnosticKind::HostKeyUnknown,
        "ssh_host_key_changed" => SshDiagnosticKind::HostKeyChanged,
        "ssh_connection_refused" => SshDiagnosticKind::MissingSshd,
        "ssh_timeout" => SshDiagnosticKind::Timeout,
        "ssh_dns_failed" => SshDiagnosticKind::Dns,
        "ssh_permission_denied" | "ssh_environment_home_not_writable" => {
            SshDiagnosticKind::Permission
        }
        "ssh_environment_disk_low" => SshDiagnosticKind::Disk,
        "ssh_environment_glibc_too_old" | "ssh_environment_missing_glibc" => {
            SshDiagnosticKind::Glibc
        }
        "ssh_environment_port_busy" | "ssh_launch_port_invalid" => SshDiagnosticKind::Port,
        "ssh_launch_failed"
        | "ssh_launch_unexpected"
        | "ssh_runtime_start_failed"
        | "ssh_runtime_health_timeout"
        | "ssh_runtime_still_running" => SshDiagnosticKind::Start,
        "ssh_upgrade_incompatible" => SshDiagnosticKind::Version,
        code if code.starts_with("ssh_tunnel") => SshDiagnosticKind::Tunnel,
        code if code.starts_with("ssh_pairing") || code.starts_with("remote_") => {
            SshDiagnosticKind::Pairing
        }
        code if code.starts_with("ssh_upgrade") => SshDiagnosticKind::Upgrade,
        _ => SshDiagnosticKind::Unknown,
    }
}

/// OpenSSH prints host fingerprints as `SHA256:<base64>` tokens. Keep them in
/// order and dedupe; a changed-key message normally carries old and new.
fn fingerprints(stderr: &str) -> Vec<String> {
    let mut found: Vec<String> = Vec::new();
    for token in stderr.split_whitespace() {
        let token = token.trim_matches(|character: char| {
            matches!(character, '(' | ')' | ',' | '.' | ';' | ':' | '\'' | '"')
        });
        if token.starts_with("SHA256:") && !found.iter().any(|value| value == token) {
            found.push(token.to_string());
        }
    }
    found
}

/// Convenience for a classified `SshFailure`.
pub fn diagnose_failure(
    failure: &SshFailure,
    host_alias: Option<&str>,
    log_path: Option<&str>,
) -> SshDiagnostic {
    diagnose(
        failure.code(),
        &failure.message,
        &failure.stderr_tail,
        host_alias,
        log_path,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn authentication_offers_retry_and_copy() {
        let card = diagnose(
            "ssh_auth_failed",
            "SSH authentication failed",
            "",
            None,
            None,
        );
        assert_eq!(card.kind, SshDiagnosticKind::Authentication);
        assert!(card.actions.contains(&SshDiagnosticAction::Retry));
        assert!(card.actions.contains(&SshDiagnosticAction::CopyDiagnostics));
        assert!(card.diagnostics.contains("ssh_auth_failed"));
    }

    #[test]
    fn a_changed_host_key_extracts_both_fingerprints() {
        let stderr = "@@@ WARNING: REMOTE HOST IDENTIFICATION HAS CHANGED!\n\
Old key fingerprint is SHA256:oldOldOldOldOld.\n\
New key fingerprint is SHA256:newNewNewNewNew.\n";
        let card = diagnose(
            "ssh_host_key_changed",
            "the host key changed since the last connection",
            stderr,
            Some("build-box"),
            Some("/home/dev/.local/state/vibex/server/server.log"),
        );
        assert_eq!(card.kind, SshDiagnosticKind::HostKeyChanged);
        assert_eq!(card.host_key_fingerprints.len(), 2);
        assert!(card.host_key_fingerprints[0].starts_with("SHA256:old"));
        assert!(card.diagnostics.contains("host: build-box"));
        assert!(
            card.log_command
                .as_deref()
                .unwrap()
                .contains("ssh build-box")
        );
    }

    #[test]
    fn disk_permission_and_client_errors_get_specific_actions() {
        assert!(
            diagnose("ssh_environment_disk_low", "low disk", "", None, None)
                .actions
                .contains(&SshDiagnosticAction::CheckDisk)
        );
        assert!(
            diagnose(
                "ssh_environment_home_not_writable",
                "read-only",
                "",
                None,
                None
            )
            .actions
            .contains(&SshDiagnosticAction::CheckPermissions)
        );
        assert!(
            diagnose("ssh_client_missing", "no ssh", "", None, None)
                .actions
                .contains(&SshDiagnosticAction::InstallClient)
        );
    }

    #[test]
    fn connection_refused_points_at_bootstrap_and_retry() {
        let card = diagnose("ssh_connection_refused", "refused", "", None, None);
        assert_eq!(card.kind, SshDiagnosticKind::MissingSshd);
        assert!(card.actions.contains(&SshDiagnosticAction::BootstrapSshd));
        assert!(card.actions.contains(&SshDiagnosticAction::Retry));
    }

    #[test]
    fn a_known_macos_host_classifies_a_refused_connection_as_remote_login() {
        let card = diagnose_with_platform(
            "ssh_connection_refused",
            "refused",
            "",
            Some("mac-mini"),
            None,
            Some("macOS"),
        );
        assert_eq!(card.kind, SshDiagnosticKind::RemoteLoginDisabled);
        assert!(
            card.actions
                .contains(&SshDiagnosticAction::EnableRemoteLogin)
        );
        assert!(!card.actions.contains(&SshDiagnosticAction::BootstrapSshd));

        let linux = diagnose_with_platform(
            "ssh_connection_refused",
            "refused",
            "",
            Some("build-box"),
            None,
            Some("linux"),
        );
        assert_eq!(linux.kind, SshDiagnosticKind::MissingSshd);
        assert!(
            !linux
                .actions
                .contains(&SshDiagnosticAction::EnableRemoteLogin),
            "a known Linux host is not pointed at Remote Login"
        );
    }

    #[test]
    fn a_refusal_before_the_os_is_known_offers_sshd_and_remote_login() {
        let card = diagnose(
            "ssh_connection_refused",
            "refused",
            "",
            Some("new-box"),
            None,
        );
        assert_eq!(card.kind, SshDiagnosticKind::MissingSshd);
        assert_eq!(
            card.actions[..3],
            [
                SshDiagnosticAction::BootstrapSshd,
                SshDiagnosticAction::EnableRemoteLogin,
                SshDiagnosticAction::Retry,
            ]
        );
        // The command is the same in every locale, so this holds whichever
        // locale another test left the process in.
        let guidance = card.guidance.expect("the card explains both cases");
        assert!(guidance.contains("SSH"));
        assert!(guidance.contains("sudo systemsetup -setremotelogin on"));
    }

    #[test]
    fn unknown_codes_still_have_a_retry_and_log_escape_hatch() {
        let card = diagnose("something_new", "unexpected", "detail", None, None);
        assert_eq!(card.kind, SshDiagnosticKind::Unknown);
        assert!(card.actions.contains(&SshDiagnosticAction::Retry));
        assert!(card.actions.contains(&SshDiagnosticAction::CopyLogCommand));
    }
}

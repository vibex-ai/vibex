//! One-time bootstrap command generation.
//!
//! The command embeds the audited bootstrap payload, verifies its SHA-256, and
//! passes a short-lived token. The payload records the token in root-owned
//! state after a successful run, so replaying the same command on that host is
//! rejected. The token is transferable by design: it is a console capability,
//! not a secret that survives a hostile root user.

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub const TTL_MS: i64 = 15 * 60 * 1000;
pub const SCRIPT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../scripts/vibex-bootstrap.sh"
));

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapCommand {
    pub command: String,
    pub token: String,
    pub expires_at_ms: i64,
    pub script_sha256: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BootstrapError {
    pub code: &'static str,
    pub message: String,
}

impl BootstrapError {
    fn new(code: &'static str, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }
}

impl std::fmt::Display for BootstrapError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for BootstrapError {}

pub fn generate(
    user: &str,
    public_key: &str,
    port: u16,
    now_ms: i64,
) -> Result<BootstrapCommand, BootstrapError> {
    validate_user(user)?;
    validate_key(public_key)?;
    if port == 0 {
        return Err(BootstrapError::new(
            "bootstrap_port_invalid",
            "the SSH port must not be zero",
        ));
    }
    let expires_at_ms = now_ms.saturating_add(TTL_MS);
    let token = format!("v1.{expires_at_ms}.{}", Uuid::new_v4().simple());
    let script_sha256 = format!("{:x}", Sha256::digest(SCRIPT.as_bytes()));
    let payload = BASE64_STANDARD.encode(SCRIPT.as_bytes());
    let inner = format!(
        "set -eu\numask 077\nd=$(mktemp -d)\ntrap 'rm -rf \"$d\"' EXIT\nprintf %s {} | base64 -d > \"$d/vibex-bootstrap.sh\"\ncd \"$d\"\necho '{}  vibex-bootstrap.sh' | sha256sum -c - >/dev/null\nsh \"$d/vibex-bootstrap.sh\" --user {} --key {} --port {} --token {}",
        shell_quote(&payload),
        script_sha256,
        shell_quote(user),
        shell_quote(public_key),
        port,
        shell_quote(&token),
    );
    Ok(BootstrapCommand {
        command: format!("sudo sh -c {}", shell_quote(&inner)),
        token,
        expires_at_ms,
        script_sha256,
    })
}

pub fn validate_user(user: &str) -> Result<(), BootstrapError> {
    if user.is_empty()
        || user.len() > 32
        || !user
            .chars()
            .next()
            .is_some_and(|character| character.is_ascii_lowercase() || character == '_')
        || !user
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '_' | '-'))
    {
        return Err(BootstrapError::new(
            "bootstrap_user_invalid",
            "the Linux user name is invalid",
        ));
    }
    Ok(())
}

pub fn validate_key(public_key: &str) -> Result<(), BootstrapError> {
    if public_key.contains('\n')
        || public_key.contains('\0')
        || !(public_key.starts_with("ssh-ed25519 ")
            || public_key.starts_with("ssh-rsa ")
            || public_key.starts_with("ecdsa-sha2-")
            || public_key.starts_with("sk-ssh-ed25519@openssh.com ")
            || public_key.starts_with("sk-ecdsa-sha2-nistp256@openssh.com "))
    {
        return Err(BootstrapError::new(
            "bootstrap_key_invalid",
            "the desktop public key is invalid",
        ));
    }
    Ok(())
}

fn shell_quote(value: &str) -> String {
    format!("'{}'", value.replace('\'', "'\\''"))
}

#[cfg(test)]
mod tests {
    use super::*;

    const KEY: &str = "ssh-ed25519 AAAAC3NzaC1lZDI1NTE5AAAAITest vibex:desktop";

    #[test]
    fn generates_a_quoted_command_with_an_expiring_token() {
        let command = generate("ubuntu", KEY, 2222, 1_000).unwrap();
        assert!(command.command.starts_with("sudo sh -c '"));
        assert!(command.command.contains("sha256sum -c"));
        assert!(command.command.contains("--port 2222"));
        assert!(command.command.contains(&command.token));
        assert!(command.command.contains("cd \"$d\""));
        assert!(command.command.contains("vibex-bootstrap.sh"));
        assert_eq!(command.expires_at_ms, 1_000 + TTL_MS);
        assert_eq!(command.token.split('.').count(), 3);
    }

    #[test]
    fn rejects_injection_shaped_users_and_bad_keys() {
        assert!(generate("root;reboot", KEY, 22, 0).is_err());
        assert!(generate("ubuntu", &format!("{KEY}\nssh-ed25519 x"), 22, 0).is_err());
        assert!(generate("ubuntu", "not-a-key", 22, 0).is_err());
    }

    #[cfg(unix)]
    #[test]
    fn generated_command_is_valid_shell() {
        let command = generate("ubuntu", KEY, 22, 1_000).unwrap();
        let status = std::process::Command::new("sh")
            .args(["-n", "-c", &command.command])
            .status()
            .unwrap();
        assert!(status.success());
    }

    #[test]
    fn script_is_embedded_and_hash_is_stable() {
        assert!(SCRIPT.contains("--token"));
        let first = generate("ubuntu", KEY, 22, 0).unwrap().script_sha256;
        let second = generate("ubuntu", KEY, 22, 0).unwrap().script_sha256;
        assert_eq!(first, second);
        assert_eq!(first.len(), 64);
    }
}

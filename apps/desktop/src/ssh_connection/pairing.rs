//! Automatic pairing for an SSH-provisioned runtime.
//!
//! The pairing code, certificate and endpoint are read from the runtime over
//! the SSH channel, the endpoint is replaced with the local tunnel address, and
//! the existing Remote v2 claim pins the certificate before the first request.
//! The code is never displayed or logged.

use std::fmt;
use std::path::Path;
use std::time::Duration;

use serde::Deserialize;
use url::Url;

use super::command::SshCommand;
use super::launch::{LaunchConfig, run_script};
use crate::remote_client::{
    DesktopRemoteCredential, DesktopRuntimeRegistryStore,
    claim_server_pairing_code_with_certificate,
};
use vibex_backend::{BackendError, BackendErrorKind};

pub const PAIRING_SCHEMA_VERSION: &str = "vibex-server-pairing.v1";
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_SERVER_ID_CHARS: usize = 128;

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PairingPayload {
    pub schema_version: String,
    pub server_id: String,
    pub server_version: String,
    pairing_code: String,
    pairing_link: String,
    tls_certificate_der: Option<String>,
    pub endpoint: String,
    pub expires_at_ms: i64,
}

impl fmt::Debug for PairingPayload {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("PairingPayload")
            .field("schema_version", &self.schema_version)
            .field("server_id", &self.server_id)
            .field("server_version", &self.server_version)
            .field("pairing_code", &"<redacted>")
            .field("pairing_link", &"<redacted>")
            .field(
                "tls_certificate_der",
                &self.tls_certificate_der.as_ref().map(|_| "<redacted>"),
            )
            .field("endpoint", &self.endpoint)
            .field("expires_at_ms", &self.expires_at_ms)
            .finish()
    }
}

impl PairingPayload {
    pub fn local_endpoint(&self, local_port: u16) -> Result<String, PairingError> {
        if local_port == 0 {
            return Err(PairingError::new(
                "ssh_pairing_local_port_invalid",
                "the local tunnel port must not be zero",
            ));
        }
        Ok(format!("https://127.0.0.1:{local_port}"))
    }
}

#[derive(Debug)]
pub struct PairingOutcome {
    pub runtime_id: String,
    pub credential: DesktopRemoteCredential,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PairingError {
    pub code: String,
    pub message: String,
    pub retryable: bool,
}

impl PairingError {
    fn new(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: false,
        }
    }

    fn retryable(code: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            code: code.into(),
            message: message.into(),
            retryable: true,
        }
    }

    fn from_launch(error: super::launch::LaunchError) -> Self {
        Self {
            code: "ssh_pairing_fetch_failed".to_string(),
            message: error.message,
            retryable: error.retryable,
        }
    }

    fn from_claim(error: BackendError) -> Self {
        Self {
            code: error.code,
            message: error.message,
            retryable: matches!(
                error.kind,
                BackendErrorKind::Offline | BackendErrorKind::Failed
            ),
        }
    }

    fn from_register(error: BackendError) -> Self {
        Self {
            code: "ssh_pairing_register_failed".to_string(),
            message: error.message,
            retryable: false,
        }
    }
}

impl fmt::Display for PairingError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.message)
    }
}

impl std::error::Error for PairingError {}

/// Runs `pairing-code --json` through the managed env and parses it.
pub fn fetch_pairing_code(
    command: &SshCommand,
    config: &LaunchConfig,
    now_ms: i64,
) -> Result<PairingPayload, PairingError> {
    let output = run_script(
        command,
        PAIRING_CODE_SCRIPT,
        &[config.env_file.clone(), config.binary.clone()],
        FETCH_TIMEOUT,
    )
    .map_err(PairingError::from_launch)?;
    parse_pairing_payload(&output.stdout_lossy(), now_ms)
}

pub fn parse_pairing_payload(json: &str, now_ms: i64) -> Result<PairingPayload, PairingError> {
    let payload: PairingPayload = serde_json::from_str(json).map_err(|_| {
        PairingError::new(
            "ssh_pairing_payload_invalid",
            "the remote runtime returned invalid pairing JSON",
        )
    })?;
    if payload.schema_version != PAIRING_SCHEMA_VERSION {
        return Err(PairingError::new(
            "ssh_pairing_schema_invalid",
            "the remote runtime returned an unsupported pairing schema",
        ));
    }
    if payload.server_id.is_empty()
        || payload.server_id.chars().count() > MAX_SERVER_ID_CHARS
        || payload.pairing_code.is_empty()
        || payload.pairing_link.is_empty()
        || payload.server_version.is_empty()
    {
        return Err(PairingError::new(
            "ssh_pairing_payload_invalid",
            "the remote runtime pairing payload is incomplete",
        ));
    }
    if payload
        .tls_certificate_der
        .as_deref()
        .is_none_or(str::is_empty)
    {
        return Err(PairingError::new(
            "ssh_pairing_certificate_missing",
            "the remote runtime did not provide a TLS certificate to pin",
        ));
    }
    let endpoint = Url::parse(&payload.endpoint).map_err(|_| {
        PairingError::new(
            "ssh_pairing_endpoint_invalid",
            "the remote runtime pairing endpoint is invalid",
        )
    })?;
    if endpoint.scheme() != "https" {
        return Err(PairingError::new(
            "ssh_pairing_endpoint_insecure",
            "the remote runtime pairing endpoint must use HTTPS",
        ));
    }
    if payload.expires_at_ms <= 0 {
        return Err(PairingError::new(
            "ssh_pairing_expiry_invalid",
            "the remote runtime pairing expiry is invalid",
        ));
    }
    if payload.expires_at_ms <= now_ms {
        return Err(PairingError::retryable(
            "ssh_pairing_code_expired",
            "the remote pairing code has expired",
        ));
    }
    Ok(payload)
}

/// Claims a code, registers the credential in the desktop runtime registry, and
/// returns the runtime id. The active runtime is intentionally not changed;
/// activation belongs to the Ready step.
pub async fn claim_and_register(
    command: &SshCommand,
    config: &LaunchConfig,
    home_dir: &Path,
    local_port: u16,
    allow_insecure_local_dev: bool,
    now_ms: i64,
) -> Result<PairingOutcome, PairingError> {
    let mut payload = fetch_fresh_pairing_code(command, config, now_ms)?;
    let local_endpoint = payload.local_endpoint(local_port)?;
    let mut credential = match claim_once(&payload, &local_endpoint, allow_insecure_local_dev).await
    {
        Ok(credential) => credential,
        Err(error) if error.code == "remote_pairing_code_expired" => {
            payload = fetch_fresh_pairing_code(command, config, now_ms)?;
            let local_endpoint = payload.local_endpoint(local_port)?;
            claim_once(&payload, &local_endpoint, allow_insecure_local_dev).await?
        }
        Err(error) => return Err(error),
    };
    if credential.expected_server_id != payload.server_id {
        return Err(PairingError::new(
            "ssh_pairing_identity_mismatch",
            "the pairing response does not match the advertised server identity",
        ));
    }
    // Keep the certificate that came over SSH, not a value the claim response
    // could have substituted.
    credential.pinned_tls_certificate_der = payload.tls_certificate_der.clone();
    credential.validate().map_err(PairingError::from_register)?;

    let store = DesktopRuntimeRegistryStore::new(home_dir);
    let mut registry = store.load_or_migrate();
    let runtime_id = registry.upsert(credential.clone(), now_ms);
    store.save(&registry).map_err(PairingError::from_register)?;
    Ok(PairingOutcome {
        runtime_id,
        credential,
    })
}

/// Mint once more if the first read was already expired. The caller still has
/// a bounded retry when the claim itself reports an expired code.
fn fetch_fresh_pairing_code(
    command: &SshCommand,
    config: &LaunchConfig,
    now_ms: i64,
) -> Result<PairingPayload, PairingError> {
    match fetch_pairing_code(command, config, now_ms) {
        Ok(payload) => Ok(payload),
        Err(error) if error.code == "ssh_pairing_code_expired" => {
            fetch_pairing_code(command, config, now_ms)
        }
        Err(error) => Err(error),
    }
}

async fn claim_once(
    payload: &PairingPayload,
    local_endpoint: &str,
    allow_insecure_local_dev: bool,
) -> Result<DesktopRemoteCredential, PairingError> {
    claim_server_pairing_code_with_certificate(
        local_endpoint.to_string(),
        payload.pairing_code.clone(),
        payload.tls_certificate_der.clone(),
        allow_insecure_local_dev,
    )
    .await
    .map_err(PairingError::from_claim)
}

pub const PAIRING_CODE_SCRIPT: &str = r#"set -eu
envfile=$1
binary=$2
set -a
. "$envfile"
set +a
"$binary" pairing-code --json
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ssh_connection::launch::LaunchConfig;
    use crate::ssh_connection::test_support;

    fn payload_json() -> String {
        r#"{
            "schemaVersion": "vibex-server-pairing.v1",
            "serverId": "server-0123456789abcdef",
            "serverVersion": "0.1.0-rc.8",
            "pairingCode": "123-456-789",
            "pairingLink": "vibex://pair#/code/secret",
            "tlsCertificateDer": "Y2VydGlmaWNhdGU",
            "endpoint": "https://127.0.0.1:8765",
            "expiresAtMs": 2000
        }"#
        .to_string()
    }

    fn config() -> LaunchConfig {
        LaunchConfig::new(
            "/home/dev",
            "/home/dev/.local/share/vibex/server",
            &["/home/dev".to_string()],
            8765,
        )
        .unwrap()
    }

    #[test]
    fn parses_and_rewrites_to_the_local_tunnel() {
        let payload = parse_pairing_payload(&payload_json(), 1000).unwrap();
        assert_eq!(payload.server_id, "server-0123456789abcdef");
        assert_eq!(
            payload.local_endpoint(18765).unwrap(),
            "https://127.0.0.1:18765"
        );
        let debug = format!("{payload:?}");
        assert!(!debug.contains("123-456-789"));
        assert!(!debug.contains("vibex://pair"));
        assert!(!debug.contains("Y2VydGlmaWNhdGU"));
    }

    #[test]
    fn rejects_invalid_schema_certificate_and_endpoint() {
        let wrong_schema = payload_json().replace("vibex-server-pairing.v1", "v2");
        assert_eq!(
            parse_pairing_payload(&wrong_schema, 1000).unwrap_err().code,
            "ssh_pairing_schema_invalid"
        );
        let no_cert = payload_json().replace("\"Y2VydGlmaWNhdGU\"", "null");
        assert_eq!(
            parse_pairing_payload(&no_cert, 1000).unwrap_err().code,
            "ssh_pairing_certificate_missing"
        );
        let http = payload_json().replace("https://127.0.0.1:8765", "http://127.0.0.1:8765");
        assert_eq!(
            parse_pairing_payload(&http, 1000).unwrap_err().code,
            "ssh_pairing_endpoint_insecure"
        );
    }

    #[test]
    fn expired_codes_are_retryable() {
        let error = parse_pairing_payload(&payload_json(), 2000).unwrap_err();
        assert_eq!(error.code, "ssh_pairing_code_expired");
        assert!(error.retryable);
    }

    #[test]
    fn local_port_must_be_nonzero() {
        let payload = parse_pairing_payload(&payload_json(), 1000).unwrap();
        assert_eq!(
            payload.local_endpoint(0).unwrap_err().code,
            "ssh_pairing_local_port_invalid"
        );
    }

    #[test]
    fn pairing_script_uses_the_managed_env_and_json_contract() {
        assert!(PAIRING_CODE_SCRIPT.contains("pairing-code --json"));
        assert!(PAIRING_CODE_SCRIPT.contains("set -a"));
        assert!(!PAIRING_CODE_SCRIPT.contains("pairingLink"));
    }

    #[cfg(unix)]
    #[test]
    fn fetches_pairing_code_through_the_fixed_script() {
        let dir = tempfile::tempdir().unwrap();
        let ssh = test_support::unique_path(dir.path(), "fake-ssh");
        test_support::write_executable(
            &ssh,
            &format!(
                "#!/bin/sh\ncat >/dev/null\ncat <<'OUT'\n{}\nOUT\n",
                payload_json()
            ),
        );
        let command = SshCommand {
            program: ssh,
            args: Vec::new(),
            stdin: None,
            remote_arg_start: None,
        };
        let payload = fetch_pairing_code(&command, &config(), 1000).unwrap();
        assert_eq!(payload.endpoint, "https://127.0.0.1:8765");
    }
}

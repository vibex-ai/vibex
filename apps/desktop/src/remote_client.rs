//! Desktop remote-client mode: connect the workbench UI to an authoritative
//! runtime (a headless `vibex-server` on a cloud host, or another desktop)
//! over Remote v2 instead of driving the local `DesktopRuntime`.
//!
//! The mode reuses the exact client stack the phone uses —
//! `claim_pairing_code_with_identity`, `RemoteCredentialRecord`,
//! `AutoRemoteTransport`, and `WebRemoteBackend` — so a paired desktop and a
//! paired phone hold equivalent grants and speak identical wire contracts.
//! Credentials persist under the desktop home with restrictive permissions.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use vibex_backend::{BackendError, BackendResult};
use vibex_core::RemoteClientType;
use vibex_remote_client::{
    AutoRemoteTransport, AutoRemoteTransportConfig, ClientDeviceIdentity, DirectCandidate,
    RemoteClientConfig, RemoteCredentialRecord, WebRemoteBackend,
};

pub const DESKTOP_CREDENTIAL_SCHEMA_VERSION: &str = "vibex-native-desktop-credentials.v1";
const DESKTOP_CREDENTIALS_FILE: &str = "remote-client-credentials.json";
pub const DESKTOP_CLIENT_ID: &str = "vibex-desktop";

/// One paired remote runtime. The record pins the server identity so a
/// credential saved for one runtime is never replayed against another.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopRemoteCredential {
    pub schema_version: String,
    pub record: RemoteCredentialRecord,
    pub identity_private_key: String,
    pub expected_server_id: String,
    #[serde(default)]
    pub allow_insecure_local_dev: bool,
    #[serde(default)]
    pub display_name: Option<String>,
}

impl std::fmt::Debug for DesktopRemoteCredential {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DesktopRemoteCredential")
            .field("schema_version", &self.schema_version)
            .field("has_auth_token", &!self.record.auth.auth_token.is_empty())
            .field(
                "has_device_identity_public_key",
                &!self.record.device_identity_public_key.is_empty(),
            )
            .field(
                "has_server_identity_public_key",
                &self.record.server_identity_public_key.is_some(),
            )
            .field(
                "has_expected_server_id",
                &!self.expected_server_id.is_empty(),
            )
            .finish()
    }
}

impl DesktopRemoteCredential {
    fn from_parts(
        bundle: vibex_remote_client::PairingCodeClientBundle,
        allow_insecure_local_dev: bool,
    ) -> BackendResult<Self> {
        let credential = Self {
            schema_version: DESKTOP_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: bundle.credential,
            identity_private_key: bundle.identity.private_key_base64(),
            expected_server_id: bundle.server_id,
            allow_insecure_local_dev,
            display_name: None,
        };
        credential.validate()?;
        Ok(credential)
    }

    pub fn validate(&self) -> BackendResult<()> {
        if self.schema_version != DESKTOP_CREDENTIAL_SCHEMA_VERSION
            || self.expected_server_id.trim().is_empty()
            || self.identity_private_key.trim().is_empty()
            || self.record.auth.auth_token.trim().is_empty()
            || self.record.device_identity_public_key.trim().is_empty()
            || self.record.server_identity_public_key.is_none()
        {
            return Err(BackendError::failed(
                "desktop_remote_credentials_invalid",
                "desktop remote credentials failed identity or version validation",
            ));
        }
        let identity = ClientDeviceIdentity::from_private_key_base64(
            self.record.auth.device_id.clone(),
            &self.identity_private_key,
        )?;
        if identity.public_key_base64() != self.record.device_identity_public_key {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "stored client identity does not match the remote device grant",
            ));
        }
        self.client_config()?;
        Ok(())
    }

    /// Builds the transport configuration the same way the phone does: the
    /// saved server URL is the single direct candidate. The development-only
    /// loopback exception is set before validation so a debug client can pair
    /// with a plain-HTTP `vibex-server serve` instance.
    pub fn client_config(&self) -> BackendResult<RemoteClientConfig> {
        let identity = ClientDeviceIdentity::from_private_key_base64(
            self.record.auth.device_id.clone(),
            &self.identity_private_key,
        )?;
        if identity.device_id() != &self.record.auth.device_id
            || identity.public_key_base64() != self.record.device_identity_public_key
        {
            return Err(BackendError::permission(
                "remote_client_identity_mismatch",
                "stored client identity does not match the remote device grant",
            ));
        }
        let mut config =
            RemoteClientConfig::new(self.record.server_url.clone(), self.record.auth.clone())
                .with_device_identity(identity);
        config.expected_server_id = Some(self.expected_server_id.clone());
        config.expected_server_identity_public_key = self.record.server_identity_public_key.clone();
        config.client_id = DESKTOP_CLIENT_ID.to_string();
        config.client_type = RemoteClientType::DesktopWeb;
        config.allow_insecure_local_dev = self.allow_insecure_local_dev && cfg!(debug_assertions);
        config.validate()?;
        Ok(config)
    }

    pub fn backend(&self) -> BackendResult<Arc<WebRemoteBackend>> {
        let config = self.client_config()?;
        let transport = AutoRemoteTransport::new(AutoRemoteTransportConfig {
            remote: config,
            direct_candidates: vec![DirectCandidate {
                url: self.record.server_url.clone(),
                label: "remote-runtime".to_string(),
                priority: 0,
                tls_certificate_der: None,
            }],
            relay: None,
        })?;
        Ok(Arc::new(WebRemoteBackend::from_auto(transport)))
    }
}

/// Pair with a headless runtime using the address and the one-time numeric
/// code its operator printed. The code travels only inside the bounded HTTPS
/// claim body and the returned credential pins the server identity before it
/// is ever stored.
pub async fn claim_server_pairing_code(
    server_url: String,
    pairing_code: String,
    allow_insecure_local_dev: bool,
) -> BackendResult<DesktopRemoteCredential> {
    let server_url = server_url.trim().trim_end_matches('/').to_string();
    let bundle = vibex_remote_client::claim_pairing_code_with_identity(
        server_url,
        pairing_code,
        "Vibex Desktop".to_string(),
        allow_insecure_local_dev,
    )
    .await?;
    DesktopRemoteCredential::from_parts(bundle, allow_insecure_local_dev)
}

/// Restrictive-permission credential file under the desktop home. Reads and
/// writes are atomic; malformed or mismatched records are discarded instead
/// of being trusted.
pub struct DesktopRemoteCredentialStore {
    path: PathBuf,
}

impl DesktopRemoteCredentialStore {
    pub fn new(home_dir: &Path) -> Self {
        Self {
            path: home_dir.join(DESKTOP_CREDENTIALS_FILE),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load(&self) -> Option<DesktopRemoteCredential> {
        let bytes = std::fs::read(&self.path).ok()?;
        match serde_json::from_slice::<DesktopRemoteCredential>(&bytes) {
            Ok(credential) => credential.validate().ok().map(|()| credential),
            Err(_) => None,
        }
    }

    pub fn save(&self, credential: &DesktopRemoteCredential) -> BackendResult<()> {
        credential.validate()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                BackendError::failed(
                    "desktop_remote_credentials_unwritable",
                    "the desktop home could not be created for remote credentials",
                )
            })?;
        }
        let bytes = serde_json::to_vec_pretty(credential).map_err(|_| {
            BackendError::failed(
                "desktop_remote_credentials_encode_failed",
                "remote credentials could not be serialized",
            )
        })?;
        write_private(&self.path, &bytes)?;
        Ok(())
    }

    pub fn clear(&self) -> BackendResult<()> {
        match std::fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(BackendError::failed(
                "desktop_remote_credentials_unwritable",
                "the stored remote credential could not be removed",
            )),
        }
    }
}

/// Atomic write with owner-only permissions where the platform supports it,
/// matching the native mobile credential contract.
fn write_private(path: &Path, bytes: &[u8]) -> BackendResult<()> {
    use std::io::Write;
    let temp_path = path.with_extension("tmp");
    {
        let mut file = std::fs::File::create(&temp_path).map_err(|_| {
            BackendError::failed(
                "desktop_remote_credentials_unwritable",
                "the credential file could not be created",
            )
        })?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o600))
                .map_err(|_| {
                    BackendError::failed(
                        "desktop_remote_credentials_unwritable",
                        "the credential file permissions could not be restricted",
                    )
                })?;
        }
        file.write_all(bytes).map_err(|_| {
            BackendError::failed(
                "desktop_remote_credentials_unwritable",
                "the credential file could not be written",
            )
        })?;
        file.sync_all().map_err(|_| {
            BackendError::failed(
                "desktop_remote_credentials_unwritable",
                "the credential file could not be flushed",
            )
        })?;
    }
    std::fs::rename(&temp_path, path).map_err(|_| {
        BackendError::failed(
            "desktop_remote_credentials_unwritable",
            "the credential file could not be moved into place",
        )
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample_record() -> (DesktopRemoteCredential, ClientDeviceIdentity) {
        let identity =
            ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).expect("identity");
        let credential = DesktopRemoteCredential {
            schema_version: DESKTOP_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: RemoteCredentialRecord {
                server_url: "https://vibex.example.com".to_string(),
                auth: vibex_core::RemoteAuthProof {
                    device_id: vibex_core::DeviceId::new(),
                    auth_token: "auth-request_test-token".to_string(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some("server-key".to_string()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: "server-test".to_string(),
            allow_insecure_local_dev: false,
            display_name: None,
        };
        (credential, identity)
    }

    #[test]
    fn credential_validate_accepts_wellformed_records() {
        let (credential, _) = sample_record();
        credential.validate().expect("well-formed credential");
    }

    #[test]
    fn credential_validate_rejects_mismatched_identity_key() {
        let (mut credential, _) = sample_record();
        let (other, _) = sample_record();
        // The stored public key must match the private key's derivation.
        credential.record.device_identity_public_key = other.record.device_identity_public_key;
        let error = credential.validate().unwrap_err();
        assert_eq!(error.code, "remote_client_identity_mismatch");
    }

    #[test]
    fn credential_validate_rejects_unknown_schema() {
        let (mut credential, _) = sample_record();
        credential.schema_version = "other".to_string();
        let error = credential.validate().unwrap_err();
        assert_eq!(error.code, "desktop_remote_credentials_invalid");
    }

    #[test]
    fn debug_output_never_exposes_the_auth_token() {
        let (credential, _) = sample_record();
        let debug = format!("{credential:?}");
        assert!(!debug.contains(&credential.record.auth.auth_token));
        assert!(!debug.contains(&credential.identity_private_key));
    }

    #[test]
    fn store_round_trips_and_discards_corrupt_records() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DesktopRemoteCredentialStore::new(dir.path());
        assert!(store.load().is_none());

        let (credential, _) = sample_record();
        store.save(&credential).expect("save");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(store.path())
                .expect("metadata")
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        let loaded = store.load().expect("loaded");
        assert_eq!(loaded, credential);

        std::fs::write(store.path(), b"{not json").expect("corrupt");
        assert!(store.load().is_none(), "corrupt records are discarded");

        store.clear().expect("clear");
        assert!(store.load().is_none());
        store.clear().expect("clear is idempotent");
    }

    #[test]
    fn client_config_pins_the_saved_server() {
        let (credential, _) = sample_record();
        let config = credential.client_config().expect("config");
        assert_eq!(config.base_url, "https://vibex.example.com");
        assert_eq!(config.client_id, DESKTOP_CLIENT_ID);
        assert_eq!(config.client_type, RemoteClientType::DesktopWeb);
        assert_eq!(config.expected_server_id, Some("server-test".to_string()));
        assert!(
            !config.allow_insecure_local_dev,
            "release builds never allow plain HTTP"
        );
    }
}

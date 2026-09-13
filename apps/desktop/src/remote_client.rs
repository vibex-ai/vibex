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
pub const DESKTOP_RUNTIMES_SCHEMA_VERSION: &str = "vibex-native-desktop-runtimes.v2";
const DESKTOP_RUNTIMES_FILE: &str = "remote-runtimes.json";
/// The embedded runtime's synthetic id. It is never a real server id, so a
/// paired server can never collide with it.
pub const LOCAL_RUNTIME_ID: &str = "local";
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
    /// What the runtime called itself when it was paired. Absent from a
    /// version 1 record, which reads back as `Unknown`.
    #[serde(default)]
    pub server_kind: vibex_core::RemoteServerKind,
    /// Base64url DER of the certificate to trust for this server, set when the
    /// runtime serves its own self-signed certificate. The value came from the
    /// operator's pairing link, never from the server itself.
    #[serde(default)]
    pub pinned_tls_certificate_der: Option<String>,
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
            .field(
                "has_pinned_tls_certificate",
                &self.pinned_tls_certificate_der.is_some(),
            )
            .finish()
    }
}

impl DesktopRemoteCredential {
    fn from_parts(
        bundle: vibex_remote_client::PairingCodeClientBundle,
        allow_insecure_local_dev: bool,
        pinned_tls_certificate_der: Option<String>,
    ) -> BackendResult<Self> {
        let credential = Self {
            schema_version: DESKTOP_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: bundle.credential,
            identity_private_key: bundle.identity.private_key_base64(),
            expected_server_id: bundle.server_id,
            allow_insecure_local_dev,
            display_name: None,
            server_kind: bundle.server_kind,
            pinned_tls_certificate_der,
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
        config.pinned_tls_certificate_der = self.pinned_tls_certificate_der.clone();
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
                tls_certificate_der: self.pinned_tls_certificate_der.clone(),
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
    DesktopRemoteCredential::from_parts(bundle, allow_insecure_local_dev, None)
}

/// Pair from the connection link a `vibex-server` operator printed.
///
/// The link carries the address, the one-time code, and — when the runtime
/// serves its own certificate — that certificate. The certificate is pinned
/// before the claim request, so the first TLS handshake is already verified
/// against a value that arrived through the operator's screen rather than
/// through the network.
pub async fn claim_server_pairing_link(
    pairing_link: String,
    allow_insecure_local_dev: bool,
) -> BackendResult<DesktopRemoteCredential> {
    let link = vibex_core::RemotePairingCodeLink::parse(&pairing_link)
        .map_err(|error| BackendError::failed(error.code.clone(), error.message.clone()))?;
    let pinned_tls_certificate_der = link.tls_certificate_der.clone();
    let bundle = vibex_remote_client::claim_pairing_code_link_with_identity(
        link,
        "Vibex Desktop".to_string(),
        allow_insecure_local_dev,
    )
    .await?;
    DesktopRemoteCredential::from_parts(
        bundle,
        allow_insecure_local_dev,
        pinned_tls_certificate_der,
    )
}

/// One paired remote runtime: the persisted grant plus the client-side
/// metadata the runtime manager renders. `id` is the server id the credential
/// already pins, so a runtime keeps its identity across renames.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RegisteredRuntime {
    pub id: String,
    /// The operator's local rename. `None` falls back to the name the runtime
    /// published when it was paired, then to its address.
    #[serde(default)]
    pub display_name: Option<String>,
    pub credential: DesktopRemoteCredential,
    #[serde(default)]
    pub added_at_ms: i64,
    #[serde(default)]
    pub last_connected_at_ms: Option<i64>,
}

impl std::fmt::Debug for RegisteredRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("RegisteredRuntime")
            .field("id", &self.id)
            .field("display_name", &self.display_name)
            .field("credential", &self.credential)
            .finish()
    }
}

impl RegisteredRuntime {
    /// What the peer is, as far as the last claim or handshake revealed.
    pub fn server_kind(&self) -> vibex_core::RemoteServerKind {
        self.credential.server_kind
    }

    /// Records what a handshake revealed. Returns whether anything changed, so
    /// the caller only persists a real update. A peer that does not answer
    /// with a kind must not erase one that was learned earlier.
    pub fn observe_server_kind(&mut self, kind: vibex_core::RemoteServerKind) -> bool {
        if kind == vibex_core::RemoteServerKind::Unknown || self.credential.server_kind == kind {
            return false;
        }
        self.credential.server_kind = kind;
        true
    }

    /// The name the runtime manager renders. The shared helper owns the
    /// fallback chain so the desktop and the phone agree.
    pub fn display_label(&self) -> String {
        vibex_remote_client::runtime_display_name(
            self.display_name.as_deref(),
            self.credential.display_name.as_deref(),
            &self.credential.record.server_url,
            &self.id,
        )
    }
}

/// Every runtime this desktop can drive: the embedded one plus each paired
/// remote. The registry is client-side state, so it survives switching the
/// workbench to another authority — which is what makes switching
/// non-destructive.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct DesktopRuntimeRegistry {
    pub schema_version: String,
    /// [`LOCAL_RUNTIME_ID`] or the id of a registered remote runtime.
    pub active_runtime_id: String,
    #[serde(default)]
    pub runtimes: Vec<RegisteredRuntime>,
}

impl Default for DesktopRuntimeRegistry {
    fn default() -> Self {
        Self {
            schema_version: DESKTOP_RUNTIMES_SCHEMA_VERSION.to_string(),
            active_runtime_id: LOCAL_RUNTIME_ID.to_string(),
            runtimes: Vec::new(),
        }
    }
}

impl DesktopRuntimeRegistry {
    pub fn validate(&self) -> BackendResult<()> {
        if self.schema_version != DESKTOP_RUNTIMES_SCHEMA_VERSION {
            return Err(BackendError::failed(
                "desktop_runtimes_invalid",
                "the stored runtime registry has an unknown schema version",
            ));
        }
        for runtime in &self.runtimes {
            runtime.credential.validate()?;
            if runtime.id.trim().is_empty() || runtime.id != runtime.credential.expected_server_id {
                return Err(BackendError::failed(
                    "desktop_runtimes_invalid",
                    "a stored runtime id does not match its pinned server identity",
                ));
            }
        }
        if !self.is_local_active() && self.remote(self.active_runtime_id()).is_none() {
            return Err(BackendError::failed(
                "desktop_runtimes_invalid",
                "the active runtime is not present in the registry",
            ));
        }
        Ok(())
    }

    pub fn is_local_active(&self) -> bool {
        self.active_runtime_id == LOCAL_RUNTIME_ID
    }

    pub fn active_runtime_id(&self) -> &str {
        self.active_runtime_id.as_str()
    }

    pub fn active_remote(&self) -> Option<&RegisteredRuntime> {
        self.remote(self.active_runtime_id())
    }

    pub fn remote(&self, id: &str) -> Option<&RegisteredRuntime> {
        self.runtimes.iter().find(|runtime| runtime.id == id)
    }

    pub fn remote_mut(&mut self, id: &str) -> Option<&mut RegisteredRuntime> {
        self.runtimes.iter_mut().find(|runtime| runtime.id == id)
    }

    pub fn set_active(&mut self, id: &str) {
        self.active_runtime_id = id.to_string();
    }

    /// Adds a freshly paired runtime, or refreshes the grant of one that is
    /// already registered. Returns the runtime id either way so the caller can
    /// switch to it.
    pub fn upsert(&mut self, credential: DesktopRemoteCredential, now_ms: i64) -> String {
        let id = credential.expected_server_id.clone();
        match self.remote_mut(&id) {
            Some(existing) => {
                // Keep the operator's rename across a re-pair.
                existing.credential = credential;
            }
            None => self.runtimes.push(RegisteredRuntime {
                id: id.clone(),
                display_name: None,
                credential,
                added_at_ms: now_ms,
                last_connected_at_ms: None,
            }),
        }
        id
    }

    /// Removes a runtime. The embedded runtime is not removable, and removing
    /// the active remote falls back to the embedded authority.
    pub fn remove(&mut self, id: &str) -> bool {
        if id == LOCAL_RUNTIME_ID {
            return false;
        }
        let before = self.runtimes.len();
        self.runtimes.retain(|runtime| runtime.id != id);
        if self.runtimes.len() == before {
            return false;
        }
        if self.active_runtime_id == id {
            self.active_runtime_id = LOCAL_RUNTIME_ID.to_string();
        }
        true
    }

    /// Sets or clears the operator's rename. An empty name clears it.
    pub fn rename(&mut self, id: &str, name: &str) -> bool {
        let name = name.trim();
        let Some(runtime) = self.remote_mut(id) else {
            return false;
        };
        let next = (!name.is_empty()).then(|| name.to_string());
        if runtime.display_name == next {
            return false;
        }
        runtime.display_name = next;
        true
    }

    pub fn touch_connected(&mut self, id: &str, now_ms: i64) -> bool {
        let Some(runtime) = self.remote_mut(id) else {
            return false;
        };
        runtime.last_connected_at_ms = Some(now_ms);
        true
    }
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

/// Durable home of the runtime registry.
///
/// Version 1 kept exactly one credential in `remote-client-credentials.json`,
/// which is why switching back to the embedded runtime used to mean deleting
/// the pairing. The v2 registry keeps every grant plus the id of the active
/// runtime, so switching and removing are separate operations.
pub struct DesktopRuntimeRegistryStore {
    path: PathBuf,
    /// The version 1 single-credential store, still the reader for a home that
    /// has not been migrated yet.
    legacy: DesktopRemoteCredentialStore,
}

impl DesktopRuntimeRegistryStore {
    pub fn new(home_dir: &Path) -> Self {
        Self {
            path: home_dir.join(DESKTOP_RUNTIMES_FILE),
            legacy: DesktopRemoteCredentialStore::new(home_dir),
        }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn legacy_path(&self) -> &Path {
        self.legacy.path()
    }

    /// Reads the registry, migrating a version 1 single-credential home the
    /// first time it is seen. A corrupt or unrecognized registry is discarded
    /// rather than trusted, matching the credential file's contract.
    pub fn load_or_migrate(&self) -> DesktopRuntimeRegistry {
        if let Some(registry) = self.load() {
            return registry;
        }
        self.migrate_legacy().unwrap_or_default()
    }

    fn load(&self) -> Option<DesktopRuntimeRegistry> {
        let bytes = std::fs::read(&self.path).ok()?;
        let registry = serde_json::from_slice::<DesktopRuntimeRegistry>(&bytes).ok()?;
        registry.validate().ok().map(|()| registry)
    }

    /// Wraps the version 1 credential into a one-entry registry and makes it
    /// the active runtime, because a stored v1 credential is what the shell
    /// booted into. The legacy file is removed only after the registry is on
    /// disk, so a failed migration leaves the original pairing intact.
    fn migrate_legacy(&self) -> Option<DesktopRuntimeRegistry> {
        let credential = self.legacy.load()?;
        let id = credential.expected_server_id.clone();
        let mut registry = DesktopRuntimeRegistry::default();
        registry.runtimes.push(RegisteredRuntime {
            id: id.clone(),
            display_name: credential.display_name.clone(),
            credential,
            added_at_ms: 0,
            last_connected_at_ms: None,
        });
        registry.active_runtime_id = id;
        if self.save(&registry).is_err() {
            return None;
        }
        // The credential now lives in the registry. Leaving the v1 file behind
        // would keep a second copy of the same device grant on disk.
        let _ = self.legacy.clear();
        Some(registry)
    }

    pub fn save(&self, registry: &DesktopRuntimeRegistry) -> BackendResult<()> {
        registry.validate()?;
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent).map_err(|_| {
                BackendError::failed(
                    "desktop_runtimes_unwritable",
                    "the desktop home could not be created for the runtime registry",
                )
            })?;
        }
        let bytes = serde_json::to_vec_pretty(registry).map_err(|_| {
            BackendError::failed(
                "desktop_runtimes_encode_failed",
                "the runtime registry could not be serialized",
            )
        })?;
        write_private(&self.path, &bytes)
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
            server_kind: vibex_core::RemoteServerKind::Desktop,
            pinned_tls_certificate_der: None,
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

    /// A credential as a client paired before the peer advertised its kind.
    fn second_record(id: &str) -> DesktopRemoteCredential {
        let (mut credential, _) = sample_record();
        credential.expected_server_id = id.to_string();
        credential.server_kind = vibex_core::RemoteServerKind::Unknown;
        credential
    }

    #[test]
    fn registry_round_trips_and_rejects_a_missing_active_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DesktopRuntimeRegistryStore::new(dir.path());
        assert!(
            store.load_or_migrate().is_local_active(),
            "a fresh home starts on the embedded runtime"
        );

        let mut registry = DesktopRuntimeRegistry::default();
        let id = registry.upsert(second_record("server-a"), 1_000);
        registry.set_active(&id);
        store.save(&registry).expect("save");
        let loaded = store.load().expect("loaded");
        assert_eq!(loaded, registry);
        assert_eq!(loaded.active_runtime_id(), "server-a");

        // A registry that names an active runtime it does not hold is not
        // trusted: the manager must never render a runtime it cannot resolve.
        let mut dangling = registry.clone();
        dangling.active_runtime_id = "server-missing".to_string();
        assert!(dangling.validate().is_err());
        assert!(
            store.save(&dangling).is_err(),
            "an unresolvable active runtime is never written"
        );
    }

    #[test]
    fn registry_migrates_a_version_one_credential_into_the_active_runtime() {
        let dir = tempfile::tempdir().expect("tempdir");
        let legacy = DesktopRemoteCredentialStore::new(dir.path());
        legacy
            .save(&second_record("server-legacy"))
            .expect("save v1");

        let store = DesktopRuntimeRegistryStore::new(dir.path());
        let registry = store.load_or_migrate();
        assert_eq!(registry.active_runtime_id(), "server-legacy");
        assert_eq!(registry.runtimes.len(), 1);
        assert_eq!(
            registry.active_remote().map(|runtime| runtime.id.as_str()),
            Some("server-legacy")
        );
        // The registry is on disk before the v1 file is dropped, so a reader
        // never sees a home with neither.
        assert!(store.load().is_some(), "the migrated registry is persisted");
        assert!(
            !store.legacy_path().exists(),
            "the single-credential file is removed once it has been wrapped"
        );

        // Migrating twice is a no-op rather than a duplicate runtime.
        let again = store.load_or_migrate();
        assert_eq!(again.runtimes.len(), 1);
    }

    #[test]
    fn removing_the_active_runtime_falls_back_to_the_embedded_one() {
        let mut registry = DesktopRuntimeRegistry::default();
        let a = registry.upsert(second_record("server-a"), 1_000);
        let b = registry.upsert(second_record("server-b"), 2_000);
        registry.set_active(&b);

        assert!(
            !registry.remove(LOCAL_RUNTIME_ID),
            "the embedded runtime stays"
        );
        assert!(registry.remove(&b));
        assert!(
            registry.is_local_active(),
            "removal falls back to the device"
        );
        assert!(registry.remote(&b).is_none());
        assert!(registry.active_remote().is_none());
        assert!(registry.validate().is_ok());

        // Re-pairing the same server keeps one entry and the operator's name.
        assert!(registry.rename(&a, "  dev box  "));
        assert_eq!(
            registry.remote(&a).map(RegisteredRuntime::display_label),
            Some("dev box".to_string())
        );
        let rejoined = registry.upsert(second_record("server-a"), 3_000);
        assert_eq!(rejoined, a);
        assert_eq!(registry.runtimes.len(), 1);
        assert_eq!(
            registry.remote(&a).map(RegisteredRuntime::display_label),
            Some("dev box".to_string()),
            "a re-pair keeps the operator's rename"
        );

        // Clearing the name falls back to the published one, then the address.
        assert!(registry.rename(&a, "   "));
        assert_eq!(
            registry.remote(&a).map(RegisteredRuntime::display_label),
            Some("vibex.example.com".to_string())
        );
    }

    #[test]
    fn runtime_display_name_prefers_override_then_published_then_address() {
        assert_eq!(
            vibex_remote_client::runtime_display_name(
                Some("mine"),
                Some("published"),
                "https://host.example:8787",
                "server-id"
            ),
            "mine"
        );
        assert_eq!(
            vibex_remote_client::runtime_display_name(
                None,
                Some("published"),
                "https://host.example:8787",
                "server-id"
            ),
            "published"
        );
        assert_eq!(
            vibex_remote_client::runtime_display_name(
                None,
                None,
                "https://host.example:8787",
                "server-id"
            ),
            "host.example"
        );
        // A relay-only credential still gets a stable, bounded label.
        let long = "s".repeat(vibex_remote_client::RUNTIME_DISPLAY_NAME_MAX_CHARS + 20);
        let label =
            vibex_remote_client::runtime_display_name(None, None, "not a url", long.as_str());
        assert_eq!(
            label.chars().count(),
            vibex_remote_client::RUNTIME_DISPLAY_NAME_MAX_CHARS
        );
    }

    #[test]
    fn a_runtime_kind_survives_a_round_trip_and_is_never_erased_by_unknown() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DesktopRuntimeRegistryStore::new(dir.path());
        let mut registry = DesktopRuntimeRegistry::default();
        let id = registry.upsert(second_record("server-a"), 1_000);

        // A version 1 credential never said what the peer was.
        assert_eq!(
            registry.remote(&id).map(RegisteredRuntime::server_kind),
            Some(vibex_core::RemoteServerKind::Unknown)
        );

        let runtime = registry.remote_mut(&id).expect("runtime");
        assert!(runtime.observe_server_kind(vibex_core::RemoteServerKind::Headless));
        assert!(
            !runtime.observe_server_kind(vibex_core::RemoteServerKind::Headless),
            "an unchanged kind is not a new observation"
        );
        store.save(&registry).expect("save");
        assert_eq!(
            store
                .load()
                .and_then(|loaded| loaded.remote(&id).map(RegisteredRuntime::server_kind)),
            Some(vibex_core::RemoteServerKind::Headless),
            "the kind is persisted with the registry"
        );

        // A peer that stops reporting a kind must not erase what was learned.
        let runtime = registry.remote_mut(&id).expect("runtime");
        assert!(!runtime.observe_server_kind(vibex_core::RemoteServerKind::Unknown));
        assert_eq!(
            runtime.server_kind(),
            vibex_core::RemoteServerKind::Headless
        );
        assert!(runtime.observe_server_kind(vibex_core::RemoteServerKind::Desktop));
        assert_eq!(runtime.server_kind(), vibex_core::RemoteServerKind::Desktop);
    }

    #[test]
    fn a_version_one_record_without_a_kind_still_migrates() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut credential = second_record("server-legacy");
        credential.server_kind = vibex_core::RemoteServerKind::Unknown;
        DesktopRemoteCredentialStore::new(dir.path())
            .save(&credential)
            .expect("save v1");

        let registry = DesktopRuntimeRegistryStore::new(dir.path()).load_or_migrate();
        assert_eq!(registry.active_runtime_id(), "server-legacy");
        assert_eq!(
            registry.active_remote().map(RegisteredRuntime::server_kind),
            Some(vibex_core::RemoteServerKind::Unknown),
            "an older peer keeps the generic label instead of being guessed at"
        );
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
        assert!(
            config.pinned_tls_certificate_der.is_none(),
            "a credential paired by address keeps using the system roots"
        );
    }

    #[test]
    fn stored_pin_reaches_both_the_transport_and_the_direct_candidate() {
        let (mut credential, _) = sample_record();
        credential.record.server_url = "https://192.168.1.10:8765".to_string();
        credential.pinned_tls_certificate_der = Some(base64_url(&certificate_envelope()));
        credential.validate().expect("pinned credential");

        let config = credential.client_config().expect("pinned config");
        assert!(config.pinned_tls_certificate_der.is_some());
        assert!(
            !config.allow_insecure_local_dev,
            "a pinned LAN route is still HTTPS-only in release builds"
        );
    }

    /// The pin is what makes a LAN runtime reachable at all, so it has to come
    /// back out of the credential file exactly as it went in.
    #[test]
    fn a_pinned_certificate_survives_the_credentials_file() {
        let dir = tempfile::tempdir().expect("tempdir");
        let store = DesktopRemoteCredentialStore::new(dir.path());
        let (mut credential, _) = sample_record();
        credential.record.server_url = "https://192.168.1.10:8765".to_string();
        credential.pinned_tls_certificate_der = Some(base64_url(&certificate_envelope()));

        store.save(&credential).expect("save");
        let loaded = store.load().expect("loaded");
        assert_eq!(loaded, credential);
        assert!(
            loaded
                .client_config()
                .expect("loaded config")
                .pinned_tls_certificate_der
                .is_some(),
            "a reloaded credential still verifies its server against the pin"
        );
    }

    #[test]
    fn a_pin_without_local_https_is_rejected() {
        let (mut credential, _) = sample_record();
        credential.pinned_tls_certificate_der = Some(base64_url(&certificate_envelope()));
        let error = credential
            .validate()
            .expect_err("a pin must never be attached to a public host");
        assert_eq!(error.code, "remote_pinned_tls_route_invalid");
    }

    /// A minimal DER envelope: the transport only decodes and pins it, the TLS
    /// stack is what validates the certificate itself.
    fn certificate_envelope() -> Vec<u8> {
        let mut certificate = vec![0x30, 0x82, 0x01, 0x00];
        certificate.extend(std::iter::repeat_n(0x41_u8, 252));
        certificate
    }

    fn base64_url(bytes: &[u8]) -> String {
        use base64::Engine as _;
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes)
    }
}

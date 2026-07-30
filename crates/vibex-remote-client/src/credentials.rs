use std::fmt;
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use serde::{Deserialize, Serialize};
use vibex_backend::{BackendBound, BackendError, BackendFuture, BackendResult};
use vibex_core::{DeviceId, RemoteAuthProof};
use x25519_dalek::{PublicKey, StaticSecret};

/// Durable client metadata.  Session keys and ephemeral WebSocket keys are
/// intentionally absent; they only live in the transport connection.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RemoteCredentialRecord {
    pub server_url: String,
    pub auth: RemoteAuthProof,
    pub device_identity_public_key: String,
    pub server_identity_public_key: Option<String>,
}

impl fmt::Debug for RemoteCredentialRecord {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteCredentialRecord")
            .field("server_url", &self.server_url)
            .field("auth", &self.auth)
            .field(
                "device_identity_public_key",
                &self.device_identity_public_key,
            )
            .field(
                "has_server_identity_public_key",
                &self.server_identity_public_key.is_some(),
            )
            .finish()
    }
}

pub trait CredentialStore: BackendBound {
    fn load(&self) -> BackendFuture<'_, Option<RemoteCredentialRecord>>;
    fn save(&self, record: RemoteCredentialRecord) -> BackendFuture<'_, ()>;
    fn clear(&self) -> BackendFuture<'_, ()>;
}

/// Durable store for the paired device's long-lived identity.  It is separate
/// from [`CredentialStore`] so a Capacitor host can keep the private key in a
/// secure-storage plugin while browser builds use a scoped Web Storage key.
/// Ephemeral connection/session keys never cross this boundary.
pub trait ClientIdentityStore: BackendBound {
    fn load(&self) -> BackendFuture<'_, Option<ClientDeviceIdentity>>;
    fn save(&self, identity: ClientDeviceIdentity) -> BackendFuture<'_, ()>;
    fn clear(&self) -> BackendFuture<'_, ()>;
}

#[derive(Clone, Default)]
pub struct MemoryCredentialStore {
    record: Arc<Mutex<Option<RemoteCredentialRecord>>>,
}

impl MemoryCredentialStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl CredentialStore for MemoryCredentialStore {
    fn load(&self) -> BackendFuture<'_, Option<RemoteCredentialRecord>> {
        let record = self.record.clone();
        Box::pin(async move {
            record
                .lock()
                .map_err(|_| {
                    BackendError::failed(
                        "credential_store_poisoned",
                        "credential storage is unavailable",
                    )
                })
                .map(|record| record.clone())
        })
    }

    fn save(&self, value: RemoteCredentialRecord) -> BackendFuture<'_, ()> {
        let record = self.record.clone();
        Box::pin(async move {
            *record.lock().map_err(|_| {
                BackendError::failed(
                    "credential_store_poisoned",
                    "credential storage is unavailable",
                )
            })? = Some(value);
            Ok(())
        })
    }

    fn clear(&self) -> BackendFuture<'_, ()> {
        let record = self.record.clone();
        Box::pin(async move {
            *record.lock().map_err(|_| {
                BackendError::failed(
                    "credential_store_poisoned",
                    "credential storage is unavailable",
                )
            })? = None;
            Ok(())
        })
    }
}

#[derive(Clone, Default)]
pub struct MemoryClientIdentityStore {
    identity: Arc<Mutex<Option<ClientDeviceIdentity>>>,
}

impl MemoryClientIdentityStore {
    pub fn new() -> Self {
        Self::default()
    }
}

impl ClientIdentityStore for MemoryClientIdentityStore {
    fn load(&self) -> BackendFuture<'_, Option<ClientDeviceIdentity>> {
        let identity = self.identity.clone();
        Box::pin(async move {
            identity
                .lock()
                .map_err(|_| {
                    BackendError::failed(
                        "client_identity_store_poisoned",
                        "client identity storage is unavailable",
                    )
                })
                .map(|identity| identity.clone())
        })
    }

    fn save(&self, value: ClientDeviceIdentity) -> BackendFuture<'_, ()> {
        let identity = self.identity.clone();
        Box::pin(async move {
            *identity.lock().map_err(|_| {
                BackendError::failed(
                    "client_identity_store_poisoned",
                    "client identity storage is unavailable",
                )
            })? = Some(value);
            Ok(())
        })
    }

    fn clear(&self) -> BackendFuture<'_, ()> {
        let identity = self.identity.clone();
        Box::pin(async move {
            *identity.lock().map_err(|_| {
                BackendError::failed(
                    "client_identity_store_poisoned",
                    "client identity storage is unavailable",
                )
            })? = None;
            Ok(())
        })
    }
}

/// Browser Web Storage implementation.  Capacitor can provide the same trait
/// with a Secure Storage bridge without changing `WebRemoteBackend`.
#[cfg(target_family = "wasm")]
#[derive(Clone)]
pub struct WebStorageCredentialStore {
    key: String,
}

#[cfg(target_family = "wasm")]
#[derive(Clone)]
pub struct WebStorageClientIdentityStore {
    key: String,
}

#[cfg(target_family = "wasm")]
impl WebStorageClientIdentityStore {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }

    fn storage(&self) -> BackendResult<web_sys::Storage> {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .ok_or_else(|| {
                BackendError::failed(
                    "client_identity_storage_unavailable",
                    "browser client identity storage is unavailable",
                )
            })
    }
}

#[cfg(target_family = "wasm")]
impl WebStorageCredentialStore {
    pub fn new(key: impl Into<String>) -> Self {
        Self { key: key.into() }
    }

    fn storage(&self) -> BackendResult<web_sys::Storage> {
        web_sys::window()
            .and_then(|window| window.local_storage().ok().flatten())
            .ok_or_else(|| {
                BackendError::failed(
                    "credential_storage_unavailable",
                    "browser credential storage is unavailable",
                )
            })
    }
}

#[cfg(target_family = "wasm")]
impl CredentialStore for WebStorageCredentialStore {
    fn load(&self) -> BackendFuture<'_, Option<RemoteCredentialRecord>> {
        let key = self.key.clone();
        Box::pin(async move {
            let storage = Self::new(key.clone()).storage()?;
            let Some(value) = storage.get_item(&key).map_err(|_| {
                BackendError::failed(
                    "credential_storage_read_failed",
                    "failed to read browser credentials",
                )
            })?
            else {
                return Ok(None);
            };
            serde_json::from_str(&value).map(Some).map_err(|_| {
                BackendError::failed(
                    "credential_storage_invalid",
                    "stored remote credentials are invalid",
                )
            })
        })
    }

    fn save(&self, record: RemoteCredentialRecord) -> BackendFuture<'_, ()> {
        let key = self.key.clone();
        Box::pin(async move {
            let storage = Self::new(key.clone()).storage()?;
            let encoded = serde_json::to_string(&record).map_err(|_| {
                BackendError::failed(
                    "credential_storage_encode_failed",
                    "remote credentials could not be encoded",
                )
            })?;
            storage.set_item(&key, &encoded).map_err(|_| {
                BackendError::failed(
                    "credential_storage_write_failed",
                    "failed to write browser credentials",
                )
            })
        })
    }

    fn clear(&self) -> BackendFuture<'_, ()> {
        let key = self.key.clone();
        Box::pin(async move {
            Self::new(key.clone())
                .storage()?
                .remove_item(&key)
                .map_err(|_| {
                    BackendError::failed(
                        "credential_storage_clear_failed",
                        "failed to clear browser credentials",
                    )
                })
        })
    }
}

#[cfg(target_family = "wasm")]
impl ClientIdentityStore for WebStorageClientIdentityStore {
    fn load(&self) -> BackendFuture<'_, Option<ClientDeviceIdentity>> {
        let key = self.key.clone();
        Box::pin(async move {
            let storage = Self::new(key.clone()).storage()?;
            let Some(value) = storage.get_item(&key).map_err(|_| {
                BackendError::failed(
                    "client_identity_storage_read_failed",
                    "failed to read browser client identity",
                )
            })?
            else {
                return Ok(None);
            };
            let record: StoredClientDeviceIdentity =
                serde_json::from_str(&value).map_err(|_| {
                    BackendError::failed(
                        "client_identity_storage_invalid",
                        "stored client identity is invalid",
                    )
                })?;
            record.into_identity().map(Some)
        })
    }

    fn save(&self, identity: ClientDeviceIdentity) -> BackendFuture<'_, ()> {
        let key = self.key.clone();
        Box::pin(async move {
            let storage = Self::new(key.clone()).storage()?;
            let encoded = serde_json::to_string(&StoredClientDeviceIdentity::from(&identity))
                .map_err(|_| {
                    BackendError::failed(
                        "client_identity_storage_encode_failed",
                        "client identity could not be encoded",
                    )
                })?;
            storage.set_item(&key, &encoded).map_err(|_| {
                BackendError::failed(
                    "client_identity_storage_write_failed",
                    "failed to write browser client identity",
                )
            })
        })
    }

    fn clear(&self) -> BackendFuture<'_, ()> {
        let key = self.key.clone();
        Box::pin(async move {
            Self::new(key.clone())
                .storage()?
                .remove_item(&key)
                .map_err(|_| {
                    BackendError::failed(
                        "client_identity_storage_clear_failed",
                        "failed to clear browser client identity",
                    )
                })
        })
    }
}

#[cfg(any(target_family = "wasm", test))]
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredClientDeviceIdentity {
    device_id: DeviceId,
    private_key: String,
    public_key: String,
}

#[cfg(any(target_family = "wasm", test))]
impl fmt::Debug for StoredClientDeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("StoredClientDeviceIdentity")
            .field("device_id", &self.device_id)
            .field("public_key", &self.public_key)
            .field("has_private_key", &!self.private_key.is_empty())
            .finish()
    }
}

#[cfg(any(target_family = "wasm", test))]
impl From<&ClientDeviceIdentity> for StoredClientDeviceIdentity {
    fn from(identity: &ClientDeviceIdentity) -> Self {
        Self {
            device_id: identity.device_id.clone(),
            private_key: identity.private_key_base64(),
            public_key: identity.public_key_base64(),
        }
    }
}

#[cfg(any(target_family = "wasm", test))]
impl StoredClientDeviceIdentity {
    fn into_identity(self) -> BackendResult<ClientDeviceIdentity> {
        let identity =
            ClientDeviceIdentity::from_private_key_base64(self.device_id, &self.private_key)?;
        if identity.public_key_base64() != self.public_key {
            return Err(BackendError::failed(
                "client_identity_storage_invalid",
                "stored client identity public/private keys do not match",
            ));
        }
        Ok(identity)
    }
}

/// The paired device's long-lived X25519 identity.  Its private bytes are
/// never included in `RemoteCredentialRecord` or formatted in diagnostics.
#[derive(Clone, PartialEq, Eq)]
pub struct ClientDeviceIdentity {
    device_id: DeviceId,
    private_key: [u8; 32],
}

impl fmt::Debug for ClientDeviceIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClientDeviceIdentity")
            .field("device_id", &self.device_id)
            .field("public_key", &self.public_key_base64())
            .field("has_private_key", &true)
            .finish()
    }
}

impl ClientDeviceIdentity {
    pub fn generate(device_id: DeviceId) -> BackendResult<Self> {
        #[cfg(not(target_family = "wasm"))]
        let private_key = StaticSecret::random_from_rng(rand_core::OsRng).to_bytes();

        #[cfg(target_family = "wasm")]
        let private_key = {
            let mut bytes = [0u8; 32];
            let crypto = web_sys::window()
                .and_then(|window| window.crypto().ok())
                .ok_or_else(|| {
                    BackendError::failed(
                        "client_identity_randomness_unavailable",
                        "browser crypto is unavailable",
                    )
                })?;
            crypto
                .get_random_values_with_u8_array(&mut bytes)
                .map_err(|_| {
                    BackendError::failed(
                        "client_identity_randomness_failed",
                        "browser crypto could not generate an identity",
                    )
                })?;
            bytes
        };

        Ok(Self {
            device_id,
            private_key,
        })
    }

    pub fn from_private_key_base64(device_id: DeviceId, encoded: &str) -> BackendResult<Self> {
        let bytes = URL_SAFE_NO_PAD.decode(encoded).map_err(|_| {
            BackendError::failed(
                "client_identity_invalid",
                "client identity private key is invalid",
            )
        })?;
        let private_key: [u8; 32] = bytes.try_into().map_err(|_| {
            BackendError::failed(
                "client_identity_invalid",
                "client identity private key length is invalid",
            )
        })?;
        Ok(Self {
            device_id,
            private_key,
        })
    }

    pub fn device_id(&self) -> &DeviceId {
        &self.device_id
    }

    pub fn public_key_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(PublicKey::from(&StaticSecret::from(self.private_key)).as_bytes())
    }

    pub fn private_key_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.private_key)
    }

    pub(crate) fn private_secret(&self) -> StaticSecret {
        StaticSecret::from(self.private_key)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn identity_debug_and_record_debug_redact_private_material() {
        let identity = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        let private = identity.private_key_base64();
        assert!(!format!("{identity:?}").contains(&private));
        let record = RemoteCredentialRecord {
            server_url: "https://desktop.example".to_string(),
            auth: RemoteAuthProof {
                device_id: identity.device_id().clone(),
                auth_token: "token-secret".to_string(),
            },
            device_identity_public_key: identity.public_key_base64(),
            server_identity_public_key: None,
        };
        assert!(!format!("{record:?}").contains("token-secret"));
    }

    #[test]
    fn memory_store_round_trips_without_session_keys() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let store = MemoryCredentialStore::new();
        let record = RemoteCredentialRecord {
            server_url: "https://desktop.example".to_string(),
            auth: RemoteAuthProof {
                device_id: DeviceId::new(),
                auth_token: "grant".to_string(),
            },
            device_identity_public_key: "public".to_string(),
            server_identity_public_key: Some("server".to_string()),
        };
        runtime.block_on(store.save(record.clone())).unwrap();
        assert_eq!(runtime.block_on(store.load()).unwrap(), Some(record));
        runtime.block_on(store.clear()).unwrap();
        assert!(runtime.block_on(store.load()).unwrap().is_none());
    }

    #[test]
    fn identity_store_round_trips_and_redacts_private_key() {
        let runtime = tokio::runtime::Runtime::new().expect("test runtime");
        let store = MemoryClientIdentityStore::new();
        let identity = ClientDeviceIdentity::generate(DeviceId::new()).unwrap();
        let private = identity.private_key_base64();
        let stored = StoredClientDeviceIdentity::from(&identity);
        assert!(!format!("{stored:?}").contains(&private));
        assert_eq!(stored.clone().into_identity().unwrap(), identity);

        runtime.block_on(store.save(identity.clone())).unwrap();
        let restored = runtime.block_on(store.load()).unwrap().unwrap();
        assert_eq!(restored, identity);
        runtime.block_on(store.clear()).unwrap();
        assert!(runtime.block_on(store.load()).unwrap().is_none());
    }
}

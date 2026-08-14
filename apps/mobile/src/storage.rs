use std::fs::{self, OpenOptions};
use std::io::Write as _;
use std::path::{Path, PathBuf};

use vibex_backend::{BackendError, BackendResult};

use crate::pairing::MobileCredentialBundle;

const CREDENTIAL_FILE: &str = "remote-credentials.json";
const MAX_CREDENTIAL_BYTES: u64 = 64 * 1024;

#[derive(Clone, Debug)]
pub struct CredentialStorage {
    data_dir: PathBuf,
}

impl CredentialStorage {
    pub fn new(data_dir: PathBuf) -> Self {
        Self { data_dir }
    }

    pub fn path(&self) -> PathBuf {
        self.data_dir.join(CREDENTIAL_FILE)
    }

    pub fn load(&self) -> BackendResult<Option<MobileCredentialBundle>> {
        let path = self.path();
        let metadata = match fs::metadata(&path) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(_) => return Err(storage_error("mobile_credentials_read_failed")),
        };
        if metadata.len() == 0 || metadata.len() > MAX_CREDENTIAL_BYTES {
            return self.reject_invalid(&path);
        }
        let bytes = fs::read(&path).map_err(|_| storage_error("mobile_credentials_read_failed"))?;
        let bundle: MobileCredentialBundle = match serde_json::from_slice(&bytes) {
            Ok(bundle) => bundle,
            Err(_) => return self.reject_invalid(&path),
        };
        if bundle.validate().is_err() {
            return self.reject_invalid(&path);
        }
        Ok(Some(bundle))
    }

    pub fn save(&self, bundle: &MobileCredentialBundle) -> BackendResult<()> {
        bundle.validate()?;
        fs::create_dir_all(&self.data_dir)
            .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
        let encoded = serde_json::to_vec(bundle)
            .map_err(|_| storage_error("mobile_credentials_encode_failed"))?;
        if encoded.is_empty() || encoded.len() as u64 > MAX_CREDENTIAL_BYTES {
            return Err(storage_error("mobile_credentials_invalid"));
        }
        let path = self.path();
        let temporary = temporary_path(&path);
        match fs::remove_file(&temporary) {
            Ok(()) => {}
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Err(_) => return Err(storage_error("mobile_credentials_write_failed")),
        }
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt as _;
            options.mode(0o600);
        }
        let outcome = (|| {
            let mut file = options
                .open(&temporary)
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            file.write_all(&encoded)
                .and_then(|_| file.sync_all())
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            fs::rename(&temporary, &path)
                .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt as _;
                fs::set_permissions(&path, fs::Permissions::from_mode(0o600))
                    .map_err(|_| storage_error("mobile_credentials_write_failed"))?;
            }
            Ok(())
        })();
        if outcome.is_err() {
            let _ = fs::remove_file(&temporary);
        }
        outcome
    }

    pub fn clear(&self) -> BackendResult<()> {
        match fs::remove_file(self.path()) {
            Ok(()) => Ok(()),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
            Err(_) => Err(storage_error("mobile_credentials_clear_failed")),
        }
    }

    fn reject_invalid(&self, path: &Path) -> BackendResult<Option<MobileCredentialBundle>> {
        match fs::remove_file(path) {
            Ok(()) => Err(storage_error("mobile_credentials_invalid")),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                Err(storage_error("mobile_credentials_invalid"))
            }
            Err(_) => Err(storage_error("mobile_credentials_clear_failed")),
        }
    }
}

fn temporary_path(path: &Path) -> PathBuf {
    path.with_extension("json.tmp")
}

fn storage_error(code: &'static str) -> BackendError {
    BackendError::failed(code, "native mobile credential storage is unavailable")
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{RemoteAuthProof, RemoteClientType};
    use vibex_remote_client::{ClientDeviceIdentity, RemoteCredentialRecord};

    use crate::pairing::MobileRemoteRouteBundle;

    fn fixture() -> MobileCredentialBundle {
        let identity = ClientDeviceIdentity::generate(vibex_core::DeviceId::new()).unwrap();
        MobileCredentialBundle {
            schema_version: crate::pairing::MOBILE_CREDENTIAL_SCHEMA_VERSION.to_string(),
            record: RemoteCredentialRecord {
                server_url: "https://desktop.example".to_string(),
                auth: RemoteAuthProof {
                    device_id: identity.device_id().clone(),
                    auth_token: "grant".to_string(),
                },
                device_identity_public_key: identity.public_key_base64(),
                server_identity_public_key: Some("server-public".to_string()),
            },
            identity_private_key: identity.private_key_base64(),
            expected_server_id: "desktop".to_string(),
            client_type: RemoteClientType::Mobile,
            allow_insecure_local_dev: false,
            route: Some(MobileRemoteRouteBundle {
                direct_candidates: vec!["https://desktop.example".to_string()],
                relay: None,
            }),
        }
    }

    #[test]
    fn credentials_round_trip_and_clear() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let fixture = fixture();
        assert!(storage.load().unwrap().is_none());
        storage.save(&fixture).unwrap();
        assert_eq!(storage.load().unwrap().unwrap(), fixture);
        storage.clear().unwrap();
        assert!(storage.load().unwrap().is_none());
    }

    #[cfg(unix)]
    #[test]
    fn credential_file_is_owner_only() {
        use std::os::unix::fs::PermissionsExt as _;

        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        storage.save(&fixture()).unwrap();
        assert_eq!(
            fs::metadata(storage.path()).unwrap().permissions().mode() & 0o777,
            0o600
        );
    }

    #[test]
    fn malformed_credentials_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir_all(temp.path()).unwrap();
        fs::write(storage.path(), b"not-json").unwrap();

        let error = storage.load().unwrap_err();
        assert_eq!(error.code, "mobile_credentials_invalid");
        assert!(!storage.path().exists());
    }

    #[test]
    fn semantically_invalid_credentials_are_removed() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        let mut invalid = fixture();
        invalid.schema_version = "future-schema".to_string();
        fs::write(storage.path(), serde_json::to_vec(&invalid).unwrap()).unwrap();

        let error = storage.load().unwrap_err();
        assert_eq!(error.code, "mobile_credentials_invalid");
        assert!(!storage.path().exists());
    }

    #[test]
    fn failed_atomic_replace_removes_the_temporary_file() {
        let temp = tempfile::tempdir().unwrap();
        let storage = CredentialStorage::new(temp.path().to_path_buf());
        fs::create_dir(storage.path()).unwrap();

        let error = storage.save(&fixture()).unwrap_err();
        assert_eq!(error.code, "mobile_credentials_write_failed");
        assert!(!temporary_path(&storage.path()).exists());
    }
}

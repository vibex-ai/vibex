use std::fmt;
use std::fs::{self, OpenOptions};
use std::io::{Read, Write};
use std::path::{Path, PathBuf};

use base64::Engine as _;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use rand_core::OsRng;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use vibex_core::{RequestId, VibexError, VibexResult};
use x25519_dalek::{PublicKey, StaticSecret};

const REMOTE_IDENTITY_FORMAT_VERSION: u16 = 1;

#[derive(Clone)]
pub struct RemoteIdentity {
    server_id: String,
    private_key: [u8; 32],
    public_key: [u8; 32],
}

impl RemoteIdentity {
    pub fn server_id(&self) -> &str {
        &self.server_id
    }

    pub fn public_key_base64(&self) -> String {
        URL_SAFE_NO_PAD.encode(self.public_key)
    }

    pub(crate) fn private_key(&self) -> StaticSecret {
        StaticSecret::from(self.private_key)
    }

    pub(crate) fn private_key_bytes(&self) -> [u8; 32] {
        self.private_key
    }

    pub(crate) fn relay_transport_private_key(&self) -> VibexResult<[u8; 32]> {
        use hkdf::Hkdf;
        let hkdf =
            Hkdf::<Sha256>::new(Some(b"vibex.relay.desktop-transport.v2"), &self.private_key);
        let mut key = [0u8; 32];
        hkdf.expand(self.server_id.as_bytes(), &mut key)
            .map_err(|_| {
                VibexError::storage(
                    "remote_relay_identity_derivation_failed",
                    "failed to derive the persistent Relay transport identity",
                )
            })?;
        Ok(key)
    }
}

impl fmt::Debug for RemoteIdentity {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RemoteIdentity")
            .field("server_id", &self.server_id)
            .field("public_key", &self.public_key_base64())
            .field("has_private_key", &true)
            .finish()
    }
}

#[derive(Debug, Clone)]
pub struct RemoteIdentityStore {
    path: PathBuf,
}

impl RemoteIdentityStore {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self { path: path.into() }
    }

    pub fn path(&self) -> &Path {
        &self.path
    }

    pub fn load_or_create(&self) -> VibexResult<RemoteIdentity> {
        match self.load() {
            Ok(identity) => Ok(identity),
            Err(error) if error.code == "remote_identity_missing" => self.create(),
            Err(error) => Err(error),
        }
    }

    fn load(&self) -> VibexResult<RemoteIdentity> {
        let metadata = fs::symlink_metadata(&self.path).map_err(|error| {
            if error.kind() == std::io::ErrorKind::NotFound {
                VibexError::storage(
                    "remote_identity_missing",
                    "remote desktop identity does not exist",
                )
            } else {
                identity_io_error(
                    "remote_identity_metadata_failed",
                    "failed to inspect remote desktop identity",
                    &error,
                )
            }
        })?;
        if metadata.file_type().is_symlink() || !metadata.is_file() {
            return Err(VibexError::storage(
                "remote_identity_invalid",
                "remote desktop identity must be a real file",
            ));
        }
        let mut file = OpenOptions::new()
            .read(true)
            .open(&self.path)
            .map_err(|error| {
                if error.kind() == std::io::ErrorKind::NotFound {
                    VibexError::storage(
                        "remote_identity_missing",
                        "remote desktop identity does not exist",
                    )
                } else {
                    identity_io_error(
                        "remote_identity_open_failed",
                        "failed to open remote desktop identity",
                        &error,
                    )
                }
            })?;
        enforce_private_permissions(&self.path)?;
        let mut encoded = String::new();
        file.read_to_string(&mut encoded).map_err(|error| {
            identity_io_error(
                "remote_identity_read_failed",
                "failed to read remote desktop identity",
                &error,
            )
        })?;
        let stored: StoredIdentity = serde_json::from_str(&encoded).map_err(|_| {
            VibexError::storage(
                "remote_identity_invalid",
                "remote desktop identity file is invalid",
            )
        })?;
        if stored.format_version != REMOTE_IDENTITY_FORMAT_VERSION {
            return Err(VibexError::storage(
                "remote_identity_version_unsupported",
                "remote desktop identity version is unsupported",
            ));
        }
        let private_key = decode_key(&stored.private_key)?;
        Ok(identity_from_private_key(private_key))
    }

    fn create(&self) -> VibexResult<RemoteIdentity> {
        let parent = self.path.parent().ok_or_else(|| {
            VibexError::storage(
                "remote_identity_parent_missing",
                "remote desktop identity path has no parent",
            )
        })?;
        fs::create_dir_all(parent).map_err(|error| {
            identity_io_error(
                "remote_identity_directory_create_failed",
                "failed to create remote identity directory",
                &error,
            )
        })?;
        let parent_metadata = fs::symlink_metadata(parent).map_err(|error| {
            identity_io_error(
                "remote_identity_directory_metadata_failed",
                "failed to inspect remote identity directory",
                &error,
            )
        })?;
        if parent_metadata.file_type().is_symlink() || !parent_metadata.is_dir() {
            return Err(VibexError::storage(
                "remote_identity_directory_invalid",
                "remote identity directory must be a real directory",
            ));
        }
        enforce_private_directory_permissions(parent)?;

        let private = StaticSecret::random_from_rng(OsRng);
        let identity = identity_from_private_key(private.to_bytes());
        let stored = StoredIdentity {
            format_version: REMOTE_IDENTITY_FORMAT_VERSION,
            private_key: URL_SAFE_NO_PAD.encode(identity.private_key),
        };
        let bytes = serde_json::to_vec(&stored).map_err(|_| {
            VibexError::storage(
                "remote_identity_encode_failed",
                "failed to encode remote desktop identity",
            )
        })?;
        let temp_path = parent.join(format!(
            ".remote-identity-{}.tmp",
            RequestId::new().into_string()
        ));
        let write_result = write_private_file(&temp_path, &bytes);
        if let Err(error) = write_result {
            let _ = fs::remove_file(&temp_path);
            return Err(error);
        }
        match fs::hard_link(&temp_path, &self.path) {
            Ok(()) => {
                let _ = fs::remove_file(&temp_path);
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => {
                let _ = fs::remove_file(&temp_path);
                return self.load();
            }
            Err(error) => {
                let _ = fs::remove_file(&temp_path);
                return Err(identity_io_error(
                    "remote_identity_commit_failed",
                    "failed to commit remote desktop identity",
                    &error,
                ));
            }
        }
        enforce_private_permissions(&self.path)?;
        Ok(identity)
    }
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct StoredIdentity {
    format_version: u16,
    private_key: String,
}

fn identity_from_private_key(private_key: [u8; 32]) -> RemoteIdentity {
    let private = StaticSecret::from(private_key);
    let public_key = PublicKey::from(&private).to_bytes();
    let digest = Sha256::digest(public_key);
    let server_id = format!("server-{}", hex_prefix(&digest, 20));
    RemoteIdentity {
        server_id,
        private_key,
        public_key,
    }
}

fn decode_key(value: &str) -> VibexResult<[u8; 32]> {
    let decoded = URL_SAFE_NO_PAD.decode(value).map_err(|_| {
        VibexError::storage(
            "remote_identity_invalid",
            "remote desktop identity key is invalid",
        )
    })?;
    decoded.try_into().map_err(|_| {
        VibexError::storage(
            "remote_identity_invalid",
            "remote desktop identity key length is invalid",
        )
    })
}

fn hex_prefix(bytes: &[u8], byte_count: usize) -> String {
    let mut output = String::with_capacity(byte_count * 2);
    for byte in bytes.iter().take(byte_count) {
        use std::fmt::Write as _;
        let _ = write!(output, "{byte:02x}");
    }
    output
}

fn write_private_file(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path).map_err(|error| {
        identity_io_error(
            "remote_identity_create_failed",
            "failed to create remote desktop identity",
            &error,
        )
    })?;
    file.write_all(bytes).map_err(|error| {
        identity_io_error(
            "remote_identity_write_failed",
            "failed to write remote desktop identity",
            &error,
        )
    })?;
    file.sync_all().map_err(|error| {
        identity_io_error(
            "remote_identity_sync_failed",
            "failed to sync remote desktop identity",
            &error,
        )
    })
}

#[cfg(unix)]
fn enforce_private_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::{MetadataExt, PermissionsExt};

    let metadata = fs::metadata(path).map_err(|error| {
        identity_io_error(
            "remote_identity_metadata_failed",
            "failed to inspect remote desktop identity permissions",
            &error,
        )
    })?;
    if metadata.mode() & 0o077 != 0 {
        fs::set_permissions(path, fs::Permissions::from_mode(0o600)).map_err(|error| {
            identity_io_error(
                "remote_identity_permissions_failed",
                "failed to protect remote desktop identity permissions",
                &error,
            )
        })?;
    }
    Ok(())
}

#[cfg(unix)]
fn enforce_private_directory_permissions(path: &Path) -> VibexResult<()> {
    use std::os::unix::fs::PermissionsExt;

    fs::set_permissions(path, fs::Permissions::from_mode(0o700)).map_err(|error| {
        identity_io_error(
            "remote_identity_directory_permissions_failed",
            "failed to protect remote identity directory permissions",
            &error,
        )
    })
}

#[cfg(not(unix))]
fn enforce_private_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

#[cfg(not(unix))]
fn enforce_private_directory_permissions(_path: &Path) -> VibexResult<()> {
    Ok(())
}

fn identity_io_error(
    code: &'static str,
    message: &'static str,
    error: &std::io::Error,
) -> VibexError {
    VibexError::storage(code, message).with_diagnostic("errorKind", format!("{:?}", error.kind()))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn identity_is_persistent_private_and_debug_redacted() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("relay/desktop-identity.json");
        let store = RemoteIdentityStore::new(&path);

        let created = store.load_or_create().unwrap();
        let loaded = store.load_or_create().unwrap();

        assert_eq!(created.server_id(), loaded.server_id());
        assert_eq!(created.public_key_base64(), loaded.public_key_base64());
        let file = fs::read_to_string(&path).unwrap();
        assert!(!format!("{created:?}").contains(&file));
        #[cfg(unix)]
        {
            use std::os::unix::fs::MetadataExt;
            assert_eq!(fs::metadata(path).unwrap().mode() & 0o077, 0);
        }
    }

    #[cfg(unix)]
    #[test]
    fn identity_rejects_symlink_files() {
        use std::os::unix::fs::symlink;

        let directory = tempfile::tempdir().unwrap();
        let real_path = directory.path().join("real-identity.json");
        RemoteIdentityStore::new(&real_path)
            .load_or_create()
            .unwrap();
        let link_path = directory.path().join("identity-link.json");
        symlink(&real_path, &link_path).unwrap();

        let error = RemoteIdentityStore::new(link_path)
            .load_or_create()
            .unwrap_err();
        assert_eq!(error.code, "remote_identity_invalid");
    }
}

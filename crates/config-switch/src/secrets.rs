use std::collections::BTreeMap;
use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use vibex_core::{
    ProviderSecretBackend, ProviderSecretKind, ProviderSecretReference, ProviderSecretSetupState,
    VibexError, VibexResult,
};

#[cfg(not(test))]
const VIBEX_SECRET_SERVICE: &str = "dev.vibex.provider-secrets";

/// Host-side secret file used when no usable OS keychain exists.
const HOST_SECRET_FILE: &str = "provider-secrets.json";

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock as TestOnceLock};

#[cfg(test)]
const TEST_PROVIDER_SECRET_STORE_FAILURE_VALUE: &str =
    "__vibex_test_provider_secret_store_failure__";

#[cfg(test)]
pub(crate) fn test_provider_secret_store_failure_value() -> &'static str {
    TEST_PROVIDER_SECRET_STORE_FAILURE_VALUE
}

#[cfg(test)]
fn test_secret_store() -> &'static Mutex<HashMap<String, String>> {
    static STORE: TestOnceLock<Mutex<HashMap<String, String>>> = TestOnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
}

/// Where a runtime writes newly stored provider secrets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderSecretStore {
    /// The desktop OS keychain.
    OsKeychain,
    /// A host-owned secret file under the configured root.
    HostFile,
}

/// Process-wide provider secret store selection.
///
/// A desktop shell keeps the default (OS keychain).  A headless server has no
/// usable keychain — Docker's default seccomp profile rejects the keyutils
/// syscalls the Linux keychain backend needs — so it stores provider secrets in
/// a host-owned file inside its runtime home instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderSecretStoreConfig {
    store: ProviderSecretStore,
    root: Option<PathBuf>,
}

impl ProviderSecretStoreConfig {
    pub fn os_keychain() -> Self {
        Self {
            store: ProviderSecretStore::OsKeychain,
            root: None,
        }
    }

    pub fn host_file(root: impl Into<PathBuf>) -> Self {
        Self {
            store: ProviderSecretStore::HostFile,
            root: Some(root.into()),
        }
    }

    pub fn store(&self) -> ProviderSecretStore {
        self.store
    }

    pub fn root(&self) -> Option<&Path> {
        self.root.as_deref()
    }
}

static PROVIDER_SECRET_STORE: OnceLock<ProviderSecretStoreConfig> = OnceLock::new();

/// Selects the provider secret store for this process.  The first call wins, so
/// a desktop shell that never calls this keeps the OS keychain.
pub fn configure_provider_secret_store(config: ProviderSecretStoreConfig) {
    let _ = PROVIDER_SECRET_STORE.set(config);
}

fn provider_secret_store() -> ProviderSecretStoreConfig {
    PROVIDER_SECRET_STORE
        .get()
        .cloned()
        .unwrap_or_else(ProviderSecretStoreConfig::os_keychain)
}

/// The backend recorded for a secret that is being stored right now.
pub fn provider_secret_write_backend() -> ProviderSecretBackend {
    write_backend_for(&provider_secret_store())
}

fn write_backend_for(config: &ProviderSecretStoreConfig) -> ProviderSecretBackend {
    match config.store() {
        ProviderSecretStore::HostFile => ProviderSecretBackend::HostFile,
        ProviderSecretStore::OsKeychain => ProviderSecretBackend::OsKeychain,
    }
}

/// Whether `backend` names a secret this host stores locally.
pub fn is_local_secret_backend(backend: ProviderSecretBackend) -> bool {
    matches!(
        backend,
        ProviderSecretBackend::OsKeychain | ProviderSecretBackend::HostFile
    )
}

/// Human-readable storage hint recorded next to a secret reference.
pub fn provider_secret_storage_hint(backend: ProviderSecretBackend) -> &'static str {
    match backend {
        ProviderSecretBackend::HostFile => "stored on the runtime host",
        _ => "stored in Vibex OS keychain",
    }
}

pub fn store_provider_secret(lookup_key: &str, secret: &str) -> VibexResult<()> {
    let lookup_key = validate_lookup_key(lookup_key)?;
    if secret.is_empty() {
        return Err(VibexError::validation(
            "provider_secret_empty",
            "provider secret must not be empty",
        ));
    }

    #[cfg(test)]
    {
        if secret == TEST_PROVIDER_SECRET_STORE_FAILURE_VALUE {
            return Err(VibexError::storage(
                "provider_secret_keychain_store_failed",
                "failed to store provider secret in OS keychain",
            )
            .with_diagnostic("backend", "os_keychain")
            .with_diagnostic("error", "forced test keychain failure"));
        }
        test_secret_store()
            .lock()
            .map_err(|_| {
                VibexError::storage(
                    "provider_secret_store_lock_failed",
                    "failed to lock provider secret test store",
                )
            })?
            .insert(lookup_key.to_string(), secret.to_string());
        Ok(())
    }

    #[cfg(not(test))]
    {
        match provider_secret_store().store() {
            ProviderSecretStore::HostFile => {
                let root = host_file_root()?;
                store_host_file_secret(&root, lookup_key, secret)
            }
            ProviderSecretStore::OsKeychain => keyring_entry(lookup_key)?
                .set_password(secret)
                .map_err(|error| {
                    VibexError::storage(
                        "provider_secret_keychain_store_failed",
                        "failed to store provider secret in OS keychain",
                    )
                    .with_diagnostic("backend", "os_keychain")
                    .with_diagnostic("error", error.to_string())
                }),
        }
    }
}

pub fn resolve_provider_secret(reference: &ProviderSecretReference) -> VibexResult<Option<String>> {
    resolve_provider_secret_reference(
        reference.backend,
        reference.setup_state,
        &reference.lookup_key,
    )
    .map_err(|error| error.with_diagnostic("secretKind", format!("{:?}", reference.secret_kind)))
}

pub fn resolve_provider_secret_reference(
    backend: ProviderSecretBackend,
    setup_state: ProviderSecretSetupState,
    lookup_key: &str,
) -> VibexResult<Option<String>> {
    if setup_state == ProviderSecretSetupState::Missing
        || backend == ProviderSecretBackend::Placeholder
    {
        return Ok(None);
    }

    match backend {
        ProviderSecretBackend::OsKeychain => load_os_secret(lookup_key),
        ProviderSecretBackend::HostFile => load_host_secret(lookup_key),
        ProviderSecretBackend::Environment => {
            let lookup_key = validate_lookup_key(lookup_key)?;
            Ok(std::env::var(lookup_key)
                .ok()
                .map(|value| value.trim().to_string())
                .filter(|value| !value.is_empty()))
        }
        ProviderSecretBackend::External => Err(VibexError::capability(
            "provider_secret_external_unsupported",
            "external provider secret backends are not supported by the local runtime yet",
        )),
        ProviderSecretBackend::Placeholder => Ok(None),
    }
}

pub fn delete_provider_secret(lookup_key: &str) -> VibexResult<()> {
    let lookup_key = validate_lookup_key(lookup_key)?;

    #[cfg(test)]
    {
        test_secret_store()
            .lock()
            .map_err(|_| {
                VibexError::storage(
                    "provider_secret_store_lock_failed",
                    "failed to lock provider secret test store",
                )
            })?
            .remove(lookup_key);
        Ok(())
    }

    #[cfg(not(test))]
    {
        match provider_secret_store().store() {
            ProviderSecretStore::HostFile => {
                let root = host_file_root()?;
                delete_host_file_secret(&root, lookup_key)?;
                // A desktop home reused as a server home can still hold an
                // earlier copy in the OS keychain; that cleanup is best effort.
                let _ = delete_keychain_secret(lookup_key);
                Ok(())
            }
            ProviderSecretStore::OsKeychain => delete_keychain_secret(lookup_key),
        }
    }
}

pub fn preferred_api_key_reference<'a>(
    secrets: &'a [ProviderSecretReference],
    env_key: &str,
) -> Option<&'a ProviderSecretReference> {
    secrets
        .iter()
        .find(|secret| {
            secret.secret_kind == ProviderSecretKind::ApiKey
                && secret.lookup_key == env_key
                && secret.backend != ProviderSecretBackend::Placeholder
        })
        .or_else(|| {
            secrets.iter().find(|secret| {
                secret.secret_kind == ProviderSecretKind::ApiKey
                    && secret.backend != ProviderSecretBackend::Placeholder
            })
        })
        .or_else(|| {
            secrets
                .iter()
                .find(|secret| secret.secret_kind == ProviderSecretKind::ApiKey)
        })
}

#[cfg(not(test))]
fn host_file_root() -> VibexResult<PathBuf> {
    provider_secret_store()
        .root()
        .map(Path::to_path_buf)
        .ok_or_else(|| {
            VibexError::process(
                "provider_secret_host_file_root_missing",
                "the host provider secret store has no runtime home configured",
            )
        })
}

/// Reads a secret recorded as [`ProviderSecretBackend::HostFile`].
fn load_host_secret(lookup_key: &str) -> VibexResult<Option<String>> {
    let lookup_key = validate_lookup_key(lookup_key)?;

    #[cfg(test)]
    {
        return Ok(test_secret_store()
            .lock()
            .map_err(|_| {
                VibexError::storage(
                    "provider_secret_store_lock_failed",
                    "failed to lock provider secret test store",
                )
            })?
            .get(lookup_key)
            .cloned());
    }

    #[cfg(not(test))]
    {
        let root = host_file_root()?;
        load_host_file_secret(&root, lookup_key)
    }
}

fn load_os_secret(lookup_key: &str) -> VibexResult<Option<String>> {
    let lookup_key = validate_lookup_key(lookup_key)?;

    #[cfg(test)]
    {
        return Ok(test_secret_store()
            .lock()
            .map_err(|_| {
                VibexError::storage(
                    "provider_secret_store_lock_failed",
                    "failed to lock provider secret test store",
                )
            })?
            .get(lookup_key)
            .cloned());
    }

    #[cfg(not(test))]
    {
        match provider_secret_store().store() {
            ProviderSecretStore::HostFile => {
                let root = host_file_root()?;
                if let Some(secret) = load_host_file_secret(&root, lookup_key)? {
                    return Ok(Some(secret));
                }
                // Best-effort fallback for a home that a desktop shell used
                // before it became a server home.
                Ok(load_keychain_secret(lookup_key).ok().flatten())
            }
            ProviderSecretStore::OsKeychain => load_keychain_secret(lookup_key),
        }
    }
}

#[cfg(not(test))]
fn load_keychain_secret(lookup_key: &str) -> VibexResult<Option<String>> {
    match keyring_entry(lookup_key)?.get_password() {
        Ok(secret) => Ok(Some(secret)),
        Err(keyring::Error::NoEntry) => Ok(None),
        Err(error) => Err(VibexError::storage(
            "provider_secret_keychain_read_failed",
            "failed to read provider secret from OS keychain",
        )
        .with_diagnostic("backend", "os_keychain")
        .with_diagnostic("error", error.to_string())),
    }
}

#[cfg(not(test))]
fn delete_keychain_secret(lookup_key: &str) -> VibexResult<()> {
    match keyring_entry(lookup_key)?.delete_credential() {
        Ok(()) | Err(keyring::Error::NoEntry) => Ok(()),
        Err(error) => Err(VibexError::storage(
            "provider_secret_keychain_delete_failed",
            "failed to delete provider secret from OS keychain",
        )
        .with_diagnostic("backend", "os_keychain")
        .with_diagnostic("error", error.to_string())),
    }
}

fn host_secret_path(root: &Path) -> PathBuf {
    root.join(HOST_SECRET_FILE)
}

fn store_host_file_secret(root: &Path, lookup_key: &str, secret: &str) -> VibexResult<()> {
    let mut secrets = read_host_secret_file(root)?;
    secrets.insert(lookup_key.to_string(), secret.to_string());
    write_host_secret_file(root, &secrets)
}

fn load_host_file_secret(root: &Path, lookup_key: &str) -> VibexResult<Option<String>> {
    Ok(read_host_secret_file(root)?
        .get(lookup_key)
        .cloned()
        .filter(|secret| !secret.is_empty()))
}

fn delete_host_file_secret(root: &Path, lookup_key: &str) -> VibexResult<()> {
    let mut secrets = read_host_secret_file(root)?;
    if secrets.remove(lookup_key).is_none() {
        return Ok(());
    }
    write_host_secret_file(root, &secrets)
}

fn read_host_secret_file(root: &Path) -> VibexResult<BTreeMap<String, String>> {
    let path = host_secret_path(root);
    let bytes = match std::fs::read(&path) {
        Ok(bytes) => bytes,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(error) => return Err(host_secret_file_error("read", &path, &error)),
    };
    if bytes.iter().all(u8::is_ascii_whitespace) {
        return Ok(BTreeMap::new());
    }
    serde_json::from_slice(&bytes).map_err(|error| {
        VibexError::storage(
            "provider_secret_host_file_read_failed",
            "provider secret file is not valid JSON",
        )
        .with_diagnostic("backend", "host_file")
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("error", error.to_string())
    })
}

fn write_host_secret_file(root: &Path, secrets: &BTreeMap<String, String>) -> VibexResult<()> {
    std::fs::create_dir_all(root)
        .map_err(|error| host_secret_file_error("create", root, &error))?;
    let path = host_secret_path(root);
    let temp_path = path.with_extension("json.tmp");
    let bytes = serde_json::to_vec_pretty(secrets).map_err(|error| {
        VibexError::storage(
            "provider_secret_host_file_write_failed",
            "provider secret file could not be encoded",
        )
        .with_diagnostic("backend", "host_file")
        .with_diagnostic("error", error.to_string())
    })?;
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt as _;
        options.mode(0o600);
    }
    let result = (|| -> std::io::Result<()> {
        let mut file = options.open(&temp_path)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        std::fs::rename(&temp_path, &path)?;
        Ok(())
    })();
    if let Err(error) = result {
        let _ = std::fs::remove_file(&temp_path);
        return Err(host_secret_file_error("write", &path, &error));
    }
    Ok(())
}

fn host_secret_file_error(action: &'static str, path: &Path, error: &std::io::Error) -> VibexError {
    let code = match action {
        "read" => "provider_secret_host_file_read_failed",
        "create" => "provider_secret_host_file_create_failed",
        _ => "provider_secret_host_file_write_failed",
    };
    VibexError::storage(code, "provider secret file could not be accessed")
        .with_diagnostic("backend", "host_file")
        .with_diagnostic("path", path.display().to_string())
        .with_diagnostic("error", error.to_string())
}

fn validate_lookup_key(lookup_key: &str) -> VibexResult<&str> {
    let lookup_key = lookup_key.trim();
    if lookup_key.is_empty() || lookup_key.contains('\0') {
        return Err(VibexError::validation(
            "provider_secret_lookup_key_invalid",
            "provider secret lookup key is invalid",
        ));
    }
    Ok(lookup_key)
}

#[cfg(not(test))]
fn keyring_entry(lookup_key: &str) -> VibexResult<keyring::Entry> {
    keyring::Entry::new(VIBEX_SECRET_SERVICE, lookup_key).map_err(|error| {
        VibexError::storage(
            "provider_secret_keychain_entry_failed",
            "failed to open provider secret keychain entry",
        )
        .with_diagnostic("backend", "os_keychain")
        .with_diagnostic("error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_secret_file_round_trips_and_stays_private() {
        let dir = tempfile::tempdir().unwrap();
        store_host_file_secret(dir.path(), "vibex-provider-secret-1", "sk-one").unwrap();
        store_host_file_secret(dir.path(), "vibex-provider-secret-2", "sk-two").unwrap();
        assert_eq!(
            load_host_file_secret(dir.path(), "vibex-provider-secret-1")
                .unwrap()
                .as_deref(),
            Some("sk-one")
        );
        assert_eq!(
            load_host_file_secret(dir.path(), "vibex-provider-secret-missing").unwrap(),
            None
        );
        delete_host_file_secret(dir.path(), "vibex-provider-secret-1").unwrap();
        assert_eq!(
            load_host_file_secret(dir.path(), "vibex-provider-secret-1").unwrap(),
            None
        );
        assert_eq!(
            load_host_file_secret(dir.path(), "vibex-provider-secret-2")
                .unwrap()
                .as_deref(),
            Some("sk-two")
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt as _;
            let mode = std::fs::metadata(host_secret_path(dir.path()))
                .unwrap()
                .permissions()
                .mode()
                & 0o777;
            assert_eq!(mode, 0o600, "the host secret file must stay owner-only");
        }
    }

    #[test]
    fn host_secret_file_tolerates_a_missing_or_empty_file() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(load_host_file_secret(dir.path(), "any").unwrap(), None);
        std::fs::write(host_secret_path(dir.path()), b"\n").unwrap();
        assert_eq!(load_host_file_secret(dir.path(), "any").unwrap(), None);
    }

    #[test]
    fn write_backend_tracks_the_configured_store() {
        assert_eq!(
            write_backend_for(&ProviderSecretStoreConfig::os_keychain()),
            ProviderSecretBackend::OsKeychain
        );
        assert_eq!(
            write_backend_for(&ProviderSecretStoreConfig::host_file("/data")),
            ProviderSecretBackend::HostFile
        );
        assert!(is_local_secret_backend(ProviderSecretBackend::HostFile));
        assert!(is_local_secret_backend(ProviderSecretBackend::OsKeychain));
        assert!(!is_local_secret_backend(ProviderSecretBackend::Environment));
        assert_eq!(
            provider_secret_storage_hint(ProviderSecretBackend::HostFile),
            "stored on the runtime host"
        );
        assert_eq!(
            provider_secret_storage_hint(ProviderSecretBackend::OsKeychain),
            "stored in Vibex OS keychain"
        );
    }
}

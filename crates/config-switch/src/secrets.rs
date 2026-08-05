use vibex_core::{
    ProviderSecretBackend, ProviderSecretKind, ProviderSecretReference, ProviderSecretSetupState,
    VibexError, VibexResult,
};

#[cfg(not(test))]
const VIBEX_SECRET_SERVICE: &str = "dev.vibex.provider-secrets";

#[cfg(test)]
use std::collections::HashMap;
#[cfg(test)]
use std::sync::{Mutex, OnceLock};

#[cfg(test)]
const TEST_PROVIDER_SECRET_STORE_FAILURE_VALUE: &str =
    "__vibex_test_provider_secret_store_failure__";

#[cfg(test)]
pub(crate) fn test_provider_secret_store_failure_value() -> &'static str {
    TEST_PROVIDER_SECRET_STORE_FAILURE_VALUE
}

#[cfg(test)]
fn test_secret_store() -> &'static Mutex<HashMap<String, String>> {
    static STORE: OnceLock<Mutex<HashMap<String, String>>> = OnceLock::new();
    STORE.get_or_init(|| Mutex::new(HashMap::new()))
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
        keyring_entry(lookup_key)?
            .set_password(secret)
            .map_err(|error| {
                VibexError::storage(
                    "provider_secret_keychain_store_failed",
                    "failed to store provider secret in OS keychain",
                )
                .with_diagnostic("backend", "os_keychain")
                .with_diagnostic("error", error.to_string())
            })
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

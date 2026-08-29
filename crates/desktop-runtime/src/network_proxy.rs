//! Process-wide outbound proxy state used by native desktop network clients.
//!
//! The setting is deliberately local to the desktop process. It is applied to
//! spawned Agent/Git processes through the standard proxy environment variables
//! and to in-process `reqwest` clients through explicit builders. Clients that
//! are cached use the revision to rebuild after a setting change.

use std::sync::{OnceLock, RwLock};

use reqwest::{Client, Url};
use vibex_desktop_model::NetworkProxyUiState;

const PROXY_ENV_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
const NO_PROXY_ENV_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];
const NO_PROXY_DEFAULT: &str = "localhost,127.0.0.1,::1";
const MAX_PROXY_URL_CHARS: usize = 2_048;

#[derive(Clone)]
struct CachedAsyncClient {
    revision: u64,
    client: Client,
}

#[derive(Clone)]
struct CachedBlockingClient {
    revision: u64,
    client: reqwest::blocking::Client,
}

struct ProxyState {
    snapshot: RwLock<(NetworkProxyUiState, u64)>,
    async_client: RwLock<Option<CachedAsyncClient>>,
    blocking_client: RwLock<Option<CachedBlockingClient>>,
}

fn state() -> &'static ProxyState {
    static STATE: OnceLock<ProxyState> = OnceLock::new();
    STATE.get_or_init(|| ProxyState {
        snapshot: RwLock::new((NetworkProxyUiState::default(), 0)),
        async_client: RwLock::new(None),
        blocking_client: RwLock::new(None),
    })
}

/// Normalize and validate a proxy URL at the settings boundary.
///
/// A bare `host:port` is accepted as a convenience and canonicalized to an
/// HTTP proxy URL. Explicit HTTP(S) and SOCKS5 schemes are preserved. User
/// info is supported because authenticated enterprise proxies are common, but
/// it is never included in error text or debug output by this module.
pub fn normalize_proxy_url(raw: &str) -> Result<String, String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() || trimmed.chars().count() > MAX_PROXY_URL_CHARS {
        return Err("proxy address is empty or too long".to_string());
    }
    let normalized = if trimmed.contains("://") {
        trimmed.to_string()
    } else {
        format!("http://{trimmed}")
    };
    let parsed = Url::parse(&normalized).map_err(|_| "proxy address is invalid".to_string())?;
    if !matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h") {
        return Err("proxy address must use http, https, socks5, or socks5h".to_string());
    }
    if parsed.host_str().is_none_or(str::is_empty) {
        return Err("proxy address must include a host".to_string());
    }
    reqwest::Proxy::all(&normalized)
        .map_err(|_| "proxy address is not supported by the HTTP client".to_string())?;
    Ok(normalized)
}

/// Validate and canonicalize a complete settings value.
pub fn normalize_settings(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    let proxy_url = settings
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_proxy_url)
        .transpose()?;
    if settings.enabled && proxy_url.is_none() {
        return Err("proxy address is required when the proxy is enabled".to_string());
    }
    Ok(NetworkProxyUiState {
        enabled: settings.enabled,
        proxy_url,
    })
}

/// Initialize the process from the persisted preference before the first
/// runtime client is constructed. A disabled preference deliberately leaves
/// externally supplied proxy environment variables untouched at startup.
pub fn initialize(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    let normalized = normalize_settings(settings)?;
    set_snapshot(normalized.clone());
    if normalized.enabled {
        apply_proxy_env(normalized.proxy_url.as_deref());
    }
    Ok(normalized)
}

/// Apply a user-initiated settings change immediately.
pub fn configure(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    let normalized = normalize_settings(settings)?;
    set_snapshot(normalized.clone());
    apply_proxy_env(if normalized.enabled {
        normalized.proxy_url.as_deref()
    } else {
        None
    });
    Ok(normalized)
}

fn set_snapshot(settings: NetworkProxyUiState) {
    let mut snapshot = state()
        .snapshot
        .write()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    if snapshot.0 != settings {
        snapshot.1 = snapshot.1.wrapping_add(1);
        snapshot.0 = settings;
        *state()
            .async_client
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
        *state()
            .blocking_client
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = None;
    }
}

fn current_snapshot() -> (NetworkProxyUiState, u64) {
    state()
        .snapshot
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone()
}

pub fn current_settings() -> NetworkProxyUiState {
    current_snapshot().0
}

pub fn revision() -> u64 {
    current_snapshot().1
}

fn apply_proxy_env(proxy_url: Option<&str>) {
    match proxy_url {
        Some(proxy_url) => {
            for key in PROXY_ENV_KEYS {
                // Environment mutation happens only at explicit native app
                // boundaries, before child/client work is spawned.
                unsafe { std::env::set_var(key, proxy_url) };
            }
            for key in NO_PROXY_ENV_KEYS {
                unsafe { std::env::set_var(key, NO_PROXY_DEFAULT) };
            }
        }
        None => {
            for key in PROXY_ENV_KEYS {
                unsafe { std::env::remove_var(key) };
            }
        }
    }
}

fn build_proxy(proxy_url: &str) -> Result<reqwest::Proxy, String> {
    reqwest::Proxy::all(proxy_url)
        .map(|proxy| proxy.no_proxy(reqwest::NoProxy::from_string(NO_PROXY_DEFAULT)))
        .map_err(|_| "proxy address is invalid".to_string())
}

pub fn async_client_builder() -> Result<reqwest::ClientBuilder, String> {
    let (settings, _) = current_snapshot();
    if settings.enabled {
        let proxy_url = settings
            .proxy_url
            .as_deref()
            .ok_or_else(|| "proxy address is required when the proxy is enabled".to_string())?;
        Ok(Client::builder().no_proxy().proxy(build_proxy(proxy_url)?))
    } else {
        Ok(Client::builder())
    }
}

pub fn blocking_client_builder() -> Result<reqwest::blocking::ClientBuilder, String> {
    let (settings, _) = current_snapshot();
    if settings.enabled {
        let proxy_url = settings
            .proxy_url
            .as_deref()
            .ok_or_else(|| "proxy address is required when the proxy is enabled".to_string())?;
        Ok(reqwest::blocking::Client::builder()
            .no_proxy()
            .proxy(build_proxy(proxy_url)?))
    } else {
        Ok(reqwest::blocking::Client::builder())
    }
}

pub fn cached_client() -> Result<Client, String> {
    let (_, snapshot_revision) = current_snapshot();
    if let Some(cached) = state()
        .async_client
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|cached| cached.revision == snapshot_revision)
    {
        return Ok(cached.client.clone());
    }
    let client = async_client_builder()?.build().map_err(|_| {
        "the configured network proxy HTTP client could not be initialized".to_string()
    })?;
    if revision() == snapshot_revision {
        *state()
            .async_client
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedAsyncClient {
            revision: snapshot_revision,
            client: client.clone(),
        });
    }
    Ok(client)
}

pub fn cached_blocking_client() -> Result<reqwest::blocking::Client, String> {
    let (_, snapshot_revision) = current_snapshot();
    if let Some(cached) = state()
        .blocking_client
        .read()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .as_ref()
        .filter(|cached| cached.revision == snapshot_revision)
    {
        return Ok(cached.client.clone());
    }
    let client = blocking_client_builder()?.build().map_err(|_| {
        "the configured network proxy HTTP client could not be initialized".to_string()
    })?;
    if revision() == snapshot_revision {
        *state()
            .blocking_client
            .write()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = Some(CachedBlockingClient {
            revision: snapshot_revision,
            client: client.clone(),
        });
    }
    Ok(client)
}

pub fn shell_proxy_envs() -> Vec<(String, String)> {
    let settings = current_settings();
    let Some(proxy_url) = settings.proxy_url.filter(|_| settings.enabled) else {
        return Vec::new();
    };
    PROXY_ENV_KEYS
        .into_iter()
        .map(|key| (key.to_string(), proxy_url.clone()))
        .chain(
            NO_PROXY_ENV_KEYS
                .into_iter()
                .map(|key| (key.to_string(), NO_PROXY_DEFAULT.to_string())),
        )
        .collect()
}

pub fn current_proxy_url() -> Result<Option<Url>, String> {
    let settings = current_settings();
    if !settings.enabled {
        return Ok(None);
    }
    settings
        .proxy_url
        .as_deref()
        .map(Url::parse)
        .transpose()
        .map_err(|_| "proxy address is invalid".to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn normalizes_bare_host_and_preserves_explicit_schemes() {
        assert_eq!(
            normalize_proxy_url(" 127.0.0.1:7890 ").unwrap(),
            "http://127.0.0.1:7890"
        );
        assert_eq!(
            normalize_proxy_url("socks5://proxy.local:1080").unwrap(),
            "socks5://proxy.local:1080"
        );
        assert_eq!(
            normalize_proxy_url("socks5h://proxy.local:1080").unwrap(),
            "socks5h://proxy.local:1080"
        );
    }

    #[test]
    fn rejects_missing_host_and_unsupported_scheme() {
        assert!(normalize_proxy_url("http://").is_err());
        assert!(normalize_proxy_url("ftp://proxy.local:21").is_err());
    }

    #[test]
    fn enabled_settings_require_an_address() {
        let settings = NetworkProxyUiState {
            enabled: true,
            proxy_url: None,
        };
        assert!(normalize_settings(&settings).is_err());
    }
}

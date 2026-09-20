//! Process-wide outbound proxy state used by native desktop network clients.
//!
//! The setting is deliberately local to the desktop process. It is applied to
//! spawned Agent/Git processes through the standard proxy environment variables
//! and to in-process `reqwest` clients through explicit builders. Clients that
//! are cached use the revision to rebuild after a setting change.
//!
//! Three modes decide the route:
//!
//! - `System` leaves the ambient environment and the HTTP client's own system
//!   proxy discovery in charge;
//! - `Direct` clears every proxy hint so nothing is routed through a proxy;
//! - `Custom` applies the configured URL and bypass list.

use std::sync::{OnceLock, RwLock};
use std::time::{Duration, Instant};

use reqwest::{Client, Url};
use vibex_desktop_model::{NetworkProxyMode, NetworkProxyUiState};

const PROXY_ENV_KEYS: [&str; 6] = [
    "HTTP_PROXY",
    "HTTPS_PROXY",
    "ALL_PROXY",
    "http_proxy",
    "https_proxy",
    "all_proxy",
];
const NO_PROXY_ENV_KEYS: [&str; 2] = ["NO_PROXY", "no_proxy"];
const MAX_PROXY_URL_CHARS: usize = 2_048;
const MAX_BYPASS_CHARS: usize = 2_048;
/// `NO_PROXY` value that disables proxying for every host. Direct mode sets it
/// in addition to clearing the proxy variables so tools that only honour the
/// bypass list cannot fall back to an ambient proxy.
const NO_PROXY_EVERYTHING: &str = "*";
/// Bypass entry meaning "hosts without a domain suffix". It is meaningful to
/// curl, Git, and the Agent runtimes that read `NO_PROXY`, but the in-process
/// HTTP client has no equivalent rule, so it is dropped from the client-side
/// list rather than being treated as a literal host name.
const LOCAL_BYPASS_ENTRY: &str = "<local>";
/// Endpoint the connection test reaches through the configured route. It is the
/// same release feed the updater uses, so a passing test covers the update path
/// the proxy is configured for.
pub const CONNECTION_TEST_URL: &str = "https://github.com/vibex-ai/vibex/releases.atom";
pub const CONNECTION_TEST_TIMEOUT: Duration = Duration::from_secs(15);

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
    /// Proxy variables as the process inherited them, captured before the first
    /// mode is applied so System mode can hand the environment back untouched.
    ambient_env: OnceLock<Vec<(&'static str, Option<String>)>>,
}

fn state() -> &'static ProxyState {
    static STATE: OnceLock<ProxyState> = OnceLock::new();
    STATE.get_or_init(|| ProxyState {
        snapshot: RwLock::new((NetworkProxyUiState::default(), 0)),
        async_client: RwLock::new(None),
        blocking_client: RwLock::new(None),
        ambient_env: OnceLock::new(),
    })
}

fn proxy_env_keys() -> impl Iterator<Item = &'static str> {
    PROXY_ENV_KEYS.into_iter().chain(NO_PROXY_ENV_KEYS)
}

fn capture_ambient_env() {
    state().ambient_env.get_or_init(|| {
        proxy_env_keys()
            .map(|key| (key, std::env::var(key).ok()))
            .collect()
    });
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

/// Collapse a user-typed bypass list into the canonical comma-separated form.
///
/// An empty result is kept: it means "bypass nothing", which is what an emptied
/// field shows. Callers that want the shipped default start from
/// [`DEFAULT_NETWORK_PROXY_BYPASS`](vibex_desktop_model::DEFAULT_NETWORK_PROXY_BYPASS).
pub fn normalize_bypass_list(raw: &str) -> String {
    let mut entries = Vec::new();
    for entry in raw.split(',') {
        let entry = entry.trim();
        if entry.is_empty() || entries.contains(&entry) {
            continue;
        }
        entries.push(entry);
        if entries.join(",").chars().count() >= MAX_BYPASS_CHARS {
            break;
        }
    }
    let joined = entries.join(",");
    joined.chars().take(MAX_BYPASS_CHARS).collect()
}

/// The subset of a bypass list the in-process HTTP client can honour.
///
/// [`LOCAL_BYPASS_ENTRY`] has no client-side rule, so it is removed instead of
/// being matched literally. Everything else — addresses, CIDR blocks, domains,
/// and `*` — is passed through.
fn client_bypass_list(bypass: &str) -> Option<String> {
    let client_entries = bypass
        .split(',')
        .map(str::trim)
        .filter(|entry| !entry.is_empty() && !entry.eq_ignore_ascii_case(LOCAL_BYPASS_ENTRY))
        .collect::<Vec<_>>();
    (!client_entries.is_empty()).then(|| client_entries.join(","))
}

/// Validate and canonicalize a complete settings value.
///
/// A custom proxy without an address is accepted rather than rejected: the mode
/// has to be selectable before the field that holds the address exists, so an
/// unfinished custom configuration is kept and simply routes like System until
/// an address arrives. An address that *is* present must still be valid.
pub fn normalize_settings(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    let proxy_url = settings
        .proxy_url
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(normalize_proxy_url)
        .transpose()?;
    Ok(NetworkProxyUiState {
        mode: settings.mode,
        proxy_url,
        bypass: normalize_bypass_list(&settings.bypass),
    })
}

/// Initialize the process from the persisted preference before the first
/// runtime client is constructed. The ambient proxy environment is captured
/// here so System mode can restore it after a custom proxy was in effect.
pub fn initialize(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    capture_ambient_env();
    let normalized = normalize_settings(settings)?;
    set_snapshot(normalized.clone());
    apply_proxy_env(&normalized);
    Ok(normalized)
}

/// Apply a user-initiated settings change immediately.
pub fn configure(settings: &NetworkProxyUiState) -> Result<NetworkProxyUiState, String> {
    let normalized = normalize_settings(settings)?;
    set_snapshot(normalized.clone());
    apply_proxy_env(&normalized);
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

/// The proxy variables a child process should be spawned with, given a mode.
fn proxy_env(settings: &NetworkProxyUiState) -> Vec<(&'static str, Option<String>)> {
    match settings.mode {
        NetworkProxyMode::System => ambient_proxy_env(),
        NetworkProxyMode::Direct => PROXY_ENV_KEYS
            .into_iter()
            .map(|key| (key, None))
            .chain(
                NO_PROXY_ENV_KEYS
                    .into_iter()
                    .map(|key| (key, Some(NO_PROXY_EVERYTHING.to_string()))),
            )
            .collect(),
        // A custom proxy without an address is not configured yet, so it keeps
        // the route System mode would have taken.
        NetworkProxyMode::Custom => match settings.proxy_url.as_deref() {
            Some(proxy_url) => {
                let bypass = (!settings.bypass.is_empty()).then(|| settings.bypass.clone());
                PROXY_ENV_KEYS
                    .into_iter()
                    .map(|key| (key, Some(proxy_url.to_string())))
                    .chain(
                        NO_PROXY_ENV_KEYS
                            .into_iter()
                            .map(|key| (key, bypass.clone())),
                    )
                    .collect()
            }
            None => ambient_proxy_env(),
        },
    }
}

fn ambient_proxy_env() -> Vec<(&'static str, Option<String>)> {
    state()
        .ambient_env
        .get()
        .map(|ambient| {
            ambient
                .iter()
                .map(|(key, value)| (*key, value.clone()))
                .collect()
        })
        .unwrap_or_else(|| proxy_env_keys().map(|key| (key, None)).collect())
}

fn apply_proxy_env(settings: &NetworkProxyUiState) {
    capture_ambient_env();
    for (key, value) in proxy_env(settings) {
        match value {
            // Environment mutation happens only at explicit native app
            // boundaries, before child/client work is spawned.
            Some(value) => unsafe { std::env::set_var(key, value) },
            None => unsafe { std::env::remove_var(key) },
        }
    }
}

fn build_proxy(proxy_url: &str, bypass: &str) -> Result<reqwest::Proxy, String> {
    let proxy =
        reqwest::Proxy::all(proxy_url).map_err(|_| "proxy address is invalid".to_string())?;
    Ok(match client_bypass_list(bypass) {
        Some(bypass) => proxy.no_proxy(reqwest::NoProxy::from_string(&bypass)),
        None => proxy,
    })
}

/// The proxy controls both `reqwest` builder flavours expose.
trait ProxyClientBuilder: Sized {
    fn without_proxy(self) -> Self;
    fn with_proxy(self, proxy: reqwest::Proxy) -> Self;
}

impl ProxyClientBuilder for reqwest::ClientBuilder {
    fn without_proxy(self) -> Self {
        self.no_proxy()
    }

    fn with_proxy(self, proxy: reqwest::Proxy) -> Self {
        self.proxy(proxy)
    }
}

impl ProxyClientBuilder for reqwest::blocking::ClientBuilder {
    fn without_proxy(self) -> Self {
        self.no_proxy()
    }

    fn with_proxy(self, proxy: reqwest::Proxy) -> Self {
        self.proxy(proxy)
    }
}

/// Apply a mode to a client builder.
///
/// System mode adds nothing: `reqwest` then resolves the operating system
/// configuration and the process environment on its own. Direct mode disables
/// that discovery, and Custom mode replaces it with the configured proxy — or
/// leaves discovery alone while a custom proxy has no address yet.
fn apply_mode<B: ProxyClientBuilder>(
    builder: B,
    settings: &NetworkProxyUiState,
) -> Result<B, String> {
    match settings.mode {
        NetworkProxyMode::System => Ok(builder),
        NetworkProxyMode::Direct => Ok(builder.without_proxy()),
        NetworkProxyMode::Custom => match settings.proxy_url.as_deref() {
            Some(proxy_url) => Ok(builder
                .without_proxy()
                .with_proxy(build_proxy(proxy_url, &settings.bypass)?)),
            None => Ok(builder),
        },
    }
}

pub fn async_client_builder() -> Result<reqwest::ClientBuilder, String> {
    let (settings, _) = current_snapshot();
    apply_mode(Client::builder(), &settings)
}

pub fn blocking_client_builder() -> Result<reqwest::blocking::ClientBuilder, String> {
    let (settings, _) = current_snapshot();
    apply_mode(reqwest::blocking::Client::builder(), &settings)
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
    proxy_env(&settings)
        .into_iter()
        .filter_map(|(key, value)| value.map(|value| (key.to_string(), value)))
        .collect()
}

pub fn current_proxy_url() -> Result<Option<Url>, String> {
    let settings = current_settings();
    if settings.mode != NetworkProxyMode::Custom {
        return Ok(None);
    }
    settings
        .proxy_url
        .as_deref()
        .map(Url::parse)
        .transpose()
        .map_err(|_| "proxy address is invalid".to_string())
}

/// Reach [`CONNECTION_TEST_URL`] through an explicit, not-yet-saved
/// configuration and report how long the round trip took.
///
/// The test never touches the process-wide snapshot or the environment, so a
/// failed attempt leaves the running configuration exactly as it was. A URL
/// that does not build is reported before any network work starts.
pub async fn test_connection(settings: &NetworkProxyUiState) -> Result<Duration, String> {
    let normalized = normalize_settings(settings)?;
    if normalized.mode == NetworkProxyMode::Custom && normalized.proxy_url.is_none() {
        return Err("proxy address is required for a custom proxy".to_string());
    }
    let client = apply_mode(Client::builder(), &normalized)?
        .timeout(CONNECTION_TEST_TIMEOUT)
        .redirect(reqwest::redirect::Policy::none())
        .user_agent(concat!("Vibex/", env!("CARGO_PKG_VERSION")))
        .build()
        .map_err(|_| "the network proxy HTTP client could not be initialized".to_string())?;
    let started = Instant::now();
    let response = client
        .get(CONNECTION_TEST_URL)
        .send()
        .await
        .map_err(|error| {
            if error.is_timeout() {
                "the connection test timed out".to_string()
            } else if error.is_connect() {
                "the proxy could not be reached".to_string()
            } else {
                "the connection test failed".to_string()
            }
        })?;
    let status = response.status();
    if !status.is_success() && !status.is_redirection() {
        return Err(match status.as_u16() {
            407 => "the proxy rejected the credentials".to_string(),
            _ => "the connection test returned an unexpected response".to_string(),
        });
    }
    Ok(started.elapsed())
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_desktop_model::DEFAULT_NETWORK_PROXY_BYPASS;

    fn custom(proxy_url: &str) -> NetworkProxyUiState {
        NetworkProxyUiState {
            mode: NetworkProxyMode::Custom,
            proxy_url: Some(proxy_url.to_string()),
            bypass: DEFAULT_NETWORK_PROXY_BYPASS.to_string(),
        }
    }

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
    fn every_mode_may_carry_no_address_and_a_present_one_must_be_valid() {
        for mode in [
            NetworkProxyMode::System,
            NetworkProxyMode::Direct,
            NetworkProxyMode::Custom,
        ] {
            let empty = NetworkProxyUiState {
                mode,
                proxy_url: Some("   ".to_string()),
                bypass: DEFAULT_NETWORK_PROXY_BYPASS.to_string(),
            };
            let normalized = normalize_settings(&empty).unwrap();
            assert_eq!(normalized.mode, mode);
            assert!(normalized.proxy_url.is_none());

            assert!(
                normalize_settings(&NetworkProxyUiState {
                    proxy_url: Some("ftp://proxy.local:21".to_string()),
                    ..empty
                })
                .is_err()
            );
        }
    }

    #[test]
    fn an_unfinished_custom_proxy_is_kept_and_routes_like_system() {
        let settings = NetworkProxyUiState {
            mode: NetworkProxyMode::Custom,
            proxy_url: None,
            bypass: String::new(),
        };
        let normalized = normalize_settings(&settings).unwrap();
        assert_eq!(normalized.mode, NetworkProxyMode::Custom);
        assert!(normalized.proxy_url.is_none());

        // The mode has to stay selectable so the address field can appear; the
        // route simply stays on the system one until an address arrives.
        assert!(apply_mode(Client::builder(), &normalized).is_ok());
        assert!(
            proxy_env(&normalized)
                .iter()
                .all(|(key, value)| value.is_none() || !PROXY_ENV_KEYS.contains(key))
        );
    }

    #[test]
    fn a_mode_keeps_its_dormant_proxy_address() {
        let settings = NetworkProxyUiState {
            mode: NetworkProxyMode::System,
            proxy_url: Some(" socks5://127.0.0.1:1080 ".to_string()),
            bypass: DEFAULT_NETWORK_PROXY_BYPASS.to_string(),
        };
        assert_eq!(
            normalize_settings(&settings).unwrap().proxy_url.as_deref(),
            Some("socks5://127.0.0.1:1080")
        );
    }

    #[test]
    fn bypass_lists_are_trimmed_deduplicated_and_may_be_empty() {
        assert_eq!(
            normalize_bypass_list(" localhost , 127.0.0.1 ,, localhost "),
            "localhost,127.0.0.1"
        );
        assert_eq!(normalize_bypass_list("   "), "");
    }

    #[test]
    fn the_client_drops_only_the_local_placeholder() {
        assert_eq!(
            client_bypass_list("localhost,127.0.0.1,::1,<local>").as_deref(),
            Some("localhost,127.0.0.1,::1")
        );
        assert_eq!(
            client_bypass_list("*.internal,<LOCAL>").as_deref(),
            Some("*.internal")
        );
        assert_eq!(client_bypass_list("<local>"), None);
    }

    #[test]
    fn system_mode_keeps_the_client_on_system_discovery() {
        let builder = apply_mode(
            Client::builder(),
            &NetworkProxyUiState {
                mode: NetworkProxyMode::System,
                proxy_url: Some("http://127.0.0.1:7890".to_string()),
                bypass: DEFAULT_NETWORK_PROXY_BYPASS.to_string(),
            },
        )
        .unwrap();
        // A system-mode builder must stay buildable even though the dormant
        // address is present, and must not be rejected for it.
        assert!(builder.build().is_ok());
    }

    #[test]
    fn custom_mode_without_an_address_keeps_system_discovery() {
        let settings = NetworkProxyUiState {
            mode: NetworkProxyMode::Custom,
            proxy_url: None,
            bypass: String::new(),
        };
        assert!(apply_mode(Client::builder(), &settings).is_ok());
        assert!(normalize_settings(&settings).is_ok());
        // An address that is present but unusable is still an error.
        assert!(normalize_settings(&custom("ftp://proxy.local:21")).is_err());
    }

    #[test]
    fn every_mode_produces_a_coherent_environment() {
        let direct = NetworkProxyUiState {
            mode: NetworkProxyMode::Direct,
            proxy_url: None,
            bypass: String::new(),
        };
        let direct_env = proxy_env(&direct);
        assert!(direct_env.iter().all(|(key, value)| {
            if NO_PROXY_ENV_KEYS.contains(key) {
                value.as_deref() == Some(NO_PROXY_EVERYTHING)
            } else {
                value.is_none()
            }
        }));

        let custom_env = proxy_env(&custom("socks5://127.0.0.1:1080"));
        assert!(custom_env.iter().all(|(key, value)| {
            if NO_PROXY_ENV_KEYS.contains(key) {
                value.as_deref() == Some(DEFAULT_NETWORK_PROXY_BYPASS)
            } else {
                value.as_deref() == Some("socks5://127.0.0.1:1080")
            }
        }));

        let empty_bypass = NetworkProxyUiState {
            bypass: String::new(),
            ..custom("socks5://127.0.0.1:1080")
        };
        assert!(
            proxy_env(&empty_bypass)
                .iter()
                .filter(|(key, _)| NO_PROXY_ENV_KEYS.contains(key))
                .all(|(_, value)| value.is_none())
        );
    }
}

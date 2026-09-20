//! Marketplace catalog fetching and installation.
//!
//! This module owns the only outbound network calls the Config Center makes on
//! behalf of a market. Two rules shape everything below.
//!
//! **The authoritative owner fetches.** The desktop renders catalog values; it
//! never fetches one itself. That keeps a paired device from becoming a second
//! network client with its own proxy and TLS story, and it means a catalog is
//! fetched once for every client of this runtime.
//!
//! **A source fails alone.** A search merges several sources, and any one of
//! them may be slow, unreachable, or malformed. None of those may take the
//! whole search down: each failure is returned in `failed_sources` naming the
//! host that refused, so the UI can explain it instead of showing an empty list.
//!
//! ## Network boundary
//!
//! Sources are user-configurable, so a fixed host allowlist is not available
//! here the way it is for the update feed. The boundary is instead:
//!
//! - credentials-free `https` only, at the source and at every redirect hop;
//! - literal loopback, private, link-local, and otherwise non-public addresses
//!   are refused, as are `localhost`-style names;
//! - redirects are followed manually, at most five, each hop re-validated;
//! - response bodies are capped, and every request shares one deadline.
//!
//! Residual risk: the hostname is resolved by the HTTP client after this
//! validation, so a name that answers publicly during the check and privately
//! at connect time is not fully excluded. Closing that would require pinning
//! the resolved address into the socket, which the blocking client used here
//! does not expose.

use std::io::Read;
use std::net::IpAddr;
use std::path::{Path, PathBuf};
use std::time::Duration;

use serde::Deserialize;
use vibex_core::{
    AgentId, MAX_MARKET_RESPONSE_BYTES, MAX_MARKET_SOURCES_PER_SEARCH,
    MAX_SKILL_MARKET_DOCUMENT_BYTES, MarketEnvRequirement, MarketSource, MarketSourceFailure,
    MarketSourceKind, MarketSourceListResponse, MarketSourceSetRequest, McpMarketCategory,
    McpMarketEntry, McpMarketEntryRequest, McpMarketInstallRequest, McpMarketInstallResult,
    McpMarketSearchRequest, McpMarketSearchResponse, McpServerCreateRequest, McpServerEnvEntry,
    McpServerScopeKind, McpServerStatus, McpServerTransportKind, SkillCreateRequest,
    SkillMarketCategory, SkillMarketDocument, SkillMarketDocumentRequest, SkillMarketEntry,
    SkillMarketInstallRequest, SkillMarketInstallResult, SkillMarketSearchRequest,
    SkillMarketSearchResponse, SkillScopeKind, SkillSourceKind, SkillStatus, VibexError,
    VibexResult,
};

use vibex_db::{McpServerRepository, SkillRepository};

use crate::{
    ProviderConfigService, diagnostic, find_existing_mcp_server, normalize_mcp_create_request,
    normalize_skill_create_request, validate_mcp_create_request, validate_skill_create_request,
};

/// One request may not take longer than this, including every redirect.
const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Redirect hops followed before the fetch is refused.
const MAX_MARKET_REDIRECTS: usize = 5;
/// Entries a single source may contribute, so one huge catalog cannot flood the
/// merged list.
const MAX_ENTRIES_PER_SOURCE: usize = 200;
/// A Skill document is markdown; anything past this is not one.
const MAX_SKILL_DOCUMENT_FETCH_BYTES: u64 = MAX_SKILL_MARKET_DOCUMENT_BYTES + 1;

/// Sources that ship with the app.
///
/// They are the offline floor: when every user source is unreachable these
/// still render, which is what keeps the market useful on a first run with no
/// configuration.
fn builtin_market_sources() -> Vec<MarketSource> {
    vec![
        MarketSource {
            id: "official-mcp-registry".to_string(),
            name: "Official MCP Registry".to_string(),
            url: "https://registry.modelcontextprotocol.io".to_string(),
            kind: MarketSourceKind::McpRegistry,
            builtin: true,
        },
        MarketSource {
            id: "builtin-skills".to_string(),
            name: "Vibex Skill Picks".to_string(),
            url: "builtin://skills".to_string(),
            kind: MarketSourceKind::SkillCatalog,
            builtin: true,
        },
    ]
}

/// User sources live beside the database rather than in it: they are small,
/// non-relational, and read on every market open, so a file keeps the schema
/// untouched.
fn user_market_sources_path(db_path: &Path) -> PathBuf {
    db_path
        .parent()
        .map(|parent| parent.join("market_sources.json"))
        .unwrap_or_else(|| PathBuf::from("market_sources.json"))
}

fn load_user_market_sources(db_path: &Path) -> Vec<MarketSource> {
    let path = user_market_sources_path(db_path);
    let Ok(raw) = std::fs::read_to_string(&path) else {
        return Vec::new();
    };
    let Ok(value) = serde_json::from_str::<serde_json::Value>(&raw) else {
        return Vec::new();
    };
    sanitize_market_sources(value)
}

/// Repair whatever was persisted into a usable source list.
///
/// Anything that is not a credentials-free `https` URL, or that repeats an id,
/// is dropped rather than surfaced: a source the fetcher would refuse anyway
/// must not appear in the UI as a working one.
fn sanitize_market_sources(value: serde_json::Value) -> Vec<MarketSource> {
    let raw = value
        .get("sources")
        .and_then(serde_json::Value::as_array)
        .cloned()
        .unwrap_or_default();
    let mut seen = std::collections::HashSet::new();
    let mut sources = Vec::new();
    for item in raw {
        let Some(object) = item.as_object() else {
            continue;
        };
        let id = object.get("id").and_then(serde_json::Value::as_str);
        let name = object.get("name").and_then(serde_json::Value::as_str);
        let url = object.get("url").and_then(serde_json::Value::as_str);
        let kind = object
            .get("kind")
            .and_then(serde_json::Value::as_str)
            .and_then(MarketSourceKind::parse);
        let (Some(id), Some(name), Some(url), Some(kind)) = (id, name, url, kind) else {
            continue;
        };
        if !market_source_url_is_allowed(kind, url) || !seen.insert(id.to_string()) {
            continue;
        }
        sources.push(MarketSource {
            id: id.chars().take(64).collect(),
            name: name.chars().take(64).collect(),
            url: url.to_string(),
            kind,
            builtin: false,
        });
    }
    sources
}

/// A builtin skill catalog is compiled in, so it needs no URL to be valid.
fn market_source_url_is_allowed(kind: MarketSourceKind, url: &str) -> bool {
    if url.starts_with("builtin://") {
        return kind == MarketSourceKind::SkillCatalog;
    }
    market_url_policy(url).is_ok()
}

// ---------------------------------------------------------------------------
// Fetch policy
// ---------------------------------------------------------------------------

fn is_public_ip(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(v4) => {
            !(v4.is_loopback()
                || v4.is_private()
                || v4.is_link_local()
                || v4.is_broadcast()
                || v4.is_documentation()
                || v4.is_unspecified()
                || v4.is_multicast()
                // 100.64.0.0/10 carrier-grade NAT.
                || (v4.octets()[0] == 100 && (64..128).contains(&v4.octets()[1]))
                // 192.0.0.0/24 IETF protocol assignments.
                || (v4.octets()[0] == 192 && v4.octets()[1] == 0 && v4.octets()[2] == 0))
        }
        IpAddr::V6(v6) => {
            // IPv4-mapped and IPv4-compatible addresses reach IPv4 space.
            if let Some(mapped) = v6.to_ipv4_mapped() {
                return is_public_ip(IpAddr::V4(mapped));
            }
            !(v6.is_loopback()
                || v6.is_unspecified()
                || v6.is_multicast()
                // Unique local fc00::/7.
                || (v6.segments()[0] & 0xfe00) == 0xfc00
                // Link local fe80::/10.
                || (v6.segments()[0] & 0xffc0) == 0xfe80)
        }
    }
}

/// Validate a market URL without performing any I/O.
///
/// A trailing dot on the host is stripped before comparison: `localhost.` is
/// the same name as `localhost`, and the dot form is a classic way to slip past
/// a suffix check.
fn market_url_policy(raw: &str) -> Result<reqwest::Url, VibexError> {
    let url = reqwest::Url::parse(raw.trim()).map_err(|_| {
        VibexError::validation(
            "market_source_url_invalid",
            "market source URL does not parse",
        )
    })?;
    if url.scheme() != "https" {
        return Err(VibexError::validation(
            "market_source_url_insecure",
            "market sources must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(VibexError::validation(
            "market_source_url_credentials",
            "market sources must not carry credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(VibexError::validation(
            "market_source_url_host_missing",
            "market source URL has no host",
        ));
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(VibexError::validation(
                "market_source_url_not_public",
                "market source URL must resolve to a public address",
            ));
        }
    } else if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || !host.contains('.')
    {
        return Err(VibexError::validation(
            "market_source_url_not_public",
            "market source URL must be a public host",
        ));
    }
    Ok(url)
}

fn market_http_client() -> VibexResult<reqwest::blocking::Client> {
    reqwest::blocking::Client::builder()
        .timeout(MARKET_REQUEST_TIMEOUT)
        .user_agent(concat!("Vibex/", env!("CARGO_PKG_VERSION"), " market"))
        // Redirects are followed by hand so every hop is re-validated; letting
        // the client follow them would skip the policy on hops 2..n.
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|error| {
            VibexError::capability(
                "market_http_client_unavailable",
                "the market network client could not be initialized",
            )
            .with_diagnostic("error", error.to_string())
        })
}

/// Fetch one market URL under the boundary described in the module docs.
fn fetch_market_bytes(
    client: &reqwest::blocking::Client,
    raw_url: &str,
    limit: u64,
) -> VibexResult<(Vec<u8>, String)> {
    let mut url = market_url_policy(raw_url)?;
    let mut hops = 0usize;
    loop {
        let response = client.get(url.clone()).send().map_err(|error| {
            market_source_failure_error(&url, "market_source_unreachable", error.to_string())
        })?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let Some(location) = location else {
                return Err(VibexError::provider(
                    "market_source_redirect_invalid",
                    "market source redirected without a location",
                ));
            };
            hops += 1;
            if hops > MAX_MARKET_REDIRECTS {
                return Err(VibexError::provider(
                    "market_source_redirect_limit",
                    "market source redirected too many times",
                ));
            }
            // Resolve relative redirects against the hop that produced them,
            // then re-run the whole policy on the result.
            let next = url.join(&location).map_err(|_| {
                VibexError::provider(
                    "market_source_redirect_invalid",
                    "market source redirected to an invalid location",
                )
            })?;
            url = market_url_policy(next.as_str())?;
            continue;
        }
        if !status.is_success() {
            return Err(VibexError::provider(
                "market_source_rejected",
                format!("market source responded with HTTP {}", status.as_u16()),
            )
            .with_diagnostic("host", url.host_str().unwrap_or_default()));
        }
        if let Some(length) = response.content_length()
            && length > limit
        {
            return Err(VibexError::provider(
                "market_source_too_large",
                "market source response exceeded the size limit",
            ));
        }
        let host = url.host_str().unwrap_or_default().to_string();
        let mut body = Vec::new();
        // `take` bounds the read even when the response lies about its length.
        response
            .take(limit)
            .read_to_end(&mut body)
            .map_err(|error| {
                VibexError::provider("market_source_read_failed", error.to_string())
            })?;
        if body.len() as u64 >= limit {
            return Err(VibexError::provider(
                "market_source_too_large",
                "market source response exceeded the size limit",
            ));
        }
        return Ok((body, host));
    }
}

fn market_source_failure_error(
    url: &reqwest::Url,
    code: &'static str,
    message: String,
) -> VibexError {
    VibexError::provider(code, message)
        .with_recovery_hint("Check the source URL and the network connection")
        .with_diagnostic("host", url.host_str().unwrap_or_default())
}

fn fetch_market_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::blocking::Client,
    url: &str,
) -> VibexResult<T> {
    let (body, host) = fetch_market_bytes(client, url, MAX_MARKET_RESPONSE_BYTES)?;
    serde_json::from_slice(&body).map_err(|error| {
        VibexError::provider("market_source_malformed", error.to_string())
            .with_diagnostic("host", host)
    })
}

fn source_failure(
    source: &MarketSource,
    error: &VibexError,
    fallback_host: Option<String>,
) -> MarketSourceFailure {
    let host = error
        .diagnostics
        .iter()
        .find(|entry| entry.key == "host")
        .map(|entry| entry.value.clone())
        .or(fallback_host);
    MarketSourceFailure {
        source_id: source.id.clone(),
        source_name: source.name.clone(),
        code: error.code.clone(),
        message: error.message.clone(),
        host,
    }
}

// ---------------------------------------------------------------------------
// Source listing
// ---------------------------------------------------------------------------

impl ProviderConfigService {
    pub fn market_sources(&self) -> VibexResult<MarketSourceListResponse> {
        let mut sources = builtin_market_sources();
        sources.extend(load_user_market_sources(self.database_path()));
        Ok(MarketSourceListResponse { sources })
    }

    /// Replace the user source list. Builtin sources are always retained, so a
    /// client cannot delete the offline floor by omission.
    pub fn set_market_sources(
        &self,
        request: MarketSourceSetRequest,
    ) -> VibexResult<MarketSourceListResponse> {
        let mut sanitized: Vec<MarketSource> = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for source in request.sources {
            let trimmed = source.url.trim().to_string();
            if !market_source_url_is_allowed(source.kind, &trimmed) {
                return Err(VibexError::validation(
                    "market_source_url_invalid",
                    "market source URL is not an allowed public https URL",
                )
                .with_diagnostic("sourceId", source.id.clone()));
            }
            if !seen.insert(source.id.clone()) {
                continue;
            }
            sanitized.push(MarketSource {
                id: source.id.chars().take(64).collect(),
                name: source.name.chars().take(64).collect(),
                url: trimmed,
                kind: source.kind,
                builtin: false,
            });
        }
        let payload = serde_json::json!({ "sources": sanitized });
        let path = user_market_sources_path(self.database_path());
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).map_err(|error| {
                VibexError::storage("market_sources_write_failed", error.to_string())
            })?;
        }
        let encoded = serde_json::to_vec_pretty(&payload).map_err(|error| {
            VibexError::storage("market_sources_encode_failed", error.to_string())
        })?;
        std::fs::write(&path, encoded).map_err(|error| {
            VibexError::storage("market_sources_write_failed", error.to_string())
        })?;
        self.market_sources()
    }

    /// Every source a search should visit, restricted to the requested kind.
    fn market_sources_for_kind(
        &self,
        requested: &[String],
        mcp: bool,
    ) -> VibexResult<Vec<MarketSource>> {
        let all = self.market_sources()?.sources;
        let mut selected: Vec<MarketSource> = all
            .into_iter()
            .filter(|source| {
                if mcp {
                    source.kind.is_mcp()
                } else {
                    source.kind.is_skill()
                }
            })
            .filter(|source| requested.is_empty() || requested.iter().any(|id| id == &source.id))
            .collect();
        selected.truncate(MAX_MARKET_SOURCES_PER_SEARCH);
        Ok(selected)
    }
}

// ---------------------------------------------------------------------------
// MCP registry adapter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct RegistryListResponse {
    #[serde(default)]
    servers: Vec<RegistryServerRecord>,
    #[serde(default)]
    metadata: Option<RegistryMetadata>,
}

#[derive(Debug, Deserialize)]
struct RegistryMetadata {
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryServerRecord {
    #[serde(default)]
    server: Option<RegistryServer>,
}

#[derive(Debug, Deserialize)]
struct RegistryServer {
    name: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    packages: Vec<RegistryPackage>,
    #[serde(default)]
    remotes: Vec<RegistryRemote>,
}

#[derive(Debug, Deserialize)]
struct RegistryPackage {
    #[serde(default, rename = "registryType")]
    registry_type: Option<String>,
    #[serde(default)]
    identifier: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "runtimeHint")]
    runtime_hint: Option<String>,
    #[serde(default, rename = "runtimeArguments")]
    runtime_arguments: Option<Vec<RegistryArgument>>,
    #[serde(default, rename = "packageArguments")]
    package_arguments: Option<Vec<RegistryArgument>>,
    #[serde(default, rename = "environmentVariables")]
    environment_variables: Option<Vec<RegistryEnvVar>>,
}

#[derive(Debug, Deserialize)]
struct RegistryArgument {
    #[serde(default)]
    value: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryEnvVar {
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default, rename = "isRequired")]
    is_required: Option<bool>,
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    default: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RegistryRemote {
    #[serde(default, rename = "type")]
    remote_type: Option<String>,
    #[serde(default)]
    url: Option<String>,
}

/// `com.pulsemcp/foo` becomes `com-pulsemcp-foo`, matching the id shape the
/// rest of the product uses.
fn registry_entry_id(name: &str) -> String {
    let slug = name
        .to_lowercase()
        .split('/')
        .map(|part| {
            part.chars()
                .map(|ch| if ch.is_ascii_alphanumeric() { ch } else { '-' })
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("-");
    let mut collapsed = String::new();
    let mut last_dash = false;
    for ch in slug.chars() {
        if ch == '-' {
            if !last_dash && !collapsed.is_empty() {
                collapsed.push('-');
            }
            last_dash = true;
        } else {
            collapsed.push(ch);
            last_dash = false;
        }
    }
    let trimmed = collapsed.trim_matches('-');
    if trimmed.is_empty() {
        "mcp-server".to_string()
    } else {
        trimmed.chars().take(60).collect()
    }
}

fn registry_arg_values(args: Option<&Vec<RegistryArgument>>) -> Vec<String> {
    args.into_iter()
        .flatten()
        .filter_map(|argument| argument.value.as_deref())
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .collect()
}

/// Package identifier with the published version pinned, so an install resolves
/// the version the registry described rather than mutable latest content.
fn registry_package_identifier(package: &RegistryPackage, runtime: &str) -> Option<String> {
    let identifier = package.identifier.as_deref()?.trim();
    if identifier.is_empty() {
        return None;
    }
    let version = package
        .version
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty() && *value != "latest");
    match (runtime, version) {
        ("uvx", Some(version)) if package.registry_type.as_deref() == Some("pypi") => {
            Some(format!("{identifier}=={version}"))
        }
        // A scoped name (`@scope/pkg`) carries a leading `@` that is not a
        // version separator, so only a second `@` means "already pinned".
        ("npx", Some(version)) if !identifier[1.min(identifier.len())..].contains('@') => {
            Some(format!("{identifier}@{version}"))
        }
        _ => Some(identifier.to_string()),
    }
}

fn registry_env_requirements(package: &RegistryPackage) -> Vec<MarketEnvRequirement> {
    package
        .environment_variables
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|variable| {
            let name = variable.name.as_deref()?.trim();
            if name.is_empty() {
                return None;
            }
            Some(MarketEnvRequirement {
                name: name.to_string(),
                description: variable
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                required: variable.is_required.unwrap_or(false),
                // A registry does not label secrets; the install form masks
                // anything whose name reads like a credential.
                secret: env_name_looks_secret(name),
                default_value: variable
                    .value
                    .as_deref()
                    .or(variable.default.as_deref())
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string),
                placeholder: None,
            })
        })
        .collect()
}

fn env_name_looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
        .iter()
        .any(|needle| upper.contains(needle))
}

fn registry_server_to_entry(source_id: &str, server: &RegistryServer) -> Option<McpMarketEntry> {
    let display = server
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(server.name.as_str())
        .to_string();

    // Prefer a stdio package: it is the form every agent can host.
    if let Some(package) = server.packages.first() {
        let runtime = package
            .runtime_hint
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| match package.registry_type.as_deref() {
                Some("pypi") => "uvx".to_string(),
                _ => "npx".to_string(),
            });
        let identifier = registry_package_identifier(package, &runtime)?;
        let mut args = registry_arg_values(package.runtime_arguments.as_ref());
        args.push("-y".to_string());
        args.extend(registry_arg_values(package.package_arguments.as_ref()));
        args.push(identifier);
        return Some(McpMarketEntry {
            id: registry_entry_id(&server.name),
            source_id: source_id.to_string(),
            name: display,
            description: server.description.clone(),
            homepage: server.homepage.clone(),
            categories: Vec::new(),
            transport: McpServerTransportKind::Stdio,
            command: Some(runtime),
            args,
            url: None,
            env: registry_env_requirements(package),
            verified: true,
            version: package.version.clone(),
            author: server.name.split('/').next().map(str::to_string),
        });
    }

    // Otherwise fall back to a remote, and only to the streamable-http form:
    // an install must not resolve to a transport the product cannot start.
    let remote = server.remotes.iter().find(|remote| {
        remote.remote_type.as_deref() == Some("streamable-http")
            && remote
                .url
                .as_deref()
                .is_some_and(|url| market_url_policy(url).is_ok())
    })?;
    Some(McpMarketEntry {
        id: registry_entry_id(&server.name),
        source_id: source_id.to_string(),
        name: display,
        description: server.description.clone(),
        homepage: server.homepage.clone(),
        categories: Vec::new(),
        transport: McpServerTransportKind::Http,
        command: None,
        args: Vec::new(),
        url: remote.url.clone(),
        env: Vec::new(),
        verified: true,
        version: None,
        author: server.name.split('/').next().map(str::to_string),
    })
}

fn search_mcp_registry(
    client: &reqwest::blocking::Client,
    source: &MarketSource,
    query: Option<&str>,
    limit: u32,
) -> Result<(Vec<McpMarketEntry>, bool), MarketSourceFailure> {
    let base = source.url.trim_end_matches('/');
    let mut url = format!("{base}/v0.1/servers?version=latest&limit={limit}");
    if let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) {
        url.push_str("&search=");
        url.push_str(&urlencode(query));
    }
    let response: RegistryListResponse =
        fetch_market_json(client, &url).map_err(|error| source_failure(source, &error, None))?;
    let has_more = response
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.next_cursor.as_deref())
        .is_some_and(|cursor| !cursor.is_empty());
    let entries = response
        .servers
        .iter()
        .filter_map(|record| record.server.as_ref())
        .filter_map(|server| registry_server_to_entry(&source.id, server))
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect();
    Ok((entries, has_more))
}

fn urlencode(value: &str) -> String {
    let mut out = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            b' ' => out.push('+'),
            other => out.push_str(&format!("%{other:02X}")),
        }
    }
    out
}

// ---------------------------------------------------------------------------
// Static catalog adapters
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct McpCatalogFile {
    #[serde(default)]
    servers: Vec<McpCatalogEntryRecord>,
}

#[derive(Debug, Deserialize)]
struct McpCatalogEntryRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    transport: Option<String>,
    #[serde(default)]
    command: Option<String>,
    #[serde(default)]
    args: Vec<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    env: Vec<McpCatalogEnvRecord>,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    version: Option<String>,
    #[serde(default)]
    author: Option<String>,
}

#[derive(Debug, Deserialize)]
struct McpCatalogEnvRecord {
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    required: bool,
    #[serde(default)]
    secret: bool,
    #[serde(default)]
    default_value: Option<String>,
    #[serde(default)]
    placeholder: Option<String>,
}

fn parse_mcp_catalog(source: &MarketSource, body: &[u8]) -> Vec<McpMarketEntry> {
    let Ok(file) = serde_json::from_slice::<McpCatalogFile>(body) else {
        return Vec::new();
    };
    file.servers
        .into_iter()
        .filter_map(|record| {
            let transport = match record.transport.as_deref().unwrap_or("stdio") {
                "stdio" => McpServerTransportKind::Stdio,
                "http" => McpServerTransportKind::Http,
                "sse" => McpServerTransportKind::Sse,
                _ => return None,
            };
            // A template the market could not start must never be listed.
            if transport == McpServerTransportKind::Stdio
                && record
                    .command
                    .as_deref()
                    .is_none_or(|command| command.trim().is_empty())
            {
                return None;
            }
            if transport != McpServerTransportKind::Stdio
                && !record
                    .url
                    .as_deref()
                    .is_some_and(|url| market_url_policy(url).is_ok())
            {
                return None;
            }
            Some(McpMarketEntry {
                id: record.id,
                source_id: source.id.clone(),
                name: record.name,
                description: record.description,
                homepage: record.homepage,
                categories: record
                    .categories
                    .iter()
                    .filter_map(|value| McpMarketCategory::parse(value))
                    .collect(),
                transport,
                command: record.command,
                args: record.args,
                url: record.url,
                env: record
                    .env
                    .into_iter()
                    .map(|env| MarketEnvRequirement {
                        name: env.name,
                        description: env.description,
                        required: env.required,
                        secret: env.secret,
                        default_value: env.default_value,
                        placeholder: env.placeholder,
                    })
                    .collect(),
                verified: record.verified,
                version: record.version,
                author: record.author,
            })
        })
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect()
}

#[derive(Debug, Deserialize)]
struct SkillCatalogFile {
    #[serde(default)]
    skills: Vec<SkillCatalogEntryRecord>,
}

#[derive(Debug, Deserialize)]
struct SkillCatalogEntryRecord {
    id: String,
    name: String,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    author: Option<String>,
    #[serde(default)]
    homepage: Option<String>,
    #[serde(default)]
    url: Option<String>,
    #[serde(default)]
    document_url: Option<String>,
    #[serde(default)]
    categories: Vec<String>,
    #[serde(default)]
    verified: bool,
    #[serde(default)]
    version: Option<String>,
}

fn parse_skill_catalog(source: &MarketSource, body: &[u8]) -> Vec<SkillMarketEntry> {
    let Ok(file) = serde_json::from_slice::<SkillCatalogFile>(body) else {
        return Vec::new();
    };
    file.skills
        .into_iter()
        .filter_map(|record| {
            let document_url = record
                .document_url
                .as_deref()
                .or(record.url.as_deref())
                .map(str::trim)
                .filter(|value| !value.is_empty())?;
            // The document is fetched later, so a URL the fetcher would refuse
            // must not be offered as installable.
            if market_url_policy(document_url).is_err() {
                return None;
            }
            Some(SkillMarketEntry {
                id: record.id,
                source_id: source.id.clone(),
                name: record.name,
                description: record.description,
                homepage: record.homepage,
                categories: record
                    .categories
                    .iter()
                    .filter_map(|value| SkillMarketCategory::parse(value))
                    .collect(),
                document_url: document_url.to_string(),
                verified: record.verified,
                version: record.version,
                author: record.author,
            })
        })
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect()
}

/// The compiled-in skill picks.
///
/// Deliberately small: it exists so the market renders something useful on a
/// first run with no configuration, not to be the shelf. The volume comes from
/// configured sources.
fn builtin_skill_catalog() -> Vec<SkillMarketEntry> {
    const SOURCE_ID: &str = "builtin-skills";
    let picks: [(&str, &str, &str, &str, SkillMarketCategory); 4] = [
        (
            "docx",
            "Word documents",
            "Create, read, and edit Word (.docx) files with tracked changes and comments",
            "https://cdn.jsdelivr.net/gh/anthropics/skills@main/skills/docx/SKILL.md",
            SkillMarketCategory::Docs,
        ),
        (
            "pdf",
            "PDF files",
            "Read, extract, merge, split, and generate PDF files",
            "https://cdn.jsdelivr.net/gh/anthropics/skills@main/skills/pdf/SKILL.md",
            SkillMarketCategory::Docs,
        ),
        (
            "xlsx",
            "Excel spreadsheets",
            "Work with spreadsheets: formulas, charts, pivots, and multiple sheets",
            "https://cdn.jsdelivr.net/gh/anthropics/skills@main/skills/xlsx/SKILL.md",
            SkillMarketCategory::Data,
        ),
        (
            "pptx",
            "PowerPoint decks",
            "Create, edit, and analyze PowerPoint (.pptx) presentations",
            "https://cdn.jsdelivr.net/gh/anthropics/skills@main/skills/pptx/SKILL.md",
            SkillMarketCategory::Docs,
        ),
    ];
    picks
        .into_iter()
        .map(|(id, name, description, url, category)| SkillMarketEntry {
            id: id.to_string(),
            source_id: SOURCE_ID.to_string(),
            name: name.to_string(),
            description: Some(description.to_string()),
            homepage: None,
            categories: vec![category],
            document_url: url.to_string(),
            verified: true,
            version: None,
            author: Some("anthropic".to_string()),
        })
        .collect()
}

// ---------------------------------------------------------------------------
// GitHub repository scan
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct GitHubRepo {
    #[serde(default)]
    default_branch: Option<String>,
}

#[derive(Debug, Deserialize)]
struct GitHubTree {
    #[serde(default)]
    tree: Vec<GitHubTreeEntry>,
}

#[derive(Debug, Deserialize)]
struct GitHubTreeEntry {
    path: String,
    #[serde(default, rename = "type")]
    entry_type: Option<String>,
}

/// `https://github.com/owner/repo` → `(owner, repo)`.
fn parse_github_repo(url: &str) -> Option<(String, String)> {
    let parsed = market_url_policy(url).ok()?;
    if parsed.host_str() != Some("github.com") {
        return None;
    }
    let mut segments = parsed.path_segments()?.filter(|part| !part.is_empty());
    let owner = segments.next()?.to_string();
    let repo = segments.next()?.trim_end_matches(".git").to_string();
    if owner.is_empty() || repo.is_empty() {
        return None;
    }
    Some((owner, repo))
}

fn is_scannable_skill_path(path: &str) -> bool {
    let lower = path.to_ascii_lowercase();
    if lower.contains("node_modules/") || lower.starts_with(".git/") || lower.contains("/.git/") {
        return false;
    }
    lower.ends_with("/skill.md") || lower == "skill.md"
}

fn skill_id_from_path(path: &str) -> String {
    let parent = path
        .trim_end_matches("SKILL.md")
        .trim_end_matches("skill.md")
        .trim_end_matches('/');
    let slug = parent.rsplit('/').next().unwrap_or("skill");
    let mut out = String::new();
    for ch in slug.chars() {
        let ch = ch.to_ascii_lowercase();
        if ch.is_ascii_alphanumeric() {
            out.push(ch);
        } else if !out.ends_with('-') && !out.is_empty() {
            out.push('-');
        }
    }
    let trimmed = out.trim_matches('-');
    if trimmed.is_empty() {
        "skill".to_string()
    } else {
        trimmed.chars().take(64).collect()
    }
}

/// Scan a GitHub repository for `SKILL.md` documents.
///
/// The repository tree is read through the API, but documents are served from
/// the CDN: the raw host is TLS-flaky from some networks while the CDN edge
/// reaches them reliably.
fn scan_skill_repository(
    client: &reqwest::blocking::Client,
    source: &MarketSource,
) -> Result<Vec<SkillMarketEntry>, MarketSourceFailure> {
    let Some((owner, repo)) = parse_github_repo(&source.url) else {
        return Err(MarketSourceFailure {
            source_id: source.id.clone(),
            source_name: source.name.clone(),
            code: "market_source_repo_invalid".to_string(),
            message: "skill repository sources must be a github.com owner/repo URL".to_string(),
            host: None,
        });
    };
    let api_base = format!("https://api.github.com/repos/{owner}/{repo}");
    let repo_info: GitHubRepo = fetch_market_json(client, &api_base)
        .map_err(|error| source_failure(source, &error, Some("api.github.com".to_string())))?;
    let branch = repo_info
        .default_branch
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or("main")
        .to_string();
    let tree_url = format!("{api_base}/git/trees/{branch}?recursive=1");
    let tree: GitHubTree = fetch_market_json(client, &tree_url)
        .map_err(|error| source_failure(source, &error, Some("api.github.com".to_string())))?;

    let entries = tree
        .tree
        .iter()
        .filter(|entry| entry.entry_type.as_deref() == Some("blob"))
        .filter(|entry| is_scannable_skill_path(&entry.path))
        .filter_map(|entry| {
            let id = skill_id_from_path(&entry.path);
            let document_url = format!(
                "https://cdn.jsdelivr.net/gh/{owner}/{repo}@{branch}/{}",
                entry.path
            );
            if market_url_policy(&document_url).is_err() {
                return None;
            }
            Some(SkillMarketEntry {
                name: id.clone(),
                id,
                source_id: source.id.clone(),
                description: None,
                homepage: Some(format!(
                    "https://github.com/{owner}/{repo}/tree/{branch}/{}",
                    entry
                        .path
                        .rsplit_once('/')
                        .map(|(dir, _)| dir)
                        .unwrap_or_default()
                )),
                categories: Vec::new(),
                document_url,
                verified: false,
                version: None,
                author: Some(owner.clone()),
            })
        })
        .take(MAX_ENTRIES_PER_SOURCE)
        .collect();
    Ok(entries)
}

// ---------------------------------------------------------------------------
// Frontmatter split
// ---------------------------------------------------------------------------

/// Split a fetched document into frontmatter metadata and the instruction body.
///
/// The body must not include the original block: the create path renders its
/// own frontmatter, so passing the original through would double it.
fn split_skill_document(text: &str) -> (Option<String>, Option<String>, String) {
    let trimmed = text.strip_prefix('\u{feff}').unwrap_or(text);
    let Some(rest) = trimmed.strip_prefix("---") else {
        return (None, None, text.to_string());
    };
    let rest = rest.strip_prefix('\n').unwrap_or(rest);
    let Some(end) = rest.find("\n---") else {
        return (None, None, text.to_string());
    };
    let frontmatter = &rest[..end];
    let mut name = None;
    let mut description = None;
    for line in frontmatter.lines() {
        let Some((key, value)) = line.split_once(':') else {
            continue;
        };
        let value = value.trim().trim_matches('"').trim_matches('\'').trim();
        if value.is_empty() {
            continue;
        }
        match key.trim() {
            "name" | "display_name" | "title" if name.is_none() => name = Some(value.to_string()),
            "description" if description.is_none() => description = Some(value.to_string()),
            _ => {}
        }
    }
    let body_start = end + "\n---".len();
    let body = rest[body_start..]
        .trim_start_matches(['\n', '\r'])
        .to_string();
    (name, description, body)
}

fn skill_document_from_text(entry_id: &str, text: &str) -> SkillMarketDocument {
    let (name, description, body) = split_skill_document(text);
    // Measure the document the create path would actually persist, so the limit
    // reflects stored bytes rather than the fetched ones.
    let rendered = render_skill_document(
        name.as_deref().unwrap_or(entry_id),
        description.as_deref(),
        &body,
    );
    let bytes = rendered.len() as u64;
    SkillMarketDocument {
        entry_id: entry_id.to_string(),
        name,
        description,
        body,
        bytes,
        too_large: bytes > MAX_SKILL_MARKET_DOCUMENT_BYTES,
    }
}

fn render_skill_document(name: &str, description: Option<&str>, body: &str) -> String {
    let mut out = format!("---\nname: {}\n", name.replace('\n', " "));
    if let Some(description) = description.map(str::trim).filter(|value| !value.is_empty()) {
        out.push_str(&format!(
            "description: {}\n",
            description.replace('\n', " ")
        ));
    }
    out.push_str("---\n\n");
    out.push_str(body.trim());
    out.push('\n');
    out
}

// ---------------------------------------------------------------------------
// Search
// ---------------------------------------------------------------------------

impl ProviderConfigService {
    pub fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> VibexResult<McpMarketSearchResponse> {
        let sources = self.market_sources_for_kind(&request.source_ids, true)?;
        let client = market_http_client()?;
        let limit = request.limit.unwrap_or(30).clamp(1, 100);
        let query = request.query.clone().unwrap_or_default();
        let mut entries = Vec::new();
        let mut failed_sources = Vec::new();
        let mut has_more = false;

        for source in &sources {
            let outcome = match source.kind {
                MarketSourceKind::McpRegistry => {
                    search_mcp_registry(&client, source, Some(&query), limit)
                }
                MarketSourceKind::McpCatalog => {
                    match fetch_market_bytes(&client, &source.url, MAX_MARKET_RESPONSE_BYTES) {
                        Ok((body, _)) => Ok((parse_mcp_catalog(source, &body), false)),
                        Err(error) => Err(source_failure(source, &error, None)),
                    }
                }
                _ => continue,
            };
            match outcome {
                Ok((mut source_entries, source_has_more)) => {
                    if request.query.is_some() && source.kind == MarketSourceKind::McpCatalog {
                        let needle = query.to_lowercase();
                        source_entries.retain(|entry| {
                            entry.name.to_lowercase().contains(&needle)
                                || entry
                                    .description
                                    .as_deref()
                                    .is_some_and(|value| value.to_lowercase().contains(&needle))
                        });
                    }
                    has_more |= source_has_more;
                    entries.append(&mut source_entries);
                }
                Err(failure) => failed_sources.push(failure),
            }
        }

        if let Some(category) = request.category {
            entries.retain(|entry| entry.categories.contains(&category));
        }
        let offset = request.offset.unwrap_or(0) as usize;
        let entries = entries.into_iter().skip(offset).collect();
        Ok(McpMarketSearchResponse {
            entries,
            failed_sources,
            has_more,
        })
    }

    pub fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> VibexResult<SkillMarketSearchResponse> {
        let sources = self.market_sources_for_kind(&request.source_ids, false)?;
        let client = market_http_client()?;
        let query = request.query.clone().unwrap_or_default();
        let mut entries = Vec::new();
        let mut failed_sources = Vec::new();

        for source in &sources {
            let outcome = match source.kind {
                MarketSourceKind::SkillCatalog => {
                    if source.url.starts_with("builtin://") {
                        Ok(builtin_skill_catalog())
                    } else {
                        match fetch_market_bytes(&client, &source.url, MAX_MARKET_RESPONSE_BYTES) {
                            Ok((body, _)) => Ok(parse_skill_catalog(source, &body)),
                            Err(error) => Err(source_failure(source, &error, None)),
                        }
                    }
                }
                MarketSourceKind::SkillRepository => scan_skill_repository(&client, source),
                _ => continue,
            };
            match outcome {
                Ok(mut source_entries) => {
                    if !query.trim().is_empty() {
                        let needle = query.to_lowercase();
                        source_entries.retain(|entry| {
                            entry.name.to_lowercase().contains(&needle)
                                || entry
                                    .description
                                    .as_deref()
                                    .is_some_and(|value| value.to_lowercase().contains(&needle))
                        });
                    }
                    entries.append(&mut source_entries);
                }
                Err(failure) => failed_sources.push(failure),
            }
        }

        if let Some(category) = request.category {
            entries.retain(|entry| entry.categories.contains(&category));
        }
        let offset = request.offset.unwrap_or(0) as usize;
        let entries = entries.into_iter().skip(offset).collect();
        Ok(SkillMarketSearchResponse {
            entries,
            failed_sources,
            has_more: false,
        })
    }

    /// Resolve one entry again by id.
    ///
    /// Install calls this rather than trusting a client-supplied template, so a
    /// crafted request cannot introduce a server the catalog never published.
    pub fn mcp_market_entry(&self, request: McpMarketEntryRequest) -> VibexResult<McpMarketEntry> {
        let sources =
            self.market_sources_for_kind(std::slice::from_ref(&request.source_id), true)?;
        let source = sources
            .iter()
            .find(|source| source.id == request.source_id)
            .ok_or_else(|| {
                VibexError::validation("market_source_not_found", "market source was not found")
                    .with_diagnostic("sourceId", request.source_id.clone())
            })?;
        let client = market_http_client()?;
        let mut entries = match source.kind {
            MarketSourceKind::McpRegistry => {
                search_mcp_registry(&client, source, None, 100)
                    .map_err(|failure| {
                        VibexError::provider(failure.code, failure.message)
                            .with_diagnostic("sourceId", failure.source_id)
                    })?
                    .0
            }
            MarketSourceKind::McpCatalog => {
                let (body, _) =
                    fetch_market_bytes(&client, &source.url, MAX_MARKET_RESPONSE_BYTES)?;
                parse_mcp_catalog(source, &body)
            }
            _ => Vec::new(),
        };
        entries
            .iter()
            .position(|entry| entry.id == request.entry_id)
            .map(|index| entries.remove(index))
            .ok_or_else(|| {
                VibexError::validation("market_entry_not_found", "market entry was not found")
                    .with_diagnostic("entryId", request.entry_id.clone())
            })
    }

    pub fn skill_market_document(
        &self,
        request: SkillMarketDocumentRequest,
    ) -> VibexResult<SkillMarketDocument> {
        let sources =
            self.market_sources_for_kind(std::slice::from_ref(&request.source_id), false)?;
        let source = sources
            .iter()
            .find(|source| source.id == request.source_id)
            .ok_or_else(|| {
                VibexError::validation("market_source_not_found", "market source was not found")
                    .with_diagnostic("sourceId", request.source_id.clone())
            })?;
        let entry = self
            .skill_market_entry_for(source, &request.entry_id)?
            .ok_or_else(|| {
                VibexError::validation("market_entry_not_found", "market entry was not found")
                    .with_diagnostic("entryId", request.entry_id.clone())
            })?;
        let client = market_http_client()?;
        let (body, _) =
            fetch_market_bytes(&client, &entry.document_url, MAX_SKILL_DOCUMENT_FETCH_BYTES)?;
        let text = String::from_utf8(body).map_err(|_| {
            VibexError::provider(
                "market_skill_document_not_utf8",
                "skill documents must be UTF-8 markdown",
            )
        })?;
        Ok(skill_document_from_text(&entry.id, &text))
    }

    fn skill_market_entry_for(
        &self,
        source: &MarketSource,
        entry_id: &str,
    ) -> VibexResult<Option<SkillMarketEntry>> {
        let mut entries = if source.url.starts_with("builtin://") {
            builtin_skill_catalog()
        } else {
            let client = market_http_client()?;
            match source.kind {
                MarketSourceKind::SkillCatalog => {
                    let (body, _) =
                        fetch_market_bytes(&client, &source.url, MAX_MARKET_RESPONSE_BYTES)?;
                    parse_skill_catalog(source, &body)
                }
                MarketSourceKind::SkillRepository => scan_skill_repository(&client, source)
                    .map_err(|failure| {
                        VibexError::provider(failure.code, failure.message)
                            .with_diagnostic("sourceId", failure.source_id)
                    })?,
                _ => Vec::new(),
            }
        };
        Ok(entries
            .iter()
            .position(|entry| entry.id == entry_id)
            .map(|index| entries.remove(index)))
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Turn a market template into a create request.
///
/// User-supplied env values win over the catalog's defaults, because the form
/// is where a credential the catalog could not know about is entered.
fn market_entry_to_candidate(
    entry: &McpMarketEntry,
    env_values: &[McpServerEnvEntry],
) -> McpServerCreateRequest {
    let mut env = Vec::new();
    for requirement in &entry.env {
        let supplied = env_values
            .iter()
            .find(|value| value.name == requirement.name)
            .map(|value| value.value.clone())
            .filter(|value| !value.trim().is_empty());
        let resolved = supplied.or_else(|| requirement.default_value.clone());
        if let Some(value) = resolved {
            env.push(McpServerEnvEntry {
                name: requirement.name.clone(),
                value,
            });
        }
    }
    // Values for names the catalog did not declare are still carried: an entry
    // may need a variable the publisher did not annotate.
    for value in env_values {
        if !env.iter().any(|existing| existing.name == value.name)
            && !value.name.trim().is_empty()
            && !value.value.trim().is_empty()
        {
            env.push(value.clone());
        }
    }
    McpServerCreateRequest {
        display_name: entry.name.clone(),
        transport_kind: entry.transport,
        status: McpServerStatus::Enabled,
        scope_kind: McpServerScopeKind::User,
        project_id: None,
        workspace_id: None,
        command: entry.command.clone(),
        args: entry.args.clone(),
        env,
        url: entry.url.clone(),
        headers: Vec::new(),
        description: entry.description.clone(),
        tags: vec!["market".to_string()],
        secret_references: Vec::new(),
        provider_matrix: Vec::new(),
    }
}

/// Whether an agent can host this transport.
///
/// Codex and DeepSeek read MCP descriptors but reject SSE, so a market install
/// must not claim to have enabled a server the agent cannot start.
fn agent_can_host_transport(agent_id: &AgentId, transport: McpServerTransportKind) -> bool {
    if transport != McpServerTransportKind::Sse {
        return true;
    }
    !matches!(agent_id.as_str(), "codex" | "deepseek")
}

impl ProviderConfigService {
    pub fn install_mcp_market_entry(
        &self,
        request: McpMarketInstallRequest,
    ) -> VibexResult<McpMarketInstallResult> {
        if request.agent_ids.is_empty() {
            return Err(VibexError::validation(
                "market_install_agents_required",
                "select at least one agent to install into",
            ));
        }
        // The candidate is either the editable form's result or the catalog
        // template re-resolved from the source; never a client-invented server
        // when the catalog can be asked.
        let candidate = match request.candidate.clone() {
            Some(candidate) => candidate,
            None => {
                let entry = self.mcp_market_entry(McpMarketEntryRequest {
                    source_id: request.source_id.clone(),
                    entry_id: request.entry_id.clone(),
                })?;
                market_entry_to_candidate(&entry, &request.env_values)
            }
        };
        validate_mcp_create_request(&candidate)?;

        let (hostable, skipped): (Vec<AgentId>, Vec<AgentId>) = request
            .agent_ids
            .iter()
            .cloned()
            .partition(|agent_id| agent_can_host_transport(agent_id, candidate.transport_kind));
        if hostable.is_empty() {
            return Err(VibexError::validation(
                "market_install_no_hostable_agent",
                "none of the selected agents can host this server's transport",
            ));
        }

        let conn = self.open_connection()?;
        let now = vibex_core::unix_timestamp_ms();
        let existing = find_existing_mcp_server(&conn, &candidate)?;
        let created = existing.is_none();
        let mut server = match existing {
            Some(existing) => {
                // Preserve identity and secret references on reinstall.
                let mut updated = existing;
                updated.display_name = candidate.display_name.clone();
                updated.transport_kind = candidate.transport_kind;
                updated.command = candidate.command.clone();
                updated.args = candidate.args.clone();
                updated.env = candidate.env.clone();
                updated.url = candidate.url.clone();
                updated.headers = candidate.headers.clone();
                updated.description = candidate.description.clone();
                updated.updated_at_ms = now;
                updated
            }
            None => McpServerRepository::from_create_request(normalize_mcp_create_request(
                candidate.clone(),
            )),
        };
        let mut diagnostics = Vec::new();
        let mut matrix = server.agent_matrix.clone();
        for agent_id in &hostable {
            set_agent_matrix_entry(&mut matrix, agent_id, true, now);
        }
        // A selected agent that cannot host the transport must not keep a stale
        // enabled row: it would win the next scan and misreport the server.
        for agent_id in &skipped {
            set_agent_matrix_entry(&mut matrix, agent_id, false, now);
        }
        server.agent_matrix = matrix;

        if McpServerRepository::get(&conn, &server.id)?.is_some() {
            McpServerRepository::update(&conn, &server)?;
            McpServerRepository::replace_agent_matrix(&conn, &server.id, &server.agent_matrix)?;
        } else {
            McpServerRepository::insert(&conn, &server)?;
            McpServerRepository::replace_agent_matrix(&conn, &server.id, &server.agent_matrix)?;
        }
        let readback = McpServerRepository::get(&conn, &server.id)?.ok_or_else(|| {
            VibexError::storage(
                "market_install_readback_missing",
                "the installed MCP server could not be read back",
            )
        })?;
        if !skipped.is_empty() {
            diagnostics.push(diagnostic(
                "marketInstallTransportSkipped",
                skipped
                    .iter()
                    .map(|agent_id| agent_id.as_str())
                    .collect::<Vec<_>>()
                    .join(","),
            ));
        }
        Ok(McpMarketInstallResult {
            server: readback,
            created,
            enabled_agent_ids: hostable,
            skipped_agent_ids: skipped,
            diagnostics,
        })
    }

    pub fn install_skill_market_entry(
        &self,
        request: SkillMarketInstallRequest,
    ) -> VibexResult<SkillMarketInstallResult> {
        if request.agent_ids.is_empty() {
            return Err(VibexError::validation(
                "market_install_agents_required",
                "select at least one agent to install into",
            ));
        }
        if request.document.too_large {
            return Err(VibexError::validation(
                "market_skill_document_too_large",
                "the skill document exceeds the maximum size",
            ));
        }
        if request.document.body.trim().is_empty() {
            return Err(VibexError::validation(
                "market_skill_document_empty",
                "the skill document has no instructions",
            ));
        }
        let display_name = request
            .document
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
            .unwrap_or_else(|| request.entry_id.clone());
        let rendered = render_skill_document(
            &display_name,
            request.document.description.as_deref(),
            &request.document.body,
        );
        if rendered.len() as u64 > MAX_SKILL_MARKET_DOCUMENT_BYTES {
            return Err(VibexError::validation(
                "market_skill_document_too_large",
                "the skill document exceeds the maximum size",
            ));
        }
        let create = SkillCreateRequest {
            display_name: display_name.clone(),
            source_kind: SkillSourceKind::Marketplace,
            status: SkillStatus::Enabled,
            scope_kind: SkillScopeKind::User,
            project_id: None,
            workspace_id: None,
            source_uri: Some(format!("market:{}:{}", request.source_id, request.entry_id)),
            description: request.document.description.clone(),
            tags: vec!["market".to_string()],
            content_preview: Some(request.document.body.chars().take(2048).collect()),
            body: Some(rendered),
            provider_matrix: Vec::new(),
        };
        validate_skill_create_request(&create)?;

        let conn = self.open_connection()?;
        let now = vibex_core::unix_timestamp_ms();
        let existing = SkillRepository::list(&conn)?
            .into_iter()
            .find(|skill| skill.display_name.eq_ignore_ascii_case(&display_name));
        let created = existing.is_none();
        let mut skill = match existing {
            Some(existing) => {
                let mut updated = existing;
                updated.display_name = display_name;
                updated.source_kind = SkillSourceKind::Marketplace;
                updated.status = SkillStatus::Enabled;
                updated.source_uri = create.source_uri.clone();
                updated.description = create.description.clone();
                updated.content_preview = create.content_preview.clone();
                updated.body = create.body.clone();
                updated.updated_at_ms = now;
                updated
            }
            None => SkillRepository::from_create_request(normalize_skill_create_request(create)),
        };
        let mut matrix = skill.agent_matrix.clone();
        for agent_id in &request.agent_ids {
            set_skill_agent_matrix_entry(&mut matrix, agent_id, true, now);
        }
        skill.agent_matrix = matrix;

        if SkillRepository::get(&conn, &skill.id)?.is_some() {
            SkillRepository::update(&conn, &skill)?;
            SkillRepository::replace_agent_matrix(&conn, &skill.id, &skill.agent_matrix)?;
        } else {
            SkillRepository::insert(&conn, &skill)?;
            SkillRepository::replace_agent_matrix(&conn, &skill.id, &skill.agent_matrix)?;
        }
        let readback = SkillRepository::get(&conn, &skill.id)?.ok_or_else(|| {
            VibexError::storage(
                "market_install_readback_missing",
                "the installed Skill could not be read back",
            )
        })?;
        Ok(SkillMarketInstallResult {
            skill: readback,
            created,
            enabled_agent_ids: request.agent_ids,
            diagnostics: Vec::new(),
        })
    }
}

fn set_agent_matrix_entry(
    matrix: &mut Vec<vibex_core::McpServerAgentMatrix>,
    agent_id: &AgentId,
    enabled: bool,
    now: i64,
) {
    if let Some(entry) = matrix.iter_mut().find(|entry| &entry.agent_id == agent_id) {
        entry.enabled = enabled;
        entry.updated_at_ms = now;
        return;
    }
    matrix.push(vibex_core::McpServerAgentMatrix {
        agent_id: agent_id.clone(),
        enabled,
        source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
        updated_at_ms: now,
    });
}

fn set_skill_agent_matrix_entry(
    matrix: &mut Vec<vibex_core::SkillAgentMatrix>,
    agent_id: &AgentId,
    enabled: bool,
    now: i64,
) {
    if let Some(entry) = matrix.iter_mut().find(|entry| &entry.agent_id == agent_id) {
        entry.enabled = enabled;
        entry.updated_at_ms = now;
        return;
    }
    matrix.push(vibex_core::SkillAgentMatrix {
        agent_id: agent_id.clone(),
        enabled,
        source_kind: vibex_core::ResourceAgentMatrixSourceKind::Manual,
        updated_at_ms: now,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn market_policy_refuses_non_public_and_insecure_urls() {
        assert!(market_url_policy("http://example.com/catalog.json").is_err());
        assert!(market_url_policy("https://127.0.0.1/catalog.json").is_err());
        assert!(market_url_policy("https://10.0.0.5/catalog.json").is_err());
        assert!(market_url_policy("https://localhost/catalog.json").is_err());
        // A trailing dot is the same name and must not slip past the check.
        assert!(market_url_policy("https://localhost./catalog.json").is_err());
        assert!(market_url_policy("https://user:pass@example.com/x.json").is_err());
        assert!(market_url_policy("https://example.com/catalog.json").is_ok());
    }

    #[test]
    fn split_skill_document_strips_frontmatter_and_reads_metadata() {
        let (name, description, body) = split_skill_document(
            "---\nname: demo\ndescription: \"A demo\"\n---\n\n# Heading\n\nBody\n",
        );
        assert_eq!(name.as_deref(), Some("demo"));
        assert_eq!(description.as_deref(), Some("A demo"));
        assert!(body.starts_with("# Heading"));
        assert!(!body.contains("---"));
    }

    #[test]
    fn split_skill_document_without_frontmatter_keeps_body() {
        let (name, description, body) = split_skill_document("# Heading\n\nBody\n");
        assert!(name.is_none());
        assert!(description.is_none());
        assert_eq!(body, "# Heading\n\nBody\n");
    }

    #[test]
    fn registry_ids_are_slugged_and_bounded() {
        assert_eq!(registry_entry_id("com.pulsemcp/foo"), "com-pulsemcp-foo");
        assert_eq!(registry_entry_id("///"), "mcp-server");
        assert!(registry_entry_id(&"a/".repeat(200)).len() <= 60);
    }

    #[test]
    fn package_identifier_pins_published_versions() {
        let npm = RegistryPackage {
            registry_type: Some("npm".to_string()),
            identifier: Some("@scope/server".to_string()),
            version: Some("1.2.3".to_string()),
            runtime_hint: None,
            runtime_arguments: None,
            package_arguments: None,
            environment_variables: None,
        };
        assert_eq!(
            registry_package_identifier(&npm, "npx").as_deref(),
            Some("@scope/server@1.2.3")
        );
        let pypi = RegistryPackage {
            registry_type: Some("pypi".to_string()),
            identifier: Some("mcp-server".to_string()),
            version: Some("2.0.0".to_string()),
            runtime_hint: None,
            runtime_arguments: None,
            package_arguments: None,
            environment_variables: None,
        };
        assert_eq!(
            registry_package_identifier(&pypi, "uvx").as_deref(),
            Some("mcp-server==2.0.0")
        );
        // `latest` is not a pin and must not be written into the launcher.
        let floating = RegistryPackage {
            version: Some("latest".to_string()),
            ..npm
        };
        assert_eq!(
            registry_package_identifier(&floating, "npx").as_deref(),
            Some("@scope/server")
        );
    }

    #[test]
    fn transport_gate_excludes_sse_for_codex_and_deepseek() {
        let codex = AgentId::parse("codex").unwrap();
        assert!(!agent_can_host_transport(
            &codex,
            McpServerTransportKind::Sse
        ));
        assert!(agent_can_host_transport(
            &codex,
            McpServerTransportKind::Stdio
        ));
        let claude = AgentId::parse("claude").unwrap();
        assert!(agent_can_host_transport(
            &claude,
            McpServerTransportKind::Sse
        ));
    }

    #[test]
    fn skill_paths_skip_vendor_directories() {
        assert!(is_scannable_skill_path("skills/demo/SKILL.md"));
        assert!(!is_scannable_skill_path("node_modules/pkg/SKILL.md"));
        assert!(!is_scannable_skill_path(".git/x/SKILL.md"));
        assert!(!is_scannable_skill_path("skills/demo/README.md"));
    }

    #[test]
    fn builtin_skill_catalog_entries_are_public_https() {
        for entry in builtin_skill_catalog() {
            assert!(
                market_url_policy(&entry.document_url).is_ok(),
                "{} must point at a public https document",
                entry.id
            );
        }
    }
}

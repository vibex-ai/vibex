//! Marketplace catalog fetching and installation.
//!
//! This module owns the only outbound network calls the Config Center makes on
//! behalf of a market. Three rules shape everything below.
//!
//! **The authoritative owner fetches.** The desktop renders catalog values; it
//! never fetches one itself. That keeps a paired device from becoming a second
//! network client with its own proxy and TLS story, and it means a catalog is
//! fetched once for every client of this runtime.
//!
//! **Each market has one upstream.** MCP reads the official registry. The Skill
//! market reads a public skill index for search and resolves the document from
//! the repository it names. There is no user-configured source list, so there is
//! no policy for one.
//!
//! **A market never invents an entry.** Everything the UI shows came out of an
//! upstream response; an entry the upstream did not publish cannot be listed.
//!
//! ## Why the MCP catalog is indexed rather than queried
//!
//! The registry's own `search` parameter is not usable interactively: measured
//! against the live registry a single `?search=` request took 9–90 seconds,
//! while walking one cursor page of the same catalog takes 2–6. A market that
//! searched upstream would time out far more often than it answered.
//!
//! So the runtime walks the registry itself, one cursor page at a time, into a
//! process-wide cache, and every query is answered by filtering that cache.
//! Indexing continues in the background while the market is in use, which is
//! what lets a search cover thousands of entries without ever waiting on the
//! registry's own scan.
//!
//! ## Network boundary
//!
//! Both upstreams are fixed, but a Skill entry names a repository and the
//! document is fetched from a CDN, so the fetcher still validates every URL it
//! is handed rather than trusting its caller:
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

use std::collections::HashSet;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};
use std::time::{Duration, Instant};

use serde::Deserialize;
use vibex_core::{
    AgentId, MAX_MARKET_RESPONSE_BYTES, MAX_SKILL_MARKET_DOCUMENT_BYTES, MarketEnvRequirement,
    McpMarketEntry, McpMarketInstallRequest, McpMarketInstallResult, McpMarketSearchRequest,
    McpMarketSearchResponse, McpServerTransportKind, ProviderKind, SkillCreateRequest,
    SkillMarketDocument, SkillMarketDocumentRequest, SkillMarketEntry, SkillMarketInstallRequest,
    SkillMarketInstallResult, SkillMarketSearchRequest, SkillMarketSearchResponse, SkillScopeKind,
    SkillSourceKind, SkillStatus, VibexError, VibexResult,
};
use vibex_db::{McpServerRepository, SkillRepository};

use crate::mcp_delivery::{AgentMcpDelivery, agent_has_native_mcp_file, agent_mcp_delivery};
use crate::native_export::AgentNativeMcpWrite;
use crate::native_surface::native_mcp_surface_supports_transport;
use crate::{
    ProviderConfigService, diagnostic, find_existing_mcp_server, normalize_mcp_create_request,
    normalize_skill_create_request, validate_mcp_create_request, validate_skill_create_request,
};

/// The official MCP registry. Its `v0.1` API is the only MCP catalog.
const MCP_REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
/// The public Skill index, used for search only.
const SKILL_INDEX_SEARCH: &str = "https://www.skills.sh/api/search";
/// The query that stands in for browsing the Skill index.
///
/// The index has no list endpoint: `q` is mandatory, must be at least two
/// characters, and is the only way in. A market that opened on an empty query
/// would therefore show nothing at all, so the browse view asks the index for a
/// deliberately broad term instead. This is a query, not an invented entry:
/// every row it returns is still published by the index. The index hands those
/// rows back in its own fuzzy-match order rather than by popularity, so the
/// caller re-ranks them for the browse view.
const SKILL_INDEX_BROWSE_QUERY: &str = "skill";
/// Shortest query the index accepts. Anything shorter is a bad request.
const SKILL_INDEX_MIN_QUERY_CHARS: usize = 2;
/// Lists a repository's files without touching the GitHub API, which rate
/// limits unauthenticated callers to a handful of requests per hour.
const JSDELIVR_DATA: &str = "https://data.jsdelivr.com/v1/packages/gh";
/// Serves the document itself.
const JSDELIVR_CDN: &str = "https://cdn.jsdelivr.net/gh";

/// One request may not take longer than this, including every redirect.
///
/// The registry is not fast: walking its pages measured between 2 and 28
/// seconds each. A ten-second ceiling — what this used to be — turned every
/// slow-but-fine page into a reported outage, so the ceiling is set above the
/// slowest page observed rather than at a comfortable interactive latency.
const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
/// Redirect hops followed before the fetch is refused.
const MAX_MARKET_REDIRECTS: usize = 5;
/// Entries a single response may contribute, so one huge catalog cannot flood
/// the list.
const MAX_ENTRIES_PER_SEARCH: usize = 500;
/// Attempts one registry page gets before the indexer gives up on it.
const MCP_PAGE_ATTEMPTS: usize = 2;
/// Registry page size. The API caps `limit` here, so asking for more only
/// pretends to.
const MCP_REGISTRY_PAGE_SIZE: u32 = 100;
/// How long a request may wait for the catalog to reach what it asked for.
const MCP_CATALOG_WAIT: Duration = Duration::from_secs(12);
/// Pause between background pages, so indexing stays a polite trickle.
const MCP_CATALOG_FILL_PAUSE: Duration = Duration::from_millis(250);
/// Entries the cache will hold before the indexer stops walking.
const MCP_CATALOG_MAX_ENTRIES: usize = 4_000;
/// A catalog older than this is walked again from the registry's first page.
const MCP_CATALOG_TTL: Duration = Duration::from_secs(30 * 60);
/// The indexer stops once nothing has asked for the market for this long, so
/// closing the view ends the network activity rather than leaving it running.
const MCP_CATALOG_IDLE: Duration = Duration::from_secs(120);
/// Longest query the market will filter on.
const MCP_CATALOG_MAX_QUERY_CHARS: usize = 120;
/// How often a waiting request re-checks the cache.
const MCP_CATALOG_POLL: Duration = Duration::from_millis(120);
/// A Skill document is markdown; anything past this is not one.
const MAX_SKILL_DOCUMENT_FETCH_BYTES: u64 = MAX_SKILL_MARKET_DOCUMENT_BYTES + 1;
/// Branches tried when resolving a skill's document. The index does not publish
/// a branch, and these two cover effectively every public repository.
const SKILL_BRANCHES: [&str; 2] = ["main", "master"];

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
    let url = reqwest::Url::parse(raw.trim())
        .map_err(|_| VibexError::validation("market_url_invalid", "market URL does not parse"))?;
    if url.scheme() != "https" {
        return Err(VibexError::validation(
            "market_url_insecure",
            "market URLs must use https",
        ));
    }
    if !url.username().is_empty() || url.password().is_some() {
        return Err(VibexError::validation(
            "market_url_credentials",
            "market URLs must not carry credentials",
        ));
    }
    let Some(host) = url.host_str() else {
        return Err(VibexError::validation(
            "market_url_host_missing",
            "market URL has no host",
        ));
    };
    let host = host.trim_end_matches('.').to_ascii_lowercase();
    if let Ok(ip) = host.parse::<IpAddr>() {
        if !is_public_ip(ip) {
            return Err(VibexError::validation(
                "market_url_not_public",
                "market URL must resolve to a public address",
            ));
        }
    } else if host == "localhost"
        || host.ends_with(".localhost")
        || host.ends_with(".local")
        || host.ends_with(".internal")
        || !host.contains('.')
    {
        return Err(VibexError::validation(
            "market_url_not_public",
            "market URL must be a public host",
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
        let response = client
            .get(url.clone())
            .send()
            .map_err(|error| market_fetch_error(&url, "market_unreachable", error.to_string()))?;
        let status = response.status();
        if status.is_redirection() {
            let location = response
                .headers()
                .get(reqwest::header::LOCATION)
                .and_then(|value| value.to_str().ok())
                .map(str::to_string);
            let Some(location) = location else {
                return Err(VibexError::provider(
                    "market_redirect_invalid",
                    "the market redirected without a location",
                ));
            };
            hops += 1;
            if hops > MAX_MARKET_REDIRECTS {
                return Err(VibexError::provider(
                    "market_redirect_limit",
                    "the market redirected too many times",
                ));
            }
            // Resolve relative redirects against the hop that produced them,
            // then re-run the whole policy on the result.
            let next = url.join(&location).map_err(|_| {
                VibexError::provider(
                    "market_redirect_invalid",
                    "the market redirected to an invalid location",
                )
            })?;
            url = market_url_policy(next.as_str())?;
            continue;
        }
        if !status.is_success() {
            return Err(VibexError::provider(
                "market_rejected",
                format!("the market responded with HTTP {}", status.as_u16()),
            )
            .with_diagnostic("host", url.host_str().unwrap_or_default()));
        }
        if let Some(length) = response.content_length()
            && length > limit
        {
            return Err(VibexError::provider(
                "market_response_too_large",
                "the market response exceeded the size limit",
            ));
        }
        let host = url.host_str().unwrap_or_default().to_string();
        let mut body = Vec::new();
        // `take` bounds the read even when the response lies about its length.
        response
            .take(limit)
            .read_to_end(&mut body)
            .map_err(|error| VibexError::provider("market_read_failed", error.to_string()))?;
        if body.len() as u64 >= limit {
            return Err(VibexError::provider(
                "market_response_too_large",
                "the market response exceeded the size limit",
            ));
        }
        return Ok((body, host));
    }
}

fn market_fetch_error(url: &reqwest::Url, code: &'static str, message: String) -> VibexError {
    VibexError::provider(code, message)
        .with_recovery_hint("Check the network connection and retry")
        .with_diagnostic("host", url.host_str().unwrap_or_default())
}

fn fetch_market_json<T: serde::de::DeserializeOwned>(
    client: &reqwest::blocking::Client,
    url: &str,
) -> VibexResult<T> {
    let (body, host) = fetch_market_bytes(client, url, MAX_MARKET_RESPONSE_BYTES)?;
    serde_json::from_slice(&body).map_err(|error| {
        VibexError::provider("market_malformed", error.to_string()).with_diagnostic("host", host)
    })
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

fn env_name_looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"]
        .iter()
        .any(|needle| upper.contains(needle))
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
    /// Registry-owned metadata. The lifecycle status lives here rather than on
    /// the server itself, so a deprecated entry can be marked as such.
    #[serde(default, rename = "_meta")]
    meta: Option<RegistryRecordMeta>,
}

#[derive(Debug, Deserialize)]
struct RegistryRecordMeta {
    #[serde(default, rename = "io.modelcontextprotocol.registry/official")]
    official: Option<RegistryOfficialMeta>,
}

#[derive(Debug, Deserialize)]
struct RegistryOfficialMeta {
    #[serde(default)]
    status: Option<String>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<String>,
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
    #[serde(default, rename = "websiteUrl")]
    website_url: Option<String>,
    #[serde(default)]
    repository: Option<RegistryRepository>,
    #[serde(default)]
    packages: Vec<RegistryPackage>,
    #[serde(default)]
    remotes: Vec<RegistryRemote>,
}

/// The registry publishes `repository` as an object, but older records carry a
/// bare URL string; both spellings name the same thing.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum RegistryRepository {
    Url(String),
    Detail(RegistryRepositoryDetail),
}

#[derive(Debug, Deserialize)]
struct RegistryRepositoryDetail {
    #[serde(default)]
    url: Option<String>,
}

impl RegistryRepository {
    fn url(&self) -> Option<&str> {
        match self {
            RegistryRepository::Url(url) => Some(url.as_str()),
            RegistryRepository::Detail(detail) => detail.url.as_deref(),
        }
    }
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
    #[serde(default, rename = "type")]
    argument_type: Option<String>,
    #[serde(default)]
    name: Option<String>,
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
    /// The registry labels credentials itself. This is authoritative where it
    /// is present; the name heuristic is only a fallback for records that omit
    /// it.
    #[serde(default, rename = "isSecret")]
    is_secret: Option<bool>,
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

/// Flatten published arguments into launcher words.
///
/// A `named` argument contributes its name and then its value, so `--port 8080`
/// survives as the flag the publisher meant; a positional one contributes only
/// its value. Keeping just the value would hand the server a bare word where it
/// expects a flag.
fn registry_arg_values(args: Option<&Vec<RegistryArgument>>) -> Vec<String> {
    let mut out = Vec::new();
    for argument in args.into_iter().flatten() {
        let name = argument
            .name
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let value = argument
            .value
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        let named = argument.argument_type.as_deref() == Some("named")
            || (name.is_some() && argument.argument_type.as_deref() != Some("positional"));
        if named {
            if let Some(name) = name {
                out.push(name.to_string());
            }
            if let Some(value) = value {
                out.push(value.to_string());
            }
        } else if let Some(value) = value {
            out.push(value.to_string());
        }
    }
    out
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
        ("uvx", Some(version)) => Some(format!("{identifier}=={version}")),
        // A scoped name (`@scope/pkg`) carries a leading `@` that is not a
        // version separator, so only a second `@` means "already pinned".
        ("npx", Some(version)) if !identifier[1.min(identifier.len())..].contains('@') => {
            Some(format!("{identifier}@{version}"))
        }
        _ => Some(identifier.to_string()),
    }
}

/// The launcher a package is started with.
fn registry_package_runtime(package: &RegistryPackage, registry_type: &str) -> String {
    package
        .runtime_hint
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .unwrap_or_else(|| match registry_type {
            "pypi" => "uvx".to_string(),
            _ => "npx".to_string(),
        })
}

/// Launcher arguments for one package.
///
/// The specifier goes directly after the launcher — `npx -y pkg`, `uvx pkg` —
/// and the arguments the registry published follow it. `-y` is npx's own flag
/// for skipping its install prompt; `uvx` has no equivalent, and handing it one
/// would make the launcher reject the command outright.
fn registry_package_args(package: &RegistryPackage, runtime: &str, specifier: &str) -> Vec<String> {
    let mut args = registry_arg_values(package.runtime_arguments.as_ref());
    if runtime == "npx" {
        args.push("-y".to_string());
    }
    args.push(specifier.to_string());
    args.extend(registry_arg_values(package.package_arguments.as_ref()));
    args
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
                // The registry labels credentials itself. Where it does, that
                // label wins; the name heuristic only covers older records
                // that predate the field.
                secret: variable
                    .is_secret
                    .unwrap_or_else(|| env_name_looks_secret(name)),
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

/// Package registries whose published form this product can actually start, in
/// the order it prefers them.
///
/// The registry also publishes `oci` and `mcpb` packages. There is no container
/// runner behind the market, so an entry built from one would install a command
/// that cannot start — an oci-first record used to become `npx <image>`, which
/// fails at launch. Those records fall through to their remote form, or are
/// dropped when they have none.
const MCP_PACKAGE_REGISTRIES: [&str; 2] = ["npm", "pypi"];

/// The runnable form one registry record resolves to.
enum RegistryLaunchForm {
    Stdio {
        runtime: String,
        args: Vec<String>,
        env: Vec<MarketEnvRequirement>,
        version: Option<String>,
        kind: &'static str,
    },
    Remote {
        url: String,
    },
}

/// Pick the form an install would actually start.
///
/// npm wins over pypi over a remote: the first two are the stdio forms every
/// Agent can host. A record with neither a runnable package nor a public
/// streamable-http remote resolves to `None`, because the market must never
/// list something it cannot start.
fn registry_launch_form(server: &RegistryServer) -> Option<RegistryLaunchForm> {
    for registry_type in MCP_PACKAGE_REGISTRIES {
        let Some(package) = server.packages.iter().find(|package| {
            package.registry_type.as_deref() == Some(registry_type)
                && package
                    .identifier
                    .as_deref()
                    .is_some_and(|identifier| !identifier.trim().is_empty())
        }) else {
            continue;
        };
        let runtime = registry_package_runtime(package, registry_type);
        let Some(specifier) = registry_package_identifier(package, &runtime) else {
            continue;
        };
        return Some(RegistryLaunchForm::Stdio {
            args: registry_package_args(package, &runtime, &specifier),
            env: registry_env_requirements(package),
            version: package.version.clone(),
            runtime,
            kind: registry_type,
        });
    }

    // Only the streamable-http remote is installable: an install must not
    // resolve to a transport the product cannot start.
    let remote = server.remotes.iter().find(|remote| {
        remote.remote_type.as_deref() == Some("streamable-http")
            && remote
                .url
                .as_deref()
                .is_some_and(|url| market_url_policy(url).is_ok())
    })?;
    Some(RegistryLaunchForm::Remote {
        url: remote.url.clone()?,
    })
}

fn registry_server_to_entry(record: &RegistryServerRecord) -> Option<McpMarketEntry> {
    let server = record.server.as_ref()?;
    if server.name.trim().is_empty() {
        return None;
    }
    let form = registry_launch_form(server)?;
    let official = record.meta.as_ref().and_then(|meta| meta.official.as_ref());

    let display = server
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(server.name.as_str())
        .to_string();
    let text = |value: Option<&String>| {
        value
            .map(|value| value.trim())
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    };
    let repository = server
        .repository
        .as_ref()
        .and_then(RegistryRepository::url)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    let homepage = text(server.homepage.as_ref())
        .or_else(|| text(server.website_url.as_ref()))
        .or_else(|| repository.clone());

    let (transport, command, args, url, env, version, package_kind) = match form {
        RegistryLaunchForm::Stdio {
            runtime,
            args,
            env,
            version,
            kind,
        } => (
            McpServerTransportKind::Stdio,
            Some(runtime),
            args,
            None,
            env,
            version,
            Some(kind.to_string()),
        ),
        RegistryLaunchForm::Remote { url } => (
            McpServerTransportKind::Http,
            None,
            Vec::new(),
            Some(url),
            Vec::new(),
            None,
            Some("remote".to_string()),
        ),
    };

    Some(McpMarketEntry {
        id: registry_entry_id(&server.name),
        name: display,
        description: text(server.description.as_ref()),
        homepage,
        repository,
        author: server.name.split('/').next().map(str::to_string),
        status: official.and_then(|meta| text(meta.status.as_ref())),
        updated_at: official.and_then(|meta| text(meta.updated_at.as_ref())),
        transport,
        command,
        args,
        url,
        env,
        version,
        package_kind,
    })
}

// ---------------------------------------------------------------------------
// MCP registry index
// ---------------------------------------------------------------------------

/// The registry as this process has walked it so far.
///
/// The cache is process-wide rather than per-service because there is exactly
/// one authoritative runtime per process, and the catalog it walks is the same
/// one every caller — local window or paired device — is asking about.
#[derive(Default)]
struct McpCatalogIndex {
    entries: Vec<McpMarketEntry>,
    /// Ids already held, so a record the registry lists twice is stored once.
    ids: HashSet<String>,
    /// Cursor for the next page, absent once the registry is exhausted.
    cursor: Option<String>,
    exhausted: bool,
    /// True while the background walker is running.
    filling: bool,
    /// Last time a caller asked for the market, which is what keeps the
    /// walker alive.
    used_at: Option<Instant>,
    /// When the first page landed, which is what staleness is measured from.
    fetched_at: Option<Instant>,
    /// The last page failure, reported when a request finds nothing to answer
    /// with. Kept here rather than returned immediately because the walker
    /// runs on its own thread and has no caller to return to.
    error: Option<VibexError>,
}

static MCP_CATALOG: LazyLock<Mutex<McpCatalogIndex>> =
    LazyLock::new(|| Mutex::new(McpCatalogIndex::default()));

/// Lock the index, ignoring poisoning.
///
/// A panic in the walker must not turn the market into a permanently dead
/// feature: the worst a poisoned lock can mean here is that a half-written
/// page is visible, and the next walk repairs that.
fn mcp_catalog_lock() -> std::sync::MutexGuard<'static, McpCatalogIndex> {
    MCP_CATALOG
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn mcp_registry_page_url(cursor: Option<&str>) -> String {
    let mut url =
        format!("{MCP_REGISTRY_BASE}/v0.1/servers?version=latest&limit={MCP_REGISTRY_PAGE_SIZE}");
    if let Some(cursor) = cursor.filter(|value| !value.is_empty()) {
        url.push_str("&cursor=");
        url.push_str(&urlencode(cursor));
    }
    url
}

/// Walk one registry page into the index and report how many entries it added.
///
/// The registry's `version=latest` view is what the market installs from, so
/// that is the view it indexes: a page of it is one request, and the response's
/// own cursor is the only way forward.
fn mcp_catalog_fetch_page(client: &reqwest::blocking::Client) -> VibexResult<usize> {
    let cursor = mcp_catalog_lock().cursor.clone();
    let response: RegistryListResponse =
        fetch_market_json(client, &mcp_registry_page_url(cursor.as_deref()))?;

    let mut index = mcp_catalog_lock();
    let mut added = 0usize;
    for record in &response.servers {
        if index.entries.len() >= MCP_CATALOG_MAX_ENTRIES {
            break;
        }
        let Some(entry) = registry_server_to_entry(record) else {
            continue;
        };
        if !index.ids.insert(entry.id.clone()) {
            continue;
        }
        index.entries.push(entry);
        added += 1;
    }

    let next = response
        .metadata
        .and_then(|metadata| metadata.next_cursor)
        .filter(|cursor| !cursor.is_empty());
    match next {
        // A cursor that does not move would loop the walker forever on the same
        // page, so a repeated cursor is treated as the end of the catalog.
        Some(next)
            if Some(&next) != cursor.as_ref() && index.entries.len() < MCP_CATALOG_MAX_ENTRIES =>
        {
            index.cursor = Some(next);
        }
        _ => index.exhausted = true,
    }
    index.fetched_at.get_or_insert_with(Instant::now);
    index.error = None;
    Ok(added)
}

/// Walk pages until one lands, retrying a page the registry drops.
///
/// A single slow page used to be indistinguishable from an outage. The walker
/// is not on anyone's critical path, so it can afford a second attempt.
fn mcp_catalog_fetch_page_with_retry(client: &reqwest::blocking::Client) -> VibexResult<usize> {
    let mut last = None;
    for _ in 0..MCP_PAGE_ATTEMPTS {
        match mcp_catalog_fetch_page(client) {
            Ok(added) => return Ok(added),
            Err(error) => last = Some(error),
        }
    }
    Err(last.unwrap_or_else(|| {
        VibexError::provider("market_unreachable", "the registry could not be reached")
    }))
}

/// Start the background walker unless it is already running or has finished.
///
/// The registry answers one cursor page at a time and the catalog holds
/// thousands of entries, so indexing continues in the background while the
/// market is in use. Every call refreshes the activity stamp; the walker stops
/// on its own once the registry is exhausted, the entry cap is reached, or the
/// market has been closed long enough that continuing would be work nobody
/// asked for.
fn mcp_catalog_ensure_walker() {
    {
        let mut index = mcp_catalog_lock();
        index.used_at = Some(Instant::now());
        if index.filling || index.exhausted || index.entries.len() >= MCP_CATALOG_MAX_ENTRIES {
            return;
        }
        index.filling = true;
    }

    let spawned = std::thread::Builder::new()
        .name("mcp-catalog".to_string())
        .spawn(|| {
            let client = match market_http_client() {
                Ok(client) => client,
                Err(error) => {
                    let mut index = mcp_catalog_lock();
                    index.error = Some(error);
                    index.filling = false;
                    return;
                }
            };
            loop {
                {
                    let index = mcp_catalog_lock();
                    if index.exhausted
                        || index.entries.len() >= MCP_CATALOG_MAX_ENTRIES
                        || index
                            .used_at
                            .is_some_and(|at| at.elapsed() > MCP_CATALOG_IDLE)
                    {
                        break;
                    }
                }
                if let Err(error) = mcp_catalog_fetch_page_with_retry(&client) {
                    mcp_catalog_lock().error = Some(error);
                    break;
                }
                std::thread::sleep(MCP_CATALOG_FILL_PAUSE);
            }
            mcp_catalog_lock().filling = false;
        });

    if spawned.is_err() {
        mcp_catalog_lock().filling = false;
    }
}

/// Drop a catalog that has gone stale, so the next request re-walks it.
fn mcp_catalog_expire_if_stale() {
    let mut index = mcp_catalog_lock();
    let stale = index
        .fetched_at
        .is_some_and(|at| at.elapsed() > MCP_CATALOG_TTL);
    if stale {
        index.entries.clear();
        index.ids.clear();
        index.cursor = None;
        index.exhausted = false;
        index.fetched_at = None;
    }
}

/// What one request needs to know about the index.
struct McpCatalogWindow {
    /// The matching entries the caller asked for, already cut to its limit.
    entries: Vec<McpMarketEntry>,
    /// How many indexed entries matched, before the limit.
    total_matches: usize,
    /// How many entries the index holds in total.
    catalog_size: usize,
    exhausted: bool,
    error: Option<VibexError>,
}

/// Wait for the index to satisfy `ready`, then cut the window the caller asked
/// for while still holding the lock.
///
/// Polling rather than a condition variable: a page takes seconds, so the
/// poll's own cost is noise, and this keeps the walker free to update the index
/// without any handshake to get wrong.
fn mcp_catalog_wait_for_window(
    deadline: Instant,
    query: &str,
    limit: usize,
    mut ready: impl FnMut(&McpCatalogIndex) -> bool,
) -> McpCatalogWindow {
    loop {
        {
            let index = mcp_catalog_lock();
            if ready(&index) || Instant::now() >= deadline {
                let total_matches = index
                    .entries
                    .iter()
                    .filter(|entry| mcp_entry_matches(entry, query))
                    .count();
                let entries = index
                    .entries
                    .iter()
                    .filter(|entry| mcp_entry_matches(entry, query))
                    .take(limit)
                    .cloned()
                    .collect();
                return McpCatalogWindow {
                    entries,
                    total_matches,
                    catalog_size: index.entries.len(),
                    exhausted: index.exhausted,
                    error: index.error.clone(),
                };
            }
        }
        std::thread::sleep(MCP_CATALOG_POLL);
    }
}

/// Whether one entry matches a lowercased query.
///
/// The registry's own search matches names only, which is why a query for what
/// a server does — "database", "screenshot" — finds nothing there. Matching the
/// description and publisher too is what makes the market's search worth using.
fn mcp_entry_matches(entry: &McpMarketEntry, query: &str) -> bool {
    if query.is_empty() {
        return true;
    }
    let haystacks = [
        Some(entry.name.as_str()),
        entry.description.as_deref(),
        entry.author.as_deref(),
        entry.version.as_deref(),
        entry.package_kind.as_deref(),
    ];
    haystacks
        .into_iter()
        .flatten()
        .any(|text| text.to_lowercase().contains(query))
}

/// Answer one market request from the index, walking further when asked to.
fn search_mcp_catalog(request: &McpMarketSearchRequest) -> VibexResult<McpMarketSearchResponse> {
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_lowercase();
    let query = query
        .chars()
        .take(MCP_CATALOG_MAX_QUERY_CHARS)
        .collect::<String>();
    let limit = request
        .limit
        .unwrap_or(MCP_REGISTRY_PAGE_SIZE)
        .clamp(1, MAX_ENTRIES_PER_SEARCH as u32) as usize;
    let extend = request.extend.unwrap_or(false);

    // A caller that did not ask to extend only needs the index to hold
    // something worth showing, so a cold start waits for one page and an
    // already-warm index answers at once. A caller that did ask to extend —
    // someone who pressed Search — waits for the window it asked for.
    let wanted = if extend {
        limit
    } else {
        (MCP_REGISTRY_PAGE_SIZE as usize).min(limit)
    };

    mcp_catalog_expire_if_stale();
    // The walker is what fills the index, so it has to be running before there
    // is anything to wait for.
    mcp_catalog_ensure_walker();

    let deadline = Instant::now() + MCP_CATALOG_WAIT;
    let window = mcp_catalog_wait_for_window(deadline, &query, limit, |index| {
        index.exhausted
            || index.entries.len() >= wanted
            || index.entries.len() >= MCP_CATALOG_MAX_ENTRIES
    });

    // An index that holds nothing and recorded a failure is an outage, not a
    // search that found nothing. Reporting the difference is the whole reason
    // the walker keeps the error instead of dropping it.
    if window.catalog_size == 0
        && let Some(error) = window.error
    {
        return Err(error);
    }

    // A search that ran out of indexed entries has not finished searching, so
    // it keeps the walker going rather than reporting an answer it knows is
    // incomplete.
    if !window.exhausted && (query.is_empty() || window.total_matches < limit) {
        mcp_catalog_ensure_walker();
    }

    Ok(McpMarketSearchResponse {
        entries: window.entries,
        has_more: !window.exhausted,
        catalog_size: window.catalog_size,
        catalog_exhausted: window.exhausted,
        total_matches: window.total_matches,
    })
}

// ---------------------------------------------------------------------------
// Skill index adapter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SkillIndexResponse {
    #[serde(default)]
    skills: Vec<SkillIndexSkill>,
}

#[derive(Debug, Deserialize)]
struct SkillIndexSkill {
    id: String,
    #[serde(rename = "skillId")]
    skill_id: String,
    name: String,
    #[serde(default)]
    installs: u64,
    source: String,
}

fn search_skill_index(
    client: &reqwest::blocking::Client,
    query: &str,
    limit: u32,
    offset: u32,
) -> VibexResult<SkillMarketSearchResponse> {
    let url = format!(
        "{SKILL_INDEX_SEARCH}?q={}&limit={limit}&offset={offset}",
        urlencode(query)
    );
    let response: SkillIndexResponse = fetch_market_json(client, &url)?;
    let entries = response
        .skills
        .into_iter()
        .filter(|skill| {
            // The document is resolved from the repository later, so an entry
            // naming something that is not `owner/repo` cannot be installed and
            // must not be listed.
            parse_github_source(&skill.source).is_ok()
        })
        .map(|skill| SkillMarketEntry {
            id: skill.id,
            skill_id: skill.skill_id,
            name: skill.name,
            source: skill.source,
            installs: skill.installs,
        })
        .take(MAX_ENTRIES_PER_SEARCH)
        .collect::<Vec<_>>();
    // The index caps the `count` it reports at the page size, so it is the size
    // of this page rather than a grand total, and it ignores `offset` outright.
    // A second page therefore cannot be fetched, and the honest answer is that
    // this response is the whole of what the index would hand over.
    Ok(SkillMarketSearchResponse {
        total: entries.len() as u64,
        entries,
        has_more: false,
    })
}

/// What a caller's query means to the index.
struct SkillIndexQuery {
    /// The query actually sent upstream.
    text: String,
    /// True when the caller did not ask for anything in particular, so the
    /// result is a browse list rather than a relevance ranking.
    browsing: bool,
}

/// Resolve a caller's query into the one actually sent to the index.
///
/// An empty or single-character query is not a failed search, it is the browse
/// view: the caller has not asked for anything in particular yet. The index
/// cannot express that, so those become the broad browse query. Anything the
/// index would accept is passed through untouched.
fn skill_index_query(query: Option<&str>) -> SkillIndexQuery {
    let query = query.map(str::trim).unwrap_or_default();
    if query.chars().count() < SKILL_INDEX_MIN_QUERY_CHARS {
        return SkillIndexQuery {
            text: SKILL_INDEX_BROWSE_QUERY.to_string(),
            browsing: true,
        };
    }
    SkillIndexQuery {
        text: query.to_string(),
        browsing: false,
    }
}

/// Split an `owner/repo` reference, rejecting anything that could escape it.
fn parse_github_source(source: &str) -> VibexResult<(String, String)> {
    let mut parts = source.trim().split('/');
    let owner = parts.next().unwrap_or_default().trim();
    let repo = parts.next().unwrap_or_default().trim();
    if owner.is_empty() || repo.is_empty() || parts.next().is_some() {
        return Err(VibexError::validation(
            "market_skill_source_invalid",
            "a skill source must be an owner/repo reference",
        ));
    }
    let allowed = |value: &str| {
        value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.'))
    };
    if !allowed(owner) || !allowed(repo) {
        return Err(VibexError::validation(
            "market_skill_source_invalid",
            "a skill source must be an owner/repo reference",
        ));
    }
    Ok((owner.to_string(), repo.trim_end_matches(".git").to_string()))
}

#[derive(Debug, Deserialize)]
struct JsDelivrListing {
    #[serde(default)]
    files: Vec<JsDelivrFile>,
}

#[derive(Debug, Deserialize)]
struct JsDelivrFile {
    name: String,
}

/// Resolve a skill directory to the raw URL of its `SKILL.md`.
///
/// The index publishes a directory name, not a path, so the repository is
/// listed and the directory is matched by its last path segment. A repository
/// holding exactly one skill resolves even when the names disagree.
fn resolve_skill_document_url(
    client: &reqwest::blocking::Client,
    source: &str,
    skill_id: &str,
) -> VibexResult<String> {
    let (owner, repo) = parse_github_source(source)?;
    for branch in SKILL_BRANCHES {
        let listing_url = format!("{JSDELIVR_DATA}/{owner}/{repo}@{branch}?structure=flat");
        let Ok(listing) = fetch_market_json::<JsDelivrListing>(client, &listing_url) else {
            continue;
        };
        let documents = listing
            .files
            .iter()
            .map(|file| file.name.trim_start_matches('/').to_string())
            .filter(|path| {
                let lower = path.to_ascii_lowercase();
                lower == "skill.md" || lower.ends_with("/skill.md")
            })
            .collect::<Vec<_>>();
        if documents.is_empty() {
            continue;
        }
        let matched = documents
            .iter()
            .find(|path| {
                path.rsplit_once('/')
                    .map(|(dir, _)| dir.rsplit('/').next().unwrap_or_default())
                    .is_some_and(|dir| dir.eq_ignore_ascii_case(skill_id))
            })
            .or_else(|| (documents.len() == 1).then(|| &documents[0]));
        if let Some(path) = matched {
            let url = format!("{JSDELIVR_CDN}/{owner}/{repo}@{branch}/{path}");
            if market_url_policy(&url).is_ok() {
                return Ok(url);
            }
        }
    }
    Err(VibexError::validation(
        "market_skill_document_not_found",
        "the skill document could not be located in its repository",
    )
    .with_diagnostic("source", source.to_string())
    .with_diagnostic("skillId", skill_id.to_string()))
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
    /// Answer one market query from the registry index.
    ///
    /// The index is walked in the background instead of queried upstream: the
    /// registry's own `search` takes tens of seconds, which no interactive
    /// view can wait for. This returns as soon as the index holds enough to
    /// answer, and reports how much of the registry it covered when it could
    /// not answer in full.
    pub fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> VibexResult<McpMarketSearchResponse> {
        search_mcp_catalog(&request)
    }

    pub fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> VibexResult<SkillMarketSearchResponse> {
        let client = market_http_client()?;
        let query = skill_index_query(request.query.as_deref());
        let limit = request.limit.unwrap_or(30).clamp(1, 100);
        let mut response =
            search_skill_index(&client, &query.text, limit, request.offset.unwrap_or(0))?;
        if query.browsing {
            // A browse query carries no relevance signal — the index only
            // matched it against a broad term — so it is reordered into the one
            // ranking a market without a query should read as: most installed
            // first. A real search keeps the index's own relevance order.
            response
                .entries
                .sort_by_key(|entry| std::cmp::Reverse(entry.installs));
        }
        Ok(response)
    }

    pub fn skill_market_document(
        &self,
        request: SkillMarketDocumentRequest,
    ) -> VibexResult<SkillMarketDocument> {
        let client = market_http_client()?;
        let document_url = resolve_skill_document_url(&client, &request.source, &request.skill_id)?;
        let (body, _) = fetch_market_bytes(&client, &document_url, MAX_SKILL_DOCUMENT_FETCH_BYTES)?;
        let text = String::from_utf8(body).map_err(|_| {
            VibexError::provider(
                "market_skill_document_not_utf8",
                "skill documents must be UTF-8 markdown",
            )
        })?;
        Ok(skill_document_from_text(&request.entry_id, &text))
    }
}

// ---------------------------------------------------------------------------
// Install
// ---------------------------------------------------------------------------

/// Whether an agent can host this transport.
///
/// Codex and DeepSeek read MCP descriptors but reject SSE, so a market install
/// must not claim to have enabled a server the agent cannot start. The DeepSeek
/// Agent's id is `deepseek-harness`; matching a bare `deepseek` never fired.
fn agent_can_host_transport(agent_id: &AgentId, transport: McpServerTransportKind) -> bool {
    if transport != McpServerTransportKind::Sse {
        return true;
    }
    !matches!(agent_id.as_str(), "codex" | "deepseek-harness")
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
        let candidate = request.candidate.clone();
        validate_mcp_create_request(&candidate)?;

        // Resolve the delivery channel before writing anything. An Agent that
        // has no channel at all is refused up front instead of getting an
        // `enabled` row that can never produce a tool.
        let mut diagnostics = Vec::new();
        let mut hostable: Vec<AgentId> = Vec::new();
        let mut native_agents: Vec<AgentId> = Vec::new();
        let mut skipped: Vec<AgentId> = Vec::new();
        for agent_id in &request.agent_ids {
            match agent_mcp_delivery(agent_id.as_str()) {
                AgentMcpDelivery::Unsupported => {
                    diagnostics.push(diagnostic(
                        "marketInstallAgentUnsupported",
                        agent_id.as_str(),
                    ));
                    skipped.push(agent_id.clone());
                }
                AgentMcpDelivery::NativeFile => {
                    if !agent_has_native_mcp_file(agent_id.as_str()) {
                        diagnostics.push(diagnostic(
                            "marketInstallAgentUnsupported",
                            agent_id.as_str(),
                        ));
                        skipped.push(agent_id.clone());
                    } else if native_mcp_surface_supports_transport(
                        agent_id.as_str(),
                        candidate.transport_kind,
                    ) {
                        hostable.push(agent_id.clone());
                        native_agents.push(agent_id.clone());
                    } else {
                        // A TOML or YAML surface only knows the stdio shape, so
                        // an HTTP or SSE entry has nowhere to go.
                        diagnostics.push(diagnostic(
                            "marketInstallTransportSkipped",
                            agent_id.as_str(),
                        ));
                        skipped.push(agent_id.clone());
                    }
                }
                AgentMcpDelivery::Wire => {
                    if agent_can_host_transport(agent_id, candidate.transport_kind) {
                        hostable.push(agent_id.clone());
                    } else {
                        diagnostics.push(diagnostic(
                            "marketInstallTransportSkipped",
                            agent_id.as_str(),
                        ));
                        skipped.push(agent_id.clone());
                    }
                }
            }
        }
        if hostable.is_empty() {
            return Err(VibexError::validation(
                "market_install_no_hostable_agent",
                "none of the selected agents can receive MCP servers",
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
        } else {
            McpServerRepository::insert(&conn, &server)?;
        }
        McpServerRepository::replace_agent_matrix(&conn, &server.id, &server.agent_matrix)?;
        let mut readback = McpServerRepository::get(&conn, &server.id)?.ok_or_else(|| {
            VibexError::storage(
                "market_install_readback_missing",
                "the installed MCP server could not be read back",
            )
        })?;

        // Agents that read their own MCP file receive the server now, not on
        // some later manual export. A write that cannot happen is not a
        // delivery, so the enabled row is withdrawn and the Agent is reported
        // as skipped rather than left claiming a server it cannot call.
        let mut matrix_rewrite_needed = false;
        for agent_id in &native_agents {
            let servers =
                McpServerRepository::list_enabled_for_agent(&conn, agent_id, ProviderKind::Acp)?;
            match self.write_agent_native_mcp(agent_id, &servers) {
                AgentNativeMcpWrite::Written | AgentNativeMcpWrite::Unchanged => {
                    diagnostics.push(diagnostic(
                        "marketInstallNativeMcpWritten",
                        agent_id.as_str(),
                    ));
                }
                AgentNativeMcpWrite::NotWritten(reason) => {
                    diagnostics.push(diagnostic(
                        "marketInstallNativeMcpNotWritten",
                        format!("{}: {reason}", agent_id.as_str()),
                    ));
                    set_agent_matrix_entry(&mut readback.agent_matrix, agent_id, false, now);
                    hostable.retain(|enabled| enabled != agent_id);
                    skipped.push(agent_id.clone());
                    matrix_rewrite_needed = true;
                }
            }
        }
        if matrix_rewrite_needed {
            McpServerRepository::replace_agent_matrix(&conn, &readback.id, &readback.agent_matrix)?;
            readback = McpServerRepository::get(&conn, &readback.id)?.ok_or_else(|| {
                VibexError::storage(
                    "market_install_readback_missing",
                    "the installed MCP server could not be read back",
                )
            })?;
        }

        if !skipped.is_empty() {
            diagnostics.push(diagnostic(
                "marketInstallSkippedAgents",
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
            source_uri: Some(format!("market:{}", request.entry_id)),
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
        } else {
            SkillRepository::insert(&conn, &skill)?;
        }
        SkillRepository::replace_agent_matrix(&conn, &skill.id, &skill.agent_matrix)?;
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

    fn registry_package_of(
        registry_type: &str,
        identifier: &str,
        version: &str,
    ) -> RegistryPackage {
        RegistryPackage {
            registry_type: Some(registry_type.to_string()),
            identifier: Some(identifier.to_string()),
            version: Some(version.to_string()),
            runtime_hint: None,
            runtime_arguments: None,
            package_arguments: None,
            environment_variables: None,
        }
    }

    fn registry_record(name: &str, packages: Vec<RegistryPackage>) -> RegistryServerRecord {
        RegistryServerRecord {
            server: Some(RegistryServer {
                name: name.to_string(),
                title: None,
                description: None,
                homepage: None,
                website_url: None,
                repository: None,
                packages,
                remotes: Vec::new(),
            }),
            meta: None,
        }
    }

    /// The registry lists packages in publisher order, and that order is not a
    /// statement about which one this product can run: an oci-first record used
    /// to install `npx <image>`, a command that cannot start.
    #[test]
    fn registry_launch_form_prefers_a_runnable_package() {
        let entry = registry_server_to_entry(&registry_record(
            "io.example/docker-first",
            vec![
                registry_package_of("oci", "docker.io/example/server:1.0.0", "1.0.0"),
                registry_package_of("npm", "example-server", "1.2.3"),
            ],
        ))
        .expect("an npm package is installable");
        assert_eq!(entry.command.as_deref(), Some("npx"));
        assert_eq!(entry.package_kind.as_deref(), Some("npm"));
        assert_eq!(
            entry.args.last().map(String::as_str),
            Some("example-server@1.2.3")
        );
        assert!(
            !entry.args.iter().any(|arg| arg.contains("docker.io")),
            "the container image must not reach the launcher: {:?}",
            entry.args
        );

        // pypi is the second preference, ahead of any remote.
        let entry = registry_server_to_entry(&registry_record(
            "io.example/pypi",
            vec![registry_package_of("pypi", "example-mcp", "2.0.0")],
        ))
        .expect("a pypi package is installable");
        assert_eq!(entry.command.as_deref(), Some("uvx"));
        assert_eq!(entry.package_kind.as_deref(), Some("pypi"));

        // A record with only a container package and no remote is not
        // installable, so it must not be listed at all.
        assert!(
            registry_server_to_entry(&registry_record(
                "io.example/oci-only",
                vec![registry_package_of(
                    "oci",
                    "docker.io/example/only:1.0.0",
                    "1.0.0"
                )],
            ))
            .is_none()
        );
    }

    #[test]
    fn registry_launcher_arguments_keep_flags_and_npx_only_switches() {
        let mut npm = registry_package_of("npm", "example-server", "1.2.3");
        npm.runtime_arguments = Some(vec![RegistryArgument {
            argument_type: Some("named".to_string()),
            name: Some("--registry".to_string()),
            value: Some("https://registry.example".to_string()),
        }]);
        npm.package_arguments = Some(vec![
            RegistryArgument {
                argument_type: Some("named".to_string()),
                name: Some("--port".to_string()),
                value: Some("8080".to_string()),
            },
            // A flag with no value keeps its name and adds nothing else.
            RegistryArgument {
                argument_type: Some("named".to_string()),
                name: Some("--verbose".to_string()),
                value: None,
            },
        ]);
        let entry = registry_server_to_entry(&registry_record("io.example/npm", vec![npm]))
            .expect("the npm package is installable");
        assert_eq!(
            entry.args,
            vec![
                "--registry",
                "https://registry.example",
                "-y",
                "example-server@1.2.3",
                "--port",
                "8080",
                "--verbose",
            ]
        );

        // `-y` is npx's own flag. uvx has no equivalent, so handing it one
        // would make the launcher reject the command.
        let entry = registry_server_to_entry(&registry_record(
            "io.example/pypi",
            vec![registry_package_of("pypi", "example-mcp", "2.0.0")],
        ))
        .expect("the pypi package is installable");
        assert_eq!(entry.args, vec!["example-mcp==2.0.0"]);
    }

    /// The registry labels credentials itself; the name heuristic only covers
    /// records that predate the label.
    #[test]
    fn registry_env_secrets_follow_the_published_label() {
        let mut package = registry_package_of("npm", "example-server", "1.0.0");
        package.environment_variables = Some(vec![
            RegistryEnvVar {
                name: Some("REGION".to_string()),
                description: None,
                is_required: Some(true),
                is_secret: Some(true),
                value: None,
                default: None,
            },
            RegistryEnvVar {
                name: Some("PUBLIC_LABEL".to_string()),
                description: None,
                is_required: Some(false),
                is_secret: Some(false),
                value: None,
                default: None,
            },
            RegistryEnvVar {
                name: Some("API_KEY".to_string()),
                description: None,
                is_required: None,
                is_secret: None,
                value: None,
                default: None,
            },
        ]);
        let requirements = registry_env_requirements(&package);
        let secret = |name: &str| {
            requirements
                .iter()
                .find(|requirement| requirement.name == name)
                .expect("the variable is declared")
                .secret
        };
        // An explicit `isSecret` wins in both directions.
        assert!(secret("REGION"));
        assert!(!secret("PUBLIC_LABEL"));
        // Without one, a credential-shaped name is still masked.
        assert!(secret("API_KEY"));
    }

    #[test]
    fn registry_entries_carry_repository_status_and_package_kind() {
        let mut record = registry_record(
            "io.github.example/airtable",
            vec![registry_package_of("npm", "airtable-mcp-server", "1.14.0")],
        );
        record.server.as_mut().expect("server").repository =
            Some(RegistryRepository::Detail(RegistryRepositoryDetail {
                url: Some("https://github.com/example/airtable-mcp-server.git".to_string()),
            }));
        record.meta = Some(RegistryRecordMeta {
            official: Some(RegistryOfficialMeta {
                status: Some("deprecated".to_string()),
                updated_at: Some("2026-07-27T15:34:57Z".to_string()),
            }),
        });

        let entry = registry_server_to_entry(&record).expect("the record is installable");
        assert_eq!(
            entry.repository.as_deref(),
            Some("https://github.com/example/airtable-mcp-server.git")
        );
        // A record with no homepage or website falls back to its repository,
        // so the UI always has something to link to.
        assert_eq!(entry.homepage, entry.repository);
        assert_eq!(entry.author.as_deref(), Some("io.github.example"));
        assert_eq!(entry.status.as_deref(), Some("deprecated"));
        assert_eq!(entry.updated_at.as_deref(), Some("2026-07-27T15:34:57Z"));
    }

    /// The registry's own search matches names only, so a query for what a
    /// server does finds nothing there. The market's search has to look wider.
    #[test]
    fn market_search_matches_more_than_the_name() {
        let entry = McpMarketEntry {
            id: "io.example/browser".to_string(),
            name: "Browser Tool".to_string(),
            description: Some("Capture a screenshot of any page".to_string()),
            homepage: None,
            repository: None,
            author: Some("io.example".to_string()),
            status: None,
            updated_at: None,
            transport: McpServerTransportKind::Http,
            command: None,
            args: Vec::new(),
            url: Some("https://example.com/mcp".to_string()),
            env: Vec::new(),
            version: Some("1.0.0".to_string()),
            package_kind: Some("remote".to_string()),
        };
        assert!(mcp_entry_matches(&entry, ""));
        assert!(mcp_entry_matches(&entry, "browser"));
        assert!(mcp_entry_matches(&entry, "screenshot"));
        assert!(mcp_entry_matches(&entry, "io.example"));
        assert!(!mcp_entry_matches(&entry, "database"));
    }

    #[test]
    fn registry_page_url_walks_the_cursor_and_encodes_it() {
        assert_eq!(
            mcp_registry_page_url(None),
            "https://registry.modelcontextprotocol.io/v0.1/servers?version=latest&limit=100"
        );
        // A cursor is `name:version`, and the slash in a name has to survive
        // the round trip.
        let url = mcp_registry_page_url(Some("ai.example/server:1.0.0"));
        assert!(
            url.ends_with("&cursor=ai.example%2Fserver%3A1.0.0"),
            "{url}"
        );
        assert!(!mcp_registry_page_url(Some("")).contains("cursor"));
    }

    /// A real page of the registry, captured from the live API.
    ///
    /// The record carries fields this adapter deliberately ignores (`icons`,
    /// each package's own `transport`), so it also pins that unknown fields
    /// stay harmless. It is the shape a page actually has, not the shape the
    /// schema suggests it might have.
    const REGISTRY_FIXTURE: &str = r#"{
      "servers": [
        {
          "server": {
            "$schema": "https://static.modelcontextprotocol.io/schemas/2025-10-17/server.schema.json",
            "name": "io.github.domdomegg/airtable-mcp-server",
            "description": "Read and write access to Airtable database schemas, tables, and records.",
            "title": "Airtable",
            "repository": {"url": "https://github.com/domdomegg/airtable-mcp-server.git", "source": "github"},
            "version": "1.14.0",
            "websiteUrl": "https://github.com/domdomegg/airtable-mcp-server#readme",
            "icons": [{"src": "https://example.com/icon.png", "mimeType": "image/png"}],
            "packages": [
              {
                "registryType": "npm",
                "identifier": "airtable-mcp-server",
                "version": "1.14.0",
                "runtimeHint": "npx",
                "transport": {"type": "stdio"},
                "environmentVariables": [
                  {
                    "description": "Airtable personal access token.",
                    "isRequired": true,
                    "isSecret": true,
                    "name": "AIRTABLE_API_KEY"
                  }
                ]
              },
              {
                "registryType": "oci",
                "identifier": "docker.io/domdomegg/airtable-mcp-server:1.14.0",
                "transport": {"type": "stdio"}
              }
            ]
          },
          "_meta": {
            "io.modelcontextprotocol.registry/official": {
              "status": "active",
              "updatedAt": "2026-07-27T15:34:57.496953Z",
              "isLatest": true
            }
          }
        },
        {
          "server": {
            "name": "ac.tandem/docs-mcp",
            "description": "Remote MCP server for Tandem docs.",
            "repository": {"url": "https://github.com/frumu-ai/tandem", "source": "github"},
            "version": "0.3.2",
            "websiteUrl": "https://tandem.ac/docs-mcp",
            "remotes": [{"type": "streamable-http", "url": "https://tandem.ac/mcp"}]
          },
          "_meta": {
            "io.modelcontextprotocol.registry/official": {
              "status": "active",
              "updatedAt": "2026-04-22T21:06:34.500049Z"
            }
          }
        }
      ],
      "metadata": {"nextCursor": "ac.tandem/docs-mcp:0.3.2", "count": 2}
    }"#;

    #[test]
    fn real_registry_page_maps_to_installable_entries() {
        let page: RegistryListResponse =
            serde_json::from_str(REGISTRY_FIXTURE).expect("the captured page parses");
        assert_eq!(page.servers.len(), 2);
        assert_eq!(
            page.metadata
                .and_then(|metadata| metadata.next_cursor)
                .as_deref(),
            Some("ac.tandem/docs-mcp:0.3.2")
        );

        let npm = registry_server_to_entry(&page.servers[0]).expect("npm record is installable");
        assert_eq!(npm.name, "Airtable");
        assert_eq!(npm.command.as_deref(), Some("npx"));
        // The npm package wins over the oci one that follows it.
        assert_eq!(npm.args, vec!["-y", "airtable-mcp-server@1.14.0"]);
        assert_eq!(npm.package_kind.as_deref(), Some("npm"));
        assert_eq!(npm.version.as_deref(), Some("1.14.0"));
        // `isSecret` is published, so the install form masks the token without
        // having to guess from the name.
        assert_eq!(npm.env.len(), 1);
        assert!(npm.env[0].secret && npm.env[0].required);
        assert_eq!(npm.env[0].name, "AIRTABLE_API_KEY");

        let remote = registry_server_to_entry(&page.servers[1]).expect("remote is installable");
        assert_eq!(remote.transport, McpServerTransportKind::Http);
        assert_eq!(remote.url.as_deref(), Some("https://tandem.ac/mcp"));
        assert_eq!(remote.package_kind.as_deref(), Some("remote"));
        // The website wins over the repository, which is only the fallback.
        assert_eq!(
            remote.homepage.as_deref(),
            Some("https://tandem.ac/docs-mcp")
        );
        assert_eq!(
            remote.repository.as_deref(),
            Some("https://github.com/frumu-ai/tandem")
        );
    }

    /// Walks the real registry. Ignored by default because it needs the
    /// network; run it with `cargo test -p vibex-config-switch -- --ignored`
    /// when the registry's shape or latency is in question.
    ///
    /// This is the check that the market answers at all: the registry's own
    /// `search` parameter used to be the only way to search, and it takes tens
    /// of seconds, so the thing worth proving is that a page lands quickly and
    /// that a query is answered from what landed.
    #[test]
    #[ignore = "hits the live MCP registry"]
    fn live_registry_index_fills_and_searches() {
        let started = Instant::now();
        let browse = search_mcp_catalog(&McpMarketSearchRequest {
            query: None,
            limit: Some(100),
            offset: None,
            extend: Some(true),
        })
        .expect("the live registry answers a browse");
        let elapsed = started.elapsed();
        assert!(
            browse.entries.len() >= 100,
            "a browse window should hold a full page, got {}",
            browse.entries.len()
        );
        assert!(browse.catalog_size >= browse.entries.len());
        assert!(
            elapsed < MCP_CATALOG_WAIT + MARKET_REQUEST_TIMEOUT,
            "a browse should answer inside one wait, took {elapsed:?}"
        );

        // Searching for something the index actually holds must find it. A
        // term from deeper in the catalog may legitimately miss: the index is
        // filled one page at a time, so a cold search only covers what has
        // landed so far.
        let needle = browse.entries[0].name.to_lowercase();
        let needle = needle
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .to_string();
        let search = search_mcp_catalog(&McpMarketSearchRequest {
            query: Some(needle.clone()),
            limit: Some(50),
            offset: None,
            extend: Some(true),
        })
        .expect("the live registry answers a search");
        assert!(
            search.total_matches >= 1,
            "searching {needle:?} found nothing among {} indexed entries",
            search.catalog_size
        );
        assert!(
            search
                .entries
                .iter()
                .any(|entry| mcp_entry_matches(entry, &needle))
        );
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
    fn skill_source_must_be_a_plain_owner_repo_pair() {
        assert_eq!(
            parse_github_source("anthropics/skills").unwrap(),
            ("anthropics".to_string(), "skills".to_string())
        );
        assert_eq!(
            parse_github_source("anthropics/skills.git").unwrap().1,
            "skills"
        );
        // A nested path could point the fetch at another repository's subtree.
        assert!(parse_github_source("anthropics/skills/pdf").is_err());
        assert!(parse_github_source("anthropics").is_err());
        assert!(parse_github_source("").is_err());
        assert!(parse_github_source("../../etc/passwd").is_err());
    }

    #[test]
    fn browse_query_stands_in_for_an_empty_search() {
        // The index rejects a query shorter than two characters, so the browse
        // view must never send one through.
        for empty in [None, Some(""), Some("   "), Some("a")] {
            let resolved = skill_index_query(empty);
            assert_eq!(resolved.text, SKILL_INDEX_BROWSE_QUERY);
            assert!(
                resolved.browsing,
                "{empty:?} must resolve to the browse view"
            );
        }
        // Two characters is the first query the index accepts, so it is passed
        // through rather than replaced, and it stays a search.
        for search in ["ai", "kubernetes"] {
            let resolved = skill_index_query(Some(search));
            assert_eq!(resolved.text, search);
            assert!(!resolved.browsing, "{search:?} must stay a search");
        }
        let padded = skill_index_query(Some("  ai  "));
        assert_eq!(padded.text, "ai");
        assert!(!padded.browsing);
    }

    #[test]
    fn browse_query_is_long_enough_for_the_index() {
        assert!(
            SKILL_INDEX_BROWSE_QUERY.chars().count() >= SKILL_INDEX_MIN_QUERY_CHARS,
            "the browse query must satisfy the index's own minimum"
        );
    }
}

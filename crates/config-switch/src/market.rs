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
//! market reads ClawHub, a public Skill registry that publishes both a catalog
//! and the skill folders themselves. There is no user-configured source list, so
//! there is no policy for one.
//!
//! **A market never invents an entry.** Everything the UI shows came out of an
//! upstream response; an entry the upstream did not publish cannot be listed.
//!
//! ## What a Skill install actually writes
//!
//! A published Skill is a folder, not a file: `SKILL.md` plus the references,
//! scripts and templates its instructions point at. ClawHub serves that folder
//! as a zip archive, so the market downloads the archive, and unpacks it under
//! this runtime's own store rather than into any Agent's Skills folder. Each
//! Agent then receives the folder through the same native Skill export a
//! hand-imported Skill uses, which is what keeps one install path for every
//! Agent instead of one per agent.
//!
//! The archive is untrusted input, so unpacking is a policy of its own: entry
//! names are confined to the Skill folder, symlinks and directories are
//! ignored, only UTF-8 text is written, and per-file, per-file-count and
//! total-byte ceilings bound a decompression bomb. Every file that is refused
//! is reported to the caller rather than dropped, because a Skill whose scripts
//! silently did not arrive is not the Skill the publisher described.
//!
//! ## How an MCP search is answered
//!
//! The registry is asked the question the reader asked. Its `search` parameter
//! ranks nothing but it does cover the whole catalog, and it answers in about a
//! second, so the runtime sends the query upstream and pages the cursor the
//! registry hands back. Nothing is indexed ahead of time: an entry that exists
//! at the registry is reachable the first time somebody asks for it, which a
//! walk of the whole catalog cannot promise within an interactive wait.
//!
//! Two properties follow from that and are load-bearing:
//!
//! - A query shorter than the registry accepts is a browse, not a failure. The
//!   same endpoint answers it in the order the registry publishes.
//! - The transport filter is applied to the window, never upstream. Filtering
//!   before paging would make a page of twenty depend on how many of a hundred
//!   happened to match, which is a pager that skips entries.
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

use std::collections::HashMap;
use std::io::Read;
use std::net::IpAddr;
use std::sync::{LazyLock, Mutex};
use std::time::Duration;

use serde::Deserialize;
use vibex_core::{
    AgentId, MAX_MARKET_ICON_BYTES, MAX_MARKET_RESPONSE_BYTES, MAX_SKILL_MARKET_BUNDLE_BYTES,
    MAX_SKILL_MARKET_BUNDLE_DOWNLOAD_BYTES, MAX_SKILL_MARKET_BUNDLE_FILE_BYTES,
    MAX_SKILL_MARKET_BUNDLE_FILES, MAX_SKILL_MARKET_DOCUMENT_BYTES, MarketEnvRequirement,
    MarketEnvTarget, McpMarketEntry, McpMarketIcon, McpMarketInstallRequest,
    McpMarketInstallResult, McpMarketSearchRequest, McpMarketSearchResponse,
    McpServerTransportKind, ProviderKind, SkillCreateRequest, SkillMarketBundleFile,
    SkillMarketDocument, SkillMarketDocumentRequest, SkillMarketEntry, SkillMarketInstallRequest,
    SkillMarketInstallResult, SkillMarketSearchRequest, SkillMarketSearchResponse, SkillMarketSort,
    SkillScopeKind, SkillSourceKind, SkillStatus, VibexError, VibexResult,
};
use vibex_db::{McpServerRepository, SkillRepository};

use crate::mcp_delivery::{AgentMcpDelivery, agent_has_native_mcp_file, agent_mcp_delivery};
use crate::native_export::AgentNativeMcpWrite;
use crate::native_surface::native_mcp_surface_supports_transport;
use crate::skill_bundle::{market_skill_dir, market_skill_store_root, write_skill_bundle};
use crate::{
    ProviderConfigService, diagnostic, find_existing_mcp_server, normalize_mcp_create_request,
    normalize_skill_create_request, validate_mcp_create_request, validate_skill_create_request,
};

/// The official MCP registry. Its `v0.1` API is the only MCP catalog.
const MCP_REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
/// The public Skill registry, and the only Skill catalog.
///
/// The registry documents this surface: `GET /api/v1/openapi.json` is served by
/// the registry itself, which is why the paths and the ranking vocabulary below
/// are pinned to it rather than to a client's guess.
const CLAWHUB_API_BASE: &str = "https://clawhub.ai";
/// Lists and pages the catalog. The ranking and the cursor are the registry's.
const CLAWHUB_SKILLS_PATH: &str = "/api/v1/skills";
/// Ranks the catalog against a query. `q` is mandatory here.
const CLAWHUB_SEARCH_PATH: &str = "/api/v1/search";
/// Serves a Skill folder as a zip archive.
const CLAWHUB_DOWNLOAD_PATH: &str = "/api/v1/download";
/// Default page when a caller does not name one.
const CLAWHUB_PAGE_SIZE: u32 = 30;
/// Largest page the registry will serve in one answer.
const CLAWHUB_MAX_PAGE_SIZE: u32 = 100;
/// Shortest query the registry's search accepts. Anything shorter is a bad
/// request, so a caller that has not asked for anything is browsing instead.
const CLAWHUB_MIN_QUERY_CHARS: usize = 2;
/// Longest query worth sending.
const CLAWHUB_MAX_QUERY_CHARS: usize = 120;
/// Directory the manifest is looked up under inside a bundle. The Agent Skills
/// convention is a capitalised `SKILL.md` at the folder root, but published
/// bundles are not uniform, so the lowercase spelling is accepted too.
const SKILL_BUNDLE_MANIFEST_NAMES: [&str; 2] = ["SKILL.md", "skill.md"];

/// One request may not take longer than this, including every redirect.
///
/// The registry answers a search in about a second and a Skill bundle can take
/// longer, so the ceiling sits above both while still failing an interactive
/// read fast enough for the reader to retry it.
const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(15);
/// Redirect hops followed before the fetch is refused.
const MAX_MARKET_REDIRECTS: usize = 5;
/// How much of a refusal body is read.
///
/// A refusal is a message, not a catalog: the Skill registry's ambiguous-slug
/// answer names the candidate publishers, and that is the whole of what a
/// caller can act on. Reading it under its own ceiling keeps a hostile upstream
/// from answering a failed request with an unbounded body.
const MAX_MARKET_REFUSAL_BYTES: u64 = 16 * 1024;
/// Entries a single response may contribute, so one huge catalog cannot flood
/// the list.
const MAX_ENTRIES_PER_SEARCH: usize = 500;
/// Entries one market page holds when the caller does not name a size.
const MARKET_PAGE_LIMIT: u32 = 20;
/// A Skill document is markdown; anything past this is not one.
const MAX_SKILL_DOCUMENT_FETCH_BYTES: u64 = MAX_SKILL_MARKET_DOCUMENT_BYTES + 1;

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
    fetch_market_bytes_rejected_by(client, raw_url, limit, |status, _| {
        VibexError::provider(
            "market_rejected",
            format!("the market responded with HTTP {}", status.as_u16()),
        )
    })
}

/// Fetch one market URL, letting the caller read a refusal.
///
/// A refusal is not always just a status: the Skill registry answers an
/// ambiguous slug with the candidate publishers in the body, and that body is
/// the whole value of the response. Passing the refusal through a mapper keeps
/// one fetch implementation — and so one redirect and size policy — while
/// still letting an upstream with something to say be heard.
fn fetch_market_bytes_rejected_by(
    client: &reqwest::blocking::Client,
    raw_url: &str,
    limit: u64,
    on_rejected: impl Fn(reqwest::StatusCode, &[u8]) -> VibexError,
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
            // The refusal is read under its own small ceiling: it is a message,
            // not a catalog, and a hostile upstream must not be able to answer
            // a failed request with an unbounded body.
            let mut body = Vec::new();
            let _ = response
                .take(MAX_MARKET_REFUSAL_BYTES)
                .read_to_end(&mut body);
            let mut error = on_rejected(status, &body);
            if error.diagnostics.is_empty() {
                error = error.with_diagnostic("host", url.host_str().unwrap_or_default());
            }
            return Err(error);
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
    icons: Vec<RegistryIcon>,
    #[serde(default)]
    packages: Vec<RegistryPackage>,
    #[serde(default)]
    remotes: Vec<RegistryRemote>,
}

/// One picture a publisher offered for its entry.
///
/// The registry publishes a list because one address never fits every surface;
/// the market shows the first one it can fetch, and the type it declares is
/// only a hint — the bytes are sniffed before they are trusted.
#[derive(Debug, Deserialize)]
struct RegistryIcon {
    #[serde(default)]
    src: Option<String>,
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
    /// Headers the entry needs before it answers, each possibly a placeholder.
    ///
    /// A remote that authenticates with a header (`Authorization`, a vendor's
    /// own key) is unusable without this, so the market carries the header
    /// names into the install form instead of saving a connection that 401s.
    #[serde(default)]
    headers: Vec<RegistryEnvVar>,
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
                target: Some(MarketEnvTarget::Env),
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
        /// Header names the remote needs before it answers, in the order the
        /// registry declared them.
        headers: Vec<MarketEnvRequirement>,
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
        headers: registry_header_requirements(remote),
    })
}

/// The headers a remote entry needs, as the install form asks for them.
///
/// A header the registry fills in itself needs nothing from the reader; one it
/// leaves as `{placeholder}`, marks secret, or marks required is an input. The
/// header's name is the key, because that is what the saved server carries.
fn registry_header_requirements(remote: &RegistryRemote) -> Vec<MarketEnvRequirement> {
    remote
        .headers
        .iter()
        .filter_map(|header| {
            let name = header.name.as_deref()?.trim();
            if name.is_empty() {
                return None;
            }
            let value = header.value.as_deref().unwrap_or_default().trim();
            let placeholder = placeholder_in(value);
            // A literal value the publisher ships is not a question; only an
            // empty one, a placeholder, or an explicit requirement is.
            if !value.is_empty()
                && placeholder.is_none()
                && !header.is_required.unwrap_or(false)
                && !header.is_secret.unwrap_or(false)
            {
                return None;
            }
            Some(MarketEnvRequirement {
                name: name.to_string(),
                target: Some(MarketEnvTarget::Header),
                description: header
                    .description
                    .as_deref()
                    .map(str::trim)
                    .filter(|value| !value.is_empty())
                    .map(str::to_string)
                    .or_else(|| placeholder.filter(|value| !value.is_empty())),
                required: header.is_required.unwrap_or(false),
                secret: header
                    .is_secret
                    .unwrap_or_else(|| env_name_looks_secret(name)),
                default_value: None,
                placeholder: None,
            })
        })
        .collect()
}

/// The variable a header's value is spelled with, when it is a placeholder.
///
/// The registry writes `{api_key}`; the install form asks for the value, so the
/// placeholder is what tells the reader what to paste rather than a literal.
fn placeholder_in(value: &str) -> Option<String> {
    let start = value.find('{')?;
    let end = value[start..].find('}')? + start;
    let inner = value[start + 1..end].trim();
    (!inner.is_empty()).then(|| inner.to_string())
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
        RegistryLaunchForm::Remote { url, headers } => (
            McpServerTransportKind::Http,
            None,
            Vec::new(),
            Some(url),
            headers,
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
        icon_url: registry_icon(server),
    })
}

/// The picture an entry offers, when it offers one the market may load.
///
/// The registry's list is a preference order, so the first address that passes
/// the same public-`https` policy every market fetch passes is the one kept. An
/// entry with none keeps none: the card falls back to its transport glyph
/// rather than inventing a logo.
fn registry_icon(server: &RegistryServer) -> Option<String> {
    server
        .icons
        .iter()
        .filter_map(|icon| icon.src.as_deref())
        .map(str::trim)
        .find(|src| market_url_policy(src).is_ok())
        .map(str::to_string)
}

// ---------------------------------------------------------------------------
// MCP registry search
// ---------------------------------------------------------------------------

/// The entries of the last answer, by id.
///
/// An icon is asked for after the grid has the entry, and the entry is what
/// carries the picture's address; keeping the last answer is what lets the
/// picture be loaded without a second search. The map is small — one market
/// page plus a screen of alternatives — and is replaced rather than added to.
static MARKET_ENTRIES: LazyLock<Mutex<HashMap<String, McpMarketEntry>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// Pictures already loaded, by entry id.
///
/// A card is re-rendered on every scroll and every keystroke elsewhere in the
/// pane, so a picture that was fetched once must not be fetched again. The
/// cache is bounded by the number of entries a market ever lists in one run.
static MARKET_ICONS: LazyLock<Mutex<HashMap<String, McpMarketIcon>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn market_entry_cache_get(entry_id: &str) -> Option<McpMarketEntry> {
    MARKET_ENTRIES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(entry_id)
        .cloned()
}

fn market_entries_cache_put(entries: &[McpMarketEntry]) {
    let mut cache = MARKET_ENTRIES
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    for entry in entries {
        cache.insert(entry.id.clone(), entry.clone());
    }
}

fn market_icon_cache_get(entry_id: &str) -> Option<McpMarketIcon> {
    MARKET_ICONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .get(entry_id)
        .cloned()
}

fn market_icon_cache_put(entry_id: &str, icon: McpMarketIcon) {
    MARKET_ICONS
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(entry_id.to_string(), icon);
}

/// What a result set is keyed by: the question, not the page of it.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct McpSearchKey {
    query: String,
    transport: Option<String>,
}

/// One result set read in this run.
#[derive(Default)]
struct McpSearchSet {
    /// Everything read for this question, in registry order.
    entries: Vec<McpMarketEntry>,
    /// Where the next unwalked page starts, absent once the registry ended.
    cursor: Option<String>,
    /// Windows already answered, by `offset:limit`.
    windows: HashMap<String, McpMarketSearchResponse>,
}

/// Result sets read in this run, by question.
///
/// The market is one grid read one page at a time, so the pages a reader moves
/// through are pages of the same answer. Keeping the answer is what makes
/// paging back instant and stops a page from being re-walked from the first
/// result; the map is bounded because a session asks a bounded number of
/// questions and this is cleared with the process.
static MARKET_SEARCH: LazyLock<Mutex<HashMap<McpSearchKey, McpSearchSet>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

fn market_search_lock() -> std::sync::MutexGuard<'static, HashMap<McpSearchKey, McpSearchSet>> {
    MARKET_SEARCH
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

fn market_search_window_get(
    key: &McpSearchKey,
    offset: usize,
    limit: usize,
) -> Option<McpMarketSearchResponse> {
    market_search_lock()
        .get(key)
        .and_then(|set| set.windows.get(&format!("{offset}:{limit}")).cloned())
}

fn market_search_window_put(
    key: &McpSearchKey,
    offset: usize,
    limit: usize,
    response: &McpMarketSearchResponse,
) {
    let mut cache = market_search_lock();
    // A new question is a new result set; only the last handful is kept so a
    // long session of typing does not accumulate answers nobody returns to.
    if cache.len() >= 16 && !cache.contains_key(key) {
        cache.clear();
    }
    cache
        .entry(key.clone())
        .or_default()
        .windows
        .insert(format!("{offset}:{limit}"), response.clone());
}

fn market_search_entries_get(key: &McpSearchKey) -> Vec<McpMarketEntry> {
    market_search_lock()
        .get(key)
        .map(|set| set.entries.clone())
        .unwrap_or_default()
}

fn market_search_cursor_get(key: &McpSearchKey) -> Option<String> {
    market_search_lock()
        .get(key)
        .and_then(|set| set.cursor.clone())
}

fn market_search_entries_put(
    key: &McpSearchKey,
    entries: Vec<McpMarketEntry>,
    cursor: Option<String>,
) {
    let mut cache = market_search_lock();
    if cache.len() >= 16 && !cache.contains_key(key) {
        cache.clear();
    }
    let set = cache.entry(key.clone()).or_default();
    set.entries = entries;
    set.cursor = cursor;
}

/// Load one entry's picture, or the empty icon when it published none.
///
/// The bytes come from the publisher's own address, so the fetch goes through
/// the same public-`https`, redirect and size policy as every other market
/// read, and the type is decided by the bytes rather than by the address.
fn load_market_icon(entry_id: &str) -> VibexResult<McpMarketIcon> {
    let entry_id = entry_id.trim();
    if entry_id.is_empty() {
        return Err(VibexError::validation(
            "market_icon_entry_missing",
            "no market entry was named",
        ));
    }
    if let Some(icon) = market_icon_cache_get(entry_id) {
        return Ok(icon);
    }
    let entry = market_entry_cache_get(entry_id).ok_or_else(|| {
        VibexError::validation(
            "market_icon_entry_unknown",
            "the entry is not in the market's last answer; search for it again",
        )
    })?;
    let Some(url) = entry.icon_url.clone() else {
        return Ok(McpMarketIcon::default());
    };
    let client = market_http_client()?;
    let bytes = fetch_market_bytes(&client, &url, MAX_MARKET_ICON_BYTES)?.0;
    let mime = sniff_image_mime(&bytes).ok_or_else(|| {
        VibexError::provider(
            "market_icon_not_image",
            "the picture the entry named is not an image the market can show",
        )
        .with_diagnostic("entry", entry_id.to_string())
    })?;
    let icon = McpMarketIcon {
        mime: Some(mime.to_string()),
        bytes,
    };
    market_icon_cache_put(entry_id, icon.clone());
    Ok(icon)
}

/// The media type of an image, read from its bytes rather than its address.
///
/// A publisher names the type it meant; the bytes are what a renderer will act
/// on, so only the formats the desktop can decode are accepted, and an SVG is
/// recognised by its markup because it has no binary header.
fn sniff_image_mime(bytes: &[u8]) -> Option<&'static str> {
    match bytes {
        [0x89, b'P', b'N', b'G', ..] => Some("image/png"),
        [0xff, 0xd8, 0xff, ..] => Some("image/jpeg"),
        [b'G', b'I', b'F', b'8', ..] => Some("image/gif"),
        [
            b'R',
            b'I',
            b'F',
            b'F',
            _,
            _,
            _,
            _,
            b'W',
            b'E',
            b'B',
            b'P',
            ..,
        ] => Some("image/webp"),
        [b'B', b'M', ..] => Some("image/bmp"),
        [0, 0, 1, 0, ..] => Some("image/x-icon"),
        _ => {
            let head = String::from_utf8_lossy(&bytes[..bytes.len().min(512)]);
            let trimmed = head.trim_start();
            (trimmed.starts_with("<svg") || (trimmed.starts_with("<?xml") && head.contains("<svg")))
                .then_some("image/svg+xml")
        }
    }
}

/// The registry lists a page per request and will not serve more than this.
const MCP_REGISTRY_PAGE_SIZE: u32 = 100;

/// Shortest query the registry's search accepts. Anything shorter is refused
/// with a 422, so a shorter query is a browse rather than a failed search.
const MCP_REGISTRY_MIN_QUERY_CHARS: usize = 3;

/// Longest query worth sending: the registry matches a bounded name field, and
/// a caller pasting a paragraph is asking for a browse rather than a match.
const MCP_REGISTRY_MAX_QUERY_CHARS: usize = 120;

/// Pages one search may walk when the registry keeps handing back a cursor.
///
/// The registry ranks nothing and pages by name, so following its cursor is
/// what makes "results 101..200" reachable. Four pages of a hundred is more
/// than one screen of results ever shows while keeping one request bounded.
const MCP_REGISTRY_SEARCH_PAGES: usize = 4;

/// A search answer plus the cursor the next page of it starts at.
struct McpSearchPage {
    entries: Vec<McpMarketEntry>,
    next_cursor: Option<String>,
}

/// Read one page of a search or a browse from the registry.
///
/// Both are the same endpoint: a search adds `search`, a browse is the name
/// order the registry publishes. `version=latest` is what the market installs
/// from, so it is also what it reads.
fn mcp_registry_page(
    client: &reqwest::blocking::Client,
    query: Option<&str>,
    cursor: Option<&str>,
    limit: u32,
) -> VibexResult<McpSearchPage> {
    let limit = limit.clamp(1, MCP_REGISTRY_PAGE_SIZE);
    let mut url = format!("{MCP_REGISTRY_BASE}/v0.1/servers?version=latest&limit={limit}");
    if let Some(query) = query.filter(|value| !value.trim().is_empty()) {
        url.push_str("&search=");
        url.push_str(&urlencode(query.trim()));
    }
    if let Some(cursor) = cursor.filter(|value| !value.trim().is_empty()) {
        url.push_str("&cursor=");
        url.push_str(&urlencode(cursor));
    }
    let response: RegistryListResponse = fetch_market_json(client, &url)?;
    let next_cursor = response
        .metadata
        .and_then(|metadata| metadata.next_cursor)
        .filter(|cursor| !cursor.trim().is_empty());
    let entries = response
        .servers
        .iter()
        .filter(|record| registry_record_is_active(record))
        .filter_map(registry_server_to_entry)
        .collect();
    Ok(McpSearchPage {
        entries,
        next_cursor,
    })
}

/// Whether a record is a server the market may offer.
///
/// The registry publishes withdrawn variants beside live ones, and a search
/// that offered a deleted server would be offering an install that cannot run.
fn registry_record_is_active(record: &RegistryServerRecord) -> bool {
    let status = record
        .meta
        .as_ref()
        .and_then(|meta| meta.official.as_ref())
        .and_then(|official| official.status.as_deref())
        .map(str::trim)
        .unwrap_or("active");
    status.eq_ignore_ascii_case("active")
}

/// Answer one market request against the registry's own search.
///
/// The registry is asked the question the reader asked, so the answer covers
/// the whole catalog rather than a window this process happened to walk. A
/// query shorter than the registry accepts is a browse: the same endpoint
/// without `search`, in the order the registry publishes.
///
/// The transport filter is applied here because the registry has no parameter
/// for it, and only to the window the caller asked for: filtering upstream
/// would make a page of twenty depend on how many of a hundred happened to
/// match, which is exactly the paging bug this replaces.
fn search_mcp_registry(request: &McpMarketSearchRequest) -> VibexResult<McpMarketSearchResponse> {
    let query = request
        .query
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .chars()
        .take(MCP_REGISTRY_MAX_QUERY_CHARS)
        .collect::<String>();
    let query = query.trim().to_string();
    let searching = query.chars().count() >= MCP_REGISTRY_MIN_QUERY_CHARS;
    let limit = request
        .limit
        .unwrap_or(MARKET_PAGE_LIMIT)
        .clamp(1, MAX_ENTRIES_PER_SEARCH as u32) as usize;
    let offset = request.offset.unwrap_or(0) as usize;
    let transport = request.transport;

    // A result set already read in this run answers a page of itself without
    // touching the network: paging back is a redraw, and paging forward inside
    // what was read costs nothing. Only a window past the end asks again.
    let key = McpSearchKey {
        query: query.clone(),
        transport: transport.map(|filter| format!("{filter:?}")),
    };
    if let Some(window) = market_search_window_get(&key, offset, limit) {
        market_entries_cache_put(&window.entries);
        return Ok(window);
    }

    let client = market_http_client()?;
    let mut entries: Vec<McpMarketEntry> = market_search_entries_get(&key);
    let mut cursor: Option<String> = market_search_cursor_get(&key);
    let mut pages = 0usize;
    // Walk until the window is filled, the cursor runs out, or the page budget
    // is spent. Counting pages rather than entries is deliberate: the registry
    // will not serve fewer than a hundred, and the transport filter may drop
    // most of them, so an entry count would stop too early.
    while entries.len() < offset + limit && pages < MCP_REGISTRY_SEARCH_PAGES {
        // A set already exhausted cannot be walked further; the first page of
        // a fresh set is read with no cursor at all.
        if (pages > 0 || !entries.is_empty()) && cursor.is_none() {
            break;
        }
        let page = mcp_registry_page(
            &client,
            searching.then_some(query.as_str()),
            cursor.as_deref(),
            MCP_REGISTRY_PAGE_SIZE,
        )?;
        pages += 1;
        let next = page.next_cursor;
        entries.extend(
            page.entries
                .into_iter()
                .filter(|entry| transport.is_none_or(|filter| filter.matches(entry.transport))),
        );
        cursor = next;
    }
    market_search_entries_put(&key, entries.clone(), cursor.clone());

    let total_matches = entries.len();
    let window = entries
        .into_iter()
        .skip(offset)
        .take(limit)
        .collect::<Vec<_>>();
    let has_more = cursor.is_some() || total_matches > offset + window.len();
    // The window is what the card needs an icon for, so only it is remembered:
    // an entry the reader never sees must not keep a picture resident.
    market_entries_cache_put(&window);
    let response = McpMarketSearchResponse {
        entries: window,
        has_more,
        next_cursor: cursor,
        total_matches,
        // A browse cannot promise a total: the registry pages by name and the
        // client stops after a bounded walk, so the count is what was read.
        total_is_exact: searching,
    };
    market_search_window_put(&key, offset, limit, &response);
    Ok(response)
}

// ---------------------------------------------------------------------------
// Skill registry adapter
// ---------------------------------------------------------------------------

/// What the registry publishes about one Skill.
///
/// Both catalog endpoints answer with the same record shape, so one decoder
/// serves the ranked search and the browse list. Every counter is optional
/// because a search result carries fewer fields than a list result; a missing
/// counter is reported as zero rather than as a guess.
#[derive(Debug, Deserialize)]
struct ClawHubSkillRecord {
    slug: String,
    /// The handle is what disambiguates a slug several publishers own. The
    /// registry nests it under `owner` on some responses.
    #[serde(default, rename = "ownerHandle")]
    owner_handle: Option<String>,
    #[serde(default)]
    owner: Option<ClawHubOwner>,
    #[serde(default, rename = "displayName")]
    display_name: Option<String>,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    description: Option<String>,
    #[serde(default)]
    version: Option<String>,
    #[serde(default, rename = "latestVersion")]
    latest_version: Option<ClawHubLatestVersion>,
    #[serde(default)]
    tags: Option<std::collections::BTreeMap<String, String>>,
    #[serde(default)]
    stats: Option<ClawHubStats>,
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    installs: Option<u64>,
    #[serde(default, rename = "installsCurrent")]
    installs_current: Option<u64>,
    #[serde(default, rename = "updatedAt")]
    updated_at: Option<i64>,
}

#[derive(Debug, Deserialize)]
struct ClawHubOwner {
    #[serde(default)]
    handle: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawHubLatestVersion {
    #[serde(default)]
    version: Option<String>,
}

/// The registry's counters. It documents this object as open, so each counter
/// is read by name and anything else in it is ignored.
#[derive(Debug, Deserialize)]
struct ClawHubStats {
    #[serde(default)]
    downloads: Option<u64>,
    #[serde(default)]
    stars: Option<u64>,
    #[serde(default)]
    installs: Option<u64>,
    #[serde(default, rename = "installsCurrent")]
    installs_current: Option<u64>,
}

#[derive(Debug, Deserialize)]
struct ClawHubListResponse {
    #[serde(default)]
    items: Vec<ClawHubSkillRecord>,
    #[serde(default, rename = "nextCursor")]
    next_cursor: Option<String>,
}

#[derive(Debug, Deserialize)]
struct ClawHubSearchResponse {
    #[serde(default)]
    results: Vec<ClawHubSkillRecord>,
}

/// A slug several publishers hold, which the registry refuses to resolve.
///
/// The refusal names the candidate handles, so it is also the one answer that
/// makes a slug resolvable: the caller re-asks with a handle from this list
/// instead of guessing one.
#[derive(Debug, Deserialize)]
struct ClawHubAmbiguousSlug {
    #[serde(default)]
    matches: Vec<ClawHubAmbiguousMatch>,
}

/// One candidate the refusal named.
///
/// These entries spell the handle the way the catalog does, not the way the
/// nested `owner` object does, so they need their own decoder.
#[derive(Debug, Deserialize)]
struct ClawHubAmbiguousMatch {
    #[serde(default, rename = "ownerHandle")]
    owner_handle: Option<String>,
    /// The spelling an older answer used for the same field.
    #[serde(default)]
    handle: Option<String>,
}

impl ClawHubAmbiguousMatch {
    fn handle(&self) -> Option<String> {
        trimmed(self.owner_handle.as_deref()).or_else(|| trimmed(self.handle.as_deref()))
    }
}

fn trimmed(value: Option<&str>) -> Option<String> {
    value
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
}

impl ClawHubSkillRecord {
    fn owner_handle(&self) -> Option<String> {
        trimmed(self.owner_handle.as_deref()).or_else(|| {
            self.owner
                .as_ref()
                .and_then(|owner| trimmed(owner.handle.as_deref()))
        })
    }

    fn version(&self) -> Option<String> {
        trimmed(self.version.as_deref())
            .or_else(|| {
                self.latest_version
                    .as_ref()
                    .and_then(|latest| trimmed(latest.version.as_deref()))
            })
            .or_else(|| {
                self.tags
                    .as_ref()
                    .and_then(|tags| tags.get("latest"))
                    .and_then(|value| trimmed(Some(value.as_str())))
            })
    }

    /// The install counter, from whichever spelling this response carries.
    ///
    /// The registry publishes both a lifetime `installs` and a current
    /// `installsCurrent`; the current one is what a market should rank and
    /// label by, because the lifetime one only ever grows.
    fn installs(&self) -> u64 {
        self.installs_current
            .or_else(|| {
                self.stats
                    .as_ref()
                    .and_then(|stats| stats.installs_current.or(stats.installs))
            })
            .or(self.installs)
            .or_else(|| self.stats.as_ref().and_then(|stats| stats.installs))
            .unwrap_or(0)
    }

    fn downloads(&self) -> u64 {
        self.downloads
            .or_else(|| self.stats.as_ref().and_then(|stats| stats.downloads))
            .unwrap_or(0)
    }

    fn stars(&self) -> u64 {
        self.stars
            .or_else(|| self.stats.as_ref().and_then(|stats| stats.stars))
            .unwrap_or(0)
    }

    fn into_entry(self) -> SkillMarketEntry {
        let slug = self.slug.trim().to_string();
        let owner_handle = self.owner_handle();
        // The entry id is the registry's own identity for the Skill. Building
        // it here rather than on the client is what lets an installed Skill
        // recognize the entry it came from without re-deriving the shape.
        let id = match owner_handle.as_deref() {
            Some(owner) => format!("{owner}/{slug}"),
            None => slug.clone(),
        };
        let downloads = self.downloads();
        let stars = self.stars();
        let installs = self.installs();
        let name = trimmed(self.display_name.as_deref()).unwrap_or_else(|| slug.clone());
        let summary =
            trimmed(self.summary.as_deref()).or_else(|| trimmed(self.description.as_deref()));
        let version = self.version();
        SkillMarketEntry {
            id,
            slug,
            owner_handle,
            name,
            summary,
            version,
            downloads,
            installs,
            stars,
            updated_at: self.updated_at,
        }
    }
}

/// What a caller's query means to the registry.
struct ClawHubQuery {
    /// The query actually sent upstream, absent when the caller is browsing.
    text: Option<String>,
}

/// Resolve a caller's query into the request the registry can answer.
///
/// An empty or single-character query is not a failed search, it is the browse
/// view: the caller has not asked for anything in particular yet. The search
/// endpoint refuses those, so they become a ranked list instead. Anything the
/// search would accept is passed through untouched.
fn clawhub_query(query: Option<&str>) -> ClawHubQuery {
    let query = query.map(str::trim).unwrap_or_default();
    let query = query
        .chars()
        .take(CLAWHUB_MAX_QUERY_CHARS)
        .collect::<String>();
    if query.chars().count() < CLAWHUB_MIN_QUERY_CHARS {
        return ClawHubQuery { text: None };
    }
    ClawHubQuery { text: Some(query) }
}

/// Build one registry URL under the fetch policy.
///
/// Every parameter is percent-encoded, including the cursor: the registry's
/// cursor is an opaque blob, so it is data and never a path.
fn clawhub_url(path: &str, params: &[(&str, String)]) -> String {
    let mut url = format!("{CLAWHUB_API_BASE}{path}");
    for (index, (key, value)) in params.iter().enumerate() {
        url.push(if index == 0 { '?' } else { '&' });
        url.push_str(key);
        url.push('=');
        url.push_str(&urlencode(value));
    }
    url
}

/// Whether the registry's answer was "this slug is ambiguous".
///
/// The registry reports it as a `409` whose body names the candidate handles,
/// so the status alone is not enough to tell it apart from any other conflict;
/// the body is what makes the failure actionable.
fn clawhub_rejection(status: reqwest::StatusCode, body: &[u8]) -> VibexError {
    if status == reqwest::StatusCode::CONFLICT
        && let Ok(parsed) = serde_json::from_slice::<ClawHubAmbiguousSlug>(body)
    {
        let handles = parsed
            .matches
            .into_iter()
            .filter_map(|candidate| candidate.handle())
            .collect::<Vec<_>>();
        if !handles.is_empty() {
            // The message is the whole of what the desktop shows, so the
            // candidates are named in it rather than only in the diagnostics:
            // "choose one" is not actionable without saying which.
            return VibexError::validation(
                "market_skill_slug_ambiguous",
                format!(
                    "several publishers hold this Skill name; reinstall it from one publisher's entry ({})",
                    handles.join(", ")
                ),
            )
            .with_diagnostic("candidates", handles.join(","));
        }
    }
    VibexError::provider(
        "market_rejected",
        format!("the registry responded with HTTP {}", status.as_u16()),
    )
}

/// Fetch one page of the catalog, ranked by the caller's sort.
fn clawhub_list_page(
    client: &reqwest::blocking::Client,
    sort: SkillMarketSort,
    cursor: Option<&str>,
    limit: u32,
) -> VibexResult<(Vec<SkillMarketEntry>, Option<String>)> {
    let mut params = vec![
        ("limit", limit.to_string()),
        ("sort", sort.as_registry_value().to_string()),
        // A Skill the registry flagged is not something a market should offer:
        // the flag is the registry's own malware signal.
        ("nonSuspiciousOnly", "true".to_string()),
    ];
    if let Some(cursor) = cursor.map(str::trim).filter(|value| !value.is_empty()) {
        params.push(("cursor", cursor.to_string()));
    }
    let response: ClawHubListResponse =
        fetch_market_json(client, &clawhub_url(CLAWHUB_SKILLS_PATH, &params))?;
    let next_cursor = trimmed(response.next_cursor.as_deref());
    let entries = response
        .items
        .into_iter()
        .filter(|record| !record.slug.trim().is_empty())
        .map(ClawHubSkillRecord::into_entry)
        .take(MAX_ENTRIES_PER_SEARCH)
        .collect();
    Ok((entries, next_cursor))
}

/// Rank the catalog against a query.
fn clawhub_search_page(
    client: &reqwest::blocking::Client,
    query: &str,
    limit: u32,
) -> VibexResult<Vec<SkillMarketEntry>> {
    let params = vec![
        ("q", query.to_string()),
        ("limit", limit.to_string()),
        ("nonSuspiciousOnly", "true".to_string()),
    ];
    let response: ClawHubSearchResponse =
        fetch_market_json(client, &clawhub_url(CLAWHUB_SEARCH_PATH, &params))?;
    Ok(response
        .results
        .into_iter()
        .filter(|record| !record.slug.trim().is_empty())
        .map(ClawHubSkillRecord::into_entry)
        .take(MAX_ENTRIES_PER_SEARCH)
        .collect())
}

// ---------------------------------------------------------------------------
// Skill bundles
// ---------------------------------------------------------------------------

/// One file read out of a downloaded Skill archive.
#[derive(Debug)]
struct ClawHubBundleFile {
    /// Relative path inside the Skill folder, `/`-separated.
    path: String,
    content: String,
    bytes: u64,
}

/// A downloaded Skill folder.
#[derive(Default)]
struct ClawHubBundle {
    /// The manifest text, still carrying its frontmatter for the caller to
    /// split: the create path renders its own, so the publisher's is held
    /// apart rather than written through.
    manifest: Option<String>,
    files: Vec<ClawHubBundleFile>,
    /// Files the archive carried that will not be written, with the reason.
    skipped: Vec<String>,
}

/// Normalise an archive entry name into a relative path inside the Skill
/// folder, or refuse it.
///
/// The `zip` crate's own `enclosed_name` already refuses an entry that would
/// escape the extraction root, which is the zip-slip case: `../`, an absolute
/// path, or a Windows drive prefix. This goes one step further and treats the
/// backslash as a separator too, because an entry named `..\..\x` is a
/// traversal on the platform that reads it as one, and the check has to hold
/// for the archive rather than only for the machine that opens it.
fn bundle_entry_path(raw: &str) -> Option<String> {
    let normalized = raw.replace('\\', "/");
    if normalized.ends_with('/') {
        return None;
    }
    if normalized.starts_with('/') || normalized.contains(':') {
        return None;
    }
    let mut parts = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => return None,
            other => parts.push(other),
        }
    }
    (!parts.is_empty()).then(|| parts.join("/"))
}

/// Read a downloaded Skill archive into the files it publishes.
///
/// The archive is untrusted input, so every ceiling is enforced while reading
/// rather than after: a file is measured as it is decompressed, so a bomb is
/// stopped at the limit instead of being materialised first. An entry that is
/// refused is named in `skipped`, because the caller discloses it rather than
/// shipping a Skill that quietly lost its scripts.
fn read_skill_bundle(archive_bytes: &[u8]) -> VibexResult<ClawHubBundle> {
    let reader = std::io::Cursor::new(archive_bytes);
    let mut archive = zip::ZipArchive::new(reader)
        .map_err(|error| VibexError::provider("market_skill_bundle_invalid", error.to_string()))?;

    let mut bundle = ClawHubBundle::default();
    let mut total_bytes = 0u64;
    for index in 0..archive.len() {
        let mut entry = match archive.by_index(index) {
            Ok(entry) => entry,
            Err(error) => {
                bundle.skipped.push(format!("entry {index}: {error}"));
                continue;
            }
        };
        let raw_name = match entry.name() {
            Ok(name) => name.to_string(),
            Err(error) => {
                bundle.skipped.push(format!("entry {index}: {error}"));
                continue;
            }
        };
        if entry.is_dir() {
            continue;
        }
        // The crate's own confinement check is kept alongside the path policy
        // so a change to either one still leaves the other standing.
        if entry.enclosed_name().is_none() {
            bundle.skipped.push(format!(
                "{raw_name}: the entry would escape the Skill folder"
            ));
            continue;
        }
        let Some(path) = bundle_entry_path(&raw_name) else {
            bundle.skipped.push(format!(
                "{raw_name}: the entry is not a file inside the Skill folder"
            ));
            continue;
        };
        if entry.size() > MAX_SKILL_MARKET_BUNDLE_FILE_BYTES {
            bundle.skipped.push(format!(
                "{path}: larger than the {MAX_SKILL_MARKET_BUNDLE_FILE_BYTES} byte file limit"
            ));
            continue;
        }
        if bundle.files.len() >= MAX_SKILL_MARKET_BUNDLE_FILES {
            bundle.skipped.push(format!(
                "{path}: the bundle already carries {MAX_SKILL_MARKET_BUNDLE_FILES} files"
            ));
            continue;
        }
        if total_bytes >= MAX_SKILL_MARKET_BUNDLE_BYTES {
            bundle.skipped.push(format!(
                "{path}: the bundle already carries {MAX_SKILL_MARKET_BUNDLE_BYTES} bytes"
            ));
            continue;
        }
        // `take` bounds the read even when the entry lies about its size, so
        // the ceiling holds against a decompression bomb rather than against
        // the header the archive chose to publish.
        let limit =
            MAX_SKILL_MARKET_BUNDLE_FILE_BYTES.min(MAX_SKILL_MARKET_BUNDLE_BYTES - total_bytes);
        let mut body = Vec::new();
        if std::io::Read::take(&mut entry, limit + 1)
            .read_to_end(&mut body)
            .is_err()
        {
            bundle
                .skipped
                .push(format!("{path}: the entry could not be decompressed"));
            continue;
        }
        if body.len() as u64 > limit {
            // Which ceiling stopped it depends on which one the read was
            // bounded by, and the report has to name the right one: a file cut
            // short by the bundle's remaining budget is not an oversized file.
            let reason = if limit < MAX_SKILL_MARKET_BUNDLE_FILE_BYTES {
                format!("the bundle already carries {MAX_SKILL_MARKET_BUNDLE_BYTES} bytes")
            } else {
                format!("larger than the {MAX_SKILL_MARKET_BUNDLE_FILE_BYTES} byte file limit")
            };
            bundle.skipped.push(format!("{path}: {reason}"));
            continue;
        }
        let Ok(content) = String::from_utf8(body) else {
            bundle
                .skipped
                .push(format!("{path}: not UTF-8 text and was not installed"));
            continue;
        };
        let bytes = content.len() as u64;
        total_bytes += bytes;
        // The manifest is not a sibling: it is the document the install path
        // renders, so the publisher's copy is kept apart from the assets.
        if bundle.manifest.is_none() && SKILL_BUNDLE_MANIFEST_NAMES.contains(&path.as_str()) {
            bundle.manifest = Some(content);
        } else {
            bundle.files.push(ClawHubBundleFile {
                path,
                content,
                bytes,
            });
        }
    }
    bundle
        .files
        .sort_by(|left, right| left.path.cmp(&right.path));
    Ok(bundle)
}

/// Download one Skill folder.
///
/// The registry answers a bundle request two ways: with the zip archive, or —
/// for a Skill it hosts from a public repository — with a JSON descriptor
/// naming an archive elsewhere. Only the first is a Skill folder as published.
/// The second falls through to the single-file endpoint, which still returns
/// the manifest, so such a Skill installs as instructions without its assets
/// and the caller is told which case it got.
fn fetch_skill_bundle(
    client: &reqwest::blocking::Client,
    slug: &str,
    owner_handle: Option<&str>,
    version: Option<&str>,
) -> VibexResult<ClawHubBundle> {
    let params = clawhub_resolution_params("slug", slug, owner_handle, version);
    let (body, _) = fetch_market_bytes_rejected_by(
        client,
        &clawhub_url(CLAWHUB_DOWNLOAD_PATH, &params),
        MAX_SKILL_MARKET_BUNDLE_DOWNLOAD_BYTES,
        clawhub_rejection,
    )?;
    // The endpoint separates the two shapes by content type, but a zip always
    // starts with its own local-file signature, so the bytes decide.
    if body.starts_with(b"PK\x03\x04") {
        return read_skill_bundle(&body);
    }
    Ok(ClawHubBundle {
        manifest: Some(fetch_skill_manifest(client, slug, owner_handle, version)?),
        skipped: vec![
            "the registry serves this Skill as a single file; the assets it publishes alongside the manifest were not available as an archive"
                .to_string(),
        ],
        ..ClawHubBundle::default()
    })
}

/// Fetch just the manifest of a Skill.
///
/// Used when the registry has no archive to hand over. It is also the fallback
/// that keeps such a Skill installable at all: the instructions are the part an
/// Agent cannot work without.
fn fetch_skill_manifest(
    client: &reqwest::blocking::Client,
    slug: &str,
    owner_handle: Option<&str>,
    version: Option<&str>,
) -> VibexResult<String> {
    let params = clawhub_resolution_params("path", "SKILL.md", owner_handle, version);
    let url = clawhub_url(
        &format!("{CLAWHUB_SKILLS_PATH}/{}/file", urlencode(slug)),
        &params,
    );
    let (body, _) = fetch_market_bytes_rejected_by(
        client,
        &url,
        MAX_SKILL_DOCUMENT_FETCH_BYTES,
        clawhub_rejection,
    )?;
    String::from_utf8(body).map_err(|_| {
        VibexError::provider(
            "market_skill_document_not_utf8",
            "skill documents must be UTF-8 markdown",
        )
    })
}

/// The parameters every endpoint that resolves one Skill version takes.
///
/// The first pair is the endpoint's own key — `slug` for the archive, `path`
/// for the single-file reader — and the rest is the resolution the registry
/// documents: a release to pin, and the handle that disambiguates the slug.
fn clawhub_resolution_params(
    key: &'static str,
    value: &str,
    owner_handle: Option<&str>,
    version: Option<&str>,
) -> Vec<(&'static str, String)> {
    let mut params = vec![(key, value.to_string())];
    match version.map(str::trim).filter(|value| !value.is_empty()) {
        // A pinned release replaces the `latest` tag rather than joining it:
        // the endpoint takes one or the other, and sending both would let the
        // tag win and install a release the preview never showed.
        Some(version) => params.push(("version", version.to_string())),
        None => params.push(("tag", "latest".to_string())),
    }
    if let Some(owner) = owner_handle
        .map(str::trim)
        .filter(|value| !value.is_empty())
    {
        params.push(("ownerHandle", owner.to_string()));
    }
    params
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

/// Build the document a preview discloses and an install replays.
///
/// The manifest is measured the way the create path would persist it, so the
/// size limit reflects stored bytes rather than fetched ones. The assets are
/// carried verbatim: they are not rewritten, so their size is their own.
fn skill_document_from_bundle(
    entry_id: &str,
    manifest: &str,
    files: Vec<ClawHubBundleFile>,
    skipped: Vec<String>,
) -> SkillMarketDocument {
    let (name, description, body) = split_skill_document(manifest);
    let rendered = render_skill_document(
        name.as_deref().unwrap_or(entry_id),
        description.as_deref(),
        &body,
    );
    let bytes = rendered.len() as u64;
    let bundle_bytes = bytes
        + files
            .iter()
            .map(|file| file.bytes)
            .fold(0u64, u64::saturating_add);
    SkillMarketDocument {
        entry_id: entry_id.to_string(),
        name,
        description,
        body,
        bytes,
        too_large: bytes > MAX_SKILL_MARKET_DOCUMENT_BYTES,
        files: files
            .into_iter()
            .map(|file| SkillMarketBundleFile {
                path: file.path,
                content: file.content,
                bytes: file.bytes,
            })
            .collect(),
        bundle_bytes,
        skipped_files: skipped,
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
    /// Answer one market query from the registry's own search.
    ///
    /// The registry is asked what the reader asked, so the answer covers the
    /// whole catalog rather than a window this process walked. A query too
    /// short for the registry's search is a browse.
    pub fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> VibexResult<McpMarketSearchResponse> {
        search_mcp_registry(&request)
    }

    /// Load the picture one market entry published.
    ///
    /// The bytes come from the runtime rather than from the card, so a paired
    /// client shows the same picture without a network client of its own, and
    /// every hop is validated like every other market fetch. The result is kept
    /// in memory under the entry id, because a grid re-renders far more often
    /// than the pictures change.
    pub fn mcp_market_icon(&self, entry_id: &str) -> VibexResult<McpMarketIcon> {
        load_market_icon(entry_id)
    }

    /// Answer one Skill market query.
    ///
    /// A query the registry's search accepts is ranked by the registry. A
    /// caller that has not asked for anything in particular is browsing, and
    /// the catalog endpoint answers that directly — there is no query to
    /// invent, and the caller's own ranking is passed through as the registry
    /// documents it.
    pub fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> VibexResult<SkillMarketSearchResponse> {
        let client = market_http_client()?;
        let limit = request
            .limit
            .unwrap_or(CLAWHUB_PAGE_SIZE)
            .clamp(1, CLAWHUB_MAX_PAGE_SIZE);
        let query = clawhub_query(request.query.as_deref());
        let sort = request.sort.unwrap_or(SkillMarketSort::Recommended);
        match query.text {
            Some(text) => {
                let entries = clawhub_search_page(&client, &text, limit)?;
                // The search endpoint ranks a whole catalog against a query and
                // has no cursor of its own, so this answer is complete: there is
                // no second page to walk and none is claimed.
                Ok(SkillMarketSearchResponse {
                    has_more: false,
                    next_cursor: None,
                    entries,
                })
            }
            None => {
                let (entries, next_cursor) =
                    clawhub_list_page(&client, sort, request.cursor.as_deref(), limit)?;
                Ok(SkillMarketSearchResponse {
                    has_more: next_cursor.is_some(),
                    next_cursor,
                    entries,
                })
            }
        }
    }

    /// Resolve one Skill folder, manifest and assets together.
    ///
    /// The whole folder is fetched here rather than at install time, because the
    /// preview is what discloses it: the bytes the caller approves are the bytes
    /// it later sends back to be written.
    pub fn skill_market_document(
        &self,
        request: SkillMarketDocumentRequest,
    ) -> VibexResult<SkillMarketDocument> {
        let client = market_http_client()?;
        let bundle = fetch_skill_bundle(
            &client,
            &request.slug,
            request.owner_handle.as_deref(),
            request.version.as_deref(),
        )?;
        let manifest = bundle.manifest.clone().ok_or_else(|| {
            VibexError::validation(
                "market_skill_document_not_found",
                "the Skill folder carries no SKILL.md",
            )
            .with_diagnostic("slug", request.slug.clone())
        })?;
        Ok(skill_document_from_bundle(
            &request.entry_id,
            &manifest,
            bundle.files,
            bundle.skipped,
        ))
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
        // The assets are re-checked here rather than trusted from the preview:
        // this request is the one that writes to disk, and it arrives from a
        // client. A preview that was never fetched, or one truncated in flight,
        // must not become an install of something else.
        validate_skill_bundle_document(&request.document)?;
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
            body: Some(rendered.clone()),
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

        // The folder is written after the row it belongs to, because the
        // folder's name is the row's id. A write that fails is reported rather
        // than swallowed: an install that stored instructions but lost the
        // scripts they call is not the bundle the user approved.
        let store_root = market_skill_store_root(self.database_path());
        let directory = market_skill_dir(&store_root, &skill.id);
        write_skill_bundle(&directory, &rendered, &request.document.files)
            .map_err(|error| error.with_diagnostic("skillId", skill.id.as_str().to_string()))?;

        let readback = SkillRepository::get(&conn, &skill.id)?.ok_or_else(|| {
            VibexError::storage(
                "market_install_readback_missing",
                "the installed Skill could not be read back",
            )
        })?;
        let mut diagnostics = Vec::new();
        if !request.document.skipped_files.is_empty() {
            diagnostics.push(diagnostic(
                "marketInstallSkillFilesSkipped",
                request.document.skipped_files.join("; "),
            ));
        }
        Ok(SkillMarketInstallResult {
            skill: readback,
            created,
            enabled_agent_ids: request.agent_ids,
            diagnostics,
        })
    }
}

/// Check a bundle an install is about to write.
///
/// The preview already applied these rules to what it fetched, but the install
/// request is what writes to disk and it arrives from a client, so the same
/// rules are applied again there. A file that names a path outside the Skill
/// folder, or a bundle past any of the ceilings, is refused rather than
/// truncated: a half-written Skill is worse than a refused one.
fn validate_skill_bundle_document(document: &SkillMarketDocument) -> VibexResult<()> {
    for file in &document.files {
        if bundle_entry_path(&file.path).as_deref() != Some(file.path.as_str()) {
            return Err(VibexError::validation(
                "market_skill_bundle_path_invalid",
                "a bundled Skill file names a path outside the Skill folder",
            )
            .with_diagnostic("path", file.path.clone()));
        }
        if file.bytes > MAX_SKILL_MARKET_BUNDLE_FILE_BYTES {
            return Err(VibexError::validation(
                "market_skill_bundle_file_too_large",
                "a bundled Skill file exceeds the maximum size",
            )
            .with_diagnostic("path", file.path.clone()));
        }
    }
    if document.files.len() > MAX_SKILL_MARKET_BUNDLE_FILES {
        return Err(VibexError::validation(
            "market_skill_bundle_too_many_files",
            "the Skill bundle carries more files than an install will write",
        ));
    }
    if document.bundle_bytes > MAX_SKILL_MARKET_BUNDLE_BYTES {
        return Err(VibexError::validation(
            "market_skill_bundle_too_large",
            "the Skill bundle exceeds the maximum size",
        ));
    }
    Ok(())
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
    use std::time::Instant;

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
                icons: Vec::new(),
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

    /// A remote entry authenticates with headers, and the registry publishes
    /// them as variables. Without carrying them the install form has nothing to
    /// ask for and the saved server answers 401.
    #[test]
    fn remote_headers_become_install_inputs() {
        let remote = RegistryRemote {
            remote_type: Some("streamable-http".to_string()),
            url: Some("https://mcp.example.com/mcp".to_string()),
            headers: vec![
                RegistryEnvVar {
                    name: Some("Authorization".to_string()),
                    description: Some("personal access token".to_string()),
                    is_required: Some(true),
                    is_secret: Some(true),
                    value: Some("Bearer {token}".to_string()),
                    default: None,
                },
                // A value the publisher ships needs nothing from the reader.
                RegistryEnvVar {
                    name: Some("X-Client".to_string()),
                    description: None,
                    is_required: None,
                    is_secret: None,
                    value: Some("vibex".to_string()),
                    default: None,
                },
            ],
        };
        let inputs = registry_header_requirements(&remote);
        assert_eq!(inputs.len(), 1);
        assert_eq!(inputs[0].name, "Authorization");
        assert!(inputs[0].required && inputs[0].secret);
        assert_eq!(
            inputs[0].description.as_deref(),
            Some("personal access token")
        );
    }

    /// The registry's list is a preference order, and an address that is not a
    /// public `https` one must be skipped rather than loaded.
    #[test]
    fn an_entry_keeps_the_first_loadable_picture() {
        let mut server = RegistryServer {
            name: "io.example/with-icon".to_string(),
            title: None,
            description: None,
            homepage: None,
            website_url: None,
            repository: None,
            icons: vec![
                RegistryIcon {
                    src: Some("http://insecure.example/icon.png".to_string()),
                },
                RegistryIcon {
                    src: Some("https://cdn.example/icon.png".to_string()),
                },
            ],
            packages: vec![registry_package_of("npm", "example-server", "1.0.0")],
            remotes: Vec::new(),
        };
        assert_eq!(
            registry_icon(&server).as_deref(),
            Some("https://cdn.example/icon.png")
        );
        server.icons.clear();
        assert_eq!(registry_icon(&server), None);
    }

    /// A withdrawn entry is not something the market may offer: installing it
    /// would write a launcher that cannot run.
    #[test]
    fn a_withdrawn_registry_record_is_not_offered() {
        let mut record = registry_record(
            "io.example/old",
            vec![registry_package_of("npm", "example-server", "1.0.0")],
        );
        assert!(registry_record_is_active(&record));
        record.meta = Some(RegistryRecordMeta {
            official: Some(RegistryOfficialMeta {
                status: Some("deleted".to_string()),
                updated_at: None,
            }),
        });
        assert!(!registry_record_is_active(&record));
    }

    /// A page size past what the registry serves is a 422, and a query shorter
    /// than three characters is one too. Both are decided before a request is
    /// made so the market answers instead of reporting a bad request.
    #[test]
    fn the_registry_limits_are_respected_before_a_request() {
        // A page larger than the registry serves is a 422, and so is a query
        // shorter than its minimum; both are refused before a request is made.
        const { assert!(MCP_REGISTRY_PAGE_SIZE == 100) };
        const { assert!(MCP_REGISTRY_MIN_QUERY_CHARS >= 3) };
        const { assert!(MCP_REGISTRY_MAX_QUERY_CHARS >= MCP_REGISTRY_MIN_QUERY_CHARS) };
    }

    #[test]
    fn image_bytes_decide_the_media_type() {
        assert_eq!(
            sniff_image_mime(&[0x89, b'P', b'N', b'G', 13, 10]),
            Some("image/png")
        );
        assert_eq!(
            sniff_image_mime(&[0xff, 0xd8, 0xff, 0xe0]),
            Some("image/jpeg")
        );
        assert_eq!(
            sniff_image_mime(b"<svg xmlns=\"http://www.w3.org/2000/svg\"/>"),
            Some("image/svg+xml")
        );
        assert_eq!(sniff_image_mime(b"<html>not a picture</html>"), None);
    }

    /// A search and a browse are the same endpoint: the query is what turns
    /// one into the other, and a cursor is echoed untouched.
    #[test]
    fn the_search_url_carries_the_query_and_the_cursor() {
        // The page builder is the only place a registry URL is assembled, so
        // its shape is what the assertions below pin.
        let source = include_str!("market.rs");
        let builder = source
            .split_once("fn mcp_registry_page(")
            .and_then(|(_, tail)| tail.split_once("\n/// Whether a record is a server"))
            .map(|(body, _)| body)
            .expect("the registry page builder should remain inspectable");
        assert!(builder.contains("version=latest"));
        assert!(builder.contains("limit="));
        assert!(builder.contains("&search="));
        assert!(builder.contains("&cursor="));
        assert!(builder.contains("&urlencode(query.trim())"));
        assert!(builder.contains("&urlencode(cursor)"));
        // The limit is clamped to what the registry serves before it is sent.
        assert!(builder.contains("limit.clamp(1, MCP_REGISTRY_PAGE_SIZE)"));
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
    /// This is the check that the market answers at all: a search has to reach
    /// the registry rather than a window this process walked, and it has to
    /// come back inside one interactive wait.
    #[test]
    #[ignore = "hits the live MCP registry"]
    fn live_registry_search_answers_from_the_whole_catalog() {
        let started = Instant::now();
        let browse = search_mcp_registry(&McpMarketSearchRequest {
            query: None,
            limit: Some(20),
            offset: None,
            cursor: None,
            transport: None,
        })
        .expect("the live registry answers a browse");
        assert!(
            browse.entries.len() >= 20,
            "a browse window should hold a full page, got {}",
            browse.entries.len()
        );
        assert!(started.elapsed() < MARKET_REQUEST_TIMEOUT);

        // A name that sits deep in the registry's published order is the whole
        // point of asking upstream: it must be found on the first search, not
        // after a background walk caught up with it.
        let search = search_mcp_registry(&McpMarketSearchRequest {
            query: Some("grafana".to_string()),
            limit: Some(20),
            offset: None,
            cursor: None,
            transport: None,
        })
        .expect("the live registry answers a search");
        assert!(
            search
                .entries
                .iter()
                .any(|entry| entry.name.to_lowercase().contains("grafana")
                    || entry
                        .description
                        .as_deref()
                        .is_some_and(|text| text.to_lowercase().contains("grafana"))),
            "searching grafana among {} entries found nothing",
            search.total_matches
        );
    }

    /// The live icon path: a search finds an entry that published a picture,
    /// and the picture the runtime loads is one it would show.
    #[test]
    #[ignore = "hits the live MCP registry"]
    fn live_registry_entry_icon_loads() {
        let response = search_mcp_registry(&McpMarketSearchRequest {
            query: Some("context7".to_string()),
            limit: Some(20),
            offset: None,
            cursor: None,
            transport: None,
        })
        .expect("the live registry answers a search");
        let entry = response
            .entries
            .iter()
            .find(|entry| entry.icon_url.is_some())
            .expect("an entry that published a picture");
        // The icon path is a service method, but it only reads the cache the
        // search above filled, so it is called through the same helper the
        // cache writes.
        let icon = load_market_icon(&entry.id).expect("the entry's picture loads");
        assert!(!icon.is_empty(), "the picture came back empty");
        assert!(
            icon.mime
                .as_deref()
                .is_some_and(|mime| mime.starts_with("image/")),
            "the picture's type is {:?}",
            icon.mime
        );
    }

    /// The live Skill registry, end to end.
    ///
    /// Run it by hand when the registry's shape or reachability is in question:
    /// `cargo test -p vibex-config-switch -- --ignored live_skill_registry`.
    /// It is the check that the market answers at all, and that a bundle it
    /// hands over is one this product will install — the two halves of the
    /// feature that a fixture cannot prove.
    #[test]
    #[ignore = "hits the live Skill registry"]
    fn live_skill_registry_answers_and_serves_an_installable_bundle() {
        let client = market_http_client().expect("the market client builds");

        let (entries, next_cursor) =
            clawhub_list_page(&client, SkillMarketSort::Recommended, None, 30)
                .expect("the live registry answers a browse");
        assert!(
            entries.len() >= 20,
            "a browse page should hold most of what was asked for, got {}",
            entries.len()
        );
        assert!(
            next_cursor.is_some(),
            "a catalog this size should name a further page"
        );
        let first = entries
            .iter()
            .find(|entry| entry.owner_handle.is_some())
            .expect("a listed Skill names its publisher");
        assert!(!first.slug.trim().is_empty());
        assert_eq!(
            first.id,
            format!("{}/{}", first.owner_handle.as_deref().unwrap(), first.slug)
        );

        // A second page must be a different page, or the cursor is not being
        // honoured and the browse view would repeat itself forever.
        let (next_entries, _) = clawhub_list_page(
            &client,
            SkillMarketSort::Recommended,
            next_cursor.as_deref(),
            30,
        )
        .expect("the live registry answers a second page");
        assert!(
            next_entries
                .iter()
                .all(|entry| !entries.iter().any(|first| first.id == entry.id)),
            "the second page repeated the first"
        );

        let search = clawhub_search_page(&client, "pdf", 10).expect("the live registry searches");
        assert!(
            !search.is_empty(),
            "searching for a common term found nothing"
        );

        // The document is the part the install writes, so it has to arrive as a
        // folder with a manifest rather than as whatever the endpoint felt like
        // sending.
        let bundle = fetch_skill_bundle(
            &client,
            &first.slug,
            first.owner_handle.as_deref(),
            first.version.as_deref(),
        )
        .expect("the live registry serves a bundle");
        let manifest = bundle
            .manifest
            .as_deref()
            .expect("the bundle carries a SKILL.md");
        assert!(
            !manifest.trim().is_empty(),
            "the manifest is the instructions an Agent follows"
        );
        let document =
            skill_document_from_bundle(&first.id, manifest, bundle.files, bundle.skipped);
        assert!(
            !document.too_large,
            "a published Skill should be installable"
        );
        validate_skill_bundle_document(&document)
            .expect("what the registry served must pass the install's own check");
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
    fn browse_view_sends_no_query_because_the_registry_rejects_a_short_one() {
        // The registry's search refuses anything shorter than two characters,
        // so a caller who has not asked for anything must not produce one: it
        // becomes the catalog's own ranked list instead.
        for short in [None, Some(""), Some("   "), Some("a")] {
            assert!(
                clawhub_query(short).text.is_none(),
                "{short:?} must resolve to the browse view"
            );
        }
        // Two characters is the first query the registry accepts, so it is
        // passed through rather than replaced.
        for search in ["ai", "kubernetes"] {
            assert_eq!(clawhub_query(Some(search)).text.as_deref(), Some(search));
        }
        let padded = clawhub_query(Some("  ai  "));
        assert_eq!(padded.text.as_deref(), Some("ai"));
    }

    #[test]
    fn a_query_is_bounded_before_it_is_sent() {
        let long = "x".repeat(CLAWHUB_MAX_QUERY_CHARS + 500);
        let resolved = clawhub_query(Some(&long));
        assert_eq!(
            resolved.text.map(|text| text.chars().count()),
            Some(CLAWHUB_MAX_QUERY_CHARS)
        );
    }

    #[test]
    fn registry_urls_encode_every_parameter() {
        let url = clawhub_url(
            CLAWHUB_SKILLS_PATH,
            &[
                ("limit", "30".to_string()),
                ("cursor", "{\"v\":1,\"index\":\"by_downloads\"}".to_string()),
            ],
        );
        assert!(url.starts_with("https://clawhub.ai/api/v1/skills?limit=30&cursor="));
        // The cursor is an opaque blob, so it is data and never a path.
        assert!(!url.contains('{'));
        assert!(!url.contains('"'));
        assert!(url.contains("%7B") && url.contains("%22"));
    }

    #[test]
    fn a_registry_record_becomes_the_entry_the_market_shows() {
        // The list form nests the counter block and names the version once.
        let listed: ClawHubSkillRecord = serde_json::from_str(
            r#"{
                "slug": "pdf",
                "ownerHandle": "awspace",
                "displayName": "Pdf",
                "summary": "Fill and read PDFs",
                "topics": ["documents"],
                "tags": {"latest": "1.4.0"},
                "stats": {"downloads": 49697, "installs": 1479, "stars": 66},
                "updatedAt": 1789594554485
            }"#,
        )
        .unwrap();
        let entry = listed.into_entry();
        assert_eq!(entry.id, "awspace/pdf");
        assert_eq!(entry.slug, "pdf");
        assert_eq!(entry.owner_handle.as_deref(), Some("awspace"));
        assert_eq!(entry.name, "Pdf");
        assert_eq!(entry.version.as_deref(), Some("1.4.0"));
        assert_eq!(entry.downloads, 49697);
        assert_eq!(entry.installs, 1479);
        assert_eq!(entry.stars, 66);
        assert_eq!(entry.updated_at, Some(1789594554485));
    }

    #[test]
    fn a_search_record_carries_fewer_fields_than_a_listed_one() {
        // The search form publishes no counter block and nests the publisher
        // under `owner`; a missing counter is zero rather than a guess.
        let searched: ClawHubSkillRecord = serde_json::from_str(
            r#"{
                "slug": "gitcrawl",
                "displayName": "Gitcrawl",
                "summary": "Crawl repositories",
                "version": "0.3.1",
                "owner": {"handle": "openclaw"},
                "updatedAt": 1784845248916
            }"#,
        )
        .unwrap();
        let entry = searched.into_entry();
        assert_eq!(entry.id, "openclaw/gitcrawl");
        assert_eq!(entry.owner_handle.as_deref(), Some("openclaw"));
        assert_eq!(entry.version.as_deref(), Some("0.3.1"));
        assert_eq!(entry.installs, 0);
        assert_eq!(entry.downloads, 0);
        assert_eq!(entry.stars, 0);
    }

    #[test]
    fn a_record_without_a_publisher_is_still_an_entry() {
        // The handle is what disambiguates a slug, not what identifies it: a
        // record that names none is listed under the slug alone rather than
        // dropped, and the ambiguity surfaces when the slug is resolved.
        let bare: ClawHubSkillRecord = serde_json::from_str(r#"{"slug": "orphan"}"#).unwrap();
        let entry = bare.into_entry();
        assert_eq!(entry.id, "orphan");
        assert_eq!(entry.owner_handle, None);
        assert_eq!(entry.name, "orphan");
    }

    #[test]
    fn an_ambiguous_slug_reports_its_candidates() {
        // The registry answers a slug several publishers hold with the handles
        // themselves, which is the one refusal a caller can act on.
        let body = br#"{
            "code": "AMBIGUOUS_SKILL_SLUG",
            "slug": "self-improving-agent",
            "matches": [{"ownerHandle": "pskoett"}, {"ownerHandle": "nguyenmanhdung-app"}]
        }"#;
        let error = clawhub_rejection(reqwest::StatusCode::CONFLICT, body);
        assert_eq!(error.code, "market_skill_slug_ambiguous");
        assert!(
            error
                .diagnostics
                .iter()
                .any(|diagnostic| format!("{diagnostic:?}").contains("pskoett")),
            "the candidate handles must survive into the error"
        );
    }

    #[test]
    fn a_refusal_without_candidates_is_reported_as_a_status() {
        let error = clawhub_rejection(reqwest::StatusCode::NOT_FOUND, b"not found");
        assert_eq!(error.code, "market_rejected");
    }

    /// A stored entry is refused unless it stays inside the Skill folder.
    #[test]
    fn bundle_entry_paths_are_confined_to_the_skill_folder() {
        assert_eq!(bundle_entry_path("SKILL.md").as_deref(), Some("SKILL.md"));
        assert_eq!(
            bundle_entry_path("./references/api.md").as_deref(),
            Some("references/api.md")
        );
        // The zip crate already refuses these, and so does the path policy, so
        // a change to either one still leaves the other standing.
        assert_eq!(bundle_entry_path("../escape.md"), None);
        assert_eq!(bundle_entry_path("references/../../escape.md"), None);
        assert_eq!(bundle_entry_path("/etc/passwd"), None);
        assert_eq!(bundle_entry_path("C:/windows/system32"), None);
        // A backslash is a separator on the platform that reads it as one.
        assert_eq!(bundle_entry_path("..\\..\\escape.md"), None);
        assert_eq!(
            bundle_entry_path("references\\api.md").as_deref(),
            Some("references/api.md")
        );
        // A directory entry is not a file to write.
        assert_eq!(bundle_entry_path("references/"), None);
        assert_eq!(bundle_entry_path(""), None);
    }

    /// Build a zip archive the way a publisher's bundle would arrive.
    fn bundle_archive(entries: &[(&str, &str)]) -> Vec<u8> {
        use std::io::Write as _;
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default();
            for (path, content) in entries {
                writer.start_file(*path, options).unwrap();
                writer.write_all(content.as_bytes()).unwrap();
            }
            writer.finish().unwrap();
        }
        bytes
    }

    #[test]
    fn a_bundle_splits_its_manifest_from_its_assets() {
        let archive = bundle_archive(&[
            ("SKILL.md", "---\nname: demo\n---\n\nBody\n"),
            ("references/api.md", "API notes"),
            ("scripts/run.sh", "echo hi"),
            ("assets/LEARNINGS.md", "notes"),
        ]);
        let bundle = read_skill_bundle(&archive).unwrap();
        assert_eq!(
            bundle.manifest.as_deref(),
            Some("---\nname: demo\n---\n\nBody\n")
        );
        let paths = bundle
            .files
            .iter()
            .map(|file| file.path.as_str())
            .collect::<Vec<_>>();
        // Sorted, so an unchanged bundle produces a stable list and diff.
        assert_eq!(
            paths,
            vec!["assets/LEARNINGS.md", "references/api.md", "scripts/run.sh"]
        );
        assert!(bundle.skipped.is_empty());
    }

    #[test]
    fn a_bundle_refuses_entries_that_escape_it() {
        // The archive is untrusted input: an entry that would write outside the
        // Skill folder is reported and never extracted.
        let archive = bundle_archive(&[
            ("SKILL.md", "manifest"),
            ("../escape.md", "escaped"),
            ("references/../../escape-too.md", "escaped"),
        ]);
        let bundle = read_skill_bundle(&archive).unwrap();
        assert_eq!(bundle.manifest.as_deref(), Some("manifest"));
        assert!(bundle.files.is_empty(), "{:?}", bundle.files);
        assert_eq!(bundle.skipped.len(), 2);
    }

    #[test]
    fn a_bundle_refuses_a_file_past_the_per_file_limit() {
        let oversized = "x".repeat(MAX_SKILL_MARKET_BUNDLE_FILE_BYTES as usize + 1);
        let archive = bundle_archive(&[("SKILL.md", "manifest"), ("big.md", &oversized)]);
        let bundle = read_skill_bundle(&archive).unwrap();
        assert!(bundle.files.is_empty());
        assert_eq!(bundle.skipped.len(), 1);
        assert!(bundle.skipped[0].contains("big.md"));
    }

    #[test]
    fn a_bundle_stops_at_the_file_count_limit() {
        let owned = (0..MAX_SKILL_MARKET_BUNDLE_FILES + 3)
            .map(|index| (format!("notes/{index}.md"), "note".to_string()))
            .collect::<Vec<_>>();
        let entries = owned
            .iter()
            .map(|(path, content)| (path.as_str(), content.as_str()))
            .collect::<Vec<_>>();
        let archive = bundle_archive(&entries);
        let bundle = read_skill_bundle(&archive).unwrap();
        assert_eq!(bundle.files.len(), MAX_SKILL_MARKET_BUNDLE_FILES);
        assert_eq!(bundle.skipped.len(), 3);
    }

    #[test]
    fn a_bundle_refuses_a_file_that_is_not_utf8_text() {
        use std::io::Write as _;
        let mut bytes = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut bytes));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("SKILL.md", options).unwrap();
            writer.write_all(b"manifest").unwrap();
            writer.start_file("logo.png", options).unwrap();
            writer.write_all(&[0xff, 0xfe, 0x00, 0x01]).unwrap();
            writer.finish().unwrap();
        }
        let bundle = read_skill_bundle(&bytes).unwrap();
        assert!(bundle.files.is_empty());
        assert_eq!(bundle.skipped.len(), 1);
        assert!(bundle.skipped[0].contains("logo.png"));
        assert!(bundle.skipped[0].contains("UTF-8"));
    }

    #[test]
    fn a_download_that_is_not_an_archive_is_refused_rather_than_unpacked() {
        assert!(read_skill_bundle(b"{\"sourceRef\":\"public-github\"}").is_err());
    }

    /// A document as it would arrive back on an install request.
    fn bundle_document(
        files: Vec<SkillMarketBundleFile>,
        bundle_bytes: u64,
    ) -> SkillMarketDocument {
        SkillMarketDocument {
            entry_id: "awspace/pdf".to_string(),
            name: Some("Pdf".to_string()),
            description: None,
            body: "Fill forms.".to_string(),
            bytes: 32,
            too_large: false,
            files,
            bundle_bytes,
            skipped_files: Vec::new(),
        }
    }

    fn bundle_file(path: &str, bytes: u64) -> SkillMarketBundleFile {
        SkillMarketBundleFile {
            path: path.to_string(),
            content: "x".repeat(bytes as usize),
            bytes,
        }
    }

    #[test]
    fn an_install_refuses_a_bundle_path_that_leaves_the_skill_folder() {
        // The preview only ever produces confined paths; the install request is
        // the one that writes, and it is checked on its own terms.
        let document = bundle_document(vec![bundle_file("../escape.md", 4)], 64);
        let error = validate_skill_bundle_document(&document).unwrap_err();
        assert_eq!(error.code, "market_skill_bundle_path_invalid");
    }

    #[test]
    fn an_install_refuses_a_bundle_past_its_ceilings() {
        let oversized = bundle_document(
            vec![bundle_file(
                "big.md",
                MAX_SKILL_MARKET_BUNDLE_FILE_BYTES + 1,
            )],
            0,
        );
        assert_eq!(
            validate_skill_bundle_document(&oversized).unwrap_err().code,
            "market_skill_bundle_file_too_large"
        );

        let many = bundle_document(
            (0..MAX_SKILL_MARKET_BUNDLE_FILES + 1)
                .map(|index| bundle_file(&format!("notes/{index}.md"), 4))
                .collect(),
            0,
        );
        assert_eq!(
            validate_skill_bundle_document(&many).unwrap_err().code,
            "market_skill_bundle_too_many_files"
        );

        let heavy = bundle_document(vec![bundle_file("ok.md", 4)], u64::MAX);
        assert_eq!(
            validate_skill_bundle_document(&heavy).unwrap_err().code,
            "market_skill_bundle_too_large"
        );
    }

    #[test]
    fn an_install_accepts_the_bundle_it_fetched() {
        // The round trip has to hold: what `skill_document_from_bundle` builds
        // is what the install accepts.
        let still_open = bundle_document(
            vec![
                bundle_file("references/api.md", 4),
                bundle_file("scripts/fill.py", 4),
            ],
            64,
        );
        validate_skill_bundle_document(&still_open).unwrap();
    }
}

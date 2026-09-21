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

use std::io::Read;
use std::net::IpAddr;
use std::time::Duration;

use serde::Deserialize;
use vibex_core::{
    AgentId, MAX_MARKET_RESPONSE_BYTES, MAX_SKILL_MARKET_DOCUMENT_BYTES, MarketEnvRequirement,
    McpMarketEntry, McpMarketInstallRequest, McpMarketInstallResult, McpMarketSearchRequest,
    McpMarketSearchResponse, McpServerTransportKind, SkillCreateRequest, SkillMarketDocument,
    SkillMarketDocumentRequest, SkillMarketEntry, SkillMarketInstallRequest,
    SkillMarketInstallResult, SkillMarketSearchRequest, SkillMarketSearchResponse, SkillScopeKind,
    SkillSourceKind, SkillStatus, VibexError, VibexResult,
};
use vibex_db::{McpServerRepository, SkillRepository};

use crate::{
    ProviderConfigService, diagnostic, find_existing_mcp_server, normalize_mcp_create_request,
    normalize_skill_create_request, validate_mcp_create_request, validate_skill_create_request,
};

/// The official MCP registry. Its `v0.1` API is the only MCP catalog.
const MCP_REGISTRY_BASE: &str = "https://registry.modelcontextprotocol.io";
/// The public Skill index, used for search only.
const SKILL_INDEX_SEARCH: &str = "https://www.skills.sh/api/search";
/// Lists a repository's files without touching the GitHub API, which rate
/// limits unauthenticated callers to a handful of requests per hour.
const JSDELIVR_DATA: &str = "https://data.jsdelivr.com/v1/packages/gh";
/// Serves the document itself.
const JSDELIVR_CDN: &str = "https://cdn.jsdelivr.net/gh";

/// One request may not take longer than this, including every redirect.
const MARKET_REQUEST_TIMEOUT: Duration = Duration::from_secs(10);
/// Redirect hops followed before the fetch is refused.
const MAX_MARKET_REDIRECTS: usize = 5;
/// Entries a single response may contribute, so one huge catalog cannot flood
/// the list.
const MAX_ENTRIES_PER_SEARCH: usize = 200;
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
        if let Some(length) = response.content_length() {
            if length > limit {
                return Err(VibexError::provider(
                    "market_response_too_large",
                    "the market response exceeded the size limit",
                ));
            }
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

fn registry_server_to_entry(server: &RegistryServer) -> Option<McpMarketEntry> {
    let display = server
        .title
        .as_deref()
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .unwrap_or(server.name.as_str())
        .to_string();
    let homepage = server
        .homepage
        .clone()
        .or_else(|| server.website_url.clone());

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
            name: display,
            description: server.description.clone(),
            homepage,
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
        name: display,
        description: server.description.clone(),
        homepage,
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
    query: Option<&str>,
    limit: u32,
) -> VibexResult<(Vec<McpMarketEntry>, bool)> {
    let mut url = format!("{MCP_REGISTRY_BASE}/v0.1/servers?version=latest&limit={limit}");
    if let Some(query) = query.map(str::trim).filter(|value| !value.is_empty()) {
        url.push_str("&search=");
        url.push_str(&urlencode(query));
    }
    let response: RegistryListResponse = fetch_market_json(client, &url)?;
    let has_more = response
        .metadata
        .as_ref()
        .and_then(|metadata| metadata.next_cursor.as_deref())
        .is_some_and(|cursor| !cursor.is_empty());
    let entries = response
        .servers
        .iter()
        .filter_map(|record| record.server.as_ref())
        .filter_map(registry_server_to_entry)
        .take(MAX_ENTRIES_PER_SEARCH)
        .collect();
    Ok((entries, has_more))
}

// ---------------------------------------------------------------------------
// Skill index adapter
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
struct SkillIndexResponse {
    #[serde(default)]
    skills: Vec<SkillIndexSkill>,
    #[serde(default)]
    count: u64,
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
    let total = response.count;
    let has_more = (offset as u64 + entries.len() as u64) < total;
    Ok(SkillMarketSearchResponse {
        entries,
        total,
        has_more,
    })
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
    pub fn search_mcp_market(
        &self,
        request: McpMarketSearchRequest,
    ) -> VibexResult<McpMarketSearchResponse> {
        let client = market_http_client()?;
        let limit = request.limit.unwrap_or(30).clamp(1, 100);
        let (entries, has_more) = search_mcp_registry(&client, request.query.as_deref(), limit)?;
        Ok(McpMarketSearchResponse { entries, has_more })
    }

    pub fn search_skill_market(
        &self,
        request: SkillMarketSearchRequest,
    ) -> VibexResult<SkillMarketSearchResponse> {
        let client = market_http_client()?;
        let query = request.query.as_deref().map(str::trim).unwrap_or_default();
        // The index refuses anything shorter than two characters, so a short
        // query is answered locally instead of being sent to fail.
        if query.chars().count() < 2 {
            return Ok(SkillMarketSearchResponse {
                entries: Vec::new(),
                total: 0,
                has_more: false,
            });
        }
        let limit = request.limit.unwrap_or(30).clamp(1, 100);
        search_skill_index(&client, query, limit, request.offset.unwrap_or(0))
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
        let candidate = request.candidate.clone();
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
        } else {
            McpServerRepository::insert(&conn, &server)?;
        }
        McpServerRepository::replace_agent_matrix(&conn, &server.id, &server.agent_matrix)?;
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
}

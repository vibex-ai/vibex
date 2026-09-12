//! Where each Agent keeps its own MCP configuration file.
//!
//! Vibex normally delivers MCP servers over ACP (`session/new.mcpServers`).
//! Several Agents never look at the wire: they parse their own configuration
//! file at launch, so for them the file *is* the delivery path. A few Agents
//! both read a native file and accept the wire field, which is why native
//! export stays an explicit, previewed action instead of something the runtime
//! does behind the user's back.
//!
//! This module is the single source of truth for that per-Agent knowledge. Each
//! entry names the file, its format and the container key that holds the server
//! map, and every entry is cross-checked against the reader in
//! [`crate::native_import`] by the round-trip tests: a file Vibex writes has to
//! be discoverable again by the import scanner.
//!
//! ## Why writes are not plain parse-and-serialize
//!
//! A native config file belongs to the user and the Agent, not to Vibex. It
//! carries comments, unrelated settings and — for Claude Code — a large amount
//! of session state. Re-serializing it would discard comments and reformat
//! everything, so:
//!
//! * JSON files are edited by replacing the byte span of the container value,
//!   leaving every other byte (including key order and indentation) untouched.
//! * TOML and YAML files cannot carry a spliceable structure without a
//!   format-preserving parser, so Vibex owns a marker-delimited block instead.
//!   A file whose container already exists outside that block is refused rather
//!   than rewritten, because replacing it would silently delete servers the
//!   user configured by hand.

use vibex_core::ProviderNativeConfigFileKind;

/// Separates Vibex-owned regions from user content in TOML and YAML files.
pub(crate) const MCP_MARKER_START: &str = "# >>> VIBEX MANAGED MCP EXPORT";
pub(crate) const MCP_MARKER_END: &str = "# <<< VIBEX MANAGED MCP EXPORT";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum NativeFileFormat {
    Json,
    Toml,
    Yaml,
}

/// One Agent's native MCP configuration file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct NativeMcpSurface {
    /// Path below the Agent home directory.
    pub relative_path: &'static str,
    pub format: NativeFileFormat,
    /// Key that holds the server map at the file's top level.
    pub container: &'static str,
    /// Whether the Agent home may be relocated by an environment variable.
    pub file_kind: ProviderNativeConfigFileKind,
}

/// A server entry ready to be written into a native file.
///
/// Secrets are already resolved (or deliberately absent) by the caller: this
/// module only knows how to place values into a file.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct NativeMcpEntry {
    pub name: String,
    pub transport: NativeMcpTransport,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeMcpTransport {
    Stdio {
        command: String,
        args: Vec<String>,
        env: Vec<(String, String)>,
    },
    Http {
        url: String,
        headers: Vec<(String, String)>,
    },
    Sse {
        url: String,
        headers: Vec<(String, String)>,
    },
}

/// Why a native MCP export cannot be prepared for an Agent.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum NativeSurfaceError {
    /// The file exists and defines the container outside Vibex's block.
    ForeignContainer { container: String },
    /// The existing file could not be parsed as its own format.
    Unparsable { error: String },
}

struct SurfaceRow {
    agent_id: &'static str,
    mcp: Option<NativeMcpSurface>,
    mcp_absent_reason: &'static str,
}

const NO_MCP_FILE: &str = "this Agent has no native MCP configuration file Vibex can write; it receives MCP servers over the ACP wire";

/// Agents whose CLI parses an MCP configuration file at launch.
///
/// Paths are relative to the Agent home resolved by
/// [`crate::import_scan_agent_skill_roots`]'s sibling logic, so an environment
/// override such as `CODEX_HOME` moves the export target with it.
const SURFACE_ROWS: &[SurfaceRow] = &[
    SurfaceRow {
        agent_id: "claude",
        mcp: Some(NativeMcpSurface {
            // Claude Code keeps user-scope MCP servers beside its state file
            // rather than inside the config directory, which is why this path
            // is not under `~/.claude`.
            relative_path: "../.claude.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::ClaudeMcpJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "codex",
        mcp: Some(NativeMcpSurface {
            relative_path: "config.toml",
            format: NativeFileFormat::Toml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::CodexConfigToml,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "cursor",
        mcp: Some(NativeMcpSurface {
            relative_path: "mcp.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentMcpJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "grok",
        mcp: Some(NativeMcpSurface {
            relative_path: "config.toml",
            format: NativeFileFormat::Toml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigToml,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "hermes",
        mcp: Some(NativeMcpSurface {
            relative_path: "config.yaml",
            format: NativeFileFormat::Yaml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigYaml,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "gemini",
        mcp: Some(NativeMcpSurface {
            relative_path: "settings.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "kimi",
        mcp: Some(NativeMcpSurface {
            relative_path: "mcp.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentMcpJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "opencode",
        mcp: Some(NativeMcpSurface {
            relative_path: "opencode.json",
            format: NativeFileFormat::Json,
            container: "mcp",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "qwen-code",
        mcp: Some(NativeMcpSurface {
            relative_path: "settings.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "cline",
        mcp: Some(NativeMcpSurface {
            relative_path: "cline_mcp_settings.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentMcpJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "codebuddy-code",
        mcp: Some(NativeMcpSurface {
            relative_path: "settings.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "copilot",
        mcp: Some(NativeMcpSurface {
            relative_path: "settings.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
    SurfaceRow {
        agent_id: "openclaw",
        mcp: Some(NativeMcpSurface {
            relative_path: "openclaw.json",
            format: NativeFileFormat::Json,
            container: "mcpServers",
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }),
        mcp_absent_reason: NO_MCP_FILE,
    },
];

/// The native MCP file for `agent_id`, when Vibex knows one.
pub(crate) fn native_mcp_surface(agent_id: &str) -> Option<&'static NativeMcpSurface> {
    SURFACE_ROWS
        .iter()
        .find(|row| row.agent_id == agent_id)
        .and_then(|row| row.mcp.as_ref())
}

/// Why an Agent has no native MCP file, for an honest blocked diagnostic.
pub(crate) fn native_mcp_absent_reason(agent_id: &str) -> &'static str {
    SURFACE_ROWS
        .iter()
        .find(|row| row.agent_id == agent_id)
        .map(|row| row.mcp_absent_reason)
        .unwrap_or(NO_MCP_FILE)
}

/// Every Agent that has a native MCP file, for coverage reporting.
#[cfg(test)]
pub(crate) fn native_mcp_agents() -> impl Iterator<Item = &'static str> {
    SURFACE_ROWS
        .iter()
        .filter(|row| row.mcp.is_some())
        .map(|row| row.agent_id)
}

/// Renders the whole native file for `entries`, or explains why it will not.
///
/// `existing` is the current file content when the file exists. The returned
/// string is the complete new file content; the caller diffs, backs up and
/// replaces it atomically.
pub(crate) fn render_mcp_file(
    surface: &NativeMcpSurface,
    existing: Option<&str>,
    entries: &[NativeMcpEntry],
) -> Result<RenderedNativeFile, NativeSurfaceError> {
    match surface.format {
        NativeFileFormat::Json => render_json_file(surface, existing, entries),
        NativeFileFormat::Toml => {
            render_marker_file(surface, existing, entries, MarkerSyntax::Toml)
        }
        NativeFileFormat::Yaml => {
            render_marker_file(surface, existing, entries, MarkerSyntax::Yaml)
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RenderedNativeFile {
    pub content: String,
    /// Entries this surface cannot express, with the reason.
    pub skipped: Vec<(String, String)>,
}

enum MarkerSyntax {
    Toml,
    Yaml,
}

fn render_json_file(
    surface: &NativeMcpSurface,
    existing: Option<&str>,
    entries: &[NativeMcpEntry],
) -> Result<RenderedNativeFile, NativeSurfaceError> {
    let mut skipped = Vec::new();
    let current = existing.unwrap_or("").trim();
    let base = if current.is_empty() {
        serde_json::json!({})
    } else {
        serde_json::from_str::<serde_json::Value>(current).map_err(|error| {
            NativeSurfaceError::Unparsable {
                error: error.to_string(),
            }
        })?
    };
    if !base.is_object() {
        return Err(NativeSurfaceError::Unparsable {
            error: "the file's top level is not a JSON object".to_string(),
        });
    }

    let mut container = serde_json::Map::new();
    for entry in entries {
        match json_entry(entry) {
            Some(value) => {
                container.insert(entry.name.clone(), value);
            }
            None => skipped.push((
                entry.name.clone(),
                "this Agent's native file cannot express the entry's transport".to_string(),
            )),
        }
    }

    let value = serde_json::Value::Object(container);
    let content = match current.is_empty() {
        true => format!(
            "{}\n",
            serde_json::to_string_pretty(&serde_json::json!({
                surface.container: value,
            }))
            .unwrap_or_else(|_| "{}".to_string())
        ),
        false => splice_json_container(current, surface.container, &value),
    };
    Ok(RenderedNativeFile { content, skipped })
}

/// Replaces the byte span of a top-level key's value, preserving every other
/// byte of the document.
fn splice_json_container(original: &str, container: &str, value: &serde_json::Value) -> String {
    let rendered = serde_json::to_string_pretty(value).unwrap_or_else(|_| "{}".to_string());
    match json_container_span(original, container) {
        Some((start, end)) => {
            let mut next = String::with_capacity(original.len() + rendered.len());
            next.push_str(&original[..start]);
            next.push_str(&rendered);
            next.push_str(&original[end..]);
            next
        }
        None => match original.rfind('}') {
            Some(close) => {
                let head = original[..close].trim_end();
                let separator = if head.ends_with('{') { "" } else { "," };
                format!(
                    "{head}{separator}\n  \"{container}\": {rendered}\n{tail}",
                    tail = &original[close..]
                )
            }
            None => original.to_string(),
        },
    }
}

/// Byte span of the value that follows the top-level `"key"` member.
///
/// Only the top level is inspected, so a nested `mcpServers` inside an
/// unrelated subtree can never be mistaken for the container. String literals
/// and escapes are tracked so braces inside values do not move the depth.
fn json_container_span(source: &str, key: &str) -> Option<(usize, usize)> {
    let bytes = source.as_bytes();
    let mut index = 0;
    let mut depth = 0usize;
    while index < bytes.len() {
        match bytes[index] {
            b'{' | b'[' => {
                depth += 1;
                index += 1;
            }
            b'}' | b']' => {
                depth = depth.saturating_sub(1);
                index += 1;
            }
            b'"' => {
                let (literal, next) = json_string_literal(bytes, index)?;
                index = next;
                if depth != 1 {
                    continue;
                }
                let decoded: String = serde_json::from_str(literal).ok()?;
                if decoded != key {
                    continue;
                }
                let mut cursor = index;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                if bytes.get(cursor) != Some(&b':') {
                    continue;
                }
                cursor += 1;
                while cursor < bytes.len() && bytes[cursor].is_ascii_whitespace() {
                    cursor += 1;
                }
                let end = json_value_end(bytes, cursor)?;
                return Some((cursor, end));
            }
            _ => index += 1,
        }
    }
    None
}

/// Returns the raw literal (including quotes) and the index just past it.
fn json_string_literal(bytes: &[u8], start: usize) -> Option<(&str, usize)> {
    let mut index = start + 1;
    while index < bytes.len() {
        match bytes[index] {
            b'\\' => index += 2,
            b'"' => {
                let end = index + 1;
                return std::str::from_utf8(&bytes[start..end])
                    .ok()
                    .map(|literal| (literal, end));
            }
            _ => index += 1,
        }
    }
    None
}

/// End offset of the JSON value starting at `start`.
fn json_value_end(bytes: &[u8], start: usize) -> Option<usize> {
    match bytes.get(start)? {
        b'"' => json_string_literal(bytes, start).map(|(_, end)| end),
        b'{' | b'[' => {
            let mut index = start;
            let mut depth = 0usize;
            while index < bytes.len() {
                match bytes[index] {
                    b'"' => {
                        index = json_string_literal(bytes, index)?.1;
                        continue;
                    }
                    b'{' | b'[' => depth += 1,
                    b'}' | b']' => {
                        depth -= 1;
                        if depth == 0 {
                            return Some(index + 1);
                        }
                    }
                    _ => {}
                }
                index += 1;
            }
            None
        }
        _ => {
            let mut index = start;
            while index < bytes.len()
                && !matches!(bytes[index], b',' | b'}' | b']')
                && !bytes[index].is_ascii_whitespace()
            {
                index += 1;
            }
            Some(index)
        }
    }
}

fn render_marker_file(
    surface: &NativeMcpSurface,
    existing: Option<&str>,
    entries: &[NativeMcpEntry],
    syntax: MarkerSyntax,
) -> Result<RenderedNativeFile, NativeSurfaceError> {
    let mut skipped = Vec::new();
    let current = existing.unwrap_or("");
    let body = match syntax {
        MarkerSyntax::Toml => toml_container_body(surface.container, entries, &mut skipped),
        MarkerSyntax::Yaml => yaml_container_body(surface.container, entries, &mut skipped),
    };
    let block = format!("{MCP_MARKER_START}\n{body}{MCP_MARKER_END}\n");

    match (
        current.contains(MCP_MARKER_START),
        current.contains(MCP_MARKER_END),
    ) {
        (true, true) => Ok(RenderedNativeFile {
            content: replace_marked_region(current, &block),
            skipped,
        }),
        (false, false) => {
            if file_defines_container(surface, current)? {
                return Err(NativeSurfaceError::ForeignContainer {
                    container: surface.container.to_string(),
                });
            }
            let mut content = current.trim_end().to_string();
            if !content.is_empty() {
                content.push_str("\n\n");
            }
            content.push_str(&block);
            Ok(RenderedNativeFile { content, skipped })
        }
        // A half-written block means a previous run was interrupted; refuse
        // rather than guess which half is authoritative.
        _ => Err(NativeSurfaceError::Unparsable {
            error: "the file contains only one half of the Vibex MCP marker pair".to_string(),
        }),
    }
}

/// Whether a file the user owns already defines the container.
fn file_defines_container(
    surface: &NativeMcpSurface,
    current: &str,
) -> Result<bool, NativeSurfaceError> {
    if current.trim().is_empty() {
        return Ok(false);
    }
    match surface.format {
        NativeFileFormat::Json => Ok(serde_json::from_str::<serde_json::Value>(current)
            .map(|value| value.get(surface.container).is_some())
            .unwrap_or(false)),
        NativeFileFormat::Toml => Ok(current.parse::<toml::Value>().map_or_else(
            |_| toml_mentions_container(current, surface.container),
            |value| value.get(surface.container).is_some(),
        )),
        NativeFileFormat::Yaml => Ok(serde_yaml::from_str::<serde_yaml::Value>(current)
            .ok()
            .and_then(|value| {
                serde_json::to_value(value)
                    .ok()
                    .and_then(|value| value.get(surface.container).cloned())
            })
            .is_some()),
    }
}

/// Fallback for a TOML file that does not parse: a `[mcp_servers.x]` header is
/// still an unambiguous claim on the container.
fn toml_mentions_container(current: &str, container: &str) -> bool {
    let header = format!("[{container}");
    current
        .lines()
        .map(str::trim)
        .any(|line| line.starts_with(&header) && line.starts_with('['))
}

fn toml_container_body(
    container: &str,
    entries: &[NativeMcpEntry],
    skipped: &mut Vec<(String, String)>,
) -> String {
    let mut body = String::new();
    for entry in entries {
        let NativeMcpTransport::Stdio { command, args, env } = &entry.transport else {
            skipped.push((
                entry.name.clone(),
                "this Agent reads only stdio servers from its TOML config".to_string(),
            ));
            continue;
        };
        body.push_str(&format!(
            "[{container}.{}]\ncommand = \"{}\"\n",
            toml_key(&entry.name),
            escape_toml(command)
        ));
        if !args.is_empty() {
            let rendered = args
                .iter()
                .map(|arg| format!("\"{}\"", escape_toml(arg)))
                .collect::<Vec<_>>()
                .join(", ");
            body.push_str(&format!("args = [{rendered}]\n"));
        }
        if !env.is_empty() {
            body.push_str(&format!("[{container}.{}.env]\n", toml_key(&entry.name)));
            for (name, value) in env {
                body.push_str(&format!(
                    "\"{}\" = \"{}\"\n",
                    escape_toml(name),
                    escape_toml(value)
                ));
            }
        }
        body.push('\n');
    }
    body
}

fn yaml_container_body(
    container: &str,
    entries: &[NativeMcpEntry],
    skipped: &mut Vec<(String, String)>,
) -> String {
    let mut body = format!("{container}:\n");
    if entries.is_empty() {
        body.push_str("  {}\n");
        return body;
    }
    for entry in entries {
        let NativeMcpTransport::Stdio { command, args, env } = &entry.transport else {
            skipped.push((
                entry.name.clone(),
                "this Agent reads only stdio servers from its YAML config".to_string(),
            ));
            continue;
        };
        body.push_str(&format!(
            "  {}:\n    command: \"{}\"\n",
            yaml_scalar(&entry.name),
            escape_yaml(command)
        ));
        if !args.is_empty() {
            body.push_str("    args:\n");
            for arg in args {
                body.push_str(&format!("      - \"{}\"\n", escape_yaml(arg)));
            }
        }
        if !env.is_empty() {
            body.push_str("    env:\n");
            for (name, value) in env {
                body.push_str(&format!(
                    "      \"{}\": \"{}\"\n",
                    escape_yaml(name),
                    escape_yaml(value)
                ));
            }
        }
    }
    body
}

/// Replaces the region between the markers, keeping everything outside it.
fn replace_marked_region(current: &str, block: &str) -> String {
    let Some(start) = current.find(MCP_MARKER_START) else {
        return current.to_string();
    };
    let Some(relative_end) = current[start..].find(MCP_MARKER_END) else {
        return current.to_string();
    };
    let end = start + relative_end + MCP_MARKER_END.len();
    let tail = current[end..].strip_prefix('\n').unwrap_or(&current[end..]);
    let head = current[..start].trim_end();
    if head.is_empty() {
        format!("{block}{tail}")
    } else {
        format!("{head}\n\n{block}{tail}")
    }
}

fn json_entry(entry: &NativeMcpEntry) -> Option<serde_json::Value> {
    Some(match &entry.transport {
        NativeMcpTransport::Stdio { command, args, env } => {
            let mut value = serde_json::json!({ "command": command });
            if !args.is_empty() {
                value["args"] = serde_json::json!(args);
            }
            if !env.is_empty() {
                let map = env
                    .iter()
                    .map(|(name, value)| (name.clone(), serde_json::json!(value)))
                    .collect::<serde_json::Map<_, _>>();
                value["env"] = serde_json::Value::Object(map);
            }
            value
        }
        NativeMcpTransport::Http { url, headers } => {
            let mut value = serde_json::json!({ "type": "http", "url": url });
            if !headers.is_empty() {
                let map = headers
                    .iter()
                    .map(|(name, value)| (name.clone(), serde_json::json!(value)))
                    .collect::<serde_json::Map<_, _>>();
                value["headers"] = serde_json::Value::Object(map);
            }
            value
        }
        NativeMcpTransport::Sse { url, headers } => {
            let mut value = serde_json::json!({ "type": "sse", "url": url });
            if !headers.is_empty() {
                let map = headers
                    .iter()
                    .map(|(name, value)| (name.clone(), serde_json::json!(value)))
                    .collect::<serde_json::Map<_, _>>();
                value["headers"] = serde_json::Value::Object(map);
            }
            value
        }
    })
}

/// TOML bare keys allow only `A-Za-z0-9_-`; anything else has to be quoted.
fn toml_key(value: &str) -> String {
    if !value.is_empty()
        && value
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || ch == '-' || ch == '_')
    {
        value.to_string()
    } else {
        format!("\"{}\"", escape_toml(value))
    }
}

fn escape_toml(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn yaml_scalar(value: &str) -> String {
    format!("\"{}\"", escape_yaml(value))
}

fn escape_yaml(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

/// Skill root for an Agent: `<agent home>/skills`, the layout the scanner in
/// [`crate::import_scan_agent_skill_roots`] discovers.
pub(crate) const SKILLS_DIR_NAME: &str = "skills";

/// Manifest file name every Agent loads from a Skill directory.
pub(crate) const SKILL_MANIFEST_NAME: &str = "SKILL.md";

/// Builds the `SKILL.md` text for a Skill that Vibex owns.
///
/// Frontmatter is emitted only when it is missing, so a Skills folder that was
/// imported from an Agent keeps the exact manifest it had; a Skill authored in
/// Vibex gets the minimal frontmatter Agents need to list it.
pub(crate) fn render_skill_manifest(
    display_name: &str,
    description: Option<&str>,
    command_name: &str,
    body: &str,
) -> String {
    let trimmed = body.trim_start();
    if trimmed.starts_with("---") {
        return format!("{}\n", body.trim_end());
    }
    let mut rendered = String::from("---\n");
    rendered.push_str(&format!("name: {}\n", yaml_plain(display_name)));
    rendered.push_str(&format!("command: {}\n", yaml_plain(command_name)));
    if let Some(description) = description.filter(|value| !value.trim().is_empty()) {
        rendered.push_str(&format!("description: {}\n", yaml_plain(description)));
    }
    rendered.push_str("---\n\n");
    rendered.push_str(body.trim_end());
    rendered.push('\n');
    rendered
}

/// Quotes a frontmatter value only when a bare scalar would be ambiguous.
fn yaml_plain(value: &str) -> String {
    let needs_quoting = value.is_empty()
        || value
            .chars()
            .any(|ch| matches!(ch, ':' | '#' | '"' | '\'' | '\n') || ch == '\t')
        || value.starts_with(['-', '?', '*', '&', '!', '|', '>', '@', '`', '[', '{', '%'])
        || value.trim() != value;
    if needs_quoting {
        format!("\"{}\"", escape_yaml(value))
    } else {
        value.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stdio(name: &str, command: &str) -> NativeMcpEntry {
        NativeMcpEntry {
            name: name.to_string(),
            transport: NativeMcpTransport::Stdio {
                command: command.to_string(),
                args: vec!["-y".to_string(), "pkg".to_string()],
                env: vec![("TOKEN".to_string(), "secret".to_string())],
            },
        }
    }

    fn json_surface(container: &'static str) -> NativeMcpSurface {
        NativeMcpSurface {
            relative_path: "settings.json",
            format: NativeFileFormat::Json,
            container,
            file_kind: ProviderNativeConfigFileKind::AgentSettingsJson,
        }
    }

    #[test]
    fn json_splice_preserves_every_other_byte_of_the_document() {
        let surface = json_surface("mcpServers");
        let existing = "{\n  \"projects\": {\n    \"/work\": {\"history\": [1, 2, 3]}\n  },\n  \"mcpServers\": {\n    \"old\": {\"command\": \"old\"}\n  },\n  \"theme\": \"dark\"\n}\n";
        let rendered = render_mcp_file(&surface, Some(existing), &[stdio("files", "npx")])
            .expect("renderable");

        assert!(rendered.content.contains("\"/work\""));
        assert!(rendered.content.contains("\"theme\": \"dark\""));
        assert!(rendered.content.contains("\"files\""));
        assert!(!rendered.content.contains("\"old\""));
        // The untouched head must survive byte for byte.
        assert!(
            rendered.content.starts_with(
                "{\n  \"projects\": {\n    \"/work\": {\"history\": [1, 2, 3]}\n  },\n"
            )
        );
        // And the spliced document must still be valid JSON.
        let value: serde_json::Value = serde_json::from_str(&rendered.content).expect("valid JSON");
        assert_eq!(value["theme"], "dark");
        assert_eq!(value["mcpServers"]["files"]["command"], "npx");
    }

    #[test]
    fn json_container_is_added_when_absent() {
        let surface = json_surface("mcpServers");
        let rendered = render_mcp_file(&surface, Some("{\n  \"theme\": \"dark\"\n}\n"), &[])
            .expect("renderable");
        let value: serde_json::Value = serde_json::from_str(&rendered.content).expect("valid JSON");
        assert_eq!(value["theme"], "dark");
        assert!(value.get("mcpServers").is_some());
    }

    #[test]
    fn json_span_ignores_a_nested_container_with_the_same_name() {
        let source = "{\n  \"outer\": {\"mcpServers\": {\"nested\": 1}},\n  \"mcpServers\": {\"real\": 1}\n}\n";
        let (start, end) = json_container_span(source, "mcpServers").expect("span");
        assert!(source[start..end].contains("\"real\""));
    }

    #[test]
    fn json_span_survives_strings_that_contain_braces() {
        let source = "{\n  \"note\": \"} tricky {\",\n  \"mcpServers\": {}\n}\n";
        let (start, end) = json_container_span(source, "mcpServers").expect("span");
        assert_eq!(&source[start..end], "{}");
    }

    #[test]
    fn toml_export_owns_a_marked_block_and_is_idempotent() {
        let surface = NativeMcpSurface {
            relative_path: "config.toml",
            format: NativeFileFormat::Toml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigToml,
        };
        let existing = "# my own comment\nmodel = \"gpt-5\"\n";
        let first = render_mcp_file(&surface, Some(existing), &[stdio("files", "npx")])
            .expect("renderable");
        assert!(first.content.contains("# my own comment"));
        assert!(first.content.contains("[mcp_servers.files]"));
        assert!(first.content.contains("command = \"npx\""));

        // Re-rendering with a different set replaces the block instead of
        // appending a second one.
        let second = render_mcp_file(&surface, Some(&first.content), &[stdio("other", "uvx")])
            .expect("renderable");
        assert_eq!(second.content.matches(MCP_MARKER_START).count(), 1);
        assert!(!second.content.contains("[mcp_servers.files]"));
        assert!(second.content.contains("[mcp_servers.other]"));
        assert!(second.content.contains("model = \"gpt-5\""));
        // The result has to stay parseable as TOML.
        second.content.parse::<toml::Value>().expect("valid TOML");
    }

    #[test]
    fn toml_export_refuses_a_container_the_user_already_owns() {
        let surface = NativeMcpSurface {
            relative_path: "config.toml",
            format: NativeFileFormat::Toml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigToml,
        };
        let existing = "[mcp_servers.mine]\ncommand = \"mine\"\n";
        assert_eq!(
            render_mcp_file(&surface, Some(existing), &[]),
            Err(NativeSurfaceError::ForeignContainer {
                container: "mcp_servers".to_string()
            })
        );
    }

    #[test]
    fn yaml_export_writes_a_container_the_scanner_can_read_back() {
        let surface = NativeMcpSurface {
            relative_path: "config.yaml",
            format: NativeFileFormat::Yaml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigYaml,
        };
        let rendered = render_mcp_file(&surface, Some("other: 1\n"), &[stdio("files", "npx")])
            .expect("renderable");
        let value: serde_yaml::Value = serde_yaml::from_str(&rendered.content).expect("valid YAML");
        let json = serde_json::to_value(value).expect("convertible");
        assert_eq!(json["other"], 1);
        assert_eq!(json["mcp_servers"]["files"]["command"], "npx");
    }

    #[test]
    fn non_stdio_entries_are_reported_instead_of_written_for_text_formats() {
        let surface = NativeMcpSurface {
            relative_path: "config.toml",
            format: NativeFileFormat::Toml,
            container: "mcp_servers",
            file_kind: ProviderNativeConfigFileKind::AgentConfigToml,
        };
        let entry = NativeMcpEntry {
            name: "remote".to_string(),
            transport: NativeMcpTransport::Http {
                url: "https://example.test/mcp".to_string(),
                headers: Vec::new(),
            },
        };
        let rendered = render_mcp_file(&surface, None, &[entry]).expect("renderable");
        assert_eq!(rendered.skipped.len(), 1);
        assert!(!rendered.content.contains("remote"));
    }

    #[test]
    fn every_profiled_agent_with_a_native_file_uses_a_known_container() {
        for agent_id in native_mcp_agents() {
            let surface = native_mcp_surface(agent_id).expect("row has a surface");
            assert!(
                !surface.relative_path.is_empty(),
                "{agent_id} must name a file"
            );
            assert!(
                !surface.container.is_empty(),
                "{agent_id} needs a container"
            );
            if surface.format != NativeFileFormat::Json {
                // Marker blocks are comment-based, so a non-JSON surface must
                // not claim a container that the marker syntax cannot hold.
                assert!(
                    matches!(surface.container, "mcp_servers"),
                    "{agent_id} uses an unexpected text container"
                );
            }
        }
    }

    #[test]
    fn skill_manifest_keeps_imported_frontmatter_verbatim() {
        let imported = "---\nname: Review\n---\n\nReview changes.\n";
        assert_eq!(
            render_skill_manifest("Review", None, "review", imported),
            imported
        );
    }

    #[test]
    fn skill_manifest_adds_frontmatter_for_a_vibex_authored_skill() {
        let rendered = render_skill_manifest(
            "Rust Quality",
            Some("Check gates"),
            "rust-quality",
            "Run the gates.",
        );
        assert!(rendered.starts_with("---\n"));
        assert!(rendered.contains("command: rust-quality"));
        assert!(rendered.contains("description: Check gates"));
        assert!(rendered.ends_with("Run the gates.\n"));
    }
}

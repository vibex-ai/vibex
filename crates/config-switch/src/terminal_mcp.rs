//! MCP delivery for Agents that run in their own terminal interface.
//!
//! An ACP session receives its MCP servers on `session/new.mcpServers`. A
//! terminal-mode session has no such request: Vibex starts the Agent's own CLI
//! in a PTY and never speaks its protocol, so the only channels left are the
//! ones each CLI reads at launch — a flag, an environment variable, or a file.
//! This module owns which channel each Agent gets and how its native format
//! spells a server.
//!
//! Every channel is *additive*. The CLI keeps reading the reader's own MCP
//! configuration, and the servers Vibex adds sit beside it; no file the reader
//! owns is written. Two consequences follow:
//!
//! * A Provider Profile can relocate an Agent's home into a private projection
//!   root (`CODEX_HOME`, `COPILOT_HOME`, `HERMES_HOME`, `CLINE_DATA_DIR`). The
//!   CLI then no longer sees the servers the reader configured in the real
//!   home, so those are read from the real home and delivered with Vibex's.
//! * Agents whose only MCP inputs are their home and project files (Cursor,
//!   Gemini, Grok, Kimi Code) or that have none (Devin) get nothing here. Writing the
//!   reader's own files behind their back is what native export exists to do
//!   explicitly, with a preview.
//!
//! Credentials never reach the argv, which every local user can read. Files
//! are written 0600 under the session's private directory; Codex, whose only
//! additive channel is `-c` overrides, receives the names of environment
//! variables that hold the values instead.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use serde_json::{Map, Value, json};
use vibex_core::{RequestId, VibexError, VibexResult};

/// One MCP server a terminal-mode Agent should be able to call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalMcpServer {
    /// The server name the Agent registers, already an identifier
    /// (`[A-Za-z0-9_-]`): CLIs build tool names such as `mcp__<name>__<tool>`
    /// from it.
    pub name: String,
    pub transport: TerminalMcpTransport,
}

/// How a server is reached, with resolved secrets already merged in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TerminalMcpTransport {
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

/// The launch input that carries MCP servers to one Agent's native CLI.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TerminalMcpChannel {
    /// `--mcp-config=<file>`. Claude Code and CodeBuddy Code load it in
    /// addition to their own user, project, and local MCP files.
    McpConfigFlag,
    /// `--additional-mcp-config @<file>`, which augments GitHub Copilot CLI's
    /// `mcp-config.json` for this process only.
    CopilotAdditionalConfig,
    /// `-c mcp_servers.<name>.<field>=<value>` overrides on top of Codex's
    /// `config.toml`.
    CodexConfigOverrides,
    /// The `mcp` table of `OPENCODE_CONFIG_CONTENT`, which OpenCode merges
    /// after its global and project configuration.
    OpenCodeConfigContent,
    /// `HERMES_MANAGED_DIR`, a config layer Hermes deep-merges over
    /// `config.yaml` at the leaf.
    HermesManagedScope,
    /// `CLINE_MCP_SETTINGS_PATH`. It *replaces* Cline's settings file, so the
    /// file Vibex writes restates the reader's servers before adding its own.
    ClineSettingsPath,
    /// The Agent has no MCP client; only the process environment reaches it.
    EnvironmentOnly,
    /// No per-process channel exists without rewriting the reader's files.
    Unavailable,
}

impl TerminalMcpChannel {
    /// Whether this channel can hand an MCP server to the CLI.
    ///
    /// Product surfaces read this to decide whether a panel delivered as an MCP
    /// server — the browser, and computer use for every Agent that has an MCP
    /// client — can be offered to a terminal-mode session at all.
    pub fn carries_mcp_servers(self) -> bool {
        !matches!(
            self,
            TerminalMcpChannel::EnvironmentOnly | TerminalMcpChannel::Unavailable
        )
    }
}

/// The channel a terminal-mode session of `agent_id` uses.
pub fn terminal_mcp_channel(agent_id: &str) -> TerminalMcpChannel {
    match agent_id {
        "claude" | "codebuddy-code" => TerminalMcpChannel::McpConfigFlag,
        "copilot" => TerminalMcpChannel::CopilotAdditionalConfig,
        "codex" => TerminalMcpChannel::CodexConfigOverrides,
        "opencode" => TerminalMcpChannel::OpenCodeConfigContent,
        "hermes" => TerminalMcpChannel::HermesManagedScope,
        "cline" => TerminalMcpChannel::ClineSettingsPath,
        // pi has no MCP client; its computer-use path is a shell command that
        // reads the endpoint and token from the process environment.
        "pi" => TerminalMcpChannel::EnvironmentOnly,
        // cursor-agent reads only `~/.cursor/mcp.json` and the project's
        // `.cursor/mcp.json`; Gemini CLI's only tier outside its home and the
        // project is the system one, which it skips unless root owns the file
        // and every parent directory; Grok's `GROK_CONFIG` overlay is confined to an
        // allowlist that excludes MCP; Kimi Code reads only its home and
        // project `mcp.json`; Devin has no MCP file.
        _ => TerminalMcpChannel::Unavailable,
    }
}

/// What one launch needs to deliver its MCP servers.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TerminalMcpLaunch {
    /// Inserted right after the executable, ahead of any positional prompt.
    pub args: Vec<String>,
    /// Applied over the launch environment; a key here replaces the same key.
    pub env: Vec<(String, String)>,
    /// Server names the channel carries, Vibex's and the reader's alike.
    pub delivered: Vec<String>,
    /// Servers that could not be expressed, with the reason.
    pub skipped: Vec<(String, String)>,
}

/// The inputs of one terminal launch.
pub struct TerminalMcpRequest<'a> {
    pub agent_id: &'a str,
    pub servers: &'a [TerminalMcpServer],
    /// A directory private to this session. Files written here carry
    /// credentials and are rewritten on every launch.
    pub session_dir: &'a Path,
    /// The environment the launch already sets — the Provider projection's.
    pub launch_env: &'a [(String, String)],
    /// The environment the CLI would inherit from the Vibex process.
    pub host_env: &'a dyn Fn(&str) -> Option<String>,
    /// The reader's home directory.
    pub home_dir: Option<&'a Path>,
    /// Root for system-wide configuration (`/` outside tests).
    pub system_root: &'a Path,
}

/// Prepares the flags, environment, and files that deliver `servers`.
///
/// Nothing is written and nothing is added when there is nothing to deliver,
/// so a session without servers launches exactly as it did before.
pub fn prepare_terminal_mcp_launch(
    request: &TerminalMcpRequest<'_>,
) -> VibexResult<TerminalMcpLaunch> {
    match terminal_mcp_channel(request.agent_id) {
        TerminalMcpChannel::McpConfigFlag => mcp_config_flag(request),
        TerminalMcpChannel::CopilotAdditionalConfig => copilot_additional_config(request),
        TerminalMcpChannel::CodexConfigOverrides => codex_config_overrides(request),
        TerminalMcpChannel::OpenCodeConfigContent => opencode_config_content(request),
        TerminalMcpChannel::HermesManagedScope => hermes_managed_scope(request),
        TerminalMcpChannel::ClineSettingsPath => cline_settings_path(request),
        TerminalMcpChannel::EnvironmentOnly | TerminalMcpChannel::Unavailable => {
            Ok(TerminalMcpLaunch {
                skipped: request
                    .servers
                    .iter()
                    .map(|server| {
                        (
                            server.name.clone(),
                            "this Agent's terminal interface has no MCP input Vibex can add to"
                                .to_string(),
                        )
                    })
                    .collect(),
                ..TerminalMcpLaunch::default()
            })
        }
    }
}

/// Turns a label into a server name every CLI accepts.
///
/// Matches the ACP wire spelling, so a server is called the same thing in a
/// terminal session as in a GUI session of the same Agent.
pub fn terminal_mcp_server_name(display_name: &str, id: &str) -> String {
    let sanitize = |value: &str| {
        let mut sanitized = String::with_capacity(value.len());
        let mut separator = false;
        for character in value.trim().chars() {
            if character.is_ascii_alphanumeric() || matches!(character, '-' | '_') {
                sanitized.push(character);
                separator = false;
            } else if !separator && !sanitized.is_empty() {
                sanitized.push('-');
                separator = true;
            }
        }
        sanitized.trim_matches('-').to_string()
    };
    let name = sanitize(display_name);
    if !name.is_empty() {
        return name;
    }
    let name = sanitize(id);
    if name.is_empty() {
        "vibex-mcp".to_string()
    } else {
        name
    }
}

// ---------------------------------------------------------------------------
// Channels
// ---------------------------------------------------------------------------

fn mcp_config_flag(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut launch = TerminalMcpLaunch::default();
    let mut servers = Map::new();
    for server in request.servers {
        servers.insert(server.name.clone(), claude_entry(&server.transport));
        launch.delivered.push(server.name.clone());
    }
    if servers.is_empty() {
        return Ok(launch);
    }
    let path = request.session_dir.join("mcp-config.json");
    write_private_json(&path, &json!({ "mcpServers": servers }))?;
    // The `=` spelling matters: Claude Code declares `--mcp-config` variadic,
    // and the space-separated form would swallow a positional first prompt as
    // a second configuration file.
    launch
        .args
        .push(format!("--mcp-config={}", path.to_string_lossy()));
    Ok(launch)
}

fn copilot_additional_config(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut launch = TerminalMcpLaunch::default();
    let ours: BTreeSet<&str> = request.servers.iter().map(|s| s.name.as_str()).collect();
    let mut servers = Map::new();
    if let Some(file) = relocated_home_file(request, &COPILOT_HOME) {
        for (name, entry) in json_servers_in_file(&file, "mcpServers", &mut launch) {
            if !ours.contains(name.as_str()) {
                launch.delivered.push(name.clone());
                servers.insert(name, entry);
            }
        }
    }
    for server in request.servers {
        servers.insert(server.name.clone(), copilot_entry(&server.transport));
        launch.delivered.push(server.name.clone());
    }
    if servers.is_empty() {
        return Ok(launch);
    }
    let path = request.session_dir.join("copilot-mcp-config.json");
    write_private_json(&path, &json!({ "mcpServers": servers }))?;
    launch.args.push("--additional-mcp-config".to_string());
    launch.args.push(format!("@{}", path.to_string_lossy()));
    Ok(launch)
}

fn opencode_config_content(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut launch = TerminalMcpLaunch::default();
    if request.servers.is_empty() {
        return Ok(launch);
    }
    // The projection's document, or the reader's own when no Profile set one:
    // replacing it would drop the provider the session was created with.
    let existing = env_value(request.launch_env, OPENCODE_CONFIG_CONTENT)
        .or_else(|| (request.host_env)(OPENCODE_CONFIG_CONTENT));
    let mut document = match existing.as_deref().map(str::trim) {
        Some(text) if !text.is_empty() => match serde_json::from_str::<Value>(text) {
            Ok(Value::Object(document)) => document,
            _ => {
                launch.skipped.extend(request.servers.iter().map(|server| {
                    (
                        server.name.clone(),
                        "OPENCODE_CONFIG_CONTENT is not a JSON object Vibex can extend".to_string(),
                    )
                }));
                return Ok(launch);
            }
        },
        _ => Map::new(),
    };
    let mcp = document
        .entry("mcp".to_string())
        .or_insert_with(|| Value::Object(Map::new()));
    if !mcp.is_object() {
        *mcp = Value::Object(Map::new());
    }
    let mcp = mcp.as_object_mut().expect("replaced with an object above");
    for server in request.servers {
        mcp.insert(server.name.clone(), opencode_entry(&server.transport));
        launch.delivered.push(server.name.clone());
    }
    let encoded = serde_json::to_string(&Value::Object(document)).map_err(|error| {
        VibexError::storage(
            "terminal_mcp_encode_failed",
            "the OpenCode configuration could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    launch
        .env
        .push((OPENCODE_CONFIG_CONTENT.to_string(), encoded));
    Ok(launch)
}

fn hermes_managed_scope(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut launch = TerminalMcpLaunch::default();
    let ours: BTreeSet<&str> = request.servers.iter().map(|s| s.name.as_str()).collect();
    let mut servers = serde_yaml::Mapping::new();
    if let Some(file) = relocated_home_file(request, &HERMES_HOME) {
        for (name, entry) in yaml_servers_in_file(&file, "mcp_servers", &mut launch) {
            if !ours.contains(name.as_str()) {
                launch.delivered.push(name.clone());
                servers.insert(serde_yaml::Value::String(name), entry);
            }
        }
    }
    for server in request.servers {
        let entry = serde_yaml::to_value(hermes_entry(&server.transport)).map_err(|error| {
            VibexError::storage(
                "terminal_mcp_encode_failed",
                "the Hermes configuration could not be encoded",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        servers.insert(serde_yaml::Value::String(server.name.clone()), entry);
        launch.delivered.push(server.name.clone());
    }
    if servers.is_empty() {
        return Ok(launch);
    }
    // The managed scope Hermes would otherwise read keeps applying: the
    // variable replaces its location, so its policy is carried over.
    let source = (request.host_env)("HERMES_MANAGED_DIR")
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .unwrap_or_else(|| request.system_root.join("etc/hermes"));
    let mut document = match fs::read_to_string(source.join("config.yaml")) {
        Ok(text) => match serde_yaml::from_str::<serde_yaml::Value>(&text) {
            Ok(serde_yaml::Value::Mapping(document)) => document,
            _ => serde_yaml::Mapping::new(),
        },
        Err(_) => serde_yaml::Mapping::new(),
    };
    let key = serde_yaml::Value::String("mcp_servers".to_string());
    let mut merged = match document.remove(&key) {
        Some(serde_yaml::Value::Mapping(existing)) => existing,
        _ => serde_yaml::Mapping::new(),
    };
    for (name, entry) in servers {
        merged.insert(name, entry);
    }
    document.insert(key, serde_yaml::Value::Mapping(merged));
    let managed_dir = request.session_dir.join("hermes-managed");
    let content =
        serde_yaml::to_string(&serde_yaml::Value::Mapping(document)).map_err(|error| {
            VibexError::storage(
                "terminal_mcp_encode_failed",
                "the Hermes configuration could not be encoded",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    write_private_file(&managed_dir.join("config.yaml"), content.as_bytes())?;
    let managed_env = managed_dir.join(".env");
    match fs::read(source.join(".env")) {
        Ok(bytes) => write_private_file(&managed_env, &bytes)?,
        Err(_) => {
            let _ = fs::remove_file(&managed_env);
        }
    }
    launch.env.push((
        "HERMES_MANAGED_DIR".to_string(),
        managed_dir.to_string_lossy().into_owned(),
    ));
    Ok(launch)
}

fn cline_settings_path(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut launch = TerminalMcpLaunch::default();
    if request.servers.is_empty() {
        return Ok(launch);
    }
    let ours: BTreeSet<&str> = request.servers.iter().map(|s| s.name.as_str()).collect();
    let mut servers = Map::new();
    // The variable replaces the file Cline would have read, so that file's
    // servers are restated first: the reader's real one when a Profile moved
    // the data directory, then the one in the directory the launch uses.
    let mut sources = Vec::new();
    if let Some(file) = relocated_home_file(request, &CLINE_DATA_DIR) {
        sources.push(
            (request.host_env)(CLINE_MCP_SETTINGS_ENV)
                .filter(|path| !path.trim().is_empty())
                .map(PathBuf::from)
                .unwrap_or(file),
        );
    }
    let effective = env_value(request.launch_env, CLINE_MCP_SETTINGS_ENV)
        .or_else(|| (request.host_env)(CLINE_MCP_SETTINGS_ENV))
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            env_value(request.launch_env, CLINE_DATA_DIR.env_key)
                .or_else(|| (request.host_env)(CLINE_DATA_DIR.env_key))
                .filter(|path| !path.trim().is_empty())
                .map(|dir| PathBuf::from(dir).join(CLINE_DATA_DIR.file))
        })
        .or_else(|| {
            request.home_dir.map(|home| {
                home.join(CLINE_DATA_DIR.default_relative)
                    .join(CLINE_DATA_DIR.file)
            })
        });
    if let Some(effective) = effective
        && !sources.contains(&effective)
    {
        sources.push(effective);
    }
    for source in sources {
        for (name, entry) in json_servers_in_file(&source, "mcpServers", &mut launch) {
            if !ours.contains(name.as_str()) {
                if !servers.contains_key(&name) {
                    launch.delivered.push(name.clone());
                }
                servers.insert(name, entry);
            }
        }
    }
    for server in request.servers {
        servers.insert(server.name.clone(), cline_entry(&server.transport));
        launch.delivered.push(server.name.clone());
    }
    let path = request.session_dir.join("cline_mcp_settings.json");
    write_private_json(&path, &json!({ "mcpServers": servers }))?;
    launch.env.push((
        CLINE_MCP_SETTINGS_ENV.to_string(),
        path.to_string_lossy().into_owned(),
    ));
    Ok(launch)
}

// ---------------------------------------------------------------------------
// Codex
// ---------------------------------------------------------------------------

/// Builds `-c` overrides and the environment their credentials live in.
///
/// Codex starts a stdio server with a cleared environment plus the server's
/// `env` table and the variables its `env_vars` list names, so a value Vibex
/// puts in the Codex process environment reaches only the servers that name
/// it. HTTP credentials use `bearer_token_env_var` and `env_http_headers`,
/// which Codex resolves the same way.
fn codex_config_overrides(request: &TerminalMcpRequest<'_>) -> VibexResult<TerminalMcpLaunch> {
    let mut builder = CodexOverrides {
        request,
        launch: TerminalMcpLaunch::default(),
        env: BTreeMap::new(),
    };
    let ours: BTreeSet<&str> = request.servers.iter().map(|s| s.name.as_str()).collect();
    if let Some(file) = relocated_home_file(request, &CODEX_HOME) {
        for (name, table) in toml_servers_in_file(&file, &mut builder.launch) {
            if !ours.contains(name.as_str()) {
                builder.add_user_table(&name, table);
            }
        }
    }
    for server in request.servers {
        builder.add_server(server);
    }
    let CodexOverrides {
        mut launch, env, ..
    } = builder;
    launch.env.extend(env);
    Ok(launch)
}

struct CodexOverrides<'r, 'a> {
    request: &'r TerminalMcpRequest<'a>,
    launch: TerminalMcpLaunch,
    env: BTreeMap<String, String>,
}

impl CodexOverrides<'_, '_> {
    fn add_server(&mut self, server: &TerminalMcpServer) {
        let name = server.name.as_str();
        if !is_bare_key(name) {
            self.skip(name, "the server name is not a Codex config key");
            return;
        }
        let mut overrides = Vec::new();
        let mut env = BTreeMap::new();
        match &server.transport {
            TerminalMcpTransport::Stdio {
                command,
                args,
                env: server_env,
            } => {
                overrides.push(("command".to_string(), toml_string(command)));
                if !args.is_empty() {
                    overrides.push(("args".to_string(), toml_string_array(args)));
                }
                let mut forwarded = Vec::new();
                for (key, value) in server_env {
                    if let Err(reason) = self.forwardable(key, value, &env) {
                        self.skip(name, &reason);
                        return;
                    }
                    env.insert(key.clone(), value.clone());
                    forwarded.push(key.clone());
                }
                if !forwarded.is_empty() {
                    overrides.push(("env_vars".to_string(), toml_string_array(&forwarded)));
                }
            }
            TerminalMcpTransport::Http { url, headers } => {
                overrides.push(("url".to_string(), toml_string(url)));
                for (header, value) in headers {
                    if let Some(token) = bearer_token(header, value) {
                        let variable = codex_env_name(name, "BEARER_TOKEN");
                        overrides
                            .push(("bearer_token_env_var".to_string(), toml_string(&variable)));
                        env.insert(variable, token.to_string());
                    } else if is_bare_key(header) {
                        let variable = codex_env_name(name, &format!("HEADER_{header}"));
                        overrides
                            .push((format!("env_http_headers.{header}"), toml_string(&variable)));
                        env.insert(variable, value.clone());
                    } else {
                        self.skip(name, "a header name is not a Codex config key");
                        return;
                    }
                }
            }
            TerminalMcpTransport::Sse { .. } => {
                self.skip(name, "Codex has no SSE MCP transport");
                return;
            }
        }
        self.commit(name, overrides, env);
    }

    /// Restates one `[mcp_servers.<name>]` table from the reader's real home.
    fn add_user_table(&mut self, name: &str, table: toml::Table) {
        if !is_bare_key(name) {
            self.skip(name, "the server name is not a Codex config key");
            return;
        }
        let mut overrides = Vec::new();
        let mut env = BTreeMap::new();
        let mut forwarded: Vec<String> = Vec::new();
        for (key, value) in table {
            match (key.as_str(), value) {
                ("env", toml::Value::Table(variables)) => {
                    for (variable, value) in variables {
                        let Some(value) = value.as_str().map(str::to_string) else {
                            continue;
                        };
                        if let Err(reason) = self.forwardable(&variable, &value, &env) {
                            self.skip(name, &reason);
                            return;
                        }
                        env.insert(variable.clone(), value);
                        forwarded.push(variable);
                    }
                }
                ("env_vars", toml::Value::Array(names)) => {
                    forwarded.extend(names.iter().filter_map(|n| n.as_str().map(str::to_string)));
                }
                ("http_headers", toml::Value::Table(headers)) => {
                    for (header, value) in headers {
                        let Some(value) = value.as_str() else {
                            continue;
                        };
                        if !is_bare_key(&header) {
                            self.skip(name, "a header name is not a Codex config key");
                            return;
                        }
                        let variable = codex_env_name(name, &format!("HEADER_{header}"));
                        overrides
                            .push((format!("env_http_headers.{header}"), toml_string(&variable)));
                        env.insert(variable, value.to_string());
                    }
                }
                (_, value) => {
                    if !flatten_toml(&key, value, &mut overrides) {
                        self.skip(name, "a field cannot be restated as a Codex override");
                        return;
                    }
                }
            }
        }
        if !forwarded.is_empty() {
            forwarded.sort();
            forwarded.dedup();
            overrides.push(("env_vars".to_string(), toml_string_array(&forwarded)));
        }
        self.commit(name, overrides, env);
    }

    /// Whether `key=value` can join the Codex process environment.
    ///
    /// The variable is set on Codex itself, so it must not change a value
    /// Codex or another server already sees under that name.
    fn forwardable(
        &self,
        key: &str,
        value: &str,
        pending: &BTreeMap<String, String>,
    ) -> Result<(), String> {
        let differs = |existing: Option<String>| existing.is_some_and(|existing| existing != value);
        if differs(env_value(self.request.launch_env, key))
            || differs((self.request.host_env)(key))
            || differs(self.env.get(key).cloned())
            || differs(pending.get(key).cloned())
        {
            return Err(format!(
                "its environment variable {key} would change a value the Codex process already has"
            ));
        }
        Ok(())
    }

    fn commit(
        &mut self,
        name: &str,
        overrides: Vec<(String, String)>,
        env: BTreeMap<String, String>,
    ) {
        for (field, value) in overrides {
            self.launch.args.push("-c".to_string());
            self.launch
                .args
                .push(format!("mcp_servers.{name}.{field}={value}"));
        }
        self.env.extend(env);
        self.launch.delivered.push(name.to_string());
    }

    fn skip(&mut self, name: &str, reason: &str) {
        self.launch
            .skipped
            .push((name.to_string(), reason.to_string()));
    }
}

/// Spreads a TOML value into dotted overrides; inline tables are not a `-c`
/// value Codex parses as a table, but dotted keys are.
fn flatten_toml(path: &str, value: toml::Value, out: &mut Vec<(String, String)>) -> bool {
    if !path.split('.').all(is_bare_key) {
        return false;
    }
    match value {
        toml::Value::Table(table) => table
            .into_iter()
            .all(|(key, value)| flatten_toml(&format!("{path}.{key}"), value, out)),
        value => {
            out.push((path.to_string(), value.to_string()));
            true
        }
    }
}

fn codex_env_name(server: &str, suffix: &str) -> String {
    let normalize = |value: &str| {
        value
            .chars()
            .map(|character| {
                if character.is_ascii_alphanumeric() {
                    character.to_ascii_uppercase()
                } else {
                    '_'
                }
            })
            .collect::<String>()
    };
    format!("VIBEX_MCP_{}_{}", normalize(server), normalize(suffix))
}

fn bearer_token<'v>(header: &str, value: &'v str) -> Option<&'v str> {
    if !header.eq_ignore_ascii_case("authorization") {
        return None;
    }
    let (scheme, token) = value.trim().split_once(' ')?;
    scheme
        .eq_ignore_ascii_case("bearer")
        .then_some(token.trim())
        .filter(|token| !token.is_empty())
}

fn is_bare_key(value: &str) -> bool {
    !value.is_empty()
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || matches!(character, '-' | '_'))
}

fn toml_string(value: &str) -> String {
    toml::Value::String(value.to_string()).to_string()
}

fn toml_string_array(values: &[String]) -> String {
    toml::Value::Array(
        values
            .iter()
            .map(|value| toml::Value::String(value.clone()))
            .collect(),
    )
    .to_string()
}

// ---------------------------------------------------------------------------
// Native entry spellings
// ---------------------------------------------------------------------------

fn string_map(entries: &[(String, String)]) -> Map<String, Value> {
    entries
        .iter()
        .map(|(key, value)| (key.clone(), Value::String(value.clone())))
        .collect()
}

/// Inserts `key` only when the map has entries, so files stay minimal.
fn insert_map(entry: &mut Map<String, Value>, key: &str, entries: &[(String, String)]) {
    if !entries.is_empty() {
        entry.insert(key.to_string(), Value::Object(string_map(entries)));
    }
}

fn stdio_entry(
    command: &str,
    args: &[String],
    env: &[(String, String)],
    kind: Option<&str>,
) -> Map<String, Value> {
    let mut entry = Map::new();
    if let Some(kind) = kind {
        entry.insert("type".to_string(), json!(kind));
    }
    entry.insert("command".to_string(), json!(command));
    entry.insert("args".to_string(), json!(args));
    insert_map(&mut entry, "env", env);
    entry
}

fn remote_entry(
    url_key: &str,
    url: &str,
    headers: &[(String, String)],
    kind: Option<&str>,
) -> Map<String, Value> {
    let mut entry = Map::new();
    if let Some(kind) = kind {
        entry.insert("type".to_string(), json!(kind));
    }
    entry.insert(url_key.to_string(), json!(url));
    insert_map(&mut entry, "headers", headers);
    entry
}

fn claude_entry(transport: &TerminalMcpTransport) -> Value {
    Value::Object(match transport {
        TerminalMcpTransport::Stdio { command, args, env } => {
            stdio_entry(command, args, env, Some("stdio"))
        }
        TerminalMcpTransport::Http { url, headers } => {
            remote_entry("url", url, headers, Some("http"))
        }
        TerminalMcpTransport::Sse { url, headers } => {
            remote_entry("url", url, headers, Some("sse"))
        }
    })
}

fn copilot_entry(transport: &TerminalMcpTransport) -> Value {
    let mut entry = match transport {
        TerminalMcpTransport::Stdio { command, args, env } => {
            stdio_entry(command, args, env, Some("local"))
        }
        TerminalMcpTransport::Http { url, headers } => {
            remote_entry("url", url, headers, Some("http"))
        }
        TerminalMcpTransport::Sse { url, headers } => {
            remote_entry("url", url, headers, Some("sse"))
        }
    };
    // Copilot exposes no tool of a server whose entry omits the allowlist.
    entry.insert("tools".to_string(), json!(["*"]));
    Value::Object(entry)
}

fn cline_entry(transport: &TerminalMcpTransport) -> Value {
    Value::Object(match transport {
        TerminalMcpTransport::Stdio { command, args, env } => {
            stdio_entry(command, args, env, Some("stdio"))
        }
        TerminalMcpTransport::Http { url, headers } => {
            remote_entry("url", url, headers, Some("streamableHttp"))
        }
        TerminalMcpTransport::Sse { url, headers } => {
            remote_entry("url", url, headers, Some("sse"))
        }
    })
}

fn hermes_entry(transport: &TerminalMcpTransport) -> Value {
    Value::Object(match transport {
        TerminalMcpTransport::Stdio { command, args, env } => stdio_entry(command, args, env, None),
        TerminalMcpTransport::Http { url, headers } => remote_entry("url", url, headers, None),
        TerminalMcpTransport::Sse { url, headers } => {
            let mut entry = remote_entry("url", url, headers, None);
            entry.insert("transport".to_string(), json!("sse"));
            entry
        }
    })
}

fn opencode_entry(transport: &TerminalMcpTransport) -> Value {
    let mut entry = Map::new();
    match transport {
        TerminalMcpTransport::Stdio { command, args, env } => {
            entry.insert("type".to_string(), json!("local"));
            let mut argv = vec![command.clone()];
            argv.extend(args.iter().cloned());
            entry.insert("command".to_string(), json!(argv));
            insert_map(&mut entry, "environment", env);
        }
        // OpenCode's remote client tries streamable HTTP and falls back to SSE.
        TerminalMcpTransport::Http { url, headers }
        | TerminalMcpTransport::Sse { url, headers } => {
            entry.insert("type".to_string(), json!("remote"));
            entry.insert("url".to_string(), json!(url));
            insert_map(&mut entry, "headers", headers);
        }
    }
    entry.insert("enabled".to_string(), json!(true));
    Value::Object(entry)
}

// ---------------------------------------------------------------------------
// The reader's real home
// ---------------------------------------------------------------------------

const OPENCODE_CONFIG_CONTENT: &str = "OPENCODE_CONFIG_CONTENT";
const CLINE_MCP_SETTINGS_ENV: &str = "CLINE_MCP_SETTINGS_PATH";

/// Where an Agent keeps its MCP servers below a home a Profile can relocate.
struct HomeSurface {
    env_key: &'static str,
    default_relative: &'static str,
    file: &'static str,
}

const CODEX_HOME: HomeSurface = HomeSurface {
    env_key: "CODEX_HOME",
    default_relative: ".codex",
    file: "config.toml",
};
const COPILOT_HOME: HomeSurface = HomeSurface {
    env_key: "COPILOT_HOME",
    default_relative: ".copilot",
    file: "mcp-config.json",
};
const HERMES_HOME: HomeSurface = HomeSurface {
    env_key: "HERMES_HOME",
    default_relative: ".hermes",
    file: "config.yaml",
};
const CLINE_DATA_DIR: HomeSurface = HomeSurface {
    env_key: "CLINE_DATA_DIR",
    default_relative: ".cline/data",
    file: "settings/cline_mcp_settings.json",
};

/// The reader's own MCP file, when the launch points the Agent elsewhere.
///
/// `None` when the launch keeps the reader's home: the CLI reads that file
/// itself, and restating it would register every server twice.
fn relocated_home_file(request: &TerminalMcpRequest<'_>, surface: &HomeSurface) -> Option<PathBuf> {
    let launched = env_value(request.launch_env, surface.env_key)?;
    let real = (request.host_env)(surface.env_key)
        .filter(|path| !path.trim().is_empty())
        .map(PathBuf::from)
        .or_else(|| {
            request
                .home_dir
                .map(|home| home.join(surface.default_relative))
        })?;
    if Path::new(&launched) == real {
        return None;
    }
    Some(real.join(surface.file))
}

fn env_value(env: &[(String, String)], key: &str) -> Option<String> {
    env.iter()
        .rev()
        .find(|(candidate, _)| candidate == key)
        .map(|(_, value)| value.clone())
}

fn json_servers_in_file(
    path: &Path,
    container: &str,
    launch: &mut TerminalMcpLaunch,
) -> Vec<(String, Value)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    if text.trim().is_empty() {
        return Vec::new();
    }
    match serde_json::from_str::<Value>(&text) {
        Ok(Value::Object(mut document)) => match document.remove(container) {
            Some(Value::Object(servers)) => servers.into_iter().collect(),
            _ => Vec::new(),
        },
        _ => {
            launch.skipped.push((
                container.to_string(),
                "the reader's own MCP file could not be parsed".to_string(),
            ));
            Vec::new()
        }
    }
}

fn yaml_servers_in_file(
    path: &Path,
    container: &str,
    launch: &mut TerminalMcpLaunch,
) -> Vec<(String, serde_yaml::Value)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    match serde_yaml::from_str::<serde_yaml::Value>(&text) {
        Ok(serde_yaml::Value::Mapping(mut document)) => {
            match document.remove(serde_yaml::Value::String(container.to_string())) {
                Some(serde_yaml::Value::Mapping(servers)) => servers
                    .into_iter()
                    .filter_map(|(name, entry)| Some((name.as_str()?.to_string(), entry)))
                    .collect(),
                _ => Vec::new(),
            }
        }
        Ok(serde_yaml::Value::Null) => Vec::new(),
        _ => {
            launch.skipped.push((
                container.to_string(),
                "the reader's own MCP file could not be parsed".to_string(),
            ));
            Vec::new()
        }
    }
}

fn toml_servers_in_file(path: &Path, launch: &mut TerminalMcpLaunch) -> Vec<(String, toml::Table)> {
    let Ok(text) = fs::read_to_string(path) else {
        return Vec::new();
    };
    match text.parse::<toml::Table>() {
        Ok(mut document) => match document.remove("mcp_servers") {
            Some(toml::Value::Table(servers)) => servers
                .into_iter()
                .filter_map(|(name, entry)| match entry {
                    toml::Value::Table(table) => Some((name, table)),
                    _ => None,
                })
                .collect(),
            _ => Vec::new(),
        },
        Err(_) => {
            launch.skipped.push((
                "mcp_servers".to_string(),
                "the reader's own MCP file could not be parsed".to_string(),
            ));
            Vec::new()
        }
    }
}

// ---------------------------------------------------------------------------
// Private files
// ---------------------------------------------------------------------------

fn write_private_json(path: &Path, value: &Value) -> VibexResult<()> {
    let mut content = serde_json::to_vec_pretty(value).map_err(|error| {
        VibexError::storage(
            "terminal_mcp_encode_failed",
            "the MCP configuration could not be encoded",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    content.push(b'\n');
    write_private_file(path, &content)
}

/// Writes `content` atomically with mode 0600 in a 0700 directory.
fn write_private_file(path: &Path, content: &[u8]) -> VibexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        VibexError::validation(
            "terminal_mcp_path_invalid",
            "the terminal MCP file has no parent directory",
        )
    })?;
    create_private_dir(parent)?;
    let temp = parent.join(format!(".terminal-mcp-{}.tmp", RequestId::new().as_str()));
    let mut options = OpenOptions::new();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let result = options
        .open(&temp)
        .and_then(|mut file| file.write_all(content).and_then(|_| file.sync_all()))
        .and_then(|_| fs::rename(&temp, path));
    if let Err(error) = result {
        let _ = fs::remove_file(&temp);
        return Err(VibexError::storage(
            "terminal_mcp_write_failed",
            "the terminal MCP configuration could not be written",
        )
        .with_diagnostic("error", error.to_string()));
    }
    Ok(())
}

fn create_private_dir(path: &Path) -> VibexResult<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::DirBuilderExt;
        builder.mode(0o700);
    }
    builder.create(path).map_err(|error| {
        VibexError::storage(
            "terminal_mcp_dir_failed",
            "the terminal MCP directory could not be created",
        )
        .with_diagnostic("error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    struct Fixture {
        temp: tempfile::TempDir,
        host: BTreeMap<String, String>,
        launch_env: Vec<(String, String)>,
    }

    impl Fixture {
        fn new() -> Self {
            Self {
                temp: tempfile::tempdir().unwrap(),
                host: BTreeMap::new(),
                launch_env: Vec::new(),
            }
        }

        fn home(&self) -> PathBuf {
            self.temp.path().join("home")
        }

        fn session_dir(&self) -> PathBuf {
            self.temp.path().join("session")
        }

        fn write(&self, relative: &str, content: &str) -> PathBuf {
            let path = self.temp.path().join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(&path, content).unwrap();
            path
        }

        fn prepare(&self, agent_id: &str, servers: &[TerminalMcpServer]) -> TerminalMcpLaunch {
            let host = self.host.clone();
            let lookup = move |key: &str| host.get(key).cloned();
            let home = self.home();
            let session_dir = self.session_dir();
            prepare_terminal_mcp_launch(&TerminalMcpRequest {
                agent_id,
                servers,
                session_dir: &session_dir,
                launch_env: &self.launch_env,
                host_env: &lookup,
                home_dir: Some(&home),
                system_root: self.temp.path(),
            })
            .unwrap()
        }
    }

    fn browser() -> TerminalMcpServer {
        TerminalMcpServer {
            name: "Embedded-browser".to_string(),
            transport: TerminalMcpTransport::Http {
                url: "http://127.0.0.1:4000/mcp/browser".to_string(),
                headers: vec![(
                    "Authorization".to_string(),
                    "Bearer session-token".to_string(),
                )],
            },
        }
    }

    fn delegation() -> TerminalMcpServer {
        TerminalMcpServer {
            name: "Agent-delegation".to_string(),
            transport: TerminalMcpTransport::Stdio {
                command: "/opt/vibex/vibex".to_string(),
                args: vec!["--agent-delegation-mcp".to_string()],
                env: vec![(
                    "VIBEX_AGENT_DELEGATION_TOKEN".to_string(),
                    "secret".to_string(),
                )],
            },
        }
    }

    fn read_json(path: &Path) -> Value {
        serde_json::from_str(&fs::read_to_string(path).unwrap()).unwrap()
    }

    fn env_of<'l>(launch: &'l TerminalMcpLaunch, key: &str) -> Option<&'l str> {
        launch
            .env
            .iter()
            .find(|(candidate, _)| candidate == key)
            .map(|(_, value)| value.as_str())
    }

    #[test]
    fn nothing_to_deliver_changes_nothing() {
        let fixture = Fixture::new();
        for agent in ["claude", "codex", "opencode", "hermes", "cline", "copilot"] {
            let launch = fixture.prepare(agent, &[]);
            assert!(launch.args.is_empty(), "{agent}");
            assert!(launch.env.is_empty(), "{agent}");
        }
        assert!(!fixture.session_dir().exists());
    }

    #[test]
    fn claude_loads_a_private_file_beside_its_own() {
        let fixture = Fixture::new();
        let launch = fixture.prepare("claude", &[browser(), delegation()]);
        let path = fixture.session_dir().join("mcp-config.json");
        assert_eq!(
            launch.args,
            vec![format!("--mcp-config={}", path.to_string_lossy())]
        );
        let document = read_json(&path);
        assert_eq!(document["mcpServers"]["Embedded-browser"]["type"], "http");
        assert_eq!(
            document["mcpServers"]["Embedded-browser"]["headers"]["Authorization"],
            "Bearer session-token"
        );
        assert_eq!(
            document["mcpServers"]["Agent-delegation"]["env"]["VIBEX_AGENT_DELEGATION_TOKEN"],
            "secret"
        );
        assert!(
            launch.args.iter().all(|arg| !arg.contains("secret")),
            "credentials stay out of the argv"
        );
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = fs::metadata(&path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o600);
        }
    }

    #[test]
    fn codex_names_environment_variables_instead_of_credentials() {
        let fixture = Fixture::new();
        let launch = fixture.prepare("codex", &[browser(), delegation()]);
        assert!(launch.args.iter().all(|arg| !arg.contains("secret")));
        assert!(launch.args.iter().all(|arg| !arg.contains("session-token")));
        assert!(launch.args.contains(
            &"mcp_servers.Embedded-browser.url=\"http://127.0.0.1:4000/mcp/browser\"".to_string()
        ));
        assert!(launch.args.contains(
            &"mcp_servers.Embedded-browser.bearer_token_env_var=\"VIBEX_MCP_EMBEDDED_BROWSER_BEARER_TOKEN\""
                .to_string()
        ));
        assert!(launch.args.contains(
            &"mcp_servers.Agent-delegation.env_vars=[\"VIBEX_AGENT_DELEGATION_TOKEN\"]".to_string()
        ));
        assert_eq!(
            env_of(&launch, "VIBEX_MCP_EMBEDDED_BROWSER_BEARER_TOKEN"),
            Some("session-token")
        );
        assert_eq!(
            env_of(&launch, "VIBEX_AGENT_DELEGATION_TOKEN"),
            Some("secret")
        );
        assert!(launch.args.chunks(2).all(|pair| pair[0] == "-c"));
    }

    #[test]
    fn codex_refuses_to_change_its_own_environment() {
        let mut fixture = Fixture::new();
        fixture
            .host
            .insert("PATH".to_string(), "/usr/bin".to_string());
        let server = TerminalMcpServer {
            name: "tool".to_string(),
            transport: TerminalMcpTransport::Stdio {
                command: "tool".to_string(),
                args: Vec::new(),
                env: vec![("PATH".to_string(), "/opt/tool".to_string())],
            },
        };
        let launch = fixture.prepare("codex", &[server]);
        assert!(launch.args.is_empty());
        assert_eq!(launch.skipped.len(), 1);
    }

    #[test]
    fn codex_restates_the_readers_servers_when_a_profile_moves_its_home() {
        let mut fixture = Fixture::new();
        fixture.write(
            "home/.codex/config.toml",
            "model = \"x\"\n[mcp_servers.grafana]\ncommand = \"npx\"\nargs = [\"-y\", \"grafana-mcp\"]\n[mcp_servers.grafana.env]\nGRAFANA_TOKEN = \"t0k\"\n[mcp_servers.docs]\nurl = \"https://docs.example/mcp\"\n[mcp_servers.docs.http_headers]\nX-Api-Key = \"k3y\"\n",
        );
        fixture.launch_env.push((
            "CODEX_HOME".to_string(),
            fixture
                .temp
                .path()
                .join("overlay")
                .to_string_lossy()
                .into_owned(),
        ));
        let launch = fixture.prepare("codex", &[browser()]);
        assert!(
            launch
                .args
                .contains(&"mcp_servers.grafana.command=\"npx\"".to_string())
        );
        assert!(
            launch
                .args
                .contains(&"mcp_servers.grafana.env_vars=[\"GRAFANA_TOKEN\"]".to_string())
        );
        assert!(
            launch.args.contains(
                &"mcp_servers.docs.env_http_headers.X-Api-Key=\"VIBEX_MCP_DOCS_HEADER_X_API_KEY\""
                    .to_string()
            )
        );
        assert!(
            launch
                .args
                .iter()
                .all(|arg| !arg.contains("t0k") && !arg.contains("k3y"))
        );
        assert_eq!(env_of(&launch, "GRAFANA_TOKEN"), Some("t0k"));
        assert!(launch.delivered.contains(&"grafana".to_string()));
        assert!(launch.delivered.contains(&"docs".to_string()));
    }

    #[test]
    fn codex_leaves_an_unmoved_home_to_codex() {
        let fixture = Fixture::new();
        fixture.write(
            "home/.codex/config.toml",
            "[mcp_servers.grafana]\ncommand = \"npx\"\n",
        );
        let launch = fixture.prepare("codex", &[browser()]);
        assert!(launch.args.iter().all(|arg| !arg.contains("grafana")));
    }

    #[test]
    fn opencode_extends_the_projected_document() {
        let mut fixture = Fixture::new();
        fixture.launch_env.push((
            OPENCODE_CONFIG_CONTENT.to_string(),
            r#"{"model":"p/m","provider":{"p":{}}}"#.to_string(),
        ));
        let launch = fixture.prepare("opencode", &[browser(), delegation()]);
        let document: Value =
            serde_json::from_str(env_of(&launch, OPENCODE_CONFIG_CONTENT).unwrap()).unwrap();
        assert_eq!(document["model"], "p/m");
        assert_eq!(document["mcp"]["Embedded-browser"]["type"], "remote");
        assert_eq!(
            document["mcp"]["Agent-delegation"]["command"],
            json!(["/opt/vibex/vibex", "--agent-delegation-mcp"])
        );
        assert!(launch.args.is_empty());
    }

    #[test]
    fn hermes_adds_a_managed_layer_and_keeps_the_readers_servers() {
        let mut fixture = Fixture::new();
        fixture.write(
            "home/.hermes/config.yaml",
            "model: x\nmcp_servers:\n  github:\n    command: gh-mcp\n",
        );
        fixture.write("etc/hermes/config.yaml", "approvals:\n  mode: ask\n");
        fixture.write("etc/hermes/.env", "CORP=1\n");
        fixture.launch_env.push((
            "HERMES_HOME".to_string(),
            fixture
                .temp
                .path()
                .join("overlay")
                .to_string_lossy()
                .into_owned(),
        ));
        let launch = fixture.prepare("hermes", &[browser()]);
        let dir = PathBuf::from(env_of(&launch, "HERMES_MANAGED_DIR").unwrap());
        let document: serde_yaml::Value =
            serde_yaml::from_str(&fs::read_to_string(dir.join("config.yaml")).unwrap()).unwrap();
        assert_eq!(document["approvals"]["mode"].as_str(), Some("ask"));
        assert_eq!(
            document["mcp_servers"]["github"]["command"].as_str(),
            Some("gh-mcp")
        );
        assert_eq!(
            document["mcp_servers"]["Embedded-browser"]["url"].as_str(),
            Some("http://127.0.0.1:4000/mcp/browser")
        );
        assert_eq!(fs::read_to_string(dir.join(".env")).unwrap(), "CORP=1\n");
    }

    #[test]
    fn cline_restates_the_file_it_replaces() {
        let mut fixture = Fixture::new();
        fixture.write(
            "home/.cline/data/settings/cline_mcp_settings.json",
            r#"{"mcpServers":{"mine":{"type":"stdio","command":"mine"}}}"#,
        );
        fixture.write(
            "overlay/settings/cline_mcp_settings.json",
            r#"{"mcpServers":{"projected":{"type":"stdio","command":"p"}}}"#,
        );
        fixture.launch_env.push((
            "CLINE_DATA_DIR".to_string(),
            fixture
                .temp
                .path()
                .join("overlay")
                .to_string_lossy()
                .into_owned(),
        ));
        let launch = fixture.prepare("cline", &[browser()]);
        let path = PathBuf::from(env_of(&launch, CLINE_MCP_SETTINGS_ENV).unwrap());
        let document = read_json(&path);
        assert_eq!(document["mcpServers"]["mine"]["command"], "mine");
        assert_eq!(document["mcpServers"]["projected"]["command"], "p");
        assert_eq!(
            document["mcpServers"]["Embedded-browser"]["type"],
            "streamableHttp"
        );
    }

    #[test]
    fn copilot_augments_and_restates_a_moved_home() {
        let mut fixture = Fixture::new();
        fixture.write(
            "home/.copilot/mcp-config.json",
            r#"{"mcpServers":{"mine":{"type":"local","command":"mine","tools":["*"]}}}"#,
        );
        fixture.launch_env.push((
            "COPILOT_HOME".to_string(),
            fixture
                .temp
                .path()
                .join("overlay")
                .to_string_lossy()
                .into_owned(),
        ));
        let launch = fixture.prepare("copilot", &[delegation()]);
        assert_eq!(launch.args[0], "--additional-mcp-config");
        let path = PathBuf::from(launch.args[1].strip_prefix('@').unwrap());
        let document = read_json(&path);
        assert_eq!(document["mcpServers"]["mine"]["command"], "mine");
        assert_eq!(document["mcpServers"]["Agent-delegation"]["type"], "local");
        assert_eq!(
            document["mcpServers"]["Agent-delegation"]["tools"],
            json!(["*"])
        );
    }

    #[test]
    fn agents_without_a_channel_report_every_server() {
        let fixture = Fixture::new();
        for agent in ["cursor", "gemini", "grok", "kimi", "devin", "pi"] {
            let launch = fixture.prepare(agent, &[browser()]);
            assert!(launch.args.is_empty() && launch.env.is_empty(), "{agent}");
            assert_eq!(launch.skipped.len(), 1, "{agent}");
        }
    }

    #[test]
    fn server_names_match_the_acp_wire() {
        assert_eq!(
            terminal_mcp_server_name("Embedded browser", "vibex-browser"),
            "Embedded-browser"
        );
        assert_eq!(terminal_mcp_server_name("  ", "mcp_1"), "mcp_1");
        assert_eq!(terminal_mcp_server_name("中文", "!!"), "vibex-mcp");
    }
}

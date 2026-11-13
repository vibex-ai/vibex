//! The host modules a plugin reaches beyond its own panel with.
//!
//! Each one is the other half of a permission the manifest can declare:
//! `vibex.clipboard` for `clipboard`, `vibex.network` for `net.fetch`,
//! `vibex.fs` for `fs.read` / `fs.write`, `vibex.process` for
//! `process.execute`, and `vibex.agent.tools` for `agent.tool`.
//!
//! A module is only built for a plugin whose grants include its permission, and
//! every function checks the grant again. The scope — which hosts, which paths,
//! which programs — comes from the manifest, never from the call: a plugin
//! cannot widen its own reach by asking for something its manifest did not
//! declare.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use gpui_shell::{HostArguments, HostError, HostModule, HostResult, HostValue};
use vibex_core::PluginPermissionGrant;

use crate::plugin_scope::{executable_allowed, host_allowed, path_allowed};

/// What one plugin's manifest declared it may reach.
///
/// Read from the package's `vibex-plugin.json` at load time and held beside the
/// granted permission keys, so a scope cannot be forgotten between the consent
/// sheet and the call.
#[derive(Debug, Clone, Default)]
pub struct PluginScope {
    /// The host patterns `net.fetch` covers.
    pub hosts: Vec<String>,
    /// The roots `fs.read` covers.
    pub read_roots: Vec<String>,
    /// The roots `fs.write` covers.
    pub write_roots: Vec<String>,
    /// The executables `process.execute` covers.
    pub executables: Vec<String>,
}

impl PluginScope {
    /// Reads the scope a package's manifest declares.
    ///
    /// A manifest that cannot be read yields an empty scope, which admits
    /// nothing: the failure is closed, not open.
    pub fn from_directory(directory: &Path) -> Self {
        let path = directory.join(vibex_plugin::manifest::AUTHOR_MANIFEST_FILE);
        let Ok(manifest) = vibex_plugin::manifest::PluginManifest::from_file(&path) else {
            return Self::default();
        };
        let capabilities = &manifest.capabilities;
        let mut scope = Self::default();
        if let Some(network) = &capabilities.network {
            scope.hosts = network.hosts.clone();
            // An HTTP rule names a host too; a manifest that only wrote rules
            // still gets the hosts those rules are about.
            for rule in &network.http {
                if !scope.hosts.contains(&rule.host) {
                    scope.hosts.push(rule.host.clone());
                }
            }
        }
        if let Some(fs) = &capabilities.fs {
            scope.read_roots = fs.read.clone();
            scope.write_roots = fs.write.clone();
        }
        if let Some(process) = &capabilities.process {
            scope.executables = process.executables.clone();
        }
        scope
    }

    /// The workspace and home a `${workspace}` / `${home}` root resolves against.
    fn roots(
        &self,
        workspace: Option<&Path>,
        home: Option<&Path>,
    ) -> (Option<PathBuf>, Option<PathBuf>) {
        (
            workspace.map(Path::to_path_buf),
            home.map(Path::to_path_buf),
        )
    }
}

/// The largest file a plugin may read or write in one call.
///
/// The bound is the same order as the package limit: a plugin that needs to
/// stream something larger should be a native panel, not a script holding the
/// UI thread for a copy.
pub const MAX_PLUGIN_FILE_BYTES: u64 = 8 * 1024 * 1024;
/// The largest response body a plugin may read.
pub const MAX_PLUGIN_RESPONSE_BYTES: u64 = 8 * 1024 * 1024;
/// The longest a plugin's process may run.
pub const MAX_PLUGIN_PROCESS_MILLIS: u64 = 30_000;
/// The largest tool result a plugin may hand back.
///
/// A tool result goes into the Agent's context, so an unbounded one would spend
/// the conversation's budget on a single call.
pub const MAX_PLUGIN_TOOL_REPLY_BYTES: usize = 256 * 1024;

/// The clipboard, as text.
///
/// Images are deliberately absent: a script that could read the clipboard's
/// image would get a handle it cannot render without a component the catalog
/// has to provide, and writing one is not a shape `HostValue` carries.
pub fn clipboard_module(plugin_id: String, grants: Vec<String>) -> HostModule {
    let read = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        move |_: &HostArguments| {
            require(&grants, "clipboard", &plugin_id)?;
            let text = gpui_shell::with_current_app(|cx| {
                cx.read_from_clipboard()
                    .map(|item| item.text())
                    .unwrap_or_default()
            })
            .unwrap_or_default();
            Ok(HostValue::from(text))
        }
    };
    let write = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        move |arguments: &HostArguments| {
            require(&grants, "clipboard", &plugin_id)?;
            let text = arguments.string(0)?.to_string();
            let item = gpui::ClipboardItem::new_string(text);
            let wrote = gpui_shell::with_current_app(|cx| {
                cx.write_to_clipboard(item);
            })
            .is_some();
            Ok(HostValue::Bool(wrote))
        }
    };
    HostModule::new("vibex.clipboard")
        .declarations(
            "export function readText(): string;\nexport function writeText(text: string): boolean;",
        )
        .function("readText", read)
        .function("writeText", write)
}

/// HTTP requests, limited to the hosts the manifest named.
pub fn network_module(
    plugin_id: String,
    grants: Vec<String>,
    scope: Arc<PluginScope>,
) -> HostModule {
    let fetch = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        move |arguments: &HostArguments| {
            require(&grants, "net.fetch", &plugin_id)?;
            let url = arguments.string(0)?.trim().to_string();
            let parsed = url::Url::parse(&url)
                .map_err(|error| HostError::new(format!("`{url}` is not a URL: {error}")))?;
            // Only https: a plugin's network reach is not a reason to allow
            // plaintext, and a loopback address would reach this machine.
            if parsed.scheme() != "https" {
                return Err(HostError::new(
                    "a plugin may only fetch https addresses".to_string(),
                ));
            }
            let Some(host) = parsed.host_str() else {
                return Err(HostError::new("the address has no host".to_string()));
            };
            if !host_allowed(&scope.hosts, host) {
                return Err(HostError::new(format!(
                    "plugin `{plugin_id}` may not reach `{host}`: its manifest names {:?}",
                    scope.hosts
                )));
            }
            if let Some(address) = parsed.host() {
                if let url::Host::Ipv4(ip) = address
                    && !vibex_plugin::market::fetch::is_public_ip(std::net::IpAddr::V4(ip))
                {
                    return Err(HostError::new(
                        "a plugin may not fetch a private address".to_string(),
                    ));
                }
                if let url::Host::Ipv6(ip) = address
                    && !vibex_plugin::market::fetch::is_public_ip(std::net::IpAddr::V6(ip))
                {
                    return Err(HostError::new(
                        "a plugin may not fetch a private address".to_string(),
                    ));
                }
            }
            Ok(Box::pin(async move {
                let client = reqwest::Client::builder()
                    .redirect(reqwest::redirect::Policy::none())
                    .user_agent(concat!("vibex-plugin/", env!("CARGO_PKG_VERSION")))
                    .build()
                    .map_err(|error| HostError::new(format!("the request failed: {error}")))?;
                let response = client
                    .get(parsed)
                    .send()
                    .await
                    .map_err(|error| HostError::new(format!("the request failed: {error}")))?;
                let status = response.status().as_u16();
                let body = response.bytes().await.map_err(|error| {
                    HostError::new(format!("the body could not be read: {error}"))
                })?;
                if body.len() as u64 > MAX_PLUGIN_RESPONSE_BYTES {
                    return Err(HostError::new(
                        "the response is larger than a plugin may read".to_string(),
                    ));
                }
                Ok(HostValue::Object(vec![
                    ("status".to_string(), HostValue::from(f64::from(status))),
                    (
                        "body".to_string(),
                        HostValue::from(String::from_utf8_lossy(&body).to_string()),
                    ),
                ]))
            }))
        }
    };
    HostModule::new("vibex.network")
        .declarations(
            "export function fetch(url: string): Promise<{ status: number; body: string }>;",
        )
        .async_function("fetch", fetch)
}

/// Files, limited to the roots the manifest named.
pub fn fs_module(
    plugin_id: String,
    grants: Vec<String>,
    scope: Arc<PluginScope>,
    workspace: Option<PathBuf>,
    home: Option<PathBuf>,
) -> HostModule {
    let (read_workspace, read_home) = scope.roots(workspace.as_deref(), home.as_deref());
    let (write_workspace, write_home) = scope.roots(workspace.as_deref(), home.as_deref());
    let read_roots = (read_workspace.clone(), read_home.clone());
    let list_roots = (read_workspace, read_home);
    let write_roots = (write_workspace, write_home);

    let read = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let scope = Arc::clone(&scope);
        move |arguments: &HostArguments| {
            require(&grants, "fs.read", &plugin_id)?;
            let path = arguments.string(0)?.to_string();
            let path = PathBuf::from(&path);
            if !path_allowed(
                &scope.read_roots,
                &path,
                read_roots.0.as_deref(),
                read_roots.1.as_deref(),
            ) {
                return Err(HostError::new(format!(
                    "plugin `{plugin_id}` may not read `{}`: it is outside {:?}",
                    path.display(),
                    scope.read_roots
                )));
            }
            let metadata = std::fs::metadata(&path).map_err(|error| {
                HostError::new(format!("`{}` could not be read: {error}", path.display()))
            })?;
            if metadata.len() > MAX_PLUGIN_FILE_BYTES {
                return Err(HostError::new(
                    "the file is larger than a plugin may read".to_string(),
                ));
            }
            let bytes = std::fs::read(&path).map_err(|error| {
                HostError::new(format!("`{}` could not be read: {error}", path.display()))
            })?;
            Ok(HostValue::from(String::from_utf8_lossy(&bytes).to_string()))
        }
    };

    let write = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let scope = Arc::clone(&scope);
        move |arguments: &HostArguments| {
            require(&grants, "fs.write", &plugin_id)?;
            let path = arguments.string(0)?.to_string();
            let text = arguments.string(1)?.to_string();
            if text.len() as u64 > MAX_PLUGIN_FILE_BYTES {
                return Err(HostError::new(
                    "the text is larger than a plugin may write".to_string(),
                ));
            }
            let path = PathBuf::from(&path);
            if !path_allowed(
                &scope.write_roots,
                &path,
                write_roots.0.as_deref(),
                write_roots.1.as_deref(),
            ) {
                return Err(HostError::new(format!(
                    "plugin `{plugin_id}` may not write `{}`: it is outside {:?}",
                    path.display(),
                    scope.write_roots
                )));
            }
            if let Some(parent) = path.parent() {
                std::fs::create_dir_all(parent).map_err(|error| {
                    HostError::new(format!(
                        "`{}` could not be created: {error}",
                        parent.display()
                    ))
                })?;
            }
            std::fs::write(&path, text.as_bytes()).map_err(|error| {
                HostError::new(format!(
                    "`{}` could not be written: {error}",
                    path.display()
                ))
            })?;
            Ok(HostValue::Bool(true))
        }
    };

    let list = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let scope = Arc::clone(&scope);
        move |arguments: &HostArguments| {
            require(&grants, "fs.read", &plugin_id)?;
            let path = arguments.string(0)?.to_string();
            let path = PathBuf::from(&path);
            if !path_allowed(
                &scope.read_roots,
                &path,
                list_roots.0.as_deref(),
                list_roots.1.as_deref(),
            ) {
                return Err(HostError::new(format!(
                    "plugin `{plugin_id}` may not list `{}`",
                    path.display()
                )));
            }
            let entries = std::fs::read_dir(&path).map_err(|error| {
                HostError::new(format!("`{}` could not be listed: {error}", path.display()))
            })?;
            let mut names = Vec::new();
            for entry in entries.flatten() {
                if let Some(name) = entry.file_name().to_str() {
                    names.push(HostValue::from(name.to_string()));
                }
            }
            names.sort_by(|left, right| format!("{left:?}").cmp(&format!("{right:?}")));
            Ok(HostValue::Array(names))
        }
    };

    HostModule::new("vibex.fs")
        .declarations(
            "export function readText(path: string): string;\nexport function writeText(path: string, text: string): boolean;\nexport function list(path: string): string[];",
        )
        .function("readText", read)
        .function("writeText", write)
        .function("list", list)
}

/// Programs, limited to the executables the manifest named.
pub fn process_module(
    plugin_id: String,
    grants: Vec<String>,
    scope: Arc<PluginScope>,
) -> HostModule {
    let run = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let executables = scope.executables.clone();
        move |arguments: &HostArguments| {
            require(&grants, "process.execute", &plugin_id)?;
            let program = arguments.string(0)?.trim().to_string();
            if !executable_allowed(&executables, &program) {
                return Err(HostError::new(format!(
                    "plugin `{plugin_id}` may not run `{program}`: its manifest names {executables:?}"
                )));
            }
            let mut argv: Vec<String> = Vec::new();
            if let Ok(HostValue::Array(items)) = arguments.value(1) {
                for item in items {
                    match item {
                        HostValue::Str(value) => argv.push(value.clone()),
                        other => {
                            return Err(HostError::new(format!(
                                "an argument must be a string, not {other:?}"
                            )));
                        }
                    }
                }
            }
            // The future is `'static`, so the program and its arguments are
            // owned values by this point.
            Ok(Box::pin(async move {
                let output = tokio::time::timeout(
                    std::time::Duration::from_millis(MAX_PLUGIN_PROCESS_MILLIS),
                    tokio::process::Command::new(&program).args(&argv).output(),
                )
                .await
                .map_err(|_| HostError::new("the program did not finish in time".to_string()))?
                .map_err(|error| {
                    HostError::new(format!("`{program}` could not be run: {error}"))
                })?;
                Ok(HostValue::Object(vec![
                    (
                        "status".to_string(),
                        HostValue::from(output.status.code().map(f64::from).unwrap_or(-1.0)),
                    ),
                    (
                        "stdout".to_string(),
                        HostValue::from(String::from_utf8_lossy(&output.stdout).to_string()),
                    ),
                    (
                        "stderr".to_string(),
                        HostValue::from(String::from_utf8_lossy(&output.stderr).to_string()),
                    ),
                ]))
            }))
        }
    };
    HostModule::new("vibex.process")
        .declarations(
            "export function run(program: string, args?: string[]): Promise<{ status: number; stdout: string; stderr: string }>;",
        )
        .async_function("run", run)
}

/// The tools a plugin contributes to the Agent.
///
/// The queue shape is the same one `vibex.commands` uses: `gpui-shell` has no
/// function value, so the host cannot call back into the script. A plugin
/// registers a tool with a name and a description, the workbench offers it to
/// the Agent, and an invocation lands in the plugin's queue for the script to
/// take and answer.
pub(crate) fn agent_tools_module(
    plugin_id: String,
    grants: Vec<String>,
    channels: crate::plugin_host::PluginChannels,
) -> HostModule {
    let register = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let channels = channels.clone();
        move |arguments: &HostArguments| {
            require(&grants, "agent.tool", &plugin_id)?;
            let name = arguments.string(0)?.trim().to_string();
            let description = arguments.string(1)?.trim().to_string();
            if name.is_empty() || description.is_empty() {
                return Err(HostError::new("a tool needs a name and a description"));
            }
            if name.len() > 64 || description.len() > 512 {
                return Err(HostError::new("the tool name or description is too long"));
            }
            Ok(HostValue::Bool(channels.register_tool(
                &plugin_id,
                &name,
                &description,
            )))
        }
    };
    let take = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let channels = channels.clone();
        move |_: &HostArguments| {
            require(&grants, "agent.tool", &plugin_id)?;
            Ok(HostValue::Array(
                channels
                    .take_tool_calls(&plugin_id)
                    .into_iter()
                    .map(|call| {
                        HostValue::Object(vec![
                            ("id".to_string(), HostValue::from(call.id)),
                            ("name".to_string(), HostValue::from(call.name)),
                            ("arguments".to_string(), HostValue::from(call.arguments)),
                        ])
                    })
                    .collect(),
            ))
        }
    };
    // The answer. Without it a call would sit in the queue for ever and the
    // Agent would wait on a promise nothing resolves.
    let respond = {
        let plugin_id = plugin_id.clone();
        let grants = grants.clone();
        let channels = channels.clone();
        move |arguments: &HostArguments| {
            require(&grants, "agent.tool", &plugin_id)?;
            let id = arguments.string(0)?.trim().to_string();
            let text = arguments.string(1)?.to_string();
            if id.is_empty() {
                return Err(HostError::new("a reply needs the call id"));
            }
            if text.len() > MAX_PLUGIN_TOOL_REPLY_BYTES {
                return Err(HostError::new(
                    "the reply is larger than an Agent tool result may be".to_string(),
                ));
            }
            channels.reply_tool_call(&plugin_id, &id, &text);
            // The MCP endpoint polls the bridge from a Tokio worker, so the
            // answer has to be published there as well as recorded here.
            if let Some(Some(bridge)) =
                gpui_shell::with_current_app(|cx| crate::plugin_host::tool_bridge(cx))
            {
                bridge.publish_reply(&plugin_id, &id, &text);
            }
            Ok(HostValue::Bool(true))
        }
    };
    HostModule::new("vibex.agent.tools")
        .declarations(
            "export function register(name: string, description: string): boolean;\nexport function take(): { id: string; name: string; arguments: string }[];\nexport function respond(id: string, text: string): boolean;",
        )
        .function("register", register)
        .function("take", take)
        .function("respond", respond)
}

/// Refuses a call the plugin was not granted.
fn require(grants: &[String], permission: &str, plugin_id: &str) -> Result<(), HostError> {
    if grants.iter().any(|grant| grant == permission) {
        Ok(())
    } else {
        Err(HostError::new(format!(
            "plugin `{plugin_id}` has not been granted `{permission}`"
        )))
    }
}

/// The granted keys of a set of grants, for the modules above.
pub fn granted_keys(grants: &[PluginPermissionGrant]) -> Vec<String> {
    grants.iter().map(|grant| grant.key.clone()).collect()
}

/// A `HostResult` alias so the module bodies read alike.
pub type PluginHostResult = HostResult;

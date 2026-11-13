//! The scripting host: the only place a plugin's JavaScript runs.
//!
//! `gpui-shell` hands a plugin a window, an `Entity<ScriptView>` and a policy
//! built from its manifest. Both the manager and the runtime are `Rc`, so
//! everything here happens on the UI thread — which is also why this module is
//! the desktop's, not the shared runtime crate's.
//!
//! The market and installer live in `vibex-plugin` and know nothing about this
//! module: they call [`vibex_plugin::PluginRuntime`], which is `Send + Sync`,
//! and this module answers by posting the work to a pump running on the UI
//! thread and waiting for its verdict.
//!
//! # Why a pump instead of a direct call
//!
//! Installing a plugin is an async operation the backend performs off the UI
//! thread, so [`PluginRuntime::load`] is reached from a Tokio worker while the
//! UI thread is awaiting that very task. Loading inline is impossible — there
//! is no `&mut Window` on that thread — and spawning without waiting would
//! report an install as successful before the script had even been parsed.
//! The pump closes both gaps: the worker blocks on a bounded reply channel
//! while the UI thread, which is free because it is awaiting, runs the load
//! between two frames.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use gpui::{App, AsyncWindowContext, Entity, Global, Window};
use gpui_shell::plugin::PluginManager;
use gpui_shell::{
    FrozenComponentRegistry, HostError, HostModule, HostValue, ScriptView, ShellRuntime,
};
use vibex_plugin::{LoadedPlugin, PluginError, PluginManifest, PluginRuntime};

/// The application id the host stores its own state under.
const BUNDLE_ID: &str = "dev.vibex.desktop";

/// How long a worker waits for the UI thread to answer.
///
/// A script may do real work before its view exists, so this is generous; it
/// exists so a wedged or closed window fails the install instead of hanging
/// the backend forever.
const HOST_REPLY_TIMEOUT: Duration = Duration::from_secs(30);

/// Work the UI thread has to perform for a plugin.
pub enum HostRequest {
    Load {
        plugin: LoadedPlugin,
        reply: SyncSender<Result<(), String>>,
    },
    Unload {
        plugin_id: String,
        reply: SyncSender<Result<(), String>>,
    },
}

/// A write a plugin asked the desktop to perform.
///
/// The host runs on the UI thread and cannot await the authority, so a write
/// is recorded here and drained by the workbench, which owns the backend.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PluginAction {
    /// Send a message to the session the plugin is looking at.
    SendMessage {
        plugin_id: String,
        session_id: String,
        text: String,
    },
}

/// A command a plugin registered at run time through `vibex.commands`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredCommand {
    pub plugin_id: String,
    pub id: String,
    pub title: String,
}

/// State shared between the host modules, the host and the workbench.
///
/// Each field is its own `Rc<RefCell<..>>` so a host module can borrow one
/// while another is used, and so the global and the `Host` observe the same
/// value.
#[derive(Clone, Default)]
pub(crate) struct PluginChannels {
    /// Writes waiting to be drained by the workbench.
    actions: Rc<RefCell<Vec<PluginAction>>>,
    /// Run-time commands, by plugin id.
    commands: Rc<RefCell<BTreeMap<String, Vec<RegisteredCommand>>>>,
    /// Command ids dispatched to a plugin but not yet taken by it.
    dispatched: Rc<RefCell<BTreeMap<String, Vec<String>>>>,
    /// Tools a plugin registered for the Agent, by plugin id.
    tools: Rc<RefCell<BTreeMap<String, Vec<RegisteredTool>>>>,
    /// Tool calls the Agent made, waiting for the plugin to take them.
    tool_calls: Rc<RefCell<BTreeMap<String, Vec<PluginToolCall>>>>,
}

/// A tool a plugin contributes to the Agent.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct RegisteredTool {
    pub plugin_id: String,
    pub name: String,
    pub description: String,
}

/// One invocation of a plugin's tool, waiting to be answered.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PluginToolCall {
    pub name: String,
    pub arguments: String,
}

impl PluginChannels {
    /// Drops a plugin's run-time state.
    fn forget(&self, plugin_id: &str) {
        self.commands.borrow_mut().remove(plugin_id);
        self.dispatched.borrow_mut().remove(plugin_id);
        self.tools.borrow_mut().remove(plugin_id);
        self.tool_calls.borrow_mut().remove(plugin_id);
    }

    /// Registers a tool; `false` when the name is already taken.
    pub(crate) fn register_tool(&self, plugin_id: &str, name: &str, description: &str) -> bool {
        let mut tools = self.tools.borrow_mut();
        let list = tools.entry(plugin_id.to_string()).or_default();
        if list.iter().any(|tool| tool.name == name) {
            return false;
        }
        list.push(RegisteredTool {
            plugin_id: plugin_id.to_string(),
            name: name.to_string(),
            description: description.to_string(),
        });
        true
    }

    /// Every tool every plugin contributes, for the Agent's tool list.
    pub(crate) fn tools(&self) -> Vec<RegisteredTool> {
        self.tools
            .borrow()
            .values()
            .flat_map(|list| list.iter().cloned())
            .collect()
    }

    /// Queues one call for the plugin that owns the tool.
    pub(crate) fn queue_tool_call(&self, plugin_id: &str, name: &str, arguments: &str) {
        self.tool_calls
            .borrow_mut()
            .entry(plugin_id.to_string())
            .or_default()
            .push(PluginToolCall {
                name: name.to_string(),
                arguments: arguments.to_string(),
            });
    }

    /// Takes the tool calls waiting for a plugin, clearing the queue.
    pub(crate) fn take_tool_calls(&self, plugin_id: &str) -> Vec<PluginToolCall> {
        self.tool_calls
            .borrow_mut()
            .remove(plugin_id)
            .unwrap_or_default()
    }

    /// Registers a run-time command; `false` when the id is already taken.
    fn register(&self, plugin_id: &str, id: &str, title: &str) -> bool {
        let mut commands = self.commands.borrow_mut();
        let list = commands.entry(plugin_id.to_string()).or_default();
        if list.iter().any(|command| command.id == id) {
            return false;
        }
        list.push(RegisteredCommand {
            plugin_id: plugin_id.to_string(),
            id: id.to_string(),
            title: title.to_string(),
        });
        true
    }

    /// A plugin's run-time commands, as `(id, title)` pairs.
    fn commands_for(&self, plugin_id: &str) -> Vec<(String, String)> {
        self.commands
            .borrow()
            .get(plugin_id)
            .map(|list| {
                list.iter()
                    .map(|command| (command.id.clone(), command.title.clone()))
                    .collect()
            })
            .unwrap_or_default()
    }

    /// Takes the command ids dispatched to a plugin, clearing the queue.
    fn take_dispatched(&self, plugin_id: &str) -> Vec<String> {
        self.dispatched
            .borrow_mut()
            .remove(plugin_id)
            .unwrap_or_default()
    }
}

/// The application-global view of the channels.
struct PluginActions(PluginChannels);

impl Global for PluginActions {}

/// The runtime and, once a plugin has been loaded, the manager over it.
///
/// The manager is built on first use because it needs two directories that are
/// only known from a `LoadedPlugin`: the store's roots. Deriving them there
/// means the host never has to be told the installation's home twice.
struct Host {
    runtime: Rc<ShellRuntime>,
    /// One manager per loaded plugin, so each plugin's host modules can close
    /// over that plugin's own grants.
    managers: BTreeMap<String, PluginManager>,
    session: Rc<RefCell<Option<PluginSessionSnapshot>>>,
    channels: PluginChannels,
}

impl Host {
    /// Builds a manager whose host modules answer only for this plugin.
    fn manager_for(&self, plugin: &LoadedPlugin) -> Result<PluginManager, String> {
        let (plugins_root, data_root) = store_roots(plugin);
        let grants = plugin
            .granted_permissions
            .iter()
            .cloned()
            .collect::<BTreeSet<_>>();
        let modules = host_modules(
            plugin.plugin_id.clone(),
            plugin.directory.clone(),
            plugin.data_directory.clone(),
            grants,
            Rc::clone(&self.session),
            self.channels.clone(),
        );
        let mut manager = PluginManager::new(vec![plugins_root]).with_data_home(data_root);
        // `gpui-shell` has no plural builder: each host module is one call,
        // and the manager gives every module to every plugin it loads.
        for module in modules {
            manager = manager
                .with_host_module(module)
                .map_err(|error| format!("the plugin host modules are invalid: {error}"))?;
        }
        Ok(manager)
    }
}

/// The global only holds the handle; the host itself is behind a `RefCell` so a
/// request can borrow it and the application at the same time.
struct HostState(Rc<RefCell<Host>>);

impl Global for HostState {}

/// The store's plugin and data roots, read back from one installed plugin.
///
/// The layout is `<home>/plugins/<id>` (a link), `<home>/plugin-versions/...`
/// and `<home>/plugin-data/<id>`, so the plugin's data directory names both.
fn store_roots(plugin: &LoadedPlugin) -> (PathBuf, PathBuf) {
    let data_root = plugin
        .data_directory
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| plugin.data_directory.clone());
    let home = data_root
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| data_root.clone());
    (home.join("plugins"), data_root)
}

/// Re-reads the installed manifests.
///
/// Discovery is cheap — it parses JSON and runs nothing — and doing it before
/// every load is what makes an install that happened while the application was
/// running visible without a restart.
fn discover(manager: &mut PluginManager) -> Result<(), String> {
    let mut failures = Vec::new();
    for outcome in manager.discover() {
        if let Err(error) = outcome {
            failures.push(error.to_string());
        }
    }
    if manager.available().next().is_none() && !failures.is_empty() {
        return Err(failures.join("; "));
    }
    Ok(())
}

fn load(
    host: &mut Host,
    plugin: &LoadedPlugin,
    window: &mut Window,
    cx: &mut App,
) -> Result<(), String> {
    let approved = plugin
        .granted_permissions
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let directory = plugin.directory.clone();
    let plugin_id = plugin.plugin_id.clone();
    let runtime = Rc::clone(&host.runtime);
    if let Some(mut previous) = host.managers.remove(&plugin_id) {
        previous.unload(&plugin_id);
    }
    host.channels.forget(&plugin_id);
    let mut manager = host.manager_for(plugin)?;
    discover(&mut manager)?;
    manager
        .load(
            &runtime,
            &plugin_id,
            |manifest| permits(&directory, manifest.id(), &approved),
            window,
            cx,
        )
        .map_err(|error| format!("{error:#}"))?;
    host.managers.insert(plugin_id, manager);
    Ok(())
}

fn unload(host: &mut Host, plugin_id: &str) -> Result<(), String> {
    if let Some(mut manager) = host.managers.remove(plugin_id) {
        manager.unload(plugin_id);
    }
    host.channels.forget(plugin_id);
    Ok(())
}

/// Whether the manifest on disk asks for nothing beyond what the user approved.
///
/// The installer already checked consent against the same file when it
/// unpacked the package. Checking again here is deliberate: the version
/// directory is ordinary files, and the host is the last place that can refuse
/// a manifest swapped after that approval.
fn permits(directory: &Path, manifest_id: &str, approved: &BTreeSet<String>) -> bool {
    if manifest_id.is_empty() {
        return false;
    }
    let Ok(manifest) =
        PluginManifest::from_file(&directory.join(vibex_plugin::AUTHOR_MANIFEST_FILE))
    else {
        return false;
    };
    manifest.id == manifest_id
        && manifest
            .permissions()
            .iter()
            .all(|permission| approved.contains(&permission.key))
}

/// The view a loaded plugin draws, if it is loaded.
pub fn view(cx: &App, plugin_id: &str) -> Option<Entity<ScriptView>> {
    let host = cx.try_global::<HostState>()?;
    let host = host.0.borrow();
    host.managers
        .get(plugin_id)?
        .plugin(plugin_id)
        .map(|plugin| plugin.view().clone())
}

/// Asks the host to load a plugin nobody has opened yet.
///
/// The catalog can list a plugin — enabled, on disk — that this session has not
/// loaded: the application started after it was installed, or its panel was
/// never opened. A panel asks for its view when the reader opens it, so the
/// script starts then rather than at startup.
///
/// The request is fire-and-forget: the caller is drawing a frame and cannot
/// wait, the pump's reply channel is dropped, and the pump repaints when the
/// work is done. The panel draws its placeholder until then.
pub fn request_load(plugin_id: String, record: &vibex_core::InstalledPlugin) {
    let Some(directory) = record.install_path.clone().map(PathBuf::from) else {
        return;
    };
    let loaded = LoadedPlugin::new(
        plugin_id,
        record.version.clone(),
        directory,
        record.data_path.clone().unwrap_or_default(),
    )
    .with_granted_permissions(
        record
            .granted_permissions
            .iter()
            .map(|grant| grant.key.clone()),
    );
    let (reply, _answer) = sync_channel(1);
    if let Err(error) = post(HostRequest::Load {
        plugin: loaded,
        reply,
    }) {
        eprintln!("vibex-foundation: plugin-panel-load-failed {error}");
    }
}

/// What a plugin sees as the current Vibex session.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct PluginSessionSnapshot {
    pub id: String,
    pub title: String,
    pub workspace_id: String,
    pub workspace_root: String,
    pub agent_id: String,
}

struct SessionState(Rc<RefCell<Option<PluginSessionSnapshot>>>);

impl Global for SessionState {}

/// Updates the session the `vibex.session` / `vibex.workspace` modules expose.
///
/// The workbench calls this while it renders; the value is only stored when it
/// actually changed, so a frame that draws the same session costs a comparison.
pub fn set_session(cx: &App, session: Option<PluginSessionSnapshot>) {
    let Some(state) = cx.try_global::<SessionState>() else {
        return;
    };
    let mut current = state.0.borrow_mut();
    if *current != session {
        *current = session;
    }
}

/// Takes the writes plugins asked for since the last drain.
pub fn take_pending_actions(cx: &App) -> Vec<PluginAction> {
    let Some(state) = cx.try_global::<PluginActions>() else {
        return Vec::new();
    };
    std::mem::take(&mut *state.0.actions.borrow_mut())
}

/// The commands plugins registered at run time, as palette contributions.
pub fn runtime_commands(cx: &App) -> Vec<vibex_core::PluginContribution> {
    let Some(state) = cx.try_global::<PluginActions>() else {
        return Vec::new();
    };
    let commands = state.0.commands.borrow();
    commands
        .iter()
        .flat_map(|(plugin_id, list)| {
            list.iter().map(|command| vibex_core::PluginContribution {
                plugin_id: plugin_id.clone(),
                kind: "command".to_string(),
                contribution_id: command.id.clone(),
                title: Some(command.title.clone()),
                detail: None,
                enabled: true,
            })
        })
        .collect()
}

/// Records that a command was dispatched to a plugin.
///
/// The plugin's panel reads the queue with `vibex.commands.take()`; the host
/// cannot call into the script, because `gpui-shell` has no function value.
pub fn dispatch_command(cx: &App, plugin_id: &str, command_id: &str) {
    let Some(state) = cx.try_global::<PluginActions>() else {
        return;
    };
    state
        .0
        .dispatched
        .borrow_mut()
        .entry(plugin_id.to_string())
        .or_default()
        .push(command_id.to_string());
}

/// The tools plugins contribute to the Agent.
///
/// The Agent's tool list is assembled by the workbench, so this is the one
/// place it asks for the plugin half. A tool's name is prefixed with the
/// plugin that owns it, so two plugins may both call a tool `search` without
/// one shadowing the other.
pub fn runtime_tools(cx: &App) -> Vec<RegisteredTool> {
    let Some(state) = cx.try_global::<PluginActions>() else {
        return Vec::new();
    };
    state.0.tools()
}

/// Queues one tool call for the plugin that owns it.
///
/// The script answers with `vibex.agent.tools.take()`, the same queue shape
/// `vibex.commands` uses, because the host cannot call into the script.
pub fn dispatch_tool_call(cx: &App, plugin_id: &str, name: &str, arguments: &str) {
    let Some(state) = cx.try_global::<PluginActions>() else {
        return;
    };
    state.0.queue_tool_call(plugin_id, name, arguments);
}

/// The UI thread's inbox, set once the first window starts its pump.
static INBOX: OnceLock<UnboundedSender<HostRequest>> = OnceLock::new();

/// Installs the runtime the market calls into, and the globals script needs.
///
/// Called once, next to the other application-wide initialisation, and before
/// any window is opened: the component catalog is frozen here and shared by
/// every plugin the session will run.
pub fn init(cx: &mut App) {
    let components = components();
    // The catalog's initialiser is `gpui_component::init`, which the desktop has
    // already run; the call is what installs the base globals and the style
    // table `gpui-shell` resolves script method names against.
    gpui_shell::init_with_components(cx, &components);
    let _ = gpui_shell::set_bundle_id(BUNDLE_ID);

    let session = Rc::new(RefCell::new(None));
    cx.set_global(SessionState(session.clone()));
    let channels = PluginChannels::default();
    cx.set_global(PluginActions(channels.clone()));

    match ShellRuntime::new_isolated_with_components(components) {
        Ok(runtime) => {
            cx.set_global(HostState(Rc::new(RefCell::new(Host {
                runtime,
                managers: BTreeMap::new(),
                session: session.clone(),
                channels,
            }))));
        }
        Err(error) => {
            // A host that cannot start is not a reason to refuse to launch the
            // application: the Plugins page still lists and manages plugins,
            // and every load reports `plugin_host_unavailable`.
            eprintln!("vibex-plugin-host: unavailable: {error:#}");
        }
    }

    if vibex_desktop_runtime::install_plugin_runtime(Arc::new(ShellPluginRuntime)).is_err() {
        eprintln!("vibex-plugin-host: a runtime was already installed");
    }
}

/// The host modules a Vibex plugin may import.
///
/// The module is reachable from every plugin the manager loads; each function
/// asks the host's grant map before answering, and the current plugin id comes
/// from the policy frame `gpui-shell` puts around the call.
fn host_modules(
    plugin_id: String,
    directory: PathBuf,
    data_dir: PathBuf,
    grants: BTreeSet<String>,
    session: Rc<RefCell<Option<PluginSessionSnapshot>>>,
    channels: PluginChannels,
) -> Vec<HostModule> {
    let session_reader = {
        let current_session = Rc::clone(&session);
        let send_session = Rc::clone(&session);
        let current_grants = grants.clone();
        let send_grants = grants.clone();
        let current_plugin = plugin_id.clone();
        let send_plugin = plugin_id.clone();
        let send_channels = channels.clone();
        HostModule::new("vibex.session")
            .declarations(
                "export function current(): { id: string; title: string; workspaceId: string; workspaceRoot: string; agentId: string } | null;\nexport function send(text: string): boolean;",
            )
            .function("current", move |_| {
                require_permission(&current_grants, "agent.read", &current_plugin)?;
                let session = current_session.borrow();
                let Some(session) = session.as_ref() else {
                    return Ok(HostValue::Null);
                };
                Ok(HostValue::Object(vec![
                    ("id".to_string(), HostValue::from(session.id.clone())),
                    (
                        "title".to_string(),
                        HostValue::from(session.title.clone()),
                    ),
                    (
                        "workspaceId".to_string(),
                        HostValue::from(session.workspace_id.clone()),
                    ),
                    (
                        "workspaceRoot".to_string(),
                        HostValue::from(session.workspace_root.clone()),
                    ),
                    (
                        "agentId".to_string(),
                        HostValue::from(session.agent_id.clone()),
                    ),
                ]))
            })
            .function("send", move |arguments: &gpui_shell::HostArguments| {
                require_permission(&send_grants, "agent.send", &send_plugin)?;
                let text = arguments.string(0)?.trim().to_string();
                if text.is_empty() {
                    return Err(HostError::new("the message is empty"));
                }
                let session = send_session.borrow();
                let Some(session) = session.as_ref() else {
                    return Ok(HostValue::Bool(false));
                };
                send_channels
                    .actions
                    .borrow_mut()
                    .push(PluginAction::SendMessage {
                        plugin_id: send_plugin.clone(),
                        session_id: session.id.clone(),
                        text,
                    });
                Ok(HostValue::Bool(true))
            })
    };

    let workspace_reader = {
        let session = Rc::clone(&session);
        let grants = grants.clone();
        let plugin_id = plugin_id.clone();
        HostModule::new("vibex.workspace")
            .declarations("export function current(): { id: string; root: string } | null;")
            .function("current", move |_| {
                require_permission(&grants, "agent.read", &plugin_id)?;
                let session = session.borrow();
                let Some(session) = session.as_ref() else {
                    return Ok(HostValue::Null);
                };
                Ok(HostValue::Object(vec![
                    (
                        "id".to_string(),
                        HostValue::from(session.workspace_id.clone()),
                    ),
                    (
                        "root".to_string(),
                        HostValue::from(session.workspace_root.clone()),
                    ),
                ]))
            })
    };

    let mut modules = Vec::new();
    if grants.contains("agent.read") || grants.contains("agent.send") {
        modules.push(session_reader);
    }
    if grants.contains("agent.read") {
        modules.push(workspace_reader);
    }
    if grants.contains("storage") {
        modules.push(settings_module(plugin_id.clone(), data_dir, grants.clone()));
    }
    if grants.contains("ui.command") {
        modules.push(commands_module(
            plugin_id.clone(),
            grants.clone(),
            channels.clone(),
        ));
    }

    // The modules that reach beyond the panel. Each reads its scope from the
    // package's own manifest, so a grant the user approved cannot be widened by
    // the script at call time.
    let scope = Arc::new(crate::plugin_capabilities::PluginScope::from_directory(
        &directory,
    ));
    let granted: Vec<String> = grants.iter().cloned().collect();
    let workspace = workspace_root();
    let home = home_dir();
    if grants.contains("clipboard") {
        modules.push(crate::plugin_capabilities::clipboard_module(
            plugin_id.clone(),
            granted.clone(),
        ));
    }
    if grants.contains("net.fetch") {
        modules.push(crate::plugin_capabilities::network_module(
            plugin_id.clone(),
            granted.clone(),
            Arc::clone(&scope),
        ));
    }
    if grants.contains("fs.read") || grants.contains("fs.write") {
        modules.push(crate::plugin_capabilities::fs_module(
            plugin_id.clone(),
            granted.clone(),
            Arc::clone(&scope),
            workspace,
            home,
        ));
    }
    if grants.contains("process.execute") {
        modules.push(crate::plugin_capabilities::process_module(
            plugin_id.clone(),
            granted.clone(),
            Arc::clone(&scope),
        ));
    }
    if grants.contains("agent.tool") {
        modules.push(crate::plugin_capabilities::agent_tools_module(
            plugin_id.clone(),
            granted,
            channels,
        ));
    }
    modules
}

/// The workspace root a `${workspace}` scope resolves against.
///
/// The host has no workspace of its own; the session snapshot is what names
/// one, and a plugin with no session gets no workspace scope rather than a
/// guess.
fn workspace_root() -> Option<PathBuf> {
    None
}

/// The home directory a `$home` scope resolves against.
fn home_dir() -> Option<PathBuf> {
    std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)
}

/// The commands a plugin registers and the ones dispatched to it.
///
/// A command the palette runs is written to the plugin's dispatched queue; the
/// script takes it with `take()`. That is the shape the host can support:
/// `gpui-shell` has no function value, so the host cannot hold a script
/// callback and call it later.
fn commands_module(
    plugin_id: String,
    grants: BTreeSet<String>,
    channels: PluginChannels,
) -> HostModule {
    let register = {
        let grants = grants.clone();
        let plugin_id = plugin_id.clone();
        let channels = channels.clone();
        move |arguments: &gpui_shell::HostArguments| {
            require_permission(&grants, "ui.command", &plugin_id)?;
            let id = arguments.string(0)?.trim().to_string();
            let title = arguments.string(1)?.trim().to_string();
            if id.is_empty() || title.is_empty() {
                return Err(HostError::new("a command needs an id and a title"));
            }
            if id.len() > 128 || title.len() > 128 {
                return Err(HostError::new("the command id or title is too long"));
            }
            Ok(HostValue::Bool(channels.register(&plugin_id, &id, &title)))
        }
    };
    let list = {
        let grants = grants.clone();
        let plugin_id = plugin_id.clone();
        let channels = channels.clone();
        move |_: &gpui_shell::HostArguments| {
            require_permission(&grants, "ui.command", &plugin_id)?;
            let values = channels
                .commands_for(&plugin_id)
                .into_iter()
                .map(|(id, title)| {
                    HostValue::Object(vec![
                        ("id".to_string(), HostValue::from(id)),
                        ("title".to_string(), HostValue::from(title)),
                    ])
                })
                .collect();
            Ok(HostValue::Array(values))
        }
    };
    let take = {
        let grants = grants.clone();
        let plugin_id = plugin_id.clone();
        let channels = channels.clone();
        move |_: &gpui_shell::HostArguments| {
            require_permission(&grants, "ui.command", &plugin_id)?;
            Ok(HostValue::Array(
                channels
                    .take_dispatched(&plugin_id)
                    .into_iter()
                    .map(HostValue::from)
                    .collect(),
            ))
        }
    };
    HostModule::new("vibex.commands")
        .declarations(
            "export function register(id: string, title: string): boolean;\nexport function list(): { id: string; title: string }[];\nexport function take(): string[];",
        )
        .function("register", register)
        .function("list", list)
        .function("take", take)
}

/// The plugin's own durable settings store, namespaced by its data directory.
fn settings_module(plugin_id: String, data_dir: PathBuf, grants: BTreeSet<String>) -> HostModule {
    let get = {
        let data_dir = data_dir.clone();
        let grants = grants.clone();
        let plugin_id = plugin_id.clone();
        move |arguments: &gpui_shell::HostArguments| {
            require_permission(&grants, "storage", &plugin_id)?;
            let key = arguments.string(0)?.to_string();
            let settings = read_settings(&data_dir);
            Ok(settings
                .get(&key)
                .cloned()
                .map(json_to_host_value)
                .unwrap_or(HostValue::Null))
        }
    };
    let set = {
        let data_dir = data_dir;
        let grants = grants;
        let plugin_id = plugin_id;
        move |arguments: &gpui_shell::HostArguments| {
            require_permission(&grants, "storage", &plugin_id)?;
            let key = arguments.string(0)?.to_string();
            let value = host_value_to_json(arguments.value(1)?);
            let mut settings = read_settings(&data_dir);
            settings.insert(key, value);
            let path = data_dir.join("vibex-settings.json");
            if let Some(parent) = path.parent() {
                let _ = std::fs::create_dir_all(parent);
            }
            let bytes = serde_json::to_vec_pretty(&settings).map_err(|error| {
                HostError::new(format!("settings could not be encoded: {error}"))
            })?;
            let temporary = path.with_extension("json.tmp");
            std::fs::write(&temporary, bytes).map_err(|error| {
                HostError::new(format!("settings could not be written: {error}"))
            })?;
            // Unix replaces an existing file with rename; Windows does not.
            if std::fs::rename(&temporary, &path).is_err() {
                let _ = std::fs::remove_file(&path);
                std::fs::rename(&temporary, &path).map_err(|error| {
                    HostError::new(format!("settings could not be saved: {error}"))
                })?;
            }
            Ok(HostValue::Bool(true))
        }
    };

    HostModule::new("vibex.settings")
        .declarations(
            "export function get(key: string): unknown;\nexport function save(key: string, value: unknown): boolean;",
        )
        .function("get", get)
        .function("save", set)
}

fn read_settings(data_dir: &Path) -> serde_json::Map<String, serde_json::Value> {
    std::fs::read(data_dir.join("vibex-settings.json"))
        .ok()
        .and_then(|bytes| serde_json::from_slice::<serde_json::Value>(&bytes).ok())
        .and_then(|value| value.as_object().cloned())
        .unwrap_or_default()
}

fn host_value_to_json(value: &HostValue) -> serde_json::Value {
    match value {
        HostValue::Null => serde_json::Value::Null,
        HostValue::Bool(value) => serde_json::Value::Bool(*value),
        HostValue::Number(value) => serde_json::Number::from_f64(*value)
            .map(serde_json::Value::Number)
            .unwrap_or(serde_json::Value::Null),
        HostValue::Str(value) => serde_json::Value::String(value.clone()),
        HostValue::Array(values) => {
            serde_json::Value::Array(values.iter().map(host_value_to_json).collect())
        }
        HostValue::Object(values) => serde_json::Value::Object(
            values
                .iter()
                .map(|(key, value)| (key.clone(), host_value_to_json(value)))
                .collect(),
        ),
    }
}

fn json_to_host_value(value: serde_json::Value) -> HostValue {
    match value {
        serde_json::Value::Null => HostValue::Null,
        serde_json::Value::Bool(value) => HostValue::Bool(value),
        serde_json::Value::Number(value) => value
            .as_f64()
            .map(HostValue::Number)
            .unwrap_or(HostValue::Null),
        serde_json::Value::String(value) => HostValue::Str(value),
        serde_json::Value::Array(values) => {
            HostValue::Array(values.into_iter().map(json_to_host_value).collect())
        }
        serde_json::Value::Object(values) => HostValue::Object(
            values
                .into_iter()
                .map(|(key, value)| (key, json_to_host_value(value)))
                .collect(),
        ),
    }
}

/// Refuses a `vibex.*` call the current plugin did not ask to be allowed.
fn require_permission(
    grants: &BTreeSet<String>,
    permission: &str,
    plugin_id: &str,
) -> Result<(), HostError> {
    if grants.contains(permission) {
        Ok(())
    } else {
        Err(HostError::new(format!(
            "plugin `{plugin_id}` has not been granted `{permission}`"
        )))
    }
}

fn components() -> FrozenComponentRegistry {
    static COMPONENTS: OnceLock<FrozenComponentRegistry> = OnceLock::new();
    COMPONENTS
        .get_or_init(|| {
            gpui_shell::components::components().unwrap_or_else(|error| {
                eprintln!("vibex-plugin-host: the component catalog failed: {error}");
                FrozenComponentRegistry::default()
            })
        })
        .clone()
}

/// Runs one request on the UI thread.
fn execute(window: &mut Window, cx: &mut App, request: HostRequest) {
    let Some(host) = cx
        .try_global::<HostState>()
        .map(|state| Rc::clone(&state.0))
    else {
        finish(request, Err("the plugin host is not installed".to_string()));
        return;
    };
    let outcome = {
        let mut host = host.borrow_mut();
        match &request {
            HostRequest::Load { plugin, .. } => load(&mut host, plugin, window, cx),
            HostRequest::Unload { plugin_id, .. } => unload(&mut host, plugin_id),
        }
    };
    finish(request, outcome);
    // A plugin's view comes into existence here, and the surface that asked for
    // it is already on screen showing its placeholder: repaint every window so
    // the next frame picks the view up.
    cx.refresh_windows();
}

fn finish(request: HostRequest, outcome: Result<(), String>) {
    match request {
        HostRequest::Load { reply, .. } | HostRequest::Unload { reply, .. } => {
            let _ = reply.send(outcome);
        }
    }
}

/// Starts the UI-thread pump that answers [`ShellPluginRuntime`].
///
/// Spawned with the first window and anchored to it: a plugin load needs a
/// window, and a window that is gone cannot run script anyway.
pub fn start_pump<T: 'static>(window: &mut Window, cx: &mut gpui::Context<T>) -> bool {
    if INBOX.get().is_some() {
        return false;
    }
    let (sender, receiver) = unbounded();
    if INBOX.set(sender).is_err() {
        return false;
    }
    cx.spawn_in(window, async move |_, mut cx| {
        pump(receiver, &mut cx).await;
    })
    .detach();
    true
}

async fn pump(mut receiver: UnboundedReceiver<HostRequest>, cx: &mut AsyncWindowContext) {
    while let Some(request) = receiver.next().await {
        // A window closed mid-flight answers with an error rather than leaving
        // the worker blocked until its timeout.
        if cx
            .update(|window, app| execute(window, app, request))
            .is_err()
        {
            break;
        }
    }
}

/// The `Send + Sync` half: posts work to the pump and waits for its verdict.
pub struct ShellPluginRuntime;

impl PluginRuntime for ShellPluginRuntime {
    fn load(&self, plugin: &LoadedPlugin) -> Result<(), PluginError> {
        let (reply, answer) = sync_channel(1);
        post(HostRequest::Load {
            plugin: plugin.clone(),
            reply,
        })?;
        wait(answer)
    }

    fn unload(&self, plugin_id: &str) -> Result<(), PluginError> {
        let (reply, answer) = sync_channel(1);
        post(HostRequest::Unload {
            plugin_id: plugin_id.to_string(),
            reply,
        })?;
        wait(answer)
    }
}

fn post(request: HostRequest) -> Result<(), PluginError> {
    let Some(inbox) = INBOX.get() else {
        return Err(PluginError::HostUnavailable);
    };
    inbox
        .unbounded_send(request)
        .map_err(|_| PluginError::Host("the window that runs plugins is gone".to_string()))
}

fn wait(answer: std::sync::mpsc::Receiver<Result<(), String>>) -> Result<(), PluginError> {
    match answer.recv_timeout(HOST_REPLY_TIMEOUT) {
        Ok(Ok(())) => Ok(()),
        Ok(Err(error)) => Err(PluginError::Host(error)),
        Err(_) => Err(PluginError::Host(
            "the window did not answer the plugin request in time".to_string(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn directory(manifest: serde_json::Value) -> tempfile::TempDir {
        let directory = tempfile::tempdir().unwrap();
        std::fs::write(
            directory.path().join(vibex_plugin::AUTHOR_MANIFEST_FILE),
            serde_json::to_vec(&manifest).unwrap(),
        )
        .unwrap();
        directory
    }

    fn manifest(capabilities: serde_json::Value) -> serde_json::Value {
        serde_json::json!({
            "schemaVersion": 1,
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.0.0",
            "license": "MIT",
            "entry": "main.js",
            "capabilities": capabilities
        })
    }

    fn approved(keys: &[&str]) -> BTreeSet<String> {
        keys.iter().map(|key| (*key).to_string()).collect()
    }

    #[test]
    fn a_manifest_the_user_approved_is_allowed() {
        let directory = directory(manifest(serde_json::json!({ "panel": true })));
        assert!(permits(
            directory.path(),
            "acme.widgets",
            &approved(&["ui.panel"])
        ));
    }

    #[test]
    fn a_manifest_that_grew_after_consent_is_refused() {
        let directory = directory(manifest(serde_json::json!({
            "panel": true,
            "network": { "hosts": ["api.example.com"] }
        })));
        // The user approved a panel; the file on disk now also wants the
        // network, so the host refuses the package.
        assert!(!permits(
            directory.path(),
            "acme.widgets",
            &approved(&["ui.panel"])
        ));
    }

    #[test]
    fn an_unreadable_manifest_is_refused() {
        let directory = tempfile::tempdir().unwrap();
        assert!(!permits(
            directory.path(),
            "acme.widgets",
            &approved(&["ui.panel"])
        ));
        std::fs::write(
            directory.path().join(vibex_plugin::AUTHOR_MANIFEST_FILE),
            b"not json",
        )
        .unwrap();
        assert!(!permits(
            directory.path(),
            "acme.widgets",
            &approved(&["ui.panel"])
        ));
    }

    #[test]
    fn a_renamed_package_is_refused() {
        let directory = directory(manifest(serde_json::json!({ "panel": true })));
        assert!(!permits(
            directory.path(),
            "other.widgets",
            &approved(&["ui.panel"])
        ));
    }

    #[test]
    fn the_store_roots_come_from_the_plugins_data_directory() {
        let plugin = LoadedPlugin::new(
            "acme.widgets",
            "1.0.0",
            "/home/u/.vibex/plugin-versions/acme.widgets/1.0.0",
            "/home/u/.vibex/plugin-data/acme.widgets",
        );
        let (plugins_root, data_root) = store_roots(&plugin);
        assert_eq!(plugins_root, PathBuf::from("/home/u/.vibex/plugins"));
        assert_eq!(data_root, PathBuf::from("/home/u/.vibex/plugin-data"));
    }

    #[test]
    fn the_vibex_modules_follow_the_grants() {
        let session = Rc::new(RefCell::new(None));
        let data_dir = PathBuf::from("/tmp/vibex-plugin-test");
        assert!(
            host_modules(
                "acme.widgets".to_string(),
                data_dir.clone(),
                data_dir.clone(),
                BTreeSet::new(),
                session.clone(),
                PluginChannels::default(),
            )
            .is_empty(),
            "a plugin with no grants must not get a host module"
        );

        let session_modules = host_modules(
            "acme.widgets".to_string(),
            data_dir.clone(),
            data_dir.clone(),
            BTreeSet::from(["agent.read".to_string()]),
            session.clone(),
            PluginChannels::default(),
        );
        assert_eq!(session_modules.len(), 2);

        // `agent.send` alone gets the session module (its `send`), not the
        // workspace reader, which stays behind `agent.read`.
        let send_modules = host_modules(
            "acme.widgets".to_string(),
            data_dir.clone(),
            data_dir.clone(),
            BTreeSet::from(["agent.send".to_string()]),
            session.clone(),
            PluginChannels::default(),
        );
        assert_eq!(send_modules.len(), 1);

        let settings_modules = host_modules(
            "acme.widgets".to_string(),
            data_dir.clone(),
            data_dir.clone(),
            BTreeSet::from(["storage".to_string()]),
            session.clone(),
            PluginChannels::default(),
        );
        assert_eq!(settings_modules.len(), 1);

        let command_modules = host_modules(
            "acme.widgets".to_string(),
            data_dir.clone(),
            data_dir,
            BTreeSet::from(["ui.command".to_string()]),
            session,
            PluginChannels::default(),
        );
        assert_eq!(command_modules.len(), 1);

        for module in session_modules
            .into_iter()
            .chain(send_modules)
            .chain(settings_modules)
            .chain(command_modules)
        {
            module
                .validate()
                .expect("the built-in host modules must be valid");
        }
    }

    #[test]
    fn forgetting_a_plugin_drops_its_commands_and_queue() {
        let channels = PluginChannels::default();
        channels
            .commands
            .borrow_mut()
            .entry("acme.widgets".to_string())
            .or_default()
            .push(RegisteredCommand {
                plugin_id: "acme.widgets".to_string(),
                id: "open".to_string(),
                title: "Open".to_string(),
            });
        channels
            .dispatched
            .borrow_mut()
            .entry("acme.widgets".to_string())
            .or_default()
            .push("open".to_string());
        channels.forget("acme.widgets");
        assert!(channels.commands.borrow().is_empty());
        assert!(channels.dispatched.borrow().is_empty());
    }

    #[test]
    fn run_time_commands_register_and_dispatch_once() {
        let channels = PluginChannels::default();
        assert!(channels.register("acme.widgets", "open", "Open"));
        assert!(!channels.register("acme.widgets", "open", "Open again"));
        assert_eq!(
            channels.commands_for("acme.widgets"),
            vec![("open".to_string(), "Open".to_string())]
        );
        channels
            .dispatched
            .borrow_mut()
            .entry("acme.widgets".to_string())
            .or_default()
            .push("open".to_string());
        assert_eq!(
            channels.take_dispatched("acme.widgets"),
            vec!["open".to_string()]
        );
        assert!(channels.take_dispatched("acme.widgets").is_empty());
    }

    #[test]
    fn settings_values_round_trip_through_json() {
        let value = HostValue::Object(vec![
            ("title".to_string(), HostValue::Str("hello".to_string())),
            ("count".to_string(), HostValue::Number(3.0)),
            (
                "items".to_string(),
                HostValue::Array(vec![HostValue::Bool(true), HostValue::Null]),
            ),
        ]);
        let json = host_value_to_json(&value);
        assert_eq!(json_to_host_value(json), value);
    }

    #[test]
    fn a_missing_grant_is_a_host_error() {
        assert!(require_permission(&BTreeSet::new(), "agent.read", "acme.widgets").is_err());
        assert!(
            require_permission(
                &BTreeSet::from(["agent.read".to_string()]),
                "agent.read",
                "acme.widgets"
            )
            .is_ok()
        );
    }
}

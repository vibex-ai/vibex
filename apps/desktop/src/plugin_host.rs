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
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::mpsc::{SyncSender, sync_channel};
use std::sync::{Arc, OnceLock};
use std::time::Duration;

use futures_channel::mpsc::{UnboundedReceiver, UnboundedSender, unbounded};
use futures_util::StreamExt as _;
use gpui::{App, AsyncWindowContext, Entity, Global, Window};
use gpui_shell::plugin::PluginManager;
use gpui_shell::{FrozenComponentRegistry, ScriptView, ShellRuntime};
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

/// The runtime and, once a plugin has been loaded, the manager over it.
///
/// The manager is built on first use because it needs two directories that are
/// only known from a `LoadedPlugin`: the store's roots. Deriving them there
/// means the host never has to be told the installation's home twice.
struct Host {
    runtime: Rc<ShellRuntime>,
    manager: Option<PluginManager>,
}

impl Host {
    fn manager(&mut self, plugin: &LoadedPlugin) -> &mut PluginManager {
        if self.manager.is_none() {
            let (plugins_root, data_root) = store_roots(plugin);
            self.manager = Some(PluginManager::new(vec![plugins_root]).with_data_home(data_root));
        }
        self.manager
            .as_mut()
            .expect("the manager was just installed")
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
    let manager = host.manager(plugin);
    discover(manager)?;
    // Re-enabling goes through this path too, and the manager refuses a second
    // load of an id it already holds.
    manager.unload(&plugin_id);
    manager
        .load(
            &runtime,
            &plugin_id,
            |manifest| permits(&directory, manifest.id(), &approved),
            window,
            cx,
        )
        .map_err(|error| format!("{error:#}"))
}

fn unload(host: &mut Host, plugin_id: &str) -> Result<(), String> {
    if let Some(manager) = host.manager.as_mut() {
        manager.unload(plugin_id);
    }
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
    host.manager
        .as_ref()?
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

    match ShellRuntime::new_isolated_with_components(components) {
        Ok(runtime) => {
            cx.set_global(HostState(Rc::new(RefCell::new(Host {
                runtime,
                manager: None,
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
}

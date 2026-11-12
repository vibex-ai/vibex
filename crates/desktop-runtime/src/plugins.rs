//! The desktop's plugin handle: opens a connection and calls the service.
//!
//! The scripting host itself lives in the desktop binary, because it is UI
//! state on the UI thread. It registers its runtime through
//! [`install_plugin_runtime`]; a build without one (the TUI, the headless
//! server, the isolated test runtimes) keeps [`vibex_plugin::NullRuntime`] and
//! manages packages without executing them.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, OnceLock};

use ed25519_dalek::VerifyingKey;
use semver::Version;
use vibex_core::{
    InstalledPlugin, PluginAutoUpdateRequest, PluginEnabledRequest, PluginLocalInstallRequest,
    PluginMarketDetail, PluginMarketDetailRequest, PluginMarketIndexRequest,
    PluginMarketIndexResponse, PluginMarketInstallRequest, PluginMarketInstallResult,
    PluginMarketSearchRequest, PluginMarketSearchResponse, PluginUninstallRequest,
    PluginUpdateApplyRequest, PluginUpdateCheckRequest, PluginUpdateCheckResponse, VibexResult,
};
use vibex_db::open_database;
use vibex_plugin::{
    MarketCache, MarketCatalog, MarketConfig, MarketHttpClient, MarketUrlPolicy, NullRuntime,
    PluginRuntime, PluginService, PluginStore,
};

/// The default index address.
pub const DEFAULT_PLUGIN_INDEX: &str = "https://plugins.peatboy.com/index.json";

/// The runtime the desktop installs once its scripting host is up.
///
/// The market crate cannot name the host, so the binary hands it over here. It
/// is process-wide because the host is: one window, one VM, one grant per
/// plugin. Until it is set, every script load reports `plugin_host_unavailable`
/// and the rest of the plugin management still works.
static RUNTIME: OnceLock<Arc<dyn PluginRuntime>> = OnceLock::new();

/// Installs the scripting host the plugin service will call.
pub fn install_plugin_runtime(
    runtime: Arc<dyn PluginRuntime>,
) -> Result<(), Arc<dyn PluginRuntime>> {
    RUNTIME.set(runtime)
}

fn installed_runtime() -> Arc<dyn PluginRuntime> {
    RUNTIME
        .get()
        .cloned()
        .unwrap_or_else(|| Arc::new(NullRuntime))
}

/// A handle that resolves the plugin service for one installation.
#[derive(Clone)]
pub struct PluginHandle {
    database_path: PathBuf,
    runtime: Arc<dyn PluginRuntime>,
}

impl PluginHandle {
    pub fn new(database_path: PathBuf) -> Self {
        Self {
            database_path,
            runtime: installed_runtime(),
        }
    }

    /// The root the store lives under.
    fn home(&self) -> PathBuf {
        self.database_path
            .parent()
            .filter(|parent| !parent.as_os_str().is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("."))
    }

    /// The service for this installation.
    fn service(&self) -> PluginService {
        let home = self.home();
        let config = MarketConfig {
            index_urls: index_urls(),
            root_public_key: embedded_root_key(),
            github: Some(vibex_plugin::GithubTopicSource::new(MarketHttpClient::new(
                MarketUrlPolicy::Public,
            ))),
            cache: MarketCache::new(home.join("plugin-market")),
            index_ttl: vibex_plugin::market::INDEX_TTL,
            github_ttl: vibex_plugin::market::GITHUB_TTL,
            allow_unverified_index: embedded_root_key().is_none(),
        };
        let catalog = MarketCatalog::new(MarketHttpClient::new(MarketUrlPolicy::Public), config);
        PluginService::new(
            catalog,
            PluginStore::new(home),
            self.runtime.clone(),
            Version::parse(env!("CARGO_PKG_VERSION")).unwrap_or_else(|_| Version::new(0, 0, 0)),
        )
    }

    fn connection(&self) -> VibexResult<rusqlite::Connection> {
        let connection = open_database(&self.database_path)?;
        seed_once(&self.database_path, &self.service(), &connection);
        Ok(connection)
    }

    // ---- catalog ----

    pub async fn index(
        &self,
        request: PluginMarketIndexRequest,
    ) -> VibexResult<PluginMarketIndexResponse> {
        Ok(self.service().index(&request).await)
    }

    pub async fn search(
        &self,
        request: PluginMarketSearchRequest,
    ) -> VibexResult<PluginMarketSearchResponse> {
        Ok(self.service().search(&request).await)
    }

    pub async fn detail(
        &self,
        request: PluginMarketDetailRequest,
    ) -> VibexResult<PluginMarketDetail> {
        self.service()
            .detail(&request.entry_id, request.version.as_deref())
            .await
    }

    // ---- lifecycle ----

    pub fn list(&self) -> VibexResult<Vec<InstalledPlugin>> {
        self.service().list(&self.connection()?)
    }

    /// Every contribution the enabled plugins make.
    ///
    /// The right rail reads this instead of a compile-time list of panels, so a
    /// plugin installed from the market appears beside the first-party ones and
    /// one turned off in the Plugins page disappears.
    pub fn contributions(&self) -> VibexResult<Vec<vibex_core::PluginContribution>> {
        self.service().contributions(&self.connection()?)
    }

    pub fn set_enabled(&self, request: PluginEnabledRequest) -> VibexResult<InstalledPlugin> {
        self.service()
            .set_enabled(&self.connection()?, &request.plugin_id, request.enabled)
    }

    pub fn set_auto_update(
        &self,
        request: PluginAutoUpdateRequest,
    ) -> VibexResult<InstalledPlugin> {
        self.service()
            .set_auto_update(&self.connection()?, &request.plugin_id, request.auto_update)
    }

    pub fn uninstall(&self, request: PluginUninstallRequest) -> VibexResult<()> {
        self.service()
            .uninstall(&self.connection()?, &request.plugin_id, request.remove_data)
    }

    pub fn install_from_file(
        &self,
        request: PluginLocalInstallRequest,
    ) -> VibexResult<PluginMarketInstallResult> {
        self.service()
            .install_from_file(&self.connection()?, &request.path, request.enable)
    }

    pub async fn install(
        &self,
        request: PluginMarketInstallRequest,
    ) -> VibexResult<PluginMarketInstallResult> {
        self.service().install(self.connection()?, &request).await
    }

    pub async fn check_updates(
        &self,
        request: PluginUpdateCheckRequest,
    ) -> VibexResult<PluginUpdateCheckResponse> {
        self.service()
            .check_updates(self.connection()?, &request)
            .await
    }

    pub async fn apply_updates(
        &self,
        request: PluginUpdateApplyRequest,
    ) -> VibexResult<Vec<PluginMarketInstallResult>> {
        let mut results = Vec::new();
        for plugin_id in &request.plugin_ids {
            let connection = self.connection()?;
            let record =
                vibex_db::PluginRepository::get(&connection, plugin_id)?.ok_or_else(|| {
                    vibex_core::VibexError::validation(
                        "plugin_not_installed",
                        format!("the plugin `{plugin_id}` is not installed"),
                    )
                })?;
            let install = PluginMarketInstallRequest {
                entry_id: plugin_id.clone(),
                version: None,
                granted_permissions: if request.granted_permissions.is_empty() {
                    record.granted_permissions.clone()
                } else {
                    request.granted_permissions.clone()
                },
                consent_fingerprint: None,
                enable: record.enabled,
                auto_update: record.auto_update,
            };
            results.push(self.service().install(connection, &install).await?);
        }
        Ok(results)
    }

    /// Recovers an interrupted install.
    pub fn recover(&self) -> VibexResult<bool> {
        let outcome = self.service().recover(&self.connection()?)?;
        Ok(!matches!(outcome, vibex_plugin::RecoveryOutcome::None))
    }
}

fn index_urls() -> Vec<url::Url> {
    let configured = std::env::var("VIBEX_PLUGIN_INDEX").ok();
    let raw = configured.unwrap_or_else(|| DEFAULT_PLUGIN_INDEX.to_string());
    raw.split(',')
        .filter_map(|url| url::Url::parse(url.trim()).ok())
        .collect()
}

fn embedded_root_key() -> Option<VerifyingKey> {
    let encoded = option_env!("VIBEX_PLUGIN_ROOT_KEY")?;
    vibex_plugin::market::decode_verifying_key(encoded).ok()
}

/// Seeds the first-party plugin rows once per database in this process.
fn seed_once(
    database_path: &std::path::Path,
    service: &PluginService,
    connection: &rusqlite::Connection,
) {
    static SEEDED: OnceLock<Mutex<std::collections::HashSet<PathBuf>>> = OnceLock::new();
    let seeded = SEEDED.get_or_init(|| Mutex::new(std::collections::HashSet::new()));
    let Ok(mut seeded) = seeded.lock() else {
        return;
    };
    if !seeded.insert(database_path.to_path_buf()) {
        return;
    }
    let _ = service.seed_native_plugins(connection);
}

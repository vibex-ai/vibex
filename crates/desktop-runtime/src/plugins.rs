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
    InstalledPlugin, InstalledPluginStatus, PluginAutoUpdateRequest, PluginEnabledRequest,
    PluginLocalInstallRequest, PluginMarketDetail, PluginMarketDetailRequest,
    PluginMarketIndexRequest, PluginMarketIndexResponse, PluginMarketInstallRequest,
    PluginMarketInstallResult, PluginMarketSearchRequest, PluginMarketSearchResponse,
    PluginRollbackRequest, PluginUninstallRequest, PluginUpdateApplyRequest,
    PluginUpdateCheckRequest, PluginUpdateCheckResponse, PluginVersionPinRequest, VibexResult,
};
use vibex_db::{PluginRepository, open_database};
use vibex_plugin::{
    MarketCache, MarketCatalog, MarketConfig, MarketHttpClient, MarketUrlPolicy, NullRuntime,
    PluginError, PluginRuntime, PluginService, PluginStore,
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
        let (root_key, root_key_invalid) = match configured_root_key() {
            Ok(key) => (key, false),
            Err(error) => {
                eprintln!("vibex-plugin-market: the configured root key is invalid: {error}");
                (None, true)
            }
        };
        let allow_unverified = !root_key_invalid
            && allow_unverified_index(
                root_key.as_ref(),
                std::env::var(ALLOW_UNVERIFIED_INDEX_ENV).ok().as_deref(),
            );
        if !root_key_invalid && root_key.is_none() && !allow_unverified {
            // `service()` runs per plugin operation; warn once per process.
            static WARNED: OnceLock<()> = OnceLock::new();
            WARNED.get_or_init(|| {
                eprintln!(
                    "vibex-plugin-market: no root key; set {ROOT_KEY_ENV} to the index signer's public key, \
                     or {ALLOW_UNVERIFIED_INDEX_ENV}=1 to read a development index"
                );
            });
        }
        let config = MarketConfig {
            index_urls: index_urls(),
            root_public_key: root_key,
            github: Some(vibex_plugin::GithubTopicSource::new(MarketHttpClient::new(
                MarketUrlPolicy::Public,
            ))),
            cache: MarketCache::new(home.join("plugin-market")),
            index_ttl: vibex_plugin::market::INDEX_TTL,
            github_ttl: vibex_plugin::market::GITHUB_TTL,
            allow_unverified_index: allow_unverified,
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
        let service = self.service();
        let response = service.index(&request).await;
        // A refresh can carry a new revocation list; apply it to what is
        // already installed before the caller draws the catalog.
        if let Ok(connection) = self.connection() {
            let _ = service.apply_revocations(&connection);
        }
        Ok(response)
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

    pub fn set_pinned_version(
        &self,
        request: PluginVersionPinRequest,
    ) -> VibexResult<InstalledPlugin> {
        self.service().set_pinned_version(
            &self.connection()?,
            &request.plugin_id,
            request.version.as_deref(),
        )
    }

    pub fn rollback(&self, request: PluginRollbackRequest) -> VibexResult<InstalledPlugin> {
        self.service().rollback(
            &self.connection()?,
            &request.plugin_id,
            request.version.as_deref(),
        )
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
        for consent in &request.consents {
            let connection = self.connection()?;
            let record = vibex_db::PluginRepository::get(&connection, &consent.plugin_id)?
                .ok_or_else(|| {
                    vibex_core::VibexError::validation(
                        "plugin_not_installed",
                        format!("the plugin `{}` is not installed", consent.plugin_id),
                    )
                })?;
            let install = PluginMarketInstallRequest {
                entry_id: consent.plugin_id.clone(),
                // Install the exact version the sheet showed, not whatever the
                // catalog happens to call latest by the time the request lands.
                version: Some(consent.version.clone()),
                granted_permissions: consent.granted_permissions.clone(),
                consent_fingerprint: Some(consent.consent_fingerprint.clone()),
                enable: record.enabled,
                auto_update: record.auto_update,
            };
            results.push(self.service().install(connection, &install).await?);
        }
        Ok(results)
    }

    /// Refuses to load a plugin the market has revoked.
    ///
    /// The rail asks this before it posts a load to the UI thread, so a
    /// revoked script never starts; the durable record is switched to
    /// `Revoked` and the link is removed at the same time.
    pub fn ensure_loadable(&self, plugin_id: &str) -> VibexResult<()> {
        let connection = self.connection()?;
        let record = PluginRepository::get(&connection, plugin_id)?
            .filter(|record| record.deleted_at_ms.is_none())
            .ok_or_else(|| {
                vibex_core::VibexError::validation(
                    "plugin_not_installed",
                    format!("the plugin `{plugin_id}` is not installed"),
                )
            })?;
        let service = self.service();
        let Some(blocked) = service.market().blocked(plugin_id, &record.version) else {
            return Ok(());
        };
        let now = vibex_core::unix_timestamp_ms();
        let _ = self.runtime.unload(plugin_id);
        let _ = service.store().deactivate(plugin_id);
        PluginRepository::set_enabled(&connection, plugin_id, false, now)?;
        PluginRepository::set_status(
            &connection,
            plugin_id,
            InstalledPluginStatus::Revoked,
            Some(&blocked.reason),
            now,
        )?;
        Err(vibex_core::VibexError::from(PluginError::Blocked(
            plugin_id.to_string(),
            blocked.reason,
        )))
    }

    /// Applies updates for plugins that opted into automatic updates.
    ///
    /// Never widens permissions: an update that adds a key is left for the
    /// user, and a pinned or yanked version is skipped.
    pub async fn auto_update(&self) -> VibexResult<Vec<PluginMarketInstallResult>> {
        let connection = self.connection()?;
        let plugin_ids = PluginRepository::list(&connection)?
            .into_iter()
            .filter(|record| {
                record.kind == vibex_core::PluginKind::Script
                    && record.auto_update
                    && record.trust.allows_auto_update()
                    && record.pinned_version.is_none()
            })
            .map(|record| record.plugin_id)
            .collect::<Vec<_>>();
        if plugin_ids.is_empty() {
            return Ok(Vec::new());
        }
        let updates = self
            .service()
            .check_updates(
                self.connection()?,
                &PluginUpdateCheckRequest {
                    plugin_ids,
                    refresh: true,
                },
            )
            .await?;
        let mut installed = Vec::new();
        for update in updates.updates {
            if update.yanked || !update.permission_diff.added.is_empty() {
                continue;
            }
            let Some(fingerprint) = update.consent_fingerprint.clone() else {
                continue;
            };
            let connection = self.connection()?;
            let record =
                PluginRepository::get(&connection, &update.plugin_id)?.ok_or_else(|| {
                    vibex_core::VibexError::validation(
                        "plugin_not_installed",
                        format!("the plugin `{}` is not installed", update.plugin_id),
                    )
                })?;
            let request = PluginMarketInstallRequest {
                entry_id: update.plugin_id.clone(),
                version: Some(update.available_version.clone()),
                granted_permissions: update
                    .requested_permissions
                    .iter()
                    .map(vibex_core::PluginPermissionGrant::from)
                    .collect(),
                consent_fingerprint: Some(fingerprint),
                enable: record.enabled,
                auto_update: true,
            };
            // One bad package must not stop the rest of the batch.
            match self.service().install(connection, &request).await {
                Ok(result) => installed.push(result),
                Err(error) => eprintln!(
                    "vibex-plugin: auto-update of {} failed: {error}",
                    update.plugin_id
                ),
            }
        }
        Ok(installed)
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

/// The environment variable carrying the index signer's public key.
pub const ROOT_KEY_ENV: &str = "VIBEX_PLUGIN_ROOT_KEY";

/// The explicit development opt-in for an unsigned index.
pub const ALLOW_UNVERIFIED_INDEX_ENV: &str = "VIBEX_PLUGIN_ALLOW_UNVERIFIED_INDEX";

/// The root key, preferring a build-time value and falling back to the
/// developer's environment.
///
/// A configured but malformed key is an error rather than "no key": silently
/// treating it as absent is how a release ends up accepting anything.
fn configured_root_key() -> Result<Option<VerifyingKey>, String> {
    let compiled = option_env!("VIBEX_PLUGIN_ROOT_KEY").unwrap_or("");
    let encoded = if compiled.trim().is_empty() {
        match std::env::var(ROOT_KEY_ENV) {
            Ok(value) if !value.trim().is_empty() => value,
            _ => return Ok(None),
        }
    } else {
        compiled.to_string()
    };
    vibex_plugin::market::decode_verifying_key(&encoded)
        .map(Some)
        .map_err(|error| error.to_string())
}

/// Whether an unsigned index may be read.
///
/// Never when a root key exists; otherwise only with the explicit development
/// opt-in. The default is closed.
fn allow_unverified_index(root_key: Option<&VerifyingKey>, override_value: Option<&str>) -> bool {
    root_key.is_none()
        && override_value
            .is_some_and(|value| matches!(value.trim(), "1" | "true" | "TRUE" | "yes" | "YES"))
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

#[cfg(test)]
mod tests {
    use super::*;

    fn verifying_key() -> VerifyingKey {
        ed25519_dalek::SigningKey::from_bytes(&[7u8; 32]).verifying_key()
    }

    #[test]
    fn an_unsigned_index_needs_an_explicit_development_opt_in() {
        assert!(!allow_unverified_index(None, None));
        assert!(!allow_unverified_index(None, Some("0")));
        assert!(!allow_unverified_index(None, Some("")));
        assert!(allow_unverified_index(None, Some("1")));
        assert!(allow_unverified_index(None, Some("true")));
    }

    #[test]
    fn a_root_key_always_disables_the_unsigned_fallback() {
        let key = verifying_key();
        assert!(!allow_unverified_index(Some(&key), None));
        assert!(!allow_unverified_index(Some(&key), Some("1")));
    }
}

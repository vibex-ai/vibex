//! The plugin service: market, install, lifecycle and updates in one place.
//!
//! Everything the runtime calls lives here. Market documents are fetched
//! through [`MarketCatalog`], packages land through [`PluginStore`], durable
//! state goes through `vibex-db`, and the scripting host is reached only
//! through [`PluginRuntime`].

use std::sync::Arc;

use semver::Version;
use vibex_core::{
    InstalledPlugin, InstalledPluginStatus, PluginContribution, PluginKind, PluginListingSource,
    PluginMarketDetail, PluginMarketEntry, PluginMarketIndexRequest, PluginMarketIndexResponse,
    PluginMarketInstallRequest, PluginMarketInstallResult, PluginMarketSearchRequest,
    PluginMarketSearchResponse, PluginMarketVersion, PluginPermission, PluginPermissionDiff,
    PluginPermissionGrant, PluginSource, PluginTrust, PluginUpdate, PluginUpdateCheckRequest,
    PluginUpdateCheckResponse, VibexError, VibexResult, unix_timestamp_ms,
};
use vibex_db::{PluginRecord, PluginRepository, PluginVersionRecord};

use crate::error::PluginError;
use crate::fingerprint::{SCRIPT_EXECUTION_MODEL, consent_fingerprint_for};
use crate::install::{PendingInstall, PluginStore, RecoveryOutcome, extract_zip, verify_checksums};
use crate::manifest::{AUTHOR_MANIFEST_FILE, PluginManifest};
use crate::market::catalog::MarketCatalog;
use crate::market::index::PluginIndexEntry;
use crate::market::merge::search_entries;
use crate::native::{NativePluginRegistry, default_native_plugins};
use crate::runtime::{LoadedPlugin, PluginRuntime};
use crate::shell::{ShellManifestContext, write_shell_manifest};

/// How many versions of a script plugin are kept on disk.
pub const KEPT_VERSIONS: usize = 2;

/// The plugin service.
pub struct PluginService {
    market: MarketCatalog,
    store: PluginStore,
    runtime: Arc<dyn PluginRuntime>,
    app_version: Version,
    native: NativePluginRegistry,
}

impl PluginService {
    pub fn new(
        market: MarketCatalog,
        store: PluginStore,
        runtime: Arc<dyn PluginRuntime>,
        app_version: Version,
    ) -> Self {
        Self {
            market,
            store,
            runtime,
            app_version,
            native: default_native_plugins(),
        }
    }

    pub fn store(&self) -> &PluginStore {
        &self.store
    }

    pub fn market(&self) -> &MarketCatalog {
        &self.market
    }

    pub fn native_registry(&self) -> NativePluginRegistry {
        self.native
    }

    fn now_ms() -> i64 {
        unix_timestamp_ms()
    }

    // ---- catalog ----

    /// Fetches and merges the catalog, including first-party listings.
    pub async fn index(&self, request: &PluginMarketIndexRequest) -> PluginMarketIndexResponse {
        // The kill list rides along with the catalog: a plugin revoked while
        // this installation was offline must stop being installable as soon as
        // the catalog refreshes, not at some later maintenance pass.
        if request.refresh || self.market.revocations().is_none() {
            let _ = self.market.refresh_revocations().await;
        }
        let mut response = self.market.load(request).await;
        let mut bundled = self.native.market_entries(env!("CARGO_PKG_VERSION"));
        // First-party listings lead the catalog.
        bundled.extend(response.entries);
        response.entries = bundled;
        response
    }

    /// Searches the merged catalog, first-party listings included.
    pub async fn search(&self, request: &PluginMarketSearchRequest) -> PluginMarketSearchResponse {
        let index = self
            .index(&PluginMarketIndexRequest {
                refresh: false,
                include_github: true,
            })
            .await;
        let entries: Vec<PluginIndexEntry> = index
            .entries
            .into_iter()
            .map(|entry| PluginIndexEntry {
                entry,
                versions: Vec::new(),
                publisher_key_id: None,
                description: None,
                readme: None,
                screenshots: Vec::new(),
                reviews: None,
            })
            .collect();
        search_entries(&entries, request)
    }

    /// Reads one plugin's detail page.
    pub async fn detail(
        &self,
        plugin_id: &str,
        version: Option<&str>,
    ) -> VibexResult<PluginMarketDetail> {
        if let Some(descriptor) = self.native.get(plugin_id) {
            return Ok(PluginMarketDetail {
                entry: descriptor.to_entry(env!("CARGO_PKG_VERSION")),
                description: Some(descriptor.description.to_string()),
                readme: None,
                readme_bytes: 0,
                screenshots: Vec::new(),
                homepage: None,
                repository: None,
                license: Some("AGPL-3.0-or-later".to_string()),
                versions: Vec::new(),
                reviews: None,
                requested_permissions: Vec::new(),
                consent_fingerprint: None,
            });
        }
        self.market
            .detail(plugin_id, version)
            .await
            .map_err(VibexError::from)
    }

    // ---- first-party plugins ----

    /// Seeds the first-party plugin rows, preserving a user's enable choice.
    pub fn seed_native_plugins(&self, conn: &rusqlite::Connection) -> VibexResult<()> {
        let now = Self::now_ms();
        let version = env!("CARGO_PKG_VERSION");
        for descriptor in self.native.descriptors() {
            match PluginRepository::get(conn, descriptor.plugin_id)? {
                Some(existing) => {
                    let mut record = descriptor.to_record(version, existing.installed_at_ms);
                    record.enabled = existing.enabled;
                    record.status = existing.status;
                    record.updated_at_ms = now;
                    PluginRepository::upsert(conn, &record)?;
                }
                None => {
                    PluginRepository::upsert(conn, &descriptor.to_record(version, now))?;
                }
            }
            PluginRepository::replace_contributions(
                conn,
                descriptor.plugin_id,
                &descriptor.to_contributions(),
            )?;
        }
        Ok(())
    }

    /// Resolves a stored right-rail activity id to a first-party plugin.
    pub fn canonical_activity_id(&self, activity_id: &str) -> Option<&'static str> {
        self.native.canonical_activity_id(activity_id)
    }

    // ---- installed state ----

    /// Every managed plugin, first-party and script.
    pub fn list(&self, conn: &rusqlite::Connection) -> VibexResult<Vec<InstalledPlugin>> {
        let mut plugins: Vec<InstalledPlugin> = PluginRepository::list(conn)?
            .into_iter()
            .map(|record| record.to_installed())
            .collect();
        for plugin in &mut plugins {
            plugin.size_bytes = plugin
                .size_bytes
                .or_else(|| Some(self.store.data_size(&plugin.plugin_id)));
        }
        Ok(plugins)
    }

    /// Every contribution the enabled plugins make, in catalog order.
    ///
    /// A contribution from a plugin the user turned off is left out here rather
    /// than filtered by each surface: the rail draws what this returns and the
    /// Plugins page is the one place that decides.
    pub fn contributions(
        &self,
        conn: &rusqlite::Connection,
    ) -> VibexResult<Vec<PluginContribution>> {
        let mut contributions = Vec::new();
        for record in PluginRepository::list(conn)? {
            if !record.enabled {
                continue;
            }
            contributions.extend(
                PluginRepository::list_contributions(conn, &record.plugin_id)?
                    .into_iter()
                    .filter(|contribution| contribution.enabled),
            );
        }
        Ok(contributions)
    }

    /// Enables or disables a plugin.
    pub fn set_enabled(
        &self,
        conn: &rusqlite::Connection,
        plugin_id: &str,
        enabled: bool,
    ) -> VibexResult<InstalledPlugin> {
        let mut record = PluginRepository::get(conn, plugin_id)?
            .ok_or_else(|| PluginError::NotInstalled(plugin_id.to_string()))?;
        if record.kind == PluginKind::Native {
            record.enabled = enabled;
            record.status = if enabled {
                InstalledPluginStatus::Enabled
            } else {
                InstalledPluginStatus::Disabled
            };
            record.updated_at_ms = Self::now_ms();
            PluginRepository::upsert(conn, &record)?;
            return Ok(record.to_installed());
        }
        if enabled {
            // Re-enabling reads the version from the record, not the link: a
            // disabled plugin has no link, which is the point of disabling it.
            let version = record.version.clone();
            if !self.store.has_version(plugin_id, &version) {
                return Err(PluginError::NotInstalled(plugin_id.to_string()).into());
            }
            if let Some(blocked) = self.market.blocked(plugin_id, &version) {
                let now = Self::now_ms();
                let _ = self.runtime.unload(plugin_id);
                let _ = self.store.deactivate(plugin_id);
                PluginRepository::set_enabled(conn, plugin_id, false, now)?;
                PluginRepository::set_status(
                    conn,
                    plugin_id,
                    InstalledPluginStatus::Revoked,
                    Some(&blocked.reason),
                    now,
                )?;
                return Err(PluginError::Blocked(plugin_id.to_string(), blocked.reason).into());
            }
            // The host discovers plugins through `plugins/<id>`, so the link
            // has to exist before the script is loaded. Put it back first and
            // remove it again if the load fails.
            self.store
                .activate(plugin_id, &version)
                .map_err(VibexError::from)?;
            let loaded = match self.loaded_plugin_with_grants(&record) {
                Ok(loaded) => loaded,
                Err(error) => {
                    let _ = self.store.deactivate(plugin_id);
                    return Err(error);
                }
            };
            if let Err(error) = self.runtime.load(&loaded) {
                let _ = self.store.deactivate(plugin_id);
                return Err(error.into());
            }
            PluginRepository::set_active(conn, plugin_id, &version)?;
            PluginRepository::set_status(
                conn,
                plugin_id,
                InstalledPluginStatus::Enabled,
                None,
                Self::now_ms(),
            )?;
        } else {
            // A failed unload must not trap the user in an enabled state: the
            // link and the durable switch still go off. If the host is gone,
            // there is no script left to unload anyway.
            if let Err(error) = self.runtime.unload(plugin_id) {
                eprintln!("vibex-plugin: unload failed for {plugin_id}: {error}");
            }
            self.store.deactivate(plugin_id).map_err(VibexError::from)?;
            PluginRepository::set_status(
                conn,
                plugin_id,
                InstalledPluginStatus::Disabled,
                None,
                Self::now_ms(),
            )?;
        }
        PluginRepository::set_enabled(conn, plugin_id, enabled, Self::now_ms())?;
        let record = PluginRepository::get(conn, plugin_id)?
            .ok_or_else(|| PluginError::NotInstalled(plugin_id.to_string()))?;
        Ok(record.to_installed())
    }

    /// Sets a plugin's auto-update flag.
    pub fn set_auto_update(
        &self,
        conn: &rusqlite::Connection,
        plugin_id: &str,
        auto_update: bool,
    ) -> VibexResult<InstalledPlugin> {
        let record = PluginRepository::get(conn, plugin_id)?
            .ok_or_else(|| PluginError::NotInstalled(plugin_id.to_string()))?;
        if record.kind == PluginKind::Native {
            return Err(PluginError::NotUninstallable(plugin_id.to_string()).into());
        }
        PluginRepository::set_auto_update(conn, plugin_id, auto_update, Self::now_ms())?;
        let record = PluginRepository::get(conn, plugin_id)?
            .ok_or_else(|| PluginError::NotInstalled(plugin_id.to_string()))?;
        Ok(record.to_installed())
    }

    /// Uninstalls a script plugin, optionally removing its data.
    pub fn uninstall(
        &self,
        conn: &rusqlite::Connection,
        plugin_id: &str,
        remove_data: bool,
    ) -> VibexResult<()> {
        let record = PluginRepository::get(conn, plugin_id)?
            .ok_or_else(|| PluginError::NotInstalled(plugin_id.to_string()))?;
        if record.kind == PluginKind::Native || !record.uninstallable {
            return Err(PluginError::NotUninstallable(plugin_id.to_string()).into());
        }
        let _ = self.runtime.unload(plugin_id);
        self.store.deactivate(plugin_id).map_err(VibexError::from)?;
        let _ = self.store.remove_version(plugin_id, &record.version);
        for version in PluginRepository::list_versions(conn, plugin_id)? {
            let _ = self.store.remove_version(plugin_id, &version.version);
            PluginRepository::delete_version(conn, plugin_id, &version.version)?;
        }
        if remove_data {
            self.store
                .remove_data(plugin_id)
                .map_err(VibexError::from)?;
        }
        PluginRepository::soft_delete(conn, plugin_id, Self::now_ms())
    }

    // ---- install ----

    /// Installs a plugin from the catalog.
    pub async fn install(
        &self,
        conn: rusqlite::Connection,
        request: &PluginMarketInstallRequest,
    ) -> VibexResult<PluginMarketInstallResult> {
        let index = self.market.load(&PluginMarketIndexRequest::default()).await;
        let entry = index
            .entries
            .iter()
            .find(|entry| entry.id == request.entry_id)
            .cloned()
            .ok_or_else(|| PluginError::NotFound(request.entry_id.clone()))?;
        let catalog_entry = self
            .store_entry(&request.entry_id)
            .ok_or_else(|| PluginError::NotFound(request.entry_id.clone()))?;
        let version = match &request.version {
            Some(version) => catalog_entry
                .versions
                .iter()
                .find(|candidate| &candidate.version == version)
                .cloned(),
            None => catalog_entry
                .versions
                .iter()
                .find(|candidate| !candidate.yanked)
                .cloned(),
        }
        .ok_or_else(|| PluginError::NotFound(request.entry_id.clone()))?;

        if version.yanked {
            return Err(PluginError::Yanked(version.version.clone()).into());
        }
        if !entry.installable_here() {
            return Err(PluginError::HostTooOld {
                required: entry
                    .compatibility
                    .min_vibex
                    .clone()
                    .unwrap_or_else(|| "unknown".to_string()),
                current: self.app_version.to_string(),
            }
            .into());
        }
        if let Some(required) = &version.min_vibex
            && let Ok(requirement) = semver::VersionReq::parse(required)
            && !requirement.matches(&self.app_version)
        {
            return Err(PluginError::HostTooOld {
                required: required.clone(),
                current: self.app_version.to_string(),
            }
            .into());
        }
        if let Some(blocked) = self.market.blocked(&entry.id, &version.version) {
            return Err(PluginError::Blocked(entry.id.clone(), blocked.reason).into());
        }

        // Consent: the fingerprint the UI approved must be this version's, and
        // every permission must be named explicitly. A missing fingerprint is
        // an unapproved install, not a reason to grant everything.
        let expected = consent_fingerprint_for(
            &entry.id,
            &version.version,
            SCRIPT_EXECUTION_MODEL,
            &version.permissions,
        );
        require_consent(&expected, request.consent_fingerprint.as_deref())?;
        // Store only grants the reviewed version actually asks for. A stale
        // grant from a previous version must not widen the new one.
        let granted: Vec<PluginPermissionGrant> = request
            .granted_permissions
            .iter()
            .filter(|grant| {
                version.permissions.iter().any(|permission| {
                    permission.key == grant.key && permission.scope == grant.scope
                })
            })
            .cloned()
            .collect();
        if let Some(missing) = missing_grant(&version.permissions, &granted) {
            return Err(PluginError::PermissionDenied(missing.key.clone()).into());
        }

        let bytes = self.download_package(&entry, &version).await?;
        // A publisher signature is verified against the key the signed index
        // vouches for; only then does the install count as `Verified`.
        let mut signature_status = if entry.listing_source == PluginListingSource::Index {
            "verified".to_string()
        } else {
            "unsigned".to_string()
        };
        let mut trusted = entry.trust;
        if let Some(signature) = &version.signature {
            let keys = self
                .market
                .config()
                .cache
                .read_publisher_keys()
                .unwrap_or_default();
            let key = keys
                .iter()
                .find(|key| key.key_id == signature.key_id)
                .ok_or(PluginError::SignatureInvalid)?;
            crate::market::index::verify_package_signature(
                &key.public_key,
                &signature.signature,
                &bytes,
            )
            .map_err(VibexError::from)?;
            signature_status = "publisher-verified".to_string();
            if entry.listing_source == PluginListingSource::Index
                && trusted != PluginTrust::Official
            {
                trusted = PluginTrust::Verified;
            }
        }
        let source = PluginSource::Market;
        self.finish_install(
            &conn,
            &entry,
            &version,
            &bytes,
            source,
            trusted,
            None,
            &signature_status,
            &granted,
            &expected,
            request.enable,
            request.auto_update,
        )
    }

    /// Installs a plugin from a local package file.
    pub fn install_from_file(
        &self,
        conn: &rusqlite::Connection,
        path: &str,
        enable: bool,
    ) -> VibexResult<PluginMarketInstallResult> {
        let bytes = std::fs::read(path).map_err(|error| {
            VibexError::storage(
                "plugin_package_read_failed",
                format!("the plugin package could not be read: {error}"),
            )
        })?;
        let manifest = crate::install::read_package_manifest(&bytes).map_err(VibexError::from)?;
        let version = PluginMarketVersion {
            version: manifest.version.clone(),
            published_at: None,
            size_bytes: Some(bytes.len() as u64),
            sha256: crate::install::sha256_hex(&bytes),
            sha512: None,
            url: path.to_string(),
            changelog: None,
            min_vibex: manifest.vibex.clone(),
            min_shell: manifest.shell_version.clone(),
            permissions: manifest.permissions(),
            platforms: Vec::new(),
            yanked: false,
            yanked_reason: None,
            signature: None,
            provenance: None,
            review: None,
        };
        let entry = PluginMarketEntry {
            id: manifest.id.clone(),
            name: manifest.name.clone(),
            kind: PluginKind::Script,
            bundled: false,
            listing_source: PluginListingSource::Github,
            summary: manifest.description.clone(),
            icon_url: None,
            publisher: vibex_core::PluginPublisher::from_login("local"),
            categories: manifest.normalized_categories(),
            tags: Vec::new(),
            latest_version: Some(manifest.version.clone()),
            installs: 0,
            rating: None,
            updated_at: None,
            license: Some(manifest.license.clone()),
            homepage: manifest.homepage.clone(),
            repository: manifest.repository.clone(),
            trust: PluginTrust::Unverified,
            permissions: manifest.permissions(),
            compatibility: Default::default(),
            size_bytes: Some(bytes.len() as u64),
            featured: false,
            i18n: Default::default(),
        };
        let granted: Vec<PluginPermissionGrant> = version
            .permissions
            .iter()
            .map(PluginPermissionGrant::from)
            .collect();
        let fingerprint = consent_fingerprint_for(
            &entry.id,
            &version.version,
            SCRIPT_EXECUTION_MODEL,
            &version.permissions,
        );
        self.finish_install(
            conn,
            &entry,
            &version,
            &bytes,
            PluginSource::Local,
            PluginTrust::Unverified,
            None,
            "unsigned",
            &granted,
            &fingerprint,
            enable,
            false,
        )
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_install(
        &self,
        conn: &rusqlite::Connection,
        entry: &PluginMarketEntry,
        version: &PluginMarketVersion,
        bytes: &[u8],
        source: PluginSource,
        trust: PluginTrust,
        index_id: Option<String>,
        signature_status: &str,
        granted: &[PluginPermissionGrant],
        fingerprint: &str,
        enable: bool,
        auto_update: bool,
    ) -> VibexResult<PluginMarketInstallResult> {
        let existing = PluginRepository::get(conn, &entry.id)?;
        let previous_record = existing
            .clone()
            .filter(|record| record.deleted_at_ms.is_none());
        let previous_version = previous_record
            .as_ref()
            .map(|record| record.version.clone());
        let created = previous_record.is_none();

        // Verify and unpack into staging.
        verify_checksums(bytes, &version.sha256, version.sha512.as_deref())
            .map_err(VibexError::from)?;
        let staging = self
            .store
            .create_staging(&entry.id)
            .map_err(PluginError::from)
            .map_err(VibexError::from)?;
        let extract_result = (|| -> Result<(), PluginError> {
            extract_zip(bytes, &staging)?;
            crate::install::flatten_single_root(&staging)?;
            if !staging.join(AUTHOR_MANIFEST_FILE).is_file() {
                return Err(PluginError::MissingManifest);
            }
            let manifest = PluginManifest::from_file(&staging.join(AUTHOR_MANIFEST_FILE))?;
            if manifest.id != entry.id {
                return Err(PluginError::Manifest(
                    crate::error::ManifestError::InvalidId(manifest.id.clone()),
                ));
            }
            write_shell_manifest(
                &staging,
                &manifest,
                &ShellManifestContext {
                    plugin_dir: Some(staging.clone()),
                    data_dir: Some(self.store.data_dir(&entry.id)),
                    workspace_root: None,
                    home_dir: None,
                },
            )
            .map_err(PluginError::Io)?;
            Ok(())
        })();
        if let Err(error) = extract_result {
            let _ = std::fs::remove_dir_all(&staging);
            return Err(VibexError::from(error));
        }

        let size = crate::install::directory_size(&staging);
        let destination = self
            .store
            .publish_version(&entry.id, &version.version, &staging)
            .map_err(VibexError::from)?;
        let install_dir = destination.to_string_lossy().to_string();
        let data_dir = self.store.data_dir(&entry.id);
        std::fs::create_dir_all(&data_dir)
            .map_err(|error| VibexError::storage("plugin_data_dir_failed", error.to_string()))?;

        // Publish, mark, then move the link.
        self.store
            .write_pending(&PendingInstall {
                plugin_id: entry.id.clone(),
                previous_version: previous_version.clone(),
                new_version: version.version.clone(),
                started_at_ms: Self::now_ms().max(0) as u64,
            })
            .map_err(VibexError::from)?;

        let now = Self::now_ms();
        let mut record = existing.unwrap_or_else(|| {
            PluginRecord::new(
                entry.id.clone(),
                entry.name.clone(),
                version.version.clone(),
                now,
            )
        });
        record.display_name = entry.name.clone();
        record.version = version.version.clone();
        record.kind = PluginKind::Script;
        record.source = source;
        record.bundled = false;
        record.uninstallable = true;
        record.publisher_id = Some(entry.publisher.id.clone());
        record.publisher_name = Some(entry.publisher.display_name.clone());
        record.publisher_verified = entry.publisher.verified;
        record.trust = trust;
        record.index_id = index_id;
        record.install_dir = Some(install_dir);
        record.data_dir = Some(data_dir.to_string_lossy().to_string());
        record.auto_update = auto_update && trust.allows_auto_update();
        record.enabled = enable;
        record.status = if enable {
            InstalledPluginStatus::Enabled
        } else {
            InstalledPluginStatus::Disabled
        };
        record.permissions = version.permissions.clone();
        record.granted_permissions = granted.to_vec();
        record.consent_fingerprint = Some(fingerprint.to_string());
        record.min_vibex = version.min_vibex.clone();
        record.size_bytes = Some(size);
        record.last_error = None;
        record.updated_at_ms = now;
        record.deleted_at_ms = None;
        PluginRepository::upsert(conn, &record)?;
        PluginRepository::upsert_version(
            conn,
            &PluginVersionRecord {
                plugin_id: entry.id.clone(),
                version: version.version.clone(),
                path: record.install_dir.clone().unwrap_or_default(),
                sha256: version.sha256.clone(),
                sha512: version.sha512.clone(),
                signature_key_id: version.signature.as_ref().map(|s| s.key_id.clone()),
                signature_status: signature_status.to_string(),
                size_bytes: Some(size),
                active: enable,
                installed_at_ms: now,
            },
        )?;
        PluginRepository::replace_grants(conn, &entry.id, granted, &version.permissions, now)?;
        PluginRepository::replace_contributions(
            conn,
            &entry.id,
            &contributions_for(entry, &version.permissions),
        )?;
        PluginRepository::set_active_version(
            conn,
            &entry.id,
            &version.version,
            record.install_dir.as_deref(),
            record.data_dir.as_deref(),
            Some(size),
            now,
        )?;

        if enable {
            self.store
                .activate(&entry.id, &version.version)
                .map_err(VibexError::from)?;
            let stored = PluginRepository::get(conn, &entry.id)?
                .ok_or_else(|| PluginError::NotInstalled(entry.id.clone()))?;
            let loaded = self.loaded_plugin_with_grants(&stored)?;
            if let Err(error) = self.runtime.load(&loaded) {
                if let Err(rollback) = self.rollback_failed_load(
                    conn,
                    entry,
                    previous_record.as_ref(),
                    &version.version,
                ) {
                    eprintln!(
                        "vibex-plugin: rolling back {} after a failed load also failed: {rollback}",
                        entry.id
                    );
                    let _ = PluginRepository::set_status(
                        conn,
                        &entry.id,
                        InstalledPluginStatus::Broken,
                        Some(&error.to_string()),
                        Self::now_ms(),
                    );
                }
                return Err(VibexError::from(error));
            }
        } else {
            // Keep the version directory: a disabled plugin still has to be
            // enable-able later. Only the link goes away.
            self.store.deactivate(&entry.id).map_err(VibexError::from)?;
        }

        self.store.clear_pending().map_err(VibexError::from)?;
        let kept = PluginRepository::list_versions(conn, &entry.id)?;
        let keep: Vec<String> = kept
            .iter()
            .take(KEPT_VERSIONS)
            .map(|version| version.version.clone())
            .collect();
        let _ = self.store.prune_versions(&entry.id, &keep);

        let plugin = PluginRepository::get(conn, &entry.id)?
            .ok_or_else(|| PluginError::NotInstalled(entry.id.clone()))?
            .to_installed();
        Ok(PluginMarketInstallResult { plugin, created })
    }

    /// Puts the previous version back after a new version failed to load.
    ///
    /// The install wrote the new record, grants and contributions before it
    /// tried the load. All three are restored with the old version, and a
    /// fresh install (no previous version) is hidden instead of being left as
    /// a half-installed row.
    fn rollback_failed_load(
        &self,
        conn: &rusqlite::Connection,
        entry: &PluginMarketEntry,
        previous: Option<&PluginRecord>,
        failed_version: &str,
    ) -> VibexResult<()> {
        let _ = self.runtime.unload(&entry.id);
        let now = Self::now_ms();
        if let Some(previous) = previous {
            if self.store.has_version(&entry.id, &previous.version) {
                self.store
                    .activate(&entry.id, &previous.version)
                    .map_err(VibexError::from)?;
                PluginRepository::upsert(conn, previous)?;
                PluginRepository::set_active(conn, &entry.id, &previous.version)?;
                PluginRepository::replace_grants(
                    conn,
                    &entry.id,
                    &previous.granted_permissions,
                    &previous.permissions,
                    now,
                )?;
                PluginRepository::replace_contributions(
                    conn,
                    &entry.id,
                    &contributions_for(entry, &previous.permissions),
                )?;
                if previous.version != failed_version {
                    let _ = self.store.remove_version(&entry.id, failed_version);
                    let _ = PluginRepository::delete_version(conn, &entry.id, failed_version);
                }
                self.store.clear_pending().map_err(VibexError::from)?;
                if previous.enabled
                    && let Err(error) = self
                        .loaded_plugin_with_grants(previous)
                        .and_then(|loaded| self.runtime.load(&loaded).map_err(VibexError::from))
                {
                    PluginRepository::set_status(
                        conn,
                        &entry.id,
                        InstalledPluginStatus::Broken,
                        Some(&error.to_string()),
                        now,
                    )?;
                }
                return Ok(());
            }
            // The old version is gone; the plugin cannot be brought back.
            let _ = self.store.deactivate(&entry.id);
            let _ = self.store.remove_version(&entry.id, failed_version);
            let _ = PluginRepository::delete_version(conn, &entry.id, failed_version);
            self.store.clear_pending().map_err(VibexError::from)?;
            PluginRepository::set_status(
                conn,
                &entry.id,
                InstalledPluginStatus::Broken,
                Some("the previous version is missing"),
                now,
            )?;
            return Ok(());
        }

        // A fresh install that never loaded is not a plugin the user has.
        let _ = self.store.deactivate(&entry.id);
        let _ = self.store.remove_version(&entry.id, failed_version);
        let _ = PluginRepository::delete_version(conn, &entry.id, failed_version);
        PluginRepository::soft_delete(conn, &entry.id, now)?;
        self.store.clear_pending().map_err(VibexError::from)?;
        Ok(())
    }

    // ---- updates ----

    /// Finds available updates for the installed script plugins.
    pub async fn check_updates(
        &self,
        conn: rusqlite::Connection,
        request: &PluginUpdateCheckRequest,
    ) -> VibexResult<PluginUpdateCheckResponse> {
        let index = self
            .market
            .load(&PluginMarketIndexRequest {
                refresh: request.refresh,
                include_github: true,
            })
            .await;
        let installed = PluginRepository::list(&conn)?;
        let now = Self::now_ms();
        let mut updates = Vec::new();
        for record in installed {
            if record.kind == PluginKind::Native
                || (!request.plugin_ids.is_empty()
                    && !request.plugin_ids.contains(&record.plugin_id))
            {
                continue;
            }
            let Some(entry) = index
                .entries
                .iter()
                .find(|entry| entry.id == record.plugin_id)
            else {
                continue;
            };
            let Some(latest) = entry.latest_version.as_deref() else {
                continue;
            };
            if !is_newer(latest, &record.version) {
                PluginRepository::set_checked(&conn, &record.plugin_id, now)?;
                continue;
            }
            let diff = self.permission_diff_for(
                &record.plugin_id,
                latest,
                &record.granted_permissions,
                record.consent_fingerprint.as_deref(),
            );
            if !diff.is_empty() {
                PluginRepository::set_status(
                    &conn,
                    &record.plugin_id,
                    InstalledPluginStatus::UpdateAvailable,
                    record.last_error.as_deref(),
                    now,
                )?;
            }
            let requested_permissions = self
                .store_entry(&record.plugin_id)
                .and_then(|entry| {
                    entry
                        .versions
                        .iter()
                        .find(|candidate| candidate.version == latest)
                        .map(|candidate| candidate.permissions.clone())
                })
                .unwrap_or_else(|| entry.permissions.clone());
            let consent_fingerprint = consent_fingerprint_for(
                &record.plugin_id,
                latest,
                SCRIPT_EXECUTION_MODEL,
                &requested_permissions,
            );
            updates.push(PluginUpdate {
                plugin_id: record.plugin_id.clone(),
                installed_version: record.version.clone(),
                available_version: latest.to_string(),
                permission_diff: diff,
                requested_permissions,
                consent_fingerprint: Some(consent_fingerprint),
                yanked: self.market.blocked(&record.plugin_id, latest).is_some(),
                changelog: None,
            });
            PluginRepository::set_checked(&conn, &record.plugin_id, now)?;
        }
        Ok(PluginUpdateCheckResponse { updates })
    }

    fn permission_diff_for(
        &self,
        plugin_id: &str,
        version: &str,
        granted: &[PluginPermissionGrant],
        installed_fingerprint: Option<&str>,
    ) -> PluginPermissionDiff {
        let Some(entry) = self.store_entry(plugin_id) else {
            return PluginPermissionDiff::default();
        };
        let Some(candidate) = entry.versions.iter().find(|c| c.version == version) else {
            return PluginPermissionDiff::default();
        };
        let added: Vec<PluginPermission> = candidate
            .permissions
            .iter()
            .filter(|permission| !granted.iter().any(|grant| grant.key == permission.key))
            .cloned()
            .collect();
        let removed: Vec<PluginPermission> = granted
            .iter()
            .filter(|grant| {
                !candidate
                    .permissions
                    .iter()
                    .any(|permission| permission.key == grant.key)
            })
            .map(|grant| PluginPermission::new(&grant.key, Default::default()))
            .collect();
        let expected = consent_fingerprint_for(
            plugin_id,
            version,
            SCRIPT_EXECUTION_MODEL,
            &candidate.permissions,
        );
        let fingerprint_changed = installed_fingerprint.is_some_and(|current| current != expected);
        PluginPermissionDiff {
            added,
            removed,
            fingerprint_changed,
        }
    }

    // ---- recovery ----

    /// Recovers an interrupted install, then applies the revocation list.
    pub fn recover(&self, conn: &rusqlite::Connection) -> VibexResult<RecoveryOutcome> {
        let outcome = self.store.recover_pending().map_err(VibexError::from)?;
        let now = Self::now_ms();
        match &outcome {
            RecoveryOutcome::Completed { plugin_id, version }
            | RecoveryOutcome::RolledBack { plugin_id, version } => {
                // Recompute the paths: a recovery must not wipe the record's
                // install/data directories (the old code passed None and left
                // the plugin unloadable from the rail).
                let install_dir = self
                    .store
                    .version_dir(plugin_id, version)
                    .to_string_lossy()
                    .to_string();
                let data_dir = self.store.data_dir(plugin_id).to_string_lossy().to_string();
                let record = PluginRepository::get(conn, plugin_id)?;
                let wanted = record.as_ref().is_some_and(|record| record.enabled);
                if !wanted {
                    // A disabled plugin may have crashed mid-install after the
                    // record was written; the link must follow the switch.
                    let _ = self.store.deactivate(plugin_id);
                }
                PluginRepository::set_active(conn, plugin_id, version)?;
                let status = if wanted && self.store.current_version(plugin_id).is_some() {
                    InstalledPluginStatus::Enabled
                } else {
                    InstalledPluginStatus::Disabled
                };
                PluginRepository::set_status(conn, plugin_id, status, None, now)?;
                PluginRepository::set_active_version(
                    conn,
                    plugin_id,
                    version,
                    Some(&install_dir),
                    Some(&data_dir),
                    record.as_ref().and_then(|record| record.size_bytes),
                    now,
                )?;
            }
            RecoveryOutcome::Broken { plugin_id } => {
                PluginRepository::set_status(
                    conn,
                    plugin_id,
                    InstalledPluginStatus::Broken,
                    Some("the install could not be recovered"),
                    now,
                )?;
            }
            RecoveryOutcome::None => {}
        }
        self.enforce_revocations(conn, now)?;
        Ok(outcome)
    }

    /// Applies the cached revocation list to installed plugins.
    ///
    /// Called on startup and again after the catalog refreshes, so a plugin
    /// revoked while the app was running is switched off without waiting for
    /// the next launch.
    pub fn apply_revocations(&self, conn: &rusqlite::Connection) -> VibexResult<()> {
        self.enforce_revocations(conn, Self::now_ms())
    }

    /// Turns off every installed version the market has revoked.
    fn enforce_revocations(&self, conn: &rusqlite::Connection, now: i64) -> VibexResult<()> {
        for record in PluginRepository::list(conn)? {
            if record.kind == PluginKind::Native {
                continue;
            }
            let Some(blocked) = self.market.blocked(&record.plugin_id, &record.version) else {
                continue;
            };
            let _ = self.runtime.unload(&record.plugin_id);
            let _ = self.store.deactivate(&record.plugin_id);
            PluginRepository::set_enabled(conn, &record.plugin_id, false, now)?;
            PluginRepository::set_status(
                conn,
                &record.plugin_id,
                InstalledPluginStatus::Revoked,
                Some(&blocked.reason),
                now,
            )?;
        }
        Ok(())
    }

    // ---- helpers ----

    fn store_entry(&self, plugin_id: &str) -> Option<PluginIndexEntry> {
        self.market.config().cache.find_entry(plugin_id)
    }

    fn loaded_plugin(&self, plugin_id: &str, version: &str) -> VibexResult<LoadedPlugin> {
        Ok(LoadedPlugin {
            plugin_id: plugin_id.to_string(),
            version: version.to_string(),
            directory: self.store.version_dir(plugin_id, version),
            data_directory: self.store.data_dir(plugin_id),
            granted_permissions: Vec::new(),
        })
    }

    /// The record plus the permissions the user approved, ready for the host.
    ///
    /// The installer asked for consent; the host asks once more, with the
    /// approved list in hand, because the version directory is ordinary files
    /// a third party could have rewritten since.
    fn loaded_plugin_with_grants(&self, record: &PluginRecord) -> VibexResult<LoadedPlugin> {
        let mut loaded = self.loaded_plugin(&record.plugin_id, &record.version)?;
        loaded.granted_permissions = record
            .granted_permissions
            .iter()
            .map(|grant| grant.key.clone())
            .collect();
        if loaded.granted_permissions.is_empty() {
            // A record that predates consent storage has not been approved for
            // anything; the manifest's own request is the only honest answer.
            loaded.granted_permissions = record
                .permissions
                .iter()
                .map(|permission| permission.key.clone())
                .collect();
        }
        Ok(loaded)
    }

    async fn download_package(
        &self,
        entry: &PluginMarketEntry,
        version: &PluginMarketVersion,
    ) -> VibexResult<Vec<u8>> {
        let client = self.market.client();
        let (url, expected_sha256) = if let Some(repo) = version.url.strip_prefix("github:") {
            let commit = crate::install::resolve_github_commit(client, repo)
                .await
                .map_err(VibexError::from)?;
            (
                format!("https://codeload.github.com/{repo}/zip/{commit}"),
                String::new(),
            )
        } else {
            (version.url.clone(), version.sha256.clone())
        };
        let url = url::Url::parse(&url).map_err(|error| {
            VibexError::validation("plugin_package_url_invalid", error.to_string())
        })?;
        let bytes = client
            .fetch_bytes(&url, vibex_core::MAX_PLUGIN_PACKAGE_BYTES)
            .await
            .map_err(VibexError::from)?;
        if bytes.len() as u64 > vibex_core::MAX_PLUGIN_PACKAGE_BYTES {
            return Err(PluginError::ResponseTooLarge.into());
        }
        verify_checksums(&bytes, &expected_sha256, version.sha512.as_deref())
            .map_err(VibexError::from)?;
        let _ = entry;
        Ok(bytes)
    }
}

/// The contribution rows an installed script plugin owns.
///
/// A script that asked for `ui.panel` contributes one right-rail activity and
/// nothing else: that is the surface the host can mount a script view in. A
/// package that asked for no panel contributes nothing, so installing a
/// background helper does not put a dead button on the bar.
fn contributions_for(
    entry: &PluginMarketEntry,
    permissions: &[PluginPermission],
) -> Vec<PluginContribution> {
    if !permissions
        .iter()
        .any(|permission| permission.key == "ui.panel")
    {
        return Vec::new();
    }
    vec![PluginContribution {
        plugin_id: entry.id.clone(),
        kind: crate::native::RIGHT_RAIL_ACTIVITY.to_string(),
        contribution_id: format!("rail_plugin_{}", entry.id),
        title: Some(entry.name.clone()),
        enabled: true,
    }]
}

/// Refuses an install whose consent fingerprint is absent or stale.
fn require_consent(expected: &str, provided: Option<&str>) -> Result<(), PluginError> {
    match provided {
        Some(provided) if provided == expected => Ok(()),
        Some(_) => Err(PluginError::PermissionDenied(
            "the approved consent fingerprint does not match the package".to_string(),
        )),
        None => Err(PluginError::PermissionDenied(
            "the install has not been approved; review its permissions first".to_string(),
        )),
    }
}

/// The first permission the user did not explicitly grant.
fn missing_grant<'a>(
    permissions: &'a [PluginPermission],
    granted: &[PluginPermissionGrant],
) -> Option<&'a PluginPermission> {
    permissions.iter().find(|permission| {
        !granted
            .iter()
            .any(|grant| grant.key == permission.key && grant.scope == permission.scope)
    })
}

/// Whether `candidate` is a newer semantic version than `current`.
pub fn is_newer(candidate: &str, current: &str) -> bool {
    match (Version::parse(candidate), Version::parse(current)) {
        (Ok(candidate), Ok(current)) => candidate > current,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;
    use vibex_core::PluginPermissionRisk;

    use crate::install::PluginStore;
    use crate::market::index::{PluginRevocations, RevokedPlugin};
    use crate::market::{MarketCache, MarketConfig, MarketHttpClient, MarketUrlPolicy};
    use crate::runtime::NullRuntime;

    #[test]
    fn an_install_needs_a_matching_consent_fingerprint() {
        assert!(require_consent("abc", None).is_err());
        assert!(require_consent("abc", Some("def")).is_err());
        assert!(require_consent("abc", Some("abc")).is_ok());
    }

    #[test]
    fn every_requested_permission_needs_an_explicit_grant() {
        let permissions = vec![
            PluginPermission::new("ui.panel", PluginPermissionRisk::Low),
            PluginPermission::new("net.fetch", PluginPermissionRisk::Medium),
        ];
        assert!(missing_grant(&permissions, &[]).is_some());
        let one = vec![PluginPermissionGrant::from(&permissions[0])];
        assert_eq!(
            missing_grant(&permissions, &one).map(|permission| permission.key.as_str()),
            Some("net.fetch")
        );
        let both: Vec<PluginPermissionGrant> = permissions
            .iter()
            .map(PluginPermissionGrant::from)
            .collect();
        assert!(missing_grant(&permissions, &both).is_none());

        let scoped = PluginPermission {
            key: "net.fetch".to_string(),
            scope: Some("api.example.com".to_string()),
            risk: PluginPermissionRisk::Medium,
            reason: None,
        };
        let wrong_scope = vec![PluginPermissionGrant {
            key: "net.fetch".to_string(),
            scope: Some("evil.example.com".to_string()),
        }];
        assert!(missing_grant(std::slice::from_ref(&scoped), &wrong_scope).is_some());
    }

    #[test]
    fn newer_versions_are_ordered_semantically() {
        assert!(is_newer("1.2.0", "1.1.9"));
        assert!(!is_newer("1.1.0", "1.1.0"));
        assert!(!is_newer("1.0.0", "1.1.0"));
        assert!(!is_newer("not-a-version", "1.0.0"));
    }

    fn package() -> Vec<u8> {
        let manifest = serde_json::json!({
            "schemaVersion": 1,
            "id": "acme.widgets",
            "name": "Widgets",
            "version": "1.0.0",
            "license": "MIT",
            "entry": "main.js",
            "capabilities": { "panel": true, "storage": true }
        });
        let mut buffer = Vec::new();
        {
            let mut writer = zip::ZipWriter::new(std::io::Cursor::new(&mut buffer));
            let options = zip::write::SimpleFileOptions::default();
            writer.start_file("vibex-plugin.json", options).unwrap();
            writer
                .write_all(&serde_json::to_vec(&manifest).unwrap())
                .unwrap();
            writer.start_file("main.js", options).unwrap();
            writer.write_all(b"export const name = 'widgets';").unwrap();
            writer.finish().unwrap();
        }
        buffer
    }

    fn service(directory: &std::path::Path) -> PluginService {
        let cache = MarketCache::new(directory.join("market"));
        let catalog = MarketCatalog::new(
            MarketHttpClient::new(MarketUrlPolicy::Testing),
            MarketConfig::empty(cache),
        );
        PluginService::new(
            catalog,
            PluginStore::new(directory),
            Arc::new(NullRuntime),
            Version::parse("0.1.2").unwrap(),
        )
    }

    fn connection() -> rusqlite::Connection {
        let mut connection = rusqlite::Connection::open_in_memory().unwrap();
        vibex_db::apply_migrations(&mut connection).unwrap();
        connection
    }

    #[test]
    fn a_local_package_installs_enables_and_uninstalls() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();

        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();

        let result = service
            .install_from_file(&connection, package_path.to_str().unwrap(), true)
            .unwrap();
        assert!(result.created);
        assert_eq!(result.plugin.plugin_id, "acme.widgets");
        assert_eq!(result.plugin.version, "1.0.0");
        assert!(result.plugin.enabled);
        assert_eq!(result.plugin.trust, PluginTrust::Unverified);
        assert_eq!(
            result
                .plugin
                .permissions
                .iter()
                .map(|permission| permission.key.as_str())
                .collect::<Vec<_>>(),
            vec!["storage", "ui.panel"]
        );
        // The link resolves and the generated host manifest is in place.
        assert_eq!(
            service.store().current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
        assert!(
            service.store().data_dir("acme.widgets").is_dir(),
            "the data directory must exist"
        );
        let installed = service.list(&connection).unwrap();
        assert_eq!(installed.len(), 1);

        // Disabling and re-enabling keeps the version but moves the link.
        service
            .set_enabled(&connection, "acme.widgets", false)
            .unwrap();
        assert_eq!(service.store().current_version("acme.widgets"), None);
        service
            .set_enabled(&connection, "acme.widgets", true)
            .unwrap();
        assert_eq!(
            service.store().current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );

        // Uninstalling removes the link, the versions and the data.
        service
            .uninstall(&connection, "acme.widgets", true)
            .unwrap();
        assert!(service.list(&connection).unwrap().is_empty());
        assert!(!service.store().data_dir("acme.widgets").exists());
        assert!(
            !service
                .store()
                .version_dir("acme.widgets", "1.0.0")
                .exists()
        );
    }

    /// The rail draws the catalog, so the catalog has to answer with the
    /// enabled plugins' contributions and nothing else.
    #[test]
    fn contributions_follow_the_enable_switch() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        service.seed_native_plugins(&connection).unwrap();

        // Read the descriptor rather than restating it: the test is about which
        // rows come back, not about the first-party ids themselves.
        let contribution = |plugin_id: &str| {
            let descriptor = crate::native::BUILTIN_NATIVE_PLUGINS
                .iter()
                .find(|descriptor| descriptor.plugin_id == plugin_id)
                .expect("a first-party plugin");
            let activity = descriptor
                .right_rail_activity()
                .expect("the plugin contributes a rail activity");
            PluginContribution {
                plugin_id: plugin_id.to_string(),
                kind: activity.kind.to_string(),
                contribution_id: activity.contribution_id.to_string(),
                title: Some(activity.title.to_string()),
                enabled: true,
            }
        };
        let enabled = service.contributions(&connection).unwrap();
        assert!(enabled.contains(&contribution("vibex.system.git")));

        service
            .set_enabled(&connection, "vibex.system.git", false)
            .unwrap();
        let enabled = service.contributions(&connection).unwrap();
        assert!(
            !enabled
                .iter()
                .any(|candidate| candidate.plugin_id == "vibex.system.git"),
            "a disabled plugin must not contribute to the rail"
        );
        assert!(enabled.contains(&contribution("vibex.system.files")));

        // Re-enabling puts it back without a restart.
        service
            .set_enabled(&connection, "vibex.system.git", true)
            .unwrap();
        assert!(
            service
                .contributions(&connection)
                .unwrap()
                .contains(&contribution("vibex.system.git"))
        );
    }

    /// A script that asked for a panel gets one rail button; a background
    /// script gets none, and neither is registered under the wrong kind.
    #[test]
    fn an_installed_script_contributes_the_panel_it_asked_for() {
        let entry = PluginMarketEntry {
            id: "acme.widgets".to_string(),
            name: "Widgets".to_string(),
            ..Default::default()
        };
        let panel = PluginPermission::new("ui.panel", PluginPermissionRisk::Low);
        let contributions = contributions_for(&entry, std::slice::from_ref(&panel));
        assert_eq!(contributions.len(), 1);
        assert_eq!(contributions[0].kind, crate::native::RIGHT_RAIL_ACTIVITY);
        assert_eq!(contributions[0].contribution_id, "rail_plugin_acme.widgets");
        assert!(contributions[0].enabled);

        let background = PluginPermission::new("storage", PluginPermissionRisk::Low);
        assert!(
            contributions_for(&entry, &[background]).is_empty(),
            "a plugin that declared no panel must not reach the rail"
        );
    }

    #[test]
    fn first_party_plugins_seed_and_keep_their_choice() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        service.seed_native_plugins(&connection).unwrap();
        let plugins = service.list(&connection).unwrap();
        assert!(
            plugins
                .iter()
                .any(|plugin| plugin.plugin_id == "vibex.system.git")
        );
        assert!(
            plugins
                .iter()
                .all(|plugin| plugin.kind == PluginKind::Native && plugin.bundled)
        );

        service
            .set_enabled(&connection, "vibex.system.git", false)
            .unwrap();
        service.seed_native_plugins(&connection).unwrap();
        let git = PluginRepository::get(&connection, "vibex.system.git")
            .unwrap()
            .unwrap();
        assert!(!git.enabled, "seeding must preserve the user's choice");

        // A native plugin cannot be uninstalled or auto-updated.
        assert!(
            service
                .uninstall(&connection, "vibex.system.git", false)
                .is_err()
        );
        assert!(
            service
                .set_auto_update(&connection, "vibex.system.git", true)
                .is_err()
        );
    }

    #[test]
    fn a_disabled_install_keeps_its_version_for_a_later_enable() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();

        let result = service
            .install_from_file(&connection, package_path.to_str().unwrap(), false)
            .unwrap();
        assert!(!result.plugin.enabled);
        assert!(
            service.store().has_version("acme.widgets", "1.0.0"),
            "a disabled install must keep the version directory"
        );
        assert_eq!(service.store().current_version("acme.widgets"), None);

        let enabled = service
            .set_enabled(&connection, "acme.widgets", true)
            .unwrap();
        assert!(enabled.enabled);
        assert_eq!(
            service.store().current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
    }

    #[test]
    fn recovery_keeps_the_install_and_data_paths() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();
        service
            .install_from_file(&connection, package_path.to_str().unwrap(), true)
            .unwrap();

        let version_path = service.store().version_dir("acme.widgets", "1.0.0");
        let data_path = service.store().data_dir("acme.widgets");
        service
            .store()
            .write_pending(&PendingInstall {
                plugin_id: "acme.widgets".to_string(),
                previous_version: None,
                new_version: "1.0.0".to_string(),
                started_at_ms: 1,
            })
            .unwrap();

        let outcome = service.recover(&connection).unwrap();
        assert!(matches!(outcome, RecoveryOutcome::Completed { .. }));
        let record = PluginRepository::get(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert_eq!(record.install_dir.as_deref(), version_path.to_str());
        assert_eq!(record.data_dir.as_deref(), data_path.to_str());
    }

    struct LinkCheckingRuntime {
        home: std::path::PathBuf,
    }

    impl crate::runtime::PluginRuntime for LinkCheckingRuntime {
        fn load(&self, plugin: &LoadedPlugin) -> Result<(), PluginError> {
            let link = self.home.join("plugins").join(&plugin.plugin_id);
            if !link.exists() {
                return Err(PluginError::Host(
                    "the plugin link was missing while loading".to_string(),
                ));
            }
            Ok(())
        }

        fn unload(&self, _plugin_id: &str) -> Result<(), PluginError> {
            Ok(())
        }
    }

    #[test]
    fn enabling_a_disabled_plugin_puts_the_link_back_before_loading() {
        let directory = tempfile::tempdir().unwrap();
        let home = directory.path().to_path_buf();
        let cache = MarketCache::new(home.join("market"));
        let catalog = MarketCatalog::new(
            MarketHttpClient::new(MarketUrlPolicy::Testing),
            MarketConfig::empty(cache),
        );
        let service = PluginService::new(
            catalog,
            PluginStore::new(&home),
            Arc::new(LinkCheckingRuntime { home: home.clone() }),
            Version::parse("0.1.2").unwrap(),
        );
        let connection = connection();
        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();

        service
            .install_from_file(&connection, package_path.to_str().unwrap(), true)
            .unwrap();
        service
            .set_enabled(&connection, "acme.widgets", false)
            .unwrap();
        service
            .set_enabled(&connection, "acme.widgets", true)
            .unwrap();
        assert_eq!(
            service.store().current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
    }

    #[test]
    fn a_failed_update_restores_the_previous_version() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();
        service
            .install_from_file(&connection, package_path.to_str().unwrap(), true)
            .unwrap();
        let previous = PluginRepository::get(&connection, "acme.widgets")
            .unwrap()
            .unwrap();

        let entry = PluginMarketEntry {
            id: "acme.widgets".to_string(),
            name: "Widgets".to_string(),
            kind: PluginKind::Script,
            bundled: false,
            listing_source: PluginListingSource::Index,
            summary: None,
            icon_url: None,
            publisher: vibex_core::PluginPublisher::from_login("acme"),
            categories: Vec::new(),
            tags: Vec::new(),
            latest_version: Some("1.1.0".to_string()),
            installs: 0,
            rating: None,
            updated_at: None,
            license: None,
            homepage: None,
            repository: None,
            trust: PluginTrust::Community,
            permissions: previous.permissions.clone(),
            compatibility: Default::default(),
            size_bytes: None,
            featured: false,
            i18n: Default::default(),
        };

        let failed_version = "1.1.0";
        let failed_path = service.store().version_dir("acme.widgets", failed_version);
        std::fs::create_dir_all(&failed_path).unwrap();
        let mut failed = previous.clone();
        failed.version = failed_version.to_string();
        failed.install_dir = Some(failed_path.to_string_lossy().to_string());
        failed.status = InstalledPluginStatus::Enabled;
        PluginRepository::upsert(&connection, &failed).unwrap();
        PluginRepository::set_active_version(
            &connection,
            "acme.widgets",
            failed_version,
            failed.install_dir.as_deref(),
            failed.data_dir.as_deref(),
            failed.size_bytes,
            1,
        )
        .unwrap();
        PluginRepository::replace_grants(
            &connection,
            "acme.widgets",
            &failed.granted_permissions,
            &failed.permissions,
            1,
        )
        .unwrap();
        PluginRepository::replace_contributions(
            &connection,
            "acme.widgets",
            &contributions_for(&entry, &failed.permissions),
        )
        .unwrap();

        service
            .rollback_failed_load(&connection, &entry, Some(&previous), failed_version)
            .unwrap();
        let restored = PluginRepository::get(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert_eq!(restored.version, "1.0.0");
        assert_eq!(
            service.store().current_version("acme.widgets").as_deref(),
            Some("1.0.0")
        );
        assert!(!service.store().has_version("acme.widgets", failed_version));
    }

    #[test]
    fn a_revoked_plugin_is_disabled_and_cannot_be_enabled() {
        let directory = tempfile::tempdir().unwrap();
        let service = service(directory.path());
        let connection = connection();
        let package_path = directory.path().join("acme.widgets.vibexplug");
        std::fs::write(&package_path, package()).unwrap();
        service
            .install_from_file(&connection, package_path.to_str().unwrap(), true)
            .unwrap();

        service.market().config().cache.write_revocations(
            b"{}",
            "signature",
            &PluginRevocations {
                schema: 1,
                revision: 1,
                blocked_plugins: vec![RevokedPlugin {
                    id: "acme.widgets".to_string(),
                    versions: Vec::new(),
                    reason: "security".to_string(),
                    advisory_url: None,
                }],
            },
        );

        service.recover(&connection).unwrap();
        let record = PluginRepository::get(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert!(!record.enabled);
        assert_eq!(record.status, InstalledPluginStatus::Revoked);
        assert_eq!(service.store().current_version("acme.widgets"), None);

        let error = service
            .set_enabled(&connection, "acme.widgets", true)
            .unwrap_err();
        assert_eq!(error.code, "plugin_blocked");
    }
}

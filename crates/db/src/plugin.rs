//! Durable plugin records: what is installed, what it may do, and its versions.

use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use vibex_core::{
    InstalledPlugin, InstalledPluginStatus, PluginContribution, PluginKind, PluginLocalReview,
    PluginPermission, PluginPermissionGrant, PluginPermissionRisk, PluginPublisher, PluginSource,
    PluginTrust, PluginUserSource, VibexResult,
};

use super::{enum_from_db_sql, enum_to_db, json_from_db_sql, json_to_db};

/// Maps a rusqlite failure onto a plugin storage error.
fn db_err(code: &'static str) -> impl FnOnce(rusqlite::Error) -> vibex_core::VibexError {
    move |error| super::storage_err(code, "the plugin storage operation failed")(error)
}

/// One installed plugin's durable record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginRecord {
    pub plugin_id: String,
    pub display_name: String,
    pub version: String,
    pub kind: PluginKind,
    pub source: PluginSource,
    pub bundled: bool,
    pub uninstallable: bool,
    pub builtin_key: Option<String>,
    pub publisher_id: Option<String>,
    pub publisher_name: Option<String>,
    pub publisher_verified: bool,
    pub trust: PluginTrust,
    pub index_id: Option<String>,
    pub install_dir: Option<String>,
    pub data_dir: Option<String>,
    pub enabled: bool,
    pub auto_update: bool,
    pub pinned_version: Option<String>,
    pub status: InstalledPluginStatus,
    pub permissions: Vec<PluginPermission>,
    pub granted_permissions: Vec<PluginPermissionGrant>,
    pub consent_fingerprint: Option<String>,
    pub min_vibex: Option<String>,
    pub size_bytes: Option<u64>,
    pub last_error: Option<String>,
    pub last_checked_at_ms: Option<i64>,
    pub installed_at_ms: i64,
    pub updated_at_ms: i64,
    pub deleted_at_ms: Option<i64>,
}

impl PluginRecord {
    /// A new record for an install that is about to happen.
    pub fn new(
        plugin_id: impl Into<String>,
        display_name: impl Into<String>,
        version: impl Into<String>,
        now_ms: i64,
    ) -> Self {
        Self {
            plugin_id: plugin_id.into(),
            display_name: display_name.into(),
            version: version.into(),
            kind: PluginKind::Script,
            source: PluginSource::Market,
            bundled: false,
            uninstallable: true,
            builtin_key: None,
            publisher_id: None,
            publisher_name: None,
            publisher_verified: false,
            trust: PluginTrust::Unverified,
            index_id: None,
            install_dir: None,
            data_dir: None,
            enabled: true,
            auto_update: false,
            pinned_version: None,
            status: InstalledPluginStatus::Disabled,
            permissions: Vec::new(),
            granted_permissions: Vec::new(),
            consent_fingerprint: None,
            min_vibex: None,
            size_bytes: None,
            last_error: None,
            last_checked_at_ms: None,
            installed_at_ms: now_ms,
            updated_at_ms: now_ms,
            deleted_at_ms: None,
        }
    }

    /// The display projection the UI renders.
    pub fn to_installed(&self) -> InstalledPlugin {
        InstalledPlugin {
            plugin_id: self.plugin_id.clone(),
            name: self.display_name.clone(),
            version: self.version.clone(),
            kind: self.kind,
            source: self.source,
            bundled: self.bundled,
            uninstallable: self.uninstallable,
            builtin_key: self.builtin_key.clone(),
            publisher: PluginPublisher {
                id: self.publisher_id.clone().unwrap_or_default(),
                display_name: self.publisher_name.clone().unwrap_or_default(),
                verified: self.publisher_verified,
                homepage: None,
            },
            trust: self.trust,
            categories: Vec::new(),
            enabled: self.enabled,
            auto_update: self.auto_update,
            pinned_version: self.pinned_version.clone(),
            rollback_version: None,
            status: self.status,
            install_path: self.install_dir.clone(),
            data_path: self.data_dir.clone(),
            permissions: self.permissions.clone(),
            granted_permissions: self.granted_permissions.clone(),
            consent_fingerprint: self.consent_fingerprint.clone(),
            installed_at: None,
            updated_at: None,
            last_checked_at: self.last_checked_at_ms.map(|value| value.to_string()),
            last_error: self.last_error.clone(),
            size_bytes: self.size_bytes,
            update: None,
        }
    }
}

/// One published version of an installed plugin.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PluginVersionRecord {
    pub plugin_id: String,
    pub version: String,
    pub path: String,
    pub sha256: String,
    pub sha512: Option<String>,
    pub signature_key_id: Option<String>,
    pub signature_status: String,
    pub size_bytes: Option<u64>,
    pub active: bool,
    pub installed_at_ms: i64,
}

/// Plugin rows.
pub struct PluginRepository;

impl PluginRepository {
    /// Inserts or replaces a plugin record.
    pub fn upsert(conn: &Connection, record: &PluginRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO plugins (
                plugin_id, display_name, version, kind, source, bundled, uninstallable,
                builtin_key, publisher_id, publisher_name, publisher_verified, trust,
                index_id, install_dir, data_dir, enabled, auto_update, status,
                permissions_json, granted_permissions_json, consent_fingerprint,
                min_vibex, size_bytes, last_error, last_checked_at_ms,
                installed_at_ms, updated_at_ms, deleted_at_ms, pinned_version
            ) VALUES (
                ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29
            )
            ON CONFLICT(plugin_id) DO UPDATE SET
                display_name = excluded.display_name,
                version = excluded.version,
                kind = excluded.kind,
                source = excluded.source,
                bundled = excluded.bundled,
                uninstallable = excluded.uninstallable,
                builtin_key = excluded.builtin_key,
                publisher_id = excluded.publisher_id,
                publisher_name = excluded.publisher_name,
                publisher_verified = excluded.publisher_verified,
                trust = excluded.trust,
                index_id = excluded.index_id,
                install_dir = excluded.install_dir,
                data_dir = excluded.data_dir,
                enabled = excluded.enabled,
                auto_update = excluded.auto_update,
                status = excluded.status,
                permissions_json = excluded.permissions_json,
                granted_permissions_json = excluded.granted_permissions_json,
                consent_fingerprint = excluded.consent_fingerprint,
                min_vibex = excluded.min_vibex,
                size_bytes = excluded.size_bytes,
                last_error = excluded.last_error,
                last_checked_at_ms = excluded.last_checked_at_ms,
                updated_at_ms = excluded.updated_at_ms,
                deleted_at_ms = excluded.deleted_at_ms,
                pinned_version = excluded.pinned_version
            ",
            params![
                record.plugin_id,
                record.display_name,
                record.version,
                enum_to_db(&record.kind)?,
                enum_to_db(&record.source)?,
                record.bundled,
                record.uninstallable,
                record.builtin_key,
                record.publisher_id,
                record.publisher_name,
                record.publisher_verified,
                enum_to_db(&record.trust)?,
                record.index_id,
                record.install_dir,
                record.data_dir,
                record.enabled,
                record.auto_update,
                enum_to_db(&record.status)?,
                json_to_db(&record.permissions)?,
                json_to_db(&record.granted_permissions)?,
                record.consent_fingerprint,
                record.min_vibex,
                record.size_bytes.map(|value| value as i64),
                record.last_error,
                record.last_checked_at_ms,
                record.installed_at_ms,
                record.updated_at_ms,
                record.deleted_at_ms,
                record.pinned_version,
            ],
        )
        .map_err(db_err("plugin_upsert_failed"))?;
        Ok(())
    }

    /// Reads one plugin.
    pub fn get(conn: &Connection, plugin_id: &str) -> VibexResult<Option<PluginRecord>> {
        conn.query_row(
            &format!("{PLUGIN_SELECT} WHERE plugin_id = ?1"),
            params![plugin_id],
            read_plugin_row,
        )
        .optional()
        .map_err(db_err("plugin_read_failed"))
    }

    /// Every live plugin, newest first.
    pub fn list(conn: &Connection) -> VibexResult<Vec<PluginRecord>> {
        let mut statement = conn
            .prepare(&format!(
                "{PLUGIN_SELECT} WHERE deleted_at_ms IS NULL ORDER BY installed_at_ms DESC"
            ))
            .map_err(db_err("plugin_list_failed"))?;
        let rows = statement
            .query_map([], read_plugin_row)
            .map_err(db_err("plugin_list_failed"))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(db_err("plugin_list_failed"))?);
        }
        Ok(records)
    }

    /// Every live plugin of one kind.
    pub fn list_kind(conn: &Connection, kind: PluginKind) -> VibexResult<Vec<PluginRecord>> {
        let kind = enum_to_db(&kind)?;
        let mut statement = conn
            .prepare(&format!(
                "{PLUGIN_SELECT} WHERE deleted_at_ms IS NULL AND kind = ?1 \
                 ORDER BY installed_at_ms DESC"
            ))
            .map_err(db_err("plugin_list_failed"))?;
        let rows = statement
            .query_map(params![kind], read_plugin_row)
            .map_err(db_err("plugin_list_failed"))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(db_err("plugin_list_failed"))?);
        }
        Ok(records)
    }

    /// Sets a plugin's enabled flag.
    pub fn set_enabled(
        conn: &Connection,
        plugin_id: &str,
        enabled: bool,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET enabled = ?2, updated_at_ms = ?3 WHERE plugin_id = ?1",
            params![plugin_id, enabled, now_ms],
        )
        .map_err(db_err("plugin_enable_failed"))?;
        Ok(())
    }

    /// Sets a plugin's auto-update flag.
    pub fn set_auto_update(
        conn: &Connection,
        plugin_id: &str,
        auto_update: bool,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET auto_update = ?2, updated_at_ms = ?3 WHERE plugin_id = ?1",
            params![plugin_id, auto_update, now_ms],
        )
        .map_err(db_err("plugin_auto_update_failed"))?;
        Ok(())
    }

    /// Pins a plugin to a version, or clears the pin.
    pub fn set_pinned_version(
        conn: &Connection,
        plugin_id: &str,
        version: Option<&str>,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET pinned_version = ?2, updated_at_ms = ?3 WHERE plugin_id = ?1",
            params![plugin_id, version, now_ms],
        )
        .map_err(db_err("plugin_pin_failed"))?;
        Ok(())
    }

    /// Records a plugin's status and last error.
    pub fn set_status(
        conn: &Connection,
        plugin_id: &str,
        status: InstalledPluginStatus,
        last_error: Option<&str>,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET status = ?2, last_error = ?3, updated_at_ms = ?4 \
             WHERE plugin_id = ?1",
            params![plugin_id, enum_to_db(&status)?, last_error, now_ms],
        )
        .map_err(db_err("plugin_status_failed"))?;
        Ok(())
    }

    /// Records the active version and the paths it lives at.
    pub fn set_active_version(
        conn: &Connection,
        plugin_id: &str,
        version: &str,
        install_dir: Option<&str>,
        data_dir: Option<&str>,
        size_bytes: Option<u64>,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET version = ?2, install_dir = ?3, data_dir = ?4, \
             size_bytes = ?5, updated_at_ms = ?6 WHERE plugin_id = ?1",
            params![
                plugin_id,
                version,
                install_dir,
                data_dir,
                size_bytes.map(|value| value as i64),
                now_ms
            ],
        )
        .map_err(db_err("plugin_version_failed"))?;
        Ok(())
    }

    /// Records the last time updates were checked.
    pub fn set_checked(conn: &Connection, plugin_id: &str, now_ms: i64) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET last_checked_at_ms = ?2, updated_at_ms = ?2 WHERE plugin_id = ?1",
            params![plugin_id, now_ms],
        )
        .map_err(db_err("plugin_checked_failed"))?;
        Ok(())
    }

    /// Soft-deletes a plugin, keeping its audit row.
    pub fn soft_delete(conn: &Connection, plugin_id: &str, now_ms: i64) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugins SET deleted_at_ms = ?2, enabled = 0, updated_at_ms = ?2 \
             WHERE plugin_id = ?1",
            params![plugin_id, now_ms],
        )
        .map_err(db_err("plugin_delete_failed"))?;
        Ok(())
    }

    // ---- versions ----

    /// Inserts or replaces a version row.
    pub fn upsert_version(conn: &Connection, record: &PluginVersionRecord) -> VibexResult<()> {
        conn.execute(
            "
            INSERT INTO plugin_versions (
                plugin_id, version, path, sha256, sha512, signature_key_id,
                signature_status, size_bytes, active, installed_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ON CONFLICT(plugin_id, version) DO UPDATE SET
                path = excluded.path,
                sha256 = excluded.sha256,
                sha512 = excluded.sha512,
                signature_key_id = excluded.signature_key_id,
                signature_status = excluded.signature_status,
                size_bytes = excluded.size_bytes,
                active = excluded.active,
                installed_at_ms = excluded.installed_at_ms
            ",
            params![
                record.plugin_id,
                record.version,
                record.path,
                record.sha256,
                record.sha512,
                record.signature_key_id,
                record.signature_status,
                record.size_bytes.map(|value| value as i64),
                record.active,
                record.installed_at_ms,
            ],
        )
        .map_err(db_err("plugin_version_upsert_failed"))?;
        Ok(())
    }

    /// Marks one version active and every other inactive.
    pub fn set_active(conn: &Connection, plugin_id: &str, version: &str) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugin_versions SET active = 0 WHERE plugin_id = ?1",
            params![plugin_id],
        )
        .map_err(db_err("plugin_version_active_failed"))?;
        conn.execute(
            "UPDATE plugin_versions SET active = 1 WHERE plugin_id = ?1 AND version = ?2",
            params![plugin_id, version],
        )
        .map_err(db_err("plugin_version_active_failed"))?;
        Ok(())
    }

    /// Lists one plugin's versions, newest first.
    pub fn list_versions(
        conn: &Connection,
        plugin_id: &str,
    ) -> VibexResult<Vec<PluginVersionRecord>> {
        let mut statement = conn
            .prepare(
                "SELECT plugin_id, version, path, sha256, sha512, signature_key_id, \
                 signature_status, size_bytes, active, installed_at_ms \
                 FROM plugin_versions WHERE plugin_id = ?1 ORDER BY installed_at_ms DESC",
            )
            .map_err(db_err("plugin_version_list_failed"))?;
        let rows = statement
            .query_map(params![plugin_id], |row| {
                Ok(PluginVersionRecord {
                    plugin_id: row.get(0)?,
                    version: row.get(1)?,
                    path: row.get(2)?,
                    sha256: row.get(3)?,
                    sha512: row.get(4)?,
                    signature_key_id: row.get(5)?,
                    signature_status: row.get(6)?,
                    size_bytes: row.get::<_, Option<i64>>(7)?.map(|value| value as u64),
                    active: row.get(8)?,
                    installed_at_ms: row.get(9)?,
                })
            })
            .map_err(db_err("plugin_version_list_failed"))?;
        let mut records = Vec::new();
        for row in rows {
            records.push(row.map_err(db_err("plugin_version_list_failed"))?);
        }
        Ok(records)
    }

    /// Removes one version row.
    pub fn delete_version(conn: &Connection, plugin_id: &str, version: &str) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM plugin_versions WHERE plugin_id = ?1 AND version = ?2",
            params![plugin_id, version],
        )
        .map_err(db_err("plugin_version_delete_failed"))?;
        Ok(())
    }

    // ---- grants ----

    /// Replaces a plugin's granted permissions.
    pub fn replace_grants(
        conn: &Connection,
        plugin_id: &str,
        grants: &[PluginPermissionGrant],
        permissions: &[PluginPermission],
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM plugin_permission_grants WHERE plugin_id = ?1",
            params![plugin_id],
        )
        .map_err(db_err("plugin_grants_clear_failed"))?;
        for grant in grants {
            let risk = permissions
                .iter()
                .find(|permission| permission.key == grant.key)
                .map(|permission| permission.risk)
                .unwrap_or(PluginPermissionRisk::High);
            let scope = grant
                .scope
                .as_ref()
                .map(|scope| json_to_db(scope))
                .transpose()?;
            conn.execute(
                "INSERT OR REPLACE INTO plugin_permission_grants (
                    plugin_id, permission, scope_json, risk, granted_at_ms, revision
                ) VALUES (?1, ?2, ?3, ?4, ?5, 1)",
                params![plugin_id, grant.key, scope, enum_to_db(&risk)?, now_ms],
            )
            .map_err(db_err("plugin_grant_insert_failed"))?;
        }
        Ok(())
    }

    /// Lists a plugin's granted permissions.
    pub fn list_grants(
        conn: &Connection,
        plugin_id: &str,
    ) -> VibexResult<Vec<PluginPermissionGrant>> {
        let mut statement = conn
            .prepare(
                "SELECT permission, scope_json FROM plugin_permission_grants \
                 WHERE plugin_id = ?1 ORDER BY permission",
            )
            .map_err(db_err("plugin_grants_list_failed"))?;
        let rows = statement
            .query_map(params![plugin_id], |row| {
                let scope: Option<String> = row.get(1)?;
                Ok(PluginPermissionGrant {
                    key: row.get(0)?,
                    scope: scope.and_then(|value| json_from_db_sql(value).ok()),
                })
            })
            .map_err(db_err("plugin_grants_list_failed"))?;
        let mut grants = Vec::new();
        for row in rows {
            grants.push(row.map_err(db_err("plugin_grants_list_failed"))?);
        }
        Ok(grants)
    }

    // ---- contributions ----

    /// Replaces a plugin's contribution rows.
    pub fn replace_contributions(
        conn: &Connection,
        plugin_id: &str,
        contributions: &[PluginContribution],
    ) -> VibexResult<()> {
        conn.execute(
            "DELETE FROM plugin_contributions WHERE plugin_id = ?1",
            params![plugin_id],
        )
        .map_err(db_err("plugin_contributions_clear_failed"))?;
        for contribution in contributions {
            conn.execute(
                "INSERT OR REPLACE INTO plugin_contributions (
                    plugin_id, kind, contribution_id, title, detail, enabled
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                params![
                    plugin_id,
                    contribution.kind,
                    contribution.contribution_id,
                    contribution.title,
                    contribution.detail,
                    contribution.enabled,
                ],
            )
            .map_err(db_err("plugin_contribution_insert_failed"))?;
        }
        Ok(())
    }

    /// Lists a plugin's contributions.
    pub fn list_contributions(
        conn: &Connection,
        plugin_id: &str,
    ) -> VibexResult<Vec<PluginContribution>> {
        let mut statement = conn
            .prepare(
                "SELECT plugin_id, kind, contribution_id, title, detail, enabled \
                 FROM plugin_contributions WHERE plugin_id = ?1 ORDER BY kind, contribution_id",
            )
            .map_err(db_err("plugin_contributions_list_failed"))?;
        let rows = statement
            .query_map(params![plugin_id], |row| {
                Ok(PluginContribution {
                    plugin_id: row.get(0)?,
                    kind: row.get(1)?,
                    contribution_id: row.get(2)?,
                    title: row.get(3)?,
                    detail: row.get(4)?,
                    enabled: row.get(5)?,
                })
            })
            .map_err(db_err("plugin_contributions_list_failed"))?;
        let mut contributions = Vec::new();
        for row in rows {
            contributions.push(row.map_err(db_err("plugin_contributions_list_failed"))?);
        }
        Ok(contributions)
    }

    /// Updates one contribution's enabled flag.
    pub fn set_contribution_enabled(
        conn: &Connection,
        plugin_id: &str,
        kind: &str,
        contribution_id: &str,
        enabled: bool,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugin_contributions SET enabled = ?4 \
             WHERE plugin_id = ?1 AND kind = ?2 AND contribution_id = ?3",
            params![plugin_id, kind, contribution_id, enabled],
        )
        .map_err(db_err("plugin_contribution_enable_failed"))?;
        Ok(())
    }

    /// Inserts or replaces the rating this machine left for a plugin.
    pub fn upsert_review(
        conn: &Connection,
        review: &PluginLocalReview,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO plugin_reviews (
                plugin_id, rating, created_at_ms, updated_at_ms
            ) VALUES (?1, ?2, ?3, ?3)
            ON CONFLICT(plugin_id) DO UPDATE SET rating = ?2, updated_at_ms = ?3",
            params![review.plugin_id, review.rating, now_ms],
        )
        .map_err(db_err("plugin_review_upsert_failed"))?;
        Ok(())
    }

    /// The rating this machine left for a plugin, if any.
    pub fn get_review(
        conn: &Connection,
        plugin_id: &str,
    ) -> VibexResult<Option<PluginLocalReview>> {
        conn.query_row(
            "SELECT plugin_id, rating, updated_at_ms FROM plugin_reviews WHERE plugin_id = ?1",
            params![plugin_id],
            |row| {
                Ok(PluginLocalReview {
                    plugin_id: row.get(0)?,
                    rating: row.get::<_, i64>(1)?.clamp(0, 255) as u8,
                    updated_at_ms: row.get(2)?,
                })
            },
        )
        .optional()
        .map_err(db_err("plugin_review_read_failed"))
    }

    /// Every rating this machine left, oldest first.
    pub fn list_reviews(conn: &Connection) -> VibexResult<Vec<PluginLocalReview>> {
        let mut statement = conn
            .prepare(
                "SELECT plugin_id, rating, updated_at_ms FROM plugin_reviews \
                 ORDER BY updated_at_ms, plugin_id",
            )
            .map_err(db_err("plugin_reviews_list_failed"))?;
        let rows = statement
            .query_map([], |row| {
                Ok(PluginLocalReview {
                    plugin_id: row.get(0)?,
                    rating: row.get::<_, i64>(1)?.clamp(0, 255) as u8,
                    updated_at_ms: row.get(2)?,
                })
            })
            .map_err(db_err("plugin_reviews_list_failed"))?;
        let mut reviews = Vec::new();
        for row in rows {
            reviews.push(row.map_err(db_err("plugin_reviews_list_failed"))?);
        }
        Ok(reviews)
    }

    /// Removes the rating; `false` when there was none.
    pub fn delete_review(conn: &Connection, plugin_id: &str) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "DELETE FROM plugin_reviews WHERE plugin_id = ?1",
                params![plugin_id],
            )
            .map_err(db_err("plugin_review_delete_failed"))?;
        Ok(changed > 0)
    }

    // ---- catalogs the reader added ----

    /// Inserts or replaces one user source, keyed by its address.
    pub fn upsert_source(conn: &Connection, source: &PluginUserSource) -> VibexResult<()> {
        conn.execute(
            "INSERT INTO plugin_sources (
                source_id, name, kind, index_url, public_key, enabled,
                added_at_ms, updated_at_ms, last_error
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)
            ON CONFLICT(source_id) DO UPDATE SET
                name = excluded.name,
                kind = excluded.kind,
                index_url = excluded.index_url,
                public_key = excluded.public_key,
                enabled = excluded.enabled,
                updated_at_ms = excluded.updated_at_ms,
                last_error = excluded.last_error",
            params![
                source.source_id,
                source.name,
                enum_to_db(&source.kind)?,
                source.index_url,
                source.public_key,
                source.enabled,
                source.added_at_ms,
                source.updated_at_ms,
                source.last_error,
            ],
        )
        .map_err(db_err("plugin_source_upsert_failed"))?;
        Ok(())
    }

    /// Every user source, in the order they were added.
    pub fn list_sources(conn: &Connection) -> VibexResult<Vec<PluginUserSource>> {
        let mut statement = conn
            .prepare(
                "SELECT source_id, name, kind, index_url, public_key, enabled, \
                 added_at_ms, updated_at_ms, last_error \
                 FROM plugin_sources ORDER BY added_at_ms, source_id",
            )
            .map_err(db_err("plugin_sources_list_failed"))?;
        let rows = statement
            .query_map([], source_from_row)
            .map_err(db_err("plugin_sources_list_failed"))?;
        let mut sources = Vec::new();
        for row in rows {
            sources.push(row.map_err(db_err("plugin_sources_list_failed"))?);
        }
        Ok(sources)
    }

    /// The enabled sources only, which is what a refresh reads.
    pub fn list_enabled_sources(conn: &Connection) -> VibexResult<Vec<PluginUserSource>> {
        Ok(Self::list_sources(conn)?
            .into_iter()
            .filter(|source| source.enabled)
            .collect())
    }

    /// One source by id.
    pub fn get_source(conn: &Connection, source_id: &str) -> VibexResult<Option<PluginUserSource>> {
        conn.query_row(
            "SELECT source_id, name, kind, index_url, public_key, enabled, \
             added_at_ms, updated_at_ms, last_error \
             FROM plugin_sources WHERE source_id = ?1",
            params![source_id],
            source_from_row,
        )
        .optional()
        .map_err(db_err("plugin_source_read_failed"))
    }

    /// Turns one source on or off; `false` when there was no such source.
    pub fn set_source_enabled(
        conn: &Connection,
        source_id: &str,
        enabled: bool,
        now_ms: i64,
    ) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "UPDATE plugin_sources SET enabled = ?2, updated_at_ms = ?3 \
                 WHERE source_id = ?1",
                params![source_id, enabled, now_ms],
            )
            .map_err(db_err("plugin_source_enable_failed"))?;
        Ok(changed > 0)
    }

    /// Records why a refresh failed, or clears it with `None`.
    pub fn set_source_error(
        conn: &Connection,
        source_id: &str,
        error: Option<&str>,
        now_ms: i64,
    ) -> VibexResult<()> {
        conn.execute(
            "UPDATE plugin_sources SET last_error = ?2, updated_at_ms = ?3 \
             WHERE source_id = ?1",
            params![source_id, error, now_ms],
        )
        .map_err(db_err("plugin_source_error_failed"))?;
        Ok(())
    }

    /// Removes one source; `false` when there was none.
    pub fn delete_source(conn: &Connection, source_id: &str) -> VibexResult<bool> {
        let changed = conn
            .execute(
                "DELETE FROM plugin_sources WHERE source_id = ?1",
                params![source_id],
            )
            .map_err(db_err("plugin_source_delete_failed"))?;
        Ok(changed > 0)
    }
}

/// Reads one `plugin_sources` row.
fn source_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginUserSource> {
    let kind: String = row.get(2)?;
    Ok(PluginUserSource {
        source_id: row.get(0)?,
        name: row.get(1)?,
        kind: enum_from_db_sql(kind)?,
        index_url: row.get(3)?,
        public_key: row.get(4)?,
        enabled: row.get(5)?,
        added_at_ms: row.get(6)?,
        updated_at_ms: row.get(7)?,
        last_error: row.get(8)?,
    })
}

const PLUGIN_SELECT: &str = "
    SELECT plugin_id, display_name, version, kind, source, bundled, uninstallable,
        builtin_key, publisher_id, publisher_name, publisher_verified, trust,
        index_id, install_dir, data_dir, enabled, auto_update, status,
        permissions_json, granted_permissions_json, consent_fingerprint,
        min_vibex, size_bytes, last_error, last_checked_at_ms,
        installed_at_ms, updated_at_ms, deleted_at_ms, pinned_version
    FROM plugins
";

fn read_plugin_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<PluginRecord> {
    let kind: String = row.get(3)?;
    let source: String = row.get(4)?;
    let trust: String = row.get(11)?;
    let status: String = row.get(17)?;
    let permissions: String = row.get(18)?;
    let granted: String = row.get(19)?;
    Ok(PluginRecord {
        plugin_id: row.get(0)?,
        display_name: row.get(1)?,
        version: row.get(2)?,
        kind: enum_from_db_sql(kind)?,
        source: enum_from_db_sql(source)?,
        bundled: row.get(5)?,
        uninstallable: row.get(6)?,
        builtin_key: row.get(7)?,
        publisher_id: row.get(8)?,
        publisher_name: row.get(9)?,
        publisher_verified: row.get(10)?,
        trust: enum_from_db_sql(trust)?,
        index_id: row.get(12)?,
        install_dir: row.get(13)?,
        data_dir: row.get(14)?,
        enabled: row.get(15)?,
        auto_update: row.get(16)?,
        status: enum_from_db_sql(status)?,
        permissions: json_from_db_sql(permissions)?,
        granted_permissions: json_from_db_sql(granted)?,
        consent_fingerprint: row.get(20)?,
        min_vibex: row.get(21)?,
        size_bytes: row.get::<_, Option<i64>>(22)?.map(|value| value as u64),
        last_error: row.get(23)?,
        last_checked_at_ms: row.get(24)?,
        installed_at_ms: row.get(25)?,
        updated_at_ms: row.get(26)?,
        deleted_at_ms: row.get(27)?,
        pinned_version: row.get(28)?,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::Connection;

    fn connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        super::super::apply_migrations(&mut connection).unwrap();
        connection
    }

    fn record() -> PluginRecord {
        let mut record = PluginRecord::new("acme.widgets", "Widgets", "1.0.0", 100);
        record.trust = PluginTrust::Community;
        record.status = InstalledPluginStatus::Enabled;
        record.permissions = vec![PluginPermission::new("ui.panel", PluginPermissionRisk::Low)];
        record.granted_permissions = vec![PluginPermissionGrant {
            key: "ui.panel".to_string(),
            scope: None,
        }];
        record
    }

    #[test]
    fn a_plugin_round_trips() {
        let connection = connection();
        PluginRepository::upsert(&connection, &record()).unwrap();
        let read = PluginRepository::get(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert_eq!(read.display_name, "Widgets");
        assert_eq!(read.status, InstalledPluginStatus::Enabled);
        assert_eq!(read.trust, PluginTrust::Community);
        assert_eq!(read.permissions.len(), 1);
        assert_eq!(read.granted_permissions[0].key, "ui.panel");
        assert_eq!(PluginRepository::list(&connection).unwrap().len(), 1);
        assert_eq!(
            PluginRepository::list_kind(&connection, PluginKind::Script)
                .unwrap()
                .len(),
            1
        );
        assert!(
            PluginRepository::list_kind(&connection, PluginKind::Native)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn versions_and_grants_round_trip() {
        let connection = connection();
        PluginRepository::upsert(&connection, &record()).unwrap();
        PluginRepository::upsert_version(
            &connection,
            &PluginVersionRecord {
                plugin_id: "acme.widgets".to_string(),
                version: "1.0.0".to_string(),
                path: "/plugins/1.0.0".to_string(),
                sha256: "a".repeat(64),
                sha512: None,
                signature_key_id: Some("root".to_string()),
                signature_status: "verified".to_string(),
                size_bytes: Some(10),
                active: false,
                installed_at_ms: 100,
            },
        )
        .unwrap();
        PluginRepository::set_active(&connection, "acme.widgets", "1.0.0").unwrap();
        let versions = PluginRepository::list_versions(&connection, "acme.widgets").unwrap();
        assert_eq!(versions.len(), 1);
        assert!(versions[0].active);

        PluginRepository::replace_grants(
            &connection,
            "acme.widgets",
            &[PluginPermissionGrant {
                key: "storage".to_string(),
                scope: Some("data".to_string()),
            }],
            &[PluginPermission::new("storage", PluginPermissionRisk::Low)],
            200,
        )
        .unwrap();
        let grants = PluginRepository::list_grants(&connection, "acme.widgets").unwrap();
        assert_eq!(grants.len(), 1);
        assert_eq!(grants[0].scope.as_deref(), Some("data"));
    }

    #[test]
    fn a_soft_deleted_plugin_is_hidden() {
        let connection = connection();
        PluginRepository::upsert(&connection, &record()).unwrap();
        PluginRepository::soft_delete(&connection, "acme.widgets", 300).unwrap();
        assert!(PluginRepository::list(&connection).unwrap().is_empty());
        assert!(
            PluginRepository::get(&connection, "acme.widgets")
                .unwrap()
                .unwrap()
                .deleted_at_ms
                .is_some()
        );
    }

    #[test]
    fn builtin_keys_are_unique() {
        let connection = connection();
        let mut native = record();
        native.plugin_id = "vibex.system.files".to_string();
        native.kind = PluginKind::Native;
        native.source = PluginSource::Bundled;
        native.builtin_key = Some("vibex.system.files".to_string());
        PluginRepository::upsert(&connection, &native).unwrap();

        let mut duplicate = native.clone();
        duplicate.plugin_id = "vibex.system.files.other".to_string();
        assert!(PluginRepository::upsert(&connection, &duplicate).is_err());
    }

    #[test]
    fn a_local_review_is_replaced_not_duplicated() {
        let connection = connection();
        let review = PluginLocalReview {
            plugin_id: "acme.widgets".to_string(),
            rating: 4,
            updated_at_ms: 0,
        };
        PluginRepository::upsert_review(&connection, &review, 100).unwrap();
        let stored = PluginRepository::get_review(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert_eq!(stored.rating, 4);
        assert_eq!(stored.updated_at_ms, 100);

        let updated = PluginLocalReview {
            rating: 2,
            ..review.clone()
        };
        PluginRepository::upsert_review(&connection, &updated, 200).unwrap();
        let stored = PluginRepository::get_review(&connection, "acme.widgets")
            .unwrap()
            .unwrap();
        assert_eq!(stored.rating, 2);
        assert_eq!(stored.updated_at_ms, 200);
        assert_eq!(
            PluginRepository::list_reviews(&connection).unwrap().len(),
            1
        );

        assert!(PluginRepository::delete_review(&connection, "acme.widgets").unwrap());
        assert!(
            PluginRepository::get_review(&connection, "acme.widgets")
                .unwrap()
                .is_none()
        );
        assert!(!PluginRepository::delete_review(&connection, "acme.widgets").unwrap());
    }

    #[test]
    fn a_user_source_round_trips_and_toggles() {
        let connection = connection();
        let source = PluginUserSource {
            source_id: "0123456789abcdef".to_string(),
            name: "Acme".to_string(),
            kind: Default::default(),
            index_url: "https://acme.example/index.json".to_string(),
            public_key: Some("key".to_string()),
            enabled: true,
            added_at_ms: 100,
            updated_at_ms: None,
            last_error: None,
        };
        PluginRepository::upsert_source(&connection, &source).unwrap();

        let read = PluginRepository::get_source(&connection, "0123456789abcdef")
            .unwrap()
            .unwrap();
        assert_eq!(read.name, "Acme");
        assert_eq!(read.public_key.as_deref(), Some("key"));
        assert!(read.enabled);
        assert_eq!(
            PluginRepository::list_sources(&connection).unwrap().len(),
            1
        );

        assert!(
            PluginRepository::set_source_enabled(&connection, "0123456789abcdef", false, 200)
                .unwrap()
        );
        assert!(
            PluginRepository::list_enabled_sources(&connection)
                .unwrap()
                .is_empty()
        );

        PluginRepository::set_source_error(&connection, "0123456789abcdef", Some("offline"), 300)
            .unwrap();
        let read = PluginRepository::get_source(&connection, "0123456789abcdef")
            .unwrap()
            .unwrap();
        assert_eq!(read.last_error.as_deref(), Some("offline"));
        assert_eq!(read.updated_at_ms, Some(300));

        assert!(PluginRepository::delete_source(&connection, "0123456789abcdef").unwrap());
        assert!(!PluginRepository::delete_source(&connection, "0123456789abcdef").unwrap());
        assert!(
            PluginRepository::get_source(&connection, "0123456789abcdef")
                .unwrap()
                .is_none()
        );
    }

    #[test]
    fn re_adding_a_source_updates_it_instead_of_duplicating() {
        let connection = connection();
        let mut source = PluginUserSource {
            source_id: "0123456789abcdef".to_string(),
            name: "Acme".to_string(),
            kind: Default::default(),
            index_url: "https://acme.example/index.json".to_string(),
            public_key: None,
            enabled: true,
            added_at_ms: 100,
            updated_at_ms: None,
            last_error: None,
        };
        PluginRepository::upsert_source(&connection, &source).unwrap();
        source.name = "Acme Renamed".to_string();
        PluginRepository::upsert_source(&connection, &source).unwrap();

        let sources = PluginRepository::list_sources(&connection).unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].name, "Acme Renamed");
        assert_eq!(sources[0].added_at_ms, 100);
    }
}

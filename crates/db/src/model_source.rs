//! Persistence for an Agent's one default model source.
//!
//! The unified record keeps the legacy Provider default's scope in its primary
//! key, `(scope_kind, scope_id, agent_id)`, so an Agent can default differently
//! in a workspace than it does globally. It stores a polymorphic source — one
//! Provider or one route group — in two columns rather than a foreign key,
//! because no SQLite foreign key can name one of two tables. The write paths
//! validate the source before storing it; this module only keeps the row.
//!
//! The backfill reads the two legacy default stores and the binding tables and
//! writes the same answer the read path would have resolved. It never deletes
//! an old row or field: the legacy defaults stay readable until a later wave
//! removes them.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, OptionalExtension, TransactionBehavior, params};
use vibex_core::{
    AgentId, AgentModelSourceDefault, AgentModelSourceRef, ModelRoute, ModelRouteId, ProjectId,
    ProviderDefaultScopeKind, ProviderProfile, ProviderProfileDefaultScope, ProviderProfileId,
    ProviderProfileStatus, RouteStatus, VibexError, VibexResult, WorkspaceId, unix_timestamp_ms,
};

use super::{
    AgentDefaultModelProviderProfileRepository, AgentModelRouteBindingRepository,
    ModelRouteRepository, ProviderProfileRepository, enum_from_db, enum_to_db, migration_applied,
    storage_err,
};

/// The row shape the unified default is stored as.
pub struct AgentModelSourceDefaultRepository;

impl AgentModelSourceDefaultRepository {
    /// Reads the explicit unified row, if one exists.
    ///
    /// This is deliberately the row and not the resolved answer: the read API
    /// in `vibex-config-switch` falls back to the legacy defaults after this
    /// returns `None`, and a caller that wants the row itself (the migration,
    /// the bundle) must not receive a synthesized one.
    pub fn get(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        agent_id: AgentId,
    ) -> VibexResult<Option<AgentModelSourceDefault>> {
        let (scope_kind, scope_id) = scope_db_parts(&scope)?;
        let row = conn
            .query_row(
                "
                SELECT source_kind, source_id, updated_at_ms
                FROM agent_model_source_defaults
                WHERE scope_kind = ?1 AND scope_id = ?2 AND agent_id = ?3
                ",
                params![scope_kind, scope_id, agent_id.as_str()],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, i64>(2)?,
                    ))
                },
            )
            .optional()
            .map_err(storage_err(
                "agent_model_source_default_lookup_failed",
                "failed to load the Agent's default model source",
            ))?;
        row.map(|(source_kind, source_id, updated_at_ms)| {
            Ok(AgentModelSourceDefault {
                scope,
                agent_id,
                source: Some(source_from_db(source_kind, source_id)?),
                updated_at_ms,
            })
        })
        .transpose()
    }

    /// Writes the explicit row, replacing whatever held the primary key.
    ///
    /// A `None` source clears the row instead of storing a null: the table is
    /// the set of explicit answers, and "no default" is the absence of one.
    pub fn set(conn: &Connection, default: &AgentModelSourceDefault) -> VibexResult<()> {
        let Some(source) = default.source.as_ref() else {
            Self::clear(conn, default.scope.clone(), default.agent_id.clone())?;
            return Ok(());
        };
        let (scope_kind, scope_id) = scope_db_parts(&default.scope)?;
        let (source_kind, source_id) = source_db_parts(source);
        let updated_at_ms = if default.updated_at_ms > 0 {
            default.updated_at_ms
        } else {
            unix_timestamp_ms()
        };
        conn.execute(
            "
            INSERT INTO agent_model_source_defaults (
                scope_kind, scope_id, agent_id, source_kind, source_id, updated_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(scope_kind, scope_id, agent_id) DO UPDATE SET
                source_kind = excluded.source_kind,
                source_id = excluded.source_id,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                scope_kind,
                scope_id,
                default.agent_id.as_str(),
                source_kind,
                source_id,
                updated_at_ms
            ],
        )
        .map_err(storage_err(
            "agent_model_source_default_set_failed",
            "failed to set the Agent's default model source",
        ))?;
        Ok(())
    }

    /// Removes the explicit row. Clearing an absent row is not an error.
    pub fn clear(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        agent_id: AgentId,
    ) -> VibexResult<bool> {
        let (scope_kind, scope_id) = scope_db_parts(&scope)?;
        let changed = conn
            .execute(
                "
                DELETE FROM agent_model_source_defaults
                WHERE scope_kind = ?1 AND scope_id = ?2 AND agent_id = ?3
                ",
                params![scope_kind, scope_id, agent_id.as_str()],
            )
            .map_err(storage_err(
                "agent_model_source_default_clear_failed",
                "failed to clear the Agent's default model source",
            ))?;
        Ok(changed > 0)
    }

    /// Every explicit row for one Agent, in scope order.
    pub fn list_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelSourceDefault>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT scope_kind, scope_id, source_kind, source_id, updated_at_ms
                FROM agent_model_source_defaults
                WHERE agent_id = ?1
                ORDER BY scope_kind ASC, scope_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_source_default_list_failed",
                "failed to list the Agent's default model source rows",
            ))?;
        let rows = stmt
            .query_map(params![agent_id.as_str()], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(storage_err(
                "agent_model_source_default_list_failed",
                "failed to list the Agent's default model source rows",
            ))?;
        let mut defaults = Vec::new();
        for row in rows {
            let (scope_kind, scope_id, source_kind, source_id, updated_at_ms) =
                row.map_err(storage_err(
                    "agent_model_source_default_list_failed",
                    "failed to decode an Agent default model source row",
                ))?;
            defaults.push(default_from_row(
                scope_kind,
                scope_id,
                agent_id.clone(),
                source_kind,
                source_id,
                updated_at_ms,
            )?);
        }
        Ok(defaults)
    }

    /// Every explicit row, in Agent then scope order.
    pub fn list_all(conn: &Connection) -> VibexResult<Vec<AgentModelSourceDefault>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT scope_kind, scope_id, agent_id, source_kind, source_id, updated_at_ms
                FROM agent_model_source_defaults
                ORDER BY agent_id ASC, scope_kind ASC, scope_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_source_default_list_failed",
                "failed to list the default model source rows",
            ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            })
            .map_err(storage_err(
                "agent_model_source_default_list_failed",
                "failed to list the default model source rows",
            ))?;
        let mut defaults = Vec::new();
        for row in rows {
            let (scope_kind, scope_id, agent_id, source_kind, source_id, updated_at_ms) = row
                .map_err(storage_err(
                    "agent_model_source_default_list_failed",
                    "failed to decode a default model source row",
                ))?;
            defaults.push(default_from_row(
                scope_kind,
                scope_id,
                AgentId::parse(agent_id)?,
                source_kind,
                source_id,
                updated_at_ms,
            )?);
        }
        Ok(defaults)
    }

    /// Resolves the answer the read API returns for one Agent and scope.
    ///
    /// Precedence is fixed and shared with the migration:
    /// explicit unified row, then the Agent's route default, then the scoped
    /// Provider default, then the Agent's sole binding. An unusable legacy
    /// source is skipped rather than returned: a deleted or disabled Provider
    /// must not become a launch decision by being the only row left.
    pub fn resolve(
        conn: &Connection,
        scope: ProviderProfileDefaultScope,
        agent_id: AgentId,
    ) -> VibexResult<AgentModelSourceDefault> {
        if let Some(explicit) = Self::get(conn, scope.clone(), agent_id.clone())? {
            // An explicit row is an answer only while its target is usable. A
            // Provider or route that was deleted or disabled later must not be
            // promoted by a stale row; the resolver falls through to the
            // legacy sources instead, which is the data-repair behaviour.
            let usable = match explicit.source.as_ref() {
                Some(AgentModelSourceRef::Provider(provider_profile_id)) => {
                    provider_is_usable(conn, &agent_id, provider_profile_id)?
                }
                Some(AgentModelSourceRef::RouteGroup(route_id)) => {
                    ModelRouteRepository::get(conn, route_id)?
                        .is_some_and(|route| route_is_usable(&route))
                }
                None => false,
            };
            if usable {
                return Ok(explicit);
            }
        }

        for binding in AgentModelRouteBindingRepository::list_for_agent(conn, &agent_id)? {
            if !binding.is_default {
                continue;
            }
            let Some(route) = ModelRouteRepository::get(conn, &binding.model_route_id)? else {
                continue;
            };
            if route_is_usable(&route) {
                return Ok(AgentModelSourceDefault {
                    scope,
                    agent_id,
                    source: Some(AgentModelSourceRef::RouteGroup(route.id)),
                    updated_at_ms: binding.updated_at_ms.max(route.updated_at_ms),
                });
            }
        }

        let provider_default =
            AgentDefaultModelProviderProfileRepository::get(conn, scope.clone(), agent_id.clone())?;
        if let Some(provider_profile_id) = provider_default.provider_profile_id
            && provider_is_usable(conn, &agent_id, &provider_profile_id)?
        {
            return Ok(AgentModelSourceDefault {
                scope,
                agent_id,
                source: Some(AgentModelSourceRef::Provider(provider_profile_id)),
                updated_at_ms: provider_default.updated_at_ms.unwrap_or(0),
            });
        }

        let mut candidates = Vec::new();
        for (provider_profile_id, updated_at_ms) in user_provider_bindings(conn, &agent_id)? {
            candidates.push((
                AgentModelSourceRef::Provider(provider_profile_id),
                updated_at_ms,
            ));
        }
        for binding in AgentModelRouteBindingRepository::list_for_agent(conn, &agent_id)? {
            let Some(route) = ModelRouteRepository::get(conn, &binding.model_route_id)? else {
                continue;
            };
            if route_is_usable(&route) {
                candidates.push((
                    AgentModelSourceRef::RouteGroup(route.id),
                    binding.updated_at_ms,
                ));
            }
        }
        if candidates.len() == 1 {
            let (source, updated_at_ms) = candidates.remove(0);
            return Ok(AgentModelSourceDefault {
                scope,
                agent_id,
                source: Some(source),
                updated_at_ms,
            });
        }

        Ok(AgentModelSourceDefault::none(scope, agent_id))
    }
}

/// One source the migration would write.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModelSourceBackfillEntry {
    pub scope: ProviderProfileDefaultScope,
    pub agent_id: AgentId,
    pub source: AgentModelSourceRef,
    pub updated_at_ms: i64,
}

/// One anomaly the migration found and reported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentModelSourceMigrationIssue {
    pub scope: Option<ProviderProfileDefaultScope>,
    pub agent_id: AgentId,
    pub reason: String,
}

/// What the backfill found, in a shape a test can assert without reading logs.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AgentModelSourceMigrationReport {
    /// Entries written into the unified table.
    pub inserted: usize,
    /// Both a route default and a scoped Provider default named the Agent.
    pub double_defaults: Vec<AgentModelSourceMigrationIssue>,
    /// A legacy default named a Provider or route that is missing or unusable.
    pub invalid_sources: Vec<AgentModelSourceMigrationIssue>,
    /// The Agent has more than one binding and no default of either kind.
    pub no_default_with_bindings: Vec<AgentModelSourceMigrationIssue>,
}

impl AgentModelSourceMigrationReport {
    /// Writes the one-time migration report to the tracing log.
    ///
    /// Nothing in the product reads this report; it exists so an operator can
    /// find the rows that need a human decision after upgrading. The old fields
    /// are retained, so every line here is a warning about ambiguity, not data
    /// loss.
    pub fn log(&self) {
        if self.inserted > 0 {
            tracing::info!(
                inserted = self.inserted,
                "agent model source defaults backfilled"
            );
        }
        for issue in &self.double_defaults {
            tracing::warn!(
                agent_id = issue.agent_id.as_str(),
                scope = issue.scope.as_ref().map(ProviderProfileDefaultScope::storage_key),
                reason = %issue.reason,
                "agent model source migration: double default"
            );
        }
        for issue in &self.invalid_sources {
            tracing::warn!(
                agent_id = issue.agent_id.as_str(),
                scope = issue.scope.as_ref().map(ProviderProfileDefaultScope::storage_key),
                reason = %issue.reason,
                "agent model source migration: invalid source"
            );
        }
        for issue in &self.no_default_with_bindings {
            tracing::warn!(
                agent_id = issue.agent_id.as_str(),
                reason = %issue.reason,
                "agent model source migration: no default with bindings"
            );
        }
    }
}

/// The deterministic backfill plan for a database at migration 81.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct AgentModelSourceMigrationPlan {
    pub entries: Vec<AgentModelSourceBackfillEntry>,
    pub report: AgentModelSourceMigrationReport,
}

/// Creates the unified table and backfills it from the legacy defaults.
pub(crate) fn apply_agent_model_source_defaults(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 81;
    const NAME: &str = "agent_model_source_defaults";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_err(
            "agent_model_source_default_migration_transaction_failed",
            "failed to start the agent model source defaults migration",
        ))?;
    // The source-default backfill reads Provider profiles through the normal
    // repository. A database that predates migration 82 has no `metadata_json`
    // column yet, so add it here before the backfill; migration 82 then records
    // itself without trying a duplicate ALTER.
    let has_metadata = transaction
        .prepare("PRAGMA table_info(model_provider_profiles)")
        .map_err(storage_err(
            "agent_model_source_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(storage_err(
            "agent_model_source_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err(
            "agent_model_source_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .iter()
        .any(|column| column == "metadata_json");
    if !has_metadata {
        transaction
            .execute_batch(
                "ALTER TABLE model_provider_profiles
                     ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';",
            )
            .map_err(storage_err(
                "agent_model_source_metadata_apply_failed",
                "failed to add the Provider metadata column",
            ))?;
    }
    transaction
        .execute_batch(
            "
            CREATE TABLE IF NOT EXISTS agent_model_source_defaults (
                scope_kind TEXT NOT NULL,
                scope_id TEXT NOT NULL,
                agent_id TEXT NOT NULL,
                source_kind TEXT NOT NULL
                    CHECK(source_kind IN ('provider', 'route_group')),
                source_id TEXT NOT NULL,
                updated_at_ms INTEGER NOT NULL CHECK(updated_at_ms >= 0),
                PRIMARY KEY(scope_kind, scope_id, agent_id)
            );
            CREATE INDEX IF NOT EXISTS idx_agent_model_source_defaults_agent
                ON agent_model_source_defaults(agent_id, updated_at_ms);
            ",
        )
        .map_err(storage_err(
            "agent_model_source_default_migration_apply_failed",
            "failed to create the agent model source defaults table",
        ))?;

    let mut plan = plan_agent_model_source_backfill(&transaction)?;
    for entry in &plan.entries {
        AgentModelSourceDefaultRepository::set(
            &transaction,
            &AgentModelSourceDefault {
                scope: entry.scope.clone(),
                agent_id: entry.agent_id.clone(),
                source: Some(entry.source.clone()),
                updated_at_ms: entry.updated_at_ms,
            },
        )?;
    }
    plan.report.inserted = plan.entries.len();
    plan.report.log();

    transaction
        .execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![VERSION, NAME, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record the agent model source defaults migration",
        ))?;
    transaction.commit().map_err(storage_err(
        "agent_model_source_default_migration_commit_failed",
        "failed to commit the agent model source defaults migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// Builds the backfill deterministically without writing it.
///
/// Kept separate from the migration so the report and the precedence can be
/// tested against seeded legacy rows after a fresh database has already run 81.
pub(crate) fn plan_agent_model_source_backfill(
    conn: &Connection,
) -> VibexResult<AgentModelSourceMigrationPlan> {
    let mut report = AgentModelSourceMigrationReport::default();

    // Route defaults are Agent-global, so one map is enough; their scope is the
    // scope of whichever read asks for them later.
    let mut route_defaults: BTreeMap<AgentId, (ModelRouteId, i64)> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, model_route_id, updated_at_ms
                FROM agent_model_route_bindings
                WHERE is_default = 1 AND deleted_at_ms IS NULL
                ORDER BY agent_id ASC, model_route_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_source_backfill_route_defaults_failed",
                "failed to read the Agent route defaults for the backfill",
            ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                ))
            })
            .map_err(storage_err(
                "agent_model_source_backfill_route_defaults_failed",
                "failed to read the Agent route defaults for the backfill",
            ))?;
        for row in rows {
            let (agent_id, route_id, updated_at_ms) = row.map_err(storage_err(
                "agent_model_source_backfill_route_defaults_failed",
                "failed to decode an Agent route default for the backfill",
            ))?;
            let agent_id = AgentId::parse(agent_id)?;
            let route_id = ModelRouteId::parse(route_id)?;
            match ModelRouteRepository::get(conn, &route_id)? {
                Some(route) if route_is_usable(&route) => {
                    route_defaults.insert(agent_id, (route_id, updated_at_ms));
                }
                Some(_) => report.invalid_sources.push(AgentModelSourceMigrationIssue {
                    scope: None,
                    agent_id,
                    reason: format!(
                        "route default {} is disabled or has no enabled member",
                        route_id.as_str()
                    ),
                }),
                None => report.invalid_sources.push(AgentModelSourceMigrationIssue {
                    scope: None,
                    agent_id,
                    reason: format!("route default {} is missing or deleted", route_id.as_str()),
                }),
            }
        }
    }

    // Provider defaults are scoped, so each scope keeps its own map. The key is
    // the stored `(scope_kind, scope_id)` pair rather than the scope value
    // because the value is reconstructed only when it is needed.
    let mut provider_defaults: BTreeMap<
        (String, String),
        BTreeMap<AgentId, (ProviderProfileId, i64)>,
    > = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "
                SELECT scope_kind, scope_id, agent_id, provider_profile_id, updated_at_ms
                FROM agent_default_model_provider_profiles
                ORDER BY scope_kind ASC, scope_id ASC, agent_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_source_backfill_provider_defaults_failed",
                "failed to read the scoped Provider defaults for the backfill",
            ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            })
            .map_err(storage_err(
                "agent_model_source_backfill_provider_defaults_failed",
                "failed to read the scoped Provider defaults for the backfill",
            ))?;
        for row in rows {
            let (scope_kind, scope_id, agent_id, provider_id, updated_at_ms) =
                row.map_err(storage_err(
                    "agent_model_source_backfill_provider_defaults_failed",
                    "failed to decode a scoped Provider default for the backfill",
                ))?;
            let scope = scope_from_db(scope_kind.clone(), scope_id.clone())?;
            let agent_id = AgentId::parse(agent_id)?;
            let provider_id = ProviderProfileId::parse(provider_id)?;
            if provider_is_usable(conn, &agent_id, &provider_id)? {
                provider_defaults
                    .entry((scope_kind, scope_id))
                    .or_default()
                    .insert(agent_id, (provider_id, updated_at_ms));
            } else {
                report.invalid_sources.push(AgentModelSourceMigrationIssue {
                    scope: Some(scope),
                    agent_id,
                    reason: format!(
                        "Provider default {} is missing, deleted, disabled or belongs to another Agent",
                        provider_id.as_str()
                    ),
                });
            }
        }
    }

    // The sole-binding fallback counts both binding kinds: one Provider or one
    // route bound to the Agent is unambiguous either way. A route is counted
    // only when it is usable, because a disabled route is not an answer.
    let mut provider_bindings: BTreeMap<AgentId, Vec<ProviderProfileId>> = BTreeMap::new();
    let mut route_bindings: BTreeMap<AgentId, Vec<ModelRouteId>> = BTreeMap::new();
    {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_id, model_route_id
                FROM agent_model_route_bindings
                WHERE deleted_at_ms IS NULL
                ORDER BY agent_id ASC, model_route_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_source_backfill_route_bindings_failed",
                "failed to read the Agent route bindings for the backfill",
            ))?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(storage_err(
                "agent_model_source_backfill_route_bindings_failed",
                "failed to read the Agent route bindings for the backfill",
            ))?;
        for row in rows {
            let (agent_id, route_id) = row.map_err(storage_err(
                "agent_model_source_backfill_route_bindings_failed",
                "failed to decode an Agent route binding for the backfill",
            ))?;
            let agent_id = AgentId::parse(agent_id)?;
            let route_id = ModelRouteId::parse(route_id)?;
            if ModelRouteRepository::get(conn, &route_id)?
                .is_some_and(|route| route_is_usable(&route))
            {
                route_bindings.entry(agent_id).or_default().push(route_id);
            }
        }
    }
    for (agent_id, bindings) in bindings_by_agent(conn)? {
        provider_bindings
            .entry(agent_id)
            .or_default()
            .extend(bindings);
    }

    let mut agent_ids: BTreeSet<AgentId> = BTreeSet::new();
    agent_ids.extend(route_defaults.keys().cloned());
    agent_ids.extend(provider_bindings.keys().cloned());
    agent_ids.extend(route_bindings.keys().cloned());
    for agent_map in provider_defaults.values() {
        agent_ids.extend(agent_map.keys().cloned());
    }

    let mut scopes: BTreeMap<(String, String), ProviderProfileDefaultScope> = BTreeMap::new();
    let global = ProviderProfileDefaultScope {
        kind: ProviderDefaultScopeKind::Global,
        project_id: None,
        workspace_id: None,
    };
    let (global_kind, global_id) = scope_db_parts(&global)?;
    scopes.insert((global_kind, global_id), global);
    for (scope_kind, scope_id) in provider_defaults.keys() {
        scopes.insert(
            (scope_kind.clone(), scope_id.clone()),
            scope_from_db(scope_kind.clone(), scope_id.clone())?,
        );
    }

    let mut entries = Vec::new();
    for ((scope_kind, scope_id), scope) in &scopes {
        for agent_id in &agent_ids {
            let route = route_defaults.get(agent_id);
            let provider = provider_defaults
                .get(&(scope_kind.clone(), scope_id.clone()))
                .and_then(|agent_map| agent_map.get(agent_id));
            if route.is_some() {
                if let Some((route_id, updated_at_ms)) = route {
                    entries.push(AgentModelSourceBackfillEntry {
                        scope: scope.clone(),
                        agent_id: agent_id.clone(),
                        source: AgentModelSourceRef::RouteGroup(route_id.clone()),
                        updated_at_ms: *updated_at_ms,
                    });
                }
            } else if let Some((provider_id, updated_at_ms)) = provider {
                entries.push(AgentModelSourceBackfillEntry {
                    scope: scope.clone(),
                    agent_id: agent_id.clone(),
                    source: AgentModelSourceRef::Provider(provider_id.clone()),
                    updated_at_ms: *updated_at_ms,
                });
            } else {
                let binding_count = provider_bindings
                    .get(agent_id)
                    .map(Vec::len)
                    .unwrap_or_default()
                    + route_bindings
                        .get(agent_id)
                        .map(Vec::len)
                        .unwrap_or_default();
                if binding_count == 1 {
                    let (source, updated_at_ms) =
                        sole_binding(conn, &provider_bindings, &route_bindings, agent_id)?;
                    entries.push(AgentModelSourceBackfillEntry {
                        scope: scope.clone(),
                        agent_id: agent_id.clone(),
                        source,
                        updated_at_ms,
                    });
                }
            }
        }
    }

    for ((scope_kind, scope_id), agent_map) in &provider_defaults {
        let scope = scope_from_db(scope_kind.clone(), scope_id.clone())?;
        for agent_id in agent_map.keys() {
            if route_defaults.contains_key(agent_id) {
                report.double_defaults.push(AgentModelSourceMigrationIssue {
                    scope: Some(scope.clone()),
                    agent_id: agent_id.clone(),
                    reason: "both a route default and a scoped Provider default are set; the route default wins".to_string(),
                });
            }
        }
    }

    for agent_id in &agent_ids {
        let has_route_default = route_defaults.contains_key(agent_id);
        let has_provider_default = provider_defaults
            .values()
            .any(|agent_map| agent_map.contains_key(agent_id));
        if has_route_default || has_provider_default {
            continue;
        }
        let binding_count = provider_bindings
            .get(agent_id)
            .map(Vec::len)
            .unwrap_or_default()
            + route_bindings
                .get(agent_id)
                .map(Vec::len)
                .unwrap_or_default();
        if binding_count > 1 {
            report
                .no_default_with_bindings
                .push(AgentModelSourceMigrationIssue {
                    scope: None,
                    agent_id: agent_id.clone(),
                    reason: format!("{binding_count} bindings and no route or Provider default"),
                });
        }
    }

    report.double_defaults.sort_by(issue_sort_key);
    report.invalid_sources.sort_by(issue_sort_key);
    report.no_default_with_bindings.sort_by(issue_sort_key);
    report.inserted = entries.len();

    Ok(AgentModelSourceMigrationPlan { entries, report })
}

fn issue_sort_key(
    left: &AgentModelSourceMigrationIssue,
    right: &AgentModelSourceMigrationIssue,
) -> std::cmp::Ordering {
    (
        &left.agent_id,
        left.scope
            .as_ref()
            .map(ProviderProfileDefaultScope::storage_key),
        &left.reason,
    )
        .cmp(&(
            &right.agent_id,
            right
                .scope
                .as_ref()
                .map(ProviderProfileDefaultScope::storage_key),
            &right.reason,
        ))
}

fn sole_binding(
    conn: &Connection,
    provider_bindings: &BTreeMap<AgentId, Vec<ProviderProfileId>>,
    route_bindings: &BTreeMap<AgentId, Vec<ModelRouteId>>,
    agent_id: &AgentId,
) -> VibexResult<(AgentModelSourceRef, i64)> {
    if let Some(provider_ids) = provider_bindings.get(agent_id)
        && let Some(provider_id) = provider_ids.first()
    {
        let updated_at_ms = provider_binding_stamp(conn, agent_id, provider_id)?;
        return Ok((
            AgentModelSourceRef::Provider(provider_id.clone()),
            updated_at_ms,
        ));
    }
    if let Some(route_ids) = route_bindings.get(agent_id)
        && let Some(route_id) = route_ids.first()
    {
        let updated_at_ms = route_binding_stamp(conn, agent_id, route_id)?;
        return Ok((
            AgentModelSourceRef::RouteGroup(route_id.clone()),
            updated_at_ms,
        ));
    }
    Err(VibexError::storage(
        "agent_model_source_backfill_binding_missing",
        "the sole binding disappeared during the backfill",
    ))
}

fn provider_binding_stamp(
    conn: &Connection,
    agent_id: &AgentId,
    provider_id: &ProviderProfileId,
) -> VibexResult<i64> {
    conn.query_row(
        "
        SELECT updated_at_ms
        FROM agent_model_provider_bindings_v2
        WHERE agent_id = ?1 AND legacy_provider_profile_id = ?2
            AND deleted_at_ms IS NULL
        ORDER BY updated_at_ms DESC, agent_model_provider_binding_id ASC
        LIMIT 1
        ",
        params![agent_id.as_str(), provider_id.as_str()],
        |row| row.get(0),
    )
    .map_err(storage_err(
        "agent_model_source_backfill_binding_stamp_failed",
        "failed to read the Provider binding timestamp for the backfill",
    ))
}

fn route_binding_stamp(
    conn: &Connection,
    agent_id: &AgentId,
    route_id: &ModelRouteId,
) -> VibexResult<i64> {
    conn.query_row(
        "
        SELECT updated_at_ms
        FROM agent_model_route_bindings
        WHERE agent_id = ?1 AND model_route_id = ?2 AND deleted_at_ms IS NULL
        LIMIT 1
        ",
        params![agent_id.as_str(), route_id.as_str()],
        |row| row.get(0),
    )
    .map_err(storage_err(
        "agent_model_source_backfill_binding_stamp_failed",
        "failed to read the route binding timestamp for the backfill",
    ))
}

/// Live Provider bindings that name a real, user-visible, enabled Provider.
fn bindings_by_agent(conn: &Connection) -> VibexResult<BTreeMap<AgentId, Vec<ProviderProfileId>>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT b.agent_id, p.provider_profile_id
            FROM agent_model_provider_bindings_v2 b
            JOIN provider_profiles p
                ON p.provider_profile_id = b.legacy_provider_profile_id
            WHERE b.deleted_at_ms IS NULL
                AND p.deleted_at_ms IS NULL
                AND p.status = 'enabled'
                AND p.provider_profile_id NOT LIKE 'provider_local_default_%'
                AND p.provider_options_json NOT LIKE '%vibex.internal.profileRole%'
            ORDER BY b.agent_id ASC, p.provider_profile_id ASC
            ",
        )
        .map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to read the Agent Provider bindings for the default source",
        ))?;
    let rows = stmt
        .query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to read the Agent Provider bindings for the default source",
        ))?;
    let mut bindings: BTreeMap<AgentId, Vec<ProviderProfileId>> = BTreeMap::new();
    for row in rows {
        let (agent_id, provider_id) = row.map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to decode an Agent Provider binding for the default source",
        ))?;
        bindings
            .entry(AgentId::parse(agent_id)?)
            .or_default()
            .push(ProviderProfileId::parse(provider_id)?);
    }
    Ok(bindings)
}

fn user_provider_bindings(
    conn: &Connection,
    agent_id: &AgentId,
) -> VibexResult<Vec<(ProviderProfileId, i64)>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT p.provider_profile_id, b.updated_at_ms
            FROM agent_model_provider_bindings_v2 b
            JOIN provider_profiles p
                ON p.provider_profile_id = b.legacy_provider_profile_id
            WHERE b.agent_id = ?1
                AND b.deleted_at_ms IS NULL
                AND p.deleted_at_ms IS NULL
                AND p.status = 'enabled'
                AND p.provider_profile_id NOT LIKE 'provider_local_default_%'
                AND p.provider_options_json NOT LIKE '%vibex.internal.profileRole%'
            ORDER BY b.updated_at_ms DESC, p.provider_profile_id ASC
            ",
        )
        .map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to read the Agent Provider bindings for the default source",
        ))?;
    let rows = stmt
        .query_map(params![agent_id.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to read the Agent Provider bindings for the default source",
        ))?;
    let mut bindings = Vec::new();
    for row in rows {
        let (provider_id, updated_at_ms) = row.map_err(storage_err(
            "agent_model_source_binding_list_failed",
            "failed to decode an Agent Provider binding for the default source",
        ))?;
        bindings.push((ProviderProfileId::parse(provider_id)?, updated_at_ms));
    }
    Ok(bindings)
}

fn provider_is_usable(
    conn: &Connection,
    agent_id: &AgentId,
    provider_profile_id: &ProviderProfileId,
) -> VibexResult<bool> {
    let Some(profile) = ProviderProfileRepository::get(conn, provider_profile_id)? else {
        return Ok(false);
    };
    Ok(provider_profile_is_usable(&profile, agent_id))
}

fn provider_profile_is_usable(profile: &ProviderProfile, agent_id: &AgentId) -> bool {
    &profile.agent_id == agent_id
        && profile.status == ProviderProfileStatus::Enabled
        && !profile.id.as_str().starts_with("provider_local_default_")
        && !profile
            .provider_options
            .entries
            .iter()
            .any(|entry| entry.key.trim() == "vibex.internal.profileRole")
}

fn route_is_usable(route: &ModelRoute) -> bool {
    route.status == RouteStatus::Enabled && route.enabled_members().next().is_some()
}

fn default_from_row(
    scope_kind: String,
    scope_id: String,
    agent_id: AgentId,
    source_kind: String,
    source_id: String,
    updated_at_ms: i64,
) -> VibexResult<AgentModelSourceDefault> {
    Ok(AgentModelSourceDefault {
        scope: scope_from_db(scope_kind, scope_id)?,
        agent_id,
        source: Some(source_from_db(source_kind, source_id)?),
        updated_at_ms,
    })
}

fn scope_db_parts(scope: &ProviderProfileDefaultScope) -> VibexResult<(String, String)> {
    Ok((enum_to_db(&scope.kind)?, scope.storage_key()))
}

fn scope_from_db(scope_kind: String, scope_id: String) -> VibexResult<ProviderProfileDefaultScope> {
    let kind: ProviderDefaultScopeKind = enum_from_db(scope_kind)?;
    Ok(match kind {
        ProviderDefaultScopeKind::Global => ProviderProfileDefaultScope {
            kind,
            project_id: None,
            workspace_id: None,
        },
        ProviderDefaultScopeKind::Project => ProviderProfileDefaultScope {
            kind,
            project_id: Some(ProjectId::parse(scope_id)?),
            workspace_id: None,
        },
        ProviderDefaultScopeKind::Workspace => ProviderProfileDefaultScope {
            kind,
            project_id: None,
            workspace_id: Some(WorkspaceId::parse(scope_id)?),
        },
    })
}

fn source_db_parts(source: &AgentModelSourceRef) -> (&'static str, &str) {
    match source {
        AgentModelSourceRef::Provider(provider_profile_id) => {
            ("provider", provider_profile_id.as_str())
        }
        AgentModelSourceRef::RouteGroup(model_route_id) => ("route_group", model_route_id.as_str()),
    }
}

fn source_from_db(source_kind: String, source_id: String) -> VibexResult<AgentModelSourceRef> {
    match source_kind.as_str() {
        "provider" => Ok(AgentModelSourceRef::Provider(ProviderProfileId::parse(
            source_id,
        )?)),
        "route_group" => Ok(AgentModelSourceRef::RouteGroup(ModelRouteId::parse(
            source_id,
        )?)),
        other => Err(VibexError::storage(
            "agent_model_source_default_source_kind_invalid",
            "an Agent default model source row names an unknown source kind",
        )
        .with_diagnostic("sourceKind", other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{
        AgentModelRouteBinding, ModelProviderProfileId, ModelRouteMember, ProviderDefaultScopeKind,
        ProviderKind, RouteKind, RouteStrategy,
    };

    use crate::ProviderProjectionCompatibilityRepository;

    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    fn global_scope() -> ProviderProfileDefaultScope {
        ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Global,
            project_id: None,
            workspace_id: None,
        }
    }

    fn workspace_scope() -> ProviderProfileDefaultScope {
        ProviderProfileDefaultScope {
            kind: ProviderDefaultScopeKind::Workspace,
            project_id: None,
            workspace_id: Some(WorkspaceId::parse("workspace_agent_model_source").unwrap()),
        }
    }

    fn seed_provider(conn: &Connection, agent: &str, name: &str) -> ProviderProfile {
        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.id = ProviderProfileId::new();
        profile.agent_id = AgentId::parse(agent).unwrap();
        profile.display_name = name.to_string();
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://api.example.test/v1".to_string());
        profile.default_model = Some("test-model".to_string());
        ProviderProfileRepository::insert(conn, &profile).unwrap();
        profile
    }

    /// Returns `(legacy Provider id, shared Provider id)` for the binding.
    fn seed_binding(
        conn: &Connection,
        profile: &ProviderProfile,
    ) -> (ProviderProfileId, ModelProviderProfileId) {
        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(conn, profile).unwrap();
        (
            records
                .binding
                .legacy_provider_profile_id
                .unwrap_or_else(|| profile.id.clone()),
            records.model_provider.id,
        )
    }

    fn seed_route(
        conn: &Connection,
        provider_id: &ModelProviderProfileId,
        agent: &AgentId,
        is_default: bool,
    ) -> ModelRoute {
        let now = unix_timestamp_ms().max(1);
        let route = ModelRoute {
            display_name: "Route".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Route".to_string()),
            strategy: RouteStrategy::Ordered,
            status: RouteStatus::Enabled,
            members: vec![ModelRouteMember::new(provider_id.clone(), "m", 0)],
            created_at_ms: now,
            updated_at_ms: now,
            ..Default::default()
        };
        ModelRouteRepository::insert(conn, &route).unwrap();
        let mut binding = AgentModelRouteBinding::new(agent.clone(), route.id.clone());
        binding.is_default = is_default;
        binding.created_at_ms = now;
        binding.updated_at_ms = now;
        AgentModelRouteBindingRepository::insert(conn, &binding).unwrap();
        route
    }

    fn rerun_migration(conn: &mut Connection) -> AgentModelSourceMigrationPlan {
        let plan = plan_agent_model_source_backfill(conn).unwrap();
        conn.execute("DELETE FROM schema_migrations WHERE version = 81", [])
            .unwrap();
        let mut applied = Vec::new();
        apply_agent_model_source_defaults(conn, &mut applied).unwrap();
        assert_eq!(applied, vec!["81:agent_model_source_defaults".to_string()]);
        plan
    }

    #[test]
    fn agent_model_source_default_repository_round_trips_and_is_unique_per_scope() {
        let conn = migrated_connection();
        let agent = AgentId::parse("claude").unwrap();
        let provider = ProviderProfileId::parse("provider_agent_model_source").unwrap();
        let default = AgentModelSourceDefault {
            scope: global_scope(),
            agent_id: agent.clone(),
            source: Some(AgentModelSourceRef::Provider(provider.clone())),
            updated_at_ms: 10,
        };
        AgentModelSourceDefaultRepository::set(&conn, &default).unwrap();
        assert_eq!(
            AgentModelSourceDefaultRepository::get(&conn, global_scope(), agent.clone()).unwrap(),
            Some(default.clone())
        );

        let replacement = AgentModelSourceDefault {
            updated_at_ms: 20,
            ..default.clone()
        };
        AgentModelSourceDefaultRepository::set(&conn, &replacement).unwrap();
        let rows = AgentModelSourceDefaultRepository::list_for_agent(&conn, &agent).unwrap();
        assert_eq!(rows.len(), 1, "the primary key keeps one row per scope");
        assert_eq!(rows[0].updated_at_ms, 20);
        assert_eq!(
            AgentModelSourceDefaultRepository::list_all(&conn).unwrap(),
            rows
        );

        assert!(
            AgentModelSourceDefaultRepository::clear(&conn, global_scope(), agent.clone()).unwrap()
        );
        assert!(
            !AgentModelSourceDefaultRepository::clear(&conn, global_scope(), agent).unwrap(),
            "clearing twice is a no-op"
        );
    }

    #[test]
    fn agent_model_source_backfill_prefers_route_then_scoped_provider_then_sole_binding() {
        let mut conn = migrated_connection();
        let agent = AgentId::parse("claude").unwrap();
        let first = seed_provider(&conn, "claude", "First");
        let (_, shared_id) = seed_binding(&conn, &first);
        let second = seed_provider(&conn, "claude", "Second");
        let _ = seed_binding(&conn, &second);

        AgentDefaultModelProviderProfileRepository::set(
            &conn,
            global_scope(),
            agent.clone(),
            second.id.clone(),
        )
        .unwrap();
        let route = seed_route(&conn, &shared_id, &agent, true);

        let plan = rerun_migration(&mut conn);
        assert_eq!(plan.report.inserted, 1);
        assert_eq!(plan.report.double_defaults.len(), 1);
        let rows = AgentModelSourceDefaultRepository::list_all(&conn).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0].source,
            Some(AgentModelSourceRef::RouteGroup(route.id.clone()))
        );

        // The old fields stay untouched: this wave only adds the unified row.
        let legacy =
            AgentDefaultModelProviderProfileRepository::get(&conn, global_scope(), agent.clone())
                .unwrap();
        assert_eq!(legacy.provider_profile_id, Some(second.id));
        assert!(
            AgentModelRouteBindingRepository::list_for_agent(&conn, &agent)
                .unwrap()
                .iter()
                .any(|binding| binding.is_default)
        );
    }

    #[test]
    fn agent_model_source_backfill_writes_a_sole_binding_and_skips_ambiguity() {
        let mut conn = migrated_connection();
        let only = seed_provider(&conn, "claude", "Only");
        let (only_id, _) = seed_binding(&conn, &only);

        let plan = plan_agent_model_source_backfill(&conn).unwrap();
        assert_eq!(plan.report.inserted, 1);
        assert!(plan.report.no_default_with_bindings.is_empty());
        assert_eq!(
            plan.entries[0].source,
            AgentModelSourceRef::Provider(only_id.clone())
        );

        // A second binding makes the same Agent ambiguous.
        let second = seed_provider(&conn, "claude", "Second");
        let _ = seed_binding(&conn, &second);
        let plan = plan_agent_model_source_backfill(&conn).unwrap();
        assert!(plan.entries.is_empty());
        assert_eq!(plan.report.no_default_with_bindings.len(), 1);
        assert!(plan.report.no_default_with_bindings[0].reason.contains('2'));

        // Applying the migration with the second binding present writes nothing.
        rerun_migration(&mut conn);
        assert!(
            AgentModelSourceDefaultRepository::list_all(&conn)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn agent_model_source_backfill_reports_invalid_provider_defaults() {
        let mut conn = migrated_connection();
        let agent = AgentId::parse("claude").unwrap();
        let provider = seed_provider(&conn, "claude", "Disabled");
        let _ = seed_binding(&conn, &provider);
        AgentDefaultModelProviderProfileRepository::set(
            &conn,
            global_scope(),
            agent.clone(),
            provider.id.clone(),
        )
        .unwrap();
        conn.execute(
            "UPDATE provider_profiles SET status = 'disabled' WHERE provider_profile_id = ?1",
            params![provider.id.as_str()],
        )
        .unwrap();

        let plan = rerun_migration(&mut conn);
        assert_eq!(plan.report.invalid_sources.len(), 1);
        assert!(plan.report.invalid_sources[0].reason.contains("disabled"));
        // The binding names the same disabled Provider, so it is not a usable
        // fallback either: the Agent correctly ends up with no source rather
        // than a default that cannot start.
        assert!(
            AgentModelSourceDefaultRepository::list_all(&conn)
                .unwrap()
                .is_empty()
        );
    }

    #[test]
    fn agent_model_source_resolution_prefers_explicit_then_legacy_sources() {
        let conn = migrated_connection();
        let agent = AgentId::parse("claude").unwrap();
        let first = seed_provider(&conn, "claude", "First");
        let (first_id, _) = seed_binding(&conn, &first);
        let second = seed_provider(&conn, "claude", "Second");
        let (_, shared_id) = seed_binding(&conn, &second);
        let route = seed_route(&conn, &shared_id, &agent, false);

        // Two bindings and no default: no answer.
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, global_scope(), agent.clone())
                .unwrap()
                .source,
            None
        );

        AgentDefaultModelProviderProfileRepository::set(
            &conn,
            workspace_scope(),
            agent.clone(),
            second.id.clone(),
        )
        .unwrap();
        // The Provider default is scoped, so the global read still has none.
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, global_scope(), agent.clone())
                .unwrap()
                .source,
            None
        );
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, workspace_scope(), agent.clone())
                .unwrap()
                .source,
            Some(AgentModelSourceRef::Provider(second.id.clone()))
        );

        AgentModelRouteBindingRepository::set_default(&conn, &agent, &route.id).unwrap();
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, workspace_scope(), agent.clone())
                .unwrap()
                .source,
            Some(AgentModelSourceRef::RouteGroup(route.id.clone()))
        );

        let explicit = AgentModelSourceDefault {
            scope: workspace_scope(),
            agent_id: agent.clone(),
            source: Some(AgentModelSourceRef::Provider(first_id)),
            updated_at_ms: 99,
        };
        AgentModelSourceDefaultRepository::set(&conn, &explicit).unwrap();
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, workspace_scope(), agent).unwrap(),
            explicit
        );
    }

    /// A stale explicit source is ignored rather than promoted by fallback.
    #[test]
    fn an_unusable_explicit_default_falls_through_to_the_route_default() {
        let conn = migrated_connection();
        let agent = AgentId::parse("claude").unwrap();
        let first = seed_provider(&conn, "claude", "First");
        let (first_legacy, first_shared) = seed_binding(&conn, &first);
        let route = seed_route(&conn, &first_shared, &agent, true);
        let explicit = AgentModelSourceDefault {
            scope: global_scope(),
            agent_id: agent.clone(),
            source: Some(AgentModelSourceRef::Provider(first_legacy.clone())),
            updated_at_ms: 50,
        };
        AgentModelSourceDefaultRepository::set(&conn, &explicit).unwrap();
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, global_scope(), agent.clone())
                .unwrap()
                .source,
            Some(AgentModelSourceRef::Provider(first_legacy))
        );

        conn.execute(
            "UPDATE provider_profiles SET status = 'disabled' WHERE provider_profile_id = ?1",
            params![first.id.as_str()],
        )
        .unwrap();
        assert_eq!(
            AgentModelSourceDefaultRepository::resolve(&conn, global_scope(), agent)
                .unwrap()
                .source,
            Some(AgentModelSourceRef::RouteGroup(route.id)),
            "the stale explicit row must not beat the usable route default"
        );
    }
}

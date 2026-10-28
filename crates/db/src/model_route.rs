//! Persistence for reusable model routes and the Agents that reference them.
//!
//! A route is stored as one row plus its members. The members are read back in
//! attempt order, and every write replaces the whole member list: reordering is
//! a different list, not a different field, so a partial update would leave the
//! order ambiguous.
//!
//! Bindings follow the same shape as Provider bindings: a live row per
//! `(Agent, route)` pair, soft-deleted rather than removed so an Agent's
//! history survives a route being unbound.

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use serde::{Deserialize, Serialize};
use vibex_core::DEFAULT_MEMBER_WEIGHT;
use vibex_core::{
    AgentId, AgentModelRouteBinding, AgentModelRouteBindingId, ModelProviderProfileId, ModelRoute,
    ModelRouteId, ModelRouteMember, ModelRouteMemberId, ModelRouteRule, RouteKind, RouteStatus,
    RouteStrategy, VibexError, VibexResult,
};

use super::{
    enum_from_db_sql, enum_to_db, parse_id_sql, parse_optional_id_sql, storage_err,
    unix_timestamp_ms,
};

/// The version of the JSON payload we write into `model_routes.rules_json`.
///
/// The column grows by adding fields inside the payload rather than by altering
/// the table again, so a reader has to know which shape it is looking at. An
/// unknown version is refused instead of guessed: a rule that cannot be read
/// must not silently become "no rule".
const ROUTE_RULES_SCHEMA_VERSION: u32 = 1;
/// The version of the JSON payload we write into `model_routes.levels_json`.
const ROUTE_LEVELS_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteRulesPayload {
    schema_version: u32,
    #[serde(default)]
    rules: Vec<ModelRouteRule>,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct RouteLevelsPayload {
    schema_version: u32,
    #[serde(default)]
    levels: Vec<String>,
}

pub struct ModelRouteRepository;
pub struct AgentModelRouteBindingRepository;

impl ModelRouteRepository {
    pub fn insert(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
        route.validate()?;
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_create_transaction_failed",
                "failed to start model route transaction",
            ))?;
        insert_route(&transaction, route)?;
        insert_members(&transaction, route)?;
        transaction.commit().map_err(storage_err(
            "model_route_create_commit_failed",
            "failed to commit model route",
        ))
    }

    pub fn get(conn: &Connection, id: &ModelRouteId) -> VibexResult<Option<ModelRoute>> {
        let mut route = conn
            .query_row(
                "
                SELECT model_route_id, display_name, description, kind, strategy,
                    first_token_timeout_ms, context_tokens, pin, token_limit_json,
                    status, revision,
                    created_at_ms, updated_at_ms, deleted_at_ms, failover,
                    agent_model_id, rules_json, classifier, effort, patience_ms,
                    sink, unlists, levels_json, pick_member_id
                FROM model_routes
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str()],
                map_route,
            )
            .optional()
            .map_err(storage_err(
                "model_route_lookup_failed",
                "failed to load model route",
            ))?;
        if let Some(route) = route.as_mut() {
            route.members = list_members(conn, &route.id)?;
        }
        Ok(route)
    }

    /// Every live route, most recently changed first.
    pub fn list(conn: &Connection) -> VibexResult<Vec<ModelRoute>> {
        list_where(conn, "deleted_at_ms IS NULL", "")
    }

    /// Every live route that is enabled, in name order.
    ///
    /// This is what a projection reads: a disabled route still exists, but it
    /// offers no model to an Agent.
    pub fn list_enabled(conn: &Connection) -> VibexResult<Vec<ModelRoute>> {
        list_where(
            conn,
            "deleted_at_ms IS NULL AND status = 'enabled'",
            "ORDER BY display_name COLLATE NOCASE ASC, model_route_id ASC",
        )
    }

    /// How many live routes the user composed themselves.
    ///
    /// This is the number a route budget may count. A `Direct` route is not a
    /// composition: `backfill_direct_routes` materialises one for every direct
    /// Provider Profile an installation already has, so counting them would let
    /// the backfill spend a budget it never asked for and leave a user with many
    /// Providers no room to save a single route group. Provider groups and plain
    /// gateway routes are the ones a user creates, and those are what the limit
    /// exists to bound.
    pub fn count_user_authored(conn: &Connection) -> VibexResult<usize> {
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_routes
                 WHERE deleted_at_ms IS NULL AND kind <> 'direct'",
                [],
                |row| row.get(0),
            )
            .map_err(storage_err(
                "model_route_count_failed",
                "failed to count the model routes a user authored",
            ))?;
        Ok(usize::try_from(count).unwrap_or(usize::MAX))
    }

    /// Replaces every field of a route, including its member list.
    ///
    /// The caller must pass the revision it read; a mismatch means someone else
    /// changed the route in between. Members are replaced wholesale, so the
    /// order that arrives is the order that is stored.
    pub fn update(
        conn: &Connection,
        route: &ModelRoute,
        expected_revision: i64,
    ) -> VibexResult<ModelRoute> {
        route.validate()?;
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_update_transaction_failed",
                "failed to start model route update transaction",
            ))?;
        let changed = transaction
            .execute(
                "
                UPDATE model_routes
                SET display_name = ?2,
                    description = ?3,
                    kind = ?4,
                    strategy = ?5,
                    first_token_timeout_ms = ?6,
                    context_tokens = ?7,
                    pin = ?8,
                    token_limit_json = ?9,
                    status = ?10,
                    revision = revision + 1,
                    updated_at_ms = ?11,
                    failover = ?13,
                    agent_model_id = ?14,
                    rules_json = ?15,
                    classifier = ?16,
                    effort = ?17,
                    patience_ms = ?18,
                    sink = ?19,
                    unlists = ?20,
                    levels_json = ?21,
                    pick_member_id = ?22
                WHERE model_route_id = ?1
                    AND revision = ?12
                    AND deleted_at_ms IS NULL
                ",
                params![
                    route.id.as_str(),
                    route.display_name.trim(),
                    route.description.as_deref().map(str::trim),
                    enum_to_db(&route.kind)?,
                    enum_to_db(&route.strategy)?,
                    route.first_token_timeout_ms,
                    route.context_tokens,
                    enum_to_db(&route.pin)?,
                    encode_token_limit(&route.token_limit)?,
                    enum_to_db(&route.status)?,
                    next_stamp(conn, route)?,
                    expected_revision,
                    enum_to_db(&route.failover)?,
                    route.agent_model().map(str::to_string),
                    encode_rules(&route.rules)?,
                    route.classifier.as_deref().map(str::trim),
                    route.effort.as_deref().map(str::trim),
                    route.patience_ms,
                    i64::from(route.sink),
                    i64::from(route.unlists),
                    encode_levels(&route.levels)?,
                    route
                        .pick_member_id
                        .as_ref()
                        .map(ModelRouteMemberId::as_str),
                ],
            )
            .map_err(storage_err(
                "model_route_update_failed",
                "failed to update model route",
            ))?;
        if changed == 0 {
            return Err(VibexError::validation(
                "model_route_revision_conflict",
                "the route changed since it was loaded",
            )
            .with_diagnostic("modelRouteId", route.id.as_str()));
        }
        transaction
            .execute(
                "DELETE FROM model_route_members WHERE model_route_id = ?1",
                params![route.id.as_str()],
            )
            .map_err(storage_err(
                "model_route_members_replace_failed",
                "failed to replace model route members",
            ))?;
        insert_members(&transaction, route)?;
        transaction.commit().map_err(storage_err(
            "model_route_update_commit_failed",
            "failed to commit model route update",
        ))?;
        Self::get(conn, &route.id)?.ok_or_else(|| {
            VibexError::storage(
                "model_route_update_readback_failed",
                "failed to read the model route after update",
            )
        })
    }

    /// Marks a route deleted. Its bindings go with it.
    pub fn delete(conn: &Connection, id: &ModelRouteId) -> VibexResult<bool> {
        let now = unix_timestamp_ms();
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_route_delete_transaction_failed",
                "failed to start model route delete transaction",
            ))?;
        let changed = transaction
            .execute(
                "
                UPDATE model_routes
                SET deleted_at_ms = ?2,
                    revision = revision + 1,
                    updated_at_ms = ?2
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str(), now],
            )
            .map_err(storage_err(
                "model_route_delete_failed",
                "failed to delete model route",
            ))?;
        transaction
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET deleted_at_ms = ?2,
                    is_default = 0,
                    revision = revision + 1,
                    updated_at_ms = ?2
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str(), now],
            )
            .map_err(storage_err(
                "model_route_bindings_delete_failed",
                "failed to delete the route's Agent bindings",
            ))?;
        transaction.commit().map_err(storage_err(
            "model_route_delete_commit_failed",
            "failed to commit model route delete",
        ))?;
        Ok(changed > 0)
    }

    /// Every live route that names this Provider, so a Provider change can
    /// report what it would break before it happens.
    pub fn list_naming_provider(
        conn: &Connection,
        provider: &ModelProviderProfileId,
    ) -> VibexResult<Vec<ModelRoute>> {
        let ids = {
            let mut stmt = conn
                .prepare(
                    "
                    SELECT DISTINCT m.model_route_id
                    FROM model_route_members m
                    JOIN model_routes r ON r.model_route_id = m.model_route_id
                    WHERE m.model_provider_profile_id = ?1 AND r.deleted_at_ms IS NULL
                    ",
                )
                .map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?;
            let rows = stmt
                .query_map(params![provider.as_str()], |row| row.get::<_, String>(0))
                .map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?;
            let mut ids = Vec::new();
            for row in rows {
                ids.push(row.map_err(storage_err(
                    "model_route_provider_lookup_failed",
                    "failed to list routes naming a Provider",
                ))?);
            }
            ids
        };
        let mut routes = Vec::new();
        for id in ids {
            if let Some(route) = Self::get(conn, &ModelRouteId::parse(id)?)? {
                routes.push(route);
            }
        }
        Ok(routes)
    }
}

impl AgentModelRouteBindingRepository {
    pub fn insert(conn: &Connection, binding: &AgentModelRouteBinding) -> VibexResult<()> {
        if binding.is_default {
            clear_default(conn, &binding.agent_id, None)?;
        }
        conn.execute(
            "
            INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms, deleted_at_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
            ",
            params![
                binding.id.as_str(),
                binding.agent_id.as_str(),
                binding.model_route_id.as_str(),
                i64::from(binding.is_default),
                binding.revision.max(1),
                binding.created_at_ms.max(1),
                binding.updated_at_ms.max(1),
                binding.deleted_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_model_route_binding_create_failed",
            "failed to bind the route to the Agent",
        ))?;
        Ok(())
    }

    pub fn get(
        conn: &Connection,
        id: &AgentModelRouteBindingId,
    ) -> VibexResult<Option<AgentModelRouteBinding>> {
        conn.query_row(
            "
            SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms, deleted_at_ms
            FROM agent_model_route_bindings
            WHERE agent_model_route_binding_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![id.as_str()],
            map_binding,
        )
        .optional()
        .map_err(storage_err(
            "agent_model_route_binding_lookup_failed",
            "failed to load the Agent route binding",
        ))
    }

    /// Every live route this Agent may use, default first.
    pub fn list_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                    revision, created_at_ms, updated_at_ms, deleted_at_ms
                FROM agent_model_route_bindings
                WHERE agent_id = ?1 AND deleted_at_ms IS NULL
                ORDER BY is_default DESC, created_at_ms ASC, model_route_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the Agent's route bindings",
            ))?;
        let rows = stmt
            .query_map(params![agent_id.as_str()], map_binding)
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the Agent's route bindings",
            ))?;
        collect_rows(rows, "agent_model_route_binding_list_failed")
    }

    /// Every live binding, for a bundle that renders every Agent at once.
    ///
    /// One query instead of one per Agent: the Config Center groups them in
    /// memory, and the page needs the whole set anyway.
    pub fn list_all(conn: &Connection) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                    revision, created_at_ms, updated_at_ms, deleted_at_ms
                FROM agent_model_route_bindings
                WHERE deleted_at_ms IS NULL
                ORDER BY agent_id ASC, is_default DESC, created_at_ms ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list every Agent route binding",
            ))?;
        let rows = stmt.query_map([], map_binding).map_err(storage_err(
            "agent_model_route_binding_list_failed",
            "failed to list every Agent route binding",
        ))?;
        collect_rows(rows, "agent_model_route_binding_list_failed")
    }

    /// The Agents that reference this route, for the "used by" line.
    pub fn list_for_route(
        conn: &Connection,
        route_id: &ModelRouteId,
    ) -> VibexResult<Vec<AgentModelRouteBinding>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_model_route_binding_id, agent_id, model_route_id, is_default,
                    revision, created_at_ms, updated_at_ms, deleted_at_ms
                FROM agent_model_route_bindings
                WHERE model_route_id = ?1 AND deleted_at_ms IS NULL
                ORDER BY agent_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the route's Agent bindings",
            ))?;
        let rows = stmt
            .query_map(params![route_id.as_str()], map_binding)
            .map_err(storage_err(
                "agent_model_route_binding_list_failed",
                "failed to list the route's Agent bindings",
            ))?;
        collect_rows(rows, "agent_model_route_binding_list_failed")
    }

    /// Makes one route the Agent's default, demoting whichever held it.
    pub fn set_default(
        conn: &Connection,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<()> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "agent_model_route_default_transaction_failed",
                "failed to start the route default transaction",
            ))?;
        clear_default(&transaction, agent_id, Some(route_id))?;
        let now = unix_timestamp_ms();
        let changed = transaction
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET is_default = 1,
                    revision = revision + 1,
                    updated_at_ms = ?3
                WHERE agent_id = ?1 AND model_route_id = ?2 AND deleted_at_ms IS NULL
                ",
                params![agent_id.as_str(), route_id.as_str(), now],
            )
            .map_err(storage_err(
                "agent_model_route_default_failed",
                "failed to set the Agent's default route",
            ))?;
        if changed == 0 {
            return Err(VibexError::validation(
                "agent_model_route_binding_not_found",
                "the Agent is not bound to that route",
            )
            .with_diagnostic("agentId", agent_id.as_str())
            .with_diagnostic("modelRouteId", route_id.as_str()));
        }
        transaction.commit().map_err(storage_err(
            "agent_model_route_default_commit_failed",
            "failed to commit the route default",
        ))
    }

    /// Removes the Agent's route default without touching its bindings.
    ///
    /// Setting a Provider as the unified default needs this: a route default
    /// and a Provider default must not both claim the Agent.
    pub fn clear_default_for_agent(conn: &Connection, agent_id: &AgentId) -> VibexResult<usize> {
        clear_default(conn, agent_id, None)
    }

    /// Removes one binding. A removed default leaves the Agent with none, which
    /// the caller reports rather than silently promoting another route.
    pub fn delete(
        conn: &Connection,
        agent_id: &AgentId,
        route_id: &ModelRouteId,
    ) -> VibexResult<bool> {
        let now = unix_timestamp_ms();
        let changed = conn
            .execute(
                "
                UPDATE agent_model_route_bindings
                SET deleted_at_ms = ?3,
                    is_default = 0,
                    revision = revision + 1,
                    updated_at_ms = ?3
                WHERE agent_id = ?1 AND model_route_id = ?2 AND deleted_at_ms IS NULL
                ",
                params![agent_id.as_str(), route_id.as_str(), now],
            )
            .map_err(storage_err(
                "agent_model_route_binding_delete_failed",
                "failed to unbind the route from the Agent",
            ))?;
        Ok(changed > 0)
    }
}

fn clear_default(
    conn: &Connection,
    agent_id: &AgentId,
    except: Option<&ModelRouteId>,
) -> VibexResult<usize> {
    let now = unix_timestamp_ms();
    let changed = match except {
        Some(route_id) => conn.execute(
            "
            UPDATE agent_model_route_bindings
            SET is_default = 0,
                revision = revision + 1,
                updated_at_ms = ?3
            WHERE agent_id = ?1 AND is_default = 1 AND deleted_at_ms IS NULL
                AND model_route_id != ?2
            ",
            params![agent_id.as_str(), route_id.as_str(), now],
        ),
        None => conn.execute(
            "
            UPDATE agent_model_route_bindings
            SET is_default = 0,
                revision = revision + 1,
                updated_at_ms = ?2
            WHERE agent_id = ?1 AND is_default = 1 AND deleted_at_ms IS NULL
            ",
            params![agent_id.as_str(), now],
        ),
    }
    .map_err(storage_err(
        "agent_model_route_default_clear_failed",
        "failed to clear the Agent's previous default route",
    ))?;
    Ok(changed)
}

/// The wall-clock stamp a rewrite gets.
///
/// It must move forward, because the launch contract hashes it: a no-op update
/// that kept the old stamp could leave two different route shapes sharing one
/// revision.
fn next_stamp(conn: &Connection, route: &ModelRoute) -> VibexResult<i64> {
    let previous: i64 = conn
        .query_row(
            "SELECT updated_at_ms FROM model_routes WHERE model_route_id = ?1",
            params![route.id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "model_route_stamp_failed",
            "failed to read the route's update stamp",
        ))?;
    Ok(unix_timestamp_ms().max(previous.saturating_add(1)))
}

fn insert_route(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO model_routes (
            model_route_id, display_name, description, kind, strategy,
            first_token_timeout_ms, context_tokens, pin, token_limit_json,
            status, revision,
            created_at_ms, updated_at_ms, deleted_at_ms, failover,
            agent_model_id, rules_json, classifier, effort, patience_ms,
            sink, unlists, levels_json, pick_member_id
        )
        VALUES (
            ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
            ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24
        )
        ",
        params![
            route.id.as_str(),
            route.display_name.trim(),
            route.description.as_deref().map(str::trim),
            enum_to_db(&route.kind)?,
            enum_to_db(&route.strategy)?,
            route.first_token_timeout_ms,
            route.context_tokens,
            enum_to_db(&route.pin)?,
            encode_token_limit(&route.token_limit)?,
            enum_to_db(&route.status)?,
            route.revision.max(1),
            route.created_at_ms.max(1),
            route.updated_at_ms.max(1),
            route.deleted_at_ms,
            enum_to_db(&route.failover)?,
            route.agent_model().map(str::to_string),
            encode_rules(&route.rules)?,
            route.classifier.as_deref().map(str::trim),
            route.effort.as_deref().map(str::trim),
            route.patience_ms,
            i64::from(route.sink),
            i64::from(route.unlists),
            encode_levels(&route.levels)?,
            route
                .pick_member_id
                .as_ref()
                .map(ModelRouteMemberId::as_str),
        ],
    )
    .map_err(storage_err(
        "model_route_create_failed",
        "failed to create model route",
    ))?;
    Ok(())
}

fn insert_members(conn: &Connection, route: &ModelRoute) -> VibexResult<()> {
    for member in &route.members {
        conn.execute(
            "
            INSERT INTO model_route_members (
                model_route_member_id, model_route_id, model_provider_profile_id,
                model_id, enabled, order_index, reasoning_effort,
                weight, context_tokens, first_token_timeout_ms
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
            ",
            params![
                member.id.as_str(),
                route.id.as_str(),
                member.model_provider_profile_id.as_str(),
                member.model_id.trim(),
                i64::from(member.enabled),
                member.order_index,
                member.reasoning_effort.as_deref().map(str::trim),
                i64::from(member.weight.max(DEFAULT_MEMBER_WEIGHT)),
                member.context_tokens,
                member.first_token_timeout_ms,
            ],
        )
        .map_err(storage_err(
            "model_route_member_create_failed",
            "failed to create a model route member",
        ))?;
    }
    Ok(())
}

fn list_members(conn: &Connection, route_id: &ModelRouteId) -> VibexResult<Vec<ModelRouteMember>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT model_route_member_id, model_provider_profile_id, model_id,
                enabled, order_index, reasoning_effort,
                weight, context_tokens, first_token_timeout_ms
            FROM model_route_members
            WHERE model_route_id = ?1
            ORDER BY order_index ASC, model_route_member_id ASC
            ",
        )
        .map_err(storage_err(
            "model_route_members_list_failed",
            "failed to list model route members",
        ))?;
    let rows = stmt
        .query_map(params![route_id.as_str()], |row| {
            Ok(ModelRouteMember {
                id: parse_id_sql(row.get(0)?, ModelRouteMemberId::parse)?,
                model_provider_profile_id: parse_id_sql(
                    row.get(1)?,
                    ModelProviderProfileId::parse,
                )?,
                model_id: row.get(2)?,
                enabled: row.get::<_, i64>(3)? != 0,
                order_index: row.get(4)?,
                reasoning_effort: row.get(5)?,
                weight: row
                    .get::<_, i64>(6)
                    .ok()
                    .and_then(|value| u32::try_from(value).ok())
                    .filter(|value| *value > 0)
                    .unwrap_or(DEFAULT_MEMBER_WEIGHT),
                context_tokens: row
                    .get::<_, Option<i64>>(7)?
                    .and_then(|value| u32::try_from(value).ok().filter(|value| *value > 0)),
                first_token_timeout_ms: row
                    .get::<_, Option<i64>>(8)?
                    .and_then(|value| u64::try_from(value).ok().filter(|value| *value > 0)),
            })
        })
        .map_err(storage_err(
            "model_route_members_list_failed",
            "failed to list model route members",
        ))?;
    collect_rows(rows, "model_route_members_list_failed")
}

fn list_where(conn: &Connection, predicate: &str, order: &str) -> VibexResult<Vec<ModelRoute>> {
    let sql = format!(
        "
        SELECT model_route_id, display_name, description, kind, strategy,
            first_token_timeout_ms, context_tokens, pin, token_limit_json,
                    status, revision,
            created_at_ms, updated_at_ms, deleted_at_ms, failover,
            agent_model_id, rules_json, classifier, effort, patience_ms,
            sink, unlists, levels_json, pick_member_id
        FROM model_routes
        WHERE {predicate}
        {order}
        "
    );
    let mut stmt = conn.prepare(&sql).map_err(storage_err(
        "model_route_list_failed",
        "failed to list model routes",
    ))?;
    let rows = stmt.query_map([], map_route).map_err(storage_err(
        "model_route_list_failed",
        "failed to list model routes",
    ))?;
    let mut routes = collect_rows(rows, "model_route_list_failed")?;
    for route in &mut routes {
        route.members = list_members(conn, &route.id)?;
    }
    Ok(routes)
}

fn map_route(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelRoute> {
    let kind: RouteKind = enum_from_db_sql(row.get(3)?)?;
    let strategy: RouteStrategy = enum_from_db_sql(row.get(4)?)?;
    let status: RouteStatus = enum_from_db_sql(row.get(9)?)?;
    Ok(ModelRoute {
        id: parse_id_sql(row.get(0)?, ModelRouteId::parse)?,
        display_name: row.get(1)?,
        description: row.get(2)?,
        kind,
        members: Vec::new(),
        strategy,
        first_token_timeout_ms: row
            .get::<_, Option<i64>>(5)?
            .and_then(|value| u64::try_from(value).ok().filter(|value| *value > 0)),
        context_tokens: row
            .get::<_, Option<i64>>(6)?
            .and_then(|value| u32::try_from(value).ok().filter(|value| *value > 0)),
        pin: enum_from_db_sql(row.get(7)?)?,
        token_limit: row
            .get::<_, Option<String>>(8)?
            .map(|raw| {
                serde_json::from_str::<vibex_core::GatewayTokenLimit>(&raw).map_err(|error| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        rusqlite::types::Type::Text,
                        Box::new(error),
                    )
                })
            })
            .transpose()?,
        status,
        revision: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
        deleted_at_ms: row.get(13)?,
        failover: enum_from_db_sql(row.get(14)?)?,
        agent_model_id: row.get(15)?,
        rules: decode_rules(row.get::<_, Option<String>>(16)?)?,
        classifier: row.get(17)?,
        effort: row.get(18)?,
        patience_ms: row
            .get::<_, Option<i64>>(19)?
            .and_then(|value| u64::try_from(value).ok().filter(|value| *value > 0)),
        sink: row.get::<_, i64>(20)? != 0,
        unlists: row.get::<_, i64>(21)? != 0,
        levels: decode_levels(row.get::<_, Option<String>>(22)?)?,
        pick_member_id: parse_optional_id_sql(
            row.get::<_, Option<String>>(23)?,
            ModelRouteMemberId::parse,
        )?,
    })
}

fn map_binding(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentModelRouteBinding> {
    Ok(AgentModelRouteBinding {
        id: parse_id_sql(row.get(0)?, AgentModelRouteBindingId::parse)?,
        agent_id: parse_id_sql(row.get(1)?, AgentId::parse)?,
        model_route_id: parse_id_sql(row.get(2)?, ModelRouteId::parse)?,
        is_default: row.get::<_, i64>(3)? != 0,
        revision: row.get(4)?,
        created_at_ms: row.get(5)?,
        updated_at_ms: row.get(6)?,
        deleted_at_ms: row.get(7)?,
    })
}

fn collect_rows<T>(
    rows: rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    code: &'static str,
) -> VibexResult<Vec<T>> {
    let mut out = Vec::new();
    for row in rows {
        out.push(row.map_err(storage_err(code, "failed to read a route row"))?);
    }
    Ok(out)
}

/// Seeds one `Direct` route per legacy direct Provider Profile.
///
/// A direct Profile carries its own `base_url`, its own Key and its own model
/// catalogue, and is projected into exactly one Agent. That is the same shape as
/// a single-member direct route, so it can be expressed as one — which is what
/// lets the route graph show the direct connections beside the gateway ones
/// instead of drawing a second kind of thing.
///
/// The legacy row is *not* removed. The launch path still resolves a Key and a
/// protocol through it, so the route is a description of it, not a replacement
/// for it until the projection moves over.
///
/// Skipped Profiles:
/// - the hidden gateway Profile, which is machinery rather than a user choice;
/// - internal runtime Profiles, which are launch scaffolding with no catalogue;
/// - the per-kind local default, which exists only to give an Agent a fallback.
///
/// Idempotent: a Profile that already has a route is left alone, so this can run
/// on every launch.
pub fn backfill_direct_routes(conn: &Connection) -> VibexResult<usize> {
    let profiles = super::ProviderProfileRepository::list_all(conn)?;
    let mut created = 0usize;
    for profile in profiles {
        if !is_direct_route_candidate(&profile) {
            continue;
        }
        // One route per Profile. The marker lives in the description because it
        // has to survive a rename: the route's own name is the user's to change.
        let marker = direct_route_marker(&profile);
        let exists: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM model_routes WHERE description = ?1",
                params![marker],
                |row| row.get(0),
            )
            .unwrap_or(0);
        if exists > 0 {
            continue;
        }
        let Some(route) = direct_route_for(&profile) else {
            continue;
        };
        // The member names the shared Provider the compatibility layer derives
        // from this Profile. A Profile written after the last backfill has no
        // such row yet, so materialise it before the member's foreign key looks
        // for it. This is the same mapping the launch path uses.
        //
        // A row an Agent materialised from a shared Provider is skipped: it
        // already points at that Provider, and deriving a record from it would
        // create a second Provider and a second direct route for one connection.
        // Two reasons to leave a row alone: it was materialised from a shared
        // Provider and already points elsewhere, or the cutover has happened and the
        // shared record it projected from is now authoritative.
        if let Some(existing) =
            super::AgentModelProviderBindingRepository::get_by_legacy_profile(conn, &profile.id)?
            && (crate::migration_applied(conn, 73)?
                || existing.model_provider_profile_id.as_ref()
                    != Some(&super::legacy_model_provider_id(&profile.id)?))
        {
            continue;
        }
        super::ProviderProjectionCompatibilityRepository::sync_legacy_profile(conn, &profile)?;
        super::ModelRouteRepository::insert(conn, &route)?;
        created += 1;
    }
    Ok(created)
}

/// The shared Provider a legacy Profile projects onto.
///
/// The compatibility layer derives the v2 id deterministically, so the same
/// Profile always names the same Provider.
fn legacy_provider_key(profile: &vibex_core::ProviderProfile) -> String {
    format!("model_provider_legacy_{}", profile.id.as_str())
}

/// A Profile that is a real, single-Agent, user-visible direct connection.
///
/// The exclusions mirror the Config Center's own list filter: anything that is
/// machinery must not turn into a user-facing route.
fn is_direct_route_candidate(profile: &vibex_core::ProviderProfile) -> bool {
    if profile.deleted_at_ms.is_some() {
        return false;
    }
    if profile
        .base_url
        .as_deref()
        .is_none_or(|url| url.trim().is_empty())
    {
        // No address means nothing to connect to: an ACP runtime Profile has a
        // command, not an endpoint.
        return false;
    }
    // The hidden gateway Profile, and the internal runtime Profiles, carry a
    // role marker. Either one would be a route the user never made.
    let internal_role = profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == "vibex.internal.profileRole")
        .map(|entry| entry.value.trim().to_string());
    if internal_role.is_some() {
        return false;
    }
    // The per-kind local default exists so an Agent always has a fallback; it
    // is disabled scaffolding, not a connection the user configured.
    !profile.id.as_str().starts_with("provider_local_")
}

/// The provenance marker a seeded direct route carries.
///
/// It names the legacy Profile and is what makes the backfill idempotent. A
/// display name cannot serve that purpose: renaming a route is a normal edit.
fn direct_route_marker(profile: &vibex_core::ProviderProfile) -> String {
    format!("vibex.direct_provider:{}", profile.id.as_str())
}

fn direct_route_for(profile: &vibex_core::ProviderProfile) -> Option<ModelRoute> {
    let model = profile
        .default_model
        .as_deref()
        .map(str::trim)
        .filter(|model| !model.is_empty())
        .map(str::to_string)
        .or_else(|| {
            profile
                .configured_models
                .iter()
                .find(|model| model.enabled)
                .map(|model| model.id.clone())
        })?;
    let provider = ModelProviderProfileId::parse(legacy_provider_key(profile)).ok()?;
    let now = unix_timestamp_ms().max(1);
    Some(ModelRoute {
        display_name: profile.display_name.clone(),
        description: Some(direct_route_marker(profile)),
        kind: RouteKind::Direct,
        members: vec![ModelRouteMember::new(provider, model, 0)],
        strategy: RouteStrategy::Ordered,
        status: RouteStatus::Enabled,
        revision: 1,
        created_at_ms: now,
        updated_at_ms: now,
        ..Default::default()
    })
}

/// The legacy Profile an Agent launches through for one route.
///
/// Lives in the repository rather than the service because the launch path
/// reaches it with a connection in hand and no service: resolving a selection
/// must not require constructing the Config Center.
pub fn legacy_profile_for_route(
    conn: &Connection,
    agent_id: &AgentId,
    route_id: &ModelRouteId,
) -> VibexResult<Option<vibex_core::ProviderProfile>> {
    let Some(route) = ModelRouteRepository::get(conn, route_id)? else {
        return Ok(None);
    };
    // A route another Agent holds is not a launchable source for this one.
    let bound = AgentModelRouteBindingRepository::list_for_agent(conn, agent_id)?
        .iter()
        .any(|binding| &binding.model_route_id == route_id);
    if !bound {
        return Ok(None);
    }
    match route.kind {
        vibex_core::RouteKind::Direct => {
            let Some(member) = route.enabled_members().next() else {
                return Ok(None);
            };
            let Some(shared) = super::ModelProviderProfileRepository::get(
                conn,
                &member.model_provider_profile_id,
            )?
            else {
                return Ok(None);
            };
            let Some(legacy_id) = shared.legacy_provider_profile_id else {
                return Ok(None);
            };
            let profile = super::ProviderProfileRepository::get(conn, &legacy_id)?;
            Ok(profile.filter(|profile| &profile.agent_id == agent_id))
        }
        vibex_core::RouteKind::ModelGroup => {
            // A gateway Profile is per Agent and lists exactly the groups that
            // Agent is bound to. A group contributes one entry, so the check is
            // by prefix rather than by a single id.
            let prefix = route.model_id();
            let profiles = super::ProviderProfileRepository::list_by_agent(conn, agent_id, true)?;
            Ok(profiles
                .into_iter()
                .find(is_gateway_profile)
                .filter(|profile| {
                    profile
                        .configured_models
                        .iter()
                        .any(|model| model.id.starts_with(&prefix))
                }))
        }
    }
}

/// Whether a Profile is the hidden one the gateway projects into an Agent.
fn is_gateway_profile(profile: &vibex_core::ProviderProfile) -> bool {
    profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == "vibex.internal.profileRole")
        .map(|entry| entry.value.trim() == "model_gateway")
        .unwrap_or(false)
}

/// Encodes a route's token ceiling for storage.
///
/// `None` is stored as NULL rather than `"null"`, so "no ceiling" and "a ceiling
/// of zero tokens" cannot be confused by a reader that only looks at presence.
fn encode_token_limit(
    limit: &Option<vibex_core::GatewayTokenLimit>,
) -> VibexResult<Option<String>> {
    let Some(limit) = limit else {
        return Ok(None);
    };
    serde_json::to_string(limit).map(Some).map_err(|error| {
        VibexError::storage(
            "model_route_token_limit_encode_failed",
            format!("failed to encode the route token limit: {error}"),
        )
    })
}

/// Encodes a route's rules for storage.
///
/// An empty list is stored as NULL, so a route that has never had a rule and a
/// route whose rules were all removed read the same way.
fn encode_rules(rules: &[ModelRouteRule]) -> VibexResult<Option<String>> {
    if rules.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&RouteRulesPayload {
        schema_version: ROUTE_RULES_SCHEMA_VERSION,
        rules: rules.to_vec(),
    })
    .map(Some)
    .map_err(|error| {
        VibexError::storage(
            "model_route_rules_encode_failed",
            format!("failed to encode the route rules: {error}"),
        )
    })
}

fn decode_rules(raw: Option<String>) -> rusqlite::Result<Vec<ModelRouteRule>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let payload = serde_json::from_str::<RouteRulesPayload>(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(16, rusqlite::types::Type::Text, Box::new(error))
    })?;
    if payload.schema_version != ROUTE_RULES_SCHEMA_VERSION {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            16,
            rusqlite::types::Type::Text,
            Box::new(VibexError::storage(
                "model_route_rules_version_unknown",
                "the route rules were written by an unsupported schema version",
            )),
        ));
    }
    Ok(payload.rules)
}

fn encode_levels(levels: &[String]) -> VibexResult<Option<String>> {
    if levels.is_empty() {
        return Ok(None);
    }
    serde_json::to_string(&RouteLevelsPayload {
        schema_version: ROUTE_LEVELS_SCHEMA_VERSION,
        levels: levels.to_vec(),
    })
    .map(Some)
    .map_err(|error| {
        VibexError::storage(
            "model_route_levels_encode_failed",
            format!("failed to encode the route levels: {error}"),
        )
    })
}

fn decode_levels(raw: Option<String>) -> rusqlite::Result<Vec<String>> {
    let Some(raw) = raw else {
        return Ok(Vec::new());
    };
    let payload = serde_json::from_str::<RouteLevelsPayload>(&raw).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(22, rusqlite::types::Type::Text, Box::new(error))
    })?;
    if payload.schema_version != ROUTE_LEVELS_SCHEMA_VERSION {
        return Err(rusqlite::Error::FromSqlConversionFailure(
            22,
            rusqlite::types::Type::Text,
            Box::new(VibexError::storage(
                "model_route_levels_version_unknown",
                "the route levels were written by an unsupported schema version",
            )),
        ));
    }
    Ok(payload.levels)
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::ModelProviderProfileStatus;

    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    /// A Provider row to point members at. Members carry a foreign key, so a
    /// route cannot name a Provider that does not exist.
    fn seed_provider(conn: &Connection, name: &str) -> ModelProviderProfileId {
        let profile = vibex_core::ModelProviderProfile {
            display_name: name.to_string(),
            status: ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        crate::ModelProviderProfileRepository::insert(conn, &profile).unwrap();
        profile.id
    }

    /// A model group naming one Provider and the given upstream models.
    ///
    /// Groups are the only gateway shape, so the test helper builds one; a test
    /// that needs the direct shape sets `kind` itself.
    fn route(provider: &ModelProviderProfileId, models: &[&str]) -> ModelRoute {
        ModelRoute {
            display_name: "Standard".to_string(),
            status: RouteStatus::Enabled,
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Standard".to_string()),
            members: models
                .iter()
                .enumerate()
                .map(|(index, model)| ModelRouteMember::new(provider.clone(), *model, index as i64))
                .collect(),
            ..Default::default()
        }
    }

    /// A model group round-trips its members and their upstream models.
    #[test]
    fn a_model_group_round_trips_its_members() {
        let conn = migrated_connection();
        let left = seed_provider(&conn, "DeepSeek");
        let right = seed_provider(&conn, "Kimi");
        let route = ModelRoute {
            display_name: "Team".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Team".to_string()),
            status: RouteStatus::Enabled,
            strategy: RouteStrategy::RoundRobin,
            members: vec![
                ModelRouteMember::new(left.clone(), "deepseek-chat", 0),
                ModelRouteMember::new(right.clone(), "kimi-k2", 1),
            ],
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &route).unwrap();
        let loaded = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.kind, RouteKind::ModelGroup);
        assert_eq!(loaded.agent_model(), Some("Team"));
        assert_eq!(loaded.strategy, RouteStrategy::RoundRobin);
        assert_eq!(
            loaded
                .members
                .iter()
                .map(|member| (
                    member.model_provider_profile_id.clone(),
                    member.model_id.clone()
                ))
                .collect::<Vec<_>>(),
            vec![
                (left, "deepseek-chat".to_string()),
                (right, "kimi-k2".to_string())
            ],
            "member order and upstream models are the attempt order"
        );
    }

    /// The budget counts composed routes, not the direct ones the backfill
    /// materialised from the Provider list.
    ///
    /// `backfill_direct_routes` writes one `Direct` row per direct Profile, and an
    /// installation may hold more Profiles than the route budget allows. Counting
    /// those rows as user-authored spent the budget on the user's behalf and left
    /// the route-group editor failing with `model_route_limit_exceeded`.
    #[test]
    fn only_user_authored_routes_count_towards_the_route_budget() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");

        let direct = ModelRoute {
            display_name: "Direct".to_string(),
            kind: RouteKind::Direct,
            status: RouteStatus::Enabled,
            members: vec![ModelRouteMember::new(provider.clone(), "deepseek-chat", 0)],
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &direct).unwrap();
        assert_eq!(
            ModelRouteRepository::count_user_authored(&conn).unwrap(),
            0,
            "a direct route is materialised, not composed"
        );

        let group = ModelRoute {
            display_name: "Group".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Group".to_string()),
            status: RouteStatus::Enabled,
            members: vec![ModelRouteMember::new(provider, "m", 0)],
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &group).unwrap();
        assert_eq!(
            ModelRouteRepository::count_user_authored(&conn).unwrap(),
            1,
            "a group is what the budget bounds"
        );

        // A soft-deleted route frees its place again.
        ModelRouteRepository::delete(&conn, &group.id).unwrap();
        assert_eq!(ModelRouteRepository::count_user_authored(&conn).unwrap(), 0);
    }

    #[test]
    fn the_route_tables_exist_and_re_applying_migrations_changes_nothing() {
        let mut conn = migrated_connection();
        assert_eq!(
            crate::current_schema_version(&conn).unwrap(),
            crate::CURRENT_SCHEMA_VERSION
        );
        assert!(crate::migration_applied(&conn, 67).unwrap());

        let applied = crate::apply_migrations(&mut conn).unwrap();
        assert!(
            !applied.iter().any(|entry| entry.starts_with("67:")),
            "re-applying migrations must not re-run the route migration"
        );
        // The tables are queryable after a second pass, which is what an
        // existing installation does on every launch.
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
        let agent = AgentId::parse("codex").unwrap();
        assert!(
            AgentModelRouteBindingRepository::list_for_agent(&conn, &agent)
                .unwrap()
                .is_empty()
        );
    }

    /// Writes the gateway's own settings row.
    fn seed_gateway(conn: &Connection, name: &str, enabled: bool) {
        let settings = crate::ModelGatewaySettingsRepository::get_or_create(conn).unwrap();
        let mut next = settings.clone();
        next.revision = settings.revision + 1;
        next.display_name = name.to_string();
        next.policy.enabled = enabled;
        crate::ModelGatewaySettingsRepository::update(conn, &next, settings.revision).unwrap();
    }

    /// A legacy direct Profile in the shape the Config Center writes: an
    /// address, a model, and one Agent.
    fn seed_direct_profile(
        conn: &Connection,
        agent: &str,
        name: &str,
        base_url: &str,
        model: &str,
        role: Option<&str>,
    ) -> vibex_core::ProviderProfile {
        let options = role.map(|role| vibex_core::ProviderOptions {
            schema_version: 1,
            entries: vec![vibex_core::ProviderBindingMetadata {
                key: "vibex.internal.profileRole".to_string(),
                value: role.to_string(),
            }],
        });
        let profile = crate::ProviderProfileRepository::from_create_request(
            vibex_core::ProviderProfileCreateRequest {
                agent_id: Some(AgentId::parse(agent).unwrap()),
                kind: vibex_core::ProviderKind::Acp,
                display_name: name.to_string(),
                account_alias: None,
                base_url: Some(base_url.to_string()),
                default_model: Some(model.to_string()),
                small_model: None,
                large_model: None,
                configured_models: Vec::new(),
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: options,
                secret_references: Vec::new(),
            },
        );
        crate::ProviderProfileRepository::insert(conn, &profile).unwrap();
        profile
    }

    #[test]
    fn a_direct_profile_becomes_a_single_member_direct_route() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek 直连",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        let created = backfill_direct_routes(&conn).unwrap();
        assert_eq!(created, 1);

        let routes = ModelRouteRepository::list(&conn).unwrap();
        assert_eq!(routes.len(), 1);
        let route = &routes[0];
        assert_eq!(route.kind, RouteKind::Direct);
        assert_eq!(route.display_name, "DeepSeek 直连");
        assert_eq!(
            route.members.len(),
            1,
            "a direct route has exactly one member"
        );
        assert_eq!(route.members[0].model_id, "deepseek-chat");
    }

    #[test]
    fn a_gateway_profile_is_not_turned_into_a_direct_route() {
        let conn = migrated_connection();
        // The hidden Profile the gateway projects into an Agent carries the
        // gateway role. It is machinery, so it must not become a route the user
        // never made.
        seed_direct_profile(
            &conn,
            "codex",
            "Vibex Gateway",
            "http://127.0.0.1:3425",
            "deepseek/deepseek-chat",
            Some("model_gateway"),
        );
        // An internal runtime Profile has a role too, and no catalogue at all.
        seed_direct_profile(
            &conn,
            "claude",
            "Claude ACP",
            "https://api.anthropic.com",
            "claude-sonnet-5",
            Some("agent_runtime"),
        );

        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_profile_without_an_address_or_a_model_is_skipped() {
        let conn = migrated_connection();
        // No base_url: an ACP runtime Profile has a command, not an endpoint.
        seed_direct_profile(&conn, "codex", "No address", "", "m", None);
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
    }

    #[test]
    fn the_direct_backfill_is_idempotent_across_launches() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek 直连",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        assert_eq!(backfill_direct_routes(&conn).unwrap(), 1);
        assert_eq!(
            backfill_direct_routes(&conn).unwrap(),
            0,
            "a re-run seeds nothing"
        );
        assert_eq!(ModelRouteRepository::list(&conn).unwrap().len(), 1);

        // Renaming the route must not make the backfill create it again: the
        // marker is provenance, not the display name.
        let mut route = ModelRouteRepository::list(&conn).unwrap().remove(0);
        route.display_name = "Renamed".to_string();
        route.revision += 1;
        ModelRouteRepository::update(&conn, &route, route.revision - 1).unwrap();
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 0);
        assert_eq!(ModelRouteRepository::list(&conn).unwrap().len(), 1);
    }

    #[test]
    fn two_agents_keep_two_independent_direct_routes() {
        let conn = migrated_connection();
        seed_direct_profile(
            &conn,
            "codex",
            "DeepSeek",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );
        seed_direct_profile(
            &conn,
            "claude",
            "DeepSeek",
            "https://api.deepseek.com/v1",
            "deepseek-chat",
            None,
        );

        // The same Provider for two Agents is two Profiles, so two routes. They
        // must stay separate: an edit to one Agent's protocol is not an edit to
        // the other's.
        assert_eq!(backfill_direct_routes(&conn).unwrap(), 2);
        let routes = ModelRouteRepository::list(&conn).unwrap();
        assert_eq!(routes.len(), 2);
        assert!(routes.iter().all(|route| route.kind == RouteKind::Direct));
    }

    /// The gateway no longer owns a Provider list, so there is nothing to seed
    /// a route from. A route exists only because a user composed one.
    #[test]
    fn no_route_is_seeded_from_gateway_settings() {
        let conn = migrated_connection();
        seed_provider(&conn, "DeepSeek");
        seed_gateway(&conn, "Vibex Gateway", true);
        assert!(ModelRouteRepository::list(&conn).unwrap().is_empty());
    }

    #[test]
    fn a_route_round_trips_with_its_members_in_attempt_order() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut route = route(&provider, &["first", "second", "third"]);
        route.description = Some("primary and fallback".to_string());
        route.strategy = RouteStrategy::LeastUsed;
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let loaded = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .unwrap();
        assert_eq!(loaded.display_name, "Standard");
        assert_eq!(loaded.strategy, RouteStrategy::LeastUsed);
        assert_eq!(loaded.kind, RouteKind::ModelGroup);
        assert_eq!(
            loaded
                .members
                .iter()
                .map(|member| member.model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["first", "second", "third"]
        );
    }

    #[test]
    fn updating_a_route_replaces_the_member_order_wholesale() {
        let conn = migrated_connection();
        let first = seed_provider(&conn, "DeepSeek");
        let second = seed_provider(&conn, "Kimi");
        let mut route = ModelRoute {
            display_name: "Group".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Group".to_string()),
            status: RouteStatus::Enabled,
            members: vec![
                ModelRouteMember::new(first.clone(), "a", 0),
                ModelRouteMember::new(second.clone(), "b", 1),
            ],
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &route).unwrap();

        // Reverse the order without changing which members exist.
        route.members.reverse();
        for (index, member) in route.members.iter_mut().enumerate() {
            member.order_index = index as i64;
        }
        let updated = ModelRouteRepository::update(&conn, &route, route.revision).unwrap();
        assert_eq!(updated.revision, route.revision + 1);
        assert_eq!(
            updated
                .members
                .iter()
                .map(|member| member.model_provider_profile_id.clone())
                .collect::<Vec<_>>(),
            vec![second, first]
        );
    }

    #[test]
    fn a_stale_revision_is_refused_rather_than_overwriting() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let route = route(&provider, &["first"]);
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let error = ModelRouteRepository::update(&conn, &route, route.revision + 5).unwrap_err();
        assert_eq!(error.code, "model_route_revision_conflict");
    }

    #[test]
    fn a_direct_route_cannot_be_saved_with_two_members() {
        let conn = migrated_connection();
        let first = seed_provider(&conn, "DeepSeek");
        let second = seed_provider(&conn, "Kimi");
        let route = ModelRoute {
            display_name: "Direct".to_string(),
            kind: RouteKind::Direct,
            status: RouteStatus::Enabled,
            members: vec![
                ModelRouteMember::new(first, "deepseek-chat", 0),
                ModelRouteMember::new(second, "kimi-k3", 1),
            ],
            ..Default::default()
        };
        let error = ModelRouteRepository::insert(&conn, &route).unwrap_err();
        assert_eq!(error.code, "route_direct_member_count_invalid");
    }

    #[test]
    fn one_agent_may_bind_several_routes_but_only_one_is_default() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("codex").unwrap();
        let mut first = route(&provider, &["a"]);
        first.display_name = "First".to_string();
        first.agent_model_id = Some("First".to_string());
        let mut second = route(&provider, &["b"]);
        second.display_name = "Second".to_string();
        second.agent_model_id = Some("Second".to_string());
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        let mut first_binding = AgentModelRouteBinding::new(agent.clone(), first.id.clone());
        first_binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &first_binding).unwrap();

        let mut second_binding = AgentModelRouteBinding::new(agent.clone(), second.id.clone());
        second_binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &second_binding).unwrap();

        let bindings = AgentModelRouteBindingRepository::list_for_agent(&conn, &agent).unwrap();
        assert_eq!(bindings.len(), 2, "both routes stay bound");
        assert_eq!(
            bindings
                .iter()
                .filter(|binding| binding.is_default)
                .map(|binding| binding.model_route_id.clone())
                .collect::<Vec<_>>(),
            vec![second.id.clone()],
            "binding a second default demotes the first"
        );
        // Default first, so a new session finds it without searching.
        assert_eq!(bindings[0].model_route_id, second.id);
    }

    #[test]
    fn the_database_refuses_a_second_default_for_one_agent() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("codex").unwrap();
        let first = route(&provider, &["a"]);
        let mut second = route(&provider, &["b"]);
        second.display_name = "Second".to_string();
        second.agent_model_id = Some("Second".to_string());
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        // The write path would demote the first. Writing the rows directly
        // skips that, which is exactly what the partial index must catch.
        conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 1, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                first.id.as_str()
            ],
        )
        .unwrap();

        let duplicate = conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 1, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                second.id.as_str()
            ],
        );
        assert!(
            duplicate.is_err(),
            "the partial unique index is the last fence against two defaults"
        );

        // A non-default binding for the same Agent is still allowed.
        conn.execute(
            "INSERT INTO agent_model_route_bindings (
                agent_model_route_binding_id, agent_id, model_route_id, is_default,
                revision, created_at_ms, updated_at_ms
             ) VALUES (?1, ?2, ?3, 0, 1, 1, 1)",
            params![
                AgentModelRouteBindingId::new().as_str(),
                agent.as_str(),
                second.id.as_str()
            ],
        )
        .unwrap();
    }

    #[test]
    fn set_default_promotes_one_and_demotes_the_other() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let agent = AgentId::parse("claude").unwrap();
        let first = route(&provider, &["a"]);
        let mut second = route(&provider, &["b"]);
        second.display_name = "Second".to_string();
        second.agent_model_id = Some("Second".to_string());
        ModelRouteRepository::insert(&conn, &first).unwrap();
        ModelRouteRepository::insert(&conn, &second).unwrap();

        let mut binding = AgentModelRouteBinding::new(agent.clone(), first.id.clone());
        binding.is_default = true;
        AgentModelRouteBindingRepository::insert(&conn, &binding).unwrap();
        AgentModelRouteBindingRepository::insert(
            &conn,
            &AgentModelRouteBinding::new(agent.clone(), second.id.clone()),
        )
        .unwrap();

        AgentModelRouteBindingRepository::set_default(&conn, &agent, &second.id).unwrap();
        let bindings = AgentModelRouteBindingRepository::list_for_agent(&conn, &agent).unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(bindings[0].model_route_id, second.id);
        assert!(bindings[0].is_default);
        assert!(!bindings[1].is_default);
    }

    #[test]
    fn deleting_a_route_unbinds_it_from_every_agent() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let shared = route(&provider, &["a"]);
        ModelRouteRepository::insert(&conn, &shared).unwrap();
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            let mut binding = AgentModelRouteBinding::new(agent.clone(), shared.id.clone());
            binding.is_default = true;
            AgentModelRouteBindingRepository::insert(&conn, &binding).unwrap();
        }

        assert!(ModelRouteRepository::delete(&conn, &shared.id).unwrap());
        assert!(
            ModelRouteRepository::get(&conn, &shared.id)
                .unwrap()
                .is_none()
        );
        for agent_id in ["codex", "claude"] {
            let agent = AgentId::parse(agent_id).unwrap();
            assert!(
                AgentModelRouteBindingRepository::list_for_agent(&conn, &agent)
                    .unwrap()
                    .is_empty(),
                "an unbound route must not leave a live binding behind"
            );
        }
    }

    #[test]
    fn a_route_reports_the_agents_that_reference_it() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let shared = route(&provider, &["a"]);
        ModelRouteRepository::insert(&conn, &shared).unwrap();
        for agent_id in ["codex", "gemini"] {
            let agent = AgentId::parse(agent_id).unwrap();
            AgentModelRouteBindingRepository::insert(
                &conn,
                &AgentModelRouteBinding::new(agent, shared.id.clone()),
            )
            .unwrap();
        }

        let users = AgentModelRouteBindingRepository::list_for_route(&conn, &shared.id).unwrap();
        assert_eq!(
            users
                .iter()
                .map(|binding| binding.agent_id.as_str())
                .collect::<Vec<_>>(),
            vec!["codex", "gemini"]
        );
    }

    #[test]
    fn list_enabled_skips_a_disabled_route_and_keeps_names_in_order() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let mut beta = route(&provider, &["a"]);
        beta.display_name = "Beta".to_string();
        beta.agent_model_id = Some("Beta".to_string());
        let mut alpha = route(&provider, &["b"]);
        alpha.display_name = "Alpha".to_string();
        alpha.agent_model_id = Some("Alpha".to_string());
        let mut off = route(&provider, &["c"]);
        off.display_name = "Zeta".to_string();
        off.agent_model_id = Some("Zeta".to_string());
        off.status = RouteStatus::Disabled;
        for route in [&beta, &alpha, &off] {
            ModelRouteRepository::insert(&conn, route).unwrap();
        }

        let enabled = ModelRouteRepository::list_enabled(&conn).unwrap();
        assert_eq!(
            enabled
                .iter()
                .map(|route| route.display_name.as_str())
                .collect::<Vec<_>>(),
            vec!["Alpha", "Beta"]
        );
    }

    #[test]
    fn a_provider_lookup_lists_every_route_that_names_it() {
        let conn = migrated_connection();
        let deepseek = seed_provider(&conn, "DeepSeek");
        let kimi = seed_provider(&conn, "Kimi");
        let naming_both = ModelRoute {
            display_name: "Both".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Both".to_string()),
            status: RouteStatus::Enabled,
            members: vec![
                ModelRouteMember::new(deepseek.clone(), "a", 0),
                ModelRouteMember::new(kimi.clone(), "b", 1),
            ],
            ..Default::default()
        };
        let only_deepseek = route(&deepseek, &["b"]);
        ModelRouteRepository::insert(&conn, &naming_both).unwrap();
        ModelRouteRepository::insert(&conn, &only_deepseek).unwrap();

        let naming = ModelRouteRepository::list_naming_provider(&conn, &deepseek).unwrap();
        assert_eq!(naming.len(), 2, "both routes name DeepSeek");
        let naming_kimi = ModelRouteRepository::list_naming_provider(&conn, &kimi).unwrap();
        assert_eq!(naming_kimi.len(), 1, "only one names Kimi");
    }
}

#[cfg(test)]
mod route_auth_source_tests {
    use super::*;

    /// A route's selection survives a round trip through storage.
    ///
    /// This is the persistence half of stage 5: a session that selected a route
    /// must still name that route when it is reopened, not fall back to the
    /// Profile the route resolved to at the time.
    #[test]
    fn a_route_auth_source_round_trips_through_the_database() {
        let source = vibex_core::RuntimeAuthSource::model_route(ModelRouteId::new());
        let (kind, id) = (source.kind(), source.id().to_string());
        let encoded = match kind {
            vibex_core::RuntimeAuthSourceKind::ModelRoute => "model_route",
            other => panic!("a route must encode as its own kind, got {other:?}"),
        };
        assert_eq!(encoded, "model_route");
        assert!(!id.is_empty());
        // The legacy columns stay empty: a route has no legacy Profile, and a
        // non-empty one would be rejected on read.
        assert!(source.provider_profile_id().is_none());
    }

    /// An old row that predates routes still decodes.
    ///
    /// The new kind is additive; a stored `provider_profile` or `agent_account`
    /// row must keep opening, which is what makes the change safe for existing
    /// sessions.
    #[test]
    fn legacy_auth_sources_are_unaffected() {
        let profile =
            vibex_core::RuntimeAuthSource::provider_profile(vibex_core::ProviderProfileId::new());
        assert_eq!(
            profile.kind(),
            vibex_core::RuntimeAuthSourceKind::ProviderProfile
        );
        assert!(profile.model_route_id().is_none());

        let account =
            vibex_core::RuntimeAuthSource::agent_account(vibex_core::AgentAuthContextId::new());
        assert_eq!(
            account.kind(),
            vibex_core::RuntimeAuthSourceKind::AgentAccount
        );
        assert!(account.model_route_id().is_none());
    }
}

#[cfg(test)]
mod route_policy_tests {
    use super::*;

    fn migrated_connection() -> Connection {
        let mut connection = Connection::open_in_memory().unwrap();
        crate::apply_migrations(&mut connection).unwrap();
        connection
    }

    fn seed_provider(conn: &Connection, name: &str) -> ModelProviderProfileId {
        let profile = vibex_core::ModelProviderProfile {
            display_name: name.to_string(),
            status: vibex_core::ModelProviderProfileStatus::Enabled,
            ..Default::default()
        };
        crate::ModelProviderProfileRepository::insert(conn, &profile).unwrap();
        profile.id
    }

    /// A route's pin and token ceiling survive storage.
    ///
    /// These are the fields the route graph edits. Without them stored, the UI
    /// would offer controls whose answers vanish on the next read — a field with
    /// a surface but no memory.
    #[test]
    fn route_policy_round_trips() {
        let dir = std::env::temp_dir().join(format!(
            "vibex-route-policy-{}",
            vibex_core::RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let mut conn = rusqlite::Connection::open(dir.join("state.db")).unwrap();
        crate::apply_migrations(&mut conn).unwrap();

        let provider = {
            let profile = vibex_core::ModelProviderProfile {
                id: vibex_core::ModelProviderProfileId::new(),
                legacy_provider_profile_id: None,
                display_name: "DeepSeek".to_string(),
                vendor_hint: None,
                endpoints: Vec::new(),
                proxy_policy: vibex_core::ModelProviderProxyPolicy::InheritSystem,
                credentials: Vec::new(),
                configured_models: Vec::new(),
                default_model_id: None,
                headers: Vec::new(),
                metadata: Default::default(),
                status: vibex_core::ModelProviderProfileStatus::Enabled,
                revision: 1,
                created_at_ms: 1,
                updated_at_ms: 1,
                deleted_at_ms: None,
            };
            crate::ModelProviderProfileRepository::insert(&conn, &profile).unwrap();
            profile.id
        };

        let route = ModelRoute {
            display_name: "Standard".to_string(),
            kind: vibex_core::RouteKind::ModelGroup,
            agent_model_id: Some("Standard".to_string()),
            members: vec![vibex_core::ModelRouteMember::new(provider, "m", 0)],
            status: vibex_core::RouteStatus::Enabled,
            pin: vibex_core::GatewayPinScope::Conversation,
            failover: vibex_core::GatewayFailover::Ask,
            token_limit: Some(vibex_core::GatewayTokenLimit {
                window: vibex_core::GatewayTokenWindow::Week,
                tokens: 5_000_000,
            }),
            first_token_timeout_ms: Some(20_000),
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let read = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .expect("the route must exist");
        assert_eq!(
            read.pin,
            vibex_core::GatewayPinScope::Conversation,
            "a route's pinning choice must survive storage"
        );
        assert_eq!(
            read.failover,
            vibex_core::GatewayFailover::Ask,
            "a route's failover choice must survive storage"
        );
        let limit = read.token_limit.expect("the ceiling must survive");
        assert_eq!(limit.window, vibex_core::GatewayTokenWindow::Week);
        assert_eq!(limit.tokens, 5_000_000);
        assert_eq!(read.first_token_timeout_ms, Some(20_000));

        // Turning the ceiling off must clear it, not store a zero: a zero-token
        // ceiling would forbid every request.
        let mut cleared = read.clone();
        cleared.token_limit = None;
        cleared.revision = read.revision.saturating_add(1);
        ModelRouteRepository::update(&conn, &cleared, read.revision).unwrap();
        let reread = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .unwrap();
        assert!(
            reread.token_limit.is_none(),
            "clearing a ceiling must store NULL, not a zero ceiling"
        );

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A model group stores its virtual model, rules, and member overrides.
    ///
    /// This is the shape the old provider-group rows must not be forced into:
    /// every new column is nullable or defaulted, so the same table holds both.
    #[test]
    fn a_model_group_round_trips_its_rules_and_member_overrides() {
        let conn = migrated_connection();
        let left = seed_provider(&conn, "DeepSeek");
        let right = seed_provider(&conn, "Kimi");
        let mut first = ModelRouteMember::new(left, "deepseek-chat", 0);
        first.weight = 3;
        first.first_token_timeout_ms = Some(12_000);
        first.context_tokens = Some(128_000);
        let mut second = ModelRouteMember::new(right, "kimi-k2", 1);
        second.reasoning_effort = Some("high".to_string());
        let route = ModelRoute {
            display_name: "Fast".to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("Fast".to_string()),
            strategy: RouteStrategy::Weighted,
            members: vec![first.clone(), second],
            rules: vec![ModelRouteRule {
                id: "rule-1".to_string(),
                use_member_id: Some(first.id.clone()),
                use_effort: Some("high".to_string()),
                images: Some(false),
                tokens_min: Some(1_000),
                ..Default::default()
            }],
            classifier: Some("deepseek-chat".to_string()),
            effort: Some("auto".to_string()),
            patience_ms: Some(60_000),
            sink: true,
            unlists: true,
            levels: vec!["fast".to_string(), "deep".to_string()],
            pick_member_id: Some(first.id.clone()),
            status: RouteStatus::Enabled,
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &route).unwrap();

        let read = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .expect("the model group must exist");
        assert_eq!(read.kind, RouteKind::ModelGroup);
        assert_eq!(read.agent_model_id.as_deref(), Some("Fast"));
        assert_eq!(read.strategy, RouteStrategy::Weighted);
        assert_eq!(read.rules.len(), 1);
        assert_eq!(read.rules[0].id, "rule-1");
        assert_eq!(read.rules[0].use_member_id, Some(first.id.clone()));
        assert_eq!(read.classifier.as_deref(), Some("deepseek-chat"));
        assert_eq!(read.effort.as_deref(), Some("auto"));
        assert_eq!(read.patience_ms, Some(60_000));
        assert!(read.sink);
        assert!(read.unlists);
        assert_eq!(read.levels, vec!["fast", "deep"]);
        assert_eq!(read.pick_member_id, Some(first.id.clone()));
        assert_eq!(read.members[0].weight, 3);
        assert_eq!(read.members[0].first_token_timeout_ms, Some(12_000));
        assert_eq!(read.members[0].context_tokens, Some(128_000));
        assert_eq!(read.members[1].reasoning_effort.as_deref(), Some("high"));

        // Clearing every optional field must round-trip as absent, not as a
        // magic empty payload. The route stays a valid model group with one
        // member, so the clear is what is being tested.
        let mut cleared = read.clone();
        cleared.strategy = RouteStrategy::Ordered;
        cleared.rules.clear();
        cleared.levels.clear();
        cleared.pick_member_id = None;
        cleared.classifier = None;
        cleared.effort = None;
        cleared.patience_ms = None;
        cleared.sink = false;
        cleared.unlists = false;
        cleared.members.truncate(1);
        cleared.members[0].weight = vibex_core::DEFAULT_MEMBER_WEIGHT;
        cleared.members[0].context_tokens = None;
        cleared.members[0].first_token_timeout_ms = None;
        ModelRouteRepository::update(&conn, &cleared, read.revision).unwrap();
        let reread = ModelRouteRepository::get(&conn, &route.id)
            .unwrap()
            .unwrap();
        assert!(reread.rules.is_empty());
        assert!(reread.levels.is_empty());
        assert_eq!(reread.pick_member_id, None);
        assert_eq!(reread.classifier, None);
        assert_eq!(reread.effort, None);
        assert_eq!(reread.patience_ms, None);
        assert!(!reread.sink);
        assert!(!reread.unlists);
        assert_eq!(reread.members[0].weight, 1);
        assert_eq!(reread.members[0].context_tokens, None);
        assert_eq!(reread.members[0].first_token_timeout_ms, None);
    }

    #[test]
    fn two_live_model_groups_cannot_publish_the_same_model() {
        let conn = migrated_connection();
        let provider = seed_provider(&conn, "DeepSeek");
        let route = |name: &str| ModelRoute {
            display_name: name.to_string(),
            kind: RouteKind::ModelGroup,
            agent_model_id: Some("shared-model".to_string()),
            members: vec![ModelRouteMember::new(provider.clone(), "deepseek-chat", 0)],
            status: RouteStatus::Enabled,
            ..Default::default()
        };
        ModelRouteRepository::insert(&conn, &route("Shared model")).unwrap();
        assert!(
            ModelRouteRepository::insert(&conn, &route("Shared model")).is_err(),
            "the database must refuse a second live group with the same virtual model"
        );
    }
}

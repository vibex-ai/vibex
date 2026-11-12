use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, TransactionBehavior, params};
use vibex_core::default_acp_adapter_id;
use vibex_core::{
    AcpAdapterId, AcpProcessStrategy, AcpProviderConfig, AcpProviderEnvReference,
    AcpProviderEnvSource, AgentConfiguredModelBinding, AgentConfiguredModelBindingId,
    AgentCredential, AgentCredentialStatus, AgentHostCapabilities, AgentId,
    AgentModelProviderBinding, AgentModelProviderBindingId, AgentModelProviderBindingStatus,
    AgentProviderProjectionDescriptorId, AgentProviderProjectionOverrides,
    AgentProviderProjectionRegistry, AgentRuntimeProfile, AgentRuntimeProfileId,
    AgentRuntimeResourcePolicy, AgentRuntimeRouteKey, AgentRuntimeVersionIdentity,
    AgentVersionSource, ModelProviderCatalogEntry, ModelProviderCredentialReference,
    ModelProviderEndpoint, ModelProviderEndpointKind, ModelProviderHeaderReference,
    ModelProviderHeaderValue, ModelProviderHeaderVariable, ModelProviderMetadata,
    ModelProviderProfile, ModelProviderProfileId, ModelProviderProfileStatus,
    ModelProviderProxyPolicy, ProjectionDescriptorMatch, ProjectionEvidenceState,
    ProjectionSecretReference, ProjectionVerificationState, ProviderBindingMetadata,
    ProviderConfiguredModel, ProviderModelWireApi, ProviderOptions, ProviderProfile,
    ProviderProfileId, ProviderProfileStatus, ProviderSecretKind, ProviderSecretSetupState,
    TransportKind, VibexError, VibexResult, WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
    WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS, WIRE_PROTOCOL_OPENAI_RESPONSES,
    acp_agent_catalog_entries, builtin_agent_definitions, unix_timestamp_ms,
};

use super::{
    AgentManagedInstallationRepository, ProviderProfileRepository,
    ProviderSecretReferenceRepository, enum_from_db_sql, enum_to_db, json_from_db_sql, json_to_db,
    map_provider_profile_without_secrets, migration_applied, parse_id_sql, storage_err,
};

const LEGACY_ACP_CONFIG_OPTION_KEY: &str = "acp.config.v1";

pub struct ModelProviderProfileRepository;
pub struct AgentRuntimeProfileRepository;
pub struct AgentModelProviderBindingRepository;
pub struct ProviderProjectionCompatibilityRepository;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyProviderProjectionRecords {
    pub model_provider: ModelProviderProfile,
    pub agent_runtime: AgentRuntimeProfile,
    pub binding: AgentModelProviderBinding,
}

impl ModelProviderProfileRepository {
    pub fn insert(conn: &Connection, profile: &ModelProviderProfile) -> VibexResult<()> {
        profile.validate()?;
        insert_model_provider_profile(conn, profile)
    }

    pub fn get(
        conn: &Connection,
        id: &ModelProviderProfileId,
    ) -> VibexResult<Option<ModelProviderProfile>> {
        conn.query_row(
            "
            SELECT model_provider_profile_id, legacy_provider_profile_id,
                display_name, vendor_hint, endpoints_json, proxy_policy_json,
                credentials_json, configured_models_json, default_model_id,
                headers_json, status, revision, created_at_ms, updated_at_ms,
                deleted_at_ms, metadata_json
            FROM model_provider_profiles
            WHERE model_provider_profile_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![id.as_str()],
            map_model_provider_profile,
        )
        .optional()
        .map_err(storage_err(
            "model_provider_profile_lookup_failed",
            "failed to load model provider profile",
        ))
    }

    pub fn get_by_legacy_profile(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
    ) -> VibexResult<Option<ModelProviderProfile>> {
        conn.query_row(
            "
            SELECT model_provider_profile_id, legacy_provider_profile_id,
                display_name, vendor_hint, endpoints_json, proxy_policy_json,
                credentials_json, configured_models_json, default_model_id,
                headers_json, status, revision, created_at_ms, updated_at_ms,
                deleted_at_ms, metadata_json
            FROM model_provider_profiles
            WHERE legacy_provider_profile_id = ?1
            ",
            params![legacy_id.as_str()],
            map_model_provider_profile,
        )
        .optional()
        .map_err(storage_err(
            "model_provider_profile_legacy_lookup_failed",
            "failed to load model provider profile by legacy identity",
        ))
    }

    /// Soft-deletes a shared Provider and everything that points at it.
    ///
    /// A Provider is not deletable on its own: its bindings would outlive it and
    /// a runtime reading one would find a dangling reference. So the bindings go
    /// first, in the same transaction, and the row is marked rather than removed —
    /// a session that ran through it still has a ledger naming it.
    pub fn soft_delete(
        conn: &Connection,
        provider_id: &ModelProviderProfileId,
        deleted_at_ms: i64,
    ) -> VibexResult<()> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "model_provider_profile_delete_transaction_failed",
                "failed to start the Provider delete transaction",
            ))?;
        // Retire every launch row reached by this Provider before the bindings
        // become tombstones. A shared Provider has no legacy id of its own, so its
        // bindings are the only records that name each Agent's launch row. Leaving
        // those rows live makes the compatibility backfill try to recreate a
        // binding whose legacy unique key is still held by the tombstone.
        transaction
            .execute(
                "UPDATE provider_profiles SET deleted_at_ms = ?2
                 WHERE deleted_at_ms IS NULL
                   AND (
                       provider_profile_id IN (
                           SELECT legacy_provider_profile_id
                           FROM agent_model_provider_bindings_v2
                           WHERE model_provider_profile_id = ?1
                               AND legacy_provider_profile_id IS NOT NULL
                       )
                       OR provider_profile_id IN (
                           SELECT legacy_provider_profile_id
                           FROM model_provider_profiles
                           WHERE model_provider_profile_id = ?1
                               AND legacy_provider_profile_id IS NOT NULL
                       )
                   )",
                params![provider_id.as_str(), deleted_at_ms],
            )
            .map_err(storage_err(
                "model_provider_profile_delete_legacy_failed",
                "failed to delete the launch rows behind this Provider",
            ))?;
        // Their configured models hang off the binding with `ON DELETE CASCADE`, so
        // marking the binding is enough. Runtime profiles are per Agent, not per
        // Provider, so deleting one here would take away a launch contract the
        // Agent still needs.
        transaction
            .execute(
                "UPDATE agent_model_provider_bindings_v2 SET deleted_at_ms = ?2
                 WHERE model_provider_profile_id = ?1 AND deleted_at_ms IS NULL",
                params![provider_id.as_str(), deleted_at_ms],
            )
            .map_err(storage_err(
                "model_provider_profile_delete_cascade_failed",
                "failed to delete the bindings that point at this Provider",
            ))?;
        transaction
            .execute(
                "UPDATE model_provider_profiles SET deleted_at_ms = ?2
                 WHERE model_provider_profile_id = ?1 AND deleted_at_ms IS NULL",
                params![provider_id.as_str(), deleted_at_ms],
            )
            .map_err(storage_err(
                "model_provider_profile_delete_failed",
                "failed to delete the Provider",
            ))?;
        transaction.commit().map_err(storage_err(
            "model_provider_profile_delete_commit_failed",
            "failed to commit the Provider delete",
        ))?;
        Ok(())
    }

    pub fn list(conn: &Connection) -> VibexResult<Vec<ModelProviderProfile>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT model_provider_profile_id, legacy_provider_profile_id,
                    display_name, vendor_hint, endpoints_json, proxy_policy_json,
                    credentials_json, configured_models_json, default_model_id,
                    headers_json, status, revision, created_at_ms, updated_at_ms,
                    deleted_at_ms, metadata_json
                FROM model_provider_profiles
                WHERE deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, model_provider_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "model_provider_profile_list_failed",
                "failed to list model provider profiles",
            ))?;
        collect_rows(
            stmt.query_map([], map_model_provider_profile),
            "model_provider_profile_list_failed",
            "failed to list model provider profiles",
        )
    }

    pub fn update(
        conn: &Connection,
        profile: &ModelProviderProfile,
        expected_revision: i64,
    ) -> VibexResult<ModelProviderProfile> {
        profile.validate()?;
        if profile.revision != expected_revision.saturating_add(1) {
            return Err(VibexError::conflict(
                "model_provider_profile_revision_invalid",
                "model provider profile revision must advance exactly once",
            ));
        }
        let changed = conn
            .execute(
                "
                UPDATE model_provider_profiles
                SET display_name = ?3,
                    vendor_hint = ?4,
                    endpoints_json = ?5,
                    proxy_policy_json = ?6,
                    credentials_json = ?7,
                    configured_models_json = ?8,
                    default_model_id = ?9,
                    headers_json = ?10,
                    status = ?11,
                    revision = ?12,
                    updated_at_ms = ?13,
                    deleted_at_ms = ?14,
                    metadata_json = ?15
                WHERE model_provider_profile_id = ?1
                    AND revision = ?2
                    AND deleted_at_ms IS NULL
                ",
                params![
                    profile.id.as_str(),
                    expected_revision,
                    profile.display_name,
                    profile.vendor_hint,
                    json_to_db(&profile.endpoints)?,
                    json_to_db(&profile.proxy_policy)?,
                    json_to_db(&profile.credentials)?,
                    json_to_db(&profile.configured_models)?,
                    profile.default_model_id,
                    json_to_db(&profile.headers)?,
                    enum_to_db(&profile.status)?,
                    profile.revision,
                    profile.updated_at_ms,
                    profile.deleted_at_ms,
                    json_to_db(&profile.metadata)?,
                ],
            )
            .map_err(storage_err(
                "model_provider_profile_update_failed",
                "failed to update model provider profile",
            ))?;
        require_revision_update(
            changed,
            "model_provider_profile_revision_conflict",
            "model provider profile changed since it was loaded",
        )?;
        Self::get(conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "model_provider_profile_update_readback_failed",
                "failed to read model provider profile after update",
            )
        })
    }
}

impl AgentRuntimeProfileRepository {
    pub fn insert(conn: &Connection, profile: &AgentRuntimeProfile) -> VibexResult<()> {
        profile.validate()?;
        insert_agent_runtime_profile(conn, profile)
    }

    pub fn get(
        conn: &Connection,
        id: &AgentRuntimeProfileId,
    ) -> VibexResult<Option<AgentRuntimeProfile>> {
        conn.query_row(
            "
            SELECT agent_runtime_profile_id, legacy_provider_profile_id,
                version_identity_json, command, args_json,
                safe_env_references_json, cwd_template, process_strategy,
                runtime_home_strategy, host_capabilities_json,
                resource_policy_json, models_json, modes_json, features_json,
                disabled_tools_json, revision, created_at_ms, updated_at_ms,
                deleted_at_ms
            FROM agent_runtime_profiles
            WHERE agent_runtime_profile_id = ?1 AND deleted_at_ms IS NULL
            ",
            params![id.as_str()],
            map_agent_runtime_profile,
        )
        .optional()
        .map_err(storage_err(
            "agent_runtime_profile_lookup_failed",
            "failed to load Agent runtime profile",
        ))
    }

    pub fn get_by_legacy_profile(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
    ) -> VibexResult<Option<AgentRuntimeProfile>> {
        conn.query_row(
            "
            SELECT agent_runtime_profile_id, legacy_provider_profile_id,
                version_identity_json, command, args_json,
                safe_env_references_json, cwd_template, process_strategy,
                runtime_home_strategy, host_capabilities_json,
                resource_policy_json, models_json, modes_json, features_json,
                disabled_tools_json, revision, created_at_ms, updated_at_ms,
                deleted_at_ms
            FROM agent_runtime_profiles
            WHERE legacy_provider_profile_id = ?1
            ",
            params![legacy_id.as_str()],
            map_agent_runtime_profile,
        )
        .optional()
        .map_err(storage_err(
            "agent_runtime_profile_legacy_lookup_failed",
            "failed to load Agent runtime profile by legacy identity",
        ))
    }

    pub fn list_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentRuntimeProfile>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT agent_runtime_profile_id, legacy_provider_profile_id,
                    version_identity_json, command, args_json,
                    safe_env_references_json, cwd_template, process_strategy,
                    runtime_home_strategy, host_capabilities_json,
                    resource_policy_json, models_json, modes_json, features_json,
                    disabled_tools_json, revision, created_at_ms, updated_at_ms,
                    deleted_at_ms
                FROM agent_runtime_profiles
                WHERE agent_id = ?1 AND deleted_at_ms IS NULL
                ORDER BY updated_at_ms DESC, agent_runtime_profile_id ASC
                ",
            )
            .map_err(storage_err(
                "agent_runtime_profile_list_failed",
                "failed to list Agent runtime profiles",
            ))?;
        collect_rows(
            stmt.query_map(params![agent_id.as_str()], map_agent_runtime_profile),
            "agent_runtime_profile_list_failed",
            "failed to list Agent runtime profiles",
        )
    }

    pub fn update(
        conn: &Connection,
        profile: &AgentRuntimeProfile,
        expected_revision: i64,
    ) -> VibexResult<AgentRuntimeProfile> {
        profile.validate()?;
        if profile.revision != expected_revision.saturating_add(1) {
            return Err(VibexError::conflict(
                "agent_runtime_profile_revision_invalid",
                "Agent runtime profile revision must advance exactly once",
            ));
        }
        let changed = conn
            .execute(
                "
                UPDATE agent_runtime_profiles
                SET agent_id = ?3,
                    adapter_id = ?4,
                    version_identity_json = ?5,
                    command = ?6,
                    args_json = ?7,
                    safe_env_references_json = ?8,
                    cwd_template = ?9,
                    process_strategy = ?10,
                    runtime_home_strategy = ?11,
                    host_capabilities_json = ?12,
                    resource_policy_json = ?13,
                    models_json = ?14,
                    modes_json = ?15,
                    features_json = ?16,
                    disabled_tools_json = ?17,
                    revision = ?18,
                    updated_at_ms = ?19,
                    deleted_at_ms = ?20
                WHERE agent_runtime_profile_id = ?1
                    AND revision = ?2
                    AND deleted_at_ms IS NULL
                ",
                params![
                    profile.id.as_str(),
                    expected_revision,
                    profile.version_identity.route.agent_id.as_str(),
                    profile.version_identity.route.adapter_id.as_str(),
                    json_to_db(&profile.version_identity)?,
                    profile.command,
                    json_to_db(&profile.args)?,
                    json_to_db(&profile.safe_env_references)?,
                    profile.cwd_template,
                    enum_to_db(&profile.process_strategy)?,
                    enum_to_db(&profile.runtime_home_strategy)?,
                    json_to_db(&profile.host_capabilities)?,
                    json_to_db(&profile.resource_policy)?,
                    json_to_db(&profile.models)?,
                    json_to_db(&profile.modes)?,
                    json_to_db(&profile.features)?,
                    json_to_db(&profile.disabled_tools)?,
                    profile.revision,
                    profile.updated_at_ms,
                    profile.deleted_at_ms,
                ],
            )
            .map_err(storage_err(
                "agent_runtime_profile_update_failed",
                "failed to update Agent runtime profile",
            ))?;
        require_revision_update(
            changed,
            "agent_runtime_profile_revision_conflict",
            "Agent runtime profile changed since it was loaded",
        )?;
        Self::get(conn, &profile.id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_runtime_profile_update_readback_failed",
                "failed to read Agent runtime profile after update",
            )
        })
    }
}

impl AgentModelProviderBindingRepository {
    pub fn insert(conn: &Connection, binding: &AgentModelProviderBinding) -> VibexResult<()> {
        binding.validate()?;
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "agent_model_provider_binding_transaction_failed",
                "failed to start Agent provider binding transaction",
            ))?;
        insert_agent_model_provider_binding(&transaction, binding)?;
        transaction.commit().map_err(storage_err(
            "agent_model_provider_binding_commit_failed",
            "failed to commit Agent provider binding",
        ))
    }

    pub fn get(
        conn: &Connection,
        id: &AgentModelProviderBindingId,
    ) -> VibexResult<Option<AgentModelProviderBinding>> {
        let mut binding = conn
            .query_row(
                "
                SELECT agent_model_provider_binding_id, legacy_provider_profile_id,
                    agent_id, agent_runtime_profile_id, model_provider_profile_id,
                    projection_descriptor_id, projection_overrides_json,
                    projection_fingerprint, status, verification_json, revision,
                    created_at_ms, updated_at_ms, deleted_at_ms,
                    account_alias, reasoning_effort, provider_options_json
                FROM agent_model_provider_bindings_v2
                WHERE agent_model_provider_binding_id = ?1 AND deleted_at_ms IS NULL
                ",
                params![id.as_str()],
                map_agent_model_provider_binding_without_models,
            )
            .optional()
            .map_err(storage_err(
                "agent_model_provider_binding_lookup_failed",
                "failed to load Agent provider binding",
            ))?;
        if let Some(binding) = binding.as_mut() {
            binding.configured_models = list_configured_model_bindings(conn, &binding.id)?;
        }
        Ok(binding)
    }

    pub fn get_by_legacy_profile(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
    ) -> VibexResult<Option<AgentModelProviderBinding>> {
        let id = conn
            .query_row(
                "
                SELECT agent_model_provider_binding_id
                FROM agent_model_provider_bindings_v2
                WHERE legacy_provider_profile_id = ?1
                ",
                params![legacy_id.as_str()],
                |row| parse_id_sql(row.get(0)?, AgentModelProviderBindingId::parse),
            )
            .optional()
            .map_err(storage_err(
                "agent_model_provider_binding_legacy_lookup_failed",
                "failed to load Agent provider binding by legacy identity",
            ))?;
        id.map(|id| Self::get(conn, &id))
            .transpose()
            .map(Option::flatten)
    }

    /// When a legacy identity is held by a deleted binding.
    ///
    /// The unique legacy id deliberately survives a soft delete so historical
    /// sessions keep their ledger. Backfill must distinguish that tombstone from a
    /// missing binding; treating it as missing attempts a duplicate INSERT.
    pub fn tombstone_timestamp(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
    ) -> VibexResult<Option<i64>> {
        conn.query_row(
            "
            SELECT MAX(deleted_at_ms)
            FROM agent_model_provider_bindings_v2
            WHERE legacy_provider_profile_id = ?1
                AND deleted_at_ms IS NOT NULL
            ",
            params![legacy_id.as_str()],
            |row| row.get(0),
        )
        .map_err(storage_err(
            "agent_model_provider_binding_tombstone_lookup_failed",
            "failed to inspect an Agent provider binding tombstone",
        ))
    }

    pub fn list_for_model_provider(
        conn: &Connection,
        id: &ModelProviderProfileId,
    ) -> VibexResult<Vec<AgentModelProviderBinding>> {
        list_bindings_where(
            conn,
            "model_provider_profile_id = ?1",
            id.as_str(),
            "agent_model_provider_binding_provider_list_failed",
        )
    }

    pub fn list_for_agent(
        conn: &Connection,
        agent_id: &AgentId,
    ) -> VibexResult<Vec<AgentModelProviderBinding>> {
        list_bindings_where(
            conn,
            "agent_id = ?1",
            agent_id.as_str(),
            "agent_model_provider_binding_agent_list_failed",
        )
    }

    /// Every binding, for a reader that aggregates across Agents.
    ///
    /// The Config Center's global Providers page answers "which Agents use this
    /// Provider", which needs the inverse of both indexed lookups. It is a
    /// deliberate contract of its own rather than an empty filter on
    /// [`list_for_agent`]: that lookup refuses an unfiltered request on
    /// purpose, so a caller cannot accidentally scan the table by passing
    /// nothing.
    pub fn list_all(conn: &Connection) -> VibexResult<Vec<AgentModelProviderBinding>> {
        list_bindings(
            conn,
            "1 = 1",
            (),
            "agent_model_provider_binding_list_failed",
        )
    }

    pub fn list_for_runtime(
        conn: &Connection,
        id: &AgentRuntimeProfileId,
    ) -> VibexResult<Vec<AgentModelProviderBinding>> {
        list_bindings_where(
            conn,
            "agent_runtime_profile_id = ?1",
            id.as_str(),
            "agent_model_provider_binding_runtime_list_failed",
        )
    }

    pub fn update(
        conn: &Connection,
        binding: &AgentModelProviderBinding,
        expected_revision: i64,
    ) -> VibexResult<AgentModelProviderBinding> {
        binding.validate()?;
        if binding.revision != expected_revision.saturating_add(1) {
            return Err(VibexError::conflict(
                "agent_model_provider_binding_revision_invalid",
                "Agent provider binding revision must advance exactly once",
            ));
        }
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "agent_model_provider_binding_transaction_failed",
                "failed to start Agent provider binding transaction",
            ))?;
        let changed = transaction
            .execute(
                "
                UPDATE agent_model_provider_bindings_v2
                SET agent_id = ?3,
                    agent_runtime_profile_id = ?4,
                    model_provider_profile_id = ?5,
                    projection_descriptor_id = ?6,
                    projection_overrides_json = ?7,
                    projection_fingerprint = ?8,
                    status = ?9,
                    verification_json = ?10,
                    revision = ?11,
                    updated_at_ms = ?12,
                    deleted_at_ms = ?13,
                    account_alias = ?14,
                    reasoning_effort = ?15,
                    provider_options_json = ?16
                WHERE agent_model_provider_binding_id = ?1
                    AND revision = ?2
                    AND deleted_at_ms IS NULL
                ",
                params![
                    binding.id.as_str(),
                    expected_revision,
                    binding.agent_id.as_str(),
                    binding.runtime_profile_id.as_str(),
                    binding
                        .model_provider_profile_id
                        .as_ref()
                        .map(ModelProviderProfileId::as_str),
                    binding.projection_descriptor_id.as_str(),
                    json_to_db(&binding.projection_overrides)?,
                    binding.projection_fingerprint,
                    enum_to_db(&binding.status)?,
                    json_to_db(&binding.verification)?,
                    binding.revision,
                    binding.updated_at_ms,
                    binding.deleted_at_ms,
                    binding.account_alias,
                    binding.reasoning_effort,
                    json_to_db(&binding.provider_options)?,
                ],
            )
            .map_err(storage_err(
                "agent_model_provider_binding_update_failed",
                "failed to update Agent provider binding",
            ))?;
        require_revision_update(
            changed,
            "agent_model_provider_binding_revision_conflict",
            "Agent provider binding changed since it was loaded",
        )?;
        replace_configured_model_bindings(&transaction, binding)?;
        transaction.commit().map_err(storage_err(
            "agent_model_provider_binding_commit_failed",
            "failed to commit Agent provider binding update",
        ))?;
        Self::get(conn, &binding.id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_model_provider_binding_update_readback_failed",
                "failed to read Agent provider binding after update",
            )
        })
    }

    pub fn set_projection_state(
        conn: &Connection,
        id: &AgentModelProviderBindingId,
        expected_revision: i64,
        fingerprint: Option<&str>,
        status: AgentModelProviderBindingStatus,
        verification: &ProjectionVerificationState,
        updated_at_ms: i64,
    ) -> VibexResult<AgentModelProviderBinding> {
        let changed = conn
            .execute(
                "
                UPDATE agent_model_provider_bindings_v2
                SET projection_fingerprint = ?3,
                    status = ?4,
                    verification_json = ?5,
                    revision = revision + 1,
                    updated_at_ms = ?6
                WHERE agent_model_provider_binding_id = ?1
                    AND revision = ?2
                    AND deleted_at_ms IS NULL
                ",
                params![
                    id.as_str(),
                    expected_revision,
                    fingerprint,
                    enum_to_db(&status)?,
                    json_to_db(verification)?,
                    updated_at_ms,
                ],
            )
            .map_err(storage_err(
                "agent_model_provider_binding_projection_update_failed",
                "failed to update Agent provider binding projection state",
            ))?;
        require_revision_update(
            changed,
            "agent_model_provider_binding_revision_conflict",
            "Agent provider binding changed since it was loaded",
        )?;
        Self::get(conn, id)?.ok_or_else(|| {
            VibexError::storage(
                "agent_model_provider_binding_projection_readback_failed",
                "failed to read Agent provider binding projection state",
            )
        })
    }
}

impl ProviderProjectionCompatibilityRepository {
    /// Idempotently creates the v2 provider/runtime/binding records for every
    /// legacy online ACP profile. This function performs no process or file IO.
    pub fn backfill_legacy_profiles(conn: &Connection) -> VibexResult<usize> {
        let profiles = list_legacy_provider_profiles(conn)?;
        let mut changed = 0;
        for profile in profiles {
            let before =
                AgentModelProviderBindingRepository::get_by_legacy_profile(conn, &profile.id)?;
            // A deleted binding leaves its unique legacy identity behind. If its
            // launch row is still live, projection would collide with that
            // tombstone. Retire the stale launch row instead of recreating a
            // Provider the user already deleted.
            if before.is_none()
                && let Some(deleted_at_ms) =
                    AgentModelProviderBindingRepository::tombstone_timestamp(conn, &profile.id)?
            {
                conn.execute(
                    "UPDATE provider_profiles
                     SET deleted_at_ms = ?2
                     WHERE provider_profile_id = ?1 AND deleted_at_ms IS NULL",
                    params![profile.id.as_str(), deleted_at_ms],
                )
                .map_err(storage_err(
                    "provider_projection_legacy_tombstone_repair_failed",
                    "failed to retire a launch row whose Agent provider binding was deleted",
                ))?;
                continue;
            }
            // A launch row an Agent materialised from a shared Provider already
            // points at that Provider, and the v2 id derived from this row is not
            // the record it belongs to. Projecting it would create a second
            // Provider, so one Provider would appear once per Agent on the
            // Providers page. Repair still happens for the ordinary case: a row
            // whose binding points at its own derived record.
            if let Some(existing) = before.as_ref()
                && Self::shared_record_reached_by_binding(conn, existing)?.is_some()
            {
                continue;
            }
            Self::sync_legacy_profile(conn, &profile)?;
            if before.is_none() {
                changed += 1;
            }
        }
        Ok(changed)
    }

    /// The Provider a launch row already reaches, when it is not the record derived
    /// from that row.
    ///
    /// A launch row is one Agent's copy of a shared Provider, and the binding is the
    /// only record that says which Provider the copy reaches: the shared record is
    /// keyed by the legacy id of the row it was first derived from, and every later
    /// Agent's row has a different id. So the answer cannot be derived from the row
    /// — it has to be read from the binding.
    ///
    /// `None` is the ordinary case: the row's binding names the record derived from
    /// it, and projecting it again is what keeps that record and its protocol
    /// repaired.
    fn shared_record_reached_by_binding(
        conn: &Connection,
        binding: &AgentModelProviderBinding,
    ) -> VibexResult<Option<ModelProviderProfileId>> {
        let Some(provider_id) = binding.model_provider_profile_id.as_ref() else {
            return Ok(None);
        };
        let Some(provider) = ModelProviderProfileRepository::get(conn, provider_id)? else {
            // The Provider is gone; re-deriving is the only way to leave this launch
            // row something to reach.
            return Ok(None);
        };
        if provider
            .legacy_provider_profile_id
            .as_ref()
            .is_some_and(|id| Some(id) == binding.legacy_provider_profile_id.as_ref())
        {
            return Ok(None);
        }
        Ok(Some(provider_id.clone()))
    }

    /// Folds a launch row's copy of a Provider into the record it already reaches.
    ///
    /// A second Agent's launch row contributes a runtime and a binding, not a second
    /// Provider. Its row is a copy made when the Agent bound the Provider, and some of
    /// what it carries is not the Provider's to begin with: `provider_options` holds
    /// the ACP config, the protocol pin and the import markers — properties of how
    /// that Agent launches, not of the shared record. Writing the copy back wholesale
    /// would move those onto the record *and* overwrite the Keys with the copy's own
    /// secret ids, which every other Agent's binding still names.
    ///
    /// So only what the record actually owns flows — the name, the status, the
    /// addresses and the model list — while the identity, the Key list and those
    /// Agent-only options stay with the record. `explicit` separates an edit from a
    /// refresh: a refresh must not push a copy's possibly stale content over the
    /// record the Providers page owns.
    fn merge_into_shared(
        conn: &Connection,
        shared_id: &ModelProviderProfileId,
        candidate: ModelProviderProfile,
        explicit: bool,
    ) -> VibexResult<ModelProviderProfile> {
        let Some(existing) = ModelProviderProfileRepository::get(conn, shared_id)? else {
            return upsert_legacy_model_provider(conn, candidate, explicit);
        };
        if !explicit {
            return Ok(existing);
        }
        let mut merged = ModelProviderProfile {
            display_name: candidate.display_name,
            vendor_hint: candidate.vendor_hint,
            endpoints: candidate.endpoints,
            // One launch row is one Agent's view of the catalogue, and that view
            // is filtered by the protocols the Agent speaks. Taking it wholesale
            // would delete every Model the other Agents can reach but this one
            // cannot, so the record keeps its own list and the copy only
            // contributes what it knows: a new Model, and each Model's label,
            // capabilities and visibility.
            configured_models: merge_model_catalog(
                &existing.configured_models,
                &candidate.configured_models,
            ),
            default_model_id: candidate.default_model_id,
            headers: candidate.headers,
            // The note and the website are the fields an Agent-page edit can
            // change; without them here the merge would silently keep the old
            // values while reporting success.
            metadata: ModelProviderMetadata {
                note: candidate.metadata.note,
                website_url: candidate.metadata.website_url,
                ..existing.metadata.clone()
            },
            status: candidate.status,
            ..existing.clone()
        };
        if same_model_provider_content(&existing, &merged) {
            return Ok(existing);
        }
        merged.revision = existing.revision.saturating_add(1);
        merged.updated_at_ms = merged
            .updated_at_ms
            .max(existing.updated_at_ms.saturating_add(1));
        ModelProviderProfileRepository::update(conn, &merged, existing.revision)
    }

    /// Projects a legacy Profile onto an *existing* shared record.
    ///
    /// The ordinary backfill derives a v2 id from the legacy id, which is right
    /// when the legacy Profile is the source of truth. It is wrong for the second
    /// Agent that binds a shared Provider: that Agent gets its own launch row, but
    /// the Provider it reaches is the same one. Deriving a v2 record from the new
    /// row made one Provider appear once per Agent on the Providers page, which is
    /// the opposite of what that page exists to show.
    ///
    /// So this maps the records the same way and then redirects the model-provider
    /// and binding onto the shared record the caller names.
    pub fn sync_legacy_profile_into_shared(
        conn: &Connection,
        legacy: &ProviderProfile,
        shared_id: &ModelProviderProfileId,
    ) -> VibexResult<LegacyProviderProjectionRecords> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "provider_projection_backfill_transaction_failed",
                "failed to start provider projection compatibility transaction",
            ))?;
        let mut records = map_legacy_projection_records(&transaction, legacy)?;
        // Keep the shared record; this legacy Profile contributes a binding and a
        // runtime, not a second Provider.
        records.model_provider.id = shared_id.clone();
        let agent_runtime = upsert_legacy_agent_runtime(&transaction, records.agent_runtime)?;
        let mut binding = records.binding;
        binding.model_provider_profile_id = Some(shared_id.clone());
        binding.runtime_profile_id = agent_runtime.id.clone();
        let binding = upsert_legacy_binding(&transaction, binding)?;
        transaction.commit().map_err(storage_err(
            "provider_projection_backfill_commit_failed",
            "failed to commit provider projection compatibility records",
        ))?;
        Ok(LegacyProviderProjectionRecords {
            model_provider: ModelProviderProfileRepository::get(conn, shared_id)?
                .unwrap_or(records.model_provider),
            agent_runtime,
            binding,
        })
    }

    /// Projects a legacy Profile onto v2 as an explicit write, not a backfill.
    ///
    /// The difference matters after the cutover: a backfill must not overwrite the
    /// authoritative shared record, but an edit the user just made through a v1
    /// entry point must reach it, or the two would disagree the moment they edit.
    /// So this variant forces the write.
    pub fn sync_legacy_profile_from_edit(
        conn: &Connection,
        legacy: &ProviderProfile,
    ) -> VibexResult<LegacyProviderProjectionRecords> {
        Self::sync_legacy_profile_inner(conn, legacy, true)
    }

    pub fn sync_legacy_profile(
        conn: &Connection,
        legacy: &ProviderProfile,
    ) -> VibexResult<LegacyProviderProjectionRecords> {
        Self::sync_legacy_profile_inner(conn, legacy, false)
    }

    fn sync_legacy_profile_inner(
        conn: &Connection,
        legacy: &ProviderProfile,
        explicit: bool,
    ) -> VibexResult<LegacyProviderProjectionRecords> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "provider_projection_backfill_transaction_failed",
                "failed to start provider projection compatibility transaction",
            ))?;
        let records = map_legacy_projection_records(&transaction, legacy)?;
        // A launch row that already reaches a shared Provider keeps reaching it.
        // Deriving a record from its own id is what turned one Provider into one
        // record per Agent: the derivation cannot know that this row is a copy.
        let existing =
            AgentModelProviderBindingRepository::get_by_legacy_profile(&transaction, &legacy.id)?;
        let reached = match existing.as_ref() {
            Some(binding) => Self::shared_record_reached_by_binding(&transaction, binding)?,
            None => None,
        };
        let model_provider = match reached {
            Some(shared_id) => Self::merge_into_shared(
                &transaction,
                &shared_id,
                ModelProviderProfile {
                    id: shared_id.clone(),
                    ..records.model_provider
                },
                explicit,
            )?,
            None => upsert_legacy_model_provider(&transaction, records.model_provider, explicit)?,
        };
        let agent_runtime = upsert_legacy_agent_runtime(&transaction, records.agent_runtime)?;
        let mut binding = records.binding;
        binding.model_provider_profile_id = Some(model_provider.id.clone());
        binding.runtime_profile_id = agent_runtime.id.clone();
        let binding = upsert_legacy_binding(&transaction, binding)?;
        transaction.commit().map_err(storage_err(
            "provider_projection_backfill_commit_failed",
            "failed to commit provider projection compatibility records",
        ))?;
        Ok(LegacyProviderProjectionRecords {
            model_provider,
            agent_runtime,
            binding,
        })
    }

    /// Projects one shared Provider Profile back onto its legacy Profile row.
    ///
    /// The compatibility layer is the only thing that made v1 and v2 agree, and it
    /// runs one way: a v1 write becomes a v2 row. Without the reverse, a Key added
    /// on the global Providers page exists only in the v2 record, and the next v1
    /// write on the same Provider rebuilds `credentials` from the v1 secret table
    /// and drops it.
    ///
    /// The reverse therefore writes only what both shapes share, plus the one thing
    /// the launch path resolves from v1: the Key list. `provider_options` is left
    /// exactly as it is, because it carries the ACP config, the protocol pin and the
    /// import markers — properties of how an Agent launches, not of a shared
    /// Provider. Overwriting it from v2 would strip the Agent of its protocol.
    ///
    /// A Profile with no legacy row is not projected: it was created outside the v1
    /// surface and has no launch path to keep in step.
    /// Creates the v2 record for a legacy Profile that is about to exist.
    ///
    /// This is the direction reversal, made concrete. A legacy Profile can no
    /// longer be the only row a Provider has: the shared record is what the
    /// Providers page edits and what the catalogue reads, and the legacy row is
    /// its launch projection. So creation writes v2 first, and the legacy row is
    /// derived from it.
    ///
    /// The v2 id is derived from the legacy id, because the direct-route backfill
    /// and the launch path both derive it that way. Deriving it here rather than
    /// inventing one is what keeps those two able to find the record.
    pub fn create_model_provider_from_legacy(
        conn: &Connection,
        legacy: &ProviderProfile,
    ) -> VibexResult<ModelProviderProfile> {
        let now = vibex_core::unix_timestamp_ms().max(1);
        let provider = ModelProviderProfile {
            id: legacy_model_provider_id(&legacy.id)?,
            legacy_provider_profile_id: Some(legacy.id.clone()),
            display_name: legacy.display_name.clone(),
            vendor_hint: None,
            endpoints: primary_endpoints_from_legacy(legacy),
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: legacy
                .configured_models
                .iter()
                .map(|model| ModelProviderCatalogEntry {
                    id: model.id.clone(),
                    display_name: model.display_name.clone(),
                    enabled: model.enabled,
                    capabilities: model.capabilities.clone(),
                    metadata: Vec::new(),
                })
                .collect(),
            default_model_id: legacy.default_model.clone(),
            headers: Vec::new(),
            metadata: Default::default(),
            status: match legacy.status {
                ProviderProfileStatus::Enabled => ModelProviderProfileStatus::Enabled,
                ProviderProfileStatus::Disabled => ModelProviderProfileStatus::Disabled,
            },
            revision: 1,
            created_at_ms: legacy.created_at_ms.min(now),
            updated_at_ms: legacy.updated_at_ms.min(now),
            deleted_at_ms: legacy.deleted_at_ms,
        };
        // The v2 model refuses a default that is not an enabled configured model,
        // and that rule is right for a Provider created on the v2 side. A legacy
        // Profile written before this direction existed may name a default before
        // its model list is filled in — the catalog arrives later — so the
        // projection keeps the value and lets the v2 rule apply to v2 edits.
        // Dropping it here would silently change what the Agent launches with.
        insert_model_provider_profile(conn, &provider)?;
        Ok(provider)
    }

    pub fn project_model_provider_to_legacy(
        conn: &Connection,
        provider: &ModelProviderProfile,
    ) -> VibexResult<bool> {
        let Some(legacy_id) = provider.legacy_provider_profile_id.as_ref() else {
            return Ok(false);
        };
        Ok(
            match Self::project_shared_provider_onto_legacy(conn, provider, legacy_id, None)? {
                Some((_, changed)) => changed,
                None => false,
            },
        )
    }

    /// Projects a shared Provider onto one specific launch row.
    ///
    /// [`Self::project_model_provider_to_legacy`] can only reach the row the
    /// record was derived from, because `legacy_provider_profile_id` is unique
    /// and names exactly one Agent. A Provider bound to a second Agent has no
    /// such pointer — the binding is the only link — so the fan-out passes the
    /// row explicitly. Everything else is the same projection: the shared
    /// record owns the name, status, address, catalogue, default and Keys, and
    /// the launch row is its Agent-facing copy.
    ///
    /// `configured_models` is supplied by the caller when the Agent's protocol
    /// decided which Models it can actually reach; `None` derives the list from
    /// the record, keeping each Model's declared protocol.
    ///
    /// Returns the row and whether it changed, or `None` when the row is gone.
    pub fn project_shared_provider_onto_legacy(
        conn: &Connection,
        provider: &ModelProviderProfile,
        legacy_id: &ProviderProfileId,
        configured_models: Option<&[ProviderConfiguredModel]>,
    ) -> VibexResult<Option<(ProviderProfile, bool)>> {
        let Some(mut legacy) = ProviderProfileRepository::get(conn, legacy_id)? else {
            return Ok(None);
        };
        let changed = apply_shared_provider_to_legacy(&mut legacy, provider, configured_models);
        if changed {
            ProviderProfileRepository::update(conn, &legacy)?;
            ProviderSecretReferenceRepository::replace_for_profile(
                conn,
                legacy_id,
                &legacy.secrets,
            )?;
        }
        Ok(Some((legacy, changed)))
    }

    /// Projects a shared Provider onto every Agent that binds it.
    ///
    /// The single-row projection above reaches one launch row, and only the one
    /// the record happened to be derived from. A Provider bound to a second
    /// Agent has no such pointer — `legacy_provider_profile_id` is `UNIQUE` and
    /// already names the first Agent's row — so the binding is the only link,
    /// and this walks it.
    ///
    /// Every Agent gets the same Provider-owned content: the name, status,
    /// address, Keys, note and default. What stays per Agent is what describes
    /// how that Agent launches: the ACP config's command and args, the protocol
    /// pin, the import markers and the Agent's own sandbox defaults. Which
    /// Models an Agent is offered is the intersection of the record's catalogue
    /// with the protocols that Agent declares.
    ///
    /// Returns the launch rows that actually changed, so the caller can restale
    /// the bindings and announce them.
    pub fn sync_shared_provider_to_bindings(
        conn: &Connection,
        provider: &ModelProviderProfile,
    ) -> VibexResult<Vec<ProviderProfile>> {
        let registry = AgentProviderProjectionRegistry::builtin()?;
        let mut touched_rows = Vec::new();
        let mut changed_rows = Vec::new();
        for binding in
            AgentModelProviderBindingRepository::list_for_model_provider(conn, &provider.id)?
        {
            let Some(legacy_id) = binding.legacy_provider_profile_id.as_ref() else {
                // A binding written natively on v2 has no launch row of its own;
                // the projection engine reads the record through the binding. Its
                // configured models are still a copy of the catalogue taken when
                // it was written, so they are rebuilt in place below.
                continue;
            };
            let Some(runtime) =
                AgentRuntimeProfileRepository::get(conn, &binding.runtime_profile_id)?
            else {
                continue;
            };
            let Some(mut legacy) = ProviderProfileRepository::get(conn, legacy_id)? else {
                continue;
            };
            // The hidden gateway row is written from the route catalogue, and
            // its model ids are the listener's `group/<route>/<model>` names.
            // Projecting the upstream catalogue onto it would leave the Agent
            // pointed at a gateway that does not serve those ids.
            if is_model_gateway_row(&legacy) {
                continue;
            }
            let before = legacy.clone();
            let interfaces = registry
                .resolve(&runtime.version_identity)?
                .descriptor
                .model_interfaces;
            let secrets = legacy_secrets_linked_by_lookup_key(provider, &before);
            apply_shared_provider_fields(&mut legacy, provider, secrets);
            sync_protocol_pins(&mut legacy, provider);
            legacy.configured_models =
                configured_models_for_agent(provider, &before, &legacy, &interfaces);
            repair_legacy_default_model(&mut legacy);
            let models =
                enabled_model_ids(&legacy.configured_models, legacy.default_model.as_deref());
            write_acp_models(&mut legacy.provider_options, &models)?;
            let changed = finish_legacy_row(&mut legacy, &before);
            if changed {
                ProviderProfileRepository::update(conn, &legacy)?;
                ProviderSecretReferenceRepository::replace_for_profile(
                    conn,
                    legacy_id,
                    &legacy.secrets,
                )?;
                // The ACP catalogue the launch path reads from the runtime
                // Profile is a second copy of the same list, and a Model this
                // Agent gained is invisible until it names that Model too. A
                // runtime Profile can in principle serve several bindings, and
                // then its `models` is not this Provider's to write.
                let shared_runtime =
                    AgentModelProviderBindingRepository::list_for_runtime(conn, &runtime.id)?.len()
                        > 1;
                if !shared_runtime && runtime.models != models {
                    let mut next = runtime.clone();
                    next.models = models;
                    next.revision = runtime.revision.saturating_add(1);
                    next.updated_at_ms =
                        unix_timestamp_ms().max(runtime.updated_at_ms.saturating_add(1));
                    AgentRuntimeProfileRepository::update(conn, &next, runtime.revision)?;
                }
                changed_rows.push(legacy.clone());
            }
            touched_rows.push(legacy);
        }
        // Every binding is rebuilt, not only the ones behind a row that moved: a
        // binding written before this fan-out existed still carries the model
        // list it was created with, and nothing else would ever repair it.
        let mut notified: Vec<ProviderProfileId> =
            changed_rows.iter().map(|row| row.id.clone()).collect();
        for legacy in &touched_rows {
            if Self::rebuild_binding_models(conn, provider, legacy)?
                && !notified.iter().any(|id| id == &legacy.id)
            {
                // The row itself did not move, but what the Agent may select did.
                // A page that only hears about the record's own pointer would keep
                // showing the list it had.
                notified.push(legacy.id.clone());
                changed_rows.push(legacy.clone());
            }
        }
        Ok(changed_rows)
    }

    /// Rewrites a binding's configured models from the record's catalogue.
    ///
    /// Used for a binding that has no launch row — one written directly on v2.
    /// Its models are a copy of the catalogue, and a Model added on the shared
    /// page has to reach it the same way it reaches a bound Agent's row.
    ///
    /// Returns whether the binding changed.
    pub fn sync_v2_binding_models(
        conn: &Connection,
        provider: &ModelProviderProfile,
    ) -> VibexResult<bool> {
        let registry = AgentProviderProjectionRegistry::builtin()?;
        let mut changed = false;
        for binding in
            AgentModelProviderBindingRepository::list_for_model_provider(conn, &provider.id)?
        {
            if binding.legacy_provider_profile_id.is_some() {
                continue;
            }
            let Some(runtime) =
                AgentRuntimeProfileRepository::get(conn, &binding.runtime_profile_id)?
            else {
                continue;
            };
            let interfaces = registry
                .resolve(&runtime.version_identity)?
                .descriptor
                .model_interfaces;
            let mut models: Vec<AgentConfiguredModelBinding> = Vec::new();
            for model in &provider.configured_models {
                let wire_api = shared_model_wire_api_of(model);
                let wire_protocol_id =
                    legacy_wire_protocol_for(&binding.agent_id, wire_api, &interfaces);
                if !interfaces.is_empty()
                    && !interfaces
                        .iter()
                        .any(|interface| interface.wire_protocol_id == wire_protocol_id)
                {
                    continue;
                }
                let sdk_adapter_id = interfaces
                    .iter()
                    .find(|interface| interface.wire_protocol_id == wire_protocol_id)
                    .and_then(|interface| interface.sdk_adapter_id.clone())
                    .or_else(|| legacy_sdk_adapter(&binding.agent_id, &wire_protocol_id));
                let process_scoped = interfaces
                    .iter()
                    .find(|interface| {
                        interface.wire_protocol_id == wire_protocol_id
                            && interface.sdk_adapter_id == sdk_adapter_id
                    })
                    .is_some_and(|interface| interface.process_scoped);
                if models
                    .iter()
                    .any(|existing| existing.agent_model_id == model.id)
                {
                    continue;
                }
                let id = binding
                    .configured_models
                    .iter()
                    .find(|existing| existing.provider_model_id == model.id)
                    .map(|existing| existing.id.clone())
                    .unwrap_or_else(AgentConfiguredModelBindingId::new);
                models.push(AgentConfiguredModelBinding {
                    id,
                    provider_model_id: model.id.clone(),
                    agent_model_id: model.id.clone(),
                    wire_protocol_id,
                    sdk_adapter_id,
                    deployment: None,
                    enabled: model.enabled,
                    process_scoped,
                });
            }
            if models == binding.configured_models {
                continue;
            }
            let mut next = binding.clone();
            next.configured_models = models;
            next.revision = binding.revision.saturating_add(1);
            next.updated_at_ms = unix_timestamp_ms().max(binding.updated_at_ms.saturating_add(1));
            AgentModelProviderBindingRepository::update(conn, &next, binding.revision)?;
            changed = true;
        }
        Ok(changed)
    }

    /// Rewrites one binding's configured models from its launch row.
    ///
    /// The binding is what the projection engine reads, so a Model the row
    /// gained is not selectable until the binding names it. Ids are reused by
    /// `provider_model_id`: `projection_overrides.default_model_binding_id`
    /// points at one of them, and renumbering the list would silently unpin the
    /// Agent's default.
    fn rebuild_binding_models(
        conn: &Connection,
        provider: &ModelProviderProfile,
        legacy: &ProviderProfile,
    ) -> VibexResult<bool> {
        let Some(binding) =
            AgentModelProviderBindingRepository::get_by_legacy_profile(conn, &legacy.id)?
        else {
            return Ok(false);
        };
        if binding.model_provider_profile_id.as_ref() != Some(&provider.id) {
            return Ok(false);
        }
        let Some(runtime) = AgentRuntimeProfileRepository::get(conn, &binding.runtime_profile_id)?
        else {
            return Ok(false);
        };
        let interfaces = AgentProviderProjectionRegistry::builtin()?
            .resolve(&runtime.version_identity)?
            .descriptor
            .model_interfaces;
        let mut models: Vec<AgentConfiguredModelBinding> = Vec::new();
        for model in &legacy.configured_models {
            let wire_protocol_id = legacy_wire_protocol(legacy, model.wire_api, &interfaces);
            let sdk_adapter_id = interfaces
                .iter()
                .find(|interface| interface.wire_protocol_id == wire_protocol_id)
                .and_then(|interface| interface.sdk_adapter_id.clone())
                .or_else(|| legacy_sdk_adapter(&legacy.agent_id, &wire_protocol_id));
            let process_scoped = interfaces
                .iter()
                .find(|interface| {
                    interface.wire_protocol_id == wire_protocol_id
                        && interface.sdk_adapter_id == sdk_adapter_id
                })
                .is_some_and(|interface| interface.process_scoped);
            let agent_model_id = legacy_agent_model_id(legacy, &model.id);
            // Two Provider models can collapse onto one Agent model id — the
            // Claude aliases do exactly that — and the binding requires the
            // Agent model ids to be unique. The first one wins.
            if models
                .iter()
                .any(|existing| existing.agent_model_id == agent_model_id)
            {
                continue;
            }
            let id = binding
                .configured_models
                .iter()
                .find(|existing| existing.provider_model_id == model.id)
                .map(|existing| existing.id.clone())
                .unwrap_or_else(AgentConfiguredModelBindingId::new);
            models.push(AgentConfiguredModelBinding {
                id,
                provider_model_id: model.id.clone(),
                agent_model_id,
                wire_protocol_id,
                sdk_adapter_id,
                deployment: None,
                enabled: model.enabled,
                process_scoped,
            });
        }
        if models == binding.configured_models {
            return Ok(false);
        }
        let mut next = binding.clone();
        next.configured_models = models;
        next.revision = binding.revision.saturating_add(1);
        next.updated_at_ms = unix_timestamp_ms().max(binding.updated_at_ms.saturating_add(1));
        AgentModelProviderBindingRepository::update(conn, &next, binding.revision)?;
        Ok(true)
    }

    pub fn mark_legacy_deleted(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
        deleted_at_ms: i64,
    ) -> VibexResult<()> {
        let transaction = Transaction::new_unchecked(conn, TransactionBehavior::Immediate)
            .map_err(storage_err(
                "provider_projection_delete_transaction_failed",
                "failed to start provider projection delete transaction",
            ))?;
        for (table, id_column) in [
            (
                "agent_model_provider_bindings_v2",
                "agent_model_provider_binding_id",
            ),
            ("agent_runtime_profiles", "agent_runtime_profile_id"),
            ("model_provider_profiles", "model_provider_profile_id"),
        ] {
            transaction
                .execute(
                    &format!(
                        "UPDATE {table}
                         SET deleted_at_ms = COALESCE(deleted_at_ms, ?2),
                             updated_at_ms = ?2,
                             revision = revision + 1
                         WHERE legacy_provider_profile_id = ?1"
                    ),
                    params![legacy_id.as_str(), deleted_at_ms],
                )
                .map_err(storage_err(
                    "provider_projection_legacy_delete_failed",
                    "failed to mark legacy projection records deleted",
                ))?;
            let _ = id_column;
        }
        transaction.commit().map_err(storage_err(
            "provider_projection_delete_commit_failed",
            "failed to commit provider projection compatibility delete",
        ))
    }
}

/// The address a legacy Profile should call: the provider-wide one, else the first.
///
/// v2 keeps one endpoint per protocol; v1 keeps one `base_url` plus a
/// per-protocol override that stays in `provider_options`. The endpoint with no
/// protocol is the fallback and is exactly what `base_url` means.
/// The endpoints a legacy Profile's single address implies.
///
/// A legacy Profile has one `base_url` plus, optionally, a per-protocol pin in
/// `provider_options`. The v2 shape needs an endpoint per protocol, so the base
/// address becomes the default endpoint and each pinned protocol gets its own
/// address — which is what a Provider created on the v2 side would have carried.
fn primary_endpoints_from_legacy(legacy: &ProviderProfile) -> Vec<ModelProviderEndpoint> {
    let Some(base_url) = legacy
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    else {
        return Vec::new();
    };
    let pin = provider_option(legacy, LEGACY_WIRE_API_OPTION_KEY)
        .filter(|value| !value.trim().is_empty());
    vec![ModelProviderEndpoint {
        id: "endpoint-api".to_string(),
        kind: ModelProviderEndpointKind::Api,
        url: base_url.to_string(),
        wire_protocol_id: pin,
    }]
}

/// Writes the shared record's website back into the legacy option map.
///
/// `provider_options` also carries the ACP config and the protocol pin, so the
/// map is reused and only this one key moves; clearing it removes the entry.
fn legacy_options_with_website(
    provider: &ModelProviderProfile,
    previous: &ProviderProfile,
) -> ProviderOptions {
    let mut options = previous.provider_options.clone();
    options
        .entries
        .retain(|entry| entry.key.trim() != LEGACY_WEBSITE_URL_OPTION_KEY);
    if let Some(website) = provider
        .metadata
        .website_url
        .as_deref()
        .map(str::trim)
        .filter(|website| !website.is_empty())
    {
        options.entries.push(ProviderBindingMetadata {
            key: LEGACY_WEBSITE_URL_OPTION_KEY.to_string(),
            value: website.to_string(),
        });
    }
    options
}

/// Copies the Provider-owned half of a shared record onto one launch row.
///
/// These are the fields both shapes share: who the Provider is, whether it is
/// on, where it is, which Models it offers and how it authenticates. Everything
/// the launch row owns — `provider_options` beyond the website link, the
/// protocol pin, the ACP config, the import markers, the Agent's own defaults —
/// is left exactly as it is, because it describes how *this* Agent launches.
///
/// `configured_models` lets the caller pass the list the Agent's protocol can
/// actually reach. `None` derives it from the record, which is what the
/// single-row projection has always done.
///
/// The row's timestamp only moves when something actually changed: it is part
/// of the launch revision, and a no-op projection that moved it would restale
/// every session reading the row.
fn apply_shared_provider_to_legacy(
    legacy: &mut ProviderProfile,
    provider: &ModelProviderProfile,
    configured_models: Option<&[ProviderConfiguredModel]>,
) -> bool {
    let before = legacy.clone();
    let legacy_id = before.id.clone();
    // The row this record was derived from is the one row allowed to keep the
    // shared credential ids: `secret_ref_id` is a global primary key, so every
    // other Agent's row has to own a distinct id per Key.
    let secrets = legacy_secrets_from_model_provider(provider, &before, &legacy_id);
    apply_shared_provider_fields(legacy, provider, secrets);
    sync_protocol_pins(legacy, provider);
    legacy.configured_models = match configured_models {
        Some(models) => models.to_vec(),
        None => legacy_configured_models_from_model_provider(provider, &before),
    };
    finish_legacy_row(legacy, &before)
}

/// The launch-row fields a shared Provider owns outright.
///
/// Split from the catalogue because the fan-out derives the Agent's model list
/// from the row this writes: which protocol a Model is called with depends on
/// the address the row ends up with, and the Claude alias rule reads `base_url`.
/// `secrets` is passed in because the id each Key gets is per Agent, not per
/// Provider: see [`legacy_secrets_linked_by_lookup_key`].
fn apply_shared_provider_fields(
    legacy: &mut ProviderProfile,
    provider: &ModelProviderProfile,
    secrets: Vec<vibex_core::ProviderSecretReference>,
) {
    let provider_options = legacy_options_with_website(provider, legacy);
    legacy.display_name = provider.display_name.clone();
    legacy.status = match provider.status {
        ModelProviderProfileStatus::Enabled => ProviderProfileStatus::Enabled,
        ModelProviderProfileStatus::Disabled => ProviderProfileStatus::Disabled,
    };
    legacy.base_url = primary_legacy_base_url(provider);
    legacy.default_model = provider.default_model_id.clone();
    legacy.secrets = secrets;
    // The note and the website live on different fields in each shape; the
    // shared record owns them now, so this direction writes them back where the
    // launch row and the import markers keep reading them.
    legacy.account_alias = provider.metadata.note.clone();
    legacy.provider_options = provider_options;
}

/// The credential rows one Agent's launch row owns for a shared record's Keys.
///
/// `provider_secret_references.secret_ref_id` is a global primary key, so two
/// launch rows can never name the same secret id: the second insert would take
/// the first Agent's Key away. The shared record's credential ids therefore stay
/// with the row the record was derived from, and every other Agent gets an id of
/// its own.
///
/// The link is `lookup_key`, which is the reference to the stored value: two
/// rows sharing it resolve the same Secret, and re-running this keeps the id it
/// gave that Key last time. That stability is what lets `launch_revision` stay
/// put when a Provider write changed nothing.
fn legacy_secrets_linked_by_lookup_key(
    provider: &ModelProviderProfile,
    previous: &ProviderProfile,
) -> Vec<vibex_core::ProviderSecretReference> {
    let now = vibex_core::unix_timestamp_ms().max(1);
    provider
        .credentials
        .iter()
        .filter_map(|credential| {
            let reference = credential.credential.secret_reference()?;
            let existing = previous
                .secrets
                .iter()
                .find(|secret| secret.lookup_key == reference.lookup_key)
                .cloned();
            let secret_kind = match credential.credential {
                AgentCredential::OAuth { .. } => ProviderSecretKind::OAuthAccount,
                _ => ProviderSecretKind::ApiKey,
            };
            let display_label = credential.display_label();
            let unchanged = existing.as_ref().is_some_and(|secret| {
                secret.secret_kind == secret_kind
                    && secret.backend == reference.backend
                    && secret.setup_state == reference.setup_state
                    && secret.display_label == display_label
                    && secret.redacted_hint == reference.redacted_hint
            });
            Some(vibex_core::ProviderSecretReference {
                id: existing
                    .as_ref()
                    .map(|secret| secret.id.clone())
                    .unwrap_or_else(vibex_core::RequestId::new),
                provider_profile_id: previous.id.clone(),
                secret_kind,
                backend: reference.backend,
                setup_state: reference.setup_state,
                lookup_key: reference.lookup_key.clone(),
                display_label,
                redacted_hint: reference.redacted_hint.clone(),
                created_at_ms: existing
                    .as_ref()
                    .map(|secret| secret.created_at_ms.max(1))
                    .unwrap_or(now),
                updated_at_ms: match existing.as_ref() {
                    Some(secret) if unchanged => secret.updated_at_ms,
                    Some(_) => now.max(1),
                    None => now,
                },
            })
        })
        .collect()
}

/// Whether a launch row is the hidden one the gateway publishes.
///
/// Those rows are written from the route catalogue by `attach_gateway_to_agents`,
/// not from the shared record's own Model list: their configured models are the
/// `group/<route>/<model>` ids the listener serves. Projecting a Provider onto
/// them would replace that catalogue with the upstream provider's model ids and
/// leave the Agent pointed at a gateway that does not serve them.
fn is_model_gateway_row(legacy: &ProviderProfile) -> bool {
    legacy
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == "vibex.internal.profileRole")
        .map(|entry| entry.value.trim() == "model_gateway")
        .unwrap_or(false)
}

/// The record's Model list as one Agent can actually call it.
///
/// A shared Provider is one catalogue for every Agent that binds it, and the
/// Agents do not all speak the same protocols: which wire a Model is called with
/// is a property of the Agent's binding, not of the record. So a Model keeps the
/// protocol it already had on this row, a new one inherits the row's own pin,
/// and anything that resolves to a protocol this Agent does not declare is left
/// out — one unreachable Model must not make the whole binding unsupported.
///
/// `interfaces` is empty for an Agent with no registered descriptor. That reads
/// as "unknown, do not narrow": the record's whole list is offered.
fn configured_models_for_agent(
    provider: &ModelProviderProfile,
    previous: &ProviderProfile,
    current: &ProviderProfile,
    interfaces: &[vibex_core::AgentModelInterfaceDescriptor],
) -> Vec<ProviderConfiguredModel> {
    let pinned = provider_option(previous, LEGACY_WIRE_API_OPTION_KEY)
        .and_then(|value| ProviderModelWireApi::from_wire_protocol_id(&value));
    let mut models: Vec<ProviderConfiguredModel> = Vec::new();
    for model in &provider.configured_models {
        let wire_api = previous
            .configured_models
            .iter()
            .find(|known| known.id == model.id)
            .and_then(|known| known.wire_api)
            .or_else(|| model.metadata.iter().find_map(shared_model_wire_api))
            .or(pinned);
        if !interfaces.is_empty() {
            let protocol = legacy_wire_protocol(current, wire_api, interfaces);
            if !interfaces
                .iter()
                .any(|interface| interface.wire_protocol_id == protocol)
            {
                continue;
            }
        }
        models.push(ProviderConfiguredModel {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            enabled: model.enabled,
            wire_api,
            capabilities: model.capabilities.clone(),
        });
    }
    models
}

/// The protocol a shared Model declares for itself, if it declares one.
fn shared_model_wire_api(metadata: &ProviderBindingMetadata) -> Option<ProviderModelWireApi> {
    (metadata.key.trim() == LEGACY_WIRE_API_OPTION_KEY)
        .then(|| ProviderModelWireApi::from_wire_protocol_id(&metadata.value))
        .flatten()
}

/// Keeps a launch row's default Model inside the list it offers.
///
/// The Provider's default can name a Model this Agent cannot call, and the
/// Model can be removed from the record outright. Either way the row must not
/// keep pointing at it: the launch path selects the default by id, so a dangling
/// default silently launches whatever the Agent falls back to.
fn repair_legacy_default_model(legacy: &mut ProviderProfile) {
    let usable = |model: &ProviderConfiguredModel| model.enabled;
    if legacy.default_model.as_deref().is_some_and(|default| {
        legacy
            .configured_models
            .iter()
            .any(|model| model.id == default && usable(model))
    }) {
        return;
    }
    legacy.default_model = legacy
        .configured_models
        .iter()
        .find(|model| usable(model))
        .map(|model| model.id.clone());
}

/// The enabled Model ids a launch row's ACP catalogue should carry.
fn enabled_model_ids(
    models: &[ProviderConfiguredModel],
    default_model: Option<&str>,
) -> Vec<String> {
    let mut ids = Vec::new();
    for model in models.iter().filter(|model| model.enabled) {
        let model = model.id.trim();
        if !model.is_empty() && !ids.iter().any(|existing| existing == model) {
            ids.push(model.to_string());
        }
    }
    if ids.is_empty()
        && let Some(model) = default_model
            .map(str::trim)
            .filter(|model| !model.is_empty())
    {
        ids.push(model.to_string());
    }
    ids
}

/// Rewrites the ACP catalogue a launch row carries inside its options.
///
/// The ACP runtime reads the selectable Models out of the typed config, so a
/// Model added to the Provider is not offered until this list names it. A row
/// with no typed config is left alone: inventing one would claim an ACP launch
/// contract the row never had.
fn write_acp_models(options: &mut ProviderOptions, models: &[String]) -> VibexResult<()> {
    let Some(entry) = options
        .entries
        .iter_mut()
        .find(|entry| entry.key.trim() == LEGACY_ACP_CONFIG_OPTION_KEY)
    else {
        return Ok(());
    };
    let Ok(mut config) = serde_json::from_str::<AcpProviderConfig>(&entry.value) else {
        return Ok(());
    };
    if config.models == models {
        return Ok(());
    }
    config.models = models.to_vec();
    entry.value = serde_json::to_string(&config).map_err(|error| {
        VibexError::storage(
            "provider_projection_acp_config_encode_failed",
            format!("failed to encode the ACP catalogue for a launch row: {error}"),
        )
    })?;
    Ok(())
}

/// Keeps a launch row's per-protocol addresses and pin in step with the record.
///
/// A shared Provider publishes one address per protocol, and the launch path
/// reads them out of `provider_options` under `protocolBaseUrl.<id>` — the pin
/// shadows `base_url`, so an address the record moved is invisible until these
/// entries move with it.
///
/// The pin itself (`wireApi`) is the Agent's own choice of protocol and stays,
/// unless the record no longer publishes it: then the row has to fall back to a
/// protocol both sides speak, exactly the way binding chose one in the first
/// place. When the record publishes a single address with no protocol at all,
/// the pin is left alone — it is the only thing saying what that address speaks.
fn sync_protocol_pins(legacy: &mut ProviderProfile, provider: &ModelProviderProfile) {
    let mut pins: Vec<(String, String)> = Vec::new();
    for endpoint in &provider.endpoints {
        let Some(protocol) = endpoint.wire_protocol_id.as_deref() else {
            continue;
        };
        let Some(wire_api) = ProviderModelWireApi::from_wire_protocol_id(protocol) else {
            continue;
        };
        let key = wire_api.protocol_base_url_option_key();
        if pins.iter().any(|(existing, _)| existing == &key) {
            continue;
        }
        pins.push((key, endpoint.url.clone()));
    }
    if pins.is_empty() {
        return;
    }
    let supported: Vec<String> = pins
        .iter()
        .filter_map(|(key, _)| key.strip_prefix("protocolBaseUrl."))
        .map(ToString::to_string)
        .collect();
    let pinned = provider_option(legacy, LEGACY_WIRE_API_OPTION_KEY);
    let usable = pinned
        .as_deref()
        .and_then(ProviderModelWireApi::from_wire_protocol_id)
        .filter(|wire_api| supported.iter().any(|id| id == wire_api.wire_protocol_id()));
    let chosen = usable.or_else(|| {
        ProviderModelWireApi::ALL.into_iter().find(|wire_api| {
            supported.iter().any(|id| id == wire_api.wire_protocol_id())
                && agent_speaks(legacy, *wire_api)
        })
    });
    legacy.provider_options.entries.retain(|entry| {
        !entry.key.trim().starts_with("protocolBaseUrl.")
            && entry.key.trim() != LEGACY_WIRE_API_OPTION_KEY
    });
    for (key, url) in pins {
        legacy
            .provider_options
            .entries
            .push(ProviderBindingMetadata { key, value: url });
    }
    if let Some(chosen) = chosen {
        legacy
            .provider_options
            .entries
            .push(ProviderBindingMetadata {
                key: LEGACY_WIRE_API_OPTION_KEY.to_string(),
                value: chosen.wire_protocol_id().to_string(),
            });
        if let Some((_, url)) = provider.endpoints.iter().find_map(|endpoint| {
            (endpoint.wire_protocol_id.as_deref() == Some(chosen.wire_protocol_id()))
                .then(|| (String::new(), endpoint.url.clone()))
        }) {
            legacy.base_url = Some(url);
        }
    }
}

/// Whether the Agent this row launches declares the given protocol.
///
/// An Agent with no registered descriptor accepts anything: its interfaces are
/// unknown, so narrowing here would refuse a protocol it may well speak.
fn agent_speaks(legacy: &ProviderProfile, wire_api: ProviderModelWireApi) -> bool {
    let Ok(registry) = AgentProviderProjectionRegistry::builtin() else {
        return true;
    };
    let mut descriptors = registry.descriptors_for_agent(&legacy.agent_id).peekable();
    if descriptors.peek().is_none() {
        return true;
    }
    descriptors
        .flat_map(|descriptor| descriptor.model_interfaces.iter())
        .any(|interface| interface.wire_protocol_id == wire_api.wire_protocol_id())
}

/// Whether the row is different, advancing its timestamp only when it is.
fn finish_legacy_row(legacy: &mut ProviderProfile, before: &ProviderProfile) -> bool {
    legacy.updated_at_ms = before.updated_at_ms;
    if *legacy == *before {
        return false;
    }
    legacy.updated_at_ms =
        vibex_core::unix_timestamp_ms().max(before.updated_at_ms.saturating_add(1));
    true
}

/// Folds one Agent's catalogue into the record's own list.
///
/// The record is authoritative, and each Agent's copy is a filtered view of it,
/// so a copy cannot simply replace the list: that is what shrank a Provider when
/// a Model was edited from an Agent page whose Agent could not speak the other
/// Models' protocols. So the record keeps its Models, in its order, and the copy
/// contributes the two things it alone knows: a Model the record has not seen
/// yet, and each known Model's label, capabilities and visibility — the fields
/// the Agent page edits.
///
/// A Model absent from the copy is therefore kept. Deleting one is the shared
/// page's job, and it writes the whole catalogue through
/// [`ModelProviderProfileRepository::update`] rather than through this merge.
fn merge_model_catalog(
    existing: &[ModelProviderCatalogEntry],
    candidate: &[ModelProviderCatalogEntry],
) -> Vec<ModelProviderCatalogEntry> {
    let mut merged = existing.to_vec();
    for model in candidate {
        match merged.iter_mut().find(|known| known.id == model.id) {
            Some(known) => {
                known.display_name = model.display_name.clone();
                known.enabled = model.enabled;
                known.capabilities = model.capabilities.clone();
            }
            None => merged.push(model.clone()),
        }
    }
    merged
}

fn primary_legacy_base_url(provider: &ModelProviderProfile) -> Option<String> {
    provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.wire_protocol_id.is_none())
        .or_else(|| provider.endpoints.first())
        .map(|endpoint| endpoint.url.trim().to_string())
        .filter(|url| !url.is_empty())
}

/// Rebuilds the legacy Model list, keeping each Model's declared protocol.
///
/// A v2 catalogue entry does not carry a wire protocol: the protocol belongs to
/// the Agent binding that selects the Model, not to the shared Provider. The
/// legacy Profile is where the declaration lives, so a Model the two shapes
/// share keeps the protocol it already had, and one v2 newly offers inherits
/// the Provider's pinned protocol.
fn legacy_configured_models_from_model_provider(
    provider: &ModelProviderProfile,
    previous: &ProviderProfile,
) -> Vec<ProviderConfiguredModel> {
    let pinned = provider_option(previous, LEGACY_WIRE_API_OPTION_KEY)
        .and_then(|value| ProviderModelWireApi::from_wire_protocol_id(&value));
    provider
        .configured_models
        .iter()
        .map(|model| ProviderConfiguredModel {
            id: model.id.clone(),
            display_name: model.display_name.clone(),
            enabled: model.enabled,
            wire_api: previous
                .configured_models
                .iter()
                .find(|known| known.id == model.id)
                .and_then(|known| known.wire_api)
                .or(pinned),
            capabilities: model.capabilities.clone(),
        })
        .collect()
}

/// Rebuilds the legacy secret rows from the v2 credential list.
///
/// The legacy table is what the launch path resolves a Key from, so a v2 Key
/// that never reached it is invisible to the Agent. Ids are preserved, which is
/// what lets a binding's `auth_source_revision` keep pointing at the same
/// secret after a projection.
fn legacy_secrets_from_model_provider(
    provider: &ModelProviderProfile,
    previous: &ProviderProfile,
    legacy_id: &ProviderProfileId,
) -> Vec<vibex_core::ProviderSecretReference> {
    provider
        .credentials
        .iter()
        .filter_map(|credential| {
            let reference = credential.credential.secret_reference()?;
            let existing = previous
                .secrets
                .iter()
                .find(|secret| secret.id == reference.id);
            Some(vibex_core::ProviderSecretReference {
                id: reference.id.clone(),
                provider_profile_id: legacy_id.clone(),
                secret_kind: match credential.credential {
                    AgentCredential::OAuth { .. } => ProviderSecretKind::OAuthAccount,
                    _ => ProviderSecretKind::ApiKey,
                },
                backend: reference.backend,
                setup_state: reference.setup_state,
                lookup_key: reference.lookup_key.clone(),
                display_label: credential.display_label(),
                redacted_hint: reference.redacted_hint.clone(),
                created_at_ms: existing
                    .map(|secret| secret.created_at_ms.max(1))
                    .unwrap_or_else(|| vibex_core::unix_timestamp_ms().max(1)),
                updated_at_ms: vibex_core::unix_timestamp_ms().max(1),
            })
        })
        .collect()
}

fn insert_model_provider_profile(
    conn: &Connection,
    profile: &ModelProviderProfile,
) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO model_provider_profiles (
            model_provider_profile_id, legacy_provider_profile_id,
            display_name, vendor_hint, endpoints_json, proxy_policy_json,
            credentials_json, configured_models_json, default_model_id,
            headers_json, status, revision, created_at_ms, updated_at_ms,
            deleted_at_ms, metadata_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16)
        ",
        params![
            profile.id.as_str(),
            profile
                .legacy_provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            profile.display_name,
            profile.vendor_hint,
            json_to_db(&profile.endpoints)?,
            json_to_db(&profile.proxy_policy)?,
            json_to_db(&profile.credentials)?,
            json_to_db(&profile.configured_models)?,
            profile.default_model_id,
            json_to_db(&profile.headers)?,
            enum_to_db(&profile.status)?,
            profile.revision,
            profile.created_at_ms,
            profile.updated_at_ms,
            profile.deleted_at_ms,
            json_to_db(&profile.metadata)?,
        ],
    )
    .map_err(storage_err(
        "model_provider_profile_insert_failed",
        "failed to insert model provider profile",
    ))?;
    Ok(())
}

fn insert_agent_runtime_profile(
    conn: &Connection,
    profile: &AgentRuntimeProfile,
) -> VibexResult<()> {
    conn.execute(
        "
        INSERT INTO agent_runtime_profiles (
            agent_runtime_profile_id, legacy_provider_profile_id, agent_id,
            adapter_id, version_identity_json, command, args_json,
            safe_env_references_json, cwd_template, process_strategy,
            runtime_home_strategy, host_capabilities_json, resource_policy_json,
            models_json, modes_json, features_json, disabled_tools_json,
            revision, created_at_ms, updated_at_ms, deleted_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15,
                  ?16, ?17, ?18, ?19, ?20, ?21)
        ",
        params![
            profile.id.as_str(),
            profile
                .legacy_provider_profile_id
                .as_ref()
                .map(ProviderProfileId::as_str),
            profile.version_identity.route.agent_id.as_str(),
            profile.version_identity.route.adapter_id.as_str(),
            json_to_db(&profile.version_identity)?,
            profile.command,
            json_to_db(&profile.args)?,
            json_to_db(&profile.safe_env_references)?,
            profile.cwd_template,
            enum_to_db(&profile.process_strategy)?,
            enum_to_db(&profile.runtime_home_strategy)?,
            json_to_db(&profile.host_capabilities)?,
            json_to_db(&profile.resource_policy)?,
            json_to_db(&profile.models)?,
            json_to_db(&profile.modes)?,
            json_to_db(&profile.features)?,
            json_to_db(&profile.disabled_tools)?,
            profile.revision,
            profile.created_at_ms,
            profile.updated_at_ms,
            profile.deleted_at_ms,
        ],
    )
    .map_err(storage_err(
        "agent_runtime_profile_insert_failed",
        "failed to insert Agent runtime profile",
    ))?;
    Ok(())
}

fn insert_agent_model_provider_binding(
    transaction: &Transaction<'_>,
    binding: &AgentModelProviderBinding,
) -> VibexResult<()> {
    transaction
        .execute(
            "
            INSERT INTO agent_model_provider_bindings_v2 (
                agent_model_provider_binding_id, legacy_provider_profile_id,
                agent_id, agent_runtime_profile_id, model_provider_profile_id,
                projection_descriptor_id, projection_overrides_json,
                projection_fingerprint, status, verification_json, revision,
                created_at_ms, updated_at_ms, deleted_at_ms,
                account_alias, reasoning_effort, provider_options_json
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                      ?15, ?16, ?17)
            ",
            params![
                binding.id.as_str(),
                binding
                    .legacy_provider_profile_id
                    .as_ref()
                    .map(ProviderProfileId::as_str),
                binding.agent_id.as_str(),
                binding.runtime_profile_id.as_str(),
                binding
                    .model_provider_profile_id
                    .as_ref()
                    .map(ModelProviderProfileId::as_str),
                binding.projection_descriptor_id.as_str(),
                json_to_db(&binding.projection_overrides)?,
                binding.projection_fingerprint,
                enum_to_db(&binding.status)?,
                json_to_db(&binding.verification)?,
                binding.revision,
                binding.created_at_ms,
                binding.updated_at_ms,
                binding.deleted_at_ms,
                binding.account_alias,
                binding.reasoning_effort,
                json_to_db(&binding.provider_options)?,
            ],
        )
        .map_err(storage_err(
            "agent_model_provider_binding_insert_failed",
            "failed to insert Agent provider binding",
        ))?;
    replace_configured_model_bindings(transaction, binding)
}

fn replace_configured_model_bindings(
    transaction: &Transaction<'_>,
    binding: &AgentModelProviderBinding,
) -> VibexResult<()> {
    transaction
        .execute(
            "DELETE FROM agent_configured_model_bindings
             WHERE agent_model_provider_binding_id = ?1",
            params![binding.id.as_str()],
        )
        .map_err(storage_err(
            "agent_configured_model_binding_replace_failed",
            "failed to replace Agent configured model bindings",
        ))?;
    for (order_index, model) in binding.configured_models.iter().enumerate() {
        transaction
            .execute(
                "
                INSERT INTO agent_configured_model_bindings (
                    agent_configured_model_binding_id,
                    agent_model_provider_binding_id, provider_model_id,
                    agent_model_id, wire_protocol_id, sdk_adapter_id,
                    deployment, enabled, process_scoped, order_index
                ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)
                ",
                params![
                    model.id.as_str(),
                    binding.id.as_str(),
                    model.provider_model_id,
                    model.agent_model_id,
                    model.wire_protocol_id,
                    model.sdk_adapter_id,
                    model.deployment,
                    bool_to_db(model.enabled),
                    bool_to_db(model.process_scoped),
                    order_index as i64,
                ],
            )
            .map_err(storage_err(
                "agent_configured_model_binding_insert_failed",
                "failed to insert Agent configured model binding",
            ))?;
    }
    Ok(())
}

fn map_model_provider_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<ModelProviderProfile> {
    Ok(ModelProviderProfile {
        id: parse_id_sql(row.get(0)?, ModelProviderProfileId::parse)?,
        legacy_provider_profile_id: row
            .get::<_, Option<String>>(1)?
            .map(ProviderProfileId::parse)
            .transpose()
            .map_err(to_sql_decode_error)?,
        display_name: row.get(2)?,
        vendor_hint: row.get(3)?,
        endpoints: json_from_db_sql(row.get(4)?)?,
        proxy_policy: json_from_db_sql(row.get(5)?)?,
        credentials: json_from_db_sql(row.get(6)?)?,
        configured_models: json_from_db_sql(row.get(7)?)?,
        default_model_id: row.get(8)?,
        headers: json_from_db_sql(row.get(9)?)?,
        status: enum_from_db_sql(row.get(10)?)?,
        revision: row.get(11)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
        deleted_at_ms: row.get(14)?,
        metadata: json_from_db_sql(row.get(15)?)?,
    })
}

fn map_agent_runtime_profile(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentRuntimeProfile> {
    Ok(AgentRuntimeProfile {
        id: parse_id_sql(row.get(0)?, AgentRuntimeProfileId::parse)?,
        legacy_provider_profile_id: row
            .get::<_, Option<String>>(1)?
            .map(ProviderProfileId::parse)
            .transpose()
            .map_err(to_sql_decode_error)?,
        version_identity: json_from_db_sql(row.get(2)?)?,
        command: row.get(3)?,
        args: json_from_db_sql(row.get(4)?)?,
        safe_env_references: json_from_db_sql(row.get(5)?)?,
        cwd_template: row.get(6)?,
        process_strategy: enum_from_db_sql(row.get(7)?)?,
        runtime_home_strategy: enum_from_db_sql(row.get(8)?)?,
        host_capabilities: json_from_db_sql(row.get(9)?)?,
        resource_policy: json_from_db_sql(row.get(10)?)?,
        models: json_from_db_sql(row.get(11)?)?,
        modes: json_from_db_sql(row.get(12)?)?,
        features: json_from_db_sql(row.get(13)?)?,
        disabled_tools: json_from_db_sql(row.get(14)?)?,
        revision: row.get(15)?,
        created_at_ms: row.get(16)?,
        updated_at_ms: row.get(17)?,
        deleted_at_ms: row.get(18)?,
    })
}

fn map_agent_model_provider_binding_without_models(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<AgentModelProviderBinding> {
    Ok(AgentModelProviderBinding {
        id: parse_id_sql(row.get(0)?, AgentModelProviderBindingId::parse)?,
        legacy_provider_profile_id: row
            .get::<_, Option<String>>(1)?
            .map(ProviderProfileId::parse)
            .transpose()
            .map_err(to_sql_decode_error)?,
        agent_id: AgentId::parse(row.get::<_, String>(2)?).map_err(to_sql_decode_error)?,
        runtime_profile_id: parse_id_sql(row.get(3)?, AgentRuntimeProfileId::parse)?,
        model_provider_profile_id: row
            .get::<_, Option<String>>(4)?
            .map(ModelProviderProfileId::parse)
            .transpose()
            .map_err(to_sql_decode_error)?,
        projection_descriptor_id: parse_id_sql(
            row.get(5)?,
            AgentProviderProjectionDescriptorId::parse,
        )?,
        projection_overrides: json_from_db_sql(row.get(6)?)?,
        projection_fingerprint: row.get(7)?,
        status: enum_from_db_sql(row.get(8)?)?,
        verification: json_from_db_sql(row.get(9)?)?,
        configured_models: Vec::new(),
        revision: row.get(10)?,
        created_at_ms: row.get(11)?,
        updated_at_ms: row.get(12)?,
        deleted_at_ms: row.get(13)?,
        account_alias: row.get(14)?,
        reasoning_effort: row.get(15)?,
        provider_options: json_from_db_sql(row.get(16)?)?,
    })
}

fn list_configured_model_bindings(
    conn: &Connection,
    binding_id: &AgentModelProviderBindingId,
) -> VibexResult<Vec<AgentConfiguredModelBinding>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT agent_configured_model_binding_id, provider_model_id,
                agent_model_id, wire_protocol_id, sdk_adapter_id, deployment,
                enabled, process_scoped
            FROM agent_configured_model_bindings
            WHERE agent_model_provider_binding_id = ?1
            ORDER BY order_index ASC, agent_configured_model_binding_id ASC
            ",
        )
        .map_err(storage_err(
            "agent_configured_model_binding_list_failed",
            "failed to list Agent configured model bindings",
        ))?;
    collect_rows(
        stmt.query_map(params![binding_id.as_str()], |row| {
            Ok(AgentConfiguredModelBinding {
                id: parse_id_sql(row.get(0)?, AgentConfiguredModelBindingId::parse)?,
                provider_model_id: row.get(1)?,
                agent_model_id: row.get(2)?,
                wire_protocol_id: row.get(3)?,
                sdk_adapter_id: row.get(4)?,
                deployment: row.get(5)?,
                enabled: db_to_bool(row.get(6)?),
                process_scoped: db_to_bool(row.get(7)?),
            })
        }),
        "agent_configured_model_binding_list_failed",
        "failed to list Agent configured model bindings",
    )
}

fn list_bindings_where(
    conn: &Connection,
    predicate: &str,
    value: &str,
    error_code: &'static str,
) -> VibexResult<Vec<AgentModelProviderBinding>> {
    list_bindings(conn, predicate, params![value], error_code)
}

/// The one binding query every lookup shares.
///
/// `bind` is the prepared parameter list, so a lookup that takes an id and the
/// unfiltered read of the whole table run the same SQL and the same row
/// mapping; only the predicate and its parameters differ.
fn list_bindings<P: rusqlite::Params>(
    conn: &Connection,
    predicate: &str,
    bind: P,
    error_code: &'static str,
) -> VibexResult<Vec<AgentModelProviderBinding>> {
    let sql = format!(
        "
        SELECT agent_model_provider_binding_id, legacy_provider_profile_id,
            agent_id, agent_runtime_profile_id, model_provider_profile_id,
            projection_descriptor_id, projection_overrides_json,
            projection_fingerprint, status, verification_json, revision,
            created_at_ms, updated_at_ms, deleted_at_ms,
            account_alias, reasoning_effort, provider_options_json
        FROM agent_model_provider_bindings_v2
        WHERE {predicate} AND deleted_at_ms IS NULL
        ORDER BY updated_at_ms DESC, agent_model_provider_binding_id ASC
        "
    );
    let mut stmt = conn.prepare(&sql).map_err(storage_err(
        error_code,
        "failed to prepare Agent provider binding list",
    ))?;
    let mut bindings = collect_rows(
        stmt.query_map(bind, map_agent_model_provider_binding_without_models),
        error_code,
        "failed to list Agent provider bindings",
    )?;
    for binding in &mut bindings {
        binding.configured_models = list_configured_model_bindings(conn, &binding.id)?;
    }
    Ok(bindings)
}

fn list_legacy_provider_profiles(conn: &Connection) -> VibexResult<Vec<ProviderProfile>> {
    let mut stmt = conn
        .prepare(
            "
            SELECT provider_profile_id, agent_id, provider_kind, display_name, status,
                account_alias, base_url, default_model, small_model,
                large_model, configured_models_json, reasoning_effort,
                sandbox_defaults_json, network_defaults_json,
                permission_defaults_json, provider_options_json, created_at_ms,
                updated_at_ms, deleted_at_ms
            FROM provider_profiles
            WHERE deleted_at_ms IS NULL
                AND provider_kind IN ('claude', 'codex', 'acp')
            ORDER BY provider_profile_id ASC
            ",
        )
        .map_err(storage_err(
            "provider_projection_legacy_list_failed",
            "failed to list legacy provider profiles for projection backfill",
        ))?;
    let rows = stmt
        .query_map([], map_provider_profile_without_secrets)
        .map_err(storage_err(
            "provider_projection_legacy_list_failed",
            "failed to list legacy provider profiles for projection backfill",
        ))?;
    let mut profiles = Vec::new();
    for row in rows {
        let mut profile = row.map_err(storage_err(
            "provider_projection_legacy_decode_failed",
            "failed to decode legacy provider profile for projection backfill",
        ))?;
        profile.secrets = ProviderSecretReferenceRepository::list_for_profile(conn, &profile.id)?;
        profiles.push(profile);
    }
    Ok(profiles)
}

fn map_legacy_projection_records(
    conn: &Connection,
    legacy: &ProviderProfile,
) -> VibexResult<LegacyProviderProjectionRecords> {
    let now = legacy.updated_at_ms.max(legacy.created_at_ms).max(1);
    let model_provider_id = legacy_model_provider_id(&legacy.id)?;
    let runtime_profile_id = legacy_runtime_profile_id(&legacy.id)?;
    let binding_id = legacy_binding_id(&legacy.id)?;
    let credentials = legacy_credentials(legacy);
    let model_provider = ModelProviderProfile {
        id: model_provider_id.clone(),
        legacy_provider_profile_id: Some(legacy.id.clone()),
        display_name: legacy.display_name.clone(),
        // The hint names the vendor, so it comes from the vendor marker — not
        // from the account alias, which is the user's own note and used to
        // arrive here as a fake brand.
        vendor_hint: provider_option(legacy, "codexModelProviderId")
            .or_else(|| Some(legacy.kind.to_string())),
        endpoints: legacy_model_provider_endpoints(legacy),
        proxy_policy: ModelProviderProxyPolicy::InheritSystem,
        headers: legacy_headers(legacy),
        credentials,
        configured_models: legacy_model_catalog(legacy),
        default_model_id: legacy.default_model.clone(),
        metadata: ModelProviderMetadata {
            note: legacy.account_alias.clone(),
            website_url: provider_option(legacy, LEGACY_WEBSITE_URL_OPTION_KEY),
            ..Default::default()
        },
        status: if legacy.status == ProviderProfileStatus::Enabled {
            ModelProviderProfileStatus::Enabled
        } else {
            ModelProviderProfileStatus::Disabled
        },
        revision: 1,
        created_at_ms: legacy.created_at_ms.max(1),
        updated_at_ms: now,
        deleted_at_ms: legacy.deleted_at_ms,
    };

    let acp_config = legacy_acp_config(legacy).or_else(|| builtin_acp_config(&legacy.agent_id));
    let (
        command,
        args,
        safe_env_references,
        cwd_template,
        process_strategy,
        terminal,
        terminal_auth,
        acp_models,
        acp_modes,
        acp_features,
        acp_disabled_tools,
    ) = acp_config.map_or_else(
        || {
            (
                legacy.agent_id.as_str().to_string(),
                Vec::new(),
                Vec::new(),
                None,
                AcpProcessStrategy::PerSession,
                false,
                false,
                Vec::new(),
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )
        },
        |config| {
            (
                config.command,
                config.args,
                config.env,
                config.cwd_template,
                config.process_strategy,
                config.terminal_tools,
                config.terminal_auth,
                config.models,
                config.modes,
                config.features,
                config.disabled_tools,
            )
        },
    );
    let version_identity = legacy_runtime_identity(conn, legacy, &command, &args)?;
    let resolution = AgentProviderProjectionRegistry::builtin()?.resolve(&version_identity)?;
    let agent_runtime = AgentRuntimeProfile {
        id: runtime_profile_id.clone(),
        legacy_provider_profile_id: Some(legacy.id.clone()),
        version_identity,
        command,
        args,
        safe_env_references,
        cwd_template,
        process_strategy,
        runtime_home_strategy: resolution.descriptor.runtime_home_strategy.clone(),
        host_capabilities: AgentHostCapabilities {
            filesystem: true,
            terminal,
            terminal_auth,
            mcp: true,
            session_config: true,
        },
        resource_policy: AgentRuntimeResourcePolicy {
            sandbox: legacy.sandbox_defaults.clone(),
            network: legacy.network_defaults.clone(),
            permissions: legacy.permission_defaults.clone(),
        },
        // The ACP catalogue the legacy row keeps inside its serialised config.
        // Carrying it across is what lets a launch read this row instead of the
        // legacy one.
        models: acp_models,
        modes: acp_modes,
        features: acp_features,
        disabled_tools: acp_disabled_tools,
        revision: 1,
        created_at_ms: legacy.created_at_ms.max(1),
        updated_at_ms: now,
        deleted_at_ms: legacy.deleted_at_ms,
    };
    let configured_models = legacy_configured_model_bindings(
        legacy,
        &binding_id,
        &resolution.descriptor.model_interfaces,
    )?;
    let verification = ProjectionVerificationState {
        state: resolution.descriptor.evidence.state,
        descriptor_version: resolution.descriptor.descriptor_version.clone(),
        source_evidence_reference: resolution.descriptor.evidence.source_reference.clone(),
        runtime_evidence_reference: resolution.descriptor.evidence.runtime_reference.clone(),
        verified_at_ms: None,
    };
    let mut binding = AgentModelProviderBinding {
        id: binding_id,
        legacy_provider_profile_id: Some(legacy.id.clone()),
        agent_id: legacy.agent_id.clone(),
        runtime_profile_id,
        model_provider_profile_id: Some(model_provider_id),
        projection_descriptor_id: resolution.descriptor.id.clone(),
        projection_overrides: AgentProviderProjectionOverrides::default(),
        configured_models,
        // Carried across unchanged: these describe how this Agent calls the
        // Provider, so the compatibility layer's job is to preserve them, not to
        // interpret them.
        account_alias: legacy.account_alias.clone(),
        reasoning_effort: legacy.reasoning_effort.clone(),
        provider_options: legacy.provider_options.clone(),
        projection_fingerprint: None,
        status: match (resolution.match_kind, resolution.descriptor.evidence.state) {
            (ProjectionDescriptorMatch::Conservative, _) => {
                AgentModelProviderBindingStatus::Unverified
            }
            (_, ProjectionEvidenceState::Verified) => AgentModelProviderBindingStatus::Ready,
            (_, ProjectionEvidenceState::Unsupported) => {
                AgentModelProviderBindingStatus::Unsupported
            }
            _ => AgentModelProviderBindingStatus::Unverified,
        },
        verification,
        revision: 1,
        created_at_ms: legacy.created_at_ms.max(1),
        updated_at_ms: now,
        deleted_at_ms: legacy.deleted_at_ms,
    };
    if resolution.match_kind != ProjectionDescriptorMatch::Conservative
        && binding
            .validate_against_descriptor(&resolution.descriptor)
            .is_err()
    {
        binding.status = AgentModelProviderBindingStatus::Unsupported;
        binding.verification.state = ProjectionEvidenceState::Stale;
    }
    Ok(LegacyProviderProjectionRecords {
        model_provider,
        agent_runtime,
        binding,
    })
}

fn legacy_runtime_identity(
    conn: &Connection,
    profile: &ProviderProfile,
    command: &str,
    args: &[String],
) -> VibexResult<AgentRuntimeVersionIdentity> {
    let agent = profile.agent_id.as_str();
    let managed_version = AgentManagedInstallationRepository::get(conn, &profile.agent_id)?
        .filter(|record| {
            record.command.as_ref().is_some_and(|managed| {
                managed.command == command && managed.args.as_slice() == args
            })
        })
        .and_then(|record| record.state.installed_version);
    let pinned_catalog_version = pinned_catalog_command_version(agent, command, args);
    let (adapter, adapter_version, agent_version, runtime_dependencies, source) =
        match (agent, managed_version.or(pinned_catalog_version)) {
            ("claude", Some(version)) => (
                "claude-agent-acp",
                Some(version),
                None,
                BTreeMap::new(),
                AgentVersionSource::Managed,
            ),
            ("codex", Some(version)) => (
                "codex-acp",
                Some(version),
                None,
                BTreeMap::new(),
                AgentVersionSource::Managed,
            ),
            ("zcode", Some(version)) => (
                "zcode-acp-server",
                Some(version),
                None,
                BTreeMap::new(),
                AgentVersionSource::Managed,
            ),
            ("claude", None)
                if looks_managed_adapter_command(command, args, "claude-agent-acp") =>
            {
                (
                    "claude-agent-acp",
                    Some("0.101.0".to_string()),
                    None,
                    BTreeMap::new(),
                    AgentVersionSource::Managed,
                )
            }
            ("codex", None) if looks_managed_adapter_command(command, args, "codex-acp") => (
                "codex-acp",
                Some("2.5.0".to_string()),
                Some("0.165.0".to_string()),
                BTreeMap::from([("@openai/codex".to_string(), "0.165.0".to_string())]),
                AgentVersionSource::Managed,
            ),
            ("zcode", None) if looks_managed_adapter_command(command, args, "zcode-acp-server") => {
                (
                    "zcode-acp-server",
                    Some("0.82.3".to_string()),
                    None,
                    BTreeMap::new(),
                    AgentVersionSource::Managed,
                )
            }
            ("opencode", _) if looks_native_agent_command(command, "opencode") => {
                let detected = latest_agent_version(conn, &profile.agent_id)?;
                let source = if detected.is_some() {
                    AgentVersionSource::Detected
                } else {
                    AgentVersionSource::Unknown
                };
                ("opencode-acp", None, detected, BTreeMap::new(), source)
            }
            (_, Some(version)) => (
                agent,
                None,
                Some(version),
                BTreeMap::new(),
                AgentVersionSource::Managed,
            ),
            _ => {
                let detected = latest_agent_version(conn, &profile.agent_id)?;
                let source = if detected.is_some() {
                    AgentVersionSource::Detected
                } else if command.trim().is_empty() {
                    AgentVersionSource::Unknown
                } else {
                    AgentVersionSource::Manual
                };
                (agent, None, detected, BTreeMap::new(), source)
            }
        };
    Ok(AgentRuntimeVersionIdentity {
        route: AgentRuntimeRouteKey {
            agent_id: profile.agent_id.clone(),
            transport_kind: TransportKind::Acp,
            adapter_id: if adapter == agent {
                default_acp_adapter_id(&profile.agent_id)
            } else {
                AcpAdapterId::parse(adapter)?
            },
        },
        adapter_version,
        agent_version,
        runtime_dependencies,
        source,
    })
}

fn pinned_catalog_command_version(
    agent_id: &str,
    command: &str,
    args: &[String],
) -> Option<String> {
    let entry = acp_agent_catalog_entries()
        .iter()
        .find(|entry| entry.id == agent_id)?;
    if entry.version.eq_ignore_ascii_case("manual")
        || !matches!(entry.command.first().copied(), Some("npx" | "uvx"))
        || entry.command.first().copied() != Some(command)
        || entry.command.get(1..).unwrap_or_default() != args
    {
        return None;
    }
    Some(entry.version.to_string())
}

fn upsert_legacy_model_provider(
    conn: &Connection,
    mut candidate: ModelProviderProfile,
    explicit: bool,
) -> VibexResult<ModelProviderProfile> {
    let Some(legacy_id) = candidate.legacy_provider_profile_id.as_ref() else {
        return Err(VibexError::validation(
            "provider_projection_legacy_identity_missing",
            "legacy model provider backfill requires a legacy profile id",
        ));
    };
    let Some(existing) = ModelProviderProfileRepository::get_by_legacy_profile(conn, legacy_id)?
    else {
        insert_model_provider_profile(conn, &candidate)?;
        return Ok(candidate);
    };
    // After the cutover the shared record is authoritative: an existing one is not
    // overwritten from the legacy row it projected from. The binding and the runtime
    // below still repair, because those are derived data the shared record owns —
    // skipping them would leave a stale protocol in place, which is what three tests
    // check.
    if !explicit && crate::migration_applied(conn, 73)? {
        return Ok(existing);
    }
    candidate.id = existing.id.clone();
    candidate.created_at_ms = existing.created_at_ms;
    candidate.revision = existing.revision;
    if same_model_provider_content(&existing, &candidate) {
        return Ok(existing);
    }
    candidate.revision = existing.revision.saturating_add(1);
    candidate.updated_at_ms = candidate
        .updated_at_ms
        .max(existing.updated_at_ms.saturating_add(1));
    ModelProviderProfileRepository::update(conn, &candidate, existing.revision)
}

fn upsert_legacy_agent_runtime(
    conn: &Connection,
    mut candidate: AgentRuntimeProfile,
) -> VibexResult<AgentRuntimeProfile> {
    let Some(legacy_id) = candidate.legacy_provider_profile_id.as_ref() else {
        return Err(VibexError::validation(
            "provider_projection_legacy_identity_missing",
            "legacy Agent runtime backfill requires a legacy profile id",
        ));
    };
    let Some(existing) = AgentRuntimeProfileRepository::get_by_legacy_profile(conn, legacy_id)?
    else {
        insert_agent_runtime_profile(conn, &candidate)?;
        return Ok(candidate);
    };
    candidate.id = existing.id.clone();
    candidate.created_at_ms = existing.created_at_ms;
    candidate.revision = existing.revision;
    if same_agent_runtime_content(&existing, &candidate) {
        return Ok(existing);
    }
    candidate.revision = existing.revision.saturating_add(1);
    candidate.updated_at_ms = candidate
        .updated_at_ms
        .max(existing.updated_at_ms.saturating_add(1));
    AgentRuntimeProfileRepository::update(conn, &candidate, existing.revision)
}

fn upsert_legacy_binding(
    conn: &Transaction<'_>,
    mut candidate: AgentModelProviderBinding,
) -> VibexResult<AgentModelProviderBinding> {
    let Some(legacy_id) = candidate.legacy_provider_profile_id.as_ref() else {
        return Err(VibexError::validation(
            "provider_projection_legacy_identity_missing",
            "legacy Agent provider binding backfill requires a legacy profile id",
        ));
    };
    let Some(existing) =
        AgentModelProviderBindingRepository::get_by_legacy_profile(conn, legacy_id)?
    else {
        insert_agent_model_provider_binding(conn, &candidate)?;
        return Ok(candidate);
    };
    candidate.id = existing.id.clone();
    candidate.created_at_ms = existing.created_at_ms;
    candidate.revision = existing.revision;
    // A content mirror must not clear a fingerprint computed by the projection
    // engine. The service updates it after comparing the effective plan.
    candidate.projection_fingerprint = existing.projection_fingerprint.clone();
    if existing.projection_fingerprint.is_some() {
        candidate.status = existing.status;
        candidate.verification = existing.verification.clone();
    }
    if same_binding_content(&existing, &candidate) {
        return Ok(existing);
    }
    candidate.revision = existing.revision.saturating_add(1);
    candidate.updated_at_ms = candidate
        .updated_at_ms
        .max(existing.updated_at_ms.saturating_add(1));
    update_binding_in_transaction(conn, &candidate, existing.revision)
}

fn update_binding_in_transaction(
    transaction: &Transaction<'_>,
    binding: &AgentModelProviderBinding,
    expected_revision: i64,
) -> VibexResult<AgentModelProviderBinding> {
    let changed = transaction
        .execute(
            "
            UPDATE agent_model_provider_bindings_v2
            SET agent_id = ?3,
                agent_runtime_profile_id = ?4,
                model_provider_profile_id = ?5,
                projection_descriptor_id = ?6,
                projection_overrides_json = ?7,
                projection_fingerprint = ?8,
                status = ?9,
                verification_json = ?10,
                revision = ?11,
                updated_at_ms = ?12,
                deleted_at_ms = ?13
            WHERE agent_model_provider_binding_id = ?1
                AND revision = ?2
                AND deleted_at_ms IS NULL
            ",
            params![
                binding.id.as_str(),
                expected_revision,
                binding.agent_id.as_str(),
                binding.runtime_profile_id.as_str(),
                binding
                    .model_provider_profile_id
                    .as_ref()
                    .map(ModelProviderProfileId::as_str),
                binding.projection_descriptor_id.as_str(),
                json_to_db(&binding.projection_overrides)?,
                binding.projection_fingerprint,
                enum_to_db(&binding.status)?,
                json_to_db(&binding.verification)?,
                binding.revision,
                binding.updated_at_ms,
                binding.deleted_at_ms,
            ],
        )
        .map_err(storage_err(
            "agent_model_provider_binding_update_failed",
            "failed to update Agent provider binding",
        ))?;
    require_revision_update(
        changed,
        "agent_model_provider_binding_revision_conflict",
        "Agent provider binding changed since it was loaded",
    )?;
    replace_configured_model_bindings(transaction, binding)?;
    AgentModelProviderBindingRepository::get(transaction, &binding.id)?.ok_or_else(|| {
        VibexError::storage(
            "agent_model_provider_binding_update_readback_failed",
            "failed to read Agent provider binding after update",
        )
    })
}

fn same_model_provider_content(left: &ModelProviderProfile, right: &ModelProviderProfile) -> bool {
    left.legacy_provider_profile_id == right.legacy_provider_profile_id
        && left.display_name == right.display_name
        && left.vendor_hint == right.vendor_hint
        && left.endpoints == right.endpoints
        && left.proxy_policy == right.proxy_policy
        && left.credentials == right.credentials
        && left.configured_models == right.configured_models
        && left.default_model_id == right.default_model_id
        && left.headers == right.headers
        && left.metadata.note == right.metadata.note
        && left.metadata.website_url == right.metadata.website_url
        && left.status == right.status
        && left.deleted_at_ms == right.deleted_at_ms
}

fn same_agent_runtime_content(left: &AgentRuntimeProfile, right: &AgentRuntimeProfile) -> bool {
    left.legacy_provider_profile_id == right.legacy_provider_profile_id
        && left.version_identity == right.version_identity
        && left.command == right.command
        && left.args == right.args
        && left.safe_env_references == right.safe_env_references
        && left.cwd_template == right.cwd_template
        && left.process_strategy == right.process_strategy
        && left.runtime_home_strategy == right.runtime_home_strategy
        && left.host_capabilities == right.host_capabilities
        && left.resource_policy == right.resource_policy
        // The ACP catalogue is part of the launch contract: a Profile that gains or
        // loses a model must be written, or the runtime keeps launching with the
        // catalogue it saw first — which is exactly what a stale `models` list did.
        && left.models == right.models
        && left.modes == right.modes
        && left.features == right.features
        && left.disabled_tools == right.disabled_tools
        && left.deleted_at_ms == right.deleted_at_ms
}

fn same_binding_content(
    left: &AgentModelProviderBinding,
    right: &AgentModelProviderBinding,
) -> bool {
    left.legacy_provider_profile_id == right.legacy_provider_profile_id
        && left.agent_id == right.agent_id
        && left.runtime_profile_id == right.runtime_profile_id
        && left.model_provider_profile_id == right.model_provider_profile_id
        && left.projection_descriptor_id == right.projection_descriptor_id
        && left.projection_overrides == right.projection_overrides
        && left.configured_models == right.configured_models
        && left.projection_fingerprint == right.projection_fingerprint
        && left.status == right.status
        && left.verification == right.verification
        && left.deleted_at_ms == right.deleted_at_ms
}

fn legacy_credentials(profile: &ProviderProfile) -> Vec<ModelProviderCredentialReference> {
    profile
        .secrets
        .iter()
        .map(|secret| {
            let status = match secret.setup_state {
                ProviderSecretSetupState::Missing => AgentCredentialStatus::Missing,
                ProviderSecretSetupState::Referenced => AgentCredentialStatus::Referenced,
                ProviderSecretSetupState::Available => AgentCredentialStatus::Ready,
                ProviderSecretSetupState::Unsupported => AgentCredentialStatus::Unsupported,
            };
            let credential = match secret.secret_kind {
                ProviderSecretKind::OAuthAccount => AgentCredential::OAuth {
                    account_reference: Some(secret.redacted_hint.clone()),
                    host_mediated: false,
                },
                _ => AgentCredential::ApiKey {
                    secret: ProjectionSecretReference {
                        id: secret.id.clone(),
                        backend: secret.backend,
                        setup_state: secret.setup_state,
                        lookup_key: secret.lookup_key.clone(),
                        redacted_hint: secret.redacted_hint.clone(),
                        revision: secret.updated_at_ms.max(1),
                        legacy_secret_reference_id: Some(secret.id.clone()),
                    },
                    target_hint: Some(secret.lookup_key.clone()),
                },
            };
            ModelProviderCredentialReference {
                id: secret.id.clone(),
                display_name: secret.display_label.clone(),
                masked_hint: String::new(),
                status,
                credential,
                revision: secret.updated_at_ms.max(1),

                models: Vec::new(),
                proxy_url: None,
            }
        })
        .collect()
}

fn legacy_model_catalog(profile: &ProviderProfile) -> Vec<ModelProviderCatalogEntry> {
    let mut models = profile.configured_models.clone();
    if let Some(default_model) = profile.default_model.as_deref()
        && !default_model.trim().is_empty()
        && !models.iter().any(|model| model.id == default_model)
    {
        models.push(ProviderConfiguredModel {
            id: default_model.to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: vibex_core::ProviderModelCapabilities::default(),
        });
    }
    models
        .into_iter()
        .map(|model| ModelProviderCatalogEntry {
            id: model.id,
            display_name: model.display_name,
            enabled: model.enabled,
            metadata: Vec::new(),
            capabilities: model.capabilities,
        })
        .collect()
}

fn legacy_model_provider_endpoints(profile: &ProviderProfile) -> Vec<ModelProviderEndpoint> {
    let mut endpoints = Vec::new();
    if let Some(url) = profile
        .base_url
        .as_deref()
        .map(str::trim)
        .filter(|url| !url.is_empty())
    {
        endpoints.push(ModelProviderEndpoint {
            id: "api".to_string(),
            kind: ModelProviderEndpointKind::Api,
            url: url.to_string(),
            wire_protocol_id: None,
        });
    }
    for wire_api in ProviderModelWireApi::ALL {
        let option_key = wire_api.protocol_base_url_option_key();
        let Some(url) = provider_option(profile, &option_key) else {
            continue;
        };
        endpoints.push(ModelProviderEndpoint {
            id: format!("api-{}", wire_api.wire_protocol_id().replace('_', "-")),
            kind: ModelProviderEndpointKind::Api,
            url,
            wire_protocol_id: Some(wire_api.wire_protocol_id().to_string()),
        });
    }
    endpoints
}

fn legacy_configured_model_bindings(
    profile: &ProviderProfile,
    binding_id: &AgentModelProviderBindingId,
    interfaces: &[vibex_core::AgentModelInterfaceDescriptor],
) -> VibexResult<Vec<AgentConfiguredModelBinding>> {
    let mut models = profile.configured_models.clone();
    if let Some(default_model) = profile.default_model.as_deref()
        && !default_model.trim().is_empty()
        && !models.iter().any(|model| model.id == default_model)
    {
        models.push(ProviderConfiguredModel {
            id: default_model.to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: Default::default(),
        });
    }
    models
        .into_iter()
        .enumerate()
        .map(|(index, model)| {
            let wire_protocol_id = legacy_wire_protocol(profile, model.wire_api, interfaces);
            let sdk_adapter_id = interfaces
                .iter()
                .find(|interface| interface.wire_protocol_id == wire_protocol_id)
                .and_then(|interface| interface.sdk_adapter_id.clone())
                .or_else(|| legacy_sdk_adapter(&profile.agent_id, &wire_protocol_id));
            let process_scoped = interfaces
                .iter()
                .find(|interface| {
                    interface.wire_protocol_id == wire_protocol_id
                        && interface.sdk_adapter_id == sdk_adapter_id
                })
                .is_some_and(|interface| interface.process_scoped);
            Ok(AgentConfiguredModelBinding {
                id: legacy_model_binding_id(binding_id, index)?,
                provider_model_id: model.id.clone(),
                agent_model_id: legacy_agent_model_id(profile, &model.id),
                wire_protocol_id,
                sdk_adapter_id,
                deployment: None,
                enabled: model.enabled,
                process_scoped,
            })
        })
        .collect()
}

fn legacy_agent_model_id(profile: &ProviderProfile, provider_model_id: &str) -> String {
    if profile.agent_id.as_str() != "claude" {
        return provider_model_id.to_string();
    }

    // The `opus`/`sonnet`/`haiku` aliases only mean what we intend on
    // Anthropic's own endpoint. A profile with its own base URL points at a
    // gateway where the CLI resolves those aliases through the user's
    // `ANTHROPIC_DEFAULT_*_MODEL` settings, so the session silently runs a
    // different model than the one selected here. Keep the concrete id there.
    if profile
        .base_url
        .as_deref()
        .map(str::trim)
        .is_some_and(|url| !url.is_empty())
    {
        return provider_model_id.to_string();
    }

    let context_suffix = if provider_model_id.ends_with("[1m]") {
        "[1m]"
    } else {
        ""
    };
    provider_model_id
        .split(|character: char| !character.is_ascii_alphanumeric())
        .find_map(|part| {
            let part = part.to_ascii_lowercase();
            matches!(part.as_str(), "default" | "opus" | "sonnet" | "haiku")
                .then(|| format!("{part}{context_suffix}"))
        })
        .unwrap_or_else(|| provider_model_id.to_string())
}

/// The request headers a legacy Profile override declares.
///
/// The override is a JSON object keyed by header name. A string is a literal
/// value; an object of the form `{"variable":"session_id"}` is a per-session
/// value the projection leaves for the runtime to fill. Anything else is
/// ignored rather than guessed at, so a hand-edited option cannot turn into a
/// header the user never configured.
///
/// The list is sorted by name because the object it is read from is not the same
/// kind of map in every build: `serde_json` keeps insertion order where the
/// `preserve_order` feature reaches it and sorts otherwise, and the feature is
/// resolved per build graph. An order that came from that map would therefore
/// move between a `-p` build and a workspace build, and the projection compares
/// this list for equality to decide whether a record changed — so the same
/// profile could look edited on one build and not on another, and the settings
/// UI would list the same headers in a different order.
const LEGACY_HEADER_OVERRIDE_OPTION_KEY: &str = "headerOverrideJson";
/// The Provider option that pins the protocol a new Model inherits.
const LEGACY_WIRE_API_OPTION_KEY: &str = "wireApi";
/// The Provider option the website link has always lived in.
///
/// It is a cc-switch import key, so it stays in the option map for older
/// readers; the shared record mirrors it in `metadata.website_url`.
const LEGACY_WEBSITE_URL_OPTION_KEY: &str = "ccSwitchWebsiteUrl";

fn legacy_headers(profile: &ProviderProfile) -> Vec<ModelProviderHeaderReference> {
    let Some(raw) = provider_option(profile, LEGACY_HEADER_OVERRIDE_OPTION_KEY) else {
        return Vec::new();
    };
    let Some(object) = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return Vec::new();
    };
    let mut headers = object
        .into_iter()
        .filter_map(|(name, value)| {
            let name = name.trim().to_string();
            if name.is_empty() {
                return None;
            }
            let (value, redacted_hint) = match &value {
                serde_json::Value::String(literal) => (
                    ModelProviderHeaderValue::NonSecretLiteral(literal.clone()),
                    "non-secret header",
                ),
                serde_json::Value::Object(fields) => {
                    let variable = fields
                        .get("variable")
                        .and_then(serde_json::Value::as_str)
                        .and_then(ModelProviderHeaderVariable::from_id)?;
                    (
                        ModelProviderHeaderValue::DynamicVariable(variable),
                        "session variable",
                    )
                }
                _ => return None,
            };
            Some(ModelProviderHeaderReference {
                name,
                value,
                redacted_hint: redacted_hint.to_string(),
            })
        })
        .collect::<Vec<_>>();
    // Case-insensitive first so a reader sees an alphabetical list whatever a
    // header's own capitalisation is, then by the name itself so two headers
    // differing only in case still have one order.
    headers.sort_by(|left, right| {
        left.name
            .to_ascii_lowercase()
            .cmp(&right.name.to_ascii_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    headers
}

fn legacy_acp_config(profile: &ProviderProfile) -> Option<AcpProviderConfig> {
    profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == LEGACY_ACP_CONFIG_OPTION_KEY)
        .and_then(|entry| serde_json::from_str(&entry.value).ok())
}

fn builtin_acp_config(agent_id: &AgentId) -> Option<AcpProviderConfig> {
    let definition = builtin_agent_definitions()
        .into_iter()
        .find(|definition| &definition.id == agent_id)?;
    let command = definition.command?;
    let env = definition
        .env
        .into_iter()
        .map(|(key, value)| AcpProviderEnvReference {
            key,
            source: AcpProviderEnvSource::Literal,
            value: Some(value),
            secret_lookup_key: None,
            redacted_hint: "builtin non-secret environment".to_string(),
        })
        .collect();
    Some(AcpProviderConfig {
        command: command.command,
        args: command.args,
        env,
        cwd_template: Some("{workspaceRoot}".to_string()),
        process_strategy: AcpProcessStrategy::PerSession,
        terminal_tools: false,
        terminal_auth: false,
        models: Vec::new(),
        modes: definition.modes,
        features: definition.capability_hints,
        disabled_tools: Vec::new(),
    })
}

fn legacy_wire_protocol(
    profile: &ProviderProfile,
    wire_api: Option<ProviderModelWireApi>,
    interfaces: &[vibex_core::AgentModelInterfaceDescriptor],
) -> String {
    match wire_api {
        Some(ProviderModelWireApi::OpenaiResponses) => WIRE_PROTOCOL_OPENAI_RESPONSES,
        Some(ProviderModelWireApi::OpenaiChatCompletions) => WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        Some(ProviderModelWireApi::AnthropicMessages) => WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
        Some(ProviderModelWireApi::MistralConversations) => {
            vibex_core::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS
        }
        Some(ProviderModelWireApi::GoogleGenerativeAi) => {
            vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI
        }
        Some(ProviderModelWireApi::GoogleVertex) => vibex_core::WIRE_PROTOCOL_GOOGLE_VERTEX,
        Some(ProviderModelWireApi::AwsBedrockConverse) => {
            vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE
        }
        None if profile.agent_id.as_str() == "claude" => WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
        None if profile.agent_id.as_str() == "opencode" => {
            legacy_opencode_default_wire_protocol(profile)
        }
        None => interfaces
            .iter()
            .find(|interface| interface.wire_protocol_id == WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS)
            .or_else(|| interfaces.first())
            .map(|interface| interface.wire_protocol_id.as_str())
            .unwrap_or(WIRE_PROTOCOL_OPENAI_RESPONSES),
    }
    .to_string()
}

/// The protocol a shared record's Model declares for itself.
fn shared_model_wire_api_of(model: &ModelProviderCatalogEntry) -> Option<ProviderModelWireApi> {
    model.metadata.iter().find_map(shared_model_wire_api)
}

/// The protocol a Model is called with when there is no launch row to ask.
///
/// [`legacy_wire_protocol`] reads the pin out of `provider_options`, which only
/// a launch row has. A binding written directly on v2 has none, so the answer
/// comes from what the Agent declares, with the two Agents that resolve a
/// protocol from their own identity handled first.
fn legacy_wire_protocol_for(
    agent_id: &AgentId,
    wire_api: Option<ProviderModelWireApi>,
    interfaces: &[vibex_core::AgentModelInterfaceDescriptor],
) -> String {
    if let Some(wire_api) = wire_api {
        return match wire_api {
            ProviderModelWireApi::OpenaiResponses => WIRE_PROTOCOL_OPENAI_RESPONSES,
            ProviderModelWireApi::OpenaiChatCompletions => WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
            ProviderModelWireApi::AnthropicMessages => WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
            ProviderModelWireApi::MistralConversations => {
                vibex_core::WIRE_PROTOCOL_MISTRAL_CONVERSATIONS
            }
            ProviderModelWireApi::GoogleGenerativeAi => {
                vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI
            }
            ProviderModelWireApi::GoogleVertex => vibex_core::WIRE_PROTOCOL_GOOGLE_VERTEX,
            ProviderModelWireApi::AwsBedrockConverse => {
                vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE
            }
        }
        .to_string();
    }
    if agent_id.as_str() == "claude" {
        return WIRE_PROTOCOL_ANTHROPIC_MESSAGES.to_string();
    }
    interfaces
        .iter()
        .find(|interface| interface.wire_protocol_id == WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS)
        .or_else(|| interfaces.first())
        .map(|interface| interface.wire_protocol_id.clone())
        .unwrap_or_else(|| WIRE_PROTOCOL_OPENAI_RESPONSES.to_string())
}

fn legacy_opencode_default_wire_protocol(profile: &ProviderProfile) -> &'static str {
    if let Some(value) = provider_option(profile, "wireApi") {
        let value = value.to_ascii_lowercase().replace(['-', ' '], "_");
        return if value.contains("anthropic") || value == "messages" {
            WIRE_PROTOCOL_ANTHROPIC_MESSAGES
        } else if value.contains("response") {
            WIRE_PROTOCOL_OPENAI_RESPONSES
        } else {
            WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        };
    }

    let mut configured_protocols = ProviderModelWireApi::ALL.into_iter().filter(|wire_api| {
        provider_option(profile, &wire_api.protocol_base_url_option_key()).is_some()
    });
    let Some(configured_protocol) = configured_protocols.next() else {
        return WIRE_PROTOCOL_OPENAI_RESPONSES;
    };
    if configured_protocols.next().is_none() {
        configured_protocol.wire_protocol_id()
    } else {
        WIRE_PROTOCOL_OPENAI_RESPONSES
    }
}

fn legacy_sdk_adapter(agent_id: &AgentId, wire_protocol_id: &str) -> Option<String> {
    if agent_id.as_str() != "opencode" {
        return None;
    }
    match wire_protocol_id {
        WIRE_PROTOCOL_OPENAI_RESPONSES => Some("@ai-sdk/openai".to_string()),
        WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS => Some("@ai-sdk/openai-compatible".to_string()),
        WIRE_PROTOCOL_ANTHROPIC_MESSAGES => Some("@ai-sdk/anthropic".to_string()),
        _ => None,
    }
}

fn provider_option(profile: &ProviderProfile, key: &str) -> Option<String> {
    profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key.trim() == key)
        .map(|entry| entry.value.trim().to_string())
        .filter(|value| !value.is_empty())
}

fn latest_agent_version(conn: &Connection, agent_id: &AgentId) -> VibexResult<Option<String>> {
    let version = conn
        .query_row(
            "
            SELECT version
            FROM agent_discovery_records
            WHERE agent_id = ?1
            ORDER BY discovered_at_ms DESC
            LIMIT 1
            ",
            params![agent_id.as_str()],
            |row| row.get::<_, Option<String>>(0),
        )
        .optional()
        .map_err(storage_err(
            "provider_projection_agent_version_lookup_failed",
            "failed to lookup detected Agent version for provider projection",
        ))?
        .flatten();
    Ok(version.and_then(|version| normalize_detected_version(&version)))
}

fn normalize_detected_version(value: &str) -> Option<String> {
    value
        .split(|character: char| {
            !(character.is_ascii_alphanumeric() || matches!(character, '.' | '-' | '+'))
        })
        .find(|part| {
            part.chars()
                .next()
                .is_some_and(|character| character.is_ascii_digit())
                && part.contains('.')
        })
        .map(ToString::to_string)
}

fn looks_managed_adapter_command(command: &str, args: &[String], adapter_id: &str) -> bool {
    looks_native_agent_command(command, adapter_id)
        || args.iter().any(|arg| arg.contains(adapter_id))
}

fn looks_native_agent_command(command: &str, expected_name: &str) -> bool {
    std::path::Path::new(command)
        .file_name()
        .and_then(|name| name.to_str())
        .is_some_and(|name| name == expected_name || name.starts_with(&format!("{expected_name}.")))
}

pub(crate) fn legacy_model_provider_id(
    id: &ProviderProfileId,
) -> VibexResult<ModelProviderProfileId> {
    ModelProviderProfileId::parse(format!("model_provider_legacy_{}", id.as_str()))
}

fn legacy_runtime_profile_id(id: &ProviderProfileId) -> VibexResult<AgentRuntimeProfileId> {
    AgentRuntimeProfileId::parse(format!("agent_runtime_legacy_{}", id.as_str()))
}

fn legacy_binding_id(id: &ProviderProfileId) -> VibexResult<AgentModelProviderBindingId> {
    AgentModelProviderBindingId::parse(format!("agent_provider_binding_legacy_{}", id.as_str()))
}

fn legacy_model_binding_id(
    binding_id: &AgentModelProviderBindingId,
    order_index: usize,
) -> VibexResult<AgentConfiguredModelBindingId> {
    AgentConfiguredModelBindingId::parse(format!(
        "agent_model_binding_legacy_{}_{}",
        binding_id.as_str(),
        order_index
    ))
}

fn require_revision_update(
    changed: usize,
    code: &'static str,
    message: &'static str,
) -> VibexResult<()> {
    if changed == 1 {
        Ok(())
    } else {
        Err(VibexError::conflict(code, message))
    }
}

fn collect_rows<T>(
    rows: rusqlite::Result<
        rusqlite::MappedRows<'_, impl FnMut(&rusqlite::Row<'_>) -> rusqlite::Result<T>>,
    >,
    code: &'static str,
    message: &'static str,
) -> VibexResult<Vec<T>> {
    let rows = rows.map_err(storage_err(code, message))?;
    let mut values = Vec::new();
    for row in rows {
        values.push(row.map_err(storage_err(code, message))?);
    }
    Ok(values)
}

fn bool_to_db(value: bool) -> i64 {
    i64::from(value)
}

fn db_to_bool(value: i64) -> bool {
    value != 0
}

fn to_sql_decode_error(error: VibexError) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(0, rusqlite::types::Type::Text, Box::new(error))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        CURRENT_SCHEMA_VERSION, MIGRATIONS, ProviderProfileRepository, apply_migrations,
        current_schema_version,
    };
    use vibex_core::{
        ModelProviderEndpoint, ProviderBindingMetadata, ProviderConfiguredModel, ProviderKind,
        ProviderModelWireApi, ProviderOptions, ProviderSecretBackend, ProviderSecretReference,
        RequestId,
    };

    fn schema_through(conn: &mut Connection, version: i64) {
        conn.execute_batch(
            "CREATE TABLE schema_migrations (
                version INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                applied_at_ms INTEGER NOT NULL
            );",
        )
        .unwrap();
        for migration in MIGRATIONS
            .iter()
            .filter(|migration| migration.version <= version)
        {
            let transaction = conn.transaction().unwrap();
            transaction.execute_batch(migration.sql).unwrap();
            transaction
                .execute(
                    "INSERT INTO schema_migrations (version, name, applied_at_ms)
                     VALUES (?1, ?2, ?3)",
                    params![migration.version, migration.name, migration.version],
                )
                .unwrap();
            transaction.commit().unwrap();
        }
    }

    fn available_secret(profile_id: ProviderProfileId) -> ProviderSecretReference {
        ProviderSecretReference {
            id: RequestId::new(),
            provider_profile_id: profile_id,
            secret_kind: ProviderSecretKind::ApiKey,
            backend: ProviderSecretBackend::OsKeychain,
            setup_state: ProviderSecretSetupState::Available,
            lookup_key: "vibex-test-secret-ref".to_string(),
            display_label: "API key".to_string(),
            redacted_hint: "configured".to_string(),
            created_at_ms: 10,
            updated_at_ms: 11,
        }
    }

    fn persist_managed_installation(
        conn: &Connection,
        agent_id: &str,
        registry_agent_id: &str,
        version: &str,
        command: &str,
        args: Vec<String>,
    ) {
        let agent_id = AgentId::parse(agent_id).unwrap();
        AgentManagedInstallationRepository::upsert(
            conn,
            &crate::AgentManagedInstallationRecord {
                agent_id: agent_id.clone(),
                registry_agent_id: registry_agent_id.to_string(),
                state: vibex_core::AgentManagedInstallState {
                    managed: true,
                    status: vibex_core::AgentManagedInstallStatus::Installed,
                    distribution_kind: Some(vibex_core::AgentManagedDistributionKind::Npm),
                    installed_version: Some(version.to_string()),
                    available_version: Some(version.to_string()),
                    last_error_code: None,
                    last_error_message: None,
                    updated_at_ms: Some(100),
                },
                command: Some(vibex_core::AgentCommandConfig {
                    command: command.to_string(),
                    args,
                }),
                install_root: Some(format!("/managed/{agent_id}")),
                updated_at_ms: 100,
            },
        )
        .unwrap();
    }

    #[test]
    fn managed_registry_versions_at_or_above_verified_baselines_keep_projection_compatible() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        let registry = AgentProviderProjectionRegistry::builtin().unwrap();

        let claude_command = "/managed/node";
        let claude_args = vec!["/managed/claude-agent-acp.js".to_string()];
        persist_managed_installation(
            &conn,
            "claude",
            "claude-acp",
            "0.72.0",
            claude_command,
            claude_args.clone(),
        );
        let claude_profile = ProviderProfile::local_default(ProviderKind::Claude);
        let claude_identity =
            legacy_runtime_identity(&conn, &claude_profile, claude_command, &claude_args).unwrap();
        assert_eq!(claude_identity.adapter_version.as_deref(), Some("0.72.0"));
        assert_eq!(
            registry.resolve(&claude_identity).unwrap().match_kind,
            ProjectionDescriptorMatch::SemverRange
        );

        let codex_command = "/managed/node";
        let codex_args = vec!["/managed/codex-acp.js".to_string()];
        persist_managed_installation(
            &conn,
            "codex",
            "codex-acp",
            "1.8.13",
            codex_command,
            codex_args.clone(),
        );
        let codex_profile = ProviderProfile::local_default(ProviderKind::Codex);
        let codex_identity =
            legacy_runtime_identity(&conn, &codex_profile, codex_command, &codex_args).unwrap();
        assert_eq!(codex_identity.adapter_version.as_deref(), Some("1.8.13"));
        assert!(codex_identity.runtime_dependencies.is_empty());
        assert_eq!(
            registry.resolve(&codex_identity).unwrap().match_kind,
            ProjectionDescriptorMatch::SemverRange
        );

        let codebuddy_command = "/managed/codebuddy";
        let codebuddy_args = vec!["--acp".to_string()];
        persist_managed_installation(
            &conn,
            "codebuddy-code",
            "codebuddy-code",
            "2.110.0",
            codebuddy_command,
            codebuddy_args.clone(),
        );
        let mut codebuddy_profile = ProviderProfile::local_default(ProviderKind::Acp);
        codebuddy_profile.agent_id = AgentId::parse("codebuddy-code").unwrap();
        let codebuddy_identity = legacy_runtime_identity(
            &conn,
            &codebuddy_profile,
            codebuddy_command,
            &codebuddy_args,
        )
        .unwrap();
        assert_eq!(codebuddy_identity.agent_version.as_deref(), Some("2.110.0"));
        assert_eq!(
            registry.resolve(&codebuddy_identity).unwrap().match_kind,
            ProjectionDescriptorMatch::SemverRange
        );
    }

    #[test]
    fn pinned_catalog_commands_supply_exact_versions_only_for_full_command_matches() {
        for (agent_id, expected_version) in [
            ("dirac", "0.4.1"),
            ("factory-droid", "0.153.1"),
            ("pi", "0.0.34"),
            ("qwen-code", "0.18.4"),
        ] {
            let entry = acp_agent_catalog_entries()
                .iter()
                .find(|entry| entry.id == agent_id)
                .unwrap_or_else(|| panic!("missing catalog entry for {agent_id}"));
            let command = entry.command[0];
            let args = entry.command[1..]
                .iter()
                .map(|value| (*value).to_string())
                .collect::<Vec<_>>();
            assert_eq!(
                pinned_catalog_command_version(agent_id, command, &args).as_deref(),
                Some(expected_version)
            );

            let mut changed_args = args.clone();
            changed_args.push("--unexpected".to_string());
            assert_eq!(
                pinned_catalog_command_version(agent_id, command, &changed_args),
                None
            );
            assert_eq!(
                pinned_catalog_command_version(agent_id, "untrusted-runner", &args),
                None
            );
        }
    }

    #[test]
    fn migration_37_backfills_v36_profiles_idempotently() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        schema_through(&mut conn, 36);

        // An ordinary Profile, not the per-kind placeholder: the placeholder is
        // gone, and using its id here would make the test depend on a row the
        // product no longer creates.
        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        // The builder still describes the shape; only the placeholder id is not
        // reused, because the product no longer creates a row under it.
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://api.example.invalid".to_string());
        profile.default_model = Some("claude-test".to_string());
        profile.configured_models = vec![ProviderConfiguredModel {
            id: "claude-test".to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: Default::default(),
        }];
        profile.secrets = vec![available_secret(profile.id.clone())];
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        // The test's subject is that migration 37 runs and backfills, not the
        // exact tail of the migration list: asserting every later version by
        // name made this test fail for each unrelated migration added after it.
        let applied = apply_migrations(&mut conn).unwrap();
        assert_eq!(
            applied.first().map(String::as_str),
            Some("37:agent_provider_projection_platform"),
            "a database at v36 should start applying from v37"
        );
        assert_eq!(
            applied.len() as i64,
            CURRENT_SCHEMA_VERSION - 36,
            "every pending migration should run exactly once: {applied:?}"
        );
        assert!(
            !apply_migrations(&mut conn)
                .unwrap()
                .iter()
                .any(|entry| entry.starts_with("37:")),
            "re-running migrations must not replay v37"
        );
        assert_eq!(
            current_schema_version(&conn).unwrap(),
            CURRENT_SCHEMA_VERSION
        );
        assert_eq!(
            ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap(),
            0
        );
        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
                .unwrap();
        assert_eq!(records.model_provider.credentials.len(), 1);
        assert_eq!(
            records.binding.status,
            AgentModelProviderBindingStatus::Ready
        );
        assert_eq!(
            records.binding.verification.state,
            ProjectionEvidenceState::Verified
        );
        assert_eq!(
            AgentModelProviderBindingRepository::list_for_model_provider(
                &conn,
                &records.model_provider.id
            )
            .unwrap()
            .len(),
            1
        );
    }

    #[test]
    fn legacy_claude_model_families_project_to_acp_aliases_and_repair_existing_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.default_model = Some("claude-opus-5".to_string());
        profile.configured_models = [
            "claude-opus-5",
            "claude-sonnet-4-5",
            "claude-haiku-4-5",
            "custom-model",
        ]
        .into_iter()
        .map(|id| ProviderConfiguredModel {
            id: id.to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: Default::default(),
        })
        .collect();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
                .unwrap();
        assert_eq!(
            records
                .binding
                .configured_models
                .iter()
                .map(|model| (
                    model.provider_model_id.as_str(),
                    model.agent_model_id.as_str()
                ))
                .collect::<Vec<_>>(),
            vec![
                ("claude-opus-5", "opus"),
                ("claude-sonnet-4-5", "sonnet"),
                ("claude-haiku-4-5", "haiku"),
                ("custom-model", "custom-model"),
            ]
        );

        conn.execute(
            "UPDATE agent_configured_model_bindings
             SET agent_model_id = provider_model_id
             WHERE agent_model_provider_binding_id = ?1",
            [records.binding.id.as_str()],
        )
        .unwrap();

        ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap();
        let repaired = AgentModelProviderBindingRepository::get(&conn, &records.binding.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            repaired
                .configured_models
                .iter()
                .map(|model| model.agent_model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["opus", "sonnet", "haiku", "custom-model"]
        );
    }

    #[test]
    fn legacy_claude_context_window_suffix_remains_distinct_during_save_and_backfill() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.default_model = Some("claude-opus-4-6[1m]".to_string());
        profile.configured_models = ["claude-opus-4-6", "claude-opus-4-6[1m]"]
            .into_iter()
            .map(|id| ProviderConfiguredModel {
                id: id.to_string(),
                display_name: None,
                enabled: true,
                wire_api: None,
                capabilities: Default::default(),
            })
            .collect();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let saved = ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
            .unwrap();
        assert_eq!(
            saved
                .binding
                .configured_models
                .iter()
                .map(|model| model.agent_model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["opus", "opus[1m]"]
        );

        assert_eq!(
            ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap(),
            0
        );
        let restored = AgentModelProviderBindingRepository::get(&conn, &saved.binding.id)
            .unwrap()
            .unwrap();
        assert_eq!(restored.configured_models, saved.binding.configured_models);
    }

    #[test]
    fn legacy_header_override_parses_literals_and_dynamic_variables() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.provider_options = ProviderOptions {
            schema_version: 1,
            entries: vec![ProviderBindingMetadata {
                key: "headerOverrideJson".to_string(),
                value: serde_json::json!({
                    "x-opencode-session": {"variable": "session_id"},
                    "User-Agent": "vibex/1.0",
                    "ignored": 7,
                    "unknown-variable": {"variable": "not_a_variable"},
                })
                .to_string(),
            }],
        };
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let saved = ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
            .unwrap();
        // The order is the projection's, not the JSON object's: it is sorted by
        // name so the same profile projects the same list on every build.
        assert_eq!(
            saved.model_provider.headers,
            vec![
                ModelProviderHeaderReference {
                    name: "User-Agent".to_string(),
                    value: ModelProviderHeaderValue::NonSecretLiteral("vibex/1.0".to_string()),
                    redacted_hint: "non-secret header".to_string(),
                },
                ModelProviderHeaderReference {
                    name: "x-opencode-session".to_string(),
                    value: ModelProviderHeaderValue::DynamicVariable(
                        ModelProviderHeaderVariable::SessionId,
                    ),
                    redacted_hint: "session variable".to_string(),
                },
            ]
        );
    }

    #[test]
    fn legacy_claude_gateway_profile_keeps_concrete_model_ids() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://gateway.example.invalid".to_string());
        profile.default_model = Some("claude-opus-5[1m]".to_string());
        profile.configured_models = ["claude-opus-5", "claude-opus-5[1m]", "claude-haiku-4-5"]
            .into_iter()
            .map(|id| ProviderConfiguredModel {
                id: id.to_string(),
                display_name: None,
                enabled: true,
                wire_api: None,
                capabilities: Default::default(),
            })
            .collect();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let saved = ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
            .unwrap();

        // A gateway resolves `opus`/`haiku` through the user's
        // `ANTHROPIC_DEFAULT_*_MODEL` settings, which would run a different
        // model than the selected one.
        assert_eq!(
            saved
                .binding
                .configured_models
                .iter()
                .map(|model| model.agent_model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-opus-5", "claude-opus-5[1m]", "claude-haiku-4-5"]
        );

        assert_eq!(
            ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap(),
            0
        );
    }

    #[test]
    fn legacy_claude_gateway_profile_repairs_previously_aliased_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://gateway.example.invalid".to_string());
        profile.default_model = Some("claude-opus-5".to_string());
        profile.configured_models = ["claude-opus-5"]
            .into_iter()
            .map(|id| ProviderConfiguredModel {
                id: id.to_string(),
                display_name: None,
                enabled: true,
                wire_api: None,
                capabilities: Default::default(),
            })
            .collect();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        let saved = ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
            .unwrap();

        // Rows written before the gateway carve-out still carry the alias.
        conn.execute(
            "UPDATE agent_configured_model_bindings
             SET agent_model_id = 'opus'
             WHERE agent_model_provider_binding_id = ?1",
            [saved.binding.id.as_str()],
        )
        .unwrap();

        assert_eq!(
            ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap(),
            0
        );
        let repaired = AgentModelProviderBindingRepository::get(&conn, &saved.binding.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            repaired
                .configured_models
                .iter()
                .map(|model| model.agent_model_id.as_str())
                .collect::<Vec<_>>(),
            vec!["claude-opus-5"]
        );
    }

    #[test]
    fn a_fresh_migration_creates_no_placeholder_providers() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        apply_migrations(&mut conn).unwrap();

        // A fresh database has no Providers, because none were added. The per-kind
        // placeholders used to be seeded here so a caller always had an id; they are
        // gone, and "no Provider" is now reported as such.
        assert!(
            ProviderProfileRepository::list(&conn).unwrap().is_empty(),
            "a fresh database must not seed Provider rows"
        );
        assert!(
            ModelProviderProfileRepository::list(&conn)
                .unwrap()
                .is_empty(),
            "and must not seed shared records either"
        );
    }

    #[test]
    fn legacy_profile_projects_default_and_protocol_specific_endpoints() {
        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.base_url = Some("https://default.example.invalid/v1".to_string());
        profile.provider_options.entries.extend([
            ProviderBindingMetadata {
                key: ProviderModelWireApi::GoogleGenerativeAi.protocol_base_url_option_key(),
                value: "https://google.example.invalid/v1beta".to_string(),
            },
            ProviderBindingMetadata {
                key: ProviderModelWireApi::AwsBedrockConverse.protocol_base_url_option_key(),
                value: "https://bedrock.example.invalid".to_string(),
            },
        ]);

        let endpoints = legacy_model_provider_endpoints(&profile);
        assert_eq!(endpoints.len(), 3);
        assert_eq!(endpoints[0].id, "api");
        assert_eq!(endpoints[0].wire_protocol_id, None);
        assert_eq!(
            endpoints[1].wire_protocol_id.as_deref(),
            Some(vibex_core::WIRE_PROTOCOL_GOOGLE_GENERATIVE_AI)
        );
        assert_eq!(endpoints[1].url, "https://google.example.invalid/v1beta");
        assert_eq!(
            endpoints[2].wire_protocol_id.as_deref(),
            Some(vibex_core::WIRE_PROTOCOL_AWS_BEDROCK_CONVERSE)
        );
        assert_eq!(endpoints[2].url, "https://bedrock.example.invalid");
    }

    #[test]
    fn legacy_opencode_inherits_a_single_protocol_url_and_repairs_existing_rows() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://gateway.example.invalid".to_string());
        profile.default_model = Some("claude-opus-5".to_string());
        profile.configured_models = vec![ProviderConfiguredModel {
            id: "claude-opus-5".to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
            capabilities: Default::default(),
        }];
        profile
            .provider_options
            .entries
            .push(ProviderBindingMetadata {
                key: ProviderModelWireApi::AnthropicMessages.protocol_base_url_option_key(),
                value: "https://gateway.example.invalid".to_string(),
            });
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
                .unwrap();
        let model = &records.binding.configured_models[0];
        assert_eq!(model.wire_protocol_id, WIRE_PROTOCOL_ANTHROPIC_MESSAGES);
        assert_eq!(model.sdk_adapter_id.as_deref(), Some("@ai-sdk/anthropic"));

        conn.execute(
            "UPDATE agent_configured_model_bindings
             SET wire_protocol_id = ?2, sdk_adapter_id = ?3
             WHERE agent_model_provider_binding_id = ?1",
            params![
                records.binding.id.as_str(),
                WIRE_PROTOCOL_OPENAI_RESPONSES,
                "@ai-sdk/openai",
            ],
        )
        .unwrap();

        ProviderProjectionCompatibilityRepository::backfill_legacy_profiles(&conn).unwrap();
        let repaired = AgentModelProviderBindingRepository::get(&conn, &records.binding.id)
            .unwrap()
            .unwrap();
        let repaired_model = &repaired.configured_models[0];
        assert_eq!(
            repaired_model.wire_protocol_id,
            WIRE_PROTOCOL_ANTHROPIC_MESSAGES
        );
        assert_eq!(
            repaired_model.sdk_adapter_id.as_deref(),
            Some("@ai-sdk/anthropic")
        );
    }

    #[test]
    fn legacy_opencode_explicit_or_ambiguous_protocol_defaults_remain_stable() {
        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.provider_options.entries.extend([
            ProviderBindingMetadata {
                key: ProviderModelWireApi::AnthropicMessages.protocol_base_url_option_key(),
                value: "https://anthropic.example.invalid".to_string(),
            },
            ProviderBindingMetadata {
                key: "wireApi".to_string(),
                value: "chat".to_string(),
            },
        ]);
        assert_eq!(
            legacy_opencode_default_wire_protocol(&profile),
            WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        );

        profile
            .provider_options
            .entries
            .retain(|entry| entry.key != "wireApi");
        profile
            .provider_options
            .entries
            .push(ProviderBindingMetadata {
                key: ProviderModelWireApi::GoogleGenerativeAi.protocol_base_url_option_key(),
                value: "https://google.example.invalid".to_string(),
            });
        assert_eq!(
            legacy_opencode_default_wire_protocol(&profile),
            WIRE_PROTOCOL_OPENAI_RESPONSES
        );
    }

    #[test]
    fn documented_legacy_projection_never_initializes_as_ready() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        let mut profile = ProviderProfile::local_default(ProviderKind::Acp);
        profile.id = ProviderProfileId::new();
        profile.agent_id = AgentId::parse("gemini").unwrap();
        profile.default_model = Some("gemini-test".to_string());
        profile.configured_models = vec![ProviderConfiguredModel {
            id: "gemini-test".to_string(),
            display_name: None,
            enabled: true,
            wire_api: Some(ProviderModelWireApi::GoogleGenerativeAi),
            capabilities: Default::default(),
        }];

        let records = map_legacy_projection_records(&conn, &profile).unwrap();
        assert_eq!(
            records.binding.verification.state,
            ProjectionEvidenceState::Documented
        );
        assert_eq!(
            records.binding.status,
            AgentModelProviderBindingStatus::Unverified
        );
    }

    #[test]
    fn legacy_repository_reads_are_safe_inside_a_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        apply_migrations(&mut conn).unwrap();
        // Any Profile will do; the subject is that reading one inside a caller's
        // transaction does not try to start a nested one. It is created here because
        // nothing is seeded any more.
        let mut profile = ProviderProfile::local_default(ProviderKind::Codex);
        profile.id = ProviderProfileId::new();
        ProviderProfileRepository::insert(&conn, &profile).unwrap();
        let profile_id = profile.id.clone();

        let transaction = conn.transaction().unwrap();
        let read = ProviderProfileRepository::get(&transaction, &profile_id).unwrap();

        assert!(read.is_some());
        transaction.commit().unwrap();
    }

    #[test]
    fn one_model_provider_can_bind_two_agent_runtimes() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        apply_migrations(&mut conn).unwrap();

        let now = 100;
        let shared = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Shared gateway".to_string(),
            vendor_hint: Some("openai-compatible".to_string()),
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://gateway.example.invalid/v1".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: vec![ModelProviderCatalogEntry {
                id: "shared-model".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            }],
            default_model_id: Some("shared-model".to_string()),
            headers: Vec::new(),
            metadata: Default::default(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: now,
            updated_at_ms: now,
            deleted_at_ms: None,
        };
        ModelProviderProfileRepository::insert(&conn, &shared).unwrap();

        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            let mut legacy = ProviderProfile::local_default(kind);
            legacy.id = ProviderProfileId::new();
            let mut records = map_legacy_projection_records(&conn, &legacy).unwrap();
            AgentRuntimeProfileRepository::insert(&conn, &records.agent_runtime).unwrap();
            records.binding.model_provider_profile_id = Some(shared.id.clone());
            AgentModelProviderBindingRepository::insert(&conn, &records.binding).unwrap();
        }

        let bindings =
            AgentModelProviderBindingRepository::list_for_model_provider(&conn, &shared.id)
                .unwrap();
        assert_eq!(bindings.len(), 2);
        assert_eq!(
            bindings
                .iter()
                .map(|binding| binding.agent_id.as_str())
                .collect::<std::collections::BTreeSet<_>>(),
            std::collections::BTreeSet::from(["claude", "codex"])
        );

        let mut updated = shared.clone();
        updated.display_name = "Shared gateway updated".to_string();
        updated.revision = 2;
        updated.updated_at_ms = now + 1;
        ModelProviderProfileRepository::update(&conn, &updated, 1).unwrap();
        let error = ModelProviderProfileRepository::update(&conn, &updated, 1).unwrap_err();
        assert_eq!(error.code, "model_provider_profile_revision_conflict");
    }

    /// The unfiltered read is what the Config Center's "used by" aggregation
    /// needs, so it must return every binding instead of nothing.
    #[test]
    fn list_all_returns_every_live_binding_across_agents() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        apply_migrations(&mut conn).unwrap();

        // Migrations seed the local default bindings, so the baseline is
        // whatever they created rather than zero.
        let baseline = AgentModelProviderBindingRepository::list_all(&conn)
            .unwrap()
            .len();

        let shared = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Shared".to_string(),
            vendor_hint: None,
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://shared.example.invalid/v1".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: Vec::new(),
            default_model_id: None,
            headers: Vec::new(),
            metadata: Default::default(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: 100,
            updated_at_ms: 100,
            deleted_at_ms: None,
        };
        ModelProviderProfileRepository::insert(&conn, &shared).unwrap();

        let mut inserted = Vec::new();
        for kind in [ProviderKind::Claude, ProviderKind::Codex] {
            let mut legacy = ProviderProfile::local_default(kind);
            legacy.id = ProviderProfileId::new();
            let mut records = map_legacy_projection_records(&conn, &legacy).unwrap();
            AgentRuntimeProfileRepository::insert(&conn, &records.agent_runtime).unwrap();
            records.binding.model_provider_profile_id = Some(shared.id.clone());
            AgentModelProviderBindingRepository::insert(&conn, &records.binding).unwrap();
            inserted.push(records.binding.agent_id.as_str().to_string());
        }

        let all = AgentModelProviderBindingRepository::list_all(&conn).unwrap();
        assert_eq!(all.len(), baseline + 2);
        let agents = all
            .iter()
            .map(|binding| binding.agent_id.as_str().to_string())
            .collect::<std::collections::BTreeSet<_>>();
        for agent in &inserted {
            assert!(
                agents.contains(agent),
                "the unfiltered read must include the {agent} binding"
            );
        }
        // The shared row mapper still hydrates each binding's own model list,
        // so the unfiltered read is not a shallower shape than the filtered
        // ones it shares its SQL with.
        assert!(
            all.iter()
                .all(|binding| binding.configured_models.is_empty() || binding.revision > 0)
        );
    }

    #[test]
    fn legacy_codex_chat_is_preserved_but_cannot_become_ready() {
        let mut conn = Connection::open_in_memory().unwrap();
        apply_migrations(&mut conn).unwrap();
        let mut profile = ProviderProfile::local_default(ProviderKind::Codex);
        profile.id = ProviderProfileId::new();
        profile.status = ProviderProfileStatus::Enabled;
        profile.default_model = Some("gpt-test".to_string());
        profile.configured_models = vec![ProviderConfiguredModel {
            id: "gpt-test".to_string(),
            display_name: None,
            enabled: true,
            wire_api: Some(ProviderModelWireApi::OpenaiChatCompletions),
            capabilities: Default::default(),
        }];
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &profile)
                .unwrap();
        assert_eq!(
            records.binding.status,
            AgentModelProviderBindingStatus::Unsupported
        );
        assert_eq!(
            records.binding.configured_models[0].wire_protocol_id,
            WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
        );
    }

    /// A Provider that points at an OpenCode gateway is repaired even when it
    /// predates the preset declaring the session header.
    #[test]
    fn opencode_session_header_backfills_existing_providers() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut opencode = ProviderProfile::local_default(ProviderKind::Acp);
        opencode.id = ProviderProfileId::new();
        opencode.status = ProviderProfileStatus::Enabled;
        opencode.base_url = Some("https://opencode.ai/zen/go/v1".to_string());
        opencode.provider_options = ProviderOptions::empty();
        ProviderProfileRepository::insert(&conn, &opencode).unwrap();

        let mut other = ProviderProfile::local_default(ProviderKind::Acp);
        other.id = ProviderProfileId::new();
        other.status = ProviderProfileStatus::Enabled;
        other.base_url = Some("https://api.example.invalid/v1".to_string());
        other.provider_options = ProviderOptions::empty();
        ProviderProfileRepository::insert(&conn, &other).unwrap();

        let standalone = ModelProviderProfile {
            id: ModelProviderProfileId::new(),
            legacy_provider_profile_id: None,
            display_name: "Imported OpenCode Go".to_string(),
            vendor_hint: Some("opencode-go".to_string()),
            endpoints: vec![ModelProviderEndpoint {
                id: "api".to_string(),
                kind: ModelProviderEndpointKind::Api,
                url: "https://opencode.ai/zen/go/v1".to_string(),
                wire_protocol_id: None,
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: vec![ModelProviderCatalogEntry {
                id: "deepseek-v4.1-flash".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
                capabilities: Default::default(),
            }],
            default_model_id: Some("deepseek-v4.1-flash".to_string()),
            headers: Vec::new(),
            metadata: Default::default(),
            status: ModelProviderProfileStatus::Enabled,
            revision: 1,
            created_at_ms: 100,
            updated_at_ms: 100,
            deleted_at_ms: None,
        };
        ModelProviderProfileRepository::insert(&conn, &standalone).unwrap();

        // Let the rest of the schema stand and run only the header repair again.
        conn.execute("DELETE FROM schema_migrations WHERE version = 87", [])
            .unwrap();
        apply_migrations(&mut conn).unwrap();

        let saved = ProviderProfileRepository::get(&conn, &opencode.id)
            .unwrap()
            .unwrap();
        let raw = provider_option(&saved, LEGACY_HEADER_OVERRIDE_OPTION_KEY)
            .expect("the OpenCode Provider must carry a header override");
        let stored: serde_json::Value = serde_json::from_str(&raw).unwrap();
        assert_eq!(stored["x-opencode-session"]["variable"], "session_id");

        let projected = ModelProviderProfileRepository::get_by_legacy_profile(&conn, &saved.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            projected
                .headers
                .iter()
                .filter(|header| header.name == OPENCODE_SESSION_HEADER)
                .count(),
            1
        );

        let imported = ModelProviderProfileRepository::get(&conn, &standalone.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            imported
                .headers
                .iter()
                .filter(|header| header.name == OPENCODE_SESSION_HEADER)
                .count(),
            1
        );

        // Another gateway is not given a header.
        let saved = ProviderProfileRepository::get(&conn, &other.id)
            .unwrap()
            .unwrap();
        assert!(provider_option(&saved, LEGACY_HEADER_OVERRIDE_OPTION_KEY).is_none());

        // Running the repair again does not add a second row.
        conn.execute("DELETE FROM schema_migrations WHERE version = 87", [])
            .unwrap();
        apply_migrations(&mut conn).unwrap();
        let imported = ModelProviderProfileRepository::get(&conn, &standalone.id)
            .unwrap()
            .unwrap();
        assert_eq!(
            imported
                .headers
                .iter()
                .filter(|header| header.name == OPENCODE_SESSION_HEADER)
                .count(),
            1
        );
    }
}

#[cfg(test)]
mod binding_launch_field_tests {
    use super::*;
    use crate::apply_migrations;
    use vibex_core::{
        ProviderConfiguredModel, ProviderOptions, WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
    };

    /// The migration adds the three columns, and adding them twice changes
    /// nothing.
    ///
    /// Round-tripping the values needs a runtime row and a Provider row, which
    /// the projection tests already build; what is not covered anywhere else is
    /// whether the columns exist at all on an existing database — the case that
    /// matters, because every user has one.
    #[test]
    fn the_launch_columns_are_added_idempotently() {
        let dir = std::env::temp_dir().join(format!(
            "vibex-binding-columns-{}",
            vibex_core::RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        apply_migrations(&mut conn).unwrap();

        let columns = |conn: &Connection| {
            let mut stmt = conn
                .prepare("SELECT name FROM pragma_table_info('agent_model_provider_bindings_v2')")
                .unwrap();
            stmt.query_map([], |row| row.get::<_, String>(0))
                .unwrap()
                .filter_map(Result::ok)
                .collect::<Vec<_>>()
        };
        let first = columns(&conn);
        for expected in ["account_alias", "reasoning_effort", "provider_options_json"] {
            assert!(
                first.iter().any(|name| name == expected),
                "{expected} is missing from the v2 binding: {first:?}"
            );
        }

        // Re-running migrations must not fail on the columns already being
        // there, which is what a user upgrading twice would exercise.
        apply_migrations(&mut conn).unwrap();
        assert_eq!(columns(&conn), first);

        std::fs::remove_dir_all(&dir).ok();
    }

    /// A legacy Profile's launch fields reach the v2 binding, and survive a
    /// second sync unchanged.
    ///
    /// This is the case the round trip has to prove: the launch path reads the
    /// legacy row, so if the projection drops these three the v2 side cannot be
    /// the authority for them — which is the whole reason they were added.
    #[test]
    fn launch_fields_survive_the_projection() {
        let dir = std::env::temp_dir().join(format!(
            "vibex-binding-projection-{}",
            vibex_core::RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        apply_migrations(&mut conn).unwrap();

        let mut options = ProviderOptions::empty();
        options.entries.push(vibex_core::ProviderBindingMetadata {
            key: "wireApi".to_string(),
            value: WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS.to_string(),
        });
        let legacy = ProviderProfile {
            id: ProviderProfileId::new(),
            agent_id: AgentId::parse("codex").unwrap(),
            kind: vibex_core::ProviderKind::Codex,
            display_name: "DeepSeek".to_string(),
            status: ProviderProfileStatus::Enabled,
            account_alias: Some("Work account".to_string()),
            base_url: Some("https://api.example.com/v1".to_string()),
            default_model: Some("deepseek-chat".to_string()),
            small_model: None,
            large_model: None,
            configured_models: vec![ProviderConfiguredModel {
                id: "deepseek-chat".to_string(),
                display_name: None,
                enabled: true,
                wire_api: Some(vibex_core::ProviderModelWireApi::OpenaiChatCompletions),
                capabilities: Default::default(),
            }],
            reasoning_effort: Some("high".to_string()),
            sandbox_defaults: vibex_core::ProviderSandboxDefaults::workspace_write_ask_on_risk(),
            network_defaults: vibex_core::ProviderNetworkDefaults::local_default(),
            permission_defaults: vibex_core::ProviderPermissionDefaults::ask_on_risk(),
            provider_options: options,
            secrets: Vec::new(),
            created_at_ms: 1,
            updated_at_ms: 1,
            deleted_at_ms: None,
        };
        crate::ProviderProfileRepository::insert(&conn, &legacy).unwrap();
        let records =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &legacy).unwrap();

        assert_eq!(
            records.binding.account_alias.as_deref(),
            Some("Work account"),
            "the alias must reach the binding"
        );
        assert_eq!(
            records.binding.reasoning_effort.as_deref(),
            Some("high"),
            "the effort must reach the binding"
        );
        assert_eq!(
            records.binding.provider_options.entries.len(),
            1,
            "the protocol pin must reach the binding, because the launch path reads it from here"
        );

        // A second sync is a no-op for these fields, so a repeated write cannot
        // quietly clear them.
        let again =
            ProviderProjectionCompatibilityRepository::sync_legacy_profile(&conn, &legacy).unwrap();
        assert_eq!(again.binding.account_alias.as_deref(), Some("Work account"));
        assert_eq!(again.binding.reasoning_effort.as_deref(), Some("high"));
        assert_eq!(again.binding.provider_options.entries.len(), 1);

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod binding_options_migration_tests {
    use crate::apply_migrations;

    /// Migration 68's default was not a decodable `ProviderOptions`.
    ///
    /// `ProviderOptions` has required fields, so `'{}'` fails to deserialize and
    /// every pre-existing binding failed to load — the runtime refused to start
    /// with `agent_model_provider_binding_lookup_failed`. This asserts the two
    /// things that have to hold: the column now defaults to a value the type can
    /// read, and a row carrying the old value is repaired.
    #[test]
    fn the_options_column_defaults_to_a_decodable_value() {
        let dir = std::env::temp_dir().join(format!(
            "vibex-binding-repair-{}",
            vibex_core::RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        apply_migrations(&mut conn).unwrap();

        // Whatever a new row gets by default must decode.
        let default: String = conn
            .query_row(
                "SELECT dflt_value FROM pragma_table_info('agent_model_provider_bindings_v2')
                 WHERE name = 'provider_options_json'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let decoded =
            serde_json::from_str::<vibex_core::ProviderOptions>(default.trim_matches('\''))
                .expect("the column default must be a decodable ProviderOptions");
        assert_eq!(decoded, vibex_core::ProviderOptions::empty());

        // And a row left with the old empty object is repaired — even though the
        // repair migration is already recorded, which is the case that matters: the
        // empty object can be written again long after the first migration ran.
        conn.execute(
            "UPDATE agent_model_provider_bindings_v2
             SET provider_options_json = '{}'",
            [],
        )
        .unwrap();
        apply_migrations(&mut conn).unwrap();
        let remaining: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_model_provider_bindings_v2
                 WHERE trim(provider_options_json) = '{}'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(remaining, 0, "no row may keep an undecodable option set");

        std::fs::remove_dir_all(&dir).ok();
    }
}

#[cfg(test)]
mod real_database_upgrade_tests {
    use crate::apply_migrations;

    /// Upgrading a database that already ran migration 68 must repair it.
    ///
    /// This reproduces the reported failure exactly: a database with 68 and 69
    /// recorded, the column defaulted to `'{}'`, and rows carrying it. Running
    /// migrations again has to make every binding loadable, because that is what
    /// happens when a user opens the new build on an old database.
    #[test]
    fn an_already_migrated_database_is_repaired_on_upgrade() {
        let dir = std::env::temp_dir().join(format!(
            "vibex-real-upgrade-{}",
            vibex_core::RequestId::new().as_str()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("state.db");
        let mut conn = rusqlite::Connection::open(&path).unwrap();
        apply_migrations(&mut conn).unwrap();

        // Put the database back into the broken state the report describes:
        // 68 applied, 70 not, and rows holding the empty object.
        conn.execute_batch(
            "DELETE FROM schema_migrations WHERE version = 70;
             UPDATE agent_model_provider_bindings_v2 SET provider_options_json = '{}';",
        )
        .unwrap();
        let broken: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_model_provider_bindings_v2
                 WHERE trim(provider_options_json) = '{}'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(broken >= 0);

        // The upgrade a user gets by opening the new build.
        apply_migrations(&mut conn).unwrap();

        let still_broken: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM agent_model_provider_bindings_v2
                 WHERE trim(provider_options_json) = '{}'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            still_broken, 0,
            "every binding must hold a decodable option set after the upgrade"
        );

        std::fs::remove_dir_all(&dir).ok();
    }
}

/// Adds the advisory Provider metadata column.
///
/// The column is appended rather than replacing `vendor_hint`: older rows
/// read as `{}`, and the launch projection never reads it.
/// Adds the Provider metadata column when the table predates it.
///
/// The compatibility backfill and the source-default backfill both read through
/// the normal repository, which selects the column. Running this before them is
/// what lets an older database migrate instead of failing with a missing-column
/// lookup error.
pub(crate) fn ensure_provider_metadata_column(conn: &Connection) -> VibexResult<()> {
    let has_metadata = conn
        .prepare("PRAGMA table_info(model_provider_profiles)")
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .iter()
        .any(|column| column == "metadata_json");
    if !has_metadata {
        conn.execute_batch(
            "ALTER TABLE model_provider_profiles
                 ADD COLUMN metadata_json TEXT NOT NULL DEFAULT '{}';",
        )
        .map_err(storage_err(
            "model_provider_metadata_apply_failed",
            "failed to add the Provider metadata column",
        ))?;
    }
    Ok(())
}

pub(crate) fn migrate_provider_metadata(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 82;
    const NAME: &str = "model_provider_metadata";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_err(
            "model_provider_metadata_transaction_failed",
            "failed to start the Provider metadata migration",
        ))?;
    let has_metadata = transaction
        .prepare("PRAGMA table_info(model_provider_profiles)")
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
            "failed to inspect the Provider profile table",
        ))?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(storage_err(
            "model_provider_metadata_inspect_failed",
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
                "model_provider_metadata_apply_failed",
                "failed to add the Provider metadata column",
            ))?;
    }
    transaction
        .execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![VERSION, NAME, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record the Provider metadata migration",
        ))?;
    transaction.commit().map_err(storage_err(
        "model_provider_metadata_commit_failed",
        "failed to commit the Provider metadata migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

/// The header OpenCode's gateways route a request by.
const OPENCODE_SESSION_HEADER: &str = "x-opencode-session";

/// Whether one request address names an OpenCode gateway.
///
/// Matched case-insensitively and by containment, the same way the Provider
/// editor recognises it: both Zen and Go sit under this path.
fn url_names_opencode_gateway(url: &str) -> bool {
    url.trim().to_ascii_lowercase().contains("opencode.ai/zen")
}

fn model_provider_names_opencode_gateway(profile: &ModelProviderProfile) -> bool {
    profile
        .endpoints
        .iter()
        .any(|endpoint| url_names_opencode_gateway(&endpoint.url))
}

fn legacy_provider_names_opencode_gateway(profile: &ProviderProfile) -> bool {
    profile
        .base_url
        .as_deref()
        .is_some_and(url_names_opencode_gateway)
        || profile.provider_options.entries.iter().any(|entry| {
            entry.key.starts_with("protocolBaseUrl.") && url_names_opencode_gateway(&entry.value)
        })
}

fn opencode_session_header_reference() -> ModelProviderHeaderReference {
    ModelProviderHeaderReference {
        name: OPENCODE_SESSION_HEADER.to_string(),
        value: ModelProviderHeaderValue::DynamicVariable(ModelProviderHeaderVariable::SessionId),
        redacted_hint: "session variable".to_string(),
    }
}

/// Adds the session header to a v2 Provider whose address names a gateway that
/// requires it. Returns whether the record changed.
fn add_session_header_to_model_provider(profile: &mut ModelProviderProfile) -> bool {
    if !model_provider_names_opencode_gateway(profile) {
        return false;
    }
    if profile
        .headers
        .iter()
        .any(|header| header.name.eq_ignore_ascii_case(OPENCODE_SESSION_HEADER))
    {
        return false;
    }
    profile.headers.push(opencode_session_header_reference());
    profile.revision = profile.revision.saturating_add(1);
    profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
    true
}

/// Adds the session header to a legacy Provider's `headerOverrideJson`.
///
/// A malformed override is left alone rather than replaced: other rows the
/// user hand-edited live in that object, and rewriting it would drop them.
fn add_session_header_to_legacy_provider(profile: &mut ProviderProfile) -> bool {
    if !legacy_provider_names_opencode_gateway(profile) {
        return false;
    }
    let mut headers = match provider_option(profile, LEGACY_HEADER_OVERRIDE_OPTION_KEY) {
        Some(raw) => match serde_json::from_str::<serde_json::Value>(&raw) {
            Ok(serde_json::Value::Object(headers)) => headers,
            _ => return false,
        },
        None => serde_json::Map::new(),
    };
    if headers
        .keys()
        .any(|name| name.eq_ignore_ascii_case(OPENCODE_SESSION_HEADER))
    {
        return false;
    }
    headers.insert(
        OPENCODE_SESSION_HEADER.to_string(),
        serde_json::json!({ "variable": ModelProviderHeaderVariable::SessionId.id() }),
    );
    let value = serde_json::Value::Object(headers).to_string();
    if let Some(entry) = profile
        .provider_options
        .entries
        .iter_mut()
        .find(|entry| entry.key == LEGACY_HEADER_OVERRIDE_OPTION_KEY)
    {
        entry.value = value;
    } else {
        profile
            .provider_options
            .entries
            .push(ProviderBindingMetadata {
                key: LEGACY_HEADER_OVERRIDE_OPTION_KEY.to_string(),
                value,
            });
    }
    profile.updated_at_ms = unix_timestamp_ms().max(profile.updated_at_ms.saturating_add(1));
    true
}

/// Gives existing OpenCode Providers the session header their gateway needs.
///
/// The preset now declares it at creation, but a Provider created before that
/// rule, imported from another client, or edited into an OpenCode address still
/// has no row. The gateway refuses those requests with a failure that names
/// nothing the user configured, so the repair belongs in the record rather than
/// in one surface's creation path. Both records are repaired: the legacy row
/// the Agent editor shows and the v2 row every projection reads.
pub(crate) fn migrate_opencode_session_header(
    conn: &mut Connection,
    applied: &mut Vec<String>,
) -> VibexResult<()> {
    const VERSION: i64 = 87;
    const NAME: &str = "opencode_session_header";
    if migration_applied(conn, VERSION)? {
        return Ok(());
    }
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .map_err(storage_err(
            "opencode_session_header_transaction_failed",
            "failed to start the OpenCode session header migration",
        ))?;
    for mut profile in ModelProviderProfileRepository::list(&transaction)? {
        let expected_revision = profile.revision;
        if add_session_header_to_model_provider(&mut profile) {
            ModelProviderProfileRepository::update(&transaction, &profile, expected_revision)?;
        }
    }
    for mut profile in list_legacy_provider_profiles(&transaction)? {
        if add_session_header_to_legacy_provider(&mut profile) {
            ProviderProfileRepository::update(&transaction, &profile)?;
        }
    }
    transaction
        .execute(
            "INSERT INTO schema_migrations (version, name, applied_at_ms) VALUES (?1, ?2, ?3)",
            params![VERSION, NAME, unix_timestamp_ms()],
        )
        .map_err(storage_err(
            "migration_record_failed",
            "failed to record the OpenCode session header migration",
        ))?;
    transaction.commit().map_err(storage_err(
        "opencode_session_header_commit_failed",
        "failed to commit the OpenCode session header migration",
    ))?;
    applied.push(format!("{VERSION}:{NAME}"));
    Ok(())
}

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, Transaction, params};
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
    ModelProviderHeaderValue, ModelProviderProfile, ModelProviderProfileId,
    ModelProviderProfileStatus, ModelProviderProxyPolicy, ProjectionDescriptorMatch,
    ProjectionEvidenceState, ProjectionSecretReference, ProjectionVerificationState,
    ProviderConfiguredModel, ProviderModelWireApi, ProviderProfile, ProviderProfileId,
    ProviderProfileStatus, ProviderSecretKind, ProviderSecretSetupState, TransportKind, VibexError,
    VibexResult, WIRE_PROTOCOL_ANTHROPIC_MESSAGES, WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
    WIRE_PROTOCOL_OPENAI_RESPONSES, builtin_agent_definitions,
};

use super::{
    AgentManagedInstallationRepository, ProviderSecretReferenceRepository, enum_from_db_sql,
    enum_to_db, json_from_db_sql, json_to_db, map_provider_profile_without_secrets, parse_id_sql,
    storage_err,
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
                deleted_at_ms
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
                deleted_at_ms
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

    pub fn list(conn: &Connection) -> VibexResult<Vec<ModelProviderProfile>> {
        let mut stmt = conn
            .prepare(
                "
                SELECT model_provider_profile_id, legacy_provider_profile_id,
                    display_name, vendor_hint, endpoints_json, proxy_policy_json,
                    credentials_json, configured_models_json, default_model_id,
                    headers_json, status, revision, created_at_ms, updated_at_ms,
                    deleted_at_ms
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
                    deleted_at_ms = ?14
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
                resource_policy_json, revision, created_at_ms, updated_at_ms,
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
                resource_policy_json, revision, created_at_ms, updated_at_ms,
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
                    resource_policy_json, revision, created_at_ms, updated_at_ms,
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
                    revision = ?14,
                    updated_at_ms = ?15,
                    deleted_at_ms = ?16
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
        let transaction = conn.unchecked_transaction().map_err(storage_err(
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
                    created_at_ms, updated_at_ms, deleted_at_ms
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
        let transaction = conn.unchecked_transaction().map_err(storage_err(
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
                    binding.model_provider_profile_id.as_str(),
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
            Self::sync_legacy_profile(conn, &profile)?;
            if before.is_none() {
                changed += 1;
            }
        }
        Ok(changed)
    }

    pub fn sync_legacy_profile(
        conn: &Connection,
        legacy: &ProviderProfile,
    ) -> VibexResult<LegacyProviderProjectionRecords> {
        let transaction = conn.unchecked_transaction().map_err(storage_err(
            "provider_projection_backfill_transaction_failed",
            "failed to start provider projection compatibility transaction",
        ))?;
        let records = map_legacy_projection_records(&transaction, legacy)?;
        let model_provider = upsert_legacy_model_provider(&transaction, records.model_provider)?;
        let agent_runtime = upsert_legacy_agent_runtime(&transaction, records.agent_runtime)?;
        let mut binding = records.binding;
        binding.model_provider_profile_id = model_provider.id.clone();
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

    pub fn mark_legacy_deleted(
        conn: &Connection,
        legacy_id: &ProviderProfileId,
        deleted_at_ms: i64,
    ) -> VibexResult<()> {
        let transaction = conn.unchecked_transaction().map_err(storage_err(
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
            deleted_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15)
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
            revision, created_at_ms, updated_at_ms, deleted_at_ms
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
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
                created_at_ms, updated_at_ms, deleted_at_ms
            ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
            ",
            params![
                binding.id.as_str(),
                binding
                    .legacy_provider_profile_id
                    .as_ref()
                    .map(ProviderProfileId::as_str),
                binding.agent_id.as_str(),
                binding.runtime_profile_id.as_str(),
                binding.model_provider_profile_id.as_str(),
                binding.projection_descriptor_id.as_str(),
                json_to_db(&binding.projection_overrides)?,
                binding.projection_fingerprint,
                enum_to_db(&binding.status)?,
                json_to_db(&binding.verification)?,
                binding.revision,
                binding.created_at_ms,
                binding.updated_at_ms,
                binding.deleted_at_ms,
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
        revision: row.get(11)?,
        created_at_ms: row.get(12)?,
        updated_at_ms: row.get(13)?,
        deleted_at_ms: row.get(14)?,
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
        model_provider_profile_id: parse_id_sql(row.get(4)?, ModelProviderProfileId::parse)?,
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
    let sql = format!(
        "
        SELECT agent_model_provider_binding_id, legacy_provider_profile_id,
            agent_id, agent_runtime_profile_id, model_provider_profile_id,
            projection_descriptor_id, projection_overrides_json,
            projection_fingerprint, status, verification_json, revision,
            created_at_ms, updated_at_ms, deleted_at_ms
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
        stmt.query_map(
            params![value],
            map_agent_model_provider_binding_without_models,
        ),
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
        vendor_hint: provider_option(legacy, "codexModelProviderId")
            .or_else(|| legacy.account_alias.clone())
            .or_else(|| Some(legacy.kind.to_string())),
        endpoints: legacy
            .base_url
            .as_deref()
            .filter(|url| !url.trim().is_empty())
            .map(|url| {
                vec![ModelProviderEndpoint {
                    id: "api".to_string(),
                    kind: ModelProviderEndpointKind::Api,
                    url: url.trim().to_string(),
                }]
            })
            .unwrap_or_default(),
        proxy_policy: ModelProviderProxyPolicy::InheritSystem,
        headers: legacy_headers(legacy, &credentials),
        credentials,
        configured_models: legacy_model_catalog(legacy),
        default_model_id: legacy.default_model.clone(),
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
        model_provider_profile_id: model_provider_id,
        projection_descriptor_id: resolution.descriptor.id.clone(),
        projection_overrides: AgentProviderProjectionOverrides::default(),
        configured_models,
        projection_fingerprint: None,
        status: if resolution.match_kind == ProjectionDescriptorMatch::Conservative {
            AgentModelProviderBindingStatus::Unverified
        } else {
            AgentModelProviderBindingStatus::Ready
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
    let (adapter, adapter_version, agent_version, runtime_dependencies, source) =
        match (agent, managed_version) {
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
            ("claude", None)
                if looks_managed_adapter_command(command, args, "claude-agent-acp") =>
            {
                (
                    "claude-agent-acp",
                    Some("0.64.2".to_string()),
                    None,
                    BTreeMap::new(),
                    AgentVersionSource::Managed,
                )
            }
            ("codex", None) if looks_managed_adapter_command(command, args, "codex-acp") => (
                "codex-acp",
                Some("1.1.9".to_string()),
                Some("0.146.0".to_string()),
                BTreeMap::from([("@openai/codex".to_string(), "0.146.0".to_string())]),
                AgentVersionSource::Managed,
            ),
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

fn upsert_legacy_model_provider(
    conn: &Connection,
    mut candidate: ModelProviderProfile,
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
                binding.model_provider_profile_id.as_str(),
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
                status,
                credential,
                revision: secret.updated_at_ms.max(1),
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
        });
    }
    models
        .into_iter()
        .map(|model| ModelProviderCatalogEntry {
            id: model.id,
            display_name: model.display_name,
            enabled: model.enabled,
            metadata: Vec::new(),
        })
        .collect()
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
        });
    }
    models
        .into_iter()
        .enumerate()
        .map(|(index, model)| {
            let wire_protocol_id = legacy_wire_protocol(profile, model.wire_api);
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
                agent_model_id: model.id,
                wire_protocol_id,
                sdk_adapter_id,
                deployment: None,
                enabled: model.enabled,
                process_scoped,
            })
        })
        .collect()
}

fn legacy_headers(
    profile: &ProviderProfile,
    credentials: &[ModelProviderCredentialReference],
) -> Vec<ModelProviderHeaderReference> {
    let Some(raw) = provider_option(profile, "headerOverrideJson") else {
        return Vec::new();
    };
    let Some(object) = serde_json::from_str::<serde_json::Value>(&raw)
        .ok()
        .and_then(|value| value.as_object().cloned())
    else {
        return Vec::new();
    };
    object
        .into_iter()
        .filter_map(|(name, value)| {
            let value = value.as_str()?.to_string();
            let sensitive = looks_sensitive(&name) || looks_sensitive(&value);
            let header_value = if sensitive {
                let credential_id = credentials
                    .first()
                    .map(|credential| credential.id.clone())?;
                ModelProviderHeaderValue::SecretReference(credential_id)
            } else {
                ModelProviderHeaderValue::NonSecretLiteral(value)
            };
            Some(ModelProviderHeaderReference {
                name,
                value: header_value,
                redacted_hint: if sensitive {
                    "Secret reference".to_string()
                } else {
                    "non-secret header".to_string()
                },
            })
        })
        .collect()
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
) -> String {
    match wire_api {
        Some(ProviderModelWireApi::OpenaiResponses) => WIRE_PROTOCOL_OPENAI_RESPONSES,
        Some(ProviderModelWireApi::OpenaiChatCompletions) => WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS,
        Some(ProviderModelWireApi::AnthropicMessages) => WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
        None if profile.agent_id.as_str() == "claude" => WIRE_PROTOCOL_ANTHROPIC_MESSAGES,
        None if profile.agent_id.as_str() == "opencode" => {
            let value = provider_option(profile, "wireApi")
                .unwrap_or_else(|| "responses".to_string())
                .to_ascii_lowercase()
                .replace(['-', ' '], "_");
            if value.contains("anthropic") || value == "messages" {
                WIRE_PROTOCOL_ANTHROPIC_MESSAGES
            } else if value.contains("response") {
                WIRE_PROTOCOL_OPENAI_RESPONSES
            } else {
                WIRE_PROTOCOL_OPENAI_CHAT_COMPLETIONS
            }
        }
        None => WIRE_PROTOCOL_OPENAI_RESPONSES,
    }
    .to_string()
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

fn legacy_model_provider_id(id: &ProviderProfileId) -> VibexResult<ModelProviderProfileId> {
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

fn looks_sensitive(value: &str) -> bool {
    let normalized = value.to_ascii_lowercase();
    [
        "key",
        "token",
        "secret",
        "password",
        "authorization",
        "cookie",
    ]
    .iter()
    .any(|needle| normalized.contains(needle))
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
        ModelProviderEndpoint, ProviderConfiguredModel, ProviderKind, ProviderModelWireApi,
        ProviderSecretBackend, ProviderSecretReference, RequestId,
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
            "0.65.0",
            claude_command,
            claude_args.clone(),
        );
        let claude_profile = ProviderProfile::local_default(ProviderKind::Claude);
        let claude_identity =
            legacy_runtime_identity(&conn, &claude_profile, claude_command, &claude_args).unwrap();
        assert_eq!(claude_identity.adapter_version.as_deref(), Some("0.65.0"));
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
            "1.1.13",
            codex_command,
            codex_args.clone(),
        );
        let codex_profile = ProviderProfile::local_default(ProviderKind::Codex);
        let codex_identity =
            legacy_runtime_identity(&conn, &codex_profile, codex_command, &codex_args).unwrap();
        assert_eq!(codex_identity.adapter_version.as_deref(), Some("1.1.13"));
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
    fn migration_37_backfills_v36_profiles_idempotently() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        schema_through(&mut conn, 36);

        let mut profile = ProviderProfile::local_default(ProviderKind::Claude);
        profile.status = ProviderProfileStatus::Enabled;
        profile.base_url = Some("https://api.example.invalid".to_string());
        profile.default_model = Some("claude-test".to_string());
        profile.configured_models = vec![ProviderConfiguredModel {
            id: "claude-test".to_string(),
            display_name: None,
            enabled: true,
            wire_api: None,
        }];
        profile.secrets = vec![available_secret(profile.id.clone())];
        ProviderProfileRepository::insert(&conn, &profile).unwrap();

        assert_eq!(
            apply_migrations(&mut conn).unwrap(),
            vec![
                "37:agent_provider_projection_platform",
                "38:agent_runtime_provider_probe_evidence",
                "39:agent_runtime_option_snapshots",
                "40:agent_auth_catalog_snapshots",
                "41:agent_managed_installations",
            ]
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
    fn fresh_migration_seeds_and_projects_local_defaults() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();

        apply_migrations(&mut conn).unwrap();

        let profiles = ProviderProfileRepository::list(&conn).unwrap();
        assert_eq!(profiles.len(), 3);
        for profile in profiles {
            assert!(
                ModelProviderProfileRepository::get_by_legacy_profile(&conn, &profile.id)
                    .unwrap()
                    .is_some()
            );
            assert!(
                AgentRuntimeProfileRepository::get_by_legacy_profile(&conn, &profile.id)
                    .unwrap()
                    .is_some()
            );
            assert!(
                AgentModelProviderBindingRepository::get_by_legacy_profile(&conn, &profile.id)
                    .unwrap()
                    .is_some()
            );
        }
    }

    #[test]
    fn legacy_repository_reads_are_safe_inside_a_transaction() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "foreign_keys", "ON").unwrap();
        apply_migrations(&mut conn).unwrap();
        let profile_id =
            ProviderProfileId::parse(ProviderKind::Codex.local_default_profile_id().to_string())
                .unwrap();

        let transaction = conn.transaction().unwrap();
        let profile = ProviderProfileRepository::get(&transaction, &profile_id).unwrap();

        assert!(profile.is_some());
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
            }],
            proxy_policy: ModelProviderProxyPolicy::InheritSystem,
            credentials: Vec::new(),
            configured_models: vec![ModelProviderCatalogEntry {
                id: "shared-model".to_string(),
                display_name: None,
                enabled: true,
                metadata: Vec::new(),
            }],
            default_model_id: Some("shared-model".to_string()),
            headers: Vec::new(),
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
            records.binding.model_provider_profile_id = shared.id.clone();
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
}

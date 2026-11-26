//! Cross-instance session clone export/import and native state artifact
//! migration.
//!
//! Exporting captures a single-project snapshot of one or more ACP sessions:
//! durable timeline entries (normalized so interrupted turns settle cleanly),
//! the session's `SessionRuntimeSelection` + non-secret Provider Profile hint,
//! and a bounded native session artifact (`Claude`, `Codex`, `OpenCode`) when
//! available.
//!
//! Importing materializes new sessions on the target runtime with **zero ACP
//! subprocesses spawned at import time**. When a native artifact is present, it
//! is written to the target runtime's native state home under a freshly minted
//! `native_session_id` with `cwd` / `sessionId` rewritten to the target
//! workspace root, and an `Inactive` `RuntimeBinding` marked with
//! [`CLONED_SESSION_RESUME_IDENTITY`] (`last_context_sequence = max_sequence`)
//! is seeded so the first turn resumes natively to preserve upstream LLM Prompt
//! Cache prefixes (falling back to `ContextBridgeService` automatically if the
//! adapter reports `NotFound` or `Unsupported`).

use std::collections::HashSet;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, OptionalExtension, params};
use serde_json::{Value, json};
use uuid::Uuid;
use vibex_config_switch::workspace_projection_key;
use vibex_core::{
    AgentId, AgentSession, AgentSessionState, BindingState, CLONED_SESSION_RESUME_IDENTITY,
    CloneProviderProfileHint, ClonedTimelineEntry, ExportedCloneSession,
    ImportedCloneSessionMapping, MAX_CLONE_TIMELINE_ITEMS_PER_SESSION,
    MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION, NativeSessionCloneArtifact, NativeStateHomeId,
    OpenCodeClonedMessage, OpenCodeClonedPart, ProviderProfile, ProviderProfileId,
    RuntimeAuthSource, RuntimeBinding, RuntimeBindingId, SessionCloneBundle,
    SessionCloneExportRequest, SessionCloneImportRequest, SessionCloneImportResult,
    SessionRuntimeConfigState, SessionRuntimeSelection, SessionTransport, TimelinePayload,
    ToolCallStatus, TransportKind, VibexError, VibexResult, VibexSessionId, default_acp_adapter_id,
    normalize_agent_session_title, unix_timestamp_ms,
};
use vibex_db::{
    AgentAuthContextRepository, AgentSessionRuntimeRepository, ModelProviderProfileRepository,
    ProviderProfileRepository, RuntimeBindingRepository, SessionCloneRepository, SessionRepository,
    WorkspaceRepository,
};

use crate::AgentManager;

const CLAUDE_AGENT_ID: &str = "claude";
const CODEX_AGENT_ID: &str = "codex";
const OPENCODE_AGENT_ID: &str = "opencode";

impl AgentManager {
    /// Exports a bounded, single-project session clone bundle.
    pub async fn export_session_clone_bundle(
        &self,
        request: SessionCloneExportRequest,
    ) -> VibexResult<SessionCloneBundle> {
        request.validate()?;
        let db_path = self.database_path().to_path_buf();
        tokio::task::spawn_blocking(move || export_bundle_blocking(&db_path, request))
            .await
            .map_err(|error| {
                VibexError::process(
                    "session_clone_export_join_failed",
                    "session clone export worker task failed",
                )
                .with_diagnostic("error", error.to_string())
            })?
    }

    /// Imports a session clone bundle into `target_workspace_root` without
    /// spawning any ACP subprocesses, announcing each newly created session and
    /// its timeline items to live subscribers.
    pub async fn import_session_clone_bundle(
        &self,
        request: SessionCloneImportRequest,
    ) -> VibexResult<SessionCloneImportResult> {
        request.validate()?;
        let db_path = self.database_path().to_path_buf();
        let imported =
            tokio::task::spawn_blocking(move || import_bundle_blocking(&db_path, request))
                .await
                .map_err(|error| {
                    VibexError::process(
                        "session_clone_import_join_failed",
                        "session clone import worker task failed",
                    )
                    .with_diagnostic("error", error.to_string())
                })??;

        let conn = self.open_migrated()?;
        for (mapping, items) in &imported.sessions_with_items {
            self.publish_root_session_update(&conn, mapping.target_session.clone());
            for item in items {
                let _ = self.publish_timeline_item(item.clone());
            }
        }

        Ok(imported.result)
    }
}

struct BlockingImportOutcome {
    result: SessionCloneImportResult,
    sessions_with_items: Vec<(ImportedCloneSessionMapping, Vec<vibex_core::TimelineItem>)>,
}

fn export_bundle_blocking(
    db_path: &Path,
    request: SessionCloneExportRequest,
) -> VibexResult<SessionCloneBundle> {
    let mut conn = vibex_db::open_database(db_path)?;
    vibex_db::apply_migrations(&mut conn)?;

    let mut seen_ids = HashSet::new();
    let mut source_project_id = None;
    let mut source_workspace_root = None;
    let mut exported_sessions = Vec::with_capacity(request.session_ids.len());

    for session_id in &request.session_ids {
        if !seen_ids.insert(session_id.clone()) {
            continue;
        }
        let session = SessionRepository::get(&conn, session_id)?.ok_or_else(|| {
            VibexError::validation(
                "session_not_found",
                "Selected session for cloning was not found",
            )
            .with_diagnostic("sessionId", session_id.as_str())
        })?;
        if session.deleted_at_ms.is_some() {
            return Err(VibexError::validation(
                "session_not_found",
                "Deleted sessions cannot be cloned",
            )
            .with_diagnostic("sessionId", session_id.as_str()));
        }
        if session.transport != SessionTransport::Acp {
            return Err(VibexError::validation(
                "session_clone_terminal_transport_unsupported",
                "Terminal-transport sessions cannot be cloned across instances",
            )
            .with_diagnostic("sessionId", session_id.as_str()));
        }
        match &source_project_id {
            None => {
                source_project_id = Some(session.project_id.clone());
                source_workspace_root = Some(session.workspace_root.clone());
            }
            Some(expected_project_id) if expected_project_id != &session.project_id => {
                return Err(VibexError::validation(
                    "session_clone_cross_project_forbidden",
                    "Cross-instance cloning must stay within a single source project",
                )
                .with_diagnostic("expectedProjectId", expected_project_id.as_str())
                .with_diagnostic("actualProjectId", session.project_id.as_str()));
            }
            Some(_) => {}
        }

        let runtime_selection = resolve_session_runtime_selection_for_export(&conn, &session)?;
        let (provider_profile, provider_hint) =
            resolve_provider_hint_for_export(&conn, runtime_selection.as_ref())?;

        let raw_timeline = SessionCloneRepository::export_timeline_entries(
            &conn,
            &session.id,
            MAX_CLONE_TIMELINE_ITEMS_PER_SESSION,
        )?;
        let timeline_items = normalize_exported_timeline(raw_timeline);

        let (native_artifact, native_artifact_omitted) = if request.include_native_artifacts {
            export_session_native_artifact(&conn, db_path, &session, provider_profile.as_ref())
        } else {
            (None, false)
        };

        exported_sessions.push(ExportedCloneSession {
            source_session_id: session.id,
            title: session.title,
            agent_id: session.agent_id,
            safety: session.safety,
            tools: session.tools,
            created_at_ms: session.created_at_ms,
            updated_at_ms: session.updated_at_ms,
            last_message_at_ms: session.last_message_at_ms,
            archived_at_ms: session.archived_at_ms,
            runtime_selection,
            provider_hint,
            timeline_items,
            native_artifact,
            native_artifact_omitted,
        });
    }

    let source_project_id = source_project_id.ok_or_else(|| {
        VibexError::validation(
            "session_clone_empty_selection",
            "At least one session must be selected for cloning",
        )
    })?;
    let source_workspace_root = source_workspace_root.unwrap_or_default();

    Ok(SessionCloneBundle {
        source_project_id,
        source_workspace_root,
        sessions: exported_sessions,
    })
}

fn import_bundle_blocking(
    db_path: &Path,
    request: SessionCloneImportRequest,
) -> VibexResult<BlockingImportOutcome> {
    let mut conn = vibex_db::open_database(db_path)?;
    vibex_db::apply_migrations(&mut conn)?;

    let target_dir = PathBuf::from(request.target_workspace_root.trim());
    if !target_dir.exists() {
        fs::create_dir_all(&target_dir).map_err(|error| {
            VibexError::storage(
                "session_clone_target_workspace_create_failed",
                "Failed to create target workspace directory on target instance",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }
    let (project, workspace) =
        WorkspaceRepository::ensure(&conn, &target_dir, request.workspace_mode)?;
    let target_workspace_root = workspace.root_path.clone();
    let source_workspace_root = request.source_workspace_root.trim().to_string();

    let mut imported_sessions = Vec::with_capacity(request.sessions.len());
    let mut sessions_with_items = Vec::with_capacity(request.sessions.len());

    for entry in request.sessions {
        let exported = entry.session;
        let new_session_id = VibexSessionId::new();
        let now = unix_timestamp_ms();
        let title = normalize_agent_session_title(&exported.title)
            .unwrap_or_else(|| format!("{} session", exported.agent_id.as_str()));

        let normalized_selection = normalize_mapped_selection_for_target(
            &conn,
            &exported.agent_id,
            entry.mapped_runtime_selection,
        )?;

        let target_profile = normalized_selection
            .as_ref()
            .and_then(|sel| sel.auth_source.provider_profile_id())
            .and_then(|profile_id| {
                ProviderProfileRepository::get(&conn, profile_id)
                    .ok()
                    .flatten()
            });

        let timeline_items = normalize_exported_timeline(exported.timeline_items);
        let max_sequence = timeline_items.len() as i64;

        let seeded_binding = if let Some(artifact) = exported.native_artifact.as_ref() {
            match import_native_session_artifact(
                db_path,
                &exported.agent_id,
                artifact,
                &source_workspace_root,
                &target_workspace_root,
                target_profile.as_ref(),
            ) {
                Ok(Some(new_native_session_id)) => build_seeded_clone_binding(
                    &conn,
                    &new_session_id,
                    &exported.agent_id,
                    normalized_selection.as_ref(),
                    new_native_session_id,
                    max_sequence,
                    now,
                )?,
                Ok(None) => None,
                Err(error) => {
                    tracing::warn!(
                        session_id = new_session_id.as_str(),
                        agent_id = exported.agent_id.as_str(),
                        error_code = error.code.as_str(),
                        "native session artifact import failed; session will fall back to ContextBridgeService"
                    );
                    None
                }
            }
        } else {
            None
        };

        let native_state_restorable = seeded_binding.is_some();
        let created_at_ms = if exported.created_at_ms > 0 {
            exported.created_at_ms
        } else {
            now
        };
        let updated_at_ms = if exported.updated_at_ms > 0 {
            exported.updated_at_ms
        } else {
            now
        };
        let last_message_at_ms = if exported.last_message_at_ms > 0 {
            exported.last_message_at_ms
        } else {
            updated_at_ms
        };

        let target_session = AgentSession {
            id: new_session_id,
            title,
            project_id: project.id.clone(),
            workspace_id: workspace.id.clone(),
            workspace_root: target_workspace_root.clone(),
            workspace_mode: workspace.mode,
            agent_id: exported.agent_id,
            transport: SessionTransport::Acp,
            terminal: None,
            tools: exported.tools,
            state: AgentSessionState::Idle,
            safety: exported.safety,
            created_at_ms,
            updated_at_ms,
            last_message_at_ms,
            archived_at_ms: exported.archived_at_ms,
            deleted_at_ms: None,
        };

        let inserted_items = SessionCloneRepository::insert_cloned_session(
            &mut conn,
            &target_session,
            &timeline_items,
            normalized_selection.as_ref(),
            seeded_binding.as_ref(),
        )?;

        let mapping = ImportedCloneSessionMapping {
            source_session_id: exported.source_session_id,
            target_session,
            mapped_runtime_selection: normalized_selection,
            native_state_restorable,
        };
        imported_sessions.push(mapping.clone());
        sessions_with_items.push((mapping, inserted_items));
    }

    Ok(BlockingImportOutcome {
        result: SessionCloneImportResult {
            target_project_id: project.id,
            target_workspace_id: workspace.id,
            target_workspace_root,
            imported_sessions,
        },
        sessions_with_items,
    })
}

fn resolve_session_runtime_selection_for_export(
    conn: &Connection,
    session: &AgentSession,
) -> VibexResult<Option<SessionRuntimeSelection>> {
    if let Some(state) = AgentSessionRuntimeRepository::get_runtime_state(conn, &session.id)? {
        if let Some(effective) = state.effective_runtime_selection {
            return Ok(Some(effective));
        }
        if let Some(desired) = state.desired_runtime_selection {
            return Ok(Some(desired));
        }
    }
    SessionCloneRepository::get_runtime_hint(conn, &session.id)
}

fn resolve_provider_hint_for_export(
    conn: &Connection,
    selection: Option<&SessionRuntimeSelection>,
) -> VibexResult<(Option<ProviderProfile>, Option<CloneProviderProfileHint>)> {
    let Some(profile_id) = selection.and_then(|sel| sel.auth_source.provider_profile_id()) else {
        return Ok((None, None));
    };
    let Some(profile) = ProviderProfileRepository::get(conn, profile_id)? else {
        return Ok((None, None));
    };
    let model_provider = ModelProviderProfileRepository::get_by_legacy_profile(conn, profile_id)?;

    let (display_name, base_url, protocol, configured_models) = if let Some(shared) = model_provider
    {
        let first_endpoint = shared.endpoints.first();
        let base_url = first_endpoint
            .map(|ep| ep.url.clone())
            .or_else(|| profile.base_url.clone());
        let protocol = first_endpoint.and_then(|ep| ep.wire_protocol_id.clone());
        let mut models: Vec<String> = shared
            .configured_models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.id.clone())
            .collect();
        for legacy_model in &profile.configured_models {
            if legacy_model.enabled && !models.contains(&legacy_model.id) {
                models.push(legacy_model.id.clone());
            }
        }
        (shared.display_name, base_url, protocol, models)
    } else {
        let models = profile
            .configured_models
            .iter()
            .filter(|m| m.enabled)
            .map(|m| m.id.clone())
            .collect();
        (
            profile.display_name.clone(),
            profile.base_url.clone(),
            None,
            models,
        )
    };

    let hint = CloneProviderProfileHint {
        provider_profile_id: profile.id.clone(),
        display_name,
        base_url,
        protocol,
        configured_models,
    };
    Ok((Some(profile), Some(hint)))
}

fn normalize_mapped_selection_for_target(
    conn: &Connection,
    session_agent_id: &AgentId,
    selection: Option<SessionRuntimeSelection>,
) -> VibexResult<Option<SessionRuntimeSelection>> {
    let Some(mut selection) = selection else {
        return Ok(None);
    };
    match &selection.auth_source {
        RuntimeAuthSource::AgentAccount { .. } => {
            let context = AgentAuthContextRepository::ensure_default(conn, &selection.agent_id)?;
            selection.auth_source = RuntimeAuthSource::agent_account(context.id);
            Ok(Some(selection))
        }
        RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } => {
            if ProviderProfileRepository::get(conn, provider_profile_id)?.is_some() {
                Ok(Some(selection))
            } else {
                // If the caller supplied a ModelProviderProfile ID by mistake or
                // the profile was not cloned, try resolving a matching legacy
                // binding for this Agent before dropping the hint.
                if let Some(resolved_id) = find_legacy_profile_for_model_provider_id(
                    conn,
                    session_agent_id,
                    provider_profile_id.as_str(),
                )? {
                    selection.auth_source = RuntimeAuthSource::provider_profile(resolved_id);
                    Ok(Some(selection))
                } else {
                    Ok(None)
                }
            }
        }
        RuntimeAuthSource::ModelRoute { model_route_id } => {
            if vibex_db::ModelRouteRepository::get(conn, model_route_id)?.is_some() {
                Ok(Some(selection))
            } else {
                Ok(None)
            }
        }
    }
}

fn find_legacy_profile_for_model_provider_id(
    conn: &Connection,
    agent_id: &AgentId,
    candidate_id: &str,
) -> VibexResult<Option<ProviderProfileId>> {
    let raw: Option<String> = conn
        .query_row(
            "
            SELECT legacy_provider_profile_id
            FROM agent_model_provider_bindings
            WHERE agent_id = ?1
              AND model_provider_profile_id = ?2
              AND deleted_at_ms IS NULL
              AND legacy_provider_profile_id IS NOT NULL
            LIMIT 1
            ",
            params![agent_id.as_str(), candidate_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|error| {
            VibexError::storage(
                "session_clone_binding_lookup_failed",
                "failed to resolve agent model provider binding for cloned session",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    raw.map(ProviderProfileId::parse).transpose()
}

fn build_seeded_clone_binding(
    conn: &Connection,
    session_id: &VibexSessionId,
    agent_id: &AgentId,
    mapped_selection: Option<&SessionRuntimeSelection>,
    native_session_id: String,
    max_sequence: i64,
    now_ms: i64,
) -> VibexResult<Option<RuntimeBinding>> {
    let (auth_source, auth_source_revision) = match mapped_selection {
        Some(selection) if selection.agent_id == *agent_id => {
            match resolve_auth_source_revision(conn, &selection.auth_source)? {
                Some(rev) => (selection.auth_source.clone(), rev),
                None => return Ok(None),
            }
        }
        _ => {
            let context = AgentAuthContextRepository::ensure_default(conn, agent_id)?;
            let rev = context.revision.max(0);
            (RuntimeAuthSource::agent_account(context.id), rev)
        }
    };

    Ok(Some(RuntimeBinding {
        binding_id: RuntimeBindingId::new(),
        session_id: session_id.clone(),
        agent_id: agent_id.clone(),
        transport_kind: TransportKind::Acp,
        auth_source,
        auth_source_revision,
        adapter_id: default_acp_adapter_id(agent_id),
        adapter_version: "cloned".to_string(),
        adapter_compatibility_identity: format!("{}:cloned", agent_id.as_str()),
        native_session_id: Some(native_session_id),
        native_state_home_id: NativeStateHomeId::new(),
        provider_resume_identity: Some(CLONED_SESSION_RESUME_IDENTITY.to_string()),
        process_spawn_fingerprint: "cloned".to_string(),
        session_runtime_config_state: SessionRuntimeConfigState::default(),
        capability_snapshot: None,
        restore_compatibility_key: None,
        last_context_sequence: max_sequence.max(0),
        last_summary_sequence: 0,
        context_bridge_version: crate::CONTEXT_BRIDGE_VERSION,
        activation_generation: 0,
        binding_state: BindingState::Inactive,
        created_by_switch_id: None,
        created_at_ms: now_ms,
        updated_at_ms: now_ms,
    }))
}

fn resolve_auth_source_revision(
    conn: &Connection,
    auth_source: &RuntimeAuthSource,
) -> VibexResult<Option<i64>> {
    match auth_source {
        RuntimeAuthSource::ProviderProfile {
            provider_profile_id,
        } => Ok(ProviderProfileRepository::get(conn, provider_profile_id)?
            .map(|profile| profile.updated_at_ms.max(0))),
        RuntimeAuthSource::AgentAccount { auth_context_id } => Ok(
            AgentAuthContextRepository::get_by_id(conn, auth_context_id)?
                .map(|ctx| ctx.revision.max(0)),
        ),
        RuntimeAuthSource::ModelRoute { model_route_id } => {
            Ok(vibex_db::ModelRouteRepository::get(conn, model_route_id)?
                .map(|route| route.revision.max(0)))
        }
    }
}

fn normalize_exported_timeline(entries: Vec<ClonedTimelineEntry>) -> Vec<ClonedTimelineEntry> {
    entries
        .into_iter()
        .filter_map(|mut entry| {
            match &mut entry.payload {
                // Ephemeral streaming deltas are superseded by settled messages;
                // drop any stray trailing delta so the cloned timeline is clean.
                TimelinePayload::AgentMessageDelta(_) => return None,
                TimelinePayload::AgentMessage(msg) => {
                    msg.is_final = true;
                }
                TimelinePayload::Reasoning(reasoning) => {
                    reasoning.is_final = true;
                }
                TimelinePayload::ToolCall(call) => {
                    if matches!(
                        call.status,
                        ToolCallStatus::Started | ToolCallStatus::Progress
                    ) {
                        call.status = ToolCallStatus::Failed;
                    }
                }
                _ => {}
            }
            Some(entry)
        })
        .collect()
}

fn export_session_native_artifact(
    conn: &Connection,
    db_path: &Path,
    session: &AgentSession,
    provider_profile: Option<&ProviderProfile>,
) -> (Option<NativeSessionCloneArtifact>, bool) {
    let Ok(bindings) = RuntimeBindingRepository::list_by_session(conn, &session.id) else {
        return (None, false);
    };
    let latest_native_id = select_latest_native_session_id(&bindings, &session.agent_id);
    let Some(native_session_id) = latest_native_id else {
        return (None, false);
    };

    let artifact_opt = match session.agent_id.as_str() {
        CLAUDE_AGENT_ID => export_claude_transcript(&native_session_id, provider_profile),
        CODEX_AGENT_ID => export_codex_rollout(
            db_path,
            &session.id,
            &native_session_id,
            Path::new(&session.workspace_root),
            provider_profile,
        ),
        OPENCODE_AGENT_ID => export_opencode_session(&native_session_id),
        _ => None,
    };

    match artifact_opt {
        Some(artifact) if artifact.approx_byte_len() <= MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION => {
            (Some(artifact), false)
        }
        Some(_) => (None, true),
        None => (None, false),
    }
}

fn select_latest_native_session_id(
    bindings: &[RuntimeBinding],
    agent_id: &AgentId,
) -> Option<String> {
    bindings
        .iter()
        .filter(|binding| {
            binding.agent_id == *agent_id
                && binding.transport_kind == TransportKind::Acp
                && binding
                    .native_session_id
                    .as_deref()
                    .is_some_and(|id| !id.trim().is_empty())
        })
        .max_by(|left, right| {
            let left_active = matches!(
                left.binding_state,
                BindingState::Current | BindingState::Preparing
            );
            let right_active = matches!(
                right.binding_state,
                BindingState::Current | BindingState::Preparing
            );
            left_active
                .cmp(&right_active)
                .then(left.last_context_sequence.cmp(&right.last_context_sequence))
                .then(left.updated_at_ms.cmp(&right.updated_at_ms))
        })
        .and_then(|binding| binding.native_session_id.clone())
}

fn is_safe_native_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 160
        && !value.contains("..")
        && !value.contains(['/', '\\', ':', '\0'])
}

fn profile_literal_env(profile: Option<&ProviderProfile>, key: &str) -> Option<PathBuf> {
    let profile = profile?;
    let raw = profile
        .provider_options
        .entries
        .iter()
        .find(|entry| entry.key == "acp_config_json")
        .and_then(|entry| serde_json::from_str::<Value>(&entry.value).ok())?;
    raw.get("env")?
        .as_array()?
        .iter()
        .find(|item| item.get("key").and_then(Value::as_str) == Some(key))
        .and_then(|item| item.get("value").and_then(Value::as_str))
        .map(str::trim)
        .filter(|val| !val.is_empty())
        .map(PathBuf::from)
}

fn claude_home_candidates(profile: Option<&ProviderProfile>) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    if let Some(from_profile) = profile_literal_env(profile, "CLAUDE_CONFIG_DIR") {
        homes.push(from_profile);
    }
    if let Some(env_dir) = std::env::var_os("CLAUDE_CONFIG_DIR").filter(|v| !v.is_empty()) {
        let path = PathBuf::from(env_dir);
        if !homes.contains(&path) {
            homes.push(path);
        }
    }
    if let Some(home) = dirs::home_dir() {
        let default_dir = home.join(".claude");
        if !homes.contains(&default_dir) {
            homes.push(default_dir);
        }
    }
    homes
}

fn claude_project_slug(workspace_root: &str) -> String {
    let trimmed = workspace_root.trim();
    let mut slug = String::with_capacity(trimmed.len().max(1));
    for ch in trimmed.chars() {
        if ch.is_ascii_alphanumeric() || ch == '-' {
            slug.push(ch);
        } else {
            slug.push('-');
        }
    }
    if slug.is_empty() {
        "-workspace".to_string()
    } else {
        slug
    }
}

fn export_claude_transcript(
    native_session_id: &str,
    profile: Option<&ProviderProfile>,
) -> Option<NativeSessionCloneArtifact> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let file_name = format!("{native_session_id}.jsonl");
    for home in claude_home_candidates(profile) {
        let Ok(projects) = fs::read_dir(home.join("projects")) else {
            continue;
        };
        for entry in projects.flatten() {
            let candidate = entry.path().join(&file_name);
            if candidate.is_file() {
                let metadata = fs::metadata(&candidate).ok()?;
                if metadata.len() as usize > MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION {
                    return Some(NativeSessionCloneArtifact::ClaudeTranscript {
                        jsonl_utf8: "x".repeat(MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION + 1),
                    });
                }
                let jsonl_utf8 = fs::read_to_string(&candidate).ok()?;
                if !jsonl_utf8.trim().is_empty() {
                    return Some(NativeSessionCloneArtifact::ClaudeTranscript { jsonl_utf8 });
                }
            }
        }
    }
    None
}

fn codex_home_candidates_for_export(
    db_path: &Path,
    session_id: &VibexSessionId,
    workspace_root: &Path,
    profile: Option<&ProviderProfile>,
) -> Vec<PathBuf> {
    let mut homes = Vec::new();
    let mut push_unique = |path: PathBuf| {
        if !homes.contains(&path) {
            homes.push(path);
        }
    };

    if let Some(runtime_parent) = db_path.parent() {
        let runtime_dir = runtime_parent.join("runtime");
        if let Some(profile) = profile {
            push_unique(
                runtime_dir
                    .join("codex-profiles")
                    .join(profile.id.as_str())
                    .join(workspace_projection_key(workspace_root)),
            );
            if let Ok(entries) =
                fs::read_dir(runtime_dir.join("codex-profiles").join(profile.id.as_str()))
            {
                for entry in entries.flatten() {
                    if entry.path().is_dir() {
                        push_unique(entry.path());
                    }
                }
            }
            push_unique(
                runtime_dir
                    .join("codex-runtime-homes")
                    .join(session_id.as_str())
                    .join(profile.id.as_str()),
            );
        }
    }
    if let Some(from_profile) = profile_literal_env(profile, "CODEX_HOME") {
        push_unique(from_profile);
    }
    if let Some(env_dir) = std::env::var_os("CODEX_HOME").filter(|v| !v.is_empty()) {
        push_unique(PathBuf::from(env_dir));
    }
    if let Some(home) = dirs::home_dir() {
        push_unique(home.join(".codex"));
    }
    homes
}

fn find_codex_rollout_in_home(
    codex_home: &Path,
    native_session_id: &str,
) -> Option<(PathBuf, String)> {
    let sessions_root = codex_home.join("sessions");
    let mut stack = vec![(sessions_root.clone(), 0usize)];
    while let Some((dir, depth)) = stack.pop() {
        if depth > 6 {
            continue;
        }
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                stack.push((path, depth + 1));
            } else if path.is_file()
                && path.extension().and_then(|ext| ext.to_str()) == Some("jsonl")
            {
                let Some(name) = path.file_name().and_then(|n| n.to_str()) else {
                    continue;
                };
                if name.starts_with("rollout-") && name.contains(native_session_id) {
                    let rel_dir = path
                        .parent()
                        .and_then(|parent| parent.strip_prefix(&sessions_root).ok())
                        .map(|rel| {
                            rel.components()
                                .filter_map(|c| c.as_os_str().to_str())
                                .collect::<Vec<_>>()
                                .join("/")
                        })
                        .filter(|s| !s.is_empty())
                        .unwrap_or_else(|| "cloned".to_string());
                    return Some((path, rel_dir));
                }
            }
        }
    }
    None
}

fn lookup_codex_thread_title(codex_home: &Path, native_session_id: &str) -> Option<String> {
    let index_path = codex_home.join("session_index.jsonl");
    let content = fs::read_to_string(index_path).ok()?;
    let mut found = None;
    for line in content.lines() {
        let Ok(val) = serde_json::from_str::<Value>(line) else {
            continue;
        };
        if val.get("id").and_then(Value::as_str) == Some(native_session_id)
            && let Some(title) = val.get("thread_name").and_then(Value::as_str)
            && !title.trim().is_empty()
        {
            found = Some(title.to_string());
        }
    }
    found
}

fn export_codex_rollout(
    db_path: &Path,
    session_id: &VibexSessionId,
    native_session_id: &str,
    workspace_root: &Path,
    profile: Option<&ProviderProfile>,
) -> Option<NativeSessionCloneArtifact> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    for home in codex_home_candidates_for_export(db_path, session_id, workspace_root, profile) {
        let Some((rollout_path, relative_date_dir)) =
            find_codex_rollout_in_home(&home, native_session_id)
        else {
            continue;
        };
        let metadata = fs::metadata(&rollout_path).ok()?;
        if metadata.len() as usize > MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION {
            return Some(NativeSessionCloneArtifact::CodexRollout {
                relative_date_dir,
                rollout_jsonl_utf8: "x".repeat(MAX_NATIVE_ARTIFACT_BYTES_PER_SESSION + 1),
                thread_title: None,
            });
        }
        let rollout_jsonl_utf8 = fs::read_to_string(&rollout_path).ok()?;
        if rollout_jsonl_utf8.trim().is_empty() {
            continue;
        }
        let thread_title = lookup_codex_thread_title(&home, native_session_id);
        return Some(NativeSessionCloneArtifact::CodexRollout {
            relative_date_dir,
            rollout_jsonl_utf8,
            thread_title,
        });
    }
    None
}

fn opencode_database_path() -> Option<PathBuf> {
    if let Some(dir) = std::env::var_os("OPENCODE_DATA_DIR").filter(|v| !v.is_empty()) {
        let root = PathBuf::from(dir);
        return Some(if root.is_file() {
            root
        } else {
            root.join("opencode.db")
        });
    }
    if let Some(xdg) = std::env::var_os("XDG_DATA_HOME").filter(|v| !v.is_empty()) {
        return Some(PathBuf::from(xdg).join("opencode").join("opencode.db"));
    }
    dirs::home_dir().map(|home| {
        home.join(".local")
            .join("share")
            .join("opencode")
            .join("opencode.db")
    })
}

fn export_opencode_session(native_session_id: &str) -> Option<NativeSessionCloneArtifact> {
    if !is_safe_native_id(native_session_id) {
        return None;
    }
    let db_path = opencode_database_path()?;
    if !db_path.is_file() {
        return None;
    }
    let conn = Connection::open_with_flags(
        &db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .ok()?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(2));

    let session_row: Option<(String, String, String, i64, i64)> = conn
        .query_row(
            "SELECT COALESCE(slug, ''), COALESCE(title, ''), COALESCE(version, '1'), time_created, time_updated
             FROM session WHERE id = ?1 LIMIT 1",
            params![native_session_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
        )
        .optional()
        .ok()
        .flatten()
        .or_else(|| {
            conn.query_row(
                "SELECT '', COALESCE(title, ''), '1', time_created, time_updated
                 FROM session WHERE id = ?1 LIMIT 1",
                params![native_session_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?, row.get(4)?)),
            )
            .optional()
            .ok()
            .flatten()
        });
    let (slug, title, version, time_created, time_updated) = session_row?;

    let has_message_updated = table_has_column(&conn, "message", "time_updated");
    let msg_sql = if has_message_updated {
        "SELECT id, time_created, time_updated, data FROM message WHERE session_id = ?1 ORDER BY time_created ASC, id ASC"
    } else {
        "SELECT id, time_created, time_created, data FROM message WHERE session_id = ?1 ORDER BY time_created ASC, id ASC"
    };
    let mut msg_stmt = conn.prepare(msg_sql).ok()?;
    let raw_msgs: Vec<(String, i64, i64, String)> = msg_stmt
        .query_map(params![native_session_id], |row| {
            Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?))
        })
        .ok()?
        .collect::<Result<Vec<_>, _>>()
        .ok()?;

    let has_part_updated = table_has_column(&conn, "part", "time_updated");
    let part_sql = if has_part_updated {
        "SELECT id, time_created, time_updated, data FROM part WHERE session_id = ?1 AND message_id = ?2 ORDER BY time_created ASC, id ASC"
    } else {
        "SELECT id, time_created, time_created, data FROM part WHERE session_id = ?1 AND message_id = ?2 ORDER BY time_created ASC, id ASC"
    };
    let mut part_stmt = conn.prepare(part_sql).ok()?;

    let mut messages = Vec::with_capacity(raw_msgs.len());
    for (msg_id, m_created, m_updated, m_data) in raw_msgs {
        let parts: Vec<OpenCodeClonedPart> = part_stmt
            .query_map(params![native_session_id, msg_id], |row| {
                Ok(OpenCodeClonedPart {
                    id: row.get(0)?,
                    time_created: row.get(1)?,
                    time_updated: row.get(2)?,
                    data_json: row.get(3)?,
                })
            })
            .ok()?
            .collect::<Result<Vec<_>, _>>()
            .ok()?;
        messages.push(OpenCodeClonedMessage {
            id: msg_id,
            time_created: m_created,
            time_updated: m_updated,
            data_json: m_data,
            parts,
        });
    }

    Some(NativeSessionCloneArtifact::OpenCodeSession {
        slug,
        title,
        version,
        time_created,
        time_updated,
        messages,
    })
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> bool {
    let Ok(mut stmt) = conn.prepare(&format!("PRAGMA table_info({table})")) else {
        return false;
    };
    let Ok(rows) = stmt.query_map([], |row| row.get::<_, String>(1)) else {
        return false;
    };
    rows.flatten().any(|col| col == column)
}

pub(crate) fn remap_workspace_path_string(
    raw: &str,
    source_workspace_root: &str,
    target_workspace_root: &str,
) -> String {
    let source = source_workspace_root.trim_end_matches(['/', '\\']);
    let target = target_workspace_root.trim_end_matches(['/', '\\']);
    if source.is_empty() || target.is_empty() || source == target {
        return raw.to_string();
    }
    if raw == source {
        return target.to_string();
    }
    if let Some(rest) = raw.strip_prefix(source)
        && (rest.starts_with('/') || rest.starts_with('\\'))
    {
        return format!("{target}{rest}");
    }
    raw.to_string()
}

fn import_native_session_artifact(
    db_path: &Path,
    agent_id: &AgentId,
    artifact: &NativeSessionCloneArtifact,
    source_workspace_root: &str,
    target_workspace_root: &str,
    target_profile: Option<&ProviderProfile>,
) -> VibexResult<Option<String>> {
    match (agent_id.as_str(), artifact) {
        (CLAUDE_AGENT_ID, NativeSessionCloneArtifact::ClaudeTranscript { jsonl_utf8 }) => {
            import_claude_transcript(
                jsonl_utf8,
                source_workspace_root,
                target_workspace_root,
                target_profile,
            )
            .map(Some)
        }
        (
            CODEX_AGENT_ID,
            NativeSessionCloneArtifact::CodexRollout {
                relative_date_dir,
                rollout_jsonl_utf8,
                thread_title,
            },
        ) => import_codex_rollout(
            db_path,
            relative_date_dir,
            rollout_jsonl_utf8,
            thread_title.as_deref(),
            source_workspace_root,
            target_workspace_root,
            target_profile,
        )
        .map(Some),
        (
            OPENCODE_AGENT_ID,
            NativeSessionCloneArtifact::OpenCodeSession {
                slug,
                title,
                version,
                time_created,
                time_updated,
                messages,
            },
        ) => import_opencode_session(
            slug,
            title,
            version,
            *time_created,
            *time_updated,
            messages,
            source_workspace_root,
            target_workspace_root,
        ),
        _ => Ok(None),
    }
}

fn import_claude_transcript(
    jsonl_utf8: &str,
    source_workspace_root: &str,
    target_workspace_root: &str,
    target_profile: Option<&ProviderProfile>,
) -> VibexResult<String> {
    let claude_home = claude_home_candidates(target_profile)
        .into_iter()
        .next()
        .ok_or_else(|| {
            VibexError::storage(
                "session_clone_claude_home_unavailable",
                "Claude configuration directory could not be resolved on target instance",
            )
        })?;
    let new_session_id = Uuid::new_v4().to_string();
    let mut rewritten = String::with_capacity(jsonl_utf8.len() + 256);

    for line in jsonl_utf8.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(mut value) = serde_json::from_str::<Value>(trimmed) {
            if let Some(obj) = value.as_object_mut() {
                if obj.contains_key("sessionId") {
                    obj.insert(
                        "sessionId".to_string(),
                        Value::String(new_session_id.clone()),
                    );
                }
                if let Some(cwd) = obj.get("cwd").and_then(Value::as_str) {
                    let mapped = remap_workspace_path_string(
                        cwd,
                        source_workspace_root,
                        target_workspace_root,
                    );
                    obj.insert("cwd".to_string(), Value::String(mapped));
                } else if matches!(
                    obj.get("type").and_then(Value::as_str),
                    Some("user" | "assistant" | "system")
                ) {
                    obj.insert(
                        "cwd".to_string(),
                        Value::String(target_workspace_root.to_string()),
                    );
                }
            }
            if let Ok(serialized) = serde_json::to_string(&value) {
                rewritten.push_str(&serialized);
                rewritten.push('\n');
                continue;
            }
        }
        rewritten.push_str(trimmed);
        rewritten.push('\n');
    }

    let project_dir = claude_home
        .join("projects")
        .join(claude_project_slug(target_workspace_root));
    ensure_private_dir(&project_dir)?;
    let target_file = project_dir.join(format!("{new_session_id}.jsonl"));
    write_private_file_atomic(&target_file, rewritten.as_bytes())?;
    Ok(new_session_id)
}

fn sanitize_relative_date_dir(relative_date_dir: &str) -> PathBuf {
    let mut out = PathBuf::new();
    for segment in relative_date_dir.split(['/', '\\']) {
        let seg = segment.trim();
        if !seg.is_empty()
            && seg != "."
            && seg != ".."
            && seg
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            out.push(seg);
        }
    }
    if out.as_os_str().is_empty() {
        out.push("cloned");
    }
    out
}

fn import_codex_rollout(
    db_path: &Path,
    relative_date_dir: &str,
    rollout_jsonl_utf8: &str,
    thread_title: Option<&str>,
    source_workspace_root: &str,
    target_workspace_root: &str,
    target_profile: Option<&ProviderProfile>,
) -> VibexResult<String> {
    let new_session_id = Uuid::new_v4().to_string();
    let mut rewritten = String::with_capacity(rollout_jsonl_utf8.len() + 256);

    for line in rollout_jsonl_utf8.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        if let Ok(mut value) = serde_json::from_str::<Value>(trimmed) {
            let record_type = value
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(payload) = value.get_mut("payload").and_then(Value::as_object_mut) {
                if record_type == "session_meta" {
                    payload.insert("id".to_string(), Value::String(new_session_id.clone()));
                    payload.insert(
                        "cwd".to_string(),
                        Value::String(target_workspace_root.to_string()),
                    );
                } else if let Some(cwd) = payload.get("cwd").and_then(Value::as_str) {
                    let mapped = remap_workspace_path_string(
                        cwd,
                        source_workspace_root,
                        target_workspace_root,
                    );
                    payload.insert("cwd".to_string(), Value::String(mapped));
                }
            }
            if let Ok(serialized) = serde_json::to_string(&value) {
                rewritten.push_str(&serialized);
                rewritten.push('\n');
                continue;
            }
        }
        rewritten.push_str(trimmed);
        rewritten.push('\n');
    }

    let mut target_homes = Vec::new();
    if let Some(profile) = target_profile
        && let Some(parent) = db_path.parent()
    {
        target_homes.push(
            parent
                .join("runtime")
                .join("codex-profiles")
                .join(profile.id.as_str())
                .join(workspace_projection_key(Path::new(target_workspace_root))),
        );
    }
    if let Some(from_profile) = profile_literal_env(target_profile, "CODEX_HOME")
        && !target_homes.contains(&from_profile)
    {
        target_homes.push(from_profile);
    }
    let default_codex_home = std::env::var_os("CODEX_HOME")
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
        .or_else(|| dirs::home_dir().map(|h| h.join(".codex")));
    if let Some(default_home) = default_codex_home
        && !target_homes.contains(&default_home)
    {
        target_homes.push(default_home);
    }

    if target_homes.is_empty() {
        return Err(VibexError::storage(
            "session_clone_codex_home_unavailable",
            "Codex home directory could not be resolved on target instance",
        ));
    }

    let rel_dir = sanitize_relative_date_dir(relative_date_dir);
    let file_name = format!(
        "rollout-cloned-{}-{new_session_id}.jsonl",
        unix_timestamp_ms()
    );
    let updated_iso = chrono::Utc::now().to_rfc3339();

    for home in &target_homes {
        let session_dir = home.join("sessions").join(&rel_dir);
        ensure_private_dir(&session_dir)?;
        write_private_file_atomic(&session_dir.join(&file_name), rewritten.as_bytes())?;
        if let Some(title) = thread_title.filter(|t| !t.trim().is_empty()) {
            let index_entry = json!({
                "id": new_session_id,
                "thread_name": title,
                "updated_at": updated_iso,
            });
            if let Ok(mut line) = serde_json::to_string(&index_entry) {
                line.push('\n');
                let _ = append_private_line(&home.join("session_index.jsonl"), line.as_bytes());
            }
        }
    }

    Ok(new_session_id)
}

#[allow(clippy::too_many_arguments)]
fn import_opencode_session(
    slug: &str,
    title: &str,
    version: &str,
    time_created: i64,
    time_updated: i64,
    messages: &[OpenCodeClonedMessage],
    source_workspace_root: &str,
    target_workspace_root: &str,
) -> VibexResult<Option<String>> {
    let Some(db_path) = opencode_database_path() else {
        return Ok(None);
    };
    // Only write into an existing OpenCode SQLite database so we never create a
    // partial schema that conflicts with OpenCode's own migrations.
    if !db_path.is_file() {
        return Ok(None);
    }
    let mut conn = Connection::open(&db_path).map_err(|error| {
        VibexError::storage(
            "session_clone_opencode_open_failed",
            "Failed to open OpenCode database on target instance",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    let _ = conn.busy_timeout(std::time::Duration::from_secs(5));
    if !table_has_column(&conn, "session", "id") || !table_has_column(&conn, "message", "id") {
        return Ok(None);
    }

    let new_session_id = format!("ses_{}", Uuid::new_v4().simple());
    let has_project_id = table_has_column(&conn, "session", "project_id");
    let has_slug = table_has_column(&conn, "session", "slug");
    let has_version = table_has_column(&conn, "session", "version");
    let has_msg_updated = table_has_column(&conn, "message", "time_updated");
    let has_part_updated = table_has_column(&conn, "part", "time_updated");

    let tx = conn.transaction().map_err(|error| {
        VibexError::storage(
            "session_clone_opencode_tx_failed",
            "Failed to start OpenCode session import transaction",
        )
        .with_diagnostic("error", error.to_string())
    })?;

    let project_id = if has_project_id {
        resolve_or_insert_opencode_project(&tx, target_workspace_root, time_created, time_updated)?
    } else {
        None
    };

    if has_project_id && has_slug && has_version {
        let Some(project_id) = project_id.as_deref() else {
            return Ok(None);
        };
        tx.execute(
            "INSERT INTO session (id, project_id, parent_id, slug, directory, title, version, time_created, time_updated)
             VALUES (?1, ?2, NULL, ?3, ?4, ?5, ?6, ?7, ?8)",
            params![
                new_session_id,
                project_id,
                slug,
                target_workspace_root,
                title,
                version,
                time_created,
                time_updated
            ],
        )
        .map_err(|error| {
            VibexError::storage(
                "session_clone_opencode_insert_failed",
                "Failed to insert cloned OpenCode session",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    } else {
        tx.execute(
            "INSERT INTO session (id, parent_id, directory, title, time_created, time_updated)
             VALUES (?1, NULL, ?2, ?3, ?4, ?5)",
            params![
                new_session_id,
                target_workspace_root,
                title,
                time_created,
                time_updated
            ],
        )
        .map_err(|error| {
            VibexError::storage(
                "session_clone_opencode_insert_failed",
                "Failed to insert cloned OpenCode session",
            )
            .with_diagnostic("error", error.to_string())
        })?;
    }

    for msg in messages {
        let new_msg_id = format!("msg_{}", Uuid::new_v4().simple());
        let rewritten_msg_data = rewrite_opencode_json_blob(
            &msg.data_json,
            &new_session_id,
            Some(&new_msg_id),
            None,
            source_workspace_root,
            target_workspace_root,
        );
        if has_msg_updated {
            tx.execute(
                "INSERT INTO message (id, session_id, time_created, time_updated, data)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    new_msg_id,
                    new_session_id,
                    msg.time_created,
                    msg.time_updated,
                    rewritten_msg_data
                ],
            )
            .map_err(|error| {
                VibexError::storage(
                    "session_clone_opencode_msg_insert_failed",
                    "Failed to insert cloned OpenCode message",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        } else {
            tx.execute(
                "INSERT INTO message (id, session_id, time_created, data)
                 VALUES (?1, ?2, ?3, ?4)",
                params![
                    new_msg_id,
                    new_session_id,
                    msg.time_created,
                    rewritten_msg_data
                ],
            )
            .map_err(|error| {
                VibexError::storage(
                    "session_clone_opencode_msg_insert_failed",
                    "Failed to insert cloned OpenCode message",
                )
                .with_diagnostic("error", error.to_string())
            })?;
        }

        for part in &msg.parts {
            let new_part_id = format!("prt_{}", Uuid::new_v4().simple());
            let rewritten_part_data = rewrite_opencode_json_blob(
                &part.data_json,
                &new_session_id,
                Some(&new_msg_id),
                Some(&new_part_id),
                source_workspace_root,
                target_workspace_root,
            );
            if has_part_updated {
                tx.execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, time_updated, data)
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
                    params![
                        new_part_id,
                        new_msg_id,
                        new_session_id,
                        part.time_created,
                        part.time_updated,
                        rewritten_part_data
                    ],
                )
                .map_err(|error| {
                    VibexError::storage(
                        "session_clone_opencode_part_insert_failed",
                        "Failed to insert cloned OpenCode message part",
                    )
                    .with_diagnostic("error", error.to_string())
                })?;
            } else {
                tx.execute(
                    "INSERT INTO part (id, message_id, session_id, time_created, data)
                     VALUES (?1, ?2, ?3, ?4, ?5)",
                    params![
                        new_part_id,
                        new_msg_id,
                        new_session_id,
                        part.time_created,
                        rewritten_part_data
                    ],
                )
                .map_err(|error| {
                    VibexError::storage(
                        "session_clone_opencode_part_insert_failed",
                        "Failed to insert cloned OpenCode message part",
                    )
                    .with_diagnostic("error", error.to_string())
                })?;
            }
        }
    }

    tx.commit().map_err(|error| {
        VibexError::storage(
            "session_clone_opencode_commit_failed",
            "Failed to commit cloned OpenCode session",
        )
        .with_diagnostic("error", error.to_string())
    })?;

    Ok(Some(new_session_id))
}

fn resolve_or_insert_opencode_project(
    conn: &Connection,
    target_workspace_root: &str,
    time_created: i64,
    time_updated: i64,
) -> VibexResult<Option<String>> {
    if !table_has_column(conn, "project", "id") {
        return Ok(None);
    }
    if let Ok(Some(existing)) = conn
        .query_row(
            "SELECT id FROM project WHERE worktree = ?1 LIMIT 1",
            params![target_workspace_root],
            |row| row.get::<_, String>(0),
        )
        .optional()
    {
        return Ok(Some(existing));
    }
    let project_id = format!("prj_{}", Uuid::new_v4().simple());
    let inserted = conn.execute(
        "INSERT INTO project (id, worktree, time_created, time_updated, sandboxes)
         VALUES (?1, ?2, ?3, ?4, '[]')",
        params![
            project_id,
            target_workspace_root,
            time_created,
            time_updated
        ],
    );
    match inserted {
        Ok(_) => Ok(Some(project_id)),
        Err(_) => Ok(None),
    }
}

fn rewrite_opencode_json_blob(
    raw_json: &str,
    new_session_id: &str,
    new_message_id: Option<&str>,
    new_part_id: Option<&str>,
    source_workspace_root: &str,
    target_workspace_root: &str,
) -> String {
    let Ok(mut val) = serde_json::from_str::<Value>(raw_json) else {
        return raw_json.to_string();
    };
    if let Some(obj) = val.as_object_mut() {
        if obj.contains_key("sessionID") {
            obj.insert(
                "sessionID".to_string(),
                Value::String(new_session_id.to_string()),
            );
        }
        if let Some(mid) = new_message_id {
            if new_part_id.is_none() && obj.contains_key("id") {
                obj.insert("id".to_string(), Value::String(mid.to_string()));
            }
            if obj.contains_key("messageID") {
                obj.insert("messageID".to_string(), Value::String(mid.to_string()));
            }
        }
        if let Some(pid) = new_part_id
            && obj.contains_key("id")
        {
            obj.insert("id".to_string(), Value::String(pid.to_string()));
        }
        if let Some(path_obj) = obj.get_mut("path").and_then(Value::as_object_mut) {
            for key in ["cwd", "root"] {
                if let Some(existing) = path_obj.get(key).and_then(Value::as_str) {
                    let mapped = remap_workspace_path_string(
                        existing,
                        source_workspace_root,
                        target_workspace_root,
                    );
                    path_obj.insert(key.to_string(), Value::String(mapped));
                }
            }
        }
    }
    serde_json::to_string(&val).unwrap_or_else(|_| raw_json.to_string())
}

fn ensure_private_dir(path: &Path) -> VibexResult<()> {
    fs::create_dir_all(path).map_err(|error| {
        VibexError::storage(
            "session_clone_native_dir_create_failed",
            "Failed to create native session directory",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = fs::set_permissions(path, fs::Permissions::from_mode(0o700));
    }
    Ok(())
}

fn write_private_file_atomic(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    let parent = path.parent().ok_or_else(|| {
        VibexError::storage(
            "session_clone_native_parent_missing",
            "Native session artifact path has no parent directory",
        )
    })?;
    ensure_private_dir(parent)?;
    let tmp_path = parent.join(format!(
        ".{}.tmp-{}",
        path.file_name()
            .and_then(|n| n.to_str())
            .unwrap_or("artifact"),
        Uuid::new_v4().simple()
    ));
    {
        let mut opts = OpenOptions::new();
        opts.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            opts.mode(0o600);
        }
        let mut file = opts.open(&tmp_path).map_err(|error| {
            VibexError::storage(
                "session_clone_native_write_failed",
                "Failed to stage native session artifact",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        file.write_all(bytes).map_err(|error| {
            let _ = fs::remove_file(&tmp_path);
            VibexError::storage(
                "session_clone_native_write_failed",
                "Failed to write native session artifact",
            )
            .with_diagnostic("error", error.to_string())
        })?;
        let _ = file.sync_all();
    }
    fs::rename(&tmp_path, path).map_err(|error| {
        let _ = fs::remove_file(&tmp_path);
        VibexError::storage(
            "session_clone_native_commit_failed",
            "Failed to commit native session artifact",
        )
        .with_diagnostic("error", error.to_string())
    })
}

fn append_private_line(path: &Path, bytes: &[u8]) -> VibexResult<()> {
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let mut opts = OpenOptions::new();
    opts.create(true).append(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        opts.mode(0o600);
    }
    let mut file = opts.open(path).map_err(|error| {
        VibexError::storage(
            "session_clone_native_append_failed",
            "Failed to append to native session index",
        )
        .with_diagnostic("error", error.to_string())
    })?;
    file.write_all(bytes).map_err(|error| {
        VibexError::storage(
            "session_clone_native_append_failed",
            "Failed to write native session index entry",
        )
        .with_diagnostic("error", error.to_string())
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vibex_core::{
        AcpAdapterId, AgentMessagePayload, AgentSessionSafety, ImportCloneSessionEntry,
        PermissionMode, ProviderConfiguredModel, ProviderKind, ProviderProfileCreateRequest,
        RuntimeModelSelection, SessionToolSettings, TimelineRedactionState, TimelineSource,
        UserMessagePayload, WorkspaceMode,
    };
    use vibex_db::TimelineRepository;

    #[test]
    fn workspace_path_remapping_rewrites_matching_prefixes_safely() {
        assert_eq!(
            remap_workspace_path_string("/src/repo", "/src/repo", "/dst/work"),
            "/dst/work"
        );
        assert_eq!(
            remap_workspace_path_string("/src/repo/crates/core", "/src/repo", "/dst/work"),
            "/dst/work/crates/core"
        );
        assert_eq!(
            remap_workspace_path_string("/src/repo-other/file.rs", "/src/repo", "/dst/work"),
            "/src/repo-other/file.rs"
        );
    }

    #[tokio::test]
    async fn export_and_import_bundle_migrates_timeline_selection_hint_and_codex_artifact() {
        let base_dir = tempfile::tempdir().expect("tempdir");
        let src_db = base_dir.path().join("instance-a").join("vibex.db");
        let dst_db = base_dir.path().join("instance-b").join("vibex.db");
        fs::create_dir_all(src_db.parent().unwrap()).unwrap();
        fs::create_dir_all(dst_db.parent().unwrap()).unwrap();

        let src_ws = base_dir.path().join("ws-a");
        let dst_ws = base_dir.path().join("ws-b");
        fs::create_dir_all(&src_ws).unwrap();
        fs::create_dir_all(&dst_ws).unwrap();

        let manager_a = AgentManager::new(&src_db).expect("manager a");
        let manager_b = AgentManager::new(&dst_db).expect("manager b");

        let codex = AgentId::parse("codex").unwrap();
        let seed_profile = |db: &Path, name: &str| -> ProviderProfileId {
            let conn = vibex_db::open_database(db).unwrap();
            let profile =
                ProviderProfileRepository::from_create_request(ProviderProfileCreateRequest {
                    agent_id: Some(codex.clone()),
                    kind: ProviderKind::Acp,
                    display_name: name.to_string(),
                    account_alias: None,
                    base_url: Some("https://api.example.test/v1".to_string()),
                    default_model: Some("gpt-5.4".to_string()),
                    small_model: None,
                    large_model: None,
                    configured_models: vec![ProviderConfiguredModel {
                        id: "gpt-5.4".to_string(),
                        display_name: Some("GPT-5.4".to_string()),
                        enabled: true,
                        wire_api: None,
                        capabilities: Default::default(),
                    }],
                    reasoning_effort: None,
                    sandbox_defaults: None,
                    network_defaults: None,
                    permission_defaults: None,
                    provider_options: None,
                    secret_references: Vec::new(),
                });
            ProviderProfileRepository::insert(&conn, &profile).unwrap();
            profile.id
        };
        let profile_id_a = seed_profile(&src_db, "Codex Proxy");
        let profile_id_b = seed_profile(&dst_db, "Codex Proxy");

        let created = {
            let conn = manager_a.open_migrated().unwrap();
            let (project, workspace) =
                WorkspaceRepository::ensure(&conn, &src_ws, WorkspaceMode::CurrentCheckout)
                    .unwrap();
            let session = AgentSession {
                id: VibexSessionId::new(),
                title: "Codex Cache Warm Session".to_string(),
                project_id: project.id,
                workspace_id: workspace.id,
                workspace_root: workspace.root_path,
                workspace_mode: WorkspaceMode::CurrentCheckout,
                agent_id: codex.clone(),
                transport: SessionTransport::Acp,
                terminal: None,
                tools: SessionToolSettings::default(),
                state: AgentSessionState::Idle,
                safety: AgentSessionSafety {
                    permission_mode: PermissionMode::WorkspaceWrite,
                    ask_on_risk: true,
                    bypass_all_permissions: false,
                },
                created_at_ms: 1_700_000_000_000,
                updated_at_ms: 1_700_000_050_000,
                last_message_at_ms: 1_700_000_020_000,
                archived_at_ms: None,
                deleted_at_ms: None,
            };
            SessionRepository::insert(&conn, &session).unwrap();
            session
        };

        let source_native_id = "01999999-aaaa-bbbb-cccc-ddddeeeeffff";
        let src_profile_codex_home = src_db
            .parent()
            .unwrap()
            .join("runtime")
            .join("codex-profiles")
            .join(profile_id_a.as_str())
            .join(workspace_projection_key(&src_ws));
        let rollout_dir = src_profile_codex_home
            .join("sessions")
            .join("2026")
            .join("11")
            .join("26");
        fs::create_dir_all(&rollout_dir).unwrap();
        let rollout_content = format!(
            "{{\"timestamp\":\"2026-11-26T00:00:00Z\",\"type\":\"session_meta\",\"payload\":{{\"id\":\"{source_native_id}\",\"cwd\":\"{}\"}}}}\n{{\"timestamp\":\"2026-11-26T00:00:01Z\",\"type\":\"turn_context\",\"payload\":{{\"cwd\":\"{}/sub\",\"model\":\"gpt-5.4\"}}}}\n",
            src_ws.display(),
            src_ws.display()
        );
        fs::write(
            rollout_dir.join(format!("rollout-2026-{source_native_id}.jsonl")),
            rollout_content,
        )
        .unwrap();

        {
            let mut conn_a = vibex_db::open_database(&src_db).unwrap();
            TimelineRepository::append(
                &mut conn_a,
                &created.id,
                TimelineSource::User,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text: "Analyze workspace cache layout".to_string(),
                    ..Default::default()
                }),
                None,
                None,
                TimelineRedactionState::None,
            )
            .unwrap();
            TimelineRepository::append(
                &mut conn_a,
                &created.id,
                TimelineSource::Agent,
                TimelinePayload::AgentMessage(AgentMessagePayload {
                    text: "Prompt cache warm response".to_string(),
                    is_final: false,
                }),
                None,
                None,
                TimelineRedactionState::None,
            )
            .unwrap();

            let selection_a = SessionRuntimeSelection {
                agent_id: codex.clone(),
                auth_source: RuntimeAuthSource::provider_profile(profile_id_a.clone()),
                model: RuntimeModelSelection::explicit("gpt-5.4"),
                reasoning_effort: Some("high".to_string()),
                mode_id: None,
                config_values: BTreeMap::new(),
            };
            SessionCloneRepository::upsert_runtime_hint(&conn_a, &created.id, &selection_a)
                .unwrap();

            let binding_a = RuntimeBinding {
                binding_id: RuntimeBindingId::new(),
                session_id: created.id.clone(),
                agent_id: codex.clone(),
                transport_kind: TransportKind::Acp,
                auth_source: selection_a.auth_source.clone(),
                auth_source_revision: 1,
                adapter_id: AcpAdapterId::parse("codex").unwrap(),
                adapter_version: "0.10.0".to_string(),
                adapter_compatibility_identity: "codex:0.10.0".to_string(),
                native_session_id: Some(source_native_id.to_string()),
                native_state_home_id: NativeStateHomeId::new(),
                provider_resume_identity: Some("acp-session-resume-v1".to_string()),
                process_spawn_fingerprint: "fp".to_string(),
                session_runtime_config_state: SessionRuntimeConfigState::default(),
                capability_snapshot: None,
                restore_compatibility_key: None,
                last_context_sequence: 2,
                last_summary_sequence: 0,
                context_bridge_version: 1,
                activation_generation: 1,
                binding_state: BindingState::Current,
                created_by_switch_id: None,
                created_at_ms: 100,
                updated_at_ms: 200,
            };
            RuntimeBindingRepository::insert(&conn_a, &binding_a).unwrap();
        }

        let bundle = manager_a
            .export_session_clone_bundle(SessionCloneExportRequest {
                session_ids: vec![created.id.clone()],
                include_native_artifacts: true,
            })
            .await
            .expect("export bundle");
        assert_eq!(bundle.sessions.len(), 1);
        let exported = &bundle.sessions[0];
        assert!(exported.provider_hint.is_some());
        assert!(matches!(
            exported.native_artifact,
            Some(NativeSessionCloneArtifact::CodexRollout { .. })
        ));
        assert!(matches!(
            &exported.timeline_items[1].payload,
            TimelinePayload::AgentMessage(msg) if msg.is_final
        ));

        let mapped_selection_b = SessionRuntimeSelection {
            agent_id: codex.clone(),
            auth_source: RuntimeAuthSource::provider_profile(profile_id_b.clone()),
            model: RuntimeModelSelection::explicit("gpt-5.4"),
            reasoning_effort: Some("high".to_string()),
            mode_id: None,
            config_values: BTreeMap::new(),
        };

        let import_result = manager_b
            .import_session_clone_bundle(SessionCloneImportRequest {
                source_workspace_root: bundle.source_workspace_root.clone(),
                target_workspace_root: dst_ws.to_string_lossy().into_owned(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                sessions: vec![ImportCloneSessionEntry {
                    session: exported.clone(),
                    mapped_runtime_selection: Some(mapped_selection_b.clone()),
                }],
            })
            .await
            .expect("import bundle");

        assert_eq!(import_result.imported_sessions.len(), 1);
        let mapping = &import_result.imported_sessions[0];
        assert_ne!(mapping.target_session.id, created.id);
        assert!(mapping.native_state_restorable);

        let conn_b = vibex_db::open_database(&dst_db).unwrap();
        let bindings_b =
            RuntimeBindingRepository::list_by_session(&conn_b, &mapping.target_session.id).unwrap();
        assert_eq!(bindings_b.len(), 1);
        assert_eq!(bindings_b[0].binding_state, BindingState::Inactive);
        assert_eq!(
            bindings_b[0].provider_resume_identity.as_deref(),
            Some(CLONED_SESSION_RESUME_IDENTITY)
        );
        assert_eq!(bindings_b[0].last_context_sequence, 2);
        let new_native_id = bindings_b[0].native_session_id.as_deref().unwrap();
        assert_ne!(new_native_id, source_native_id);

        let dst_profile_codex_home = dst_db
            .parent()
            .unwrap()
            .join("runtime")
            .join("codex-profiles")
            .join(profile_id_b.as_str())
            .join(workspace_projection_key(&dst_ws));
        let (written_rollout, _) =
            find_codex_rollout_in_home(&dst_profile_codex_home, new_native_id)
                .expect("rewritten rollout exists in target codex profile home");
        let rewritten_text = fs::read_to_string(written_rollout).unwrap();
        assert!(rewritten_text.contains(new_native_id));
        assert!(!rewritten_text.contains(source_native_id));
        assert!(rewritten_text.contains(&format!("{}/sub", dst_ws.display())));
    }
}

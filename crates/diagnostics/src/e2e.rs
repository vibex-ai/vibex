use std::collections::BTreeMap;
use std::fs;
use std::future::Future;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::Serialize;
use serde::de::DeserializeOwned;
use vibex_agent::{AgentManager, ScheduledTaskRunner};
use vibex_agent_claude::{
    ClaudeSessionImportPreviewRequest, import_selected_claude_sessions,
    preview_claude_external_sessions,
};
use vibex_agent_codex::{
    CodexSessionImportPreviewRequest, import_selected_codex_sessions,
    preview_codex_external_sessions,
};
use vibex_config_switch::ProviderConfigService;
use vibex_core::{
    AcpAdapterId, AcpProviderProfileCreateRequest, AgentCommandDiscoverRequest,
    AgentCommandExecuteRequest, AgentCommandExecuteStatus, AgentCommandExecutionBehavior,
    AgentCommandSelectionBehavior, AgentCommandSourceKind, AgentCommandTrigger, AgentId,
    AgentSession, AgentSessionSafety, AgentSessionState, BindingState, ErrorCategory,
    FetchTimelineRequest, FileReadRequest, FileSearchRequest, FileTreeRequest, FileWriteRequest,
    NativeStateHomeId, OpenWorkspaceRequest, PromptCreateRequest, PromptKind, PromptScopeKind,
    PromptStatus, ProviderConfiguredModel, ProviderKind, RemoteAgentRequest,
    RemoteAgentSendMessageRequest, RemoteAgentSessionListRequest, RemoteAgentSessionListResponse,
    RemoteAgentTimelineFetchRequest, RemoteAgentTimelineFetchResponse, RemoteAuthProof,
    RemoteClaimPairingCodeRequest, RemoteCreatePairingCodeRequest, RemoteDevicePermissionLevel,
    RemoteEnvelopeStatus, RemoteFileReadRequest, RemoteFileReadResponse, RemoteFileSearchRequest,
    RemoteFileSearchResponse, RemoteFileTreeRequest, RemoteFileTreeResponse,
    RemoteFileWriteRequest, RemoteFileWriteResponse, RemoteGitStatusRequest,
    RemoteGitStatusResponse, RemoteHandshakeResponse, RemoteOperationKind, RemoteRequestEnvelope,
    RemoteResponseEnvelope, RemoteTerminalCreateRequest, RemoteTerminalCreateResponse,
    RemoteTerminalKillRequest, RemoteTerminalKillResponse, RemoteTerminalListRequest,
    RemoteTerminalListResponse, RemoteTerminalSnapshotRequest, RemoteTerminalSnapshotResponse,
    RemoteTerminalWriteRequest, RemoteTerminalWriteResponse, RemoteWorkbenchListWorkspacesRequest,
    RemoteWorkbenchListWorkspacesResponse, RemoteWorkbenchOpenWorkspaceRequest,
    RemoteWorkbenchOpenWorkspaceResponse, RemoteWorkbenchRequest, RuntimeBinding, RuntimeBindingId,
    ScheduledTaskCreateRequest, ScheduledTaskOneShotSchedule, ScheduledTaskRunListRequest,
    ScheduledTaskRunStatus, ScheduledTaskSchedule, SendAgentMessageRequest,
    SessionRuntimeConfigState, SessionRuntimeSelection, SkillCreateRequest, SkillProviderMatrix,
    SkillScopeKind, SkillSourceKind, SkillStatus, TerminalCreateRequest, TerminalWriteRequest,
    TimelineItemKind, TransportKind, VibexError, VibexResult, WorkspaceMode, unix_timestamp_ms,
};
use vibex_db::{
    AgentSessionRuntimeRepository, ProviderProfileRepository, ScheduledTaskRepository,
    SessionRepository, WorkspaceRepository, apply_migrations, open_database,
};
use vibex_remote::{
    RemoteDispatcher, RemoteServiceConfig, RemoteTrustService, RemoteWorkbenchRuntime,
};
use vibex_terminal::TerminalManager;

const E2E_AGENT_PROMPT_SENTINEL: &str = "E2E_AGENT_PROMPT_SENTINEL";
const E2E_FILE_CONTENT_SENTINEL: &str = "E2E_FILE_CONTENT_SENTINEL";
const E2E_TERMINAL_OUTPUT_SENTINEL: &str = "E2E_TERMINAL_OUTPUT_SENTINEL";
const E2E_SCHEDULED_PROMPT_SENTINEL: &str = "E2E_SCHEDULED_PROMPT_SENTINEL";

/// Hermetic ACP stand-in used only with durable test bindings.
#[derive(Debug)]
struct E2eStubAcpProvider;

#[async_trait::async_trait]
impl vibex_agent::AgentProvider for E2eStubAcpProvider {
    fn kind(&self) -> ProviderKind {
        ProviderKind::Acp
    }

    fn capabilities(&self) -> vibex_core::ProviderCapabilities {
        let mut capabilities =
            vibex_core::ProviderCapabilities::conservative(ProviderKind::Acp, "e2e-stub");
        capabilities.slash_commands = true;
        capabilities.skills = true;
        capabilities
    }

    async fn create_session(
        &self,
        request: vibex_agent::ProviderCreateRequest,
    ) -> VibexResult<vibex_agent::ProviderSessionHandle> {
        let now = unix_timestamp_ms();
        Ok(vibex_agent::ProviderSessionHandle {
            binding: vibex_core::ProviderBinding {
                session_id: request.session_id,
                provider_kind: ProviderKind::Acp,
                provider_profile_id: request.provider_profile_id,
                native: vibex_core::ProviderNativeBinding::empty(),
                created_at_ms: now,
                updated_at_ms: now,
            },
            capabilities: self.capabilities(),
        })
    }

    async fn resume_session(
        &self,
        binding: vibex_core::ProviderBinding,
    ) -> VibexResult<vibex_agent::ProviderSessionHandle> {
        Ok(vibex_agent::ProviderSessionHandle {
            binding,
            capabilities: self.capabilities(),
        })
    }

    async fn send_turn(
        &self,
        _handle: vibex_agent::ProviderSessionHandle,
        request: vibex_agent::ProviderTurnRequest,
    ) -> VibexResult<vibex_agent::ProviderTurnResult> {
        Ok(vibex_agent::ProviderTurnResult {
            events: vec![vibex_agent::ProviderEvent::agent(
                vibex_core::TimelinePayload::AgentMessage(vibex_core::AgentMessagePayload {
                    text: format!("e2e-stub reply to: {}", request.text),
                    is_final: true,
                }),
            )],
            binding_update: None,
            completed: true,
        })
    }

    async fn prepare_turn_execution(
        &self,
        _handle: &vibex_agent::ProviderSessionHandle,
        request: &vibex_agent::ProviderTurnRequest,
    ) -> VibexResult<Option<vibex_agent::ProviderTurnExecutionIdentity>> {
        Ok(request.execution_identity.clone())
    }

    async fn discover_commands(
        &self,
        _request: AgentCommandDiscoverRequest,
    ) -> VibexResult<vibex_core::AgentCommandDiscoverResponse> {
        Ok(vibex_core::AgentCommandDiscoverResponse {
            entries: vec![vibex_core::AgentCommandEntry {
                id: "provider:codex:test".to_string(),
                trigger: AgentCommandTrigger::Slash,
                source_kind: AgentCommandSourceKind::Provider,
                label: "/test".to_string(),
                description: Some("E2E stub provider command".to_string()),
                insertion_text: "/test ".to_string(),
                command_name: Some("test".to_string()),
                provider_kind: Some(ProviderKind::Acp),
                prompt_id: None,
                skill_id: None,
                reference_path: None,
                selection_behavior: AgentCommandSelectionBehavior::Insert,
                execution_behavior: AgentCommandExecutionBehavior::ProviderCommand,
                destructive: false,
                metadata: Vec::new(),
            }],
            diagnostics: Vec::new(),
        })
    }

    async fn execute_command(
        &self,
        _handle: vibex_agent::ProviderSessionHandle,
        request: AgentCommandExecuteRequest,
        _turn: vibex_agent::ProviderTurnRequest,
    ) -> VibexResult<vibex_agent::ProviderTurnResult> {
        Ok(vibex_agent::ProviderTurnResult {
            events: vec![vibex_agent::ProviderEvent::agent(
                vibex_core::TimelinePayload::Command(vibex_core::CommandPayload {
                    command: request.command_text.clone(),
                    cwd: None,
                    status: vibex_core::CommandStatus::Completed,
                    exit_code: Some(0),
                    output_summary: Some("e2e stub command output".to_string()),
                    raw_extension: None,
                }),
            )],
            binding_update: None,
            completed: true,
        })
    }
}

/// Builds the AgentManager used by every regression check with the hermetic
/// stub provider registered.
fn e2e_agent_manager(db_path: &Path) -> VibexResult<AgentManager> {
    let config_service = ProviderConfigService::new(db_path.to_path_buf());
    let mut profile = config_service.create_acp_profile(AcpProviderProfileCreateRequest {
        agent_id: Some(AgentId::parse("codex")?),
        display_name: "E2E Codex ACP".to_string(),
        account_alias: None,
        preset_id: Some("codex-acp".to_string()),
        config: None,
    })?;
    profile.default_model = Some("e2e-stub".to_string());
    profile.configured_models = vec![ProviderConfiguredModel {
        id: "e2e-stub".to_string(),
        display_name: Some("E2E Stub".to_string()),
        enabled: true,
        wire_api: None,
        capabilities: Default::default(),
    }];
    let conn = open_database(db_path)?;
    ProviderProfileRepository::update(&conn, &profile)?;

    let mut manager = AgentManager::new(db_path)?;
    manager.register_runtime(
        vibex_core::AgentRuntimeRouteKey {
            agent_id: AgentId::parse("codex")?,
            transport_kind: vibex_core::TransportKind::Acp,
            adapter_id: AcpAdapterId::parse("codex-acp")?,
        },
        Arc::new(E2eStubAcpProvider),
    )?;
    Ok(manager)
}

fn create_e2e_durable_session(
    db_path: &Path,
    workspace_root: &Path,
    title: &str,
) -> VibexResult<(AgentSession, SessionRuntimeSelection)> {
    let mut conn = open_database(db_path)?;
    apply_migrations(&mut conn)?;
    let agent_id = AgentId::parse("codex")?;
    let profile = ProviderProfileRepository::first_enabled_for_agent(&conn, &agent_id)?
        .ok_or_else(|| {
            VibexError::validation("e2e_acp_profile_missing", "E2E ACP Profile was not created")
        })?;
    let (project, workspace) =
        WorkspaceRepository::ensure(&conn, workspace_root, WorkspaceMode::CurrentCheckout)?;
    let now = unix_timestamp_ms();
    let session = AgentSession {
        id: vibex_core::VibexSessionId::new(),
        title: title.to_string(),
        project_id: project.id,
        workspace_id: workspace.id,
        workspace_root: workspace.root_path,
        workspace_mode: workspace.mode,
        agent_id: agent_id.clone(),
        state: AgentSessionState::Idle,
        safety: AgentSessionSafety::workspace_write_ask_on_risk(),
        created_at_ms: now,
        updated_at_ms: now,
        archived_at_ms: None,
        deleted_at_ms: None,
    };
    SessionRepository::insert(&conn, &session)?;
    let selection = SessionRuntimeSelection {
        agent_id: agent_id.clone(),
        provider_profile_id: profile.id.clone(),
        model_id: "e2e-stub".to_string(),
        reasoning_effort: None,
        mode_id: None,
        config_values: Default::default(),
    };
    let mut runtime_config = SessionRuntimeConfigState {
        preferred_model: Some(selection.model_id.clone()),
        effective_model: Some(selection.model_id.clone()),
        ..SessionRuntimeConfigState::default()
    };
    runtime_config.mark_generation_if_converged(0);
    let binding = RuntimeBinding {
        binding_id: RuntimeBindingId::new(),
        session_id: session.id.clone(),
        agent_id,
        transport_kind: TransportKind::Acp,
        provider_profile_id: profile.id,
        adapter_id: AcpAdapterId::parse("codex-acp")?,
        adapter_version: "e2e".to_string(),
        adapter_compatibility_identity: "adapter=codex-acp@e2e".to_string(),
        native_session_id: Some(format!("e2e-{}", session.id.as_str())),
        native_state_home_id: NativeStateHomeId::parse("statehome_e2e")?,
        provider_resume_identity: None,
        process_spawn_fingerprint: "e2e-spawn-fingerprint".to_string(),
        session_runtime_config_state: runtime_config,
        capability_snapshot: None,
        restore_compatibility_key: None,
        profile_revision: profile.updated_at_ms,
        last_context_sequence: 0,
        last_summary_sequence: 0,
        context_bridge_version: vibex_agent::CONTEXT_BRIDGE_VERSION,
        activation_generation: 0,
        binding_state: BindingState::Current,
        created_by_switch_id: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    AgentSessionRuntimeRepository::initialize_runtime_selection(&mut conn, &binding, &selection)?;
    Ok((session, selection))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum E2eRegressionOverallStatus {
    Pass,
    PassWithFollowUps,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum E2eRegressionCheckStatus {
    Pass,
    FollowUp,
    Fail,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum E2eRegressionClassification {
    Blocker,
    FollowUp,
    AcceptableMvpLimit,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct E2eRegressionCheck {
    pub name: String,
    pub status: E2eRegressionCheckStatus,
    pub classification: E2eRegressionClassification,
    pub fixture_size: BTreeMap<String, u64>,
    pub output_count: u64,
    pub notes: String,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct E2eRegressionHarnessResult {
    pub schema_version: u32,
    pub generated_at_ms: i64,
    pub overall_status: E2eRegressionOverallStatus,
    pub checks: Vec<E2eRegressionCheck>,
}

impl E2eRegressionHarnessResult {
    pub fn has_blocker(&self) -> bool {
        self.overall_status == E2eRegressionOverallStatus::Fail
    }
}

pub async fn run_e2e_regression_harness() -> VibexResult<E2eRegressionHarnessResult> {
    let root = e2e_regression_root();
    match fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VibexError::storage(
                "e2e_regression_cleanup_failed",
                "failed to clean previous E2E regression fixture",
            )
            .with_diagnostic("error", err.to_string()));
        }
    }
    fs::create_dir_all(&root).map_err(storage_io("e2e_regression_fixture_create_failed"))?;

    let result = run_e2e_regression_harness_in(&root).await;
    match fs::remove_dir_all(&root) {
        Ok(()) => {}
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
        Err(err) => {
            return Err(VibexError::storage(
                "e2e_regression_cleanup_failed",
                "failed to remove E2E regression fixture root",
            )
            .with_diagnostic("error", err.to_string()));
        }
    }
    result
}

async fn run_e2e_regression_harness_in(root: &Path) -> VibexResult<E2eRegressionHarnessResult> {
    let checks = vec![
        capture_e2e_check("agent_command_protocol", || agent_command_protocol(root)).await,
        capture_e2e_check("remote_web_workbench", || remote_web_workbench(root)).await,
        capture_e2e_check("remote_agent_protocol", || remote_agent_protocol(root)).await,
        capture_e2e_check("scheduled_task_visibility", || {
            scheduled_task_visibility(root)
        })
        .await,
        capture_e2e_check("import_fixture_smoke", || import_fixture_smoke(root)).await,
    ];

    let has_failure = checks
        .iter()
        .any(|check| check.status == E2eRegressionCheckStatus::Fail);
    let has_follow_up = checks
        .iter()
        .any(|check| check.status == E2eRegressionCheckStatus::FollowUp);
    let overall_status = if has_failure {
        E2eRegressionOverallStatus::Fail
    } else if has_follow_up {
        E2eRegressionOverallStatus::PassWithFollowUps
    } else {
        E2eRegressionOverallStatus::Pass
    };

    Ok(E2eRegressionHarnessResult {
        schema_version: 1,
        generated_at_ms: unix_timestamp_ms(),
        overall_status,
        checks,
    })
}

async fn capture_e2e_check<F, Fut>(name: &str, run: F) -> E2eRegressionCheck
where
    F: FnOnce() -> Fut,
    Fut: Future<Output = VibexResult<E2eRegressionCheck>>,
{
    match run().await {
        Ok(check) => check,
        Err(err) => E2eRegressionCheck {
            name: name.to_string(),
            status: E2eRegressionCheckStatus::Fail,
            classification: E2eRegressionClassification::Blocker,
            fixture_size: BTreeMap::new(),
            output_count: 0,
            notes: format!("check failed with {}", err.code),
        },
    }
}

async fn agent_command_protocol(root: &Path) -> VibexResult<E2eRegressionCheck> {
    let db_path = root.join("agent-command-protocol.db");
    let workspace_root = root.join("agent-command-workspace");
    fs::create_dir_all(&workspace_root)
        .map_err(storage_io("e2e_agent_command_workspace_create_failed"))?;

    let config_service = ProviderConfigService::new(db_path.clone());
    let prompt = config_service.create_prompt(PromptCreateRequest {
        display_name: "Review".to_string(),
        kind: PromptKind::SlashCommand,
        status: PromptStatus::Enabled,
        scope_kind: PromptScopeKind::User,
        project_id: None,
        workspace_id: None,
        body: "Review target: {{input}}".to_string(),
        description: Some("E2E slash prompt".to_string()),
        tags: vec!["e2e".to_string(), "slash".to_string()],
    })?;
    let _skill = config_service.create_skill(SkillCreateRequest {
        display_name: "Rust Quality".to_string(),
        source_kind: SkillSourceKind::Manual,
        status: SkillStatus::Enabled,
        scope_kind: SkillScopeKind::User,
        project_id: None,
        workspace_id: None,
        source_uri: None,
        description: Some("E2E local skill".to_string()),
        tags: vec!["e2e".to_string(), "skill".to_string()],
        content_preview: Some("Check Rust quality gates.".to_string()),
        provider_matrix: vec![SkillProviderMatrix {
            provider_kind: ProviderKind::Acp,
            enabled: true,
            updated_at_ms: unix_timestamp_ms(),
        }],
    })?;

    let manager = e2e_agent_manager(&db_path)?;
    let (session, selection) =
        create_e2e_durable_session(&db_path, &workspace_root, "E2E command protocol")?;

    let discovered = manager
        .discover_commands(AgentCommandDiscoverRequest {
            agent_id: Some(selection.agent_id.clone()),
            provider_profile_id: Some(selection.provider_profile_id),
            session_id: Some(session.id.clone()),
            workspace_id: Some(session.workspace_id.clone()),
            trigger: None,
            query: None,
            limit: None,
        })
        .await?;
    let provider_entry_insert_only = discovered.entries.iter().any(|entry| {
        entry.source_kind == AgentCommandSourceKind::Provider
            && entry.label == "/test"
            && entry.selection_behavior == AgentCommandSelectionBehavior::Insert
            && entry.execution_behavior == AgentCommandExecutionBehavior::ProviderCommand
    });
    let prompt_entry = discovered.entries.iter().any(|entry| {
        entry.source_kind == AgentCommandSourceKind::Prompt && entry.label == "/review"
    });
    let skill_entry = discovered.entries.iter().any(|entry| {
        entry.source_kind == AgentCommandSourceKind::Skill && entry.label == "$rust-quality"
    });

    let provider_result = manager
        .execute_command(AgentCommandExecuteRequest {
            session_id: session.id.clone(),
            command_id: Some("provider:codex:test".to_string()),
            trigger: AgentCommandTrigger::Slash,
            source_kind: AgentCommandSourceKind::Provider,
            command_text: "/test inspect".to_string(),
            command_name: Some("test".to_string()),
            arguments: Some("inspect".to_string()),
            prompt_id: None,
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        })
        .await?;
    let prompt_result = manager
        .execute_command(AgentCommandExecuteRequest {
            session_id: session.id.clone(),
            command_id: Some(format!("prompt:{}", prompt.id.as_str())),
            trigger: AgentCommandTrigger::Slash,
            source_kind: AgentCommandSourceKind::Prompt,
            command_text: "/review crates/diagnostics".to_string(),
            command_name: Some("review".to_string()),
            arguments: Some("crates/diagnostics".to_string()),
            prompt_id: Some(prompt.id),
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        })
        .await?;
    let skill_rejected = manager
        .execute_command(AgentCommandExecuteRequest {
            session_id: session.id.clone(),
            command_id: None,
            trigger: AgentCommandTrigger::Dollar,
            source_kind: AgentCommandSourceKind::Skill,
            command_text: "$rust-quality".to_string(),
            command_name: Some("rust-quality".to_string()),
            arguments: None,
            prompt_id: None,
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        })
        .await
        .is_err_and(|err| err.code == "agent_command_source_not_executable");
    let reference_rejected = manager
        .execute_command(AgentCommandExecuteRequest {
            session_id: session.id.clone(),
            command_id: None,
            trigger: AgentCommandTrigger::Mention,
            source_kind: AgentCommandSourceKind::Reference,
            command_text: "@src".to_string(),
            command_name: None,
            arguments: None,
            prompt_id: None,
            attachments: Vec::new(),
            reasoning_effort: None,
            correlation_id: None,
        })
        .await
        .is_err_and(|err| err.code == "agent_command_source_not_executable");

    let provider_completed = provider_result.status == AgentCommandExecuteStatus::Completed
        && provider_result
            .items
            .iter()
            .any(|item| item.kind == TimelineItemKind::Command);
    let prompt_completed = prompt_result.status == AgentCommandExecuteStatus::Completed
        && prompt_result.items.iter().any(|item| match &item.payload {
            vibex_core::TimelinePayload::UserMessage(message) => {
                message.text == "Review target: crates/diagnostics"
            }
            _ => false,
        });
    let status = if provider_entry_insert_only
        && prompt_entry
        && skill_entry
        && provider_completed
        && prompt_completed
        && skill_rejected
        && reference_rejected
    {
        E2eRegressionCheckStatus::Pass
    } else {
        E2eRegressionCheckStatus::Fail
    };

    Ok(E2eRegressionCheck {
        name: "agent_command_protocol".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("discovered_commands", discovered.entries.len() as u64),
            ("executed_commands", 2),
            ("rejected_insert_only_sources", 2),
        ]),
        output_count: discovered.entries.len() as u64
            + provider_result.items.len() as u64
            + prompt_result.items.len() as u64,
        notes:
            "Agent command discovery/execution covered provider, slash prompt, skill, and reference contracts"
                .to_string(),
    })
}

async fn remote_web_workbench(root: &Path) -> VibexResult<E2eRegressionCheck> {
    let db_path = root.join("remote-workbench.db");
    let workspace_root = root.join("remote-workbench-workspace");
    create_remote_workspace_fixture(&workspace_root)?;
    let git_available = Command::new("git").arg("--version").output().is_ok();
    if git_available {
        create_git_fixture(&workspace_root)?;
    }

    let manager = Arc::new(e2e_agent_manager(&db_path)?);
    let dispatcher = RemoteDispatcher::with_agent_and_workbench(
        RemoteServiceConfig::loopback_disabled(),
        manager,
        RemoteWorkbenchRuntime::new(db_path.clone(), TerminalManager::new()),
    );
    let auth = pair_e2e_device(
        &db_path,
        RemoteDevicePermissionLevel::FullControl,
        "E2E Workbench",
    )?;

    let handshake = dispatch_handshake(&dispatcher).await?;
    let opened: RemoteWorkbenchOpenWorkspaceResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::OpenWorkspace(RemoteWorkbenchOpenWorkspaceRequest {
            auth: auth.clone(),
            request: OpenWorkspaceRequest {
                root_path: workspace_root.display().to_string(),
                mode: Some(WorkspaceMode::CurrentCheckout),
            },
        }),
    )
    .await?;
    let workspace_id = opened.summary.workspace.id.clone();
    let listed: RemoteWorkbenchListWorkspacesResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::ListWorkspaces(RemoteWorkbenchListWorkspacesRequest {
            auth: auth.clone(),
        }),
    )
    .await?;
    let tree: RemoteFileTreeResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::FileListTree(RemoteFileTreeRequest {
            auth: auth.clone(),
            request: FileTreeRequest {
                workspace_id: workspace_id.clone(),
                path: None,
                max_depth: Some(3),
                include_hidden: false,
            },
        }),
    )
    .await?;
    let read: RemoteFileReadResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::FileRead(RemoteFileReadRequest {
            auth: auth.clone(),
            request: FileReadRequest {
                workspace_id: workspace_id.clone(),
                path: "src/main.txt".to_string(),
                max_bytes: Some(2048),
            },
        }),
    )
    .await?;
    let search: RemoteFileSearchResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::FileSearch(RemoteFileSearchRequest {
            auth: auth.clone(),
            request: FileSearchRequest {
                workspace_id: workspace_id.clone(),
                query: "E2E_FILE".to_string(),
                include_content: true,
                limit: Some(20),
            },
        }),
    )
    .await?;
    let written: RemoteFileWriteResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::WorkspaceFile,
        RemoteWorkbenchRequest::FileWrite(RemoteFileWriteRequest {
            auth: auth.clone(),
            request: FileWriteRequest {
                workspace_id: workspace_id.clone(),
                path: "src/generated.txt".to_string(),
                content: E2E_FILE_CONTENT_SENTINEL.to_string(),
                create_if_missing: true,
                expected_revision: None,
                encoding: None,
                line_ending: None,
            },
        }),
    )
    .await?;

    let git_changes = if git_available {
        let status: RemoteGitStatusResponse = dispatch_remote(
            &dispatcher,
            RemoteOperationKind::Git,
            RemoteWorkbenchRequest::GitStatus(RemoteGitStatusRequest {
                auth: auth.clone(),
                workspace_id: workspace_id.clone(),
            }),
        )
        .await?;
        status.status.changes.len() as u64
    } else {
        0
    };

    let terminal: RemoteTerminalCreateResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::Terminal,
        RemoteWorkbenchRequest::TerminalCreate(RemoteTerminalCreateRequest {
            auth: auth.clone(),
            request: TerminalCreateRequest {
                workspace_id: workspace_id.clone(),
                title: Some("E2E shell".to_string()),
                shell: Some(e2e_shell()),
                cwd: None,
                rows: 24,
                cols: 80,
            },
        }),
    )
    .await?;
    let _: RemoteTerminalWriteResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::Terminal,
        RemoteWorkbenchRequest::TerminalWrite(RemoteTerminalWriteRequest {
            auth: auth.clone(),
            request: TerminalWriteRequest {
                terminal_id: terminal.terminal.id.clone(),
                data: terminal_sentinel_command(),
            },
        }),
    )
    .await?;
    let snapshot = wait_for_terminal_snapshot(&dispatcher, &auth, &terminal.terminal.id).await?;
    let terminals: RemoteTerminalListResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::Terminal,
        RemoteWorkbenchRequest::TerminalList(RemoteTerminalListRequest {
            auth: auth.clone(),
            workspace_id,
        }),
    )
    .await?;
    let killed: RemoteTerminalKillResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::Terminal,
        RemoteWorkbenchRequest::TerminalKill(RemoteTerminalKillRequest {
            auth,
            terminal_id: terminal.terminal.id,
        }),
    )
    .await?;

    let pass = handshake.capabilities.supports_workspace_files
        && handshake.capabilities.supports_git
        && handshake.capabilities.supports_terminal
        && !listed.workspaces.is_empty()
        && !tree.entries.is_empty()
        && read.file.size_bytes > 0
        && !search.results.is_empty()
        && written.file.size_bytes > 0
        && (!git_available || git_changes >= 2)
        && !terminals.terminals.is_empty()
        && killed.terminal.status == vibex_core::TerminalStatus::Killed;
    let status = if pass {
        if git_available {
            E2eRegressionCheckStatus::Pass
        } else {
            E2eRegressionCheckStatus::FollowUp
        }
    } else {
        E2eRegressionCheckStatus::Fail
    };
    let notes = if git_available {
        "remote dispatcher opened workspace and exercised file, Git, and terminal basics"
            .to_string()
    } else {
        "remote dispatcher exercised workspace/file/terminal basics; Git binary unavailable"
            .to_string()
    };

    Ok(E2eRegressionCheck {
        name: "remote_web_workbench".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("files", 4),
            ("git_changes", if git_available { 2 } else { 0 }),
            ("terminal_sessions", 1),
        ]),
        output_count: listed.workspaces.len() as u64
            + tree.entries.len() as u64
            + search.results.len() as u64
            + git_changes
            + terminals.terminals.len() as u64
            + snapshot.snapshot.chunks.len() as u64,
        notes,
    })
}

async fn remote_agent_protocol(root: &Path) -> VibexResult<E2eRegressionCheck> {
    let db_path = root.join("remote-agent.db");
    let workspace_root = root.join("remote-agent-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("e2e_remote_agent_workspace_failed"))?;

    let manager = Arc::new(e2e_agent_manager(&db_path)?);
    let (session, selection) =
        create_e2e_durable_session(&db_path, &workspace_root, "E2E remote Agent")?;
    let dispatcher =
        RemoteDispatcher::with_agent_manager(RemoteServiceConfig::loopback_disabled(), manager);
    let auth = pair_e2e_device(
        &db_path,
        RemoteDevicePermissionLevel::FullControl,
        "E2E Agent",
    )?;

    let listed: RemoteAgentSessionListResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::AgentSession,
        RemoteAgentRequest::ListSessions(RemoteAgentSessionListRequest {
            auth: auth.clone(),
            include_archived: Some(false),
            timeline_limit: Some(10),
        }),
    )
    .await?;
    let send_rejected = dispatch_remote::<_, vibex_core::RemoteAgentSendMessageResponse>(
        &dispatcher,
        RemoteOperationKind::AgentSession,
        RemoteAgentRequest::SendMessage(RemoteAgentSendMessageRequest {
            auth: auth.clone(),
            request: SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: "e2e-remote-agent-message".to_string(),
                desired_runtime: selection,
                text: E2E_AGENT_PROMPT_SENTINEL.to_string(),
                attachments: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
            },
        }),
    )
    .await
    .is_err_and(|error| error.code == "message_submission_coordinator_unavailable");
    let fetched: RemoteAgentTimelineFetchResponse = dispatch_remote(
        &dispatcher,
        RemoteOperationKind::AgentSession,
        RemoteAgentRequest::FetchTimeline(RemoteAgentTimelineFetchRequest {
            auth,
            request: FetchTimelineRequest {
                session_id: session.id,
                after_sequence: Some(0),
                limit: 100,
            },
        }),
    )
    .await?;

    let status = if listed.sessions.len() == 1 && send_rejected && fetched.page.items.is_empty() {
        E2eRegressionCheckStatus::Pass
    } else {
        E2eRegressionCheckStatus::Fail
    };

    Ok(E2eRegressionCheck {
        name: "remote_agent_protocol".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([("sessions", 1), ("remote_agent_operations", 3)]),
        output_count: listed.sessions.len() as u64 + fetched.page.items.len() as u64,
        notes: "remote list/fetch succeeded while ordinary send failed closed without a durable coordinator"
            .to_string(),
    })
}

async fn scheduled_task_visibility(root: &Path) -> VibexResult<E2eRegressionCheck> {
    let db_path = root.join("scheduled-task.db");
    let workspace_root = root.join("scheduled-task-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("e2e_scheduled_workspace_failed"))?;

    let manager = e2e_agent_manager(&db_path)?;
    let due_at_ms = unix_timestamp_ms();
    {
        let mut conn = open_database(&db_path)?;
        apply_migrations(&mut conn)?;
        ScheduledTaskRepository::create(
            &conn,
            ScheduledTaskCreateRequest {
                title: "E2E scheduled task".to_string(),
                prompt: E2E_SCHEDULED_PROMPT_SENTINEL.to_string(),
                project_id: None,
                workspace_id: None,
                workspace_root: workspace_root.display().to_string(),
                workspace_mode: WorkspaceMode::CurrentCheckout,
                provider_kind: ProviderKind::Codex,
                provider_profile_id: None,
                schedule: ScheduledTaskSchedule::OneShot(ScheduledTaskOneShotSchedule {
                    run_at_ms: due_at_ms,
                }),
                safety: None,
                next_run_at_ms: Some(due_at_ms),
            },
        )?;
    }

    let tick = ScheduledTaskRunner::new(&manager)
        .with_due_limit(4)
        .tick(due_at_ms)
        .await?;
    let conn = open_database(&db_path)?;
    let runs = ScheduledTaskRepository::list_runs(
        &conn,
        ScheduledTaskRunListRequest {
            task_id: None,
            session_id: None,
            status: None,
            limit: Some(20),
        },
    )?;
    let succeeded = runs
        .iter()
        .filter(|run| run.status == ScheduledTaskRunStatus::Succeeded)
        .count() as u64;
    let failed = runs
        .iter()
        .filter(|run| run.status == ScheduledTaskRunStatus::Failed)
        .count() as u64;
    let skipped = runs
        .iter()
        .filter(|run| run.status == ScheduledTaskRunStatus::Skipped)
        .count() as u64;
    let status = if tick.checked == 1 && tick.failed == 1 && failed == 1 && succeeded == 0 {
        E2eRegressionCheckStatus::Pass
    } else {
        E2eRegressionCheckStatus::Fail
    };

    Ok(E2eRegressionCheck {
        name: "scheduled_task_visibility".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("scheduled_tasks", 1),
            ("scheduled_runs", runs.len() as u64),
        ]),
        output_count: succeeded + failed + skipped,
        notes: "scheduler recorded a bounded failure instead of bypassing missing durable runtime composition"
            .to_string(),
    })
}

async fn import_fixture_smoke(root: &Path) -> VibexResult<E2eRegressionCheck> {
    let db_path = root.join("import-fixtures.db");
    let workspace_root = root.join("import-workspace");
    fs::create_dir_all(&workspace_root).map_err(storage_io("e2e_import_workspace_failed"))?;
    let manager = e2e_agent_manager(&db_path)?;

    let codex_preview = preview_codex_external_sessions(CodexSessionImportPreviewRequest {
        paths: vec![
            workspace_root_path()
                .join("crates")
                .join("agent-codex")
                .join("tests")
                .join("fixtures")
                .join("codex_resumable.jsonl"),
        ],
        workspace_root: Some(workspace_root.display().to_string()),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        provider_profile_id: None,
        correlation_id: None,
        limit: Some(1),
    })?;
    let codex_import =
        import_selected_codex_sessions(&manager, codex_preview.candidates.clone(), None).await?;

    let claude_preview = preview_claude_external_sessions(ClaudeSessionImportPreviewRequest {
        paths: vec![
            workspace_root_path()
                .join("crates")
                .join("agent-claude")
                .join("tests")
                .join("fixtures")
                .join("claude_resumable.jsonl"),
        ],
        workspace_root: Some(workspace_root.display().to_string()),
        workspace_mode: WorkspaceMode::CurrentCheckout,
        provider_profile_id: None,
        correlation_id: None,
        limit: Some(1),
    })?;
    let claude_import =
        import_selected_claude_sessions(&manager, claude_preview.candidates.clone(), None).await?;

    let candidate_count = codex_preview.candidates.len() + claude_preview.candidates.len();
    let imported_count = codex_import.sessions.len() + claude_import.sessions.len();
    let timeline_count: u64 = codex_import
        .imported_timeline_counts
        .iter()
        .chain(claude_import.imported_timeline_counts.iter())
        .map(|count| u64::from(count.count))
        .sum();
    let status = if candidate_count == 2 && imported_count == 2 && timeline_count > 0 {
        E2eRegressionCheckStatus::Pass
    } else {
        E2eRegressionCheckStatus::Fail
    };

    Ok(E2eRegressionCheck {
        name: "import_fixture_smoke".to_string(),
        status,
        classification: classification_for_status(status),
        fixture_size: fixture_size([
            ("sources", 2),
            ("preview_candidates", candidate_count as u64),
            ("imported_sessions", imported_count as u64),
        ]),
        output_count: timeline_count,
        notes: "Codex and Claude fixture-backed import preview/import contracts completed"
            .to_string(),
    })
}

async fn dispatch_handshake(dispatcher: &RemoteDispatcher) -> VibexResult<RemoteHandshakeResponse> {
    let request = RemoteRequestEnvelope::new(RemoteOperationKind::Handshake)
        .with_payload(serde_json::json!({"clientName": "e2e-regression", "clientVersion": "0"}));
    let response = dispatcher.dispatch(request).await;
    decode_remote_response(response)
}

async fn dispatch_remote<T, R>(
    dispatcher: &RemoteDispatcher,
    operation: RemoteOperationKind,
    payload: T,
) -> VibexResult<R>
where
    T: Serialize,
    R: DeserializeOwned,
{
    let payload = serde_json::to_value(payload).map_err(|err| {
        VibexError::validation(
            "e2e_remote_payload_encode_failed",
            "failed to encode remote E2E request payload",
        )
        .with_diagnostic("error", err.to_string())
    })?;
    let request = RemoteRequestEnvelope::new(operation).with_payload(payload);
    let response = dispatcher.dispatch(request).await;
    decode_remote_response(response)
}

fn decode_remote_response<R>(response: RemoteResponseEnvelope) -> VibexResult<R>
where
    R: DeserializeOwned,
{
    if response.status != RemoteEnvelopeStatus::Ok {
        return Err(response.error.unwrap_or_else(|| {
            VibexError::new(
                ErrorCategory::Remote,
                "e2e_remote_dispatch_failed",
                "remote E2E request failed without structured error",
            )
        }));
    }
    let payload = response.payload.ok_or_else(|| {
        VibexError::new(
            ErrorCategory::Remote,
            "e2e_remote_payload_missing",
            "remote E2E response did not include a payload",
        )
    })?;
    serde_json::from_value(payload).map_err(|err| {
        VibexError::validation(
            "e2e_remote_payload_decode_failed",
            "failed to decode remote E2E response payload",
        )
        .with_diagnostic("error", err.to_string())
    })
}

fn pair_e2e_device(
    db_path: &Path,
    permission_level: RemoteDevicePermissionLevel,
    display_name: &str,
) -> VibexResult<RemoteAuthProof> {
    let mut conn = open_database(db_path)?;
    apply_migrations(&mut conn)?;
    let created = RemoteTrustService::create_pairing_code(
        &conn,
        RemoteCreatePairingCodeRequest {
            permission_level,
            ttl_ms: Some(60_000),
        },
    )?;
    let claimed = RemoteTrustService::claim_pairing_code(
        &conn,
        RemoteClaimPairingCodeRequest {
            pairing_code: created.pairing_code,
            display_name: display_name.to_string(),
            public_key: None,
        },
    )?;
    Ok(RemoteAuthProof {
        device_id: claimed.device.device_id,
        auth_token: claimed.auth_token,
    })
}

fn create_remote_workspace_fixture(workspace_root: &Path) -> VibexResult<()> {
    fs::create_dir_all(workspace_root.join("src"))
        .map_err(storage_io("e2e_workspace_fixture_create_failed"))?;
    fs::create_dir_all(workspace_root.join("docs"))
        .map_err(storage_io("e2e_workspace_fixture_create_failed"))?;
    fs::write(
        workspace_root.join("src").join("main.txt"),
        format!("{E2E_FILE_CONTENT_SENTINEL}\nremote workbench fixture\n"),
    )
    .map_err(storage_io("e2e_workspace_fixture_write_failed"))?;
    fs::write(
        workspace_root.join("docs").join("notes.md"),
        "bounded E2E fixture notes\n",
    )
    .map_err(storage_io("e2e_workspace_fixture_write_failed"))?;
    Ok(())
}

fn create_git_fixture(repo: &Path) -> VibexResult<()> {
    run_e2e_git_command(repo, &["init"])?;
    run_e2e_git_command(repo, &["config", "user.email", "e2e@example.invalid"])?;
    run_e2e_git_command(repo, &["config", "user.name", "Vibex E2E"])?;
    run_e2e_git_command(repo, &["add", "src/main.txt", "docs/notes.md"])?;
    run_e2e_git_command(repo, &["commit", "-m", "e2e fixture"])?;
    fs::write(
        repo.join("src").join("main.txt"),
        format!("{E2E_FILE_CONTENT_SENTINEL}\nmodified remote workbench fixture\n"),
    )
    .map_err(storage_io("e2e_git_fixture_write_failed"))?;
    fs::write(
        repo.join("src").join("untracked.txt"),
        "untracked e2e fixture\n",
    )
    .map_err(storage_io("e2e_git_fixture_write_failed"))?;
    Ok(())
}

fn run_e2e_git_command(repo: &Path, args: &[&str]) -> VibexResult<()> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .output()
        .map_err(|err| {
            VibexError::process("e2e_git_spawn_failed", "failed to spawn git")
                .with_diagnostic("error", err.to_string())
        })?;
    if output.status.success() {
        return Ok(());
    }
    Err(VibexError::process(
        "e2e_git_fixture_command_failed",
        "git fixture command failed",
    )
    .with_diagnostic("command", args.join(" ")))
}

async fn wait_for_terminal_snapshot(
    dispatcher: &RemoteDispatcher,
    auth: &RemoteAuthProof,
    terminal_id: &vibex_core::TerminalId,
) -> VibexResult<RemoteTerminalSnapshotResponse> {
    let deadline = Instant::now() + Duration::from_secs(3);
    let mut latest: Option<RemoteTerminalSnapshotResponse> = None;
    while Instant::now() < deadline {
        let snapshot: RemoteTerminalSnapshotResponse = dispatch_remote(
            dispatcher,
            RemoteOperationKind::Terminal,
            RemoteWorkbenchRequest::TerminalSnapshot(RemoteTerminalSnapshotRequest {
                auth: auth.clone(),
                terminal_id: terminal_id.clone(),
            }),
        )
        .await?;
        if !snapshot.snapshot.chunks.is_empty() {
            return Ok(snapshot);
        }
        latest = Some(snapshot);
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    latest.ok_or_else(|| {
        VibexError::process(
            "e2e_terminal_snapshot_missing",
            "terminal snapshot was unavailable during E2E regression",
        )
    })
}

pub fn assert_e2e_regression_output_redacted(serialized_output: &str) -> VibexResult<()> {
    for sentinel in e2e_sensitive_sentinels() {
        if serialized_output.contains(sentinel) {
            return Err(VibexError::validation(
                "e2e_regression_redaction_failed",
                "E2E regression evidence included a sensitive fixture sentinel",
            )
            .with_diagnostic("sentinel", sentinel));
        }
    }
    Ok(())
}

fn e2e_sensitive_sentinels() -> [&'static str; 6] {
    [
        E2E_AGENT_PROMPT_SENTINEL,
        E2E_FILE_CONTENT_SENTINEL,
        E2E_TERMINAL_OUTPUT_SENTINEL,
        E2E_SCHEDULED_PROMPT_SENTINEL,
        "authToken",
        "pairingCode",
    ]
}

fn e2e_regression_root() -> PathBuf {
    PathBuf::from("target").join("stage0").join(format!(
        "e2e-regression-{}-{}",
        std::process::id(),
        unix_timestamp_ms()
    ))
}

fn workspace_root_path() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("diagnostics crate must live under crates/")
        .to_path_buf()
}

fn fixture_size(items: impl IntoIterator<Item = (&'static str, u64)>) -> BTreeMap<String, u64> {
    items
        .into_iter()
        .map(|(key, value)| (key.to_string(), value))
        .collect()
}

fn classification_for_status(status: E2eRegressionCheckStatus) -> E2eRegressionClassification {
    match status {
        E2eRegressionCheckStatus::Pass => E2eRegressionClassification::AcceptableMvpLimit,
        E2eRegressionCheckStatus::FollowUp => E2eRegressionClassification::FollowUp,
        E2eRegressionCheckStatus::Fail => E2eRegressionClassification::Blocker,
    }
}

fn storage_io(code: &'static str) -> impl Fn(std::io::Error) -> VibexError {
    move |err| {
        VibexError::storage(code, "E2E regression fixture IO failed")
            .with_diagnostic("error", err.to_string())
    }
}

fn e2e_shell() -> String {
    #[cfg(target_os = "windows")]
    {
        std::env::var("COMSPEC").unwrap_or_else(|_| "cmd.exe".to_string())
    }

    #[cfg(not(target_os = "windows"))]
    {
        "/bin/sh".to_string()
    }
}

fn terminal_sentinel_command() -> String {
    #[cfg(target_os = "windows")]
    {
        format!("echo {E2E_TERMINAL_OUTPUT_SENTINEL}\r\n")
    }

    #[cfg(not(target_os = "windows"))]
    {
        format!("printf '{E2E_TERMINAL_OUTPUT_SENTINEL}\\n'\n")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn e2e_regression_output_is_bounded_and_redacted() {
        let result = run_e2e_regression_harness().await.unwrap();
        assert!(!result.has_blocker());
        assert_eq!(result.checks.len(), 5);

        let json = serde_json::to_string(&result).unwrap();
        assert!(json.contains("agent_command_protocol"));
        assert!(json.contains("remote_web_workbench"));
        assert!(json.contains("import_fixture_smoke"));
        assert_e2e_regression_output_redacted(&json).unwrap();
        assert!(!json.contains("e2e-regression-"));
        assert!(!json.contains("remote-workbench-workspace"));
        assert!(!json.contains(".db"));
        assert!(!json.contains("nativeThreadId"));
        assert!(!json.contains("nativeSessionId"));
    }
}

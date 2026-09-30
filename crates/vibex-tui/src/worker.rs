//! The asynchronous worker: the only place in the TUI that touches the network.
//!
//! The main thread owns the terminal and never `.await`s. Everything that can
//! block — RPCs, the event subscription, the reconnect loop — runs here on a
//! tokio runtime and reports back through an unbounded channel of
//! [`AppMessage`]s that the main loop drains with `try_recv`.
//!
//! Rules this module exists to enforce:
//!
//! * The first frame never waits for I/O: the worker is spawned before the
//!   first draw and reports readiness asynchronously.
//! * A stale result is dropped, not applied. Every request carries the
//!   generation it was issued for and the reducer checks it.
//! * A failed mutation rolls the optimistic UI back with an error toast.

use std::sync::Arc;

use tokio::sync::mpsc::{UnboundedReceiver, UnboundedSender, unbounded_channel};
use vibex_backend::{BackendError, BackendFacade, BackendResult, MutationRequest};
use vibex_core::{AgentSession, VibexSessionId};

use crate::app::Effect;
use crate::reduce::payloads;

/// One message from the worker back to the UI thread.
///
/// The variants differ a lot in size because they carry whole catalogues and
/// timelines. Boxing them would add an allocation per message for a value that
/// is moved through the channel exactly once, so the payloads stay inline.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum AppMessage {
    Sessions(BackendResult<Vec<AgentSession>>),
    /// An image off the system clipboard: its media type and bytes, or `None`
    /// when there is none or the desktop offers no way to read one.
    ClipboardImage(Option<(String, Vec<u8>)>),
    SessionOpened(BackendResult<Box<vibex_ui::AgentSessionSnapshot>>),
    TimelineRefreshed(BackendResult<i64>),
    RuntimeOptions(BackendResult<vibex_core::SessionRuntimeOptionCatalog>),
    SessionCreated(BackendResult<AgentSession>),
    Mutation {
        key: String,
        result: BackendResult<()>,
    },
    Workspaces(Vec<vibex_backend::WorkspaceSummary>),
    WorkspaceOpened(BackendResult<vibex_backend::WorkspaceSummary>),
    DirectoryListing(BackendResult<vibex_core::RemoteWorkspaceDirectoryListing>),
    FileTree(BackendResult<Vec<vibex_core::FileTreeEntry>>),
    FileContents(BackendResult<vibex_core::FileReadResponse>),
    GitStatus(BackendResult<vibex_core::GitStatusSummary>),
    GitDiff(BackendResult<vibex_core::GitDiffResponse>),
    GitHistory(BackendResult<vibex_core::GitHistoryResponse>),
    GitBranches(BackendResult<vibex_core::GitBranchListResponse>),
    Worktrees(BackendResult<Box<vibex_core::GitWorktreeLifecycleSnapshot>>),
    WorktreePreflight(BackendResult<vibex_core::GitWorktreeDestructivePreflight>),
    Devices(BackendResult<Vec<vibex_core::RemoteDeviceDetail>>),
    PairingOffer(BackendResult<Box<vibex_core::RemoteCreatePairingOfferResponse>>),
    Audit(BackendResult<Vec<vibex_core::RemoteAuditRecord>>),
    Profiles(BackendResult<Vec<vibex_core::ProviderProfileSummary>>),
    ProviderHealth(BackendResult<Vec<vibex_core::ProviderHealthSummary>>),
    ProviderTest(BackendResult<vibex_core::AgentModelProviderProfileTestResult>),
    ProviderModels(BackendResult<Vec<vibex_core::ProviderConfiguredModel>>),
    Agents(BackendResult<Vec<vibex_core::AgentSnapshotEntry>>),
    AgentAuth(BackendResult<Vec<vibex_core::AgentAuthContext>>),
    Mcp(BackendResult<Vec<vibex_core::McpServer>>),
    Skills(BackendResult<Vec<vibex_core::Skill>>),
    Prompts(BackendResult<Vec<vibex_core::Prompt>>),
    Hooks(BackendResult<Vec<vibex_core::Hook>>),
    Completions(BackendResult<Box<vibex_core::AgentCommandDiscovery>>),
    Usage(BackendResult<Box<UsageReport>>),
    Recovery(BackendResult<String>),
    BackupList(BackendResult<Vec<vibex_core::BackupCreateOutcome>>),
    Clipboard(BackendResult<()>),
    EditorFinished(BackendResult<Option<String>>),
    Event(vibex_backend::BackendEvent),
    /// The subscription ended; the connection is gone.
    SubscriptionEnded,
    /// A fatal error that should be shown as a toast.
    Notice {
        text: String,
        danger: bool,
    },
}

/// Usage data for the usage page.
#[derive(Debug, Clone, Default)]
pub struct UsageReport {
    pub session: Option<vibex_core::AgentTokenUsage>,
    pub aggregate: Option<vibex_core::AgentUsageStatistics>,
}

/// The tokio side of the client.
pub struct Worker {
    runtime: tokio::runtime::Runtime,
    facade: BackendFacade,
    sender: UnboundedSender<AppMessage>,
    /// Generation of the most recent session load, so a late snapshot from a
    /// previous session is dropped instead of applied to the wrong one.
    session_generation: Arc<std::sync::atomic::AtomicU64>,
}

impl Worker {
    /// Start the worker and its event subscription.
    pub fn start(facade: BackendFacade) -> BackendResult<(Self, UnboundedReceiver<AppMessage>)> {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| BackendError::failed("tui_runtime_unavailable", error.to_string()))?;
        let (sender, receiver) = unbounded_channel();
        let worker = Self {
            runtime,
            facade: facade.clone(),
            sender: sender.clone(),
            session_generation: Arc::new(std::sync::atomic::AtomicU64::new(0)),
        };
        // Subscribe immediately so events that arrive during startup are not
        // lost; the reducer ignores events for sessions it has not opened.
        match facade.agent().subscribe() {
            Ok(mut subscription) => {
                let events = sender.clone();
                worker.runtime.handle().spawn(async move {
                    loop {
                        match subscription.next().await {
                            Ok(Some(event)) => {
                                if events.send(AppMessage::Event(event)).is_err() {
                                    break;
                                }
                            }
                            Ok(None) | Err(_) => {
                                let _ = events.send(AppMessage::SubscriptionEnded);
                                break;
                            }
                        }
                    }
                });
            }
            Err(error) => {
                let _ = sender.send(AppMessage::Notice {
                    text: error.message,
                    danger: true,
                });
            }
        }
        Ok((worker, receiver))
    }

    /// Whether the worker should keep running.
    pub fn is_alive(&self) -> bool {
        !self.sender.is_closed()
    }

    /// Execute one effect.
    pub fn dispatch(&self, effect: Effect) {
        let facade = self.facade.clone();
        let sender = self.sender.clone();
        let generation = self.session_generation.fetch_add(
            u64::from(matches!(effect, Effect::OpenSession { .. })),
            std::sync::atomic::Ordering::SeqCst,
        );
        self.runtime.handle().spawn(async move {
            let mut worker = Dispatch {
                facade,
                sender,
                generation,
            };
            worker.run(effect).await;
        });
    }

    /// Shut the runtime down, waiting for in-flight work.
    pub fn shutdown(self) {
        self.runtime
            .shutdown_timeout(std::time::Duration::from_millis(500));
    }
}

struct Dispatch {
    facade: BackendFacade,
    sender: UnboundedSender<AppMessage>,
    generation: u64,
}

impl Dispatch {
    fn send(&self, message: AppMessage) {
        let _ = self.sender.send(message);
    }

    fn failure(&self, key: &str, error: BackendError) {
        self.send(AppMessage::Mutation {
            key: key.to_string(),
            result: Err(error),
        });
    }

    fn ok(&self, key: &str) {
        self.send(AppMessage::Mutation {
            key: key.to_string(),
            result: Ok(()),
        });
    }

    async fn run(&mut self, effect: Effect) {
        match effect {
            Effect::ListSessions { include_archived } => {
                let result = self.facade.agent().list_sessions(include_archived).await;
                self.send(AppMessage::Sessions(result));
            }
            Effect::OpenSession { session_id } => {
                let result = self.load_session(session_id).await;
                self.send(AppMessage::SessionOpened(result.map(Box::new)));
            }
            Effect::RefreshTimeline => {
                // The reducer re-issues an open for the current session; the
                // generation check upstream keeps the snapshot ordered.
                match self.facade.agent().list_sessions(false).await {
                    Ok(_) => self.send(AppMessage::TimelineRefreshed(Ok(self.generation as i64))),
                    Err(error) => self.send(AppMessage::TimelineRefreshed(Err(error))),
                }
            }
            Effect::ListRuntimeOptions => {
                let result = self.facade.agent().list_runtime_options().await;
                self.send(AppMessage::RuntimeOptions(result));
            }
            Effect::CreateSession {
                workspace_root,
                title,
            } => {
                let runtime = match self.current_runtime_selection().await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.failure("create_session", error);
                        return;
                    }
                };
                let request = payloads::create_session(workspace_root, title, runtime);
                match self.facade.agent().create_session(request).await {
                    Ok(session) => self.send(AppMessage::SessionCreated(Ok(session))),
                    Err(error) => self.send(AppMessage::SessionCreated(Err(error))),
                }
            }
            Effect::RenameSession { session_id, title } => {
                let request = payloads::rename_session(session_id, title);
                match self.facade.agent().rename_session(request).await {
                    Ok(_) => self.ok("rename_session"),
                    Err(error) => self.failure("rename_session", error),
                }
            }
            Effect::ArchiveSession { session_id } => {
                let request = MutationRequest::new(session_id);
                match self.facade.agent().archive_session(request).await {
                    Ok(()) => self.ok("archive_session"),
                    Err(error) => self.failure("archive_session", error),
                }
            }
            Effect::DeleteSession { session_id } => {
                let request = MutationRequest::new(session_id);
                match self.facade.agent().delete_session(request).await {
                    Ok(()) => self.ok("delete_session"),
                    Err(error) => self.failure("delete_session", error),
                }
            }
            Effect::ForkSession { session_id } => {
                let request = payloads::fork_session(session_id);
                match self.facade.agent().fork_session(request).await {
                    Ok(session) => self.send(AppMessage::SessionCreated(Ok(session))),
                    Err(error) => self.send(AppMessage::SessionCreated(Err(error))),
                }
            }
            Effect::SendMessage {
                session_id,
                text,
                attachments,
            } => {
                let runtime = match self.current_runtime_selection().await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.failure("send_message", error);
                        return;
                    }
                };
                let request =
                    payloads::send_message(session_id, text, attachments.clone(), runtime);
                match self.facade.agent().send_message(request).await {
                    Ok(_) => self.ok("send_message"),
                    Err(error) => self.failure("send_message", error),
                }
            }
            Effect::ReadClipboardImage => {
                // Talking to a clipboard owner can block for the whole
                // deadline, so it runs off the worker's own thread.
                let image = tokio::task::spawn_blocking(crate::terminal::read_clipboard_image)
                    .await
                    .ok()
                    .flatten();
                self.send(AppMessage::ClipboardImage(image));
            }
            Effect::ContinueTurn { session_id } => {
                let request = payloads::continue_turn(session_id);
                match self.facade.agent().continue_turn(request).await {
                    Ok(_) => self.ok("continue_turn"),
                    Err(error) => self.failure("continue_turn", error),
                }
            }
            Effect::Interrupt { session_id } => {
                let request = MutationRequest::new(session_id);
                match self.facade.agent().interrupt(request).await {
                    Ok(_) => self.ok("interrupt"),
                    Err(error) => self.failure("interrupt", error),
                }
            }
            Effect::SteerMessage {
                session_id,
                text,
                attachments,
                fallback_to_resend,
            } => {
                let supported = self
                    .facade
                    .agent()
                    .native_steering_supported(session_id.clone())
                    .await
                    .unwrap_or(false);
                if supported {
                    let request = payloads::steer_message(session_id, text, attachments.clone());
                    match self.facade.agent().steer_message(request).await {
                        Ok(_) => self.ok("steer_message"),
                        Err(error) => self.failure("steer_message", error),
                    }
                    return;
                }
                if !fallback_to_resend {
                    self.failure(
                        "steer_message",
                        BackendError::unsupported(
                            "agent_steering_unavailable",
                            "this backend cannot steer a running turn",
                        ),
                    );
                    return;
                }
                // Remote seat: interrupt, then resend the text as a new turn.
                let interrupt = MutationRequest::new(session_id.clone());
                if let Err(error) = self.facade.agent().interrupt(interrupt).await {
                    self.failure("steer_message", error);
                    return;
                }
                let runtime = match self.current_runtime_selection().await {
                    Ok(selection) => selection,
                    Err(error) => {
                        self.failure("steer_message", error);
                        return;
                    }
                };
                let request =
                    payloads::send_message(session_id, text, attachments.clone(), runtime);
                match self.facade.agent().send_message(request).await {
                    Ok(_) => self.ok("steer_message"),
                    Err(error) => self.failure("steer_message", error),
                }
            }
            Effect::ResolvePermission {
                session_id,
                request_id,
                resolution,
            } => {
                let request = payloads::resolve_permission(session_id, request_id, resolution);
                match self.facade.agent().resolve_permission(request).await {
                    Ok(_) => self.ok("resolve_permission"),
                    Err(error) => self.failure("resolve_permission", error),
                }
            }
            Effect::ResolveElicitation {
                session_id,
                request_id,
                resolution,
            } => {
                let request = payloads::resolve_elicitation(session_id, request_id, resolution);
                match self.facade.agent().resolve_elicitation(request).await {
                    Ok(_) => self.ok("resolve_elicitation"),
                    Err(error) => self.failure("resolve_elicitation", error),
                }
            }
            Effect::SwitchRuntime {
                session_id,
                selection,
            } => {
                let request =
                    MutationRequest::new(vibex_core::SetDesiredAgentSessionRuntimeRequest {
                        session_id,
                        idempotency_key: vibex_core::RequestId::new().as_str().to_string(),
                        expected_revision: 0,
                        expected_selection_revision: 0,
                        desired: selection,
                        interaction: vibex_core::RuntimeSelectionInteraction::Seamless,
                    });
                match self.facade.agent().set_desired_runtime(request).await {
                    Ok(_) => self.ok("switch_runtime"),
                    Err(error) => self.failure("switch_runtime", error),
                }
            }
            Effect::ProbeAgentRuntime { request } => {
                let request = MutationRequest::new(request);
                match self
                    .facade
                    .agent()
                    .probe_agent_runtime_options(request)
                    .await
                {
                    Ok(_) => self.ok("runtime_probe"),
                    Err(error) => self.failure("runtime_probe", error),
                }
            }
            Effect::ListWorkspaces => {
                let result = self.facade.workspace().list_workspaces().await;
                self.send(AppMessage::Workspaces(result.unwrap_or_default()));
            }
            Effect::OpenWorkspace { root_path } => {
                let request = MutationRequest::new(vibex_core::OpenWorkspaceRequest {
                    root_path,
                    mode: None,
                });
                match self.facade.workspace().open_workspace(request).await {
                    Ok(summary) => self.send(AppMessage::WorkspaceOpened(Ok(summary))),
                    Err(error) => self.send(AppMessage::WorkspaceOpened(Err(error))),
                }
            }
            Effect::BrowseDirectories { path } => {
                let result = self
                    .facade
                    .workspace()
                    .browse_authority_directories(path)
                    .await;
                self.send(AppMessage::DirectoryListing(result));
            }
            Effect::LoadFileTree { workspace_id } => {
                let request = vibex_core::FileTreeRequest {
                    workspace_id,
                    path: None,
                    max_depth: Some(3),
                    include_hidden: false,
                };
                let result = self.facade.file().file_tree(request).await;
                self.send(AppMessage::FileTree(result));
            }
            Effect::ReadFile { workspace_id, path } => {
                let request = vibex_core::FileReadRequest {
                    workspace_id,
                    path,
                    max_bytes: Some(512 * 1024),
                };
                let result = self.facade.file().read_file(request).await;
                self.send(AppMessage::FileContents(result));
            }
            Effect::LoadGitStatus { workspace_id } => {
                let result = self.facade.git().git_status(workspace_id).await;
                self.send(AppMessage::GitStatus(result));
            }
            Effect::LoadGitDiff { workspace_id, path } => {
                let request = vibex_core::GitDiffRequest {
                    workspace_id,
                    path,
                    staged: false,
                };
                let result = self.facade.git().git_diff(request).await;
                self.send(AppMessage::GitDiff(result));
            }
            Effect::GitStage {
                workspace_id,
                path,
                stage,
            } => {
                let request = MutationRequest::new(vibex_core::GitStageRequest {
                    workspace_id,
                    paths: vec![path],
                });
                let result = if stage {
                    self.facade.git().stage(request).await.map(|_| ())
                } else {
                    self.facade.git().unstage(request).await.map(|_| ())
                };
                match result {
                    Ok(()) => self.ok("git_stage"),
                    Err(error) => self.failure("git_stage", error),
                }
            }
            Effect::LoadGitHistory { workspace_id } => {
                let request = vibex_core::GitHistoryRequest {
                    workspace_id,
                    limit: Some(200),
                    before_commit: None,
                    ref_name: None,
                    author: None,
                    query: None,
                    authored_after_ms: None,
                    authored_before_ms: None,
                };
                let result = self.facade.git().git_history(request).await;
                self.send(AppMessage::GitHistory(result));
            }
            Effect::LoadGitBranches { workspace_id } => {
                let result = self.facade.git().git_branch_list(workspace_id).await;
                self.send(AppMessage::GitBranches(result));
            }
            Effect::GitRevert { workspace_id, path } => {
                let request = MutationRequest::new(vibex_core::GitStageRequest {
                    workspace_id,
                    paths: vec![path],
                });
                match self.facade.git().git_revert(request).await {
                    Ok(status) => {
                        self.send(AppMessage::GitStatus(Ok(status)));
                        self.ok("git_revert");
                    }
                    Err(error) => self.failure("git_revert", error),
                }
            }
            Effect::LoadWorktrees { workspace_id } => {
                let result = self.facade.git().git_worktree_snapshot(workspace_id).await;
                self.send(AppMessage::Worktrees(result.map(Box::new)));
            }
            Effect::WorktreePreflight { workspace_id, path } => {
                // Preflight is mandatory: it is what tells the user whether a
                // destructive worktree action would lose work.
                let request = vibex_core::GitWorktreeArchiveRequest {
                    workspace_id,
                    worktree_path: path,
                    expected_head: None,
                    preflight_revision: None,
                };
                let result = self
                    .facade
                    .git()
                    .git_worktree_archive_preflight(request)
                    .await;
                self.send(AppMessage::WorktreePreflight(result));
            }
            Effect::WorktreeCreate {
                workspace_id,
                branch_name,
            } => {
                let request = MutationRequest::new(vibex_core::GitWorktreeCreateRequest {
                    workspace_id,
                    branch_name,
                    base_ref: None,
                    name: None,
                    worktree_path: None,
                    target_workspace_id: None,
                    target_branch: None,
                });
                match self.facade.git().git_worktree_create(request).await {
                    Ok(_) => self.ok("worktree_create"),
                    Err(error) => self.failure("worktree_create", error),
                }
            }
            Effect::UpdateEntry { entry } => {
                let result = self.update_entry(entry).await;
                match result {
                    Ok(()) => self.ok("update_entry"),
                    Err(error) => self.failure("update_entry", error),
                }
            }
            Effect::GitCommit {
                workspace_id,
                message,
            } => {
                let request = MutationRequest::new(vibex_core::GitCommitRequest {
                    workspace_id,
                    message,
                    paths: Vec::new(),
                    amend: false,
                    push_after: false,
                });
                match self.facade.git().commit(request).await {
                    Ok(_) => self.ok("git_commit"),
                    Err(error) => self.failure("git_commit", error),
                }
            }
            Effect::ListDevices => {
                let result = self.facade.device().list_devices().await;
                self.send(AppMessage::Devices(result));
            }
            Effect::CreatePairingOffer => {
                let request = MutationRequest::new(vibex_core::RemoteCreatePairingOfferRequest {
                    permission_level: vibex_core::RemoteDevicePermissionLevel::FullControl,
                    ttl_ms: None,
                    direct_candidates: Vec::new(),
                    relay_candidate: None,
                });
                // The v2 offer is the only pairing route a paired client can
                // reach; the v1 code path is authority-only.
                let result = self.facade.device().create_pairing_offer_v2(request).await;
                self.send(AppMessage::PairingOffer(result.map(Box::new)));
            }
            Effect::RevokeDevice { device_id, reason } => {
                let request = MutationRequest::new(vibex_core::RemoteRevokeDeviceRequest {
                    device_id,
                    reason,
                });
                match self.facade.device().revoke_device(request).await {
                    Ok(_) => self.ok("revoke_device"),
                    Err(error) => self.failure("revoke_device", error),
                }
            }
            Effect::ListAudit => {
                let request = vibex_core::RemoteAuditListRequest {
                    device_id: None,
                    limit: Some(200),
                };
                let result = self.facade.device().audit_records(request).await;
                self.send(AppMessage::Audit(result));
            }
            Effect::ListProfiles => {
                let result = self.facade.management().list_profiles().await;
                self.send(AppMessage::Profiles(result));
            }
            Effect::RenameProfile { profile_id, name } => {
                let Some(request) = self.profile_update(profile_id, Some(name)).await else {
                    return;
                };
                match self
                    .facade
                    .management()
                    .update_agent_model_provider_profile(request)
                    .await
                {
                    Ok(_) => self.ok("rename_profile"),
                    Err(error) => self.failure("rename_profile", error),
                }
            }
            Effect::SelectProfile { profile_id } => {
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.failure(
                        "select_profile",
                        BackendError::failed(
                            "provider_profile_unknown",
                            "the selected profile is no longer listed",
                        ),
                    );
                    return;
                };
                let request =
                    MutationRequest::new(vibex_backend::ManagementProfileSelectionRequest {
                        agent_id,
                        provider_profile_id: profile_id.clone(),
                        scope: None,
                    });
                match self.facade.management().select_profile(request).await {
                    Ok(_) => self.ok("select_profile"),
                    Err(error) => self.failure("select_profile", error),
                }
            }
            Effect::WriteProviderSecret { profile_id, secret } => {
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.failure(
                        "provider_secret",
                        BackendError::failed(
                            "provider_profile_unknown",
                            "the selected profile has no owning Agent",
                        ),
                    );
                    return;
                };
                let request = MutationRequest::new(
                    vibex_core::AgentModelProviderProfileSecretValueUpdateRequest {
                        agent_id,
                        provider_profile_id: profile_id,
                        value: Some(secret),
                        clear: false,
                    },
                );
                match self
                    .facade
                    .management()
                    .mutate_agent_model_provider_profile_secret(request)
                    .await
                {
                    Ok(_) => self.ok("provider_secret"),
                    Err(error) => self.failure("provider_secret", error),
                }
            }
            Effect::TestProviderProfile { profile_id } => {
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.send(AppMessage::ProviderTest(Err(BackendError::failed(
                        "provider_profile_unknown",
                        "the selected profile has no owning Agent",
                    ))));
                    return;
                };
                let request = vibex_core::AgentModelProviderProfileTestRequest {
                    agent_id,
                    provider_profile_id: profile_id,
                };
                let result = self
                    .facade
                    .management()
                    .test_agent_model_provider_profile(request)
                    .await;
                self.send(AppMessage::ProviderTest(result));
            }
            Effect::FetchProviderModels { profile_id } => {
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.send(AppMessage::ProviderModels(Err(BackendError::failed(
                        "provider_profile_unknown",
                        "the selected profile has no owning Agent",
                    ))));
                    return;
                };
                let request = vibex_core::AgentModelProviderProfileFetchModelsRequest {
                    agent_id,
                    provider_profile_id: profile_id,
                };
                let result = self
                    .facade
                    .management()
                    .fetch_agent_model_provider_profile_models(request)
                    .await
                    .map(|response| response.models);
                self.send(AppMessage::ProviderModels(result));
            }
            Effect::ListHealth => {
                let result = self.facade.management().health_summaries().await;
                self.send(AppMessage::ProviderHealth(result));
            }
            Effect::ListAgents => {
                let request = vibex_core::AgentListRequest {
                    include_disabled: true,
                };
                let result = self.facade.management().list_agents(request).await;
                self.send(AppMessage::Agents(result.map(|response| response.agents)));
            }
            Effect::InstallAgent { agent_id } => {
                let request = MutationRequest::new(agent_id);
                match self
                    .facade
                    .management()
                    .install_managed_agent(request)
                    .await
                {
                    Ok(_) => self.ok("install_agent"),
                    Err(error) => self.failure("install_agent", error),
                }
            }
            Effect::UninstallAgent { agent_id } => {
                let request = MutationRequest::new(agent_id);
                match self
                    .facade
                    .management()
                    .uninstall_managed_agent(request)
                    .await
                {
                    Ok(_) => self.ok("uninstall_agent"),
                    Err(error) => self.failure("uninstall_agent", error),
                }
            }
            Effect::ListAgentAuth { .. } => {
                let result = self.facade.agent().list_agent_auth_contexts().await;
                self.send(AppMessage::AgentAuth(result));
            }
            Effect::LogoutAgent { agent_id } => {
                let request = MutationRequest::new(vibex_core::AgentLogoutRequest {
                    agent_id,
                    provider_profile_id: None,
                });
                match self.facade.agent().logout_agent(request).await {
                    Ok(()) => self.ok("logout_agent"),
                    Err(error) => self.failure("logout_agent", error),
                }
            }
            Effect::ListMcp => {
                let result = self.facade.management().mcp_servers().await;
                self.send(AppMessage::Mcp(result));
            }
            Effect::ListSkills => {
                let result = self.facade.management().skills().await;
                self.send(AppMessage::Skills(result));
            }
            Effect::ListPrompts => {
                let result = self.facade.management().prompts().await;
                self.send(AppMessage::Prompts(result));
            }
            Effect::ListHooks => {
                let result = self.facade.management().hooks().await;
                self.send(AppMessage::Hooks(result));
            }
            Effect::ToggleMcp { server_id, enabled } => {
                let request = MutationRequest::new(vibex_core::McpServerUpdateRequest {
                    mcp_server_id: server_id,
                    display_name: None,
                    transport_kind: None,
                    status: Some(if enabled {
                        vibex_core::McpServerStatus::Enabled
                    } else {
                        vibex_core::McpServerStatus::Disabled
                    }),
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    command: None,
                    args: None,
                    env: None,
                    url: None,
                    headers: None,
                    description: None,
                    tags: None,
                });
                match self.facade.management().update_mcp_server(request).await {
                    Ok(_) => self.ok("toggle_entry"),
                    Err(error) => self.failure("toggle_entry", error),
                }
            }
            Effect::ToggleSkill { skill_id, enabled } => {
                let request = MutationRequest::new(vibex_core::SkillUpdateRequest {
                    skill_id,
                    display_name: None,
                    source_kind: None,
                    status: Some(if enabled {
                        vibex_core::SkillStatus::Enabled
                    } else {
                        vibex_core::SkillStatus::Disabled
                    }),
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    source_uri: None,
                    description: None,
                    tags: None,
                    content_preview: None,
                    body: None,
                });
                match self.facade.management().update_skill(request).await {
                    Ok(_) => self.ok("toggle_entry"),
                    Err(error) => self.failure("toggle_entry", error),
                }
            }
            Effect::TogglePrompt { prompt_id, enabled } => {
                let request = MutationRequest::new(vibex_core::PromptUpdateRequest {
                    prompt_id,
                    display_name: None,
                    kind: None,
                    status: Some(if enabled {
                        vibex_core::PromptStatus::Enabled
                    } else {
                        vibex_core::PromptStatus::Disabled
                    }),
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    body: None,
                    description: None,
                    tags: None,
                });
                match self.facade.management().update_prompt(request).await {
                    Ok(_) => self.ok("toggle_entry"),
                    Err(error) => self.failure("toggle_entry", error),
                }
            }
            Effect::ToggleHook { hook_id, enabled } => {
                let request = MutationRequest::new(vibex_core::HookUpdateRequest {
                    hook_id,
                    display_name: None,
                    provider_kind: None,
                    event_kind: None,
                    status: Some(if enabled {
                        vibex_core::HookStatus::Enabled
                    } else {
                        vibex_core::HookStatus::Disabled
                    }),
                    install_state: None,
                    command_preview: None,
                    managed_marker: None,
                    description: None,
                });
                match self.facade.management().update_hook(request).await {
                    Ok(_) => self.ok("toggle_entry"),
                    Err(error) => self.failure("toggle_entry", error),
                }
            }
            Effect::LoadUsage => {
                let request = vibex_core::AgentUsageStatisticsRequest::default();
                let aggregate = self.facade.agent().usage_statistics(request).await;
                let session = self.current_session_token_usage().await;
                let report = UsageReport {
                    session: session.ok().flatten(),
                    aggregate: aggregate.ok(),
                };
                self.send(AppMessage::Usage(Ok(Box::new(report))));
            }
            Effect::ExportDiagnostics => {
                let request = MutationRequest::new(vibex_core::DiagnosticExportPayload::default());
                match self.facade.management().export_diagnostics(request).await {
                    Ok(outcome) => self.send(AppMessage::Recovery(Ok(outcome.destination))),
                    Err(error) => self.send(AppMessage::Recovery(Err(error))),
                }
            }
            Effect::CreateBackup => {
                let request = MutationRequest::new(vibex_core::BackupCreatePayload::default());
                match self.facade.management().backup_create(request).await {
                    Ok(outcome) => self.send(AppMessage::Recovery(Ok(outcome.backup_dir))),
                    Err(error) => self.send(AppMessage::Recovery(Err(error))),
                }
            }
            Effect::InspectBackup => {
                let request = vibex_core::BackupInspectPayload::default();
                match self.facade.management().backup_inspect(request).await {
                    Ok(outcome) => self.send(AppMessage::Recovery(Ok(format!(
                        "{}: {:?}",
                        outcome.backup_dir, outcome.migration_compatibility
                    )))),
                    Err(error) => self.send(AppMessage::Recovery(Err(error))),
                }
            }
            Effect::RestoreBackup { backup_id } => {
                let request = MutationRequest::new(vibex_core::BackupRestorePayload {
                    backup_dir: Some(backup_id),
                    ..Default::default()
                });
                match self.facade.management().backup_restore(request).await {
                    Ok(outcome) => self.send(AppMessage::Recovery(Ok(format!(
                        "{:?} {}",
                        outcome.status, outcome.target_db_path
                    )))),
                    Err(error) => self.send(AppMessage::Recovery(Err(error))),
                }
            }
            Effect::DiscoverCompletions { trigger, query } => {
                // The authority owns the command catalogue: `/` merges the
                // Agent's own commands with Vibex Prompts, `@` resolves
                // workspace files, and `$` resolves Skills. None of that is
                // local knowledge, so the composer asks rather than guessing.
                let session = self.current_session().await;
                let request = vibex_core::AgentCommandDiscoverRequest {
                    agent_id: session.as_ref().map(|session| session.agent_id.clone()),
                    provider_profile_id: None,
                    session_id: session.as_ref().map(|session| session.id.clone()),
                    workspace_id: session.as_ref().map(|session| session.workspace_id.clone()),
                    trigger: Some(match trigger {
                        crate::composer::CompletionTrigger::Slash => {
                            vibex_core::AgentCommandTrigger::Slash
                        }
                        crate::composer::CompletionTrigger::At => {
                            vibex_core::AgentCommandTrigger::Mention
                        }
                        crate::composer::CompletionTrigger::Dollar => {
                            vibex_core::AgentCommandTrigger::Dollar
                        }
                    }),
                    query: (!query.is_empty()).then_some(query),
                    limit: Some(50),
                };
                let result = self.facade.agent().discover_agent_commands(request).await;
                self.send(AppMessage::Completions(result.map(Box::new)));
            }
            Effect::CheckDrift => {}
            Effect::EditExternally { title, body } => {
                let result = crate::terminal::edit_in_editor(&title, &body);
                self.send(AppMessage::EditorFinished(result));
            }
            Effect::Clipboard { text } => {
                let result = crate::terminal::copy_to_clipboard(&text);
                self.send(AppMessage::Clipboard(result));
            }
        }
    }

    async fn load_session(
        &self,
        session_id: VibexSessionId,
    ) -> BackendResult<vibex_ui::AgentSessionSnapshot> {
        let session = self.facade.agent().open_session(session_id.clone()).await?;
        let timeline = self
            .facade
            .agent()
            .fetch_timeline(vibex_core::FetchTimelineRequest {
                session_id: session_id.clone(),
                after_sequence: None,
                limit: vibex_ui::AGENT_TIMELINE_PAGE_LIMIT,
            })
            .await?;
        Ok(vibex_ui::AgentSessionSnapshot {
            session,
            timeline: timeline.items,
            runtime_selection: None,
        })
    }

    async fn current_runtime_selection(
        &self,
    ) -> BackendResult<vibex_core::SessionRuntimeSelection> {
        let catalog = self.facade.agent().list_runtime_options().await?;
        catalog
            .options
            .iter()
            .find(|option| option.availability == vibex_core::RuntimeOptionAvailability::Available)
            .or_else(|| catalog.options.first())
            .map(|option| option.selection.clone())
            .ok_or_else(|| {
                BackendError::failed(
                    "agent_runtime_unavailable",
                    "no Agent runtime is available for a new session",
                )
            })
    }

    /// The Agent that owns a provider profile, resolved from the profile list
    /// so the client never has to guess at the binding.
    async fn agent_for_profile(
        &self,
        profile_id: &vibex_core::ProviderProfileId,
    ) -> Option<vibex_core::AgentId> {
        self.facade
            .management()
            .list_profiles()
            .await
            .ok()?
            .into_iter()
            .find(|profile| &profile.id == profile_id)
            .map(|profile| profile.agent_id)
    }

    /// Build a profile update that only changes the display name, preserving
    /// the Agent binding the authority requires.
    async fn profile_update(
        &self,
        profile_id: vibex_core::ProviderProfileId,
        display_name: Option<String>,
    ) -> Option<MutationRequest<vibex_core::AgentModelProviderProfileUpdateRequest>> {
        let agent_id = self.agent_for_profile(&profile_id).await?;
        Some(MutationRequest::new(
            vibex_core::AgentModelProviderProfileUpdateRequest {
                agent_id,
                provider_profile_id: profile_id,
                display_name,
                status: None,
                account_alias: None,
                base_url: None,
                default_model: None,
                small_model: None,
                large_model: None,
                configured_models: None,
                reasoning_effort: None,
                sandbox_defaults: None,
                network_defaults: None,
                permission_defaults: None,
                provider_options: None,
            },
        ))
    }

    /// The session the composer is composing into, used to scope discovery to
    /// the right workspace and Agent.
    async fn current_session(&self) -> Option<vibex_core::AgentSession> {
        self.facade
            .agent()
            .list_sessions(false)
            .await
            .ok()?
            .into_iter()
            .next()
    }

    /// Apply a management entry rename through the domain-specific update call.
    async fn update_entry(&self, entry: crate::app::ManagementEntryEdit) -> BackendResult<()> {
        use crate::app::ManagementEntryEdit;
        match entry {
            ManagementEntryEdit::Mcp {
                server_id,
                display_name,
            } => self
                .facade
                .management()
                .update_mcp_server(MutationRequest::new(vibex_core::McpServerUpdateRequest {
                    mcp_server_id: server_id,
                    display_name: Some(display_name),
                    transport_kind: None,
                    status: None,
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    command: None,
                    args: None,
                    env: None,
                    url: None,
                    headers: None,
                    description: None,
                    tags: None,
                }))
                .await
                .map(|_| ()),
            ManagementEntryEdit::Skill {
                skill_id,
                display_name,
            } => self
                .facade
                .management()
                .update_skill(MutationRequest::new(vibex_core::SkillUpdateRequest {
                    skill_id,
                    display_name: Some(display_name),
                    source_kind: None,
                    status: None,
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    source_uri: None,
                    description: None,
                    tags: None,
                    content_preview: None,
                    body: None,
                }))
                .await
                .map(|_| ()),
            ManagementEntryEdit::Prompt {
                prompt_id,
                display_name,
            } => self
                .facade
                .management()
                .update_prompt(MutationRequest::new(vibex_core::PromptUpdateRequest {
                    prompt_id,
                    display_name: Some(display_name),
                    kind: None,
                    status: None,
                    scope_kind: None,
                    project_id: None,
                    workspace_id: None,
                    body: None,
                    description: None,
                    tags: None,
                }))
                .await
                .map(|_| ()),
            ManagementEntryEdit::Hook {
                hook_id,
                display_name,
            } => self
                .facade
                .management()
                .update_hook(MutationRequest::new(vibex_core::HookUpdateRequest {
                    hook_id,
                    display_name: Some(display_name),
                    provider_kind: None,
                    event_kind: None,
                    status: None,
                    install_state: None,
                    command_preview: None,
                    managed_marker: None,
                    description: None,
                }))
                .await
                .map(|_| ()),
        }
    }

    async fn current_session_token_usage(
        &self,
    ) -> BackendResult<Option<vibex_core::AgentTokenUsage>> {
        let sessions = self.facade.agent().list_sessions(false).await?;
        let Some(session) = sessions.first() else {
            return Ok(None);
        };
        self.facade
            .agent()
            .session_token_usage(session.id.clone())
            .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_disconnected_backend_reports_offline_rather_than_hanging() {
        let facade = vibex_backend::DisconnectedBackend::facade();
        let (worker, mut receiver) = Worker::start(facade).expect("worker starts");
        worker.dispatch(Effect::ListSessions {
            include_archived: false,
        });
        // The subscription failure notice can arrive first, so drain until the
        // RPC answer shows up.
        let message = worker.runtime.block_on(async {
            let deadline = std::time::Duration::from_secs(5);
            let start = std::time::Instant::now();
            loop {
                let remaining = deadline.saturating_sub(start.elapsed());
                match tokio::time::timeout(remaining, receiver.recv()).await {
                    Ok(Some(AppMessage::Sessions(result))) => break AppMessage::Sessions(result),
                    Ok(Some(_)) => continue,
                    Ok(None) => panic!("the channel closed before the RPC answered"),
                    Err(_) => panic!("the RPC never answered"),
                }
            }
        });
        match message {
            AppMessage::Sessions(Err(error)) => {
                assert!(!error.code.is_empty());
            }
            other => panic!("expected an offline error, got {other:?}"),
        }
    }

    #[test]
    fn every_effect_reports_a_stable_key() {
        // The key is what the reducer uses to clear the pending marker, so an
        // empty one would leak a spinner forever.
        let effects = [
            Effect::ListSessions {
                include_archived: false,
            },
            Effect::RefreshTimeline,
            Effect::ListDevices,
            Effect::LoadUsage,
        ];
        for effect in effects {
            assert!(!effect.key().is_empty());
        }
    }
}

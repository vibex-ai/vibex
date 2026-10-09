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

/// How much of a session's history a probe reads: the tail of the latest turn
/// is all the answer needs, and the same page is what the Desktop asks for when
/// it probes.
const AUTO_CONTINUE_PROBE_LIMIT: u32 = 500;

/// One message from the worker back to the UI thread.
///
/// What a clipboard held when the reader asked for it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ClipboardContent {
    Image {
        mime_type: String,
        bytes: Vec<u8>,
    },
    Text(String),
    /// Nothing pasteable on the host: no image, no text.
    ///
    /// The terminal on the reader's side of the connection may still hold
    /// something, so this is not yet "nothing to paste" — see
    /// [`crate::terminal_clipboard`].
    Empty,
    /// The terminal was asked for its own clipboard and would not hand it
    /// over: the reader declined, or its configuration forbids the read.
    Refused,
}

/// The variants differ a lot in size because they carry whole catalogues and
/// timelines. Boxing them would add an allocation per message for a value that
/// is moved through the channel exactly once, so the payloads stay inline.
#[derive(Debug)]
#[allow(clippy::large_enum_variant)]
pub enum AppMessage {
    Sessions(BackendResult<Vec<AgentSession>>),
    /// What a session's latest turn did, asked for by auto-continue.
    AutoContinueTurnStatus {
        session_id: VibexSessionId,
        updated_at_ms: i64,
        ended_normally: Option<bool>,
    },
    /// The arrangement the authority draws its sidebar from, when it has one.
    SidebarOrganization(BackendResult<vibex_core::RemoteSidebarOrganizationSnapshot>),
    /// The arrangement after a change this client asked for.
    SidebarOrganizationMutated(BackendResult<vibex_core::RemoteSidebarOrganizationSnapshot>),
    /// What a clipboard held when the reader asked for it.
    ///
    /// The answer carries the gesture that asked, because the two of them do
    /// different things with nothing: a paste says so, and the attach action
    /// opens the path prompt.
    Pasted {
        ticket: crate::app::ComposerTicket,
        wanted: crate::app::ClipboardWanted,
        content: ClipboardContent,
    },
    SessionOpened {
        ticket: vibex_ui::AgentSessionLoadTicket,
        result: BackendResult<vibex_ui::AgentSessionSnapshot>,
    },
    /// One page of history fetched with the `before` cursor.
    OlderTimeline {
        ticket: vibex_ui::AgentTimelineBeforeTicket,
        result: BackendResult<vibex_core::TimelinePage>,
    },
    TimelineRefreshed(BackendResult<i64>),
    RuntimeOptions(BackendResult<vibex_core::SessionRuntimeOptionCatalog>),
    SessionCreated {
        request_id: VibexSessionId,
        result: BackendResult<AgentSession>,
    },
    SessionForked {
        request_id: VibexSessionId,
        result: BackendResult<AgentSession>,
    },
    MessageSent {
        session_id: VibexSessionId,
        send_id: u64,
        result: BackendResult<Vec<vibex_core::TimelineItem>>,
    },
    Mutation {
        key: String,
        result: BackendResult<()>,
    },
    Workspaces(Vec<vibex_backend::WorkspaceSummary>),
    WorkspaceOpened {
        draft_id: VibexSessionId,
        navigation_serial: u64,
        result: BackendResult<vibex_backend::WorkspaceSummary>,
    },
    DirectoryListing(BackendResult<vibex_core::RemoteWorkspaceDirectoryListing>),
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
    Completions {
        ticket: crate::app::ComposerTicket,
        result: BackendResult<Box<vibex_core::AgentCommandDiscovery>>,
    },
    Usage(BackendResult<Box<UsageReport>>),
    Clipboard(BackendResult<()>),
    EditorFinished {
        ticket: Option<crate::app::ComposerTicket>,
        result: BackendResult<Option<String>>,
    },
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

/// List one directory of *this* machine for the workspace picker.
///
/// The shape is the authority's answer, because the picker draws both the same
/// way: directories only, hidden names skipped, case-insensitively sorted, and
/// a parent the reader can walk up to — absent at the filesystem root, which is
/// what makes `Up` there say there is nothing above rather than offering a
/// directory that does not exist. There are no browse roots: those bound a
/// *paired* client, and this branch only runs for a backend that is the
/// authority itself.
///
/// `path` of `None` opens on the reader's home, which is where a directory
/// starts being worth choosing from.
fn local_directory_listing(
    path: Option<String>,
) -> BackendResult<vibex_core::RemoteWorkspaceDirectoryListing> {
    let requested = match path {
        Some(path) if !path.trim().is_empty() => std::path::PathBuf::from(path),
        _ => home_directory(),
    };
    let canonical = requested.canonicalize().map_err(|error| {
        BackendError::failed(
            "directory_unavailable",
            "the requested directory does not exist on this machine",
        )
        .with_recovery_hint(format!(
            "Choose a directory that exists, or walk up from the one you are in ({error})"
        ))
    })?;
    // Blocking, and deliberately so: it is one short read on the worker's
    // runtime, and the same call the desktop's own picker makes from its.
    let read_dir = std::fs::read_dir(&canonical).map_err(|error| {
        BackendError::failed(
            "directory_list_failed",
            format!("the requested directory could not be listed: {error}"),
        )
    })?;
    let mut entries = Vec::new();
    for entry in read_dir.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        // A symlinked directory is browsable; a broken link fails `metadata`
        // and is skipped rather than failing the listing.
        let is_dir = if file_type.is_symlink() {
            entry
                .path()
                .metadata()
                .map(|metadata| metadata.is_dir())
                .unwrap_or(false)
        } else {
            file_type.is_dir()
        };
        if !is_dir {
            continue;
        }
        let name = entry.file_name().to_string_lossy().into_owned();
        if name.starts_with('.') {
            continue;
        }
        entries.push(vibex_core::RemoteWorkspaceDirectoryEntry {
            name,
            path: entry.path().to_string_lossy().into_owned(),
        });
    }
    entries.sort_by(|left, right| {
        left.name
            .to_lowercase()
            .cmp(&right.name.to_lowercase())
            .then_with(|| left.name.cmp(&right.name))
    });
    Ok(vibex_core::RemoteWorkspaceDirectoryListing {
        roots: Vec::new(),
        path: canonical.to_string_lossy().into_owned(),
        parent: canonical
            .parent()
            .map(|parent| parent.to_string_lossy().into_owned()),
        entries,
    })
}

/// The directory a listing starts from when the reader named none.
fn home_directory() -> std::path::PathBuf {
    std::env::var_os("HOME")
        .map(std::path::PathBuf::from)
        .filter(|path| path.is_absolute())
        .unwrap_or_else(|| std::path::PathBuf::from("/"))
}

/// Read the clipboard of the machine this client runs on.
///
/// Image first: a clipboard can hold both, and the picture is the half a
/// terminal cannot paste by itself. Nothing here reaches the reader's own
/// clipboard when the client is on the far side of an `ssh` link — the host has
/// no display to read it from — which is why an empty answer is not the end of
/// the story: the event loop asks the terminal itself next.
fn read_host_clipboard(wanted: crate::app::ClipboardWanted) -> ClipboardContent {
    if let Some((mime_type, bytes)) = crate::terminal::read_clipboard_image() {
        return ClipboardContent::Image { mime_type, bytes };
    }
    if wanted == crate::app::ClipboardWanted::Image {
        // The attach action names a file when there is no picture to take, so
        // a host that has only text is a host with nothing to offer it.
        return ClipboardContent::Empty;
    }
    match crate::terminal::read_clipboard_text() {
        Some(text) if !text.is_empty() => ClipboardContent::Text(text),
        _ => ClipboardContent::Empty,
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
            Effect::ProbeAutoContinue {
                session_id,
                updated_at_ms,
            } => {
                // The latest page is what "the last turn" means: the answer is
                // read off its tail, the way the Desktop reads its own.
                let request = vibex_core::FetchTimelineRequest {
                    session_id: session_id.clone(),
                    after_sequence: None,
                    before_sequence: None,
                    limit: AUTO_CONTINUE_PROBE_LIMIT,
                };
                let page = self.facade.agent().fetch_timeline(request).await;
                let ended_normally = page.ok().and_then(|page| {
                    (page.session_id == session_id)
                        .then(|| vibex_core::latest_timeline_turn_ended_normally(&page.items))
                        .flatten()
                });
                self.send(AppMessage::AutoContinueTurnStatus {
                    session_id,
                    updated_at_ms,
                    ended_normally,
                });
            }
            Effect::LoadSidebarOrganization => {
                let result = self.facade.sidebar().sidebar_organization().await;
                self.send(AppMessage::SidebarOrganization(result));
            }
            Effect::MutateSidebarOrganization {
                mutation,
                expected_revision,
            } => {
                let result = self
                    .facade
                    .sidebar()
                    .mutate_sidebar_organization(mutation, expected_revision)
                    .await;
                self.send(AppMessage::SidebarOrganizationMutated(result));
            }
            Effect::OpenSession { session_id, ticket } => {
                let result = self.load_session(session_id, ticket.after_sequence).await;
                self.send(AppMessage::SessionOpened { ticket, result });
            }
            Effect::LoadOlder { ticket } => {
                let request = vibex_core::FetchTimelineRequest {
                    session_id: ticket.session_id.clone(),
                    after_sequence: None,
                    before_sequence: Some(ticket.before_sequence),
                    limit: vibex_ui::AGENT_TIMELINE_PAGE_LIMIT,
                };
                let result = self.facade.agent().fetch_timeline(request).await;
                self.send(AppMessage::OlderTimeline { ticket, result });
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
                request_id,
                workspace_root,
                title,
                runtime,
            } => {
                // The page may have chosen one; otherwise the first available
                // catalogue entry is what a new session gets.
                let runtime = match runtime {
                    Some(selection) => selection,
                    None => match self.current_runtime_selection().await {
                        Ok(selection) => selection,
                        Err(error) => {
                            self.send(AppMessage::SessionCreated {
                                request_id,
                                result: Err(error),
                            });
                            return;
                        }
                    },
                };
                let mut request = payloads::create_session(workspace_root, title, runtime);
                request.payload.session_id = Some(request_id.clone());
                request.payload.defer_runtime_materialization = true;
                let result = self.facade.agent().create_session(request).await;
                self.send(AppMessage::SessionCreated { request_id, result });
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
            Effect::ForkSession {
                request_id,
                session_id,
            } => {
                let request = payloads::fork_session(session_id);
                let result = self.facade.agent().fork_session(request).await;
                self.send(AppMessage::SessionForked { request_id, result });
            }
            Effect::SendMessage {
                session_id,
                send_id,
                correlation_id,
                text,
                attachments,
            } => {
                let result = match self.session_runtime_selection(&session_id).await {
                    Ok(runtime) => {
                        let mut request =
                            payloads::send_message(session_id.clone(), text, attachments, runtime);
                        request.payload.correlation_id = Some(correlation_id);
                        self.facade.agent().send_message(request).await
                    }
                    Err(error) => Err(error),
                };
                self.send(AppMessage::MessageSent {
                    session_id,
                    send_id,
                    result,
                });
            }
            Effect::ReadClipboard { ticket, wanted } => {
                // Talking to a clipboard owner can block for the whole
                // deadline, so it runs off the worker's own thread.
                let content = tokio::task::spawn_blocking(move || read_host_clipboard(wanted))
                    .await
                    .unwrap_or(ClipboardContent::Empty);
                self.send(AppMessage::Pasted {
                    ticket,
                    wanted,
                    content,
                });
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
                let runtime = match self.session_runtime_selection(&session_id).await {
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
                // The switch is a compare-and-set: the durable revisions are
                // read first, because a stale expectation is refused rather
                // than silently applied to whatever the session has become.
                let state = match self
                    .facade
                    .agent()
                    .runtime_selection(session_id.clone())
                    .await
                {
                    Ok(state) => state,
                    Err(error) => {
                        self.failure("switch_runtime", error);
                        return;
                    }
                };
                let request =
                    MutationRequest::new(vibex_core::SetDesiredAgentSessionRuntimeRequest {
                        session_id,
                        idempotency_key: vibex_core::RequestId::new().as_str().to_string(),
                        expected_revision: state.session_revision,
                        expected_selection_revision: state.selection_revision,
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
            Effect::OpenWorkspace {
                draft_id,
                navigation_serial,
                root_path,
            } => {
                let request = MutationRequest::new(vibex_core::OpenWorkspaceRequest {
                    root_path,
                    mode: None,
                });
                match self.facade.workspace().open_workspace(request).await {
                    Ok(summary) => self.send(AppMessage::WorkspaceOpened {
                        draft_id,
                        navigation_serial,
                        result: Ok(summary),
                    }),
                    Err(error) => self.send(AppMessage::WorkspaceOpened {
                        draft_id,
                        navigation_serial,
                        result: Err(error),
                    }),
                }
            }
            Effect::BrowseDirectories { path } => {
                // The listing belongs to the machine the Agent will run on. A
                // paired authority owns that machine and answers over the wire;
                // a native backend *is* that machine and browses locally — it
                // deliberately does not report `WorkspaceBrowseDirectories`, so
                // the capability is what says which of the two this is. Asking
                // a native backend for the authority's directories is what made
                // the picker open empty and say the backend could not browse.
                let authority_owned = self
                    .facade
                    .capabilities()
                    .workspace
                    .supports(vibex_backend::BackendOperation::WorkspaceBrowseDirectories);
                let result = if authority_owned {
                    self.facade
                        .workspace()
                        .browse_authority_directories(path)
                        .await
                } else {
                    local_directory_listing(path)
                };
                self.send(AppMessage::DirectoryListing(result));
            }
            Effect::UpdateEntry { entry } => {
                let result = self.update_entry(entry).await;
                match result {
                    Ok(()) => self.ok("update_entry"),
                    Err(error) => self.failure("update_entry", error),
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
            Effect::DiscoverCompletions {
                ticket,
                trigger,
                query,
            } => {
                // The authority owns the command catalogue: `/` merges the
                // Agent's own commands with Vibex Prompts, `@` resolves
                // workspace files, and `$` resolves Skills. None of that is
                // local knowledge, so the composer asks rather than guessing.
                let session = match ticket.target.as_ref() {
                    Some(crate::app::ComposerTarget::Session(id)) => {
                        self.facade.agent().open_session(id.clone()).await.ok()
                    }
                    _ => None,
                };
                let request = vibex_core::AgentCommandDiscoverRequest {
                    agent_id: ticket
                        .runtime
                        .as_ref()
                        .map(|runtime| runtime.agent_id.clone())
                        .or_else(|| session.as_ref().map(|session| session.agent_id.clone())),
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
                self.send(AppMessage::Completions {
                    ticket,
                    result: result.map(Box::new),
                });
            }
            Effect::CheckDrift => {}
            Effect::EditExternally {
                ticket,
                title,
                body,
            } => {
                let result = crate::terminal::edit_in_editor(&title, &body);
                self.send(AppMessage::EditorFinished { ticket, result });
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
        after_sequence: i64,
    ) -> BackendResult<vibex_ui::AgentSessionSnapshot> {
        let session = self.facade.agent().open_session(session_id.clone()).await?;
        // Hydration is a bounded window rather than the whole archive: the
        // newest page on a cold open, the tail after a restored cached window
        // otherwise. Older pages are fetched on demand as the reader scrolls
        // up, which is what keeps a 100 000-item session usable.
        let timeline = self
            .facade
            .agent()
            .fetch_timeline(vibex_core::FetchTimelineRequest {
                session_id: session_id.clone(),
                after_sequence: (after_sequence > 0).then_some(after_sequence),
                before_sequence: None,
                limit: vibex_ui::AGENT_TIMELINE_PAGE_LIMIT,
            })
            .await?;
        if timeline.session_id != session_id
            || timeline.items.iter().any(|item| {
                item.session_id != session_id
                    || (after_sequence > 0 && item.sequence <= after_sequence)
            })
        {
            return Err(BackendError::failed(
                "agent_timeline_page_invalid",
                "the backend returned an invalid Agent timeline page",
            ));
        }
        // Best effort, and only where the backend can describe it: the
        // authoritative timeline still renders when a provider or a remote
        // device cannot expose runtime-selection details, and the composer then
        // simply has no Agent and model to name.
        let include_runtime = self
            .facade
            .capabilities()
            .agent
            .supports(vibex_backend::BackendOperation::AgentSwitchRuntime);
        let runtime_selection = if include_runtime {
            self.facade
                .agent()
                .runtime_selection(session_id.clone())
                .await
                .ok()
        } else {
            None
        };
        Ok(vibex_ui::AgentSessionSnapshot {
            session,
            timeline: timeline.items,
            runtime_selection,
            timeline_has_older: timeline.has_older,
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

    /// The runtime selection an *existing* session is already using.
    ///
    /// The selection carried by a message is authoritative, so sending the
    /// client's preferred catalogue entry would move the session onto another
    /// Agent as a side effect of typing into it. Read the session's own desired
    /// selection instead; only a session that predates runtime-selection state
    /// falls back to the catalogue, and then only to an option belonging to the
    /// Agent the session already records.
    async fn session_runtime_selection(
        &self,
        session_id: &VibexSessionId,
    ) -> BackendResult<vibex_core::SessionRuntimeSelection> {
        if let Ok(state) = self
            .facade
            .agent()
            .runtime_selection(session_id.clone())
            .await
        {
            return Ok(state.desired);
        }
        let session = self.facade.agent().open_session(session_id.clone()).await?;
        let catalog = self.facade.agent().list_runtime_options().await?;
        // Only an option belonging to the Agent the session already records may
        // stand in: the point of the fallback is to send through the session's
        // own Agent, never to move it onto another one.
        let agent_id = session.agent_id;
        let for_agent =
            |option: &&vibex_core::SessionRuntimeOption| option.selection.agent_id == agent_id;
        catalog
            .options
            .iter()
            .filter(|option| for_agent(option))
            .find(|option| option.availability == vibex_core::RuntimeOptionAvailability::Available)
            .or_else(|| catalog.options.iter().find(|option| for_agent(option)))
            .map(|option| option.selection.clone())
            .ok_or_else(|| {
                BackendError::failed(
                    "agent_runtime_unavailable",
                    "the session's Agent has no runtime option to send through",
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

    #[test]
    fn a_local_listing_offers_directories_and_a_way_up() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let inside = root.path().join("clash-report");
        std::fs::create_dir(&inside).expect("a directory");
        std::fs::create_dir(root.path().join(".hidden")).expect("a hidden directory");
        std::fs::write(root.path().join("notes.md"), "x").expect("a file");

        let listing = local_directory_listing(Some(root.path().to_string_lossy().into_owned()))
            .expect("the directory lists");
        let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
        assert_eq!(
            names,
            vec!["clash-report"],
            "a file or a hidden name reached the picker: {names:?}"
        );
        assert_eq!(listing.path, inside.parent().unwrap().to_string_lossy());
        assert_eq!(
            listing.parent.as_deref(),
            Some(root.path().parent().unwrap().to_string_lossy().as_ref()),
            "the reader cannot walk out of the directory they opened"
        );
        assert!(listing.roots.is_empty(), "a local listing has no roots");

        // The entry carries the path the picker will hand back, not just a name.
        assert_eq!(
            listing.entries[0].path,
            inside.to_string_lossy(),
            "the entry does not name the directory it stands for"
        );
    }

    #[test]
    fn a_local_listing_refuses_a_directory_that_is_not_one() {
        let root = tempfile::tempdir().expect("a temporary directory");
        let file = root.path().join("notes.md");
        std::fs::write(&file, "x").expect("a file");
        let error = local_directory_listing(Some(file.to_string_lossy().into_owned()))
            .expect_err("a file is not a directory to browse");
        assert_eq!(error.code, "directory_list_failed");
    }
}

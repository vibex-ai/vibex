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
    /// What a resource kind can be imported from. `mcp` and `skill` are the
    /// same answer for two different modules, so the module travels with it
    /// rather than the client guessing from which field is set.
    ConfigSources {
        module: crate::config_center::ConfigModule,
        mcp: BackendResult<Vec<vibex_core::McpServerDiscovery>>,
        skill: Option<BackendResult<Vec<vibex_core::SkillDiscovery>>>,
    },
    /// One page of a registry search.
    Registry {
        module: crate::config_center::ConfigModule,
        mcp: BackendResult<Vec<vibex_core::McpMarketEntry>>,
        skill: Option<BackendResult<Vec<vibex_core::SkillMarketEntry>>>,
        /// What the registry said about the search itself, when it said
        /// anything: how much of the catalogue the page covers.
        note: Option<crate::app::MarketNote>,
    },
    /// A configuration write finished, so the area it changed is re-read.
    ConfigReload,
    TeamLoaded {
        ticket: vibex_ui::TeamLoadTicket,
        result: BackendResult<vibex_core::TeamSnapshot>,
    },
    TeamMutated {
        session_id: VibexSessionId,
        result: BackendResult<()>,
    },
    Sessions(BackendResult<Vec<AgentSession>>),
    /// One page of the session ownership tree: the children of one session, or
    /// the ownership roots themselves.
    ///
    /// `parent_session_id` is the page's own key rather than the open session's,
    /// so an answer that lands after the reader moved on is filed where it was
    /// asked for instead of over another branch.
    SessionTree {
        parent_session_id: Option<VibexSessionId>,
        append: bool,
        result: BackendResult<vibex_core::SessionTreePage>,
    },
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
    /// The full payload behind one activity the reader opened.
    TimelineItem {
        ticket: vibex_ui::AgentTimelineItemTicket,
        result: BackendResult<vibex_core::TimelineItem>,
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
    /// A write finished: what it answered, and which record it was about.
    ///
    /// `key` is the operation, as the effect named it, because the client uses
    /// it to decide what to re-read. `record` is the row the reader pressed the
    /// verb on, which is what the answer is filed under — the two are not the
    /// same thing, and the reader only ever sees the second.
    Mutation {
        key: String,
        record: Option<String>,
        result: BackendResult<crate::app::MutationOutcome>,
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
    /// The offer the runtime now holds, answering a cancel or a status read.
    /// The summary carries no secret: the code was shown once and is not
    /// travelling back.
    PairingOfferStatus(BackendResult<Box<vibex_core::RemotePairingOfferSummary>>),
    /// What the runtime answered when asked how it publishes itself. An empty
    /// list is the answer that explains an empty pairing page.
    PairingPublications(BackendResult<Vec<vibex_core::RemotePairingPublicationSummary>>),
    /// Every LAN-pairing answer: opening a window, re-reading one, deciding a
    /// request, and taking it down all answer with the window the runtime now
    /// holds. `None` is a state rather than an error — the window is gone.
    LanPairing(BackendResult<Option<Box<vibex_core::RemoteLanPairingWindowSnapshot>>>),
    Audit(BackendResult<Vec<vibex_core::RemoteAuditRecord>>),
    Profiles(BackendResult<Vec<vibex_core::ProviderProfileSummary>>),
    Gateway(BackendResult<vibex_core::GatewaySnapshot>),
    GatewayRequests(BackendResult<Vec<vibex_core::GatewayRequestSummary>>),
    /// One profile as the authority holds it, read for an editor.
    ProviderProfile(BackendResult<vibex_core::ProviderProfile>),
    /// One Agent's profile order, as the authority holds it.
    ProviderOrder(BackendResult<Vec<vibex_core::AgentModelProviderDisplayOrderEntry>>),
    /// What other CLI tools on this machine hold.
    NativeImport(BackendResult<vibex_core::ProviderNativeImportPreview>),
    /// Which built-in tool panels each Agent receives.
    AgentTools(BackendResult<vibex_core::AgentToolPreferences>),
    /// One registry Skill, read in full.
    SkillMarketDocument(BackendResult<Box<vibex_core::SkillMarketDocument>>),
    /// The registry page after the one on screen.
    SkillMarketPage(BackendResult<Box<vibex_core::SkillMarketSearchResponse>>),
    /// What one profile's export would write.
    NativeExport(BackendResult<Box<vibex_core::ProviderNativeExportPreview>>),
    /// What has been exported for one profile.
    NativeExportHistory(BackendResult<Vec<vibex_core::ProviderNativeExportRecordSummary>>),
    /// One Agent's account, and the ways in it advertises.
    AgentAuthCatalog(BackendResult<Box<vibex_core::AgentAuthCatalog>>),
    /// What signing out of one Agent's account would end.
    AgentAuthLogoutPreview(BackendResult<Box<vibex_core::AgentAuthContextLogoutPreview>>),
    ProviderTest(BackendResult<vibex_core::AgentModelProviderProfileTestResult>),
    ProviderModels(BackendResult<Vec<vibex_core::ProviderConfiguredModel>>),
    Agents(BackendResult<Vec<vibex_core::AgentSnapshotEntry>>),
    /// One Agent's accounts, and whether the reader asked to see them. A read
    /// that answers a verb opens the account panel; a re-read that follows a
    /// sign-out only refreshes the row.
    AgentAuth {
        result: BackendResult<Vec<vibex_core::AgentAuthContext>>,
        open: bool,
    },
    /// The bytes a sign-in terminal produced, in order.
    ///
    /// The bytes stay bytes: a frame can split a UTF-8 character, and the
    /// emulator that has to hold the tail lives on the client side.
    AgentAuthTerminalFrame {
        agent_id: vibex_core::AgentId,
        terminal_id: vibex_core::TerminalId,
        bytes: Vec<u8>,
    },
    /// The terminal a sign-in method opened, and what the authority says about
    /// it. An error means the client could not attach, which the panel says
    /// rather than claiming a terminal it is not drawing.
    AgentAuthTerminal {
        agent_id: vibex_core::AgentId,
        terminal_id: vibex_core::TerminalId,
        title: String,
        result: BackendResult<()>,
    },
    /// A sign-in terminal's frame stream ended.
    AgentAuthTerminalEnded {
        terminal_id: vibex_core::TerminalId,
    },
    Mcp(BackendResult<Vec<vibex_core::McpServer>>),
    Skills(BackendResult<Vec<vibex_core::Skill>>),
    Prompts(BackendResult<Vec<vibex_core::Prompt>>),
    Completions {
        ticket: crate::app::ComposerTicket,
        result: BackendResult<Box<vibex_core::AgentCommandDiscovery>>,
    },
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

/// What an install or a rollback left behind.
///
/// The authority answers these two with the record's whole managed-install
/// state, so the version is read off it. A state that came back failed is not
/// an error the transport noticed — the request succeeded and the installer did
/// not — so it is reported as the verdict it is rather than as a failure of the
/// call.
fn install_outcome(state: &vibex_core::AgentManagedInstallState) -> crate::app::MutationOutcome {
    use crate::app::MutationOutcome;
    if let Some(message) = install_failure(state) {
        return MutationOutcome::Verdict {
            passed: false,
            warned: false,
            message,
        };
    }
    MutationOutcome::Installed {
        version: state.installed_version.clone(),
    }
}

/// What a check for updates found.
///
/// "Up to date" and "a version is waiting" are the two answers the verb exists
/// to tell apart: the reader pressed it to find out which one is true.
fn check_outcome(state: &vibex_core::AgentManagedInstallState) -> crate::app::MutationOutcome {
    use crate::app::MutationOutcome;
    if let Some(message) = install_failure(state) {
        return MutationOutcome::Verdict {
            passed: false,
            warned: false,
            message,
        };
    }
    match state.available_version.clone() {
        Some(available) if state.installed_version.as_deref() != Some(available.as_str()) => {
            MutationOutcome::UpdateAvailable { version: available }
        }
        _ => MutationOutcome::UpToDate,
    }
}

/// The reason a managed-install state is a failure, if it is one.
fn install_failure(state: &vibex_core::AgentManagedInstallState) -> Option<String> {
    (state.status == vibex_core::AgentManagedInstallStatus::Failed).then(|| {
        state
            .last_error_message
            .clone()
            .or_else(|| state.last_error_code.clone())
            .unwrap_or_else(|| "the installer failed".to_string())
    })
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

    /// Hand one message back, saying whether the interface is still listening.
    ///
    /// A frame pump loops until the receiver goes away, and it needs to tell
    /// "sent" from "dropped": the client closed, so the loop must stop rather
    /// than keep reading a terminal nobody is looking at.
    fn try_send(&self, message: AppMessage) -> bool {
        self.sender.send(message).is_ok()
    }

    /// Search one registry and hand the page back.
    ///
    /// The two registries page differently and the request follows each: MCP is
    /// addressed by offset and asked for exactly the page the reader is on,
    /// while the Skill registry answers a cursor it hands back and reports no
    /// total, so one window is fetched and the client cuts the pages out of it.
    async fn search_registry(
        &mut self,
        module: crate::config_center::ConfigModule,
        query: String,
        registry: crate::config_center::RegistryQuery,
    ) {
        use crate::config_center::{ConfigModule, MARKET_PAGE_SIZE, SKILL_MARKET_FETCH_LIMIT};
        let message = match module {
            ConfigModule::Mcp => {
                let offset = registry.page.saturating_mul(MARKET_PAGE_SIZE);
                let request = vibex_core::McpMarketSearchRequest {
                    query: (!query.trim().is_empty()).then_some(query),
                    limit: Some(MARKET_PAGE_SIZE as u32),
                    offset: Some(offset as u32),
                    // The registry pages by an opaque cursor, but the table
                    // counts pages, so the runtime walks the cursor until the
                    // offset is covered and cuts the window from what it read.
                    cursor: None,
                    transport: registry.transport_filter(),
                };
                match self.facade.management().search_mcp_market(request).await {
                    Ok(response) => AppMessage::Registry {
                        module,
                        note: Some(crate::app::MarketNote {
                            matched: Some(response.total_matches),
                            exhausted: !response.has_more,
                            // The MCP market pages by offset: the runtime
                            // walks the registry's cursor on the reader's
                            // behalf, so the table names the page it wants.
                            cursor: None,
                            error: None,
                        }),
                        mcp: Ok(response.entries),
                        skill: None,
                    },
                    Err(error) => AppMessage::Registry {
                        module,
                        note: Some(crate::app::MarketNote {
                            matched: None,
                            exhausted: true,
                            cursor: None,
                            error: Some((error.code.clone(), error.message.clone())),
                        }),
                        mcp: Err(error.clone()),
                        skill: Some(Err(error)),
                    },
                }
            }
            _ => {
                let request = vibex_core::SkillMarketSearchRequest {
                    query: (!query.trim().is_empty()).then_some(query),
                    limit: Some(SKILL_MARKET_FETCH_LIMIT as u32),
                    cursor: None,
                    sort: Some(registry.sort.wire()),
                };
                match self.facade.management().search_skill_market(request).await {
                    Ok(response) => AppMessage::Registry {
                        module,
                        note: Some(crate::app::MarketNote {
                            // The window is the denominator: the registry
                            // reports no total, so the line counts what was
                            // handed over rather than inventing one.
                            matched: Some(response.entries.len()),
                            exhausted: !response.has_more,
                            cursor: response.next_cursor.clone(),
                            error: None,
                        }),
                        mcp: Ok(Vec::new()),
                        skill: Some(Ok(response.entries)),
                    },
                    Err(error) => AppMessage::Registry {
                        module,
                        note: Some(crate::app::MarketNote {
                            matched: None,
                            exhausted: true,
                            cursor: None,
                            error: Some((error.code.clone(), error.message.clone())),
                        }),
                        mcp: Err(error.clone()),
                        skill: Some(Err(error)),
                    },
                }
            }
        };
        self.send(message);
    }

    /// Issue one configuration-centre write and report what it did.
    ///
    /// Every arm ends the same way: report what the authority answered under
    /// the record the write was about, and ask the client to re-read the area.
    /// That is deliberate — the authority is what decides what a record now
    /// looks like, and a client that patched its own copy would be guessing at
    /// the parts of the record the request did not carry. What the answer
    /// carries instead is the part the re-read cannot show: the version that
    /// landed, the verdict a validation returned, whether an account is signed
    /// in.
    async fn config_mutation(&mut self, mutation: crate::app::ConfigMutation, record: String) {
        use crate::app::ConfigMutation as M;
        use crate::app::MutationOutcome as Outcome;
        use vibex_backend::MutationRequest;
        let key = mutation.label().to_string();
        let result: BackendResult<Outcome> = match mutation {
            M::ToggleAgent { agent_id, enabled } => {
                let request = MutationRequest::new(vibex_core::AgentUpdateConfigRequest {
                    agent_id,
                    added: None,
                    enabled: Some(enabled),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                });
                self.facade
                    .management()
                    .update_agent_config(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::RefreshAgentSnapshot { agent_id } => {
                let request = vibex_core::AgentRefreshSnapshotRequest {
                    agent_id,
                    cwd_scope: None,
                };
                self.facade
                    .management()
                    .refresh_agent_snapshot(request)
                    .await
                    .map(|response| Outcome::Models {
                        count: response.agent.models.len(),
                    })
            }
            M::CheckAgentUpdate { agent_id } => {
                let request = MutationRequest::new(agent_id);
                self.facade
                    .management()
                    .check_managed_agent_update(request)
                    .await
                    .map(|state| check_outcome(&state))
            }
            M::RollbackAgent { agent_id } => {
                let request = MutationRequest::new(agent_id);
                self.facade
                    .management()
                    .rollback_managed_agent(request)
                    .await
                    .map(|state| install_outcome(&state))
            }
            M::ApplyNativeExport { export_id } => {
                let Ok(export_id) = vibex_core::RequestId::parse(export_id) else {
                    return self.failure_for(
                        "provider_native_export_apply",
                        None,
                        BackendError::failed(
                            "provider_native_export_unknown",
                            "the export is no longer listed",
                        ),
                    );
                };
                let request = MutationRequest::new(vibex_core::ProviderNativeExportApplyRequest {
                    export_id,
                });
                self.facade
                    .management()
                    .apply_native_export(request)
                    .await
                    .map(|result| crate::app::MutationOutcome::Verdict {
                        passed: matches!(
                            result.status,
                            vibex_core::ProviderNativeExportApplyStatus::Applied
                        ),
                        warned: matches!(
                            result.status,
                            vibex_core::ProviderNativeExportApplyStatus::PartiallyApplied
                        ),
                        message: format!("{} {}", result.files.len(), "files"),
                    })
            }
            M::RollbackNativeExport { export_id } => {
                let Ok(export_id) = vibex_core::RequestId::parse(export_id) else {
                    return self.failure_for(
                        "provider_native_export_rollback",
                        None,
                        BackendError::failed(
                            "provider_native_export_unknown",
                            "the export is no longer listed",
                        ),
                    );
                };
                let request =
                    MutationRequest::new(vibex_core::ProviderNativeExportRollbackRequest {
                        export_id,
                    });
                self.facade
                    .management()
                    .rollback_native_export(request)
                    .await
                    .map(|result| crate::app::MutationOutcome::Verdict {
                        passed: matches!(
                            result.status,
                            vibex_core::ProviderNativeExportRollbackStatus::Restored
                        ),
                        warned: false,
                        message: format!("{} {}", result.files.len(), "files"),
                    })
            }
            M::LogoutAgentAuthContext {
                auth_context_id,
                expected_context_revision,
                confirmed_affected_session_count,
            } => {
                self.logout_agent_auth_context(
                    auth_context_id,
                    expected_context_revision,
                    confirmed_affected_session_count,
                )
                .await
            }
            M::SetAgentTool {
                agent_id,
                tool,
                enabled,
            } => {
                let request = vibex_backend::MutationRequest::new(
                    vibex_core::AgentToolPreferenceUpdateRequest {
                        agent_id,
                        tool,
                        enabled,
                    },
                );
                match self.facade.agent().set_agent_tool_enabled(request).await {
                    // The whole answer is what comes back, so the client keeps
                    // the authority's record rather than its own guess at the
                    // other panel's state.
                    Ok(preferences) => {
                        self.send(AppMessage::AgentTools(Ok(preferences)));
                        Ok(crate::app::MutationOutcome::Plain)
                    }
                    Err(error) => Err(error),
                }
            }
            M::SwapAgentOrder {
                first,
                first_index,
                second,
                second_index,
            } => {
                // Two writes, one answer: the roster is a list, and a swap that
                // landed only half of itself would be an order nobody chose.
                self.swap_agent_order(first, first_index, second, second_index)
                    .await
            }
            M::SetAgentAdded { agent_id, added } => {
                // The roster is what the reader wants to see; nothing on disk
                // changes either way, which is why this is not install or
                // uninstall.
                let request =
                    vibex_backend::MutationRequest::new(vibex_core::AgentUpdateConfigRequest {
                        agent_id,
                        added: Some(added),
                        enabled: None,
                        label_override: None,
                        description_override: None,
                        order_index: None,
                        command: None,
                        env: None,
                        params: None,
                    });
                self.facade
                    .management()
                    .update_agent_config(request)
                    .await
                    .map(|_| crate::app::MutationOutcome::Plain)
            }
            M::CreateCustomAgent(request) => {
                let request = vibex_backend::MutationRequest::new(*request);
                self.facade
                    .management()
                    .create_custom_agent(request)
                    .await
                    .map(|_| crate::app::MutationOutcome::Verdict {
                        passed: true,
                        warned: false,
                        message: String::new(),
                    })
            }
            M::AuthenticateAgentMethod {
                agent_id,
                method_id,
                values,
            } => {
                self.authenticate_agent_method(agent_id, method_id, values)
                    .await
            }
            M::ImportNativeProfile { item_id, sources } => {
                let Ok(import_item_id) = vibex_core::RequestId::parse(item_id) else {
                    return self.failure_for(
                        "provider_native_import_run",
                        None,
                        BackendError::failed(
                            "provider_native_import_item_unknown",
                            "the import candidate is no longer listed",
                        ),
                    );
                };
                let request = MutationRequest::new(vibex_core::ProviderNativeImportCreateRequest {
                    preview_request: vibex_core::ProviderNativeImportPreviewRequest { sources },
                    import_item_id,
                });
                self.facade
                    .management()
                    .create_profile_from_import(request)
                    .await
                    .map(|result| crate::app::MutationOutcome::Installed {
                        version: Some(result.profile.display_name.clone()),
                    })
            }
            M::SetProviderOrder { agent_id, profiles } => {
                let request =
                    MutationRequest::new(vibex_core::AgentModelProviderDisplayOrderSetRequest {
                        agent_id,
                        entries: profiles
                            .into_iter()
                            .map(|provider_profile_id| {
                                vibex_core::AgentModelProviderDisplayOrderSetEntry {
                                    provider_profile_id,
                                }
                            })
                            .collect(),
                    });
                self.facade
                    .management()
                    .set_agent_model_provider_display_order(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::CreateProviderProfile(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .create_agent_model_provider_profile(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::UpdateProviderProfile {
                agent_id,
                profile_id,
                display_name,
                base_url,
                default_model,
                configured_models,
                provider_options,
            } => {
                let request =
                    MutationRequest::new(vibex_core::AgentModelProviderProfileUpdateRequest {
                        agent_id,
                        provider_profile_id: profile_id,
                        display_name,
                        status: None,
                        account_alias: None,
                        base_url,
                        default_model,
                        small_model: None,
                        large_model: None,
                        configured_models,
                        reasoning_effort: None,
                        sandbox_defaults: None,
                        network_defaults: None,
                        permission_defaults: None,
                        provider_options: provider_options.map(|options| *options),
                    });
                self.facade
                    .management()
                    .update_agent_model_provider_profile(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DeleteProviderProfile {
                agent_id,
                profile_id,
            } => {
                let request =
                    MutationRequest::new(vibex_core::AgentModelProviderProfileDeleteRequest {
                        agent_id,
                        provider_profile_id: profile_id,
                    });
                self.facade
                    .management()
                    .delete_agent_model_provider_profile(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::CreateMcpServer(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .create_mcp_server(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::UpdateMcpServer(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .update_mcp_server(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DeleteMcpServer { server_id } => {
                let request = MutationRequest::new(vibex_core::McpServerDeleteRequest {
                    mcp_server_id: server_id,
                });
                self.facade
                    .management()
                    .delete_mcp_server(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::ValidateMcpServer { server_id } => {
                let request = vibex_core::McpServerValidateRequest {
                    mcp_server_id: Some(server_id),
                    candidate: None,
                };
                self.facade
                    .management()
                    .validate_mcp_server(request)
                    .await
                    .map(|result| {
                        use vibex_core::McpServerValidationStatus as Status;
                        Outcome::verdict(
                            result.status == Status::Warn,
                            result.status == Status::Fail,
                            &result.code,
                            &result.message,
                        )
                    })
            }
            M::SetMcpAgentMatrix { server_id, matrix } => {
                let request = MutationRequest::new(vibex_core::McpServerSetAgentMatrixRequest {
                    mcp_server_id: server_id,
                    agent_matrix: matrix,
                });
                self.facade
                    .management()
                    .set_mcp_server_agent_matrix(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DiscoverMcpSources => {
                let request = vibex_core::McpServerDiscoverRequest {
                    source_agent_id: None,
                };
                let result = self.facade.management().discover_mcp_sources(request).await;
                self.send(AppMessage::ConfigSources {
                    module: crate::config_center::ConfigModule::Mcp,
                    mcp: result.map(|response| response.discoveries),
                    skill: None,
                });
                Ok(Outcome::Plain)
            }
            M::ImportMcpServer { selection } => {
                let request = MutationRequest::new(vibex_core::McpServerImportRequest {
                    selections: vec![*selection],
                });
                self.facade
                    .management()
                    .import_mcp_servers(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::InstallMcpMarketEntry {
                entry_id,
                candidate,
                env_values,
                agent_ids,
            } => {
                let request = MutationRequest::new(vibex_core::McpMarketInstallRequest {
                    entry_id,
                    agent_ids,
                    env_values,
                    candidate: *candidate,
                });
                self.facade
                    .management()
                    .install_mcp_market_entry(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::ValidateMcpMarketEntry {
                entry_id: _,
                candidate,
            } => {
                // The entry is not a record yet, so the check names the
                // candidate the registry published instead of an identifier.
                let request = vibex_core::McpServerValidateRequest {
                    mcp_server_id: None,
                    candidate: Some(*candidate),
                };
                self.facade
                    .management()
                    .validate_mcp_server(request)
                    .await
                    .map(|result| {
                        use vibex_core::McpServerValidationStatus as Status;
                        Outcome::verdict(
                            result.status == Status::Warn,
                            result.status == Status::Fail,
                            &result.code,
                            &result.message,
                        )
                    })
            }
            M::CreateSkill(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .create_skill(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::UpdateSkill(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .update_skill(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DeleteSkill { skill_id } => {
                let request = MutationRequest::new(vibex_core::SkillDeleteRequest { skill_id });
                self.facade
                    .management()
                    .delete_skill(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::ValidateSkill { skill_id } => {
                let request = vibex_core::SkillValidateRequest {
                    skill_id: Some(skill_id),
                    candidate: None,
                };
                self.facade
                    .management()
                    .validate_skill(request)
                    .await
                    .map(|result| {
                        use vibex_core::SkillValidationStatus as Status;
                        Outcome::verdict(
                            result.status == Status::Warn,
                            result.status == Status::Fail,
                            &result.code,
                            &result.message,
                        )
                    })
            }
            M::SetSkillAgentMatrix { skill_id, matrix } => {
                let request = MutationRequest::new(vibex_core::SkillSetAgentMatrixRequest {
                    skill_id,
                    agent_matrix: matrix,
                });
                self.facade
                    .management()
                    .set_skill_agent_matrix(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DiscoverSkillSources => {
                let request = vibex_core::SkillDiscoverRequest {
                    source_agent_id: None,
                    workspace_id: None,
                };
                let result = self
                    .facade
                    .management()
                    .discover_skill_sources(request)
                    .await;
                self.send(AppMessage::ConfigSources {
                    module: crate::config_center::ConfigModule::Skills,
                    mcp: Ok(Vec::new()),
                    skill: Some(result.map(|response| response.discoveries)),
                });
                Ok(Outcome::Plain)
            }
            M::ImportSkill { selection } => {
                let request = MutationRequest::new(vibex_core::SkillImportRequest {
                    selections: vec![*selection],
                });
                self.facade
                    .management()
                    .import_skills(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::InstallSkillMarketEntry {
                entry_id,
                slug,
                owner_handle,
                version,
                agent_ids,
                document,
            } => {
                // The document the reader read travels with the write. Only a
                // client that could not read one asks the registry again, and
                // then it is asking for exactly what it could not show.
                let read = match document {
                    Some(document) => Ok(*document),
                    None => {
                        let document_request = vibex_core::SkillMarketDocumentRequest {
                            entry_id: entry_id.clone(),
                            slug,
                            owner_handle,
                            version,
                        };
                        self.facade
                            .management()
                            .skill_market_document(document_request)
                            .await
                    }
                };
                match read {
                    Ok(document) => {
                        let request = MutationRequest::new(vibex_core::SkillMarketInstallRequest {
                            entry_id,
                            agent_ids,
                            document,
                        });
                        self.facade
                            .management()
                            .install_skill_market_entry(request)
                            .await
                            .map(|_| Outcome::Plain)
                    }
                    Err(error) => Err(error),
                }
            }
            M::CreatePrompt(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .create_prompt(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::UpdatePrompt(request) => {
                let request = MutationRequest::new(*request);
                self.facade
                    .management()
                    .update_prompt(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::DeletePrompt { prompt_id } => {
                let request = MutationRequest::new(vibex_core::PromptDeleteRequest { prompt_id });
                self.facade
                    .management()
                    .delete_prompt(request)
                    .await
                    .map(|_| Outcome::Plain)
            }
            M::ValidatePrompt { prompt_id } => {
                let request = vibex_core::PromptValidateRequest {
                    prompt_id: Some(prompt_id),
                    candidate: None,
                };
                self.facade
                    .management()
                    .validate_prompt(request)
                    .await
                    .map(|result| {
                        use vibex_core::PromptValidationStatus as Status;
                        Outcome::verdict(
                            result.status == Status::Warn,
                            result.status == Status::Fail,
                            &result.code,
                            &result.message,
                        )
                    })
            }
        };
        match result {
            Ok(outcome) => {
                self.send(AppMessage::Mutation {
                    key: key.clone(),
                    record: Some(record),
                    result: Ok(outcome),
                });
                self.send(AppMessage::ConfigReload);
            }
            Err(error) => self.config_failed(&key, Some(record), error),
        }
    }

    fn config_failed(&mut self, key: &str, record: Option<String>, error: BackendError) {
        self.send(AppMessage::Mutation {
            key: key.to_string(),
            record,
            result: Err(error),
        });
        self.send(AppMessage::ConfigReload);
    }

    /// A write outside the configuration centre: there is no row, and nothing
    /// to say beyond whether it worked.
    fn failure(&self, key: &str, error: BackendError) {
        self.failure_for(key, None, error);
    }

    fn failure_for(&self, key: &str, record: Option<String>, error: BackendError) {
        self.send(AppMessage::Mutation {
            key: key.to_string(),
            record,
            result: Err(error),
        });
    }

    fn ok(&self, key: &str) {
        self.ok_with(key, None, crate::app::MutationOutcome::Plain);
    }

    /// A write that answered something: the record files the answer, and the
    /// outcome is what it says.
    fn ok_with(&self, key: &str, record: Option<String>, outcome: crate::app::MutationOutcome) {
        self.send(AppMessage::Mutation {
            key: key.to_string(),
            record,
            result: Ok(outcome),
        });
    }

    async fn run(&mut self, effect: Effect) {
        match effect {
            Effect::ListSessions { include_archived } => {
                let result = self.facade.agent().list_sessions(include_archived).await;
                self.send(AppMessage::Sessions(result));
            }
            Effect::LoadSessionTree {
                parent_session_id,
                cursor,
                append,
            } => {
                let request = vibex_core::SessionTreeRequest {
                    parent_session_id: parent_session_id.clone(),
                    cursor,
                    limit: Some(crate::children::CHILD_PAGE_LIMIT),
                    include_archived: false,
                };
                let result = self.facade.agent().session_tree(request).await;
                self.send(AppMessage::SessionTree {
                    parent_session_id,
                    append,
                    result,
                });
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
                    max_turns: None,
                    max_bytes: None,
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
            Effect::LoadTeam { ticket } => {
                let result = self
                    .facade
                    .agent()
                    .team_snapshot(ticket.request().clone())
                    .await;
                self.send(AppMessage::TeamLoaded { ticket, result });
            }
            Effect::ControlTeam { request } => {
                let session_id = request.session_id.clone();
                let result = self
                    .facade
                    .agent()
                    .control_team_task(MutationRequest::new(request))
                    .await
                    .map(|_| ());
                self.send(AppMessage::TeamMutated { session_id, result });
            }
            Effect::AcknowledgeTeam { request } => {
                let session_id = request.session_id.clone();
                let result = self
                    .facade
                    .agent()
                    .acknowledge_team_events(MutationRequest::new(request))
                    .await
                    .map(|_| ());
                self.send(AppMessage::TeamMutated { session_id, result });
            }
            Effect::OpenSession { ticket, .. } => {
                let result = self.load_session(ticket.clone()).await;
                self.send(AppMessage::SessionOpened { ticket, result });
            }
            Effect::LoadOlder { ticket } => {
                // One gesture, one bounded page: the cursor the ticket pinned
                // is what decides where the reader lands, and the shared
                // helper is what keeps the page turn-aligned and byte-capped.
                let result = vibex_ui::load_timeline_before(
                    self.facade.agent().as_ref(),
                    &ticket.session_id,
                    ticket.before_sequence,
                )
                .await;
                self.send(AppMessage::OlderTimeline { ticket, result });
            }
            Effect::LoadTimelineItem { ticket } => {
                // A page carries summaries, so this is the read that puts an
                // opened activity's output back. The ticket pins the sequence:
                // an answer that arrives after the reader navigated away is
                // dropped by the controller instead of drawn over another item.
                let result = self
                    .facade
                    .agent()
                    .fetch_timeline_item(vibex_core::FetchTimelineItemRequest {
                        session_id: ticket.session_id.clone(),
                        sequence: ticket.sequence,
                    })
                    .await;
                self.send(AppMessage::TimelineItem { ticket, result });
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
            Effect::UnarchiveSession { session_id } => {
                let request = MutationRequest::new(session_id);
                match self.facade.agent().unarchive_session(request).await {
                    Ok(_) => self.ok("unarchive_session"),
                    Err(error) => self.failure("unarchive_session", error),
                }
            }
            Effect::DeleteSession { session_id } => {
                let request = MutationRequest::new(session_id);
                match self.facade.agent().delete_session(request).await {
                    Ok(()) => self.ok("delete_session"),
                    Err(error) => self.failure("delete_session", error),
                }
            }
            Effect::DeleteProject { project_id } => {
                // The id comes from a row the runtime published, so a project
                // whose id does not parse is one this client invented rather
                // than one the runtime can be asked about. It is refused here
                // instead of travelling as a request the runtime would have to
                // reject.
                let Some(project_id) = vibex_core::ProjectId::parse(&project_id).ok() else {
                    self.failure(
                        "delete_project",
                        vibex_backend::BackendError::unsupported(
                            "invalid_project_id",
                            "the project id is not one the runtime published",
                        ),
                    );
                    return;
                };
                let request = MutationRequest::new(project_id);
                match self.facade.workspace().delete_project(request).await {
                    Ok(()) => self.ok("delete_project"),
                    Err(error) => self.failure("delete_project", error),
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
            Effect::CancelPairingOffer { offer_id } => {
                let request =
                    MutationRequest::new(vibex_core::RemoteCancelPairingOfferRequest { offer_id });
                // Taking an offer down answers with the summary it now holds, so
                // the client learns the runtime really stopped accepting the
                // code rather than assuming it did.
                let result = self.facade.device().cancel_pairing_offer(request).await;
                self.send(AppMessage::PairingOfferStatus(result.map(Box::new)));
            }
            Effect::PairingOfferStatus { offer_id } => {
                let result = self
                    .facade
                    .device()
                    .pairing_offer_status(vibex_core::RemotePairingOfferStatusRequest { offer_id })
                    .await;
                self.send(AppMessage::PairingOfferStatus(result.map(Box::new)));
            }
            Effect::PairingPublications => {
                let result = self.facade.device().pairing_publications().await;
                self.send(AppMessage::PairingPublications(result));
            }
            Effect::StartLanPairing {
                permission_level,
                ttl_ms,
            } => {
                let request = MutationRequest::new(vibex_core::RemoteStartLanPairingRequest {
                    permission_level,
                    ttl_ms: Some(ttl_ms),
                });
                // The window is opened by the runtime that owns the
                // advertisement, so this is the one pairing call that can take
                // as long as a TLS probe.
                let result = self
                    .facade
                    .device()
                    .start_lan_pairing(request)
                    .await
                    .map(|window| Some(Box::new(window)));
                self.send(AppMessage::LanPairing(result));
            }
            Effect::LanPairingStatus => {
                let result = self
                    .facade
                    .device()
                    .lan_pairing_window()
                    .await
                    .map(|window| window.map(Box::new));
                self.send(AppMessage::LanPairing(result));
            }
            Effect::DecideLanPairing {
                request_id,
                approve,
            } => {
                let request = MutationRequest::new(vibex_core::RemoteLanPairingDecision {
                    request_id,
                    approve,
                });
                let result = self
                    .facade
                    .device()
                    .decide_lan_pairing(request)
                    .await
                    .map(|window| Some(Box::new(window)));
                self.send(AppMessage::LanPairing(result));
            }
            Effect::CancelLanPairing => {
                let result = self.facade.device().cancel_lan_pairing().await;
                // Taking a window down answers with no window, which is exactly
                // the state the panel was just put into.
                self.send(AppMessage::LanPairing(result.map(|()| None)));
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
            Effect::LoadProviderProfile {
                agent_id,
                profile_id,
            } => {
                let request = vibex_backend::MutationRequest::new(
                    vibex_core::AgentModelProviderProfileDetailRequest {
                        agent_id,
                        provider_profile_id: profile_id,
                    },
                );
                let result = self
                    .facade
                    .management()
                    .agent_model_provider_profile_detail(request)
                    .await;
                self.send(AppMessage::ProviderProfile(result));
            }
            Effect::OpenUrl { url } => match crate::terminal::open_url(&url) {
                Ok(()) => {}
                Err(error) => self.failure("open_url", error),
            },
            Effect::PreviewAgentAuthLogout { agent_id } => {
                let Some((context_id, _)) = self.default_auth_context(&agent_id).await else {
                    self.failure_for(
                        "agent_auth_logout_preview",
                        Some(agent_id.as_str().to_string()),
                        BackendError::failed(
                            "agent_auth_context_unknown",
                            "this Agent holds no account to sign out of",
                        ),
                    );
                    return;
                };
                let result = self
                    .facade
                    .agent()
                    .preview_agent_auth_logout(context_id)
                    .await;
                self.send(AppMessage::AgentAuthLogoutPreview(result.map(Box::new)));
            }
            Effect::LoadAgentTools => {
                let result = self.facade.agent().agent_tool_preferences().await;
                self.send(AppMessage::AgentTools(result));
            }
            Effect::DiscoverLocalAgents => {
                let result = self
                    .discover_local_agents()
                    .await
                    .map(|found| crate::app::MutationOutcome::Discovered { found });
                self.send(AppMessage::Mutation {
                    key: "agent_discovery".to_string(),
                    record: Some("agents".to_string()),
                    result,
                });
            }
            Effect::LoadAgentAuthCatalog { agent_id, refresh } => {
                // The context is seeded first: a runtime that has never been
                // asked holds none, and the methods are advertised against one.
                let result = match self
                    .facade
                    .agent()
                    .ensure_default_agent_auth_context(vibex_backend::MutationRequest::new(
                        agent_id.clone(),
                    ))
                    .await
                {
                    // A refresh re-probes the Agent's own runtime and replaces
                    // the snapshot; a plain open answers from the snapshot the
                    // authority already holds, so opening the panel does not
                    // launch a process.
                    Ok(_) if refresh => self
                        .facade
                        .agent()
                        .refresh_agent_auth_methods(vibex_backend::MutationRequest::new(agent_id))
                        .await
                        .map(Box::new),
                    Ok(_) => self
                        .facade
                        .agent()
                        .list_agent_auth_methods(agent_id)
                        .await
                        .map(Box::new),
                    Err(error) => Err(error),
                };
                self.send(AppMessage::AgentAuthCatalog(result));
            }
            Effect::AttachAgentAuthTerminal {
                agent_id,
                terminal_id,
                title,
            } => {
                // The session is read before the frames are subscribed to: the
                // subscription starts at the sequence the snapshot reached, so a
                // client that subscribed first would either repeat bytes it
                // already has or drop the ones between the two calls.
                let snapshot = self
                    .facade
                    .terminal()
                    .terminal_snapshot(terminal_id.clone())
                    .await;
                let session = match snapshot {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        self.send(AppMessage::AgentAuthTerminal {
                            agent_id,
                            terminal_id,
                            title,
                            result: Err(error),
                        });
                        return;
                    }
                };
                let next = session.next_sequence;
                let mut subscription = match self
                    .facade
                    .terminal()
                    .subscribe_terminal(terminal_id.clone(), next)
                {
                    Ok(subscription) => subscription,
                    Err(error) => {
                        self.send(AppMessage::AgentAuthTerminal {
                            agent_id,
                            terminal_id,
                            title,
                            result: Err(error),
                        });
                        return;
                    }
                };
                self.send(AppMessage::AgentAuthTerminal {
                    agent_id: agent_id.clone(),
                    terminal_id: terminal_id.clone(),
                    title,
                    result: Ok(()),
                });
                // The first snapshot is what a terminal that has already
                // printed something looks like: the login prompt is usually
                // there before the reader attaches.
                let initial = session
                    .chunks
                    .iter()
                    .map(|chunk| chunk.data.as_str())
                    .collect::<String>();
                if !initial.is_empty() {
                    self.send(AppMessage::AgentAuthTerminalFrame {
                        agent_id: agent_id.clone(),
                        terminal_id: terminal_id.clone(),
                        bytes: initial.into_bytes(),
                    });
                }
                // Frames arrive on this worker's own task rather than through
                // the agent event subscription: a terminal is a different
                // stream with a different lifetime, and one that ends when the
                // sign-in does rather than when the connection does.
                // A subscription that ends or fails ends the view: the process
                // is gone, and a frozen grid over a finished sign-in reads as a
                // hung client.
                while let Ok(Some(batch)) = subscription.next().await {
                    let bytes = batch
                        .frames
                        .iter()
                        .flat_map(|frame| frame.bytes.iter().copied())
                        .collect::<Vec<_>>();
                    if self.sender.is_closed() {
                        break;
                    }
                    if !bytes.is_empty()
                        && !self.try_send(AppMessage::AgentAuthTerminalFrame {
                            agent_id: agent_id.clone(),
                            terminal_id: terminal_id.clone(),
                            bytes,
                        })
                    {
                        break;
                    }
                    // A reset is the authority telling the client to forget the
                    // grid and start again; the emulator keeps its own state, so
                    // what there is to do is go on reading.
                    if batch.reset_required {
                        continue;
                    }
                }
                self.send(AppMessage::AgentAuthTerminalEnded { terminal_id });
            }
            Effect::WriteAgentAuthTerminal { terminal_id, data } => {
                let request =
                    MutationRequest::new(vibex_core::TerminalWriteRequest { terminal_id, data });
                if let Err(error) = self.facade.terminal().write_terminal(request).await {
                    // A key that cannot be delivered is not worth a toast: the
                    // reader is typing into a terminal, and the terminal is
                    // about to be gone. The answer that matters is the sign-in's.
                    let _ = error;
                }
            }
            Effect::ResizeAgentAuthTerminal {
                terminal_id,
                rows,
                columns,
            } => {
                let request = MutationRequest::new(vibex_core::TerminalResizeRequest {
                    terminal_id,
                    rows,
                    cols: columns,
                });
                if let Err(error) = self.facade.terminal().resize_terminal(request).await {
                    let _ = error;
                }
            }
            Effect::CancelAgentAuth {
                agent_id,
                operation_id,
            } => {
                let record = agent_id.as_str().to_string();
                let context = self.default_auth_context(&agent_id).await;
                let Some((context_id, revision)) = context else {
                    self.failure_for(
                        "agent_auth_cancel",
                        Some(record),
                        BackendError::failed(
                            "agent_auth_context_unknown",
                            "this Agent holds no account to cancel a sign-in for",
                        ),
                    );
                    return;
                };
                let request =
                    MutationRequest::new(vibex_core::AgentAuthContextCancelAuthenticationRequest {
                        operation_id,
                        auth_context_id: context_id,
                        expected_context_revision: revision,
                    });
                match self
                    .facade
                    .agent()
                    .cancel_agent_context_authentication(request)
                    .await
                {
                    Ok(_) => self.ok_with(
                        "agent_auth_cancel",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("agent_auth_cancel", Some(record), error),
                }
            }
            Effect::VerifyAgentAuth { agent_id } => {
                let record = agent_id.as_str().to_string();
                let context = self.default_auth_context(&agent_id).await;
                let Some((context_id, revision)) = context else {
                    self.failure_for(
                        "agent_auth_verify",
                        Some(record),
                        BackendError::failed(
                            "agent_auth_context_unknown",
                            "this Agent holds no account to verify",
                        ),
                    );
                    return;
                };
                let request = vibex_backend::MutationRequest::new(
                    vibex_core::AgentAuthContextVerifyRequest {
                        auth_context_id: context_id,
                        expected_context_revision: revision,
                        // A plain verification completes no operation: the
                        // interactive flow that does is the terminal's, and it
                        // names its own operation when it lands.
                        operation_id: None,
                    },
                );
                match self.facade.agent().verify_agent_auth_context(request).await {
                    Ok(result) => self.ok_with(
                        "agent_auth_verify",
                        Some(record),
                        crate::app::MutationOutcome::Auth {
                            methods: 0,
                            account: Some(result.context.account_hint.clone().unwrap_or_default()),
                            authenticated: result.context.status
                                == vibex_core::AgentAuthContextStatus::Authenticated,
                        },
                    ),
                    Err(error) => self.failure_for("agent_auth_verify", Some(record), error),
                }
            }
            Effect::RefreshAgentAuthModels { agent_id } => {
                let record = agent_id.as_str().to_string();
                let context = self.default_auth_context(&agent_id).await;
                let Some((context_id, revision)) = context else {
                    self.failure_for(
                        "agent_auth_models",
                        Some(record),
                        BackendError::failed(
                            "agent_auth_context_unknown",
                            "this Agent holds no account to read a catalogue for",
                        ),
                    );
                    return;
                };
                let request = vibex_backend::MutationRequest::new(
                    vibex_core::AgentAuthContextRefreshModelsRequest {
                        auth_context_id: context_id,
                        expected_context_revision: revision,
                    },
                );
                match self.facade.agent().refresh_agent_auth_models(request).await {
                    Ok(result) => self.ok_with(
                        "agent_auth_models",
                        Some(record),
                        crate::app::MutationOutcome::Models {
                            count: result
                                .model_catalog
                                .as_ref()
                                .map(|catalog| catalog.models.len())
                                .unwrap_or(0),
                        },
                    ),
                    Err(error) => self.failure_for("agent_auth_models", Some(record), error),
                }
            }
            Effect::PreviewNativeExport {
                profile_id,
                source,
                mode,
            } => {
                // A preview is recorded so it can be applied: the plan the
                // reader is looking at is the plan that would be written, and an
                // export that was re-derived at apply time could differ from it.
                let request = vibex_core::ProviderNativeExportPreviewRequest {
                    provider_profile_id: profile_id,
                    source,
                    mode,
                    persist: true,
                };
                let result = self
                    .facade
                    .management()
                    .preview_native_export(request)
                    .await;
                self.send(AppMessage::NativeExport(result.map(Box::new)));
            }
            Effect::NativeExportHistory { profile_id } => {
                let request = vibex_core::ProviderNativeExportListRequest {
                    provider_profile_id: Some(profile_id),
                    limit: Some(20),
                };
                let result = self.facade.management().native_exports(request).await;
                self.send(AppMessage::NativeExportHistory(result));
            }
            Effect::LoadMoreSkillMarket {
                cursor,
                query,
                registry,
            } => {
                let request = vibex_core::SkillMarketSearchRequest {
                    query: (!query.trim().is_empty()).then_some(query),
                    limit: Some(crate::config_center::SKILL_MARKET_FETCH_LIMIT as u32),
                    cursor: Some(cursor),
                    sort: Some(registry.sort.wire()),
                };
                let result = self.facade.management().search_skill_market(request).await;
                self.send(AppMessage::SkillMarketPage(result.map(Box::new)));
            }
            Effect::PreviewSkillMarket {
                entry_id,
                slug,
                owner_handle,
                version,
            } => {
                let request = vibex_core::SkillMarketDocumentRequest {
                    entry_id,
                    slug,
                    owner_handle,
                    version,
                };
                let result = self
                    .facade
                    .management()
                    .skill_market_document(request)
                    .await;
                self.send(AppMessage::SkillMarketDocument(result.map(Box::new)));
            }
            Effect::PreviewNativeImport { sources } => {
                let request = vibex_core::ProviderNativeImportPreviewRequest { sources };
                let result = self
                    .facade
                    .management()
                    .preview_native_import(request)
                    .await;
                self.send(AppMessage::NativeImport(result));
            }
            Effect::LoadProviderOrder { agent_id } => {
                let request = vibex_core::AgentModelProviderDisplayOrderListRequest { agent_id };
                let result = self
                    .facade
                    .management()
                    .agent_model_provider_display_order(request)
                    .await;
                self.send(AppMessage::ProviderOrder(
                    result.map(|response| response.entries),
                ));
            }
            Effect::ListProfiles => {
                let result = self.facade.management().list_profiles().await;
                self.send(AppMessage::Profiles(result));
            }
            Effect::LoadGateway => {
                let result = self.facade.management().model_gateway_snapshot().await;
                self.send(AppMessage::Gateway(result));
            }
            Effect::LoadGatewayRequests => {
                let result = self.facade.management().model_gateway_requests().await;
                self.send(AppMessage::GatewayRequests(result));
            }
            Effect::SetGatewayEnabled { enabled } => {
                // The answer is the snapshot that resulted, so the row is drawn
                // from what the authority did rather than from the press: only
                // it knows which port comes back, and turning the gateway off
                // moves every Agent bound to it back to its own Provider.
                match self
                    .facade
                    .management()
                    .set_model_gateway_enabled(enabled)
                    .await
                {
                    Ok(snapshot) => {
                        self.send(AppMessage::Gateway(Ok(snapshot)));
                        self.ok_with(
                            "gateway_toggle",
                            Some("gateway".to_string()),
                            crate::app::MutationOutcome::Plain,
                        )
                    }
                    Err(error) => {
                        self.failure_for("gateway_toggle", Some("gateway".to_string()), error)
                    }
                }
            }
            Effect::SetGatewayKey {
                profile,
                credential,
                enabled,
                preferred,
            } => {
                let snapshot = match self.facade.management().model_gateway_snapshot().await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        return self.failure_for("gateway_key", Some("gateway".to_string()), error);
                    }
                };
                let settings = &snapshot.settings;
                let request = vibex_core::GatewaySettingsUpdateRequest {
                    display_name: settings.display_name.clone(),
                    token_limit: settings.policy.token_limit,
                    account_limits: settings.policy.account_limits.clone(),
                    translate: settings.policy.translate,
                    account_updates: vec![vibex_core::GatewayAccountUpdate {
                        model_provider_profile_id: profile,
                        credential_id: credential,
                        enabled,
                        preferred,
                    }],
                    error_rate_warn_percent: settings.policy.error_rate_warn_percent,
                    latency_warn_ms: settings.policy.latency_warn_ms,
                    default_connect_timeout_ms: settings.policy.default_connect_timeout_ms,
                    default_request_timeout_ms: settings.policy.default_request_timeout_ms,
                    default_stream_idle_timeout_ms: settings.policy.default_stream_idle_timeout_ms,
                    cooldown_overrides_ms: settings.policy.cooldown_overrides_ms.clone(),
                    expected_revision: settings.revision,
                };
                match self
                    .facade
                    .management()
                    .update_model_gateway_settings(vibex_backend::MutationRequest::new(request))
                    .await
                {
                    Ok(snapshot) => {
                        self.send(AppMessage::Gateway(Ok(snapshot)));
                        self.ok_with(
                            "gateway_key",
                            Some("gateway".to_string()),
                            crate::app::MutationOutcome::Plain,
                        )
                    }
                    Err(error) => {
                        self.failure_for("gateway_key", Some("gateway".to_string()), error)
                    }
                }
            }
            Effect::SetGatewayPolicy { token_limit } => {
                let snapshot = match self.facade.management().model_gateway_snapshot().await {
                    Ok(snapshot) => snapshot,
                    Err(error) => {
                        return self.failure_for(
                            "gateway_policy",
                            Some("gateway".to_string()),
                            error,
                        );
                    }
                };
                let settings = &snapshot.settings;
                let request = vibex_core::GatewaySettingsUpdateRequest {
                    display_name: settings.display_name.clone(),
                    token_limit: token_limit.unwrap_or(settings.policy.token_limit),
                    account_limits: settings.policy.account_limits.clone(),
                    translate: settings.policy.translate,
                    account_updates: Vec::new(),
                    error_rate_warn_percent: settings.policy.error_rate_warn_percent,
                    latency_warn_ms: settings.policy.latency_warn_ms,
                    default_connect_timeout_ms: settings.policy.default_connect_timeout_ms,
                    default_request_timeout_ms: settings.policy.default_request_timeout_ms,
                    default_stream_idle_timeout_ms: settings.policy.default_stream_idle_timeout_ms,
                    cooldown_overrides_ms: settings.policy.cooldown_overrides_ms.clone(),
                    expected_revision: settings.revision,
                };
                match self
                    .facade
                    .management()
                    .update_model_gateway_settings(vibex_backend::MutationRequest::new(request))
                    .await
                {
                    Ok(snapshot) => {
                        self.send(AppMessage::Gateway(Ok(snapshot)));
                        self.ok_with(
                            "gateway_policy",
                            Some("gateway".to_string()),
                            crate::app::MutationOutcome::Plain,
                        )
                    }
                    Err(error) => {
                        self.failure_for("gateway_policy", Some("gateway".to_string()), error)
                    }
                }
            }
            Effect::RenameProfile { profile_id, name } => {
                let record = profile_id.as_str().to_string();
                let Some(request) = self.profile_update(profile_id, Some(name)).await else {
                    return;
                };
                match self
                    .facade
                    .management()
                    .update_agent_model_provider_profile(request)
                    .await
                {
                    Ok(_) => self.ok_with(
                        "rename_profile",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("rename_profile", Some(record), error),
                }
            }
            Effect::SelectProfile { profile_id, scope } => {
                let record = profile_id.as_str().to_string();
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.failure_for(
                        "select_profile",
                        Some(record),
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
                        scope: scope.clone(),
                    });
                // Whether the default landed on a workspace or on everything is
                // half the answer, and the reader cannot see the other half.
                let workspace = scope.as_ref().is_some_and(|scope| {
                    scope.kind != vibex_core::ProviderDefaultScopeKind::Global
                });
                match self.facade.management().select_profile(request).await {
                    Ok(_) => self.ok_with(
                        "select_profile",
                        Some(record),
                        crate::app::MutationOutcome::DefaultScope { workspace },
                    ),
                    Err(error) => self.failure_for("select_profile", Some(record), error),
                }
            }
            Effect::ClearProviderSecret { profile_id } => {
                let record = profile_id.as_str().to_string();
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.failure_for(
                        "provider_secret_clear",
                        Some(record),
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
                        value: None,
                        clear: true,
                    },
                );
                match self
                    .facade
                    .management()
                    .mutate_agent_model_provider_profile_secret(request)
                    .await
                {
                    Ok(_) => self.ok_with(
                        "provider_secret_clear",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("provider_secret_clear", Some(record), error),
                }
            }
            Effect::WriteProviderSecret { profile_id, secret } => {
                let record = profile_id.as_str().to_string();
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    self.failure_for(
                        "provider_secret",
                        Some(record),
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
                    Ok(_) => self.ok_with(
                        "provider_secret",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("provider_secret", Some(record), error),
                }
            }
            Effect::TestProviderProfile { profile_id } => {
                let record = profile_id.as_str().to_string();
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    let error = BackendError::failed(
                        "provider_profile_unknown",
                        "the selected profile has no owning Agent",
                    );
                    self.send(AppMessage::ProviderTest(Err(error.clone())));
                    self.failure_for("provider_test", Some(record), error);
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
                match &result {
                    Ok(tested) => {
                        use vibex_core::AgentModelProviderTestStatus as Status;
                        self.ok_with(
                            "provider_test",
                            Some(record),
                            crate::app::MutationOutcome::verdict(
                                tested.status == Status::Warn,
                                tested.status == Status::Fail,
                                &tested.code,
                                &tested.message,
                            ),
                        );
                    }
                    Err(error) => {
                        self.failure_for("provider_test", Some(record), error.clone());
                    }
                }
                self.send(AppMessage::ProviderTest(result));
            }
            Effect::FetchProviderModels { profile_id } => {
                let record = profile_id.as_str().to_string();
                let Some(agent_id) = self.agent_for_profile(&profile_id).await else {
                    let error = BackendError::failed(
                        "provider_profile_unknown",
                        "the selected profile has no owning Agent",
                    );
                    self.send(AppMessage::ProviderModels(Err(error.clone())));
                    self.failure_for("provider_models", Some(record), error);
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
                match &result {
                    Ok(models) => self.ok_with(
                        "provider_models",
                        Some(record),
                        crate::app::MutationOutcome::Models {
                            count: models.len(),
                        },
                    ),
                    Err(error) => {
                        self.failure_for("provider_models", Some(record), error.clone());
                    }
                }
                self.send(AppMessage::ProviderModels(result));
            }
            Effect::ListAgents => {
                let request = vibex_core::AgentListRequest {
                    include_disabled: true,
                };
                let result = self.facade.management().list_agents(request).await;
                self.send(AppMessage::Agents(result.map(|response| response.agents)));
            }
            Effect::InstallAgent { agent_id } => {
                let record = agent_id.as_str().to_string();
                let request = MutationRequest::new(agent_id);
                match self
                    .facade
                    .management()
                    .install_managed_agent(request)
                    .await
                {
                    Ok(state) => {
                        self.ok_with("install_agent", Some(record), install_outcome(&state))
                    }
                    Err(error) => self.failure_for("install_agent", Some(record), error),
                }
            }
            Effect::UninstallAgent { agent_id } => {
                let record = agent_id.as_str().to_string();
                let request = MutationRequest::new(agent_id);
                match self
                    .facade
                    .management()
                    .uninstall_managed_agent(request)
                    .await
                {
                    Ok(_) => self.ok_with(
                        "uninstall_agent",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("uninstall_agent", Some(record), error),
                }
            }
            Effect::ListAgentAuth { agent_id, open } => {
                let result = self.facade.agent().list_agent_auth_contexts().await;
                // A read that refreshes the Agents table has no record to file
                // under and no verb to report: it is the column's source, not an
                // answer to a key. The row it fills is the whole table, so the
                // message is all it says.
                let Some(agent_id) = agent_id else {
                    self.send(AppMessage::AgentAuth {
                        result,
                        open: false,
                    });
                    return;
                };
                let record = agent_id.as_str().to_string();
                // The answer is filed under the Agent the reader asked about,
                // which is not the whole list the authority returns: the row
                // says how many ways in *this* Agent has.
                let outcome = result.as_ref().ok().map(|contexts| {
                    let mine = contexts
                        .iter()
                        .filter(|context| context.agent_id == agent_id)
                        .collect::<Vec<_>>();
                    let signed_in = mine.iter().find(|context| {
                        context.status == vibex_core::AgentAuthContextStatus::Authenticated
                    });
                    crate::app::MutationOutcome::Auth {
                        methods: mine.len(),
                        account: signed_in.and_then(|context| context.account_hint.clone()),
                        authenticated: signed_in.is_some(),
                    }
                });
                match (result, outcome) {
                    (Ok(contexts), Some(outcome)) => {
                        self.send(AppMessage::AgentAuth {
                            result: Ok(contexts),
                            open,
                        });
                        self.ok_with("agent_auth", Some(record), outcome);
                    }
                    (Err(error), _) => {
                        self.failure_for("agent_auth", Some(record), error);
                    }
                    (Ok(_), None) => {}
                }
            }
            Effect::LogoutAgent { agent_id } => {
                let record = agent_id.as_str().to_string();
                let request = MutationRequest::new(vibex_core::AgentLogoutRequest {
                    agent_id,
                    provider_profile_id: None,
                });
                match self.facade.agent().logout_agent(request).await {
                    Ok(()) => {
                        self.ok_with(
                            "logout_agent",
                            Some(record),
                            crate::app::MutationOutcome::Plain,
                        );
                        // The account the row shows is the authority's to
                        // state, so it is read again rather than assumed: the
                        // row must not keep claiming a sign-in the reader just
                        // ended. The answer is not put on screen — the verb
                        // that asked for it is over.
                        let contexts = self.facade.agent().list_agent_auth_contexts().await;
                        if let Ok(contexts) = contexts {
                            self.send(AppMessage::AgentAuth {
                                result: Ok(contexts),
                                open: false,
                            });
                        }
                    }
                    Err(error) => self.failure_for("logout_agent", Some(record), error),
                }
            }
            Effect::ConfigMutation { mutation, record } => {
                self.config_mutation(mutation, record).await
            }
            Effect::DiscoverSources { module } => {
                use crate::config_center::ConfigModule;
                match module {
                    ConfigModule::Mcp => {
                        let request = vibex_core::McpServerDiscoverRequest {
                            source_agent_id: None,
                        };
                        let result = self.facade.management().discover_mcp_sources(request).await;
                        self.send(AppMessage::ConfigSources {
                            module,
                            mcp: result.map(|response| response.discoveries),
                            skill: None,
                        });
                    }
                    _ => {
                        let request = vibex_core::SkillDiscoverRequest {
                            source_agent_id: None,
                            workspace_id: None,
                        };
                        let result = self
                            .facade
                            .management()
                            .discover_skill_sources(request)
                            .await;
                        self.send(AppMessage::ConfigSources {
                            module,
                            mcp: Ok(Vec::new()),
                            skill: Some(result.map(|response| response.discoveries)),
                        });
                    }
                }
            }
            Effect::SearchRegistry {
                module,
                query,
                registry,
            } => {
                self.search_registry(module, query, registry).await;
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
                let record = server_id.as_str().to_string();
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
                    Ok(_) => self.ok_with(
                        "toggle_entry",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("toggle_entry", Some(record), error),
                }
            }
            Effect::ToggleSkill { skill_id, enabled } => {
                let record = skill_id.as_str().to_string();
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
                    Ok(_) => self.ok_with(
                        "toggle_entry",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("toggle_entry", Some(record), error),
                }
            }
            Effect::TogglePrompt { prompt_id, enabled } => {
                let record = prompt_id.as_str().to_string();
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
                    Ok(_) => self.ok_with(
                        "toggle_entry",
                        Some(record),
                        crate::app::MutationOutcome::Plain,
                    ),
                    Err(error) => self.failure_for("toggle_entry", Some(record), error),
                }
            }
            Effect::DiscoverCompletions {
                ticket,
                trigger,
                query,
            } => {
                // The authority owns the command catalogue: `/` merges the
                // Agent's own commands with Vibex Prompts, `@` resolves
                // workspace files and the collaborators work can be handed to,
                // and `$` resolves Skills. None of that is local knowledge, so
                // the composer asks rather than guessing.
                //
                // Everything the request needs is read off the ticket the
                // composer minted, so no read here can answer for a page the
                // reader has already left: the target names the session (or
                // reserves the absence of one), the ticket's runtime names the
                // Agent the draft would go through, and its workspace is the one
                // the page resolved — the `@` file list is a workspace read, and
                // a request that names none comes back without files rather than
                // with another workspace's.
                let request = vibex_core::AgentCommandDiscoverRequest {
                    agent_id: ticket
                        .runtime
                        .as_ref()
                        .map(|runtime| runtime.agent_id.clone()),
                    provider_profile_id: None,
                    session_id: match ticket.target.as_ref() {
                        Some(crate::app::ComposerTarget::Session(id)) => Some(id.clone()),
                        _ => None,
                    },
                    workspace_id: ticket.workspace_id.clone(),
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
        ticket: vibex_ui::AgentSessionLoadTicket,
    ) -> BackendResult<vibex_ui::AgentSessionSnapshot> {
        let session_id = ticket.session_id.clone();
        let session = self.facade.agent().open_session(session_id.clone()).await?;
        // Hydration is a bounded window rather than the whole archive: the
        // newest turn-aligned window on a cold open, the tail after a restored
        // cached window otherwise. Older pages are fetched on demand as the
        // reader scrolls up, which is what keeps a 100 000-item session usable.
        // The window bounds live in the shared helpers so the TUI cannot page
        // the archive back in.
        let (timeline, timeline_has_older) = match ticket.load {
            vibex_ui::TimelineLoadKind::Cold | vibex_ui::TimelineLoadKind::Before(_) => {
                let page =
                    vibex_ui::load_timeline_window(self.facade.agent().as_ref(), &session_id)
                        .await?;
                (page.items, page.has_older)
            }
            // Resuming a restored prefix only needs what arrived above it.
            vibex_ui::TimelineLoadKind::Tail(after_sequence) => {
                vibex_ui::load_timeline_tail(
                    self.facade.agent().as_ref(),
                    &session_id,
                    after_sequence,
                )
                .await?
            }
        };
        if timeline.iter().any(|item| item.session_id != session_id) {
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
            timeline,
            runtime_selection,
            timeline_has_older,
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
    /// Sign out of one Agent's account, having been shown what it ends.
    async fn logout_agent_auth_context(
        &self,
        auth_context_id: String,
        expected_context_revision: i64,
        confirmed_affected_session_count: usize,
    ) -> BackendResult<crate::app::MutationOutcome> {
        let auth_context_id = match vibex_core::AgentAuthContextId::parse(auth_context_id) {
            Ok(id) => id,
            Err(_) => {
                return Err(BackendError::failed(
                    "agent_auth_context_unknown",
                    "this Agent holds no account to sign out of",
                ));
            }
        };
        let request =
            vibex_backend::MutationRequest::new(vibex_core::AgentAuthContextLogoutRequest {
                auth_context_id,
                expected_context_revision,
                confirmed_affected_session_count,
            });
        self.facade
            .agent()
            .logout_agent_auth_context(request)
            .await
            .map(|_| crate::app::MutationOutcome::Plain)
    }

    /// Sign in with one of an Agent's methods.
    ///
    /// What the method collects is stored first: the sign-in reads the
    /// profile's own environment, so a key the reader just typed has to be in it
    /// before the Agent's bridge is launched.
    async fn authenticate_agent_method(
        &self,
        agent_id: vibex_core::AgentId,
        method_id: String,
        values: Vec<vibex_core::AgentAuthEnvironmentValue>,
    ) -> BackendResult<crate::app::MutationOutcome> {
        let Some((context_id, revision)) = self.default_auth_context(&agent_id).await else {
            return Err(BackendError::failed(
                "agent_auth_context_unknown",
                "this Agent holds no account to sign in to",
            ));
        };
        // What the method collects is stored first: the sign-in reads
        // the profile's environment, and a key the reader just typed
        // has to be in it before the bridge is launched.
        if !values.is_empty() {
            let profile_id = self.default_profile_for(&agent_id).await;
            let Some(profile_id) = profile_id else {
                return Err(BackendError::failed(
                    "agent_auth_profile_unknown",
                    "this Agent has no provider profile to store the values in",
                ));
            };
            let request = vibex_backend::MutationRequest::new(
                vibex_core::AgentAuthEnvironmentUpdateRequest {
                    agent_id: agent_id.clone(),
                    provider_profile_id: profile_id,
                    method_id: method_id.clone(),
                    values,
                },
            );
            self.facade
                .agent()
                .update_agent_auth_environment(request)
                .await?;
        }
        let request =
            vibex_backend::MutationRequest::new(vibex_core::AgentAuthContextAuthenticateRequest {
                operation_id: vibex_core::AgentAuthenticationOperationId::new(),
                auth_context_id: context_id,
                expected_context_revision: revision,
                method_id,
            });
        match self
            .facade
            .agent()
            .authenticate_agent_context(request)
            .await
        {
            Ok(result) => {
                if result.context.status == vibex_core::AgentAuthContextStatus::Authenticated {
                    return Ok(crate::app::MutationOutcome::Auth {
                        methods: 0,
                        account: result.context.account_hint.clone(),
                        authenticated: true,
                    });
                }
                // The runtime keeps the operation open and names where the reader
                // has to finish it. The command a terminal method runs is
                // redacted at the authority boundary — the descriptor carries
                // the terminal's own title and its id instead — so what can be
                // said is where to look, not what was run there.
                let detail = result
                    .terminal
                    .as_ref()
                    .map(|terminal| terminal.title.clone())
                    .filter(|title| !title.trim().is_empty())
                    .or_else(|| result.context.account_hint.clone());
                Ok(crate::app::MutationOutcome::AuthWaiting {
                    location: result.execution_location,
                    detail,
                    operation: Some(Box::new(result.operation)),
                    terminal: result.terminal.map(Box::new),
                })
            }
            Err(error) => Err(error),
        }
    }

    /// Probe this machine for the Agent CLIs that have not been added yet.
    ///
    /// One pass, in the order the desktop runs it: a candidate is added without
    /// being enabled, the machine is asked whether the CLI is there, and only
    /// one that answered is enabled. An Agent that was added and left disabled
    /// is the honest answer for a CLI that is not installed — the reader can
    /// still install it, and nothing claims to be ready that is not.
    async fn discover_local_agents(&self) -> BackendResult<usize> {
        let request = vibex_core::AgentListRequest {
            include_disabled: true,
        };
        let candidates = match self.facade.management().list_agents(request).await {
            Ok(response) => response
                .agents
                .into_iter()
                .filter(|agent| !agent.added)
                .map(|agent| agent.id)
                .collect::<Vec<_>>(),
            Err(error) => return Err(error),
        };
        let mut found = 0_usize;
        for agent_id in candidates {
            let add = vibex_backend::MutationRequest::new(vibex_core::AgentUpdateConfigRequest {
                agent_id: agent_id.clone(),
                added: Some(true),
                // Added switched off: an Agent the machine does not have is one
                // the reader has not agreed to run.
                enabled: Some(false),
                label_override: None,
                description_override: None,
                order_index: None,
                command: None,
                env: None,
                params: None,
            });
            self.facade.management().update_agent_config(add).await?;
            let refreshed = self
                .facade
                .management()
                .refresh_agent_snapshot(vibex_core::AgentRefreshSnapshotRequest {
                    agent_id: agent_id.clone(),
                    cwd_scope: None,
                })
                .await?;
            if !refreshed.agent.installed {
                continue;
            }
            let enable =
                vibex_backend::MutationRequest::new(vibex_core::AgentUpdateConfigRequest {
                    agent_id: agent_id.clone(),
                    added: None,
                    enabled: Some(true),
                    label_override: None,
                    description_override: None,
                    order_index: None,
                    command: None,
                    env: None,
                    params: None,
                });
            self.facade.management().update_agent_config(enable).await?;
            // The runtime options are what the Agent's own CLI advertises; a
            // probe that fails is not a discovery that failed, so it is the one
            // step whose error is not returned.
            let probe =
                vibex_backend::MutationRequest::new(vibex_core::AgentRuntimeOptionProbeRequest {
                    agent_id: agent_id.clone(),
                });
            let _ = self.facade.agent().probe_agent_runtime_options(probe).await;
            found = found.saturating_add(1);
        }
        Ok(found)
    }

    /// Exchange two Agents' places in the roster.
    async fn swap_agent_order(
        &self,
        first: vibex_core::AgentId,
        first_index: i64,
        second: vibex_core::AgentId,
        second_index: i64,
    ) -> BackendResult<crate::app::MutationOutcome> {
        let place = |agent_id: vibex_core::AgentId, order_index: i64| {
            vibex_backend::MutationRequest::new(vibex_core::AgentUpdateConfigRequest {
                agent_id,
                added: None,
                enabled: None,
                label_override: None,
                description_override: None,
                order_index: Some(order_index),
                command: None,
                env: None,
                params: None,
            })
        };
        self.facade
            .management()
            .update_agent_config(place(first, second_index))
            .await?;
        self.facade
            .management()
            .update_agent_config(place(second, first_index))
            .await?;
        Ok(crate::app::MutationOutcome::Plain)
    }

    /// The Agent's default provider profile, which is where a sign-in method's
    /// collected values are stored.
    async fn default_profile_for(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> Option<vibex_core::ProviderProfileId> {
        self.facade
            .management()
            .list_profiles()
            .await
            .ok()?
            .into_iter()
            .find(|profile| profile.agent_id == *agent_id)
            .map(|profile| profile.id)
    }

    /// The Agent's default account context and its revision, when it holds one.
    async fn default_auth_context(
        &self,
        agent_id: &vibex_core::AgentId,
    ) -> Option<(vibex_core::AgentAuthContextId, i64)> {
        let contexts = self.facade.agent().list_agent_auth_contexts().await.ok()?;
        contexts
            .into_iter()
            .find(|context| context.agent_id == *agent_id)
            .map(|context| (context.id, context.revision))
    }

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

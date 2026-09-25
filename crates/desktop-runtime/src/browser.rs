//! Runtime composition for the embedded browser.
//!
//! This is where the browser service, the loopback MCP endpoint and the
//! runtime's own permission channel meet. The browser crate deliberately knows
//! nothing about agent sessions, bearer-token policy or human approval; the
//! runtime owns all three, so the seam between them lives here.
//!
//! Three responsibilities:
//!
//! 1. Own the [`BrowserService`] for the process lifetime and hand it to the
//!    `BrowserBackend` facade and to `shutdown_inner`.
//! 2. Serve the loopback HTTP MCP endpoint that the ACP layer advertises to
//!    Agents that can use HTTP MCP.
//! 3. Answer approval prompts through the existing timeline permission card,
//!    with a TTL the browser layer enforces itself.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use async_trait::async_trait;
use serde_json::Value;
use std::collections::HashMap;
use tokio::sync::Mutex;
use vibex_browser::{
    BrowserMcpEndpoint, BrowserMcpHandler, BrowserMcpHost, BrowserMcpSession,
    BrowserPermissionDecision, BrowserService, BrowserServiceConfig, BrowserServiceEvent,
    BrowserSessionKey, issue_session_token, verify_session_token,
};
use vibex_core::{
    BROWSER_ACTION_HIGHLIGHT_MS, BrowserActionRecord, BrowserToolDelivery, BrowserToolTier,
    PermissionActionDetail, PermissionRequest, PermissionRequestStatus, PermissionResponseKind,
    PermissionRiskCategory, RequestId, VibexError, VibexResult, VibexSessionId, WorkspaceId,
    unix_timestamp_ms,
};
use vibex_db::PermissionRepository;

use crate::AgentHandle;

/// How long a browser approval card stays open before it is denied.
///
/// The runtime has no global pending-permission expiry — `expires_at_ms` is
/// always `None` elsewhere and nothing ever writes `Expired` — so an
/// unanswered browser card would otherwise sit there for the entire two-hour
/// ACP prompt budget while the Agent waits on a tool call.
pub const BROWSER_APPROVAL_TTL_MS: i64 = 120_000;
/// How often the approval wait re-reads the request.
const BROWSER_APPROVAL_POLL_MS: u64 = 250;

/// Per-run browser state held by the runtime.
pub struct BrowserRuntime {
    service: BrowserService,
    capability_token: String,
    agent: AgentHandle,
    endpoint: Mutex<Option<BrowserMcpEndpoint>>,
    endpoint_url: Mutex<Option<String>>,
    /// Ownership of each browser session, so a ledger entry can be attributed
    /// when it is persisted.
    session_owners: Mutex<HashMap<vibex_core::BrowserSessionId, SessionOwner>>,
    audit_task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

/// Who a browser session belongs to, for the audit row.
#[derive(Debug, Clone, Default)]
struct SessionOwner {
    agent_session_id: Option<VibexSessionId>,
    workspace_id: Option<WorkspaceId>,
}

impl std::fmt::Debug for BrowserRuntime {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("BrowserRuntime")
            .field(
                "endpoint",
                &self
                    .endpoint_url
                    .try_lock()
                    .ok()
                    .and_then(|url| url.clone()),
            )
            .finish()
    }
}

impl BrowserRuntime {
    /// Builds the service. The browser is not launched until it is first used.
    pub fn new(home_dir: impl Into<PathBuf>, agent: AgentHandle) -> Arc<Self> {
        Arc::new(Self {
            service: BrowserService::new(BrowserServiceConfig::new(home_dir)),
            capability_token: format!("cap_{}", RequestId::new().as_str()),
            agent,
            endpoint: Mutex::new(None),
            endpoint_url: Mutex::new(None),
            session_owners: Mutex::new(HashMap::new()),
            audit_task: Mutex::new(None),
        })
    }

    pub fn service(&self) -> &BrowserService {
        &self.service
    }

    pub fn subscribe(&self) -> tokio::sync::broadcast::Receiver<BrowserServiceEvent> {
        self.service.subscribe()
    }

    /// The global secret used to mint per-session bearer tokens. Never handed to
    /// an Agent.
    pub fn capability_token(&self) -> &str {
        &self.capability_token
    }

    /// Mints the bearer token for one Agent session.
    pub fn session_token(&self, agent_session_id: &VibexSessionId) -> String {
        issue_session_token(&self.capability_token, agent_session_id.as_str())
    }

    /// The live MCP endpoint URL, once the endpoint has started.
    pub async fn endpoint_url(&self) -> Option<String> {
        self.endpoint_url.lock().await.clone()
    }

    /// Starts the loopback MCP endpoint.
    ///
    /// Called once, after the runtime's own services are up. A failure is
    /// reported to the caller rather than swallowed: without the endpoint there
    /// is no HTTP delivery path, and the stdio sidecar would have nothing to
    /// forward to.
    pub async fn start_endpoint(self: &Arc<Self>) -> VibexResult<String> {
        let mut guard = self.endpoint.lock().await;
        if let Some(url) = self.endpoint_url.lock().await.clone() {
            return Ok(url);
        }
        let host = Arc::new(RuntimeBrowserHost {
            runtime: Arc::clone(self),
        });
        let handler =
            Arc::new(BrowserMcpHandler::new(self.service.clone()).with_host(host.clone()));
        let endpoint = BrowserMcpEndpoint::start(handler, host)
            .await
            .map_err(VibexError::from)?;
        let url = endpoint.url();
        tracing::info!(
            target: "vibex_browser",
            endpoint = %url,
            "embedded browser MCP endpoint listening on loopback"
        );
        *self.endpoint_url.lock().await = Some(url.clone());
        *guard = Some(endpoint);
        Ok(url)
    }

    /// Persists every browser action into the audit ledger.
    ///
    /// The browser layer keeps the redacted record in memory for the panel; the
    /// runtime writes it down, because the ledger has to survive the process.
    /// Page content, form values, cookies and screenshots never reach this
    /// table.
    pub async fn start_audit_consumer(self: &Arc<Self>) {
        let mut guard = self.audit_task.lock().await;
        if guard.is_some() {
            return;
        }
        let mut events = self.service.subscribe();
        let runtime = Arc::clone(self);
        *guard = Some(tokio::spawn(async move {
            loop {
                match events.recv().await {
                    Ok(BrowserServiceEvent::Action(record)) => {
                        runtime.persist_action(&record).await;
                    }
                    Ok(_) => {}
                    Err(tokio::sync::broadcast::error::RecvError::Lagged(skipped)) => {
                        tracing::warn!(
                            target: "vibex_browser",
                            skipped,
                            "the browser event stream lagged; some audit rows were dropped"
                        );
                    }
                    Err(tokio::sync::broadcast::error::RecvError::Closed) => return,
                }
            }
        }));
    }

    async fn persist_action(&self, record: &BrowserActionRecord) {
        let owner = self
            .session_owners
            .lock()
            .await
            .get(&record.session_id)
            .cloned()
            .unwrap_or_default();
        let row = Self::audit_record(
            record,
            owner.agent_session_id.as_ref(),
            owner.workspace_id.as_ref(),
        );
        let database_path = self.agent.manager().database_path().to_path_buf();
        let outcome = tokio::task::spawn_blocking(move || {
            let mut connection = vibex_db::open_database(&database_path)?;
            vibex_db::apply_migrations(&mut connection)?;
            vibex_db::BrowserAuditRepository::insert(&connection, &row)
        })
        .await;
        if let Ok(Err(error)) = outcome {
            tracing::warn!(
                target: "vibex_browser",
                error_code = %error.code,
                "failed to persist a browser audit row"
            );
        }
    }

    /// Remembers who owns a browser session.
    async fn remember_owner(
        &self,
        session_id: &vibex_core::BrowserSessionId,
        agent_session_id: Option<VibexSessionId>,
        workspace_id: Option<WorkspaceId>,
    ) {
        self.session_owners.lock().await.insert(
            session_id.clone(),
            SessionOwner {
                agent_session_id,
                workspace_id,
            },
        );
    }

    /// Stops the endpoint and closes the browser.
    pub async fn shutdown(&self) {
        if let Some(task) = self.audit_task.lock().await.take() {
            task.abort();
        }
        if let Some(endpoint) = self.endpoint.lock().await.take() {
            endpoint.stop();
        }
        self.endpoint_url.lock().await.take();
        self.session_owners.lock().await.clear();
        self.service.shutdown().await;
    }

    /// Installs the MCP launch configuration on the Agent manager.
    pub async fn install_tool_config(
        self: &Arc<Self>,
        sidecar_command: PathBuf,
    ) -> VibexResult<()> {
        let Some(endpoint) = self.endpoint_url.lock().await.clone() else {
            return Err(VibexError::process(
                "browser_mcp_endpoint_missing",
                "the browser MCP endpoint must be started before the tool is installed",
            ));
        };
        let config = vibex_agent::BrowserMcpToolConfig {
            command: sidecar_command,
            endpoint,
            capability_token: self.capability_token.clone(),
        };
        match self.agent.manager().install_browser_mcp_tool(config) {
            Ok(()) => Ok(()),
            // A second install is a no-op: the process may rebuild the runtime
            // in tests or after a restart within the same manager.
            Err(error) if error.code == "browser_mcp_tool_already_installed" => Ok(()),
            Err(error) => Err(error),
        }
    }

    /// Maps a redacted ledger entry onto the persisted audit row.
    pub fn audit_record(
        entry: &BrowserActionRecord,
        agent_session_id: Option<&VibexSessionId>,
        workspace_id: Option<&WorkspaceId>,
    ) -> vibex_db::BrowserAuditRecord {
        vibex_db::BrowserAuditRecord {
            id: entry.id.clone(),
            session_id: entry.session_id.as_str().to_string(),
            workspace_id: workspace_id.map(|workspace| workspace.as_str().to_string()),
            agent_session_id: agent_session_id.map(|session| session.as_str().to_string()),
            tab_id: entry.tab_id.as_str().to_string(),
            kind: entry.kind.as_str().to_string(),
            // Already redacted by the browser layer: no page text, form values,
            // cookies or screenshots ever reach this field.
            summary: entry.summary.clone(),
            domain: entry.domain.clone(),
            execution_source: match entry.execution_source {
                vibex_core::BrowserExecutionSource::Agent => "agent".to_string(),
                vibex_core::BrowserExecutionSource::User => "user".to_string(),
            },
            status: match entry.status {
                vibex_core::BrowserOperationStatus::Verified => "verified".to_string(),
                vibex_core::BrowserOperationStatus::Dispatched => "dispatched".to_string(),
                vibex_core::BrowserOperationStatus::Failed => "failed".to_string(),
                vibex_core::BrowserOperationStatus::Unknown => "unknown".to_string(),
            },
            at_ms: entry.at_ms,
        }
    }

    /// How browser tools reach one Agent, for the capability matrix the UI
    /// shows. The runtime does not pretend every Agent is equal.
    pub fn tool_delivery_for_agent(agent_id: &str) -> BrowserToolDelivery {
        use vibex_agent_acp::{McpWireDelivery, agent_dialect_profile};
        match agent_dialect_profile(agent_id).mcp_wire_delivery {
            McpWireDelivery::Delivered => BrowserToolDelivery::Http,
            // These Agents never receive wire MCP servers today, so neither the
            // delegation tool nor the browser tool can reach them. Saying so is
            // better than listing tools that can never be called.
            McpWireDelivery::NativeConfig
            | McpWireDelivery::AcceptedButDropped
            | McpWireDelivery::Rejected => BrowserToolDelivery::Unavailable,
        }
    }

    /// The delivery answer for Agents that follow the generic dialect.
    ///
    /// `agent_dialect_profiles()` only lists Agents that deviate from the
    /// generic path, so an Agent missing from the table — Claude and Codex among
    /// them — takes this answer.
    pub fn generic_tool_delivery() -> BrowserToolDelivery {
        BrowserToolDelivery::Http
    }

    /// The list of exceptions the UI shows next to the capability matrix.
    ///
    /// Only Agents that deviate from the generic delivery are listed, because
    /// those are the ones a user needs to know about: for them the browser tools
    /// are advertised as unavailable rather than offered and silently dropped.
    pub fn delivery_matrix() -> Vec<(String, BrowserToolDelivery)> {
        vibex_agent_acp::agent_dialect_profiles()
            .iter()
            .map(|profile| {
                (
                    profile.agent_id.to_string(),
                    Self::tool_delivery_for_agent(profile.agent_id),
                )
            })
            .collect()
    }
}

impl BrowserRuntime {
    /// Reads back whether the approval was recorded as "always allow".
    ///
    /// The permission row keeps the full resolution JSON but the repository has
    /// no accessor for it, and the browser layer is the only consumer that needs
    /// to tell a one-off approval from a session grant. Reading the single
    /// column here keeps that requirement out of the shared repository.
    async fn last_always_allow(&self, request_id: &RequestId) -> bool {
        let database_path = self.agent.manager().database_path().to_path_buf();
        let request_id = request_id.clone();
        tokio::task::spawn_blocking(move || {
            let Ok(connection) = vibex_db::open_database(&database_path) else {
                return false;
            };
            let Ok(mut statement) = connection
                .prepare("SELECT resolution_json FROM permission_requests WHERE request_id = ?1")
            else {
                return false;
            };
            let Ok(value) =
                statement.query_row([request_id.as_str()], |row| row.get::<_, Option<String>>(0))
            else {
                return false;
            };
            value.is_some_and(|json| json.contains("always_allow"))
        })
        .await
        .unwrap_or(false)
    }
}

/// Adapts raw terminal output to the browser service's dev-server detector.
///
/// Runs on the PTY reader thread, so it only decodes the bytes and hands them
/// on: the scan is bounded and the readiness probe is spawned by the service.
pub fn terminal_output_observer(service: BrowserService) -> vibex_terminal::TerminalOutputObserver {
    std::sync::Arc::new(
        move |_terminal_id: &vibex_core::TerminalId, workspace_id: &WorkspaceId, bytes: &[u8]| {
            if bytes.is_empty() {
                return;
            }
            let text = String::from_utf8_lossy(bytes);
            service.observe_terminal_output(workspace_id, &text);
        },
    )
}

/// Adapts [`BrowserRuntime`] to the browser crate's MCP host seam.
///
/// A distinct type rather than an `impl ... for Arc<BrowserRuntime>` because the
/// trait and `Arc` are both foreign to this crate; the newtype is also the
/// natural place to keep the token and approval policy out of `BrowserRuntime`'s
/// public surface.
pub struct RuntimeBrowserHost {
    runtime: Arc<BrowserRuntime>,
}

impl std::fmt::Debug for RuntimeBrowserHost {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("RuntimeBrowserHost").finish()
    }
}

#[async_trait]
impl BrowserMcpHost for RuntimeBrowserHost {
    async fn resolve_token(&self, token: &str) -> Option<BrowserMcpSession> {
        let session_id = verify_session_token(self.runtime.capability_token(), token)?;
        let agent_session_id = VibexSessionId::parse(session_id.clone()).ok()?;
        // The Agent session must still exist: a revoked or deleted session must
        // not keep a live browser credential.
        let session = self
            .runtime
            .agent
            .manager()
            .get_session(&agent_session_id)
            .await
            .ok()?;
        let tier = browser_tool_tier(&session.agent_id);
        let session_key = BrowserSessionKey::Agent(agent_session_id.clone());
        let browser_session_id = self
            .runtime
            .service
            .ensure_session(session_key, Some(session.workspace_id.clone()))
            .await
            .ok()?;
        let mut roots = vec![PathBuf::from(&session.workspace_root)];
        roots.extend(workspace_roots(&session.workspace_root));
        self.runtime
            .remember_owner(
                &browser_session_id,
                Some(agent_session_id.clone()),
                Some(session.workspace_id.clone()),
            )
            .await;
        Some(BrowserMcpSession {
            session_id: browser_session_id,
            agent_session_id: Some(agent_session_id),
            workspace_id: Some(session.workspace_id),
            authorized_roots: roots,
            tier,
            agent_label: session.agent_id.to_string(),
        })
    }

    async fn request_permission(
        &self,
        session: &BrowserMcpSession,
        error: &vibex_browser::BrowserError,
    ) -> BrowserPermissionDecision {
        let Some(agent_session_id) = session.agent_session_id.clone() else {
            return BrowserPermissionDecision::Deny;
        };
        let domain = error
            .diagnostics
            .iter()
            .find(|(key, _)| key == "domain")
            .map(|(_, value)| value.clone())
            .unwrap_or_else(|| "the requested origin".to_string());
        let origin = error
            .diagnostics
            .iter()
            .find(|(key, _)| key == "origin")
            .map(|(_, value)| value.clone())
            .unwrap_or_default();
        let private_network = error.code == "browser_private_network_approval_required";
        let requested_at_ms = unix_timestamp_ms();
        let request = PermissionRequest {
            id: RequestId::new(),
            session_id: agent_session_id.clone(),
            project_id: None,
            workspace_id: session.workspace_id.clone(),
            provider_request_id: None,
            // The existing `Network` category already means "the Agent wants to
            // reach somewhere"; adding a variant would force edits to the
            // exhaustive risk-label matches on desktop and mobile for no gain.
            risk_category: PermissionRiskCategory::Network,
            title: if private_network {
                format!("Agent wants to reach the private address {domain}")
            } else {
                format!("Agent wants to visit {domain}")
            },
            details: vec![
                PermissionActionDetail {
                    label: "Origin".to_string(),
                    value: origin.clone(),
                },
                PermissionActionDetail {
                    label: "Agent".to_string(),
                    value: session.agent_label.clone(),
                },
                PermissionActionDetail {
                    label: "Network".to_string(),
                    value: if private_network {
                        "Private or loopback: the browser runs on the machine hosting the Vibex \
                         runtime, so this can reach services that are not exposed to the network."
                            .to_string()
                    } else {
                        "Public".to_string()
                    },
                },
            ],
            allowed_responses: vec![
                PermissionResponseKind::Approve,
                PermissionResponseKind::Deny,
                PermissionResponseKind::AlwaysAllowForSession,
            ],
            response_options: Vec::new(),
            status: PermissionRequestStatus::Pending,
            requested_at_ms,
            // The browser layer enforces this itself; the runtime never expires
            // a pending request on its own.
            expires_at_ms: Some(requested_at_ms + BROWSER_APPROVAL_TTL_MS),
        };
        if let Err(error) = self
            .runtime
            .agent
            .manager()
            .record_permission_request(request.clone())
            .await
        {
            tracing::warn!(
                target: "vibex_browser",
                code = %error.code,
                "failed to record a browser navigation approval request"
            );
            return BrowserPermissionDecision::Deny;
        }
        let database_path = self.runtime.agent.manager().database_path().to_path_buf();
        let deadline = requested_at_ms + BROWSER_APPROVAL_TTL_MS;
        loop {
            tokio::time::sleep(Duration::from_millis(BROWSER_APPROVAL_POLL_MS)).await;
            let resolved = tokio::task::spawn_blocking({
                let database_path = database_path.clone();
                let request_id = request.id.clone();
                move || {
                    let connection = vibex_db::open_database(&database_path).ok()?;
                    PermissionRepository::get_request(&connection, &request_id).ok()?
                }
            })
            .await
            .ok()
            .flatten();
            match resolved.map(|stored| stored.status) {
                Some(PermissionRequestStatus::Approved) => {
                    // "Always allow for this session" is remembered by the
                    // browser layer, because the runtime keeps no permission
                    // policy store of its own.
                    return match self.runtime.last_always_allow(&request.id).await {
                        true => BrowserPermissionDecision::AlwaysAllow,
                        false => BrowserPermissionDecision::Approve,
                    };
                }
                Some(PermissionRequestStatus::Denied) => {
                    return BrowserPermissionDecision::Deny;
                }
                Some(PermissionRequestStatus::Expired) => {
                    return BrowserPermissionDecision::Deny;
                }
                _ => {}
            }
            if unix_timestamp_ms() >= deadline {
                tracing::info!(
                    target: "vibex_browser",
                    "a browser navigation approval card expired unanswered; denying"
                );
                return BrowserPermissionDecision::Deny;
            }
        }
    }

    async fn report(&self, _session: &BrowserMcpSession, event: &str, payload: Value) {
        tracing::debug!(
            target: "vibex_browser",
            event,
            payload = %payload,
            "browser MCP host event"
        );
        let _ = BROWSER_ACTION_HIGHLIGHT_MS;
    }
}

/// Picks the tool tier for an Agent.
///
/// The tier is a property of the Agent's dialect, not a user setting: giving a
/// weak model the full fine-grained surface produces worse outcomes than giving
/// it two coarse tools it can actually drive.
pub fn browser_tool_tier(agent_id: &vibex_core::AgentId) -> BrowserToolTier {
    if AGENTS_ACCEPTING_IMAGE_TOOL_RESULTS.contains(&agent_id.as_str()) {
        return BrowserToolTier::Visual;
    }
    BrowserToolTier::Fine
}

/// Agents that accept image content inside an MCP tool result.
///
/// Extend this list only after confirming an Agent actually forwards the image
/// to its model. Until an Agent is listed it receives the fine-grained tier
/// without screenshots: advertising `browser_screenshot` to an Agent that drops
/// the image produces a tool that looks available and returns nothing, which is
/// worse than not offering it.
pub const AGENTS_ACCEPTING_IMAGE_TOOL_RESULTS: &[&str] = &["claude", "gemini"];

/// Additional directories a session may read from.
///
/// V1 has no attached-directory concept beyond the workspace root, so this is
/// the single place that would grow if one is added. Returning an empty list is
/// deliberate: the browser's upload and preview tools must not silently widen
/// their reach.
fn workspace_roots(_workspace_root: &str) -> Vec<PathBuf> {
    Vec::new()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tool_delivery_is_honest_about_agents_that_never_receive_wire_mcp() {
        assert_eq!(
            BrowserRuntime::tool_delivery_for_agent("claude"),
            BrowserToolDelivery::Http
        );
        assert_eq!(
            BrowserRuntime::tool_delivery_for_agent("codex"),
            BrowserToolDelivery::Http
        );
        // These Agents read their own configuration file instead, so a wire MCP
        // server never reaches them. Reporting `Unavailable` is the honest
        // answer; listing tools that can never be called is worse than listing
        // none.
        for agent in ["grok", "cursor", "hermes", "pi", "factory-droid"] {
            assert_eq!(
                BrowserRuntime::tool_delivery_for_agent(agent),
                BrowserToolDelivery::Unavailable,
                "{agent} should be reported as unreachable"
            );
        }
    }

    #[test]
    fn the_delivery_matrix_lists_every_agent_that_deviates() {
        let matrix = BrowserRuntime::delivery_matrix();
        assert!(!matrix.is_empty());
        // The table holds every Agent with a deviation profile, reachable or
        // not, so the UI can render one row per profiled Agent.
        for (agent, _) in &matrix {
            assert!(!agent.is_empty());
        }
        assert!(matrix.iter().any(|(agent, _)| agent == "cursor"));
        assert!(matrix.iter().any(|(agent, _)| agent == "pi"));
        // At least one profiled Agent is unreachable, which is the whole reason
        // the matrix exists.
        assert!(
            matrix
                .iter()
                .any(|(_, delivery)| *delivery == BrowserToolDelivery::Unavailable)
        );
    }

    #[test]
    fn agents_missing_from_the_table_take_the_generic_answer() {
        // Claude and Codex are not in the dialect table; they follow the generic
        // path, which delivers browser tools over the runtime's HTTP endpoint.
        assert_eq!(
            BrowserRuntime::generic_tool_delivery(),
            BrowserToolDelivery::Http
        );
        assert_eq!(
            BrowserRuntime::tool_delivery_for_agent("claude"),
            BrowserToolDelivery::Http
        );
    }

    #[test]
    fn workspace_roots_do_not_widen_authorization() {
        assert!(workspace_roots("/work/project").is_empty());
    }
}

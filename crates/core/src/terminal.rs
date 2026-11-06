use serde::{Deserialize, Serialize};

use crate::agent::AgentSessionSafety;
use crate::agent_config::AgentId;
use crate::ids::{ProviderProfileId, TerminalId, VibexSessionId, WorkspaceId};
use crate::workspace::WorkspaceMode;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalStatus {
    Running,
    Exited,
    Killed,
    Stale,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSession {
    pub id: TerminalId,
    pub workspace_id: WorkspaceId,
    pub title: String,
    pub shell: String,
    pub cwd: String,
    pub rows: u16,
    pub cols: u16,
    pub status: TerminalStatus,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub closed_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalCreateRequest {
    pub workspace_id: WorkspaceId,
    pub title: Option<String>,
    pub shell: Option<String>,
    pub cwd: Option<String>,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSwitchShellRequest {
    pub terminal_id: TerminalId,
    pub shell: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalShell {
    pub name: String,
    pub path: String,
    pub is_default: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalWriteRequest {
    pub terminal_id: TerminalId,
    pub data: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalResizeRequest {
    pub terminal_id: TerminalId,
    pub rows: u16,
    pub cols: u16,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalOutputChunk {
    pub terminal_id: TerminalId,
    pub sequence: i64,
    pub data: String,
    pub timestamp_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSnapshot {
    pub session: TerminalSession,
    pub chunks: Vec<TerminalOutputChunk>,
    pub next_sequence: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalAuthActionDescriptor {
    pub id: String,
    pub provider_profile_id: String,
    /// Present when the desktop host launched the interactive authentication
    /// command in a shared PTY. Remote or headless hosts may return `None`.
    #[serde(default)]
    pub terminal_id: Option<TerminalId>,
    pub title: String,
    pub command: String,
    pub args: Vec<String>,
    pub cwd: Option<String>,
    pub env_keys: Vec<String>,
    pub redacted_env_summary: Vec<String>,
}

// ---------------------------------------------------------------------------
// Terminal-mode Agent sessions
// ---------------------------------------------------------------------------
//
// A terminal-mode session keeps Vibex's own session row, sidebar entry, and
// lifecycle, while the conversation itself — timeline, tool cards, diffs,
// approvals — belongs to the Agent CLI's native TUI inside the PTY. Vibex only
// observes what it must render outside that rectangle, so these types are the
// observation contract rather than a second conversation model.

/// How a Logical Session reaches its Agent.
///
/// This is deliberately separate from [`crate::TransportKind`]: that type is
/// the execution fence stored on `session_runtime_bindings` and only ever names
/// ACP. A terminal session never materializes a runtime binding, so it must not
/// widen the binding transport.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SessionTransport {
    /// Vibex drives the Agent over ACP and owns the structured timeline.
    #[default]
    Acp,
    /// The Agent CLI runs in a PTY and owns its own interface.
    Terminal,
}

impl SessionTransport {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Terminal)
    }
}

/// What Vibex can currently say about a terminal session's Agent.
///
/// `Unknown` is a first-class value, not an error: a terminal session with no
/// observation evidence must read as unknown rather than as idle, because
/// claiming "idle" while the CLI waits on an approval prompt is a lie the reader
/// acts on.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalActivity {
    #[default]
    Unknown,
    Idle,
    Working,
    /// The CLI is blocked on the reader, most often an approval prompt.
    Waiting,
    /// The CLI reported a failure it cannot continue from.
    ///
    /// Distinct from `Unknown`: a failed launch is a lifecycle fact, but a
    /// program that says `error` is still running and still says so, and a row
    /// that drew it as idle or busy would hide the thing the reader has to
    /// look at.
    Failed,
}

/// Which observation produced [`TerminalActivity`].
///
/// The sidebar tooltip names this so a reader can tell a confident answer from
/// a guess, and so a stale screen rule is visibly different from a live hook.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalActivitySource {
    /// No evidence, or evidence past its freshness window.
    #[default]
    None,
    /// PTY liveness and output activity only.
    Process,
    /// Bottom-of-buffer screen rules.
    Screen,
    /// The CLI reported its own state.
    Hook,
}

/// Everything Vibex observes about a terminal session that has a GUI consumer.
///
/// Each field exists because something outside the terminal rectangle renders
/// it: the sidebar dot, the unread marker, the title bar, or the restart path.
/// Conversation content is intentionally absent — it never leaves the PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionFacts {
    /// The PTY these facts describe.
    ///
    /// Carried here so a client can attach to the session's terminal from the
    /// session row it already holds, without a second round trip while it is
    /// painting the page.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub terminal_id: Option<TerminalId>,
    pub activity: TerminalActivity,
    pub source: TerminalActivitySource,
    /// When [`Self::activity`] was last confirmed. Readers age a stale value
    /// back to `Unknown` instead of trusting an old answer forever.
    pub observed_at_ms: i64,
    /// Consecutive observations supporting `activity`, used to damp flapping.
    #[serde(default)]
    pub confirmations: u8,
    /// The title the CLI published (OSC 0/2 or a hook report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_title: Option<String>,
    /// The working directory the CLI published (OSC 7 or a hook report).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cli_cwd: Option<String>,
    /// The Agent's own session id, which is what a restart resumes.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub native_session_id: Option<String>,
    /// When the CLI last started waiting for the reader.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub waiting_since_ms: Option<i64>,
    /// When the CLI last finished a turn.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub completed_at_ms: Option<i64>,
}

impl Default for TerminalSessionFacts {
    fn default() -> Self {
        Self {
            terminal_id: None,
            activity: TerminalActivity::Unknown,
            source: TerminalActivitySource::None,
            observed_at_ms: 0,
            confirmations: 0,
            cli_title: None,
            cli_cwd: None,
            native_session_id: None,
            waiting_since_ms: None,
            completed_at_ms: None,
        }
    }
}

impl TerminalSessionFacts {
    /// Two facts that disagree only in freshness observation.
    ///
    /// Persistence uses this so a 250ms liveness tick does not rewrite the row
    /// (and its `updated_at_ms`) while nothing a reader sees has moved.
    pub fn same_presentation(&self, other: &Self) -> bool {
        // `terminal_id` is the row's own key, not an observed value: it is read
        // back from the link table and is never written by a fact update, so
        // comparing it would report a change on every call.
        self.activity == other.activity
            && self.source == other.source
            && self.cli_title == other.cli_title
            && self.cli_cwd == other.cli_cwd
            && self.native_session_id == other.native_session_id
            && self.waiting_since_ms == other.waiting_since_ms
            && self.completed_at_ms == other.completed_at_ms
    }

    /// Whether the observation is old enough that a reader must not trust it.
    pub fn is_stale_at(&self, now_ms: i64, max_age_ms: i64) -> bool {
        now_ms.saturating_sub(self.observed_at_ms) > max_age_ms
    }
}

/// Facts a launcher must know to start a terminal-mode Agent.
///
/// Arguments are passed straight to the child; no shell interpolation happens.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionLaunchSpec {
    pub workspace_id: WorkspaceId,
    pub workspace_root: String,
    /// The Agent's native interactive executable followed by its arguments.
    pub argv: Vec<String>,
    /// Environment overlays resolved for the Agent, already free of secrets the
    /// launcher must not persist.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub env: Vec<(String, String)>,
    pub title: String,
    pub rows: u16,
    pub cols: u16,
}

/// A started terminal-mode Agent process.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TerminalSessionHandle {
    pub terminal_id: TerminalId,
}

/// Request to create a Logical Session whose Agent runs in a PTY.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateTerminalSessionRequest {
    /// Identifier reserved by the caller so an optimistic row and the
    /// authoritative row share one route.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub session_id: Option<VibexSessionId>,
    pub agent_id: AgentId,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub title: Option<String>,
    pub safety: Option<AgentSessionSafety>,
    /// Extra arguments appended after the Agent's native executable.
    ///
    /// These are launch flags, not conversation: they are replayed verbatim when
    /// the session is restarted, so a prompt must not be put here.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub args: Vec<String>,
    /// The turn the reader already described on the page that created this
    /// session.
    ///
    /// Kept apart from `args` because it belongs to the first launch only: it is
    /// the CLI's positional prompt, and replaying it on a restart would make the
    /// Agent answer the same request twice.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_prompt: Option<String>,
    /// Provider Profile whose credentials and endpoint the CLI should inherit.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_profile_id: Option<ProviderProfileId>,
    pub rows: u16,
    pub cols: u16,
}

/// Whether this release supports a terminal-mode session for an Agent.
///
/// The mapping from Agent to native CLI lives beside the Agent catalog; this
/// type is only the projection a client renders in the new-session panel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TerminalSessionSupport {
    /// The Agent's native TUI, state recognition, and restart path were
    /// exercised together for this release.
    Supported,
    /// A native executable is known but this release has not verified it.
    Unverified,
    /// No native interactive executable is known for the Agent.
    Unavailable,
}

impl TerminalSessionSupport {
    pub const fn is_supported(self) -> bool {
        matches!(self, Self::Supported)
    }
}

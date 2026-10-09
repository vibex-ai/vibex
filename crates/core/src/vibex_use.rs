//! Vibex-use: the product contract behind the Agent-facing session tools.
//!
//! Vibex-use is not a second Agent scheduler. It is the typed boundary between
//! an Agent's tool request and the authoritative runtime that already owns
//! sessions, delegations and message submissions. Everything in this module is
//! provider-neutral: ACP adapters, MCP sidecars and GUI shells all agree on
//! these shapes instead of inventing their own.
//!
//! Three rules shape the types here:
//!
//! 1. **Identity before protocol.** Task, execution and operation identity is
//!    durable and product-owned. Provider session ids, ACP connection ids and
//!    pane ids never appear as routing keys.
//! 2. **Facts are separate.** A message submission, a provider turn and a task
//!    acceptance are three different facts and are never substituted for one
//!    another.
//! 3. **Presentation is not execution.** A group membership or a split-pane
//!    request never grants control over the sessions it shows.

use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::{
    AgentDelegationId, AgentId, MessageSubmissionId, ProviderProfileId, SessionRuntimeSelection,
    VibexExecutionId, VibexOperationId, VibexSessionId,
};

// ---------------------------------------------------------------------------
// Policy defaults
// ---------------------------------------------------------------------------

/// Scheme of every resource reference returned to an Agent.
pub const VIBEX_USE_REF_SCHEME: &str = "vibex";
/// `serverInfo.name` reported by the built-in stdio sidecar.
pub const VIBEX_USE_SERVER_NAME: &str = "vibex-use";
/// The wire server id stays the historical one so an Agent that already has the
/// sidecar configured keeps working across the upgrade.
pub const VIBEX_USE_WIRE_SERVER_ID: &str = "vibex-agent-delegation";
/// Maximum delegation nesting depth counted from the root session.
pub const VIBEX_USE_MAX_DEPTH: u32 = 2;
/// Maximum concurrent executions a single root session may own.
pub const VIBEX_USE_ROOT_EXECUTION_BUDGET: u32 = 8;
/// Upper bound accepted for `timeout_ms` on one `vibex_wait` call. It stays
/// below the sidecar's own 30-second broker read timeout so the wait has room
/// to answer before the transport gives up.
pub const VIBEX_USE_MAX_WAIT_MS: u64 = 25_000;
/// Default `max_items` for a content read.
pub const VIBEX_USE_DEFAULT_READ_ITEMS: usize = 50;
/// Hard ceiling for `max_items`, enforced by the service.
pub const VIBEX_USE_MAX_READ_ITEMS: usize = 200;
/// Default character budget for one content read response.
pub const VIBEX_USE_DEFAULT_READ_CHARS: usize = 16_000;
/// Hard ceiling for `max_chars`, enforced by the service.
pub const VIBEX_USE_MAX_READ_CHARS: usize = 64_000;
/// Maximum number of task references accepted by one batch call.
pub const VIBEX_USE_MAX_BATCH_REFS: usize = 32;
/// Maximum number of context references one task may carry.
pub const VIBEX_USE_MAX_CONTEXT_REFS: usize = 16;

/// How many `@` references one prompt may carry into its route note.
pub const VIBEX_USE_MAX_MENTIONS: usize = 16;
/// Maximum number of members one presentation request may name.
pub const VIBEX_USE_MAX_GROUP_MEMBERS: usize = 16;
/// Maximum number of live panes a presented layout may request.
pub const VIBEX_USE_MAX_LIVE_PANES: usize = 4;
/// Character budget for one bounded summary string.
pub const VIBEX_USE_SUMMARY_CHARS: usize = 480;
/// Character budget for a task title.
pub const VIBEX_USE_TITLE_CHARS: usize = 160;
/// Character budget for an idempotency key.
pub const VIBEX_USE_IDEMPOTENCY_KEY_CHARS: usize = 160;

// ---------------------------------------------------------------------------
// Resource references
// ---------------------------------------------------------------------------

/// The resource kinds that may appear in a `vibex://` reference.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseResourceKind {
    Session,
    Task,
    Execution,
    Operation,
    Group,
    Workspace,
    RuntimeOption,
    Catalog,
}

impl VibexUseResourceKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Session => "session",
            Self::Task => "task",
            Self::Execution => "execution",
            Self::Operation => "operation",
            Self::Group => "group",
            Self::Workspace => "workspace",
            Self::RuntimeOption => "runtime-option",
            Self::Catalog => "catalog",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        Some(match value {
            "session" => Self::Session,
            "task" => Self::Task,
            "execution" => Self::Execution,
            "operation" => Self::Operation,
            "group" => Self::Group,
            "workspace" => Self::Workspace,
            "runtime-option" => Self::RuntimeOption,
            "catalog" => Self::Catalog,
            _ => return None,
        })
    }
}

/// One stable, readable, copyable product reference.
///
/// A reference names a resource; it is never an authorization credential. The
/// service re-resolves and re-authorizes every reference on every call.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct VibexUseRef {
    pub kind: VibexUseResourceKind,
    pub id: String,
}

impl VibexUseRef {
    pub fn new(kind: VibexUseResourceKind, id: impl Into<String>) -> Self {
        Self {
            kind,
            id: id.into(),
        }
    }

    pub fn session(id: &VibexSessionId) -> Self {
        Self::new(VibexUseResourceKind::Session, id.as_str())
    }

    pub fn task(id: &AgentDelegationId) -> Self {
        Self::new(VibexUseResourceKind::Task, id.as_str())
    }

    pub fn execution(id: &VibexExecutionId) -> Self {
        Self::new(VibexUseResourceKind::Execution, id.as_str())
    }

    pub fn operation(id: &VibexOperationId) -> Self {
        Self::new(VibexUseResourceKind::Operation, id.as_str())
    }

    pub fn group(id: &str) -> Self {
        Self::new(VibexUseResourceKind::Group, id)
    }

    pub fn workspace(id: &str) -> Self {
        Self::new(VibexUseResourceKind::Workspace, id)
    }

    pub fn runtime_option(id: &str) -> Self {
        Self::new(VibexUseResourceKind::RuntimeOption, id)
    }

    pub fn catalog(revision: u64) -> Self {
        Self::new(VibexUseResourceKind::Catalog, revision.to_string())
    }

    /// Parses `vibex://<kind>/<id>` and the compact `<kind>:<id>` spelling.
    pub fn parse(value: &str) -> Option<Self> {
        let value = value.trim();
        let rest = value
            .strip_prefix("vibex://")
            .or_else(|| value.strip_prefix("vibex:"))?;
        let (kind, id) = rest.split_once('/')?;
        let kind = VibexUseResourceKind::parse(kind)?;
        let id = id.trim();
        if id.is_empty() || id.len() > 256 || id.chars().any(char::is_control) {
            return None;
        }
        Some(Self {
            kind,
            id: id.to_string(),
        })
    }

    pub fn as_uri(&self) -> String {
        format!(
            "{VIBEX_USE_REF_SCHEME}://{}/{}",
            self.kind.as_str(),
            self.id
        )
    }

    pub fn execution_id(&self) -> Option<VibexExecutionId> {
        (self.kind == VibexUseResourceKind::Execution)
            .then(|| VibexExecutionId::parse(self.id.clone()).ok())
            .flatten()
    }

    pub fn operation_id(&self) -> Option<VibexOperationId> {
        (self.kind == VibexUseResourceKind::Operation)
            .then(|| VibexOperationId::parse(self.id.clone()).ok())
            .flatten()
    }

    pub fn group_id(&self) -> Option<crate::SessionGroupId> {
        (self.kind == VibexUseResourceKind::Group)
            .then(|| crate::SessionGroupId::parse(self.id.clone()).ok())
            .flatten()
    }

    pub fn session_id(&self) -> Option<VibexSessionId> {
        (self.kind == VibexUseResourceKind::Session)
            .then(|| VibexSessionId::parse(self.id.clone()).ok())
            .flatten()
    }

    pub fn task_id(&self) -> Option<AgentDelegationId> {
        (self.kind == VibexUseResourceKind::Task)
            .then(|| AgentDelegationId::parse(self.id.clone()).ok())
            .flatten()
    }
}

impl std::fmt::Display for VibexUseRef {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(&self.as_uri())
    }
}

impl Serialize for VibexUseRef {
    fn serialize<S>(&self, serializer: S) -> Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        serializer.serialize_str(&self.as_uri())
    }
}

impl<'de> Deserialize<'de> for VibexUseRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::parse(&value)
            .ok_or_else(|| serde::de::Error::custom("invalid vibex resource reference"))
    }
}

// ---------------------------------------------------------------------------
// Composer mentions
// ---------------------------------------------------------------------------

/// Prefix a composer token id carries when it names a collaborator or a session.
pub const VIBEX_USE_MENTION_PREFIX: &str = "vibex-use:";

/// What kind of resource one `@` mention points at.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseMentionKind {
    /// A collaborator, identified by its Agent and the runtime option the user
    /// picked.
    Agent,
    /// An existing conversation the user pointed at.
    Session,
}

impl VibexUseMentionKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Agent => "agent",
            Self::Session => "session",
        }
    }
}

/// One `@` reference the user put in the composer.
///
/// The mention is a typed contract rather than a display string: it survives
/// drafts, the clipboard and a remote client's degraded view, and the send
/// boundary reads it back without a side table. The display name is only a
/// label — the identity that travels is the stable Agent id and the
/// `vibex://` reference the Agent tools accept.
///
/// A mention is a *suggestion* to the main Agent, never a grant. The call that
/// follows re-authorizes every reference it names.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseMention {
    pub kind: VibexUseMentionKind,
    /// The resource the mention resolves to: a runtime option for a
    /// collaborator, a session for a conversation.
    pub reference: VibexUseRef,
    /// The stable Agent the user named, for a collaborator mention.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_id: Option<AgentId>,
    /// The name the user saw, kept for the route note only.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
}

impl VibexUseMention {
    /// A collaborator mention: an Agent plus the configuration that was offered.
    pub fn agent(agent_id: &AgentId, option: &VibexUseRef, label: Option<&str>) -> Self {
        Self {
            kind: VibexUseMentionKind::Agent,
            reference: option.clone(),
            agent_id: Some(agent_id.clone()),
            label: label.map(str::to_string),
        }
    }

    /// A conversation mention.
    pub fn session(id: &VibexSessionId, label: Option<&str>) -> Self {
        Self {
            kind: VibexUseMentionKind::Session,
            reference: VibexUseRef::session(id),
            agent_id: None,
            label: label.map(str::to_string),
        }
    }

    /// The id a composer token carries.
    ///
    /// The whole mention is encoded, so a token restored from a draft or
    /// pasted into another window still resolves to the same collaborator and
    /// the same configuration.
    pub fn encode(&self) -> String {
        let option = self.reference.id.as_str();
        match self.kind {
            VibexUseMentionKind::Agent => format!(
                "{VIBEX_USE_MENTION_PREFIX}agent/{}:{}",
                self.agent_id
                    .as_ref()
                    .map(AgentId::as_str)
                    .unwrap_or_default(),
                option
            ),
            VibexUseMentionKind::Session => {
                format!("{VIBEX_USE_MENTION_PREFIX}session/{}", self.reference.id)
            }
        }
    }

    /// Reads a mention back out of a composer token id.
    pub fn parse(value: &str) -> Option<Self> {
        let rest = value.strip_prefix(VIBEX_USE_MENTION_PREFIX)?;
        let (kind, body) = rest.split_once('/')?;
        match kind {
            "agent" => {
                let (agent_id, option_id) = body.split_once(':')?;
                if option_id.is_empty() {
                    return None;
                }
                let agent_id = AgentId::parse(agent_id).ok()?;
                Some(Self {
                    kind: VibexUseMentionKind::Agent,
                    reference: VibexUseRef::runtime_option(option_id),
                    agent_id: Some(agent_id),
                    label: None,
                })
            }
            "session" => {
                let session_id = VibexSessionId::parse(body).ok()?;
                Some(Self {
                    kind: VibexUseMentionKind::Session,
                    reference: VibexUseRef::session(&session_id),
                    agent_id: None,
                    label: None,
                })
            }
            _ => None,
        }
    }
}

/// Builds the provider-only route note for the mentions of one prompt.
///
/// It tells the main Agent which collaborator the user named and how to reach
/// the product's session tools. It is a model instruction, not a credential:
/// importing a `@` or a `vibex://` link from history grants nothing.
pub fn vibex_use_route_note(mentions: &[VibexUseMention]) -> Option<String> {
    if mentions.is_empty() {
        return None;
    }
    let mut lines = vec![
        "The user addressed these collaborators and conversations in the request above."
            .to_string(),
        "Use the Vibex-use session tools (vibex_discover, vibex_delegate, vibex_send_message, \
         vibex_wait, vibex_get_tasks, vibex_read_session) to act on them; the references below \
         are the identity to pass, not an authorization."
            .to_string(),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for mention in mentions.iter().take(VIBEX_USE_MAX_MENTIONS) {
        let line = match mention.kind {
            VibexUseMentionKind::Agent => format!(
                "- collaborator {}{}: {}",
                mention.label.as_deref().unwrap_or("(unnamed)"),
                mention
                    .agent_id
                    .as_ref()
                    .map(|id| format!(" ({})", id.as_str()))
                    .unwrap_or_default(),
                mention.reference.as_uri()
            ),
            VibexUseMentionKind::Session => format!(
                "- conversation {}{}: {}",
                mention.label.as_deref().unwrap_or("(unnamed)"),
                String::new(),
                mention.reference.as_uri()
            ),
        };
        // One note per distinct reference, however many times it was typed.
        if seen.insert(line.clone()) {
            lines.push(line);
        }
    }
    lines.push(
        "A delegated session receives only the context declared in the call, never this whole \
         conversation. Prefer vibex_wait over polling, and read results with \
         vibex_read_session before reporting them."
            .to_string(),
    );
    Some(lines.join("\n"))
}

// ---------------------------------------------------------------------------
// Actor and scope
// ---------------------------------------------------------------------------

/// Who is calling, derived from the capability the caller actually holds.
///
/// Nothing here may be supplied by a tool argument: the broker derives the
/// actor from the session-scoped token that launched the sidecar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseActor {
    /// Identifier of the runtime authority that issued every reference in this
    /// exchange. A paired client must never resolve a foreign authority's
    /// references against its own local runtime.
    pub authority: String,
    /// The Agent session whose sidecar made the call. It is derived from the
    /// session-scoped capability that launched the sidecar, never from a tool
    /// argument, so a caller cannot impersonate another parent.
    pub session_id: VibexSessionId,
    /// Monotonic revision of the delivery activation that authorized this
    /// caller. A call is re-validated against the current revision.
    pub activation_revision: u64,
}

impl VibexUseActor {
    pub fn new(
        authority: impl Into<String>,
        session_id: VibexSessionId,
        activation_revision: u64,
    ) -> Self {
        Self {
            authority: authority.into(),
            session_id,
            activation_revision,
        }
    }

    /// Stable key that scopes idempotency to one authority and actor.
    pub fn key(&self) -> String {
        format!("{}\u{1f}{}", self.authority, self.session_id.as_str())
    }
}

/// How the caller relates to one target session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseScope {
    /// The caller created this session through Vibex-use and still owns it.
    Owned,
    /// The caller was explicitly granted control of an existing session.
    Controlled,
    /// The caller may read the session because the user referenced it.
    Referenced,
    /// The caller may only observe bounded metadata.
    ReadOnly,
}

impl VibexUseScope {
    pub const fn can_write(self) -> bool {
        matches!(self, Self::Owned | Self::Controlled)
    }

    pub const fn can_read_content(self) -> bool {
        matches!(self, Self::Owned | Self::Controlled | Self::Referenced)
    }
}

/// Delivery channel that actually reaches one Agent session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseDelivery {
    /// Built-in stdio MCP server injected into the Agent's session.
    Mcp,
    /// A CLI plus Skill channel driving the same domain service.
    CliSkill,
    /// No channel is reachable for this Agent right now.
    Unavailable,
}

/// One workspace the caller may name when creating a session.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseWorkspaceOption {
    pub reference: VibexUseRef,
    pub project_id: String,
    pub label: String,
    pub workspace_mode: String,
    /// Whether a group could show sessions of this workspace together with the
    /// caller's own workspace. Cross-Worktree groups are not in the first
    /// version, so this is reported honestly instead of silently accepted.
    pub shareable_with_caller: bool,
}

/// One selectable runtime configuration, keyed by a stable reference.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseRuntimeOption {
    pub reference: VibexUseRef,
    pub agent_id: AgentId,
    pub label: String,
    pub configuration_label: String,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub model_id: Option<String>,
    pub reasoning_effort: Option<String>,
    pub mode_id: Option<String>,
    /// ACP `configOptions` values that the target really supports.
    #[serde(default)]
    pub config_values: BTreeMap<String, String>,
    /// Stable reason the option cannot be used right now, if any.
    #[serde(default)]
    pub unavailable_reason: Option<VibexUseUnavailableReason>,
    /// The typed selection to send back. A label is never a routing key.
    pub selection: SessionRuntimeSelection,
}

/// Machine-readable reason a capability or target is unavailable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseUnavailableReason {
    AgentDisabled,
    AgentNotInstalled,
    RequiresAuthentication,
    RequiresConfiguration,
    ModelUnavailable,
    DeliveryUnsupported,
    NoPresentationClient,
    NoShell,
    ForeignAuthority,
    CrossWorkspaceUnsupported,
    TaskBudgetExhausted,
    DepthLimitReached,
}

impl VibexUseUnavailableReason {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::AgentDisabled => "agent_disabled",
            Self::AgentNotInstalled => "agent_not_installed",
            Self::RequiresAuthentication => "requires_authentication",
            Self::RequiresConfiguration => "requires_configuration",
            Self::ModelUnavailable => "model_unavailable",
            Self::DeliveryUnsupported => "delivery_unsupported",
            Self::NoPresentationClient => "no_presentation_client",
            Self::NoShell => "no_shell",
            Self::ForeignAuthority => "foreign_authority",
            Self::CrossWorkspaceUnsupported => "cross_workspace_unsupported",
            Self::TaskBudgetExhausted => "task_budget_exhausted",
            Self::DepthLimitReached => "depth_limit_reached",
        }
    }
}

/// What the caller can actually do in this runtime, right now.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseCapabilitySnapshot {
    pub delivery: VibexUseDelivery,
    /// Present when `delivery` is [`VibexUseDelivery::Unavailable`].
    #[serde(default)]
    pub delivery_reason: Option<VibexUseUnavailableReason>,
    pub authority: String,
    pub caller_session_ref: VibexUseRef,
    pub root_session_ref: VibexUseRef,
    #[serde(default)]
    pub current_task_ref: Option<VibexUseRef>,
    pub current_workspace_ref: VibexUseRef,
    pub catalog_revision: u64,
    pub activation_revision: u64,
    /// Remaining execution slots under the root budget.
    pub remaining_executions: u32,
    pub max_depth: u32,
    pub remaining_depth: u32,
    pub can_delegate: bool,
    /// Why new delegated work is refused, when it is refused for a reason other
    /// than depth or budget. A cancel that is still being confirmed is the
    /// first such reason: it fences the whole subtree.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_blocked_by: Option<String>,
    pub can_read_any_session: bool,
    pub presentation: VibexUsePresentationCapability,
    /// Tools this caller may actually use. A tool that cannot succeed is not
    /// advertised as available.
    pub available_tools: Vec<String>,
}

/// What the connected shell can do with a team group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUsePresentationCapability {
    pub available: bool,
    #[serde(default)]
    pub reason: Option<VibexUseUnavailableReason>,
    pub supports_layout: bool,
    pub supports_focus: bool,
    pub supports_cross_workspace: bool,
    pub max_live_panes: usize,
    pub max_members: usize,
}

impl Default for VibexUsePresentationCapability {
    fn default() -> Self {
        Self {
            available: false,
            reason: Some(VibexUseUnavailableReason::NoShell),
            supports_layout: false,
            supports_focus: false,
            supports_cross_workspace: false,
            max_live_panes: 0,
            max_members: 0,
        }
    }
}

// ---------------------------------------------------------------------------
// Tool catalogue
// ---------------------------------------------------------------------------

/// Every Vibex-use tool, in catalogue order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseTool {
    Discover,
    ListSessions,
    GetSession,
    ReadSession,
    CreateSession,
    Delegate,
    SendMessage,
    GetOperation,
    GetTasks,
    Wait,
    FinishTask,
    Interrupt,
    CancelTask,
    GetEvents,
    AckEvents,
    ListGroups,
    CreateGroup,
    UpdateGroup,
    PresentGroup,
    DissolveGroup,
}

impl VibexUseTool {
    pub const ALL: [Self; 20] = [
        Self::Discover,
        Self::ListSessions,
        Self::GetSession,
        Self::ReadSession,
        Self::CreateSession,
        Self::Delegate,
        Self::SendMessage,
        Self::GetOperation,
        Self::GetTasks,
        Self::Wait,
        Self::FinishTask,
        Self::Interrupt,
        Self::CancelTask,
        Self::GetEvents,
        Self::AckEvents,
        Self::ListGroups,
        Self::CreateGroup,
        Self::UpdateGroup,
        Self::PresentGroup,
        Self::DissolveGroup,
    ];

    pub const fn name(self) -> &'static str {
        match self {
            Self::Discover => "vibex_discover",
            Self::ListSessions => "vibex_list_sessions",
            Self::GetSession => "vibex_get_session",
            Self::ReadSession => "vibex_read_session",
            Self::CreateSession => "vibex_create_session",
            Self::Delegate => "vibex_delegate",
            Self::SendMessage => "vibex_send_message",
            Self::GetOperation => "vibex_get_operation",
            Self::GetTasks => "vibex_get_tasks",
            Self::Wait => "vibex_wait",
            Self::FinishTask => "vibex_finish_task",
            Self::Interrupt => "vibex_interrupt",
            Self::CancelTask => "vibex_cancel_task",
            Self::GetEvents => "vibex_get_events",
            Self::AckEvents => "vibex_ack_events",
            Self::ListGroups => "vibex_list_groups",
            Self::CreateGroup => "vibex_create_group",
            Self::UpdateGroup => "vibex_update_group",
            Self::PresentGroup => "vibex_present_group",
            Self::DissolveGroup => "vibex_dissolve_group",
        }
    }

    pub fn parse(name: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|tool| tool.name() == name)
    }

    /// Whether the tool only reads. Read tools never need a write grant.
    pub const fn is_read_only(self) -> bool {
        matches!(
            self,
            Self::Discover
                | Self::ListSessions
                | Self::GetSession
                | Self::ReadSession
                | Self::GetOperation
                | Self::GetTasks
                | Self::Wait
                | Self::GetEvents
                | Self::ListGroups
        )
    }

    /// Whether the tool changes an execution or a task lifecycle.
    pub const fn mutates_execution(self) -> bool {
        matches!(
            self,
            Self::Delegate
                | Self::SendMessage
                | Self::FinishTask
                | Self::Interrupt
                | Self::CancelTask
        )
    }

    /// Whether the tool only changes presentation.
    pub const fn is_presentation(self) -> bool {
        matches!(
            self,
            Self::ListGroups
                | Self::CreateGroup
                | Self::UpdateGroup
                | Self::PresentGroup
                | Self::DissolveGroup
        )
    }

    /// Whether the tool needs a client that can lay panes out.
    ///
    /// `list_groups` is deliberately not on this list: reading which groups
    /// exist is useful on a client that can never draw one.
    pub const fn needs_layout_capability(self) -> bool {
        matches!(
            self,
            Self::CreateGroup | Self::UpdateGroup | Self::PresentGroup | Self::DissolveGroup
        )
    }
}

/// The `tools/list` payload of one activation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseToolDefinition {
    pub name: String,
    pub description: String,
    pub input_schema: Value,
}

/// Builds the whole catalogue. Tools whose capability is missing are omitted
/// instead of being advertised and then failing.
pub fn vibex_use_tool_definitions(
    capability: &VibexUseCapabilitySnapshot,
) -> Vec<VibexUseToolDefinition> {
    VibexUseTool::ALL
        .into_iter()
        .filter(|tool| {
            if tool.is_presentation() {
                // `list_groups` is still meaningful without a shell because it
                // reports the capability itself.
                if matches!(tool, VibexUseTool::ListGroups) {
                    return true;
                }
                // A client that can show a group but cannot lay panes out
                // cannot build one either, so the writing tools stay hidden
                // instead of being offered and then refused.
                return capability.presentation.available
                    && (!tool.needs_layout_capability()
                        || capability.presentation.supports_layout);
            }
            if matches!(tool, VibexUseTool::Delegate | VibexUseTool::CreateSession) {
                return capability.can_delegate;
            }
            true
        })
        .map(|tool| VibexUseToolDefinition {
            name: tool.name().to_string(),
            description: tool_description(tool).to_string(),
            input_schema: tool_input_schema(tool),
        })
        .collect()
}

fn tool_description(tool: VibexUseTool) -> &'static str {
    match tool {
        VibexUseTool::Discover => {
            "Report what this Agent can actually do: delivery channel, caller/root/workspace \
             references, remaining budget, selectable runtime options, readable sessions and \
             whether a client can present a session group. Call this first."
        }
        VibexUseTool::ListSessions => {
            "List sessions inside the caller's scope as a flat page. Use roots_only for the \
             top-level list and parent_session_ref for the delegated children of one session."
        }
        VibexUseTool::GetSession => {
            "Read the status, desired/effective runtime configuration, current execution and \
             pending attention of one session."
        }
        VibexUseTool::ReadSession => {
            "Read bounded, ordered content of one session. Pick exactly one of latest, after or \
             before; the response returns a cursor bound to the session and view."
        }
        VibexUseTool::CreateSession => {
            "Create a new logical Agent session from a discovery selection. The response carries \
             an operation reference; poll vibex_get_operation until it succeeds."
        }
        VibexUseTool::Delegate => {
            "Register a task and start it in a new or already controlled session. The response is \
             an acceptance, not a completion: task_ref, session_ref, operation_ref and \
             execution_ref come back immediately."
        }
        VibexUseTool::SendMessage => {
            "Queue one more message to a task's session. This is a durable enqueue: the response \
             reports the submission/execution reference and the queue position, not the reply."
        }
        VibexUseTool::GetOperation => {
            "Read the durable outcome of a write request by operation_ref or by its original \
             idempotency key."
        }
        VibexUseTool::GetTasks => {
            "Read task phases, executions, blocked_on and result references for explicit task \
             references or for the caller's whole team."
        }
        VibexUseTool::Wait => {
            "Wait up to timeout_ms for new terminal states, attention requests or failures across \
             references. 0 returns an immediate snapshot. timed_out is not a task failure."
        }
        VibexUseTool::FinishTask => {
            "Accept a task that is awaiting review. The task becomes an immutable terminal record \
             with a fixed result list. Follow-up work is a new task that follows it."
        }
        VibexUseTool::Interrupt => {
            "Abort the current execution of one session. Other tasks in the team keep running; \
             use vibex_cancel_task for that."
        }
        VibexUseTool::CancelTask => {
            "Cancel one task, optionally including the child tasks it owns."
        }
        VibexUseTool::GetEvents => {
            "Read the caller's durable result inbox from a cursor. Events are idempotent by \
             event_id and stay readable until acknowledged."
        }
        VibexUseTool::AckEvents => {
            "Acknowledge delivered event ids. Acknowledgement is a transport fact; it does not \
             claim the model understood the content."
        }
        VibexUseTool::ListGroups => {
            "List session groups the caller may see, with membership, layout summary, revision \
             and whether this client can actually present them."
        }
        VibexUseTool::CreateGroup => {
            "Create a group of already authorized sessions in one workspace. A group is a display \
             unit: it never widens control over its members."
        }
        VibexUseTool::UpdateGroup => {
            "Apply a typed membership, name or layout change to a group the caller created, \
             guarded by expected_revision."
        }
        VibexUseTool::PresentGroup => {
            "Ask a connected client to show a group as a split workspace. Returns presented, \
             prepared, deferred or unavailable; execution is unaffected either way."
        }
        VibexUseTool::DissolveGroup => {
            "Dissolve a group the caller created. Sessions remain and tasks keep running."
        }
    }
}

fn tool_input_schema(tool: VibexUseTool) -> Value {
    use serde_json::json;

    let string = |description: &str| json!({ "type": "string", "description": description });
    match tool {
        VibexUseTool::Discover => json!({
            "type": "object",
            "properties": {
                "agentId": string("Only report options for this Agent."),
                "includeUnavailable": {
                    "type": "boolean",
                    "description": "Also report targets that cannot start right now, with reasons."
                },
                "cursor": string("Paging cursor returned by a previous call."),
                "maxItems": { "type": "integer", "minimum": 1, "maximum": 200 }
            }
        }),
        VibexUseTool::ListSessions => json!({
            "type": "object",
            "properties": {
                "rootsOnly": { "type": "boolean", "description": "Only top-level user sessions." },
                "parentSessionRef": string("List the delegated children of this session."),
                "includeArchived": { "type": "boolean" },
                "cursor": string("Paging cursor returned by a previous call."),
                "maxItems": { "type": "integer", "minimum": 1, "maximum": 200 }
            }
        }),
        VibexUseTool::GetSession => json!({
            "type": "object",
            "properties": { "sessionRef": string("vibex://session/...") },
            "required": ["sessionRef"]
        }),
        VibexUseTool::ReadSession => json!({
            "type": "object",
            "properties": {
                "sessionRef": string("vibex://session/..."),
                "view": { "type": "string", "enum": ["summary", "conversation", "timeline"] },
                "latest": { "type": "boolean", "description": "Read the newest window." },
                "after": { "type": "integer", "description": "Exclusive lower sequence bound." },
                "before": { "type": "integer", "description": "Exclusive upper sequence bound." },
                "cursor": {
                    "type": "object",
                    "description": "A next_cursor from a previous page. It continues that page \
                                    in the same direction and is bound to its session and view."
                },
                "maxItems": { "type": "integer", "minimum": 1, "maximum": 200 },
                "maxChars": { "type": "integer", "minimum": 256, "maximum": 64000 }
            },
            "required": ["sessionRef"]
        }),
        VibexUseTool::CreateSession => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "selectionRef": string("vibex://runtime-option/... from vibex_discover."),
                "catalogRevision": { "type": "integer" },
                "workspaceRef": string("vibex://workspace/... from vibex_discover."),
                "title": string(&format!("Bounded to {VIBEX_USE_TITLE_CHARS} characters.")),
                "firstMessage": string("Optional first user-visible message."),
                "context": { "type": "array", "items": { "type": "object" } }
            },
            "required": ["idempotencyKey", "selectionRef"]
        }),
        VibexUseTool::Delegate => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "task": {
                    "type": "object",
                    "properties": {
                        "title": string("Short user-facing title."),
                        "prompt": string("The complete task handed to the child Agent."),
                        "acceptanceCriteria": { "type": "array", "items": { "type": "string" } },
                        "completionPolicy": {
                            "type": "string",
                            "enum": ["owner_review", "single_turn_legacy"],
                            "description": "owner_review keeps the task open for follow-ups."
                        }
                    },
                    "required": ["prompt"]
                },
                "target": {
                    "type": "object",
                    "properties": {
                        "selectionRef": string("vibex://runtime-option/... from vibex_discover."),
                        "catalogRevision": { "type": "integer" },
                        "agentId": string("Only when no selectionRef is given."),
                        "model": { "type": "string" },
                        "reasoningEffort": { "type": "string" },
                        "modeId": { "type": "string" }
                    }
                },
                "session": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["new", "existing"] },
                        "workspaceRef": string("vibex://workspace/... for a new session."),
                        "sessionRef": string("vibex://session/... for an existing session."),
                        "title": { "type": "string" }
                    },
                    "required": ["kind"]
                },
                "context": {
                    "type": "array",
                    "description": "Explicit context windows. The parent transcript is never copied wholesale.",
                    "items": {
                        "type": "object",
                        "properties": {
                            "sessionRef": { "type": "string" },
                            "fromSequence": { "type": "integer" },
                            "throughSequence": { "type": "integer" },
                            "view": { "type": "string", "enum": ["summary", "conversation", "timeline"] }
                        }
                    }
                },
                "presentation": {
                    "type": "object",
                    "properties": {
                        "groupRef": string("Join this existing group."),
                        "groupName": string("Create or reuse a group with this name."),
                        "present": { "type": "boolean" }
                    }
                },
                "followsTaskRef": string("vibex://task/... of a finished task this continues.")
            },
            "required": ["idempotencyKey", "task", "target"]
        }),
        VibexUseTool::SendMessage => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "sessionRef": string("vibex://session/..."),
                "taskRef": string("vibex://task/... when the message belongs to a task."),
                "text": string("The message text."),
                "expectedControllerRevision": { "type": "integer" },
                "expectedExecutionRef": string("Reject the send when another execution started."),
                "context": { "type": "array", "items": { "type": "object" } },
                "selectionRef": string("Optional new runtime selection for this task.")
            },
            "required": ["idempotencyKey", "sessionRef", "text"]
        }),
        VibexUseTool::GetOperation => json!({
            "type": "object",
            "properties": {
                "operationRef": string("vibex://operation/..."),
                "tool": string("Original tool name when looking up by idempotency key."),
                "idempotencyKey": string("The caller key used by the original request.")
            }
        }),
        VibexUseTool::GetTasks => json!({
            "type": "object",
            "properties": {
                "taskRefs": { "type": "array", "items": { "type": "string" } },
                "includeTeam": { "type": "boolean", "description": "Include every task of the root session." },
                "includeFinished": { "type": "boolean" },
                "cursor": string("Paging cursor returned by a previous call."),
                "maxItems": { "type": "integer", "minimum": 1, "maximum": 200 }
            }
        }),
        VibexUseTool::Wait => json!({
            "type": "object",
            "properties": {
                "taskRefs": { "type": "array", "items": { "type": "string" } },
                "sessionRefs": { "type": "array", "items": { "type": "string" } },
                "afterEventCursor": { "type": "integer" },
                "timeoutMs": {
                    "type": "integer",
                    "minimum": 0,
                    "maximum": VIBEX_USE_MAX_WAIT_MS,
                    "description": "0 returns a snapshot immediately; otherwise a bounded wait."
                }
            }
        }),
        VibexUseTool::FinishTask => json!({
            "type": "object",
            "properties": {
                "taskRef": string("vibex://task/..."),
                "expectedRevision": { "type": "integer" },
                "outcome": { "type": "string", "enum": ["accepted", "rejected"] },
                "summary": string("Why the task is accepted or rejected.")
            },
            "required": ["taskRef"]
        }),
        VibexUseTool::Interrupt => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "sessionRef": string("vibex://session/..."),
                "expectedExecutionRef": string("Only abort when this execution is still current.")
            },
            "required": ["idempotencyKey", "sessionRef"]
        }),
        VibexUseTool::CancelTask => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "taskRef": string("vibex://task/..."),
                "cascade": { "type": "boolean", "description": "Also cancel owned child tasks." }
            },
            "required": ["idempotencyKey", "taskRef"]
        }),
        VibexUseTool::GetEvents => json!({
            "type": "object",
            "properties": {
                "afterEventCursor": { "type": "integer" },
                "maxItems": { "type": "integer", "minimum": 1, "maximum": 200 },
                "includeAcknowledged": { "type": "boolean" }
            }
        }),
        VibexUseTool::AckEvents => json!({
            "type": "object",
            "properties": {
                "eventIds": { "type": "array", "items": { "type": "string" } },
                "throughEventCursor": { "type": "integer" }
            }
        }),
        VibexUseTool::ListGroups => json!({
            "type": "object",
            "properties": {
                "groupRef": string("Read one group instead of the list."),
                "includeMembers": { "type": "boolean" }
            }
        }),
        VibexUseTool::CreateGroup => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses the group."),
                "name": { "type": "string" },
                "scope": {
                    "type": "object",
                    "properties": {
                        "kind": { "type": "string", "enum": ["workspace"] },
                        "workspaceRef": string("vibex://workspace/...")
                    },
                    "required": ["kind"]
                },
                "memberSessionRefs": { "type": "array", "items": { "type": "string" } },
                "layout": {
                    "type": "object",
                    "properties": {
                        "preset": {
                            "type": "string",
                            "enum": ["single", "columns", "lead_and_workers", "grid", "tabs"]
                        },
                        "leadSessionRef": { "type": "string" },
                        "preferredLivePanes": {
                            "type": "integer",
                            "minimum": 1,
                            "maximum": VIBEX_USE_MAX_LIVE_PANES
                        }
                    }
                }
            },
            "required": ["idempotencyKey", "name", "scope", "memberSessionRefs"]
        }),
        VibexUseTool::UpdateGroup => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "groupRef": string("vibex://group/..."),
                "expectedRevision": { "type": "integer" },
                "name": { "type": "string" },
                "memberSessionRefs": { "type": "array", "items": { "type": "string" } },
                "layout": { "type": "object" },
                "releaseOwnership": {
                    "type": "boolean",
                    "description": "Let the user own the group again so later layout edits are not overridden."
                }
            },
            "required": ["idempotencyKey", "groupRef"]
        }),
        VibexUseTool::PresentGroup => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "groupRef": string("vibex://group/..."),
                "activationPolicy": {
                    "type": "string",
                    "enum": ["if_current_team", "when_user_returns"],
                    "description": "How eagerly the client may switch the user's workspace."
                },
                "focusSessionRef": string("Optional pane to focus once applied.")
            },
            "required": ["idempotencyKey", "groupRef"]
        }),
        VibexUseTool::DissolveGroup => json!({
            "type": "object",
            "properties": {
                "idempotencyKey": string("Stable key; retrying the same request reuses it."),
                "groupRef": string("vibex://group/..."),
                "expectedRevision": { "type": "integer" }
            },
            "required": ["idempotencyKey", "groupRef"]
        }),
    }
}

// ---------------------------------------------------------------------------
// Tool host
// ---------------------------------------------------------------------------

/// Boxed future returned by [`VibexUseToolHost::call`].
pub type VibexUseToolFuture<'a> =
    Pin<Box<dyn Future<Output = Result<Value, crate::VibexError>> + Send + 'a>>;

/// The one entry point the protocol adapter knows about.
///
/// The MCP sidecar owns framing only: it derives the actor from its scoped
/// capability and forwards the typed tool name plus arguments. Authorization,
/// idempotency and orchestration all happen behind this boundary.
pub trait VibexUseToolHost: Send + Sync + 'static {
    fn call(
        &self,
        actor: VibexUseActor,
        tool: VibexUseTool,
        arguments: Value,
    ) -> VibexUseToolFuture<'_>;

    /// The tool catalogue for one activation. The host answers from the live
    /// capability snapshot, so a target disabled after `tools/list` is rejected
    /// on the next call.
    fn tool_definitions(&self, actor: &VibexUseActor) -> Vec<VibexUseToolDefinition> {
        let _ = actor;
        Vec::new()
    }
}

// ---------------------------------------------------------------------------
// Task model
// ---------------------------------------------------------------------------

/// Whether a task closes when its first turn ends.
///
/// The default is the historical single-turn behaviour so a row persisted
/// before this field existed keeps its original meaning. New `vibex_delegate`
/// calls default to [`DelegationCompletionPolicy::OwnerReview`] explicitly.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelegationCompletionPolicy {
    /// Historical behaviour: the child's first idle state completes the task.
    #[default]
    SingleTurnLegacy,
    /// The child ending a turn only means the round awaits the owner's review.
    OwnerReview,
}

impl DelegationCompletionPolicy {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::SingleTurnLegacy => "single_turn_legacy",
            Self::OwnerReview => "owner_review",
        }
    }
}

/// Explicit domain phase of a delegated task.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelegationTaskPhase {
    #[default]
    Queued,
    Starting,
    Active,
    AwaitingReview,
    Cancelling,
    Completed,
    Failed,
    Cancelled,
}

impl DelegationTaskPhase {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }

    pub const fn is_active(self) -> bool {
        matches!(
            self,
            Self::Queued | Self::Starting | Self::Active | Self::AwaitingReview | Self::Cancelling
        )
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Starting => "starting",
            Self::Active => "active",
            Self::AwaitingReview => "awaiting_review",
            Self::Cancelling => "cancelling",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }
}

/// Who owns the child session a task runs in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DelegationOwnershipKind {
    /// The task created the session; the task tree may cascade to it.
    #[default]
    OwnedChild,
    /// The task was explicitly granted control of a pre-existing session. It
    /// never enters the creation tree and never cascades a delete.
    ControlledExisting,
}

/// What a task is waiting for. Blocking is not a terminal phase.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum DelegationBlockedOn {
    Permission { request_id: String },
    Question { request_id: String },
    PlanApproval { request_id: String },
    Configuration { reason: VibexUseUnavailableReason },
    Resource { reason: String },
}

impl DelegationBlockedOn {
    pub const fn request_id(&self) -> Option<&str> {
        match self {
            Self::Permission { request_id }
            | Self::Question { request_id }
            | Self::PlanApproval { request_id } => Some(request_id.as_str()),
            Self::Configuration { .. } | Self::Resource { .. } => None,
        }
    }

    pub const fn attention_kind(&self) -> &'static str {
        match self {
            Self::Permission { .. } => "permission",
            Self::Question { .. } => "question",
            Self::PlanApproval { .. } => "plan_approval",
            Self::Configuration { .. } => "configuration",
            Self::Resource { .. } => "resource",
        }
    }
}

/// One explicit context window handed to a child task.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationContextRef {
    pub session_ref: VibexUseRef,
    #[serde(default)]
    pub from_sequence: Option<i64>,
    #[serde(default)]
    pub through_sequence: Option<i64>,
    #[serde(default)]
    pub view: Option<SessionReadView>,
    /// How many characters of that window were actually included, so the child
    /// knows what it did and did not receive.
    #[serde(default)]
    pub included_chars: usize,
    #[serde(default)]
    pub truncated: bool,
}

/// The resolved runtime of one task, with nothing credential-bearing in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationRuntimeSummary {
    pub agent_id: AgentId,
    pub auth_kind: String,
    pub configuration_label: String,
    #[serde(default)]
    pub provider_profile_id: Option<ProviderProfileId>,
    #[serde(default)]
    pub model_id: Option<String>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    #[serde(default)]
    pub mode_id: Option<String>,
    #[serde(default)]
    pub config_values: BTreeMap<String, String>,
    #[serde(default)]
    pub catalog_revision: u64,
}

/// A fixed range of one session's timeline that carries a task result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExecutionResultRange {
    pub start_sequence: i64,
    pub end_sequence: i64,
}

/// How complete the usage numbers of one execution are.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionUsageState {
    Reported,
    Partial,
    #[default]
    Unknown,
}

/// One bounded artefact reference produced by an execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationResultRef {
    pub execution_ref: VibexUseRef,
    pub session_ref: VibexUseRef,
    pub outcome: ExecutionOutcome,
    #[serde(default)]
    pub stop_reason: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub result_ranges: Vec<ExecutionResultRange>,
    #[serde(default)]
    pub artifact_refs: Vec<VibexUseRef>,
    #[serde(default)]
    pub usage: ExecutionUsageState,
    #[serde(default)]
    pub truncated: bool,
}

/// Result classification of one execution. It is never collapsed into a
/// boolean "successful".
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionOutcome {
    /// The turn ended normally with a final reply.
    Completed,
    /// The turn ended normally but produced only tool activity.
    ActionsOnly,
    /// The turn ended with no assistant content at all.
    EmptyReply,
    Refusal,
    MaxTokens,
    AuthRequired,
    Cancelled,
    Failed,
    /// The prompt may have reached the provider; the durable outcome is unknown
    /// and is never auto-replayed.
    Ambiguous,
    Running,
    Queued,
}

impl ExecutionOutcome {
    pub const fn is_settled(self) -> bool {
        !matches!(self, Self::Running | Self::Queued)
    }
}

/// Durable, bounded provenance of one message that entered a session.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MessageProvenance {
    HumanInput,
    DelegatedInput {
        actor_session_ref: VibexUseRef,
        /// The task the message belongs to. A session an Agent created is not
        /// a task, so its first message carries no task reference.
        #[serde(default)]
        task_ref: Option<VibexUseRef>,
        operation_ref: VibexUseRef,
    },
    TeamResultInput {
        source_task_ref: VibexUseRef,
        source_execution_ref: VibexUseRef,
        event_id: String,
    },
    SystemContinuation {
        policy_id: String,
        trigger_event_id: String,
    },
    /// Historical rows whose origin cannot be proven. It is never guessed.
    #[default]
    LegacyUnknown,
}

impl MessageProvenance {
    /// The provenance of a message a person typed.
    ///
    /// It is the default for every historical caller of the send API: a caller
    /// that does not describe itself is a user-facing path, not an Agent.
    pub fn human_input() -> Self {
        Self::HumanInput
    }

    pub const fn kind(&self) -> &'static str {
        match self {
            Self::HumanInput => "human_input",
            Self::DelegatedInput { .. } => "delegated_input",
            Self::TeamResultInput { .. } => "team_result_input",
            Self::SystemContinuation { .. } => "system_continuation",
            Self::LegacyUnknown => "legacy_unknown",
        }
    }
}

// ---------------------------------------------------------------------------
// Execution model
// ---------------------------------------------------------------------------

/// One message actually dispatched into a session, with its fixed result range.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationExecution {
    pub id: VibexExecutionId,
    pub execution_ref: VibexUseRef,
    pub task_ref: Option<VibexUseRef>,
    pub session_ref: VibexUseRef,
    pub submission_id: MessageSubmissionId,
    pub input_idempotency_key: String,
    pub provenance: MessageProvenance,
    #[serde(default)]
    pub start_sequence: Option<i64>,
    #[serde(default)]
    pub end_sequence: Option<i64>,
    #[serde(default)]
    pub runtime_selection_revision: u64,
    pub outcome: ExecutionOutcome,
    #[serde(default)]
    pub stop_reason: Option<String>,
    /// Stable code of the failure that ended this execution, when one did.
    ///
    /// It is separate from `stop_reason`: a stop reason says *why* the turn
    /// ended, while this says which failure a caller should act on.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_code: Option<String>,
    #[serde(default)]
    pub summary: Option<String>,
    #[serde(default)]
    pub result_ranges: Vec<ExecutionResultRange>,
    #[serde(default)]
    pub artifact_refs: Vec<VibexUseRef>,
    #[serde(default)]
    pub usage: ExecutionUsageState,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub blocked_on: Option<DelegationBlockedOn>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub finished_at_ms: Option<i64>,
}

impl DelegationExecution {
    pub const fn is_settled(&self) -> bool {
        self.outcome.is_settled()
    }
}

// ---------------------------------------------------------------------------
// Operation model
// ---------------------------------------------------------------------------

/// Lifecycle of one accepted write request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum VibexUseOperationState {
    /// The authoritative service persisted the request but has not finished.
    #[default]
    Accepted,
    InProgress,
    Succeeded,
    Failed,
    /// The request crossed a boundary where its effect cannot be proven.
    Ambiguous,
}

impl VibexUseOperationState {
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Ambiguous)
    }
}

/// Durable record of one write request, keyed for idempotent retries.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseOperation {
    pub id: VibexOperationId,
    pub operation_ref: VibexUseRef,
    /// Authority that accepted the request.
    pub authority: String,
    /// The actor key the request was scoped to. It is derived from the
    /// capability the caller held, never from a tool argument.
    pub actor_key: String,
    pub tool: String,
    pub caller_key: String,
    pub payload_fingerprint: String,
    pub state: VibexUseOperationState,
    /// Stable, machine-readable failure reason.
    #[serde(default)]
    pub error_code: Option<String>,
    #[serde(default)]
    pub error_message: Option<String>,
    /// Whether repeating the identical request could still succeed.
    #[serde(default)]
    pub retryable: bool,
    /// Resources this operation already created. A retry never creates more.
    #[serde(default)]
    pub resources: Vec<VibexUseOperationResource>,
    #[serde(default)]
    pub checkpoint: BTreeMap<String, String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl VibexUseOperation {
    /// Key that scopes one idempotency key to a single authority and actor.
    pub fn authority_key(&self) -> String {
        format!("{}\u{1f}{}", self.authority, self.actor_key)
    }
}

/// One resource an operation produced, addressed by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseOperationResource {
    pub kind: String,
    pub reference: VibexUseRef,
}

// ---------------------------------------------------------------------------
// Task events and inbox
// ---------------------------------------------------------------------------

/// Typed task/team event kinds carried by the durable inbox.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DelegationTaskEventKind {
    TaskAccepted,
    TaskStarted,
    TaskBlocked,
    TaskUnblocked,
    ExecutionAccepted,
    ExecutionStarted,
    ExecutionFinished,
    ExecutionAmbiguous,
    TaskResultAvailable,
    TaskFinished,
    TaskCancelRequested,
    TaskCancelled,
    ControllerChanged,
    GroupMembershipChanged,
    PresentationApplied,
    PresentationDeferred,
}

impl DelegationTaskEventKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::TaskAccepted => "task_accepted",
            Self::TaskStarted => "task_started",
            Self::TaskBlocked => "task_blocked",
            Self::TaskUnblocked => "task_unblocked",
            Self::ExecutionAccepted => "execution_accepted",
            Self::ExecutionStarted => "execution_started",
            Self::ExecutionFinished => "execution_finished",
            Self::ExecutionAmbiguous => "execution_ambiguous",
            Self::TaskResultAvailable => "task_result_available",
            Self::TaskFinished => "task_finished",
            Self::TaskCancelRequested => "task_cancel_requested",
            Self::TaskCancelled => "task_cancelled",
            Self::ControllerChanged => "controller_changed",
            Self::GroupMembershipChanged => "group_membership_changed",
            Self::PresentationApplied => "presentation_applied",
            Self::PresentationDeferred => "presentation_deferred",
        }
    }

    /// Events that should wake a waiting parent even when no task reached a
    /// terminal phase.
    pub const fn is_attention(self) -> bool {
        matches!(
            self,
            Self::TaskBlocked | Self::ExecutionAmbiguous | Self::ControllerChanged
        )
    }

    /// Events that mean "there is something to read now".
    ///
    /// Acceptances are progress, not results: a caller woken by
    /// `task_accepted` learns nothing it did not already know when its own
    /// call returned.
    pub const fn settles_a_wait(self) -> bool {
        matches!(
            self,
            Self::ExecutionFinished
                | Self::TaskResultAvailable
                | Self::TaskFinished
                | Self::TaskCancelled
        )
    }
}

/// One durable event in the caller's inbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationTaskEvent {
    /// Monotonic per-authority cursor position.
    pub cursor: i64,
    pub event_id: String,
    pub kind: DelegationTaskEventKind,
    #[serde(default)]
    pub task_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub session_ref: Option<VibexUseRef>,
    /// Root of the team this event belongs to, so a client can route it
    /// without another lookup.
    #[serde(default)]
    pub root_session_ref: Option<VibexUseRef>,
    pub revision: u64,
    #[serde(default)]
    pub payload: Value,
    pub occurred_at_ms: i64,
    #[serde(default)]
    pub delivered: bool,
    #[serde(default)]
    pub acknowledged: bool,
}

// ---------------------------------------------------------------------------
// Session reads
// ---------------------------------------------------------------------------

/// Which slice of a session's product-visible content to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionReadView {
    /// Overview, latest conclusion, current blocking.
    Summary,
    /// Human and Agent inputs, visible replies, plans and major tool summaries.
    #[default]
    Conversation,
    /// Finer tool, file and permission/question events.
    Timeline,
}

impl SessionReadView {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Summary => "summary",
            Self::Conversation => "conversation",
            Self::Timeline => "timeline",
        }
    }
}

/// Exactly one page anchor. The legacy facade let `after` win when both were
/// supplied; Vibex-use rejects the ambiguous request instead.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "anchor", rename_all = "snake_case")]
pub enum SessionReadAnchor {
    #[default]
    Latest,
    After {
        sequence: i64,
    },
    Before {
        sequence: i64,
    },
}

/// A cursor is bound to the session and view that produced it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadCursor {
    pub session_ref: VibexUseRef,
    pub view: SessionReadView,
    pub anchor: SessionReadAnchor,
}

/// One bounded content entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadEntry {
    pub sequence: i64,
    pub kind: String,
    pub source: String,
    /// Already redacted by the product; provider logs and credentials never
    /// travel here.
    pub text: String,
    #[serde(default)]
    pub truncated: bool,
    #[serde(default)]
    pub artifact_refs: Vec<VibexUseRef>,
    pub timestamp_ms: i64,
}

/// One bounded page of session content.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionReadPage {
    pub session_ref: VibexUseRef,
    pub view: SessionReadView,
    pub items: Vec<SessionReadEntry>,
    pub snapshot_end_sequence: i64,
    #[serde(default)]
    pub next_cursor: Option<SessionReadCursor>,
    pub has_more: bool,
    pub truncated: bool,
    /// What was left out and how to continue reading it.
    #[serde(default)]
    pub notices: Vec<String>,
}

// ---------------------------------------------------------------------------
// Session summary
// ---------------------------------------------------------------------------

/// Read-only projection of one session inside the caller's scope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseSessionSummary {
    pub session_ref: VibexUseRef,
    pub title: String,
    pub state: String,
    pub scope: VibexUseScope,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub workspace_ref: VibexUseRef,
    pub project_id: String,
    #[serde(default)]
    pub parent_session_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub child_count: usize,
    #[serde(default)]
    pub has_more_children: bool,
    #[serde(default)]
    pub current_task_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub current_execution_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub blocked_on: Option<DelegationBlockedOn>,
    #[serde(default)]
    pub attention_count: usize,
    /// Whether this session was created by a delegated task rather than by the
    /// user. The sidebar tree is built from this, never from heuristics.
    pub created_by_delegation: bool,
    #[serde(default)]
    pub archived: bool,
    pub updated_at_ms: i64,
}

/// One entry of a session list page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionListPage {
    pub sessions: Vec<VibexUseSessionSummary>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

/// Task projection returned by `vibex_get_tasks` and `vibex_delegate`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationTaskView {
    pub task_ref: VibexUseRef,
    pub session_ref: Option<VibexUseRef>,
    pub parent_session_ref: VibexUseRef,
    pub root_session_ref: VibexUseRef,
    pub title: String,
    #[serde(default)]
    pub task_summary: String,
    pub phase: DelegationTaskPhase,
    /// Compatibility projection the legacy tool result still reports.
    pub legacy_status: String,
    pub ownership_kind: DelegationOwnershipKind,
    pub completion_policy: DelegationCompletionPolicy,
    #[serde(default)]
    pub parent_task_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub follows_task_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub blocked_on: Option<DelegationBlockedOn>,
    #[serde(default)]
    pub current_execution_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub result_refs: Vec<DelegationResultRef>,
    #[serde(default)]
    pub acceptance_criteria: Vec<String>,
    #[serde(default)]
    pub requested_runtime: Option<DelegationRuntimeSummary>,
    #[serde(default)]
    pub effective_runtime: Option<DelegationRuntimeSummary>,
    #[serde(default)]
    pub error_code: Option<String>,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    #[serde(default)]
    pub completed_at_ms: Option<i64>,
    #[serde(default)]
    pub cancellation_requested_at_ms: Option<i64>,
}

/// One task page.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TaskListPage {
    pub tasks: Vec<DelegationTaskView>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Wait
// ---------------------------------------------------------------------------

/// Why a wait returned.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WaitOutcome {
    /// The timeout elapsed with no new event. Not a failure.
    TimedOut,
    /// At least one watcher reached a terminal phase.
    Settled,
    /// A new attention request or an ambiguous dispatch appeared.
    Attention,
    /// A referenced resource failed or disappeared.
    Failed,
}

/// One `vibex_wait` response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WaitResponse {
    pub outcome: WaitOutcome,
    /// Events the caller has not seen yet, ordered by cursor.
    #[serde(default)]
    pub events: Vec<DelegationTaskEvent>,
    #[serde(default)]
    pub tasks: Vec<DelegationTaskView>,
    pub event_cursor: i64,
    /// Whether a further wait with the same cursor is worth attempting.
    pub more_expected: bool,
}

// ---------------------------------------------------------------------------
// Discovery
// ---------------------------------------------------------------------------

/// The complete `vibex_discover` payload.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoverResponse {
    pub capability: VibexUseCapabilitySnapshot,
    #[serde(default)]
    pub runtime_options: Vec<VibexUseRuntimeOption>,
    #[serde(default)]
    pub workspaces: Vec<VibexUseWorkspaceOption>,
    #[serde(default)]
    pub groups: Vec<VibexUseGroupSummary>,
    /// Stable guidance for the model. It is documentation, not a policy engine.
    #[serde(default)]
    pub guidance: Vec<String>,
    #[serde(default)]
    pub next_cursor: Option<String>,
    #[serde(default)]
    pub has_more: bool,
}

// ---------------------------------------------------------------------------
// Groups and presentation
// ---------------------------------------------------------------------------

/// Layout presets an Agent may ask for. They are intentions, not coordinates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum SessionGroupLayoutPreset {
    Single,
    Columns,
    #[default]
    LeadAndWorkers,
    Grid,
    Tabs,
}

impl SessionGroupLayoutPreset {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Single => "single",
            Self::Columns => "columns",
            Self::LeadAndWorkers => "lead_and_workers",
            Self::Grid => "grid",
            Self::Tabs => "tabs",
        }
    }
}

/// The typed layout intention attached to a group request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct SessionGroupLayoutIntent {
    #[serde(default)]
    pub preset: SessionGroupLayoutPreset,
    #[serde(default)]
    pub lead_session_ref: Option<VibexUseRef>,
    /// How many panes should hold a live conversation. The rest keep their
    /// place as tabs and load history on demand.
    #[serde(default)]
    pub preferred_live_panes: Option<usize>,
}

/// Scope of one group. The first version only supports one workspace.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum SessionGroupScope {
    /// Every member shares one Worktree.
    Workspace { workspace_ref: VibexUseRef },
    /// Reserved for a later cross-Worktree contract. Requests naming it are
    /// rejected with `cross_workspace_unsupported` instead of silently losing
    /// members.
    ProjectTeam { project_id: String },
}

/// Read model of one group.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct VibexUseGroupSummary {
    pub group_ref: VibexUseRef,
    pub name: String,
    pub revision: u64,
    pub member_session_refs: Vec<VibexUseRef>,
    pub workspace_ref: VibexUseRef,
    #[serde(default)]
    pub layout: SessionGroupLayoutIntent,
    /// Whether the caller created the group through Vibex-use and may still
    /// change it.
    pub owned_by_caller: bool,
    /// Whether the connected client can actually show it.
    pub presentable: bool,
    #[serde(default)]
    pub presentation_reason: Option<VibexUseUnavailableReason>,
    /// Set once a client applied the layout.
    #[serde(default)]
    pub applied: bool,
    #[serde(default)]
    pub presented: bool,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

/// How eagerly a client may switch the user's workspace to show a group.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum PresentationActivationPolicy {
    /// Apply only when the user is already looking at this team's workspace.
    #[default]
    IfCurrentTeam,
    /// Wait until the user comes back to the team.
    WhenUserReturns,
}

/// The staged result of one presentation request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresentationState {
    /// The group definition was accepted and stored by its owner.
    Created,
    /// A display intent exists but no client confirmed applying it.
    Prepared,
    /// A client adopted the layout.
    Applied,
    /// A client made the group the visible workspace.
    Presented,
    /// The user is elsewhere, or a revision conflict held the change back.
    Deferred,
    /// This runtime has no client that can show groups.
    Unavailable,
}

/// The concrete shape a client actually rendered.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
#[serde(rename_all = "camelCase")]
pub struct AppliedGroupLayout {
    pub preset: SessionGroupLayoutPreset,
    /// Sessions that received a live pane.
    #[serde(default)]
    pub visible_session_refs: Vec<VibexUseRef>,
    /// Sessions stacked as tabs behind a live pane.
    #[serde(default)]
    pub tabbed_session_refs: Vec<VibexUseRef>,
    pub live_panes: usize,
}

/// The result of `vibex_present_group`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PresentationOutcome {
    pub group_ref: VibexUseRef,
    pub state: PresentationState,
    #[serde(default)]
    pub reason: Option<VibexUseUnavailableReason>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub applied_layout: Option<AppliedGroupLayout>,
    pub revision: u64,
}

/// One request from the runtime to a connected shell about a group.
///
/// The runtime owns the intent and the stable group id; the shell owns the
/// live layout. Sending the id in the request — instead of letting the shell
/// mint one — is what makes a retry after a lost reply resolve to the same
/// group rather than drawing a second copy of the team.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPresentationCommand {
    pub group_id: crate::SessionGroupId,
    #[serde(default)]
    pub operation_id: Option<crate::VibexOperationId>,
    pub name: String,
    pub workspace_ref: VibexUseRef,
    pub member_session_refs: Vec<VibexUseRef>,
    #[serde(default)]
    pub layout: SessionGroupLayoutIntent,
    #[serde(default)]
    pub expected_revision: Option<u64>,
    #[serde(default)]
    pub activation_policy: PresentationActivationPolicy,
    #[serde(default)]
    pub focus_session_ref: Option<VibexUseRef>,
    /// Set when the caller only wants the group definition stored or updated
    /// and does not want the user's workspace switched.
    #[serde(default)]
    pub present: bool,
    /// Set when the caller only wants a client to *show* an existing group.
    ///
    /// A presenting retry must not restate the definition: re-sending the
    /// original members and layout would silently undo whatever the user
    /// rearranged in between.
    #[serde(default)]
    pub presentation_only: bool,
}

/// What a shell actually did with one [`GroupPresentationCommand`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GroupPresentationReply {
    pub state: PresentationState,
    pub revision: u64,
    #[serde(default)]
    pub reason: Option<VibexUseUnavailableReason>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub applied_layout: Option<AppliedGroupLayout>,
}

impl GroupPresentationReply {
    pub fn unavailable(reason: VibexUseUnavailableReason) -> Self {
        Self {
            state: PresentationState::Unavailable,
            revision: 0,
            reason: Some(reason),
            message: None,
            applied_layout: None,
        }
    }
}

/// One node of the user-facing ownership tree.
///
/// The tree is built from persisted ownership edges and the task that created
/// each child. It is never inferred from titles, message counts or the shape of
/// an already-loaded timeline.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SessionTreeNode {
    pub session_ref: VibexUseRef,
    pub parent_session_ref: Option<VibexUseRef>,
    pub title: String,
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    #[serde(default)]
    pub agent_label: Option<String>,
    #[serde(default)]
    pub task_ref: Option<VibexUseRef>,
    #[serde(default)]
    pub task_title: Option<String>,
    #[serde(default)]
    pub task_phase: Option<DelegationTaskPhase>,
    #[serde(default)]
    pub completion_policy: Option<DelegationCompletionPolicy>,
    /// Direct children, so a collapsed node can still show that it has more.
    pub child_count: usize,
    /// Whether the child list is longer than the page the caller received.
    pub has_more_children: bool,
    #[serde(default)]
    pub blocked_on: Option<DelegationBlockedOn>,
    /// Running executions anywhere in this node's subtree.
    pub active_descendants: usize,
    /// Pending attention requests anywhere in this node's subtree.
    pub blocked_descendants: usize,
    /// Task the child session is currently working on, when one is active.
    #[serde(default)]
    pub current_task_ref: Option<VibexUseRef>,
    pub updated_at_ms: i64,
}

// ---------------------------------------------------------------------------
// Tool responses
// ---------------------------------------------------------------------------

/// Acceptance response of `vibex_delegate` and `vibex_create_session`.
///
/// `accepted` means the authoritative service persisted the request. It does
/// not mean the model started generating, and it never means the task is done.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DelegationAccepted {
    pub status: &'static str,
    pub task_ref: Option<VibexUseRef>,
    pub session_ref: Option<VibexUseRef>,
    pub operation_ref: VibexUseRef,
    #[serde(default)]
    pub execution_ref: Option<VibexUseRef>,
    pub phase: DelegationTaskPhase,
    #[serde(default)]
    pub effective_target: Option<DelegationRuntimeSummary>,
    #[serde(default)]
    pub presentation: Option<PresentationOutcome>,
    /// A group or presentation failure is reported here without pretending the
    /// task itself failed.
    #[serde(default)]
    pub warnings: Vec<String>,
    #[serde(default)]
    pub context: Vec<DelegationContextRef>,
}

impl DelegationAccepted {
    pub const ACCEPTED: &'static str = "accepted";
}

/// Acceptance response of `vibex_send_message`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAccepted {
    pub status: &'static str,
    pub session_ref: VibexUseRef,
    pub submission_id: MessageSubmissionId,
    pub operation_ref: VibexUseRef,
    pub execution_ref: VibexUseRef,
    /// Where the message sits in this session's durable queue.
    pub queued: bool,
    pub queue_position: usize,
    pub provenance: MessageProvenance,
}

impl MessageAccepted {
    pub const ENQUEUED: &'static str = "enqueued";
}

// ---------------------------------------------------------------------------
// Stable error codes
// ---------------------------------------------------------------------------

/// Stable error codes returned to an Agent. Each one tells the caller what to
/// do next instead of only that something failed.
pub mod codes {
    /// The caller's catalog revision is older than the live one.
    pub const CATALOG_STALE: &str = "catalog_stale";
    /// `expected_revision` did not match the stored revision.
    pub const REVISION_CONFLICT: &str = "session_revision_conflict";
    /// The target Agent is disabled or otherwise not startable.
    pub const AGENT_DISABLED: &str = "agent_disabled";
    /// The target Agent or runtime option cannot be used right now.
    pub const TARGET_UNAVAILABLE: &str = "target_unavailable";
    /// This runtime cannot offer the requested capability.
    pub const CAPABILITY_UNAVAILABLE: &str = "capability_unavailable";
    /// The caller may not act on this resource.
    pub const SCOPE_DENIED: &str = "scope_denied";
    /// The resource is invisible, so its existence is not disclosed.
    pub const NOT_FOUND_OR_NOT_AUTHORIZED: &str = "not_found_or_not_authorized";
    /// The same idempotency key was reused with a different payload.
    pub const IDEMPOTENCY_PAYLOAD_CONFLICT: &str = "idempotency_payload_conflict";
    /// The session's controller changed since the caller last read it.
    pub const CONTROLLER_CHANGED: &str = "controller_changed";
    /// A previous prompt dispatch may have reached the provider.
    pub const DISPATCH_AMBIGUOUS: &str = "message_submission_prompt_dispatch_ambiguous";
    /// No client can present a group.
    pub const PRESENTATION_UNAVAILABLE: &str = "presentation_unavailable";
    /// The group layout changed after the caller read it.
    pub const PRESENTATION_LAYOUT_CONFLICT: &str = "presentation_layout_conflict";
    /// The task is already in a terminal phase.
    pub const TASK_TERMINAL: &str = "task_terminal";
    /// A cancel has been accepted for this task or one of its ancestors, so the
    /// subtree stops accepting new messages and new children until the stop is
    /// confirmed.
    pub const TASK_CANCELLING: &str = "task_cancelling";
    /// The root session has used every execution slot.
    pub const BUDGET_EXHAUSTED: &str = "root_execution_budget_exhausted";
    /// The caller may not delegate any deeper.
    pub const DEPTH_EXCEEDED: &str = "delegation_depth_exceeded";
    /// The requested read anchor combination is ambiguous.
    pub const READ_ANCHOR_AMBIGUOUS: &str = "read_anchor_ambiguous";
    /// A cursor was reused against another session or view.
    pub const CURSOR_SCOPE_MISMATCH: &str = "cursor_scope_mismatch";
    /// The request is malformed.
    pub const REQUEST_INVALID: &str = "vibex_use_request_invalid";
}

// ---------------------------------------------------------------------------
// Parsing helpers
// ---------------------------------------------------------------------------

/// Bounds a string on a Unicode character boundary, appending an ellipsis when
/// it had to cut. Byte budgets are never called token budgets.
pub fn bounded_chars(value: &str, limit: usize) -> (String, bool) {
    if value.chars().count() <= limit {
        return (value.to_string(), false);
    }
    let mut output: String = value.chars().take(limit.saturating_sub(1)).collect();
    output.push('…');
    (output, true)
}

/// Parses one resource reference from a tool argument.
pub fn parse_ref(
    arguments: &Value,
    key: &str,
    expected: VibexUseResourceKind,
) -> Result<VibexUseRef, crate::VibexError> {
    let raw = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            crate::VibexError::validation(
                codes::REQUEST_INVALID,
                format!("{key} is required and must be a vibex resource reference"),
            )
        })?;
    let reference = VibexUseRef::parse(raw).ok_or_else(|| {
        crate::VibexError::validation(
            codes::REQUEST_INVALID,
            format!("{key} is not a valid vibex resource reference"),
        )
    })?;
    if reference.kind != expected {
        return Err(crate::VibexError::validation(
            codes::REQUEST_INVALID,
            format!(
                "{key} must reference a {}, not a {}",
                expected.as_str(),
                reference.kind.as_str()
            ),
        ));
    }
    Ok(reference)
}

/// Parses an optional resource reference of an expected kind.
pub fn parse_optional_ref(
    arguments: &Value,
    key: &str,
    expected: VibexUseResourceKind,
) -> Result<Option<VibexUseRef>, crate::VibexError> {
    match arguments.get(key).and_then(Value::as_str) {
        Some(value) if !value.trim().is_empty() => parse_ref(arguments, key, expected).map(Some),
        _ => Ok(None),
    }
}

/// Reads a required bounded non-empty string argument.
pub fn required_string(
    arguments: &Value,
    key: &str,
    limit: usize,
) -> Result<String, crate::VibexError> {
    let value = arguments
        .get(key)
        .and_then(Value::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty())
        .ok_or_else(|| {
            crate::VibexError::validation(codes::REQUEST_INVALID, format!("{key} is required"))
        })?;
    if value.chars().count() > limit {
        return Err(crate::VibexError::validation(
            codes::REQUEST_INVALID,
            format!("{key} exceeds {limit} characters"),
        ));
    }
    Ok(value.to_string())
}

/// Reads an optional bounded string argument.
pub fn optional_string(arguments: &Value, key: &str, limit: usize) -> Option<String> {
    let value = arguments.get(key).and_then(Value::as_str)?.trim();
    if value.is_empty() {
        return None;
    }
    Some(value.chars().take(limit).collect())
}

/// Reads one JSON pointer with a default.
pub fn optional_bool(arguments: &Value, key: &str, default: bool) -> bool {
    arguments
        .get(key)
        .and_then(Value::as_bool)
        .unwrap_or(default)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn a_mention_carries_its_typed_identity_through_a_token_id() {
        let agent_id = AgentId::parse("codex").unwrap();
        let option = VibexUseRef::runtime_option("option_a");
        let mention = VibexUseMention::agent(&agent_id, &option, Some("@Codex"));
        let encoded = mention.encode();
        let parsed = VibexUseMention::parse(&encoded).unwrap();
        // The identity that travels is the Agent and the configuration, not the
        // label the reader saw.
        assert_eq!(parsed.kind, VibexUseMentionKind::Agent);
        assert_eq!(parsed.agent_id, Some(agent_id));
        assert_eq!(parsed.reference, option);

        let session = VibexSessionId::new();
        let session_mention = VibexUseMention::session(&session, Some("@Review"));
        let parsed = VibexUseMention::parse(&session_mention.encode()).unwrap();
        assert_eq!(parsed.kind, VibexUseMentionKind::Session);
        assert_eq!(parsed.reference.session_id(), Some(session));
        assert_eq!(parsed.agent_id, None);

        // A file reference and a provider command are not mentions, and an
        // unrelated string must never be mistaken for one.
        assert!(VibexUseMention::parse("reference:file:src/main.rs").is_none());
        assert!(VibexUseMention::parse("vibex-use:agent/").is_none());
        assert!(VibexUseMention::parse("vibex-use:unknown/thing").is_none());
    }

    #[test]
    fn the_route_note_names_every_reference_once_and_claims_no_authority() {
        let first = VibexUseMention::agent(
            &AgentId::parse("codex").unwrap(),
            &VibexUseRef::runtime_option("option_a"),
            Some("@Codex"),
        );
        let duplicate = first.clone();
        let session = VibexUseMention::session(&VibexSessionId::new(), None);
        let note = vibex_use_route_note(&[first, duplicate, session]).unwrap();
        assert_eq!(note.matches("vibex://runtime-option/option_a").count(), 1);
        assert!(note.contains("vibex://session/"));
        assert!(note.contains("not an authorization"));
        assert!(vibex_use_route_note(&[]).is_none());
    }

    #[test]
    fn resource_references_round_trip_through_their_uri() {
        let session = VibexSessionId::new();
        let reference = VibexUseRef::session(&session);
        assert_eq!(reference.as_uri(), format!("vibex://session/{session}"));
        assert_eq!(
            VibexUseRef::parse(&reference.as_uri()),
            Some(reference.clone())
        );
        assert_eq!(
            VibexUseRef::parse(&format!("vibex:session/{}", session.as_str())),
            Some(reference.clone())
        );
        assert_eq!(reference.session_id(), Some(session));
        assert!(VibexUseRef::parse("vibex://session/").is_none());
        assert!(VibexUseRef::parse("https://example.com").is_none());
        assert!(VibexUseRef::parse("vibex://unknown/abc").is_none());
    }

    #[test]
    fn task_references_resolve_to_a_delegation_id() {
        let task = AgentDelegationId::new();
        let reference = VibexUseRef::task(&task);
        assert!(reference.as_uri().starts_with("vibex://task/delegation_"));
        assert_eq!(reference.task_id(), Some(task.clone()));
        assert_eq!(reference.session_id(), None);
    }

    #[test]
    fn references_serialize_as_plain_uris() {
        let reference = VibexUseRef::session(&VibexSessionId::new());
        let encoded = serde_json::to_string(&reference).unwrap();
        assert_eq!(encoded, format!("\"{}\"", reference.as_uri()));
        let decoded: VibexUseRef = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, reference);
    }

    #[test]
    fn the_catalogue_omits_tools_that_cannot_succeed() {
        let capability = VibexUseCapabilitySnapshot {
            delivery: VibexUseDelivery::Mcp,
            delivery_reason: None,
            authority: "authority".to_string(),
            caller_session_ref: VibexUseRef::session(&VibexSessionId::new()),
            root_session_ref: VibexUseRef::session(&VibexSessionId::new()),
            current_task_ref: None,
            current_workspace_ref: VibexUseRef::workspace("workspace_a"),
            catalog_revision: 3,
            activation_revision: 1,
            remaining_executions: 4,
            max_depth: VIBEX_USE_MAX_DEPTH,
            remaining_depth: 1,
            can_delegate: true,
            delegation_blocked_by: None,
            can_read_any_session: false,
            presentation: VibexUsePresentationCapability::default(),
            available_tools: Vec::new(),
        };
        let tools = vibex_use_tool_definitions(&capability);
        let names: Vec<&str> = tools.iter().map(|tool| tool.name.as_str()).collect();
        assert!(names.contains(&"vibex_delegate"));
        assert!(names.contains(&"vibex_list_groups"));
        assert!(!names.contains(&"vibex_create_group"));
        assert!(!names.contains(&"vibex_present_group"));

        let mut budget_exhausted = capability.clone();
        budget_exhausted.can_delegate = false;
        let names: Vec<String> = vibex_use_tool_definitions(&budget_exhausted)
            .into_iter()
            .map(|tool| tool.name)
            .collect();
        assert!(!names.contains(&"vibex_delegate".to_string()));
        assert!(!names.contains(&"vibex_create_session".to_string()));
        assert!(names.contains(&"vibex_send_message".to_string()));
    }

    #[test]
    fn every_catalogue_tool_has_a_schema_and_a_unique_name() {
        let mut seen = std::collections::BTreeSet::new();
        for tool in VibexUseTool::ALL {
            assert!(seen.insert(tool.name()), "duplicate tool {}", tool.name());
            assert_eq!(VibexUseTool::parse(tool.name()), Some(tool));
            let schema = tool_input_schema(tool);
            assert_eq!(schema["type"], "object");
            assert!(!tool_description(tool).is_empty());
            assert!(
                !schema["properties"].is_null(),
                "{} has no properties",
                tool.name()
            );
        }
        assert_eq!(seen.len(), VibexUseTool::ALL.len());
    }

    #[test]
    fn read_anchors_are_exclusive_in_their_serialized_shape() {
        let latest = serde_json::to_value(SessionReadAnchor::Latest).unwrap();
        assert_eq!(latest, json!({ "anchor": "latest" }));
        let after = serde_json::to_value(SessionReadAnchor::After { sequence: 4 }).unwrap();
        assert_eq!(after, json!({ "anchor": "after", "sequence": 4 }));
    }

    #[test]
    fn bounded_text_cuts_on_a_character_boundary() {
        let (value, truncated) = bounded_chars("存储事务审查报告", 4);
        assert!(truncated);
        assert_eq!(value.chars().count(), 4);
        let (value, truncated) = bounded_chars("short", 10);
        assert!(!truncated);
        assert_eq!(value, "short");
    }

    #[test]
    fn reference_parsing_rejects_the_wrong_kind() {
        let arguments = json!({ "sessionRef": "vibex://task/delegation_abc" });
        let error = parse_ref(&arguments, "sessionRef", VibexUseResourceKind::Session)
            .expect_err("a task reference is not a session reference");
        assert_eq!(error.code, codes::REQUEST_INVALID);
    }

    #[test]
    fn provenance_never_guesses_a_historical_source() {
        assert_eq!(MessageProvenance::default().kind(), "legacy_unknown");
    }

    #[test]
    fn task_phases_separate_active_from_terminal() {
        assert!(DelegationTaskPhase::AwaitingReview.is_active());
        assert!(!DelegationTaskPhase::AwaitingReview.is_terminal());
        assert!(DelegationTaskPhase::Completed.is_terminal());
        assert!(DelegationTaskPhase::Cancelling.is_active());
    }

    #[test]
    fn execution_outcomes_distinguish_actions_only_from_a_reply() {
        assert!(ExecutionOutcome::Completed.is_settled());
        assert!(!ExecutionOutcome::Running.is_settled());
        assert_ne!(ExecutionOutcome::ActionsOnly, ExecutionOutcome::Completed);
    }
}

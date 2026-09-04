use std::fmt;

use serde::{Deserialize, Serialize};

use crate::agent_config::AgentId;
use crate::ids::{
    CorrelationId, MessageSubmissionId, ProjectId, PromptId, ProviderProfileId, RequestId,
    RuntimeSwitchId, SkillId, TimelineItemId, VibexSessionId, WorkspaceId,
};
use crate::permission::{PermissionMode, PermissionResolution};
use crate::provider::{
    ProviderBindingMetadata, ProviderCapabilities, ProviderKind, ProviderSessionConfigOption,
    ProviderSessionConfigValue,
};
use crate::runtime::{MessageSubmissionStatus, SessionRuntimeSelection};
use crate::timeline::{MessageAttachment, TimelineItem, TimelinePage};
use crate::workspace::WorkspaceMode;

pub const MAX_MESSAGE_IDEMPOTENCY_KEY_LEN: usize = 256;
pub const MAX_AGENT_SESSION_TITLE_CHARS: usize = 120;
pub const AGENT_ATTENTION_NOTIFICATION_TTL_MS: i64 = 15 * 60 * 1000;
pub const AGENT_TERMINAL_NOTIFICATION_TTL_MS: i64 = 24 * 60 * 60 * 1000;

/// Normalize a user- or Agent-supplied session title at the shared contract
/// boundary. Titles are single-line display labels and must remain bounded
/// before they enter persistence or a client projection.
pub fn normalize_agent_session_title(value: &str) -> Option<String> {
    let normalized = value.split_whitespace().collect::<Vec<_>>().join(" ");
    if normalized.is_empty() {
        return None;
    }
    Some(
        normalized
            .chars()
            .take(MAX_AGENT_SESSION_TITLE_CHARS)
            .collect(),
    )
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSessionState {
    Initializing,
    Idle,
    Running,
    NeedsInput,
    Error,
    Closed,
    Archived,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(
    tag = "type",
    content = "data",
    rename_all = "snake_case",
    rename_all_fields = "camelCase"
)]
pub enum AgentNotificationKind {
    ApprovalRequired { request_id: RequestId },
    InputRequired { request_id: RequestId },
    TurnCompleted,
    TurnFailed,
}

/// Privacy-bounded notification produced by the authoritative Agent runtime.
/// The locator is routing metadata only and must be resolved through an
/// authenticated backend before a client displays session details.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentNotificationIntent {
    pub notification_id: String,
    pub source_event_id: TimelineItemId,
    pub session_id: VibexSessionId,
    pub kind: AgentNotificationKind,
    pub created_at_ms: i64,
    pub expires_at_ms: i64,
    pub opaque_locator: String,
}

impl AgentNotificationIntent {
    pub fn approval_required(item: &TimelineItem, request_id: RequestId) -> Self {
        Self::new(
            format!(
                "approval.{}.{}",
                item.session_id.as_str(),
                request_id.as_str()
            ),
            item,
            AgentNotificationKind::ApprovalRequired { request_id },
            AGENT_ATTENTION_NOTIFICATION_TTL_MS,
        )
    }

    pub fn input_required(item: &TimelineItem, request_id: RequestId) -> Self {
        Self::new(
            format!("input.{}.{}", item.session_id.as_str(), request_id.as_str()),
            item,
            AgentNotificationKind::InputRequired { request_id },
            AGENT_ATTENTION_NOTIFICATION_TTL_MS,
        )
    }

    pub fn turn_completed(item: &TimelineItem) -> Self {
        Self::new(
            format!(
                "turn-completed.{}.{}",
                item.session_id.as_str(),
                item.id.as_str()
            ),
            item,
            AgentNotificationKind::TurnCompleted,
            AGENT_TERMINAL_NOTIFICATION_TTL_MS,
        )
    }

    pub fn turn_failed(item: &TimelineItem) -> Self {
        Self::new(
            format!(
                "turn-failed.{}.{}",
                item.session_id.as_str(),
                item.id.as_str()
            ),
            item,
            AgentNotificationKind::TurnFailed,
            AGENT_TERMINAL_NOTIFICATION_TTL_MS,
        )
    }

    fn new(
        notification_id: String,
        item: &TimelineItem,
        kind: AgentNotificationKind,
        ttl_ms: i64,
    ) -> Self {
        Self {
            notification_id,
            source_event_id: item.id.clone(),
            session_id: item.session_id.clone(),
            kind,
            created_at_ms: item.timestamp_ms,
            expires_at_ms: item.timestamp_ms.saturating_add(ttl_ms),
            opaque_locator: item.session_id.as_str().to_string(),
        }
    }
}

pub fn agent_session_turn_requires_continuation(
    state: AgentSessionState,
    latest_turn_ended_normally: Option<bool>,
) -> bool {
    match state {
        AgentSessionState::Error => latest_turn_ended_normally != Some(true),
        AgentSessionState::Idle => latest_turn_ended_normally == Some(false),
        AgentSessionState::Initializing
        | AgentSessionState::Running
        | AgentSessionState::NeedsInput
        | AgentSessionState::Closed
        | AgentSessionState::Archived => false,
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSafety {
    pub permission_mode: PermissionMode,
    pub ask_on_risk: bool,
    pub bypass_all_permissions: bool,
}

impl AgentSessionSafety {
    pub const fn workspace_write_ask_on_risk() -> Self {
        Self {
            permission_mode: PermissionMode::WorkspaceWrite,
            ask_on_risk: true,
            bypass_all_permissions: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSession {
    pub id: VibexSessionId,
    pub title: String,
    pub project_id: ProjectId,
    pub workspace_id: WorkspaceId,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    /// Current Agent identity. Runtime Profile/Model state is exposed through
    /// `AgentSessionRuntimeSelectionState`, not duplicated on the session DTO.
    pub agent_id: AgentId,
    pub state: AgentSessionState,
    pub safety: AgentSessionSafety,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    /// Timestamp of the latest persisted timeline item. This is independent
    /// from `updated_at_ms`, which also changes for session state and metadata.
    #[serde(default)]
    pub last_message_at_ms: i64,
    pub archived_at_ms: Option<i64>,
    pub deleted_at_ms: Option<i64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CreateAgentSessionRequest {
    pub runtime: SessionRuntimeSelection,
    pub workspace_root: String,
    pub workspace_mode: WorkspaceMode,
    pub title: Option<String>,
    pub safety: Option<AgentSessionSafety>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ForkAgentSessionRequest {
    pub source_session_id: VibexSessionId,
    pub through_sequence: i64,
    pub expected_source_end_sequence: Option<i64>,
}

/// Shared session timeline presentation preferences.  The desktop remains the
/// authority for the inherited values; mobile may persist a local override.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTimelineReasoningDisplayMode {
    #[default]
    LatestAtBottom,
    Timeline,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentTimelineDisplaySettings {
    #[serde(default = "default_show_agent_generation_status")]
    pub show_agent_generation_status: bool,
    #[serde(default)]
    pub reasoning_display_mode: AgentTimelineReasoningDisplayMode,
    #[serde(default)]
    pub reasoning_expanded_by_default: bool,
    #[serde(default)]
    pub enhanced_command_execution_display: bool,
    #[serde(default = "default_enhanced_file_operation_display")]
    pub enhanced_file_operation_display: bool,
}

impl Default for AgentTimelineDisplaySettings {
    fn default() -> Self {
        Self {
            show_agent_generation_status: true,
            reasoning_display_mode: AgentTimelineReasoningDisplayMode::LatestAtBottom,
            reasoning_expanded_by_default: false,
            enhanced_command_execution_display: false,
            enhanced_file_operation_display: true,
        }
    }
}

const fn default_show_agent_generation_status() -> bool {
    true
}

const fn default_enhanced_file_operation_display() -> bool {
    true
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct RenameAgentSessionRequest {
    pub session_id: VibexSessionId,
    pub title: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ContinueAgentTurnRequest {
    pub session_id: VibexSessionId,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentModelListSource {
    Probed,
    Configured,
    Unavailable,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelListRequest {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub session_id: Option<VibexSessionId>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentReasoningEffort {
    pub value: String,
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelCapabilities {
    pub model: String,
    pub default_reasoning_effort: Option<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentModelListResponse {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub provider_kind: ProviderKind,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub models: Vec<String>,
    #[serde(default)]
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    #[serde(default)]
    pub model_capabilities: Vec<AgentModelCapabilities>,
    pub source: AgentModelListSource,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

/// Session-level configuration evidence discovered through a stateless
/// provider probe (e.g. an ACP `session/new` handshake). Values are
/// product-safe: raw payloads and native session ids never cross this
/// boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionConfigProbe {
    #[serde(default)]
    pub models: Vec<String>,
    #[serde(default)]
    pub modes: Vec<ProviderSessionConfigValue>,
    #[serde(default)]
    pub reasoning_efforts: Vec<AgentReasoningEffort>,
    #[serde(default)]
    pub options: Vec<ProviderSessionConfigOption>,
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendAgentMessageRequest {
    pub session_id: VibexSessionId,
    pub message_idempotency_key: String,
    pub desired_runtime: SessionRuntimeSelection,
    pub text: String,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub correlation_id: Option<CorrelationId>,
}

impl fmt::Debug for SendAgentMessageRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SendAgentMessageRequest")
            .field("session_id", &self.session_id)
            .field(
                "has_message_idempotency_key",
                &!self.message_idempotency_key.is_empty(),
            )
            .field("desired_runtime", &self.desired_runtime)
            .field("has_text", &!self.text.is_empty())
            .field("attachment_count", &self.attachments.len())
            .field("reasoning_effort", &self.reasoning_effort)
            .field("correlation_id", &self.correlation_id)
            .finish()
    }
}

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GetMessageSubmissionRequest {
    pub session_id: VibexSessionId,
    pub message_idempotency_key: String,
}

impl fmt::Debug for GetMessageSubmissionRequest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("GetMessageSubmissionRequest")
            .field("session_id", &self.session_id)
            .field(
                "has_message_idempotency_key",
                &!self.message_idempotency_key.is_empty(),
            )
            .finish()
    }
}

/// Product-safe projection of a durable message submission. Payload and
/// provider dispatch internals deliberately stay behind the repository API.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageSubmissionState {
    pub submission_id: MessageSubmissionId,
    pub session_id: VibexSessionId,
    pub message_idempotency_key: String,
    pub submission_sequence: i64,
    pub desired_runtime: SessionRuntimeSelection,
    pub required_switch_id: Option<RuntimeSwitchId>,
    pub status: MessageSubmissionStatus,
    pub user_message_timeline_item_id: Option<TimelineItemId>,
    pub error_code: Option<String>,
    pub error_detail_redacted: Option<String>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub dispatched_at_ms: Option<i64>,
}

impl fmt::Debug for MessageSubmissionState {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MessageSubmissionState")
            .field("submission_id", &self.submission_id)
            .field("session_id", &self.session_id)
            .field(
                "has_message_idempotency_key",
                &!self.message_idempotency_key.is_empty(),
            )
            .field("submission_sequence", &self.submission_sequence)
            .field("required_switch_id", &self.required_switch_id)
            .field("status", &self.status)
            .field(
                "user_message_timeline_item_id",
                &self.user_message_timeline_item_id,
            )
            .field("error_code", &self.error_code)
            .field("has_error_detail", &self.error_detail_redacted.is_some())
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .field("dispatched_at_ms", &self.dispatched_at_ms)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FetchTimelineRequest {
    pub session_id: VibexSessionId,
    pub after_sequence: Option<i64>,
    pub limit: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolvePermissionRequest {
    pub session_id: VibexSessionId,
    pub request_id: RequestId,
    pub resolution: PermissionResolution,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ResolveElicitationRequest {
    pub session_id: VibexSessionId,
    pub request_id: RequestId,
    pub resolution: crate::ElicitationResolution,
}

impl ResolveElicitationRequest {
    pub fn validate(&self) -> crate::VibexResult<()> {
        if self.resolution.request_id != self.request_id
            || self.resolution.session_id != self.session_id
        {
            return Err(crate::VibexError::validation(
                "elicitation_resolution_target_mismatch",
                "elicitation resolution must match the target session and request id",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentSessionSummary {
    pub session: AgentSession,
    pub latest_timeline: TimelinePage,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ProviderCapabilitiesResponse {
    pub providers: Vec<ProviderCapabilities>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandTrigger {
    Slash,
    Mention,
    Dollar,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandSourceKind {
    Provider,
    Prompt,
    Skill,
    Reference,
    ClientBuiltin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandSelectionBehavior {
    Insert,
    ExecuteImmediately,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandExecutionBehavior {
    None,
    ProviderCommand,
    ExpandPromptAndSend,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandEntry {
    pub id: String,
    pub trigger: AgentCommandTrigger,
    pub source_kind: AgentCommandSourceKind,
    pub label: String,
    pub description: Option<String>,
    pub insertion_text: String,
    pub command_name: Option<String>,
    pub provider_kind: Option<ProviderKind>,
    pub prompt_id: Option<PromptId>,
    pub skill_id: Option<SkillId>,
    pub reference_path: Option<String>,
    pub selection_behavior: AgentCommandSelectionBehavior,
    pub execution_behavior: AgentCommandExecutionBehavior,
    pub destructive: bool,
    pub metadata: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandDiscoverRequest {
    #[serde(default)]
    pub agent_id: Option<AgentId>,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub session_id: Option<VibexSessionId>,
    pub workspace_id: Option<WorkspaceId>,
    pub trigger: Option<AgentCommandTrigger>,
    pub query: Option<String>,
    pub limit: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandDiscoverResponse {
    pub entries: Vec<AgentCommandEntry>,
    pub diagnostics: Vec<ProviderBindingMetadata>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandExecuteRequest {
    pub session_id: VibexSessionId,
    pub command_id: Option<String>,
    pub trigger: AgentCommandTrigger,
    pub source_kind: AgentCommandSourceKind,
    pub command_text: String,
    pub command_name: Option<String>,
    pub arguments: Option<String>,
    pub prompt_id: Option<PromptId>,
    #[serde(default)]
    pub attachments: Vec<MessageAttachment>,
    #[serde(default)]
    pub reasoning_effort: Option<String>,
    pub correlation_id: Option<CorrelationId>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentCommandExecuteStatus {
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentCommandExecuteResult {
    pub status: AgentCommandExecuteStatus,
    pub message: Option<String>,
    pub items: Vec<TimelineItem>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn session_title_normalization_collapses_whitespace_and_bounds_unicode() {
        assert_eq!(
            normalize_agent_session_title("  plan\n\t the release  "),
            Some("plan the release".to_string())
        );
        assert_eq!(normalize_agent_session_title(" \n\t "), None);

        let title = normalize_agent_session_title(&"你".repeat(121)).unwrap();
        assert_eq!(title.chars().count(), MAX_AGENT_SESSION_TITLE_CHARS);
    }

    #[test]
    fn continuation_requirement_uses_turn_completion_for_idle_and_error_sessions() {
        assert!(agent_session_turn_requires_continuation(
            AgentSessionState::Idle,
            Some(false)
        ));
        assert!(!agent_session_turn_requires_continuation(
            AgentSessionState::Idle,
            Some(true)
        ));
        assert!(!agent_session_turn_requires_continuation(
            AgentSessionState::Idle,
            None
        ));
        assert!(agent_session_turn_requires_continuation(
            AgentSessionState::Error,
            Some(false)
        ));
        assert!(agent_session_turn_requires_continuation(
            AgentSessionState::Error,
            None
        ));
        assert!(!agent_session_turn_requires_continuation(
            AgentSessionState::Error,
            Some(true)
        ));
        assert!(!agent_session_turn_requires_continuation(
            AgentSessionState::NeedsInput,
            Some(false)
        ));
        assert!(!agent_session_turn_requires_continuation(
            AgentSessionState::Running,
            Some(false)
        ));
    }

    #[test]
    fn durable_send_request_serializes_required_runtime_and_idempotency_key() {
        let request = SendAgentMessageRequest {
            session_id: VibexSessionId::new(),
            message_idempotency_key: "message-1".to_string(),
            desired_runtime: SessionRuntimeSelection {
                reasoning_effort: Some("high".to_string()),
                ..SessionRuntimeSelection::provider(
                    AgentId::parse("codex").unwrap(),
                    ProviderProfileId::parse("provider_openai").unwrap(),
                    "gpt-5",
                )
            },
            text: "prompt-secret-SHOULD-NOT-DEBUG".to_string(),
            attachments: vec![MessageAttachment {
                label: "secret-attachment".to_string(),
                mime_type: Some("text/plain".to_string()),
                uri: Some("file:///private/secret.txt".to_string()),
                inline_text_offset: None,
            }],
            reasoning_effort: Some("high".to_string()),
            correlation_id: None,
        };

        let json = serde_json::to_value(&request).unwrap();
        assert_eq!(json["messageIdempotencyKey"], "message-1");
        assert_eq!(json["desiredRuntime"]["agentId"], "codex");
        assert_eq!(json["desiredRuntime"]["modelId"], "gpt-5");
        let debug = format!("{request:?}");
        assert!(!debug.contains("prompt-secret-SHOULD-NOT-DEBUG"));
        assert!(!debug.contains("secret-attachment"));
        assert!(!debug.contains("/private/secret.txt"));
        assert!(!debug.contains("message-1"));

        let query = GetMessageSubmissionRequest {
            session_id: request.session_id.clone(),
            message_idempotency_key: "message-query-secret".to_string(),
        };
        assert!(!format!("{query:?}").contains("message-query-secret"));

        let state = MessageSubmissionState {
            submission_id: MessageSubmissionId::new(),
            session_id: request.session_id,
            message_idempotency_key: "message-state-secret".to_string(),
            submission_sequence: 1,
            desired_runtime: request.desired_runtime,
            required_switch_id: None,
            status: MessageSubmissionStatus::Completed,
            user_message_timeline_item_id: None,
            error_code: None,
            error_detail_redacted: None,
            created_at_ms: 1,
            updated_at_ms: 2,
            dispatched_at_ms: Some(2),
        };
        assert!(!format!("{state:?}").contains("message-state-secret"));
    }

    #[test]
    fn notification_intents_have_stable_ids_bounded_ttl_and_no_message_content() {
        let session_id = VibexSessionId::new();
        let source_event_id = TimelineItemId::new();
        let payload = crate::TimelinePayload::AgentMessage(crate::AgentMessagePayload {
            text: "private answer that must not enter a notification".to_string(),
            is_final: true,
        });
        let item = TimelineItem {
            id: source_event_id.clone(),
            session_id: session_id.clone(),
            sequence: 7,
            timestamp_ms: 1_000,
            source: crate::TimelineSource::Agent,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: crate::TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        };
        let request_id = RequestId::new();

        let approval = AgentNotificationIntent::approval_required(&item, request_id.clone());
        assert_eq!(
            approval.notification_id,
            format!("approval.{}.{}", session_id.as_str(), request_id.as_str())
        );
        assert_eq!(approval.source_event_id, source_event_id);
        assert_eq!(approval.opaque_locator, session_id.as_str());
        assert_eq!(
            approval.expires_at_ms - approval.created_at_ms,
            AGENT_ATTENTION_NOTIFICATION_TTL_MS
        );
        assert_eq!(
            approval,
            AgentNotificationIntent::approval_required(&item, request_id.clone())
        );
        let approval_json = serde_json::to_value(&approval).unwrap();
        assert_eq!(approval_json["kind"]["type"], "approval_required");
        assert_eq!(
            approval_json["kind"]["data"]["requestId"],
            request_id.as_str()
        );
        assert!(approval_json["kind"]["data"].get("request_id").is_none());

        let completed = AgentNotificationIntent::turn_completed(&item);
        assert_eq!(
            completed.expires_at_ms - completed.created_at_ms,
            AGENT_TERMINAL_NOTIFICATION_TTL_MS
        );
        let encoded = serde_json::to_string(&completed).unwrap();
        assert!(!encoded.contains("private answer"));
        assert!(!format!("{completed:?}").contains("private answer"));
    }
}

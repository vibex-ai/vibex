use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Deserializer, Serialize};

use crate::agent_config::AgentId;
use crate::elicitation::{ElicitationRequest, ElicitationResolution};
use crate::ids::{
    CorrelationId, ProviderProfileId, RuntimeBindingId, TimelineItemId, VibexSessionId,
};
use crate::permission::{PermissionRequest, PermissionResolution};
use crate::{RuntimeAuthSource, RuntimeModelSelection};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineSource {
    User,
    Agent,
    System,
    Provider,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineItemKind {
    UserMessage,
    AgentMessageDelta,
    AgentMessage,
    Reasoning,
    Plan,
    ToolCall,
    Command,
    FileOperation,
    WebSearch,
    TodoUpdate,
    Collaboration,
    ImageGeneration,
    GitNotice,
    SystemNotice,
    PermissionRequest,
    PermissionResolution,
    ElicitationRequest,
    ElicitationResolution,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineRedactionState {
    None,
    Redacted,
    ContainsRedactions,
}

const TURN_ATTRIBUTION_LABEL_LIMIT: usize = 160;
const TURN_ATTRIBUTION_MODEL_ID_LIMIT: usize = 256;

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnExecutionAttribution {
    pub agent_id: AgentId,
    pub auth_source: RuntimeAuthSource,
    pub model: RuntimeModelSelection,
    pub effective_model_id: Option<String>,
    pub binding_id: RuntimeBindingId,
    pub activation_generation: i64,
    pub agent_label: String,
    pub auth_source_label: String,
    pub model_label: String,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct TurnExecutionAttributionWire {
    agent_id: AgentId,
    auth_source: Option<RuntimeAuthSource>,
    provider_profile_id: Option<ProviderProfileId>,
    model: Option<RuntimeModelSelection>,
    model_id: Option<String>,
    effective_model_id: Option<String>,
    binding_id: RuntimeBindingId,
    activation_generation: i64,
    agent_label: String,
    #[serde(alias = "providerProfileLabel")]
    auth_source_label: String,
    model_label: String,
}

impl<'de> Deserialize<'de> for TurnExecutionAttribution {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let wire = TurnExecutionAttributionWire::deserialize(deserializer)?;
        let auth_source = wire
            .auth_source
            .or_else(|| {
                wire.provider_profile_id
                    .map(RuntimeAuthSource::provider_profile)
            })
            .ok_or_else(|| serde::de::Error::missing_field("authSource"))?;
        let model = wire
            .model
            .or_else(|| wire.model_id.clone().map(RuntimeModelSelection::explicit))
            .ok_or_else(|| serde::de::Error::missing_field("model"))?;
        Self::new(
            wire.agent_id,
            auth_source,
            model,
            wire.effective_model_id.or(wire.model_id),
            wire.binding_id,
            wire.activation_generation,
            wire.agent_label,
            wire.auth_source_label,
            wire.model_label,
        )
        .ok_or_else(|| serde::de::Error::custom("invalid turn execution attribution"))
    }
}

impl TurnExecutionAttribution {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        agent_id: AgentId,
        auth_source: RuntimeAuthSource,
        model: RuntimeModelSelection,
        effective_model_id: Option<String>,
        binding_id: RuntimeBindingId,
        activation_generation: i64,
        agent_label: impl Into<String>,
        auth_source_label: impl Into<String>,
        model_label: impl Into<String>,
    ) -> Option<Self> {
        if activation_generation < 0 {
            return None;
        }
        let model = match model {
            RuntimeModelSelection::Explicit { model_id } => RuntimeModelSelection::Explicit {
                model_id: bounded_attribution_value(model_id, TURN_ATTRIBUTION_MODEL_ID_LIMIT)?,
            },
            RuntimeModelSelection::AgentDefault => RuntimeModelSelection::AgentDefault,
        };
        let effective_model_id = match effective_model_id {
            Some(model_id) => Some(bounded_attribution_value(
                model_id,
                TURN_ATTRIBUTION_MODEL_ID_LIMIT,
            )?),
            None => None,
        };
        let agent_label = bounded_attribution_value(agent_label, TURN_ATTRIBUTION_LABEL_LIMIT)?;
        let auth_source_label =
            bounded_attribution_value(auth_source_label, TURN_ATTRIBUTION_LABEL_LIMIT)?;
        let model_label = bounded_attribution_value(model_label, TURN_ATTRIBUTION_LABEL_LIMIT)?;
        Some(Self {
            agent_id,
            auth_source,
            model,
            effective_model_id,
            binding_id,
            activation_generation,
            agent_label,
            auth_source_label,
            model_label,
        })
    }

    pub fn view(&self) -> TurnExecutionAttributionView {
        TurnExecutionAttributionView {
            agent_label: self.agent_label.clone(),
            auth_source_label: self.auth_source_label.clone(),
            model_label: self.model_label.clone(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TurnExecutionAttributionView {
    pub agent_label: String,
    #[serde(alias = "providerProfileLabel")]
    pub auth_source_label: String,
    pub model_label: String,
}

fn bounded_attribution_value(value: impl Into<String>, limit: usize) -> Option<String> {
    let value = value.into();
    let value = value.trim();
    if value.is_empty() {
        return None;
    }
    Some(bounded_event_bytes(value, limit).0)
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct MessageAttachment {
    pub label: String,
    pub mime_type: Option<String>,
    pub uri: Option<String>,
    #[serde(default)]
    pub inline_text_offset: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessagePayload {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessageDeltaPayload {
    pub text_delta: String,
    pub chunk_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<AgentMessagePhase>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentMessagePhase {
    Commentary,
    FinalAnswer,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentMessagePayload {
    pub text: String,
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ReasoningPayload {
    pub text: String,
    pub is_final: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanStepPayload {
    pub title: String,
    pub status: PlanStepStatus,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlanStepStatus {
    Pending,
    Running,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct PlanPayload {
    pub title: String,
    pub steps: Vec<PlanStepPayload>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    Started,
    Progress,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ToolCallPayload {
    pub tool_call_id: String,
    pub tool_name: String,
    pub status: ToolCallStatus,
    pub summary: String,
    pub input_summary: Option<String>,
    pub output_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CommandStatus {
    Started,
    Completed,
    Failed,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CommandPayload {
    pub command: String,
    pub cwd: Option<String>,
    pub status: CommandStatus,
    pub exit_code: Option<i32>,
    pub output_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationKind {
    Read,
    Write,
    Edit,
    Delete,
    Move,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationPayload {
    pub operation: FileOperationKind,
    pub path: String,
    pub summary: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub old_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub new_text: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

const RAW_EVENT_TEXT_LIMIT: usize = 4_096;
const RAW_EVENT_SUMMARY_LIMIT: usize = 512;
const RAW_EVENT_ITEM_LIMIT: usize = 16;
const RAW_EVENT_KEY_LIMIT: usize = 64;
const RAW_EVENT_URI_LIMIT: usize = 1_024;
const RAW_EVENT_TRUNCATION_SUFFIX: &str = "...(truncated)";

fn bounded_event_bytes(value: &str, limit: usize) -> (String, bool) {
    if value.len() <= limit {
        return (value.to_string(), false);
    }
    let suffix = RAW_EVENT_TRUNCATION_SUFFIX;
    let mut end = limit.saturating_sub(suffix.len()).min(value.len());
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    (format!("{}{suffix}", &value[..end]), true)
}

fn bounded_text(value: impl Into<String>, limit: usize) -> (String, bool) {
    let value = value.into();
    let value = value.trim();
    bounded_event_bytes(value, limit)
}

fn bounded_event_value(value: impl Into<String>, limit: usize) -> (String, bool) {
    let value = value.into();
    if value.ends_with(RAW_EVENT_TRUNCATION_SUFFIX) {
        let (value, _) = bounded_event_bytes(value.trim(), limit);
        return (value, true);
    }
    bounded_text(value, limit)
}

fn bounded_event_output_value(value: impl Into<String>, limit: usize) -> (String, bool) {
    let value = value.into();
    if value.ends_with(RAW_EVENT_TRUNCATION_SUFFIX) {
        let (value, _) = bounded_event_bytes(&value, limit);
        return (value, true);
    }
    bounded_event_bytes(&value, limit)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentEventRawOutputMode {
    Snapshot,
    Append,
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventRawOutput {
    pub mode: AgentEventRawOutputMode,
    pub text: String,
    #[serde(skip)]
    sanitized: bool,
}

impl fmt::Debug for AgentEventRawOutput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventRawOutput")
            .field("mode", &self.mode)
            .field("text", &"<redacted>")
            .finish()
    }
}

impl AgentEventRawOutput {
    pub fn new(mode: AgentEventRawOutputMode, text: impl Into<String>) -> (Self, bool) {
        let (text, truncated) = bounded_event_output_value(text, RAW_EVENT_TEXT_LIMIT);
        (
            Self {
                mode,
                text,
                sanitized: truncated,
            },
            truncated,
        )
    }
}

impl<'de> Deserialize<'de> for AgentEventRawOutput {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            mode: AgentEventRawOutputMode,
            text: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self::new(wire.mode, wire.text).0)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventContentBlock {
    pub block_type: String,
    pub summary: String,
    #[serde(skip)]
    sanitized: bool,
}

impl fmt::Debug for AgentEventContentBlock {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventContentBlock")
            .field("block_type", &self.block_type)
            .field("summary", &"<redacted>")
            .finish()
    }
}

impl AgentEventContentBlock {
    pub fn new(block_type: impl Into<String>, summary: impl Into<String>) -> (Self, bool) {
        let (block_type, type_truncated) = bounded_event_value(block_type, RAW_EVENT_KEY_LIMIT);
        let (summary, summary_truncated) = bounded_event_value(summary, RAW_EVENT_SUMMARY_LIMIT);
        (
            Self {
                block_type,
                summary,
                sanitized: type_truncated || summary_truncated,
            },
            type_truncated || summary_truncated,
        )
    }
}

impl<'de> Deserialize<'de> for AgentEventContentBlock {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            block_type: String,
            summary: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self::new(wire.block_type, wire.summary).0)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventLocation {
    pub uri: String,
    pub line: Option<u32>,
    pub column: Option<u32>,
    #[serde(skip)]
    sanitized: bool,
}

impl fmt::Debug for AgentEventLocation {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventLocation")
            .field("uri", &"<redacted>")
            .field("line", &self.line)
            .field("column", &self.column)
            .finish()
    }
}

impl AgentEventLocation {
    pub fn new(uri: impl Into<String>, line: Option<u32>, column: Option<u32>) -> (Self, bool) {
        let (uri, truncated) = bounded_event_value(uri, RAW_EVENT_URI_LIMIT);
        (
            Self {
                uri,
                line,
                column,
                sanitized: truncated,
            },
            truncated,
        )
    }
}

impl<'de> Deserialize<'de> for AgentEventLocation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            uri: String,
            line: Option<u32>,
            column: Option<u32>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Ok(Self::new(wire.uri, wire.line, wire.column).0)
    }
}

#[derive(Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentEventRawExtension {
    pub schema_version: u16,
    pub content_blocks: Vec<AgentEventContentBlock>,
    pub raw_input: Option<String>,
    pub raw_output: Option<AgentEventRawOutput>,
    pub locations: Vec<AgentEventLocation>,
    pub meta: BTreeMap<String, String>,
    pub truncated: bool,
}

impl fmt::Debug for AgentEventRawExtension {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventRawExtension")
            .field("schema_version", &self.schema_version)
            .field("content_block_count", &self.content_blocks.len())
            .field("has_raw_input", &self.raw_input.is_some())
            .field(
                "raw_output_mode",
                &self.raw_output.as_ref().map(|value| value.mode),
            )
            .field("location_count", &self.locations.len())
            .field("meta_keys", &self.meta.keys().collect::<Vec<_>>())
            .field("truncated", &self.truncated)
            .finish()
    }
}

impl AgentEventRawExtension {
    pub fn bounded_text(value: impl Into<String>) -> String {
        bounded_event_value(value, RAW_EVENT_TEXT_LIMIT).0
    }

    pub fn new(
        content_blocks: Vec<AgentEventContentBlock>,
        raw_input: Option<String>,
        raw_output: Option<AgentEventRawOutput>,
        locations: Vec<AgentEventLocation>,
        meta: BTreeMap<String, String>,
        already_truncated: bool,
    ) -> Self {
        let mut truncated = already_truncated;
        truncated |= content_blocks.len() > RAW_EVENT_ITEM_LIMIT;
        truncated |= locations.len() > RAW_EVENT_ITEM_LIMIT;
        truncated |= meta.len() > RAW_EVENT_ITEM_LIMIT;
        truncated |= content_blocks.iter().any(|block| block.sanitized);
        truncated |= locations.iter().any(|location| location.sanitized);
        truncated |= raw_output.as_ref().is_some_and(|output| output.sanitized);
        let content_blocks = content_blocks
            .into_iter()
            .take(RAW_EVENT_ITEM_LIMIT)
            .collect::<Vec<_>>();
        let locations = locations
            .into_iter()
            .take(RAW_EVENT_ITEM_LIMIT)
            .collect::<Vec<_>>();
        let (raw_input, input_truncated) = raw_input
            .map(|value| bounded_event_value(value, RAW_EVENT_TEXT_LIMIT))
            .map_or((None, false), |(value, was_truncated)| {
                (Some(value), was_truncated)
            });
        truncated |= input_truncated;
        let mut bounded_meta = BTreeMap::new();
        for (key, value) in meta {
            let (key, key_truncated) = bounded_text(key, RAW_EVENT_KEY_LIMIT);
            let (value, value_truncated) = bounded_event_value(value, RAW_EVENT_SUMMARY_LIMIT);
            truncated |= key_truncated || value_truncated;
            if !key.is_empty() {
                bounded_meta.insert(key, value);
                if bounded_meta.len() == RAW_EVENT_ITEM_LIMIT {
                    break;
                }
            }
        }
        Self {
            schema_version: 1,
            content_blocks,
            raw_input,
            raw_output,
            locations,
            meta: bounded_meta,
            truncated,
        }
    }

    pub fn is_empty(&self) -> bool {
        !self.truncated
            && self.content_blocks.is_empty()
            && self.raw_input.is_none()
            && self.raw_output.is_none()
            && self.locations.is_empty()
            && self.meta.is_empty()
    }
}

impl<'de> Deserialize<'de> for AgentEventRawExtension {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "camelCase")]
        struct Wire {
            schema_version: u16,
            #[serde(default)]
            content_blocks: Vec<AgentEventContentBlock>,
            raw_input: Option<String>,
            raw_output: Option<AgentEventRawOutput>,
            #[serde(default)]
            locations: Vec<AgentEventLocation>,
            #[serde(default)]
            meta: BTreeMap<String, String>,
            #[serde(default)]
            truncated: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        if wire.schema_version != 1 {
            return Err(serde::de::Error::custom(
                "unsupported agent event raw extension schema version",
            ));
        }
        Ok(Self::new(
            wire.content_blocks,
            wire.raw_input,
            wire.raw_output,
            wire.locations,
            wire.meta,
            wire.truncated,
        ))
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct WebSearchPayload {
    pub query: String,
    pub status: ToolCallStatus,
    pub result_summary: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TodoUpdatePayload {
    pub title: String,
    pub items: Vec<PlanStepPayload>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct CollaborationPayload {
    pub action: String,
    pub status: ToolCallStatus,
    pub summary: String,
    pub agent_label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ImageGenerationPayload {
    pub status: ToolCallStatus,
    pub summary: String,
    pub mime_type: Option<String>,
    pub image_reference: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct GitNoticePayload {
    pub summary: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SystemNoticeLevel {
    Info,
    Warning,
    Error,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemNoticePayload {
    pub level: SystemNoticeLevel,
    pub message: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineErrorPayload {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TimelinePayload {
    UserMessage(UserMessagePayload),
    AgentMessageDelta(AgentMessageDeltaPayload),
    AgentMessage(AgentMessagePayload),
    Reasoning(ReasoningPayload),
    Plan(PlanPayload),
    ToolCall(ToolCallPayload),
    Command(CommandPayload),
    FileOperation(FileOperationPayload),
    WebSearch(WebSearchPayload),
    TodoUpdate(TodoUpdatePayload),
    Collaboration(CollaborationPayload),
    ImageGeneration(ImageGenerationPayload),
    GitNotice(GitNoticePayload),
    SystemNotice(SystemNoticePayload),
    PermissionRequest(PermissionRequest),
    PermissionResolution(PermissionResolution),
    ElicitationRequest(ElicitationRequest),
    ElicitationResolution(ElicitationResolution),
    Error(TimelineErrorPayload),
}

impl TimelinePayload {
    pub const fn kind(&self) -> TimelineItemKind {
        match self {
            Self::UserMessage(_) => TimelineItemKind::UserMessage,
            Self::AgentMessageDelta(_) => TimelineItemKind::AgentMessageDelta,
            Self::AgentMessage(_) => TimelineItemKind::AgentMessage,
            Self::Reasoning(_) => TimelineItemKind::Reasoning,
            Self::Plan(_) => TimelineItemKind::Plan,
            Self::ToolCall(_) => TimelineItemKind::ToolCall,
            Self::Command(_) => TimelineItemKind::Command,
            Self::FileOperation(_) => TimelineItemKind::FileOperation,
            Self::WebSearch(_) => TimelineItemKind::WebSearch,
            Self::TodoUpdate(_) => TimelineItemKind::TodoUpdate,
            Self::Collaboration(_) => TimelineItemKind::Collaboration,
            Self::ImageGeneration(_) => TimelineItemKind::ImageGeneration,
            Self::GitNotice(_) => TimelineItemKind::GitNotice,
            Self::SystemNotice(_) => TimelineItemKind::SystemNotice,
            Self::PermissionRequest(_) => TimelineItemKind::PermissionRequest,
            Self::PermissionResolution(_) => TimelineItemKind::PermissionResolution,
            Self::ElicitationRequest(_) => TimelineItemKind::ElicitationRequest,
            Self::ElicitationResolution(_) => TimelineItemKind::ElicitationResolution,
            Self::Error(_) => TimelineItemKind::Error,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineItem {
    pub id: TimelineItemId,
    pub session_id: VibexSessionId,
    pub sequence: i64,
    pub timestamp_ms: i64,
    pub source: TimelineSource,
    pub kind: TimelineItemKind,
    pub correlation_id: Option<CorrelationId>,
    pub provider_correlation_id: Option<String>,
    pub redaction_state: TimelineRedactionState,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_attribution: Option<TurnExecutionAttributionView>,
    pub payload: TimelinePayload,
}

/// Returns whether the latest conversational turn has an explicit final Agent
/// message. `None` means the timeline has no conversational turn to continue.
pub fn latest_timeline_turn_ended_normally(items: &[TimelineItem]) -> Option<bool> {
    let latest_boundary = items.iter().rposition(|item| {
        item.kind == TimelineItemKind::UserMessage
            || (item.kind == TimelineItemKind::Error && item.provider_correlation_id.is_none())
    });
    let turn_items = &items[latest_boundary.unwrap_or_default()..];
    let has_turn_content = latest_boundary.is_some()
        || turn_items.iter().any(|item| {
            matches!(
                item.source,
                TimelineSource::Agent | TimelineSource::Provider
            ) && item.kind != TimelineItemKind::SystemNotice
        });
    if !has_turn_content {
        return None;
    }

    let latest_conversational_item = turn_items.iter().rev().find(|item| {
        matches!(
            item.source,
            TimelineSource::Agent | TimelineSource::Provider
        ) && item.kind != TimelineItemKind::SystemNotice
    });
    Some(latest_conversational_item.is_some_and(|item| {
        matches!(
            &item.payload,
            TimelinePayload::AgentMessage(message) if message.is_final
        )
    }))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelinePage {
    pub session_id: VibexSessionId,
    pub items: Vec<TimelineItem>,
    pub start_sequence: Option<i64>,
    pub end_sequence: Option<i64>,
    pub has_older: bool,
    pub has_newer: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineLiveEvent {
    pub session_id: VibexSessionId,
    pub sequence: i64,
    pub item: TimelineItem,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn completion_item(
        session_id: &VibexSessionId,
        sequence: i64,
        source: TimelineSource,
        payload: TimelinePayload,
    ) -> TimelineItem {
        TimelineItem {
            id: TimelineItemId::new(),
            session_id: session_id.clone(),
            sequence,
            timestamp_ms: sequence,
            source,
            kind: payload.kind(),
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload,
        }
    }

    #[test]
    fn latest_turn_normal_end_requires_an_explicit_final_agent_message() {
        let session_id = VibexSessionId::new();
        let system_notice = completion_item(
            &session_id,
            1,
            TimelineSource::System,
            TimelinePayload::SystemNotice(SystemNoticePayload {
                level: SystemNoticeLevel::Info,
                message: "runtime ready".into(),
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(std::slice::from_ref(&system_notice)),
            None
        );

        let user_message = completion_item(
            &session_id,
            2,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "finish the task".into(),
                attachments: Vec::new(),
            }),
        );
        let commentary = completion_item(
            &session_id,
            3,
            TimelineSource::Agent,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "still working".into(),
                chunk_index: 0,
                phase: Some(AgentMessagePhase::Commentary),
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[
                system_notice.clone(),
                user_message.clone(),
                commentary.clone(),
            ]),
            Some(false)
        );

        let final_message = completion_item(
            &session_id,
            4,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[
                system_notice,
                user_message,
                commentary,
                final_message.clone(),
            ]),
            Some(true)
        );

        let error = completion_item(
            &session_id,
            5,
            TimelineSource::Provider,
            TimelinePayload::Error(TimelineErrorPayload {
                code: "turn_stopped".into(),
                message: "stopped".into(),
                recoverable: true,
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[final_message.clone(), error.clone()]),
            Some(false)
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[error, final_message.clone()]),
            Some(true)
        );

        let trailing_delta = completion_item(
            &session_id,
            6,
            TimelineSource::Agent,
            TimelinePayload::AgentMessageDelta(AgentMessageDeltaPayload {
                text_delta: "more work".into(),
                chunk_index: 1,
                phase: Some(AgentMessagePhase::Commentary),
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[final_message, trailing_delta]),
            Some(false)
        );
    }

    fn sample_turn_attribution() -> TurnExecutionAttribution {
        TurnExecutionAttribution::new(
            AgentId::parse("codex").unwrap(),
            RuntimeAuthSource::provider_profile(ProviderProfileId::parse("provider_work").unwrap()),
            RuntimeModelSelection::explicit("gpt-5"),
            Some("gpt-5".to_string()),
            RuntimeBindingId::parse("binding_current").unwrap(),
            7,
            "Codex",
            "OpenAI work",
            "GPT-5",
        )
        .unwrap()
    }

    #[test]
    fn turn_attribution_bounds_and_revalidates_deserialization() {
        let long = "x".repeat(TURN_ATTRIBUTION_LABEL_LIMIT * 2);
        let attribution = TurnExecutionAttribution::new(
            AgentId::parse("codex").unwrap(),
            RuntimeAuthSource::provider_profile(ProviderProfileId::parse("provider_work").unwrap()),
            RuntimeModelSelection::explicit("gpt-5"),
            Some("gpt-5".to_string()),
            RuntimeBindingId::parse("binding_current").unwrap(),
            7,
            long.clone(),
            long.clone(),
            long,
        )
        .unwrap();
        assert!(attribution.agent_label.len() <= TURN_ATTRIBUTION_LABEL_LIMIT);
        assert!(attribution.auth_source_label.len() <= TURN_ATTRIBUTION_LABEL_LIMIT);
        assert!(attribution.model_label.len() <= TURN_ATTRIBUTION_LABEL_LIMIT);

        let encoded = serde_json::to_value(sample_turn_attribution()).unwrap();
        assert_eq!(
            serde_json::from_value::<TurnExecutionAttribution>(encoded).unwrap(),
            sample_turn_attribution()
        );
        let invalid = serde_json::json!({
            "agentId": "codex",
            "providerProfileId": "provider_work",
            "modelId": "gpt-5",
            "bindingId": "binding_current",
            "activationGeneration": -1,
            "agentLabel": "Codex",
            "providerProfileLabel": "OpenAI work",
            "modelLabel": "GPT-5"
        });
        assert!(serde_json::from_value::<TurnExecutionAttribution>(invalid).is_err());
    }

    #[test]
    fn timeline_item_exposes_only_safe_turn_attribution_view() {
        let attribution = sample_turn_attribution();
        let item = TimelineItem {
            id: TimelineItemId::new(),
            session_id: VibexSessionId::new(),
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Agent,
            kind: TimelineItemKind::AgentMessage,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: Some(attribution.view()),
            payload: TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".to_string(),
                is_final: true,
            }),
        };
        let encoded = serde_json::to_string(&item).unwrap();
        assert!(encoded.contains("executionAttribution"));
        assert!(!encoded.contains("binding_current"));
        assert!(!encoded.contains("activationGeneration"));

        let mut legacy = serde_json::to_value(&item).unwrap();
        legacy
            .as_object_mut()
            .unwrap()
            .remove("executionAttribution");
        let decoded: TimelineItem = serde_json::from_value(legacy).unwrap();
        assert_eq!(decoded.execution_attribution, None);
    }

    #[test]
    fn agent_message_delta_phase_is_optional_and_uses_provider_neutral_values() {
        let final_answer = AgentMessageDeltaPayload {
            text_delta: "done".to_string(),
            chunk_index: 0,
            phase: Some(AgentMessagePhase::FinalAnswer),
        };
        assert_eq!(
            serde_json::to_value(&final_answer).unwrap(),
            serde_json::json!({
                "textDelta": "done",
                "chunkIndex": 0,
                "phase": "final_answer"
            })
        );

        let legacy: AgentMessageDeltaPayload = serde_json::from_value(serde_json::json!({
            "textDelta": "legacy",
            "chunkIndex": 1
        }))
        .unwrap();
        assert_eq!(legacy.phase, None);
    }

    #[test]
    fn raw_extension_preserves_content_bounds_and_revalidates_deserialization() {
        let long = "x".repeat(RAW_EVENT_TEXT_LIMIT * 2);
        let output = AgentEventRawOutput::new(AgentEventRawOutputMode::Append, long.clone()).0;
        let extension = AgentEventRawExtension::new(
            vec![AgentEventContentBlock::new("text", long.clone()).0; RAW_EVENT_ITEM_LIMIT + 2],
            Some("/home/alice/repo sk-secret-value".to_string()),
            Some(output),
            vec![
                AgentEventLocation::new("/Users/alice/repo/src/lib.rs", Some(1), Some(2)).0;
                RAW_EVENT_ITEM_LIMIT + 2
            ],
            BTreeMap::from([
                ("safe".to_string(), long),
                ("apiToken".to_string(), "secret-value".to_string()),
            ]),
            false,
        );
        assert!(extension.truncated);
        assert_eq!(extension.content_blocks.len(), RAW_EVENT_ITEM_LIMIT);
        assert_eq!(extension.locations.len(), RAW_EVENT_ITEM_LIMIT);
        assert_eq!(
            extension.meta.get("apiToken").map(String::as_str),
            Some("secret-value")
        );
        let encoded = serde_json::to_string(&extension).unwrap();
        assert!(encoded.contains("alice"));
        assert!(encoded.contains("secret-value"));
        let decoded: AgentEventRawExtension = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, extension);
        let debug = format!("{extension:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret-value"));

        let meta_only = AgentEventRawExtension::new(
            Vec::new(),
            None,
            None,
            Vec::new(),
            BTreeMap::from([(String::from("apiToken"), String::from("secret-value"))]),
            false,
        );
        assert!(!meta_only.truncated);
        assert!(!meta_only.is_empty());
        let unsupported_schema = serde_json::json!({
            "schemaVersion": 2,
            "contentBlocks": [],
            "locations": [],
            "meta": {},
            "truncated": false
        });
        assert!(serde_json::from_value::<AgentEventRawExtension>(unsupported_schema).is_err());
    }

    #[test]
    fn raw_extension_preserves_sensitive_output_and_utf8_bounds_round_trip() {
        let sensitive = AgentEventRawExtension::new(
            Vec::new(),
            None,
            Some(
                AgentEventRawOutput::new(
                    AgentEventRawOutputMode::Snapshot,
                    "Authorization: Bearer secret-value",
                )
                .0,
            ),
            Vec::new(),
            BTreeMap::new(),
            false,
        );
        let encoded = serde_json::to_string(&sensitive).unwrap();
        assert!(encoded.contains("secret-value"));
        let decoded: AgentEventRawExtension = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, sensitive);
        assert!(!decoded.truncated);

        let unicode = "界".repeat(RAW_EVENT_TEXT_LIMIT);
        let extension = AgentEventRawExtension::new(
            vec![AgentEventContentBlock::new("text", unicode.clone()).0],
            Some(unicode.clone()),
            Some(AgentEventRawOutput::new(AgentEventRawOutputMode::Append, unicode).0),
            Vec::new(),
            BTreeMap::new(),
            false,
        );
        assert!(extension.raw_input.as_ref().unwrap().len() <= RAW_EVENT_TEXT_LIMIT);
        assert!(extension.raw_output.as_ref().unwrap().text.len() <= RAW_EVENT_TEXT_LIMIT);
        assert!(extension.content_blocks[0].summary.len() <= RAW_EVENT_SUMMARY_LIMIT);
        let encoded = serde_json::to_string(&extension).unwrap();
        let decoded: AgentEventRawExtension = serde_json::from_str(&encoded).unwrap();
        assert_eq!(decoded, extension);
    }

    #[test]
    fn raw_extension_preserves_cross_platform_private_paths() {
        let extension = AgentEventRawExtension::new(
            Vec::new(),
            Some(r#"{"cwd":"C:\\Users\\alice\\private-repo"}"#.to_string()),
            Some(
                AgentEventRawOutput::new(
                    AgentEventRawOutputMode::Snapshot,
                    r#"wrote C:\Users\alice\private-repo\src\lib.rs"#,
                )
                .0,
            ),
            vec![
                AgentEventLocation::new(r#"C:\Users\alice\private-repo\src\lib.rs"#, Some(1), None)
                    .0,
            ],
            BTreeMap::new(),
            false,
        );
        let encoded = serde_json::to_string(&extension).unwrap();
        assert!(encoded.contains("alice"));
        assert!(encoded.contains(r#"C:\\Users\\alice\\private-repo"#));
    }

    #[test]
    fn absent_raw_extension_preserves_legacy_tool_json() {
        let payload = ToolCallPayload {
            tool_call_id: "tool-safe".to_string(),
            tool_name: "lookup".to_string(),
            status: ToolCallStatus::Completed,
            summary: "done".to_string(),
            input_summary: None,
            output_summary: None,
            raw_extension: None,
        };
        let json = serde_json::to_value(&payload).unwrap();
        assert!(json.get("rawExtension").is_none());
        let legacy: ToolCallPayload = serde_json::from_value(serde_json::json!({
            "toolCallId": "tool-safe",
            "toolName": "lookup",
            "status": "completed",
            "summary": "done",
            "inputSummary": null,
            "outputSummary": null
        }))
        .unwrap();
        assert_eq!(legacy.raw_extension, None);
    }

    #[test]
    fn advanced_timeline_variants_have_stable_kinds() {
        let variants = [
            TimelinePayload::WebSearch(WebSearchPayload {
                query: "rust".to_string(),
                status: ToolCallStatus::Completed,
                result_summary: None,
                raw_extension: None,
            }),
            TimelinePayload::TodoUpdate(TodoUpdatePayload {
                title: "todo".to_string(),
                items: Vec::new(),
                raw_extension: None,
            }),
            TimelinePayload::Collaboration(CollaborationPayload {
                action: "spawn_agent".to_string(),
                status: ToolCallStatus::Completed,
                summary: "done".to_string(),
                agent_label: None,
                raw_extension: None,
            }),
            TimelinePayload::ImageGeneration(ImageGenerationPayload {
                status: ToolCallStatus::Completed,
                summary: "image".to_string(),
                mime_type: Some("image/png".to_string()),
                image_reference: None,
                raw_extension: None,
            }),
        ];
        assert_eq!(
            variants.map(|payload| payload.kind()),
            [
                TimelineItemKind::WebSearch,
                TimelineItemKind::TodoUpdate,
                TimelineItemKind::Collaboration,
                TimelineItemKind::ImageGeneration,
            ]
        );
    }
}

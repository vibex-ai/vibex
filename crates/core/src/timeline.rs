use std::collections::BTreeMap;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Deserializer, Serialize};
use similar::TextDiff;

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
    Goal,
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
    Retry,
    Error,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelineRedactionState {
    None,
    Redacted,
    ContainsRedactions,
}

/// How much of a timeline item's payload a read carries.
///
/// Timeline content is read in two depths. `Summary` is what a page or a live
/// event ships by default: every field the conversation projection needs, and
/// none of the unbounded tool detail. `Detail` is the whole payload, fetched
/// one item at a time when a reader opens a tool activity.
///
/// The split exists because the heavy fields are tool detail rather than
/// conversation: file snapshots (`oldText`/`newText`/`patch`) are as large as
/// the files they describe and raw extension output is bounded per block but
/// multiplied across a long turn, while a collapsed activity row only ever
/// needs the path, action, and bounded input summary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TimelinePayloadDepth {
    /// Conversation-complete payload with tool detail withheld.
    Summary,
    /// The full payload, including tool output and file snapshots.
    ///
    /// This is the deserialization default so that a payload read straight out
    /// of durable storage — which is always complete — is not mistaken for a
    /// stripped one and re-fetched.
    #[default]
    Detail,
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
            auth_source: Some(self.auth_source.clone()),
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
    /// The account `auth_source_label` names, when the writer knew it.
    ///
    /// The label is written by the runtime in English, but an Agent account is
    /// the product's own concept rather than a name the reader chose, so a
    /// client that renders a locale names it from the kind instead. Absent on
    /// records written before the kind travelled with the label, and then the
    /// label is all there is to show.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auth_source: Option<RuntimeAuthSource>,
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

impl MessageAttachment {
    /// The words this attachment contributes to a one-line projection of its
    /// message.
    ///
    /// A picture pasted into a composer carries a label and often no prose at
    /// all, so a projection that kept only the text would leave a session with
    /// nothing to name it — or, when the reader wrote around the picture, a
    /// sentence that starts where the picture was. The label is what the client
    /// that wrote the message drew in its place, so it is what every other
    /// client puts back. An attachment that arrived without one is still named
    /// by what it is.
    pub fn display_label(&self) -> &str {
        let label = self.label.trim();
        if !label.is_empty() {
            return label;
        }
        if self
            .mime_type
            .as_deref()
            .is_some_and(|mime| mime.trim().to_ascii_lowercase().starts_with("image/"))
            || self
                .uri
                .as_deref()
                .is_some_and(|uri| uri.trim().to_ascii_lowercase().starts_with("data:image/"))
        {
            "Image"
        } else {
            "Attachment"
        }
    }
}

/// A user message projected to one line of display text.
///
/// This is the projection a session title, a sidebar row or a search excerpt
/// reads: the message the way the reader composed it, with every attachment
/// standing where it sat. `inline_text_offset` is in UTF-16 units into `text`,
/// the same units every client writes, and an attachment that carries no offset
/// is appended in the order it was sent — which is where a reader that cannot
/// place it has always found it.
///
/// A message that is only pictures therefore projects to their labels rather
/// than to nothing, which is what keeps a pasted screenshot from leaving a
/// blank row behind.
pub fn user_message_display_text(text: &str, attachments: &[MessageAttachment]) -> String {
    if attachments.is_empty() {
        return text.to_string();
    }
    // The offsets are the wire's, so they are sorted rather than trusted: a
    // message can carry several, and the text is walked once, left to right.
    let mut placed = attachments
        .iter()
        .enumerate()
        .filter_map(|(index, attachment)| {
            attachment
                .inline_text_offset
                .map(|offset| (offset as usize, index, attachment))
        })
        .collect::<Vec<_>>();
    placed.sort_by_key(|(offset, index, _)| (*offset, *index));

    let mut body = String::with_capacity(text.len() + attachments.len() * 12);
    let mut cursor = 0usize;
    for (offset, _, attachment) in placed {
        let byte = utf16_offset_to_byte(text, offset).clamp(cursor, text.len());
        body.push_str(&text[cursor..byte]);
        cursor = byte;
        push_display_word(&mut body, attachment.display_label());
        // What follows is appended after this, and a row draws the label as
        // prose: without a space of its own it fuses with the next word
        // (`shot.png世界`).
        if text[cursor..]
            .chars()
            .next()
            .is_some_and(|character| !character.is_whitespace())
        {
            body.push(' ');
        }
    }
    body.push_str(&text[cursor..]);
    for attachment in attachments
        .iter()
        .filter(|attachment| attachment.inline_text_offset.is_none())
    {
        push_display_word(&mut body, attachment.display_label());
    }
    body
}

/// Append a word, one space apart from whatever it follows.
///
/// The wire text separates a picture from its neighbours the way the client
/// that wrote it did, so the space a reader expects on the left is supplied
/// here when the text before it does not already end in whitespace.
fn push_display_word(body: &mut String, word: &str) {
    if !body.is_empty() && !body.ends_with(char::is_whitespace) {
        body.push(' ');
    }
    body.push_str(word);
}

/// The byte index in `text` of a UTF-16 offset, clamped to its length.
///
/// `inline_text_offset` is counted in UTF-16 units because that is what the
/// other clients count in; the buffer counts bytes, and a message full of CJK
/// is where the two disagree.
pub fn utf16_offset_to_byte(text: &str, offset: usize) -> usize {
    if offset == 0 {
        return 0;
    }
    let mut utf16 = 0usize;
    for (byte, character) in text.char_indices() {
        let next = utf16 + character.len_utf16();
        if offset < next {
            return byte;
        }
        if offset == next {
            return byte + character.len_utf8();
        }
        utf16 = next;
    }
    text.len()
}

/// How a user message reached the Agent's turn.
///
/// The timeline keeps the delivery because it is the only durable evidence of
/// whether a message opened its own turn, interrupted the running one, or was
/// injected into it. Readers that predate the field see [`Self::Prompt`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UserMessageDelivery {
    /// An ordinary prompt that claimed its own turn.
    #[default]
    Prompt,
    /// A queued message injected into the turn that was already running.
    Steer,
    /// A queued message that interrupted the running turn and claimed a new one.
    Resend,
}

impl UserMessageDelivery {
    pub fn is_prompt(&self) -> bool {
        *self == Self::Prompt
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct UserMessagePayload {
    pub text: String,
    pub attachments: Vec<MessageAttachment>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub mentions: Vec<crate::VibexUseMention>,
    #[serde(default, skip_serializing_if = "UserMessageDelivery::is_prompt")]
    pub delivery: UserMessageDelivery,
    /// Who actually authored this message.
    ///
    /// A delegated instruction reaches the provider through the same prompt as
    /// a typed one; without this the transcript would read as if the user had
    /// written their Agent's follow-up themselves. Old rows deserialize as
    /// `legacy_unknown` rather than being guessed from their text.
    #[serde(default)]
    pub provenance: crate::MessageProvenance,
}

impl Default for UserMessagePayload {
    fn default() -> Self {
        Self {
            text: String::new(),
            attachments: Vec::new(),
            mentions: Vec::new(),
            delivery: UserMessageDelivery::Prompt,
            provenance: crate::MessageProvenance::LegacyUnknown,
        }
    }
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileOperationPatchFormat {
    UnifiedDiffV1,
    #[serde(other)]
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FileOperationPatch {
    pub format: FileOperationPatchFormat,
    pub text: String,
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
    pub patch: Option<FileOperationPatch>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raw_extension: Option<AgentEventRawExtension>,
}

impl FileOperationPayload {
    /// The patch this operation is stored with.
    ///
    /// A durable timeline keeps the patch rather than both file snapshots:
    /// the snapshots are exactly as large as the files they describe, while
    /// the patch is what a reader renders and is usually a small fraction of
    /// it. A one-sided change — a file that did not exist before, or was
    /// removed — has no diff to compute, so its patch is synthesized from the
    /// side that does exist. Without that, storing patches would silently
    /// discard the content of every file an Agent created.
    pub fn canonical_patch(&self) -> Option<FileOperationPatch> {
        if let Some(patch) = &self.patch {
            return Some(patch.clone());
        }
        let (old_text, new_text) = (self.old_text.as_deref(), self.new_text.as_deref());
        if old_text.is_some() && new_text.is_some() {
            return self.generated_patch();
        }
        let text = one_sided_patch(old_text.unwrap_or_default(), new_text.unwrap_or_default());
        (!text.is_empty()).then_some(FileOperationPatch {
            format: FileOperationPatchFormat::UnifiedDiffV1,
            text,
        })
    }

    pub fn generated_patch(&self) -> Option<FileOperationPatch> {
        let (Some(old_text), Some(new_text)) = (self.old_text.as_deref(), self.new_text.as_deref())
        else {
            return None;
        };
        if old_text == new_text {
            return None;
        }
        let patch = TextDiff::configure()
            .timeout(Duration::from_millis(500))
            .diff_lines(old_text, new_text)
            .unified_diff()
            .context_radius(3)
            .header("old", "new")
            .to_string();
        (!patch.is_empty()).then_some(FileOperationPatch {
            format: FileOperationPatchFormat::UnifiedDiffV1,
            text: patch,
        })
    }
}

/// Builds the unified diff for a change that only has one side.
///
/// The result uses the same `old`/`new` header pair as
/// [`FileOperationPayload::generated_patch`], so a reader parses both the same
/// way.
fn one_sided_patch(old_text: &str, new_text: &str) -> String {
    let added = old_text.is_empty() && !new_text.is_empty();
    let removed = new_text.is_empty() && !old_text.is_empty();
    if !added && !removed {
        return String::new();
    }
    let body = if added { new_text } else { old_text };
    let lines = body.lines().collect::<Vec<_>>();
    let (from, to) = if added {
        ("-0,0".to_string(), format!("+1,{}", lines.len()))
    } else {
        (format!("-1,{}", lines.len()), "+0,0".to_string())
    };
    let marker = if added { '+' } else { '-' };
    let mut patch = format!("--- old\n+++ new\n@@ {from} {to} @@\n");
    for line in lines {
        patch.push(marker);
        patch.push_str(line);
        patch.push('\n');
    }
    patch
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

    /// Whether this extension holds anything only an opened activity renders.
    pub fn carries_tool_detail(&self) -> bool {
        !self.content_blocks.is_empty() || self.raw_output.is_some() || !self.meta.is_empty()
    }

    /// Keeps the invocation and its locations, drops the opened-activity parts.
    ///
    /// `raw_input` and `locations` are what a collapsed activity row uses to
    /// name its action and target; `content_blocks`, `raw_output`, and `meta`
    /// are only read after a reader expands the activity.
    pub fn into_summary(mut self) -> Self {
        self.content_blocks.clear();
        self.raw_output = None;
        self.meta.clear();
        self
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
    /// Present only for a product-managed child Agent session. Provider-native
    /// collaboration events intentionally leave these fields absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delegation_id: Option<crate::AgentDelegationId>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_session_id: Option<crate::VibexSessionId>,
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

/// The notice a session appends as it is created, before it can answer.
///
/// The wording lives here rather than at the one call site so the client that
/// leaves the notice out of its conversation reads the same string the runtime
/// writes — see [`is_runtime_startup_message`].
pub const SESSION_INITIALIZING_NOTICE: &str = "Agent session is initializing";

/// What every turn-startup notice opens with, and what every one ends with.
///
/// The predicate and the builder share these, so a rewording cannot leave one
/// of them behind.
const TURN_STARTUP_HEAD: &str = "Starting ";
const TURN_STARTUP_TAIL: &str = "waiting for first response...";

/// The notice a turn appends as it starts its provider runtime.
///
/// `provider_label` names the runtime, and `resource_summary` is what it
/// prepares the context from, when there is anything to name.
pub fn turn_startup_notice(provider_label: &str, resource_summary: Option<&str>) -> String {
    let context = match resource_summary {
        Some(summary) => format!("preparing context with {summary}; "),
        None => String::new(),
    };
    format!("{TURN_STARTUP_HEAD}{provider_label} agent runtime; {context}{TURN_STARTUP_TAIL}")
}

/// Whether a system notice announces that the runtime is starting up.
///
/// A session writes [`SESSION_INITIALIZING_NOTICE`] as it is created, and
/// [`turn_startup_notice`] at the top of each turn. Both say that work is under
/// way; neither holds a word the Agent said. A client that draws the
/// conversation can leave them to its own progress line, and a client that
/// counts the reader's turns must not count one of them as a turn.
pub fn is_runtime_startup_message(message: &str) -> bool {
    let message = message.trim();
    message == SESSION_INITIALIZING_NOTICE
        || (message.starts_with(TURN_STARTUP_HEAD) && message.ends_with(TURN_STARTUP_TAIL))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SystemNoticePayload {
    pub level: SystemNoticeLevel,
    pub message: String,
}

impl SystemNoticePayload {
    /// Returns whether this notice announces the runtime's own startup.
    pub fn is_runtime_startup(&self) -> bool {
        is_runtime_startup_message(&self.message)
    }

    /// Returns whether this notice records a provider-neutral context compaction.
    ///
    /// ACP currently surfaces compaction as a bounded system notice rather than
    /// a dedicated wire event, so adapters normalize the common notice wording
    /// here before consumers project it into metrics or UI.
    pub fn is_context_compaction(&self) -> bool {
        let normalized = self
            .message
            .trim()
            .trim_start_matches(['*', '-'])
            .trim()
            .trim_end_matches(['.', ':'])
            .trim();
        normalized.eq_ignore_ascii_case("context compacted")
            || normalized.eq_ignore_ascii_case("context window compacted")
            || normalized.eq_ignore_ascii_case("context compaction")
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineErrorPayload {
    pub code: String,
    pub message: String,
    pub recoverable: bool,
}

/// Provider-neutral description of an automatic retry performed inside an
/// Agent turn. ACP has no standard retry notification, so adapters normalize
/// their native extensions and progress text into this payload.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryKind {
    ModelRequest,
    StreamReconnect,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetryPhase {
    Started,
    Recovered,
    Exhausted,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct AgentRetryPayload {
    pub kind: RetryKind,
    pub phase: RetryPhase,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub attempt: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_attempts: Option<u32>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub delay_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", content = "data", rename_all = "snake_case")]
pub enum TimelinePayload {
    UserMessage(UserMessagePayload),
    AgentMessageDelta(AgentMessageDeltaPayload),
    AgentMessage(AgentMessagePayload),
    Reasoning(ReasoningPayload),
    Plan(PlanPayload),
    Goal(crate::goal::GoalPayload),
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
    Retry(AgentRetryPayload),
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
            Self::Goal(_) => TimelineItemKind::Goal,
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
            Self::Retry(_) => TimelineItemKind::Retry,
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
    /// Which depth `payload` was read at. A `Summary` item still renders the
    /// whole conversation; only its tool detail is missing, and a reader that
    /// opens that detail fetches the same item at `Detail` depth.
    #[serde(default)]
    pub payload_depth: TimelinePayloadDepth,
    pub payload: TimelinePayload,
}

impl TimelineItemKind {
    /// The kinds whose items can repeat as updates of one operation.
    ///
    /// A provider narrates an operation as a run of items — a tool call
    /// streams its growing input, a search its results — where each update is
    /// its own item and only the newest one describes the operation. These are
    /// the kinds that fold; every other kind is a single event that stands on
    /// its own.
    pub const FOLDABLE_REPEATED_UPDATES: [Self; 7] = [
        Self::ToolCall,
        Self::Command,
        Self::FileOperation,
        Self::WebSearch,
        Self::TodoUpdate,
        Self::Collaboration,
        Self::ImageGeneration,
    ];

    /// Whether items of this kind can repeat as updates of one operation.
    pub const fn folds_repeated_updates(self) -> bool {
        matches!(
            self,
            Self::ToolCall
                | Self::Command
                | Self::FileOperation
                | Self::WebSearch
                | Self::TodoUpdate
                | Self::Collaboration
                | Self::ImageGeneration
        )
    }
}

impl TimelineItem {
    /// The identity that makes repeated updates of one operation one row.
    ///
    /// The projection folds a turn's updates of one operation into a single
    /// row, and the durable timeline can collapse them for exactly the same
    /// reason: they are one operation, and only the newest item describes it.
    /// Both sides read this function, so what a reader sees and what is kept
    /// on disk cannot drift apart — a key added here is folded by both, and a
    /// kind missing from it is folded by neither.
    ///
    /// `None` means the item is not a repeated update; it is its own row.
    pub fn process_compaction_key(&self) -> Option<String> {
        if !self.kind.folds_repeated_updates() {
            return None;
        }
        match &self.payload {
            TimelinePayload::ToolCall(tool) => Some(format!("tool:{}", tool.tool_call_id)),
            TimelinePayload::Command(command) => Some(format!(
                "command:{}:{}",
                command.cwd.as_deref().unwrap_or_default(),
                command.command
            )),
            TimelinePayload::FileOperation(operation) => {
                Some(format!("file:{:?}:{}", operation.operation, operation.path))
            }
            TimelinePayload::WebSearch(search) => Some(format!("web:{}", search.query)),
            TimelinePayload::TodoUpdate(todo) => Some(format!("todo:{}", todo.title)),
            TimelinePayload::Collaboration(collaboration) => Some(format!(
                "collaboration:{}",
                self.provider_correlation_id
                    .clone()
                    .unwrap_or_else(|| format!(
                        "{}:{}",
                        collaboration.action,
                        collaboration.agent_label.as_deref().unwrap_or_default()
                    ))
            )),
            TimelinePayload::ImageGeneration(_) => Some(format!(
                "image:{}",
                self.provider_correlation_id
                    .as_deref()
                    .unwrap_or(self.id.as_str())
            )),
            _ => None,
        }
    }
}

impl TimelineItem {
    /// Whether this item already carries its complete payload.
    pub fn has_complete_payload(&self) -> bool {
        self.payload_depth == TimelinePayloadDepth::Detail
    }

    /// Drops the unbounded tool detail, keeping every conversation field.
    ///
    /// Returns `self` unchanged when the payload carries no such detail, so a
    /// page of ordinary messages is not rewritten item by item.
    pub fn into_summary(mut self) -> Self {
        if self.payload_depth == TimelinePayloadDepth::Summary {
            return self;
        }
        if !self.payload.carries_tool_detail() {
            // Nothing was withheld, so the payload is already complete. Saying
            // otherwise would make a reader ask for detail that does not
            // exist, and would hide an activity that genuinely has nothing to
            // open behind a promise of more.
            self.payload_depth = TimelinePayloadDepth::Detail;
            return self;
        }
        self.payload = self.payload.into_summary();
        self.payload_depth = TimelinePayloadDepth::Summary;
        self
    }

    /// A summary for a client that pays for every byte on the wire.
    ///
    /// The ordinary summary keeps a user message's inline `data:` attachment
    /// because the desktop can read it straight from the item. A paired phone
    /// cannot, and would download the picture with every timeline window, so
    /// the remote read replaces the payload with a bounded marker. Opening the
    /// attachment reads the one detail item, which still carries the bytes.
    pub fn into_remote_summary(self) -> Self {
        let mut item = self.into_summary();
        let mut stripped = false;
        if let TimelinePayload::UserMessage(message) = &mut item.payload {
            for (index, attachment) in message.attachments.iter_mut().enumerate() {
                if attachment
                    .uri
                    .as_deref()
                    .is_some_and(|uri| uri.starts_with("data:"))
                {
                    attachment.uri = Some(format!("{INLINE_ATTACHMENT_URI_PREFIX}{index}"));
                    stripped = true;
                }
            }
        }
        if stripped {
            item.payload_depth = TimelinePayloadDepth::Summary;
        }
        item
    }
}

/// The URI a summary read puts in place of an inline `data:` attachment.
///
/// The bytes stay in the authoritative detail row; a summary page only names
/// the attachment and its position, so a client that wants the picture asks
/// for that one item instead of downloading it with every timeline window.
pub const INLINE_ATTACHMENT_URI_PREFIX: &str = "vibex-inline-attachment://";

pub fn inline_attachment_index(uri: &str) -> Option<usize> {
    uri.strip_prefix(INLINE_ATTACHMENT_URI_PREFIX)
        .and_then(|index| index.parse().ok())
}

impl TimelinePayload {
    /// Whether this payload holds fields that [`Self::into_summary`] removes.
    pub fn carries_tool_detail(&self) -> bool {
        match self {
            Self::ToolCall(tool) => tool
                .raw_extension
                .as_ref()
                .is_some_and(AgentEventRawExtension::carries_tool_detail),
            Self::Command(command) => command
                .raw_extension
                .as_ref()
                .is_some_and(AgentEventRawExtension::carries_tool_detail),
            Self::FileOperation(file) => {
                file.old_text.is_some()
                    || file.new_text.is_some()
                    || file.patch.is_some()
                    || file
                        .raw_extension
                        .as_ref()
                        .is_some_and(AgentEventRawExtension::carries_tool_detail)
            }
            Self::WebSearch(search) => search
                .raw_extension
                .as_ref()
                .is_some_and(AgentEventRawExtension::carries_tool_detail),
            Self::ImageGeneration(image) => image
                .raw_extension
                .as_ref()
                .is_some_and(AgentEventRawExtension::carries_tool_detail),
            _ => false,
        }
    }

    /// Removes the unbounded tool detail from this payload.
    ///
    /// What survives is exactly what a collapsed activity row renders: the
    /// action, its target, the bounded invocation source, and the file path.
    /// Everything a reader only sees after opening the activity — captured
    /// output, content blocks, metadata, and file snapshots — is dropped.
    pub fn into_summary(self) -> Self {
        match self {
            Self::ToolCall(mut tool) => {
                tool.raw_extension = tool.raw_extension.map(AgentEventRawExtension::into_summary);
                Self::ToolCall(tool)
            }
            Self::Command(mut command) => {
                command.raw_extension = command
                    .raw_extension
                    .map(AgentEventRawExtension::into_summary);
                Self::Command(command)
            }
            Self::WebSearch(mut search) => {
                search.raw_extension = search
                    .raw_extension
                    .map(AgentEventRawExtension::into_summary);
                Self::WebSearch(search)
            }
            Self::ImageGeneration(mut image) => {
                image.raw_extension = image
                    .raw_extension
                    .map(AgentEventRawExtension::into_summary);
                Self::ImageGeneration(image)
            }
            Self::FileOperation(mut file) => {
                file.old_text = None;
                file.new_text = None;
                file.patch = None;
                file.raw_extension = file.raw_extension.map(AgentEventRawExtension::into_summary);
                Self::FileOperation(file)
            }
            other => other,
        }
    }

    /// A cheap estimate of the memory and wire cost this payload carries.
    ///
    /// String bytes dominate every payload, so summing them (plus a small
    /// per-value overhead for the surrounding JSON) tracks both the resident
    /// cost a cache holds and the size a page spends against its byte budget.
    /// It is deliberately allocation-free: readers call it per item per page.
    pub fn approximate_bytes(&self) -> usize {
        /// The JSON scaffolding around one payload value.
        const ENVELOPE_BYTES: usize = 64;
        /// The JSON scaffolding around one array element or map entry.
        const ELEMENT_BYTES: usize = 24;
        fn text(value: &str) -> usize {
            value.len()
        }
        fn optional(value: &Option<String>) -> usize {
            value.as_deref().map_or(0, text)
        }
        fn raw(extension: &Option<AgentEventRawExtension>) -> usize {
            extension.as_ref().map_or(0, |extension| {
                extension.raw_input.as_deref().map_or(0, text)
                    + extension
                        .raw_output
                        .as_ref()
                        .map_or(0, |output| text(&output.text))
                    + extension
                        .content_blocks
                        .iter()
                        .map(|block| text(&block.block_type) + text(&block.summary) + ELEMENT_BYTES)
                        .sum::<usize>()
                    + extension
                        .locations
                        .iter()
                        .map(|location| text(&location.uri) + ELEMENT_BYTES)
                        .sum::<usize>()
                    + extension
                        .meta
                        .iter()
                        .map(|(key, value)| text(key) + text(value) + ELEMENT_BYTES)
                        .sum::<usize>()
            })
        }
        let body = match self {
            Self::UserMessage(message) => {
                text(&message.text)
                    + message
                        .attachments
                        .iter()
                        .map(|attachment| {
                            text(&attachment.label)
                                + optional(&attachment.mime_type)
                                + optional(&attachment.uri)
                                + ELEMENT_BYTES
                        })
                        .sum::<usize>()
            }
            Self::AgentMessageDelta(delta) => text(&delta.text_delta),
            Self::AgentMessage(message) => text(&message.text),
            Self::Reasoning(reasoning) => text(&reasoning.text),
            Self::Plan(plan) => {
                text(&plan.title)
                    + plan
                        .steps
                        .iter()
                        .map(|step| text(&step.title) + ELEMENT_BYTES)
                        .sum::<usize>()
            }
            Self::Goal(goal) => text(&goal.message.clone().unwrap_or_default()),
            Self::ToolCall(tool) => {
                text(&tool.tool_call_id)
                    + text(&tool.tool_name)
                    + text(&tool.summary)
                    + optional(&tool.input_summary)
                    + optional(&tool.output_summary)
                    + raw(&tool.raw_extension)
            }
            Self::Command(command) => {
                text(&command.command)
                    + optional(&command.cwd)
                    + optional(&command.output_summary)
                    + raw(&command.raw_extension)
            }
            Self::FileOperation(file) => {
                text(&file.path)
                    + text(&file.summary)
                    + optional(&file.old_text)
                    + optional(&file.new_text)
                    + file.patch.as_ref().map_or(0, |patch| text(&patch.text))
                    + raw(&file.raw_extension)
            }
            Self::WebSearch(search) => {
                text(&search.query) + optional(&search.result_summary) + raw(&search.raw_extension)
            }
            Self::TodoUpdate(todo) => {
                text(&todo.title)
                    + todo
                        .items
                        .iter()
                        .map(|item| text(&item.title) + ELEMENT_BYTES)
                        .sum::<usize>()
                    + raw(&todo.raw_extension)
            }
            Self::Collaboration(collaboration) => {
                text(&collaboration.action)
                    + text(&collaboration.summary)
                    + optional(&collaboration.agent_label)
                    + raw(&collaboration.raw_extension)
            }
            Self::ImageGeneration(image) => {
                text(&image.summary)
                    + optional(&image.mime_type)
                    + optional(&image.image_reference)
                    + raw(&image.raw_extension)
            }
            Self::GitNotice(notice) => text(&notice.summary),
            Self::SystemNotice(notice) => text(&notice.message),
            Self::Error(error) => text(&error.code) + text(&error.message),
            Self::Retry(retry) => optional(&retry.reason),
            // Interactive requests carry bounded titles and options; they are
            // never a page's cost driver.
            Self::PermissionRequest(_) | Self::ElicitationRequest(_) => 256,
            Self::PermissionResolution(_) | Self::ElicitationResolution(_) => 0,
        };
        body.saturating_add(ENVELOPE_BYTES)
    }

    /// The inline attachment bytes a paired client never receives.
    ///
    /// These are not "tool detail": the local summary keeps them on purpose.
    /// The page budget still has to discount them, or one pasted photograph
    /// would consume the whole window that a phone then receives without it.
    pub fn stripped_inline_attachment_bytes(&self) -> usize {
        match self {
            Self::UserMessage(message) => message
                .attachments
                .iter()
                .filter(|attachment| {
                    attachment
                        .uri
                        .as_deref()
                        .is_some_and(|uri| uri.starts_with("data:"))
                })
                .filter_map(|attachment| attachment.uri.as_deref())
                .map(str::len)
                .sum(),
            _ => 0,
        }
    }

    /// The bytes [`Self::into_summary`] would remove from this payload.
    pub fn stripped_tool_detail_bytes(&self) -> usize {
        let complete = self.approximate_bytes();
        if !self.carries_tool_detail() {
            return 0;
        }
        complete.saturating_sub(self.clone().into_summary().approximate_bytes())
    }
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

    // An exhausted automatic retry is a terminal failure for the current
    // conversational turn, even if a late provider snapshot also carries a
    // synthetic final message. Otherwise the UI could hide continuation after
    // the retry budget was already spent.
    if turn_items.iter().any(|item| {
        matches!(
            &item.payload,
            TimelinePayload::Retry(AgentRetryPayload {
                phase: RetryPhase::Exhausted,
                ..
            })
        )
    }) {
        return Some(false);
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
    /// Whether the page's oldest item begins a conversational turn.
    ///
    /// A reader prepends a page above what it already holds, so a page that
    /// opens inside a turn hands that turn over in pieces: the rows added below
    /// it are older than the ones on screen, and the turn's own header — when it
    /// started, how long it worked — moves under the reader. A read looks for a
    /// turn boundary for this end, and this says whether it found one: the one
    /// turn a page opens in is the one a client has to be careful about.
    #[serde(default = "timeline_page_starts_at_turn_start_default")]
    pub starts_at_turn_start: bool,
    /// Where the turn this page opens inside begins, when it opens inside one.
    ///
    /// [`Self::starts_at_turn_start`] only says the oldest row is not a turn
    /// boundary. This says which turn it is inside and when that turn began,
    /// which is the anchor its header counts from: the page's own oldest row is
    /// somewhere in the middle of that turn, so a reader that only had the rows
    /// would date the turn from a row that is not its start — and move the
    /// number once the older rows arrive.
    ///
    /// The pair travels with the page because only the read that produced the
    /// page knows it, and it is one indexed probe where the page was cut
    /// anyway. It is `None` for a page that opens on a turn boundary, and for a
    /// peer too old to report one.
    #[serde(default)]
    pub leading_turn_start: Option<TimelineTurnStart>,
    /// Rows this page deliberately skipped even though the store still holds
    /// them.
    ///
    /// A client has two ways to learn that a window is not the whole history:
    /// `has_older` says something sits below the oldest row, and `has_newer`
    /// says something sits above the newest one. Neither can describe a window
    /// missing rows *in the middle* of its own span, and that is what a page
    /// assembled from two bands is: the newest rows of a live turn plus the
    /// turns above it, with the live turn's middle left out because the stream
    /// writing it was going to deliver that part.
    ///
    /// Only the read knows which of those rows exist, so only the read can say
    /// so. A client that installs such a page as a complete projection shows a
    /// conversation with a hole in it and — because `has_older` is false at the
    /// session's first row — never fetches the missing part back.
    ///
    /// `None` means the page omitted nothing the store holds, and is also what
    /// a peer too old to report a gap sends. A client must read it as "nothing
    /// known to be missing": proof of contiguity is the gap being absent, not
    /// the field being absent.
    #[serde(default)]
    pub omitted: Option<TimelineSequenceGap>,
}

/// A range of stored rows a page did not carry.
///
/// The bounds are the sequences of the rows on either side of the omission, so
/// the range itself is exclusive: the missing rows are those with
/// `after < sequence < before`. Both ends are rows the reader already has,
/// which is what makes the gap addressable — a read addressed `before` the
/// upper bound lands on exactly the rows this page skipped.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineSequenceGap {
    /// The newest row below the omission, carried by the page.
    pub after: i64,
    /// The oldest row above the omission, carried by the page.
    pub before: i64,
}

impl TimelineSequenceGap {
    /// Whether this describes a range that actually skips stored rows.
    ///
    /// Adjacent sequences leave nothing out, so a page that reports such a
    /// pair is making no claim at all and should not be treated as partial.
    pub fn is_empty(&self) -> bool {
        self.before <= self.after.saturating_add(1)
    }
}

/// The first row of the turn a page opens inside.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct TimelineTurnStart {
    /// The sequence the turn begins at.
    pub sequence: i64,
    /// When the turn began, read from that row exactly as the turn projection
    /// reads it, so a header built from this and a header built from the row
    /// itself agree.
    pub timestamp_ms: i64,
}

/// Assume a boundary when a peer does not report one.
///
/// An older peer cuts pages wherever its budget ended; treating its pages as cut
/// would hold a reader in a loading state their history could never leave, so a
/// missing answer reads as the behavior that peer had.
fn timeline_page_starts_at_turn_start_default() -> bool {
    true
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

    fn attachment(label: &str, offset: Option<u32>) -> MessageAttachment {
        MessageAttachment {
            label: label.to_string(),
            mime_type: Some("image/png".to_string()),
            uri: Some("file:///tmp/shot.png".to_string()),
            inline_text_offset: offset,
        }
    }

    #[test]
    fn a_message_of_only_pictures_projects_to_their_labels() {
        // The case the sidebar showed a blank row for: a pasted image and no
        // prose. The label is what the writer saw in the composer.
        let text = user_message_display_text(
            "",
            &[
                attachment("粘贴的图片.png", None),
                attachment("shot.png", None),
            ],
        );
        assert_eq!(text, "粘贴的图片.png shot.png");
    }

    #[test]
    fn a_picture_sits_where_it_was_written() {
        let text = user_message_display_text("before  after", &[attachment("shot.png", Some(7))]);
        assert_eq!(text, "before shot.png after");
    }

    #[test]
    fn an_offset_is_counted_in_utf16_units() {
        let text = user_message_display_text("你好 世界", &[attachment("shot.png", Some(3))]);
        assert_eq!(text, "你好 shot.png 世界");
    }

    #[test]
    fn an_attachment_without_a_label_is_named_by_what_it_is() {
        let unnamed = MessageAttachment {
            label: String::new(),
            mime_type: Some("image/png".to_string()),
            uri: None,
            inline_text_offset: None,
        };
        assert_eq!(user_message_display_text("look", &[unnamed]), "look Image");

        let file = MessageAttachment {
            label: "  ".to_string(),
            mime_type: Some("text/plain".to_string()),
            uri: Some("file:///tmp/notes.txt".to_string()),
            inline_text_offset: None,
        };
        assert_eq!(
            user_message_display_text("read", &[file]),
            "read Attachment"
        );
    }

    #[test]
    fn a_message_with_no_attachments_is_its_own_text() {
        assert_eq!(user_message_display_text("plain text", &[]), "plain text");
    }

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
            payload_depth: crate::TimelinePayloadDepth::Detail,
            payload,
        }
    }

    #[test]
    fn a_summary_item_keeps_the_conversation_and_drops_the_tool_detail() {
        let session_id = VibexSessionId::new();
        let mut item = TimelineItem {
            id: TimelineItemId::new(),
            session_id,
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::Provider,
            kind: TimelineItemKind::FileOperation,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: TimelinePayloadDepth::Detail,
            payload: TimelinePayload::FileOperation(FileOperationPayload {
                operation: FileOperationKind::Write,
                path: "src/lib.rs".into(),
                summary: "Created src/lib.rs".into(),
                old_text: None,
                new_text: Some("x".repeat(64 * 1024)),
                patch: None,
                raw_extension: Some(AgentEventRawExtension::new(
                    vec![AgentEventContentBlock::new("text", "block").0],
                    Some("{\"path\":\"src/lib.rs\"}".into()),
                    None,
                    Vec::new(),
                    BTreeMap::new(),
                    false,
                )),
            }),
        };
        let complete_bytes = item.payload.approximate_bytes();

        item = item.into_summary();
        assert_eq!(item.payload_depth, TimelinePayloadDepth::Summary);
        assert!(!item.has_complete_payload());
        assert!(item.payload.approximate_bytes() < complete_bytes / 8);
        let TimelinePayload::FileOperation(file) = &item.payload else {
            panic!("the payload kind survives a summary");
        };
        assert_eq!(file.path, "src/lib.rs");
        assert_eq!(file.old_text, None);
        assert_eq!(file.new_text, None);
        let extension = file
            .raw_extension
            .as_ref()
            .expect("the invocation survives a summary");
        assert!(extension.content_blocks.is_empty());
        assert!(extension.raw_input.is_some());
    }

    #[test]
    fn a_summary_names_an_inline_attachment_without_its_payload() {
        let session = VibexSessionId::new();
        let item = TimelineItem {
            id: TimelineItemId::new(),
            session_id: session,
            sequence: 1,
            timestamp_ms: 1,
            source: TimelineSource::User,
            kind: TimelineItemKind::UserMessage,
            correlation_id: None,
            provider_correlation_id: None,
            redaction_state: TimelineRedactionState::None,
            execution_attribution: None,
            payload_depth: TimelinePayloadDepth::Detail,
            payload: TimelinePayload::UserMessage(UserMessagePayload {
                text: "look".into(),
                attachments: vec![MessageAttachment {
                    label: "shot.png".into(),
                    mime_type: Some("image/png".into()),
                    uri: Some(format!("data:image/png;base64,{}", "A".repeat(4096))),
                    inline_text_offset: None,
                }],
                ..Default::default()
            }),
        };
        let complete_bytes = item.payload.approximate_bytes();

        // The desktop reads the local summary straight from the item and can
        // still open the picture, so the ordinary summary keeps the bytes.
        let local = item.clone().into_summary();
        assert!(local.has_complete_payload());
        let TimelinePayload::UserMessage(local_message) = &local.payload else {
            panic!("the payload kind survives a summary");
        };
        assert!(
            local_message.attachments[0]
                .uri
                .as_deref()
                .is_some_and(|uri| uri.starts_with("data:"))
        );

        // A paired phone gets the marker instead; the detail read still has
        // the payload.
        let remote = item.into_remote_summary();
        assert_eq!(remote.payload_depth, TimelinePayloadDepth::Summary);
        let TimelinePayload::UserMessage(message) = &remote.payload else {
            panic!("the payload kind survives a summary");
        };
        assert_eq!(
            message.attachments[0].uri.as_deref(),
            Some("vibex-inline-attachment://0")
        );
        assert_eq!(
            inline_attachment_index("vibex-inline-attachment://0"),
            Some(0)
        );
        assert!(
            remote.payload.approximate_bytes() < complete_bytes / 2,
            "the remote summary must not carry the inline payload"
        );
    }

    #[test]
    fn a_payload_with_nothing_to_withhold_stays_complete() {
        // Messages carry no tool detail, so marking them incomplete would make
        // a reader fetch detail that does not exist and hide the fact that the
        // item is already whole.
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
            execution_attribution: None,
            payload_depth: TimelinePayloadDepth::Detail,
            payload: TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "done".into(),
                is_final: true,
            }),
        };
        let summarized = item.into_summary();
        assert!(summarized.has_complete_payload());
        assert_eq!(summarized.payload_depth, TimelinePayloadDepth::Detail);
    }

    #[test]
    fn a_one_sided_file_change_still_gets_a_rendered_patch() {
        // Storing patches instead of snapshots must not lose the content of a
        // file an Agent created: there is no diff to compute against nothing,
        // so the patch is synthesized from the side that exists.
        let created = FileOperationPayload {
            operation: FileOperationKind::Write,
            path: "src/new.rs".into(),
            summary: "Created src/new.rs".into(),
            old_text: None,
            new_text: Some("fn main() {}\n".into()),
            patch: None,
            raw_extension: None,
        };
        let patch = created.canonical_patch().expect("a creation renders");
        assert!(patch.text.contains("+fn main() {}"), "{}", patch.text);

        let deleted = FileOperationPayload {
            operation: FileOperationKind::Delete,
            path: "src/old.rs".into(),
            summary: "Deleted src/old.rs".into(),
            old_text: Some("fn gone() {}\n".into()),
            new_text: None,
            patch: None,
            raw_extension: None,
        };
        let patch = deleted.canonical_patch().expect("a deletion renders");
        assert!(patch.text.contains("-fn gone() {}"), "{}", patch.text);

        let untouched = FileOperationPayload {
            operation: FileOperationKind::Edit,
            path: "src/same.rs".into(),
            summary: "Read src/same.rs".into(),
            old_text: None,
            new_text: None,
            patch: None,
            raw_extension: None,
        };
        assert!(untouched.canonical_patch().is_none());
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
                ..Default::default()
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

    #[test]
    fn retry_progress_is_not_a_turn_boundary_but_exhaustion_can_require_continuation() {
        let session_id = VibexSessionId::new();
        let user = completion_item(
            &session_id,
            1,
            TimelineSource::User,
            TimelinePayload::UserMessage(UserMessagePayload {
                text: "try again".into(),
                attachments: Vec::new(),
                ..Default::default()
            }),
        );
        let retry = completion_item(
            &session_id,
            2,
            TimelineSource::Provider,
            TimelinePayload::Retry(AgentRetryPayload {
                kind: RetryKind::ModelRequest,
                phase: RetryPhase::Started,
                attempt: Some(1),
                max_attempts: Some(3),
                delay_ms: None,
                reason: None,
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[user.clone(), retry.clone()]),
            Some(false)
        );

        let mut terminal_error = completion_item(
            &session_id,
            3,
            TimelineSource::Provider,
            TimelinePayload::Error(TimelineErrorPayload {
                code: "provider_retry_exhausted".into(),
                message: "retry budget exhausted".into(),
                recoverable: true,
            }),
        );
        terminal_error.provider_correlation_id = None;
        assert_eq!(
            latest_timeline_turn_ended_normally(&[user, retry, terminal_error]),
            Some(false)
        );

        let exhausted = completion_item(
            &session_id,
            4,
            TimelineSource::Provider,
            TimelinePayload::Retry(AgentRetryPayload {
                kind: RetryKind::ModelRequest,
                phase: RetryPhase::Exhausted,
                attempt: Some(3),
                max_attempts: Some(3),
                delay_ms: None,
                reason: Some("retry budget exhausted".to_string()),
            }),
        );
        let late_final = completion_item(
            &session_id,
            5,
            TimelineSource::Agent,
            TimelinePayload::AgentMessage(AgentMessagePayload {
                text: "late synthetic completion".to_string(),
                is_final: true,
            }),
        );
        assert_eq!(
            latest_timeline_turn_ended_normally(&[
                completion_item(
                    &session_id,
                    1,
                    TimelineSource::User,
                    TimelinePayload::UserMessage(UserMessagePayload {
                        text: "try again".into(),
                        attachments: Vec::new(),
                        ..Default::default()
                    }),
                ),
                exhausted,
                late_final,
            ]),
            Some(false)
        );
    }

    #[test]
    fn system_notice_identifies_context_compaction() {
        let notice = |message: &str| SystemNoticePayload {
            level: SystemNoticeLevel::Info,
            message: message.into(),
        };

        assert!(notice("*Context compacted").is_context_compaction());
        assert!(notice("Context window compacted.").is_context_compaction());
        assert!(!notice("ACP agent switched to mode default").is_context_compaction());
    }

    #[test]
    fn runtime_startup_notices_are_recognizable_by_their_wording() {
        // The exact sentences the runtime writes, because the predicate is the
        // only thing a client can hide them by: a rewording that moves one and
        // not the other puts a turn the reader never took back on the timeline.
        assert_eq!(SESSION_INITIALIZING_NOTICE, "Agent session is initializing");
        assert_eq!(
            turn_startup_notice("ACP", Some("2 MCP servers")),
            "Starting ACP agent runtime; preparing context with 2 MCP servers; waiting for first response..."
        );
        assert_eq!(
            turn_startup_notice("Claude", None),
            "Starting Claude agent runtime; waiting for first response..."
        );

        assert!(is_runtime_startup_message(SESSION_INITIALIZING_NOTICE));
        assert!(is_runtime_startup_message(&turn_startup_notice(
            "ACP",
            Some("2 MCP servers and 1 skill/prompt")
        )));
        assert!(is_runtime_startup_message(&turn_startup_notice(
            "Codex", None
        )));

        // Everything else stays: the notices a reader does need to see, and
        // text that merely shares a word with the startup pair.
        for message in [
            "Context compacted",
            "Imported 3 local sessions as read-only",
            "Ignored unsupported ACP event kind: foo",
            "Starting a new turn without a runtime",
            "The runtime is waiting for first response...",
        ] {
            assert!(
                !is_runtime_startup_message(message),
                "{message} was classified as startup"
            );
        }

        let notice = SystemNoticePayload {
            level: SystemNoticeLevel::Info,
            message: turn_startup_notice("ACP", None),
        };
        assert!(notice.is_runtime_startup());
        assert!(!notice.is_context_compaction());
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
            payload_depth: crate::TimelinePayloadDepth::Detail,
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
    fn file_operation_patch_is_versioned_and_legacy_snapshots_remain_compatible() {
        let old_text = (0..200)
            .map(|index| format!("line-{index}"))
            .collect::<Vec<_>>()
            .join("\n");
        let new_text = old_text.replace("line-100", "line-100-edited");
        let operation = FileOperationPayload {
            operation: FileOperationKind::Edit,
            path: "src/lib.rs".to_string(),
            summary: "Edited src/lib.rs".to_string(),
            old_text: Some(old_text),
            new_text: Some(new_text),
            patch: None,
            raw_extension: None,
        };
        let patch = operation.generated_patch().expect("generated patch");
        assert_eq!(patch.format, FileOperationPatchFormat::UnifiedDiffV1);
        assert!(patch.text.contains("@@"));
        assert!(patch.text.contains("-line-100"));
        assert!(patch.text.contains("+line-100-edited"));

        let legacy: FileOperationPayload = serde_json::from_value(serde_json::json!({
            "operation": "edit",
            "path": "src/lib.rs",
            "summary": "legacy",
            "oldText": "before",
            "newText": "after"
        }))
        .unwrap();
        assert_eq!(legacy.old_text.as_deref(), Some("before"));
        assert_eq!(legacy.new_text.as_deref(), Some("after"));
        assert_eq!(legacy.patch, None);
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
                delegation_id: None,
                child_session_id: None,
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

    #[test]
    fn retry_payload_round_trips_with_optional_progress_fields() {
        let payload = TimelinePayload::Retry(AgentRetryPayload {
            kind: RetryKind::ModelRequest,
            phase: RetryPhase::Started,
            attempt: Some(2),
            max_attempts: Some(5),
            delay_ms: Some(4_000),
            reason: Some("provider returned 503".to_string()),
        });
        let encoded = serde_json::to_value(&payload).unwrap();
        assert_eq!(
            encoded,
            serde_json::json!({
                "type": "retry",
                "data": {
                    "kind": "model_request",
                    "phase": "started",
                    "attempt": 2,
                    "maxAttempts": 5,
                    "delayMs": 4000,
                    "reason": "provider returned 503"
                }
            })
        );
        assert_eq!(
            serde_json::from_value::<TimelinePayload>(encoded).unwrap(),
            payload
        );

        let sparse = serde_json::json!({
            "type": "retry",
            "data": {"kind": "unknown", "phase": "exhausted"}
        });
        let decoded = serde_json::from_value::<TimelinePayload>(sparse).unwrap();
        assert_eq!(
            decoded,
            TimelinePayload::Retry(AgentRetryPayload {
                kind: RetryKind::Unknown,
                phase: RetryPhase::Exhausted,
                attempt: None,
                max_attempts: None,
                delay_ms: None,
                reason: None,
            })
        );
    }
}

//! Provider-neutral ACP event normalization and exact-identity enrichers.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::Value;
use sha2::{Digest, Sha256};
use vibex_agent::ProviderEvent;
use vibex_core::{
    AgentEventContentBlock, AgentEventLocation, AgentEventRawExtension, AgentEventRawOutput,
    AgentMessagePayload, CollaborationPayload, CommandPayload, CommandStatus, FileOperationKind,
    FileOperationPayload, ImageGenerationPayload, PermissionRequest, PlanPayload, PlanStepPayload,
    PlanStepStatus, ReasoningPayload, SystemNoticePayload, TimelinePayload, TimelineRedactionState,
    TimelineSource, TodoUpdatePayload, ToolCallPayload, ToolCallStatus, WebSearchPayload,
};

use crate::registry::AgentEventEnricherKind;

const PUBLIC_EVENT_TEXT_LIMIT: usize = 512;
const EVENT_LOCATION_LIMIT: usize = 16;
const EVENT_META_LIMIT: usize = 16;
const CORRELATION_DOMAIN: &[u8] = b"vibex/acp/canonical-event/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
struct StructuredFileChange {
    operation: FileOperationKind,
    path: String,
    old_text: Option<String>,
    new_text: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AgentEventInputSource {
    Live,
    Transcript,
}

#[derive(Clone, PartialEq)]
pub struct AgentEventInput {
    pub source: AgentEventInputSource,
    pub compatibility_identity: String,
    pub native_event_id: String,
    pub tool_name: String,
    pub title: String,
    pub status: ToolCallStatus,
    pub raw_input: Option<Value>,
    pub output_summary: Option<String>,
    pub raw_output: Option<AgentEventRawOutput>,
    pub content: Option<Value>,
    pub locations: Vec<AgentEventLocation>,
    pub meta: BTreeMap<String, String>,
}

impl fmt::Debug for AgentEventInput {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("AgentEventInput")
            .field("source", &self.source)
            .field("compatibility_identity", &"<redacted>")
            .field("native_event_id", &"<redacted>")
            .field("tool_name", &self.tool_name)
            .field("title", &"<redacted>")
            .field("status", &self.status)
            .field("has_raw_input", &self.raw_input.is_some())
            .field("has_output_summary", &self.output_summary.is_some())
            .field(
                "raw_output_mode",
                &self.raw_output.as_ref().map(|value| value.mode),
            )
            .field("has_content", &self.content.is_some())
            .field("location_count", &self.locations.len())
            .field("meta_keys", &self.meta.keys().collect::<Vec<_>>())
            .finish()
    }
}

impl AgentEventInput {
    pub fn raw_extension(&self) -> Option<AgentEventRawExtension> {
        let (content_blocks, content_truncated) = content_blocks(self.content.as_ref());
        let extension = AgentEventRawExtension::new(
            content_blocks,
            self.raw_input.as_ref().map(stable_json_text),
            self.raw_output.clone(),
            self.locations.clone(),
            canonical_event_meta(&self.meta),
            content_truncated,
        );
        (!extension.is_empty()).then_some(extension)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CanonicalAgentEvent {
    AgentMessage(AgentMessagePayload),
    Reasoning(ReasoningPayload),
    Plan(PlanPayload),
    ToolCall(ToolCallPayload),
    CommandExecution(CommandPayload),
    FileOperation(FileOperationPayload),
    WebSearch(WebSearchPayload),
    TodoUpdate(TodoUpdatePayload),
    Collaboration(CollaborationPayload),
    ImageGeneration(ImageGenerationPayload),
    PermissionRequest(PermissionRequest),
    SystemNotice(SystemNoticePayload),
}

impl CanonicalAgentEvent {
    pub const fn kind_name(&self) -> &'static str {
        match self {
            Self::AgentMessage(_) => "agent_message",
            Self::Reasoning(_) => "reasoning",
            Self::Plan(_) => "plan",
            Self::ToolCall(_) => "tool_call",
            Self::CommandExecution(_) => "command_execution",
            Self::FileOperation(_) => "file_operation",
            Self::WebSearch(_) => "web_search",
            Self::TodoUpdate(_) => "todo_update",
            Self::Collaboration(_) => "collaboration",
            Self::ImageGeneration(_) => "image_generation",
            Self::PermissionRequest(_) => "permission_request",
            Self::SystemNotice(_) => "system_notice",
        }
    }

    pub fn into_timeline_payload(self) -> TimelinePayload {
        match self {
            Self::AgentMessage(payload) => TimelinePayload::AgentMessage(payload),
            Self::Reasoning(payload) => TimelinePayload::Reasoning(payload),
            Self::Plan(payload) => TimelinePayload::Plan(payload),
            Self::ToolCall(payload) => TimelinePayload::ToolCall(payload),
            Self::CommandExecution(payload) => TimelinePayload::Command(payload),
            Self::FileOperation(payload) => TimelinePayload::FileOperation(payload),
            Self::WebSearch(payload) => TimelinePayload::WebSearch(payload),
            Self::TodoUpdate(payload) => TimelinePayload::TodoUpdate(payload),
            Self::Collaboration(payload) => TimelinePayload::Collaboration(payload),
            Self::ImageGeneration(payload) => TimelinePayload::ImageGeneration(payload),
            Self::PermissionRequest(payload) => TimelinePayload::PermissionRequest(payload),
            Self::SystemNotice(payload) => TimelinePayload::SystemNotice(payload),
        }
    }

    fn source(&self) -> TimelineSource {
        match self {
            Self::PermissionRequest(_) | Self::SystemNotice(_) => TimelineSource::Provider,
            _ => TimelineSource::Agent,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NormalizedAgentEvent {
    pub event: CanonicalAgentEvent,
    pub provider_correlation_id: String,
}

impl NormalizedAgentEvent {
    pub fn into_provider_event(self) -> ProviderEvent {
        let source = self.event.source();
        ProviderEvent {
            source,
            payload: self.event.into_timeline_payload(),
            provider_correlation_id: Some(self.provider_correlation_id),
            redaction_state: TimelineRedactionState::None,
        }
    }
}

pub trait AgentEventEnricher: Send + Sync {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent>;
}

#[derive(Debug, Default)]
pub struct PassthroughEventEnricher;

impl AgentEventEnricher for PassthroughEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        vec![generic_tool_call(input)]
    }
}

#[derive(Debug, Default)]
pub struct ClaudeEventEnricher;

impl AgentEventEnricher for ClaudeEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        let kind = normalized_kind(&input.tool_name);
        if matches!(
            kind.as_str(),
            "claude_subagent"
                | "claude_background_task"
                | "claude_background_agent"
                | "claude_background_shell"
                | "claude_task_notification"
                | "claude_task_update"
        ) {
            return vec![collaboration_event(input)];
        }
        PassthroughEventEnricher.enrich(input)
    }
}

#[derive(Debug, Default)]
pub struct CodexEventEnricher;

impl AgentEventEnricher for CodexEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        let kind = normalized_kind(&input.tool_name);
        if let Some(files) = file_events(input) {
            return files;
        }
        if is_command_kind(&kind) && command_text(input.raw_input.as_ref()).is_some() {
            return vec![command_event(input)];
        }
        if matches!(kind.as_str(), "web_search" | "search_web") {
            return vec![web_search_event(input)];
        }
        if matches!(kind.as_str(), "todo" | "todo_list" | "todo_update")
            && todo_items(input.raw_input.as_ref()).is_some()
        {
            return vec![todo_event(input)];
        }
        if matches!(
            kind.as_str(),
            "collaboration" | "collab" | "spawn_agent" | "send_agent_message" | "wait_agent"
        ) {
            return vec![collaboration_event(input)];
        }
        if matches!(kind.as_str(), "image_generation" | "generate_image")
            || contains_image_block(input.content.as_ref())
        {
            return vec![image_event(input)];
        }
        PassthroughEventEnricher.enrich(input)
    }
}

#[derive(Debug, Default)]
pub struct GrokEventEnricher;

impl AgentEventEnricher for GrokEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        let kind = normalized_kind(&input.tool_name);
        // Grok reports plan progress and its interview tool as ordinary tool
        // calls named after the `_x.ai` extension. Passthrough would render
        // them as anonymous tool cards with the raw method name.
        if matches!(
            kind.as_str(),
            "x_ai_exit_plan_mode" | "exit_plan_mode" | "update_plan" | "plan_mode"
        ) && let Some(plan) = grok_plan_event(input)
        {
            return vec![plan];
        }
        if matches!(
            kind.as_str(),
            "x_ai_ask_user_question" | "ask_user_question" | "interview"
        ) {
            return vec![collaboration_event(input)];
        }
        if matches!(kind.as_str(), "task" | "subagent" | "spawn_subagent") {
            return vec![collaboration_event(input)];
        }
        if is_command_kind(&kind) && command_text(input.raw_input.as_ref()).is_some() {
            return vec![command_event(input)];
        }
        if let Some(files) = file_events(input) {
            return files;
        }
        PassthroughEventEnricher.enrich(input)
    }
}

/// Grok carries the plan as `planContent` markdown rather than the ACP step
/// array, so it becomes a single-step plan instead of an empty one.
fn grok_plan_event(input: &AgentEventInput) -> Option<CanonicalAgentEvent> {
    let raw_input = input.raw_input.as_ref()?;
    let plan = string_value(raw_input, &["planContent", "plan", "content"])?;
    Some(CanonicalAgentEvent::Plan(PlanPayload {
        title: public_text(if input.title.trim().is_empty() {
            "Plan"
        } else {
            &input.title
        }),
        steps: vec![PlanStepPayload {
            title: public_text(&plan),
            status: match input.status {
                ToolCallStatus::Started => PlanStepStatus::Pending,
                ToolCallStatus::Progress => PlanStepStatus::Running,
                ToolCallStatus::Completed => PlanStepStatus::Completed,
                ToolCallStatus::Failed => PlanStepStatus::Failed,
            },
        }],
    }))
}

#[derive(Debug, Default)]
pub struct CodeBuddyEventEnricher;

impl AgentEventEnricher for CodeBuddyEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        // Unwrap the virtualization layer first: everything downstream keys on
        // the real tool name.
        if let Some(unwrapped) = unwrap_codebuddy_deferred_call(input) {
            return CodeBuddyEventEnricher.enrich(&unwrapped);
        }
        let kind = normalized_kind(&input.tool_name);
        if codebuddy_marks_subagent(input) {
            return vec![collaboration_event(input)];
        }
        if let Some(files) = file_events(input) {
            return files;
        }
        if is_command_kind(&kind) && command_text(input.raw_input.as_ref()).is_some() {
            return vec![command_event(input)];
        }
        PassthroughEventEnricher.enrich(input)
    }
}

/// CodeBuddy routes MCP tools through a `DeferExecuteTool` virtualization
/// layer: the ACP tool call reports the wrapper, while the real call sits in
/// `raw_input` as `{toolName: "mcp__…", params: {…}}` and the result arrives
/// re-serialized as a single `{type: "text", text: <inner>}` content part.
///
/// Rewrite the event to the inner identity so tool cards resolve, and peel the
/// result wrapper so consumers see the payload the MCP server actually
/// returned. Returns `None` for a call that is not virtualized.
fn unwrap_codebuddy_deferred_call(input: &AgentEventInput) -> Option<AgentEventInput> {
    let raw_input = input.raw_input.as_ref()?;
    let tool_name = string_value(raw_input, &["toolName", "tool_name"])?;
    let mut unwrapped = input.clone();
    unwrapped.tool_name = tool_name.clone();
    if unwrapped.title.trim().is_empty() || normalized_kind(&input.title).contains("defer") {
        unwrapped.title = tool_name;
    }
    // The cards peel `params` themselves; keeping the original `raw_input`
    // would let generic input inference misread the wrapper's own fields.
    unwrapped.raw_input = raw_input.get("params").cloned();
    if let Some(inner) = unwrapped
        .output_summary
        .as_deref()
        .and_then(unwrap_codebuddy_deferred_output)
    {
        unwrapped.output_summary = Some(inner);
    }
    Some(unwrapped)
}

fn unwrap_codebuddy_deferred_output(text: &str) -> Option<String> {
    // Cheap guard before parsing a potentially large payload.
    if !text.contains("\"type\"") {
        return None;
    }
    let value = serde_json::from_str::<Value>(text).ok()?;
    let object = value.as_object()?;
    if object.get("type").and_then(Value::as_str) != Some("text") {
        return None;
    }
    object
        .get("text")
        .and_then(Value::as_str)
        .map(str::to_string)
}

/// CodeBuddy tags a native sub-agent invocation in `_meta` from the first
/// frame, long before `subagent_type` reaches `raw_input`. Detecting it early
/// keeps the collaboration card stable instead of downgrading mid-stream.
fn codebuddy_marks_subagent(input: &AgentEventInput) -> bool {
    if input
        .meta
        .get("tool_name")
        .is_some_and(|value| value.eq_ignore_ascii_case("agent"))
    {
        return true;
    }
    if input
        .meta
        .keys()
        .any(|key| key.contains("subagent") || key.contains("sub_agent"))
    {
        return true;
    }
    input
        .raw_input
        .as_ref()
        .and_then(|value| string_value(value, &["subagent_type", "subagentType"]))
        .is_some()
}

pub fn normalize_agent_event(
    enricher_kind: AgentEventEnricherKind,
    input: &AgentEventInput,
) -> Vec<NormalizedAgentEvent> {
    let events = match enricher_kind {
        AgentEventEnricherKind::Claude => ClaudeEventEnricher.enrich(input),
        AgentEventEnricherKind::Codex => CodexEventEnricher.enrich(input),
        AgentEventEnricherKind::Grok => GrokEventEnricher.enrich(input),
        AgentEventEnricherKind::CodeBuddy => CodeBuddyEventEnricher.enrich(input),
        AgentEventEnricherKind::Passthrough => PassthroughEventEnricher.enrich(input),
    };
    events
        .into_iter()
        .enumerate()
        .map(|(index, event)| NormalizedAgentEvent {
            provider_correlation_id: stable_event_correlation_id(
                &input.compatibility_identity,
                &input.native_event_id,
                event.kind_name(),
                index,
            ),
            event,
        })
        .collect()
}

pub fn stable_event_correlation_id(
    compatibility_identity: &str,
    native_event_id: &str,
    canonical_kind: &str,
    ordinal: usize,
) -> String {
    let mut hasher = Sha256::new();
    hash_component(&mut hasher, CORRELATION_DOMAIN);
    hash_component(&mut hasher, compatibility_identity.as_bytes());
    hash_component(&mut hasher, native_event_id.as_bytes());
    hash_component(&mut hasher, canonical_kind.as_bytes());
    hash_component(&mut hasher, ordinal.to_string().as_bytes());
    let digest = hasher.finalize();
    let prefix = digest[..16]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    format!("acp_event_{prefix}")
}

pub fn parse_event_locations(value: Option<&Value>) -> Vec<AgentEventLocation> {
    let values = value
        .and_then(Value::as_array)
        .map(Vec::as_slice)
        .unwrap_or_default();
    values
        .iter()
        .filter_map(|location| {
            let uri = string_value(location, &["uri", "path", "file"])?;
            let line = integer_value(location, &["line", "startLine"]);
            let column = integer_value(location, &["column", "startColumn"]);
            Some(AgentEventLocation::new(uri, line, column).0)
        })
        .take(EVENT_LOCATION_LIMIT)
        .collect()
}

pub fn parse_event_meta(value: Option<&Value>) -> BTreeMap<String, String> {
    value
        .and_then(Value::as_object)
        .into_iter()
        .flat_map(|meta| meta.iter())
        .filter_map(|(key, value)| {
            let key = normalized_kind(key);
            if !allowed_meta_key(&key) {
                return None;
            }
            let value = match value {
                Value::String(value) => value.clone(),
                Value::Number(_) | Value::Bool(_) => value.to_string(),
                _ => return None,
            };
            Some((key, value))
        })
        .take(EVENT_META_LIMIT)
        .collect()
}

fn allowed_meta_key(key: &str) -> bool {
    matches!(
        normalized_kind(key).as_str(),
        "exit_code"
            | "status"
            | "phase"
            | "kind"
            | "operation"
            | "mime_type"
            | "tool_name"
            | "agent"
            | "role"
    )
}

fn canonical_event_meta(meta: &BTreeMap<String, String>) -> BTreeMap<String, String> {
    meta.iter()
        .filter_map(|(key, value)| {
            let key = normalized_kind(key);
            allowed_meta_key(&key).then(|| (key, value.clone()))
        })
        .take(EVENT_META_LIMIT)
        .collect()
}

fn generic_tool_call(input: &AgentEventInput) -> CanonicalAgentEvent {
    CanonicalAgentEvent::ToolCall(ToolCallPayload {
        tool_call_id: stable_event_correlation_id(
            &input.compatibility_identity,
            &input.native_event_id,
            "tool_call_id",
            0,
        ),
        tool_name: public_text(&input.tool_name),
        status: input.status,
        summary: public_text(if input.title.trim().is_empty() {
            &input.tool_name
        } else {
            &input.title
        }),
        input_summary: input.raw_input.as_ref().map(summary_value),
        output_summary: input
            .output_summary
            .as_ref()
            .map(|value| public_text(value)),
        raw_extension: input.raw_extension(),
    })
}

fn command_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let command = command_text(input.raw_input.as_ref()).unwrap_or_else(|| input.title.clone());
    let cwd = input
        .raw_input
        .as_ref()
        .and_then(|value| string_value(value, &["cwd", "workingDirectory"]));
    let exit_code = input
        .meta
        .iter()
        .find(|(key, _)| normalized_kind(key) == "exit_code")
        .map(|(_, value)| value)
        .and_then(|value| value.parse::<i32>().ok());
    CanonicalAgentEvent::CommandExecution(CommandPayload {
        command: public_text(&command),
        cwd: cwd.map(|value| public_text(&value)),
        status: match input.status {
            ToolCallStatus::Started | ToolCallStatus::Progress => CommandStatus::Started,
            ToolCallStatus::Completed => CommandStatus::Completed,
            ToolCallStatus::Failed => CommandStatus::Failed,
        },
        exit_code,
        output_summary: input
            .output_summary
            .as_ref()
            .map(|value| public_text(value)),
        raw_extension: input.raw_extension(),
    })
}

fn file_events(input: &AgentEventInput) -> Option<Vec<CanonicalAgentEvent>> {
    let mut changes = Vec::new();
    let kind = normalized_kind(&input.tool_name);
    let operation_hint = file_operation_hint(&kind);
    let structured_file_kind = matches!(
        kind.as_str(),
        "diff"
            | "file_change"
            | "file_operation"
            | "apply_patch"
            | "edit_file"
            | "write_file"
            | "delete_file"
    );
    if structured_file_kind && let Some(raw_input) = input.raw_input.as_ref() {
        collect_file_changes(raw_input, operation_hint, &mut changes);
    }
    if let Some(content) = input.content.as_ref() {
        collect_diff_blocks(content, operation_hint, &mut changes);
    }
    if changes.is_empty() {
        return None;
    }
    let mut deduplicated = Vec::new();
    for change in changes {
        if !deduplicated.contains(&change) {
            deduplicated.push(change);
        }
    }
    let extension = input.raw_extension();
    Some(
        deduplicated
            .into_iter()
            .map(|change| {
                CanonicalAgentEvent::FileOperation(FileOperationPayload {
                    operation: change.operation,
                    summary: format!(
                        "{} {}",
                        file_operation_label(change.operation),
                        public_text(&change.path)
                    ),
                    path: public_text(&change.path),
                    old_text: change.old_text,
                    new_text: change.new_text,
                    patch: None,
                    raw_extension: extension.clone(),
                })
            })
            .collect(),
    )
}

fn web_search_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let query = input
        .raw_input
        .as_ref()
        .and_then(|value| string_value(value, &["query", "searchQuery"]))
        .or_else(|| {
            input
                .raw_input
                .as_ref()
                .and_then(Value::as_str)
                .map(str::to_string)
        })
        .unwrap_or_else(|| input.title.clone());
    CanonicalAgentEvent::WebSearch(WebSearchPayload {
        query: public_text(&query),
        status: input.status,
        result_summary: input
            .output_summary
            .as_ref()
            .map(|value| public_text(value)),
        raw_extension: input.raw_extension(),
    })
}

fn todo_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    CanonicalAgentEvent::TodoUpdate(TodoUpdatePayload {
        title: public_text(if input.title.trim().is_empty() {
            "Todo"
        } else {
            &input.title
        }),
        items: todo_items(input.raw_input.as_ref()).unwrap_or_default(),
        raw_extension: input.raw_extension(),
    })
}

fn collaboration_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let agent_label = input
        .raw_input
        .as_ref()
        .and_then(|value| string_value(value, &["agent", "agentName", "role"]))
        .map(|value| public_text(&value));
    CanonicalAgentEvent::Collaboration(CollaborationPayload {
        action: public_text(&input.tool_name),
        status: input.status,
        summary: public_text(if input.title.trim().is_empty() {
            &input.tool_name
        } else {
            &input.title
        }),
        agent_label,
        raw_extension: input.raw_extension(),
    })
}

fn image_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let mime_type = image_string(input, &["mimeType", "mime_type"]);
    let image_reference = image_string(input, &["imageReference", "uri", "url"])
        .filter(|value| !value.trim_start().starts_with("data:"))
        .map(|value| public_text(&value));
    CanonicalAgentEvent::ImageGeneration(ImageGenerationPayload {
        status: input.status,
        summary: public_text(if input.title.trim().is_empty() {
            "Image generation"
        } else {
            &input.title
        }),
        mime_type: mime_type.map(|value| public_text(&value)),
        image_reference,
        raw_extension: input.raw_extension(),
    })
}

fn content_blocks(content: Option<&Value>) -> (Vec<AgentEventContentBlock>, bool) {
    let Some(content) = content else {
        return (Vec::new(), false);
    };
    let items = content
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(content));
    let truncated = items.len() > 16;
    let blocks = items
        .iter()
        .take(16)
        .map(|item| {
            let block_type = item
                .get("type")
                .and_then(Value::as_str)
                .unwrap_or("unknown");
            let summary = match block_type {
                "text" | "content" => item
                    .get("text")
                    .or_else(|| item.get("content"))
                    .map(summary_value)
                    .unwrap_or_else(|| block_type.to_string()),
                "diff" => string_value(item, &["path", "file", "uri"])
                    .map(|path| format!("diff {}", public_text(&path)))
                    .unwrap_or_else(|| "diff".to_string()),
                "image" => image_string_value(item)
                    .map(|value| public_text(&value))
                    .unwrap_or_else(|| "image".to_string()),
                _ => summary_value(item),
            };
            AgentEventContentBlock::new(block_type, summary).0
        })
        .collect();
    (blocks, truncated)
}

fn collect_file_changes(
    value: &Value,
    operation_hint: Option<FileOperationKind>,
    changes: &mut Vec<StructuredFileChange>,
) {
    let entries = value
        .get("changes")
        .and_then(Value::as_array)
        .or_else(|| value.as_array());
    if let Some(entries) = entries {
        for entry in entries.iter().take(16) {
            if let Some(path) = string_value(entry, &["path", "file", "uri"]) {
                changes.push(structured_file_change(entry, path, operation_hint));
            }
        }
        return;
    }
    if let Some(path) = string_value(value, &["path", "file", "uri"]) {
        changes.push(structured_file_change(value, path, operation_hint));
    }
}

fn collect_diff_blocks(
    value: &Value,
    operation_hint: Option<FileOperationKind>,
    changes: &mut Vec<StructuredFileChange>,
) {
    let items = value
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(value));
    for item in items.iter().take(16) {
        if item.get("type").and_then(Value::as_str) != Some("diff") {
            continue;
        }
        if let Some(path) = string_value(item, &["path", "file", "uri"]) {
            changes.push(structured_file_change(item, path, operation_hint));
        }
    }
}

fn structured_file_change(
    value: &Value,
    path: String,
    operation_hint: Option<FileOperationKind>,
) -> StructuredFileChange {
    let old_text = text_value(value, &["oldText", "old_text", "before"]);
    let new_text = text_value(value, &["newText", "new_text", "after", "text", "content"]);
    StructuredFileChange {
        operation: file_operation(
            value,
            operation_hint,
            old_text.as_deref(),
            new_text.as_deref(),
        ),
        path,
        old_text,
        new_text,
    }
}

fn file_operation(
    value: &Value,
    operation_hint: Option<FileOperationKind>,
    old_text: Option<&str>,
    new_text: Option<&str>,
) -> FileOperationKind {
    let explicit = string_value(value, &["kind", "operation"])
        .and_then(|kind| file_operation_from_kind(&kind))
        .or_else(|| {
            value
                .get("_meta")
                .and_then(|meta| string_value(meta, &["kind", "operation"]))
                .and_then(|kind| file_operation_from_kind(&kind))
        });
    let inferred = match (old_text, new_text) {
        // ACP v1 defines an absent oldText as a newly created file.
        (None, Some(_)) => FileOperationKind::Write,
        // Codex ACP represents deletion as old contents followed by an
        // empty or omitted replacement. Explicit operations above remain
        // authoritative for providers that distinguish clearing a file.
        (Some(_), None | Some("")) => FileOperationKind::Delete,
        _ => FileOperationKind::Edit,
    };
    explicit.or(operation_hint).unwrap_or(inferred)
}

fn file_operation_from_kind(kind: &str) -> Option<FileOperationKind> {
    match normalized_kind(kind).as_str() {
        "add" | "create" | "write" => Some(FileOperationKind::Write),
        "delete" | "remove" => Some(FileOperationKind::Delete),
        "move" | "rename" => Some(FileOperationKind::Move),
        "read" => Some(FileOperationKind::Read),
        "edit" | "modify" | "update" => Some(FileOperationKind::Edit),
        _ => None,
    }
}

fn file_operation_hint(tool_kind: &str) -> Option<FileOperationKind> {
    match tool_kind {
        "write_file" => Some(FileOperationKind::Write),
        "edit_file" => Some(FileOperationKind::Edit),
        "delete_file" => Some(FileOperationKind::Delete),
        _ => None,
    }
}

fn file_operation_label(operation: FileOperationKind) -> &'static str {
    match operation {
        FileOperationKind::Read => "Read",
        FileOperationKind::Write => "Wrote",
        FileOperationKind::Edit => "Edited",
        FileOperationKind::Delete => "Deleted",
        FileOperationKind::Move => "Moved",
    }
}

fn todo_items(value: Option<&Value>) -> Option<Vec<PlanStepPayload>> {
    let items = value?.get("items").and_then(Value::as_array)?;
    Some(
        items
            .iter()
            .take(32)
            .filter_map(|item| {
                let title = string_value(item, &["text", "title", "description"])?;
                let status = string_value(item, &["status"])
                    .map(|status| normalized_kind(&status))
                    .map(|status| match status.as_str() {
                        "completed" | "done" => PlanStepStatus::Completed,
                        "running" | "in_progress" => PlanStepStatus::Running,
                        "failed" => PlanStepStatus::Failed,
                        _ => PlanStepStatus::Pending,
                    })
                    .unwrap_or_else(|| {
                        if item.get("completed").and_then(Value::as_bool) == Some(true) {
                            PlanStepStatus::Completed
                        } else {
                            PlanStepStatus::Pending
                        }
                    });
                Some(PlanStepPayload {
                    title: public_text(&title),
                    status,
                })
            })
            .collect(),
    )
}

fn command_text(value: Option<&Value>) -> Option<String> {
    let value = value?;
    value
        .as_str()
        .map(str::to_string)
        .or_else(|| string_value(value, &["command", "cmd", "shellCommand"]))
}

fn is_command_kind(kind: &str) -> bool {
    matches!(
        kind,
        "command" | "command_execution" | "execute" | "shell" | "shell_command" | "terminal"
    )
}

fn contains_image_block(content: Option<&Value>) -> bool {
    content.is_some_and(|content| {
        content
            .as_array()
            .map(|items| {
                items
                    .iter()
                    .any(|item| item.get("type").and_then(Value::as_str) == Some("image"))
            })
            .unwrap_or_else(|| content.get("type").and_then(Value::as_str) == Some("image"))
    })
}

fn image_string(input: &AgentEventInput, keys: &[&str]) -> Option<String> {
    input
        .raw_output
        .as_ref()
        .and_then(|output| serde_json::from_str::<Value>(&output.text).ok())
        .as_ref()
        .and_then(|value| string_value(value, keys))
        .or_else(|| {
            input
                .raw_input
                .as_ref()
                .and_then(|value| string_value(value, keys))
        })
        .or_else(|| {
            input
                .content
                .as_ref()
                .and_then(|value| image_value(value, keys))
        })
}

fn image_value(value: &Value, keys: &[&str]) -> Option<String> {
    if let Some(items) = value.as_array() {
        return items.iter().find_map(|item| image_value(item, keys));
    }
    string_value(value, keys)
}

fn image_string_value(value: &Value) -> Option<String> {
    string_value(value, &["mimeType", "mime_type", "uri", "url"])
}

fn string_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
            .map(str::to_string)
    })
}

fn text_value(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_str).map(str::to_string))
}

fn integer_value(value: &Value, keys: &[&str]) -> Option<u32> {
    keys.iter()
        .find_map(|key| value.get(*key).and_then(Value::as_u64))
        .and_then(|value| u32::try_from(value).ok())
}

fn normalized_kind(value: &str) -> String {
    let mut normalized = String::with_capacity(value.len());
    let mut previous_was_lower_or_digit = false;
    let mut previous_was_separator = true;
    for character in value.trim().chars() {
        if character.is_ascii_alphanumeric() {
            if character.is_ascii_uppercase()
                && previous_was_lower_or_digit
                && !previous_was_separator
            {
                normalized.push('_');
            }
            normalized.push(character.to_ascii_lowercase());
            previous_was_lower_or_digit =
                character.is_ascii_lowercase() || character.is_ascii_digit();
            previous_was_separator = false;
        } else if !previous_was_separator && !normalized.is_empty() {
            normalized.push('_');
            previous_was_lower_or_digit = false;
            previous_was_separator = true;
        }
    }
    normalized.trim_matches('_').to_string()
}

fn summary_value(value: &Value) -> String {
    match value {
        Value::String(value) => public_text(value),
        _ => public_text(&stable_json_text(value)),
    }
}

fn stable_json_text(value: &Value) -> String {
    serde_json::to_string(&sorted_json_value(value)).unwrap_or_else(|_| "null".to_string())
}

fn sorted_json_value(value: &Value) -> Value {
    match value {
        Value::Array(values) => Value::Array(values.iter().map(sorted_json_value).collect()),
        Value::Object(values) => {
            let mut entries = values.iter().collect::<Vec<_>>();
            entries.sort_unstable_by_key(|(key, _)| *key);
            Value::Object(
                entries
                    .into_iter()
                    .map(|(key, value)| (key.clone(), sorted_json_value(value)))
                    .collect(),
            )
        }
        _ => value.clone(),
    }
}

fn public_text(value: &str) -> String {
    let value = AgentEventRawExtension::bounded_text(value);
    if value.len() <= PUBLIC_EVENT_TEXT_LIMIT {
        value
    } else {
        let suffix = "...(truncated)";
        let mut end = PUBLIC_EVENT_TEXT_LIMIT.saturating_sub(suffix.len());
        while end > 0 && !value.is_char_boundary(end) {
            end -= 1;
        }
        let prefix = &value[..end];
        format!("{prefix}...(truncated)")
    }
}

fn hash_component(hasher: &mut Sha256, value: &[u8]) {
    hasher.update((value.len() as u64).to_be_bytes());
    hasher.update(value);
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use vibex_core::AgentEventRawOutputMode;

    fn input(kind: &str, raw_input: Value) -> AgentEventInput {
        AgentEventInput {
            source: AgentEventInputSource::Live,
            compatibility_identity: "adapter=codex-acp@1.1.9".to_string(),
            native_event_id: "native-event-secret".to_string(),
            tool_name: kind.to_string(),
            title: "Safe event".to_string(),
            status: ToolCallStatus::Completed,
            raw_input: Some(raw_input),
            output_summary: Some("safe output".to_string()),
            raw_output: Some(
                AgentEventRawOutput::new(AgentEventRawOutputMode::Snapshot, "safe output").0,
            ),
            content: None,
            locations: Vec::new(),
            meta: BTreeMap::new(),
        }
    }

    #[test]
    fn codebuddy_unwraps_deferred_mcp_calls_to_their_real_identity() {
        let mut event = input(
            "DeferExecuteTool",
            json!({
                "toolName": "mcp__github__create_issue",
                "params": {"title": "bug", "repo": "vibex"}
            }),
        );
        event.title = "DeferExecuteTool".to_string();
        // CodeBuddy re-serializes a deferred result into the OpenAI-Agents
        // content shape; consumers expect the inner payload.
        event.output_summary = Some(r#"{"type":"text","text":"{\"number\":42}"}"#.to_string());

        let events = CodeBuddyEventEnricher.enrich(&event);
        let CanonicalAgentEvent::ToolCall(call) = &events[0] else {
            panic!("expected a tool call: {events:?}");
        };
        assert_eq!(call.tool_name, "mcp__github__create_issue");
        assert_eq!(call.summary, "mcp__github__create_issue");
        assert_eq!(call.output_summary.as_deref(), Some(r#"{"number":42}"#));
        // The wrapper's own fields must not reach input inference.
        assert!(
            call.input_summary
                .as_deref()
                .is_some_and(|summary| !summary.contains("toolName"))
        );

        // A non-virtualized call keeps its own identity.
        let plain = input("Read", json!({"path": "src/lib.rs"}));
        let plain_events = CodeBuddyEventEnricher.enrich(&plain);
        let CanonicalAgentEvent::ToolCall(call) = &plain_events[0] else {
            panic!("expected a tool call: {plain_events:?}");
        };
        assert_eq!(call.tool_name, "Read");
        assert_eq!(call.output_summary.as_deref(), Some("safe output"));
    }

    #[test]
    fn codebuddy_marks_subagent_invocations_as_collaboration() {
        let mut meta_tagged = input("Task", json!({}));
        meta_tagged
            .meta
            .insert("tool_name".to_string(), "Agent".to_string());
        assert!(matches!(
            CodeBuddyEventEnricher.enrich(&meta_tagged)[0],
            CanonicalAgentEvent::Collaboration(_)
        ));

        // `subagent_type` only reaches `raw_input` dozens of frames later, so
        // both signals must classify.
        let raw_tagged = input("Task", json!({"subagent_type": "reviewer"}));
        assert!(matches!(
            CodeBuddyEventEnricher.enrich(&raw_tagged)[0],
            CanonicalAgentEvent::Collaboration(_)
        ));
    }

    #[test]
    fn grok_plan_content_becomes_a_plan_rather_than_an_anonymous_tool_call() {
        let mut event = input("_x.ai/exit_plan_mode", json!({"planContent": "1. survey"}));
        event.status = ToolCallStatus::Progress;
        let events = GrokEventEnricher.enrich(&event);
        let CanonicalAgentEvent::Plan(plan) = &events[0] else {
            panic!("expected a plan: {events:?}");
        };
        assert_eq!(plan.steps.len(), 1);
        assert_eq!(plan.steps[0].title, "1. survey");
        assert_eq!(plan.steps[0].status, PlanStepStatus::Running);

        // Without plan content there is nothing to project, so the generic
        // path still applies.
        let empty = input("_x.ai/exit_plan_mode", json!({}));
        assert!(matches!(
            GrokEventEnricher.enrich(&empty)[0],
            CanonicalAgentEvent::ToolCall(_)
        ));
    }

    #[test]
    fn canonical_enum_converts_every_variant() {
        let extension = None;
        let variants = vec![
            CanonicalAgentEvent::AgentMessage(AgentMessagePayload {
                text: "answer".to_string(),
                is_final: true,
            }),
            CanonicalAgentEvent::Reasoning(ReasoningPayload {
                text: "reason".to_string(),
                is_final: true,
            }),
            CanonicalAgentEvent::Plan(PlanPayload {
                title: "plan".to_string(),
                steps: Vec::new(),
            }),
            CanonicalAgentEvent::ToolCall(ToolCallPayload {
                tool_call_id: "tool".to_string(),
                tool_name: "tool".to_string(),
                status: ToolCallStatus::Completed,
                summary: "done".to_string(),
                input_summary: None,
                output_summary: None,
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::CommandExecution(CommandPayload {
                command: "true".to_string(),
                cwd: None,
                status: CommandStatus::Completed,
                exit_code: Some(0),
                output_summary: None,
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::FileOperation(FileOperationPayload {
                operation: FileOperationKind::Edit,
                path: "src/lib.rs".to_string(),
                summary: "edited".to_string(),
                old_text: None,
                new_text: None,
                patch: None,
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::WebSearch(WebSearchPayload {
                query: "query".to_string(),
                status: ToolCallStatus::Completed,
                result_summary: None,
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::TodoUpdate(TodoUpdatePayload {
                title: "todo".to_string(),
                items: Vec::new(),
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::Collaboration(CollaborationPayload {
                action: "spawn_agent".to_string(),
                status: ToolCallStatus::Completed,
                summary: "done".to_string(),
                agent_label: None,
                raw_extension: extension.clone(),
            }),
            CanonicalAgentEvent::ImageGeneration(ImageGenerationPayload {
                status: ToolCallStatus::Completed,
                summary: "image".to_string(),
                mime_type: Some("image/png".to_string()),
                image_reference: None,
                raw_extension: extension,
            }),
            CanonicalAgentEvent::PermissionRequest(PermissionRequest {
                id: vibex_core::RequestId::new(),
                session_id: vibex_core::VibexSessionId::new(),
                project_id: None,
                workspace_id: None,
                provider_request_id: None,
                risk_category: vibex_core::PermissionRiskCategory::CustomTool,
                title: "allow".to_string(),
                details: Vec::new(),
                allowed_responses: vec![vibex_core::PermissionResponseKind::Deny],
                response_options: Vec::new(),
                status: vibex_core::PermissionRequestStatus::Pending,
                requested_at_ms: 0,
                expires_at_ms: None,
            }),
            CanonicalAgentEvent::SystemNotice(SystemNoticePayload {
                level: vibex_core::SystemNoticeLevel::Info,
                message: "notice".to_string(),
            }),
        ];
        let kinds = variants
            .into_iter()
            .map(|event| event.into_timeline_payload().kind())
            .collect::<Vec<_>>();
        assert_eq!(kinds.len(), 12);
    }

    #[test]
    fn exact_enricher_classifies_structured_codex_events_only() {
        let command = normalize_agent_event(
            AgentEventEnricherKind::Codex,
            &input("command_execution", json!({"command":"cargo test"})),
        );
        assert!(matches!(
            command[0].event,
            CanonicalAgentEvent::CommandExecution(_)
        ));

        let passthrough = normalize_agent_event(
            AgentEventEnricherKind::Passthrough,
            &input("command_execution", json!({"command":"cargo test"})),
        );
        assert!(matches!(
            passthrough[0].event,
            CanonicalAgentEvent::ToolCall(_)
        ));

        let ambiguous = normalize_agent_event(
            AgentEventEnricherKind::Codex,
            &input(
                "command_execution",
                json!({"description":"no command field"}),
            ),
        );
        assert!(matches!(
            ambiguous[0].event,
            CanonicalAgentEvent::ToolCall(_)
        ));

        let mut camel_case = input("commandExecution", json!({"command":"cargo test"}));
        camel_case.meta = parse_event_meta(Some(&json!({
            "exitCode": 7,
            "mimeType": "text/plain",
            "apiToken": "must-not-survive"
        })));
        let camel_case = normalize_agent_event(AgentEventEnricherKind::Codex, &camel_case);
        match &camel_case[0].event {
            CanonicalAgentEvent::CommandExecution(command) => {
                assert_eq!(command.exit_code, Some(7));
                let mime_type = command
                    .raw_extension
                    .as_ref()
                    .unwrap()
                    .meta
                    .get("mime_type")
                    .map(String::as_str);
                assert_eq!(mime_type, Some("text/plain"));
                assert!(
                    !command
                        .raw_extension
                        .as_ref()
                        .unwrap()
                        .meta
                        .contains_key("api_token")
                );
            }
            other => panic!("expected camelCase command event, got {other:?}"),
        }
    }

    #[test]
    fn claude_background_extensions_share_the_collaboration_projection() {
        for kind in [
            "claude_subagent",
            "claude_background_task",
            "claude_background_agent",
            "claude_background_shell",
            "claude_task_notification",
            "claude_task_update",
        ] {
            let events = normalize_agent_event(
                AgentEventEnricherKind::Claude,
                &input(kind, json!({"agent":"reviewer"})),
            );
            assert!(matches!(
                events[0].event,
                CanonicalAgentEvent::Collaboration(_)
            ));
        }
    }

    #[test]
    fn diff_batch_and_advanced_events_keep_product_semantics() {
        let files = normalize_agent_event(
            AgentEventEnricherKind::Codex,
            &input(
                "file_change",
                json!({"changes":[
                    {"path":"src/new.rs","kind":"add","newText":"new file"},
                    {"path":"src/lib.rs","kind":"update","oldText":"before","newText":"after"},
                    {"path":"src/old.rs","kind":"delete"}
                ]}),
            ),
        );
        assert_eq!(files.len(), 3);
        assert!(matches!(
            &files[0].event,
            CanonicalAgentEvent::FileOperation(FileOperationPayload {
                operation: FileOperationKind::Write,
                ..
            })
        ));
        match &files[1].event {
            CanonicalAgentEvent::FileOperation(file) => {
                assert_eq!(file.old_text.as_deref(), Some("before"));
                assert_eq!(file.new_text.as_deref(), Some("after"));
            }
            other => panic!("expected lossless file operation, got {other:?}"),
        }

        for (kind, raw, predicate) in [
            ("web_search", json!({"query":"rust"}), "web_search"),
            (
                "todo_list",
                json!({"items":[{"text":"test","completed":false}]}),
                "todo_update",
            ),
            ("spawn_agent", json!({"agent":"reviewer"}), "collaboration"),
            (
                "image_generation",
                json!({"mimeType":"image/png","imageReference":"asset:1"}),
                "image_generation",
            ),
        ] {
            let events = normalize_agent_event(AgentEventEnricherKind::Codex, &input(kind, raw));
            assert_eq!(events[0].event.kind_name(), predicate);
        }
    }

    #[test]
    fn acp_v1_diffs_infer_file_lifecycle_and_preserve_empty_text() {
        let mut event = input("diff", json!({}));
        event.content = Some(json!([
            {
                "type": "diff",
                "path": "src/new.rs",
                "newText": "new file",
                "_meta": { "kind": "add" }
            },
            {
                "type": "diff",
                "path": "src/empty.rs",
                "newText": ""
            },
            {
                "type": "diff",
                "path": "src/lib.rs",
                "oldText": "before",
                "newText": "after"
            },
            {
                "type": "diff",
                "path": "src/old.rs",
                "oldText": "old file",
                "newText": "",
                "kind": "future_file_operation",
                "_meta": { "kind": "delete" }
            },
            {
                "type": "diff",
                "path": "src/cleared.rs",
                "oldText": "clear me",
                "newText": "",
                "_meta": { "kind": "update" }
            }
        ]));

        let files = normalize_agent_event(AgentEventEnricherKind::Codex, &event)
            .into_iter()
            .map(|event| match event.event {
                CanonicalAgentEvent::FileOperation(file) => file,
                other => panic!("expected file operation, got {other:?}"),
            })
            .collect::<Vec<_>>();

        assert_eq!(files.len(), 5);
        assert_eq!(files[0].operation, FileOperationKind::Write);
        assert_eq!(files[1].operation, FileOperationKind::Write);
        assert_eq!(files[1].new_text.as_deref(), Some(""));
        assert_eq!(files[2].operation, FileOperationKind::Edit);
        assert_eq!(files[3].operation, FileOperationKind::Delete);
        assert_eq!(files[3].new_text.as_deref(), Some(""));
        assert_eq!(files[4].operation, FileOperationKind::Edit);
        assert_eq!(files[4].new_text.as_deref(), Some(""));
    }

    #[test]
    fn file_tool_kind_is_used_when_the_diff_has_no_operation() {
        for (tool_name, operation) in [
            ("write_file", FileOperationKind::Write),
            ("edit_file", FileOperationKind::Edit),
            ("delete_file", FileOperationKind::Delete),
        ] {
            let events = normalize_agent_event(
                AgentEventEnricherKind::Codex,
                &input(
                    tool_name,
                    json!({"path":"src/lib.rs","oldText":"before","newText":"after"}),
                ),
            );
            assert!(matches!(
                &events[0].event,
                CanonicalAgentEvent::FileOperation(FileOperationPayload {
                    operation: actual,
                    ..
                }) if *actual == operation
            ));
        }
    }

    #[test]
    fn correlation_is_stable_across_live_and_transcript_without_native_id() {
        let live = input("web_search", json!({"query":"rust"}));
        let mut transcript = live.clone();
        transcript.source = AgentEventInputSource::Transcript;
        let live_result = normalize_agent_event(AgentEventEnricherKind::Codex, &live);
        let transcript_result = normalize_agent_event(AgentEventEnricherKind::Codex, &transcript);
        assert_eq!(
            live_result[0].provider_correlation_id,
            transcript_result[0].provider_correlation_id
        );
        assert!(
            !live_result[0]
                .provider_correlation_id
                .contains("native-event-secret")
        );

        let mut other = live.clone();
        other.compatibility_identity.push_str("-other");
        assert_ne!(
            live_result[0].provider_correlation_id,
            normalize_agent_event(AgentEventEnricherKind::Codex, &other)[0].provider_correlation_id
        );
    }

    #[test]
    fn input_debug_hides_payloads_while_timeline_extension_preserves_them() {
        let input = input(
            "tool",
            json!({"path":"/home/alice/private","token":"secret-value"}),
        );
        let input_debug = format!("{input:?}");
        assert!(!input_debug.contains("alice"));
        assert!(!input_debug.contains("secret-value"));
        let extension = input.raw_extension().unwrap();
        let debug = format!("{extension:?}");
        assert!(!debug.contains("alice"));
        assert!(!debug.contains("secret-value"));
        let json = serde_json::to_string(&extension).unwrap();
        assert!(json.contains("alice"));
        assert!(json.contains("secret-value"));
    }

    #[test]
    fn raw_json_text_is_stable_when_serde_json_preserves_insertion_order() {
        let mut nested = serde_json::Map::new();
        nested.insert("zeta".to_string(), Value::Bool(true));
        nested.insert("alpha".to_string(), Value::Bool(false));
        let mut root = serde_json::Map::new();
        root.insert("path".to_string(), Value::String("src/lib.rs".to_string()));
        root.insert("kind".to_string(), Value::String("update".to_string()));
        root.insert("nested".to_string(), Value::Object(nested));

        assert_eq!(
            stable_json_text(&Value::Object(root)),
            r#"{"kind":"update","nested":{"alpha":false,"zeta":true},"path":"src/lib.rs"}"#
        );
    }
}

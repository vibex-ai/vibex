//! Provider-neutral ACP event normalization and exact-identity enrichers.

use std::collections::BTreeMap;
use std::fmt;

use serde_json::{Value, json};
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
            session_title: None,
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
pub struct HermesEventEnricher;

impl AgentEventEnricher for HermesEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        // Hermes' ACP adapter maps its native tool names to the generic ACP
        // `kind` enum. Its generated title is stable, so recover only the
        // known names and keep unknown calls on the generic path.
        let mut normalized = input.clone();
        if let Some(tool_name) = hermes_tool_name(&input.tool_name, &input.title) {
            normalized.tool_name = tool_name.to_string();
        }

        if let Some(files) = file_events(&normalized) {
            return files;
        }

        let kind = normalized_kind(&normalized.tool_name);
        if matches!(kind.as_str(), "terminal" | "execute_code") {
            if normalized.raw_input.is_none()
                && let Some(command) = hermes_command_from_title(&input.title)
            {
                normalized.raw_input = Some(json!({"command": command}));
            }
            if command_text(normalized.raw_input.as_ref()).is_some() {
                return vec![command_event(&normalized)];
            }
        }
        if kind == "web_search" {
            return vec![web_search_event(&normalized)];
        }
        if kind == "todo" && todo_items(normalized.raw_input.as_ref()).is_some() {
            return vec![todo_event(&normalized)];
        }
        PassthroughEventEnricher.enrich(&normalized)
    }
}

fn hermes_tool_name(tool_kind: &str, title: &str) -> Option<&'static str> {
    let kind = normalized_kind(tool_kind);
    let title = title.trim().to_ascii_lowercase();
    match kind.as_str() {
        "read" if title.starts_with("read:") => Some("read_file"),
        "edit" if title.starts_with("write:") => Some("write_file"),
        "edit" if title.starts_with("patch") => Some("patch"),
        "search" if title.starts_with("search:") => Some("search_files"),
        "execute" if title.starts_with("terminal:") => Some("terminal"),
        "execute" if title.starts_with("python:") => Some("execute_code"),
        "fetch" if title.starts_with("web search:") => Some("web_search"),
        "other" if title.starts_with("todo") => Some("todo"),
        _ => None,
    }
}

fn hermes_command_from_title(title: &str) -> Option<String> {
    let title = title.trim();
    ["terminal:", "python:", "execute:"]
        .iter()
        .find_map(|prefix| {
            let prefix_len = prefix.len();
            title
                .get(..prefix_len)
                .is_some_and(|head| head.eq_ignore_ascii_case(prefix))
                .then(|| title[prefix_len..].trim())
                .filter(|command| !command.is_empty())
                .map(str::to_string)
        })
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

#[derive(Debug, Default)]
pub struct AntigravityEventEnricher;

impl AgentEventEnricher for AntigravityEventEnricher {
    fn enrich(&self, input: &AgentEventInput) -> Vec<CanonicalAgentEvent> {
        let mut normalized = input.clone();
        // `agy_acp_server` replays non-MCP historical tool arguments as a JSON
        // string (`tool_call.arguments_json`); decode object strings so live
        // and replayed sessions produce identical canonical payloads.
        if let Some(Value::String(raw_json)) = normalized.raw_input.as_ref()
            && raw_json.trim_start().starts_with('{')
            && let Ok(parsed) = serde_json::from_str::<Value>(raw_json)
            && parsed.is_object()
        {
            normalized.raw_input = Some(parsed);
        }

        // Antigravity tags MCP calls with `_meta.mcp.{server,tool}` and puts a
        // redundant `"arguments"` wrapper alongside the flattened tool args in
        // `rawInput`. Normalize to `mcp__<server>__<tool>` and unwrap the
        // duplicate envelope so the timeline classifies and displays it as MCP.
        if let Some(mcp_name) = antigravity_mcp_tool_name(&normalized) {
            normalized.tool_name = mcp_name;
            if let Some(prompt) = normalized
                .meta
                .get("prompt")
                .map(String::as_str)
                .map(str::trim)
                .filter(|value| !value.is_empty())
            {
                normalized.title = prompt.to_string();
            } else {
                normalized.title = normalized.tool_name.clone();
            }
            if let Some(unwrapped) = normalized
                .raw_input
                .as_ref()
                .and_then(antigravity_unwrap_mcp_raw_input)
            {
                normalized.raw_input = Some(unwrapped);
            }
            return PassthroughEventEnricher.enrich(&normalized);
        }

        let original_kind = normalized_kind(&normalized.tool_name);
        if let Some(recovered) = antigravity_recovered_tool_name(&normalized, &original_kind) {
            normalized.tool_name = recovered;
        }
        let kind = normalized_kind(&normalized.tool_name);

        if kind == "run_command"
            || kind == "shell"
            || (original_kind == "execute"
                && (command_text(normalized.raw_input.as_ref()).is_some()
                    || !normalized.title.trim().is_empty()))
        {
            if let Some(summary) = antigravity_exec_output_summary(&normalized) {
                normalized.output_summary = Some(summary);
            }
            return vec![command_event(&normalized)];
        }

        if matches!(
            kind.as_str(),
            "client_create_file"
                | "client_edit_file"
                | "create_file"
                | "edit_file"
                | "write_file"
                | "write_to_file"
                | "replace_file_content"
                | "multi_replace_file_content"
                | "delete_file"
        ) || (original_kind == "edit" && has_diff_content(normalized.content.as_ref()))
        {
            // `FileOperationPayload` carries no failed state; when a write or
            // edit fails (permission denial, quota/step error), keep a named
            // `ToolCall` so the timeline preserves the failed badge and error.
            if normalized.status != ToolCallStatus::Failed
                && let Some(files) = antigravity_file_events(&normalized)
            {
                return files;
            }
            return PassthroughEventEnricher.enrich(&normalized);
        }

        if matches!(kind.as_str(), "search_web" | "web_search") {
            return vec![web_search_event(&normalized)];
        }

        if matches!(kind.as_str(), "generate_image" | "image_generation")
            || contains_image_block(normalized.content.as_ref())
        {
            return vec![image_event(&normalized)];
        }

        if matches!(
            kind.as_str(),
            "invoke_subagent" | "start_subagent" | "run_workflow" | "browser_subagent"
        ) {
            return vec![antigravity_collaboration_event(&normalized)];
        }

        if matches!(
            kind.as_str(),
            "todo" | "todo_list" | "todo_update" | "todo_write" | "update_steps"
        ) && todo_items(normalized.raw_input.as_ref()).is_some()
        {
            return vec![todo_event(&normalized)];
        }

        PassthroughEventEnricher.enrich(&normalized)
    }
}

fn antigravity_mcp_tool_name(input: &AgentEventInput) -> Option<String> {
    let tool = input
        .meta
        .get("mcp_tool")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    let server = input
        .meta
        .get("mcp_server")
        .map(String::as_str)
        .map(str::trim)
        .filter(|value| !value.is_empty());
    match (server, tool) {
        (Some(server), Some(tool)) => Some(format!("mcp__{server}__{tool}")),
        (None, Some(tool)) => Some(format!("mcp__mcp__{tool}")),
        _ => None,
    }
}

fn antigravity_unwrap_mcp_raw_input(raw_input: &Value) -> Option<Value> {
    let object = raw_input.as_object()?;
    let arguments = object.get("arguments")?.as_object()?;
    // `agy_acp_server`'s `unwrap_mcp_tool_call` copies every key from
    // `arguments` to the top level as well as keeping `"arguments"`. Strip the
    // duplicate wrapper only when all other top-level keys already come from
    // `arguments`.
    let redundant_wrapper = object
        .iter()
        .all(|(key, value)| key == "arguments" || arguments.get(key) == Some(value));
    redundant_wrapper.then(|| Value::Object(arguments.clone()))
}

fn antigravity_recovered_tool_name(input: &AgentEventInput, original_kind: &str) -> Option<String> {
    if let Some(rest) = input.title.trim().strip_prefix("Running ") {
        let candidate = rest.trim();
        if is_antigravity_tool_identifier(candidate) {
            return Some(candidate.to_string());
        }
    }
    if input.native_event_id.starts_with("interaction_")
        && matches!(original_kind, "" | "tool" | "other")
    {
        return Some("ask_question".to_string());
    }
    if original_kind == "execute" {
        return Some("run_command".to_string());
    }
    if original_kind == "fetch" {
        return Some("read_url_content".to_string());
    }
    if original_kind == "read"
        && input
            .raw_input
            .as_ref()
            .and_then(|value| {
                string_value(
                    value,
                    &["absolute_path", "AbsolutePath", "file_path", "FilePath"],
                )
            })
            .is_some()
    {
        return Some("view_file".to_string());
    }
    None
}

fn is_antigravity_tool_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value.contains('_')
        && !value.contains(char::is_whitespace)
        && value
            .chars()
            .all(|character| character.is_ascii_alphanumeric() || character == '_')
}

fn antigravity_exec_output_summary(input: &AgentEventInput) -> Option<String> {
    let raw = input.raw_output.as_ref()?.text.as_str();
    let parsed = serde_json::from_str::<Value>(raw).ok()?;
    let object = parsed.as_object()?;
    let output = object
        .get("formatted_output")
        .or_else(|| object.get("combinedOutput"))
        .or_else(|| object.get("combined_output"))
        .and_then(Value::as_str)?;
    let trimmed = output.trim();
    (!trimmed.is_empty()).then(|| trimmed.to_string())
}

fn has_diff_content(content: Option<&Value>) -> bool {
    let Some(content) = content else {
        return false;
    };
    let items = content
        .as_array()
        .map(Vec::as_slice)
        .unwrap_or_else(|| std::slice::from_ref(content));
    items
        .iter()
        .any(|item| item.get("type").and_then(Value::as_str) == Some("diff"))
}

fn antigravity_file_events(input: &AgentEventInput) -> Option<Vec<CanonicalAgentEvent>> {
    // Prefer ACP diff blocks emitted by `agy_acp_server` (`extract_tool_content`),
    // which carry both `oldText` (loaded from disk before the edit) and `newText`.
    if has_diff_content(input.content.as_ref()) {
        let mut diff_only = input.clone();
        diff_only.tool_name = "diff".to_string();
        if let Some(files) = file_events(&diff_only) {
            return Some(files);
        }
    }
    let raw_input = input.raw_input.as_ref()?;
    let path = string_value(
        raw_input,
        &[
            "target_file",
            "TargetFile",
            "file_path",
            "FilePath",
            "absolute_path",
            "AbsolutePath",
            "path",
            "file",
        ],
    )?;
    let kind = normalized_kind(&input.tool_name);
    let hint = match kind.as_str() {
        "client_create_file" | "create_file" | "write_file" | "write_to_file" => {
            Some(FileOperationKind::Write)
        }
        "client_edit_file"
        | "edit_file"
        | "replace_file_content"
        | "multi_replace_file_content" => Some(FileOperationKind::Edit),
        "delete_file" => Some(FileOperationKind::Delete),
        _ => None,
    };
    let (old_text, new_text) = if let Some(chunks) = raw_input
        .get("ReplacementChunks")
        .or_else(|| raw_input.get("replacement_chunks"))
        .and_then(Value::as_array)
        .filter(|chunks| !chunks.is_empty())
    {
        let old = chunks
            .iter()
            .filter_map(|chunk| text_value(chunk, &["TargetContent", "target_content"]))
            .collect::<Vec<_>>()
            .join("\n");
        let new = chunks
            .iter()
            .filter_map(|chunk| text_value(chunk, &["ReplacementContent", "replacement_content"]))
            .collect::<Vec<_>>()
            .join("\n");
        (Some(old), Some(new))
    } else {
        let old_text = text_value(
            raw_input,
            &[
                "oldText",
                "old_text",
                "old_content",
                "TargetContent",
                "target_content",
                "before",
            ],
        );
        let new_text = text_value(
            raw_input,
            &[
                "newText",
                "new_text",
                "code_content",
                "CodeContent",
                "ReplacementContent",
                "replacement_content",
                "content",
                "text",
                "after",
            ],
        );
        (old_text, new_text)
    };
    let operation = file_operation(raw_input, hint, old_text.as_deref(), new_text.as_deref());
    let extension = input.raw_extension();
    let snapshots = FileOperationPayload {
        operation,
        summary: format!("{} {}", file_operation_label(operation), public_text(&path)),
        path: public_text(&path),
        old_text,
        new_text,
        patch: None,
        raw_extension: extension.clone(),
    };
    let patch = snapshots.canonical_patch();
    Some(vec![CanonicalAgentEvent::FileOperation(
        FileOperationPayload {
            old_text: None,
            new_text: None,
            patch,
            raw_extension: extension,
            ..snapshots
        },
    )])
}

fn antigravity_collaboration_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let agent_label = input
        .raw_input
        .as_ref()
        .and_then(|value| {
            string_value(
                value,
                &[
                    "role",
                    "Role",
                    "subagent_name",
                    "type_name",
                    "TypeName",
                    "agent",
                    "agentName",
                ],
            )
        })
        .map(|value| public_text(&value));
    let summary = input
        .raw_input
        .as_ref()
        .and_then(|value| {
            string_value(
                value,
                &[
                    "description",
                    "Description",
                    "task_description",
                    "Task",
                    "prompt",
                    "Prompt",
                ],
            )
        })
        .or_else(|| agent_label.clone())
        .unwrap_or_else(|| {
            if input.title.trim().is_empty() {
                input.tool_name.clone()
            } else {
                input.title.clone()
            }
        });
    CanonicalAgentEvent::Collaboration(CollaborationPayload {
        action: public_text(&input.tool_name),
        status: input.status,
        summary: public_text(&summary),
        agent_label,
        delegation_id: None,
        child_session_id: None,
        raw_extension: input.raw_extension(),
    })
}

pub fn normalize_agent_event(
    enricher_kind: AgentEventEnricherKind,
    input: &AgentEventInput,
) -> Vec<NormalizedAgentEvent> {
    let events = match enricher_kind {
        AgentEventEnricherKind::Claude => ClaudeEventEnricher.enrich(input),
        AgentEventEnricherKind::Codex => CodexEventEnricher.enrich(input),
        AgentEventEnricherKind::Hermes => HermesEventEnricher.enrich(input),
        AgentEventEnricherKind::Grok => GrokEventEnricher.enrich(input),
        AgentEventEnricherKind::CodeBuddy => CodeBuddyEventEnricher.enrich(input),
        AgentEventEnricherKind::Antigravity => AntigravityEventEnricher.enrich(input),
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
    let Some(meta) = value.and_then(Value::as_object) else {
        return BTreeMap::new();
    };
    let mut parsed = BTreeMap::new();
    for (key, value) in meta {
        if parsed.len() >= EVENT_META_LIMIT {
            break;
        }
        let normalized = normalized_kind(key);
        // Antigravity carries MCP attribution under `_meta.mcp = {server, tool}`.
        if normalized == "mcp"
            && let Some(mcp) = value.as_object()
        {
            if let Some(server) = string_value(&Value::Object(mcp.clone()), &["server"])
                && parsed.len() < EVENT_META_LIMIT
            {
                parsed.insert("mcp_server".to_string(), server);
            }
            if let Some(tool) = string_value(&Value::Object(mcp.clone()), &["tool"])
                && parsed.len() < EVENT_META_LIMIT
            {
                parsed.insert("mcp_tool".to_string(), tool);
            }
            continue;
        }
        if !allowed_meta_key(&normalized) {
            continue;
        }
        let value = match value {
            Value::String(value) => value.clone(),
            Value::Number(_) | Value::Bool(_) => value.to_string(),
            _ => continue,
        };
        parsed.insert(normalized, value);
    }
    parsed
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
            | "mcp_server"
            | "mcp_tool"
            | "prompt"
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
        .and_then(|value| {
            string_value(
                value,
                &[
                    "cwd",
                    "Cwd",
                    "workingDirectory",
                    "working_dir",
                    "workingDir",
                ],
            )
        })
        .or_else(|| {
            input
                .raw_output
                .as_ref()
                .and_then(|output| serde_json::from_str::<Value>(&output.text).ok())
                .as_ref()
                .and_then(|value| {
                    string_value(
                        value,
                        &["workingDir", "working_dir", "workingDirectory", "cwd"],
                    )
                })
        });
    let exit_code = input
        .meta
        .iter()
        .find(|(key, _)| normalized_kind(key) == "exit_code")
        .map(|(_, value)| value)
        .and_then(|value| value.parse::<i32>().ok())
        .or_else(|| command_exit_code_from_raw_output(input.raw_output.as_ref()));
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

fn command_exit_code_from_raw_output(raw_output: Option<&AgentEventRawOutput>) -> Option<i32> {
    let text = raw_output?.text.as_str();
    let parsed = serde_json::from_str::<Value>(text).ok()?;
    let object = parsed.as_object()?;
    object
        .get("exitCode")
        .or_else(|| object.get("exit_code"))
        .and_then(Value::as_i64)
        .and_then(|code| i32::try_from(code).ok())
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
                let snapshots = FileOperationPayload {
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
                };
                // The durable timeline keeps the patch, not both snapshots.
                // A snapshot is exactly as large as the file it describes, and
                // a page that has to carry them is what makes a long session
                // unopenable; the patch is what a reader renders and is what a
                // detail read returns.
                let patch = snapshots.canonical_patch();
                CanonicalAgentEvent::FileOperation(FileOperationPayload {
                    old_text: None,
                    new_text: None,
                    patch,
                    raw_extension: extension.clone(),
                    ..snapshots
                })
            })
            .collect(),
    )
}

fn web_search_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let query = input
        .raw_input
        .as_ref()
        .and_then(|value| string_value(value, &["query", "Query", "searchQuery", "search_query"]))
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
        delegation_id: None,
        child_session_id: None,
        raw_extension: input.raw_extension(),
    })
}

fn image_event(input: &AgentEventInput) -> CanonicalAgentEvent {
    let mime_type = image_string(input, &["mimeType", "mime_type"]);
    let image_reference = image_string(
        input,
        &[
            "imageReference",
            "imagePath",
            "output_path",
            "generated_image_path",
            "image_uri",
            "uri",
            "url",
        ],
    )
    .filter(|value| !value.trim_start().starts_with("data:"))
    .map(|value| public_text(&value));
    let summary =
        image_string(input, &["prompt", "imageName", "image_name"]).unwrap_or_else(|| {
            if input.title.trim().is_empty() {
                "Image generation".to_string()
            } else {
                input.title.clone()
            }
        });
    CanonicalAgentEvent::ImageGeneration(ImageGenerationPayload {
        status: input.status,
        summary: public_text(&summary),
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
    let value = value?;
    let items = ["items", "steps", "todos", "plan"]
        .iter()
        .find_map(|key| value.get(*key).and_then(Value::as_array))?;
    Some(
        items
            .iter()
            .take(32)
            .filter_map(|item| {
                let title =
                    string_value(item, &["text", "title", "content", "step", "description"])?;
                let status = string_value(item, &["status"])
                    .map(|status| normalized_kind(&status))
                    .map(|status| match status.as_str() {
                        "completed" | "complete" | "done" => PlanStepStatus::Completed,
                        "running" | "in_progress" | "inprogress" | "active" => {
                            PlanStepStatus::Running
                        }
                        "failed" | "error" | "cancelled" | "canceled" => PlanStepStatus::Failed,
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
    value.as_str().map(str::to_string).or_else(|| {
        string_value(
            value,
            &[
                "command",
                "command_line",
                "CommandLine",
                "cmd",
                "shellCommand",
            ],
        )
    })
}

fn is_command_kind(kind: &str) -> bool {
    matches!(
        kind,
        "command" | "command_execution" | "execute" | "shell" | "shell_command" | "terminal"
    )
}

fn contains_image_block(content: Option<&Value>) -> bool {
    content.is_some_and(|content| {
        let is_image = |item: &Value| {
            item.get("type").and_then(Value::as_str) == Some("image")
                || (item.get("type").and_then(Value::as_str) == Some("content")
                    && item
                        .get("content")
                        .and_then(|inner| inner.get("type"))
                        .and_then(Value::as_str)
                        == Some("image"))
        };
        content
            .as_array()
            .map(|items| items.iter().any(is_image))
            .unwrap_or_else(|| is_image(content))
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
    string_value(value, keys).or_else(|| {
        value
            .get("content")
            .and_then(|inner| image_value(inner, keys))
    })
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
            compatibility_identity: "adapter=codex-acp@2.0.1".to_string(),
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
                delegation_id: None,
                child_session_id: None,
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
    fn hermes_titles_restore_tool_identity_and_product_semantics() {
        let mut terminal = input("execute", json!({}));
        terminal.title = "terminal: cargo test -p vibex-agent-acp".to_string();
        terminal.raw_input = None;
        let terminal = normalize_agent_event(AgentEventEnricherKind::Hermes, &terminal);
        let CanonicalAgentEvent::CommandExecution(command) = &terminal[0].event else {
            panic!("expected Hermes terminal command: {terminal:?}");
        };
        assert_eq!(command.command, "cargo test -p vibex-agent-acp");

        let mut write = input("edit", json!({}));
        write.title = "write: src/lib.rs".to_string();
        write.raw_input = None;
        write.content = Some(json!({
            "type": "diff",
            "path": "src/lib.rs",
            "newText": "fn main() {}"
        }));
        let write = normalize_agent_event(AgentEventEnricherKind::Hermes, &write);
        assert!(matches!(
            &write[0].event,
            CanonicalAgentEvent::FileOperation(FileOperationPayload {
                operation: FileOperationKind::Write,
                path,
                ..
            }) if path == "src/lib.rs"
        ));

        let mut read = input("read", json!({}));
        read.title = "read: src/lib.rs".to_string();
        read.raw_input = None;
        let read = normalize_agent_event(AgentEventEnricherKind::Hermes, &read);
        let CanonicalAgentEvent::ToolCall(call) = &read[0].event else {
            panic!("expected generic Hermes read tool: {read:?}");
        };
        assert_eq!(call.tool_name, "read_file");

        let mut todo = input(
            "other",
            json!({
                "items": [{"title": "Run the focused tests", "status": "in_progress"}]
            }),
        );
        todo.title = "todo".to_string();
        let todo = normalize_agent_event(AgentEventEnricherKind::Hermes, &todo);
        assert!(matches!(
            &todo[0].event,
            CanonicalAgentEvent::TodoUpdate(update)
                if update.items.len() == 1
                    && update.items[0].status == PlanStepStatus::Running
        ));

        let mut unknown = input("execute", json!({"description": "not a command"}));
        unknown.title = "execute: maybe a future Hermes tool".to_string();
        let unknown = normalize_agent_event(AgentEventEnricherKind::Hermes, &unknown);
        assert!(matches!(
            &unknown[0].event,
            CanonicalAgentEvent::ToolCall(call) if call.tool_name == "execute"
        ));
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
                // The durable payload carries the rendered patch rather than
                // both snapshots, so the change stays inspectable without
                // paying for the whole file on every page.
                assert_eq!(file.old_text, None);
                assert_eq!(file.new_text, None);
                let patch = file
                    .patch
                    .as_ref()
                    .expect("an edit keeps its rendered patch");
                assert!(patch.text.contains("-before"), "{}", patch.text);
                assert!(patch.text.contains("+after"), "{}", patch.text);
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
    fn acp_v1_diffs_infer_file_lifecycle_from_an_empty_replacement() {
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
        // The lifecycle is inferred from both sides before they are dropped, so
        // an empty replacement is a value here rather than an absence: a file
        // created empty is a write, and an edit that clears its contents stays
        // an edit instead of reading as a delete.
        assert_eq!(files[0].operation, FileOperationKind::Write);
        assert_eq!(files[1].operation, FileOperationKind::Write);
        assert_eq!(files[2].operation, FileOperationKind::Edit);
        assert_eq!(files[3].operation, FileOperationKind::Delete);
        assert_eq!(files[4].operation, FileOperationKind::Edit);
        // What the durable payload keeps is the rendered patch, not the two
        // snapshots. A file created empty has nothing to render, and a file
        // cleared to empty renders the line that went away.
        assert!(files[1].patch.is_none());
        let cleared = files[4].patch.as_ref().expect("a cleared file has a patch");
        assert!(cleared.text.contains("-clear me"), "{}", cleared.text);
        for file in &files {
            assert_eq!(file.old_text, None);
            assert_eq!(file.new_text, None);
        }
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

    #[test]
    fn antigravity_enricher_normalizes_commands_diffs_mcp_and_history_replay() {
        // 1. `run_command` with structured Antigravity exec rawOutput.
        let mut command = input(
            "execute",
            json!({
                "command_line": "cargo test -p vibex-ui",
                "working_dir": "/work/vibex"
            }),
        );
        command.title = "Running run_command".to_string();
        command.raw_output = Some(
            vibex_core::AgentEventRawOutput::new(
                vibex_core::AgentEventRawOutputMode::Snapshot,
                json!({
                    "commandLine": "cargo test -p vibex-ui",
                    "workingDir": "/work/vibex",
                    "exitCode": 2,
                    "exit_code": 2,
                    "combinedOutput": "test failed\n",
                    "formatted_output": "test failed\n"
                })
                .to_string(),
            )
            .0,
        );
        let events = normalize_agent_event(AgentEventEnricherKind::Antigravity, &command);
        match &events[0].event {
            CanonicalAgentEvent::CommandExecution(exec) => {
                assert_eq!(exec.command, "cargo test -p vibex-ui");
                assert_eq!(exec.cwd.as_deref(), Some("/work/vibex"));
                assert_eq!(exec.exit_code, Some(2));
                assert_eq!(exec.output_summary.as_deref(), Some("test failed"));
            }
            other => panic!("expected CommandExecution, got {other:?}"),
        }

        // 2. Historical `session/load` sends non-MCP `rawInput` as a JSON string.
        let mut replay = command.clone();
        replay.raw_input = Some(Value::String(
            json!({"CommandLine": "git status", "Cwd": "/work/vibex"}).to_string(),
        ));
        let replay_events = normalize_agent_event(AgentEventEnricherKind::Antigravity, &replay);
        match &replay_events[0].event {
            CanonicalAgentEvent::CommandExecution(exec) => {
                assert_eq!(exec.command, "git status");
                assert_eq!(exec.cwd.as_deref(), Some("/work/vibex"));
            }
            other => {
                panic!("expected CommandExecution from stringified history input, got {other:?}")
            }
        }

        // 3. Successful edit with ACP diff block -> FileOperation with patch.
        let mut edit = input(
            "edit",
            json!({
                "TargetFile": "/work/src/lib.rs",
                "TargetContent": "before",
                "ReplacementContent": "after"
            }),
        );
        edit.title = "Running replace_file_content".to_string();
        edit.content = Some(json!([{
            "type": "diff",
            "path": "/work/src/lib.rs",
            "oldText": "before\n",
            "newText": "after\n"
        }]));
        let edit_events = normalize_agent_event(AgentEventEnricherKind::Antigravity, &edit);
        match &edit_events[0].event {
            CanonicalAgentEvent::FileOperation(file) => {
                assert_eq!(file.operation, FileOperationKind::Edit);
                assert_eq!(file.path, "/work/src/lib.rs");
                let patch = file.patch.as_ref().expect("diff should render a patch");
                assert!(patch.text.contains("-before"), "{}", patch.text);
                assert!(patch.text.contains("+after"), "{}", patch.text);
            }
            other => panic!("expected FileOperation, got {other:?}"),
        }

        // 4. Failed edit stays a named ToolCall so failure status & error survive.
        let mut failed_edit = edit.clone();
        failed_edit.status = ToolCallStatus::Failed;
        failed_edit.output_summary = Some("permission denied".to_string());
        let failed_events =
            normalize_agent_event(AgentEventEnricherKind::Antigravity, &failed_edit);
        match &failed_events[0].event {
            CanonicalAgentEvent::ToolCall(call) => {
                assert_eq!(call.tool_name, "replace_file_content");
                assert_eq!(call.status, ToolCallStatus::Failed);
                assert_eq!(call.output_summary.as_deref(), Some("permission denied"));
            }
            other => panic!("expected failed edit to remain ToolCall, got {other:?}"),
        }

        // 5. MCP tool call with `_meta.mcp` and duplicated `"arguments"` envelope.
        let mut mcp = input(
            "other",
            json!({
                "emoji": "wave",
                "arguments": {"emoji": "wave"}
            }),
        );
        mcp.title = "Running vibex_react".to_string();
        mcp.meta = parse_event_meta(Some(&json!({
            "mcp": {
                "server": "Agent-delegation",
                "tool": "vibex_react"
            },
            "prompt": "React to the latest message"
        })));
        let mcp_events = normalize_agent_event(AgentEventEnricherKind::Antigravity, &mcp);
        match &mcp_events[0].event {
            CanonicalAgentEvent::ToolCall(call) => {
                assert_eq!(call.tool_name, "mcp__Agent-delegation__vibex_react");
                assert_eq!(call.summary, "React to the latest message");
                assert_eq!(call.input_summary.as_deref(), Some(r#"{"emoji":"wave"}"#));
            }
            other => panic!("expected MCP ToolCall, got {other:?}"),
        }

        // 6. Web search, image generation (nested content block), subagent, todo, interaction.
        let mut search = input("other", json!({"Query": "rust acp protocol"}));
        search.title = "Running search_web".to_string();
        assert!(matches!(
            &normalize_agent_event(AgentEventEnricherKind::Antigravity, &search)[0].event,
            CanonicalAgentEvent::WebSearch(search) if search.query == "rust acp protocol"
        ));

        let mut image = input("other", json!({"Prompt": "sunset over mountains"}));
        image.title = "Running generate_image".to_string();
        image.content = Some(json!([{
            "type": "content",
            "content": {
                "type": "image",
                "mimeType": "image/png",
                "uri": "file:///work/out.png"
            }
        }]));
        assert!(matches!(
            &normalize_agent_event(AgentEventEnricherKind::Antigravity, &image)[0].event,
            CanonicalAgentEvent::ImageGeneration(image)
                if image.mime_type.as_deref() == Some("image/png")
                    && image.image_reference.as_deref() == Some("file:///work/out.png")
        ));

        let mut subagent = input(
            "other",
            json!({"Role": "Codebase Researcher", "Prompt": "Audit ACP wiring"}),
        );
        subagent.title = "Running invoke_subagent".to_string();
        assert!(matches!(
            &normalize_agent_event(AgentEventEnricherKind::Antigravity, &subagent)[0].event,
            CanonicalAgentEvent::Collaboration(collab)
                if collab.agent_label.as_deref() == Some("Codebase Researcher")
                    && collab.summary == "Audit ACP wiring"
        ));

        let mut steps = input(
            "other",
            json!({"steps": [{"title": "Inspect server", "status": "in_progress"}]}),
        );
        steps.title = "Running update_steps".to_string();
        assert!(matches!(
            &normalize_agent_event(AgentEventEnricherKind::Antigravity, &steps)[0].event,
            CanonicalAgentEvent::TodoUpdate(todo)
                if todo.items.len() == 1
                    && todo.items[0].title == "Inspect server"
                    && todo.items[0].status == PlanStepStatus::Running
        ));

        let mut interaction = input("other", json!({"questions": []}));
        interaction.native_event_id = "interaction_42".to_string();
        interaction.title = "Clarify scope".to_string();
        assert!(matches!(
            &normalize_agent_event(AgentEventEnricherKind::Antigravity, &interaction)[0].event,
            CanonicalAgentEvent::ToolCall(call) if call.tool_name == "ask_question"
        ));
    }
}

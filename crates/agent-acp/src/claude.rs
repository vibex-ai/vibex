//! Claude ACP extension and transcript contracts.
//!
//! Standard ACP messages remain handled by the generic runtime. Claude-only
//! `_claude/*` messages and JSONL transcript records are decoded here so they
//! can be bounded, deduplicated and routed through the same canonical event
//! pipeline without leaking provider payloads.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::File;
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::PathBuf;
use std::process::Command;
use std::sync::Arc;

use serde::Serialize;
use serde_json::Value;
use sha2::{Digest, Sha256};
use vibex_agent::{RuntimeMetricName, RuntimeMetricResult, RuntimeObservability};
use vibex_core::ToolCallStatus;

use crate::{AgentEventInput, AgentEventInputSource};

const MAX_ID_LEN: usize = 160;
const MAX_TITLE_LEN: usize = 512;
const MAX_TEXT_LEN: usize = 8 * 1024;
const FINGERPRINT_DOMAIN: &[u8] = b"vibex/claude-prompt-fingerprint/v1";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaudeExtensionEvent {
    pub native_event_id: String,
    pub tool_name: String,
    pub title: String,
    pub status: ToolCallStatus,
    pub raw_input: Option<Value>,
    pub output_summary: Option<String>,
    pub meta: BTreeMap<String, String>,
}

impl ClaudeExtensionEvent {
    pub fn into_event_input(self, compatibility_identity: &str) -> AgentEventInput {
        AgentEventInput {
            source: AgentEventInputSource::Live,
            compatibility_identity: compatibility_identity.to_string(),
            native_event_id: self.native_event_id,
            tool_name: self.tool_name,
            title: self.title,
            status: self.status,
            raw_input: self.raw_input,
            output_summary: self.output_summary,
            raw_output: None,
            content: None,
            locations: Vec::new(),
            meta: self.meta,
        }
    }
}

/// Decodes only versioned Claude extensions. Unknown `_claude/*` methods are
/// deliberately ignored so an adapter upgrade cannot break standard ACP.
pub fn decode_claude_extension(method: &str, params: &Value) -> Option<ClaudeExtensionEvent> {
    let kind = method.strip_prefix("_claude/")?;
    let kind = kind.to_ascii_lowercase();
    let event_kind = match kind.as_str() {
        "background_task" | "background_agent" | "task_notification" | "background_shell"
        | "task_update" => kind,
        _ => return None,
    };
    let native_event_id = bounded_id(
        first_string(params, &["taskId", "task_id", "eventId", "event_id", "id"])
            .unwrap_or_else(|| stable_extension_id(method, params)),
    );
    let title = bounded_text(
        first_string(params, &["title", "name", "description", "message"])
            .unwrap_or_else(|| event_kind.replace('_', " ")),
        MAX_TITLE_LEN,
    );
    let state = first_string(params, &["status", "state", "phase"])
        .unwrap_or_else(|| "started".to_string())
        .to_ascii_lowercase();
    let status = match state.as_str() {
        "completed" | "complete" | "done" | "settled" | "success" => ToolCallStatus::Completed,
        "failed" | "error" | "cancelled" | "canceled" => ToolCallStatus::Failed,
        "progress" | "running" | "in_progress" => ToolCallStatus::Progress,
        _ => ToolCallStatus::Started,
    };
    let output_summary = first_string(params, &["output", "result", "summary", "message"])
        .map(|value| bounded_text(value, MAX_TEXT_LEN));
    let mut meta = BTreeMap::new();
    meta.insert("kind".to_string(), event_kind.clone());
    if let Some(agent) = first_string(params, &["agent", "agentName", "agent_name"]) {
        meta.insert("agent".to_string(), bounded_text(agent, MAX_TITLE_LEN));
    }
    Some(ClaudeExtensionEvent {
        native_event_id,
        tool_name: format!("claude_{event_kind}"),
        title,
        status,
        raw_input: Some(params.clone()),
        output_summary,
        meta,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ClaudeTranscriptEventKind {
    Prompt,
    AgentMessage,
    Thinking,
    ToolCall,
    BackgroundTask,
    BackgroundShell,
    TaskNotification,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClaudeTranscriptEvent {
    pub native_event_id: String,
    pub session_id: Option<String>,
    pub kind: ClaudeTranscriptEventKind,
    pub text: Option<String>,
    pub prompt_fingerprint: Option<String>,
    pub tool_name: Option<String>,
    pub status: ToolCallStatus,
    pub raw: Value,
}

pub fn claude_transcript_event_input(
    event: &ClaudeTranscriptEvent,
    compatibility_identity: &str,
) -> AgentEventInput {
    let (tool_name, title) = match event.kind {
        ClaudeTranscriptEventKind::BackgroundTask => {
            ("claude_background_task", "Claude background task")
        }
        ClaudeTranscriptEventKind::BackgroundShell => {
            ("claude_background_shell", "Claude background shell")
        }
        ClaudeTranscriptEventKind::TaskNotification => {
            ("claude_task_notification", "Claude task notification")
        }
        ClaudeTranscriptEventKind::ToolCall => (
            event.tool_name.as_deref().unwrap_or("claude_tool"),
            "Claude tool call",
        ),
        ClaudeTranscriptEventKind::Prompt => ("claude_prompt", "Claude prompt"),
        ClaudeTranscriptEventKind::AgentMessage => ("claude_message", "Claude message"),
        ClaudeTranscriptEventKind::Thinking => ("claude_thinking", "Claude thinking"),
    };
    let mut meta = BTreeMap::new();
    meta.insert("kind".to_string(), tool_name.to_string());
    AgentEventInput {
        source: AgentEventInputSource::Transcript,
        compatibility_identity: compatibility_identity.to_string(),
        native_event_id: event.native_event_id.clone(),
        tool_name: tool_name.to_string(),
        title: title.to_string(),
        status: event.status,
        raw_input: Some(event.raw.clone()),
        output_summary: event.text.clone(),
        raw_output: None,
        content: None,
        locations: Vec::new(),
        meta,
    }
}

impl fmt::Debug for ClaudeTranscriptEvent {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("ClaudeTranscriptEvent")
            .field("native_event_id", &"<redacted>")
            .field(
                "session_id",
                &self.session_id.as_ref().map(|_| "<redacted>"),
            )
            .field("kind", &self.kind)
            .field("has_text", &self.text.is_some())
            .field("prompt_fingerprint", &self.prompt_fingerprint)
            .field("tool_name", &self.tool_name)
            .field("status", &self.status)
            .finish()
    }
}

/// Parses one Claude JSONL transcript line. Unsupported records return `None`;
/// malformed JSON is an error so a watcher can record a bounded diagnostic and
/// continue with the next line.
pub fn parse_claude_transcript_line(line: &str) -> Result<Option<ClaudeTranscriptEvent>, String> {
    let value: Value = serde_json::from_str(line).map_err(|_| "malformed_json".to_string())?;
    let object = value
        .as_object()
        .ok_or_else(|| "transcript_record_not_object".to_string())?;
    let record_type = object
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let native_event_id = bounded_id(
        first_string(&value, &["uuid", "id", "eventId"])
            .unwrap_or_else(|| stable_extension_id(record_type, &value)),
    );
    let session_id = first_string(&value, &["sessionId", "session_id"]);
    let message = value.get("message").unwrap_or(&value);
    let content = message.get("content").unwrap_or(message);
    let text = transcript_text(content).map(|text| bounded_text(text, MAX_TEXT_LEN));
    let prompt_fingerprint = (record_type == "user")
        .then(|| text.as_deref().map(claude_prompt_fingerprint))
        .flatten();
    let kind = match record_type {
        "user" => ClaudeTranscriptEventKind::Prompt,
        "assistant" | "result" => {
            if contains_content_type(content, "thinking") {
                ClaudeTranscriptEventKind::Thinking
            } else if contains_content_type(content, "tool_use") {
                ClaudeTranscriptEventKind::ToolCall
            } else {
                ClaudeTranscriptEventKind::AgentMessage
            }
        }
        "background_task" | "background_agent" | "task_update" => {
            ClaudeTranscriptEventKind::BackgroundTask
        }
        "background_shell" => ClaudeTranscriptEventKind::BackgroundShell,
        "task_notification" => ClaudeTranscriptEventKind::TaskNotification,
        _ => return Ok(None),
    };
    let status = transcript_status(&value);
    let tool_name = content
        .as_array()
        .and_then(|blocks| {
            blocks.iter().find_map(|block| {
                (block.get("type").and_then(Value::as_str) == Some("tool_use"))
                    .then(|| first_string(block, &["name", "toolName", "tool_name"]))
                    .flatten()
            })
        })
        .map(|name| bounded_text(name, MAX_TITLE_LEN));
    Ok(Some(ClaudeTranscriptEvent {
        native_event_id,
        session_id,
        kind,
        text,
        prompt_fingerprint,
        tool_name,
        status,
        raw: value,
    }))
}

pub fn claude_prompt_fingerprint(text: &str) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
    let mut hasher = Sha256::new();
    hasher.update(FINGERPRINT_DOMAIN);
    hasher.update((normalized.len() as u64).to_be_bytes());
    hasher.update(normalized.as_bytes());
    let digest = hasher.finalize();
    format!(
        "claude_prompt_{}",
        digest[..16]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

#[derive(Debug, Default)]
pub struct ClaudeTranscriptDeduper {
    seen_event_ids: BTreeSet<String>,
    live_prompt_fingerprints: BTreeSet<String>,
}

/// Incremental, read-only JSONL tail reader used by Claude background/task
/// compensation. The file offset is local watcher state; records are still
/// fenced by the caller's binding and activation generation.
#[derive(Debug)]
pub struct ClaudeTranscriptTailWatcher {
    path: PathBuf,
    offset: u64,
    deduper: ClaudeTranscriptDeduper,
    observability: Option<Arc<RuntimeObservability>>,
}

impl ClaudeTranscriptTailWatcher {
    pub fn new(path: impl Into<PathBuf>) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            deduper: ClaudeTranscriptDeduper::default(),
            observability: None,
        }
    }

    pub fn with_observability(
        path: impl Into<PathBuf>,
        observability: Arc<RuntimeObservability>,
    ) -> Self {
        Self {
            path: path.into(),
            offset: 0,
            deduper: ClaudeTranscriptDeduper::default(),
            observability: Some(observability),
        }
    }

    pub fn relocate(&mut self, path: impl Into<PathBuf>) {
        self.path = path.into();
        self.offset = 0;
        self.deduper = ClaudeTranscriptDeduper::default();
    }

    pub fn observe_live_prompt(&mut self, text: &str) {
        self.deduper.observe_live_prompt(text);
    }

    pub fn poll(&mut self) -> Result<Vec<ClaudeTranscriptEvent>, String> {
        let mut file = File::open(&self.path).map_err(|_| "transcript_open_failed".to_string())?;
        let metadata = file
            .metadata()
            .map_err(|_| "transcript_metadata_failed".to_string())?;
        let length = metadata.len();
        if length < self.offset {
            self.offset = 0;
            self.deduper = ClaudeTranscriptDeduper::default();
        }
        file.seek(SeekFrom::Start(self.offset))
            .map_err(|_| "transcript_seek_failed".to_string())?;
        let mut reader = BufReader::new(file);
        let mut events = Vec::new();
        loop {
            let before = reader
                .stream_position()
                .map_err(|_| "transcript_position_failed".to_string())?;
            let mut line = String::new();
            let read = reader
                .read_line(&mut line)
                .map_err(|_| "transcript_read_failed".to_string())?;
            if read == 0 {
                self.offset = before;
                break;
            }
            let after = reader
                .stream_position()
                .map_err(|_| "transcript_position_failed".to_string())?;
            self.offset = after;
            if !line.ends_with('\n') {
                // Keep a partial final line for the next poll.
                self.offset = before;
                break;
            }
            if let Some(event) = parse_claude_transcript_line(line.trim_end())?
                && self.deduper.should_emit(&event)
            {
                events.push(event);
            }
        }
        if !events.is_empty()
            && let Some(observability) = self.observability.as_ref()
            && let Ok(modified) = metadata.modified()
            && let Ok(lag) = modified.elapsed()
        {
            observability.observe_duration(
                RuntimeMetricName::TranscriptWatcherLag,
                None,
                RuntimeMetricResult::Success,
                lag,
            );
        }
        Ok(events)
    }
}

impl ClaudeTranscriptDeduper {
    pub fn observe_live_prompt(&mut self, text: &str) {
        self.live_prompt_fingerprints
            .insert(claude_prompt_fingerprint(text));
    }

    /// Returns false when a transcript record is a duplicate of a live prompt
    /// or a record already observed by this watcher.
    pub fn should_emit(&mut self, event: &ClaudeTranscriptEvent) -> bool {
        if event.kind == ClaudeTranscriptEventKind::Prompt
            && event
                .prompt_fingerprint
                .as_ref()
                .is_some_and(|fingerprint| self.live_prompt_fingerprints.contains(fingerprint))
        {
            return false;
        }
        self.seen_event_ids.insert(event.native_event_id.clone())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord)]
pub struct ClaudeWorkKey {
    pub binding_id: String,
    pub activation_generation: i64,
}

#[derive(Debug, Default)]
pub struct ClaudeBackgroundWorkRegistry {
    active: BTreeMap<ClaudeWorkKey, BTreeSet<String>>,
}

#[derive(Debug, Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ClaudeAcpSmokeResult {
    pub command: String,
    pub workspace_path: PathBuf,
    pub prompt: String,
    pub version_output: Option<String>,
    pub started: bool,
}

#[derive(Debug, thiserror::Error)]
pub enum ClaudeAcpSmokeError {
    #[error("Claude ACP smoke workspace failed: {0}")]
    Workspace(String),
    #[error("Claude ACP adapter is unavailable: {0}")]
    AdapterUnavailable(String),
}

/// Explicit real-adapter smoke entry point. It is intentionally never called
/// by default tests; the command only probes the configured adapter binary and
/// leaves credentials and transcript files untouched.
pub async fn run_claude_agent_acp_smoke(
    prompt: Option<String>,
) -> Result<ClaudeAcpSmokeResult, ClaudeAcpSmokeError> {
    let workspace_path = crate::resolve_agent_smoke_workspace("claude-acp", "direct")
        .map_err(|error| ClaudeAcpSmokeError::Workspace(error.to_string()))?;
    let command = std::env::var("VIBEX_CLAUDE_ACP_COMMAND")
        .unwrap_or_else(|_| "claude-agent-acp".to_string());
    let output = Command::new(&command)
        .arg("--version")
        .output()
        .map_err(|error| ClaudeAcpSmokeError::AdapterUnavailable(error.to_string()))?;
    if !output.status.success() {
        return Err(ClaudeAcpSmokeError::AdapterUnavailable(
            "adapter version probe returned a failure status".to_string(),
        ));
    }
    Ok(ClaudeAcpSmokeResult {
        command,
        workspace_path,
        prompt: prompt.unwrap_or_else(|| "Reply with a Claude ACP smoke marker.".to_string()),
        version_output: String::from_utf8(output.stdout)
            .ok()
            .map(|value| bounded_text(value, MAX_TITLE_LEN)),
        started: true,
    })
}

impl ClaudeBackgroundWorkRegistry {
    pub fn begin(&mut self, key: ClaudeWorkKey, work_id: impl Into<String>) {
        self.active.entry(key).or_default().insert(work_id.into());
    }

    pub fn finish(&mut self, key: &ClaudeWorkKey, work_id: &str) {
        if let Some(work) = self.active.get_mut(key) {
            work.remove(work_id);
            if work.is_empty() {
                self.active.remove(key);
            }
        }
    }

    pub fn reposition(&mut self, from: &ClaudeWorkKey, to: ClaudeWorkKey) {
        if let Some(work) = self.active.remove(from) {
            self.active.entry(to).or_default().extend(work);
        }
    }

    pub fn has_active(&self, key: &ClaudeWorkKey) -> bool {
        self.active.get(key).is_some_and(|work| !work.is_empty())
    }

    pub fn active_count(&self, key: &ClaudeWorkKey) -> usize {
        self.active.get(key).map(BTreeSet::len).unwrap_or_default()
    }

    pub fn can_idle_sweep(&self, key: &ClaudeWorkKey) -> bool {
        !self.has_active(key)
    }
}

fn transcript_text(value: &Value) -> Option<String> {
    match value {
        Value::String(text) => Some(text.clone()),
        Value::Array(blocks) => blocks.iter().find_map(|block| {
            if matches!(
                block.get("type").and_then(Value::as_str),
                Some("text") | Some("thinking")
            ) {
                block
                    .get("text")
                    .or_else(|| block.get("thinking"))
                    .and_then(Value::as_str)
                    .map(ToString::to_string)
            } else {
                None
            }
        }),
        _ => None,
    }
}

fn contains_content_type(value: &Value, expected: &str) -> bool {
    value.as_array().is_some_and(|blocks| {
        blocks
            .iter()
            .any(|block| block.get("type").and_then(Value::as_str) == Some(expected))
    })
}

fn transcript_status(value: &Value) -> ToolCallStatus {
    match first_string(value, &["status", "state", "phase"])
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "completed" | "complete" | "done" | "settled" => ToolCallStatus::Completed,
        "failed" | "error" | "cancelled" | "canceled" => ToolCallStatus::Failed,
        "running" | "in_progress" | "progress" => ToolCallStatus::Progress,
        _ => ToolCallStatus::Started,
    }
}

fn first_string(value: &Value, keys: &[&str]) -> Option<String> {
    keys.iter().find_map(|key| {
        value
            .get(*key)
            .and_then(Value::as_str)
            .map(ToString::to_string)
    })
}

fn stable_extension_id(method: &str, params: &Value) -> String {
    let mut hasher = Sha256::new();
    hasher.update(method.as_bytes());
    hasher.update(params.to_string().as_bytes());
    let digest = hasher.finalize();
    format!(
        "claude_ext_{}",
        digest[..12]
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    )
}

fn bounded_id(value: String) -> String {
    bounded_text(value, MAX_ID_LEN)
}

fn bounded_text(value: String, limit: usize) -> String {
    let value = value.trim();
    if value.len() <= limit {
        return value.to_string();
    }
    let mut end = limit;
    while end > 0 && !value.is_char_boundary(end) {
        end -= 1;
    }
    value[..end].to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extension_decoder_is_versioned_and_bounded() {
        let event = decode_claude_extension(
            "_claude/background_task",
            &serde_json::json!({
                "taskId": "task-1",
                "title": "Index repository",
                "status": "running",
                "prompt": "secret prompt must remain bounded"
            }),
        )
        .unwrap();
        assert_eq!(event.tool_name, "claude_background_task");
        assert_eq!(event.status, ToolCallStatus::Progress);
        assert!(decode_claude_extension("_claude/unknown", &Value::Null).is_none());
        let input = event.into_event_input("claude-code-acp@1");
        assert_eq!(input.source, AgentEventInputSource::Live);
        assert_eq!(input.compatibility_identity, "claude-code-acp@1");
    }

    #[test]
    fn transcript_parser_and_deduper_drop_live_prompt_duplicate() {
        let line = r#"{"type":"user","sessionId":"native-1","uuid":"event-1","message":{"content":"  fix   the bug "}}"#;
        let event = parse_claude_transcript_line(line).unwrap().unwrap();
        assert_eq!(event.kind, ClaudeTranscriptEventKind::Prompt);
        assert_eq!(
            event.prompt_fingerprint.as_deref(),
            Some(claude_prompt_fingerprint("fix the bug").as_str())
        );
        let mut deduper = ClaudeTranscriptDeduper::default();
        deduper.observe_live_prompt("fix the bug");
        assert!(!deduper.should_emit(&event));
        assert!(!format!("{event:?}").contains("native-1"));
    }

    #[test]
    fn background_work_is_fenced_and_repositionable() {
        let old = ClaudeWorkKey {
            binding_id: "binding-1".to_string(),
            activation_generation: 1,
        };
        let new = ClaudeWorkKey {
            binding_id: "binding-1".to_string(),
            activation_generation: 2,
        };
        let mut registry = ClaudeBackgroundWorkRegistry::default();
        registry.begin(old.clone(), "task-1");
        registry.begin(old.clone(), "task-2");
        assert_eq!(registry.active_count(&old), 2);
        assert!(!registry.can_idle_sweep(&old));
        registry.reposition(&old, new.clone());
        assert_eq!(registry.active_count(&new), 2);
        assert!(!registry.can_idle_sweep(&new));
        assert!(registry.can_idle_sweep(&old));
        registry.finish(&new, "task-1");
        assert_eq!(registry.active_count(&new), 1);
        registry.finish(&new, "task-2");
        assert!(registry.can_idle_sweep(&new));
    }

    #[test]
    fn transcript_events_share_the_canonical_input_contract() {
        let event = parse_claude_transcript_line(
            r#"{"type":"assistant","uuid":"assistant-1","message":{"content":[{"type":"tool_use","name":"Bash"}]}}"#,
        )
        .unwrap()
        .unwrap();
        let input = claude_transcript_event_input(&event, "claude-code-acp@1");
        assert_eq!(input.source, AgentEventInputSource::Transcript);
        assert_eq!(input.compatibility_identity, "claude-code-acp@1");
        assert!(input.raw_input.is_some());
    }

    #[test]
    fn transcript_tail_watcher_reads_complete_lines_and_relocates() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("session.jsonl");
        std::fs::write(
            &path,
            "{\"type\":\"user\",\"uuid\":\"u1\",\"message\":{\"content\":\"hello\"}}\n",
        )
        .unwrap();
        let mut watcher = ClaudeTranscriptTailWatcher::new(&path);
        assert_eq!(watcher.poll().unwrap().len(), 1);
        assert!(watcher.poll().unwrap().is_empty());
        let other = directory.path().join("resumed.jsonl");
        std::fs::write(
            &other,
            "{\"type\":\"assistant\",\"uuid\":\"a1\",\"message\":{\"content\":\"done\"}}\n",
        )
        .unwrap();
        watcher.relocate(&other);
        assert_eq!(watcher.poll().unwrap().len(), 1);
    }
}

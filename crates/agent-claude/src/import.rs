use std::collections::BTreeSet;
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use vibex_agent::AgentManager;
use vibex_core::{
    AgentMessagePayload, CorrelationId, ExternalSessionContinuationStatus,
    ExternalSessionImportCandidate, ExternalSessionImportCandidateStatus,
    ExternalSessionImportDiagnostic, ExternalSessionImportPreview, ExternalSessionImportRequest,
    ExternalSessionImportResult, ExternalSessionImportSource, ExternalSessionImportTimelineItem,
    ProviderBindingMetadata, ProviderKind, ProviderProfileId, ReasoningPayload, TimelinePayload,
    TimelineRedactionState, TimelineSource, ToolCallPayload, ToolCallStatus, UserMessagePayload,
    VibexError, VibexResult, WorkspaceMode,
};

const DEFAULT_IMPORT_LIMIT: usize = 50;
const MAX_IMPORT_LIMIT: usize = 200;
const MAX_DIAGNOSTICS_PER_CANDIDATE: usize = 32;
const MISSING_NATIVE_SESSION_ID: &str = "missing_native_session_id";
const AMBIGUOUS_NATIVE_SESSION_ID: &str = "ambiguous_native_session_id";

#[derive(Debug, Clone)]
pub struct ClaudeSessionImportPreviewRequest {
    pub paths: Vec<PathBuf>,
    pub workspace_root: Option<String>,
    pub workspace_mode: WorkspaceMode,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub correlation_id: Option<CorrelationId>,
    pub limit: Option<usize>,
}

pub fn preview_claude_external_sessions(
    request: ClaudeSessionImportPreviewRequest,
) -> VibexResult<ExternalSessionImportPreview> {
    let paths = discover_jsonl_paths(&request.paths, request.limit)?;
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();

    for path in paths {
        match parse_claude_jsonl_candidate(&path, &request) {
            Ok(candidate) => {
                diagnostics.extend(candidate.diagnostics.clone());
                candidates.push(candidate);
            }
            Err(err) => diagnostics.push(diagnostic(
                "claude_import_file_unreadable",
                "Claude import candidate could not be read",
                vec![
                    detail("pathHash", stable_hash_hex(path.display().to_string())),
                    detail("error", bounded(err.to_string(), 160)),
                ],
            )),
        }
    }

    Ok(ExternalSessionImportPreview {
        candidates,
        diagnostics,
        correlation_id: request.correlation_id,
    })
}

pub async fn import_selected_claude_sessions(
    manager: &AgentManager,
    candidates: Vec<ExternalSessionImportCandidate>,
    correlation_id: Option<CorrelationId>,
) -> VibexResult<ExternalSessionImportResult> {
    for candidate in &candidates {
        if candidate.source != ExternalSessionImportSource::Claude
            || candidate.provider_kind != ProviderKind::Claude
        {
            return Err(VibexError::validation(
                "claude_import_candidate_provider_mismatch",
                "selected import candidate is not a Claude candidate",
            )
            .with_diagnostic("candidateId", &candidate.candidate_id)
            .with_diagnostic("source", candidate.source.to_string())
            .with_diagnostic("providerKind", candidate.provider_kind.to_string()));
        }
    }

    manager
        .import_external_sessions(ExternalSessionImportRequest {
            candidates,
            correlation_id,
        })
        .await
}

fn discover_jsonl_paths(paths: &[PathBuf], limit: Option<usize>) -> VibexResult<Vec<PathBuf>> {
    if paths.is_empty() {
        return Err(VibexError::validation(
            "claude_import_paths_empty",
            "Claude import preview requires at least one explicit fixture or read-only path",
        ));
    }

    let limit = limit
        .unwrap_or(DEFAULT_IMPORT_LIMIT)
        .clamp(1, MAX_IMPORT_LIMIT);
    let mut discovered = Vec::new();
    for path in paths {
        if path.is_file() {
            discovered.push(path.clone());
        } else if path.is_dir() {
            collect_jsonl_paths_recursive(path, &mut discovered, limit)?;
        } else {
            return Err(VibexError::validation(
                "claude_import_path_missing",
                "Claude import path must exist and be readable",
            )
            .with_diagnostic("pathHash", stable_hash_hex(path.display().to_string())));
        }
        if discovered.len() >= limit {
            break;
        }
    }
    discovered.truncate(limit);
    Ok(discovered)
}

fn collect_jsonl_paths_recursive(
    directory: &Path,
    discovered: &mut Vec<PathBuf>,
    limit: usize,
) -> VibexResult<()> {
    if discovered.len() >= limit {
        return Ok(());
    }

    let mut entries = Vec::new();
    for entry in std::fs::read_dir(directory).map_err(|err| {
        VibexError::storage("claude_import_directory_unreadable", err.to_string())
            .with_diagnostic("pathHash", stable_hash_hex(directory.display().to_string()))
    })? {
        let entry = entry.map_err(|err| {
            VibexError::storage("claude_import_directory_entry_unreadable", err.to_string())
                .with_diagnostic("pathHash", stable_hash_hex(directory.display().to_string()))
        })?;
        let file_type = entry.file_type().map_err(|err| {
            VibexError::storage("claude_import_directory_entry_unreadable", err.to_string())
                .with_diagnostic(
                    "pathHash",
                    stable_hash_hex(entry.path().display().to_string()),
                )
        })?;
        entries.push((entry.path(), file_type));
    }

    entries.sort_by(|left, right| left.0.cmp(&right.0));
    for (entry_path, file_type) in entries {
        if discovered.len() >= limit {
            break;
        }
        if (file_type.is_file() || (file_type.is_symlink() && entry_path.is_file()))
            && entry_path
                .extension()
                .is_some_and(|extension| extension == "jsonl")
        {
            discovered.push(entry_path);
        } else if file_type.is_dir() {
            collect_jsonl_paths_recursive(&entry_path, discovered, limit)?;
        }
    }

    Ok(())
}

fn parse_claude_jsonl_candidate(
    path: &Path,
    request: &ClaudeSessionImportPreviewRequest,
) -> VibexResult<ExternalSessionImportCandidate> {
    let file = File::open(path).map_err(|err| {
        VibexError::storage("claude_import_file_open_failed", err.to_string())
            .with_diagnostic("pathHash", stable_hash_hex(path.display().to_string()))
    })?;
    let mut state = ParseState {
        path,
        workspace_root: request.workspace_root.clone(),
        native_session_ids: BTreeSet::new(),
        updated_at_ms: file_modified_at_ms(path),
        timeline_items: Vec::new(),
        diagnostics: Vec::new(),
    };

    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        let line_number = line_index + 1;
        let line = match line {
            Ok(line) => line,
            Err(err) => {
                state.push_diagnostic(diagnostic(
                    "claude_import_line_read_failed",
                    "Claude import skipped an unreadable JSONL line",
                    vec![
                        detail("line", line_number.to_string()),
                        detail("error", bounded(err.to_string(), 160)),
                    ],
                ));
                continue;
            }
        };
        if line.trim().is_empty() {
            continue;
        }
        let value = match serde_json::from_str::<serde_json::Value>(&line) {
            Ok(value) => value,
            Err(_) => {
                state.push_diagnostic(diagnostic(
                    "claude_import_malformed_jsonl",
                    "Claude import skipped a malformed JSONL line",
                    vec![detail("line", line_number.to_string())],
                ));
                continue;
            }
        };
        state.ingest_record(line_number, &value);
    }

    let (native_session_id, continuation_status, continuation_reason) =
        continuation_from_ids(&state.native_session_ids);
    let workspace_root = state
        .workspace_root
        .filter(|value| has_text(value))
        .unwrap_or_else(|| "/tmp/vibex-claude-import".to_string());
    let candidate_id = candidate_id(path, native_session_id.as_deref());
    let title = title_for(path, native_session_id.as_deref());
    let status = if state.timeline_items.is_empty() {
        ExternalSessionImportCandidateStatus::Blocked
    } else {
        ExternalSessionImportCandidateStatus::Importable
    };

    Ok(ExternalSessionImportCandidate {
        candidate_id,
        source: ExternalSessionImportSource::Claude,
        provider_kind: ProviderKind::Claude,
        provider_profile_id: request.provider_profile_id.clone(),
        workspace_root,
        workspace_mode: request.workspace_mode,
        title,
        native_session_id,
        native_thread_id: None,
        native_resume_token: None,
        continuation_status,
        continuation_reason,
        updated_at_ms: state.updated_at_ms,
        session_config_state: None,
        status,
        redaction_state: TimelineRedactionState::None,
        timeline_items: state.timeline_items,
        diagnostics: state.diagnostics,
    })
}

struct ParseState<'a> {
    path: &'a Path,
    workspace_root: Option<String>,
    native_session_ids: BTreeSet<String>,
    updated_at_ms: Option<i64>,
    timeline_items: Vec<ExternalSessionImportTimelineItem>,
    diagnostics: Vec<ExternalSessionImportDiagnostic>,
}

impl ParseState<'_> {
    fn ingest_record(&mut self, line_number: usize, value: &serde_json::Value) {
        if let Some(session_id) = text_field(value, "sessionId").filter(|value| has_text(value)) {
            self.native_session_ids.insert(session_id.to_string());
        }
        if self.workspace_root.is_none() {
            self.workspace_root = text_field(value, "cwd")
                .filter(|value| has_text(value))
                .map(ToOwned::to_owned);
        }

        let record_type = text_field(value, "type").unwrap_or("missing");
        let Some(message) = value.get("message") else {
            self.push_unsupported(line_number, record_type, None, None);
            return;
        };
        let role = text_field(message, "role").or(match record_type {
            "user" => Some("user"),
            "assistant" => Some("assistant"),
            _ => None,
        });
        let provider_correlation_id = text_field(value, "uuid")
            .map(ToOwned::to_owned)
            .or_else(|| Some(format!("line-{line_number}")));

        match role {
            Some("user") => self.ingest_user_message(line_number, message, provider_correlation_id),
            Some("assistant") => {
                self.ingest_assistant_message(line_number, message, provider_correlation_id)
            }
            Some(role) => self.push_unsupported(line_number, record_type, None, Some(role)),
            None => self.push_unsupported(line_number, record_type, None, None),
        }
    }

    fn ingest_user_message(
        &mut self,
        line_number: usize,
        message: &serde_json::Value,
        provider_correlation_id: Option<String>,
    ) {
        if let Some(text) = extract_message_text(message).filter(|value| has_text(value)) {
            self.timeline_items.push(import_item(
                TimelineSource::User,
                TimelinePayload::UserMessage(UserMessagePayload {
                    text,
                    attachments: Vec::new(),
                }),
                provider_correlation_id,
            ));
        } else {
            self.push_empty_content(line_number, "user");
        }
    }

    fn ingest_assistant_message(
        &mut self,
        line_number: usize,
        message: &serde_json::Value,
        provider_correlation_id: Option<String>,
    ) {
        let Some(content) = message.get("content") else {
            self.push_empty_content(line_number, "assistant");
            return;
        };

        match content {
            serde_json::Value::String(text) if has_text(text) => {
                self.timeline_items.push(import_item(
                    TimelineSource::Agent,
                    TimelinePayload::AgentMessage(AgentMessagePayload {
                        text: text.clone(),
                        is_final: true,
                    }),
                    provider_correlation_id,
                ));
            }
            serde_json::Value::Array(parts) => {
                let before = self.timeline_items.len();
                for (index, part) in parts.iter().enumerate() {
                    self.ingest_assistant_content_block(
                        line_number,
                        index,
                        part,
                        provider_correlation_id.clone(),
                    );
                }
                if self.timeline_items.len() == before {
                    self.push_empty_content(line_number, "assistant");
                }
            }
            _ => self.push_empty_content(line_number, "assistant"),
        }
    }

    fn ingest_assistant_content_block(
        &mut self,
        line_number: usize,
        block_index: usize,
        block: &serde_json::Value,
        provider_correlation_id: Option<String>,
    ) {
        match text_field(block, "type") {
            Some("text") => {
                if let Some(text) = text_field(block, "text").filter(|value| has_text(value)) {
                    self.timeline_items.push(import_item(
                        TimelineSource::Agent,
                        TimelinePayload::AgentMessage(AgentMessagePayload {
                            text: text.to_string(),
                            is_final: true,
                        }),
                        block_correlation_id(provider_correlation_id, block_index),
                    ));
                }
            }
            Some("thinking") => {
                if let Some(text) = text_field(block, "thinking").filter(|value| has_text(value)) {
                    self.timeline_items.push(import_item(
                        TimelineSource::Agent,
                        TimelinePayload::Reasoning(ReasoningPayload {
                            text: text.to_string(),
                            is_final: true,
                        }),
                        block_correlation_id(provider_correlation_id, block_index),
                    ));
                }
            }
            Some("tool_use") => {
                let tool_call_id = text_field(block, "id").unwrap_or("unknown-tool-use");
                let tool_name = text_field(block, "name").unwrap_or("tool_use");
                self.timeline_items.push(import_item(
                    TimelineSource::Agent,
                    TimelinePayload::ToolCall(ToolCallPayload {
                        tool_call_id: bounded(tool_call_id, 160),
                        tool_name: bounded(tool_name, 120),
                        status: ToolCallStatus::Started,
                        summary: "Imported Claude tool use".to_string(),
                        input_summary: Some("Claude tool input omitted from import".to_string()),
                        output_summary: None,
                        raw_extension: None,
                    }),
                    block_correlation_id(provider_correlation_id, block_index),
                ));
            }
            Some("tool_result") => {
                let tool_call_id = text_field(block, "tool_use_id").unwrap_or("unknown-tool-use");
                let is_error = block
                    .get("is_error")
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false);
                self.timeline_items.push(import_item(
                    TimelineSource::Agent,
                    TimelinePayload::ToolCall(ToolCallPayload {
                        tool_call_id: bounded(tool_call_id, 160),
                        tool_name: "tool_result".to_string(),
                        status: if is_error {
                            ToolCallStatus::Failed
                        } else {
                            ToolCallStatus::Completed
                        },
                        summary: "Imported Claude tool result".to_string(),
                        input_summary: None,
                        output_summary: Some("Claude tool output omitted from import".to_string()),
                        raw_extension: None,
                    }),
                    block_correlation_id(provider_correlation_id, block_index),
                ));
            }
            Some(item_type) => {
                self.push_unsupported(line_number, "assistant_content", Some(item_type), None);
            }
            None => self.push_unsupported(line_number, "assistant_content", None, None),
        }
    }

    fn push_empty_content(&mut self, line_number: usize, content_kind: &str) {
        self.push_diagnostic(diagnostic(
            "claude_import_empty_content",
            "Claude import skipped an empty supported record",
            vec![
                detail("line", line_number.to_string()),
                detail("contentKind", content_kind),
            ],
        ));
    }

    fn push_unsupported(
        &mut self,
        line_number: usize,
        record_type: &str,
        item_type: Option<&str>,
        role: Option<&str>,
    ) {
        let mut details = vec![
            detail("line", line_number.to_string()),
            detail("recordType", bounded(record_type, 80)),
            detail("pathHash", stable_hash_hex(self.path.display().to_string())),
        ];
        if let Some(item_type) = item_type {
            details.push(detail("itemType", bounded(item_type, 80)));
        }
        if let Some(role) = role {
            details.push(detail("role", bounded(role, 80)));
        }
        self.push_diagnostic(diagnostic(
            "claude_import_unsupported_record",
            "Claude import skipped an unsupported native record",
            details,
        ));
    }

    fn push_diagnostic(&mut self, diagnostic: ExternalSessionImportDiagnostic) {
        if self.diagnostics.len() < MAX_DIAGNOSTICS_PER_CANDIDATE {
            self.diagnostics.push(diagnostic);
        }
    }
}

fn continuation_from_ids(
    native_session_ids: &BTreeSet<String>,
) -> (
    Option<String>,
    ExternalSessionContinuationStatus,
    Option<String>,
) {
    if native_session_ids.len() == 1 {
        (
            native_session_ids.first().cloned(),
            ExternalSessionContinuationStatus::Resumable,
            None,
        )
    } else if native_session_ids.is_empty() {
        (
            None,
            ExternalSessionContinuationStatus::ReadOnly,
            Some(MISSING_NATIVE_SESSION_ID.to_string()),
        )
    } else {
        (
            None,
            ExternalSessionContinuationStatus::ReadOnly,
            Some(AMBIGUOUS_NATIVE_SESSION_ID.to_string()),
        )
    }
}

fn import_item(
    source: TimelineSource,
    payload: TimelinePayload,
    provider_correlation_id: Option<String>,
) -> ExternalSessionImportTimelineItem {
    ExternalSessionImportTimelineItem {
        source,
        payload,
        provider_correlation_id,
        redaction_state: TimelineRedactionState::None,
        timestamp_ms: None,
    }
}

fn extract_message_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let mut chunks = Vec::new();
            for part in parts {
                if let Some(text) = text_field(part, "text").filter(|value| has_text(value)) {
                    chunks.push(text.to_string());
                }
            }
            if chunks.is_empty() {
                None
            } else {
                Some(chunks.join("\n"))
            }
        }
        _ => None,
    }
}

fn block_correlation_id(base: Option<String>, block_index: usize) -> Option<String> {
    base.map(|value| format!("{value}:block-{block_index}"))
}

fn text_field<'a>(value: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    value.get(key).and_then(serde_json::Value::as_str)
}

fn file_modified_at_ms(path: &Path) -> Option<i64> {
    path.metadata()
        .ok()
        .and_then(|metadata| metadata.modified().ok())
        .and_then(|modified| modified.duration_since(UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .ok()
                .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        })
}

fn candidate_id(path: &Path, native_session_id: Option<&str>) -> String {
    match native_session_id {
        Some(session_id) => format!("claude:session:{}", stable_hash_hex(session_id)),
        None => format!(
            "claude:path:{}",
            stable_hash_hex(path.display().to_string())
        ),
    }
}

fn title_for(path: &Path, native_session_id: Option<&str>) -> String {
    if let Some(session_id) = native_session_id {
        return format!("Imported Claude session {}", redacted_prefix(session_id));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("jsonl");
    format!("Imported Claude session {}", bounded(stem, 24))
}

fn redacted_prefix(value: &str) -> String {
    let prefix: String = value.chars().take(8).collect();
    if prefix.is_empty() {
        "unknown".to_string()
    } else {
        prefix
    }
}

fn diagnostic(
    code: impl Into<String>,
    message: impl Into<String>,
    redacted_details: Vec<ProviderBindingMetadata>,
) -> ExternalSessionImportDiagnostic {
    ExternalSessionImportDiagnostic {
        code: code.into(),
        message: message.into(),
        source: ExternalSessionImportSource::Claude,
        redacted_details,
    }
}

fn detail(key: impl Into<String>, value: impl Into<String>) -> ProviderBindingMetadata {
    ProviderBindingMetadata {
        key: key.into(),
        value: value.into(),
    }
}

fn bounded(value: impl AsRef<str>, max_chars: usize) -> String {
    let value = value.as_ref();
    let mut output: String = value.chars().take(max_chars).collect();
    if output.len() < value.len() {
        output.push_str("...");
    }
    output
}

fn stable_hash_hex(value: impl AsRef<str>) -> String {
    let mut hasher = StableHasher::default();
    value.as_ref().hash(&mut hasher);
    format!("{:016x}", hasher.finish())
}

#[derive(Default)]
struct StableHasher(u64);

impl Hasher for StableHasher {
    fn finish(&self) -> u64 {
        self.0
    }

    fn write(&mut self, bytes: &[u8]) {
        if self.0 == 0 {
            self.0 = 0xcbf29ce484222325;
        }
        for byte in bytes {
            self.0 ^= u64::from(*byte);
            self.0 = self.0.wrapping_mul(0x100000001b3);
        }
    }
}

fn has_text(value: &str) -> bool {
    !value.trim().is_empty()
}

#[cfg(test)]
mod tests {
    use super::*;
    use vibex_core::{FetchTimelineRequest, SendAgentMessageRequest, TimelineItemKind};

    #[test]
    fn preview_resumable_fixture_maps_timeline_in_order() {
        let preview =
            preview_claude_external_sessions(request(vec![fixture("claude_resumable.jsonl")]))
                .unwrap();

        assert_eq!(preview.candidates.len(), 1);
        let candidate = &preview.candidates[0];
        assert_eq!(
            candidate.continuation_status,
            ExternalSessionContinuationStatus::Resumable
        );
        assert_eq!(
            candidate.native_session_id.as_deref(),
            Some("claude-session-fixture-1")
        );
        assert_eq!(candidate.timeline_items.len(), 5);
        assert!(matches!(
            candidate.timeline_items[0].payload,
            TimelinePayload::UserMessage(_)
        ));
        assert!(matches!(
            candidate.timeline_items[1].payload,
            TimelinePayload::AgentMessage(_)
        ));
        assert!(matches!(
            candidate.timeline_items[2].payload,
            TimelinePayload::Reasoning(_)
        ));
        assert!(matches!(
            candidate.timeline_items[3].payload,
            TimelinePayload::ToolCall(_)
        ));
        assert!(matches!(
            candidate.timeline_items[4].payload,
            TimelinePayload::ToolCall(_)
        ));
        assert!(candidate.diagnostics.is_empty());
    }

    #[test]
    fn preview_missing_native_session_id_is_read_only_with_bounded_diagnostics() {
        let preview = preview_claude_external_sessions(request(vec![fixture(
            "claude_read_only_malformed.jsonl",
        )]))
        .unwrap();

        assert_eq!(preview.candidates.len(), 1);
        let candidate = &preview.candidates[0];
        assert_eq!(
            candidate.continuation_status,
            ExternalSessionContinuationStatus::ReadOnly
        );
        assert_eq!(
            candidate.continuation_reason.as_deref(),
            Some(MISSING_NATIVE_SESSION_ID)
        );
        assert_eq!(candidate.native_session_id, None);
        assert_eq!(
            candidate.status,
            ExternalSessionImportCandidateStatus::Importable
        );
        assert!(
            candidate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "claude_import_malformed_jsonl")
        );
        assert!(
            candidate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "claude_import_unsupported_record")
        );
        for diagnostic in &candidate.diagnostics {
            let details = serde_json::to_string(&diagnostic.redacted_details).unwrap();
            assert!(!details.contains("unsupported secret text"));
            assert!(!details.contains("Synthetic Claude user import prompt"));
            assert!(!details.contains("TOKEN=secret"));
        }
    }

    #[tokio::test]
    async fn selected_claude_import_delegates_to_agent_manager_storage() {
        let db_path = temp_db_path("claude-selected-import");
        let manager = AgentManager::new(&db_path).unwrap();
        let preview =
            preview_claude_external_sessions(request(vec![fixture("claude_resumable.jsonl")]))
                .unwrap();

        let result = import_selected_claude_sessions(&manager, preview.candidates.clone(), None)
            .await
            .unwrap();

        assert_eq!(result.sessions.len(), 1);
        let session = &result.sessions[0];
        assert_eq!(
            session.agent_id,
            vibex_core::AgentId::parse("claude").unwrap()
        );
        let page = manager
            .fetch_timeline(FetchTimelineRequest {
                session_id: session.id.clone(),
                after_sequence: Some(0),
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 6);
        assert_eq!(page.items[0].kind, TimelineItemKind::SystemNotice);
        assert_eq!(page.items[1].kind, TimelineItemKind::UserMessage);
        assert_eq!(page.items[2].kind, TimelineItemKind::AgentMessage);
        assert_eq!(page.items[3].kind, TimelineItemKind::Reasoning);
        assert_eq!(page.items[4].kind, TimelineItemKind::ToolCall);
        assert_eq!(page.items[5].kind, TimelineItemKind::ToolCall);

        cleanup_db(db_path);
    }

    #[tokio::test]
    async fn read_only_selected_claude_import_fails_closed_without_runtime_state() {
        let db_path = temp_db_path("claude-read-only-import");
        let manager = AgentManager::new(&db_path).unwrap();
        let preview = preview_claude_external_sessions(request(vec![fixture(
            "claude_read_only_malformed.jsonl",
        )]))
        .unwrap();
        let result = import_selected_claude_sessions(&manager, preview.candidates, None)
            .await
            .unwrap();
        let session = &result.sessions[0];

        let err = manager
            .send_message(SendAgentMessageRequest {
                session_id: session.id.clone(),
                message_idempotency_key: "claude-read-only-import".to_string(),
                desired_runtime: vibex_core::SessionRuntimeSelection {
                    agent_id: vibex_core::AgentId::parse("claude").unwrap(),
                    provider_profile_id: vibex_core::ProviderProfileId::parse(
                        ProviderKind::Claude.local_default_profile_id().to_string(),
                    )
                    .unwrap(),
                    model_id: "legacy-import".to_string(),
                    reasoning_effort: None,
                    mode_id: None,
                    config_values: Default::default(),
                },
                text: "continue".to_string(),
                attachments: Vec::new(),
                reasoning_effort: None,
                correlation_id: None,
            })
            .await
            .unwrap_err();
        assert_eq!(err.code, "message_submission_coordinator_unavailable");

        cleanup_db(db_path);
    }

    #[test]
    fn discover_jsonl_paths_recurses_project_directories_with_limit() {
        let root = unique_temp_dir("vibex-claude-import-recursive");
        let nested = root.join(".claude").join("projects").join("vibex");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("root.jsonl"), "{}\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "{}\n").unwrap();
        std::fs::write(nested.join("nested-a.jsonl"), "{}\n").unwrap();
        std::fs::write(nested.join("nested-b.jsonl"), "{}\n").unwrap();

        let paths = discover_jsonl_paths(std::slice::from_ref(&root), Some(2)).unwrap();

        assert_eq!(
            paths,
            vec![
                root.join(".claude")
                    .join("projects")
                    .join("vibex")
                    .join("nested-a.jsonl"),
                root.join(".claude")
                    .join("projects")
                    .join("vibex")
                    .join("nested-b.jsonl")
            ]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    fn request(paths: Vec<PathBuf>) -> ClaudeSessionImportPreviewRequest {
        ClaudeSessionImportPreviewRequest {
            paths,
            workspace_root: Some("/tmp/vibex-claude-import-fixture".to_string()),
            workspace_mode: WorkspaceMode::CurrentCheckout,
            provider_profile_id: None,
            correlation_id: None,
            limit: None,
        }
    }

    fn fixture(name: &str) -> PathBuf {
        PathBuf::from(env!("CARGO_MANIFEST_DIR"))
            .join("tests")
            .join("fixtures")
            .join(name)
    }

    fn temp_db_path(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!(
            "vibex-agent-claude-{label}-{}.db",
            vibex_core::RequestId::new().as_str()
        ))
    }

    fn cleanup_db(path: PathBuf) {
        let _ = std::fs::remove_file(&path);
        let _ = std::fs::remove_file(path.with_extension("db-wal"));
        let _ = std::fs::remove_file(path.with_extension("db-shm"));
    }

    fn unique_temp_dir(label: &str) -> PathBuf {
        std::env::temp_dir().join(format!("{label}-{}", vibex_core::RequestId::new().as_str()))
    }
}

use std::collections::{BTreeSet, HashMap};
use std::fs::File;
use std::hash::{Hash, Hasher};
use std::io::{BufRead, BufReader};
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::SystemTime;

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
const DEFAULT_SCAN_LIMIT: usize = 500;
const MAX_SCAN_LIMIT: usize = 2000;
const MAX_SCAN_WORKERS: usize = 8;
/// Summary scans stop reading a transcript once every scan field is known;
/// this line cap bounds the read for transcripts whose user prompt is buried
/// under long non-message prefixes.
const SCAN_PROBE_LINE_LIMIT: usize = 512;

/// Default Claude transcript store roots: `$CLAUDE_CONFIG_DIR/projects` when
/// set, plus the `~/.claude/projects` fallback. Existing directories only — a
/// missing store simply means "no local sessions".
pub fn claude_external_session_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(config_dir) = std::env::var_os("CLAUDE_CONFIG_DIR")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        roots.push(config_dir.join("projects"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home).join(".claude").join("projects"));
    }
    roots.retain(|root| root.is_dir());
    roots.dedup();
    roots
}
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

    // Summary scans carry no native history; materialize each selected
    // candidate from its transcript file so the disk — not a possibly stale
    // scan — is the source of truth for what gets imported.
    let mut materialized = Vec::with_capacity(candidates.len());
    let mut diagnostics = Vec::new();
    let mut first_failure: Option<(String, String)> = None;
    for candidate in candidates {
        if candidate.timeline_items.is_empty()
            && let Some(source_path) = candidate.source_path.as_deref()
        {
            match materialize_claude_candidate(&candidate, Path::new(source_path)) {
                Ok(candidate) => materialized.push(candidate),
                Err(error) => {
                    if first_failure.is_none() {
                        first_failure = Some((candidate.candidate_id.clone(), error.message.clone()));
                    }
                    diagnostics.push(
                        VibexError::validation(
                            "claude_import_materialize_failed",
                            "Claude import candidate could not be read from its transcript",
                        )
                        .with_diagnostic("candidateId", candidate.candidate_id.clone())
                        .with_diagnostic("error", bounded(&error.message, 160)),
                    );
                }
            }
            continue;
        }
        materialized.push(candidate);
    }
    if materialized.is_empty()
        && let Some((candidate_id, error)) = first_failure
    {
        return Err(VibexError::validation(
            "claude_import_materialize_failed",
            "no selected Claude candidates could be read from their transcripts",
        )
        .with_diagnostic("candidateId", candidate_id)
        .with_diagnostic("error", bounded(&error, 160)));
    }

    manager
        .import_external_sessions(ExternalSessionImportRequest {
            candidates: materialized,
            correlation_id,
        })
        .await
}

/// Re-read a scanned candidate's transcript and return a fully materialized
/// candidate (native ids, workspace, title, and full native history). The
/// candidate id is deterministic given the file, so callers can keep matching
/// the result against the selected scan summary.
fn materialize_claude_candidate(
    candidate: &ExternalSessionImportCandidate,
    source_path: &Path,
) -> VibexResult<ExternalSessionImportCandidate> {
    let request = ClaudeSessionImportPreviewRequest {
        paths: vec![source_path.to_path_buf()],
        workspace_root: Some(candidate.workspace_root.clone()),
        workspace_mode: candidate.workspace_mode,
        provider_profile_id: candidate.provider_profile_id.clone(),
        correlation_id: None,
        limit: Some(1),
    };
    let preview = preview_claude_external_sessions(request)?;
    let mut candidates = preview.candidates;
    candidates
        .iter()
        .position(|parsed| parsed.candidate_id == candidate.candidate_id)
        .map(|index| candidates.remove(index))
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| {
            VibexError::validation(
                "claude_import_materialize_empty",
                "Claude transcript no longer contains an importable session",
            )
            .with_diagnostic("candidateId", candidate.candidate_id.clone())
        })
}

/// Fast multi-file scan for the import picker. Unlike the preview request it
/// is recursive, does not stop at 200 files, and — critically — never parses a
/// transcript to EOF: each file contributes a summary candidate whose native
/// history is re-read from disk only if that candidate is selected for import.
/// Results are capped at `limit`, newest transcript first.
pub fn scan_claude_external_sessions(
    roots: &[PathBuf],
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
    limit: Option<usize>,
) -> ExternalSessionImportPreview {
    let limit = limit.unwrap_or(DEFAULT_SCAN_LIMIT).clamp(1, MAX_SCAN_LIMIT);
    let mut paths = Vec::new();
    for root in roots {
        collect_jsonl_paths_recursive_lenient(root, &mut paths);
    }
    if paths.len() > 1 {
        paths.sort_by_cached_key(|path| std::cmp::Reverse(file_modified_at_ms(path).unwrap_or(0)));
    }
    paths.truncate(limit);

    let summaries = parse_claude_summaries_parallel(&paths, workspace_mode, provider_profile_id);
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    for summary in summaries {
        match summary {
            Ok(Some(candidate)) => candidates.push(candidate),
            Ok(None) => {}
            Err(diagnostic) => diagnostics.push(diagnostic),
        }
    }
    ExternalSessionImportPreview {
        candidates,
        diagnostics,
        correlation_id: None,
    }
}

/// Directory walk that never fails the scan: unreadable directories and
/// vanished entries are skipped, matching the summary-cache parser semantics.
fn collect_jsonl_paths_recursive_lenient(directory: &Path, discovered: &mut Vec<PathBuf>) {
    let Ok(entries) = std::fs::read_dir(directory) else {
        return;
    };
    let mut directories = Vec::new();
    for entry in entries.flatten() {
        let Ok(file_type) = entry.file_type() else {
            continue;
        };
        let path = entry.path();
        if file_type.is_dir() {
            directories.push(path);
        } else if (file_type.is_file() || (file_type.is_symlink() && path.is_file()))
            && path.extension().is_some_and(|extension| extension == "jsonl")
        {
            discovered.push(path);
        }
    }
    directories.sort();
    for directory in directories {
        collect_jsonl_paths_recursive_lenient(&directory, discovered);
    }
}

fn parse_claude_summaries_parallel(
    paths: &[PathBuf],
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Vec<Result<Option<ExternalSessionImportCandidate>, ExternalSessionImportDiagnostic>> {
    if paths.is_empty() {
        return Vec::new();
    }
    if paths.len() == 1 {
        return vec![parse_claude_summary_with_cache(
            &paths[0],
            workspace_mode,
            provider_profile_id,
        )];
    }
    let worker_count = MAX_SCAN_WORKERS.min(paths.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(
        Vec::<(usize, Result<Option<ExternalSessionImportCandidate>, ExternalSessionImportDiagnostic>)>::with_capacity(
            paths.len(),
        ),
    );
    std::thread::scope(|scope| {
        for _ in 0..worker_count {
            scope.spawn(|| {
                loop {
                    let index = next.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                    if index >= paths.len() {
                        break;
                    }
                    let parsed = parse_claude_summary_with_cache(
                        &paths[index],
                        workspace_mode,
                        provider_profile_id.clone(),
                    );
                    if let Ok(mut slot) = results.lock() {
                        slot.push((index, parsed));
                    }
                }
            });
        }
    });
    let mut slots: Vec<_> = results.into_inner().unwrap_or_default();
    slots.sort_by_key(|(index, _)| *index);
    slots.into_iter().map(|(_, parsed)| parsed).collect()
}

/// Memoized summary parse keyed on `(mtime, size)` — a cache hit `stat`s the
/// file (microseconds) instead of reading and JSON-parsing it. Rescans of a
/// history that only grows are therefore dominated by the directory walk.
fn parse_claude_summary_with_cache(
    path: &Path,
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Result<Option<ExternalSessionImportCandidate>, ExternalSessionImportDiagnostic> {
    let cache = claude_summary_cache();
    let fingerprint = summary_fingerprint(path);

    if let (Some(fingerprint), Ok(cache)) = (fingerprint, cache.lock())
        && let Some(candidate) = cache.entries.get(path)
        && candidate.fingerprint == fingerprint
    {
        let mut cached = candidate.candidate.clone();
        cached.provider_profile_id = provider_profile_id;
        return Ok(Some(cached));
    }

    let parsed = parse_claude_summary_candidate(path, workspace_mode, provider_profile_id);
    // Only memoize positive summaries whose fingerprint survived the read:
    // a transcript written while we parsed it stays uncached so the next
    // scan re-reads the settled bytes.
    if let (Some(fingerprint), Ok(Some(parsed)), Ok(mut cache)) =
        (fingerprint, parsed.as_ref(), cache.lock())
        && summary_fingerprint(path) == Some(fingerprint)
        && parsed.status == ExternalSessionImportCandidateStatus::Importable
    {
        cache.entries.insert(
            path.to_path_buf(),
            CachedSummary {
                fingerprint,
                candidate: parsed.clone(),
            },
        );
    }
    parsed
}

/// Process-global summary cache. Keyed by transcript path and invalidated by
/// the `(mtime, size)` fingerprint, so a rescan of a history that only grows
/// `stat`s each file instead of re-reading and re-parsing it.
fn claude_summary_cache() -> &'static Mutex<SummaryCache> {
    static CACHE: OnceLock<Mutex<SummaryCache>> = OnceLock::new();
    CACHE.get_or_init(|| {
        Mutex::new(SummaryCache {
            entries: HashMap::new(),
        })
    })
}

fn summary_fingerprint(path: &Path) -> Option<(Option<SystemTime>, u64)> {
    let metadata = std::fs::metadata(path).ok()?;
    Some((metadata.modified().ok(), metadata.len()))
}

struct SummaryCache {
    entries: HashMap<PathBuf, CachedSummary>,
}

struct CachedSummary {
    fingerprint: (Option<SystemTime>, u64),
    candidate: ExternalSessionImportCandidate,
}

/// Summary-only parse: enough of the transcript to title the session and
/// resolve its native session id and workspace — typically just the first
/// user turn — with `timeline_items` left empty so the import path re-reads
/// the file. `Ok(None)` marks transcripts that are not main-conversation
/// history (sidechain subagent files), which the picker should not offer.
fn parse_claude_summary_candidate(
    path: &Path,
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Result<Option<ExternalSessionImportCandidate>, ExternalSessionImportDiagnostic> {
    let Ok(file) = File::open(path) else {
        return Err(diagnostic(
            "claude_import_file_unreadable",
            "Claude session transcript could not be read",
            vec![detail("pathHash", stable_hash_hex(path.display().to_string()))],
        ));
    };
    let mut state = ScanState {
        native_session_ids: BTreeSet::new(),
        workspace_root: None,
        first_user_text: None,
        summary_text: None,
        has_content: false,
        first_sidechain_flag: None,
        updated_at_ms: file_modified_at_ms(path),
    };
    for (line_index, line) in BufReader::new(file).lines().enumerate() {
        if state.scan_is_complete() || line_index >= SCAN_PROBE_LINE_LIMIT {
            break;
        }
        let Ok(line) = line else {
            break;
        };
        let Ok(value) = serde_json::from_str::<serde_json::Value>(&line) else {
            continue;
        };
        state.ingest_summary_record(&value);
    }
    // Subagent transcripts carry the parent conversation's sidechain flag;
    // they are internal implementation detail, not user-facing sessions.
    if state.first_sidechain_flag == Some(true) {
        return Ok(None);
    }
    let (native_session_id, continuation_status, continuation_reason) =
        continuation_from_ids(&state.native_session_ids);
    let workspace_root = state
        .workspace_root
        .filter(|value| has_text(value))
        .unwrap_or_else(|| "/tmp/vibex-claude-import".to_string());
    let status = if state.has_content
        || continuation_status == ExternalSessionContinuationStatus::Resumable
    {
        ExternalSessionImportCandidateStatus::Importable
    } else {
        ExternalSessionImportCandidateStatus::Blocked
    };
    Ok(Some(ExternalSessionImportCandidate {
        candidate_id: candidate_id(path, native_session_id.as_deref()),
        source: ExternalSessionImportSource::Claude,
        agent_id: vibex_core::AgentId::parse("claude").expect("builtin Claude Agent id"),
        provider_kind: ProviderKind::Claude,
        provider_profile_id,
        workspace_root,
        additional_workspace_roots: Vec::new(),
        workspace_mode,
        title: title_for(
            path,
            native_session_id.as_deref(),
            state
                .first_user_text
                .as_deref()
                .or(state.summary_text.as_deref()),
        ),
        native_session_id,
        native_thread_id: None,
        native_resume_token: None,
        continuation_status,
        continuation_reason,
        updated_at_ms: state.updated_at_ms,
        session_config_state: None,
        status,
        already_imported: false,
        redaction_state: TimelineRedactionState::None,
        timeline_items: Vec::new(),
        diagnostics: Vec::new(),
        source_path: Some(path.display().to_string()),
    }))
}

struct ScanState {
    native_session_ids: BTreeSet<String>,
    workspace_root: Option<String>,
    first_user_text: Option<String>,
    summary_text: Option<String>,
    has_content: bool,
    first_sidechain_flag: Option<bool>,
    updated_at_ms: Option<i64>,
}

impl ScanState {
    fn ingest_summary_record(&mut self, value: &serde_json::Value) {
        if let Some(session_id) = text_field(value, "sessionId").filter(|value| has_text(value)) {
            self.native_session_ids.insert(session_id.to_string());
        }
        if self.workspace_root.is_none() {
            self.workspace_root = text_field(value, "cwd")
                .filter(|value| has_text(value))
                .map(ToOwned::to_owned);
        }
        if self.first_sidechain_flag.is_none()
            && let Some(flag) = value.get("isSidechain").and_then(serde_json::Value::as_bool)
        {
            self.first_sidechain_flag = Some(flag);
        }
        if text_field(value, "type") == Some("summary")
            && self.summary_text.is_none()
            && let Some(summary) = text_field(value, "summary").filter(|value| has_text(value))
        {
            self.summary_text = Some(bounded(summary, 80));
            return;
        }
        if self.has_content {
            return;
        }
        let record_type = text_field(value, "type").unwrap_or("missing");
        let Some(message) = value.get("message") else {
            return;
        };
        let role = text_field(message, "role").or(match record_type {
            "user" => Some("user"),
            "assistant" => Some("assistant"),
            _ => None,
        });
        match role {
            Some("user") => {
                self.has_content = true;
                if self.first_user_text.is_none()
                    && value.get("isMeta").and_then(serde_json::Value::as_bool) != Some(true)
                    && let Some(text) = extract_message_text(message).filter(|value| has_text(value))
                {
                    self.first_user_text = title_from_user_text(&text);
                }
            }
            Some("assistant") => {
                self.has_content = true;
            }
            _ => {}
        }
    }

    fn scan_is_complete(&self) -> bool {
        self.native_session_ids.len() == 1
            && self.workspace_root.is_some()
            && self.first_user_text.is_some()
    }
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
    let title = title_for(path, native_session_id.as_deref(), None);
    let status = if state.timeline_items.is_empty() {
        ExternalSessionImportCandidateStatus::Blocked
    } else {
        ExternalSessionImportCandidateStatus::Importable
    };

    Ok(ExternalSessionImportCandidate {
        candidate_id,
        source: ExternalSessionImportSource::Claude,
        agent_id: vibex_core::AgentId::parse("claude").expect("builtin Claude Agent id"),
        provider_kind: ProviderKind::Claude,
        provider_profile_id: request.provider_profile_id.clone(),
        workspace_root,
        additional_workspace_roots: Vec::new(),
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
        already_imported: false,
        redaction_state: TimelineRedactionState::None,
        timeline_items: state.timeline_items,
        diagnostics: state.diagnostics,
        source_path: Some(path.display().to_string()),
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
        .and_then(|modified| modified.duration_since(SystemTime::UNIX_EPOCH).ok())
        .and_then(|duration| i64::try_from(duration.as_millis()).ok())
        .or_else(|| {
            SystemTime::now()
                .duration_since(SystemTime::UNIX_EPOCH)
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

fn title_for(path: &Path, native_session_id: Option<&str>, first_user_text: Option<&str>) -> String {
    if let Some(text) = first_user_text {
        return format!("Imported Claude: {}", text);
    }
    if let Some(session_id) = native_session_id {
        return format!("Imported Claude session {}", redacted_prefix(session_id));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("jsonl");
    format!("Imported Claude session {}", bounded(stem, 24))
}

/// Derive a readable row title from the first user prompt: flatten to the
/// first non-empty line and drop wrapper tags like `<command-message>` or
/// `<system-reminder>` blocks so scanned rows read like chat titles.
fn title_from_user_text(text: &str) -> Option<String> {
    let mut cleaned = text.trim();
    while cleaned.starts_with('<') {
        let Some(tag_end) = cleaned.find('>') else {
            break;
        };
        let tag = &cleaned[1..tag_end];
        let tag_name = tag
            .split_whitespace()
            .next()
            .unwrap_or_default()
            .trim_end_matches('/');
        if tag_name.is_empty() {
            break;
        }
        let rest = &cleaned[tag_end + 1..];
        let close = format!("</{}>", tag_name);
        cleaned = match rest.rfind(&close) {
            Some(position) => rest[..position].trim(),
            None => rest.trim(),
        };
    }
    cleaned
        .lines()
        .map(str::trim)
        .find(|line| has_text(line))
        .map(|line| bounded(line, 80))
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
                desired_runtime: vibex_core::SessionRuntimeSelection::provider(
                    vibex_core::AgentId::parse("claude").unwrap(),
                    vibex_core::ProviderProfileId::parse(
                        ProviderKind::Claude.local_default_profile_id().to_string(),
                    )
                    .unwrap(),
                    "legacy-import",
                ),
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

    fn write_claude_session(path: &Path, session_id: &str, cwd: &str, user_text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let user = serde_json::json!({
            "type": "user",
            "sessionId": session_id,
            "cwd": cwd,
            "message": {"role": "user", "content": [{"type": "text", "text": user_text}]}
        });
        let assistant = serde_json::json!({
            "type": "assistant",
            "sessionId": session_id,
            "cwd": cwd,
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Reply text"}]}
        });
        std::fs::write(
            path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&user).unwrap(),
                serde_json::to_string(&assistant).unwrap()
            ),
        )
        .unwrap();
    }

    fn set_mtime(path: &Path, seconds: u64) {
        let file = File::options().write(true).open(path).unwrap();
        file.set_modified(
            SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(seconds),
        )
        .unwrap();
    }

    #[test]
    fn scan_orders_newest_first_and_skips_sidechain_transcripts() {
        let root = unique_temp_dir("vibex-claude-scan-order");
        write_claude_session(
            &root.join("-tmp-project-b/sessions-b.jsonl"),
            "session-b",
            "/tmp/project-b",
            "Second session prompt",
        );
        write_claude_session(
            &root.join("-tmp-project-a/sessions-a.jsonl"),
            "session-a",
            "/tmp/project-a",
            "First session prompt",
        );
        set_mtime(&root.join("-tmp-project-a/sessions-a.jsonl"), 1_000);
        set_mtime(&root.join("-tmp-project-b/sessions-b.jsonl"), 2_000);

        let sidechain_path = root.join("-tmp-project-b/agent-sidechain.jsonl");
        std::fs::write(
            &sidechain_path,
            serde_json::to_string(&serde_json::json!({
                "type": "user",
                "isSidechain": true,
                "sessionId": "session-sidechain",
                "cwd": "/tmp/project-b",
                "message": {"role": "user", "content": "sidechain work"}
            }))
            .unwrap(),
        )
        .unwrap();
        set_mtime(&sidechain_path, 3_000);

        let preview = scan_claude_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, None);

        assert_eq!(preview.candidates.len(), 2);
        assert_eq!(preview.candidates[0].native_session_id.as_deref(), Some("session-b"));
        assert_eq!(preview.candidates[1].native_session_id.as_deref(), Some("session-a"));
        for candidate in &preview.candidates {
            assert_eq!(candidate.timeline_items.len(), 0, "summary scan stays light");
            assert!(candidate.source_path.is_some());
            assert_eq!(candidate.status, ExternalSessionImportCandidateStatus::Importable);
            assert_eq!(candidate.source, ExternalSessionImportSource::Claude);
        }
        assert_eq!(preview.candidates[0].title, "Imported Claude: Second session prompt");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_summary_cache_skips_reparse_unchanged_files() {
        let root = unique_temp_dir("vibex-claude-scan-cache");
        let path = root.join("cached-session.jsonl");
        write_claude_session(&path, "session-cache", "/tmp/project-cache", "Cached prompt");

        let first = scan_claude_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, None);
        assert_eq!(first.candidates.len(), 1);
        let fingerprint = summary_fingerprint(&path);

        // Same fingerprint: the memoized summary must round-trip unchanged.
        let second = parse_claude_summary_with_cache(&path, WorkspaceMode::CurrentCheckout, None).unwrap();
        assert_eq!(second.as_ref().map(|c| c.candidate_id.clone()), Some(first.candidates[0].candidate_id.clone()));
        assert_eq!(summary_fingerprint(&path), fingerprint);

        // Mutating content invalidates the cache entry.
        write_claude_session(&path, "session-cache-2", "/tmp/project-cache", "Changed prompt");
        let third = parse_claude_summary_with_cache(&path, WorkspaceMode::CurrentCheckout, None).unwrap();
        assert_ne!(third.and_then(|c| c.native_session_id), Some("session-cache".to_string()));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_uses_summary_record_as_title_fallback() {
        let root = unique_temp_dir("vibex-claude-scan-summary");
        let path = root.join("summarized.jsonl");
        let summary = serde_json::json!({
            "type": "summary",
            "summary": "Refactor the importer pipeline",
            "leafUuid": "leaf-1"
        });
        let assistant = serde_json::json!({
            "type": "assistant",
            "sessionId": "session-summary",
            "cwd": "/tmp/project-summary",
            "message": {"role": "assistant", "content": [{"type": "text", "text": "Working"}]}
        });
        std::fs::create_dir_all(&root).unwrap();
        std::fs::write(
            &path,
            format!(
                "{}\n{}\n",
                serde_json::to_string(&summary).unwrap(),
                serde_json::to_string(&assistant).unwrap()
            ),
        )
        .unwrap();

        let preview = scan_claude_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, None);
        assert_eq!(preview.candidates.len(), 1);
        assert_eq!(preview.candidates[0].title, "Imported Claude: Refactor the importer pipeline");
        std::fs::remove_dir_all(root).ok();
    }
}

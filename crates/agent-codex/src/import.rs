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
    TimelineRedactionState, TimelineSource, UserMessagePayload, VibexError, VibexResult,
    WorkspaceMode,
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

/// Default Codex transcript store roots: `$CODEX_HOME/sessions` when set,
/// plus the `~/.codex/sessions` fallback. Existing directories only — a
/// missing store simply means "no local sessions".
pub fn codex_external_session_roots() -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(codex_home) = std::env::var_os("CODEX_HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
    {
        roots.push(codex_home.join("sessions"));
    }
    if let Some(home) = std::env::var_os("HOME").filter(|value| !value.is_empty()) {
        roots.push(PathBuf::from(home).join(".codex").join("sessions"));
    }
    roots.retain(|root| root.is_dir());
    roots.dedup();
    roots
}
const MAX_DIAGNOSTICS_PER_CANDIDATE: usize = 32;
const MISSING_NATIVE_THREAD_ID: &str = "missing_native_thread_id";
const AMBIGUOUS_NATIVE_THREAD_ID: &str = "ambiguous_native_thread_id";

#[derive(Debug, Clone)]
pub struct CodexSessionImportPreviewRequest {
    pub paths: Vec<PathBuf>,
    pub workspace_root: Option<String>,
    pub workspace_mode: WorkspaceMode,
    pub provider_profile_id: Option<ProviderProfileId>,
    pub correlation_id: Option<CorrelationId>,
    pub limit: Option<usize>,
}

pub fn preview_codex_external_sessions(
    request: CodexSessionImportPreviewRequest,
) -> VibexResult<ExternalSessionImportPreview> {
    let paths = discover_jsonl_paths(&request.paths, request.limit)?;
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();

    for path in paths {
        match parse_codex_jsonl_candidate(&path, &request) {
            Ok(candidate) => {
                diagnostics.extend(candidate.diagnostics.clone());
                candidates.push(candidate);
            }
            Err(err) => diagnostics.push(diagnostic(
                "codex_import_file_unreadable",
                "Codex import candidate could not be read",
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

pub async fn import_selected_codex_sessions(
    manager: &AgentManager,
    candidates: Vec<ExternalSessionImportCandidate>,
    correlation_id: Option<CorrelationId>,
) -> VibexResult<ExternalSessionImportResult> {
    for candidate in &candidates {
        if candidate.source != ExternalSessionImportSource::Codex
            || candidate.provider_kind != ProviderKind::Codex
        {
            return Err(VibexError::validation(
                "codex_import_candidate_provider_mismatch",
                "selected import candidate is not a Codex candidate",
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
            match materialize_codex_candidate(&candidate, Path::new(source_path)) {
                Ok(candidate) => materialized.push(candidate),
                Err(error) => {
                    if first_failure.is_none() {
                        first_failure = Some((candidate.candidate_id.clone(), error.message.clone()));
                    }
                    diagnostics.push(
                        VibexError::validation(
                            "codex_import_materialize_failed",
                            "Codex import candidate could not be read from its transcript",
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
            "codex_import_materialize_failed",
            "no selected Codex candidates could be read from their transcripts",
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
fn materialize_codex_candidate(
    candidate: &ExternalSessionImportCandidate,
    source_path: &Path,
) -> VibexResult<ExternalSessionImportCandidate> {
    let request = CodexSessionImportPreviewRequest {
        paths: vec![source_path.to_path_buf()],
        workspace_root: Some(candidate.workspace_root.clone()),
        workspace_mode: candidate.workspace_mode,
        provider_profile_id: candidate.provider_profile_id.clone(),
        correlation_id: None,
        limit: Some(1),
    };
    let preview = preview_codex_external_sessions(request)?;
    let mut candidates = preview.candidates;
    candidates
        .iter()
        .position(|parsed| parsed.candidate_id == candidate.candidate_id)
        .map(|index| candidates.remove(index))
        .or_else(|| candidates.into_iter().next())
        .ok_or_else(|| {
            VibexError::validation(
                "codex_import_materialize_empty",
                "Codex transcript no longer contains an importable session",
            )
            .with_diagnostic("candidateId", candidate.candidate_id.clone())
        })
}

fn discover_jsonl_paths(paths: &[PathBuf], limit: Option<usize>) -> VibexResult<Vec<PathBuf>> {
    if paths.is_empty() {
        return Err(VibexError::validation(
            "codex_import_paths_empty",
            "Codex import preview requires at least one explicit fixture or read-only path",
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
                "codex_import_path_missing",
                "Codex import path must exist and be readable",
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

/// Fast multi-file scan for the import picker. Unlike the preview request it
/// is recursive, does not stop at 200 files, and — critically — never parses a
/// transcript to EOF: each file contributes a summary candidate whose native
/// history is re-read from disk only if that candidate is selected for import.
/// Results are capped at `limit`, newest transcript first.
pub fn scan_codex_external_sessions(
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

    let summaries = parse_codex_summaries_parallel(&paths, workspace_mode, provider_profile_id);
    let mut candidates = Vec::new();
    let mut diagnostics = Vec::new();
    for summary in summaries {
        match summary {
            Ok(candidate) => candidates.push(candidate),
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

fn parse_codex_summaries_parallel(
    paths: &[PathBuf],
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Vec<Result<ExternalSessionImportCandidate, ExternalSessionImportDiagnostic>> {
    if paths.is_empty() {
        return Vec::new();
    }
    if paths.len() == 1 {
        return vec![parse_codex_summary_with_cache(
            &paths[0],
            workspace_mode,
            provider_profile_id,
        )];
    }
    let worker_count = MAX_SCAN_WORKERS.min(paths.len());
    let next = std::sync::atomic::AtomicUsize::new(0);
    let results = std::sync::Mutex::new(
        Vec::<(usize, Result<ExternalSessionImportCandidate, ExternalSessionImportDiagnostic>)>::with_capacity(
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
                    let parsed = parse_codex_summary_with_cache(
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
fn parse_codex_summary_with_cache(
    path: &Path,
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Result<ExternalSessionImportCandidate, ExternalSessionImportDiagnostic> {
    let cache = codex_summary_cache();
    let fingerprint = summary_fingerprint(path);

    if let (Some(fingerprint), Ok(cache)) = (fingerprint, cache.lock())
        && let Some(candidate) = cache.entries.get(path)
        && candidate.fingerprint == fingerprint
    {
        let mut cached = candidate.candidate.clone();
        cached.provider_profile_id = provider_profile_id;
        return Ok(cached);
    }

    let parsed = parse_codex_summary_candidate(path, workspace_mode, provider_profile_id);
    // Only memoize positive summaries whose fingerprint survived the read:
    // a transcript written while we parsed it stays uncached so the next
    // scan re-reads the settled bytes.
    if let (Some(fingerprint), Ok(parsed), Ok(mut cache)) =
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
fn codex_summary_cache() -> &'static Mutex<SummaryCache> {
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
/// resolve its native thread id and workspace — typically just the leading
/// `session_meta` and the first user turn — with `timeline_items` left empty
/// so the import path re-reads the file. `Err` results are scan-level
/// diagnostics so one unreadable transcript cannot fail the whole scan.
fn parse_codex_summary_candidate(
    path: &Path,
    workspace_mode: WorkspaceMode,
    provider_profile_id: Option<ProviderProfileId>,
) -> Result<ExternalSessionImportCandidate, ExternalSessionImportDiagnostic> {
    let Ok(file) = File::open(path) else {
        return Err(diagnostic(
            "codex_import_file_unreadable",
            "Codex session transcript could not be read",
            vec![detail("pathHash", stable_hash_hex(path.display().to_string()))],
        ));
    };
    let mut state = ScanState {
        native_thread_ids: BTreeSet::new(),
        workspace_root: None,
        first_user_text: None,
        saw_native_record: false,
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
        let record_type = value.get("type").and_then(serde_json::Value::as_str);
        let payload = value.get("payload").unwrap_or(&value);
        if record_type == Some("session_meta") {
            state.ingest_session_meta(payload);
            continue;
        }
        if record_type == Some("response_item") {
            state.saw_native_record = true;
            if state.first_user_text.is_none()
                && text_field(payload, "type") == Some("message")
                && text_field(payload, "role") == Some("user")
            {
                state.first_user_text = extract_text(payload)
                    .filter(|value| has_text(value))
                    .and_then(|value| title_from_user_text(&value));
            }
        }
    }
    let (native_thread_id, continuation_status, continuation_reason) =
        continuation_from_ids(&state.native_thread_ids);
    let workspace_root = state
        .workspace_root
        .filter(|value| has_text(value))
        .unwrap_or_else(|| "/tmp/vibex-codex-import".to_string());
    let status = if state.saw_native_record || continuation_status
        == ExternalSessionContinuationStatus::Resumable
    {
        ExternalSessionImportCandidateStatus::Importable
    } else {
        ExternalSessionImportCandidateStatus::Blocked
    };
    Ok(ExternalSessionImportCandidate {
        candidate_id: candidate_id(path, native_thread_id.as_deref()),
        source: ExternalSessionImportSource::Codex,
        agent_id: vibex_core::AgentId::parse("codex").expect("builtin Codex Agent id"),
        provider_kind: ProviderKind::Codex,
        provider_profile_id,
        workspace_root,
        additional_workspace_roots: Vec::new(),
        workspace_mode,
        title: title_for(path, native_thread_id.as_deref(), state.first_user_text.as_deref()),
        native_session_id: None,
        native_thread_id,
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
    })
}

struct ScanState {
    native_thread_ids: BTreeSet<String>,
    workspace_root: Option<String>,
    first_user_text: Option<String>,
    saw_native_record: bool,
    updated_at_ms: Option<i64>,
}

impl ScanState {
    fn ingest_session_meta(&mut self, payload: &serde_json::Value) {
        if let Some(id) = text_field(payload, "id").filter(|value| has_text(value)) {
            self.native_thread_ids.insert(id.to_string());
        }
        if self.workspace_root.is_none() {
            self.workspace_root = text_field(payload, "cwd")
                .filter(|value| has_text(value))
                .map(ToOwned::to_owned);
        }
    }

    fn scan_is_complete(&self) -> bool {
        self.native_thread_ids.len() == 1
            && self.workspace_root.is_some()
            && self.first_user_text.is_some()
    }
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
        VibexError::storage("codex_import_directory_unreadable", err.to_string())
            .with_diagnostic("pathHash", stable_hash_hex(directory.display().to_string()))
    })? {
        let entry = entry.map_err(|err| {
            VibexError::storage("codex_import_directory_entry_unreadable", err.to_string())
                .with_diagnostic("pathHash", stable_hash_hex(directory.display().to_string()))
        })?;
        let file_type = entry.file_type().map_err(|err| {
            VibexError::storage("codex_import_directory_entry_unreadable", err.to_string())
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

fn parse_codex_jsonl_candidate(
    path: &Path,
    request: &CodexSessionImportPreviewRequest,
) -> VibexResult<ExternalSessionImportCandidate> {
    let file = File::open(path).map_err(|err| {
        VibexError::storage("codex_import_file_open_failed", err.to_string())
            .with_diagnostic("pathHash", stable_hash_hex(path.display().to_string()))
    })?;
    let mut state = ParseState {
        path,
        workspace_root: request.workspace_root.clone(),
        native_thread_ids: BTreeSet::new(),
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
                    "codex_import_line_read_failed",
                    "Codex import skipped an unreadable JSONL line",
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
                    "codex_import_malformed_jsonl",
                    "Codex import skipped a malformed JSONL line",
                    vec![detail("line", line_number.to_string())],
                ));
                continue;
            }
        };
        state.ingest_record(line_number, &value);
    }

    let (native_thread_id, continuation_status, continuation_reason) =
        continuation_from_ids(&state.native_thread_ids);
    let workspace_root = state
        .workspace_root
        .filter(|value| has_text(value))
        .unwrap_or_else(|| "/tmp/vibex-codex-import".to_string());
    let candidate_id = candidate_id(path, native_thread_id.as_deref());
    let title = title_for(path, native_thread_id.as_deref(), None);
    let status = if state.timeline_items.is_empty() {
        ExternalSessionImportCandidateStatus::Blocked
    } else {
        ExternalSessionImportCandidateStatus::Importable
    };

    Ok(ExternalSessionImportCandidate {
        candidate_id,
        source: ExternalSessionImportSource::Codex,
        agent_id: vibex_core::AgentId::parse("codex").expect("builtin Codex Agent id"),
        provider_kind: ProviderKind::Codex,
        provider_profile_id: request.provider_profile_id.clone(),
        workspace_root,
        additional_workspace_roots: Vec::new(),
        workspace_mode: request.workspace_mode,
        title,
        native_session_id: None,
        native_thread_id,
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
    native_thread_ids: BTreeSet<String>,
    updated_at_ms: Option<i64>,
    timeline_items: Vec<ExternalSessionImportTimelineItem>,
    diagnostics: Vec<ExternalSessionImportDiagnostic>,
}

impl ParseState<'_> {
    fn ingest_record(&mut self, line_number: usize, value: &serde_json::Value) {
        let Some(record_type) = value.get("type").and_then(serde_json::Value::as_str) else {
            self.push_unsupported(line_number, "missing", None, None);
            return;
        };
        let payload = value.get("payload").unwrap_or(value);

        match record_type {
            "session_meta" => self.ingest_session_meta(line_number, payload),
            "response_item" => self.ingest_response_item(line_number, payload),
            _ => self.push_unsupported(line_number, record_type, None, None),
        }
    }

    fn ingest_session_meta(&mut self, line_number: usize, payload: &serde_json::Value) {
        if let Some(id) = text_field(payload, "id").filter(|value| has_text(value)) {
            self.native_thread_ids.insert(id.to_string());
        } else {
            self.push_diagnostic(diagnostic(
                "codex_import_session_meta_missing_id",
                "Codex session metadata did not include a stable native thread id",
                vec![detail("line", line_number.to_string())],
            ));
        }

        if self.workspace_root.is_none() {
            self.workspace_root = text_field(payload, "cwd")
                .filter(|value| has_text(value))
                .map(ToOwned::to_owned);
        }
    }

    fn ingest_response_item(&mut self, line_number: usize, payload: &serde_json::Value) {
        let item_type = text_field(payload, "type");
        let role = text_field(payload, "role");
        let provider_correlation_id = text_field(payload, "id")
            .map(ToOwned::to_owned)
            .or_else(|| Some(format!("line-{line_number}")));

        match (item_type, role) {
            (Some("message"), Some("user")) => {
                if let Some(text) = extract_text(payload).filter(|value| has_text(value)) {
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
            (Some("message"), Some("assistant" | "agent")) => {
                if let Some(text) = extract_text(payload).filter(|value| has_text(value)) {
                    self.timeline_items.push(import_item(
                        TimelineSource::Agent,
                        TimelinePayload::AgentMessage(AgentMessagePayload {
                            text,
                            is_final: true,
                        }),
                        provider_correlation_id,
                    ));
                } else {
                    self.push_empty_content(line_number, "assistant");
                }
            }
            (Some("reasoning"), _) | (Some("reasoning_summary"), _) => {
                if let Some(text) = extract_text(payload).filter(|value| has_text(value)) {
                    self.timeline_items.push(import_item(
                        TimelineSource::Agent,
                        TimelinePayload::Reasoning(ReasoningPayload {
                            text,
                            is_final: true,
                        }),
                        provider_correlation_id,
                    ));
                } else {
                    self.push_empty_content(line_number, "reasoning");
                }
            }
            (Some(item_type), role) => {
                self.push_unsupported(line_number, "response_item", Some(item_type), role);
            }
            (None, role) => {
                self.push_unsupported(line_number, "response_item", None, role);
            }
        }
    }

    fn push_empty_content(&mut self, line_number: usize, content_kind: &str) {
        self.push_diagnostic(diagnostic(
            "codex_import_empty_content",
            "Codex import skipped an empty supported record",
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
            "codex_import_unsupported_record",
            "Codex import skipped an unsupported native record",
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
    native_thread_ids: &BTreeSet<String>,
) -> (
    Option<String>,
    ExternalSessionContinuationStatus,
    Option<String>,
) {
    if native_thread_ids.len() == 1 {
        (
            native_thread_ids.first().cloned(),
            ExternalSessionContinuationStatus::Resumable,
            None,
        )
    } else if native_thread_ids.is_empty() {
        (
            None,
            ExternalSessionContinuationStatus::ReadOnly,
            Some(MISSING_NATIVE_THREAD_ID.to_string()),
        )
    } else {
        (
            None,
            ExternalSessionContinuationStatus::ReadOnly,
            Some(AMBIGUOUS_NATIVE_THREAD_ID.to_string()),
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

fn extract_text(payload: &serde_json::Value) -> Option<String> {
    if let Some(text) = text_field(payload, "text").filter(|value| has_text(value)) {
        return Some(text.to_string());
    }
    if let Some(summary) = text_field(payload, "summary").filter(|value| has_text(value)) {
        return Some(summary.to_string());
    }

    let content = payload.get("content")?;
    match content {
        serde_json::Value::String(text) => Some(text.clone()),
        serde_json::Value::Array(parts) => {
            let mut chunks = Vec::new();
            for part in parts {
                if let Some(text) = text_field(part, "text")
                    .or_else(|| text_field(part, "input_text"))
                    .or_else(|| text_field(part, "output_text"))
                    .or_else(|| text_field(part, "summary"))
                    .filter(|value| has_text(value))
                {
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

fn candidate_id(path: &Path, native_thread_id: Option<&str>) -> String {
    match native_thread_id {
        Some(thread_id) => format!("codex:thread:{}", stable_hash_hex(thread_id)),
        None => format!("codex:path:{}", stable_hash_hex(path.display().to_string())),
    }
}

fn title_for(path: &Path, native_thread_id: Option<&str>, first_user_text: Option<&str>) -> String {
    if let Some(text) = first_user_text {
        return format!("Imported Codex: {}", text);
    }
    if let Some(thread_id) = native_thread_id {
        return format!("Imported Codex session {}", redacted_prefix(thread_id));
    }
    let stem = path
        .file_stem()
        .and_then(|stem| stem.to_str())
        .unwrap_or("jsonl");
    format!("Imported Codex session {}", bounded(stem, 24))
}

/// Derive a readable row title from the first user prompt: flatten to the
/// first non-empty line and drop wrapper tags like `<environment_context>` or
/// `<user_instructions>` blocks so scanned rows read like chat titles.
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
        source: ExternalSessionImportSource::Codex,
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
    use vibex_core::{FetchTimelineRequest, TimelineItemKind};

    #[test]
    fn preview_resumable_fixture_maps_timeline_in_order() {
        let preview =
            preview_codex_external_sessions(request(vec![fixture("codex_resumable.jsonl")]))
                .unwrap();

        assert_eq!(preview.candidates.len(), 1);
        let candidate = &preview.candidates[0];
        assert_eq!(
            candidate.continuation_status,
            ExternalSessionContinuationStatus::Resumable
        );
        assert_eq!(
            candidate.native_thread_id.as_deref(),
            Some("codex-thread-fixture-1")
        );
        assert_eq!(candidate.timeline_items.len(), 3);
        assert!(matches!(
            candidate.timeline_items[0].payload,
            TimelinePayload::UserMessage(_)
        ));
        assert!(matches!(
            candidate.timeline_items[1].payload,
            TimelinePayload::Reasoning(_)
        ));
        assert!(matches!(
            candidate.timeline_items[2].payload,
            TimelinePayload::AgentMessage(_)
        ));
        assert!(candidate.diagnostics.is_empty());
    }

    #[test]
    fn preview_missing_native_thread_id_is_read_only_with_bounded_diagnostics() {
        let preview = preview_codex_external_sessions(request(vec![fixture(
            "codex_read_only_malformed.jsonl",
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
            Some(MISSING_NATIVE_THREAD_ID)
        );
        assert_eq!(candidate.native_thread_id, None);
        assert_eq!(
            candidate.status,
            ExternalSessionImportCandidateStatus::Importable
        );
        assert!(
            candidate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "codex_import_malformed_jsonl")
        );
        assert!(
            candidate
                .diagnostics
                .iter()
                .any(|diagnostic| diagnostic.code == "codex_import_unsupported_record")
        );
        for diagnostic in &candidate.diagnostics {
            let details = serde_json::to_string(&diagnostic.redacted_details).unwrap();
            assert!(!details.contains("unsupported secret text"));
            assert!(!details.contains("Synthetic user import prompt"));
        }
    }

    #[tokio::test]
    async fn selected_codex_import_delegates_to_agent_manager_storage() {
        let db_path = temp_db_path("codex-selected-import");
        let manager = AgentManager::new(&db_path).unwrap();
        let preview =
            preview_codex_external_sessions(request(vec![fixture("codex_resumable.jsonl")]))
                .unwrap();

        let result = import_selected_codex_sessions(&manager, preview.candidates.clone(), None)
            .await
            .unwrap();

        assert_eq!(result.sessions.len(), 1);
        let session = &result.sessions[0];
        assert_eq!(
            session.agent_id,
            vibex_core::AgentId::parse("codex").unwrap()
        );
        let page = manager
            .fetch_timeline(FetchTimelineRequest {
                session_id: session.id.clone(),
                after_sequence: Some(0),
                limit: 10,
            })
            .await
            .unwrap();
        assert_eq!(page.items.len(), 4);
        assert_eq!(page.items[0].kind, TimelineItemKind::SystemNotice);
        assert_eq!(page.items[1].kind, TimelineItemKind::UserMessage);
        assert_eq!(page.items[2].kind, TimelineItemKind::Reasoning);
        assert_eq!(page.items[3].kind, TimelineItemKind::AgentMessage);

        cleanup_db(db_path);
    }

    #[test]
    fn discover_jsonl_paths_recurses_project_directories_with_limit() {
        let root = unique_temp_dir("vibex-codex-import-recursive");
        let nested = root.join(".codex").join("sessions").join("2026");
        std::fs::create_dir_all(&nested).unwrap();
        std::fs::write(root.join("root.jsonl"), "{}\n").unwrap();
        std::fs::write(root.join("ignored.txt"), "{}\n").unwrap();
        std::fs::write(nested.join("nested-a.jsonl"), "{}\n").unwrap();
        std::fs::write(nested.join("nested-b.jsonl"), "{}\n").unwrap();

        let paths = discover_jsonl_paths(std::slice::from_ref(&root), Some(2)).unwrap();

        assert_eq!(
            paths,
            vec![
                root.join(".codex")
                    .join("sessions")
                    .join("2026")
                    .join("nested-a.jsonl"),
                root.join(".codex")
                    .join("sessions")
                    .join("2026")
                    .join("nested-b.jsonl")
            ]
        );

        let _ = std::fs::remove_dir_all(root);
    }

    fn request(paths: Vec<PathBuf>) -> CodexSessionImportPreviewRequest {
        CodexSessionImportPreviewRequest {
            paths,
            workspace_root: Some("/tmp/vibex-codex-import-fixture".to_string()),
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
            "vibex-agent-codex-{label}-{}.db",
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

    fn write_session(path: &Path, thread_id: &str, cwd: &str, user_text: &str) {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent).unwrap();
        }
        let meta = serde_json::json!({
            "timestamp": "2026-01-01T00:00:00.000Z",
            "type": "session_meta",
            "payload": {"id": thread_id, "cwd": cwd}
        });
        let turn = serde_json::json!({
            "timestamp": "2026-01-01T00:00:01.000Z",
            "type": "response_item",
            "payload": {
                "type": "message",
                "role": "user",
                "content": [{"type": "input_text", "text": user_text}]
            }
        });
        std::fs::write(
            path,
            format!("{}\n{}\n", serde_json::to_string(&meta).unwrap(), serde_json::to_string(&turn).unwrap()),
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
    fn scan_orders_newest_first_and_returns_summary_candidates() {
        let root = unique_temp_dir("vibex-codex-scan-order");
        write_session(
            &root.join("2026/01/02/rollout-b.jsonl"),
            "thread-b",
            "/tmp/project-b",
            "Second session prompt",
        );
        write_session(
            &root.join("2026/01/01/rollout-a.jsonl"),
            "thread-a",
            "/tmp/project-a",
            "First session prompt",
        );
        set_mtime(&root.join("2026/01/01/rollout-a.jsonl"), 1_000);
        set_mtime(&root.join("2026/01/02/rollout-b.jsonl"), 2_000);

        let preview = scan_codex_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, None);

        assert_eq!(preview.candidates.len(), 2);
        assert_eq!(preview.candidates[0].native_thread_id.as_deref(), Some("thread-b"));
        assert_eq!(preview.candidates[1].native_thread_id.as_deref(), Some("thread-a"));
        for candidate in &preview.candidates {
            assert_eq!(candidate.timeline_items.len(), 0, "summary scan stays light");
            assert!(candidate.source_path.is_some());
            assert_eq!(candidate.status, ExternalSessionImportCandidateStatus::Importable);
            assert_eq!(candidate.source, ExternalSessionImportSource::Codex);
        }
        assert_eq!(preview.candidates[0].title, "Imported Codex: Second session prompt");
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_summary_cache_skips_reparse_unchanged_files() {
        let root = unique_temp_dir("vibex-codex-scan-cache");
        let path = root.join("rollout-cached.jsonl");
        write_session(&path, "thread-cache", "/tmp/project-cache", "Cached prompt");

        let first = scan_codex_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, None);
        assert_eq!(first.candidates.len(), 1);
        let fingerprint = summary_fingerprint(&path);

        // Same fingerprint: the memoized summary must round-trip unchanged.
        let second = parse_codex_summary_with_cache(&path, WorkspaceMode::CurrentCheckout, None).unwrap();
        assert_eq!(second.candidate_id, first.candidates[0].candidate_id);
        assert_eq!(second.source_path, first.candidates[0].source_path);
        assert_eq!(summary_fingerprint(&path), fingerprint);

        // Mutating content invalidates the cache entry.
        write_session(&path, "thread-cache-2", "/tmp/project-cache", "Changed prompt");
        let third = parse_codex_summary_with_cache(&path, WorkspaceMode::CurrentCheckout, None).unwrap();
        assert_ne!(third.native_thread_id.as_deref(), Some("thread-cache"));
        std::fs::remove_dir_all(root).ok();
    }

    #[test]
    fn scan_limits_result_count() {
        let root = unique_temp_dir("vibex-codex-scan-limit");
        for index in 0..5 {
            write_session(
                &root.join(format!("rollout-{index}.jsonl")),
                &format!("thread-{index}"),
                "/tmp/project-limit",
                "Limit probe",
            );
        }
        let preview = scan_codex_external_sessions(&[root.clone()], WorkspaceMode::CurrentCheckout, None, Some(3));
        assert_eq!(preview.candidates.len(), 3);
        std::fs::remove_dir_all(root).ok();
    }
}
